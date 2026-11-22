//! BW-02 Mercury packet framing constants and flag layout
//! (`lib/network/packet.hpp`).
//!
//! All multi-byte fields on the wire are little-endian (the reference's
//! `BW_HTON*` macros are byte-swap helpers for big-endian hosts and are
//! identity on x86 — see `binary_stream.ipp`).

use std::cell::RefCell;
use std::rc::Rc;

use crate::misc::{ChannelID, ChannelVersion, Reason, SEQ_NULL, SeqNum};

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

/// The maximum number of ACKs that can fit on a single packet
/// (`MAX_ACKS`: the `AckCount` type is one byte).
pub const MAX_ACKS: u8 = u8::MAX;

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
/// The `PACKET_MAX_SIZE` data array is the datagram; `msg_end_offset` is
/// the end of the headers and message data (temporarily wrong while
/// footers are being written, and on receive until they are stripped),
/// `footer_size` the bytes reserved for footers, and `next` the simple
/// linked list a multi-packet bundle chains.
pub struct Packet {
    /// The datagram.
    data: Box<[u8; PACKET_MAX_SIZE]>,
    /// `msgEndOffset_`: end of headers and message data.
    msg_end_offset: usize,
    /// `footerSize_`: bytes reserved for footers.
    footer_size: usize,
    /// `extraFilterSize_`: bytes reserved for the packet filter (writes
    /// only).
    extra_filter_size: usize,
    /// `firstRequestOffset_`.
    first_request_offset: u16,
    /// `pLastRequestOffset_`: the data offset of the "next request offset"
    /// field of the last request written to this packet. The reference
    /// stores the pointer; the offset is stable because the data array
    /// never moves.
    last_request_offset_pos: Option<usize>,
    /// `nAcks_`.
    n_acks: u8,
    /// `seq_`.
    seq: SeqNum,
    /// `channelID_`.
    channel_id: ChannelID,
    /// `channelVersion_`.
    channel_version: ChannelVersion,
    /// `fragBegin_`.
    frag_begin: SeqNum,
    /// `fragEnd_`.
    frag_end: SeqNum,
    /// `isPiggyback_`.
    is_piggyback: bool,
    /// `checksum_`.
    checksum: u32,
    /// `next_`: the chain of a multi-packet bundle.
    next: Option<Rc<RefCell<Packet>>>,
}

impl Packet {
    /// `Packet()`.
    pub fn new() -> Self {
        Self {
            data: Box::new([0; PACKET_MAX_SIZE]),
            msg_end_offset: 0,
            footer_size: 0,
            extra_filter_size: 0,
            first_request_offset: 0,
            last_request_offset_pos: None,
            n_acks: 0,
            seq: SEQ_NULL,
            channel_id: crate::misc::CHANNEL_ID_NULL,
            channel_version: 0,
            frag_begin: SEQ_NULL,
            frag_end: SEQ_NULL,
            is_piggyback: false,
            checksum: 0,
            next: None,
        }
    }

    // ------------------------------------------------------------------
    // Chain
    // ------------------------------------------------------------------

    /// `next()`: the rest of the bundle after this packet.
    pub fn next(&self) -> Option<Rc<RefCell<Packet>>> {
        self.next.clone()
    }

    /// `chain( Packet * )`: link `p` after this packet.
    pub fn chain(&mut self, p: Rc<RefCell<Packet>>) {
        self.next = Some(p);
    }

    /// `chainLength()`: how many packets this chain holds.
    pub fn chain_length(&self) -> usize {
        let mut count = 1;
        let mut node = self.next();
        while let Some(packet) = node {
            count += 1;
            node = packet.borrow().next();
        }
        count
    }

    // ------------------------------------------------------------------
    // Sizes and offsets
    // ------------------------------------------------------------------

    /// `msgEndOffset()`.
    pub fn msg_end_offset(&self) -> usize {
        self.msg_end_offset
    }

    /// `msgEndOffset( int )`: the setter half of the reference's overload.
    pub fn set_msg_end_offset(&mut self, offset: usize) {
        self.msg_end_offset = offset;
    }

    /// `grow( nBytes )`.
    pub fn grow(&mut self, n_bytes: usize) {
        self.msg_end_offset += n_bytes;
    }

