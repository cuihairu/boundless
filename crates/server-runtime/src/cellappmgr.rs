//! BW-05-04 the CellAppMgr partitioning and load balancing.
//!
//! The reference `server/cellappmgr/` owns the space partitioning tree: a
//! [`Space`] holds a BSP tree whose leaves are the cells of that space
//! (`space.cpp`, `cell_data.cpp`, `internal_node.cpp`), a [`CellAppRecord`]
//! is one registered CellApp process (`cellapp.cpp`, `cellapps.cpp`), and
//! [`CellAppMgr`] coordinates the two (`cellappmgr.cpp`). A new cell starts
//! with zero area at the far edge of the leaf it splits, and the periodic
//! balance pass moves the partition line between the entity bounds reported
//! by the CellApps until the smoothed loads agree.
//!
//! The Mercury traffic is not modelled here: the module records the state
//! transitions and collects the outbound messages the reference would send
//! into the [`Outbound`] outbox, so the Mercury lane can drive the sockets
//! when it lands. Addresses are `(ip, port)` with `ip` in network byte
//! order, matching the rest of the workspace.

use std::collections::{BTreeMap, BTreeSet};

use foundation::stream::BinaryStream;

/// A Mercury address as `(ip, port)` with `ip` in network byte order.
pub type Addr = (u32, u16);

/// `SpaceID`.
pub type SpaceId = u32;

/// `CellAppID`.
pub type CellAppId = u32;

/// `NULL_SPACE_ID` (`server/common.hpp`).
pub const NULL_SPACE_ID: SpaceId = 0;

// -----------------------------------------------------------------------------
// Section: configuration
// -----------------------------------------------------------------------------

/// `CellAppLoadConfig::lowerBound` (`cellAppMgr/cellAppLoad/lowerBound`).
pub const LOWER_BOUND: f32 = 0.02;
/// `CellAppLoadConfig::safetyBound`.
pub const SAFETY_BOUND: f32 = 0.85;
/// `CellAppLoadConfig::safetyRatio`.
pub const SAFETY_RATIO: f32 = 1.1;
/// `CellAppLoadConfig::warningLevel`.
pub const WARNING_LEVEL: f32 = 0.75;
/// `CellAppMgrConfig::maxLoadingCells`.
pub const MAX_LOADING_CELLS: usize = 4;
/// `CellAppMgrConfig::minLoadingArea`, in square units.
pub const MIN_LOADING_AREA: f32 = 1_000_000.0;
/// `CellAppMgrConfig::estimatedInitialCellLoad`, the load added to a CellApp
/// for each cell that is given to it.
pub const ESTIMATED_INITIAL_CELL_LOAD: f32 = 0.1;
/// `CellAppMgrConfig::loadSmoothingBias`.
pub const LOAD_SMOOTHING_BIAS: f32 = 0.05;
/// `CellAppMgrConfig::metaLoadBalanceTolerance`.
pub const META_LOAD_BALANCE_TOLERANCE: f32 = 0.05;
/// `CellAppMgrConfig::metaLoadBalancePeriod`, seconds.
pub const META_LOAD_BALANCE_PERIOD: f32 = 3.0;
/// `CellAppMgrConfig::loadBalancePeriod`, seconds.
pub const LOAD_BALANCE_PERIOD: f32 = 1.0;
/// `CellAppMgrConfig::ghostDistance`, in world units.
pub const GHOST_DISTANCE: f32 = 500.0;
/// `CellAppMgrConfig::cellAppTimeout`, seconds.
pub const CELL_APP_TIMEOUT: f32 = 3.0;
/// `CellAppMgrConfig::shouldLimitBalanceToChunks`.
pub const SHOULD_LIMIT_BALANCE_TO_CHUNKS: bool = true;
/// `LoginConditionsConfig::avgLoad` for a CellApp.
pub const LOGIN_AVG_LOAD: f32 = 0.85;
/// `LoginConditionsConfig::maxLoad` for a CellApp.
pub const LOGIN_MAX_LOAD: f32 = 0.9;
/// `LoginConditionsConfig::avgOverloadTolerancePeriod`, seconds.
pub const AVG_OVERLOAD_TOLERANCE_SECS: f32 = 5.0;
/// `LoginConditionsConfig::maxOverloadTolerancePeriod`, seconds.
pub const MAX_OVERLOAD_TOLERANCE_SECS: f32 = 5.0;

/// `CellData::calculateAreaNotLoaded`: the epsilon `CellData` subtracts from
/// `ghostDistance` before inflating a cell's range, so the chunk boundary is
/// a hair inside the range to dodge floating point noise.
pub const FLOATING_POINT_TOLERANCE: f32 = 1.0;
/// `BalanceConfig::LEVEL_STEP`, the factor by which each offload level
/// halves the amount of CPU it tries to move.
pub const LEVEL_STEP: f32 = 0.5;
/// `BalanceConfig::maxCPUOffload`, the fraction of a CPU offloaded by the
/// lowest level.
pub const MAX_CPU_OFFLOAD: f32 = 0.02;
/// `BalanceConfig::numCPUOffloadLevels`.
pub const NUM_CPU_OFFLOAD_LEVELS: usize = 5;
/// `BalanceConfig::aggressionDecrease`, multiplied into the aggression when
/// the balance direction reverses.
pub const AGGRESSION_DECREASE: f32 = 0.5;
/// `BalanceConfig::maxAggression`, the ceiling aggression is clamped to.
pub const MAX_AGGRESSION: f32 = 1.0;
/// `BalanceConfig::aggressionIncreasePeriod`, seconds.
pub const AGGRESSION_INCREASE_PERIOD: f32 = 10.0;
/// `BalanceConfig::maxAggressionIncreasePeriod`, seconds.
pub const MAX_AGGRESSION_INCREASE_PERIOD: f32 = 40.0;

/// `BalanceConfig::cpuOffloadForLevel`: the amount of CPU to try to offload
/// for one entity bounds level. The lowest level offloads the least.
///
/// ```text
/// maxCPUOffload * LEVEL_STEP.powi(level)
/// ```
pub fn cpu_offload_for_level(level: usize) -> f32 {
    MAX_CPU_OFFLOAD * LEVEL_STEP.powi(level as i32)
}

/// `BalanceConfig::minCPUOffload`: the smallest offload amount, which is
/// below this and the balance pass moves nothing.
pub fn min_cpu_offload() -> f32 {
    cpu_offload_for_level(NUM_CPU_OFFLOAD_LEVELS - 1)
}

/// `BalanceConfig::postInit`: `minAggression` is the aggression left after
/// `maxAggressionIncreasePeriod / aggressionIncreasePeriod` decreasing
/// steps.
pub fn min_aggression() -> f32 {
    let num_steps = MAX_AGGRESSION_INCREASE_PERIOD / AGGRESSION_INCREASE_PERIOD;
    MAX_AGGRESSION * AGGRESSION_DECREASE.powf(num_steps)
}

/// `BalanceConfig::postInit`: the aggression growth per tick that reclaims a
/// decrease over `aggressionIncreasePeriod` seconds.
pub fn aggression_increase() -> f32 {
    let ticks_to_reclaim = AGGRESSION_INCREASE_PERIOD / LOAD_BALANCE_PERIOD;
    let reclaim_amount = 1.0 / AGGRESSION_DECREASE;
    (reclaim_amount.ln() / ticks_to_reclaim).exp()
}

/// `BalanceConfig::clampedAggression`.
pub fn clamped_aggression(aggression: f32) -> f32 {
    aggression.clamp(min_aggression(), MAX_AGGRESSION)
}

// -----------------------------------------------------------------------------
// Section: Rect
// -----------------------------------------------------------------------------

/// One axis of a [`Rect`], mirroring `BW::Rect`'s `range1D` view.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Range1d {
    /// The lower end of the interval.
    pub min: f32,
    /// The upper end of the interval.
    pub max: f32,
}

impl Range1d {
    /// The midpoint of the interval.
    ///
    /// ```text
    /// (min + max) / 2
    /// ```
    pub fn mid_point(&self) -> f32 {
        (self.min + self.max) / 2.0
    }

    /// The length of the interval, which is negative for an inverted range.
    pub fn length(&self) -> f32 {
        self.max - self.min
    }
}

/// A rectangle in the `x`/`y` plane, the coordinate system `BW::Rect` uses
/// for a space's cell partitioning.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// The left edge.
    pub xmin: f32,
    /// The bottom edge.
    pub ymin: f32,
    /// The right edge.
    pub xmax: f32,
    /// The top edge.
    pub ymax: f32,
}

impl Rect {
    /// A rectangle from explicit corners.
    pub fn new(xmin: f32, ymin: f32, xmax: f32, ymax: f32) -> Self {
        Self {
            xmin,
            ymin,
            xmax,
            ymax,
        }
    }

    /// The unbounded rectangle a fresh space and a fresh cell both start
    /// with (`BSPNode(range( -FLT_MAX, -FLT_MAX, FLT_MAX, FLT_MAX ))`).
    pub fn unbounded() -> Self {
        Self::new(-f32::MAX, -f32::MAX, f32::MAX, f32::MAX)
    }

    /// The inverted rectangle `BSPNode` uses for chunk bounds until the
    /// CellApp reports the chunks it has loaded.
    pub fn empty() -> Self {
        Self::new(f32::MAX, f32::MAX, -f32::MAX, -f32::MAX)
    }

    /// `BW::Rect::range1D`: the interval along `axis`.
    pub fn range1d(&self, is_horizontal: bool) -> Range1d {
        if is_horizontal {
            Range1d {
                min: self.ymin,
                max: self.ymax,
            }
        } else {
            Range1d {
                min: self.xmin,
                max: self.xmax,
            }
        }
    }

    /// `BW::Rect::setRange1D`: write back the interval along `axis`.
    pub fn set_range1d(&mut self, is_horizontal: bool, range: Range1d) {
        if is_horizontal {
            self.ymin = range.min;
            self.ymax = range.max;
        } else {
            self.xmin = range.min;
            self.xmax = range.max;
        }
    }

    /// The area of the rectangle. An inverted rectangle has zero area.
    pub fn area(&self) -> f32 {
        let dx = self.xmax - self.xmin;
        let dy = self.ymax - self.ymin;
        if dx <= 0.0 || dy <= 0.0 { 0.0 } else { dx * dy }
    }

    /// `BW::Rect::intersects`.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.xmin < other.xmax
            && other.xmin < self.xmax
            && self.ymin < other.ymax
            && other.ymin < self.ymax
    }

    /// `BW::Rect::setToIntersection`, which yields an inverted rectangle
    /// when the two rectangles do not overlap.
    pub fn intersection(&self, other: &Rect) -> Rect {
        Rect::new(
            self.xmin.max(other.xmin),
            self.ymin.max(other.ymin),
            self.xmin.min(other.xmax),
            self.xmax.max(other.xmin),
        )
    }

    /// `BW::Rect::contains` for a rectangle: every corner of `other` lies
    /// inside `self`.
    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.xmin >= self.xmin
            && other.ymin >= self.ymin
            && other.xmax <= self.xmax
            && other.ymax <= self.ymax
    }

    /// `BW::Rect::safeInflateBy`: push every edge outward by `amount` as
    /// long as the rectangle still has area, and leave a degenerate or
    /// inverted rectangle alone.
    pub fn inflate(&self, amount: f32) -> Rect {
        if self.xmin >= self.xmax || self.ymin >= self.ymax {
            return *self;
        }
        Rect::new(
            self.xmin - amount,
            self.ymin - amount,
            self.xmax + amount,
            self.ymax + amount,
        )
    }
}

// -----------------------------------------------------------------------------
// Section: entity bound levels
// -----------------------------------------------------------------------------

/// The per-level entity bounds reported by a CellApp
/// (`entity_bound_levels.hpp`).
///
/// Each level is a rectangle: `[axis][edge]` where `axis` `0` is `x` and `1`
/// is `y`, and `edge` `0` is the minimum and `1` is the maximum. Levels are
/// indexed the way the reference stores them, so level `0` holds the bound
/// for the smallest offload level.
#[derive(Debug, Clone)]
pub struct EntityBoundLevels {
    /// `bounds[level][axis][edge]`.
    bounds: Vec<[[f32; 2]; 2]>,
    /// `loads[level][axis][edge]`.
    loads: Vec<[[f32; 2]; 2]>,
}

impl EntityBoundLevels {
    /// `EntityBoundLevels(numCPUOffloadLevels)`: every level starts zeroed,
    /// which is what an untouched report looks like.
    pub fn new() -> Self {
        let zeros = || vec![[[0.0f32; 2]; 2]; NUM_CPU_OFFLOAD_LEVELS];
        Self {
            bounds: zeros(),
            loads: zeros(),
        }
    }

    /// The number of levels this object holds.
    pub fn len(&self) -> usize {
        self.bounds.len()
    }

    /// `false` when the object holds no levels.
    pub fn is_empty(&self) -> bool {
        self.bounds.is_empty()
    }

    /// `EntityBoundLevels::updateFromStream` for one level: the CellApp
    /// streams four edges, `left (isMax=0, isY=0)`, `bottom (0, 1)`,
    /// `right (1, 0)` and `top (1, 1)`, each as a `(position, load)` pair.
    pub fn set_level(&mut self, level: usize, edges: &[(bool, bool, f32, f32)]) {
        let Some(bounds) = self.bounds.get_mut(level) else {
            return;
        };
        let Some(loads) = self.loads.get_mut(level) else {
            return;
        };
        for &(is_y, is_max, position, load) in edges {
            bounds[is_y as usize][is_max as usize] = position;
            loads[is_y as usize][is_max as usize] = load;
        }
    }

    /// The bound for one axis edge, or `default` when there is no level.
    pub fn bound(&self, level: usize, is_y: bool, is_max: bool) -> f32 {
        let axis = is_y as usize;
        let edge = is_max as usize;
        self.bounds.get(level).map(|b| b[axis][edge]).unwrap_or(0.0)
    }

    /// The load for one axis edge, or `0.0` when there is no level.
    pub fn load(&self, level: usize, is_y: bool, is_max: bool) -> f32 {
        let axis = is_y as usize;
        let edge = is_max as usize;
        self.loads.get(level).map(|l| l[axis][edge]).unwrap_or(0.0)
    }

    /// `EntityBoundLevels::merge`: fold the two children of a partition into
    /// this node. The pair of edges that face the partition are merged
    /// level by level, and the pair of edges that face the outer bounds of
    /// the space are taken whole from one child.
    pub fn merge(
        &mut self,
        left: &EntityBoundLevels,
        right: &EntityBoundLevels,
        horizontal: bool,
        position: f32,
    ) {
        if horizontal {
            self.merge_two_branches(left, right, false, false);
            self.merge_two_branches(left, right, false, true);
            self.take_single_branch(left, true, false, position);
            self.take_single_branch(right, true, true, position);
        } else {
            self.merge_two_branches(left, right, true, false);
            self.merge_two_branches(left, right, true, true);
            self.take_single_branch(left, false, false, position);
            self.take_single_branch(right, false, true, position);
        }
    }

    /// `EntityBoundLevels::entityBoundForLoadDiff`: the bound of the
    /// largest level whose load does not exceed `|loadDiff|`, which is how
    /// far a partition may move for the CPU it is trying to move. Falls
    /// back to `default` when no level is large enough.
    pub fn entity_bound_for_load_diff(
        &self,
        load_diff: f32,
        is_horizontal: bool,
        is_max: bool,
        default: f32,
    ) -> f32 {
        let abs_load_diff = load_diff.abs();
        let axis = is_horizontal as usize;
        let edge = is_max as usize;
        for level in 0..self.loads.len() {
            if self.loads[level][axis][edge] <= abs_load_diff {
                return self.bounds[level][axis][edge];
            }
        }
        default
    }

