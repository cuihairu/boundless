//! BW-01-03-02 packed binary sections, an exact port of
//! `resmgr/packed_section.cpp`.
//!
//! File layout (all integers little-endian):
//!
//! ```text
//! u32  magic 0x62a14e45
//! u8   version 0
//!      string table: NUL-terminated strings, ended by an empty string
//!      (the double NUL). Strings are indexed in stored order; the writer
//!      stores them sorted (`BW::map`).
//!      root block (see [`PackedSection`]).
//! ```
//!
//! A block with children is `[i16 count][count × record][u32 last]` where
//! a record is `[u32 dataPos][i16 keyPos]`. The physical layout is shifted
//! by one against the logical one: `record[k].dataPos` holds the *start*
//! of child `k` (the end position of child `k-1`, or of the section's own
//! data for `k == 0`) and `record[k].keyPos` names child `k`; the *end*
//! and *type* of child `k` live in `record[k+1].dataPos`, whose final
//! instance is the trailing `u32`. This mirrors the reference's
//! `setEndPosAndType` writing through `(this + 1)->dataPos_`.
//!
//! Positions are relative to the end of the block header.

use crate::base64;
use crate::datasection::DataSection;
use crate::datasection::stream_float;
use foundation::math::{Matrix, Vector2, Vector3, Vector4};

/// `PACKED_SECTION_MAGIC`.
pub const PACKED_SECTION_MAGIC: u32 = 0x62a1_4e45;

/// `PACKED_SECTION_VERSION`.
pub const PACKED_SECTION_VERSION: u8 = 0;

/// `DATA_POS_MASK`.
pub const DATA_POS_MASK: u32 = 0x0fff_ffff;

/// `TYPE_MASK`.
pub const TYPE_MASK: u32 = !DATA_POS_MASK;

/// `SectionType` from `packed_section.hpp`. Kept as the raw 32-bit value
/// because the reference casts unchecked high bits straight into the enum
/// and every accessor treats unknown values as the error case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionType(pub u32);

impl SectionType {
    pub const DATA_SECTION: Self = Self(0x0000_0000);
    pub const STRING: Self = Self(0x1000_0000);
    pub const INT: Self = Self(0x2000_0000);
    /// Used for Vector[234] and Matrix too.
    pub const FLOAT: Self = Self(0x3000_0000);
    pub const BOOL: Self = Self(0x4000_0000);
    pub const BLOB: Self = Self(0x5000_0000);
    pub const ENCRYPTED_BLOB: Self = Self(0x6000_0000);
    pub const ERROR: Self = Self(0x7000_0000);

    fn from_bits(bits: u32) -> Self {
        Self(bits & TYPE_MASK)
    }
}

/// `BW_ENCRYPTION_KEY`: the key shipped in the reference binaries
/// (`HIDE_ENCRYPTION_KEY` not defined).
const BW_ENCRYPTION_KEY: u8 = 0xc9;
const BW_ENCRYPTED_BLOCK: u8 = 1;
const BW_DECRYPTED_BLOCK: u8 = 0;

/// Writer failure: the only fatal case in the reference conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    /// A section with more than `i16::MAX` children cannot be packed
    /// (`NumChildrenType` overflow).
    TooManyChildren { tag: String },
}

/// A parsed packed section tree. The reference keeps pointers into the
/// file, but every observable property is captured eagerly here: name,
/// own data window with its type, and the parsed children.
#[derive(Debug, Clone, PartialEq)]
pub struct PackedSection {
    name: String,
    own_type: SectionType,
    own_data: Vec<u8>,
    /// Stated own-data length. Differs from `own_data.len()` only on the
    /// malformed files the reference tolerates with a one-byte slack.
    own_len: usize,
    /// Set for `TYPE_DATA_SECTION` blocks; the children count as written.
    num_children: Option<i16>,
    children: Vec<PackedSection>,
}

impl PackedSection {
    /// Whether the data starts with the packed-section magic
    /// (`PackedSection::create`'s check).
    pub fn is_packed_file(data: &[u8]) -> bool {
        data.len() >= 5 && u32_le(data, 0) == PACKED_SECTION_MAGIC
    }

    /// Parse a whole packed file (`PackedSectionFile::createRoot`). The
    /// magic must be checked with [`PackedSection::is_packed_file`] first;
    /// malformed content degrades to an empty string-typed section exactly
    /// like the reference constructor's error path.
    pub fn parse(name: &str, data: &[u8]) -> PackedSection {
        let (strings, consumed) = init_string_table(&data[5..]);
        let used = 5 + consumed;
        // init_string_table never consumes past the buffer, so `used` is
        // always a valid slice start (an empty window degrades the root).
        let root_window = &data[used..];
        parse_window(name, root_window, SectionType::DATA_SECTION, &strings)
    }

    /// `PackedSection::sectionName`.
    pub fn section_name(&self) -> &str {
        &self.name
    }

    /// The section's own data type (after encrypted-blob decryption).
    pub fn own_type(&self) -> SectionType {
        self.own_type
    }

    /// `PackedSection::countChildren`.
    pub fn count_children(&self) -> usize {
        self.children.len()
    }

    /// `PackedSection::openChild` (parsed children; matrix rows are
    /// materialised eagerly).
    pub fn open_child(&self, index: usize) -> Option<&PackedSection> {
        self.children.get(index)
    }

    /// `PackedSection::findChild`: first child whose name matches, with the
    /// matrix `row0..row3` synthesis already covered by the materialised
    /// rows.
    pub fn find_child(&self, tag: &str) -> Option<&PackedSection> {
        self.children.iter().find(|c| c.name == tag)
    }

    /// `isMatrix`: float data sized as four Vector3s.
    pub fn is_matrix(&self) -> bool {
        self.own_type == SectionType::FLOAT && self.own_len == 4 * 3 * 4
    }

    // ------------------------------------------------------------------
    // Value accessors (packed_section.cpp "Value accessors").
    // ------------------------------------------------------------------

    /// `PackedSection::asBool`: bool data is true iff any byte is stored;
    /// everything else falls back to the string form.
    pub fn as_bool(&self, default: bool) -> bool {
        if self.own_type == SectionType::BOOL {
            return self.own_len > 0;
        }
        self.fallback().as_bool(default)
    }

    /// `PackedSection::asInt`.
    ///
    /// An INT section with a corrupt length (not 0/1/2/4/8) reaches the
    /// string fallback, where the reference recurses forever between
    /// `asString` and the virtual `asInt64` — reproduced by this port
    /// as unbounded recursion, never exercised on well-formed files.
    pub fn as_int(&self, default: i32) -> i32 {
        if self.own_type == SectionType::INT
            && let Some(v) = get_integer_val(&self.own_data)
        {
            return v as i32;
        }
        self.fallback().as_int(default)
    }

    /// `PackedSection::asLong` is `asInt` on every 64-bit platform build
    /// of the reference ("This is incorrect for 64bit platforms" — their
    /// comment).
    pub fn as_long(&self, default: i32) -> i32 {
        self.as_int(default)
    }

    /// `PackedSection::asUInt`.
    ///
    /// An INT section with a corrupt length (not 0/1/2/4/8) reaches the
    /// string fallback, where the reference recurses forever between
    /// `asString` and the virtual `asInt64` — reproduced by this port
    /// as unbounded recursion, never exercised on well-formed files.
    pub fn as_uint(&self, default: u32) -> u32 {
        if self.own_type == SectionType::INT
            && let Some(v) = get_integer_val(&self.own_data)
        {
            return v as u32;
        }
        self.fallback().as_uint(default)
    }

    /// `PackedSection::asInt64`.
    ///
    /// An INT section with a corrupt length (not 0/1/2/4/8) reaches the
    /// string fallback, where the reference recurses forever between
    /// `asString` and the virtual `asInt64` — reproduced by this port
    /// as unbounded recursion, never exercised on well-formed files.
    pub fn as_int64(&self, default: i64) -> i64 {
        if self.own_type == SectionType::INT
            && let Some(v) = get_integer_val(&self.own_data)
        {
            return v;
        }
        self.fallback().as_int64(default)
    }

