//! `lib/network/packet_receiver.cpp`: the read loop registered on a
//! listening socket. Every input notification drains the socket until it
//! is empty (or a processing budget runs out), routing each datagram to
//! the interface's sink and reporting socket errors through the error
//! reporter.
//!
//! Scope of BW-02-01-03: the loop, the error taxonomy, and the seams the
//! later items plug in. `PacketReceiverStats` and its one-second stats
//! timer belong to the EventDispatcher (BW-02-01-05); channel routing
//! (`processPacket`'s channel map, dead-channel bookkeeping and the rate
//! limiter) arrives with the channels (BW-02-03 / BW-02-05-04).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

use platform::socket;

use crate::endpoint::Address;
use crate::fragmented_bundle::FragmentedBundle;
use crate::misc::{Reason, SEQ_NULL, SeqNum, seq_less_than};
use crate::network_interface::{ChannelRef, NetworkInterface};
use crate::packet::{HEADER_SIZE, Packet, flags};
use crate::poller::InputNotificationHandler;
use crate::udp_bundle::UDPBundle;
use crate::udp_bundle_processor::UDPBundleProcessor;
use crate::udp_channel::{AddToReceiveWindowResult, UDPChannel};

/// `OnceOffReceiver::FragmentedBundles` (once_off_packet.hpp): the
/// off-channel fragment bundles, keyed by (address, fragBegin).
type FragmentedBundles = BTreeMap<(Address, SeqNum), Rc<RefCell<FragmentedBundle>>>;

/// `OnceOffReceiver::OnceOffReceipts`: the two generations of received
/// once-off reliable packets (once_off_packet.cpp:65).
type OnceOffReceipts = (HashSet<(Address, SeqNum)>, HashSet<(Address, SeqNum)>);

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
/// channels (BW-02-03) or its tests. The packet is shared (`PacketPtr` in
/// the reference) because the receive window and the fragment chains store
/// it and chain onto it.
pub trait PacketSink {
    /// `processPacket`: consume the datagram, name the reason it failed
    /// (`REASON_SUCCESS` when it was delivered onward).
    fn process_packet(&self, addr: &Address, packet: Rc<RefCell<Packet>>) -> Reason;
}

/// The slice of `PacketReceiverStats` (packet_receiver_stats.hpp) the
/// channel's receive window touches: the duplicate-packet counter. The full
/// statistics container and its one-second gathering belong to the
/// EventDispatcher item (BW-02-01-05).
#[derive(Default)]
pub struct PacketReceiverStats {
    num_duplicate_packets_received: Cell<u32>,
}

impl PacketReceiverStats {
    pub fn new() -> Self {
        Self {
            num_duplicate_packets_received: Cell::new(0),
        }
    }

    /// `incDuplicatePackets()`.
    pub fn inc_duplicate_packets(&self) {
        self.num_duplicate_packets_received
            .set(self.num_duplicate_packets_received.get() + 1);
    }

    /// `numDuplicatePacketsReceived()`.
    pub fn num_duplicate_packets_received(&self) -> u32 {
        self.num_duplicate_packets_received.get()
    }
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
    /// `stats_`: the receive-path statistics (BW-02-03-05).
    stats: RefCell<PacketReceiverStats>,
    /// `onceOffReceiver_.fragmentedBundles()`: the off-channel fragment
    /// bundles, keyed by (address, fragBegin) (packet_receiver.cpp:1295).
    /// On-channel bundles live in the channel's own `pFragments_`.
    fragmented_bundles: RefCell<FragmentedBundles>,
    /// `onceOffReceiver_`'s received once-off reliable receipts
    /// (once_off_packet.cpp:65): two generations, so a resend of a received
    /// packet is dropped without losing legitimate new ones.
    once_off_receipts: RefCell<OnceOffReceipts>,
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
            stats: RefCell::new(PacketReceiverStats::new()),
            fragmented_bundles: RefCell::new(BTreeMap::new()),
            once_off_receipts: RefCell::new((HashSet::new(), HashSet::new())),
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

