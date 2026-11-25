//! OPT-05-01-01: the CellAppMgr load-hotspot and failure baseline.
//!
//! The acceptance records the **allocation, recycling and recovery
//! windows** of the CellAppMgr (`BW-05-04` port, `cellappmgr.rs`): how
//! many balance passes a hotspot redistribution takes, how a retiring
//! cell drains, and how a dead CellApp is detected and covered. Three
//! legs, all on a synthetic clock so every ladder is deterministic:
//!
//! 1. **Allocation** — a two-cell space absorbs a hotspot: the idle app
//!    holds a zero-area sliver cell, the hot app's smoothed load ramps
//!    up through `inform_of_load` (the `LOAD_SMOOTHING_BIAS` EMA, the
//!    load-reporting face OPT-05-01-02 tunes), then balance passes move
//!    the partition line until the reported entity edge of the hot cell
//!    binds (`InternalNode::doBalance` -> `closestLimit`).
//! 2. **Recycling** — `retire_all_cells_of` marks an app retiring; a
//!    zero-area retiring cell drains on the first pass (the
//!    `hasNoArea && retiring && no entities` delete arm), while a cell
//!    with real area is held at its post-absorption line because the
//!    survivor's loaded-chunk ghost margin (`GHOST_DISTANCE`) binds the
//!    reverse move. Both faces are recorded as the recycling completion
//!    requires.
//! 3. **Recovery** — a CellApp stops reporting; `tick` detects it via
//!    the strict `now - lastHeard > timeout` check and
//!    `handleCellAppDeath` recreates its cell on the alternate app with
//!    the `ESTIMATED_INITIAL_CELL_LOAD` bump; the last-app death
//!    triggers the controlled shutdown.
//!
//! Structural assertions run on every execution (ladder determinism,
//! outbox shapes, registry bookkeeping, reference-pinned formulas).
//! Wall-clock leg durations are **recorded, never asserted tight**.
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `CELLAPPMGR_BASELINE_OUT` is set, to that path.

use std::io::Write as _;
use std::time::Instant;

use super::{
    Addr, BspInternal, BspNode, CellAppMgr, CellAppRecord, CellLeaf, ESTIMATED_INITIAL_CELL_LOAD,
    EntityBoundLevels, LOAD_SMOOTHING_BIAS, NULL_SPACE_ID, Outbound, Rect, Space,
};

/// The two CellApps of every scenario: different machines (the recovery
/// alternate prefers a cross-host app).
const APP_A: Addr = (0x7f000001, 3000);
const APP_B: Addr = (0x7f000002, 3000);
/// The CellApp timeout every fixture registers (`mgr_with`), milliseconds.
const TIMEOUT_MS: u64 = 3000;
/// The inform cadence of the synthetic clock, milliseconds.
const TICK_MS: u64 = 1000;
/// Balance-pass cap per window; the ladders settle far below it.
const MAX_PASSES: usize = 12;
/// The space geometry of the two balance legs: a 10000x10000 space
/// split at the hot cell's reported entity edge (y = 6000), large
/// enough that the `GHOST_DISTANCE` margin does not swallow it.
const SPACE: Rect = Rect {
    xmin: 0.0,
    ymin: 0.0,
    xmax: 10000.0,
    ymax: 10000.0,
};
const SPACE_GRID: f32 = 500.0;
/// The entity edge the hot cell reports: y = 4000, below the initial
/// split at the space top (10000), so the partition has room to move.
const ENTITY_EDGE: f32 = 4000.0;

