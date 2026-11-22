//! BW-23-01-01: Rust interval and counter recording.
//!
//! Ports the core of the reference's `lib/cstdmf/profiler.cpp`: raw events
//! (`EVENT_START`, `EVENT_START_IDLE`, `EVENT_END`, `EVENT_COUNTER`) recorded
//! from any thread, processed into a per-thread hierarchy whose nodes carry a
//! name, an inclusive cycle count, the cycles spent in children and (for idle
//! sections) an idle cycle count — exactly the `DisplayEntry` shape in the
//! reference — and aggregated by name into call counts.
//!
//! Idle sections are accounted separately, like the reference: an idle
//! section's cycles are its own `inclusive_idle_cycles`, they propagate to the
//! enclosing section, and a non-idle section's `inclusive_cycles` excludes
//! them, so inclusive cycles measure active time only.
//!
//! The reference's `ProfileEntry` uses a 96-byte fixed name buffer because its
//! entry buffer is lock-free and shared with a background processor; here a
//! `String` is used because entries are owned by a mutex-guarded buffer and the
//! pool's lifetime rules (see the reference's `ENTRY_NAME_SIZE`) are not needed
//! for this bounded port.
//!
//! The clock is injectable (`with_clock`) so tests can drive a fixed sequence
//! of scoped calls with a pinned stamp source and assert both the call counts
//! and the nesting (the acceptance: 固定调用的次数和嵌套匹配) deterministically.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::thread::ThreadId;

use crate::time::stamp_now;

/// The reference's `Profiler::MAX_STACK_DEPTH`.
const MAX_STACK_DEPTH: usize = 64;

/// The reference's `Profiler::MAXIMUM_NUMBER_OF_THREADS`.
const MAX_THREADS: u32 = 96;

/// The reference's `Profiler::EventType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A scoped section opens (`EVENT_START`).
    Start,
    /// An idle scoped section opens (`EVENT_START_IDLE`).
    StartIdle,
    /// A scoped section closes (`EVENT_END`).
    End,
    /// A named counter value (`EVENT_COUNTER`).
    Counter,
}

/// The reference's `Profiler::EventCategory` bit flags, as a plain enum for
/// the bounded port (the reference ORs them; `Category::all()` is provided for
/// parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Native C++ / Rust code (`CATEGORY_CPP`).
    Cpp,
    /// GPU work (`CATEGORY_GPU`).
    Gpu,
    /// Python calls (`CATEGORY_PYTHON`).
    Python,
}

impl Category {
    /// `CATEGORY_ALL`.
    pub const fn all() -> [Category; 3] {
        [Category::Cpp, Category::Gpu, Category::Python]
    }
}

/// One raw profiler event, the `ProfileEntry` analog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileEntry {
    /// The section or counter name.
    pub name: String,
    /// The event kind.
    pub kind: EventKind,
    /// The event category.
    pub category: Category,
    /// The monotonic stamp when the event was recorded.
    pub time: u64,
    /// The small thread index this event was recorded on.
    pub thread: u32,
    /// For `Counter`, the value carried by the event.
    pub value: i32,
    /// The frame number current when the event was recorded.
    pub frame: u32,
    /// The request id this section belongs to (0 = no request). A request
    /// tags the section that handles a logical request, so spans with the
    /// same id can be followed across roles on a merged timeline.
    pub request: u32,
}

/// A node in the processed hierarchy — the `DisplayEntry` analog.
#[derive(Debug, Clone)]
pub struct DisplayNode {
    /// The section name.
    pub name: String,
    /// The index of the parent node, if this is not a root.
    pub parent: Option<usize>,
    /// The indices of the child nodes, in start order.
    pub children: Vec<usize>,
    /// The index of the next sibling under the same parent, if any.
    pub sibling: Option<usize>,
    /// The stamp at which the section opened.
    pub start: u64,
    /// Active cycles from open to close (idle excluded).
    pub inclusive_cycles: u64,
    /// Cycles spent in (non-idle) children.
    pub child_cycles: u64,
    /// Cycles this section (including its children) was idle.
    pub inclusive_idle_cycles: u64,
    /// Whether this section is an idle section (`EVENT_START_IDLE`).
    pub is_idle: bool,
    /// The thread the section ran on.
    pub thread: u32,
    /// The frame (tick) current when the section opened.
    pub frame: u32,
    /// The nearest enclosing request id (0 = outside any request scope).
    /// A section opened inside `scoped_request` inherits its id.
    pub request: u32,
    /// Whether the section got its matching `End`. A section whose thread
    /// exited (or whose guard was forgotten) stays open and is marked here —
    /// the missing marker for an interval that never closed.
    pub closed: bool,
}

