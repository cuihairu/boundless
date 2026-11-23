//! BW-24-01: memory snapshot.
//!
//! A snapshot answers what the diagnostic's memory interface is asked for:
//! each process's resident set, the allocator's live allocation and
//! high-water readings, the entity and cache object counts, the Python object
//! census, and the metric table recording each reading's source, cost and
//! live-or-cumulative kind (BW-24-01-01..06).
//!
//! The resident-set reading (BW-24-01-01) is the one the platforms differ
//! on: the reference reports it from whatever the platform offers
//! (`memory_debug.cpp` `ProcessMemoryInfo` is Windows-only; the Linux and
//! macOS diagnostics read their own sources), and each reports in its own
//! native unit. A snapshot must compare processes against each other, so
//! every reading is unified into bytes before it leaves this module: Linux
//! `/proc/<pid>/status` reports kB, macOS `mach_task_basic_info` and Windows
//! `GetProcessMemoryInfo` report bytes, and a page-based source would be
//! scaled by the page size.

/// The unit a platform natively reports a resident-set reading in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RssUnit {
    /// Windows `GetProcessMemoryInfo` and macOS `mach_task_basic_info` report
    /// their resident set in bytes.
    Bytes,
    /// Linux `/proc/<pid>/status` reports `VmRSS:` in kB.
    Kilobytes,
    /// A page-based source (a BSD process table's resident page count) would
    /// report in pages; the contract normalizes that too, whatever the
    /// platform reads.
    Pages,
}

/// A resident-set reading as its platform reports it, before unification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RssReading {
    value: u64,
    unit: RssUnit,
}

impl RssReading {
    /// A reading the platform already reports in bytes (Windows).
    pub fn bytes(value: u64) -> Self {
        Self {
            value,
            unit: RssUnit::Bytes,
        }
    }

    /// A reading the platform reports in kB (Linux).
    pub fn kilobytes(value: u64) -> Self {
        Self {
            value,
            unit: RssUnit::Kilobytes,
        }
    }

    /// A reading the platform reports in pages (macOS).
    pub fn pages(value: u64) -> Self {
        Self {
            value,
            unit: RssUnit::Pages,
        }
    }

    /// The reading as reported, in its native unit.
    pub fn value(&self) -> u64 {
        self.value
    }

    /// The native unit the platform reported in.
    pub fn unit(&self) -> RssUnit {
        self.unit
    }

    /// The reading in bytes — the one unit every platform's output shares.
    /// Only a page-unit reading depends on `page_size`.
    pub fn to_bytes(&self, page_size: u64) -> u64 {
        match self.unit {
            RssUnit::Bytes => self.value,
            RssUnit::Kilobytes => self.value * 1024,
            RssUnit::Pages => self.value * page_size,
        }
    }
}

/// The system's memory page size, which scales page-unit readings. Windows
/// reports bytes directly and never produces a page-unit reading, so a
/// non-unix platform has no page size to ask for and the fixed x86-64 page
/// size stands in.
pub fn page_size() -> u64 {
    #[cfg(unix)]
    {
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if size > 0 {
            return size as u64;
        }
        4096
    }
    #[cfg(not(unix))]
    {
        4096
    }
}

/// Parse the resident-set line out of a `/proc/<pid>/status` dump. The
/// kernel reports `VmRSS:` in kB, so the reading is kilobytes; the virtual
/// (`VmSize`) and peak (`VmHWM`) lines are not resident memory and stay
/// ignored.
pub fn parse_proc_status_rss(status: &str) -> Option<RssReading> {
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        let digits = rest.trim_end_matches("kB").trim();
        if let Ok(kb) = digits.parse::<u64>() {
            return Some(RssReading::kilobytes(kb));
        }
    }
    None
}

/// A process's resident set in bytes — the unified unit on every platform,
/// whatever its source reports natively. Nothing is returned when the
/// platform source cannot be read (the process gone, or permission denied).
pub fn resident_bytes(pid: u32) -> Option<u64> {
    native_rss(pid).map(|reading| reading.to_bytes(page_size()))
}

/// The resident set of `pid` as its platform natively reports it.
#[cfg(target_os = "linux")]
fn native_rss(pid: u32) -> Option<RssReading> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_proc_status_rss(&status)
}

