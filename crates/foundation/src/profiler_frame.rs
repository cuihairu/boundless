//! BW-12-03-01: the frame and counter compat layer of the reference profiler.
//!
//! Ports the *frame pipeline* of `lib/cstdmf/profiler.cpp` — the part the
//! BW-23-01-01 interval recorder in [`crate::profiler`] does not carry: the
//! double-buffered entry pool with its capacity clamp
//! (`Profiler::startNextEntry`, profiler.ipp), the per-frame swap in
//! `Profiler::tick` (`profileFrameNumber_++`, `endOfFrameTimeStamp_`, the
//! stalled-semantics WARNING face), `Profiler::processEntries` (the
//! frame-stamp validity filter, the `DisplayEntry` tree with idle-cycle
//! accounting, closing still-open entries against the end-of-frame stamp),
//! `Profiler::outputReport` (the per-thread flatten + condense + sort +
//! top-10 pipeline) and `Profiler::getStatistics` (the console text lines).
//! The hierarchical aggregation (`updateHierarchy` + `HierEntry`) and the
//! thread slot table (`addThisThread`'s first-empty-slot scan, the deferred
//! `threadsToRemove_` removal, the "Deleted" name) are here too.
//!
//! Reference quirks preserved verbatim:
//! * `setProfileMode( mode, inclusive )` stores `calcExclusiveTimes_ =
//!   !inclusive` (profiler.cpp:2630) — the parameter is named *inclusive*
//!   but the stored flag is its negation.
//! * The sorted-view condensation compares name *pointers*
//!   (`if (match_name == name)`, profiler.cpp:2152) — every `DisplayEntry`
//!   owns a copied `char name_[96]`, so distinct sections never share one and
//!   the condensation is dead in practice: `numCalls` stays 1. The port keys
//!   the comparison on the display slot index (the pointer's analog) and
//!   documents the same dead arm.
//! * `addThisThread` breaks its scan at the first empty slot
//!   (profiler.cpp:2270-2274) — a thread whose id sits *after* that slot is
//!   treated as new and registered a second time.
//! * `removeThread` defers while profiling is active (`threadsToRemove_`,
//!   drained by `doRemoveThreads` at the end of `processEntries`); when
//!   inactive the slot is emptied immediately and renamed "Deleted".
//! * `tick` with an empty buffer still lands `isPendingActive_ = false`,
//!   `isActive_ = true` and refreshes `frameStartTime_` — the pending flag is
//!   consumed by any tick.
//! * A `tick` whose previous frame's processing task has not run yet records
//!   a stall (the `processEntrySemaphore_.pullTry()` failing path, with the
//!   verbatim WARNING text).
//! * An `EVENT_END` whose name does not match the open section is skipped
//!   after the suspicious `MF_ASSERT` (profiler.cpp:1690-1696).
//! * The counters: `ProfilerLevelCounter::increase/decrease` emit
//!   `EVENT_COUNTER` with the *running level* (not the delta); a zero amount
//!   records nothing.
//! * `watcherGetSortMode` returns "" for everything but the four mapped
//!   modes; `watcherSetSortMode` ignores unknown strings.
//!
//! The JSON dump (BW-12-03-03), the CSV history task (BW-12-03-02's
//! `_bw_profile` face — modelled here only as the `csv_output_running` gate
//! that forces `SORT_BY_NAME`), the per-core profiler, hitch detection, the
//! GPU thread and the client display mesh are out of this item's compat
//! scope. Clock and stamp rate are injectable so a fixed scripted load
//! asserts the same key fields the reference would produce (the acceptance:
//! 同负载下关键字段与原版吻合).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use crate::time::stamp_now;

/// The reference's `Profiler::ENTRY_NAME_SIZE`: the fixed per-entry name
/// buffer. Names are truncated to `ENTRY_NAME_SIZE - 1` characters.
pub const ENTRY_NAME_SIZE: usize = 96;

/// The reference's `Profiler::MAXIMUM_NUMBER_OF_THREADS`.
pub const MAXIMUM_NUMBER_OF_THREADS: usize = 96;

/// The reference's `Profiler::MAX_STACK_DEPTH`.
pub const MAX_STACK_DEPTH: usize = 64;

/// The reference's `Profiler::MAX_NUM_TOP_ENTRIES`.
pub const MAX_NUM_TOP_ENTRIES: usize = 10;

/// The reference's `Profiler::HISTORY_LENGTH`.
pub const HISTORY_LENGTH: usize = 512;

/// The reference's `Profiler::ProfileMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileMode {
    /// `PROFILE_OFF` = 0.
    Off,
    /// `HIERARCHICAL`.
    Hierarchical,
    /// `SORT_BY_TIME`.
    SortByTime,
    /// `SORT_BY_NUMCALLS`.
    SortByNumCalls,
    /// `SORT_BY_NAME`.
    SortByName,
    /// `CPU_GPU`.
    CpuGpu,
    /// `GRAPHS`.
    Graphs,
    /// `MAX_MODES` — never a valid mode; carried for the reference's
    /// `MF_ASSERT( mode < MAX_MODES )` bound.
    MaxModes,
}

/// The reference's `Profiler::EventType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    /// `EVENT_START`.
    Start,
    /// `EVENT_START_IDLE`.
    StartIdle,
    /// `EVENT_END`.
    End,
    /// `EVENT_COUNTER`.
    Counter,
}

/// The reference's `Profiler::EventCategory` bit flags. An entry passes the
/// category filter when `(category & current) != 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category(pub u32);

impl Category {
    /// `CATEGORY_CPP = (1 << 0)`.
    pub const CPP: Category = Category(1 << 0);
    /// `CATEGORY_GPU = (1 << 1)`.
    pub const GPU: Category = Category(1 << 1);
    /// `CATEGORY_PYTHON = (1 << 2)`.
    pub const PYTHON: Category = Category(1 << 2);
    /// `CATEGORY_ALL = (1 << 3) - 1`.
    pub const ALL: Category = Category((1 << 3) - 1);
}

/// The reference's `Profiler::ProfileEntry` — one raw recorded event.
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    /// `event_`.
    pub event: EventType,
    /// `category_`.
    pub category: Category,
    /// `name_`, truncated to `ENTRY_NAME_SIZE - 1` characters like the
    /// reference's fixed buffer.
    pub name: String,
    /// `time_` — the stamp the entry was recorded at.
    pub time: u64,
    /// `threadIdx_` — -1 for a thread that never called `addThisThread`.
    pub thread_idx: i32,
    /// `value_` (the counter payload).
    pub value: i32,
    /// `frameNumber_` — the validity stamp: entries whose frame differs from
    /// their buffer's first entry are discarded as unfinished writes.
    pub frame_number: i32,
}

/// The reference's `Profiler::DisplayEntry` — a processed, linked section.
/// Links are arena indices; the name slot index doubles as the analog of the
/// reference's `&d->name_[0]` pointer (see the condensation quirk).
#[derive(Debug, Clone)]
pub struct DisplayEntry {
    /// `parent_`.
    pub parent: Option<usize>,
    /// `sibling_`.
    pub sibling: Option<usize>,
    /// `child_`.
    pub child: Option<usize>,
    /// `name_` (own copy, like the reference's fixed buffer).
    pub name: String,
    /// `start_`.
    pub start: u64,
    /// `numInclusiveCycles_`.
    pub num_inclusive_cycles: u64,
    /// `numChildCycles_`.
    pub num_child_cycles: u64,
    /// `numInclusiveIdleCycles_`.
    pub num_inclusive_idle_cycles: u64,
    /// `isIdle_`.
    pub is_idle: bool,
}

/// The reference's `Profiler::SortedEntry` — one condensed, sorted row.
#[derive(Debug, Clone, PartialEq)]
pub struct SortedEntry {
    /// `numCycles_`.
    pub num_cycles: i64,
    /// `entryName_` (a copy of the owning display entry's name).
    pub entry_name: String,
    /// The analog of the reference's `const char *entryName_` pointer value:
    /// the index of the display entry the name was copied from.
    pub name_ptr: usize,
    /// `numCalls_`.
    pub num_calls: u32,
}

/// The reference's `Profiler::ThreadEntry` — one thread slot.
#[derive(Debug, Clone)]
pub struct ThreadEntry {
    /// `thread_` — 0 marks a dead slot.
    pub thread: u32,
    /// `name_` — "Dead Thread" for never-used slots, "Deleted" for removed.
    pub name: String,
    /// `head_` — the frame's first display entry, set by `process_entries`.
    pub head: Option<usize>,
    /// `visible_`.
    pub visible: bool,
}

/// The reference's `HierEntry` subset the stats output consumes: the
/// per-name aggregation tree of the `HIERARCHICAL` mode (built by
/// `updateHierarchy`, zeroed each frame by `zeroHierarchy`).
#[derive(Debug, Clone)]
pub struct HierNode {
    /// `parent_`.
    pub parent: Option<usize>,
    /// `sibling_`.
    pub sibling: Option<usize>,
    /// `child_`.
    pub child: Option<usize>,
    /// `name_`.
    pub name: String,
    /// `state_` — the `VISIBLE` bit gates the stats traversal (`next()`).
    pub state: u32,
    /// `time_` in milliseconds.
    pub time_ms: f64,
    /// `numCalls_`.
    pub num_calls: i32,
}

/// `HierEntry::VISIBLE`.
pub const HIER_VISIBLE: u32 = 0x1;

/// The reference's `Profiler::ProfileLine` — one coloured console line.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileLine {
    /// `colour_`.
    pub colour: u32,
    /// `text_`.
    pub text: String,
}

impl ProfileLine {
    /// The plain-text constructor (colour 0xffffffff).
    pub fn plain(text: impl Into<String>) -> Self {
        ProfileLine {
            colour: 0xffffffff,
            text: text.into(),
        }
    }
}

