//! Byte and character-set handling for the MySQL backend.
//!
//! The reference sizes its string columns by **characters**, stores them as
//! UTF-8 and reads back rows in the collation the server reports
//! (`MySql::setCharacterSet` pins the connection to utf8, `wrapper.cpp:131-132`;
//! the widths come from `SHOW CHARACTER SET`/`SHOW COLLATION` joined into a
//! collation-id map, `MySql::queryCharsetLength`, `wrapper.cpp:185-251`).
//! Binary columns (`BLOB`) carry raw bytes and never pass through any of this.

use std::collections::HashMap;

/// `UnicodeStringMapping::getUTF8CharOffsets`
/// (`unicode_string_mapping.cpp:58-87`): the start byte offset of every UTF-8
/// character, with each character's width taken from its leading byte.
///
/// A leading byte none of the four masks match makes the reference print
/// `Invalid UTF-8 character start byte` and leave the width untouched, which
/// stalls the scan; the port advances a single byte there instead so a stray
/// byte (a binary blob read as text, a torn write) cannot hang the mapping.
pub fn utf8_char_offsets(bytes: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i];
        let width = if ch & 0x80 == 0x00 {
            1
        } else if ch & 0xE0 == 0xC0 {
            2
        } else if ch & 0xF0 == 0xE0 {
            3
        } else if ch & 0xF8 == 0xF0 {
            4
        } else {
            // The reference's invalid-start-byte case.
            1
        };
        offsets.push(i);
        i += width;
    }
    offsets
}

/// The character length of a UTF-8 string: the size of its offsets vector,
/// which is how the string mappings measure a default value.
pub fn utf8_char_length(bytes: &[u8]) -> usize {
    utf8_char_offsets(bytes).len()
}

/// `StringLikeMapping`'s column budget: "MySQL requires 3 bytes for each
/// character" (`unicode_string_mapping.cpp:21-22`), the byte length the utf8
/// collation charges per character.
pub fn utf8_byte_length_for(char_length: usize) -> usize {
    char_length * 3
}

/// Truncate a UTF-8 value to at most `char_length` characters, cutting at the
/// first byte of the character that crosses the limit
/// (`defaultValue_.resize( offsets[charLength_] )`), and report whether a cut
/// happened (`offsets.size() > charLength_` driving the reference's warning).
pub fn utf8_truncate_to_chars(bytes: &[u8], char_length: usize) -> (&[u8], bool) {
    let offsets = utf8_char_offsets(bytes);
    if offsets.len() > char_length {
        (&bytes[..offsets[char_length]], true)
    } else {
        (bytes, false)
    }
}

/// `MySql::queryCharsetLength`'s map: the character width of each collation id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharsetWidths {
    collations: HashMap<u32, usize>,
}

impl CharsetWidths {
    /// Build the map the way the reference fills `s_collationLengths_`:
    /// `charsets` pairs a character set name with its `maxlen`, `collations`
    /// pairs a collation id with the character set it belongs to.
    ///
    /// A collation whose charset is missing from the character-set table still
    /// lands in the map with width 0 — `charsetLengths[charsetName]` inserts a
    /// zero — so the port mirrors that rather than dropping the row.
    pub fn from_rows(charsets: &[(String, usize)], collations: &[(u32, String)]) -> Self {
        let widths: HashMap<String, usize> = charsets.iter().cloned().collect();
        let collations = collations
            .iter()
            .map(|(id, name)| (*id, widths.get(name).copied().unwrap_or(0)))
            .collect();
        Self { collations }
    }

