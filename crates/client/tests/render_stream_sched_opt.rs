//! OPT-11-01-03: resource streaming and frame scheduling, evaluated —
//! the chunk switches and the long frames reported.
//!
//! The fixed-camera walk (the OPT-11-01-01 baseline configuration,
//! 640×480, 64 frames) is driven through the shipped frame-scheduling
//! face — `FrameLoop::run_frame` firing the update, animation and draw
//! phases in the reference's order with the script hooks in phase —
//! and six scheduled chunk switches stream the reference fixture map
//! into a live `ChunkSpace` through the shipped `load_and_bind` face
//! (37 candidates split across the six switch frames, the way an app
//! streams a batch of chunks per frame). The frame ledger records, per
//! frame, the wall-clock, the allocation count and the live-bytes
//! high-water mark after the frame; the chunk switches are the frames
//! that allocate (asserted: every switch frame allocates strictly more
//! than every steady frame), and the long frames are reported from the
//! same ledger — magnitudes are probe data, printed for the review doc,
//! never asserted.
//!
//! ```sh
//! cargo test -p client --test render_stream_sched_opt
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chunk::{ChunkSpace, GeometryMapping};
use client::device::{Device, DeviceConfig};
use client::frame::{FrameLoop, FramePhase, PhaseHook, is_canonical_order};
use client::render::scene::TestScene;
use client::screen_shot::ENDS;
use foundation::math::Vector3;
use resources::datasection;

// ---------------------------------------------------------------------------
// The counting allocator with a live high-water (the OPT-10-01-02 variant),
// read as deltas over one long region so per-frame counts and the live
// trajectory come out of a single tracked walk.
// ---------------------------------------------------------------------------

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            LIVE.with(|l| {
                let live = l.get() + layout.size() as isize;
                l.set(live);
                if live > 0 && live as usize > PEAK.with(Cell::get) {
                    PEAK.with(|p| p.set(live as usize));
                }
            });
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACKING.with(Cell::get) {
            LIVE.with(|l| l.set(l.get() - layout.size() as isize));
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn walk_begin() {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
}

fn walk_end() -> (usize, usize) {
    TRACKING.with(|t| t.set(false));
    (ALLOCS.with(Cell::get), PEAK.with(Cell::get))
}

fn live_now() -> usize {
    LIVE.with(|l| l.get().max(0) as usize)
}

// ---------------------------------------------------------------------------
// The fixture map and the streaming faces (the BW-10-07-03/OPT-10-01-01
// wiring).
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("a crates/ parent")
        .parent()
        .expect("the repository root")
        .to_path_buf()
}

fn space_root() -> PathBuf {
    repo_root().join("docs/review/fixtures/chunk_space/res/space_00010001")
}

fn load_mapping() -> GeometryMapping {
    let raw = std::fs::read(space_root().join("settings.xml")).expect("settings.xml readable");
    let settings = datasection::parse("", &raw).expect("settings.xml parses");
    GeometryMapping::from_settings("space_00010001/", &settings)
}

/// The raw streaming resolver: read + parse per candidate — the
/// baseline face; the cached resolver's trade is the OPT-10-01-02 row's.
fn raw_resolver(root: PathBuf) -> impl Fn(&str) -> Option<datasection::DataSection> {
    move |name: &str| {
        let raw = std::fs::read(root.join(name)).ok()?;
        datasection::parse("", &raw).ok()
    }
}

/// The whole candidate set in scan order, and the chunks that bind when
/// every candidate is offered at once (the probe the streaming walk's
/// union is checked against).
fn candidates_and_probe() -> (Vec<String>, Vec<String>) {
    let mapping = load_mapping();
    let candidates: Vec<String> = mapping.gather_chunks(&space_root()).into_iter().collect();
    let mut space = ChunkSpace::new(1, vec![load_mapping()]);
    let resolver = raw_resolver(space_root());
    let bound = space.load_and_bind(&candidates, &resolver);
    (candidates, bound)
}

// ---------------------------------------------------------------------------
// The scheduled streaming walk.
// ---------------------------------------------------------------------------

/// Frames in one end's walk.
const FRAMES: usize = 64;