/// Which process recorded a snapshot: the role name plus its pid. A profiler
/// that never gets a label reports `role: ""` and `pid: 0`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfilerIdentity {
    /// The role name (e.g. `cellapp01`, `baseapp`).
    pub role: String,
    /// The pid of the recording process.
    pub pid: u32,
}

/// One logical request's spans as a snapshot saw them: how often it was
/// handled, on which ticks and threads, and for how long. The request id is
/// the handle a merged cross-role timeline follows the request across with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSpan {
    /// The request id the spans were tagged with.
    pub request: u32,
    /// Number of outermost spans opened for this request.
    pub calls: u32,
    /// Summed inclusive cycles of the outermost spans.
    pub inclusive_cycles: u64,
    /// Distinct threads that handled it, sorted.
    pub threads: Vec<u32>,
    /// Ticks (frames) it was handled in, sorted.
    pub frames: Vec<u32>,
}

/// One aggregated per-name statistic — the `SortedEntry` analog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameStat {
    /// The section name.
    pub name: String,
    /// How many times the section was entered (nodes with this name).
    pub calls: u32,
    /// Total active cycles across those calls.
    pub inclusive_cycles: u64,
    /// Total cycles spent in children across those calls.
    pub child_cycles: u64,
    /// The thread indices the name appeared on, sorted.
    pub threads: Vec<u32>,
}

/// One aggregated per-name counter statistic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterStat {
    /// The counter name.
    pub name: String,
    /// The number of counter events recorded.
    pub count: u32,
    /// The last value recorded.
    pub last: i32,
    /// The sum of all recorded values.
    pub sum: i64,
}

/// One CPU sample: the call stack a thread had open at the sample instant,
/// outermost section first. Aggregated by (thread, stack) signature, the
/// samples locate where the time goes — the hot spot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackSample {
    /// The small thread index the stack belongs to.
    pub thread: u32,
    /// The open section names, outermost first.
    pub stack: Vec<String>,
}

/// The result of a snapshot: the per-thread hierarchy plus counter aggregates.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Which role/process recorded this snapshot.
    pub identity: ProfilerIdentity,
    /// Root node indices, ordered by thread then start stamp.
    pub roots: Vec<usize>,
    /// All nodes in the arena (indexed by the ids used in `roots`/links).
    pub nodes: Vec<DisplayNode>,
    /// Per-name section statistics, sorted by name.
    pub stats: Vec<NameStat>,
    /// Per-name counter statistics, sorted by name.
    pub counters: Vec<CounterStat>,
    /// Request spans, grouped by request id (sorted by id).
    pub requests: Vec<RequestSpan>,
    /// How many sections never closed (thread exit or a forgotten guard) —
    /// the missing markers. Zero means every interval closed normally.
    pub unbalanced: u32,
}

struct Inner {
    /// Raw events awaiting processing.
    entries: Vec<ProfileEntry>,
    /// Thread id -> small index, assigned on first use.
    thread_ids: HashMap<ThreadId, u32>,
    /// The next thread index to hand out.
    next_thread: u32,
    /// Small thread indices handed back by `remove_thread`, reused first.
    free_threads: Vec<u32>,
    /// The clock producing monotonic stamps.
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
    /// The current frame number.
    frame: u32,
    /// Which role/process this profiler records for.
    identity: ProfilerIdentity,
    /// The processed node arena, accumulating across ticks.
    nodes: Vec<DisplayNode>,
    /// Per-thread open-node stacks (indices into `nodes`).
    stacks: HashMap<u32, Vec<usize>>,
    /// Per-thread last child per stack depth, for sibling linking.
    tails: HashMap<u32, Vec<Option<usize>>>,
    /// Counter totals, accumulated as counter events are processed.
    counters: HashMap<String, CounterStat>,
}

/// The profiler: records events from any thread and aggregates them into a
/// per-name hierarchy and counter totals.
pub struct Profiler {
    inner: Mutex<Inner>,
}

impl Profiler {
    /// A profiler using the crate's monotonic `stamp_now` clock.
    pub fn new() -> Self {
        Self::with_clock(stamp_now)
    }

