//! `lib/network/packet_sender.hpp` / `packet_sender.cpp`: the physical send
//! half of the data plane. [`PacketSender`] turns a finalised [`UDPBundle`]
//! into wire datagrams (`send`), and pushes a single packet again on a
//! resend (`send_packet`), counting what went out through the [`SendingStats`]
//! it shares.
//!
//! Still the seam: the once-off sender's resend timer for off-channel
//! reliable packets, the channel packet filter and packet monitor, and the
//! `PacketLossParameters` drop-per-million / latency rescheduling (the port
//! keeps the deterministic `dropNextSend` lever; the statistical and delayed
//! arms belong to a later latency/loss item).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use platform::socket;

use crate::endpoint::{Address, Endpoint, NONE};
use crate::misc::{Reason, SeqNumAllocator};
use crate::packet::Packet;
use crate::udp_bundle::UDPBundle;
use crate::udp_channel::UDPChannel;

/// `UDP_OVERHEAD` (basictypes.hpp:34): what an IP/UDP header adds to the
/// payload when counting bytes on the wire.
const UDP_OVERHEAD: usize = 28;

/// `SendingStats` (sending_stats.hpp): the counters the sender updates. The
/// full statistics container with its timer lanes belongs to the interface
/// statistics item (BW-02-01-04); this is the slice `PacketSender` touches.
#[derive(Default)]
pub struct SendingStats {
    /// `numPacketsSent_`.
    pub num_packets_sent: Cell<u64>,
    /// `numBytesSent_`.
    pub num_bytes_sent: Cell<u64>,
    /// `numPacketsResent_`.
    pub num_packets_resent: Cell<u64>,
    /// `numBytesResent_`.
    pub num_bytes_resent: Cell<u64>,
    /// `numPacketsSentOffChannel_`.
    pub num_packets_sent_off_channel: Cell<u64>,
    /// `numBundlesSent_`.
    pub num_bundles_sent: Cell<u64>,
    /// `numFailedPacketSend_`.
    pub num_failed_packet_send: Cell<u64>,
}

/// `PacketSender` (packet_sender.cpp): owns the sequence-number allocator for
/// off-channel packets and drives the socket.
pub struct PacketSender {
    /// `sendingStats_`.
    sending_stats: Rc<SendingStats>,
    /// `seqNumAllocator_`.
    seq_num_allocator: RefCell<SeqNumAllocator>,
    /// `isDroppingNextSend_`.
    is_dropping_next_send: Cell<bool>,
    /// `shouldUseChecksums_`.
    should_use_checksums: Cell<bool>,
}

impl PacketSender {
    /// `PacketSender::PacketSender` minus the dispatcher, request manager and
    /// once-off sender it does not need at this scope.
    pub fn new() -> Self {
        Self {
            sending_stats: Rc::new(SendingStats::default()),
            seq_num_allocator: RefCell::new(SeqNumAllocator::new(1)),
            is_dropping_next_send: Cell::new(false),
            should_use_checksums: Cell::new(false),
        }
    }

    /// `sendingStats()`.
    pub fn sending_stats(&self) -> Rc<SendingStats> {
        Rc::clone(&self.sending_stats)
    }

    /// `isDroppingNextSend()`.
    pub fn is_dropping_next_send(&self) -> bool {
        self.is_dropping_next_send.get()
    }

    /// `dropNextSend()`: arrange for the next sent packet to be dropped.
    pub fn drop_next_send(&self) {
        self.is_dropping_next_send.set(true);
    }

    /// `shouldUseChecksums( b )`.
    pub fn set_should_use_checksums(&self, b: bool) {
        self.should_use_checksums.set(b);
    }

    /// `send( address, bundle, channel )` (packet_sender.cpp:78): finalise a
    /// not-yet-finalised bundle, stamp its packets' footers and sequence
    /// numbers, and push every packet that did not overflow the send window.
    ///
    /// The reference registers an unfinalised bundle's reply orders with the
    /// request manager here (`addReplyOrdersTo`); the port leaves that to the
    /// reply-correlation seam, so callers that carry reply orders must
    /// finalise (and register) before handing the bundle over.
    pub fn send(
        &self,
        socket: &Endpoint,
        address: &Address,
        bundle: &mut UDPBundle,
        channel: Option<&UDPChannel>,
    ) {
        assert!(address != &NONE);
        if let Some(channel) = channel {
            assert!(channel.addr() == *address);
        }

        if !bundle.is_finalised() {
            bundle.finalise();
        }

        // Fill in all the footers that are left to us, and do not send the
        // packets that overflowed the send window.
        let first_overflow = bundle.prepare_packets(
            channel,
            &mut self.seq_num_allocator.borrow_mut(),
            self.should_use_checksums.get(),
        );

        let mut node = bundle.first_packet();
        while let Some(packet) = node {
            if let Some(overflow) = first_overflow.as_ref()
                && Rc::ptr_eq(&packet, overflow)
            {
                break;
            }

            self.send_packet(socket, address, &packet, false);

            node = packet.borrow().next();
        }

        self.sending_stats
            .num_bundles_sent
            .set(self.sending_stats.num_bundles_sent.get() + 1);
    }

