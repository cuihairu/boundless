//! The `Mercury::PacketFilter` seam (`lib/network/packet_filter.{hpp,cpp}`):
//! the filter an interface may install to gate which datagrams it is willing
//! to take before any channel exists.
//!
//! A filter hangs off the interface's `pOffChannelFilter_` slot
//! (network_interface.hpp:134-135, 328) and `processPacket` hands it every
//! datagram that found no channel (packet_receiver.cpp:492-497). It decides
//! by returning a `Reason`: the base class `recv` is the pass-through --
//! `PacketFilter::recv` (packet_filter.cpp:57) calls the receiver's
//! `processFilteredPacket` -- so a filter that admits a packet calls the
//! base behaviour, and one that denies it returns a failure reason and the
//! datagram never reaches the receive path at all.
//!
//! `InitialConnectionFilter` is the instance the reference enables on a
//! BaseApp's external interface; the trait is deliberately bare, so the
//! app-level filter is a plain `impl PacketFilter`.

use crate::endpoint::Address;
use crate::misc::Reason;
use crate::network_interface::NetworkInterface;
use crate::packet::Packet;

/// `Mercury::PacketFilter` (packet_filter.hpp:29).
pub trait PacketFilter {
    /// `recv`: decide the datagram. The default is the base class's
    /// pass-through to the receive path; overrides call it themselves for
    /// the packets they allow -- the reference's idiom is
    /// `return this->Mercury::PacketFilter::recv( ... )`.
    fn recv(&self, interface: &NetworkInterface, addr: &Address, packet: &mut Packet) -> Reason {
        interface.process_filtered_packet(addr, packet)
    }

    /// `maxSpareSize()`: the extra room the filter needs a Bundle to leave
    /// at the back of a packet. Zero by default, as the base class's.
    fn max_spare_size(&self) -> usize {
        0
    }
}
