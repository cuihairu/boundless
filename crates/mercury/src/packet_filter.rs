//! `lib/network/packet_filter.hpp` and the BaseApp's enabled filter,
//! `server/baseapp/initial_connection_filter.cpp`.
//!
//! A `PacketFilter` is the seam Mercury uses to accept or reject a packet
//! before it is processed. The reference wires one onto a channel
//! (`UDPChannel::pFilter`, the encryption filter of BW-02-05) and one onto
//! an interface's off-channel path (`NetworkInterface::pOffChannelFilter`,
//! applied in `PacketReceiver::processPacket`). BW-02-04-03 wires the
//! latter: the `InitialConnectionFilter` a BaseApp puts on its external
//! interface admits only a `baseAppLogin` request and drops every other
//! stray datagram with `REASON_GENERAL_NETWORK`.
//!
//! The send side (`PacketFilter::send`, packet_filter.cpp:25),
//! `maxSpareSize` (packet_filter.hpp:56) and the channel-level filter
//! (`UDPChannel::pFilter`, wired in BW-02-05-01) are all of this module.

use crate::endpoint::Address;
use crate::misc::{MessageID, Reason};
use crate::packet::{self, HEADER_SIZE, Packet};

/// `lib/network/packet_filter.hpp`: a filter on packets entering or leaving
/// a channel or an interface.
///
/// The reference's base `recv` hands the packet to
/// `receiver.processFilteredPacket`, i.e. the normal processing continues;
/// a subclass inspects the packet first and either forwards it (calling
/// that base `recv`) or drops it. The port expresses the "continue" step
/// as the `next` continuation the caller passes in -- the interface
/// routes it to its own `deliver_packet`, the interface's send path to
/// its own `send_packet`.
pub trait PacketFilter {
    /// `maxSpareSize` (packet_filter.hpp:56): the room a filter may claim
    /// on each of its packets. The channel sizes its bundles off it
    /// (`UDPChannel::newBundle`, udp_channel.cpp:1523-1529); a filter
    /// that adds nothing declares zero.
    fn max_spare_size(&self) -> usize {
        0
    }

    /// `send` (packet_filter.cpp:25): decide an outbound packet's fate.
    /// The base forwards it to `next` (the reference's
    /// `packetSender.basicSendWithRetries`); a subclass may transform or
    /// drop the packet instead.
    fn send(&self, addr: &Address, packet: &mut Packet, next: &dyn Fn(&Address, &mut Packet)) {
        next(addr, packet)
    }

    /// `recv` (packet_filter.cpp:39): decide a received packet's fate.
    /// Return `Reason::Success` to hand the packet to `next` (the
    /// reference's `processFilteredPacket`); any other reason drops it.
    fn recv(
        &self,
        addr: &Address,
        packet: &mut Packet,
        next: &dyn Fn(&Address, &mut Packet) -> Reason,
    ) -> Reason;
}

/// `server/baseapp/initial_connection_filter.cpp`: the BaseApp external
/// interface's off-channel filter. It admits exactly one datagram shape --
/// a single `baseAppLogin` request -- and drops everything else with
/// `REASON_GENERAL_NETWORK` (the reference's "noise/malicious" warning).
pub struct InitialConnectionFilter {
    /// `BaseAppExtInterface::baseAppLogin.id()`: the message id the filter
    /// admits. A BaseApp declares it from its interface's declaration
    /// order (`InterfaceMinder::add`), so the caller supplies it.
    base_app_login_id: MessageID,
}

impl InitialConnectionFilter {
    /// `InitialConnectionFilter::create()`.
    pub fn new(base_app_login_id: MessageID) -> Self {
        Self { base_app_login_id }
    }

    /// The datagram the filter admits, in the reference's terms
    /// (initial_connection_filter.cpp:45-51):
    ///
    /// - flags word, exactly `FLAG_HAS_REQUESTS` (2 bytes),
    /// - the `MessageID` of `baseAppLogin` (1 byte),
    /// - `baseAppLoginArgs` (`SessionKey` 4 + `numAttempts` 1 = 5 bytes),
    /// - the request's `ReplyID` (4 bytes),
    /// - the request's next-request-offset link (2 bytes),
    /// - the packet's `firstRequestOffset` footer (2 bytes).
    ///
    /// The reference counts the last two as `2 * sizeof(Offset)`.
    pub const ADMITTED_SIZE: usize = 2 + 1 + 5 + 4 + 2 + 2;

    /// The admission test (initial_connection_filter.cpp:42-53): request
    /// flags only, the exact total size, and the first body byte as the
    /// `baseAppLogin` message id.
    fn admits(&self, packet: &Packet) -> bool {
        packet.flags() == packet::flags::HAS_REQUESTS
            && packet.total_size() == Self::ADMITTED_SIZE
            && packet.data().get(HEADER_SIZE) == Some(&self.base_app_login_id)
    }
}

