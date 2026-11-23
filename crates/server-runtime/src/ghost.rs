//! BW-08-04-01: ghost creation and removal.
//!
//! Ported from `server/cellapp/entity_ghost_maintainer.{hpp,cpp}`: `check`
//! (`.cpp:56-75`), `checkEntityForOffload` (`.cpp:91-123`), `markHaunts`
//! (`.cpp:135-154`), `createOrUnmarkRequiredHaunts` (`.cpp:169-188`),
//! `visit` (`.cpp:194-241`) and `deleteMarkedHaunts`
//! (`.cpp:258-309`), plus the deletion cap in
//! `server/cellapp/offload_checker.cpp:123-127`.
//!
//! The reference threads the work through live `Entity` / `RealEntity` /
//! `CellInfo` / `CellAppChannel` objects and a space tree; here the entity's
//! haunting set is [`RealGhosts`] and the space is a flat list of
//! [`GhostCell`]. `SpaceTree::visitRect` prunes by internal node bounds, so
//! the set of cells it reaches is exactly those whose rect intersects the
//! interest rect - which is what [`GhostMaintainer::check`] scans for.
//! `BW::Rect`'s XZ ranges become [`GhostRect`]
//! (`programming/bigworld/lib/math/range1dt.ipp:129-131` keeps the
//! touching-edge case intersecting).

/// `GHOST_FUDGE` (`entity_ghost_maintainer.cpp:172`).
pub const GHOST_FUDGE: f32 = 20.0;

/// `CellAppConfig::ghostDistance()` default (`cellapp_config.cpp:29`).
pub const DEFAULT_GHOST_DISTANCE: f32 = 500.0;

/// `NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS` (`entity_ghost_maintainer.cpp:260`).
pub const NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS: f32 = 2.0;

/// An XZ range pair, `BW::Rect`'s member ranges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GhostRect {
    pub min_x: f32,
    pub max_x: f32,
    pub min_z: f32,
    pub max_z: f32,
}

impl GhostRect {
    /// `Rect(x, y, x, y)` - a point rect at `position`.
    pub fn point(x: f32, z: f32) -> Self {
        GhostRect {
            min_x: x,
            max_x: x,
            min_z: z,
            max_z: z,
        }
    }

    /// `Range1DT::inflateBy`: both ends grow, clamped at the coordinate
    /// limits (`range1dt.ipp:108-117`).
    pub fn inflate_by(&mut self, value: f32) {
        if self.min_x > f32::MIN {
            self.min_x -= value;
        }
        if self.max_x < f32::MAX {
            self.max_x += value;
        }
        if self.min_z > f32::MIN {
            self.min_z -= value;
        }
        if self.max_z < f32::MAX {
            self.max_z += value;
        }
    }

    /// `RectT::intersects` (`rectt.ipp:89-92`).
    pub fn intersects(&self, other: &GhostRect) -> bool {
        other.min_x <= self.max_x
            && self.min_x <= other.max_x
            && other.min_z <= self.max_z
            && self.min_z <= other.max_z
    }
}

/// One link to a neighbouring cell: its channel state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GhostChannel {
    /// Whether `mark()` is set (`CellAppChannel::mark`).
    pub marked: bool,
    /// `isDeletePending()` on the remote `CellInfo`.
    pub delete_pending: bool,
    /// `isGood()` on the channel.
    pub good: bool,
}

impl GhostChannel {
    pub const fn new() -> Self {
        GhostChannel {
            marked: false,
            delete_pending: false,
            good: true,
        }
    }
}

impl Default for GhostChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// A `CellInfo` in the space: a rect, an address and its channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GhostCell {
    pub cell_id: u32,
    /// `Mercury::Address`, modelled as an opaque value.
    pub address: u64,
    pub rect: GhostRect,
    pub channel: GhostChannel,
}

/// One haunt (`RealEntity::Haunt`): which cell holds the ghost and when it
/// was created.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Haunt {
    pub cell_id: u32,
    pub creation_time: u64,
}