    /// `shrink( nBytes )`.
    pub fn shrink(&mut self, n_bytes: usize) {
        self.msg_end_offset -= n_bytes;
    }

    /// `back()`: the write position, as a data offset. The reference hands
    /// back a pointer; callers here pair this with
    /// [`byte_range_mut`](Self::byte_range_mut) or a `grow`.
    pub fn back(&self) -> usize {
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

    /// `freeSpace()`: what `qreserve` can still take, respecting the
    /// reserved footer and filter space.
    pub fn free_space(&self) -> isize {
        PACKET_MAX_SIZE as isize
            - RESERVED_FOOTER_SIZE as isize
            - self.msg_end_offset as isize
            - self.footer_size as isize
            - self.extra_filter_size as isize
    }

    /// `reserveFooter( nBytes )`.
    pub fn reserve_footer(&mut self, n_bytes: usize) {
        self.footer_size += n_bytes;
    }

    /// `footerSize()`.
    pub fn footer_size(&self) -> usize {
        self.footer_size
    }

    /// `reserveFilterSpace( nBytes )`.
    pub fn reserve_filter_space(&mut self, n_bytes: usize) {
        self.extra_filter_size = n_bytes;
    }

    // ------------------------------------------------------------------
    // Data access
    // ------------------------------------------------------------------

    /// The bytes that arrived, i.e. `data_[ .. msgEndOffset_ ]`.
    pub fn data(&self) -> &[u8] {
        &self.data[..self.msg_end_offset]
    }

    /// The whole datagram a sent packet becomes: the message data plus the
    /// footer bytes packed above it (`data_[ .. totalSize() ]`).
    ///
    /// The reference sends `totalSize()` bytes from `data()`; here the
    /// message region and the footer region are contiguous in one array.
    pub fn datagram(&self) -> &[u8] {
        &self.data[..self.total_size()]
    }

    /// The raw backing array, for the read the kernel fills.
    fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data[..]
    }

    /// `data() + start`, `len` bytes: a read window into the datagram.
    pub fn byte_range(&self, start: usize, len: usize) -> &[u8] {
        &self.data[start..start + len]
    }

    /// The mutable write window the bundle's `qreserve` hands out.
    pub(crate) fn byte_range_mut(&mut self, start: usize, len: usize) -> &mut [u8] {
        &mut self.data[start..start + len]
    }

    /// `body()`: the message data after the flags word.
    pub fn body(&self) -> &[u8] {
        &self.data[HEADER_SIZE..self.msg_end_offset]
    }

    // ------------------------------------------------------------------
    // Flags
    // ------------------------------------------------------------------

    /// `flags()`: the header flags word.
    pub fn flags(&self) -> u16 {
        u16::from_le_bytes([self.data[0], self.data[1]])
    }

    /// `hasFlags( flags )`.
    pub fn has_flags(&self, flags: u16) -> bool {
        (self.flags() & flags) == flags
    }

    /// `setFlags( flags )`.
    pub fn set_flags(&mut self, flags: u16) {
        self.data[0..2].copy_from_slice(&flags.to_le_bytes());
    }

    /// `enableFlags( flags )`.
    pub fn enable_flags(&mut self, flags: u16) {
        let set = self.flags() | flags;
        self.set_flags(set);
    }

    /// `disableFlags( flags )`.
    pub fn disable_flags(&mut self, flags: u16) {
        let set = self.flags() & !flags;
        self.set_flags(set);
    }

    // ------------------------------------------------------------------
    // Footer packing
    // ------------------------------------------------------------------

    // The reference has one `packFooter<T>`/`stripFooter<T>` pair switched
    // on `sizeof(T)`; the port spells each width out. Unknown sizes hit a
    // `CRITICAL_MSG` there, which cannot happen with only these three.

    /// `packFooter( uint8 )`: write a one-byte footer below the current
    /// end.
    pub fn pack_footer_u8(&mut self, value: u8) {
        self.msg_end_offset -= 1;
        self.data[self.msg_end_offset] = value;
    }

    /// `packFooter( uint16 )`.
    pub fn pack_footer_u16(&mut self, value: u16) {
        self.msg_end_offset -= 2;
        self.data[self.msg_end_offset..self.msg_end_offset + 2]
            .copy_from_slice(&value.to_le_bytes());
    }

