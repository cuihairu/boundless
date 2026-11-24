//! BW-08-04: ghost creation and deletion (`entity_ghost_maintainer.cpp`).
//!
//! A real entity haunts every neighbouring cell its ghost distance reaches:
//! `EntityGhostMaintainer::check` marks the existing haunts, creates the
//! missing ones the interest area touches, and deletes the haunts left
//! marked — with the reference's keep rules (offload destination, new real,
//! new ghost, delete budget) applied. The receiving side mirrors
//! `Space::createGhost`'s immediate path and `delGhost`.

use std::collections::BTreeMap;

use foundation::math::Vector3;

use crate::cellappmgr::{Addr, Rect, SpaceId};
use crate::entity_dispatch::EntityId;
use crate::real_entity::{Haunt, RealEntity};
use crate::space::CellInfoRecord;
use crate::witness::VolatileInfo;

// -----------------------------------------------------------------------------
// Section: configuration (`cellapp_config.cpp` + maintainer locals)
// -----------------------------------------------------------------------------

/// `DEFAULT_AOI_RADIUS`, the `ghostDistance` default.
pub const DEFAULT_GHOST_DISTANCE: f32 = 500.0;
/// `minGhostLifespan` default, in seconds.
pub const DEFAULT_MIN_GHOST_LIFESPAN_IN_SECONDS: f32 = 5.0;
/// `maxGhostsToDelete` default.
pub const DEFAULT_MAX_GHOSTS_TO_DELETE: u32 = 100;
/// `GHOST_FUDGE` (`entity_ghost_maintainer.cpp:172`): the extra band around
/// the interest area. An existing haunt survives inside it, a new ghost does
/// not appear in it.
pub const GHOST_FUDGE: f32 = 20.0;
/// `NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS`
/// (`entity_ghost_maintainer.cpp:260`).
pub const NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS: f32 = 2.0;

/// `ServerAppConfig::secondsToTicks`: `max(lower, floor(seconds*hertz+0.5))`.
pub fn seconds_to_ticks(seconds: f32, lower_bound: i32, update_hertz: f32) -> i32 {
    let ticks = (seconds * update_hertz + 0.5).floor() as i32;
    if ticks < lower_bound {
        lower_bound
    } else {
        ticks
    }
}

/// The ghost knobs the maintainer reads
/// (`CellAppConfig::ghostDistance/minGhostLifespanInTicks/maxGhostsToDelete`).
#[derive(Debug, Clone, PartialEq)]
pub struct GhostConfig {
    /// `ghostDistance`.
    pub ghost_distance: f32,
    /// `updateHertz` — feeds the tick conversions.
    pub update_hertz: f32,
    /// `minGhostLifespanInTicks` (`secondsToTicks(minGhostLifespan, 0)`).
    pub min_ghost_lifespan_in_ticks: i32,
    /// `maxGhostsToDelete`.
    pub max_ghosts_to_delete: u32,
}

impl Default for GhostConfig {
    fn default() -> Self {
        let update_hertz = 10.0;
        GhostConfig {
            ghost_distance: DEFAULT_GHOST_DISTANCE,
            update_hertz,
            min_ghost_lifespan_in_ticks: seconds_to_ticks(
                DEFAULT_MIN_GHOST_LIFESPAN_IN_SECONDS,
                0,
                update_hertz,
            ),
            max_ghosts_to_delete: DEFAULT_MAX_GHOSTS_TO_DELETE,
        }
    }
}

// -----------------------------------------------------------------------------
// Section: the CellAppChannel mirror
// -----------------------------------------------------------------------------

/// One message the maintainer streams onto a channel's bundle
/// (`CellAppInterface::createGhost` / `delGhost`).
#[derive(Clone, Debug, PartialEq)]
pub enum GhostMessage {
    /// `Entity::createGhost` (`entity.cpp:4742`): the space id plus the
    /// entity's ghost snapshot.
    CreateGhost(GhostCreatePayload),
    /// `RealEntity::addDelGhostMessage` (`real_entity.cpp:755`).
    DelGhost {
        /// `entity_.id()`.
        entity_id: EntityId,
    },
}

/// The `writeGhostDataToStream` head (`entity.cpp:1846-1874`): everything the
/// receiving side needs before the ghosted properties and controllers.
#[derive(Clone, Debug, PartialEq)]
pub struct GhostCreatePayload {
    /// `cell().space().id()`, streamed ahead of the ghost data.
    pub space_id: SpaceId,
    /// `id_` — read off first by `Space::createGhost`.
    pub entity_id: EntityId,
    /// `entityTypeID()`.
    pub entity_type_id: u16,
    /// `numTimesRealOffloaded_` — `BufferedGhostMessage` reads it too.
    pub num_times_real_offloaded: u32,
    /// `localPosition_`.
    pub position: Vector3,
    /// `isOnGround_`.
    pub is_on_ground: bool,
    /// `lastEventNumber_`.
    pub last_event_number: i32,
    /// `volatileInfo_`.
    pub volatile: VolatileInfo,
    /// `CellApp::instance().interface().address()` — where the real lives.
    pub real_addr: Addr,
    /// `baseAddr_`.
    pub base_addr: Addr,
}