    /// `PackedSection::asUInt64` always goes through the string form —
    /// the reference has no packed representation for it.
    pub fn as_uint64(&self, default: u64) -> u64 {
        self.fallback().as_uint64(default)
    }

    /// `PackedSection::asFloat`.
    pub fn as_float(&self, default: f32) -> f32 {
        match self.own_type {
            SectionType::FLOAT => match self.own_len {
                0 => 0.0,
                4 if self.own_data.len() >= 4 => f32_le(&self.own_data, 0),
                // Incorrect lengths warn and fall back in the reference.
                _ => self.fallback().as_float(default),
            },
            SectionType::INT => self.as_int(0) as f32,
            SectionType::STRING => {
                if self.own_len > 0 {
                    // The reference extracts from an istringstream and only
                    // checks fail — trailing junk is fine here.
                    if let Some((v, _)) = stream_float(&self.own_data, 0) {
                        return v as f32;
                    }
                }
                default
            }
            _ => self.fallback().as_float(default),
        }
    }

    /// `PackedSection::asDouble` is `asFloat` narrowed through a float.
    pub fn as_double(&self, default: f64) -> f64 {
        self.as_float(default as f32) as f64
    }

    /// `PackedSection::asString`.
    pub fn as_string(&self) -> String {
        match self.own_type {
            SectionType::STRING => String::from_utf8_lossy(&self.own_data).into_owned(),
            SectionType::BLOB => base64::encode(&self.own_data),
            SectionType::INT => {
                // Big uint64s are handled as string by the writer; the
                // reader prints the signed value.
                self.as_int64(0).to_string()
            }
            SectionType::FLOAT if self.is_matrix() => {
                // A matrix is an empty string; it has children.
                String::new()
            }
            SectionType::FLOAT if self.own_len.is_multiple_of(4) => {
                let mut ds = DataSection::new("scratch");
                match self.own_len / 4 {
                    1 => ds.set_float(self.as_float(0.0)),
                    2 => ds.set_vector2(self.as_vector2(Vector2::ZERO)),
                    3 => ds.set_vector3(self.as_vector3(Vector3::ZERO)),
                    4 => ds.set_vector4(self.as_vector4(Vector4::ZERO)),
                    _ => ds.set_string(""),
                }
                ds.as_string().to_string()
            }
            SectionType::FLOAT => String::new(),
            SectionType::BOOL => {
                if self.as_bool(false) {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            _ => String::new(),
        }
    }

    /// `PackedSection::asVector2`.
    pub fn as_vector2(&self, default: Vector2) -> Vector2 {
        if self.own_type == SectionType::FLOAT && self.own_len == 8 {
            return Vector2::new(f32_le(&self.own_data, 0), f32_le(&self.own_data, 4));
        }
        self.fallback().as_vector2(default)
    }

    /// `PackedSection::asVector3`.
    pub fn as_vector3(&self, default: Vector3) -> Vector3 {
        if self.own_type == SectionType::FLOAT && self.own_len == 12 {
            return Vector3::new(
                f32_le(&self.own_data, 0),
                f32_le(&self.own_data, 4),
                f32_le(&self.own_data, 8),
            );
        }
        self.fallback().as_vector3(default)
    }

    /// `PackedSection::asVector4`.
    pub fn as_vector4(&self, default: Vector4) -> Vector4 {
        if self.own_type == SectionType::FLOAT && self.own_len == 16 {
            return Vector4::new(
                f32_le(&self.own_data, 0),
                f32_le(&self.own_data, 4),
                f32_le(&self.own_data, 8),
                f32_le(&self.own_data, 12),
            );
        }
        self.fallback().as_vector4(default)
    }

    /// `PackedSection::asMatrix34`.
    pub fn as_matrix34(&self, default: Matrix) -> Matrix {
        let mut m = default;
        if self.own_type == SectionType::FLOAT && self.own_len == 4 * 12 {
            for (i, row) in m.m.iter_mut().enumerate().take(4) {
                for (j, cell) in row.iter_mut().enumerate().take(3) {
                    *cell = f32_le(&self.own_data, (i * 3 + j) * 4);
                }
            }
        } else {
            m.m[0] = row_of(self, "row0", default.m[0]);
            m.m[1] = row_of(self, "row1", default.m[1]);
            m.m[2] = row_of(self, "row2", default.m[2]);
            m.m[3] = row_of(self, "row3", default.m[3]);
        }
        m.m[0][3] = 0.0;
        m.m[1][3] = 0.0;
        m.m[2][3] = 0.0;
        m.m[3][3] = 1.0;
        m
    }

    /// `PackedSection::asBlob`.
    pub fn as_blob(&self, default: &str) -> Vec<u8> {
        if self.own_type == SectionType::BLOB {
            return self.own_data.clone();
        }
        if self.own_type == SectionType::STRING && self.own_len == 0 {
            return Vec::new();
        }
        if (self.own_type == SectionType::INT || self.own_type == SectionType::BOOL)
            && let Some(decoded) = base64::decode_strict(&self.as_string())
        {
            return decoded;
        }
        // The XMLSection fallback ignores defaultVal entirely.
        let _ = default;
        self.fallback().as_blob()
    }

    /// `PackedSection::asBinary`: blob bytes, else the blob form of the
    /// value.
    pub fn as_binary(&self) -> Vec<u8> {
        if self.own_type == SectionType::BLOB {
            return self.own_data.clone();
        }
        self.as_blob("")
    }

    // ------------------------------------------------------------------
    // Helpers.
    // ------------------------------------------------------------------

    /// The reference's error fallback: a scratch XML section loaded with
    /// this section's string form, queried with XML semantics.
    fn fallback(&self) -> DataSection {
        let mut ds = DataSection::new("scratch");
        ds.set_string(&self.as_string());
        ds
    }
}

fn row_of(section: &PackedSection, tag: &str, default_row: [f32; 4]) -> [f32; 4] {
    let v = section
        .find_child(tag)
        .map(|c| c.as_vector3(Vector3::new(default_row[0], default_row[1], default_row[2])))
        .unwrap_or(Vector3::new(default_row[0], default_row[1], default_row[2]));
    [v.x, v.y, v.z, default_row[3]]
}

/// `getIntegerVal`: own data interpreted by its stored length. Zero bytes
/// mean zero; 1/2/4/8 byte forms are read as signed little-endian values
/// (the reference reads through the signed types).
fn get_integer_val(data: &[u8]) -> Option<i64> {
    match data.len() {
        0 => Some(0),
        1 => Some(data[0] as i8 as i64),
        2 => Some(i16_le(data, 0) as i64),
        4 => Some(i32_le(data, 0) as i64),
        8 => Some(i64_le(data, 0)),
        _ => None,
    }
}

/// `decryptBinaryBlock`. Returns `None` on the reference's error path.
fn decrypt_block(data: &[u8]) -> Option<Vec<u8>> {
    match *data.first()? {
        BW_ENCRYPTED_BLOCK => Some(data[1..].iter().map(|b| b ^ BW_ENCRYPTION_KEY).collect()),
        BW_DECRYPTED_BLOCK => Some(data[1..].to_vec()),
        _ => None,
    }
}

/// `PackedSectionFile::StringTable::init`: NUL-separated strings ended by
/// an empty string. On failure the whole input is consumed (which makes
/// the root block degrade to the empty section, like the reference).
fn init_string_table(data: &[u8]) -> (Vec<String>, usize) {
    let len = data.len();
    let mut pos = 0usize;
    let mut table: Vec<String> = Vec::new();

    while pos != len && data[pos] != 0 {
        let start = pos;
        while pos < len && data[pos] != 0 {
            pos += 1;
        }
        if pos == len {
            // Ran to the end without a NUL: the reference reports "not
            // enough data" and consumes everything.
            return (Vec::new(), len);
        }
        table.push(String::from_utf8_lossy(&data[start..pos]).into_owned());
        pos += 1; // the string's NUL
    }

    if pos == len {
        // The terminating empty string never appeared.
        return (Vec::new(), len);
    }
    pos += 1; // the extra terminator NUL
    (table, pos)
}

/// Parse one section window: the section's total data (own block plus all
/// descendants) with the type taken from the parent's record.
fn parse_window(name: &str, bytes: &[u8], ty: SectionType, strings: &[String]) -> PackedSection {
    if ty == SectionType::DATA_SECTION {
        return parse_children_block(name, bytes, strings);
    }

    let (own_data, own_type) = if ty == SectionType::ENCRYPTED_BLOB {
        match decrypt_block(bytes) {
            Some(data) => (data, SectionType::BLOB),
            None => return degraded(name),
        }
    } else {
        (bytes.to_vec(), ty)
    };

    let mut section = PackedSection {
        name: name.to_string(),
        own_type,
        own_len: own_data.len(),
        own_data,
        num_children: None,
        children: Vec::new(),
    };
    materialise_matrix_rows(&mut section);
    section
}

/// The reference constructor's error path: empty string-typed section.
fn degraded(name: &str) -> PackedSection {
    PackedSection {
        name: name.to_string(),
        own_type: SectionType::STRING,
        own_data: Vec::new(),
        own_len: 0,
        num_children: None,
        children: Vec::new(),
    }
}

/// `TYPE_DATA_SECTION` window: header, validation with the reference's
/// one-byte slack, own data, then each child window parsed recursively.
fn parse_children_block(name: &str, bytes: &[u8], strings: &[String]) -> PackedSection {
    if bytes.len() < 6 {
        // The reference reads the first record's position after checking
        // only two bytes; anything shorter is malformed beyond recovery.
        return degraded(name);
    }
    let num_children = i16_le(bytes, 0);
    let offset = 2 + num_children as i64 * 6 + 4;
    let first_pos = u32_le(bytes, 2);
    let own_len = (first_pos & DATA_POS_MASK) as i64;
    let own_type = SectionType::from_bits(first_pos);

    if offset < 0 || bytes.len() as i64 + 1 < offset + own_len {
        // "Invalid offset" — the reference allows one byte of slack.
        return degraded(name);
    }

    let data_start = offset as usize;
    let own_end = (data_start + own_len as usize).min(bytes.len());
    let own_data = bytes[data_start..own_end].to_vec();

    // The constructor decrypts its own data block whatever the depth: a
    // numChildren=0 root typed ENCRYPTED_BLOB still lands here.
    let (own_data, own_type) = if own_type == SectionType::ENCRYPTED_BLOB {
        match decrypt_block(&own_data) {
            Some(data) => (data, SectionType::BLOB),
            None => return degraded(name),
        }
    } else {
        (own_data, own_type)
    };
    let own_len = own_data.len();

    let mut children = Vec::with_capacity(num_children.unsigned_abs() as usize);
    for k in 0..num_children {
        let rec = 2 + k as i64 * 6;
        let start = ((u32_le(bytes, rec as usize) & DATA_POS_MASK) as i64 + offset) as usize;
        let key_pos = i16_le(bytes, rec as usize + 4);
        let end_rec = rec + 6;
        let end_pos = u32_le(bytes, end_rec as usize);
        let end = ((end_pos & DATA_POS_MASK) as i64 + offset) as usize;
        let child_type = SectionType::from_bits(end_pos);

        let name_owned = strings.get(key_pos as usize).cloned().unwrap_or_default();
        let lo = start.min(bytes.len());
        let hi = end.min(bytes.len());
        if hi < lo {
            return degraded(name);
        }
        children.push(parse_window(
            &name_owned,
            &bytes[lo..hi],
            child_type,
            strings,
        ));
    }

    let n = num_children.max(0);
    let mut section = PackedSection {
        name: name.to_string(),
        own_type,
        own_len,
        own_data,
        num_children: Some(n),
        children,
    };
    materialise_matrix_rows(&mut section);
    section
}

/// A 48-byte float section is a Matrix34: synthesise the four rows the
/// reference creates on demand (`openChild`/`findChild` matrix special
/// case).
fn materialise_matrix_rows(section: &mut PackedSection) {
    if !section.is_matrix() {
        return;
    }
    let mut rows = Vec::with_capacity(4);
    for i in 0..4 {
        let start = i * 12;
        rows.push(PackedSection {
            name: format!("row{i}"),
            own_type: SectionType::FLOAT,
            own_len: 12,
            own_data: section.own_data[start..start + 12].to_vec(),
            num_children: None,
            children: Vec::new(),
        });
    }
    section.children = rows;
}

// ----------------------------------------------------------------------
// Writer (convertToPackedFile and helpers).
// ----------------------------------------------------------------------

/// Serialise a data-section tree into the packed file format
/// (`convertToPackedFile`).
pub fn pack(root: &DataSection) -> Result<Vec<u8>, PackError> {
    let mut out = Vec::new();
    out.extend_from_slice(&PACKED_SECTION_MAGIC.to_le_bytes());
    out.push(PACKED_SECTION_VERSION);

    // The string table collects every child tag in sorted order (the
    // reference's BW::map), skipping matrix subtrees; the root's own name
    // is never added.
    let mut names = std::collections::BTreeMap::new();
    populate_string_table(root, &mut names);
    for name in names.keys() {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
    }
    out.push(0);

    // Index map: keys iterate in sorted order, which fixes the stored
    // indices (`calculateSaveSizeAndUpdate`).
    let indices: std::collections::HashMap<&str, u16> = names
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i as u16))
        .collect();