/// The messages a check iteration would send, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhostMessage {
    /// `Entity::createGhost`.
    Create { cell_id: u32 },
    /// `RealEntity::addDelGhostMessage`.
    Delete { cell_id: u32 },
}

/// `RealEntity`'s haunting set plus what a check iteration sends.
#[derive(Debug, Default)]
pub struct RealGhosts {
    pub entity_id: u32,
    /// `Entity::position()` x.
    pub x: f32,
    /// `Entity::position()` z.
    pub z: f32,
    pub haunts: Vec<Haunt>,
    pub messages: Vec<GhostMessage>,
    /// `RealEntity::creationTime`.
    pub creation_time: u64,
    /// `pType()->description().appealRadius()`.
    pub appeal_radius: f32,
}

impl RealGhosts {
    /// `pReal()->addHaunt(channel)`.
    pub fn add_haunt(&mut self, cell_id: u32, creation_time: u64) {
        self.haunts.push(Haunt {
            cell_id,
            creation_time,
        });
    }

    /// `pReal()->delHaunt`.
    pub fn del_haunt(&mut self, cell_id: u32) {
        if let Some(pos) = self.haunts.iter().position(|h| h.cell_id == cell_id) {
            self.haunts.remove(pos);
        }
    }
}

/// `OffloadChecker`'s half that matters here: the per-iteration deletion cap
/// (`offload_checker.cpp:123-127`) and the offload queue.
#[derive(Debug, Default)]
pub struct OffloadChecker {
    /// `CellAppConfig::maxGhostsToDelete()` (default 100).
    pub max_ghosts_to_delete: u32,
    /// `numGhostsDeleted_`.
    pub num_ghosts_deleted: u32,
    /// `OffloadList`: entity id, destination cell.
    pub offloads: Vec<(u32, u32)>,
}

impl OffloadChecker {
    /// `canDeleteMoreGhosts`: note the `<=`, not `<`.
    pub fn can_delete_more_ghosts(&self) -> bool {
        self.num_ghosts_deleted <= self.max_ghosts_to_delete
    }

    /// `addDeletedGhost`.
    pub fn add_deleted_ghost(&mut self) {
        self.num_ghosts_deleted += 1;
    }

    /// `addToOffloads`.
    pub fn add_to_offloads(&mut self, entity_id: u32, destination_cell: u32) {
        self.offloads.push((entity_id, destination_cell));
    }
}

/// The cell-app configuration the reference reads for this work.
#[derive(Debug, Clone, Copy)]
pub struct GhostMaintainerConfig {
    /// `CellAppConfig::ghostDistance()` (default 500).
    pub ghost_distance: f32,
    /// `CellAppConfig::updateHertz()`.
    pub update_hertz: u64,
    /// `CellAppConfig::minGhostLifespanInTicks()` - seconds to ticks.
    pub min_ghost_lifespan_ticks: u64,
    /// `CellAppConfig::maxGhostsToDelete()` (default 100).
    pub max_ghosts_to_delete: u32,
    /// `CellApp::instance().interface().address()`.
    pub own_address: u64,
}

impl Default for GhostMaintainerConfig {
    fn default() -> Self {
        GhostMaintainerConfig {
            ghost_distance: DEFAULT_GHOST_DISTANCE,
            update_hertz: 50,
            min_ghost_lifespan_ticks: 250,
            max_ghosts_to_delete: 100,
            own_address: 0,
        }
    }
}

