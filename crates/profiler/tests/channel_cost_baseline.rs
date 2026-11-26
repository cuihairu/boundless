//! OPT-12-01-01 (the Profiler channel): the Profiler channel's cost,
//! recorded separately from the logging channel's and the Watcher's —
//! one event pair added, per event, and one frame closed, per tick,
//! through the shipped faces (`Profiler::with_clock` +
//! `add_entry` + `tick`, with the query faces reading the closed
//! frame back).
//!
//! The per-event and per-tick wall-clock times and allocation counts
//! are the channel's baseline: times are probe data, printed for the
//! review doc, never asserted; the allocation ledger and the record
//! contract (every closed frame carries its events, every pair
//! collapses into one display entry, the frame-end stamp rides the
//! clock) are asserted and rerun identically.
//!
//! The clock is deterministic (`with_clock` over an atomic the walk
//! steps by hand), so the recorded durations are part of the contract
//! even though the wall-clocks around the calls are probe data.
//!
//! ```sh
//! cargo test -p profiler --test channel_cost_baseline
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

/// Allocations + bytes counted during the region, on the calling thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
}

fn tracked<T>(body: impl FnOnce() -> T) -> (T, Count) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|b| b.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    let count = Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
    };
    (value, count)
}

// ---------------------------------------------------------------------------
// The Profiler channel.
// ---------------------------------------------------------------------------

/// Frames in one channel walk.
const FRAMES: usize = 64;

/// Event pairs per frame (a plausible frame's scopes).
const EVENTS: usize = 8;

/// Entries the closed frame carries per frame (a Start and an End per
/// event).
const ENTRIES_PER_FRAME: i32 = (EVENTS * 2) as i32;

/// One event's ledger row: the Start cost, the End cost (allocations
/// counted per region; the wall-clocks ride along as probe data).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EventLedger {
    start_ns: u128,
    start_allocs: usize,
    end_ns: u128,
    end_allocs: usize,
}

/// One frame's ledger row: the per-event rows, the tick cost, and the
/// closed frame's contract values.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FrameLedger {
    index: usize,
    events: Vec<EventLedger>,
    tick_ns: u128,
    tick_allocs: usize,
    final_entries: i32,
    display_entries: u32,
    frame_end_stamp: u64,
    high_watermark: i32,
}

/// One Profiler-channel walk: the profiler inits enabled against the
/// stepped clock, one thread registers, the first tick activates
/// recording, then FRAMES frames each add EVENTS Start/End pairs and
/// close with a tick; every closed frame is read back through the
/// query faces.
fn channel_walk() -> Vec<FrameLedger> {
    let stamp = Arc::new(AtomicU64::new(1_000));
    let clock_stamp = Arc::clone(&stamp);
    let profiler = Profiler::with_clock(move || clock_stamp.load(Ordering::SeqCst), 1_000_000.0);
    assert!(profiler.init(4096 * 336, 4), "the profiler inits");
    profiler.enable(true);
    profiler.add_this_thread("main");
    profiler.tick(); // the first tick after enable only activates
    assert_eq!(
        profiler.final_entry_count(),
        0,
        "activation records nothing"
    );

    let names: Vec<String> = (0..EVENTS).map(|e| format!("task{e}")).collect();
    let mut ledgers = Vec::with_capacity(FRAMES);
    for frame in 0..FRAMES {
        let mut events = Vec::with_capacity(EVENTS);
        for (event, name) in names.iter().enumerate() {
            stamp.fetch_add(50, Ordering::SeqCst);
            let started = std::time::Instant::now();
            let (_, start) =
                tracked(|| profiler.add_entry(name, EventType::Start, 0, CATEGORY_CPP));
            let start_ns = started.elapsed().as_nanos();

            stamp.fetch_add(50, Ordering::SeqCst);
            let started = std::time::Instant::now();
            let (_, end) = tracked(|| profiler.add_entry(name, EventType::End, 0, CATEGORY_CPP));
            let end_ns = started.elapsed().as_nanos();

            events.push(EventLedger {
                start_ns,
                start_allocs: start.allocs,
                end_ns,
                end_allocs: end.allocs,
            });
            let _ = event;
        }

        stamp.fetch_add(1_000, Ordering::SeqCst);
        let started = std::time::Instant::now();
        let (_, tick) = tracked(|| profiler.tick());
        let tick_ns = started.elapsed().as_nanos();

        // the record contract, per closed frame
        let final_entries = profiler.final_entry_count();
        assert_eq!(
            final_entries, ENTRIES_PER_FRAME,
            "frame {frame}: the closed frame carries every event"
        );
        let display_entries = profiler.display_entry_count();
        assert_eq!(
            display_entries, EVENTS as u32,
            "frame {frame}: every pair collapses into one display entry"
        );
        for (event, name) in names.iter().enumerate() {
            let entry = profiler
                .final_entry((event * 2) as i32)
                .unwrap_or_else(|| panic!("frame {frame}: event {event} Start readable"));
            assert_eq!(
                entry.name_str(),
                name.as_str(),
                "frame {frame}: event {event} answers its name"
            );
        }
        let frame_end_stamp = profiler.final_end_of_frame_time_stamp();
        assert_eq!(
            frame_end_stamp,
            1_000 + ((frame as u64 + 1) * (EVENTS as u64 * 100 + 1_000)),
            "frame {frame}: the frame-end stamp rides the clock"
        );
        assert_eq!(
            profiler.recording_entry_count(),
            0,
            "frame {frame}: the recording buffer swapped empty"
        );

        ledgers.push(FrameLedger {
            index: frame + 1,
            events,
            tick_ns,
            tick_allocs: tick.allocs,
            final_entries,
            display_entries,
            frame_end_stamp,
            high_watermark: profiler.high_watermark_of_entry_count(),
        });
    }
    assert_eq!(profiler.num_threads(), 1, "one thread walked");
    assert_eq!(
        ledgers.last().expect("frames").high_watermark,
        ENTRIES_PER_FRAME,
        "the high-water never exceeds one frame's entries"
    );
    ledgers
}