    write_node(root, true, &indices, &mut out)?;
    Ok(out)
}

/// `OutputStringTable::populate`.
fn populate_string_table(ds: &DataSection, names: &mut std::collections::BTreeMap<String, ()>) {
    if is_matrix(ds) {
        return;
    }
    for child in children_of(ds) {
        names.insert(child.section_name().to_string(), ());
        populate_string_table(child, names);
    }
}

/// `writeAsPackedSection`.
fn write_node(
    ds: &DataSection,
    is_root: bool,
    indices: &std::collections::HashMap<&str, u16>,
    out: &mut Vec<u8>,
) -> Result<SectionType, PackError> {
    let children = children_of(ds);
    let num_children = children.len();
    if num_children > i16::MAX as usize {
        return Err(PackError::TooManyChildren {
            tag: ds.section_name().to_string(),
        });
    }

    if num_children == 0 && !is_root {
        return Ok(add_best_guess(ds, out));
    }
    if is_matrix(ds) && !is_root {
        for row in children {
            let v = row.as_vector3(Vector3::ZERO);
            out.extend_from_slice(&v.x.to_le_bytes());
            out.extend_from_slice(&v.y.to_le_bytes());
            out.extend_from_slice(&v.z.to_le_bytes());
        }
        return Ok(SectionType::FLOAT);
    }

    // Header: count, records, final position. The bytes are patched as the
    // children stream out.
    let header_pos = out.len();
    out.extend_from_slice(&(num_children as i16).to_le_bytes());
    out.resize(header_pos + 2 + num_children * 6 + 4, 0);

    let anchor = out.len();
    let own_type = add_best_guess(ds, out);
    let own_end = (out.len() - anchor) as u32;
    patch_u32(out, header_pos + 2, own_end | own_type.0);

    for (k, child) in children.iter().enumerate() {
        let child_type = write_node(child, false, indices, out)?;
        let end = (out.len() - anchor) as u32;
        // record[k+1].dataPos carries this child's end and type.
        patch_u32(out, header_pos + 2 + (k + 1) * 6, end | child_type.0);
        // record[k].keyPos names this child.
        let index = indices.get(child.section_name()).copied().unwrap_or(0);
        patch_i16(out, header_pos + 2 + k * 6 + 4, index as i16);
    }

    Ok(SectionType::DATA_SECTION)
}

/// The XML view of a packed leaf: only element children count (attribute
/// nodes exist in our DataSection model but the reference sections used
/// for packing have none).
fn children_of(ds: &DataSection) -> Vec<&DataSection> {
    (0..ds.count_children())
        .filter_map(|i| ds.open_child(i))
        .filter(|c| !c.is_attribute())
        .collect()
}

/// `isMatrix` from packed_section.cpp: four children named row0..row3
/// whose values each parse as exactly three iostream floats.
fn is_matrix(ds: &DataSection) -> bool {
    if ds.count_children() != 4 {
        return false;
    }
    ["row0", "row1", "row2", "row3"].iter().all(|tag| {
        ds.find_child(tag)
            .map(parses_as_stream_vector3)
            .unwrap_or(false)
    })
}

