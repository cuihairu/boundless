//! OPT-11-01-01: the three-end fixed-camera baseline — frame times and
//! video memory saved.
//!
//! The fixed camera is the fixed presentation parameters
//! (`DeviceConfig::default()`, 640×480 RGBA8) with the scene state as
//! the only thing that changes between frames — so frame N's image is a
//! pure function of the frame index. Each of the three platform ends
//! (linux / windows / macos, the BW-11-04-07 comparability set) runs the
//! identical walk through the shipped device faces (`check_device`,
//! `step`, `present`): the per-frame digests and allocation counts are
//! the saved baseline (asserted equal across ends and across reruns),
//! and the per-frame wall-clock times are the saved frame times
//! (printed for the review doc, never asserted — the machine's
//! numbers, not the face's contract).
//!
//! Video memory: the software analog of the back buffer is the device's
//! own allocation. The walk records the device-creation ledger, the
//! per-frame allocation (zero — present draws and digests the back
//! buffer in place, no upload churn) and the peak live bytes over a
//! whole run (the high-water a frame never raises).
//!
//! ```sh
//! cargo test -p client --test render_baseline_opt
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

use client::device::{Device, DeviceConfig, DeviceStatus};
use client::render::scene::TestScene;
use client::screen_shot::ENDS;

// ---------------------------------------------------------------------------
// The counting allocator with a live high-water (the OPT-10-01-02 variant).
// ---------------------------------------------------------------------------

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            BYTES.with(|b| b.set(b.get() + layout.size()));
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

/// Allocations, bytes and the live high-water of the region, on the
/// calling thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
    peak: usize,
}

fn tracked<T>(body: impl FnOnce() -> T) -> (T, Count) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|b| b.set(0));
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    let count = Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
        peak: PEAK.with(Cell::get),
    };
    (value, count)
}

/// A percentile of a sorted latency list, in nanoseconds.
fn percentile(sorted: &mut [u128], fraction: usize) -> u128 {
    sorted.sort_unstable();
    sorted[sorted.len() * fraction / 100]
}

// ---------------------------------------------------------------------------
// The fixed-camera walk.
// ---------------------------------------------------------------------------

/// Frames in one end's baseline walk.
const FRAMES: u32 = 64;

/// The saved fixed-camera baseline (the acceptance's record): the frame
/// digest at indices 1..=8 and 64, then the whole-run memory ledger —
/// two allocations totalling the 640×480 RGBA8 back buffer plus the
/// 48-byte device-counter allocation, which is also the run's peak.
/// Any change to the raster, the scene or the device's memory shows up
/// here; OPT-11-01-04's rerun walks the same pins.
const BASELINE_DIGESTS: [u64; 9] = [
    0x69cb_1245_6d1a_e325,
    0x228c_7e64_89e2_e325,
    0xa645_7f3f_bc5c_6325,
    0x4ea0_a845_350b_6325,
    0x6e6b_e175_60a8_e325,
    0x0842_51ac_435d_e325,
    0x2e95_89ed_046c_6325,
    0x93cf_bf1f_9eb1_e325,
    0x6e8d_a4fa_c38f_6325, // frame 64
];
const BASELINE_RUN_ALLOCATIONS: usize = 2;
const BASELINE_RUN_BYTES: usize = 1_228_848; // 640*480*4 + 48

/// One frame's baseline record: the presented digest, the present
/// region's allocations and its latency.
struct FrameRecord {
    index: u64,
    checksum: u64,
    ns: u128,
    allocs: usize,
    bytes: usize,
}

/// One end's fixed-camera walk: the device created in its own tracked
/// region (the video-memory creation ledger), then FRAMES frames of
/// `check_device` → `step` → `present`, each present in its own tracked
/// region with a wall-clock around it. The end label only names the
/// row: every end runs the identical path, which is what makes the
/// baselines comparable.
fn end_walk() -> (Device, Vec<FrameRecord>, Count) {
    let config = DeviceConfig::default();
    let (mut device, creation) = tracked(|| Device::create(config));
    let mut scene = TestScene::new();

    let mut records = Vec::with_capacity(FRAMES as usize);
    for _ in 0..FRAMES {
        assert!(
            device.check_device(),
            "the fixed-camera walk keeps the device operational"
        );
        scene.step();
        let started = Instant::now();
        let (presented, count) = tracked(|| device.present(&scene));
        let ns = started.elapsed().as_nanos();
        records.push(FrameRecord {
            index: presented.frame_index,
            checksum: presented.checksum,
            ns,
            allocs: count.allocs,
            bytes: count.bytes,
        });
    }
    (device, records, creation)
}

/// One end's whole-run memory ledger: creation plus every frame inside
/// a single tracked region, so the peak is the run's live high-water.
/// Returns the back-buffer width and the ledger.
fn end_memory_walk() -> (u32, Count) {
    let (((width, _height), _frames), total) = tracked(|| {
        let mut device = Device::create(DeviceConfig::default());
        let mut scene = TestScene::new();
        for _ in 0..FRAMES {
            if device.check_device() {
                scene.step();
                device.present(&scene);
            }
        }
        (device.back_buffer(), device.frame_count())
    });
    assert!(total.allocs > 0, "the run allocated its back buffer");
    (width, total)
}

/// Frame N's digest from the frame index alone: a fresh device and
/// scene stepped `steps` times, presented once — the fixed-camera
/// regression anchor.
fn seek_checksum(steps: u32) -> u64 {
    let mut device = Device::create(DeviceConfig::default());
    let mut scene = TestScene::new();
    for _ in 0..steps {
        scene.step();
    }
    device.present(&scene).checksum
}

