//! BW-08-01-02: create and register a Cell.
//!
//! One half of this handshake already lives in [`crate::cellappmgr`]:
//! `CellAppMgr::add_cell_for_app` (`cellappmgr.rs:3035`) records a
//! [`crate::cellappmgr::CellLeaf`] on the manager's space and queues
//! [`crate::cellappmgr::Outbound::AddCell`] for the chosen app. This module is
//! the other half, the CellApp side: `CellApp::addCell` (`cellapp.cpp:1606`)
//! finds or re-creates the space, then creates a `Cell` (`cell.cpp:82`) which
//! registers itself on that space (`space_.pCell(this)`) and is kept in the
//! app's `cells_` collection (`cells.hpp`). The manager's `CellLeaf` for a
//! space and the CellApp's own `Cell` for the same space are the two ends of
//! one fact, so both are exposed and checked against each other.

use std::collections::{BTreeMap, BTreeSet};

use crate::cellappmgr::{Addr, Rect};
use crate::space::{CellRef, Space, SpaceId, Spaces};

/// `EntityID`.
pub type EntityId = u32;

/// `CellInfo` (`cell_info.hpp:32`): the manager's description of one cell of a
/// space — which app hosts it and the region it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct CellInfo {
    /// `spaceID_`.
    pub space_id: SpaceId,
    /// `addr_`: the CellApp that hosts this cell.
    pub app: Addr,
    /// `rect_`: the region this cell covers.
    pub rect: Rect,
    /// `load_`.
    pub load: f32,
    /// `hasBeenCreated_`.
    pub has_been_created: bool,
    /// `shouldDelete_`.
    pub should_delete: bool,
    /// `isDeletePending_`.
    pub delete_pending: bool,
}

impl CellInfo {
    /// A fresh cell description: unbounded region, no load, nothing created.
    pub fn new(space_id: SpaceId, app: Addr) -> Self {
        CellInfo {
            space_id,
            app,
            rect: Rect::unbounded(),
            load: 0.0,
            has_been_created: false,
            should_delete: false,
            delete_pending: false,
        }
    }

    /// `CellInfo::contains` (`cell_info.hpp:58`): the cell covers a position.
    pub fn contains(&self, x: f32, z: f32) -> bool {
        self.rect.contains_rect(&Rect::new(x, z, x, z))
    }
}

/// A CellApp's cell for one space (`Cell`, `cell.hpp`). The cell owns the real
/// entities of the space that run on this app.
#[derive(Debug, Clone)]
pub struct Cell {
    space_id: SpaceId,
    cell_info: CellInfo,
    /// `realEntities_` (`cell.hpp:39`): the entities this cell owns the real
    /// for.
    real_entities: BTreeSet<EntityId>,
    /// `shouldOffload_`.
    should_offload: bool,
    /// `lastERTFactor_`.
    last_ert_factor: f32,
    /// `isRetiring_`.
    is_retiring: bool,
    /// `isRemoved_`.
    is_removed: bool,
}

impl Cell {
    /// `Cell::Cell` (`cell.cpp:82`): a cell for one space, with nothing else
    /// running yet. The reference attaches itself to the space here
    /// (`space_.pCell(this)`); that registration lives in
    /// [`CellApp::add_cell`] here, since the CellApp owns both sides.
    pub fn new(space_id: SpaceId, cell_info: CellInfo) -> Self {
        Cell {
            space_id,
            cell_info,
            real_entities: BTreeSet::new(),
            should_offload: true,
            last_ert_factor: 1.0,
            is_retiring: false,
            is_removed: false,
        }
    }

    /// The space this cell is in.
    pub fn space_id(&self) -> SpaceId {
        self.space_id
    }

    /// The cell's description.
    pub fn cell_info(&self) -> &CellInfo {
        &self.cell_info
    }

    /// `cellInfo()` (`cell.hpp`), mutably.
    pub fn cell_info_mut(&mut self) -> &mut CellInfo {
        &mut self.cell_info
    }

    /// `Cell::spaceID`.
    pub fn contains(&self, x: f32, z: f32) -> bool {
        self.cell_info.contains(x, z)
    }

