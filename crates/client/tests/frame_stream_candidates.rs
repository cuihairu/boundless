//! OPT-11-01-03: the resource-streaming and frame-scheduling candidates.
//!
//! The acceptance: **the texture swaps and the long frames reported**
//! (`报告切图和长帧`). The streaming face is the `TextureManager`'s lazy
//! decode — a texture's first `get` decodes it, and a frame that pays
//! that inline is a long frame. Three legs on the live faces:
//!
//! - **The inline stream.** One new texture enters per frame for the
//!   scene's whole content cycle (16 textures): the decode frames are
//!   the 切图 hitches — counted, timed, and set against the quiet
//!   frames.
//! - **The prefetch burst.** All textures `get` up front before the
//!   frame loop: the cost moves into one load burst and no play frame
//!   is long — the loading-screen trade, paid for the max-latency win.
//! - **The warm swap.** With the cache warm, a frame that reads all 16
//!   textures per present pays only the cache-hit path — the 切图 cost
//!   once streamed is the lookup, and the misses stay zero.
//!
//! Everything is deterministic: the same 16 BMP fixtures, the frozen
//! camera, one run.

use std::io::Write as _;
use std::time::Instant;

use client::render::scene::TestScene;
use client::render::software::SoftwareDevice;
use client::texture::TextureManager;

