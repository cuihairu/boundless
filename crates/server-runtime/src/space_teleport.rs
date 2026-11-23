//! BW-08-05-05: cross-space teleport.
//!
//! A teleport moves an entity's Real to another Space. On the old space the
//! Real deletes every Ghost it had (`delGhost` to each haunt), because they
//! are recreated on the destination Space rather than moved. On the new Space
//! the visible set is rebuilt from the destination AOI: every cell whose
//! rectangle intersects the entity's interest area at the teleport position
//! gets a fresh Ghost.
//!
//! The acceptance, "新旧 Space 的可见集吻合", is that the new Space's
//! visible set equals the destination-AOI-derived set (and excludes the
//! Real's own cell), while the old Space's visible set is emptied.
//!
//! Reference:
//! - `real_entity.cpp:1183-1342` `RealEntity::teleport` — the whole flow:
//!   a same-space position teleport (`nearbyMBRef.id == 0`) just sets the
//!   position and direction (`:1203-1221`); a cross-space teleport deletes
//!   every old-space ghost (`:1258-1269`), calls `onLeavingCell` (`:1276`),
//!   re-adds the destination as a haunt (`:1284`), writes a fresh ghost
//!   (`:1285-1293`) and offloads with `isTeleport = true` (`:1327`).
//! - `space.cpp:273` `createGhost` — the destination Space builds the ghost
//!   from the streamed data.
//! - `real_entity.cpp:339` `readOffloadData` — `hasChangedSpace` sends a
//!   `delGhost` to the old haunts rather than a `ghostSetReal`.
//! - `entity_ghost_maintainer.cpp:180-184` — the interest area that decides
//!   whether a cell sees the entity.
//! - `space.cpp:228-270` `createGhost` message — buffered when the entity
//!   already exists or a subsequence is delayed.

use crate::ghost::{Cell, GhostMaintainer};
use crate::range_list::Position;

/// The cross-space ghost-set change one teleport produces.
#[derive(Debug, Clone, PartialEq)]
pub struct TeleportOutcome {
    /// The Ghosts deleted from the old Space — every haunt
    /// (`RealEntity::teleport`, `real_entity.cpp:1258-1269`).
    pub deleted_old_space: Vec<u32>,
    /// The Ghosts created in the new Space — the destination-AOI cells
    /// (`Space::createGhost`, `space.cpp:273`).
    pub created_new_space: Vec<u32>,
}

/// `RealEntity::teleport` (`real_entity.cpp:1183`): the ghost-set change of a
/// cross-space teleport. Every old-space Ghost is deleted, and the new
/// Space's Ghosts are recreated from the destination AOI around `position`.
///
/// `maintainer` is pointed at the destination cell (`set_own_cell`), so the
/// Real's new home cell never receives a Ghost of itself.
pub fn teleport_visible_sets(
    old_haunts: &[u32],
    position: Position,
    maintainer: &GhostMaintainer,
    new_cells: &[Cell],
) -> TeleportOutcome {
    // Delete all the old-space Ghosts; they are recreated on the destination.
    let deleted_old_space = old_haunts.to_vec();

    // Recreate the new-space Ghosts from the destination interest area
    // (`entity_ghost_maintainer.cpp:180`).
    let created_new_space = new_space_visible_set(position, maintainer, new_cells);

    TeleportOutcome {
        deleted_old_space,
        created_new_space,
    }
}

