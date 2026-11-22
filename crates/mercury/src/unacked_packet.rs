//! `lib/network/unacked_packet.{hpp,cpp}` -- one sent packet that may need
//! resending (BW-02-03-03).
//!
//! "This class stores sent packets that may need to be resent. These things
//! need to be fast so we pool them and use custom allocators." (hpp:12-15).
//! The reference's pooling and custom allocator have no Rust analogue: the
//! ring that holds these is a `Vec`, which is the port's allocator.

use std::rc::Rc;

use crate::misc::SeqNum;
use crate::packet::Packet;
use crate::reliable_order::ReliableOrder;

/// `UDPChannel::UnackedPacket` (unacked_packet.hpp:16): a packet on the way
/// out, with the bookkeeping the ack and resend paths draw on.
pub struct UnackedPacket {
    /// `pPacket_`.
    packet: Rc<Packet>,
    /// `lastSentAtOutSeq_`: the outgoing sequence number on the channel the
    /// last time this packet was sent.
    last_sent_at_out_seq: SeqNum,
    /// `lastSentTime_`.
    last_sent_time: u64,
    /// `wasResent_`: set the moment a resend goes out, so the RTT average
    /// does not count the round trip of a resent packet.
    was_resent: bool,
    /// `reliableOrders_`: which runs of the packet were reliable, kept for
    /// when the resend is piggybacked.
    reliable_orders: Vec<ReliableOrder>,
}

impl UnackedPacket {
    /// `UnackedPacket( pPacket )` (unacked_packet.cpp:17): the send
    /// bookkeeping starts empty and is stamped by the channel when it sends.
    pub fn new(packet: Rc<Packet>) -> Self {
        Self {
            packet,
            last_sent_at_out_seq: 0,
            last_sent_time: 0,
            was_resent: false,
            reliable_orders: Vec::new(),
        }
    }

    /// `seq()` (hpp:20): the sequence number travels in the packet itself.
    pub fn seq(&self) -> SeqNum {
        self.packet.seq()
    }

    /// `pPacket_`.
    pub fn packet(&self) -> &Rc<Packet> {
        &self.packet
    }

    /// The write half of `pPacket_`, for the resend that re-stamps an
    /// indexed packet's channel version. The window's `Rc` is the sole
    /// owner at that point, so this mutates in place rather than copying.
    pub fn packet_mut(&mut self) -> &mut Packet {
        Rc::make_mut(&mut self.packet)
    }

    /// `lastSentAtOutSeq_`.
    pub fn last_sent_at_out_seq(&self) -> SeqNum {
        self.last_sent_at_out_seq
    }

    /// The channel stamps this when it moves the packet forward.
    pub fn set_last_sent_at_out_seq(&mut self, seq: SeqNum) {
        self.last_sent_at_out_seq = seq;
    }

    /// `lastSentTime_`.
    pub fn last_sent_time(&self) -> u64 {
        self.last_sent_time
    }

    /// The channel stamps this on every send and resend.
    pub fn set_last_sent_time(&mut self, now: u64) {
        self.last_sent_time = now;
    }

    /// `wasResent_`.
    pub fn was_resent(&self) -> bool {
        self.was_resent
    }

    /// Set by [`UdpChannel::send_unacked`](crate::udp_channel::UdpChannel).
    pub fn set_was_resent(&mut self, resent: bool) {
        self.was_resent = resent;
    }

    /// `reliableOrders_`.
    pub fn reliable_orders(&self) -> &[ReliableOrder] {
        &self.reliable_orders
    }

    /// `reliableOrders_.assign( roBeg, roEnd )` (udp_channel.cpp:843).
    pub fn set_reliable_orders(&mut self, orders: Vec<ReliableOrder>) {
        self.reliable_orders = orders;
    }

    // `initFromStream` / `addToStream` (unacked_packet.cpp:29-69) move with the
    // channel's state streaming, which needs the packet's stream encoding:
    // BW-02-03-06. Only the packet and `lastSentAtOutSeq_` are ever written;
    // `lastSentTime_` is deliberately not streamed -- the receiving channel
    // stamps it with the time the state arrives.
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packet with a known sequence number.
    fn packet_with_seq(seq: SeqNum) -> Rc<Packet> {
        let mut packet = Packet::new();
        packet.set_seq(seq);
        Rc::new(packet)
    }

    /// The send bookkeeping starts empty; the sequence number is the
    /// packet's own.
    #[test]
    fn the_send_bookkeeping_starts_empty() {
        let packet = packet_with_seq(0x00ff_ffff);
        let unacked = UnackedPacket::new(Rc::clone(&packet));

        assert_eq!(unacked.seq(), 0x00ff_ffff);
        assert!(Rc::ptr_eq(unacked.packet(), &packet), "pPacket_");
        assert_eq!(unacked.last_sent_at_out_seq(), 0);
        assert_eq!(unacked.last_sent_time(), 0);
        assert!(!unacked.was_resent());
        assert!(unacked.reliable_orders().is_empty());
    }

    /// The channel stamps the slot on send and marks it resent on a resend.
    #[test]
    fn a_slot_records_its_send_and_resend() {
        let mut unacked = UnackedPacket::new(packet_with_seq(7));
        unacked.set_last_sent_at_out_seq(5);
        unacked.set_last_sent_time(1_234);

        assert_eq!(unacked.last_sent_at_out_seq(), 5);
        assert_eq!(unacked.last_sent_time(), 1_234);
        assert!(!unacked.was_resent());

        unacked.set_was_resent(true);
        assert!(unacked.was_resent());
    }

    /// `reliableOrders_` carries the reliable orders the resend may
    /// piggyback.
    #[test]
    fn the_slot_holds_its_reliable_orders() {
        let mut unacked = UnackedPacket::new(packet_with_seq(3));
        assert!(unacked.reliable_orders().is_empty());

        let order = crate::reliable_order::ReliableOrder {
            packet: 0,
            offset: 1,
            length: 5,
            part_of_request: false,
        };
        unacked.set_reliable_orders(vec![order.clone()]);
        assert_eq!(unacked.reliable_orders(), [order]);
    }

    /// Two slots holding the same packet keep independent bookkeeping --
    /// the resend timing is per slot, not per packet.
    #[test]
    fn two_slots_keep_independent_bookkeeping() {
        let packet = packet_with_seq(11);
        let first = Rc::clone(&packet);
        let second = Rc::clone(&packet);

        let mut a = UnackedPacket::new(first);
        let mut b = UnackedPacket::new(second);
        a.set_last_sent_time(100);
        b.set_last_sent_time(200);
        b.set_was_resent(true);

        assert_eq!(a.last_sent_time(), 100);
        assert_eq!(b.last_sent_time(), 200);
        assert!(!a.was_resent());
        assert!(b.was_resent());
    }
}
