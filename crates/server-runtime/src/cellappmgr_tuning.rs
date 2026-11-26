//! OPT-05-01-02: the load-reporting and split/merge tuning comparison.
//!
//! The acceptance compares the scheduling effect of the load reporting
//! and the partition move/split tuning on **fixed scenarios**. The
//! engine is unchanged — every knob the comparison varies is one the
//! reference exposes as runtime config (`cellappmgr/loadSmoothingBias`,
//! `balance/aggression*`, `cellappmgr/shouldLimitBalanceToChunks`) and
//! this harness reaches it the way an operator would:
//!
//! - **Load reporting** — the `LOAD_SMOOTHING_BIAS` EMA is replicated at
//!   the harness level over the record's public fields (the replication
//!   is validated element-wise against the shipped `inform_of_load` at
//!   the default bias), then run at candidate biases.
//! - **Split/merge direction** — the aggression tuner
//!   (`adjustAggression`: halve on reversal, recover
//!   `aggressionIncrease` per pass, clamp) is exercised through the
//!   live tree; the fixture seeds `prev_direction` to place the first
//!   post-swap pass on the reversal or the continuation branch.
//! - **Chunk limiting** — `BalanceContext.limit_to_chunks` is the
//!   config switch; the retirement-drain scenario runs it on (shipped)
//!   and off.
//!
//! Three fixed scenarios (the OPT-05-01-01 geometry), all deterministic:
//!
//! 1. **Hotspot ramp** — A ramps to 0.9 through the candidate EMA;
//!    measures informs to the 0.6 threshold and balance passes to the
//!    entity-edge bind. Faster reporting reaches the threshold sooner
//!    and converges to the same partition, so the bias only trades
//!    detection latency, not the landing geometry.
//! 2. **Reversal** — B becomes the hot side; the partition reverses.
//!    The reversal halves the aggression (`AGGRESSION_DECREASE`) and it
//!    recovers `aggression_increase` per pass; the continuation branch
//!    keeps it clamped at max. The position lands on the same entity
//!    level either way (`closest_limit` is monotone), so the tuner's
//!    observable is the aggression trajectory itself.
//! 3. **Retirement drain** — the real-area retiring cell of the
//!    baseline's held arm, re-run with the chunk limit off: the drain
//!    completes instead of being pinned by the survivor's ghost
//!    margin. The retiring cell reports the empty-cell bounds the
//!    reference streams (`Space::writeEntityBoundsForEdge`: every
//!    offload level at the unbounded edge, loaded with the level's
//!    offload floor), which is what lets the partition travel to the
//!    world edge and shrink the cell to zero area.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `CELLAPPMGR_TUNING_OUT` is set, to that path.

use std::io::Write as _;
use std::time::Instant;

use super::cellappmgr_baseline::{
    APP_A, APP_B, ENTITY_EDGE, MAX_PASSES, SPACE, TICK_MS, cell, fmt_rect, mgr_with, report_bounds,
    report_bounds_both, top_split_two_cell_space,
};
use super::{
    AGGRESSION_DECREASE, BalanceContext, BalanceDirection, BspInternal, BspNode, CellAppMgr,
    EntityBoundLevels, LOAD_SMOOTHING_BIAS, NULL_SPACE_ID, Outbound, Rect, Space,
    cpu_offload_for_level, min_cpu_offload,
};

/// The reported load of the ramp scenario.
const RAMP_LOAD: f32 = 0.9;
/// The smoothed-load threshold the ramp is timed against.
const RAMP_THRESHOLD: f32 = 0.6;
/// The inform cap of the ramp; the slowest candidate crosses far below.
const RAMP_MAX_INFORMS: usize = 60;
/// Balance passes of the reversal window (fixed count — the aggression
/// recovery is the face, not a settle detector).
const SWAP_PASSES: usize = 8;
/// Balance passes of the retirement windows.
const RETIRE_PASSES: usize = 8;