/// `EntityGhostMaintainer`.
pub struct GhostMaintainer {
    pub cfg: GhostMaintainerConfig,
    /// `NEW_REAL_KEEP_GHOST_PERIOD` in ticks.
    pub new_real_keep_ghost_period: u64,
    /// `hysteresisArea_`.
    pub hysteresis_area: GhostRect,
    /// `pOffloadDestination_`.
    pub offload_destination: Option<u32>,
    /// `numGhostsCreated_`.
    pub num_ghosts_created: u32,
    /// The real entity's home cell, `pEntity_->cell().cellInfo()`.
    pub home_cell: u32,
    /// `Cell::shouldOffload()`.
    pub should_offload: bool,
    /// `CellApp::instance().time()`.
    pub current_time: u64,
    /// The `OffloadChecker` this iteration runs against.
    pub checker: OffloadChecker,
}

impl GhostMaintainer {
    pub fn new(cfg: GhostMaintainerConfig, home_cell: u32) -> Self {
        GhostMaintainer {
            checker: OffloadChecker {
                max_ghosts_to_delete: cfg.max_ghosts_to_delete,
                ..OffloadChecker::default()
            },
            current_time: 0,
            new_real_keep_ghost_period: (NEW_REAL_KEEP_GHOST_PERIOD_IN_SECONDS
                * cfg.update_hertz as f32) as u64,
            hysteresis_area: GhostRect::point(0.0, 0.0),
            offload_destination: None,
            num_ghosts_created: 0,
            cfg,
            home_cell,
            should_offload: false,
        }
    }

    /// `check()` (`entity_ghost_maintainer.cpp:56-75`).
    ///
    /// Returns `numGhostsCreated_` for the assertion the reference makes
    /// (`MF_ASSERT` at `.cpp:70-72`), which this port checks itself.
    pub fn check(&mut self, ghosts: &mut RealGhosts, cells: &mut [GhostCell]) -> u32 {
        // The reference builds a fresh maintainer per entity per run, so
        // its counters start at zero every iteration.
        self.num_ghosts_created = 0;
        if self.should_offload {
            self.check_entity_for_offload(ghosts, cells);
        }

        // We mark all the haunts for this entity, and then we unmark all
        // the valid ones. The invalid ghosts are left marked and are
        // deleted.
        let does_offload_destination_have_ghost = self.mark_haunts(ghosts, cells);

        self.create_or_unmark_required_haunts(ghosts, cells);

        debug_assert!(
            self.offload_destination.is_none()
                || does_offload_destination_have_ghost
                || self.num_ghosts_created == 1,
            "an offload destination without a ghost and no ghost created"
        );

        self.delete_marked_haunts(ghosts, cells);
        self.num_ghosts_created
    }

    /// `checkEntityForOffload` (`.cpp:91-123`).
    fn check_entity_for_offload(&mut self, ghosts: &RealGhosts, cells: &mut [GhostCell]) {
        // Find out where we really want to live.
        let home = cells
            .iter()
            .find(|c| c.rect.intersects(&GhostRect::point(ghosts.x, ghosts.z)));
        let Some(home) = home else {
            return;
        };
        if home.cell_id == self.home_cell {
            // Don't offload to ourselves.
            return;
        }
        if home.channel.delete_pending {
            // Don't offload to a cell that is being deleted.
            return;
        }
        if !home.channel.good {
            // Don't offload if other cell has failed or died.
            return;
        }

        // OK to offload now.
        self.offload_destination = Some(home.cell_id);
        self.checker.add_to_offloads(ghosts.entity_id, home.cell_id);
    }

    /// `markHaunts` (`.cpp:135-154`).
    fn mark_haunts(&mut self, ghosts: &RealGhosts, cells: &mut [GhostCell]) -> bool {
        let mut does_offload_destination_have_ghost = false;
        for haunt in ghosts.haunts.iter() {
            let Some(cell) = cells.iter_mut().find(|c| c.cell_id == haunt.cell_id) else {
                continue;
            };
            if Some(cell.cell_id) == self.offload_destination {
                does_offload_destination_have_ghost = true;
            }
            cell.channel.marked = true;
        }
        does_offload_destination_have_ghost
    }