/// The resident set of `pid` as its platform natively reports it: the
/// current task's resident size in bytes. The task is reached through
/// `mach_task_self`, which names only this process — another process would
/// need a privileged `task_for_pid`, which a diagnostic cannot assume — so
/// only `pid == self` yields a reading. The reference's own
/// `ProcessMemoryInfo` is self-only too.
#[cfg(target_os = "macos")]
fn native_rss(pid: u32) -> Option<RssReading> {
    if pid != std::process::id() {
        return None;
    }
    let mut info: libc::mach_task_basic_info = unsafe { std::mem::zeroed() };
    let mut count: libc::mach_msg_type_number_t = libc::MACH_TASK_BASIC_INFO_COUNT;
    let rc = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut libc::mach_task_basic_info as *mut libc::c_void as libc::task_info_t,
            &mut count,
        )
    };
    if rc != libc::KERN_SUCCESS {
        return None;
    }
    // The struct is packed, so its fields may sit unaligned: read the
    // resident size through a pointer rather than a field reference.
    let resident = unsafe { std::ptr::addr_of!(info.resident_size).read_unaligned() };
    Some(RssReading::bytes(resident))
}

/// The resident set of `pid` as its platform natively reports it: the
/// process working set, already in bytes. Other processes need a query
/// handle; the current process reuses its own.
#[cfg(target_os = "windows")]
fn native_rss(pid: u32) -> Option<RssReading> {
    use std::ffi::c_void;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    let own = pid == std::process::id();
    let process = if own {
        unsafe { GetCurrentProcess() }
    } else {
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }
    };
    if process.is_null() {
        return None;
    }
    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        ..ProcessMemoryCounters::default()
    };
    let rc = unsafe { K32GetProcessMemoryInfo(process, &mut counters, counters.cb) };
    if !own {
        unsafe { CloseHandle(process) };
    }
    (rc != 0).then(|| RssReading::bytes(counters.working_set_size as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_platforms_report_in_one_unit() {
        // Windows: already bytes, the reading passes through.
        assert_eq!(RssReading::bytes(4096).to_bytes(4096), 4096);
        // Linux: kB scales by 1024, whatever the page size.
        assert_eq!(RssReading::kilobytes(2).to_bytes(4096), 2048);
        assert_eq!(RssReading::kilobytes(2).to_bytes(16384), 2048);
        // macOS: pages scale by the system's page size (4 KiB and the
        // 16 KiB pages Apple Silicon boots with).
        assert_eq!(RssReading::pages(3).to_bytes(4096), 12_288);
        assert_eq!(RssReading::pages(3).to_bytes(16_384), 49_152);

        // The native reading keeps what its platform said.
        let linux = RssReading::kilobytes(4711);
        assert_eq!(linux.unit(), RssUnit::Kilobytes);
        assert_eq!(linux.value(), 4711);
        assert_eq!(linux.to_bytes(4096), 4711 * 1024);
    }

    #[test]
    fn a_proc_status_fixture_parses_its_resident_line() {
        // A trimmed `/proc/self/status` around its memory lines: virtual and
        // peak lines must not be mistaken for resident memory.
        let status = "\
Name:\tcellapp
VmPeak:\t   2097152 kB
VmSize:\t   1048576 kB
VmHWM:\t     262144 kB
VmRSS:\t      4711 kB
Threads:\t9\n";
        let reading = parse_proc_status_rss(status).expect("VmRSS is parsed");
        assert_eq!(reading, RssReading::kilobytes(4711));

        // Without a resident line there is no reading.
        assert_eq!(
            parse_proc_status_rss("Name:\tcellapp\nVmSize:\t 128 kB\n"),
            None
        );
    }

    #[test]
    fn the_running_process_reports_its_own_resident_set() {
        let first = resident_bytes(std::process::id()).expect("own RSS is readable");
        assert!(first > 0, "a running test process is resident somewhere");
        // Sane for a test binary: resident, not the whole address space.
        assert!(first < 64 * 1024 * 1024 * 1024);
        // A second reading also comes back, in the same unified unit.
        let second = resident_bytes(std::process::id()).expect("own RSS stays readable");
        assert!(second > 0);

        // A process that cannot exist reports nothing rather than zero.
        assert_eq!(resident_bytes(u32::MAX), None);
    }
}

/// BW-24-01-02 / BW-24-01-03: the live allocation and high-water readings.
///
/// The tracker counts the bytes its fixture allocations hold and gives each
/// one back on release, so the reading is the **live** value — it moves with
/// every allocation and falls back when memory is returned, unlike a
/// cumulative total that only ever grows. The tracker also keeps the round's
/// **high-water mark**, the highest live reading the round reached: a
/// release lowers the live reading and leaves the mark alone, so a round's
/// peak survives every free. The reference instruments the real allocator
/// for these readings (`memory_debug.cpp` registers every `allocate` and
/// `deallocate`); the port has no allocator hook yet, so the diagnostic owns
/// its allocations through [`TrackedAllocation`] guards and accounts exactly
/// what it hands out.
use std::sync::atomic::{AtomicU64, Ordering};

/// One fixture allocation, live for as long as the guard is. Dropping it
/// releases the bytes back to the tracker, so the reading cannot drift.
pub struct TrackedAllocation<'tracker> {
    tracker: &'tracker LiveAllocationTracker,
    buffer: Vec<u8>,
}

