//! BW-10-02-01 the `BWAL` asset list section.
//!
//! A port of `lib/compiled_space/asset_list_types.hpp` (`AssetType`, `Entry`,
//! format magic/version) and the reader half of `lib/compiled_space/asset_list.cpp`
//! (`AssetList::read` and the accessors). The preload half (`preloadAssets`)
//! is client-side rendering machinery (Moo textures, Models) and stays with
//! the client.
//!
//! Payload layout: an `ExternalArray<Entry>` — a u32 typeSize that must equal
//! 8, a u32 count, then `count` × 8-byte entries. The original `Entry` has no
//! `#pragma pack`, so it is `{u8 type_, 3 padding bytes, u32 stringTableIndex_}`
//! and the writer leaves the padding uninitialized — genuine files carry
//! stack garbage there, which the reader neither checks nor zeroes.

use crate::binary_format::{BinaryFormat, Magic};

/// `AssetListTypes::FORMAT_MAGIC`.
pub const FORMAT_MAGIC: Magic = *b"BWAL";

/// `AssetListTypes::FORMAT_VERSION`.
pub const FORMAT_VERSION: u32 = 1;

/// `sizeof(AssetListTypes::Entry)` — the ExternalArray element size,
/// including the three padding bytes.
pub const ENTRY_SIZE: usize = 8;

/// `AssetListTypes::AssetType`. The reference stores the raw byte
/// unvalidated; the named constants below cover every value the writer emits
/// (`asset_list_types.hpp:12-22`).
pub type AssetType = u8;

/// `AssetListTypes::ASSET_TYPE_UNKNOWN`.
pub const ASSET_TYPE_UNKNOWN: AssetType = 0;

/// `AssetListTypes::ASSET_TYPE_DATASECTION`.
pub const ASSET_TYPE_DATASECTION: AssetType = 1;

/// `AssetListTypes::ASSET_TYPE_TEXTURE`.
pub const ASSET_TYPE_TEXTURE: AssetType = 2;

/// `AssetListTypes::ASSET_TYPE_EFFECT`.
pub const ASSET_TYPE_EFFECT: AssetType = 3;

/// `AssetListTypes::ASSET_TYPE_PRIMITIVE`.
pub const ASSET_TYPE_PRIMITIVE: AssetType = 4;

/// `AssetListTypes::ASSET_TYPE_VISUAL`.
pub const ASSET_TYPE_VISUAL: AssetType = 5;

/// `AssetListTypes::ASSET_TYPE_MODEL`.
pub const ASSET_TYPE_MODEL: AssetType = 6;

/// `AssetListTypes::Entry`: `{AssetType type_, uint32 stringTableIndex_}`
/// with three unwritten alignment bytes between the fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub asset_type: AssetType,
    pub string_table_index: u32,
}

impl Entry {
    /// Parse one 8-byte little-endian entry. The three padding bytes are
    /// skipped, whatever they hold.
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < ENTRY_SIZE {
            return None;
        }
        Some(Self {
            asset_type: bytes[0],
            string_table_index: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }
}

/// `AssetList` (`asset_list.cpp:63-161`): the parsed entry list, plus the
/// valid flag that tracks whether the ExternalArray mapped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AssetList {
    entries: Vec<Entry>,
    valid: bool,
}

impl AssetList {
    /// `AssetList::read` (`asset_list.cpp:80-102`).
    ///
    /// Quirk kept: when the entry array fails to read — a wrong typeSize or
    /// a count that runs past the section — the reference `close()`s and
    /// still returns `true`, leaving `isValid()` false; the port mirrors
    /// that with `Ok` and a cleared list. Only a missing or wrong-version
    /// `BWAL` section is an `Err` (the message `findAndOpenSection` printed).
    pub fn read(reader: &BinaryFormat) -> Result<Self, String> {
        let mut stream = reader.find_and_open_section(FORMAT_MAGIC, FORMAT_VERSION, "AssetList")?;

        match stream.read_external_array(ENTRY_SIZE) {
            Some(raw) => {
                let mut entries = Vec::with_capacity(raw.len() / ENTRY_SIZE);
                for chunk in raw.as_chunks::<ENTRY_SIZE>().0 {
                    entries.push(Entry::from_le_bytes(chunk).expect("as_chunks yields 8 bytes"));
                }
                Ok(Self {
                    entries,
                    valid: true,
                })
            }
            // The reference resets the entries and still reports success
            // (asset_list.cpp:97-102).
            None => Ok(Self {
                entries: Vec::new(),
                valid: false,
            }),
        }
    }

    /// `AssetList::isValid` (`asset_list.cpp:123-126`): whether the entry
    /// array mapped (an empty but well-formed list is valid).
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// `AssetList::size` (`asset_list.cpp:130-135`).
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// `AssetList::empty` (`asset_list.cpp:138-141`).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The parsed entry list.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// `AssetList::assetType` (`asset_list.cpp:145-151`).
    pub fn asset_type(&self, idx: usize) -> Option<AssetType> {
        self.entries.get(idx).map(|e| e.asset_type)
    }

