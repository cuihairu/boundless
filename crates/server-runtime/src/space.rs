//! BW-08-01-01 creating and destroying a Space (`server/cellapp/`).
//!
//! The reference `server/cellapp/space.cpp` is the CellApp-side `Space`: the
//! per-process half of a space, holding the local `Cell` slot, the entities
//! the process serves in that space, and one of the two physical backends
//! (`PhysicalChunkSpace` or, under a game delegate, `PhysicalDelegateSpace`).
//! `server/cellapp/spaces.cpp` is the per-process map of them.
//!
//! The lifecycle the acceptance pins is the empty-space reclamation chain:
//!
//! * `CellApp::addCell` reuses an existing space (`space.reuse()`, the
//!   "Re-using space %d" branch, `cellapp.cpp:1607-1617`) and `Spaces::create`
//!   only ever makes a fresh one;
//! * the last entity leaving a celled space runs `checkForShutDown`
//!   (`space.cpp:340-345`), which — single cell, no entities, app started,
//!   not already shutting down, not the default space — asks the CellAppMgr
//!   to shut the space down (`requestShutDown`, `space.cpp:1125`);
//! * the CellAppMgr's echo arms a one-second timer (`shutDownSpace`,
//!   `space.cpp:1017`, idempotent); the timeout runs `Cell::onSpaceGone` and
//!   destroys the cell when it is the single one and the space is empty
//!   (`handleTimeout`, `space.cpp:1037`);
//! * `Spaces::deleteOldSpaces` (`spaces.cpp:70-90`) then reclaims every
//!   space that has no cell and is fully unloaded — entities empty and the
//!   physical space unloaded (`isFullyUnloaded`, `space.cpp:1230`);
//! * `~Space` (`space.cpp:86-125`) destroys the ghosts it holds, warns about
//!   (but never destroys) the reals — those belong to `~Cell` — cancels the
//!   shutdown timer and clears the physical space.
//!
//! The Mercury traffic is not modelled here: the module records the state
//! transitions and collects the outbound `shutDownSpace` requests into a
//! [`SpaceOutbound`] outbox, matching the CellAppMgr module's shape. The
//! one-second dispatcher timer is the caller's job: `shut_down_space` arms
//! the flag and `handle_timeout` is the expiry. Addresses are `(ip, port)`
//! with `ip` in network byte order, like `cellappmgr`.
//!
//! Faithfulness notes (deltas from the reference, all deliberate):
//!
//! * `hasSingleCell` tests `pCellInfoTree_ == &pCell_->cellInfo()`; the
//!   cell-info tree is BW-08-01-03's topology surface, so until that lands a
//!   celled space in this module is single-cell by construction — the port
//!   tests `pCell_ != NULL` only;
//! * `PhysicalChunkSpace::isFullyUnloaded` reads
//!   `EdgeGeometryMappings::isFullyUnloaded`, whose per-axis edge collapse
//!   belongs to the chunk-loading lane; the port tracks the live mapping
//!   names directly and is unloaded when none remain;
//! * `Space::reuse`'s `recentData_`/`begDataSeq_`/`endDataSeq_` and
//!   time-of-day fields are ported as the plain fields they reset;
//!   `spaceDataMapping_` arrives with BW-08-01-03.

use std::collections::{BTreeMap, BTreeSet};

use crate::cellappmgr::{Addr, SpaceId};

/// `EntityID` (`server/common.hpp`), the entity identity across this module.
pub type EntityId = u32;

/// `SpaceEntryID` (`network/basictypes.hpp:394`): a `Mercury::Address`
/// subclass, so the port is the same `(ip, port)` pair.
pub type SpaceEntryId = Addr;

/// `DEFAULT_SPACE_ID`: the reference compares `id_ == 1` for the default
/// space (`space.cpp:972`, `:1127`).
pub const DEFAULT_SPACE_ID: SpaceId = 1;

/// `NO_SPACE_REMOVAL_HANDLE` (`entity.hpp:74`): `size_type(-1)`.
pub const NO_SPACE_REMOVAL_HANDLE: usize = usize::MAX;

// -----------------------------------------------------------------------------
// Section: The CellApp context the space reads
// -----------------------------------------------------------------------------

/// The `CellApp::instance()` state a `Space` reads in its shutdown checks.
#[derive(Clone, Copy, Debug, Default)]
pub struct CellAppContext {
    /// `CellApp::instance().hasStarted()` — the space only ever asks to shut
    /// down on a started app (`space.cpp:971`).
    pub has_started: bool,
    /// `CellAppConfig::useDefaultSpace()` (`lib/server/server_app_config.cpp:40`,
    /// default `false`): the default space (id 1) never shuts itself down.
    pub use_default_space: bool,
}