impl TrackedAllocation<'_> {
    /// The allocation's bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.buffer
    }

    /// The allocation's bytes, mutable.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    /// How many bytes this allocation holds.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Whether the allocation is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Release the allocation now instead of at scope exit; the reading
    /// falls back immediately.
    pub fn release(self) {}
}

impl Drop for TrackedAllocation<'_> {
    fn drop(&mut self) {
        self.tracker
            .live_bytes
            .fetch_sub(self.buffer.len() as u64, Ordering::SeqCst);
    }
}

/// The live-byte accounting for a snapshot's allocation reading, together
/// with the round's high-water mark.
#[derive(Debug, Default)]
pub struct LiveAllocationTracker {
    live_bytes: AtomicU64,
    peak_bytes: AtomicU64,
    total_allocated: AtomicU64,
}

impl LiveAllocationTracker {
    /// A tracker with nothing live and no high-water mark yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes currently held by unreleased allocations.
    pub fn live_bytes(&self) -> u64 {
        self.live_bytes.load(Ordering::SeqCst)
    }

    /// The bytes allocated since the tracker began, cumulatively: it only
    /// ever grows, so it is the cumulative (累计) reading the live one is
    /// distinguished from.
    pub fn total_allocated_bytes(&self) -> u64 {
        self.total_allocated.load(Ordering::SeqCst)
    }

    /// The round's high-water mark: the highest live reading seen since the
    /// mark started. Releasing allocations lowers the live reading and
    /// leaves the mark untouched — a release is not a reclamation of the
    /// bytes that were once resident.
    pub fn peak_bytes(&self) -> u64 {
        self.peak_bytes.load(Ordering::SeqCst)
    }

    /// Begin a new round with its own high-water mark.
    pub fn reset_peak(&self) {
        self.peak_bytes.store(0, Ordering::SeqCst);
    }

    /// Whether the reading is back in the test's allowed range: at most
    /// `slack` bytes above the baseline (and never below it — the tracker
    /// never under-counts).
    pub fn is_within(&self, baseline: u64, slack: u64) -> bool {
        let live = self.live_bytes();
        live >= baseline && live <= baseline + slack
    }

    /// Allocate `bytes` and account them as live until the returned guard
    /// is dropped or released. The bytes are real, so a fixture can write
    /// through them while they are held.
    pub fn allocate(&self, bytes: usize) -> TrackedAllocation<'_> {
        let added = bytes as u64;
        let live_before = self.live_bytes.fetch_add(added, Ordering::SeqCst);
        self.total_allocated.fetch_add(added, Ordering::SeqCst);
        // Only allocations raise the mark: the live reading moves up here and
        // comes back down on release, and the mark ratchets.
        self.raise_peak(live_before + added);
        TrackedAllocation {
            tracker: self,
            buffer: vec![0_u8; bytes],
        }
    }

    /// Ratchet the mark up to `candidate`, losing races with other writers.
    fn raise_peak(&self, candidate: u64) {
        loop {
            let peak = self.peak_bytes.load(Ordering::SeqCst);
            if candidate <= peak {
                return;
            }
            if self
                .peak_bytes
                .compare_exchange_weak(peak, candidate, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return;
            }
        }
    }
}

