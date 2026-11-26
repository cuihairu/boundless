//! BW-15-02-03 the client entry point's Linux display leg: load the fixed
//! map (a chunk space), render it through the software device and write the
//! screenshot artifact the acceptance's display evidence rides.
//!
//! Usage:
//!
//! ```sh
//! boundless-client [--res DIR] [--frames N] [--width W] [--height H] [--shot PATH]
//! ```
//!
//! `--res` is the space directory (default
//! `docs/review/fixtures/chunk_space/res/space_00010001`, the fixed map the
//! BW-10-01 fixtures carry). The run prints `metric<TAB>value` lines on
//! stdout — the loaded chunk count, the item count, the frame digest and
//! the screenshot's byte count and digest — and exits non-zero when the
//! display contract breaks (no chunk bound, or a static map rendering two
//! different frames).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chunk::{ChunkSpace, GeometryMapping};
use client::render::map_scene::MapScene;
use client::render::software::SoftwareDevice;
use client::screen_shot::ScreenCopy;
use resources::bwresource::BwResource;
use resources::datasection;

const DEFAULT_RES: &str = "docs/review/fixtures/chunk_space/res/space_00010001";
const DEFAULT_FRAMES: u32 = 8;
const DEFAULT_WIDTH: u32 = 320;
const DEFAULT_HEIGHT: u32 = 240;
const DEFAULT_SHOT: &str = "client-view.tga";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = arg_value(&args, "--res").unwrap_or_else(|| DEFAULT_RES.to_string());
    let frames: u32 = arg_value(&args, "--frames")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_FRAMES);
    let width: u32 = arg_value(&args, "--width")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WIDTH);
    let height: u32 = arg_value(&args, "--height")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_HEIGHT);
    let shot = arg_value(&args, "--shot").unwrap_or_else(|| DEFAULT_SHOT.to_string());
    if frames == 0 || width == 0 || height == 0 {
        eprintln!("boundless-client: --frames/--width/--height must be non-zero");
        return ExitCode::from(2);
    }

    let root = PathBuf::from(&res);
    let (space, items) = match load_space(&root) {
        Ok(found) => found,
        Err(err) => {
            eprintln!("boundless-client: {res}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let chunks = space.chunks().count();
    if chunks == 0 {
        eprintln!("boundless-client: {res}: no chunk bound from the settings grid");
        return ExitCode::FAILURE;
    }

    // The fixed map is static: every frame the device presents must carry
    // the same buffer, and the screenshot must be that same buffer.
    let scene = MapScene::from_space(&space);
    let mut device = SoftwareDevice::new(width, height);
    let mut digests = Vec::new();
    for _ in 0..frames {
        let frame = device.present(&scene);
        digests.push(frame.checksum);
    }
    if digests.iter().any(|d| *d != digests[0]) {
        eprintln!("boundless-client: static map rendered differing frames");
        return ExitCode::FAILURE;
    }

    let copy = ScreenCopy::capture(device.framebuffer());
    let tga = copy.to_tga();
    if let Err(err) = std::fs::write(&shot, &tga) {
        eprintln!("boundless-client: cannot write {shot}: {err}");
        return ExitCode::FAILURE;
    }

    println!("chunks_loaded\t{chunks}");
    println!("items\t{items}");
    println!(
        "bounds\t{:.3},{:.3} .. {:.3},{:.3}",
        scene.min.x, scene.min.z, scene.max.x, scene.max.z
    );
    println!("buffer\t{width}x{height}");
    println!("frames\t{frames}");
    println!("frame_digest\t{:016x}", digests[0]);
    println!("shot_path\t{shot}");
    println!("shot_bytes\t{}", tga.len());
    println!("shot_digest\t{:016x}", fnv1a(&tga));
    ExitCode::SUCCESS
}

/// Load the space directory: the settings grid mapping, then the chunk set
/// through `gather_chunks` + `load_and_bind`, the same path the chunk
/// fixture tests drive. Returns the bound space and the total item count.
fn load_space(root: &Path) -> Result<(ChunkSpace, usize), String> {
    let raw = std::fs::read(root.join("settings.xml"))
        .map_err(|err| format!("cannot read settings.xml: {err}"))?;
    let settings = datasection::parse("", &raw).map_err(|err| format!("settings.xml: {err}"))?;
    let dir_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("space directory name is not utf-8")?;
    let mapping = GeometryMapping::from_settings(format!("{dir_name}/"), &settings);

    let root_str = root.to_str().ok_or("space path is not utf-8")?;
    let resource = BwResource::init_paths(&[root_str]);
    let resolver = |name: &str| {
        resource
            .open_section(name, false)
            .map(|section| (*section).clone())
    };
    let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
    let candidates: Vec<String> = mapping.gather_chunks(root).into_iter().collect();
    space.load_and_bind(&candidates, &resolver);
    let items = space.chunks().map(|c| c.items.len()).sum();
    Ok((space, items))
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// FNV-1a over bytes, the digest the framebuffer uses.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
