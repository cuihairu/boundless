//! The profiler frame and counter compatibility layer
//! (`lib/cstdmf/profiler.hpp` / `profiler.cpp` / `profiler.ipp`, BW-12-03-01).
//!
//! This is the surface the reference's `g_profiler` singleton exposes to the
//! `_bw_profile` Python module (`server/tools/bw_profile/bw_profile.cpp`) and
//! to the JSON dump writer: the double-buffered [`ProfileEntry`] pool with
//! per-frame `frameNumber_` validation, the [`Profiler::tick`] buffer swap
//! that closes a frame, the per-thread [`ThreadEntry`] registry, the
//! [`DisplayEntry`] hierarchy the frame processor builds with inclusive /
//! child / idle cycle accounting, and [`ProfilerLevelCounter`] — the Chrome
//! Trace style level counter that emits `EVENT_COUNTER` entries.
//!
//! # Port shape
//!
//! - [`Profiler`] replaces the `g_profiler` singleton: one instance, shared
//!   `&self` methods over an internal mutex. The reference records lock-free
//!   from many threads and processes the frame on a background task; here a
//!   single lock makes the recording/swap race unrepresentable and
//!   [`Profiler::tick`] processes inline — the reference's `forceJsonDump_`
//!   path (`profiler.cpp:1169-1176`). The task-manager dispatch, its
//!   semaphore stall warning and the per-core profiler are not carried.
//! - The recording thread index is the reference's `tls_ThreadIndex_`
//!   (`profiler.cpp:32`, default `-1`): a `thread_local` set by
//!   [`Profiler::add_this_thread`], never cleared on removal, so entries
//!   written by a removed thread still attribute to its last slot until the
//!   slot is reused.
//! - Event names use the reference's fixed 96-byte buffer
//!   (`ENTRY_NAME_SIZE`, `profiler.hpp:182`): names are truncated to 95
//!   bytes and the remainder is zero-filled, exactly `strncpy`'s semantics,
//!   because the pool owns the string and callers may pass pointers into
//!   memory that dies before the entry is consumed.
//! - The frame processor is a line-faithful port of `Profiler::processEntries`
//!   (`profiler.cpp:1480-1789`), including its quirks: the frame identifier
//!   comes from the first entry only when there is more than one (so a
//!   one-entry frame is skipped entirely), mismatched `EVENT_END`s are
//!   ignored, unclosed sections clamp to the frame's end stamp, idle cycles
//!   propagate to parents and are excluded from non-idle inclusive cycles,
//!   and the per-thread history is written at `frame % HISTORY_LENGTH`
//!   *after* `frame_` is incremented (first processed frame lands at index
//!   1).
//! - [`Profiler::init`] derives the entry/display budgets from the memory
//!   budget with the reference's formula (`profiler.cpp:969`) over the pinned
//!   64-bit C layout sizes (128 / 160 bytes) so field values match the
//!   original under the same budget.
//! - `outputReport`, the hierarchical (`HierEntry`) graph face, hitch
//!   detection accumulation and the JSON dump file writer are out of scope
//!   here: statistics belongs to the analysis delivery (BW-23) and the dump
//!   writer to BW-12-03-03. The dump *state machine* fields this layer's
//!   callers read (`enable`, `watcherSetDumpProfile`, `setForceJsonDump`,
//!   `getJsonDumpState`) are ported.
//!
//! The clock is injectable so the acceptance workload (same entries, same
//! stamps in, same key fields out) is deterministic.

use std::cell::Cell;
use std::collections::{BTreeSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// The reference's `Profiler::MAXIMUM_NUMBER_OF_THREADS` (`profiler.hpp:148`).
pub const MAXIMUM_NUMBER_OF_THREADS: usize = 96;
/// The reference's `Profiler::MAX_STACK_DEPTH` (`profiler.hpp:149`).
pub const MAX_STACK_DEPTH: usize = 64;
/// The reference's `Profiler::MAX_NUM_TOP_ENTRIES` (`profiler.hpp:150`).
pub const MAX_NUM_TOP_ENTRIES: usize = 10;
/// The reference's `Profiler::HISTORY_LENGTH` (`profiler.hpp:151`).
pub const HISTORY_LENGTH: usize = 512;
/// The reference's `Profiler::ENTRY_NAME_SIZE` (`profiler.hpp:182`): the
/// fixed storage for an entry's name.
pub const ENTRY_NAME_SIZE: usize = 96;

// The init budget formula (`profiler.cpp:969`) divides the memory budget by
// `sizeof(ProfileEntry) * 2 + sizeof(DisplayEntry) / 2`. The pinned values
// are the reference's 64-bit C layout: ProfileEntry is 4+4+96+8+4+4+4 = 124
// padded to 128, DisplayEntry is 24+96+8+24+1 = 153 padded to 160.
const PROFILE_ENTRY_SIZEOF: i32 = 128;
const DISPLAY_ENTRY_SIZEOF: i32 = 160;

/// `Profiler::CATEGORY_CPP` (`profiler.hpp:164`).
pub const CATEGORY_CPP: i32 = 1 << 0;
/// `Profiler::CATEGORY_GPU` (`profiler.hpp:165`).
pub const CATEGORY_GPU: i32 = 1 << 1;
/// `Profiler::CATEGORY_PYTHON` (`profiler.hpp:166`).
pub const CATEGORY_PYTHON: i32 = 1 << 2;
/// `Profiler::CATEGORY_ALL` (`profiler.hpp:168`).
pub const CATEGORY_ALL: i32 = (1 << 3) - 1;

/// The reference's `Profiler::EventType` (`profiler.hpp:154-160`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum EventType {
    /// `EVENT_START`: a scoped section opens.
    Start = 0,
    /// `EVENT_START_IDLE`: an idle scoped section opens.
    StartIdle = 1,
    /// `EVENT_END`: a scoped section closes.
    End = 2,
    /// `EVENT_COUNTER`: a named counter value.
    Counter = 3,
}

/// The reference's `Profiler::JsonDumpState` (`profiler.hpp:171-177`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JsonDumpState {
    /// `JSON_DUMPING_INACTIVE`.
    Inactive = 0,
    /// `JSON_DUMPING_ACTIVE`.
    Active = 1,
    /// `JSON_DUMPING_COMPLETED`.
    Completed = 2,
    /// `JSON_DUMPING_FAILED`.
    Failed = 3,
}

/// The reference's `Profiler::ProfileMode` (`profiler.hpp:130-143`), server
/// build: `ENABLE_PER_CORE_PROFILER` is off, so there is no `CORES` mode and
/// `MAX_MODES` is 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ProfileMode {
    /// `PROFILE_OFF`.
    ProfileOff = 0,
    /// `HIERARCHICAL`.
    Hierarchical = 1,
    /// `SORT_BY_TIME`.
    SortByTime = 2,
    /// `SORT_BY_NUMCALLS`.
    SortByNumCalls = 3,
    /// `SORT_BY_NAME`.
    SortByName = 4,
    /// `CPU_GPU`.
    CpuGpu = 5,
    /// `GRAPHS`.
    Graphs = 6,
    /// `MAX_MODES`.
    MaxModes = 7,
}

/// The reference's `Profiler::ProfileEntry` (`profiler.hpp:184-196`): one raw
/// event in the double-buffered pool. `coreNumber_` is not carried (the
/// per-core profiler is off in server builds).
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    /// `event_`.
    pub event: EventType,
    /// `category_`: a `CATEGORY_*` bit mask, filtered against the category
    /// captured at the frame's tick.
    pub category: i32,
    /// `name_`: fixed 96-byte storage, truncated to 95 bytes and
    /// zero-filled like `strncpy` (`profiler.ipp:40-41`).
    pub name: [u8; ENTRY_NAME_SIZE],
    /// `time_`: the raw stamp from the clock (`timestamp()`).
    pub time: u64,
    /// `threadIdx_`: the recording thread's registry slot, `-1` for a thread
    /// that never registered.
    pub thread_idx: i32,
    /// `value_`: the counter value for `EVENT_COUNTER` entries.
    pub value: i32,
    /// `frameNumber_`: the recording frame number, the torn-entry guard the
    /// frame processor validates against (`profiler.cpp:1560-1564`).
    pub frame_number: i32,
}

impl Default for ProfileEntry {
    fn default() -> Self {
        Self {
            event: EventType::Start,
            category: 0,
            name: [0; ENTRY_NAME_SIZE],
            time: 0,
            thread_idx: -1,
            value: 0,
            frame_number: 0,
        }
    }
}

impl ProfileEntry {
    /// The entry name as a string, cut at the first NUL like `strcmp`.
    pub fn name_str(&self) -> &str {
        name_str(&self.name)
    }
}

/// The reference's `Profiler::DisplayEntry` (`profiler.hpp:199-210`): one
/// node of the per-thread frame hierarchy the processor builds. The
/// reference links nodes with raw pointers into `displayEntryBuffer_`; the
/// port links them by pool index, the observable structure is unchanged.
#[derive(Debug, Clone)]
pub struct DisplayEntry {
    /// `parent_`, `sibling_`, `child_`: pool indices.
    pub parent: Option<u32>,
    pub sibling: Option<u32>,
    pub child: Option<u32>,
    /// `name_`.
    pub name: [u8; ENTRY_NAME_SIZE],
    /// `start_`: the opening entry's stamp.
    pub start: u64,
    /// `numInclusiveCycles_`: closing stamp minus opening stamp, with idle
    /// children subtracted for non-idle sections (`profiler.cpp:1678-1690`).
    pub num_inclusive_cycles: u64,
    /// `numChildCycles_`: the inclusive cycles propagated by non-idle
    /// children.
    pub num_child_cycles: u64,
    /// `numInclusiveIdleCycles_`: full span for idle sections, accumulated
    /// idle children otherwise.
    pub num_inclusive_idle_cycles: u64,
    /// `isIdle_`.
    pub is_idle: bool,
}

impl DisplayEntry {
    /// The entry name as a string, cut at the first NUL like `strcmp`.
    pub fn name_str(&self) -> &str {
        name_str(&self.name)
    }
}

fn name_str(name: &[u8; ENTRY_NAME_SIZE]) -> &str {
    let end = name.iter().position(|&b| b == 0).unwrap_or(ENTRY_NAME_SIZE);
    std::str::from_utf8(&name[..end]).unwrap_or("")
}

/// One slot of the reference's `Profiler::ThreadEntry` (`profiler.hpp:225-235`)
/// minus the client-only display faces (`vertexData_`, `hierHead_`).
#[derive(Debug, Clone)]
struct ThreadEntry {
    /// `thread_`: the OS thread id, 0 for an empty slot.
    thread: u64,
    /// `name_`: full string, no truncation. Empty slots keep the reference's
    /// "Dead Thread" default (`profiler.cpp:996`); removed slots are renamed
    /// "Deleted" (`profiler.cpp:2496`).
    name: String,
    /// `head_`: the first display entry of the last processed frame.
    head: Option<u32>,
    /// `visible_`.
    visible: bool,
}

impl Default for ThreadEntry {
    fn default() -> Self {
        Self {
            thread: 0,
            name: "Dead Thread".to_string(),
            head: None,
            visible: false,
        }
    }
}

/// The reference's `Profiler::EntryBuffer` (`profiler.hpp:263-268`).
#[derive(Debug, Clone)]
struct EntryBuffer {
    /// `entries_`, sized `maxEntryCount_`.
    entries: Vec<ProfileEntry>,
    /// `entryCount_`.
    entry_count: i32,
    /// `endOfFrameTimeStamp_`, stamped after the swap in
    /// [`Profiler::tick`] (`profiler.cpp:1140`).
    end_of_frame_time_stamp: u64,
}

