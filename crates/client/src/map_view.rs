//! The fixed-map display face (BW-15-02-03's Linux client leg).
//!
//! The BW-11-01-02 test scene proves the device presents *frames*; this
//! module proves the device presents *the map*: a loaded chunk space —
//! the reference fixture `docs/review/fixtures/chunk_space`, the same
//! 18×2 grid `crates/chunk`'s load baseline streams — rasterised top-down
//! into the software back buffer. Each bound outside chunk is a tile
//! shaded from its identifier, each chunk item a coloured dot at its
//! world position (`chunk.transform` applied to the item's position or
//! transform origin), an inside chunk a filled inset at its transform
//! origin, and a camera marker whose position follows the animation
//! phase so consecutive frames differ exactly the way the test scene's
//! do. Everything is integer/f32 scalar maths with no libm, so the frame
//! checksums are byte-deterministic per the BW-11-04-07 comparability
//! contract.
//!
//! [`load_fixture_space`] is the display-side cold start: parse the
//! space's `settings.xml`, gather the chunk candidates, and run the
//! shipped `load_and_bind` sequence. It carries no obstacle registration
//! — display needs no physics slabs, and the load baseline already pins
//! the focussed-gate ledger.

use std::path::{Path, PathBuf};

use chunk::{ChunkSpace, GeometryMapping};
use foundation::math::Vector3;
use resources::datasection;

use crate::render::framebuffer::Framebuffer;
use crate::render::software::SoftwareDevice;
use crate::screen_shot;

/// Pixels per chunk side.
const TILE: u32 = 48;
/// Padding around the map area.
const PAD: u32 = 16;
/// Item dot radius.
const DOT_RADIUS: i32 = 2;
/// The camera marker half-size: a `2 * MARKER_HALF + 1` square.
const MARKER_HALF: i32 = 1;
/// The inside-chunk inset half-size.
const INSET_HALF: i32 = 6;

/// Background: cells of the grid with no bound chunk.
pub const BACKGROUND: u32 = 0xff_20_18_14;
/// A bound outside chunk's tile border.
pub const TILE_BORDER: u32 = 0xff_38_30_28;
/// An inside chunk's inset fill.
pub const INSET_FILL: u32 = 0xff_60_40_30;
/// The camera marker.
pub const MARKER: u32 = 0xff_f0_f0_f0;
/// `<model>` items.
pub const MODEL_DOT: u32 = 0xff_2a_c8_50;
/// `<light>` items.
pub const LIGHT_DOT: u32 = 0xff_20_90_ff;
/// `<sound>` and every other item tag.
pub const OTHER_DOT: u32 = 0xff_ff_b0_40;

/// One drawn chunk item: its tag and its world position (the chunk
/// transform applied to the item position, or the transform origin when
/// the item carries a bare transform).
#[derive(Debug, Clone, PartialEq)]
pub struct MapItem {
    /// The section tag (`model`, `light`, `sound`, ...).
    pub tag: String,
    /// World position.
    pub world: Vector3,
}

/// One drawn outside chunk: its grid cell, identifier and items.
#[derive(Debug, Clone, PartialEq)]
pub struct MapTile {
    /// Grid x of the chunk.
    pub x: i16,
    /// Grid z of the chunk.
    pub z: i16,
    /// The chunk identifier.
    pub identifier: String,
    /// The chunk's items.
    pub items: Vec<MapItem>,
}

/// The display scene over a loaded fixed map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapScene {
    /// The bound outside chunks.
    pub tiles: Vec<MapTile>,
    /// The inside chunk's world origin, when the space loaded one.
    pub inside_origin: Option<Vector3>,
    /// The mapping's grid size (metres per chunk).
    pub grid_size: f32,
    /// Loaded grid extents (inclusive).
    pub min_grid_x: i32,
    pub max_grid_x: i32,
    pub min_grid_z: i32,
    pub max_grid_z: i32,
    /// Animation phase in `[0, 1)`, the test scene's stepper.
    pub phase: f32,
    /// Steps taken so far.
    pub steps: u32,
    /// Steps per full phase wrap.
    pub period: u32,
}

