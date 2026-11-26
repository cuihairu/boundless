//! The PR smoke face (BW-17-03-05): drive a saved scenario through the
//! offline faces at the harness's low-load envelope and print the harness's
//! `metric<TAB>value` lines.
//!
//! Contract (`ci/load.sh`): a scenario is an executable under
//! `tests/load/scenarios/` that runs with `LOAD_DURATION` (wall budget,
//! seconds) and `LOAD_CLIENTS` (concurrency) and prints one
//! `metric<TAB>value` line per metric on stdout; it passes when it exits 0
//! inside its budget having printed at least one metric line.
//!
//! The offline faces are simulated time — a run costs milliseconds of wall,
//! so the envelope is a cap on the saved scenario's scale, not a cost. The
//! caps exist so the nightly-scale saved scenario can also run in the PR
//! gate at a scale a PR runner affords, and the same fixed seed reproduces
//! the run bit for bit: [`run_smoke`] runs the face twice and refuses to
//! report a run whose two passes disagree, which is what makes a failure
//! re-runnable — the same input gives the same numbers.

use std::fmt;
use std::time::Duration;

use crate::bot::FD_PER_BOT;
use crate::cross_cell::{CrossingLedger, run_cross_cell};
use crate::dense::{DenseAoiLedger, run_dense_aoi};
use crate::hurricane::{HurricaneLedger, run_login_hurricane};
use crate::long_stability::run_long_stability;
use crate::reconnect::{ReconnectLedger, run_reconnect};
use crate::scenario::Scenario;
use crate::steady::{SteadyLedger, run_steady_movement};

/// The low-load envelope the smoke runs under (`ci/load.sh`'s
/// `LOAD_CLIENTS`/`LOAD_DURATION`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmokeCaps {
    /// The concurrency cap (`LOAD_CLIENTS`).
    pub clients: u64,
    /// The wall-budget cap the scenario duration is clamped to
    /// (`LOAD_DURATION`, seconds).
    pub duration: Duration,
}

/// One recorded metric: the harness's `metric<TAB>value` line, typed.
#[derive(Clone, Debug, PartialEq)]
pub enum MetricValue {
    /// A whole-number metric.
    Int(i64),
    /// A single-precision metric — the faces' own float, printed in Rust's
    /// shortest round-trip form (`0.1` for the mini-tick, not its f64
    /// widening).
    F32(f32),
    /// A fractional metric, printed in Rust's shortest round-trip form.
    Float(f64),
}

impl fmt::Display for MetricValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MetricValue::Int(v) => write!(f, "{v}"),
            MetricValue::F32(v) => write!(f, "{v}"),
            MetricValue::Float(v) => write!(f, "{v}"),
        }
    }
}

/// Why a scenario cannot be smoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmokeError {
    /// The scenario's kind names no offline run face (an unknown kind,
    /// or one whose face is not delivered yet).
    Unsupported {
        /// The scenario name that named no known kind.
        name: String,
    },
    /// Two passes over the same fixed seed disagreed — the run is not
    /// reproducible, so a failure would not re-run the same way.
    Nondeterministic {
        /// The scenario name that would not reproduce.
        name: String,
    },
}

impl fmt::Display for SmokeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmokeError::Unsupported { name } => {
                write!(f, "scenario kind {name:?} has no offline run face yet")
            }
            SmokeError::Nondeterministic { name } => write!(
                f,
                "two runs of {name:?} disagreed, so a failure would not re-run the same way"
            ),
        }
    }
}

impl std::error::Error for SmokeError {}

/// The run face a scenario kind dispatches to.
#[derive(Clone, Debug, PartialEq)]
pub enum SmokeRun {
    /// The login hurricane's ledger.
    Hurricane(HurricaneLedger),
    /// The steady-movement run's record.
    Steady(SteadyLedger),
    /// The dense AOI run's record.
    Dense(DenseAoiLedger),
    /// The cross-cell run's record.
    CrossCell(CrossingLedger),
    /// The reconnect run's record.
    Reconnect(ReconnectLedger),
    /// The long-stability run's record.
    LongStability(crate::long_stability::LongStabilityLedger),
}