    /// `processPacket( addr, p, pStatsHelper )` (packet_receiver.cpp:445):
    /// the entry point for a received datagram: find the channel it belongs
    /// to (creating an anonymous one for an off-channel `CREATE_CHANNEL`
    /// packet on an internal interface), then parse and dispatch. The rate
    /// limiter and the channel / off-channel filters are the later item's
    /// seams.
    pub fn process_packet(
        &self,
        interface: &Rc<NetworkInterface>,
        addr: &Address,
        p: Rc<RefCell<Packet>>,
    ) -> Reason {
        // Packets arriving on an external interface may be encrypted, so
        // their header flags are not examined there.
        let p_channel = interface.find_or_create_channel(
            addr,
            !interface.is_external() && p.borrow().should_create_anonymous(),
        );

        if let Some(channel) = p_channel {
            // Addressed channels update their received time here; indexed
            // channels do so after their footer is stripped.
            channel.on_packet_received(p.borrow().total_size() as i32);

            // The channel filter (pFilter()->recv) is a seam.
            self.process_filtered_packet(interface, addr, p, Some(&channel))
        } else if interface.is_external() && interface.is_dead(addr) {
            // Ignoring incoming packet on recently dead channel.
            Reason::Success
        } else {
            // The off-channel filter is a seam.
            self.process_filtered_packet(interface, addr, p, None)
        }
    }

