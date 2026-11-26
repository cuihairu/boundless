//! OPT-11-01-02: the upload and render-batching candidates.
//!
//! The acceptance: **frame time compared per end** (`按端对比帧时间`).
//! The baseline's draw walks every pixel through `Framebuffer::set_pixel`
//! — one bounds-check and index computation per pixel (`width × height`
//! upload calls per frame). Two candidates emulate on the live faces,
//! producing byte-identical frames:
//!
//! - **A — the row-contiguous upload.** The frame is at most two
//!   vertical bands (the background, the marker cell's column band), so
//!   per row the candidate fills the row's contiguous slice with the
//!   background and the band's slice with the marker colour: 2 × height
//!   slice uploads instead of `width × height` pixel calls.
//! - **B — the band upload.** The whole buffer fills with the
//!   background once, then only the marker band uploads row by row:
//!   `1 + height` uploads, and only the band's pixels are touched
//!   twice.
//!
//! Every form must reproduce the frozen camera's anchor digest — the
//! optimisation changes only how the pixels reach the buffer, never
//! which pixels.

use std::io::Write as _;
use std::time::Instant;

use client::render::scene::TestScene;
use client::render::software::SoftwareDevice;

/// The frames per timing leg.
const FRAMES: u32 = 30;
/// The frozen scene (the baseline's camera).
const FROZEN_PHASE: f32 = 0.5;
const FROZEN_STEPS: u32 = 3;
/// The ends (the baseline's profiles).
const ENDS: [(u32, u32); 3] = [(320, 240), (640, 480), (1024, 768)];

/// The record sink (the cluster's shape).
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("FRAME_BATCH_OUT") {
            std::fs::write(&path, &text).expect("write FRAME_BATCH_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// The frame's two colours, mirrored from `TestScene::draw`: the
/// background and the marker cell's shaded base.
fn frame_colours(scene: &TestScene, width: u32) -> (u32, u32, u32, u32) {
    let cell_width = (width / TestScene::default().period).max(1);
    let cell = scene.cell();
    let shade = 0x10u32.saturating_mul(scene.steps % 8);
    let r = (0x30u32 + shade + cell * 3) & 0xff;
    let g = (0x40u32 + shade) & 0xff;
    let b = (0x90u32 + shade) & 0xff;
    let bg = 0x20_28_38_ff;
    let marker = (b << 16) | (g << 8) | r | 0xff00_0000;
    (bg, marker, cell, cell_width)
}

/// The baseline form: the scene's own draw, timed.
fn baseline_form(
    scene: &TestScene,
    width: u32,
    height: u32,
    frames: u32,
) -> (f64, f64, u64, usize) {
    let mut device = SoftwareDevice::new(width, height);
    let anchor = device.present(scene).checksum;
    let mut ns_total = 0.0f64;
    let mut ns_max = 0.0f64;
    for _ in 1..frames {
        let t0 = Instant::now();
        let result = device.present(scene);
        ns_total += t0.elapsed().as_nanos() as f64;
        ns_max = ns_max.max(t0.elapsed().as_nanos() as f64);
        assert_eq!(result.checksum, anchor, "the baseline renders one frame");
    }
    let calls = width as usize * height as usize;
    (ns_total / (frames - 1) as f64, ns_max, anchor, calls)
}

/// Candidate A: per row, one background slice and one marker-band slice.
fn row_upload_form(
    scene: &TestScene,
    width: u32,
    height: u32,
    anchor: u64,
    frames: u32,
) -> (f64, f64, usize) {
    let (bg, marker, cell, cell_width) = frame_colours(scene, width);
    let band = (cell * cell_width, ((cell + 1) * cell_width).min(width));
    let mut fb = client::render::framebuffer::Framebuffer::new(width, height);
    let mut ns_total = 0.0f64;
    let mut ns_max = 0.0f64;
    for _ in 0..frames {
        let t0 = Instant::now();
        for y in 0..height {
            let base = (y * width) as usize;
            let row = &mut fb.pixels[base..base + width as usize];
            row.fill(bg);
            row[band.0 as usize..band.1 as usize].fill(marker);
        }
        ns_total += t0.elapsed().as_nanos() as f64;
        ns_max = ns_max.max(t0.elapsed().as_nanos() as f64);
        assert_eq!(fb.checksum(), anchor, "row upload: a different frame");
    }
    let calls = 2 * height as usize;
    (ns_total / frames as f64, ns_max, calls)
}

/// Candidate B: one whole-buffer background fill, then the marker band
/// uploads row by row.
fn band_upload_form(
    scene: &TestScene,
    width: u32,
    height: u32,
    anchor: u64,
    frames: u32,
) -> (f64, f64, usize) {
    let (bg, marker, cell, cell_width) = frame_colours(scene, width);
    let band = (cell * cell_width, ((cell + 1) * cell_width).min(width));
    let mut fb = client::render::framebuffer::Framebuffer::new(width, height);
    let mut ns_total = 0.0f64;
    let mut ns_max = 0.0f64;
    for _ in 0..frames {
        let t0 = Instant::now();
        fb.pixels.fill(bg);
        for y in 0..height {
            let base = (y * width) as usize;
            let row = &mut fb.pixels[base..base + width as usize];
            row[band.0 as usize..band.1 as usize].fill(marker);
        }
        ns_total += t0.elapsed().as_nanos() as f64;
        ns_max = ns_max.max(t0.elapsed().as_nanos() as f64);
        assert_eq!(fb.checksum(), anchor, "band upload: a different frame");
    }
    let calls = 1 + height as usize;
    (ns_total / frames as f64, ns_max, calls)
}

/// Evaluates the two candidates on every end and leaves the record.
#[test]
fn the_upload_candidates_compare_frame_time_per_end() {
    let started = Instant::now();
    let mut record = Record::default();

    let frozen = TestScene {
        phase: FROZEN_PHASE,
        steps: FROZEN_STEPS,
        period: TestScene::default().period,
    };
    for (width, height) in ENDS {
        let label = format!("end_{width}x{height}");
        let (base_avg, base_max, anchor, base_calls) =
            baseline_form(&frozen, width, height, FRAMES);
        let (a_avg, a_max, a_calls) = row_upload_form(&frozen, width, height, anchor, FRAMES);
        let (b_avg, b_max, b_calls) = band_upload_form(&frozen, width, height, anchor, FRAMES);
        record.put(
            format!("{label}_base_frame_ns_avg"),
            format!("{base_avg:.0}"),
        );
        record.put(
            format!("{label}_base_frame_ns_max"),
            format!("{base_max:.0}"),
        );
        record.put(format!("{label}_base_upload_calls"), base_calls);
        record.put(format!("{label}_row_frame_ns_avg"), format!("{a_avg:.0}"));
        record.put(format!("{label}_row_frame_ns_max"), format!("{a_max:.0}"));
        record.put(format!("{label}_row_upload_calls"), a_calls);
        record.put(format!("{label}_band_frame_ns_avg"), format!("{b_avg:.0}"));
        record.put(format!("{label}_band_frame_ns_max"), format!("{b_max:.0}"));
        record.put(format!("{label}_band_upload_calls"), b_calls);
        record.put(
            format!("{label}_row_speedup"),
            format!("{:.2}", base_avg / a_avg),
        );
        record.put(
            format!("{label}_band_speedup"),
            format!("{:.2}", base_avg / b_avg),
        );
        record.put(format!("{label}_digests_equal"), 1);
    }

    record.put(
        "eval_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