    /// `packFooter( uint32 )`.
    pub fn pack_footer_u32(&mut self, value: u32) {
        self.msg_end_offset -= 4;
        self.data[self.msg_end_offset..self.msg_end_offset + 4]
            .copy_from_slice(&value.to_le_bytes());
    }

    /// `stripFooter( uint8 )`: take a one-byte footer off the back. `None`
    /// is the reference's "not enough data" false.
    pub fn strip_footer_u8(&mut self) -> Option<u8> {
        if self.body_size() < 1 {
            return None;
        }
        self.msg_end_offset -= 1;
        self.footer_size += 1;
        Some(self.data[self.msg_end_offset])
    }

    /// `stripFooter( uint16 )`.
    pub fn strip_footer_u16(&mut self) -> Option<u16> {
        if self.body_size() < 2 {
            return None;
        }
        self.msg_end_offset -= 2;
        self.footer_size += 2;
        Some(u16::from_le_bytes([
            self.data[self.msg_end_offset],
            self.data[self.msg_end_offset + 1],
        ]))
    }

    /// `stripFooter( uint32 )`.
    pub fn strip_footer_u32(&mut self) -> Option<u32> {
        if self.body_size() < 4 {
            return None;
        }
        self.msg_end_offset -= 4;
        self.footer_size += 4;
        Some(u32::from_le_bytes(
            self.data[self.msg_end_offset..self.msg_end_offset + 4]
                .try_into()
                .unwrap(),
        ))
    }

    // ------------------------------------------------------------------
    // Requests
    // ------------------------------------------------------------------

    /// `addRequest( messageStart, nextRequestLink )`: link this request into
    /// the packet's request chain. `next_request_link` is the data offset of
    /// this request's "next request offset" field.
    pub fn add_request(&mut self, message_start: u16, next_request_link: usize) {
        if self.first_request_offset == 0 {
            self.first_request_offset = message_start;
        } else if let Some(offset) = self.last_request_offset_pos {
            self.data[offset..offset + 2].copy_from_slice(&message_start.to_le_bytes());
        }

        // Remember the offset of this link for next time, and mark this
        // request as the last one on this packet (for now).
        self.last_request_offset_pos = Some(next_request_link);
        self.data[next_request_link..next_request_link + 2].copy_from_slice(&0u16.to_le_bytes());
    }

    /// `firstRequestOffset()`.
    pub fn first_request_offset(&self) -> u16 {
        self.first_request_offset
    }

    /// `firstRequestOffset()`'s setter half, set when a received packet's
    /// footer is stripped.
    pub fn set_first_request_offset(&mut self, offset: u16) {
        self.first_request_offset = offset;
    }

    // ------------------------------------------------------------------
    // Housekeeping fields
    // ------------------------------------------------------------------

    /// `seq()`.
    pub fn seq(&self) -> SeqNum {
        self.seq
    }

    /// `seq()`'s setter half.
    pub fn set_seq(&mut self, seq: SeqNum) {
        self.seq = seq;
    }

    /// `nAcks()`.
    pub fn n_acks(&self) -> u8 {
        self.n_acks
    }

    /// `nAcks()`'s setter half.
    pub fn set_n_acks(&mut self, n_acks: u8) {
        self.n_acks = n_acks;
    }

    /// `channelID()`.
    pub fn channel_id(&self) -> ChannelID {
        self.channel_id
    }

    /// `channelID()`'s setter half.
    pub fn set_channel_id(&mut self, id: ChannelID) {
        self.channel_id = id;
    }

    /// `channelVersion()`.
    pub fn channel_version(&self) -> ChannelVersion {
        self.channel_version
    }

    /// `channelVersion()`'s setter half.
    pub fn set_channel_version(&mut self, version: ChannelVersion) {
        self.channel_version = version;
    }

    /// `isPiggyback()`.
    pub fn is_piggyback(&self) -> bool {
        self.is_piggyback
    }

    /// `isPiggyback( bool )`.
    pub fn set_is_piggyback(&mut self, value: bool) {
        self.is_piggyback = value;
    }

