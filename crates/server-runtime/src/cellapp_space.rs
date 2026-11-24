//! The CellApp-side Space lifecycle (BW-08-01-01).
//!
//! Reference face: `server/cellapp/space.cpp` — `Space( SpaceID )`,
//! `addEntity`/`removeEntity` (the removal-handle swap-with-back, so an
//! `addEntity` that a removal trigger sets off stays correct),
//! `checkForShutDown`, `requestShutDown`, `shutDownSpace`'s one-shot arming
//! of the recurring `ShutdownSpace` timer, and `handleTimeout`'s
//! `onSpaceGone` + `destroyCell`.
//!
//! The manager half of the round trip already lives in
//! [`crate::cellappmgr::CellAppMgr::shut_down_space`]; this module is the
//! Space that sends the request when it empties and receives the message
//! back, so an empty Space is reclaimed: emptied, one timer-width later,
//! cell gone. The timer is the reference's recurring one-second timer —
//! armed once, cancelled only by the space's destruction — and it is
//! driven on an injected microsecond clock ([`Space::tick`]) instead of a
//! live dispatcher pass, which keeps the one-second delay observable.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::cellappmgr::SpaceId;

/// `addTimer( 1000000, ... )` in `Space::shutDownSpace`.
pub const SHUTDOWN_SPACE_DELAY_US: u64 = 1_000_000;

/// The timer's name, as the reference registers it.
pub const SHUTDOWN_SPACE_TIMER: &str = "ShutdownSpace";

/// `NO_SPACE_REMOVAL_HANDLE`: an entity no space holds.
pub const NO_SPACE_REMOVAL_HANDLE: usize = usize::MAX;

/// One cell of a Space (`CellInfo` in the space's tree).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellInfo {
    /// The space this cell partitions.
    pub space_id: SpaceId,
    /// The CellApp this cell is assigned to (`CellApp::interface().address()`).
    pub app_addr: u32,
    /// The cell's tree identity.
    pub node: CellInfoNode,
}

/// The identity a `Cell`'s info holds; [`Space::has_single_cell`] compares
/// it against the space's tree root, exactly as
/// `((SpaceNode*)&pCell_->cellInfo() == pCellInfoTree_)` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellInfoNode(usize);

impl CellInfoNode {
    /// The next tree identity.
    pub fn next(value: CellInfoNode) -> Self {
        CellInfoNode(value.0 + 1)
    }
}

/// `Cell`: the CellApp's own cell for a Space, the one holding the space's
/// real entities.
#[derive(Clone, Debug)]
pub struct Cell {
    /// `cellInfo()`.
    pub info: CellInfo,
    /// How many `onSpaceGone` deliveries this cell has had.
    pub space_gone_count: u32,
    /// How many `Cell::reuse` deliveries this cell has had ("Cell did not
    /// fully remove; reusing.").
    pub reuse_count: u32,
}

impl Cell {
    /// `Cell::reuse`: the cell did not fully remove before the manager
    /// re-added it; the same cell object carries on.
    pub fn reuse(&mut self) {
        self.reuse_count += 1;
    }
}

/// `Entity`: the space's own entry for one of its entities — the removal
/// handle bookkeeping and the real/ghost flag the destructor turns on.
#[derive(Clone, Debug)]
pub struct Entity {
    /// The entity id.
    pub id: u32,
    /// `removalHandle_`: the index this entity sits at in its space's
    /// entity list, or [`NO_SPACE_REMOVAL_HANDLE`] when none holds it.
    pub removal_handle: usize,
    /// `isReal()`.
    pub is_real: bool,
    /// `isDestroyed()`.
    pub destroyed: bool,
}

impl Entity {
    /// An entity no space holds yet, with the handle reset.
    pub fn new(id: u32, is_real: bool) -> Self {
        Self {
            id,
            removal_handle: NO_SPACE_REMOVAL_HANDLE,
            is_real,
            destroyed: false,
        }
    }
}

/// `Space` (`space.cpp`): one space of the world as the CellApp holds it.
#[derive(Debug)]
pub struct Space {
    /// `id_`.
    pub id: SpaceId,
    /// `pCell_`: the cell holding this space's real entities.
    cell: Option<Rc<RefCell<Cell>>>,
    /// `cellInfos_`: every cell this space spans, tree order.
    cells: Vec<CellInfo>,
    /// `entities_`.
    entities: Vec<Rc<RefCell<Entity>>>,
    /// When the `ShutdownSpace` timer was armed, or `None` while it is
    /// unset — `shuttingDownTimerHandle_.isSet()`.
    timer_armed_at: Option<u64>,
    /// `begDataSeq_`.
    pub beg_data_seq: u64,
    /// `endDataSeq_`.
    pub end_data_seq: u64,
    /// `initialTimeOfDay_`, the reference's `-1.f` unset sentinel.
    pub initial_time_of_day: f32,
    /// `gameSecondsPerSecond_`.
    pub game_seconds_per_second: f32,
    /// The entry count of `spaceDataMapping_`.
    pub space_data_entries: usize,
}