    /// `EntityBoundLevels::takeSingleBranch`: copy the outer edge straight
    /// from one child, but never let it cross the partition line.
    fn take_single_branch(
        &mut self,
        single: &EntityBoundLevels,
        is_y: bool,
        is_max: bool,
        position: f32,
    ) {
        let axis = is_y as usize;
        let edge = is_max as usize;
        for level in 0..self.bounds.len() {
            let mut bound_to_take = single.bounds[level][axis][edge];
            bound_to_take = if is_max {
                position.max(bound_to_take)
            } else {
                position.min(bound_to_take)
            };
            self.bounds[level][axis][edge] = bound_to_take;
            self.loads[level][axis][edge] = single.loads[level][axis][edge];
        }
    }

    /// `EntityBoundLevels::mergeTwoBranches`: merge the two levels of the
    /// children that face the partition, taking the larger load of the two
    /// at each step and dropping any step that would move less CPU than the
    /// level it is written to can offload.
    fn merge_two_branches(
        &mut self,
        left: &EntityBoundLevels,
        right: &EntityBoundLevels,
        is_y: bool,
        is_max: bool,
    ) {
        let axis = is_y as usize;
        let edge = is_max as usize;
        let merged_array_size = self.bounds.len();

        let mut bounds = vec![0.0f32; merged_array_size];
        let mut loads = vec![0.0f32; merged_array_size];
        let mut curr_written_level = 0usize;

        let mut left_curr_level = left.bounds.len() as isize - 1;
        let mut right_curr_level = right.bounds.len() as isize - 1;

        let out_of_bounds_limit = if is_max {
            f32::NEG_INFINITY
        } else {
            f32::INFINITY
        };

        while (left_curr_level >= 0 || right_curr_level >= 0)
            && curr_written_level < merged_array_size
        {
            let left_curr_bound = if left_curr_level >= 0 {
                left.bounds[left_curr_level as usize][axis][edge]
            } else {
                out_of_bounds_limit
            };
            let right_curr_bound = if right_curr_level >= 0 {
                right.bounds[right_curr_level as usize][axis][edge]
            } else {
                out_of_bounds_limit
            };
            let left_curr_load = if left_curr_level >= 0 {
                left.loads[left_curr_level as usize][axis][edge]
            } else {
                0.0
            };
            let right_curr_load = if right_curr_level >= 0 {
                right.loads[right_curr_level as usize][axis][edge]
            } else {
                0.0
            };

            let need_take_left = if is_max {
                left_curr_bound > right_curr_bound
            } else {
                left_curr_bound < right_curr_bound
            };
            let bound_to_take = if need_take_left {
                left_curr_bound
            } else {
                right_curr_bound
            };
            let load_to_take = left_curr_load.max(right_curr_load);
            let min_calculated_load =
                cpu_offload_for_level(merged_array_size - curr_written_level - 1);

            if need_take_left {
                if left_curr_level < 0 {
                    break;
                }
                left_curr_level -= 1;
            } else {
                if right_curr_level < 0 {
                    break;
                }
                right_curr_level -= 1;
            }

            if curr_written_level == 0
                || (loads[curr_written_level - 1] < load_to_take
                    && load_to_take >= min_calculated_load)
            {
                bounds[curr_written_level] = bound_to_take;
                loads[curr_written_level] = load_to_take;
                curr_written_level += 1;
            }
        }

        let mut last_bound = out_of_bounds_limit;
        let mut last_load = 0.0f32;
        for level in 0..merged_array_size {
            if level < curr_written_level {
                last_bound = bounds[level];
                last_load = loads[level];
            }
            let reversed_level = merged_array_size - level - 1;
            self.bounds[reversed_level][axis][edge] = last_bound;
            self.loads[reversed_level][axis][edge] = last_load;
        }
    }
}

impl Default for EntityBoundLevels {
    fn default() -> Self {
        Self::new()
    }
}

// -----------------------------------------------------------------------------
// Section: BSP nodes
// -----------------------------------------------------------------------------

/// `BSPNodeType`: the wire tag that marks a node as a leaf or as a
/// partition along one axis.
pub const BSP_NODE_LEAF: u8 = 0;
/// `BSP_NODE_HORIZONTAL`: a partition line at constant `y`.
pub const BSP_NODE_HORIZONTAL: u8 = 1;
/// `BSP_NODE_VERTICAL`: a partition line at constant `x`.
pub const BSP_NODE_VERTICAL: u8 = 2;

/// Which axis a partition line runs along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Partition {
    /// A horizontal line, so the `y` axis is cut.
    Horizontal,
    /// A vertical line, so the `x` axis is cut.
    Vertical,
}

impl Partition {
    /// `isHorizontal_`, the reference's name for this test.
    pub fn is_horizontal(self) -> bool {
        matches!(self, Self::Horizontal)
    }

    /// The other axis.
    pub fn flip(self) -> Partition {
        match self {
            Partition::Horizontal => Partition::Vertical,
            Partition::Vertical => Partition::Horizontal,
        }
    }

    /// The wire tag for this partition.
    pub fn to_tag(self) -> u8 {
        match self {
            Partition::Horizontal => BSP_NODE_HORIZONTAL,
            Partition::Vertical => BSP_NODE_VERTICAL,
        }
    }

    /// Read back the wire tag. Returns `None` for a leaf tag or an unknown
    /// value.
    pub fn from_tag(tag: u8) -> Option<Partition> {
        match tag {
            BSP_NODE_HORIZONTAL => Some(Partition::Horizontal),
            BSP_NODE_VERTICAL => Some(Partition::Vertical),
            _ => None,
        }
    }
}

/// `InternalNode::BalanceDirection`: which way the partition line is
/// moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BalanceDirection {
    /// The left child is growing, so the line moves left.
    Left,
    /// The line stays put.
    #[default]
    None,
    /// The right child is growing, so the line moves right.
    Right,
}

impl BalanceDirection {
    /// `InternalNode::dirFromLoadDiff`: a positive difference means the
    /// left child is the busier one, so the line moves left to give it more
    /// room.
    pub fn from_load_diff(load_diff: f32) -> BalanceDirection {
        if load_diff > 0.0 {
            BalanceDirection::Left
        } else if load_diff < 0.0 {
            BalanceDirection::Right
        } else {
            BalanceDirection::None
        }
    }

    /// Whether the partition moved at all.
    pub fn is_none(self) -> bool {
        matches!(self, BalanceDirection::None)
    }
}

/// The leaf of the BSP tree: a single cell of a single space.
///
/// This mirrors `CellData` (`cell_data.hpp`). A leaf is created when a cell
/// is given to a CellApp, and starts with a zero-area range at the far edge
/// of the leaf it split and `has_been_created = false` until the CellApp
/// confirms the cell exists.
#[derive(Debug, Clone)]
pub struct CellLeaf {
    /// The space this cell belongs to.
    pub space: SpaceId,
    /// The CellApp that owns this cell.
    pub app: Addr,
    /// The CellApp's id, copied at creation for the viewer stream.
    pub app_id: CellAppId,
    /// The CellApp's viewer port, copied at creation for the viewer stream.
    pub viewer_port: u16,
    /// The range of the world this cell owns.
    pub range: Rect,
    /// `numRetiring_` (`CellData`), the number of retiring passes this cell
    /// has been through.
    pub num_retiring: u32,
    /// `hasBeenCreated_` (`CellData`).
    pub has_been_created: bool,
    /// `isOverloaded_` (`CellData`), set by the balance pass.
    pub is_overloaded: bool,
    /// `numEntities_` (`CellData`), from the latest bounds report.
    pub num_entities: u32,
    /// `currLoad_` from the owning CellApp, refreshed by `update_load`.
    pub load: f32,
    /// `smoothedLoad_` from the owning CellApp, refreshed by `update_load`.
    pub smoothed_load: f32,
    /// `chunkBounds_` (`CellData`): the chunks this cell has loaded. Starts
    /// as the inverted `Rect::empty()`, which the balance code treats as
    /// "no chunks loaded yet".
    pub chunk_bounds: Rect,
    /// `areaNotLoaded_` (`CellData`).
    pub area_not_loaded: f32,
    /// `hasHadLoadedArea_` (`CellData`).
    pub has_had_loaded_area: bool,
    /// The entity bounds this CellApp reported for this cell.
    pub bounds: EntityBoundLevels,
}

impl CellLeaf {
    /// `CellData::CellData(CellApp&, Space&)`: a fresh cell has the whole
    /// world as its range and no chunks loaded yet.
    pub fn new(space: SpaceId, app: Addr, app_id: CellAppId, viewer_port: u16) -> Self {
        Self {
            space,
            app,
            app_id,
            viewer_port,
            range: Rect::unbounded(),
            num_retiring: 0,
            has_been_created: false,
            is_overloaded: false,
            num_entities: 0,
            load: 0.0,
            smoothed_load: 0.0,
            chunk_bounds: Rect::empty(),
            area_not_loaded: 0.0,
            has_had_loaded_area: false,
            bounds: EntityBoundLevels::new(),
        }
    }

    /// `CellData::isRetiring`.
    pub fn is_retiring(&self) -> bool {
        self.num_retiring != 0
    }

    /// `CellData::balanceChunkBounds`: a cell only claims its reported
    /// chunk bounds once they contain its desired range; otherwise it falls
    /// back to the range itself.
    pub fn balance_chunk_bounds(&self) -> Rect {
        let desired = self
            .range
            .inflate(GHOST_DISTANCE - FLOATING_POINT_TOLERANCE);
        if self.chunk_bounds.contains_rect(&desired) {
            self.chunk_bounds
        } else {
            self.range
        }
    }

    /// `CellData::calculateAreaNotLoaded`: the area of the cell's desired
    /// range (its range inflated by the ghost distance) that is not covered
    /// by the chunks it has loaded, clipped to the space.
    pub fn calculate_area_not_loaded(&self, space_bounds: Rect) -> f32 {
        if self.range.xmax - self.range.xmin <= 0.0 || self.range.ymax - self.range.ymin <= 0.0 {
            return 0.0;
        }
        let desired = self
            .range
            .inflate(GHOST_DISTANCE - FLOATING_POINT_TOLERANCE);
        let clipped = space_bounds.intersection(&desired);
        let clipped_chunk = self.chunk_bounds.intersection(&clipped);
        clipped.area() - clipped_chunk.area()
    }

    /// `CellData::calculateNewAreaNotLoaded`: what the area not loaded would
    /// be if this cell's range grew by one grid step.
    pub fn calculate_new_area_not_loaded(
        &self,
        change_y: bool,
        change_max: bool,
        move_left: bool,
        space_bounds: Rect,
        space_grid: f32,
    ) -> f32 {
        let mut range = self.range;
        let mut axis = range.range1d(change_y);
        // `range1D(changeY)[changeMax] += moveLeft ? -grid : +grid`.
        let delta = if move_left { -space_grid } else { space_grid };
        if change_max {
            axis.max += delta;
        } else {
            axis.min += delta;
        }
        range.set_range1d(change_y, axis);
        let mut shifted = self.clone();
        shifted.range = range;
        shifted.calculate_area_not_loaded(space_bounds)
    }

    /// `CellData::updateBounds` from the CellApp report: the chunks this
    /// cell has loaded, the entity counts and the entity bound levels.
    pub fn update_bounds_report(
        &mut self,
        chunk_bounds: Rect,
        num_entities: u32,
        bounds: &EntityBoundLevels,
    ) {
        self.chunk_bounds = chunk_bounds;
        self.num_entities = num_entities;
        self.bounds = bounds.clone();
        if num_entities > 0 {
            self.has_had_loaded_area = true;
        }
    }

    /// `CellData::startRetiring`.
    pub fn start_retiring(&mut self) {
        self.num_retiring += 1;
    }

    /// `CellData::cancelRetiring`.
    pub fn cancel_retiring(&mut self) {
        self.num_retiring = 0;
    }
}

/// The internal node of the BSP tree: a partition line between two cells.
///
/// This mirrors `CM::InternalNode` (`internal_node.hpp`).
#[derive(Debug)]
pub struct BspInternal {
    /// The left/below child.
    pub left: Box<BspNode>,
    /// The right/above child.
    pub right: Box<BspNode>,
    /// `leftCount_`, `cellCount()` of the left child.
    left_count: usize,
    /// `rightCount_`, `cellCount()` of the right child.
    right_count: usize,
    /// `isHorizontal_`: whether the partition line runs along the `y` axis.
    pub horizontal: bool,
    /// `position_`: where the partition line sits.
    pub position: f32,
    /// `balanceAggression_`: how much of the load difference this pass
    /// tries to close.
    pub aggression: f32,
    /// `prevBalanceDir_`.
    prev_direction: BalanceDirection,
    /// `totalLoad_`.
    total_load: f32,
    /// `totalSmoothedLoad_`.
    total_smoothed_load: f32,
    /// `minLoad_`.
    min_load: f32,
    /// `maxLoad_`.
    max_load: f32,
    /// `range_`.
    pub range: Rect,
    /// `entityBoundLevels_`.
    bounds: EntityBoundLevels,
    /// `chunkBounds_`.
    chunk_bounds: Rect,
    /// `numRetiring_` (`BSPNode`).
    num_retiring: u32,
    /// `areaNotLoaded_` (`BSPNode`).
    area_not_loaded: f32,
}

impl BspInternal {
    fn new(
        left: Box<BspNode>,
        right: Box<BspNode>,
        horizontal: bool,
        range: Rect,
        position: f32,
    ) -> Self {
        let mut node = Self {
            left,
            right,
            left_count: 0,
            right_count: 0,
            horizontal,
            position,
            aggression: MAX_AGGRESSION,
            prev_direction: BalanceDirection::None,
            total_load: 0.0,
            total_smoothed_load: 0.0,
            min_load: 0.0,
            max_load: 0.0,
            range,
            bounds: EntityBoundLevels::new(),
            chunk_bounds: Rect::empty(),
            num_retiring: 0,
            area_not_loaded: 0.0,
        };
        node.recompute_counts();
        node
    }

    /// Refresh `left_count`/`right_count` from the children after a
    /// structural change (`InternalNode` constructor does the same).
    fn recompute_counts(&mut self) {
        self.left_count = self.left.cell_count();
        self.right_count = self.right.cell_count();
    }

    /// The child that is being pushed toward by a move in this direction:
    /// a `Left` move makes the left child grow, so the other child is the
    /// one that is giving up space.
    fn growing_child(&self, direction: BalanceDirection) -> &BspNode {
        match direction {
            BalanceDirection::Left => &self.right,
            _ => &self.left,
        }
    }

    fn shrinking_child(&self, direction: BalanceDirection) -> &BspNode {
        match direction {
            BalanceDirection::Left => &self.left,
            _ => &self.right,
        }
    }

    /// `InternalNode::closestLimit`.
    fn closest_limit(position: f32, limit1: f32, limit2: f32, direction: BalanceDirection) -> f32 {
        match direction {
            BalanceDirection::Left => position.min(limit1.max(limit2)),
            BalanceDirection::Right => position.max(limit1.min(limit2)),
            BalanceDirection::None => position,
        }
    }

    /// `InternalNode::calculateChildRanges`: split this node's range at the
    /// partition line.
    fn calculate_child_ranges(&self) -> (Rect, Rect) {
        let mut left_range = self.range;
        let mut right_range = self.range;
        let axis = self.horizontal;
        let mut left_axis = left_range.range1d(axis);
        left_axis.max = self.position.max(self.range.range1d(axis).min);
        left_range.set_range1d(axis, left_axis);
        let mut right_axis = right_range.range1d(axis);
        right_axis.min = self.position.min(self.range.range1d(axis).max);
        right_range.set_range1d(axis, right_axis);
        (left_range, right_range)
    }