impl PacketFilter for InitialConnectionFilter {
    fn recv(
        &self,
        addr: &Address,
        packet: &mut Packet,
        next: &dyn Fn(&Address, &mut Packet) -> Reason,
    ) -> Reason {
        if self.admits(packet) {
            return next(addr, packet);
        }

        // The reference prints this only under `receiver.isVerbose()`;
        // the port has no receiver here, so it warns unconditionally.
        eprintln!(
            "InitialConnectionFilter::recv( {} ): Dropping unexpected packet on BaseApps \
             external interface (noise/malicious)",
            addr.c_str()
        );
        Reason::GeneralNetwork
    }
}

/// Fixtures shared with the interface's processPacket tests, which drive
/// an interface with the filter installed and observe it through these.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// The `baseAppLogin` request the filter admits: flags(2) + id(1) +
    /// args(5) + ReplyID(4) + next-offset(2) + first-request-offset
    /// footer(2). `baseAppLogin` is the first message of the external
    /// interface, so its id is 0 (the fixture's declaration index).
    pub fn base_app_login_packet(id: MessageID) -> Packet {
        let mut wire = Vec::new();
        wire.extend_from_slice(&packet::flags::HAS_REQUESTS.to_le_bytes());
        wire.push(id);
        wire.extend_from_slice(&[0; 5]); // baseAppLoginArgs
        wire.extend_from_slice(&[0; 4]); // ReplyID
        wire.extend_from_slice(&0u16.to_le_bytes()); // next-request-offset link
        wire.extend_from_slice(&2u16.to_le_bytes()); // first-request-offset footer

        let mut packet = Packet::new();
        packet.grow(wire.len());
        packet.buffer_mut()[..wire.len()].copy_from_slice(&wire);
        packet
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::base_app_login_packet;
    use super::*;

    /// The acceptance: the filter admits the `baseAppLogin` request, so
    /// the continuation runs and its reason is returned.
    #[test]
    fn the_filter_admits_the_login_request() {
        let filter = InitialConnectionFilter::new(0);
        let addr = Address::new(0x0100_007f, 0);
        let mut packet = base_app_login_packet(0);

        let delivered = std::cell::Cell::new(0);
        let reason = filter.recv(&addr, &mut packet, &|_, _| {
            delivered.set(delivered.get() + 1);
            Reason::Success
        });

        assert_eq!(reason, Reason::Success);
        assert_eq!(delivered.get(), 1, "the allowed packet is delivered");
    }

    /// The acceptance: a packet that misses any of the filter's three
    /// checks is denied with `REASON_GENERAL_NETWORK` and never reaches
    /// the continuation -- wrong message id, wrong flags, wrong size.
    #[test]
    fn the_filter_denies_mismatching_packets() {
        let filter = InitialConnectionFilter::new(0);
        let addr = Address::new(0x0100_007f, 0);

        // Wrong message id.
        let mut wrong_id = base_app_login_packet(7);

        // Wrong flags: an extra `FLAG_HAS_PIGGYBACKS` bit makes the word
        // no longer exactly `FLAG_HAS_REQUESTS`.
        let mut wrong_flags = base_app_login_packet(0);
        wrong_flags.enable_flags(packet::flags::HAS_PIGGYBACKS);

        // Wrong size: one byte short of the admitted shape.
        let mut short = Packet::new();
        short.grow(InitialConnectionFilter::ADMITTED_SIZE - 1);
        short.buffer_mut()[0..2].copy_from_slice(&packet::flags::HAS_REQUESTS.to_le_bytes());
        short.buffer_mut()[2] = 0;

        let delivered = std::cell::Cell::new(0);
        let next = |_: &Address, _: &mut Packet| {
            delivered.set(delivered.get() + 1);
            Reason::Success
        };

        for packet in [&mut wrong_id, &mut wrong_flags, &mut short] {
            assert_eq!(
                filter.recv(&addr, packet, &next),
                Reason::GeneralNetwork,
                "a mismatching packet is denied"
            );
        }
        assert_eq!(delivered.get(), 0, "denied packets are not delivered");
    }

    /// The reference's base arms: `send` forwards its packet to the
    /// physical sender, and `maxSpareSize` declares no padding, so a
    /// filter that only inspects packets neither changes nor sizes them.
    #[test]
    fn the_base_send_forwards_and_claims_no_spare() {
        let filter = InitialConnectionFilter::new(0);
        let addr = Address::new(0x0100_007f, 0);
        let mut packet = base_app_login_packet(0);
        let sent = std::cell::Cell::new(0);

        filter.send(&addr, &mut packet, &|_, _| sent.set(sent.get() + 1));

        assert_eq!(sent.get(), 1, "the base send hands the packet on");
        assert_eq!(filter.max_spare_size(), 0, "it claims no spare room");
    }
}