    /// `sendPacket( address, packet, channel, isResend )`
    /// (packet_sender.cpp:114): push a single packet, counting a resend
    /// against the resend stats and an off-channel packet against the
    /// off-channel counter, then through the artificial-loss lever. The
    /// reference's channel slot only feeds the packet filter and the
    /// once-off sender there, both seams, so the port takes none.
    pub fn send_packet(
        &self,
        socket: &Endpoint,
        address: &Address,
        packet: &Rc<RefCell<Packet>>,
        is_resend: bool,
    ) {
        if is_resend {
            self.sending_stats.num_bytes_resent.set(
                self.sending_stats.num_bytes_resent.get() + packet.borrow().total_size() as u64,
            );
            self.sending_stats
                .num_packets_resent
                .set(self.sending_stats.num_packets_resent.get() + 1);
        } else {
            // The once-off sender's resend timer for off-channel reliable
            // packets is a seam; there is no once-off sender yet.
            if !packet.borrow().has_flags(crate::packet::flags::ON_CHANNEL) {
                self.sending_stats
                    .num_packets_sent_off_channel
                    .set(self.sending_stats.num_packets_sent_off_channel.get() + 1);
            }
        }

        // Check if we want artificial loss (the latency arm of
        // rescheduleSend is a seam).
        if self.reschedule_send(packet) {
            return;
        }

        self.basic_send_with_retries(socket, address, packet);
    }

    /// `rescheduleSend` (packet_sender.cpp:150): the deterministic
    /// artificial-loss lever. The packet-loss-parameters drop-per-million and
    /// latency arms are a seam, so this only honours `dropNextSend`.
    fn reschedule_send(&self, packet: &Rc<RefCell<Packet>>) -> bool {
        if !self.is_dropping_next_send.get() {
            return false;
        }

        if self.is_verbose() {
            eprintln!(
                "PacketSender::rescheduleSend: dropped packet #{} due to artificial loss",
                packet.borrow().seq()
            );
        }

        self.is_dropping_next_send.set(false);
        true
    }

    /// `basicSendWithRetries` (packet_sender.cpp:246): try a few times,
    /// retrying a refused port and sleeping briefly on a full transmit
    /// queue.
    fn basic_send_with_retries(
        &self,
        socket: &Endpoint,
        addr: &Address,
        packet: &Rc<RefCell<Packet>>,
    ) -> Reason {
        let mut reason = Reason::Success;
        for retries in 1..=4 {
            reason = self.basic_send_single_try(socket, addr, packet);

            if reason == Reason::Success {
                return reason;
            }

            if reason == Reason::NoSuchPort {
                continue;
            }

            if reason == Reason::ResourceUnavailable || reason == Reason::TransmitQueueFull {
                if self.is_verbose() {
                    eprintln!(
                        "PacketSender::basicSendWithRetries: Transmit queue full, waiting for space... ({})",
                        retries
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }

            break;
        }

        reason
    }

    /// `basicSendSingleTry` (packet_sender.cpp:313): one `sendto`, mapping
    /// the errno to the reason the reference does.
    fn basic_send_single_try(
        &self,
        socket: &Endpoint,
        addr: &Address,
        packet: &Rc<RefCell<Packet>>,
    ) -> Reason {
        let len = socket.sendto(packet.borrow().datagram(), addr.port, addr.ip);

        if len == packet.borrow().total_size() as i32 {
            self.sending_stats
                .num_bytes_sent
                .set(self.sending_stats.num_bytes_sent.get() + len as u64 + UDP_OVERHEAD as u64);
            self.sending_stats
                .num_packets_sent
                .set(self.sending_stats.num_packets_sent.get() + 1);

            return Reason::Success;
        }

        self.sending_stats
            .num_failed_packet_send
            .set(self.sending_stats.num_failed_packet_send.get() + 1);

        let err = last_errno();
        let reason = match err {
            e if e == socket::ECONNREFUSED => Reason::NoSuchPort,
            e if e == socket::EAGAIN => Reason::ResourceUnavailable,
            e if e == socket::ENOBUFS => Reason::TransmitQueueFull,
            e if e == socket::EMSGSIZE => Reason::MessageTooLong,
            _ => Reason::GeneralNetwork,
        };

        if self.is_verbose() {
            eprintln!(
                "PacketSender::basicSendSingleTry: Packet to {}: Could not send: {}",
                addr.c_str(),
                std::io::Error::from_raw_os_error(err)
            );
        }

        reason
    }

    fn is_verbose(&self) -> bool {
        true
    }
}

impl Default for PacketSender {
    fn default() -> Self {
        Self::new()
    }
}

/// `errno` of the failed `sendto`.
fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}