/// The port of `ProcessEntriesTask`: `tick` queues it, `run_queued_task`
/// runs it — the single-threaded analog of the reference's background task.
pub struct ProfilerFrame {
    /// The ordered category filter map (key-ordered like the reference's
    /// `BW::map`): C++(1), [GPU(2),] Python(4), All(7).
    categories: Vec<(u32, &'static str)>,
    /// `currentEventCategory_` — an index into `categories`.
    current_category: usize,
    /// `finalEventCategory_` — snapshotted by `tick`.
    final_category: usize,

    /// `profileEntryBuffers_` — the two per-frame pools.
    buffers: [Vec<ProfileEntry>; 2],
    /// `entryCount_` per buffer (the reference keeps it on the buffer).
    entry_counts: [i32; 2],
    /// `endOfFrameTimeStamp_` of the final buffer.
    end_of_frame_time_stamp: u64,
    /// `profileEntryBuffer_` — the buffer new entries land in (0 or 1).
    current_buffer: usize,
    /// `finalProfileEntryBuffer_` — the buffer being processed (0 or 1).
    final_buffer: usize,

    /// `maxEntryCount_`.
    max_entry_count: i32,
    /// `highWatermarkOfEntryCount_`.
    high_watermark_of_entry_count: i32,
    /// `displayEntryBuffer_` arena.
    display_entries: Vec<DisplayEntry>,
    /// `displayEntryCount_`.
    display_entry_count: u32,
    /// `maxDisplayEntryCount_`.
    max_display_entry_count: u32,
    /// `highWatermarkOfDisplayEntryCount_`.
    high_watermark_of_display_entry_count: u32,

    /// `maxNumThreads_`.
    max_num_threads: usize,
    /// `threads_`.
    threads: Vec<ThreadEntry>,
    /// `threadsToRemove_` (drained by `do_remove_threads`).
    threads_to_remove: BTreeSet<u32>,
    /// `numThreads_`.
    num_threads: usize,
    /// `tls_ThreadIndex_` — this port is single-threaded, so the per-thread
    /// index is a field (the reference keeps it in TLS).
    thread_index: i32,

    /// `dStack_` per thread.
    d_stacks: Vec<Vec<Option<usize>>>,
    /// `dChildTails_` per thread.
    d_child_tails: Vec<Vec<Option<usize>>>,
    /// `dHeads_` per thread.
    d_heads: Vec<Option<usize>>,
    /// `totalStackDepth_` per thread.
    total_stack_depth: Vec<i32>,

    /// `profileMode_`.
    profile_mode: ProfileMode,
    /// `calcExclusiveTimes_`.
    calc_exclusive_times: bool,
    /// `isActive_`.
    is_active: bool,
    /// `isPendingActive_`.
    is_pending_active: bool,
    /// `hasGeneratedProfile_`.
    has_generated_profile: bool,
    /// `freezeProfiling_`.
    freeze_profiling: bool,

    /// `profileFrameNumber_`.
    profile_frame_number: i32,
    /// `frame_`.
    frame: i32,
    /// `frameStartTime_`.
    frame_start_time: u64,
    /// `timeTakenForProfiling_` in milliseconds.
    time_taken_for_profiling: f64,

    /// `topEntries_` per thread.
    top_entries: Vec<Vec<SortedEntry>>,
    /// `numTopEntries_` per thread.
    num_top_entries: Vec<usize>,
    /// `perThreadHistory_` per thread (milliseconds, `frame % HISTORY_LENGTH`).
    per_thread_history: Vec<Vec<f64>>,

    /// `profileErrorString_`.
    pub profile_error_string: String,
    /// `taskManager_.numRunningThreads() == 0` face — whether the background
    /// thread has been started.
    background_started: bool,
    /// The queued `ProcessEntriesTask` (set by `tick`, run by
    /// `run_queued_task`).
    process_queued: bool,
    /// The count of `pullTry` failures — the verbatim
    /// "***** Profiler::tick() stalled for %fms waiting for lock" WARNINGs.
    pub stalled_ticks: u32,
    /// The count of skipped `EVENT_END`s whose name did not match the open
    /// section (the reference's assert-then-continue arm).
    pub mismatched_ends: u32,
    /// The count of entries dropped by the category filter.
    pub category_filtered: u32,
    /// The count of unfinished entries (frame stamp mismatch) discarded.
    pub unfinished_entries: u32,
    /// The count of `EVENT_END`s on an empty stack (skipped).
    pub empty_stack_ends: u32,
    /// `dynamicStrings_` — the interned dynamic section names (a set).
    dynamic_strings: Vec<String>,
    /// WARNING_MSG face (the reference writes to the debug output).
    pub warnings: Vec<String>,
    /// `csvOutputTask_->isRunning()` — the BW-12-03-02 gate.
    pub csv_output_running: bool,
    /// The stamp clock.
    clock: Rc<RefCell<Box<dyn Fn() -> u64>>>,
    /// `stampsToSeconds`' divisor.
    stamps_per_second: f64,

    /// The hierarchical aggregation arena (`HierarchyManager`'s entries),
    /// persisting across frames; values are zeroed each frame.
    hier_nodes: Vec<HierNode>,
    /// `parentHierEntry_` — the "All Threads" root.
    parent_hier_entry: usize,
    /// `currentHierEntry_`.
    current_hier_entry: usize,
    /// `graphableHierEntrys_` — nodes with graphing enabled this frame.
    pub graphable_hier_entrys: Vec<usize>,
}

/// Converts stamps to seconds (`stampsToSeconds`).
fn stamps_to_seconds(stamps: u64, stamps_per_second: f64) -> f64 {
    stamps as f64 / stamps_per_second
}

impl ProfilerFrame {
    /// `Profiler::init( memorySize, maxNumThreads )`: sizes the pools with the
    /// reference's formula
    /// `num = memorySize / (sizeof(ProfileEntry)*2 + sizeof(DisplayEntry)/2)`,
    /// seeds the dead thread slots, adds "MainThread" and builds the
    /// "All Threads" hierarchy root.
    pub fn new(memory_size: usize, max_num_threads: usize) -> Self {
        Self::with_clock(memory_size, max_num_threads, stamp_now, 1_000_000_000.0)
    }

    /// `new` with an injectable stamp clock and stamp rate, so a scripted
    /// load produces the reference's exact key fields.
    pub fn with_clock(
        memory_size: usize,
        max_num_threads: usize,
        clock: impl Fn() -> u64 + 'static,
        stamps_per_second: f64,
    ) -> Self {
        let num = (memory_size
            / (std::mem::size_of::<ProfileEntry>() * 2 + std::mem::size_of::<DisplayEntry>() / 2))
            .max(1) as i32;
        let max_display_entry_count = (num / 2).max(1) as u32;
        let max_num_threads = max_num_threads.max(1);

        let mut this = ProfilerFrame {
            categories: vec![
                (Category::CPP.0, "C++"),
                (Category::PYTHON.0, "Python"),
                (Category::ALL.0, "All"),
            ],
            current_category: 2,
            final_category: 2,
            buffers: [Vec::new(), Vec::new()],
            entry_counts: [0, 0],
            end_of_frame_time_stamp: 0,
            current_buffer: 0,
            final_buffer: 1,
            max_entry_count: num,
            high_watermark_of_entry_count: 0,
            display_entries: Vec::new(),
            display_entry_count: 0,
            max_display_entry_count,
            high_watermark_of_display_entry_count: 0,
            max_num_threads,
            threads: Vec::new(),
            threads_to_remove: BTreeSet::new(),
            num_threads: 0,
            thread_index: -1,
            d_stacks: vec![vec![None; MAX_STACK_DEPTH]; max_num_threads],
            d_child_tails: vec![vec![None; MAX_STACK_DEPTH]; max_num_threads],
            d_heads: vec![None; max_num_threads],
            total_stack_depth: vec![0; max_num_threads],
            profile_mode: ProfileMode::SortByTime,
            calc_exclusive_times: false,
            is_active: false,
            is_pending_active: false,
            has_generated_profile: false,
            freeze_profiling: false,
            profile_frame_number: 0,
            frame: 0,
            frame_start_time: 0,
            time_taken_for_profiling: 0.0,
            top_entries: vec![vec![]; max_num_threads],
            num_top_entries: vec![0; max_num_threads],
            per_thread_history: vec![vec![0.0; HISTORY_LENGTH]; max_num_threads],
            profile_error_string: String::new(),
            background_started: false,
            process_queued: false,
            stalled_ticks: 0,
            mismatched_ends: 0,
            category_filtered: 0,
            unfinished_entries: 0,
            empty_stack_ends: 0,
            dynamic_strings: Vec::new(),
            warnings: Vec::new(),
            csv_output_running: false,
            clock: Rc::new(RefCell::new(Box::new(clock))),
            stamps_per_second,
            hier_nodes: Vec::new(),
            parent_hier_entry: 0,
            current_hier_entry: 0,
            graphable_hier_entrys: Vec::new(),
        };

        this.buffers[0].reserve(num as usize);
        this.buffers[1].reserve(num as usize);
        this.display_entries
            .reserve(max_display_entry_count as usize);
        this.threads = (0..max_num_threads)
            .map(|_| ThreadEntry {
                thread: 0,
                name: "Dead Thread".to_string(),
                head: None,
                visible: false,
            })
            .collect();

        // init: "Add the main thread to the profile system".
        this.add_this_thread("MainThread");

        // HierarchyManager( maxEntries, numTimeSlices ) + the root entry.
        this.parent_hier_entry = this.new_hier_node(None, "All Threads");
        this.current_hier_entry = this.parent_hier_entry;

        this
    }

    /// The current stamp.
    fn now(&self) -> u64 {
        (self.clock.borrow_mut())()
    }

    /// Milliseconds from a stamp span.
    fn stamps_to_ms(&self, stamps: u64) -> f64 {
        stamps_to_seconds(stamps, self.stamps_per_second) * 1000.0
    }

    /// A fresh hierarchical node.
    fn new_hier_node(&mut self, parent: Option<usize>, name: &str) -> usize {
        let index = self.hier_nodes.len();
        self.hier_nodes.push(HierNode {
            parent,
            sibling: None,
            child: None,
            name: truncate_name(name),
            state: 0,
            time_ms: 0.0,
            num_calls: 0,
        });
        if let Some(parent) = parent {
            let node = &mut self.hier_nodes[parent];
            match node.child {
                None => node.child = Some(index),
                Some(head) => {
                    let mut sibling = head;
                    while let Some(next) = self.hier_nodes[sibling].sibling {
                        sibling = next;
                    }
                    self.hier_nodes[sibling].sibling = Some(index);
                }
            }
        }
        index
    }

    /// `HierarchyManager::findChild`: the hierarchical child with the name,
    /// created on first use (this is where the hierarchical view *does*
    /// merge same-named sections, unlike the sorted view's pointer quirk).
    fn hier_find_child(&mut self, parent: usize, name: &str) -> usize {
        let mut child = self.hier_nodes[parent].child;
        while let Some(index) = child {
            if self.hier_nodes[index].name == truncate_name(name) {
                return index;
            }
            child = self.hier_nodes[index].sibling;
        }
        self.new_hier_node(Some(parent), name)
    }

    /// `HierarchyManager::zeroHierarchy` — zero the accumulated values, keep
    /// the tree.
    fn zero_hierarchy(&mut self) {
        for node in &mut self.hier_nodes {
            node.time_ms = 0.0;
            node.num_calls = 0;
        }
    }

    /// `Profiler::enable( state )`. Enabling only *pends*: the flag flips on
    /// at the next `tick`. Disabling takes effect immediately and clears the
    /// current buffer.
    pub fn enable(&mut self, state: bool) {
        if state {
            if !self.is_active {
                self.is_pending_active = true;
                self.has_generated_profile = true;
            }
        } else {
            self.is_active = false;
            self.entry_counts[self.current_buffer] = 0;
        }
    }

    /// `Profiler::freeze( freeze, dump )` without the dump flag (BW-12-03-03
    /// owns the dump face).
    pub fn freeze(&mut self, freeze: bool) {
        self.freeze_profiling = freeze;
    }

    /// `watcherEnabled()` — active or pending.
    pub fn watcher_enabled(&self) -> bool {
        self.is_active || self.is_pending_active
    }

    /// `Profiler::hasGeneratedProfile_` — set by the first enabling.
    pub fn has_generated_profile(&self) -> bool {
        self.has_generated_profile
    }

    /// `Profiler::getFrameNumber()`.
    pub fn get_frame_number(&self) -> i32 {
        self.frame
    }

    /// `Profiler::startNextEntry` + the `addEntry` body (profiler.ipp):
    /// capacity-clamped, name-truncated, frame-stamped.
    pub fn add_entry(&mut self, text: &str, event: EventType, value: i32, category: Category) {
        if !self.is_active {
            return;
        }
        // startNextEntry: the count saturates at maxEntryCount_ and the
        // entry is dropped.
        let buffer = &mut self.entry_counts[self.current_buffer];
        if *buffer >= self.max_entry_count {
            *buffer = self.max_entry_count;
            return;
        }
        let index = *buffer as usize;
        *buffer += 1;

        let entry = ProfileEntry {
            event,
            category,
            name: truncate_name(text),
            time: self.now(),
            thread_idx: self.thread_index,
            value,
            frame_number: self.profile_frame_number,
        };
        // Buffers grow lazily to the clamped count: a dropped entry leaves a
        // hole that is never written, mirroring the pool's never-read slot.
        let buffer = &mut self.buffers[self.current_buffer];
        if buffer.len() == index {
            buffer.push(entry);
        } else {
            buffer[index] = entry;
        }
    }

