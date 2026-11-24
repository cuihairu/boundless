//! The space-data mapping and its key constants (`lib/network/space_data_mapping.*`,
//! `common/space_data_types.hpp`).
//!
//! A space's data is a set of `(SpaceEntryID, key, value)` rows — the
//! authoritative copy lives on the CellAppMgr and every CellApp serving the
//! space mirrors it (`Space::spaceDataMapping_`). The mapping is the
//! per-space state this module tracks; the routing that keeps one space's
//! rows out of another's is [`crate::space::Space::space_data_entry`]'s,
//! driven per space.

use std::collections::BTreeMap;

use crate::space::SpaceEntryId;

// -----------------------------------------------------------------------------
// Section: The key constants (`common/space_data_types.hpp`)
// -----------------------------------------------------------------------------

/// `uint16(-1)`: the delete key.
pub const NO_KEY: u16 = u16::MAX;

/// `SPACE_DATA_TOD_KEY` — the `SpaceData_ToDData` payload (two `f32`s).
pub const SPACE_DATA_TOD_KEY: u16 = 0;

/// `SPACE_DATA_MAPPING_KEY_CLIENT_SERVER` — geometry both sides load. The
/// payload is `SpaceData_MappingData` (a 4x4 matrix) followed by the path.
pub const SPACE_DATA_MAPPING_KEY_CLIENT_SERVER: u16 = 1;

/// `SPACE_DATA_MAPPING_KEY_CLIENT_ONLY` — client-only geometry; the server
/// records the row but loads nothing.
pub const SPACE_DATA_MAPPING_KEY_CLIENT_ONLY: u16 = 2;

/// `SPACE_DATA_FIRST_USER_KEY`.
pub const SPACE_DATA_FIRST_USER_KEY: u16 = 256;

/// `SPACE_DATA_FINAL_USER_KEY`.
pub const SPACE_DATA_FINAL_USER_KEY: u16 = 32767;

/// `SPACE_DATA_FIRST_CELL_ONLY_KEY`: keys at and above this never reach a
/// client, and only these bypass the recently-deleted guard
/// (`space.cpp:704`).
pub const SPACE_DATA_FIRST_CELL_ONLY_KEY: u16 = 16384;

/// `SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY`.
pub const SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY: u16 = 32768;

/// `SPACE_DATA_RECORDING` — the replay recording name (`cell.cpp`
/// `startRecording`; the recording face is the replay lane's).
pub const SPACE_DATA_RECORDING: u16 = SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY;

/// `SPACE_DATA_ARTIFICIAL_LOAD` — the `SpaceData_ArtificialLoad` payload
/// (one `f32`).
pub const SPACE_DATA_ARTIFICIAL_LOAD: u16 = SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY + 1;

/// `SPACE_DATA_SERVER_LOAD_BOUNDS` — the `SpaceData_LoadBounds` payload.
pub const SPACE_DATA_SERVER_LOAD_BOUNDS: u16 = SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY + 2;

// -----------------------------------------------------------------------------
// Section: The mapping
// -----------------------------------------------------------------------------

/// One stored value: `SpaceDataMapping::DataValue`. The `NO_KEY` key marks
/// the invalid value a lookup misses with. The payload is the reference's
/// `BW::string` — a byte string, since the well-known keys carry raw
/// structs (`SpaceData_ToDData` and friends).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataValue {
    /// `key_`.
    pub key: u16,
    /// `data_`.
    pub data: Vec<u8>,
}

impl DataValue {
    /// The invalid value (`DataValue()`), key `uint16(-1)`.
    pub fn invalid() -> Self {
        Self {
            key: NO_KEY,
            data: Vec::new(),
        }
    }

    /// `valid()`.
    pub fn valid(&self) -> bool {
        self.key != NO_KEY
    }
}

/// The per-space data rows (`SpaceDataMapping`,
/// `lib/network/space_data_mapping.cpp`), keyed by entry id like the
/// reference's `BW::map` — so "the first entry for a key" is the lowest
/// entry id holding it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceDataMapping {
    /// `dataEntries_`.
    data_entries: BTreeMap<SpaceEntryId, DataValue>,
}

impl SpaceDataMapping {
    /// A fresh, empty mapping.
    pub fn new() -> Self {
        Self::default()
    }

    /// `clear()`.
    pub fn clear(&mut self) {
        self.data_entries.clear();
    }

    /// `addDataEntry` (`space_data_mapping.cpp:22-36`): one row per entry
    /// id — a second add for the same id is refused, exactly the
    /// "Attempted to add or delete entry twice" face.
    pub fn add_data_entry(&mut self, entry_id: SpaceEntryId, key: u16, data: &[u8]) -> bool {
        if self.data_entries.contains_key(&entry_id) {
            return false;
        }
        self.data_entries.insert(
            entry_id,
            DataValue {
                key,
                data: data.to_vec(),
            },
        );
        true
    }

