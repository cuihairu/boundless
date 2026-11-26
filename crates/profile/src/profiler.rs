//! The `Profiler` (`cstdmf/profiler.hpp`, `cstdmf/profiler.cpp`): the
//! entry-frame collector the `_bw_profile` module drives
//! (`server/tools/bw_profile/bw_profile.cpp`), the start/stop interface of
//! BW-12-03-02.
//!
//! The core is a double-buffered event log: producers append
//! [`Profiler::add_entry`] records into the profile buffer while it is
//! active, and [`Profiler::tick`] swaps the finished frame's buffer into
//! the final slot, bumping the frame number (`profiler.cpp:1108-1190`).
//! Threads register through [`Profiler::add_this_thread`] — the slot
//! index their entries carry — and deregister through
//! [`Profiler::remove_thread`], deferred to the frame consumer while
//! profiling runs (`profiler.cpp:2448-2473`).
//!
//! The port delivers the start/stop core the module face needs: the
//! entry/buffer bookkeeping, the thread table, `enable`/`watcherEnabled`
//! and the `profiler/enabled` watcher `init` registers
//! (`profiler.cpp:896-899`). The deep internals stay with their own items:
//! the display-entry hierarchy, `processEntries` and its background task
//! are BW-23's (the section note at the BW-12-03 heading), and the JSON
//! dump is BW-12-03-03's. The port's buffers therefore grow on demand up
//! to the capacity `init` computes, where the reference pre-allocates —
//! the observable contract, the capacity bound and the pin-when-full
//! behavior, is what the port keeps (`profiler.ipp:67-85`).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::stamps_to_seconds;
use foundation::time::stamp_now;
use watcher::{Mode, Watcher, add_watcher};

/// `MAXIMUM_NUMBER_OF_THREADS` (`profiler.hpp:141-142`): the arbitrary
/// limit the thread table is sized by.
pub const MAXIMUM_NUMBER_OF_THREADS: usize = 96;

/// `ENTRY_NAME_SIZE` (`profiler.hpp:208`): the fixed per-entry name
/// storage.
pub const ENTRY_NAME_SIZE: usize = 96;

/// `sizeof(DisplayEntry)` of the reference's server build
/// (`profiler.hpp:199-210`: three pointers, the name, four `uint64`
/// cycle counters and `isIdle_`), the capacity formula's other term
/// (`profiler.cpp:986-988`). The display entries themselves are BW-23's;
/// only the size is this item's.
const DISPLAY_ENTRY_SIZE: usize = 160;

/// `EventType` (`profiler.hpp:173-177`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(i32)]
pub enum EventType {
    /// `EVENT_START`.
    #[default]
    Start,
    /// `EVENT_START_IDLE`.
    StartIdle,
    /// `EVENT_END`.
    End,
    /// `EVENT_COUNTER`.
    Counter,
}

/// `EventCategory` (`profiler.hpp:179-185`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(i32)]
pub enum EventCategory {
    /// `CATEGORY_CPP` — the default of `addEntry` (`profiler.ipp:25`).
    #[default]
    Cpp = 1 << 0,
    /// `CATEGORY_GPU`.
    Gpu = 1 << 1,
    /// `CATEGORY_PYTHON`.
    Python = 1 << 2,
    /// `CATEGORY_ALL`.
    All = (1 << 3) - 1,
}

/// `JsonDumpState` (`profiler.hpp:190-195`). The states beyond
/// `Inactive` only arrive with the dump machine of BW-12-03-03; `enable`
/// already resets the field the way the reference does
/// (`profiler.cpp:2538-2561`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(i32)]
pub enum JsonDumpState {
    /// `JSON_DUMPING_INACTIVE`.
    #[default]
    Inactive,
    /// `JSON_DUMPING_ACTIVE`.
    Active,
    /// `JSON_DUMPING_COMPLETED`.
    Completed,
    /// `JSON_DUMPING_FAILED`.
    Failed,
}

/// One collected record: `ProfileEntry` (`profiler.hpp:184-197`). The
/// name is the fixed `ENTRY_NAME_SIZE` storage with C-string semantics —
/// NUL-terminated, truncated at the last byte the storage can hold —
/// because callers may hand names that outlive nothing (a Python frame's
/// code object).
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ProfileEntry {
    /// `event_`.
    pub event: EventType,
    /// `category_`.
    pub category: EventCategory,
    /// `name_`: NUL-terminated, at most `ENTRY_NAME_SIZE - 1` bytes.
    pub name: [u8; ENTRY_NAME_SIZE],
    /// `time_`: the `timestamp()` of the record.
    pub time: u64,
    /// `threadIdx_`: the registering thread's slot, 0 before any
    /// registration (the tls default, `profiler.ipp:25-51`).
    pub thread_idx: i32,
    /// `value_`.
    pub value: i32,
    /// `frameNumber_`.
    pub frame_number: i32,
}

impl ProfileEntry {
    /// The zeroed record — the storage the entry is filled into
    /// (`profiler.ipp:33`, `buffer.entries.push` in the port).
    fn zeroed() -> Self {
        Self {
            event: EventType::Start,
            category: EventCategory::Cpp,
            name: [0; ENTRY_NAME_SIZE],
            time: 0,
            thread_idx: 0,
            value: 0,
            frame_number: 0,
        }
    }

    /// The entry's name as text — the bytes up to the first NUL, the way
    /// the reference reads `name_` back.
    pub fn name(&self) -> &str {
        let end = self
            .name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(ENTRY_NAME_SIZE);
        std::str::from_utf8(&self.name[..end]).unwrap_or("")
    }
}

/// `EntryBuffer` (`profiler.hpp:263-268`): the entries plus the frame's
/// end stamp. `entryCount_` is the vector's length — the port grows the
/// buffer on demand and the capacity bound is [`Profiler`]'s
/// `max_entry_count`.
#[derive(Clone, Debug, Default)]
pub struct EntryBuffer {
    /// `entries_`.
    pub entries: Vec<ProfileEntry>,
    /// `endOfFrameTimeStamp_`.
    pub end_of_frame_time_stamp: u64,
}

/// `ThreadEntry` (`profiler.hpp:225-233`): the thread table slot. The
/// display-entry pointers (`head_`, `hierHead_`) belong to the hierarchy
/// processing of BW-23 and stay out.
#[derive(Clone, Debug)]
struct ThreadEntry {
    /// `thread_`: 0 marks an empty slot.
    thread: u32,
    /// `name_`: "Dead Thread" until a registration fills the slot,
    /// "Deleted" after a removal empties it.
    name: String,
    /// `visible_`.
    visible: bool,
}

impl ThreadEntry {
    /// The `threads_[i]` the constructor leaves: empty and dead
    /// (`profiler.cpp:1004-1012`).
    fn dead() -> Self {
        Self {
            thread: 0,
            name: "Dead Thread".to_string(),
            visible: false,
        }
    }
}

