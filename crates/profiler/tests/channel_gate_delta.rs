//! OPT-12-01-02: lower the observation cost — the enabled/disabled
//! delta reported.
//!
//! The Profiler is the observation channel that ships a runtime gate
//! (`Profiler::enable`); the walk drives the OPT-12-01-01 frame shape
//! (64 frames × 8 Start/End pairs + a tick) through the gate in
//! halves — frames 1..=32 with the profiler enabled, frames 33..=64
//! with it disabled — plus a re-arm leg after the disabled half. Per
//! operation the walk records the wall-clock and the allocation count
//! under both gate states; the delta report is printed (`GATE-DELTA`
//! lines — wall-clock is probe data on this shared debug runner, the
//! allocation ledger is structural), and the gate's contract is
//! asserted: disabling drops what is buffered, disabled entries and
//! ticks are no-ops (the recording count, the closed frame, the
//! display pool and the processed-frame number all freeze), and the
//! gate re-arms through the same activation tick the first enable
//! used.
//!
//! The log and Watcher channels ship no runtime gate face (survey in
//! the review doc): their costs are the unconditional OPT-12-01-01
//! baselines, and that nil delta is part of the row's report.
//!
//! ```sh
//! cargo test -p profiler --test channel_gate_delta
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
// The gated walk.
// ---------------------------------------------------------------------------

/// Event pairs per frame.
const EVENTS: usize = 8;

/// Frames in the enabled half and in the disabled half.
const HALF: usize = 32;

/// One operation's cost under one gate state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpCost {
    ns: u128,
    allocs: usize,
}

/// One frame's ledger row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameRow {
    index: usize,
    enabled: bool,
    start: OpCost,
    end: OpCost,
    tick: OpCost,
    processed_frames: i32,
}

fn percentile(values: &[u128], fraction: f64) -> u128 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[index]
}

/// One gated walk: the OPT-12-01-01 frame shape through the gate.
fn gate_walk() -> Vec<FrameRow> {
    let stamp = Arc::new(AtomicU64::new(1_000));
    let clock_stamp = Arc::clone(&stamp);
    let profiler = Profiler::with_clock(move || clock_stamp.load(Ordering::SeqCst), 1_000_000.0);
    assert!(profiler.init(4096 * 336, 4), "the profiler inits");
    profiler.enable(true);
    profiler.add_this_thread("main");
    profiler.tick(); // the activation tick
    assert_eq!(
        profiler.final_entry_count(),
        0,
        "activation records nothing"
    );

    let names: Vec<String> = (0..EVENTS).map(|e| format!("task{e}")).collect();
    let mut rows = Vec::with_capacity(2 * HALF);
    for frame in 0..(2 * HALF) {
        let enabled = frame < HALF;

        if frame == HALF {
            // the drop contract: disabling drops whatever is buffered
            for name in &names {
                stamp.fetch_add(50, Ordering::SeqCst);
                profiler.add_entry(name, EventType::Start, 0, CATEGORY_CPP);
            }
            assert!(
                profiler.recording_entry_count() > 0,
                "the buffer is armed before the disable"
            );
            profiler.enable(false);
            assert!(!profiler.watcher_enabled(), "the gate closes");
            assert_eq!(
                profiler.recording_entry_count(),
                0,
                "disabling drops the buffered entries"
            );
        }
        // the closed-frame views freeze at the last processed frame
        // while disabled (disabled ticks process nothing)
        let (frozen_final, frozen_display) = if enabled {
            ((EVENTS * 2) as i32, EVENTS as u32)
        } else {
            (profiler.final_entry_count(), profiler.display_entry_count())
        };

        // the frame's event pairs, each cost measured per region
        let mut start_cost = OpCost { ns: 0, allocs: 0 };
        let mut end_cost = OpCost { ns: 0, allocs: 0 };
        for name in &names {
            stamp.fetch_add(50, Ordering::SeqCst);
            let started = std::time::Instant::now();
            let (_, s) = tracked(|| profiler.add_entry(name, EventType::Start, 0, CATEGORY_CPP));
            start_cost.ns += started.elapsed().as_nanos();
            start_cost.allocs += s.allocs;

            stamp.fetch_add(50, Ordering::SeqCst);
            let started = std::time::Instant::now();
            let (_, e) = tracked(|| profiler.add_entry(name, EventType::End, 0, CATEGORY_CPP));
            end_cost.ns += started.elapsed().as_nanos();
            end_cost.allocs += e.allocs;

            // the gate contract, live: a disabled entry must not land
            if !enabled {
                assert_eq!(
                    profiler.recording_entry_count(),
                    0,
                    "frame {}: a disabled entry landed",
                    frame + 1
                );
            }
        }
        if enabled {
            assert_eq!(
                profiler.recording_entry_count(),
                EVENTS as i32 * 2,
                "frame {}: the enabled frame buffers every entry",
                frame + 1
            );
        }

        stamp.fetch_add(1_000, Ordering::SeqCst);
        let started = std::time::Instant::now();
        let (_, tick) = tracked(|| profiler.tick());
        let tick_cost = OpCost {
            ns: started.elapsed().as_nanos(),
            allocs: tick.allocs,
        };
        start_cost.ns /= EVENTS as u128;
        end_cost.ns /= EVENTS as u128;

        // the closed-frame contract per gate state: live on the enabled
        // half, frozen at the last processed frame on the disabled half
        assert_eq!(
            profiler.final_entry_count(),
            frozen_final,
            "frame {}: the closed frame follows the gate",
            frame + 1
        );
        assert_eq!(
            profiler.display_entry_count(),
            frozen_display,
            "frame {}: the display pool follows the gate",
            frame + 1
        );
        let processed = profiler.get_frame_number();
        let expected_processed = if enabled {
            frame as i32 + 1
        } else {
            HALF as i32
        };
        assert_eq!(
            processed,
            expected_processed,
            "frame {}: the processed-frame number follows the gate",
            frame + 1
        );
        assert_eq!(
            profiler.watcher_enabled(),
            enabled,
            "frame {}: the watcher face answers the gate",
            frame + 1
        );

        rows.push(FrameRow {
            index: frame + 1,
            enabled,
            start: OpCost {
                ns: start_cost.ns,
                allocs: start_cost.allocs / EVENTS,
            },
            end: OpCost {
                ns: end_cost.ns,
                allocs: end_cost.allocs / EVENTS,
            },
            tick: tick_cost,
            processed_frames: processed,
        });
    }

    // the re-arm leg: the gate opens again through the same faces
    profiler.enable(true);
    assert!(profiler.watcher_enabled(), "the re-enable arms");
    profiler.tick(); // the activation tick again; the empty recording
    // buffer swaps nothing, so the last closed frame stays visible
    assert_eq!(
        profiler.final_entry_count(),
        (EVENTS * 2) as i32,
        "the re-activation closes no new frame"
    );
    stamp.fetch_add(50, Ordering::SeqCst);
    profiler.add_entry("rearm", EventType::Start, 0, CATEGORY_CPP);
    stamp.fetch_add(50, Ordering::SeqCst);
    profiler.add_entry("rearm", EventType::End, 0, CATEGORY_CPP);
    assert_eq!(
        profiler.recording_entry_count(),
        2,
        "the re-armed profiler records again"
    );
    stamp.fetch_add(1_000, Ordering::SeqCst);
    profiler.tick();
    assert_eq!(profiler.final_entry_count(), 2, "the re-armed frame closes");
    assert_eq!(
        profiler.display_entry_count(),
        1,
        "the re-armed pair collapses"
    );
    assert_eq!(
        profiler.get_frame_number(),
        HALF as i32 + 1,
        "processing resumes at the next frame number"
    );

    rows
}