impl Space {
    /// `Space( SpaceID id )`: no cell, no entities, no timer, and the data
    /// faces at the constructor's own defaults.
    pub fn new(id: SpaceId) -> Self {
        Self {
            id,
            cell: None,
            cells: Vec::new(),
            entities: Vec::new(),
            timer_armed_at: None,
            beg_data_seq: 0,
            end_data_seq: 0,
            initial_time_of_day: -1.0,
            game_seconds_per_second: 0.0,
            space_data_entries: 0,
        }
    }

    /// `Space::reuse`: the space data faces reset and the cell-info tree
    /// is deleted — the cell and the entities stay where they are.
    pub fn reuse(&mut self) {
        self.beg_data_seq = 0;
        self.end_data_seq = 0;
        self.initial_time_of_day = -1.0;
        self.game_seconds_per_second = 0.0;
        self.cells.clear();
        self.space_data_entries = 0;
    }

    /// `updateGeometry`: the manager's tree becomes this space's cells.
    pub fn update_geometry(&mut self, cells: Vec<CellInfo>) {
        self.cells = cells;
    }

    /// `findCell( address )`: the tree's cell assigned to this CellApp,
    /// if any.
    pub fn find_cell_info(&self, app_addr: u32) -> Option<CellInfo> {
        self.cells
            .iter()
            .find(|info| info.app_addr == app_addr)
            .cloned()
    }

    /// `allSpaceData`: the manager's full space-data payload replaces
    /// whatever the space held. An empty payload leaves it untouched (the
    /// reference only reads it when `remainingLength() > 0`).
    pub fn receive_all_space_data(&mut self, count: usize) {
        if count > 0 {
            self.space_data_entries = count;
        }
    }

    /// `numEntities`.
    pub fn num_entities(&self) -> usize {
        self.entities.len()
    }

    /// `numCells`.
    pub fn num_cells(&self) -> usize {
        self.cells.len()
    }

    /// The space's entities.
    pub fn entities(&self) -> &[Rc<RefCell<Entity>>] {
        &self.entities
    }

    /// `pCell()`.
    pub fn cell(&self) -> Option<&Rc<RefCell<Cell>>> {
        self.cell.as_ref()
    }

    /// `pCell( Cell * )`: install the cell — nothing else; the cell-info
    /// tree is the manager's, delivered by [`Space::update_geometry`].
    pub fn set_cell(&mut self, cell: Rc<RefCell<Cell>>) {
        self.cell = Some(cell);
    }

    /// `cellInfos_` gaining another cell: a multi-cell space never
    /// self-reclaims.
    pub fn add_cell(&mut self, info: CellInfo) {
        self.cells.push(info);
    }

    /// `hasSingleCell`: `pCell_ && ((SpaceNode*)&pCell_->cellInfo() ==
    /// pCellInfoTree_)` — the space's one cell is the tree root.
    pub fn has_single_cell(&self) -> bool {
        match &self.cell {
            Some(cell) => self.cells.len() == 1 && self.cells[0].node == cell.borrow().info.node,
            None => false,
        }
    }

    /// `isShuttingDown`: the timer has been armed and not cancelled.
    pub fn is_shutting_down(&self) -> bool {
        self.timer_armed_at.is_some()
    }

    /// `addEntity( Entity * )`.
    pub fn add_entity(&mut self, entity: Rc<RefCell<Entity>>) {
        assert_eq!(
            entity.borrow().removal_handle,
            NO_SPACE_REMOVAL_HANDLE,
            "addEntity on an entity a space still holds"
        );
        let handle = self.entities.len();
        entity.borrow_mut().removal_handle = handle;
        self.entities.push(entity);
    }

    /// `removeEntity( Entity * )`: the swap-with-back — the back element's
    /// removal handle moves onto the vacated slot, the entity's own handle
    /// resets, and only then does the shutdown check run (the reference's
    /// comment: the removal can set off triggers that add or remove).
    pub fn remove_entity(&mut self, entity: &Rc<RefCell<Entity>>, cell_app: &mut CellApp) {
        let handle = entity.borrow().removal_handle;
        assert!(
            handle < self.entities.len() && self.entities[handle].borrow().id == entity.borrow().id,
            "removal handle does not name this entity"
        );

        let back = self.entities.last().cloned().expect("handle in range");
        back.borrow_mut().removal_handle = handle;
        self.entities[handle] = back;
        entity.borrow_mut().removal_handle = NO_SPACE_REMOVAL_HANDLE;
        self.entities.pop();

        self.check_for_shut_down(cell_app);
    }

    /// `checkForShutDown`: an emptied single-cell space asks for its
    /// shutdown, with the reference's own refusals — a multi-cell space, a
    /// space already shutting down, a CellApp that has not started, a
    /// dispatcher mid break, and the default space.
    pub fn check_for_shut_down(&mut self, cell_app: &mut CellApp) -> bool {
        let eligible = self.has_single_cell()
            && self.entities.is_empty()
            && cell_app.has_started()
            && !self.is_shutting_down()
            && !cell_app.processing_broken()
            && !(cell_app.use_default_space && self.id == 1);
        if eligible {
            self.request_shut_down(cell_app);
        }
        eligible
    }

    /// `requestShutDown`: ask the CellAppMgr, which routes the message
    /// back to the hosting CellApp. The reference still sends for the
    /// default space (it errors about it first).
    pub fn request_shut_down(&mut self, cell_app: &mut CellApp) -> SpaceId {
        let id = self.id;
        cell_app.shut_down_space(id);
        id
    }