    /// `realEntities_` size (`Cells::numRealEntities` sums these).
    pub fn num_real_entities(&self) -> usize {
        self.real_entities.len()
    }

    /// `realEntities_` membership.
    pub fn has_real_entity(&self, id: EntityId) -> bool {
        self.real_entities.contains(&id)
    }

    /// `Cell::addRealEntity` (`cell.cpp`): claim the real for an entity.
    pub fn add_real_entity(&mut self, id: EntityId) -> bool {
        self.real_entities.insert(id)
    }

    /// `Cell::entityDestroyed` / `realEntities_` removal.
    pub fn remove_real_entity(&mut self, id: EntityId) -> bool {
        self.real_entities.remove(&id)
    }

    /// `shouldOffload_`.
    pub fn should_offload(&self) -> bool {
        self.should_offload
    }

    /// `shouldOffload( bool )`.
    pub fn set_should_offload(&mut self, value: bool) {
        self.should_offload = value;
    }

    /// `lastERTFactor_`.
    pub fn last_ert_factor(&self) -> f32 {
        self.last_ert_factor
    }

    /// `lastERTFactor( float )`.
    pub fn set_last_ert_factor(&mut self, value: f32) {
        self.last_ert_factor = value;
    }

    /// `isRetiring_` (`cell.cpp:745`).
    pub fn is_retiring(&self) -> bool {
        self.is_retiring
    }

    /// `Cell::retireCell` (`cell.cpp:630`): the manager set or cleared the
    /// retiring flag; the reference asserts the new value is the opposite of
    /// the old one.
    pub fn set_retiring(&mut self, value: bool) {
        self.is_retiring = value;
    }

    /// `isRemoved_` (`cell.cpp:661`).
    pub fn is_removed(&self) -> bool {
        self.is_removed
    }

    /// `Cell::removeCell` (`cell.cpp:661`): mark this cell for removal. The
    /// acknowledgement bookkeeping is not modelled here yet (BW-08-0x).
    pub fn mark_removed(&mut self) {
        self.is_removed = true;
    }

    /// `Cell::reuse` (`cell.cpp:745`): come back from retirement/removal.
    /// Returns whether the cell really was retiring and removed.
    pub fn reuse(&mut self) -> bool {
        let is_okay = self.is_retiring && self.is_removed;
        self.is_retiring = false;
        self.is_removed = false;
        is_okay
    }
}

/// `Cells` (`cells.hpp`): the CellApp's cells, at most one per space, keyed by
/// `SpaceID` (`Cells::Container`).
#[derive(Debug, Clone, Default)]
pub struct Cells {
    container: BTreeMap<SpaceId, Cell>,
}

impl Cells {
    /// An empty collection.
    pub fn new() -> Self {
        Cells::default()
    }

    /// `Cells::add` (`cells.hpp`): the reference asserts the space id is not
    /// already present; here a duplicate replaces nothing and returns the
    /// previous cell.
    pub fn add(&mut self, cell: Cell) -> Option<Cell> {
        self.container.insert(cell.space_id(), cell)
    }

    /// `Cells::destroy` (`cells.hpp`, via `CellApp::destroyCell` at
    /// `cellapp.cpp:1508`): drop the cell for a space.
    pub fn destroy(&mut self, space_id: SpaceId) -> Option<Cell> {
        self.container.remove(&space_id)
    }

    /// `CellApp::findCell` / `Cells::findFromSpaceID`.
    pub fn find_from_space_id(&self, space_id: SpaceId) -> Option<&Cell> {
        self.container.get(&space_id)
    }

    /// `Cells::findFromSpaceID`, mutably.
    pub fn find_mut_from_space_id(&mut self, space_id: SpaceId) -> Option<&mut Cell> {
        self.container.get_mut(&space_id)
    }

    /// `Cells::empty`.
    pub fn is_empty(&self) -> bool {
        self.container.is_empty()
    }

    /// `Cells::size`.
    pub fn len(&self) -> usize {
        self.container.len()
    }

