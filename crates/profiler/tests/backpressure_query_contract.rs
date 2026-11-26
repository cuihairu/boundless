//! OPT-12-01-03: rerun backpressure and tool interoperability —
//! the Profiler channel's side of the query/data-record contract.
//!
//! The channel's backpressure is the entry budget: a profiler inited
//! at a four-entry budget records a six-entry frame and clamps at the
//! budget — the kept set is the budget-size prefix (the later entries
//! are refused, not the earlier ones), the closed frame answers the
//! kept entries through the query faces (`final_entry_count`,
//! `final_entry`, the display pool built from them), the high-water
//! marks the clamp, and the channel is not wedged: the next frame
//! records and closes normally. The refused entries allocate nothing
//! (the cost reruns flat), and the walk reruns to the same ledger.
//!
//! ```sh
//! cargo test -p profiler --test backpressure_query_contract
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use profiler::{CATEGORY_CPP, EventType, Profiler};

// ---------------------------------------------------------------------------
// The counting allocator (the OPT-01-01-01 pattern).
// ---------------------------------------------------------------------------

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
            BYTES.with(|b| b.set(b.get() + layout.size()));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn tracked<T>(body: impl FnOnce() -> T) -> (T, usize) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|b| b.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    (value, ALLOCS.with(Cell::get))
}

// ---------------------------------------------------------------------------
// The backpressured walk.
// ---------------------------------------------------------------------------

/// The init budget: the four-entry shape the reference formula gives
/// `4 * 336` (the unit-test budget).
const BUDGET_ENTRIES: i32 = 4;

/// Entries offered to the overflowing frame (more than the budget).
const OFFERED: usize = 6;

/// One walk: a four-entry-budget profiler records an overflowing
/// frame, closes it, then records a normal frame and closes it.
fn backpressure_walk() -> (Vec<String>, Vec<String>, usize) {
    let stamp = Arc::new(AtomicU64::new(1_000));
    let clock_stamp = Arc::clone(&stamp);
    let profiler = Profiler::with_clock(move || clock_stamp.load(Ordering::SeqCst), 1_000_000.0);
    assert!(profiler.init(BUDGET_ENTRIES * 336, 4), "the profiler inits");
    assert_eq!(
        profiler.max_entry_count(),
        BUDGET_ENTRIES,
        "the budget took"
    );
    profiler.enable(true);
    profiler.add_this_thread("main");
    profiler.tick();

    // the overflowing frame: more entries than the budget
    let names: Vec<String> = (0..OFFERED).map(|e| format!("e{e}")).collect();
    let mut refused_allocs = 0;
    for name in &names {
        stamp.fetch_add(50, Ordering::SeqCst);
        let (_, allocs) = tracked(|| profiler.add_entry(name, EventType::Start, 0, CATEGORY_CPP));
        refused_allocs += allocs;
    }
    assert_eq!(
        profiler.recording_entry_count(),
        BUDGET_ENTRIES,
        "the recording buffer clamps at the budget"
    );
    assert_eq!(refused_allocs, 0, "the refused entries allocate nothing");

    stamp.fetch_add(1_000, Ordering::SeqCst);
    profiler.tick();
    assert_eq!(
        profiler.final_entry_count(),
        BUDGET_ENTRIES,
        "the closed frame carries exactly the budget"
    );
    assert_eq!(
        profiler.high_watermark_of_entry_count(),
        BUDGET_ENTRIES,
        "the high-water marks the clamp"
    );
    let overflow_names: Vec<String> = (0..BUDGET_ENTRIES as usize)
        .map(|i| {
            profiler
                .final_entry(i as i32)
                .expect("the kept entries answer")
                .name_str()
                .to_string()
        })
        .collect();
    assert!(
        profiler.final_entry(BUDGET_ENTRIES).is_none(),
        "nothing beyond the budget answers"
    );
    // the kept set is the prefix, not the newest entries
    assert_eq!(
        overflow_names,
        vec![
            "e0".to_string(),
            "e1".to_string(),
            "e2".to_string(),
            "e3".to_string()
        ],
        "the budget keeps the earliest entries"
    );
    assert_eq!(
        profiler.display_entry_count(),
        profiler.max_display_entry_count(),
        "the display pool clamps at its own budget"
    );

    // the channel is not wedged: the next frame records and closes
    stamp.fetch_add(50, Ordering::SeqCst);
    profiler.add_entry("x", EventType::Start, 0, CATEGORY_CPP);
    stamp.fetch_add(50, Ordering::SeqCst);
    profiler.add_entry("x", EventType::End, 0, CATEGORY_CPP);
    stamp.fetch_add(50, Ordering::SeqCst);
    profiler.add_entry("y", EventType::Start, 0, CATEGORY_CPP);
    stamp.fetch_add(50, Ordering::SeqCst);
    profiler.add_entry("y", EventType::End, 0, CATEGORY_CPP);
    assert_eq!(
        profiler.recording_entry_count(),
        4,
        "the next frame records normally"
    );
    stamp.fetch_add(1_000, Ordering::SeqCst);
    profiler.tick();
    assert_eq!(
        profiler.final_entry_count(),
        4,
        "the next frame closes fully"
    );
    let recovery_names: Vec<String> = (0..4)
        .map(|i| {
            profiler
                .final_entry(i)
                .expect("the recovery entries answer")
                .name_str()
                .to_string()
        })
        .collect();
    assert_eq!(
        recovery_names,
        vec![
            "x".to_string(),
            "x".to_string(),
            "y".to_string(),
            "y".to_string()
        ],
        "the recovery frame carries its own entries"
    );
    assert_eq!(profiler.display_entry_count(), 2, "the pairs collapse");
    assert_eq!(profiler.get_frame_number(), 2, "both frames processed");

    (overflow_names, recovery_names, refused_allocs)
}

#[test]
fn the_buffer_backpressures_at_the_budget_and_the_query_answers() {
    let (overflow, recovery, refused_allocs) = backpressure_walk();
    println!(
        "PROFILER-BACKPRESSURE budget={BUDGET_ENTRIES} offered={OFFERED} kept={overflow:?} refused_allocs={refused_allocs} recovery={recovery:?}"
    );
}

#[test]
fn the_backpressured_ledger_reruns() {
    let (overflow_a, recovery_a, allocs_a) = backpressure_walk();
    let (overflow_b, recovery_b, allocs_b) = backpressure_walk();
    assert_eq!(overflow_a, overflow_b, "the kept set reruns");
    assert_eq!(recovery_a, recovery_b, "the recovery frame reruns");
    assert_eq!(allocs_a, allocs_b, "the refusal cost reruns");
    println!(
        "PROFILER-BACKPRESSURE-RERUN kept_equal=true recovery_equal=true refused_allocs={allocs_a}"
    );
}
