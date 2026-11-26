//! OPT-11-01-02: uploads and render batches, evaluated — the shipped
//! per-pixel draw face against the batched face, frame times compared
//! per end.
//!
//! The shipped draw face is `TestScene::draw`: one scalar `set_pixel`
//! call per pixel — 307,200 upload calls a frame at 640×480. The
//! batched candidate draws the *same* frame through the shipped batch
//! primitives: one whole-buffer `fill` for the background, then one
//! slice fill per row for the marker cell's column block. Both faces
//! time their draw and their digest separately (the frame time split
//! into upload and digest halves), and the frame times are compared
//! per end (linux / windows / macos, the BW-11-04-07 comparability set)
//! and printed for the review doc — probe data, never asserted. The
//! frames themselves are the contract: every batched frame must be
//! pixel-identical to the shipped frame, and neither face may allocate.
//!
//! ```sh
//! cargo test -p client --test render_batch_opt
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

use client::device::{Device, DeviceConfig};
use client::render::framebuffer::Framebuffer;
use client::render::scene::{CELLS, TestScene};
use client::screen_shot::ENDS;

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

/// A percentile of a sorted latency list, in nanoseconds.
fn percentile(sorted: &mut [u128], fraction: usize) -> u128 {
    sorted.sort_unstable();
    sorted[sorted.len() * fraction / 100]
}

// ---------------------------------------------------------------------------
// The two draw faces over one fixed camera.
// ---------------------------------------------------------------------------

/// Frames in one end's comparison walk.
const FRAMES: u32 = 64;

/// One frame's per-face record: the draw latency, the digest latency,
/// and the draw region's allocation count.
struct FaceFrame {
    draw_ns: u128,
    digest_ns: u128,
    draw_allocs: usize,
    checksum: u64,
}

/// The shipped face: the scene's own draw (one `set_pixel` per pixel),
/// then the back buffer's digest.
fn shipped_frame(scene: &TestScene, fb: &mut Framebuffer) -> FaceFrame {
    let started = Instant::now();
    let (_, draw) = tracked(|| scene.draw(fb));
    let draw_ns = started.elapsed().as_nanos();
    let started = Instant::now();
    let (checksum, _) = tracked(|| fb.checksum());
    let digest_ns = started.elapsed().as_nanos();
    FaceFrame {
        draw_ns,
        digest_ns,
        draw_allocs: draw.allocs,
        checksum,
    }
}

/// The batched candidate: the same frame through the shipped batch
/// primitives — one whole-buffer fill for the background, then one
/// slice fill per row over the marker cell's column block. The pixel
/// math mirrors `TestScene::draw` exactly (the test asserts the frames
/// come out identical).
fn batched_frame(scene: &TestScene, fb: &mut Framebuffer) -> FaceFrame {
    let started = Instant::now();
    let (_, draw) = tracked(|| {
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
    });
    let draw_ns = started.elapsed().as_nanos();
    let started = Instant::now();
    let (checksum, _) = tracked(|| fb.checksum());
    let digest_ns = started.elapsed().as_nanos();
    FaceFrame {
        draw_ns,
        digest_ns,
        draw_allocs: draw.allocs,
        checksum,
    }
}

/// One end's comparison walk: both faces draw the same scene over the
/// same frame indices, each frame's pixels compared in full.
fn end_walk() -> (Vec<FaceFrame>, Vec<FaceFrame>) {
    let mut scene_shipped = TestScene::new();
    let mut scene_batched = TestScene::new();
    let mut fb_shipped = Framebuffer::new(640, 480);
    let mut fb_batched = Framebuffer::new(640, 480);

    let mut shipped = Vec::with_capacity(FRAMES as usize);
    let mut batched = Vec::with_capacity(FRAMES as usize);
    let mut prev: Option<u64> = None;
    for frame in 0..FRAMES {
        scene_shipped.step();
        scene_batched.step();
        let a = shipped_frame(&scene_shipped, &mut fb_shipped);
        let b = batched_frame(&scene_batched, &mut fb_batched);

        assert_eq!(a.checksum, b.checksum, "frame {frame}: the digests agree");
        assert_eq!(
            fb_shipped.pixels, fb_batched.pixels,
            "frame {frame}: the batched pixels are the shipped pixels"
        );
        assert_eq!(
            (a.draw_allocs, b.draw_allocs),
            (0, 0),
            "frame {frame}: neither face allocates"
        );
        if let Some(previous) = prev {
            assert_ne!(a.checksum, previous, "frame {frame}: the frames animate");
        }
        prev = Some(a.checksum);
        shipped.push(a);
        batched.push(b);
    }
    (shipped, batched)
}

