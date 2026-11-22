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
use std::rc::{Rc, Weak};

use platform::socket;

use crate::endpoint::Address;
use crate::misc::Reason;
use crate::network_interface::NetworkInterface;
use crate::packet::Packet;
use crate::poller::InputNotificationHandler;

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
            && let Some(channel) = interface.find_channel(offender)
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
        let dead_channel = interface.find_channel(offender);

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
        if let Some(channel) = interface.find_channel(offender) {
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
}