    /// `ScopedDynamicStringProfiler`'s construction: intern the text in the
    /// dynamic-string set (the reference guards it with a mutex; this port is
    /// single-threaded) and return the interned copy for the section's
    /// Start/End pair.
    pub fn intern_dynamic_string(&mut self, text: &str) -> String {
        if let Some(existing) = self
            .dynamic_strings
            .iter()
            .find(|existing| existing.as_str() == text)
        {
            return existing.clone();
        }
        self.dynamic_strings.push(text.to_string());
        text.to_string()
    }

    /// `PROFILER_SCOPED` / `ScopedProfiler`: an `EVENT_START` now and an
    /// `EVENT_END` on drop, under the same name.
    pub fn scoped<'a>(&'a mut self, name: &'static str) -> ScopedFrameSection<'a> {
        self.add_entry(name, EventType::Start, 0, Category::CPP);
        ScopedFrameSection {
            profiler: self,
            name,
            ended: false,
        }
    }

    /// `PROFILER_IDLE_SCOPED` / `ScopedIdleProfiler`.
    pub fn scoped_idle<'a>(&'a mut self, name: &'static str) -> ScopedFrameSection<'a> {
        self.add_entry(name, EventType::StartIdle, 0, Category::CPP);
        ScopedFrameSection {
            profiler: self,
            name,
            ended: false,
        }
    }