/// The pieces of a `CellAppChannel` the maintainer touches: the mark used to
/// sweep haunts, the channel health, and the bundle's outbound messages.
#[derive(Clone, Debug, PartialEq)]
pub struct GhostChannel {
    /// The remote app's address (the map key, kept for message reads).
    pub addr: Addr,
    /// `mark_` — 1 while the sweep is deciding the haunt's fate.
    pub mark: u8,
    /// `isGood()`.
    pub is_good: bool,
    /// `bundle()` — the messages gathered for this channel.
    pub out: Vec<GhostMessage>,
}

impl GhostChannel {
    /// A fresh, healthy channel (`CellAppChannels::get` creates on demand).
    pub fn new(addr: Addr) -> Self {
        GhostChannel {
            addr,
            mark: 0,
            is_good: true,
            out: Vec::new(),
        }
    }
}

/// `OffloadChecker`'s ghost-deletion budget (`offload_checker.hpp:38-44`).
/// One instance lives for one offload-checker iteration; `numGhostsDeleted_`
/// carries across the entities it visits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OffloadCheckerBudget {
    /// `numGhostsDeleted_`.
    pub num_ghosts_deleted: u32,
}

impl OffloadCheckerBudget {
    /// `OffloadChecker::canDeleteMoreGhosts`
    /// (`offload_checker.cpp:123-126`): `num <= max` — the reference allows
    /// one deletion past the configured cap.
    pub fn can_delete_more_ghosts(&self, config: &GhostConfig) -> bool {
        self.num_ghosts_deleted <= config.max_ghosts_to_delete
    }

    /// `OffloadChecker::addDeletedGhost`.
    pub fn add_deleted_ghost(&mut self) {
        self.num_ghosts_deleted += 1;
    }
}

// -----------------------------------------------------------------------------
// Section: the maintainer (`entity_ghost_maintainer.cpp`)
// -----------------------------------------------------------------------------

/// What one `check` pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhostCheckReport {
    /// Haunts added this pass.
    pub ghosts_created: usize,
    /// Haunts removed this pass.
    pub ghosts_deleted: usize,
    /// `pOffloadDestination_` when `checkEntityForOffload` picked one.
    pub offload_destination: Option<Addr>,
}

/// The per-entity half of the ghost sweep (`EntityGhostMaintainer`). The
/// channel map and the space's cell records are shared across entities, the
/// budget is the caller's iteration counter.
pub struct GhostMaintainer {
    /// The knobs.
    pub config: GhostConfig,
    /// `ownAddress_` — this app's interface address.
    pub own_addr: Addr,
    /// The space id the createGhost messages carry.
    pub space_id: SpaceId,
    /// `Cell::shouldOffload()` — the balance decision is BW-08-05-01's face;
    /// when false the offload half of `check` is skipped entirely.
    pub should_offload: bool,
    /// `offloadChecker_`'s ghost budget.
    pub budget: OffloadCheckerBudget,
}

impl GhostMaintainer {
    /// `EntityGhostMaintainer::check` (`entity_ghost_maintainer.cpp:56-75`):
    /// mark the haunts, create or unmark the required ones, delete the rest.
    pub fn check(
        &mut self,
        cells: &BTreeMap<Addr, CellInfoRecord>,
        channels: &mut BTreeMap<Addr, GhostChannel>,
        real: &mut RealEntity,
        view: &GhostedEntityView,
        time: u32,
    ) -> GhostCheckReport {
        let mut report = GhostCheckReport::default();

        // `if (this->cell().shouldOffload())` — find where we really want to
        // live before deciding which ghosts to touch.
        let mut offload_destination = None;
        if self.should_offload {
            offload_destination = self.check_entity_for_offload(cells, channels, view.position);
        }
        report.offload_destination = offload_destination;

        self.mark_haunts(channels, real);
        self.create_or_unmark_required_haunts(
            cells,
            channels,
            real,
            view,
            offload_destination,
            time,
            &mut report,
        );
        self.delete_marked_haunts(channels, real, offload_destination, time, &mut report);

        report
    }

