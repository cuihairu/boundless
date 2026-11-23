//! `Mercury::ReliableOrder` (`lib/network/reliable_order.hpp`): one
//! contiguous run of a reliable message as it sits on a single packet.
//!
//! When a reliable packet is lost, the resend re-sends only the reliable
//! bytes of the dropped packet, piggybacked onto a later one
//! (`UDPBundle::piggyback`). These orders say which run of bytes each
//! dropped packet owes that later packet.
//!
//! The reference stores `segBegin` -- a raw pointer into the dropped
//! packet -- and marks the boundary between packets with a `NULL`
//! sentinel order. The port keeps the packet's index in the chain
//! instead: a packet index answers everywhere the pointer did, and the
//! sentinel is not needed because each order already knows which packet
//! it belongs to.

/// `Mercury::ReliableOrder` (`reliable_order.hpp:12-18`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReliableOrder {
    /// Which packet of the bundle's chain the run sits on.
    pub packet: usize,
    /// The run's first byte inside that packet.
    pub offset: usize,
    /// `segLength`.
    pub length: u16,
    /// `segPartOfRequest`: request bodies are not piggybacked at all.
    pub part_of_request: bool,
}