/// The mutable state behind [`Profiler`], guarded by one mutex. Field names
/// follow the reference's member list (`profiler.hpp:420-547`) minus the
/// faces this layer does not carry.
struct Inner {
    inited: bool,
    memory_used: i32,
    max_num_threads: usize,
    max_entry_count: i32,
    max_display_entry_count: u32,
    /// `profileEntryBuffers_[2]` plus the two pointer members, as indices.
    buffers: [EntryBuffer; 2],
    profile_entry_buffer: usize,
    final_profile_entry_buffer: usize,
    profile_frame_number: i32,
    /// `isActive_`; the reference marks it volatile (set in one thread, read
    /// in another) — the mutex plays that role here.
    is_active: bool,
    is_pending_active: bool,
    has_generated_profile: bool,
    frame: i32,
    frame_start_time: u64,
    threads: Vec<ThreadEntry>,
    num_threads: i32,
    /// `threadsToRemove_`: a `BW::set<uint>`, kept ordered.
    threads_to_remove: BTreeSet<u64>,
    /// `displayEntryBuffer_`.
    display_pool: Vec<DisplayEntry>,
    display_entry_count: u32,
    high_watermark_of_entry_count: i32,
    high_watermark_of_display_entry_count: u32,
    /// `perThreadHistory_`.
    per_thread_history: Vec<Vec<f32>>,
    /// `dStack_`, `dChildTails_`, `dHeads_`, `totalStackDepth_`: the
    /// processor's per-thread scratch, pre-allocated in `init`
    /// (`profiler.cpp:1012-1018`).
    d_stack: Vec<Vec<Option<u32>>>,
    d_child_tails: Vec<Vec<Option<u32>>>,
    d_heads: Vec<Option<u32>>,
    total_stack_depth: Vec<i32>,
    profile_error_string: String,
    time_taken_for_profiling: f32,
    profile_mode: ProfileMode,
    calc_exclusive_times: bool,
    freeze_profiling: bool,
    hitch_freeze: bool,
    allow_hitch_freezing: bool,
    profile_bar_visibility: bool,
    do_json_dump: bool,
    /// `forceJsonDump_`: processing synchronously rather than dispatching
    /// the background task.
    force_json_dump: bool,
    /// `setForceJsonDump`'s function-static `oldDumpCount`
    /// (`profiler.cpp:1353`): process-global in the reference, per-instance
    /// here where one instance plays `g_profiler`.
    old_dump_count: i32,
    json_dump_index: i32,
    json_dump_count: i32,
    json_output_file_path: String,
    json_dump_dir: String,
    json_dump_state: JsonDumpState,
    /// `jsonDumpStartTime_` (`profiler.hpp:456`): the first entry's stamp of
    /// the frame that opened the file; every dump `ts` is the offset from it.
    json_dump_start_time: u64,
    /// `jsonFileHasData_` (`profiler.hpp:457`): gates the comma separator
    /// and persists across frames sharing one file.
    json_file_has_data: bool,
    /// `jsonOutputFile_` (`profiler.hpp:458`): the reference's raw `FILE*`.
    json_output_file: Option<std::fs::File>,
    /// `eventCategories_`: a `BW::map` keyed by the category mask, so
    /// iteration is ascending by key; the server build registers "C++" and
    /// "Python" plus the "All" entry (`profiler.cpp:951-959`).
    event_categories: Vec<(i32, &'static str)>,
    /// `currentEventCategory_`, as an index into `event_categories`.
    current_event_category: usize,
    /// `finalEventCategory_`, captured from `current_event_category` at the
    /// tick that closes the frame (`profiler.cpp:1141`).
    final_event_category: usize,
    /// The `stampsToSeconds` denominator (`timestamp.hpp:176-179`).
    stamps_per_second: f64,
}

/// The profiler compatibility layer: one instance plays the reference's
/// `g_profiler`.
pub struct Profiler {
    inner: Mutex<Inner>,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
}

fn default_clock() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// The reference's `OurThreadID()` (`concurrency.hpp:190`): the OS thread
/// id. The port hashes the std thread id — stable within the process, which
/// is the only guarantee the reference's platform id carries — and keeps it
/// nonzero (`profiler.cpp:2255` asserts it).
pub fn this_thread_id() -> u64 {
    thread_local! {
        static ID: u64 = {
            let mut hasher = DefaultHasher::new();
            std::thread::current().id().hash(&mut hasher);
            hasher.finish() | 1
        };
    }
    ID.with(|id| *id)
}

/// The reference's `g_profiler` definition (`profiler.cpp:27`): the one
/// profiler instance the whole process shares. Every consumer — the
/// `_bw_profile` module (`server/tools/bw_profile/bw_profile.cpp`), the
/// watchers, the JSON dump writer — records into and reads from this
/// object; the reference exports the instance itself and this port hands
/// out the same `&'static` handle over the port's mutex shape (see the
/// port shape note at the crate top).
static G_PROFILER: LazyLock<Profiler> = LazyLock::new(Profiler::default);

/// The process-global profiler: the reference's `g_profiler`
/// (`profiler.cpp:27`).
pub fn global() -> &'static Profiler {
    &G_PROFILER
}

// The reference's `tls_ThreadIndex_` (`profiler.cpp:32`): the recording
// thread's registry slot, `-1` until `addThisThread` runs and never cleared
// on removal.
thread_local! {
    static THREAD_INDEX: Cell<i32> = const { Cell::new(-1) };
}

fn get_thread_index() -> i32 {
    THREAD_INDEX.with(|cell| cell.get())
}

fn set_thread_index(index: i32) {
    THREAD_INDEX.with(|cell| cell.set(index));
}

fn stamps_to_ms(stamps: u64, stamps_per_second: f64) -> f32 {
    (stamps as f64 / stamps_per_second * 1000.0) as f32
}

impl Default for Profiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Profiler {
    /// The reference's `Profiler()` constructor init list
    /// (`profiler.cpp:718-772`): usable only after [`Profiler::init`].
    pub fn new() -> Self {
        Self::with_clock(default_clock, 1_000_000_000.0)
    }

    /// A profiler over a caller-supplied stamp source and the
    /// `stampsToSeconds` denominator, for deterministic workloads.
    pub fn with_clock(
        clock: impl Fn() -> u64 + Send + Sync + 'static,
        stamps_per_second: f64,
    ) -> Self {
        let inner = Inner {
            inited: false,
            memory_used: 0,
            max_num_threads: MAXIMUM_NUMBER_OF_THREADS,
            max_entry_count: 0,
            max_display_entry_count: 0,
            buffers: [
                EntryBuffer {
                    entries: Vec::new(),
                    entry_count: 0,
                    end_of_frame_time_stamp: 0,
                },
                EntryBuffer {
                    entries: Vec::new(),
                    entry_count: 0,
                    end_of_frame_time_stamp: 0,
                },
            ],
            profile_entry_buffer: 0,
            final_profile_entry_buffer: 0,
            profile_frame_number: 0,
            is_active: false,
            is_pending_active: false,
            has_generated_profile: false,
            frame: 0,
            frame_start_time: 0,
            threads: Vec::new(),
            num_threads: 0,
            threads_to_remove: BTreeSet::new(),
            display_pool: Vec::new(),
            display_entry_count: 0,
            high_watermark_of_entry_count: 0,
            high_watermark_of_display_entry_count: 0,
            per_thread_history: Vec::new(),
            d_stack: Vec::new(),
            d_child_tails: Vec::new(),
            d_heads: Vec::new(),
            total_stack_depth: Vec::new(),
            profile_error_string: String::new(),
            time_taken_for_profiling: 0.0,
            profile_mode: ProfileMode::SortByTime,
            calc_exclusive_times: false,
            freeze_profiling: false,
            hitch_freeze: false,
            allow_hitch_freezing: false,
            profile_bar_visibility: false,
            do_json_dump: false,
            force_json_dump: false,
            old_dump_count: 1,
            json_dump_index: 0,
            json_dump_count: 1,
            json_output_file_path: String::new(),
            json_dump_dir: String::new(),
            json_dump_state: JsonDumpState::Inactive,
            json_dump_start_time: 0,
            json_file_has_data: false,
            json_output_file: None,
            event_categories: Vec::new(),
            current_event_category: 0,
            final_event_category: 0,
            stamps_per_second,
        };
        Self {
            inner: Mutex::new(inner),
            clock: Box::new(clock),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("profiler mutex poisoned")
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    /// The reference's `Profiler::init` (`profiler.cpp:881-1073`): derives
    /// the entry/display budgets from the memory budget, allocates the
    /// double buffer, the thread table and the processor scratch, registers
    /// the category names and adds the calling thread as "MainThread".
    ///
    /// The watcher registrations, `HierarchyManager` and background task
    /// belong to other faces and are not carried.
    pub fn init(&self, memory_size: i32, max_num_threads: i32) -> bool {
        {
            let mut s = self.lock();
            s.memory_used = memory_size;
            let num = memory_size / (PROFILE_ENTRY_SIZEOF * 2 + DISPLAY_ENTRY_SIZEOF / 2);
            s.max_entry_count = num.max(0);
            s.max_display_entry_count = (num / 2).max(0) as u32;
            let num = num.max(0) as usize;
            s.buffers = [
                Self::empty_buffer(&mut s, num),
                Self::empty_buffer(&mut s, num),
            ];
            s.profile_entry_buffer = 0;
            s.final_profile_entry_buffer = 1;
            s.max_num_threads = max_num_threads.max(0) as usize;
            s.threads = vec![ThreadEntry::default(); s.max_num_threads];
            s.num_threads = 0;
            s.display_pool = vec![Self::empty_display(); s.max_display_entry_count as usize];
            s.display_entry_count = 0;
            s.per_thread_history = vec![vec![0.0; HISTORY_LENGTH]; s.max_num_threads];
            s.d_stack = vec![vec![None; MAX_STACK_DEPTH]; s.max_num_threads];
            s.d_child_tails = vec![vec![None; MAX_STACK_DEPTH]; s.max_num_threads];
            s.d_heads = vec![None; s.max_num_threads];
            s.total_stack_depth = vec![0; s.max_num_threads];
            s.hitch_freeze = false;
            s.allow_hitch_freezing = false;
            s.freeze_profiling = false;
            s.calc_exclusive_times = false;
            s.profile_mode = ProfileMode::SortByTime;
            s.is_active = false;
            s.is_pending_active = false;
            // Server build: no GPU category (`profiler.cpp:954-956`). The map
            // is keyed by mask, so "All" sorts last.
            s.event_categories = vec![
                (CATEGORY_CPP, "C++"),
                (CATEGORY_PYTHON, "Python"),
                (CATEGORY_ALL, "All"),
            ];
            s.current_event_category = s.event_categories.len() - 1;
            s.final_event_category = s.current_event_category;
            s.profile_bar_visibility = true;
            // By default, dumping to the executable directory
            // (`profiler.cpp:1069-1070`, `BWUtil::executableDirectory`).
            s.json_dump_dir = std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|dir| dir.to_string_lossy().into_owned()))
                .unwrap_or_default();
        }
        // Add the main thread to the profile system (`profiler.cpp:1049`).
        self.add_this_thread("MainThread");
        let mut s = self.lock();
        s.inited = true;
        s.frame = 0;
        true
    }

    fn empty_buffer(_s: &mut Inner, num: usize) -> EntryBuffer {
        EntryBuffer {
            entries: vec![ProfileEntry::default(); num],
            entry_count: 0,
            end_of_frame_time_stamp: 0,
        }
    }

    fn empty_display() -> DisplayEntry {
        DisplayEntry {
            parent: None,
            sibling: None,
            child: None,
            name: [0; ENTRY_NAME_SIZE],
            start: 0,
            num_inclusive_cycles: 0,
            num_child_cycles: 0,
            num_inclusive_idle_cycles: 0,
            is_idle: false,
        }
    }

    /// The reference's `Profiler::fini` (`profiler.cpp:1079-1100`): disables
    /// and releases the buffers, thread table and scratch.
    pub fn fini(&self) {
        let mut s = self.lock();
        if !s.inited {
            return;
        }
        enable_locked(&mut s, false);
        s.buffers = [
            EntryBuffer {
                entries: Vec::new(),
                entry_count: 0,
                end_of_frame_time_stamp: 0,
            },
            EntryBuffer {
                entries: Vec::new(),
                entry_count: 0,
                end_of_frame_time_stamp: 0,
            },
        ];
        s.threads.clear();
        s.display_pool.clear();
        s.per_thread_history.clear();
        s.d_stack.clear();
        s.d_child_tails.clear();
        s.d_heads.clear();
        s.total_stack_depth.clear();
        s.inited = false;
    }

    /// The reference's `Profiler::addEntry` (`profiler.ipp:25-52`): records
    /// one event into the recording buffer, no-op while the profiler is
    /// inactive. The name is truncated to 95 bytes into the fixed pool slot;
    /// the stamp, thread index and frame number are captured here.
    pub fn add_entry(&self, text: &str, event: EventType, value: i32, category: i32) {
        let thread_idx = get_thread_index();
        let now = self.now();
        let mut s = self.lock();
        if !s.is_active {
            return;
        }
        let Some(index) = start_next_entry(&mut s) else {
            return;
        };
        let buffer_index = s.profile_entry_buffer;
        let frame_number = s.profile_frame_number;
        let entry = &mut s.buffers[buffer_index].entries[index as usize];
        entry.name = [0; ENTRY_NAME_SIZE];
        let bytes = text.as_bytes();
        let kept = bytes.len().min(ENTRY_NAME_SIZE - 1);
        entry.name[..kept].copy_from_slice(&bytes[..kept]);
        entry.event = event;
        entry.category = category;
        entry.time = now;
        entry.value = value;
        entry.thread_idx = thread_idx;
        entry.frame_number = frame_number;
    }

    /// Test fixture seam: places an entry with arbitrary `frame_number` /
    /// `thread_idx` into the recording buffer, standing in for the torn
    /// entries a racing thread can produce in the reference (the guards at
    /// `profiler.cpp:1552-1564` are unreachable from the single-lock port).
    #[doc(hidden)]
    pub fn debug_inject_raw_entry(&self, entry: ProfileEntry) {
        let mut s = self.lock();
        if !s.inited {
            return;
        }
        if let Some(index) = start_next_entry(&mut s) {
            let buffer_index = s.profile_entry_buffer;
            s.buffers[buffer_index].entries[index as usize] = entry;
        }
    }

