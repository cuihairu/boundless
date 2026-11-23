//! The metadata stack (`lib/cstdmf/log_meta_data.hpp`, `server/tools/
//! message_logger/metadata.hpp/.cpp`): the wire tail of key/value pairs,
//! the UTF-8 cleanup applied to every string, and the JSON image the MLDB
//! backend stores — `Json::FastWriter` output, i.e. keys in `std::map`
//! (byte) order, no whitespace, one trailing newline.
//!
//! The `Json::Value` subset here mirrors what `MLDBMetadataStream` can
//! hold; the serializer reproduces jsoncpp 1.9.6 (the version the
//! reference binary links) byte-for-byte, including `%.17g` reals and
//! `null` for non-finite numbers.

use crate::stream::{MemoryIStream, MemoryOStream};
use std::collections::BTreeMap;

/// `LogMetaData::MessageLoggerValueType` — the one-byte type tags on the
/// wire (`log_meta_data.cpp:18-25`).
pub mod mlvt {
    /// A fully-formed JSON payload.
    pub const JSON_STRING: u8 = b'j';
    /// Signed 32-bit.
    pub const INT: u8 = b'i';
    /// Unsigned 32-bit.
    pub const UNSIGNED_INT: u8 = b'u';
    /// Signed 64-bit.
    pub const LONG_LONG_INT: u8 = b'l';
    /// Length-prefixed string.
    pub const STRING: u8 = b's';
    /// 32-bit float.
    pub const FLOAT: u8 = b'f';
    /// 64-bit float.
    pub const DOUBLE: u8 = b'd';
}

/// `LogMetaData::key_jsonString` — the only key allowed to carry
/// `MLVT_JSON_STRING`.
pub const KEY_JSON_STRING: &str = "__bw_json";

/// The `Json::Value` shapes the metadata stream can hold.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum JsonValue {
    #[default]
    Null,
    Bool(bool),
    Int(i32),
    UInt(u32),
    Int64(i64),
    UInt64(u64),
    Real(f64),
    Str(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

/// C `%g` with the given precision — jsoncpp renders every real as
/// `%.17g`, stripping trailing zeros, switching to scientific notation
/// when the exponent is below -4 or reaches the precision budget.
fn format_g(value: f64, precision: usize) -> String {
    if !value.is_finite() {
        // FastWriter keeps useSpecialFloats false: non-finite is null.
        return String::new();
    }
    if value == 0.0 {
        // Covers +0.0 and -0.0 the way printf does.
        return if value.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }

    // Exponent of the value in decimal scientific notation — Rust's
    // exponential formatting is correctly rounded like C's.
    let sci = format!("{:e}", value.abs());
    let exp: i32 = sci.split_once('e').unwrap().1.parse().unwrap();

    if exp < -4 || exp >= precision as i32 {
        // Scientific: P-1 fraction digits, then trim.
        let sci = format!("{:.*e}", precision - 1, value);
        let (mantissa, exp_part) = sci.split_once('e').unwrap();
        let mantissa = trim_number(mantissa);
        let exp: i32 = exp_part.parse().unwrap();
        if exp < 0 {
            format!("{}e-{:02}", mantissa, -exp)
        } else {
            format!("{}e+{:02}", mantissa, exp)
        }
    } else {
        // Fixed: P-1-exp fraction digits, then trim.
        let decimals = (precision as i32 - 1 - exp).max(0) as usize;
        let fixed = format!("{:.*}", decimals, value);
        trim_number(&fixed)
    }
}

/// Strip trailing zeros and a bare trailing point, like `%g` output.
fn trim_number(text: &str) -> String {
    if !text.contains('.') {
        return text.to_string();
    }
    let trimmed = text.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

/// Escape a string the way jsoncpp's `valueToString` does: quote, reverse
/// solidus, the five named controls, and `\\u00xx` (lowercase) for the
/// remaining C0 bytes. jsoncpp walks the C string byte by byte, so every
/// byte is copied verbatim except the escaped ones — this port does the
/// same on bytes and only then re-decodes, which keeps multi-byte UTF-8
/// sequences intact. jsoncpp sees `.c_str()`, so everything after the first
/// NUL byte is invisible to it.
fn json_escape(text: &str) -> String {
    let cut = text.find('\0').unwrap_or(text.len());
    let mut out = Vec::with_capacity(cut + 2);
    out.push(b'"');
    for &byte in text.as_bytes()[..cut].iter() {
        match byte {
            b'"' | b'\\' => {
                out.push(b'\\');
                out.push(byte);
            }
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x00..=0x1f => out.extend_from_slice(&format!("\\u{:04x}", byte).into_bytes()),
            _ => out.push(byte),
        }
    }
    out.push(b'"');
    String::from_utf8_lossy(&out).into_owned()
}

/// `Json::FastWriter::write` — compact serialization plus the trailing
/// newline FastWriter always appends.
pub fn fast_writer_json(value: &JsonValue) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out.push('\n');
    out
}

fn write_value(value: &JsonValue, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(true) => out.push_str("true"),
        JsonValue::Bool(false) => out.push_str("false"),
        JsonValue::Int(v) => out.push_str(&v.to_string()),
        JsonValue::UInt(v) => out.push_str(&v.to_string()),
        JsonValue::Int64(v) => out.push_str(&v.to_string()),
        JsonValue::UInt64(v) => out.push_str(&v.to_string()),
        JsonValue::Real(v) => {
            let rendered = format_g(*v, 17);
            if rendered.is_empty() {
                out.push_str("null");
            } else {
                out.push_str(&rendered);
            }
        }
        JsonValue::Str(text) => out.push_str(&json_escape(text)),
        JsonValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&json_escape(key));
                out.push(':');
                write_value(item, out);
            }
            out.push('}');
        }
    }
}