    /// `checkEntityForOffload` (`entity_ghost_maintainer.cpp:91-123`): the
    /// cell at the entity's position, provided it is not ours, not being
    /// deleted, and its channel is still good.
    fn check_entity_for_offload(
        &self,
        cells: &BTreeMap<Addr, CellInfoRecord>,
        channels: &mut BTreeMap<Addr, GhostChannel>,
        position: Vector3,
    ) -> Option<Addr> {
        // `Space::pCellAt`: the leaf containing the point, and only a leaf
        // that has been created is a candidate.
        let home = cells.values().find(|info| {
            info.has_been_created
                && info.rect.xmin <= position.x
                && position.x < info.rect.xmax
                && info.rect.ymin <= position.z
                && position.z < info.rect.ymax
        })?;

        // Don't offload to ourselves.
        if home.addr == self.own_addr {
            return None;
        }
        // Don't offload to a cell that is being deleted.
        if home.is_delete_pending {
            return None;
        }

        let channel = channels
            .entry(home.addr)
            .or_insert_with(|| GhostChannel::new(home.addr));
        // Don't offload if the other cell has failed or died.
        if !channel.is_good {
            return None;
        }

        Some(home.addr)
    }

    /// `markHaunts` (`entity_ghost_maintainer.cpp:135-154`): every haunt's
    /// channel is marked 1; `createOrUnmarkRequiredHaunts` unmarks the ones
    /// that survive.
    fn mark_haunts(&self, channels: &mut BTreeMap<Addr, GhostChannel>, real: &RealEntity) {
        for haunt in &real.haunts {
            let channel = channels
                .entry(haunt.addr)
                .or_insert_with(|| GhostChannel::new(haunt.addr));
            channel.mark = 1;
        }
    }

    /// `createOrUnmarkRequiredHaunts`
    /// (`entity_ghost_maintainer.cpp:169-188`): the interest area is the
    /// entity's position grown by `ghostDistance + appealRadius`; the
    /// hysteresis area is that without the fudge. Cells in the interest area
    /// are visited, cells in the hysteresis area gain or keep ghosts.
    #[allow(clippy::too_many_arguments)]
    fn create_or_unmark_required_haunts(
        &self,
        cells: &BTreeMap<Addr, CellInfoRecord>,
        channels: &mut BTreeMap<Addr, GhostChannel>,
        real: &mut RealEntity,
        view: &GhostedEntityView,
        offload_destination: Option<Addr>,
        time: u32,
        report: &mut GhostCheckReport,
    ) {
        let mut interest_area = Rect::new(
            view.position.x,
            view.position.z,
            view.position.x,
            view.position.z,
        );
        interest_area.inflate_by(self.config.ghost_distance + view.appeal_radius);
        let hysteresis_area = interest_area;
        interest_area.inflate_by(GHOST_FUDGE);

        // `Space::visitRect` over the flat leaf set: a leaf is visited when
        // it has been created and its range meets the query rectangle
        // (`CellInfo::visitRect` visits its leaf unconditionally; the BSP
        // prunes the rest).
        for info in cells
            .values()
            .filter(|info| info.has_been_created && interest_area.intersects(&info.rect))
        {
            self.visit(
                info,
                channels,
                real,
                view,
                &hysteresis_area,
                offload_destination,
                time,
                report,
            );
        }
    }

    /// `EntityGhostMaintainer::visit` (`entity_ghost_maintainer.cpp:194-241`).
    #[allow(clippy::too_many_arguments)]
    fn visit(
        &self,
        info: &CellInfoRecord,
        channels: &mut BTreeMap<Addr, GhostChannel>,
        real: &mut RealEntity,
        view: &GhostedEntityView,
        hysteresis_area: &Rect,
        offload_destination: Option<Addr>,
        time: u32,
        report: &mut GhostCheckReport,
    ) {
        let remote_addr = info.addr;

        // Discard it if it is ourself.
        if remote_addr == self.own_addr {
            return;
        }

        // Do not have ghosts on cells that are about to be deleted.
        if info.is_delete_pending {
            return;
        }

        let channel = channels
            .entry(remote_addr)
            .or_insert_with(|| GhostChannel::new(remote_addr));

        // An existing haunt this pass still wants: unmark it and bail.
        if channel.mark == 1 {
            channel.mark = 0;
            return;
        }

        // While offloading, only the destination may gain a ghost.
        if offload_destination.is_some() && offload_destination != Some(remote_addr) {
            return;
        }

        // Not far enough in yet (the hysteresis check).
        if !info.rect.intersects(hysteresis_area) {
            return;
        }

        // Otherwise create a new ghost.
        // `addHaunt` stamps the app time (`real_entity.cpp:859-863`).
        real.haunts.push(Haunt {
            addr: remote_addr,
            creation_time: time,
        });
        channel
            .out
            .push(GhostMessage::CreateGhost(GhostCreatePayload {
                space_id: self.space_id,
                entity_id: view.entity_id,
                entity_type_id: view.entity_type_id,
                num_times_real_offloaded: view.num_times_real_offloaded,
                position: view.position,
                is_on_ground: view.is_on_ground,
                last_event_number: view.last_event_number,
                volatile: view.volatile,
                real_addr: self.own_addr,
                base_addr: view.base_addr,
            }));
        report.ghosts_created += 1;
    }

