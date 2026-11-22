//! BW-02 Mercury packet framing constants and flag layout
//! (`lib/network/packet.hpp/.cpp`).
//!
//! All multi-byte fields on the wire are little-endian (the reference's
//! `BW_HTON*` macros are byte-swap helpers for big-endian hosts and are
//! identity on x86 — see `binary_stream.ipp`).
//!
//! A packet is one datagram. The two flag bytes come first; the message
//! body follows; the footers trail the body, growing backwards from the
//! end. `msg_end_offset` always names the end of the body: while a packet
//! is being written it is the write cursor, and once footers have been
//! reserved it walks back over them, `footer_size` remembering how much
//! trails it so `total_size()` is the whole datagram either way.

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

use std::rc::Rc;

/// Message id 0xFF is reserved for replies (`Mercury::REPLY`).
pub const REPLY_MESSAGE_ID: u8 = 0xFF;

/// One packet on the wire (`lib/network/packet.cpp`).
///
/// The footer bookkeeping lives in the same offsets the reference uses:
/// `msg_end_offset` is the end of the message body (and, while writing,
/// the cursor new body bytes go to), `footer_size` is the reserved trail
/// behind it, and `extra_filter_size` the room an outbound filter may yet
/// claim. The request tracking fields hold the first request's body
/// offset and the patch point of the last link written, exactly where the
/// reference's `firstRequestOffset_`/`pLastRequestOffset_` sit.
pub struct Packet {
    data: Box<[u8; PACKET_MAX_SIZE]>,
    msg_end_offset: usize,
    footer_size: usize,
    extra_filter_size: usize,
    /// `firstRequestOffset_`: body offset of the first request, 0 = none.
    first_request_offset: usize,
    /// The data index `pLastRequestOffset_` points at, the link slot the
    /// next request patches.
    last_request_offset: Option<usize>,
    /// `seq_`: this packet's sequence number.
    seq: u32,
    /// `channelID_`.
    channel_id: i32,
    /// `channelVersion_`.
    channel_version: u32,
    /// `fragBegin_`/`fragEnd_`: the fragment number range, when this
    /// packet is one tail of a multi-packet message.
    frag_begin: u32,
    frag_end: u32,
    /// `isPiggyback_`: this packet was recovered from a piggyback block,
    /// not received whole.
    is_piggyback: bool,
    /// `pChain_`: the next packet in a receive-window chain, when this
    /// packet arrived in order but consecutive successors were already
    /// buffered ahead of it. [`Packet::chain`] links a run together; the
    /// receive dispatch walks it in sequence order (udp_channel.cpp:1281).
    chain_next: Option<Rc<Packet>>,
}