    /// `processFilteredPacket` (packet_receiver.cpp:518): validate the
    /// packet, strip and apply the ACK footers, then push reliable packets
    /// through the channel's receive window and dispatch the ordered chain.
    /// `p_channel` is `Some` for an addressed or indexed channel, `None`
    /// for once-off traffic.
    fn process_filtered_packet(
        &self,
        interface: &Rc<NetworkInterface>,
        addr: &Address,
        p: Rc<RefCell<Packet>>,
        p_channel: Option<&Rc<UDPChannel>>,
    ) -> Reason {
        if p.borrow().total_size() < std::mem::size_of::<u16>() {
            return Reason::CorruptedPacket;
        }

        // The packet monitor (pPacketMonitor_->packetIn) is a seam.

        // Make sure we understand all the flags.
        if p.borrow().flags() & !flags::KNOWN_FLAGS != 0 {
            return Reason::CorruptedPacket;
        }

        // The off-channel receive counter belongs to the statistics item.

        // Don't allow FLAG_CREATE_CHANNEL on external interfaces.
        if interface.is_external() && p.borrow().has_flags(flags::CREATE_CHANNEL) {
            return Reason::CorruptedPacket;
        }

        // Make sure there's something in the packet.
        if p.borrow().total_size() <= HEADER_SIZE {
            return Reason::CorruptedPacket;
        }

        // Start stripping footers: the checksum first.
        if !p.borrow_mut().validate_checksum() {
            return Reason::CorruptedPacket;
        }

        // Strip off piggybacks and process them immediately (they are older
        // than this packet). The visitation is the BW-02-03-04 seam.
        if !self.process_piggybacks(&p) {
            return Reason::CorruptedPacket;
        }

        // Strip off the indexed channel ID and version, if present.
        let mut p_channel = p_channel.cloned();
        if p.borrow().has_flags(flags::INDEXED_CHANNEL) {
            let Some(channel_id) = p.borrow_mut().strip_footer_u32() else {
                return Reason::CorruptedPacket;
            };
            let Some(channel_version) = p.borrow_mut().strip_footer_u32() else {
                return Reason::CorruptedPacket;
            };
            {
                let mut packet = p.borrow_mut();
                packet.set_channel_id(channel_id as i32);
                packet.set_channel_version(channel_version);
            }

            // `findIndexedChannel` (the channel-finder seam) may forward the
            // packet; the port looks in the condemned map and gives up when
            // the channel is gone.
            p_channel = interface.find_condemned_channel(channel_id as i32);
            if p_channel.is_none() {
                return Reason::Success;
            }
            if let Some(channel) = p_channel.as_ref() {
                channel.on_packet_received(p.borrow().total_size() as i32);
            }
        }

        if p_channel.is_none() && p.borrow().has_flags(flags::ON_CHANNEL) {
            p_channel = interface.find_channel_owned(addr);
            if p_channel.is_none() {
                // Dropping packet due to absence of local channel.
                return Reason::GeneralNetwork;
            }
        }

        if let Some(channel) = p_channel.as_ref() {
            if channel.has_remote_failed() {
                return Reason::GeneralNetwork;
            }
            if seq_less_than(p.borrow().channel_version(), channel.creation_version()) {
                // Dropping packet from old channel.
                return Reason::Success;
            }
            if channel.wants_first_packet() {
                if p.borrow().has_flags(flags::CREATE_CHANNEL) {
                    channel.got_first_packet();
                } else {
                    // Dropping packet on channel wanting FLAG_CREATE_CHANNEL.
                    return Reason::GeneralNetwork;
                }
            }
        }

        // Strip and handle the cumulative ACK.
        if p.borrow().has_flags(flags::HAS_CUMULATIVE_ACK) {
            let Some(channel) = p_channel.as_ref() else {
                return Reason::CorruptedPacket;
            };
            let Some(end_seq) = p.borrow_mut().strip_footer_u32() else {
                return Reason::CorruptedPacket;
            };
            if !channel.handle_cumulative_ack(end_seq) {
                return Reason::CorruptedPacket;
            }
        }

        // Strip and handle the ACK list.
        if p.borrow().has_flags(flags::HAS_ACKS) {
            let Some(n_acks) = p.borrow_mut().strip_footer_u8() else {
                return Reason::CorruptedPacket;
            };
            if n_acks == 0 {
                return Reason::CorruptedPacket;
            }

            let ack_size = n_acks as usize * std::mem::size_of::<SeqNum>();

            // Check that we have enough footers for the acks the packet
            // claims to have.
            if p.borrow().body_size() < ack_size {
                return Reason::CorruptedPacket;
            }

            if let Some(channel) = p_channel.as_ref() {
                for _ in 0..n_acks {
                    let Some(seq) = p.borrow_mut().strip_footer_u32() else {
                        return Reason::CorruptedPacket;
                    };
                    if !channel.handle_ack(seq) {
                        return Reason::CorruptedPacket;
                    }
                }
            } else if !p.borrow().has_flags(flags::ON_CHANNEL) {
                // Once-off: the resend timer cancellation is the once-off
                // sender seam; the ACKs are just stripped.
                for _ in 0..n_acks {
                    if p.borrow_mut().strip_footer_u32().is_none() {
                        return Reason::CorruptedPacket;
                    }
                }
            } else {
                // Got acks without a channel: drop them.
                p.borrow_mut().shrink(ack_size);
            }
        }

        // Strip the sequence number.
        if p.borrow().has_flags(flags::HAS_SEQUENCE_NUMBER) {
            let Some(seq) = p.borrow_mut().strip_footer_u32() else {
                return Reason::CorruptedPacket;
            };
            p.borrow_mut().set_seq(seq);
        }

        let mut should_send_channel = false;

        // Now do something if it's reliable.
        if p.borrow().has_flags(flags::IS_RELIABLE) {
            // First make sure it has a sequence number.
            if p.borrow().seq() == SEQ_NULL {
                return Reason::CorruptedPacket;
            }

            if let Some(channel) = p_channel.as_ref() {
                let result = channel.add_to_receive_window(p.clone(), *addr, &self.stats.borrow());

                if !channel.is_local_regular() {
                    should_send_channel = true;
                }

                if result != AddToReceiveWindowResult::PacketIsNextInWindow {
                    // Already received, or too early and buffered. ACK it
                    // immediately when the channel is established and
                    // irregular; a corrupt packet is a real failure.
                    if result != AddToReceiveWindowResult::PacketIsCorrupt {
                        if channel.is_established() && should_send_channel {
                            let mut empty = UDPBundle::new(0);
                            channel.send(&mut empty);
                        }
                        return Reason::Success;
                    }
                    return Reason::CorruptedPacket;
                }
            } else {
                // Once-off reliable: no such traffic on external interfaces.
                if interface.is_external() {
                    return Reason::Success;
                }

                // Send back the ack for this packet.
                let mut back_bundle = UDPBundle::new(0);
                back_bundle.add_ack(p.borrow().seq());
                interface.send(addr, &mut back_bundle, None);

                if self.on_reliable_received(*addr, p.borrow().seq()) {
                    return Reason::Success;
                }
            }
        } else if let Some(channel) = p_channel.as_ref() {
            // Unreliable: confirm the sequence number is incrementing, as a
            // sanity check against malicious parties.
            if channel.is_external() && !channel.validate_unreliable_seq_num(p.borrow().seq()) {
                return Reason::CorruptedPacket;
            }
        }

        // Push the packet chain through processOrderedPacket. Condemned
        // channels still deliver their remaining messages.
        let mut ret = Reason::Success;
        let mut curr = Some(p);
        while let Some(p_curr) = curr {
            if p_channel.as_ref().is_some_and(|c| c.is_condemned()) {
                break;
            }

            let p_next = p_curr.borrow().next();
            let r = self.process_ordered_packet(interface, addr, p_curr, p_channel.as_ref());
            if ret == Reason::Success {
                ret = r;
            }
            curr = p_next;
        }

        // If this bundle was delivered to a channel and there are still ACKs
        // to send, do it now.
        if let Some(channel) = p_channel.as_ref()
            && !channel.is_destroyed()
            && should_send_channel
            && channel.is_established()
            && channel.has_acks()
        {
            let mut empty = UDPBundle::new(0);
            channel.send(&mut empty);
        }

        ret
    }

