//! BW-23-03-04: function, message and tick statistics.
//!
//! Reduces a capture into the three aggregates the section asks for: by
//! function (the profiler's per-name section statistics), by message (the
//! request handlers, grouped by handler name across the request ids they
//! served), and by tick (what each tick ran and how long it took). Tick
//! totals count only the sections that open a tick, so nested work is not
//! counted twice.

use foundation::profiler::{NameStat, Snapshot};

/// One handler's aggregate across the request ids it served: how often it
/// was entered, its total active cycles, the ids and threads involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageStat {
    /// The handler section name.
    pub name: String,
    /// How many times the handler was entered.
    pub calls: u32,
    /// The request ids it handled, ascending.
    pub requests: Vec<u32>,
    /// Total active cycles across those calls.
    pub inclusive_cycles: u64,
    /// The thread indices it ran on, ascending.
    pub threads: Vec<u32>,
}

/// One tick's aggregate: how many sections it opened and their active cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickStat {
    /// The tick (frame) number.
    pub tick: u32,
    /// How many sections opened in that tick.
    pub calls: u32,
    /// The active cycles those sections ran (nested work is theirs alone).
    pub inclusive_cycles: u64,
}

/// The three aggregates of one capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsReport {
    /// Per-function section statistics, as the profiler aggregated them.
    pub functions: Vec<NameStat>,
    /// Per-message aggregates, grouped by handler name.
    pub messages: Vec<MessageStat>,
    /// Per-tick aggregates, one row per tick that ran sections.
    pub ticks: Vec<TickStat>,
}

/// Aggregate one capture into function, message and tick statistics.
pub fn report(snapshot: &Snapshot) -> StatsReport {
    let messages = message_stats(snapshot);
    let ticks = tick_stats(snapshot);
    StatsReport {
        functions: snapshot.stats.clone(),
        messages,
        ticks,
    }
}

/// Group the request scopes by handler name. A request handled under several
/// handler names contributes to each, so a message is what a handler did
/// across the ids it served. Only the outermost scope of a request counts, so
/// a handler's own cycles are not counted twice from its children.
fn message_stats(snapshot: &Snapshot) -> Vec<MessageStat> {
    let mut by_name: std::collections::BTreeMap<String, MessageStat> =
        std::collections::BTreeMap::new();
    for node in &snapshot.nodes {
        if node.request == 0 {
            continue;
        }
        let top_level = match node.parent {
            None => true,
            Some(parent) => snapshot.nodes[parent].request != node.request,
        };
        if !top_level {
            continue;
        }
        let entry = by_name
            .entry(node.name.clone())
            .or_insert_with(|| MessageStat {
                name: node.name.clone(),
                calls: 0,
                requests: Vec::new(),
                inclusive_cycles: 0,
                threads: Vec::new(),
            });
        entry.calls += 1;
        entry.inclusive_cycles += node.inclusive_cycles;
        entry.requests.push(node.request);
        entry.threads.push(node.thread);
    }
    let mut messages: Vec<MessageStat> = by_name.into_values().collect();
    for message in &mut messages {
        message.requests.sort_unstable();
        message.requests.dedup();
        message.threads.sort_unstable();
        message.threads.dedup();
    }
    messages
}

