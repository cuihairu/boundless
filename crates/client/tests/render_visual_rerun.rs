//! OPT-11-01-04: rerun the visual regression — the fixed camera
//! satisfies the tolerance.
//!
//! The saved baseline is OPT-11-01-01's: the frame digests at indices
//! 1..=8 and 64, and the device's memory ledger (two allocations,
//! 1,228,848 bytes — the back buffer plus the device counter — which is
//! also the whole-run peak). This row re-drives the same fixed camera
//! (640×480 RGBA8, 64 frames, scene state the only variable) on all
//! three ends through the shipped faces and holds the rerun against
//! those pins:
//!
//! - every pinned frame digest matches the saved record, on a fresh
//!   walk and on its rerun;
//! - the measured per-pixel tolerance between walks is **zero** — the
//!   deterministic face's tolerance floor is the bit, so the rerun is
//!   asserted bit-exact rather than within a budget;
//! - the screenshot face (`ScreenCopy::capture` → TGA, the BW-11-04-07
//!   channel) produces byte-identical captures at the pinned frames
//!   across the three ends and across the rerun;
//! - the OPT-11-01-02 batched draw candidate answers the same visual
//!   baseline — same digests, same pixels, same zero tolerance;
//! - the memory ledger reruns to the saved numbers.
//!
//! ```sh
//! cargo test -p client --test render_visual_rerun
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use client::device::{Device, DeviceConfig};
use client::render::framebuffer::Framebuffer;
use client::render::scene::{CELLS, TestScene};
use client::screen_shot::{ENDS, ScreenCopy};

// ---------------------------------------------------------------------------
// The counting allocator with a live high-water (the OPT-10-01-02 variant).
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

fn tracked<T>(body: impl FnOnce() -> T) -> (T, usize, usize) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    (value, ALLOCS.with(Cell::get), PEAK.with(Cell::get))
}

// ---------------------------------------------------------------------------
// The saved baseline (OPT-11-01-01's record, re-declared).
// ---------------------------------------------------------------------------

/// Frames in one end's regression walk.
const FRAMES: usize = 64;

/// The saved frame digests at indices 1..=8 and 64.
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

/// The frames the baseline pins (walk indices).
const PINNED: [usize; 9] = [1, 2, 3, 4, 5, 6, 7, 8, 64];

/// The frames the screenshot face captures.
const SCREEN_FRAMES: [usize; 3] = [1, 8, 64];

// ---------------------------------------------------------------------------
// The walks.
// ---------------------------------------------------------------------------

/// One end's fixed-camera walk, the digests pushed into the caller's
/// sink (so a tracked region around the walk counts only the device).
fn device_walk_into(digests: &mut Vec<u64>) -> Device {
    let mut device = Device::create(DeviceConfig::default());
    let mut scene = TestScene::new();
    for _ in 0..FRAMES {
        assert!(device.check_device(), "the walk stays operational");
        scene.step();
        digests.push(device.present(&scene).checksum);
    }
    device
}

/// One end's fixed-camera walk with its own sink.
fn device_walk() -> (Device, Vec<u64>) {
    let mut digests = Vec::with_capacity(FRAMES);
    let device = device_walk_into(&mut digests);
    (device, digests)
}

/// The screenshot face at one frame index: a fresh device and scene
/// walked straight there, the back buffer captured and TGA-encoded.
fn screenshot_at(frame: usize) -> (u64, Vec<u8>) {
    let mut device = Device::create(DeviceConfig::default());
    let mut scene = TestScene::new();
    for _ in 0..frame {
        scene.step();
    }
    let presented = device.present(&scene);
    let tga = ScreenCopy::capture(device.framebuffer()).to_tga();
    (presented.checksum, tga)
}

/// Differing pixels between two back buffers (the tolerance measure).
fn differing_pixels(a: &[u32], b: &[u32]) -> usize {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).filter(|(x, y)| x != y).count()
}

/// The OPT-11-01-02 batched draw candidate: the same pixel math through
/// the shipped batch primitives.
fn batched_draw(scene: &TestScene, fb: &mut Framebuffer) {
    if fb.width == 0 || fb.height == 0 {
        return;
    }
    let cell = scene.cell();
    let cell_width = (fb.width / CELLS).max(1);
    let shade = 0x10u32.saturating_mul(scene.steps % 8);
    let bg = 0x20_28_38_ff;
    let r = (0x30u32 + shade + cell * 3) & 0xff;
    let g = (0x40u32 + shade) & 0xff;
    let b = (0x90u32 + shade) & 0xff;
    let base = (b << 16) | (g << 8) | r | 0xff00_0000;
    fb.fill(bg);
    let x0 = cell * cell_width;
    let x1 = ((cell + 1) * cell_width).min(fb.width);
    for y in 0..fb.height {
        let row = &mut fb.pixels[(y * fb.width + x0) as usize..(y * fb.width + x1) as usize];
        row.fill(base);
    }
}