    /// `delDataEntry(entryID)` without the old value out.
    pub fn del_data_entry(&mut self, entry_id: SpaceEntryId) -> bool {
        self.data_entries.remove(&entry_id).is_some()
    }

    /// `delDataEntry(entryID, oldValue)` (`space_data_mapping.cpp:48-62`):
    /// erase the row and hand back what it held.
    pub fn del_data_entry_with_old(&mut self, entry_id: SpaceEntryId) -> Option<DataValue> {
        self.data_entries.remove(&entry_id)
    }

    /// `dataRetrieveFirst` (`space_data_mapping.cpp:104-118`): the data of
    /// the first row carrying `key`, in entry-id order.
    pub fn data_retrieve_first(&self, key: u16) -> Option<&[u8]> {
        self.data_entries
            .values()
            .find(|value| value.key == key)
            .map(|value| value.data.as_slice())
    }

    /// `dataRetrieveSpecificPtr`: the row for the exact entry id.
    pub fn data_retrieve_specific(&self, entry_id: SpaceEntryId) -> Option<&DataValue> {
        self.data_entries.get(&entry_id)
    }

    /// `size()`.
    pub fn len(&self) -> usize {
        self.data_entries.len()
    }

    /// Whether the mapping holds no rows.
    pub fn is_empty(&self) -> bool {
        self.data_entries.is_empty()
    }

    /// The rows, in entry-id order.
    pub fn iter(&self) -> impl Iterator<Item = (&SpaceEntryId, &DataValue)> {
        self.data_entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // TDD: BW-08-01-03 传播空间数据和拓扑变化 — 多 Space 不串数据
    // (the mapping half: one row per entry id, first-row-per-key lookups)
    // -------------------------------------------------------------------

    /// `addDataEntry` (`space_data_mapping.cpp:22-36`): novel ids add;
    /// a duplicate id is refused and keeps the original row.
    #[test]
    fn adding_a_row_refuses_a_duplicate_entry_id() {
        let mut mapping = SpaceDataMapping::new();
        assert!(mapping.add_data_entry((1, 5000), 7, b"alpha"));
        assert!(!mapping.add_data_entry((1, 5000), 8, b"beta"));
        assert_eq!(mapping.len(), 1);
        let row = mapping.data_retrieve_specific((1, 5000)).unwrap();
        assert_eq!(row.key, 7);
        assert_eq!(row.data, b"alpha");
    }

    /// `dataRetrieveFirst` (`space_data_mapping.cpp:104-118`): the first
    /// row for a key wins, in entry-id order — the ordering
    /// `BW::map` gives for free.
    #[test]
    fn the_first_row_for_a_key_wins_in_entry_id_order() {
        let mut mapping = SpaceDataMapping::new();
        assert!(mapping.add_data_entry((2, 5001), 7, b"second"));
        assert!(mapping.add_data_entry((1, 5000), 7, b"first"));
        assert!(mapping.add_data_entry((3, 5002), 8, b"other"));
        assert_eq!(mapping.data_retrieve_first(7), Some(b"first".as_slice()));
        assert_eq!(mapping.data_retrieve_first(8), Some(b"other".as_slice()));
        assert_eq!(mapping.data_retrieve_first(9), None);
    }

    /// `delDataEntry(entryID, oldValue)` (`space_data_mapping.cpp:48-62`):
    /// the erase hands back the old row; a miss answers nothing.
    #[test]
    fn deleting_a_row_hands_back_the_old_value() {
        let mut mapping = SpaceDataMapping::new();
        assert!(mapping.add_data_entry((1, 5000), 7, b"gone soon"));
        let old = mapping.del_data_entry_with_old((1, 5000)).unwrap();
        assert_eq!(old.key, 7);
        assert_eq!(old.data, b"gone soon");
        assert!(mapping.is_empty());
        assert!(mapping.del_data_entry_with_old((1, 5000)).is_none());
        assert!(!mapping.del_data_entry((1, 5000)));
    }

    /// The invalid value (`DataValue()`) is the `NO_KEY` sentinel and
    /// answers `valid() == false`.
    #[test]
    fn the_invalid_value_carries_the_no_key_sentinel() {
        let invalid = DataValue::invalid();
        assert!(!invalid.valid());
        assert_eq!(invalid.key, NO_KEY);
        let valid = DataValue {
            key: 1,
            data: b"x".to_vec(),
        };
        assert!(valid.valid());
    }

    /// `clear()` drops every row.
    #[test]
    fn clearing_drops_every_row() {
        let mut mapping = SpaceDataMapping::new();
        mapping.add_data_entry((1, 5000), 0, b"tod");
        mapping.add_data_entry((2, 5001), 1, b"mapping");
        assert_eq!(mapping.len(), 2);
        mapping.clear();
        assert!(mapping.is_empty());
        assert_eq!(
            mapping.iter().count(),
            0,
            "the iterator drains with the map"
        );
    }
}
