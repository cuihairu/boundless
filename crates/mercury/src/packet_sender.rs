//! `lib/network/packet_sender.{hpp,cpp}`: the interface's socket write
//! path -- turn a finalised bundle into datagrams, count what went out,
//! and (when asked) drop or delay packets to fake a lossy link.
//!
//! The reference's `PacketSender` holds the socket, the once-off sender,
//! and the `SendingStats` watcher block. The port hands it a weak handle
//! to its interface instead of the socket: the socket lives on the
//! interface, and the weak handle is what a delayed send needs to reach
//! it when the rescheduling timer fires.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use foundation::time_queue::{TimerHandle, TimerHandler};

use crate::endpoint::Address;
use crate::misc::{Reason, SeqNumAllocator};
use crate::network_interface::NetworkInterface;
use crate::packet::{Packet, flags};
use crate::udp_bundle::UdpBundle;
use crate::udp_channel::ChannelPtr;
use platform::socket;

/// `UDP_OVERHEAD` (basictypes.hpp): the 8 UDP + 20 IP header bytes a
/// datagram carries, counted into the byte totals the reference keeps.
pub const UDP_OVERHEAD: u32 = 28;

/// `Mercury::SendingStats` (`sending_stats.hpp`): the outbound counters
/// the watchers expose. Only what the send path touches is kept here; the
/// timers and filters of the reference's block are BW-02-05 and later.
#[derive(Debug, Default, Clone)]
pub struct SendingStats {
    /// `numPacketsSent_`.
    pub num_packets_sent: u32,
    /// `numPacketsResent_`.
    pub num_packets_resent: u32,
    /// `numBytesSent_`.
    pub num_bytes_sent: u64,
    /// `numBytesResent_`.
    pub num_bytes_resent: u64,
    /// `numBundlesSent_`.
    pub num_bundles_sent: u32,
    /// `numMessagesSent_`.
    pub num_messages_sent: u32,
    /// `numReliableMessagesSent_`.
    pub num_reliable_messages_sent: u32,
    /// `numPacketsSentOffChannel_`.
    pub num_packets_sent_off_channel: u32,
    /// `numPiggybacks_`.
    pub num_piggybacks: u32,
    /// `numBytesPiggybacked_`.
    pub num_bytes_piggybacked: u64,
    /// `numBytesNotPiggybacked_`.
    pub num_bytes_not_piggybacked: u64,
    /// `numFailedPacketSend_`.
    pub num_failed_packet_send: u32,
}

/// `Mercury::PacketLossParameters` (`packet_loss_parameters.hpp`): the
/// artificial loss and latency knobs, applied in
/// [`reschedule_send`](PacketSender::reschedule_send).
pub struct PacketLossParameters {
    /// `maxLatency_`, seconds.
    max_latency: f32,
    /// `minLatency_`, seconds.
    min_latency: f32,
    /// `lossRatio_`, 0..1.
    loss_ratio: f32,
    /// `maxLatencyMillion_`, milliseconds.
    max_latency_million: i64,
    /// `minLatencyMillion_`, milliseconds.
    min_latency_million: i64,
    /// `dropPerMillion_`, packets in a million.
    drop_per_million: u32,
}

impl Default for PacketLossParameters {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketLossParameters {
    /// All zeros: no artificial loss, no latency.
    pub fn new() -> Self {
        Self {
            max_latency: 0.0,
            min_latency: 0.0,
            loss_ratio: 0.0,
            max_latency_million: 0,
            min_latency_million: 0,
            drop_per_million: 0,
        }
    }

    /// `set( lossRatio, minLatency, maxLatency )`.
    pub fn set(&mut self, loss_ratio: f32, min_latency: f32, max_latency: f32) {
        self.set_loss_ratio(loss_ratio);
        self.set_min_latency(min_latency);
        self.set_max_latency(max_latency);
    }

    /// `lossRatio( value )`: non-zero loss is its own warning. The
    /// reference's message has no space after the colon: the C++
    /// concatenates `"PacketLossParameters::lossRatio:"` with
    /// `"Loss ratio set to %.2f\n"` (packet_loss_parameters.cpp:58-60).
    pub fn set_loss_ratio(&mut self, value: f32) {
        self.loss_ratio = value;
        self.drop_per_million = (value * 1_000_000.0).max(0.0) as u32;
        if value != 0.0 {
            eprintln!(
                "PacketLossParameters::lossRatio:Loss ratio set to {:.2}",
                value
            );
        }
    }

