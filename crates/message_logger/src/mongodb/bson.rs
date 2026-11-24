//! The BSON document the backend writes
//! (`mongo::BSONObj`/`BSONObjBuilder` in `lib/mongodb/...`, used through
//! `mongo/client/dbclient.h`).
//!
//! `BsonObj` is an ordered element list, exactly like the driver's object:
//! fields keep the order `append` put them in, and duplicates are legal
//! (the reference never relies on that, but neither does it forbid it).
//! [`BsonObj::encode`] emits the driver's wire bytes and
//! [`BsonObj::decode`] reads them back, so the storage path can be checked
//! against real BSON rather than against a Rust data structure.
//!
//! The element types present are the ones the backend emits: `double`,
//! `string`, embedded `object`, `bool`, `date` (millisecond `int64`),
//! `int` and `long`. Nothing else appears in
//! `server/tools/message_logger/mongodb/`.

/// One BSON value.
#[derive(Clone, PartialEq, Debug)]
pub enum Bson {
    /// `double` (type `0x01`).
    Double(f64),
    /// `string` (type `0x02`).
    Str(String),
    /// Embedded `object` (type `0x03`).
    Doc(BsonObj),
    /// `bool` (type `0x08`).
    Bool(bool),
    /// `date` (type `0x09`): milliseconds since the epoch. The driver's
    /// `Date_t` is an `int64` of milliseconds.
    Date(i64),
    /// `int` (type `0x10`): `int32`.
    Int(i32),
    /// `long` (type `0x12`): `int64`.
    Long(i64),
    /// BSON `null` (type `0x0A`) — the driver emits one for a missing field.
    Null,
}

impl Bson {
    /// `getElement().date().toMsecs()` / the raw `int64` behind a date.
    pub fn as_date_millis(&self) -> Option<i64> {
        match self {
            Bson::Date(ms) => Some(*ms),
            _ => None,
        }
    }

    /// `getElement().int()` — a numeric field read back as an `int32`.
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Bson::Int(v) => Some(*v),
            Bson::Long(v) => (*v).try_into().ok(),
            _ => None,
        }
    }

    /// `getElement().numberDouble()`.
    pub fn as_double(&self) -> Option<f64> {
        match self {
            Bson::Double(v) => Some(*v),
            _ => None,
        }
    }

    /// `getElement().string()` / `getStringField`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Bson::Str(v) => Some(v),
            _ => None,
        }
    }

    /// `getElement().bool()`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Bson::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// `getElement().embeddedObjectUserCheck()`.
    pub fn as_doc(&self) -> Option<&BsonObj> {
        match self {
            Bson::Doc(v) => Some(v),
            _ => None,
        }
    }

    /// The wire type byte.
    fn type_byte(&self) -> u8 {
        match self {
            Bson::Double(_) => 0x01,
            Bson::Str(_) => 0x02,
            Bson::Doc(_) => 0x03,
            Bson::Bool(_) => 0x08,
            Bson::Date(_) => 0x09,
            Bson::Int(_) => 0x10,
            Bson::Long(_) => 0x12,
            Bson::Null => 0x0A,
        }
    }

    /// The encoded value payload, without its type byte.
    fn encode_value(&self, out: &mut Vec<u8>) {
        match self {
            Bson::Double(v) => out.extend_from_slice(&v.to_bits().to_le_bytes()),
            Bson::Str(v) => {
                let bytes = v.as_bytes();
                out.extend_from_slice(&(bytes.len() as i32 + 1).to_le_bytes());
                out.extend_from_slice(bytes);
                out.push(0);
            }
            Bson::Doc(v) => out.extend_from_slice(v.encode().as_slice()),
            Bson::Bool(v) => out.push(*v as u8),
            Bson::Date(v) => out.extend_from_slice(&v.to_le_bytes()),
            Bson::Int(v) => out.extend_from_slice(&v.to_le_bytes()),
            Bson::Long(v) => out.extend_from_slice(&v.to_le_bytes()),
            Bson::Null => out.push(0),
        }
    }

    /// Decode the value payload that follows a type byte.
    fn decode_value(ty: u8, input: &[u8], offset: &mut usize) -> Option<Self> {
        match ty {
            0x01 => Some(Bson::Double(f64::from_bits(u64::from_le_bytes(
                take_le(input, offset, 8)?.try_into().ok()?,
            )))),
            0x02 => Some(Bson::Str(read_cstring(input, offset)?)),
            0x03 => {
                // The embedded document's size prefix counts itself, so the
                // whole document starts where the prefix does — and the
                // caller's cursor must step over all of it.
                let mut start = *offset;
                let len = i32::from_le_bytes(take_le(input, offset, 4)?.try_into().ok()?);
                if len < 5 {
                    return None;
                }
                let doc = BsonObj::decode(take_le(input, &mut start, len as usize)?)?;
                *offset = start;
                Some(Bson::Doc(doc))
            }
            0x08 => Some(Bson::Bool(take_le(input, offset, 1)?[0] != 0)),
            0x09 => Some(Bson::Date(i64::from_le_bytes(
                take_le(input, offset, 8)?.try_into().ok()?,
            ))),
            0x10 => Some(Bson::Int(i32::from_le_bytes(
                take_le(input, offset, 4)?.try_into().ok()?,
            ))),
            0x12 => Some(Bson::Long(i64::from_le_bytes(
                take_le(input, offset, 8)?.try_into().ok()?,
            ))),
            0x0A => {
                if take_le(input, offset, 1)?[0] != 0 {
                    return None;
                }
                Some(Bson::Null)
            }
            _ => None,
        }
    }
}