    /// A profiler with an injected clock, for deterministic tests.
    pub fn with_clock(clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        let mut thread_ids = HashMap::new();
        thread_ids.insert(std::thread::current().id(), 0);
        Self {
            inner: Mutex::new(Inner {
                entries: Vec::new(),
                thread_ids,
                next_thread: 1,
                clock: Box::new(clock),
                frame: 0,
                identity: ProfilerIdentity::default(),
                free_threads: Vec::new(),
                nodes: Vec::new(),
                stacks: HashMap::new(),
                tails: HashMap::new(),
                counters: HashMap::new(),
            }),
        }
    }

    /// Label this profiler as the recording for `role` with process id `pid`,
    /// so its snapshots join a cross-role timeline. Roles usually call this
    /// once at startup, before any section opens.
    pub fn set_identity(&self, role: &str, pid: u32) {
        let mut inner = self.inner.lock().expect("profiler lock");
        inner.identity = ProfilerIdentity {
            role: role.to_string(),
            pid,
        };
    }

    /// The role/process label currently set on this profiler.
    pub fn identity(&self) -> ProfilerIdentity {
        self.inner.lock().expect("profiler lock").identity.clone()
    }

    /// Take the raw events buffered since the last drain, in order — the
    /// entry-buffer swap the reference performs each tick. A capture session
    /// uses this to bound its window: draining at start discards the baseline,
    /// draining at stop yields exactly the events recorded in between. The
    /// drained events are consumed, so they are not processed into snapshots.
    pub fn drain_entries(&self) -> Vec<ProfileEntry> {
        let mut inner = self.inner.lock().expect("profiler lock");
        std::mem::take(&mut inner.entries)
    }

    /// Remove the calling thread — the reference's `removeThread`/
    /// `__onThreadEnd`. Any section it still has open is marked as a missing
    /// interval (never closed) rather than left silently open, and its thread
    /// index is freed for a later thread to reuse. Call it last, before the
    /// thread exits: no event may come from that thread afterwards.
    pub fn remove_thread(&self) {
        let mut inner = self.inner.lock().expect("profiler lock");
        // The stacks are built from buffered events, so drain them first;
        // otherwise the still-open sections would not exist yet. The reference
        // removes a thread at the tick when it is active.
        process_entries(&mut inner);
        let id = std::thread::current().id();
        let Some(&idx) = inner.thread_ids.get(&id) else {
            return;
        };
        if let Some(stack) = inner.stacks.remove(&idx) {
            for node in stack {
                if let Some(node) = inner.nodes.get_mut(node) {
                    node.closed = false;
                }
            }
        }
        inner.tails.remove(&idx);
        inner.thread_ids.remove(&id);
        inner.free_threads.push(idx);
    }

    /// Record a raw event — the reference's `addEntry`.
    pub fn add_entry(&self, name: &str, kind: EventKind, value: i32, category: Category) {
        self.add_entry_request(name, kind, value, category, 0)
    }

    /// Record a raw event tagged with a request id.
    pub fn add_entry_request(
        &self,
        name: &str,
        kind: EventKind,
        value: i32,
        category: Category,
        request: u32,
    ) {
        let mut inner = self.inner.lock().expect("profiler lock");
        let thread = thread_index(&mut inner);
        let entry = ProfileEntry {
            name: name.to_string(),
            kind,
            category,
            time: (inner.clock)(),
            thread,
            value,
            frame: inner.frame,
            request,
        };
        inner.entries.push(entry);
    }

    /// Record a named counter value — the reference's `PROFILER_COUNTER`.
    pub fn counter(&self, name: &str, value: i32) {
        self.add_entry(name, EventKind::Counter, value, Category::Cpp);
    }

