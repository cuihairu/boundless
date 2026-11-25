//! Allocation counts for the `DataSectionCache` hot path (OPT-01-01-03):
//! the LRU relink machinery — `find` on a hit (head, middle and tail),
//! `find` on a miss, `purgeLRU`, `remove`, and an in-place re-add — must
//! run allocation-free in steady state, like the reference's raw
//! list-node relinks in `data_section_cache.cpp`. Only `add` with a new
//! name may allocate (the map key and the slot's name copy, once each).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use resources::data_section_cache::DataSectionCache;

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            BYTES.with(|c| c.set(c.get() + layout.size()));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Counts every allocation made by `body`. The counters are thread-local
/// so concurrent tests in the same binary cannot truncate each other.
fn measure<T>(body: impl FnOnce() -> T) -> (usize, usize, T) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|c| c.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    (ALLOCS.with(Cell::get), BYTES.with(Cell::get), value)
}

fn section(tag: &str) -> Arc<String> {
    Arc::new(tag.to_string())
}

/// A cache holding `entry0` (oldest) .. `entry6` (newest), no purge in
/// play: 7 entries of 100 bytes under a 10 KiB budget.
fn seeded_cache() -> DataSectionCache<String> {
    let mut cache = DataSectionCache::with_size(10_000);
    for i in 0..7 {
        cache.add(&format!("entry{i}"), 100, section("payload"));
    }
    cache
}

#[test]
fn find_hit_and_miss_allocate_nothing() {
    let mut cache = seeded_cache();
    // Head hit: move-to-head early-returns.
    let (allocs, _, hit) = measure(|| cache.find("entry6"));
    assert!(hit.is_some());
    assert_eq!(allocs, 0);
    // Middle hit: unlink rejoins two neighbours, then a relink.
    let (allocs, _, hit) = measure(|| cache.find("entry2"));
    assert!(hit.is_some());
    assert_eq!(allocs, 0);
    // Tail hit: unlink detaches the tail, then a relink.
    let (allocs, _, hit) = measure(|| cache.find("entry0"));
    assert!(hit.is_some());
    assert_eq!(allocs, 0);
    // Miss: counted, no allocation.
    let (allocs, _, miss) = measure(|| cache.find("ghost.xml"));
    assert!(miss.is_none());
    assert_eq!(allocs, 0);
    assert_eq!((cache.hits(), cache.misses()), (3, 1));
}

#[test]
fn purge_remove_and_in_place_readd_allocate_nothing() {
    let mut cache = seeded_cache();

    // purgeLRU drops the tail.
    let (allocs, _, ()) = measure(|| cache.purge_lru());
    assert_eq!(allocs, 0);
    assert_eq!(cache.len(), 6);
    assert_eq!(cache.current_bytes(), 600);

    // Middle removal (unlink case 4)...
    let (allocs, _, ()) = measure(|| cache.remove("entry3"));
    assert_eq!(allocs, 0);
    // ...tail removal (case 3)...
    let (allocs, _, ()) = measure(|| cache.remove("entry1"));
    assert_eq!(allocs, 0);
    // ...and head removal (case 2).
    let (allocs, _, ()) = measure(|| cache.remove("entry5"));
    assert_eq!(allocs, 0);
    assert_eq!(cache.len(), 3);
    assert_eq!(cache.current_bytes(), 300);

    // In-place re-add replaces the payload without allocating (the
    // payload Arc is built outside the measured region).
    let fresh = section("fresh");
    let (allocs, _, ()) = measure(|| cache.add("entry2", 100, fresh));
    assert_eq!(allocs, 0);
    assert_eq!(cache.current_bytes(), 300);

    // The freed slots are reused: re-adding a purged name lands on the
    // free chain, and the chain still walks correctly afterwards — the
    // purged "entry0" comes back as a fresh head, the in-place "entry2"
    // keeps its tail position.
    cache.add("entry0", 100, section("back"));
    assert_eq!(
        cache.lru_order(),
        vec![
            ("entry0".to_string(), 100),
            ("entry6".to_string(), 100),
            ("entry4".to_string(), 100),
            ("entry2".to_string(), 100),
        ]
    );
}

#[test]
fn fresh_add_allocates_for_the_new_entry_and_is_reproducible() {
    // New names cost the map key and the slot name copy (plus whatever
    // first-insert table growth the hash map pays); the payload Arcs are
    // built outside the measured regions.
    let mut cache = DataSectionCache::<String>::with_size(10_000);
    let payload_a = section("a");
    let (first, _, ()) = measure(|| cache.add("a/path.xml", 100, payload_a));
    let payload_b = section("b");
    let (second, _, ()) = measure(|| cache.add("b/path.xml", 100, payload_b));
    let payload_c = section("c");
    let (third, _, ()) = measure(|| cache.add("c/path.xml", 100, payload_c));
    assert!(first >= 2, "first add must pay at least the two Strings");
    assert!(second >= 2);
    assert!(third >= 2);

    // Toolchain-exact: two identical fresh sequences allocate identically.
    let mut first_run = DataSectionCache::<String>::with_size(10_000);
    let mut second_run = DataSectionCache::<String>::with_size(10_000);
    let (count_a, _, ()) = measure(|| {
        first_run.add("a/path.xml", 100, section("a"));
        first_run.add("b/path.xml", 100, section("b"));
        first_run.add("c/path.xml", 100, section("c"));
    });
    let (count_b, _, ()) = measure(|| {
        second_run.add("a/path.xml", 100, section("a"));
        second_run.add("b/path.xml", 100, section("b"));
        second_run.add("c/path.xml", 100, section("c"));
    });
    assert!(count_a >= 6);
    assert_eq!(count_a, count_b);
}