/// A length-prefixed, zero-terminated wire string at the position.
fn read_cstring(input: &[u8], offset: &mut usize) -> Option<String> {
    let bytes = take_le(input, offset, 4)?;
    let len = i32::from_le_bytes(bytes.try_into().ok()?);
    if len <= 0 {
        return None;
    }
    let body = take_le(input, offset, (len - 1) as usize)?;
    let nul = take_le(input, offset, 1)?;
    if nul[0] != 0 {
        return None;
    }
    Some(String::from_utf8_lossy(body).into_owned())
}

/// An ordered BSON document (`mongo::BSONObj`).
#[derive(Clone, Default, PartialEq, Debug)]
pub struct BsonObj {
    elements: Vec<(String, Bson)>,
}

impl BsonObj {
    /// An empty object.
    pub fn new() -> Self {
        Self::default()
    }

    /// `BSONObjBuilder::append` — add a field, in order.
    pub fn append(&mut self, key: &str, value: Bson) -> &mut Self {
        self.elements.push((key.to_string(), value));
        self
    }

    /// The number of fields.
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Whether there are no fields.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The fields in the order they were appended.
    pub fn fields(&self) -> &[(String, Bson)] {
        &self.elements
    }

    /// `getField(name)` — the last field with that name, if any.
    pub fn get(&self, name: &str) -> Option<&Bson> {
        self.elements
            .iter()
            .rev()
            .find_map(|(k, v)| (k == name).then_some(v))
    }

    /// Whether the document has a field of that name.
    pub fn has_field(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// `getIntField(name)`: zero for a missing or non-numeric field.
    pub fn get_int(&self, name: &str) -> i32 {
        self.get(name).and_then(Bson::as_int).unwrap_or(0)
    }

    /// `getStringField(name)`: empty for a missing or non-string field.
    pub fn get_string(&self, name: &str) -> String {
        self.get(name)
            .and_then(Bson::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// `getDateField(name)`: the millisecond count, or zero.
    pub fn get_date(&self, name: &str) -> i64 {
        self.get(name).and_then(Bson::as_date_millis).unwrap_or(0)
    }

    /// `BSONObjBuilder::obj()` / the wire form: a size-prefixed element list
    /// terminated by a zero byte.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for (key, value) in &self.elements {
            body.push(value.type_byte());
            let name = key.as_bytes();
            body.extend_from_slice(name);
            body.push(0);
            value.encode_value(&mut body);
        }
        let total = (body.len() + 5) as i32;
        let mut out = Vec::with_capacity(body.len() + 5);
        out.extend_from_slice(&total.to_le_bytes());
        out.extend_from_slice(&body);
        out.push(0);
        out
    }

    /// Read a wire document. Returns `None` for a truncated or malformed one.
    pub fn decode(input: &[u8]) -> Option<Self> {
        let mut offset = 0usize;
        let total = i32::from_le_bytes(take_le(input, &mut offset, 4)?.try_into().ok()?);
        if total < 5 || total as usize > input.len() {
            return None;
        }
        let end = total as usize;
        let mut out = Self::default();
        while offset + 1 < end {
            let ty = take_le(input, &mut offset, 1)?[0];
            let mut name_end = offset;
            while name_end < end && input[name_end] != 0 {
                name_end += 1;
            }
            if name_end >= end {
                return None;
            }
            let key = String::from_utf8_lossy(&input[offset..name_end]).into_owned();
            offset = name_end + 1;
            let value = Bson::decode_value(ty, input, &mut offset)?;
            out.elements.push((key, value));
        }
        if offset != end - 1 || input[end - 1] != 0 {
            return None;
        }
        Some(out)
    }

    /// `BSONObj::jsonString()`: the compact form the reference prints to
    /// stdout in `--stdout` mode.
    pub fn json_string(&self) -> String {
        let mut out = String::from("{");
        for (index, (key, value)) in self.elements.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&json_string_literal(key));
            out.push(':');
            out.push_str(&value.json());
        }
        out.push('}');
        out
    }

    /// Append every field of `other` to this object
    /// (`BSONObjBuilder::appendElements`).
    pub fn append_elements(&mut self, other: &BsonObj) {
        self.elements.extend(other.elements.iter().cloned());
    }
}

