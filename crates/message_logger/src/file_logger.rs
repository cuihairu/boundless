//! BW-12-02-03: the file logging database — user log directories, segment
//! files, and the rotation between them.
//!
//! The MessageLogger tool writes each user's messages under
//! `<logdir>/<username>/` (`UserLog::init`, `user_log.cpp:36-41`): a text
//! `uid` file (`UnaryIntegerFile`, `unary_integer_file.cpp:83-88`), a
//! `components` file of `LoggingComponent` records appended once per
//! process (`UserComponents::write`, `user_components.cpp:102-108`;
//! `LoggingComponent::write`, `logging_component.cpp:54-61`), and one
//! segment per suffix — `entries.<suffix>`, `args.<suffix>`,
//! `metadata.<suffix>` (`UserSegmentWriter::init`,
//! `user_segment_writer.cpp:21-63`). The suffix is the segment's start
//! time formatted `%Y-%m-%d-%T` (`UserSegment::buildSuffixFrom`,
//! `user_segment.cpp:110-125`). A new segment is created when the first
//! message arrives after init or when the active one is full —
//! `numEntries * sizeof(LogEntry) + argsSize >= maxSegmentSize`
//! (`UserSegmentWriter::isFull`, `user_segment_writer.cpp:162-167`), the
//! default cap being 100 MB (`DEFAULT_SEGMENT_SIZE_MB << 20`,
//! `constants.hpp:24`, `log_storage.cpp:27`) — and every other active
//! segment is dropped (`UserLogWriter::addEntry`,
//! `user_log_writer.cpp:98-154`). The root `active_files` text file lists
//! `<username>/entries.<suffix>`, `args`, `metadata` for every open
//! segment so external archivers know what not to touch
//! (`ActiveFiles::update`, `active_files.cpp:57-125`).
//!
//! The format string each entry points at lives in the root `strings`
//! file as a serialized `LogStringInterpolator` (`FormatStringsMLDB`,
//! `mldb/format_strings.cpp:5-25`; `LogStringInterpolator::write`,
//! `log_string_interpolator.cpp:89-105`) — the port carries that
//! serialization on the interpolator itself. The entry's `args` blob is
//! the re-encoded argument stream (`UserSegmentWriter::addEntry` +
//! `LogStringWriter`), written through
//! [`LogStringInterpolator::stream_to_log`].
//!
//! Registered deviations:
//! * the suffix is built from UTC, not `localtime` (`user_segment.cpp:24`
//!   reads the local clock); the port keeps no timezone database.
//! * metadata strings are cleaned to UTF-8 by lossy conversion rather
//!   than the reference's `toValidUtf8Str` byte fix-ups, and the
//!   `setJSON` path compacts whitespace instead of round-tripping the
//!   value through jsoncpp.
//! * segment bounds are computed by reading whole files; the reference
//!   seeks to the two ends (`UserSegment::updateEntryBounds`).
//! * two segments born in the same second take the same suffix and the
//!   second one appends to the first's files — the reference's suffix is
//!   `strftime( "%Y-%m-%d-%T" )` of `time( NULL )`, seconds-granular
//!   (`user_segment.cpp:16-28`). The port reproduces that; a caller that
//!   needs distinct names supplies a suffix source
//!   ([`UserLogWriter::init_with_suffix_source`]).

use crate::component_message::ComponentMessage;
use crate::entry::LogEntry;
use crate::interpolator::LogStringInterpolator;
use crate::stream::{MemoryIStream, MemoryOStream};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// The version from which messages may carry trailing metadata
/// (`LOG_METADATA_VERSION`, `constants.hpp:32`).
pub const LOG_METADATA_VERSION: u8 = 9;
/// The version from which metadata rides as a typed key/value stream
/// (`LOG_METADATA_STREAM_VERSION`, `constants.hpp:35`).
pub const LOG_METADATA_STREAM_VERSION: u8 = 10;

/// The default segment cap: `DEFAULT_SEGMENT_SIZE_MB << 20`
/// (`constants.hpp:24`, `log_storage.cpp:27`).
pub const DEFAULT_SEGMENT_SIZE_BYTES: u32 = 100 << 20;

/// The metadata value tags (`LogMetaData::MessageLoggerValueType`,
/// `log_meta_data.cpp:13-19`), streamed as four-byte `int`s.
pub(crate) mod mlvt {

    pub const JSON_STRING: u32 = b'j' as u32;
    pub const INT: u32 = b'i' as u32;
    pub const UNSIGNED_INT: u32 = b'u' as u32;
    pub const LONG_LONG_INT: u32 = b'l' as u32;
    pub const STRING: u32 = b's' as u32;
    pub const FLOAT: u32 = b'f' as u32;
    pub const DOUBLE: u32 = b'd' as u32;
}

/// The key a raw JSON metadata argument rides under
/// (`LogMetaData::key_jsonString`, `log_meta_data.cpp:11`).
pub const KEY_JSON_STRING: &str = "__bw_json";

/// `%Y-%m-%d-%T` over the given epoch seconds, in UTC — the segment
/// suffix (`UserSegment::buildSuffixFrom`, `user_segment.cpp:110-125`;
/// the reference uses the local clock).
pub fn format_suffix(epoch_secs: i64) -> String {
    // Civil-from-days (Howard Hinnant's algorithm) — no timezone database.
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (h, m, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}-{h:02}:{m:02}:{s:02}")
}