    /// `Cells::numRealEntities`.
    pub fn num_real_entities(&self) -> usize {
        self.container.values().map(Cell::num_real_entities).sum()
    }

    /// The cells, by space id.
    pub fn iter(&self) -> impl Iterator<Item = (&SpaceId, &Cell)> {
        self.container.iter()
    }

    /// `Cells::destroyAll` (`cellapp.cpp:465`): drop every cell, returning the
    /// space ids that were destroyed.
    pub fn destroy_all(&mut self) -> Vec<SpaceId> {
        let ids = self.container.keys().copied().collect::<Vec<_>>();
        self.container.clear();
        ids
    }
}

/// The outcome of [`CellApp::add_cell`], mirroring the paths of
/// `CellApp::addCell` (`cellapp.cpp:1606`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddCellOutcome {
    /// A fresh cell was created and registered on the space.
    Created,
    /// The space still had a cell (`Cell did not fully remove; reusing`,
    /// `cellapp.cpp:1629`), so it was reused instead of created.
    ReusedCell,
    /// The cell description was for a different app than this one, which the
    /// reference reports with a `CRITICAL_MSG` (`cellapp.cpp:1641`).
    WrongApp,
}

/// The CellApp-side registry: the app's own address, its live spaces and its
/// cells, tied together so registering a cell updates both ends.
#[derive(Debug, Clone)]
pub struct CellApp {
    /// `interface_.address()`: this app's own address.
    app: Addr,
    /// Whether the delegated game supplies the spaces.
    delegated: bool,
    /// `pSpaces_`.
    spaces: Spaces,
    /// `cells_`.
    cells: Cells,
}

impl CellApp {
    /// A CellApp with no spaces and no cells.
    pub fn new(app: Addr, delegated: bool) -> Self {
        CellApp {
            app,
            delegated,
            spaces: Spaces::new(),
            cells: Cells::new(),
        }
    }

    /// This app's own address.
    pub fn app(&self) -> Addr {
        self.app
    }

    /// The live spaces.
    pub fn spaces(&self) -> &Spaces {
        &self.spaces
    }

    /// The cells.
    pub fn cells(&self) -> &Cells {
        &self.cells
    }

    /// `CellApp::findSpace`.
    pub fn find_space(&self, space_id: SpaceId) -> Option<&Space> {
        self.spaces.find(space_id)
    }

    /// `CellApp::findCell`: the cell for a space, if this app hosts one.
    pub fn find_cell(&self, space_id: SpaceId) -> Option<&Cell> {
        self.cells.find_from_space_id(space_id)
    }

    /// `CellApp::addCell` (`cellapp.cpp:1606`): create or reuse the space,
    /// then create and register the cell. The cell is registered on the space
    /// (`space_.pCell(this)`) and added to `cells_`.
    pub fn add_cell(&mut self, cell_info: CellInfo) -> AddCellOutcome {
        if cell_info.app != self.app {
            // CRITICAL_MSG: "Failed to add a cell for space %u".
            return AddCellOutcome::WrongApp;
        }

        let space_id = cell_info.space_id;

        // Reuse the space if it is already live, otherwise create it.
        if self.spaces.find(space_id).is_some() {
            // INFO_MSG "Re-using space".
            self.spaces.find_mut(space_id).expect("checked").reuse();
        } else {
            self.spaces
                .create(space_id, self.delegated)
                .expect("space id is not live");
        }

        // A cell that did not fully remove is reused, not recreated
        // (`cellapp.cpp:1627`, WARNING "Cell did not fully remove; reusing.").
        if self.spaces.find(space_id).and_then(Space::p_cell).is_some() {
            self.cells
                .find_mut_from_space_id(space_id)
                .expect("a cell is registered")
                .reuse();
            return AddCellOutcome::ReusedCell;
        }

        // Create the cell, register it on the space, and keep it.
        let cell = Cell::new(space_id, cell_info);
        self.spaces
            .find_mut(space_id)
            .expect("space was just created or found")
            .set_cell(Some(CellRef { app: self.app }));
        self.cells.add(cell);
        AddCellOutcome::Created
    }

