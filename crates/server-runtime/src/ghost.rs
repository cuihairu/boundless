//! BW-08-04: ghost creation and deletion (`entity_ghost_maintainer.cpp`).
//!
//! A real entity haunts every neighbouring cell its ghost distance reaches:
//! `EntityGhostMaintainer::check` marks the existing haunts, creates the
//! missing ones the interest area touches, and deletes the haunts left
//! marked — with the reference's keep rules (offload destination, new real,
//! new ghost, delete budget) applied. The receiving side mirrors
//! `Space::createGhost`'s immediate path and `delGhost`.

use std::collections::{BTreeMap, VecDeque};

use foundation::math::Vector3;

use crate::cellappmgr::{Addr, Rect, SpaceId};
use crate::controllers::Controllers;
use crate::entity_creator::PropValue;
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
    /// `CellAppInterface::ghostSetNextReal`
    /// (`real_entity.cpp:272-278`): the offloading real's sign-off.
    SetNextReal(GhostSetNextReal),
    /// `CellAppInterface::ghostSetReal`
    /// (`real_entity.cpp:384-398`): the loaded real's routing update.
    SetReal(GhostSetReal),
}

/// The offload sign-off payload (`ghostSetNextRealArgs`).
#[derive(Clone, Debug, PartialEq)]
pub struct GhostSetNextReal {
    /// The entity whose real is moving.
    pub entity_id: EntityId,
    /// `nextRealAddr` — where the real will live.
    pub next_real_addr: Addr,
}

/// The routing update payload (`ghostSetRealArgs`).
#[derive(Clone, Debug, PartialEq)]
pub struct GhostSetReal {
    /// The entity whose real has landed.
    pub entity_id: EntityId,
    /// `numTimesRealOffloaded` — the subsequence number.
    pub num_times_real_offloaded: u32,
    /// `owner` — the real's new address.
    pub owner: Addr,
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
    /// `localDirection_`.
    pub direction: Vector3,
    /// `propertyEventStamps_` — one stamp per event-stamped property.
    pub property_event_stamps: Vec<i32>,
    /// The `propCountGhost` leading properties (`GProperties` token block).
    pub ghost_properties: Vec<PropValue>,
    /// `Controllers::writeGhostsToStream`'s per-controller records
    /// (`GController` token block): id, type, user arg, ghost payload.
    pub ghost_controllers: Vec<GhostControllerSeed>,
    /// `periodsWithoutWitness_` (`GTail` token block).
    pub periods_without_witness: u16,
    /// `aoiUpdateSchemeID_`.
    pub aoi_update_scheme_id: u8,
}

/// One `Controllers::writeGhostsToStream` entry
/// (`controllers.cpp:152-165`): `id << type << userArg` then the
/// controller's own `writeGhostToStream` payload.
#[derive(Clone, Debug, PartialEq)]
pub struct GhostControllerSeed {
    /// `pController->id()`.
    pub id: crate::controllers::ControllerId,
    /// `pController->type()`.
    pub type_id: crate::controllers::ControllerType,
    /// `pController->userArg()`.
    pub user_arg: i32,
    /// `writeGhostToStream`'s payload.
    pub payload: Vec<u8>,
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
                direction: view.direction,
                property_event_stamps: view.property_event_stamps.clone(),
                ghost_properties: view.ghost_properties.clone(),
                ghost_controllers: view.ghost_controllers.clone(),
                periods_without_witness: view.periods_without_witness,
                aoi_update_scheme_id: view.aoi_update_scheme_id,
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
#[derive(Clone, Debug)]
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
    /// The real's cellapp address (`pRealChannel_`'s address).
    pub real_addr: Addr,
    /// `nextRealAddr_` — set by the offload sign-off, cleared when the
    /// new real's `ghostSetReal` lands.
    pub next_real_addr: Option<Addr>,
    /// `baseAddr_`.
    pub base_addr: Addr,
    /// `localDirection_` (`globalDirection_ = localDirection_` on init).
    pub direction: Vector3,
    /// `propertyEventStamps_` after the read — one per event-stamped
    /// property of the type.
    pub property_event_stamps: Vec<i32>,
    /// `eventHistory_.lastTrimmedEventNumber( lastEventNumber_ )` — the
    /// ghost's history starts trimmed at the real's last event number.
    pub last_trimmed_event_number: i32,
    /// The `propCountGhost` leading properties.
    pub ghost_properties: Vec<PropValue>,
    /// `Controllers::readGhostsFromStream`'s result: the ghost-domain
    /// controllers, started (`startGhost`).
    pub controllers: Controllers,
    /// `periodsWithoutWitness_`.
    pub periods_without_witness: u16,
    /// `aoiUpdateSchemeID_`.
    pub aoi_update_scheme_id: u8,
    /// The app time the ghost was created at (haunt-age bookkeeping).
    pub creation_time: u32,
}

/// The local entity type facts `initGhost` needs: how many properties are
/// ghosted, and how many are event-stamped (`propertyEventStamps_.init`
/// resizes to the stamped count, `readGhostDataFromStreamInternal` reads
/// exactly that many).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhostEntityTypeDesc {
    /// `pEntityType_->propCountGhost()`.
    pub prop_count_ghost: usize,
    /// `entityDescription.numEventStampedProperties()`.
    pub num_event_stamped_properties: usize,
}

/// What `Space::createGhost(srcAddr, data)` did with the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhostCreateOutcome {
    /// The immediate path: the ghost was created.
    Created,
    /// The entity already exists here, or messages from this real are
    /// already queued (`space.cpp:236-266`): the message was buffered for
    /// a later replay.
    Buffered,
    /// The payload names a type this app does not know (the reference
    /// MF-asserts on the lookup).
    UnknownEntityType,
    /// The streamed stamp count does not match the local type
    /// (`removeFromStream` reads exactly `eventStamps_.size()` entries).
    StampCountMismatch,
    /// The streamed property count does not match the local type's
    /// `propCountGhost`.
    PropertyCountMismatch,
}

/// The receiving CellApp's ghost surface: the ghost records of one space.
#[derive(Debug, Default)]
pub struct GhostHost {
    /// `app.findEntity` + the ghost population, keyed by entity id.
    pub ghosts: BTreeMap<EntityId, GhostRecord>,
    /// The local `EntityTypeDB` slice the creates are validated against.
    pub types: BTreeMap<u16, GhostEntityTypeDesc>,
    /// `BufferedGhostMessages` — the out-of-order real->ghost messages.
    pub buffer: GhostBuffer,
}

impl GhostHost {
    /// Register a local entity type (`EntityTypeDB` lookup fact).
    pub fn register_type(&mut self, entity_type_id: u16, desc: GhostEntityTypeDesc) {
        self.types.insert(entity_type_id, desc);
    }

