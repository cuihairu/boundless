//! BW-08-04-01: creating and destroying Ghosts.
//!
//! An entity keeps a Real on its own cell and a Ghost on every neighbouring
//! cell whose area it may reach. [`GhostMaintainer`] decides which neighbours
//! need one: a cell gets a Ghost when its rectangle intersects the entity's
//! interest area — the entity position inflated by `ghostDistance + appealRadius`
//! (`entity_ghost_maintainer.cpp:180`) — and keeps it while the rectangle
//! still intersects the visit area, which is the interest area inflated by
//! `GHOST_FUDGE` (`:184`). Because the visit area is strictly larger than the
//! interest area, a Ghost that is created at the far edge of the interest band
//! survives a small move back, so the pair of cells never flaps the Ghost on
//! and off at the boundary.
//!
//! The acceptance, "两相邻 Cell 保持正确数量", is that as the entity moves
//! across the boundary between two adjacent cells and back, each neighbour's
//! Ghost count stays exactly right — one while the entity is in range, none
//! when it is out of range — and repeated ticks at one position never churn
//! the count.
//!
//! Reference:
//! - `entity_ghost_maintainer.cpp:176` `GHOST_FUDGE` (20.f).
//! - `entity_ghost_maintainer.cpp:180-184` the interest and visit areas.
//! - `entity_ghost_maintainer.cpp:196-214` `visit`: skip self and deleted
//!   cells, keep an existing haunt, create a new one when the cell intersects
//!   the hysteresis area.
//! - `entity_ghost_maintainer.cpp:216` `addHaunt` + `createGhost`.
//! - `entity_ghost_maintainer.cpp:262` `deleteMarkedHaunts`: a haunt that is
//!   no longer required is deleted (with keep-periods for new ghosts and
//!   recent reals, `:274-280`, which matter at offload time).
//! - `cellapp_config.cpp:29` `ghostDistance` default = `DEFAULT_AOI_RADIUS`.

use crate::range_list::Position;

/// A cell's rectangular area (`CellInfo::rect()`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellRect {
    /// The lower X edge.
    pub x_min: f64,
    /// The lower Z edge.
    pub z_min: f64,
    /// The upper X edge.
    pub x_max: f64,
    /// The upper Z edge.
    pub z_max: f64,
}

impl CellRect {
    /// A rectangle from its corners.
    pub fn new(x_min: f64, z_min: f64, x_max: f64, z_max: f64) -> Self {
        CellRect {
            x_min,
            z_min,
            x_max,
            z_max,
        }
    }

    /// Whether the two rectangles overlap, edges touching included
    /// (`CellRect::intersects`, `entity_ghost_maintainer.cpp:206`).
    pub fn intersects(self, other: CellRect) -> bool {
        self.x_min <= other.x_max
            && other.x_min <= self.x_max
            && self.z_min <= other.z_max
            && other.z_min <= self.z_max
    }

    /// `inflateBy(amount)` (`entity_ghost_maintainer.cpp:184`): grow the
    /// rectangle by `amount` on every side.
    pub fn inflate_by(self, amount: f64) -> CellRect {
        CellRect {
            x_min: self.x_min - amount,
            z_min: self.z_min - amount,
            x_max: self.x_max + amount,
            z_max: self.z_max + amount,
        }
    }
}

/// One adjacent cell and whether it currently holds a Ghost of the tracked
/// entity (`CellAppChannel` haunt state).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// The cell's identity (`CellInfo::addr()`).
    pub id: u32,
    /// The cell's rectangle.
    pub rect: CellRect,
    /// `addHaunt`/`delHaunt`: whether the cell holds the Ghost.
    pub has_ghost: bool,
}

impl Cell {
    /// A cell with no Ghost yet.
    pub fn new(id: u32, rect: CellRect) -> Self {
        Cell {
            id,
            rect,
            has_ghost: false,
        }
    }
}

/// `DEFAULT_AOI_RADIUS` (`cellapp_config.cpp:21`), the default
/// `ghostDistance`.
pub const DEFAULT_GHOST_DISTANCE: f64 = 500.0;

/// `GHOST_FUDGE` (`entity_ghost_maintainer.cpp:176`): the hysteresis band
/// that keeps a Ghost alive slightly beyond the interest area.
pub const GHOST_FUDGE: f64 = 20.0;