    /// `minLatency( value )`: seconds. A non-zero value warns, quoting the
    /// whole window in seconds while the reference's text says
    /// "milliseconds" -- kept verbatim from the quirk
    /// (packet_loss_parameters.cpp:88-92).
    pub fn set_min_latency(&mut self, value: f32) {
        self.min_latency = value;
        self.min_latency_million = (value * 1000.0).max(0.0) as i64;
        if value != 0.0 {
            eprintln!(
                "PacketLossParameters::latencyMin: Latency enabled. In range from {:.2} to {:.2} milliseconds.",
                self.min_latency, self.max_latency
            );
        }
    }

    /// `maxLatency( value )`: seconds. A non-zero value warns like the
    /// minimum's does (packet_loss_parameters.cpp:116-120).
    pub fn set_max_latency(&mut self, value: f32) {
        self.max_latency = value;
        self.max_latency_million = (value * 1000.0).max(0.0) as i64;
        if value != 0.0 {
            eprintln!(
                "PacketLossParameters::maxLatency: Latency enabled. In range from {:.2} to {:.2} milliseconds.",
                self.min_latency, self.max_latency
            );
        }
    }

    /// `dropPerMillion( value )`: the watcher-facing per-million arm
    /// round-trips through the ratio (packet_loss_parameters.cpp:72-76).
    pub fn set_drop_per_million(&mut self, value: i32) {
        self.set_loss_ratio(value as f32 / 1_000_000.0);
    }

    /// `minLatencyMillion( value )` (packet_loss_parameters.cpp:99-102).
    pub fn set_min_latency_million(&mut self, value: i32) {
        self.set_min_latency(value as f32 / 1000.0);
    }

    /// `maxLatencyMillion( value )` (packet_loss_parameters.cpp:129-132).
    pub fn set_max_latency_million(&mut self, value: i32) {
        self.set_max_latency(value as f32 / 1000.0);
    }

    /// `lossRatio()`.
    pub fn loss_ratio(&self) -> f32 {
        self.loss_ratio
    }

    /// `minLatency()`, seconds.
    pub fn min_latency(&self) -> f32 {
        self.min_latency
    }

    /// `maxLatency()`, seconds.
    pub fn max_latency(&self) -> f32 {
        self.max_latency
    }

    /// `dropPerMillion()`.
    pub fn drop_per_million(&self) -> u32 {
        self.drop_per_million
    }

    /// `maxLatencyMillion()`.
    pub fn max_latency_million(&self) -> i64 {
        self.max_latency_million
    }

    /// `minLatencyMillion()`.
    pub fn min_latency_million(&self) -> i64 {
        self.min_latency_million
    }

    /// `hasArtificialLossOrLatency()` (packet_loss_parameters.hpp): any
    /// knob sits at a non-zero value in its integer unit.
    pub fn has_artificial_loss_or_latency(&self) -> bool {
        self.drop_per_million != 0 || self.min_latency_million != 0 || self.max_latency_million != 0
    }
}

/// The last errno of a failed OS call on this thread.
fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `basicSendSingleTry`'s errno switch (packet_sender.cpp:370-390).
fn map_send_errno(err: i32) -> Reason {
    match err {
        socket::ECONNREFUSED => Reason::NoSuchPort,
        socket::EAGAIN => Reason::ResourceUnavailable,
        socket::ENOBUFS => Reason::TransmitQueueFull,
        socket::EMSGSIZE => Reason::MessageTooLong,
        _ => Reason::GeneralNetwork,
    }
}

/// `Mercury::PacketSender` (`packet_sender.cpp`).
pub struct PacketSender {
    /// The interface this sender writes through, weak so the send timer
    /// does not keep it alive.
    interface: Weak<NetworkInterface>,
    /// `seqNumAllocator_`, for off-channel sequence numbers.
    seq_num_allocator: SeqNumAllocator,
    /// `sendingStats_`, shared so the filter chain's next hop can count
    /// from a `&self` (the port's `PacketFilter` hands it a `Fn`).
    stats: RefCell<SendingStats>,
    /// `packetLossParameters_`.
    loss: PacketLossParameters,
    /// `isDroppingNextSend_`.
    is_dropping_next_send: bool,
    /// `shouldUseChecksums_`.
    should_use_checksums: bool,
    /// The reference's `rand()` stream, seeded 1 like the C default.
    rand_state: Cell<u32>,
}

impl PacketSender {
    /// `PacketSender( ... )`: the sender starts with the allocator at 1
    /// and no artificial loss. The weak interface is filled in once the
    /// interface exists.
    pub fn new(interface: Weak<NetworkInterface>) -> Self {
        Self {
            interface,
            seq_num_allocator: SeqNumAllocator::new(1),
            stats: RefCell::new(SendingStats::default()),
            loss: PacketLossParameters::new(),
            is_dropping_next_send: false,
            should_use_checksums: false,
            rand_state: Cell::new(1),
        }
    }