    /// `Space::createGhost(srcAddr, header, data)`
    /// (`space.cpp:228-266`): an existing entity (or pending buffer) takes
    /// the buffered path, otherwise the ghost is created at once.
    pub fn receive_create_ghost(
        &mut self,
        src: Addr,
        payload: GhostCreatePayload,
        time: u32,
    ) -> GhostCreateOutcome {
        let entity_id = payload.entity_id;
        if self.ghosts.contains_key(&entity_id) || self.buffer.has_messages_for(entity_id, src) {
            let msg = BufferedGhostMessage::CreateGhost { src, payload };
            if self.buffer.has_messages_for(entity_id, src) {
                self.buffer.add(entity_id, src, msg);
            } else {
                self.buffer.delay_subsequence(entity_id, src, msg);
            }
            return GhostCreateOutcome::Buffered;
        }

        let Some(desc) = self.types.get(&payload.entity_type_id) else {
            return GhostCreateOutcome::UnknownEntityType;
        };
        if payload.property_event_stamps.len() != desc.num_event_stamped_properties {
            return GhostCreateOutcome::StampCountMismatch;
        }
        if payload.ghost_properties.len() != desc.prop_count_ghost {
            return GhostCreateOutcome::PropertyCountMismatch;
        }

        // `readGhostsFromStream` (`controllers.cpp:50-73`): create each
        // ghost-domain controller and start it.
        let mut controllers = Controllers::new();
        for seed in &payload.ghost_controllers {
            controllers.create_ghost(seed.id, seed.type_id, seed.user_arg, seed.payload.clone());
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
                next_real_addr: None,
                base_addr: payload.base_addr,
                direction: payload.direction,
                property_event_stamps: payload.property_event_stamps,
                last_trimmed_event_number: payload.last_event_number,
                ghost_properties: payload.ghost_properties,
                controllers,
                periods_without_witness: payload.periods_without_witness,
                aoi_update_scheme_id: payload.aoi_update_scheme_id,
                creation_time: time,
            },
        );
        GhostCreateOutcome::Created
    }

    /// The `delGhost` receive: the ghost goes; a second notice finds
    /// nothing. Returns whether a ghost was removed. Like `Entity::destroy`
    /// (`entity.cpp:3007`), a removal then offers the buffered queues a new
    /// lifespan (a queued `createGhost` may now start).
    pub fn receive_del_ghost(&mut self, entity_id: EntityId, time: u32) -> bool {
        if self.ghosts.remove(&entity_id).is_none() {
            return false;
        }
        self.play_new_lifespan_for(entity_id, time);
        true
    }

    /// `Entity::ghostSetNextReal` (`entity.cpp:4961-4979`): the offloading
    /// real's sign-off. Only the first sign-off takes (`nextRealAddr_` must
    /// be NONE). This is the last message from the current real, so from
    /// now on only the new address is accepted — and any buffered
    /// subsequence from it plays (`playSubsequenceFor`).
    pub fn receive_ghost_set_next_real(
        &mut self,
        entity_id: EntityId,
        next_real_addr: Addr,
        time: u32,
    ) -> bool {
        let Some(record) = self.ghosts.get_mut(&entity_id) else {
            return false;
        };
        if record.next_real_addr.is_some() {
            return false;
        }
        record.next_real_addr = Some(next_real_addr);
        self.play_subsequence_for(entity_id, next_real_addr, time);
        true
    }

    /// `Entity::ghostSetReal` (`entity.cpp:4927-4955`): the loaded real's
    /// routing update. The subsequence number must be exactly one past the
    /// ghost's; a mismatch is delayed (`delaySubsequence`) for the buffered
    /// replay, leaving the routing table untouched.
    pub fn receive_ghost_set_real(
        &mut self,
        entity_id: EntityId,
        num_times_real_offloaded: u32,
        owner: Addr,
    ) -> GhostSetRealOutcome {
        let Some(record) = self.ghosts.get_mut(&entity_id) else {
            return GhostSetRealOutcome::UnknownEntity;
        };

        let expected = record.num_times_real_offloaded + 1;
        if num_times_real_offloaded != expected {
            // `entity.cpp:4941-4945`: the stale update heads its own
            // delayed subsequence until a sign-off plays it.
            self.buffer.delay_subsequence(
                entity_id,
                owner,
                BufferedGhostMessage::Common(GhostMessage::SetReal(GhostSetReal {
                    entity_id,
                    num_times_real_offloaded,
                    owner,
                })),
            );
            return GhostSetRealOutcome::DelayedSubsequence;
        }

        record.num_times_real_offloaded = num_times_real_offloaded;
        record.real_addr = owner;
        record.next_real_addr = None;
        // `relocated()` (`entity.cpp:4204-4211`) only notifies entity extras,
        // which this port does not model.
        GhostSetRealOutcome::Relocated
    }
}

/// What `Entity::ghostSetReal` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhostSetRealOutcome {
    /// The routing table moved to the new real.
    Relocated,
    /// Out of subsequence order — the message is delayed
    /// (`delaySubsequence`, replayed by the buffered-ghost pass).
    DelayedSubsequence,
    /// No such ghost here.
    UnknownEntity,
}

// -----------------------------------------------------------------------------
// Section: buffered ghost messages (`buffered_ghost_message*.cpp`)
// -----------------------------------------------------------------------------

/// A real->ghost message held while its subsequence is out of order.
/// The three reference plays are `BufferedCreateGhostMessage`,
/// `BufferedGhostSetRealMessage` and `CommonBufferedGhostMessage`
/// (`buffered_ghost_message_factory.cpp`).
#[derive(Clone, Debug)]
pub enum BufferedGhostMessage {
    /// A `createGhost` that arrived too early; it carries the sender so a
    /// replay that finds the ghost alive can re-delay onto the same queue.
    CreateGhost {
        src: Addr,
        payload: GhostCreatePayload,
    },
    /// Any other ghost message, replayed through its normal receiver.
    Common(GhostMessage),
}

impl BufferedGhostMessage {
    /// `isSubsequenceStart` (`buffered_ghost_messages.hpp:57-59`) plus the
    /// two message classes the factory hard-marks as starts.
    pub fn is_subsequence_start(&self) -> bool {
        match self {
            BufferedGhostMessage::CreateGhost { .. } => true,
            BufferedGhostMessage::Common(
                GhostMessage::CreateGhost(_) | GhostMessage::SetReal(_),
            ) => true,
            BufferedGhostMessage::Common(_) => false,
        }
    }

    /// `isSubsequenceEnd` (`buffered_ghost_messages.hpp:62-65`).
    pub fn is_subsequence_end(&self) -> bool {
        matches!(
            self,
            BufferedGhostMessage::Common(
                GhostMessage::DelGhost { .. } | GhostMessage::SetNextReal(_),
            )
        )
    }

    /// `isCreateGhostMessage`.
    pub fn is_create_ghost(&self) -> bool {
        matches!(
            self,
            BufferedGhostMessage::CreateGhost { .. }
                | BufferedGhostMessage::Common(GhostMessage::CreateGhost(_))
        )
    }
}

