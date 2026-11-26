//! OPT-23-01-02: collection buffering and export evaluation.
//!
//! The acceptance: **report the overhead change on the same scenario**
//! (`按同场景报告开销变化`). One scripted scenario — 100 frames, 20
//! zero-width sections (40 entries) per frame — runs under four shapes:
//!
//! - **Three buffer budgets** (`init` memory → 256 / 40 / 10 entry
//!   slots): the generous and exact budgets record everything, the
//!   tight budget clamps every frame at its 10-slot ceiling and drops
//!   the rest. Per-tick cost and drop counts are the report.
//! - **Export on the generous budget**: the first 20 frames of the
//!   same scenario dump Chrome-trace JSON into a scratch directory,
//!   the remaining 80 run dump-free. Splitting the per-tick samples at
//!   the dump boundary reports the export overhead on identical
//!   frames; the state machine (Inactive → Active → Completed), the
//!   output file and the unwritable-directory failure face are gated.

use profiler::{CATEGORY_CPP, EventType, JsonDumpState, Profiler};
use std::io::Write as _;
use std::time::Instant;

const FRAMES: usize = 100;
const SECTIONS_PER_FRAME: usize = 20;
const ENTRIES_PER_FRAME: usize = SECTIONS_PER_FRAME * 2;
const DUMP_FRAMES: i32 = 20;
const THREADS: i32 = 4;
/// `max_entry_count = memory / (128*2 + 160/2)`.
const SLOT_UNIT: i32 = 336;
const SECTION_NAMES: [&str; 4] = [
    "cellapp::tick",
    "cellapp::move",
    "cellapp::ai",
    "cellapp::vision",
];

#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }
}

fn percentile(samples: &[u64], pct: usize) -> u64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() - 1) * pct / 100;
    sorted[index]
}

/// Runs the scenario with collection on, measuring every tick.
fn run_scenario(memory: i32, dump_dir: Option<&str>) -> (Profiler, Vec<u64>) {
    let p = Profiler::new();
    assert!(p.init(memory, THREADS));
    if let Some(dir) = dump_dir {
        p.set_json_dump_dir(dir);
        p.set_json_dump_count(DUMP_FRAMES);
        p.watcher_set_dump_profile(true);
    }
    p.enable(true);
    p.tick(); // activates recording
    let mut tick_ns = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let t0 = Instant::now();
        for s in 0..SECTIONS_PER_FRAME {
            let name = SECTION_NAMES[s % SECTION_NAMES.len()];
            p.add_entry(name, EventType::Start, s as i32, CATEGORY_CPP);
            p.add_entry(name, EventType::End, 0, CATEGORY_CPP);
        }
        p.tick();
        tick_ns.push(t0.elapsed().as_nanos() as u64);
    }
    (p, tick_ns)
}

fn put_ticks(record: &mut Record, prefix: impl AsRef<str>, tick_ns: &[u64]) {
    let prefix = prefix.as_ref();
    let ticks = tick_ns.len() as f64;
    let total: u64 = tick_ns.iter().sum();
    record.put(
        format!("{prefix}_tick_avg_ns"),
        format!("{:.0}", total as f64 / ticks),
    );
    record.put(
        format!("{prefix}_tick_max_ns"),
        tick_ns.iter().max().copied().unwrap_or(0),
    );
    record.put(format!("{prefix}_tick_p99_ns"), percentile(tick_ns, 99));
}