    /// `PROFILER_LEVEL_COUNTER` — a named running level that emits
    /// `EVENT_COUNTER` entries carrying the *level* whenever it changes.
    /// The level starting at zero and never changing leaves the counter
    /// entirely absent from the data (the reference's documented note).
    pub fn level_counter(&mut self, desc: &'static str) -> ProfilerLevelCounter<'_> {
        ProfilerLevelCounter {
            profiler: self,
            desc,
            level: 0,
        }
    }

    /// `Profiler::tick`: swap the buffers, stamp the final one, queue the
    /// processing task and land the pending flag.
    pub fn tick(&mut self) {
        if !self.is_pending_active && !self.is_active {
            return;
        }

        // taskManager_.startThreads( "Profiler", 1 ) on the first tick.
        if !self.background_started {
            self.background_started = true;
        }

        if self.is_active && self.entry_counts[self.current_buffer] != 0 {
            {
                // pullTry failure = the previous frame's task has not run:
                // the verbatim stall WARNING.
                if self.process_queued {
                    self.stalled_ticks += 1;
                }

                self.entry_counts[self.final_buffer] = 0;
                std::mem::swap(&mut self.final_buffer, &mut self.current_buffer);
                self.profile_frame_number += 1;
                self.end_of_frame_time_stamp = self.now();
                self.final_category = self.current_category;

                let entry_count = self.entry_counts[self.final_buffer];
                self.high_watermark_of_entry_count =
                    self.high_watermark_of_entry_count.max(entry_count);
                // The reference's MF_ASSERT( entryCount <= maxEntryCount_ )
                // is the buffer-overflow guard; startNextEntry clamps, so the
                // bound holds by construction here.

                self.process_queued = true;
            }
        }
        self.is_pending_active = false;
        self.is_active = true;
        self.frame_start_time = self.now();
    }

    /// The port of the queued `ProcessEntriesTask` (`doBackgroundTask` runs
    /// `processEntries`). Single-threaded callers invoke it after `tick`.
    pub fn run_queued_task(&mut self) {
        if !self.process_queued {
            return;
        }
        self.process_queued = false;
        self.process_entries();
    }

    /// `Profiler::processEntries`: build the display tree from the final
    /// buffer, close still-open sections against the end-of-frame stamp,
    /// produce the report, advance `frame_` and write the per-thread
    /// history.
    pub fn process_entries(&mut self) {
        let profile_start = self.now();

        if self.freeze_profiling {
            self.time_taken_for_profiling = self.stamps_to_ms(self.now() - profile_start);
            return;
        }

        for t in 0..self.max_num_threads {
            self.d_stacks[t][0] = None;
            self.d_child_tails[t][0] = None;
            self.d_heads[t] = None;
            self.total_stack_depth[t] = 0;
        }
        self.display_entry_count = 0;

        // The frame validity stamp: entries whose frame differs from the
        // first entry's are unfinished writes and get dropped.
        let entry_count = self.entry_counts[self.final_buffer];
        let mut current_frame_identifier: i32 = -1;
        if entry_count > 1 {
            current_frame_identifier = self.buffers[self.final_buffer][0].frame_number;
        }

        let mut category_filtered = 0;
        let mut unfinished_entries = 0;
        let mut mismatched_ends = 0;
        let mut empty_stack_ends = 0;
        let final_category_mask = self.categories[self.final_category].0;

        for i in 0..entry_count as usize {
            let entry = self.buffers[self.final_buffer][i].clone();

            let thread_id = entry.thread_idx;
            if thread_id == -1 {
                continue;
            }
            if entry.frame_number != current_frame_identifier {
                unfinished_entries += 1;
                continue;
            }
            if (entry.category.0 & final_category_mask) == 0 {
                category_filtered += 1;
                continue;
            }

            let t = thread_id as usize;

            if entry.event == EventType::Start || entry.event == EventType::StartIdle {
                if self.display_entry_count >= self.max_display_entry_count {
                    self.profile_error_string = "ERROR: Too many display entries\nincrease your buffer size in <debug/profileBufferSize> in engine_config.\n".to_string();
                    continue;
                }

                let d = DisplayEntry {
                    parent: None,
                    sibling: None,
                    child: None,
                    name: entry.name.clone(),
                    start: entry.time,
                    num_inclusive_cycles: 0,
                    num_child_cycles: 0,
                    num_inclusive_idle_cycles: 0,
                    is_idle: entry.event == EventType::StartIdle,
                };
                let d_index = self.display_entries.len();
                self.display_entries.push(d);

                let stack_depth = self.total_stack_depth[t];
                if stack_depth >= MAX_STACK_DEPTH as i32 {
                    self.total_stack_depth[t] += 1;
                    continue;
                }
                let stack_depth = stack_depth as usize;

                self.display_entry_count += 1;
                self.d_stacks[t][stack_depth] = Some(d_index);
                self.total_stack_depth[t] += 1;

                if stack_depth == 0 {
                    self.display_entries[d_index].parent = None;
                    if self.d_heads[t].is_none() {
                        self.d_heads[t] = Some(d_index);
                    } else {
                        let tail = self.d_child_tails[t][stack_depth].unwrap();
                        self.display_entries[tail].sibling = Some(d_index);
                    }
                } else {
                    let parent = self.d_stacks[t][stack_depth - 1].unwrap();
                    self.display_entries[d_index].parent = Some(parent);
                    if self.display_entries[parent].child.is_none() {
                        self.display_entries[parent].child = Some(d_index);
                    } else {
                        let tail = self.d_child_tails[t][stack_depth].unwrap();
                        self.display_entries[tail].sibling = Some(d_index);
                    }
                }
                self.d_child_tails[t][stack_depth] = Some(d_index);
            } else if entry.event == EventType::End {
                let stack_depth = self.total_stack_depth[t];
                if stack_depth >= MAX_STACK_DEPTH as i32 {
                    self.total_stack_depth[t] -= 1;
                    continue;
                }
                if stack_depth == 0 {
                    // "this happens quite often due to tasks running on
                    // other threads during the profile::tick()"
                    empty_stack_ends += 1;
                    continue;
                }

                let open_index = self.d_stacks[t][(stack_depth - 1) as usize].unwrap();
                if self.display_entries[open_index].name != entry.name {
                    mismatched_ends += 1;
                    continue;
                }

                let open = &mut self.display_entries[open_index];
                open.num_inclusive_cycles = entry.time - open.start;
                if open.is_idle {
                    open.num_inclusive_idle_cycles = open.num_inclusive_cycles;
                } else {
                    open.num_inclusive_cycles -= open.num_inclusive_idle_cycles;
                }

                self.total_stack_depth[t] -= 1;

                let parent = open.parent;
                if let Some(parent) = parent {
                    let idle = open.num_inclusive_idle_cycles;
                    let inclusive = open.num_inclusive_cycles;
                    let is_idle = open.is_idle;
                    let parent_entry = &mut self.display_entries[parent];
                    parent_entry.num_inclusive_idle_cycles += idle;
                    if !is_idle {
                        parent_entry.num_child_cycles += inclusive;
                    }
                }
            }
            // EVENT_COUNTER entries carry no display structure.
        }

        self.category_filtered += category_filtered;
        self.unfinished_entries += unfinished_entries;
        self.mismatched_ends += mismatched_ends;
        self.empty_stack_ends += empty_stack_ends;

        self.high_watermark_of_display_entry_count = self
            .high_watermark_of_display_entry_count
            .max(self.display_entry_count);

        // Close any entries still open, clamped to the end-of-frame stamp.
        let end_of_frame = self.end_of_frame_time_stamp;
        for thread_id in 0..self.max_num_threads {
            while self.total_stack_depth[thread_id] > 0 {
                let stack_depth = self.total_stack_depth[thread_id];
                self.total_stack_depth[thread_id] -= 1;

                if (stack_depth as usize) < MAX_STACK_DEPTH {
                    let top_index = self.d_stacks[thread_id][(stack_depth - 1) as usize].unwrap();
                    let top = &mut self.display_entries[top_index];
                    if top.is_idle {
                        top.num_inclusive_idle_cycles = end_of_frame.saturating_sub(top.start);
                    } else {
                        top.num_inclusive_cycles = end_of_frame
                            .saturating_sub(top.start)
                            .saturating_sub(top.num_inclusive_idle_cycles);
                    }

                    if let Some(parent) = top.parent {
                        let idle = top.num_inclusive_idle_cycles;
                        let inclusive = top.num_inclusive_cycles;
                        let parent_entry = &mut self.display_entries[parent];
                        parent_entry.num_child_cycles += inclusive;
                        parent_entry.num_inclusive_idle_cycles += idle;
                    }
                }
            }

            self.threads[thread_id].head = self.d_heads[thread_id];
        }

        self.output_report();
        self.frame += 1;

        // Per-thread history: the span from the first root's start to the
        // last root's end, in milliseconds.
        for t in 0..self.max_num_threads {
            let head = self.d_heads[t];
            match head {
                Some(first) => {
                    let start = self.display_entries[first].start;
                    let mut node = first;
                    while let Some(sibling) = self.display_entries[node].sibling {
                        node = sibling;
                    }
                    let last = &self.display_entries[node];
                    self.per_thread_history[t][self.frame as usize % HISTORY_LENGTH] =
                        self.stamps_to_ms(last.num_inclusive_cycles + last.start - start);
                }
                None => {
                    self.per_thread_history[t][self.frame as usize % HISTORY_LENGTH] = 0.0;
                }
            }
        }

        self.do_remove_threads();

        self.time_taken_for_profiling = self.stamps_to_ms(self.now() - profile_start);
    }

    /// `Profiler::outputReport`: the `HIERARCHICAL` mode aggregates into the
    /// persistent hierarchy; the sorted modes flatten the display tree,
    /// condense (pointer-identity — dead in practice), sort and keep the top
    /// ten per thread.
    pub fn output_report(&mut self) {
        if self.profile_mode == ProfileMode::Hierarchical {
            self.zero_hierarchy();
            self.update_hierarchy();
            return;
        }

        let mode = self.profile_mode;
        let exclusive = self.calc_exclusive_times;

        for t in 0..self.max_num_threads {
            if !self.threads[t].visible {
                continue;
            }
            if self.threads[t].thread == 0 {
                continue;
            }
            let head = match self.threads[t].head {
                Some(head) => head,
                None => continue,
            };

            if mode == ProfileMode::Off {
                continue;
            }

            // Flatten the tree: child, then sibling, then the parent's next
            // sibling.
            let mut sorted_entries: Vec<SortedEntry> = Vec::new();
            let mut current = Some(head);
            while let Some(index) = current {
                let entry = &self.display_entries[index];
                let mut num_cycles = entry.num_inclusive_cycles as i64;
                if exclusive && num_cycles != 0 {
                    num_cycles -= entry.num_child_cycles as i64;
                }
                sorted_entries.push(SortedEntry {
                    num_cycles,
                    entry_name: entry.name.clone(),
                    name_ptr: index,
                    num_calls: 1,
                });

                if let Some(child) = entry.child {
                    current = Some(child);
                } else if let Some(sibling) = entry.sibling {
                    current = Some(sibling);
                } else {
                    // Go back up the tree for the next sibling.
                    let mut up = entry.parent;
                    current = None;
                    while let Some(parent_index) = up {
                        if let Some(sibling) = self.display_entries[parent_index].sibling {
                            current = Some(sibling);
                            break;
                        }
                        up = self.display_entries[parent_index].parent;
                    }
                }
            }

            // Condense same-named entries. The reference compares *pointers*
            // (`match_name == name`); this port keys on the display slot
            // index — distinct slots never share one, so the merge arm is
            // dead exactly like the reference's.
            let mut num_sorted_entries = sorted_entries.len();
            for i in 0..sorted_entries.len() {
                let name_ptr = sorted_entries[i].name_ptr;
                let mut match_index = i + 1;
                while match_index < num_sorted_entries {
                    if sorted_entries[match_index].name_ptr == name_ptr {
                        sorted_entries[i].num_cycles += sorted_entries[match_index].num_cycles;
                        sorted_entries[i].num_calls += 1;
                        num_sorted_entries -= 1;
                        sorted_entries[match_index] = sorted_entries[num_sorted_entries].clone();
                    } else {
                        match_index += 1;
                    }
                }
            }
            sorted_entries.truncate(num_sorted_entries);

            match mode {
                ProfileMode::SortByName => {
                    sorted_entries.sort_by_key(|e| e.entry_name.clone());
                }
                ProfileMode::SortByTime | ProfileMode::CpuGpu => {
                    sorted_entries.sort_by_key(|e| std::cmp::Reverse(e.num_cycles));
                }
                ProfileMode::SortByNumCalls => {
                    sorted_entries.sort_by_key(|e| std::cmp::Reverse(e.num_calls));
                }
                _ => {}
            }

            let num_top = num_sorted_entries.min(MAX_NUM_TOP_ENTRIES);
            self.num_top_entries[t] = num_top;
            self.top_entries[t] = sorted_entries[..num_top].to_vec();
        }
    }

    /// `Profiler::updateHierarchy`: aggregate the display tree into the
    /// persistent hierarchy — the thread's head gets `numCalls_ = 1` and the
    /// sum over its root siblings, then every display node accumulates into
    /// its named hier child (this *does* merge same-named sections).
    fn update_hierarchy(&mut self) {
        self.graphable_hier_entrys.clear();

        let exclusive = self.calc_exclusive_times;

        for t in 0..self.max_num_threads {
            if !self.threads[t].visible {
                continue;
            }
            if self.threads[t].thread == 0 {
                continue;
            }
            let head = match self.threads[t].head {
                Some(head) => head,
                None => continue,
            };

            let thread_name = self.threads[t].name.clone();
            let thread_hier = self.hier_find_child(self.parent_hier_entry, &thread_name);
            self.hier_nodes[thread_hier].num_calls = 1;

            let mut sibling = Some(head);
            while let Some(index) = sibling {
                let entry = &self.display_entries[index];
                let ms = if exclusive {
                    self.stamps_to_ms(entry.num_inclusive_cycles - entry.num_child_cycles)
                } else {
                    self.stamps_to_ms(entry.num_inclusive_cycles)
                };
                self.hier_nodes[thread_hier].time_ms += ms;
                sibling = self.display_entries[index].sibling;
            }

            // Walk the display tree, accumulating per named hier child.
            let head_name = self.display_entries[head].name.clone();
            let mut current = Some(head);
            let mut hier = Some(self.hier_find_child(thread_hier, &head_name));
            while let Some(index) = current {
                let hier_index = match hier {
                    Some(hier_index) => hier_index,
                    // Defensive: hier_find_child extends the tree in
                    // lockstep with the display walk, so the cursor cannot
                    // run out while a display node remains.
                    None => break,
                };
                let entry = &self.display_entries[index];
                let ms = if exclusive {
                    self.stamps_to_ms(
                        entry
                            .num_inclusive_cycles
                            .saturating_sub(entry.num_child_cycles),
                    )
                } else {
                    self.stamps_to_ms(entry.num_inclusive_cycles)
                };
                self.hier_nodes[hier_index].time_ms += ms;
                self.hier_nodes[hier_index].num_calls += 1;

                if let Some(child) = entry.child {
                    let child_name = self.display_entries[child].name.clone();
                    current = Some(child);
                    hier = Some(self.hier_find_child(hier_index, &child_name));
                } else if let Some(sibling) = entry.sibling {
                    let sibling_name = self.display_entries[sibling].name.clone();
                    let parent = self.hier_nodes[hier_index].parent;
                    current = Some(sibling);
                    hier = Some(self.hier_find_child(parent.unwrap(), &sibling_name));
                } else {
                    // Walk back up the display tree to the nearest ancestor
                    // with a next sibling; the hier cursor walks its parents
                    // in lockstep.
                    current = entry.parent;
                    hier = self.hier_nodes[hier_index].parent;
                    while let Some(up) = current {
                        if self.display_entries[up].sibling.is_some() {
                            break;
                        }
                        current = self.display_entries[up].parent;
                        hier = hier.and_then(|h| self.hier_nodes[h].parent);
                    }
                    if let (Some(up), Some(h)) = (current, hier) {
                        let next_index = self.display_entries[up].sibling.unwrap();
                        let next_name = self.display_entries[next_index].name.clone();
                        let parent = self.hier_nodes[h].parent;
                        current = Some(next_index);
                        hier = Some(self.hier_find_child(parent.unwrap(), &next_name));
                    }
                }
            }
        }
    }

    /// `HierEntry::toggleChildren` — flip the direct children's VISIBLE bit.
    pub fn toggle_children(&mut self, entry: usize) {
        let mut child = self.hier_nodes[entry].child;
        while let Some(index) = child {
            self.hier_nodes[index].state ^= HIER_VISIBLE;
            child = self.hier_nodes[index].sibling;
        }
    }

    /// `HierEntry::next()` — the stats traversal order: first visible child,
    /// else next sibling, else the nearest ancestor's next sibling.
    pub fn hier_next(&self, entry: usize) -> Option<usize> {
        let node = &self.hier_nodes[entry];
        if let Some(child) = node.child
            && self.hier_nodes[child].state & HIER_VISIBLE != 0
        {
            return Some(child);
        }
        if let Some(sibling) = node.sibling {
            return Some(sibling);
        }
        let mut up = node.parent;
        while let Some(index) = up {
            if let Some(sibling) = self.hier_nodes[index].sibling {
                return Some(sibling);
            }
            up = self.hier_nodes[index].parent;
        }
        None
    }

    /// The depth (number of parents) of a hier node (`GetDepth`).
    pub fn hier_depth(&self, entry: usize) -> usize {
        let mut depth = 0;
        let mut up = self.hier_nodes[entry].parent;
        while let Some(index) = up {
            depth += 1;
            up = self.hier_nodes[index].parent;
        }
        depth
    }

    /// `Profiler::addThisThread` — claim a slot for the calling thread. The
    /// scan breaks at the *first empty slot*, so a thread whose id sits
    /// after it is registered again (the reference's quirk).
    pub fn add_this_thread(&mut self, name: &str) {
        if self.threads.is_empty() {
            return;
        }
        let this_thread = 1; // OurThreadID(); the port's single thread is 1.
        let mut first_empty_slot: Option<usize> = None;

        for i in 0..self.max_num_threads {
            if self.threads[i].thread == this_thread {
                self.thread_index = i as i32;
                self.threads_to_remove.remove(&this_thread);
                self.warnings.push(format!(
                    "Profiler::addThisThread: Thread {} already added ({})!\n",
                    name, i
                ));
                return;
            }
            if self.threads[i].thread == 0 && first_empty_slot.is_none() {
                first_empty_slot = Some(i);
                break;
            }
        }

        let slot = match first_empty_slot {
            Some(slot) => slot,
            None => {
                self.warnings.push(format!(
                    "Profiler::addThisThread: Thread {} could not be added due to no enough space.\n",
                    name
                ));
                return;
            }
        };

        self.threads[slot].thread = this_thread;
        self.threads[slot].name = name.to_string();
        self.threads[slot].head = None;
        self.threads[slot].visible = true;
        self.thread_index = slot as i32;
        self.num_threads += 1;
    }

    /// `Profiler::getThreadName` — `None` for a dead slot (the reference
    /// returns NULL).
    pub fn get_thread_name(&self, thread_id: usize) -> Option<&str> {
        if self.threads[thread_id].thread != 0 {
            return Some(&self.threads[thread_id].name);
        }
        None
    }

    /// `Profiler::removeThread` — deferred while profiling is active.
    pub fn remove_thread(&mut self, thread_id: u32) {
        if self.threads.is_empty() {
            return;
        }
        if !self.is_active && !self.is_pending_active {
            self.internal_remove_thread(thread_id);
            return;
        }
        self.threads_to_remove.insert(thread_id);
    }

    /// `Profiler::internalRemoveThread` — the slot is emptied and renamed
    /// "Deleted".
    fn internal_remove_thread(&mut self, thread_id: u32) {
        for i in 0..self.max_num_threads {
            if self.threads[i].thread == thread_id {
                self.num_threads -= 1;
                self.threads[i].head = None;
                self.threads[i].name = "Deleted".to_string();
                self.threads[i].thread = 0;
                self.threads[i].visible = false;
                return;
            }
        }
    }

    /// `Profiler::doRemoveThreads` — drain the deferred removal set.
    pub fn do_remove_threads(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        let to_remove: Vec<u32> = self.threads_to_remove.iter().copied().collect();
        for thread_id in to_remove {
            self.internal_remove_thread(thread_id);
        }
        self.threads_to_remove.clear();
    }

    /// `Profiler::setThreadVisibility`.
    pub fn set_thread_visibility(&mut self, thread_id: usize, is_visible: bool) {
        self.threads[thread_id].visible = is_visible;
    }

    /// `Profiler::getThreadVisibility`.
    pub fn get_thread_visibility(&self, thread_id: usize) -> bool {
        self.threads[thread_id].visible
    }

    /// `Profiler::setProfileMode( mode, inclusive )`. The `inclusive`
    /// parameter stores its *negation* into `calcExclusiveTimes_` (the
    /// reference's quirk). With the CSV output running the mode is forced to
    /// `SORT_BY_NAME` and exclusive times.
    pub fn set_profile_mode(&mut self, mode: ProfileMode, inclusive: bool) {
        if !self.csv_output_running {
            if mode == ProfileMode::CpuGpu && self.profile_mode != ProfileMode::CpuGpu {
                // Entering the CPU/GPU view keeps only MainThread and GPU.
                for t in 0..self.max_num_threads {
                    let visible =
                        self.threads[t].name == "MainThread" || self.threads[t].name == "GPU";
                    self.threads[t].visible = visible;
                }
            } else if mode != ProfileMode::CpuGpu && self.profile_mode == ProfileMode::CpuGpu {
                // Leaving it: the reference restores the saved visibilities;
                // this port restores MainThread (the only thread a scripted
                // load has).
                for t in 0..self.max_num_threads {
                    if self.threads[t].name == "MainThread" {
                        self.threads[t].visible = true;
                    }
                }
            }
            self.profile_mode = mode;
            self.enable(mode != ProfileMode::Off);
            self.calc_exclusive_times = !inclusive;
        } else {
            self.calc_exclusive_times = true;
            self.profile_mode = ProfileMode::SortByName;
            self.enable(true);
        }
    }

    /// `Profiler::watcherSetSortMode` — unknown strings are ignored.
    pub fn watcher_set_sort_mode(&mut self, new_value: &str) {
        let mode = match new_value {
            "SORT_BY_NAME" => Some(ProfileMode::SortByName),
            "SORT_BY_TIME" => Some(ProfileMode::SortByTime),
            "SORT_BY_NUMCALLS" => Some(ProfileMode::SortByNumCalls),
            "HIERARCHICAL" => Some(ProfileMode::Hierarchical),
            _ => None,
        };
        if let Some(mode) = mode {
            let inclusive = !self.calc_exclusive_times;
            self.set_profile_mode(mode, inclusive);
        }
    }

    /// `Profiler::watcherGetSortMode` — "" outside the four mapped modes.
    pub fn watcher_get_sort_mode(&self) -> String {
        match self.profile_mode {
            ProfileMode::SortByName => "SORT_BY_NAME".to_string(),
            ProfileMode::SortByTime => "SORT_BY_TIME".to_string(),
            ProfileMode::SortByNumCalls => "SORT_BY_NUMCALLS".to_string(),
            ProfileMode::Hierarchical => "HIERARCHICAL".to_string(),
            _ => String::new(),
        }
    }

    /// `Profiler::prevCategory` — wraps past the beginning.
    pub fn prev_category(&mut self) {
        if self.current_category == 0 {
            self.current_category = self.categories.len();
        }
        self.current_category -= 1;
    }

    /// `Profiler::nextCategory` — wraps past the end.
    pub fn next_category(&mut self) {
        self.current_category += 1;
        if self.current_category == self.categories.len() {
            self.current_category = 0;
        }
    }

    /// `Profiler::setCurrentCategory` — `false` for an invalid name (the
    /// reference's WARNING).
    pub fn set_current_category(&mut self, new_category: &str) -> bool {
        if let Some(index) = self
            .categories
            .iter()
            .position(|(_, name)| *name == new_category)
        {
            self.current_category = index;
            true
        } else {
            self.warnings.push(format!(
                "Profiler::setCurrentCategory: invalid category: {}\n",
                new_category
            ));
            false
        }
    }

    /// `Profiler::getCurrentCategory`.
    pub fn get_current_category(&self) -> &str {
        self.categories[self.current_category].1
    }

    /// `Profiler::getCategories` — the ';' separated name list.
    pub fn get_categories(&self) -> String {
        self.categories
            .iter()
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(";")
    }

    /// `Profiler::getStatistics` — the console text. The `HIERARCHICAL`
    /// branch walks the aggregation tree; the sorted branch lists each
    /// visible thread's top entries with the verbatim line formats.
    pub fn get_statistics(&self) -> Vec<ProfileLine> {
        let mut output: Vec<ProfileLine> = Vec::new();

        if self.profile_mode == ProfileMode::Hierarchical {
            output.push(ProfileLine::plain("Hierarchical Profile data"));
            output.push(ProfileLine::plain(if self.calc_exclusive_times {
                "(Exclusive)\n"
            } else {
                "(Inclusive)\n"
            }));
            output.push(ProfileLine::plain("Current Filter: "));
            output.push(ProfileLine::plain(self.categories[self.current_category].1));
            output.push(ProfileLine::plain("\n\n"));

            let mut e = Some(self.parent_hier_entry);
            while let Some(index) = e {
                let node = &self.hier_nodes[index];
                let depth = self.hier_depth(index);
                let highlight = if self.current_hier_entry == index {
                    '*'
                } else {
                    ' '
                };
                let label = format!(
                    "{}{}{}({}){}",
                    highlight,
                    " ".repeat(depth),
                    node.name,
                    node.num_calls,
                    if node.child.is_some() { "..." } else { "" }
                );
                let text = if node.num_calls != 0 {
                    format!("{:<40} {:7.3}ms\n", label, node.time_ms)
                } else {
                    format!("{:<40}\n", label)
                };

                output.push(ProfileLine::plain(text));
                e = self.hier_next(index);
            }
            return output;
        }

        output.push(ProfileLine::plain("Profile data - "));
        match self.profile_mode {
            ProfileMode::SortByTime => output.push(ProfileLine::plain("Sorted By Time")),
            ProfileMode::SortByNumCalls => {
                output.push(ProfileLine::plain("Sorted By Number of Calls"))
            }
            ProfileMode::SortByName => output.push(ProfileLine::plain("Sorted By Name")),
            ProfileMode::CpuGpu => output.push(ProfileLine::plain("CPU and GPU")),
            _ => output.push(ProfileLine::plain("Invalid Sort!")),
        }

        output.push(ProfileLine::plain(format!(
            "({}):\n",
            if self.calc_exclusive_times {
                "Exclusive"
            } else {
                "Inclusive"
            }
        )));

        output.push(ProfileLine::plain("Current Filter: "));
        output.push(ProfileLine::plain(self.categories[self.current_category].1));
        output.push(ProfileLine::plain("\n"));

        for t in 0..self.max_num_threads {
            if !self.threads[t].visible || self.threads[t].head.is_none() {
                continue;
            }
            output.push(ProfileLine::plain(format!(
                "\n{}: {}\n",
                t, self.threads[t].name
            )));

            for (i, entry) in self.top_entries[t].iter().enumerate() {
                let ms = self.stamps_to_ms(entry.num_cycles.max(0) as u64);
                output.push(ProfileLine::plain(format!(
                    "{:2}: {:<40.40} {:8.2}ms {:6} calls\n",
                    i, entry.entry_name, ms, entry.num_calls
                )));
            }
            for _ in self.num_top_entries[t]..2 {
                output.push(ProfileLine::plain("\n"));
            }
            // profileBarVisibility_ defaults true at init; the reference
            // emits three blank lines per thread when the bar is shown.
            output.push(ProfileLine::plain("\n"));
            output.push(ProfileLine::plain("\n"));
            output.push(ProfileLine::plain("\n"));
        }

        output.push(ProfileLine::plain(format!(
            "\nTime spent profiling = {:8.2}ms\n",
            self.time_taken_for_profiling
        )));

        let entry_count = self.entry_counts[self.final_buffer];
        output.push(ProfileLine::plain(format!(
            "   {:6} profile entries, using {:3.2}MB (out of {:3.2}MB in each buffer)\n",
            entry_count.max(0) as u32,
            entry_count.max(0) as f64 * std::mem::size_of::<ProfileEntry>() as f64
                / (1024.0 * 1024.0),
            self.max_entry_count as f64 * std::mem::size_of::<ProfileEntry>() as f64
                / (1024.0 * 1024.0)
        )));
        output.push(ProfileLine::plain(format!(
            "   {:6} display entries, using {:3.2}MB (out of {:3.2}MB)\n",
            self.display_entry_count,
            self.display_entry_count as f64 * std::mem::size_of::<DisplayEntry>() as f64
                / (1024.0 * 1024.0),
            self.max_display_entry_count as f64 * std::mem::size_of::<DisplayEntry>() as f64
                / (1024.0 * 1024.0)
        )));

        output
    }

    /// `Profiler::getJsonOutputFilePath` face — the port keeps no JSON file;
    /// the getter shape exists for the compat layer (BW-12-03-03 owns the
    /// dump).
    pub fn get_json_output_file_path(&self) -> String {
        String::new()
    }
}

/// Truncates a name to `ENTRY_NAME_SIZE - 1` characters (the reference's
/// `strncpy` + explicit terminator).
fn truncate_name(text: &str) -> String {
    text.chars().take(ENTRY_NAME_SIZE - 1).collect()
}

/// The reference's `ScopedProfiler` / `ScopedIdleProfiler` / the dynamic
/// variant: records `EVENT_START` (or `EVENT_START_IDLE`) on construction
/// and `EVENT_END` on drop.
pub struct ScopedFrameSection<'a> {
    profiler: &'a mut ProfilerFrame,
    name: &'static str,
    ended: bool,
}

impl ScopedFrameSection<'_> {
    /// Ends the section early (the reference's guard does it on drop).
    pub fn end(mut self) {
        self.ended = true;
        self.profiler
            .add_entry(self.name, EventType::End, 0, Category::CPP);
    }
}