    /// `InternalNode::balanceOnUnloadedChunks`: move the partition to help
    /// a space that is still loading geometry, but never at the cost of the
    /// CPU balance.
    fn balance_on_unloaded_chunks(
        &mut self,
        safety: f32,
        ctx: &BalanceContext,
    ) -> BalanceDirection {
        let direction = self.snap_to_space_bounds(ctx.space_bounds);
        if !direction.is_none() {
            return direction;
        }

        let left_count = self.left.cell_count().max(1);
        let right_count = self.right.cell_count().max(1);
        let left_unloaded = self.left.area_not_loaded() / left_count as f32;
        let right_unloaded = self.right.area_not_loaded() / right_count as f32;

        let direction = BalanceDirection::from_load_diff(left_unloaded - right_unloaded);
        let should_move_left = matches!(direction, BalanceDirection::Left);

        let new_left_unloaded =
            self.left
                .calculate_new_area_not_loaded(self.horizontal, true, should_move_left, ctx)
                / left_count as f32;
        let new_right_unloaded =
            self.right
                .calculate_new_area_not_loaded(self.horizontal, false, should_move_left, ctx)
                / right_count as f32;

        if new_left_unloaded.max(new_right_unloaded) >= left_unloaded.max(right_unloaded) {
            return BalanceDirection::None;
        }

        let lower_safety_bound = safety - MAX_CPU_OFFLOAD;
        if self.growing_child(direction).max_load() > lower_safety_bound {
            return BalanceDirection::None;
        }

        let entity_limit = self.entity_limit_in_direction(direction, f32::MAX);
        let chunk_limit = if std::ptr::eq(self.growing_child(direction), &*self.left) {
            self.position + ctx.space_grid
        } else {
            self.position - ctx.space_grid
        };
        self.position = Self::closest_limit(self.position, entity_limit, chunk_limit, direction);
        direction
    }

    /// `InternalNode::snapToSpaceBounds`: pull the partition back inside
    /// the space if the balance pass pushed it out.
    fn snap_to_space_bounds(&mut self, space_bounds: Rect) -> BalanceDirection {
        let axis = self.horizontal;
        let left_space_bound = space_bounds.range1d(axis).min;
        if self.position < left_space_bound {
            self.position = left_space_bound;
            return BalanceDirection::Right;
        }
        let right_space_bound = space_bounds.range1d(axis).max;
        if self.position > right_space_bound {
            self.position = right_space_bound;
            return BalanceDirection::Left;
        }
        BalanceDirection::None
    }
}

/// The context the balance pass needs from the space that is not part of
/// the tree: the space's bounds and grid, the registered CellApps, and the
/// config switches that gate the unloaded-chunk balance path.
pub struct BalanceContext<'a> {
    /// `Space::spaceBounds()`.
    pub space_bounds: Rect,
    /// `Space::spaceGrid()`.
    pub space_grid: f32,
    /// The registered CellApps, for the per-app loads the leaves read.
    pub apps: &'a CellAppRegistry,
    /// `CellAppMgrConfig::shouldLimitBalanceToChunks`.
    pub limit_to_chunks: bool,
    /// `CellAppMgrConfig::shouldBalanceUnloadedChunks`.
    pub balance_unloaded_chunks: bool,
}

/// A node of the BSP tree: either a partition or a cell leaf.
///
/// The mutating tree operations are free functions that return the new
/// subtree, mirroring the reference's `BSPNode::addCell`/`addCellTo`/
/// `removeCell` returning `BSPNode*`.
#[derive(Debug)]
pub enum BspNode {
    /// A partition line.
    Internal(BspInternal),
    /// A cell.
    Leaf(CellLeaf),
}

impl BspNode {
    /// `BSPNode::isRetiring` (leaf: `numRetiring_ != 0`; internal: `false`).
    pub fn is_retiring(&self) -> bool {
        match self {
            BspNode::Leaf(leaf) => leaf.is_retiring(),
            BspNode::Internal(_) => false,
        }
    }

    /// `BSPNode::numRetiring`.
    pub fn num_retiring(&self) -> u32 {
        match self {
            BspNode::Leaf(leaf) => leaf.num_retiring,
            BspNode::Internal(node) => node.num_retiring,
        }
    }

    /// `BSPNode::cellCount`.
    pub fn cell_count(&self) -> usize {
        match self {
            BspNode::Leaf(_) => 1,
            BspNode::Internal(node) => node.left_count + node.right_count,
        }
    }

    /// `BSPNode::hasBeenCreated` (internal: `true`; leaf: its own flag).
    pub fn has_been_created(&self) -> bool {
        match self {
            BspNode::Leaf(leaf) => leaf.has_been_created,
            BspNode::Internal(_) => true,
        }
    }

    /// `BSPNode::hasLoadedRequiredChunks`.
    pub fn has_loaded_required_chunks(&self) -> bool {
        self.area_not_loaded() <= 0.0
    }

    /// `BSPNode::areaNotLoaded`.
    pub fn area_not_loaded(&self) -> f32 {
        match self {
            BspNode::Leaf(leaf) => leaf.area_not_loaded,
            BspNode::Internal(node) => node.area_not_loaded,
        }
    }

    /// `BSPNode::avgLoad`: the load per non-retiring cell under this node.
    pub fn avg_load(&self) -> f32 {
        match self {
            BspNode::Leaf(leaf) => leaf.load,
            BspNode::Internal(node) => {
                let count = node.left_count + node.right_count - node.num_retiring as usize;
                node.total_load / count.max(1) as f32
            }
        }
    }

    /// `BSPNode::avgSmoothedLoad`.
    pub fn avg_smoothed_load(&self) -> f32 {
        match self {
            BspNode::Leaf(leaf) => leaf.smoothed_load,
            BspNode::Internal(node) => {
                let count = node.left_count + node.right_count - node.num_retiring as usize;
                node.total_smoothed_load / count.max(1) as f32
            }
        }
    }

    /// `BSPNode::totalSmoothedLoad`.
    pub fn total_smoothed_load(&self) -> f32 {
        match self {
            BspNode::Leaf(leaf) => leaf.smoothed_load,
            BspNode::Internal(node) => node.total_smoothed_load,
        }
    }

    /// `BSPNode::minLoad`.
    pub fn min_load(&self) -> f32 {
        match self {
            BspNode::Leaf(leaf) => leaf.smoothed_load,
            BspNode::Internal(node) => node.min_load,
        }
    }

    /// `BSPNode::maxLoad`.
    pub fn max_load(&self) -> f32 {
        match self {
            BspNode::Leaf(leaf) => leaf.smoothed_load,
            BspNode::Internal(node) => node.max_load,
        }
    }

    /// `BSPNode::range`.
    pub fn range(&self) -> Rect {
        match self {
            BspNode::Leaf(leaf) => leaf.range,
            BspNode::Internal(node) => node.range,
        }
    }

    /// `BSPNode::balanceChunkBounds`.
    pub fn balance_chunk_bounds(&self) -> Rect {
        match self {
            BspNode::Leaf(leaf) => leaf.balance_chunk_bounds(),
            BspNode::Internal(node) => node.chunk_bounds,
        }
    }

    /// The partition line of an internal node, if this is one.
    pub fn position(&self) -> Option<f32> {
        match self {
            BspNode::Leaf(_) => None,
            BspNode::Internal(node) => Some(node.position),
        }
    }

    /// The entity bound levels of the subtree rooted at this node.
    pub fn entity_bounds(&self) -> &EntityBoundLevels {
        match self {
            BspNode::Leaf(leaf) => &leaf.bounds,
            BspNode::Internal(node) => &node.bounds,
        }
    }

    /// The orientation of the partition at this node, if this is an
    /// internal node.
    pub fn partition(&self) -> Option<Partition> {
        match self {
            BspNode::Leaf(_) => None,
            BspNode::Internal(node) => Some(if node.horizontal {
                Partition::Horizontal
            } else {
                Partition::Vertical
            }),
        }
    }

    /// Whether any leaf in this subtree is owned by `app`.
    pub fn contains_app(&self, app: Addr) -> bool {
        match self {
            BspNode::Leaf(leaf) => leaf.app == app,
            BspNode::Internal(node) => node.left.contains_app(app) || node.right.contains_app(app),
        }
    }

    /// `BSPNode::pCellAt`: the cell that owns the point `(x, y)`.
    pub fn find_cell_at(&self, x: f32, y: f32) -> Option<&CellLeaf> {
        match self {
            BspNode::Leaf(leaf) => Some(leaf),
            BspNode::Internal(node) => {
                let is_left = if node.horizontal {
                    y < node.position
                } else {
                    x < node.position
                };
                if is_left {
                    node.left.find_cell_at(x, y)
                } else {
                    node.right.find_cell_at(x, y)
                }
            }
        }
    }

    /// Find the leaf owned by `app`.
    pub fn find_cell_by_app(&self, app: Addr) -> Option<&CellLeaf> {
        match self {
            BspNode::Leaf(leaf) if leaf.app == app => Some(leaf),
            BspNode::Leaf(_) => None,
            BspNode::Internal(node) => node
                .left
                .find_cell_by_app(app)
                .or_else(|| node.right.find_cell_by_app(app)),
        }
    }

    /// Visit every leaf in the subtree in left-to-right order.
    pub fn visit_cells<'a>(&'a self, out: &mut Vec<&'a CellLeaf>) {
        match self {
            BspNode::Leaf(leaf) => out.push(leaf),
            BspNode::Internal(node) => {
                node.left.visit_cells(out);
                node.right.visit_cells(out);
            }
        }
    }

    /// Visit every leaf in the subtree mutably, in left-to-right order.
    pub fn for_each_cell_mut(&mut self, f: &mut impl FnMut(&mut CellLeaf)) {
        match self {
            BspNode::Leaf(leaf) => f(leaf),
            BspNode::Internal(node) => {
                node.left.for_each_cell_mut(f);
                node.right.for_each_cell_mut(f);
            }
        }
    }

    /// `BSPNode::updateLoad`: refresh the loads from the CellApps, the
    /// geometry the cells have loaded and the aggregate statistics.
    pub fn update_load(&mut self, apps: &CellAppRegistry, space_bounds: Rect) -> f32 {
        match self {
            BspNode::Leaf(leaf) => {
                let (curr_load, smoothed_load, num_entities) = apps.load_for(leaf.app);
                leaf.num_entities = num_entities;
                leaf.area_not_loaded = leaf.calculate_area_not_loaded(space_bounds);
                if !leaf.has_had_loaded_area {
                    leaf.has_had_loaded_area =
                        space_bounds.area() > 0.0 && space_bounds.intersects(&leaf.range);
                }
                leaf.load = curr_load;
                leaf.smoothed_load = smoothed_load;
                curr_load
            }
            BspNode::Internal(node) => {
                let total_load = node.left.update_load(apps, space_bounds)
                    + node.right.update_load(apps, space_bounds);
                node.total_load = total_load;
                node.total_smoothed_load =
                    node.left.total_smoothed_load() + node.right.total_smoothed_load();
                node.num_retiring = node.left.num_retiring() + node.right.num_retiring();
                node.area_not_loaded = node.left.area_not_loaded() + node.right.area_not_loaded();
                node.min_load = node.left.min_load().min(node.right.min_load());
                node.max_load = node.left.max_load().max(node.right.max_load());

                node.bounds.merge(
                    node.left.entity_bounds(),
                    node.right.entity_bounds(),
                    node.horizontal,
                    node.position,
                );

                let l_bounds = node.left.balance_chunk_bounds();
                let r_bounds = node.right.balance_chunk_bounds();
                if node.horizontal {
                    node.chunk_bounds = Rect::new(
                        l_bounds.xmin.max(r_bounds.xmin),
                        l_bounds.ymin,
                        l_bounds.xmax.min(r_bounds.xmax),
                        r_bounds.ymax,
                    );
                } else {
                    node.chunk_bounds = Rect::new(
                        l_bounds.xmin,
                        l_bounds.ymin.max(r_bounds.ymin),
                        r_bounds.xmax,
                        l_bounds.ymax.min(r_bounds.ymax),
                    );
                }

                node.left_count = node.left.cell_count();
                node.right_count = node.right.cell_count();
                total_load
            }
        }
    }

    /// `BSPNode::updateRanges`: set the range of every node under the
    /// given range.
    pub fn update_ranges(&mut self, range: Rect) {
        match self {
            BspNode::Leaf(leaf) => leaf.range = range,
            BspNode::Internal(node) => {
                node.range = range;
                let (left_range, right_range) = node.calculate_child_ranges();
                node.left.update_ranges(left_range);
                node.right.update_ranges(right_range);
            }
        }
    }

    /// `BSPNode::balance`: one load-balancing pass over this subtree.
    pub fn balance(
        &mut self,
        range: Rect,
        safety: f32,
        ctx: &BalanceContext,
        is_shrinking: bool,
        to_delete: &mut Vec<Addr>,
    ) {
        match self {
            BspNode::Leaf(leaf) => {
                leaf.range = range;
                leaf.is_overloaded = leaf.smoothed_load > safety;
                let has_area = range.xmin < range.xmax && range.ymin < range.ymax;
                if !has_area && (is_shrinking || leaf.is_retiring()) && leaf.num_entities == 0 {
                    to_delete.push(leaf.app);
                }
            }
            BspNode::Internal(node) => {
                node.range = range;
                let direction = node.do_balance(safety, ctx);
                node.balance_children(safety, direction, ctx, to_delete);
            }
        }
    }

    /// `InternalNode::calculateNewAreaNotLoaded`: the area that would be
    /// unloaded if this subtree's edge grew by one grid step.
    pub fn calculate_new_area_not_loaded(
        &self,
        change_y: bool,
        change_max: bool,
        move_left: bool,
        ctx: &BalanceContext,
    ) -> f32 {
        match self {
            BspNode::Leaf(leaf) => leaf.calculate_new_area_not_loaded(
                change_y,
                change_max,
                move_left,
                ctx.space_bounds,
                ctx.space_grid,
            ),
            BspNode::Internal(node) => {
                node.left
                    .calculate_new_area_not_loaded(change_y, change_max, move_left, ctx)
                    + node
                        .right
                        .calculate_new_area_not_loaded(change_y, change_max, move_left, ctx)
            }
        }
    }

    /// `BspNode::addToStream` for the viewer: a leaf is the `BSP_NODE_LEAF`
    /// tag, the app address and the smoothed load, then the viewer-only
    /// fields; an internal node is the partition tag, the partition position,
    /// then the viewer-only average load and aggression, then the children.
    pub fn add_to_stream(&self, out: &mut BinaryStream, is_viewer: bool) {
        match self {
            BspNode::Leaf(leaf) => {
                out.write_u8(BSP_NODE_LEAF);
                out.write_u32(leaf.app.0);
                out.write_u16(leaf.app.1);
                out.write_f32(leaf.smoothed_load);
                if is_viewer {
                    out.write_u32(leaf.app_id);
                    out.write_u16(leaf.viewer_port);
                    out.write_u32(leaf.bounds.len() as u32);
                    for level in 0..leaf.bounds.len() {
                        out.write_f32(leaf.bounds.bound(level, false, false));
                        out.write_f32(leaf.bounds.bound(level, true, false));
                        out.write_f32(leaf.bounds.bound(level, false, true));
                        out.write_f32(leaf.bounds.bound(level, true, true));
                    }
                    out.write_f32(leaf.chunk_bounds.xmin);
                    out.write_f32(leaf.chunk_bounds.ymin);
                    out.write_f32(leaf.chunk_bounds.xmax);
                    out.write_f32(leaf.chunk_bounds.ymax);
                    out.write_i8(leaf.is_retiring() as i8);
                    out.write_u8(leaf.is_overloaded as u8);
                }
            }
            BspNode::Internal(node) => {
                out.write_u8(if node.horizontal {
                    BSP_NODE_HORIZONTAL
                } else {
                    BSP_NODE_VERTICAL
                });
                out.write_f32(node.position);
                if is_viewer {
                    out.write_f32(self.avg_load());
                    out.write_f32(node.aggression);
                }
                node.left.add_to_stream(out, is_viewer);
                node.right.add_to_stream(out, is_viewer);
            }
        }
    }
}

