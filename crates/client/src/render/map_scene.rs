//! The fixed-map scene (BW-15-02-03's "固定地图可显示"): the loaded chunk
//! space projected top-down into the back buffer.
//!
//! The reference client's `ChunkSpace` draws its loaded chunks through the
//! Moo scene graph; the port's software rasterizer paints the same
//! observables the space loader reports — one cell per loaded chunk (the
//! grid rectangle its transform anchors), one marker per chunk item at the
//! item's world position — so the display evidence derives from the map
//! itself rather than from synthetic animation. Two runs over the same
//! space produce identical buffers; a scene built from a different map
//! produces a different digest.

use super::Scene;
use super::framebuffer::Framebuffer;
use chunk::ChunkSpace;
use foundation::math::Vector3;

/// The empty background (`0xAABBGGRR`).
const BACKGROUND: u32 = 0xff_28_20_18;
/// The outline of the whole loaded area.
const OUTLINE: u32 = 0xff_60_70_80;
/// Marker colours, one per chunk item tag (`0xAABBGGRR`).
const MARKER_MODEL: u32 = 0xff_20_80_ff;
const MARKER_LIGHT: u32 = 0xff_20_e0_e0;
const MARKER_SOUND: u32 = 0xff_e0_e0_20;
const MARKER_ENTITY: u32 = 0xff_e0_20_e0;
const MARKER_OTHER: u32 = 0xff_c0_c0_c0;
/// Screen margin kept around the projected area, in pixels.
const MARGIN: u32 = 4;

/// One loaded chunk's grid cell, in world coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct MapCell {
    /// Cell corner, world space (the chunk transform's origin).
    pub origin: Vector3,
    /// Cell size on the ground plane (the mapping's grid size).
    pub size: f32,
    /// Deterministic fill colour, derived from the chunk identifier.
    pub fill: u32,
    /// The owning chunk's identifier.
    pub identifier: String,
}

/// One chunk item's world position.
#[derive(Debug, Clone, PartialEq)]
pub struct MapMarker {
    /// World position (chunk transform applied to the item's origin).
    pub position: Vector3,
    /// Colour by item tag.
    pub color: u32,
}

/// The fixed-map scene: the loaded chunk set projected top-down.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapScene {
    /// One cell per loaded chunk.
    pub cells: Vec<MapCell>,
    /// One marker per chunk item.
    pub markers: Vec<MapMarker>,
    /// The loaded area's bounds (cell union; markers may sit inside cells).
    pub min: Vector3,
    pub max: Vector3,
}

impl MapScene {
    /// Project the space's loaded chunks and their items.
    pub fn from_space(space: &ChunkSpace) -> Self {
        let mut scene = Self::default();
        for chunk in space.chunks() {
            let origin = chunk.transform.apply_to_origin();
            let size = chunk.mapping.grid_size;
            scene.cells.push(MapCell {
                origin,
                size,
                fill: cell_fill(&chunk.identifier),
                identifier: chunk.identifier.clone(),
            });
            for item in &chunk.items {
                let local = item
                    .position
                    .unwrap_or_else(|| item.transform.apply_to_origin());
                scene.markers.push(MapMarker {
                    position: chunk.transform.apply_point(&local),
                    color: marker_color(&item.tag),
                });
            }
            scene.min.x = scene.min.x.min(origin.x);
            scene.min.z = scene.min.z.min(origin.z);
            scene.max.x = scene.max.x.max(origin.x + size);
            scene.max.z = scene.max.z.max(origin.z + size);
        }
        scene
    }

    /// Project a world point to back-buffer coordinates.
    fn project(&self, p: Vector3, fb: &Framebuffer) -> (i64, i64) {
        let span_x = (self.max.x - self.min.x).max(1.0);
        let span_z = (self.max.z - self.min.z).max(1.0);
        let usable_w = fb.width.saturating_sub(2 * MARGIN).max(1) as f32;
        let usable_h = fb.height.saturating_sub(2 * MARGIN).max(1) as f32;
        let scale = (usable_w / span_x).min(usable_h / span_z);
        let offset_x = (fb.width as f32 - span_x * scale) * 0.5;
        let offset_y = (fb.height as f32 - span_z * scale) * 0.5;
        let px = offset_x + (p.x - self.min.x) * scale;
        let py = offset_y + (p.z - self.min.z) * scale;
        (px.round() as i64, py.round() as i64)
    }
}

