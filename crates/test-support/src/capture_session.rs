//! BW-23-02-01 / BW-23-02-05: bounded capture session control.
//!
//! A session runs over a set of profilers (one per role/process) and yields
//! exactly the events its filter selects — only the chosen processes, only
//! the chosen categories. Start bounds the window: everything recorded before
//! it is discarded; stop returns the events recorded in between, consuming
//! them from the profilers' buffers (the reference's entry-buffer swap each
//! `Profiler::tick`, plus its enable/category-mask gating, expressed as an
//! explicit session). A stopped session is inert until `start` reopens it.
//!
//! A [`BoundedCapture`] adds the resource limits on top: a duration, after
//! which nothing more is captured, and a buffer cap, past which the events
//! are counted as lost instead of silently missing.

use std::cell::{Cell, RefCell};

use foundation::profiler::{Category, ProfileEntry, Profiler, ProfilerIdentity};

/// Which processes and event categories a session returns. Both lists are
/// allow-lists: empty means "everything" (the reference's `CATEGORY_ALL`).
#[derive(Debug, Clone)]
pub struct CaptureFilter {
    /// The role names whose events are kept; empty keeps every role.
    pub roles: Vec<String>,
    /// The categories kept; empty keeps every category.
    pub categories: Vec<Category>,
}

impl CaptureFilter {
    /// Keep everything from every process.
    pub fn all() -> Self {
        Self {
            roles: Vec::new(),
            categories: Vec::new(),
        }
    }

    /// Keep only the named roles' events.
    pub fn for_roles(roles: &[&str]) -> Self {
        Self {
            roles: roles.iter().map(|role| role.to_string()).collect(),
            categories: Vec::new(),
        }
    }

    /// Keep only the given categories.
    pub fn with_categories(categories: &[Category]) -> Self {
        Self {
            roles: Vec::new(),
            categories: categories.to_vec(),
        }
    }

    /// Whether one event (its recording identity and category) is kept.
    pub fn accepts(&self, identity: &ProfilerIdentity, category: Category) -> bool {
        (self.roles.is_empty() || self.roles.contains(&identity.role))
            && (self.categories.is_empty() || self.categories.contains(&category))
    }
}

/// One captured event: which process recorded it, and the raw entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedEvent {
    /// The role/pid of the recording process.
    pub identity: ProfilerIdentity,
    /// The raw event as the profiler buffered it.
    pub entry: ProfileEntry,
}

/// A bounded capture session over one or more profilers.
#[derive(Clone)]
pub struct CaptureSession<'a> {
    profilers: Vec<&'a Profiler>,
    filter: CaptureFilter,
    active: Cell<bool>,
}

impl<'a> CaptureSession<'a> {
    /// A session over the given profilers keeping everything.
    pub fn new(profilers: &[&'a Profiler]) -> Self {
        Self::with_filter(profilers, CaptureFilter::all())
    }

    /// A session with an explicit filter.
    pub fn with_filter(profilers: &[&'a Profiler], filter: CaptureFilter) -> Self {
        Self {
            profilers: profilers.to_vec(),
            filter,
            active: Cell::new(false),
        }
    }

    /// Whether the session is currently capturing.
    pub fn is_active(&self) -> bool {
        self.active.get()
    }

    /// Start capturing: discard everything buffered so far, so the session's
    /// window begins now.
    pub fn start(&self) {
        self.active.set(true);
        for profiler in &self.profilers {
            let _ = profiler.drain_entries();
        }
    }

    /// Return the events buffered since the last drain, keeping only those
    /// the filter selects. The session keeps capturing: this is the poll a
    /// bounded session uses to pull events as the load runs.
    pub fn drain(&self) -> Vec<CapturedEvent> {
        if !self.active.get() {
            return Vec::new();
        }
        let mut events = Vec::new();
        for profiler in &self.profilers {
            let identity = profiler.identity();
            for entry in profiler.drain_entries() {
                if self.filter.accepts(&identity, entry.category) {
                    events.push(CapturedEvent {
                        identity: identity.clone(),
                        entry,
                    });
                }
            }
        }
        events
    }

    /// Stop capturing and return the events recorded since `start`, keeping
    /// only those the filter selects. A stopped session is inert: calling
    /// `stop` again (or any time the session is not active) returns nothing.
    pub fn stop(&self) -> Vec<CapturedEvent> {
        let events = self.drain();
        self.active.set(false);
        events
    }
}

/// How long and how much a bounded session may capture. Either limit may be
/// 0, which means unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureLimit {
    /// The clock stamps after which the session stops capturing (0 = none).
    pub max_duration: u64,
    /// The number of events buffered before the overflow is counted as lost
    /// (0 = unlimited).
    pub max_events: usize,
}

impl CaptureLimit {
    /// Capture for as long and as much as the caller asks.
    pub fn unbounded() -> Self {
        Self {
            max_duration: 0,
            max_events: 0,
        }
    }