/// The outbound messages this module's objects would send.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceOutbound {
    /// `CellAppMgrGateway::shutDownSpace(spaceID)` requests
    /// (`space.cpp:1137`).
    pub shutdown_space: Vec<SpaceId>,
    /// The `ERROR_MSG "Space::requestShutDown: Requesting shut down for the
    /// default space"` cases (`space.cpp:1127-1131`): the request is still
    /// sent, exactly as the reference logs and proceeds.
    pub default_space_shutdown_errors: Vec<SpaceId>,
}

// -----------------------------------------------------------------------------
// Section: The physical space backends
// -----------------------------------------------------------------------------

/// One of the two `IPhysicalSpace` backends `Space` picks at construction
/// (`space.cpp:71-81`): a game delegate selects the delegate backend,
/// everything else gets the chunk one.
#[derive(Clone, Debug, PartialEq)]
pub enum PhysicalSpace {
    /// `PhysicalChunkSpace` (`physical_chunk_space.cpp`): the live geometry
    /// mappings, by name. The reference's
    /// `EdgeGeometryMappings::isFullyUnloaded` walks per-axis edge collapse;
    /// the edge machinery is the chunk-loading lane's, so the port tracks
    /// the mapping names and is unloaded when none remain.
    Chunk { mappings: BTreeSet<String> },
    /// `PhysicalDelegateSpace` (`physical_delegate_space.cpp`): the world
    /// resources by entry id (`paths_`); unloaded when none remain
    /// (`physical_delegate_space.cpp:258-261`).
    Delegate {
        paths: BTreeMap<SpaceEntryId, String>,
    },
}

impl Default for PhysicalSpace {
    /// The chunk backend is the construction default (no game delegate).
    fn default() -> Self {
        PhysicalSpace::Chunk {
            mappings: BTreeSet::new(),
        }
    }
}

impl PhysicalSpace {
    /// `IPhysicalSpace::isFullyUnloaded` for the chosen backend.
    pub fn is_fully_unloaded(&self) -> bool {
        match self {
            PhysicalSpace::Chunk { mappings } => mappings.is_empty(),
            PhysicalSpace::Delegate { paths } => paths.is_empty(),
        }
    }

    /// `PhysicalChunkSpace::reuse` / `PhysicalDelegateSpace::reuse`: both
    /// clear (`physical_chunk_space.cpp:172-175` — `pChunkSpace_->clear()`,
    /// and `physical_delegate_space.cpp:267-270` — `this->clear()`).
    pub fn reuse(&mut self) {
        self.clear();
    }

    /// `IPhysicalSpace::clear`: drop every loaded resource.
    pub fn clear(&mut self) {
        match self {
            PhysicalSpace::Chunk { mappings } => mappings.clear(),
            PhysicalSpace::Delegate { paths } => paths.clear(),
        }
    }
}

// -----------------------------------------------------------------------------
// Section: Entity slots
// -----------------------------------------------------------------------------

/// The entity slot a `Space` holds. The reference stores `EntityPtr`s
/// (`SpaceEntities`); the port holds the identity and the two flags the
/// space's lifecycle reads (`isReal`, `isDestroyed`) until BW-08-02 brings
/// the real entity object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceEntitySlot {
    /// `Entity::id()`.
    pub id: EntityId,
    /// `Entity::isReal()`: reals belong to the `Cell`, not the `Space`.
    pub is_real: bool,
    /// `Entity::isDestroyed()`.
    pub destroyed: bool,
}

/// What a [`Space::destroy`] teardown did, the `~Space` observables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpaceTeardown {
    /// Ghosts destroyed by the scan (`~Space`'s destroy-and-restart loop).
    pub destroyed_ghosts: usize,
    /// `WARNING_MSG "Space::~Space: not destroying real entity %s %u"`
    /// emissions (`space.cpp:99-104`) — one per surviving real per pass:
    /// each destroyed ghost restarts the scan from the front, so a real in
    /// front of N ghosts is warned N+1 times.
    pub real_warnings: usize,
}

// -----------------------------------------------------------------------------
// Section: The local Cell slot
// -----------------------------------------------------------------------------

/// The local `Cell` of a space. The full cell surface (entity ranges,
/// retirement, offloads) is BW-08-01-02's; the slot here carries the
/// identity and the one marker the space-shutdown path reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The address of the CellApp running this cell (the reference's
    /// `cellInfo().addr()`).
    pub addr: Addr,
    /// `Cell::onSpaceGone` was called (`cell.cpp:837`) — the shutdown
    /// timer's first act.
    pub space_gone: bool,
}

// -----------------------------------------------------------------------------
// Section: Space
// -----------------------------------------------------------------------------

/// One recent data entry, `Space::RecentDataEntry` (`space.hpp:186-190`):
/// reset by `reuse`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentDataEntry {
    /// The entry the data belongs to.
    pub entry_id: SpaceEntryId,
    /// The game time it arrived.
    pub time: u32,
    /// The data key.
    pub key: u16,
}