/// The stream of buffered real->ghost messages from one CellApp for one
/// entity (`BufferedGhostMessageQueue`). Only ordering within the queue is
/// guaranteed; the per-entity map works out the order between queues.
#[derive(Debug, Default)]
pub struct GhostMessageQueue {
    messages: VecDeque<BufferedGhostMessage>,
    /// `pDelayedMessages_` — an old (unexpected) subsequence being
    /// collected until its end marker shows up.
    delayed: Option<Vec<BufferedGhostMessage>>,
}

impl GhostMessageQueue {
    /// `add`: while delaying, late arrivals join the delayed list;
    /// otherwise the queue must keep starting at a subsequence start.
    fn add(&mut self, msg: BufferedGhostMessage) {
        if self.is_delaying() {
            self.delayed.as_mut().unwrap().push(msg);
        } else {
            debug_assert!(self.messages.is_empty() || self.messages[0].is_subsequence_start());
            self.messages.push_back(msg);
        }
    }

    /// `isDelaying`: mid-collection of a delayed subsequence.
    fn is_delaying(&self) -> bool {
        self.delayed
            .as_ref()
            .is_some_and(|d| !d.last().unwrap().is_subsequence_end())
    }

    /// `delaySubsequence`: head a subsequence with `first`, then swallow
    /// the front of the queue until the delayed list reaches an end.
    fn delay_subsequence(&mut self, first: BufferedGhostMessage) {
        debug_assert!(first.is_subsequence_start());
        self.delayed.get_or_insert_with(Vec::new).push(first);
        while !self.messages.is_empty() && self.is_delaying() {
            self.delayed
                .as_mut()
                .unwrap()
                .push(self.messages.pop_front().unwrap());
        }
    }

    /// `promoteDelayedSubsequences`: un-delay the collected messages.
    fn promote_delayed(&mut self) {
        if let Some(delayed) = self.delayed.take() {
            for msg in delayed.into_iter().rev() {
                self.messages.push_front(msg);
            }
        }
    }

    /// `isEmpty` — the playable list only (a delaying queue reports
    /// empty, matching `hasMessagesFrom`'s check).
    fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    fn front_is_subsequence_end(&self) -> bool {
        self.messages
            .front()
            .is_some_and(|m| m.is_subsequence_end())
    }

    fn front_is_create_ghost(&self) -> bool {
        self.messages.front().is_some_and(|m| m.is_create_ghost())
    }
}

/// `BufferedGhostMessages` — the out-of-order real->ghost messages of one
/// app, keyed by entity, then by the CellApp they came from
/// (`buffered_ghost_messages.cpp`). The ghost lifespan is a chain of
/// subsequences, one per real the entity visits; the sign-off
/// (`ghostSetNextReal`) tells the ghost which subsequence plays next.
#[derive(Debug, Default)]
pub struct GhostBuffer {
    entries: BTreeMap<EntityId, BTreeMap<Addr, GhostMessageQueue>>,
}

impl GhostBuffer {
    /// `add`.
    pub fn add(&mut self, entity_id: EntityId, src: Addr, msg: BufferedGhostMessage) {
        self.entries
            .entry(entity_id)
            .or_default()
            .entry(src)
            .or_default()
            .add(msg);
    }

    /// `hasMessagesFor` — a playable (non-delaying) queue from `src`.
    pub fn has_messages_for(&self, entity_id: EntityId, src: Addr) -> bool {
        self.queue_for(entity_id, src)
            .is_some_and(|q| !q.is_empty())
    }

    /// `isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether any queue (playable or delaying) exists from `src` — the
    /// `ForEntity::playSubsequence` lookup the plays are gated on.
    pub fn has_queue_for(&self, entity_id: EntityId, src: Addr) -> bool {
        self.queue_for(entity_id, src).is_some()
    }

    /// `isDelayingMessagesFor`.
    pub fn is_delaying_for(&self, entity_id: EntityId, src: Addr) -> bool {
        self.queue_for(entity_id, src)
            .is_some_and(|q| q.is_delaying())
    }

    fn queue_for(&self, entity_id: EntityId, src: Addr) -> Option<&GhostMessageQueue> {
        self.entries.get(&entity_id)?.get(&src)
    }

    /// `delaySubsequence`.
    pub fn delay_subsequence(
        &mut self,
        entity_id: EntityId,
        src: Addr,
        first: BufferedGhostMessage,
    ) {
        self.entries
            .entry(entity_id)
            .or_default()
            .entry(src)
            .or_default()
            .delay_subsequence(first);
    }

    fn promote_delayed_for(&mut self, entity_id: EntityId, src: Addr) {
        if let Some(q) = self
            .entries
            .get_mut(&entity_id)
            .and_then(|m| m.get_mut(&src))
        {
            q.promote_delayed();
        }
    }

    /// The next playable message plus the facts `playFront` needs:
    /// (message, queue-drained-before-play, front-was-a-subsequence-end).
    fn next_to_play(
        &mut self,
        entity_id: EntityId,
        src: Addr,
    ) -> Option<(BufferedGhostMessage, bool, bool)> {
        let q = self.entries.get_mut(&entity_id)?.get_mut(&src)?;
        if q.is_empty() {
            return None;
        }
        let finished = q.front_is_subsequence_end();
        let msg = q.messages.pop_front()?;
        let empty_before_play = q.is_empty() && q.delayed.is_none();
        Some((msg, empty_before_play, finished))
    }

    /// The queue's post-play state: `Some((messages_empty, delayed))`,
    /// `None` when the queue no longer exists (a nested play erased it).
    fn queue_state(
        &self,
        entity_id: EntityId,
        src: Addr,
    ) -> Option<(bool, Option<&Vec<BufferedGhostMessage>>)> {
        let q = self.queue_for(entity_id, src)?;
        Some((q.is_empty(), q.delayed.as_ref()))
    }

    fn erase_queue(&mut self, entity_id: EntityId, src: Addr) {
        if let Some(queues) = self.entries.get_mut(&entity_id) {
            queues.remove(&src);
            if queues.is_empty() {
                self.entries.remove(&entity_id);
            }
        }
    }

    fn peek_first_create_lifespan(&mut self, entity_id: EntityId) -> Option<Addr> {
        let queues = self.entries.get_mut(&entity_id)?;
        for (&addr, queue) in queues.iter_mut() {
            queue.promote_delayed();
            if queue.front_is_create_ghost() {
                return Some(addr);
            }
        }
        None
    }
}

impl GhostHost {
    /// `BufferedGhostMessages::playSubsequenceFor` (`buffered_ghost_messages
    /// .cpp:22-40`): play the buffered subsequence from `src` up to its end,
    /// erasing the queue when it drains.
    pub fn play_subsequence_for(&mut self, entity_id: EntityId, src: Addr, time: u32) {
        if !self.buffer.has_queue_for(entity_id, src) {
            // "Not playing buffered ghost messages ... still talking to
            // real at %s" — no queue from that real exists at all.
            return;
        }
        self.play_queue(entity_id, src, time);
    }

