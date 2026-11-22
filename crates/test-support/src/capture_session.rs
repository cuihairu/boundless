//! BW-23-02-01: bounded capture session control.
//!
//! A session runs over a set of profilers (one per role/process) and yields
//! exactly the events its filter selects — only the chosen processes, only
//! the chosen categories. Start bounds the window: everything recorded before
//! it is discarded; stop returns the events recorded in between, consuming
//! them from the profilers' buffers (the reference's entry-buffer swap each
//! `Profiler::tick`, plus its enable/category-mask gating, expressed as an
//! explicit session). A stopped session is inert until `start` reopens it.

use std::cell::Cell;

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
        (self.roles.is_empty() || self.roles.iter().any(|role| *role == identity.role))
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

    /// Stop capturing and return the events recorded since `start`, keeping
    /// only those the filter selects. A stopped session is inert: calling
    /// `stop` again (or any time the session is not active) returns nothing.
    pub fn stop(&self) -> Vec<CapturedEvent> {
        if !self.active.get() {
            return Vec::new();
        }
        self.active.set(false);
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
}