    /// Open a scoped section and return the RAII guard. On drop the section is
    /// closed with an `End` event, like the reference's `ScopedProfiler`.
    pub fn scoped(&self, name: &str) -> ScopedProfiler<'_> {
        self.scoped_category(name, Category::Cpp)
    }

    /// Open a scoped section in a specific category. Both the opening `Start`
    /// and the closing `End` carry the category, like the reference's
    /// `ScopedDynamicStringProfiler`.
    pub fn scoped_category(&self, name: &str, category: Category) -> ScopedProfiler<'_> {
        self.add_entry(name, EventKind::Start, 0, category);
        ScopedProfiler {
            profiler: self,
            name: name.to_string(),
            category,
            request: 0,
        }
    }

    /// Open an idle scoped section — the reference's `PROFILER_IDLE_SCOPED`.
    pub fn scoped_idle(&self, name: &str) -> ScopedProfiler<'_> {
        self.add_entry(name, EventKind::StartIdle, 0, Category::Cpp);
        ScopedProfiler {
            profiler: self,
            name: name.to_string(),
            category: Category::Cpp,
            request: 0,
        }
    }

    /// Open a scoped section belonging to a logical request. Every role that
    /// touches the same request opens its handler under the same request id,
    /// which is the handle a cross-role timeline follows the request with.
    pub fn scoped_request(&self, request: u32, name: &str) -> ScopedProfiler<'_> {
        self.add_entry_request(name, EventKind::Start, 0, Category::Cpp, request);
        ScopedProfiler {
            profiler: self,
            name: name.to_string(),
            category: Category::Cpp,
            request,
        }
    }

    /// Close the current frame: advance the frame number and process any
    /// buffered entries into the hierarchy — the reference's `tick`.
    pub fn tick(&self) {
        let mut inner = self.inner.lock().expect("profiler lock");
        inner.frame = inner.frame.wrapping_add(1);
        process_entries(&mut inner);
    }

    /// Process any buffered entries and return the hierarchy and aggregates.
    pub fn snapshot(&self) -> Snapshot {
        let mut inner = self.inner.lock().expect("profiler lock");
        process_entries(&mut inner);
        let mut roots: Vec<usize> = (0..inner.nodes.len())
            .filter(|&i| inner.nodes[i].parent.is_none())
            .collect();
        roots.sort_by_key(|&i| (inner.nodes[i].thread, inner.nodes[i].start));
        let stats = aggregate_stats(&inner.nodes);
        let mut counters: Vec<CounterStat> = inner.counters.values().cloned().collect();
        counters.sort_by(|a, b| a.name.cmp(&b.name));
        let requests = aggregate_requests(&inner.nodes);
        let unbalanced = u32::try_from(inner.nodes.iter().filter(|node| !node.closed).count())
            .unwrap_or(u32::MAX);
        Snapshot {
            identity: inner.identity.clone(),
            roots,
            nodes: inner.nodes.clone(),
            stats,
            counters,
            requests,
            unbalanced,
        }
    }

    /// The call stack each thread has open right now, outermost section first —
    /// what a CPU sampler reads when it samples. Buffered events are drained
    /// first so the stacks are current. Threads with nothing open are omitted.
    pub fn sample_stacks(&self) -> Vec<StackSample> {
        let mut inner = self.inner.lock().expect("profiler lock");
        process_entries(&mut inner);
        let mut samples: Vec<StackSample> = inner
            .stacks
            .iter()
            .filter(|(_, stack)| !stack.is_empty())
            .map(|(&thread, stack)| StackSample {
                thread,
                stack: stack
                    .iter()
                    .map(|&idx| inner.nodes[idx].name.clone())
                    .collect(),
            })
            .collect();
        samples.sort_by_key(|sample| sample.thread);
        samples
    }
}

impl Default for Profiler {
    fn default() -> Self {
        Self::new()
    }
}

/// The RAII guard returned by `Profiler::scoped`/`scoped_category`/
/// `scoped_request`/`scoped_idle`. Dropping it closes the section, exactly
/// like the reference's `ScopedProfiler`; the category and request id the
/// section opened with are carried so the matching `End` closes the same
/// scope (the reference's `ScopedDynamicStringProfiler` does the same).
pub struct ScopedProfiler<'a> {
    profiler: &'a Profiler,
    name: String,
    category: Category,
    request: u32,
}

impl Drop for ScopedProfiler<'_> {
    fn drop(&mut self) {
        self.profiler
            .add_entry_request(&self.name, EventKind::End, 0, self.category, self.request);
    }
}

/// A named level gauge — the reference's `ProfilerLevelCounter`. Each change
/// records the new level as a counter event, and (like the reference's const
/// `increase`/`decrease`) the methods take `&self`.
pub struct LevelCounter<'a> {
    profiler: &'a Profiler,
    name: String,
    level: Cell<i32>,
}

impl<'a> LevelCounter<'a> {
    /// Create a level counter starting at zero.
    pub fn new(profiler: &'a Profiler, name: &str) -> Self {
        Self {
            profiler,
            name: name.to_string(),
            level: Cell::new(0),
        }
    }

    /// Increase the level by `amount` and record it — `increase`.
    pub fn increase(&self, amount: i32) {
        self.level.set(self.level.get() + amount);
        self.profiler.counter(&self.name, self.level.get());
    }

    /// Decrease the level by `amount` and record it — `decrease`.
    pub fn decrease(&self, amount: i32) {
        self.level.set(self.level.get() - amount);
        self.profiler.counter(&self.name, self.level.get());
    }
}