    /// `deleteMarkedHaunts` (`entity_ghost_maintainer.cpp:258-308`): the
    /// marked haunts go, unless a keep rule holds. The mark is always
    /// cleared for the next user.
    fn delete_marked_haunts(
        &mut self,
        channels: &mut BTreeMap<Addr, GhostChannel>,
        real: &mut RealEntity,
        offload_destination: Option<Addr>,
        time: u32,
        report: &mut GhostCheckReport,
    ) {
        let new_real_keep_period = seconds_to_ticks(
            NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS,
            0,
            self.config.update_hertz,
        ) as u32;

        let mut index = 0;
        while index < real.haunts.len() {
            let haunt = &real.haunts[index];
            let haunt_addr = haunt.addr;
            let haunt_creation_time = haunt.creation_time;

            let should_del_ghost =
                // Too many ghosts deleted in this iteration.
                self.budget.can_delete_more_ghosts(&self.config)
                    // Keep the ghost if we're offloading there.
                    && offload_destination != Some(haunt_addr)
                    // Keep the ghost if the real entity is new.
                    && time.wrapping_sub(real.creation_time) > new_real_keep_period
                    // Keep the ghost if the ghost is new.
                    && time.wrapping_sub(haunt_creation_time)
                        > self.config.min_ghost_lifespan_in_ticks as u32;

            let channel = channels
                .entry(haunt_addr)
                .or_insert_with(|| GhostChannel::new(haunt_addr));

            if channel.mark != 0 && should_del_ghost {
                // Only bother telling it if it hasn't failed.
                if channel.is_good {
                    channel
                        .out
                        .push(GhostMessage::DelGhost { entity_id: real.id });
                    self.budget.add_deleted_ghost();
                }
                real.haunts.remove(index);
                report.ghosts_deleted += 1;
            } else {
                index += 1;
            }

            // Always clear the mark for the next user.
            channel.mark = 0;
        }
    }
}

// -----------------------------------------------------------------------------
// Section: the receiving side (`Space::createGhost` + `delGhost`)
// -----------------------------------------------------------------------------

/// The ghost-side entity record `Space::createGhost` builds
/// (`space.cpp:271-292`): `newEntity` + `initGhost` over the streamed head.
#[derive(Clone, Debug, PartialEq)]
pub struct GhostRecord {
    /// `id_`.
    pub entity_id: EntityId,
    /// `entityTypeID()`.
    pub entity_type_id: u16,
    /// `numTimesRealOffloaded_`.
    pub num_times_real_offloaded: u32,
    /// `localPosition_`.
    pub position: Vector3,
    /// `isOnGround_`.
    pub is_on_ground: bool,
    /// `lastEventNumber_`.
    pub last_event_number: i32,
    /// `volatileInfo_`.
    pub volatile: VolatileInfo,
    /// The real's cellapp address.
    pub real_addr: Addr,
    /// `baseAddr_`.
    pub base_addr: Addr,
    /// The app time the ghost was created at (haunt-age bookkeeping).
    pub creation_time: u32,
}

/// What `Space::createGhost(srcAddr, data)` did with the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhostCreateOutcome {
    /// The immediate path: the ghost was created.
    Created,
    /// The entity already exists here (`space.cpp:236-266` buffers the
    /// message instead; the buffered replay is BW-08-04-04's face).
    EntityAlreadyExists,
}

/// The receiving CellApp's ghost surface: the ghost records of one space.
#[derive(Debug, Default)]
pub struct GhostHost {
    /// `app.findEntity` + the ghost population, keyed by entity id.
    pub ghosts: BTreeMap<EntityId, GhostRecord>,
}

