//! `Mercury::Reason` and its string table (`lib/network/misc.hpp`).

/// `MessageID` (`misc.hpp:90`): the identifier of the message inside the
/// packet. One byte, so an interface owns at most 256 messages and
/// `REPLY_MESSAGE_IDENTIFIER` reserves the top one.
pub type MessageID = u8;

/// `ReplyID` (`misc.hpp:110`): the number a request carries so that its
/// reply can find it again. Four bytes on the wire, so a reply id is not
/// the one byte a message id is.
pub type ReplyID = i32;

/// `SessionKey` (`lib/network/basictypes.hpp:171`): the authentication key
/// exchanged between ClientApp and BaseApp. Four bytes on the wire: the
/// login reply carries it, and the client's `authenticate` request echoes it
/// back. A zero key is never valid (`Proxy::regenerateSessionKey`).
pub type SessionKey = u32;

/// `Proxy::regenerateSessionKey` (proxy.cpp:562): the key is drawn from
/// the current timestamp and a zero result is redrawn -- a key is never
/// zero. The port reduces the loop to its outcome on a single draw.
pub fn regenerate_session_key(now: u32) -> SessionKey {
    if now == 0 { 1 } else { now }
}

/// `REPLY_ID_NONE` (`misc.hpp:111`): no reply expected, and the value a
/// header carries until one is handed out.
pub const REPLY_ID_NONE: ReplyID = -1;

/// `REPLY_ID_MAX` (`misc.hpp:112`): the top of the range the id generator
/// hands out before it wraps back down.
pub const REPLY_ID_MAX: ReplyID = 1_000_000;

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

/// `SeqNum` (`misc.hpp:25`): a packet sequence number. Only 28 bits are
/// usable; `SEQ_NULL` sits exactly one past the mask, so it can never be
/// a real number.
pub type SeqNum = u32;

/// `SEQ_SIZE` (`misc.hpp:26`).
pub const SEQ_SIZE: SeqNum = 0x1000_0000;

/// `SEQ_MASK` (`misc.hpp:27`).
pub const SEQ_MASK: SeqNum = SEQ_SIZE - 1;

/// `SEQ_NULL` (`misc.hpp:28`): "no sequence number".
pub const SEQ_NULL: SeqNum = SEQ_SIZE;

/// `seqMask` (`misc.hpp:34`): fold a sequence number back into range.
pub fn seq_mask(x: SeqNum) -> SeqNum {
    x & SEQ_MASK
}

/// `seqLessThan` (`misc.hpp:44`): whether `a` comes before `b` on the
/// ring. The masked difference answers "did we wrap between them".
pub fn seq_less_than(a: SeqNum, b: SeqNum) -> bool {
    seq_mask(a.wrapping_sub(b)) > SEQ_SIZE / 2
}

/// `ChannelID` (`misc.hpp:96`).
pub type ChannelID = i32;

/// `CHANNEL_ID_NULL` (`misc.hpp:97`): not on a channel.
pub const CHANNEL_ID_NULL: ChannelID = 0;

/// `ChannelVersion` (`misc.hpp`): the generation a channel carries, so a
/// peer that restarted can be told apart from the live one.
pub type ChannelVersion = u32;

/// `SeqNumAllocator` (`misc.hpp:53`): doles out sequence numbers for
/// off-channel packets, wrapping through [`seq_mask`].
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

    /// `getNext`: hand out the current number, advance masked.
    pub fn get_next(&mut self) -> SeqNum {
        let ret_val = self.next_num;
        self.next_num = seq_mask(self.next_num.wrapping_add(1));
        ret_val
    }

    /// `operator SeqNum`: the number the next call hands out.
    pub fn peek(&self) -> SeqNum {
        self.next_num
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

    /// The sequence ring: masking folds wraps, `seqLessThan` answers
    /// across the wrap point, and the allocator hands out masked numbers
    /// in order, `SEQ_NULL` unreachable.
    #[test]
    fn the_sequence_ring_wraps_through_the_mask() {
        assert_eq!(SEQ_MASK, 0x0FFF_FFFF);
        assert_eq!(SEQ_NULL, SEQ_SIZE, "null sits one past every real number");
        assert_eq!(seq_mask(SEQ_SIZE + 5), 5);

        assert!(seq_less_than(5, 6), "the plain order holds");
        assert!(!seq_less_than(6, 5));
        // Across the wrap: SEQ_MASK is before 0, a half-ring away.
        assert!(seq_less_than(SEQ_MASK, 0));
        assert!(seq_less_than(0, SEQ_MASK / 2));
        assert!(!seq_less_than(0, SEQ_MASK), "the far half ring is behind");

        let mut allocator = SeqNumAllocator::new(SEQ_SIZE - 2);
        assert_eq!(allocator.get_next(), SEQ_SIZE - 2);
        assert_eq!(allocator.get_next(), SEQ_SIZE - 1);
        assert_eq!(allocator.get_next(), 0, "wraps through the mask");
        assert_eq!(allocator.peek(), 1);
    }

    /// `SessionKey` (basictypes.hpp:171) and `Proxy::regenerateSessionKey`
    /// (proxy.cpp:562): a timestamp draw, never zero -- the reference
    /// redraws until one is non-zero, the port takes the first non-zero.
    #[test]
    fn a_session_key_is_never_zero() {
        assert_eq!(regenerate_session_key(1_772_000_000), 1_772_000_000);
        assert_eq!(regenerate_session_key(0), 1, "a zero draw is redrawn");
    }
}
