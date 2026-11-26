//! OPT-11-01-01: the three-end fixed-camera baseline.
//!
//! The acceptance: **frame time and video memory recorded** (`保存帧时间
//! 和显存`). The reference client optimises across three target machines;
//! this port's pixel-producing device is the deterministic software
//! rasterizer (`render::software`), which is the same binary on every
//! platform — the per-platform split is BW-15-04's matrix — so the three
//! ends here are the three presentation profiles the device models
//! (the back-buffer sizes a small windowed, a standard windowed and a
//! fullscreen-size present take). The baseline records, per end:
//!
//! - **The fixed-camera frame time.** The scene frozen at a fixed phase
//!   and step, 120 presents: every frame must produce the identical
//!   back-buffer digest (that digest is the visual anchor the rerun row
//!   OPT-11-01-04 replays against), and the per-present wall time is the
//!   frame time.
//! - **The memory ledger.** The bytes the device holds: the back buffer
//!   (width × height × 4, `D3DFMT_A8R8G8B8`) plus the cached textures'
//!   level-0 pixel bytes from a fixed BMP fixture set loaded through the
//!   `TextureManager`.
//! - **The animated frame time.** The same walk with the scene stepping,
//!   so the frame time under changing content (every frame a different
//!   digest) is recorded next to the frozen one.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `CLIENT_FRAME_OUT` is set, to that path:
//!
//! ```sh
//! CLIENT_FRAME_OUT=<abs path> cargo test -p client \
//!     --test client_frame_baseline -- --nocapture
//! ```

use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use client::render::scene::TestScene;
use client::render::software::SoftwareDevice;
use client::texture::TextureManager;

/// The frames per leg.
const FRAMES: u32 = 120;
/// The frozen scene the fixed camera holds.
const FROZEN_PHASE: f32 = 0.5;
const FROZEN_STEPS: u32 = 3;
/// The texture fixtures: `(name, width, height)` in 24bpp BMPs.
const TEXTURES: [(&str, u32, u32); 4] = [
    ("icons.bmp", 32, 32),
    ("font.bmp", 64, 64),
    ("grass.bmp", 128, 128),
    ("sky.bmp", 256, 64),
];

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
        if let Ok(path) = std::env::var("CLIENT_FRAME_OUT") {
            std::fs::write(&path, &text).expect("write CLIENT_FRAME_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// A minimal uncompressed 24bpp BMP (`BI_RGB`) of the given size, with
/// deterministic pixel content.
fn bmp_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut raw = Vec::new();
    let stride = (width as usize * 3).div_ceil(4) * 4;
    let pixel_bytes = stride * height as usize;
    raw.extend_from_slice(b"BM");
    raw.extend_from_slice(&((54 + pixel_bytes) as u32).to_le_bytes());
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&54u32.to_le_bytes());
    raw.extend_from_slice(&40u32.to_le_bytes());
    raw.extend_from_slice(&(width as i32).to_le_bytes());
    raw.extend_from_slice(&(height as i32).to_le_bytes());
    raw.extend_from_slice(&1u16.to_le_bytes());
    raw.extend_from_slice(&24u16.to_le_bytes());
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    raw.extend_from_slice(&2835u32.to_le_bytes());
    raw.extend_from_slice(&2835u32.to_le_bytes());
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&0u32.to_le_bytes());
    for y in 0..height as usize {
        for x in 0..width as usize {
            raw.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 0x80]);
        }
        let row = width as usize * 3;
        raw.resize(raw.len() + stride - row, 0);
    }
    raw
}

/// The texture fixture set loaded through the manager: the cached
/// textures and their level-0 byte cost.
fn texture_ledger() -> (
    TextureManager,
    Vec<Rc<client::texture::Texture>>,
    usize,
    usize,
) {
    let mut mgr = TextureManager::new();
    let mut file_bytes = 0usize;
    for (name, width, height) in TEXTURES {
        let raw = bmp_bytes(width, height);
        file_bytes += raw.len();
        mgr.add_file(name, &raw);
    }
    let mut loaded = Vec::new();
    for (name, width, height) in TEXTURES {
        let tex = mgr
            .get(name)
            .unwrap_or_else(|| panic!("{name} loads from the res tree"));
        assert_eq!((tex.width, tex.height), (width, height), "{name} decodes");
        loaded.push(tex);
    }
    let pixel_bytes = loaded.iter().map(|t| t.pixels.len() * 4).sum();
    (mgr, loaded, file_bytes, pixel_bytes)
}

/// What one end recorded.
#[derive(Debug, Default)]
struct EndOutcome {
    frames: u32,
    frame_ns_avg: f64,
    frame_ns_max: f64,
    back_buffer_bytes: usize,
    checksum: u64,
    anim_frame_ns_avg: f64,
    anim_frame_ns_max: f64,
    anim_digests: usize,
}

