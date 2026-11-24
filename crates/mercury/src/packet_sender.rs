//! `lib/network/packet_sender.{hpp,cpp}`: the interface's socket write
//! path -- turn a finalised bundle into datagrams, count what went out,
//! and (when asked) drop or delay packets to fake a lossy link.
//!
//! The reference's `PacketSender` holds the socket, the once-off sender,
//! and the `SendingStats` watcher block. The port hands it a weak handle
//! to its interface instead of the socket: the socket lives on the
//! interface, and the weak handle is what a delayed send needs to reach
//! it when the rescheduling timer fires.
//!
//! The same artificial-loss decision also runs on the interface's own
//! inline send path, so both routes share
//! [`reschedule_decision`](crate::packet_sender::reschedule_decision) and
//! the [`RescheduledSender`] once-off timer; the reference reaches one
//! sender through the interface, the port keeps the two faces honest
//! against each other.

use std::cell::{Cell, Ref, RefCell, RefMut};
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
/// the watchers expose. The interface owns one for its inline send path;
/// a standalone [`PacketSender`] counts its own.
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
/// [`reschedule_decision`](Self::drop_hit_rand)'s caller.
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

    /// `PacketLossParameters( lossRatioValue, minLatencyValue,
    /// maxLatencyValue )` (packet_loss_parameters.hpp:32).
    pub fn with(loss_ratio: f32, min_latency: f32, max_latency: f32) -> Self {
        let mut loss = Self::new();
        loss.set(loss_ratio, min_latency, max_latency);
        loss
    }

    /// `set( lossRatio, minLatency, maxLatency )`.
    pub fn set(&mut self, loss_ratio: f32, min_latency: f32, max_latency: f32) {
        self.set_loss_ratio(loss_ratio);
        self.set_min_latency(min_latency);
        self.set_max_latency(max_latency);
    }

    /// `apply( networkInterface )` (packet_loss_parameters.cpp:33): push
    /// the settings onto an interface's own knobs.
    pub fn apply(&self, interface: &NetworkInterface) {
        interface.set_loss_ratio(self.loss_ratio);
        interface.set_latency(self.min_latency, self.max_latency);
    }

    /// `hasArtificialLossOrLatency()`: any of the three integer knobs is
    /// non-zero (packet_loss_parameters.hpp:49).
    pub fn has_artificial_loss_or_latency(&self) -> bool {
        self.drop_per_million != 0 || self.min_latency_million != 0 || self.max_latency_million != 0
    }

    /// `lossRatio()`.
    pub fn loss_ratio(&self) -> f32 {
        self.loss_ratio
    }

    /// `lossRatio( value )`: non-zero loss is its own warning.
    pub fn set_loss_ratio(&mut self, value: f32) {
        self.loss_ratio = value;
        self.drop_per_million = (value * 1_000_000.0).max(0.0) as u32;
        if value != 0.0 {
            eprintln!(
                "PacketLossParameters::lossRatio: Loss ratio set to {:.2}",
                value
            );
        }
    }

    /// `dropPerMillion( value )` (packet_loss_parameters.cpp:74): goes
    /// through the ratio setter, like the reference.
    pub fn set_drop_per_million(&mut self, value: u32) {
        self.set_loss_ratio(value as f32 / 1_000_000.0);
    }

    /// `minLatency()`, seconds.
    pub fn min_latency(&self) -> f32 {
        self.min_latency
    }

    /// `minLatency( value )`: seconds.
    pub fn set_min_latency(&mut self, value: f32) {
        self.min_latency = value;
        self.min_latency_million = (value * 1000.0).max(0.0) as i64;
    }

    /// `minLatencyMillion( value )` (packet_loss_parameters.cpp:106):
    /// milliseconds in, seconds stored.
    pub fn set_min_latency_million(&mut self, value: i64) {
        self.set_min_latency(value as f32 / 1000.0);
    }

    /// `maxLatency()`, seconds.
    pub fn max_latency(&self) -> f32 {
        self.max_latency
    }

    /// `maxLatency( value )`: seconds.
    pub fn set_max_latency(&mut self, value: f32) {
        self.max_latency = value;
        self.max_latency_million = (value * 1000.0).max(0.0) as i64;
    }

    /// `maxLatencyMillion( value )` (packet_loss_parameters.cpp:143).
    pub fn set_max_latency_million(&mut self, value: i64) {
        self.set_max_latency(value as f32 / 1000.0);
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

    /// The `rescheduleSend` drop clause (packet_sender.cpp:166) against
    /// one draw of `rand`. Consumes the stream exactly as the reference's
    /// short-circuit does -- nothing is drawn while the ratio is zero.
    /// `forced` is the caller's `isDroppingNextSend_` one-shot, which the
    /// caller clears.
    pub(crate) fn drop_hit_rand(&self, rand: &RandStream, forced: bool) -> bool {
        forced || {
            let per_million = self.drop_per_million;
            per_million != 0 && rand.next() % 1_000_000 < per_million
        }
    }

    /// The `rescheduleSend` latency clause (packet_sender.cpp:206): the
    /// minimum, plus a draw across the window when it has width. The
    /// caller applies the reference's sub-2ms "send now" gate and
    /// schedules the [`RescheduledSender`].
    pub(crate) fn latency_rand(&self, rand: &RandStream) -> i64 {
        let mut latency = self.min_latency_million;
        if self.max_latency_million > self.min_latency_million {
            latency += (rand.next() as i64) % (self.max_latency_million - self.min_latency_million);
        }
        latency
    }
}

