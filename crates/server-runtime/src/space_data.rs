//! BW-08-01-03: propagate space data and topology changes.
//!
//! Space data is the per-space key/value store every CellApp and the manager
//! keep in step (`SpaceDataMapping`, `space_data_mapping.hpp`). Propagation
//! runs both ways over `CellAppMgrInterface::updateSpaceData`:
//!
//! - The CellApp side, `Space::spaceDataEntry` (`space.cpp:675`), stores the
//!   entry and — when told to update the manager — forwards it. Keys below
//!   [`SPACE_DATA_FIRST_CELL_ONLY_KEY`] (`space_data_types.hpp:44`) are
//!   cell-only and stay local; `Space::allSpaceData` (`space.cpp:666`) is the
//!   bulk receive path that feeds the same routine with
//!   `DONT_UPDATE_CELL_APP_MGR`.
//! - The manager side, `CellAppMgr::updateSpaceData` (`cellappmgr.cpp:2078`),
//!   stores the entry and re-sends it to the other CellApps. A key of
//!   `uint16(-1)` deletes the entry instead (`pSpace->delData`, `:2096`).
//!
//! Each space owns its mapping, and the mapping is looked up by `SpaceID` at
//! every step, so two spaces never share an entry — the acceptance of this
//! task, "多 Space 不串数据".
//!
//! The topology half is the cell info tree: `Space::updateGeometry`
//! (`space.cpp:885`) drops the old tree and installs the new one whenever the
//! manager adds or removes cells, reporting the cells that went
//! ("Cell in space %u is going", `:944`).

use std::collections::BTreeMap;

use crate::cellappmgr::{Addr, Rect};
use crate::space::SpaceId;

/// `SPACE_DATA_FIRST_CELL_ONLY_KEY` (`space_data_types.hpp:44`): keys below
/// this stay on the CellApp and are never forwarded to the manager.
pub const SPACE_DATA_FIRST_CELL_ONLY_KEY: u16 = 16384;

/// `uint16(-1)`: the deletion marker (`space_data_mapping.hpp:42`).
pub const DELETE_KEY: u16 = u16::MAX;

/// `SpaceEntryID` (`basictypes.hpp:394`): the address of the CellApp that
/// created the entry, plus the salt it bumps for each new entry
/// (`CellApp::nextSpaceEntryID`, `cellapp.cpp:4092`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SpaceEntryId {
    /// The CellApp that created the entry.
    pub creator: Addr,
    /// `salt`: the creating app's counter.
    pub salt: u16,
}

impl SpaceEntryId {
    /// An entry id from its parts.
    pub const fn new(creator: Addr, salt: u16) -> Self {
        SpaceEntryId { creator, salt }
    }
}

/// `CellApp::nextSpaceEntryID` (`cellapp.cpp:4092`): the creating app's
/// counter. Ids from different apps never collide because the creator address
/// is part of the id.
#[derive(Debug, Clone)]
pub struct SpaceEntryIds {
    creator: Addr,
    salt: u16,
}

impl SpaceEntryIds {
    /// An id generator for one app.
    pub const fn new(creator: Addr) -> Self {
        SpaceEntryIds { creator, salt: 0 }
    }

    /// `++nextSpaceEntryIDSalt_` (`cellapp.cpp:4095`). Named for the reference
    /// `nextSpaceEntryID`, since `next` would be taken for an iterator.
    pub fn next_entry_id(&mut self) -> SpaceEntryId {
        self.salt += 1;
        SpaceEntryId::new(self.creator, self.salt)
    }
}

/// `SpaceDataMapping::DataValue` (`space_data_mapping.hpp:25`): a key and its
/// payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataValue {
    /// The entry's key.
    pub key: u16,
    /// The entry's payload.
    pub data: Vec<u8>,
}

impl DataValue {
    /// A value from its parts.
    pub const fn new(key: u16, data: Vec<u8>) -> Self {
        DataValue { key, data }
    }

    /// `DataValue::valid`: the default-constructed marker is not a real value.
    pub fn is_valid(&self) -> bool {
        self.key != DELETE_KEY
    }
}

