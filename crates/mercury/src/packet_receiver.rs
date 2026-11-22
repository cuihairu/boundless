//! `lib/network/packet_receiver.cpp`: the read loop registered on a
//! listening socket. Every input notification drains the socket until it
//! is empty (or a processing budget runs out), routing each datagram to
//! the interface's sink and reporting socket errors through the error
//! reporter.
//!
//! Scope of BW-02-01-03: the loop, the error taxonomy, and the seams the
//! later items plug in. `PacketReceiverStats` and its one-second stats
//! timer belong to the EventDispatcher (BW-02-01-05); channel routing
//! (`processPacket`'s channel map and the header walk) is BW-02-03-07,
//! with the dead-channel bookkeeping, the rate limiter and the channel
//! filters still later items (BW-02-03-06 / BW-02-05).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use platform::socket;

use crate::bundle_dispatch::dispatch_messages;
use crate::endpoint::Address;
use crate::fragmented_bundle::{FragmentOutcome, FragmentedBundles, assemble_fragment};
use crate::misc::{Reason, SEQ_NULL, seq_less_than};
use crate::network_interface::NetworkInterface;
use crate::packet::{HEADER_SIZE, Packet, flags};
use crate::poller::InputNotificationHandler;
use crate::udp_channel::{AddToReceiveWindowResult, ChannelPtr, UdpChannel};

/// `dispatcher().errorReporter().reportException` (error_reporter.cpp:86).
/// The real reporter counts and throttles reports; that machinery belongs
/// to the EventDispatcher item, so the default here logs the first-report
/// text of `ErrorReporter::outputErrorString` (error_reporter.cpp:38).
pub trait ErrorReporter {
    /// `reportException`: `reason` raised by `source` (none when the
    /// packet never got far enough to have one).
    fn report_exception(&self, reason: Reason, source: Option<&Address>);
}

/// The first-report logging default.
pub struct LogReporter;

impl ErrorReporter for LogReporter {
    fn report_exception(&self, reason: Reason, source: Option<&Address>) {
        eprintln!(
            "Exception occurred: '{}' on '{}'",
            reason.to_string(),
            source.map_or_else(String::new, |address| address.c_str())
        );
    }
}

/// Where a received packet goes: `PacketReceiver::processPacket` hands
/// each datagram to the interface, and the interface routes it to its
/// channels (BW-02-03) or its tests.
pub trait PacketSink {
    /// `processPacket`: consume the datagram, name the reason it failed
    /// (`REASON_SUCCESS` when it was delivered onward).
    fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason;
}

/// `INoSuchPort::onNoSuchPort`: told when a send to `address` bounced.
pub type NoSuchPortCallback = dyn Fn(&Address);

/// The errno of the last failed OS call on this thread.
fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `strerror`.
pub(crate) fn strerror(errno: i32) -> String {
    std::io::Error::from_raw_os_error(errno).to_string()
}

/// The counters the receive dispatch owns. `PacketReceiverStats` and its
/// one-second timer belong to the EventDispatcher (BW-02-01-05); the
/// channel-related tallies (`numDuplicatePackets_` etc.) stay on the
/// channel they belong to (BW-02-03-05).
#[derive(Debug, Default, Clone)]
pub struct ReceiverStats {
    /// `numCorruptedPacketsReceived_` (packet_receiver.cpp:521).
    pub num_corrupted_packets_received: u32,
    /// `numPacketsReceivedOffChannel_` (packet_receiver.cpp:568).
    pub num_packets_received_off_channel: u32,
}

/// `PacketReceiver` (packet_receiver.cpp:58): the per-socket read loop.
/// The reference holds plain references to its socket and interface; the
/// port holds a `Weak` back to the interface so the two can point at each
/// other without a reference cycle.
pub struct PacketReceiver {
    /// The interface whose socket this drains.
    interface: RefCell<Weak<NetworkInterface>>,
    /// `pNextPacket_`: the buffer the next recvfrom fills.
    next_packet: RefCell<Packet>,
    /// `maxSocketProcessingTimeStamps_`: a processing budget in
    /// microseconds (the reference's stamps are its timestamp unit), 0
    /// meaning disabled.
    max_socket_processing_time_stamps: Cell<u64>,
    /// `pNoSuchPortCallback_`.
    no_such_port_callback: RefCell<Option<Rc<NoSuchPortCallback>>>,
    /// `onceOffReceiver_`'s `fragmentedBundles_` (BW-02-03-06): the
    /// off-channel fragment chains still assembling.
    once_off_fragments: RefCell<FragmentedBundles>,
    /// The receive counters (partial `stats_`).
    stats: RefCell<ReceiverStats>,
}

