//! OPT-16-01-01: the dependency and incremental-build baseline.
//!
//! The acceptance: **locate the rebuild sources and their cost**
//! (`定位重编源及耗时`). Two fixtures from the same workspace:
//!
//! - **The crate graph** (`cargo metadata --no-deps`, captured in
//!   `docs/review/fixtures/dep-graph/crates-graph.tsv`) — every
//!   workspace crate and its internal dependencies. The harness
//!   builds the reverse graph and computes each crate's *fan-out*:
//!   the crates a touch to that crate forces to recompile.
//! - **The measured rebuilds** (`rebuild-samples.tsv`) — real
//!   `cargo build --workspace` runs after touching one source file's
//!   timestamp (content unchanged, mtime restored afterwards): a
//!   leaf (`bots`), the top hub (`foundation`, twice), and
//!   `resources`. Cargo's `Compiling` lines count the recompiled
//!   crates; wall seconds come from `/usr/bin/time`.
//!
//! The contracts: the measured count equals the graph's fan-out
//! plus the touched crate itself; the leaf rebuild is cheap, the
//! hub rebuild touches most of the workspace. Those are structural
//! facts, stable across machines; the seconds are a captured
//! observation (noise-labelled, not a regression threshold).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::PathBuf;

/// The record sink (the OPT family's shape).
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }
}

/// The fixture directory (both files are captured next to the evidence page).
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/review/fixtures/dep-graph")
}

/// Reads a TSV fixture: the header is skipped, each subsequent line
/// is split on tabs.
fn rows(path: &PathBuf) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(path).expect("fixture reads");
    text.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(str::to_string).collect())
        .collect()
}

/// Parses the crate graph fixture: crate → its internal dependencies.
fn graph() -> BTreeMap<String, BTreeSet<String>> {
    let mut result = BTreeMap::new();
    for cells in rows(&fixtures().join("crates-graph.tsv")) {
        let crate_name = cells[0].clone();
        let deps = if cells[1].is_empty() {
            BTreeSet::new()
        } else {
            cells[1].split(',').map(str::to_string).collect()
        };
        result.insert(crate_name, deps);
    }
    result
}

/// Builds the reverse graph: dependency → the crates that name it.
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

/// The transitive downstream closure: every crate that must rebuild
/// when `crate_name` changes.
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

/// The dependency and incremental-build baseline runs.
#[test]
fn the_dependency_and_rebuild_baseline_locates_sources() {
    let mut record = Record::default();
    let graph = graph();
    let rev = reverse(&graph);

    // The graph's shape: 29 workspace crates; the hubs the first
    // dependency pass identified; the leaf set with no dependents.
    assert_eq!(graph.len(), 29);
    let foundation = fan_out(&rev, "foundation");
    let resources = fan_out(&rev, "resources");
    let bots = fan_out(&rev, "bots");
    assert_eq!(
        foundation.len(),
        24,
        "a foundation touch rebuilds 24 crates"
    );
    assert_eq!(resources.len(), 14);
    assert!(bots.is_empty(), "a leaf rebuilds nothing downstream");
    assert!(foundation.contains("db-postgres"));
    assert!(foundation.contains("server-runtime"));
    assert!(foundation.contains("bots"));
    assert!(
        !resources.contains("foundation"),
        "the fan-out is downstream only"
    );
    record.put("workspace_crates", graph.len());
    record.put("foundation_fanout", foundation.len());
    record.put("resources_fanout", resources.len());
    record.put("bots_fanout", bots.len());

    // Rank the hubs: the top five are the first dependency pass's
    // hot spots. Pinning the order catches graph drift (a new edge
    // that couples a formerly-independent crate).
    let mut ranked: Vec<_> = graph
        .keys()
        .map(|c| (fan_out(&rev, c).len(), c.clone()))
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let top: Vec<&str> = ranked.iter().take(5).map(|(_, c)| c.as_str()).collect();
    assert_eq!(
        top,
        vec!["foundation", "resources", "physics", "platform", "terrain"],
        "the rebuild hot spots stay the same"
    );
    for (count, crate_name) in ranked.iter().take(5) {
        record.put(format!("hub_{crate_name}_fanout"), count);
    }

    // The measured rebuilds: the graph's prediction must equal
    // what cargo actually recompiled (the touched crate + its
    // downstream fan-out). Times are observations, not gates.
    let samples = rows(&fixtures().join("rebuild-samples.tsv"));
    assert_eq!(samples.len(), 4);
    let mut foundation_seconds = Vec::new();
    let mut leaf_seconds = 0.0;
    for cells in &samples {
        let crate_name = &cells[0];
        let recompiled: usize = cells[2].parse().expect("count");
        let seconds: f64 = cells[3].parse().expect("seconds");
        let predicted = fan_out(&rev, crate_name).len() + 1;
        assert_eq!(
            recompiled, predicted,
            "{crate_name}: the graph predicts the real rebuild set"
        );
        if crate_name == "foundation" {
            foundation_seconds.push(seconds);
        }
        if crate_name == "bots" {
            leaf_seconds = seconds;
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
    assert_eq!(foundation_seconds.len(), 2);
    let slowest = foundation_seconds.iter().copied().fold(0.0, f64::max);
    record.put(
        "hub_to_leaf_cost_ratio",
        format!("{:.1}", slowest / leaf_seconds),
    );
    record.put("hub_to_leaf_crate_ratio", format!("{:.1}", 25.0 / 1.0));

    // The rebuild hot spots are the crates to split or decouple in
    // 16-01-02: 24 downstream crates from one foundation touch.
    record.put("hotspots", top.join(","));

    let mut text = String::new();
    for (k, v) in &record.0 {
        text.push_str(&format!("{k}\t{v}\n"));
    }
    print!("{text}");
    if let Ok(path) = std::env::var("DEP_GRAPH_OUT") {
        std::fs::write(&path, &text).expect("write DEP_GRAPH_OUT");
        println!("record: {path}");
    }
    let _ = std::io::stdout().flush();
}