impl Bson {
    /// One value's `jsonString` rendering.
    fn json(&self) -> String {
        match self {
            Bson::Double(v) => format_double(*v),
            Bson::Str(v) => json_string_literal(v),
            Bson::Doc(v) => v.json_string(),
            Bson::Bool(v) => v.to_string(),
            Bson::Date(ms) => format!("{{Date({ms})}}"),
            Bson::Int(v) => v.to_string(),
            Bson::Long(v) => format!("L\"{v}\""),
            Bson::Null => "null".to_string(),
        }
    }
}

/// A four-byte little-endian scalar at the stream position.
fn take_le<'a>(input: &'a [u8], offset: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = *offset + n;
    if end > input.len() {
        return None;
    }
    let slice = &input[*offset..end];
    *offset = end;
    Some(slice)
}

/// A JSON string literal: the escapes `mongo::fromjson`/`jsonString` use.
pub fn json_string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        let escaped = match ch {
            '"' => "\\\"",
            '\\' => "\\\\",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
                continue;
            }
            _ => {
                out.push(ch);
                continue;
            }
        };
        out.push_str(escaped);
    }
    out.push('"');
    out
}

/// The driver's `%g`-style double rendering, close enough for the
/// `--stdout` dump and `jsonString`.
fn format_double(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// Parse the JSON text `setJSON` accepts (`mongo::fromjson`,
/// `mongodb/metadata.cpp:125-142`). Only the object form the metadata
/// builder can produce: `{ "key": <string|number|true|false|null> }` with
/// nested objects. Returns `None` for anything the driver would throw
/// `MsgAssertionException` on, which is how the reference signals invalid
/// JSON.
pub fn from_json(text: &str) -> Option<BsonObj> {
    let mut cursor = Cursor::new(text);
    cursor.skip_ws();
    let doc = cursor.parse_object()?;
    cursor.skip_ws();
    if !cursor.at_end() {
        return None;
    }
    Some(doc)
}

struct Cursor<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn expect(&mut self, want: u8) -> bool {
        self.bump() == Some(want)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn parse_string(&mut self) -> Option<String> {
        if !self.expect(b'"') {
            return None;
        }
        // Bytes, not chars: raw UTF-8 passes through, escapes and \uXXXX
        // decode into their UTF-8 bytes.
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = self.bump()?;
            match byte {
                b'"' => break,
                b'\\' => match self.bump()? {
                    b'"' => out.push(b'"'),
                    b'\\' => out.push(b'\\'),
                    b'/' => out.push(b'/'),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'u' => {
                        let code = self.hex4()? as u32;
                        // A UTF-16 surrogate pair resolves to one scalar.
                        let ch = if (0xd800..0xdc00).contains(&code) {
                            if !(self.expect(b'\\') && self.expect(b'u')) {
                                return None;
                            }
                            let low = self.hex4()? as u32;
                            if !(0xdc00..0xe000).contains(&low) {
                                return None;
                            }
                            char::from_u32(0x1_0000 + ((code - 0xd800) << 10) + (low - 0xdc00))?
                        } else if (0xdc00..0xe000).contains(&code) {
                            return None;
                        } else {
                            char::from_u32(code)?
                        };
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    _ => return None,
                },
                _ => out.push(byte),
            }
        }
        String::from_utf8(out).ok()
    }

    fn hex4(&mut self) -> Option<u16> {
        let mut value: u32 = 0;
        for _ in 0..4 {
            let digit = match self.bump()? {
                b @ b'0'..=b'9' => (b - b'0') as u32,
                b @ b'a'..=b'f' => (b - b'a' + 10) as u32,
                b @ b'A'..=b'F' => (b - b'A' + 10) as u32,
                _ => return None,
            };
            value = value * 16 + digit;
        }
        Some(value as u16)
    }

    fn parse_value(&mut self) -> Option<Bson> {
        self.skip_ws();
        match self.peek()? {
            b'{' => Some(Bson::Doc(self.parse_object()?)),
            b'"' => Some(Bson::Str(self.parse_string()?)),
            b't' => {
                if !self.word("true") {
                    return None;
                }
                Some(Bson::Bool(true))
            }
            b'f' => {
                if !self.word("false") {
                    return None;
                }
                Some(Bson::Bool(false))
            }
            b'n' => {
                if !self.word("null") {
                    return None;
                }
                Some(Bson::Null)
            }
            _ => self.parse_number(),
        }
    }

    fn word(&mut self, want: &str) -> bool {
        for byte in want.bytes() {
            if self.bump() != Some(byte) {
                return false;
            }
        }
        true
    }

    fn parse_number(&mut self) -> Option<Bson> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let mut digits = 0;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
            digits += 1;
        }
        let mut float = false;
        if self.peek() == Some(b'.') {
            float = true;
            self.pos += 1;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.input[start..self.pos]).ok()?;
        if digits == 0 {
            return None;
        }
        if float {
            Some(Bson::Double(text.parse().ok()?))
        } else {
            match text.parse::<i64>() {
                Ok(v) if v >= i32::MIN as i64 && v <= i32::MAX as i64 => Some(Bson::Int(v as i32)),
                Ok(v) => Some(Bson::Long(v)),
                Err(_) => Some(Bson::Double(text.parse().ok()?)),
            }
        }
    }

    fn parse_object(&mut self) -> Option<BsonObj> {
        if !self.expect(b'{') {
            return None;
        }
        let mut doc = BsonObj::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Some(doc);
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if !self.expect(b':') {
                return None;
            }
            doc.append(&key, self.parse_value()?);
            self.skip_ws();
            match self.bump()? {
                b',' => continue,
                b'}' => return Some(doc),
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_round_trip_their_wire_bytes() {
        let mut doc = BsonObj::new();
        doc.append("ts", Bson::Date(1_700_000_000_123));
        doc.append("cnt", Bson::Int(1));
        doc.append("ctg", Bson::Int(3));
        doc.append("msg", Bson::Str("hello".into()));
        doc.append("ok", Bson::Bool(true));
        let mut inner = BsonObj::new();
        inner.append("nested", Bson::Long(i32::MAX as i64 + 7));
        doc.append("md", Bson::Doc(inner));

        let bytes = doc.encode();
        // A size prefix, then the fields, then the terminating zero.
        assert_eq!(&bytes[..4], &(bytes.len() as i32).to_le_bytes());
        assert_eq!(bytes[bytes.len() - 1], 0);
        assert_eq!(BsonObj::decode(&bytes), Some(doc));
    }

    #[test]
    fn fields_keep_their_append_order() {
        let mut doc = BsonObj::new();
        doc.append("second", Bson::Int(2));
        doc.append("first", Bson::Int(1));
        let keys: Vec<&str> = doc.fields().iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["second", "first"]);
        // And `getField` finds either by name, order-independent.
        assert_eq!(doc.get_int("first"), 1);
        assert_eq!(doc.get_int("second"), 2);
        assert_eq!(doc.get_int("absent"), 0);
        assert_eq!(doc.get_string("absent"), "");
        assert_eq!(doc.get_date("absent"), 0);
    }

    #[test]
    fn truncated_documents_are_rejected() {
        let mut doc = BsonObj::new();
        doc.append("a", Bson::Int(1));
        let bytes = doc.encode();
        for cut in 0..bytes.len() {
            assert!(BsonObj::decode(&bytes[..cut]).is_none(), "cut at {cut}");
        }
    }

    #[test]
    fn json_parsing_matches_the_driver_subset() {
        let doc = from_json(r#"{"key1":"key","n":-7,"big":3000000000,"f":1.5,"t":true,"x":null}"#)
            .expect("valid json");
        assert_eq!(doc.get_string("key1"), "key");
        assert_eq!(doc.get_int("n"), -7);
        // 3000000000 does not fit an int32, so the driver makes it a long.
        assert_eq!(doc.get("big"), Some(&Bson::Long(3_000_000_000)));
        assert_eq!(doc.get("f"), Some(&Bson::Double(1.5)));
        assert_eq!(doc.get("t"), Some(&Bson::Bool(true)));
        assert_eq!(doc.get("x"), Some(&Bson::Null));

        let nested = from_json(r#"{"a":{"b":"c"},"d":true}"#).expect("nested object");
        let inner = nested.get("a").and_then(Bson::as_doc).expect("object");
        assert_eq!(inner.get_string("b"), "c");

        for bad in [
            "",
            "{",
            "{\"a\":}",
            "[1,2]",
            "{\"a\":1} trailing",
            "{\"a\":}",
        ] {
            assert!(from_json(bad).is_none(), "accepted {bad:?}");
        }
    }

    #[test]
    fn escaped_strings_survive_a_round_trip() {
        let text = "quote\"slash\\newline\n";
        let literal = json_string_literal(text);
        let doc = from_json(&format!("{{\"v\":{literal}}}")).expect("literal");
        assert_eq!(doc.get_string("v"), text);
    }

    #[test]
    fn the_json_dump_renders_the_compact_form() {
        let mut doc = BsonObj::new();
        doc.append("host", Bson::Int(167773459));
        doc.append("msg", Bson::Str("a\"b".into()));
        doc.append("long", Bson::Long(42));
        assert_eq!(
            doc.json_string(),
            r#"{"host":167773459,"msg":"a\"b","long":L"42"}"#
        );
    }
}