    /// `fragBegin()`.
    pub fn frag_begin(&self) -> SeqNum {
        self.frag_begin
    }

    /// `fragEnd()`.
    pub fn frag_end(&self) -> SeqNum {
        self.frag_end
    }

    // ------------------------------------------------------------------
    // Fragments and checksum
    // ------------------------------------------------------------------

    /// `stripFragInfo()`: take the fragment sequence numbers off the back
    /// and sanity-check them. False is the reference's "not a legal
    /// fragment spec" (with its warning already printed).
    pub fn strip_frag_info(&mut self) -> bool {
        if self.body_size() < 8 {
            eprintln!(
                "Packet::stripFragInfo: Not enough footers for fragment spec (have {} bytes but need 8)",
                self.body_size()
            );
            return false;
        }

        // Take off the fragment sequence numbers.
        // (The reference strips fragEnd_ then fragBegin_.)
        let Some(end) = self.strip_footer_u32() else {
            return false;
        };
        let Some(begin) = self.strip_footer_u32() else {
            return false;
        };
        self.frag_end = end;
        self.frag_begin = begin;

        let num_fragments_in_bundle = self.frag_end.wrapping_sub(self.frag_begin).wrapping_add(1);
        if num_fragments_in_bundle < 2 {
            eprintln!(
                "Packet::stripFragInfo: Illegal fragment count ({})",
                num_fragments_in_bundle
            );
            return false;
        }

        if self.seq < self.frag_begin || self.seq > self.frag_end {
            eprintln!(
                "Packet::stripFragInfo: Fragment range [#{},#{}] does not include packet's sequence #{}",
                self.frag_begin, self.frag_end, self.seq
            );
            return false;
        }

        true
    }

    /// `validateChecksum()`: strip and verify the checksum footer.
    pub fn validate_checksum(&mut self) -> bool {
        if self.has_flags(flags::HAS_CHECKSUM) {
            let Some(stripped) = self.strip_footer_u32() else {
                eprintln!(
                    "Packet::validateChecksum: Packet too short ({} bytes) for checksum!",
                    self.total_size()
                );
                return false;
            };
            self.checksum = stripped;

            // Zero data in checksum field on packet to avoid padding
            // issues, calculate, then put it back in case this packet is
            // forwarded on.
            let checksum_at = self.msg_end_offset;
            self.data[checksum_at..checksum_at + 4].fill(0);

            let sum = self.checksum_over(checksum_at);

            self.data[checksum_at..checksum_at + 4].copy_from_slice(&self.checksum.to_le_bytes());

            if sum != self.checksum {
                eprintln!(
                    "Packet::validateChecksum: Packet (flags {:x}, size {}) failed checksum (wanted {:08x}, got {:08x})",
                    self.flags(),
                    self.total_size(),
                    self.checksum,
                    sum
                );
                return false;
            }
        }

        true
    }

    /// `writeChecksum( Checksum * )`: fill in the checksum at data offset
    /// `checksum_at`, covering every 4-byte word up to it. The field must
    /// already have been packed (so it sits inside the datagram).
    pub fn write_checksum(&mut self, checksum_at: usize) {
        debug_assert!(checksum_at < PACKET_MAX_SIZE);
        debug_assert!(self.has_flags(flags::HAS_CHECKSUM));

        // Calculate the checksum and write it in.  We don't have to worry
        // about padding issues here because we have written a 0 into the
        // checksum field, and any overrun will be read from that field.
        self.data[checksum_at..checksum_at + 4].fill(0);

        let sum = self.checksum_over(checksum_at);
        self.data[checksum_at..checksum_at + 4].copy_from_slice(&sum.to_le_bytes());
    }

    /// The checksum itself: XOR of the 4-byte words from the start of the
    /// packet up to (but not including) `end`. The reference walks aligned
    /// `Checksum*`s, so a tail shorter than four bytes shares a word with
    /// the zeroed field just past the end; the round-up here is the same
    /// read.
    fn checksum_over(&self, end: usize) -> u32 {
        let mut sum: u32 = 0;
        for word in 0..end.div_ceil(4) {
            let at = word * 4;
            sum ^= u32::from_le_bytes(self.data[at..at + 4].try_into().unwrap());
        }
        sum
    }