    /// Stop after `stamps` clock stamps.
    pub fn duration(stamps: u64) -> Self {
        Self {
            max_duration: stamps,
            max_events: 0,
        }
    }

    /// Buffer at most `count` events.
    pub fn events(count: usize) -> Self {
        Self {
            max_duration: 0,
            max_events: count,
        }
    }
}

/// Why a bounded session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The caller called `stop`.
    Explicit,
    /// The duration limit ran out: nothing recorded after it is captured.
    Expired,
    /// The buffer limit was reached: what came after was counted as lost.
    BufferFull,
}

/// What a bounded session captured: the events, what was lost, and why it
/// stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureReport {
    /// The events kept within the limits.
    pub events: Vec<CapturedEvent>,
    /// The events dropped once the buffer filled, so losses are reported
    /// rather than silently absent.
    pub dropped: usize,
    /// Why the session stopped.
    pub reason: StopReason,
}

/// A capture session bounded by a duration and a buffer. The caller drives
/// it: `start`, run the load, `poll` as the clock moves, `stop`. The clock
/// is read to see whether the duration limit is due.
pub struct BoundedCapture<'a> {
    session: CaptureSession<'a>,
    limit: CaptureLimit,
    clock: Box<dyn Fn() -> u64>,
    started_at: Cell<u64>,
    buffer: RefCell<Vec<CapturedEvent>>,
    dropped: Cell<usize>,
    outcome: Cell<Option<StopReason>>,
}