    /// `BufferedGhostMessages::playNewLifespanFor` (`buffered_ghost_messages
    /// .cpp:42-55`): after this ghost was destroyed, a queue whose front is
    /// a `createGhost` starts the entity's next lifespan here.
    pub fn play_new_lifespan_for(&mut self, entity_id: EntityId, time: u32) {
        let Some(src) = self.buffer.peek_first_create_lifespan(entity_id) else {
            return;
        };
        self.play_queue(entity_id, src, time);
    }

    /// The shared drain loop (`BufferedGhostMessageQueue::playSubsequence`):
    /// play until a subsequence end has been played and the queue did not
    /// refill; a drained queue is erased.
    fn play_queue(&mut self, entity_id: EntityId, src: Addr, time: u32) {
        self.buffer.promote_delayed_for(entity_id, src);

        while let Some((msg, empty_before_play, finished)) =
            self.buffer.next_to_play(entity_id, src)
        {
            self.dispatch_buffered(msg, time);

            match self.buffer.queue_state(entity_id, src) {
                None => return,
                Some((messages_empty, delayed)) => {
                    if empty_before_play && messages_empty && delayed.is_none() {
                        self.buffer.erase_queue(entity_id, src);
                        return;
                    }
                }
            }
            if finished {
                return;
            }
        }
    }