/// What one maintenance pass did (`numGhostsCreated_` and the deletions).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GhostDelta {
    /// The cells that gained a Ghost this pass.
    pub created: Vec<u32>,
    /// The cells that lost their Ghost this pass.
    pub deleted: Vec<u32>,
}

/// `EntityGhostMaintainer` (`entity_ghost_maintainer.cpp`): decides which
/// neighbouring cells must hold a Ghost of one entity, and keeps the decision
/// stable across the boundary through hysteresis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GhostMaintainer {
    /// `CellAppConfig::ghostDistance()`.
    pub ghost_distance: f64,
    /// The entity type's `appealRadius` (`entity_ghost_maintainer.cpp:181`).
    pub appeal_radius: f64,
    /// `GHOST_FUDGE` (`:176`).
    pub ghost_fudge: f64,
    /// `ownAddress_` (`:197`): the cell the entity's Real lives on, never
    /// given a Ghost.
    own_cell: u32,
}

impl GhostMaintainer {
    /// A maintainer for an entity whose Real is on `own_cell`, with the
    /// default fudge and no appeal radius.
    pub fn new(ghost_distance: f64, own_cell: u32) -> Self {
        GhostMaintainer {
            ghost_distance,
            appeal_radius: 0.0,
            ghost_fudge: GHOST_FUDGE,
            own_cell,
        }
    }

    /// The cell the Real lives on.
    pub fn own_cell(&self) -> u32 {
        self.own_cell
    }

    /// Re-points the maintainer at a new home cell (after an offload).
    pub fn set_own_cell(&mut self, cell: u32) {
        self.own_cell = cell;
    }

    /// The interest area (`entity_ghost_maintainer.cpp:180`): the entity
    /// position inflated by `ghostDistance + appealRadius`.
    pub fn interest_area(&self, pos: Position) -> CellRect {
        let reach = self.ghost_distance + self.appeal_radius;
        CellRect::new(pos.x - reach, pos.z - reach, pos.x + reach, pos.z + reach)
    }

    /// The visit area (`entity_ghost_maintainer.cpp:184`): the interest area
    /// further inflated by `GHOST_FUDGE`, the band a Ghost must survive in.
    pub fn visit_area(&self, pos: Position) -> CellRect {
        self.interest_area(pos).inflate_by(self.ghost_fudge)
    }

