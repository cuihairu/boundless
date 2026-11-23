//! The log-entry record and its timestamp (`server/tools/message_logger`
//! `log_time.hpp/.cpp`, `log_entry.hpp`). `LogTime` is the fixed
//! `(seconds, milliseconds)` pair; `LogEntry` is the fixed-length portion of
//! an entry, the part that lands in the `entries` file.

use crate::MessageSource;

/// `LOG_BEGIN`/`LOG_END` sentinels (`constants.hpp:21-22`).
pub const LOG_BEGIN: f64 = 0.0;
/// `LOG_END`: queries using it run to positive infinity.
pub const LOG_END: f64 = -1.0;

/// `LogTime` (`log_time.hpp`): seconds plus milliseconds, ordered
/// lexicographically.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LogTime {
    /// `Seconds` — an `int64`.
    pub secs: i64,
    /// `Milliseconds` — a `uint16`.
    pub msecs: u16,
}

impl LogTime {
    /// `LogTime( ftime )`: `LOG_END` becomes positive infinity
    /// (`log_time.cpp:20-33`).
    pub fn from_double(ftime: f64) -> Self {
        if ftime == LOG_END {
            Self {
                secs: i64::MAX,
                msecs: 0,
            }
        } else {
            let secs = ftime as i64;
            Self {
                secs,
                msecs: ((ftime - secs as f64) * 1000.0 + 0.5) as u16,
            }
        }
    }

    /// `asDouble()`.
    pub fn as_double(&self) -> f64 {
        self.secs as f64 + self.msecs as f64 / 1000.0
    }
}

impl PartialOrd for LogTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LogTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.secs, self.msecs).cmp(&(other.secs, other.msecs))
    }
}

/// The fixed-length portion of a log entry (`log_entry.hpp`), packed (one
/// `#pragma pack(1)` struct) in this field order:
/// time (10 bytes), userComponentID (4), categoryID (2), messageSource (1),
/// messagePriority (1), stringOffset (4), argsOffset (4), argsLen (2),
/// metadataOffset (4), metadataLen (2).
#[derive(Clone, Debug, Default)]
pub struct LogEntry {
    /// When the message was logged.
    pub time: LogTime,
    /// `UserComponentID` — the sending process instance in the user log.
    pub user_component_id: u32,
    /// `CategoryID` — index into the user log's category table.
    pub category_id: u16,
    /// `DebugMessageSource` — C++ or script call site.
    pub message_source: MessageSource,
    /// `NetworkMessagePriority` — the raw `DebugMessagePriority` byte.
    pub message_priority: u8,
    /// `FormatStringOffsetId` — where the format string lives.
    pub string_offset: u32,
    /// Offset of the format-arguments blob in the `args` file.
    pub args_offset: u32,
    /// Length of the format-arguments blob.
    pub args_len: u16,
    /// Offset of the metadata blob.
    pub metadata_offset: u32,
    /// Length of the metadata blob.
    pub metadata_len: u16,
}

impl LogEntry {
    /// The packed record size: 10 + 4 + 2 + 1 + 1 + 4 + 4 + 2 + 4 + 2.
    pub const PACKED_SIZE: usize = 34;

    /// Build the fixed-length record the way `UserSegmentWriter` does,
    /// taking the timestamp split already millisecond-rounded.
    pub fn new(
        secs: i64,
        msecs: u16,
        user_component_id: u32,
        category_id: u16,
        message_source: MessageSource,
        message_priority: u8,
        string_offset: u32,
    ) -> Self {
        Self {
            time: LogTime { secs, msecs },
            user_component_id,
            category_id,
            message_source,
            message_priority,
            string_offset,
            args_offset: 0,
            args_len: 0,
            metadata_offset: 0,
            metadata_len: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_time_orders_lexicographically() {
        assert!(LogTime { secs: 1, msecs: 0 } < LogTime { secs: 1, msecs: 1 });
        assert!(
            LogTime {
                secs: 1,
                msecs: 999
            } < LogTime { secs: 2, msecs: 0 }
        );
        assert_eq!(
            LogTime {
                secs: 5,
                msecs: 250
            },
            LogTime {
                secs: 5,
                msecs: 250
            }
        );
    }

    #[test]
    fn log_time_converts_from_doubles() {
        let time = LogTime::from_double(1234.5678);
        assert_eq!(time.secs, 1234);
        // 0.5678 * 1000 + 0.5 rounds to 568.
        assert_eq!(time.msecs, 568);
        assert!((time.as_double() - 1234.568).abs() < 1e-9);

        // LOG_END is positive infinity in the time domain.
        let end = LogTime::from_double(LOG_END);
        assert_eq!(end.secs, i64::MAX);
        assert_eq!(end.msecs, 0);
        assert!(
            LogTime {
                secs: 9999,
                msecs: 0
            } < end
        );

        // LOG_BEGIN is just zero.
        assert_eq!(
            LogTime::from_double(LOG_BEGIN),
            LogTime { secs: 0, msecs: 0 }
        );
    }

    #[test]
    fn entries_carry_the_fixed_fields() {
        let entry = LogEntry::new(100, 250, 7, 3, MessageSource::Script, 2, 4096);
        assert_eq!(entry.time.secs, 100);
        assert_eq!(entry.time.msecs, 250);
        assert_eq!(entry.user_component_id, 7);
        assert_eq!(entry.category_id, 3);
        assert_eq!(entry.message_source, MessageSource::Script);
        assert_eq!(entry.message_priority, 2);
        assert_eq!(entry.string_offset, 4096);
        assert_eq!(LogEntry::default().args_len, 0);
        // The packed record is 34 bytes; keep the constant honest.
        assert_eq!(LogEntry::PACKED_SIZE, 34);
    }
}