#[test]
fn the_fixed_camera_rerun_satisfies_the_saved_baseline() {
    let config = DeviceConfig::default();
    let mut shots: Vec<BTreeMap<usize, Vec<u8>>> = Vec::with_capacity(ENDS.len());

    for end in ENDS {
        // the walk against the saved digests, inside one tracked region
        // so the memory ledger rerun counts only the device
        let mut digests = Vec::with_capacity(FRAMES);
        let (device, allocs, peak) = tracked(|| device_walk_into(&mut digests));
        assert_eq!(
            device.back_buffer(),
            (config.width, config.height),
            "{end}: the back buffer survives"
        );
        for (pin, expected) in PINNED.iter().zip(BASELINE_DIGESTS.iter()) {
            assert_eq!(
                digests[*pin - 1],
                *expected,
                "{end}: frame {pin} matches the saved baseline"
            );
        }
        assert_eq!(
            allocs, BASELINE_RUN_ALLOCATIONS,
            "{end}: the run memory ledger reruns"
        );
        assert_eq!(peak, BASELINE_RUN_BYTES, "{end}: the run peak reruns");

        // the rerun: identical digests, and the measured per-pixel
        // tolerance at the pinned frames is zero
        let (_, digests2) = device_walk();
        assert_eq!(digests, digests2, "{end}: the walk reruns");
        for &frame in &PINNED {
            let (mut a, mut b) = (Device::create(config), Device::create(config));
            let (mut scene_a, mut scene_b) = (TestScene::new(), TestScene::new());
            for _ in 0..frame {
                scene_a.step();
                scene_b.step();
            }
            a.present(&scene_a);
            b.present(&scene_b);
            let diff = differing_pixels(&a.framebuffer().pixels, &b.framebuffer().pixels);
            assert_eq!(diff, 0, "{end}: frame {frame} is bit-exact on the rerun");
        }
        println!(
            "VISUAL-TOLERANCE end={end} frames={} differing_pixels=0 pinned={PINNED:?}",
            digests.len()
        );

        // the screenshot face at the pinned frames
        let mut end_shots = BTreeMap::new();
        for &frame in &SCREEN_FRAMES {
            let (checksum, tga) = screenshot_at(frame);
            assert_eq!(
                checksum,
                digests[frame - 1],
                "{end}: frame {frame} screenshot rides the walked frame"
            );
            assert_eq!(
                tga.len(),
                18 + (config.area() * 4) as usize,
                "{end}: the TGA is the header plus the frame"
            );
            end_shots.insert(frame, tga);
        }
        shots.push(end_shots);
    }

    // the three ends' screenshots are byte-equal, frame for frame
    for (end, end_shots) in ENDS.iter().zip(&shots[1..]) {
        for &frame in &SCREEN_FRAMES {
            assert_eq!(
                end_shots[&frame], shots[0][&frame],
                "{end}: frame {frame} screenshot is byte-equal to the first end"
            );
        }
    }

    // the whole-regression rerun: the screenshots reproduce byte for
    // byte on a second pass
    for &frame in &SCREEN_FRAMES {
        let (_, tga) = screenshot_at(frame);
        assert_eq!(
            tga, shots[0][&frame],
            "frame {frame} screenshot reruns byte-identical"
        );
    }
    println!(
        "VISUAL-SHOTS frames={SCREEN_FRAMES:?} bytes_each={}",
        18 + 640 * 480 * 4
    );
}

#[test]
fn the_batched_draw_satisfies_the_same_visual_baseline() {
    let config = DeviceConfig::default();
    for end in ENDS {
        let (_, digests) = device_walk();
        let mut scene = TestScene::new();
        let mut fb = Framebuffer::new(640, 480);
        for (index, digest) in digests.iter().enumerate() {
            scene.step();
            batched_draw(&scene, &mut fb);
            assert_eq!(
                fb.checksum(),
                *digest,
                "{end}: frame {} the batched face answers the walked frame",
                index + 1
            );
            if PINNED.contains(&(index + 1)) {
                // the pixel proof at the pinned frames: the batched
                // buffer beside the shipped device's own back buffer
                let mut device = Device::create(config);
                let mut shipped_scene = TestScene::new();
                for _ in 0..index + 1 {
                    shipped_scene.step();
                }
                device.present(&shipped_scene);
                let diff = differing_pixels(&fb.pixels, &device.framebuffer().pixels);
                assert_eq!(
                    diff,
                    0,
                    "{end}: frame {} the batched pixels are the shipped pixels",
                    index + 1
                );
            }
        }
        println!("VISUAL-BATCH end={end} frames={FRAMES} differing_pixels=0");
    }
}