/// `SpaceDataMapping` (`space_data_mapping.hpp:17`): the data entries of one
/// space, keyed by `SpaceEntryID`.
#[derive(Debug, Clone, Default)]
pub struct SpaceDataMapping {
    entries: BTreeMap<SpaceEntryId, DataValue>,
}

impl SpaceDataMapping {
    /// An empty mapping.
    pub const fn new() -> Self {
        SpaceDataMapping {
            entries: BTreeMap::new(),
        }
    }

    /// `addDataEntry` (`space_data_mapping.cpp`): store an entry, replacing
    /// any previous payload for the same id. Returns whether this was new.
    pub fn add_data_entry(&mut self, entry: SpaceEntryId, key: u16, data: Vec<u8>) -> bool {
        self.entries
            .insert(entry, DataValue::new(key, data))
            .is_none()
    }

    /// `delDataEntry(entryID, oldValue)` (`space_data_mapping.cpp`): remove an
    /// entry and hand its old value back.
    pub fn del_data_entry(&mut self, entry: SpaceEntryId) -> Option<DataValue> {
        self.entries.remove(&entry)
    }

    /// `dataRetrieveFirst(key)`: the payload of the first entry holding a key.
    pub fn data_retrieve_first(&self, key: u16) -> Option<&[u8]> {
        self.entries
            .values()
            .find(|value| value.key == key)
            .map(|value| value.data.as_slice())
    }

    /// `dataRetrieveSpecific(entryID)`.
    pub fn data_retrieve_specific(&self, entry: SpaceEntryId) -> Option<&DataValue> {
        self.entries.get(&entry)
    }

    /// `SpaceDataMapping::size`.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the mapping holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `begin()`/`end()`.
    pub fn iter(&self) -> impl Iterator<Item = (&SpaceEntryId, &DataValue)> {
        self.entries.iter()
    }

    /// `clear`.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The result of `Space::spaceDataEntry` (`space.cpp:675`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceDataResult {
    /// `onSpaceData`: a fresh entry was stored, with the effective key.
    Added { key: u16 },
    /// `onSpaceDataDeleted`: a stored entry was removed, with its old key.
    Deleted { key: u16 },
    /// "attempted to add back a recently deleted entry!" (`space.cpp:715`).
    RecentlyDeleted,
    /// "Attempted to add or delete entry twice!" (`space.cpp:748`).
    AlreadyDone,
}

/// `RecentDataEntry` (`space.hpp:222`): what the CellApp remembers about the
/// entries it has touched recently, so a revoked entry is not re-added.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecentDataEntry {
    entry: SpaceEntryId,
    /// The key as it arrived, `DELETE_KEY` for revoked entries.
    key: u16,
}

/// The CellApp-side store: one mapping per space, plus the recent-entry log
/// and the global change sequence.
#[derive(Debug, Clone, Default)]
pub struct CellAppSpaceData {
    /// `s_allSpacesDataChangeSeq_` (`space.cpp:775`), bumped by every
    /// cell-only change in any space.
    pub all_spaces_data_change_seq: u32,
    mappings: BTreeMap<SpaceId, SpaceDataMapping>,
    recent: Vec<RecentDataEntry>,
}

impl CellAppSpaceData {
    /// An app with no space data yet.
    pub fn new() -> Self {
        CellAppSpaceData::default()
    }

    /// The mapping of one space.
    pub fn mapping(&self, space_id: SpaceId) -> Option<&SpaceDataMapping> {
        self.mappings.get(&space_id)
    }

    /// Whether a key is cell-only and must not reach the manager
    /// (`space.cpp:704`, `:773`).
    pub fn is_cell_only(key: u16) -> bool {
        key < SPACE_DATA_FIRST_CELL_ONLY_KEY
    }