/// Assign the calling thread a small index, like the reference's thread table.
fn thread_index(inner: &mut Inner) -> u32 {
    let id = std::thread::current().id();
    if let Some(&idx) = inner.thread_ids.get(&id) {
        return idx;
    }
    // Reuse an index a removed thread handed back before growing the table.
    let idx = inner.free_threads.pop().unwrap_or_else(|| {
        let n = inner.next_thread;
        inner.next_thread += 1;
        n
    });
    // The reference caps at MAXIMUM_NUMBER_OF_THREADS; overflow indices clamp
    // so the table stays bounded.
    let clamped = idx.min(MAX_THREADS - 1);
    inner.thread_ids.insert(id, clamped);
    clamped
}

/// Process buffered entries into the hierarchy, mirroring the reference's
/// `processEntries`: a per-thread stack pairs each `Start` with its `End`,
/// links the node under the current stack top, and accumulates cycle totals.
fn process_entries(inner: &mut Inner) {
    let entries = std::mem::take(&mut inner.entries);
    for entry in &entries {
        let thread = entry.thread;
        let stack = inner.stacks.entry(thread).or_default();

        match entry.kind {
            EventKind::Start | EventKind::StartIdle => {
                if stack.len() >= MAX_STACK_DEPTH {
                    continue;
                }
                let node_idx = inner.nodes.len();
                let parent = stack.last().copied();
                let is_idle = entry.kind == EventKind::StartIdle;
                // A section inherits the nearest enclosing request; its own tag
                // wins, which makes request scopes nest.
                let request = if entry.request != 0 {
                    entry.request
                } else {
                    parent.map_or(0, |p| inner.nodes[p].request)
                };
                inner.nodes.push(DisplayNode {
                    name: entry.name.clone(),
                    parent,
                    children: Vec::new(),
                    sibling: None,
                    start: entry.time,
                    inclusive_cycles: 0,
                    child_cycles: 0,
                    inclusive_idle_cycles: 0,
                    is_idle,
                    thread,
                    frame: entry.frame,
                    request,
                    closed: true,
                });
                if let Some(p) = parent {
                    inner.nodes[p].children.push(node_idx);
                }
                // Link the node after the previous node at this depth so the
                // tree preserves start order per parent.
                let tails = inner.tails.entry(thread).or_default();
                if tails.len() < stack.len() + 1 {
                    tails.resize(stack.len() + 1, None);
                }
                if let Some(prev) = tails[stack.len()] {
                    inner.nodes[prev].sibling = Some(node_idx);
                }
                tails[stack.len()] = Some(node_idx);
                stack.push(node_idx);
            }
            EventKind::End => {
                // An end with an empty stack means the section started on
                // another thread or before this profiler existed; the
                // reference skips it.
                let Some(open) = stack.pop() else {
                    continue;
                };
                // A name mismatch means an unbalanced section (the reference
                // skips it); the RAII guard makes this unreachable in practice.
                if inner.nodes[open].name != entry.name {
                    continue;
                }
                let raw = entry.time.saturating_sub(inner.nodes[open].start);
                let parent = inner.nodes[open].parent;
                if inner.nodes[open].is_idle {
                    // An idle section's cycles are all idle; this overwrites
                    // any idle accumulated from children, like the reference.
                    inner.nodes[open].inclusive_cycles = raw;
                    inner.nodes[open].inclusive_idle_cycles = raw;
                } else {
                    let idle = inner.nodes[open].inclusive_idle_cycles;
                    inner.nodes[open].inclusive_cycles = raw.saturating_sub(idle);
                }
                if let Some(p) = parent {
                    let cycles = inner.nodes[open].inclusive_cycles;
                    let idle = inner.nodes[open].inclusive_idle_cycles;
                    inner.nodes[p].inclusive_idle_cycles += idle;
                    if !inner.nodes[open].is_idle {
                        inner.nodes[p].child_cycles += cycles.saturating_sub(idle);
                    }
                }
            }
            EventKind::Counter => {
                let stat =
                    inner
                        .counters
                        .entry(entry.name.clone())
                        .or_insert_with(|| CounterStat {
                            name: entry.name.clone(),
                            count: 0,
                            last: 0,
                            sum: 0,
                        });
                stat.count += 1;
                stat.last = entry.value;
                stat.sum += i64::from(entry.value);
            }
        }
    }
}