/// `isVector3` helper: three iostream extractions consuming the value.
fn parses_as_stream_vector3(ds: &DataSection) -> bool {
    let bytes = ds.as_string().as_bytes();
    let mut p = 0usize;
    for _ in 0..3 {
        let Some((_, np)) = stream_float(bytes, p) else {
            return false;
        };
        p = np;
    }
    p == bytes.len()
}

/// `addBestGuess`: choose the packed representation for a leaf value.
fn add_best_guess(ds: &DataSection, out: &mut Vec<u8>) -> SectionType {
    let s = ds.as_string();

    // Empty string.
    if s.is_empty() {
        return SectionType::STRING;
    }

    // BOOL (exact strings only).
    if s == "true" {
        out.push(1);
        return SectionType::BOOL;
    }
    if s == "false" {
        return SectionType::BOOL;
    }

    // Big uint64s stay strings to avoid conversion errors downstream.
    if let Some(u) = sscanf_u64(s)
        && u > i64::MAX as u64
        && s == u.to_string()
    {
        out.extend_from_slice(s.as_bytes());
        return SectionType::STRING;
    }

    // INT: the value must reformat to exactly the same string (no leading
    // zeros, no plus, no padding).
    if let Some(v) = stream_i64(s)
        && s == v.to_string()
    {
        match v {
            0 => {}
            v if (-128..=127).contains(&v) => out.push(v as i8 as u8),
            v if (i16::MIN as i64..=i16::MAX as i64).contains(&v) => {
                out.extend_from_slice(&(v as i16).to_le_bytes())
            }
            v if (i32::MIN as i64..=i32::MAX as i64).contains(&v) => {
                out.extend_from_slice(&(v as i32).to_le_bytes())
            }
            v => out.extend_from_slice(&v.to_le_bytes()),
        }
        return SectionType::INT;
    }

    // FLOAT: up to four iostream floats; the reference requires the value
    // to reformat identically through the XML writers.
    if let Some(count) = stream_floats_up_to_4(s) {
        let vals = parse_floats(s, count);
        let f: Vec<f32> = vals.iter().map(|v| *v as f32).collect();
        let mut scratch = DataSection::new("scratch");
        let ok = match count {
            1 => {
                scratch.set_float(f[0]);
                scratch.as_string() == s
            }
            2 => {
                scratch.set_vector2(Vector2::new(f[0], f[1]));
                scratch.as_string() == s
            }
            3 => {
                scratch.set_vector3(Vector3::new(f[0], f[1], f[2]));
                scratch.as_string() == s
            }
            _ => {
                scratch.set_vector4(Vector4::new(f[0], f[1], f[2], f[3]));
                scratch.as_string() == s
            }
        };
        if ok {
            for v in &f {
                out.extend_from_slice(&v.to_le_bytes());
            }
            return SectionType::FLOAT;
        }
        // Numeric but not round-trippable: stored as a string below (the
        // reference warns here).
    }

    // BLOB: only when the base64 form is canonical.
    if let Some(decoded) = base64::decode_strict(s)
        && base64::encode(&decoded) == s
    {
        out.extend_from_slice(&decoded);
        return SectionType::BLOB;
    }

    // STRING (everything else).
    out.extend_from_slice(s.as_bytes());
    SectionType::STRING
}

/// `sscanf("%llu")` acceptance: any digit-bearing prefix converts, with
/// sign wrap and overflow saturation, matching glibc.
fn sscanf_u64(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    let mut p = 0usize;
    while p < bytes.len() && is_c_space(bytes[p]) {
        p += 1;
    }
    let mut negative = false;
    if p < bytes.len() && (bytes[p] == b'+' || bytes[p] == b'-') {
        negative = bytes[p] == b'-';
        p += 1;
    }
    let mut acc: u64 = 0;
    let mut any = false;
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        any = true;
        acc = acc.wrapping_mul(10).wrapping_add((bytes[p] - b'0') as u64);
        p += 1;
    }
    if !any {
        return None;
    }
    Some(if negative { acc.wrapping_neg() } else { acc })
}

/// `stringstream >> int64`: C whitespace, optional sign, digits required,
/// overflow fails, and extraction must reach the end of the value.
fn stream_i64(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    let mut p = 0usize;
    while p < bytes.len() && is_c_space(bytes[p]) {
        p += 1;
    }
    let mut negative = false;
    if p < bytes.len() && (bytes[p] == b'+' || bytes[p] == b'-') {
        negative = bytes[p] == b'-';
        p += 1;
    }
    let mut acc: i64 = 0;
    let mut any = false;
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        any = true;
        acc = acc
            .checked_mul(10)
            .and_then(|v| v.checked_add((bytes[p] - b'0') as i64))?;
        p += 1;
    }
    if !any {
        return None;
    }
    if p != bytes.len() {
        return None; // not eof
    }
    Some(if negative { -acc } else { acc })
}

/// Up to four iostream floats consuming the whole value; returns the count
/// when the value ends exactly after the last one.
fn stream_floats_up_to_4(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut p = 0usize;
    let mut count = 0usize;
    loop {
        let (_, np) = stream_float(bytes, p)?;
        count += 1;
        p = np;
        if count == 4 || p == bytes.len() {
            break;
        }
    }
    if p == bytes.len() { Some(count) } else { None }
}

/// Re-parse the floats already validated by [`stream_floats_up_to_4`].
fn parse_floats(s: &str, count: usize) -> Vec<f64> {
    let bytes = s.as_bytes();
    let mut p = 0usize;
    let mut vals = Vec::with_capacity(count);
    for _ in 0..count {
        let (v, np) = stream_float(bytes, p).expect("validated by stream_floats_up_to_4");
        vals.push(v);
        p = np;
    }
    vals
}

// ----------------------------------------------------------------------
// Byte helpers.
// ----------------------------------------------------------------------

fn u32_le(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn i32_le(data: &[u8], at: usize) -> i32 {
    u32_le(data, at) as i32
}

fn i16_le(data: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([data[at], data[at + 1]])
}

fn i64_le(data: &[u8], at: usize) -> i64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[at..at + 8]);
    i64::from_le_bytes(b)
}