    /// `createOrUnmarkRequiredHaunts` + `visit` (`.cpp:169-188`, `.cpp:194-241`).
    fn create_or_unmark_required_haunts(
        &mut self,
        ghosts: &mut RealGhosts,
        cells: &mut [GhostCell],
    ) {
        // Find all the haunts that we should have.
        let mut interest_area = GhostRect::point(ghosts.x, ghosts.z);

        // Entities with an appeal radius have to ghost more.
        interest_area.inflate_by(self.cfg.ghost_distance + ghosts.appeal_radius);

        self.hysteresis_area = interest_area;

        interest_area.inflate_by(GHOST_FUDGE);

        // `SpaceTree::visitRect`: every cell whose rect intersects.
        let targets: Vec<GhostCell> = cells
            .iter()
            .filter(|c| c.rect.intersects(&interest_area))
            .copied()
            .collect();
        for cell in targets {
            self.visit(&cell, ghosts, cells);
        }
    }

    /// `visit` (`.cpp:194-241`).
    fn visit(&mut self, target: &GhostCell, ghosts: &mut RealGhosts, cells: &mut [GhostCell]) {
        // discard it if it is ourself
        if target.address == self.cfg.own_address {
            return;
        }

        if target.channel.delete_pending {
            // Do not have ghosts on cells that are about to be deleted.
            return;
        }

        // If it has been marked as an existing haunt then unmark it and bail.
        let marked = target.channel.marked;
        if marked {
            for cell in cells.iter_mut() {
                if cell.cell_id == target.cell_id {
                    cell.channel.marked = false;
                }
            }
            return;
        }

        // Do not create a ghost if we are about to be offloaded. Let the
        // destination do this.
        if self
            .offload_destination
            .is_some_and(|destination| destination != target.cell_id)
        {
            return;
        }

        // and if we are not far enough in then toss it too (hysteresis check)
        if !target.rect.intersects(&self.hysteresis_area) {
            return;
        }

        // Otherwise we should create a new ghost.
        ghosts.add_haunt(target.cell_id, self.current_time);
        ghosts.messages.push(GhostMessage::Create {
            cell_id: target.cell_id,
        });

        self.num_ghosts_created += 1;
    }

