//! BW-24-01-01: memory snapshot — per-process RSS.
//!
//! The reference reports a process's resident set from whatever the platform
//! offers (`memory_debug.cpp` `ProcessMemoryInfo` is Windows-only; the Linux
//! and macOS diagnostics read their own sources), and each platform reports
//! in its own native unit. A snapshot must compare processes against each
//! other, so every reading is unified into bytes here before it leaves this
//! module: Linux `/proc/<pid>/status` reports kB, macOS
//! `mach_task_basic_info` and Windows `GetProcessMemoryInfo` report bytes,
//! and a page-based source would be scaled by the page size.

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

/// BW-24-01-02: the live allocation reading.
///
/// The tracker counts the bytes its fixture allocations hold and gives each
/// one back on release, so the reading is the **live** value — it moves with
/// every allocation and falls back when memory is returned, unlike a
/// cumulative total that only ever grows. The reference instruments the real
/// allocator for this (`memory_debug.cpp` registers every `allocate` and
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

/// The live-byte accounting for a snapshot's allocation reading.
#[derive(Debug, Default)]
pub struct LiveAllocationTracker {
    live_bytes: AtomicU64,
}

impl LiveAllocationTracker {
    /// A tracker with nothing live.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes currently held by unreleased allocations.
    pub fn live_bytes(&self) -> u64 {
        self.live_bytes.load(Ordering::SeqCst)
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
        self.live_bytes.fetch_add(bytes as u64, Ordering::SeqCst);
        TrackedAllocation {
            tracker: self,
            buffer: vec![0_u8; bytes],
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
}
