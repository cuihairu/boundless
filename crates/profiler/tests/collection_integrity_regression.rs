//! OPT-23-01-03: collection integrity regression.
//!
//! The acceptance: **the expected sequence, cancellation and overflow
//! statistics are correct** (`预期序列、取消及溢出统计正确`). Three
//! legs over fresh local profilers:
//!
//! - **Sequence** — a mixed workload (a scoped worker thread's
//!   sections, a counter, an idle section, a plain section) replays
//!   from the final buffer field-exact: names, event kinds, counter
//!   values, thread slots and the recording frame number, in
//!   submission order.
//! - **Cancellation** — `enable(false)` mid-frame discards the
//!   in-flight data ("ignore all current data"): the recording buffer
//!   empties, nothing reaches the final buffer, and a re-enabled
//!   profiler collects fresh from its next activation.
//! - **Overflow statistics** — an entry budget of 4 keeps the first 4
//!   submissions and clamps the watermark at 4; a display budget of 2
//!   sets the reference error string; a 65-deep stack keeps the 64
//!   sections that fit. A removal requested while active defers to the
//!   frame's end (the cancellation-adjacent face).

use profiler::{CATEGORY_CPP, EventType, Profiler};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

const SLOT_UNIT: i32 = 336;

#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }
}

fn expect(
    entry: Option<profiler::ProfileEntry>,
    name: &str,
    event: EventType,
    value: i32,
    thread_idx: i32,
    frame_number: i32,
    checks: &mut usize,
) {
    let entry = entry.expect("the entry exists");
    assert_eq!(entry.name_str(), name, "name at submission order");
    assert_eq!(entry.event, event, "{name} event kind");
    assert_eq!(entry.value, value, "{name} counter value");
    assert_eq!(entry.thread_idx, thread_idx, "{name} thread slot");
    assert_eq!(entry.frame_number, frame_number, "{name} recorded frame");
    *checks += 1;
}