/// The collector (`profiler.hpp:126-`, `profiler.cpp:718-766` the
/// constructor's defaults). The un-inited profiler is inert: the
/// reference guards on `threads_ == NULL` and on the never-pending
/// early return of `tick`; the port guards on its empty thread table.
pub struct Profiler {
    /// The two entry buffers (`profileEntryBuffers_`), double-buffered
    /// so a frame's records survive the next frame's collection.
    buffers: [EntryBuffer; 2],
    /// Which `buffers` slot is `profileEntryBuffer_`.
    profile_index: usize,
    /// Which `buffers` slot is `finalProfileEntryBuffer_`.
    final_index: usize,
    /// `maxEntryCount_`.
    max_entry_count: usize,
    /// `threads_`.
    threads: Vec<ThreadEntry>,
    /// `maxNumThreads_`.
    max_num_threads: usize,
    /// `numThreads_`.
    num_threads: usize,
    /// `threadsToRemove_` — a `BW::set`, so the consumer's sweep walks
    /// in ascending id order.
    threads_to_remove: BTreeSet<u32>,
    /// `isActive_`.
    is_active: bool,
    /// `isPendingActive_`.
    is_pending_active: bool,
    /// `hasGeneratedProfile_`.
    has_generated_profile: bool,
    /// `profileFrameNumber_`.
    profile_frame_number: i32,
    /// `frameStartTime_`.
    frame_start_time: u64,
    /// `highWatermarkOfEntryCount_`.
    high_watermark_of_entry_count: usize,
    /// `jsonDumpState_`.
    json_dump_state: JsonDumpState,
    /// `doJsonDump_` (`profiler.hpp:453`).
    do_json_dump: bool,
    /// `jsonDumpIndex_` (`profiler.hpp:454`): the frames written into
    /// the open file so far.
    json_dump_index: i32,
    /// `jsonDumpCount_` (`profiler.hpp:455`): the frames per file.
    json_dump_count: i32,
    /// `jsonDumpStartTime_` (`profiler.hpp:456`): the file's `ts`
    /// origin — the first entry of the first dumped frame.
    json_dump_start_time: u64,
    /// `jsonFileHasData_` (`profiler.hpp:457`): the comma gate across
    /// the whole file.
    json_file_has_data: bool,
    /// `jsonOutputFile_` (`profiler.hpp:458`): the open dump stream;
    /// `None` is the reference's NULL.
    json_output_file: Option<std::fs::File>,
    /// `jsonOutputFilePath_` (`profiler.hpp:459`).
    json_output_file_path: String,
    /// `jsonDumpDir_` (`profiler.hpp:460`).
    json_dump_dir: String,
}

impl Profiler {
    /// The constructor's state (`profiler.cpp:718-766`): everything off,
    /// zero, or `JSON_DUMPING_INACTIVE`.
    pub fn new() -> Self {
        Self {
            buffers: [EntryBuffer::default(), EntryBuffer::default()],
            profile_index: 0,
            final_index: 1,
            max_entry_count: 0,
            threads: Vec::new(),
            max_num_threads: MAXIMUM_NUMBER_OF_THREADS,
            num_threads: 0,
            threads_to_remove: BTreeSet::new(),
            is_active: false,
            is_pending_active: false,
            has_generated_profile: false,
            profile_frame_number: 0,
            frame_start_time: 0,
            high_watermark_of_entry_count: 0,
            json_dump_state: JsonDumpState::Inactive,
            do_json_dump: false,
            json_dump_index: 0,
            json_dump_count: 1,
            json_dump_start_time: 0,
            json_file_has_data: false,
            json_output_file: None,
            json_output_file_path: String::new(),
            json_dump_dir: String::new(),
        }
    }