/// Clamp a saved scenario to the smoke envelope: the concurrency is the
/// smaller of the saved scale and `clients`, the duration the smaller of
/// the saved window and the budget. The seed, pool and ramp shape are the
/// saved scenario's own — they are what makes a re-run comparable.
pub fn cap_scenario(scenario: &Scenario, caps: &SmokeCaps) -> Scenario {
    let mut capped = scenario.clone();
    capped.concurrency = scenario.concurrency.min(caps.clients.max(1));
    capped.duration = scenario.duration.min(caps.duration);
    capped
}

/// Run the scenario's kind twice off the same fixed seed and compare: the
/// ledgers must be equal, or the run is refused as unreproducible. The
/// capped scenario comes back so the caller can envelope it in a report.
pub fn run_smoke(
    scenario: &Scenario,
    caps: &SmokeCaps,
) -> Result<(Scenario, SmokeRun), SmokeError> {
    smoke_with_face(scenario, caps, run_face)
}

/// [`run_smoke`] over an injected face — the shape the reproducibility
/// check is tested through.
fn smoke_with_face<F>(
    scenario: &Scenario,
    caps: &SmokeCaps,
    mut face: F,
) -> Result<(Scenario, SmokeRun), SmokeError>
where
    F: FnMut(&Scenario) -> Result<SmokeRun, SmokeError>,
{
    let capped = cap_scenario(scenario, caps);
    let first = face(&capped)?;
    let second = face(&capped)?;
    if first != second {
        return Err(SmokeError::Nondeterministic {
            name: capped.name.clone(),
        });
    }
    Ok((capped, first))
}

/// Dispatch the scenario kind to its offline run face. The fd budget is
/// sized to the capped concurrency so an offline run stays refusal-free —
/// refusals are the live FD budget's to hand out.
pub fn run_face(scenario: &Scenario) -> Result<SmokeRun, SmokeError> {
    let fd_limit = FD_PER_BOT
        .saturating_mul(scenario.concurrency.max(1) as i32)
        .saturating_add(FD_PER_BOT);
    match scenario.name.as_str() {
        "login-hurricane" => Ok(SmokeRun::Hurricane(run_login_hurricane(
            scenario,
            fd_limit,
            scenario.fixed_seed,
        ))),
        "steady-move" => Ok(SmokeRun::Steady(run_steady_movement(
            scenario,
            fd_limit,
            scenario.fixed_seed,
        ))),
        "dense-aoi" => Ok(SmokeRun::Dense(run_dense_aoi(scenario, 5.0, 25.0))),
        "cross-cell" => Ok(SmokeRun::CrossCell(run_cross_cell(
            scenario,
            fd_limit,
            scenario.fixed_seed,
        ))),
        "reconnect" => Ok(SmokeRun::Reconnect(run_reconnect(
            scenario,
            fd_limit,
            scenario.fixed_seed,
        ))),
        "long-stability" => Ok(SmokeRun::LongStability(run_long_stability(
            scenario,
            fd_limit,
            scenario.fixed_seed,
        ))),
        other => Err(SmokeError::Unsupported {
            name: other.to_string(),
        }),
    }
}

