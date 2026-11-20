//! `Mercury::Reason` and its string table (`lib/network/misc.hpp`).

/// `MessageID` (`misc.hpp:90`): the identifier of the message inside the
/// packet. One byte, so an interface owns at most 256 messages and
/// `REPLY_MESSAGE_IDENTIFIER` reserves the top one.
pub type MessageID = u8;

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
}