    /// `MySql::charsetWidth` (`wrapper.cpp:260-273`): the character width of the
    /// collation, with an unknown id (or collation id 0) falling back to the
    /// one-byte-per-character default.
    pub fn width(&self, collation_id: u32) -> usize {
        self.collations.get(&collation_id).copied().unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_offsets_follow_the_leading_byte_widths() {
        // ASCII, two-byte, three-byte and four-byte characters side by side.
        let mixed = "a\u{e9}\u{4e2d}\u{1f600}b";
        let bytes = mixed.as_bytes();
        let offsets = utf8_char_offsets(bytes);
        // One offset per character, at each character's first byte.
        assert_eq!(offsets, [0, 1, 3, 6, 10]);
        assert_eq!(utf8_char_length(bytes), 5);
        // Cutting at every offset boundary yields whole characters, so the
        // value round-trips without losing or splitting a character.
        for end in offsets.iter().rev().chain(std::iter::once(&bytes.len())) {
            assert_eq!(std::str::from_utf8(&bytes[..*end]).unwrap(), &mixed[..*end]);
        }
    }

    #[test]
    fn an_empty_string_has_no_offsets() {
        assert!(utf8_char_offsets(b"").is_empty());
        assert_eq!(utf8_char_length(b""), 0);
    }

    #[test]
    fn an_invalid_start_byte_advances_one_byte_instead_of_stalling() {
        // A blob's raw bytes read as text: 0xff matches none of the masks.
        let blob = [0x61u8, 0xff, 0x00, 0x80, 0x62];
        assert_eq!(utf8_char_offsets(&blob), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_string_column_is_budgeted_three_bytes_per_character() {
        assert_eq!(utf8_byte_length_for(10), 30);
        assert_eq!(utf8_byte_length_for(0), 0);
    }

    #[test]
    fn truncation_cuts_at_the_first_byte_of_the_overflowing_character() {
        let value = "\u{4e2d}\u{6587}\u{6d4b}\u{8bd5}"; // four CJK characters
        let bytes = value.as_bytes();
        // Within the limit: untouched.
        assert_eq!(utf8_truncate_to_chars(bytes, 4), (bytes, false));
        assert_eq!(utf8_truncate_to_chars(bytes, 8), (bytes, false));
        // Over the limit: cut at the third character's first byte (offset 6).
        let (kept, truncated) = utf8_truncate_to_chars(bytes, 2);
        assert!(truncated);
        assert_eq!(std::str::from_utf8(kept).unwrap(), "\u{4e2d}\u{6587}");
        // An empty budget keeps nothing.
        let (kept, truncated) = utf8_truncate_to_chars(bytes, 0);
        assert!(truncated);
        assert!(kept.is_empty());
    }

    #[test]
    fn collation_widths_come_from_the_character_set_table() {
        // The SHOW CHARACTER SET rows: (name, maxlen).
        let charsets = [
            ("latin1".to_owned(), 1usize),
            ("utf8".to_owned(), 3),
            ("utf8mb4".to_owned(), 4),
        ];
        // The SHOW COLLATION rows: (collation id, charset name).
        let collations = [
            (8u32, "latin1".to_owned()),
            (33, "utf8".to_owned()),
            (45, "utf8".to_owned()),
            (46, "utf8mb4".to_owned()),
        ];
        let widths = CharsetWidths::from_rows(&charsets, &collations);
        assert_eq!(widths.width(8), 1);
        assert_eq!(widths.width(33), 3);
        assert_eq!(widths.width(45), 3);
        assert_eq!(widths.width(46), 4);
        // An unknown collation id falls back to one byte per character.
        assert_eq!(widths.width(0), 1);
        assert_eq!(widths.width(999), 1);
    }

    #[test]
    fn a_collation_whose_charset_is_unknown_lands_at_zero_like_the_reference() {
        // MariaDB's UCA 14.0 collations read as NULL through a MySQL client
        // library and are skipped by the reference; the ones that survive with
        // an unlisted charset name keep the zero the map insert produces.
        let widths = CharsetWidths::from_rows(
            &[("utf8".to_owned(), 3usize)],
            &[(248u32, "uca1400_ai_ci".to_owned())],
        );
        assert_eq!(widths.width(248), 0);
    }
}