// ---------------------------------------------------------------------------
// The record sink (the baseline harness shape, own env)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$CELLAPPMGR_TUNING_OUT` when set.
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("CELLAPPMGR_TUNING_OUT") {
            std::fs::write(&path, &text).expect("write CELLAPPMGR_TUNING_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

/// The `inform_of_load` EMA, at an operator-chosen bias. The default
/// bias candidate is validated against the shipped method.
fn ema(prev: f32, load: f32, bias: f32) -> f32 {
    ((1.0 - bias) * prev) + (bias * load)
}

/// Reports one ramp inform through the harness EMA at `bias`, mirroring
/// `CellAppRecord::inform_of_load` over the record's public fields.
fn inform_at_bias(mgr: &mut CellAppMgr, load: f32, bias: f32, now: u64) {
    let rec = mgr.apps.find_mut(APP_A).expect("the ramp app");
    let smoothed = ema(rec.smoothed_load, load, bias);
    rec.last_received_load = load;
    rec.curr_load = load;
    rec.smoothed_load = smoothed;
    rec.estimated_load = smoothed;
    rec.last_heard_ms = now;
}

/// A two-cell space with a horizontal split at `position`: A holds
/// everything below the line, B everything above — the shape after the
/// first absorption pass. `aggression` and `prev_direction` seed the
/// reversal branch the first balance pass takes.
fn split_fixture(
    mgr: &mut CellAppMgr,
    position: f32,
    aggression: f32,
    prev: BalanceDirection,
) -> u32 {
    let id = mgr.generate_space_id(NULL_SPACE_ID);
    let mut space = Space::new(id, false, 0);
    let a = mgr.apps.find(APP_A).unwrap().clone();
    let b = mgr.apps.find(APP_B).unwrap().clone();
    let a_leaf = cell(Rect::new(SPACE.xmin, SPACE.ymin, SPACE.xmax, position), &a);
    let b_leaf = cell(Rect::new(SPACE.xmin, position, SPACE.xmax, SPACE.ymax), &b);
    let node = BspInternal {
        left: Box::new(BspNode::Leaf(a_leaf)),
        right: Box::new(BspNode::Leaf(b_leaf)),
        horizontal: true,
        position,
        aggression,
        total_load: 0.0,
        total_smoothed_load: 0.0,
        min_load: 0.0,
        max_load: 0.0,
        left_count: 1,
        right_count: 1,
        prev_direction: prev,
        range: SPACE,
        bounds: EntityBoundLevels::new(),
        chunk_bounds: Rect::empty(),
        num_retiring: 0,
        area_not_loaded: 0.0,
    };
    space.root = Some(Box::new(BspNode::Internal(node)));
    mgr.spaces.insert(id, space);
    id
}

/// One balance pass with an operator-chosen `limit_to_chunks`, the way
/// `CellAppMgr::load_balance` runs it with the config switch read.
fn balance_with(mgr: &mut CellAppMgr, space_id: u32, limit_to_chunks: bool) {
    let mut space = mgr.spaces.remove(&space_id).unwrap();
    let removed = {
        let ctx = BalanceContext {
            space_bounds: space.bounds,
            space_grid: space.grid,
            apps: &mgr.apps,
            limit_to_chunks,
            balance_unloaded_chunks: true,
        };
        space.load_balance(&ctx)
    };
    for app in removed {
        if let Some(rec) = mgr.apps.find_mut(app) {
            rec.num_cells = rec.num_cells.saturating_sub(1);
        }
        mgr.outbox.push(Outbound::CellRemoval(app, space_id, app));
    }
    mgr.spaces.insert(space_id, space);
}

/// The partition position after the passes so far (B's bottom edge).
fn line(mgr: &CellAppMgr, space_id: u32) -> f32 {
    mgr.spaces[&space_id]
        .find_cell_by_app(APP_B)
        .map(|c| c.range.ymin)
        .expect("B holds the upper strip")
}

/// The root node's aggression (the tuner state the scenario reads).
fn root_aggression(mgr: &CellAppMgr, space_id: u32) -> f32 {
    match mgr.spaces[&space_id].root.as_deref() {
        Some(BspNode::Internal(node)) => node.aggression,
        _ => panic!("the space holds the split root"),
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: the hotspot ramp across reporting biases
// ---------------------------------------------------------------------------

/// Runs the ramp at `bias`; `(informs to the threshold, passes to the
/// bind, final partition edge)`. `shipped` routes the informs through
/// `CellAppRecord::inform_of_load` instead of the harness EMA.
fn ramp_scenario(bias: f32, shipped: bool) -> (usize, usize, f32) {
    let mut mgr = mgr_with(vec![(APP_A, 0.05), (APP_B, 0.05)]);
    let id = top_split_two_cell_space(&mut mgr, SPACE.ymax);
    report_bounds(&mut mgr, id, (true, ENTITY_EDGE));

    let mut informs = 0;
    let mut ladder = Vec::new();
    for step in 1..=RAMP_MAX_INFORMS {
        let now = step as u64 * TICK_MS;
        if shipped {
            mgr.apps
                .find_mut(APP_A)
                .unwrap()
                .inform_of_load(RAMP_LOAD, 0, now);
        } else {
            inform_at_bias(&mut mgr, RAMP_LOAD, bias, now);
        }
        ladder.push(mgr.apps.find(APP_A).unwrap().smoothed_load);
        informs = step;
        if *ladder.last().unwrap() >= RAMP_THRESHOLD {
            break;
        }
    }
    assert!(
        *ladder.last().unwrap() >= RAMP_THRESHOLD,
        "bias {bias} crosses the threshold within the cap"
    );

    let mut passes = 0;
    let mut positions = Vec::new();
    for _ in 0..MAX_PASSES {
        mgr.load_balance();
        passes += 1;
        let position = line(&mgr, id);
        let settled = positions.last() == Some(&position);
        positions.push(position);
        if settled {
            break;
        }
    }
    let final_edge = positions.last().copied().expect("at least one pass");
    assert_eq!(final_edge, ENTITY_EDGE, "bias {bias} binds the entity edge");
    (informs, passes, final_edge)
}

fn ramp_leg(record: &mut Record) {
    // The replication face: the harness EMA at the shipped bias must
    // reproduce the shipped `inform_of_load` ladder element-wise, or no
    // candidate number is comparable.
    let (shipped_informs, _, _) = ramp_scenario(LOAD_SMOOTHING_BIAS, true);
    let (harness_informs, _, _) = ramp_scenario(LOAD_SMOOTHING_BIAS, false);
    assert_eq!(
        shipped_informs, harness_informs,
        "the harness EMA replicates the shipped reporting at the default bias"
    );
    record.put("ramp_default_bias_informs", harness_informs);

    // The candidates: a slower and two faster reporting biases.
    let slow = ramp_scenario(0.02, false);
    let fast = ramp_scenario(0.1, false);
    let fastest = ramp_scenario(0.2, false);
    assert!(
        slow.0 > harness_informs && harness_informs > fast.0 && fast.0 > fastest.0,
        "the detection latency orders with the bias: {} > {} > {} > {}",
        slow.0,
        harness_informs,
        fast.0,
        fastest.0
    );
    assert_eq!(slow.2, ENTITY_EDGE);
    assert_eq!(fast.2, ENTITY_EDGE);
    assert_eq!(fastest.2, ENTITY_EDGE);
    for (bias, (informs, passes, edge)) in [(0.02, slow), (0.1, fast), (0.2, fastest)] {
        record.put(format!("ramp_bias_{bias}_informs_to_060"), informs);
        record.put(format!("ramp_bias_{bias}_passes"), passes);
        record.put(format!("ramp_bias_{bias}_final_edge"), format!("{edge:.0}"));
    }
}

// ---------------------------------------------------------------------------
// Scenario 2: the reversal — the aggression tuner's two branches
// ---------------------------------------------------------------------------

/// The multi-level entity bound of the hot side: descending loads, one
/// bound per level, so the level the pass picks orders with
/// `|loadDiff * aggression|`.
fn hot_levels() -> EntityBoundLevels {
    let mut levels = EntityBoundLevels::new();
    let loads = [0.45f32, 0.25, 0.05, 0.02, min_cpu_offload()];
    let bounds = [6000.0f32, 5000.0, 4500.0, 4200.0, 4000.0];
    for (level, (&load, &bound)) in loads.iter().zip(bounds.iter()).enumerate() {
        levels.set_level(level, &[(true, false, bound, load)]);
    }
    levels
}

/// Runs the reversal: B hot, the partition moving up. `prev` seeds the
/// tuner branch (reversal halves, continuation keeps the max). Returns
/// the aggression after each pass.
fn swap_scenario(prev: BalanceDirection) -> Vec<f32> {
    let mut mgr = mgr_with(vec![(APP_A, 0.05), (APP_B, RAMP_LOAD)]);
    let id = split_fixture(&mut mgr, ENTITY_EDGE, 1.0, prev);
    report_bounds_both(&mut mgr, id, (true, ENTITY_EDGE), hot_levels());

    let mut aggs = Vec::new();
    for _ in 0..SWAP_PASSES {
        balance_with(&mut mgr, id, false);
        aggs.push(root_aggression(&mgr, id));
    }
    // Both branches land on the same entity level: the position is
    // monotone (`closest_limit`), so the tuner costs no convergence.
    assert_eq!(line(&mgr, id), 5000.0, "the reversal binds level 1");
    aggs
}

fn swap_leg(record: &mut Record) {
    // Reversal: the pre-swap window moved Left, so the first up pass
    // halves the aggression and it recovers per pass.
    let damped = swap_scenario(BalanceDirection::Left);
    // Continuation: the tuner was already heading up, so no reversal —
    // the aggression stays clamped at the max.
    let live = swap_scenario(BalanceDirection::Right);

    assert!(
        (damped[0] - 1.0 * AGGRESSION_DECREASE).abs() < 1e-6,
        "the reversal halves the aggression: {}",
        damped[0]
    );
    assert!(
        (live[0] - 1.0).abs() < 1e-6,
        "the continuation keeps the max aggression: {}",
        live[0]
    );
    for pair in damped.windows(2) {
        assert!(
            pair[1] > pair[0],
            "the damped aggression recovers: {pair:?}"
        );
        assert!(pair[1] <= 1.0 + 1e-6, "the recovery clamps at max");
    }
    for agg in &live {
        assert!((agg - 1.0).abs() < 1e-6, "the live aggression stays max");
    }

    for (n, agg) in damped.iter().enumerate() {
        record.put(
            format!("swap_damped_aggr_pass_{:02}", n + 1),
            format!("{agg:.4}"),
        );
    }
    record.put("swap_damped_first_aggr", format!("{:.2}", damped[0]));
    record.put("swap_live_first_aggr", format!("{:.2}", live[0]));
    record.put("swap_final_edge", 5000);
}

// ---------------------------------------------------------------------------
// Scenario 3: the retirement drain across the chunk-limit switch
// ---------------------------------------------------------------------------

/// The entity bounds an empty cell reports (`Space::
/// writeEntityBoundsForEdge`): no real entity overcame any offload
/// limit, so every level sits at the unbounded world edge with the
/// level's offload floor as its load.
fn retiree_levels() -> EntityBoundLevels {
    let mut levels = EntityBoundLevels::new();
    for level in 0..super::NUM_CPU_OFFLOAD_LEVELS {
        levels.set_level(
            level,
            &[(true, false, f32::MAX, cpu_offload_for_level(level))],
        );
    }
    levels
}

fn retire_leg(record: &mut Record) {
    // Chunks limiting on (shipped): the survivor's ghost margin pins the
    // line — the baseline's held face.
    let mut mgr = mgr_with(vec![(APP_A, 0.6), (APP_B, 0.05)]);
    let id = split_fixture(&mut mgr, ENTITY_EDGE, 1.0, BalanceDirection::None);
    report_bounds(&mut mgr, id, (true, ENTITY_EDGE));
    mgr.retire_all_cells_of(APP_B);
    // The retiring cell is empty, so its CellApp reports the same bounds
    // an empty cell does (`Space::writeEntityBoundsForEdge`): every
    // offload level sits at the unbounded edge with the level's offload
    // floor as load — the partition may travel to the world edge.
    let b_levels = retiree_levels();
    report_bounds_both(&mut mgr, id, (true, ENTITY_EDGE), b_levels.clone());
    let mut held = Vec::new();
    for _ in 0..RETIRE_PASSES {
        balance_with(&mut mgr, id, true);
        held.push(line(&mgr, id));
    }
    assert!(
        held.iter().all(|p| (*p - ENTITY_EDGE).abs() < 1e-3),
        "the chunk limit holds the line: {held:?}"
    );
    assert_eq!(
        mgr.outbox
            .iter()
            .filter(|o| matches!(o, Outbound::CellRemoval(a, s, _) if *a == APP_B && *s == id))
            .count(),
        0,
        "the held drain removes nothing"
    );

    // The same window with the switch off: the drain completes in one
    // pass, the retiring cell is deleted, the survivor is promoted.
    let mut mgr = mgr_with(vec![(APP_A, 0.6), (APP_B, 0.05)]);
    let id = split_fixture(&mut mgr, ENTITY_EDGE, 1.0, BalanceDirection::None);
    report_bounds(&mut mgr, id, (true, ENTITY_EDGE));
    mgr.retire_all_cells_of(APP_B);
    report_bounds_both(&mut mgr, id, (true, ENTITY_EDGE), b_levels.clone());
    let mut drain_passes = 0;
    while mgr.spaces[&id].find_cell_by_app(APP_B).is_some() {
        drain_passes += 1;
        assert!(drain_passes <= MAX_PASSES, "the open drain completes");
        balance_with(&mut mgr, id, false);
    }
    assert_eq!(drain_passes, 1, "the open drain completes in one pass");
    assert_eq!(
        mgr.outbox
            .iter()
            .filter(|o| matches!(o, Outbound::CellRemoval(a, s, _) if *a == APP_B && *s == id))
            .count(),
        1,
        "exactly one CellRemoval for the drained cell"
    );
    let a = mgr.spaces[&id]
        .find_cell_by_app(APP_A)
        .expect("the survivor remains");
    assert_eq!(a.range, Rect::unbounded(), "the survivor is promoted");

    record.put("retire_chunks_on_final_edge", format!("{:.0}", held[0]));
    record.put("retire_chunks_on_cell_removals", 0);
    record.put("retire_chunks_off_passes", drain_passes);
    record.put("retire_chunks_off_cell_removals", 1);
    record.put("retire_chunks_off_survivor_range", fmt_rect(a.range));
}

/// Runs every scenario in fixed order and leaves the comparison record.
#[test]
fn the_cellappmgr_tuning_compares_the_fixed_scenarios() {
    let started = Instant::now();
    let mut record = Record::default();
    ramp_leg(&mut record);
    swap_leg(&mut record);
    retire_leg(&mut record);
    record.put(
        "tuning_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