/// A byte-oriented recursive-descent JSON reader — the `Json::Reader`
/// subset `MLDBMetadataStream::setJSON` needs. Numbers map to the same
/// variant jsoncpp would pick: integer tokens widen through
/// Int/UInt/Int64/UInt64, anything with a point or exponent becomes a
/// real.
fn parse_json(text: &str) -> Option<JsonValue> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let value = parse_value(bytes, &mut pos)?;
    skip_ws(bytes, &mut pos);
    if pos != bytes.len() {
        return None;
    }
    Some(value)
}

fn skip_ws(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

fn parse_value(bytes: &[u8], pos: &mut usize) -> Option<JsonValue> {
    skip_ws(bytes, pos);
    match *bytes.get(*pos)? {
        b'n' => literal(bytes, pos, "null", JsonValue::Null),
        b't' => literal(bytes, pos, "true", JsonValue::Bool(true)),
        b'f' => literal(bytes, pos, "false", JsonValue::Bool(false)),
        b'"' => parse_string(bytes, pos).map(JsonValue::Str),
        b'[' => parse_array(bytes, pos),
        b'{' => parse_object(bytes, pos),
        b'-' | b'0'..=b'9' => parse_number(bytes, pos),
        _ => None,
    }
}

fn literal(bytes: &[u8], pos: &mut usize, word: &str, value: JsonValue) -> Option<JsonValue> {
    if bytes[*pos..].starts_with(word.as_bytes()) {
        *pos += word.len();
        Some(value)
    } else {
        None
    }
}

fn parse_string(bytes: &[u8], pos: &mut usize) -> Option<String> {
    if bytes.get(*pos) != Some(&b'"') {
        return None;
    }
    *pos += 1;
    let mut out = Vec::new();
    loop {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        match byte {
            b'"' => return String::from_utf8(out).ok(),
            b'\\' => {
                let escape = *bytes.get(*pos)?;
                *pos += 1;
                match escape {
                    b'"' => out.push(b'"'),
                    b'\\' => out.push(b'\\'),
                    b'/' => out.push(b'/'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => {
                        let hex = std::str::from_utf8(bytes.get(*pos..*pos + 4)?).ok()?;
                        *pos += 4;
                        // BMP only — jsoncpp resolves surrogate pairs; the
                        // metadata JSON this reader accepts never carries
                        // astral code points in practice.
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        let ch = char::from_u32(code).unwrap_or('\u{fffd}');
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    _ => return None,
                }
            }
            _ => out.push(byte),
        }
    }
}

fn parse_array(bytes: &[u8], pos: &mut usize) -> Option<JsonValue> {
    *pos += 1; // '['
    let mut items = Vec::new();
    skip_ws(bytes, pos);
    if bytes.get(*pos) == Some(&b']') {
        *pos += 1;
        return Some(JsonValue::Array(items));
    }
    loop {
        items.push(parse_value(bytes, pos)?);
        skip_ws(bytes, pos);
        match *bytes.get(*pos)? {
            b',' => *pos += 1,
            b']' => {
                *pos += 1;
                return Some(JsonValue::Array(items));
            }
            _ => return None,
        }
    }
}

fn parse_object(bytes: &[u8], pos: &mut usize) -> Option<JsonValue> {
    *pos += 1; // '{'
    let mut map = BTreeMap::new();
    skip_ws(bytes, pos);
    if bytes.get(*pos) == Some(&b'}') {
        *pos += 1;
        return Some(JsonValue::Object(map));
    }
    loop {
        skip_ws(bytes, pos);
        let key = parse_string(bytes, pos)?;
        skip_ws(bytes, pos);
        if bytes.get(*pos) != Some(&b':') {
            return None;
        }
        *pos += 1;
        let value = parse_value(bytes, pos)?;
        map.insert(key, value);
        skip_ws(bytes, pos);
        match *bytes.get(*pos)? {
            b',' => *pos += 1,
            b'}' => {
                *pos += 1;
                return Some(JsonValue::Object(map));
            }
            _ => return None,
        }
    }
}

fn parse_number(bytes: &[u8], pos: &mut usize) -> Option<JsonValue> {
    let start = *pos;
    if bytes.get(*pos) == Some(&b'-') {
        *pos += 1;
    }
    while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
        *pos += 1;
    }
    let mut real = false;
    if bytes.get(*pos) == Some(&b'.') {
        real = true;
        *pos += 1;
        while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
            *pos += 1;
        }
    }
    if matches!(bytes.get(*pos), Some(b'e') | Some(b'E')) {
        real = true;
        *pos += 1;
        if matches!(bytes.get(*pos), Some(b'+') | Some(b'-')) {
            *pos += 1;
        }
        while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
            *pos += 1;
        }
    }
    let token = std::str::from_utf8(&bytes[start..*pos]).ok()?;
    if real {
        token.parse::<f64>().ok().map(JsonValue::Real)
    } else if let Ok(v) = token.parse::<i32>() {
        Some(JsonValue::Int(v))
    } else if let Ok(v) = token.parse::<u32>() {
        Some(JsonValue::UInt(v))
    } else if let Ok(v) = token.parse::<i64>() {
        Some(JsonValue::Int64(v))
    } else if let Ok(v) = token.parse::<u64>() {
        Some(JsonValue::UInt64(v))
    } else {
        token.parse::<f64>().ok().map(JsonValue::Real)
    }
}

