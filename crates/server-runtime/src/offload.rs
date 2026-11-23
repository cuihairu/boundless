//! BW-08-05-01: the Offload determination.
//!
//! Each space is partitioned by a BSP of cells. Every tick
//! (`CellAppConfig::checkOffloadsPeriod`, 0.1s) `OffloadChecker::run`
//! walks the cell's Real entities and asks each whether it must move to
//! the cell that now contains its position (`EntityGhostMaintainer::
//! checkEntityForOffload`). The decision is purely geometric plus a few
//! gates: the cell owning the entity's XZ position is looked up in the
//! BSP, and the entity offloads there unless that cell is itself, not yet
//! created, being deleted, or its channel is missing or dead.
//!
//! The acceptance, "越过测试边界时按阈值迁移", is that a Real crossing
//! the boundary between two cells migrates to the cell on the far side
//! (the boundary point itself belongs to the side reached by the branch
//! rule `coordinate < position`), while one that stays inside its home
//! cell does not.
//!
//! Reference:
//! - `entity_ghost_maintainer.cpp:56-69` `check` — the per-entity pass:
//!   run the offload check only when `cell().shouldOffload()`.
//! - `entity_ghost_maintainer.cpp:91-123` `checkEntityForOffload` — the
//!   decision: look up the cell at the position, and offload to it unless
//!   it is the home cell (`:98`), not yet created or missing (`:98`),
//!   delete-pending (`:104`), or unreachable through a dead channel
//!   (`:111-113`).
//! - `cell.cpp:611-613` `shouldOffload` — the gate is the cell's flag AND
//!   the CellApp's flag (`shouldOffload_ && CellApp::shouldOffload()`).
//! - `space.cpp:2263-2271` `pCellAt` — the space forwards the lookup to
//!   its cell tree.
//! - `space_branch.cpp:61-67` `SpaceBranch::pCellAt` — dispatch to the
//!   side the coordinate is on (`(isHorizontal_ ? z : x) < position_`),
//!   so the boundary plane belongs to the right child.
//! - `cell_info.cpp:66-75` `CellInfo::pCellAt` — a leaf answers for its
//!   point only once it has been created; an uncreated cell is
//!   "unsuitable for offloading".
//! - `offload_checker.cpp:33-53` `run` — the Real-entity loop that feeds
//!   the decision, then `sendOffloads`.

use std::collections::BTreeMap;

use crate::ghost::CellRect;
use crate::ghost_data::Address;

/// A leaf of the space's cell BSP (`CellInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// The cell's rectangle (`CellInfo::rect()`).
    pub rect: CellRect,
    /// `addr()`: the owning CellApp.
    pub address: Address,
    /// `hasBeenCreated()`: an uncreated cell is unsuitable for offloading
    /// (`cell_info.cpp:66-75`).
    pub created: bool,
    /// `isDeletePending()`: a cell being deleted is skipped
    /// (`entity_ghost_maintainer.cpp:104`).
    pub delete_pending: bool,
}

impl Cell {
    /// A created cell with no deletion pending.
    pub fn new(address: Address, rect: CellRect) -> Self {
        Cell {
            rect,
            address,
            created: true,
            delete_pending: false,
        }
    }
}

/// An internal node of the cell BSP: one partition plane (`SpaceBranch`).
#[derive(Debug, Clone, PartialEq)]
pub struct Branch {
    /// `position_`: the split coordinate.
    pub position: f64,
    /// `isHorizontal_`: split on Z (true) or on X (false).
    pub horizontal: bool,
    /// The left subtree (`coordinate < position_`).
    pub left: Box<SpaceNode>,
    /// The right subtree (`coordinate >= position_`).
    pub right: Box<SpaceNode>,
}

/// A node of the space's cell BSP (`SpaceNode`).
#[derive(Debug, Clone, PartialEq)]
pub enum SpaceNode {
    /// A leaf cell.
    Cell(Cell),
    /// A partition plane.
    Branch(Branch),
}

impl SpaceNode {
    /// `SpaceBranch::pCellAt` (`space_branch.cpp:61-67`) and
    /// `CellInfo::pCellAt` (`cell_info.cpp:66-75`): the leaf owning the
    /// point, or `None` when the point falls in an uncreated cell.
    ///
    /// A branch dispatches to the side the coordinate is on, never falling
    /// back to the other side, so a point exactly on the plane belongs to
    /// the right child.
    pub fn p_cell_at(&self, x: f64, z: f64) -> Option<&Cell> {
        match self {
            SpaceNode::Cell(cell) => {
                if cell.created {
                    Some(cell)
                } else {
                    None
                }
            }
            SpaceNode::Branch(branch) => {
                let on_left = if branch.horizontal {
                    z < branch.position
                } else {
                    x < branch.position
                };
                if on_left {
                    branch.left.p_cell_at(x, z)
                } else {
                    branch.right.p_cell_at(x, z)
                }
            }
        }
    }
}