    /// `AssetList::stringTableIndex` (`asset_list.cpp:155-161`).
    pub fn string_table_index(&self, idx: usize) -> Option<u32> {
        self.entries.get(idx).map(|e| e.string_table_index)
    }
}

/// Wrap a `BWAL` payload in a one-section BWTB container.
#[cfg(test)]
mod tests {
    use super::*;

    fn container(payload: &[u8]) -> BinaryFormat {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BWTB");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(48u32 + payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(b"BWAL");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&48u64.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(payload);
        BinaryFormat::open("test.bin", bytes).unwrap()
    }

    fn entry_bytes(asset_type: AssetType, pad: [u8; 3], index: u32) -> [u8; ENTRY_SIZE] {
        [
            asset_type,
            pad[0],
            pad[1],
            pad[2],
            index.to_le_bytes()[0],
            index.to_le_bytes()[1],
            index.to_le_bytes()[2],
            index.to_le_bytes()[3],
        ]
    }

    #[test]
    fn entry_parses_type_and_index_and_skips_padding() {
        // The padding is genuinely uninitialized in original files; here it
        // carries the stack garbage bytes a genuine sample holds.
        let bytes = entry_bytes(ASSET_TYPE_TEXTURE, [0xdf, 0xac, 0x91], 1);
        let entry = Entry::from_le_bytes(&bytes).unwrap();
        assert_eq!(entry.asset_type, ASSET_TYPE_TEXTURE);
        assert_eq!(entry.string_table_index, 1);
    }

    #[test]
    fn entry_needs_8_bytes() {
        assert!(Entry::from_le_bytes(&[0; 7]).is_none());
    }

    #[test]
    fn read_parses_entries_of_every_written_type() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&entry_bytes(ASSET_TYPE_MODEL, [0, 0, 0], 0));
        payload.extend_from_slice(&entry_bytes(ASSET_TYPE_TEXTURE, [0, 0, 0], 1));
        payload.extend_from_slice(&entry_bytes(ASSET_TYPE_EFFECT, [0, 0, 0], 2));
        let reader = container(&payload);

        let assets = AssetList::read(&reader).unwrap();
        assert!(assets.is_valid());
        assert_eq!(assets.size(), 3);
        assert!(!assets.is_empty());
        assert_eq!(assets.asset_type(0), Some(ASSET_TYPE_MODEL));
        assert_eq!(assets.asset_type(1), Some(ASSET_TYPE_TEXTURE));
        assert_eq!(assets.asset_type(2), Some(ASSET_TYPE_EFFECT));
        assert_eq!(assets.string_table_index(0), Some(0));
        assert_eq!(assets.string_table_index(1), Some(1));
        assert_eq!(assets.string_table_index(2), Some(2));
        assert_eq!(assets.asset_type(3), None);
        assert_eq!(assets.string_table_index(3), None);
        assert_eq!(assets.entries().len(), 3);
    }

    #[test]
    fn read_keeps_unwritten_type_bytes() {
        // The reference stores whatever byte the file holds.
        let mut payload = Vec::new();
        payload.extend_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&entry_bytes(99, [0, 0, 0], 0));
        let reader = container(&payload);

        let assets = AssetList::read(&reader).unwrap();
        assert_eq!(assets.asset_type(0), Some(99));
    }

    #[test]
    fn read_accepts_an_empty_list() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        let reader = container(&payload);

        let assets = AssetList::read(&reader).unwrap();
        assert!(assets.is_valid());
        assert_eq!(assets.size(), 0);
        assert!(assets.is_empty());
    }

    #[test]
    fn read_still_succeeds_when_the_entries_are_malformed() {
        // Quirk: a wrong typeSize resets the list but read returns true.
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes());
        let reader = container(&payload);

        let assets = AssetList::read(&reader).unwrap();
        assert!(!assets.is_valid());
        assert_eq!(assets.size(), 0);
    }

    #[test]
    fn read_still_succeeds_when_the_count_overruns() {
        // The count runs past the section payload.
        let mut payload = Vec::new();
        payload.extend_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(&[0u8; 16]);
        let reader = container(&payload);

        let assets = AssetList::read(&reader).unwrap();
        assert!(!assets.is_valid());
        assert_eq!(assets.size(), 0);
    }

    #[test]
    fn read_reports_a_missing_section() {
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
        let reader = BinaryFormat::open("no-assets.bin", bytes).unwrap();

        let err = AssetList::read(&reader).unwrap_err();
        assert_eq!(err, "'no-assets.bin' does not contain AssetList.\n");
    }

    #[test]
    fn read_reports_a_wrong_version() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BWTB");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&48u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(b"BWAL");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&48u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        let reader = BinaryFormat::open("v2.bin", bytes).unwrap();

        let err = AssetList::read(&reader).unwrap_err();
        assert_eq!(
            err,
            "'v2.bin' has the wrong version of AssetList (expected 2, got 1).\n"
        );
    }
}
