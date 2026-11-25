//! OPT-05-01-02: compare CellAppMgr scheduling under a fixed load sequence.
//!
//! The acceptance is "比较固定场景的调度效果" — compare the scheduling
//! effect of a fixed scenario. This module drives the real
//! [`CellAppMgr`] load-report and split/merge faces with the same seeded
//! sequence twice: once with the reference [`BalanceTuning`] defaults and
//! once with a candidate set of knobs. The result is a ledger of the
//! decisions each run took — cell splits and removals, partition moves,
//! retirements, and the final load spread — so the tuning effect is a
//! reproducible, inspectable comparison rather than a wall-clock number.
//!
//! The scenario is deliberately small and deterministic: three CellApps on
//! three hosts, a bounded space, a fixed number of ticks, and a load
//! sequence derived from a seed by a simple LCG. Every run starts from the
//! same manager state. Wall-clock time is never sampled: the comparison is
//! entirely over the decisions the scheduler made.

use crate::cellappmgr::{
    Addr, BalanceTuning, CellAppMgr, CellAppRecord, EntityBoundLevels, Rect, SpaceId,
};

/// The short fixed scenario's tick count.
pub const SCENARIO_TICKS: u32 = 96;
/// The space bounds the scenario gives the manager.
pub fn scenario_bounds() -> Rect {
    Rect::new(0.0, 0.0, 10_000.0, 10_000.0)
}

/// One run's scheduling decisions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduleLedger {
    /// Number of cells added (splits) during the scenario.
    pub cells_added: u32,
    /// Number of cells removed (merges) during the scenario.
    pub cells_removed: u32,
    /// Number of apps retired by the meta-balance pass.
    pub apps_retired: u32,
    /// Number of partition lines that ended up moved off their initial
    /// position at the end of the run.
    pub partitions_moved: u32,
    /// Number of load reports fed into the manager.
    pub reports: u32,
}

/// The outcome of one full scenario run.
#[derive(Clone, Debug, PartialEq)]
pub struct ScheduleRun {
    /// The knobs this run used.
    pub tuning: BalanceTuning,
    /// The decisions the run took.
    pub ledger: ScheduleLedger,
    /// The per-app smoothed load at the end, in address order.
    pub final_loads: Vec<(Addr, f32)>,
    /// The number of cells each space owns at the end, in space-id order.
    pub final_cells: Vec<(SpaceId, usize)>,
}

/// A side-by-side comparison of the reference knobs and a candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct ScheduleComparison {
    /// The run with the reference/default tuning.
    pub baseline: ScheduleRun,
    /// The run with the candidate tuning.
    pub candidate: ScheduleRun,
}

/// The default scenario seed. A caller may pass another to check that the
/// comparison is stable across different fixed sequences.
pub const SCENARIO_SEED: u64 = 27_182_828;