impl Scene for MapScene {
    fn draw(&self, fb: &mut Framebuffer) {
        if fb.width == 0 || fb.height == 0 {
            return;
        }
        fb.fill(BACKGROUND);
        // One filled, outlined rectangle per loaded chunk cell.
        for cell in &self.cells {
            let (x0, y0) = self.project(cell.origin, fb);
            let (x1, y1) = self.project(
                Vector3::new(
                    cell.origin.x + cell.size,
                    cell.origin.y,
                    cell.origin.z + cell.size,
                ),
                fb,
            );
            let (x0, x1) = (x0.min(x1), x0.max(x1));
            let (y0, y1) = (y0.min(y1), y0.max(y1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let border = x == x0 || x == x1 || y == y0 || y == y1;
                    let rgba = if border { OUTLINE } else { cell.fill };
                    if x >= 0 && y >= 0 {
                        fb.set_pixel(x as u32, y as u32, rgba);
                    }
                }
            }
        }
        // One 3x3 marker per chunk item.
        for marker in &self.markers {
            let (px, py) = self.project(marker.position, fb);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let x = px + dx;
                    let y = py + dy;
                    if x >= 0 && y >= 0 {
                        fb.set_pixel(x as u32, y as u32, marker.color);
                    }
                }
            }
        }
        // The loaded area's outline, on top.
        let (ox0, oy0) = self.project(self.min, fb);
        let (ox1, oy1) = self.project(self.max, fb);
        for y in oy0..=oy1 {
            for x in [ox0, ox1] {
                if x >= 0 && y >= 0 {
                    fb.set_pixel(x as u32, y as u32, OUTLINE);
                }
            }
        }
        for x in ox0..=ox1 {
            for y in [oy0, oy1] {
                if x >= 0 && y >= 0 {
                    fb.set_pixel(x as u32, y as u32, OUTLINE);
                }
            }
        }
    }
}

/// FNV-1a over the identifier bytes, the same digest the framebuffer uses.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A deterministic per-chunk cell fill: a green whose channels vary with the
/// identifier, so neighbouring cells stay distinguishable.
fn cell_fill(identifier: &str) -> u32 {
    let h = fnv1a(identifier.as_bytes());
    let r = 0x30 + ((h & 0x1f) as u32);
    let g = 0x50 + (((h >> 8) & 0x1f) as u32);
    let b = 0x38 + (((h >> 16) & 0x1f) as u32);
    0xff00_0000 | (b << 16) | (g << 8) | r
}

/// The marker colour for a chunk item tag.
fn marker_color(tag: &str) -> u32 {
    match tag {
        "model" => MARKER_MODEL,
        "light" => MARKER_LIGHT,
        "sound" => MARKER_SOUND,
        "entity" => MARKER_ENTITY,
        _ => MARKER_OTHER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::software::SoftwareDevice;
    use chunk::{ChunkSpace, GeometryMapping};
    use resources::datasection;
    use std::path::{Path, PathBuf};

    fn space_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("the repository root")
            .join("docs/review/fixtures/chunk_space/res/space_00010001")
    }

    fn loaded_space() -> ChunkSpace {
        let root = space_root();
        let raw = std::fs::read(root.join("settings.xml")).expect("settings.xml readable");
        let settings = datasection::parse("", &raw).expect("settings.xml parses");
        let mapping = GeometryMapping::from_settings("space_00010001/", &settings);
        let resource =
            resources::bwresource::BwResource::init_paths(&[root.to_str().expect("utf-8")]);
        let resolver = |name: &str| {
            resource
                .open_section(name, false)
                .map(|section| (*section).clone())
        };
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        let candidates: Vec<String> = mapping.gather_chunks(&root).into_iter().collect();
        space.load_and_bind(&candidates, &resolver);
        space
    }

    #[test]
    fn the_fixed_map_projects_every_loaded_chunk_and_item() {
        let space = loaded_space();
        let loaded: Vec<&chunk::Chunk> = space.chunks().collect();
        let items: usize = loaded.iter().map(|c| c.items.len()).sum();
        let scene = MapScene::from_space(&space);
        assert_eq!(scene.cells.len(), loaded.len());
        assert_eq!(scene.markers.len(), items);
        // The two origin-neighbourhood cells are exactly one grid apart.
        let a = scene
            .cells
            .iter()
            .find(|c| c.identifier == "00000000o")
            .expect("origin chunk loaded");
        let b = scene
            .cells
            .iter()
            .find(|c| c.identifier == "00010001o")
            .expect("diagonal chunk loaded");
        assert_eq!(b.origin.x - a.origin.x, a.size);
        assert_eq!(b.origin.z - a.origin.z, a.size);
        assert_ne!(a.fill, b.fill, "neighbouring cells shade differently");
    }

    #[test]
    fn the_drawn_map_is_deterministic_and_differs_from_background() {
        let space = loaded_space();
        let scene = MapScene::from_space(&space);
        let mut fb = Framebuffer::new(160, 120);
        scene.draw(&mut fb);
        let first = fb.checksum();
        let mut again = Framebuffer::new(160, 120);
        scene.draw(&mut again);
        assert_eq!(first, again.checksum(), "two draws agree");

        // The background-only buffer differs, and the map's own pixels sit
        // inside the projected cell area.
        let mut empty = Framebuffer::new(160, 120);
        empty.fill(BACKGROUND);
        assert_ne!(first, empty.checksum());
        let cell = &scene.cells[0];
        let (px, py) = scene.project(
            Vector3::new(
                cell.origin.x + cell.size * 0.5,
                cell.origin.y,
                cell.origin.z + cell.size * 0.5,
            ),
            &fb,
        );
        assert_eq!(fb.get_pixel(px as u32, py as u32), cell.fill);
    }

    #[test]
    fn the_map_scene_drives_the_software_device() {
        let space = loaded_space();
        let scene = MapScene::from_space(&space);
        let mut device = SoftwareDevice::new(80, 60);
        let a = device.present(&scene);
        let b = device.present(&scene);
        assert_eq!(a.frame_index, 1);
        assert_eq!(b.frame_index, 2);
        // A static map renders the same frame every present.
        assert_eq!(a.checksum, b.checksum);
    }
}