    /// `deleteMarkedHaunts` (`.cpp:258-309`).
    fn delete_marked_haunts(&mut self, ghosts: &mut RealGhosts, cells: &mut [GhostCell]) {
        for haunt in ghosts.haunts.clone() {
            let cell_id = haunt.cell_id;
            let good = cells
                .iter()
                .find(|c| c.cell_id == cell_id)
                .map(|c| c.channel.good)
                .unwrap_or(false);

            // Keep the ghost if we're offloading there, or the real entity
            // or the ghost is too new.
            let should_del_ghost = self.checker.can_delete_more_ghosts()
                && self.offload_destination != Some(cell_id)
                && self.current_time.saturating_sub(ghosts.creation_time)
                    > self.new_real_keep_ghost_period
                && self.current_time.saturating_sub(haunt.creation_time)
                    > self.cfg.min_ghost_lifespan_ticks;

            let marked = cells
                .iter()
                .find(|c| c.cell_id == cell_id)
                .map(|c| c.channel.marked)
                .unwrap_or(false);

            if marked && should_del_ghost {
                // only bother telling it if it hasn't failed
                if good {
                    ghosts.messages.push(GhostMessage::Delete { cell_id });
                    self.checker.add_deleted_ghost();
                }
                ghosts.del_haunt(cell_id);
            }

            // always clear the mark for the next user
            if let Some(cell) = cells.iter_mut().find(|c| c.cell_id == cell_id) {
                cell.channel.marked = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(max_deletions: u32) -> GhostMaintainerConfig {
        GhostMaintainerConfig {
            max_ghosts_to_delete: max_deletions,
            // The CellApp hosting cell 1: its own cell is skipped by address.
            own_address: 1,
            ..GhostMaintainerConfig::default()
        }
    }

    /// Two adjacent cells of half-width 250 either side of the origin:
    /// the left one is the real entity's own cell, the right one the
    /// neighbour.
    fn two_cells() -> Vec<GhostCell> {
        vec![
            GhostCell {
                cell_id: 1,
                address: 1,
                rect: GhostRect {
                    min_x: -250.0,
                    max_x: 250.0,
                    min_z: -250.0,
                    max_z: 250.0,
                },
                channel: GhostChannel::new(),
            },
            GhostCell {
                cell_id: 2,
                address: 2,
                rect: GhostRect {
                    min_x: 250.0,
                    max_x: 500.0,
                    min_z: -250.0,
                    max_z: 250.0,
                },
                channel: GhostChannel::new(),
            },
        ]
    }

    fn ghosts_at(x: f32, z: f32) -> RealGhosts {
        let mut ghosts = RealGhosts {
            entity_id: 9,
            ..RealGhosts::default()
        };
        ghosts.x = x;
        ghosts.z = z;
        ghosts
    }

    #[test]
    fn channel_default_matches_new() {
        assert_eq!(GhostChannel::default(), GhostChannel::new());
    }

    #[test]
    fn inflate_by_respects_the_coordinate_limits() {
        let mut r = GhostRect {
            min_x: f32::MIN,
            max_x: f32::MAX,
            min_z: f32::MIN,
            max_z: f32::MAX,
        };
        r.inflate_by(100.0);
        assert_eq!(r.min_x, f32::MIN);
        assert_eq!(r.max_x, f32::MAX);
        assert_eq!(r.min_z, f32::MIN);
        assert_eq!(r.max_z, f32::MAX);
    }

    #[test]
    fn del_haunt_of_an_unknown_cell_is_a_noop() {
        let mut ghosts = ghosts_at(0.0, 0.0);
        ghosts.add_haunt(2, 0);
        ghosts.del_haunt(99);
        assert_eq!(ghosts.haunts.len(), 1);
    }

    #[test]
    fn offload_checks_refuse_our_own_cell() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.should_offload = true;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(m.offload_destination, None, "don't offload to ourselves");
    }

    #[test]
    fn offload_checks_refuse_no_home_cell() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(10_000.0, 10_000.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.should_offload = true;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(m.offload_destination, None);
    }

    #[test]
    fn a_haunt_on_a_missing_cell_is_left_alone() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        ghosts.add_haunt(2, 0);
        ghosts.add_haunt(99, 0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);
        assert_eq!(
            ghosts.haunts.len(),
            2,
            "a cell that vanished mid-run keeps its haunt"
        );
        assert!(!cells.iter().any(|c| c.channel.marked));
    }

    #[test]
    fn deleting_through_a_dead_channel_sends_no_message() {
        let mut cells = two_cells();
        cells[1].channel.good = false;
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);
        assert_eq!(ghosts.haunts.len(), 1);

        // Walk out of range: the stale haunt is dropped locally but the
        // dead channel gets no message.
        ghosts.x = -10_000.0;
        ghosts.z = -10_000.0;
        ghosts.creation_time = 0;
        ghosts.haunts[0].creation_time = 0;
        m.current_time = 1000;
        m.check(&mut ghosts, &mut cells);
        assert!(ghosts.haunts.is_empty());
        assert!(!ghosts
            .messages
            .contains(&GhostMessage::Delete { cell_id: 2 }));
        assert_eq!(m.checker.num_ghosts_deleted, 0);
    }

    #[test]
    fn the_offload_destination_is_the_only_cell_created_on() {
        let mut cells = two_cells();
        cells.push(GhostCell {
            cell_id: 3,
            address: 3,
            rect: GhostRect {
                min_x: 500.0,
                max_x: 750.0,
                min_z: -250.0,
                max_z: 250.0,
            },
            channel: GhostChannel::new(),
        });
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.offload_destination = Some(3);
        let created = m.check(&mut ghosts, &mut cells);
        assert_eq!(created, 1, "only the destination gets a new ghost");
        assert_eq!(ghosts.haunts.len(), 1);
        assert_eq!(ghosts.haunts[0].cell_id, 3);
    }