    /// The reference's `Profiler::tick` (`profiler.cpp:1108-1189`): swaps
    /// the entry buffers, stamps the frame end and processes the frame
    /// inline (the reference's `forceJsonDump_` path). The first tick after
    /// [`Profiler::enable`] only activates recording; the background task
    /// dispatch, semaphore stall path and hitch accumulation are other
    /// faces.
    pub fn tick(&self) {
        let clock = self.clock.as_ref();
        let mut s = self.lock();
        // The reference would run on null buffers here; the port no-ops
        // before init instead (documented guard over reference UB).
        if !s.inited {
            return;
        }
        if !s.is_pending_active && !s.is_active {
            return;
        }
        if s.is_active {
            let recording_index = s.profile_entry_buffer;
            if s.buffers[recording_index].entry_count != 0 {
                let final_index = s.final_profile_entry_buffer;
                s.buffers[final_index].entry_count = 0;
                // std::mem::swap cannot take two fields through one
                // MutexGuard deref; swap via locals instead.
                let recording_index = s.profile_entry_buffer;
                s.final_profile_entry_buffer = recording_index;
                s.profile_entry_buffer = final_index;
                s.profile_frame_number += 1;
                let now = clock();
                let new_final = s.final_profile_entry_buffer;
                s.buffers[new_final].end_of_frame_time_stamp = now;
                s.final_event_category = s.current_event_category;
                let entry_count = s.buffers[new_final].entry_count;
                s.high_watermark_of_entry_count = s.high_watermark_of_entry_count.max(entry_count);
                let profile_start = clock();
                process_entries(&mut s, clock, profile_start);
            }
        }
        s.is_pending_active = false;
        s.is_active = true;
        s.frame_start_time = clock();
    }

    /// The reference's `Profiler::addThisThread` (`profiler.cpp:2242-2294`):
    /// claims the first empty registry slot for the calling thread and pins
    /// its TLS index. Re-adding an already-registered thread only re-pins
    /// the index and un-marks a pending removal; the scan stops at the
    /// first empty slot, exactly like the reference's `break`.
    pub fn add_this_thread(&self, name: &str) {
        let mut s = self.lock();
        if s.threads.is_empty() {
            return;
        }
        let this_thread = this_thread_id();
        let mut first_empty: Option<usize> = None;
        for i in 0..s.max_num_threads {
            if s.threads[i].thread == this_thread {
                set_thread_index(i as i32);
                // Erase the thread from being removed just in case it was
                // marked for removal.
                s.threads_to_remove.remove(&this_thread);
                return;
            }
            if s.threads[i].thread == 0 && first_empty.is_none() {
                first_empty = Some(i);
                break;
            }
        }
        let Some(slot) = first_empty else {
            // Too many threads.
            return;
        };
        s.threads[slot].thread = this_thread;
        s.threads[slot].name = name.to_string();
        s.threads[slot].head = None;
        s.threads[slot].visible = true;
        set_thread_index(slot as i32);
        s.num_threads += 1;
    }

    /// The reference's `Profiler::removeThread` (`profiler.cpp:2448-2471`):
    /// while the profiler is active (or about to be) the removal is deferred
    /// to the end of the frame's processing; otherwise it happens now.
    pub fn remove_thread(&self, thread_id: u64) {
        let mut s = self.lock();
        if s.threads.is_empty() {
            return;
        }
        if !s.is_active && !s.is_pending_active {
            internal_remove_thread(&mut s, thread_id);
            return;
        }
        s.threads_to_remove.insert(thread_id);
    }

    /// The reference's `Profiler::getThreadName` (`profiler.ipp:55-64`):
    /// `None` for an empty slot. The reference's range assert is against the
    /// constant 96 even when the table is smaller (an out-of-range id reads
    /// past the allocation); the port answers `None` instead (documented
    /// safety guard).
    pub fn get_thread_name(&self, thread_id: i32) -> Option<String> {
        let s = self.lock();
        if thread_id < 0 || thread_id as usize >= s.threads.len() {
            return None;
        }
        if s.threads[thread_id as usize].thread != 0 {
            Some(s.threads[thread_id as usize].name.clone())
        } else {
            None
        }
    }

    /// `ThreadEntry::visible_` for a registered slot.
    pub fn get_thread_visibility(&self, thread_id: i32) -> Option<bool> {
        let s = self.lock();
        s.threads
            .get(thread_id as usize)
            .filter(|slot| slot.thread != 0)
            .map(|slot| slot.visible)
    }

    /// The reference's `Profiler::enable` (`profiler.cpp:2538-2561`):
    /// enabling arms the next tick (recording starts after it); disabling
    /// stops recording now and drops whatever is buffered.
    pub fn enable(&self, state: bool) {
        let mut s = self.lock();
        if !state && !s.inited {
            // The reference dereferences profileEntryBuffer_ here; the port
            // no-ops before init instead (documented guard).
            return;
        }
        enable_locked(&mut s, state);
    }

    /// `Profiler::watcherEnabled` (`profiler.hpp:386-389`).
    pub fn watcher_enabled(&self) -> bool {
        let s = self.lock();
        s.is_active || s.is_pending_active
    }

    /// `Profiler::getFrameNumber` (`profiler.hpp:406`): processed frames.
    pub fn get_frame_number(&self) -> i32 {
        self.lock().frame
    }

    /// The recording frame number: the value stamped into entries until the
    /// next tick (`profileFrameNumber_`).
    pub fn profile_frame_number(&self) -> i32 {
        self.lock().profile_frame_number
    }

    /// `Profiler::freeze` (`profiler.hpp:306-313`): while frozen, frame
    /// processing short-circuits before building the hierarchy.
    pub fn freeze(&self, freeze: bool, dump: bool) {
        let mut s = self.lock();
        freeze_locked(&mut s, freeze, dump);
    }

    /// `Profiler::allowHitchFreezing` getters/setter (`profiler.hpp:315-323`).
    pub fn allow_hitch_freezing(&self) -> bool {
        self.lock().allow_hitch_freezing
    }

    pub fn set_allow_hitch_freezing(&self, allow: bool) {
        self.lock().allow_hitch_freezing = allow;
    }

    /// `Profiler::hitchFreeze`/setter (`profiler.hpp:325-334`): freezing on
    /// a hitch also arms a dump exactly when the hitch is new.
    pub fn hitch_freeze(&self) -> bool {
        self.lock().hitch_freeze
    }

    pub fn set_hitch_freeze(&self, hitch: bool) {
        let mut s = self.lock();
        let was = s.hitch_freeze;
        freeze_locked(&mut s, hitch, hitch != was && hitch);
        s.hitch_freeze = hitch;
    }

    /// `Profiler::getProfileBarVisibility`/setter (`profiler.hpp:336-340`).
    pub fn get_profile_bar_visibility(&self) -> bool {
        self.lock().profile_bar_visibility
    }

    pub fn set_profile_bar_visibility(&self, state: bool) {
        self.lock().profile_bar_visibility = state;
    }

    /// `Profiler::setForceJsonDump` (`profiler.cpp:1351-1364`): parks the
    /// dump count at 15 while forced, restores it otherwise.
    pub fn set_force_json_dump(&self, dump: bool) {
        let mut s = self.lock();
        s.force_json_dump = dump;
        if dump {
            s.old_dump_count = s.json_dump_count;
            s.json_dump_count = 15;
        } else {
            s.json_dump_count = s.old_dump_count;
        }
    }

    /// `Profiler::setJsonDumpCount` (`profiler.hpp:367-370`).
    pub fn set_json_dump_count(&self, count: i32) {
        self.lock().json_dump_count = count;
    }

    /// `Profiler::watcherSetDumpProfile` (`profiler.hpp:375-379`).
    pub fn watcher_set_dump_profile(&self, new_value: bool) {
        let mut s = self.lock();
        s.json_dump_index = 0;
        s.do_json_dump = new_value;
    }

    /// `Profiler::watcherGetDumpProfile` (`profiler.hpp:381-384`).
    pub fn watcher_get_dump_profile(&self) -> bool {
        self.lock().do_json_dump
    }

    /// `Profiler::setJsonDumpDir` (`profiler.cpp:1282-1287`).
    pub fn set_json_dump_dir(&self, json_dump_dir: &str) {
        self.lock().json_dump_dir = json_dump_dir.to_string();
    }

    /// The `jsonDumpDir_` field, for the dump writer face (BW-12-03-03).
    pub fn json_dump_dir(&self) -> String {
        self.lock().json_dump_dir.clone()
    }

    /// `Profiler::getJsonDumpState` (`profiler.cpp:1273-1276`).
    pub fn get_json_dump_state(&self) -> JsonDumpState {
        self.lock().json_dump_state
    }

    /// `Profiler::getJsonOutputFilePath` (`profiler.hpp:418`): empty until
    /// the dump writer face (BW-12-03-03) runs.
    pub fn get_json_output_file_path(&self) -> String {
        self.lock().json_output_file_path.clone()
    }

    /// `Profiler::getErrorString` (`profiler.hpp:410`).
    pub fn get_error_string(&self) -> String {
        self.lock().profile_error_string.clone()
    }

    /// `Profiler::timeTakenForProfiling_`: the last frame's processing
    /// duration in milliseconds.
    pub fn time_taken_for_profiling(&self) -> f32 {
        self.lock().time_taken_for_profiling
    }

    /// The reference's `Profiler::prevCategory` (`profiler.cpp:2300-2313`):
    /// walks the mask-ordered map with wrap-around. The `resetHierarchy()`
    /// call belongs to the hierarchical display face and is not carried.
    pub fn prev_category(&self) {
        let mut s = self.lock();
        if s.event_categories.is_empty() {
            return;
        }
        if s.current_event_category == 0 {
            s.current_event_category = s.event_categories.len();
        }
        s.current_event_category -= 1;
    }

    /// The reference's `Profiler::nextCategory` (`profiler.cpp:2319-2332`).
    pub fn next_category(&self) {
        let mut s = self.lock();
        if s.event_categories.is_empty() {
            return;
        }
        s.current_event_category += 1;
        if s.current_event_category == s.event_categories.len() {
            s.current_event_category = 0;
        }
    }

    /// The reference's `Profiler::setCurrentCategory` (`profiler.cpp:2338-2360`):
    /// an unknown name is ignored (warning dropped).
    pub fn set_current_category(&self, new_category: &str) {
        let mut s = self.lock();
        if let Some(index) = s
            .event_categories
            .iter()
            .position(|(_, name)| *name == new_category)
        {
            s.current_event_category = index;
        }
    }

    /// `Profiler::getCurrentCategory` (`profiler.cpp:2366-2370`).
    pub fn get_current_category(&self) -> String {
        let s = self.lock();
        s.event_categories
            .get(s.current_event_category)
            .map(|(_, name)| (*name).to_string())
            .unwrap_or_default()
    }

    /// `Profiler::getCategories` (`profiler.cpp:2376-2391`): all names,
    /// ';' separated.
    pub fn get_categories(&self) -> String {
        let s = self.lock();
        s.event_categories
            .iter()
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(";")
    }

    // -- Read-only views for the acceptance workload ----------------------

    /// `entryCount_` of the recording buffer.
    pub fn recording_entry_count(&self) -> i32 {
        let s = self.lock();
        s.buffers[s.profile_entry_buffer].entry_count
    }

    /// `entryCount_` of the buffer the last tick closed.
    pub fn final_entry_count(&self) -> i32 {
        let s = self.lock();
        s.buffers[s.final_profile_entry_buffer].entry_count
    }

    /// One entry of the closed frame buffer, by index.
    pub fn final_entry(&self, index: i32) -> Option<ProfileEntry> {
        let s = self.lock();
        let buf = &s.buffers[s.final_profile_entry_buffer];
        if index >= 0 && index < buf.entry_count {
            Some(buf.entries[index as usize].clone())
        } else {
            None
        }
    }

    /// `endOfFrameTimeStamp_` of the closed frame buffer.
    pub fn final_end_of_frame_time_stamp(&self) -> u64 {
        let s = self.lock();
        s.buffers[s.final_profile_entry_buffer].end_of_frame_time_stamp
    }

    /// `displayEntryCount_`: display entries built by the last processed
    /// frame.
    pub fn display_entry_count(&self) -> u32 {
        self.lock().display_entry_count
    }

    /// A clone of the display pool (the reference's `displayEntryBuffer_`).
    pub fn display_pool(&self) -> Vec<DisplayEntry> {
        self.lock().display_pool.clone()
    }

    /// `ThreadEntry::head_` for a slot: the first display entry of the last
    /// processed frame.
    pub fn thread_head(&self, thread_id: usize) -> Option<u32> {
        self.lock()
            .threads
            .get(thread_id)
            .and_then(|slot| slot.head)
    }

    /// `numThreads_`.
    pub fn num_threads(&self) -> i32 {
        self.lock().num_threads
    }

    /// `highWatermarkOfEntryCount_`.
    pub fn high_watermark_of_entry_count(&self) -> i32 {
        self.lock().high_watermark_of_entry_count
    }

    /// `highWatermarkOfDisplayEntryCount_`.
    pub fn high_watermark_of_display_entry_count(&self) -> u32 {
        self.lock().high_watermark_of_display_entry_count
    }

    /// One `perThreadHistory_[thread][index]` sample (milliseconds).
    pub fn per_thread_history_value(&self, thread: usize, index: usize) -> Option<f32> {
        let s = self.lock();
        s.per_thread_history
            .get(thread)
            .and_then(|history| history.get(index))
            .copied()
    }

    /// `maxEntryCount_` derived by [`Profiler::init`].
    pub fn max_entry_count(&self) -> i32 {
        self.lock().max_entry_count
    }

    /// `maxDisplayEntryCount_` derived by [`Profiler::init`].
    pub fn max_display_entry_count(&self) -> u32 {
        self.lock().max_display_entry_count
    }