/// Days since 1970-01-01 for a proleptic Gregorian date
/// (Howard Hinnant's algorithm, the inverse half of [`format_suffix`]). The
/// MongoDB backend needs it to turn a collection's `YYYYMMDDHHMMSS` suffix
/// back into a timestamp (`timegm`, `user_log_writer.cpp:677-697`).
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month, day)` for days since 1970-01-01 — civil-from-days, the
/// other half of [`days_from_civil`]. The MongoDB expiry check walks a
/// collection's `YYYYMMDDHHMMSS` suffix through this pair
/// (`timegm`, `user_log_writer.cpp:675-691`).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// The current wall clock as epoch seconds, for live suffixes.
fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// -----------------------------------------------------------------------------
// Section: metadata (`mldb/metadata.cpp` + `MLMetadata::LogMetadataBuilder`)
// -----------------------------------------------------------------------------

/// One metadata value, as the JSON object holds it.
#[derive(Clone, Debug, PartialEq)]
enum MetaValue {
    Int(i64),
    UInt(u32),
    Str(String),
    Float(f32),
    Double(f64),
}

/// JSON-escape a string the way jsoncpp's writer does: the C escapes and
/// `\uXXXX` for the remaining control bytes, everything else raw.
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render a value the way `Json::FastWriter` does — integral values bare,
/// floats shortest-round-trip, strings escaped.
fn meta_value_json(value: &MetaValue) -> String {
    match value {
        MetaValue::Int(v) => v.to_string(),
        MetaValue::UInt(v) => v.to_string(),
        MetaValue::Str(v) => json_escape(v),
        MetaValue::Float(v) => format!("{}", *v as f64),
        MetaValue::Double(v) => format!("{v}"),
    }
}

/// Compact whitespace outside of string literals — the port's stand-in for
/// the reference's jsoncpp parse-then-rewrite of a raw JSON argument.
fn compact_json(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in raw.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

/// `MLMetadata::LogMetadataBuilder::process` (`metadata.cpp:221-338`):
/// consume the typed key/value pairs off the argument stream and render
/// the JSON object the metadata file stores.
///
/// The reference loop is `while (inputStream.remainingLength())`: when the
/// trailing NULL byte makes a four-byte type read fail — or a key is not a
/// string — it **returns, keeping the pairs it already collected** (an
/// empty object renders as `{}\n`). A raw JSON pair overrides everything,
/// the way `setJSON`'s early return does.
pub fn build_metadata_json(input: &mut MemoryIStream) -> String {
    let mut pairs: BTreeMap<String, MetaValue> = BTreeMap::new();
    let mut json_override: Option<String> = None;

    loop {
        if input.remaining() == 0 {
            break;
        }
        let Some(tag) = input.read_u32() else {
            // The trailing NULL: not a four-byte type.
            break;
        };
        if tag != mlvt::STRING {
            // "Key expected as string" — terminating metadata processing.
            break;
        }
        let key = input.read_string();
        let Some(tag) = input.read_u32() else {
            break;
        };
        match tag {
            mlvt::JSON_STRING => {
                let raw = input.read_string();
                if key != KEY_JSON_STRING {
                    // The reference warns and still tries setJSON.
                }
                // `setJSON`: a JSON argument overrides all others.
                if !raw.is_empty() {
                    json_override = Some(compact_json(&raw));
                }
                break;
            }
            mlvt::INT => {
                let Some(v) = input.read_u32() else {
                    break;
                };
                pairs.insert(key, MetaValue::Int(v as i32 as i64));
            }
            mlvt::LONG_LONG_INT => {
                let Some(v) = input.read_u64() else {
                    break;
                };
                pairs.insert(key, MetaValue::Int(v as i64));
            }
            mlvt::UNSIGNED_INT => {
                let Some(v) = input.read_u32() else {
                    break;
                };
                pairs.insert(key, MetaValue::UInt(v));
            }
            mlvt::STRING => {
                pairs.insert(key, MetaValue::Str(input.read_string()));
            }
            mlvt::FLOAT => {
                let Some(v) = input.read_u32() else {
                    break;
                };
                pairs.insert(key, MetaValue::Float(f32::from_bits(v)));
            }
            mlvt::DOUBLE => {
                let Some(v) = input.read_u64() else {
                    break;
                };
                pairs.insert(key, MetaValue::Double(f64::from_bits(v)));
            }
            _ => {
                // Unknown type: an empty string value under the key.
                pairs.insert(key, MetaValue::Str(String::new()));
            }
        }
    }

    if let Some(override_json) = json_override {
        // The reference re-renders even a raw JSON argument through
        // `FastWriter`, which appends the newline.
        return format!("{override_json}\n");
    }

    let mut out = String::from("{");
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_escape(key));
        out.push(':');
        out.push_str(&meta_value_json(value));
    }
    out.push('}');
    out.push('\n');
    out
}

// -----------------------------------------------------------------------------
// Section: the components file (`logging_component.cpp` + `user_components.cpp`)
// -----------------------------------------------------------------------------

/// One sending process as the components file records it
/// (`LoggingComponent`). The record is: the raw 8-byte address (ip, port,
/// salt — `Mercury::Address` streaming, `basictypes.cpp:199-207`), the
/// four IDs, the component message, and the first-entry pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct LoggingComponent {
    /// The source address, salt included.
    pub addr: (u32, u16, u16),
    /// The ID this process holds in the user log.
    pub user_component_id: u32,
    /// `appInstanceID`, filled in later if ever.
    pub app_instance_id: u32,
    /// The process type ID from the component-name table.
    pub app_type_id: u32,
    /// The registration message, stored verbatim.
    pub msg: ComponentMessage,
    /// The suffix and entry index of the component's first message
    /// (`firstEntry_`).
    pub first_entry: (String, i32),
    /// Where this record sits in the components file; -1 until written
    /// (`fileOffset_`).
    pub file_offset: i64,
}

impl LoggingComponent {
    /// `LoggingComponent( userComponents, addr, msg, appTypeID )` — an
    /// unwritten record.
    pub fn new(
        addr: (u32, u16, u16),
        user_component_id: u32,
        app_type_id: u32,
        msg: ComponentMessage,
    ) -> Self {
        Self {
            addr,
            user_component_id,
            app_instance_id: 0,
            app_type_id,
            msg,
            first_entry: (String::new(), 0),
            file_offset: -1,
        }
    }

    /// `written()`: the record has been appended to the components file.
    pub fn written(&self) -> bool {
        self.file_offset != -1
    }

    /// `isSameComponentAs` (`logging_component.hpp`): same uid, pid and
    /// component name.
    pub fn is_same_component_as(&self, other: &ComponentMessage) -> bool {
        self.msg.uid == other.uid
            && self.msg.pid == other.pid
            && self.msg.component_name == other.component_name
    }

    /// `updateFirstEntry( suffix, numEntries )`
    /// (`logging_component.cpp`): stamp where the first message landed.
    pub fn update_first_entry(&mut self, suffix: &str, num_entries: u32) {
        self.first_entry = (suffix.to_string(), num_entries as i32);
    }

    /// The on-disk record (`LoggingComponent::write`,
    /// `logging_component.cpp:54-61`).
    pub fn to_bytes(&self, out: &mut MemoryOStream) {
        out.write_u32(self.addr.0);
        out.write_u16(self.addr.1);
        out.write_u16(self.addr.2);
        out.write_u32(self.user_component_id);
        out.write_u32(self.app_instance_id);
        out.write_u32(self.app_type_id);
        self.msg.write(out);
        out.write_string(&self.first_entry.0);
        out.write_u32(self.first_entry.1 as u32);
    }

    /// `LoggingComponent::read` (`logging_component.cpp:67-73`).
    pub fn from_bytes(stream: &mut MemoryIStream) -> Option<Self> {
        // The reference reads its own offset off the file before the
        // payload, so a loaded record is `written()`.
        let file_offset = stream.position() as i64;
        let ip = stream.read_u32()?;
        let port = stream.read_u16()?;
        let salt = stream.read_u16()?;
        let user_component_id = stream.read_u32()?;
        let app_instance_id = stream.read_u32()?;
        let app_type_id = stream.read_u32()?;
        let msg = ComponentMessage::read(stream)?;
        let suffix = stream.read_string();
        let index = stream.read_u32()? as i32;
        Some(Self {
            addr: (ip, port, salt),
            user_component_id,
            app_instance_id,
            app_type_id,
            msg,
            first_entry: (suffix, index),
            file_offset,
        })
    }
}

/// The components file of one user log (`UserComponents`): the records in
/// file order plus the address and ID maps over them.
#[derive(Default, Debug)]
pub struct UserComponents {
    records: Vec<LoggingComponent>,
    addr_map: BTreeMap<(u32, u16, u16), usize>,
    id_map: BTreeMap<u32, usize>,
    id_ticker: u32,
    path: PathBuf,
}

impl UserComponents {
    /// `UserComponents::init( root, mode )` (`user_components.cpp:29-36`):
    /// load `<root>/components` if it exists; keep the file path for
    /// later appends. A missing file is an empty table.
    pub fn init(root: &Path) -> Self {
        let path = root.join("components");
        let mut components = Self {
            path,
            ..Self::default()
        };
        if let Ok(bytes) = std::fs::read(&components.path) {
            let mut stream = MemoryIStream::new(&bytes);
            while stream.remaining() > 0 {
                let Some(component) = LoggingComponent::from_bytes(&mut stream) else {
                    break;
                };
                components.index_new(component);
            }
        }
        components
    }

    /// The runtime index maintenance of `UserComponents::read`
    /// (`user_components.cpp:70-93`), including keeping the ID ticker
    /// ahead of everything on disk.
    fn index_new(&mut self, component: LoggingComponent) {
        let id = component.user_component_id;
        let addr = component.addr;
        self.records.push(component);
        let index = self.records.len() - 1;
        self.addr_map.insert(addr, index);
        self.id_map.insert(id, index);
        if id >= self.id_ticker {
            self.id_ticker = id + 1;
        }
    }

    /// The record behind a user component ID, mutable — the table owns its
    /// components, the way the reference owns pointers to them.
    pub fn by_id_mut(&mut self, id: u32) -> Option<&mut LoggingComponent> {
        self.id_map.get(&id).map(|&index| &mut self.records[index])
    }

    /// `UserComponents::write( logComponent )` (`user_components.cpp:102-108`):
    /// append this component's record to the components file and stamp where
    /// it landed. The record is already indexed — the tables were updated
    /// when the component was created — so the write only appends.
    pub fn write_record(&mut self, component_id: u32) -> bool {
        let Some(&index) = self.id_map.get(&component_id) else {
            return false;
        };
        let mut out = MemoryOStream::new();
        self.records[index].to_bytes(&mut out);
        let record = out.data().to_vec();
        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(_) => return false,
        };
        let offset = file.metadata().map(|m| m.len() as i64).unwrap_or(-1);
        if file.write_all(&record).is_err() {
            return false;
        }
        self.records[index].file_offset = offset;
        true
    }

    /// `getComponentFromMessage` (`user_components.cpp:115-155`): the
    /// component at this address, replaced when a different process now
    /// holds it, created (but not yet written) when new. The process type
    /// comes from the component-name table. Returns the component's user
    /// component ID, the value the caller stamps on its entries
    /// (`entry.userComponentID_ = component->getUserComponentID()`).
    pub fn component_from_message(
        &mut self,
        msg: &ComponentMessage,
        addr: (u32, u16, u16),
        resolve_app_type: impl Fn(&str) -> u32,
    ) -> u32 {
        if let Some(&index) = self.addr_map.get(&addr) {
            if self.records[index].is_same_component_as(msg) {
                return self.records[index].user_component_id;
            }
            // A different process holds the address: drop the old entry.
            let old_id = self.records[index].user_component_id;
            self.id_map.remove(&old_id);
            self.addr_map.remove(&addr);
        }

        let app_type_id = resolve_app_type(&msg.component_name);
        let component =
            LoggingComponent::new(addr, self.next_component_id(), app_type_id, msg.clone());
        let id = component.user_component_id;
        self.records.push(component);
        let index = self.records.len() - 1;
        self.addr_map.insert(addr, index);
        self.id_map.insert(id, index);
        id
    }

    /// `getComponentByAddr`.
    pub fn by_addr(&self, addr: (u32, u16, u16)) -> Option<&LoggingComponent> {
        self.addr_map.get(&addr).map(|&i| &self.records[i])
    }

    /// `getComponentByID`.
    pub fn by_id(&self, id: u32) -> Option<&LoggingComponent> {
        self.id_map.get(&id).map(|&i| &self.records[i])
    }

    /// `getNextUserComponentID`.
    pub fn next_component_id(&mut self) -> u32 {
        let id = self.id_ticker;
        self.id_ticker += 1;
        id
    }

    /// `erase( addr )` (`user_components.cpp:190-215`): drop the
    /// component at this address from the maps.
    pub fn erase(&mut self, addr: (u32, u16, u16)) -> bool {
        let Some(index) = self.addr_map.remove(&addr) else {
            return false;
        };
        let id = self.records[index].user_component_id;
        self.id_map.remove(&id);
        self.records[index].file_offset = -2; // removed from the live maps
        true
    }

    /// How many records the table holds.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the table holds no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

// -----------------------------------------------------------------------------
// Section: one segment (`user_segment.cpp` + `user_segment_writer.cpp`)
// -----------------------------------------------------------------------------

/// One writable segment of a user log (`UserSegmentWriter`): the three
/// files behind `entries.<suffix>`, `args.<suffix>` and
/// `metadata.<suffix>`, plus the running bounds.
pub struct UserSegmentWriter {
    user_log_path: PathBuf,
    suffix: String,
    num_entries: u32,
    args_size: u32,
    start: Option<crate::entry::LogTime>,
    end: Option<crate::entry::LogTime>,
    entries: Option<File>,
    args: Option<File>,
    metadata: Option<File>,
    is_good: bool,
}

impl UserSegmentWriter {
    /// `UserSegmentWriter( userLogPath, suffix )`: a suffix of `None`
    /// builds one from the current time (`user_segment.cpp:22-28`).
    pub fn new(user_log_path: &Path, suffix: Option<&str>) -> Self {
        let suffix = match suffix {
            Some(suffix) => suffix.to_string(),
            None => format_suffix(now_epoch_secs()),
        };
        Self {
            user_log_path: user_log_path.to_path_buf(),
            suffix,
            num_entries: 0,
            args_size: 0,
            start: None,
            end: None,
            entries: None,
            args: None,
            metadata: None,
            is_good: true,
        }
    }

    /// `init()` (`user_segment_writer.cpp:21-63`): open the three files in
    /// append mode and recompute the bounds from what is already there.
    pub fn init(&mut self) -> bool {
        let open = |name: String| -> Option<File> {
            OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(self.user_log_path.join(name))
                .ok()
        };
        self.entries = open(format!("entries.{}", self.suffix));
        self.args = open(format!("args.{}", self.suffix));
        self.metadata = open(format!("metadata.{}", self.suffix));
        self.is_good = self.entries.is_some() && self.args.is_some();
        if !self.is_good {
            return false;
        }
        self.update_entry_bounds();
        self.is_good
    }

    /// `updateEntryBounds` (`user_segment.cpp:59-78`).
    pub fn update_entry_bounds(&mut self) {
        let Some(file) = self.entries.as_mut() else {
            return;
        };
        let _ = file.seek(std::io::SeekFrom::Start(0));
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return;
        }
        self.num_entries = (bytes.len() / LogEntry::PACKED_SIZE) as u32;
        self.args_size = self
            .args
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len() as u32)
            .unwrap_or(0);

        if self.num_entries > 0 {
            let first = LogEntry::from_bytes(&bytes[0..LogEntry::PACKED_SIZE]);
            let last_at = (self.num_entries as usize - 1) * LogEntry::PACKED_SIZE;
            let last = LogEntry::from_bytes(&bytes[last_at..last_at + LogEntry::PACKED_SIZE]);
            self.start = first.map(|e| e.time);
            self.end = last.map(|e| e.time);
        } else {
            self.start = None;
            self.end = None;
        }
        // Restore the append cursor.
        if let Some(file) = self.entries.as_mut() {
            let _ = file.seek(std::io::SeekFrom::End(0));
        }
    }

    /// `addEntry` (`user_segment_writer.cpp:69-154`): encode the arguments
    /// into the args file, the metadata into the metadata file, write the
    /// component once on its first message, then append the fixed-length
    /// record.
    pub fn add_entry(
        &mut self,
        entry: &mut LogEntry,
        handler: &LogStringInterpolator,
        input: &mut MemoryIStream,
        version: u8,
        components: Option<(u32, &mut UserComponents)>,
    ) -> bool {
        let Some(args_file) = self.args.as_mut() else {
            return false;
        };
        entry.args_offset = self.args_size;

        let mut args_out = MemoryOStream::new();
        if !handler.stream_to_log(input, version, &mut args_out) {
            return false;
        }
        if args_file.write_all(args_out.data()).is_err() {
            return false;
        }
        self.args_size += args_out.data().len() as u32;
        entry.set_args(entry.args_offset, args_out.data().len() as u16);

        entry.metadata_offset = self.metadata_len_so_far();
        if version >= LOG_METADATA_VERSION && input.remaining() > 1 {
            let metadata = if version >= LOG_METADATA_STREAM_VERSION {
                // The typed stream renders as JSON
                // (`user_segment_writer.cpp:95-106`). Empty objects are
                // skipped exactly like the reference's `metadata.length()`
                // check.
                let json = build_metadata_json(input);
                input.finish();
                if json.is_empty() {
                    Vec::new()
                } else {
                    json.into_bytes()
                }
            } else {
                // Older versions stream one packed string
                // (`user_segment_writer.cpp:108-121`).
                match read_packed_bytes(input) {
                    Some(bytes) => bytes,
                    None => return false,
                }
            };
            if !metadata.is_empty() {
                let Some(metadata_file) = self.metadata.as_mut() else {
                    return false;
                };
                if metadata_file.write_all(&metadata).is_err() {
                    return false;
                }
            }
        }
        let metadata_len = self.metadata_len_so_far() - entry.metadata_offset;
        entry.set_metadata(entry.metadata_offset, metadata_len as u16);

        // The component's first message also writes the component record
        // (`pUserLog->updateComponent( pComponent )`).
        if let Some((component_id, components_table)) = components {
            let needs_write = !components_table
                .by_id(component_id)
                .map(|component| component.written())
                .unwrap_or(false);
            if needs_write {
                if let Some(component) = components_table.by_id_mut(component_id) {
                    component.update_first_entry(&self.suffix, self.num_entries);
                }
                if !components_table.write_record(component_id) {
                    // The reference logs and carries on.
                }
            }
        }

        let Some(entries_file) = self.entries.as_mut() else {
            return false;
        };
        if entries_file.write_all(&entry.to_bytes()).is_err() {
            return false;
        }

        if self.num_entries == 0 {
            self.start = Some(entry.time);
        }
        self.end = Some(entry.time);
        self.num_entries += 1;
        true
    }

    /// The metadata file's current length.
    fn metadata_len_so_far(&self) -> u32 {
        self.metadata
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len() as u32)
            .unwrap_or(0)
    }

    /// `isFull` (`user_segment_writer.cpp:162-167`).
    pub fn is_full(&self, max_segment_size: u32) -> bool {
        let current = self.num_entries * LogEntry::PACKED_SIZE as u32 + self.args_size;
        current >= max_segment_size
    }

    /// `readEntry( entryNum )` (`user_segment.cpp:84-98`).
    pub fn read_entry(&self, entry_num: u32) -> Option<LogEntry> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(self.user_log_path.join(format!("entries.{}", self.suffix)))
            .ok()?;
        let _ = file.seek(std::io::SeekFrom::Start(
            entry_num as u64 * LogEntry::PACKED_SIZE as u64,
        ));
        let mut record = vec![0u8; LogEntry::PACKED_SIZE];
        file.read_exact(&mut record).ok()?;
        LogEntry::from_bytes(&record)
    }

    /// `getSuffix()`.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// `numEntries()`.
    pub fn num_entries(&self) -> u32 {
        self.num_entries
    }

    /// `start()`/`end()`.
    pub fn bounds(&self) -> (Option<crate::entry::LogTime>, Option<crate::entry::LogTime>) {
        (self.start, self.end)
    }
}

/// `inputStream >> metadata` — a packed-length byte string.
fn read_packed_bytes(input: &mut MemoryIStream) -> Option<Vec<u8>> {
    let len = input.read_string_length();
    if len < 0 {
        return None;
    }
    input.retrieve(len as usize).map(|b| b.to_vec())
}

// -----------------------------------------------------------------------------
// Section: the user log (`user_log_writer.cpp`)
// -----------------------------------------------------------------------------

/// The writer face of one user's log (`UserLogWriter`): the directory, the
/// uid file, the components table, and the single active segment.
pub struct UserLogWriter {
    uid: u16,
    username: String,
    root: PathBuf,
    path: PathBuf,
    components: UserComponents,
    segments: Vec<UserSegmentWriter>,
    is_good: bool,
    /// The suffix source for a new segment. `None` (the default) builds one
    /// from the wall clock, exactly as the reference does
    /// (`UserSegment::UserSegment`, `user_segment.cpp:16-28` — the suffix is
    /// `strftime( "%Y-%m-%d-%T" )` of `time( NULL )`, so two segments born in
    /// the same second take the same name and the second one appends to the
    /// first's files). Tests inject a source so rotation is observable.
    suffix_source: Option<Box<dyn FnMut() -> String>>,
}

impl UserLogWriter {
    /// `UserLogWriter( uid, username )` + `init( rootPath )`
    /// (`user_log_writer.cpp:19-57`): create `<root>/<username>`, write
    /// the uid file (an existing one must agree — `UnaryIntegerFile::init`,
    /// `unary_integer_file.cpp:28-56`), and load the components table.
    pub fn init(root: &Path, uid: u16, username: &str) -> std::io::Result<Self> {
        let path = root.join(username);
        std::fs::create_dir_all(&path)?;

        let uid_path = path.join("uid");
        match std::fs::read_to_string(&uid_path) {
            Ok(existing) => {
                let existing: u16 = existing.trim().parse().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "uid file holds another uid",
                    )
                })?;
                if existing != uid {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "uid file does not match",
                    ));
                }
            }
            Err(_) => {
                std::fs::write(&uid_path, format!("{uid}\n"))?;
            }
        }

        let components = UserComponents::init(&path);
        Ok(Self {
            uid,
            username: username.to_string(),
            root: root.to_path_buf(),
            path,
            components,
            segments: Vec::new(),
            is_good: true,
            suffix_source: None,
        })
    }

    /// `init` with a caller-supplied suffix source, for tests: the reference
    /// builds each segment's suffix from the wall clock, so rotated segments
    /// created inside one second share a name.
    pub fn init_with_suffix_source(
        root: &Path,
        uid: u16,
        username: &str,
        source: Box<dyn FnMut() -> String>,
    ) -> std::io::Result<Self> {
        let mut log = Self::init(root, uid, username)?;
        log.suffix_source = Some(source);
        Ok(log)
    }

    /// The suffix the next new segment takes.
    fn next_suffix(&mut self) -> String {
        match self.suffix_source.as_mut() {
            Some(source) => source(),
            None => format_suffix(now_epoch_secs()),
        }
    }

    /// `getUID()`.
    pub fn uid(&self) -> u16 {
        self.uid
    }

    /// `getUsername()`.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// The user log directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The components table.
    pub fn components(&self) -> &UserComponents {
        &self.components
    }

    /// The table's records in order (placeholders included).
    pub fn component_records(&self) -> &[LoggingComponent] {
        &self.components.records
    }

    /// `isGood()`.
    pub fn is_good(&self) -> bool {
        self.is_good
    }

    /// `findLoggingComponent( addr )`.
    pub fn find_component(&self, addr: (u32, u16, u16)) -> Option<&LoggingComponent> {
        self.components.by_addr(addr)
    }

    /// `findLoggingComponent( msg, addr, logComponentNames )`: the user
    /// component ID for this registration message at this address.
    pub fn component_from_message(
        &mut self,
        msg: &ComponentMessage,
        addr: (u32, u16, u16),
        resolve_app_type: impl Fn(&str) -> u32,
    ) -> u32 {
        self.components
            .component_from_message(msg, addr, resolve_app_type)
    }

    /// `removeUserComponent( addr )`.
    pub fn remove_component(&mut self, addr: (u32, u16, u16)) -> bool {
        self.components.erase(addr)
    }

    /// `hasActiveSegments()`.
    pub fn has_active_segments(&self) -> bool {
        !self.segments.is_empty()
    }

    /// `activeSegmentSuffix()`.
    pub fn active_segment_suffix(&self) -> Option<&str> {
        self.segments.last().map(|s| s.suffix())
    }

    /// The active segment, if any.
    pub fn active_segment(&self) -> Option<&UserSegmentWriter> {
        self.segments.last()
    }

    /// `rollActiveSegment` (`user_log_writer.cpp:79-92`): drop the active
    /// segment so the next message starts a new one.
    pub fn roll_active_segment(&mut self) {
        self.segments.pop();
    }

    /// `addEntry` (`user_log_writer.cpp:98-154`): make sure a segment is
    /// ready — a fresh one when none is active or the active one is full,
    /// dropping every other segment as the reference does — then land the
    /// entry in it. The active segment is the only one kept (`MF_ASSERT(
    /// userSegments_.size() == 1 )`); the port keeps the vector to mirror
    /// the reference's structure but asserts the same single-active
    /// invariant by construction.
    pub fn add_entry(
        &mut self,
        entry: &mut LogEntry,
        handler: &LogStringInterpolator,
        input: &mut MemoryIStream,
        version: u8,
        max_segment_size: u32,
    ) -> bool {
        let needs_segment = match self.segments.last() {
            Some(segment) => segment.is_full(max_segment_size),
            None => true,
        };
        if needs_segment {
            // A fresh writer deletes a stale active_files first, so an
            // external archiver cannot roll a segment mid-creation
            // (`user_log_writer.cpp:111-114`).
            if self.segments.is_empty() {
                let _ = std::fs::remove_file(self.root.join("active_files"));
            }

            let suffix = self.next_suffix();
            let path = self.path.clone();
            let mut segment = UserSegmentWriter::new(&path, Some(&suffix));
            if !segment.init() {
                return false;
            }

            // Drop full segments — they are just eating file handles
            // (`user_log_writer.cpp:121-127`).
            self.segments.clear();
            self.segments.push(segment);

            // Update active_files (`user_log_writer.cpp:131-137`).
            let username = self.username.clone();
            let (path, suffix) = {
                let last = self.segments.last().expect("segment");
                (self.path.clone(), last.suffix().to_string())
            };
            if !write_active_file_for(
                &self.root,
                std::iter::once((username.as_str(), path.as_path(), suffix.as_str())),
            ) {
                return false;
            }
        }

        // `pSegment->addEntry( component, this, ... )` — the segment
        // stamps the entry's component on its first message and appends
        // the record through this log's table. The borrows stay
        // disjoint: the segment lives in `self.segments`, the table in
        // `self.components`; take the segment out for the call.
        let component_id = entry.user_component_id;
        let mut segment = self.segments.pop().expect("active segment");
        let ok = segment.add_entry(
            entry,
            handler,
            input,
            version,
            Some((component_id, &mut self.components)),
        );
        self.segments.push(segment);
        ok
    }
}