    /// `Space::spaceDataEntry` (`space.cpp:675`) with
    /// `DONT_UPDATE_CELL_APP_MGR`: store or remove one entry of one space.
    pub fn space_data_entry(
        &mut self,
        space_id: SpaceId,
        entry: SpaceEntryId,
        key: u16,
        data: Vec<u8>,
    ) -> SpaceDataResult {
        // The recent-data guard: a recently revoked entry is not re-added
        // (`space.cpp:704`).
        if key < SPACE_DATA_FIRST_CELL_ONLY_KEY
            && self
                .recent
                .iter()
                .any(|r| r.entry == entry && r.key == DELETE_KEY)
        {
            return SpaceDataResult::RecentlyDeleted;
        }

        // The store-and-detect step (`space.cpp:724`).
        let effective_key;
        if key != DELETE_KEY {
            let mapping = self.mappings.entry(space_id).or_default();
            let added = mapping.add_data_entry(entry, key, data);
            if added {
                effective_key = key;
            } else {
                return SpaceDataResult::AlreadyDone;
            }
        } else {
            let Some(mapping) = self.mappings.get_mut(&space_id) else {
                return SpaceDataResult::AlreadyDone;
            };
            let Some(old) = mapping.del_data_entry(entry) else {
                return SpaceDataResult::AlreadyDone;
            };
            effective_key = old.key;
        }

        // Cell-only data is remembered so it cannot be re-added, and bumps
        // the global change sequence (`space.cpp:773`).
        if effective_key < SPACE_DATA_FIRST_CELL_ONLY_KEY {
            self.recent.push(RecentDataEntry { entry, key });
            self.all_spaces_data_change_seq += 1;
        }

        if key == DELETE_KEY {
            SpaceDataResult::Deleted { key: effective_key }
        } else {
            SpaceDataResult::Added { key: effective_key }
        }
    }

    /// `Space::allSpaceData` (`space.cpp:666`) feeding
    /// `readDataFromStream` (`:1311`): receive a whole mapping at once.
    pub fn read_all_space_data(
        &mut self,
        space_id: SpaceId,
        entries: &[(SpaceEntryId, u16, Vec<u8>)],
    ) -> Vec<SpaceDataResult> {
        entries
            .iter()
            .map(|(entry, key, data)| self.space_data_entry(space_id, *entry, *key, data.clone()))
            .collect()
    }

    /// The entries a newly created space would forward to the manager: none,
    /// because cell-only keys stay local (`space.cpp:773`).
    pub fn forwards_to_manager(key: u16) -> bool {
        key >= SPACE_DATA_FIRST_CELL_ONLY_KEY
    }
}

/// One re-propagation the manager owes the other CellApps
/// (`Space::sendSpaceDataUpdate`, `cellappmgr.cpp:2100`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Propagation {
    /// A stored entry, re-sent with its payload.
    Add {
        space_id: SpaceId,
        entry: SpaceEntryId,
        key: u16,
        data: Vec<u8>,
    },
    /// A deleted entry, re-sent with `uint16(-1)` and no payload.
    Delete {
        space_id: SpaceId,
        entry: SpaceEntryId,
    },
}

/// The result of `CellAppMgr::updateSpaceData` (`cellappmgr.cpp:2078`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerOutcome {
    /// `pSpace->addData(entryID, key, value)` and one `Add` propagation.
    Added,
    /// `pSpace->delData(entryID)` and one `Delete` propagation
    /// (`cellappmgr.cpp:2096`).
    Deleted,
    /// "Could not find space %u" (`cellappmgr.cpp:2115`).
    UnknownSpace,
}

/// The manager-side store: the authoritative copy of every space's data and
/// the re-propagations it owes.
#[derive(Debug, Clone, Default)]
pub struct ManagerSpaceData {
    mappings: BTreeMap<SpaceId, SpaceDataMapping>,
    /// The `sendSpaceDataUpdate` calls made so far, in order.
    pub propagations: Vec<Propagation>,
}

impl ManagerSpaceData {
    /// A manager with no space data yet.
    pub fn new() -> Self {
        ManagerSpaceData::default()
    }

    /// The mapping of one space.
    pub fn mapping(&self, space_id: SpaceId) -> Option<&SpaceDataMapping> {
        self.mappings.get(&space_id)
    }

    /// `CellAppMgr::updateSpaceData` (`cellappmgr.cpp:2078`): store an entry
    /// (or delete it, for a key of `uint16(-1)`) and queue the
    /// re-propagation to the other CellApps.
    pub fn update_space_data(
        &mut self,
        space_id: SpaceId,
        entry: SpaceEntryId,
        key: u16,
        data: Vec<u8>,
    ) -> ManagerOutcome {
        let Some(mapping) = self.mappings.get_mut(&space_id) else {
            return ManagerOutcome::UnknownSpace;
        };

        if key == DELETE_KEY {
            mapping.del_data_entry(entry);
            self.propagations
                .push(Propagation::Delete { space_id, entry });
            ManagerOutcome::Deleted
        } else {
            mapping.add_data_entry(entry, key, data.clone());
            self.propagations.push(Propagation::Add {
                space_id,
                entry,
                key,
                data,
            });
            ManagerOutcome::Added
        }
    }