/// Aggregate the outermost spans per request id by walking the processed
/// nodes: only the nodes that open the request's scope count, so a request
/// that nests sections doesn't double count its children.
fn aggregate_requests(nodes: &[DisplayNode]) -> Vec<RequestSpan> {
    let mut spans: HashMap<u32, RequestSpan> = HashMap::new();
    for node in nodes {
        if node.request == 0 {
            continue;
        }
        // Top-level span: no parent, or the parent belongs to another request.
        let top_level = match node.parent {
            None => true,
            Some(p) => nodes[p].request != node.request,
        };
        if !top_level {
            continue;
        }
        let span = spans.entry(node.request).or_insert_with(|| RequestSpan {
            request: node.request,
            calls: 0,
            inclusive_cycles: 0,
            threads: Vec::new(),
            frames: Vec::new(),
        });
        span.calls += 1;
        span.inclusive_cycles += node.inclusive_cycles;
        if !span.threads.contains(&node.thread) {
            span.threads.push(node.thread);
        }
        if !span.frames.contains(&node.frame) {
            span.frames.push(node.frame);
        }
    }
    let mut spans: Vec<RequestSpan> = spans.into_values().collect();
    for span in &mut spans {
        span.threads.sort_unstable();
        span.frames.sort_unstable();
    }
    spans.sort_by_key(|s| s.request);
    spans
}