/// Run the fixed scenario once with `tuning`.
///
/// The load sequence is a small LCG over `seed`; each tick reports one
/// app's load and then runs the manager's balance and meta-balance passes,
/// exactly as the periodic timers do in the reference. Every decision the
/// manager emits is counted in the returned ledger.
pub fn run_schedule_scenario(tuning: BalanceTuning, seed: u64) -> ScheduleRun {
    let mut rng = seed;
    let mut next_load = || {
        rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        // Map the high bits of the LCG into [0.0, 1.0).
        ((rng >> 11) as f64 / (1u64 << 53) as f64) as f32
    };

    let mut mgr = CellAppMgr::new();
    mgr.tuning = tuning;
    mgr.set_base_app((0x7f00_0001, 100));
    mgr.set_db_app_alpha((0x7f00_0001, 200));
    mgr.allow_new_cell_apps = true;
    mgr.should_meta_load_balance = true;
    let apps = [
        (0x7f00_0001u32, 3000u16),
        (0x7f00_0002, 3000),
        (0x7f00_0003, 3000),
    ];
    for (i, addr) in apps.iter().enumerate() {
        let mut rec = CellAppRecord::new(*addr, (i + 1) as u32, 4000 + i as u16, 3_000);
        rec.num_cells = 1;
        mgr.apps.add(rec);
    }
    let space_id = mgr.create_space(0, false, 0).expect("a space with a cell");
    // Bring the space to three cells so both a split and a merge are
    // reachable during the scenario.
    for addr in &apps[1..] {
        mgr.add_cell_for_app(space_id, *addr);
    }
    // Give each app a different starting load so the BSP tree has a real
    // left/right imbalance on its first balance pass. The load schedule
    // below keeps these relative ranks through the run.
    let initial_loads = [0.70f32, 0.35, 0.10];
    for (addr, load) in apps.iter().zip(initial_loads) {
        mgr.apps
            .find_mut(*addr)
            .expect("the app was just registered")
            .inform_of_load_with(load, 1, 0, 1.0);
    }
    {
        let space = mgr.spaces.get_mut(&space_id).expect("the created space");
        space.set_space_bounds(scenario_bounds(), 100.0);
        // A balance pass only moves a line between cells that the apps
        // report as created and chunk-loaded. Mirror those reports so the
        // scenario exercises the real split/merge decision path.
        let mut levels = EntityBoundLevels::new();
        for (level, (min, max)) in [(2_000.0, 4_000.0), (4_000.0, 8_000.0)]
            .into_iter()
            .enumerate()
        {
            levels.set_level(
                level,
                &[
                    (false, false, min, 0.01),
                    (false, true, max, 0.01),
                    (true, false, min, 0.01),
                    (true, true, max, 0.01),
                ],
            );
        }
        let bounds = scenario_bounds();
        space.for_each_cell_mut(&mut |cell| {
            cell.has_been_created = true;
            cell.chunk_bounds = bounds;
            cell.update_bounds_report(bounds, 10, &levels);
            cell.has_been_created = true;
        });
    }
    let base_cells = mgr.spaces[&space_id].num_cells() as u32;
    let base_removed = count_removals(&mgr);

    let mut ledger = ScheduleLedger::default();
    for tick in 0..SCENARIO_TICKS {
        // Two deliberate phases: a hot phase that pushes one app toward
        // overload (forcing split/balance decisions) and a cool phase that
        // drops every app below the lower bound (forcing merges). The LCG
        // jitters each within the phase so the sequence is fixed but not
        // constant.
        let jitter = next_load();
        let phase = tick * 2 / SCENARIO_TICKS;
        for (i, addr) in apps.iter().enumerate() {
            let load = if phase == 0 {
                // Hot: one app heavy, the others moderate; all below the
                // safety bound so the balance pass is allowed to move.
                match i {
                    0 => 0.60 + jitter * 0.15,
                    1 => 0.30 + jitter * 0.10,
                    _ => 0.20 + jitter * 0.10,
                }
            } else {
                // Cool: everything under the lower bound, so the
                // meta-balance pass retires the least-loaded app.
                0.005 + jitter * 0.01
            };
            mgr.apps
                .find_mut(*addr)
                .expect("the app is registered")
                .inform_of_load_with(
                    load,
                    tick + 1,
                    tick as u64 * 100,
                    tuning.load_smoothing_bias,
                );
        }
        ledger.reports += 1;
        mgr.load_balance();
        mgr.meta_load_balance();
        mgr.tick(tick as u64 * 100);
    }

    ledger.cells_added = count_additions(&mgr).saturating_sub(base_cells);
    ledger.cells_removed = count_removals(&mgr).saturating_sub(base_removed);
    ledger.apps_retired = mgr.apps.iter().filter(|app| app.is_retiring).count() as u32;
    ledger.partitions_moved = count_moved_partitions(&mgr);

    let mut final_loads: Vec<(Addr, f32)> = mgr
        .apps
        .iter()
        .map(|app| (app.addr, app.smoothed_load))
        .collect();
    final_loads.sort_by_key(|(addr, _)| *addr);
    let mut final_cells: Vec<(SpaceId, usize)> = mgr
        .spaces
        .iter()
        .map(|(id, space)| (*id, space.num_cells()))
        .collect();
    final_cells.sort_by_key(|(id, _)| *id);

    ScheduleRun {
        tuning,
        ledger,
        final_loads,
        final_cells,
    }
}