    /// The `jsonDumpCount_` field, for the dump writer face (BW-12-03-03)
    /// and its fixture workload.
    #[doc(hidden)]
    pub fn json_dump_count_snapshot(&self) -> i32 {
        self.lock().json_dump_count
    }
}

fn enable_locked(s: &mut Inner, state: bool) {
    if state {
        // Turn it on at the next tick().
        if !s.is_active {
            s.is_pending_active = true;
            s.has_generated_profile = true;
            s.json_dump_state = JsonDumpState::Inactive;
        }
    } else {
        // Turn it off now and ignore all current data.
        s.is_active = false;
        s.buffers[s.profile_entry_buffer].entry_count = 0;
        s.json_dump_state = JsonDumpState::Inactive;
    }
}

fn freeze_locked(s: &mut Inner, freeze: bool, dump: bool) {
    if freeze && !s.freeze_profiling {
        s.do_json_dump = dump;
    }
    s.freeze_profiling = freeze;
}

/// The reference's `Profiler::startNextEntry` (`profiler.ipp:67-83`): claims
/// a pool slot, clamping `entryCount_` at `maxEntryCount_` once full.
fn start_next_entry(s: &mut Inner) -> Option<i32> {
    let buf = &mut s.buffers[s.profile_entry_buffer];
    if buf.entry_count >= s.max_entry_count {
        buf.entry_count = s.max_entry_count;
        return None;
    }
    buf.entry_count += 1;
    let index = buf.entry_count - 1;
    if index >= s.max_entry_count {
        buf.entry_count = s.max_entry_count;
        return None;
    }
    Some(index)
}

fn internal_remove_thread(s: &mut Inner, thread_id: u64) {
    for slot in &mut s.threads {
        if slot.thread == thread_id {
            s.num_threads -= 1;
            slot.head = None;
            slot.name = "Deleted".to_string();
            slot.thread = 0;
            slot.visible = false;
            return;
        }
    }
}

fn do_remove_threads_locked(s: &mut Inner) {
    let ids: Vec<u64> = s.threads_to_remove.iter().copied().collect();
    for id in ids {
        internal_remove_thread(s, id);
    }
    s.threads_to_remove.clear();
}

/// The reference's `Profiler::processEntries` (`profiler.cpp:1480-1789`):
/// builds the per-thread display hierarchy from the closed frame buffer and
/// advances the frame counter. See the crate docs for the ported quirks.
/// The dump file name schema (`profiler.cpp:1308-1313`); the comment there
/// warns the WebConsole depends on it, so the format is pinned verbatim.
fn json_dump_file_path(dir: &str, executable_basename: &str, t: &libc::tm) -> String {
    let (year, month, day) = (1900 + t.tm_year, t.tm_mon + 1, t.tm_mday);
    let (hour, minute, second) = (t.tm_hour, t.tm_min, t.tm_sec);
    format!(
        "{dir}/{executable_basename}_profile_{year:04}{month:02}{day:02}_{hour:02}{minute:02}{second:02}.json"
    )
}

/// `Profiler::jsonDump` (`profiler.cpp:1198-1243`). The reference's
/// `PROFILER_SCOPED` self-instrumentation (jsonDump/jsonDump_open/
/// jsonDump_close) is not carried — the port has no scoped-macro face, the
/// same divergence as the rest of this layer. `fprintf` return values are
/// ignored here exactly like the reference ignores them.
fn json_dump(s: &mut Inner) {
    s.json_dump_index += 1;

    if s.json_dump_index == 1 {
        // jsonDump_open: a still-open file from a previous run is closed
        // with its final `]` before the new one is created
        // (`profiler.cpp:1208-1213`).
        if let Some(mut file) = s.json_output_file.take() {
            let _ = std::io::Write::write_all(&mut file, b"]");
        }

        // The first entry of the frame being dumped anchors every `ts`
        // (`profiler.cpp:1215`); an empty frame reads slot 0's stale stamp,
        // faithfully.
        s.json_dump_start_time = s.buffers[s.final_profile_entry_buffer].entries[0].time;

        match create_new_json_dump_file(s) {
            Some(file) => s.json_output_file = Some(file),
            None => {
                s.json_dump_index = 0;
                s.do_json_dump = false;
                s.json_dump_state = JsonDumpState::Failed;
                return;
            }
        }

        s.json_dump_state = JsonDumpState::Active;
        s.json_file_has_data = false;
        if let Some(file) = s.json_output_file.as_mut() {
            let _ = std::io::Write::write_all(file, b"[");
        }
    }

    write_json_entries(s);

    if let Some(file) = s.json_output_file.as_mut() {
        let _ = std::io::Write::flush(file);
    }

    if s.json_dump_index == s.json_dump_count {
        stop_json_dump(s);
    }
}

/// `Profiler::stopJsonDump` (`profiler.cpp:1249-1258`). The reference
/// writes through the raw pointer without a null check — both call sites
/// guard it, so the `take()` arm here is the documented port.
fn stop_json_dump(s: &mut Inner) {
    if let Some(mut file) = s.json_output_file.take() {
        let _ = std::io::Write::write_all(&mut file, b"]");
    }
    s.json_dump_index = 0;
    s.do_json_dump = false;
    s.json_dump_state = JsonDumpState::Completed;
}

/// `Profiler::createNewJsonDumpFile` (`profiler.cpp:1296-1338`): wall-clock
/// `time(NULL)` + `localtime` name the file (independent of the profiler's
/// stamp clock). The INFO/ERROR message faces are debug-log faces this
/// layer does not carry.
fn create_new_json_dump_file(s: &mut Inner) -> Option<std::fs::File> {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    let basename = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();

    let path = json_dump_file_path(&s.json_dump_dir, &basename, &tm);

    match std::fs::File::create(&path) {
        Ok(file) => {
            s.json_output_file_path = path;
            Some(file)
        }
        Err(_) => None,
    }
}

/// `Profiler::writeJsonEntries` (`profiler.cpp:1370-1473`): one frame's
/// thread-name meta events plus the raw entries, in Chrome Trace Event
/// format. The whole frame is rendered into a string and written once; the
/// per-thread entry stacks are balance tracking the reference runs while
/// emitting (start pushes, matching end pops) with no output effect.
fn write_json_entries(s: &mut Inner) {
    let mut out = String::new();

    // Thread-name meta events (`profiler.cpp:1382-1401`): one per
    // registered slot, every frame.
    for t in 0..s.max_num_threads {
        if s.threads[t].thread != 0 {
            if s.json_file_has_data {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"pid\":0,\"tid\":{t}, \"ph\": \"M\", \"name\":\"thread_name\",\"args\":{{\"name\": \"{}\"}} }} ",
                s.threads[t].name
            ));
            s.json_file_has_data = true;
        }
    }

    let final_buffer = &s.buffers[s.final_profile_entry_buffer];
    let entry_count = final_buffer.entry_count;
    let start_time = s.json_dump_start_time;
    let stamps_per_second = s.stamps_per_second;
    let mut entry_stacks: Vec<Vec<String>> = vec![Vec::new(); s.max_num_threads];

    for entry in final_buffer.entries.iter().take(entry_count as usize) {
        // Copy the face fields out so the entry borrow ends before the
        // flag mutation below.
        let (event, category, time, thread_idx, value) = (
            entry.event,
            entry.category,
            entry.time,
            entry.thread_idx,
            entry.value,
        );
        let name = entry.name_str().to_owned();
        if thread_idx == -1 {
            continue;
        }

        if s.json_file_has_data {
            out.push(',');
        }

        // `eventCategories_[entry->category_]` on the reference's BW::map
        // default-constructs an empty string for an unregistered mask
        // (`profiler.cpp:1421-1422`).
        let category = s
            .event_categories
            .iter()
            .find(|(mask, _)| *mask == category)
            .map(|(_, name)| *name)
            .unwrap_or("");
        let ts = (time.wrapping_sub(start_time) as f64 / stamps_per_second * 1000000.0) as i32;

        let event_string = match event {
            EventType::Start | EventType::StartIdle => {
                entry_stacks[thread_idx as usize].push(name.clone());
                "B"
            }
            EventType::End => {
                let stack = &mut entry_stacks[thread_idx as usize];
                if stack.last().map(|top| top.as_str()) == Some(name.as_str()) {
                    stack.pop();
                }
                "E"
            }
            EventType::Counter => "C",
        };

        out.push_str(&format!(
            "{{ \"cat\":\"{category}\", \"pid\":0,\"tid\":{thread_idx}, \"ts\":{ts},\"ph\":\"{event_string}\",\"name\":\"{name}\""
        ));
        if event == EventType::Counter {
            out.push_str(&format!(",\"args\":{{\"counter\": {value}}}}}"));
        } else {
            out.push('}');
        }

        s.json_file_has_data = true;
    }

    if let Some(file) = s.json_output_file.as_mut() {
        let _ = std::io::Write::write_all(file, out.as_bytes());
    }
}

