//! The persisted parse of a format string — `format_data.hpp`,
//! `string_offset.hpp` and the `LogStringInterpolator::write`/`read` pair
//! that lands it in the MLDB `strings` file. Each record is
//! `BW::string formatString` + `BW::string components` +
//! `BW::vector<StringOffset>` + `BW::vector<FormatData>`, written once per
//! distinct format string so a restarted logger can still render old
//! entries.
//!
//! `components` is the parse walk in order: one `'s'` per non-empty literal
//! segment and one `'t'` per token (`onString`/`onToken`); `stringOffsets`
//! carry the `[start, end)` bounds of each literal in the same order.

use crate::format_string::{FormatStringHandler, handle_format_string};
use crate::stream::{MemoryIStream, MemoryOStream};

/// `FormatData` (`format_data.hpp`): one `#pragma pack(1)` struct, 15 bytes
/// on disk.
///
/// The `cflags`/`vflags` bitfields pack into a single byte — on the
/// little-endian GCC layout the first-declared `cflags_` takes the low
/// nibble and `vflags_` the high one. Note `cflags` is the length-modifier
/// class (`DP_C_*`, e.g. `%hd` stores 1), not the `DP_F_*` conversion
/// flags, which travel whole in `flags`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatData {
    /// The conversion character (`'d'`, `'s'`, …; `%X` stores `'x'`).
    pub type_char: u8,
    /// Length-modifier class, stored in the low nibble.
    pub cflags: u8,
    /// `VARIABLE_MIN_WIDTH`/`VARIABLE_MAX_WIDTH` (the `*` widths), stored
    /// in the high nibble.
    pub vflags: u8,
    /// Numeric base: 10 for `d`/`u`, 8 for `o`, 16 for `x`/`p`, 0 elsewhere.
    pub base: u8,
    /// Field width (0 when unset).
    pub min: i32,
    /// Precision (-1 when unset).
    pub max: i32,
    /// The full `DP_F_*` conversion-flag bitmask.
    pub flags: i32,
}

impl FormatData {
    /// The `#pragma pack(1)` record size.
    pub const PACKED_SIZE: usize = 15;

    /// `FormatData( type, cflags, base, min, max, flags, vflags )` — the
    /// argument order `LogStringInterpolator::onToken` receives from
    /// `handleFormatString`. The 4-bit bitfields truncate like the C
    /// bitfield assignment does (e.g. `DP_F_ZERO` never fits `cflags`,
    /// which is why it duplicates into `flags`).
    #[allow(clippy::too_many_arguments)]
    pub fn from_token(
        type_char: char,
        cflags: i32,
        min: i32,
        max: i32,
        flags: i32,
        base: u8,
        vflags: i32,
    ) -> Self {
        Self {
            type_char: type_char as u8,
            cflags: (cflags & 0xF) as u8,
            vflags: (vflags & 0xF) as u8,
            base,
            min,
            max,
            flags,
        }
    }

    /// The packed 15-byte little-endian record.
    pub fn to_disk_bytes(&self) -> [u8; Self::PACKED_SIZE] {
        let mut out = [0u8; Self::PACKED_SIZE];
        out[0] = self.type_char;
        out[1] = (self.cflags & 0xF) | ((self.vflags & 0xF) << 4);
        out[2] = self.base;
        out[3..7].copy_from_slice(&self.min.to_le_bytes());
        out[7..11].copy_from_slice(&self.max.to_le_bytes());
        out[11..15].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    /// Decode one packed record; `None` unless exactly 15 bytes are given.
    pub fn from_disk_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::PACKED_SIZE {
            return None;
        }
        let rd_i32 = |r: std::ops::Range<usize>| i32::from_le_bytes(bytes[r].try_into().unwrap());
        Some(Self {
            type_char: bytes[0],
            cflags: bytes[1] & 0xF,
            vflags: bytes[1] >> 4,
            base: bytes[2],
            min: rd_i32(3..7),
            max: rd_i32(7..11),
            flags: rd_i32(11..15),
        })
    }
}

/// `StringOffset` (`string_offset.hpp`): the `[start, end)` bounds of one
/// literal segment, `#pragma pack(1)` — 4 bytes on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StringOffset {
    pub start: u16,
    pub end: u16,
}