    /// `processOrderedPacket` (packet_receiver.cpp:1170): dispatch a packet
    /// after reliable ordering has been imposed, reassembling fragmented
    /// bundles (the fragment chain the receive window or the off-channel
    /// map holds) before the message walk.
    fn process_ordered_packet(
        &self,
        interface: &Rc<NetworkInterface>,
        addr: &Address,
        p: Rc<RefCell<Packet>>,
        p_channel: Option<&Rc<UDPChannel>>,
    ) -> Reason {
        // The label used in debug output.
        let source = p_channel.map_or_else(|| addr.c_str(), |c| c.c_str());

        // Strip first request offset.
        if p.borrow().has_flags(flags::HAS_REQUESTS) {
            let Some(offset) = p.borrow_mut().strip_footer_u16() else {
                return Reason::CorruptedPacket;
            };
            p.borrow_mut().set_first_request_offset(offset);
            if offset as usize > p.borrow().msg_end_offset() {
                return Reason::CorruptedPacket;
            }
        }

        let is_on_channel = p_channel.is_some() && p.borrow().has_flags(flags::IS_RELIABLE);

        // Strip fragment footers.
        if !p.borrow().has_flags(flags::IS_FRAGMENT)
            && p.borrow().has_flags(flags::HAS_SEQUENCE_NUMBER)
            && (is_on_channel && p_channel.unwrap().p_fragments().is_some())
        {
            // Got a non-fragment packet when expecting a fragment.
            return Reason::CorruptedPacket;
        }

        let mut dispatch_packet = p.clone();
        if p.borrow().has_flags(flags::IS_FRAGMENT) {
            if !p.borrow_mut().strip_frag_info() {
                return Reason::CorruptedPacket;
            }

            let key = (*addr, p.borrow().frag_begin());

            // Find the existing packet chain for this bundle, if any.
            let mut p_fragments: Option<Rc<RefCell<FragmentedBundle>>> = if is_on_channel {
                p_channel.unwrap().p_fragments()
            } else {
                self.fragmented_bundles.borrow().get(&key).cloned()
            };

            // If the previous fragment is really old, this must be a failed
            // bundle that was never resent, so get rid of it.
            if let Some(ref fragments) = p_fragments
                && fragments.borrow().is_old(interface.dispatcher().now())
                && !is_on_channel
            {
                eprintln!(
                    "PacketReceiver::processOrderedPacket( {} ): Discarding abandoned stale overlapping fragmented bundle from seq {} to {}",
                    source,
                    p.borrow().frag_begin(),
                    fragments.borrow().last_fragment()
                );
                self.fragmented_bundles.borrow_mut().remove(&key);
                p_fragments = None;
            }

            if p_fragments.is_none() {
                // On a channel this must be the first packet in the bundle,
                // since channel traffic is ordered.
                if p_channel.is_some() && p.borrow().seq() != p.borrow().frag_begin() {
                    eprintln!(
                        "PacketReceiver::processOrderedPacket( {} ): Bundle (#{},#{}) is missing packets before #{}",
                        source,
                        p.borrow().frag_begin(),
                        p.borrow().frag_end(),
                        p.borrow().seq()
                    );
                    return Reason::CorruptedPacket;
                }

                // This is the first fragment of the bundle: make a new one.
                let new_bundle = Rc::new(RefCell::new(FragmentedBundle::new(
                    &p,
                    interface.dispatcher().now(),
                )));
                if is_on_channel {
                    p_channel
                        .unwrap()
                        .set_p_fragments(Some(Rc::clone(&new_bundle)));
                } else {
                    self.fragmented_bundles
                        .borrow_mut()
                        .insert(key, Rc::clone(&new_bundle));
                }
                return Reason::Success;
            }

            let p_fragments = p_fragments.unwrap();
            if !p_fragments.borrow_mut().add_packet(
                p.clone(),
                interface.is_external(),
                &source,
                interface.dispatcher().now(),
            ) {
                return Reason::CorruptedPacket;
            }

            // If the bundle is still incomplete, stop processing now.
            if !p_fragments.borrow().is_complete() {
                return Reason::Success;
            }

            // The bundle is complete: process the whole chain below.
            dispatch_packet = p_fragments.borrow().p_chain();

            if is_on_channel {
                p_channel.unwrap().set_p_fragments(None);
            } else {
                self.fragmented_bundles.borrow_mut().remove(&key);
            }
        }

        // We have a complete packet chain (or a single packet). Dispatch.
        let output_bundle = UDPBundleProcessor::new(Some(dispatch_packet), false);
        let channel_ref: Option<Rc<dyn ChannelRef>> =
            p_channel.map(|c| Rc::clone(c) as Rc<dyn ChannelRef>);

        let reason = output_bundle.dispatch_messages(
            interface.interface_table(),
            addr,
            channel_ref.as_ref(),
        );

        if reason == Reason::CorruptedPacket {
            return Reason::CorruptedPacket;
        }
        reason
    }