#[cfg(test)]
mod live_allocation_tests {
    use super::*;

    #[test]
    fn a_fully_released_fixture_returns_to_the_allowed_range() {
        let tracker = LiveAllocationTracker::new();
        let baseline = tracker.live_bytes();

        // The fixture allocates three entity-table-sized blocks.
        let mut blocks = Vec::new();
        for _ in 0..3 {
            blocks.push(tracker.allocate(4096));
        }
        assert_eq!(tracker.live_bytes(), baseline + 3 * 4096);
        assert!(
            !tracker.is_within(baseline, 1024),
            "still holding the fixture"
        );

        // Releasing every block returns the reading to the allowed range.
        for block in blocks {
            block.release();
        }
        assert_eq!(tracker.live_bytes(), baseline);
        assert!(tracker.is_within(baseline, 1024), "back within the range");
    }

    #[test]
    fn a_partial_release_keeps_only_the_still_live_bytes() {
        let tracker = LiveAllocationTracker::new();
        let first = tracker.allocate(4096);
        let second = tracker.allocate(4096);
        let _third = tracker.allocate(4096);
        assert_eq!(tracker.live_bytes(), 3 * 4096);

        // Two of the three stay live: the reading only loses the released
        // one, it does not reset.
        drop(second);
        assert_eq!(tracker.live_bytes(), 2 * 4096);
        drop(first);
        assert_eq!(tracker.live_bytes(), 4096);
    }

    #[test]
    fn a_guard_leaving_scope_releases_its_own_bytes() {
        let tracker = LiveAllocationTracker::new();
        let baseline = tracker.live_bytes();

        {
            let mut block = tracker.allocate(2048);
            block.as_mut_slice().fill(0xA5);
            assert_eq!(block.len(), 2048);
            assert!(!block.is_empty());
            assert_eq!(block.as_slice()[0], 0xA5, "the bytes are real");
            assert_eq!(tracker.live_bytes(), baseline + 2048);
        }

        // The guard dropped at scope exit: the reading is back, with no
        // explicit release call.
        assert!(tracker.is_within(baseline, 0));
    }

    #[test]
    fn the_high_water_mark_keeps_the_round_peak_after_release() {
        let tracker = LiveAllocationTracker::new();
        let baseline = tracker.live_bytes();

        // A burst of allocations raises the live reading to its peak.
        let blocks: Vec<_> = (0..3).map(|_| tracker.allocate(4096)).collect();
        assert_eq!(tracker.live_bytes(), baseline + 3 * 4096);
        assert_eq!(tracker.peak_bytes(), baseline + 3 * 4096);

        // Every object is released; the live reading falls back but the
        // mark keeps this round's peak.
        for block in blocks {
            drop(block);
        }
        assert_eq!(tracker.live_bytes(), baseline);
        assert_eq!(
            tracker.peak_bytes(),
            baseline + 3 * 4096,
            "the mark keeps the round's peak"
        );
    }

    #[test]
    fn a_new_round_records_its_own_peak() {
        let tracker = LiveAllocationTracker::new();
        let first: Vec<_> = (0..3).map(|_| tracker.allocate(4096)).collect();
        assert_eq!(tracker.peak_bytes(), 3 * 4096);
        for block in first {
            drop(block);
        }

        // A new round starts with no mark; its own peak is smaller.
        tracker.reset_peak();
        assert_eq!(tracker.peak_bytes(), 0);
        let second: Vec<_> = (0..2).map(|_| tracker.allocate(2048)).collect();
        assert_eq!(tracker.live_bytes(), 2 * 2048);
        assert_eq!(tracker.peak_bytes(), 2 * 2048);
        for block in second {
            drop(block);
        }
        assert_eq!(tracker.live_bytes(), 0);
        assert_eq!(tracker.peak_bytes(), 2 * 2048);
    }