    /// `sendingStats_`.
    pub fn stats(&self) -> std::cell::Ref<'_, SendingStats> {
        self.stats.borrow()
    }

    /// `packetLossParameters_`.
    pub fn loss(&self) -> &PacketLossParameters {
        &self.loss
    }

    /// The mutable loss knobs, so `NetworkInterface::setLossRatio` etc.
    /// reach them.
    pub fn loss_mut(&mut self) -> &mut PacketLossParameters {
        &mut self.loss
    }

    /// `shouldUseChecksums_`.
    pub fn should_use_checksums(&self) -> bool {
        self.should_use_checksums
    }

    /// `shouldUseChecksums( value )`.
    pub fn set_should_use_checksums(&mut self, value: bool) {
        self.should_use_checksums = value;
    }

    /// `dropNextSend()`: the next packet is counted and dropped, exactly
    /// once.
    pub fn drop_next_send(&mut self) {
        self.is_dropping_next_send = true;
    }

    /// `PacketSender::send( address, bundle, pChannel )`
    /// (packet_sender.cpp:61): finalise an unfinalised bundle, then send
    /// every packet of it up to the first overflow packet. Off-channel
    /// bundles are closed here; a channel's own bundle was closed by
    /// [`Channel::send`](crate::udp_channel::UdpChannel::send) already.
    pub fn send(
        &mut self,
        address: &Address,
        bundle: &mut UdpBundle,
        channel: Option<&ChannelPtr>,
    ) {
        assert!(
            !address.is_none(),
            "PacketSender::send: MF_ASSERT( address != Address::NONE )"
        );

        if !bundle.is_finalised() {
            bundle.finalise();
            if let Some(interface) = self.interface.upgrade()
                && let Some(manager) = interface.request_manager()
            {
                bundle.add_reply_orders_to(&manager, Some(*address));
            }
        }

        let num_packets = bundle.num_data_units();
        let first_overflow = bundle
            .prepare_packets(
                &mut self.seq_num_allocator,
                self.should_use_checksums,
                channel,
            )
            .unwrap_or(num_packets)
            .min(num_packets);

        // Do not send overflow packets.
        for index in 0..first_overflow {
            let packet = Rc::new(bundle.packet(index).clone());
            self.send_packet(address, &packet, channel, false);
        }

        let mut stats = self.stats.borrow_mut();
        stats.num_bundles_sent += 1;
        stats.num_messages_sent += bundle.num_messages();
        stats.num_reliable_messages_sent += bundle.num_reliable_messages();
    }

    /// `PacketSender::sendPacket( address, pPacket, pChannel, isResend )`
    /// (packet_sender.cpp:114): count the packet, then hand it to the
    /// rescheduling check. The once-off reliable resend timers the
    /// reference arms for off-channel packets are BW-02-03-07's deferred
    /// `OnceOffSender`.
    pub fn send_packet(
        &mut self,
        address: &Address,
        packet: &Rc<Packet>,
        channel: Option<&ChannelPtr>,
        is_resend: bool,
    ) {
        if is_resend {
            let mut stats = self.stats.borrow_mut();
            stats.num_bytes_resent += packet.total_size() as u64;
            stats.num_packets_resent += 1;
        } else if !packet.has_flags(flags::ON_CHANNEL) {
            self.stats.borrow_mut().num_packets_sent_off_channel += 1;
        }

        if !self.reschedule_send(address, packet, channel) {
            self.send_rescheduled_packet(address, packet, channel);
        }
    }

    /// `PacketSender::rescheduleSend` (packet_sender.cpp:158): drop the
    /// packet under artificial loss, delay it under artificial latency,
    /// or let it go now.
    ///
    /// The loss draw is the port's seeded xorshift in place of the C
    /// `rand()`: the reference gates on `rand() < dropPerMillion * RAND_MAX
    /// / 1000000` (packet_sender.cpp:167), uniform over the draws; the
    /// port gates on `rand() % 1_000_000 < drop_per_million`, uniform over
    /// its own stream, and stays deterministic for tests.
    fn reschedule_send(
        &mut self,
        addr: &Address,
        packet: &Rc<Packet>,
        channel: Option<&ChannelPtr>,
    ) -> bool {
        let drop_per_million = self.loss.drop_per_million();
        let hit_loss = drop_per_million != 0 && self.next_rand() % 1_000_000 < drop_per_million;

        if self.is_dropping_next_send || hit_loss {
            // The reference counts a dropped packet as sent.
            self.is_dropping_next_send = false;
            let mut stats = self.stats.borrow_mut();
            stats.num_bytes_sent += packet.total_size() as u64 + UDP_OVERHEAD as u64;
            stats.num_packets_sent += 1;
            return true;
        }

        let max_latency_million = self.loss.max_latency_million();
        if max_latency_million == 0 {
            return false;
        }

        let min_latency_million = self.loss.min_latency_million();
        let mut latency = min_latency_million;
        if max_latency_million > min_latency_million {
            latency += (self.next_rand() as i64) % (max_latency_million - min_latency_million);
        }

        if latency < 2 {
            return false; // 2ms => send now
        }

        // Delay this packet then.
        if let Some(interface) = self.interface.upgrade() {
            let sender = RescheduledSender::new(
                interface.clone(),
                *addr,
                Rc::clone(packet),
                channel.cloned(),
            );
            interface.dispatcher().add_once_off_timer(
                latency * 1000,
                Rc::new(sender),
                0,
                "RescheduledSender",
            );
        }
        true
    }