    /// The manager's half of creating a space: give it an empty mapping so
    /// `updateSpaceData` can find it.
    pub fn create_space(&mut self, space_id: SpaceId) {
        self.mappings.entry(space_id).or_default();
    }
}

/// One leaf of the partitioning tree as the CellApp sees it (`CellInfo`,
/// `cell_info.hpp:32`): which app hosts the cell and the region it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct CellInfoLeaf {
    /// `addr_`: the CellApp that hosts this cell.
    pub app: Addr,
    /// `rect_`: the region this cell covers.
    pub rect: Rect,
}

impl CellInfoLeaf {
    /// `CellInfo::contains` (`cell_info.hpp:58`).
    pub fn contains(&self, x: f32, z: f32) -> bool {
        self.rect.contains_rect(&Rect::new(x, z, x, z))
    }
}

/// What `Space::updateGeometry` reported when the tree changed
/// (`space.cpp:885`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeometryUpdate {
    /// The apps whose cells are gone ("Cell in space %u is going", `:944`).
    pub cells_gone: Vec<Addr>,
    /// The apps whose cells arrived with the new tree.
    pub cells_arrived: Vec<Addr>,
}

/// The cell info tree of one space: the leaves of the manager's partitioning
/// (`SpaceNode`, `space_node.hpp`), kept as the flat list of leaves the
/// `updateGeometry` stream carries.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CellInfoTree {
    leaves: Vec<CellInfoLeaf>,
}

impl CellInfoTree {
    /// An empty tree: no cells yet.
    pub const fn new() -> Self {
        CellInfoTree { leaves: Vec::new() }
    }

    /// A tree from its leaves.
    pub fn from_leaves(leaves: Vec<CellInfoLeaf>) -> Self {
        CellInfoTree { leaves }
    }

    /// The leaves, in order.
    pub fn leaves(&self) -> &[CellInfoLeaf] {
        &self.leaves
    }

    /// The number of cells in the tree.
    pub fn num_cells(&self) -> usize {
        self.leaves.len()
    }

    /// The distinct apps hosting a cell.
    pub fn apps(&self) -> Vec<Addr> {
        let mut apps: Vec<Addr> = self.leaves.iter().map(|leaf| leaf.app).collect();
        apps.sort_unstable();
        apps.dedup();
        apps
    }

    /// `Space::findCell(addr)` (`space.cpp`): the leaf hosted by an app.
    pub fn find_cell(&self, app: Addr) -> Option<&CellInfoLeaf> {
        self.leaves.iter().find(|leaf| leaf.app == app)
    }

    /// `SpaceNode::pCellAt(x, z)`: the leaf covering a position.
    pub fn cell_at(&self, x: f32, z: f32) -> Option<&CellInfoLeaf> {
        self.leaves.iter().find(|leaf| leaf.contains(x, z))
    }

