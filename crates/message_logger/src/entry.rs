//! The log-entry record and its timestamp (`server/tools/message_logger`
//! `log_time.hpp/.cpp`, `log_entry.hpp`). `LogTime` is the fixed
//! `(seconds, milliseconds)` pair; `LogEntry` is the fixed-length portion of
//! an entry, the part that lands in the `entries` file.
//!
//! On disk the record is the C++ struct's natural-alignment image: the
//! writer does `*pEntries_ << entry`, a raw 48-byte `memcpy` of the struct
//! that includes the alignment holes after `time_`, `categoryID_`,
//! `messagePriority_` and `argsLen_`. The reference leaves those holes
//! holding uninitialized stack bytes; this port zeroes them (the only
//! deterministic choice), and the reader skips them, so records written by
//! the reference binary decode correctly and vice versa.

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

/// The fixed-length portion of a log entry (`log_entry.hpp`). The C++ struct
/// is **not** `#pragma pack`ed: fields sit at their natural alignment and
/// the holes between them travel with the record. Field offsets (x86-64,
/// confirmed from the reference binary's debug info and the original sample):
///
/// ```text
///  0..8   time_.secs_        i64
///  8..10  time_.msecs_       u16
///  10..12 (alignment hole)
/// 12..16  componentID_       u32
/// 16..18  categoryID_        u16
/// 18..20  (alignment hole)
/// 20..24  messageSource_     i32 (DebugMessageSource)
/// 24      messagePriority_   u8
/// 25..28  (alignment hole)
/// 28..32  stringOffset_      u32
/// 32..36  argsOffset_        u32
/// 36..38  argsLen_           u16
/// 38..40  (alignment hole)
/// 40..44  metadataOffset_    u32
/// 44..46  metadataLen_       u16
/// 46..48  (trailing padding)
/// ```
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
    /// The on-disk record size: the struct's aligned size (`sizeof`).
    pub const DISK_SIZE: usize = 48;

    /// The packed record size the user segment files carry:
    /// 10 + 4 + 2 + 1 + 1 + 4 + 4 + 2 + 4 + 2.
    pub const PACKED_SIZE: usize = 34;

    /// The little-endian 48-byte struct image the entries file carries.
    /// Holes and trailing padding are zeroed — the reference leaves them
    /// uninitialized, so the reader must never give them meaning.
    pub fn to_disk_bytes(&self) -> [u8; Self::DISK_SIZE] {
        let mut out = [0u8; Self::DISK_SIZE];
        out[0..8].copy_from_slice(&self.time.secs.to_le_bytes());
        out[8..10].copy_from_slice(&self.time.msecs.to_le_bytes());
        out[12..16].copy_from_slice(&self.user_component_id.to_le_bytes());
        out[16..18].copy_from_slice(&self.category_id.to_le_bytes());
        out[20..24].copy_from_slice(&(self.message_source.to_byte() as i32).to_le_bytes());
        out[24] = self.message_priority;
        out[28..32].copy_from_slice(&self.string_offset.to_le_bytes());
        out[32..36].copy_from_slice(&self.args_offset.to_le_bytes());
        out[36..38].copy_from_slice(&self.args_len.to_le_bytes());
        out[40..44].copy_from_slice(&self.metadata_offset.to_le_bytes());
        out[44..46].copy_from_slice(&self.metadata_len.to_le_bytes());
        out
    }

    /// Decode one 48-byte record. Holes are ignored (they hold garbage in
    /// reference-written files).
    pub fn from_disk_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::DISK_SIZE {
            return None;
        }
        let rd_i64 = |r: std::ops::Range<usize>| i64::from_le_bytes(bytes[r].try_into().unwrap());
        let rd_u16 = |r: std::ops::Range<usize>| u16::from_le_bytes(bytes[r].try_into().unwrap());
        let rd_u32 = |r: std::ops::Range<usize>| u32::from_le_bytes(bytes[r].try_into().unwrap());
        Some(Self {
            time: LogTime {
                secs: rd_i64(0..8),
                msecs: rd_u16(8..10),
            },
            user_component_id: rd_u32(12..16),
            category_id: rd_u16(16..18),
            message_source: MessageSource::from_byte(bytes[20]),
            message_priority: bytes[24],
            string_offset: rd_u32(28..32),
            args_offset: rd_u32(32..36),
            args_len: rd_u16(36..38),
            metadata_offset: rd_u32(40..44),
            metadata_len: rd_u16(44..46),
        })
    }

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

    /// Record the format-arguments blob placement
    /// (`UserSegmentWriter::addEntry`, `user_segment_writer.cpp:74-87`).
    pub fn set_args(&mut self, offset: u32, len: u16) {
        self.args_offset = offset;
        self.args_len = len;
    }

    /// Record the metadata blob placement
    /// (`user_segment_writer.cpp:124`).
    pub fn set_metadata(&mut self, offset: u32, len: u16) {
        self.metadata_offset = offset;
        self.metadata_len = len;
    }

    /// The 34-byte on-disk record the segment files hold — little-endian
    /// scalars in the packed field order.
    pub fn to_bytes(&self) -> [u8; Self::PACKED_SIZE] {
        let mut out = [0u8; Self::PACKED_SIZE];
        out[0..8].copy_from_slice(&self.time.secs.to_le_bytes());
        out[8..10].copy_from_slice(&self.time.msecs.to_le_bytes());
        out[10..14].copy_from_slice(&self.user_component_id.to_le_bytes());
        out[14..16].copy_from_slice(&self.category_id.to_le_bytes());
        out[16] = self.message_source.to_byte();
        out[17] = self.message_priority;
        out[18..22].copy_from_slice(&self.string_offset.to_le_bytes());
        out[22..26].copy_from_slice(&self.args_offset.to_le_bytes());
        out[26..28].copy_from_slice(&self.args_len.to_le_bytes());
        out[28..32].copy_from_slice(&self.metadata_offset.to_le_bytes());
        out[32..34].copy_from_slice(&self.metadata_len.to_le_bytes());
        out
    }

    /// Parse a 34-byte record — the read face for original samples.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::PACKED_SIZE {
            return None;
        }
        let read_u16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let read_u32 = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        Some(Self {
            time: LogTime {
                secs: i64::from_le_bytes(bytes[0..8].try_into().ok()?),
                msecs: read_u16(8),
            },
            user_component_id: read_u32(10),
            category_id: read_u16(14),
            message_source: MessageSource::from_byte(bytes[16]),
            message_priority: bytes[17],
            string_offset: read_u32(18),
            args_offset: read_u32(22),
            args_len: read_u16(26),
            metadata_offset: read_u32(28),
            metadata_len: read_u16(32),
        })
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
        // The aligned record is 48 bytes; keep the constant honest.
        assert_eq!(LogEntry::DISK_SIZE, 48);
    }

    /// The entries file of the reference-written sample user log
    /// (`message_logger` from BigWorld, four messages fed through its
    /// REGISTER/LOG protocol). Records carry the struct's alignment holes
    /// with uninitialized bytes (e.g. `e0 7f`, `43 e8`, `ce 69 31`) — the
    /// exact stack garbage the reference happened to hold.
    const SAMPLE_ENTRIES: [u8; 192] = [
        0x9e, 0x3b, 0x04, 0x6b, 0x00, 0x00, 0x00, 0x00, 0x28, 0x03, 0xe0, 0x7f, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x43, 0xe8, 0x00, 0x00, 0x00, 0x00, 0x02, 0xce, 0x69, 0x31, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x24,
        0x00, 0x00, 0x00, 0x9e, 0x3b, 0x04, 0x6b, 0x00, 0x00, 0x00, 0x00, 0x29, 0x03, 0xe0, 0x7f,
        0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x43, 0xe8, 0x01, 0x00, 0x00, 0x00, 0x04, 0xce, 0x69,
        0x31, 0x28, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x24, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9e, 0x3b, 0x04, 0x6b, 0x00, 0x00, 0x00, 0x00, 0x29,
        0x03, 0xe0, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x43, 0xe8, 0x00, 0x00, 0x00, 0x00,
        0x02, 0xce, 0x69, 0x31, 0x52, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00,
        0x00, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9e, 0x3b, 0x04, 0x6b, 0x00, 0x00,
        0x00, 0x00, 0x29, 0x03, 0xe0, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x43, 0xe8, 0x00,
        0x00, 0x00, 0x00, 0x03, 0xce, 0x69, 0x31, 0x79, 0x00, 0x00, 0x00, 0x12, 0x00, 0x00, 0x00,
        0x10, 0x00, 0x00, 0x00, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn disk_roundtrip_preserves_all_fields() {
        let entry = LogEntry::new(1795550878, 808, 0, 1, MessageSource::Cpp, 2, 0);
        let bytes = entry.to_disk_bytes();
        assert_eq!(bytes.len(), 48);
        // Holes and padding stay zero.
        for hole in [10..12, 18..20, 25..28, 38..40, 46..48] {
            assert!(bytes[hole].iter().all(|&b| b == 0), "hole not zeroed");
        }
        let back = LogEntry::from_disk_bytes(&bytes).unwrap();
        assert_eq!(back.time, entry.time);
        assert_eq!(back.user_component_id, entry.user_component_id);
        assert_eq!(back.category_id, entry.category_id);
        assert_eq!(back.message_source, entry.message_source);
        assert_eq!(back.message_priority, entry.message_priority);
        assert_eq!(back.string_offset, entry.string_offset);
        assert_eq!(back.args_offset, entry.args_offset);
        assert_eq!(back.args_len, entry.args_len);
        assert_eq!(back.metadata_offset, entry.metadata_offset);
        assert_eq!(back.metadata_len, entry.metadata_len);
        // Short input is refused.
        assert!(LogEntry::from_disk_bytes(&bytes[..47]).is_none());
        assert!(LogEntry::from_disk_bytes(&[]).is_none());
    }

    #[test]
    fn reads_the_reference_sample_entries_file() {
        for (i, record) in SAMPLE_ENTRIES.chunks(LogEntry::DISK_SIZE).enumerate() {
            let entry =
                LogEntry::from_disk_bytes(record).unwrap_or_else(|| panic!("record {i} decode"));
            assert_eq!(entry.time.secs, 0x6b043b9e, "record {i} secs");
            assert_eq!(entry.user_component_id, 0, "record {i} componentID");
            assert_eq!(
                entry.args_offset + entry.args_len as u32,
                [4, 10, 18, 34][i],
                "record {i} args extent"
            );
            // Only the first record carried metadata.
            assert_eq!(
                (entry.metadata_offset, entry.metadata_len as u32),
                [(0, 36), (36, 0), (36, 0), (36, 0)][i],
                "record {i} metadata extent"
            );
        }

        let first = LogEntry::from_disk_bytes(&SAMPLE_ENTRIES[..48]).unwrap();
        assert_eq!(first.time.msecs, 808);
        assert_eq!(first.category_id, 1); // "combat"
        assert_eq!(first.message_source, MessageSource::Cpp);
        assert_eq!(first.message_priority, 2); // INFO
        assert_eq!(first.string_offset, 0);
        assert_eq!(first.args_len, 4);
        assert_eq!(first.metadata_len, 36);

        let second = LogEntry::from_disk_bytes(&SAMPLE_ENTRIES[48..96]).unwrap();
        assert_eq!(second.time.msecs, 809);
        assert_eq!(second.category_id, 2); // ""
        assert_eq!(second.message_source, MessageSource::Script);
        assert_eq!(second.message_priority, 4); // WARNING
        assert_eq!(second.string_offset, 40);
        assert_eq!(second.args_len, 6);
        assert_eq!(second.metadata_len, 0);

        let third = LogEntry::from_disk_bytes(&SAMPLE_ENTRIES[96..144]).unwrap();
        assert_eq!(third.message_priority, 2);
        assert_eq!(third.string_offset, 82);
        assert_eq!(third.args_len, 8);

        let fourth = LogEntry::from_disk_bytes(&SAMPLE_ENTRIES[144..]).unwrap();
        assert_eq!(fourth.message_priority, 3); // NOTICE
        assert_eq!(fourth.category_id, 1);
        assert_eq!(fourth.string_offset, 121);
        assert_eq!(fourth.args_len, 16);
    }

    #[test]
    fn rewriting_the_sample_matches_every_non_hole_byte() {
        // Re-encode the four decoded records; every field byte must land
        // back where it came from. Only the holes may differ (ours are
        // zeroed, the reference's hold stack garbage).
        let holes: &[usize] = &[10, 11, 18, 19, 25, 26, 27, 38, 39, 46, 47];
        for (i, record) in SAMPLE_ENTRIES.chunks(LogEntry::DISK_SIZE).enumerate() {
            let entry = LogEntry::from_disk_bytes(record).unwrap();
            let rewritten = entry.to_disk_bytes();
            for (off, &byte) in rewritten.iter().enumerate() {
                if holes.contains(&off) {
                    assert_eq!(byte, 0, "record {i} hole byte {off} not zeroed");
                } else {
                    assert_eq!(byte, record[off], "record {i} byte {off} diverged");
                }
            }
        }
    }
}