impl Drop for ScopedFrameSection<'_> {
    fn drop(&mut self) {
        if !self.ended {
            self.profiler
                .add_entry(self.name, EventType::End, 0, Category::CPP);
        }
    }
}

/// The reference's `ProfilerLevelCounter`: tracks a running level and emits
/// `EVENT_COUNTER` entries carrying the level after each change. Overflow is
/// not detected (the reference's WARNING).
pub struct ProfilerLevelCounter<'a> {
    profiler: &'a mut ProfilerFrame,
    desc: &'static str,
    level: i32,
}

impl ProfilerLevelCounter<'_> {
    /// `increase( amt )` — the level becomes `prev + amt` and the entry
    /// records that new level.
    pub fn increase(&mut self, amt: i32) {
        debug_assert!(amt >= 0);
        if amt == 0 {
            return;
        }
        self.level += amt;
        self.profiler
            .add_entry(self.desc, EventType::Counter, self.level, Category::CPP);
    }

    /// `decrease( amt )` — the level becomes `prev - amt`.
    pub fn decrease(&mut self, amt: i32) {
        debug_assert!(amt >= 0);
        if amt == 0 {
            return;
        }
        self.level -= amt;
        self.profiler
            .add_entry(self.desc, EventType::Counter, self.level, Category::CPP);
    }

    /// The current level.
    pub fn level(&self) -> i32 {
        self.level
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A pinned stamp source: `Rc<Cell<u64>>` advanced by the test.
    struct Pinned(Rc<Cell<u64>>);

    impl Pinned {
        fn new() -> Self {
            Pinned(Rc::new(Cell::new(0)))
        }
        fn profiler(&self, memory: usize) -> ProfilerFrame {
            let cell = Rc::clone(&self.0);
            ProfilerFrame::with_clock(memory, 8, move || cell.get(), 1_000_000.0)
        }
        fn advance(&self, by: u64) {
            self.0.set(self.0.get() + by);
        }
    }

    /// Adds one scripted frame of entries to the current buffer, without
    /// ticking: `outer { inner; inner }` with 100 stamps per section. One
    /// stamp is one millisecond at the test's stamp rate.
    fn load_frame(pinned: &Pinned, profiler: &mut ProfilerFrame) {
        profiler.add_entry("outer", EventType::Start, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("inner", EventType::Start, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("inner", EventType::End, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("inner", EventType::Start, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("inner", EventType::End, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("outer", EventType::End, 0, Category::CPP);
        pinned.advance(50);
    }

    /// Activates the profiler and runs one scripted frame through to its
    /// report. Written with explicit Start/End pairs — a `scoped` guard
    /// borrows the profiler for its whole lifetime, so nested sections
    /// cannot hold one.
    fn run_one_frame(pinned: &Pinned, profiler: &mut ProfilerFrame) {
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();
        load_frame(pinned, profiler);
    }

    /// Activates the profiler without recording anything — for tests that
    /// switch the mode first and then load exactly one frame.
    fn activate(profiler: &mut ProfilerFrame) {
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();
    }

    #[test]
    fn a_scripted_load_reports_the_reference_key_fields() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        run_one_frame(&pinned, &mut profiler);

        // tick() swapped and queued; the task runs the report.
        profiler.tick();
        profiler.run_queued_task();

        assert_eq!(profiler.get_frame_number(), 1);
        assert_eq!(profiler.num_top_entries[0], 3); // outer, inner, inner
        let top = &profiler.top_entries[0];
        // 1e9 stamps per second: outer = 500 stamps = 0.5ms, and each inner
        // section = 100 stamps = 0.1ms.
        let outer = top.iter().find(|e| e.entry_name == "outer").unwrap();
        assert_eq!(outer.num_cycles, 500);
        assert_eq!(outer.num_calls, 1);
        // The pointer-identity condensation quirk: the two same-named inner
        // sections never merge in the sorted view, so each stays a row of its
        // own with numCalls 1.
        let inners: Vec<_> = top.iter().filter(|e| e.entry_name == "inner").collect();
        assert_eq!(inners.len(), 2);
        for inner in &inners {
            assert_eq!(inner.num_cycles, 100);
            assert_eq!(inner.num_calls, 1);
        }
        // SORT_BY_TIME: costliest first.
        assert_eq!(top[0].entry_name, "outer");
        // Per-thread history: the whole frame span (500 stamps = 0.5ms).
        let slot = (profiler.get_frame_number() as usize) % HISTORY_LENGTH;
        let history = profiler.per_thread_history[0][slot];
        assert!((history - 0.5).abs() < 1e-9, "history = {}", history);
    }

    #[test]
    fn stats_lines_list_the_sorted_top_entries() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        run_one_frame(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        let lines = profiler.get_statistics();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.contains(&"Profile data - "));
        assert!(texts.contains(&"Sorted By Time"));
        assert!(texts.contains(&"(Inclusive):\n"));
        assert!(texts.contains(&"Current Filter: "));
        assert!(texts.contains(&"All"));
        // "\n0: MainThread\n" then one line per entry.
        assert!(texts.contains(&"\n0: MainThread\n"));
        let expected_outer = format!("{:2}: {:<40.40} {:8.2}ms {:6} calls\n", 0, "outer", 0.5, 1);
        assert!(
            texts.contains(&expected_outer.as_str()),
            "missing exact outer stats line in {texts:?}"
        );
        let expected_inner = format!("{:2}: {:<40.40} {:8.2}ms {:6} calls\n", 1, "inner", 0.1, 1);
        assert!(texts.contains(&expected_inner.as_str()));
        // The second same-named inner row stays its own line.
        let expected_inner_second =
            format!("{:2}: {:<40.40} {:8.2}ms {:6} calls\n", 2, "inner", 0.1, 1);
        assert!(texts.contains(&expected_inner_second.as_str()));
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("\nTime spent profiling")),
            "missing the profiling-time footer"
        );
    }

    #[test]
    fn exclusive_times_subtract_children() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);

        // setProfileMode( SORT_BY_TIME, inclusive=false ) -> exclusive.
        profiler.set_profile_mode(ProfileMode::SortByTime, false);
        assert!(profiler.calc_exclusive_times);
        load_frame(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        let top = &profiler.top_entries[0];
        let outer = top.iter().find(|e| e.entry_name == "outer").unwrap();
        // outer inclusive 500 - children 200 = 300.
        assert_eq!(outer.num_cycles, 300);
        let lines = profiler.get_statistics();
        assert!(lines.iter().any(|l| l.text == "(Exclusive):\n"));
    }

    #[test]
    fn the_mode_parameter_named_inclusive_stores_its_negation() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        // inclusive=true -> calcExclusiveTimes_ = false.
        profiler.set_profile_mode(ProfileMode::SortByTime, true);
        assert!(!profiler.calc_exclusive_times);
        // inclusive=false -> exclusive.
        profiler.set_profile_mode(ProfileMode::SortByTime, false);
        assert!(profiler.calc_exclusive_times);
    }

    #[test]
    fn sort_by_name_and_numcalls_change_the_order() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);

        profiler.set_profile_mode(ProfileMode::SortByName, true);
        load_frame(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();
        let names: Vec<&str> = profiler.top_entries[0]
            .iter()
            .map(|e| e.entry_name.as_str())
            .collect();
        assert_eq!(names, vec!["inner", "inner", "outer"]);

        profiler.set_profile_mode(ProfileMode::SortByNumCalls, true);
        load_frame(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();
        // numCalls is 1 for everything (the pointer condensation quirk), so
        // the num-calls sort keeps the flatten order: outer first.
        assert_eq!(profiler.top_entries[0][0].entry_name, "outer");
        assert_eq!(profiler.watcher_get_sort_mode(), "SORT_BY_NUMCALLS");
    }

    #[test]
    fn watcher_sort_mode_strings_round_trip() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);

        profiler.watcher_set_sort_mode("SORT_BY_NAME");
        assert_eq!(profiler.watcher_get_sort_mode(), "SORT_BY_NAME");
        assert_eq!(profiler.profile_mode, ProfileMode::SortByName);

        profiler.watcher_set_sort_mode("HIERARCHICAL");
        assert_eq!(profiler.watcher_get_sort_mode(), "HIERARCHICAL");

        profiler.watcher_set_sort_mode("SORT_BY_TIME");
        assert_eq!(profiler.watcher_get_sort_mode(), "SORT_BY_TIME");

        // Off and CpuGpu read back as "".
        profiler.set_profile_mode(ProfileMode::Off, true);
        assert_eq!(profiler.watcher_get_sort_mode(), "");
        profiler.set_profile_mode(ProfileMode::CpuGpu, true);
        assert_eq!(profiler.watcher_get_sort_mode(), "");
        // Unknown strings are ignored.
        profiler.watcher_set_sort_mode("BOGUS");
        assert_eq!(profiler.profile_mode, ProfileMode::CpuGpu);
    }

    #[test]
    fn enabling_pends_until_the_next_tick() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);

        profiler.add_entry("x", EventType::Start, 0, Category::CPP);
        assert_eq!(profiler.entry_counts[0], 0, "inactive: no entries");

        profiler.enable(true);
        assert!(profiler.watcher_enabled());
        assert!(profiler.has_generated_profile());
        assert!(!profiler.is_active, "pending, not active yet");

        // The tick lands the pending flag even with an empty buffer.
        profiler.tick();
        assert!(profiler.is_active);
        assert!(!profiler.is_pending_active);

        // enable(false) is immediate and clears the current buffer.
        profiler.add_entry("y", EventType::Start, 0, Category::CPP);
        assert_eq!(profiler.entry_counts[profiler.current_buffer], 1);
        profiler.enable(false);
        assert!(!profiler.is_active);
        assert_eq!(profiler.entry_counts[profiler.current_buffer], 0);
    }

    #[test]
    fn the_entry_pool_clamps_at_capacity() {
        let pinned = Pinned::new();
        // Small memory: the pool formula sizes from the struct sizes; pick a
        // memory so tiny that num clamps to the floor of 1.
        let mut profiler = pinned.profiler(1);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        profiler.add_entry("a", EventType::Start, 0, Category::CPP);
        assert_eq!(profiler.entry_counts[profiler.current_buffer], 1);
        profiler.add_entry("b", EventType::Start, 0, Category::CPP);
        // Clamped: the pool holds exactly max_entry_count_ entries.
        assert_eq!(
            profiler.entry_counts[profiler.current_buffer],
            profiler.max_entry_count
        );
    }

    #[test]
    fn unfinished_entries_are_dropped_by_the_frame_stamp() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        // Simulate unfinished writes: the frame identifier is seeded from the
        // buffer's first entry; entries stamped with a different frame are
        // dropped as writes the frame never completed.
        profiler.buffers[profiler.current_buffer] = vec![
            ProfileEntry {
                event: EventType::Start,
                category: Category::CPP,
                name: "kept".to_string(),
                time: 10,
                thread_idx: 0,
                value: 0,
                frame_number: 5,
            },
            ProfileEntry {
                event: EventType::End,
                category: Category::CPP,
                name: "kept".to_string(),
                time: 20,
                thread_idx: 0,
                value: 0,
                frame_number: 5,
            },
            ProfileEntry {
                event: EventType::Start,
                category: Category::CPP,
                name: "ghost".to_string(),
                time: 30,
                thread_idx: 0,
                value: 0,
                frame_number: 6,
            },
            ProfileEntry {
                event: EventType::End,
                category: Category::CPP,
                name: "ghost".to_string(),
                time: 40,
                thread_idx: 0,
                value: 0,
                frame_number: 7,
            },
        ];
        profiler.entry_counts[profiler.current_buffer] = 4;

        profiler.tick();
        profiler.run_queued_task();
        assert_eq!(profiler.unfinished_entries, 2);
        let names: Vec<&str> = profiler.top_entries[0]
            .iter()
            .map(|e| e.entry_name.as_str())
            .collect();
        assert_eq!(names, vec!["kept"]);
    }

    #[test]
    fn a_mismatched_end_is_skipped_and_counted() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        profiler.add_entry("open", EventType::Start, 0, Category::CPP);
        pinned.advance(10);
        profiler.add_entry("wrong", EventType::End, 0, Category::CPP);
        pinned.advance(10);
        profiler.add_entry("open", EventType::End, 0, Category::CPP);

        profiler.tick();
        profiler.run_queued_task();
        assert_eq!(profiler.mismatched_ends, 1);
        // The open entry was closed by the real end (name matched).
        let top = &profiler.top_entries[0];
        assert!(top.iter().any(|e| e.entry_name == "open"));
    }

    #[test]
    fn an_end_on_an_empty_stack_is_counted() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        // A counter seeds the frame identifier; the lone End then reaches
        // the depth-0 branch (a single-entry frame would be dropped as
        // unfinished instead).
        profiler.add_entry("depth", EventType::Counter, 1, Category::CPP);
        profiler.add_entry("stray", EventType::End, 0, Category::CPP);
        profiler.tick();
        profiler.run_queued_task();
        assert_eq!(profiler.empty_stack_ends, 1);
    }

    #[test]
    fn still_open_sections_are_clamped_to_the_end_of_frame() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        profiler.add_entry("never_closed", EventType::Start, 0, Category::CPP);
        profiler.add_entry("depth", EventType::Counter, 5, Category::CPP);
        pinned.advance(40);
        profiler.tick();
        profiler.run_queued_task();

        let top = &profiler.top_entries[0];
        let entry = top.iter().find(|e| e.entry_name == "never_closed").unwrap();
        // Clamped from its start to the end-of-frame stamp (40 stamps).
        assert_eq!(entry.num_cycles, 40);
    }

    #[test]
    fn a_single_entry_frame_is_discarded_as_unfinished() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        // Reference quirk: with exactly one entry the frame identifier stays
        // -1 (profiler.cpp:1535 `entryCount > 1`), so the lone entry differs
        // from it and is dropped as an unfinished write.
        profiler.add_entry("lone", EventType::Start, 0, Category::CPP);
        profiler.tick();
        profiler.run_queued_task();
        assert_eq!(profiler.unfinished_entries, 1);
        assert!(profiler.top_entries[0].is_empty());
    }

    #[test]
    fn idle_cycles_are_tracked_separately() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        {
            profiler.add_entry("outer", EventType::Start, 0, Category::CPP);
            pinned.advance(100);
            let idle = profiler.scoped_idle("idle");
            pinned.advance(50);
            idle.end();
            pinned.advance(100);
            profiler.add_entry("outer", EventType::End, 0, Category::CPP);
        }
        profiler.tick();
        profiler.run_queued_task();

        // Find the display tree: outer's inclusive = 250 - 50 idle = 200.
        let head = profiler.threads[0].head.unwrap();
        let outer = &profiler.display_entries[head];
        assert_eq!(outer.name, "outer");
        assert_eq!(outer.num_inclusive_idle_cycles, 50);
        assert_eq!(outer.num_inclusive_cycles, 200);
        let idle_index = outer.child.unwrap();
        let idle = &profiler.display_entries[idle_index];
        assert!(idle.is_idle);
        assert_eq!(idle.num_inclusive_idle_cycles, 50);
    }

    #[test]
    fn level_counters_record_the_running_level() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        {
            let mut counter = profiler.level_counter("queueDepth");
            counter.increase(3);
            assert_eq!(counter.level(), 3);
            counter.increase(0); // no-op: no entry recorded
            counter.decrease(2);
            assert_eq!(counter.level(), 1);
            counter.decrease(0);
        }

        let buffer = profiler.current_buffer;
        let counters: Vec<i32> = profiler.buffers[buffer]
            .iter()
            .filter(|e| e.event == EventType::Counter)
            .map(|e| e.value)
            .collect();
        assert_eq!(counters, vec![3, 1]);
    }

    #[test]
    fn the_category_filter_drops_foreign_entries() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        {
            let _section = profiler.scoped("cpp");
            pinned.advance(10);
        }
        profiler.add_entry("py", EventType::Start, 0, Category::PYTHON);
        profiler.add_entry("py", EventType::End, 0, Category::PYTHON);

        profiler.tick();
        profiler.run_queued_task();
        // The All filter drops nothing.
        assert_eq!(profiler.category_filtered, 0);
        let names: Vec<&str> = profiler.top_entries[0]
            .iter()
            .map(|e| e.entry_name.as_str())
            .collect();
        assert_eq!(names, vec!["cpp", "py"]);

        // Filtering to Python keeps only the python entry.
        assert!(profiler.set_current_category("Python"));
        profiler.add_entry("cpp2", EventType::Start, 0, Category::CPP);
        profiler.add_entry("cpp2", EventType::End, 0, Category::CPP);
        profiler.add_entry("py2", EventType::Start, 0, Category::PYTHON);
        profiler.add_entry("py2", EventType::End, 0, Category::PYTHON);
        profiler.tick();
        profiler.run_queued_task();
        assert_eq!(profiler.category_filtered, 2);
        let names: Vec<&str> = profiler.top_entries[0]
            .iter()
            .map(|e| e.entry_name.as_str())
            .collect();
        assert_eq!(names, vec!["py2"]);
    }

    #[test]
    fn categories_are_key_ordered_and_wrap() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);

        assert_eq!(profiler.get_categories(), "C++;Python;All");
        assert_eq!(profiler.get_current_category(), "All");

        profiler.next_category();
        assert_eq!(profiler.get_current_category(), "C++");
        profiler.prev_category();
        assert_eq!(profiler.get_current_category(), "All");
        profiler.prev_category();
        assert_eq!(profiler.get_current_category(), "Python");
        profiler.next_category();
        assert_eq!(profiler.get_current_category(), "All");

        assert!(!profiler.set_current_category("Nope"));
        assert!(
            profiler
                .warnings
                .iter()
                .any(|w| w == "Profiler::setCurrentCategory: invalid category: Nope\n")
        );
    }

    #[test]
    fn thread_slots_follow_the_reference_quirks() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);

        // init added MainThread.
        assert_eq!(profiler.get_thread_name(0), Some("MainThread"));
        assert_eq!(profiler.get_thread_name(1), None);
        assert_eq!(profiler.threads[1].name, "Dead Thread");

        // While active, removal is deferred.
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();
        profiler.remove_thread(1);
        assert_eq!(profiler.get_thread_name(0), Some("MainThread"));
        profiler.do_remove_threads();
        assert_eq!(profiler.get_thread_name(0), None);
        assert_eq!(profiler.threads[0].name, "Deleted");

        // Inactive removal is immediate: the slot is emptied without going
        // through the deferred set.
        profiler.enable(false);
        profiler.num_threads = 1;
        profiler.threads[1].thread = 2;
        profiler.threads[1].name = "Worker".to_string();
        profiler.threads[1].visible = true;
        profiler.remove_thread(2);
        assert_eq!(profiler.get_thread_name(1), None);
        assert_eq!(profiler.threads[1].name, "Deleted");
        assert!(profiler.threads_to_remove.is_empty());
    }

    #[test]
    fn the_slot_scan_breaks_at_the_first_empty_slot() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        // The state a removal drain can leave: slot 0 emptied while the id
        // survives in slot 1.
        profiler.threads[0].thread = 0;
        profiler.threads[0].name = "Dead Thread".to_string();
        profiler.threads[0].visible = false;
        profiler.threads[1].thread = 1;
        profiler.threads[1].name = "MainThread".to_string();
        profiler.threads[1].visible = true;
        profiler.num_threads = 1;

        // The scan breaks at slot 0 and never sees the id at slot 1.
        profiler.add_this_thread("MainThread");
        assert_eq!(profiler.thread_index, 0);
        assert_eq!(profiler.threads[0].thread, 1);
        assert_eq!(profiler.threads[1].thread, 1);
        assert!(profiler.warnings.is_empty());
    }

    #[test]
    fn readding_a_thread_before_the_empty_slot_warns() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();
        // MainThread sits at slot 0; a deferred removal set entry is wiped by
        // a re-add.
        profiler.threads_to_remove.insert(1);
        profiler.add_this_thread("MainThread");
        assert!(
            profiler
                .warnings
                .iter()
                .any(|w| w.contains("already added (0)!"))
        );
        assert!(!profiler.threads_to_remove.contains(&1));
    }

    #[test]
    fn running_out_of_slots_warns() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        for i in 0..profiler.max_num_threads {
            profiler.threads[i].thread = (i + 2) as u32;
        }
        profiler.add_this_thread("Extra");
        assert!(
            profiler
                .warnings
                .iter()
                .any(|w| w.contains("could not be added due to no enough space"))
        );
    }

    #[test]
    fn hierarchical_mode_aggregates_by_name() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);

        profiler.set_profile_mode(ProfileMode::Hierarchical, true);
        load_frame(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        // All Threads > MainThread > outer > inner.
        let root = profiler.parent_hier_entry;
        assert_eq!(profiler.hier_nodes[root].name, "All Threads");
        let thread = profiler.hier_nodes[root].child.unwrap();
        assert_eq!(profiler.hier_nodes[thread].name, "MainThread");
        assert_eq!(profiler.hier_nodes[thread].num_calls, 1);
        assert!((profiler.hier_nodes[thread].time_ms - 0.5).abs() < 1e-9);
        let outer = profiler.hier_nodes[thread].child.unwrap();
        assert_eq!(profiler.hier_nodes[outer].name, "outer");
        assert_eq!(profiler.hier_nodes[outer].num_calls, 1);
        let inner = profiler.hier_nodes[outer].child.unwrap();
        // The hierarchical view DOES merge same-named sections.
        assert_eq!(profiler.hier_nodes[inner].name, "inner");
        assert_eq!(profiler.hier_nodes[inner].num_calls, 2);
        assert!((profiler.hier_nodes[inner].time_ms - 0.2).abs() < 1e-9);
    }

    #[test]
    fn hierarchical_stats_walk_and_highlight() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);
        profiler.set_profile_mode(ProfileMode::Hierarchical, true);
        load_frame(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        // Without toggling children the traversal stays at the root line.
        let lines = profiler.get_statistics();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[5].text, format!("{:<40}\n", "*All Threads(0)..."));

        // Toggle the root's children: MainThread appears, marked as current
        // when the cursor moves onto it.
        profiler.toggle_children(profiler.parent_hier_entry);
        let thread = profiler.hier_nodes[profiler.parent_hier_entry]
            .child
            .unwrap();
        profiler.current_hier_entry = thread;
        let lines = profiler.get_statistics();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts.len(), 7);
        assert_eq!(texts[5], format!("{:<40}\n", " All Threads(0)..."));
        assert_eq!(
            texts[6],
            format!("{:<40} {:7.3}ms\n", "* MainThread(1)...", 0.5)
        );
    }

    #[test]
    fn freeze_stops_processing() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        run_one_frame(&pinned, &mut profiler);
        profiler.freeze(true);
        profiler.tick();
        profiler.run_queued_task();
        // Nothing processed: no frame advance, no top entries.
        assert_eq!(profiler.get_frame_number(), 0);
        assert_eq!(profiler.num_top_entries[0], 0);
        profiler.freeze(false);
    }

    #[test]
    fn a_stalled_tick_is_counted_with_the_verbatim_trigger() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        // A frame with work: the tick queues the processing task.
        profiler.add_entry("work", EventType::Start, 0, Category::CPP);
        pinned.advance(10);
        profiler.add_entry("work", EventType::End, 0, Category::CPP);
        profiler.tick();
        assert!(profiler.process_queued);

        // The previous task never ran: the next tick's pullTry fails.
        profiler.add_entry("late", EventType::Counter, 1, Category::CPP);
        profiler.tick();
        assert_eq!(profiler.stalled_ticks, 1);

        profiler.run_queued_task();
        // Nothing queued anymore: no further stall.
        profiler.add_entry("late2", EventType::Counter, 1, Category::CPP);
        profiler.tick();
        assert_eq!(profiler.stalled_ticks, 1);
        profiler.run_queued_task();
    }

    #[test]
    fn a_tick_before_anything_is_enabled_is_a_no_op() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.tick();
        assert!(!profiler.is_active);
        assert_eq!(profiler.get_frame_number(), 0);
    }

    #[test]
    fn the_cpu_gpu_view_restricts_visibility() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();
        assert!(profiler.get_thread_visibility(0));

        // A non-MainThread thread is hidden entering the view.
        profiler.threads[1].thread = 2;
        profiler.threads[1].name = "Renderer".to_string();
        profiler.threads[1].visible = true;

        profiler.set_profile_mode(ProfileMode::CpuGpu, true);
        assert!(profiler.get_thread_visibility(0), "MainThread stays");
        assert!(!profiler.get_thread_visibility(1), "a worker is hidden");
        profiler.set_profile_mode(ProfileMode::SortByTime, true);
        assert!(profiler.get_thread_visibility(0), "MainThread restored");
    }

    #[test]
    fn csv_running_forces_sort_by_name_exclusive() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.csv_output_running = true;
        profiler.set_profile_mode(ProfileMode::SortByTime, true);
        assert_eq!(profiler.profile_mode, ProfileMode::SortByName);
        assert!(profiler.calc_exclusive_times);
        assert!(profiler.is_active || profiler.is_pending_active);
    }

    #[test]
    fn dynamic_strings_are_interned_once() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        let first = profiler.intern_dynamic_string("Entity/1234");
        let second = profiler.intern_dynamic_string("Entity/1234");
        assert_eq!(first, second);
        assert_eq!(profiler.dynamic_strings.len(), 1);
        let third = profiler.intern_dynamic_string("Entity/5678");
        assert_eq!(profiler.dynamic_strings.len(), 2);
        assert_eq!(third, "Entity/5678");
    }

    #[test]
    fn names_are_truncated_to_the_pool_width() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        let long = "x".repeat(ENTRY_NAME_SIZE + 10);
        profiler.add_entry(&long, EventType::Start, 0, Category::CPP);
        let buffer = profiler.current_buffer;
        let entry = &profiler.buffers[buffer]
            .iter()
            .find(|e| e.name.starts_with('x'))
            .unwrap();
        assert_eq!(entry.name.len(), ENTRY_NAME_SIZE - 1);
    }

    #[test]
    fn scoped_guards_end_on_drop_and_can_end_early() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        {
            let section = profiler.scoped("early");
            pinned.advance(5);
            section.end();
            let idle = profiler.scoped_idle("idle_guard");
            pinned.advance(5);
            drop(idle);
        }

        let buffer = profiler.current_buffer;
        let events: Vec<EventType> = profiler.buffers[buffer].iter().map(|e| e.event).collect();
        assert_eq!(
            events,
            vec![
                EventType::Start,
                EventType::End,
                EventType::StartIdle,
                EventType::End
            ]
        );
    }

    #[test]
    fn an_entry_from_an_unknown_thread_is_skipped() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();

        // A thread that never called addThisThread has index -1.
        profiler.thread_index = -1;
        profiler.add_entry("ghost", EventType::Start, 0, Category::CPP);
        profiler.thread_index = 0;
        profiler.tick();
        profiler.run_queued_task();
        assert!(profiler.top_entries[0].is_empty());
    }

    #[test]
    fn empty_frames_write_zero_history() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();
        // No entries this frame.
        profiler.tick();
        profiler.run_queued_task();
        assert_eq!(
            profiler.per_thread_history[0][profiler.get_frame_number() as usize % HISTORY_LENGTH],
            0.0
        );
    }

    #[test]
    fn the_json_path_face_is_empty_until_03_03() {
        let pinned = Pinned::new();
        let profiler = pinned.profiler(1 << 20);
        assert_eq!(profiler.get_json_output_file_path(), "");
    }

    #[test]
    fn the_queued_task_is_a_no_op_without_a_queue() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.run_queued_task();
        assert_eq!(profiler.get_frame_number(), 0);
    }

    #[test]
    fn the_real_clock_constructor_runs_a_frame() {
        let mut profiler = ProfilerFrame::new(1 << 20, 8);
        profiler.enable(true);
        profiler.tick();
        profiler.run_queued_task();
        assert!(profiler.is_active);
        assert_eq!(profiler.get_frame_number(), 0);
    }

    /// A two-root load: `alpha { xx }` and `beta` side by side. This is the
    /// shape that exercises every sibling arm of the flatten walk, the
    /// hierarchical accumulation and the stats traversal.
    fn load_two_roots(pinned: &Pinned, profiler: &mut ProfilerFrame) {
        profiler.add_entry("alpha", EventType::Start, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("xx", EventType::Start, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("xx", EventType::End, 0, Category::CPP);
        profiler.add_entry("alpha", EventType::End, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("beta", EventType::Start, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("beta", EventType::End, 0, Category::CPP);
        pinned.advance(50);
    }

    #[test]
    fn two_roots_traverse_every_sibling_arm() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);
        load_two_roots(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        // Sorted view: alpha (200 stamps) leads; xx and beta tie at 100 and
        // the stable sort keeps the flatten order (child before the parent's
        // sibling reached by the up-walk).
        let names: Vec<&str> = profiler.top_entries[0]
            .iter()
            .map(|e| e.entry_name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "xx", "beta"]);

        // Hierarchical view: MainThread gains two children.
        profiler.set_profile_mode(ProfileMode::Hierarchical, true);
        load_two_roots(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        let thread = profiler.hier_nodes[profiler.parent_hier_entry]
            .child
            .unwrap();
        let alpha = profiler.hier_nodes[thread].child.unwrap();
        let beta = profiler.hier_nodes[alpha].sibling.unwrap();
        assert_eq!(profiler.hier_nodes[beta].name, "beta");
        // Appending a third child walks the existing sibling chain.
        let gamma = profiler.hier_find_child(thread, "gamma");
        assert_eq!(
            profiler.hier_nodes[beta].sibling,
            Some(gamma),
            "gamma appended after beta"
        );
        assert_eq!(profiler.hier_nodes[gamma].name, "gamma");

        // The stats traversal, top-down: toggling the root makes MainThread
        // visible; toggling the thread makes alpha and beta visible. xx is
        // still hidden, so next(alpha) falls to the direct-sibling arm and
        // reaches beta.
        profiler.toggle_children(profiler.parent_hier_entry);
        profiler.toggle_children(thread);
        let lines = profiler.get_statistics();
        let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(joined.contains("alpha"), "{joined}");
        assert!(joined.contains("beta"), "{joined}");
        assert!(!joined.contains("xx"), "{joined}");

        // Toggling alpha exposes xx: next(xx) has no child or sibling, so
        // the traversal walks the ancestor chain up to alpha's sibling beta.
        let alpha = profiler.hier_nodes[thread].child.unwrap();
        profiler.toggle_children(alpha);
        let lines = profiler.get_statistics();
        let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(joined.contains("xx"), "{joined}");
        // beta still appears, reached through the ancestor walk.
        assert!(joined.contains("beta"), "{joined}");
    }

    #[test]
    fn hierarchical_exclusive_times_flow_through_the_stats() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);
        // inclusive=false stores calcExclusiveTimes_ = true.
        profiler.set_profile_mode(ProfileMode::Hierarchical, false);
        assert!(profiler.calc_exclusive_times);
        load_two_roots(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        let lines = profiler.get_statistics();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.contains(&"(Exclusive)\n"));
    }

    #[test]
    fn too_many_display_entries_set_the_error_string() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);
        profiler.max_display_entry_count = 1;

        profiler.add_entry("first", EventType::Start, 0, Category::CPP);
        profiler.add_entry("second", EventType::Start, 0, Category::CPP);
        profiler.add_entry("first", EventType::End, 0, Category::CPP);
        profiler.tick();
        profiler.run_queued_task();

        assert!(
            profiler
                .profile_error_string
                .starts_with("ERROR: Too many display entries")
        );
    }

    #[test]
    fn stack_depth_overflow_skips_entries_in_both_directions() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);

        // Sixty-five nested unclosed sections: the first 64 open display
        // entries, the 65th Start overflows the cap (depth grows past it and
        // the entry is dropped), and the matching End decrements back
        // without closing anything.
        for i in 0..(MAX_STACK_DEPTH + 1) {
            profiler.add_entry(&format!("deep{i}"), EventType::Start, 0, Category::CPP);
        }
        for i in (0..(MAX_STACK_DEPTH + 1)).rev() {
            profiler.add_entry(&format!("deep{i}"), EventType::End, 0, Category::CPP);
        }

        profiler.tick();
        profiler.run_queued_task();
        assert_eq!(profiler.total_stack_depth[0], 0);
        // Only the 64 in-cap sections became display entries.
        assert_eq!(profiler.display_entry_count, MAX_STACK_DEPTH as u32);
    }

    #[test]
    fn nested_open_sections_are_clamped_with_idle_separated() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);

        // outer { idle-inner never closed; the frame ends under both.
        profiler.add_entry("outer", EventType::Start, 0, Category::CPP);
        pinned.advance(100);
        profiler.add_entry("idle-inner", EventType::StartIdle, 0, Category::CPP);
        pinned.advance(100);
        profiler.tick();
        profiler.run_queued_task();

        let head = profiler.threads[0].head.unwrap();
        let outer = &profiler.display_entries[head];
        assert_eq!(outer.name, "outer");
        // Both clamped to the end-of-frame stamp (200 stamps): the idle
        // child contributes its full span as idle, the parent accumulates
        // child cycles plus idle.
        assert_eq!(outer.num_inclusive_idle_cycles, 100, "outer idle");
        // The reference's idle clamp only fills numInclusiveIdleCycles_
        // (numInclusiveCycles_ stays 0), so the parent's numChildCycles_
        // gains nothing from an idle child.
        assert_eq!(outer.num_child_cycles, 0, "outer child");
        assert_eq!(outer.num_inclusive_cycles, 100, "outer incl");
        let inner = &profiler.display_entries[outer.child.unwrap()];
        assert_eq!(inner.num_inclusive_idle_cycles, 100, "inner idle");
    }

    #[test]
    fn dead_and_headless_threads_are_skipped_by_the_reports() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);
        load_two_roots(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        // A visible slot whose thread id is 0 (a dead slot flipped visible)
        // and a live visible slot with no entries both fall out of the
        // report passes.
        profiler.threads[1].visible = true;
        profiler.threads[2].thread = 2;
        profiler.threads[2].name = "Idle".to_string();
        profiler.threads[2].visible = true;

        // The sorted pass skips the dead slot too.
        profiler.output_report();
        assert!(profiler.top_entries[1].is_empty());

        profiler.set_profile_mode(ProfileMode::Hierarchical, true);
        load_two_roots(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        // Both passes completed without touching the dead slots.
        assert!(profiler.top_entries[1].is_empty());
        let thread = profiler.hier_nodes[profiler.parent_hier_entry]
            .child
            .unwrap();
        assert_eq!(profiler.hier_nodes[thread].name, "MainThread");
    }

    #[test]
    fn the_off_mode_short_circuits_the_sorted_report() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);
        load_two_roots(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();
        assert!(!profiler.top_entries[0].is_empty());

        // Off keeps the last head but the sorted pass stops before the
        // flatten, leaving the previous top entries untouched.
        profiler.set_profile_mode(ProfileMode::Off, true);
        let before = profiler.top_entries[0].clone();
        profiler.output_report();
        assert_eq!(profiler.top_entries[0], before);
        assert!(!before.is_empty());
    }

    #[test]
    fn every_sort_mode_produces_its_stats_label() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);
        load_two_roots(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();

        profiler.set_profile_mode(ProfileMode::SortByNumCalls, true);
        let labels: Vec<String> = profiler
            .get_statistics()
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert!(labels.contains(&"Sorted By Number of Calls".to_string()));

        profiler.set_profile_mode(ProfileMode::CpuGpu, true);
        let labels: Vec<String> = profiler
            .get_statistics()
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert!(labels.contains(&"CPU and GPU".to_string()));

        profiler.set_profile_mode(ProfileMode::SortByName, true);
        let labels: Vec<String> = profiler
            .get_statistics()
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert!(labels.contains(&"Sorted By Name".to_string()));

        // Graphs is not one of the reference's sorted labels.
        profiler.set_profile_mode(ProfileMode::Graphs, true);
        load_two_roots(&pinned, &mut profiler);
        profiler.tick();
        profiler.run_queued_task();
        let labels: Vec<String> = profiler
            .get_statistics()
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert!(labels.contains(&"Invalid Sort!".to_string()));
        // The Graphs mode still flattens (the reference's match has no arm
        // for it), keeping the flatten order.
        let names: Vec<&str> = profiler.top_entries[0]
            .iter()
            .map(|e| e.entry_name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "xx", "beta"]);
    }

    #[test]
    fn watcher_sort_mode_accepts_numcalls_too() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.watcher_set_sort_mode("SORT_BY_NUMCALLS");
        assert_eq!(profiler.watcher_get_sort_mode(), "SORT_BY_NUMCALLS");
    }

    #[test]
    fn the_empty_slot_table_guards_fire() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        // The reference guards every slot-table entry against an empty
        // table; the port keeps them, so poke the table empty to reach them.
        profiler.threads.clear();
        profiler.add_this_thread("Nobody");
        profiler.remove_thread(1);
        profiler.do_remove_threads();
        assert!(profiler.threads.is_empty());
    }

    #[test]
    fn thread_visibility_setters_round_trip() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        profiler.set_thread_visibility(1, true);
        assert!(profiler.get_thread_visibility(1));
        profiler.set_thread_visibility(1, false);
        assert!(!profiler.get_thread_visibility(1));
    }

    #[test]
    fn a_single_top_entry_pads_the_stats_block() {
        let pinned = Pinned::new();
        let mut profiler = pinned.profiler(1 << 20);
        activate(&mut profiler);
        profiler.add_entry("solo", EventType::Start, 0, Category::CPP);
        profiler.add_entry("depth", EventType::Counter, 1, Category::CPP);
        pinned.advance(10);
        profiler.add_entry("solo", EventType::End, 0, Category::CPP);
        profiler.tick();
        profiler.run_queued_task();
        assert_eq!(profiler.num_top_entries[0], 1);

        let texts: Vec<String> = profiler
            .get_statistics()
            .into_iter()
            .map(|l| l.text)
            .collect();
        // One entry, then the blank pad line for the missing second row.
        let solo_index = texts.iter().position(|t| t.contains("solo")).unwrap();
        assert_eq!(texts[solo_index + 1], "\n");
    }
}