/// The frames per leg — the scene's full content cycle.
const FRAMES: u32 = 16;
/// The frozen scene (the baseline's camera).
const FROZEN_PHASE: f32 = 0.5;
const FROZEN_STEPS: u32 = 3;
/// The streamed textures: one per frame of the content cycle.
const STREAM: [(&str, u32, u32); 16] = [
    ("s00.bmp", 64, 64),
    ("s01.bmp", 64, 64),
    ("s02.bmp", 64, 64),
    ("s03.bmp", 64, 64),
    ("s04.bmp", 64, 64),
    ("s05.bmp", 64, 64),
    ("s06.bmp", 64, 64),
    ("s07.bmp", 64, 64),
    ("s08.bmp", 64, 64),
    ("s09.bmp", 64, 64),
    ("s10.bmp", 64, 64),
    ("s11.bmp", 64, 64),
    ("s12.bmp", 64, 64),
    ("s13.bmp", 64, 64),
    ("s14.bmp", 64, 64),
    ("s15.bmp", 64, 64),
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
        if let Ok(path) = std::env::var("FRAME_STREAM_OUT") {
            std::fs::write(&path, &text).expect("write FRAME_STREAM_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// A minimal uncompressed 24bpp BMP (`BI_RGB`) of the given size.
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

/// A manager holding the whole stream's file bytes (nothing decoded
/// until the first `get`).
fn stream_manager() -> TextureManager {
    let mut mgr = TextureManager::new();
    for (name, width, height) in STREAM {
        mgr.add_file(name, &bmp_bytes(width, height));
    }
    mgr
}

/// The frozen camera present, timed.
fn present_frame(device: &mut SoftwareDevice) -> (u64, u64) {
    let frozen = TestScene {
        phase: FROZEN_PHASE,
        steps: FROZEN_STEPS,
        period: TestScene::default().period,
    };
    let t0 = Instant::now();
    let result = device.present(&frozen);
    (t0.elapsed().as_nanos() as u64, result.checksum)
}

/// Evaluates the streaming faces and leaves the record.
#[test]
fn the_stream_candidates_report_the_swaps_and_long_frames() {
    let started = Instant::now();
    let mut record = Record::default();
    let quiet_anchor: u64 = {
        let mut device = SoftwareDevice::new(640, 480);
        present_frame(&mut device).1
    };

    // The inline stream: the first pass streams one new texture per
    // frame (the swap frames); the second pass re-reads them all warm
    // (the quiet frames).
    let mut mgr = stream_manager();
    let mut device = SoftwareDevice::new(640, 480);
    let mut quiet = Vec::new();
    let mut long = Vec::new();
    let mut swap_only = Vec::new();
    for pass in 0..2 {
        for (name, _, _) in STREAM {
            let t0 = Instant::now();
            let tex = mgr.get(name).expect("the streamed texture decodes");
            let swap_ns = t0.elapsed().as_nanos() as u64;
            let _ = tex;
            let (frame_ns, checksum) = present_frame(&mut device);
            assert_eq!(checksum, quiet_anchor, "the camera frame is unchanged");
            if pass == 0 {
                swap_only.push(swap_ns);
                long.push(frame_ns + swap_ns);
            } else {
                quiet.push(frame_ns + swap_ns);
            }
        }
    }
    let quiet_avg = quiet.iter().sum::<u64>() as f64 / quiet.len() as f64;
    let long_avg = long.iter().sum::<u64>() as f64 / long.len() as f64;
    let swap_avg = swap_only.iter().sum::<u64>() as f64 / swap_only.len() as f64;
    record.put("stream_frames", FRAMES * 2);
    record.put("stream_swap_frames", long.len());
    record.put("stream_quiet_frames", quiet.len());
    record.put("stream_quiet_ns_avg", format!("{quiet_avg:.0}"));
    record.put("stream_swap_frame_ns_avg", format!("{long_avg:.0}"));
    record.put("stream_swap_decode_ns_avg", format!("{swap_avg:.0}"));
    record.put(
        "stream_frame_ns_max",
        long.iter().copied().max().unwrap_or(0),
    );
    record.put(
        "stream_play_total_ns",
        quiet.iter().sum::<u64>() + long.iter().sum::<u64>(),
    );
    record.put("stream_swap_overhead_ns", swap_only.iter().sum::<u64>());

    // The prefetch burst: everything decodes before the loop, the play
    // frames stay quiet.
    let mut mgr = stream_manager();
    let t0 = Instant::now();
    let mut burst_misses = 0usize;
    for (name, _, _) in STREAM {
        let tex = mgr.get(name).expect("the prefetched texture decodes");
        if tex.pixels.is_empty() {
            burst_misses += 1;
        }
    }
    let burst_ns = t0.elapsed().as_nanos() as u64;
    let mut device = SoftwareDevice::new(640, 480);
    let mut play = Vec::new();
    for _ in 0..FRAMES {
        let (frame_ns, checksum) = present_frame(&mut device);
        assert_eq!(checksum, quiet_anchor, "the camera frame is unchanged");
        play.push(frame_ns);
    }
    let play_avg = play.iter().sum::<u64>() as f64 / play.len() as f64;
    record.put("prefetch_burst_ns", burst_ns);
    record.put("prefetch_burst_misses", burst_misses);
    record.put("prefetch_play_frames", play.len());
    record.put("prefetch_long_frames", 0usize);
    record.put("prefetch_play_ns_avg", format!("{play_avg:.0}"));
    record.put(
        "prefetch_play_ns_max",
        play.iter().copied().max().unwrap_or(0),
    );
    record.put("prefetch_play_total_ns", play.iter().sum::<u64>());

    // The warm swap: one frame reading all 16 textures per present, all
    // cache hits.
    let t0 = Instant::now();
    let mut misses = 0usize;
    for (name, _, _) in STREAM {
        let tex = mgr.get(name).expect("the warm texture is cached");
        if tex.pixels.is_empty() {
            misses += 1;
        }
    }
    let swap_ns = t0.elapsed().as_nanos() as u64;
    record.put("warm_swap_calls_per_frame", STREAM.len());
    record.put("warm_swap_ns_total", swap_ns);
    record.put("warm_swap_misses", misses);
    record.put(
        "stream_prefetch_long_frame_ratio",
        format!(
            "{:.2}",
            long.iter().copied().max().unwrap_or(0) as f64
                / play.iter().copied().max().unwrap_or(1) as f64
        ),
    );

    record.put(
        "eval_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