    /// `PacketSender::sendRescheduledPacket` (packet_sender.cpp:241): the
    /// channel's filter gets the packet first; off-channel packets (and
    /// channels without a filter) go straight out.
    pub(crate) fn send_rescheduled_packet(
        &self,
        addr: &Address,
        packet: &Rc<Packet>,
        channel: Option<&ChannelPtr>,
    ) {
        let filter = channel.and_then(|c| c.borrow().p_filter());
        match filter {
            Some(filter) => {
                let mut wire = (**packet).clone();
                filter.send(addr, &mut wire, &|addr, packet| {
                    self.basic_send_with_retries(addr, packet);
                });
            }
            None => {
                self.basic_send_with_retries(addr, packet);
            }
        }
    }

    /// `PacketSender::basicSendWithRetries` (packet_sender.cpp:274):
    /// transient queue errors wait a moment and retry. The base
    /// `PacketFilter::send` (packet_filter.cpp:34) sends through here.
    pub(crate) fn basic_send_with_retries(&self, addr: &Address, packet: &Packet) -> Reason {
        let mut retries = 0;
        let mut reason = Reason::Success;

        while retries <= 3 {
            retries += 1;
            reason = self.basic_send_single_try(addr, packet);

            if reason == Reason::Success {
                return reason;
            }
            if reason == Reason::NoSuchPort {
                continue;
            }
            if reason == Reason::ResourceUnavailable || reason == Reason::TransmitQueueFull {
                eprintln!(
                    "PacketSender::basicSendWithRetries: Transmit queue full, waiting for space... ({})",
                    retries
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            break;
        }

        reason
    }

    /// `PacketSender::basicSendSingleTry` (packet_sender.cpp:347): one
    /// `sendto`, mapping the errno back to a `Reason`.
    fn basic_send_single_try(&self, addr: &Address, packet: &Packet) -> Reason {
        let Some(interface) = self.interface.upgrade() else {
            return Reason::GeneralNetwork;
        };
        let socket = interface.udp_socket();
        let len = socket.sendto_address(packet.wire(), *addr);

        if len == packet.total_size() as i32 {
            let mut stats = self.stats.borrow_mut();
            stats.num_bytes_sent += len as u64 + UDP_OVERHEAD as u64;
            stats.num_packets_sent += 1;
            Reason::Success
        } else {
            self.stats.borrow_mut().num_failed_packet_send += 1;
            map_send_errno(last_errno())
        }
    }

    /// The `rand()` stream the reference seeds once at 1: a small
    /// xorshift, deterministic for tests.
    fn next_rand(&self) -> u32 {
        let mut x = self.rand_state.get();
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rand_state.set(x);
        x
    }
}

/// `Mercury::RescheduledSender` (rescheduled_sender.cpp:15): a packet
/// held for its artificial latency, sent when the once-off timer fires.
/// The reference hands it the owning `PacketSender`; the port hands it the
/// interface, so the delayed datagram re-enters the interface's send path
/// (filter first, then the socket) and the sender's own counters see only
/// what left immediately.
struct RescheduledSender {
    /// The interface to write through at fire time.
    interface: Rc<NetworkInterface>,
    /// `addr_`.
    addr: Address,
    /// `pPacket_`.
    packet: Rc<Packet>,
    /// `pChannel_`: the channel this was sent on, or None off-channel.
    channel: Option<ChannelPtr>,
}

impl RescheduledSender {
    fn new(
        interface: Rc<NetworkInterface>,
        addr: Address,
        packet: Rc<Packet>,
        channel: Option<ChannelPtr>,
    ) -> Self {
        Self {
            interface,
            addr,
            packet,
            channel,
        }
    }
}

impl TimerHandler<u64> for RescheduledSender {
    fn handle_timeout(&self, _handle: TimerHandle<u64>, _user: usize) {
        let channel = self.channel.as_ref();
        self.interface
            .send_rescheduled_packet(&self.addr, &self.packet, channel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_dispatcher::EventDispatcher;
    use crate::network_interface::InterfaceType;
    use crate::packet_filter::PacketFilter;
    use crate::packet_receiver::PacketReceiver;
    use std::cell::Cell as SoloCell;

    /// A counting filter: the fixture proves the channel's filter sees
    /// each packet before the wire does.
    struct CountingFilter {
        sends: Rc<SoloCell<u32>>,
    }

    impl PacketFilter for CountingFilter {
        fn recv(
            &self,
            addr: &Address,
            packet: &mut Packet,
            next: &dyn Fn(&Address, &mut Packet) -> Reason,
        ) -> Reason {
            next(addr, packet)
        }

        fn send(&self, addr: &Address, packet: &mut Packet, next: &dyn Fn(&Address, &mut Packet)) {
            self.sends.set(self.sends.get() + 1);
            next(addr, packet)
        }
    }

    /// `127.0.0.1` as the repo stores it.
    const LOOPBACK: u32 = 0x0100_007f;

    /// An internal interface on the loopback driven by a pinned clock,
    /// with a fresh sender pointed at it (the interface's own sender is
    /// left untouched).
    fn sender_under_pinned_clock() -> (
        Rc<EventDispatcher>,
        Rc<SoloCell<u64>>,
        Rc<NetworkInterface>,
        PacketSender,
    ) {
        let clock = Rc::new(SoloCell::new(1_000_000u64));
        let clock_handle = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_handle.get()
        })));
        let interface =
            NetworkInterface::new_with_dispatcher(InterfaceType::Internal, Rc::clone(&dispatcher));
        let receiver = PacketReceiver::new();
        assert!(interface.recreate_listening_socket(&receiver, 0, "127.0.0.1"));
        receiver.attach_interface(&interface);
        let sender = PacketSender::new(Rc::downgrade(&interface));
        (dispatcher, clock, interface, sender)
    }