/// A space as one CellApp process serves it (`server/cellapp/space.hpp`).
#[derive(Clone, Debug, Default)]
pub struct Space {
    id: SpaceId,
    cell: Option<Cell>,
    physical: PhysicalSpace,
    /// `entities_`: the entities this process serves here, with the swap-pop
    /// removal-handle order (`space.cpp:302-345`).
    entities: Vec<SpaceEntitySlot>,
    /// `shuttingDownTimerHandle_.isSet()`.
    shutting_down: bool,
    /// `begDataSeq_`.
    beg_data_seq: i32,
    /// `endDataSeq_`.
    end_data_seq: i32,
    /// `recentData_`.
    recent_data: Vec<RecentDataEntry>,
    /// `initialTimeOfDay_` (starts at `-1.f`).
    initial_time_of_day: f32,
    /// `gameSecondsPerSecond_`.
    game_seconds_per_second: f32,
}

impl Space {
    /// `Space::Space(SpaceID)` (`space.cpp:62-84`): the physical backend is
    /// picked by the delegate flag.
    pub fn new(id: SpaceId, delegate_mode: bool) -> Self {
        Self {
            id,
            cell: None,
            physical: if delegate_mode {
                PhysicalSpace::Delegate {
                    paths: BTreeMap::new(),
                }
            } else {
                PhysicalSpace::Chunk {
                    mappings: BTreeSet::new(),
                }
            },
            entities: Vec::new(),
            shutting_down: false,
            beg_data_seq: 0,
            end_data_seq: 0,
            recent_data: Vec::new(),
            initial_time_of_day: -1.0,
            game_seconds_per_second: 0.0,
        }
    }

    /// `Space::id`.
    pub fn id(&self) -> SpaceId {
        self.id
    }

    /// `Space::pCell()` — the local cell slot.
    pub fn cell(&self) -> Option<&Cell> {
        self.cell.as_ref()
    }

    /// `Space::pCell()`: whether a local cell controls this space.
    pub fn has_cell(&self) -> bool {
        self.cell.is_some()
    }

    /// `Space::pCell( Cell * )` (`space.cpp:147-150`) plus the `Cell`
    /// constructor's `space_.pCell( this )` (`cell.cpp:96`): install the
    /// local cell slot.
    pub fn set_cell(&mut self, addr: Addr) {
        self.cell = Some(Cell {
            addr,
            space_gone: false,
        });
    }

    /// The physical backend, read-only.
    pub fn physical(&self) -> &PhysicalSpace {
        &self.physical
    }

    /// The physical backend, mutable (the chunk-loading lane drives it).
    pub fn physical_mut(&mut self) -> &mut PhysicalSpace {
        &mut self.physical
    }

    /// `Space::numEntities`.
    pub fn num_entities(&self) -> usize {
        self.entities.len()
    }

    /// The entity slots, in removal-handle order.
    pub fn entities(&self) -> &[SpaceEntitySlot] {
        &self.entities
    }

    /// `Space::begDataSeq`.
    pub fn beg_data_seq(&self) -> i32 {
        self.beg_data_seq
    }

    /// `Space::endDataSeq`.
    pub fn end_data_seq(&self) -> i32 {
        self.end_data_seq
    }

    /// The recent data entries.
    pub fn recent_data(&self) -> &[RecentDataEntry] {
        &self.recent_data
    }

    /// `Space::initialTimeOfDay_`.
    pub fn initial_time_of_day(&self) -> f32 {
        self.initial_time_of_day
    }

    /// `Space::gameSecondsPerSecond_`.
    pub fn game_seconds_per_second(&self) -> f32 {
        self.game_seconds_per_second
    }

    /// `Space::isShuttingDown` (`space.hpp:214`).
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    /// `Space::addEntity` (`space.cpp:302-312`): append and hand out the
    /// removal handle. `None` when the entity is already in the space (the
    /// reference's `MF_ASSERT( removalHandle() == NO_SPACE_REMOVAL_HANDLE )`).
    pub fn add_entity(&mut self, id: EntityId, is_real: bool) -> Option<usize> {
        if self.entities.iter().any(|slot| slot.id == id) {
            return None;
        }
        let handle = self.entities.len();
        self.entities.push(SpaceEntitySlot {
            id,
            is_real,
            destroyed: false,
        });
        Some(handle)
    }

    /// `Space::removeEntity` (`space.cpp:314-345`): swap the last entity
    /// into the removed slot, then — the space having gone empty under a
    /// cell — run the shutdown check. Returns whether the entity was in the
    /// space.
    pub fn remove_entity(
        &mut self,
        id: EntityId,
        context: &CellAppContext,
        out: &mut SpaceOutbound,
    ) -> bool {
        let Some(handle) = self.entities.iter().position(|slot| slot.id == id) else {
            return false;
        };
        let last = self.entities.len() - 1;
        self.entities.swap(handle, last);
        self.entities.pop();
        if self.entities.is_empty() && self.cell.is_some() {
            self.check_for_shut_down(context, out);
        }
        true
    }