    /// `shutDownSpace( BinaryIStream & )` arriving at `now_us`: the
    /// CellAppMgr's message, arming the recurring timer once. A second
    /// message finds it already set and does nothing.
    pub fn shut_down_space(&mut self, now_us: u64) -> bool {
        if self.timer_armed_at.is_none() {
            self.timer_armed_at = Some(now_us);
            true
        } else {
            false
        }
    }

    /// Advance the space's timer clock to `now_us`, firing the armed
    /// `ShutdownSpace` timer once for each delay width that has elapsed —
    /// the reference's `handleTimeout` on its recurring timer.
    pub fn tick(&mut self, now_us: u64, cell_app: &mut CellApp) -> bool {
        let mut fired = false;
        while let Some(armed_at) = self.timer_armed_at {
            if now_us < armed_at + SHUTDOWN_SPACE_DELAY_US {
                break;
            }
            self.timer_armed_at = Some(armed_at + SHUTDOWN_SPACE_DELAY_US);
            self.handle_timeout(cell_app);
            fired = true;
        }
        fired
    }

    /// `handleTimeout`: tell the cell the space is gone, and destroy the
    /// cell when this is a single-cell space with nothing left in it.
    /// Otherwise — multi-cell space, entities back, or cell already gone —
    /// the recurring timer simply re-checks on its next width ("deletion
    /// will occur once all chunks have been unloaded").
    fn handle_timeout(&mut self, cell_app: &mut CellApp) {
        let cell = match self.cell.clone() {
            Some(cell) => cell,
            None => return,
        };
        cell.borrow_mut().space_gone_count += 1;
        if self.has_single_cell() && self.entities.is_empty() {
            let node = cell.borrow().info.node;
            cell_app.destroy_cell(&cell);
            self.cell = None;
            self.cells.retain(|info| info.node != node);
        }
    }

    /// The `~Space` face: a live ghost is destroyed, a live real entity is
    /// warned about and kept, and the shutdown timer is cancelled.
    pub fn reclaim_entities(&mut self) -> EntityReclaim {
        let mut reclaimed = EntityReclaim::default();
        let mut kept = Vec::with_capacity(self.entities.len());
        for entity in self.entities.drain(..) {
            let (is_real, destroyed, id) = {
                let borrowed = entity.borrow();
                (borrowed.is_real, borrowed.destroyed, borrowed.id)
            };
            if !is_real && !destroyed {
                entity.borrow_mut().destroyed = true;
                reclaimed.destroyed_ghosts.push(id);
            } else if is_real && !destroyed {
                reclaimed.warning_real.push(id);
                kept.push(entity);
            }
        }
        self.entities = kept;
        self.timer_armed_at = None;
        reclaimed
    }
}

/// What [`Space::reclaim_entities`] did — the `~Space` bookkeeping made
/// observable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityReclaim {
    /// The ghosts the space destroyed.
    pub destroyed_ghosts: Vec<u32>,
    /// The live real entities it warned about instead of destroying.
    pub warning_real: Vec<u32>,
}

/// The `CellApp::addCell` message's payload, decoded from the manager.
#[derive(Clone, Debug)]
pub struct AddCellRequest {
    /// The space the manager is adding a cell of.
    pub space_id: SpaceId,
    /// The `updateGeometry` payload: the space's tree cells.
    pub cells: Vec<CellInfo>,
    /// `isFirstCell`.
    pub is_first_cell: bool,
    /// `isFromDB`.
    pub is_from_db: bool,
    /// The `allSpaceData` entry count; zero means the message carried no
    /// space data at all (`remainingLength() == 0`).
    pub space_data_count: usize,
}

/// What `CellApp::addCell` did — the reference's branch matrix made
/// observable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddCellOutcome {
    /// An existing space was found and re-used ("Re-using space %d").
    pub space_reused: bool,
    /// An undestroyed cell was re-used ("Cell did not fully remove;
    /// reusing.").
    pub cell_reused: bool,
    /// A new cell was built and added to `cells_`.
    pub cell_registered: bool,
    /// `onGetFirstCell( isFromDB )` fired — the first cell of the default
    /// space.
    pub first_cell_delivered: bool,
    /// The CRITICAL path: no cell of this space belongs to this CellApp.
    pub no_cell_here: bool,
}

/// The manager's own ledger of what it created and assigned — the side
/// [`CellApp::add_cell`]'s delivery must agree with.
#[derive(Clone, Debug, Default)]
pub struct MgrLedger {
    /// The spaces the manager created.
    pub spaces: Vec<SpaceId>,
    /// The cells it assigned: `(space, CellApp address, tree node)`.
    pub assignments: Vec<(SpaceId, u32, CellInfoNode)>,
}

