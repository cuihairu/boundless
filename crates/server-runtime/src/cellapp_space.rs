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
}

impl Space {
    /// `Space( SpaceID id )`: no cell, no entities, no timer.
    pub fn new(id: SpaceId) -> Self {
        Self {
            id,
            cell: None,
            cells: Vec::new(),
            entities: Vec::new(),
            timer_armed_at: None,
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

    /// `pCell( Cell * )`: install the cell. With exactly one cell its info
    /// is the tree root — the shape [`Space::has_single_cell`] recognises.
    pub fn set_cell(&mut self, cell: Rc<RefCell<Cell>>) {
        let info = cell.borrow().info.clone();
        self.cell = Some(cell);
        self.cells = vec![info];
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
    /// The `shutDownSpace` requests the spaces sent (the CellAppMgr half
    /// of the round trip).
    pub shut_down_requests: Vec<SpaceId>,
    /// The `destroyCell` calls, by cell's tree identity.
    pub destroyed_cell_nodes: Vec<CellInfoNode>,
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

    /// `CellApp::destroyCell`: the cell goes away; the space clears its
    /// own `pCell_` (the reference's destructed cell does it).
    pub fn destroy_cell(&mut self, cell: &Rc<RefCell<Cell>>) {
        self.destroyed_cell_nodes.push(cell.borrow().info.node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(id: u32, real: bool) -> Rc<RefCell<Entity>> {
        Rc::new(RefCell::new(Entity::new(id, real)))
    }

    /// A started CellApp, a single-cell space, and the cell it holds.
    fn fixture(id: SpaceId, node: usize) -> (CellApp, Space, Rc<RefCell<Cell>>) {
        let mut cell_app = CellApp::new();
        cell_app.start();
        let cell = Rc::new(RefCell::new(Cell {
            info: CellInfo {
                space_id: id,
                node: CellInfoNode(node),
            },
            space_gone_count: 0,
        }));
        let mut space = Space::new(id);
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
            info: CellInfo {
                space_id: 3,
                node: CellInfoNode(1),
            },
            space_gone_count: 0,
        }));
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
}