/// The offload gate (`Cell::shouldOffload`, `cell.cpp:611-613`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffloadGate {
    /// `Cell::shouldOffload_`.
    pub cell_should_offload: bool,
    /// `CellApp::shouldOffload()`.
    pub app_should_offload: bool,
}

impl OffloadGate {
    /// `Cell::shouldOffload()`: both the cell and the CellApp must permit
    /// offloading.
    pub fn enabled(&self) -> bool {
        self.cell_should_offload && self.app_should_offload
    }
}

impl Default for OffloadGate {
    /// Both flags default on (`cell.cpp:84`, `cellapp.cpp:398`).
    fn default() -> Self {
        OffloadGate {
            cell_should_offload: true,
            app_should_offload: true,
        }
    }
}

/// The result of `EntityGhostMaintainer::checkEntityForOffload`
/// (`entity_ghost_maintainer.cpp:91-123`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffloadDecision {
    /// The Real stays on its current cell.
    Stay,
    /// The Real migrates to the cell that now contains its position.
    OffloadTo(Address),
}

/// `EntityGhostMaintainer::checkEntityForOffload`
/// (`entity_ghost_maintainer.cpp:91-123`): decide whether a Real must move
/// to the cell that contains its position.
///
/// `channels` maps a cell address to whether its channel is alive
/// (`CellAppChannel::isGood`); a missing or dead channel keeps the entity
/// home. `own_cell` is the address of the entity's current cell.
pub fn decide_offload(
    position: (f64, f64),
    own_cell: Address,
    tree: &SpaceNode,
    gate: &OffloadGate,
    channels: &BTreeMap<Address, bool>,
) -> OffloadDecision {
    // cell().shouldOffload() (`entity_ghost_maintainer.cpp:58`).
    if !gate.enabled() {
        return OffloadDecision::Stay;
    }

    // Find out where we really want to live (`:95`).
    let Some(home) = tree.p_cell_at(position.0, position.1) else {
        // Don't offload to an uncreated cell (`:98`).
        return OffloadDecision::Stay;
    };

    // Don't offload to ourselves (`:98`).
    if home.address == own_cell {
        return OffloadDecision::Stay;
    }

    // Don't offload to a cell that is being deleted (`:104`).
    if home.delete_pending {
        return OffloadDecision::Stay;
    }

    // Don't offload if the other cell has failed or died (`:111-113`).
    if !channels.get(&home.address).copied().unwrap_or(false) {
        return OffloadDecision::Stay;
    }

    OffloadDecision::OffloadTo(home.address)
}