#[test]
fn the_three_ends_record_one_fixed_camera_baseline() {
    let config = DeviceConfig::default();
    let back_buffer_bytes = (config.area() * 4) as usize;
    assert_eq!(config.width, 640);
    assert_eq!(config.height, 480);

    let mut baselines = Vec::with_capacity(ENDS.len());
    for end in ENDS {
        let (device, records, creation) = end_walk();
        let (width, total) = end_memory_walk();

        // the walk is the fixed camera's: every frame presents, in
        // order, on one device object
        assert_eq!(records.len(), FRAMES as usize, "{end}: every frame ran");
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.index, index as u64 + 1, "{end}: the frame order");
        }
        assert_eq!(device.frame_count(), FRAMES as u64, "{end}: frames counted");
        assert_eq!(
            device.status(),
            DeviceStatus::Ok,
            "{end}: device operational"
        );
        assert_eq!(device.leaked(), 0, "{end}: no device leaked");

        // video memory: the frames allocate nothing — present draws and
        // digests the back buffer in place — so the whole-run ledger is
        // the creation ledger itself, and the peak is that footprint,
        // never raised by a frame
        for (index, record) in records.iter().enumerate() {
            assert_eq!(
                (record.allocs, record.bytes),
                (0, 0),
                "{end}: frame {index} allocates nothing"
            );
        }
        assert_eq!(width, config.width, "{end}: the back buffer survives");
        assert_eq!(
            total.allocs, creation.allocs,
            "{end}: the run's allocations are the device's own"
        );
        assert_eq!(
            total.bytes, creation.bytes,
            "{end}: the run's bytes are the device's own"
        );
        assert_eq!(
            total.peak, total.bytes,
            "{end}: the high-water is the creation footprint"
        );
        assert!(
            creation.bytes >= back_buffer_bytes,
            "{end}: the device carries the back buffer ({} bytes)",
            creation.bytes
        );

        // the scene actually animates under the fixed camera: every
        // consecutive frame differs
        for pair in records.windows(2) {
            assert_ne!(
                pair[0].checksum, pair[1].checksum,
                "{end}: consecutive frames differ"
            );
        }

        // frame content is a pure function of the frame index
        for &frame in &[1usize, 7, 31, FRAMES as usize] {
            assert_eq!(
                seek_checksum(frame as u32),
                records[frame - 1].checksum,
                "{end}: frame {frame} reproduces from the index alone"
            );
        }

        println!(
            "MEM end={end} creation_allocs={} creation_bytes={} back_buffer_bytes={back_buffer_bytes} run_peak={} frames_allocs=0",
            creation.allocs, creation.bytes, total.peak
        );
        baselines.push(records);
    }

    // the three ends' baselines agree digest for digest — the
    // comparability the screenshot face established, per frame
    let baseline = &baselines[0];
    for records in &baselines[1..] {
        assert_eq!(records.len(), baseline.len());
        for (index, (a, b)) in baseline.iter().zip(records.iter()).enumerate() {
            assert_eq!(a.checksum, b.checksum, "frame {index}: the ends agree");
            assert_eq!(a.allocs, b.allocs, "frame {index}: the ends pay the same");
        }
    }

    // the saved frame times (probe data: printed, never asserted)
    for (end, records) in ENDS.iter().zip(&baselines) {
        for record in records {
            println!(
                "FRAME-TIME end={end} frame={} checksum={:016x} ns={}",
                record.index, record.checksum, record.ns
            );
        }
        let mut ns: Vec<u128> = records.iter().map(|r| r.ns).collect();
        println!(
            "FRAME-TAIL end={end} p50_us={:.3} p99_us={:.3} max_us={:.3}",
            percentile(&mut ns, 50) as f64 / 1_000.0,
            percentile(&mut ns, 99) as f64 / 1_000.0,
            ns[records.len() - 1] as f64 / 1_000.0,
        );
    }
}

#[test]
fn the_baseline_reruns_to_the_same_ledger() {
    let (_, first, creation) = end_walk();
    let (_, second, _) = end_walk();
    assert_eq!(first.len(), second.len());
    for (index, (a, b)) in first.iter().zip(second.iter()).enumerate() {
        assert_eq!(a.checksum, b.checksum, "frame {index}: the digest reruns");
        assert_eq!(a.allocs, b.allocs, "frame {index}: the ledger reruns");
        assert_eq!(a.bytes, b.bytes, "frame {index}: the bytes rerun");
    }
    assert_eq!(creation.allocs, 2, "the device creation ledger is stable");

    let (_, first_total) = end_memory_walk();
    let (_, second_total) = end_memory_walk();
    assert_eq!(first_total, second_total, "the memory ledger reruns");

    // the saved baseline: the frame digests of the first eight frames
    // and the last — the visual-regression anchor OPT-11-01-04 reruns —
    // and the run memory ledger they ride on
    let pinned: Vec<u64> = first
        .iter()
        .take(8)
        .chain(first.last())
        .map(|r| r.checksum)
        .collect();
    assert_eq!(pinned, BASELINE_DIGESTS, "the saved digests");
    assert_eq!(
        (first_total.allocs, first_total.bytes, first_total.peak),
        (
            BASELINE_RUN_ALLOCATIONS,
            BASELINE_RUN_BYTES,
            BASELINE_RUN_BYTES
        ),
        "the saved memory ledger"
    );
    println!("BASELINE-CHECKSUMS {pinned:016x?}");
    println!(
        "MEM-RERUN run_allocs={} run_bytes={} run_peak={}",
        first_total.allocs, first_total.bytes, first_total.peak
    );
}