    /// `Space::hasSingleCell` (`space.cpp:982-985`). The reference tests
    /// `pCell_ && pCellInfoTree_ == &pCell_->cellInfo()`; the cell-info tree
    /// is BW-08-01-03's, so a celled space here is single-cell by
    /// construction.
    pub fn has_single_cell(&self) -> bool {
        self.cell.is_some()
    }

    /// `Space::checkForShutDown` (`space.cpp:966-979`): single cell, no
    /// entities, app started, not already shutting down, and not the default
    /// space — ask for the space's shutdown.
    pub fn check_for_shut_down(&mut self, context: &CellAppContext, out: &mut SpaceOutbound) {
        if self.has_single_cell()
            && self.entities.is_empty()
            && context.has_started
            && !self.is_shutting_down()
            && !(context.use_default_space && self.id == DEFAULT_SPACE_ID)
        {
            // INFO_MSG "Space::checkForShutDown: Space %u is now empty."
            self.request_shut_down(context, out);
        }
    }

    /// `Space::requestShutDown` (`space.cpp:1125-1138`): the default space
    /// under `useDefaultSpace` logs the ERROR (recorded in the outbox) and
    /// the request is sent either way.
    pub fn request_shut_down(&mut self, context: &CellAppContext, out: &mut SpaceOutbound) {
        if context.use_default_space && self.id == DEFAULT_SPACE_ID {
            out.default_space_shutdown_errors.push(self.id);
        }
        out.shutdown_space.push(self.id);
    }

    /// `Space::shutDownSpace` (`space.cpp:1017-1034`): arm the one-second
    /// timer, once. A second call is the `INFO_MSG "Already shutting down"`
    /// branch and changes nothing.
    pub fn shut_down_space(&mut self) -> bool {
        if self.shutting_down {
            return false;
        }
        self.shutting_down = true;
        true
    }

    /// `Space::handleTimeout` (`space.cpp:1037-1050`): the shutdown timer's
    /// expiry — `onSpaceGone` on the cell, and the cell destroyed when it is
    /// the single one and the space is empty. The destructor of the
    /// reference's `Cell` clears the space's pointer back, which is the
    /// slot removal here.
    pub fn handle_timeout(&mut self) {
        if let Some(cell) = self.cell.as_mut() {
            cell.space_gone = true;
            if self.has_single_cell() && self.entities.is_empty() {
                self.cell = None;
            }
        }
    }

    /// `Space::isFullyUnloaded` (`space.cpp:1230-1248`): no entities, and
    /// the physical space unloaded.
    pub fn is_fully_unloaded(&self) -> bool {
        // "if some (ghost) entities have been given to us since killCell was
        // called, then 1. that's bad, and 2. we're not unloaded"
        if !self.entities.is_empty() {
            return false;
        }
        self.physical.is_fully_unloaded()
    }

    /// `Space::reuse` (`space.cpp:128-145`): the state a recycled space
    /// starts from — the recent-data window and sequence counters, the
    /// time-of-day pair, and every loaded physical resource.
    pub fn reuse(&mut self) {
        self.recent_data.clear();
        self.beg_data_seq = 0;
        self.end_data_seq = 0;
        self.initial_time_of_day = -1.0;
        self.game_seconds_per_second = 0.0;
        self.physical.reuse();
    }

    /// Record a recent data entry (the `recentData_` push the data handlers
    /// do; the handlers themselves are BW-08-01-03's).
    pub fn push_recent_data(&mut self, entry_id: SpaceEntryId, time: u32, key: u16) {
        self.recent_data.push(RecentDataEntry {
            entry_id,
            time,
            key,
        });
    }

    /// Bump the data sequence window (the `begDataSeq_`/`endDataSeq_`
    /// movement the data handlers do).
    pub fn advance_data_seq(&mut self, beg: i32, end: i32) {
        self.beg_data_seq = beg;
        self.end_data_seq = end;
    }

    /// `~Space` (`space.cpp:86-125`) as an explicit teardown: destroy the
    /// ghosts (restart the scan after each), warn about the reals, cancel
    /// the timer, clear the physical space. The caller (the `Spaces` map,
    /// `deleteOldSpaces`'s erase + `delete`) drops the object afterwards.
    pub fn destroy(&mut self) -> SpaceTeardown {
        let mut teardown = SpaceTeardown::default();
        loop {
            let mut restarted = false;
            let mut index = 0;
            while index < self.entities.len() {
                let slot = self.entities[index];
                if !slot.is_real && !slot.destroyed {
                    self.entities[index].destroyed = true;
                    teardown.destroyed_ghosts += 1;
                    restarted = true;
                    break;
                } else if slot.is_real && !slot.destroyed {
                    teardown.real_warnings += 1;
                }
                index += 1;
            }
            if !restarted {
                break;
            }
        }
        // shuttingDownTimerHandle_.cancel();
        self.shutting_down = false;
        self.physical.clear();
        teardown
    }
}

// -----------------------------------------------------------------------------
// Section: Spaces
// -----------------------------------------------------------------------------