    /// The three buffered plays (`buffered_ghost_message_factory.cpp`).
    fn dispatch_buffered(&mut self, msg: BufferedGhostMessage, time: u32) {
        match msg {
            BufferedGhostMessage::CreateGhost { src, payload } => {
                let entity_id = payload.entity_id;
                if self.ghosts.contains_key(&entity_id) {
                    // The ghost (re)appeared before its create could play:
                    // hold the whole subsequence again.
                    self.buffer.delay_subsequence(
                        entity_id,
                        src,
                        BufferedGhostMessage::CreateGhost { src, payload },
                    );
                } else {
                    let _ = self.receive_create_ghost(src, payload, time);
                }
            }
            BufferedGhostMessage::Common(GhostMessage::CreateGhost(payload)) => {
                let src = payload.real_addr;
                if self.ghosts.contains_key(&payload.entity_id) {
                    self.buffer.delay_subsequence(
                        payload.entity_id,
                        src,
                        BufferedGhostMessage::Common(GhostMessage::CreateGhost(payload)),
                    );
                } else {
                    let _ = self.receive_create_ghost(src, payload, time);
                }
            }
            BufferedGhostMessage::Common(GhostMessage::SetReal(set)) => {
                // A stale update re-delays inside the receiver
                // (`Entity::ghostSetReal`'s own mismatch arm) — no
                // dispatcher-side copy, or the message lands twice.
                let _ = self.receive_ghost_set_real(
                    set.entity_id,
                    set.num_times_real_offloaded,
                    set.owner,
                );
            }
            BufferedGhostMessage::Common(GhostMessage::DelGhost { entity_id }) => {
                let _ = self.receive_del_ghost(entity_id, time);
            }
            BufferedGhostMessage::Common(GhostMessage::SetNextReal(set)) => {
                let _ = self.receive_ghost_set_next_real(set.entity_id, set.next_real_addr, time);
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Section: the real-side handover (`real_entity.cpp`)
// -----------------------------------------------------------------------------

/// The flags `RealEntity::readOffloadData` branches on
/// (`real_entity.cpp:315-405`).
#[derive(Clone, Debug, Default)]
pub struct OnloadHauntContext {
    /// `hasChangedSpace` — the offload crossed a space boundary.
    pub has_changed_space: bool,
    /// `isTeleported`.
    pub is_teleported: bool,
    /// `pBadHauntAddr` — skipped during an onload from a failed teleport.
    pub bad_haunt_addr: Option<Addr>,
}

/// `RealEntity::destroy`'s offload sign-off (`real_entity.cpp:250-270`):
/// every haunt except the destination learns where the real is going.
pub fn offload_sign_off(
    real: &RealEntity,
    next_real_addr: Addr,
    channels: &mut BTreeMap<Addr, GhostChannel>,
) {
    for haunt in &real.haunts {
        if haunt.addr == next_real_addr {
            continue;
        }
        let channel = channels
            .entry(haunt.addr)
            .or_insert_with(|| GhostChannel::new(haunt.addr));
        channel
            .out
            .push(GhostMessage::SetNextReal(GhostSetNextReal {
                entity_id: real.id,
                next_real_addr,
            }));
    }
}

/// `RealEntity::readOffloadData`'s haunt pass (`real_entity.cpp:353-405`):
/// the loaded real re-haunts the ghost apps and tells each one where the
/// real now lives — or, across a space change or teleport, deletes the
/// ghost instead. Returns `areWeHaunted`: whether our own address was in
/// the list — a checkable stand-in for the reference's
/// `MF_ASSERT( areWeHaunted )` (the offload destination always ghosts the
/// entity before the move).
pub fn onload_notify_haunts(
    real: &mut RealEntity,
    haunt_addrs: &[Addr],
    channels: &mut BTreeMap<Addr, GhostChannel>,
    own_addr: Addr,
    num_times_real_offloaded: u32,
    time: u32,
    context: &OnloadHauntContext,
) -> bool {
    let mut are_we_haunted = false;

    for addr in haunt_addrs {
        // We don't care if it's our own address — it could be from a local
        // teleport or some such.
        if *addr == own_addr {
            are_we_haunted = true;
            continue;
        }

        // Skipped during an onload from a failed teleport: no ghost was
        // created on the destination.
        if context.bad_haunt_addr == Some(*addr) {
            continue;
        }

        // No channel means the app is unknown to us: same skip as a dead
        // one (`Not notifying ghost for %u on dead app`).
        let Some(channel) = channels.get_mut(addr) else {
            continue;
        };

        if !channel.is_good {
            continue;
        }

        if !context.has_changed_space && !context.is_teleported {
            real.haunts.push(Haunt {
                addr: *addr,
                creation_time: time,
            });
            channel.out.push(GhostMessage::SetReal(GhostSetReal {
                entity_id: real.id,
                num_times_real_offloaded,
                owner: own_addr,
            }));
        } else {
            channel
                .out
                .push(GhostMessage::DelGhost { entity_id: real.id });
        }
    }

    are_we_haunted
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
    /// `localDirection()`.
    pub direction: Vector3,
    /// `propertyEventStamps()`.
    pub property_event_stamps: Vec<i32>,
    /// The ghosted properties (`propCountGhost` leading values).
    pub ghost_properties: Vec<PropValue>,
    /// The ghost-domain controllers (`writeGhostsToStream`).
    pub ghost_controllers: Vec<GhostControllerSeed>,
    /// `periodsWithoutWitness()`.
    pub periods_without_witness: u16,
    /// `aoiUpdateSchemeID()`.
    pub aoi_update_scheme_id: u8,
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
    const APP_E: Addr = (5, 5);

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
            direction: Vector3::new(1.5, 0.25, -0.5),
            property_event_stamps: vec![7, 13],
            ghost_properties: vec![PropValue::Int(5), PropValue::Str("hi".to_string())],
            ghost_controllers: vec![GhostControllerSeed {
                id: 3,
                type_id: 9,
                user_arg: 42,
                payload: vec![1, 2, 3],
            }],
            periods_without_witness: 4,
            aoi_update_scheme_id: 3,
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
                direction: Vector3::new(1.5, 0.25, -0.5),
                property_event_stamps: vec![7, 13],
                ghost_properties: vec![PropValue::Int(5), PropValue::Str("hi".to_string())],
                ghost_controllers: vec![GhostControllerSeed {
                    id: 3,
                    type_id: 9,
                    user_arg: 42,
                    payload: vec![1, 2, 3],
                }],
                periods_without_witness: 4,
                aoi_update_scheme_id: 3,
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
                    direction: Vector3::new(1.5, 0.25, -0.5),
                    property_event_stamps: vec![7, 13],
                    ghost_properties: vec![PropValue::Int(5), PropValue::Str("hi".to_string())],
                    ghost_controllers: vec![GhostControllerSeed {
                        id: 3,
                        type_id: 9,
                        user_arg: 42,
                        payload: vec![1, 2, 3],
                    }],
                    periods_without_witness: 4,
                    aoi_update_scheme_id: 3,
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
    fn the_ghost_history_starts_trimmed_at_the_real() {
        let mut host = GhostHost::default();
        host.register_type(
            4,
            GhostEntityTypeDesc {
                prop_count_ghost: 0,
                num_event_stamped_properties: 0,
            },
        );
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 12,
            entity_type_id: 4,
            num_times_real_offloaded: 0,
            position: Vector3::ZERO,
            is_on_ground: true,
            last_event_number: 31,
            volatile: VolatileInfo::NONE,
            real_addr: APP_A,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![],
            ghost_properties: vec![],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };
        assert_eq!(
            host.receive_create_ghost(APP_A, payload, 5),
            GhostCreateOutcome::Created
        );
        // `eventHistory_.lastTrimmedEventNumber( lastEventNumber_ )`: the
        // ghost never replays events the real already handed out.
        assert_eq!(host.ghosts[&12].last_trimmed_event_number, 31);
    }

    #[test]
    fn a_create_for_an_unknown_type_or_mismatched_body_is_rejected() {
        let mut host = GhostHost::default();
        host.register_type(
            4,
            GhostEntityTypeDesc {
                prop_count_ghost: 2,
                num_event_stamped_properties: 1,
            },
        );
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 12,
            entity_type_id: 5,
            num_times_real_offloaded: 0,
            position: Vector3::ZERO,
            is_on_ground: true,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            real_addr: APP_A,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![1],
            ghost_properties: vec![PropValue::Int(0), PropValue::Int(0)],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };

        // Unknown type: the local EntityTypeDB has no such entry.
        assert_eq!(
            host.receive_create_ghost(APP_A, payload.clone(), 5),
            GhostCreateOutcome::UnknownEntityType
        );

        // Wrong stamp count: removeFromStream would read past the message.
        let mut payload = payload;
        payload.entity_type_id = 4;
        payload.property_event_stamps = vec![1, 2];
        assert_eq!(
            host.receive_create_ghost(APP_A, payload.clone(), 5),
            GhostCreateOutcome::StampCountMismatch
        );

        // Wrong ghosted-property count.
        payload.property_event_stamps = vec![1];
        payload.ghost_properties = vec![PropValue::Int(0)];
        assert_eq!(
            host.receive_create_ghost(APP_A, payload, 5),
            GhostCreateOutcome::PropertyCountMismatch
        );
        assert!(host.ghosts.is_empty());
    }

    #[test]
    fn a_routing_update_moves_the_ghost_to_the_new_real() {
        let (cells, mut channels, mut maintainer) = two_cell_world();
        let (mut real, view) = ghosted_real(10, 90.0);

        // The real ghosts onto B and the ghost starts routed to A.
        maintainer.check(&cells, &mut channels, &mut real, &view, 100);
        let mut host = GhostHost::default();
        host.register_type(
            4,
            GhostEntityTypeDesc {
                prop_count_ghost: 2,
                num_event_stamped_properties: 2,
            },
        );
        let GhostMessage::CreateGhost(payload) = channels[&APP_B].out[0].clone() else {
            panic!("expected a create");
        };
        assert_eq!(
            host.receive_create_ghost(APP_A, payload, 100),
            GhostCreateOutcome::Created
        );
        assert_eq!(host.ghosts[&10].real_addr, APP_A);
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 0);

        // The real offloads to C: the sign-off tells B where the real is
        // going, and B records the handover.
        offload_sign_off(&real, APP_C, &mut channels);
        assert_eq!(
            channels[&APP_B].out[1],
            GhostMessage::SetNextReal(GhostSetNextReal {
                entity_id: 10,
                next_real_addr: APP_C,
            })
        );
        assert!(host.receive_ghost_set_next_real(10, APP_C, 300));
        assert_eq!(host.ghosts[&10].next_real_addr, Some(APP_C));

        // The handover address is taken exactly once: a duplicate
        // sign-off (a retransmit) never moves it again.
        assert!(!host.receive_ghost_set_next_real(10, APP_A, 300));
        assert_eq!(host.ghosts[&10].next_real_addr, Some(APP_C));

        // C loads the entity and re-haunts B with the routing update.
        // C ghosted the entity before the move (offload checker rule), so
        // C itself is in the haunt list — the areWeHaunted assert holds.
        let mut landed = RealEntity::new(10, (9, 9), Vector3::new(140.0, 0.0, 50.0), 300, 0);
        let haunted = onload_notify_haunts(
            &mut landed,
            &[APP_B, APP_C],
            &mut channels,
            APP_C,
            1,
            300,
            &OnloadHauntContext::default(),
        );
        assert!(haunted);
        assert_eq!(
            landed.haunts,
            vec![Haunt {
                addr: APP_B,
                creation_time: 300
            }]
        );
        assert_eq!(
            channels[&APP_B].out[2],
            GhostMessage::SetReal(GhostSetReal {
                entity_id: 10,
                num_times_real_offloaded: 1,
                owner: APP_C,
            })
        );

        // B applies the update: the ghost now routes to C, and the next
        // message down that route (a delGhost from C) lands on it.
        assert_eq!(
            host.receive_ghost_set_real(10, 1, APP_C),
            GhostSetRealOutcome::Relocated
        );
        assert_eq!(host.ghosts[&10].real_addr, APP_C);
        assert_eq!(host.ghosts[&10].next_real_addr, None);
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 1);
        assert!(host.receive_del_ghost(10, 200));
    }

    #[test]
    fn a_set_real_out_of_subsequence_is_delayed_not_applied() {
        let mut host = GhostHost::default();
        host.register_type(
            4,
            GhostEntityTypeDesc {
                prop_count_ghost: 0,
                num_event_stamped_properties: 0,
            },
        );
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 10,
            entity_type_id: 4,
            num_times_real_offloaded: 0,
            position: Vector3::ZERO,
            is_on_ground: true,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            real_addr: APP_A,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![],
            ghost_properties: vec![],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };
        assert_eq!(
            host.receive_create_ghost(APP_A, payload, 100),
            GhostCreateOutcome::Created
        );