/// The buffering and export evaluation.
#[test]
fn the_buffer_and_export_overheads_report_on_one_scenario() {
    let mut record = Record::default();
    record.put("frames", FRAMES);
    record.put("entries_per_frame", ENTRIES_PER_FRAME);

    // ---- Buffer budgets: the same frames under three ceilings.
    for (name, slots) in [
        ("generous", 256),
        ("exact", ENTRIES_PER_FRAME as i32),
        ("tight", 10),
    ] {
        let (p, tick_ns) = run_scenario(slots * SLOT_UNIT, None);
        assert_eq!(p.max_entry_count(), slots, "{name} budget derived");
        let watermark = p.high_watermark_of_entry_count();
        let final_count = p.final_entry_count();
        assert!(
            watermark <= slots,
            "{name}: the watermark stays inside the ceiling"
        );
        let dropped = if slots >= ENTRIES_PER_FRAME as i32 {
            assert_eq!(final_count, ENTRIES_PER_FRAME as i32, "{name} records all");
            assert_eq!(watermark, ENTRIES_PER_FRAME as i32);
            0
        } else {
            assert_eq!(final_count, slots, "{name}: the clamp pins the buffer");
            assert_eq!(watermark, slots, "{name}: the watermark hits the ceiling");
            FRAMES * (ENTRIES_PER_FRAME - slots as usize)
        };
        put_ticks(&mut record, format!("buffer_{name}"), &tick_ns);
        record.put(format!("buffer_{name}_slots"), slots);
        record.put(format!("buffer_{name}_dropped"), dropped);
        record.put(format!("buffer_{name}_watermark"), watermark);
    }

    // ---- Export: the same scenario, first 20 frames dump JSON.
    let dir = std::env::temp_dir().join(format!("opt23-buffer-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dump dir");
    let (p, tick_ns) = run_scenario(256 * SLOT_UNIT, Some(dir.to_str().unwrap()));

    // The state machine: active while dumping, completed after count.
    assert_eq!(p.get_json_dump_state(), JsonDumpState::Completed);
    assert!(!p.watcher_get_dump_profile(), "the dump flag clears itself");
    let path = p.get_json_output_file_path();
    assert!(
        path.contains("_profile_") && path.ends_with(".json"),
        "the pinned file-name schema: {path}"
    );
    let raw = std::fs::read(&path).expect("the dump file exists");
    assert_eq!(raw[0], b'[', "the dump opens the trace array");
    assert_eq!(raw[raw.len() - 1], b']', "the stop closes the trace array");
    record.put("export_dump_frames", DUMP_FRAMES);
    record.put("export_file_bytes", raw.len());

    // The overhead split: identical frames before and after the dump
    // boundary. The first sample is the dump-free activation tick, so
    // the dump window is frames 1..=20 and the tail is 21..=100.
    let dump_ticks = &tick_ns[1..=DUMP_FRAMES as usize];
    let tail_ticks = &tick_ns[(DUMP_FRAMES as usize + 1)..];
    let dump_avg = dump_ticks.iter().sum::<u64>() as f64 / dump_ticks.len() as f64;
    let tail_avg = tail_ticks.iter().sum::<u64>() as f64 / tail_ticks.len() as f64;
    put_ticks(&mut record, "export_dump", dump_ticks);
    put_ticks(&mut record, "export_tail", tail_ticks);
    record.put(
        "export_overhead_ratio",
        format!("{:.2}", dump_avg / tail_avg),
    );

    // The failure face: an unwritable dump path fails the state, not
    // the frame (the reference's `createNewJsonDumpFile` null return).
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"x").expect("blocker file");
    let (p2, _) = run_scenario(256 * SLOT_UNIT, Some(blocker.to_str().unwrap()));
    assert_eq!(p2.get_json_dump_state(), JsonDumpState::Failed);
    assert_eq!(p2.get_json_output_file_path(), "", "no path on failure");
    record.put("export_failure_face", 1);

    let _ = std::fs::remove_dir_all(&dir);

    let mut text = String::new();
    for (k, v) in &record.0 {
        text.push_str(&format!("{k}\t{v}\n"));
    }
    print!("{text}");
    if let Ok(path) = std::env::var("BUFFER_EXPORT_OUT") {
        std::fs::write(&path, &text).expect("write BUFFER_EXPORT_OUT");
        println!("record: {path}");
    }
    let _ = std::io::stdout().flush();
}