/// The run's metrics — every line the harness records, derived from the
/// run's ledger so a re-run of the same seed records the same numbers.
pub fn smoke_metrics(run: &SmokeRun) -> Vec<(String, MetricValue)> {
    let int = |v: i64| MetricValue::Int(v);
    match run {
        SmokeRun::Hurricane(ledger) => {
            let attempts = ledger.total_sends() as i64;
            let admitted = ledger.names.len() as i64;
            let refused = ledger.refused as i64;
            let success_rate = if attempts == 0 {
                0.0
            } else {
                admitted as f64 / attempts as f64
            };
            let online_peak = ledger.online.iter().copied().max().unwrap_or(0);
            let online_final = ledger.online.last().copied().unwrap_or(0);
            vec![
                ("login_attempts".to_string(), int(attempts)),
                ("login_admitted".to_string(), int(admitted)),
                ("login_refused".to_string(), int(refused)),
                (
                    "login_success_rate".to_string(),
                    MetricValue::Float(success_rate),
                ),
                ("online_peak".to_string(), int(online_peak as i64)),
                ("online_final".to_string(), int(online_final as i64)),
            ]
        }
        SmokeRun::Steady(ledger) => {
            // The trajectory's coordinate sum is the machine-checkable
            // fingerprint of "same seed, same walk": a bot off its circle
            // moves the sum.
            let mut trajectory_sum = 0.0f64;
            for track in &ledger.tracks {
                trajectory_sum += track.xs.iter().sum::<f32>() as f64;
                trajectory_sum += track.zs.iter().sum::<f32>() as f64;
            }
            vec![
                ("bots_online".to_string(), int(ledger.tracks.len() as i64)),
                (
                    "sample_period".to_string(),
                    MetricValue::F32(ledger.sample_period),
                ),
                (
                    "trajectory_sum".to_string(),
                    MetricValue::Float(trajectory_sum),
                ),
            ]
        }
        SmokeRun::Dense(ledger) => {
            let visible_total: u64 = ledger.visible_counts.iter().map(|&v| v as u64).sum();
            let bots = ledger.positions.len().max(1) as f64;
            let avg_visible = visible_total as f64 / bots;
            let max_visible = ledger.visible_counts.iter().copied().max().unwrap_or(0);
            vec![
                ("bots".to_string(), int(ledger.positions.len() as i64)),
                ("density".to_string(), MetricValue::F32(ledger.density)),
                ("avg_visible".to_string(), MetricValue::Float(avg_visible)),
                ("max_visible".to_string(), int(max_visible as i64)),
            ]
        }
        SmokeRun::CrossCell(ledger) => {
            let set_out = ledger.tracks.len() as i64;
            vec![
                ("bots_set_out".to_string(), int(set_out)),
                (
                    "crossings_completed".to_string(),
                    int(ledger.crossings_completed() as i64),
                ),
                (
                    "crossing_success_rate".to_string(),
                    MetricValue::Float(ledger.crossing_success_rate()),
                ),
            ]
        }
        SmokeRun::Reconnect(ledger) => {
            let dropped = ledger.tracks.len() as i64;
            vec![
                ("bots_dropped".to_string(), int(dropped)),
                (
                    "recoveries_completed".to_string(),
                    int(ledger.recoveries_completed() as i64),
                ),
                (
                    "mean_recovery_ticks".to_string(),
                    MetricValue::Float(ledger.mean_recovery_ticks()),
                ),
                (
                    "sample_period".to_string(),
                    MetricValue::F32(ledger.sample_period),
                ),
            ]
        }
        SmokeRun::LongStability(ledger) => {
            // The reproducible face only: the soak's schedule and holding
            // rate. The wall-clock resource numbers travel through the
            // ledger's `resource_report_entries` into the saved report.
            vec![
                (
                    "bots_online".to_string(),
                    int(ledger.online.last().copied().unwrap_or(0) as i64),
                ),
                ("ticks".to_string(), int(ledger.window_ticks as i64)),
                (
                    "sampling_period_secs".to_string(),
                    MetricValue::Float(ledger.resource_period.as_secs_f64()),
                ),
                (
                    "resource_sample_count".to_string(),
                    int(ledger.samples.len() as i64),
                ),
                (
                    "min_online_ratio".to_string(),
                    MetricValue::Float(ledger.min_online_ratio()),
                ),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::ReportValue;
    use crate::resource::CPU_METRIC_DEF;
    use crate::scenario::AccountPool;
    use std::path::Path;

    fn scenario(name: &str, concurrency: u64, duration_secs: u64) -> Scenario {
        Scenario {
            name: name.to_string(),
            fixed_seed: 20261123,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate: 2,
            warmup: Duration::from_secs(1),
            duration: Duration::from_secs(duration_secs),
        }
    }

    /// The saved scenario set, exactly as the PR gate will read it.
    fn saved(name: &str) -> Scenario {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/load/scenarios")
            .join(format!("{name}.xml"));
        let raw = std::fs::read(&path).expect(&format!("saved scenario {name} missing"));
        Scenario::load_xml(&raw).expect(&format!("saved {name}.xml failed to load"))
    }

    fn caps() -> SmokeCaps {
        SmokeCaps {
            clients: 4,
            duration: Duration::from_secs(20),
        }
    }

    /// A metric read as a number, whichever of the two shapes carries it.
    fn as_float(value: &MetricValue) -> f64 {
        match value {
            MetricValue::Float(v) => *v,
            MetricValue::F32(v) => *v as f64,
            MetricValue::Int(v) => *v as f64,
        }
    }

    /// The named metric of a metric list.
    fn find<'a>(metrics: &'a [(String, MetricValue)], key: &str) -> &'a MetricValue {
        &metrics
            .iter()
            .find(|(k, _)| k == key)
            .expect(&format!("no {key} metric"))
            .1
    }

    /// The envelope is a cap, not a rewrite: a nightly-scale scenario comes
    /// down to the harness's knobs, a scenario already inside the envelope
    /// keeps its own scale, and the seed and pool stay the saved ones.
    #[test]
    fn the_caps_clamp_the_saved_scale() {
        let saved = saved("login-hurricane");
        let capped = cap_scenario(&saved, &caps());
        assert_eq!(capped.concurrency, 4);
        assert_eq!(capped.duration, Duration::from_secs(20));
        assert_eq!(capped.fixed_seed, saved.fixed_seed);
        assert_eq!(capped.account_pool, saved.account_pool);
        assert_eq!(capped.ramp_rate, saved.ramp_rate);

        // A scenario already inside the envelope keeps its own scale, and a
        // zero-clients cap still runs one bot rather than none.
        let small = scenario("login-hurricane", 3, 5);
        let kept = cap_scenario(&small, &caps());
        assert_eq!(kept.concurrency, 3);
        assert_eq!(kept.duration, Duration::from_secs(5));

        let floored = cap_scenario(
            &small,
            &SmokeCaps {
                clients: 0,
                duration: Duration::from_secs(0),
            },
        );
        assert_eq!(floored.concurrency, 1);
        assert_eq!(floored.duration, Duration::from_secs(0));
    }

    /// The acceptance face: each delivered kind, loaded from its saved XML
    /// and run twice through the smoke, is refused nothing, and the second
    /// pass records the same metrics — the failure that re-runs.
    #[test]
    fn the_saved_kinds_smoke_green_and_reproduce() {
        for name in [
            "login-hurricane",
            "steady-move",
            "dense-aoi",
            "cross-cell",
            "reconnect",
            "long-stability",
        ] {
            let saved = saved(name);
            let (capped, run) =
                run_smoke(&saved, &caps()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let metrics = smoke_metrics(&run);
            assert!(!metrics.is_empty(), "{name} printed no metric line");

            let (_, rerun) = run_smoke(&saved, &caps()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(&metrics, &smoke_metrics(&rerun), "{name} did not reproduce");
            assert_eq!(capped.concurrency, 4, "{name} ignored the clients cap");
        }
    }

    /// The hurricane's metric lines are hand-checked off the ramp curve:
    /// 4 bots at 2/s after 1 warmup second in a 3s window.
    #[test]
    fn the_hurricane_metrics_are_hand_checked() {
        let small = scenario("login-hurricane", 4, 3);
        let (_, run) = run_smoke(&small, &caps()).expect("smoke");
        let metrics = smoke_metrics(&run);
        // Both metric shapes read as numbers: the whole-number metrics
        // through the Int arm, the rate through the Float arm.
        assert_eq!(as_float(find(&metrics, "login_attempts")), 4.0);
        assert_eq!(find(&metrics, "login_admitted"), &MetricValue::Int(4));
        assert_eq!(find(&metrics, "login_refused"), &MetricValue::Int(0));
        assert!((as_float(find(&metrics, "login_success_rate")) - 1.0).abs() < 1e-12);
        assert_eq!(find(&metrics, "online_peak"), &MetricValue::Int(4));
        assert_eq!(find(&metrics, "online_final"), &MetricValue::Int(4));
    }

    /// The steady metrics name the fleet, the mini-tick and the seed's
    /// trajectory fingerprint.
    #[test]
    fn the_steady_metrics_fingerprint_the_trajectory() {
        let small = scenario("steady-move", 4, 3);
        let (_, run) = run_smoke(&small, &caps()).expect("smoke");
        let metrics = smoke_metrics(&run);
        assert_eq!(find(&metrics, "bots_online"), &MetricValue::Int(4));
        // One mini-tick is a tenth of a second (TICK_PERIOD).
        assert!((as_float(find(&metrics, "sample_period")) - 0.1).abs() < 1e-6);
        // The fingerprint travels with the metrics — a re-run reproduces
        // it (pinned by the reproduce test); here it is merely present and
        // finite, the walk having moved on a circle's worth of coordinates.
        let sum = as_float(find(&metrics, "trajectory_sum"));
        assert!(sum.is_finite());
    }

    /// The dense metrics pack the capped grid: four bots at spacing 5 on a
    /// 2x2 grid under radius 25 see three neighbours each.
    #[test]
    fn the_dense_metrics_pack_the_grid() {
        let small = scenario("dense-aoi", 4, 3);
        let (_, run) = run_smoke(&small, &caps()).expect("smoke");
        let metrics = smoke_metrics(&run);
        assert_eq!(find(&metrics, "bots"), &MetricValue::Int(4));
        // count/extent^2 = 4/(2*5)^2.
        assert!((as_float(find(&metrics, "density")) - 0.04).abs() < 1e-6);
        assert_eq!(find(&metrics, "max_visible"), &MetricValue::Int(3));
        assert!((as_float(find(&metrics, "avg_visible")) - 3.0).abs() < 1e-12);
    }

    /// The cross-cell metrics name the fleet that set out, the crossings
    /// that completed, and the acceptance rate — the number the scenario
    /// exists to report. The smoke window is far too short to walk the
    /// 600-unit span, so every bot sets out and none completes; the rate
    /// over an empty numerator is still 0.0.
    #[test]
    fn the_cross_cell_metrics_report_the_success_rate() {
        let small = scenario("cross-cell", 4, 3);
        let (_, run) = run_smoke(&small, &caps()).expect("smoke");
        let metrics = smoke_metrics(&run);
        assert_eq!(find(&metrics, "bots_set_out"), &MetricValue::Int(4));
        assert_eq!(find(&metrics, "crossings_completed"), &MetricValue::Int(0));
        assert!(as_float(find(&metrics, "crossing_success_rate")).abs() < 1e-12);
    }

    /// The reconnect metrics name the bots dropped, the recoveries that
    /// completed, and the mean recovery length — the statistic the scenario
    /// exists to report. With the saved 300s window clamped to the smoke
    /// budget the drop/relogin/reply floor of two mini-ticks applies to
    /// every bot.
    #[test]
    fn the_reconnect_metrics_report_the_recovery_length() {
        let small = scenario("reconnect", 4, 3);
        let (_, run) = run_smoke(&small, &caps()).expect("smoke");
        let metrics = smoke_metrics(&run);
        assert_eq!(find(&metrics, "bots_dropped"), &MetricValue::Int(4));
        assert_eq!(find(&metrics, "recoveries_completed"), &MetricValue::Int(4));
        assert!((as_float(find(&metrics, "mean_recovery_ticks")) - 2.0).abs() < 1e-12);
    }

    /// The long-stability smoke lines carry the soak's reproducible
    /// schedule; the wall-clock resource half is saved through the
    /// ledger's report entries — the series, the peak and the metric
    /// definitions, the numbers the scenario exists to save.
    #[test]
    fn the_long_stability_metrics_save_the_resource_use() {
        let small = scenario("long-stability", 4, 3);
        let (_, run) = run_smoke(&small, &caps()).expect("smoke");
        let metrics = smoke_metrics(&run);
        assert_eq!(find(&metrics, "bots_online"), &MetricValue::Int(4));
        assert_eq!(find(&metrics, "ticks"), &MetricValue::Int(30));
        assert_eq!(
            find(&metrics, "resource_sample_count"),
            &MetricValue::Int(8)
        );
        assert!((as_float(find(&metrics, "min_online_ratio")) - 1.0).abs() < 1e-12);

        let SmokeRun::LongStability(ledger) = &run else {
            panic!("the long-stability kind dispatched elsewhere");
        };
        let saved = ledger.resource_report_entries();
        let saved_find = |key: &str| {
            saved
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| &entry.value)
                .unwrap_or_else(|| panic!("no {key} entry"))
        };
        assert_eq!(saved_find("resource_sample_count"), &ReportValue::Int(8));
        assert!(
            matches!(saved_find("peak_rss_kib"), ReportValue::Int(kib) if *kib > 0),
            "the process has a resident size"
        );
        assert_eq!(
            saved_find("cpu_metric_def"),
            &ReportValue::Str(CPU_METRIC_DEF.to_string())
        );
        assert_eq!(
            match saved_find("rss_kib_series") {
                ReportValue::List(items) => items.len(),
                _ => 0,
            },
            8
        );
    }

    /// A scenario kind with no offline run face is refused by name — an
    /// unknown kind parses, but nothing can run it.
    #[test]
    fn an_undelivered_kind_is_refused_by_name() {
        let unknown = scenario("no-such-face", 4, 3);
        let error = run_smoke(&unknown, &caps()).expect_err("unsupported kind ran");
        assert_eq!(
            error,
            SmokeError::Unsupported {
                name: "no-such-face".to_string()
            }
        );
        assert!(error.to_string().contains("no-such-face"));
    }

    /// A face that would not reproduce is refused by name — the second
    /// pass of the same fixed seed answering differently is exactly the
    /// run the smoke refuses to report.
    #[test]
    fn a_nondeterministic_face_is_refused_by_name() {
        // The hand-checked shape: big enough that both passes admit bots,
        // so a redraw between them is visible in the ledgers.
        let small = scenario("login-hurricane", 4, 3);
        let mut passes = 0;
        let flaky = |scenario: &Scenario| -> Result<SmokeRun, SmokeError> {
            passes += 1;
            if passes == 1 {
                run_face(scenario)
            } else {
                // The second pass "redraws": a face that reads anything but
                // the fixed input answers differently. The ramp rate is the
                // redraw — a different curve admits a different schedule.
                let redrawn = Scenario {
                    ramp_rate: scenario.ramp_rate + 1,
                    ..scenario.clone()
                };
                run_face(&redrawn)
            }
        };
        let error = smoke_with_face(&small, &caps(), flaky).expect_err("a flaky face passed");
        assert_eq!(
            error,
            SmokeError::Nondeterministic {
                name: "login-hurricane".to_string()
            }
        );
        assert!(error.to_string().contains("would not re-run the same way"));

        // The face's own error passes through untouched.
        let refusing = |scenario: &Scenario| -> Result<SmokeRun, SmokeError> {
            Err(SmokeError::Unsupported {
                name: scenario.name.clone(),
            })
        };
        let error = smoke_with_face(&small, &caps(), refusing).expect_err("refusal passed");
        assert_eq!(
            error,
            SmokeError::Unsupported {
                name: "login-hurricane".to_string()
            }
        );
    }

    /// The metric lines format the way the harness parses them: exactly
    /// two tab-separated non-empty fields.
    #[test]
    fn the_metric_lines_format_the_harness_contract() {
        let dense = scenario("dense-aoi", 4, 1);
        let (_, run) = run_smoke(&dense, &caps()).expect("smoke");
        // The f32 shape renders the faces' own value, so the mini-tick reads
        // 0.1 rather than its f64 widening.
        assert_eq!(format!("{}", find(&smoke_metrics(&run), "density")), "0.04");

        let small = scenario("login-hurricane", 2, 1);
        let (_, run) = run_smoke(&small, &caps()).expect("smoke");
        for (key, value) in smoke_metrics(&run) {
            let line = format!("{key}\t{value}");
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 2, "line {line:?} is not metric<TAB>value");
            assert!(!fields[0].is_empty() && !fields[1].is_empty());
        }
    }
}