impl PacketReceiver {
    /// `PacketReceiver::PacketReceiver` minus the dispatcher-owned stats
    /// timer (BW-02-01-05).
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            interface: RefCell::new(Weak::new()),
            next_packet: RefCell::new(Packet::new()),
            max_socket_processing_time_stamps: Cell::new(0),
            no_such_port_callback: RefCell::new(None),
            once_off_fragments: RefCell::new(FragmentedBundles::new()),
            stats: RefCell::new(ReceiverStats::default()),
        })
    }

    /// Link back to the interface that owns this receiver's socket; done
    /// once the interface is behind its `Rc`.
    pub(crate) fn attach_interface(&self, interface: &Rc<NetworkInterface>) {
        *self.interface.borrow_mut() = Rc::downgrade(interface);
    }

    /// `maxSocketProcessingTime` (packet_receiver.cpp:1421): a non-positive
    /// value disables the budget.
    pub fn max_socket_processing_time(&self, value: f32) {
        if value <= 0.0 {
            self.max_socket_processing_time_stamps.set(0);
        } else {
            self.max_socket_processing_time_stamps
                .set((value * 1_000_000.0) as u64);
        }
    }

    /// `setNoSuchPortCallback`.
    pub fn set_no_such_port_callback(&self, callback: Option<Rc<NoSuchPortCallback>>) {
        *self.no_such_port_callback.borrow_mut() = callback;
    }

    /// Run `body` against the attached interface, when there is one.
    fn with_interface<T>(&self, body: impl FnOnce(&NetworkInterface) -> T) -> Option<T> {
        self.interface
            .borrow()
            .upgrade()
            .map(|interface| body(&interface))
    }

    /// `handleInputNotification` (packet_receiver.cpp:94): drain the
    /// socket, stopping early once the processing budget is spent.
    fn handle_input(&self) -> i32 {
        self.with_interface(|interface| {
            let processing_start = platform::clock::monotonic_now_micros();

            let mut num_packets_processed = 0;

            // Only true for the first call to processSocket(): a first
            // EAGAIN on an input notification is itself reportable.
            let mut expecting_packet = true;
            let mut should_process = true;
            let mut source_address = Address::default();

            while should_process {
                should_process =
                    self.process_socket(interface, &mut source_address, expecting_packet);
                expecting_packet = false;

                num_packets_processed += 1;

                let processing_elapsed = platform::clock::monotonic_now_micros()
                    .micros()
                    .saturating_sub(processing_start.micros());
                let max_stamps = self.max_socket_processing_time_stamps.get();

                if should_process && max_stamps != 0 && processing_elapsed > max_stamps {
                    eprintln!(
                        "PacketReceiver::handleInputNotification: last received from {}, \
                         stopping socket processing after {:.03}ms (processed {} packets), \
                         receive queue at {} bytes",
                        source_address.c_str(),
                        processing_elapsed as f64 / 1000.0,
                        num_packets_processed,
                        interface.udp_socket().receive_queue_size()
                    );
                    should_process = false;
                }
            }
        });
        0
    }

    /// `processSocket` (packet_receiver.cpp:209): one recvfrom, then either
    /// the sink or the error path. True when a packet was read.
    fn process_socket(
        &self,
        interface: &NetworkInterface,
        source_address: &mut Address,
        expecting_packet: bool,
    ) -> bool {
        let len = self
            .next_packet
            .borrow_mut()
            .recv_from_endpoint(&interface.udp_socket(), source_address);

        if len <= 0 {
            self.check_socket_errors(interface, len, expecting_packet);
            return false;
        }

        // The reference swaps in a fresh packet buffer and hands the full
        // one to processPacket.
        let mut current_packet = std::mem::take(&mut *self.next_packet.borrow_mut());

        let reason = interface.process_packet(source_address, &mut current_packet);

        if reason != Reason::Success && interface.is_verbose() {
            interface
                .error_reporter()
                .report_exception(reason, Some(source_address));
        }

        true
    }

    /// `checkSocketErrors` (packet_receiver.cpp:261). True when a
    /// reportable error was found; a quiet EAGAIN on a follow-up read is
    /// not reportable.
    pub fn check_socket_errors(
        &self,
        interface: &NetworkInterface,
        len: i32,
        expecting_packet: bool,
    ) -> bool {
        // is len weird?
        if len == 0 {
            if interface.is_verbose() {
                eprintln!(
                    "PacketReceiver::checkSocketErrors: Throwing REASON_GENERAL_NETWORK (1)- {}",
                    strerror(last_errno())
                );
            }
            interface
                .error_reporter()
                .report_exception(Reason::GeneralNetwork, None);
            return true;
        }

        // I'm not quite sure what it means if len is 0
        // (0 => 'end of file', but with dgram sockets?)

        // is the buffer empty?
        if last_errno() == socket::EAGAIN && !expecting_packet {
            return false;
        }

        // is it telling us there's an error?
        if last_errno() == socket::EAGAIN
            || last_errno() == socket::ECONNREFUSED
            || last_errno() == socket::EHOSTUNREACH
        {
            let mut queue_err_no = 0;
            let mut offender = Address::default();
            let mut info = 0;

            if interface
                .udp_socket()
                .read_error_queue(&mut queue_err_no, &mut offender, &mut info)
            {
                return self.dispatch_queued_send_error(interface, queue_err_no, &offender, info);
            } else {
                eprintln!(
                    "PacketReceiver::checkSocketErrors: \
                     Endpoint::readFromErrorQueue() failed: {}",
                    strerror(last_errno())
                );
            }
        }

        // Something's wrong, but no other details are available.
        // Only warn on internal interfaces. External interfaces can be noisy
        if interface.is_verbose() {
            eprintln!(
                "PacketReceiver::checkSocketErrors: Throwing REASON_GENERAL_NETWORK - {}",
                strerror(last_errno())
            );
        }

        interface
            .error_reporter()
            .report_exception(Reason::GeneralNetwork, None);
        true
    }

    /// The queued-error dispatch of `checkSocketErrors`
    /// (packet_receiver.cpp:333-353): a refused peer kills the address
    /// (`REASON_NO_SUCH_PORT`), anything else is general network noise.
    fn dispatch_queued_send_error(
        &self,
        interface: &NetworkInterface,
        queue_err_no: i32,
        offender: &Address,
        info: u32,
    ) -> bool {
        if queue_err_no == socket::ECONNREFUSED {
            self.on_connection_refused_to(interface, offender);

            if let Some(callback) = self.no_such_port_callback.borrow().as_ref() {
                callback(offender);
            }

            interface
                .error_reporter()
                .report_exception(Reason::NoSuchPort, Some(offender));
        } else {
            eprintln!(
                "PacketReceiver::checkSocketErrors: got errno {}: {} (info: {})",
                queue_err_no,
                strerror(queue_err_no),
                info
            );
            interface
                .error_reporter()
                .report_exception(Reason::GeneralNetwork, Some(offender));
        }
        true
    }

    /// `handleErrorNotification` (packet_receiver.cpp:139): read one error
    /// off the queue; false when the queue could not be cleared.
    fn handle_error(&self, _fd: i32) -> bool {
        self.with_interface(|interface| {
            let mut queue_err_no = 0;
            let mut offender = Address::default();
            let mut info = 0;

            if interface
                .udp_socket()
                .read_error_queue(&mut queue_err_no, &mut offender, &mut info)
            {
                self.dispatch_error_notification(interface, queue_err_no, &offender, info);
                true
            } else {
                // Failed to clear the error state.
                false
            }
        })
        .unwrap_or(false)
    }

    /// The body of `handleErrorNotification` after the queue read
    /// (packet_receiver.cpp:147-176), split so the quiet arms are drivable.
    fn dispatch_error_notification(
        &self,
        interface: &NetworkInterface,
        queue_err_no: i32,
        offender: &Address,
        info: u32,
    ) {
        if queue_err_no == socket::ECONNREFUSED {
            self.on_connection_refused_to(interface, offender);
        } else if queue_err_no == socket::EMSGSIZE {
            self.on_mtu_exceeded(interface, offender, info);
        } else if interface.is_verbose()
            && let Some(channel) = interface.find_channel_ref(offender)
        {
            // TRACE on external interfaces, ERROR on internal
            // (packet_receiver.cpp:161-174); both print the same text.
            eprintln!(
                "PacketReceiver::handleErrorNotification( {} ): Ignoring errno {}: {} \
                 (from {} to {})",
                channel.c_str(),
                queue_err_no,
                strerror(queue_err_no),
                interface.udp_socket().c_str(),
                offender.c_str()
            );
        }
    }

    /// `onConnectionRefusedTo` (packet_receiver.cpp:402): mark the
    /// internal channel to `offender` as remote-failed.
    fn on_connection_refused_to(&self, interface: &NetworkInterface, offender: &Address) {
        let dead_channel = interface.find_channel_ref(offender);

        if let Some(channel) = dead_channel
            && channel.is_internal()
            && !channel.has_remote_failed()
        {
            eprintln!(
                "PacketReceiver::onConnectionRefusedTo: Marking channel to {} as dead ({})",
                channel.c_str(),
                Reason::NoSuchPort.to_string()
            );
            channel.set_remote_failed();
        }
    }

    /// `onMTUExceeded` (packet_receiver.cpp:428).
    fn on_mtu_exceeded(&self, interface: &NetworkInterface, offender: &Address, info: u32) {
        if let Some(channel) = interface.find_channel_ref(offender) {
            eprintln!(
                "PacketReceiver::onMTUExceeded( {} ): MTU now {} bytes from {} to {}.",
                channel.c_str(),
                info,
                interface.udp_socket().c_str(),
                offender.c_str()
            );
        }
    }

    // -- receive dispatch (BW-02-03-07) -----------------------------------

    /// The receive counters: the port's slice of `stats_`.
    pub fn stats(&self) -> ReceiverStats {
        self.stats.borrow().clone()
    }

    /// `processPacket` (packet_receiver.cpp:445): the entry point for new
    /// packets, running the filter chain in the reference's fixed order:
    /// a channel's own filter first, then the recently-dead check, then
    /// the interface's off-channel filter, and the header walk last. The
    /// rate limiter (BW-02-05-04) comes before all of these in the
    /// reference and is still to come.
    pub fn route_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        let Some(interface) = self.interface.borrow().upgrade() else {
            return Reason::Success;
        };

        // Packets arriving on external interface will probably be
        // encrypted, so there's no point examining their header flags
        // right now.
        let channel = interface.find_channel(
            *addr,
            !interface.is_external() && packet.should_create_anonymous(),
        );

        if let Some(channel) = &channel {
            // We update received times for addressed channels here.
            // Indexed channels are done in processFilteredPacket().
            UdpChannel::on_packet_received(channel, packet.total_size() as i32);

            // The channel's filter decides what happens to its packet
            // (packet_receiver.cpp:464-473); a channel whose remote has
            // failed falls through to the header walk instead.
            let remote_failed = channel.borrow().has_remote_failed();
            if !remote_failed && let Some(filter) = UdpChannel::p_filter(channel) {
                return filter.recv(self, &interface, addr, packet, Some(Rc::clone(channel)));
            }
        }
        // If we're monitoring recent channel deaths, check now if this
        // packet should be dropped (packet_receiver.cpp:474-487).
        else if interface.is_external() && interface.is_dead(addr) {
            if interface.is_verbose() {
                eprintln!(
                    "PacketReceiver::processPacket( {} ): Ignoring incoming packet on recently dead channel",
                    addr.c_str()
                );
            }
            return Reason::Success;
        }
        // Use the network interface's default filter to filter out
        // off-channel malicious/noise packets (packet_receiver.cpp:488-492).
        else if let Some(off_channel) = interface.p_off_channel_filter() {
            return off_channel.recv(self, &interface, addr, packet, None);
        }

        // Parse raw.
        self.process_filtered_packet(&interface, addr, packet, channel)
    }

    /// `RETURN_FOR_CORRUPTED_PACKET()`: the macro's counter and reason.
    fn corrupted(&self) -> Reason {
        self.stats.borrow_mut().num_corrupted_packets_received += 1;
        Reason::CorruptedPacket
    }

    /// `processFilteredPacket` (packet_receiver.cpp:526): the header walk
    /// every received packet takes. This has to be very robust, if we
    /// intend to use this transport over the big bad internet -- we
    /// basically have to assume it'll be complete garbage. `channel` is
    /// the one `route_packet` found, if any; a `PacketFilter`'s base
    /// `recv` (packet_filter.cpp:44) hands the packet here.
    pub(crate) fn process_filtered_packet(
        &self,
        interface: &Rc<NetworkInterface>,
        addr: &Address,
        packet: &mut Packet,
        mut channel: Option<ChannelPtr>,
    ) -> Reason {
        if packet.total_size() < std::mem::size_of::<u16>() {
            if interface.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): received undersized packet",
                    addr.c_str()
                );
            }
            return self.corrupted();
        }

        // Make sure we understand all the flags
        if packet.flags() & !flags::KNOWN_FLAGS != 0 {
            if interface.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): received packet with bad flags {:x}",
                    addr.c_str(),
                    packet.flags()
                );
            }
            return self.corrupted();
        }

        if !packet.has_flags(flags::ON_CHANNEL) {
            self.stats.borrow_mut().num_packets_received_off_channel += 1;
        }

        // Don't allow FLAG_CREATE_CHANNEL on external interfaces
        if interface.is_external() && packet.has_flags(flags::CREATE_CHANNEL) {
            if interface.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): Got FLAG_CREATE_CHANNEL on external interface",
                    addr.c_str()
                );
            }
            return self.corrupted();
        }

        // make sure there's something in the packet
        if packet.total_size() <= HEADER_SIZE {
            if interface.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): received undersize packet ({} bytes)",
                    addr.c_str(),
                    packet.total_size()
                );
            }
            return self.corrupted();
        }

        // Start stripping footers
        if !packet.validate_checksum() {
            if interface.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): Invalid checksum",
                    addr.c_str()
                );
            }
            return self.corrupted();
        }

        // Strip off piggybacks and process them immediately (i.e. before
        // the messages on this packet) as the piggybacks must be older
        // than this packet
        if !self.process_piggybacks(interface, addr, packet) {
            if interface.is_verbose() {
                eprintln!(
                    "PacketReceiver::processFilteredPacket( {} ): Corrupted piggyback packets",
                    addr.c_str()
                );
            }
            return self.corrupted();
        }

        let mut should_send_channel = false;

        // Strip off indexed channel ID if present
        if packet.has_flags(flags::INDEXED_CHANNEL) {
            let (id, version) = match (packet.strip_footer_u32(), packet.strip_footer_u32()) {
                (Some(id), Some(version)) => (id, version),
                _ => {
                    if interface.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): Not enough data for indexed channel footer ({} bytes left)",
                            addr.c_str(),
                            packet.body_size()
                        );
                    }
                    return self.corrupted();
                }
            };
            packet.set_channel_id(id as i32);
            packet.set_channel_version(version);

            // The indexed finder and the condemned-channel list are
            // BW-02-03-06. With no finder registered there is no channel
            // to resolve, so the packet is dropped the way the reference
            // drops one it cannot get a channel for.
            match channel.as_ref() {
                Some(channel) => {
                    // We update received times for indexed channels here.
                    // Addressed channels are done in processPacket().
                    UdpChannel::on_packet_received(channel, packet.total_size() as i32);
                }
                None => {
                    if interface.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): Could not get indexed channel for id {}",
                            addr.c_str(),
                            id
                        );
                    }
                    return Reason::Success;
                }
            }
        }

        if channel.is_none() && packet.has_flags(flags::ON_CHANNEL) {
            channel = interface.find_channel(*addr, false);

            if channel.is_none() {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Dropping packet due to absence of local channel",
                        addr.c_str()
                    );
                }
                return Reason::GeneralNetwork;
            }
        }

        if let Some(channel) = &channel {
            if channel.borrow().has_remote_failed() {
                // __glenc__ We could consider resetting the channel here
                // if it has FLAG_CREATE_CHANNEL. This would help cope
                // with fast restarts on the same address.
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Dropping packet due to remote process failure",
                        channel.borrow().c_str()
                    );
                }
                return Reason::GeneralNetwork;
            }
            // If the packet is out of date, drop it.
            else if seq_less_than(
                packet.channel_version(),
                channel.borrow().creation_version(),
            ) {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Dropping packet from old channel {} (v{} < v{})",
                        channel.borrow().c_str(),
                        addr.c_str(),
                        packet.channel_version(),
                        channel.borrow().creation_version()
                    );
                }
                return Reason::Success;
            } else if channel.borrow().wants_first_packet() {
                if packet.has_flags(flags::CREATE_CHANNEL) {
                    UdpChannel::got_first_packet(channel);
                } else {
                    if interface.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): Dropping packet on channel wanting FLAG_CREATE_CHANNEL (flags: {:x})",
                            channel.borrow().c_str(),
                            packet.flags()
                        );
                    }
                    return Reason::GeneralNetwork;
                }
            }
        }

        if packet.has_flags(flags::HAS_CUMULATIVE_ACK) {
            let Some(channel) = channel.as_ref() else {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Cumulative ack was not on a channel.",
                        addr.c_str()
                    );
                }
                return self.corrupted();
            };

            let Some(end_seq) = packet.strip_footer_u32() else {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Not enough data for cumulative.",
                        channel.borrow().c_str()
                    );
                }
                return self.corrupted();
            };

            if !UdpChannel::handle_cumulative_ack(channel, end_seq) {
                return self.corrupted();
            }
        }

        // Strip and handle ACKs
        if packet.has_flags(flags::HAS_ACKS) {
            let Some(n_acks) = packet.strip_footer_u8() else {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Not enough data for ack count footer ({} bytes left)",
                        addr.c_str(),
                        packet.body_size()
                    );
                }
                return self.corrupted();
            };
            packet.set_n_acks(n_acks);

            if n_acks == 0 {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Packet with FLAG_HAS_ACKS had 0 acks",
                        addr.c_str()
                    );
                }
                return self.corrupted();
            }

            // The total size of all the ACKs on this packet
            let ack_size = n_acks as usize * std::mem::size_of::<u32>();

            // check that we have enough footers to account for all of the
            // acks the packet claims to have (thanks go to netease)
            if packet.body_size() < ack_size {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Not enough footers for {} acks (have {} bytes but need {})",
                        addr.c_str(),
                        n_acks,
                        packet.body_size(),
                        ack_size
                    );
                }
                return self.corrupted();
            }

            // For each ACK that we receive, we no longer need to store the
            // corresponding packet.
            if let Some(channel) = &channel {
                for _ in 0..n_acks {
                    let Some(seq) = packet.strip_footer_u32() else {
                        if interface.is_verbose() {
                            eprintln!(
                                "PacketReceiver::processFilteredPacket( {} ): Not enough data for acknowledgement footer ({} bytes left)",
                                addr.c_str(),
                                packet.body_size()
                            );
                        }
                        return self.corrupted();
                    };

                    if !UdpChannel::handle_ack(channel, seq) && interface.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): handleAck() failed for #{}",
                            addr.c_str(),
                            seq
                        );
                    }
                }
            } else if !packet.has_flags(flags::ON_CHANNEL) {
                // The once-off resend timers the reference cancels here
                // are the deferred OnceOffSender; the acks are still
                // stripped so the body walk stays correct.
                for _ in 0..n_acks {
                    if packet.strip_footer_u32().is_none() {
                        if interface.is_verbose() {
                            eprintln!(
                                "PacketReceiver::processFilteredPacket( {} ): Not enough data for acknowledgement footer ({} bytes left)",
                                addr.c_str(),
                                packet.body_size()
                            );
                        }
                        return self.corrupted();
                    }
                }
            } else {
                // Got acks without a channel
                packet.shrink(ack_size);
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Got {} acks without a channel",
                        addr.c_str(),
                        n_acks
                    );
                }
            }
        }

        // Strip sequence number
        if packet.has_flags(flags::HAS_SEQUENCE_NUMBER) {
            let Some(seq) = packet.strip_footer_u32() else {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Not enough data for sequence number footer ({} bytes left)",
                        addr.c_str(),
                        packet.body_size()
                    );
                }
                return self.corrupted();
            };
            packet.set_seq(seq);
        }

        // The ordered chain to walk: the reliable arm fills it from the
        // receive window, everything else walks just this packet.
        let mut chain: Vec<Rc<Packet>> = Vec::new();

        // now do something if it's reliable
        if packet.has_flags(flags::IS_RELIABLE) {
            // first make sure it has a sequence number, so we can address it
            if packet.seq() == SEQ_NULL {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processFilteredPacket( {} ): Dropping packet due to illegal request for reliability without related sequence number",
                        addr.c_str()
                    );
                }
                return self.corrupted();
            }

            // should we be looking in a channel
            if let Some(channel) = &channel {
                let result =
                    UdpChannel::add_to_receive_window(channel, Rc::new(packet.clone()), addr);

                if !UdpChannel::is_local_regular(channel) {
                    should_send_channel = true;
                }

                match result {
                    AddToReceiveWindowResult::NextInWindow(chained) => chain = chained,
                    AddToReceiveWindowResult::Corrupt => return self.corrupted(),
                    // The packet is not corrupted, and has either already
                    // been received, or is too early and has been buffered.
                    // In either case, we send the ACK immediately, as long
                    // as the channel is established and is irregular.
                    _ => {
                        if channel.borrow().is_established() && should_send_channel {
                            UdpChannel::send_acks(channel);
                        }
                        return Reason::Success;
                    }
                }
            } else {
                // If the packet is not on a channel (i.e. FLAG_ON_CHANNEL
                // is not set), it must be once-off reliable. Don't allow
                // incoming once-off-reliable traffic on external
                // interfaces since this is a potential DOS vulnerability.
                if interface.is_external() {
                    if interface.is_verbose() {
                        eprintln!(
                            "PacketReceiver::processFilteredPacket( {} ): Dropping deprecated once-off-reliable packet",
                            addr.c_str()
                        );
                    }
                    return Reason::Success;
                }

                // The reference acks the packet back and consults the
                // OnceOffReceiver; both are deferred with the once-off
                // sender. The packet is dropped here for now.
                return Reason::Success;
            }
        } else if let Some(channel) = &channel {
            // If the packet is unreliable, confirm that the sequence
            // number is incrementing as a sanity check against malicious
            // parties.
            if channel.borrow().is_external()
                && !UdpChannel::validate_unreliable_seq_num(channel, packet.seq())
            {
                return self.corrupted();
            }
        }

        if chain.is_empty() {
            chain.push(Rc::new(packet.clone()));
        }

        // Push this packet chain (frequently one) through
        // processOrderedPacket.
        //
        // NOTE: We check isCondemned on the channel and not isDead. If a
        // channel has isDestroyed set to true but isCondemned false, we
        // still want to process remaining messages.
        let mut oret = Reason::Success;

        for chained_packet in chain {
            if let Some(channel) = &channel
                && channel.borrow().is_condemned()
            {
                break;
            }

            let ret = self.process_ordered_packet(
                interface,
                addr,
                (*chained_packet).clone(),
                channel.clone(),
            );

            if oret == Reason::Success {
                oret = ret;
            }
        }

        // If this bundle was delivered to a channel and there are still
        // ACKs to send, do it now.
        if let Some(channel) = &channel
            && !channel.borrow().is_destroyed()
            && should_send_channel
            && channel.borrow().is_established()
            && channel.borrow().has_acks()
        {
            UdpChannel::send_acks(channel);
        }

        oret
    }

    /// `processPiggybacks` (packet_receiver.cpp:1140): strip any piggyback
    /// packets and hand each to the header walk. False when one of them
    /// was corrupted.
    fn process_piggybacks(
        &self,
        interface: &Rc<NetworkInterface>,
        addr: &Address,
        packet: &mut Packet,
    ) -> bool {
        packet.process_piggyback_packets(|inner| {
            self.process_filtered_packet(interface, addr, inner, None) != Reason::CorruptedPacket
        })
    }

    /// `processOrderedPacket` (packet_receiver.cpp:1160): process a packet
    /// after any ordering guaranteed by reliable channels has been
    /// imposed (further ordering guaranteed by fragmented bundles is
    /// still to be imposed).
    fn process_ordered_packet(
        &self,
        interface: &Rc<NetworkInterface>,
        addr: &Address,
        mut packet: Packet,
        channel: Option<ChannelPtr>,
    ) -> Reason {
        // Label to use in debug output
        let source = channel
            .as_ref()
            .map_or_else(|| addr.c_str(), |channel| channel.borrow().c_str());

        // Strip first request offset.
        let first_request_offset = if packet.has_flags(flags::HAS_REQUESTS) {
            let Some(offset) = packet.strip_footer_u16() else {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processOrderedPacket( {} ): Not enough data for first request offset ({} bytes left)",
                        source,
                        packet.body_size()
                    );
                }
                return self.corrupted();
            };

            if offset as usize > packet.msg_end_offset() {
                if interface.is_verbose() {
                    eprintln!(
                        "PacketReceiver::processOrderedPacket( {} ): First request offset out of bounds (got offset of {} bytes, {} bytes left)",
                        source,
                        offset,
                        packet.msg_end_offset()
                    );
                }
                return self.corrupted();
            }

            packet.set_first_request_offset(offset as usize);
            Some(offset as usize)
        } else {
            None
        };

        // Strip fragment footers and merge the fragment into its bundle.
        let now = interface.dispatcher().now();
        let outcome = if let Some(channel) = &channel {
            let mut this = channel.borrow_mut();
            assemble_fragment(
                addr,
                packet,
                true,
                interface.is_external(),
                &source,
                now,
                &mut this.fragments,
                &mut self.once_off_fragments.borrow_mut(),
            )
        } else {
            assemble_fragment(
                addr,
                packet,
                false,
                interface.is_external(),
                &source,
                now,
                &mut None,
                &mut self.once_off_fragments.borrow_mut(),
            )
        };

        let reason = match outcome {
            FragmentOutcome::NotFragment(mut packet) => {
                let table = interface.interface_table();
                let end = packet.msg_end_offset();
                let body = &mut packet.buffer_mut()[HEADER_SIZE..end];
                dispatch_messages(&table, addr, body, first_request_offset)
            }
            // The bundle is still incomplete, stop processing now.
            FragmentOutcome::Incomplete => Reason::Success,
            FragmentOutcome::Complete(chain) => {
                // The completed bundle dispatches as one body. The
                // reference's chain of packets presents one logical
                // stream through the bundle processor's chained buffers;
                // the port concatenates the chain's bodies into one
                // buffer instead. The first fragment's request offset,
                // stripped when it first arrived, opens the walk.
                let first_request = if chain[0].has_flags(flags::HAS_REQUESTS) {
                    Some(chain[0].first_request_offset())
                } else {
                    None
                };

                let mut body = Vec::new();
                for fragment in &chain {
                    let end = fragment.msg_end_offset();
                    body.extend_from_slice(&fragment.data()[HEADER_SIZE..end]);
                }

                let table = interface.interface_table();
                dispatch_messages(&table, addr, &mut body, first_request)
            }
            FragmentOutcome::Corrupt => return self.corrupted(),
        };

        if reason == Reason::CorruptedPacket {
            return self.corrupted();
        }

        reason
    }
}