    /// `CellApp::destroyCell` (`cellapp.cpp:1508`): drop the cell for a space
    /// and detach it from the space (`Space::~Space` / `space_.pCell(NULL)`).
    pub fn destroy_cell(&mut self, space_id: SpaceId) -> Option<Cell> {
        let cell = self.cells.destroy(space_id);
        if cell.is_some()
            && let Some(space) = self.spaces.find_mut(space_id)
        {
            space.set_cell(None);
        }
        cell
    }

    /// `CellApp::numRealEntities` (`cellapp.cpp:2776`): the reals this app
    /// owns across all its cells.
    pub fn num_real_entities(&self) -> usize {
        self.cells.num_real_entities()
    }

    /// The cells this app hosts, as `(space, app)` pairs, in the same shape as
    /// the manager's [`crate::cellappmgr::CellLeaf`] record.
    pub fn cell_applications(&self) -> Vec<(SpaceId, Addr)> {
        self.cells
            .iter()
            .map(|(space_id, cell)| (*space_id, cell.cell_info().app))
            .collect()
    }

    /// The cells, mutably, for callers that need to reach into a specific cell.
    pub fn cells_mut(&mut self) -> &mut Cells {
        &mut self.cells
    }

    /// Whether the manager's record of which app owns each space's cell
    /// matches this app's own record: the same `(space, app)` pairs. This is
    /// the BW-08-01-02 acceptance, "管理器和 CellApp 状态一致".
    pub fn agrees_with(&self, manager: &[(SpaceId, Addr)]) -> bool {
        let mut ours = self.cell_applications();
        let mut theirs: Vec<(SpaceId, Addr)> = manager
            .iter()
            .copied()
            .filter(|(_, app)| *app == self.app)
            .collect();
        ours.sort_unstable();
        theirs.sort_unstable();
        ours == theirs
    }
}