/// The visible set of the new Space: the cells whose rectangle intersects the
/// entity's interest area at `position`, excluding the Real's own cell.
pub fn new_space_visible_set(
    position: Position,
    maintainer: &GhostMaintainer,
    cells: &[Cell],
) -> Vec<u32> {
    let interest = maintainer.interest_area(position);
    cells
        .iter()
        .filter(|cell| cell.id != maintainer.own_cell() && interest.intersects(cell.rect))
        .map(|cell| cell.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghost::CellRect;

    fn cell(id: u32, x_min: f64, z_min: f64, x_max: f64, z_max: f64) -> Cell {
        Cell::new(id, CellRect::new(x_min, z_min, x_max, z_max))
    }

    /// The new Space: a 2x2 grid of 100-wide cells, cell 3 the Real's
    /// destination home in the lower-left corner.
    fn new_space() -> Vec<Cell> {
        vec![
            cell(3, 0.0, 0.0, 100.0, 100.0),
            cell(4, 100.0, 0.0, 200.0, 100.0),
            cell(5, 0.0, 100.0, 100.0, 200.0),
            cell(6, 100.0, 100.0, 200.0, 200.0),
        ]
    }

    /// A 20-unit reach: short enough to pick out a single neighbour.
    fn maintainer() -> GhostMaintainer {
        GhostMaintainer::new(20.0, 3)
    }

    /// The acceptance: the new Space's visible set matches the destination
    /// AOI, and the old Space's visible set is emptied.
    #[test]
    fn the_new_space_visible_set_matches_the_destination_aoi() {
        let maintainer = maintainer();
        let new_cells = new_space();

        // Landed near the corner between cells 4, 5 and 6: all three are
        // within the interest area, so the new Space sees the entity.
        let outcome =
            teleport_visible_sets(&[1, 2], Position::new(95.0, 95.0), &maintainer, &new_cells);

        assert_eq!(
            outcome.deleted_old_space,
            vec![1, 2],
            "every old-space ghost is deleted"
        );
        assert_eq!(
            outcome.created_new_space,
            vec![4, 5, 6],
            "the new visible set is exactly the destination-AOI cells"
        );
        assert_eq!(
            new_space_visible_set(Position::new(95.0, 95.0), &maintainer, &new_cells),
            outcome.created_new_space,
            "the new Space's visible set matches the destination AOI"
        );

        // A teleport deep inside the destination cell, out of reach of every
        // neighbour: the new Space sees nothing.
        let outcome =
            teleport_visible_sets(&[1, 2], Position::new(50.0, 50.0), &maintainer, &new_cells);
        assert_eq!(outcome.created_new_space, Vec::<u32>::new());
    }

    /// The Real's own (destination) cell is never a Ghost of itself
    /// (`entity_ghost_maintainer.cpp:201`).
    #[test]
    fn the_destination_cell_is_not_a_ghost_of_itself() {
        let maintainer = maintainer();
        let new_cells = new_space();

        // Even a position that interests cell 3 does not give it a Ghost.
        let visible = new_space_visible_set(Position::new(10.0, 10.0), &maintainer, &new_cells);
        assert!(!visible.contains(&3), "the own cell is never a haunt");
    }

    /// A later teleport re-points the own cell, so the old home becomes a
    /// visible neighbour instead.
    #[test]
    fn a_second_teleport_repoints_the_own_cell() {
        let mut maintainer = maintainer();
        let new_cells = new_space();

        assert_eq!(
            new_space_visible_set(Position::new(90.0, 50.0), &maintainer, &new_cells),
            vec![4],
            "landed on cell 3's eastern edge, cell 4 sees it"
        );

        // The Real moves on into cell 4: cell 4 is the own cell now, and cell
        // 3 is the visible neighbour.
        maintainer.set_own_cell(4);
        assert_eq!(
            new_space_visible_set(Position::new(110.0, 50.0), &maintainer, &new_cells),
            vec![3],
            "the old home becomes a visible neighbour"
        );
    }

    /// An empty old-space haunt list means nothing to delete.
    #[test]
    fn a_fresh_entity_deletes_nothing_from_the_old_space() {
        let maintainer = maintainer();
        let new_cells = new_space();

        let outcome =
            teleport_visible_sets(&[], Position::new(90.0, 50.0), &maintainer, &new_cells);
        assert!(outcome.deleted_old_space.is_empty());
        assert_eq!(outcome.created_new_space, vec![4]);
    }
}