    /// One maintenance pass: create a Ghost where the interest area reaches,
    /// keep one where the visit area still reaches, and delete one where
    /// neither does (`createOrUnmarkRequiredHaunts` + `deleteMarkedHaunts`).
    ///
    /// Because the visit area is the interest area grown by the fudge, a
    /// Ghost created at the far edge of the interest band is not deleted by a
    /// small move back — the pair of cells never flaps.
    pub fn tick(&mut self, pos: Position, cells: &mut [Cell]) -> GhostDelta {
        let interest = self.interest_area(pos);
        let visit = self.visit_area(pos);
        let mut delta = GhostDelta::default();

        for cell in cells.iter_mut() {
            // The Real's own cell is never a haunt
            // (`entity_ghost_maintainer.cpp:201`).
            if cell.id == self.own_cell {
                continue;
            }
            if !cell.has_ghost {
                if interest.intersects(cell.rect) {
                    cell.has_ghost = true;
                    delta.created.push(cell.id);
                }
            } else if !visit.intersects(cell.rect) {
                // Marked as no longer required, then deleted
                // (`deleteMarkedHaunts`).
                cell.has_ghost = false;
                delta.deleted.push(cell.id);
            }
        }

        delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two adjacent cells sharing the X = 100 boundary, both 100 deep in Z.
    fn two_cells() -> Vec<Cell> {
        vec![
            Cell::new(1, CellRect::new(0.0, 0.0, 100.0, 100.0)),
            Cell::new(2, CellRect::new(100.0, 0.0, 200.0, 100.0)),
        ]
    }

    #[test]
    fn the_visit_area_is_the_interest_area_plus_the_fudge() {
        // entity_ghost_maintainer.cpp:180-184.
        let maintainer = GhostMaintainer::new(50.0, 1);
        let pos = Position::new(55.0, 50.0);

        let interest = maintainer.interest_area(pos);
        assert_eq!(
            interest,
            CellRect::new(5.0, 0.0, 105.0, 100.0),
            "interest reaches ghostDistance + appealRadius"
        );

        let visit = maintainer.visit_area(pos);
        assert_eq!(
            visit,
            CellRect::new(-15.0, -20.0, 125.0, 120.0),
            "visit grows the interest area by the fudge"
        );
        assert!(interest.intersects(CellRect::new(100.0, 0.0, 200.0, 100.0)));
        assert!(!CellRect::new(0.0, 0.0, 4.0, 100.0).intersects(interest));
    }

    #[test]
    fn two_adjacent_cells_keep_the_correct_ghost_counts() {
        // The acceptance: crossing the boundary and coming back keeps each
        // neighbour's count exactly right — one in range, none out of range.
        let mut maintainer = GhostMaintainer::new(50.0, 1);
        let mut cells = two_cells();

        // Deep in cell 1: cell 2 is out of range.
        let delta = maintainer.tick(Position::new(0.0, 50.0), &mut cells);
        assert!(delta.created.is_empty() && delta.deleted.is_empty());
        assert!(!cells[1].has_ghost, "no ghost on the far cell");

        // Within ghostDistance of the boundary: cell 2 gains a ghost.
        let delta = maintainer.tick(Position::new(55.0, 50.0), &mut cells);
        assert_eq!(delta.created, vec![2]);
        assert!(cells[1].has_ghost, "one ghost on cell 2");

        // Across the boundary: the ghost stays (cell 2 still intersects).
        let delta = maintainer.tick(Position::new(105.0, 50.0), &mut cells);
        assert!(delta.created.is_empty() && delta.deleted.is_empty());
        assert!(cells[1].has_ghost);

        // Back inside cell 1, past the hysteresis band: cell 2 loses it.
        let delta = maintainer.tick(Position::new(20.0, 50.0), &mut cells);
        assert_eq!(delta.deleted, vec![2]);
        assert!(!cells[1].has_ghost, "no ghost once out of range");
        assert!(!cells[0].has_ghost, "the real's own cell never ghosts");
    }

    #[test]
    fn a_ghost_created_at_the_edge_survives_a_small_move_back() {
        // Hysteresis: the ghost is created at x >= 50 (interest reaches
        // 100) but only deleted below x < 30 (visit stops reaching 100).
        let mut maintainer = GhostMaintainer::new(50.0, 1);
        let mut cells = two_cells();

        assert_eq!(
            maintainer
                .tick(Position::new(55.0, 50.0), &mut cells)
                .created,
            vec![2],
            "created at the edge"
        );

        // Inside the hysteresis band [30, 50): kept, no churn.
        for x in [40.0, 35.0] {
            let delta = maintainer.tick(Position::new(x, 50.0), &mut cells);
            assert!(delta.created.is_empty() && delta.deleted.is_empty());
            assert!(cells[1].has_ghost, "kept inside the band at {x}");
        }

        // Past the band: deleted.
        let delta = maintainer.tick(Position::new(29.0, 50.0), &mut cells);
        assert_eq!(delta.deleted, vec![2]);
        assert!(!cells[1].has_ghost);
    }

    #[test]
    fn repeated_ticks_at_one_position_never_churn_the_count() {
        // The acceptance's stability half: once the count is right, ticking
        // again at the same position does nothing.
        let mut maintainer = GhostMaintainer::new(50.0, 1);
        let mut cells = two_cells();

        let delta = maintainer.tick(Position::new(55.0, 50.0), &mut cells);
        assert_eq!(delta.created, vec![2]);

        for _ in 0..5 {
            let delta = maintainer.tick(Position::new(55.0, 50.0), &mut cells);
            assert!(delta.created.is_empty() && delta.deleted.is_empty());
            assert!(cells[1].has_ghost);
        }
    }

    #[test]
    fn an_appeal_radius_widens_the_band() {
        // entity_ghost_maintainer.cpp:181: the interest area also covers the
        // entity type's appeal radius, so a wide entity ghosts further out.
        let mut maintainer = GhostMaintainer::new(50.0, 1);
        maintainer.appeal_radius = 100.0;
        let mut cells = two_cells();

        // x = 40: without the appeal radius the interest area ends at 90 and
        // no ghost is created; with it the area reaches 140.
        let delta = maintainer.tick(Position::new(40.0, 50.0), &mut cells);
        assert_eq!(delta.created, vec![2]);
        assert!(cells[1].has_ghost);
    }
}