/// The `rand()` stream the reference seeds once at 1 (the C default): a
/// small xorshift, deterministic for tests. Each user keeps its own.
pub(crate) struct RandStream(Cell<u32>);

impl RandStream {
    /// Seeded 1, like the C runtime's `rand()` before a seed.
    pub(crate) fn new() -> Self {
        Self(Cell::new(1))
    }

    /// The next draw.
    pub(crate) fn next(&self) -> u32 {
        let mut x = self.0.get();
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0.set(x);
        x
    }

    /// The raw state, so tests can tell "drew" from "didn't".
    #[cfg(test)]
    pub(crate) fn state(&self) -> u32 {
        self.0.get()
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

/// What [`reschedule_decision`] told the caller to do with a packet.
pub(crate) enum Reschedule {
    /// Artificial loss: the caller counts it as sent and puts nothing on
    /// the wire.
    Dropped,
    /// Artificial latency: hold for this many milliseconds, then send.
    Delayed { latency_millis: i64 },
    /// No knob applies, or the drawn delay is under the reference's 2ms
    /// gate: send now.
    Now,
}

/// `PacketSender::rescheduleSend`'s decision core (packet_sender.cpp:158),
/// shared by the interface's own send path and the standalone
/// [`PacketSender`]: drop it under artificial loss, delay it under
/// artificial latency, or let it go now.
pub(crate) fn reschedule_decision(
    loss: &PacketLossParameters,
    rand: &RandStream,
    forced: bool,
) -> Reschedule {
    if loss.drop_hit_rand(rand, forced) {
        return Reschedule::Dropped;
    }

    if loss.max_latency_million() == 0 {
        return Reschedule::Now;
    }

    let latency = loss.latency_rand(rand);
    if latency < 2 {
        return Reschedule::Now; // 2ms => send now
    }

    Reschedule::Delayed {
        latency_millis: latency,
    }
}

/// Arm the once-off timer that sends a held packet
/// (`RescheduledSender`'s constructor, rescheduled_sender.cpp:15): the
/// delay is milliseconds of knobs converted to the timer's microseconds.
pub(crate) fn schedule_rescheduled_packet(
    interface: &Rc<NetworkInterface>,
    addr: Address,
    packet: Rc<Packet>,
    channel: Option<ChannelPtr>,
    latency_millis: i64,
) {
    let sender = RescheduledSender::new(interface.clone(), addr, packet, channel);
    interface.dispatcher().add_once_off_timer(
        latency_millis * 1000,
        Rc::new(sender),
        0,
        "RescheduledSender",
    );
}

/// `Mercury::PacketSender` (`packet_sender.cpp`): the reference's socket
/// write face, kept as its own object. The interface's inline send path
/// carries the same decision; this object exists so a fixture can drive
/// the reference's exact class against a live interface.
pub struct PacketSender {
    /// The interface this sender writes through, weak so the send timer
    /// does not keep it alive.
    interface: Weak<NetworkInterface>,
    /// `seqNumAllocator_`, for off-channel sequence numbers.
    seq_num_allocator: RefCell<SeqNumAllocator>,
    /// `sendingStats_`.
    stats: RefCell<SendingStats>,
    /// `packetLossParameters_`.
    loss: RefCell<PacketLossParameters>,
    /// `isDroppingNextSend_`.
    is_dropping_next_send: Cell<bool>,
    /// `shouldUseChecksums_`.
    should_use_checksums: Cell<bool>,
    /// The sender's own draw of the reference's `rand()` stream.
    rand: RandStream,
}

impl PacketSender {
    /// `PacketSender( ... )`: the sender starts with the allocator at 1
    /// and no artificial loss. The weak interface is filled in once the
    /// interface exists.
    pub fn new(interface: Weak<NetworkInterface>) -> Self {
        Self {
            interface,
            seq_num_allocator: RefCell::new(SeqNumAllocator::new(1)),
            stats: RefCell::new(SendingStats::default()),
            loss: RefCell::new(PacketLossParameters::new()),
            is_dropping_next_send: Cell::new(false),
            should_use_checksums: Cell::new(false),
            rand: RandStream::new(),
        }
    }

    /// `sendingStats_`.
    pub fn stats(&self) -> Ref<'_, SendingStats> {
        self.stats.borrow()
    }

    /// `packetLossParameters_`.
    pub fn loss(&self) -> Ref<'_, PacketLossParameters> {
        self.loss.borrow()
    }

