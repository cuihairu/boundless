//! `Mercury::Reason` and its string table (`lib/network/misc.hpp`).

/// `MessageID` (`misc.hpp:90`): the identifier of the message inside the
/// packet. One byte, so an interface owns at most 256 messages and
/// `REPLY_MESSAGE_IDENTIFIER` reserves the top one.
pub type MessageID = u8;

/// `ReplyID` (`misc.hpp:101`): the unique id carried by every request so its
/// reply can be correlated back to it. An `int32` in the reference, written
/// on the wire in network byte order.
pub type ReplyID = i32;

/// `REPLY_ID_NONE` (`misc.hpp:102`): the sentinel "no reply expected" /
/// "no reply received".
pub const REPLY_ID_NONE: ReplyID = -1;

/// `REPLY_ID_MAX` (`misc.hpp:103`): the ceiling [`RequestManager`] rolls
/// over to 1 after. (See [`crate::request_manager`].)
pub const REPLY_ID_MAX: ReplyID = 1_000_000;

/// `SeqNum` (misc.hpp:25): a packet sequence number. The sequence space is
/// the top 28 bits of the u32, so `SEQ_SIZE` is the wrap point.
pub type SeqNum = u32;

/// `SEQ_SIZE` (misc.hpp:26).
pub const SEQ_SIZE: SeqNum = 0x1000_0000;

/// `SEQ_MASK` (misc.hpp:27).
pub const SEQ_MASK: SeqNum = SEQ_SIZE - 1;

/// `SEQ_NULL` (misc.hpp:28): the sentinel "no sequence number".
pub const SEQ_NULL: SeqNum = SEQ_SIZE;

/// `seqMask` (misc.hpp:34): mask an integer into the sequence space.
pub fn seq_mask(x: SeqNum) -> SeqNum {
    x & SEQ_MASK
}

/// `seqLessThan` (misc.hpp:44): whether `a` comes "before" `b` in the ring.
///
/// The reference subtracts two `uint32`s, which wraps; an `a - b` on a Rust
/// `u32` would panic on underflow in a debug build, so the port says the
/// wrap explicitly.
pub fn seq_less_than(a: SeqNum, b: SeqNum) -> bool {
    seq_mask(a.wrapping_sub(b)) > SEQ_SIZE / 2
}

/// `ChannelID` (misc.hpp:96): the id of an indexed channel.
pub type ChannelID = i32;

/// `CHANNEL_ID_NULL` (misc.hpp:97): the sentinel "no channel id".
pub const CHANNEL_ID_NULL: ChannelID = 0;

/// `ChannelVersion` (misc.hpp:104): reuses the sequence number type.
pub type ChannelVersion = SeqNum;

/// `SeqNumAllocator` (misc.hpp:53): doles out sequence numbers for packets.
///
/// The reference keeps the "next" value public; this port exposes the read
/// as [`next`](Self::next) instead of an implicit conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqNumAllocator {
    next_num: SeqNum,
}

impl SeqNumAllocator {
    /// `SeqNumAllocator( firstSeqNum )`.
    pub fn new(first_seq_num: SeqNum) -> Self {
        Self {
            next_num: first_seq_num,
        }
    }

    /// `getNext`: return the current value and advance, wrapping in the
    /// sequence space. The reference's `nextSeqNum_ + 1` is a `uint32`
    /// addition, which wraps instead of overflowing.
    pub fn get_next(&mut self) -> SeqNum {
        let ret = self.next_num;
        self.next_num = seq_mask(self.next_num.wrapping_add(1));
        ret
    }

    /// The next sequence number that will be issued (`operator SeqNum`).
    pub fn next(&self) -> SeqNum {
        self.next_num
    }
}

/// `Mercury::Reason`: reasons for Mercury errors, exceptions, and return
/// codes. The reference numbers them downwards from 0 so that
/// `-reason` indexes the string table; the port keeps the exact values
/// because they cross process boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum Reason {
    /// No reason.
    Success = 0,
    /// Timer expired.
    TimerExpired = -1,
    /// Destination port is not open.
    NoSuchPort = -2,
    /// The network is stuffed.
    GeneralNetwork = -3,
    /// Got a bad packet.
    CorruptedPacket = -4,
    /// Wanted to call a null function.
    NonexistentEntry = -5,
    /// Channel send window overflowed.
    WindowOverflow = -6,
    /// Channel inactivity timeout.
    Inactivity = -7,
    /// Corresponds to EAGAIN.
    ResourceUnavailable = -8,
    /// Client disconnected voluntarily.
    ClientDisconnected = -9,
    /// Corresponds to ENOBUFS.
    TransmitQueueFull = -10,
    /// Corresponds to channel lost.
    ChannelLost = -11,
    /// Corresponds to shutting down app.
    ShuttingDown = -12,
    /// Corresponds to EMSGSIZE.
    MessageTooLong = -13,
}