    /// `processPiggybacks` (packet_receiver.cpp:1205): visit and process the
    /// piggyback packets on `p` before its own footers. The port never
    /// emits piggybacks (the BW-02-03-04 seam), so a packet claiming them
    /// cannot be parsed and is refused.
    fn process_piggybacks(&self, p: &Rc<RefCell<Packet>>) -> bool {
        !p.borrow().has_flags(flags::HAS_PIGGYBACKS)
    }

    /// `OnceOffReceiver::onReliableReceived` (once_off_packet.cpp:65): is a
    /// once-off reliable packet already accepted? A resend of an accepted
    /// packet is dropped; the 30-second cleanup timer that rotates the two
    /// receipt generations is the dispatcher seam.
    fn on_reliable_received(&self, addr: Address, seq: SeqNum) -> bool {
        let mut receipts = self.once_off_receipts.borrow_mut();
        let (curr, prev) = &mut *receipts;
        if curr.contains(&(addr, seq)) || prev.contains(&(addr, seq)) {
            return true;
        }
        curr.insert((addr, seq));
        false
    }

    /// `onceOffReliableCleanup()` (once_off_packet.cpp:114): demote the
    /// recent receipts to less-recent and clear the oldest.
    pub fn once_off_reliable_cleanup(&self) {
        let mut receipts = self.once_off_receipts.borrow_mut();
        let (curr, prev) = &mut *receipts;
        prev.clear();
        std::mem::swap(curr, prev);
    }

    /// `stats()`: the receive-path statistics slice.
    pub fn stats(&self) -> std::cell::Ref<'_, PacketReceiverStats> {
        self.stats.borrow()
    }

    /// Run `body` against the attached interface, when there is one. The
    /// shared handle is passed so `processPacket`'s `findChannel` can call
    /// the `self: &Rc<Self>` arms.
    fn with_interface<T>(&self, body: impl FnOnce(&Rc<NetworkInterface>) -> T) -> Option<T> {
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
        interface: &Rc<NetworkInterface>,
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
        // one to processPacket. The packet is shared (`PacketPtr`) because
        // the receive window and the fragment chains take it.
        let current_packet = std::mem::take(&mut *self.next_packet.borrow_mut());
        let current_packet = Rc::new(RefCell::new(current_packet));

        let reason = interface.process_packet(source_address, current_packet);

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
}

impl InputNotificationHandler for PacketReceiver {
    fn handle_input_notification(&self, _fd: i32) -> i32 {
        self.handle_input()
    }

    fn handle_error_notification(&self, fd: i32) -> bool {
        self.handle_error(fd)
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
        fn process_packet(&self, addr: &Address, packet: Rc<RefCell<Packet>>) -> Reason {
            self.packets
                .borrow_mut()
                .push((*addr, packet.borrow().data().to_vec()));
            Reason::Success
        }
    }

    /// A sink that rejects everything with a fixed reason, for the
    /// processPacket report arm.
    pub struct RefusingSink {
        pub reason: Reason,
    }