    /// `PacketReceiver::processFilteredPacket`/`processOrderedPacket` footer
    /// strip, in the order the reference walks a received off-channel
    /// packet: checksum, then acks, then the sequence number, then the
    /// first request offset, then the fragment info.
    ///
    /// This is the reverse of [`UDPBundle::prepare_packets`]'s packing
    /// order -- the first-packed footer sits highest, so it comes off first
    /// -- and matches every `RETURN_FOR_CORRUPTED_PACKET` arm of the two
    /// reference functions. `None` is `REASON_SUCCESS` (the packet
    /// survived); `Some` is the reason it was rejected.
    ///
    /// Piggybacks (BW-02-03-04) and indexed channels (BW-02-03-02) are
    /// stripped by their own seams; the off-channel codec scope carries
    /// neither.
    pub fn strip_receive_footers(&mut self) -> Option<Reason> {
        // Start stripping footers: checksum first.
        if self.has_flags(flags::HAS_CHECKSUM) && !self.validate_checksum() {
            return Some(Reason::CorruptedPacket);
        }

        // Off-channel acks: the count, then that many sequence numbers.
        if self.has_flags(flags::HAS_ACKS) {
            let Some(count) = self.strip_footer_u8() else {
                return Some(Reason::CorruptedPacket);
            };
            self.set_n_acks(count);
            for _ in 0..count {
                if self.strip_footer_u32().is_none() {
                    return Some(Reason::CorruptedPacket);
                }
            }
        }

        // The sequence number.
        if self.has_flags(flags::HAS_SEQUENCE_NUMBER) {
            let Some(seq) = self.strip_footer_u32() else {
                return Some(Reason::CorruptedPacket);
            };
            self.set_seq(seq);
        }

        // The first request offset, which must point inside the packet.
        if self.has_flags(flags::HAS_REQUESTS) {
            let Some(offset) = self.strip_footer_u16() else {
                return Some(Reason::CorruptedPacket);
            };
            self.set_first_request_offset(offset);
            if offset as usize > self.msg_end_offset() {
                return Some(Reason::CorruptedPacket);
            }
        }

        // The fragment spec.
        if self.has_flags(flags::IS_FRAGMENT) && !self.strip_frag_info() {
            return Some(Reason::CorruptedPacket);
        }

        None
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
        assert_eq!(MAX_ACKS, 255);
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

    /// The flags word lives in the first two bytes, little-endian, and the
    /// enable/disable/set accessors dance around it exactly as the
    /// reference's pointer writes do.
    #[test]
    fn the_flags_word_reads_and_writes_in_place() {
        let mut packet = Packet::new();
        assert_eq!(packet.flags(), 0);

        packet.set_flags(f::HAS_ACKS | f::IS_RELIABLE);
        assert_eq!(packet.flags(), 0x0014);

        packet.enable_flags(f::HAS_CHECKSUM);
        assert_eq!(packet.flags(), 0x0114);
        assert!(packet.has_flags(f::HAS_ACKS));
        assert!(packet.has_flags(f::HAS_ACKS | f::IS_RELIABLE | f::HAS_CHECKSUM));
        assert!(!packet.has_flags(f::ON_CHANNEL));

        packet.disable_flags(f::IS_RELIABLE);
        assert_eq!(packet.flags(), 0x0104);

        // The word is really in the datagram's first two bytes.
        assert_eq!(&packet.byte_range(0, 2), &[0x04, 0x01]);
    }

    /// `grow`/`shrink`/`back` move the write cursor, and `free_space` is
    /// the capacity left after the reserved footer.
    #[test]
    fn the_write_cursor_counts_against_the_reserved_footer() {
        let mut packet = Packet::new();
        packet.set_msg_end_offset(HEADER_SIZE);
        assert_eq!(packet.back(), HEADER_SIZE);
        assert_eq!(packet.free_space() as usize, MAX_CAPACITY);

        packet.grow(100);
        assert_eq!(packet.free_space() as usize, MAX_CAPACITY - 100);

        packet.reserve_footer(4);
        assert_eq!(packet.free_space() as usize, MAX_CAPACITY - 104);
        assert_eq!(packet.total_size(), HEADER_SIZE + 100 + 4);

        packet.reserve_filter_space(8);
        assert_eq!(packet.free_space() as usize, MAX_CAPACITY - 112);

        packet.shrink(100);
        assert_eq!(packet.msg_end_offset(), HEADER_SIZE);
    }

    /// Footers pack below the end, working towards the message data, and
    /// the receive side strips them off again from the datagram's top —
    /// little-endian on the wire.
    #[test]
    fn footers_pack_below_the_end_and_strip_back_off() {
        let mut packet = Packet::new();
        packet.set_msg_end_offset(HEADER_SIZE + 10);
        let message_end = packet.msg_end_offset();
        packet.reserve_footer(2 + 4 + 1);
        packet.grow(2 + 4 + 1);

        // Pack: first packed sits highest, each next one below it.
        packet.pack_footer_u32(0xDEAD_BEEF);
        packet.pack_footer_u16(0xBEEF);
        packet.pack_footer_u8(7);

        assert_eq!(packet.msg_end_offset(), message_end);
        assert_eq!(packet.footer_size(), 7);
        // Layout from the message end up: [u8 7][u16 beef][u32 deadbeef].
        let at = message_end;
        assert_eq!(packet.datagram()[at], 7);
        assert_eq!(&packet.datagram()[at + 1..at + 3], &[0xEF, 0xBE]);
        assert_eq!(
            &packet.datagram()[at + 3..at + 7],
            &[0xEF, 0xBE, 0xAD, 0xDE]
        );

        // The receive side starts at the datagram's top and works down.
        let wire = packet.datagram().to_vec();
        let mut received = Packet::new();
        received.set_msg_end_offset(wire.len());
        received
            .byte_range_mut(0, wire.len())
            .copy_from_slice(&wire);

        // Strip in the order they were packed: the first packed sits at the
        // top, so it comes off first.
        assert_eq!(received.strip_footer_u32(), Some(0xDEAD_BEEF));
        assert_eq!(received.strip_footer_u16(), Some(0xBEEF));
        assert_eq!(received.strip_footer_u8(), Some(7));
        assert_eq!(received.msg_end_offset(), message_end);
        assert_eq!(received.footer_size(), 4 + 2 + 1, "strips count as footers");

        // A packet with no body left refuses to strip.
        let mut empty = Packet::new();
        assert_eq!(empty.strip_footer_u8(), None);
        assert_eq!(empty.strip_footer_u16(), None);
        assert_eq!(empty.strip_footer_u32(), None);
    }

    /// `addRequest` chains the requests: the first sets the packet's
    /// `firstRequestOffset`, later ones are threaded through the previous
    /// request's link field, and each new link is zeroed.
    #[test]
    fn requests_thread_through_their_link_fields() {
        let mut packet = Packet::new();
        packet.set_msg_end_offset(HEADER_SIZE);

        // Two requests, each with a link field at a known offset.
        packet.add_request(10, 20);
        packet.add_request(30, 40);

        assert_eq!(packet.first_request_offset(), 10);
        // The first request's link points at the second request's header...
        assert_eq!(&packet.byte_range(20, 2), &[30, 0]);
        // ...and the latest link is zero (end of the chain, for now).
        assert_eq!(&packet.byte_range(40, 2), &[0, 0]);
    }

    /// A checksum written over the datagram validates; a flipped byte in
    /// the covered region fails it; the field is restored either way.
    #[test]
    fn a_written_checksum_validates_until_the_data_changes() {
        let mut packet = Packet::new();
        packet.set_msg_end_offset(HEADER_SIZE + 9);
        let message_end = packet.msg_end_offset();
        packet.reserve_footer(4);
        packet.grow(4);
        packet.enable_flags(f::HAS_CHECKSUM);
        packet.pack_footer_u32(0);
        assert_eq!(packet.msg_end_offset(), message_end);
        assert_eq!(packet.total_size(), message_end + 4);

        let checksum_at = packet.msg_end_offset();
        packet.write_checksum(checksum_at);

        // The field holds the XOR of the words up to it (not zero, the
        // flags word alone contributes).
        let written = u32::from_le_bytes(packet.byte_range(checksum_at, 4).try_into().unwrap());
        assert_ne!(written, 0);
        // The covered region is the 4-byte-aligned window ending at the
        // (round-up) checksum field; the last word shares the zeroed field.
        let mut expected = 0u32;
        for word in 0..checksum_at.div_ceil(4) {
            let at = word * 4;
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(packet.byte_range(at, 4));
            // Zero out the part that is the checksum field itself.
            if at < checksum_at && checksum_at < at + 4 {
                for (i, b) in bytes.iter_mut().enumerate() {
                    if at + i >= checksum_at {
                        *b = 0;
                    }
                }
            }
            expected ^= u32::from_le_bytes(bytes);
        }
        assert_eq!(written, expected);

        // Round trip through a copy of the datagram bytes.
        let wire: Vec<u8> = packet.datagram().to_vec();
        let mut received = Packet::new();
        received.set_msg_end_offset(wire.len());
        received
            .byte_range_mut(0, wire.len())
            .copy_from_slice(&wire);

        assert!(received.has_flags(f::HAS_CHECKSUM));
        assert!(received.validate_checksum());
        // The checksum value is put back in place, so the wire bytes are
        // exactly what arrived (a packet forwarded on validates again).
        assert_eq!(received.datagram(), &wire[..]);

        // Corrupt one covered byte and it fails.
        received.byte_range_mut(3, 1)[0] ^= 0xFF;
        assert!(!received.validate_checksum());
    }

    /// Fragment info strips into the packet's `fragBegin`/`fragEnd` and is
    /// rejected when the numbers do not describe this packet.
    ///
    /// Builds a datagram then hands it to the receive side, which starts at
    /// the top of the wire bytes.
    fn received_fragment_packet(begin: SeqNum, end: SeqNum, seq: SeqNum) -> Packet {
        let mut sent = Packet::new();
        sent.set_msg_end_offset(HEADER_SIZE + 2);
        sent.reserve_footer(8);
        sent.grow(8);
        sent.pack_footer_u32(end); // fragEnd
        sent.pack_footer_u32(begin); // fragBegin
        sent.set_seq(seq);
        let wire = sent.datagram().to_vec();

        let mut received = Packet::new();
        received.set_msg_end_offset(wire.len());
        received
            .byte_range_mut(0, wire.len())
            .copy_from_slice(&wire);
        received.set_seq(seq);
        received
    }

    #[test]
    fn fragment_info_strips_and_is_sanity_checked() {
        let mut packet = received_fragment_packet(10, 12, 11);
        assert!(packet.strip_frag_info());
        assert_eq!(packet.frag_begin(), 10);
        assert_eq!(packet.frag_end(), 12);

        // Sequence outside the range is refused.
        let mut stranger = received_fragment_packet(10, 12, 99);
        assert!(!stranger.strip_frag_info());

        // So is a range that is not at least two packets.
        let mut tiny = received_fragment_packet(10, 10, 10);
        assert!(!tiny.strip_frag_info());

        // And a packet without the eight bytes for the pair.
        let mut short = Packet::new();
        short.set_msg_end_offset(HEADER_SIZE + 2);
        assert!(!short.strip_frag_info());
    }

    /// Packets chain into the list a multi-packet bundle walks.
    #[test]
    fn packets_chain_into_a_list() {
        let first = Rc::new(RefCell::new(Packet::new()));
        let second = Rc::new(RefCell::new(Packet::new()));
        let third = Rc::new(RefCell::new(Packet::new()));

        first.borrow_mut().chain(Rc::clone(&second));
        second.borrow_mut().chain(Rc::clone(&third));

        assert_eq!(first.borrow().chain_length(), 3);
        assert!(second.borrow().next().is_some());
        assert!(third.borrow().next().is_none());

        // The housekeeping fields start at their sentinels.
        assert_eq!(Packet::new().seq(), SEQ_NULL);
        assert_eq!(Packet::new().channel_id(), crate::misc::CHANNEL_ID_NULL);
        assert_eq!(Packet::new().channel_version(), 0);
        assert!(!Packet::new().is_piggyback());
        assert_eq!(Packet::new().n_acks(), 0);
        assert_eq!(Packet::new().first_request_offset(), 0);
    }

    /// A received datagram sets its offsets (BW-02-01-03's receive shape,
    /// kept as-is).
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
