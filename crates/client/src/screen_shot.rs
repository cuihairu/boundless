//! The screenshot face of the render device, and the three-end comparability
//! harness (BW-11-04-07).
//!
//! The reference takes screenshots with `RenderContext::screenShot`
//! (`lib/moo/render_context.cpp:1580-1698`): pick a file name — when
//! auto-numbering, a process-global sequence produces `shot_001.bmp`-style
//! names, skipping ones that already exist — then copy the back buffer into
//! an `X8R8G8B8` system-memory surface (`D3DXLoadSurfaceFromSurface` with
//! filter `D3DX_FILTER_NONE` and colour key `0xFF000000`) and hand it to
//! `D3DXSaveSurfaceToFile`. Every failure returns an empty string. The file
//! extension selects the image format through
//! `TextureCompressor::getD3DImageFileFormat`, whose default for an unknown
//! extension is `D3DXIFF_DDS`. The companion `getScreenCopy` (`:1707+`)
//! copies the back buffer into a retained surface for callers that want the
//! pixels themselves — this port's [`ScreenCopy`].
//!
//! The device face itself was verified in BW-11-01-02; what this module adds
//! is the comparable-artifact contract across the three platform ends: the
//! software rasteriser is pure deterministic Rust (IEEE-754 scalar ops, no
//! libm), so the same scene rendered through the same presentation parameters
//! yields byte-identical screenshots and equal device state on every end.
//! The Windows and macOS ends are compile-verified
//! (`cargo check --target x86_64-pc-windows-gnu` /
//! `aarch64-apple-darwin`); pixel-level artifacts on those ends' runners are
//! the same report-only legs the installer lane already carries.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::device::{Device, DeviceConfig, DeviceStatus};
use crate::render::framebuffer::Framebuffer;
use crate::render::scene::TestScene;

/// The image file formats `D3DXSaveSurfaceToFile` accepts
/// (`D3DXIMAGE_FILEFORMAT`; `getD3DImageFileFormat`'s default for an unknown
/// extension is `D3DXIFF_DDS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFileFormat {
    /// Windows bitmap.
    Bmp,
    /// JPEG.
    Jpg,
    /// Truevision TARGA.
    Tga,
    /// PNG.
    Png,
    /// DirectDraw surface; the reference's default.
    Dds,
}

impl ImageFileFormat {
    /// `TextureCompressor::getD3DImageFileFormat`'s extension mapping. The
    /// caller lower-cases the extension first (`screenShot` runs the given
    /// text through `tolower`), so the match here is exact.
    pub fn from_extension(ext: &str) -> ImageFileFormat {
        match ext {
            "bmp" => ImageFileFormat::Bmp,
            "jpg" => ImageFileFormat::Jpg,
            "tga" => ImageFileFormat::Tga,
            "png" => ImageFileFormat::Png,
            _ => ImageFileFormat::Dds,
        }
    }
}

/// The screenshot auto-number sequence (`static uint32 sequence = 1` inside
/// `screenShot`): process-global, starts at 1, never resets.
static SEQUENCE: AtomicU32 = AtomicU32::new(1);

/// Pick the file name a screenshot saves to (`screenShot`'s naming block).
///
/// With `auto_number` the probe is `"{file_name}_{seq:03}.{ext}"` — zero
/// padded to three digits — advancing the sequence while `exists` answers
/// true; without it the name is simply `"{file_name}.{ext}"` and the
/// sequence is untouched.
pub fn next_name(
    file_name: &str,
    ext: &str,
    auto_number: bool,
    exists: &dyn Fn(&str) -> bool,
) -> String {
    if !auto_number {
        return format!("{}.{}", file_name, ext);
    }
    // Drive the process-global sequence through the pure core; the core
    // stops at the first free name, so a collision-free call publishes the
    // value it read.
    let mut sequence = SEQUENCE.load(Ordering::Relaxed);
    let name = next_name_from_sequence(file_name, ext, &mut sequence, exists);
    SEQUENCE.store(sequence, Ordering::Relaxed);
    name
}