#[test]
fn the_gate_reports_the_enabled_disabled_delta() {
    let rows = gate_walk();
    assert_eq!(rows.len(), 2 * HALF);

    // the structural delta: the disabled path allocates nothing and
    // never costs more than the enabled path
    let enabled = &rows[..HALF];
    let disabled = &rows[HALF..];
    for (op, pick) in [
        (
            "start_entry",
            (|r: &FrameRow| r.start) as fn(&FrameRow) -> OpCost,
        ),
        (
            "end_entry",
            (|r: &FrameRow| r.end) as fn(&FrameRow) -> OpCost,
        ),
        ("tick", (|r: &FrameRow| r.tick) as fn(&FrameRow) -> OpCost),
    ] {
        let on: Vec<u128> = enabled.iter().map(|r| pick(r).ns).collect();
        let off: Vec<u128> = disabled.iter().map(|r| pick(r).ns).collect();
        let on_allocs = enabled.iter().map(|r| pick(r).allocs).sum::<usize>();
        let off_allocs = disabled.iter().map(|r| pick(r).allocs).sum::<usize>();
        assert!(
            off_allocs <= on_allocs,
            "{op}: the disabled path must not allocate more than the enabled path"
        );
        println!(
            "GATE-DELTA op={op} enabled_ns_p50={} enabled_ns_p90={} disabled_ns_p50={} disabled_ns_p90={} enabled_allocs_total={on_allocs} disabled_allocs_total={off_allocs}",
            percentile(&on, 0.5),
            percentile(&on, 0.9),
            percentile(&off, 0.5),
            percentile(&off, 0.9),
        );
    }

    for row in &rows {
        println!(
            "GATE-FRAME frame={} enabled={} start_ns={} start_allocs={} end_ns={} end_allocs={} tick_ns={} tick_allocs={} processed={}",
            row.index,
            row.enabled,
            row.start.ns,
            row.start.allocs,
            row.end.ns,
            row.end.allocs,
            row.tick.ns,
            row.tick.allocs,
            row.processed_frames
        );
    }
    println!("GATE-HALVES enabled_frames={HALF} disabled_frames={HALF} events={EVENTS}/frame");
}

#[test]
fn the_gate_ledger_reruns_to_the_same_shape() {
    let first = gate_walk();
    let second = gate_walk();
    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(
            a.enabled, b.enabled,
            "frame {}: the gate schedule reruns",
            a.index
        );
        assert_eq!(
            a.start.allocs, b.start.allocs,
            "frame {}: the start ledger reruns",
            a.index
        );
        assert_eq!(
            a.end.allocs, b.end.allocs,
            "frame {}: the end ledger reruns",
            a.index
        );
        assert_eq!(
            a.tick.allocs, b.tick.allocs,
            "frame {}: the tick ledger reruns",
            a.index
        );
        assert_eq!(
            a.processed_frames, b.processed_frames,
            "frame {}: the processed-frame number reruns",
            a.index
        );
    }
    println!(
        "GATE-RERUN frames={} enabled_start_allocs={} disabled_start_allocs={} enabled_tick_allocs={} disabled_tick_allocs={}",
        first.len(),
        first[0].start.allocs,
        first[HALF].start.allocs,
        first[0].tick.allocs,
        first[HALF].tick.allocs,
    );
}
