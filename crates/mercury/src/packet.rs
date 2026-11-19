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

#[cfg(test)]
mod tests {
    use super::*;
    use super::flags as f;

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
}