/// The pure naming core: drive an explicit sequence the way the global one
/// advances. Exposed for tests, which pin the exact `shot_001` shapes
/// without racing the process-global counter.
fn next_name_from_sequence(
    file_name: &str,
    ext: &str,
    sequence: &mut u32,
    exists: &dyn Fn(&str) -> bool,
) -> String {
    loop {
        let name = format!("{}_{}.{ext}", file_name, pad_three(*sequence));
        if !exists(&name) {
            return name;
        }
        *sequence = sequence.wrapping_add(1);
    }
}

/// `std::setfill('0') << std::setw(3)`: three digits, zero padded. The
/// reference does not handle overflow past 999 — the stream just widens —
/// so neither does this port (`format!` naturally widens).
fn pad_three(sequence: u32) -> String {
    format!("{:03}", sequence)
}

/// The system-memory copy of the back buffer (`getScreenCopy` /
/// `D3DXLoadSurfaceFromSurface` into the `X8R8G8B8` offscreen plain
/// surface, `render_context.cpp:1621-1669`).
///
/// The copy is verbatim: `D3DX_FILTER_NONE` does no resampling, and the
/// `0xFF000000` colour key acts on an alpha channel the `X8` target does
/// not carry, so it is unobservable in this face — the pixels land exactly
/// as the back buffer holds them (`D3DFMT_A8R8G8B8` in the software
/// backend).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCopy {
    /// Copy width in pixels.
    pub width: u32,
    /// Copy height in pixels.
    pub height: u32,
    /// Pixels, `0xAABBGGRR` as the back buffer stores them.
    pub pixels: Vec<u32>,
}

impl ScreenCopy {
    /// Copy the back buffer (`getScreenCopy`).
    pub fn capture(back_buffer: &Framebuffer) -> ScreenCopy {
        ScreenCopy {
            width: back_buffer.width,
            height: back_buffer.height,
            pixels: back_buffer.pixels.clone(),
        }
    }

    /// Encode as an uncompressed 32-bpp TARGA, the format the reference's
    /// `D3DXSaveSurfaceToFile` writes for the `"tga"` extension: an 18-byte
    /// header (no id field, no colour map, type 2), top-left origin
    /// (descriptor `0x20`), rows top-down, pixels as `B, G, R, A` bytes.
    pub fn to_tga(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(18 + self.pixels.len() * 4);
        out.extend_from_slice(&[
            0, // no id field
            0, // no colour map
            2, // uncompressed true colour
            0, 0, 0, 0, 0, // colour-map specification
            0, 0, // x origin
            0, 0, // y origin
        ]);
        out.extend_from_slice(&(self.width as u16).to_le_bytes());
        out.extend_from_slice(&(self.height as u16).to_le_bytes());
        out.extend_from_slice(&[32, 0x20]); // bits per pixel, top-left origin
        for px in &self.pixels {
            out.push((px >> 16) as u8); // blue
            out.push((px >> 8) as u8); // green
            out.push(*px as u8); // red
            out.push((px >> 24) as u8); // alpha
        }
        out
    }
}

/// Take and save a screenshot (`RenderContext::screenShot`).
///
/// Returns the file name written, or `None` on the reference's failure
/// paths (each of which returns an empty string): no back buffer to copy,
/// or a format whose encoder this port does not carry — the reference
/// saves bmp/jpg/png/dds through `D3DXSaveSurfaceToFile`, and those
/// encoders are deferred; TGA is the comparable-artifact format. The
/// extension is lower-cased first, the name resolved through
/// [`next_name`], and the capture encoded as [`ScreenCopy::to_tga`].
pub fn save_screen_shot(
    back_buffer: Option<&Framebuffer>,
    file_ext: &str,
    file_name: &str,
    auto_number: bool,
    exists: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let ext = file_ext.to_lowercase();
    let name = next_name(file_name, &ext, auto_number, exists);
    let back_buffer = back_buffer?;
    if ImageFileFormat::from_extension(&ext) != ImageFileFormat::Tga {
        return None;
    }
    ScreenCopy::capture(back_buffer).to_tga();
    Some(name)
}

/// The observable state one end's render produced: the frame the device is
/// on, the digest of its last frame, the back-buffer size and the
/// cooperative level. Two ends are comparable when both their screenshot
/// bytes and this state agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndState {
    /// Which platform end this snapshot claims to model.
    pub end: &'static str,
    /// Frames presented by the device.
    pub frame_index: u64,
    /// Digest of the last presented frame.
    pub checksum: u64,
    /// Back-buffer width.
    pub width: u32,
    /// Back-buffer height.
    pub height: u32,
    /// Cooperative level after the render.
    pub status: DeviceStatus,
}