    /// A four-message bundle of fixed-length messages.
    fn filled_bundle() -> UdpBundle {
        use crate::interface_element::{FIXED_LENGTH_MESSAGE, InterfaceElement};
        let mut bundle = UdpBundle::new(0);
        let element = InterfaceElement::new("m", 1, FIXED_LENGTH_MESSAGE, 4, None);
        for index in 0..4u32 {
            bundle.start_message(&element, crate::udp_bundle::ReliableType::Driver);
            bundle.write_bytes(&index.to_le_bytes());
        }
        bundle
    }

    /// A real loopback socket for the far end, plus the address to send
    /// to it: the fixture's wire. Reads time out quickly, so "nothing
    /// arrived" asserts instead of hanging.
    fn wire_end() -> (std::net::UdpSocket, Address) {
        let wire = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        wire.set_read_timeout(Some(std::time::Duration::from_millis(100)))
            .unwrap();
        let port = wire.local_addr().unwrap().port();
        let addr = Address::new(LOOPBACK, socket::htons(port));
        (wire, addr)
    }

    /// The knobs translate seconds and ratios to their integer units, and
    /// the constructors agree (`packet_loss_parameters.cpp`).
    #[test]
    fn the_loss_knobs_translate_and_default() {
        let mut loss = PacketLossParameters::default();
        assert_eq!(loss.drop_per_million(), 0);
        assert_eq!(loss.max_latency_million(), 0);
        assert_eq!(loss.min_latency_million(), 0);
        assert_eq!(
            PacketLossParameters::new().drop_per_million(),
            loss.drop_per_million(),
            "new and default start from zero"
        );

        loss.set(0.5, 0.25, 0.75);
        assert_eq!(loss.drop_per_million(), 500_000);
        assert_eq!(loss.min_latency_million(), 250);
        assert_eq!(loss.max_latency_million(), 750);

        // A negative ratio clamps to zero rather than wrapping.
        loss.set_loss_ratio(-1.0);
        assert_eq!(loss.drop_per_million(), 0);
        loss.set_min_latency(-1.0);
        assert_eq!(loss.min_latency_million(), 0);
        loss.set_max_latency(-1.0);
        assert_eq!(loss.max_latency_million(), 0);

        // The per-million arms round-trip through the ratio
        // (packet_loss_parameters.cpp:72-76, 99-102, 129-132).
        loss.set_drop_per_million(250_000);
        assert_eq!(loss.drop_per_million(), 250_000);
        assert_eq!(loss.loss_ratio(), 0.25);
        loss.set_min_latency_million(40);
        assert_eq!(loss.min_latency_million(), 40);
        assert_eq!(loss.min_latency(), 0.04);
        loss.set_max_latency_million(80);
        assert_eq!(loss.max_latency_million(), 80);
        assert_eq!(loss.max_latency(), 0.08);

        // The window state face (packet_loss_parameters.hpp:43-47).
        let idle = PacketLossParameters::default();
        assert!(!idle.has_artificial_loss_or_latency());
        loss.set(0.5, 0.25, 0.75);
        assert!(loss.has_artificial_loss_or_latency());
    }