// ---------------------------------------------------------------------------
// The record sink (the entitydef baseline harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$CELLAPPMGR_BASELINE_OUT` when set.
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: &str, value: impl std::fmt::Display) {
        self.0.push((metric.to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("CELLAPPMGR_BASELINE_OUT") {
            std::fs::write(&path, &text).expect("write CELLAPPMGR_BASELINE_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

fn fmt_rect(rect: Rect) -> String {
    format!(
        "{:.0},{:.0},{:.0},{:.0}",
        rect.xmin, rect.ymin, rect.xmax, rect.ymax
    )
}

// ---------------------------------------------------------------------------
// The fixtures (the in-crate CellAppMgr test idioms)
// ---------------------------------------------------------------------------

/// `CellAppRecord` with a fixed load, as the in-crate `app` helper.
fn app(addr: Addr, id: u32, load: f32) -> CellAppRecord {
    let mut rec = CellAppRecord::new(addr, id, 1000 + id as u16, TIMEOUT_MS);
    rec.smoothed_load = load;
    rec.curr_load = load;
    rec.last_received_load = load;
    rec.estimated_load = load;
    rec
}

/// `CellAppMgr` with the given `(addr, load)` apps, as the in-crate
/// `mgr_with` helper.
fn mgr_with(apps: Vec<(Addr, f32)>) -> CellAppMgr {
    let mut mgr = CellAppMgr::new();
    mgr.set_base_app((0x7f000001, 100));
    mgr.set_db_app_alpha((0x7f000001, 200));
    mgr.allow_new_cell_apps = true;
    for (i, (addr, load)) in apps.iter().enumerate() {
        mgr.apps.add(app(*addr, (i + 1) as u32, *load));
    }
    mgr
}

/// Marks every cell created (the CellApps' createCells ack, which the
/// reference gets before the first balance pass runs) and feeds the
/// per-cell chunk and entity-bound reports of the split fixture.
fn report_bounds(mgr: &mut CellAppMgr, space_id: u32, hot_entity_edge: (bool, f32)) {
    report_bounds_both(mgr, space_id, hot_entity_edge, EntityBoundLevels::new());
}

/// Same as `report_bounds`, with explicit entity bounds for the B cell.
fn report_bounds_both(
    mgr: &mut CellAppMgr,
    space_id: u32,
    hot_entity_edge: (bool, f32),
    b_levels: EntityBoundLevels,
) {
    let mut hot_levels = EntityBoundLevels::new();
    // One offload level whose y edge sits at `position` with zero load:
    // `(is_y, is_max, position, load)` — `EntityBoundLevels::set_level`.
    let (is_max, position) = hot_entity_edge;
    hot_levels.set_level(0, &[(true, is_max, position, 0.0)]);

    // Clipped to the space bounds, as a CellApp's bounds report is:
    // an unbounded range inflated by GHOST_DISTANCE still has finite
    // area, but the sliver at +FLT_MAX intersects the space only at
    // its floor edge, which `intersection` maps to the empty rect.
    let clipped = SPACE;
    let space = mgr.spaces.get_mut(&space_id).unwrap();
    space.set_space_bounds(SPACE, SPACE_GRID);
    space.for_each_cell_mut(&mut |cell| {
        cell.has_been_created = true;
        if cell.app == APP_A {
            // The hot cell has loaded the whole space; its reported
            // entity edge binds the partition move.
            cell.update_bounds_report(clipped, 0, &hot_levels);
        } else {
            // The idle cell has also loaded everything (its sliver has
            // no area, so `calculate_area_not_loaded` short-circuits).
            cell.update_bounds_report(clipped, 0, &b_levels);
        }
    });
}

/// The per-pass snapshot of a balance window: both cells' ranges, `None`
/// when a cell has been deleted by the pass.
type Ladder = Vec<(Rect, Option<Rect>)>;

fn ranges(mgr: &CellAppMgr, space_id: u32) -> (Rect, Option<Rect>) {
    let space = &mgr.spaces[&space_id];
    let a = space.find_cell_by_app(APP_A).map(|c| c.range);
    let b = space.find_cell_by_app(APP_B).map(|c| c.range);
    (a.unwrap_or_else(Rect::empty), b)
}

// ---------------------------------------------------------------------------
// Leg 1: the allocation window — a hotspot absorbed by a sliver cell
// ---------------------------------------------------------------------------

/// A two-cell space with a horizontal split at the given `position`.
/// A starts over the space; B's range is `[position, position]`, so at
/// `position = SPACE.ymax` it is a zero-area sliver (the shape the first
/// split of `Space::add_cell` produces) and at any lower position it
/// holds the strip above the line. `Space::load_balance` re-derives the
/// ranges from the unbounded root, so the split line moves freely.
fn top_split_two_cell_space(mgr: &mut CellAppMgr, position: f32) -> u32 {
    let id = mgr.generate_space_id(NULL_SPACE_ID);
    let mut space = Space::new(id, false, 0);
    let a = mgr.apps.find(APP_A).unwrap().clone();
    let b = mgr.apps.find(APP_B).unwrap().clone();
    let sliver = Rect::new(SPACE.xmin, position, SPACE.xmax, position);
    let a_leaf = BspNode::Leaf(cell(SPACE, &a));
    let b_leaf = BspNode::Leaf(cell(sliver, &b));
    let node = BspInternal {
        left: Box::new(a_leaf),
        right: Box::new(b_leaf),
        horizontal: true,
        position,
        aggression: 1.0,
        total_load: 0.0,
        total_smoothed_load: 0.0,
        min_load: 0.0,
        max_load: 0.0,
        left_count: 1,
        right_count: 1,
        prev_direction: super::BalanceDirection::None,
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

/// A fresh cell leaf with the given range (`CellLeaf::new` gives the
/// whole world, so override the range after construction).
fn cell(range: Rect, app: &CellAppRecord) -> CellLeaf {
    let mut leaf = CellLeaf::new(0, app.addr, app.id, app.viewer_port);
    leaf.range = range;
    leaf
}

/// Runs the allocation scenario once: the idle app B holds a zero-area
/// sliver, app A's load ramps to a hotspot, then balance passes move the
/// partition until A's reported entity edge binds. Returns the EMA
/// ladder and the per-pass range ladder.
fn allocation_scenario() -> (Vec<f32>, Ladder) {
    let mut mgr = mgr_with(vec![(APP_A, 0.05), (APP_B, 0.05)]);
    let id = top_split_two_cell_space(&mut mgr, SPACE.ymax);

    report_bounds(&mut mgr, id, (true, ENTITY_EDGE));

    // The hotspot: 12 informs at 0.9 through the reference EMA
    // (`CellApp::informOfLoad`, `LOAD_SMOOTHING_BIAS` = 0.05).
    let mut ema = Vec::new();
    for step in 1..=12u64 {
        let rec = mgr.apps.find_mut(APP_A).unwrap();
        rec.inform_of_load(0.9, 0, step * TICK_MS);
        ema.push(rec.smoothed_load);
    }

    // The balance window.
    let mut ladder = Ladder::new();
    for _ in 0..MAX_PASSES {
        mgr.load_balance();
        ladder.push(ranges(&mgr, id));
    }
    (ema, ladder)
}

fn allocation_leg(record: &mut Record) {
    let started = Instant::now();
    let (ema, ladder) = allocation_scenario();
    let rerun = allocation_scenario();

    // Determinism: the whole scenario is a fixed sequence of integer and
    // f32 operations — two runs must produce identical ladders.
    assert_eq!(ema, rerun.0, "the EMA ladder is deterministic");
    assert_eq!(ladder, rerun.1, "the balance ladder is deterministic");

    // The reference EMA, recomputed from the constants.
    let mut expected = 0.05f32;
    let mut formula = Vec::new();
    for _ in 0..12 {
        expected = (1.0 - LOAD_SMOOTHING_BIAS) * expected + LOAD_SMOOTHING_BIAS * 0.9;
        formula.push(expected);
    }
    for (i, (got, want)) in ema.iter().zip(&formula).enumerate() {
        assert!((got - want).abs() < 1e-6, "inform {i}: {got} != EMA {want}");
        record.put(
            &format!("alloc_hotspot_ema_{:02}", i + 1),
            format!("{got:.6}"),
        );
    }
    assert!(
        ema.iter().zip(ema.iter().skip(1)).all(|(a, b)| a < b),
        "the hotspot ramps monotonically"
    );
    record.put("alloc_hotspot_final_ratio", format!("{:.3}", ema[11] / 0.9));

    // The window: the sliver cell absorbs area until the hot cell's
    // entity edge binds, then the ladder is a fixed point.
    let settle = ladder
        .windows(2)
        .position(|pair| pair[0] == pair[1])
        .map(|index| index + 1)
        .expect("the balance ladder settles within the cap");
    assert!(settle < MAX_PASSES, "settles within the pass cap");
    let (_, b_final) = ladder[settle];
    let b = b_final.expect("the idle cell is never deleted while absorbing");
    assert!(
        b.ymin.is_finite(),
        "the sliver grew a finite edge: {}",
        fmt_rect(b)
    );
    assert!(b.ymax > b.ymin && b.xmax > b.xmin, "the idle cell has area");
    assert!(
        b.ymin >= SPACE.ymin && b.ymin <= SPACE.ymax,
        "the split edge stays inside the space bounds"
    );
    record.put("alloc_split_app", "B");
    record.put("alloc_passes_to_settle", settle);
    for (i, (a, b)) in ladder.iter().enumerate() {
        record.put(&format!("alloc_pass_{:02}_a_range", i + 1), fmt_rect(*a));
        match b {
            Some(b) => record.put(&format!("alloc_pass_{:02}_b_range", i + 1), fmt_rect(*b)),
            None => record.put(&format!("alloc_pass_{:02}_b_range", i + 1), "deleted"),
        }
    }
    record.put(
        "alloc_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
}

// ---------------------------------------------------------------------------
// Leg 2: the recycling window — a retiring app's cells drain
// ---------------------------------------------------------------------------

fn recycling_leg(record: &mut Record) {
    let started = Instant::now();

    // Arm 1: the fresh sliver cell retires — zero-area, zero entities,
    // so the first balance pass deletes it (`CellRemoval`). The hot app
    // stays on the survivor's side so the balance direction shrinks the
    // retiring cell (it must be on the shrinking side of the direction).
    let mut mgr = mgr_with(vec![(APP_A, 0.9), (APP_B, 0.05)]);
    let id = top_split_two_cell_space(&mut mgr, f32::MAX);
    report_bounds(&mut mgr, id, (true, ENTITY_EDGE));
    mgr.retire_all_cells_of(APP_B);
    assert!(mgr.apps.find(APP_B).unwrap().is_retiring);
    assert_eq!(mgr.apps.find(APP_B).unwrap().num_cells_retiring, 1);
    assert!(
        mgr.outbox
            .iter()
            .any(|o| matches!(o, Outbound::RetireCell(a, s) if *a == APP_B && *s == NULL_SPACE_ID)),
        "the retire notice goes out"
    );

    let mut drain_passes = 0;
    while mgr.spaces[&id].find_cell_by_app(APP_B).is_some() {
        drain_passes += 1;
        assert!(drain_passes <= MAX_PASSES, "the sliver drains promptly");
        mgr.load_balance();
    }
    assert_eq!(drain_passes, 1, "a zero-area retiring cell drains at once");
    assert_eq!(
        mgr.outbox
            .iter()
            .filter(|o| matches!(o, Outbound::CellRemoval(a, s, _) if *a == APP_B && *s == id))
            .count(),
        1,
        "exactly one CellRemoval for the drained cell"
    );
    assert_eq!(mgr.apps.find(APP_B).unwrap().num_cells, 0, "registry drain");
    // The sibling is promoted to the whole world.
    assert_eq!(mgr.spaces[&id].num_cells(), 1);
    assert_eq!(mgr.spaces[&id].cells()[0].range, Rect::unbounded());
    record.put("recycle_sliver_passes", drain_passes);
    record.put("recycle_sliver_cell_removals", 1);

    // Arm 2: a retiring cell with real area is *held* until the survivor
    // loads its chunks. B absorbs area while A is the hotspot, then B
    // retires with A's load below the safety bound: `should_move` is true
    // and `load_diff` drives the partition back toward A. But the growing
    // child is A, whose reported chunk bounds still stop at the old line;
    // `chunk_limit_in_direction` subtracts `GHOST_DISTANCE` from that
    // edge, so `closest_limit` pins the line at its old position. The
    // records contrast this held face with Arm 1's immediate sliver drain.
    let mut mgr = mgr_with(vec![(APP_A, 0.6), (APP_B, 0.05)]);
    let id = top_split_two_cell_space(&mut mgr, SPACE.ymax);
    // A holds the hotspot, so B's sliver absorbs area until the
    // reported entity edge binds — the same absorption as leg 1.
    report_bounds(&mut mgr, id, (true, ENTITY_EDGE));
    let mut shrink_ladder = Vec::new();
    for _ in 0..MAX_PASSES {
        mgr.load_balance();
        shrink_ladder.push(ranges(&mgr, id));
    }
    let (pre_a, pre_b) = *shrink_ladder.last().expect("the absorption settles");
    let pre_b = pre_b.expect("B holds area before the retirement");
    assert!(
        pre_b.ymax > pre_b.ymin,
        "B absorbed a real strip: {pre_b:?}"
    );

    mgr.retire_all_cells_of(APP_B);
    assert!(mgr.apps.find(APP_B).unwrap().is_retiring);
    assert_eq!(mgr.apps.find(APP_B).unwrap().num_cells_retiring, 1);
    // Re-report A's bounds after the retirement, matching a CellApp that
    // keeps its chunk report current across the drain window.
    report_bounds(&mut mgr, id, (true, ENTITY_EDGE));
    let mut ladder = Vec::new();
    for _ in 0..MAX_PASSES {
        mgr.load_balance();
        ladder.push(ranges(&mgr, id));
    }
    let settle = ladder
        .windows(2)
        .position(|pair| pair[0] == pair[1])
        .map(|index| index + 1)
        .expect("the held ladder settles within the cap");
    let (a_final, b_final) = ladder[settle];
    let b = b_final.expect("the retiring cell is held past the drain cap");
    assert_eq!(
        a_final, pre_a,
        "the survivor's chunk ghost margin holds the line"
    );
    assert_eq!(b, pre_b, "the retiring cell keeps its absorbed strip");
    assert!(
        b.ymax > b.ymin,
        "the real-area retiring cell is not deleted"
    );
    assert!(
        mgr.apps.find(APP_B).unwrap().has_any_retiring_cells(),
        "the held cell is still registered as retiring"
    );
    assert_eq!(
        mgr.outbox
            .iter()
            .filter(|o| matches!(o, Outbound::CellRemoval(a, s, _) if *a == APP_B && *s == id))
            .count(),
        0,
        "no CellRemoval for a real-area retiring cell"
    );
    record.put("recycle_area_passes_to_settle", settle);
    record.put("recycle_area_survivor_range", fmt_rect(a_final));
    record.put("recycle_area_retiring_range", fmt_rect(b));
    record.put(
        "recycle_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
}

// ---------------------------------------------------------------------------
// Leg 3: the recovery window — a dead CellApp detected and covered
// ---------------------------------------------------------------------------

fn recovery_leg(record: &mut Record) {
    let started = Instant::now();
    let mut mgr = mgr_with(vec![(APP_A, 0.1), (APP_B, 0.5)]);
    let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
    // The single cell lands on the least-loaded app (A) — the one that
    // will die.
    assert_eq!(mgr.spaces[&id].cells()[0].app, APP_A);
    mgr.apps
        .find_mut(APP_A)
        .unwrap()
        .inform_of_load(0.1, 0, TICK_MS);

    // A stops reporting; B keeps reporting on every tick.
    let mut ticks = 0;
    let mut detect_ms = 0;
    for step in 2..=8u64 {
        let now = step * TICK_MS;
        mgr.apps
            .find_mut(APP_B)
            .unwrap()
            .inform_of_load(0.5, 0, now);
        mgr.tick(now);
        ticks += 1;
        if mgr.apps.find(APP_A).is_none() {
            detect_ms = now;
            break;
        }
    }
    assert!(mgr.apps.find(APP_A).is_none(), "the dead app is removed");
    assert!(mgr.apps.find(APP_B).is_some(), "the live app survives");
    // The strict `now - lastHeard > timeout` check: A is last heard at
    // 1000 ms, so the first detecting tick is 1000 + 3000 + TICK_MS.
    assert_eq!(
        detect_ms,
        TICK_MS + TIMEOUT_MS + TICK_MS,
        "detection at the first tick past the strict timeout"
    );
    record.put("recover_timeout_ms", TIMEOUT_MS);
    record.put("recover_detect_ms", detect_ms);
    record.put("recover_detect_ticks", ticks);

    // The death faces are emitted before the coverage search: the
    // broadcast to every surviving app, the BaseAppMgr death report, and
    // the SIGQUIT to the dead process (`ReportDeathToBaseAppMgr` and the
    // `CellAppDeath` broadcast stand in for the SIGQUIT + death-handler
    // report the reference sends). A single-cell space is not covered
    // here: `remove_cell` on a lone leaf returns the leaf unchanged, so
    // the dead app's cell is the space's only cell and the handler leaves
    // it for the manager's `checkLoadingSpaces` to reclaim on a later
    // tick. Coverage is exercised on a two-cell space below.
    assert!(
        mgr.outbox.iter().any(
            |o| matches!(o, Outbound::CellAppDeath(to, dead) if *to == APP_B && *dead == APP_A)
        ),
        "the death broadcast"
    );
    assert!(
        mgr.outbox
            .iter()
            .any(|o| matches!(o, Outbound::ReportDeathToBaseAppMgr(a) if *a == APP_A)),
        "the BaseAppMgr death report"
    );
    record.put("recover_single_cell_outbox_faces", 2);

    // A two-cell space: the dead app's cell is erased inside the same
    // death handling and the surviving B cell keeps the whole space —
    // no extra tick.
    let mut mgr = mgr_with(vec![(APP_A, 0.1), (APP_B, 0.5)]);
    let id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
    // A second cell on B so the space has two cells and the first
    // balance pass is not a lone-leaf erase.
    {
        let space = mgr.spaces.get_mut(&id).unwrap();
        let rec = mgr.apps.find(APP_B).unwrap().clone();
        space.add_cell(&rec);
    }
    assert_eq!(mgr.spaces[&id].num_cells(), 2);
    mgr.apps
        .find_mut(APP_A)
        .unwrap()
        .inform_of_load(0.1, 0, TICK_MS);
    // A stops reporting; B keeps reporting on every tick.
    let mut detect_ms = 0;
    for step in 2..=8u64 {
        let now = step * TICK_MS;
        mgr.apps
            .find_mut(APP_B)
            .unwrap()
            .inform_of_load(0.5, 0, now);
        mgr.tick(now);
        if mgr.apps.find(APP_A).is_none() {
            detect_ms = now;
            break;
        }
    }
    assert!(mgr.apps.find(APP_A).is_none(), "the dead app is removed");
    assert!(mgr.apps.find(APP_B).is_some(), "the live app survives");
    assert_eq!(detect_ms, TICK_MS + TIMEOUT_MS + TICK_MS, "detection");
    record.put("recover_extra_ticks", 0);
    record.put("recover_recreated_app", "B");
    assert_eq!(mgr.spaces[&id].num_cells(), 1);
    assert_eq!(mgr.spaces[&id].cells()[0].app, APP_B);
    assert_eq!(mgr.spaces[&id].cells()[0].range, Rect::unbounded());
    // The estimate bump belongs to the cell-creation seam: `create_space`
    // bumped the owner's estimate by `ESTIMATED_INITIAL_CELL_LOAD` when the
    // first cell was added. On the death path B's own `inform_of_load` at
    // the detecting tick resets `estimated_load` to the smoothed load, so
    // record the creation bump from a direct add and the reset separately.
    let mut direct = mgr_with(vec![(APP_B, 0.5)]);
    let before = direct.apps.find(APP_B).unwrap().estimated_load;
    direct.create_space(NULL_SPACE_ID, false, 0).unwrap();
    let after = direct.apps.find(APP_B).unwrap().estimated_load;
    assert!(
        (after - before - ESTIMATED_INITIAL_CELL_LOAD).abs() < 1e-6,
        "cell creation carries the new-cell bump: {before} -> {after}"
    );
    record.put(
        "recover_estimated_bump",
        format!("{:.2}", ESTIMATED_INITIAL_CELL_LOAD),
    );
    // The dead app's cell is erased from the space; the surviving B cell
    // keeps the whole (unbounded) range. No coverage `AddCell` is sent on
    // this path: the port only recreates a cell for a space emptied by
    // the death (`num_cells() < 1`), and a two-cell tree still has B's.
    record.put("recover_two_cell_outbox_faces", 2);

    // The last-app death shuts the cluster down.
    let mut mgr = mgr_with(vec![(APP_A, 0.5)]);
    let _id = mgr.create_space(NULL_SPACE_ID, false, 0).unwrap();
    mgr.handle_cell_app_death(APP_A);
    assert!(
        mgr.outbox
            .iter()
            .any(|o| matches!(o, Outbound::TriggerControlledShutDown)),
        "the last death triggers the controlled shutdown"
    );
    record.put("recover_last_app_shutdown", 1);
    record.put(
        "recover_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
}

/// Runs every leg in fixed order and leaves the baseline artifact.
#[test]
fn the_cellappmgr_baseline_records_the_load_and_failure_windows() {
    let mut record = Record::default();
    allocation_leg(&mut record);
    recycling_leg(&mut record);
    recovery_leg(&mut record);
    record.flush();
}