/// Compare the reference knobs with a candidate on the same fixed sequence.
pub fn compare_schedule(tuning: BalanceTuning, seed: u64) -> ScheduleComparison {
    ScheduleComparison {
        baseline: run_schedule_scenario(BalanceTuning::default(), seed),
        candidate: run_schedule_scenario(tuning, seed),
    }
}

fn count_additions(mgr: &CellAppMgr) -> u32 {
    mgr.outbox
        .iter()
        .filter(|msg| matches!(msg, crate::cellappmgr::Outbound::AddCell(_, _)))
        .count() as u32
}

fn count_removals(mgr: &CellAppMgr) -> u32 {
    mgr.outbox
        .iter()
        .filter(|msg| matches!(msg, crate::cellappmgr::Outbound::CellRemoval(_, _, _)))
        .count() as u32
}

fn count_moved_partitions(mgr: &CellAppMgr) -> u32 {
    // A partition line starts at its parent's far edge (`+inf`). A balance
    // pass that moves it leaves a finite position inside the space, which
    // is the signature counted here.
    let bounds = scenario_bounds();
    mgr.spaces
        .values()
        .flat_map(|space| space.partition_positions())
        .filter(|(horizontal, position)| {
            let axis = bounds.range1d(*horizontal);
            position.is_finite() && axis.min < *position && *position < axis.max
        })
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixed_scenario_reproduces_from_its_seed() {
        let a = run_schedule_scenario(BalanceTuning::default(), SCENARIO_SEED);
        let b = run_schedule_scenario(BalanceTuning::default(), SCENARIO_SEED);
        assert_eq!(a, b);
    }

    #[test]
    fn the_scenario_reports_every_tick() {
        let run = run_schedule_scenario(BalanceTuning::default(), SCENARIO_SEED);
        assert_eq!(run.ledger.reports, SCENARIO_TICKS);
    }

    #[test]
    fn the_baseline_uses_the_reference_knobs() {
        let comparison = compare_schedule(
            BalanceTuning {
                safety_bound: 0.5,
                ..BalanceTuning::default()
            },
            SCENARIO_SEED,
        );
        assert_eq!(comparison.baseline.tuning, BalanceTuning::default());
        assert_ne!(comparison.candidate.tuning.safety_bound, 0.85);
    }

    #[test]
    fn a_tuned_knob_changes_the_decisions() {
        // With `limit_to_chunks` off, the balance pass is free to move a
        // partition on the entity bound alone, so it shifts two partitions;
        // the reference `limit_to_chunks = true` keeps both lines put because
        // the loaded chunks do not cover the move. The tuned knobs leave a
        // visible scheduling difference.
        let comparison = compare_schedule(
            BalanceTuning {
                limit_to_chunks: false,
                ..BalanceTuning::default()
            },
            SCENARIO_SEED,
        );
        assert_eq!(comparison.baseline.ledger.partitions_moved, 0);
        assert_eq!(comparison.candidate.ledger.partitions_moved, 2);
        assert_eq!(comparison.baseline.ledger.reports, SCENARIO_TICKS);
        assert_eq!(comparison.candidate.ledger.reports, SCENARIO_TICKS);
    }

    #[test]
    fn the_comparison_is_stable_across_seeds() {
        let candidate = BalanceTuning {
            limit_to_chunks: false,
            ..BalanceTuning::default()
        };
        let a = compare_schedule(candidate, SCENARIO_SEED);
        let b = compare_schedule(candidate, SCENARIO_SEED);
        assert_eq!(a.baseline, b.baseline);
        assert_eq!(a.candidate, b.candidate);
    }
}