/// The fixed-camera leg on one end: the frames, the frame time and the
/// visual anchor digest.
fn fixed_camera_leg(width: u32, height: u32) -> (u32, f64, f64, u64) {
    // The fixed camera: the scene never changes, so every present draws
    // the identical frame — the digest is the visual anchor.
    let frozen = TestScene {
        phase: FROZEN_PHASE,
        steps: FROZEN_STEPS,
        period: TestScene::default().period,
    };
    let mut device = SoftwareDevice::new(width, height);
    let mut ns_total = 0.0f64;
    let mut ns_max = 0.0f64;
    let anchor = device.present(&frozen).checksum;
    for _ in 1..FRAMES {
        let t0 = Instant::now();
        let result = device.present(&frozen);
        ns_total += t0.elapsed().as_nanos() as f64;
        ns_max = ns_max.max(t0.elapsed().as_nanos() as f64);
        assert_eq!(
            result.checksum, anchor,
            "the frozen camera renders one frame"
        );
    }
    let frames = device.frame_count() as u32;
    (frames, ns_total / (FRAMES - 1) as f64, ns_max, anchor)
}

/// The animated leg on one end: the frame time under changing content.
fn animated_leg(width: u32, height: u32) -> (f64, f64, usize) {
    let mut scene = TestScene::default();
    let mut anim_total = 0.0f64;
    let mut anim_max = 0.0f64;
    let mut digests = std::collections::BTreeSet::new();
    let mut device = SoftwareDevice::new(width, height);
    for _ in 0..FRAMES {
        scene.step();
        let t0 = Instant::now();
        let result = device.present(&scene);
        anim_total += t0.elapsed().as_nanos() as f64;
        anim_max = anim_max.max(t0.elapsed().as_nanos() as f64);
        digests.insert(result.checksum);
    }
    // The scene cycles: the phase wraps every `period` steps and the
    // shade every 8, so the content cycle is the phase period — the
    // walk must hit every frame of it.
    let period = TestScene::default().period as usize;
    assert_eq!(
        digests.len(),
        period,
        "the animated walk covers the scene's full content cycle"
    );
    (anim_total / FRAMES as f64, anim_max, digests.len())
}

/// The fixed-camera leg plus the animated leg on one end.
fn run_end(width: u32, height: u32) -> EndOutcome {
    let (frames, frame_ns_avg, frame_ns_max, checksum) = fixed_camera_leg(width, height);
    let (anim_frame_ns_avg, anim_frame_ns_max, anim_digests) = animated_leg(width, height);
    EndOutcome {
        frames,
        frame_ns_avg,
        frame_ns_max,
        back_buffer_bytes: width as usize * height as usize * 4,
        checksum,
        anim_frame_ns_avg,
        anim_frame_ns_max,
        anim_digests,
    }
}

/// Records the three ends and the memory ledger.
#[test]
fn the_fixed_camera_baseline_records_frame_time_and_memory() {
    let started = Instant::now();
    let mut record = Record::default();

    // The memory ledger: the textures, once — the cache cost is the same
    // on every end.
    let (_mgr, textures, file_bytes, pixel_bytes) = texture_ledger();
    record.put("tex_count", textures.len());
    record.put("tex_file_bytes", file_bytes);
    record.put("tex_pixel_bytes", pixel_bytes);

    // The three ends.
    let ends = [(320u32, 240u32), (640, 480), (1024, 768)];
    for (width, height) in ends {
        let out = run_end(width, height);
        let label = format!("end_{width}x{height}");
        record.put(format!("{label}_frames"), out.frames);
        record.put(
            format!("{label}_frame_ns_avg"),
            format!("{:.0}", out.frame_ns_avg),
        );
        record.put(
            format!("{label}_frame_ns_max"),
            format!("{:.0}", out.frame_ns_max),
        );
        record.put(format!("{label}_back_buffer_bytes"), out.back_buffer_bytes);
        record.put(
            format!("{label}_total_bytes"),
            out.back_buffer_bytes + pixel_bytes,
        );
        record.put(
            format!("{label}_anchor_checksum"),
            format!("{:016x}", out.checksum),
        );
        record.put(
            format!("{label}_anim_frame_ns_avg"),
            format!("{:.0}", out.anim_frame_ns_avg),
        );
        record.put(
            format!("{label}_anim_frame_ns_max"),
            format!("{:.0}", out.anim_frame_ns_max),
        );
        record.put(format!("{label}_anim_digests"), out.anim_digests);
    }

    record.put(
        "baseline_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
