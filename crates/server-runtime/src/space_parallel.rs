//! OPT-08-01-03: spatial parallelism, evaluated.
//!
//! The space is a BSP of cells; the shipped offload checker
//! (`decide_offload` + `collect_offloads`, the BW-08-05-01 faces) is
//! the boundary-crossing discipline this evaluation drives. The dense
//! 10×12 field of the AoI baseline straddles a partition plane at
//! x = 16 m — the same deterministic step schedule — and every
//! crossing must migrate to the far cell's side, while every shipped
//! gate (uncreated far cell, dead or missing channel, delete-pending
//! far cell, disabled gate) keeps its entity home.
//!
//! The acceptance **报告跨边界时序及耗时** (report the cross-boundary
//! timing and cost). The structural counts are asserted: the boundary
//! plane belongs to the right side (the `<` branch rule) for both
//! split axes, exactly one migration per crossing, each migration
//! lands on the side that contains the entity, and the four Stay
//! gates hold. The wall-clock of the whole check is probe data,
//! printed for the review doc, never asserted.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::aoi_baseline::{DENSE_ENTITIES, GRID_COLS, SPACING, STEP, step_of};
use crate::cellappmgr::Rect;
use crate::ghost_data::Address;
use crate::offload::{Branch, Cell, OffloadGate, SpaceNode, collect_offloads};

/// The two cells' addresses: the left of the plane is home.
pub const HOME: Address = Address(100);
/// The far side every crossing migrates to.
pub const FAR: Address = Address(200);
/// The partition plane, mid-field — exactly on the grid column 4
/// boundary (the field spans x 0..36 m at 4 m spacing).
pub const SPLIT: f64 = 16.0;

/// A created leaf cell at one address.
pub fn leaf_cell(address: Address) -> SpaceNode {
    SpaceNode::Cell(Cell {
        rect: Rect {
            xmin: -1_000.0,
            ymin: -1_000.0,
            xmax: 1_000.0,
            ymax: 1_000.0,
        },
        address,
        created: true,
        delete_pending: false,
    })
}

/// An uncreated leaf cell at one address.
pub fn uncreated_cell(address: Address) -> SpaceNode {
    SpaceNode::Cell(Cell {
        rect: Rect {
            xmin: -1_000.0,
            ymin: -1_000.0,
            xmax: 1_000.0,
            ymax: 1_000.0,
        },
        address,
        created: false,
        delete_pending: false,
    })
}

/// Two cells split at [`SPLIT`] on X.
pub fn two_cell_tree() -> SpaceNode {
    SpaceNode::Branch(Branch {
        position: SPLIT,
        horizontal: false,
        left: Box::new(leaf_cell(HOME)),
        right: Box::new(leaf_cell(FAR)),
    })
}

/// Two cells split at `position` on Z.
pub fn z_tree(position: f64) -> SpaceNode {
    SpaceNode::Branch(Branch {
        position,
        horizontal: true,
        left: Box::new(leaf_cell(HOME)),
        right: Box::new(leaf_cell(FAR)),
    })
}

/// The dense field on the AoI baseline's step schedule (tick 0's
/// draw), each entity homed on the side its grid column starts on.
pub fn dense_field() -> Vec<(u32, (f64, f64), Address)> {
    (0..DENSE_ENTITIES)
        .map(|index| {
            let id = (index + 2) as u32;
            let grid_x = (index % GRID_COLS) as f64 * f64::from(SPACING);
            let grid_z = (index / GRID_COLS) as f64 * f64::from(SPACING);
            let (dx, dz) = step_of(id, 0);
            let home = if grid_x < SPLIT { HOME } else { FAR };
            (
                id,
                (
                    grid_x + f64::from(dx) * f64::from(STEP),
                    grid_z + f64::from(dz) * f64::from(STEP),
                ),
                home,
            )
        })
        .collect()
}

/// The cross-boundary check's ledger.
#[derive(Debug)]
pub struct CrossBoundaryLedger {
    /// Entities whose position's cell is not their own.
    pub crossings: usize,
    /// Decisions that migrated them.
    pub offloads: usize,
    /// Decisions that stayed (crossings the gates kept home + the
    /// home-side entities).
    pub stays: usize,
    /// Probe cost of the whole check, in milliseconds — printed.
    pub check_ms: f64,
}