impl MapScene {
    /// Snapshot the drawn faces of a loaded space. Items and tiles are
    /// copied out so the scene owns no borrow of the space.
    pub fn from_space(space: &ChunkSpace) -> Self {
        let mut tiles = Vec::new();
        let mut inside_origin = None;
        let mut min_grid_x = i32::MAX;
        let mut max_grid_x = i32::MIN;
        let mut min_grid_z = i32::MAX;
        let mut max_grid_z = i32::MIN;
        // An inside chunk's transform can sit outside the bound grid
        // (the fixture's is at 150,0,250), so fold its origin into the
        // extents too — otherwise its inset rasterises off-buffer and
        // the map silently drops it.
        for chunk in space.chunks() {
            if !chunk.is_outside {
                let origin = chunk.transform.apply_to_origin();
                let gx = (origin.x / space.grid_size()).floor() as i32;
                let gz = (origin.z / space.grid_size()).floor() as i32;
                min_grid_x = min_grid_x.min(gx);
                max_grid_x = max_grid_x.max(gx);
                min_grid_z = min_grid_z.min(gz);
                max_grid_z = max_grid_z.max(gz);
            }
        }
        for chunk in space.chunks() {
            let items = chunk
                .items
                .iter()
                .map(|item| MapItem {
                    tag: item.tag.clone(),
                    world: chunk.transform.apply_point(
                        &item
                            .position
                            .unwrap_or_else(|| item.transform.apply_to_origin()),
                    ),
                })
                .collect();
            if chunk.is_outside {
                tiles.push(MapTile {
                    x: chunk.x,
                    z: chunk.z,
                    identifier: chunk.identifier.clone(),
                    items,
                });
                min_grid_x = min_grid_x.min(i32::from(chunk.x));
                max_grid_x = max_grid_x.max(i32::from(chunk.x));
                min_grid_z = min_grid_z.min(i32::from(chunk.z));
                max_grid_z = max_grid_z.max(i32::from(chunk.z));
            } else if inside_origin.is_none() {
                inside_origin = Some(chunk.transform.apply_to_origin());
            }
        }
        Self {
            tiles,
            inside_origin,
            grid_size: space.grid_size(),
            min_grid_x,
            max_grid_x,
            min_grid_z,
            max_grid_z,
            phase: 0.0,
            steps: 0,
            period: 16,
        }
    }

    /// Advance the animation: the marker's phase only — the map itself is
    /// static, which is the evidence that the displayed content *is* the
    /// fixed map.
    pub fn step(&mut self) {
        self.steps = self.steps.wrapping_add(1);
        self.phase = (self.phase + 1.0 / self.period as f32).fract();
    }

    /// Back-buffer size the map needs.
    pub fn buffer_size(&self) -> (u32, u32) {
        let cols = (self.max_grid_x - self.min_grid_x + 1).max(1) as u32;
        let rows = (self.max_grid_z - self.min_grid_z + 1).max(1) as u32;
        (PAD * 2 + cols * TILE, PAD * 2 + rows * TILE)
    }

    /// World metres to back-buffer pixels (top-down, x right, z down).
    pub fn world_to_px(&self, world: &Vector3) -> (f32, f32) {
        let gx = world.x / self.grid_size;
        let gz = world.z / self.grid_size;
        (
            PAD as f32 + (gx - self.min_grid_x as f32) * TILE as f32,
            PAD as f32 + (gz - self.min_grid_z as f32) * TILE as f32,
        )
    }

    /// The marker's pixel position for the current phase: a linear sweep
    /// across the map with a triangle-wave bounce down the rows — scalar
    /// maths only, no libm, so the sweep is identical on every end.
    pub fn marker_px(&self) -> (f32, f32) {
        let (width, height) = self.buffer_size();
        let x0 = (PAD + 2) as f32;
        let x1 = (width - PAD - 2) as f32;
        let y0 = (PAD + 2) as f32;
        let y1 = (height - PAD - 2) as f32;
        let x = x0 + (x1 - x0) * self.phase;
        // Triangle wave: 0..1..0 over one phase wrap.
        let bounce = 1.0 - (self.phase * 2.0 - 1.0).abs();
        let y = y0 + (y1 - y0) * bounce;
        (x, y)
    }

    /// The tile shade an identifier maps to: FNV-1a folded into the
    /// middle band so no tile reads as background or border.
    fn tile_shade(identifier: &str) -> u32 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in identifier.as_bytes() {
            h ^= u64::from(*byte);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let band = ((h >> 24) & 0x1f) as u32; // 0..=31
        0xff_28_38_20 + 0x00_05_04 * band
    }

    fn draw_dot(fb: &mut Framebuffer, cx: i32, cy: i32, rgba: u32) {
        for dy in -DOT_RADIUS..=DOT_RADIUS {
            for dx in -DOT_RADIUS..=DOT_RADIUS {
                if dx * dx + dy * dy <= DOT_RADIUS * DOT_RADIUS {
                    fb.set_pixel((cx + dx).max(0) as u32, (cy + dy).max(0) as u32, rgba);
                }
            }
        }
    }

    fn draw_square(fb: &mut Framebuffer, cx: i32, cy: i32, half: i32, rgba: u32) {
        for dy in -half..=half {
            for dx in -half..=half {
                fb.set_pixel((cx + dx).max(0) as u32, (cy + dy).max(0) as u32, rgba);
            }
        }
    }