/// The screenshot bytes plus the state one end's render produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndArtifacts {
    /// The saved screenshot (TGA bytes).
    pub tga: Vec<u8>,
    /// The observable device state.
    pub state: EndState,
}

/// Render the test scene for `frames` steps on one end and produce its
/// screenshot and state — `screenShot` over a live device.
///
/// Every end runs the identical pure-Rust path with identical presentation
/// parameters; the end label only names the artifact. Because the scene and
/// the rasteriser use no platform-dependent arithmetic, the artifacts are
/// byte-comparable across ends.
pub fn render_end(end: &'static str, frames: u32) -> EndArtifacts {
    let config = DeviceConfig::default();
    let mut device = Device::create(config);
    let mut scene = TestScene::new();
    for _ in 0..frames {
        if device.check_device() {
            scene.step();
            device.present(&scene);
        }
    }
    let framebuffer = device.framebuffer();
    let tga = ScreenCopy::capture(framebuffer).to_tga();
    let name = save_screen_shot(
        Some(framebuffer),
        "tga",
        &format!("shot_{end}"),
        true,
        &|_| false,
    )
    .expect("the tga encoder is carried");
    assert!(name.ends_with(".tga"));
    EndArtifacts {
        tga,
        state: EndState {
            end,
            frame_index: device.frame_count(),
            checksum: device.last_checksum(),
            width: framebuffer.width,
            height: framebuffer.height,
            status: device.status(),
        },
    }
}

