//! BW-10-02-01 the `BWST` string table section.
//!
//! A port of `lib/compiled_space/string_table_types.hpp` (`Entry`, `Index`,
//! `INVALID_INDEX`, format magic/version) and `lib/compiled_space/string_table.cpp`
//! (`StringTable::read` / `readFromStream` and the entries they validate).
//!
//! Payload layout: an `ExternalArray<Entry>` (u32 typeSize that must equal 8,
//! u32 count, then `count` × 8-byte `{offset, length}` entries), then a plain
//! u32 `stringDataSize`, then that many raw bytes. Offsets index into the
//! string data; `length` excludes the NUL that must terminate every entry.

use crate::binary_format::{BinaryFormat, Magic, Stream};

/// `StringTableTypes::FORMAT_MAGIC`.
pub const FORMAT_MAGIC: Magic = *b"BWST";

/// `StringTableTypes::FORMAT_VERSION`.
pub const FORMAT_VERSION: u32 = 1;

/// `StringTableTypes::Index`.
pub type Index = u32;

/// `StringTableTypes::INVALID_INDEX` — `(Index)-1`.
pub const INVALID_INDEX: Index = u32::MAX;

/// `sizeof(StringTableTypes::Entry)` — the ExternalArray element size.
pub const ENTRY_SIZE: usize = 8;

/// `StringTableTypes::Entry`: `{uint32 offset_, uint32 length_}`, with
/// `length_` excluding the terminating NUL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub offset: u32,
    pub length: u32,
}

impl Entry {
    /// Parse one 8-byte little-endian entry.
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < ENTRY_SIZE {
            return None;
        }
        Some(Self {
            offset: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            length: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }
}

/// `StringTable` (`string_table.cpp:14-167`): the entry list plus a borrowed
/// view of the string data it indexes, with the reference's validation
/// carried into `read`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringTable<'a> {
    entries: Vec<Entry>,
    string_data: &'a [u8],
}

impl<'a> StringTable<'a> {
    /// `StringTable::read` (`string_table.cpp:31-59`).
    ///
    /// The reference prints a message from `findAndOpenSection` and then its
    /// own on top; the port concatenates the two so one `Err` carries both
    /// lines. A malformed payload likewise concatenates the inner
    /// `readFromStream` message with the outer `'%s' is malformed.` line.
    pub fn read(reader: &'a BinaryFormat) -> Result<Self, String> {
        let mut stream = reader
            .find_and_open_section(FORMAT_MAGIC, FORMAT_VERSION, "StringTable")
            .map_err(|err| {
                format!(
                    "{err}Failed to map string table '{}' into memory.\n",
                    reader.resource_id()
                )
            })?;

        let (entries, string_data) = read_from_stream(&mut stream)
            .map_err(|inner| format!("{}'{}' is malformed.\n", inner, reader.resource_id()))?;

        Ok(Self {
            entries,
            string_data,
        })
    }

    /// `StringTable::isValid` (`string_table.cpp:130-133`): true once the
    /// entry array mapped (even for an empty table).
    pub fn is_valid(&self) -> bool {
        true
    }

    /// `StringTable::size` (`string_table.cpp:136-141`).
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// The parsed entry list.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// `StringTable::entryLength` (`string_table.cpp:161-167`): the entry's
    /// byte length without the terminating NUL.
    pub fn entry_length(&self, idx: usize) -> Option<u32> {
        self.entries.get(idx).map(|e| e.length)
    }

    /// `StringTable::entry` (`string_table.cpp:145-157`) as raw bytes: the
    /// NUL is not part of the returned slice. Out-of-range indices yield
    /// `None` (the reference returns NULL).
    pub fn entry_bytes(&self, idx: usize) -> Option<&'a [u8]> {
        let entry = self.entries.get(idx)?;
        let start = entry.offset as usize;
        let end = start.wrapping_add(entry.length as usize);
        self.string_data.get(start..end)
    }

    /// `StringTable::entry` as UTF-8: a convenience over [`Self::entry_bytes`]
    /// for callers holding ASCII resource ids (the reference does no encoding
    /// validation at all; invalid UTF-8 yields `None` here).
    pub fn entry_str(&self, idx: usize) -> Option<&'a str> {
        std::str::from_utf8(self.entry_bytes(idx)?).ok()
    }
}