fn process_entries(s: &mut Inner, clock: &dyn Fn() -> u64, profile_start: u64) {
    // The jsonDump hookup runs before the freeze bail-out, so a frozen
    // profiler still dumps (`profiler.cpp:1485-1500`).
    if s.do_json_dump || s.force_json_dump {
        json_dump(s);
    } else if s.json_output_file.is_some() {
        stop_json_dump(s);
    }
    if s.freeze_profiling {
        s.time_taken_for_profiling =
            stamps_to_ms(clock().wrapping_sub(profile_start), s.stamps_per_second);
        return;
    }

    let num_threads = s.max_num_threads;
    for i in 0..num_threads {
        s.d_stack[i][0] = None;
        s.d_child_tails[i][0] = None;
        s.d_heads[i] = None;
        s.total_stack_depth[i] = 0;
    }

    s.display_entry_count = 0;

    let final_buf = &s.buffers[s.final_profile_entry_buffer];
    let entry_count = final_buf.entry_count;
    let end_of_frame_time_stamp = final_buf.end_of_frame_time_stamp;
    let final_category = s.event_categories[s.final_event_category].0;

    // The currentFrameIdentifier should be the same for all entries in the
    // frame buffer; a differing entry is incomplete and skipped. Only read
    // from the first entry when there is more than one, so a one-entry
    // frame is skipped entirely (`profiler.cpp:1533-1538`).
    let current_frame_identifier: i32 = if entry_count > 1 {
        final_buf.entries[0].frame_number
    } else {
        -1
    };

    for entry in final_buf.entries.iter().take(entry_count.max(0) as usize) {
        // First grab and check the thread ID of the entry.
        let thread_id = entry.thread_idx;
        if thread_id == -1 {
            continue;
        }
        // If this entry has an invalid frameNumber_ then it is discarded.
        if entry.frame_number != current_frame_identifier {
            continue;
        }
        if (entry.category & final_category) == 0 {
            continue;
        }
        let tid = thread_id as usize;

        match entry.event {
            EventType::Start | EventType::StartIdle => {
                if s.display_entry_count >= s.max_display_entry_count {
                    s.profile_error_string = "ERROR: Too many display entries\n\
                         increase your buffer size in <debug/profileBufferSize> \
                         in engine_config.\n"
                        .to_string();
                    continue;
                }
                let slot = s.display_entry_count;
                {
                    let d = &mut s.display_pool[slot as usize];
                    d.name = [0; ENTRY_NAME_SIZE];
                    let bytes = &entry.name;
                    let kept = bytes
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(ENTRY_NAME_SIZE)
                        .min(ENTRY_NAME_SIZE - 1);
                    d.name[..kept].copy_from_slice(&bytes[..kept]);
                    d.start = entry.time;
                    d.child = None;
                    d.sibling = None;
                    d.num_inclusive_cycles = 0;
                    d.num_child_cycles = 0;
                    d.num_inclusive_idle_cycles = 0;
                    d.is_idle = entry.event == EventType::StartIdle;
                }

                let stack_depth = s.total_stack_depth[tid];
                if stack_depth as usize >= MAX_STACK_DEPTH {
                    s.total_stack_depth[tid] += 1;
                    continue;
                }

                s.display_entry_count += 1;

                // Add the display entry to the stack.
                s.d_stack[tid][stack_depth as usize] = Some(slot);
                s.total_stack_depth[tid] += 1;

                if stack_depth == 0 {
                    s.display_pool[slot as usize].parent = None;
                    if s.d_heads[tid].is_none() {
                        s.d_heads[tid] = Some(slot);
                    } else if let Some(tail) = s.d_child_tails[tid][stack_depth as usize] {
                        s.display_pool[tail as usize].sibling = Some(slot);
                    }
                } else {
                    let parent = s.d_stack[tid][stack_depth as usize - 1]
                        .expect("parent of a nested section is on the stack");
                    s.display_pool[slot as usize].parent = Some(parent);
                    if s.display_pool[parent as usize].child.is_none() {
                        s.display_pool[parent as usize].child = Some(slot);
                    } else if let Some(tail) = s.d_child_tails[tid][stack_depth as usize] {
                        // If there is already a sibling, link that to this.
                        s.display_pool[tail as usize].sibling = Some(slot);
                    }
                }

                // Set this display entry up to be potentially used as a
                // sibling.
                s.d_child_tails[tid][stack_depth as usize] = Some(slot);
            }
            EventType::End => {
                let stack_depth = s.total_stack_depth[tid];
                if stack_depth as usize >= MAX_STACK_DEPTH {
                    s.total_stack_depth[tid] -= 1;
                    continue;
                }
                if stack_depth == 0 {
                    // This happens quite often due to tasks running on
                    // other threads during the profile tick.
                    continue;
                }
                // The slot is set whenever the depth is positive (every push
                // wrote it); the reference's defensive null check cannot fire
                // (`profiler.cpp:1665-1667`).
                let open_slot =
                    s.d_stack[tid][stack_depth as usize - 1].expect("open section slot is set");
                // A mismatched name is ignored (the reference's vacuous
                // assert and `continue`, profiler.cpp:1667-1672).
                if s.display_pool[open_slot as usize].name != entry.name {
                    continue;
                }

                let (open_parent, open_idle, open_inclusive, open_is_idle) = {
                    let open = &mut s.display_pool[open_slot as usize];
                    open.num_inclusive_cycles = entry.time.wrapping_sub(open.start);

                    if open.is_idle {
                        open.num_inclusive_idle_cycles = open.num_inclusive_cycles;
                    } else {
                        open.num_inclusive_cycles -= open.num_inclusive_idle_cycles;
                    }

                    s.total_stack_depth[tid] -= 1;

                    (
                        open.parent,
                        open.num_inclusive_idle_cycles,
                        open.num_inclusive_cycles,
                        open.is_idle,
                    )
                };

                if let Some(parent) = open_parent {
                    let parent = &mut s.display_pool[parent as usize];
                    parent.num_inclusive_idle_cycles += open_idle;
                    if !open_is_idle {
                        parent.num_child_cycles += open_inclusive;
                    }
                }
            }
            // EVENT_COUNTER entries carry no hierarchy.
            EventType::Counter => {}
        }
    }

    s.high_watermark_of_display_entry_count = s
        .high_watermark_of_display_entry_count
        .max(s.display_entry_count);

    // Close any entries that are still open (due to mismatched open-close
    // input entries), clamped to the end of the frame.
    for thread_id in 0..num_threads {
        while s.total_stack_depth[thread_id] > 0 {
            let stack_depth = s.total_stack_depth[thread_id];
            s.total_stack_depth[thread_id] -= 1;

            let top_slot = if (stack_depth as usize) < MAX_STACK_DEPTH {
                s.d_stack[thread_id][stack_depth as usize - 1]
            } else {
                None
            };
            if let Some(top_slot) = top_slot {
                let (top_inclusive, top_idle, top_parent) = {
                    let top = &mut s.display_pool[top_slot as usize];
                    if top.is_idle {
                        top.num_inclusive_idle_cycles =
                            end_of_frame_time_stamp.wrapping_sub(top.start);
                    } else {
                        top.num_inclusive_cycles = end_of_frame_time_stamp
                            .wrapping_sub(top.start)
                            .wrapping_sub(top.num_inclusive_idle_cycles);
                    }
                    (
                        top.num_inclusive_cycles,
                        top.num_inclusive_idle_cycles,
                        top.parent,
                    )
                };
                if let Some(parent) = top_parent {
                    let parent = &mut s.display_pool[parent as usize];
                    parent.num_child_cycles += top_inclusive;
                    parent.num_inclusive_idle_cycles += top_idle;
                }
            }
        }

        s.threads[thread_id].head = s.d_heads[thread_id];
    }

    // outputReport(): the sorted top-entries/statistics face, delivered with
    // the statistics item; the per-thread frame history below is the field
    // this layer carries.
    s.frame += 1;

    // Add current frame's times to the appropriate history array. The index
    // is `frame_ % HISTORY_LENGTH` *after* the increment, so the first
    // processed frame lands at index 1 (`profiler.cpp:1759-1782`).
    for i in 0..num_threads {
        if let Some(head_slot) = s.d_heads[i] {
            let start = s.display_pool[head_slot as usize].start;
            let mut node = head_slot;
            while let Some(sibling) = s.display_pool[node as usize].sibling {
                node = sibling;
            }
            let last = &s.display_pool[node as usize];
            let span = last
                .num_inclusive_cycles
                .wrapping_add(last.start)
                .wrapping_sub(start);
            s.per_thread_history[i][(s.frame as usize) % HISTORY_LENGTH] =
                stamps_to_ms(span, s.stamps_per_second);
        } else {
            // No children. Set timer to zero.
            s.per_thread_history[i][(s.frame as usize) % HISTORY_LENGTH] = 0.0;
        }
    }

    do_remove_threads_locked(s);

    s.time_taken_for_profiling =
        stamps_to_ms(clock().wrapping_sub(profile_start), s.stamps_per_second);
}

/// The reference's `ProfilerLevelCounter` (`profiler.hpp:728-743`,
/// `profiler.cpp:3493-3520`): tracks the level of something (like a buffer)
/// and emits an `EVENT_COUNTER` entry on every change. The level moves even
/// while the profiler is inactive — only the entries are dropped. A zero
/// amount emits nothing; a negative amount asserts in debug builds (the
/// reference's `MF_ASSERT`) and mirrors its release behaviour otherwise.
pub struct ProfilerLevelCounter<'a> {
    profiler: &'a Profiler,
    desc: String,
    level: AtomicI32,
}

impl<'a> ProfilerLevelCounter<'a> {
    pub fn new(profiler: &'a Profiler, desc: &str) -> Self {
        Self {
            profiler,
            desc: desc.to_string(),
            level: AtomicI32::new(0),
        }
    }

    /// `ProfilerLevelCounter::increase` (`profiler.cpp:3493-3502`).
    pub fn increase(&self, amt: i32) {
        debug_assert!(amt >= 0);
        if amt == 0 {
            return;
        }
        let prev = self.level.fetch_add(amt, Ordering::Relaxed);
        self.profiler
            .add_entry(&self.desc, EventType::Counter, prev + amt, CATEGORY_CPP);
    }

    /// `ProfilerLevelCounter::decrease` (`profiler.cpp:3510-3520`): the
    /// reference only asserts non-negative levels in debug builds, so the
    /// port also lets the level go below zero.
    pub fn decrease(&self, amt: i32) {
        debug_assert!(amt >= 0);
        if amt == 0 {
            return;
        }
        let prev = self.level.fetch_sub(amt, Ordering::Relaxed);
        self.profiler
            .add_entry(&self.desc, EventType::Counter, prev - amt, CATEGORY_CPP);
    }

