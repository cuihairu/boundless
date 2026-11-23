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
//! BW-08-01-02 adds the CellApp-side `Cell` (`server/cellapp/cell.cpp`) and
//! its registration (`server/cellapp/cells.cpp`), with the CellAppMgr-facing
//! bookkeeping the acceptance pins — manager and CellApp agreeing on which
//! cells exist where:
//!
//! * `CellApp::addCell` (`cellapp.cpp:1584-1667`) acquires the space (reusing
//!   an existing one), refreshes the cell-info mirror, and creates or
//!   revives the local cell — `Cells::add` registering it by space id while
//!   the `Cell` constructor installs itself as the space's `pCell_`;
//! * `Cell::retireCell` (`cell.cpp:638`) toggles the retiring flag;
//! * `Cell::removeCell` (`cell.cpp:661`) marks the cell removed and banks the
//!   neighbour-address acks it must collect, `Cell::notifyOfCellRemoval`
//!   (`cell.cpp:694`) sends an `ackCellRemoval` back and marks the leaving
//!   neighbour's `CellInfo` pending-delete, and `Cell::ackCellRemoval`
//!   (`cell.cpp:714`) consumes a banked ack (or banks an early one);
//! * `Cells::checkOffloads` (`cells.cpp:115-143`) destroys the cells that are
//!   ready for deletion — removed, no space entities, no pending acks
//!   (`Cell::isReadyForDeletion`, `cell.cpp:149-159`) — each `~Cell`
//!   (`cell.cpp:108-131`) clearing the space's `pCell_` back-pointer, and
//!   `Cells::handleCellAppDeath` (`cells.cpp:96-106`) drops a dead app from
//!   every ack set.
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
//! * `hasSingleCell` tests `pCellInfoTree_ == &pCell_->cellInfo()`; since
//!   BW-08-01-03 the port compares the read tree's root leaf against the
//!   cell's address, and a cell installed without a topology (the
//!   [`Space::set_cell`] face) is single-cell by construction;
//! * the reference's `Cells::container_` owns the heap `Cell`s, with each
//!   `Space` holding a raw `pCell_` back-pointer; the port keeps each cell
//!   inside its space and `Cells` holds the registered keys, so the two
//!   views agreeing is exactly the invariant the reference's
//!   `MF_ASSERT_DEV( space_.pCell() == this )` pins;
//! * `Cell::isReadyForDeletion`'s `realEntities_` and buffered-message gates
//!   arrive with BW-08-02's entities; until then the space's entity list
//!   stands in for both;
//! * the `RemovalAcks` are a `BW::multiset` keyed by app address; the
//!   manager's addr lists name each app once, and a `multiset::erase(key)`
//!   erases every occurrence, so the port's sets carry the same semantics;
//! * `Space::updateGeometry`'s tree read and mark-and-sweep is BW-08-01-03's
//!   ([`Space::update_geometry`]); the mirror population stays available as
//!   the flat [`Space::set_cell_infos`] face, and `CellApp::addCell`'s
//!   reply and `onGetFirstCell` faces are not modelled;
//! * the space-data rows live in [`crate::space_data::SpaceDataMapping`]
//!   (`lib/network/space_data_mapping.cpp`), keyed per space — the routing
//!   that keeps one space's rows out of another's is the caller picking the
//!   space, which is exactly the reference's per-space
//!   `spaceDataMapping_`; the script-event faces
//!   (`onSpaceData`/`onSpaceDataDeleted`) are recorded on the space until
//!   the scripting lane lands, and the `writeDataToStream` wire face is the
//!   transport lane's;
//! * `PhysicalChunkSpace::isFullyUnloaded` reads
//!   `EdgeGeometryMappings::isFullyUnloaded`, whose per-axis edge collapse
//!   belongs to the chunk-loading lane; the port tracks the live mapping
//!   names directly and is unloaded when none remain;
//! * `Space::reuse`'s `recentData_`/`begDataSeq_`/`endDataSeq_`, the
//!   time-of-day pair, the cell-info tree and `spaceDataMapping_` are the
//!   plain fields it resets; `lastMappedGeometry_` and
//!   `artificialMinLoad_` survive it, exactly as the reference leaves them;

use std::collections::{BTreeMap, BTreeSet};

use crate::cellappmgr::{Addr, Rect, SpaceId};
use crate::space_data::{
    DataValue, NO_KEY, SPACE_DATA_ARTIFICIAL_LOAD, SPACE_DATA_FIRST_CELL_ONLY_KEY,
    SPACE_DATA_MAPPING_KEY_CLIENT_SERVER, SPACE_DATA_TOD_KEY, SpaceDataMapping,
};

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
    /// The `CellAppMgrInterface::updateSpaceData` sends
    /// (`space.cpp:686-694`): `(space, entry, key, value)` — the value
    /// `None` for a delete, whose message omits the blob.
    pub update_space_data: Vec<(SpaceId, SpaceEntryId, u16, Option<Vec<u8>>)>,
}

// -----------------------------------------------------------------------------
// Section: The CellInfo mirror
// -----------------------------------------------------------------------------

/// One `CellInfo` record (`server/cellapp/cell_info.hpp`): the CellAppMgr's
/// view of one cell of a space, mirrored on the app. The record's identity
/// is the owning app's address — `Space::cellInfos_` keys it that way.
#[derive(Clone, Debug, PartialEq)]
pub struct CellInfoRecord {
    /// `addr_`: the CellApp running the cell.
    pub addr: Addr,
    /// `load_`, streamed by the manager (`CellInfo::updateFromStream`).
    pub load: f32,
    /// `rect_`: the range of the world this cell owns.
    pub rect: Rect,
    /// `shouldDelete_`: `updateGeometry`'s mark-and-sweep mark (BW-08-01-03).
    pub should_delete: bool,
    /// `isDeletePending_`: ghosts stop being created toward a pending-delete
    /// cell (`setPendingDelete`).
    pub is_delete_pending: bool,
    /// `hasBeenCreated_`.
    pub has_been_created: bool,
}

impl CellInfoRecord {
    /// `CellInfo::CellInfo` (`cell_info.cpp:23-39`): everything at its zero
    /// but the identity and the range.
    pub fn new(addr: Addr, rect: Rect) -> Self {
        Self {
            addr,
            load: 0.0,
            rect,
            should_delete: false,
            is_delete_pending: false,
            has_been_created: false,
        }
    }
}

// -----------------------------------------------------------------------------
// Section: The CellApp outbound messages
// -----------------------------------------------------------------------------

