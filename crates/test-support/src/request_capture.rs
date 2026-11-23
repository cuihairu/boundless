//! BW-23-02-03: Tick and RPC capture.
//!
//! Reduces a snapshot into per-request records: for every request scope a
//! handler opened, which role/pid handled it, in which tick (frame) and on
//! which thread, how long the handler ran, and how long it waited — the wait
//! being the handler's dominant child section, where the time sat waiting
//! for the reply. A fixed request's wait is thus located, and the tick it
//! was handled on is captured with it.

use foundation::profiler::Snapshot;

/// One captured request handling: which process handled it, in which tick
/// and thread, and how the handler's time split between work and waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRecord {
    /// The request id the handler was tagged with.
    pub request: u32,
    /// The role that handled it.
    pub role: String,
    /// The pid of the handling process.
    pub pid: u32,
    /// The tick (frame) the handler ran in.
    pub tick: u32,
    /// The handler section name.
    pub name: String,
    /// The thread index the handler ran on.
    pub thread: u32,
    /// Total active cycles in the handler.
    pub handler_cycles: u64,
    /// Cycles the handler spent waiting — its dominant child section.
    pub wait_cycles: u64,
    /// The name of the wait section (empty if the handler never waited).
    pub wait_name: String,
}

/// Reduce a snapshot into one record per request scope a handler opened.
/// Only the outermost span of each request counts, so a request handled
/// several times yields several records.
pub fn request_records(snapshot: &Snapshot) -> Vec<RequestRecord> {
    let mut records = Vec::new();
    for node in &snapshot.nodes {
        if node.request == 0 {
            continue;
        }
        // Outermost span of the request (parent belongs to another request).
        let top_level = match node.parent {
            None => true,
            Some(p) => snapshot.nodes[p].request != node.request,
        };
        if !top_level {
            continue;
        }
        // The wait is the handler's dominant direct child: the section the
        // time sat in, blocked on the reply.
        let mut wait_cycles = 0;
        let mut wait_name = String::new();
        for &child in &node.children {
            let cycles = snapshot.nodes[child].inclusive_cycles;
            if cycles > wait_cycles {
                wait_cycles = cycles;
                wait_name = snapshot.nodes[child].name.clone();
            }
        }
        records.push(RequestRecord {
            request: node.request,
            role: snapshot.identity.role.clone(),
            pid: snapshot.identity.pid,
            tick: node.frame,
            name: node.name.clone(),
            thread: node.thread,
            handler_cycles: node.inclusive_cycles,
            wait_cycles,
            wait_name,
        });
    }
    records.sort_by(|a, b| {
        (a.request, a.role.as_str(), a.tick, a.name.as_str()).cmp(&(
            b.request,
            b.role.as_str(),
            b.tick,
            b.name.as_str(),
        ))
    });
    records
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

    #[test]
    fn a_fixed_requests_wait_is_located_in_its_tick() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        // A fixed request 42 handled on tick 0: 20 stamps of work, a 60-stamp
        // wait for the reply, then 20 stamps to finish.
        {
            let _handle = prof.scoped_request(42, "handleRequest");
            advance(&source, 20);
            {
                let _wait = prof.scoped("awaitReply");
                advance(&source, 60);
            }
            advance(&source, 20);
        }
        prof.tick();

        let records = request_records(&prof.snapshot());
        assert_eq!(records.len(), 1, "one request handled: {records:?}");
        let record = &records[0];
        assert_eq!(record.request, 42);
        assert_eq!(record.role, "cellapp01");
        assert_eq!(record.pid, 100);
        assert_eq!(record.tick, 0, "the tick it was handled on is captured");
        assert_eq!(record.name, "handleRequest");
        assert_eq!(record.handler_cycles, 100);
        assert_eq!(record.wait_cycles, 60, "the wait is located");
        assert_eq!(record.wait_name, "awaitReply");
    }

    #[test]
    fn a_request_handled_later_lands_in_its_own_tick() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("baseapp01", 200);

        // The same request handled on a later tick, with no wait.
        prof.tick();
        {
            let _handle = prof.scoped_request(42, "deliverReply");
            advance(&source, 5);
        }
        prof.tick();

        let records = request_records(&prof.snapshot());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tick, 1);
        assert_eq!(records[0].handler_cycles, 5);
        assert_eq!(records[0].wait_cycles, 0, "no wait section");
        assert_eq!(records[0].wait_name, "");
    }

    #[test]
    fn a_request_handled_twice_yields_two_records() {
        let source = pinned();
        let prof = profiler_with(&source);

        for _ in 0..2 {
            {
                let _h = prof.scoped_request(7, "handle");
                advance(&source, 10);
            }
            prof.tick();
        }

        let records = request_records(&prof.snapshot());
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].tick, 0);
        assert_eq!(records[1].tick, 1);
        assert_eq!(records[0].handler_cycles, 10);
        assert_eq!(records[1].handler_cycles, 10);
    }
}