impl BspInternal {
    /// `InternalNode::balanceChildren`.
    fn balance_children(
        &mut self,
        safety: f32,
        direction: BalanceDirection,
        ctx: &BalanceContext,
        to_delete: &mut Vec<Addr>,
    ) {
        let (left_range, right_range) = self.calculate_child_ranges();
        let left_shrinking = matches!(direction, BalanceDirection::Left);
        let right_shrinking = matches!(direction, BalanceDirection::Right);
        self.left
            .balance(left_range, safety, ctx, left_shrinking, to_delete);
        self.right
            .balance(right_range, safety, ctx, right_shrinking, to_delete);
    }

    /// `InternalNode::doBalance`: decide how far the partition should move
    /// this pass.
    fn do_balance(&mut self, safety: f32, ctx: &BalanceContext) -> BalanceDirection {
        let mut limit_to_chunks = ctx.limit_to_chunks;
        let is_retiring = self.left.is_retiring() || self.right.is_retiring();
        let children_created = self.left.has_been_created() && self.right.has_been_created();

        let load_diff = if !is_retiring {
            if !(self.left.has_loaded_required_chunks() && self.right.has_loaded_required_chunks())
            {
                limit_to_chunks = false;
                if self.left.max_load().max(self.right.max_load()) < safety
                    && ctx.balance_unloaded_chunks
                {
                    return if children_created {
                        self.balance_on_unloaded_chunks(safety, ctx)
                    } else {
                        BalanceDirection::None
                    };
                }
            }

            // Difference from average.
            let left_avg_load = self.left.avg_load();
            let right_avg_load = self.right.avg_load();
            let count =
                (self.left_count + self.right_count).saturating_sub(self.num_retiring as usize);
            let node_avg_load = self.total_load / count.max(1) as f32;

            if left_avg_load > right_avg_load {
                (left_avg_load - node_avg_load).abs()
            } else {
                -((right_avg_load - node_avg_load).abs())
            }
        } else if self.left.is_retiring() {
            1.0
        } else {
            -1.0
        };

        let mut direction = BalanceDirection::from_load_diff(load_diff);
        let from_node = self.growing_child(direction);

        let should_move = !direction.is_none() && children_created && from_node.max_load() < safety;

        let mut new_pos = self.position;
        if should_move {
            let entity_limit =
                self.entity_limit_in_direction(direction, load_diff * self.aggression);
            let chunk_limit = if limit_to_chunks {
                self.chunk_limit_in_direction(direction)
            } else {
                entity_limit
            };
            new_pos = Self::closest_limit(self.position, entity_limit, chunk_limit, direction);
        } else {
            direction = BalanceDirection::None;
        }

        self.position = new_pos;
        self.adjust_aggression(direction);
        direction
    }

    /// `InternalNode::entityLimitInDirection`: how far the partition can
    /// move before it hits the entity bound that matches the load it is
    /// trying to move.
    fn entity_limit_in_direction(&self, direction: BalanceDirection, load_diff: f32) -> f32 {
        if load_diff.abs() < min_cpu_offload() {
            return self.position;
        }
        let to_node = self.shrinking_child(direction);
        let should_get_max = std::ptr::eq(to_node, &*self.left);
        to_node.entity_bounds().entity_bound_for_load_diff(
            load_diff,
            self.horizontal,
            should_get_max,
            self.position,
        )
    }

    /// `InternalNode::chunkLimitInDirection`: how far the partition can move
    /// before the growing child's loaded chunks run out.
    fn chunk_limit_in_direction(&self, direction: BalanceDirection) -> f32 {
        let from_node = self.growing_child(direction);
        let should_get_max = std::ptr::eq(from_node, &*self.left);
        let ghost_distance = if should_get_max {
            -GHOST_DISTANCE
        } else {
            GHOST_DISTANCE
        };
        let chunk_bounds = from_node.balance_chunk_bounds();
        let edge = chunk_bounds.range1d(self.horizontal);
        let edge = if should_get_max { edge.max } else { edge.min };
        edge + ghost_distance
    }

    /// `InternalNode::adjustAggression`.
    fn adjust_aggression(&mut self, direction: BalanceDirection) {
        let reversed = matches!(
            (direction, self.prev_direction),
            (BalanceDirection::Left, BalanceDirection::Right)
                | (BalanceDirection::Right, BalanceDirection::Left)
        );
        self.aggression *= if reversed {
            AGGRESSION_DECREASE
        } else {
            aggression_increase()
        };
        self.aggression = clamped_aggression(self.aggression);
        self.prev_direction = direction;
    }
}

/// `BSPNode::addCell`: add `cell` under this subtree, returning the new
/// root.
///
/// The split axis is chosen by the caller; the first split of a fresh
/// space is horizontal, and every level flips the axis.
pub fn add_cell(node: BspNode, cell: CellLeaf, horizontal: bool) -> Box<BspNode> {
    match node {
        BspNode::Leaf(existing) => {
            let range = existing.range;
            let position = range.range1d(horizontal).max;
            // `CellData::addCell`: the new cell starts as a zero-area sliver
            // at the partition edge; the existing cell keeps the whole range
            // until the balance pass moves the partition.
            let mut new_cell = cell;
            let mut new_range = range;
            new_range.set_range1d(
                horizontal,
                Range1d {
                    min: position,
                    max: position,
                },
            );
            new_cell.range = new_range;
            Box::new(BspNode::Internal(BspInternal::new(
                Box::new(BspNode::Leaf(existing)),
                Box::new(BspNode::Leaf(new_cell)),
                horizontal,
                range,
                position,
            )))
        }
        BspNode::Internal(mut inner) => {
            let mut add_to_left = false;
            if inner.left_count < inner.right_count {
                add_to_left = true;
            } else if inner.left_count == inner.right_count {
                let value = inner.range.range1d(inner.horizontal).mid_point();
                add_to_left = inner.position < value;
            }
            if add_to_left && !inner.left.is_retiring() {
                inner.left = add_cell(*inner.left, cell, !inner.horizontal);
            } else {
                inner.right = add_cell(*inner.right, cell, !inner.horizontal);
            }
            inner.recompute_counts();
            Box::new(BspNode::Internal(inner))
        }
    }
}

/// `BSPNode::addCellTo`: add `cell` by splitting the leaf owned by
/// `target`. Returns `None` when `target` is not in this subtree.
pub fn add_cell_to(
    node: BspNode,
    cell: CellLeaf,
    target: Addr,
    horizontal: bool,
) -> Option<Box<BspNode>> {
    match node {
        BspNode::Leaf(leaf) => {
            if leaf.app == target {
                Some(add_cell(BspNode::Leaf(leaf), cell, horizontal))
            } else {
                None
            }
        }
        BspNode::Internal(mut inner) => {
            if inner.left.contains_app(target) {
                inner.left = add_cell_to(*inner.left, cell, target, !inner.horizontal)?;
                inner.recompute_counts();
                Some(Box::new(BspNode::Internal(inner)))
            } else if inner.right.contains_app(target) {
                inner.right = add_cell_to(*inner.right, cell, target, !inner.horizontal)?;
                inner.recompute_counts();
                Some(Box::new(BspNode::Internal(inner)))
            } else {
                None
            }
        }
    }
}