    /// The mutable loss knobs, so `NetworkInterface::setLossRatio` etc.
    /// reach them.
    pub fn loss_mut(&self) -> RefMut<'_, PacketLossParameters> {
        self.loss.borrow_mut()
    }

    /// `shouldUseChecksums_`.
    pub fn should_use_checksums(&self) -> bool {
        self.should_use_checksums.get()
    }

    /// `shouldUseChecksums( value )`.
    pub fn set_should_use_checksums(&self, value: bool) {
        self.should_use_checksums.set(value);
    }

    /// `dropNextSend()`: the next packet is counted and dropped, exactly
    /// once.
    pub fn drop_next_send(&self) {
        self.is_dropping_next_send.set(true);
    }

    /// `PacketSender::send( address, bundle, pChannel )`
    /// (packet_sender.cpp:61): finalise an unfinalised bundle, then send
    /// every packet of it up to the first overflow packet. Off-channel
    /// bundles are closed here; a channel's own bundle was closed by
    /// [`Channel::send`](crate::udp_channel::UdpChannel::send_bundle)
    /// already.
    pub fn send(&self, address: &Address, bundle: &mut UdpBundle, channel: Option<&ChannelPtr>) {
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
        let first_overflow = bundle.prepare_packets(
            &mut self.seq_num_allocator.borrow_mut(),
            self.should_use_checksums.get(),
            channel,
        );

        // Do not send overflow packets.
        for index in 0..num_packets {
            if Some(index) == first_overflow {
                break;
            }
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
    /// rescheduling check.
    pub fn send_packet(
        &self,
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
    fn reschedule_send(
        &self,
        addr: &Address,
        packet: &Rc<Packet>,
        channel: Option<&ChannelPtr>,
    ) -> bool {
        let loss = self.loss.borrow();
        let forced = self.is_dropping_next_send.replace(false);
        let decision = reschedule_decision(&loss, &self.rand, forced);
        drop(loss);

        match decision {
            Reschedule::Dropped => {
                // The reference counts a dropped packet as sent.
                let mut stats = self.stats.borrow_mut();
                stats.num_bytes_sent += packet.total_size() as u64 + UDP_OVERHEAD as u64;
                stats.num_packets_sent += 1;
                true
            }
            Reschedule::Delayed { latency_millis } => {
                if let Some(interface) = self.interface.upgrade() {
                    schedule_rescheduled_packet(
                        &interface,
                        *addr,
                        Rc::clone(packet),
                        channel.cloned(),
                        latency_millis,
                    );
                }
                true
            }
            Reschedule::Now => false,
        }
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
                let mut owned = packet.as_ref().clone();
                filter.send(addr, &mut owned, &|addr, packet| {
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
}

/// `Mercury::RescheduledSender` (rescheduled_sender.cpp:15): a packet
/// held for its artificial latency, sent when the once-off timer fires.
/// Shared by the interface's send path and the standalone [`PacketSender`].
pub(crate) struct RescheduledSender {
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
    pub(crate) fn new(
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
    use crate::packet_receiver::PacketReceiver;
    use std::cell::Cell as SoloCell;

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

    /// The knobs translate seconds and ratios to their integer units, the
    /// constructors agree, and the per-million setters round-trip through
    /// the same float knobs (`packet_loss_parameters.cpp`).
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
        assert!(
            !loss.has_artificial_loss_or_latency(),
            "the zeros disable everything"
        );

        loss.set(0.5, 0.25, 0.75);
        assert_eq!(loss.drop_per_million(), 500_000);
        assert_eq!(loss.min_latency_million(), 250);
        assert_eq!(loss.max_latency_million(), 750);
        assert_eq!(loss.loss_ratio(), 0.5);
        assert_eq!(loss.min_latency(), 0.25);
        assert_eq!(loss.max_latency(), 0.75);
        assert!(loss.has_artificial_loss_or_latency());

        // The reference's per-million setters route through the float
        // setters (packet_loss_parameters.cpp:74-149).
        let mut loss = PacketLossParameters::new();
        loss.set_drop_per_million(250_000);
        assert_eq!(loss.loss_ratio(), 0.25);
        assert_eq!(loss.drop_per_million(), 250_000);
        loss.set_min_latency_million(30);
        assert!((loss.min_latency() - 0.03).abs() < 1e-6);
        assert_eq!(loss.min_latency_million(), 30);
        loss.set_max_latency_million(900);
        assert!((loss.max_latency() - 0.9).abs() < 1e-6);
        assert_eq!(loss.max_latency_million(), 900);

        // The value constructor sets the same three knobs.
        let mut loss = PacketLossParameters::with(1.0, 0.01, 0.02);
        assert_eq!(loss.drop_per_million(), 1_000_000);
        assert_eq!(loss.min_latency_million(), 10);
        assert_eq!(loss.max_latency_million(), 20);

        // A negative ratio clamps to zero rather than wrapping.
        loss.set_loss_ratio(-1.0);
        assert_eq!(loss.drop_per_million(), 0);
        let mut loss = PacketLossParameters::with(1.0, 0.01, 0.02);
        loss.set_min_latency(-1.0);
        assert_eq!(loss.min_latency_million(), 0);
        loss.set_max_latency(-1.0);
        assert_eq!(loss.max_latency_million(), 0);
        // The clamped latencies leave only the ratio live; clearing it
        // returns to the disabled state.
        assert!(loss.has_artificial_loss_or_latency());
        loss.set_loss_ratio(0.0);
        assert!(!loss.has_artificial_loss_or_latency());
    }

    /// `apply` pushes the settings onto an interface's own knobs
    /// (packet_loss_parameters.cpp:33).
    #[test]
    fn apply_pushes_the_settings_onto_the_interface() {
        let (_dispatcher, _clock, interface, _sender) = sender_under_pinned_clock();
        assert!(!interface.has_artificial_loss_or_latency());

        let loss = PacketLossParameters::with(0.25, 0.03, 0.9);
        loss.apply(&interface);

        assert!(interface.has_artificial_loss_or_latency());
        let applied = interface.packet_loss_parameters();
        assert_eq!(applied.drop_per_million(), 250_000);
        assert_eq!(applied.min_latency_million(), 30);
        assert_eq!(applied.max_latency_million(), 900);
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
        let (_dispatcher, _clock, _interface, sender) = sender_under_pinned_clock();
        sender.set_should_use_checksums(true);
        assert!(sender.should_use_checksums());

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
        let (_dispatcher, _clock, _interface, sender) = sender_under_pinned_clock();
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
    }

    /// A latency window holds the packet until the once-off timer fires,
    /// then writes it (packet_sender.cpp:186-238, rescheduled_sender.cpp).
    #[test]
    fn a_latency_window_holds_the_packet_until_the_timer_fires() {
        let (dispatcher, clock, interface, sender) = sender_under_pinned_clock();
        sender.loss_mut().set_min_latency(0.5);
        sender.loss_mut().set_max_latency(0.5);

        let addr = Address::new(LOOPBACK, socket::htons(9));
        let mut bundle = filled_bundle();
        sender.send(&addr, &mut bundle, None);
        assert_eq!(
            sender.stats().num_packets_sent,
            0,
            "nothing went out before the timer"
        );
        assert_eq!(
            interface.sending_stats().num_packets_sent,
            0,
            "and the wire got nothing either"
        );

        // The timer hands the packet to the interface's own sender.
        clock.set(clock.get() + 600_000);
        assert_eq!(dispatcher.process_once(false), 1, "the delay timer fired");
        assert_eq!(
            interface.sending_stats().num_packets_sent,
            bundle.num_data_units() as u32,
            "the held packets went out when it did"
        );
    }

    /// A resend is counted as a resend, not a fresh packet
    /// (packet_sender.cpp:114-130).
    #[test]
    fn a_resend_counts_resent_totals() {
        let (_dispatcher, _clock, _interface, sender) = sender_under_pinned_clock();
        let addr = Address::new(LOOPBACK, socket::htons(9));
        let packet = Rc::new(Packet::new());
        sender.send_packet(&addr, &packet, None, true);
        assert_eq!(sender.stats().num_packets_resent, 1);
        assert_eq!(sender.stats().num_packets_sent, 1);
        assert_eq!(sender.stats().num_bytes_resent, packet.total_size() as u64);
    }

    /// A window that rolls below two milliseconds is "now" as far as the
    /// scheduler is concerned (packet_sender.cpp:222).
    #[test]
    fn a_sub_two_millisecond_window_sends_immediately() {
        let (_dispatcher, _clock, interface, sender) = sender_under_pinned_clock();
        sender.loss_mut().set_min_latency(0.0);
        sender.loss_mut().set_max_latency(0.001);

        let addr = Address::new(LOOPBACK, socket::htons(9));
        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units() as u32;
        sender.send(&addr, &mut bundle, None);
        assert_eq!(sender.stats().num_packets_sent, packets);
        assert_eq!(
            interface.sending_stats().num_packets_sent,
            0,
            "nothing was held for the interface's own sender"
        );
    }

    /// With the interface gone a send reports a network failure and
    /// counts nothing (`basicSendSingleTry`, packet_sender.cpp:347).
    #[test]
    fn a_sender_without_its_interface_counts_nothing() {
        let (dispatcher, _clock, interface, sender) = sender_under_pinned_clock();
        let addr = Address::new(LOOPBACK, socket::htons(9));
        drop(interface);

        sender.send_packet(&addr, &Rc::new(Packet::new()), None, false);
        assert_eq!(sender.stats().num_packets_sent, 0);
        assert_eq!(dispatcher.process_once(false), 0, "nothing was scheduled");
    }

    /// The reference seeds `rand()` at 1 and draws only where its
    /// short-circuits allow: the drop draw never runs while the ratio is
    /// zero, and the latency draw never runs while the window is closed.
    #[test]
    fn the_decision_draws_exactly_where_the_reference_does() {
        let rand = RandStream::new();

        // Zero ratio: no draw, no drop, forced one-shot still drops.
        let loss = PacketLossParameters::new();
        assert!(!loss.drop_hit_rand(&rand, false));
        let first = rand_state(&rand);
        assert!(!loss.drop_hit_rand(&rand, false));
        assert_eq!(rand_state(&rand), first, "an unused knob draws nothing");
        assert!(loss.drop_hit_rand(&rand, true), "the one-shot is its own");

        // Ratio one drops every draw.
        let loss = PacketLossParameters::with(1.0, 0.0, 0.0);
        assert!(loss.drop_hit_rand(&rand, false));
        assert!(loss.drop_hit_rand(&rand, false));

        // A closed window keeps the minimum without drawing.
        let loss = PacketLossParameters::with(0.0, 0.4, 0.4);
        let first = rand_state(&rand);
        assert_eq!(loss.latency_rand(&rand), 400);
        assert_eq!(rand_state(&rand), first, "no draw across a closed window");

        // An open window draws inside it.
        let loss = PacketLossParameters::with(0.0, 0.1, 0.3);
        for _ in 0..16 {
            let latency = loss.latency_rand(&rand);
            assert!((100..300).contains(&latency), "{latency} out of window");
        }

        // Max at or below min means the minimum, never a wrap.
        let loss = PacketLossParameters::with(0.0, 0.3, 0.1);
        assert_eq!(loss.latency_rand(&RandStream::new()), 300);
    }

    /// The interface's inline send path and the standalone sender agree
    /// under the same knobs: same stream, same drops.
    #[test]
    fn both_send_faces_reach_the_same_decision() {
        // A ratio of one drops on both faces without touching the wire.
        let (_dispatcher, _clock, interface, sender) = sender_under_pinned_clock();
        interface.set_loss_ratio(1.0);
        let addr = Address::new(LOOPBACK, socket::htons(9));

        let mut bundle = filled_bundle();
        let packets = bundle.num_data_units() as u32;
        sender.send(&addr, &mut bundle, None);
        assert_eq!(
            sender.stats().num_packets_sent,
            packets,
            "the standalone sender dropped everything"
        );
        assert_eq!(
            interface.sending_stats().num_packets_sent,
            0,
            "and nothing reached the interface's own sender"
        );

        // The interface's own face drops the same way.
        let mut bundle = filled_bundle();
        interface.send(&addr, &mut bundle, None);
        assert_eq!(
            interface.sending_stats().num_packets_sent,
            packets,
            "the inline face counted its drops as sends"
        );
        assert!(interface.has_artificial_loss_or_latency());
    }

    /// The current state of a stream, for the no-draw assertions.
    fn rand_state(rand: &RandStream) -> u32 {
        rand.state()
    }
}