impl<'a> BoundedCapture<'a> {
    /// A session with the given limits, timing off `clock`.
    pub fn with_clock(
        profilers: &[&'a Profiler],
        limit: CaptureLimit,
        clock: impl Fn() -> u64 + 'static,
    ) -> Self {
        Self {
            session: CaptureSession::new(profilers),
            limit,
            clock: Box::new(clock),
            started_at: Cell::new(0),
            buffer: RefCell::new(Vec::new()),
            dropped: Cell::new(0),
            outcome: Cell::new(None),
        }
    }

    /// A session with the given limits and a filter.
    pub fn with_filter(
        profilers: &[&'a Profiler],
        filter: CaptureFilter,
        limit: CaptureLimit,
        clock: impl Fn() -> u64 + 'static,
    ) -> Self {
        Self {
            session: CaptureSession::with_filter(profilers, filter),
            limit,
            clock: Box::new(clock),
            started_at: Cell::new(0),
            buffer: RefCell::new(Vec::new()),
            dropped: Cell::new(0),
            outcome: Cell::new(None),
        }
    }

    /// Start capturing: discard everything buffered so far, so the window —
    /// and the duration limit — begin now.
    pub fn start(&self) {
        self.session.start();
        self.started_at.set((self.clock)());
        self.buffer.borrow_mut().clear();
        self.dropped.set(0);
        self.outcome.set(None);
    }

    /// Pull in the events buffered since the last poll, if the duration limit
    /// is not yet due. Events beyond the buffer limit are counted as lost.
    pub fn poll(&self) {
        if self.outcome.get().is_some() {
            return;
        }
        if self.limit.max_duration > 0
            && (self.clock)() - self.started_at.get() >= self.limit.max_duration
        {
            self.outcome.set(Some(StopReason::Expired));
            return;
        }
        let events = self.session.drain();
        let mut buffer = self.buffer.borrow_mut();
        for event in events {
            if self.limit.max_events > 0 && buffer.len() >= self.limit.max_events {
                self.dropped.set(self.dropped.get() + 1);
            } else {
                buffer.push(event);
            }
        }
    }

    /// Stop capturing and report what was captured: the events, the losses,
    /// and why the session stopped.
    pub fn stop(&self) -> CaptureReport {
        // One last pull, so events buffered after the final poll are not lost.
        if self.outcome.get().is_none() {
            self.poll();
        }
        let reason = match self.outcome.get() {
            Some(reason) => reason,
            None if self.dropped.get() > 0 => StopReason::BufferFull,
            None => StopReason::Explicit,
        };
        CaptureReport {
            events: self.buffer.take(),
            dropped: self.dropped.get(),
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// Record one section in every category on the given profiler.
    fn record_in_every_category(source: &Pinned, prof: &Profiler) {
        {
            let _s = prof.scoped("nativeSection");
            advance(source, 4);
        }
        {
            let _s = prof.scoped_category("pythonSection", Category::Python);
            advance(source, 2);
        }
        {
            let _s = prof.scoped_category("gpuSection", Category::Gpu);
            advance(source, 2);
        }
    }

    #[test]
    fn the_session_returns_only_the_selected_processes_and_categories() {
        let source = pinned();
        let cell = profiler_with(&source);
        let base = profiler_with(&source);
        cell.set_identity("cellapp01", 100);
        base.set_identity("baseapp01", 200);

        // Work before the session starts must not appear in the window.
        record_in_every_category(&source, &cell);
        record_in_every_category(&source, &base);

        let session = CaptureSession::with_filter(
            &[&cell, &base],
            CaptureFilter {
                roles: vec!["cellapp01".to_string()],
                categories: vec![Category::Cpp, Category::Python],
            },
        );
        session.start();
        record_in_every_category(&source, &cell);
        record_in_every_category(&source, &base);
        let events = session.stop();

        // Only cellapp01's events, and only its native and python ones:
        // 2 sections x (Start, End) each.
        assert_eq!(events.len(), 4, "2 sections x 2 events each");
        assert!(
            events
                .iter()
                .all(|e| e.identity.role == "cellapp01" && e.identity.pid == 100)
        );
        let names: Vec<&str> = events.iter().map(|e| e.entry.name.as_str()).collect();
        assert!(names.contains(&"nativeSection"));
        assert!(names.contains(&"pythonSection"));
        assert!(!names.contains(&"gpuSection"), "gpu is filtered out");
        assert!(!session.is_active(), "stop closes the session");
    }

    #[test]
    fn an_empty_filter_returns_every_process_and_category() {
        let source = pinned();
        let cell = profiler_with(&source);
        let base = profiler_with(&source);
        cell.set_identity("cellapp01", 100);
        base.set_identity("baseapp01", 200);

        let session = CaptureSession::new(&[&cell, &base]);
        session.start();
        record_in_every_category(&source, &cell);
        record_in_every_category(&source, &base);
        let events = session.stop();

        // 3 sections x 2 events x 2 roles = 12; all roles and categories.
        assert_eq!(events.len(), 12);
        let roles: Vec<&str> = events.iter().map(|e| e.identity.role.as_str()).collect();
        assert!(roles.contains(&"cellapp01") && roles.contains(&"baseapp01"));
        assert!(
            Category::all()
                .iter()
                .all(|category| events.iter().any(|e| e.entry.category == *category))
        );
    }

    #[test]
    fn category_selection_without_role_selection_covers_every_process() {
        let source = pinned();
        let cell = profiler_with(&source);
        let base = profiler_with(&source);
        cell.set_identity("cellapp01", 100);
        base.set_identity("baseapp01", 200);

        let session = CaptureSession::with_filter(
            &[&cell, &base],
            CaptureFilter::with_categories(&[Category::Gpu]),
        );
        session.start();
        record_in_every_category(&source, &cell);
        record_in_every_category(&source, &base);
        let events = session.stop();

        // Both roles, gpu section only: 2 events x 2 roles.
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|e| e.entry.category == Category::Gpu));
    }

    #[test]
    fn a_stopped_session_is_inert_until_started_again() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        let session = CaptureSession::new(&[&prof]);
        session.start();
        {
            let _s = prof.scoped("inside");
            advance(&source, 4);
        }
        let events = session.stop();
        assert_eq!(events.len(), 2);

        // Work after stop is buffered, but a stopped session is inert: no
        // second drain, and the events stay for whoever starts next.
        {
            let _s = prof.scoped("outside");
            advance(&source, 4);
        }
        assert_eq!(session.stop().len(), 0, "stopped sessions return nothing");

        // Restarting re-baselines, so only new events are captured.
        session.start();
        let events = session.stop();
        assert_eq!(events.len(), 0, "the outside events were not in the window");
    }

    fn record_section(source: &Pinned, prof: &Profiler, name: &str, by: u64) {
        {
            let _s = prof.scoped(name);
            advance(source, by);
        }
    }

    #[test]
    fn the_session_stops_when_the_duration_expires() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        let clock = source.clone();
        let session = BoundedCapture::with_clock(&[&prof], CaptureLimit::duration(50), move || {
            clock.load(Ordering::SeqCst)
        });
        session.start();

        // Two sections land before the deadline, one crosses it.
        record_section(&source, &prof, "early", 20);
        session.poll();
        record_section(&source, &prof, "mid", 20);
        session.poll();
        record_section(&source, &prof, "late", 20);
        session.poll();

        let report = session.stop();
        assert_eq!(report.reason, StopReason::Expired, "the deadline stops it");
        assert_eq!(report.events.len(), 4, "{:?}", report.events);
        assert_eq!(report.dropped, 0);
        let names: Vec<&str> = report
            .events
            .iter()
            .map(|event| event.entry.name.as_str())
            .collect();
        assert!(
            !names.contains(&"late"),
            "nothing after the deadline: {names:?}"
        );
    }

