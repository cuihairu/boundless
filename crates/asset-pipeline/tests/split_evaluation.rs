//! OPT-16-01-02: the high-coupling module split evaluation.
//!
//! The acceptance: **compare the recompile cost before and after the
//! same change** (`同修改前后重编耗时可比`). The split is evaluated in
//! a disposable workspace copy; this test records the two graphs and
//! the measured rebuilds, and pins the structural facts:
//!
//! - before: `time_queue` is a module inside `foundation`. Touching it
//!   recompiles `foundation` plus its 24 downstream crates (25 total).
//! - after: `time_queue` is its own crate, depended on by only six
//!   crates. Touching its source recompiles seven crates total.
//!
//! Both measurements used warm targets on the same filesystem, the
//! same `cargo build --workspace --offline` command, and a timestamp
//! touch with unchanged content (mtime restored afterwards). The
//! crate counts are structural and gated; the seconds are recorded
//! observations with machine noise.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::PathBuf;

#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/review/fixtures/dep-graph")
}

/// Reads a TSV fixture: skip the header, split each line on tabs.
fn rows(path: &PathBuf) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(path).expect("fixture reads");
    text.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(str::to_string).collect())
        .collect()
}

/// crate → internal dependencies, from a graph TSV.
fn graph(path: &PathBuf) -> BTreeMap<String, BTreeSet<String>> {
    let mut result = BTreeMap::new();
    for cells in rows(path) {
        let deps = if cells[1].is_empty() {
            BTreeSet::new()
        } else {
            cells[1].split(',').map(str::to_string).collect()
        };
        result.insert(cells[0].clone(), deps);
    }
    result
}

/// Transitive downstream closure (the crates that rebuild when this
/// crate changes).
fn fan_out(rev: &BTreeMap<String, BTreeSet<String>>, crate_name: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![crate_name.to_string()];
    while let Some(current) = stack.pop() {
        if let Some(dependents) = rev.get(&current) {
            for next in dependents {
                if seen.insert(next.clone()) {
                    stack.push(next.clone());
                }
            }
        }
    }
    seen
}

fn reverse(graph: &BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, BTreeSet<String>> {
    let mut rev: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (crate_name, deps) in graph {
        for dep in deps {
            rev.entry(dep.clone())
                .or_default()
                .insert(crate_name.clone());
        }
    }
    rev
}

#[test]
fn the_time_queue_split_cuts_the_rebuild_fan_out() {
    let mut record = Record::default();
    let before = graph(&fixtures().join("crates-graph.tsv"));
    let after = graph(&fixtures().join("split-graph.tsv"));
    let before_rev = reverse(&before);
    let after_rev = reverse(&after);

    // The before shape: `time_queue` is a foundation module, so its
    // change carries foundation's entire downstream fan-out.
    assert_eq!(before.len(), 29);
    assert_eq!(after.len(), 30, "the split adds one workspace crate");
    assert!(!before.contains_key("time-queue"));
    assert!(after.contains_key("time-queue"));
    assert!(after["time-queue"].contains("foundation"));
    assert!(
        after["foundation"].iter().all(|dep| dep != "time-queue"),
        "the dependency points from the extracted crate to foundation"
    );
    let before_fanout = fan_out(&before_rev, "foundation");
    let after_fanout = fan_out(&after_rev, "time-queue");
    assert_eq!(before_fanout.len(), 24);
    assert_eq!(after_fanout.len(), 6);
    assert_eq!(before_fanout.len() - after_fanout.len(), 18);
    for name in [
        "mercury",
        "db",
        "server-runtime",
        "db-postgres",
        "test-support",
        "boundless-server",
    ] {
        assert!(
            after_fanout.contains(name),
            "{name} still depends on time-queue"
        );
    }
    record.put("crates_before", before.len());
    record.put("crates_after", after.len());
    record.put("before_fanout", before_fanout.len());
    record.put("after_fanout", after_fanout.len());
    record.put("fanout_reduction", before_fanout.len() - after_fanout.len());
    record.put(
        "after_dependents",
        after_fanout.iter().cloned().collect::<Vec<_>>().join(","),
    );

    // The measured rebuilds: each run's recompiled count must match
    // the graph prediction. The seconds are an observation, not a
    // gate (the two after runs differ by ~57% from machine noise).
    let samples = rows(&fixtures().join("split-samples.tsv"));
    assert_eq!(samples.len(), 3);
    let mut before_seconds = Vec::new();
    let mut after_seconds = Vec::new();
    for cells in &samples {
        let crate_name = cells[0].as_str();
        let recompiled: usize = cells[2].parse().expect("count");
        let seconds: f64 = cells[3].parse().expect("seconds");
        let prediction = if crate_name == "foundation" {
            fan_out(&before_rev, crate_name).len() + 1
        } else {
            fan_out(&after_rev, crate_name).len() + 1
        };
        assert_eq!(
            recompiled, prediction,
            "{crate_name}: the graph predicts the real rebuild set"
        );
        if crate_name == "foundation" {
            before_seconds.push(seconds);
        } else {
            after_seconds.push(seconds);
        }
        record.put(
            format!("recompiled_{crate_name}_run{}", cells[4]),
            recompiled,
        );
        record.put(
            format!("seconds_{crate_name}_run{}", cells[4]),
            format!("{seconds:.2}"),
        );
    }
    assert_eq!(before_seconds.len(), 1);
    assert_eq!(after_seconds.len(), 2);
    let min_after = after_seconds.iter().copied().fold(f64::INFINITY, f64::min);
    record.put(
        "best_wall_ratio",
        format!("{:.2}", before_seconds[0] / min_after),
    );
    record.put("recompiled_reduction", 25 - 7);

    let mut text = String::new();
    for (k, v) in &record.0 {
        text.push_str(&format!("{k}\t{v}\n"));
    }
    print!("{text}");
    if let Ok(path) = std::env::var("SPLIT_EVAL_OUT") {
        std::fs::write(&path, &text).expect("write SPLIT_EVAL_OUT");
        println!("record: {path}");
    }
    let _ = std::io::stdout().flush();
}
