//! BW-02 Mercury packet framing constants and flag layout
//! (`lib/network/packet.hpp`).
//!
//! All multi-byte fields on the wire are little-endian (the reference's
//! `BW_HTON*` macros are byte-swap helpers for big-endian hosts and are
//! identity on x86 — see `binary_stream.ipp`).

/// Maximum UDP payload the reference sends, below the typical MTU
/// (`PACKET_MAX_SIZE`).
pub const PACKET_MAX_SIZE: usize = 1472;

/// Bytes of packet header: a single u16 flags field.
pub const HEADER_SIZE: usize = 2;

/// Worst-case footer: checksum(4) + piggyback block + channel id(4) +
/// channel version(4) + cumulative ack(4) + ack count(1) + acks + seq(4) +
/// first request offset(2) + fragment offsets(8) (`RESERVED_FOOTER_SIZE`).
pub const RESERVED_FOOTER_SIZE: usize = 27;

/// Payload budget left after reserving header + worst-case footer.
pub const MAX_CAPACITY: usize = PACKET_MAX_SIZE - HEADER_SIZE - RESERVED_FOOTER_SIZE;

/// Flags, `packet.hpp`. Unlisted bits are reserved and must stay zero when
/// writing; readers tolerate anything inside `KNOWN_FLAGS`.
pub mod flags {
    /// Packet contains at least one request (replies expected).
    pub const HAS_REQUESTS: u16 = 0x0001;
    /// Piggybacked raw block appended before the footer.
    pub const HAS_PIGGYBACKS: u16 = 0x0002;
    /// Ack block present.
    pub const HAS_ACKS: u16 = 0x0004;
    /// Sent on an established channel.
    pub const ON_CHANNEL: u16 = 0x0008;
    /// Reliable (sequence number + retransmission).
    pub const IS_RELIABLE: u16 = 0x0010;
    /// Fragment tail of a multi-packet message.
    pub const IS_FRAGMENT: u16 = 0x0020;
    /// Explicit sequence number field present.
    pub const HAS_SEQUENCE_NUMBER: u16 = 0x0040;
    /// Channel id selects an indexed sub-channel.
    pub const INDEXED_CHANNEL: u16 = 0x0080;
    /// Trailing checksum present.
    pub const HAS_CHECKSUM: u16 = 0x0100;
    /// Channel creation request from an anonymous sender.
    pub const CREATE_CHANNEL: u16 = 0x0200;
    /// Single cumulative ack instead of an ack list.
    pub const HAS_CUMULATIVE_ACK: u16 = 0x0400;

    /// Mask of every flag this build understands (`KNOWN_FLAGS`).
    pub const KNOWN_FLAGS: u16 = HAS_REQUESTS
        | HAS_PIGGYBACKS
        | HAS_ACKS
        | ON_CHANNEL
        | IS_RELIABLE
        | IS_FRAGMENT
        | HAS_SEQUENCE_NUMBER
        | INDEXED_CHANNEL
        | HAS_CHECKSUM
        | CREATE_CHANNEL
        | HAS_CUMULATIVE_ACK;
}

/// Message id 0xFF is reserved for replies (`Mercury::REPLY`).
pub const REPLY_MESSAGE_ID: u8 = 0xFF;

/// One packet on the wire (`lib/network/packet.cpp`).
///
/// This is the receive-shaped slice of `Packet`: the `PACKET_MAX_SIZE`
/// data array, the end-of-message offset, and the footer size it adds up
/// to. A fresh packet starts empty (`msgEndOffset_( 0 )`,
/// `footerSize_( 0 )`); `recvFromEndpoint` sets the offset to the byte
/// count the kernel handed back, so `totalSize()` is exactly what arrived.
/// The write side -- flags, acks, fragments -- is the BW-02-03 codec and
/// lands there.
pub struct Packet {
    data: Box<[u8; PACKET_MAX_SIZE]>,
    msg_end_offset: usize,
    footer_size: usize,
}

impl Packet {
    /// `Packet()`.
    pub fn new() -> Self {
        Self {
            data: Box::new([0; PACKET_MAX_SIZE]),
            msg_end_offset: 0,
            footer_size: 0,
        }
    }

    /// `msgEndOffset()`.
    pub fn msg_end_offset(&self) -> usize {
        self.msg_end_offset
    }

    /// `bodySize()`: everything after the flags word.
    pub fn body_size(&self) -> usize {
        self.msg_end_offset.saturating_sub(HEADER_SIZE)
    }

    /// `totalSize()`: `msgEndOffset_ + footerSize_`, the whole datagram.
    pub fn total_size(&self) -> usize {
        self.msg_end_offset + self.footer_size
    }