impl StringOffset {
    /// The packed record size.
    pub const PACKED_SIZE: usize = 4;

    pub fn to_disk_bytes(&self) -> [u8; Self::PACKED_SIZE] {
        let mut out = [0u8; Self::PACKED_SIZE];
        out[0..2].copy_from_slice(&self.start.to_le_bytes());
        out[2..4].copy_from_slice(&self.end.to_le_bytes());
        out
    }

    /// Decode one packed record.
    pub fn from_disk_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::PACKED_SIZE {
            return None;
        }
        Some(Self {
            start: u16::from_le_bytes(bytes[0..2].try_into().unwrap()),
            end: u16::from_le_bytes(bytes[2..4].try_into().unwrap()),
        })
    }
}

/// Collects the callbacks `handleFormatString` makes, the way
/// `LogStringInterpolator` does when its constructor parses a format
/// string.
#[derive(Default)]
struct RecordBuilder {
    record: FormatStringRecord,
    ok: bool,
}

impl FormatStringHandler for RecordBuilder {
    fn on_string(&mut self, start: usize, end: usize) {
        self.record.string_offsets.push(StringOffset {
            start: start as u16,
            end: end as u16,
        });
        self.record.components.push('s');
    }

    fn on_token(
        &mut self,
        type_char: char,
        cflags: i32,
        min: i32,
        max: i32,
        flags: i32,
        base: u8,
        vflags: i32,
    ) {
        self.record.fmt_data.push(FormatData::from_token(
            type_char, cflags, min, max, flags, base, vflags,
        ));
        self.record.components.push('t');
    }
}

/// The persisted state of one format string — `LogStringInterpolator`'s
/// `formatString_`/`components_`/`stringOffsets_`/`fmtData_` quadruple.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FormatStringRecord {
    /// The verbatim format string.
    pub format_string: String,
    /// `'s'`/`'t'` per literal/token, in parse order.
    pub components: String,
    /// Literal bounds, parallel to the `'s'` markers.
    pub string_offsets: Vec<StringOffset>,
    /// One entry per token, parallel to the `'t'` markers.
    pub fmt_data: Vec<FormatData>,
}

impl FormatStringRecord {
    /// Parse a format string the way the interpolator constructor does.
    /// `None` when `handleFormatString` rejects it (illegal conversion).
    pub fn parse(format_string: &str) -> Option<Self> {
        let mut builder = RecordBuilder::default();
        builder.ok = handle_format_string(format_string, &mut builder);
        if !builder.ok {
            return None;
        }
        builder.record.format_string = format_string.to_string();
        Some(builder.record)
    }

    /// `LogStringInterpolator::write`: the four fields appended to a
    /// `FileStream`, i.e. `<< formatString_ << components_ <<
    /// stringOffsets_ << fmtData_`.
    pub fn write_to(&self, os: &mut MemoryOStream) {
        os.write_string(&self.format_string);
        os.write_string(&self.components);
        os.write_u32(self.string_offsets.len() as u32);
        for offset in &self.string_offsets {
            os.add_blob(&offset.to_disk_bytes());
        }
        os.write_u32(self.fmt_data.len() as u32);
        for data in &self.fmt_data {
            os.add_blob(&data.to_disk_bytes());
        }
    }

    /// The bytes `write_to` produces — the record as it sits in the
    /// strings file.
    pub fn to_disk_bytes(&self) -> Vec<u8> {
        let mut os = MemoryOStream::new();
        self.write_to(&mut os);
        os.data().to_vec()
    }