    #[test]
    fn the_mark_ratchets_up_never_down() {
        let tracker = LiveAllocationTracker::new();

        // A first spike to 4096.
        let one = tracker.allocate(4096);
        assert_eq!(tracker.peak_bytes(), 4096);
        drop(one);
        assert_eq!(tracker.live_bytes(), 0);
        assert_eq!(tracker.peak_bytes(), 4096);

        // A wider spike raises the mark; releasing back to the first
        // spike's level does not lower it.
        let mut two: Vec<_> = (0..2).map(|_| tracker.allocate(4096)).collect();
        assert_eq!(tracker.peak_bytes(), 8192);
        drop(two.pop().expect("one of the two"));
        assert_eq!(tracker.live_bytes(), 4096);
        assert_eq!(tracker.peak_bytes(), 8192);
    }
}

/// BW-24-01-04: the object counts.
///
/// A snapshot answers how many entities and how many cache objects the
/// process holds, per name: entities by their type name and cache objects by
/// their cache name. The counts are exact, so a scheduled load — spawns and
/// despawns plus cache fills and drops — must reconcile to the arithmetic
/// the diagnostic is checking against.
use std::collections::BTreeMap;

/// Entity and cache object counts, kept per name.
#[derive(Debug, Default)]
pub struct ObjectCounts {
    entities: BTreeMap<String, u64>,
    caches: BTreeMap<String, u64>,
}

impl ObjectCounts {
    /// Counts with no entities and no cache objects.
    pub fn new() -> Self {
        Self::default()
    }

    /// One more entity of the given type.
    pub fn add_entity(&mut self, type_name: &str) {
        *self.entities.entry(type_name.to_string()).or_insert(0) += 1;
    }

    /// One entity of the given type goes away. `false` when there was none to
    /// remove — a count never underflows to negative.
    pub fn remove_entity(&mut self, type_name: &str) -> bool {
        remove_one(&mut self.entities, type_name)
    }

    /// One more cache object from the given cache.
    pub fn add_cache(&mut self, name: &str) {
        *self.caches.entry(name.to_string()).or_insert(0) += 1;
    }

    /// One cache object leaves the given cache. `false` when there was none.
    pub fn remove_cache(&mut self, name: &str) -> bool {
        remove_one(&mut self.caches, name)
    }

    /// How many entities of the given type.
    pub fn entity_count(&self, type_name: &str) -> u64 {
        self.entities.get(type_name).copied().unwrap_or(0)
    }

    /// How many entities of every type.
    pub fn entity_total(&self) -> u64 {
        self.entities.values().sum()
    }

    /// The entity type names, ascending.
    pub fn entity_types(&self) -> Vec<&str> {
        self.entities.keys().map(String::as_str).collect()
    }

    /// How many objects the given cache holds.
    pub fn cache_count(&self, name: &str) -> u64 {
        self.caches.get(name).copied().unwrap_or(0)
    }

    /// How many objects every cache holds.
    pub fn cache_total(&self) -> u64 {
        self.caches.values().sum()
    }

    /// The cache names, ascending.
    pub fn cache_names(&self) -> Vec<&str> {
        self.caches.keys().map(String::as_str).collect()
    }
}

/// Decrement one count, returning whether anything was left to remove.
fn remove_one(counts: &mut BTreeMap<String, u64>, name: &str) -> bool {
    let Some(entry) = counts.get_mut(name) else {
        return false;
    };
    if *entry == 0 {
        return false;
    }
    *entry -= 1;
    if *entry == 0 {
        counts.remove(name);
    }
    true
}

#[cfg(test)]
mod object_counts_tests {
    use super::*;

    #[test]
    fn a_scheduled_load_reconciles_to_its_expected_counts() {
        let mut counts = ObjectCounts::new();

        // A scheduled load: spawns and cache fills.
        for _ in 0..10 {
            counts.add_entity("Avatar");
        }
        for _ in 0..3 {
            counts.add_entity("Space");
        }
        for _ in 0..4 {
            counts.add_cache("Waypoint");
        }
        counts.add_cache("Query");
        assert_eq!(counts.entity_total(), 13);
        assert_eq!(counts.cache_total(), 5);

        // The scheduled despawns and drops.
        assert!(counts.remove_entity("Avatar"));
        assert!(counts.remove_entity("Avatar"));
        assert!(counts.remove_entity("Space"));
        for _ in 0..4 {
            assert!(counts.remove_cache("Waypoint"));
        }

        // The counts match the arithmetic the load is checking against.
        assert_eq!(counts.entity_count("Avatar"), 8);
        assert_eq!(counts.entity_count("Space"), 2);
        assert_eq!(counts.entity_count("Waypoint"), 0);
        assert_eq!(counts.entity_total(), 10);
        assert_eq!(counts.cache_count("Waypoint"), 0);
        assert_eq!(counts.cache_count("Query"), 1);
        assert_eq!(counts.cache_total(), 1);
        // A fully emptied cache leaves no row behind.
        assert_eq!(counts.cache_names(), vec!["Query"]);
    }