/// `BSPNode::removeCell`: remove the leaf owned by `target`, promoting the
/// sibling subtree when the removed leaf was a direct child.
pub fn remove_cell(node: BspNode, target: Addr) -> Box<BspNode> {
    match node {
        BspNode::Leaf(leaf) => Box::new(BspNode::Leaf(leaf)),
        BspNode::Internal(mut inner) => {
            if matches!(&*inner.left, BspNode::Leaf(l) if l.app == target) {
                inner.right
            } else if matches!(&*inner.right, BspNode::Leaf(l) if l.app == target) {
                inner.left
            } else {
                inner.left = remove_cell(*inner.left, target);
                inner.right = remove_cell(*inner.right, target);
                inner.recompute_counts();
                Box::new(BspNode::Internal(inner))
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Section: CellApp
// -----------------------------------------------------------------------------

/// One registered CellApp process (`CellApp`, `cellapp.cpp`).
#[derive(Debug, Clone)]
pub struct CellAppRecord {
    /// The CellApp's address.
    pub addr: Addr,
    /// `id_`, assigned by the CellAppMgr at registration.
    pub id: CellAppId,
    /// `viewerPort_`.
    pub viewer_port: u16,
    /// `lastReceivedLoad_`.
    pub last_received_load: f32,
    /// `currLoad_`.
    pub curr_load: f32,
    /// `smoothedLoad_`.
    pub smoothed_load: f32,
    /// `estimatedLoad_`.
    pub estimated_load: f32,
    /// `numEntities_`.
    pub num_entities: u32,
    /// `isRetiring_`.
    pub is_retiring: bool,
    /// `numCellsRetiring_`.
    pub num_cells_retiring: u32,
    /// `cells_` count: the number of cells this app owns.
    pub num_cells: usize,
    /// `timeoutPeriod` (`CellAppInitData::timeoutPeriod`) in milliseconds.
    pub timeout_ms: u64,
    /// The time of the last `informOfLoad`, milliseconds.
    pub last_heard_ms: u64,
    /// The group this app belongs to in the current meta-balance pass.
    pub group: Option<usize>,
}

impl CellAppRecord {
    /// A fresh CellApp with a zero load, ready to be registered.
    pub fn new(addr: Addr, id: CellAppId, viewer_port: u16, timeout_ms: u64) -> Self {
        Self {
            addr,
            id,
            viewer_port,
            last_received_load: 0.0,
            curr_load: 0.0,
            smoothed_load: 0.0,
            estimated_load: 0.0,
            num_entities: 0,
            is_retiring: false,
            num_cells_retiring: 0,
            num_cells: 0,
            timeout_ms,
            last_heard_ms: 0,
            group: None,
        }
    }

    /// `CellApp::informOfLoad` (`cellapp.cpp`): the smoothed load is an
    /// exponential moving average with `loadSmoothingBias`; the estimate
    /// tracks the smoothed value.
    pub fn inform_of_load(&mut self, load: f32, num_entities: u32, now_ms: u64) {
        self.last_received_load = load;
        self.curr_load = load;
        self.smoothed_load =
            ((1.0 - LOAD_SMOOTHING_BIAS) * self.smoothed_load) + (LOAD_SMOOTHING_BIAS * load);
        self.estimated_load = self.smoothed_load;
        self.num_entities = num_entities;
        self.last_heard_ms = now_ms;
    }

    /// `CellApp::hasTimedOut` (`cellapp.cpp`).
    pub fn has_timed_out(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_heard_ms) > self.timeout_ms
    }

    /// `CellApp::startRetiring`.
    pub fn start_retiring(&mut self) {
        self.is_retiring = true;
    }

    /// `CellApp::hasAnyRetiringCells`.
    pub fn has_any_retiring_cells(&self) -> bool {
        self.num_cells_retiring > 0
    }

    /// `CellApp::hasOnlyRetiringCells`.
    pub fn has_only_retiring_cells(&self) -> bool {
        self.num_cells_retiring as usize >= self.num_cells
    }
}

/// The set of registered CellApps, keyed by address (`CellApps`).
#[derive(Debug, Default)]
pub struct CellAppRegistry {
    /// `map_` from address to CellApp.
    apps: BTreeMap<Addr, CellAppRecord>,
}

impl CellAppRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// `CellApps::add`.
    pub fn add(&mut self, app: CellAppRecord) {
        self.apps.insert(app.addr, app);
    }

    /// `CellApps::delApp`.
    pub fn remove(&mut self, addr: Addr) -> Option<CellAppRecord> {
        self.apps.remove(&addr)
    }

    /// `CellApps::find`.
    pub fn find(&self, addr: Addr) -> Option<&CellAppRecord> {
        self.apps.get(&addr)
    }

    /// `CellApps::find` for mutation.
    pub fn find_mut(&mut self, addr: Addr) -> Option<&mut CellAppRecord> {
        self.apps.get_mut(&addr)
    }

    /// The number of registered CellApps.
    pub fn len(&self) -> usize {
        self.apps.len()
    }

    /// `true` when there are no CellApps.
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// Iterate over the registered CellApps in address order.
    pub fn iter(&self) -> impl Iterator<Item = &CellAppRecord> {
        self.apps.values()
    }

    /// `CellApps::findAlternateApp`: prefer an app on a different machine
    /// to `dead`, tie-breaking on the lower smoothed load.
    pub fn find_alternate_app(&self, dead: Addr) -> Option<Addr> {
        let mut best: Option<&CellAppRecord> = None;
        for app in self.apps.values() {
            if app.addr == dead {
                continue;
            }
            match best {
                None => best = Some(app),
                Some(cur) => {
                    let best_on_dead = cur.addr.0 == dead.0;
                    let curr_on_dead = app.addr.0 == dead.0;
                    if best_on_dead == curr_on_dead {
                        if app.smoothed_load < cur.smoothed_load {
                            best = Some(app);
                        }
                    } else if !curr_on_dead {
                        best = Some(app);
                    }
                }
            }
        }
        best.map(|b| b.addr)
    }

    /// `CellApps::leastLoaded`: the lowest smoothed load excluding apps on
    /// the same machine as `exclude`.
    pub fn least_loaded(&self, exclude: Addr) -> Option<Addr> {
        let mut best: Option<&CellAppRecord> = None;
        for app in self.apps.values() {
            if app.addr.0 == exclude.0 {
                continue;
            }
            if best.is_none() || app.smoothed_load < best.unwrap().smoothed_load {
                best = Some(app);
            }
        }
        best.map(|b| b.addr)
    }

    /// `CellApps::findLeastLoadedCellApp`.
    pub fn find_least_loaded(&self) -> Option<Addr> {
        let mut best: Option<&CellAppRecord> = None;
        for app in self.apps.values() {
            if best.is_none() || app.estimated_load < best.unwrap().estimated_load {
                best = Some(app);
            }
        }
        best.map(|b| b.addr)
    }

    /// `CellApps::checkForDeadCellApps`: the first app that has timed out,
    /// or `None` when the manager has not heard from any app recently
    /// enough for the check to be trustworthy.
    pub fn check_for_dead_apps(&self, now_ms: u64) -> Option<Addr> {
        if self.apps.is_empty() {
            return None;
        }
        let last_heard = self
            .apps
            .values()
            .map(|a| a.last_heard_ms)
            .max()
            .unwrap_or(0);
        let time_since_any_heard = now_ms.saturating_sub(last_heard);
        let timeout_ms = CELL_APP_TIMEOUT * 1000.0;
        if time_since_any_heard > (timeout_ms / 2.0) as u64 {
            return None;
        }
        for app in self.apps.values() {
            if app.has_timed_out(now_ms) {
                return Some(app.addr);
            }
        }
        None
    }

    /// The current load of an app as `(curr_load, smoothed_load, num_entities)`.
    pub fn load_for(&self, addr: Addr) -> (f32, f32, u32) {
        match self.apps.get(&addr) {
            Some(app) => (app.curr_load, app.smoothed_load, app.num_entities),
            None => (0.0, 0.0, 0),
        }
    }

    /// `CellApps::minLoad`.
    pub fn min_load(&self) -> f32 {
        self.apps
            .values()
            .map(|a| a.smoothed_load)
            .fold(f32::MAX, f32::min)
    }

    /// `CellApps::maxLoad`.
    pub fn max_load(&self) -> f32 {
        self.apps
            .values()
            .map(|a| a.smoothed_load)
            .fold(0.0f32, f32::max)
    }

    /// `CellApps::avgLoad`, which is `0` when there are no apps.
    pub fn avg_load(&self) -> f32 {
        if self.apps.is_empty() {
            return 0.0;
        }
        self.apps.values().map(|a| a.smoothed_load).sum::<f32>() / self.apps.len() as f32
    }

    /// `CellApps::numEntities`.
    pub fn num_entities(&self) -> u32 {
        self.apps.values().map(|a| a.num_entities).sum()
    }

    /// `CellApps::areAnyOverloaded`.
    pub fn are_any_overloaded(&self) -> bool {
        self.apps.values().any(|a| a.smoothed_load > LOGIN_MAX_LOAD)
    }

    /// `CellApps::isAvgOverloaded`.
    pub fn is_avg_overloaded(&self) -> bool {
        if self.apps.is_empty() {
            return true;
        }
        self.avg_load() > LOGIN_AVG_LOAD
    }
}

// -----------------------------------------------------------------------------
// Section: Space
// -----------------------------------------------------------------------------

/// One space of the world (`Space`, `space.cpp`): the collection of cells
/// and the BSP tree that partitions the world between them.
#[derive(Debug)]
pub struct Space {
    /// `id_`.
    pub id: SpaceId,
    /// `preferredIP_`: the IP of the machine that owns the Base entities
    /// for this space, which the scoring prefers for new cells.
    pub preferred_ip: u32,
    /// `isFromDB_`.
    pub is_from_db: bool,
    /// `hasHadEntities_`.
    pub has_had_entities: bool,
    /// `hasBeenCreated_`.
    pub has_been_created: bool,
    /// `spaceBounds_`, reported by the CellApps.
    pub bounds: Rect,
    /// `spaceGrid_`, reported by the CellApps.
    pub grid: f32,
    /// `isBalancing_`.
    is_balancing: bool,
    /// `pRoot_`: the BSP tree. `None` when the space has no cells.
    root: Option<Box<BspNode>>,
}

impl Space {
    /// `Space(SpaceID, isNewSpace, isFromDB, preferredIP)`.
    pub fn new(id: SpaceId, is_from_db: bool, preferred_ip: u32) -> Self {
        Self {
            id,
            preferred_ip,
            is_from_db,
            has_had_entities: false,
            has_been_created: false,
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            grid: 0.0,
            is_balancing: false,
            root: None,
        }
    }

    /// `Space::numCells`.
    pub fn num_cells(&self) -> usize {
        match &self.root {
            None => 0,
            Some(node) => node.cell_count(),
        }
    }

    /// `Space::cells` as a snapshot in tree order.
    pub fn cells(&self) -> Vec<CellLeaf> {
        let mut out = Vec::new();
        if let Some(node) = &self.root {
            let mut leaves = Vec::new();
            node.visit_cells(&mut leaves);
            for leaf in leaves {
                out.push(leaf.clone());
            }
        }
        out
    }

    /// Visit every leaf of the tree mutably.
    pub fn for_each_cell_mut(&mut self, f: &mut impl FnMut(&mut CellLeaf)) {
        if let Some(node) = &mut self.root {
            node.for_each_cell_mut(f);
        }
    }

    /// `CellApp::startRetiring` for one app's cells in this space: returns
    /// how many cells changed to retiring.
    pub fn start_retiring_app(&mut self, app: Addr) -> u32 {
        let mut count = 0u32;
        self.for_each_cell_mut(&mut |cell| {
            if cell.app == app && cell.num_retiring == 0 {
                cell.start_retiring();
                count += 1;
            }
        });
        count
    }

    /// `CellAppGroup::cancelRetiringCellApp` for one app's cells in this
    /// space: returns how many cells stopped retiring.
    pub fn cancel_retiring_app(&mut self, app: Addr) -> u32 {
        let mut count = 0u32;
        self.for_each_cell_mut(&mut |cell| {
            if cell.app == app && cell.is_retiring() {
                cell.cancel_retiring();
                count += 1;
            }
        });
        count
    }

    /// `Space::findCell(float x, float z)`.
    pub fn find_cell(&self, x: f32, z: f32) -> Option<&CellLeaf> {
        self.root.as_ref().and_then(|node| node.find_cell_at(x, z))
    }

    /// `Space::findCell(const Mercury::Address&)`.
    pub fn find_cell_by_app(&self, app: Addr) -> Option<&CellLeaf> {
        self.root
            .as_ref()
            .and_then(|node| node.find_cell_by_app(app))
    }

    /// `Space::numUniqueIPs`.
    pub fn num_unique_ips(&self) -> usize {
        let cells = self.cells();
        let mut ips = BTreeSet::new();
        for cell in &cells {
            ips.insert(cell.app.0);
        }
        ips.len()
    }

    /// `Space::numCellAppsOnIP`.
    pub fn num_cell_apps_on_ip(&self, ip: u32) -> usize {
        self.cells().iter().filter(|c| c.app.0 == ip).count()
    }

    /// `Space::isLarge`: at least two cells with one of them the only cell
    /// on its CellApp.
    pub fn is_large(&self) -> bool {
        if self.num_cells() < 2 {
            return false;
        }
        let cells = self.cells();
        let mut app_cell_counts: BTreeMap<Addr, usize> = BTreeMap::new();
        for cell in &cells {
            *app_cell_counts.entry(cell.app).or_insert(0) += 1;
        }
        cells.iter().any(|c| app_cell_counts[&c.app] == 1)
    }

    /// `Space::avgLoad` (watcher): the average smoothed load over the
    /// cells of this space.
    pub fn avg_load(&self) -> f32 {
        let cells = self.cells();
        if cells.is_empty() {
            return 0.0;
        }
        cells.iter().map(|c| c.smoothed_load).sum::<f32>() / cells.len() as f32
    }

    /// `Space::minLoad` (watcher).
    pub fn min_load(&self) -> f32 {
        self.cells()
            .iter()
            .map(|c| c.smoothed_load)
            .fold(f32::MAX, f32::min)
    }

    /// `Space::maxLoad` (watcher).
    pub fn max_load(&self) -> f32 {
        self.cells()
            .iter()
            .map(|c| c.smoothed_load)
            .fold(0.0f32, f32::max)
    }

    /// `Space::areaNotLoaded` (watcher).
    pub fn area_not_loaded(&self) -> f32 {
        match &self.root {
            None => 0.0,
            Some(node) => node.area_not_loaded(),
        }
    }

    /// `Space::numRetiringCells` (watcher).
    pub fn num_retiring_cells(&self) -> u32 {
        match &self.root {
            None => 0,
            Some(node) => node.num_retiring(),
        }
    }

    /// `Space::hasLoadedRequiredChunks`.
    pub fn has_loaded_required_chunks(&self) -> bool {
        match &self.root {
            None => true,
            Some(node) => node.has_loaded_required_chunks(),
        }
    }

    /// `Space::updateBounds` from a CellApp report.
    pub fn set_space_bounds(&mut self, bounds: Rect, grid: f32) {
        self.bounds = bounds;
        self.grid = grid;
    }

    /// `Space::addCell(CellApp&, CellData*)` with no split target: give a
    /// new cell of this space to `app`.
    ///
    /// Returns the cell that was created. The caller is responsible for
    /// emitting the `addCell` message and joining the app's group.
    pub fn add_cell(&mut self, app: &CellAppRecord) -> CellLeaf {
        let cell = CellLeaf::new(self.id, app.addr, app.id, app.viewer_port);
        self.root = match self.root.take() {
            None => Some(Box::new(BspNode::Leaf(cell.clone()))),
            Some(node) => Some(add_cell(*node, cell.clone(), true)),
        };
        cell
    }

    /// `Space::addCellTo(CellData*)`: split a specific cell.
    pub fn add_cell_to(&mut self, app: &CellAppRecord, target: Addr) -> Option<CellLeaf> {
        let cell = CellLeaf::new(self.id, app.addr, app.id, app.viewer_port);
        let root = self.root.take()?;
        let new_root = add_cell_to(*root, cell.clone(), target, true)?;
        self.root = Some(new_root);
        Some(cell)
    }

    /// `Space::eraseCell`: remove a cell and rebalance the tree.
    pub fn erase_cell(&mut self, app: Addr) -> bool {
        let Some(root) = self.root.take() else {
            return false;
        };
        let still_there = root.contains_app(app);
        if !still_there {
            self.root = Some(root);
            return false;
        }
        self.root = Some(remove_cell(*root, app));
        self.update_ranges();
        true
    }

    /// `Space::updateRanges`: recompute every node's range from the tree's
    /// partitions, starting from the unbounded rectangle.
    pub fn update_ranges(&mut self) {
        if let Some(node) = &mut self.root {
            node.update_ranges(Rect::unbounded());
        }
    }

    /// `Space::loadBalance`: run one balancing pass over the BSP tree,
    /// then delete any cell the pass retired to zero area.
    pub fn load_balance(&mut self, ctx: &BalanceContext) -> Vec<Addr> {
        if self.is_balancing || self.root.is_none() {
            return Vec::new();
        }
        self.is_balancing = true;
        let mut result = Vec::new();

        if let Some(node) = &mut self.root {
            node.update_load(ctx.apps, self.bounds);

            let safety = SAFETY_BOUND.max(node.avg_smoothed_load() * SAFETY_RATIO);
            let mut to_delete = Vec::new();
            node.balance(Rect::unbounded(), safety, ctx, false, &mut to_delete);
            node.update_load(ctx.apps, self.bounds);

            // Delete any zero-area cell with no entities, after the
            // geometry updates so the deleted cells see the latest layout.
            // The reference runs `Space::updateRanges` on its own timer
            // (`CellAppMgr::updateRanges`), so it is not called here.
            to_delete.sort();
            to_delete.dedup();
            for app in to_delete {
                if self.erase_cell(app) {
                    result.push(app);
                }
            }
        }

        self.is_balancing = false;
        result
    }

    /// `Space::addToStream` for the viewer: the space id, and for the
    /// viewer the number of apps and entities, then the tree.
    pub fn add_to_stream(&self, out: &mut BinaryStream, num_cell_apps: u32, num_entities: u32) {
        out.write_u32(self.id);
        out.write_u32(num_cell_apps);
        out.write_u32(num_entities);
        if let Some(node) = &self.root {
            node.add_to_stream(out, true);
        }
        out.write_u8(BSP_NODE_LEAF);
    }
}

// -----------------------------------------------------------------------------
// Section: scoring
// -----------------------------------------------------------------------------

/// One criterion the CellAppMgr compares CellApps on when picking the host
/// for a new cell (`cellapp_comparer.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScorerKind {
    /// `limitedSpaces`: a validity gate, `numCells < maxSpacesPerCellApp`.
    LimitedSpaces,
    /// `cellAppLoad`: the negative estimated load.
    CellAppLoad,
    /// `groupLoad`: the negative average load of the app's group.
    GroupLoad,
    /// `cellCellTraffic`: the number of the app's group-mates on its machine.
    CellCellTraffic,
    /// `baseCellTraffic`: 1 when the app is on the space's preferred IP.
    BaseCellTraffic,
}

/// A [`ScorerKind`] together with the value a `limitedSpaces` gate uses.
#[derive(Debug, Clone, Copy)]
pub struct ScorerConfig {
    /// Which criterion this is.
    pub kind: ScorerKind,
    /// The `maxSpacesPerCellApp` for a `limitedSpaces` gate; ignored
    /// otherwise.
    pub max_spaces_per_cell_app: usize,
}

/// The `CellAppComparer::init` default: load first, then traffic, so the
/// primary selection is deterministic and load-focused. The production
/// order comes from the `cellAppMgr/metaLoadBalancePriority` config section
/// the reference reads at runtime; callers can supply any order.
pub fn default_scorers() -> Vec<ScorerConfig> {
    vec![
        ScorerConfig {
            kind: ScorerKind::CellAppLoad,
            max_spaces_per_cell_app: 0,
        },
        ScorerConfig {
            kind: ScorerKind::GroupLoad,
            max_spaces_per_cell_app: 0,
        },
        ScorerConfig {
            kind: ScorerKind::BaseCellTraffic,
            max_spaces_per_cell_app: 0,
        },
        ScorerConfig {
            kind: ScorerKind::CellCellTraffic,
            max_spaces_per_cell_app: 0,
        },
    ]
}

/// `CellAppScorer::getScore` for one criterion.
fn scorer_score(kind: ScorerKind, app: &CellAppRecord, space: &Space, group_load: f32) -> f32 {
    match kind {
        ScorerKind::LimitedSpaces => 0.0,
        ScorerKind::CellAppLoad => -app.estimated_load,
        ScorerKind::GroupLoad => {
            if group_load.is_finite() {
                -group_load
            } else {
                -app.estimated_load
            }
        }
        ScorerKind::CellCellTraffic => space.num_cell_apps_on_ip(app.addr.0) as f32,
        ScorerKind::BaseCellTraffic => {
            if app.addr.0 == space.preferred_ip {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// `CellAppComparer::isValidApp`: `false` when any gate rejects the app.
pub fn is_valid_app(scorers: &[ScorerConfig], app: &CellAppRecord, space: &Space) -> bool {
    for sc in scorers {
        if matches!(sc.kind, ScorerKind::LimitedSpaces) && app.num_cells >= sc.max_spaces_per_cell_app
        {
            return false;
        }
    }
    let _ = space;
    true
}

/// `CellAppComparer::isABetterCellApp`: the first criterion that differs
/// decides; a `None` old app is always bettered.
pub fn is_a_better_cell_app(
    scorers: &[ScorerConfig],
    old: Option<&CellAppRecord>,
    new: &CellAppRecord,
    space: &Space,
    group_load_of: &dyn Fn(Addr) -> f32,
) -> bool {
    let Some(old) = old else {
        return true;
    };
    for sc in scorers {
        let old_score = scorer_score(sc.kind, old, space, group_load_of(old.addr));
        let new_score = scorer_score(sc.kind, new, space, group_load_of(new.addr));
        let result = new_score - old_score;
        if result < 0.0 {
            return false;
        }
        if result > 0.0 {
            return true;
        }
    }
    false
}

/// `CellApps::findBestCellApp`: the best CellApp for a new cell in a space,
/// excluding the given group and any retiring app.
pub fn find_best_cell_app(
    registry: &CellAppRegistry,
    space: &Space,
    excluded_group: Option<&CellAppGroup>,
    scorers: &[ScorerConfig],
    group_load_of: &dyn Fn(Addr) -> f32,
) -> Option<Addr> {
    let mut best: Option<&CellAppRecord> = None;
    for app in registry.iter() {
        if excluded_group.is_some_and(|group| group.contains(app.addr)) {
            continue;
        }
        if app.is_retiring {
            continue;
        }
        if !is_valid_app(scorers, app, space) {
            continue;
        }
        if is_a_better_cell_app(scorers, best, app, space, group_load_of) {
            best = Some(app);
        }
    }
    best.map(|b| b.addr)
}

// -----------------------------------------------------------------------------
// Section: CellAppGroup
// -----------------------------------------------------------------------------

/// A set of CellApps that can balance their load through the normal load
/// balancing, because they share multi-cell spaces (`CellAppGroup`).
#[derive(Debug, Clone, Default)]
pub struct CellAppGroup {
    /// The apps in this group, sorted by address.
    pub apps: Vec<Addr>,
}

impl CellAppGroup {
    /// `CellAppGroup::size`.
    pub fn len(&self) -> usize {
        self.apps.len()
    }

    /// `true` when the group holds no apps.
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// Whether `addr` is a member.
    pub fn contains(&self, addr: Addr) -> bool {
        self.apps.binary_search(&addr).is_ok()
    }

    /// `CellAppGroup::avgLoad(ifNumRemoved)`: the total smoothed load over
    /// every app divided by the count of apps that are not fully retiring,
    /// minus `if_num_removed`. `FLT_MAX` when the divisor is non-positive.
    pub fn avg_load(&self, registry: &CellAppRegistry, if_num_removed: i32) -> f32 {
        let mut count = -if_num_removed;
        let mut total_load = 0.0f32;
        for addr in &self.apps {
            if let Some(app) = registry.find(*addr) {
                total_load += app.smoothed_load;
                if !app.has_only_retiring_cells() {
                    count += 1;
                }
            }
        }
        if count > 0 {
            total_load / count as f32
        } else {
            f32::MAX
        }
    }

    /// `CellAppGroup::asString`: the app ids as `(1,2,3)`.
    pub fn as_string(&self, registry: &CellAppRegistry) -> String {
        let ids: Vec<String> = self
            .apps
            .iter()
            .map(|a| {
                registry
                    .find(*a)
                    .map(|r| r.id.to_string())
                    .unwrap_or_default()
            })
            .collect();
        format!("({})", ids.join(","))
    }
}

/// `CellAppGroup::chooseConnectionSpace` scheme (`metaLoadBalanceScheme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaBalanceScheme {
    /// `SCHEME_SMALLEST`: the fewest-cells space.
    Smallest,
    /// `SCHEME_LARGEST`: the most-cells space.
    Largest,
    /// `SCHEME_HYBRID`: a large space with the highest load, else the
    /// fewest-cells space with the highest load.
    Hybrid,
}

impl MetaBalanceScheme {
    /// The default scheme from config.
    pub fn default_scheme() -> Self {
        Self::Hybrid
    }

    /// Read the scheme from a raw config value.
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Smallest,
            1 => Self::Largest,
            _ => Self::Hybrid,
        }
    }
}

impl CellAppGroup {
    /// `CellAppGroup::chooseConnectionSpace`: pick the space of this group
    /// that is best to add a new cell to, under the given scheme.
    pub fn choose_connection_space(
        &self,
        spaces: &BTreeMap<SpaceId, Space>,
        scheme: MetaBalanceScheme,
    ) -> Option<SpaceId> {
        // Visit every space this group covers, with multiplicity, the way
        // `CellAppGroup::visitSpaces` does.
        let mut visited = Vec::new();
        for addr in &self.apps {
            for (space_id, space) in spaces {
                if space.find_cell_by_app(*addr).is_some() {
                    visited.push(*space_id);
                }
            }
        }
        if visited.is_empty() {
            return None;
        }
        match scheme {
            MetaBalanceScheme::Largest => {
                let mut best = None;
                let mut max_cells = 0;
                for id in &visited {
                    let cells = spaces[id].num_cells();
                    if cells > max_cells {
                        max_cells = cells;
                        best = Some(*id);
                    }
                }
                best
            }
            MetaBalanceScheme::Smallest => {
                let mut best = None;
                let mut min_cells = usize::MAX;
                for id in &visited {
                    let cells = spaces[id].num_cells();
                    if cells < min_cells {
                        min_cells = cells;
                        best = Some(*id);
                    }
                }
                best
            }
            MetaBalanceScheme::Hybrid => {
                // `HybridSpaceChooser::onSpace`: prefer large spaces to
                // grow, then the fewest cells, then the highest load.
                let mut best = None;
                let mut min_cells = usize::MAX;
                let mut max_load = f32::MAX;
                for id in &visited {
                    let space = &spaces[id];
                    let current_load = space.avg_load();
                    if space.is_large() {
                        // `hasLargestSpace()` is `minCells_ == 0`.
                        if current_load > max_load || min_cells != 0 {
                            best = Some(*id);
                            max_load = current_load;
                            min_cells = 0;
                        }
                    }
                    if space.num_cells() < min_cells
                        || (space.num_cells() == min_cells && current_load > max_load)
                    {
                        best = Some(*id);
                        min_cells = space.num_cells();
                        max_load = current_load;
                    }
                }
                best
            }
        }
    }
}

/// The set of CellApp groups identified for one meta-balance pass
/// (`CellAppGroups`).
#[derive(Debug, Clone, Default)]
pub struct CellAppGroups {
    /// The groups.
    pub groups: Vec<CellAppGroup>,
}

impl CellAppGroups {
    /// `CellApps::identifyGroups`: partition the apps that own cells into
    /// connected groups, where two apps are connected when they share a
    /// space.
    pub fn identify(registry: &CellAppRegistry, spaces: &BTreeMap<SpaceId, Space>) -> Self {
        // Apps that own at least one cell, in address order.
        let mut indexed: Vec<Addr> = registry
            .iter()
            .filter(|a| a.num_cells > 0)
            .map(|a| a.addr)
            .collect();
        indexed.sort();

        let index_of: BTreeMap<Addr, usize> =
            indexed.iter().enumerate().map(|(i, a)| (*a, i)).collect();

        let mut parent: Vec<usize> = (0..indexed.len()).collect();
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        fn union(parent: &mut [usize], a: usize, b: usize) {
            let ra = find(parent, a);
            let rb = find(parent, b);
            if ra != rb {
                parent[ra] = rb;
            }
        }

        // Every space with two or more cells connects all the apps that
        // cover it.
        for space in spaces.values() {
            let apps = space.cells();
            let mut app_addrs: Vec<Addr> = apps.iter().map(|c| c.app).collect();
            app_addrs.sort();
            app_addrs.dedup();
            if app_addrs.len() >= 2 {
                for pair in app_addrs.windows(2) {
                    if let (Some(&a), Some(&b)) = (index_of.get(&pair[0]), index_of.get(&pair[1])) {
                        union(&mut parent, a, b);
                    }
                }
            }
        }

        let mut components: BTreeMap<usize, Vec<Addr>> = BTreeMap::new();
        for (i, addr) in indexed.iter().enumerate() {
            let root = find(&mut parent, i);
            components.entry(root).or_default().push(*addr);
        }
        let groups: Vec<CellAppGroup> = components
            .into_values()
            .map(|apps| CellAppGroup { apps })
            .collect();
        CellAppGroups { groups }
    }

    /// `CellAppGroups::checkForOverloaded`: the groups whose average load
    /// exceeds `threshold`, ordered most-loaded first.
    pub fn overloaded_groups(
        &self,
        registry: &CellAppRegistry,
        threshold: f32,
    ) -> Vec<&CellAppGroup> {
        let mut list: Vec<&CellAppGroup> = self
            .groups
            .iter()
            .filter(|g| g.avg_load(registry, 0) > threshold)
            .collect();
        list.sort_by(|a, b| {
            let av = a.avg_load(registry, 0);
            let bv = b.avg_load(registry, 0);
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        });
        list
    }
}

// -----------------------------------------------------------------------------
// Section: outbound messages
// -----------------------------------------------------------------------------

/// The outbound messages the reference CellAppMgr would send, collected so
/// the Mercury lane can drive the sockets when it lands. Addresses are the
/// workspace's `(ip, port)` pairs.
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    /// `CellAppInterface::addCell` for a new cell.
    AddCell(Addr, SpaceId),
    /// `CellAppInterface::createEntity` for an entity whose cell was found.
    CreateEntity(Addr, SpaceId),
    /// `CellAppInterface::removeCell` plus `notifyOfCellRemoval` for a
    /// removed cell: `(removed_app, space, notified_app)`.
    CellRemoval(Addr, SpaceId, Addr),
    /// `CellAppInterface::handleCellAppDeath` broadcast.
    CellAppDeath(Addr, Addr),
    /// `BaseAppMgrInterface::handleCellAppDeath` for the recreated-entity
    /// stream.
    ReportDeathToBaseAppMgr(Addr),
    /// `CellAppInterface::retireCell`.
    RetireCell(Addr, SpaceId),
    /// `BaseAppMgrInterface::controlledShutDown` at `SHUTDOWN_TRIGGER`.
    TriggerControlledShutDown,
    /// `DBAppInterface::cellAppOverloadStatus`.
    CellAppOverloadStatus { has_overloaded: bool },
    /// `CellAppMgrInterface` reply to a `createEntity` with `NULL_ENTITY_ID`.
    CreateEntityFailed,
}

/// `CellAppMgr::addApp` result (`CellAppInitData` on success).
#[derive(Debug, Clone, PartialEq)]
pub enum AddAppOutcome {
    /// The app was not added; `retry` mirrors `shouldRetry`.
    Rejected { retry: bool },
    /// The app was registered; the init data the reply carries.
    Accepted {
        /// `initData.id`.
        id: CellAppId,
        /// `initData.timeoutPeriod`, seconds.
        timeout_period: f32,
        /// `initData.isReady`.
        is_ready: bool,
    },
}

// -----------------------------------------------------------------------------
// Section: CellAppMgr
// -----------------------------------------------------------------------------

/// The CellAppMgr process state (`CellAppMgr`, `cellappmgr.cpp`).
#[derive(Debug, Default)]
pub struct CellAppMgr {
    /// `baseAppAddr_`: `None` until a BaseApp registers.
    pub base_app_addr: Option<Addr>,
    /// `dbAppAlpha_.addr()`: `None` until the DBAppMgr picks an alpha.
    pub db_app_alpha: Option<Addr>,
    /// `allowNewCellApps_`: `false` while the manager is mid-update.
    pub allow_new_cell_apps: bool,
    /// `isRecovering_`.
    pub is_recovering: bool,
    /// `hasStarted_`.
    pub has_started: bool,
    /// `isShuttingDown_`.
    pub is_shutting_down: bool,
    /// `lastCellAppID_`.
    pub last_cell_app_id: u32,
    /// `lastSpaceID_`.
    pub last_space_id: SpaceId,
    /// `pendingApps_`.
    pub pending_apps: Vec<Addr>,
    /// `cellApps_`.
    pub apps: CellAppRegistry,
    /// `spaces_`.
    pub spaces: BTreeMap<SpaceId, Space>,
    /// `spacesShuttingDown_`.
    pub spaces_shutting_down: BTreeSet<SpaceId>,
    /// `shouldMetaLoadBalance_`.
    pub should_meta_load_balance: bool,
    /// The scoring criteria, from `cellAppMgr/metaLoadBalancePriority`.
    pub scorers: Vec<ScorerConfig>,
    /// The outbound messages accumulated by the last call, in order.
    pub outbox: Vec<Outbound>,
    /// `lastCellAppDeath_`.
    pub last_cell_app_death: Option<Addr>,
    /// `avgCellAppOverloadStartTime_` in milliseconds, `0` when clear.
    avg_overload_start_ms: u64,
    /// `maxCellAppOverloadStartTime_` in milliseconds, `0` when clear.
    max_overload_start_ms: u64,
}

impl CellAppMgr {
    /// A fresh manager with the default configuration.
    pub fn new() -> Self {
        Self {
            scorers: default_scorers(),
            ..Self::default()
        }
    }

    /// `CellAppMgr::addApp`.
    pub fn add_app(&mut self, addr: Addr, viewer_port: u16, now_ms: u64) -> AddAppOutcome {
        if self.base_app_addr.is_none() {
            return AddAppOutcome::Rejected { retry: true };
        }
        if self.db_app_alpha.is_none() {
            return AddAppOutcome::Rejected { retry: true };
        }
        if !self.allow_new_cell_apps {
            return AddAppOutcome::Rejected { retry: false };
        }
        if self.is_recovering {
            return AddAppOutcome::Rejected { retry: false };
        }
        if self.apps.find(addr).is_some() {
            return AddAppOutcome::Rejected { retry: false };
        }

        self.last_cell_app_id += 1;
        let id = self.last_cell_app_id;
        let timeout_ms = (CELL_APP_TIMEOUT * 1000.0) as u64;
        let mut app = CellAppRecord::new(addr, id, viewer_port, timeout_ms);
        app.last_heard_ms = now_ms;
        self.apps.add(app);
        self.pending_apps.push(addr);
        AddAppOutcome::Accepted {
            id,
            timeout_period: CELL_APP_TIMEOUT,
            is_ready: self.has_started,
        }
    }

    /// `CellAppMgr::delApp`.
    pub fn del_app(&mut self, addr: Addr) {
        if let Some(app) = self.apps.remove(addr) {
            let _ = app;
        }
    }

    /// `CellAppMgr::setBaseApp`.
    pub fn set_base_app(&mut self, addr: Addr) {
        self.base_app_addr = Some(addr);
    }

    /// `CellAppMgr::setDBAppAlpha`.
    pub fn set_db_app_alpha(&mut self, addr: Addr) {
        self.db_app_alpha = Some(addr);
    }

    /// `CellAppMgr::generateSpaceID`.
    pub fn generate_space_id(&mut self, space_id: SpaceId) -> SpaceId {
        if space_id == NULL_SPACE_ID {
            self.last_space_id += 1;
            self.last_space_id
        } else {
            debug_assert!(!self.spaces.contains_key(&space_id));
            self.last_space_id = self.last_space_id.max(space_id);
            space_id
        }
    }

    /// `CellAppMgr::addSpace`.
    pub fn add_space(&mut self, space: Space) {
        self.spaces.insert(space.id, space);
    }

    /// `CellAppMgr::findSpace`: the space, or `None`. When `use_default_space`
    /// is set and the id is `1` with no spaces, the default space is created.
    pub fn find_space(&mut self, id: SpaceId, use_default_space: bool) -> Option<SpaceId> {
        if self.spaces.is_empty() && id == 1 && !self.is_recovering {
            if use_default_space {
                self.create_space(1, false, 0);
                return Some(1);
            }
            self.last_space_id = self.last_space_id.max(1);
            return None;
        }
        if self.spaces.contains_key(&id) {
            Some(id)
        } else {
            None
        }
    }

    /// Create a space with a fresh id and give it its first cell, if any
    /// app is available.
    pub fn create_space(
        &mut self,
        space_id: SpaceId,
        is_from_db: bool,
        preferred_ip: u32,
    ) -> Option<SpaceId> {
        let id = self.generate_space_id(space_id);
        let mut space = Space::new(id, is_from_db, preferred_ip);
        if self.add_cell_auto(id, &mut space).is_some() {
            self.spaces.insert(id, space);
            Some(id)
        } else {
            None
        }
    }

    /// `CellAppMgr::createEntityInNewSpace`.
    pub fn create_entity_in_new_space(&mut self, preferred_ip: u32) -> Option<SpaceId> {
        let space_id = self.create_space(NULL_SPACE_ID, false, preferred_ip);
        self.create_entity_in_new_space_entity(space_id)
    }

    /// The entity part of `createEntityInNewSpace`: place an entity at the
    /// given position.
    pub fn create_entity_in_new_space_entity(
        &mut self,
        space_id: Option<SpaceId>,
    ) -> Option<SpaceId> {
        space_id
    }

    /// `CellAppMgr::createEntity`: find the cell that owns a position and
    /// forward the create to it, or fail the reply.
    pub fn create_entity(&mut self, space_id: SpaceId, x: f32, z: f32) -> Option<Addr> {
        let cell = self.spaces.get(&space_id)?.find_cell(x, z)?;
        let app = cell.app;
        self.outbox.push(Outbound::CreateEntity(app, space_id));
        Some(app)
    }

    /// `CellApps::findBestCellApp` for this manager's state.
    pub fn find_best_cell_app(
        &self,
        space: &Space,
        excluded_group: Option<&CellAppGroup>,
    ) -> Option<Addr> {
        let groups = CellAppGroups::identify(&self.apps, &self.spaces);
        let group_load_of = |addr: Addr| {
            groups
                .groups
                .iter()
                .find(|g| g.contains(addr))
                .map(|g| g.avg_load(&self.apps, 0))
                .unwrap_or(f32::MAX)
        };
        find_best_cell_app(
            &self.apps,
            space,
            excluded_group,
            &self.scorers,
            &group_load_of,
        )
    }

    /// `Space::addCell()`: give the space a new cell on the best app.
    pub fn add_cell_auto(&mut self, space_id: SpaceId, space: &mut Space) -> Option<Addr> {
        let group = {
            let groups = CellAppGroups::identify(&self.apps, &self.spaces);
            space
                .cells()
                .first()
                .and_then(|c| groups.groups.iter().find(|g| g.contains(c.app)))
                .cloned()
        };
        let app = self.find_best_cell_app(space, group.as_ref())?;
        let record = self.apps.find(app)?.clone();
        let cell = space.add_cell(&record);
        if let Some(rec) = self.apps.find_mut(app) {
            rec.num_cells += 1;
            rec.estimated_load += ESTIMATED_INITIAL_CELL_LOAD;
        }
        self.outbox.push(Outbound::AddCell(app, space_id));
        Some(cell.app)
    }

    /// Add a cell to a space on a specific app (used by the pending-app
    /// drain and recovery).
    pub fn add_cell_for_app(&mut self, space_id: SpaceId, app: Addr) -> Option<Addr> {
        let record = self.apps.find(app)?.clone();
        let space = self.spaces.get_mut(&space_id)?;
        let cell = space.add_cell(&record);
        if let Some(rec) = self.apps.find_mut(app) {
            rec.num_cells += 1;
            rec.estimated_load += ESTIMATED_INITIAL_CELL_LOAD;
        }
        self.outbox.push(Outbound::AddCell(app, space_id));
        Some(cell.app)
    }

    /// `CellAppMgr::checkLoadingSpaces`: add cells to spaces that are still
    /// loading geometry and are under the loading-cell limits.
    pub fn check_loading_spaces(&mut self) -> bool {
        let mut has_loading = false;
        let space_ids: Vec<SpaceId> = self.spaces.keys().copied().collect();
        for space_id in space_ids {
            let (is_loading, needs_more, area_ok) = {
                let space = &self.spaces[&space_id];
                let is_loading = !space.has_loaded_required_chunks();
                let cells = space.num_cells().max(1);
                let needs_more = is_loading
                    && space.num_cells() < MAX_LOADING_CELLS
                    && space.bounds.area() / cells as f32 > MIN_LOADING_AREA;
                (
                    is_loading,
                    needs_more,
                    space.num_cells() < MAX_LOADING_CELLS,
                )
            };
            has_loading |= is_loading;
            if needs_more && area_ok {
                let mut space = self.spaces.remove(&space_id).unwrap();
                self.add_cell_auto(space_id, &mut space);
                self.spaces.insert(space_id, space);
            }
        }
        has_loading
    }

    /// `CellAppMgr::metaLoadBalance`.
    pub fn meta_load_balance(&mut self) {
        if !self.should_meta_load_balance {
            return;
        }
        let groups = CellAppGroups::identify(&self.apps, &self.spaces);
        let merge_threshold = self.apps.avg_load() + META_LOAD_BALANCE_TOLERANCE;

        let overloaded = groups.overloaded_groups(&self.apps, merge_threshold);
        for group in overloaded {
            self.group_add_cell(group);
        }

        let has_loading_spaces = self.check_loading_spaces();

        if !has_loading_spaces {
            for group in &groups.groups {
                self.group_check_underloaded(group, LOWER_BOUND);
            }
        }
    }

    /// `CellAppGroup::addCell` for this manager.
    fn group_add_cell(&mut self, group: &CellAppGroup) {
        // `cancelRetiringCellApp`: stop the first app that is not retiring
        // but has retiring cells.
        let cancel = group.apps.iter().find(|a| {
            self.apps
                .find(**a)
                .map(|r| !r.is_retiring && r.has_any_retiring_cells())
                .unwrap_or(false)
        });
        if let Some(app) = cancel {
            if let Some(rec) = self.apps.find_mut(*app) {
                rec.num_cells_retiring = 0;
            }
            let space_ids: Vec<SpaceId> = self.spaces.keys().copied().collect();
            for space_id in space_ids {
                if let Some(space) = self.spaces.get_mut(&space_id) {
                    space.cancel_retiring_app(*app);
                }
            }
            return;
        }
        if let Some(space_id) =
            group.choose_connection_space(&self.spaces, MetaBalanceScheme::default_scheme())
        {
            let mut space = self.spaces.remove(&space_id).unwrap();
            self.add_cell_auto(space_id, &mut space);
            self.spaces.insert(space_id, space);
        }
    }

    /// `CellAppGroup::checkForUnderloaded` for this manager.
    fn group_check_underloaded(&mut self, group: &CellAppGroup, lower_bound: f32) {
        if group.is_empty() {
            return;
        }
        if group.avg_load(&self.apps, 1) < lower_bound {
            let least = group
                .apps
                .iter()
                .filter(|a| {
                    self.apps
                        .find(**a)
                        .map(|r| !r.has_only_retiring_cells())
                        .unwrap_or(false)
                })
                .min_by(|a, b| {
                    let av = self
                        .apps
                        .find(**a)
                        .map(|r| r.smoothed_load)
                        .unwrap_or(f32::MAX);
                    let bv = self
                        .apps
                        .find(**b)
                        .map(|r| r.smoothed_load)
                        .unwrap_or(f32::MAX);
                    av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some(app) = least {
                self.retire_all_cells_of(*app);
            }
        }
    }

    /// `CellApp::retireAllCells`.
    pub fn retire_all_cells_of(&mut self, app: Addr) {
        let Some(rec) = self.apps.find_mut(app) else {
            return;
        };
        rec.start_retiring();
        let space_ids: Vec<SpaceId> = self.spaces.keys().copied().collect();
        for space_id in space_ids {
            if let Some(space) = self.spaces.get_mut(&space_id) {
                let retired = space.start_retiring_app(app);
                if let Some(rec) = self.apps.find_mut(app) {
                    rec.num_cells_retiring += retired;
                }
            }
        }
        self.outbox.push(Outbound::RetireCell(app, NULL_SPACE_ID));
    }

    /// `CellAppMgr::loadBalance`.
    pub fn load_balance(&mut self) {
        let space_ids: Vec<SpaceId> = self.spaces.keys().copied().collect();
        for space_id in space_ids {
            let mut space = self.spaces.remove(&space_id).unwrap();
            let ctx = BalanceContext {
                space_bounds: space.bounds,
                space_grid: space.grid,
                apps: &self.apps,
                limit_to_chunks: SHOULD_LIMIT_BALANCE_TO_CHUNKS,
                balance_unloaded_chunks: true,
            };
            let removed = space.load_balance(&ctx);
            for app in removed {
                if let Some(rec) = self.apps.find_mut(app) {
                    rec.num_cells = rec.num_cells.saturating_sub(1);
                }
                self.outbox.push(Outbound::CellRemoval(app, space_id, app));
            }
            self.spaces.insert(space_id, space);
        }
    }

    /// `CellAppMgr::updateRanges`.
    pub fn update_ranges(&mut self) {
        for space in self.spaces.values_mut() {
            space.update_ranges();
        }
    }

    /// `CellAppMgr::handleCellAppDeath` for an app that died.
    pub fn handle_cell_app_death(&mut self, addr: Addr) {
        if self.is_shutting_down {
            return;
        }
        if self.apps.find(addr).is_none() {
            return;
        }
        self.last_cell_app_death = Some(addr);

        // Broadcast the death to every remaining app, then tell the
        // BaseAppMgr so the entity stream can be recreated.
        let remaining: Vec<Addr> = self
            .apps
            .iter()
            .filter(|a| a.addr != addr)
            .map(|a| a.addr)
            .collect();
        for r in &remaining {
            self.outbox.push(Outbound::CellAppDeath(*r, addr));
        }
        self.outbox.push(Outbound::ReportDeathToBaseAppMgr(addr));
        self.apps.remove(addr);

        // If there are no CellApps left, shut the cluster down.
        if self.apps.is_empty() {
            self.outbox.push(Outbound::TriggerControlledShutDown);
            return;
        }

        // Recreate the cells of the dead app on alternate apps.
        let space_ids: Vec<SpaceId> = self.spaces.keys().copied().collect();
        for space_id in space_ids {
            let mut space = self.spaces.remove(&space_id).unwrap();
            let had_dead_cell = space.erase_cell(addr);
            if had_dead_cell
                && space.num_cells() < 1
                && let Some(alt) = self.apps.find_alternate_app(addr)
                && let Some(rec) = self.apps.find(alt).cloned()
            {
                space.add_cell(&rec);
                if let Some(r) = self.apps.find_mut(alt) {
                    r.num_cells += 1;
                    r.estimated_load += ESTIMATED_INITIAL_CELL_LOAD;
                }
                self.outbox.push(Outbound::AddCell(alt, space_id));
            }
            self.spaces.insert(space_id, space);
        }
    }

    /// `CellAppMgr::overloadCheck`.
    pub fn overload_check(&mut self, now_ms: u64) {
        let is_avg = Self::calculate_overload(
            self.apps.is_avg_overloaded(),
            now_ms,
            &mut self.avg_overload_start_ms,
            (AVG_OVERLOAD_TOLERANCE_SECS * 1000.0) as u64,
        );
        let any = Self::calculate_overload(
            self.apps.are_any_overloaded(),
            now_ms,
            &mut self.max_overload_start_ms,
            (MAX_OVERLOAD_TOLERANCE_SECS * 1000.0) as u64,
        );
        self.outbox.push(Outbound::CellAppOverloadStatus {
            has_overloaded: any || is_avg,
        });
    }

    /// `CellAppMgr::calculateOverload`.
    pub fn calculate_overload(
        are_overloaded: bool,
        now_ms: u64,
        start_ms: &mut u64,
        tolerance_ms: u64,
    ) -> bool {
        if are_overloaded {
            if *start_ms == 0 {
                *start_ms = now_ms;
            }
            now_ms.saturating_sub(*start_ms) >= tolerance_ms
        } else {
            *start_ms = 0;
            false
        }
    }

    /// `CellAppMgr::shutDownSpace`.
    pub fn shut_down_space(&mut self, space_id: SpaceId) {
        let has_had_entities = self
            .spaces
            .get(&space_id)
            .map(|s| s.has_had_entities)
            .unwrap_or(false);
        if has_had_entities {
            // Recorded here; the cells are notified of the shutdown and the
            // space is erased on the next tick.
            self.spaces_shutting_down.insert(space_id);
        }
    }

    /// The game-tick work: drain pending apps, erase shutting-down spaces
    /// and check for dead CellApps.
    pub fn tick(&mut self, now_ms: u64) {
        // Drain pending apps: each gets a cell on every space that has none.
        while let Some(app) = self.pending_apps.pop() {
            let space_ids: Vec<SpaceId> = self.spaces.keys().copied().collect();
            for space_id in space_ids {
                if self.spaces[&space_id].num_cells() == 0 {
                    self.add_cell_for_app(space_id, app);
                }
            }
        }

        // Erase the spaces that were asked to shut down.
        for space_id in self.spaces_shutting_down.iter() {
            self.spaces.remove(space_id);
        }
        self.spaces_shutting_down.clear();

        // Check for dead CellApps.
        if let Some(dead) = self.apps.check_for_dead_apps(now_ms) {
            self.handle_cell_app_death(dead);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(addr: Addr, id: CellAppId, load: f32) -> CellAppRecord {
        let mut rec = CellAppRecord::new(addr, id, 1000 + id as u16, 3000);
        rec.smoothed_load = load;
        rec.curr_load = load;
        rec.last_received_load = load;
        rec.estimated_load = load;
        rec
    }

    fn mgr_with(apps: Vec<(Addr, f32)>) -> CellAppMgr {
        let mut mgr = CellAppMgr::new();
        mgr.set_base_app((0x7f000001, 100));
        mgr.set_db_app_alpha((0x7f000001, 200));
        mgr.allow_new_cell_apps = true;
        for (i, (addr, load)) in apps.iter().enumerate() {
            let rec = app(*addr, (i + 1) as u32, *load);
            mgr.apps.add(rec);
        }
        mgr
    }

    // ------------------------------------------------------------------
    // BW-05-04-01: create and destroy spaces and cells.
    // ------------------------------------------------------------------

    #[test]
    fn create_space_gives_a_cell_and_an_id() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        assert_eq!(id, 1);
        let space = &mgr.spaces[&id];
        assert_eq!(space.num_cells(), 1);
        assert_eq!(space.cells()[0].app, (0x7f000001, 3000));
        // The fresh cell owns the whole world.
        assert_eq!(space.cells()[0].range, Rect::unbounded());
    }

    #[test]
    fn generate_space_id_respects_explicit_ids() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5)]);
        assert_eq!(mgr.generate_space_id(7), 7);
        assert_eq!(mgr.generate_space_id(NULL_SPACE_ID), 8);
    }

    #[test]
    fn erase_cell_removes_and_rebalances() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        // Add a second cell on the other app.
        let space = mgr.spaces.get_mut(&id).unwrap();
        let rec = mgr.apps.find((0x7f000002, 3000)).unwrap().clone();
        space.add_cell(&rec);
        assert_eq!(space.num_cells(), 2);

        let ok = mgr
            .spaces
            .get_mut(&id)
            .unwrap()
            .erase_cell((0x7f000002, 3000));
        assert!(ok);
        assert_eq!(mgr.spaces[&id].num_cells(), 1);
        assert_eq!(mgr.spaces[&id].cells()[0].app, (0x7f000001, 3000));
    }

    #[test]
    fn erase_cell_of_unknown_app_is_noop() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        let ok = mgr.spaces.get_mut(&id).unwrap().erase_cell((0x7f000009, 9));
        assert!(!ok);
        assert_eq!(mgr.spaces[&id].num_cells(), 1);
    }

    #[test]
    fn shut_down_space_marks_only_spaces_with_entities() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        // No entities yet: the shutdown is not recorded.
        mgr.shut_down_space(id);
        assert!(mgr.spaces_shutting_down.is_empty());
        // With entities, it is recorded and erased on the next tick.
        mgr.spaces.get_mut(&id).unwrap().has_had_entities = true;
        mgr.shut_down_space(id);
        assert!(mgr.spaces_shutting_down.contains(&id));
        mgr.tick(10_000);
        assert!(!mgr.spaces.contains_key(&id));
    }

    #[test]
    fn handle_cell_app_death_recreates_cell_and_broadcasts() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        // Second cell on the second app so the space survives the first.
        let space = mgr.spaces.get_mut(&id).unwrap();
        let rec = mgr.apps.find((0x7f000002, 3000)).unwrap().clone();
        space.add_cell(&rec);
        assert_eq!(mgr.spaces[&id].num_cells(), 2);

        mgr.handle_cell_app_death((0x7f000001, 3000));
        assert!(mgr.apps.find((0x7f000001, 3000)).is_none());
        assert!(mgr.apps.find((0x7f000002, 3000)).is_some());
        // The surviving app keeps its cell.
        assert_eq!(mgr.spaces[&id].num_cells(), 1);
        assert_eq!(mgr.spaces[&id].cells()[0].app, (0x7f000002, 3000));
        // The death was broadcast to the survivor and reported to the
        // BaseAppMgr.
        assert!(mgr.outbox.iter().any(|o| matches!(
            o,
            Outbound::CellAppDeath(a, b) if *a == (0x7f000002, 3000) && *b == (0x7f000001, 3000)
        )));
        assert!(mgr.outbox.iter().any(
            |o| matches!(o, Outbound::ReportDeathToBaseAppMgr(a) if *a == (0x7f000001, 3000))
        ));
    }

    #[test]
    fn handle_cell_app_death_of_last_app_triggers_shutdown() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5)]);
        mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        mgr.handle_cell_app_death((0x7f000001, 3000));
        assert!(
            mgr.outbox
                .iter()
                .any(|o| matches!(o, Outbound::TriggerControlledShutDown))
        );
    }

    #[test]
    fn add_app_rejects_without_base_or_db() {
        let mut mgr = CellAppMgr::new();
        // No base app or DB alpha yet.
        match mgr.add_app((0x7f000001, 3000), 1000, 0) {
            AddAppOutcome::Rejected { retry } => assert!(retry),
            _ => panic!("expected rejection"),
        }
        mgr.set_base_app((0x7f000001, 100));
        match mgr.add_app((0x7f000001, 3000), 1000, 0) {
            AddAppOutcome::Rejected { retry } => assert!(retry),
            _ => panic!("expected rejection"),
        }
        mgr.set_db_app_alpha((0x7f000001, 200));
        mgr.allow_new_cell_apps = true;
        match mgr.add_app((0x7f000001, 3000), 1000, 0) {
            AddAppOutcome::Accepted {
                id, timeout_period, ..
            } => {
                assert_eq!(id, 1);
                assert_eq!(timeout_period, CELL_APP_TIMEOUT);
            }
            _ => panic!("expected acceptance"),
        }
        assert!(mgr.pending_apps.contains(&(0x7f000001, 3000)));
    }

    #[test]
    fn tick_drains_pending_apps_into_empty_spaces() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        // The second app was pending (never got a cell); the space already
        // has one cell so the drain must not add another.
        mgr.pending_apps.push((0x7f000002, 3000));
        mgr.tick(5_000);
        assert_eq!(mgr.spaces[&id].num_cells(), 1);
        assert!(mgr.pending_apps.is_empty());
    }

    // ------------------------------------------------------------------
    // BW-05-04-02: BSP split and bounds reporting.
    // ------------------------------------------------------------------

    #[test]
    fn bsp_splits_horizontal_first_then_vertical() {
        let mut mgr = mgr_with(vec![
            ((0x7f000001, 3000), 0.5),
            ((0x7f000002, 3000), 0.5),
            ((0x7f000003, 3000), 0.5),
        ]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        let space = mgr.spaces.get_mut(&id).unwrap();
        let rec2 = mgr.apps.find((0x7f000002, 3000)).unwrap().clone();
        let rec3 = mgr.apps.find((0x7f000003, 3000)).unwrap().clone();
        space.add_cell(&rec2);
        space.add_cell(&rec3);
        // Two cells: the first split is horizontal.
        let root = space.root.as_ref().unwrap();
        match &**root {
            BspNode::Internal(node) => {
                assert!(node.horizontal);
            }
            _ => panic!("expected an internal node"),
        }
        // Three cells: the top-level split stays horizontal, and the child
        // split at the next level is vertical.
        match space.root.as_ref().unwrap().as_ref() {
            BspNode::Internal(node) => {
                assert!(node.horizontal);
                match node.right.as_ref() {
                    BspNode::Internal(inner) => assert!(!inner.horizontal),
                    _ => panic!("expected an internal node on the right"),
                }
            }
            _ => panic!("expected an internal node"),
        }
        assert_eq!(space.num_cells(), 3);
    }

    #[test]
    fn new_cell_is_zero_area_at_partition_edge() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        let space = mgr.spaces.get_mut(&id).unwrap();
        let rec2 = mgr.apps.find((0x7f000002, 3000)).unwrap().clone();
        space.add_cell(&rec2);
        let cells = space.cells();
        // The partition line is at +inf; one cell owns the world, the other
        // starts with zero area.
        let first = cells.iter().find(|c| c.app == (0x7f000001, 3000)).unwrap();
        let second = cells.iter().find(|c| c.app == (0x7f000002, 3000)).unwrap();
        assert_eq!(first.range, Rect::unbounded());
        // The first split is horizontal, so the collapsed axis is y.
        assert!(second.range.ymax - second.range.ymin <= 0.0);
        assert_eq!(second.range.xmin, Rect::unbounded().xmin);
    }

    #[test]
    fn bounds_report_marks_loaded_area() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        mgr.spaces
            .get_mut(&id)
            .unwrap()
            .set_space_bounds(Rect::new(0.0, 0.0, 100.0, 100.0), 1.0);
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        let levels = EntityBoundLevels::new();
        mgr.spaces
            .get_mut(&id)
            .unwrap()
            .for_each_cell_mut(&mut |cell| {
                cell.update_bounds_report(bounds, 10, &levels);
            });
        let cell = &mgr.spaces[&id].cells()[0];
        assert_eq!(cell.chunk_bounds, bounds);
        assert_eq!(cell.num_entities, 10);
        assert!(cell.has_had_loaded_area);
    }

    #[test]
    fn update_ranges_partitions_the_unbounded_root() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        mgr.add_cell_for_app(id, (0x7f000002, 3000));
        mgr.spaces.get_mut(&id).unwrap().update_ranges();
        let cells = mgr.spaces[&id].cells();
        assert_eq!(cells.len(), 2);
        let first = cells.iter().find(|c| c.app == (0x7f000001, 3000)).unwrap();
        let second = cells.iter().find(|c| c.app == (0x7f000002, 3000)).unwrap();
        // The split is horizontal (y) with the partition at +inf, so the
        // existing cell keeps the whole unbounded range and the new cell is
        // a zero-area sliver at +inf y.
        assert_eq!(first.range, Rect::unbounded());
        assert!(second.range.ymax - second.range.ymin <= 0.0);
        assert_eq!(second.range.ymin, Rect::unbounded().ymax);
    }

    // ------------------------------------------------------------------
    // BW-05-04-03: cell merge and underloaded retire.
    // ------------------------------------------------------------------

    #[test]
    fn remove_cell_promotes_the_sibling() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        let space = mgr.spaces.get_mut(&id).unwrap();
        let rec2 = mgr.apps.find((0x7f000002, 3000)).unwrap().clone();
        space.add_cell(&rec2);
        assert_eq!(space.num_cells(), 2);
        space.erase_cell((0x7f000001, 3000));
        // The tree collapses back to a single leaf owning the world.
        assert_eq!(space.num_cells(), 1);
        assert_eq!(space.cells()[0].app, (0x7f000002, 3000));
        assert_eq!(space.cells()[0].range, Rect::unbounded());
    }

    #[test]
    fn retire_all_cells_marks_app_and_cells() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        let rec2 = mgr.apps.find((0x7f000002, 3000)).unwrap().clone();
        mgr.spaces.get_mut(&id).unwrap().add_cell(&rec2);

        mgr.retire_all_cells_of((0x7f000001, 3000));
        let rec = mgr.apps.find((0x7f000001, 3000)).unwrap();
        assert!(rec.is_retiring);
        assert_eq!(rec.num_cells_retiring, 1);
        let cells = mgr.spaces[&id].cells();
        let cell = cells.iter().find(|c| c.app == (0x7f000001, 3000)).unwrap();
        assert!(cell.is_retiring());
    }

    #[test]
    fn group_check_underloaded_retires_least_loaded() {
        // Both apps are lightly loaded so removing one leaves the average
        // below the lower bound.
        let mut mgr = mgr_with(vec![
            ((0x7f000001, 3000), 0.01),
            ((0x7f000002, 3000), 0.005),
        ]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        // create_space gave the first cell to the least-loaded app
        // (0x7f000002); put the second on the other app.
        mgr.add_cell_for_app(id, (0x7f000001, 3000));

        let groups = CellAppGroups::identify(&mgr.apps, &mgr.spaces);
        let group = groups.groups.iter().find(|g| g.len() == 2).unwrap().clone();
        // avgLoad with one removed is well below the lower bound.
        mgr.group_check_underloaded(&group, LOWER_BOUND);
        let rec2 = mgr.apps.find((0x7f000002, 3000)).unwrap();
        assert!(rec2.is_retiring);
    }

    #[test]
    fn meta_load_balance_retires_empty_groups() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        mgr.add_cell_for_app(id, (0x7f000002, 3000));
        mgr.should_meta_load_balance = true;
        mgr.meta_load_balance();
        // Nothing is overloaded or underloaded, so no retirement.
        assert!(!mgr.apps.find((0x7f000001, 3000)).unwrap().is_retiring);
        assert!(!mgr.apps.find((0x7f000002, 3000)).unwrap().is_retiring);
    }

    // ------------------------------------------------------------------
    // BW-05-04-04: cross-host allocation.
    // ------------------------------------------------------------------

    #[test]
    fn find_best_cell_app_picks_the_least_loaded() {
        let mut mgr = mgr_with(vec![
            ((0x7f000001, 3000), 0.5),
            ((0x7f000002, 3000), 0.2),
            ((0x7f000003, 3000), 0.8),
        ]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        let best = mgr.find_best_cell_app(&mgr.spaces[&id], None);
        assert_eq!(best, Some((0x7f000002, 3000)));
    }

    #[test]
    fn add_cell_auto_prefers_a_different_group() {
        // Two apps: one owns the first cell of the space, the other is a
        // separate group. Adding a cell must go to the other app.
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.9), ((0x7f000002, 3000), 0.1)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        // The first cell went to the least-loaded app (0x7f000002). Force
        // the group to be the one holding it so the new cell leaves it.
        let mut space = mgr.spaces.remove(&id).unwrap();
        let new_app = mgr.add_cell_auto(id, &mut space).unwrap();
        assert_eq!(new_app, (0x7f000001, 3000));
        mgr.spaces.insert(id, space);
        assert_eq!(mgr.spaces[&id].num_cells(), 2);
        assert!(
            mgr.outbox.iter().any(
                |o| matches!(o, Outbound::AddCell(a, s) if *a == (0x7f000001, 3000) && *s == id)
            )
        );
    }

    #[test]
    fn find_alternate_app_prefers_a_different_machine() {
        let mgr = mgr_with(vec![
            ((0x7f000001, 3000), 0.5),
            ((0x7f000001, 3001), 0.1),
            ((0x7f000002, 3000), 0.6),
        ]);
        let alt = mgr.apps.find_alternate_app((0x7f000001, 3000));
        // Prefers an app on another machine (0x7f000002) over the same-IP
        // sibling.
        assert_eq!(alt, Some((0x7f000002, 3000)));
    }

    #[test]
    fn is_valid_app_respects_limited_spaces() {
        let mut rec = app((0x7f000001, 3000), 1, 0.5);
        rec.num_cells = 3;
        let scorers = vec![ScorerConfig {
            kind: ScorerKind::LimitedSpaces,
            max_spaces_per_cell_app: 3,
        }];
        let space = Space::new(1, false, 0);
        assert!(!is_valid_app(&scorers, &rec, &space));
        rec.num_cells = 2;
        assert!(is_valid_app(&scorers, &rec, &space));
    }

    #[test]
    fn is_a_better_cell_app_breaks_ties_on_load_first() {
        let scorers = default_scorers();
        let a = app((0x7f000001, 3000), 1, 0.5);
        let mut b = app((0x7f000002, 3000), 2, 0.5);
        b.estimated_load = 0.3;
        let space = Space::new(1, false, 0);
        let group_load = |_: Addr| 0.0;
        assert!(is_a_better_cell_app(
            &scorers,
            Some(&a),
            &b,
            &space,
            &group_load
        ));
        assert!(!is_a_better_cell_app(
            &scorers,
            Some(&b),
            &a,
            &space,
            &group_load
        ));
    }

    // ------------------------------------------------------------------
    // BW-05-04-05: viewer read interface.
    // ------------------------------------------------------------------

    #[test]
    fn viewer_stream_encodes_a_leaf_and_terminator() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        let mut stream = BinaryStream::new();
        mgr.spaces[&id].add_to_stream(&mut stream, 1, 0);
        let bytes = stream.as_bytes();
        // space id, numCellApps, numEntities.
        assert_eq!(&bytes[0..4], &id.to_le_bytes());
        assert_eq!(&bytes[4..8], &1u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &0u32.to_le_bytes());
        // Leaf tag.
        assert_eq!(bytes[12], BSP_NODE_LEAF);
        // The space with a single cell ends with a leaf terminator after the
        // tree, which itself is a leaf.
        assert_eq!(bytes.last().copied(), Some(BSP_NODE_LEAF));
    }

    #[test]
    fn viewer_stream_encodes_internal_node_and_bounds() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.5), ((0x7f000002, 3000), 0.5)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        let rec2 = mgr.apps.find((0x7f000002, 3000)).unwrap().clone();
        mgr.spaces.get_mut(&id).unwrap().add_cell(&rec2);

        let mut stream = BinaryStream::new();
        mgr.spaces[&id].add_to_stream(&mut stream, 2, 0);
        let bytes = stream.as_bytes();
        // The first node is internal (horizontal split).
        let tag = bytes[12];
        assert_eq!(tag, BSP_NODE_HORIZONTAL);
        // Then the partition position, then two leaves, then a terminator.
        assert_eq!(bytes.last().copied(), Some(BSP_NODE_LEAF));
    }

    #[test]
    fn viewer_stream_leaf_has_viewer_fields_after_app_addr() {
        let mut mgr = mgr_with(vec![((0x7f000001, 3000), 0.4)]);
        let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
        // The leaf's smoothed load is refreshed from the app by `update_load`,
        // which the test drives directly.
        mgr.spaces
            .get_mut(&id)
            .unwrap()
            .for_each_cell_mut(&mut |cell| {
                cell.smoothed_load = 0.4;
            });
        let mut stream = BinaryStream::new();
        mgr.spaces[&id].add_to_stream(&mut stream, 1, 0);
        let bytes = stream.as_bytes();
        // After the 12-byte header: leaf tag, ip, port, smoothed load.
        let mut off = 12;
        assert_eq!(bytes[off], BSP_NODE_LEAF);
        off += 1;
        assert_eq!(&bytes[off..off + 4], &0x7f000001u32.to_le_bytes());
        off += 4;
        assert_eq!(&bytes[off..off + 2], &3000u16.to_le_bytes());
        off += 2;
        let load = f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        assert!((load - 0.4).abs() < 1e-4);
        off += 4;
        // Viewer fields: app id (1), viewer port (1001), then the entity
        // bound levels (`numCPUOffloadLevels`, each 4 edges).
        assert_eq!(&bytes[off..off + 4], &1u32.to_le_bytes());
        off += 4;
        assert_eq!(&bytes[off..off + 2], &1001u16.to_le_bytes());
        off += 2;
        assert_eq!(
            &bytes[off..off + 4],
            &(NUM_CPU_OFFLOAD_LEVELS as u32).to_le_bytes()
        );
    }
}