    impl PacketSink for RefusingSink {
        fn process_packet(&self, _addr: &Address, _packet: Rc<RefCell<Packet>>) -> Reason {
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
    use foundation::stream::BinaryStream;

    use crate::endpoint::Endpoint;
    use crate::interface_element::{FIXED_LENGTH_MESSAGE, InterfaceElement};
    use crate::interfaces::InputMessageHandler;
    use crate::misc::{MessageID, REPLY_ID_NONE, ReplyID};
    use crate::network_interface::InterfaceType;
    use crate::udp_bundle::ReliableType;
    use crate::unpacked_message_header::UnpackedMessageHeader;
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
        fn addr(&self) -> Address {
            crate::endpoint::NONE
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
    /// packet is not reportable). The datagrams are valid header-only
    /// packets (zero flags, one body byte) so they clear the filtered
    /// path's guards and reach the sink.
    #[test]
    fn every_queued_datagram_is_processed_in_one_notification() {
        let (interface, _receiver, sink, reporter) = listening_interface();

        for index in 0..3u8 {
            send_to(&interface, &[0, 0, b'x' + index]);
        }

        assert!(
            interface.poller().process_pending_events(0.05) >= 1,
            "the socket reported input"
        );
        {
            let packets = sink.packets.borrow();
            assert_eq!(packets.len(), 3);
            for (index, (_, payload)) in packets.iter().enumerate() {
                assert_eq!(payload, &[0, 0, b'x' + index as u8]);
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
            send_to(&interface, &[0, 0, b'y' + index]);
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

        let sender = send_to(&interface, &[0, 0, b'r']);
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

        let sender = send_to(&a, &[0, 0, b'a']);
        drop(sender);
        send_to(&b, &[0, 0, b'b']);

        // Poll only A: its socket saw its own datagram, never B's.
        assert!(a.poller().process_pending_events(0.05) >= 1);
        {
            let packets = sink_a.packets.borrow();
            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0].1, &[0, 0, b'a']);
        }
        assert!(sink_b.packets.borrow().is_empty());

        // Poll B: its own datagram, and A stays untouched.
        assert!(b.poller().process_pending_events(0.05) >= 1);
        {
            let packets = sink_b.packets.borrow();
            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0].1, &[0, 0, b'b']);
        }
        assert_eq!(sink_a.packets.borrow().len(), 1);
    }

    /// One delivery: `(identifier, length, reply_id, body bytes)`.
    type Delivery = (MessageID, i32, ReplyID, Vec<u8>);

    /// A delivery that records what it was handed.
    struct Recorder {
        records: Rc<RefCell<Vec<Delivery>>>,
    }

    impl Recorder {
        fn shared() -> (Rc<Self>, Rc<RefCell<Vec<Delivery>>>) {
            let records = Rc::new(RefCell::new(Vec::new()));
            (
                Rc::new(Self {
                    records: Rc::clone(&records),
                }),
                records,
            )
        }
    }

