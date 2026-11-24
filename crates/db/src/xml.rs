//! The XML backend — `scripts/db.xml` (`db_storage_xml/xml_database.cpp`).
//!
//! The reference keeps the whole database as one `DataSection` tree: one
//! child section per entity (the section name is the entity type name),
//! plus a `_BigWorldInfo` section holding the billing log-on mapping, the
//! auto-load set and the archived space data. Every entity section carries
//! a `databaseID` child, and cell-capable entities carry
//! `position`/`direction`/`spaceID` after the properties
//! (`xml_database.cpp:630-646`).
//!
//! The section conventions ported here are the reference's own:
//!
//! * values are the tag text with both ends of the whitespace stripped
//!   (`XMLSection::stripWhitespace`), and escape sequences are reduced on
//!   read and expanded on write (`xml_special_chars.cpp:15-130` — the five
//!   named entities plus `&#N;` byte codes);
//! * blobs are Base64 (`xml_section.cpp:967,1273`);
//! * sequences write one `<item>` child per element but read any children
//!   (`sequence_data_type.cpp:373-412,420-450`);
//! * fixed dicts write one child per field, with an empty section standing
//!   for `<AllowNone>` (`fixed_dict_data_type.cpp:787-860`);
//! * mailboxes write `id`/`ip`/`etc`, where `etc` is the address's
//!   `port | salt << 16` re-read as one integer
//!   (`mailbox_data_type.cpp:244-257`).
//!
//! Startup assigns database ids exactly like `initEntityMap`
//! (`xml_database.cpp:178-292`): the first pass reads every `databaseID`
//! and takes the maximum, the second pass — only when some entity had
//! none — hands out `++maxID_` in document order and writes the ids back
//! into the sections. Duplicate ids skip the second section (the reference
//! warns for non-negative ones); entities of an unknown type are ignored
//! as data but preserved in the document, as are all `_BigWorldInfo`
//! children.
//!
//! The `activeSet_` face is runtime-only: the reference never persists it,
//! so [`XmlDatabase::to_xml`] does not write check-outs back.
//!
//! Deviations, all recorded at the call site: `putEntity`'s trailing
//! position/direction/spaceID are stream-supplied in the reference (the
//! snapshot face, BW-09-04), so this face *retains* the sections it loaded
//! instead of dropping them; a type mismatch on update is
//! [`DbError::NotFound`] here, where the reference's id-only map would
//! silently retype the row (MySQL, and every task above the backend, keys
//! rows by type and id); a `values` vector that does not match the def
//! face is rejected, where the reference reads a fixed-size stream; and a
//! corrupt value fails the load, where the reference defers the failure to
//! `getEntity`.

use std::collections::{BTreeMap, BTreeSet};

use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId, GameTime, PENDING_DATABASE_ID};

use crate::auto_load::{EntityAutoLoader, UpdateAutoLoad};
use crate::error::DbError;
use crate::record::EntityRecord;
use crate::storage::{BaseMailbox, CheckOut, EntityKey, EntityStorage, StorageImage};

/// The `_BigWorldInfo` server-info section (`xml_database.cpp:34`).
pub const INFO_SECTION: &str = "_BigWorldInfo";
/// The `_BigWorldInfo` billing mapping child (`xml_database.cpp:35`).
pub const LOG_ON_MAPPING_SECTION: &str = "LogOnMapping";
/// The `_BigWorldInfo` auto-load child (`xml_database.cpp:36`).
pub const AUTO_LOAD_SECTION: &str = "AutoLoadedEntities";

// -----------------------------------------------------------------------------
// Section: the DataSection tree
// -----------------------------------------------------------------------------

/// One `DataSection` of the `db.xml` tree: a tag name, its stripped text
/// value and its child sections, in document order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XmlSection {
    /// The tag name (`DataSection::sectionName`).
    pub name: String,
    /// The tag's text with the outer whitespace stripped.
    pub value: String,
    /// The child sections, in document order.
    pub children: Vec<XmlSection>,
}

impl XmlSection {
    /// A leaf section holding `value`.
    pub fn leaf(name: &str, value: &str) -> Self {
        Self {
            name: name.to_owned(),
            value: value.to_owned(),
            children: Vec::new(),
        }
    }

    /// A section holding only children.
    pub fn branch(name: &str, children: Vec<XmlSection>) -> Self {
        Self {
            name: name.to_owned(),
            value: String::new(),
            children,
        }
    }

    /// `DataSection::findChild`: the first child named `name`.
    pub fn find(&self, name: &str) -> Option<&XmlSection> {
        self.children.iter().find(|child| child.name == name)
    }

    /// The first child named `name`, mutably.
    pub fn find_mut(&mut self, name: &str) -> Option<&mut XmlSection> {
        self.children.iter_mut().find(|child| child.name == name)
    }

    /// `DataSection::read(tag, default)` for an integer child.
    fn int_child(&self, name: &str) -> i64 {
        self.find(name)
            .and_then(|child| child.value.parse::<i64>().ok())
            .unwrap_or(0)
    }

    /// `DataSection::write` for an integer child: replace an existing one
    /// or append.
    fn write_int(&mut self, name: &str, value: i64) {
        let text = value.to_string();
        match self.find_mut(name) {
            Some(child) => child.value = text,
            None => self.children.push(Self::leaf(name, &text)),
        }
    }
}

/// Parse a `DataSection` XML document: elements with text values, nested
/// children, self-closing tags, comments and prolog skipped, attributes
/// accepted and ignored. Text is trimmed and its escape sequences reduced
/// exactly as the reference does. Anything malformed is a
/// [`DbError::Storage`].
pub fn parse_document(text: &str) -> Result<XmlSection, DbError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_ignorable()?;
    let root = parser.parse_element()?;
    parser.skip_ignorable()?;
    if parser.pos != parser.bytes.len() {
        return Err(DbError::Storage(format!(
            "db xml: trailing content at byte {}",
            parser.pos
        )));
    }
    Ok(root)
}

/// Write the tree the way `XMLSection::writeToStream` does
/// (`xml_section.cpp:2304-2394`): tab indentation, a tab between the tag
/// and its text, ` />` for empty sections and one line per section.
pub fn write_document(root: &XmlSection) -> String {
    let mut out = String::new();
    write_section(&mut out, root, 0);
    out
}

fn write_section(out: &mut String, section: &XmlSection, level: usize) {
    for _ in 0..level {
        out.push('\t');
    }
    out.push('<');
    out.push_str(&section.name);

    if section.children.is_empty() && section.value.is_empty() {
        out.push_str(" />\n");
        return;
    }

    out.push('>');
    if !section.value.is_empty() {
        out.push('\t');
        out.push_str(&expand_escapes(&section.value));
    }
    if !section.children.is_empty() {
        out.push('\n');
        for child in &section.children {
            write_section(out, child, level + 1);
        }
        for _ in 0..level {
            out.push('\t');
        }
    } else {
        out.push('\t');
    }
    out.push_str("</");
    out.push_str(&section.name);
    out.push_str(">\n");
}

/// `XmlSpecialChars::expand` (`xml_special_chars.cpp:98-130`): the five
/// named entities, and every byte outside ASCII as a `&#N;` code.
fn expand_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        match byte {
            b'&' => out.push_str("&amp;"),
            b'<' => out.push_str("&lt;"),
            b'>' => out.push_str("&gt;"),
            b'"' => out.push_str("&quot;"),
            b'\'' => out.push_str("&apos;"),
            0x80..=0xFF => out.push_str(&format!("&#{};", byte)),
            _ => out.push(byte as char),
        }
    }
    out
}

/// `XmlSpecialChars::reduce` (`xml_special_chars.cpp:15-96`): the five
/// named entities plus decimal and hex `&#N;` codes. A malformed sequence
/// is left in the text, as the reference leaves it with a warning.
fn reduce_escapes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // Collect the escape name up to ';' — at most 31 characters, the
        // reference's buffer bound (`specialLen >= 31`).
        let mut end = i + 1;
        while end < bytes.len() && bytes[end] != b';' && end - i <= 31 {
            end += 1;
        }
        if end >= bytes.len() || bytes[end] != b';' || end - i <= 2 {
            out.push(b'&');
            i += 1;
            continue;
        }
        let name = &text[i + 1..end];
        let replaced: Option<u8> = match name {
            "amp" => Some(b'&'),
            "lt" => Some(b'<'),
            "gt" => Some(b'>'),
            "quot" => Some(b'"'),
            "apos" => Some(b'\''),
            _ => {
                if let Some(code) = name.strip_prefix("#x") {
                    u32::from_str_radix(code, 16).ok().map(|v| v as u8)
                } else if let Some(code) = name.strip_prefix('#') {
                    code.parse::<u32>().ok().map(|v| v as u8)
                } else {
                    None
                }
            }
        };
        match replaced {
            Some(byte) => out.push(byte),
            None => out.extend_from_slice(&bytes[i..=end]),
        }
        i = end + 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ignorable(&mut self) -> Result<(), DbError> {
        loop {
            while self
                .bytes
                .get(self.pos)
                .is_some_and(|b| b.is_ascii_whitespace())
            {
                self.pos += 1;
            }
            if self.starts_with("<!--") {
                self.pos += 4;
                self.skip_to("unterminated comment", b"-->")?;
                continue;
            }
            if self.starts_with("<?") {
                self.pos += 2;
                self.skip_to("unterminated prolog", b">")?;
                continue;
            }
            return Ok(());
        }
    }

    fn skip_to(&mut self, message: &str, marker: &[u8]) -> Result<(), DbError> {
        let close = self
            .window()
            .windows(marker.len())
            .position(|window| window == marker)
            .ok_or_else(|| self.err(message))?;
        self.pos += close + marker.len();
        Ok(())
    }

    fn parse_element(&mut self) -> Result<XmlSection, DbError> {
        if self.bytes.get(self.pos) != Some(&b'<') {
            return Err(self.err("expected an element"));
        }
        self.pos += 1;
        let name = self.read_name()?;
        let mut section = XmlSection {
            name,
            value: String::new(),
            children: Vec::new(),
        };

        // Attributes are accepted and ignored.
        loop {
            match self.bytes.get(self.pos) {
                Some(b'>') => {
                    self.pos += 1;
                    break;
                }
                Some(b'/') if self.bytes.get(self.pos + 1) == Some(&b'>') => {
                    self.pos += 2;
                    return Ok(section);
                }
                Some(b'"') | Some(b'\'') => {
                    let quote = self.bytes[self.pos];
                    self.pos += 1;
                    let close = self
                        .window()
                        .iter()
                        .position(|&byte| byte == quote)
                        .ok_or_else(|| self.err("unterminated attribute value"))?;
                    self.pos += close + 1;
                }
                Some(_) => self.pos += 1,
                None => return Err(self.err("unterminated start tag")),
            }
        }

        let mut text = String::new();
        loop {
            let Some(open) = self.window().iter().position(|&byte| byte == b'<') else {
                return Err(self.err("unterminated element"));
            };
            text.push_str(&String::from_utf8_lossy(&self.window()[..open]));
            self.pos += open;

            if self.starts_with("<!--") {
                self.pos += 4;
                self.skip_to("unterminated comment", b"-->")?;
                continue;
            }
            if self.starts_with("</") {
                self.pos += 2;
                let close = self.read_name()?;
                if close != section.name {
                    return Err(self.err(&format!(
                        "closing </{}> does not match <{}>",
                        close, section.name
                    )));
                }
                if self.bytes.get(self.pos) != Some(&b'>') {
                    return Err(self.err("malformed close tag"));
                }
                self.pos += 1;
                section.value = reduce_escapes(text.trim());
                return Ok(section);
            }
            section.children.push(self.parse_element()?);
        }
    }

    fn read_name(&mut self) -> Result<String, DbError> {
        let len = self
            .window()
            .iter()
            .position(|byte| {
                !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'.' | b':' | b'-')
            })
            .ok_or_else(|| self.err("unterminated name"))?;
        if len == 0 {
            return Err(self.err("empty tag name"));
        }
        let name = String::from_utf8_lossy(&self.window()[..len]).into_owned();
        self.pos += len;
        Ok(name)
    }

    fn starts_with(&self, marker: &str) -> bool {
        self.window().starts_with(marker.as_bytes())
    }

    fn window(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }

    fn err(&self, message: &str) -> DbError {
        DbError::Storage(format!("db xml at byte {}: {message}", self.pos))
    }
}