    /// `init` (`profiler.cpp:881-1050`): size the entry capacity, stand
    /// up the double buffers and the thread table, reset the run state.
    ///
    /// The capacity formula is the reference's
    /// (`profiler.cpp:986-988`): the budget split over two entry buffers
    /// plus a half-size display-entry allowance, where the entry size is
    /// the port's own `repr(C)` mirror of the reference's server-build
    /// `ProfileEntry` and the display allowance is the reference's
    /// `sizeof(DisplayEntry)`. The thread table starts as all "Dead
    /// Thread" (`profiler.cpp:1004-1012`) and the run state resets the
    /// constructor's way (`profiler.cpp:1046-1048`), so a re-init leaves
    /// no stale frame behind.
    ///
    /// The reference registers its watchers here (`profiler.cpp:886-975`);
    /// the port mounts the start/stop one separately through
    /// [`mount_enabled_watcher`], which addresses the global instance
    /// the way the reference's watcher addresses `g_profiler`.
    pub fn init(&mut self, memory_size: i32, max_num_threads: usize) -> bool {
        // The `TRACE_MSG` of `profiler.cpp:885-887` and the memory stat
        // it feeds are display-side; the budget's observable effect here
        // is the capacity it buys.
        let denominator =
            std::mem::size_of::<ProfileEntry>() as i32 * 2 + DISPLAY_ENTRY_SIZE as i32 / 2;
        self.max_entry_count = (memory_size / denominator) as usize;

        self.buffers = [EntryBuffer::default(), EntryBuffer::default()];
        self.profile_index = 0;
        self.final_index = 1;
        self.max_num_threads = max_num_threads;
        self.threads = vec![ThreadEntry::dead(); max_num_threads];
        self.num_threads = 0;
        self.threads_to_remove.clear();
        // "By default, dumping to the executable directory"
        // (`profiler.cpp:1070`).
        self.json_dump_dir = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|dir| dir.to_string_lossy().into_owned()))
            .unwrap_or_default();
        self.is_active = false;
        self.is_pending_active = false;
        self.profile_frame_number = 0;
        self.frame_start_time = 0;
        self.high_watermark_of_entry_count = 0;
        self.json_dump_state = JsonDumpState::Inactive;
        true
    }

    /// `tick` (`profiler.cpp:1108-1190`): the frame boundary. Nothing
    /// happens while the profiler is off; a tick while active hands the
    /// collected frame to the consumer — the reference queues the
    /// `processEntries` background task (`profiler.cpp:1162-1165`,
    /// BW-23's machinery); the port keeps the buffer where the consumer
    /// reads it — then every path arms the active state and restarts the
    /// frame clock.
    pub fn tick(&mut self) {
        if !self.is_pending_active && !self.is_active {
            return;
        }

        if self.is_active && !self.profile_entries().is_empty() {
            // `profiler.cpp:1128-1146`: zero the final slot's count,
            // swap the buffer pointers, bump the frame the entries
            // carry, stamp the handed-over buffer, keep the high
            // watermark.
            self.buffers[self.final_index].entries.clear();
            std::mem::swap(&mut self.profile_index, &mut self.final_index);
            self.profile_frame_number += 1;
            let now = stamp_now();
            self.buffers[self.final_index].end_of_frame_time_stamp = now;
            let entry_count = self.buffers[self.final_index].entries.len();
            self.high_watermark_of_entry_count =
                self.high_watermark_of_entry_count.max(entry_count);
        }

        self.is_pending_active = false;
        self.is_active = true;
        self.frame_start_time = stamp_now();

        // The consumer side of the hand-over (`profiler.cpp:1483-1491`):
        // the reference's queued `processEntries` task dumps or closes
        // the JSON file at this boundary (`forceJsonDump_` belongs to
        // the hitch/freeze machinery and stays out); the port runs the
        // step inline, the background task itself being BW-23's.
        if self.do_json_dump {
            self.json_dump();
        } else if self.json_output_file.is_some() {
            self.stop_json_dump();
        }
    }

    /// `addEntry` (`profiler.ipp:25-51`): append one record — gated on
    /// the active state, named into the fixed storage, stamped, tagged
    /// with this thread's slot and the current frame number.
    pub fn add_entry(&mut self, text: &str, event: EventType, value: i32, category: EventCategory) {
        if !self.is_active {
            return;
        }
        // Read before the buffer claim: the claim mutably borrows the
        // profiler (`profiler.ipp:44` reads the same field after the
        // claim in the reference).
        let frame_number = self.profile_frame_number;
        let Some(entry) = self.start_next_entry() else {
            return;
        };
        // `strncpy(name_, text, ENTRY_NAME_SIZE - 1)` with the last byte
        // pinned to NUL: a C-string copy of at most 95 bytes, zero-filled.
        let bytes = text.as_bytes();
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len())
            .min(ENTRY_NAME_SIZE - 1);
        entry.name = [0; ENTRY_NAME_SIZE];
        entry.name[..end].copy_from_slice(&bytes[..end]);
        entry.event = event;
        entry.category = category;
        entry.time = stamp_now();
        entry.value = value;
        entry.thread_idx = thread_index();
        entry.frame_number = frame_number;
    }

    /// `startNextEntry` (`profiler.ipp:67-85`): claim the next slot in
    /// the profile buffer, `None` when the buffer is full — with the
    /// count pinned at the capacity, the reference's `entryCount_ =
    /// maxEntryCount_` clamp the overflow path performs. The port's
    /// buffer grows to the capacity, so the clamp is the length it
    /// already has.
    fn start_next_entry(&mut self) -> Option<&mut ProfileEntry> {
        let buffer = &mut self.buffers[self.profile_index];
        if buffer.entries.len() >= self.max_entry_count {
            return None;
        }
        buffer.entries.push(ProfileEntry::zeroed());
        buffer.entries.last_mut()
    }

    /// `addThisThread` (`profiler.cpp:2242-2301`): register the calling
    /// thread into the first empty slot — or, for a thread already
    /// registered, re-point the tls index at its slot and erase any
    /// pending removal. A full table refuses the registration.
    pub fn add_this_thread(&mut self, name: &str) {
        if self.threads.is_empty() {
            return;
        }
        let this_thread = our_thread_id();
        assert!(this_thread != 0);
        let mut first_empty_slot = None;
        for (slot, thread) in self.threads.iter().enumerate() {
            if this_thread == thread.thread {
                set_thread_index(slot as i32);
                self.threads_to_remove.remove(&this_thread);
                return;
            }
            if thread.thread == 0 && first_empty_slot.is_none() {
                first_empty_slot = Some(slot);
                break;
            }
        }
        let Some(slot) = first_empty_slot else {
            return;
        };
        let thread = &mut self.threads[slot];
        thread.thread = this_thread;
        thread.name = name.to_string();
        thread.visible = true;
        set_thread_index(slot as i32);
        self.num_threads += 1;
    }

    /// `removeThread` (`profiler.cpp:2448-2473`): while the profiler is
    /// off the slot empties immediately; while it runs the removal is
    /// deferred to the frame consumer's `doRemoveThreads`
    /// (`profiler.cpp:1784`, BW-23's `processEntries`) so the entries the
    /// thread produced are consumed first.
    pub fn remove_thread(&mut self, thread_id: u32) {
        if self.threads.is_empty() {
            return;
        }
        assert!(thread_id != 0);
        if !self.is_active && !self.is_pending_active {
            self.internal_remove_thread(thread_id);
            return;
        }
        self.threads_to_remove.insert(thread_id);
    }

    /// `internalRemoveThread` (`profiler.cpp:2477-2503`): mark the slot
    /// deleted.
    fn internal_remove_thread(&mut self, thread_id: u32) {
        for thread in &mut self.threads {
            if thread.thread == thread_id {
                self.num_threads -= 1;
                thread.name = "Deleted".to_string();
                thread.thread = 0;
                thread.visible = false;
                return;
            }
        }
    }

    /// `doRemoveThreads` (`profiler.cpp:2505-2518`): the consumer's
    /// sweep over the deferred removals. The reference's call site is
    /// `processEntries` (`profiler.cpp:1784`), the BW-23 consumer; the
    /// method itself is public here so the consumer's successor and the
    /// tests can drive it.
    pub fn do_remove_threads(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        let removals: Vec<u32> = self.threads_to_remove.iter().copied().collect();
        for thread_id in removals {
            self.internal_remove_thread(thread_id);
        }
        self.threads_to_remove.clear();
    }

    /// `getThreadName` (`profiler.ipp:53-62`): the registered name, or
    /// nothing for an empty or dead slot.
    pub fn get_thread_name(&self, thread_id: usize) -> Option<&str> {
        assert!(thread_id < self.max_num_threads);
        let thread = &self.threads[thread_id];
        if thread.thread != 0 {
            Some(&thread.name)
        } else {
            None
        }
    }

    /// `enable` (`profiler.cpp:2538-2561`): on at the next `tick` — with
    /// `hasGeneratedProfile_` set and the dump state reset — or off
    /// immediately, dropping the current frame's data. The off write
    /// clears `isActive_` alone: a pending arm survives it, and the
    /// next `tick` activates anyway — the reference's own semantics.
    pub fn enable(&mut self, state: bool) {
        if state {
            if !self.is_active {
                self.is_pending_active = true;
                self.has_generated_profile = true;
                self.json_dump_state = JsonDumpState::Inactive;
            }
        } else {
            self.is_active = false;
            self.buffers[self.profile_index].entries.clear();
            self.json_dump_state = JsonDumpState::Inactive;
        }
    }

    /// `watcherEnabled` (`profiler.hpp`, registered at
    /// `profiler.cpp:896-899`): pending or active.
    pub fn watcher_enabled(&self) -> bool {
        self.is_active || self.is_pending_active
    }

    /// The live frame's records — `profileEntryBuffer_`.
    pub fn profile_entries(&self) -> &[ProfileEntry] {
        &self.buffers[self.profile_index].entries
    }

    /// The handed-over frame's records — `finalProfileEntryBuffer_`:
    /// what the consumer (`processEntries`, BW-23) reads.
    pub fn final_entries(&self) -> &[ProfileEntry] {
        &self.buffers[self.final_index].entries
    }

    /// `isActive_`.
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// `isPendingActive_`.
    pub fn is_pending_active(&self) -> bool {
        self.is_pending_active
    }

    /// `hasGeneratedProfile_` — the dump machine's precondition
    /// (`profiler.cpp:783`), set by the first enable.
    pub fn has_generated_profile(&self) -> bool {
        self.has_generated_profile
    }

    /// `profileFrameNumber_`.
    pub fn profile_frame_number(&self) -> i32 {
        self.profile_frame_number
    }

    /// `frameStartTime_`.
    pub fn frame_start_time(&self) -> u64 {
        self.frame_start_time
    }

    /// `maxEntryCount_`.
    pub fn max_entry_count(&self) -> usize {
        self.max_entry_count
    }

    /// `highWatermarkOfEntryCount_`.
    pub fn high_watermark_of_entry_count(&self) -> usize {
        self.high_watermark_of_entry_count
    }

    /// `numThreads_`.
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }

    /// The pending deferred removals — the deferred-removal state
    /// `removeThread` builds for the consumer.
    pub fn threads_to_remove(&self) -> &BTreeSet<u32> {
        &self.threads_to_remove
    }

    /// `getJsonDumpState()` — the field's read face; the transitions
    /// beyond the enable resets are BW-12-03-03's.
    pub fn json_dump_state(&self) -> JsonDumpState {
        self.json_dump_state
    }

    /// `watcherSetDumpProfile` (`profiler.hpp:375-379`): arm (or
    /// disarm) the per-frame JSON dump; the index restarts so the next
    /// dump opens a fresh file.
    pub fn watcher_set_dump_profile(&mut self, new_value: bool) {
        self.json_dump_index = 0;
        self.do_json_dump = new_value;
    }

    /// `watcherGetDumpProfile` (`profiler.hpp:381-383`).
    pub fn watcher_get_dump_profile(&self) -> bool {
        self.do_json_dump
    }

    /// `setJsonDumpCount` (`profiler.hpp:367-370`): the frames per
    /// file — multiple frames share one file through this.
    pub fn set_json_dump_count(&mut self, count: i32) {
        self.json_dump_count = count;
    }

    /// `setJsonDumpDir` (`profiler.cpp:1282-1288`): the reference
    /// guards the write with `jsonDumpDirMutex_`; the port's global
    /// lock covers it.
    pub fn set_json_dump_dir(&mut self, json_dump_dir: &str) {
        self.json_dump_dir = json_dump_dir.to_string();
    }

    /// `getJsonOutputFilePath` (`profiler.hpp:417-418`): the latest
    /// dump file's path, empty before the first successful create.
    pub fn get_json_output_file_path(&self) -> &str {
        &self.json_output_file_path
    }

    /// `jsonDump` (`profiler.cpp:1198-1261`): write one frame's
    /// records into the open file. The first frame opens the file —
    /// closing any stale one first (`profiler.cpp:1207-1213`) — and
    /// takes the file's `ts` origin from the handed-over buffer's
    /// first entry (`profiler.cpp:1215`; the reference reads
    /// `entries_[0].time_` off the eagerly allocated array, the port's
    /// on-demand buffer reads 0 when the frame is empty). A failed
    /// create fails the dump; the last frame of the count closes it.
    fn json_dump(&mut self) {
        // `PROFILER_SCOPED( jsonDump )` (`profiler.cpp:1200`): the
        // records land in the collecting buffer, the same way.
        self.add_entry("jsonDump", EventType::Start, 0, EventCategory::Cpp);
        self.json_dump_index += 1;

        if self.json_dump_index == 1 {
            // `PROFILER_SCOPED( jsonDump_open )` (`profiler.cpp:1207`).
            self.add_entry("jsonDump_open", EventType::Start, 0, EventCategory::Cpp);
            if let Some(mut stale) = self.json_output_file.take() {
                let _ = std::io::Write::write_all(&mut stale, b"]");
            }

            self.json_dump_start_time = self.buffers[self.final_index]
                .entries
                .first()
                .map(|entry| entry.time)
                .unwrap_or(0);

            self.json_output_file = self.create_new_json_dump_file();
            if self.json_output_file.is_none() {
                self.json_dump_index = 0;
                self.do_json_dump = false;
                self.set_json_dump_state(JsonDumpState::Failed);
                self.add_entry("jsonDump_open", EventType::End, 0, EventCategory::Cpp);
                self.add_entry("jsonDump", EventType::End, 0, EventCategory::Cpp);
                return;
            }

            self.set_json_dump_state(JsonDumpState::Active);
            self.json_file_has_data = false;
            if let Some(file) = self.json_output_file.as_mut() {
                let _ = std::io::Write::write_all(file, b"[");
            }
            self.add_entry("jsonDump_open", EventType::End, 0, EventCategory::Cpp);
        }

        self.write_json_entries();
        if let Some(file) = self.json_output_file.as_mut() {
            let _ = std::io::Write::flush(file);
        }

        if self.json_dump_index == self.json_dump_count {
            self.stop_json_dump();
        }
        self.add_entry("jsonDump", EventType::End, 0, EventCategory::Cpp);
    }

    /// `stopJsonDump` (`profiler.cpp:1268-1279`): close the file and
    /// mark the dump complete.
    fn stop_json_dump(&mut self) {
        // `PROFILER_SCOPED( jsonDump_close )` (`profiler.cpp:1270`).
        self.add_entry("jsonDump_close", EventType::Start, 0, EventCategory::Cpp);
        if let Some(mut file) = self.json_output_file.take() {
            let _ = std::io::Write::write_all(&mut file, b"]");
        }
        self.json_dump_index = 0;
        self.do_json_dump = false;
        self.set_json_dump_state(JsonDumpState::Completed);
        self.add_entry("jsonDump_close", EventType::End, 0, EventCategory::Cpp);
    }

    /// `setJsonDumpState` (`profiler.cpp:1264-1267`).
    fn set_json_dump_state(&mut self, json_dump_state: JsonDumpState) {
        self.json_dump_state = json_dump_state;
    }

    /// `createNewJsonDumpFile` (`profiler.cpp:1296-1337`):
    /// `<dir>/<exe>_profile_YYYYMMDD_HHMMSS.json` in local time — the
    /// reference pins the schema because WebConsole parses it
    /// (`profiler.cpp:1309-1310`). A failed create answers `None`, the
    /// reference's NULL.
    fn create_new_json_dump_file(&mut self) -> Option<std::fs::File> {
        let exe = std::env::current_exe().ok()?;
        let basename = exe
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stamp = local_stamp();
        let file_name = format!("{basename}_profile_{stamp}.json");
        let path = if self.json_dump_dir.is_empty() {
            file_name
        } else {
            format!("{}/{file_name}", self.json_dump_dir)
        };
        let file = std::fs::File::create(&path).ok()?;
        self.json_output_file_path = path;
        Some(file)
    }

    /// `writeJsonEntries` (`profiler.cpp:1390-1465`): the Chrome trace
    /// event format — one `thread_name` metadata event per live thread
    /// slot, then one event per record, `B`/`E`/`C` phases, category
    /// names the way `eventCategories_` spells them (`profiler.cpp:951-959`:
    /// `C++`, `Python`, `GPU`, `All`), timestamps in micros from the
    /// file origin. The reference keeps a per-thread entry stack to
    /// detect unclosed scopes; nothing it computes reaches the output,
    /// so the port leaves it out.
    fn write_json_entries(&mut self) {
        let mut out = String::new();

        for (tid, slot) in self.threads.iter().enumerate().take(self.max_num_threads) {
            if slot.thread != 0 {
                if self.json_file_has_data {
                    out.push(',');
                }
                out.push_str(&format!(
                    "{{\"pid\":0,\"tid\":{tid}, \"ph\": \"M\", \
                     \"name\":\"thread_name\",\"args\":{{\"name\": \"{}\"}}}} ",
                    slot.name
                ));
                self.json_file_has_data = true;
            }
        }

        for entry in &self.buffers[self.final_index].entries {
            if entry.thread_idx == -1 {
                continue;
            }
            if self.json_file_has_data {
                out.push(',');
            }
            let category = match entry.category {
                EventCategory::Cpp => "C++",
                EventCategory::Gpu => "GPU",
                EventCategory::Python => "Python",
                EventCategory::All => "All",
            };
            // `(int)(stampsToSeconds( entry->time_ - jsonDumpStartTime_ )
            // * 1000000)` (`profiler.cpp:1428-1430`): the two's
            // complement subtraction wraps the same way.
            let ts = (stamps_to_seconds(entry.time.wrapping_sub(self.json_dump_start_time))
                * 1_000_000.0) as i32;
            let phase = match entry.event {
                EventType::Start | EventType::StartIdle => "\"B\"",
                EventType::End => "\"E\"",
                EventType::Counter => "\"C\"",
            };
            out.push_str(&format!(
                "{{ \"cat\":\"{category}\", \"pid\":0,\"tid\":{}, \
                 \"ts\":{ts},\"ph\":{phase},\"name\":\"{}\"",
                entry.thread_idx,
                entry.name()
            ));
            if entry.event == EventType::Counter {
                out.push_str(&format!(",\"args\":{{\"counter\": {}}}}}", entry.value));
            } else {
                out.push('}');
            }
            self.json_file_has_data = true;
        }

        if let Some(file) = self.json_output_file.as_mut() {
            let _ = std::io::Write::write_all(file, out.as_bytes());
        }
    }
}

