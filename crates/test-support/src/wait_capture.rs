//! BW-23-02-04: Network and database wait capture.
//!
//! A wait is a scoped section for an outbound operation — a network send or a
//! database query. Ops run under an injectable latency table, so a test pins
//! each op's delay; the capture session buffers the raw events, and
//! [`wait_events`] reduces them back to one wait event per section, carrying
//! the observed latency. The injected delay is thus reflected in the
//! corresponding captured event, one-to-one.

use foundation::profiler::{EventKind, Profiler, ProfilerIdentity};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::capture_session::CapturedEvent;

/// Which kind of outbound operation a wait section is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    /// A network operation (`net.*`).
    Network,
    /// A database operation (`db.*`).
    Database,
}

/// Classify a section name as a wait: `net.` and `db.` prefixed ops are the
/// waits everything else is ordinary work.
pub fn classify(name: &str) -> Option<WaitKind> {
    if name.starts_with("net.") {
        Some(WaitKind::Network)
    } else if name.starts_with("db.") {
        Some(WaitKind::Database)
    } else {
        None
    }
}

/// An injectable per-op latency: the deterministic test delay each op waits.
#[derive(Debug, Clone, Default)]
pub struct LatencyTable {
    delays: HashMap<String, u64>,
}

impl LatencyTable {
    /// A table with no injected latencies (every op instant).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one op's delay, in clock stamps.
    pub fn set(mut self, op: &str, delay: u64) -> Self {
        self.delays.insert(op.to_string(), delay);
        self
    }

    /// The delay injected for `op` (0 if unlisted).
    pub fn delay_for(&self, op: &str) -> u64 {
        self.delays.get(op).copied().unwrap_or(0)
    }
}

/// Run one outbound op under the profiler: open its section tagged with the
/// handling request (0 = untagged), wait out the injected latency, close.
pub fn run_op(prof: &Profiler, table: &LatencyTable, clock: &AtomicU64, request: u32, op: &str) {
    let _op = prof.scoped_request(request, op);
    clock.fetch_add(table.delay_for(op), Ordering::SeqCst);
}

/// One captured wait: the op, its kind, and the latency the capture observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitEvent {
    /// The role/pid of the process that waited.
    pub identity: ProfilerIdentity,
    /// The op's section name (e.g. `net.send`).
    pub name: String,
    /// Whether the wait was a network or database op.
    pub kind: WaitKind,
    /// The observed latency, in clock stamps.
    pub cycles: u64,
    /// The request the op ran under (0 = none).
    pub request: u32,
}

/// Reduce captured events to wait events: pair each Start with its End on the
/// same thread, keep the sections that classify as waits, and report each
/// with its observed latency, in the order the waits completed. Non-wait
/// sections and counters pass through unkept; an unmatched End is skipped.
pub fn wait_events(captured: &[CapturedEvent]) -> Vec<WaitEvent> {
    let mut open: HashMap<u32, Vec<&CapturedEvent>> = HashMap::new();
    let mut waits = Vec::new();
    for event in captured {
        match event.entry.kind {
            EventKind::Start | EventKind::StartIdle => {
                open.entry(event.entry.thread).or_default().push(event);
            }
            EventKind::End => {
                let Some(start) = open.get_mut(&event.entry.thread).and_then(Vec::pop) else {
                    continue;
                };
                if classify(&event.entry.name).is_some() {
                    waits.push(WaitEvent {
                        identity: event.identity.clone(),
                        name: event.entry.name.clone(),
                        kind: classify(&event.entry.name).expect("just classified"),
                        cycles: event.entry.time - start.entry.time,
                        request: event.entry.request,
                    });
                }
            }
            EventKind::Counter => {}
        }
    }
    waits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_session::CaptureSession;
    use std::sync::Arc;

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

    #[test]
    fn injected_latency_is_reflected_in_the_corresponding_event() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        // The fixed latencies: a 40-stamp network round trip and a 90-stamp
        // database query, the db one serving request 7.
        let table = LatencyTable::new().set("net.send", 40).set("db.query", 90);
        let session = CaptureSession::new(&[&prof]);
        session.start();
        run_op(&prof, &table, &source, 0, "net.send");
        run_op(&prof, &table, &source, 7, "db.query");
        let captured = session.stop();

        let waits = wait_events(&captured);
        assert_eq!(waits.len(), 2, "{waits:?}");
        let net = &waits[0];
        assert_eq!(net.name, "net.send");
        assert_eq!(net.kind, WaitKind::Network);
        assert_eq!(net.cycles, 40, "the injected net delay shows up here");
        assert_eq!(net.request, 0);
        assert_eq!(net.identity.role, "cellapp01");
        assert_eq!(net.identity.pid, 100);
        let db = &waits[1];
        assert_eq!(db.name, "db.query");
        assert_eq!(db.kind, WaitKind::Database);
        assert_eq!(db.cycles, 90, "the injected db delay shows up here");
        assert_eq!(db.request, 7, "the wait is tied to its request");
    }

    #[test]
    fn ordinary_work_is_not_a_wait() {
        let source = pinned();
        let prof = profiler_with(&source);

        let table = LatencyTable::new().set("cpu.work", 50);
        let session = CaptureSession::new(&[&prof]);
        session.start();
        run_op(&prof, &table, &source, 0, "cpu.work");
        let waits = wait_events(&session.stop());
        assert!(
            waits.is_empty(),
            "unclassified sections are kept out: {waits:?}"
        );
    }

    #[test]
    fn the_same_op_waited_twice_yields_two_events() {
        let source = pinned();
        let prof = profiler_with(&source);

        let table = LatencyTable::new().set("net.send", 12);
        let session = CaptureSession::new(&[&prof]);
        session.start();
        for _ in 0..2 {
            run_op(&prof, &table, &source, 3, "net.send");
            advance(&source, 5);
        }
        let waits = wait_events(&session.stop());
        assert_eq!(waits.len(), 2, "{waits:?}");
        for wait in &waits {
            assert_eq!(wait.cycles, 12);
            assert_eq!(wait.request, 3);
        }
    }
}