/// `OffloadChecker::run` (`offload_checker.cpp:33-53`): the per-Real
/// decision over a cell, returning the offload entries
/// (`addToOffloads` + `sendOffloads`) as `(entity_id, destination)`.
pub fn collect_offloads(
    reals: &[(u32, (f64, f64), Address)],
    tree: &SpaceNode,
    gate: &OffloadGate,
    channels: &BTreeMap<Address, bool>,
) -> Vec<(u32, Address)> {
    reals
        .iter()
        .filter_map(|(id, position, own_cell)| {
            match decide_offload(*position, *own_cell, tree, gate, channels) {
                OffloadDecision::OffloadTo(destination) => Some((*id, destination)),
                OffloadDecision::Stay => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two cells split at X = 100: cell 100 on the left, cell 200 on the
    /// right (`SpaceBranch` with `horizontal = false`).
    fn two_cells() -> SpaceNode {
        SpaceNode::Branch(Branch {
            position: 100.0,
            horizontal: false,
            left: Box::new(SpaceNode::Cell(Cell::new(
                Address(100),
                CellRect::new(0.0, 0.0, 100.0, 100.0),
            ))),
            right: Box::new(SpaceNode::Cell(Cell::new(
                Address(200),
                CellRect::new(100.0, 0.0, 200.0, 100.0),
            ))),
        })
    }

    fn alive(channels: &[(Address, bool)]) -> BTreeMap<Address, bool> {
        channels.iter().copied().collect()
    }

    #[test]
    fn p_cell_at_dispatches_to_the_side_of_the_coordinate() {
        let tree = two_cells();

        // A point on the left side, on the plane, and on the right side.
        assert_eq!(
            tree.p_cell_at(50.0, 50.0).map(|c| c.address),
            Some(Address(100))
        );
        assert_eq!(
            tree.p_cell_at(99.9, 50.0).map(|c| c.address),
            Some(Address(100))
        );
        assert_eq!(
            tree.p_cell_at(100.0, 50.0).map(|c| c.address),
            Some(Address(200)),
            "the boundary plane belongs to the right child (x < position_)"
        );
        assert_eq!(
            tree.p_cell_at(150.0, 50.0).map(|c| c.address),
            Some(Address(200))
        );
    }

    #[test]
    fn a_horizontal_branch_splits_on_z() {
        let tree = SpaceNode::Branch(Branch {
            position: 50.0,
            horizontal: true,
            left: Box::new(SpaceNode::Cell(Cell::new(
                Address(300),
                CellRect::new(0.0, 0.0, 100.0, 50.0),
            ))),
            right: Box::new(SpaceNode::Cell(Cell::new(
                Address(400),
                CellRect::new(0.0, 50.0, 100.0, 100.0),
            ))),
        });

        assert_eq!(
            tree.p_cell_at(10.0, 40.0).map(|c| c.address),
            Some(Address(300))
        );
        assert_eq!(
            tree.p_cell_at(10.0, 50.0).map(|c| c.address),
            Some(Address(400)),
            "z == position belongs to the right child"
        );
    }

    /// A nested BSP: an outer X split at 100, each side split on Z at 50.
    #[test]
    fn a_nested_tree_resolves_to_the_leaf_containing_the_point() {
        fn quad(x_split: f64, left_addr: Address, right_addr: Address) -> SpaceNode {
            SpaceNode::Branch(Branch {
                position: x_split,
                horizontal: false,
                left: Box::new(SpaceNode::Branch(Branch {
                    position: 50.0,
                    horizontal: true,
                    left: Box::new(SpaceNode::Cell(Cell::new(
                        left_addr,
                        CellRect::new(x_split - 100.0, 0.0, x_split, 50.0),
                    ))),
                    right: Box::new(SpaceNode::Cell(Cell::new(
                        Address(0),
                        CellRect::new(x_split - 100.0, 50.0, x_split, 100.0),
                    ))),
                })),
                right: Box::new(SpaceNode::Branch(Branch {
                    position: 50.0,
                    horizontal: true,
                    left: Box::new(SpaceNode::Cell(Cell::new(
                        right_addr,
                        CellRect::new(x_split, 0.0, x_split + 100.0, 50.0),
                    ))),
                    right: Box::new(SpaceNode::Cell(Cell::new(
                        Address(0),
                        CellRect::new(x_split, 50.0, x_split + 100.0, 100.0),
                    ))),
                })),
            })
        }

        let tree = quad(100.0, Address(100), Address(200));
        assert_eq!(
            tree.p_cell_at(10.0, 10.0).map(|c| c.address),
            Some(Address(100))
        );
        assert_eq!(
            tree.p_cell_at(110.0, 10.0).map(|c| c.address),
            Some(Address(200))
        );
        assert_eq!(
            tree.p_cell_at(10.0, 70.0).map(|c| c.address),
            Some(Address(0)),
            "the upper-left quadrant is its own cell"
        );
    }

    /// An uncreated leaf deep in a nested tree answers None
    /// (`cell_info.cpp:66-75`).
    #[test]
    fn an_uncreated_leaf_in_a_nested_tree_is_unsuitable() {
        let tree = SpaceNode::Branch(Branch {
            position: 50.0,
            horizontal: false,
            left: Box::new(SpaceNode::Cell(Cell {
                rect: CellRect::new(0.0, 0.0, 50.0, 100.0),
                address: Address(500),
                created: false,
                delete_pending: false,
            })),
            right: Box::new(SpaceNode::Cell(Cell::new(
                Address(600),
                CellRect::new(50.0, 0.0, 100.0, 100.0),
            ))),
        });

        assert_eq!(tree.p_cell_at(10.0, 10.0), None, "not yet created");
        assert_eq!(
            tree.p_cell_at(60.0, 10.0).map(|c| c.address),
            Some(Address(600)),
            "the created sibling still resolves"
        );
    }

    /// The acceptance: a Real crossing the boundary migrates to the far
    /// cell; one that stays inside its home cell does not.
    #[test]
    fn crossing_the_boundary_migrates_by_the_threshold() {
        let tree = two_cells();
        let gate = OffloadGate::default();
        let channels = alive(&[(Address(100), true), (Address(200), true)]);

        // Inside cell 100, the entity's home.
        assert_eq!(
            decide_offload((50.0, 50.0), Address(100), &tree, &gate, &channels),
            OffloadDecision::Stay,
            "still inside the home cell"
        );

        // Just before the boundary.
        assert_eq!(
            decide_offload((99.9, 50.0), Address(100), &tree, &gate, &channels),
            OffloadDecision::Stay
        );

        // At and past the boundary: the point belongs to cell 200.
        assert_eq!(
            decide_offload((100.0, 50.0), Address(100), &tree, &gate, &channels),
            OffloadDecision::OffloadTo(Address(200)),
            "crossing the boundary migrates"
        );
        assert_eq!(
            decide_offload((150.0, 50.0), Address(100), &tree, &gate, &channels),
            OffloadDecision::OffloadTo(Address(200))
        );
    }

    /// A Real already living on the far cell is not offloaded to itself.
    #[test]
    fn a_real_on_the_far_cell_does_not_offload_to_itself() {
        let tree = two_cells();
        let gate = OffloadGate::default();
        let channels = alive(&[(Address(100), true), (Address(200), true)]);

        assert_eq!(
            decide_offload((150.0, 50.0), Address(200), &tree, &gate, &channels),
            OffloadDecision::Stay,
            "don't offload to ourselves"
        );
    }

    #[test]
    fn a_disabled_offload_keeps_everything_home() {
        let tree = two_cells();
        let channels = alive(&[(Address(100), true), (Address(200), true)]);

        for gate in [
            OffloadGate {
                cell_should_offload: false,
                app_should_offload: true,
            },
            OffloadGate {
                cell_should_offload: true,
                app_should_offload: false,
            },
        ] {
            assert_eq!(
                decide_offload((150.0, 50.0), Address(100), &tree, &gate, &channels),
                OffloadDecision::Stay,
                "offload disabled"
            );
        }
    }

    #[test]
    fn an_uncreated_or_deleted_home_cell_keeps_the_entity_home() {
        let gate = OffloadGate::default();
        let channels = alive(&[(Address(100), true), (Address(200), true)]);

        // The far cell is not yet created.
        let uncreated = SpaceNode::Branch(Branch {
            position: 100.0,
            horizontal: false,
            left: Box::new(SpaceNode::Cell(Cell::new(
                Address(100),
                CellRect::new(0.0, 0.0, 100.0, 100.0),
            ))),
            right: Box::new(SpaceNode::Cell(Cell {
                rect: CellRect::new(100.0, 0.0, 200.0, 100.0),
                address: Address(200),
                created: false,
                delete_pending: false,
            })),
        });
        assert_eq!(
            decide_offload((150.0, 50.0), Address(100), &uncreated, &gate, &channels),
            OffloadDecision::Stay,
            "an uncreated cell is unsuitable for offloading"
        );

        // The far cell is being deleted.
        let mut deleted = two_cells();
        if let SpaceNode::Branch(branch) = &mut deleted
            && let SpaceNode::Cell(cell) = branch.right.as_mut()
        {
            cell.delete_pending = true;
        }
        assert_eq!(
            decide_offload((150.0, 50.0), Address(100), &deleted, &gate, &channels),
            OffloadDecision::Stay,
            "don't offload to a cell being deleted"
        );
    }

    #[test]
    fn a_dead_or_missing_destination_channel_keeps_the_entity_home() {
        let tree = two_cells();
        let gate = OffloadGate::default();

        // The channel is dead.
        assert_eq!(
            decide_offload(
                (150.0, 50.0),
                Address(100),
                &tree,
                &gate,
                &alive(&[(Address(100), true), (Address(200), false)])
            ),
            OffloadDecision::Stay,
            "don't offload to a dead channel"
        );

        // The channel is missing entirely.
        assert_eq!(
            decide_offload(
                (150.0, 50.0),
                Address(100),
                &tree,
                &gate,
                &alive(&[(Address(100), true)])
            ),
            OffloadDecision::Stay,
            "don't offload when the destination is unknown"
        );
    }

    /// The per-cell loop (`OffloadChecker::run`): each Real decides
    /// independently, and only the ones that crossed migrate.
    #[test]
    fn collect_offloads_migrates_only_the_reals_that_crossed() {
        let tree = two_cells();
        let gate = OffloadGate::default();
        let channels = alive(&[(Address(100), true), (Address(200), true)]);

        let reals = [
            // (id, position, own cell)
            (1, (50.0, 50.0), Address(100)),  // stays home
            (2, (150.0, 50.0), Address(100)), // crossed to cell 200
            (3, (150.0, 50.0), Address(200)), // already on cell 200
            (4, (100.0, 50.0), Address(100)), // on the boundary: cell 200
        ];

        assert_eq!(
            collect_offloads(&reals, &tree, &gate, &channels),
            vec![(2, Address(200)), (4, Address(200))],
            "only the reals whose position is in another cell migrate"
        );
    }
}