/// The outbound messages this module's cell lifecycle would send: the
/// `ackCellRemoval` replies `Cell::notifyOfCellRemoval` streams, one per
/// leaving neighbour — `(removed cell app, space)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellAppOutbound {
    /// `bundle.startMessage( CellAppInterface::ackCellRemoval )` sends
    /// (`cell.cpp:700-706`).
    pub ack_cell_removals: Vec<(Addr, SpaceId)>,
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
    /// mappings, keyed by their loading entry id. The reference's
    /// `EdgeGeometryMappings::isFullyUnloaded` walks per-axis edge collapse;
    /// the edge machinery is the chunk-loading lane's, so the port tracks
    /// the mapping paths and is unloaded when none remain.
    Chunk {
        mappings: BTreeMap<SpaceEntryId, String>,
    },
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
            mappings: BTreeMap::new(),
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

    /// `PhysicalChunkSpace::loadResource` /
    /// `PhysicalDelegateSpace::loadResource`: bind the path under the
    /// loading entry id (the `SPACE_DATA_MAPPING_KEY_CLIENT_SERVER` effect,
    /// `space.cpp:834-835`).
    pub fn load_resource(&mut self, entry_id: SpaceEntryId, path: &str) {
        match self {
            PhysicalSpace::Chunk { mappings } => {
                mappings.insert(entry_id, path.to_owned());
            }
            PhysicalSpace::Delegate { paths } => {
                paths.insert(entry_id, path.to_owned());
            }
        }
    }

    /// `PhysicalChunkSpace::unloadResource` /
    /// `PhysicalDelegateSpace::unloadResource`: drop the mapping the entry
    /// id loaded (`space.cpp:849`).
    pub fn unload_resource(&mut self, entry_id: SpaceEntryId) {
        match self {
            PhysicalSpace::Chunk { mappings } => {
                mappings.remove(&entry_id);
            }
            PhysicalSpace::Delegate { paths } => {
                paths.remove(&entry_id);
            }
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
// Section: The local Cell
// -----------------------------------------------------------------------------

/// The local `Cell` of a space (`server/cellapp/cell.hpp`) — the CellApp-side
/// half of one cell, running on this app. The entity surfaces (real-entity
/// ranges, offloads) are BW-08-02's; the state here is the removal
/// bookkeeping the CellAppMgr drives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The address of the CellApp running this cell (the reference's
    /// `cellInfo().addr()`).
    pub addr: Addr,
    /// `Cell::onSpaceGone` was called (`cell.cpp:837`) — the shutdown
    /// timer's first act.
    pub space_gone: bool,
    /// `isRetiring_`: the manager is moving this cell's load away.
    is_retiring: bool,
    /// `isRemoved_`: the manager has asked for this cell to go.
    is_removed: bool,
    /// `pendingAcks_`: the CellApps that must still confirm no entities are
    /// on their way before this cell may die.
    pending_acks: BTreeSet<Addr>,
    /// `receivedAcks_`: early acks, banked against a removal that has not
    /// asked for them yet.
    received_acks: BTreeSet<Addr>,
}

impl Cell {
    /// `Cell::Cell` (`cell.cpp:82-102`): the removal state starts clean and
    /// the constructor installs itself as the space's cell
    /// (`space_.pCell( this )`).
    pub fn new(addr: Addr) -> Self {
        Self {
            addr,
            space_gone: false,
            is_retiring: false,
            is_removed: false,
            pending_acks: BTreeSet::new(),
            received_acks: BTreeSet::new(),
        }
    }

    /// `Cell::retireCell` (`cell.cpp:638-652`): set (or clear) the retiring
    /// flag — never on a removed cell, and only ever toggling.
    pub fn retire(&mut self, is_retiring: bool) {
        debug_assert!(!self.is_removed);
        debug_assert!(is_retiring == !self.is_retiring);
        self.is_retiring = is_retiring;
    }

    /// `Cell::removeCell` (`cell.cpp:661-685`): mark removed and collect the
    /// acks owed — every address not already banked in `receivedAcks_`.
    pub fn remove(&mut self, addrs: &[Addr]) {
        debug_assert!(!self.is_removed);
        self.is_removed = true;
        for addr in addrs {
            if self.received_acks.remove(addr) {
                // The early ack is consumed.
            } else {
                self.pending_acks.insert(*addr);
            }
        }
    }

    /// `Cell::ackCellRemoval` (`cell.cpp:714-739`): a neighbour confirms no
    /// more entities are coming — consume the pending ack, or bank an early
    /// one.
    pub fn ack_removal(&mut self, src: Addr) {
        if self.pending_acks.remove(&src) {
            // INFO_MSG "Got removal ack"
        } else {
            self.received_acks.insert(src);
        }
    }

    /// `Cell::handleCellAppDeath` (`cell.cpp:762-768`): a dead app owes
    /// nothing any more.
    pub fn handle_app_death(&mut self, addr: Addr) {
        self.pending_acks.remove(&addr);
        self.received_acks.remove(&addr);
    }

    /// `Cell::reuse` (`cell.cpp:745-755`): revive from a retiring-and-made
    /// removal — the "Cell did not fully remove" recovery. Returns whether
    /// the cell was in that state.
    pub fn reuse(&mut self) -> bool {
        let is_ok = self.is_retiring && self.is_removed;
        self.is_retiring = false;
        self.is_removed = false;
        is_ok
    }

    /// `Cell::isReadyForDeletion` (`cell.cpp:149-159`), the BW-08-01 face:
    /// removed, the space empty and no acks pending. The reference also
    /// gates on the cell's own `realEntities_` and the app's buffered
    /// messages — BW-08-02's surfaces.
    pub fn is_ready_for_deletion(&self, space_entities_empty: bool) -> bool {
        self.is_removed && space_entities_empty && self.pending_acks.is_empty()
    }

    /// `isRetiring_`.
    pub fn is_retiring(&self) -> bool {
        self.is_retiring
    }

    /// `isRemoved_`.
    pub fn is_removed(&self) -> bool {
        self.is_removed
    }

    /// `pendingAcks_`.
    pub fn pending_acks(&self) -> &BTreeSet<Addr> {
        &self.pending_acks
    }

    /// `receivedAcks_`.
    pub fn received_acks(&self) -> &BTreeSet<Addr> {
        &self.received_acks
    }
}

// -----------------------------------------------------------------------------
// Section: The topology tree
// -----------------------------------------------------------------------------

/// One node of the space's BSP tree (`SpaceNode`,
/// `server/cellapp/space_node.hpp`): what `Space::readTree`
/// (`space.cpp:2288-2334`) reads the manager's topology into. The wire
/// decode is the transport lane's; the tree itself is constructed here.
#[derive(Clone, Debug, PartialEq)]
pub enum SpaceNode {
    /// `SpaceBranch` (`space_branch.cpp:13-36`): the split, with the two
    /// subtrees the recursion read — horizontal splits on z, vertical on x.
    Branch {
        /// `isHorizontal_`.
        is_horizontal: bool,
        /// `position_`.
        position: f32,
        /// `pLeft_`.
        left: Box<SpaceNode>,
        /// `pRight_`.
        right: Box<SpaceNode>,
    },
    /// The `CellInfo` leaf: the app address with the streamed
    /// `load`/`hasBeenCreated` pair (`CellInfo::updateFromStream`). The
    /// rect the branch recursion computes lands on the mirrored record,
    /// not on the node.
    Leaf {
        /// `addr_`.
        addr: Addr,
        /// `load_`.
        load: f32,
        /// `hasBeenCreated_`.
        has_been_created: bool,
    },
}

/// What one [`Space::update_geometry`] pass did
/// (`space.cpp:885-958`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpdateGeometryReport {
    /// The `wasMulticell` flag the reference captures before the sweep.
    pub was_multicell: bool,
    /// Fresh `CellInfo`s the tree introduced.
    pub added: Vec<Addr>,
    /// Existing `CellInfo`s the tree refreshed (`updateFromStream`).
    pub refreshed: usize,
    /// The marked records the sweep erased.
    pub erased: Vec<Addr>,
    /// The `INFO_MSG "Cell in space %u is going"` case: this app's cell's
    /// record was swept, so the cell is going.
    pub own_cell_going: bool,
    /// Whether the `wasMulticell` tail fired a shutdown request.
    pub requested_shutdown: bool,
}

// -----------------------------------------------------------------------------
// Section: Space data
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

/// The script event a space-data change triggers
/// (`CellApp::instance().scriptEvents().triggerEvent`, `space.cpp:755-770`):
/// recorded on the space until the scripting lane lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceScriptEvent {
    /// `"onSpaceData"` for an add, `"onSpaceDataDeleted"` for a delete.
    pub is_added: bool,
    /// The effective key — for a delete, the key the deleted row held.
    pub key: u16,
    /// The payload — for a delete, the deleted row's data.
    pub value: Vec<u8>,
}