/// Aggregate per-name call totals by walking the processed nodes.
fn aggregate_stats(nodes: &[DisplayNode]) -> Vec<NameStat> {
    let mut by_name: HashMap<&str, NameStat> = HashMap::new();
    for node in nodes {
        let stat = by_name.entry(&node.name).or_insert_with(|| NameStat {
            name: node.name.clone(),
            calls: 0,
            inclusive_cycles: 0,
            child_cycles: 0,
            threads: Vec::new(),
        });
        stat.calls += 1;
        stat.inclusive_cycles += node.inclusive_cycles;
        stat.child_cycles += node.child_cycles;
        if !stat.threads.contains(&node.thread) {
            stat.threads.push(node.thread);
        }
    }
    let mut stats: Vec<NameStat> = by_name.into_values().collect();
    for stat in &mut stats {
        stat.threads.sort_unstable();
    }
    stats.sort_by(|a, b| a.name.cmp(&b.name));
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A deterministic stamp source, shared with the profiler's clock closure.
    type Pinned = Arc<AtomicU64>;

    fn pinned() -> Pinned {
        Arc::new(AtomicU64::new(0))
    }

    fn profiler_with(source: &Pinned) -> Profiler {
        // Clone the Arc so the clock closure owns it and is `'static`.
        let source = source.clone();
        Profiler::with_clock(move || source.load(Ordering::SeqCst))
    }

    fn advance(source: &Pinned, by: u64) {
        source.fetch_add(by, Ordering::SeqCst);
    }

    #[test]
    fn fixed_calls_report_counts_and_nesting() {
        let source = pinned();
        let prof = profiler_with(&source);

        // A fixed call pattern: two outer calls, each with one inner call.
        for _ in 0..2 {
            let _outer = prof.scoped("outer");
            advance(&source, 10);
            {
                let _inner = prof.scoped("inner");
                advance(&source, 5);
            }
            advance(&source, 5);
        }

        let snap = prof.snapshot();

        // Call counts: outer twice, inner twice.
        let outer = snap.stats.iter().find(|s| s.name == "outer").unwrap();
        let inner = snap.stats.iter().find(|s| s.name == "inner").unwrap();
        assert_eq!(outer.calls, 2, "outer call count");
        assert_eq!(inner.calls, 2, "inner call count");

        // Nesting: each outer node owns one inner child; inner has no children.
        let inner_node = snap.nodes.iter().find(|n| n.name == "inner").unwrap();
        assert!(inner_node.children.is_empty(), "inner has no children");
        let inner_parent = inner_node.parent.unwrap();
        assert_eq!(snap.nodes[inner_parent].name, "outer");
        let outer_children: Vec<usize> = snap
            .nodes
            .iter()
            .filter(|n| n.name == "outer")
            .flat_map(|n| n.children.iter().copied())
            .collect();
        assert_eq!(
            outer_children.len(),
            2,
            "two outer nodes each own one child"
        );

        // The two outer roots are siblings, in start order.
        let roots: Vec<usize> = snap
            .roots
            .iter()
            .copied()
            .filter(|&i| snap.nodes[i].name == "outer")
            .collect();
        assert_eq!(roots.len(), 2);
        assert_eq!(snap.nodes[roots[0]].sibling, Some(roots[1]));
        assert_eq!(snap.nodes[roots[1]].sibling, None);

        // Cycles: each outer spans 20 stamps, of which 5 in inner; totals
        // across the two calls double.
        assert_eq!(inner.inclusive_cycles, 10);
        assert_eq!(outer.inclusive_cycles, 40);
        assert_eq!(outer.child_cycles, 10);
        assert_eq!(inner.child_cycles, 0);
    }

    #[test]
    fn counter_events_aggregate_by_name() {
        let source = pinned();
        let prof = profiler_with(&source);

        prof.counter("load", 1);
        prof.counter("load", 3);
        prof.counter("load", 7);

        let snap = prof.snapshot();
        let load = snap.counters.iter().find(|c| c.name == "load").unwrap();
        assert_eq!(load.count, 3);
        assert_eq!(load.last, 7);
        assert_eq!(load.sum, 11);
    }

    #[test]
    fn level_counter_records_the_running_level() {
        let source = pinned();
        let prof = profiler_with(&source);

        let level = LevelCounter::new(&prof, "players");
        level.increase(2);
        level.increase(3);
        level.decrease(1);

        let snap = prof.snapshot();
        let players = snap.counters.iter().find(|c| c.name == "players").unwrap();
        assert_eq!(players.count, 3);
        assert_eq!(players.last, 4, "2 + 3 - 1");
        assert_eq!(players.sum, 2 + 5 + 4);
    }

    #[test]
    fn idle_sections_track_idle_cycles_separately() {
        let source = pinned();
        let prof = profiler_with(&source);

        {
            let _outer = prof.scoped("frame");
            advance(&source, 3);
            {
                let _idle = prof.scoped_idle("wait");
                advance(&source, 7);
            }
            advance(&source, 2);
        }

        let snap = prof.snapshot();
        let frame = snap.nodes.iter().find(|n| n.name == "frame").unwrap();
        let wait = snap.nodes.iter().find(|n| n.name == "wait").unwrap();
        assert!(wait.is_idle);
        assert_eq!(wait.inclusive_cycles, 7);
        // The frame spans 12 stamps, of which 7 idle: 5 active.
        assert_eq!(frame.inclusive_cycles, 5);
        // Idle child cycles are excluded from the parent's child cycles and
        // accounted as the parent's idle instead.
        assert_eq!(frame.child_cycles, 0);
        assert_eq!(frame.inclusive_idle_cycles, 7);
    }

    #[test]
    fn events_are_attributed_to_their_thread() {
        let source = pinned();
        let prof = profiler_with(&source);

        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _s = prof.scoped("worker");
            });
        });

        let snap = prof.snapshot();
        let worker = snap.stats.iter().find(|s| s.name == "worker").unwrap();
        assert_eq!(worker.calls, 1);
        assert_eq!(worker.threads, vec![1], "second thread gets index 1");
    }

    #[test]
    fn ticks_across_frames_accumulate_counts() {
        let source = pinned();
        let prof = profiler_with(&source);

        for _ in 0..3 {
            let _s = prof.scoped("update");
            advance(&source, 1);
            prof.tick();
        }

        let snap = prof.snapshot();
        let update = snap.stats.iter().find(|s| s.name == "update").unwrap();
        assert_eq!(update.calls, 3, "counts accumulate across ticks");
    }

    #[test]
    fn the_default_clock_is_monotonic() {
        let prof = Profiler::new();
        {
            let _s = prof.scoped("real");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let snap = prof.snapshot();
        let real = snap.stats.iter().find(|s| s.name == "real").unwrap();
        assert_eq!(real.calls, 1);
        // Nanosecond stamps: five milliseconds of wall time is at least a
        // million stamps, and sleeps never take negative time.
        assert!(
            real.inclusive_cycles >= 1_000_000,
            "{}",
            real.inclusive_cycles
        );
    }

    #[test]
    fn a_snapshot_carries_the_profiler_identity() {
        let prof = Profiler::with_clock(|| 0);
        prof.set_identity("cellapp01", 4242);

        let snap = prof.snapshot();
        assert_eq!(
            snap.identity,
            ProfilerIdentity {
                role: "cellapp01".to_string(),
                pid: 4242,
            }
        );
    }

    #[test]
    fn unlabelled_profilers_report_the_default_identity() {
        let snap = Profiler::with_clock(|| 0).snapshot();
        assert_eq!(snap.identity, ProfilerIdentity::default());
    }

    #[test]
    fn request_spans_group_by_their_request_id_and_record_the_tick() {
        let source = pinned();
        let prof = profiler_with(&source);

        // Two handlers for request 7 on tick 0, and one more on a later tick.
        for _ in 0..2 {
            let _s = prof.scoped_request(7, "handleRequest");
            advance(&source, 4);
        }
        prof.tick();
        advance(&source, 2);
        {
            let _s = prof.scoped_request(7, "handleRequest");
            advance(&source, 6);
        }
        prof.tick();

        let snap = prof.snapshot();
        assert_eq!(
            snap.requests,
            vec![RequestSpan {
                request: 7,
                calls: 3,
                inclusive_cycles: 14,
                threads: vec![0],
                frames: vec![0, 1],
            }]
        );
    }

    #[test]
    fn untagged_sections_nested_in_a_request_span_inherit_its_id() {
        let source = pinned();
        let prof = profiler_with(&source);

        {
            let _outer = prof.scoped_request(9, "handleRequest");
            // An untagged section inside the request inherits its id.
            let _inner = prof.scoped("resolveEntity");
            advance(&source, 10);
        }
        prof.tick();

        let snap = prof.snapshot();
        let inner = snap
            .nodes
            .iter()
            .find(|n| n.name == "resolveEntity")
            .unwrap();
        assert_eq!(inner.request, 9, "inner section belongs to the request");
        // Only the tagged span is the request's span; the inherited section is
        // not a second request span.
        assert_eq!(snap.requests.len(), 1);
        assert_eq!(snap.requests[0].calls, 1);
    }

    #[test]
    fn a_scoped_section_closes_in_the_category_it_opened_in() {
        let source = pinned();
        let prof = profiler_with(&source);

        {
            let _s = prof.scoped_category("pyWork", Category::Python);
            advance(&source, 3);
        }

        // The closing End carries the section's own category (the reference's
        // ScopedDynamicStringProfiler does the same), so a capture filtered
        // by category keeps whole sections, not halves.
        let entries = prof.drain_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].category, Category::Python);
        assert_eq!(entries[1].category, Category::Python);
    }

    #[test]
    fn a_caught_panic_still_closes_the_section() {
        let source = pinned();
        let prof = profiler_with(&source);

        // The RAII guard drops during unwinding, so an exception unwinding
        // through the section closes it normally — no missing marker.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _section = prof.scoped("handleRequest");
            advance(&source, 20);
            panic!("boom");
        }));
        assert!(result.is_err(), "the panic was caught");

        let snap = prof.snapshot();
        let handle = snap
            .nodes
            .iter()
            .find(|n| n.name == "handleRequest")
            .unwrap();
        assert!(handle.closed, "unwinding closed the interval");
        assert_eq!(handle.inclusive_cycles, 20);
        assert_eq!(snap.unbalanced, 0, "no interval is missing");
    }

    #[test]
    fn a_thread_that_exits_with_open_sections_marks_them_missing() {
        let source = pinned();
        let prof = profiler_with(&source);

        // A worker opens a section, drops its guard without closing it, then
        // removes itself — the thread exit analog of the reference's
        // `__onThreadEnd`.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Forget the guard: the section never ends, so its interval is
                // still open when the thread removes itself.
                std::mem::forget(prof.scoped("workerJob"));
                advance(&source, 8);
                prof.remove_thread();
            });
        });

        let snap = prof.snapshot();
        let job = snap.nodes.iter().find(|n| n.name == "workerJob").unwrap();
        assert!(!job.closed, "the open section is marked missing");
        assert_eq!(job.thread, 1, "the worker got the first free index");
        assert_eq!(snap.unbalanced, 1, "the snapshot reports the missing one");

        // The freed index is handed back to the next thread, which links a
        // fresh root rather than continuing the dead thread's section.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _s = prof.scoped("nextJob");
                advance(&source, 4);
            });
        });
        let snap = prof.snapshot();
        let next = snap.nodes.iter().find(|n| n.name == "nextJob").unwrap();
        assert_eq!(next.thread, 1, "the index was reused");
        assert!(next.closed, "the new section closes normally");
        assert_eq!(next.parent, None, "it is not nested under the dead section");
        assert_eq!(snap.unbalanced, 1, "only the original section is missing");
    }

    #[test]
    fn a_forgotten_guard_leaves_a_missing_marker() {
        let source = pinned();
        let prof = profiler_with(&source);

        // No End is ever emitted for the forgotten guard; removing the thread
        // flags the still-open interval as missing.
        std::mem::forget(prof.scoped("leakMe"));
        advance(&source, 6);
        prof.remove_thread();

        let snap = prof.snapshot();
        let leak = snap.nodes.iter().find(|n| n.name == "leakMe").unwrap();
        assert!(!leak.closed, "the forgotten interval is marked missing");
        assert_eq!(snap.unbalanced, 1);
    }
}
