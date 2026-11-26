//! OPT-06-01-03: allocation and live-bytes baseline for the download drain.
//!
//! The counting allocator follows the `tests/alloc_counts.rs` contract —
//! thread-local tracking, exactly reproducible on a given toolchain —
//! extended with live/peak bytes so the drain's resident bytes are
//! comparable. The `legacy_queue.rs` twin of the `d741450` queue mechanics
//! (a fresh staging `Vec<ServerChunk>` per drained download) is driven
//! through the same measured region in the same process, so the allocation
//! comparison between the per-download staging shape and the into-chunks
//! shape shares one allocator and one fixture builder. Run with:
//!
//! ```sh
//! cargo test -p server-runtime --test download_drain_alloc
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use server_runtime::server_download::{ServerChunk, ServerDownloads};

#[path = "../benches/legacy_queue.rs"]
mod legacy;
use legacy::LegacyServerDownloads;

/// Allocations, allocated bytes and live/peak live bytes counted during a
/// tracked region, on the calling thread only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
    live: usize,
    peak: usize,
}

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            BYTES.with(|c| c.set(c.get() + layout.size()));
            let live = LIVE.with(|c| {
                let live = c.get() + layout.size();
                c.set(live);
                live
            });
            PEAK.with(|c| {
                if live > c.get() {
                    c.set(live);
                }
            });
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACKING.with(Cell::get) {
            LIVE.with(|c| c.set(c.get().saturating_sub(layout.size())));
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn measure<T>(body: impl FnOnce() -> T) -> (Count, T) {
    TRACKING.with(|t| {
        ALLOCS.with(|c| c.set(0));
        BYTES.with(|c| c.set(0));
        LIVE.with(|c| c.set(0));
        PEAK.with(|c| c.set(0));
        t.set(true);
    });
    let out = body();
    TRACKING.with(|t| t.set(false));
    let count = TRACKING.with(|_| Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
        live: LIVE.with(Cell::get),
        peak: PEAK.with(Cell::get),
    });
    (count, out)
}

fn assert_reproducible<T>(body: impl Fn() -> T, label: &str) -> Count {
    let (first, _) = measure(&body);
    let (second, _) = measure(&body);
    assert_eq!(
        first, second,
        "{label}: allocation counts are not reproducible"
    );
    first
}

/// The stampede fixture: `count` one-fragment downloads (one-byte
/// description, 16-byte payload), finished by one bundle budget
/// (header 4 + fragment overhead 4 + payload 16 = 24 bytes each).
const STAMPEDE: usize = 2048;
const BUDGET: i32 = (STAMPEDE * 24) as i32;

fn build_pair() -> (ServerDownloads, LegacyServerDownloads) {
    let mut current = ServerDownloads::new();
    let mut old = LegacyServerDownloads::new();
    for _ in 0..STAMPEDE {
        let description = b"f".to_vec();
        let data = vec![7u8; 16];
        current.stream(description.clone(), data.clone(), u16::MAX);
        old.stream(description, data, u16::MAX);
    }
    (current, old)
}

/// The twin produces byte-identical chunks and finish reports, so the A/B
/// numbers below compare identical work.
#[test]
fn the_legacy_twin_matches_the_into_chunks_shape() {
    let (mut current, mut old) = build_pair();
    let current = current.add_to_bundle(BUDGET);
    let old = old.add_to_bundle(BUDGET);
    assert_eq!(current.chunks.len(), old.chunks.len());
    assert_eq!(current.finished.len(), old.finished.len());
    for (new_chunk, old_chunk) in current.chunks.iter().zip(old.chunks.iter()) {
        assert_eq!(new_chunk, old_chunk);
        assert!(matches!(
            new_chunk,
            ServerChunk::Header { .. } | ServerChunk::Fragment { .. }
        ));
    }
    assert_eq!(current.finished, old.finished);
}

/// The stampede drain is allocation-reproducible and its peak live bytes
/// cover the payload residency; both shapes are recorded for the doc.
#[test]
fn the_stampede_drain_is_recorded_and_reproducible() {
    // Drain-only window on a prebuilt fixture, recorded for the doc.
    let (mut current, mut old) = build_pair();
    let (count, finished) = measure(|| {
        let result = current.add_to_bundle(BUDGET);
        result.finished.len()
    });
    assert_eq!(finished, STAMPEDE);
    assert!(
        count.peak >= STAMPEDE * 16,
        "the payload residency must be visible in the peak"
    );
    println!("download_drain/stampede_{STAMPEDE} into_chunks (drain only): {count:?}");

    let (legacy_count, legacy_finished) = measure(|| {
        let result = old.add_to_bundle(BUDGET);
        result.finished.len()
    });
    assert_eq!(legacy_finished, STAMPEDE);
    println!("download_drain/stampede_{STAMPEDE} per_download_vec (drain only): {legacy_count:?}");

    // The full build+drain shape must be exactly reproducible per shape.
    let full = assert_reproducible(
        || {
            let (mut current, _) = build_pair();
            let result = current.add_to_bundle(BUDGET);
            result.finished.len()
        },
        "download_drain/stampede_2048 into_chunks",
    );
    assert!(full.allocs > 0);
    println!("download_drain/stampede_{STAMPEDE} into_chunks (build+drain): {full:?}");

    let legacy_full = assert_reproducible(
        || {
            let (_, mut old) = build_pair();
            let result = old.add_to_bundle(BUDGET);
            result.finished.len()
        },
        "download_drain/stampede_2048 per_download_vec",
    );
    println!("download_drain/stampede_{STAMPEDE} per_download_vec (build+drain): {legacy_full:?}");
}