/// Runs the shipped check over the field.
pub fn run_check(
    reals: &[(u32, (f64, f64), Address)],
    tree: &SpaceNode,
    gate: &OffloadGate,
    channels: &BTreeMap<Address, bool>,
) -> CrossBoundaryLedger {
    let started = Instant::now();
    let decisions = collect_offloads(reals, tree, gate, channels);
    let check_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let crossings = reals
        .iter()
        .filter(|(_, pos, own)| tree.p_cell_at(pos.0, pos.1).map(|c| c.address) != Some(*own))
        .count();
    CrossBoundaryLedger {
        crossings,
        offloads: decisions.len(),
        stays: reals.len() - decisions.len(),
        check_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offload::{OffloadDecision, decide_offload};

    #[test]
    fn the_boundary_plane_lands_on_the_right_side() {
        // The branch rule `coordinate < position` gives the plane to
        // the right child, on both split axes.
        let tree = two_cell_tree();
        assert_eq!(
            tree.p_cell_at(SPLIT, 0.0).map(|c| c.address),
            Some(FAR),
            "the plane belongs to the right child"
        );
        assert_eq!(tree.p_cell_at(15.999, 0.0).map(|c| c.address), Some(HOME));

        let z_split = z_tree(22.0);
        assert_eq!(z_split.p_cell_at(0.0, 22.0).map(|c| c.address), Some(FAR));
        assert_eq!(
            z_split.p_cell_at(0.0, 21.999).map(|c| c.address),
            Some(HOME)
        );
    }

    #[test]
    fn the_dense_crossings_migrate_exactly_once_to_the_far_side() {
        let reals = dense_field();
        let tree = two_cell_tree();
        let gate = OffloadGate::default();
        let channels = BTreeMap::from([(HOME, true), (FAR, true)]);

        let ledger = run_check(&reals, &tree, &gate, &channels);
        assert!(ledger.crossings > 0, "the dense field actually crosses");
        assert_eq!(
            ledger.offloads, ledger.crossings,
            "exactly one migration per crossing"
        );
        assert!(ledger.stays > 0, "the home side stays put");

        // Every migration lands on the side that contains the entity.
        let decisions = collect_offloads(&reals, &tree, &gate, &channels);
        for (id, destination) in &decisions {
            let (_, pos, _) = reals.iter().find(|(i, _, _)| i == id).expect("the entity");
            assert_eq!(
                tree.p_cell_at(pos.0, pos.1).map(|c| c.address),
                Some(*destination),
                "entity {id} migrates to its containing cell"
            );
        }

        // A straddler exactly on the plane, homed left, migrates: the
        // plane is owned by the right.
        assert_eq!(
            decide_offload((SPLIT, 0.0), HOME, &tree, &gate, &channels),
            OffloadDecision::OffloadTo(FAR)
        );

        println!(
            "SPACE-PARALLEL crossings={} offloads={} stays={} check_ms={:.4}",
            ledger.crossings, ledger.offloads, ledger.stays, ledger.check_ms
        );
    }

    #[test]
    fn the_far_cell_gates_keep_the_entity_home() {
        let tree = two_cell_tree();
        let gate = OffloadGate::default();
        let all = BTreeMap::from([(HOME, true), (FAR, true)]);
        let straddler = (SPLIT + 0.5, 0.0);

        // An uncreated far cell is unsuitable for offloading.
        let uncreated = SpaceNode::Branch(Branch {
            position: SPLIT,
            horizontal: false,
            left: Box::new(leaf_cell(HOME)),
            right: Box::new(uncreated_cell(FAR)),
        });
        assert_eq!(
            decide_offload(straddler, HOME, &uncreated, &gate, &all),
            OffloadDecision::Stay,
            "an uncreated far cell is unsuitable for offloading"
        );

        // A dead-channel far cell keeps the entity home.
        let dead = BTreeMap::from([(HOME, true), (FAR, false)]);
        assert_eq!(
            decide_offload(straddler, HOME, &tree, &gate, &dead),
            OffloadDecision::Stay
        );

        // A missing channel entry is a dead channel.
        let missing = BTreeMap::from([(HOME, true)]);
        assert_eq!(
            decide_offload(straddler, HOME, &tree, &gate, &missing),
            OffloadDecision::Stay
        );

        // A delete-pending far cell is skipped.
        let delete_pending = SpaceNode::Branch(Branch {
            position: SPLIT,
            horizontal: false,
            left: Box::new(leaf_cell(HOME)),
            right: Box::new(SpaceNode::Cell(Cell {
                rect: Rect {
                    xmin: -1_000.0,
                    ymin: -1_000.0,
                    xmax: 1_000.0,
                    ymax: 1_000.0,
                },
                address: FAR,
                created: true,
                delete_pending: true,
            })),
        });
        assert_eq!(
            decide_offload(straddler, HOME, &delete_pending, &gate, &all),
            OffloadDecision::Stay,
            "a delete-pending cell is skipped"
        );

        // A disabled gate stops every offload.
        let disabled = OffloadGate {
            cell_should_offload: false,
            ..OffloadGate::default()
        };
        assert_eq!(
            decide_offload(straddler, HOME, &tree, &disabled, &all),
            OffloadDecision::Stay,
            "the gate stops every offload"
        );
    }

    #[test]
    fn the_check_reruns_to_the_same_ledger() {
        let reals = dense_field();
        let tree = two_cell_tree();
        let gate = OffloadGate::default();
        let channels = BTreeMap::from([(HOME, true), (FAR, true)]);
        let first = run_check(&reals, &tree, &gate, &channels);
        let second = run_check(&reals, &tree, &gate, &channels);
        assert_eq!(first.crossings, second.crossings);
        assert_eq!(first.offloads, second.offloads);
        assert_eq!(first.stays, second.stays);
    }
}