/// The sequence, cancellation and overflow integrity regression.
#[test]
fn the_collection_sequence_cancellation_and_overflow_stats_hold() {
    let mut record = Record::default();

    // ---- Sequence: worker thread sections, then main-thread mixed
    // entries, replayed field-exact in submission order.
    let p = Profiler::new();
    assert!(p.init(64 * SLOT_UNIT, 4));
    p.enable(true);
    p.tick(); // activates recording
    let worker_tid = AtomicU64::new(0);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            p.add_this_thread("WorkerThread");
            worker_tid.store(profiler::this_thread_id(), Ordering::SeqCst);
            p.add_entry("worker::job", EventType::Start, 7, CATEGORY_CPP);
            p.add_entry("worker::job", EventType::End, 0, CATEGORY_CPP);
        });
    });
    p.add_entry("fps", EventType::Counter, 60, CATEGORY_CPP);
    p.add_entry("idle::wait", EventType::StartIdle, 0, CATEGORY_CPP);
    p.add_entry("idle::wait", EventType::End, 0, CATEGORY_CPP);
    p.add_entry("render", EventType::Start, 0, CATEGORY_CPP);
    p.add_entry("render", EventType::End, 0, CATEGORY_CPP);
    assert_eq!(p.recording_entry_count(), 7);
    p.tick();
    assert_eq!(p.final_entry_count(), 7, "every submission landed");
    let mut checks = 0;
    let frame = 0; // the first recorded frame
    expect(
        p.final_entry(0),
        "worker::job",
        EventType::Start,
        7,
        1,
        frame,
        &mut checks,
    );
    expect(
        p.final_entry(1),
        "worker::job",
        EventType::End,
        0,
        1,
        frame,
        &mut checks,
    );
    expect(
        p.final_entry(2),
        "fps",
        EventType::Counter,
        60,
        0,
        frame,
        &mut checks,
    );
    expect(
        p.final_entry(3),
        "idle::wait",
        EventType::StartIdle,
        0,
        0,
        frame,
        &mut checks,
    );
    expect(
        p.final_entry(4),
        "idle::wait",
        EventType::End,
        0,
        0,
        frame,
        &mut checks,
    );
    expect(
        p.final_entry(5),
        "render",
        EventType::Start,
        0,
        0,
        frame,
        &mut checks,
    );
    expect(
        p.final_entry(6),
        "render",
        EventType::End,
        0,
        0,
        frame,
        &mut checks,
    );
    assert!(p.final_entry(7).is_none());
    record.put("sequence_entries", 7);
    record.put("sequence_field_checks", checks);
    record.put("sequence_threads", p.num_threads());

    // ---- Deferred removal: the worker's slot frees at the frame's
    // end, not at the request.
    p.remove_thread(worker_tid.load(Ordering::SeqCst));
    assert_eq!(
        p.get_thread_name(1),
        Some("WorkerThread".to_string()),
        "the removal defers while the profiler is active"
    );
    assert_eq!(p.num_threads(), 2);
    p.add_entry("fps", EventType::Counter, 61, CATEGORY_CPP);
    p.tick();
    assert_eq!(p.final_entry_count(), 1, "the post-removal frame collected");
    assert_eq!(
        p.get_thread_name(1),
        None,
        "the slot freed at the frame end"
    );
    assert_eq!(p.num_threads(), 1);
    record.put("deferred_removal", 1);

    // ---- Cancellation: mid-frame disable discards in-flight data.
    let p = Profiler::new();
    assert!(p.init(64 * SLOT_UNIT, 4));
    p.enable(true);
    p.tick();
    for i in 0..3 {
        p.add_entry(&format!("torn{i}"), EventType::Start, 0, CATEGORY_CPP);
    }
    assert_eq!(p.recording_entry_count(), 3);
    p.enable(false);
    assert_eq!(
        p.recording_entry_count(),
        0,
        "the disable ignores all current data"
    );
    p.tick();
    assert_eq!(p.final_entry_count(), 0, "nothing reached the final buffer");
    assert!(!p.watcher_enabled());
    // Resume: collection restarts from its next activation.
    p.enable(true);
    p.tick();
    p.add_entry("fresh", EventType::Counter, 1, CATEGORY_CPP);
    p.tick();
    assert_eq!(p.final_entry_count(), 1, "the resumed run collects fresh");
    assert_eq!(
        p.final_entry(0).expect("kept").name_str(),
        "fresh",
        "the torn frame never leaks into the resumed run"
    );
    record.put("cancellation_dropped", 3);
    record.put("resume_collected", 1);

    // ---- Overflow statistics: entry clamp, display error, stack skip.
    let p = Profiler::new();
    // 4 slots of entry budget, 2 of display budget.
    assert!(p.init(4 * SLOT_UNIT, 4));
    assert_eq!(p.max_entry_count(), 4);
    assert_eq!(p.max_display_entry_count(), 2);
    p.enable(true);
    p.tick();
    for i in 0..6 {
        p.add_entry(&format!("e{i}"), EventType::Start, 0, CATEGORY_CPP);
    }
    assert_eq!(p.recording_entry_count(), 4, "the clamp holds the ceiling");
    p.tick();
    assert_eq!(p.final_entry_count(), 4);
    assert_eq!(p.high_watermark_of_entry_count(), 4, "the watermark pins");
    assert_eq!(
        p.final_entry(3).expect("kept").name_str(),
        "e3",
        "first come kept"
    );
    assert!(p.final_entry(4).is_none(), "the overflow entries are gone");
    // The display overflow surfaces as the reference error string.
    p.tick();
    p.add_entry("a", EventType::Start, 0, CATEGORY_CPP);
    p.add_entry("b", EventType::Start, 0, CATEGORY_CPP);
    p.add_entry("c", EventType::Start, 0, CATEGORY_CPP);
    p.add_entry("b", EventType::End, 0, CATEGORY_CPP);
    p.add_entry("a", EventType::End, 0, CATEGORY_CPP);
    p.tick();
    assert_eq!(p.display_entry_count(), 2);
    assert_eq!(
        p.get_error_string(),
        "ERROR: Too many display entries\n\
         increase your buffer size in <debug/profileBufferSize> \
         in engine_config.\n"
    );
    // The stack depth skip: 65 nested sections, 64 stored. A fresh
    // profiler — the 4-slot budget above would clamp the entries long
    // before the stack overflows (200 slots hold all 130).
    let p = Profiler::new();
    assert!(p.init(200 * SLOT_UNIT, 4));
    p.enable(true);
    p.tick();
    for _ in 0..65 {
        p.add_entry("deep", EventType::Start, 0, CATEGORY_CPP);
    }
    for _ in 0..65 {
        p.add_entry("deep", EventType::End, 0, CATEGORY_CPP);
    }
    p.tick();
    assert_eq!(p.display_entry_count(), 64, "the 65th start skipped");
    let mut chain = 0u32;
    let mut node = p.thread_head(0).expect("root built");
    loop {
        chain += 1;
        match p.display_pool()[node as usize].child {
            Some(child) => node = child,
            None => break,
        }
    }
    assert_eq!(chain, 64, "the display chain carries exactly what fit");
    record.put("entry_overflow_budget", 4);
    record.put("entry_overflow_lost", 2);
    record.put("entry_overflow_watermark", 4);
    record.put("display_overflow_error", 1);
    record.put("stack_overflow_kept", 64);
    record.put("stack_overflow_lost", 1);

    let mut text = String::new();
    for (k, v) in &record.0 {
        text.push_str(&format!("{k}\t{v}\n"));
    }
    print!("{text}");
    if let Ok(path) = std::env::var("COLLECTION_REGRESSION_OUT") {
        std::fs::write(&path, &text).expect("write COLLECTION_REGRESSION_OUT");
        println!("record: {path}");
    }
    let _ = std::io::stdout().flush();
}