/// The frames that carry a chunk switch (a streaming batch).
const SWITCH_FRAMES: [usize; 6] = [8, 16, 24, 32, 40, 48];

/// The candidate-group edges per switch (37 candidates split across the
/// six switches).
const GROUP_EDGES: [usize; 7] = [0, 7, 13, 19, 25, 31, 37];

/// One frame's ledger row.
struct StreamFrame {
    index: usize,
    ns: u128,
    allocs: usize,
    live_after: usize,
    bound: Vec<String>,
}

struct StreamLedger {
    frames: Vec<StreamFrame>,
    total_allocs: usize,
    peak: usize,
}

/// One end's scheduled streaming walk: the frame scheduler drives every
/// frame; the switch frames stream their candidate group into the live
/// space. One tracked region spans the whole walk so the per-frame
/// allocation counts are deltas and the live trajectory is continuous.
fn stream_walk() -> StreamLedger {
    let (candidates, _probe) = candidates_and_probe();
    assert_eq!(candidates.len(), 37, "the fixture map's scan order");

    walk_begin();
    let resolver = raw_resolver(space_root());
    let mut space = ChunkSpace::new(1, vec![load_mapping()]);
    let mut scheduler = FrameLoop::new();
    scheduler.add_hook(PhaseHook {
        name: "onTick",
        phase: FramePhase::Tick,
    });
    scheduler.add_hook(PhaseHook {
        name: "onDrawFrame",
        phase: FramePhase::Draw,
    });
    let mut device = Device::create(DeviceConfig::default());
    let mut scene = TestScene::new();

    let mut frames = Vec::with_capacity(FRAMES);
    let mut switch_index = 0usize;
    for frame in 0..FRAMES {
        let before = ALLOCS.with(Cell::get);
        let started = Instant::now();

        let log = scheduler.run_frame(true, true);
        let mut bound = Vec::new();
        if switch_index < SWITCH_FRAMES.len() && frame + 1 == SWITCH_FRAMES[switch_index] {
            let group = &candidates[GROUP_EDGES[switch_index]..GROUP_EDGES[switch_index + 1]];
            bound = space.load_and_bind(group, &resolver);
            switch_index += 1;
        }
        assert!(
            device.check_device(),
            "frame {frame}: the device stays operational"
        );
        scene.step();
        device.present(&scene);
        let ns = started.elapsed().as_nanos();

        // the scheduling face: the canonical active frame, hooks in
        // phase order, every frame
        assert_eq!(
            log.phases,
            vec![
                FramePhase::Tick,
                FramePhase::UpdateAnimations,
                FramePhase::Draw
            ],
            "frame {frame}: the canonical active frame"
        );
        assert!(is_canonical_order(&log), "frame {frame}: the phase order");
        assert_eq!(
            log.hooks,
            vec!["onTick", "onDrawFrame"],
            "frame {frame}: the hooks fire in phase"
        );

        frames.push(StreamFrame {
            index: frame + 1,
            ns,
            allocs: ALLOCS.with(Cell::get) - before,
            live_after: live_now(),
            bound,
        });
    }
    let (total_allocs, peak) = walk_end();

    assert_eq!(switch_index, SWITCH_FRAMES.len(), "every switch ran");
    assert_eq!(
        scheduler.frame_count(),
        FRAMES as u64,
        "the schedule ran out"
    );
    assert_eq!(device.frame_count(), FRAMES as u64, "every frame presented");

    StreamLedger {
        frames,
        total_allocs,
        peak,
    }
}