/// `utf8ParseLeadByte` (`string_utils.cpp:73-99`): the sequence length a
/// lead byte promises, 0 for an invalid one.
fn utf8_parse_lead_byte(byte: u8) -> usize {
    if byte & 0x80 == 0 {
        1
    } else if byte & 0xe0 == 0xc0 {
        2
    } else if byte & 0xf0 == 0xe0 {
        3
    } else if byte & 0xf8 == 0xf0 {
        4
    } else {
        0
    }
}

fn is_continuation_byte(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// `findNextNonUtf8Char` — the index of the first invalid position, if any.
fn find_next_non_utf8_char(bytes: &[u8]) -> Option<usize> {
    let mut pos = 0usize;
    while pos < bytes.len() {
        let count = utf8_parse_lead_byte(bytes[pos]);
        if count == 0 {
            return Some(pos);
        }
        if count > 1 {
            for i in 1..count {
                if pos + i >= bytes.len() || !is_continuation_byte(bytes[pos + i]) {
                    return Some(pos);
                }
            }
        }
        pos += count;
    }
    None
}

/// `LogMetadataBuilder::cleanString` (`string_utils.cpp:153-186`): runs
/// over the wire bytes (a `BW::string` is byte storage — the C++ walks it
/// as a C string, so the first NUL byte ends it). Every invalid lead byte
/// is replaced by its `byteToString[value - 128]` printable name
/// (`"0xNN"`, uppercase) and the walk continues after it.
pub fn clean_string(bytes: &[u8]) -> String {
    let cut = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let bytes = &bytes[..cut];
    if find_next_non_utf8_char(bytes).is_none() {
        return String::from_utf8_lossy(bytes).into_owned();
    }

    let mut out = String::new();
    let mut pos = 0usize;
    while let Some(bad) = find_next_non_utf8_char(&bytes[pos..]) {
        let abs = pos + bad;
        out.push_str(&String::from_utf8_lossy(&bytes[pos..abs]));
        out.push_str(&format!("0x{:02X}", bytes[abs]));
        pos = abs + 1;
    }
    out.push_str(&String::from_utf8_lossy(&bytes[pos..]));
    out
}

/// `MLDBMetadataStream` (`metadata.hpp:32-48`): accumulates key/value
/// pairs into a JSON object until `set_json` replaces the whole thing and
/// locks it.
#[derive(Clone, Debug, Default)]
pub struct MldbMetadataStream {
    json_added: bool,
    root: JsonValue,
}

impl MldbMetadataStream {
    pub fn new() -> Self {
        Self {
            json_added: false,
            root: JsonValue::Null,
        }
    }
    /// Assign `root_[key] = value` — promotes a null root to an object,
    /// ignored once `setJSON` succeeded. No cleaning happens here: the C++
    /// `add` overloads take `key.c_str()` verbatim; the builder cleans
    /// before calling.
    fn assign(&mut self, key: &str, value: JsonValue) {
        if self.json_added {
            return;
        }
        match &mut self.root {
            JsonValue::Object(map) => {
                map.insert(key.to_string(), value);
            }
            _ => {
                let mut map = BTreeMap::new();
                map.insert(key.to_string(), value);
                self.root = JsonValue::Object(map);
            }
        }
    }

    /// `add( key, int )`.
    pub fn add_int(&mut self, key: &str, value: i32) -> &mut Self {
        self.assign(key, JsonValue::Int(value));
        self
    }

    /// `add( key, unsigned int )`.
    pub fn add_uint(&mut self, key: &str, value: u32) -> &mut Self {
        self.assign(key, JsonValue::UInt(value));
        self
    }

    /// `add( key, long long int )`.
    pub fn add_int64(&mut self, key: &str, value: i64) -> &mut Self {
        self.assign(key, JsonValue::Int64(value));
        self
    }

    /// `add( key, float )` — jsoncpp has no float slot; the value
    /// promotes to double.
    pub fn add_float(&mut self, key: &str, value: f32) -> &mut Self {
        self.assign(key, JsonValue::Real(value as f64));
        self
    }

    /// `add( key, double )`.
    pub fn add_double(&mut self, key: &str, value: f64) -> &mut Self {
        self.assign(key, JsonValue::Real(value));
        self
    }

    /// `add( key, const BW::string & )` — stores `value.c_str()`, i.e.
    /// truncated at the first NUL, no cleaning.
    pub fn add_string(&mut self, key: &str, value: &str) -> &mut Self {
        let cut = value.find('\0').unwrap_or(value.len());
        self.assign(key, JsonValue::Str(value[..cut].to_string()));
        self
    }

    /// `setJSON`: parse a complete JSON document, replacing everything;
    /// a successful parse blocks all further additions.
    pub fn set_json(&mut self, json: &str) -> bool {
        if self.json_added {
            return true;
        }
        match parse_json(json) {
            Some(value) => {
                self.root = value;
                self.json_added = true;
                true
            }
            None => false,
        }
    }

    /// `getJSON`: the FastWriter image ready for the MLDB file.
    pub fn get_json(&self) -> String {
        fast_writer_json(&self.root)
    }

    /// Whether `set_json` locked the stream.
    pub fn json_added(&self) -> bool {
        self.json_added
    }
}

/// `LogMetadataBuilder::process` (`metadata.cpp:226-330`): drain the wire
/// tail into the output stream, key-first with `MLVT_STRING` enforced,
/// bailing on the first read error and letting a valid `__bw_json` payload
/// short-circuit everything else.
pub fn process(is: &mut MemoryIStream, out: &mut MldbMetadataStream) {
    while is.remaining() > 0 {
        let key_type = match is.read_u8() {
            Some(t) => t,
            None => return,
        };
        if key_type != mlvt::STRING {
            // Key expected as string; terminate metadata processing.
            return;
        }
        let key = clean_string(&is.read_string_bytes());
        let value_type = match is.read_u8() {
            Some(t) => t,
            None => return,
        };

        if value_type == mlvt::JSON_STRING {
            let buffer = clean_string(&is.read_string_bytes());
            if out.set_json(&buffer) {
                // A valid JSON argument overrides all others.
                return;
            }
            continue;
        }

        match value_type {
            mlvt::INT => {
                let Some(raw) = is.retrieve(4) else { return };
                out.add_int(&key, i32::from_le_bytes(raw.try_into().unwrap()));
            }
            mlvt::LONG_LONG_INT => {
                let Some(raw) = is.retrieve(8) else { return };
                out.add_int64(&key, i64::from_le_bytes(raw.try_into().unwrap()));
            }
            mlvt::UNSIGNED_INT => {
                let Some(raw) = is.retrieve(4) else { return };
                out.add_uint(&key, u32::from_le_bytes(raw.try_into().unwrap()));
            }
            mlvt::STRING => {
                let buffer = is.read_string_bytes();
                // The value lands via c_str(): NUL-truncated, cleaned.
                let cut = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
                out.add_string(&key, &clean_string(&buffer[..cut]));
            }
            mlvt::FLOAT => {
                let Some(raw) = is.retrieve(4) else { return };
                out.add_float(&key, f32::from_le_bytes(raw.try_into().unwrap()));
            }
            mlvt::DOUBLE => {
                let Some(raw) = is.retrieve(8) else { return };
                out.add_double(&key, f64::from_le_bytes(raw.try_into().unwrap()));
            }
            _ => {
                // Unknown type: the reference stores an empty string under
                // the key and keeps going.
                out.add_string(&key, "");
            }
        }
    }
}

/// `MetadataMLDB::writeMetadataToMLDB` framing: the JSON string lands in
/// the per-segment `metadata` file as one packed-length string.
pub fn metadata_record_to_disk(metadata: &str, os: &mut MemoryOStream) {
    os.write_string(metadata);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference-written metadata file of the first sample run: one
    /// record, `{"hp":42,"ratio":2.5,"state":"ok"}` with the FastWriter
    /// newline. The second run appends `05 "null\n"` — the leftover-args
    /// artifact the `%hd` message produced (see the builder test below).
    const SAMPLE_METADATA: [u8; 36] = [
        0x23, 0x7b, 0x22, 0x68, 0x70, 0x22, 0x3a, 0x34, 0x32, 0x2c, 0x22, 0x72, 0x61, 0x74, 0x69,
        0x6f, 0x22, 0x3a, 0x32, 0x2e, 0x35, 0x2c, 0x22, 0x73, 0x74, 0x61, 0x74, 0x65, 0x22, 0x3a,
        0x22, 0x6f, 0x6b, 0x22, 0x7d, 0x0a,
    ];

    /// The wire tail make_sample sent for that record: hp=42 (int),
    /// state="ok" (string), ratio=2.5 (double).
    fn sample_wire_tail() -> Vec<u8> {
        let mut os = MemoryOStream::new();
        os.write_u8(mlvt::STRING);
        os.write_string("hp");
        os.write_u8(mlvt::INT);
        os.add_blob(&42i32.to_le_bytes());
        os.write_u8(mlvt::STRING);
        os.write_string("state");
        os.write_u8(mlvt::STRING);
        os.write_string("ok");
        os.write_u8(mlvt::STRING);
        os.write_string("ratio");
        os.write_u8(mlvt::DOUBLE);
        os.add_blob(&2.5f64.to_le_bytes());
        os.data().to_vec()
    }

    #[test]
    fn builds_the_reference_metadata_record() {
        let mut out = MldbMetadataStream::new();
        let tail = sample_wire_tail();
        let mut is = MemoryIStream::new(&tail);
        process(&mut is, &mut out);
        let json = out.get_json();
        assert_eq!(json, "{\"hp\":42,\"ratio\":2.5,\"state\":\"ok\"}\n");

        // Framing: packed length prefix, then the JSON — byte-identical to
        // the reference file.
        let mut os = MemoryOStream::new();
        metadata_record_to_disk(&json, &mut os);
        assert_eq!(os.data(), SAMPLE_METADATA.as_slice());
    }

    #[test]
    fn empty_tail_stays_null_and_writes_the_artifact_record() {
        // No pairs: the root stays null, FastWriter prints "null\n".
        let out = MldbMetadataStream::new();
        assert_eq!(out.get_json(), "null\n");

        // The sample2 artifact: two leftover bytes from a short-consumed
        // `%hd` argument trip the `remaining > 1` check, the builder reads
        // a bogus key type and stops, and the null JSON is what lands on
        // disk.
        let mut out2 = MldbMetadataStream::new();
        let mut is = MemoryIStream::new(&[0x00, 0x00]);
        process(&mut is, &mut out2);
        assert_eq!(out2.get_json(), "null\n");
        let mut os = MemoryOStream::new();
        metadata_record_to_disk(&out2.get_json(), &mut os);
        assert_eq!(os.data(), [0x05, b'n', b'u', b'l', b'l', b'\n']);
    }

    #[test]
    fn covers_every_wire_value_type() {
        let mut tail = MemoryOStream::new();
        tail.write_u8(mlvt::STRING);
        tail.write_string("i");
        tail.write_u8(mlvt::INT);
        tail.add_blob(&(-7i32).to_le_bytes());
        tail.write_u8(mlvt::STRING);
        tail.write_string("u");
        tail.write_u8(mlvt::UNSIGNED_INT);
        tail.add_blob(&4000000000u32.to_le_bytes());
        tail.write_u8(mlvt::STRING);
        tail.write_string("l");
        tail.write_u8(mlvt::LONG_LONG_INT);
        tail.add_blob(&(-5000000000i64).to_le_bytes());
        tail.write_u8(mlvt::STRING);
        tail.write_string("s");
        tail.write_u8(mlvt::STRING);
        tail.write_string("twö");
        tail.write_u8(mlvt::STRING);
        tail.write_string("f");
        tail.write_u8(mlvt::FLOAT);
        tail.add_blob(&1.5f32.to_le_bytes());
        tail.write_u8(mlvt::STRING);
        tail.write_string("d");
        tail.write_u8(mlvt::DOUBLE);
        tail.add_blob(&0.1f64.to_le_bytes());

        let mut out = MldbMetadataStream::new();
        process(&mut MemoryIStream::new(tail.data()), &mut out);
        assert_eq!(
            out.get_json(),
            "{\"d\":0.10000000000000001,\"f\":1.5,\"i\":-7,\"l\":-5000000000,\
             \"s\":\"twö\",\"u\":4000000000}\n"
        );
    }

    #[test]
    fn json_payload_overrides_everything() {
        let mut tail = MemoryOStream::new();
        tail.write_u8(mlvt::STRING);
        tail.write_string("plain");
        tail.write_u8(mlvt::INT);
        tail.add_blob(&1i32.to_le_bytes());
        tail.write_u8(mlvt::STRING);
        tail.write_string(KEY_JSON_STRING);
        tail.write_u8(mlvt::JSON_STRING);
        tail.write_string("[1, 2.5, \"x\", true, null]");
        // Everything after a successful setJSON is ignored.
        tail.write_u8(mlvt::STRING);
        tail.write_string("later");
        tail.write_u8(mlvt::STRING);
        tail.write_string("gone");

        let mut out = MldbMetadataStream::new();
        process(&mut MemoryIStream::new(tail.data()), &mut out);
        assert_eq!(out.get_json(), "[1,2.5,\"x\",true,null]\n");
        assert!(out.json_added());

        // A failed JSON parse does not lock the stream; processing
        // continues with later pairs.
        let mut tail2 = MemoryOStream::new();
        tail2.write_u8(mlvt::STRING);
        tail2.write_string(KEY_JSON_STRING);
        tail2.write_u8(mlvt::JSON_STRING);
        tail2.write_string("{not json");
        tail2.write_u8(mlvt::STRING);
        tail2.write_string("after");
        tail2.write_u8(mlvt::INT);
        tail2.add_blob(&3i32.to_le_bytes());
        let mut out2 = MldbMetadataStream::new();
        process(&mut MemoryIStream::new(tail2.data()), &mut out2);
        assert!(!out2.json_added());
        assert_eq!(out2.get_json(), "{\"after\":3}\n");

        // JSON under a different key is accepted but warned about in the
        // reference; the override still applies.
        let mut tail3 = MemoryOStream::new();
        tail3.write_u8(mlvt::STRING);
        tail3.write_string("other");
        tail3.write_u8(mlvt::JSON_STRING);
        tail3.write_string("{\"k\":1}");
        let mut out3 = MldbMetadataStream::new();
        process(&mut MemoryIStream::new(tail3.data()), &mut out3);
        assert_eq!(out3.get_json(), "{\"k\":1}\n");
    }

    #[test]
    fn non_string_key_type_terminates_processing() {
        let mut tail = MemoryOStream::new();
        tail.write_u8(mlvt::INT); // Bogus key type.
        tail.write_string("nope");
        tail.write_u8(mlvt::STRING);
        tail.write_string("later");
        tail.write_u8(mlvt::INT);
        tail.add_blob(&9i32.to_le_bytes());

        let mut out = MldbMetadataStream::new();
        process(&mut MemoryIStream::new(tail.data()), &mut out);
        // Nothing was consumed into the object.
        assert_eq!(out.get_json(), "null\n");
    }

    #[test]
    fn unknown_value_type_stores_an_empty_string() {
        let mut tail = MemoryOStream::new();
        tail.write_u8(mlvt::STRING);
        tail.write_string("odd");
        tail.write_u8(b'z');
        tail.write_u8(mlvt::STRING);
        tail.write_string("next");
        tail.write_u8(mlvt::INT);
        tail.add_blob(&2i32.to_le_bytes());

        let mut out = MldbMetadataStream::new();
        process(&mut MemoryIStream::new(tail.data()), &mut out);
        assert_eq!(out.get_json(), "{\"next\":2,\"odd\":\"\"}\n");
    }

    #[test]
    fn truncates_strings_and_keys_at_nul_like_c_str() {
        // BW::string is byte storage: an embedded NUL byte is a legitimate
        // byte, but jsoncpp's c_str() walk stops there.
        let mut tail = MemoryOStream::new();
        tail.write_u8(mlvt::STRING);
        tail.write_string("ke\u{0}ygone");
        tail.write_u8(mlvt::STRING);
        tail.write_string("va\u{0}lgone");

        let mut out = MldbMetadataStream::new();
        process(&mut MemoryIStream::new(tail.data()), &mut out);
        assert_eq!(out.get_json(), "{\"ke\":\"va\"}\n");
    }

    #[test]
    fn clean_string_replaces_bad_utf8_lead_bytes() {
        assert_eq!(clean_string(b"plain"), "plain");
        // One stray 0xFF byte becomes its printable name.
        assert_eq!(clean_string(b"a\xffb"), "a0xFFb");
        // A truncated two-byte sequence: the lead is replaced, the walk
        // continues at the continuation byte, which is invalid as a lead.
        assert_eq!(clean_string(b"a\xc3"), "a0xC3");
        assert_eq!(clean_string(b"a\xc3db"), "a0xC3db");
        // A continuation byte is never a valid lead.
        assert_eq!(clean_string(b"\x805"), "0x805");
        // Valid multi-byte sequences pass through untouched.
        assert_eq!(clean_string("twö".as_bytes()), "twö");
        assert_eq!(clean_string(b""), "");
        // The C walk stops at the first NUL byte.
        assert_eq!(clean_string(b"ab\0cd"), "ab");
    }

    #[test]
    fn json_escape_matches_jsoncpp() {
        assert_eq!(json_escape("hi"), "\"hi\"");
        assert_eq!(json_escape("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_escape("a\nb\tc"), "\"a\\nb\\tc\"");
        // C0 bytes render lowercase \u00xx.
        assert_eq!(json_escape("\u{1}"), "\"\\u0001\"");
        assert_eq!(json_escape("\u{1f}"), "\"\\u001f\"");
        // A NUL byte truncates the string, exactly like the C walk.
        assert_eq!(json_escape("ab\0cd"), "\"ab\"");
    }

    #[test]
    #[allow(clippy::approx_constant)] // the case under test is this literal
    fn real_formatting_reproduces_printf_g17() {
        assert_eq!(format_g(2.5, 17), "2.5");
        assert_eq!(format_g(0.1, 17), "0.10000000000000001");
        assert_eq!(format_g(3.14159, 17), "3.1415899999999999");
        assert_eq!(format_g(0.0, 17), "0");
        assert_eq!(format_g(-0.0, 17), "-0");
        assert_eq!(format_g(1e20, 17), "1e+20");
        // 1e-7 is 9.999999999999999547…e-8 exactly; 17 significant digits
        // round to ...95 and the exponent leaves the -4 floor.
        assert_eq!(format_g(1e-7, 17), "9.9999999999999995e-08");
        assert_eq!(format_g(-42.25, 17), "-42.25");
        // Non-finite reals serialize as null.
        assert!(format_g(f64::NAN, 17).is_empty());
        assert!(format_g(f64::INFINITY, 17).is_empty());
    }

    #[test]
    fn json_reader_maps_number_widths_like_jsoncpp() {
        assert_eq!(parse_json("42"), Some(JsonValue::Int(42)));
        assert_eq!(parse_json("4000000000"), Some(JsonValue::UInt(4000000000)));
        assert_eq!(
            parse_json("-5000000000"),
            Some(JsonValue::Int64(-5000000000))
        );
        assert_eq!(
            parse_json("18446744073709551615"),
            Some(JsonValue::UInt64(u64::MAX))
        );
        assert_eq!(parse_json("2.5"), Some(JsonValue::Real(2.5)));
        assert_eq!(parse_json("-2.5e3"), Some(JsonValue::Real(-2500.0)));
        assert_eq!(
            parse_json("{\"a\":[1,{\"b\":null}],\"c\":true}"),
            Some(JsonValue::Object({
                let mut inner = BTreeMap::new();
                inner.insert("b".to_string(), JsonValue::Null);
                let mut map = BTreeMap::new();
                map.insert(
                    "a".to_string(),
                    JsonValue::Array(vec![JsonValue::Int(1), JsonValue::Object(inner)]),
                );
                map.insert("c".to_string(), JsonValue::Bool(true));
                map
            }))
        );
        assert_eq!(parse_json("{nope}"), None);
        assert_eq!(parse_json("trailing junk"), None);
    }

    #[test]
    fn set_json_roundtrips_through_the_writer() {
        let mut out = MldbMetadataStream::new();
        assert!(out.set_json("{\"z\":1,\"a\":[1,2]}"));
        // Keys re-sort into map order on the way out.
        assert_eq!(out.get_json(), "{\"a\":[1,2],\"z\":1}\n");
        // Locked: later adds are ignored.
        out.add_int("b", 2);
        assert_eq!(out.get_json(), "{\"a\":[1,2],\"z\":1}\n");
    }
}