impl MgrLedger {
    /// The manager and this CellApp agree about what they hold each other
    /// to: every space the ledger created exists here, every assignment
    /// addressed to this CellApp is registered here or already destroyed,
    /// and this CellApp holds no cell the ledger never assigned to it.
    pub fn agrees_with(&self, app: &CellApp, spaces: &BTreeMap<SpaceId, Space>) -> bool {
        if !self.spaces.iter().all(|id| spaces.contains_key(id)) {
            return false;
        }
        for (id, addr, node) in &self.assignments {
            if *addr != app.address {
                continue;
            }
            let registered = spaces
                .get(id)
                .and_then(|space| space.cell().cloned())
                .map(|cell| cell.borrow().info.node == *node)
                .unwrap_or(false);
            if !registered && !app.destroyed_cell_nodes.contains(node) {
                return false;
            }
        }
        app.cells.iter().all(|cell| {
            let borrowed = cell.borrow();
            self.assignments.iter().any(|(space_id, addr, node)| {
                *addr == app.address
                    && *node == borrowed.info.node
                    && *space_id == borrowed.info.space_id
            })
        })
    }
}

/// The CellApp state the shutdown decision reads (`CellApp::instance()` +
/// its dispatcher + config), recording what the Space said to it.
#[derive(Debug, Default)]
pub struct CellApp {
    /// `CellApp::instance().hasStarted()`.
    started: bool,
    /// `CellAppConfig::useDefaultSpace()`.
    use_default_space: bool,
    /// `mainDispatcher().processingBroken()`.
    broken: bool,
    /// `interface_.address()`: who this CellApp is on the internal network.
    pub address: u32,
    /// `cells_`: every cell this CellApp holds, in registration order.
    pub cells: Vec<Rc<RefCell<Cell>>>,
    /// The `shutDownSpace` requests the spaces sent (the CellAppMgr half
    /// of the round trip).
    pub shut_down_requests: Vec<SpaceId>,
    /// The `destroyCell` calls, by cell's tree identity.
    pub destroyed_cell_nodes: Vec<CellInfoNode>,
    /// The `onGetFirstCell( isFromDB )` deliveries.
    pub first_cell_deliveries: Vec<bool>,
    /// The CRITICAL `addCell` deliveries: no cell of that space is ours.
    pub failed_adds: Vec<SpaceId>,
    /// The `Spaces::create` calls.
    pub created_spaces: Vec<SpaceId>,
}

impl CellApp {
    /// A fresh CellApp: not started, nothing broken.
    pub fn new() -> Self {
        Self::default()
    }

    /// `CellApp::instance().hasStarted()` turning true.
    pub fn start(&mut self) {
        self.started = true;
    }

    /// `hasStarted`.
    pub fn has_started(&self) -> bool {
        self.started
    }

    /// `CellAppConfig::useDefaultSpace`.
    pub fn set_use_default_space(&mut self, on: bool) {
        self.use_default_space = on;
    }

    pub fn use_default_space(&self) -> bool {
        self.use_default_space
    }

    /// `mainDispatcher().processingBroken`.
    pub fn set_processing_broken(&mut self, broken: bool) {
        self.broken = broken;
    }

    pub fn processing_broken(&self) -> bool {
        self.broken
    }

    /// `CellAppMgr::shutDownSpace` receiving the Space's request.
    fn shut_down_space(&mut self, space_id: SpaceId) {
        self.shut_down_requests.push(space_id);
    }

    /// A fresh CellApp at `address`: not started, nothing broken, no cells.
    pub fn with_address(address: u32) -> Self {
        Self {
            address,
            ..Self::default()
        }
    }

    /// `CellApp::destroyCell`: the cell goes away; the space clears its
    /// own `pCell_` (the reference's destructed cell does it) and the
    /// `cells_` registry forgets it.
    pub fn destroy_cell(&mut self, cell: &Rc<RefCell<Cell>>) {
        let node = cell.borrow().info.node;
        self.destroyed_cell_nodes.push(node);
        self.cells.retain(|held| held.borrow().info.node != node);
    }