/// `%04d%02d%02d_%02d%02d%02d` over the local clock
/// (`profiler.cpp:1305-1315`): `time(NULL)` through `localtime`.
fn local_stamp() -> String {
    // SAFETY: `libc::time` needs no argument; the null pointer asks for
    // the return value form.
    let time_t = unsafe { libc::time(std::ptr::null_mut()) };
    // SAFETY: `tm` is a plain C struct; zeroed is its "empty" value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` fills the given `tm` from the clock value;
    // it is the thread-safe form of the reference's `localtime`.
    unsafe {
        libc::localtime_r(&time_t, &mut tm);
    }
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        1900 + tm.tm_year,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

impl Default for Profiler {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// `tls_ThreadIndex_`: the calling thread's slot, 0 before any
    /// registration (`getThreadIndex`, `profiler.cpp:2521-2527`).
    static THREAD_INDEX: Cell<i32> = const { Cell::new(0) };
    /// This thread's minted `OurThreadID()` token.
    static THREAD_ID: Cell<Option<u32>> = const { Cell::new(None) };
}

/// `Profiler::getThreadIndex` (`profiler.cpp:2521-2527`): the tls slot.
fn thread_index() -> i32 {
    THREAD_INDEX.with(Cell::get)
}

/// The tls write of `addThisThread` (`profiler.cpp:2268, 2297`).
fn set_thread_index(index: i32) {
    THREAD_INDEX.with(|slot| slot.set(index));
}

static NEXT_THREAD_ID: AtomicU32 = AtomicU32::new(1);

/// `OurThreadID()` (`concurrency.hpp:190`): a nonzero token unique to
/// the calling thread, the id the thread table keys on. The reference
/// reads the OS thread id; the port mints one from a process counter —
/// the same role without a libc dependency, and never 0
/// (`addThisThread`'s `MF_ASSERT`, `profiler.cpp:2253`).
pub fn our_thread_id() -> u32 {
    THREAD_ID.with(|slot| match slot.get() {
        Some(id) => id,
        None => {
            let id = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
            slot.set(Some(id));
            id
        }
    })
}

static G_PROFILER: OnceLock<Mutex<Profiler>> = OnceLock::new();

/// `g_profiler` (`profiler.hpp`): the process singleton every face
/// drives — the module methods, the trace callback, the scoped guards.
/// The reference's producers append lock-free through atomics
/// (`profiler.ipp:67-85`); the port serialises appends through this
/// mutex, the same observable behavior without the lock-free guarantee.
pub fn g_profiler() -> &'static Mutex<Profiler> {
    G_PROFILER.get_or_init(|| Mutex::new(Profiler::new()))
}