/// A space as one CellApp process serves it (`server/cellapp/space.hpp`).
#[derive(Clone, Debug, Default)]
pub struct Space {
    id: SpaceId,
    cell: Option<Cell>,
    physical: PhysicalSpace,
    /// `cellInfos_` (`space.hpp:186`): the CellAppMgr's cells of this space,
    /// mirrored, keyed by the owning app's address.
    cell_infos: BTreeMap<Addr, CellInfoRecord>,
    /// `pCellInfoTree_`: the topology the last `update_geometry` read,
    /// `None` until one has.
    tree_root: Option<SpaceNode>,
    /// `spaceDataMapping_`.
    space_data_mapping: SpaceDataMapping,
    /// `lastMappedGeometry_`.
    last_mapped_geometry: String,
    /// `shouldCheckLoadBounds_` (`space.hpp:176-181`).
    should_check_load_bounds: bool,
    /// `artificialMinLoad_`.
    artificial_min_load: f32,
    /// The script events the data handlers triggered, in order.
    script_events: Vec<SpaceScriptEvent>,
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
                    mappings: BTreeMap::new(),
                }
            },
            cell_infos: BTreeMap::new(),
            tree_root: None,
            space_data_mapping: SpaceDataMapping::new(),
            last_mapped_geometry: String::new(),
            should_check_load_bounds: false,
            artificial_min_load: 0.0,
            script_events: Vec::new(),
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

    /// The mutable half of `Space::pCell()`.
    pub fn cell_mut(&mut self) -> Option<&mut Cell> {
        self.cell.as_mut()
    }

    /// `Space::pCell( Cell * )` (`space.cpp:147-150`) plus the `Cell`
    /// constructor's `space_.pCell( this )` (`cell.cpp:96`): install the
    /// local cell slot.
    pub fn set_cell(&mut self, addr: Addr) {
        self.cell = Some(Cell::new(addr));
    }

    /// `~Cell`'s `space_.pCell( NULL )` (`cell.cpp:130`): the cell slot
    /// clears when the cell is destroyed.
    pub fn clear_cell(&mut self) {
        self.cell = None;
    }

    /// `Space::findCell` (`space.cpp:2338-2350`): the CellAppMgr's record
    /// for the cell running on `addr`, if the mirror knows one.
    pub fn find_cell(&self, addr: Addr) -> Option<&CellInfoRecord> {
        self.cell_infos.get(&addr)
    }

    /// The mirror itself, keyed by app address.
    pub fn cell_infos(&self) -> &BTreeMap<Addr, CellInfoRecord> {
        &self.cell_infos
    }

    /// Populate the cell-info mirror — the flat face of
    /// `Space::updateGeometry` (`space.cpp:885-958`); the tree read and the
    /// mark-and-sweep arrive with BW-08-01-03.
    pub fn set_cell_infos(&mut self, records: &[CellInfoRecord]) {
        self.cell_infos = records.iter().cloned().map(|r| (r.addr, r)).collect();
    }

    /// `Space::setPendingCellDelete` (`space.cpp:2357-2366`): mark the
    /// leaving neighbour's record — ghosts should no longer be created
    /// toward it. Returns whether the mirror knew the address.
    pub fn set_pending_cell_delete(&mut self, addr: Addr) -> bool {
        match self.cell_infos.get_mut(&addr) {
            Some(record) => {
                record.is_delete_pending = true;
                true
            }
            None => false,
        }
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

    /// `Space::hasSingleCell` (`space.cpp:982-985`): a cell, and the read
    /// tree's root being that cell's own leaf. A cell installed without a
    /// topology — the [`Space::set_cell`] face — is single-cell by
    /// construction; a multi-cell tree says otherwise.
    pub fn has_single_cell(&self) -> bool {
        let Some(cell) = &self.cell else {
            return false;
        };
        match &self.tree_root {
            None => true,
            Some(SpaceNode::Leaf { addr, .. }) => *addr == cell.addr,
            Some(SpaceNode::Branch { .. }) => false,
        }
    }

    /// The read topology tree, read-only.
    pub fn tree_root(&self) -> Option<&SpaceNode> {
        self.tree_root.as_ref()
    }

    /// `spaceDataMapping_`.
    pub fn space_data_mapping(&self) -> &SpaceDataMapping {
        &self.space_data_mapping
    }

    /// `lastMappedGeometry_`.
    pub fn last_mapped_geometry(&self) -> &str {
        &self.last_mapped_geometry
    }

    /// `space.hpp:120`'s setter.
    pub fn set_last_mapped_geometry(&mut self, path: &str) {
        self.last_mapped_geometry = path.to_owned();
    }

    /// `shouldCheckLoadBounds_`.
    pub fn should_check_load_bounds(&self) -> bool {
        self.should_check_load_bounds
    }

    /// `artificialMinLoad_`.
    pub fn artificial_min_load(&self) -> f32 {
        self.artificial_min_load
    }

    /// The script events the data handlers triggered, in order.
    pub fn script_events(&self) -> &[SpaceScriptEvent] {
        &self.script_events
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
    /// slot removal here. Returns whether the cell was destroyed.
    pub fn handle_timeout(&mut self) -> bool {
        let Some(cell) = self.cell.as_mut() else {
            return false;
        };
        cell.space_gone = true;
        if self.has_single_cell() && self.entities.is_empty() {
            self.clear_cell();
            return true;
        }
        false
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
        // space.cpp:149-153 — the topology is deleted and re-read.
        self.tree_root = None;
        // space.cpp:155
        self.space_data_mapping.clear();
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

    /// `Space::updateGeometry` (`space.cpp:885-958`): read the manager's
    /// topology, mark every mirrored record for deletion, unmark and
    /// refresh the leaves the tree still carries, add the fresh ones, sweep
    /// the rest, then run the tails — the "our cell is going" check and the
    /// `wasMulticell` shutdown check.
    pub fn update_geometry(
        &mut self,
        root: &SpaceNode,
        context: &CellAppContext,
        out: &mut SpaceOutbound,
    ) -> UpdateGeometryReport {
        let mut report = UpdateGeometryReport {
            was_multicell: !self.has_single_cell(),
            ..UpdateGeometryReport::default()
        };

        // Mark them all to be deleted (space.cpp:889-902).
        for record in self.cell_infos.values_mut() {
            record.should_delete = true;
        }

        // Read the tree (space.cpp:904-915), refreshing and adding leaves;
        // the tree read becomes the space's topology (space.cpp:915).
        Self::read_tree_node(self, root, Rect::unbounded(), &mut report);
        self.tree_root = Some(root.clone());

        // Sweep the records the tree no longer carries
        // (space.cpp:917-937).
        let erased: Vec<Addr> = self
            .cell_infos
            .iter()
            .filter(|(_, record)| record.should_delete)
            .map(|(addr, _)| *addr)
            .collect();
        for addr in &erased {
            self.cell_infos.remove(addr);
        }
        report.erased = erased;

        // The app's own cell (space.cpp:939-951): its record swept means it
        // is going; the staying case runs `checkOffloadsAndGhosts`, the
        // BW-08-02 offload face, not modelled yet.
        report.own_cell_going = self
            .cell
            .as_ref()
            .is_some_and(|cell| report.erased.contains(&cell.addr));

        // The `wasMulticell` tail (space.cpp:953-957).
        if report.was_multicell {
            let before = out.shutdown_space.len();
            self.check_for_shut_down(context, out);
            report.requested_shutdown = out.shutdown_space.len() > before;
        }
        report
    }

    /// One `Space::readTree` recursion step (`space.cpp:2288-2334`): the
    /// branch splits its rect and recurses; the leaf refreshes or adds its
    /// record — `updateFromStream` also clears a pending delete, an update
    /// for a re-added cell (`cell_info.cpp:50-60`).
    fn read_tree_node(
        space: &mut Self,
        node: &SpaceNode,
        rect: Rect,
        report: &mut UpdateGeometryReport,
    ) {
        match node {
            SpaceNode::Branch {
                is_horizontal,
                position,
                left,
                right,
            } => {
                let (mut left_rect, mut right_rect) = (rect, rect);
                if *is_horizontal {
                    left_rect.ymax = *position;
                    right_rect.ymin = *position;
                } else {
                    left_rect.xmax = *position;
                    right_rect.xmin = *position;
                }
                Self::read_tree_node(space, left, left_rect, report);
                Self::read_tree_node(space, right, right_rect, report);
            }
            SpaceNode::Leaf {
                addr,
                load,
                has_been_created,
            } => match space.cell_infos.get_mut(addr) {
                Some(record) => {
                    record.should_delete = false;
                    record.rect = rect;
                    record.is_delete_pending = false;
                    record.load = *load;
                    record.has_been_created = *has_been_created;
                    report.refreshed += 1;
                }
                None => {
                    space.cell_infos.insert(
                        *addr,
                        CellInfoRecord {
                            addr: *addr,
                            load: *load,
                            rect,
                            should_delete: false,
                            is_delete_pending: false,
                            has_been_created: *has_been_created,
                        },
                    );
                    report.added.push(*addr);
                }
            },
        }
    }

    /// `Space::spaceDataEntry` (`space.cpp:675-878`) — the one face every
    /// data row flows through. `update_cell_app_mgr` selects the
    /// `UPDATE_CELL_APP_MGR` branch (the manager echo goes out and the
    /// recently-deleted guard does not run) or the broadcast branch (the
    /// guard runs, no echo). `need_effect` selects the `NEED_TO_EFFECT`
    /// tail. `now` is the app time the recent entries stamp. Returns
    /// whether the row changed anything.
    #[allow(clippy::too_many_arguments)]
    pub fn space_data_entry(
        &mut self,
        entry_id: SpaceEntryId,
        key: u16,
        value: &[u8],
        update_cell_app_mgr: bool,
        need_effect: bool,
        now: u32,
        out: &mut SpaceOutbound,
    ) -> bool {
        if update_cell_app_mgr {
            // space.cpp:682-700 — the manager echo; the replay-data copy is
            // the replay lane's.
            out.update_space_data.push((
                self.id,
                entry_id,
                key,
                (key != NO_KEY).then(|| value.to_vec()),
            ));
        } else if key < SPACE_DATA_FIRST_CELL_ONLY_KEY {
            // space.cpp:703-720 — never add back a recently deleted entry.
            if self
                .recent_data
                .iter()
                .rev()
                .any(|rde| rde.entry_id == entry_id && rde.key == NO_KEY)
            {
                return false;
            }
        }

        let has_been_added;
        let mut old_value = DataValue::invalid();
        let effective_key;
        if key != NO_KEY {
            has_been_added = self.space_data_mapping.add_data_entry(entry_id, key, value);
            effective_key = if has_been_added { key } else { NO_KEY };
        } else {
            match self.space_data_mapping.del_data_entry_with_old(entry_id) {
                Some(old) => {
                    has_been_added = false;
                    effective_key = old.key;
                    old_value = old;
                }
                None => {
                    has_been_added = false;
                    effective_key = NO_KEY;
                }
            }
        }
        if effective_key == NO_KEY {
            // WARNING_MSG "Attempted to add or delete entry twice!"
            return false;
        }

        // The script events (space.cpp:753-770).
        self.script_events.push(SpaceScriptEvent {
            is_added: has_been_added,
            key: effective_key,
            value: if has_been_added {
                value.to_vec()
            } else {
                old_value.data.clone()
            },
        });

        // Client-visible keys join the recent window
        // (space.cpp:772-785) — the key itself, `-1` included for a
        // revoked row.
        if effective_key < SPACE_DATA_FIRST_CELL_ONLY_KEY {
            self.recent_data.push(RecentDataEntry {
                entry_id,
                time: now,
                key,
            });
            self.end_data_seq += 1;
        }

        // The `NEED_TO_EFFECT` tail (space.cpp:787-875).
        if need_effect {
            if effective_key == SPACE_DATA_TOD_KEY {
                match self
                    .space_data_mapping
                    .data_retrieve_first(SPACE_DATA_TOD_KEY)
                {
                    Some(data) if data.len() >= 8 => {
                        self.initial_time_of_day =
                            f32::from_le_bytes(data[0..4].try_into().expect("four bytes"));
                        self.game_seconds_per_second =
                            f32::from_le_bytes(data[4..8].try_into().expect("four bytes"));
                    }
                    _ => {
                        self.initial_time_of_day = -1.0;
                        self.game_seconds_per_second = 0.0;
                    }
                }
            } else if effective_key == SPACE_DATA_ARTIFICIAL_LOAD {
                match self
                    .space_data_mapping
                    .data_retrieve_first(SPACE_DATA_ARTIFICIAL_LOAD)
                {
                    Some(data) if data.len() >= 4 => {
                        self.artificial_min_load =
                            f32::from_le_bytes(data[0..4].try_into().expect("four bytes"));
                    }
                    _ => {
                        self.artificial_min_load = 0.0;
                    }
                }
            } else if effective_key == SPACE_DATA_MAPPING_KEY_CLIENT_SERVER {
                if has_been_added {
                    // space.cpp:827-845 — the payload is the 4x4 matrix
                    // followed by the path.
                    let path = if value.len() >= 64 {
                        String::from_utf8_lossy(&value[64..]).into_owned()
                    } else {
                        String::new()
                    };
                    self.physical.load_resource(entry_id, &path);
                    self.should_check_load_bounds = true;
                    // Someone else mapped a geometry: our last-mapped mark
                    // cannot be the last any more.
                    self.last_mapped_geometry.clear();
                } else {
                    self.physical.unload_resource(entry_id);
                }
            }
            // `SPACE_DATA_RECORDING` starts a replay recording
            // (`space.cpp:858-874`) — the replay lane's face; the row
            // itself lands in the mapping all the same.
        }
        true
    }

    /// `Space::spaceData` (`space.cpp:648-659`), the message handler
    /// another CellApp streams through: no manager echo, the effect runs.
    pub fn space_data(
        &mut self,
        entry_id: SpaceEntryId,
        key: u16,
        value: &[u8],
        now: u32,
        out: &mut SpaceOutbound,
    ) -> bool {
        self.space_data_entry(entry_id, key, value, false, true, now, out)
    }

    /// `Space::allSpaceData` (`space.cpp:666-669`), the manager's
    /// catch-up stream: every row through the same face, no echo.
    pub fn all_space_data(
        &mut self,
        entries: &[(SpaceEntryId, u16, Vec<u8>)],
        now: u32,
        out: &mut SpaceOutbound,
    ) -> usize {
        let mut applied = 0;
        for (entry_id, key, value) in entries {
            if self.space_data_entry(*entry_id, *key, value, false, true, now, out) {
                applied += 1;
            }
        }
        applied
    }

    /// `Space::dataBySeq` (`space.cpp:593-619`): the row a local sequence
    /// number names — `None` outside the window, and `None` data for a
    /// row since revoked (or since unloaded from the mapping).
    pub fn data_by_seq(&self, seq: i32) -> Option<(SpaceEntryId, u16, Option<Vec<u8>>)> {
        if seq < self.beg_data_seq || seq >= self.end_data_seq {
            return None;
        }
        let rde = &self.recent_data[(seq - self.beg_data_seq) as usize];
        if rde.key != NO_KEY {
            let data = self
                .space_data_mapping
                .data_retrieve_specific(rde.entry_id)
                .filter(|value| value.valid())
                .map(|value| value.data.clone());
            Some((rde.entry_id, rde.key, data))
        } else {
            Some((rde.entry_id, rde.key, None))
        }
    }

    /// `Space::dataRecencyLevel` (`space.cpp:626-640`): 0 under 10
    /// seconds, 1 under 20, 3 under 30, else 4 — `None` outside the
    /// window.
    pub fn data_recency_level(&self, seq: i32, now: u32, hertz: u32) -> Option<i32> {
        if seq < self.beg_data_seq || seq >= self.end_data_seq {
            return None;
        }
        let rde = &self.recent_data[(seq - self.beg_data_seq) as usize];
        let diff = now.saturating_sub(rde.time) as i32;
        let hertz = hertz as i32;
        if diff < hertz * 10 {
            Some(0)
        } else if diff < hertz * 20 {
            Some(1)
        } else if diff < hertz * 30 {
            Some(3)
        } else {
            Some(4)
        }
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
        // space.cpp:126-130 — the cell-info tree goes with the space.
        self.tree_root = None;
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

    /// The mutable half of [`Spaces::iter`].
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Space> {
        self.container.values_mut()
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

// -----------------------------------------------------------------------------
// Section: The cell registry
// -----------------------------------------------------------------------------

/// The CellApp's collection of `Cell`s (`server/cellapp/cells.hpp`). The
/// reference's `container_` maps space ids to the heap `Cell`s, with each
/// `Space` holding a raw `pCell_` back-pointer; the port keeps each cell
/// inside its space, so the registry holds the registered keys — the two
/// views agreeing is the invariant every operation here maintains.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cells {
    /// `container_`, ordered by space id like the reference's `BW::map`.
    container: BTreeSet<SpaceId>,
}

impl Cells {
    /// A fresh, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// `Cells::add` (`cells.cpp:159-166`): register one cell per space. The
    /// reference asserts the space is not registered; the port refuses a
    /// duplicate id and keeps the original.
    pub fn add(&mut self, space_id: SpaceId) -> bool {
        self.container.insert(space_id)
    }

    /// `Cells::destroy` (`cells.cpp:44-61`): drop a registered cell.
    /// Returns whether it was registered.
    pub fn destroy(&mut self, space_id: SpaceId) -> bool {
        self.container.remove(&space_id)
    }

    /// Whether the space has its cell registered.
    pub fn contains(&self, space_id: SpaceId) -> bool {
        self.container.contains(&space_id)
    }

    /// `Cells::size`.
    pub fn len(&self) -> usize {
        self.container.len()
    }

    /// `Cells::empty`.
    pub fn is_empty(&self) -> bool {
        self.container.is_empty()
    }

    /// The registered space ids, in id order.
    pub fn iter(&self) -> impl Iterator<Item = SpaceId> + '_ {
        self.container.iter().copied()
    }
}

// -----------------------------------------------------------------------------
// Section: The CellApp
// -----------------------------------------------------------------------------

/// What [`CellApp::add_cell`] did — the `CellApp::addCell` branches
/// (`cellapp.cpp:1607-1666`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddCellOutcome {
    /// The `CRITICAL_MSG "Failed to add a cell for space %u"` branch: the
    /// space's mirror holds no `CellInfo` for this app. The space itself has
    /// been created or reused and its mirror refreshed — only the cell is
    /// refused.
    NoCellInfo,
    /// A fresh `Cell` was made and registered
    /// (`new Cell( *pSpace, *pCellInfo )` + `cells_.add`).
    Created {
        /// How the space itself was acquired (`Spaces::acquire`).
        space: SpaceAcquisition,
    },
    /// The `WARNING_MSG "Cell did not fully remove; reusing."` branch: the
    /// space still holds its cell and `Cell::reuse` revives it.
    Reused {
        /// How the space itself was acquired (`Spaces::acquire`).
        space: SpaceAcquisition,
    },
}

/// The CellApp-side state this module tracks (`CellApp`, `cellapp.cpp`):
/// the spaces this process serves, the cells registered on them, and the
/// configuration context their lifecycles read.
#[derive(Clone, Debug, Default)]
pub struct CellApp {
    /// `pSpaces_`.
    spaces: Spaces,
    /// `cells_`.
    cells: Cells,
    /// The `CellApp::instance()` state the lifecycles read.
    context: CellAppContext,
}

impl CellApp {
    /// A fresh app under the given configuration.
    pub fn new(context: CellAppContext) -> Self {
        Self {
            spaces: Spaces::new(),
            cells: Cells::new(),
            context,
        }
    }

    /// The spaces this process serves.
    pub fn spaces(&self) -> &Spaces {
        &self.spaces
    }

    /// The mutable half of [`CellApp::spaces`].
    pub fn spaces_mut(&mut self) -> &mut Spaces {
        &mut self.spaces
    }

    /// `cells_`.
    pub fn cells(&self) -> &Cells {
        &self.cells
    }

    /// The configuration context.
    pub fn context(&self) -> &CellAppContext {
        &self.context
    }

    /// `CellApp::addCell` (`cellapp.cpp:1584-1667`), without the reply and
    /// the `allSpaceData`/`onGetFirstCell` faces: acquire the space — the
    /// reuse branch runs `Space::reuse` — refresh the cell-info mirror,
    /// then create or revive this app's cell on it.
    pub fn add_cell(
        &mut self,
        space_id: SpaceId,
        geometry: &[CellInfoRecord],
        own_addr: Addr,
        delegate_mode: bool,
    ) -> AddCellOutcome {
        let (acquisition, space) = self.spaces.acquire(space_id, delegate_mode);
        if matches!(acquisition, SpaceAcquisition::Reused) {
            space.reuse();
        }
        space.set_cell_infos(geometry);

        let info_addr = match space.find_cell(own_addr) {
            Some(info) => info.addr,
            None => {
                // CRITICAL_MSG "Failed to add a cell for space %u"
                return AddCellOutcome::NoCellInfo;
            }
        };

        if let Some(cell) = space.cell_mut() {
            // WARNING_MSG "Cell did not fully remove; reusing." — the
            // reference asserts the surviving cell is the same one.
            debug_assert_eq!(cell.addr, info_addr);
            cell.reuse();
            return AddCellOutcome::Reused { space: acquisition };
        }

        space.set_cell(info_addr);
        let newly_registered = self.cells.add(space_id);
        debug_assert!(newly_registered);
        AddCellOutcome::Created { space: acquisition }
    }

    /// `Cell::retireCell` for the cell of `space_id`: set or clear the
    /// retiring flag. Returns whether the cell exists.
    pub fn retire_cell(&mut self, space_id: SpaceId, is_retiring: bool) -> bool {
        let Some(cell) = self.cell_of_mut(space_id) else {
            return false;
        };
        cell.retire(is_retiring);
        true
    }

    /// `Cell::removeCell` for the cell of `space_id`: mark removed and
    /// collect the neighbour acks. Returns whether the cell exists.
    pub fn remove_cell(&mut self, space_id: SpaceId, addrs: &[Addr]) -> bool {
        let Some(cell) = self.cell_of_mut(space_id) else {
            return false;
        };
        cell.remove(addrs);
        true
    }

    /// `Cell::notifyOfCellRemoval` (`cell.cpp:694-707`) for the cell this
    /// app holds in `space_id`: reply `ackCellRemoval` to the leaving cell
    /// and mark its `CellInfo` pending-delete in this app's mirror of the
    /// space. Returns whether this app holds a cell there.
    pub fn notify_of_cell_removal(
        &mut self,
        space_id: SpaceId,
        removed_addr: Addr,
        out: &mut CellAppOutbound,
    ) -> bool {
        let Some(space) = self.spaces.find_mut(space_id) else {
            return false;
        };
        if !space.has_cell() {
            return false;
        }
        out.ack_cell_removals.push((removed_addr, space_id));
        space.set_pending_cell_delete(removed_addr);
        true
    }

    /// `Cell::ackCellRemoval` for the cell of `space_id`. Returns whether
    /// the cell exists.
    pub fn ack_cell_removal(&mut self, space_id: SpaceId, src: Addr) -> bool {
        let Some(cell) = self.cell_of_mut(space_id) else {
            return false;
        };
        cell.ack_removal(src);
        true
    }

    /// `Cells::handleCellAppDeath` (`cells.cpp:96-106`): a dead app owes no
    /// acks anywhere.
    pub fn handle_cell_app_death(&mut self, addr: Addr) {
        for space in self.spaces.iter_mut() {
            if let Some(cell) = space.cell_mut() {
                cell.handle_app_death(addr);
            }
        }
    }

    /// `Cell::isReadyForDeletion` for the cell of `space_id`: `None` when
    /// this app holds no cell there.
    pub fn is_cell_ready_for_deletion(&self, space_id: SpaceId) -> Option<bool> {
        let space = self.spaces.find(space_id)?;
        let cell = space.cell()?;
        Some(cell.is_ready_for_deletion(space.num_entities() == 0))
    }

    /// `Cells::checkOffloads` (`cells.cpp:115-143`): destroy every cell
    /// that is ready for deletion — removed, its space empty and no acks
    /// pending — and report the destroyed space ids.
    pub fn check_offloads(&mut self) -> Vec<SpaceId> {
        let doomed: Vec<SpaceId> = self
            .cells
            .iter()
            .filter(|id| self.is_cell_ready_for_deletion(*id) == Some(true))
            .collect();
        for id in &doomed {
            self.destroy_cell(*id);
        }
        doomed
    }

    /// `CellApp::destroyCell` (`cellapp.cpp:1487-1490`) — `Cells::destroy`
    /// plus `~Cell`'s `space_.pCell( NULL )` (`cell.cpp:130`). Returns
    /// whether the cell was registered.
    pub fn destroy_cell(&mut self, space_id: SpaceId) -> bool {
        if !self.cells.destroy(space_id) {
            return false;
        }
        if let Some(space) = self.spaces.find_mut(space_id) {
            space.clear_cell();
        }
        true
    }

    /// `Space::handleTimeout`'s destroy, driven from the app: the expiry
    /// runs on the space and a destroyed cell unregisters, exactly as the
    /// reference's `destroyCell` call does. Returns whether the space
    /// exists.
    pub fn handle_space_timeout(&mut self, space_id: SpaceId) -> bool {
        let Some(space) = self.spaces.find_mut(space_id) else {
            return false;
        };
        let destroyed = space.handle_timeout();
        if destroyed {
            self.cells.destroy(space_id);
        }
        true
    }

    /// `Cells::destroyAll` (`cells.cpp:64-74`): the shutdown drain — every
    /// registered cell goes, whatever state it is in. Returns how many.
    pub fn destroy_all_cells(&mut self) -> usize {
        let ids: Vec<SpaceId> = self.cells.iter().collect();
        for id in &ids {
            self.destroy_cell(*id);
        }
        ids.len()
    }

    /// `Space::spaceDataEntry` routed through the app's spaces: `None`
    /// when no such space lives here, the space's answer otherwise
    /// (the manager echo already collected in `out`).
    #[allow(clippy::too_many_arguments)]
    pub fn space_data_entry(
        &mut self,
        space_id: SpaceId,
        entry_id: SpaceEntryId,
        key: u16,
        value: &[u8],
        update_cell_app_mgr: bool,
        need_effect: bool,
        now: u32,
        out: &mut SpaceOutbound,
    ) -> Option<bool> {
        Some(self.spaces_mut().find_mut(space_id)?.space_data_entry(
            entry_id,
            key,
            value,
            update_cell_app_mgr,
            need_effect,
            now,
            out,
        ))
    }

    /// `Space::spaceData` routed: another CellApp's broadcast row — no
    /// manager echo, the effect runs.
    pub fn space_data(
        &mut self,
        space_id: SpaceId,
        entry_id: SpaceEntryId,
        key: u16,
        value: &[u8],
        now: u32,
        out: &mut SpaceOutbound,
    ) -> Option<bool> {
        Some(
            self.spaces_mut()
                .find_mut(space_id)?
                .space_data(entry_id, key, value, now, out),
        )
    }

    /// `Space::allSpaceData` routed: the manager's catch-up stream.
    pub fn all_space_data(
        &mut self,
        space_id: SpaceId,
        entries: &[(SpaceEntryId, u16, Vec<u8>)],
        now: u32,
        out: &mut SpaceOutbound,
    ) -> Option<usize> {
        Some(
            self.spaces_mut()
                .find_mut(space_id)?
                .all_space_data(entries, now, out),
        )
    }

    /// `Space::updateGeometry` routed: the app's context feeds the
    /// `wasMulticell` shutdown tail.
    pub fn update_geometry(
        &mut self,
        space_id: SpaceId,
        root: &SpaceNode,
        out: &mut SpaceOutbound,
    ) -> Option<UpdateGeometryReport> {
        let context = self.context;
        Some(
            self.spaces_mut()
                .find_mut(space_id)?
                .update_geometry(root, &context, out),
        )
    }

    /// The cell of `space_id` — the registered-cell read accessor.
    pub fn cell_of(&self, space_id: SpaceId) -> Option<&Cell> {
        self.spaces.find(space_id)?.cell()
    }

    /// The cell of `space_id`, mutable — the registered-cell accessor.
    fn cell_of_mut(&mut self, space_id: SpaceId) -> Option<&mut Cell> {
        self.spaces.find_mut(space_id)?.cell_mut()
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
                mappings: BTreeMap::new()
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
                mappings.insert((1, 5000), "space.center".to_owned());
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
            mappings.insert((1, 5000), "space.center".to_owned());
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
                mappings: BTreeMap::new()
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
                mappings: BTreeMap::new()
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

    // -------------------------------------------------------------------
    // TDD: BW-08-01-02 创建与注册 Cell — 管理器和 CellApp 状态一致
    // -------------------------------------------------------------------

    /// The `CellApp::addCell` registration: the cell lands in the registry,
    /// the space holds it as its local cell, and the manager's and the
    /// CellApp's views of the space's cells agree — the acceptance.
    #[test]
    fn the_manager_and_the_cellapp_agree_on_a_spaces_cells() {
        use crate::cellappmgr::{AddAppOutcome, CellAppMgr, Outbound, Space as MgrSpace};

        let app1_addr = (0x7f000001, 40001);
        let app2_addr = (0x7f000001, 40002);
        let space_id = 1;

        // The manager's side: one space, first cell on the first app.
        let mut mgr = CellAppMgr::new();
        mgr.set_base_app((0x7f000001, 9000));
        mgr.set_db_app_alpha((0x7f000001, 9001));
        mgr.allow_new_cell_apps = true;
        assert!(matches!(
            mgr.add_app(app1_addr, 0, 0),
            AddAppOutcome::Accepted { .. }
        ));
        assert!(matches!(
            mgr.add_app(app2_addr, 0, 0),
            AddAppOutcome::Accepted { .. }
        ));
        mgr.add_space(MgrSpace::new(space_id, false, 0));
        assert_eq!(mgr.add_cell_for_app(space_id, app1_addr), Some(app1_addr));
        assert_eq!(mgr.outbox, vec![Outbound::AddCell(app1_addr, space_id)]);

        // The CellApp side: the geometry the addCell message carries is the
        // manager's cells of the space.
        let geometry = |mgr: &CellAppMgr| -> Vec<CellInfoRecord> {
            mgr.spaces[&space_id]
                .cells()
                .iter()
                .map(|leaf| CellInfoRecord::new(leaf.app, leaf.range))
                .collect()
        };
        let mut app1 = CellApp::new(started());
        assert_eq!(
            app1.add_cell(space_id, &geometry(&mgr), app1_addr, false),
            AddCellOutcome::Created {
                space: SpaceAcquisition::Created
            }
        );

        // Both views hold exactly the one cell, on the same app.
        assert_eq!(mgr.spaces[&space_id].num_cells(), 1);
        assert_eq!(app1.cells().len(), 1);
        assert!(app1.cells().contains(space_id));
        let space = app1.spaces().find(space_id).expect("the space");
        assert_eq!(space.cell().expect("the local cell").addr, app1_addr);

        // A second cell on a second app: both sides grow to two.
        assert_eq!(mgr.add_cell_for_app(space_id, app2_addr), Some(app2_addr));
        let two_cell_geometry = geometry(&mgr);
        let mut app2 = CellApp::new(started());
        assert_eq!(
            app2.add_cell(space_id, &two_cell_geometry, app2_addr, false),
            AddCellOutcome::Created {
                space: SpaceAcquisition::Created
            }
        );
        // The first app's addCell refresh carries the same two-cell
        // geometry; its cell survives it (the reusing branch).
        assert_eq!(
            app1.add_cell(space_id, &two_cell_geometry, app1_addr, false),
            AddCellOutcome::Reused {
                space: SpaceAcquisition::Reused
            }
        );
        assert_eq!(mgr.spaces[&space_id].num_cells(), 2);
        assert_eq!(app1.cells().len() + app2.cells().len(), 2);

        // The removal, driven from the manager's view: the manager erases
        // the second app's cell from the space, and the addr list the
        // removeCell message carries is the other cells' apps
        // (`Cells::notifyOfCellRemoval`, cellappmgr/cells.cpp:83-113).
        assert!(mgr.spaces.get_mut(&space_id).unwrap().erase_cell(app2_addr));
        let neighbours: Vec<Addr> = mgr.spaces[&space_id]
            .cells()
            .iter()
            .map(|leaf| leaf.app)
            .filter(|addr| *addr != app2_addr)
            .collect();
        assert_eq!(neighbours, vec![app1_addr]);

        // The leaving app gets removeCell; the staying app gets
        // notifyOfCellRemoval, acks back and marks the leaver pending.
        assert!(app2.remove_cell(space_id, &neighbours));
        let mut out1 = CellAppOutbound::default();
        assert!(app1.notify_of_cell_removal(space_id, app2_addr, &mut out1));
        assert_eq!(out1.ack_cell_removals, vec![(app2_addr, space_id)]);
        assert!(
            app1.spaces()
                .find(space_id)
                .unwrap()
                .find_cell(app2_addr)
                .expect("the mirror record")
                .is_delete_pending
        );

        // The ack lands on the leaving cell: no acks pending, space empty —
        // ready to die.
        assert!(!app2.is_cell_ready_for_deletion(space_id).unwrap());
        assert!(app2.ack_cell_removal(space_id, app1_addr));
        assert_eq!(app2.is_cell_ready_for_deletion(space_id), Some(true));
        assert_eq!(app2.check_offloads(), vec![space_id]);

        // The end state, both views: the manager sees one cell, the first
        // app holds it and the second app holds none.
        assert_eq!(mgr.spaces[&space_id].num_cells(), 1);
        assert_eq!(app1.cells().len(), 1);
        assert!(app2.cells().is_empty());
        assert!(app2.spaces().find(space_id).unwrap().cell().is_none());
    }

    /// The `CRITICAL_MSG "Failed to add a cell for space %u"` branch
    /// (`cellapp.cpp:1662-1666`): a space with no `CellInfo` for this app
    /// gets no cell — but the space itself exists and holds the geometry.
    #[test]
    fn add_cell_refuses_without_a_cell_info() {
        let mut app = CellApp::new(started());
        let other_geometry = [CellInfoRecord::new(
            (0x7f000001, 5000),
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert_eq!(
            app.add_cell(4, &other_geometry, (0x7f000001, 6000), false),
            AddCellOutcome::NoCellInfo
        );
        let space = app.spaces().find(4).expect("the space exists");
        assert!(!space.has_cell());
        assert!(app.cells().is_empty());
        assert_eq!(space.cell_infos().len(), 1);
    }

    /// `Cell::reuse` (`cell.cpp:745-755`) through `add_cell`: a cell left
    /// retiring-and-removed by an aborted removal revives, and the return
    /// says whether that was the state it was in.
    #[test]
    fn add_cell_revives_a_cell_that_did_not_fully_remove() {
        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        let mut app = CellApp::new(started());

        // Create, then abort a removal mid-flight: retiring and removed.
        assert!(matches!(
            app.add_cell(7, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));
        assert!(app.retire_cell(7, true));
        assert!(app.remove_cell(7, &[(0x7f000001, 5000), (0x7f000001, 5001)]));
        assert!(app.cell_of(7).expect("the cell").is_retiring());
        // One neighbour acked; the other still pends, so the cell lingers.
        assert!(app.ack_cell_removal(7, (0x7f000001, 5000)));
        assert!(
            app.cell_of(7)
                .unwrap()
                .pending_acks()
                .contains(&(0x7f000001, 5001))
        );

        // The re-add revives the cell in place — same registration.
        assert_eq!(
            app.add_cell(7, &geometry, own_addr, false),
            AddCellOutcome::Reused {
                space: SpaceAcquisition::Reused
            }
        );
        assert_eq!(app.cells().len(), 1);
        let cell = app.cell_of(7).expect("the revived cell");
        assert!(!cell.is_retiring());
        assert!(!cell.is_removed());
        // `Cell::reuse` resets only the two flags (`cell.cpp:745-755`) —
        // the pending ack of the unfinished removal survives it.
        assert!(cell.pending_acks().contains(&(0x7f000001, 5001)));
    }

    /// `Cell::retireCell` (`cell.cpp:638-652`): the flag sets and clears,
    /// on the registered cell of the space.
    #[test]
    fn retire_cell_toggles_the_flag() {
        let mut app = CellApp::new(started());
        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert!(matches!(
            app.add_cell(8, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));

        assert!(app.retire_cell(8, true));
        assert!(app.cell_of(8).unwrap().is_retiring());
        assert!(app.retire_cell(8, false));
        assert!(!app.cell_of(8).unwrap().is_retiring());

        // A space with no cell answers false.
        assert!(!app.retire_cell(99, true));
    }

    /// `Cell::removeCell` (`cell.cpp:661-685`) + `Cell::ackCellRemoval`
    /// (`cell.cpp:714-739`) + `Cells::checkOffloads` (`cells.cpp:115-143`):
    /// the removal only completes once every neighbour has acked.
    #[test]
    fn a_removed_cell_dies_only_after_every_neighbour_acked() {
        let mut app = CellApp::new(started());
        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert!(matches!(
            app.add_cell(9, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));

        let neighbours = [(0x7f000001, 5000), (0x7f000001, 5001)];
        assert!(app.remove_cell(9, &neighbours));
        assert!(app.cell_of(9).unwrap().is_removed());

        // Removed, space empty, but one ack still pending: not ready.
        assert!(app.ack_cell_removal(9, neighbours[0]));
        assert_eq!(app.is_cell_ready_for_deletion(9), Some(false));
        assert!(app.check_offloads().is_empty());
        assert!(app.cells().contains(9));

        // The last ack completes it.
        assert!(app.ack_cell_removal(9, neighbours[1]));
        assert_eq!(app.is_cell_ready_for_deletion(9), Some(true));
        assert_eq!(app.check_offloads(), vec![9]);

        // The registry and the space slot cleared together.
        assert!(app.cells().is_empty());
        assert!(app.spaces().find(9).unwrap().cell().is_none());
        // A further removal of the dead cell is refused.
        assert!(!app.remove_cell(9, &neighbours));
    }

    /// `Cell::ackCellRemoval`'s bank branch (`cell.cpp:729-738`): an ack
    /// that arrives before the removal is banked and then consumed by it.
    #[test]
    fn an_early_ack_is_banked_and_consumed_by_the_removal() {
        let mut app = CellApp::new(started());
        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert!(matches!(
            app.add_cell(10, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));

        // The neighbour acks before the manager's removal arrives.
        let neighbour = (0x7f000001, 5000);
        assert!(app.ack_cell_removal(10, neighbour));
        assert!(
            app.cell_of(10)
                .unwrap()
                .received_acks()
                .contains(&neighbour)
        );

        // The removal consumes the banked ack — nothing pending.
        assert!(app.remove_cell(10, &[neighbour]));
        assert!(app.cell_of(10).unwrap().pending_acks().is_empty());
        assert_eq!(app.is_cell_ready_for_deletion(10), Some(true));
        assert_eq!(app.check_offloads(), vec![10]);
    }

    /// `Cell::isReadyForDeletion`'s space gate (`cell.cpp:156`): the
    /// space's entities pin the cell even with every ack in.
    #[test]
    fn a_cell_with_space_entities_is_not_ready_for_deletion() {
        let mut app = CellApp::new(started());
        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert!(matches!(
            app.add_cell(11, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));
        app.spaces_mut().find_mut(11).unwrap().add_entity(61, false);

        assert!(app.remove_cell(11, &[]));
        assert_eq!(app.is_cell_ready_for_deletion(11), Some(false));
        assert!(app.check_offloads().is_empty());

        // The entity leaving releases the cell.
        let context = started();
        let mut out = SpaceOutbound::default();
        assert!(
            app.spaces_mut()
                .find_mut(11)
                .unwrap()
                .remove_entity(61, &context, &mut out)
        );
        assert_eq!(app.check_offloads(), vec![11]);
    }

    /// `Cell::handleCellAppDeath` (`cell.cpp:762-768`) through
    /// `Cells::handleCellAppDeath` (`cells.cpp:96-106`): a dead app's acks
    /// vanish from every cell of the app.
    #[test]
    fn a_dead_app_loses_the_acks_it_owed() {
        let mut app = CellApp::new(started());
        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert!(matches!(
            app.add_cell(12, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));

        let dead = (0x7f000001, 5000);
        assert!(app.remove_cell(12, &[dead]));
        assert_eq!(app.is_cell_ready_for_deletion(12), Some(false));

        app.handle_cell_app_death(dead);
        assert!(app.cell_of(12).unwrap().pending_acks().is_empty());
        assert_eq!(app.is_cell_ready_for_deletion(12), Some(true));
    }

    /// `CellApp::destroyCell` + `Cells::destroyAll` (`cells.cpp:64-74`): the
    /// shutdown drain unregisters every cell and clears every slot,
    /// whatever state the cells are in.
    #[test]
    fn destroy_all_cells_drains_the_registry_and_the_slots() {
        let mut app = CellApp::new(started());
        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert!(matches!(
            app.add_cell(13, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));
        assert!(matches!(
            app.add_cell(14, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));
        // One cell mid-removal, one clean.
        assert!(app.remove_cell(13, &[(0x7f000001, 5000)]));

        assert_eq!(app.destroy_all_cells(), 2);
        assert!(app.cells().is_empty());
        for id in [13, 14] {
            assert!(app.spaces().find(id).unwrap().cell().is_none());
        }
        // The spaces themselves stay, cellless.
        assert_eq!(app.spaces().len(), 2);
    }

    /// `Space::handleTimeout`'s destroy, driven from the app
    /// (`Space::handleTimeout`, `space.cpp:1037-1050`): a destroyed cell
    /// unregisters, so the registry and the slot agree.
    #[test]
    fn the_space_shutdown_timer_unregisters_the_destroyed_cell() {
        let mut app = CellApp::new(started());
        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert!(matches!(
            app.add_cell(15, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));

        assert!(app.handle_space_timeout(15));
        assert!(app.cells().is_empty());
        assert!(app.spaces().find(15).unwrap().cell().is_none());
        // The space stays until `deleteOldSpaces` reclaims it, exactly as
        // the -01 chain pins.
        assert_eq!(app.spaces_mut().delete_old_spaces(), vec![15]);
    }

    /// The remaining lifecycle edges: the chunk-backend default, the
    /// config accessor, the space iterators, the duplicate-entity refusal
    /// (`Space::addEntity`'s `MF_ASSERT` face) and the false answers of the
    /// registry lookups on unknown spaces and cellless spaces.
    #[test]
    fn the_remaining_lifecycle_edges_answer_false_and_walk_the_map() {
        assert_eq!(
            PhysicalSpace::default(),
            PhysicalSpace::Chunk {
                mappings: BTreeMap::new()
            }
        );

        let mut app = CellApp::new(started());
        assert!(app.context().has_started);
        assert!(!app.context().use_default_space);

        let own_addr = (0x7f000001, 6000);
        let geometry = [CellInfoRecord::new(
            own_addr,
            crate::cellappmgr::Rect::unbounded(),
        )];
        assert!(matches!(
            app.add_cell(20, &geometry, own_addr, false),
            AddCellOutcome::Created { .. }
        ));
        app.spaces_mut().create(21, false);

        // The iterators walk every served space in id order.
        assert_eq!(
            app.spaces().iter().map(|s| s.id()).collect::<Vec<_>>(),
            vec![20, 21]
        );
        assert_eq!(
            app.spaces_mut()
                .iter_mut()
                .map(|s| s.id())
                .collect::<Vec<_>>(),
            vec![20, 21]
        );

        // The duplicate-entity add is refused.
        let space = app.spaces_mut().find_mut(20).unwrap();
        assert_eq!(space.add_entity(70, false), Some(0));
        assert_eq!(space.add_entity(70, false), None);

        // Unknown records, unknown spaces and cellless spaces refuse.
        assert!(
            !app.spaces_mut()
                .find_mut(20)
                .unwrap()
                .set_pending_cell_delete((9, 9))
        );
        assert!(!app.notify_of_cell_removal(99, (9, 9), &mut CellAppOutbound::default()));
        assert!(!app.notify_of_cell_removal(21, (9, 9), &mut CellAppOutbound::default()));
        assert!(!app.ack_cell_removal(99, (9, 9)));
        assert!(!app.destroy_cell(99));
        assert!(!app.handle_space_timeout(99));
    }

    // -------------------------------------------------------------------
    // TDD: BW-08-01-03 传播空间数据和拓扑变化 — 多 Space 不串数据
    // -------------------------------------------------------------------

    /// `SpaceNode::Leaf` shorthand for the tree fixtures.
    fn leaf(addr: Addr) -> SpaceNode {
        SpaceNode::Leaf {
            addr,
            load: 0.0,
            has_been_created: true,
        }
    }

    /// The `SpaceData_ToDData` payload: initial time-of-day then
    /// game-seconds-per-second.
    fn tod_payload(initial: f32, rate: f32) -> Vec<u8> {
        let mut data = initial.to_le_bytes().to_vec();
        data.extend_from_slice(&rate.to_le_bytes());
        data
    }

    /// The acceptance: two spaces served by one app each carry their own
    /// rows — a row in one never lands in the other, and every manager
    /// echo is tagged with its own space.
    #[test]
    fn two_spaces_keep_their_data_rows_separate() {
        let mut app = CellApp::new(started());
        let geometry = vec![CellInfoRecord::new((10, 1), Rect::unbounded())];
        assert!(matches!(
            app.add_cell(7, &geometry, (10, 1), false),
            AddCellOutcome::Created { .. }
        ));
        assert!(matches!(
            app.add_cell(8, &geometry, (10, 1), false),
            AddCellOutcome::Created { .. }
        ));

        let mut out = SpaceOutbound::default();
        assert!(
            app.space_data_entry(
                7,
                (1, 5000),
                SPACE_DATA_TOD_KEY,
                &tod_payload(6.5, 1.0),
                true,
                true,
                100,
                &mut out
            )
            .unwrap()
        );
        assert!(
            app.space_data_entry(
                8,
                (1, 5000),
                SPACE_DATA_TOD_KEY,
                &tod_payload(3.25, 2.0),
                true,
                true,
                100,
                &mut out
            )
            .unwrap()
        );
        assert!(
            app.space_data_entry(7, (2, 5001), 9, b"seven", true, true, 100, &mut out)
                .unwrap()
        );

        // The echoes name their own space.
        assert_eq!(out.update_space_data.len(), 3);
        assert_eq!(out.update_space_data[0].0, 7);
        assert_eq!(out.update_space_data[1].0, 8);
        assert_eq!(out.update_space_data[2].0, 7);

        // Each space reads only its own rows.
        {
            let (seven, eight) = (app.spaces().find(7).unwrap(), app.spaces().find(8).unwrap());
            assert_eq!(
                seven
                    .space_data_mapping()
                    .data_retrieve_first(SPACE_DATA_TOD_KEY),
                Some(tod_payload(6.5, 1.0).as_slice())
            );
            assert_eq!(
                seven.space_data_mapping().data_retrieve_first(9),
                Some(b"seven".as_slice())
            );
            assert_eq!(
                eight
                    .space_data_mapping()
                    .data_retrieve_first(SPACE_DATA_TOD_KEY),
                Some(tod_payload(3.25, 2.0).as_slice())
            );
            assert_eq!(eight.space_data_mapping().data_retrieve_first(9), None);
            assert_eq!(seven.initial_time_of_day(), 6.5);
            assert_eq!(eight.initial_time_of_day(), 3.25);
        }

        // Deleting the row in space 7 leaves space 8's alone.
        assert!(
            app.space_data_entry(7, (1, 5000), NO_KEY, b"", true, true, 100, &mut out)
                .unwrap()
        );
        {
            let (seven, eight) = (app.spaces().find(7).unwrap(), app.spaces().find(8).unwrap());
            assert_eq!(
                seven
                    .space_data_mapping()
                    .data_retrieve_first(SPACE_DATA_TOD_KEY),
                None
            );
            assert_eq!(seven.initial_time_of_day(), -1.0);
            assert_eq!(
                eight
                    .space_data_mapping()
                    .data_retrieve_first(SPACE_DATA_TOD_KEY),
                Some(tod_payload(3.25, 2.0).as_slice())
            );
            assert_eq!(eight.initial_time_of_day(), 3.25);
        }
    }

    /// `Space::spaceDataEntry` (`space.cpp:720-750`): one row per entry id
    /// — a second add or a delete of nothing is the "twice" rejection that
    /// stops before the script events; the manager echoes still go out.
    #[test]
    fn adding_the_same_entry_twice_and_deleting_nothing_change_nothing() {
        let mut space = Space::new(3, false);
        let mut out = SpaceOutbound::default();
        assert!(space.space_data_entry((1, 5000), 7, b"a", true, true, 100, &mut out));
        assert!(!space.space_data_entry((1, 5000), 8, b"b", true, true, 100, &mut out));
        assert!(!space.space_data_entry((2, 5001), NO_KEY, b"", true, true, 100, &mut out));
        assert_eq!(space.space_data_mapping().len(), 1);
        assert_eq!(
            space.space_data_mapping().data_retrieve_first(7),
            Some(b"a".as_slice())
        );
        assert_eq!(space.script_events().len(), 1);
        assert_eq!(out.update_space_data.len(), 3);
    }

    /// The recently-deleted guard (`space.cpp:703-720`): a broadcast add
    /// for an entry deleted inside the window is refused; the
    /// `UPDATE_CELL_APP_MGR` echo branch skips the guard; a cell-only key
    /// never consults the window at all.
    #[test]
    fn a_recently_deleted_entry_stays_deleted_until_the_manager_echo() {
        let mut space = Space::new(3, false);
        let mut out = SpaceOutbound::default();
        assert!(space.space_data_entry((4, 5002), 7, b"row", true, true, 100, &mut out));
        assert!(space.space_data_entry((4, 5002), NO_KEY, b"", true, true, 100, &mut out));
        // The delete echo carries no blob, like the reference message.
        assert_eq!(out.update_space_data.last().unwrap().3, None);

        assert!(!space.space_data_entry((4, 5002), 7, b"again", false, true, 100, &mut out));
        assert!(space.space_data_mapping().data_retrieve_first(7).is_none());

        // The manager echo path skips the guard.
        assert!(space.space_data_entry((4, 5002), 7, b"again", true, true, 100, &mut out));
        assert_eq!(
            space.space_data_mapping().data_retrieve_first(7),
            Some(b"again".as_slice())
        );

        // A cell-only key bypasses the guard both ways.
        assert!(space.space_data_entry(
            (5, 5003),
            SPACE_DATA_ARTIFICIAL_LOAD,
            &4.5f32.to_le_bytes(),
            true,
            true,
            100,
            &mut out
        ));
        assert!(space.space_data_entry((5, 5003), NO_KEY, b"", true, true, 100, &mut out));
        assert!(space.space_data_entry(
            (5, 5003),
            SPACE_DATA_ARTIFICIAL_LOAD,
            &1.0f32.to_le_bytes(),
            false,
            true,
            100,
            &mut out
        ));
        assert_eq!(space.artificial_min_load(), 1.0);
    }

    /// The script events (`space.cpp:753-770`): an add names the row; a
    /// delete names the deleted key and its old value, and lands in the
    /// recent window under `NO_KEY`.
    #[test]
    fn script_events_carry_the_row_and_its_deleted_value() {
        let mut space = Space::new(3, false);
        let mut out = SpaceOutbound::default();
        assert!(space.space_data_entry((1, 5000), 7, b"payload", false, true, 100, &mut out));
        assert!(space.space_data_entry((1, 5000), NO_KEY, b"", false, true, 100, &mut out));
        let events = space.script_events();
        assert_eq!(events.len(), 2);
        assert!(events[0].is_added);
        assert_eq!(events[0].key, 7);
        assert_eq!(events[0].value, b"payload");
        assert!(!events[1].is_added);
        assert_eq!(events[1].key, 7);
        assert_eq!(events[1].value, b"payload");
        // The add's sequence still names its entry, but the mapping row is
        // gone — the data answers empty, exactly the reference's miss.
        assert_eq!(space.data_by_seq(0), Some(((1, 5000), 7, None)));
        assert_eq!(space.data_by_seq(1), Some(((1, 5000), NO_KEY, None)));
    }

    /// `Space::dataBySeq` (`space.cpp:593-619`): the window bounds the
    /// sequence numbers; a row revoked from the mapping since answers with
    /// no data.
    #[test]
    fn the_sequence_window_bounds_data_by_seq() {
        let mut space = Space::new(3, false);
        let mut out = SpaceOutbound::default();
        assert!(space.space_data((1, 5000), 7, b"one", 100, &mut out));
        assert!(space.space_data((2, 5001), 8, b"two", 100, &mut out));
        assert_eq!(
            space.data_by_seq(0),
            Some(((1, 5000), 7, Some(b"one".to_vec())))
        );
        assert_eq!(
            space.data_by_seq(1),
            Some(((2, 5001), 8, Some(b"two".to_vec())))
        );
        assert_eq!(space.data_by_seq(2), None);
        assert_eq!(space.data_by_seq(-1), None);

        // The window only grows: the reference never moves `begDataSeq_`
        // outside the constructor and `reuse` (space.cpp:70,144), so the
        // entries keep their sequence numbers.
        assert!(space.space_data_entry((1, 5000), NO_KEY, b"", true, false, 100, &mut out));
        assert_eq!(space.data_by_seq(0), Some(((1, 5000), 7, None)));
        assert_eq!(
            space.data_by_seq(1),
            Some(((2, 5001), 8, Some(b"two".to_vec())))
        );
        assert_eq!(space.data_by_seq(2), Some(((1, 5000), NO_KEY, None)));
    }

    /// `Space::dataRecencyLevel` (`space.cpp:626-640`): the four age
    /// brackets at `hertz * 10/20/30`.
    #[test]
    fn data_recency_levels_match_the_hertz_thresholds() {
        let mut space = Space::new(3, false);
        let mut out = SpaceOutbound::default();
        assert!(space.space_data((1, 5000), 7, b"x", 1000, &mut out));
        assert_eq!(space.data_recency_level(0, 1009, 1), Some(0));
        assert_eq!(space.data_recency_level(0, 1019, 1), Some(1));
        assert_eq!(space.data_recency_level(0, 1029, 1), Some(3));
        assert_eq!(space.data_recency_level(0, 1030, 1), Some(4));
        assert_eq!(space.data_recency_level(5, 1009, 1), None);
    }

    /// The `NEED_TO_EFFECT` tail for `SPACE_DATA_TOD_KEY`
    /// (`space.cpp:787-810`): the payload drives the clock; a delete or a
    /// truncated payload resets it to the constructor values.
    #[test]
    fn time_of_day_rows_drive_and_reset_the_space_clock() {
        let mut space = Space::new(3, false);
        let mut out = SpaceOutbound::default();
        assert!(space.space_data(
            (1, 5000),
            SPACE_DATA_TOD_KEY,
            &tod_payload(6.5, 2.0),
            100,
            &mut out
        ));
        assert_eq!(space.initial_time_of_day(), 6.5);
        assert_eq!(space.game_seconds_per_second(), 2.0);
        assert!(space.space_data((1, 5000), NO_KEY, b"", 100, &mut out));
        assert_eq!(space.initial_time_of_day(), -1.0);
        assert_eq!(space.game_seconds_per_second(), 0.0);
        assert!(space.space_data((2, 5001), SPACE_DATA_TOD_KEY, &[1, 2, 3], 100, &mut out));
        assert_eq!(space.initial_time_of_day(), -1.0);
        assert_eq!(space.game_seconds_per_second(), 0.0);
    }

    /// The `NEED_TO_EFFECT` tail for `SPACE_DATA_MAPPING_KEY_CLIENT_SERVER`
    /// (`space.cpp:827-845`): the payload tail after the 4x4 matrix is the
    /// path — load it, mark the load bounds, clear the last-mapped mark;
    /// the delete unloads.
    #[test]
    fn mapping_rows_load_and_unload_the_geometry() {
        let mut space = Space::new(3, false);
        space.set_last_mapped_geometry("maps/old.chunk");
        let mut out = SpaceOutbound::default();
        let mut payload = vec![0u8; 64];
        payload.extend_from_slice(b"maps/central.chunk");
        assert!(space.space_data(
            (1, 5000),
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            &payload,
            100,
            &mut out
        ));
        let loaded = match space.physical() {
            PhysicalSpace::Chunk { mappings } => mappings.get(&(1, 5000)).cloned(),
            _ => panic!("the chunk backend is the default"),
        };
        assert_eq!(loaded.as_deref(), Some("maps/central.chunk"));
        assert!(space.should_check_load_bounds());
        assert_eq!(space.last_mapped_geometry(), "");

        assert!(space.space_data((1, 5000), NO_KEY, b"", 100, &mut out));
        let unloaded = match space.physical() {
            PhysicalSpace::Chunk { mappings } => mappings.is_empty(),
            _ => panic!("the chunk backend is the default"),
        };
        assert!(unloaded);
        // Only the add raises the flag; the delete leaves it standing.
        assert!(space.should_check_load_bounds());
    }

    /// `Space::updateGeometry` (`space.cpp:885-958`), first read on a
    /// flat-installed mirror: refresh the surviving cell, add the fresh
    /// one, sweep the stale one — and clear a pending delete on the way.
    #[test]
    fn update_geometry_refreshes_adds_and_sweeps() {
        let mut space = Space::new(7, false);
        space.set_cell((10, 1));
        let mut own = CellInfoRecord::new((10, 1), Rect::unbounded());
        own.is_delete_pending = true;
        let stale = CellInfoRecord::new((10, 2), Rect::unbounded());
        space.set_cell_infos(&[own, stale]);

        let tree = SpaceNode::Branch {
            is_horizontal: false,
            position: 100.0,
            left: Box::new(leaf((10, 1))),
            right: Box::new(leaf((10, 3))),
        };
        let mut out = SpaceOutbound::default();
        let report = space.update_geometry(&tree, &started(), &mut out);
        assert_eq!(report.added, vec![(10, 3)]);
        assert_eq!(report.erased, vec![(10, 2)]);
        assert_eq!(report.refreshed, 1);
        assert!(!report.own_cell_going);
        assert!(!report.was_multicell, "the flat mirror reads as single");
        assert!(!report.requested_shutdown);
        assert!(out.shutdown_space.is_empty());

        let info = space.find_cell((10, 1)).unwrap();
        assert!(!info.should_delete);
        assert!(!info.is_delete_pending);
        assert_eq!(info.rect.xmax, 100.0, "a vertical branch splits x");
        assert!(space.tree_root().is_some());
        assert!(!space.has_single_cell(), "a branch tree is multicell");
    }

    /// The `wasMulticell` tail (`space.cpp:953-957`): once a tree is in,
    /// shrinking back to exactly the local cell's leaf runs
    /// `checkForShutDown`, which — no entities, app started — asks for
    /// the space's shutdown.
    #[test]
    fn shrinking_back_to_one_cell_checks_for_shutdown() {
        let mut space = Space::new(7, false);
        space.set_cell((10, 1));
        space.set_cell_infos(&[
            CellInfoRecord::new((10, 1), Rect::unbounded()),
            CellInfoRecord::new((10, 2), Rect::unbounded()),
        ]);
        let branch = SpaceNode::Branch {
            is_horizontal: true,
            position: 50.0,
            left: Box::new(leaf((10, 1))),
            right: Box::new(leaf((10, 2))),
        };
        let mut out = SpaceOutbound::default();
        let first = space.update_geometry(&branch, &started(), &mut out);
        assert!(!first.was_multicell);
        assert!(!first.requested_shutdown);

        let second = space.update_geometry(&leaf((10, 1)), &started(), &mut out);
        assert!(second.was_multicell);
        assert!(second.requested_shutdown);
        assert_eq!(out.shutdown_space, vec![7]);
        assert!(space.has_single_cell(), "the leaf is the local cell");
    }

    /// The "our cell is going" check (`space.cpp:939-951`): a sweep that
    /// takes the local cell's record reports it — and the shutdown tail
    /// re-reads `hasSingleCell`, which is false for a tree naming
    /// another cell, so no shutdown is requested.
    #[test]
    fn losing_our_own_cell_in_a_multicell_space_reports_it_without_shutdown() {
        let mut space = Space::new(7, false);
        space.set_cell((10, 1));
        space.set_cell_infos(&[CellInfoRecord::new((10, 1), Rect::unbounded())]);
        let branch = SpaceNode::Branch {
            is_horizontal: false,
            position: 100.0,
            left: Box::new(leaf((10, 1))),
            right: Box::new(leaf((10, 2))),
        };
        let mut out = SpaceOutbound::default();
        assert!(
            !space
                .update_geometry(&branch, &started(), &mut out)
                .was_multicell
        );

        let report = space.update_geometry(&leaf((10, 2)), &started(), &mut out);
        assert!(report.was_multicell);
        assert!(report.own_cell_going);
        assert_eq!(report.erased, vec![(10, 1)]);
        assert!(!report.requested_shutdown);
        assert!(out.shutdown_space.is_empty());
    }

    /// The `CellApp` routes: every face answers `None` for a space this
    /// app does not serve and lands on the named space otherwise — one
    /// space's topology read never touches another's.
    #[test]
    fn the_app_routes_data_and_topology_to_the_named_space() {
        let mut app = CellApp::new(started());
        let mut out = SpaceOutbound::default();
        assert!(
            app.space_data_entry(9, (1, 5000), 7, b"x", false, true, 100, &mut out)
                .is_none()
        );
        assert!(
            app.space_data(9, (1, 5000), 7, b"x", 100, &mut out)
                .is_none()
        );
        assert!(
            app.all_space_data(9, &[((1, 5000), 7, b"x".to_vec())], 100, &mut out)
                .is_none()
        );
        assert!(app.update_geometry(9, &leaf((10, 1)), &mut out).is_none());

        let geometry = vec![CellInfoRecord::new((10, 1), Rect::unbounded())];
        assert!(matches!(
            app.add_cell(7, &geometry, (10, 1), false),
            AddCellOutcome::Created { .. }
        ));
        assert_eq!(
            app.all_space_data(7, &[((1, 5000), 9, b"nine".to_vec())], 100, &mut out),
            Some(1)
        );
        assert_eq!(
            app.spaces()
                .find(7)
                .unwrap()
                .space_data_mapping()
                .data_retrieve_first(9),
            Some(b"nine".as_slice())
        );
        let branch = SpaceNode::Branch {
            is_horizontal: false,
            position: 100.0,
            left: Box::new(leaf((10, 1))),
            right: Box::new(leaf((10, 2))),
        };
        let report = app.update_geometry(7, &branch, &mut out).unwrap();
        assert_eq!(report.added, vec![(10, 2)]);
    }
}