    /// The current level (`level_`).
    pub fn level(&self) -> i32 {
        self.level.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A profiler with a microsecond stamp source (`stampsPerSecond` 1e6, so
    /// 1000 stamps == 1 ms) and a 96-slot thread table, over a shared
    /// controllable stamp.
    fn clocked() -> (Profiler, Arc<AtomicU64>) {
        let stamp = Arc::new(AtomicU64::new(0));
        let shared = Arc::clone(&stamp);
        let profiler = Profiler::with_clock(move || shared.load(Ordering::SeqCst), 1_000_000.0);
        (profiler, stamp)
    }

    /// The init budget formula (`profiler.cpp:969`).
    fn entry(
        name: &str,
        event: EventType,
        time: u64,
        thread_idx: i32,
        frame_number: i32,
    ) -> ProfileEntry {
        let mut e = ProfileEntry::default();
        let bytes = name.as_bytes();
        let kept = bytes.len().min(ENTRY_NAME_SIZE - 1);
        e.name[..kept].copy_from_slice(&bytes[..kept]);
        e.event = event;
        e.category = CATEGORY_CPP;
        e.time = time;
        e.thread_idx = thread_idx;
        e.frame_number = frame_number;
        e
    }

    #[test]
    fn init_derives_buffer_budget_from_memory_size() {
        let (profiler, _clock) = clocked();
        // 128MB budget like `_bw_profile`'s init (`bw_profile.cpp:333-334`).
        assert!(profiler.init(128 * 1024 * 1024, 96));
        assert_eq!(profiler.max_entry_count(), 399_457);
        assert_eq!(profiler.max_display_entry_count(), 199_728);
        assert_eq!(profiler.get_frame_number(), 0);
        assert_eq!(profiler.profile_frame_number(), 0);
        assert!(!profiler.watcher_enabled());
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Inactive);
        assert_eq!(profiler.get_categories(), "C++;Python;All");
        assert_eq!(profiler.get_current_category(), "All");
        assert_eq!(profiler.num_threads(), 1);
        assert_eq!(profiler.get_thread_name(0), Some("MainThread".to_string()));
        assert_eq!(profiler.get_thread_name(1), None);
        assert_eq!(profiler.get_thread_visibility(0), Some(true));
        assert_eq!(profiler.get_json_output_file_path(), "");
        assert_eq!(profiler.get_error_string(), "");
        assert!(profiler.get_profile_bar_visibility());
    }

    #[test]
    fn json_dump_dir_defaults_to_executable_directory() {
        let (profiler, _clock) = clocked();
        profiler.init(4096 * 336, 4);
        let expected = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|dir| dir.to_string_lossy().into_owned()))
            .unwrap_or_default();
        assert_eq!(profiler.json_dump_dir(), expected);
        profiler.set_json_dump_dir("/tmp/dumps");
        assert_eq!(profiler.json_dump_dir(), "/tmp/dumps");
    }

    #[test]
    fn add_entry_truncates_name_to_entry_name_size() {
        let (profiler, clock) = clocked();
        profiler.init(16 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        let long = "x".repeat(120);
        clock.store(1000, Ordering::SeqCst);
        profiler.add_entry(&long, EventType::Start, 0, CATEGORY_CPP);
        clock.store(2000, Ordering::SeqCst);
        profiler.add_entry("ok", EventType::End, 0, CATEGORY_CPP);
        profiler.tick();
        let first = profiler.final_entry(0).expect("entry kept");
        assert_eq!(first.name_str(), "x".repeat(95));
        assert_eq!(first.name[95], 0);
        assert_eq!(first.time, 1000);
        assert_eq!(first.thread_idx, 0);
        assert_eq!(second_entry_frame(&profiler, 1), 0);
    }

    fn second_entry_frame(profiler: &Profiler, index: i32) -> i32 {
        profiler
            .final_entry(index)
            .expect("entry kept")
            .frame_number
    }

    #[test]
    fn entries_are_dropped_while_inactive_and_pending() {
        let (profiler, clock) = clocked();
        profiler.init(16 * 336, 4);
        profiler.add_entry("early", EventType::Start, 0, CATEGORY_CPP);
        assert_eq!(profiler.recording_entry_count(), 0);
        profiler.enable(true);
        assert!(profiler.watcher_enabled());
        // The activation tick records nothing and processes nothing.
        profiler.tick();
        assert_eq!(profiler.final_entry_count(), 0);
        assert_eq!(profiler.profile_frame_number(), 0);
        assert_eq!(profiler.get_frame_number(), 0);
        // First recorded frame carries frame number 0; after its tick the
        // frame has been processed.
        clock.store(100, Ordering::SeqCst);
        profiler.add_entry("a", EventType::Start, 0, CATEGORY_CPP);
        assert_eq!(profiler.recording_entry_count(), 1);
        profiler.tick();
        assert_eq!(profiler.final_entry_count(), 1);
        assert_eq!(second_entry_frame(&profiler, 0), 0);
        assert_eq!(profiler.profile_frame_number(), 1);
        assert_eq!(profiler.get_frame_number(), 1);
    }

    #[test]
    fn frame_number_fields_follow_the_tick_swaps() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        for frame in 0..3i32 {
            clock.store(1000, Ordering::SeqCst);
            profiler.add_entry("a", EventType::Start, 0, CATEGORY_CPP);
            clock.store(2000, Ordering::SeqCst);
            profiler.add_entry("a", EventType::End, 0, CATEGORY_CPP);
            profiler.tick();
            assert_eq!(profiler.final_entry(0).expect("kept").frame_number, frame);
            assert_eq!(profiler.profile_frame_number(), frame + 1);
            assert_eq!(profiler.get_frame_number(), frame + 1);
        }
    }

    #[test]
    fn hierarchy_cycle_fields_match_the_reference_workload() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        clock.store(1000, Ordering::SeqCst);
        profiler.add_entry("outer", EventType::Start, 0, CATEGORY_CPP);
        clock.store(1500, Ordering::SeqCst);
        profiler.add_entry("inner", EventType::Start, 0, CATEGORY_CPP);
        clock.store(2500, Ordering::SeqCst);
        profiler.add_entry("inner", EventType::End, 0, CATEGORY_CPP);
        clock.store(3000, Ordering::SeqCst);
        profiler.add_entry("outer", EventType::End, 0, CATEGORY_CPP);
        profiler.tick();

        assert_eq!(profiler.display_entry_count(), 2);
        let pool = profiler.display_pool();
        let head = profiler.thread_head(0).expect("root built");
        assert_eq!(head, 0);
        let outer = &pool[0];
        assert_eq!(outer.name_str(), "outer");
        assert_eq!(outer.start, 1000);
        assert_eq!(outer.num_inclusive_cycles, 2000);
        assert_eq!(outer.num_child_cycles, 1000);
        assert_eq!(outer.num_inclusive_idle_cycles, 0);
        assert_eq!(outer.child, Some(1));
        assert_eq!(outer.parent, None);
        assert_eq!(outer.sibling, None);
        let inner = &pool[1];
        assert_eq!(inner.name_str(), "inner");
        assert_eq!(inner.start, 1500);
        assert_eq!(inner.num_inclusive_cycles, 1000);
        assert_eq!(inner.num_child_cycles, 0);
        assert_eq!(inner.parent, Some(0));
        // history[0][frame 1] = (last sibling inclusive + start - first start)
        // == 2000us == 2ms.
        assert_eq!(profiler.per_thread_history_value(0, 1), Some(2.0));
        assert_eq!(profiler.high_watermark_of_entry_count(), 4);
        assert_eq!(profiler.high_watermark_of_display_entry_count(), 2);
    }

    #[test]
    fn second_root_becomes_sibling_and_drives_history() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        clock.store(1000, Ordering::SeqCst);
        profiler.add_entry("r1", EventType::Start, 0, CATEGORY_CPP);
        clock.store(3000, Ordering::SeqCst);
        profiler.add_entry("r1", EventType::End, 0, CATEGORY_CPP);
        clock.store(3200, Ordering::SeqCst);
        profiler.add_entry("r2", EventType::Start, 0, CATEGORY_CPP);
        clock.store(3500, Ordering::SeqCst);
        profiler.add_entry("r2", EventType::End, 0, CATEGORY_CPP);
        profiler.tick();

        let pool = profiler.display_pool();
        let head = profiler.thread_head(0).expect("root built");
        assert_eq!(pool[head as usize].sibling, Some(1));
        assert_eq!(pool[1].parent, None);
        // history = r2.inclusive + r2.start - r1.start = 300 + 3200 - 1000 us
        // == 2.5ms.
        assert_eq!(profiler.per_thread_history_value(0, 1), Some(2.5));
    }

    #[test]
    fn idle_cycles_propagate_and_exclude_inclusive() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        clock.store(50, Ordering::SeqCst);
        profiler.add_entry("root", EventType::Start, 0, CATEGORY_CPP);
        clock.store(100, Ordering::SeqCst);
        profiler.add_entry("wait", EventType::StartIdle, 0, CATEGORY_CPP);
        clock.store(600, Ordering::SeqCst);
        profiler.add_entry("wait", EventType::End, 0, CATEGORY_CPP);
        clock.store(900, Ordering::SeqCst);
        profiler.add_entry("root", EventType::End, 0, CATEGORY_CPP);
        profiler.tick();

        let pool = profiler.display_pool();
        let wait = &pool[1];
        // An idle section keeps the full span in both fields.
        assert_eq!(wait.num_inclusive_cycles, 500);
        assert_eq!(wait.num_inclusive_idle_cycles, 500);
        assert!(wait.is_idle);
        let root = &pool[0];
        // 900-50 == 850, minus the propagated idle 500.
        assert_eq!(root.num_inclusive_cycles, 350);
        assert_eq!(root.num_inclusive_idle_cycles, 500);
        // Idle children do not propagate child cycles.
        assert_eq!(root.num_child_cycles, 0);
    }

    #[test]
    fn unclosed_entries_clamp_to_the_frame_end_stamp() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        clock.store(100, Ordering::SeqCst);
        profiler.add_entry("a", EventType::Start, 0, CATEGORY_CPP);
        clock.store(200, Ordering::SeqCst);
        profiler.add_entry("b", EventType::Start, 0, CATEGORY_CPP);
        clock.store(1000, Ordering::SeqCst);
        profiler.tick();

        let pool = profiler.display_pool();
        // b is clamped to 1000-200 == 800 and propagates child cycles to a.
        assert_eq!(pool[1].num_inclusive_cycles, 800);
        let a = &pool[0];
        // a is clamped to the raw span 1000-100; the clamp does not subtract
        // child cycles.
        assert_eq!(a.num_inclusive_cycles, 900);
        assert_eq!(a.num_child_cycles, 800);
    }

    #[test]
    fn mismatched_end_is_ignored_and_extra_end_drops_off() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        clock.store(100, Ordering::SeqCst);
        profiler.add_entry("a", EventType::Start, 0, CATEGORY_CPP);
        clock.store(200, Ordering::SeqCst);
        profiler.add_entry("b", EventType::Start, 0, CATEGORY_CPP);
        // Does not match the top of the stack ("b"): ignored.
        clock.store(300, Ordering::SeqCst);
        profiler.add_entry("a", EventType::End, 0, CATEGORY_CPP);
        clock.store(400, Ordering::SeqCst);
        profiler.add_entry("b", EventType::End, 0, CATEGORY_CPP);
        clock.store(500, Ordering::SeqCst);
        profiler.add_entry("a", EventType::End, 0, CATEGORY_CPP);
        // Depth is 0 now: ignored too.
        clock.store(600, Ordering::SeqCst);
        profiler.add_entry("a", EventType::End, 0, CATEGORY_CPP);
        clock.store(1000, Ordering::SeqCst);
        profiler.tick();

        assert_eq!(profiler.display_entry_count(), 2);
        let pool = profiler.display_pool();
        // a closed at its own END (500 - 100); b closed at 400 - 200 and
        // propagated its inclusive cycles to a. The trailing ENDs at depth 0
        // and after a is closed are ignored.
        assert_eq!(pool[0].num_inclusive_cycles, 400);
        assert_eq!(pool[1].num_inclusive_cycles, 200);
        assert_eq!(pool[0].num_child_cycles, 200);
    }

    #[test]
    fn single_entry_frame_is_skipped_by_the_identifier_guard() {
        let (profiler, clock) = clocked();
        profiler.init(16 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        clock.store(100, Ordering::SeqCst);
        profiler.add_entry("lonely", EventType::Start, 0, CATEGORY_CPP);
        profiler.tick();
        // One entry: the identifier stays -1, so the entry is discarded and
        // only the frame counter advances.
        assert_eq!(profiler.display_entry_count(), 0);
        assert_eq!(profiler.thread_head(0), None);
        assert_eq!(profiler.get_frame_number(), 1);
        assert_eq!(profiler.per_thread_history_value(0, 1), Some(0.0));
    }

    #[test]
    fn unfinished_entry_frame_guard_skips_foreign_frame_numbers() {
        let (profiler, _clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        profiler.debug_inject_raw_entry(entry("good1", EventType::Start, 100, 0, 0));
        profiler.debug_inject_raw_entry(entry("torn", EventType::Start, 150, 0, 99));
        profiler.debug_inject_raw_entry(entry("good1", EventType::End, 200, 0, 0));
        profiler.tick();
        // The identifier comes from the first entry (0); the torn entry with
        // frame number 99 is skipped.
        assert_eq!(profiler.display_entry_count(), 1);
        let pool = profiler.display_pool();
        assert_eq!(pool[0].name_str(), "good1");
        assert_eq!(pool[0].num_inclusive_cycles, 100);
    }

    #[test]
    fn unregistered_thread_entries_are_skipped_but_recorded() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        // A thread that never registered keeps the -1 TLS index; its entries
        // are recorded but discarded by the processor.
        profiler.debug_inject_raw_entry(entry("ghost", EventType::Start, 100, -1, 0));
        clock.store(200, Ordering::SeqCst);
        profiler.add_entry("main", EventType::Start, 0, CATEGORY_CPP);
        clock.store(300, Ordering::SeqCst);
        profiler.add_entry("main", EventType::End, 0, CATEGORY_CPP);
        profiler.tick();
        assert_eq!(profiler.final_entry_count(), 3);
        assert_eq!(profiler.display_entry_count(), 1);
        assert_eq!(profiler.display_pool()[0].name_str(), "main");
    }

    #[test]
    fn entry_buffer_overflow_clamps_at_max_entry_count() {
        let (profiler, _clock) = clocked();
        // 4*336 budget: maxEntryCount 4, maxDisplayEntryCount 2.
        profiler.init(4 * 336, 4);
        assert_eq!(profiler.max_entry_count(), 4);
        assert_eq!(profiler.max_display_entry_count(), 2);
        profiler.enable(true);
        profiler.tick();
        for i in 0..6 {
            profiler.add_entry(&format!("e{i}"), EventType::Start, 0, CATEGORY_CPP);
        }
        assert_eq!(profiler.recording_entry_count(), 4);
        profiler.tick();
        assert_eq!(profiler.final_entry_count(), 4);
        assert_eq!(profiler.high_watermark_of_entry_count(), 4);
        assert_eq!(profiler.final_entry(3).expect("kept").name_str(), "e3");
        assert!(profiler.final_entry(4).is_none());
    }

    #[test]
    fn display_overflow_sets_the_reference_error_string() {
        let (profiler, _clock) = clocked();
        profiler.init(4 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("a", EventType::Start, 0, CATEGORY_CPP);
        profiler.add_entry("b", EventType::Start, 0, CATEGORY_CPP);
        // Display budget is 2: this start overflows and sets the error.
        profiler.add_entry("c", EventType::Start, 0, CATEGORY_CPP);
        profiler.add_entry("b", EventType::End, 0, CATEGORY_CPP);
        profiler.add_entry("a", EventType::End, 0, CATEGORY_CPP);
        profiler.tick();
        assert_eq!(profiler.display_entry_count(), 2);
        assert_eq!(
            profiler.get_error_string(),
            "ERROR: Too many display entries\n\
             increase your buffer size in <debug/profileBufferSize> \
             in engine_config.\n"
        );
    }

    #[test]
    fn stack_depth_overflow_skips_display_but_keeps_pairs() {
        let (profiler, _clock) = clocked();
        profiler.init(200 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        // 65 nested sections: the 65th start overflows MAX_STACK_DEPTH.
        for _ in 0..65 {
            profiler.add_entry("x", EventType::Start, 0, CATEGORY_CPP);
        }
        for _ in 0..65 {
            profiler.add_entry("x", EventType::End, 0, CATEGORY_CPP);
        }
        profiler.tick();
        // Only the 64 sections that fit were stored; the overflow start and
        // its matching end (the first one seen at depth 65) consumed none.
        assert_eq!(profiler.display_entry_count(), 64);
        let mut chain = 0u32;
        let mut node = profiler.thread_head(0).expect("root built");
        loop {
            chain += 1;
            match profiler.display_pool()[node as usize].child {
                Some(child) => node = child,
                None => break,
            }
        }
        assert_eq!(chain, 64);
    }

    #[test]
    fn level_counter_tracks_level_and_emits_counter_events() {
        let (profiler, _clock) = clocked();
        profiler.init(64 * 336, 4);
        let counter = ProfilerLevelCounter::new(&profiler, "bufferSize");
        // Inactive: the level still moves, only the entries are dropped.
        counter.increase(3);
        assert_eq!(counter.level(), 3);
        assert_eq!(profiler.recording_entry_count(), 0);
        profiler.enable(true);
        profiler.tick();
        counter.increase(2);
        counter.increase(0); // A zero amount emits nothing.
        counter.decrease(4);
        counter.decrease(0);
        counter.decrease(5); // Below zero: release behaviour.
        profiler.tick();
        assert_eq!(counter.level(), -4);
        let e0 = profiler.final_entry(0).expect("counter entry");
        assert_eq!(e0.name_str(), "bufferSize");
        assert_eq!(e0.event, EventType::Counter);
        assert_eq!(e0.value, 5);
        assert_eq!(e0.category, CATEGORY_CPP);
        assert_eq!(e0.frame_number, 0);
        let e1 = profiler.final_entry(1).expect("counter entry");
        assert_eq!(e1.value, 1);
        let e2 = profiler.final_entry(2).expect("counter entry");
        assert_eq!(e2.value, -4);
        assert!(profiler.final_entry(3).is_none());
    }

    #[test]
    fn category_filter_uses_the_tick_capture() {
        let (profiler, _clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        profiler.set_current_category("Python");
        assert_eq!(profiler.get_current_category(), "Python");
        profiler.add_entry("cpp", EventType::Start, 0, CATEGORY_CPP);
        profiler.add_entry("py", EventType::Start, 0, CATEGORY_PYTHON);
        profiler.tick();
        // Only the Python entry passes the category captured at the tick.
        assert_eq!(profiler.display_entry_count(), 1);
        assert_eq!(profiler.display_pool()[0].name_str(), "py");
        // Unknown names are ignored.
        profiler.set_current_category("Bogus");
        assert_eq!(profiler.get_current_category(), "Python");
        // prev/next walk the mask-ordered map with wrap-around.
        profiler.prev_category();
        assert_eq!(profiler.get_current_category(), "C++");
        profiler.prev_category();
        assert_eq!(profiler.get_current_category(), "All");
        profiler.next_category();
        assert_eq!(profiler.get_current_category(), "C++");
    }

    #[test]
    fn enable_disable_semantics() {
        let (profiler, _clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("will-drop", EventType::Start, 0, CATEGORY_CPP);
        profiler.enable(false);
        assert!(!profiler.watcher_enabled());
        // Disabling drops the buffered entries immediately.
        assert_eq!(profiler.recording_entry_count(), 0);
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Inactive);
        // And recording stays off.
        profiler.add_entry("dropped", EventType::Start, 0, CATEGORY_CPP);
        assert_eq!(profiler.recording_entry_count(), 0);
        // Re-enabling arms the next tick again.
        profiler.enable(true);
        assert!(profiler.watcher_enabled());
        profiler.tick();
        profiler.add_entry("kept", EventType::Start, 0, CATEGORY_CPP);
        assert_eq!(profiler.recording_entry_count(), 1);
        // Enabling while already active does nothing.
        profiler.enable(true);
        profiler.tick();
        // The tick processed a one-entry frame: the counter advances even
        // though the identifier guard skipped the entry.
        assert_eq!(profiler.get_frame_number(), 1);
    }

    #[test]
    fn freeze_gates_processing_and_frame_advance() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        profiler.freeze(true, true);
        assert!(profiler.watcher_get_dump_profile());
        clock.store(100, Ordering::SeqCst);
        profiler.add_entry("a", EventType::Start, 0, CATEGORY_CPP);
        profiler.tick();
        // Frozen: the frame is not processed and frame_ does not advance,
        // but the buffer still swaps (entries are consumed).
        assert_eq!(profiler.get_frame_number(), 0);
        assert_eq!(profiler.display_entry_count(), 0);
        assert_eq!(profiler.final_entry_count(), 1);
        profiler.freeze(false, false);
        clock.store(200, Ordering::SeqCst);
        profiler.add_entry("a", EventType::End, 0, CATEGORY_CPP);
        profiler.tick();
        // The unfrozen tick processed a one-entry frame: the frame counter
        // advances but the identifier guard skips the lone entry.
        assert_eq!(profiler.get_frame_number(), 1);
        assert_eq!(profiler.display_entry_count(), 0);
    }

    #[test]
    fn hitch_freeze_arms_the_dump_only_for_a_new_hitch() {
        let (profiler, _clock) = clocked();
        profiler.init(64 * 336, 4);
        assert!(!profiler.allow_hitch_freezing());
        profiler.set_allow_hitch_freezing(true);
        assert!(profiler.allow_hitch_freezing());
        profiler.set_hitch_freeze(true);
        assert!(profiler.hitch_freeze());
        assert!(profiler.watcher_get_dump_profile());
        // A repeated hitch does not re-arm (hitch == hitchFreeze_).
        profiler.watcher_set_dump_profile(false);
        profiler.set_hitch_freeze(true);
        assert!(!profiler.watcher_get_dump_profile());
        // Releasing the hitch does not clear the dump flag; the next hitch
        // arms it again.
        profiler.set_hitch_freeze(false);
        assert!(!profiler.hitch_freeze());
        assert!(!profiler.watcher_get_dump_profile());
        profiler.set_hitch_freeze(true);
        assert!(profiler.watcher_get_dump_profile());
    }

    #[test]
    fn set_force_json_dump_parks_the_dump_count() {
        let (profiler, _clock) = clocked();
        profiler.init(16 * 336, 4);
        profiler.set_json_dump_count(3);
        profiler.set_force_json_dump(true);
        profiler.set_json_dump_count(20);
        profiler.set_force_json_dump(false);
        // The restore parks the *forced* save, not the post-force edit.
        profiler.set_force_json_dump(true);
        assert_eq!(profiler.json_dump_count_snapshot(), 15);
        profiler.set_force_json_dump(false);
        assert_eq!(profiler.json_dump_count_snapshot(), 3);
    }

    #[test]
    fn watcher_dump_profile_resets_the_index_and_state_stays_inactive() {
        let (profiler, _clock) = clocked();
        profiler.init(16 * 336, 4);
        profiler.watcher_set_dump_profile(true);
        assert!(profiler.watcher_get_dump_profile());
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Inactive);
        profiler.watcher_set_dump_profile(false);
        assert!(!profiler.watcher_get_dump_profile());
    }

    #[test]
    fn duplicate_add_this_thread_reuses_slot_without_renumbering() {
        let (profiler, _clock) = clocked();
        profiler.init(16 * 336, 4);
        // Same OS thread registers twice: the second add only re-pins the
        // TLS index and keeps the original name.
        profiler.add_this_thread("Worker");
        assert_eq!(profiler.get_thread_name(0), Some("MainThread".to_string()));
        assert_eq!(profiler.num_threads(), 1);
    }

    #[test]
    fn thread_removal_is_deferred_while_active() {
        let (profiler, clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        clock.store(100, Ordering::SeqCst);
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let p = &profiler;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                p.add_this_thread("Worker");
                assert_eq!(p.get_thread_name(1), Some("Worker".to_string()));
                gate_rx.recv().expect("gate");
                p.add_entry("w", EventType::Start, 0, CATEGORY_PYTHON);
                p.remove_thread(this_thread_id());
                done_tx.send(()).expect("done");
            });
            gate_tx.send(()).expect("gate");
            done_rx.recv().expect("done");
            // Active: the removal is deferred; the slot still answers.
            assert_eq!(p.get_thread_name(1), Some("Worker".to_string()));
            clock.store(200, Ordering::SeqCst);
            p.add_entry("a", EventType::End, 0, CATEGORY_CPP);
            clock.store(400, Ordering::SeqCst);
            p.tick();
            // The deferred removal ran at the end of processing, after the
            // worker's entry was built into the frame hierarchy. The worker
            // entry is unclosed (its END never came), so it is clamped to
            // the frame end stamp 400.
            assert_eq!(p.get_thread_name(1), None);
            assert_eq!(p.num_threads(), 1);
            assert_eq!(p.display_entry_count(), 1);
            // The removal cleared the slot's head (profiler.cpp:2495); the
            // processed hierarchy stays observable through the pool.
            assert_eq!(p.thread_head(1), None);
            assert_eq!(p.thread_head(0), None);
            let pool = p.display_pool();
            assert_eq!(pool[0].name_str(), "w");
            assert_eq!(pool[0].num_inclusive_cycles, 300);
            assert_eq!(pool[0].parent, None);
        });
    }

    #[test]
    fn thread_removal_is_immediate_while_inactive() {
        let (profiler, _clock) = clocked();
        profiler.init(64 * 336, 4);
        let (tx, rx) = std::sync::mpsc::channel::<u64>();
        let p = &profiler;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                p.add_this_thread("Worker");
                tx.send(this_thread_id()).expect("send id");
            });
            let worker_id = rx.recv().expect("id");
            // Inactive: removed right away. MainThread still counts, so the
            // table holds one live slot.
            p.remove_thread(worker_id);
            assert_eq!(p.get_thread_name(1), None);
            assert_eq!(p.num_threads(), 1);
            // A removed slot is reused by the next registration.
            std::thread::scope(|inner| {
                inner.spawn(|| {
                    p.add_this_thread("Second");
                });
            });
            assert_eq!(p.get_thread_name(1), Some("Second".to_string()));
            assert_eq!(p.num_threads(), 2);
        });
    }

    #[test]
    fn thread_table_full_is_rejected() {
        let (profiler, _clock) = clocked();
        profiler.init(16 * 336, 1);
        let p = &profiler;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                // No empty slot: the thread is not added and its index stays
                // unregistered (-1).
                p.add_this_thread("Extra");
            });
        });
        assert_eq!(p.num_threads(), 1);
        assert_eq!(p.get_thread_name(0), Some("MainThread".to_string()));
        assert_eq!(p.get_thread_name(1), None);
    }

    #[test]
    fn unregistered_threads_record_with_minus_one_index() {
        let (profiler, _clock) = clocked();
        profiler.init(16 * 336, 1);
        profiler.enable(true);
        profiler.tick();
        let p = &profiler;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Registered nowhere: TLS stays -1, entries still record.
                p.add_entry("stray", EventType::Start, 0, CATEGORY_CPP);
            });
        });
        assert_eq!(p.recording_entry_count(), 1);
        p.tick();
        let stray = p.final_entry(0).expect("recorded");
        assert_eq!(stray.thread_idx, -1);
        assert_eq!(p.display_entry_count(), 0);
    }

    #[test]
    fn pre_init_calls_are_safe_noops() {
        let (profiler, _clock) = clocked();
        profiler.tick();
        profiler.add_entry("x", EventType::Start, 0, CATEGORY_CPP);
        profiler.enable(false);
        profiler.fini();
        assert_eq!(profiler.get_frame_number(), 0);
        assert!(!profiler.watcher_enabled());
    }

    #[test]
    fn memory_size_zero_yields_zero_capacity() {
        let (profiler, _clock) = clocked();
        profiler.init(0, 4);
        assert_eq!(profiler.max_entry_count(), 0);
        assert_eq!(profiler.max_display_entry_count(), 0);
        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("x", EventType::Start, 0, CATEGORY_CPP);
        assert_eq!(profiler.recording_entry_count(), 0);
        profiler.tick();
        assert_eq!(profiler.final_entry_count(), 0);
        assert_eq!(profiler.get_frame_number(), 0);
    }

    #[test]
    fn fini_disables_and_uninits() {
        let (profiler, _clock) = clocked();
        profiler.init(16 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        profiler.fini();
        assert!(!profiler.watcher_enabled());
        assert_eq!(profiler.get_thread_name(0), None);
        profiler.tick();
        profiler.add_entry("x", EventType::Start, 0, CATEGORY_CPP);
        assert_eq!(profiler.recording_entry_count(), 0);
        profiler.fini();
    }

    #[test]
    fn max_modes_and_event_type_values_match_the_reference() {
        assert_eq!(ProfileMode::MaxModes as i32, 7);
        assert_eq!(ProfileMode::SortByTime as i32, 2);
        assert_eq!(EventType::Counter as i32, 3);
        assert_eq!(EventType::StartIdle as i32, 1);
        assert_eq!(JsonDumpState::Failed as i32, 3);
        assert_eq!(CATEGORY_ALL, 7);
        assert_eq!(CATEGORY_PYTHON, 4);
        assert_eq!(MAX_NUM_TOP_ENTRIES, 10);
        assert_eq!(HISTORY_LENGTH, 512);
        assert_eq!(MAX_STACK_DEPTH, 64);
        assert_eq!(MAXIMUM_NUMBER_OF_THREADS, 96);
        assert_eq!(ENTRY_NAME_SIZE, 96);
    }

    #[test]
    fn real_clock_and_pre_init_guards_are_safe() {
        // Covers the default clock path and Default.
        let profiler = Profiler::default();
        assert!(profiler.init(16 * 336, 2));
        assert!(!profiler.watcher_enabled());
        // Pre-init guards on a fresh instance: nothing may panic.
        let bare = Profiler::new();
        bare.remove_thread(this_thread_id());
        bare.add_this_thread("x");
        assert_eq!(bare.get_thread_name(-1), None);
        assert_eq!(bare.get_thread_visibility(0), None);
        bare.prev_category();
        bare.next_category();
        bare.set_current_category("C++");
        assert_eq!(bare.get_current_category(), "");
        assert_eq!(bare.get_categories(), "");
        assert_eq!(bare.final_end_of_frame_time_stamp(), 0);
        assert_eq!(bare.time_taken_for_profiling(), 0.0);
        bare.set_profile_bar_visibility(false);
        assert!(!bare.get_profile_bar_visibility());
    }

    #[test]
    fn second_child_of_a_parent_links_through_the_sibling_tail() {
        let (profiler, _clock) = clocked();
        profiler.init(64 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("a", EventType::Start, 0, CATEGORY_CPP);
        profiler.add_entry("b", EventType::Start, 0, CATEGORY_CPP);
        profiler.add_entry("b", EventType::End, 0, CATEGORY_CPP);
        profiler.add_entry("c", EventType::Start, 0, CATEGORY_CPP);
        profiler.add_entry("c", EventType::End, 0, CATEGORY_CPP);
        profiler.add_entry("a", EventType::End, 0, CATEGORY_CPP);
        profiler.tick();
        let pool = profiler.display_pool();
        // b and c are siblings under a; the second child links through the
        // per-depth sibling tail.
        assert_eq!(pool[0].child, Some(1));
        assert_eq!(pool[1].sibling, Some(2));
        assert_eq!(pool[2].sibling, None);
        assert_eq!(pool[2].parent, Some(0));
    }

    #[test]
    fn unclosed_idle_and_over_depth_sections_clamp_per_the_reference() {
        let (profiler, clock) = clocked();
        profiler.init(400 * 336, 4);
        profiler.enable(true);
        profiler.tick();
        // An idle section left open at the frame end only fills its idle
        // cycles (profiler.cpp:1733-1737).
        clock.store(100, Ordering::SeqCst);
        profiler.add_entry("wait", EventType::StartIdle, 0, CATEGORY_CPP);
        // Sixty-five nested sections left open push the depth past the cap;
        // the clamp pops them without reading a slot for the overflow level.
        for _ in 0..65 {
            profiler.add_entry("deep", EventType::Start, 0, CATEGORY_CPP);
        }
        clock.store(2000, Ordering::SeqCst);
        profiler.tick();
        let pool = profiler.display_pool();
        assert_eq!(pool[0].name_str(), "wait");
        assert_eq!(pool[0].num_inclusive_idle_cycles, 1900);
        assert_eq!(pool[0].num_inclusive_cycles, 0);
        // The idle section occupied depth 0, so the first 63 deep sections
        // were stored before the depth cap; all clamp to the frame end.
        assert_eq!(pool[1].num_inclusive_cycles, 1900);
        assert_eq!(profiler.display_entry_count(), 64);
        assert_eq!(profiler.final_end_of_frame_time_stamp(), 2000);
        assert!(profiler.time_taken_for_profiling() >= 0.0);
    }

    // ==== BW-12-03-03: the JSON dump writer (`profiler.cpp:1198-1492`) ====

    /// A raw entry with full control over category and thread slot.
    fn raw_entry(
        name: &str,
        event: EventType,
        time: u64,
        category: i32,
        thread_idx: i32,
        frame_number: i32,
    ) -> ProfileEntry {
        let mut e = entry(name, event, time, thread_idx, frame_number);
        e.category = category;
        e
    }

    /// A writable scratch directory unique to this test run.
    fn dump_dir(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("bw120303-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn the_json_dump_path_matches_the_reference_schema() {
        // `"%s/%s_profile_%04d%02d%02d_%02d%02d%02d.json"`
        // (`profiler.cpp:1308-1313`): the comment there warns the
        // WebConsole depends on the schema.
        let mut t: libc::tm = unsafe { std::mem::zeroed() };
        t.tm_year = 126;
        t.tm_mon = 10;
        t.tm_mday = 25;
        t.tm_hour = 14;
        t.tm_min = 3;
        t.tm_sec = 2;
        assert_eq!(
            json_dump_file_path("/tmp/d", "cellapp", &t),
            "/tmp/d/cellapp_profile_20261125_140302.json"
        );
        let mut t: libc::tm = unsafe { std::mem::zeroed() };
        t.tm_year = -913; // year 987 (tm_year is relative to 1900)
        t.tm_mon = 0;
        t.tm_mday = 2;
        t.tm_hour = 3;
        t.tm_min = 4;
        t.tm_sec = 5;
        assert_eq!(
            json_dump_file_path("/tmp/d", "cellapp", &t),
            "/tmp/d/cellapp_profile_09870102_030405.json"
        );
    }

    /// Fixed events in, byte-exact Chrome Trace Event file out: the
    /// thread meta line, B/E/C phases, the category map lookup and the
    /// `ts` microsecond offsets all pinned verbatim
    /// (`profiler.cpp:1382-1472`).
    #[test]
    fn json_dump_writes_the_reference_face_for_fixed_events() {
        let (profiler, _stamp) = clocked();
        assert!(profiler.init(128 * 1024 * 1024, 16));
        profiler.enable(true);
        profiler.tick(); // activation only
        let dir = dump_dir("fixed");
        profiler.set_json_dump_dir(&dir);
        profiler.watcher_set_dump_profile(true);

        // stamps_per_second is 1e6, so one stamp is one microsecond and
        // `ts` is the raw offset from the frame's first entry stamp.
        profiler.debug_inject_raw_entry(raw_entry(
            "work",
            EventType::Start,
            1000,
            CATEGORY_CPP,
            0,
            1,
        ));
        profiler.debug_inject_raw_entry(raw_entry(
            "work",
            EventType::End,
            1500,
            CATEGORY_PYTHON,
            0,
            1,
        ));
        profiler.debug_inject_raw_entry(raw_entry(
            "idle",
            EventType::StartIdle,
            1600,
            CATEGORY_ALL,
            0,
            1,
        ));
        profiler.debug_inject_raw_entry(raw_entry(
            "fps",
            EventType::Counter,
            2000,
            CATEGORY_CPP,
            0,
            1,
        ));
        profiler.tick();

        let path = profiler.get_json_output_file_path();
        assert!(path.starts_with(&format!("{dir}/")), "path: {path}");
        assert!(path.ends_with(".json"), "path: {path}");
        assert!(path.contains("_profile_"), "path: {path}");
        // Default count is 1: the frame both opened and completed the file.
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Completed);
        assert!(!profiler.watcher_get_dump_profile());

        let basename = std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let name = path.rsplit('/').next().unwrap();
        assert!(name.starts_with(&basename), "path: {path}");

        let expected = "[\
             {\"pid\":0,\"tid\":0, \"ph\": \"M\", \"name\":\"thread_name\",\"args\":{\"name\": \"MainThread\"} } ,\
             { \"cat\":\"C++\", \"pid\":0,\"tid\":0, \"ts\":0,\"ph\":\"B\",\"name\":\"work\"}\
             ,{ \"cat\":\"Python\", \"pid\":0,\"tid\":0, \"ts\":500,\"ph\":\"E\",\"name\":\"work\"}\
             ,{ \"cat\":\"All\", \"pid\":0,\"tid\":0, \"ts\":600,\"ph\":\"B\",\"name\":\"idle\"}\
             ,{ \"cat\":\"C++\", \"pid\":0,\"tid\":0, \"ts\":1000,\"ph\":\"C\",\"name\":\"fps\",\"args\":{\"counter\": 0}}]";
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    /// `jsonDumpCount_` frames share one file: the comma separator
    /// persists (`jsonFileHasData_`), the thread meta line repeats every
    /// frame and every `ts` stays anchored to the first frame's first
    /// entry (`profiler.cpp:1239-1242`, `1406`).
    #[test]
    fn json_dump_count_frames_share_one_file_and_one_time_anchor() {
        let (profiler, _stamp) = clocked();
        assert!(profiler.init(128 * 1024 * 1024, 16));
        profiler.enable(true);
        profiler.tick();
        profiler.set_json_dump_dir(&dump_dir("count"));
        profiler.set_json_dump_count(2);
        profiler.watcher_set_dump_profile(true);

        profiler.debug_inject_raw_entry(raw_entry(
            "work",
            EventType::Start,
            1000,
            CATEGORY_CPP,
            0,
            1,
        ));
        profiler.debug_inject_raw_entry(raw_entry(
            "work",
            EventType::End,
            1500,
            CATEGORY_CPP,
            0,
            1,
        ));
        profiler.tick();
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Active);
        assert!(profiler.watcher_get_dump_profile());

        profiler.debug_inject_raw_entry(raw_entry(
            "run",
            EventType::Start,
            5000,
            CATEGORY_CPP,
            0,
            2,
        ));
        profiler.debug_inject_raw_entry(raw_entry(
            "fps",
            EventType::Counter,
            5600,
            CATEGORY_CPP,
            0,
            2,
        ));
        profiler.tick();
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Completed);

        let path = profiler.get_json_output_file_path();
        let meta = "{\"pid\":0,\"tid\":0, \"ph\": \"M\", \"name\":\"thread_name\",\"args\":{\"name\": \"MainThread\"} } ";
        let expected = format!(
            "[{meta},\
             {{ \"cat\":\"C++\", \"pid\":0,\"tid\":0, \"ts\":0,\"ph\":\"B\",\"name\":\"work\"}}\
             ,{{ \"cat\":\"C++\", \"pid\":0,\"tid\":0, \"ts\":500,\"ph\":\"E\",\"name\":\"work\"}}\
             ,{meta}\
             ,{{ \"cat\":\"C++\", \"pid\":0,\"tid\":0, \"ts\":4000,\"ph\":\"B\",\"name\":\"run\"}}\
             ,{{ \"cat\":\"C++\", \"pid\":0,\"tid\":0, \"ts\":4600,\"ph\":\"C\",\"name\":\"fps\",\"args\":{{\"counter\": 0}}}}]"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    /// `watcherSetDumpProfile(false)` stops after the NEXT frame:
    /// `processEntries`' else arm closes the open file with its `]` and
    /// reports completed (`profiler.cpp:1489-1492`, `1249-1258`).
    #[test]
    fn json_dump_stops_after_dumping_is_disabled() {
        let (profiler, _stamp) = clocked();
        assert!(profiler.init(128 * 1024 * 1024, 16));
        profiler.enable(true);
        profiler.tick();
        profiler.set_json_dump_dir(&dump_dir("stop"));
        profiler.set_json_dump_count(5);
        profiler.watcher_set_dump_profile(true);

        profiler.debug_inject_raw_entry(entry("work", EventType::Start, 1000, 0, 1));
        profiler.debug_inject_raw_entry(entry("work", EventType::End, 1500, 0, 1));
        profiler.tick();
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Active);

        profiler.watcher_set_dump_profile(false);
        profiler.debug_inject_raw_entry(entry("lost", EventType::Start, 2000, 0, 2));
        profiler.tick();
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Completed);
        assert!(!profiler.watcher_get_dump_profile());

        let path = profiler.get_json_output_file_path();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with('['), "content: {content}");
        assert!(content.ends_with("]"), "content: {content}");
        assert!(content.contains("\"ts\":0,\"ph\":\"B\",\"name\":\"work\""));
        assert!(content.contains("\"ts\":500,\"ph\":\"E\",\"name\":\"work\""));
        // The stop frame contributes nothing: its "lost" entry sat in the
        // final buffer, but the else arm only closes the file - the meta
        // line plus the two frame-1 entries are all that "ph" there is.
        assert_eq!(content.matches("\"ph\"").count(), 3);
        assert!(!content.contains("lost"));
    }

    /// An unwritable dump directory fails the open, resets the index,
    /// drops the flag and reports `JSON_DUMPING_FAILED`
    /// (`profiler.cpp:1218-1225`).
    #[test]
    fn json_dump_open_failure_reports_failed_and_drops_the_flag() {
        let (profiler, _stamp) = clocked();
        assert!(profiler.init(128 * 1024 * 1024, 16));
        profiler.enable(true);
        profiler.tick();
        // A regular file where the directory should be: the create fails
        // with ENOTDIR even for root.
        let blocker = std::env::temp_dir().join(format!("bw120303-file-{}", std::process::id()));
        std::fs::write(&blocker, b"").unwrap();
        profiler.set_json_dump_dir(blocker.to_string_lossy().as_ref());
        profiler.watcher_set_dump_profile(true);

        profiler.debug_inject_raw_entry(entry("work", EventType::Start, 1000, 0, 1));
        profiler.tick();
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Failed);
        assert!(!profiler.watcher_get_dump_profile());
        assert_eq!(profiler.get_json_output_file_path(), "");
        let _ = std::fs::remove_file(&blocker);
    }

    /// Unregistered masks render as an empty `cat` (the reference's
    /// `BW::map::operator[]` default), thread-slot -1 entries are skipped
    /// whole, and a slot number with no registered thread still prints
    /// (`profiler.cpp:1411-1414`, `1421-1422`).
    #[test]
    fn json_dump_category_and_thread_slot_faces() {
        let (profiler, _stamp) = clocked();
        assert!(profiler.init(128 * 1024 * 1024, 16));
        profiler.enable(true);
        profiler.tick();
        profiler.set_json_dump_dir(&dump_dir("faces"));
        profiler.watcher_set_dump_profile(true);

        // The skipped entry still anchors `jsonDumpStartTime_`.
        profiler.debug_inject_raw_entry(entry("ghost", EventType::Start, 1000, -1, 1));
        profiler.debug_inject_raw_entry(raw_entry(
            "other",
            EventType::Start,
            1100,
            CATEGORY_GPU,
            3,
            1,
        ));
        profiler.debug_inject_raw_entry(entry("real", EventType::Start, 1200, 0, 1));
        profiler.tick();

        let path = profiler.get_json_output_file_path();
        let expected = "[\
             {\"pid\":0,\"tid\":0, \"ph\": \"M\", \"name\":\"thread_name\",\"args\":{\"name\": \"MainThread\"} } ,\
             { \"cat\":\"\", \"pid\":0,\"tid\":3, \"ts\":100,\"ph\":\"B\",\"name\":\"other\"}\
             ,{ \"cat\":\"C++\", \"pid\":0,\"tid\":0, \"ts\":200,\"ph\":\"B\",\"name\":\"real\"}]";
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    /// A frozen profiler still dumps: the hookup runs before the freeze
    /// bail-out (`profiler.cpp:1485-1500`).
    #[test]
    fn json_dump_runs_before_the_freeze_bail_out() {
        let (profiler, _stamp) = clocked();
        assert!(profiler.init(128 * 1024 * 1024, 16));
        profiler.enable(true);
        profiler.tick();
        profiler.set_json_dump_dir(&dump_dir("frozen"));
        profiler.freeze(true, false);
        profiler.watcher_set_dump_profile(true);

        profiler.debug_inject_raw_entry(entry("work", EventType::Start, 1000, 0, 1));
        profiler.debug_inject_raw_entry(entry("work", EventType::End, 1500, 0, 1));
        profiler.tick();
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Completed);
        let path = profiler.get_json_output_file_path();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"name\":\"work\"")
        );
    }

    /// Re-arming mid-file resets the index, so the next frame closes the
    /// stale file and opens a fresh one (`profiler.cpp:1205-1213`).
    #[test]
    fn json_dump_rearm_reopens_a_fresh_file() {
        let (profiler, _stamp) = clocked();
        assert!(profiler.init(128 * 1024 * 1024, 16));
        profiler.enable(true);
        profiler.tick();
        profiler.set_json_dump_dir(&dump_dir("rearm"));
        profiler.set_json_dump_count(3);
        profiler.watcher_set_dump_profile(true);
        profiler.debug_inject_raw_entry(entry("work", EventType::Start, 1000, 0, 1));
        profiler.tick();
        let first_path = profiler.get_json_output_file_path();
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Active);

        profiler.watcher_set_dump_profile(true);
        profiler.debug_inject_raw_entry(entry("work", EventType::End, 2000, 0, 2));
        profiler.tick();
        assert_eq!(profiler.get_json_dump_state(), JsonDumpState::Active);
        // Within the same wall-clock second the new file replaces the old
        // one at the same schema name; either way the fresh file starts
        // with its own `[`.
        let path = profiler.get_json_output_file_path();
        assert!(path.ends_with(".json"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with('['), "content: {content}");
        let _ = first_path;
    }
}