    /// `basicSendSingleTry`'s errno switch (packet_sender.cpp:370-390).
    #[test]
    fn send_errnos_map_to_reasons() {
        assert_eq!(map_send_errno(socket::ECONNREFUSED), Reason::NoSuchPort);
        assert_eq!(map_send_errno(socket::EAGAIN), Reason::ResourceUnavailable);
        assert_eq!(map_send_errno(socket::ENOBUFS), Reason::TransmitQueueFull);
        assert_eq!(map_send_errno(socket::EMSGSIZE), Reason::MessageTooLong);
        assert_eq!(
            map_send_errno(socket::EHOSTUNREACH),
            Reason::GeneralNetwork,
            "an unmapped errno is general"
        );
    }

    /// A plain send writes the bundle's packets, counts them with their
    /// overhead, and counts the bundle and its messages
    /// (packet_sender.cpp:61-110).
    #[test]
    fn a_plain_send_counts_what_went_out() {
        let (_dispatcher, _clock, _interface, mut sender) = sender_under_pinned_clock();
        sender.set_should_use_checksums(true);
        assert!(sender.should_use_checksums());
        assert_eq!(
            sender.loss().max_latency_million(),
            0,
            "no artificial latency by default"
        );

        let addr = Address::new(LOOPBACK, socket::htons(9));
        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units();
        assert!(packets >= 1);
        sender.send(&addr, &mut bundle, None);

        let stats = sender.stats();
        assert_eq!(stats.num_bundles_sent, 1);
        assert_eq!(stats.num_messages_sent, 4);
        assert_eq!(stats.num_reliable_messages_sent, 4);
        assert_eq!(stats.num_packets_sent, packets as u32);
        assert_eq!(stats.num_packets_sent_off_channel, packets as u32);
        assert!(
            stats.num_bytes_sent
                >= packets as u64 * (bundle.packet(0).total_size() as u64 + UDP_OVERHEAD as u64)
        );
        assert_eq!(stats.num_failed_packet_send, 0);
    }

    /// `dropNextSend` counts the packet as sent and puts nothing on the
    /// wire; a full loss ratio drops everything the same way
    /// (packet_sender.cpp:158-180).
    #[test]
    fn artificial_loss_drops_and_counts_the_packet() {
        let (_dispatcher, _clock, _interface, mut sender) = sender_under_pinned_clock();
        let addr = Address::new(LOOPBACK, socket::htons(9));

        sender.drop_next_send();
        let mut bundle = filled_bundle();
        sender.send(&addr, &mut bundle, None);
        assert_eq!(sender.stats().num_packets_sent, 1, "the drop counts as one");

        // A loss ratio of one drops every later packet too.
        sender.loss_mut().set_loss_ratio(1.0);
        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units();
        sender.send(&addr, &mut bundle, None);
        assert_eq!(
            sender.stats().num_packets_sent,
            1 + packets as u32,
            "every packet of the bundle hit the loss"
        );
        assert_eq!(sender.stats().num_packets_resent, 0);
        // The drop branch counts the prepared packet, footer included
        // (packet_sender.cpp:178-179).
        let size = bundle.packet(0).total_size();
        assert_eq!(
            sender.stats().num_bytes_sent,
            (1 + packets as u64) * (size as u64 + UDP_OVERHEAD as u64),
            "a dropped packet still carries its overhead into the byte totals"
        );
    }

    /// `dropNextSend` spends itself on the very next packet and leaves
    /// the ones after it alone (packet_sender.cpp:159-181).
    #[test]
    fn dropping_next_send_spends_itself_on_one_packet() {
        let (_dispatcher, _clock, _interface, mut sender) = sender_under_pinned_clock();
        let addr = Address::new(LOOPBACK, socket::htons(9));
        sender.drop_next_send();

        let mut first = filled_bundle();
        let first_packets = first.num_data_units();
        sender.send(&addr, &mut first, None);
        assert_eq!(
            sender.stats().num_packets_sent,
            first_packets as u32,
            "the drop spent itself on the whole first bundle"
        );

        // A zero ratio afterwards sends for real: the flag was spent,
        // not latched.
        let mut second = filled_bundle();
        let second_packets = second.num_data_units();
        sender.send(&addr, &mut second, None);
        assert_eq!(
            sender.stats().num_packets_sent,
            (first_packets + second_packets) as u32,
            "the second bundle went out normally"
        );
    }