    /// The bytes that arrived, i.e. `data_[ .. msgEndOffset_ ]`.
    pub fn data(&self) -> &[u8] {
        &self.data[..self.msg_end_offset]
    }

    /// The raw backing array, for the read the kernel fills.
    fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data[..]
    }

    /// `Packet::recvFromEndpoint` (`packet.cpp:175`): recv into the data
    /// array and set the length on success. The return value is the
    /// low-level `recvfrom` result, byte count or -1, untouched.
    pub fn recv_from_endpoint(
        &mut self,
        ep: &crate::endpoint::Endpoint,
        addr: &mut crate::endpoint::Address,
    ) -> i32 {
        let len = ep.recvfrom(self.data_mut(), Some(&mut addr.port), Some(&mut addr.ip));
        if len >= 0 {
            self.msg_end_offset = len as usize;
        }
        len
    }
}

impl Default for Packet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::flags as f;
    use super::*;

    #[test]
    fn capacity_arithmetic_matches_reference() {
        assert_eq!(PACKET_MAX_SIZE, 1472);
        assert_eq!(HEADER_SIZE, 2);
        assert_eq!(RESERVED_FOOTER_SIZE, 27);
        assert_eq!(MAX_CAPACITY, 1472 - 2 - 27);
        assert_eq!(MAX_CAPACITY, 1443);
    }

    #[test]
    fn flag_values_are_pinned() {
        assert_eq!(f::HAS_REQUESTS, 0x0001);
        assert_eq!(f::HAS_PIGGYBACKS, 0x0002);
        assert_eq!(f::HAS_ACKS, 0x0004);
        assert_eq!(f::ON_CHANNEL, 0x0008);
        assert_eq!(f::IS_RELIABLE, 0x0010);
        assert_eq!(f::IS_FRAGMENT, 0x0020);
        assert_eq!(f::HAS_SEQUENCE_NUMBER, 0x0040);
        assert_eq!(f::INDEXED_CHANNEL, 0x0080);
        assert_eq!(f::HAS_CHECKSUM, 0x0100);
        assert_eq!(f::CREATE_CHANNEL, 0x0200);
        assert_eq!(f::HAS_CUMULATIVE_ACK, 0x0400);
    }

    #[test]
    fn known_flags_cover_exactly_the_documented_bits() {
        assert_eq!(f::KNOWN_FLAGS, 0x07FF);
        // A reliable, on-channel packet with checksum lands inside the mask.
        let typical = f::IS_RELIABLE | f::ON_CHANNEL | f::HAS_CHECKSUM;
        assert_eq!(typical & !f::KNOWN_FLAGS, 0);
    }

    #[test]
    fn reply_message_id_is_the_escape_value() {
        assert_eq!(REPLY_MESSAGE_ID, 0xFF);
    }

    /// A fresh packet is empty (`msgEndOffset_( 0 )`, `footerSize_( 0 )`),
    /// and a received datagram sets the offsets to what arrived.
    #[test]
    fn a_received_datagram_sets_its_offsets() {
        let mut listener = crate::endpoint::Endpoint::new();
        listener.socket(platform::socket::SOCK_DGRAM);
        assert_eq!(listener.bind(0, crate::endpoint::INADDR_ANY), 0);
        let bound = listener.local();

        let mut sender = crate::endpoint::Endpoint::new();
        sender.socket(platform::socket::SOCK_DGRAM);
        // Bind so `local()` names a precise address: an auto-bound socket
        // reports INADDR_ANY, not the interface the datagram left by.
        // 127.0.0.1 as the repo stores it (host-order word of the
        // network-order bytes).
        assert_eq!(sender.bind(0, 0x0100_007f), 0);
        let payload = b"hello packet";
        assert_eq!(
            sender.sendto(payload, bound.port, bound.ip),
            payload.len() as i32
        );

        let mut packet = Packet::new();
        assert_eq!(packet.msg_end_offset(), 0);
        assert_eq!(packet.total_size(), 0);
        assert_eq!(packet.body_size(), 0);

        let mut source = crate::endpoint::Address::default();
        assert_eq!(packet.recv_from_endpoint(&listener, &mut source), 12);
        assert_eq!(source, sender.local());
        assert_eq!(
            packet.total_size(),
            12,
            "footer is empty, size is the datagram"
        );
        assert_eq!(packet.body_size(), 10, "everything after the flags word");
        assert_eq!(packet.data(), payload);

        // A zero-length datagram is still a datagram: recvfrom returns 0
        // and the packet stays empty.
        assert_eq!(sender.sendto(b"", bound.port, bound.ip), 0);
        let mut second = Packet::new();
        assert_eq!(second.recv_from_endpoint(&listener, &mut source), 0);
        assert_eq!(second.total_size(), 0);
    }
}
