//! The load-generation process (BW-17-01) as a standalone runner:
//! drive one saved scenario through the offline faces at the harness's
//! low-load envelope (BW-17-03-05).
//!
//! Usage: `bots <scenario.xml>` with the `ci/load.sh` contract's env —
//! `LOAD_DURATION` (wall budget, seconds, default 30), `LOAD_CLIENTS`
//! (concurrency cap, default 4), optional `LOAD_TARGET` (the topology name
//! the report carries) and `LOAD_REPORT` (a path to write the
//! machine-readable report to). The `metric<TAB>value` lines go to stdout;
//! chatter and failures go to stderr.
//!
//! Usage: `bots <scenario.xml>` with the `ci/load.sh` contract's
//! env — `LOAD_DURATION` (wall budget, seconds, default 30),
//! `LOAD_CLIENTS` (concurrency cap, default 4), optional `LOAD_TARGET`
//! (the topology name the report carries) and `LOAD_REPORT` (a path to
//! write the machine-readable report to). The `metric<TAB>value` lines go
//! to stdout; chatter and failures go to stderr.

use std::path::Path;
use std::time::Duration;

use load::report::{ReportEntry, ReportValue, ScenarioReport};
use load::scenario::Scenario;
use load::smoke::{MetricValue, SmokeCaps, run_smoke, smoke_metrics};

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: bots <scenario.xml>");
        return 2;
    };
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!("bots: cannot read {path}: {err}");
            return 2;
        }
    };
    let scenario = match Scenario::load_xml(&raw) {
        Ok(scenario) => scenario,
        Err(err) => {
            eprintln!("bots: bad scenario {path}: {err}");
            return 2;
        }
    };
    let caps = SmokeCaps {
        clients: env_uint("LOAD_CLIENTS", 4),
        duration: Duration::from_secs(env_uint("LOAD_DURATION", 30)),
    };
    let (capped, smoke_run) = match run_smoke(&scenario, &caps) {
        Ok(run) => run,
        Err(err) => {
            eprintln!("bots: {path}: {err}");
            return 1;
        }
    };

    let mut report_values = Vec::new();
    for (key, value) in smoke_metrics(&smoke_run) {
        println!("{key}\t{value}");
        report_values.push(ReportEntry::new(
            &key,
            match value {
                MetricValue::Int(v) => ReportValue::Int(v),
                MetricValue::F32(v) => ReportValue::Float(v as f64),
                MetricValue::Float(v) => ReportValue::Float(v),
            },
        ));
    }
    if let Some(report_path) = std::env::var("LOAD_REPORT").ok().filter(|p| !p.is_empty()) {
        let topology = std::env::var("LOAD_TARGET")
            .ok()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "offline".to_string());
        let report = ScenarioReport::record(&capped, &topology, report_values);
        if let Err(err) = report.write_json(Path::new(&report_path)) {
            eprintln!("bots: cannot write {report_path}: {err}");
            return 1;
        }
    }
    0
}

/// An unsigned env knob, falling back to its default when unset or not a
/// number — the harness owns the envelope's well-formedness.
fn env_uint(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}