    #[test]
    fn removing_more_than_present_is_rejected() {
        let mut counts = ObjectCounts::new();
        assert!(!counts.remove_entity("Avatar"), "nothing was there");
        assert!(!counts.remove_cache("Query"));

        counts.add_entity("Avatar");
        assert!(counts.remove_entity("Avatar"));
        assert_eq!(counts.entity_count("Avatar"), 0);
        // A second removal cannot underflow the count.
        assert!(!counts.remove_entity("Avatar"));
        assert_eq!(counts.entity_count("Avatar"), 0);
        assert_eq!(counts.entity_types(), Vec::<&str>::new());
    }

    #[test]
    fn names_come_out_ascending() {
        let mut counts = ObjectCounts::new();
        counts.add_entity("Zombie");
        counts.add_entity("Avatar");
        counts.add_entity("Npc");
        counts.add_cache("Zone");
        counts.add_cache("Item");
        assert_eq!(counts.entity_types(), vec!["Avatar", "Npc", "Zombie"]);
        assert_eq!(counts.cache_names(), vec!["Item", "Zone"]);
    }
}

/// BW-24-01-05: the Python object census.
///
/// The diagnostic registers every live Python object it observes, keyed by
/// the object's address — the same per-address live map the reference keeps
/// for its Python allocations (`memory_debug.cpp` `LiveAllocationMap`), so a
/// known object's whole lifetime is one register followed by one release:
/// the count rises when it is created, falls when it is dropped, and nothing
/// in between disturbs it. Each observation is a reference, so an object is
/// counted once per address and stays counted while any reference holds it.
use std::sync::{Mutex, MutexGuard};

/// The live Python object census: each observed address with how many
/// references the diagnostic holds to it.
#[derive(Debug, Default)]
pub struct PythonObjectTracker {
    live: Mutex<BTreeMap<usize, u64>>,
}

/// One observed reference to a Python object, held until the lease is
/// dropped. Dropping it releases the reference, so the object's lifetime
/// closes when its last reference does.
#[derive(Debug)]
pub struct PyObjectLease<'tracker> {
    tracker: &'tracker PythonObjectTracker,
    address: usize,
}

impl PythonObjectTracker {
    /// A census with nothing live.
    pub fn new() -> Self {
        Self::default()
    }

    /// The census, recovering from a panicked holder like the other
    /// process-wide registries.
    fn live_census(&self) -> MutexGuard<'_, BTreeMap<usize, u64>> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Observe a reference to the live Python object at `address`. The object
    /// is counted while any reference is held, so a second reference to the
    /// same object still counts it once.
    pub fn observe(&self, address: usize) -> PyObjectLease<'_> {
        *self.live_census().entry(address).or_insert(0) += 1;
        PyObjectLease {
            tracker: self,
            address,
        }
    }

    /// The number of distinct live Python objects.
    pub fn live_count(&self) -> usize {
        self.live_census().len()
    }

    /// Whether an object at `address` is currently counted.
    pub fn is_live(&self, address: usize) -> bool {
        self.live_census().contains_key(&address)
    }
}

impl Drop for PyObjectLease<'_> {
    fn drop(&mut self) {
        let mut live = self.tracker.live_census();
        let release = {
            let references = live
                .get_mut(&self.address)
                .expect("a lease releases a reference it observed");
            *references -= 1;
            *references == 0
        };
        if release {
            live.remove(&self.address);
        }
    }
}

#[cfg(test)]
mod python_object_tests {
    use super::*;