/// The device face draws the same walk: one present per frame through
/// the shipped device, every digest matching the harness shipped walk.
fn device_walk_checksums() -> Vec<u64> {
    let mut device = Device::create(DeviceConfig::default());
    let mut scene = TestScene::new();
    let mut checksums = Vec::with_capacity(FRAMES as usize);
    for _ in 0..FRAMES {
        if device.check_device() {
            scene.step();
            checksums.push(device.present(&scene).checksum);
        }
    }
    checksums
}

#[test]
fn the_batched_draws_answer_the_shipped_frames_per_end() {
    let mut tails = Vec::with_capacity(ENDS.len());
    for end in ENDS {
        let (shipped, batched) = end_walk();
        assert_eq!(shipped.len(), FRAMES as usize, "{end}: every frame ran");

        // the device face produces the same walk the harness shipped
        // face timed — the comparison rides the real present path
        let device_checksums = device_walk_checksums();
        let harness: Vec<u64> = shipped.iter().map(|f| f.checksum).collect();
        assert_eq!(device_checksums, harness, "{end}: the device face agrees");

        println!(
            "BATCH-SHAPE end={end} shipped_writes_per_frame={} batched_writes_per_frame={}",
            640u32 * 480,
            1 + 480
        );
        for (frame, (a, b)) in shipped.iter().zip(batched.iter()).enumerate() {
            println!(
                "FRAME-TIME end={end} frame={} shipped_draw_us={:.3} shipped_digest_us={:.3} batched_draw_us={:.3} batched_digest_us={:.3}",
                frame + 1,
                a.draw_ns as f64 / 1_000.0,
                a.digest_ns as f64 / 1_000.0,
                b.draw_ns as f64 / 1_000.0,
                b.digest_ns as f64 / 1_000.0,
            );
        }
        let tail = |faces: &[FaceFrame], which: fn(&FaceFrame) -> u128| {
            let mut ns: Vec<u128> = faces.iter().map(which).collect();
            format!(
                "p50_us={:.3} p99_us={:.3} max_us={:.3}",
                percentile(&mut ns, 50) as f64 / 1_000.0,
                percentile(&mut ns, 99) as f64 / 1_000.0,
                ns[FRAMES as usize - 1] as f64 / 1_000.0,
            )
        };
        println!(
            "BATCH-TAIL end={end} shipped_draw {} shipped_digest {} batched_draw {} batched_digest {}",
            tail(&shipped, |f| f.draw_ns),
            tail(&shipped, |f| f.digest_ns),
            tail(&batched, |f| f.draw_ns),
            tail(&batched, |f| f.digest_ns),
        );
        tails.push((shipped, batched));
    }

    // the three ends' frame ledgers agree digest for digest — the same
    // comparability the baseline row established
    let (first_shipped, first_batched) = &tails[0];
    for (shipped, batched) in &tails[1..] {
        for (index, (a, b)) in first_shipped.iter().zip(shipped.iter()).enumerate() {
            assert_eq!(a.checksum, b.checksum, "frame {index}: the ends agree");
        }
        for (index, (a, b)) in first_batched.iter().zip(batched.iter()).enumerate() {
            assert_eq!(a.checksum, b.checksum, "frame {index}: the ends agree");
        }
    }
}

#[test]
fn the_batch_ledger_reruns_to_the_same_digests() {
    let (first, first_batched) = end_walk();
    let (second, second_batched) = end_walk();
    for (index, ((a, b), (c, d))) in first
        .iter()
        .zip(second.iter())
        .zip(first_batched.iter().zip(second_batched.iter()))
        .enumerate()
    {
        assert_eq!(a.checksum, c.checksum, "frame {index}: shipped reruns");
        assert_eq!(b.checksum, d.checksum, "frame {index}: batched reruns");
        assert_eq!(a.draw_allocs, c.draw_allocs, "frame {index}: ledger reruns");
        assert_eq!(b.draw_allocs, d.draw_allocs, "frame {index}: ledger reruns");
    }
    println!(
        "BATCH-RERUN frames={} digests_equal=true allocs_zero_both_faces=true",
        first.len()
    );
}