    impl InputMessageHandler for Recorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            self.records.borrow_mut().push((
                header.identifier,
                header.length,
                header.reply_id,
                data.as_bytes().to_vec(),
            ));
        }
    }

    /// Read one datagram off the interface's non-blocking socket, waiting
    /// briefly for the loopback delivery to land.
    fn wait_frame(interface: &Rc<NetworkInterface>) -> (Packet, Address) {
        for _ in 0..100 {
            let mut frame = Packet::new();
            let mut source = Address::default();
            {
                let socket = interface.udp_socket();
                if frame.recv_from_endpoint(&socket, &mut source) > 0 {
                    return (frame, source);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("no datagram arrived on the loopback");
    }

    /// BW-02-03-07: two real loopback interfaces exchange a reliable message
    /// in each direction. A's first send is dropped at the sender, so its
    /// resend lands on B instead; each receiver auto-acks, so both windows
    /// drain, and the resend counters track what went out twice.
    #[test]
    fn two_directions_exchange_and_resends_are_counted() {
        let (interface_a, _receiver_a) =
            NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let (interface_b, _receiver_b) =
            NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let addr_a = interface_a.address();
        let addr_b = interface_b.address();

        let ping = InterfaceElement::new("ping", 1, FIXED_LENGTH_MESSAGE, 3, None);
        let (recorder_a, records_a) = Recorder::shared();
        let (recorder_b, records_b) = Recorder::shared();
        interface_a.interface_table().serve(
            &ping,
            Some(Rc::clone(&recorder_a) as Rc<dyn InputMessageHandler>),
        );
        interface_b.interface_table().serve(
            &ping,
            Some(Rc::clone(&recorder_b) as Rc<dyn InputMessageHandler>),
        );

        // Each side holds the channel for its peer. Both sides are irregular:
        // the receiver pushes an ACK as soon as it queues one, and the
        // sender's resend scan uses the inactivity rule.
        let channel_a = UDPChannel::get(&interface_a, addr_b);
        let channel_b = UDPChannel::get(&interface_b, addr_a);
        channel_a.debug_set_local_regular(false);
        channel_b.debug_set_local_regular(false);
        channel_a.debug_set_remote_regular(false);
        channel_b.debug_set_remote_regular(false);

        // A -> B: the first send is dropped at the sender, so nothing reaches
        // B. The packet is already in A's window, and a resend pushes it out.
        channel_a.set_should_drop_next_send(true);
        {
            let mut bundle = UDPBundle::new(0);
            bundle.start_message(&ping, ReliableType::Driver);
            bundle.add_blob(&[0x11, 0x22, 0x33]);
            channel_a.send(&mut bundle);
        }
        assert_eq!(
            channel_a.num_reliable_packets_sent(),
            1,
            "the reliable send is counted"
        );
        assert_eq!(channel_a.num_packets_resent(), 0, "nothing resent yet");
        assert!(
            channel_a.has_unacked_packets(),
            "the packet waits in A's window"
        );
        assert_eq!(
            interface_a
                .packet_sender()
                .sending_stats()
                .num_packets_resent
                .get(),
            0,
            "the sender has resent nothing"
        );

        channel_a.debug_make_packets_old();
        channel_a.check_resend_timers();
        assert_eq!(
            channel_a.num_packets_resent(),
            1,
            "the aged packet is resent once"
        );
        assert_eq!(
            interface_a
                .packet_sender()
                .sending_stats()
                .num_packets_resent
                .get(),
            1,
            "the sender counted the resend"
        );

        // B receives the resent packet, dispatches it, and auto-acks back.
        let (frame, source) = wait_frame(&interface_b);
        assert_eq!(
            interface_b.process_packet(&source, Rc::new(RefCell::new(frame))),
            Reason::Success
        );
        assert_eq!(
            *records_b.borrow(),
            vec![(1u8, 3, REPLY_ID_NONE, vec![0x11, 0x22, 0x33])],
            "B delivered the message A resent"
        );

        // A receives B's ACK, which releases the packet from A's window.
        let (frame, source) = wait_frame(&interface_a);
        assert_eq!(
            interface_a.process_packet(&source, Rc::new(RefCell::new(frame))),
            Reason::Success
        );
        assert!(
            !channel_a.has_unacked_packets(),
            "the ACK drained A's window"
        );

        // B -> A: the same exchange, without the drop.
        channel_b.set_should_drop_next_send(true);
        {
            let mut bundle = UDPBundle::new(0);
            bundle.start_message(&ping, ReliableType::Driver);
            bundle.add_blob(&[0xaa, 0xbb, 0xcc]);
            channel_b.send(&mut bundle);
        }
        assert_eq!(
            channel_b.num_reliable_packets_sent(),
            1,
            "the reliable send is counted"
        );
        assert_eq!(channel_b.num_packets_resent(), 0, "nothing resent yet");
        assert!(
            channel_b.has_unacked_packets(),
            "the packet waits in B's window"
        );

        channel_b.debug_make_packets_old();
        channel_b.check_resend_timers();
        assert_eq!(
            channel_b.num_packets_resent(),
            1,
            "the aged packet is resent once"
        );
        assert_eq!(
            interface_b
                .packet_sender()
                .sending_stats()
                .num_packets_resent
                .get(),
            1,
            "B's sender counted the resend"
        );

        // A receives the resent packet, dispatches it, and auto-acks back.
        let (frame, source) = wait_frame(&interface_a);
        assert_eq!(
            interface_a.process_packet(&source, Rc::new(RefCell::new(frame))),
            Reason::Success
        );
        assert_eq!(
            *records_a.borrow(),
            vec![(1u8, 3, REPLY_ID_NONE, vec![0xaa, 0xbb, 0xcc])],
            "A delivered the message B resent"
        );

        // B receives A's ACK, which releases B's packet.
        let (frame, source) = wait_frame(&interface_b);
        assert_eq!(
            interface_b.process_packet(&source, Rc::new(RefCell::new(frame))),
            Reason::Success
        );
        assert!(
            !channel_b.has_unacked_packets(),
            "the ACK drained B's window"
        );
    }
}