    #[test]
    fn a_known_objects_lifetime_is_observable() {
        let tracker = PythonObjectTracker::new();
        // A known object appears at a fixed address; its lifetime is the
        // span between register and release.
        let address = 0x1234_5678;
        assert!(!tracker.is_live(address));
        assert_eq!(tracker.live_count(), 0);

        {
            let _known = tracker.observe(address);
            assert!(tracker.is_live(address));
            assert_eq!(tracker.live_count(), 1);
        }

        // The object dropped: the census saw the end of its lifetime.
        assert!(!tracker.is_live(address));
        assert_eq!(tracker.live_count(), 0);
    }

    #[test]
    fn several_objects_count_individually() {
        let tracker = PythonObjectTracker::new();
        let first = tracker.observe(0x1000);
        let second = tracker.observe(0x2000);
        let third = tracker.observe(0x3000);
        assert_eq!(tracker.live_count(), 3);

        // One object goes away; the other two stay observable.
        drop(second);
        assert_eq!(tracker.live_count(), 2);
        assert!(tracker.is_live(0x1000));
        assert!(!tracker.is_live(0x2000));
        assert!(tracker.is_live(0x3000));

        // The address can be observed again as a new object's lifetime.
        drop(first);
        drop(third);
        assert_eq!(tracker.live_count(), 0);
        let again = tracker.observe(0x2000);
        assert!(tracker.is_live(0x2000));
        drop(again);
        assert_eq!(tracker.live_count(), 0);
    }

    #[test]
    fn observing_the_same_object_twice_counts_it_once() {
        let tracker = PythonObjectTracker::new();
        let address = 0xABCD;
        let first = tracker.observe(address);
        let second = tracker.observe(address);
        assert_eq!(tracker.live_count(), 1, "the object is one object");

        // One lease ending does not end the object the other holds.
        drop(first);
        assert!(tracker.is_live(address));
        assert_eq!(tracker.live_count(), 1);
        drop(second);
        assert!(!tracker.is_live(address));
    }
}

// BW-24-01-06: the metric registry.
//
// A snapshot's metrics are not interchangeable: each comes from a source
// (the platform call or counter that produced it), costs something to
// collect, and is either a **live** (实时) reading — a gauge that moves up
// and down, like the resident set or the bytes held right now — or a
// **cumulative** (累计) total that only ever grows, like the bytes
// allocated since the run began. The registry records that per metric, so a
// consumer can tell a live value from a cumulative one and know what each
// reading cost.

/// Whether a reading is live (实时) or cumulative (累计).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// A gauge: moves up and down with the thing it measures.
    Live,
    /// A total: only ever grows.
    Cumulative,
}

/// One metric the snapshot reports: its source (口径), its collection cost
/// in basic operations, and whether the reading is live or cumulative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricDescriptor {
    /// The metric's name in the snapshot.
    pub name: &'static str,
    /// Where the reading comes from: the platform call or counter.
    pub source: &'static str,
    /// The collection cost, in basic operations (file reads, syscalls,
    /// atomic updates).
    pub cost: u64,
    /// Whether the reading is live or cumulative.
    pub kind: MetricKind,
}

/// The snapshot's metric table, in the order the metrics were defined.
#[derive(Debug, Default)]
pub struct MetricRegistry {
    metrics: Vec<MetricDescriptor>,
}

impl MetricRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a metric to the table.
    pub fn define(
        &mut self,
        name: &'static str,
        source: &'static str,
        cost: u64,
        kind: MetricKind,
    ) -> &mut Self {
        self.metrics.push(MetricDescriptor {
            name,
            source,
            cost,
            kind,
        });
        self
    }

    /// The metrics, in definition order.
    pub fn metrics(&self) -> &[MetricDescriptor] {
        &self.metrics
    }

    /// The reading's kind for a metric name.
    pub fn kind(&self, name: &str) -> Option<MetricKind> {
        self.metrics
            .iter()
            .find(|metric| metric.name == name)
            .map(|metric| metric.kind)
    }

    /// The live (实时) metrics.
    pub fn live_metrics(&self) -> Vec<&MetricDescriptor> {
        self.metrics
            .iter()
            .filter(|metric| metric.kind == MetricKind::Live)
            .collect()
    }

    /// The cumulative (累计) metrics.
    pub fn cumulative_metrics(&self) -> Vec<&MetricDescriptor> {
        self.metrics
            .iter()
            .filter(|metric| metric.kind == MetricKind::Cumulative)
            .collect()
    }
}

