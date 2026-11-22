//! BW-23-01-03: cross-role timeline correlation.
//!
//! Merges per-role `profiler::Snapshot`s into one timeline under a unified
//! monotonic clock, so a logical request can be followed across the roles
//! that touched it. Each snapshot already carries its own identity (role +
//! pid); a per-role clock offset shifts process-local stamps onto the
//! unified timebase, resolved from a `ClockAnchor` that reads both clocks at
//! the same wall instant (the reference merges processes the same way).

use foundation::profiler::Snapshot;

/// Two monotonic readings taken at the same wall instant: one from the role's
/// process-local clock, one from the unified timeline's clock. The offset that
/// lands the role's stamps on the unified timebase is `unified - role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockAnchor {
    /// The role's monotonic stamp at the anchor instant.
    pub role_monotonic: u64,
    /// The unified timeline's monotonic stamp at the same instant.
    pub unified_monotonic: u64,
}

impl ClockAnchor {
    /// The stamp shift: `unified_stamp = role_stamp + offset()`.
    pub fn offset(&self) -> u64 {
        self.unified_monotonic.saturating_sub(self.role_monotonic)
    }
}

/// One role's contribution to the merged timeline.
#[derive(Debug, Clone)]
pub struct RoleTrace {
    /// The role's snapshot (carries the role name and pid).
    pub snapshot: Snapshot,
    /// Stamps in the snapshot are shifted by this many nanoseconds onto the
    /// unified timebase. Zero when the role already shares the clock.
    pub offset: u64,
}

/// A section on the merged timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineSpan {
    /// The role that recorded it (the snapshot identity's role).
    pub role: String,
    /// The pid of the recording process.
    pub pid: u32,
    /// The section name.
    pub name: String,
    /// The tick (frame) current when the section opened.
    pub frame: u32,
    /// The small thread index the section ran on.
    pub thread: u32,
    /// The unified stamp at which the section opened.
    pub start: u64,
    /// Active cycles from open to close.
    pub inclusive_cycles: u64,
    /// The nearest enclosing request id, 0 if outside any request scope.
    pub request: u32,
    /// Whether this span opens the request's scope (its parent, if any,
    /// belongs to another request). Only these count as the request's trace.
    pub top_level_request: bool,
}

/// The merged cross-role timeline, ordered by unified start stamp.
#[derive(Debug, Clone, Default)]
pub struct MergedTimeline {
    spans: Vec<TimelineSpan>,
}

impl MergedTimeline {
    /// Merge one or more role traces into a single time-ordered timeline.
    /// Each section becomes a span carrying the role, pid, tick and request
    /// id it was recorded under.
    pub fn merge(traces: &[RoleTrace]) -> Self {
        let mut spans = Vec::new();
        for trace in traces {
            let identity = &trace.snapshot.identity;
            for node in &trace.snapshot.nodes {
                let top_level_request = node.request != 0
                    && match node.parent {
                        None => true,
                        Some(p) => trace.snapshot.nodes[p].request != node.request,
                    };
                spans.push(TimelineSpan {
                    role: identity.role.clone(),
                    pid: identity.pid,
                    name: node.name.clone(),
                    frame: node.frame,
                    thread: node.thread,
                    start: node.start.saturating_add(trace.offset),
                    inclusive_cycles: node.inclusive_cycles,
                    request: node.request,
                    top_level_request,
                });
            }
        }
        spans.sort_by(|a, b| {
            (a.start, a.role.as_str(), a.pid, a.frame).cmp(&(
                b.start,
                b.role.as_str(),
                b.pid,
                b.frame,
            ))
        });
        MergedTimeline { spans }
    }

    /// All spans, in unified time order.
    pub fn spans(&self) -> &[TimelineSpan] {
        &self.spans
    }