impl Drop for CellApp {
    /// `CellApp::~CellApp` (`cellapp.cpp:465`): destroy all the cells.
    fn drop(&mut self) {
        self.cells.destroy_all();
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

    /// Creating a cell registers it on the space and in the app's cells.
    #[test]
    fn creating_a_cell_registers_it_and_the_space() {
        let mut cell_app = CellApp::new(app_a(), false);
        let outcome = cell_app.add_cell(CellInfo::new(7, app_a()));

        assert_eq!(outcome, AddCellOutcome::Created);
        let cell = cell_app.find_cell(7).expect("cell is kept");
        assert_eq!(cell.space_id(), 7);
        assert_eq!(cell.cell_info().app, app_a());
        assert_eq!(
            cell_app.find_space(7).and_then(Space::p_cell),
            Some(&CellRef { app: app_a() }),
            "the space points at the cell that owns it"
        );
    }

    /// A second cell for the same space reuses the space (it is not re-created)
    /// and reuses the cell that has not fully removed.
    #[test]
    fn adding_the_same_space_reuses_the_existing_cell() {
        let mut cell_app = CellApp::new(app_a(), false);
        cell_app.add_cell(CellInfo::new(7, app_a()));

        // The cell is mid-removal; the manager asks us to add it again.
        let cell = cell_app.cells_mut().find_mut_from_space_id(7).unwrap();
        cell.set_retiring(true);
        cell.mark_removed();
        let outcome = cell_app.add_cell(CellInfo::new(7, app_a()));

        assert_eq!(outcome, AddCellOutcome::ReusedCell);
        assert_eq!(cell_app.cells().len(), 1, "no second cell was created");
        let cell = cell_app.find_cell(7).unwrap();
        assert!(!cell.is_removed(), "reuse brought the cell back");
        assert!(!cell.is_retiring());
    }

    /// A cell description for a different app is rejected, as the reference
    /// does with a `CRITICAL_MSG`.
    #[test]
    fn a_cell_for_another_app_is_rejected() {
        let mut cell_app = CellApp::new(app_a(), false);
        let outcome = cell_app.add_cell(CellInfo::new(7, app_b()));

        assert_eq!(outcome, AddCellOutcome::WrongApp);
        assert!(cell_app.cells().is_empty());
        assert!(cell_app.find_space(7).is_none(), "nothing was created");
    }

    /// Destroying a cell drops it from the app and detaches it from the space.
    #[test]
    fn destroying_a_cell_detaches_it_from_the_space() {
        let mut cell_app = CellApp::new(app_a(), false);
        cell_app.add_cell(CellInfo::new(7, app_a()));
        cell_app
            .cells_mut()
            .find_mut_from_space_id(7)
            .unwrap()
            .add_real_entity(11);

        let removed = cell_app.destroy_cell(7).expect("cell was live");
        assert_eq!(removed.space_id(), 7);
        assert!(cell_app.find_cell(7).is_none());
        assert!(cell_app.find_space(7).unwrap().p_cell().is_none());
        assert_eq!(cell_app.num_real_entities(), 0);
    }

    /// The cell owns the real entities that are added to it.
    #[test]
    fn a_cell_owns_its_real_entities() {
        let mut cell_app = CellApp::new(app_a(), false);
        cell_app.add_cell(CellInfo::new(7, app_a()));
        let cell = cell_app
            .cells_mut()
            .find_mut_from_space_id(7)
            .expect("cell was just added");

        assert!(cell.add_real_entity(11));
        assert!(cell.add_real_entity(12));
        assert!(!cell.add_real_entity(11), "duplicates are not re-added");
        assert_eq!(cell.num_real_entities(), 2);
        assert!(cell.has_real_entity(12));
        assert!(cell.remove_real_entity(12));
        assert_eq!(cell.num_real_entities(), 1);
        assert_eq!(cell_app.num_real_entities(), 1);
    }

    /// Reuse brings a retiring, removed cell back, and only then. It always
    /// clears both flags (`cell.cpp:751`).
    #[test]
    fn reuse_only_brings_back_a_retiring_removed_cell() {
        let mut cell = Cell::new(7, CellInfo::new(7, app_a()));
        assert!(!cell.reuse(), "a live cell is not reused");
        cell.set_retiring(true);
        assert!(!cell.reuse(), "retiring alone is not enough");
        assert!(!cell.is_retiring(), "reuse clears the flag either way");
        cell.set_retiring(true);
        cell.mark_removed();
        assert!(cell.reuse(), "retiring and removed can be reused");
        assert!(!cell.is_retiring());
        assert!(!cell.is_removed());
    }

    /// A cell's region decides which positions it covers.
    #[test]
    fn a_cell_covers_the_positions_in_its_region() {
        let mut info = CellInfo::new(7, app_a());
        info.rect = Rect::new(-10.0, -20.0, 10.0, 20.0);
        let cell = Cell::new(7, info);

        assert!(cell.contains(0.0, 0.0));
        assert!(cell.contains(10.0, 20.0), "edges are inclusive");
        assert!(!cell.contains(11.0, 0.0));
        assert!(!cell.contains(0.0, -21.0));
    }

    /// The BW-08-01-02 acceptance: after the manager records the cell for an
    /// app and the app registers it, the two records agree.
    #[test]
    fn manager_and_cell_app_agree_after_the_handshake() {
        let mut cell_app = CellApp::new(app_a(), false);
        cell_app.add_cell(CellInfo::new(7, app_a()));

        // The manager's record: space 7 is owned by app A, and a different
        // space 9 is owned by app B. The app only cares about its own.
        let manager_record = vec![(7u32, app_a()), (9u32, app_b())];
        assert!(cell_app.agrees_with(&manager_record));
    }

    /// The end-to-end acceptance: the manager records a cell on an app and
    /// queues an `AddCell`; the app registers it; the two records agree.
    #[test]
    fn manager_and_cell_app_agree_end_to_end() {
        use crate::cellappmgr::{CellAppMgr, CellAppRecord};

        // A manager with one app and one space, set up the way the manager's
        // own tests do.
        let mut mgr = CellAppMgr::new();
        mgr.set_base_app((0x7f000001, 100));
        mgr.set_db_app_alpha((0x7f000001, 200));
        mgr.allow_new_cell_apps = true;
        mgr.apps.add(CellAppRecord::new(app_a(), 1, 3000, 3000));
        let space_id = mgr
            .create_space(crate::cellappmgr::NULL_SPACE_ID, false, 0)
            .expect("a space is created");

        // The manager recorded that app A owns the space's cell and queued the
        // AddCell message.
        let manager_record: Vec<(SpaceId, Addr)> = mgr.spaces[&space_id]
            .cells()
            .iter()
            .map(|cell| (cell.space, cell.app))
            .collect();
        assert_eq!(manager_record, vec![(space_id, app_a())]);
        assert!(
            mgr.outbox
                .iter()
                .any(|m| matches!(m, crate::cellappmgr::Outbound::AddCell(a, s)
                if *a == app_a() && *s == space_id))
        );

        // The CellApp receives the AddCell and registers the cell.
        let mut cell_app = CellApp::new(app_a(), false);
        let outcome = cell_app.add_cell(CellInfo::new(space_id, app_a()));
        assert_eq!(outcome, AddCellOutcome::Created);

        // 管理器和 CellApp 状态一致.
        assert!(cell_app.agrees_with(&manager_record));
    }

    /// When the CellApp has no cell that the manager recorded, or has one the
    /// manager did not, the records disagree.
    #[test]
    fn manager_and_cell_app_disagree_on_a_mismatch() {
        let mut cell_app = CellApp::new(app_a(), false);
        cell_app.add_cell(CellInfo::new(7, app_a()));

        // The manager thinks space 7 belongs to app B.
        assert!(!cell_app.agrees_with(&[(7, app_b())]));
        // The manager has no record of space 7 at all.
        assert!(!cell_app.agrees_with(&[]));
    }

    /// Adding a second, different space works independently, and a cell can be
    /// created on a space that already exists.
    #[test]
    fn a_second_space_gets_its_own_cell() {
        let mut cell_app = CellApp::new(app_a(), false);
        cell_app.add_cell(CellInfo::new(7, app_a()));
        cell_app.add_cell(CellInfo::new(9, app_a()));

        assert_eq!(cell_app.cells().len(), 2);
        assert_eq!(cell_app.find_cell(7).unwrap().space_id(), 7);
        assert_eq!(cell_app.find_cell(9).unwrap().space_id(), 9);
        assert_eq!(
            cell_app.cell_applications(),
            vec![(7, app_a()), (9, app_a())]
        );
    }

    /// A space the app already has from before is reused rather than re-created
    /// when a cell is added to it.
    #[test]
    fn adding_a_cell_to_a_live_space_reuses_the_space() {
        let mut cell_app = CellApp::new(app_a(), false);
        // Pre-create the space and mark its tree.
        cell_app
            .spaces
            .create(7, false)
            .expect("space is new")
            .set_cell_info_tree(true);

        let outcome = cell_app.add_cell(CellInfo::new(7, app_a()));

        assert_eq!(outcome, AddCellOutcome::Created);
        assert_eq!(cell_app.spaces().len(), 1, "no second space was created");
        assert!(!cell_app.find_space(7).unwrap().has_cell_info_tree());
        assert!(cell_app.find_space(7).unwrap().p_cell().is_some());
    }

    /// The delegated flavour is carried into the spaces the app creates.
    #[test]
    fn a_delegated_app_makes_delegated_spaces() {
        let mut cell_app = CellApp::new(app_a(), true);
        cell_app.add_cell(CellInfo::new(7, app_a()));
        let space = cell_app.find_space(7).unwrap();
        assert!(space.physical_space().is_delegated());
    }

    /// Destroying all the cells empties the app; dropping the app does the
    /// same (the `Drop` impl calls `destroyAll`).
    #[test]
    fn destroying_all_the_cells_empties_the_app() {
        let mut cell_app = CellApp::new(app_a(), false);
        cell_app.add_cell(CellInfo::new(7, app_a()));
        cell_app.add_cell(CellInfo::new(9, app_a()));

        assert_eq!(cell_app.cells_mut().destroy_all().len(), 2);
        assert!(cell_app.cells().is_empty());
    }
}