/// The table a memory snapshot reports: each reading, its platform source
/// (口径), its collection cost, and whether it is live or cumulative.
pub fn snapshot_metrics() -> MetricRegistry {
    let mut registry = MetricRegistry::new();
    // The resident set is one platform read; the source is the platform's.
    registry.define("process_rss", rss_source(), 1, MetricKind::Live);
    // Live and peak allocations move with the allocator; the total is the
    // cumulative counterpart.
    registry.define(
        "live_allocations",
        "tracked allocation counter",
        2,
        MetricKind::Live,
    );
    registry.define(
        "allocation_high_water",
        "ratcheting peak counter",
        1,
        MetricKind::Live,
    );
    registry.define(
        "total_allocated",
        "cumulative allocation counter",
        1,
        MetricKind::Cumulative,
    );
    registry.define("entities", "per-type entity counter", 1, MetricKind::Live);
    registry.define(
        "cache_objects",
        "per-cache object counter",
        1,
        MetricKind::Live,
    );
    registry.define(
        "python_objects",
        "per-address live census",
        1,
        MetricKind::Live,
    );
    registry
}

/// The platform source the resident-set reading comes from (口径).
fn rss_source() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "/proc/<pid>/status VmRSS"
    }
    #[cfg(target_os = "macos")]
    {
        "mach_task_basic_info resident_size"
    }
    #[cfg(target_os = "windows")]
    {
        "GetProcessMemoryInfo WorkingSetSize"
    }
}

#[cfg(test)]
mod metric_registry_tests {
    use super::*;

    #[test]
    fn live_and_cumulative_metrics_are_distinguishable() {
        let registry = snapshot_metrics();
        assert_eq!(
            registry.kind("total_allocated"),
            Some(MetricKind::Cumulative)
        );
        assert_eq!(registry.kind("live_allocations"), Some(MetricKind::Live));
        assert_eq!(registry.kind("process_rss"), Some(MetricKind::Live));
        assert_eq!(registry.kind("no_such_metric"), None);

        // The two kinds partition the table: every metric is one or the
        // other, and the cumulative set is the cumulative one only.
        let live = registry.live_metrics();
        let cumulative = registry.cumulative_metrics();
        assert_eq!(live.len() + cumulative.len(), registry.metrics().len());
        assert!(live.iter().all(|m| m.kind == MetricKind::Live));
        assert!(cumulative.iter().all(|m| m.kind == MetricKind::Cumulative));
        assert_eq!(
            cumulative.iter().map(|m| m.name).collect::<Vec<_>>(),
            vec!["total_allocated"]
        );
    }

    #[test]
    fn the_snapshot_table_records_sources_and_costs() {
        let registry = snapshot_metrics();
        assert!(!registry.metrics().is_empty());
        for metric in registry.metrics() {
            assert!(!metric.source.is_empty(), "{} has a source", metric.name);
            assert!(
                metric.cost >= 1,
                "{} costs something to collect",
                metric.name
            );
        }
        // The resident-set source names the platform call behind it.
        let rss = registry
            .metrics()
            .iter()
            .find(|m| m.name == "process_rss")
            .expect("process_rss is in the table");
        assert!(
            rss.source.contains("VmRSS")
                || rss.source.contains("resident_size")
                || rss.source.contains("WorkingSetSize")
        );
    }

    #[test]
    fn the_cumulative_total_only_grows() {
        let tracker = LiveAllocationTracker::new();
        {
            let one = tracker.allocate(4096);
            assert_eq!(tracker.live_bytes(), 4096);
            assert_eq!(tracker.total_allocated_bytes(), 4096);
            drop(one);
        }
        // The live reading fell back; the cumulative total kept the bytes.
        assert_eq!(tracker.live_bytes(), 0);
        assert_eq!(tracker.total_allocated_bytes(), 4096);

        // Another round only adds to the cumulative total.
        {
            let _two = tracker.allocate(2048);
            assert_eq!(tracker.live_bytes(), 2048);
            assert_eq!(tracker.total_allocated_bytes(), 4096 + 2048);
        }
        assert_eq!(tracker.live_bytes(), 0);
        assert_eq!(tracker.total_allocated_bytes(), 4096 + 2048);
    }
}