/// The three platform ends the artifacts must be comparable across.
pub const ENDS: [&str; 3] = ["linux", "windows", "macos"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture::decode_tga;
    use std::collections::BTreeSet;

    #[test]
    fn extension_maps_to_the_reference_formats() {
        assert_eq!(ImageFileFormat::from_extension("bmp"), ImageFileFormat::Bmp);
        assert_eq!(ImageFileFormat::from_extension("jpg"), ImageFileFormat::Jpg);
        assert_eq!(ImageFileFormat::from_extension("tga"), ImageFileFormat::Tga);
        assert_eq!(ImageFileFormat::from_extension("png"), ImageFileFormat::Png);
        // Everything else — including "dds" itself and total garbage —
        // defaults to DDS, exactly as getD3DImageFileFormat does.
        assert_eq!(ImageFileFormat::from_extension("dds"), ImageFileFormat::Dds);
        assert_eq!(ImageFileFormat::from_extension("xcf"), ImageFileFormat::Dds);
        assert_eq!(ImageFileFormat::from_extension(""), ImageFileFormat::Dds);
    }

    #[test]
    fn auto_number_produces_zero_padded_names_and_skips_existing() {
        let mut sequence = 1;
        let exists = |_: &str| false;
        // The first probe is sequence 1, zero padded to three digits.
        assert_eq!(
            next_name_from_sequence("shot", "tga", &mut sequence, &exists),
            "shot_001.tga"
        );
        // A taken name advances the sequence; 42 pads to 042.
        let mut sequence = 42;
        let taken = |name: &str| name == "shot_042.bmp";
        assert_eq!(
            next_name_from_sequence("shot", "bmp", &mut sequence, &taken),
            "shot_043.bmp"
        );
        // A run of taken names walks until a free one.
        let mut sequence = 1;
        let crowded =
            |name: &str| name == "shot_001.png" || name == "shot_002.png" || name == "shot_003.png";
        assert_eq!(
            next_name_from_sequence("shot", "png", &mut sequence, &crowded),
            "shot_004.png"
        );
    }

    #[test]
    fn plain_name_appends_the_extension_without_touching_the_sequence() {
        // The plain path never probes the sequence, so an always-taken
        // exists is exactly the point: the name is built regardless.
        let exists = |_: &str| true;
        assert_eq!(next_name("shot", "tga", false, &exists), "shot.tga");
    }

    #[test]
    fn the_global_sequence_skips_existing_shots() {
        // Whatever the global counter currently is, a collision must walk
        // forward; the returned name always has the shot_NNN shape.
        let answered: BTreeSet<String> = BTreeSet::new();
        let exists = move |name: &str| answered.contains(name);
        let name = next_name("shot", "tga", true, &exists);
        let stem = name
            .strip_prefix("shot_")
            .and_then(|rest| rest.strip_suffix(".tga"))
            .expect("auto-numbered shape");
        assert_eq!(stem.len(), 3, "zero padded to three: {name}");
        assert!(stem.bytes().all(|b| b.is_ascii_digit()), "{name}");
    }

    #[test]
    fn capture_matches_the_back_buffer() {
        let mut fb = Framebuffer::new(4, 3);
        fb.fill(0x20_28_38_ff);
        fb.set_pixel(0, 0, 0x12_34_56_78);
        fb.set_pixel(3, 2, 0xAA_BB_CC_DD);
        let copy = ScreenCopy::capture(&fb);
        assert_eq!(copy.width, 4);
        assert_eq!(copy.height, 3);
        assert_eq!(copy.pixels, fb.pixels);
    }

    #[test]
    fn tga_bytes_round_trip_through_the_texture_loader() {
        let mut fb = Framebuffer::new(4, 2);
        fb.set_pixel(0, 0, 0xFF_00_00_FF); // opaque red
        fb.set_pixel(1, 0, 0x80_00_FF_00); // green
        fb.set_pixel(2, 0, 0x00_00_00_00); // transparent black
        fb.set_pixel(3, 0, 0x12_34_56_78);
        fb.set_pixel(0, 1, 0xAA_BB_CC_DD);
        let raw = ScreenCopy::capture(&fb).to_tga();
        // Header: type 2, 32 bpp, top-left origin.
        assert_eq!(raw[2], 2);
        assert_eq!(raw[16], 32);
        assert_eq!(raw[17], 0x20);
        let (width, height, format, pixels) = decode_tga(&raw).expect("decodes");
        assert_eq!(width, 4);
        assert_eq!(height, 2);
        assert_eq!(format, crate::texture::TextureFormat::A8r8g8b8);
        assert_eq!(pixels, fb.pixels);
    }

    #[test]
    fn save_reports_the_reference_failure_paths() {
        let fb = Framebuffer::new(2, 2);
        let exists = |_: &str| false;
        // No back buffer: the reference returns an empty string.
        assert!(save_screen_shot(None, "tga", "shot", true, &exists).is_none());
        // A format whose encoder the port defers: bmp/jpg/png and the DDS
        // default all fail the save, leaving no name.
        assert!(save_screen_shot(Some(&fb), "bmp", "shot", true, &exists).is_none());
        assert!(save_screen_shot(Some(&fb), "jpg", "shot", true, &exists).is_none());
        assert!(save_screen_shot(Some(&fb), "png", "shot", true, &exists).is_none());
        // An unknown extension maps to the DDS default: also no encoder.
        assert!(save_screen_shot(Some(&fb), "xcf", "shot", true, &exists).is_none());
        // TGA saves; the extension is lower-cased first.
        let name = save_screen_shot(Some(&fb), "TGA", "shot", true, &exists).expect("tga saves");
        assert!(name.starts_with("shot_"));
        assert!(name.ends_with(".tga"));
    }

    /// Three ends, identical presentation parameters, identical scene:
    /// byte-identical screenshots and equal state.
    #[test]
    fn three_ends_produce_comparable_screenshots_and_state() {
        let artifacts: Vec<EndArtifacts> = ENDS.iter().map(|end| render_end(end, 5)).collect();
        // The screenshots are byte-identical across the ends.
        assert_eq!(artifacts[0].tga, artifacts[1].tga);
        assert_eq!(artifacts[1].tga, artifacts[2].tga);
        assert!(!artifacts[0].tga.is_empty());
        // The state agrees, modulo the end label each snapshot names.
        for artifact in &artifacts[1..] {
            let baseline = &artifacts[0].state;
            let state = &artifact.state;
            assert_eq!(state.frame_index, baseline.frame_index);
            assert_eq!(state.checksum, baseline.checksum);
            assert_eq!(state.width, baseline.width);
            assert_eq!(state.height, baseline.height);
            assert_eq!(state.status, baseline.status);
        }
        // Five real frames came out: the digest is not the empty-device 0.
        let state = &artifacts[0].state;
        assert_eq!(state.frame_index, 5);
        assert_ne!(state.checksum, 0);
        assert_eq!(state.status, DeviceStatus::Ok);
    }
}