/// The `profiler/enabled` watcher `init` registers
/// (`profiler.cpp:896-899`): `watcherEnabled` read as "1"/"0", written
/// the same way — the `istream >> bool` extraction of the reference's
/// stream-in path. It addresses the global instance the way the
/// reference's member watcher addresses `g_profiler`.
pub fn mount_enabled_watcher() -> bool {
    let node: Rc<RefCell<dyn Watcher>> = Rc::new(RefCell::new(EnabledWatcher));
    add_watcher("profiler/enabled", node).is_some()
}

/// The "1"/"0" leaf of `profiler/enabled`: the reference's
/// `Watcher::WT_READ_WRITE` member watcher over `watcherEnabled` /
/// `enable`.
struct EnabledWatcher;

impl Watcher for EnabledWatcher {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        if !path.is_empty() {
            return false;
        }
        let enabled = g_profiler()
            .lock()
            .expect("the profiler lock")
            .watcher_enabled();
        *result = if enabled {
            "1".to_string()
        } else {
            "0".to_string()
        };
        *desc = "Dump the profiling data for this frame".to_string();
        *mode = Mode::ReadWrite;
        true
    }

    fn set_from_string(&mut self, path: &str, value: &str) -> bool {
        if !path.is_empty() {
            return false;
        }
        match value.trim() {
            "1" => g_profiler().lock().expect("the profiler lock").enable(true),
            "0" => g_profiler()
                .lock()
                .expect("the profiler lock")
                .enable(false),
            _ => return false,
        }
        true
    }

    fn comment(&self) -> &str {
        "Dump the profiling data for this frame"
    }

    fn set_comment(&mut self, _comment: String) {}
}

/// `ScopedProfiler` (`profiler.hpp:643-657`): the RAII producer of
/// `PROFILER_SCOPED` — a start record on construction, the end record
/// on scope exit, addressed to the global the way the reference's is.
pub struct ScopedProfiler {
    text: String,
}

impl ScopedProfiler {
    /// The constructor's `addEntry(text, EVENT_START, 0)` with the
    /// `CATEGORY_CPP` default (`profiler.ipp:25`).
    pub fn new(text: &str) -> Self {
        g_profiler().lock().expect("the profiler lock").add_entry(
            text,
            EventType::Start,
            0,
            EventCategory::Cpp,
        );
        Self {
            text: text.to_string(),
        }
    }
}

impl Drop for ScopedProfiler {
    fn drop(&mut self) {
        g_profiler().lock().expect("the profiler lock").add_entry(
            &self.text,
            EventType::End,
            0,
            EventCategory::Cpp,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The g_profiler-driven tests serialise on this lock: two tests
    /// toggling the same process singleton would otherwise race.
    static G_PROFILER_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// The reference's capacity formula
    /// (`profiler.cpp:986-988`): the budget over two entry buffers plus a
    /// half-size display allowance. The entry size is the port's
    /// `repr(C)` mirror, so the same constants must land on the same
    /// capacities — 336 bytes buy one entry, the module's 128MB budget
    /// buys 399456.
    #[test]
    fn the_reference_sizes_drive_the_capacity_formula() {
        assert_eq!(std::mem::size_of::<ProfileEntry>(), 128);
        let mut profiler = Profiler::new();
        profiler.init(336, MAXIMUM_NUMBER_OF_THREADS);
        assert_eq!(profiler.max_entry_count(), 1);
        profiler.init(672, MAXIMUM_NUMBER_OF_THREADS);
        assert_eq!(profiler.max_entry_count(), 2);
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        assert_eq!(profiler.max_entry_count(), 399_457);
    }

    /// The un-inited profiler is inert (`profiler.cpp:2253-2256` guard,
    /// `:1111-1114` the tick early return, `addEntry`'s `isActive_` gate):
    /// no thread registers, no frame starts, no record lands.
    #[test]
    fn the_un_inited_profiler_is_inert() {
        let mut profiler = Profiler::new();
        profiler.add_this_thread("Main");
        assert_eq!(profiler.num_threads(), 0);
        profiler.tick();
        assert_eq!(profiler.profile_frame_number(), 0);
        profiler.add_entry("work", EventType::Start, 0, EventCategory::Cpp);
        assert!(profiler.profile_entries().is_empty());
        assert!(!profiler.watcher_enabled());
    }

    /// `enable(true)` arms at the next tick (`profiler.cpp:2545-2552`):
    /// pending is watcher-visible immediately, the records only flow
    /// once tick activated the profiler.
    #[test]
    fn enable_turns_on_at_the_next_tick() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        assert!(!profiler.watcher_enabled());

        profiler.enable(true);
        assert!(profiler.is_pending_active());
        assert!(!profiler.is_active());
        assert!(profiler.watcher_enabled());
        assert!(profiler.has_generated_profile());
        profiler.add_entry("early", EventType::Start, 0, EventCategory::Cpp);
        assert!(profiler.profile_entries().is_empty(), "not yet active");

        profiler.tick();
        assert!(profiler.is_active());
        assert!(!profiler.is_pending_active());
        profiler.add_entry("work", EventType::Start, 0, EventCategory::Python);
        assert_eq!(profiler.profile_entries().len(), 1);
    }

    /// `enable(true)` on an active profiler is a no-op
    /// (`profiler.cpp:2543` the `if (!isActive_)` guard): no pending
    /// state, the collected records stay.
    #[test]
    fn enabling_an_active_profiler_changes_nothing() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("work", EventType::Start, 0, EventCategory::Cpp);

        profiler.enable(true);
        assert!(!profiler.is_pending_active());
        assert!(profiler.is_active());
        assert_eq!(profiler.profile_entries().len(), 1);
    }