/// What [`Spaces::acquire`] did for an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceAcquisition {
    /// `CellApp::addCell`'s reuse branch (`cellapp.cpp:1609-1612`): the id
    /// was already here and the space must be `reuse`d ("Re-using space %d").
    Reused,
    /// `Spaces::create` (`spaces.cpp:44-51`): a fresh space was made.
    Created,
}

/// The per-process space map (`server/cellapp/spaces.hpp`).
#[derive(Clone, Debug, Default)]
pub struct Spaces {
    /// `container_`, ordered by id like the reference's `BW::map`.
    container: BTreeMap<SpaceId, Space>,
}

impl Spaces {
    /// A fresh, empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// `Spaces::find`.
    pub fn find(&self, id: SpaceId) -> Option<&Space> {
        self.container.get(&id)
    }

    /// The mutable half of `Spaces::find`.
    pub fn find_mut(&mut self, id: SpaceId) -> Option<&mut Space> {
        self.container.get_mut(&id)
    }

    /// `Spaces::size`.
    pub fn len(&self) -> usize {
        self.container.len()
    }

    /// Whether the map holds no spaces.
    pub fn is_empty(&self) -> bool {
        self.container.is_empty()
    }

    /// The spaces, in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Space> {
        self.container.values()
    }

    /// `Spaces::create` (`spaces.cpp:44-51`). The reference asserts the id
    /// is not present (`MF_ASSERT( this->find( id ) == NULL )`) and would
    /// otherwise leak the old entry; the port returns `None` for a duplicate
    /// and keeps the existing space.
    pub fn create(&mut self, id: SpaceId, delegate_mode: bool) -> Option<&mut Space> {
        if self.container.contains_key(&id) {
            return None;
        }
        self.container.insert(id, Space::new(id, delegate_mode));
        self.container.get_mut(&id)
    }

    /// `CellApp::addCell`'s space acquisition (`cellapp.cpp:1607-1617`): an
    /// existing id comes back as [`SpaceAcquisition::Reused`] — the caller
    /// runs `reuse()` on it — a fresh one as [`SpaceAcquisition::Created`].
    pub fn acquire(&mut self, id: SpaceId, delegate_mode: bool) -> (SpaceAcquisition, &mut Space) {
        let existed = self.container.contains_key(&id);
        let space = self
            .container
            .entry(id)
            .or_insert_with(|| Space::new(id, delegate_mode));
        let acquisition = if existed {
            SpaceAcquisition::Reused
        } else {
            SpaceAcquisition::Created
        };
        (acquisition, space)
    }

    /// `Spaces::deleteOldSpaces` (`spaces.cpp:70-90`): erase and destroy
    /// every space with no cell that is fully unloaded, in id order. The
    /// reclaimed ids come back — the "spaces only erased from container_
    /// here" pass.
    pub fn delete_old_spaces(&mut self) -> Vec<SpaceId> {
        let doomed: Vec<SpaceId> = self
            .container
            .values()
            .filter(|space| !space.has_cell() && space.is_fully_unloaded())
            .map(|space| space.id())
            .collect();
        for id in &doomed {
            if let Some(mut space) = self.container.remove(id) {
                space.destroy();
            }
        }
        doomed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> CellAppContext {
        CellAppContext {
            has_started: true,
            use_default_space: false,
        }
    }

    fn app(use_default_space: bool) -> CellAppContext {
        CellAppContext {
            has_started: true,
            use_default_space,
        }
    }

    // -------------------------------------------------------------------
    // TDD: BW-08-01-01 创建和销毁 Space — 空 Space 回收行为吻合
    // -------------------------------------------------------------------

    /// `Space::Space(SpaceID)` (`space.cpp:62-84`): the id sticks, no cell,
    /// the chunk backend by default, the delegate backend under the flag,
    /// the sequence window at zero and the time-of-day at its `-1` start.
    #[test]
    fn a_fresh_space_starts_empty_with_the_chosen_backend() {
        let chunk = Space::new(7, false);
        assert_eq!(chunk.id(), 7);
        assert!(!chunk.has_cell());
        assert_eq!(
            chunk.physical(),
            &PhysicalSpace::Chunk {
                mappings: BTreeSet::new()
            }
        );
        assert_eq!(chunk.beg_data_seq(), 0);
        assert_eq!(chunk.end_data_seq(), 0);
        assert_eq!(chunk.initial_time_of_day(), -1.0);
        assert_eq!(chunk.game_seconds_per_second(), 0.0);
        assert!(!chunk.is_shutting_down());

        let delegate = Space::new(8, true);
        assert_eq!(
            delegate.physical(),
            &PhysicalSpace::Delegate {
                paths: BTreeMap::new()
            }
        );
    }

    /// `Spaces::create` (`spaces.cpp:44-51`): a fresh id creates; a
    /// duplicate id is refused and keeps the original (the reference's
    /// `MF_ASSERT( this->find( id ) == NULL )`).
    #[test]
    fn creating_a_space_refuses_a_duplicate_id() {
        let mut spaces = Spaces::new();
        assert!(spaces.create(3, false).is_some());
        assert!(spaces.create(3, false).is_none());
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces.find(3).expect("the first space").id(), 3);
    }

    /// `Spaces::deleteOldSpaces` (`spaces.cpp:70-90`) — the acceptance: a
    /// cellless, fully-unloaded space is reclaimed; everything else stays.
    #[test]
    fn delete_old_spaces_reclaims_cellless_unloaded_spaces_only() {
        let mut spaces = Spaces::new();
        spaces.create(1, false);
        spaces.create(2, false);

        // Nothing has a cell and nothing is loaded: both go.
        let reclaimed = spaces.delete_old_spaces();
        assert_eq!(reclaimed, vec![1, 2]);
        assert!(spaces.is_empty());
    }

    /// A space that still has a cell survives `deleteOldSpaces`.
    #[test]
    fn delete_old_spaces_keeps_a_space_with_a_cell() {
        let mut spaces = Spaces::new();
        spaces.create(5, false);
        spaces.find_mut(5).unwrap().set_cell((1, 5000));
        assert_eq!(spaces.delete_old_spaces(), Vec::<SpaceId>::new());
        assert!(spaces.find(5).is_some());
    }

    /// `Space::isFullyUnloaded` (`space.cpp:1230-1248`): entities pin the
    /// space even with no cell — "if some (ghost) entities have been given
    /// to us since killCell was called … we're not unloaded".
    #[test]
    fn delete_old_spaces_keeps_a_space_with_entities() {
        let mut spaces = Spaces::new();
        spaces.create(6, false);
        let space = spaces.find_mut(6).unwrap();
        assert_eq!(space.add_entity(41, false), Some(0));
        assert!(!space.is_fully_unloaded());
        assert_eq!(spaces.delete_old_spaces(), Vec::<SpaceId>::new());
        assert!(spaces.find(6).is_some());
    }

    /// Loaded geometry pins the space too: no cell, no entities, but a
    /// live mapping means not fully unloaded.
    #[test]
    fn delete_old_spaces_keeps_a_space_with_loaded_geometry() {
        let mut spaces = Spaces::new();
        spaces.create(9, false);
        {
            let space = spaces.find_mut(9).unwrap();
            if let PhysicalSpace::Chunk { mappings } = space.physical_mut() {
                mappings.insert("space.center".to_owned());
            }
            assert!(!space.is_fully_unloaded());
        }
        assert_eq!(spaces.delete_old_spaces(), Vec::<SpaceId>::new());
        assert!(spaces.find(9).is_some());

        // The mapping unloading releases the space.
        let space = spaces.find_mut(9).unwrap();
        if let PhysicalSpace::Chunk { mappings } = space.physical_mut() {
            mappings.clear();
        }
        assert!(space.is_fully_unloaded());
    }

    /// The delegate backend is unloaded when its paths are
    /// (`physical_delegate_space.cpp:258-261`).
    #[test]
    fn the_delegate_backend_unloads_with_its_paths() {
        let mut space = Space::new(4, true);
        assert!(space.is_fully_unloaded());
        if let PhysicalSpace::Delegate { paths } = space.physical_mut() {
            paths.insert((1, 5000), "worlds/a".to_owned());
        }
        assert!(!space.is_fully_unloaded());
        space.physical_mut().clear();
        assert!(space.is_fully_unloaded());
    }

    /// `Space::removeEntity` (`space.cpp:314-345`): the swap-pop order and
    /// the empty-space shutdown check on the last removal.
    #[test]
    fn removing_entities_swaps_the_last_into_the_slot_and_checks_on_empty() {
        let mut space = Space::new(2, false);
        space.set_cell((1, 5000));
        assert_eq!(space.add_entity(11, true), Some(0));
        assert_eq!(space.add_entity(12, true), Some(1));
        assert_eq!(space.add_entity(13, true), Some(2));

        let context = started();
        let mut out = SpaceOutbound::default();
        // Removing a middle entity pulls the last one into its slot.
        assert!(space.remove_entity(11, &context, &mut out));
        assert_eq!(
            space.entities(),
            &[
                SpaceEntitySlot {
                    id: 13,
                    is_real: true,
                    destroyed: false
                },
                SpaceEntitySlot {
                    id: 12,
                    is_real: true,
                    destroyed: false
                },
            ]
        );
        // Still populated: no shutdown request yet.
        assert!(out.shutdown_space.is_empty());

        // The second removal leaves one entity: still no request.
        assert!(space.remove_entity(12, &context, &mut out));
        assert!(out.shutdown_space.is_empty());

        // The last removal empties the space under a cell: one request.
        assert!(space.remove_entity(13, &context, &mut out));
        assert_eq!(out.shutdown_space, vec![2]);

        // Unknown ids report false and touch nothing.
        assert!(!space.remove_entity(99, &context, &mut out));
        assert_eq!(out.shutdown_space.len(), 1);
    }

    /// `Space::removeEntity` without a cell does not check for shutdown
    /// (`space.cpp:340-345`: the check only runs `if (pCell_ != NULL)`).
    #[test]
    fn the_last_removal_in_a_cellless_space_requests_nothing() {
        let mut space = Space::new(3, false);
        space.add_entity(21, false);
        let context = started();
        let mut out = SpaceOutbound::default();
        assert!(space.remove_entity(21, &context, &mut out));
        assert_eq!(out, SpaceOutbound::default());
    }

    /// `Space::checkForShutDown` (`space.cpp:966-979`) gates: app started,
    /// not already shutting down.
    #[test]
    fn check_for_shutdown_waits_for_a_started_unshutting_app() {
        let mut space = Space::new(10, false);
        space.set_cell((1, 5000));

        // Not started: nothing.
        let mut out = SpaceOutbound::default();
        space.check_for_shut_down(
            &CellAppContext {
                has_started: false,
                use_default_space: false,
            },
            &mut out,
        );
        assert!(out.shutdown_space.is_empty());

        // Started: the request goes.
        space.check_for_shut_down(&started(), &mut out);
        assert_eq!(out.shutdown_space, vec![10]);

        // Already shutting down (the timer armed): nothing more.
        assert!(space.shut_down_space());
        space.check_for_shut_down(&started(), &mut out);
        assert_eq!(out.shutdown_space.len(), 1);
    }

    /// `Space::hasSingleCell` (`space.cpp:982-985`): without a cell there is
    /// nothing to shut down.
    #[test]
    fn check_for_shutdown_requires_a_cell() {
        let mut space = Space::new(11, false);
        let mut out = SpaceOutbound::default();
        space.check_for_shut_down(&started(), &mut out);
        assert!(out.shutdown_space.is_empty());
    }

    /// The default space never shuts itself down under `useDefaultSpace`
    /// (`space.cpp:972`, `:1127-1131`), and the refusal still sends the
    /// request with the ERROR recorded.
    #[test]
    fn the_default_space_refuses_but_records_its_shutdown_request() {
        let mut spaces = Spaces::new();
        spaces.create(DEFAULT_SPACE_ID, false);
        let space = spaces.find_mut(DEFAULT_SPACE_ID).unwrap();
        space.set_cell((1, 5000));

        // useDefaultSpace on: the check never fires for id 1.
        let mut out = SpaceOutbound::default();
        space.check_for_shut_down(&app(true), &mut out);
        assert!(out.shutdown_space.is_empty());

        // A direct request logs the ERROR and sends anyway.
        space.request_shut_down(&app(true), &mut out);
        assert_eq!(out.default_space_shutdown_errors, vec![DEFAULT_SPACE_ID]);
        assert_eq!(out.shutdown_space, vec![DEFAULT_SPACE_ID]);

        // A non-default space in the same mode shuts down normally.
        spaces.create(2, false);
        let other = spaces.find_mut(2).unwrap();
        other.set_cell((1, 5000));
        let mut out2 = SpaceOutbound::default();
        other.check_for_shut_down(&app(true), &mut out2);
        assert_eq!(out2.shutdown_space, vec![2]);
        assert!(out2.default_space_shutdown_errors.is_empty());
    }

    /// With `useDefaultSpace` off, the default space behaves like any other
    /// (the gate is the config flag paired with the id, not the id alone).
    #[test]
    fn the_default_space_shuts_down_when_the_config_disallows_defaults() {
        let mut space = Space::new(DEFAULT_SPACE_ID, false);
        space.set_cell((1, 5000));
        let mut out = SpaceOutbound::default();
        space.check_for_shut_down(&started(), &mut out);
        assert_eq!(out.shutdown_space, vec![DEFAULT_SPACE_ID]);
        assert!(out.default_space_shutdown_errors.is_empty());
    }

    /// `Space::shutDownSpace` (`space.cpp:1017-1034`): the timer arms once;
    /// the second call is the "Already shutting down" no-op.
    #[test]
    fn shut_down_space_arms_its_timer_once() {
        let mut space = Space::new(12, false);
        assert!(space.shut_down_space());
        assert!(space.is_shutting_down());
        assert!(!space.shut_down_space());
        assert!(space.is_shutting_down());
    }

    /// `Space::handleTimeout` (`space.cpp:1037-1050`): `onSpaceGone` runs
    /// and the single empty cell is destroyed in the same call — the
    /// `MF_ASSERT( pCell_ == NULL )` the reference makes after
    /// `destroyCell` — which is what lets `deleteOldSpaces` reclaim the
    /// space. The full reclamation chain.
    #[test]
    fn the_shutdown_timer_destroys_a_single_empty_cell_and_the_space_is_reclaimed() {
        let mut spaces = Spaces::new();
        spaces.create(20, false);
        {
            let space = spaces.find_mut(20).unwrap();
            space.set_cell((1, 5000));
            assert!(space.shut_down_space());
            space.handle_timeout();
            assert!(space.cell().is_none());
            assert!(space.is_fully_unloaded());
        }
        // A second expiry changes nothing (the cell is already gone).
        spaces.find_mut(20).unwrap().handle_timeout();
        assert_eq!(spaces.delete_old_spaces(), vec![20]);
        assert!(spaces.is_empty());
    }

    /// `Space::handleTimeout` keeps the cell when entities remain: only
    /// `onSpaceGone` runs.
    #[test]
    fn the_shutdown_timer_keeps_a_cell_with_entities() {
        let mut space = Space::new(21, false);
        space.set_cell((1, 5000));
        space.add_entity(31, true);
        space.shut_down_space();
        space.handle_timeout();
        space.handle_timeout();
        let cell = space.cell().expect("the cell survives");
        assert!(cell.space_gone);
        assert_eq!(space.num_entities(), 1);
    }

    /// `Space::reuse` (`space.cpp:128-145`): the recycled state — recent
    /// data window, sequence counters, time-of-day pair and the loaded
    /// physical resources.
    #[test]
    fn reuse_resets_the_sequence_window_time_of_day_and_physical_state() {
        let mut space = Space::new(30, false);
        space.push_recent_data((1, 5000), 77, 3);
        space.advance_data_seq(5, 9);
        space.initial_time_of_day = 0.25;
        space.game_seconds_per_second = 2.0;
        if let PhysicalSpace::Chunk { mappings } = space.physical_mut() {
            mappings.insert("space.center".to_owned());
        }

        space.reuse();
        assert!(space.recent_data().is_empty());
        assert_eq!(space.beg_data_seq(), 0);
        assert_eq!(space.end_data_seq(), 0);
        assert_eq!(space.initial_time_of_day(), -1.0);
        assert_eq!(space.game_seconds_per_second(), 0.0);
        assert_eq!(
            space.physical(),
            &PhysicalSpace::Chunk {
                mappings: BTreeSet::new()
            }
        );
    }

    /// `Spaces::acquire` (`cellapp.cpp:1607-1617`): an existing id is
    /// reused and the caller resets it; a fresh id creates.
    #[test]
    fn acquiring_reuses_an_existing_space_and_creates_a_missing_one() {
        let mut spaces = Spaces::new();
        spaces.create(40, false);
        spaces
            .find_mut(40)
            .unwrap()
            .push_recent_data((1, 5000), 9, 1);
        spaces.find_mut(40).unwrap().advance_data_seq(2, 3);

        let (what, space) = spaces.acquire(40, false);
        assert_eq!(what, SpaceAcquisition::Reused);
        assert_eq!(space.id(), 40);
        space.reuse();
        assert!(space.recent_data().is_empty());
        assert_eq!(space.beg_data_seq(), 0);

        let (what, space) = spaces.acquire(41, false);
        assert_eq!(what, SpaceAcquisition::Created);
        assert_eq!(space.id(), 41);
        assert_eq!(spaces.len(), 2);
    }

    /// `~Space` (`space.cpp:86-125`): the ghost scan destroys every ghost
    /// (restarting after each), warns about each surviving real on every
    /// pass that meets it — the restart re-warns — and never destroys the
    /// reals.
    #[test]
    fn the_teardown_destroys_ghosts_and_warns_about_reals() {
        let mut space = Space::new(50, false);
        space.add_entity(1, true); // real
        space.add_entity(2, false); // ghost
        space.add_entity(3, false); // ghost

        let teardown = space.destroy();
        assert_eq!(teardown.destroyed_ghosts, 2);
        // The scan restarts after each ghost: [R,G,G] warns over R three
        // times (once per pass), G twice, G once — one warning per pass the
        // real survives, three passes.
        assert_eq!(teardown.real_warnings, 3);
        let slots = space.entities();
        assert!(
            slots
                .iter()
                .filter(|slot| slot.is_real)
                .all(|slot| !slot.destroyed)
        );
        assert!(
            slots
                .iter()
                .filter(|slot| !slot.is_real)
                .all(|slot| slot.destroyed)
        );
        // The timer is cancelled and the physical space cleared.
        assert!(!space.is_shutting_down());
        assert_eq!(
            space.physical(),
            &PhysicalSpace::Chunk {
                mappings: BTreeSet::new()
            }
        );
    }

    /// A ghost-only teardown has no warnings, and an already-destroyed
    /// ghost is not destroyed twice.
    #[test]
    fn the_teardown_counts_only_undestroyed_ghosts() {
        let mut space = Space::new(51, false);
        space.add_entity(1, false);
        space.entities[0].destroyed = true;
        space.add_entity(2, false);

        let teardown = space.destroy();
        assert_eq!(teardown.destroyed_ghosts, 1);
        assert_eq!(teardown.real_warnings, 0);
    }
}