        // A stale or duplicated update skips its subsequence number: the
        // reference delays it for the buffered replay and keeps routing.
        assert_eq!(
            host.receive_ghost_set_real(10, 5, APP_C),
            GhostSetRealOutcome::DelayedSubsequence
        );
        assert_eq!(host.ghosts[&10].real_addr, APP_A);
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 0);

        // The right number still lands afterwards.
        assert_eq!(
            host.receive_ghost_set_real(10, 1, APP_C),
            GhostSetRealOutcome::Relocated
        );
        assert_eq!(host.ghosts[&10].real_addr, APP_C);

        // An unknown entity has nothing to route.
        assert_eq!(
            host.receive_ghost_set_real(11, 1, APP_C),
            GhostSetRealOutcome::UnknownEntity
        );
        assert!(!host.receive_ghost_set_next_real(11, APP_C, 300));
    }

    /// A ghost holding one real, for the buffering tests.
    fn hold_host(entity_id: EntityId, num_times_real_offloaded: u32) -> GhostHost {
        let mut host = GhostHost::default();
        host.register_type(
            4,
            GhostEntityTypeDesc {
                prop_count_ghost: 0,
                num_event_stamped_properties: 0,
            },
        );
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id,
            entity_type_id: 4,
            num_times_real_offloaded,
            position: Vector3::ZERO,
            is_on_ground: true,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            real_addr: APP_A,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![],
            ghost_properties: vec![],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };
        assert_eq!(
            host.receive_create_ghost(APP_A, payload, 100),
            GhostCreateOutcome::Created
        );
        host
    }

    #[test]
    fn buffered_flags_classify_the_reference_subsequence_markers() {
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 10,
            entity_type_id: 4,
            num_times_real_offloaded: 0,
            position: Vector3::ZERO,
            is_on_ground: true,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            real_addr: APP_A,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![],
            ghost_properties: vec![],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };
        let create = BufferedGhostMessage::CreateGhost {
            src: APP_C,
            payload,
        };
        assert!(create.is_subsequence_start() && create.is_create_ghost());
        assert!(!create.is_subsequence_end());

        let set_real = BufferedGhostMessage::Common(GhostMessage::SetReal(GhostSetReal {
            entity_id: 10,
            num_times_real_offloaded: 1,
            owner: APP_C,
        }));
        assert!(set_real.is_subsequence_start());
        assert!(!set_real.is_subsequence_end() && !set_real.is_create_ghost());

        let del = BufferedGhostMessage::Common(GhostMessage::DelGhost { entity_id: 10 });
        let set_next = BufferedGhostMessage::Common(GhostMessage::SetNextReal(GhostSetNextReal {
            entity_id: 10,
            next_real_addr: APP_C,
        }));
        assert!(del.is_subsequence_end() && set_next.is_subsequence_end());
        assert!(!del.is_subsequence_start() && !set_next.is_subsequence_start());
    }

    #[test]
    fn a_stale_set_real_plays_when_its_subsequence_is_signed_off() {
        // The real offloaded twice in quick succession (A->C->D) and the
        // D->B channel beat C's update here: the ghost's number is 0, so
        // SetReal{2, D} is out of subsequence.
        let mut host = hold_host(10, 0);
        assert_eq!(
            host.receive_ghost_set_real(10, 2, APP_D),
            GhostSetRealOutcome::DelayedSubsequence
        );
        // A delaying queue reports empty to `hasMessagesFor` but is
        // mid-collection to `isDelayingMessagesFor`.
        assert!(!host.buffer.has_messages_for(10, APP_D));
        assert!(host.buffer.is_delaying_for(10, APP_D));

        // C's update is in order and applies straight away.
        assert_eq!(
            host.receive_ghost_set_real(10, 1, APP_C),
            GhostSetRealOutcome::Relocated
        );

        // C's sign-off hands routing to D — which releases the held
        // update, and the ghost now routes to the right process.
        assert!(host.receive_ghost_set_next_real(10, APP_D, 300));
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 2);
        assert_eq!(host.ghosts[&10].real_addr, APP_D);
        assert_eq!(host.ghosts[&10].next_real_addr, None);
        assert!(host.buffer.is_empty());
    }

    #[test]
    fn a_create_for_a_live_ghost_re_delays_instead_of_duplicating() {
        let mut host = hold_host(10, 0);
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 10,
            entity_type_id: 4,
            num_times_real_offloaded: 1,
            position: Vector3::new(12.0, 3.0, 4.0),
            is_on_ground: false,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            real_addr: APP_C,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![],
            ghost_properties: vec![],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };

        // The create arrives while the ghost is still alive: buffered.
        assert_eq!(
            host.receive_create_ghost(APP_C, payload, 150),
            GhostCreateOutcome::Buffered
        );
        assert_eq!(host.ghosts.len(), 1);
        assert!(host.buffer.is_delaying_for(10, APP_C));

        // The sign-off plays it, but the ghost still lives: the create
        // re-delays rather than duplicating the record.
        assert!(host.receive_ghost_set_next_real(10, APP_C, 300));
        assert_eq!(host.ghosts.len(), 1);
        assert_eq!(host.ghosts[&10].real_addr, APP_A);
        assert!(host.buffer.is_delaying_for(10, APP_C));

        // The death starts a new lifespan: the held create replays once.
        assert!(host.receive_del_ghost(10, 400));
        assert_eq!(host.ghosts.len(), 1);
        assert_eq!(host.ghosts[&10].real_addr, APP_C);
        assert_eq!(host.ghosts[&10].position, Vector3::new(12.0, 3.0, 4.0));
        assert_eq!(host.ghosts[&10].creation_time, 400);
        assert!(host.buffer.is_empty());
    }

    #[test]
    fn a_chained_sign_off_plays_each_subsequence_in_turn() {
        let mut host = hold_host(10, 0);

        // Two whole subsequences queued: C's handover to D, then D's own
        // routing update.
        host.buffer.add(
            10,
            APP_C,
            BufferedGhostMessage::Common(GhostMessage::SetReal(GhostSetReal {
                entity_id: 10,
                num_times_real_offloaded: 1,
                owner: APP_C,
            })),
        );
        host.buffer.add(
            10,
            APP_C,
            BufferedGhostMessage::Common(GhostMessage::SetNextReal(GhostSetNextReal {
                entity_id: 10,
                next_real_addr: APP_D,
            })),
        );
        host.buffer.add(
            10,
            APP_D,
            BufferedGhostMessage::Common(GhostMessage::SetReal(GhostSetReal {
                entity_id: 10,
                num_times_real_offloaded: 2,
                owner: APP_D,
            })),
        );

        // Signing off to C plays C's subsequence; its trailing handover
        // chains straight into D's.
        assert!(host.receive_ghost_set_next_real(10, APP_C, 300));
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 2);
        assert_eq!(host.ghosts[&10].real_addr, APP_D);
        assert_eq!(host.ghosts[&10].next_real_addr, None);
        assert!(host.buffer.is_empty());
    }

    #[test]
    fn a_delaying_subsequence_collects_until_its_end() {
        let mut host = hold_host(10, 1);

        // Out of subsequence: the update heads a delayed list.
        assert_eq!(
            host.receive_ghost_set_real(10, 3, APP_D),
            GhostSetRealOutcome::DelayedSubsequence
        );
        assert!(host.buffer.is_delaying_for(10, APP_D));

        // Late arrivals join the delayed list until an end marker closes
        // the collection.
        host.buffer.add(
            10,
            APP_D,
            BufferedGhostMessage::Common(GhostMessage::DelGhost { entity_id: 10 }),
        );
        assert!(!host.buffer.is_delaying_for(10, APP_D));
        assert!(!host.buffer.has_messages_for(10, APP_D));

        // The skipped update arrives and moves the ghost's count to 2,
        // making the held {3, D} the next expected subsequence number.
        assert_eq!(
            host.receive_ghost_set_real(10, 2, APP_C),
            GhostSetRealOutcome::Relocated
        );

        // The sign-off promotes and plays both, in order.
        assert!(host.receive_ghost_set_next_real(10, APP_D, 300));
        assert!(!host.ghosts.contains_key(&10));
        assert!(host.buffer.is_empty());
    }

    #[test]
    fn a_create_while_delaying_joins_the_delayed_subsequence() {
        let mut host = hold_host(10, 1);
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 10,
            entity_type_id: 4,
            num_times_real_offloaded: 3,
            position: Vector3::new(1.0, 2.0, 3.0),
            is_on_ground: true,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            real_addr: APP_D,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![],
            ghost_properties: vec![],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };

        assert_eq!(
            host.receive_ghost_set_real(10, 3, APP_D),
            GhostSetRealOutcome::DelayedSubsequence
        );

        // No playable queue yet (`hasMessagesFor` is false while
        // delaying), so the create delays too — after the held update.
        assert_eq!(
            host.receive_create_ghost(APP_D, payload, 150),
            GhostCreateOutcome::Buffered
        );
        assert!(host.buffer.is_delaying_for(10, APP_D));

        // The skipped update arrives, making the held {3, D} next in line.
        assert_eq!(
            host.receive_ghost_set_real(10, 2, APP_C),
            GhostSetRealOutcome::Relocated
        );

        // The sign-off applies the update; the replayed create still finds
        // the ghost alive and holds again — exactly one record throughout.
        assert!(host.receive_ghost_set_next_real(10, APP_D, 300));
        assert_eq!(host.ghosts.len(), 1);
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 3);
        assert_eq!(host.ghosts[&10].real_addr, APP_D);
        assert!(host.buffer.is_delaying_for(10, APP_D));
    }

    #[test]
    fn a_buffered_create_behind_a_playable_queue_appends_to_it() {
        let mut host = hold_host(10, 0);

        // A playable queue from C already exists, so the early create
        // joins it (`Space::createGhost`'s add branch) instead of heading
        // its own delayed subsequence.
        host.buffer.add(
            10,
            APP_C,
            BufferedGhostMessage::Common(GhostMessage::SetReal(GhostSetReal {
                entity_id: 10,
                num_times_real_offloaded: 1,
                owner: APP_C,
            })),
        );
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 10,
            entity_type_id: 4,
            num_times_real_offloaded: 1,
            position: Vector3::ZERO,
            is_on_ground: true,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            real_addr: APP_C,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![],
            ghost_properties: vec![],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };
        assert_eq!(
            host.receive_create_ghost(APP_C, payload, 150),
            GhostCreateOutcome::Buffered
        );
        assert!(host.buffer.has_messages_for(10, APP_C));
        assert!(!host.buffer.is_delaying_for(10, APP_C));

        // The sign-off plays the pair in order: the update applies, then
        // the replayed create finds the ghost alive and holds again.
        assert!(host.receive_ghost_set_next_real(10, APP_C, 300));
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 1);
        assert_eq!(host.ghosts.len(), 1);
        assert!(host.buffer.is_delaying_for(10, APP_C));
    }

    #[test]
    fn an_out_of_order_set_real_in_a_played_queue_re_heads_the_delay() {
        let mut host = hold_host(10, 0);

        // A whole playable subsequence from D lands while the ghost still
        // expects subsequence 1: playing it re-delays from the stale head,
        // swallowing the rest of the queue behind it.
        host.buffer.add(
            10,
            APP_D,
            BufferedGhostMessage::Common(GhostMessage::SetReal(GhostSetReal {
                entity_id: 10,
                num_times_real_offloaded: 2,
                owner: APP_D,
            })),
        );
        host.buffer.add(
            10,
            APP_D,
            BufferedGhostMessage::Common(GhostMessage::DelGhost { entity_id: 10 }),
        );
        host.play_subsequence_for(10, APP_D, 300);
        assert!(!host.buffer.has_messages_for(10, APP_D));
        assert!(host.buffer.has_queue_for(10, APP_D));
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 0);

        // The skipped update lands, and D's sign-off releases the pair:
        // the update applies, the del destroys the ghost.
        assert_eq!(
            host.receive_ghost_set_real(10, 1, APP_C),
            GhostSetRealOutcome::Relocated
        );
        assert!(host.receive_ghost_set_next_real(10, APP_D, 400));
        assert!(!host.ghosts.contains_key(&10));
        assert!(host.buffer.is_empty());
    }

    #[test]
    fn a_played_subsequence_pause_keeps_the_rest_queued() {
        let mut host = hold_host(10, 0);
        let payload = GhostCreatePayload {
            space_id: 7,
            entity_id: 10,
            entity_type_id: 4,
            num_times_real_offloaded: 1,
            position: Vector3::new(6.0, 7.0, 8.0),
            is_on_ground: false,
            last_event_number: 0,
            volatile: VolatileInfo::NONE,
            real_addr: APP_C,
            base_addr: (9, 9),
            direction: Vector3::ZERO,
            property_event_stamps: vec![],
            ghost_properties: vec![],
            ghost_controllers: vec![],
            periods_without_witness: 0,
            aoi_update_scheme_id: 0,
        };

        // A whole subsequence plus a trailing create form carried as a
        // common message (the channel shape).
        host.buffer.add(
            10,
            APP_C,
            BufferedGhostMessage::Common(GhostMessage::SetReal(GhostSetReal {
                entity_id: 10,
                num_times_real_offloaded: 1,
                owner: APP_C,
            })),
        );
        host.buffer.add(
            10,
            APP_C,
            BufferedGhostMessage::Common(GhostMessage::SetNextReal(GhostSetNextReal {
                entity_id: 10,
                next_real_addr: APP_E,
            })),
        );
        host.buffer.add(
            10,
            APP_C,
            BufferedGhostMessage::Common(GhostMessage::CreateGhost(payload.clone())),
        );

        // Playing stops at the subsequence end: the update and handover
        // apply, the rest of the queue stays for a later play.
        host.play_subsequence_for(10, APP_C, 300);
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 1);
        assert_eq!(host.ghosts[&10].real_addr, APP_C);
        assert_eq!(host.ghosts[&10].next_real_addr, Some(APP_E));
        assert!(host.buffer.has_messages_for(10, APP_C));

        // The ghost dies: the queued create starts the next lifespan and
        // runs the immediate create path.
        assert!(host.receive_del_ghost(10, 400));
        assert_eq!(host.ghosts.len(), 1);
        assert_eq!(host.ghosts[&10].position, Vector3::new(6.0, 7.0, 8.0));
        assert_eq!(host.ghosts[&10].num_times_real_offloaded, 1);
        assert_eq!(host.ghosts[&10].creation_time, 400);
        assert!(host.buffer.is_empty());
    }

    #[test]
    fn the_sign_off_skips_the_destination_haunt() {
        let (cells, mut channels, mut maintainer) = two_cell_world();
        let (mut real, view) = ghosted_real(10, 90.0);
        maintainer.check(&cells, &mut channels, &mut real, &view, 100);

        // The real already haunts C as well; offloading to C leaves that
        // haunt unsigned (its ghost learns from the new real directly).
        real.haunts.push(Haunt {
            addr: APP_C,
            creation_time: 100,
        });
        offload_sign_off(&real, APP_C, &mut channels);
        assert_eq!(
            channels[&APP_B].out[1],
            GhostMessage::SetNextReal(GhostSetNextReal {
                entity_id: 10,
                next_real_addr: APP_C,
            })
        );
        // The destination is not even touched: no channel is created for
        // it here.
        assert!(!channels.contains_key(&APP_C));
    }

    #[test]
    fn onload_skips_unsuitable_haunts_and_deletes_across_spaces() {
        let mut channels = BTreeMap::new();
        for addr in [APP_A, APP_B, APP_D, APP_E] {
            channels.insert(addr, GhostChannel::new(addr));
        }
        channels.get_mut(&APP_E).unwrap().is_good = false;
        let unknown = (6, 6);
        let mut real = RealEntity::new(10, (9, 9), Vector3::new(140.0, 0.0, 50.0), 0, 0);
        let context = OnloadHauntContext {
            bad_haunt_addr: Some(APP_D),
            ..Default::default()
        };

        // Own address (local teleport), the bad haunt, the dead channel,
        // and an app we have no channel for all pass silently; only the
        // healthy B haunt is notified.
        let haunted = onload_notify_haunts(
            &mut real,
            &[APP_A, APP_D, unknown, APP_E, APP_B],
            &mut channels,
            APP_A,
            1,
            300,
            &context,
        );
        assert!(haunted);
        assert_eq!(
            real.haunts,
            vec![Haunt {
                addr: APP_B,
                creation_time: 300
            }]
        );
        assert_eq!(
            channels[&APP_B].out,
            vec![GhostMessage::SetReal(GhostSetReal {
                entity_id: 10,
                num_times_real_offloaded: 1,
                owner: APP_A,
            })]
        );
        assert!(channels[&APP_E].out.is_empty());

        // Same list across a space change: the ghosts are deleted instead.
        let mut real = RealEntity::new(10, (9, 9), Vector3::new(140.0, 0.0, 50.0), 0, 0);
        let context = OnloadHauntContext {
            has_changed_space: true,
            ..Default::default()
        };
        let haunted = onload_notify_haunts(
            &mut real,
            &[APP_B, APP_E],
            &mut channels,
            APP_A,
            1,
            300,
            &context,
        );
        assert!(!haunted);
        assert!(real.haunts.is_empty());
        assert_eq!(
            channels[&APP_B].out[1],
            GhostMessage::DelGhost { entity_id: 10 }
        );
        // The dead channel is skipped in this pass too: a dead app gets
        // no delGhost at all.
        assert!(channels[&APP_E].out.is_empty());
    }

    #[test]
    fn the_ghost_host_keeps_exactly_one_record_per_create() {
        let mut host = GhostHost::default();
        host.register_type(
            4,
            GhostEntityTypeDesc {
                prop_count_ghost: 2,
                num_event_stamped_properties: 2,
            },
        );
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
            direction: Vector3::new(1.5, 0.25, -0.5),
            property_event_stamps: vec![7, 13],
            ghost_properties: vec![PropValue::Int(5), PropValue::Str("hi".to_string())],
            ghost_controllers: vec![GhostControllerSeed {
                id: 3,
                type_id: 9,
                user_arg: 42,
                payload: vec![1, 2, 3],
            }],
            periods_without_witness: 4,
            aoi_update_scheme_id: 3,
        };

        assert_eq!(
            host.receive_create_ghost(APP_A, payload.clone(), 42),
            GhostCreateOutcome::Created
        );
        assert_eq!(host.ghosts.len(), 1);
        let record = host.ghosts.get(&10).unwrap();
        assert_eq!(record.entity_type_id, 4);
        assert_eq!(record.num_times_real_offloaded, 2);
        assert_eq!(record.last_event_number, 5);
        assert_eq!(record.real_addr, APP_A);
        assert_eq!(record.creation_time, 42);
        // The snapshot matches the real that streamed it, head to tail.
        assert_eq!(record.direction, Vector3::new(1.5, 0.25, -0.5));
        assert_eq!(record.property_event_stamps, vec![7, 13]);
        assert_eq!(
            record.ghost_properties,
            vec![PropValue::Int(5), PropValue::Str("hi".to_string())]
        );
        assert_eq!(record.periods_without_witness, 4);
        assert_eq!(record.aoi_update_scheme_id, 3);
        // The ghost-domain controllers were created and started.
        assert_eq!(record.controllers.len(), 1);
        assert_eq!(record.controllers.ids(), vec![3]);
        let controller = record.controllers.controller(3).unwrap();
        assert_eq!(controller.type_id, 9);
        assert_eq!(controller.user_arg, 42);
        assert_eq!(controller.domain, crate::controllers::DOMAIN_GHOST);
        assert_eq!(controller.ghost_state, vec![1, 2, 3]);
        assert!(controller.attached);

        // A second create for a living entity takes the buffered path — no
        // duplicate record, no count drift.
        assert_eq!(
            host.receive_create_ghost(APP_A, payload, 43),
            GhostCreateOutcome::Buffered
        );
        assert_eq!(host.ghosts.len(), 1);

        assert!(host.receive_del_ghost(10, 200));
        // The death offered the buffered queue a new lifespan: the held
        // create replayed (with the replay time) instead of leaking.
        assert_eq!(host.ghosts.len(), 1);
        assert_eq!(host.ghosts[&10].creation_time, 200);
        assert!(host.receive_del_ghost(10, 200));
        assert_eq!(host.ghosts.len(), 0);
        assert!(!host.receive_del_ghost(10, 200));
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