impl GhostHost {
    /// `Space::createGhost(srcAddr, header, data)`
    /// (`space.cpp:228-266`): an existing entity (or pending buffer) takes
    /// the buffered path, otherwise the ghost is created at once.
    pub fn receive_create_ghost(
        &mut self,
        payload: GhostCreatePayload,
        time: u32,
    ) -> GhostCreateOutcome {
        if self.ghosts.contains_key(&payload.entity_id) {
            return GhostCreateOutcome::EntityAlreadyExists;
        }

        self.ghosts.insert(
            payload.entity_id,
            GhostRecord {
                entity_id: payload.entity_id,
                entity_type_id: payload.entity_type_id,
                num_times_real_offloaded: payload.num_times_real_offloaded,
                position: payload.position,
                is_on_ground: payload.is_on_ground,
                last_event_number: payload.last_event_number,
                volatile: payload.volatile,
                real_addr: payload.real_addr,
                base_addr: payload.base_addr,
                creation_time: time,
            },
        );
        GhostCreateOutcome::Created
    }

    /// The `delGhost` receive: the ghost goes; a second notice finds nothing.
    /// Returns whether a ghost was removed.
    pub fn receive_del_ghost(&mut self, entity_id: EntityId) -> bool {
        self.ghosts.remove(&entity_id).is_some()
    }
}

// -----------------------------------------------------------------------------
// Section: the entity view
// -----------------------------------------------------------------------------

/// What the maintainer reads off the entity (`Entity::createGhost`'s
/// payload sources). `appealRadius` comes from the entity type's
/// description; `creationTime` is the app time for haunt stamps.
#[derive(Clone, Debug)]
pub struct GhostedEntityView {
    /// `id_`.
    pub entity_id: EntityId,
    /// `entityTypeID()`.
    pub entity_type_id: u16,
    /// `pType()->description().appealRadius()`.
    pub appeal_radius: f32,
    /// `numTimesRealOffloaded_`.
    pub num_times_real_offloaded: u32,
    /// `position()`.
    pub position: Vector3,
    /// `isOnGround()`.
    pub is_on_ground: bool,
    /// `lastEventNumber()`.
    pub last_event_number: i32,
    /// `volatileInfo()`.
    pub volatile: VolatileInfo,
    /// `baseAddr()`.
    pub base_addr: Addr,
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const APP_A: Addr = (1, 1);
    const APP_B: Addr = (2, 2);
    const APP_C: Addr = (3, 3);
    const APP_D: Addr = (4, 4);

    /// Two adjacent 100m cells: A owns x in [0,100), B owns x in [100,200).
    /// A tight ghost distance keeps the interest bands inside the fixture.
    fn two_cell_world() -> (
        BTreeMap<Addr, CellInfoRecord>,
        BTreeMap<Addr, GhostChannel>,
        GhostMaintainer,
    ) {
        let mut cells = BTreeMap::new();
        cells.insert(
            APP_A,
            CellInfoRecord::new(APP_A, Rect::new(0.0, 0.0, 100.0, 100.0)),
        );
        cells.insert(
            APP_B,
            CellInfoRecord::new(APP_B, Rect::new(100.0, 0.0, 200.0, 100.0)),
        );
        for info in cells.values_mut() {
            info.has_been_created = true;
        }

        let mut channels = BTreeMap::new();
        channels.insert(APP_A, GhostChannel::new(APP_A));
        channels.insert(APP_B, GhostChannel::new(APP_B));

        let maintainer = GhostMaintainer {
            config: GhostConfig {
                ghost_distance: 30.0,
                ..GhostConfig::default()
            },
            own_addr: APP_A,
            space_id: 7,
            should_offload: false,
            budget: OffloadCheckerBudget::default(),
        };
        (cells, channels, maintainer)
    }

    /// A real entity of type 4 living on A, created 100 ticks ago.
    fn ghosted_real(id: EntityId, x: f32) -> (RealEntity, GhostedEntityView) {
        let mut real = RealEntity::new(id, APP_A, Vector3::new(x, 0.0, 50.0), 0, 0);
        real.creation_time = 0;
        let view = GhostedEntityView {
            entity_id: id,
            entity_type_id: 4,
            appeal_radius: 0.0,
            num_times_real_offloaded: 0,
            position: Vector3::new(x, 0.0, 50.0),
            is_on_ground: true,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            base_addr: (9, 9),
        };
        (real, view)
    }

    #[test]
    fn a_real_near_the_boundary_ghosts_onto_the_adjacent_cell() {
        let (cells, mut channels, mut maintainer) = two_cell_world();
        let (mut real, view) = ghosted_real(10, 90.0);

        // x = 90: B's rect is 10m away, inside the 30m ghost distance.
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        assert_eq!(report.ghosts_created, 1);
        assert_eq!(report.ghosts_deleted, 0);
        assert_eq!(
            real.haunts,
            vec![Haunt {
                addr: APP_B,
                creation_time: 100
            }]
        );
        assert_eq!(
            channels[&APP_B].out,
            vec![GhostMessage::CreateGhost(GhostCreatePayload {
                space_id: 7,
                entity_id: 10,
                entity_type_id: 4,
                num_times_real_offloaded: 0,
                position: Vector3::new(90.0, 0.0, 50.0),
                is_on_ground: true,
                last_event_number: 0,
                volatile: VolatileInfo::NONE,
                real_addr: APP_A,
                base_addr: (9, 9),
            })]
        );

        // A second pass at the same spot keeps exactly one haunt: the
        // channel unmarks and no second create rides.
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        assert_eq!(report.ghosts_created, 0);
        assert_eq!(report.ghosts_deleted, 0);
        assert_eq!(real.haunts.len(), 1);
        assert_eq!(channels[&APP_B].out.len(), 1);
    }