    #[test]
    fn a_full_buffer_reports_the_losses() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        let clock = source.clone();
        let session = BoundedCapture::with_clock(&[&prof], CaptureLimit::events(4), move || {
            clock.load(Ordering::SeqCst)
        });
        session.start();

        // Four sections of two events each against a four-event buffer: the
        // first two sections fit, the next two are counted as lost.
        for name in ["first", "second", "third", "fourth"] {
            record_section(&source, &prof, name, 4);
            session.poll();
        }

        let report = session.stop();
        assert_eq!(report.events.len(), 4, "the buffer holds its cap");
        assert_eq!(report.dropped, 4, "the losses are reported");
        assert_eq!(report.reason, StopReason::BufferFull);
    }

    #[test]
    fn an_unbounded_session_captures_everything() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        let clock = source.clone();
        let session = BoundedCapture::with_clock(&[&prof], CaptureLimit::unbounded(), move || {
            clock.load(Ordering::SeqCst)
        });
        session.start();

        for name in ["one", "two", "three"] {
            record_section(&source, &prof, name, 3);
            session.poll();
        }

        let report = session.stop();
        assert_eq!(report.events.len(), 6, "3 sections x 2 events");
        assert_eq!(report.dropped, 0);
        assert_eq!(report.reason, StopReason::Explicit);
    }
}