/// `StringTable::readFromStream` (`string_table.cpp:63-110`).
fn read_from_stream<'a>(stream: &mut Stream<'a>) -> Result<(Vec<Entry>, &'a [u8]), String> {
    // read(ExternalArray<Entry>): typeSize must equal 8.
    let raw_entries = stream
        .read_external_array(ENTRY_SIZE)
        .ok_or_else(|| "Failed to read string table\n".to_string())?;

    // A plain u32 read of the string data size. The reference ignores the
    // read's return value, so a section that ends here proceeds with a
    // zero size (string_table.cpp:75-77).
    let string_data_size = stream.read_u32().unwrap_or(0);

    let string_data = stream
        .read_raw(string_data_size as usize)
        .ok_or_else(|| "String table has invalid string data.\n".to_string())?;

    let mut entries = Vec::with_capacity(raw_entries.len() / ENTRY_SIZE);
    for chunk in raw_entries.as_chunks::<ENTRY_SIZE>().0 {
        entries.push(Entry::from_le_bytes(chunk).expect("as_chunks yields 8 bytes"));
    }

    // The reference's validation pass (string_table.cpp:84-107): each entry
    // must fit the string data, end at a NUL, and the accumulated
    // (length+1) total must equal the string data size. The additions wrap
    // at u32 exactly like the reference's uint32 arithmetic.
    let mut accumulated: u64 = 0;
    for entry in &entries {
        if string_data_size < entry.offset.wrapping_add(entry.length) {
            return Err("String table has incomplete data.\n".to_string());
        }
        accumulated += u64::from(entry.length) + 1;
        // The NUL check reads one byte past the entry; a missing NUL fails
        // silently (no message), and so does an out-of-range index, which
        // in the reference would be an out-of-bounds read.
        if !matches!(
            string_data.get((entry.offset + entry.length) as usize),
            Some(&0)
        ) {
            return Err(String::new());
        }
    }

    if string_data_size as u64 != accumulated {
        return Err(String::new());
    }

    Ok((entries, string_data))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a BWST container payload from raw strings.
    fn string_table_payload(strings: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        let mut data = Vec::new();
        for s in strings {
            let offset = data.len() as u32;
            data.extend_from_slice(s.as_bytes());
            data.push(0);
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        }
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    /// Wrap a section payload in a BWTB container with a BWST header.
    fn container(payload: &[u8]) -> BinaryFormat {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BWTB");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(48u32 + payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(b"BWST");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&48u64.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(payload);
        BinaryFormat::open("test.bin", bytes).unwrap()
    }

    #[test]
    fn entry_parses_the_8_byte_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x22u32.to_le_bytes());
        bytes.extend_from_slice(&0x24u32.to_le_bytes());
        let entry = Entry::from_le_bytes(&bytes).unwrap();
        assert_eq!(entry.offset, 0x22);
        assert_eq!(entry.length, 0x24);
        assert_eq!(bytes.len(), 8);
    }

    #[test]
    fn entry_needs_8_bytes() {
        assert!(Entry::from_le_bytes(&[0; 7]).is_none());
    }

    #[test]
    fn invalid_index_is_all_ones() {
        assert_eq!(INVALID_INDEX, u32::MAX);
    }

    #[test]
    fn read_parses_entries_and_strings() {
        let reader = container(&string_table_payload(&[
            "spaces/sample_set/spaces/01.model",
            "spaces/sample_set/textures/grass.tga",
        ]));
        let table = StringTable::read(&reader).unwrap();

        assert!(table.is_valid());
        assert_eq!(table.size(), 2);
        assert_eq!(
            table.entry_str(0),
            Some("spaces/sample_set/spaces/01.model")
        );
        assert_eq!(
            table.entry_str(1),
            Some("spaces/sample_set/textures/grass.tga")
        );
        assert_eq!(table.entry_length(0), Some(33));
        assert_eq!(table.entry_length(1), Some(36));
        assert_eq!(table.entry_str(2), None);
        assert_eq!(table.entry_length(2), None);
        assert_eq!(
            table.entries(),
            &[
                Entry {
                    offset: 0,
                    length: 33
                },
                Entry {
                    offset: 34,
                    length: 36
                }
            ]
        );
    }

    #[test]
    fn entry_bytes_is_raw_and_excludes_the_nul() {
        let reader = container(&string_table_payload(&["abc"]));
        let table = StringTable::read(&reader).unwrap();
        assert_eq!(table.entry_bytes(0), Some(&b"abc"[..]));
    }

    #[test]
    fn read_accepts_an_empty_table() {
        // Zero entries, zero string data: the accumulated size is 0 == 0.
        let reader = container(&string_table_payload(&[]));
        let table = StringTable::read(&reader).unwrap();
        assert!(table.is_valid());
        assert_eq!(table.size(), 0);
    }

    #[test]
    fn read_reports_a_missing_section_with_both_messages() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BWTB");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&48u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(b"XXXX");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&48u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        let reader = BinaryFormat::open("no-table.bin", bytes).unwrap();

        let err = StringTable::read(&reader).unwrap_err();
        assert_eq!(
            err,
            "'no-table.bin' does not contain StringTable.\n\
             Failed to map string table 'no-table.bin' into memory.\n"
        );
    }

    #[test]
    fn read_reports_a_wrong_version_with_both_messages() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BWTB");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&48u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(b"BWST");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&48u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        let reader = BinaryFormat::open("v2.bin", bytes).unwrap();

        let err = StringTable::read(&reader).unwrap_err();
        assert_eq!(
            err,
            "'v2.bin' has the wrong version of StringTable (expected 2, got 1).\n\
             Failed to map string table 'v2.bin' into memory.\n"
        );
    }

    #[test]
    fn read_reports_a_truncated_entry_array() {
        // Type size prefix says 8 but the payload ends right after it.
        let mut payload = Vec::new();
        payload.extend_from_slice(&8u32.to_le_bytes());
        let reader = container(&payload);

        let err = StringTable::read(&reader).unwrap_err();
        assert_eq!(
            err,
            "Failed to read string table\n'test.bin' is malformed.\n"
        );
    }

    #[test]
    fn read_reports_a_truncated_string_data_block() {
        // The declared string data size runs past the section.
        let mut payload = Vec::new();
        payload.extend_from_slice(&8u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&10u32.to_le_bytes());
        let reader = container(&payload);

        let err = StringTable::read(&reader).unwrap_err();
        assert_eq!(
            err,
            "String table has invalid string data.\n'test.bin' is malformed.\n"
        );
    }

    #[test]
    fn read_reports_an_entry_beyond_the_string_data() {
        // Entry offset 0, length 5 but only 3 bytes of string data.
        let mut payload = Vec::new();
        payload.extend_from_slice(&8u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&5u32.to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(b"abc");
        let reader = container(&payload);

        let err = StringTable::read(&reader).unwrap_err();
        assert_eq!(
            err,
            "String table has incomplete data.\n'test.bin' is malformed.\n"
        );
    }

    #[test]
    fn read_fails_silently_when_the_nul_is_missing() {
        // One entry, "abc" without a NUL, then a byte the NUL check reads.
        let mut payload = Vec::new();
        payload.extend_from_slice(&8u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"abcX");
        let reader = container(&payload);

        let err = StringTable::read(&reader).unwrap_err();
        assert_eq!(err, "'test.bin' is malformed.\n");
    }

    #[test]
    fn read_fails_silently_when_the_accumulated_size_differs() {
        // Entry (0,3) over "abc\0" accumulates 4 but stringDataSize says 5;
        // pad the data so the extent read succeeds and only the totals
        // disagree.
        let mut payload = Vec::new();
        payload.extend_from_slice(&8u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&5u32.to_le_bytes());
        payload.extend_from_slice(b"abc\0Z");
        let reader = container(&payload);

        let err = StringTable::read(&reader).unwrap_err();
        assert_eq!(err, "'test.bin' is malformed.\n");
    }

    #[test]
    fn read_treats_a_missing_string_data_size_as_zero() {
        // The section ends after the (empty) entry array: the unchecked u32
        // read of the string data size stays 0 and an empty table validates.
        let mut payload = Vec::new();
        payload.extend_from_slice(&8u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        let reader = container(&payload);

        let table = StringTable::read(&reader).unwrap();
        assert_eq!(table.size(), 0);
    }

    #[test]
    fn read_accepts_a_string_of_the_wrong_encoding_as_bytes() {
        // Non-UTF-8 string data: entry_bytes still serves the raw bytes,
        // entry_str declines (a port-side convenience, not a reference check).
        let mut payload = Vec::new();
        payload.extend_from_slice(&8u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(&[0xff, 0xfe, 0xfd, 0x00]);
        let reader = container(&payload);

        let table = StringTable::read(&reader).unwrap();
        assert_eq!(table.entry_bytes(0), Some(&[0xff, 0xfe, 0xfd][..]));
        assert_eq!(table.entry_str(0), None);
    }
}