fn f32_le(data: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn patch_u32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn patch_i16(out: &mut [u8], at: usize, value: i16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

/// C `isspace`: space, \t, \n, \r, \v, \f.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a packed file: header, sorted string table, root block.
    fn packed_file(table: &[&str], root_block: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&PACKED_SECTION_MAGIC.to_le_bytes());
        v.push(PACKED_SECTION_VERSION);
        for s in table {
            v.extend_from_slice(s.as_bytes());
            v.push(0);
        }
        v.push(0);
        v.extend_from_slice(root_block);
        v
    }

    // ------------------------------------------------------------------
    // Reader: hand-built fixtures at the byte level.
    // ------------------------------------------------------------------

    #[test]
    fn magic_detection_matches_the_reference_header_check() {
        assert!(PackedSection::is_packed_file(&packed_file(&[], &[])));
        assert!(!PackedSection::is_packed_file(&[0x45]));
        assert!(!PackedSection::is_packed_file(&[]));
        let mut bad = packed_file(&[], &[]);
        bad[0] = 0x00;
        assert!(!PackedSection::is_packed_file(&bad));
    }

    #[test]
    fn simple_string_child_roundtrips_at_byte_level() {
        // Root with one child "name" = "hello". Own data empty (STRING).
        let block = [
            0x01, 0x00, // numChildren = 1
            0x00, 0x00, 0x00, 0x10, // record[0].dataPos = 0 | TYPE_STRING
            0x00, 0x00, // record[0].keyPos = 0 ("name")
            0x05, 0x00, 0x00, 0x10, // final: 5 | TYPE_STRING
        ];
        let mut file = packed_file(&["name"], &block);
        file.extend_from_slice(b"hello");

        let root = PackedSection::parse("root", &file);
        assert_eq!(root.section_name(), "root");
        assert_eq!(root.count_children(), 1);
        let child = root.find_child("name").expect("name child");
        assert_eq!(child.as_string(), "hello");
        assert_eq!(child.own_type(), SectionType::STRING);
        // The root itself is an empty string section.
        assert_eq!(root.as_string(), "");
        assert_eq!(root.own_type(), SectionType::STRING);
    }

    #[test]
    fn integer_width_variants_read_back_signed() {
        // Own i8(-5) then child "n" i16(300).
        let block = [
            0x01, 0x00, // numChildren
            0x01, 0x00, 0x00, 0x20, // own end 1 | TYPE_INT
            0x00, 0x00, // key 0
            0x03, 0x00, 0x00, 0x20, // final: 3 | TYPE_INT
            0xFB, // own: i8 -5
            0x2C, 0x01, // child: i16 300
        ];
        let root = PackedSection::parse("root", &packed_file(&["n"], &block));
        assert_eq!(root.own_type(), SectionType::INT);
        assert_eq!(root.as_int(7), -5, "i8 data sign-extends");
        assert_eq!(root.as_int64(7), -5);
        assert_eq!(root.as_long(7), -5, "asLong is asInt");

        let n = root.find_child("n").unwrap();
        assert_eq!(n.as_int(7), 300, "i16 data");
        assert_eq!(n.as_uint(7), 300);
    }

    #[test]
    fn zero_int_stores_no_bytes() {
        // Own type INT with no data at all reads as 0.
        let block = [
            0x00, 0x00, // numChildren = 0
            0x00, 0x00, 0x00, 0x20, // own end 0 | TYPE_INT (also the final)
        ];
        let root = PackedSection::parse("root", &packed_file(&[], &block));
        assert_eq!(root.as_int(7), 0, "empty INT data is zero");
        assert_eq!(root.as_int64(7), 0);
        assert_eq!(root.count_children(), 0);
    }

    #[test]
    fn bool_data_is_true_iff_bytes_are_stored() {
        let block_true = [0x00, 0x00, 0x01, 0x00, 0x00, 0x40, 0x01];
        let root = PackedSection::parse("t", &packed_file(&[], &block_true));
        assert_eq!(root.own_type(), SectionType::BOOL);
        assert!(root.as_bool(false));
        assert_eq!(root.as_string(), "true");

        let block_false = [0x00, 0x00, 0x00, 0x00, 0x00, 0x40];
        let root = PackedSection::parse("f", &packed_file(&[], &block_false));
        assert!(!root.as_bool(true));
        assert_eq!(root.as_string(), "false");
    }

    #[test]
    fn float_and_vector_data_read_little_endian() {
        // One float 1.5f32 = 00 00 C0 3F.
        let block = [
            0x00, 0x00, //
            0x04, 0x00, 0x00, 0x30, // own end 4 | TYPE_FLOAT
            0x00, 0x00, 0xC0, 0x3F,
        ];
        let root = PackedSection::parse("f", &packed_file(&[], &block));
        assert_eq!(root.as_float(9.0), 1.5);
        assert_eq!(
            root.as_string(),
            "1.500000",
            "floats print via the XML writer"
        );
        assert_eq!(root.as_double(9.0), 1.5);
        // A two-float section: asFloat falls back to the string prefix.
        let block2 = [
            0x00, 0x00, //
            0x08, 0x00, 0x00, 0x30, // own end 8 | TYPE_FLOAT
            0x00, 0x00, 0x80, 0x3F, 0x00, 0x00, 0x00, 0x40, // 1.0, 2.0
        ];
        let root = PackedSection::parse("v", &packed_file(&[], &block2));
        assert_eq!(root.as_float(9.0), 1.0, "vector data reads the first float");
        assert_eq!(
            root.as_vector2(Vector2::new(-1.0, -1.0)),
            Vector2::new(1.0, 2.0)
        );
    }

    #[test]
    fn float_string_data_parses_with_iostream_rules() {
        // TYPE_STRING holding "2.5junk": extraction takes the prefix.
        let block = [
            0x00, 0x00, 0x07, 0x00, 0x00, 0x10, b'2', b'.', b'5', b'j', b'u', b'n', b'k',
        ];
        let root = PackedSection::parse("s", &packed_file(&[], &block));
        assert_eq!(root.as_float(9.0), 2.5);
        // "abc" does not parse.
        let block2 = [0x00, 0x00, 0x03, 0x00, 0x00, 0x10, b'a', b'b', b'c'];
        let root = PackedSection::parse("s", &packed_file(&[], &block2));
        assert_eq!(root.as_float(9.0), 9.0);
    }

    #[test]
    fn matrix_synthesises_row_children() {
        // 48-byte float section: three basis rows plus a translation row.
        let mut data = Vec::new();
        for row in [
            [1.0f32, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [2.0, 3.0, 4.0],
        ] {
            for v in row {
                data.extend_from_slice(&v.to_le_bytes());
            }
        }
        let mut block = vec![0x00, 0x00];
        block.extend_from_slice(&((data.len() as u32) | SectionType::FLOAT.0).to_le_bytes());
        block.extend_from_slice(&data);
        let root = PackedSection::parse("m", &packed_file(&[], &block));

        assert!(root.is_matrix());
        assert_eq!(root.count_children(), 4, "matrix pretends to have 4 rows");
        assert_eq!(root.as_string(), "", "a matrix is an empty string");
        let row2 = root.find_child("row2").expect("row2");
        assert_eq!(row2.as_vector3(Vector3::ZERO), Vector3::new(0.0, 0.0, 1.0));

        let m = root.as_matrix34(Matrix::zero());
        assert_eq!(m.m[0][0], 1.0);
        assert_eq!(m.m[1][1], 1.0);
        assert_eq!(m.m[2][2], 1.0);
        assert_eq!((m.m[3][0], m.m[3][1], m.m[3][2]), (2.0, 3.0, 4.0));
        assert_eq!(m.m[0][3], 0.0);
        assert_eq!(m.m[3][3], 1.0);
    }

    #[test]
    fn encrypted_blob_xor_decrypts() {
        // First byte 1 (encrypted), then "hi" XOR 0xc9.
        let payload = [
            BW_ENCRYPTED_BLOCK,
            b'h' ^ BW_ENCRYPTION_KEY,
            b'i' ^ BW_ENCRYPTION_KEY,
        ];
        let mut block = vec![0x00, 0x00];
        block.extend_from_slice(
            &((payload.len() as u32) | SectionType::ENCRYPTED_BLOB.0).to_le_bytes(),
        );
        block.extend_from_slice(&payload);
        let root = PackedSection::parse("e", &packed_file(&[], &block));
        assert_eq!(root.own_type(), SectionType::BLOB, "decrypted to BLOB");
        assert_eq!(root.as_blob(""), b"hi");
        assert_eq!(root.as_binary(), b"hi");
        assert_eq!(root.as_string(), base64::encode(b"hi"));
    }

    #[test]
    fn blob_string_and_empty_blob_forms() {
        // BLOB holding "foo".
        let mut block = vec![0x00, 0x00];
        block.extend_from_slice(&(3u32 | SectionType::BLOB.0).to_le_bytes());
        block.extend_from_slice(b"foo");
        let root = PackedSection::parse("b", &packed_file(&[], &block));
        assert_eq!(root.as_string(), "Zm9v", "blob prints as base64");
        assert_eq!(root.as_binary(), b"foo");

        // Empty STRING asBlob is empty.
        let block = [0x00, 0x00, 0x00, 0x00, 0x00, 0x10];
        let root = PackedSection::parse("b", &packed_file(&[], &block));
        assert!(root.as_blob("").is_empty());
    }

    #[test]
    fn malformed_root_degrades_to_empty_string_section() {
        // String table without the terminating empty string consumes all
        // data, so the root has no block: empty string section.
        let mut file = Vec::new();
        file.extend_from_slice(&PACKED_SECTION_MAGIC.to_le_bytes());
        file.push(0);
        file.extend_from_slice(b"name"); // never NUL-terminated
        let root = PackedSection::parse("root", &file);
        assert_eq!(root.own_type(), SectionType::STRING);
        assert_eq!(root.as_string(), "");
        assert_eq!(root.count_children(), 0);

        // Negative child count makes the header offset invalid.
        let block = [0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F];
        let root = PackedSection::parse("root", &packed_file(&["x"], &block));
        assert_eq!(root.own_type(), SectionType::STRING);
        assert_eq!(root.as_string(), "");
    }

    #[test]
    fn unknown_string_key_yields_an_empty_name() {
        // keyPos 5 in a one-entry table: the C code would dereference
        // NULL; the port degrades the name to empty (documented).
        let block = [
            0x01, 0x00, //
            0x00, 0x00, 0x00, 0x10, //
            0x05, 0x00, // key 5 — out of range
            0x02, 0x00, 0x00, 0x10, //
            b'h', b'i',
        ];
        let root = PackedSection::parse("root", &packed_file(&["n"], &block));
        assert_eq!(root.count_children(), 1);
        assert_eq!(root.open_child(0).unwrap().section_name(), "");
        assert_eq!(root.open_child(0).unwrap().as_string(), "hi");
    }

    #[test]
    fn uint64_always_reads_through_the_string_form() {
        // i32 data 300: asUInt64 goes via the printed value.
        let block = [
            0x00, 0x00, //
            0x04, 0x00, 0x00, 0x20, //
            0x2C, 0x01, 0x00, 0x00,
        ];
        let root = PackedSection::parse("u", &packed_file(&[], &block));
        assert_eq!(root.as_uint64(7), 300);
    }

    // ------------------------------------------------------------------
    // Writer: byte-exact fixtures and roundtrips.
    // ------------------------------------------------------------------

    #[test]
    fn writer_emits_the_reference_layout() {
        let mut root = DataSection::new("root");
        root.add_child("name").set_string("hello");

        let bytes = pack(&root).expect("packs");
        let mut expected = Vec::new();
        expected.extend_from_slice(&PACKED_SECTION_MAGIC.to_le_bytes());
        expected.push(0);
        expected.extend_from_slice(b"name\0\0");
        expected.extend_from_slice(&[
            0x01, 0x00, // numChildren = 1
            0x00, 0x00, 0x00, 0x10, //
            0x00, 0x00, //
            0x05, 0x00, 0x00, 0x10,
        ]);
        expected.extend_from_slice(b"hello");
        assert_eq!(bytes, expected, "byte-exact against the hand-built fixture");
    }

    #[test]
    fn string_table_is_sorted_with_double_nul_terminator() {
        let mut root = DataSection::new("root");
        root.add_child("zeta").set_string("1");
        root.add_child("alpha").set_string("2");
        root.add_child("mike").set_string("3");

        let bytes = pack(&root).unwrap();
        let table = &bytes[5..];
        assert!(
            table.starts_with(b"alpha\0mike\0zeta\0\0"),
            "sorted order, double NUL terminator: {:?}",
            String::from_utf8_lossy(table)
        );
    }

    #[test]
    fn best_guess_covers_every_leaf_type() {
        let mut root = DataSection::new("root");
        let add = |root: &mut DataSection, tag: &str, value: &str| {
            root.add_child(tag).set_string(value);
        };
        add(&mut root, "zero", "0");
        add(&mut root, "i8", "-42");
        add(&mut root, "i16", "300");
        add(&mut root, "i32", "70000");
        add(&mut root, "i64", "5000000000");
        add(&mut root, "flag_t", "true");
        add(&mut root, "flag_f", "false");
        // "1.5" does not survive the 6-digit float reformat → string.
        add(&mut root, "shortfloat", "1.5");
        add(&mut root, "canonfloat", "1.500000");
        add(&mut root, "vec3", "1.000000 2.000000 3.000000");
        add(&mut root, "bigu", "18446744073709551615");
        add(&mut root, "text", "not a number");
        add(&mut root, "blob64", "Zm9vYmFy");
        add(&mut root, "empty", "");

        let bytes = pack(&root).expect("packs");
        let parsed = PackedSection::parse("root", &bytes);

        let own = |tag: &str| parsed.find_child(tag).expect(tag);
        assert_eq!(own("zero").own_type(), SectionType::INT);
        assert_eq!(own("zero").as_int(7), 0);
        assert_eq!(own("i8").own_type(), SectionType::INT);
        assert_eq!(own("i8").as_int(7), -42);
        assert_eq!(own("i16").as_int(7), 300);
        assert_eq!(own("i32").as_int(7), 70000);
        assert_eq!(own("i64").as_int64(7), 5000000000);
        assert!(own("flag_t").as_bool(false));
        assert!(!own("flag_f").as_bool(true));
        assert_eq!(own("shortfloat").own_type(), SectionType::STRING);
        assert_eq!(own("shortfloat").as_string(), "1.5");
        assert_eq!(own("shortfloat").as_float(0.0), 1.5);
        assert_eq!(own("canonfloat").own_type(), SectionType::FLOAT);
        assert_eq!(own("canonfloat").as_float(0.0), 1.5);
        assert_eq!(own("canonfloat").as_string(), "1.500000");
        assert_eq!(
            own("vec3").as_vector3(Vector3::ZERO),
            Vector3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(
            own("bigu").own_type(),
            SectionType::STRING,
            "big uint64 stays string"
        );
        assert_eq!(own("bigu").as_uint64(7), u64::MAX);
        assert_eq!(own("text").as_string(), "not a number");
        assert_eq!(own("blob64").own_type(), SectionType::BLOB);
        assert_eq!(own("blob64").as_binary(), b"foobar");
        assert_eq!(own("empty").own_type(), SectionType::STRING);
        assert_eq!(own("empty").as_string(), "");
    }

    #[test]
    fn nested_trees_roundtrip_with_child_order() {
        let mut root = DataSection::new("root");
        root.add_child("keep").set_string("first");
        {
            let sub = root.add_child("sub");
            sub.set_string("own value");
            sub.add_child("leaf1").set_string("alpha");
            sub.add_child("leaf2").set_string("beta");
        }
        root.add_child("keep").set_string("second");

        let bytes = pack(&root).expect("packs");
        let parsed = PackedSection::parse("root", &bytes);

        // Duplicate names keep document order and findChild takes first.
        assert_eq!(parsed.find_child("keep").unwrap().as_string(), "first");
        assert_eq!(parsed.open_child(2).unwrap().as_string(), "second");
        assert_eq!(parsed.open_child(0).unwrap().section_name(), "keep");
        assert_eq!(parsed.open_child(1).unwrap().section_name(), "sub");

        let sub = parsed.find_child("sub").unwrap();
        assert_eq!(
            sub.as_string(),
            "own value",
            "a parent may own typed data too"
        );
        assert_eq!(sub.own_type(), SectionType::STRING);
        assert_eq!(sub.count_children(), 2);
        assert_eq!(sub.find_child("leaf1").unwrap().as_string(), "alpha");
        assert_eq!(sub.find_child("leaf2").unwrap().as_string(), "beta");
    }

    #[test]
    fn matrix_children_pack_as_raw_floats() {
        let mut root = DataSection::new("root");
        let m = root.add_child("m");
        for (i, row) in [
            [1.0f32, 0.0, 2.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [7.0, 8.0, 9.0],
        ]
        .iter()
        .enumerate()
        {
            let child = m.add_child(format!("row{i}").as_str());
            child.set_string(&format!(
                "{:.6} {:.6} {:.6}",
                row[0] as f64, row[1] as f64, row[2] as f64
            ));
        }

        let bytes = pack(&root).expect("packs");
        let parsed = PackedSection::parse("root", &bytes);
        let pm = parsed.find_child("m").expect("m");
        assert_eq!(pm.own_type(), SectionType::FLOAT);
        assert!(pm.is_matrix());
        assert_eq!(pm.count_children(), 4);
        assert_eq!(
            pm.find_child("row0").unwrap().as_vector3(Vector3::ZERO),
            Vector3::new(1.0, 0.0, 2.0)
        );
        assert_eq!(
            pm.find_child("row3").unwrap().as_vector3(Vector3::ZERO),
            Vector3::new(7.0, 8.0, 9.0)
        );
        assert_eq!(pm.as_matrix34(Matrix::zero()).m[3][2], 9.0);
    }

    #[test]
    fn too_many_children_is_the_single_fatal_pack_error() {
        let mut root = DataSection::new("root");
        for i in 0..(i16::MAX as usize + 1) {
            root.add_child(format!("c{i}").as_str());
        }
        assert_eq!(
            pack(&root),
            Err(PackError::TooManyChildren {
                tag: "root".to_string()
            })
        );
    }

    #[test]
    fn encrypted_write_api_produces_readable_blobs() {
        // The reader path of TYPE_ENCRYPTED_BLOB, exercised through the
        // same transform the writer would use.
        let raw = b"payload";
        let mut encrypted = vec![BW_ENCRYPTED_BLOCK];
        encrypted.extend(raw.iter().map(|b| b ^ BW_ENCRYPTION_KEY));
        let mut block = vec![0x00, 0x00];
        block.extend_from_slice(
            &((encrypted.len() as u32) | SectionType::ENCRYPTED_BLOB.0).to_le_bytes(),
        );
        block.extend_from_slice(&encrypted);
        let root = PackedSection::parse("e", &packed_file(&[], &block));
        assert_eq!(root.as_blob(""), b"payload");

        // An already-decrypted block marker passes through.
        let mut block = vec![0x00, 0x00];
        let plain = [BW_DECRYPTED_BLOCK, b'x'];
        block.extend_from_slice(
            &((plain.len() as u32) | SectionType::ENCRYPTED_BLOB.0).to_le_bytes(),
        );
        block.extend_from_slice(&plain);
        let root = PackedSection::parse("e", &packed_file(&[], &block));
        assert_eq!(root.as_blob(""), b"x");

        // An invalid marker degrades the section.
        let mut block = vec![0x00, 0x00];
        block.extend_from_slice(&(2u32 | SectionType::ENCRYPTED_BLOB.0).to_le_bytes());
        block.extend_from_slice(&[0x7F, b'x']);
        let root = PackedSection::parse("e", &packed_file(&[], &block));
        assert_eq!(root.own_type(), SectionType::STRING);
        assert_eq!(root.as_string(), "");
    }

    // ------------------------------------------------------------------
    // Accessor fallbacks: mismatched types route through the string form,
    // exactly like the reference's getXMLSection tail.
    // ------------------------------------------------------------------

    /// One-leaf fixture: a root whose own data is `payload` with type `ty`.
    fn leaf_root(ty: SectionType, payload: &[u8]) -> PackedSection {
        let mut block = vec![0x00, 0x00];
        block.extend_from_slice(&((payload.len() as u32) | ty.0).to_le_bytes());
        block.extend_from_slice(payload);
        PackedSection::parse("leaf", &packed_file(&[], &block))
    }

    #[test]
    fn mismatched_types_fall_back_through_the_string_form() {
        // STRING "300x": integer readers strtol-prefix it, float and
        // vector readers stream-extract the prefix.
        let s = leaf_root(SectionType::STRING, b"300x");
        assert_eq!(s.as_int(7), 300);
        assert_eq!(s.as_uint(7), 300);
        assert_eq!(s.as_int64(7), 300);
        assert_eq!(s.as_float(7.0), 300.0);
        assert_eq!(
            s.as_vector2(Vector2::new(-1.0, -2.0)),
            Vector2::new(-1.0, -2.0),
            "one number cannot feed two components"
        );

        let v2 = leaf_root(SectionType::STRING, b"1.000000 2.000000");
        assert_eq!(
            v2.as_vector2(Vector2::ZERO),
            Vector2::new(1.0, 2.0),
            "string form still parses as a vector"
        );
        let v3 = leaf_root(SectionType::STRING, b"1.000000 2.000000 3.000000");
        assert_eq!(v3.as_vector3(Vector3::ZERO), Vector3::new(1.0, 2.0, 3.0));
        let v4 = leaf_root(SectionType::STRING, b"1.000000 2.000000 3.000000 4.000000");
        assert_eq!(
            v4.as_vector4(Vector4::ZERO),
            Vector4::new(1.0, 2.0, 3.0, 4.0)
        );

        // A string of "true" reads as bool through the fallback.
        assert!(leaf_root(SectionType::STRING, b"true").as_bool(false));
        // Empty strings take the default directly.
        assert_eq!(leaf_root(SectionType::STRING, b"").as_float(9.0), 9.0);
        // And asBlob of a plain string is its lenient base64 decode.
        assert_eq!(
            leaf_root(SectionType::STRING, b"true").as_blob("zz"),
            base64::decode_lenient("true")
        );

        // BOOL "true" reads back through the string form everywhere.
        let t = leaf_root(SectionType::BOOL, &[1]);
        assert!(t.as_bool(false));
        assert_eq!(
            t.as_int(7),
            7,
            "the string form 'true' has no digit prefix, so the fallback default wins"
        );
        assert_eq!(t.as_float(9.0), 9.0);

        // DATA_SECTION-typed own data prints as nothing.
        let mut block = vec![
            0x01, 0x00, //
            0x00, 0x00, 0x00, 0x00, // own: 0 | TYPE_DATA_SECTION
            0x00, 0x00, // key 0
            0x02, 0x00, 0x00, 0x10, // child end 2 | STRING
        ];
        block.extend_from_slice(b"hi");
        let root = PackedSection::parse("root", &packed_file(&["c"], &block));
        assert_eq!(root.as_string(), "", "a parent prints empty");
        assert_eq!(root.own_type(), SectionType::DATA_SECTION);
        assert_eq!(root.find_child("c").unwrap().as_string(), "hi");
    }

    #[test]
    fn float_sections_with_odd_lengths_degrade() {
        // Zero floats read as zero.
        let zero = leaf_root(SectionType::FLOAT, &[]);
        assert_eq!(zero.as_float(9.0), 0.0);

        // A length that is not a multiple of four prints empty and reads
        // through the fallback.
        let odd = leaf_root(SectionType::FLOAT, &[0, 0, 0, 0, 0, 0]);
        assert_eq!(odd.as_string(), "");
        let five = leaf_root(
            SectionType::FLOAT,
            &[0; 20], // five floats: outside the 1-4 formatting cases
        );
        assert_eq!(five.as_string(), "");

        // One, three and four floats print through the XML writers.
        let mut f1 = [0u8; 4];
        f1.copy_from_slice(&1.5f32.to_le_bytes());
        assert_eq!(leaf_root(SectionType::FLOAT, &f1).as_string(), "1.500000");
        let mut f3 = Vec::new();
        for v in [1.0f32, 2.0, 3.0] {
            f3.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(
            leaf_root(SectionType::FLOAT, &f3).as_string(),
            "1.000000 2.000000 3.000000"
        );
        let mut f4 = Vec::new();
        for v in [1.0f32, 2.0, 3.0, 4.0] {
            f4.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(
            leaf_root(SectionType::FLOAT, &f4).as_string(),
            "1.000000 2.000000 3.000000 4.000000"
        );
        assert_eq!(
            leaf_root(SectionType::FLOAT, &f4).as_vector4(Vector4::ZERO),
            Vector4::new(1.0, 2.0, 3.0, 4.0)
        );

        // INT data converts to float through asInt.
        let int_float = leaf_root(SectionType::INT, &0x012Cu32.to_le_bytes());
        assert_eq!(int_float.as_float(9.0), 300.0);
    }

    #[test]
    fn blob_reads_from_int_and_bool_string_forms() {
        // "2544" is four base64 digits: the strict decode succeeds.
        let int_blob = leaf_root(SectionType::INT, &2544i16.to_le_bytes());
        assert_eq!(int_blob.as_blob(""), base64::decode_strict("2544").unwrap());
        assert_eq!(int_blob.as_binary(), base64::decode_strict("2544").unwrap());

        // A decimal string of the wrong length misses strict decoding and
        // goes through the XML fallback (lenient decode, default ignored).
        let int_short = leaf_root(SectionType::INT, &256i16.to_le_bytes());
        assert_eq!(int_short.as_blob("zz"), base64::decode_lenient("256"));

        // "true" is all alphabet characters: strict decode applies.
        let bool_blob = leaf_root(SectionType::BOOL, &[1]);
        assert_eq!(
            bool_blob.as_blob(""),
            base64::decode_strict("true").unwrap()
        );
        assert_eq!(
            bool_blob.as_binary(),
            base64::decode_strict("true").unwrap()
        );
    }

    #[test]
    fn matrix_rows_read_from_string_children_when_floats_are_absent() {
        // A node with row0..3 children that survive the writer as strings
        // (the trailing junk defeats the strict vector3 check, so the
        // node is NOT packed as raw matrix floats). Reading the matrix
        // back falls back to per-row vector parsing, junk tolerated.
        let mut root = DataSection::new("root");
        let m = root.add_child("m");
        for (i, row) in [
            [1.0f64, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [7.0, 8.0, 9.0],
        ]
        .iter()
        .enumerate()
        {
            let junk = if i == 1 { "junk" } else { "" };
            m.add_child(format!("row{i}").as_str()).set_string(&format!(
                "{:.6} {:.6} {:.6}{}",
                row[0], row[1], row[2], junk
            ));
        }

        // A second node whose row2 does not parse at all exercises the
        // early exit of the writer's vector3 check.
        let m2 = root.add_child("m2");
        for (i, row) in [
            [1.0f64, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [7.0, 8.0, 9.0],
        ]
        .iter()
        .enumerate()
        {
            let value = if i == 2 {
                "garbage".to_string()
            } else {
                format!("{:.6} {:.6} {:.6}", row[0], row[1], row[2])
            };
            m2.add_child(format!("row{i}").as_str()).set_string(&value);
        }

        let bytes = pack(&root).expect("packs");
        let parsed = PackedSection::parse("root", &bytes);
        let pm = parsed.find_child("m").expect("m");
        assert_ne!(
            pm.own_type(),
            SectionType::FLOAT,
            "junk defeats the matrix form"
        );
        assert_eq!(
            pm.find_child("row1").unwrap().as_string(),
            "0.000000 1.000000 0.000000junk"
        );
        let pm2 = parsed.find_child("m2").expect("m2");
        assert_ne!(pm2.own_type(), SectionType::FLOAT);
        assert_eq!(pm2.find_child("row2").unwrap().as_string(), "garbage");

        let m34 = pm.as_matrix34(Matrix::zero());
        assert_eq!((m34.m[0][0], m34.m[1][1], m34.m[2][2]), (1.0, 1.0, 1.0));
        assert_eq!((m34.m[3][0], m34.m[3][1], m34.m[3][2]), (7.0, 8.0, 9.0));
        assert_eq!(m34.m[3][3], 1.0);
        assert_eq!(m34.m[0][3], 0.0);
    }

    #[test]
    fn malformed_children_windows_degrade_the_parent() {
        // Child record whose start lies past its end.
        let block = [
            0x01, 0x00, //
            0x0A, 0x00, 0x00, 0x10, // own len 10 | STRING
            0x00, 0x00, // key 0
            0x05, 0x00, 0x00, 0x10, // child end 5 | STRING (start was 10)
        ]
        .iter()
        .copied()
        .chain(std::iter::repeat_n(0x41, 10))
        .collect::<Vec<u8>>();
        let root = PackedSection::parse("root", &packed_file(&["c"], &block));
        assert_eq!(root.own_type(), SectionType::STRING);
        assert_eq!(root.as_string(), "");
        assert_eq!(root.count_children(), 0);

        // A leaf child carrying a valid encrypted marker decrypts.
        let block = [
            0x01,
            0x00, //
            0x00,
            0x00,
            0x00,
            0x00, // own: 0 | DATA_SECTION
            0x00,
            0x00, // key 0
            0x03,
            0x00,
            0x00,
            0x60, // child end 3 | ENCRYPTED_BLOB
            BW_ENCRYPTED_BLOCK,
            b'h' ^ BW_ENCRYPTION_KEY,
            b'i' ^ BW_ENCRYPTION_KEY,
        ];
        let root = PackedSection::parse("root", &packed_file(&["c"], &block));
        let child = root.find_child("c").unwrap();
        assert_eq!(child.own_type(), SectionType::BLOB);
        assert_eq!(child.as_blob(""), b"hi");

        // A leaf child carrying an invalid encryption marker degrades.
        let block = [
            0x01, 0x00, //
            0x00, 0x00, 0x00, 0x00, // own: 0 | DATA_SECTION
            0x00, 0x00, // key 0
            0x02, 0x00, 0x00, 0x60, // child end 2 | ENCRYPTED_BLOB
            0x7F, b'x', // invalid first marker byte
        ];
        let root = PackedSection::parse("root", &packed_file(&["c"], &block));
        let child = root.find_child("c").unwrap();
        assert_eq!(child.own_type(), SectionType::STRING);
        assert_eq!(child.as_string(), "");
    }

    #[test]
    fn string_table_ending_without_the_empty_entry_consumes_everything() {
        // Strings present, but no terminating empty string: the whole
        // tail is consumed, leaving the root without a block.
        let mut file = Vec::new();
        file.extend_from_slice(&PACKED_SECTION_MAGIC.to_le_bytes());
        file.push(0);
        file.extend_from_slice(b"name\0");
        let root = PackedSection::parse("root", &file);
        assert_eq!(root.own_type(), SectionType::STRING);
        assert_eq!(root.as_string(), "");
    }

    #[test]
    fn get_integer_val_rejects_unusual_lengths() {
        // Lengths other than 0/1/2/4/8 fail (the reference then recurses
        // through its string fallback on such corrupt input — a path the
        // writer never produces).
        assert_eq!(get_integer_val(&[]), Some(0));
        assert_eq!(get_integer_val(&[0xFB]), Some(-5));
        assert_eq!(get_integer_val(&[0x2C, 0x01]), Some(300));
        assert_eq!(get_integer_val(&[1, 2, 3]), None);
        assert_eq!(get_integer_val(&[1, 2, 3, 4, 5]), None);
    }

    #[test]
    fn best_guess_float_vectors_and_degenerate_values() {
        let mut root = DataSection::new("root");
        let add = |root: &mut DataSection, tag: &str, value: &str| {
            root.add_child(tag).set_string(value);
        };
        add(&mut root, "vec2", "1.000000 2.000000");
        add(&mut root, "vec4", "1.000000 2.000000 3.000000 4.000000");
        add(&mut root, "spaced_int", " -42");
        add(&mut root, "huge_int", "99999999999999999999");
        add(&mut root, "dash", "-");
        add(
            &mut root,
            "five_floats",
            "1.000000 2.000000 3.000000 4.000000 5.000000",
        );
        add(&mut root, "double_b64", "Zg==Zg==");
        add(&mut root, "signed_wrap", "-5k");

        let bytes = pack(&root).expect("packs");
        let parsed = PackedSection::parse("root", &bytes);
        let own = |tag: &str| parsed.find_child(tag).expect(tag);

        assert_eq!(own("vec2").own_type(), SectionType::FLOAT);
        assert_eq!(
            own("vec2").as_vector2(Vector2::ZERO),
            Vector2::new(1.0, 2.0)
        );
        assert_eq!(own("vec4").own_type(), SectionType::FLOAT);
        assert_eq!(
            own("vec4").as_vector4(Vector4::ZERO),
            Vector4::new(1.0, 2.0, 3.0, 4.0)
        );

        // Leading space defeats the int round-trip; sscanf still sees the
        // value but the reformat check fails, so the text stays string.
        assert_eq!(own("spaced_int").own_type(), SectionType::STRING);
        assert_eq!(own("spaced_int").as_string(), " -42");

        // Twenty digits overflow i64 (stream failbit) and u64 (sscanf
        // saturation). The value is digits-only with length %4 == 0, so
        // the canonical base64 round-trip succeeds and the reference's
        // BLOB branch claims it — verified against addBestGuess order.
        assert_eq!(own("huge_int").own_type(), SectionType::BLOB);
        assert_eq!(
            own("huge_int").as_string(),
            "99999999999999999999",
            "a canonical base64 blob prints back its own text"
        );
        assert_eq!(own("huge_int").as_uint64(7), u64::MAX);

        assert_eq!(own("dash").as_string(), "-");
        assert_eq!(own("five_floats").own_type(), SectionType::STRING);

        // The base64 round-trip fails (decoding "Zg==Zg==" re-encodes as
        // "Zg=="), so the raw text survives.
        assert_eq!(own("double_b64").own_type(), SectionType::STRING);
        assert_eq!(own("double_b64").as_string(), "Zg==Zg==");

        // sscanf wraps the sign into a huge value that fails the
        // reformat check: string.
        assert_eq!(own("signed_wrap").own_type(), SectionType::STRING);
        assert_eq!(own("signed_wrap").as_string(), "-5k");
    }
}