    /// Draw this frame into the back buffer.
    pub fn draw(&self, fb: &mut Framebuffer) {
        if fb.width == 0 || fb.height == 0 {
            return;
        }
        fb.fill(BACKGROUND);
        let min_gx = self.min_grid_x as f32;
        let min_gz = self.min_grid_z as f32;
        // Tiles: fill + one-pixel border, then the item dots.
        for tile in &self.tiles {
            let x0 = PAD + (tile.x as f32 - min_gx) as u32 * TILE;
            let z0 = PAD + (tile.z as f32 - min_gz) as u32 * TILE;
            let fill = Self::tile_shade(&tile.identifier);
            for py in z0..z0 + TILE {
                for px in x0..x0 + TILE {
                    let edge = px == x0 || px == x0 + TILE - 1 || py == z0 || py == z0 + TILE - 1;
                    fb.set_pixel(px, py, if edge { TILE_BORDER } else { fill });
                }
            }
            for item in &tile.items {
                let (fx, fz) = self.world_to_px(&item.world);
                let rgba = match item.tag.as_str() {
                    "model" => MODEL_DOT,
                    "light" => LIGHT_DOT,
                    _ => OTHER_DOT,
                };
                Self::draw_dot(fb, fx as i32, fz as i32, rgba);
            }
        }
        // The inside chunk: a filled inset at its transform origin.
        if let Some(origin) = self.inside_origin {
            let (fx, fz) = self.world_to_px(&origin);
            Self::draw_square(fb, fx as i32, fz as i32, INSET_HALF, INSET_FILL);
        }
        // The camera marker, on top.
        let (mx, my) = self.marker_px();
        Self::draw_square(fb, mx as i32, my as i32, MARKER_HALF, MARKER);
    }
}

/// The record of one display run: the bound ledger and the presented
/// frames, plus the last back buffer for the screenshot face.
#[derive(Debug)]
pub struct DisplayRun {
    /// The identifiers `load_and_bind` bound, in order.
    pub bound: Vec<String>,
    /// Back-buffer size.
    pub width: u32,
    pub height: u32,
    /// One `FrameResult` per presented frame.
    pub frames: Vec<crate::render::FrameResult>,
    /// The last presented back buffer.
    pub back_buffer: Framebuffer,
}

/// Load the fixed map under `space_root` (the `space_00010001`-style
/// directory): parse its `settings.xml`, gather the chunk candidates and
/// run the shipped load-and-bind sequence. The mapping directory name is
/// taken from the path's file name so a copied space loads identically.
pub fn load_fixture_space(space_root: &Path) -> Result<ChunkSpace, String> {
    let dir_name = space_root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("space root has no directory name: {}", space_root.display()))?;
    let mapping_path = format!("{dir_name}/");
    let raw = std::fs::read(space_root.join("settings.xml"))
        .map_err(|e| format!("settings.xml unreadable: {e}"))?;
    let settings = datasection::parse("", &raw).map_err(|e| format!("settings.xml: {e}"))?;
    let mapping = GeometryMapping::from_settings(mapping_path, &settings);
    let candidates: Vec<String> = mapping.gather_chunks(space_root).into_iter().collect();
    let resolver = |name: &str| -> Option<datasection::DataSection> {
        let raw = std::fs::read(space_root.join(name)).ok()?;
        datasection::parse("", &raw).ok()
    };
    let mut space = ChunkSpace::new(1, vec![mapping]);
    let bound = space.load_and_bind(&candidates, &resolver);
    if bound.is_empty() {
        return Err(format!("no chunk bound from {}", space_root.display()));
    }
    Ok(space)
}

/// The display leg: load the fixed map, present `frames` frames of it
/// through the software device and return the run record.
pub fn run_map_display(space_root: &Path, frames: u32) -> Result<DisplayRun, String> {
    let space = load_fixture_space(space_root)?;
    let bound: Vec<String> = space.bind_order().to_vec();
    let mut scene = MapScene::from_space(&space);
    let (width, height) = scene.buffer_size();
    let mut device = SoftwareDevice::new(width, height);
    let mut frames_out = Vec::new();
    let mut back_buffer = Framebuffer::new(0, 0);
    for _ in 0..frames {
        scene.step();
        let result = device.present_with(|fb| scene.draw(fb));
        let fb = device.framebuffer();
        back_buffer = Framebuffer {
            width: fb.width,
            height: fb.height,
            pixels: fb.pixels.clone(),
        };
        frames_out.push(result);
    }
    Ok(DisplayRun {
        bound,
        width,
        height,
        frames: frames_out,
        back_buffer,
    })
}

/// Write the run's last back buffer as a TGA screenshot (`--shot`),
/// through the same `screenShot` face the device tests use: the name
/// comes from [`screen_shot::save_screen_shot`]'s naming rules and the
/// bytes are the [`screen_shot::ScreenCopy`] TGA encoding. Returns the
/// path written.
pub fn write_screenshot(run: &DisplayRun, path: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .to_str()
        .ok_or_else(|| format!("screenshot path is not utf-8: {}", path.display()))?
        .to_string();
    let exists = |name: &str| Path::new(name).exists();
    let written =
        screen_shot::save_screen_shot(Some(&run.back_buffer), "tga", &file_name, false, &exists)
            .ok_or_else(|| "screenshot write failed".to_string())?;
    let tga = screen_shot::ScreenCopy::capture(&run.back_buffer).to_tga();
    std::fs::write(&written, &tga).map_err(|e| format!("screenshot write: {e}"))?;
    Ok(PathBuf::from(written))
}