/// Sum one tick's sections. Only the nodes that open a tick count, so a
/// nested section's cycles ride inside its parent's.
fn tick_stats(snapshot: &Snapshot) -> Vec<TickStat> {
    let mut per_tick: std::collections::BTreeMap<u32, TickStat> = std::collections::BTreeMap::new();
    for &root in &snapshot.roots {
        let node = &snapshot.nodes[root];
        let entry = per_tick.entry(node.frame).or_insert(TickStat {
            tick: node.frame,
            calls: 0,
            inclusive_cycles: 0,
        });
        entry.calls += 1;
        entry.inclusive_cycles += node.inclusive_cycles;
    }
    per_tick.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundation::profiler::Profiler;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    type Pinned = Arc<AtomicU64>;

    fn pinned() -> Pinned {
        Arc::new(AtomicU64::new(0))
    }

    fn profiler_with(source: &Pinned) -> Profiler {
        let source = source.clone();
        Profiler::with_clock(move || source.load(Ordering::SeqCst))
    }

    fn advance(source: &Pinned, by: u64) {
        source.fetch_add(by, Ordering::SeqCst);
    }

    fn handle(prof: &Profiler, source: &Pinned, request: u32, handler: &str, cycles: u64) {
        {
            let _h = prof.scoped_request(request, handler);
            advance(source, cycles);
        }
    }

    #[test]
    fn a_fixed_load_aggregates_to_its_exact_stats() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        // Tick 0: request 5 handled twice, plus an ordinary warmup section.
        handle(&prof, &source, 5, "handleMove", 10);
        handle(&prof, &source, 5, "handleMove", 6);
        {
            let _w = prof.scoped("warmup");
            advance(&source, 4);
        }
        prof.tick();
        // Tick 1: request 6 through the same handler, request 7 through another.
        handle(&prof, &source, 6, "handleMove", 12);
        handle(&prof, &source, 7, "handleAttack", 8);

        let report = report(&prof.snapshot());

        // By function: each name's calls and cycles add up exactly.
        let move_stat = report
            .functions
            .iter()
            .find(|stat| stat.name == "handleMove")
            .expect("handleMove is aggregated");
        assert_eq!(move_stat.calls, 3);
        assert_eq!(move_stat.inclusive_cycles, 10 + 6 + 12);
        let warmup = report
            .functions
            .iter()
            .find(|stat| stat.name == "warmup")
            .expect("warmup is aggregated");
        assert_eq!(warmup.calls, 1);
        assert_eq!(warmup.inclusive_cycles, 4);

        // By message: grouped by handler name across the ids it served.
        let by_move = report
            .messages
            .iter()
            .find(|stat| stat.name == "handleMove")
            .expect("handleMove is a message");
        assert_eq!(by_move.calls, 3);
        assert_eq!(by_move.requests, vec![5, 6], "the ids it served");
        assert_eq!(by_move.inclusive_cycles, 28);
        let by_attack = report
            .messages
            .iter()
            .find(|stat| stat.name == "handleAttack")
            .expect("handleAttack is a message");
        assert_eq!(by_attack.calls, 1);
        assert_eq!(by_attack.requests, vec![7]);
        assert_eq!(by_attack.inclusive_cycles, 8);

        // By tick: each tick counts what it ran, and no more.
        let tick0 = report
            .ticks
            .iter()
            .find(|stat| stat.tick == 0)
            .expect("tick 0 ran sections");
        assert_eq!(tick0.calls, 3, "two handles plus the warmup");
        assert_eq!(tick0.inclusive_cycles, 10 + 6 + 4);
        let tick1 = report
            .ticks
            .iter()
            .find(|stat| stat.tick == 1)
            .expect("tick 1 ran sections");
        assert_eq!(tick1.calls, 2);
        assert_eq!(tick1.inclusive_cycles, 12 + 8);
        assert_eq!(report.ticks.len(), 2, "no tick that ran nothing");
    }

    #[test]
    fn nested_work_counts_once_in_its_own_tick() {
        let source = pinned();
        let prof = profiler_with(&source);

        // The outer section owns the tick; the nested one rides inside it.
        {
            let _outer = prof.scoped_request(3, "handleMove");
            advance(&source, 5);
            {
                let _inner = prof.scoped("validate");
                advance(&source, 3);
            }
            advance(&source, 4);
        }
        prof.tick();

        let report = report(&prof.snapshot());
        let tick0 = report
            .ticks
            .iter()
            .find(|stat| stat.tick == 0)
            .expect("tick 0");
        // Only the outer section opens the tick: 12 cycles, counted once.
        assert_eq!(tick0.calls, 1);
        assert_eq!(tick0.inclusive_cycles, 12);

        // By function both sections appear separately.
        assert_eq!(
            report
                .functions
                .iter()
                .filter(|stat| stat.name == "validate")
                .count(),
            1,
            "the nested section is its own function row"
        );
    }
}