    /// `Space::updateGeometry` (`space.cpp:885`): drop the old tree and
    /// install the new one, reporting the topology change.
    pub fn update_geometry(&mut self, next: CellInfoTree) -> GeometryUpdate {
        let old_apps = self.apps();
        let new_apps = next.apps();
        let report = GeometryUpdate {
            cells_gone: old_apps
                .iter()
                .copied()
                .filter(|app| !new_apps.contains(app))
                .collect(),
            cells_arrived: new_apps
                .iter()
                .copied()
                .filter(|app| !old_apps.contains(app))
                .collect(),
        };
        self.leaves = next.leaves;
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_a() -> Addr {
        (0x7f000001, 3000)
    }

    fn app_b() -> Addr {
        (0x7f000002, 3000)
    }

    /// The manager stores an entry, deletes it by the marker key, and
    /// re-propagates both to the other CellApps (`cellappmgr.cpp:2078`).
    #[test]
    fn the_manager_stores_and_repropagates_entries() {
        let mut mgr = ManagerSpaceData::new();
        mgr.create_space(7);
        let entry = SpaceEntryId::new(app_a(), 1);

        assert_eq!(
            mgr.update_space_data(7, entry, SPACE_DATA_FIRST_CELL_ONLY_KEY, b"bounds".to_vec()),
            ManagerOutcome::Added
        );
        assert_eq!(
            mgr.mapping(7)
                .unwrap()
                .data_retrieve_specific(entry)
                .unwrap()
                .data,
            b"bounds".to_vec()
        );
        assert_eq!(
            mgr.update_space_data(7, entry, DELETE_KEY, Vec::new()),
            ManagerOutcome::Deleted
        );
        assert!(
            mgr.mapping(7)
                .unwrap()
                .data_retrieve_specific(entry)
                .is_none()
        );

        assert_eq!(
            mgr.propagations,
            vec![
                Propagation::Add {
                    space_id: 7,
                    entry,
                    key: SPACE_DATA_FIRST_CELL_ONLY_KEY,
                    data: b"bounds".to_vec(),
                },
                Propagation::Delete { space_id: 7, entry },
            ]
        );
    }

    /// An update for an unknown space is refused with the manager's error
    /// path (`cellappmgr.cpp:2115`).
    #[test]
    fn the_manager_refuses_an_unknown_space() {
        let mut mgr = ManagerSpaceData::new();
        assert_eq!(
            mgr.update_space_data(9, SpaceEntryId::new(app_a(), 1), 1, Vec::new()),
            ManagerOutcome::UnknownSpace
        );
        assert!(mgr.propagations.is_empty());
    }

    /// The CellApp stores an entry and reports it, then deletes it and
    /// reports the deletion with its old key (`space.cpp:675`).
    #[test]
    fn the_cell_app_stores_and_deletes_entries() {
        let mut app = CellAppSpaceData::new();
        let entry = SpaceEntryId::new(app_a(), 1);

        assert_eq!(
            app.space_data_entry(7, entry, 1, b"tod".to_vec()),
            SpaceDataResult::Added { key: 1 }
        );
        assert_eq!(
            app.mapping(7).unwrap().data_retrieve_first(1),
            Some(&b"tod".to_vec()[..])
        );
        assert_eq!(
            app.space_data_entry(7, entry, DELETE_KEY, Vec::new()),
            SpaceDataResult::Deleted { key: 1 }
        );
        assert!(app.mapping(7).unwrap().is_empty());
    }

    /// Adding the same entry twice is refused (`space.cpp:748`).
    #[test]
    fn adding_an_entry_twice_is_refused() {
        let mut app = CellAppSpaceData::new();
        let entry = SpaceEntryId::new(app_a(), 1);
        app.space_data_entry(7, entry, 1, b"tod".to_vec());

        assert_eq!(
            app.space_data_entry(7, entry, 1, b"other".to_vec()),
            SpaceDataResult::AlreadyDone
        );
    }

    /// A recently revoked cell-only entry is not re-added (`space.cpp:704`).
    #[test]
    fn a_recently_deleted_entry_is_not_re_added() {
        let mut app = CellAppSpaceData::new();
        let entry = SpaceEntryId::new(app_a(), 1);
        app.space_data_entry(7, entry, 1, b"tod".to_vec());
        app.space_data_entry(7, entry, DELETE_KEY, Vec::new());

        assert_eq!(
            app.space_data_entry(7, entry, 1, b"tod".to_vec()),
            SpaceDataResult::RecentlyDeleted
        );
    }

    /// The recent-data guard only applies to cell-only keys, so a shared key
    /// above the threshold can come back.
    #[test]
    fn the_recent_guard_only_covers_cell_only_keys() {
        let mut app = CellAppSpaceData::new();
        let entry = SpaceEntryId::new(app_a(), 1);
        let shared = SPACE_DATA_FIRST_CELL_ONLY_KEY + 5;
        app.space_data_entry(7, entry, shared, b"v1".to_vec());
        app.space_data_entry(7, entry, DELETE_KEY, Vec::new());

        assert_eq!(
            app.space_data_entry(7, entry, shared, b"v2".to_vec()),
            SpaceDataResult::Added { key: shared }
        );
    }

    /// Cell-only changes bump the global change sequence, shared ones do not
    /// (`space.cpp:773`).
    #[test]
    fn cell_only_changes_bump_the_global_sequence() {
        let mut app = CellAppSpaceData::new();
        let shared = SPACE_DATA_FIRST_CELL_ONLY_KEY;
        assert!(CellAppSpaceData::forwards_to_manager(shared));
        assert!(!CellAppSpaceData::forwards_to_manager(shared - 1));

        app.space_data_entry(7, SpaceEntryId::new(app_a(), 1), shared, Vec::new());
        assert_eq!(app.all_spaces_data_change_seq, 0);

        app.space_data_entry(7, SpaceEntryId::new(app_a(), 2), 1, Vec::new());
        assert_eq!(app.all_spaces_data_change_seq, 1);
    }

    /// The acceptance, "多 Space 不串数据": the same entry id and key in two
    /// spaces stay in their own mappings.
    #[test]
    fn two_spaces_never_share_entries() {
        let mut app = CellAppSpaceData::new();
        let entry = SpaceEntryId::new(app_a(), 1);
        app.space_data_entry(7, entry, 3, b"seven".to_vec());
        app.space_data_entry(9, entry, 3, b"nine".to_vec());

        assert_eq!(
            app.mapping(7).unwrap().data_retrieve_first(3),
            Some(&b"seven".to_vec()[..])
        );
        assert_eq!(
            app.mapping(9).unwrap().data_retrieve_first(3),
            Some(&b"nine".to_vec()[..])
        );

        // Deleting from one space leaves the other alone.
        app.space_data_entry(7, entry, DELETE_KEY, Vec::new());
        assert!(app.mapping(7).unwrap().is_empty());
        assert_eq!(app.mapping(9).unwrap().len(), 1);

        // The recent-data log is per app, not per space, but the guard still
        // does not reach into the untouched space's mapping.
        assert!(
            app.mapping(9)
                .unwrap()
                .data_retrieve_specific(entry)
                .is_some()
        );
    }

    /// A deletion for a space with no mapping is the "twice" error, not a
    /// crash.
    #[test]
    fn deleting_from_an_unknown_space_is_reported() {
        let mut app = CellAppSpaceData::new();
        assert_eq!(
            app.space_data_entry(7, SpaceEntryId::new(app_a(), 1), DELETE_KEY, Vec::new()),
            SpaceDataResult::AlreadyDone
        );
    }

    /// `allSpaceData` loads a whole mapping at once and refuses the duplicate
    /// entries in it (`space.cpp:1311`).
    #[test]
    fn all_space_data_loads_a_whole_mapping() {
        let mut app = CellAppSpaceData::new();
        let e1 = SpaceEntryId::new(app_a(), 1);
        let e2 = SpaceEntryId::new(app_a(), 2);
        let results = app.read_all_space_data(
            7,
            &[
                (e1, 1, b"a".to_vec()),
                (e2, 2, b"b".to_vec()),
                (e2, 2, b"b".to_vec()),
            ],
        );

        assert_eq!(
            results,
            vec![
                SpaceDataResult::Added { key: 1 },
                SpaceDataResult::Added { key: 2 },
                SpaceDataResult::AlreadyDone,
            ]
        );
        assert_eq!(app.mapping(7).unwrap().len(), 2);
    }

    /// The entry-id generator produces distinct ids and the creator address
    /// keeps two apps' ids apart (`cellapp.cpp:4092`).
    #[test]
    fn entry_ids_never_collide_across_apps() {
        let mut a = SpaceEntryIds::new(app_a());
        let mut b = SpaceEntryIds::new(app_b());
        let first = a.next_entry_id();
        assert_eq!(first, SpaceEntryId::new(app_a(), 1));
        assert_eq!(a.next_entry_id(), SpaceEntryId::new(app_a(), 2));
        assert_eq!(b.next_entry_id(), SpaceEntryId::new(app_b(), 1));
        assert_ne!(first, b.next_entry_id());
    }

    /// `updateGeometry` swaps the tree and reports the apps that went and the
    /// ones that arrived (`space.cpp:885`).
    #[test]
    fn update_geometry_reports_the_topology_change() {
        let mut tree = CellInfoTree::from_leaves(vec![CellInfoLeaf {
            app: app_a(),
            rect: Rect::unbounded(),
        }]);

        let report = tree.update_geometry(CellInfoTree::from_leaves(vec![
            CellInfoLeaf {
                app: app_a(),
                rect: Rect::new(-f32::MAX, -f32::MAX, 0.0, f32::MAX),
            },
            CellInfoLeaf {
                app: app_b(),
                rect: Rect::new(0.0, -f32::MAX, f32::MAX, f32::MAX),
            },
        ]));

        assert!(report.cells_gone.is_empty());
        assert_eq!(report.cells_arrived, vec![app_b()]);
        assert_eq!(tree.num_cells(), 2);
        assert_eq!(tree.apps(), vec![app_a(), app_b()]);
    }

    /// Removing a cell from the tree reports it as going
    /// ("Cell in space %u is going", `space.cpp:944`).
    #[test]
    fn a_removed_cell_is_reported_as_going() {
        let mut tree = CellInfoTree::from_leaves(vec![
            CellInfoLeaf {
                app: app_a(),
                rect: Rect::new(-f32::MAX, -f32::MAX, 0.0, f32::MAX),
            },
            CellInfoLeaf {
                app: app_b(),
                rect: Rect::new(0.0, -f32::MAX, f32::MAX, f32::MAX),
            },
        ]);

        let report = tree.update_geometry(CellInfoTree::from_leaves(vec![CellInfoLeaf {
            app: app_a(),
            rect: Rect::unbounded(),
        }]));

        assert_eq!(report.cells_gone, vec![app_b()]);
        assert!(report.cells_arrived.is_empty());
        assert_eq!(tree.find_cell(app_b()), None);
    }

    /// The tree finds the cell covering a position and the cell of an app.
    #[test]
    fn the_tree_finds_cells_by_position_and_app() {
        let tree = CellInfoTree::from_leaves(vec![
            CellInfoLeaf {
                app: app_a(),
                rect: Rect::new(-10.0, -10.0, 0.0, 10.0),
            },
            CellInfoLeaf {
                app: app_b(),
                rect: Rect::new(0.0, -10.0, 10.0, 10.0),
            },
        ]);

        assert_eq!(tree.cell_at(-5.0, 0.0).unwrap().app, app_a());
        assert_eq!(tree.cell_at(5.0, 0.0).unwrap().app, app_b());
        assert!(tree.cell_at(50.0, 0.0).is_none());
        assert_eq!(tree.find_cell(app_b()).unwrap().app, app_b());
    }

    /// A full round trip: the CellApp stores an entry, the manager receives
    /// the forwarded update, and another CellApp applies the manager's
    /// re-propagation.
    #[test]
    fn a_round_trip_keeps_two_apps_in_step() {
        let shared = SPACE_DATA_FIRST_CELL_ONLY_KEY;
        let entry = SpaceEntryId::new(app_a(), 1);
        let payload = b"arena bounds".to_vec();

        // The originating app stores the entry.
        let mut origin = CellAppSpaceData::new();
        assert_eq!(
            origin.space_data_entry(7, entry, shared, payload.clone()),
            SpaceDataResult::Added { key: shared }
        );

        // The manager stores the forwarded update and re-propagates it.
        let mut mgr = ManagerSpaceData::new();
        mgr.create_space(7);
        assert_eq!(
            mgr.update_space_data(7, entry, shared, payload.clone()),
            ManagerOutcome::Added
        );

        // Another CellApp applies the propagation.
        let mut peer = CellAppSpaceData::new();
        match &mgr.propagations[0] {
            Propagation::Add {
                space_id,
                entry,
                key,
                data,
            } => {
                assert_eq!(
                    peer.space_data_entry(*space_id, *entry, *key, data.clone()),
                    SpaceDataResult::Added { key: shared }
                );
            }
            other => panic!("expected an Add propagation, got {other:?}"),
        }

        // All three agree on the payload.
        assert_eq!(
            peer.mapping(7).unwrap().data_retrieve_first(shared),
            Some(&payload[..])
        );
        assert_eq!(
            mgr.mapping(7).unwrap().data_retrieve_first(shared),
            Some(&payload[..])
        );
        assert_eq!(
            origin.mapping(7).unwrap().data_retrieve_first(shared),
            Some(&payload[..])
        );
    }
}