    /// The time-ordered sequence of spans that open one request's scope
    /// across roles — the cross-role trace of that request.
    pub fn request_trace(&self, request: u32) -> Vec<&TimelineSpan> {
        self.spans
            .iter()
            .filter(|s| s.request == request && s.top_level_request)
            .collect()
    }
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
    fn a_shared_clock_tracks_a_request_across_two_roles() {
        // The unified monotonic clock: both roles read the same source, so
        // their stamps are comparable and the offsets are zero.
        let source = pinned();
        let cell = profiler_with(&source);
        let base = profiler_with(&source);
        cell.set_identity("cellapp01", 100);
        base.set_identity("baseapp01", 200);

        // Request 42: cellapp01 handles it first (tick 0), then baseapp01 —
        // already on its second tick — replies.
        {
            let _handle = cell.scoped_request(42, "handleRequest");
            advance(&source, 50);
        }
        cell.tick();
        advance(&source, 20);
        base.tick();
        {
            let _reply = base.scoped_request(42, "deliverReply");
            advance(&source, 30);
        }

        let timeline = MergedTimeline::merge(&[
            RoleTrace {
                snapshot: cell.snapshot(),
                offset: 0,
            },
            RoleTrace {
                snapshot: base.snapshot(),
                offset: 0,
            },
        ]);

        let trace = timeline.request_trace(42);
        assert_eq!(trace.len(), 2, "both roles handled request 42");
        assert_eq!(trace[0].role, "cellapp01");
        assert_eq!(trace[0].pid, 100);
        assert_eq!(trace[0].frame, 0, "cell handled it before its tick");
        assert_eq!(trace[1].role, "baseapp01");
        assert_eq!(trace[1].pid, 200);
        assert_eq!(trace[1].frame, 1, "base handled it after its tick");

        // Time order: the cell's handler opens before the base's reply.
        assert!(trace[0].start < trace[1].start, "{trace:?}");
        assert_eq!(trace[0].inclusive_cycles, 50);
        assert_eq!(trace[1].inclusive_cycles, 30);

        // The full timeline is time-ordered and carries identity + tick.
        let spans = timeline.spans();
        assert_eq!(spans.len(), 2);
        assert!(spans.windows(2).all(|w| w[0].start <= w[1].start));
    }

    #[test]
    fn a_clock_anchor_aligns_a_process_local_clock_onto_the_unified_timebase() {
        // Two roles under different process-local origins. The unified
        // timeline's own origin is 10000 (it started earlier). The cell role
        // reads the same machine monotonic clock but froze its own origin
        // later, so its stamps start at 4000 while the unified clock reads
        // 10000 at that same instant.
        let unified_source = pinned();
        let cell_source = pinned();
        advance(&unified_source, 10_000);
        advance(&cell_source, 4_000);

        let cell = profiler_with(&cell_source);
        cell.set_identity("cellapp01", 100);

        {
            let _handle = cell.scoped_request(42, "handleRequest");
            advance(&cell_source, 50);
            advance(&unified_source, 50);
        }
        cell.tick();

        // The anchor: both clocks are read at the same wall instant.
        let anchor = ClockAnchor {
            role_monotonic: cell_source.load(Ordering::SeqCst),
            unified_monotonic: unified_source.load(Ordering::SeqCst),
        };
        let offset = anchor.offset();
        assert_eq!(offset, 6_000, "unified origin is 6000 ahead");

        let timeline = MergedTimeline::merge(&[RoleTrace {
            snapshot: cell.snapshot(),
            offset,
        }]);

        let span = &timeline.spans()[0];
        // The cell's stamp (4000) shifted onto the unified timebase lands at
        // 10000, i.e. exactly where the unified clock reads at open time.
        assert_eq!(span.start, 10_000, "aligned with the unified clock");
        // And its handler still measures 50 cycles on the aligned timebase.
        assert_eq!(span.inclusive_cycles, 50);
    }

    #[test]
    fn untagged_sections_join_the_timeline_outside_any_request() {
        let source = pinned();
        let prof = profiler_with(&source);
        {
            let _s = prof.scoped("work");
            advance(&source, 5);
        }
        let timeline = MergedTimeline::merge(&[RoleTrace {
            snapshot: prof.snapshot(),
            offset: 0,
        }]);
        let span = &timeline.spans()[0];
        assert_eq!(span.role, "", "unlabelled role keeps the default");
        assert_eq!(span.request, 0, "not in any request scope");
        assert!(!span.top_level_request);
        assert!(timeline.request_trace(42).is_empty());
    }
}