    /// `LogStringInterpolator::read`: decode one record from the front of
    /// `is`. On a truncated read the stream's error flag sticks and `None`
    /// comes back.
    pub fn read_from(is: &mut MemoryIStream) -> Option<Self> {
        let format_string = is.read_string();
        let components = is.read_string();
        let count = is.read_u32()?;
        let string_offsets = (0..count)
            .map(|_| {
                is.retrieve(StringOffset::PACKED_SIZE)
                    .and_then(StringOffset::from_disk_bytes)
            })
            .collect::<Option<Vec<_>>>()?;
        let count = is.read_u32()?;
        let fmt_data = (0..count)
            .map(|_| {
                is.retrieve(FormatData::PACKED_SIZE)
                    .and_then(FormatData::from_disk_bytes)
            })
            .collect::<Option<Vec<_>>>()?;
        if is.error() {
            return None;
        }
        Some(Self {
            format_string,
            components,
            string_offsets,
            fmt_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference-written strings file: the second original-sample run,
    /// fourteen distinct format strings driven through the real
    /// `message_logger` (the flag/width/precision batch pins the bitfield
    /// layout byte-for-byte).
    const SAMPLE_STRINGS: [u8; 549] = [
        0x09, 0x64, 0x61, 0x6d, 0x61, 0x67, 0x65, 0x20, 0x25, 0x64, 0x02, 0x73, 0x74, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x01, 0x00, 0x00, 0x00, 0x64, 0x00, 0x0a, 0x00, 0x00,
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x6d, 0x6f, 0x76, 0x65,
        0x64, 0x20, 0x74, 0x6f, 0x20, 0x25, 0x73, 0x02, 0x73, 0x74, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x09, 0x00, 0x01, 0x00, 0x00, 0x00, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff,
        0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x08, 0x70, 0x69, 0x20, 0x69, 0x73, 0x20, 0x25,
        0x66, 0x02, 0x73, 0x74, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00,
        0x00, 0x21, 0x25, 0x73, 0x20, 0x64, 0x69, 0x64, 0x20, 0x25, 0x64, 0x20, 0x64, 0x61, 0x6d,
        0x61, 0x67, 0x65, 0x20, 0x28, 0x25, 0x75, 0x20, 0x72, 0x61, 0x77, 0x2c, 0x20, 0x25, 0x78,
        0x20, 0x68, 0x65, 0x78, 0x29, 0x08, 0x74, 0x73, 0x74, 0x73, 0x74, 0x73, 0x74, 0x73, 0x04,
        0x00, 0x00, 0x00, 0x02, 0x00, 0x07, 0x00, 0x09, 0x00, 0x12, 0x00, 0x14, 0x00, 0x1a, 0x00,
        0x1c, 0x00, 0x21, 0x00, 0x04, 0x00, 0x00, 0x00, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00,
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x75, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00,
        0xff, 0xff, 0xff, 0xff, 0x40, 0x00, 0x00, 0x00, 0x78, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00,
        0xff, 0xff, 0xff, 0xff, 0x40, 0x00, 0x00, 0x00, 0x03, 0x25, 0x35, 0x64, 0x01, 0x74, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x64, 0x00, 0x0a, 0x05, 0x00, 0x00, 0x00, 0xff,
        0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x05, 0x25, 0x2d, 0x35, 0x73, 0x7c, 0x02, 0x74,
        0x73, 0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x73, 0x00,
        0x00, 0x05, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x01, 0x00, 0x00, 0x00, 0x05, 0x25,
        0x2b, 0x2e, 0x32, 0x66, 0x01, 0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x66,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x06,
        0x25, 0x30, 0x38, 0x2e, 0x33, 0x66, 0x01, 0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x66, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00,
        0x00, 0x03, 0x25, 0x23, 0x78, 0x01, 0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x78, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x48, 0x00, 0x00, 0x00,
        0x02, 0x25, 0x6f, 0x01, 0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x6f, 0x00,
        0x08, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x40, 0x00, 0x00, 0x00, 0x02, 0x25,
        0x58, 0x01, 0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x78, 0x00, 0x10, 0x00,
        0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x60, 0x00, 0x00, 0x00, 0x03, 0x25, 0x2a, 0x64,
        0x01, 0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x64, 0x10, 0x0a, 0x00, 0x00,
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x04, 0x25, 0x2e, 0x2a, 0x66,
        0x01, 0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x66, 0x20, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x03, 0x25, 0x68, 0x64, 0x01,
        0x74, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x64, 0x01, 0x0a, 0x00, 0x00, 0x00,
        0x00, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn format_data_packs_the_fifteen_bytes() {
        // "damage %d": base 10, nothing else set.
        let fd = FormatData::from_token('d', 0, 0, -1, 0, 10, 0);
        assert_eq!(
            fd.to_disk_bytes(),
            [
                b'd', 0x00, 0x0a, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0
            ]
        );
        assert_eq!(FormatData::from_disk_bytes(&fd.to_disk_bytes()), Some(fd));
        assert!(FormatData::from_disk_bytes(&fd.to_disk_bytes()[..14]).is_none());
    }

    #[test]
    fn format_data_splits_the_bitfield_nibbles() {
        // %*d: variable min width rides the high nibble.
        let star = FormatData::from_token('d', 0, 0, -1, 0, 10, 1);
        assert_eq!(star.to_disk_bytes()[1], 0x10);
        // %.*f: variable max width is bit 1.
        let dot_star = FormatData::from_token('f', 0, 0, -1, 0, 0, 2);
        assert_eq!(dot_star.to_disk_bytes()[1], 0x20);
        // %hd: DP_C_SHORT sits in the low nibble.
        let short = FormatData::from_token('d', 1, 0, -1, 0, 10, 0);
        assert_eq!(short.to_disk_bytes()[1], 0x01);
        // 4-bit truncation, exactly like the C bitfield assignment.
        let truncated = FormatData::from_token('d', 0x12, 0, -1, 0, 10, 0x25);
        assert_eq!(truncated.to_disk_bytes()[1], 0x52);
    }

    #[test]
    fn string_offset_packs_four_bytes() {
        let so = StringOffset { start: 0, end: 7 };
        assert_eq!(so.to_disk_bytes(), [0, 0, 7, 0]);
        assert_eq!(StringOffset::from_disk_bytes(&so.to_disk_bytes()), Some(so));
        assert_eq!(
            StringOffset::from_disk_bytes(&[0, 0, 0xff, 0xff]),
            Some(StringOffset {
                start: 0,
                end: 0xffff
            })
        );
        assert!(StringOffset::from_disk_bytes(&[0, 0]).is_none());
    }

    #[test]
    fn parses_literal_prefix_format() {
        let record = FormatStringRecord::parse("damage %d").unwrap();
        assert_eq!(record.format_string, "damage %d");
        // One literal then one token: components "st".
        assert_eq!(record.components, "st");
        assert_eq!(
            record.string_offsets,
            vec![StringOffset { start: 0, end: 7 }]
        );
        assert_eq!(
            record.fmt_data,
            vec![FormatData::from_token('d', 0, 0, -1, 0, 10, 0)]
        );
    }

    #[test]
    fn parses_token_first_format_with_suffix_literal() {
        let record = FormatStringRecord::parse("%-5s|").unwrap();
        // Token first, then the "|" literal: components "ts".
        assert_eq!(record.components, "ts");
        assert_eq!(
            record.string_offsets,
            vec![StringOffset { start: 4, end: 5 }]
        );
        assert_eq!(
            record.fmt_data,
            vec![FormatData::from_token('s', 0, 5, -1, 1, 0, 0)]
        );
    }

    #[test]
    fn parses_width_precision_and_unsigned_flags() {
        // "%08.3f": DP_F_ZERO cannot fit the 4-bit cflags field and only
        // shows up in flags, exactly as the reference stores it.
        let zero_pad = FormatStringRecord::parse("%08.3f").unwrap();
        assert_eq!(
            zero_pad.fmt_data,
            vec![FormatData::from_token('f', 0, 8, 3, 0x10, 0, 0)]
        );

        // "%#x": DP_F_NUM | DP_F_UNSIGNED, base 16.
        let alt_hex = FormatStringRecord::parse("%#x").unwrap();
        assert_eq!(
            alt_hex.fmt_data,
            vec![FormatData::from_token('x', 0, 0, -1, 0x48, 16, 0)]
        );

        // "%X": upper-case renders through DP_F_UP with type 'x'.
        let upper = FormatStringRecord::parse("%X").unwrap();
        assert_eq!(
            upper.fmt_data,
            vec![FormatData::from_token('x', 0, 0, -1, 0x60, 16, 0)]
        );

        // "%o": octal is unsigned, base 8.
        let octal = FormatStringRecord::parse("%o").unwrap();
        assert_eq!(
            octal.fmt_data,
            vec![FormatData::from_token('o', 0, 0, -1, 0x40, 8, 0)]
        );

        // Variable widths.
        let star = FormatStringRecord::parse("%*d").unwrap();
        assert_eq!(
            star.fmt_data,
            vec![FormatData::from_token('d', 0, 0, -1, 0, 10, 1)]
        );
        let dot_star = FormatStringRecord::parse("%.*f").unwrap();
        assert_eq!(
            dot_star.fmt_data,
            vec![FormatData::from_token('f', 0, 0, -1, 0, 0, 2)]
        );

        // Length modifier class.
        let short = FormatStringRecord::parse("%hd").unwrap();
        assert_eq!(
            short.fmt_data,
            vec![FormatData::from_token('d', 1, 0, -1, 0, 10, 0)]
        );
    }

    #[test]
    fn parse_rejects_illegal_conversions() {
        assert!(FormatStringRecord::parse("%y bad").is_none());
    }

    #[test]
    fn parse_tracks_multiple_tokens_and_literals() {
        let record = FormatStringRecord::parse("%s did %d damage (%u raw, %x hex)").unwrap();
        assert_eq!(record.components, "tstststs");
        assert_eq!(
            record.string_offsets,
            vec![
                StringOffset { start: 2, end: 7 },
                StringOffset { start: 9, end: 18 },
                StringOffset { start: 20, end: 26 },
                StringOffset { start: 28, end: 33 },
            ]
        );
        assert_eq!(
            record.fmt_data,
            vec![
                FormatData::from_token('s', 0, 0, -1, 0, 0, 0),
                FormatData::from_token('d', 0, 0, -1, 0, 10, 0),
                FormatData::from_token('u', 0, 0, -1, 0x40, 10, 0),
                FormatData::from_token('x', 0, 0, -1, 0x40, 16, 0),
            ]
        );
    }

    #[test]
    fn writes_the_reference_strings_file_byte_for_byte() {
        // The fourteen format strings of the sample, in the order the
        // reference wrote them.
        let formats = [
            "damage %d",
            "moved to %s",
            "pi is %f",
            "%s did %d damage (%u raw, %x hex)",
            "%5d",
            "%-5s|",
            "%+.2f",
            "%08.3f",
            "%#x",
            "%o",
            "%X",
            "%*d",
            "%.*f",
            "%hd",
        ];
        let mut written = Vec::new();
        for format in formats {
            let record =
                FormatStringRecord::parse(format).unwrap_or_else(|| panic!("parse {format}"));
            let mut os = MemoryOStream::new();
            record.write_to(&mut os);
            written.extend_from_slice(os.data());
        }
        assert_eq!(written.len(), 549);
        assert_eq!(written.as_slice(), SAMPLE_STRINGS.as_slice());
    }

    #[test]
    fn reads_every_reference_record_back() {
        let mut is = MemoryIStream::new(&SAMPLE_STRINGS);
        let mut formats = Vec::new();
        let mut offsets_in_file = Vec::new();
        while is.remaining() > 0 {
            offsets_in_file.push(549 - is.remaining());
            formats.push(FormatStringRecord::read_from(&mut is).unwrap());
        }
        assert_eq!(formats.len(), 14);
        assert_eq!(formats[0].format_string, "damage %d");
        assert_eq!(formats[3].components, "tstststs");
        assert_eq!(formats[13].format_string, "%hd");
        assert_eq!(formats[13].fmt_data[0].cflags, 1);
        // The record boundaries are the offsets the entries file refers to.
        assert_eq!(
            offsets_in_file,
            vec![
                0, 40, 82, 121, 248, 277, 313, 344, 376, 405, 433, 461, 490, 520
            ]
        );
    }

    #[test]
    fn read_rejects_truncated_records() {
        assert!(
            FormatStringRecord::read_from(&mut MemoryIStream::new(&SAMPLE_STRINGS[..20])).is_none()
        );
        assert!(FormatStringRecord::read_from(&mut MemoryIStream::new(&[])).is_none());
    }

    #[test]
    fn disk_roundtrip_of_every_record_is_lossless() {
        let mut is = MemoryIStream::new(&SAMPLE_STRINGS);
        let mut start = 0usize;
        while is.remaining() > 0 {
            let record = FormatStringRecord::read_from(&mut is).unwrap();
            let encoded = record.to_disk_bytes();
            assert_eq!(
                encoded.as_slice(),
                &SAMPLE_STRINGS[start..start + encoded.len()]
            );
            start += encoded.len();
        }
        assert_eq!(start, 549);
    }
}