    /// A minimum latency alone never delays: the gate reads the maximum
    /// first and returns "send now" while it is zero
    /// (packet_sender.cpp:195-200).
    #[test]
    fn a_zero_max_latency_never_schedules() {
        let (dispatcher, _clock, _interface, mut sender) = sender_under_pinned_clock();
        sender.loss_mut().set_min_latency(5.0);
        sender.loss_mut().set_max_latency(0.0);
        let (_wire, addr) = wire_end();

        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units() as u32;
        sender.send(&addr, &mut bundle, None);
        assert_eq!(sender.stats().num_packets_sent, packets);
        assert_eq!(
            dispatcher.process_once(false),
            0,
            "the minimum's magnitude never reached the rescheduling check"
        );
    }

    /// A latency window holds the packet until the once-off timer fires,
    /// then writes it (packet_sender.cpp:186-238, rescheduled_sender.cpp).
    #[test]
    fn a_latency_window_holds_the_packet_until_the_timer_fires() {
        let (dispatcher, clock, _interface, mut sender) = sender_under_pinned_clock();
        sender.loss_mut().set_min_latency(0.5);
        sender.loss_mut().set_max_latency(0.5);
        let (wire, addr) = wire_end();
        let mut buffer = [0u8; 2048];

        let mut bundle = filled_bundle();
        sender.send(&addr, &mut bundle, None);
        assert_eq!(
            sender.stats().num_packets_sent,
            0,
            "nothing went out before the timer"
        );
        assert!(
            wire.recv_from(&mut buffer).is_err(),
            "and the wire got nothing either"
        );

        // The timer hands the packet to the interface's own sender.
        clock.set(clock.get() + 600_000);
        assert_eq!(dispatcher.process_once(false), 0, "no inbound events");
        assert_eq!(
            dispatcher.num_timer_calls(),
            1,
            "the delay timer fired once"
        );
        for index in 0..bundle.num_data_units() {
            assert!(
                wire.recv_from(&mut buffer).is_ok(),
                "held packet {} went out when it did",
                index
            );
        }
        assert!(
            wire.recv_from(&mut buffer).is_err(),
            "no more than the held packets arrived"
        );
    }

    /// A resend is counted as a resend, not a fresh packet
    /// (packet_sender.cpp:114-130).
    #[test]
    fn a_resend_counts_resent_totals() {
        let (_dispatcher, _clock, _interface, mut sender) = sender_under_pinned_clock();
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let packet = Rc::new(Packet::new());
        sender.send_packet(&addr, &packet, None, true);
        assert_eq!(sender.stats().num_packets_resent, 1);
        assert_eq!(sender.stats().num_packets_sent, 1);
        assert_eq!(sender.stats().num_bytes_resent, packet.total_size() as u64);
    }

    /// A window that rolls below two milliseconds is "now" as far as the
    /// scheduler is concerned (packet_sender.cpp:300's `latency < 2`).
    #[test]
    fn a_sub_two_millisecond_window_sends_immediately() {
        let (dispatcher, _clock, _interface, mut sender) = sender_under_pinned_clock();
        sender.loss_mut().set_min_latency(0.0);
        sender.loss_mut().set_max_latency(0.001);
        let (wire, addr) = wire_end();
        let mut buffer = [0u8; 2048];

        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units() as u32;
        sender.send(&addr, &mut bundle, None);
        assert_eq!(sender.stats().num_packets_sent, packets);
        assert_eq!(
            dispatcher.process_once(false),
            0,
            "nothing was held for a rescheduling timer"
        );
        for _ in 0..packets {
            assert!(
                wire.recv_from(&mut buffer).is_ok(),
                "each packet went out at once, no timer involved"
            );
        }
    }

    /// With the interface gone a send reports a network failure and
    /// counts nothing (`basicSendSingleTry`, packet_sender.cpp:347).
    #[test]
    fn a_sender_without_its_interface_counts_nothing() {
        let (dispatcher, _clock, interface, mut sender) = sender_under_pinned_clock();
        let addr = Address::new(LOOPBACK, socket::htons(9));
        drop(interface);

        sender.send_packet(&addr, &Rc::new(Packet::new()), None, false);
        assert_eq!(sender.stats().num_packets_sent, 0);
        assert_eq!(dispatcher.process_once(false), 0, "nothing was scheduled");
    }