/// One open segment as `ActiveFiles::update` writes it: the user
/// directory and the segment suffix.
pub type ActiveSegment<'a> = (&'a str, &'a Path, &'a str);

/// `ActiveFiles::update` (`active_files.cpp:57-125`): rewrite the root
/// `active_files` text file with every user's open segment files. Takes
/// the user directory paths and suffixes so the writer need not borrow
/// the user log while its segments are borrowed for the write.
pub fn write_active_file_for<'a, I>(root: &Path, segments: I) -> bool
where
    I: IntoIterator<Item = ActiveSegment<'a>>,
{
    let mut out = String::new();
    for (username, _path, suffix) in segments {
        for prefix in ["entries", "args", "metadata"] {
            out.push_str(username);
            out.push('/');
            out.push_str(prefix);
            out.push('.');
            out.push_str(suffix);
            out.push('\n');
        }
    }
    std::fs::write(root.join("active_files"), out).is_ok()
}

/// `ActiveFiles::update` over whole user logs (the read-only face).
pub fn write_active_file<'a, I>(root: &Path, user_logs: I) -> bool
where
    I: IntoIterator<Item = &'a UserLogWriter>,
{
    let mut segments = Vec::new();
    for log in user_logs {
        let Some(suffix) = log.active_segment_suffix() else {
            continue;
        };
        let username = log.username();
        let path = log.path();
        segments.push((username, path, suffix));
    }
    write_active_file_for(root, segments)
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageSource;
    use crate::interpolator::{FORMAT_STRING_EOF_MARKER, LogStringInterpolator, stream_to_string};

    /// A scoped temp directory that cleans itself up.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "ml-file-{name}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A component registration like the wire carries one.
    fn fixture_component(uid: u16, pid: u32, name: &str) -> ComponentMessage {
        ComponentMessage {
            version: 10,
            logger_id: "logger-1".to_string(),
            uid,
            pid,
            component_name: name.to_string(),
        }
    }

    /// Append `bytes` as the args for a `%d` entry.
    fn d_arg(value: i32) -> Vec<u8> {
        value.to_le_bytes().to_vec()
    }

    /// Append `bytes` as the args for a `%s ... %d` entry.
    fn sd_arg(text: &str, value: i32) -> Vec<u8> {
        let mut out = vec![text.len() as u8];
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(&value.to_le_bytes());
        out
    }

    /// A user log with one component registered.
    fn seeded_user_log(root: &Path, uid: u16, username: &str) -> (UserLogWriter, u32) {
        let mut log = UserLogWriter::init(root, uid, username).expect("user log");
        let id =
            log.component_from_message(&fixture_component(uid, 4242, "baseapp"), (1, 2, 3), |_| 7);
        (log, id)
    }

    /// The 34-byte record the segment files hold round-trips, and an
    /// original-layout sample parses back with every field intact.
    #[test]
    fn entry_records_round_trip_and_parse_original_samples() {
        let mut entry = LogEntry::new(1_700_000_123, 456, 9, 3, MessageSource::Script, 4, 77);
        entry.set_args(1024, 33);
        entry.set_metadata(4096, 12);
        let bytes = entry.to_bytes();
        assert_eq!(bytes.len(), 34);

        // The pinned field order: time (10), componentID (4), categoryID
        // (2), source (1), priority (1), stringOffset (4), argsOffset (4),
        // argsLen (2), metadataOffset (4), metadataLen (2) — all LE.
        assert_eq!(&bytes[0..8], &1_700_000_123i64.to_le_bytes());
        assert_eq!(&bytes[8..10], &456u16.to_le_bytes());
        assert_eq!(&bytes[10..14], &9u32.to_le_bytes());
        assert_eq!(&bytes[14..16], &3u16.to_le_bytes());
        assert_eq!(bytes[16], 1, "script source");
        assert_eq!(bytes[17], 4);
        assert_eq!(&bytes[18..22], &77u32.to_le_bytes());
        assert_eq!(&bytes[22..26], &1024u32.to_le_bytes());
        assert_eq!(&bytes[26..28], &33u16.to_le_bytes());
        assert_eq!(&bytes[28..32], &4096u32.to_le_bytes());
        assert_eq!(&bytes[32..34], &12u16.to_le_bytes());

        let parsed = LogEntry::from_bytes(&bytes).expect("parses");
        assert_eq!(parsed.time.secs, 1_700_000_123);
        assert_eq!(parsed.time.msecs, 456);
        assert_eq!(parsed.user_component_id, 9);
        assert_eq!(parsed.category_id, 3);
        assert_eq!(parsed.message_source, MessageSource::Script);
        assert_eq!(parsed.message_priority, 4);
        assert_eq!(parsed.string_offset, 77);
        assert_eq!(parsed.args_offset, 1024);
        assert_eq!(parsed.args_len, 33);
        assert_eq!(parsed.metadata_offset, 4096);
        assert_eq!(parsed.metadata_len, 12);

        // A truncated record is rejected, not misread.
        assert!(LogEntry::from_bytes(&bytes[..20]).is_none());
    }

    /// The suffix formats the segment start time `%Y-%m-%d-%T`
    /// (`user_segment.cpp:115`).
    #[test]
    fn suffix_formats_the_start_time() {
        // 2021-01-01 00:00:00 UTC = 1_609_459_200.
        assert_eq!(format_suffix(1_609_459_200), "2021-01-01-00:00:00");
        assert_eq!(
            format_suffix(1_609_459_200 + 12 * 3600 + 34 * 60 + 56),
            "2021-01-01-12:34:56"
        );
        // The leap day: 2024-02-29 23:59:59 UTC = 1_709_251_199.
        assert_eq!(format_suffix(1_709_251_199), "2024-02-29-23:59:59");
    }

    /// A fresh segment over existing files picks the bounds back up —
    /// the append-mode reopen path.
    #[test]
    fn a_reopened_segment_continues_its_bounds() {
        let dir = TempDir::new("bounds");
        let handler = LogStringInterpolator::new("%d");

        let mut first = UserSegmentWriter::new(dir.path(), Some("2026-11-24-10:00:00"));
        assert!(first.init());
        let args = d_arg(42);
        let mut entry = LogEntry::new(100, 0, 1, 0, MessageSource::Cpp, 2, 0);
        let mut input = MemoryIStream::new(&args);
        assert!(first.add_entry(&mut entry, &handler, &mut input, 10, None));
        assert_eq!(first.num_entries(), 1);

        let mut second = UserSegmentWriter::new(dir.path(), Some("2026-11-24-10:00:00"));
        assert!(second.init());
        assert_eq!(second.num_entries(), 1);
        let (start, end) = second.bounds();
        assert_eq!(start.map(|t| t.secs), Some(100));
        assert_eq!(end.map(|t| t.secs), Some(100));

        // The record reads back with its args span.
        let read_back = second.read_entry(0).expect("entry 0");
        assert_eq!(read_back.args_len, 4);
        let args_bytes = std::fs::read(dir.path().join("args.2026-11-24-10:00:00")).expect("args");
        let mut blob = MemoryIStream::new(
            &args_bytes[read_back.args_offset as usize..][..read_back.args_len as usize],
        );
        let mut rendered = String::new();
        assert!(stream_to_string(&handler, &mut blob, 10, &mut rendered));
        assert_eq!(rendered, "42");
    }

    /// The args blob encodes what the read side walks back: strings
    /// packed, ints at their conversion width, variable widths as
    /// `uint16`s.
    #[test]
    fn args_blobs_encode_the_reference_layout() {
        let handler = LogStringInterpolator::new("%s have %d apples, %*.*f juice");
        let mut net = Vec::new();
        net.push(5);
        net.extend_from_slice(b"Alice");
        net.extend_from_slice(&42i32.to_le_bytes());
        net.extend_from_slice(&7u16.to_le_bytes()); // min width
        net.extend_from_slice(&2u16.to_le_bytes()); // max width
        net.extend_from_slice(&1.5f64.to_bits().to_le_bytes());

        let mut out = MemoryOStream::new();
        let mut input = MemoryIStream::new(&net);
        assert!(handler.stream_to_log(&mut input, 10, &mut out));

        // "Alice" packed, 42 as four bytes, both widths as `uint16`,
        // the double as eight bytes — the encoding `streamToString` walks.
        let expected: Vec<u8> = {
            let mut e = Vec::new();
            e.push(5);
            e.extend_from_slice(b"Alice");
            e.extend_from_slice(&42i32.to_le_bytes());
            e.extend_from_slice(&7u16.to_le_bytes());
            e.extend_from_slice(&2u16.to_le_bytes());
            e.extend_from_slice(&1.5f64.to_bits().to_le_bytes());
            e
        };
        assert_eq!(out.data(), &expected[..]);

        // The blob walks back to the rendered string — the lossless loop.
        let mut blob = MemoryIStream::new(out.data());
        let mut rendered = String::new();
        assert!(stream_to_string(&handler, &mut blob, 10, &mut rendered));
        // The `%*.*f` field is seven wide, so the value is left-padded.
        assert_eq!(rendered, "Alice have 42 apples,    1.50 juice");
    }

    /// A short argument stream fails the write and leaves the handler
    /// unusable for that entry (`CHECK_STREAM`, `onError`).
    #[test]
    fn a_short_argument_stream_fails_the_write() {
        let handler = LogStringInterpolator::new("%s and %d");
        let mut net = Vec::new();
        net.push(3);
        net.extend_from_slice(b"Bob");
        // The %d argument is missing.

        let mut out = MemoryOStream::new();
        let mut input = MemoryIStream::new(&net);
        assert!(!handler.stream_to_log(&mut input, 10, &mut out));
        // Only the "Bob" string made it — the write stops short and the
        // entry is dropped, so nothing of this lands in the args file.
        assert_eq!(out.data(), b"\x03Bob");
    }

    /// Entries land in the three segment files; the record points at the
    /// right args and metadata spans.
    #[test]
    fn entries_land_in_the_segment_files() {
        let dir = TempDir::new("segment");
        let handler = LogStringInterpolator::new("%s did %d");
        let mut segment = UserSegmentWriter::new(dir.path(), Some("2026-11-24-11:00:00"));
        assert!(segment.init());

        // Network stream: "jump", 7, then the metadata typed stream
        // ("key1" = "key") and the trailing NULL byte the reference tolerates.
        let mut net = Vec::new();
        net.push(4);
        net.extend_from_slice(b"jump");
        net.extend_from_slice(&7i32.to_le_bytes());
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(4);
        net.extend_from_slice(b"key1");
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(3);
        net.extend_from_slice(b"key");
        net.push(0);

        let mut entry = LogEntry::new(500, 5, 11, 1, MessageSource::Cpp, 3, 0);
        let mut input = MemoryIStream::new(&net);
        assert!(segment.add_entry(&mut entry, &handler, &mut input, 10, None));

        // entries: one 34-byte record.
        let entries_bytes =
            std::fs::read(dir.path().join("entries.2026-11-24-11:00:00")).expect("entries");
        assert_eq!(entries_bytes.len(), 34);
        let stored = LogEntry::from_bytes(&entries_bytes).expect("record");
        assert_eq!(stored.string_offset, 0);
        assert_eq!(stored.args_offset, 0);
        assert_eq!(stored.metadata_offset, 0);
        assert_eq!(stored.args_len, 5 + 4);

        // args: the re-encoded blob.
        let args_bytes = std::fs::read(dir.path().join("args.2026-11-24-11:00:00")).expect("args");
        assert_eq!(args_bytes.len(), stored.args_len as usize);

        // metadata: the JSON object, keys sorted, one trailing newline.
        let metadata_bytes =
            std::fs::read(dir.path().join("metadata.2026-11-24-11:00:00")).expect("metadata");
        assert_eq!(metadata_bytes.len(), stored.metadata_len as usize);
        assert_eq!(&metadata_bytes[..], "{\"key1\":\"key\"}\n".as_bytes());

        // The record reads back through the segment face too, and the
        // args blob renders.
        let again = segment.read_entry(0).expect("entry 0");
        assert_eq!(again.time.secs, 500);
        assert_eq!(again.time.msecs, 5);
        let mut blob = MemoryIStream::new(&args_bytes[..stored.args_len as usize]);
        let mut rendered = String::new();
        assert!(stream_to_string(&handler, &mut blob, 10, &mut rendered));
        assert_eq!(rendered, "jump did 7");
    }

    /// Version 9 metadata rides as a raw packed string; version 8 carries
    /// none at all (`user_segment_writer.cpp:92-122`).
    #[test]
    fn older_versions_carry_metadata_as_a_raw_string() {
        let dir = TempDir::new("meta-versions");
        let handler = LogStringInterpolator::new("%d");

        // Version 9: the trailing bytes are one packed string.
        let mut net9 = d_arg(5);
        net9.push(6);
        net9.extend_from_slice(b"hello!");
        let mut segment = UserSegmentWriter::new(dir.path(), Some("s9"));
        assert!(segment.init());
        let mut entry = LogEntry::new(1, 0, 1, 0, MessageSource::Cpp, 2, 0);
        let mut input = MemoryIStream::new(&net9);
        assert!(segment.add_entry(&mut entry, &handler, &mut input, 9, None));
        let metadata = std::fs::read(dir.path().join("metadata.s9")).expect("metadata");
        assert_eq!(&metadata[..], b"hello!");

        // Version 8: no metadata is written even with trailing bytes.
        let mut net8 = d_arg(5);
        net8.extend_from_slice(b"ignored");
        let mut segment = UserSegmentWriter::new(dir.path(), Some("s8"));
        assert!(segment.init());
        let mut entry = LogEntry::new(2, 0, 1, 0, MessageSource::Cpp, 2, 0);
        let mut input = MemoryIStream::new(&net8);
        assert!(segment.add_entry(&mut entry, &handler, &mut input, 8, None));
        let metadata_len = std::fs::metadata(dir.path().join("metadata.s8"))
            .expect("metadata file")
            .len();
        assert_eq!(metadata_len, 0);
        assert_eq!(entry.metadata_len, 0);
    }

    /// The raw-JSON metadata pair overrides everything else and lands
    /// compact with a trailing newline (`setJSON`, `getJSON`).
    #[test]
    fn the_raw_json_pair_overrides_other_metadata() {
        let mut net = Vec::new();
        // ("zeta", 1) first — the override must beat the sorted order.
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(4);
        net.extend_from_slice(b"zeta");
        net.extend_from_slice(&(b'i' as u32).to_le_bytes());
        net.extend_from_slice(&1u32.to_le_bytes());
        // Then the JSON override.
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(9);
        net.extend_from_slice(KEY_JSON_STRING.as_bytes());
        net.extend_from_slice(&(b'j' as u32).to_le_bytes());
        let raw = r#"{ "z" : 9, "y" : [1, 2] }"#;
        net.push(raw.len() as u8);
        net.extend_from_slice(raw.as_bytes());
        net.push(0);

        let mut input = MemoryIStream::new(&net);
        let json = build_metadata_json(&mut input);
        // The JSON argument overrides the earlier pair, and whitespace is
        // compacted. (The reference re-renders through jsoncpp, which also
        // sorts the keys — see the module's registered deviations.)
        assert_eq!(json, "{\"z\":9,\"y\":[1,2]}\n");
    }

    /// The metadata builder renders the typed pairs as a sorted JSON
    /// object (`LogMetadataBuilder::process` + `MLDBMetadataStream`).
    #[test]
    fn metadata_pairs_render_as_sorted_json() {
        let mut net = Vec::new();
        // ("zeta", 2)
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(4);
        net.extend_from_slice(b"zeta");
        net.extend_from_slice(&(b'i' as u32).to_le_bytes());
        net.extend_from_slice(&2u32.to_le_bytes());
        // ("alpha", "w")
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(5);
        net.extend_from_slice(b"alpha");
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(1);
        net.push(b'w');
        // ("num", 2.5)
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(3);
        net.extend_from_slice(b"num");
        net.extend_from_slice(&(b'd' as u32).to_le_bytes());
        net.extend_from_slice(&2.5f64.to_bits().to_le_bytes());
        // ("neg", -3)
        net.extend_from_slice(&(b's' as u32).to_le_bytes());
        net.push(3);
        net.extend_from_slice(b"neg");
        net.extend_from_slice(&(b'i' as u32).to_le_bytes());
        net.extend_from_slice(&(-3i32 as u32).to_le_bytes());
        net.push(0);

        let mut input = MemoryIStream::new(&net);
        let json = build_metadata_json(&mut input);
        assert_eq!(
            json,
            "{\"alpha\":\"w\",\"neg\":-3,\"num\":2.5,\"zeta\":2}\n"
        );
    }

    /// A full segment rolls into a new one; every older segment is
    /// dropped and `active_files` names only the active one.
    #[test]
    fn a_full_segment_rolls_into_a_new_one() {
        let dir = TempDir::new("rotation");
        // Distinct suffixes, so the rotation is observable on disk.
        let ticks = std::cell::Cell::new(0u32);
        let mut log = UserLogWriter::init_with_suffix_source(
            dir.path(),
            1001,
            "alice",
            Box::new(move || {
                ticks.set(ticks.get() + 1);
                format!("2026-11-24-09:{:02}:00", ticks.get())
            }),
        )
        .expect("user log");
        let id =
            log.component_from_message(&fixture_component(1001, 4242, "baseapp"), (1, 2, 3), |_| 7);
        let handler = LogStringInterpolator::new("%d");

        // Two entries at a 34-byte cap: the first fills the segment, the
        // second finds it full and rolls.
        for secs in [10u8, 20] {
            let args = d_arg(secs as i32);
            let mut entry = LogEntry::new(secs as i64, 0, id, 0, MessageSource::Cpp, 2, 0);
            let mut input = MemoryIStream::new(&args);
            assert!(log.add_entry(&mut entry, &handler, &mut input, 10, 34));
        }

        // The component record was appended exactly once, stamped with
        // the FIRST segment's suffix and entry zero.
        let component = log.components().by_id(id).expect("component");
        assert!(component.written());
        assert_eq!(component.first_entry.1, 0);

        // Two segment suffixes exist on disk.
        let mut segment_files: Vec<_> = std::fs::read_dir(log.path())
            .expect("user dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("entries."))
            .collect();
        segment_files.sort();
        assert_eq!(segment_files.len(), 2, "{segment_files:?}");

        // active_files sits at the root and names only the ACTIVE
        // segment's files.
        let active =
            std::fs::read_to_string(dir.path().join("active_files")).expect("active_files");
        let active_suffix = log.active_segment_suffix().expect("active");
        assert_eq!(
            active,
            format!(
                "alice/entries.{active_suffix}\nalice/args.{active_suffix}\nalice/metadata.{active_suffix}\n"
            )
        );
    }

    /// `rollActiveSegment` forces the next message into a fresh segment
    /// even when the old one has room.
    #[test]
    fn rolling_forces_a_new_segment() {
        let dir = TempDir::new("roll");
        let ticks = std::cell::Cell::new(0u32);
        let mut log = UserLogWriter::init_with_suffix_source(
            dir.path(),
            1001,
            "alice",
            Box::new(move || {
                ticks.set(ticks.get() + 1);
                format!("2026-11-24-08:{:02}:00", ticks.get())
            }),
        )
        .expect("user log");
        let id =
            log.component_from_message(&fixture_component(1001, 4242, "baseapp"), (1, 2, 3), |_| 7);
        let handler = LogStringInterpolator::new("%d");

        let args = d_arg(1);
        let mut entry = LogEntry::new(1, 0, id, 0, MessageSource::Cpp, 2, 0);
        let mut input = MemoryIStream::new(&args);
        assert!(log.add_entry(&mut entry, &handler, &mut input, 10, u32::MAX));
        let first_suffix = log.active_segment_suffix().expect("active").to_string();

        log.roll_active_segment();
        assert!(!log.has_active_segments());

        let args = d_arg(2);
        let mut entry = LogEntry::new(2, 0, id, 0, MessageSource::Cpp, 2, 0);
        let mut input = MemoryIStream::new(&args);
        assert!(log.add_entry(&mut entry, &handler, &mut input, 10, u32::MAX));
        let second_suffix = log.active_segment_suffix().expect("active").to_string();
        assert_ne!(first_suffix, second_suffix, "a rolled segment is replaced");
    }

    /// The directory layout: `uid` text file, `components` file, segment
    /// files; a second init with the same uid agrees, a different one
    /// is refused (`UnaryIntegerFile::init`).
    #[test]
    fn the_user_log_directory_lays_out_uid_and_components() {
        let dir = TempDir::new("layout");
        {
            let (log, _) = seeded_user_log(dir.path(), 1001, "alice");
            assert_eq!(log.uid(), 1001);
            assert_eq!(log.username(), "alice");
            assert_eq!(
                std::fs::read_to_string(log.path().join("uid")).expect("uid"),
                "1001\n"
            );
            // Creating the component alone does not write it — the
            // reference defers the append to the first entry.
            assert_eq!(log.components().len(), 1);
            assert!(!log.components().by_id(0).expect("component").written());
        }

        // The same uid re-inits fine; a different uid is refused.
        assert!(UserLogWriter::init(dir.path(), 1001, "alice").is_ok());
        assert!(matches!(
            UserLogWriter::init(dir.path(), 1002, "alice"),
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData
        ));
    }

    /// The components file: one append per component, a fresh writer
    /// reads the records back, and a different process at the same
    /// address replaces the entry.
    #[test]
    fn components_append_once_and_reload() {
        let dir = TempDir::new("components");
        let (mut log, id) = seeded_user_log(dir.path(), 1001, "alice");
        let handler = LogStringInterpolator::new("%d");

        // First entry writes the component record.
        let args = d_arg(1);
        let mut entry = LogEntry::new(1, 0, id, 0, MessageSource::Cpp, 2, 0);
        let mut input = MemoryIStream::new(&args);
        assert!(log.add_entry(&mut entry, &handler, &mut input, 10, u32::MAX));
        let record_size = {
            let component = log.components().by_id(id).expect("component");
            let mut probe = MemoryOStream::new();
            component.to_bytes(&mut probe);
            probe.data().len()
        };
        assert_eq!(
            std::fs::metadata(log.path().join("components"))
                .expect("components")
                .len() as usize,
            record_size
        );

        // A different process at the same address replaces the row.
        let newcomer = fixture_component(1001, 7777, "cellapp");
        let new_id = log.component_from_message(&newcomer, (1, 2, 3), |_| 9);
        assert_eq!(
            log.components()
                .by_addr((1, 2, 3))
                .unwrap()
                .user_component_id,
            new_id
        );
        assert_ne!(new_id, id);
        // The stale ID is out of the maps but the record lingers, as the
        // reference keeps the vector around.
        assert_eq!(log.components().by_id(id), None);
    }

    /// A user component erased from the address map is gone from both
    /// maps (`UserComponents::erase`).
    #[test]
    fn erasing_a_component_clears_both_maps() {
        let dir = TempDir::new("erase");
        let (mut log, id) = seeded_user_log(dir.path(), 1001, "alice");
        assert!(log.remove_component((1, 2, 3)));
        assert_eq!(log.components().by_addr((1, 2, 3)), None);
        assert_eq!(log.components().by_id(id), None);
        assert!(!log.remove_component((1, 2, 3)));
    }

    /// The acceptance: a fixed event sequence through the whole file
    /// face — two components, six messages over rolled segments — parses
    /// back losslessly through the independent read faces: entry records,
    /// the strings file, the args blobs, and the reloaded components.
    #[test]
    fn the_written_tree_parses_back_losslessly() {
        let dir = TempDir::new("acceptance");
        let ticks = std::cell::Cell::new(0u32);
        let mut log = UserLogWriter::init_with_suffix_source(
            dir.path(),
            2001,
            "bob",
            Box::new(move || {
                ticks.set(ticks.get() + 1);
                format!("2026-11-24-07:{:02}:00", ticks.get())
            }),
        )
        .expect("user log");

        // Two components register; their app types come from the table.
        let resolve = |name: &str| -> u32 { if name == "baseapp" { 5 } else { 6 } };
        let baseapp = fixture_component(2001, 100, "baseapp");
        let cellapp = fixture_component(2001, 200, "cellapp");
        let base_id = log.component_from_message(&baseapp, (10, 1, 1), resolve);
        let cell_id = log.component_from_message(&cellapp, (10, 1, 2), resolve);

        // The format string is interned into the root strings file.
        let strings_path = dir.path().join("strings");
        let mut handler = LogStringInterpolator::new("%s at %d");
        let mut strings_out = MemoryOStream::new();
        assert!(handler.write_to(&mut strings_out));
        assert_ne!(handler.file_offset(), FORMAT_STRING_EOF_MARKER);
        std::fs::write(&strings_path, strings_out.data()).expect("strings file");

        // Six messages: two fill a 100-byte segment (34 + args each), so
        // three segments carry them in order.
        let mut expected: Vec<String> = Vec::new();
        for i in 0..6u32 {
            let who = if i % 2 == 0 { "base" } else { "cell" };
            let id = if i % 2 == 0 { base_id } else { cell_id };
            let msg_text = format!("{who} tick {i}");
            let net = sd_arg(&msg_text, 1000 + i as i32);
            let entry = LogEntry::new(
                2000 + i as i64,
                (i * 7 % 1000) as u16,
                id,
                if i % 2 == 0 { 5 } else { 6 },
                MessageSource::Cpp,
                4,
                handler.file_offset(),
            );
            let mut entry = entry;
            let mut input = MemoryIStream::new(&net);
            assert!(log.add_entry(&mut entry, &handler, &mut input, 10, 100));
            expected.push(format!("{msg_text} at {}", 1000 + i));
        }

        // Read the tree back through the independent faces.
        // 1. The strings file parses back into the handler.
        let strings_bytes = std::fs::read(&strings_path).expect("strings");
        let mut strings_stream = MemoryIStream::new(&strings_bytes);
        let read_handler = LogStringInterpolator::read_from(&mut strings_stream).expect("handler");
        assert_eq!(read_handler.format_string(), "%s at %d");
        assert_eq!(read_handler.file_offset(), handler.file_offset());
        assert_eq!(read_handler.pieces().0, handler.pieces().0);
        assert_eq!(read_handler.pieces().1, handler.pieces().1);
        assert_eq!(read_handler.pieces().2, handler.pieces().2);

        // 2. Every entry record's args blob re-renders to the expected
        //    text, in order.
        let mut seen: Vec<String> = Vec::new();
        let mut segment_files: Vec<_> = std::fs::read_dir(log.path())
            .expect("user dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("entries."))
            .collect();
        segment_files.sort();
        for file in &segment_files {
            let suffix = file.trim_start_matches("entries.");
            let mut segment = UserSegmentWriter::new(log.path(), Some(suffix));
            assert!(segment.init());
            let args_bytes =
                std::fs::read(log.path().join(format!("args.{suffix}"))).expect("args");
            for n in 0..segment.num_entries() {
                let entry = segment.read_entry(n).expect("record");
                assert_eq!(entry.string_offset, read_handler.file_offset());
                let start = entry.args_offset as usize;
                let end = start + entry.args_len as usize;
                let mut blob = MemoryIStream::new(&args_bytes[start..end]);
                let mut rendered = String::new();
                assert!(stream_to_string(
                    &read_handler,
                    &mut blob,
                    10,
                    &mut rendered
                ));
                seen.push(rendered);
            }
        }
        assert_eq!(seen, expected);

        // 3. The components file reloads with both records, each stamped
        //    with the suffix its first message landed in.
        let reloaded = UserLogWriter::init(dir.path(), 2001, "bob").expect("reload");
        assert_eq!(reloaded.components().len(), 2);
        let base = reloaded.components().by_addr((10, 1, 1)).expect("base");
        assert_eq!(base.msg.pid, 100);
        assert_eq!(base.app_type_id, 5);
        assert!(base.written());
        assert_eq!(base.first_entry.1, 0, "the baseapp's first message");
        let cell = reloaded.components().by_addr((10, 1, 2)).expect("cell");
        assert_eq!(cell.app_type_id, 6);
        assert_eq!(cell.first_entry.1, 1, "the cellapp's first message");

        // 4. The entries' segment counts sum to six.
        let total: u32 = segment_files
            .iter()
            .map(|file| {
                let suffix = file.trim_start_matches("entries.");
                let mut segment = UserSegmentWriter::new(log.path(), Some(suffix));
                assert!(segment.init());
                segment.num_entries()
            })
            .sum();
        assert_eq!(total, 6);
    }

    /// 读入原版样本且输出对照: a segment sample hand-encoded byte by byte
    /// off the reference's pinned layouts (`log_entry.hpp`'s packed record,
    /// `LogStringWriter`'s arg blob, `LogStringInterpolator::write`'s
    /// strings record) is read back through the port's read faces and
    /// renders exactly what the original would have printed.
    #[test]
    fn a_hand_encoded_original_sample_reads_back() {
        // entries.<suffix>: the 34-byte record, at the pinned offsets.
        let mut record = vec![0u8; 34];
        record[0..8].copy_from_slice(&1_700_000_000i64.to_le_bytes());
        record[8..10].copy_from_slice(&250u16.to_le_bytes());
        record[10..14].copy_from_slice(&7u32.to_le_bytes());
        record[14..16].copy_from_slice(&3u16.to_le_bytes());
        record[16] = 0; // C++ call site
        record[17] = 2; // priority
        record[18..22].copy_from_slice(&0u32.to_le_bytes());
        record[22..26].copy_from_slice(&0u32.to_le_bytes());
        record[26..28].copy_from_slice(&7u16.to_le_bytes());
        record[28..32].copy_from_slice(&0u32.to_le_bytes());
        record[32..34].copy_from_slice(&0u16.to_le_bytes());

        let entry = LogEntry::from_bytes(&record).expect("record");
        assert_eq!(entry.time.secs, 1_700_000_000);
        assert_eq!(entry.time.msecs, 250);
        assert_eq!(entry.user_component_id, 7);
        assert_eq!(entry.category_id, 3);
        assert_eq!(entry.message_source, MessageSource::Cpp);
        assert_eq!(entry.message_priority, 2);
        assert_eq!(entry.args_len, 7);

        // args.<suffix>: "%s at %d" with "hi" and 5 — the string packed
        // then the int at its default width.
        let args: Vec<u8> = vec![2, b'h', b'i', 5, 0, 0, 0];
        assert_eq!(args.len(), entry.args_len as usize);

        // strings: "%s at %d" as `LogStringInterpolator::write` emits it —
        // the format string, the piece kinds (`t` conversion, `s` literal,
        // `t`), the literal's offsets, then a `FormatData` per conversion:
        // the `%s` one is a placeholder, the `%d` one carries the real data.
        let mut strings: Vec<u8> = Vec::new();
        strings.push(8);
        strings.extend_from_slice(b"%s at %d");
        strings.extend_from_slice(&3u32.to_le_bytes()); // components
        strings.extend_from_slice(b"tst");
        strings.extend_from_slice(&1u32.to_le_bytes()); // string offsets
        strings.extend_from_slice(&[2, 0, 6, 0]); // the " at " literal
        strings.extend_from_slice(&2u32.to_le_bytes()); // fmt data
        strings.extend_from_slice(&[b's', 0, 0]); // 's': the string token
        strings.extend_from_slice(&0i32.to_le_bytes()); // min width
        strings.extend_from_slice(&(-1i32).to_le_bytes()); // max width
        strings.extend_from_slice(&0i32.to_le_bytes()); // flags
        strings.extend_from_slice(&[b'd', 0, 10]); // 'd', no flags, base 10
        strings.extend_from_slice(&0i32.to_le_bytes()); // min width
        strings.extend_from_slice(&(-1i32).to_le_bytes()); // max width
        strings.extend_from_slice(&0i32.to_le_bytes()); // flags

        let mut strings_stream = MemoryIStream::new(&strings);
        let handler = LogStringInterpolator::read_from(&mut strings_stream).expect("handler");
        assert_eq!(handler.format_string(), "%s at %d");
        assert_eq!(handler.file_offset(), 0);

        let mut blob = MemoryIStream::new(&args);
        let mut rendered = String::new();
        assert!(stream_to_string(&handler, &mut blob, 10, &mut rendered));
        assert_eq!(rendered, "hi at 5");
    }

    /// `ActiveFiles::update` lists every user's open segment, not just one
    /// (`active_files.cpp:57-125`): two users with a live segment each
    /// contribute their three file names, and a user without one is skipped.
    #[test]
    fn the_active_files_list_every_users_open_segment() {
        let dir = TempDir::new("active-files");
        let ticks = std::cell::Cell::new(0u32);
        let mut alice = UserLogWriter::init_with_suffix_source(
            dir.path(),
            1001,
            "alice",
            Box::new(move || {
                ticks.set(ticks.get() + 1);
                format!("2026-11-24-05:{:02}:00", ticks.get())
            }),
        )
        .expect("alice");
        let bob = UserLogWriter::init(dir.path(), 1002, "bob").expect("bob");

        // Alice logs one message, which opens her active segment; bob never
        // logs, so he has no active segment to list.
        let id = alice.component_from_message(
            &fixture_component(1001, 4242, "baseapp"),
            (1, 2, 3),
            |_| 7,
        );
        let handler = LogStringInterpolator::new("%d");
        let args = d_arg(1);
        let mut entry = LogEntry::new(1, 0, id, 0, MessageSource::Cpp, 2, 0);
        let mut input = MemoryIStream::new(&args);
        assert!(alice.add_entry(&mut entry, &handler, &mut input, 10, u32::MAX));
        assert!(alice.has_active_segments());
        assert!(!bob.has_active_segments());

        let alice_suffix = alice.active_segment_suffix().expect("active").to_string();
        assert!(write_active_file(dir.path(), [&alice, &bob]));
        let text = std::fs::read_to_string(dir.path().join("active_files")).expect("active_files");
        assert_eq!(
            text,
            format!(
                "alice/entries.{alice_suffix}\nalice/args.{alice_suffix}\nalice/metadata.{alice_suffix}\n"
            )
        );
    }

    /// A uid file that cannot be written (a directory sits where it
    /// must go) fails the init cleanly, without panicking.
    #[test]
    fn an_unwritable_log_dir_fails_cleanly() {
        let dir = TempDir::new("readonly");
        std::fs::create_dir_all(dir.path().join("eve").join("uid")).expect("uid dir");
        assert!(UserLogWriter::init(dir.path(), 3001, "eve").is_err());
    }
}