    #[test]
    fn the_hysteresis_band_keeps_an_existing_ghost_without_creating_new_ones() {
        let (cells, mut channels, mut maintainer) = two_cell_world();
        let (mut real, mut view) = ghosted_real(10, 90.0);
        maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        assert_eq!(channels[&APP_B].out.len(), 1);

        // x = 65: B is 35m away — outside the 30m hysteresis area, inside
        // the 50m fudged interest area. The visit unmarks the haunt without
        // creating anything, so the ghost survives the sweep.
        view.position.x = 65.0;
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 200);
        assert_eq!(report.ghosts_created, 0);
        assert_eq!(report.ghosts_deleted, 0);
        assert_eq!(real.haunts.len(), 1);

        // A third pass still holds the ghost: the cell is visited with its
        // mark already cleared, and the visit stops at the hysteresis band.
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 200);
        assert_eq!(report.ghosts_created, 0);
        assert_eq!(report.ghosts_deleted, 0);
        assert_eq!(real.haunts.len(), 1);

        // x = 45: B is 55m away — outside even the fudged area. The haunt
        // stays marked and the sweep deletes it.
        view.position.x = 45.0;
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 200);
        assert_eq!(report.ghosts_created, 0);
        assert_eq!(report.ghosts_deleted, 1);
        assert!(real.haunts.is_empty());
        assert_eq!(
            channels[&APP_B].out,
            vec![
                GhostMessage::CreateGhost(GhostCreatePayload {
                    space_id: 7,
                    entity_id: 10,
                    entity_type_id: 4,
                    num_times_real_offloaded: 0,
                    position: Vector3::new(90.0, 0.0, 50.0),
                    is_on_ground: true,
                    last_event_number: 0,
                    volatile: VolatileInfo::NONE,
                    real_addr: APP_A,
                    base_addr: (9, 9),
                }),
                GhostMessage::DelGhost { entity_id: 10 },
            ]
        );
    }

    #[test]
    fn a_pending_delete_cell_neither_gains_nor_keeps_ghosts() {
        let (mut cells, mut channels, mut maintainer) = two_cell_world();
        let (mut real, view) = ghosted_real(10, 90.0);

        // No new ghost toward a pending-delete cell.
        cells.get_mut(&APP_B).unwrap().is_delete_pending = true;
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        assert_eq!(report.ghosts_created, 0);
        assert!(real.haunts.is_empty());

        // ... and an existing ghost on a cell that turns pending-delete is
        // swept: the pending check runs before the unmark, so the haunt
        // stays marked and the delete pass takes it.
        cells.get_mut(&APP_B).unwrap().is_delete_pending = false;
        maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        assert_eq!(real.haunts.len(), 1);
        cells.get_mut(&APP_B).unwrap().is_delete_pending = true;
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 200);
        assert_eq!(report.ghosts_deleted, 1);
        assert!(real.haunts.is_empty());
    }

    #[test]
    fn new_reals_and_new_ghosts_survive_the_sweep() {
        let (cells, mut channels, mut maintainer) = two_cell_world();
        let (mut real, mut view) = ghosted_real(10, 90.0);

        // Ghost created at tick 100 with the default 50-tick lifespan.
        maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        view.position.x = 45.0; // out of the fudged area: the sweep runs

        // At tick 120 the haunt is 20 ticks old: below the lifespan. Kept.
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 120);
        assert_eq!(report.ghosts_deleted, 0);
        assert_eq!(real.haunts.len(), 1);

        // A young real outranks even a spent ghost lifespan: with the
        // lifespan cut to 5 ticks, the haunt at tick 112 is 12 ticks old
        // (past the ghost rule) but the real is 12 ticks old, inside the
        // 20-tick new-real keep period.
        maintainer.config.min_ghost_lifespan_in_ticks = 5;
        let (mut young_real, mut young_view) = ghosted_real(11, 90.0);
        young_real.creation_time = 100;
        maintainer.check(&cells, &mut channels, &mut young_real, &young_view, 100);
        young_view.position.x = 45.0;
        let report = maintainer.check(&cells, &mut channels, &mut young_real, &young_view, 112);
        assert_eq!(report.ghosts_deleted, 0);
        assert_eq!(young_real.haunts.len(), 1);

        // Past both periods the same haunt goes.
        let report = maintainer.check(&cells, &mut channels, &mut young_real, &young_view, 200);
        assert_eq!(report.ghosts_deleted, 1);
        assert!(young_real.haunts.is_empty());

        // And the first real's long-lived haunt is now past every rule.
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 200);
        assert_eq!(report.ghosts_deleted, 1);
        assert!(real.haunts.is_empty());
    }

    #[test]
    fn the_delete_budget_blocks_the_second_ghost_of_an_iteration() {
        let (cells, mut channels, mut maintainer) = two_cell_world();

        // Entity 10 ghosts and is then swept: with maxGhostsToDelete = 0 the
        // first deletion still goes through (`num <= max`).
        maintainer.config.max_ghosts_to_delete = 0;
        let (mut real_a, mut view_a) = ghosted_real(10, 90.0);
        maintainer.check(&cells, &mut channels, &mut real_a, &view_a, 100);
        view_a.position.x = 45.0;
        let report = maintainer.check(&cells, &mut channels, &mut real_a, &view_a, 200);
        assert_eq!(report.ghosts_deleted, 1);
        assert_eq!(maintainer.budget.num_ghosts_deleted, 1);

        // Entity 11 in the same checker iteration: the budget is spent, its
        // marked haunt survives, and the mark still clears for next time.
        let (mut real_b, mut view_b) = ghosted_real(11, 90.0);
        maintainer.check(&cells, &mut channels, &mut real_b, &view_b, 100);
        view_b.position.x = 45.0;
        let report = maintainer.check(&cells, &mut channels, &mut real_b, &view_b, 200);
        assert_eq!(report.ghosts_deleted, 0);
        assert_eq!(real_b.haunts.len(), 1);
        assert_eq!(channels[&APP_B].mark, 0);
    }

    #[test]
    fn a_failed_channel_is_dropped_without_a_del_ghost_message() {
        let (cells, mut channels, mut maintainer) = two_cell_world();
        let (mut real, mut view) = ghosted_real(10, 90.0);
        maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        assert_eq!(real.haunts.len(), 1);

        // The channel failed: the haunt still goes (no point keeping a ghost
        // on a dead app), but no delGhost rides the dead channel.
        channels.get_mut(&APP_B).unwrap().is_good = false;
        view.position.x = 45.0;
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 200);
        assert_eq!(report.ghosts_deleted, 1);
        assert!(real.haunts.is_empty());
        assert_eq!(channels[&APP_B].out.len(), 1);
    }

    #[test]
    fn an_offloading_real_only_ghosts_onto_its_destination() {
        let (mut cells, mut channels, mut maintainer) = two_cell_world();
        // A third cell off the A/B axis, reachable at ghost distance.
        cells.insert(
            APP_C,
            CellInfoRecord::new(APP_C, Rect::new(0.0, 100.0, 100.0, 200.0)),
        );
        cells.get_mut(&APP_C).unwrap().has_been_created = true;
        channels.insert(APP_C, GhostChannel::new(APP_C));

        maintainer.should_offload = true;
        // The real sits inside B's rect: the offload destination is B.
        let (mut real, mut view) = ghosted_real(10, 130.0);

        // A fourth cell east of B is inside the interest area but not the
        // destination: while offloading, no ghost may appear there.
        cells.insert(
            APP_D,
            CellInfoRecord::new(APP_D, Rect::new(150.0, 0.0, 250.0, 100.0)),
        );
        cells.get_mut(&APP_D).unwrap().has_been_created = true;
        channels.insert(APP_D, GhostChannel::new(APP_D));

        // First pass ghosts onto B only — C and D are inside the interest
        // area but not the destination.
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        assert_eq!(report.offload_destination, Some(APP_B));
        assert_eq!(report.ghosts_created, 1);
        assert_eq!(
            real.haunts,
            vec![Haunt {
                addr: APP_B,
                creation_time: 100
            }]
        );
        assert!(channels[&APP_C].out.is_empty());

        // A haunt on C from an earlier pass is swept (B stays: it is the
        // offload destination).
        real.haunts.push(Haunt {
            addr: APP_C,
            creation_time: 0,
        });
        channels.get_mut(&APP_C).unwrap().mark = 1;
        // Still inside B's rect, so the destination does not move; C falls
        // out of the fudged interest area and its marked haunt is swept.
        view.position.x = 140.0;
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 200);
        assert_eq!(report.offload_destination, Some(APP_B));
        assert_eq!(report.ghosts_deleted, 1);
        assert_eq!(
            real.haunts,
            vec![Haunt {
                addr: APP_B,
                creation_time: 100
            }]
        );
        assert_eq!(
            channels[&APP_C].out,
            vec![GhostMessage::DelGhost { entity_id: 10 }]
        );
        // The B haunt was kept and unmarked without a second create.
        assert_eq!(channels[&APP_B].out.len(), 1);
    }

    #[test]
    fn an_offload_check_skips_our_own_cell_and_unsuitable_ground() {
        let (mut cells, mut channels, mut maintainer) = two_cell_world();
        maintainer.should_offload = true;

        // Standing on our own cell: no offload destination.
        let (real, view) = ghosted_real(10, 50.0);
        let report = maintainer.check(&cells, &mut channels, &mut real.clone(), &view, 100);
        assert_eq!(report.offload_destination, None);

        // Off the topology entirely: no cell owns the point.
        let (real, mut view) = ghosted_real(10, 250.0);
        let report = maintainer.check(&cells, &mut channels, &mut real.clone(), &view, 100);
        assert_eq!(report.offload_destination, None);

        // The cell under our feet is being deleted.
        view.position.x = 130.0;
        cells.get_mut(&APP_B).unwrap().is_delete_pending = true;
        let report = maintainer.check(&cells, &mut channels, &mut real.clone(), &view, 100);
        assert_eq!(report.offload_destination, None);

        // The cell under our feet has a dead channel.
        cells.get_mut(&APP_B).unwrap().is_delete_pending = false;
        channels.get_mut(&APP_B).unwrap().is_good = false;
        let report = maintainer.check(&cells, &mut channels, &mut real.clone(), &view, 100);
        assert_eq!(report.offload_destination, None);
    }

    #[test]
    fn a_haunt_whose_channel_never_existed_is_still_swept() {
        let (cells, mut channels, mut maintainer) = two_cell_world();
        let (mut real, mut view) = ghosted_real(10, 45.0);

        // A haunt for an app the channel map lost: mark and delete passes
        // recreate the channel on the fly (`CellAppChannels::get`).
        real.haunts.push(Haunt {
            addr: APP_C,
            creation_time: 0,
        });
        view.position.x = 45.0; // nothing new to create
        let report = maintainer.check(&cells, &mut channels, &mut real, &view, 200);
        assert_eq!(report.ghosts_deleted, 1);
        assert!(real.haunts.is_empty());
        // The recreated channel is healthy, so the delGhost rides it.
        assert_eq!(
            channels[&APP_C].out,
            vec![GhostMessage::DelGhost { entity_id: 10 }]
        );
    }

    #[test]
    fn the_ghost_host_keeps_exactly_one_record_per_create() {
        let mut host = GhostHost::default();
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 10,
            entity_type_id: 4,
            num_times_real_offloaded: 2,
            position: Vector3::new(90.0, 0.0, 50.0),
            is_on_ground: true,
            last_event_number: 5,
            volatile: VolatileInfo::NONE,
            real_addr: APP_A,
            base_addr: (9, 9),
        };

        assert_eq!(
            host.receive_create_ghost(payload.clone(), 42),
            GhostCreateOutcome::Created
        );
        assert_eq!(host.ghosts.len(), 1);
        let record = host.ghosts.get(&10).unwrap();
        assert_eq!(record.entity_type_id, 4);
        assert_eq!(record.num_times_real_offloaded, 2);
        assert_eq!(record.last_event_number, 5);
        assert_eq!(record.real_addr, APP_A);
        assert_eq!(record.creation_time, 42);

        // A second create for a living entity takes the buffered path — no
        // duplicate record, no count drift.
        assert_eq!(
            host.receive_create_ghost(payload, 43),
            GhostCreateOutcome::EntityAlreadyExists
        );
        assert_eq!(host.ghosts.len(), 1);

        assert!(host.receive_del_ghost(10));
        assert_eq!(host.ghosts.len(), 0);
        assert!(!host.receive_del_ghost(10));
    }

    #[test]
    fn seconds_to_ticks_matches_the_reference_rounding() {
        // `max(lower, floor(seconds*hertz + 0.5))`.
        assert_eq!(seconds_to_ticks(5.0, 0, 10.0), 50);
        assert_eq!(seconds_to_ticks(2.0, 0, 10.0), 20);
        assert_eq!(seconds_to_ticks(0.0, 0, 10.0), 0);
        assert_eq!(seconds_to_ticks(0.01, 0, 10.0), 0);
        // The lower bound floors a sub-tick result up.
        assert_eq!(seconds_to_ticks(0.01, 1, 10.0), 1);
    }
}