// -----------------------------------------------------------------------------
// Section: Base64
// -----------------------------------------------------------------------------

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `Base64::encode` (`cstdmf/base64.cpp`): the standard alphabet with `=`
/// padding.
fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let group = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_ALPHABET[(group >> 18) as usize & 0x3F] as char);
        out.push(BASE64_ALPHABET[(group >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(group >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[group as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

/// `Base64::decode`: inverse of [`base64_encode`], whitespace tolerated.
fn base64_decode(text: &str) -> Result<Vec<u8>, DbError> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut group = 0u32;
    let mut filled = 0;
    for byte in text.bytes() {
        let Some(sextet) = (match byte {
            b'A'..=b'Z' => Some((byte - b'A') as u32),
            b'a'..=b'z' => Some((byte - b'a' + 26) as u32),
            b'0'..=b'9' => Some((byte - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }) else {
            // '=' padding and whitespace only advance nothing; any other
            // byte is a corrupt payload.
            if !matches!(byte, b'=' | b'\t' | b'\r' | b'\n' | b' ') {
                return Err(DbError::Storage(format!(
                    "db xml: invalid base64 byte {byte:#x}"
                )));
            }
            continue;
        };
        group = (group << 6) | sextet;
        filled += 1;
        if filled == 4 {
            out.push((group >> 16) as u8);
            out.push((group >> 8) as u8);
            out.push(group as u8);
            group = 0;
            filled = 0;
        }
    }
    match filled {
        0 => {}
        // Two sextets carry one byte: the first six bits and the second
        // sextet's top two.
        2 => out.push((group >> 4) as u8),
        // Three sextets carry two bytes.
        3 => {
            out.push((group >> 10) as u8);
            out.push((group >> 2) as u8);
        }
        _ => return Err(DbError::Storage("db xml: base64 group of 1".to_owned())),
    }
    Ok(out)
}

// -----------------------------------------------------------------------------
// Section: the def face the XML reader needs
// -----------------------------------------------------------------------------

/// The value shape one property takes in the XML document. This is the
/// `DataType` face the section reader needs; the def-loading face (which
/// walks `entitydef`) adapts its types onto this.
#[derive(Clone, Debug, PartialEq)]
pub enum XmlKind {
    /// An integer, signed or unsigned
    /// (`IntegerDataType`/`LongIntegerDataType`).
    Int { signed: bool },
    /// `FloatDataType` — `FLOAT32` and `FLOAT64` read alike.
    Float,
    /// `StringDataType` (and `UnicodeStringDataType`).
    String,
    /// `BlobDataType` — Base64 text.
    Blob,
    /// `VectorDataType` with `n` whitespace-separated components.
    Vector(usize),
    /// `SequenceDataType`: one child per element (the reference writes
    /// `<item>`, reads any children).
    Sequence(Box<XmlKind>),
    /// `FixedDictDataType`: one child per field; an empty section is the
    /// `<AllowNone>` value.
    FixedDict {
        allow_none: bool,
        fields: Vec<(String, XmlKind)>,
    },
    /// `MailBoxDataType`: `id`/`ip`/`etc` children.
    MailBox,
}

/// One property of an [`XmlTypeDef`]: its tag name and value shape.
#[derive(Clone, Debug)]
pub struct XmlPropDef {
    /// The property's tag name (`DataDescription::name`).
    pub name: String,
    /// The value shape.
    pub kind: XmlKind,
}

/// One entity type's storage face: the section name it lives under, its
/// id, the persistent properties in [`EntityRecord::values`] order and the
/// identifier property's index.
#[derive(Clone, Debug, Default)]
pub struct XmlTypeDef {
    /// The entity type name — the entity section's tag name.
    pub name: String,
    /// The type's id.
    pub type_id: EntityTypeId,
    /// The persistent properties, in values order.
    pub properties: Vec<XmlPropDef>,
    /// The identifier property's index (`EntityDefs::getIdentifierProperty`).
    pub identifier: Option<usize>,
}

/// Read one property's value out of its section.
fn read_property(section: &XmlSection, prop: &XmlPropDef) -> Result<PropertyValue, DbError> {
    let invalid = |expected: &str| {
        DbError::Storage(format!(
            "db xml: '{}' holds '{}', not {expected}",
            prop.name, section.value
        ))
    };
    Ok(match &prop.kind {
        XmlKind::Int { signed: true } => PropertyValue::Int(
            section
                .value
                .parse::<i64>()
                .map_err(|_| invalid("an integer"))?,
        ),
        XmlKind::Int { signed: false } => PropertyValue::Uint(
            section
                .value
                .parse::<u64>()
                .map_err(|_| invalid("an unsigned integer"))?,
        ),
        XmlKind::Float => PropertyValue::Float(
            section
                .value
                .parse::<f64>()
                .map_err(|_| invalid("a float"))?,
        ),
        XmlKind::String => PropertyValue::String(section.value.clone()),
        XmlKind::Blob => PropertyValue::Blob(base64_decode(&section.value)?),
        XmlKind::Vector(n) => {
            let mut components = Vec::with_capacity(*n);
            for word in section.value.split_whitespace() {
                components.push(word.parse::<f32>().map_err(|_| invalid("a vector"))?);
            }
            if components.len() != *n {
                return Err(invalid(&format!("a vector of {n}")));
            }
            PropertyValue::Vector(components)
        }
        XmlKind::Sequence(element) => PropertyValue::Sequence(
            section
                .children
                .iter()
                .map(|child| read_value(child, element))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        XmlKind::FixedDict { allow_none, fields } => {
            if *allow_none && section.children.is_empty() {
                PropertyValue::FixedDict(None)
            } else {
                let mut values = Vec::with_capacity(fields.len());
                for (name, kind) in fields {
                    let child = section.find(name).ok_or_else(|| {
                        DbError::Storage(format!(
                            "db xml: '{}' is missing its '{name}' field",
                            prop.name
                        ))
                    })?;
                    values.push((name.clone(), read_value(child, kind)?));
                }
                PropertyValue::FixedDict(Some(values))
            }
        }
        XmlKind::MailBox => {
            let etc = section.int_child("etc") as u32;
            PropertyValue::MailBox {
                id: section.int_child("id") as i32,
                ip: section.int_child("ip") as u32,
                port: etc as u16,
                salt: (etc >> 16) as u16,
            }
        }
    })
}

/// Read a value out of an unnamed section (a sequence element).
fn read_value(section: &XmlSection, kind: &XmlKind) -> Result<PropertyValue, DbError> {
    read_property(
        section,
        &XmlPropDef {
            name: section.name.clone(),
            kind: kind.clone(),
        },
    )
}

/// Write one property's value into a fresh section. A value whose shape
/// does not fit the kind (a caller contract breach) writes an empty
/// section, the way the reference leaves a section untouched on a failed
/// `addToSection`.
fn write_property(name: &str, value: &PropertyValue, kind: &XmlKind) -> XmlSection {
    let mut section = XmlSection {
        name: name.to_owned(),
        value: String::new(),
        children: Vec::new(),
    };
    match (value, kind) {
        (
            _,
            XmlKind::FixedDict {
                allow_none: true, ..
            },
        ) if matches!(value, PropertyValue::FixedDict(None)) => {}
        (_, XmlKind::FixedDict { fields, .. }) => {
            if let PropertyValue::FixedDict(Some(values)) = value {
                for ((field_name, field_value), (name, kind)) in values.iter().zip(fields) {
                    debug_assert_eq!(field_name, name);
                    section
                        .children
                        .push(write_property(name, field_value, kind));
                }
            }
        }
        (_, XmlKind::Sequence(element)) => {
            if let PropertyValue::Sequence(items) = value {
                for item in items {
                    section.children.push(write_property("item", item, element));
                }
            }
        }
        (_, XmlKind::MailBox) => {
            if let PropertyValue::MailBox { id, ip, port, salt } = value {
                let etc = *port as u32 | ((*salt as u32) << 16);
                section
                    .children
                    .push(XmlSection::leaf("id", &(*id as i64).to_string()));
                section
                    .children
                    .push(XmlSection::leaf("ip", &(*ip as i64).to_string()));
                section
                    .children
                    .push(XmlSection::leaf("etc", &(etc as i64).to_string()));
            }
        }
        (PropertyValue::Int(v), _) => section.value = v.to_string(),
        (PropertyValue::Uint(v), _) => section.value = v.to_string(),
        (PropertyValue::Float(v), _) => section.value = v.to_string(),
        (PropertyValue::String(v), _) => section.value = v.clone(),
        (PropertyValue::Blob(v), _) => section.value = base64_encode(v),
        (PropertyValue::Vector(v), _) => {
            section.value = v
                .iter()
                .map(|component| component.to_string())
                .collect::<Vec<_>>()
                .join(" ");
        }
        _ => {}
    }
    section
}

/// The identifier's name-key text: `readString` on the identifier child,
/// which is the value itself for a string property and its decimal text
/// for the numeric ones.
fn name_of_value(value: &PropertyValue) -> Option<String> {
    match value {
        PropertyValue::String(text) => Some(text.clone()),
        PropertyValue::Int(v) => Some(v.to_string()),
        PropertyValue::Uint(v) => Some(v.to_string()),
        PropertyValue::Float(v) => Some(v.to_string()),
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// Section: the database
// -----------------------------------------------------------------------------

/// One loaded entity row: the decoded values plus the sections the def
/// face did not consume (the trailing `position`/`direction`/`spaceID`,
/// anything unknown), kept verbatim for the next save.
#[derive(Clone, Debug)]
struct XmlRow {
    type_id: EntityTypeId,
    values: Vec<PropertyValue>,
    extra: Vec<XmlSection>,
}

/// A document child: a non-entity section preserved verbatim, or the slot
/// of an entity row.
#[derive(Clone, Debug)]
enum Slot {
    Info(XmlSection),
    Entity(DatabaseId),
}

/// The XML database (`XMLDatabase`): the parsed `db.xml` tree plus the
/// maps `initEntityMap` builds over it.
#[derive(Clone, Debug)]
pub struct XmlDatabase {
    /// The root section's tag name (`db.xml`).
    root_name: String,
    /// The document children in order: info sections and entity slots.
    slots: Vec<Slot>,
    /// The def face, by type name.
    defs: BTreeMap<String, XmlTypeDef>,
    /// Type id → type name.
    name_of_type: BTreeMap<EntityTypeId, String>,
    /// `nameToIdMaps_`: per type, the identifier value → database id.
    name_maps: BTreeMap<EntityTypeId, BTreeMap<String, DatabaseId>>,
    /// `idToData_`: database id → row.
    rows: BTreeMap<DatabaseId, XmlRow>,
    /// `maxID_`.
    max_id: DatabaseId,
    /// `autoLoadEntityMap_`.
    auto_load: BTreeSet<DatabaseId>,
    /// `activeSet_` — runtime only, never saved.
    active_set: BTreeMap<DatabaseId, BaseMailbox>,
}

impl XmlDatabase {
    /// A database with `defs` and no document (`startup` on a missing file
    /// creates it, `xml_database.cpp:98-104`). Also runs `initAutoLoad`,
    /// which creates the `_BigWorldInfo`/`AutoLoadedEntities` skeleton the
    /// way `findOrCreateInfoSection` does at startup.
    pub fn create(defs: Vec<XmlTypeDef>) -> Self {
        let mut database = Self::bare(defs);
        database.init_auto_load();
        database
    }

    /// The empty database: defs and maps, no document sections. The
    /// auto-load skeleton is the caller's — [`Self::create`] makes one for
    /// a missing file, while [`Self::parse`] lets the document bring its
    /// own `_BigWorldInfo`.
    fn bare(defs: Vec<XmlTypeDef>) -> Self {
        let mut database = Self {
            root_name: "db.xml".to_owned(),
            slots: Vec::new(),
            defs: defs
                .into_iter()
                .map(|def| (def.name.clone(), def))
                .collect(),
            name_of_type: BTreeMap::new(),
            name_maps: BTreeMap::new(),
            rows: BTreeMap::new(),
            max_id: 0,
            auto_load: BTreeSet::new(),
            active_set: BTreeMap::new(),
        };
        for def in database.defs.values() {
            database.name_of_type.insert(def.type_id, def.name.clone());
        }
        database
    }

    /// `startup` + `initEntityMap` over an original `db.xml`. Entities of
    /// a type with no def are preserved as info sections; entities missing
    /// a `databaseID` get `++maxID_` in document order and the id is
    /// written back into the section; a duplicate id keeps the first row.
    pub fn parse(xml: &str, defs: Vec<XmlTypeDef>) -> Result<Self, DbError> {
        let root = parse_document(xml)?;
        let mut database = Self::bare(defs);
        database.root_name = root.name.clone();

        // Pass one: read every entity's databaseID and take the maximum
        // (xml_database.cpp:186-231).
        let mut ids = vec![0; root.children.len()];
        let mut has_missing = false;
        for (index, child) in root.children.iter().enumerate() {
            if !database.defs.contains_key(&child.name) {
                continue;
            }
            let id = child.int_child("databaseID");
            ids[index] = id;
            if id == 0 {
                has_missing = true;
            } else if id > database.max_id {
                database.max_id = id;
            }
        }

        // Pass two: hand out ids to the entities that had none, writing
        // them back into the sections (xml_database.cpp:213-222). `write`
        // replaces an existing child, so an explicit 0 is rewritten too.
        if has_missing {
            for (index, child) in root.children.iter().enumerate() {
                if ids[index] != 0 || !database.defs.contains_key(&child.name) {
                    continue;
                }
                database.max_id += 1;
                ids[index] = database.max_id;
            }
        }

        // Register the rows in document order, skipping duplicate ids (the
        // second entity is ignored, xml_database.cpp:233-250), building the
        // name maps and keeping the document layout.
        let mut registered = BTreeSet::new();
        for (index, mut child) in root.children.into_iter().enumerate() {
            if !database.defs.contains_key(&child.name) {
                database.slots.push(Slot::Info(child));
                continue;
            }
            let id = ids[index];
            if !registered.insert(id) {
                continue;
            }
            if has_missing && child.int_child("databaseID") == 0 {
                child.write_int("databaseID", id);
            }

            let def = database.defs[&child.name].clone();
            let mut values = Vec::with_capacity(def.properties.len());
            let faced: Vec<&str> = def
                .properties
                .iter()
                .map(|prop| prop.name.as_str())
                .chain(["databaseID"])
                .collect();
            for prop in &def.properties {
                let section = child.find(&prop.name).ok_or_else(|| {
                    DbError::Storage(format!(
                        "db xml: '{}' entity is missing its '{}' property",
                        child.name, prop.name
                    ))
                })?;
                values.push(read_property(section, prop)?);
            }
            let extra: Vec<XmlSection> = child
                .children
                .into_iter()
                .filter(|section| !faced.contains(&section.name.as_str()))
                .collect();
            database.rows.insert(
                id,
                XmlRow {
                    type_id: def.type_id,
                    values,
                    extra,
                },
            );
            database.slots.push(Slot::Entity(id));

            if let Some(name_index) = def.identifier
                && let Some(name) = name_of_value(&database.rows[&id].values[name_index])
            {
                database
                    .name_maps
                    .entry(def.type_id)
                    .or_default()
                    .entry(name)
                    .or_insert(id);
            }
        }

        database.init_auto_load();
        Ok(database)
    }

    /// `initAutoLoad` (`xml_database.cpp:1232-1257`): harvest the
    /// `_BigWorldInfo/AutoLoadedEntities` ids, creating the section when it
    /// is missing.
    fn init_auto_load(&mut self) {
        {
            let info = self.info_section_mut();
            if info.find(AUTO_LOAD_SECTION).is_none() {
                info.children
                    .push(XmlSection::branch(AUTO_LOAD_SECTION, Vec::new()));
            }
        }
        let ids: Vec<DatabaseId> = self
            .info_section()
            .and_then(|info| info.find(AUTO_LOAD_SECTION))
            .into_iter()
            .flat_map(|section| &section.children)
            .filter(|child| child.name == "databaseID")
            .filter_map(|child| child.value.parse::<DatabaseId>().ok())
            .filter(|id| *id > 0)
            .collect();
        self.auto_load.extend(ids);
    }

    /// The `_BigWorldInfo` section, if the document has one.
    fn info_section(&self) -> Option<&XmlSection> {
        self.slots.iter().find_map(|slot| match slot {
            Slot::Info(info) if info.name == INFO_SECTION => Some(info),
            _ => None,
        })
    }

    /// `findOrCreateInfoSection(INFO_SECTION)` over the document slots
    /// (`xml_database.cpp:1208-1226`).
    fn info_section_mut(&mut self) -> &mut XmlSection {
        let index = self
            .slots
            .iter()
            .position(|slot| matches!(slot, Slot::Info(info) if info.name == INFO_SECTION));
        if index.is_none() {
            self.slots.push(Slot::Info(XmlSection::branch(
                INFO_SECTION,
                vec![XmlSection::branch(AUTO_LOAD_SECTION, Vec::new())],
            )));
        }
        let index = index.unwrap_or(self.slots.len() - 1);
        match &mut self.slots[index] {
            Slot::Info(info) => info,
            Slot::Entity(_) => unreachable!("the info filter matched an entity slot"),
        }
    }

    /// `addToAutoLoad` (`xml_database.cpp:1263-1277`): record the id and
    /// append its `databaseID` child to the document.
    pub fn add_auto_load(&mut self, db_id: DatabaseId) {
        if !self.auto_load.insert(db_id) {
            return;
        }
        self.info_section_mut()
            .find_mut(AUTO_LOAD_SECTION)
            .expect("init_auto_load created the section")
            .children
            .push(XmlSection::leaf("databaseID", &db_id.to_string()));
    }

    /// `removeFromAutoLoad` (`xml_database.cpp:1283-1298`): drop the id
    /// and its document child.
    pub fn remove_auto_load(&mut self, db_id: DatabaseId) {
        if !self.auto_load.remove(&db_id) {
            return;
        }
        let info = self.info_section_mut();
        info.find_mut(AUTO_LOAD_SECTION)
            .expect("init_auto_load created the section")
            .children
            .retain(|child| !(child.name == "databaseID" && child.value == db_id.to_string()));
    }

    /// The auto-load set, in id order (`autoLoadEntities`'s source).
    /// The put-time flag write (`xml_database.cpp:700-711`): only a
    /// successful put (`isOK`) touches the flag; `Retain` leaves it.
    fn apply_auto_load(&mut self, db_id: DatabaseId, update_auto_load: UpdateAutoLoad) {
        match update_auto_load {
            UpdateAutoLoad::Retain => {}
            UpdateAutoLoad::True => self.add_auto_load(db_id),
            UpdateAutoLoad::False => self.remove_auto_load(db_id),
        }
    }

    pub fn auto_load_ids(&self) -> Vec<DatabaseId> {
        self.auto_load.iter().copied().collect()
    }

    /// `findEntityByName` (`xml_database.cpp:846-855`): the name map's id,
    /// or `None` where the reference answers 0.
    pub fn find_entity_by_name(&self, type_id: EntityTypeId, name: &str) -> Option<DatabaseId> {
        self.name_maps
            .get(&type_id)
            .and_then(|map| map.get(name))
            .copied()
    }

    /// The database document (`timedSave`'s payload): entity sections in
    /// document order with their `databaseID`, info sections verbatim.
    pub fn to_xml(&self) -> String {
        let root = XmlSection {
            name: self.root_name.clone(),
            value: String::new(),
            children: self
                .slots
                .iter()
                .map(|slot| match slot {
                    Slot::Info(info) => info.clone(),
                    Slot::Entity(db_id) => self.build_section(*db_id),
                })
                .collect(),
        };
        write_document(&root)
    }

    /// Rebuild one entity's section: the properties in def order, the
    /// preserved extras, then `databaseID` last (`xml_database.cpp:630-683`).
    fn build_section(&self, db_id: DatabaseId) -> XmlSection {
        let row = &self.rows[&db_id];
        let def = &self.defs[&self.name_of_type[&row.type_id]];
        let mut children: Vec<XmlSection> = def
            .properties
            .iter()
            .zip(&row.values)
            .map(|(prop, value)| write_property(&prop.name, value, &prop.kind))
            .collect();
        children.extend(row.extra.iter().cloned());
        children.push(XmlSection::leaf("databaseID", &db_id.to_string()));
        XmlSection::branch(&def.name, children)
    }

    /// The identifier's name-key for `values`, if the type has an
    /// identifier and it holds a name-shaped value.
    fn identifier_name(&self, type_id: EntityTypeId, values: &[PropertyValue]) -> Option<String> {
        let def = self.defs.get(self.name_of_type.get(&type_id)?)?;
        let index = def.identifier?;
        name_of_value(values.get(index)?)
    }
}

impl EntityStorage for XmlDatabase {
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
        match self.rows.get(&key.db_id) {
            Some(row) if row.type_id == key.type_id => Ok(Some(EntityRecord {
                type_id: row.type_id,
                db_id: key.db_id,
                values: row.values.clone(),
            })),
            _ => Ok(None),
        }
    }

    fn put_entity(
        &mut self,
        key: &EntityKey,
        values: &[PropertyValue],
        put_explicit_id: bool,
        update_auto_load: UpdateAutoLoad,
    ) -> Result<DatabaseId, DbError> {
        // `putExplicitID` is unused by the XML backend
        // (xml_database.cpp:67).
        let _ = put_explicit_id;

        if !self.name_of_type.contains_key(&key.type_id) {
            return Err(DbError::Storage(format!(
                "entity type {} is invalid",
                key.type_id
            )));
        }

        if key.db_id == PENDING_DATABASE_ID {
            // A `values` vector must match the def face, the way the
            // reference reads a fixed-size stream into the fresh section.
            let prop_count = self.defs[&self.name_of_type[&key.type_id]].properties.len();
            if values.len() != prop_count {
                return Err(DbError::Storage(format!(
                    "entity type {} takes {} values, got {}",
                    key.type_id,
                    prop_count,
                    values.len()
                )));
            }
            // `dbID = ++maxID_` (xml_database.cpp:626).
            self.max_id += 1;
            let db_id = self.max_id;

            if let Some(name) = self.identifier_name(key.type_id, values) {
                let map = self.name_maps.entry(key.type_id).or_default();
                if map.contains_key(&name) {
                    return Err(DbError::AlreadyExists);
                }
                map.insert(name, db_id);
            }
            self.rows.insert(
                db_id,
                XmlRow {
                    type_id: key.type_id,
                    values: values.to_vec(),
                    extra: Vec::new(),
                },
            );
            self.slots.push(Slot::Entity(db_id));
            self.apply_auto_load(db_id, update_auto_load);
            return Ok(db_id);
        }

        // An update: the row must exist and be of the key's type. The
        // name check runs before anything moves: a taken name fails the
        // put with the old row intact (xml_database.cpp:652-679).
        let Some(row) = self.rows.get(&key.db_id) else {
            return Err(DbError::NotFound);
        };
        if row.type_id != key.type_id {
            return Err(DbError::NotFound);
        }
        let prop_count = self.defs[&self.name_of_type[&key.type_id]].properties.len();
        if values.len() != prop_count {
            return Err(DbError::Storage(format!(
                "entity type {} takes {} values, got {}",
                key.type_id,
                prop_count,
                values.len()
            )));
        }
        let old_name = self.identifier_name(key.type_id, &row.values);
        let new_name = self.identifier_name(key.type_id, values);
        if let (Some(old_name), Some(new_name)) = (&old_name, &new_name)
            && old_name != new_name
        {
            let map = self.name_maps.entry(key.type_id).or_default();
            if map.contains_key(new_name) {
                return Err(DbError::AlreadyExists);
            }
            map.remove(old_name);
            map.insert(new_name.clone(), key.db_id);
        }

        // delChild(pOldProps) + the fresh section appended at the end
        // (xml_database.cpp:685-692): the slot moves to the document's
        // end. The extras stay — the reference rewrites them from the
        // stream (BW-09-04's face).
        let row = self.rows.get_mut(&key.db_id).expect("checked above");
        row.values = values.to_vec();
        self.slots
            .retain(|slot| !matches!(slot, Slot::Entity(db_id) if *db_id == key.db_id));
        self.slots.push(Slot::Entity(key.db_id));
        self.apply_auto_load(key.db_id, update_auto_load);
        Ok(key.db_id)
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        let Some(row) = self.rows.get(&key.db_id) else {
            return Err(DbError::NotFound);
        };
        if row.type_id != key.type_id {
            return Err(DbError::NotFound);
        }

        // deleteEntity: the name, the id, the section, the auto-load set
        // (xml_database.cpp:809-840); the active-set entry goes with it
        // (xml_database.cpp:791-799).
        if let Some(name) = self.identifier_name(key.type_id, &row.values)
            && let Some(map) = self.name_maps.get_mut(&key.type_id)
        {
            map.remove(&name);
        }
        self.rows.remove(&key.db_id);
        self.slots
            .retain(|slot| !matches!(slot, Slot::Entity(db_id) if *db_id == key.db_id));
        self.remove_auto_load(key.db_id);
        self.active_set.remove(&key.db_id);
        Ok(())
    }

    fn logon_of(&self, key: &EntityKey) -> Result<Option<BaseMailbox>, DbError> {
        Ok(self.active_set.get(&key.db_id).cloned())
    }

    fn check_out(&mut self, key: &EntityKey, mailbox: BaseMailbox) -> Result<CheckOut, DbError> {
        match self.rows.get(&key.db_id) {
            Some(row) if row.type_id == key.type_id => {}
            _ => return Err(DbError::NotFound),
        }
        match self.active_set.get(&key.db_id) {
            Some(holder) => Ok(CheckOut::Elsewhere(holder.clone())),
            None => {
                self.active_set.insert(key.db_id, mailbox);
                Ok(CheckOut::Granted)
            }
        }
    }

    fn check_in(&mut self, key: &EntityKey) -> Result<(), DbError> {
        self.active_set.remove(&key.db_id);
        Ok(())
    }

    fn all_entities(&self) -> Result<Vec<EntityRecord>, DbError> {
        let mut records: Vec<EntityRecord> = self
            .rows
            .iter()
            .map(|(db_id, row)| EntityRecord {
                type_id: row.type_id,
                db_id: *db_id,
                values: row.values.clone(),
            })
            .collect();
        records.sort_by_key(|record| (record.type_id, record.db_id));
        Ok(records)
    }

    fn game_time_of(&self, _key: &EntityKey) -> Result<Option<GameTime>, DbError> {
        // The XML database has no per-entity gameTime: the reference reads
        // the consolidation game time off the put stream and discards it
        // ("Used in MySQL only", xml_database.cpp:648-650), so there is
        // never a stored time to compare and no record is ever "newer".
        Ok(None)
    }

    fn put_consolidated(
        &mut self,
        key: &EntityKey,
        values: &[PropertyValue],
        _game_time: GameTime,
    ) -> Result<(), DbError> {
        // Same discarded-stream face: the write is a plain put, and the
        // consolidator never touches the flag
        // (`UPDATE_AUTO_LOAD_RETAIN`).
        self.put_entity(key, values, false, UpdateAutoLoad::Retain)
            .map(|_| ())
    }

    /// `XMLDatabase::autoLoadEntities` (`xml_database.cpp:1059-1095`):
    /// stream the flagged ids out in flag order; an id whose row is
    /// gone is pruned from the set with a warning, not delivered. The
    /// unknown-type error leg is unreachable here — our rows carry
    /// `type_id` directly and puts reject unknown types up front.
    fn auto_load_entities(&mut self, loader: &mut dyn EntityAutoLoader) -> Result<(), DbError> {
        for db_id in self.auto_load.iter().copied().collect::<Vec<_>>() {
            match self.rows.get(&db_id) {
                Some(row) => loader.add_entity(row.type_id, db_id),
                None => {
                    // WARNING_MSG "removing database ID for non-existent
                    // entity from auto-loading entities set".
                    self.remove_auto_load(db_id);
                }
            }
        }
        loader.start();
        Ok(())
    }
}

impl crate::storage::EntityStorageImage for XmlDatabase {
    fn image(&self) -> StorageImage {
        StorageImage::of_xml(self.clone())
    }

    fn restore_image(&mut self, image: &StorageImage) {
        *self = image.as_xml().clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{EntityStorage, EntityStorageImage, MemoryStorage, load_entity};
    use crate::transaction::EntityTransaction;

    /// The original fantasydemo `scripts/db.xml`.
    const FANTASYDEMO_DB_XML: &str = r##"<db.xml>
<!-- Generated by scripts/generate-fantasydemo-assets.py -- do not edit. -->
	<_BigWorldInfo>
		<LogOnMapping>
			<item>
				<logOnName>	FantasyAccount	</logOnName>
				<password>	a	</password>
				<type>	Account	</type>
				<entityID>	2	</entityID>
			</item>
		</LogOnMapping>
		<AutoLoadedEntities/>
		<AutoLoadedSpaceData/>
	</_BigWorldInfo>

	<Account>
		<playerName>	FantasyAccount	</playerName>
		<character>	FantasyHero	</character>
		<databaseID>	2	</databaseID>
	</Account>

	<Avatar>
		<playerName>	FantasyHero	</playerName>
		<prop1>	1	</prop1>
		<prop2>	2	</prop2>
		<prop3>	3	</prop3>
		<position>	0.0 0.0 0.0	</position>
		<direction>	0.0 0.0 0.0	</direction>
		<spaceID>	1	</spaceID>
		<databaseID>	1	</databaseID>
	</Avatar>
</db.xml>
"##;

    fn string_prop(name: &str) -> XmlPropDef {
        XmlPropDef {
            name: name.to_owned(),
            kind: XmlKind::String,
        }
    }

    fn int_prop(name: &str) -> XmlPropDef {
        XmlPropDef {
            name: name.to_owned(),
            kind: XmlKind::Int { signed: true },
        }
    }

    fn def(
        name: &str,
        type_id: u16,
        properties: Vec<XmlPropDef>,
        identifier: Option<usize>,
    ) -> XmlTypeDef {
        XmlTypeDef {
            name: name.to_owned(),
            type_id,
            properties,
            identifier,
        }
    }

    /// The def face of the fantasydemo fixture: Account(2) and Avatar(1),
    /// both identified by `playerName`.
    fn fantasydemo_defs() -> Vec<XmlTypeDef> {
        vec![
            def(
                "Account",
                2,
                vec![string_prop("playerName"), string_prop("character")],
                Some(0),
            ),
            def(
                "Avatar",
                1,
                vec![
                    string_prop("playerName"),
                    int_prop("prop1"),
                    int_prop("prop2"),
                    int_prop("prop3"),
                ],
                Some(0),
            ),
        ]
    }

    fn demo() -> XmlDatabase {
        XmlDatabase::parse(FANTASYDEMO_DB_XML, fantasydemo_defs()).expect("the fixture parses")
    }

    fn key(type_id: u16, db_id: DatabaseId) -> EntityKey {
        EntityKey { type_id, db_id }
    }

    fn avatar_values(name: &str) -> Vec<PropertyValue> {
        vec![
            PropertyValue::String(name.to_owned()),
            PropertyValue::Int(1),
            PropertyValue::Int(2),
            PropertyValue::Int(3),
        ]
    }

    fn record_of(database: &XmlDatabase, type_id: u16, db_id: DatabaseId) -> EntityRecord {
        load_entity(database, &key(type_id, db_id)).expect("the entity loads")
    }

    // -- parser and writer ---------------------------------------------------

    #[test]
    fn the_original_fixture_loads_both_entities() {
        let database = demo();

        let account = record_of(&database, 2, 2);
        assert_eq!(
            account.values,
            vec![
                PropertyValue::String("FantasyAccount".to_owned()),
                PropertyValue::String("FantasyHero".to_owned()),
            ]
        );

        let avatar = record_of(&database, 1, 1);
        assert_eq!(avatar.values, avatar_values("FantasyHero"));

        assert_eq!(database.max_id, 2, "maxID is the largest databaseID");
    }

    #[test]
    fn an_original_account_name_resolves_to_its_database_id() {
        let database = demo();
        assert_eq!(database.find_entity_by_name(2, "FantasyAccount"), Some(2));
        assert_eq!(database.find_entity_by_name(1, "FantasyHero"), Some(1));
        assert_eq!(database.find_entity_by_name(1, "FantasyAccount"), None);
        assert_eq!(database.find_entity_by_name(9, "FantasyHero"), None);
    }

    #[test]
    fn unknown_sections_survive_a_round_trip_verbatim() {
        let database = demo();

        // The billing mapping is not this face's to read, but it is this
        // face's to keep.
        assert!(
            demo()
                .to_xml()
                .contains("<logOnName>\tFantasyAccount\t</logOnName>"),
            "the LogOnMapping survives the save"
        );
        assert_eq!(
            database.auto_load_ids(),
            Vec::<DatabaseId>::new(),
            "the empty auto-load set round-trips"
        );
    }

    #[test]
    fn a_round_trip_reproduces_the_same_tree() {
        let database = demo();
        let once = XmlDatabase::parse(&database.to_xml(), fantasydemo_defs())
            .expect("the first save parses");
        let twice =
            XmlDatabase::parse(&once.to_xml(), fantasydemo_defs()).expect("the second save parses");

        assert_eq!(once.to_xml(), twice.to_xml(), "the save is idempotent");
        assert_eq!(record_of(&once, 1, 1), record_of(&database, 1, 1));
        assert_eq!(record_of(&once, 2, 2), record_of(&database, 2, 2));
    }

    #[test]
    fn a_saved_document_keeps_the_original_layout() {
        let text = demo().to_xml();
        assert!(
            text.starts_with("<db.xml>\n\t<_BigWorldInfo>\n\t\t<LogOnMapping>\n"),
            "tab-indented children under the root tag: {text}"
        );
        assert!(
            text.contains("\t\t<AutoLoadedEntities />\n"),
            "an empty section writes as a self-closing tag"
        );
        assert!(
            text.contains("\t<Account>\n\t\t<playerName>\tFantasyAccount\t</playerName>\n"),
            "a tab separates the tag from its text"
        );
        assert!(text.contains("<databaseID>\t1\t</databaseID>"));
        assert!(text.ends_with("</db.xml>\n"));
    }

    #[test]
    fn a_prolog_and_attributes_are_tolerated() {
        let text = "<?xml version=\"1.0\"?><db.xml version=\"2\"><Thing prop=\"x\">	7	</Thing><!-- trailing --></db.xml>";
        let defs = vec![def("Thing", 5, vec![int_prop("value_placeholder")], None)];
        // `prop` and `version` are attributes — accepted, ignored.
        let section = parse_document(text).expect("the prolog and attributes parse");
        assert_eq!(section.name, "db.xml");
        assert_eq!(section.children[0].value, "7");
        assert_eq!(section.children[0].find("prop"), None);
        let _ = defs;

        // A comment ahead of the root is skipped too.
        let section =
            parse_document("<!-- generated -->\n<db.xml/>").expect("the leading comment parses");
        assert_eq!(section.name, "db.xml");
    }

    #[test]
    fn escape_sequences_round_trip() {
        let text = r#"<db.xml><Thing><name>a &lt;b&gt; &amp; &quot;c&quot; &apos;d&apos;</name><blob>YWJj</blob></Thing></db.xml>"#;
        let defs = vec![def(
            "Thing",
            5,
            vec![
                string_prop("name"),
                XmlPropDef {
                    name: "blob".to_owned(),
                    kind: XmlKind::Blob,
                },
            ],
            None,
        )];
        let database = XmlDatabase::parse(text, defs.clone()).expect("the escapes reduce");
        let record = record_of(&database, 5, 1);
        assert_eq!(
            record.values[0],
            PropertyValue::String("a <b> & \"c\" 'd'".to_owned())
        );
        assert_eq!(record.values[1], PropertyValue::Blob(b"abc".to_vec()));

        let again =
            XmlDatabase::parse(&database.to_xml(), defs).expect("the expanded escapes reduce");
        assert_eq!(record_of(&again, 5, 1), record);
        assert!(
            database
                .to_xml()
                .contains("<name>\ta &lt;b&gt; &amp; &quot;c&quot; &apos;d&apos;\t</name>"),
            "the writer expands the special chars back"
        );
    }

    #[test]
    fn unknown_escapes_are_left_in_the_text() {
        assert_eq!(reduce_escapes("&nope;"), "&nope;");
        assert_eq!(reduce_escapes("&x"), "&x", "no semicolon, no reduction");
        assert_eq!(reduce_escapes("&;"), "&;", "too short, no reduction");
        assert_eq!(reduce_escapes("&#65;"), "A");
        assert_eq!(reduce_escapes("&#x41;"), "A");
        assert_eq!(
            reduce_escapes("a&amp;&b"),
            "a&&b",
            "scan continues past a hit"
        );
    }

    #[test]
    fn a_non_ascii_byte_writes_as_a_char_code() {
        assert_eq!(
            expand_escapes("é"),
            "&#195;&#169;",
            "one code per UTF-8 byte"
        );
        let section = XmlSection::leaf("name", "é");
        let text = write_document(&XmlSection::branch("db.xml", vec![section]));
        assert!(text.contains("&#195;&#169;"));
        let back = parse_document(&text).expect("the codes reduce");
        assert_eq!(back.children[0].value, "é");
    }

    #[test]
    fn malformed_documents_are_rejected() {
        for broken in [
            "<db.xml><a></b></db.xml>",
            "<db.xml><a></db.xml>",
            "<db.xml><a",
            "<db.xml><a ",
            "<db.xml><a>text",
            "<db.xml><a></a</db.xml>",
            "<db.xml><</db.xml>",
            "<db.xml><a/></db.xml>tail",
            "no element at all",
            "<db.xml><a b=\"x></a></db.xml>",
            "<db.xml><a><!-- unterminated</a></db.xml>",
        ] {
            assert!(parse_document(broken).is_err(), "{broken:?} must not parse");
        }
    }

    // -- value shapes ---------------------------------------------------------

    #[test]
    fn sequences_fixed_dicts_and_none_round_trip() {
        let text = r#"<db.xml>
	<Bag>
		<owner>	FantasyHero	</owner>
		<items>
			<item>	7	</item>
			<item>	9	</item>
		</items>
		<enchant>
			<level>	3	</level>
			<label>	sharp	</label>
		</enchant>
		<curse/>
		<bind>	FantasyAccount	</bind>
	</Bag>
</db.xml>"#;
        let defs = vec![def(
            "Bag",
            6,
            vec![
                string_prop("owner"),
                XmlPropDef {
                    name: "items".to_owned(),
                    kind: XmlKind::Sequence(Box::new(XmlKind::Int { signed: true })),
                },
                XmlPropDef {
                    name: "enchant".to_owned(),
                    kind: XmlKind::FixedDict {
                        allow_none: false,
                        fields: vec![
                            ("level".to_owned(), XmlKind::Int { signed: true }),
                            ("label".to_owned(), XmlKind::String),
                        ],
                    },
                },
                XmlPropDef {
                    name: "curse".to_owned(),
                    kind: XmlKind::FixedDict {
                        allow_none: true,
                        fields: vec![("level".to_owned(), XmlKind::Int { signed: true })],
                    },
                },
                XmlPropDef {
                    name: "bind".to_owned(),
                    kind: XmlKind::MailBox,
                },
            ],
            None,
        )];
        let database = XmlDatabase::parse(text, defs.clone()).expect("the shapes decode");
        let record = record_of(&database, 6, 1);
        assert_eq!(
            record.values[0],
            PropertyValue::String("FantasyHero".to_owned())
        );
        assert_eq!(
            record.values[1],
            PropertyValue::Sequence(vec![PropertyValue::Int(7), PropertyValue::Int(9)])
        );
        assert_eq!(
            record.values[2],
            PropertyValue::FixedDict(Some(vec![
                ("level".to_owned(), PropertyValue::Int(3)),
                (
                    "label".to_owned(),
                    PropertyValue::String("sharp".to_owned())
                ),
            ]))
        );
        assert_eq!(record.values[3], PropertyValue::FixedDict(None));
        assert_eq!(
            record.values[4],
            PropertyValue::MailBox {
                id: 0,
                ip: 0,
                port: 0,
                salt: 0,
            }
        );

        let again = XmlDatabase::parse(&database.to_xml(), defs).expect("the shapes re-encode");
        assert_eq!(record_of(&again, 6, 1), record);
        assert!(
            database
                .to_xml()
                .contains("<items>\n\t\t\t<item>\t7\t</item>\n"),
            "sequence elements write as <item> children"
        );
        assert!(
            database.to_xml().contains("<curse />"),
            "the None fixed dict writes as an empty section"
        );
    }

    #[test]
    fn a_fixed_dict_missing_a_field_fails_the_load() {
        let text = "<db.xml><Bag><enchant><level>3</level></enchant></Bag></db.xml>";
        let defs = vec![def(
            "Bag",
            6,
            vec![XmlPropDef {
                name: "enchant".to_owned(),
                kind: XmlKind::FixedDict {
                    allow_none: false,
                    fields: vec![
                        ("level".to_owned(), XmlKind::Int { signed: true }),
                        ("label".to_owned(), XmlKind::String),
                    ],
                },
            }],
            None,
        )];
        let error = XmlDatabase::parse(text, defs).expect_err("the field is missing");
        assert!(
            error.to_string().contains("is missing its 'label' field"),
            "the error names the field: {error}"
        );
    }

    #[test]
    fn a_mailbox_writes_id_ip_and_etc() {
        let value = PropertyValue::MailBox {
            id: -3,
            ip: 0x0102_0304,
            port: 20933,
            salt: 0x0102,
        };
        let section = write_property("mb", &value, &XmlKind::MailBox);
        assert_eq!(
            section.children,
            vec![
                XmlSection::leaf("id", "-3"),
                XmlSection::leaf("ip", "16909060"),
                XmlSection::leaf("etc", &(20933 | (0x0102 << 16)).to_string()),
            ]
        );
        let read_back = read_property(
            &section,
            &XmlPropDef {
                name: "mb".to_owned(),
                kind: XmlKind::MailBox,
            },
        )
        .expect("the mailbox reads");
        assert_eq!(read_back, value);
    }

    #[test]
    fn unsigned_and_float_values_round_trip() {
        let text = r#"<db.xml><Stat><count>	4000000000	</count><ratio>	2.5	</ratio><dir>	0.5 -1.25 3	</dir></Stat></db.xml>"#;
        let defs = vec![def(
            "Stat",
            7,
            vec![
                XmlPropDef {
                    name: "count".to_owned(),
                    kind: XmlKind::Int { signed: false },
                },
                XmlPropDef {
                    name: "ratio".to_owned(),
                    kind: XmlKind::Float,
                },
                XmlPropDef {
                    name: "dir".to_owned(),
                    kind: XmlKind::Vector(3),
                },
            ],
            None,
        )];
        let database = XmlDatabase::parse(text, defs.clone()).expect("the values decode");
        let record = record_of(&database, 7, 1);
        assert_eq!(record.values[0], PropertyValue::Uint(4_000_000_000));
        assert_eq!(record.values[1], PropertyValue::Float(2.5));
        assert_eq!(
            record.values[2],
            PropertyValue::Vector(vec![0.5, -1.25, 3.0])
        );
        let again = XmlDatabase::parse(&database.to_xml(), defs).expect("the values re-encode");
        assert_eq!(record_of(&again, 7, 1), record);
    }

    #[test]
    fn a_corrupt_value_fails_the_load() {
        let corrupt_defs = |kind| {
            vec![def(
                "Stat",
                7,
                vec![XmlPropDef {
                    name: "count".to_owned(),
                    kind,
                }],
                None,
            )]
        };
        let error = XmlDatabase::parse(
            "<db.xml><Stat><count>not a number</count></Stat></db.xml>",
            corrupt_defs(XmlKind::Int { signed: true }),
        )
        .expect_err("the value is corrupt");
        assert!(
            error.to_string().contains("not an integer"),
            "the error names the property: {error}"
        );

        let missing = XmlDatabase::parse(
            "<db.xml><Stat></Stat></db.xml>",
            corrupt_defs(XmlKind::Int { signed: true }),
        )
        .expect_err("the property is missing");
        assert!(
            missing
                .to_string()
                .contains("is missing its 'count' property"),
            "the error names the missing property: {missing}"
        );

        let vector = XmlDatabase::parse(
            "<db.xml><Stat><count>1 2</count></Stat></db.xml>",
            corrupt_defs(XmlKind::Vector(3)),
        )
        .expect_err("the vector is short");
        assert!(
            vector.to_string().contains("not a vector of 3"),
            "the error names the arity: {vector}"
        );

        let unsigned = XmlDatabase::parse(
            "<db.xml><Stat><count>-4</count></Stat></db.xml>",
            corrupt_defs(XmlKind::Int { signed: false }),
        )
        .expect_err("the value is negative");
        assert!(
            unsigned.to_string().contains("not an unsigned integer"),
            "the error names the sign: {unsigned}"
        );

        let float = XmlDatabase::parse(
            "<db.xml><Stat><count>2.5x</count></Stat></db.xml>",
            corrupt_defs(XmlKind::Float),
        )
        .expect_err("the value is not a float");
        assert!(
            float.to_string().contains("not a float"),
            "the error names the shape: {float}"
        );

        let component = XmlDatabase::parse(
            "<db.xml><Stat><count>1 x 3</count></Stat></db.xml>",
            corrupt_defs(XmlKind::Vector(3)),
        )
        .expect_err("the component is corrupt");
        assert!(
            component.to_string().contains("not a vector"),
            "the error names the component: {component}"
        );

        // A corrupt sequence element fails the load the same way.
        let sequence_defs = vec![def(
            "Bag",
            6,
            vec![XmlPropDef {
                name: "items".to_owned(),
                kind: XmlKind::Sequence(Box::new(XmlKind::Int { signed: true })),
            }],
            None,
        )];
        let element = XmlDatabase::parse(
            "<db.xml><Bag><items><item>	7	</item><item>	x	</item></items></Bag></db.xml>",
            sequence_defs,
        )
        .expect_err("the element is corrupt");
        assert!(
            element
                .to_string()
                .contains("'item' holds 'x', not an integer"),
            "the error names the element: {element}"
        );
    }

    #[test]
    fn base64_covers_every_chunk_shape() {
        for data in [
            b"".to_vec(),
            b"a".to_vec(),
            b"ab".to_vec(),
            b"abc".to_vec(),
            b"abcd".to_vec(),
            b"\xfb\xef\xdd".to_vec(),
        ] {
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(
            base64_encode(b"\xfb\xef\xdd"),
            "++/d",
            "the + and / sextets are in play"
        );
        assert_eq!(
            base64_decode("aGVs\nbG8=").unwrap(),
            b"hello".to_vec(),
            "line breaks are tolerated"
        );
        assert!(base64_decode("a*c").is_err(), "a stray byte is rejected");
        assert!(base64_decode("y").is_err(), "a group of one is malformed");
        assert_eq!(
            base64_decode("YQ").unwrap(),
            b"a".to_vec(),
            "stripped padding decodes leniently"
        );
    }

    #[test]
    fn a_mismatched_value_writes_an_empty_section() {
        // A caller contract breach: a sequence value under a string kind.
        let section = write_property("x", &PropertyValue::Sequence(vec![]), &XmlKind::String);
        assert_eq!(section, XmlSection::leaf("x", ""), "no crash, no data");

        // The same for the structured kinds: a scalar under a table-backed
        // kind writes the bare section and nothing else.
        let scalar = PropertyValue::Int(5);
        let fields = vec![("level".to_owned(), XmlKind::Int { signed: true })];
        let dict = XmlKind::FixedDict {
            allow_none: false,
            fields: fields.clone(),
        };
        assert_eq!(
            write_property("d", &scalar, &dict),
            XmlSection::leaf("d", "")
        );
        let none_dict = XmlKind::FixedDict {
            allow_none: true,
            fields,
        };
        assert_eq!(
            write_property("d", &scalar, &none_dict),
            XmlSection::leaf("d", ""),
            "the AllowNone guard does not fire for a scalar"
        );
        assert_eq!(
            write_property("s", &scalar, &XmlKind::Sequence(Box::new(XmlKind::String))),
            XmlSection::leaf("s", "")
        );
        assert_eq!(
            write_property("m", &scalar, &XmlKind::MailBox),
            XmlSection::leaf("m", "")
        );
    }

    #[test]
    fn a_non_name_identifier_value_keys_on_its_text() {
        assert_eq!(
            name_of_value(&PropertyValue::Uint(77)),
            Some("77".to_owned())
        );
        assert_eq!(
            name_of_value(&PropertyValue::Float(1.5)),
            Some("1.5".to_owned())
        );
        assert_eq!(
            name_of_value(&PropertyValue::Int(-2)),
            Some("-2".to_owned())
        );
        assert_eq!(name_of_value(&PropertyValue::Blob(vec![1])), None);
    }

    // -- the id map and name maps ---------------------------------------------

    #[test]
    fn entities_without_database_ids_get_them_in_document_order() {
        let text = r#"<db.xml>
	<Account>
		<playerName>	old	</playerName>
		<character>	c	</character>
		<databaseID>	5	</databaseID>
	</Account>
	<Account>
		<playerName>	first	</playerName>
		<character>	c	</character>
	</Account>
	<Account>
		<playerName>	second	</playerName>
		<character>	c	</character>
	</Account>
</db.xml>"#;
        let database = XmlDatabase::parse(text, fantasydemo_defs()).expect("it loads");

        // The first pass took the maximum (5); the second pass handed out
        // 6 and 7 in document order, and maxID_ moved with it.
        assert_eq!(database.max_id, 7);
        assert_eq!(database.find_entity_by_name(2, "first"), Some(6));
        assert_eq!(database.find_entity_by_name(2, "second"), Some(7));

        // The written ids round-trip, so the next start does not hand out
        // fresh ones.
        let again = XmlDatabase::parse(&database.to_xml(), fantasydemo_defs())
            .expect("the written ids parse");
        assert_eq!(again.max_id, 7);
        assert_eq!(
            record_of(&again, 2, 6).values[0],
            PropertyValue::String("first".to_owned())
        );
        assert_eq!(
            record_of(&again, 2, 7).values[0],
            PropertyValue::String("second".to_owned())
        );
    }

    #[test]
    fn an_explicit_zero_id_is_rewritten_in_place() {
        let text = r#"<db.xml><Account><playerName>	zero	</playerName><character>	c	</character><databaseID>	0	</databaseID></Account></db.xml>"#;
        let database = XmlDatabase::parse(text, fantasydemo_defs()).expect("it loads");
        assert_eq!(database.max_id, 1);
        assert_eq!(database.find_entity_by_name(2, "zero"), Some(1));
        // The id was written back into the existing child, not appended.
        let text = database.to_xml();
        assert!(
            text.contains("<databaseID>\t1\t</databaseID>\n\t</Account>"),
            "the rewritten id stays in place: {text}"
        );
        assert_eq!(
            text.matches("<databaseID>").count(),
            1,
            "no second id child was appended"
        );
    }

    #[test]
    fn a_duplicate_database_id_keeps_the_first_section() {
        let text = r#"<db.xml>
	<Account>
		<playerName>	keeper	</playerName>
		<character>	c	</character>
		<databaseID>	4	</databaseID>
	</Account>
	<Account>
		<playerName>	loser	</playerName>
		<character>	c	</character>
		<databaseID>	4	</databaseID>
	</Account>
</db.xml>"#;
        let database = XmlDatabase::parse(text, fantasydemo_defs()).expect("it loads");
        assert_eq!(database.find_entity_by_name(2, "keeper"), Some(4));
        assert_eq!(
            database.find_entity_by_name(2, "loser"),
            None,
            "the second section is ignored"
        );
        assert!(load_entity(&database, &key(2, 4)).is_ok());
    }

    #[test]
    fn an_entity_of_an_unknown_type_is_ignored_but_kept() {
        let text = r#"<db.xml>
	<_BigWorldInfo>
		<AutoLoadedEntities/>
	</_BigWorldInfo>
	<Thing>
		<name>	mystery	</name>
		<databaseID>	9	</databaseID>
	</Thing>
	<Account>
		<playerName>	known	</playerName>
		<character>	c	</character>
		<databaseID>	2	</databaseID>
	</Account>
</db.xml>"#;
        let database = XmlDatabase::parse(text, fantasydemo_defs()).expect("it loads");
        assert!(load_entity(&database, &key(2, 2)).is_ok());
        assert_eq!(database.max_id, 2, "the unknown section claims no id");

        let saved = database.to_xml();
        assert!(saved.contains("<Thing>"), "the unknown section is kept");
        assert!(
            saved.find("<Thing>").expect("kept") < saved.find("<Account>").expect("kept"),
            "its document position is kept"
        );

        // A full round trip keeps both.
        let again = XmlDatabase::parse(&saved, fantasydemo_defs()).expect("it reloads");
        assert!(again.to_xml().contains("<Thing>"));
        assert!(load_entity(&again, &key(2, 2)).is_ok());
    }

    // -- the storage face ------------------------------------------------------

    #[test]
    fn get_reports_a_missing_row_and_a_type_mismatch_as_none() {
        let database = demo();
        assert_eq!(
            database.get_entity(&key(1, 99)).expect("the read succeeds"),
            None
        );
        assert_eq!(
            database.get_entity(&key(2, 1)).expect("the read succeeds"),
            None,
            "dbID 1 is an Avatar, not an Account"
        );
    }

    #[test]
    fn an_update_renames_and_moves_the_section_to_the_end() {
        let mut database = demo();
        let mut values = avatar_values("Renamed");
        values[1] = PropertyValue::Int(10);
        let db_id = database
            .put_entity(&key(1, 1), &values, false, UpdateAutoLoad::Retain)
            .expect("the update lands");

        assert_eq!(db_id, 1);
        assert_eq!(
            record_of(&database, 1, 1).values,
            vec![
                PropertyValue::String("Renamed".to_owned()),
                PropertyValue::Int(10),
                PropertyValue::Int(2),
                PropertyValue::Int(3),
            ]
        );
        assert_eq!(
            database.find_entity_by_name(1, "FantasyHero"),
            None,
            "the old name is gone from the map"
        );
        assert_eq!(database.find_entity_by_name(1, "Renamed"), Some(1));

        let text = database.to_xml();
        let avatar_at = text.find("<Avatar>").expect("the avatar is there");
        let info_at = text.find("<_BigWorldInfo>").expect("the info is there");
        assert!(
            avatar_at > info_at,
            "the updated section moved to the document's end"
        );

        let again = XmlDatabase::parse(&text, fantasydemo_defs()).expect("it saves");
        assert_eq!(record_of(&again, 1, 1).values[1], PropertyValue::Int(10));
    }

    #[test]
    fn an_update_keeps_the_cell_sections_the_def_face_does_not_read() {
        let mut database = demo();
        database
            .put_entity(
                &key(1, 1),
                &avatar_values("FantasyHero"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the update lands");

        let text = database.to_xml();
        assert!(
            text.contains("<position>\t0.0 0.0 0.0\t</position>"),
            "the preserved position survives the save: {text}"
        );
        assert!(text.contains("<spaceID>\t1\t</spaceID>"));
    }

    #[test]
    fn a_new_entity_appends_with_the_next_id() {
        let mut database = demo();
        let db_id = database
            .put_entity(
                &key(1, PENDING_DATABASE_ID),
                &avatar_values("Rookie"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the create lands");

        assert_eq!(db_id, 3, "maxID was 2");
        assert_eq!(record_of(&database, 1, 3).values, avatar_values("Rookie"));
        assert_eq!(database.find_entity_by_name(1, "Rookie"), Some(3));
        assert!(database.to_xml().ends_with("</Avatar>\n</db.xml>\n"));

        let again =
            XmlDatabase::parse(&database.to_xml(), fantasydemo_defs()).expect("the create saves");
        assert_eq!(again.max_id, 3);
        assert_eq!(
            load_entity(&again, &key(1, 3))
                .expect("the row is there")
                .values,
            avatar_values("Rookie")
        );
    }

    #[test]
    fn a_taken_name_fails_the_put_and_keeps_the_old_row() {
        let mut database = demo();
        let error = database
            .put_entity(
                &key(1, PENDING_DATABASE_ID),
                &avatar_values("FantasyHero"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect_err("the name is taken");
        assert_eq!(error, DbError::AlreadyExists);

        // The name maps are per type (`nameToIDMaps_`), so renaming the
        // Avatar to the Account's name is legal — but taking the other
        // Avatar's name is not. Create a second Avatar first.
        let rookie = database
            .put_entity(
                &key(1, PENDING_DATABASE_ID),
                &avatar_values("Rookie"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the create lands");
        let rename = database
            .put_entity(
                &key(1, rookie),
                &avatar_values("FantasyHero"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect_err("the other avatar owns that name");
        assert_eq!(rename, DbError::AlreadyExists);
        assert_eq!(
            record_of(&database, 1, rookie).values,
            avatar_values("Rookie"),
            "the failed rename left the row alone"
        );
        assert_eq!(database.find_entity_by_name(1, "Rookie"), Some(rookie));

        // Renaming an entity to its own name is not a conflict.
        database
            .put_entity(
                &key(1, rookie),
                &avatar_values("Rookie"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the same name is fine");

        // A cross-type name clash is fine too: the Account owns
        // FantasyAccount, but the Avatar's map does not.
        database
            .put_entity(
                &key(1, rookie),
                &avatar_values("FantasyAccount"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the other type's map is separate");
    }

    #[test]
    fn an_update_of_a_missing_or_foreign_row_is_not_found() {
        let mut database = demo();
        assert_eq!(
            database.put_entity(
                &key(1, 99),
                &avatar_values("x"),
                false,
                UpdateAutoLoad::Retain
            ),
            Err(DbError::NotFound)
        );
        assert_eq!(
            database.put_entity(
                &key(2, 1),
                &avatar_values("x"),
                false,
                UpdateAutoLoad::Retain
            ),
            Err(DbError::NotFound),
            "dbID 1 is an Avatar, not an Account"
        );
    }

    #[test]
    fn a_values_vector_that_fits_no_def_is_rejected() {
        let mut database = demo();
        let error = database
            .put_entity(
                &key(1, PENDING_DATABASE_ID),
                &[],
                false,
                UpdateAutoLoad::Retain,
            )
            .expect_err("the empty vector fits no def");
        assert!(
            error.to_string().contains("takes 4 values, got 0"),
            "the error reports the arity: {error}"
        );

        // The same guard guards an update.
        let update = database
            .put_entity(&key(1, 1), &[], false, UpdateAutoLoad::Retain)
            .expect_err("the empty vector fits the update either");
        assert!(
            update.to_string().contains("takes 4 values, got 0"),
            "the update reports the arity: {update}"
        );
    }

    #[test]
    fn an_identifierless_type_creates_without_a_name_entry() {
        let defs = vec![def("Stat", 7, vec![int_prop("count")], None)];
        let mut database = XmlDatabase::create(defs);
        let db_id = database
            .put_entity(
                &key(7, PENDING_DATABASE_ID),
                &[PropertyValue::Int(5)],
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the create lands");
        assert_eq!(db_id, 1);
        assert_eq!(
            record_of(&database, 7, 1).values,
            vec![PropertyValue::Int(5)]
        );
        assert_eq!(
            database.find_entity_by_name(7, "5"),
            None,
            "no identifier, no name map entry"
        );
        assert_eq!(
            database.del_entity(&key(7, 1)),
            Ok(()),
            "the identifierless row deletes cleanly"
        );
    }

    #[test]
    fn an_unknown_type_is_invalid_on_put() {
        let mut database = demo();
        let error = database
            .put_entity(
                &key(9, PENDING_DATABASE_ID),
                &avatar_values("x"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect_err("type 9 has no def");
        assert!(error.to_string().contains("entity type 9 is invalid"));
    }

    #[test]
    fn a_delete_removes_the_row_name_section_and_auto_load() {
        let mut database = demo();
        database.add_auto_load(1);
        assert_eq!(database.auto_load_ids(), vec![1]);

        database.del_entity(&key(1, 1)).expect("the delete lands");
        assert_eq!(database.get_entity(&key(1, 1)), Ok(None));
        assert_eq!(database.find_entity_by_name(1, "FantasyHero"), None);
        assert_eq!(database.auto_load_ids(), Vec::<DatabaseId>::new());
        assert!(
            !database.to_xml().contains("<Avatar>"),
            "the section is gone from the document"
        );
        assert!(
            database.to_xml().contains("<_BigWorldInfo>"),
            "the info section stays"
        );

        assert_eq!(
            database.del_entity(&key(1, 1)),
            Err(DbError::NotFound),
            "a repeated delete is a missing row"
        );
        assert_eq!(
            database.del_entity(&key(1, 2)),
            Err(DbError::NotFound),
            "the type must match: dbID 2 is an Account"
        );
    }

    #[test]
    fn all_entities_lists_the_rows_in_type_then_id_order() {
        let mut database = demo();
        database
            .put_entity(
                &key(1, PENDING_DATABASE_ID),
                &avatar_values("Rookie"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the create lands");
        let records = database.all_entities().expect("the listing succeeds");
        let keys: Vec<(u16, DatabaseId)> = records
            .iter()
            .map(|record| (record.type_id, record.db_id))
            .collect();
        assert_eq!(keys, vec![(1, 1), (1, 3), (2, 2)]);
    }

    // -- the runtime-only occupancy face ----------------------------------------

    #[test]
    fn occupancy_is_runtime_only_and_never_persisted() {
        let mut database = demo();
        let holder = BaseMailbox {
            addr: mercury::endpoint::Address {
                ip: 1,
                port: 30000,
                salt: 7,
            },
            entity_id: 42,
        };
        assert_eq!(
            database.check_out(&key(1, 1), holder.clone()),
            Ok(CheckOut::Granted)
        );
        assert_eq!(
            database.logon_of(&key(1, 1)).expect("the read succeeds"),
            Some(holder.clone())
        );
        assert_eq!(
            database.logon_of(&key(1, 2)).expect("the read succeeds"),
            None,
            "the other row is free"
        );

        // A second taker learns who holds the row, and does not take it.
        let rival = BaseMailbox {
            addr: mercury::endpoint::Address {
                ip: 9,
                port: 30000,
                salt: 8,
            },
            entity_id: 43,
        };
        assert_eq!(
            database.check_out(&key(1, 1), rival),
            Ok(CheckOut::Elsewhere(holder.clone()))
        );
        assert_eq!(
            database.logon_of(&key(1, 1)).expect("the read succeeds"),
            Some(holder.clone()),
            "the first holder keeps the row"
        );

        let saved =
            XmlDatabase::parse(&database.to_xml(), fantasydemo_defs()).expect("the document saves");
        assert_eq!(
            saved.logon_of(&key(1, 1)).expect("the read succeeds"),
            None,
            "the active set is not part of the document"
        );

        database.check_in(&key(1, 1)).expect("the release succeeds");
        assert_eq!(
            database.logon_of(&key(1, 1)).expect("the read succeeds"),
            None
        );
        assert_eq!(
            database.check_out(
                &key(1, 99),
                BaseMailbox {
                    addr: mercury::endpoint::Address {
                        ip: 9,
                        port: 30000,
                        salt: 7,
                    },
                    entity_id: 43,
                }
            ),
            Err(DbError::NotFound),
            "a checkout needs a row"
        );
        assert_eq!(
            database.logon_of(&key(1, 1)).expect("the read succeeds"),
            None,
            "the failed checkout did not take the freed row"
        );
    }

    // -- the auto-load face ------------------------------------------------------

    #[test]
    fn auto_load_adds_and_removes_document_children() {
        let mut database = demo();
        database.add_auto_load(1);
        database.add_auto_load(1);
        assert_eq!(database.auto_load_ids(), vec![1], "a repeat add is a no-op");
        database.add_auto_load(2);
        assert_eq!(database.auto_load_ids(), vec![1, 2]);
        let text = database.to_xml();
        assert!(
            text.contains("<AutoLoadedEntities>\n\t\t\t<databaseID>\t1\t</databaseID>\n"),
            "the children land under the info section: {text}"
        );

        let mut again = XmlDatabase::parse(&text, fantasydemo_defs()).expect("it loads");
        assert_eq!(again.auto_load_ids(), vec![1, 2]);

        again.remove_auto_load(1);
        assert_eq!(again.auto_load_ids(), vec![2]);
        let stripped = again.to_xml();
        assert!(
            stripped.contains("<AutoLoadedEntities>\n\t\t\t<databaseID>\t2\t</databaseID>\n\t\t</AutoLoadedEntities>"),
            "only the survivor's child is left: {stripped}"
        );
        again.remove_auto_load(1);
        assert_eq!(again.auto_load_ids(), vec![2], "a repeat remove is a no-op");
    }

    #[test]
    fn a_missing_auto_load_section_is_created_at_startup() {
        // No `_BigWorldInfo` at all: startup creates the skeleton the way
        // `findOrCreateInfoSection` does.
        let text = r#"<db.xml><Account><playerName>	lone	</playerName><character>	c	</character><databaseID>	2	</databaseID></Account></db.xml>"#;
        let database = XmlDatabase::parse(text, fantasydemo_defs()).expect("it loads");
        let text = database.to_xml();
        assert!(
            text.contains("\t<_BigWorldInfo>\n\t\t<AutoLoadedEntities />\n\t</_BigWorldInfo>\n"),
            "the skeleton is created: {text}"
        );
        assert!(
            text.contains("<_BigWorldInfo>"),
            "and it comes before the entities"
        );

        // A `_BigWorldInfo` without the auto-load child gains it too.
        let text = "<db.xml><_BigWorldInfo><LogOnMapping/></_BigWorldInfo></db.xml>";
        let database = XmlDatabase::parse(text, fantasydemo_defs()).expect("it loads");
        assert!(database.to_xml().contains("<AutoLoadedEntities />"));
    }

    #[test]
    fn an_empty_document_creates_the_info_skeleton() {
        let database = XmlDatabase::create(fantasydemo_defs());
        let text = database.to_xml();
        assert_eq!(
            text,
            "<db.xml>\n\t<_BigWorldInfo>\n\t\t<AutoLoadedEntities />\n\t</_BigWorldInfo>\n</db.xml>\n"
        );
        let again = XmlDatabase::parse(&text, fantasydemo_defs()).expect("it reloads");
        assert_eq!(again.all_entities().expect("the listing succeeds"), vec![]);
    }

    // -- the transaction face ------------------------------------------------------

    #[test]
    fn a_failed_transaction_restores_the_document() {
        let mut database = demo();
        let mut transaction = EntityTransaction::new(&mut database);
        transaction
            .put(
                key(1, PENDING_DATABASE_ID),
                avatar_values("Rookie"),
                false,
                UpdateAutoLoad::Retain,
            )
            .put(
                key(1, 99),
                avatar_values("ghost"),
                false,
                UpdateAutoLoad::Retain,
            );
        let error = transaction
            .commit()
            .expect_err("the second write misses its row");
        assert_eq!(error, DbError::NotFound);

        assert_eq!(database.max_id, 2, "the create's id is undone");
        assert_eq!(database.get_entity(&key(1, 3)), Ok(None));
        let text = database.to_xml();
        assert!(
            !text.contains("Rookie"),
            "the rolled-back section is not in the document"
        );
        assert!(
            text.contains("<Avatar>"),
            "the untouched entity is still there"
        );

        // And the database keeps working after the restore.
        database
            .put_entity(
                &key(1, PENDING_DATABASE_ID),
                &avatar_values("Rookie"),
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the retry lands");
        assert_eq!(database.max_id, 3);
    }

    #[test]
    fn images_are_debuggable_across_kinds() {
        let mut database = demo();
        let image = database.image();
        assert!(
            format!("{image:?}").contains("Xml"),
            "the image names its kind"
        );

        let mut memory = MemoryStorage::new();
        let memory_image = memory.image();
        assert!(format!("{memory_image:?}").contains("Memory"));

        database.restore_image(&image);
        memory.restore_image(&memory_image);
        assert_eq!(
            record_of(&database, 1, 1).values,
            avatar_values("FantasyHero")
        );
        assert!(
            memory
                .all_entities()
                .expect("the listing succeeds")
                .is_empty()
        );
    }
}