    /// `enable(false)` turns off now and drops the current frame's data
    /// (`profiler.cpp:2554-2560`); the next enable collects fresh, the
    /// frame number starting over because the swap path never ran.
    #[test]
    fn disable_drops_the_current_data() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("work", EventType::Start, 0, EventCategory::Cpp);
        assert_eq!(profiler.profile_entries().len(), 1);

        profiler.enable(false);
        assert!(!profiler.watcher_enabled());
        assert!(profiler.profile_entries().is_empty(), "the data dropped");

        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("again", EventType::Start, 0, EventCategory::Cpp);
        let entry = &profiler.profile_entries()[0];
        assert_eq!(entry.name(), "again");
        assert_eq!(entry.frame_number, 0, "no frame completed meanwhile");
    }

    /// `tick` hands the finished frame over (`profiler.cpp:1128-1146`):
    /// the swap moves the records into the final buffer, bumps the frame
    /// number, stamps the buffer and keeps the high watermark — and a
    /// tick with no records swaps nothing, so the handed-over frame
    /// stays readable.
    #[test]
    fn tick_swaps_the_finished_frame_into_the_final_buffer() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.enable(true);
        profiler.tick();

        for name in ["alpha", "beta", "gamma"] {
            profiler.add_entry(name, EventType::Start, 0, EventCategory::Cpp);
        }
        let before = stamp_now();
        profiler.tick();
        let after = stamp_now();

        assert_eq!(profiler.profile_frame_number(), 1);
        assert!(
            profiler.profile_entries().is_empty(),
            "the new frame starts empty"
        );
        let final_entries = profiler.final_entries();
        assert_eq!(final_entries.len(), 3);
        assert_eq!(final_entries[0].name(), "alpha");
        // The records carry the frame they were collected in — the first
        // active frame is number 0; the tick that hands it over bumps
        // the counter for the next frame (`profiler.ipp:48`,
        // `profiler.cpp:1136`).
        assert_eq!(final_entries[0].frame_number, 0);
        assert!(
            (before..=after)
                .contains(&profiler.buffers[profiler.final_index].end_of_frame_time_stamp),
            "the handed-over buffer is stamped at the tick"
        );
        assert_eq!(profiler.high_watermark_of_entry_count(), 3);

        profiler.tick();
        assert_eq!(
            profiler.profile_frame_number(),
            1,
            "no swap without records"
        );
        assert_eq!(profiler.final_entries().len(), 3);
    }

    /// One record carries its event, category, value and stamp
    /// (`profiler.ipp:25-51`), and the un-registered thread's slot is
    /// the tls default 0.
    #[test]
    fn entries_carry_their_event_fields() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("frame", EventType::Counter, 42, EventCategory::Gpu);

        let entry = &profiler.profile_entries()[0];
        assert_eq!(entry.name(), "frame");
        assert_eq!(entry.event, EventType::Counter);
        assert_eq!(entry.category, EventCategory::Gpu);
        assert_eq!(entry.value, 42);
        assert_eq!(entry.thread_idx, 0);
        assert!(entry.time > 0, "the record is stamped");
    }

    /// A name longer than the fixed storage is truncated at 95 bytes
    /// (`profiler.ipp:30-31`).
    #[test]
    fn a_long_name_is_truncated_at_the_fixed_storage() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.enable(true);
        profiler.tick();
        profiler.add_entry(&"x".repeat(120), EventType::Start, 0, EventCategory::Cpp);
        assert_eq!(profiler.profile_entries()[0].name().len(), 95);
    }

    /// A full buffer pins: `startNextEntry`'s clamp
    /// (`profiler.ipp:69-85`) — the overflow records are dropped, the
    /// count stays at the capacity.
    #[test]
    fn a_full_buffer_pins_at_the_capacity() {
        let mut profiler = Profiler::new();
        profiler.init(672, MAXIMUM_NUMBER_OF_THREADS);
        assert_eq!(profiler.max_entry_count(), 2);
        profiler.enable(true);
        profiler.tick();
        for _ in 0..4 {
            profiler.add_entry("work", EventType::Start, 0, EventCategory::Cpp);
        }
        assert_eq!(profiler.profile_entries().len(), 2);
    }

    /// `addThisThread` fills the first empty slot and the tls index its
    /// records carry (`profiler.cpp:2278-2301`); a re-add of the same
    /// thread keeps the one slot (`profiler.cpp:2258-2267`).
    #[test]
    fn a_thread_registers_into_the_first_empty_slot() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.add_this_thread("Main");

        assert_eq!(profiler.num_threads(), 1);
        assert_eq!(profiler.get_thread_name(0), Some("Main"));

        profiler.add_this_thread("Main again");
        assert_eq!(profiler.num_threads(), 1, "the re-add keeps the one slot");
        assert_eq!(profiler.get_thread_name(0), Some("Main"));

        profiler.enable(true);
        profiler.tick();
        profiler.add_entry("work", EventType::Start, 0, EventCategory::Cpp);
        assert_eq!(profiler.profile_entries()[0].thread_idx, 0);
    }

    /// `removeThread` while the profiler is off empties the slot at once
    /// (`profiler.cpp:2460-2466`): the slot reads deleted — no name, no
    /// id, not visible.
    #[test]
    fn a_removed_thread_is_deleted_immediately_when_inactive() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.add_this_thread("Main");
        assert_eq!(profiler.num_threads(), 1);

        profiler.remove_thread(our_thread_id());
        assert_eq!(profiler.num_threads(), 0);
        assert_eq!(profiler.get_thread_name(0), None);
        assert_eq!(profiler.threads[0].name, "Deleted");
        assert!(!profiler.threads[0].visible);
    }

    /// While the profiler runs the removal is deferred
    /// (`profiler.cpp:2468-2472`): the thread stays registered until the
    /// consumer's `doRemoveThreads` sweep — the reference's sweep site is
    /// `processEntries` (`profiler.cpp:1784`, BW-23).
    #[test]
    fn an_active_profiler_defers_removal_to_the_consumer() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.enable(true);
        profiler.tick();
        profiler.add_this_thread("Main");

        profiler.remove_thread(our_thread_id());
        assert_eq!(
            profiler.get_thread_name(0),
            Some("Main"),
            "still registered"
        );
        assert!(profiler.threads_to_remove().contains(&our_thread_id()));

        profiler.do_remove_threads();
        assert_eq!(profiler.get_thread_name(0), None);
        assert!(profiler.threads_to_remove().is_empty());
    }

    /// The thread table fills to its bound and refuses the overflow
    /// (`profiler.cpp:2270-2276`).
    #[test]
    fn the_thread_table_fills_and_refuses_overflow() {
        let shared = Arc::new(Mutex::new(Profiler::new()));
        shared
            .lock()
            .expect("the test lock")
            .init(128 * 1024 * 1024, 2);

        let mut workers = Vec::new();
        for index in 0..3 {
            let shared = Arc::clone(&shared);
            workers.push(std::thread::spawn(move || {
                let mut profiler = shared.lock().expect("the test lock");
                profiler.add_this_thread(&format!("worker-{index}"));
                (index, our_thread_id())
            }));
        }
        let ids: Vec<u32> = workers
            .into_iter()
            .map(|worker| worker.join().expect("the worker").1)
            .collect();

        let profiler = shared.lock().expect("the test lock");
        assert_eq!(profiler.num_threads(), 2, "the third registration refused");
        assert!(ids.iter().all(|id| *id != 0), "the ids are nonzero");
        assert_eq!(ids.len(), 3);
        let first_two: std::collections::HashSet<u32> = ids.iter().copied().take(2).collect();
        assert_eq!(first_two.len(), 2, "the registered ids are distinct");
    }

    /// Records carry the registering thread's slot, whichever thread
    /// produced them (`profiler.ipp:44`), and the ids stay distinct
    /// per thread.
    #[test]
    fn entries_record_their_threads_slot() {
        let shared = Arc::new(Mutex::new(Profiler::new()));
        {
            let mut profiler = shared.lock().expect("the test lock");
            profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
            profiler.enable(true);
            profiler.tick();
            profiler.add_this_thread("Main");
        }

        let worker = Arc::clone(&shared);
        let joined = std::thread::spawn(move || {
            let mut profiler = worker.lock().expect("the test lock");
            profiler.add_this_thread("Worker");
            profiler.add_entry("worker-work", EventType::Start, 0, EventCategory::Cpp);
        });
        joined.join().expect("the worker");

        let mut profiler = shared.lock().expect("the test lock");
        profiler.add_entry("main-work", EventType::Start, 0, EventCategory::Cpp);
        let slots: Vec<i32> = profiler
            .profile_entries()
            .iter()
            .map(|entry| entry.thread_idx)
            .collect();
        assert_eq!(slots, vec![1, 0], "the worker took the second slot");
        assert_eq!(profiler.get_thread_name(0), Some("Main"));
        assert_eq!(profiler.get_thread_name(1), Some("Worker"));
    }

    /// The `profiler/enabled` watcher: "1"/"0" over `watcherEnabled`,
    /// the write the `enable` face (`profiler.cpp:896-899`), and a
    /// non-boolean write refused the way the reference's stream
    /// extraction fails.
    #[test]
    fn the_enabled_watcher_reads_and_writes_the_state() {
        let guard = G_PROFILER_TEST_LOCK.lock().expect("the test lock");
        g_profiler()
            .lock()
            .expect("the lock")
            .init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        assert!(mount_enabled_watcher());

        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert_eq!(
            watcher::get_watcher_string("profiler/enabled", &mut desc, &mut mode)
                .expect("the watcher mounted"),
            "0"
        );
        assert_eq!(mode, Mode::ReadWrite);
        assert_eq!(desc, "Dump the profiling data for this frame");

        assert!(watcher::set_watcher_string("profiler/enabled", "1"));
        assert!(g_profiler().lock().expect("the lock").watcher_enabled());
        assert_eq!(
            watcher::get_watcher_string("profiler/enabled", &mut desc, &mut mode).unwrap(),
            "1"
        );

        // A tick takes the pending arm to active — the only state an off
        // write can clear (`enable(false)` clears `isActive_` alone,
        // `profiler.cpp:2554-2560`).
        g_profiler().lock().expect("the lock").tick();
        assert!(watcher::set_watcher_string("profiler/enabled", "0"));
        assert!(!g_profiler().lock().expect("the lock").watcher_enabled());
        assert_eq!(
            watcher::get_watcher_string("profiler/enabled", &mut desc, &mut mode).unwrap(),
            "0"
        );

        assert!(!watcher::set_watcher_string("profiler/enabled", "yes"));

        // A sub-path misses the "1"/"0" leaf: the guards answer false the
        // way a watcher-tree miss does.
        assert!(
            watcher::get_watcher_string("profiler/enabled/foo", &mut desc, &mut mode).is_none()
        );
        assert!(!watcher::set_watcher_string("profiler/enabled/foo", "1"));
        drop(guard);
    }

    /// The enabled watcher's leaf face driven straight: a sub-path
    /// misses both directions and the comment is the reference's
    /// `MF_WATCH` description (`profiler.cpp:896-899`).
    #[test]
    fn the_enabled_watcher_leaf_rejects_a_sub_path() {
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(!EnabledWatcher.get_as_string("child", &mut result, &mut desc, &mut mode));
        assert!(!EnabledWatcher.set_from_string("child", "1"));
        assert_eq!(
            EnabledWatcher.comment(),
            "Dump the profiling data for this frame"
        );
        EnabledWatcher.set_comment("ignored".to_string());
    }

    /// The sweeps return at once on a never-inited table (the guard
    /// `profiler.cpp:2448-2451` reads), and the read faces expose the
    /// reset defaults: the frame start moves only when a tick stamps
    /// one, the dump state stays `INACTIVE` through the enable resets.
    #[test]
    fn the_sweeps_and_reads_hold_on_a_fresh_table() {
        let mut profiler = Profiler::default();
        profiler.remove_thread(1);
        profiler.do_remove_threads();

        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        // An id no slot holds walks the table out — the reference's
        // loop simply doesn't find it (`profiler.cpp:2477-2503`).
        profiler.remove_thread(999);
        assert_eq!(profiler.num_threads(), 0);
        assert_eq!(profiler.frame_start_time(), 0);
        assert_eq!(profiler.json_dump_state(), JsonDumpState::Inactive);
        let before = stamp_now();
        profiler.enable(true);
        profiler.tick();
        assert!(
            profiler.frame_start_time() >= before,
            "the tick stamps the frame start"
        );
        assert_eq!(profiler.json_dump_state(), JsonDumpState::Inactive);
    }

    /// `ScopedProfiler` produces its start record on construction and
    /// the end record on scope exit, on the global the module faces
    /// share (`profiler.hpp:643-657`).
    #[test]
    fn the_scoped_profiler_produces_its_pair() {
        let _guard = G_PROFILER_TEST_LOCK.lock().expect("the test lock");
        let mut profiler = g_profiler().lock().expect("the lock");
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.enable(true);
        profiler.tick();
        drop(profiler);

        {
            let _scoped = ScopedProfiler::new("frame-run");
        }

        let profiler = g_profiler().lock().expect("the lock");
        assert_eq!(profiler.profile_entries().len(), 2);
        assert_eq!(profiler.profile_entries()[0].name(), "frame-run");
        assert_eq!(profiler.profile_entries()[0].event, EventType::Start);
        assert_eq!(profiler.profile_entries()[0].category, EventCategory::Cpp);
        assert_eq!(profiler.profile_entries()[1].event, EventType::End);
    }

    /// A scratch dump directory for one test.
    fn dump_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "boundless-profile-json-{tag}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("build the dump dir");
        dir
    }

    /// The JSON dump of one fixed frame (`profiler.cpp:1198-1465`):
    /// the file is the Chrome trace event format — `[`, one
    /// `thread_name` metadata event per live slot, one event per
    /// record with the reference's exact spacing, `]` at the close.
    /// The first record pins the `ts` origin, so its delta reads 0;
    /// the rest carry the real micro deltas the same formula
    /// (`profiler.cpp:1428-1430`) renders.
    #[test]
    fn the_json_dump_writes_the_fixed_event_product() {
        let dir = dump_dir("single");
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.set_json_dump_dir(dir.to_str().expect("utf8 temp dir"));
        profiler.add_this_thread("MainThread");
        profiler.enable(true);
        profiler.tick();
        profiler.watcher_set_dump_profile(true);

        profiler.add_entry("busy", EventType::Start, 0, EventCategory::Python);
        profiler.add_entry("busy", EventType::End, 0, EventCategory::Python);
        profiler.tick();

        assert_eq!(profiler.json_dump_state(), JsonDumpState::Completed);
        assert!(!profiler.watcher_get_dump_profile(), "the dump rearmed off");
        let path = profiler.get_json_output_file_path().to_string();
        assert!(
            path.starts_with(dir.to_str().expect("utf8 temp dir")),
            "{path}"
        );
        let name = path.rsplit('/').next().expect("a file name");
        let binary = std::env::current_exe()
            .expect("the test binary")
            .file_name()
            .expect("a binary name")
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with(&binary) && name.contains("_profile_") && name.ends_with(".json"),
            "{name} carries the reference's schema"
        );

        let product = std::fs::read_to_string(&path).expect("the dump file");
        let prefix = "[{\"pid\":0,\"tid\":0, \"ph\": \"M\", \
                      \"name\":\"thread_name\",\"args\":{\"name\": \"MainThread\"}} \
                      ,{ \"cat\":\"Python\", \"pid\":0,\"tid\":0, \
                      \"ts\":0,\"ph\":\"B\",\"name\":\"busy\"}";
        assert!(
            product.starts_with(prefix),
            "the file opens with the metadata and the zero-delta start:\n{product}"
        );
        assert!(
            product.ends_with("\"ph\":\"E\",\"name\":\"busy\"}]"),
            "the file closes on the end record and the bracket:\n{product}"
        );

        // The dump's own scoped records color the collecting buffer the
        // way the reference's `PROFILER_SCOPED`s do
        // (`profiler.cpp:1200,1207,1270`).
        let scopes: Vec<_> = profiler
            .profile_entries()
            .iter()
            .map(|entry| (entry.name(), entry.event))
            .collect();
        assert_eq!(
            scopes,
            vec![
                ("jsonDump", EventType::Start),
                ("jsonDump_open", EventType::Start),
                ("jsonDump_open", EventType::End),
                ("jsonDump_close", EventType::Start),
                ("jsonDump_close", EventType::End),
                ("jsonDump", EventType::End),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Several frames share one file through `jsonDumpCount_`
    /// (`profiler.hpp:367-370`): the comma gate joins the frames'
    /// events, the metadata repeats per frame, and the last frame of
    /// the count closes the file
    /// (`profiler.cpp:1239-1241`).
    #[test]
    fn a_multi_frame_dump_shares_one_file() {
        let dir = dump_dir("multi");
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.set_json_dump_dir(dir.to_str().expect("utf8 temp dir"));
        profiler.add_this_thread("MainThread");
        profiler.enable(true);
        profiler.tick();
        profiler.watcher_set_dump_profile(true);
        profiler.set_json_dump_count(2);

        profiler.add_entry("first", EventType::Start, 0, EventCategory::Cpp);
        profiler.add_entry("first", EventType::End, 0, EventCategory::Cpp);
        profiler.tick();
        assert_eq!(profiler.json_dump_state(), JsonDumpState::Active);
        assert!(profiler.watcher_get_dump_profile(), "still dumping");

        profiler.add_entry("second", EventType::Counter, 7, EventCategory::Cpp);
        profiler.tick();
        assert_eq!(profiler.json_dump_state(), JsonDumpState::Completed);

        let product =
            std::fs::read_to_string(profiler.get_json_output_file_path()).expect("the dump file");
        assert!(product.starts_with('['));
        assert!(product.ends_with(']'));
        assert_eq!(
            product.matches("\"name\":\"thread_name\"").count(),
            2,
            "each frame writes its metadata:\n{product}"
        );
        assert!(product.contains("\"ph\":\"B\",\"name\":\"first\""));
        assert!(product.contains("\"ph\":\"E\",\"name\":\"first\""));
        assert!(
            product.contains("\"ph\":\"C\",\"name\":\"second\",\"args\":{\"counter\": 7}}"),
            "the counter phase carries the value:\n{product}"
        );
        assert!(
            product.contains(",{\"pid\":0,\"tid\":0, \"ph\": \"M\""),
            "the second frame's metadata joins through the comma gate:\n{product}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A failed create fails the dump (`profiler.cpp:1217-1224`): the
    /// state goes `JSON_DUMPING_FAILED`, the arm clears and the index
    /// resets, so the next arm opens fresh.
    #[test]
    fn a_failed_create_fails_the_dump() {
        let dir = dump_dir("failed");
        let missing = dir.join("no-such-subdir");
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.set_json_dump_dir(missing.to_str().expect("utf8 temp dir"));
        profiler.enable(true);
        profiler.tick();
        profiler.watcher_set_dump_profile(true);
        profiler.tick();

        assert_eq!(profiler.json_dump_state(), JsonDumpState::Failed);
        assert!(!profiler.watcher_get_dump_profile());
        assert_eq!(profiler.get_json_output_file_path(), "");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An arm cleared between frames closes the open file through the
    /// consumer's other branch (`profiler.cpp:1489-1491`): the next
    /// tick stops the dump and the state completes.
    #[test]
    fn a_disarmed_dump_closes_the_open_file() {
        let dir = dump_dir("disarm");
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        profiler.set_json_dump_dir(dir.to_str().expect("utf8 temp dir"));
        profiler.add_this_thread("MainThread");
        profiler.enable(true);
        profiler.tick();
        profiler.watcher_set_dump_profile(true);
        profiler.set_json_dump_count(2);
        profiler.add_entry("work", EventType::Start, 0, EventCategory::Cpp);
        profiler.tick();
        assert_eq!(profiler.json_dump_state(), JsonDumpState::Active);

        profiler.watcher_set_dump_profile(false);
        profiler.tick();
        assert_eq!(profiler.json_dump_state(), JsonDumpState::Completed);
        let product =
            std::fs::read_to_string(profiler.get_json_output_file_path()).expect("the dump file");
        assert!(product.ends_with(']'));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `init` defaults the dump directory to the executable's
    /// (`profiler.cpp:1070`).
    #[test]
    fn the_dump_defaults_to_the_executable_directory() {
        let mut profiler = Profiler::new();
        profiler.init(128 * 1024 * 1024, MAXIMUM_NUMBER_OF_THREADS);
        let expected = std::env::current_exe()
            .expect("the test binary")
            .parent()
            .expect("a binary directory")
            .to_string_lossy()
            .into_owned();
        assert_eq!(profiler.json_dump_dir, expected);
    }
}