    /// `CellApp::findSpace`.
    pub fn find_space<'a>(
        &self,
        spaces: &'a BTreeMap<SpaceId, Space>,
        id: SpaceId,
    ) -> Option<&'a Space> {
        spaces.get(&id)
    }

    /// `CellApp::findCell( SpaceID )`: the space's own cell.
    pub fn find_cell(
        &self,
        spaces: &BTreeMap<SpaceId, Space>,
        id: SpaceId,
    ) -> Option<Rc<RefCell<Cell>>> {
        spaces.get(&id).and_then(|space| space.cell().cloned())
    }

    /// `CellApp::addCell`: the manager's message adding a cell of
    /// `space_id` to this CellApp — find or create the space, re-use it,
    /// read the tree, then find, re-use or build the cell that is ours,
    /// and finally carry the space data and the first-cell event.
    pub fn add_cell(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        request: AddCellRequest,
    ) -> AddCellOutcome {
        let mut outcome = AddCellOutcome::default();
        let space = match spaces.get_mut(&request.space_id) {
            Some(space) => {
                outcome.space_reused = true;
                space.reuse();
                space
            }
            None => {
                self.created_spaces.push(request.space_id);
                spaces
                    .entry(request.space_id)
                    .or_insert_with(|| Space::new(request.space_id))
            }
        };

        space.update_geometry(request.cells);
        let info = match space.find_cell_info(self.address) {
            Some(info) => info,
            None => {
                outcome.no_cell_here = true;
                self.failed_adds.push(request.space_id);
                return outcome;
            }
        };

        let existing = space.cell().cloned();
        match existing {
            Some(cell) => {
                assert_eq!(
                    info.node,
                    cell.borrow().info.node,
                    "the re-added cell is not the one that did not fully remove"
                );
                cell.borrow_mut().reuse();
                outcome.cell_reused = true;
            }
            None => {
                let cell = Rc::new(RefCell::new(Cell {
                    info: info.clone(),
                    space_gone_count: 0,
                    reuse_count: 0,
                }));
                space.set_cell(cell.clone());
                self.cells.push(cell);
                outcome.cell_registered = true;
            }
        }

        space.receive_all_space_data(request.space_data_count);

        // The first cell in the first space.
        if request.is_first_cell && self.use_default_space && request.space_id == 1 {
            self.first_cell_deliveries.push(request.is_from_db);
            outcome.first_cell_delivered = true;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test CellApp's own address on the internal network.
    const APP_ADDR: u32 = 10;

    fn entity(id: u32, real: bool) -> Rc<RefCell<Entity>> {
        Rc::new(RefCell::new(Entity::new(id, real)))
    }

    fn cell_info(space_id: SpaceId, node: usize) -> CellInfo {
        CellInfo {
            space_id,
            app_addr: APP_ADDR,
            node: CellInfoNode(node),
        }
    }

    /// A started CellApp, a single-cell space, and the cell it holds.
    fn fixture(id: SpaceId, node: usize) -> (CellApp, Space, Rc<RefCell<Cell>>) {
        let mut cell_app = CellApp::with_address(APP_ADDR);
        cell_app.start();
        let cell = Rc::new(RefCell::new(Cell {
            info: cell_info(id, node),
            space_gone_count: 0,
            reuse_count: 0,
        }));
        let mut space = Space::new(id);
        space.update_geometry(vec![cell_info(id, node)]);
        space.set_cell(cell.clone());
        (cell_app, space, cell)
    }

    /// A Space with no cell is never a single-cell space, and a timerless
    /// Space never ticks.
    #[test]
    fn a_cellless_space_never_reclaims() {
        let mut app = CellApp::new();
        app.start();
        let mut bare = Space::new(9);
        assert!(!bare.has_single_cell(), "no cell, no single cell");
        assert!(!bare.is_shutting_down());
        assert!(!bare.tick(10_000_000, &mut app), "no armed timer, no fire");
    }

    /// The acceptance face: an emptied single-cell Space asks for its
    /// shutdown, and one timer-width later the cell is gone — then the
    /// recurring timer keeps re-checking without destroying anything
    /// further.
    #[test]
    fn an_emptied_space_is_reclaimed() {
        let (mut app, mut space, cell) = fixture(7, 0);
        space.add_entity(entity(10, true));
        space.add_entity(entity(11, true));
        assert_eq!(space.num_entities(), 2);
        assert_eq!(space.num_cells(), 1);
        assert!(!space.is_shutting_down());

        // The first entity leaves; the space is not empty yet.
        space.remove_entity(&space.entities()[0].clone(), &mut app);
        assert!(!space.is_shutting_down());
        assert!(app.shut_down_requests.is_empty());

        // The last entity leaves: the space is now empty and asks the
        // manager for its shutdown — the request is not yet the timer, the
        // message still has to come back.
        let last = space.entities()[0].clone();
        space.remove_entity(&last, &mut app);
        assert!(!space.is_shutting_down(), "a request is not a timer");
        assert_eq!(app.shut_down_requests, vec![7u32]);

        // The manager's message comes back and arms the timer at t=0;
        // before the delay elapses nothing happens.
        assert!(space.shut_down_space(0));
        assert!(space.is_shutting_down());
        assert!(!space.tick(999_999, &mut app));
        assert!(space.cell().is_some());

        // At the delay the cell is told and destroyed, and the space
        // loses it.
        assert!(space.tick(1_000_000, &mut app));
        assert!(space.cell().is_none());
        assert_eq!(app.destroyed_cell_nodes, vec![CellInfoNode(0)]);
        assert_eq!(cell.borrow().space_gone_count, 1);
        assert_eq!(space.num_cells(), 0);
        assert!(space.is_shutting_down(), "a recurring timer stays armed");

        // Each further width re-checks and finds nothing left to do.
        assert!(!space.tick(1_500_000, &mut app));
        assert!(space.tick(2_000_000, &mut app));
        assert_eq!(cell.borrow().space_gone_count, 1, "no cell, no delivery");
        assert_eq!(
            app.destroyed_cell_nodes.len(),
            1,
            "nothing further destroyed"
        );
    }

    /// The removal handle is a swap-with-back: survivors keep the right
    /// index, the removed entity is out of every list, and the entity's
    /// own handle is reset.
    #[test]
    fn the_removal_handle_swaps_with_the_back() {
        let (mut app, mut space, _) = fixture(9, 0);
        let entities: Vec<_> = (1..=3).map(|id| entity(id, true)).collect();
        for entity in &entities {
            space.add_entity(entity.clone());
        }
        assert_eq!(
            entities
                .iter()
                .map(|e| e.borrow().removal_handle)
                .collect::<Vec<_>>(),
            vec![0usize, 1, 2]
        );

        // Removing the middle one moves the back element into its slot.
        space.remove_entity(&entities[1], &mut app);
        assert_eq!(entities[0].borrow().removal_handle, 0);
        assert_eq!(entities[1].borrow().removal_handle, NO_SPACE_REMOVAL_HANDLE);
        assert_eq!(entities[2].borrow().removal_handle, 1);
        let ids: Vec<_> = space.entities().iter().map(|e| e.borrow().id).collect();
        assert_eq!(ids, vec![1u32, 3]);
    }

    /// Every refusal the reference has: a multi-cell space, an unstarted
    /// CellApp, a broken dispatcher, and the default space. The direct
    /// `requestShutDown` still goes out for the default space — the
    /// reference errors about it and sends anyway.
    #[test]
    fn the_shutdown_gate_refuses_like_the_reference() {
        // A multi-cell space never self-reclaims, even emptied.
        let (mut app, mut multi, _) = fixture(2, 0);
        assert!(!app.use_default_space());
        multi.add_cell(CellInfo {
            space_id: 2,
            app_addr: APP_ADDR,
            node: CellInfoNode::next(CellInfoNode(0)),
        });
        let e = entity(1, true);
        multi.add_entity(e.clone());
        multi.remove_entity(&e, &mut app);
        assert!(!multi.is_shutting_down());
        assert!(app.shut_down_requests.is_empty());

        // An unstarted CellApp holds every space back.
        let mut unstarted_app = CellApp::new();
        let mut not_started = Space::new(3);
        let cell = Rc::new(RefCell::new(Cell {
            info: cell_info(3, 1),
            space_gone_count: 0,
            reuse_count: 0,
        }));
        not_started.update_geometry(vec![cell_info(3, 1)]);
        not_started.set_cell(cell);
        let e = entity(2, true);
        not_started.add_entity(e.clone());
        not_started.remove_entity(&e, &mut unstarted_app);
        assert!(!not_started.is_shutting_down());
        assert!(unstarted_app.shut_down_requests.is_empty());

        // A dispatcher mid break does not reclaim either.
        let (mut broken_app, mut broken, _) = fixture(4, 2);
        broken_app.set_processing_broken(true);
        let e = entity(3, true);
        broken.add_entity(e.clone());
        broken.remove_entity(&e, &mut broken_app);
        assert!(!broken.is_shutting_down());
        assert!(broken_app.shut_down_requests.is_empty());

        // The default space refuses through the gate...
        let (mut default_app, mut default_space, _) = fixture(1, 3);
        default_app.set_use_default_space(true);
        assert!(default_app.use_default_space());
        let e = entity(4, true);
        default_space.add_entity(e.clone());
        assert!(
            !default_space.check_for_shut_down(&mut default_app),
            "the default space reclaimed"
        );
        assert!(default_app.shut_down_requests.is_empty());
        // ...but a direct requestShutDown still goes out.
        assert_eq!(default_space.request_shut_down(&mut default_app), 1);
        assert_eq!(default_app.shut_down_requests, vec![1u32]);
    }

    /// A second shutdown message finds the timer already armed and does
    /// not re-arm it: the delay stays anchored to the first arming.
    #[test]
    fn a_second_shutdown_message_does_not_re_arm() {
        let (mut app, mut space, _) = fixture(3, 0);
        let lonely = entity(8, true);
        space.add_entity(lonely.clone());
        space.remove_entity(&lonely, &mut app);
        assert_eq!(app.shut_down_requests, vec![3u32]);

        assert!(space.shut_down_space(500_000), "first message arms");
        assert!(!space.shut_down_space(600_000), "second finds it armed");
        assert!(!space.tick(1_499_999, &mut app));
        assert!(
            space.tick(1_500_000, &mut app),
            "fires one width after arming"
        );
        assert!(space.is_shutting_down(), "the recurring timer stays armed");
    }

    /// The timer on a multi-cell space delivers `onSpaceGone` every width
    /// but keeps the cell — deletion waits for the whole space.
    #[test]
    fn a_multi_cell_space_keeps_its_cell() {
        let (mut app, mut space, cell) = fixture(5, 0);
        space.add_cell(CellInfo {
            space_id: 5,
            app_addr: APP_ADDR,
            node: CellInfoNode::next(CellInfoNode(0)),
        });
        assert!(space.shut_down_space(0));
        assert!(space.tick(1_000_000, &mut app));
        assert_eq!(cell.borrow().space_gone_count, 1);
        assert!(space.cell().is_some(), "multi-cell space keeps its cell");
        assert!(!space.has_single_cell(), "two cells left");

        // Entities arriving back hold the destruction off too.
        let (mut app2, mut repopulated, cell2) = fixture(6, 0);
        repopulated.shut_down_space(0);
        let back = entity(9, true);
        repopulated.add_entity(back.clone());
        assert!(repopulated.tick(1_000_000, &mut app2));
        assert_eq!(cell2.borrow().space_gone_count, 1);
        assert!(
            repopulated.cell().is_some(),
            "an occupied single-cell space keeps its cell"
        );
        assert_eq!(app2.destroyed_cell_nodes, Vec::<CellInfoNode>::new());
    }

    /// The `~Space` face: ghosts are destroyed, live real entities are
    /// warned about and kept, and the timer is cancelled.
    #[test]
    fn the_space_destructor_reclaims_its_ghosts() {
        let (mut app, mut space, _) = fixture(6, 0);
        let ghost = entity(100, false);
        let real = entity(101, true);
        // A ghost already destroyed is left where it is: nothing to do.
        let gone = entity(99, false);
        gone.borrow_mut().destroyed = true;
        space.add_entity(gone);
        space.add_entity(ghost.clone());
        space.add_entity(real.clone());
        space.shut_down_space(0);

        let reclaimed = space.reclaim_entities();
        assert_eq!(
            reclaimed,
            EntityReclaim {
                destroyed_ghosts: vec![100],
                warning_real: vec![101],
            }
        );
        assert!(ghost.borrow().destroyed);
        assert!(!real.borrow().destroyed);
        assert_eq!(space.num_entities(), 1);
        assert!(!space.is_shutting_down(), "destruction cancels the timer");
        assert!(
            !space.tick(10_000_000, &mut app),
            "a cancelled timer stays dead"
        );

        // An already-empty space reclaims nothing and still cancels.
        let (mut app2, mut empty, _) = fixture(7, 0);
        empty.shut_down_space(0);
        assert_eq!(empty.reclaim_entities(), EntityReclaim::default());
        assert!(!empty.is_shutting_down());
        assert!(
            !empty.tick(10_000_000, &mut app2),
            "and it never fires again"
        );
    }

    /// The acceptance face: the manager's `addCell` delivery leaves the
    /// manager and each CellApp agreeing — the space exists where the
    /// manager created it, and each assigned cell is registered on its own
    /// CellApp.
    #[test]
    fn add_cell_registers_the_cells_the_manager_assigned() {
        let mgr = MgrLedger {
            spaces: vec![5],
            assignments: vec![(5, 10, CellInfoNode(0)), (5, 11, CellInfoNode(1))],
        };
        let tree = vec![
            CellInfo {
                space_id: 5,
                app_addr: 10,
                node: CellInfoNode(0),
            },
            CellInfo {
                space_id: 5,
                app_addr: 11,
                node: CellInfoNode(1),
            },
        ];

        let mut spaces10 = BTreeMap::new();
        let mut app10 = CellApp::with_address(10);
        app10.start();
        let outcome = app10.add_cell(
            &mut spaces10,
            AddCellRequest {
                space_id: 5,
                cells: tree.clone(),
                is_first_cell: false,
                is_from_db: false,
                space_data_count: 2,
            },
        );
        assert_eq!(
            outcome,
            AddCellOutcome {
                cell_registered: true,
                ..Default::default()
            }
        );
        assert_eq!(app10.created_spaces, vec![5u32]);
        assert_eq!(app10.cells.len(), 1);
        assert_eq!(app10.cells[0].borrow().info.node, CellInfoNode(0));
        let space = app10.find_space(&spaces10, 5).expect("space");
        assert_eq!(space.num_cells(), 2);
        assert_eq!(space.space_data_entries, 2, "the payload was carried");
        assert_eq!(
            app10.find_cell(&spaces10, 5).map(|c| c.borrow().info.node),
            Some(CellInfoNode(0))
        );

        let mut spaces11 = BTreeMap::new();
        let mut app11 = CellApp::with_address(11);
        app11.start();
        let outcome = app11.add_cell(
            &mut spaces11,
            AddCellRequest {
                space_id: 5,
                cells: tree,
                is_first_cell: false,
                is_from_db: false,
                space_data_count: 2,
            },
        );
        assert!(outcome.cell_registered);
        assert_eq!(app11.cells[0].borrow().info.node, CellInfoNode(1));

        assert!(mgr.agrees_with(&app10, &spaces10));
        assert!(mgr.agrees_with(&app11, &spaces11));
    }

    /// The manager re-adding a cell it had added before re-uses the space
    /// and the cell: the data faces reset, the entities stay, and no
    /// duplicate registration appears.
    #[test]
    fn a_second_add_cell_reuses_the_space_and_the_cell() {
        let tree = vec![cell_info(5, 0)];
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        let request = |space_data_count| AddCellRequest {
            space_id: 5,
            cells: tree.clone(),
            is_first_cell: false,
            is_from_db: false,
            space_data_count,
        };
        let first = app.add_cell(&mut spaces, request(3));
        assert!(first.cell_registered && !first.space_reused);

        // Dirt the data faces and put an entity in, then let the manager
        // re-add the cell.
        {
            let space = spaces.get_mut(&5).expect("space");
            space.beg_data_seq = 7;
            space.end_data_seq = 9;
            space.initial_time_of_day = 0.5;
            space.game_seconds_per_second = 2.0;
            space.add_entity(entity(1, true));
        }
        let second = app.add_cell(&mut spaces, request(0));
        assert_eq!(
            second,
            AddCellOutcome {
                space_reused: true,
                cell_reused: true,
                ..Default::default()
            }
        );
        let space = spaces.get(&5).expect("space");
        assert_eq!(space.beg_data_seq, 0, "reuse reset the data sequence");
        assert_eq!(space.end_data_seq, 0);
        assert_eq!(
            space.initial_time_of_day, -1.0,
            "reuse reset the time of day"
        );
        assert_eq!(space.game_seconds_per_second, 0.0);
        assert_eq!(
            space.space_data_entries, 0,
            "an empty payload leaves the count at reuse's zero"
        );
        assert_eq!(space.num_entities(), 1, "reuse keeps the entities");
        assert_eq!(
            space.num_cells(),
            1,
            "the restored tree replaced the cleared one"
        );
        assert_eq!(app.cells.len(), 1, "no duplicate registration");
        assert_eq!(app.cells[0].borrow().reuse_count, 1);
        assert_eq!(app.created_spaces, vec![5u32], "the space was created once");
    }

    /// A space none of whose cells belong to this CellApp takes the
    /// CRITICAL path: the space is still created, but nothing registers.
    #[test]
    fn an_add_cell_without_our_cell_is_the_critical_path() {
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(10);
        app.start();
        let outcome = app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 5,
                cells: vec![CellInfo {
                    space_id: 5,
                    app_addr: 11,
                    node: CellInfoNode(0),
                }],
                is_first_cell: false,
                is_from_db: false,
                space_data_count: 0,
            },
        );
        assert_eq!(
            outcome,
            AddCellOutcome {
                no_cell_here: true,
                ..Default::default()
            }
        );
        assert_eq!(app.failed_adds, vec![5u32]);
        assert!(app.cells.is_empty());
        assert!(
            app.find_space(&spaces, 5).is_some(),
            "the space is created before the failure"
        );
        assert!(app.find_cell(&spaces, 5).is_none());
    }

    /// Only the first cell of the default space on a useDefaultSpace
    /// CellApp fires `onGetFirstCell`, with its `isFromDB`.
    #[test]
    fn the_first_cell_of_the_default_space_starts_the_app() {
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        app.set_use_default_space(true);
        let tree = vec![cell_info(1, 0)];

        let outcome = app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 1,
                cells: tree.clone(),
                is_first_cell: true,
                is_from_db: true,
                space_data_count: 0,
            },
        );
        assert!(outcome.first_cell_delivered);
        assert_eq!(app.first_cell_deliveries, vec![true]);

        // Not the first cell: no delivery.
        let outcome = app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 1,
                cells: tree.clone(),
                is_first_cell: false,
                is_from_db: true,
                space_data_count: 0,
            },
        );
        assert!(!outcome.first_cell_delivered);

        // First cell of another space: still no delivery.
        let outcome = app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 2,
                cells: vec![cell_info(2, 1)],
                is_first_cell: true,
                is_from_db: false,
                space_data_count: 0,
            },
        );
        assert!(!outcome.first_cell_delivered);
        assert_eq!(app.first_cell_deliveries.len(), 1);

        // useDefaultSpace off: no delivery either.
        let mut plain_spaces = BTreeMap::new();
        let mut plain = CellApp::with_address(APP_ADDR);
        plain.start();
        let outcome = plain.add_cell(
            &mut plain_spaces,
            AddCellRequest {
                space_id: 1,
                cells: tree,
                is_first_cell: true,
                is_from_db: true,
                space_data_count: 0,
            },
        );
        assert!(!outcome.first_cell_delivered);
        assert!(plain.first_cell_deliveries.is_empty());
    }

    /// Destroying the cell through the empty-space chain keeps the manager
    /// and the CellApp agreeing: the registry forgets the cell, the space
    /// outlives it, and the ledger still matches through its destroyed
    /// cells.
    #[test]
    fn destroying_the_cell_leaves_manager_and_cellapp_agreeing() {
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        let mgr = MgrLedger {
            spaces: vec![5],
            assignments: vec![(5, APP_ADDR, CellInfoNode(0))],
        };
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 5,
                cells: vec![cell_info(5, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data_count: 0,
            },
        );
        assert!(mgr.agrees_with(&app, &spaces));

        // The 01-01 chain empties the space; the timer width destroys the
        // cell.
        {
            let space = spaces.get_mut(&5).expect("space");
            space.add_entity(entity(1, true));
            let e = space.entities()[0].clone();
            space.remove_entity(&e, &mut app);
            space.shut_down_space(0);
            space.tick(1_000_000, &mut app);
        }

        assert!(app.cells.is_empty(), "cells_ forgot the destroyed cell");
        assert_eq!(app.destroyed_cell_nodes, vec![CellInfoNode(0)]);
        assert!(app.find_cell(&spaces, 5).is_none());
        assert!(
            app.find_space(&spaces, 5).is_some(),
            "the space outlives its cell"
        );
        assert!(
            mgr.agrees_with(&app, &spaces),
            "destroyed cells still count as agreement"
        );
    }

    /// The ledger refuses to agree with a CellApp that does not hold a
    /// space the manager created, or that lacks a cell the manager
    /// assigned to it.
    #[test]
    fn the_ledger_refuses_a_cellapp_that_disagrees() {
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 5,
                cells: vec![cell_info(5, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data_count: 0,
            },
        );

        // A ledger naming a space this CellApp does not hold disagrees.
        let unknown_space = MgrLedger {
            spaces: vec![6],
            assignments: vec![],
        };
        assert!(!unknown_space.agrees_with(&app, &spaces));

        // A ledger assigning this CellApp a cell it neither holds nor has
        // destroyed disagrees.
        let stranger = MgrLedger {
            spaces: vec![5],
            assignments: vec![(5, APP_ADDR, CellInfoNode(1))],
        };
        assert!(!stranger.agrees_with(&app, &spaces));
    }
}