#[test]
fn the_stream_switches_land_on_their_scheduled_frames() {
    let (_candidates, probe) = candidates_and_probe();
    assert_eq!(probe.len(), 6, "the probe binds six chunks");

    for end in ENDS {
        let ledger = stream_walk();

        // the chunk switches landed exactly on their scheduled frames,
        // and their union binds the probe's whole set
        let mut union = Vec::new();
        for frame in SWITCH_FRAMES {
            let record = &ledger.frames[frame - 1];
            assert_eq!(record.index, frame, "{end}: the switch frame");
            union.extend(record.bound.iter().cloned());
        }
        union.sort();
        let mut expected = probe.clone();
        expected.sort();
        assert_eq!(union, expected, "{end}: the switches bind the whole set");

        // the streaming frames are where the allocations land: every
        // switch frame allocates strictly more than every steady frame
        let steady_max = ledger
            .frames
            .iter()
            .filter(|f| !SWITCH_FRAMES.contains(&f.index))
            .map(|f| f.allocs)
            .max()
            .expect("steady frames");
        for frame in SWITCH_FRAMES {
            assert!(
                ledger.frames[frame - 1].allocs > steady_max,
                "{end}: switch frame {frame} allocates more than every steady frame \
                 ({} vs {steady_max})",
                ledger.frames[frame - 1].allocs
            );
        }

        // per-frame ledger, switch ledger, long-frame report, memory
        for record in &ledger.frames {
            println!(
                "STREAM-FRAME end={end} frame={} ns={} allocs={} live={}",
                record.index, record.ns, record.allocs, record.live_after
            );
        }
        for (order, frame) in SWITCH_FRAMES.iter().enumerate() {
            let record = &ledger.frames[*frame - 1];
            println!(
                "STREAM-SWITCH end={end} switch={} frame={} ns={} allocs={} live_after={} bound={}",
                order + 1,
                record.index,
                record.ns,
                record.allocs,
                record.live_after,
                record.bound.join("+")
            );
        }
        let mut by_time: Vec<&StreamFrame> = ledger.frames.iter().collect();
        by_time.sort_by_key(|f| std::cmp::Reverse(f.ns));
        for record in by_time.iter().take(5) {
            println!(
                "LONG-FRAME end={end} frame={} ns={} allocs={} switched={}",
                record.index,
                record.ns,
                record.allocs,
                SWITCH_FRAMES.contains(&record.index)
            );
        }
        println!(
            "MEM end={end} total_allocs={} peak={} final_live={}",
            ledger.total_allocs,
            ledger.peak,
            ledger.frames.last().expect("frames").live_after
        );

        // the streamed space is live: every exterior chunk the switches
        // bound resolves its own cell centre. The sixth bound chunk is
        // the fixture's separator (`…/sep/…`) — it has no grid cell, so
        // the centre walk covers the four hex-grid exterior chunks.
        let mut space = ChunkSpace::new(1, vec![load_mapping()]);
        let resolver = raw_resolver(space_root());
        let bound = space.load_and_bind(&probe, &resolver);
        assert_eq!(bound.len(), 6, "{end}: the probe rebinds");
        let grid_chunks: Vec<&String> = bound
            .iter()
            .filter(|id| id.len() == 9 && id.ends_with('o'))
            .collect();
        assert_eq!(grid_chunks.len(), 4, "{end}: the four exterior cells");
        for id in grid_chunks {
            let gx = i32::from_str_radix(&id[0..4], 16).expect("grid x");
            let gz = i32::from_str_radix(&id[4..8], 16).expect("grid z");
            let found = space
                .find_chunk_from_point(&Vector3::new(
                    (gx * 100 + 50) as f32,
                    0.0,
                    (gz * 100 + 50) as f32,
                ))
                .expect("{end}: the centre resolves")
                .identifier
                .clone();
            assert_eq!(found, *id, "{end}: the streamed chunk owns its cell");
        }
    }
}

#[test]
fn the_stream_walk_reruns_to_the_same_ledger() {
    let first = stream_walk();
    let second = stream_walk();
    assert_eq!(first.frames.len(), second.frames.len());
    for (a, b) in first.frames.iter().zip(second.frames.iter()) {
        assert_eq!(a.index, b.index);
        assert_eq!(a.allocs, b.allocs, "frame {}: the ledger reruns", a.index);
        assert_eq!(a.bound, b.bound, "frame {}: the switches rerun", a.index);
    }
    assert_eq!(first.total_allocs, second.total_allocs);
    assert_eq!(first.peak, second.peak, "the high-water reruns");
    println!(
        "STREAM-RERUN frames={} total_allocs={} peak={} switches_all_bound=true",
        first.frames.len(),
        first.total_allocs,
        first.peak
    );
}