impl InputNotificationHandler for PacketReceiver {
    fn handle_input_notification(&self, _fd: i32) -> i32 {
        self.handle_input()
    }

    fn handle_error_notification(&self, fd: i32) -> bool {
        self.handle_error(fd)
    }
}

impl PacketSink for PacketReceiver {
    /// `processPacket`: the read loop hands the datagram here; the
    /// receiver routes it to its channels (BW-02-03-07).
    fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        self.route_packet(addr, packet)
    }
}

/// Fixtures shared with the network_interface tests, which drive an
/// interface and observe it through these.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// A sink that records what reached the interface.
    #[derive(Default)]
    pub struct RecordingSink {
        pub packets: RefCell<Vec<(Address, Vec<u8>)>>,
    }

    impl PacketSink for RecordingSink {
        fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
            self.packets
                .borrow_mut()
                .push((*addr, packet.data().to_vec()));
            Reason::Success
        }
    }

    /// A sink that rejects everything with a fixed reason, for the
    /// processPacket report arm.
    pub struct RefusingSink {
        pub reason: Reason,
    }

    impl PacketSink for RefusingSink {
        fn process_packet(&self, _addr: &Address, _packet: &mut Packet) -> Reason {
            self.reason
        }
    }

    /// A reporter that records what was raised.
    #[derive(Default)]
    pub struct RecordingReporter {
        pub reports: RefCell<Vec<(Reason, Option<Address>)>>,
    }

    impl ErrorReporter for RecordingReporter {
        fn report_exception(&self, reason: Reason, source: Option<&Address>) {
            self.reports.borrow_mut().push((reason, source.copied()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::Endpoint;
    use crate::event_dispatcher::EventDispatcher;
    use crate::network_interface::InterfaceType;
    use std::cell::Cell as SoloCell;

    /// `127.0.0.1` as the repo stores it.
    const LOOPBACK: u32 = 0x0100_007f;

    /// The slice of a channel the error paths touch, counting the
    /// markings so "left alone" is observable even on a channel that
    /// starts failed.
    #[derive(Default)]
    struct FakeChannel {
        internal: bool,
        failed: SoloCell<bool>,
        marks: SoloCell<u32>,
    }

    impl FakeChannel {
        fn internal() -> Rc<Self> {
            Rc::new(Self {
                internal: true,
                failed: SoloCell::new(false),
                marks: SoloCell::new(0),
            })
        }

        fn external() -> Rc<Self> {
            Rc::new(Self {
                internal: false,
                failed: SoloCell::new(false),
                marks: SoloCell::new(0),
            })
        }
    }

    impl crate::network_interface::ChannelRef for FakeChannel {
        fn c_str(&self) -> String {
            "fake-channel".to_string()
        }
        fn is_internal(&self) -> bool {
            self.internal
        }
        fn has_remote_failed(&self) -> bool {
            self.failed.get()
        }
        fn set_remote_failed(&self) {
            self.failed.set(true);
            self.marks.set(self.marks.get() + 1);
        }
    }

    /// An internal interface on the loopback, with recording plumbing.
    fn listening_interface() -> (
        Rc<NetworkInterface>,
        Rc<PacketReceiver>,
        Rc<tests_support::RecordingSink>,
        Rc<tests_support::RecordingReporter>,
    ) {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let sink = Rc::new(tests_support::RecordingSink::default());
        let reporter = Rc::new(tests_support::RecordingReporter::default());
        interface.set_packet_sink(Some(sink.clone()));
        interface.set_error_reporter(Some(reporter.clone()));
        (interface, receiver, sink, reporter)
    }

    /// Send a datagram from a fresh socket to the interface.
    fn send_to(interface: &NetworkInterface, payload: &[u8]) -> Endpoint {
        let mut sender = Endpoint::new();
        sender.socket(socket::SOCK_DGRAM);
        assert_eq!(
            sender.sendto(payload, interface.address().port, LOOPBACK),
            payload.len() as i32
        );
        sender
    }

    /// Every queued datagram goes through the sink in one notification;
    /// the follow-up empty read is quiet (`EAGAIN` when not expecting a
    /// packet is not reportable).
    #[test]
    fn every_queued_datagram_is_processed_in_one_notification() {
        let (interface, _receiver, sink, reporter) = listening_interface();

        for index in 0..3 {
            send_to(&interface, &[b'x' + index]);
        }

        assert!(
            interface.poller().process_pending_events(0.05) >= 1,
            "the socket reported input"
        );
        {
            let packets = sink.packets.borrow();
            assert_eq!(packets.len(), 3);
            for (index, (_, payload)) in packets.iter().enumerate() {
                assert_eq!(payload, &[b'x' + index as u8]);
            }
        }

        // Nothing new: the second pass reports no events and no errors.
        interface.poller().process_pending_events(0.05);
        assert_eq!(sink.packets.borrow().len(), 3);
        assert!(reporter.reports.borrow().is_empty());
    }

    /// A zero-length datagram is a hard error: the read reports
    /// `REASON_GENERAL_NETWORK` and stops (packet_receiver.cpp:264-277).
    #[test]
    fn a_zero_length_datagram_reports_general_network() {
        let (interface, _receiver, sink, reporter) = listening_interface();

        let mut sender = Endpoint::new();
        sender.socket(socket::SOCK_DGRAM);
        assert_eq!(sender.sendto(b"", interface.address().port, LOOPBACK), 0);

        assert!(interface.poller().process_pending_events(0.05) >= 1);
        assert!(sink.packets.borrow().is_empty(), "len 0 is not delivered");
        assert_eq!(
            *reporter.reports.borrow(),
            vec![(Reason::GeneralNetwork, None)]
        );
    }

    /// A refused send marks the channel to the offender dead. The kernel
    /// reports the pending error as `EPOLLERR`, so the flow goes through
    /// `handleErrorNotification`, which marks but does not report
    /// (packet_receiver.cpp:139-184, 402-418).
    #[test]
    fn a_refused_send_marks_the_channel_dead() {
        let (interface, _receiver, _sink, reporter) = listening_interface();

        let channel = FakeChannel::internal();
        let watched = channel.clone();
        interface.set_channel_finder(Some(Rc::new(move |_| Some(watched.clone()))));

        // Send from the interface's own socket to a port nobody listens
        // on, then wake the read loop: the pending error surfaces before
        // the wake datagram is delivered.
        assert_eq!(
            interface
                .udp_socket()
                .sendto(b"bounce me", socket::htons(1), LOOPBACK),
            9
        );
        send_to(&interface, b"wake");

        assert!(interface.poller().process_pending_events(0.05) >= 1);

        assert_eq!(
            channel.marks.get(),
            1,
            "the internal channel was marked remote-failed once"
        );
        // The error-notification path marks the channel only.
        assert!(reporter.reports.borrow().is_empty());
    }

    /// `handleErrorNotification` reads the queue itself: refused marks the
    /// channel, message-too-long reports the MTU, and a quiet queue
    /// reports false (packet_receiver.cpp:139-184).
    #[test]
    fn the_error_notification_reads_the_queue() {
        let (interface, receiver, _sink, _reporter) = listening_interface();

        // Nothing queued: the queue cannot be cleared.
        let fd = interface.udp_socket().fileno();
        assert!(!receiver.handle_error_notification(fd));

        // The EMSGSIZE arm: the kernel returns an oversized send
        // synchronously without queueing it, so the arm is driven through
        // the dispatch directly.
        let channel = FakeChannel::internal();
        let watched = channel.clone();
        interface.set_channel_finder(Some(Rc::new(move |_| Some(watched.clone()))));
        let oversized = vec![0u8; 70_000];
        assert!(
            interface
                .udp_socket()
                .sendto(&oversized, socket::htons(1), LOOPBACK)
                < 0,
            "70k bytes exceed the IP datagram limit"
        );
        receiver.dispatch_error_notification(
            &interface,
            socket::EMSGSIZE,
            &Address::new(LOOPBACK, socket::htons(1)),
            1500,
        );
        assert!(!channel.failed.get(), "MTU reports do not kill channels");

        // A refused error goes through the same queue.
        assert_eq!(
            interface
                .udp_socket()
                .sendto(b"bounce", socket::htons(1), LOOPBACK),
            6
        );
        assert!(receiver.handle_error_notification(fd));
    }

    /// Queued send errors route by reason: refused to
    /// `REASON_NO_SUCH_PORT` (with the callback), everything else to
    /// `REASON_GENERAL_NETWORK` with the offender named
    /// (packet_receiver.cpp:333-353).
    #[test]
    fn queued_send_errors_route_by_reason() {
        let (interface, receiver, _sink, reporter) = listening_interface();

        let offender = Address::new(LOOPBACK, socket::htons(1));

        // A host-unreachable queue entry is general network noise.
        assert!(receiver.dispatch_queued_send_error(
            &interface,
            socket::EHOSTUNREACH,
            &offender,
            1500
        ));
        assert_eq!(
            *reporter.reports.borrow(),
            vec![(Reason::GeneralNetwork, Some(offender))]
        );

        // A refused entry reports `REASON_NO_SUCH_PORT` and calls the
        // no-such-port callback with the offender.
        let port_calls = Rc::new(RefCell::new(0));
        let calls = port_calls.clone();
        receiver.set_no_such_port_callback(Some(Rc::new(move |_addr| {
            *calls.borrow_mut() += 1;
        })));
        assert!(receiver.dispatch_queued_send_error(
            &interface,
            socket::ECONNREFUSED,
            &offender,
            0
        ));
        assert_eq!(*port_calls.borrow(), 1);
        assert_eq!(
            *reporter.reports.borrow(),
            vec![
                (Reason::GeneralNetwork, Some(offender)),
                (Reason::NoSuchPort, Some(offender)),
            ]
        );
    }

    /// The quiet arms of the error paths: only an internal channel that
    /// has not failed yet gets marked, and only channels that exist get
    /// the ignore notice (packet_receiver.cpp:147-176, 402-418).
    #[test]
    fn the_quiet_arms_leave_the_living_alone() {
        let (interface, receiver, _sink, _reporter) = listening_interface();

        // An external channel is never marked by a refusal.
        let external = FakeChannel::external();
        let watched = external.clone();
        interface.set_channel_finder(Some(Rc::new(move |_| Some(watched.clone()))));
        receiver.on_connection_refused_to(&interface, &Address::new(LOOPBACK, socket::htons(1)));
        assert_eq!(
            external.marks.get(),
            0,
            "external channels are never marked"
        );

        // An internal channel that already failed is left alone.
        let failed = FakeChannel::internal();
        let watched = failed.clone();
        failed.failed.set(true);
        interface.set_channel_finder(Some(Rc::new(move |_| Some(watched.clone()))));
        receiver.on_connection_refused_to(&interface, &Address::default());
        assert_eq!(
            failed.marks.get(),
            0,
            "an already-failed channel is left alone"
        );

        // No channel at all: nothing to mark, nothing to print.
        interface.set_channel_finder(None);
        receiver.on_connection_refused_to(&interface, &Address::default());
        receiver.on_mtu_exceeded(&interface, &Address::default(), 1500);

        // A channel present for an unexpected errno prints the ignore
        // notice and moves on.
        let any = FakeChannel::internal();
        interface.set_channel_finder(Some(Rc::new(move |_| Some(any.clone()))));
        receiver.dispatch_error_notification(
            &interface,
            socket::EHOSTUNREACH,
            &Address::default(),
            0,
        );
    }

    /// The budget stops the drain after its first packet, warning with
    /// the last source and the queue depth
    /// (packet_receiver.cpp:108-130).
    #[test]
    fn the_processing_budget_stops_the_drain() {
        let (interface, receiver, sink, reporter) = listening_interface();

        for index in 0..3 {
            send_to(&interface, &[b'y' + index]);
        }

        // Non-positive values disable the budget; one microsecond is
        // exceeded by any real processing.
        receiver.max_socket_processing_time(0.0);
        receiver.max_socket_processing_time(-1.0);
        receiver.max_socket_processing_time(0.000_001);
        assert!(interface.poller().process_pending_events(0.05) >= 1);
        assert_eq!(
            sink.packets.borrow().len(),
            1,
            "the budget stopped the drain"
        );

        // The remaining two wait for the next notification.
        receiver.max_socket_processing_time(0.0);
        assert!(interface.poller().process_pending_events(0.05) >= 1);
        assert_eq!(sink.packets.borrow().len(), 3);
        assert!(reporter.reports.borrow().is_empty());
    }

    /// An unattached receiver (no interface behind it) answers its
    /// notifications quietly.
    #[test]
    fn an_unattached_receiver_is_quiet() {
        let receiver = PacketReceiver::new();
        assert_eq!(receiver.handle_input_notification(3), 0);
        assert!(!receiver.handle_error_notification(3));
    }

    /// `checkSocketErrors` on a real `EAGAIN`: quiet when not expecting a
    /// packet, a report when the notification itself was spurious
    /// (packet_receiver.cpp:305-393).
    #[test]
    fn an_empty_socket_is_quiet_unless_the_wake_was_spurious() {
        let (interface, receiver, _sink, reporter) = listening_interface();

        // Make sure the socket is truly empty so the errno is `EAGAIN`.
        let mut scratch = [0u8; 64];
        assert!(
            interface.udp_socket().recvfrom(&mut scratch, None, None) < 0,
            "the socket was drained first"
        );

        assert!(
            !receiver.check_socket_errors(&interface, -1, false),
            "a drained buffer on a follow-up read is not reportable"
        );
        assert!(reporter.reports.borrow().is_empty());

        assert!(
            receiver.check_socket_errors(&interface, -1, true),
            "an EAGAIN on a notification is reportable"
        );
        assert_eq!(
            *reporter.reports.borrow(),
            vec![(Reason::GeneralNetwork, None)]
        );
    }

    /// A refusal surfacing on the read path (the interface socket reads
    /// its own bounced send) marks the channel and reports
    /// `REASON_NO_SUCH_PORT`; with no callback registered the flow just
    /// reports (packet_receiver.cpp:261-353).
    #[test]
    fn a_refused_read_reports_no_such_port() {
        let (interface, receiver, _sink, reporter) = listening_interface();

        let channel = FakeChannel::internal();
        let watched = channel.clone();
        interface.set_channel_finder(Some(Rc::new(move |_| Some(watched.clone()))));

        let offender = Address::new(LOOPBACK, socket::htons(1));

        // No callback registered yet: the refusal still reports.
        assert_eq!(
            interface
                .udp_socket()
                .sendto(b"bounce", socket::htons(1), LOOPBACK),
            6
        );
        let fd = interface.udp_socket().fileno();
        receiver.handle_input_notification(fd);
        assert_eq!(channel.marks.get(), 1);
        assert_eq!(
            *reporter.reports.borrow(),
            vec![(Reason::NoSuchPort, Some(offender))]
        );

        // The callback side of the refusal is covered by
        // `queued_send_errors_route_by_reason`; the kernel rate-limits
        // repeated destination-unreachable messages for the same
        // destination, so a second refusal cannot be awaited here.
    }

    /// A sink that refuses a datagram surfaces the reason through the
    /// error reporter (packet_receiver.cpp processPacket's report arm).
    #[test]
    fn a_rejecting_sink_reports_the_reason() {
        let (interface, _receiver, _sink, reporter) = listening_interface();
        interface.set_packet_sink(Some(Rc::new(tests_support::RefusingSink {
            reason: Reason::WindowOverflow,
        })));

        let sender = send_to(&interface, b"reject me");
        assert!(interface.poller().process_pending_events(0.05) >= 1);

        let reports = reporter.reports.borrow();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, Reason::WindowOverflow);
        assert_eq!(
            reports[0].1.unwrap().port,
            sender.local().port,
            "the report names the sender"
        );
    }

    /// The acceptance: a packet sent to interface A's socket is
    /// processed by A and never by B, and the other way round.
    #[test]
    fn the_correct_interface_receives_its_event() {
        let (a, _ra, sink_a, _reporter_a) = listening_interface();
        let (b, _rb, sink_b, _reporter_b) = listening_interface();

        let sender = send_to(&a, b"for a");
        drop(sender);
        send_to(&b, b"for b");

        // Poll only A: its socket saw its own datagram, never B's.
        assert!(a.poller().process_pending_events(0.05) >= 1);
        {
            let packets = sink_a.packets.borrow();
            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0].1, b"for a");
        }
        assert!(sink_b.packets.borrow().is_empty());

        // Poll B: its own datagram, and A stays untouched.
        assert!(b.poller().process_pending_events(0.05) >= 1);
        {
            let packets = sink_b.packets.borrow();
            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0].1, b"for b");
        }
        assert_eq!(sink_a.packets.borrow().len(), 1);
    }

    // ------------------------------------------------------------------
    // The bidirectional exchange (BW-02-03-07)
    // ------------------------------------------------------------------

    /// The `msg1` message id, matching `CommonInterface::msg1`.
    const MSG1_ID: u8 = 3;
    /// The `disconnect` message id, matching `CommonInterface::disconnect`.
    const DISCONNECT_ID: u8 = 4;

    /// `msg1`'s wire body: traits byte, seq, data -- 9 bytes.
    fn msg1_body(traits: u8, seq: u32, data: u32) -> Vec<u8> {
        let mut body = Vec::with_capacity(9);
        body.push(traits);
        body.extend_from_slice(&seq.to_le_bytes());
        body.extend_from_slice(&data.to_le_bytes());
        body
    }

    /// `disconnect`'s wire body: just the seq.
    fn disconnect_body(seq: u32) -> Vec<u8> {
        seq.to_le_bytes().to_vec()
    }

    /// `RunUntilCompleteHandler` (test_reliable.cpp:36): a handler that
    /// records the `msg1` seqs and the disconnect, asserting each arrives
    /// exactly once and nothing is delivered out of order.
    struct WireRecorder {
        msg1s: RefCell<Vec<u32>>,
        disconnect_seq: Cell<u32>,
        seen: RefCell<std::collections::BTreeSet<u32>>,
    }

    impl WireRecorder {
        fn new() -> Self {
            Self {
                msg1s: RefCell::new(Vec::new()),
                disconnect_seq: Cell::new(u32::MAX),
                seen: RefCell::new(std::collections::BTreeSet::new()),
            }
        }

        fn got(&self, seq: u32) {
            assert!(
                self.seen.borrow_mut().insert(seq),
                "seq {seq} was delivered twice"
            );
        }

        fn complete(&self) -> bool {
            let disconnect = self.disconnect_seq.get();
            disconnect != u32::MAX
                && self.msg1s.borrow().len() == disconnect as usize
                && self.msg1s.borrow().iter().copied().eq(0..disconnect)
        }
    }

    impl crate::interfaces::InputMessageHandler for WireRecorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut crate::unpacked_message_header::UnpackedMessageHeader,
            data: &mut foundation::stream::BinaryStream,
        ) {
            match header.identifier() {
                MSG1_ID => {
                    let (seq, data_word) = {
                        let body = data.unread_bytes();
                        assert_eq!(body.len(), 9, "msg1 body is traits + seq + data");
                        (
                            u32::from_le_bytes(body[1..5].try_into().unwrap()),
                            u32::from_le_bytes(body[5..9].try_into().unwrap()),
                        )
                    };
                    if data_word == 0 {
                        self.msg1s.borrow_mut().push(seq);
                        self.got(seq);
                    }
                }
                DISCONNECT_ID => {
                    let seq = {
                        let body = data.unread_bytes();
                        assert_eq!(body.len(), 4, "disconnect body is just the seq");
                        u32::from_le_bytes(body[0..4].try_into().unwrap())
                    };
                    self.disconnect_seq.set(seq);
                    self.got(seq);
                }
                _ => {}
            }
            data.finish();
        }
    }

    /// An external interface wired for the exchange: socket on the
    /// loopback, its receiver attached and set as the sink, the two
    /// elements served.
    fn exchange_interface(
        dispatcher: &Rc<EventDispatcher>,
        recorder: &Rc<WireRecorder>,
    ) -> (Rc<NetworkInterface>, Rc<PacketReceiver>) {
        let interface =
            NetworkInterface::new_with_dispatcher(InterfaceType::External, Rc::clone(dispatcher));
        let receiver = PacketReceiver::new();
        assert!(
            interface.recreate_listening_socket(&receiver, 0, "127.0.0.1"),
            "the loopback socket opened"
        );
        receiver.attach_interface(&interface);
        interface.set_packet_sink(Some(receiver.clone() as Rc<dyn PacketSink>));
        interface.set_irregular_channels_resend_period(0.1);

        let msg1 = crate::interface_element::InterfaceElement::new(
            "msg1",
            MSG1_ID,
            crate::interface_element::FIXED_LENGTH_MESSAGE,
            9,
            None,
        );
        let disconnect = crate::interface_element::InterfaceElement::new(
            "disconnect",
            DISCONNECT_ID,
            crate::interface_element::FIXED_LENGTH_MESSAGE,
            4,
            None,
        );
        interface.interface_table().serve(
            &msg1,
            Some(Rc::clone(recorder) as Rc<dyn crate::interfaces::InputMessageHandler>),
        );
        interface.interface_table().serve(
            &disconnect,
            Some(Rc::clone(recorder) as Rc<dyn crate::interfaces::InputMessageHandler>),
        );

        (interface, receiver)
    }

    /// `RunUntilCompleteHandler::generateMessages` (test_reliable.cpp:66):
    /// `count` msg1s, odd ones reliable drivers, plus one disconnect
    /// driver with seq `count`.
    fn fill_bundle(channel: &ChannelPtr) {
        let msg1 = crate::interface_element::InterfaceElement::new(
            "msg1",
            MSG1_ID,
            crate::interface_element::FIXED_LENGTH_MESSAGE,
            9,
            None,
        );
        let disconnect = crate::interface_element::InterfaceElement::new(
            "disconnect",
            DISCONNECT_ID,
            crate::interface_element::FIXED_LENGTH_MESSAGE,
            4,
            None,
        );
        let mut this = channel.borrow_mut();
        let bundle = this.bundle_mut();
        for seq in 0..401u32 {
            let reliable = if seq % 2 == 1 {
                crate::udp_bundle::ReliableType::Driver
            } else {
                crate::udp_bundle::ReliableType::No
            };
            bundle.start_message(&msg1, reliable);
            bundle.write_bytes(&msg1_body(0, seq, 0));
        }
        bundle.start_message(&disconnect, crate::udp_bundle::ReliableType::Driver);
        bundle.write_bytes(&disconnect_body(401));
    }

    /// The acceptance for BW-02-03-07: both directions of an external
    /// exchange, each losing exactly one packet, deliver every message
    /// exactly once and in order, and each side counts exactly one
    /// resend -- the `test_reliable.cpp` `ExternalLink` fixture, run both
    /// ways on a shared pinned-clock dispatcher.
    #[test]
    fn both_directions_exchange_and_count_the_resend() {
        let clock = Rc::new(SoloCell::new(1_000_000u64));
        let clock_handle = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_handle.get()
        })));

        let recorder_a = Rc::new(WireRecorder::new());
        let recorder_b = Rc::new(WireRecorder::new());

        let (interface_a, _receiver_a) = exchange_interface(&dispatcher, &recorder_a);
        let (interface_b, _receiver_b) = exchange_interface(&dispatcher, &recorder_b);

        let channel_a = UdpChannel::new(
            &interface_a,
            interface_b.address(),
            crate::udp_channel::Traits::External,
            crate::udp_channel::DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        );
        let channel_b = UdpChannel::new(
            &interface_b,
            interface_a.address(),
            crate::udp_channel::Traits::External,
            crate::udp_channel::DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        );

        // The reference fixture's irregular channels: both regularity
        // flags off, so the resend collection watches them.
        UdpChannel::set_local_regular(&channel_a, false);
        UdpChannel::set_remote_regular(&channel_a, false);
        UdpChannel::set_local_regular(&channel_b, false);
        UdpChannel::set_remote_regular(&channel_b, false);

        // The reference fixture runs one direction at a time: one
        // side drops its first packet and sends 401 messages, the other
        // delivers them and acknowledges. Both ways are run here, each
        // with a fresh packet count for the side that sends, because a
        // sender only ever receives acknowledgements and so its own
        // bundle plus the one resend is exactly its traffic.
        let a_packets =
            exchange_one_direction(&dispatcher, &clock, &channel_a, &interface_a, &recorder_b);

        // Exactly one more packet than the bundle spans (test_reliable.cpp:425).
        assert_eq!(
            interface_a.sending_stats().num_packets_sent,
            (a_packets + 1) as u32,
            "side A's packets are the bundle plus the resend"
        );
        assert_eq!(
            interface_a.sending_stats().num_packets_resent,
            1,
            "side A resent its dropped packet"
        );
        delivered_everything(&recorder_b);

        // B's acknowledgement traffic from the first direction would
        // pollute its totals, so the second direction asserts a delta.
        let b_before = interface_b.sending_stats().num_packets_sent;
        let b_packets =
            exchange_one_direction(&dispatcher, &clock, &channel_b, &interface_b, &recorder_a);

        assert_eq!(
            interface_b.sending_stats().num_packets_sent - b_before,
            (b_packets + 1) as u32,
            "side B's packets are the bundle plus the resend"
        );
        assert_eq!(
            interface_b.sending_stats().num_packets_resent,
            1,
            "side B resent its dropped packet"
        );
        delivered_everything(&recorder_a);

        // Nothing was piggybacked out of a multi-packet bundle
        // (test_reliable.cpp:440).
        assert_eq!(interface_a.sending_stats().num_piggybacks, 0);
        assert_eq!(interface_b.sending_stats().num_piggybacks, 0);
    }

    /// `fillBundle`, `dropNextSend`, `send`, then the pinned clock until
    /// the sender has nothing left outstanding and the receiver delivered
    /// everything. Returns the number of packets the bundle spanned.
    fn exchange_one_direction(
        dispatcher: &Rc<EventDispatcher>,
        clock: &Rc<SoloCell<u64>>,
        sender: &ChannelPtr,
        sender_interface: &Rc<NetworkInterface>,
        receiver: &Rc<WireRecorder>,
    ) -> usize {
        fill_bundle(sender);
        sender_interface.drop_next_send();
        let packets = sender.borrow().bundle().num_data_units();
        UdpChannel::send(sender);
        assert!(packets > 1, "401 messages span more than one packet");

        let mut steps = 0;
        loop {
            steps += 1;
            assert!(steps <= 600, "the exchange did not settle in 60s");
            clock.set(clock.get() + 100_000);
            dispatcher.process_once(false);
            if !sender.borrow().has_unsent_data()
                && !sender.borrow().has_unacked_packets()
                && receiver.complete()
            {
                return packets;
            }
        }
    }

    /// The reference's delivery checks: 0..=401 exactly once, in order,
    /// and the disconnect last.
    fn delivered_everything(receiver: &Rc<WireRecorder>) {
        assert_eq!(
            receiver.msg1s.borrow().len(),
            401,
            "every msg1 was delivered in order"
        );
        assert_eq!(
            receiver.disconnect_seq.get(),
            401,
            "the disconnect arrived last"
        );
    }

    // ------------------------------------------------------------------
    // The header walk's refusals (processFilteredPacket)
    // ------------------------------------------------------------------

    /// Build a received packet: flags word plus a body, no footers yet.
    fn wire_packet(flags_word: u16, body: &[u8]) -> Packet {
        let mut packet = Packet::new();
        packet.grow(HEADER_SIZE);
        packet.set_flags(flags_word);
        packet.grow(body.len());
        packet.buffer_mut()[HEADER_SIZE..HEADER_SIZE + body.len()].copy_from_slice(body);
        packet
    }

    /// Append a footer field to the end of a packet.
    fn append_footer(packet: &mut Packet, bytes: &[u8]) {
        let at = packet.msg_end_offset();
        packet.grow(bytes.len());
        packet.buffer_mut()[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Walk one packet through the header checks, as `processPacket`
    /// hands them to it (packet_receiver.cpp:526).
    fn header_walk(
        interface: &Rc<NetworkInterface>,
        receiver: &Rc<PacketReceiver>,
        addr: &Address,
        packet: &mut Packet,
        channel: Option<ChannelPtr>,
    ) -> Reason {
        receiver.process_filtered_packet(interface, addr, packet, channel)
    }

    /// The guards `processFilteredPacket` refuses: an undersized body, a
    /// checksum that does not match, a piggyback footer with too little
    /// body, an indexed footer missing a word, an on-channel packet with
    /// no channel to sit on, and the once-off shapes the reference
    /// drops. Each trip logs the branch and stops the packet
    /// (packet_receiver.cpp:526-830).
    #[test]
    fn the_header_walk_refuses_every_garbage_shape() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let walk = |packet: &mut Packet| header_walk(&interface, &receiver, &addr, packet, None);

        // A header with no body at all.
        assert_eq!(walk(&mut wire_packet(0, b"")), Reason::CorruptedPacket);

        // A packet smaller than the walk's 16-bit floor
        // (processFilteredPacket, :460).
        let mut tiny = Packet::new();
        tiny.grow(1);
        assert_eq!(walk(&mut tiny), Reason::CorruptedPacket);

        // A flag word with a bit the walk does not understand
        // (processFilteredPacket, :471, KNOWN_FLAGS is 0x07FF).
        assert_eq!(
            walk(&mut wire_packet(0x2000, b"x")),
            Reason::CorruptedPacket
        );

        // A cumulative ack with no channel to apply it to
        // (processFilteredPacket, :635-643).
        assert_eq!(
            walk(&mut wire_packet(flags::HAS_CUMULATIVE_ACK, b"x")),
            Reason::CorruptedPacket
        );

        // Acks off a channel (no FLAG_ON_CHANNEL): the once-off resend
        // timers are deferred, the acks are still stripped so the body
        // walk stays correct (processFilteredPacket, :725-740).
        assert_eq!(
            walk(&mut wire_packet(flags::HAS_ACKS, &[0, 0, 0, 0, 1])),
            Reason::Success
        );

        // A checksum flag with nothing to check against.
        assert_eq!(
            walk(&mut wire_packet(flags::HAS_CHECKSUM, b"hello world")),
            Reason::CorruptedPacket
        );

        // A piggyback footer whose body is too short to hold it.
        assert_eq!(
            walk(&mut wire_packet(flags::HAS_PIGGYBACKS, b"x")),
            Reason::CorruptedPacket
        );

        // An indexed footer missing its second word.
        assert_eq!(
            walk(&mut wire_packet(
                flags::INDEXED_CHANNEL,
                &4u32.to_le_bytes()
            )),
            Reason::CorruptedPacket
        );

        // An indexed packet with no channel to resolve for it.
        let mut indexed = wire_packet(flags::INDEXED_CHANNEL, b"");
        append_footer(&mut indexed, &4u32.to_le_bytes());
        append_footer(&mut indexed, &1u32.to_le_bytes());
        assert_eq!(walk(&mut indexed), Reason::Success);

        // An on-channel packet with no local channel to receive it.
        assert_eq!(
            walk(&mut wire_packet(flags::ON_CHANNEL, b"x")),
            Reason::GeneralNetwork
        );

        // A reliable once-off packet: dropped after the header, not
        // corrupted (packet_receiver.cpp:824-827).
        let mut once_off = wire_packet(flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER, b"");
        append_footer(&mut once_off, &5u32.to_le_bytes());
        assert_eq!(walk(&mut once_off), Reason::Success);

        // Acks with a count of zero, and with not enough footers.
        assert_eq!(
            walk(&mut wire_packet(flags::HAS_ACKS, &[0])),
            Reason::CorruptedPacket
        );
        assert_eq!(
            walk(&mut wire_packet(flags::HAS_ACKS, &[2])),
            Reason::CorruptedPacket
        );

        // A sequence footer that is not there.
        assert_eq!(
            walk(&mut wire_packet(flags::HAS_SEQUENCE_NUMBER, b"x")),
            Reason::CorruptedPacket
        );

        // Reliability without a sequence number to address it.
        assert_eq!(
            walk(&mut wire_packet(flags::IS_RELIABLE, b"x")),
            Reason::CorruptedPacket
        );

        // A request offset footer that is not there, and one past the
        // end of the message (processOrderedPacket, :1160).
        assert_eq!(
            walk(&mut wire_packet(flags::HAS_REQUESTS, b"x")),
            Reason::CorruptedPacket
        );
        let mut past = wire_packet(flags::HAS_REQUESTS, b"");
        append_footer(&mut past, &100u16.to_le_bytes());
        assert_eq!(walk(&mut past), Reason::CorruptedPacket);

        // A request offset footer that is in bounds: the offset is stored
        // and the packet goes on to the fragment walk and dispatch
        // (processOrderedPacket, :1166-1175).
        let mut in_bounds = wire_packet(flags::HAS_REQUESTS, b"");
        append_footer(&mut in_bounds, &0u16.to_le_bytes());
        assert_eq!(walk(&mut in_bounds), Reason::Success);
    }

    /// The same walk on an external interface refuses a
    /// `FLAG_CREATE_CHANNEL` there and drops the deprecated once-off
    /// reliable traffic (packet_receiver.cpp:487, 814-821).
    #[test]
    fn an_external_interface_refuses_create_and_once_off_reliable() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));

        assert_eq!(
            header_walk(
                &interface,
                &receiver,
                &addr,
                &mut wire_packet(flags::CREATE_CHANNEL, b""),
                None
            ),
            Reason::CorruptedPacket
        );

        let mut once_off = wire_packet(flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER, b"");
        append_footer(&mut once_off, &5u32.to_le_bytes());
        assert_eq!(
            header_walk(&interface, &receiver, &addr, &mut once_off, None),
            Reason::Success,
            "the packet is dropped, not corrupted"
        );
    }

    /// The channel-bound half of the walk: a failed remote end, a
    /// cumulative ack without its footer, a missing ack footer, and an
    /// unreliable sequence the channel will not accept all stop the
    /// packet where the reference does
    /// (packet_receiver.cpp:591-631, 645-651, 706-713, 833-836).
    #[test]
    fn channel_bound_guards_stop_the_walk() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = UdpChannel::new(
            &interface,
            addr,
            crate::udp_channel::Traits::External,
            crate::udp_channel::DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        );

        // A cumulative ack with no footer to read.
        let mut cumulative = wire_packet(flags::HAS_CUMULATIVE_ACK, b"x");
        assert_eq!(
            header_walk(
                &interface,
                &receiver,
                &addr,
                &mut cumulative,
                Some(Rc::clone(&channel))
            ),
            Reason::CorruptedPacket
        );

        // Acks naming more sequences than the footers hold.
        let mut short_acks = wire_packet(flags::HAS_ACKS, b"");
        append_footer(&mut short_acks, &[1]);
        append_footer(&mut short_acks, &[0, 0]);
        assert_eq!(
            header_walk(
                &interface,
                &receiver,
                &addr,
                &mut short_acks,
                Some(Rc::clone(&channel))
            ),
            Reason::CorruptedPacket
        );

        // An unreliable packet whose sequence the channel refuses.
        assert_eq!(
            header_walk(
                &interface,
                &receiver,
                &addr,
                &mut wire_packet(flags::ON_CHANNEL, b"x"),
                Some(Rc::clone(&channel))
            ),
            Reason::CorruptedPacket
        );

        // A channel whose far end failed takes no more traffic; it is
        // last because it poisons the channel for the rest of the walk.
        UdpChannel::set_remote_failed(&channel);
        let mut failed = wire_packet(flags::ON_CHANNEL, b"x");
        assert_eq!(
            header_walk(&interface, &receiver, &addr, &mut failed, Some(channel)),
            Reason::GeneralNetwork
        );
    }

    /// A channel that follows its source to a new address waits for a
    /// `FLAG_CREATE_CHANNEL` from there; any other packet is dropped
    /// (udp_channel.cpp:1655-1661, packet_receiver.cpp:618-629).
    #[test]
    fn a_rebuilt_channel_waits_for_its_create_packet() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let moved = Address::new(LOOPBACK, socket::htons(10));
        let channel = UdpChannel::new(
            &interface,
            addr,
            crate::udp_channel::Traits::External,
            crate::udp_channel::DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        );

        // Following the source re-opens the door at the new address.
        UdpChannel::set_address_fn(&channel, moved);
        assert!(channel.borrow().wants_first_packet());

        // Waiting for the flag, any other traffic is dropped.
        assert_eq!(
            header_walk(
                &interface,
                &receiver,
                &moved,
                &mut wire_packet(flags::ON_CHANNEL, b"x"),
                Some(Rc::clone(&channel))
            ),
            Reason::GeneralNetwork
        );

        // The CREATE_CHANNEL packet opens it again.
        assert_eq!(
            header_walk(
                &interface,
                &receiver,
                &moved,
                &mut wire_packet(flags::ON_CHANNEL | flags::CREATE_CHANNEL, b"x"),
                Some(Rc::clone(&channel))
            ),
            Reason::CorruptedPacket
        );
        assert!(!channel.borrow().wants_first_packet());
    }

    /// With the interface gone there is nothing to route a packet to: the
    /// walk is skipped, not corrupted (processPacket, :445).
    #[test]
    fn a_receiver_without_its_interface_routes_nowhere() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        drop(interface);

        assert_eq!(
            receiver.route_packet(&addr, &mut wire_packet(0, b"x")),
            Reason::Success
        );
    }

    // ------------------------------------------------------------------
    // The filter chain (processPacket, BW-02-05-01)
    // ------------------------------------------------------------------

    use crate::packet_filter::PacketFilter;
    use crate::packet_sender::PacketSender;
    use crate::udp_channel::Traits as ChannelTraits;

    /// A filter that records whether it was consulted and then either
    /// hands the packet on or stops it, and claims a spare room out of
    /// every packet.
    struct RecordingFilter {
        recv_calls: SoloCell<u32>,
        send_calls: SoloCell<u32>,
        /// The reason `recv` returns when it does not pass through.
        reject: Option<Reason>,
        /// `maxSpareSize`.
        spare: SoloCell<usize>,
    }

    impl RecordingFilter {
        fn passthrough() -> Rc<Self> {
            Self::with_spare(0)
        }

        fn rejecting(reason: Reason) -> Rc<Self> {
            Rc::new(RecordingFilter {
                recv_calls: SoloCell::new(0),
                send_calls: SoloCell::new(0),
                reject: Some(reason),
                spare: SoloCell::new(0),
            })
        }

        fn with_spare(spare: usize) -> Rc<Self> {
            Rc::new(RecordingFilter {
                recv_calls: SoloCell::new(0),
                send_calls: SoloCell::new(0),
                reject: None,
                spare: SoloCell::new(spare),
            })
        }
    }

    impl PacketFilter for RecordingFilter {
        fn recv(
            &self,
            receiver: &PacketReceiver,
            interface: &Rc<NetworkInterface>,
            addr: &Address,
            packet: &mut Packet,
            channel: Option<ChannelPtr>,
        ) -> Reason {
            self.recv_calls.set(self.recv_calls.get() + 1);
            match self.reject {
                Some(reason) => reason,
                None => receiver.process_filtered_packet(interface, addr, packet, channel),
            }
        }

        fn send(&self, sender: &mut PacketSender, addr: &Address, packet: &Rc<Packet>) -> Reason {
            self.send_calls.set(self.send_calls.get() + 1);
            sender.basic_send_with_retries(addr, packet)
        }

        fn max_spare_size(&self) -> usize {
            self.spare.get()
        }
    }

    fn external_channel(interface: &Rc<NetworkInterface>, addr: Address) -> ChannelPtr {
        UdpChannel::new(
            interface,
            addr,
            ChannelTraits::External,
            crate::udp_channel::DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        )
    }

    /// A channel's filter is consulted first; a packet it hands on still
    /// takes the header walk and its refusals (packet_receiver.cpp:464-473).
    #[test]
    fn a_channel_filter_is_consulted_before_the_walk() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        let filter = RecordingFilter::passthrough();
        UdpChannel::set_p_filter(&channel, Some(filter.clone() as Rc<dyn PacketFilter>));

        let reason = receiver.route_packet(&addr, &mut wire_packet(flags::ON_CHANNEL, b"x"));
        assert_eq!(reason, Reason::CorruptedPacket, "the walk still refuses it");
        assert_eq!(filter.recv_calls.get(), 1, "the filter saw it first");
    }

    /// A filter that stops the packet wins; the walk never runs, so the
    /// corrupted counter does not move.
    #[test]
    fn a_rejecting_channel_filter_stops_the_walk() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        let filter = RecordingFilter::rejecting(Reason::GeneralNetwork);
        UdpChannel::set_p_filter(&channel, Some(filter.clone() as Rc<dyn PacketFilter>));

        let reason = receiver.route_packet(&addr, &mut wire_packet(flags::ON_CHANNEL, b"x"));
        assert_eq!(reason, Reason::GeneralNetwork);
        assert_eq!(filter.recv_calls.get(), 1);
        assert_eq!(
            receiver.stats().num_corrupted_packets_received,
            0,
            "the walk did not run"
        );
    }

    /// A channel whose remote has failed skips its filter: the reference
    /// guards `pFilter() && !hasRemoteFailed()` (packet_receiver.cpp:465).
    #[test]
    fn a_remote_failed_channel_skips_its_filter() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        let filter = RecordingFilter::passthrough();
        UdpChannel::set_p_filter(&channel, Some(filter.clone() as Rc<dyn PacketFilter>));
        UdpChannel::set_remote_failed(&channel);

        let reason = receiver.route_packet(&addr, &mut wire_packet(flags::ON_CHANNEL, b"x"));
        assert_eq!(
            reason,
            Reason::GeneralNetwork,
            "the walk refuses the failed link"
        );
        assert_eq!(
            filter.recv_calls.get(),
            0,
            "the failed channel's filter is skipped"
        );
    }

    /// A found channel that has no filter falls straight through to the
    /// header walk; the off-channel filter is not consulted
    /// (packet_receiver.cpp:488's `else if` binds to "no channel").
    #[test]
    fn a_channel_without_a_filter_skips_the_off_channel_filter() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        external_channel(&interface, addr);
        let off = RecordingFilter::passthrough();
        interface.set_p_off_channel_filter(Some(off.clone() as Rc<dyn PacketFilter>));

        let reason = receiver.route_packet(&addr, &mut wire_packet(flags::ON_CHANNEL, b"x"));
        assert_eq!(reason, Reason::CorruptedPacket);
        assert_eq!(off.recv_calls.get(), 0, "a found channel stays on-channel");
    }

    /// An off-channel packet on an external interface goes to the
    /// interface's default filter.
    #[test]
    fn an_off_channel_packet_goes_to_the_off_channel_filter() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let off = RecordingFilter::rejecting(Reason::GeneralNetwork);
        interface.set_p_off_channel_filter(Some(off.clone() as Rc<dyn PacketFilter>));

        let reason = receiver.route_packet(&addr, &mut wire_packet(0, b"x"));
        assert_eq!(reason, Reason::GeneralNetwork);
        assert_eq!(off.recv_calls.get(), 1, "the off-channel filter saw it");
    }

    /// A straggler from a recently dead external channel is dropped before
    /// any filter sees it (packet_receiver.cpp:474-487).
    #[test]
    fn a_dead_external_address_is_dropped_before_any_filter() {
        let (interface, receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        interface.deregister_channel(&channel);
        assert!(interface.is_dead(&addr));

        let off = RecordingFilter::passthrough();
        interface.set_p_off_channel_filter(Some(off.clone() as Rc<dyn PacketFilter>));
        let reason = receiver.route_packet(&addr, &mut wire_packet(0, b"x"));
        assert_eq!(reason, Reason::Success, "the straggler is dropped");
        assert_eq!(
            off.recv_calls.get(),
            0,
            "no filter sees the dead channel's packet"
        );
    }

    /// Only external channels are remembered when deregistered: an
    /// internal interface has no recently-dead set (network_interface.cpp:165-170).
    #[test]
    fn deregister_marks_only_external_channels_dead() {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        assert!(!interface.is_dead(&addr));
        interface.deregister_channel(&channel);
        assert!(
            interface.is_dead(&addr),
            "an external channel is remembered"
        );

        let (internal, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let iaddr = Address::new(LOOPBACK, socket::htons(10));
        let internal_channel = UdpChannel::new(
            &internal,
            iaddr,
            ChannelTraits::Internal,
            crate::udp_channel::DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        );
        internal.deregister_channel(&internal_channel);
        assert!(
            !internal.is_dead(&iaddr),
            "an internal channel is not remembered"
        );
    }

    /// A channel's filter claims its spare room out of the channel's
    /// bundle (`newBundle`, udp_channel.cpp:1526-1531): the room is
    /// reserved as a packet opens, so the next bundle cycle after the
    /// filter is attached carries it.
    #[test]
    fn a_filter_spare_room_is_reserved_in_the_bundle() {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        assert_eq!(channel.borrow().bundle().packet(0).extra_filter_size(), 0);

        let filter = RecordingFilter::with_spare(5);
        UdpChannel::set_p_filter(&channel, Some(filter.clone() as Rc<dyn PacketFilter>));
        channel.borrow_mut().clear_bundle();
        assert_eq!(
            channel.borrow().bundle().packet(0).extra_filter_size(),
            5,
            "the filter's spare room is reserved"
        );
    }

    /// The send path consults the channel's filter before the socket
    /// (packet_sender.cpp:245-255).
    #[test]
    fn the_send_path_consults_the_channel_filter() {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        let filter = RecordingFilter::passthrough();
        UdpChannel::set_p_filter(&channel, Some(filter.clone() as Rc<dyn PacketFilter>));

        let mut packet = Packet::new();
        packet.grow(HEADER_SIZE);
        interface.send_packet(&addr, &Rc::new(packet), Some(&channel), false);
        assert_eq!(
            filter.send_calls.get(),
            1,
            "the filter saw the outbound packet"
        );
    }

    /// The trait's defaults are the transport: a filter with no overrides
    /// sends through the retrying socket write and receives through the
    /// header walk, and claims no spare room (packet_filter.cpp:34-49).
    #[test]
    fn the_default_filter_hands_everything_to_the_transport() {
        struct DefaultFilter;
        impl PacketFilter for DefaultFilter {}

        let (interface, receiver) = NetworkInterface::new(InterfaceType::External, 0, "127.0.0.1");
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let channel = external_channel(&interface, addr);
        UdpChannel::set_p_filter(
            &channel,
            Some(Rc::new(DefaultFilter) as Rc<dyn PacketFilter>),
        );
        assert_eq!(DefaultFilter.max_spare_size(), 0, "the base claims no room");

        // In: the walk still runs and refuses the packet.
        let reason = receiver.route_packet(&addr, &mut wire_packet(flags::ON_CHANNEL, b"x"));
        assert_eq!(reason, Reason::CorruptedPacket);

        // Out: the retrying socket write runs and the packet leaves.
        let mut packet = Packet::new();
        packet.grow(HEADER_SIZE);
        interface.send_packet(&addr, &Rc::new(packet), Some(&channel), false);
        assert_eq!(interface.sending_stats().num_packets_sent, 1);
    }
}