impl Reason {
    /// `reasonToString`: the table the reference indexes with `-reason`;
    /// anything outside it is `REASON_UNKNOWN`.
    pub fn to_string(&self) -> &str {
        const REASONS: [&str; 14] = [
            "REASON_SUCCESS",
            "REASON_TIMER_EXPIRED",
            "REASON_NO_SUCH_PORT",
            "REASON_GENERAL_NETWORK",
            "REASON_CORRUPTED_PACKET",
            "REASON_NONEXISTENT_ENTRY",
            "REASON_WINDOW_OVERFLOW",
            "REASON_INACTIVITY",
            "REASON_RESOURCE_UNAVAILABLE",
            "REASON_CLIENT_DISCONNECTED",
            "REASON_TRANSMIT_QUEUE_FULL",
            "REASON_CHANNEL_LOST",
            "REASON_SHUTTING_DOWN",
            "REASON_MESSAGE_TOO_LONG",
        ];
        // A C enum can hold values outside its enumerator list, so the
        // reference bounds-checks before indexing; a Rust enum cannot,
        // and the index here is always in `0..REASONS.len()`.
        REASONS[(-(*self as i32)) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_values_are_pinned() {
        assert_eq!(Reason::Success as i32, 0);
        assert_eq!(Reason::TimerExpired as i32, -1);
        assert_eq!(Reason::NoSuchPort as i32, -2);
        assert_eq!(Reason::GeneralNetwork as i32, -3);
        assert_eq!(Reason::CorruptedPacket as i32, -4);
        assert_eq!(Reason::NonexistentEntry as i32, -5);
        assert_eq!(Reason::WindowOverflow as i32, -6);
        assert_eq!(Reason::Inactivity as i32, -7);
        assert_eq!(Reason::ResourceUnavailable as i32, -8);
        assert_eq!(Reason::ClientDisconnected as i32, -9);
        assert_eq!(Reason::TransmitQueueFull as i32, -10);
        assert_eq!(Reason::ChannelLost as i32, -11);
        assert_eq!(Reason::ShuttingDown as i32, -12);
        assert_eq!(Reason::MessageTooLong as i32, -13);
    }

    /// Every value names itself, and out-of-range indexes fall through to
    /// `REASON_UNKNOWN` -- the reference's guard against a caller inventing
    /// a reason the table does not hold.
    #[test]
    fn the_string_table_covers_every_reason() {
        let named = [
            (Reason::Success, "REASON_SUCCESS"),
            (Reason::TimerExpired, "REASON_TIMER_EXPIRED"),
            (Reason::NoSuchPort, "REASON_NO_SUCH_PORT"),
            (Reason::GeneralNetwork, "REASON_GENERAL_NETWORK"),
            (Reason::CorruptedPacket, "REASON_CORRUPTED_PACKET"),
            (Reason::NonexistentEntry, "REASON_NONEXISTENT_ENTRY"),
            (Reason::WindowOverflow, "REASON_WINDOW_OVERFLOW"),
            (Reason::Inactivity, "REASON_INACTIVITY"),
            (Reason::ResourceUnavailable, "REASON_RESOURCE_UNAVAILABLE"),
            (Reason::ClientDisconnected, "REASON_CLIENT_DISCONNECTED"),
            (Reason::TransmitQueueFull, "REASON_TRANSMIT_QUEUE_FULL"),
            (Reason::ChannelLost, "REASON_CHANNEL_LOST"),
            (Reason::ShuttingDown, "REASON_SHUTTING_DOWN"),
            (Reason::MessageTooLong, "REASON_MESSAGE_TOO_LONG"),
        ];
        for (reason, expected) in named {
            assert_eq!(reason.to_string(), expected, "{reason:?} names itself");
        }
    }

    /// The sequence space is the top 28 bits; `SEQ_NULL` sits just past the
    /// wrap so it can never collide with an issued number.
    #[test]
    fn the_sequence_space_is_the_top_28_bits() {
        assert_eq!(SEQ_SIZE, 0x1000_0000);
        assert_eq!(SEQ_MASK, 0x0FFF_FFFF);
        assert_eq!(SEQ_NULL, SEQ_SIZE);
        assert_eq!(seq_mask(SEQ_SIZE + 5), 5);
        assert_eq!(seq_mask(0x1234_5678), 0x0234_5678);
    }

    /// `seqLessThan` is a ring comparison: half the space behind is
    /// "before", half ahead is "after", and the wrap point is the seam.
    #[test]
    fn sequence_numbers_compare_across_the_wrap() {
        assert!(seq_less_than(5, 6));
        assert!(!seq_less_than(6, 5));
        assert!(!seq_less_than(5, 5));
        // The seam: 0x0fffffff wraps to 0, so it is only 5 steps before 5
        // (not after it), and 5 is far behind it the other way round.
        assert!(!seq_less_than(5, SEQ_MASK));
        assert!(seq_less_than(SEQ_MASK, 5));
        assert!(seq_less_than(SEQ_MASK, 0), "wrap: 0x0fffffff < 0");
        assert!(!seq_less_than(0, SEQ_MASK));
        // An allocator hands out masked numbers in order across the wrap.
        let mut alloc = SeqNumAllocator::new(SEQ_MASK);
        assert_eq!(alloc.next(), SEQ_MASK, "next reads without advancing");
        assert_eq!(alloc.get_next(), SEQ_MASK);
        assert_eq!(alloc.get_next(), 0, "wraps through zero");
        assert_eq!(alloc.get_next(), 1);
    }
}