    #[test]
    fn the_hysteresis_band_rejects_cells_only_inside_the_fudge() {
        let mut cells = two_cells();
        // Cell 3 reaches into the GHOST_FUDGE band past the interest area
        // but not into the interest area itself.
        cells.push(GhostCell {
            cell_id: 3,
            address: 3,
            rect: GhostRect {
                min_x: 515.0,
                max_x: 750.0,
                min_z: -250.0,
                max_z: 250.0,
            },
            channel: GhostChannel::new(),
        });
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        let created = m.check(&mut ghosts, &mut cells);
        assert_eq!(created, 1, "only the fully-overlapping cell is haunted");
        assert_eq!(ghosts.haunts[0].cell_id, 2);
    }

    #[test]
    fn rect_intersection_keeps_touching_edges() {
        let mut a = GhostRect::point(0.0, 0.0);
        a.inflate_by(100.0);
        let mut b = GhostRect::point(100.0, 0.0);
        b.inflate_by(100.0);
        assert!(a.intersects(&b), "edge to edge still intersects");

        let c = GhostRect::point(1000.0, 0.0);
        assert!(!a.intersects(&c));
    }

    #[test]
    fn a_real_entity_ghosts_into_both_adjacent_cells() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        let created = m.check(&mut ghosts, &mut cells);
        assert_eq!(created, 1, "only the neighbour cell needs a ghost");
        assert_eq!(ghosts.haunts.len(), 1);
        assert_eq!(ghosts.haunts[0].cell_id, 2);
        assert_eq!(ghosts.messages, vec![GhostMessage::Create { cell_id: 2 }]);
    }

    #[test]
    fn hysteresis_keeps_a_ghost_while_the_cell_still_overlaps() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);

        // Move away: still inside the inflated interest area, so the
        // existing ghost survives (its channel gets unmarked).
        let mut further = ghosts;
        further.x = 400.0;
        let created = m.check(&mut further, &mut cells);
        assert_eq!(created, 0);
        assert_eq!(further.haunts.len(), 1);
        assert!(!cells.iter().any(|c| c.channel.marked));
    }

    #[test]
    fn offloading_does_not_create_ghosts_elsewhere() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.offload_destination = Some(2);
        m.check(&mut ghosts, &mut cells);
        // The destination cell still gets its ghost, and no others.
        assert_eq!(ghosts.haunts.len(), 1);
        assert_eq!(ghosts.haunts[0].cell_id, 2);
    }

    #[test]
    fn delete_pending_cell_never_receives_a_ghost() {
        let mut cells = two_cells();
        cells[1].channel.delete_pending = true;
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        let created = m.check(&mut ghosts, &mut cells);
        assert_eq!(created, 0);
        assert!(ghosts.haunts.is_empty());
    }

    #[test]
    fn a_new_real_entity_keeps_its_ghosts() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);

        // Teleport out of range before the keep period is up.
        ghosts.x = -10_000.0;
        ghosts.z = -10_000.0;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(ghosts.haunts.len(), 1, "the new real keeps its ghost");
    }

    #[test]
    fn a_new_ghost_outlives_the_minimum_lifespan() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);

        // Move out of range with the haunt created at the same tick.
        ghosts.x = -10_000.0;
        ghosts.z = -10_000.0;
        ghosts.haunts[0].creation_time = m.current_time;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(ghosts.haunts.len(), 1, "a brand new ghost is kept");
    }

    #[test]
    fn the_deletion_cap_limits_ghost_removals() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(0), 1);
        m.check(&mut ghosts, &mut cells);
        // With no deletions allowed the stale haunt stays marked but lives.
        ghosts.x = -10_000.0;
        ghosts.z = -10_000.0;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(ghosts.haunts.len(), 1);
        assert!(m.checker.num_ghosts_deleted == 0);
    }

    #[test]
    fn marks_are_cleared_for_the_next_user() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);
        assert!(
            !cells.iter().any(|c| c.channel.marked),
            "marks never leak into the next iteration"
        );
    }

    #[test]
    fn an_entity_standing_in_the_neighbour_offloads_there() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(300.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.should_offload = true;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(m.offload_destination, Some(2));
        assert_eq!(m.checker.offloads, vec![(9, 2)]);
    }

    #[test]
    fn offload_to_a_delete_pending_cell_is_refused() {
        let mut cells = two_cells();
        cells[1].channel.delete_pending = true;
        let mut ghosts = ghosts_at(300.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.should_offload = true;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(m.offload_destination, None);
    }

    #[test]
    fn offload_to_a_dead_cell_is_refused() {
        let mut cells = two_cells();
        cells[1].channel.good = false;
        let mut ghosts = ghosts_at(300.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.should_offload = true;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(m.offload_destination, None);
    }

    #[test]
    fn the_own_cell_is_skipped_by_address() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);
        // Cell 1 carries the app's own address, so only the neighbour is
        // haunted.
        assert_eq!(ghosts.haunts.len(), 1);
        assert_eq!(ghosts.haunts[0].cell_id, 2);
    }

    #[test]
    fn appeal_radius_reaches_further_cells() {
        let mut cells = two_cells();
        // A cell a cell-width to the right, beyond the ghost distance.
        cells.push(GhostCell {
            cell_id: 3,
            address: 3,
            rect: GhostRect {
                min_x: 500.0,
                max_x: 750.0,
                min_z: -250.0,
                max_z: 250.0,
            },
            channel: GhostChannel::new(),
        });
        let mut ghosts = ghosts_at(0.0, 0.0);
        ghosts.appeal_radius = 100.0;
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);
        assert_eq!(ghosts.haunts.len(), 2, "the appeal radius ghosts further");
        assert_eq!(ghosts.haunts[0].cell_id, 2);
        assert_eq!(ghosts.haunts[1].cell_id, 3);
    }

    #[test]
    fn crossing_the_boundary_swaps_the_ghost_side() {
        let mut cells = two_cells();
        let mut ghosts = ghosts_at(0.0, 0.0);
        let mut m = GhostMaintainer::new(config(100), 1);
        m.check(&mut ghosts, &mut cells);
        assert_eq!(
            ghosts.haunts,
            vec![Haunt {
                cell_id: 2,
                creation_time: 0
            }]
        );

        // The real walks into the neighbour and is offloaded there: its
        // own haunt on the destination is kept and nothing else changes.
        ghosts.x = 300.0;
        ghosts.creation_time = 0;
        ghosts.haunts[0].creation_time = 0;
        m.current_time = 1000;
        m.should_offload = true;
        m.check(&mut ghosts, &mut cells);
        assert_eq!(m.offload_destination, Some(2));
        assert_eq!(ghosts.haunts.len(), 1, "the destination keeps its haunt");

        // From the destination app's point of view: cell 2 is now home,
        // so the ghost flips sides - cell 1 gains one, cell 2 drops its
        // stale one.
        let mut m2 = GhostMaintainer::new(config(100), 2);
        m2.cfg.own_address = 2;
        m2.current_time = 1000;
        m2.check(&mut ghosts, &mut cells);

        assert_eq!(ghosts.haunts.len(), 1);
        assert_eq!(ghosts.haunts[0].cell_id, 1, "cell 1 is haunted instead");
        assert!(
            ghosts
                .messages
                .contains(&GhostMessage::Create { cell_id: 1 }),
            "the old cell gains its ghost"
        );
        assert!(
            ghosts
                .messages
                .contains(&GhostMessage::Delete { cell_id: 2 }),
            "the stale ghost on the new home cell is dropped"
        );
    }
}