    /// A failed sendto counts the failure instead of the packet
    /// (`basicSendSingleTry`, packet_sender.cpp:347-390).
    #[test]
    fn a_failed_send_counts_the_failure() {
        let (_dispatcher, _clock, interface, mut sender) = sender_under_pinned_clock();
        interface.close_socket();
        let addr = Address::new(LOOPBACK, socket::htons(9));

        let packet = Rc::new(Packet::new());
        sender.send_packet(&addr, &packet, None, false);
        let stats = sender.stats();
        assert_eq!(stats.num_packets_sent, 0);
        assert_eq!(
            stats.num_failed_packet_send, 1,
            "the sendto failure is counted"
        );
    }

    /// An unfinalised bundle picks up reply orders from the interface's
    /// request manager while being finalised
    /// (packet_sender.cpp:61-75).
    #[test]
    fn an_unfinalised_bundle_collects_reply_orders_from_the_manager() {
        let (dispatcher, _clock, interface, mut sender) = sender_under_pinned_clock();
        interface.set_request_manager(Some(Rc::new(crate::request_manager::RequestManager::new(
            false, dispatcher,
        ))));
        let (_wire, addr) = wire_end();

        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units() as u32;
        sender.send(&addr, &mut bundle, None);
        assert_eq!(sender.stats().num_bundles_sent, 1);
        assert_eq!(sender.stats().num_packets_sent, packets);
    }

    /// A channel's filter gets each packet before the wire
    /// (`sendRescheduledPacket`, packet_sender.cpp:241-258).
    #[test]
    fn a_filtered_channel_hands_its_filter_the_packet() {
        let (_dispatcher, _clock, interface, mut sender) = sender_under_pinned_clock();
        let (wire, addr) = wire_end();
        let mut buffer = [0u8; 2048];
        let channel = interface
            .find_channel(addr, true)
            .expect("anonymous channel");
        let counter = Rc::new(SoloCell::new(0u32));
        channel
            .borrow_mut()
            .set_p_filter(Some(Rc::new(CountingFilter {
                sends: Rc::clone(&counter),
            })));

        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units() as u32;
        sender.send(&addr, &mut bundle, Some(&channel));
        assert_eq!(counter.get(), packets, "the filter saw every packet");
        assert_eq!(sender.stats().num_packets_sent, packets);
        assert_eq!(
            sender.stats().num_packets_sent_off_channel,
            0,
            "on-channel packets are not counted off-channel"
        );
        for _ in 0..packets {
            assert!(
                wire.recv_from(&mut buffer).is_ok(),
                "the filter forwarded each packet to the wire"
            );
        }
    }

    /// A held packet on a filtered channel meets the filter only when
    /// the delay timer fires -- not before
    /// (rescheduled_sender.cpp, packet_sender.cpp:241-258).
    #[test]
    fn a_held_packet_meets_the_channel_filter_at_fire_time() {
        let (dispatcher, clock, interface, mut sender) = sender_under_pinned_clock();
        sender.loss_mut().set_min_latency(0.004);
        sender.loss_mut().set_max_latency(0.004);
        let (wire, addr) = wire_end();
        let mut buffer = [0u8; 2048];
        let channel = interface
            .find_channel(addr, true)
            .expect("anonymous channel");
        let counter = Rc::new(SoloCell::new(0u32));
        channel
            .borrow_mut()
            .set_p_filter(Some(Rc::new(CountingFilter {
                sends: Rc::clone(&counter),
            })));

        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units() as u32;
        sender.send(&addr, &mut bundle, Some(&channel));
        assert_eq!(
            counter.get(),
            0,
            "the filter saw nothing while the packets were held"
        );
        assert!(
            wire.recv_from(&mut buffer).is_err(),
            "and the wire got nothing either"
        );

        clock.set(clock.get() + 600_000);
        assert_eq!(dispatcher.process_once(false), 0);
        assert_eq!(dispatcher.num_timer_calls(), 1, "the delay timer fired");
        assert_eq!(
            counter.get(),
            packets,
            "each held packet met the filter when it fired"
        );
        for _ in 0..packets {
            assert!(
                wire.recv_from(&mut buffer).is_ok(),
                "and reached the wire through it"
            );
        }
    }

    /// The counting filter's recv forwards: the fixture's contract on
    /// the trait's required arm (`PacketFilter::recv`).
    #[test]
    fn the_counting_filters_recv_forwards() {
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let filter = CountingFilter {
            sends: Rc::new(SoloCell::new(0u32)),
        };
        let mut packet = Packet::new();
        let reason = filter.recv(&addr, &mut packet, &|_addr, _packet| Reason::Success);
        assert_eq!(reason, Reason::Success);
        assert_eq!(filter.sends.get(), 0, "recv does not count as a send");
    }
}