impl Packet {
    /// `Packet()`: empty, with the reference's sentinel fields set.
    pub fn new() -> Self {
        Self {
            data: Box::new([0; PACKET_MAX_SIZE]),
            msg_end_offset: 0,
            footer_size: 0,
            extra_filter_size: 0,
            first_request_offset: 0,
            last_request_offset: None,
            seq: crate::misc::SEQ_NULL,
            channel_id: crate::misc::CHANNEL_ID_NULL,
            channel_version: 0,
            frag_begin: crate::misc::SEQ_NULL,
            frag_end: crate::misc::SEQ_NULL,
            is_piggyback: false,
            chain_next: None,
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

    /// The bytes that arrived, i.e. `data_[ .. msgEndOffset_ ]`. After a
    /// footer strip this is flags plus body; after a prepare it is the
    /// same, the footers living past `totalSize()`.
    pub fn data(&self) -> &[u8] {
        &self.data[..self.msg_end_offset]
    }

    /// The datagram as it goes out or came in: `data_[ .. totalSize() ]`,
    /// footers included. The send path walks this.
    pub fn wire(&self) -> &[u8] {
        &self.data[..self.total_size()]
    }

    /// The whole backing array, for the read the kernel fills and for
    /// assembling a datagram by hand.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        &mut self.data[..]
    }

    // -- flags -----------------------------------------------------------

    /// `flags()`: the header word.
    pub fn flags(&self) -> u16 {
        u16::from_le_bytes([self.data[0], self.data[1]])
    }

    /// `hasFlags( f )`.
    pub fn has_flags(&self, f: u16) -> bool {
        self.flags() & f == f
    }

    /// `setFlags( f )`: overwrite the whole word.
    pub fn set_flags(&mut self, f: u16) {
        self.data[0..HEADER_SIZE].copy_from_slice(&f.to_le_bytes());
    }

    /// `enableFlags( f )`.
    pub fn enable_flags(&mut self, f: u16) {
        self.set_flags(self.flags() | f);
    }

    /// `disableFlags( f )`.
    pub fn disable_flags(&mut self, f: u16) {
        self.set_flags(self.flags() & !f);
    }

    // -- body growth ------------------------------------------------------

    /// `back()`: the index new body bytes go to.
    pub fn back(&self) -> usize {
        self.msg_end_offset
    }

    /// `grow( nBytes )`: claim body space, moving the cursor.
    pub fn grow(&mut self, n_bytes: usize) {
        self.msg_end_offset += n_bytes;
    }

    /// `shrink( nBytes )`: give body space back.
    pub fn shrink(&mut self, n_bytes: usize) {
        self.msg_end_offset -= n_bytes;
    }

    /// `freeSpace()`: `MAX_SIZE - RESERVED_FOOTER_SIZE - msgEndOffset -
    /// footerSize - extraFilterSize` — the body room left with the
    /// worst-case footers held back.
    pub fn free_space(&self) -> usize {
        PACKET_MAX_SIZE
            - RESERVED_FOOTER_SIZE
            - self.msg_end_offset
            - self.footer_size
            - self.extra_filter_size
    }

    /// `reserveFooter( nBytes )`: promise `n` trail bytes the body must
    /// not touch.
    pub fn reserve_footer(&mut self, n_bytes: usize) {
        self.footer_size += n_bytes;
    }

    /// `reserveFilterSpace( nBytes )`: set the room a filter may claim.
    pub fn reserve_filter_space(&mut self, n_bytes: usize) {
        self.extra_filter_size = n_bytes;
    }

    /// `extraFilterSize()`.
    pub fn extra_filter_size(&self) -> usize {
        self.extra_filter_size
    }

    /// `footerSize()`.
    pub fn footer_size(&self) -> usize {
        self.footer_size
    }

    // -- footers ----------------------------------------------------------
    //
    // `packFooter` writes a footer at the back of the body, walking the
    // cursor back over it; `stripFooter` is its inverse for a received
    // packet, moving the bytes from body to footer space. Both are the
    // reference's `packFooter< TYPE >`/`stripFooter< TYPE >` templates,
    // split per width.

    /// `packFooter< uint8 >( value )`.
    pub fn pack_footer_u8(&mut self, value: u8) {
        self.msg_end_offset -= 1;
        self.data[self.msg_end_offset] = value;
    }

    /// `packFooter< uint16 >( value )`.
    pub fn pack_footer_u16(&mut self, value: u16) {
        self.msg_end_offset -= 2;
        self.data[self.msg_end_offset..self.msg_end_offset + 2]
            .copy_from_slice(&value.to_le_bytes());
    }

    /// `packFooter< uint32 >( value )`.
    pub fn pack_footer_u32(&mut self, value: u32) {
        self.msg_end_offset -= 4;
        self.data[self.msg_end_offset..self.msg_end_offset + 4]
            .copy_from_slice(&value.to_le_bytes());
    }

    /// `stripFooter< uint8 >()`: false when the body has no room for it.
    pub fn strip_footer_u8(&mut self) -> Option<u8> {
        if self.msg_end_offset < HEADER_SIZE + 1 {
            return None;
        }
        self.msg_end_offset -= 1;
        self.footer_size += 1;
        Some(self.data[self.msg_end_offset])
    }

    /// `stripFooter< uint16 >()`.
    pub fn strip_footer_u16(&mut self) -> Option<u16> {
        if self.msg_end_offset < HEADER_SIZE + 2 {
            return None;
        }
        self.msg_end_offset -= 2;
        self.footer_size += 2;
        Some(u16::from_le_bytes(
            self.data[self.msg_end_offset..self.msg_end_offset + 2]
                .try_into()
                .unwrap(),
        ))
    }

    /// `stripFooter< uint32 >()`.
    pub fn strip_footer_u32(&mut self) -> Option<u32> {
        if self.msg_end_offset < HEADER_SIZE + 4 {
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

    // -- requests ---------------------------------------------------------

    /// `addRequest( messageStart, nextRequestLink )`: record the first
    /// request's body offset, patch the previous link to point at this
    /// message, and leave the new link slot zeroed for the next one.
    pub fn add_request(&mut self, message_start: u16, next_request_link: u16) {
        if self.first_request_offset == 0 {
            self.first_request_offset = message_start as usize;
        } else {
            let slot = self
                .last_request_offset
                .expect("a request followed another");
            self.data[slot..slot + 2].copy_from_slice(&message_start.to_le_bytes());
        }
        self.last_request_offset = Some(next_request_link as usize);
        self.data[next_request_link as usize..next_request_link as usize + 2]
            .copy_from_slice(&0u16.to_le_bytes());
    }

    /// `firstRequestOffset()`: body offset of the first request, 0 when
    /// the packet carries none.
    pub fn first_request_offset(&self) -> usize {
        self.first_request_offset
    }

    /// `seq()`.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// `chainNext()` (packet.hpp): the next packet in the receive-window
    /// chain, or `None` at the tail of a run.
    pub fn chain_next(&self) -> Option<Rc<Packet>> {
        self.chain_next.clone()
    }

    /// The write half of [`chain_next`](Self::chain_next), without cloning:
    /// the receive-window chain walk re-borrows each link, which needs the
    /// slot itself rather than a clone -- a clone would push a link's
    /// refcount to two and break `Rc::get_mut`.
    pub(crate) fn chain_next_mut(&mut self) -> Option<&mut Rc<Packet>> {
        self.chain_next.as_mut()
    }

    /// The chain link itself, so a chain can be walked by taking each link
    /// out of its predecessor (`FragmentedBundle` splices fragments in
    /// sequence order).
    pub(crate) fn chain_next_slot_mut(&mut self) -> &mut Option<Rc<Packet>> {
        &mut self.chain_next
    }

    /// `chain( Packet * )` (packet.hpp): link `next` behind this packet in
    /// the receive-window chain, so one dispatch walks the run in sequence
    /// order. The reference also folds the chained length into its message
    /// size; the port keeps each packet whole, so only the link is needed.
    pub fn chain(&mut self, next: Rc<Packet>) {
        self.chain_next = Some(next);
    }

    /// The write half of `seq()`.
    pub fn set_seq(&mut self, seq: u32) {
        self.seq = seq;
    }

    /// `channelID()`.
    pub fn channel_id(&self) -> i32 {
        self.channel_id
    }

    /// `channelVersion()`.
    pub fn channel_version(&self) -> u32 {
        self.channel_version
    }

    /// The reference's writable `channelVersion()` overload.
    pub fn set_channel_version(&mut self, channel_version: u32) {
        self.channel_version = channel_version;
    }

    /// `fragBegin()`.
    pub fn frag_begin(&self) -> u32 {
        self.frag_begin
    }

    /// `fragEnd()`.
    pub fn frag_end(&self) -> u32 {
        self.frag_end
    }

    /// The write half of `fragBegin`/`fragEnd`, as the send path writes a
    /// fragment's footers (`strip_frag_info` is the read half the receive
    /// side uses).
    pub fn set_frag_info(&mut self, frag_begin: u32, frag_end: u32) {
        self.frag_begin = frag_begin;
        self.frag_end = frag_end;
    }

    /// `isPiggyback()`.
    pub fn is_piggyback(&self) -> bool {
        self.is_piggyback
    }

    /// `shouldCreateAnonymous()`: the first packet on a non-indexed
    /// channel asks its peer to create one.
    pub fn should_create_anonymous(&self) -> bool {
        self.has_flags(flags::ON_CHANNEL)
            && self.has_flags(flags::CREATE_CHANNEL)
            && !self.has_flags(flags::INDEXED_CHANNEL)
    }

    /// `stripFragInfo()`: pull the fragment footers off a received
    /// packet, filling `fragEnd_`/`fragBegin_`. False when the body is
    /// too short, the range holds fewer than two fragments, or the
    /// packet's sequence number sits outside the range.
    pub fn strip_frag_info(&mut self) -> bool {
        // Two u32 footers must fit in the body.
        if self.msg_end_offset < HEADER_SIZE + 8 {
            return false;
        }

        self.frag_end = self.strip_footer_u32().expect("bounds checked above");
        self.frag_begin = self.strip_footer_u32().expect("bounds checked above");

        let num_fragments = self.frag_end - self.frag_begin + 1;
        if num_fragments < 2 {
            return false;
        }

        if self.seq < self.frag_begin || self.seq > self.frag_end {
            return false;
        }

        true
    }

    // -- checksum ---------------------------------------------------------

    /// `writeChecksum( pChecksum )`: zero the four-byte field at
    /// `checksum_offset`, XOR every 32-bit word from the flags word up to
    /// it, and store the sum there. The XOR walks whole words, so a body
    /// length that is not a multiple of four reads its last word across
    /// the zeroed field -- the same overrun the reference does.
    pub fn write_checksum(&mut self, checksum_offset: usize) {
        self.data[checksum_offset..checksum_offset + 4].copy_from_slice(&[0; 4]);

        let mut checksum = 0u32;
        let mut start = 0;
        while start < checksum_offset {
            checksum ^= u32::from_le_bytes(self.data[start..start + 4].try_into().unwrap());
            start += 4;
        }

        self.data[checksum_offset..checksum_offset + 4].copy_from_slice(&checksum.to_le_bytes());
    }

    /// `validateChecksum()`: strip the trailing checksum footer, recompute
    /// over the packet as it arrived, and answer whether it matches. The
    /// packet is left as `validateChecksum` leaves it: stripped, with the
    /// field restored for the caller to skip over.
    pub fn validate_checksum(&mut self) -> bool {
        let stored = match self.strip_footer_u32() {
            Some(stored) => stored,
            None => return false,
        };

        let checksum_offset = self.msg_end_offset;
        self.data[checksum_offset..checksum_offset + 4].copy_from_slice(&[0; 4]);

        let mut checksum = 0u32;
        let mut start = 0;
        while start < checksum_offset {
            checksum ^= u32::from_le_bytes(self.data[start..start + 4].try_into().unwrap());
            start += 4;
        }

        self.data[checksum_offset..checksum_offset + 4].copy_from_slice(&stored.to_le_bytes());

        if checksum != stored {
            eprintln!(
                "Packet (flags {:x}, size {}) failed checksum (wanted {:08x}, got {:08x})",
                self.flags(),
                self.total_size(),
                checksum,
                stored
            );
            return false;
        }

        true
    }

    // -- piggybacks -------------------------------------------------------

    /// `processPiggybackPackets( visitor )`: peel the piggybacked packets
    /// off the end, handing each to `visitor` as its own packet. The
    /// lengths are i16s; the last one is stored one's complemented.
    /// False when the footer runs out, a length overruns the body, or a
    /// visitor says stop.
    pub fn process_piggyback_packets(
        &mut self,
        mut visitor: impl FnMut(&mut Packet) -> bool,
    ) -> bool {
        loop {
            let raw = match self.strip_footer_u16() {
                Some(raw) => raw,
                None => return false,
            };

            // A negative length marks the last piggyback: `~len` in i16 is
            // `!raw` over the same sixteen bits.
            let is_last = raw & 0x8000 != 0;
            let len = if is_last { !raw } else { raw } as usize;

            if self.body_size() < len {
                return false;
            }

            self.shrink(len);
            let start = self.msg_end_offset;

            let mut inner = Packet::new();
            inner.data[..len].copy_from_slice(&self.data[start..start + len]);
            inner.msg_end_offset = len;
            inner.is_piggyback = true;

            if !visitor(&mut inner) {
                return false;
            }

            if is_last {
                return true;
            }
        }
    }

    /// `Packet::recvFromEndpoint` (`packet.cpp:175`): recv into the data
    /// array and set the length on success. The return value is the
    /// low-level `recvfrom` result, byte count or -1, untouched.
    pub fn recv_from_endpoint(
        &mut self,
        ep: &crate::endpoint::Endpoint,
        addr: &mut crate::endpoint::Address,
    ) -> i32 {
        let len = ep.recvfrom(self.buffer_mut(), Some(&mut addr.port), Some(&mut addr.ip));
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

    // ------------------------------------------------------------------
    // Write side: flags and body growth
    // ------------------------------------------------------------------

    /// The flags word lives in the first two bytes on the wire, and
    /// enable/disable go through it.
    #[test]
    fn the_flags_word_is_the_wire_header() {
        let mut packet = Packet::new();
        assert_eq!(packet.flags(), 0);

        packet.set_flags(f::HAS_REQUESTS | f::HAS_CHECKSUM);
        assert!(packet.has_flags(f::HAS_REQUESTS));
        assert!(!packet.has_flags(f::HAS_ACKS));

        packet.grow(HEADER_SIZE);
        assert_eq!(
            &packet.data()[..HEADER_SIZE],
            &[0x01, 0x01],
            "little-endian"
        );

        packet.enable_flags(f::IS_RELIABLE);
        assert_eq!(
            packet.flags(),
            f::HAS_REQUESTS | f::HAS_CHECKSUM | f::IS_RELIABLE
        );

        packet.disable_flags(f::HAS_CHECKSUM);
        assert_eq!(packet.flags(), f::HAS_REQUESTS | f::IS_RELIABLE);
    }

    /// `freeSpace` holds the worst-case footers back, and
    /// `reserveFilterSpace` takes its room out of the same budget.
    #[test]
    fn free_space_holds_the_worst_case_back() {
        let mut packet = Packet::new();
        assert_eq!(
            packet.free_space(),
            PACKET_MAX_SIZE - RESERVED_FOOTER_SIZE,
            "a fresh packet holds the worst case back before anything else"
        );

        packet.set_flags(0);
        packet.grow(HEADER_SIZE);
        packet.grow(100);
        assert_eq!(packet.msg_end_offset(), HEADER_SIZE + 100);
        assert_eq!(
            packet.free_space(),
            PACKET_MAX_SIZE - RESERVED_FOOTER_SIZE - HEADER_SIZE - 100
        );

        packet.reserve_footer(10);
        assert_eq!(
            packet.free_space(),
            PACKET_MAX_SIZE - RESERVED_FOOTER_SIZE - HEADER_SIZE - 110
        );

        packet.reserve_filter_space(5);
        assert_eq!(packet.extra_filter_size(), 5);
        assert_eq!(
            packet.free_space(),
            PACKET_MAX_SIZE - RESERVED_FOOTER_SIZE - HEADER_SIZE - 115
        );
        assert_eq!(
            packet.total_size(),
            HEADER_SIZE + 110,
            "reserved footers trail"
        );
    }

    // ------------------------------------------------------------------
    // Footers
    // ------------------------------------------------------------------

    /// `packFooter` walks the cursor backwards over each footer through
    /// the space `reserveFooter` held; the wire bytes end up after the
    /// body, little-endian, and the cursor returns to the body's end.
    #[test]
    fn packed_footers_trail_the_body_backwards() {
        let mut packet = Packet::new();
        packet.set_flags(f::HAS_REQUESTS);
        packet.grow(HEADER_SIZE);
        packet.buffer_mut()[HEADER_SIZE..HEADER_SIZE + 3].copy_from_slice(b"abc");
        packet.grow(3);

        // `preparePackets` grows into the reserved footers before packing.
        packet.reserve_footer(7);
        packet.grow(7);

        packet.pack_footer_u8(0x42);
        packet.pack_footer_u16(0x1234);
        packet.pack_footer_u32(0xDEAD_BEEF);

        // The body ends where it ended before packing; everything packed
        // sits behind it.
        assert_eq!(packet.msg_end_offset(), HEADER_SIZE + 3);
        assert_eq!(&packet.data()[..5], b"\x01\x00abc");
        assert_eq!(
            &packet.wire()[5..],
            &[0xEF, 0xBE, 0xAD, 0xDE, 0x34, 0x12, 0x42],
            "checksum word, then seq, then the byte, wire order"
        );
    }

    /// `stripFooter` moves footers back into footer space: the body
    /// shrinks, `totalSize` stays, and a body too short refuses.
    #[test]
    fn stripped_footers_move_from_body_to_footer() {
        let mut packet = Packet::new();
        let wire = [
            0x07, 0x00, 1, 2, 3, 4, 0xEF, 0xBE, 0xAD, 0xDE, 0x34, 0x12, 0x42,
        ];
        packet.grow(wire.len());
        packet.buffer_mut()[..wire.len()].copy_from_slice(&wire);

        assert_eq!(packet.strip_footer_u8(), Some(0x42));
        assert_eq!(packet.strip_footer_u16(), Some(0x1234));
        assert_eq!(packet.strip_footer_u32(), Some(0xDEAD_BEEF));
        assert_eq!(
            packet.msg_end_offset(),
            6,
            "flags word plus four body bytes"
        );
        assert_eq!(packet.footer_size(), 7);
        assert_eq!(packet.total_size(), wire.len(), "size survives the strip");
        assert_eq!(packet.body_size(), 4);

        // The strip keeps going while there is anything past the flags
        // word; a packet with no body has nothing to give.
        let mut bare = Packet::new();
        bare.grow(HEADER_SIZE);
        assert_eq!(bare.strip_footer_u8(), None, "nothing left but the flags");
        let mut tiny = Packet::new();
        tiny.grow(HEADER_SIZE + 1);
        tiny.buffer_mut()[2] = 9;
        assert_eq!(tiny.strip_footer_u8(), Some(9));
        assert_eq!(tiny.strip_footer_u8(), None, "nothing left but the flags");
    }

    /// A packet with nothing past the flags word refuses every footer:
    /// the piggyback peel and the checksum validation both answer "no".
    #[test]
    fn footers_past_the_flags_word_are_refused() {
        let mut bare = Packet::new();
        bare.grow(HEADER_SIZE);
        assert!(
            !bare.process_piggyback_packets(|_| true),
            "there is no piggyback footer to peel"
        );
        assert!(
            !bare.validate_checksum(),
            "there is no checksum footer to validate"
        );
    }

    /// `channelVersion()` reads what the channel layer stamped on the
    /// packet; the reference's writable overload becomes a setter.
    #[test]
    fn a_packet_carries_a_channel_version() {
        let mut packet = Packet::new();
        assert_eq!(packet.channel_version(), 0, "fresh packets carry zero");
        packet.set_channel_version(7);
        assert_eq!(packet.channel_version(), 7);
    }

    // ------------------------------------------------------------------
    // Requests
    // ------------------------------------------------------------------

    /// `addRequest` records the first request's offset and zero-fills its
    /// link; a second request patches the first's link to point at it,
    /// the last link staying zero.
    #[test]
    fn request_links_chain_through_the_patch_point() {
        let mut packet = Packet::new();
        packet.grow(HEADER_SIZE);

        // One request with its preamble at body offset 0: the message
        // starts there, its link slot sits six bytes further in.
        packet.add_request(2, 2 + 6);
        assert_eq!(packet.first_request_offset(), 2);
        assert_eq!(
            &packet.buffer_mut()[2 + 6..2 + 8],
            &[0, 0],
            "the only link stays empty"
        );

        // A second request at body offset 20 patches the first link.
        packet.add_request(2 + 20, 2 + 20 + 6);
        assert_eq!(
            &packet.buffer_mut()[2 + 6..2 + 8],
            &(2 + 20u16).to_le_bytes(),
            "the first link now names the second request"
        );
        assert_eq!(&packet.buffer_mut()[2 + 26..2 + 28], &[0, 0]);
    }

    // ------------------------------------------------------------------
    // Fragments
    // ------------------------------------------------------------------

    /// `stripFragInfo` reads the fragment range off the end of a received
    /// datagram -- first-seq at the lower address, last-seq above it --
    /// and checks the range against the packet's own sequence number.
    #[test]
    fn fragment_footers_are_stripped_and_checked() {
        let mut packet = Packet::new();
        packet.set_seq(3);
        packet.grow(HEADER_SIZE);
        packet.grow(4); // a body the footers trail

        // The footers as they arrive on the wire: fragBegin, then
        // fragEnd above it (the pack order leaves last-seq on top).
        packet.buffer_mut()[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&2u32.to_le_bytes());
        packet.buffer_mut()[HEADER_SIZE + 8..HEADER_SIZE + 12].copy_from_slice(&5u32.to_le_bytes());
        packet.grow(8);

        assert!(packet.strip_frag_info());
        assert_eq!(packet.frag_begin(), 2);
        assert_eq!(packet.frag_end(), 5);
        assert_eq!(packet.seq(), 3, "inside the range");
        assert_eq!(packet.body_size(), 4, "the footers left the body");

        // The packet's number outside the range is refused.
        let mut outside = Packet::new();
        outside.set_seq(6);
        outside.grow(HEADER_SIZE + 4);
        outside.buffer_mut()[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&2u32.to_le_bytes());
        outside.buffer_mut()[HEADER_SIZE + 8..HEADER_SIZE + 12]
            .copy_from_slice(&5u32.to_le_bytes());
        outside.grow(8);
        assert!(!outside.strip_frag_info());

        // A single-fragment range is refused.
        let mut single = Packet::new();
        single.set_seq(4);
        single.grow(HEADER_SIZE + 4);
        single.buffer_mut()[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&4u32.to_le_bytes());
        single.buffer_mut()[HEADER_SIZE + 8..HEADER_SIZE + 12].copy_from_slice(&4u32.to_le_bytes());
        single.grow(8);
        assert!(!single.strip_frag_info());

        // A body too short to hold both footers is refused.
        let mut short = Packet::new();
        short.grow(HEADER_SIZE);
        short.buffer_mut()[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&5u32.to_le_bytes());
        short.grow(4);
        assert!(!short.strip_frag_info());
    }

    // ------------------------------------------------------------------
    // Checksum
    // ------------------------------------------------------------------

    /// `writeChecksum` then `validateChecksum` agree, a flipped byte is
    /// caught, and an odd body length still validates (the last XOR word
    /// reaches across the zeroed field, exactly as the reference's does).
    #[test]
    fn the_checksum_covers_the_whole_datagram() {
        let mut packet = Packet::new();
        packet.set_flags(f::HAS_CHECKSUM);
        packet.grow(HEADER_SIZE);
        packet.buffer_mut()[HEADER_SIZE..HEADER_SIZE + 11].copy_from_slice(b"hello world");
        packet.grow(11);
        let checksum_at = packet.msg_end_offset();
        packet.reserve_footer(4);

        packet.write_checksum(checksum_at);
        assert_eq!(packet.total_size(), checksum_at + 4);

        // Deliver the exact bytes and validate them.
        let wire: Vec<u8> = packet.wire().to_vec();
        let mut received = Packet::new();
        received.grow(wire.len());
        received.buffer_mut()[..wire.len()].copy_from_slice(&wire);
        assert!(
            received.validate_checksum(),
            "an untouched packet validates"
        );
        assert_eq!(received.total_size(), wire.len(), "the field is restored");
        assert_eq!(&received.buffer_mut()[..wire.len()], &wire[..]);

        // One flipped payload byte breaks the match.
        let mut corrupt = Packet::new();
        corrupt.grow(wire.len());
        corrupt.buffer_mut()[..wire.len()].copy_from_slice(&wire);
        corrupt.buffer_mut()[4] ^= 0x01;
        assert!(!corrupt.validate_checksum());
    }

    // ------------------------------------------------------------------
    // Piggybacks
    // ------------------------------------------------------------------

    /// `processPiggybackPackets` peels each length-suffixed block off the
    /// end into its own packet, the last packed length one's complemented,
    /// and the visitor's verdict stops the peel.
    #[test]
    fn piggyback_blocks_peel_off_the_end() {
        // Body "body", then two piggyback blocks. Packing walks down from
        // the top -- the first piggyback's block ends up highest -- so the
        // wire carries [two block][~len two][one block][len one] and the
        // peel recovers them in pack order.
        let mut packet = Packet::new();
        packet.set_flags(f::HAS_PIGGYBACKS);
        packet.grow(HEADER_SIZE);
        packet.buffer_mut()[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(b"body");
        packet.grow(4);

        // Piggyback one: flags word 0 + three payload bytes.
        let mut one = Vec::new();
        one.extend_from_slice(&0u16.to_le_bytes());
        one.extend_from_slice(b"xyz");
        // Piggyback two: flags word 0 + seq 7.
        let mut two = Vec::new();
        two.extend_from_slice(&0u16.to_le_bytes());
        two.extend_from_slice(&7u32.to_le_bytes());

        let mut wire = Vec::new();
        wire.extend_from_slice(&two);
        wire.extend_from_slice(&(!(two.len() as u16)).to_le_bytes());
        wire.extend_from_slice(&one);
        wire.extend_from_slice(&(one.len() as u16).to_le_bytes());
        packet.grow(wire.len());
        packet.buffer_mut()[HEADER_SIZE + 4..HEADER_SIZE + 4 + wire.len()].copy_from_slice(&wire);

        let mut seen: Vec<(usize, bool)> = Vec::new();
        let ok = packet.process_piggyback_packets(|inner| {
            seen.push((inner.body_size(), inner.is_piggyback()));
            true
        });

        assert!(ok);
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0],
            (one.len() - HEADER_SIZE, true),
            "the first piggyback peels first"
        );
        assert_eq!(seen[1], (two.len() - HEADER_SIZE, true));
        assert_eq!(
            packet.body_size(),
            4,
            "the real body is left after the peel"
        );

        // A visitor that says stop ends the peel with false.
        let mut stopped = Packet::new();
        stopped.grow(HEADER_SIZE + 4 + wire.len());
        stopped.buffer_mut()[HEADER_SIZE + 4..HEADER_SIZE + 4 + wire.len()].copy_from_slice(&wire);
        assert!(!stopped.process_piggyback_packets(|_inner| false));

        // A length reaching past the whole body is refused.
        let mut greedy = Packet::new();
        greedy.grow(HEADER_SIZE + 4);
        greedy.buffer_mut()[HEADER_SIZE..HEADER_SIZE + 2].copy_from_slice(&0x7FFFu16.to_le_bytes());
        assert!(!greedy.process_piggyback_packets(|_inner| true));
    }

    /// `shouldCreateAnonymous` is the three-flag conjunction.
    #[test]
    fn anonymous_creation_needs_the_three_flags() {
        let mut packet = Packet::new();
        packet.set_flags(f::ON_CHANNEL | f::CREATE_CHANNEL);
        assert!(packet.should_create_anonymous());

        packet.enable_flags(f::INDEXED_CHANNEL);
        assert!(!packet.should_create_anonymous());

        packet.disable_flags(f::INDEXED_CHANNEL);
        packet.disable_flags(f::ON_CHANNEL);
        assert!(!packet.should_create_anonymous());

        let fresh = Packet::new();
        assert_eq!(fresh.seq(), crate::misc::SEQ_NULL);
        assert_eq!(fresh.channel_id(), crate::misc::CHANNEL_ID_NULL);
        assert_eq!(fresh.frag_begin(), crate::misc::SEQ_NULL);
        assert_eq!(fresh.frag_end(), crate::misc::SEQ_NULL);
        assert!(!fresh.is_piggyback());
    }
}