#[test]
fn the_profiler_channel_records_per_event_and_per_tick() {
    let ledgers = channel_walk();
    assert_eq!(ledgers.len(), FRAMES);

    // the cost contract: the ledger is flat — every event and every
    // tick costs the same allocations, so the channel has no drift
    let first = &ledgers[0];
    for (frame, ledger) in ledgers.iter().enumerate() {
        for (event, row) in ledger.events.iter().enumerate() {
            assert_eq!(
                row.start_allocs, first.events[event].start_allocs,
                "frame {frame} event {event}: the Start cost is flat"
            );
            assert_eq!(
                row.end_allocs, first.events[event].end_allocs,
                "frame {frame} event {event}: the End cost is flat"
            );
        }
        assert_eq!(
            ledger.tick_allocs, first.tick_allocs,
            "frame {frame}: the tick cost is flat"
        );
        assert_eq!(ledger.final_entries, ENTRIES_PER_FRAME);
        assert_eq!(ledger.display_entries, EVENTS as u32);
    }

    for ledger in &ledgers {
        for (event, row) in ledger.events.iter().enumerate() {
            println!(
                "PROFILER-CHANNEL frame={} event={} start_ns={} start_allocs={} end_ns={} end_allocs={}",
                ledger.index, event, row.start_ns, row.start_allocs, row.end_ns, row.end_allocs
            );
        }
        println!(
            "PROFILER-TICK frame={} tick_ns={} tick_allocs={} final_entries={} display_entries={} stamp={}",
            ledger.index,
            ledger.tick_ns,
            ledger.tick_allocs,
            ledger.final_entries,
            ledger.display_entries,
            ledger.frame_end_stamp
        );
    }
    let starts: usize = ledgers
        .iter()
        .flat_map(|l| l.events.iter())
        .map(|e| e.start_allocs)
        .sum();
    let ends: usize = ledgers
        .iter()
        .flat_map(|l| l.events.iter())
        .map(|e| e.end_allocs)
        .sum();
    let ticks: usize = ledgers.iter().map(|l| l.tick_allocs).sum();
    println!(
        "PROFILER-CHANNEL-TOTAL frames={FRAMES} events={EVENTS}/frame start_allocs={starts} end_allocs={ends} tick_allocs={ticks}"
    );
}

#[test]
fn the_profiler_channel_reruns_to_the_same_ledger() {
    let first = channel_walk();
    let second = channel_walk();
    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(a.index, b.index);
        for (event, (x, y)) in a.events.iter().zip(b.events.iter()).enumerate() {
            assert_eq!(
                x.start_allocs, y.start_allocs,
                "frame {} event {event}: the Start ledger reruns",
                a.index
            );
            assert_eq!(
                x.end_allocs, y.end_allocs,
                "frame {} event {event}: the End ledger reruns",
                a.index
            );
        }
        assert_eq!(
            a.tick_allocs, b.tick_allocs,
            "frame {}: the tick ledger reruns",
            a.index
        );
        assert_eq!(
            a.frame_end_stamp, b.frame_end_stamp,
            "frame {}: the stamp reruns",
            a.index
        );
        assert_eq!(
            a.high_watermark, b.high_watermark,
            "frame {}: the high-water reruns",
            a.index
        );
    }
    println!(
        "PROFILER-RERUN frames={FRAMES} events={EVENTS}/frame start_allocs={} tick_allocs={} per_frame",
        first[0].events[0].start_allocs, first[0].tick_allocs
    );
}
