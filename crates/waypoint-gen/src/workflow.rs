//! The NavGen per-girth workflow and the two chunk-facing arms of the
//! waypoint generator that never made it into the crate before.
//!
//! `tools/navgen/navgen.cpp` `doGenerate` walks the chunk's girth list
//! once: flood, generate, output — and clears the dirty flag afterwards.
//! Here the chunk, physics and resource seams become the traits
//! [`FloodStage`], [`NavmeshProbe`] and [`DropPhysics`], so the loop is
//! runnable headlessly.
//!
//! The two arms ported verbatim from `waypoint_generator.cpp`:
//!
//! * `readTGA` (`:252-334`) — the inverse of
//!   [`crate::flood::WaypointFlood::write_tga`];
//! * `extendThroughUnboundPortals` (`:2369-2628`) — grow a polygon one
//!   step past each unbound portal;
//! * `determineEdgesAdjacentToOtherChunks` (`:2632-2730`) — flag the
//!   edges that actually sit outside the chunk.
//!
//! Neither arm has a live `Chunk` on this side, so the chunk reaches
//! them as a [`NavmeshProbe`]: its local bounds, transforms and `owns`
//! verdict. The reference's `IPhysics*` argument becomes
//! [`DropPhysics`].

use std::io::{Read, Seek, SeekFrom};

use foundation::math::boundbox::BoundBox;
use foundation::math::matrix::Matrix;
use foundation::math::plane::PlaneEq;
use foundation::math::vector::{Vector2, Vector3};

use crate::flood::{AdjGridElt, MAX_HEIGHTS};
use crate::generator::{PolygonDef, WaypointGenerator};

/// `sizeof(WaypointTGAHeader)` (`waypoint_tga.hpp`): the 18-byte TGA
/// core plus the 16 bytes of extra data carrying `gridMin` and
/// `gridResolution`. `readTGA` reads exactly this many bytes.
const TGA_HEADER_SIZE: usize = 34;

/// Palette bytes skipped past the header (`fseek(pFile, 768, SEEK_CUR)`).
const TGA_PALETTE: usize = 768;

/// `sizeof(Vector3) + sizeof(float)`: the only extra-data length
/// `readTGA` accepts.
const TGA_EXTRA_LENGTH: u8 = 16;

/// The reference writes the flood result at `xsize*16*4` by
/// `zsize*16*4` pixels, so `readTGA` divides both by that factor.
const TGA_PIXELS_PER_CELL: i32 = 16 * 4;

/// `extraExtendPoints.size() != 1` (`waypoint_generator.cpp:2498`): the
/// edges to be extended must form one contiguous run.
const EXTEND_NEEDS_ONE_END: &str =
    "WaypointGenerator::extendThroughUnboundPortals:extraExtendPoints.size() != 1";

/// `non consecutive edges through portal`
/// (`waypoint_generator.cpp:2519-2521`).
const NON_CONSECUTIVE_EDGES: &str =
    "WaypointGenerator::extendThroughUnboundPortals: non consecutive edges through portal.";

// ---------------------------------------------------------------------------
// the TGA read
// ---------------------------------------------------------------------------

/// `WaypointTGAHeader` (`waypoint_tga.hpp`), read verbatim from the
/// first 34 bytes of the file.
#[derive(Clone, Copy, Debug, Default)]
pub struct TgaHeader {
    pub extra_length: u8,
    pub colour_map_type: u8,
    pub image_type: u8,
    pub colour_map_start: u16,
    pub colour_map_length: u16,
    pub colour_map_depth: u8,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
    pub image_descriptor: u8,
    pub grid_min: Vector3,
    pub grid_resolution: f32,
}

/// What went wrong reading a flood result. Every variant is one of the
/// `ERROR_MSG` sites of `readTGA` (`waypoint_generator.cpp:268-323`),
/// the first two added by the port (the reference reports them with
/// `bw_fopen`'s `ERROR_MSG`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TgaError {
    /// `Failed to open %s.`
    Open(&'static str),
    /// `Failed to read header.`
    Header(&'static str),
    /// `Invalid TGA header.`
    InvalidHeader(&'static str),
    /// `Seek error.`
    Seek(&'static str),
    /// `Read error.`
    Read(&'static str),
}

impl std::fmt::Display for TgaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (what, path) = match self {
            TgaError::Open(p) => ("Failed to open", p),
            TgaError::Header(p) => ("Failed to read header", p),
            TgaError::InvalidHeader(p) => ("Invalid TGA header", p),
            TgaError::Seek(p) => ("Seek error", p),
            TgaError::Read(p) => ("Read error", p),
        };
        writeln!(f, "WaypointGenerator: {what} {path}.")
    }
}

impl std::error::Error for TgaError {}

// ---------------------------------------------------------------------------
// the chunk and physics seams
// ---------------------------------------------------------------------------

/// The chunk face `WaypointGenerator` reads. The reference passes
/// `Chunk*` to both portal arms; the port takes the parts it consumes.
///
/// `owns` is `Chunk::owns` (`chunk.cpp:3474`) and the bounds are
/// `pChunk->localBB()`'s corners, kept in the chunk's local frame —
/// the reference compares them against points that `toVector3` has
/// pulled through `transformInverse`, and the port keeps that.
pub trait NavmeshProbe {
    fn local_bounds(&self) -> (Vector3, Vector3);
    fn transform(&self) -> Matrix;
    fn transform_inverse(&self) -> Matrix;
    fn owns(&self, point: Vector3) -> bool;
}

/// `IPhysics::findDropPoint` (`physics_handler.cpp`), called on the
/// already-transformed world point with `y` filled in.
pub trait DropPhysics {
    fn drop_point(&mut self, point: Vector3) -> Option<f32>;
}

/// `ChunkWaypointGenerator::generate`'s single `pChunk_` plus its
/// `PhysicsHandler`: one probe, one physics object bound to that
/// chunk's space.
pub struct NavmeshChunk<P, F> {
    probe: P,
    physics: F,
}

impl<P: NavmeshProbe, F: FloodPhysicsLike> NavmeshChunk<P, F> {
    pub fn new(probe: P, physics: F) -> Self {
        Self { probe, physics }
    }

    /// The chunk's face and its physics as separate borrows, for
    /// [`WaypointGenerator::navgen_determine_edges_adjacent_to_other_chunks`].
    pub fn split_at_mut(&mut self) -> (&P, &mut F) {
        (&self.probe, &mut self.physics)
    }

    /// The probe half alone.
    pub fn probe(&self) -> &P {
        &self.probe
    }
}

impl<P: NavmeshProbe, F: FloodPhysicsLike> NavmeshProbe for NavmeshChunk<P, F> {
    fn local_bounds(&self) -> (Vector3, Vector3) {
        self.probe.local_bounds()
    }
    fn transform(&self) -> Matrix {
        self.probe.transform()
    }
    fn transform_inverse(&self) -> Matrix {
        self.probe.transform_inverse()
    }
    fn owns(&self, point: Vector3) -> bool {
        self.probe.owns(point)
    }
}

/// `PhysicsHandler`'s `findDropPoint` (`chunk_waypoint_generator.cpp:169`
/// passes `&phand` where the reference expects `IPhysics*`).
pub trait FloodPhysicsLike {
    fn find_drop_point(&self, point: Vector3) -> Option<f32>;
}

/// `flood.rs`'s `IPhysics` seam, boxed in a `RefCell` so the probe and
/// the physics can be borrowed at the same time
/// (`chunk_waypoint_generator.cpp:169` holds both).
impl<F> FloodPhysicsLike for std::cell::RefCell<F>
where
    F: crate::flood::FloodPhysics,
{
    fn find_drop_point(&self, point: Vector3) -> Option<f32> {
        let mut y = 0.0f32;
        self.borrow_mut()
            .find_drop_point(&point, &mut y)
            .then_some(y)
    }
}

impl<P: NavmeshProbe, F: FloodPhysicsLike> DropPhysics for NavmeshChunk<P, F> {
    fn drop_point(&mut self, point: Vector3) -> Option<f32> {
        self.physics.find_drop_point(point)
    }
}

/// A wrapper handing [`DropPhysics`] to any plain [`FloodPhysicsLike`]
/// value: the tests reuse `FieldPhysics` through it, the reference
/// threads `&PhysicsHandler` straight into the same argument.
pub struct DropPhysicsOf<T>(pub std::cell::RefCell<T>);

impl<T> FloodPhysicsLike for DropPhysicsOf<T>
where
    T: crate::flood::FloodPhysics,
{
    fn find_drop_point(&self, point: Vector3) -> Option<f32> {
        let mut y = 0.0f32;
        self.0
            .borrow_mut()
            .find_drop_point(&point, &mut y)
            .then_some(y)
    }
}

impl<T> DropPhysics for DropPhysicsOf<T>
where
    T: crate::flood::FloodPhysics,
{
    fn drop_point(&mut self, point: Vector3) -> Option<f32> {
        let mut y = 0.0f32;
        self.0
            .get_mut()
            .find_drop_point(&point, &mut y)
            .then_some(y)
    }
}

// ---------------------------------------------------------------------------
// WaypointGenerator::readTGA
// ---------------------------------------------------------------------------

impl WaypointGenerator {
    /// `identifier_` (`waypoint_generator.hpp:197`), set by
    /// [`read_tga`](Self::read_tga).
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// `readTGA` (`waypoint_generator.cpp:252-334`): load a flood
    /// result written by [`crate::flood::WaypointFlood::write_tga`].
    ///
    /// The 16 adjacency layers come first, then the 16 height layers,
    /// each layer `gridX * gridZ` entries of 4 bytes.
    pub fn read_tga(&mut self, filename: &str) -> Result<(), TgaError> {
        let mut file = std::fs::File::open(filename).map_err(|_| TgaError::Open("file"))?;

        let mut raw = [0u8; TGA_HEADER_SIZE];
        let got = file.read(&mut raw).map_err(|_| TgaError::Header("file"))?;
        if got != TGA_HEADER_SIZE {
            return Err(TgaError::Header("file"));
        }
        let hdr = parse_tga_header(&raw);

        if hdr.extra_length != TGA_EXTRA_LENGTH
            || hdr.colour_map_type != 1
            || hdr.image_type != 1
            || hdr.colour_map_start != 0
            || hdr.colour_map_length != 256
            || hdr.colour_map_depth != 24
            || hdr.x != 0
            || hdr.y != 0
            || hdr.bpp != 8
            || hdr.image_descriptor != 0
        {
            return Err(TgaError::InvalidHeader("file"));
        }

        self.init(
            (hdr.width as i32) / TGA_PIXELS_PER_CELL,
            (hdr.height as i32) / TGA_PIXELS_PER_CELL,
            &hdr.grid_min,
            hdr.grid_resolution,
        );

        // the Seek error arm mirrors `waypoint_generator.cpp:301` but is
        // unreachable through a `std::fs::File`: a seek over a local
        // file either succeeds or the open/read already failed.
        if file.seek(SeekFrom::Current(TGA_PALETTE as i64)).is_err() {
            return Err(TgaError::Seek("file"));
        }

        read_tga_layers(&mut file, self)?;

        self.identifier = identifier_from_path(filename);
        Ok(())
    }
}

/// The reference's two layer-read loops
/// (`waypoint_generator.cpp:307-325`): 16 adjacency layers, then 16
/// height layers, each `gridX * gridZ` four-byte entries.
fn read_tga_layers(
    file: &mut impl std::io::Read,
    gener: &mut WaypointGenerator,
) -> Result<(), TgaError> {
    let (adj, hgt) = gener.grids_mut();
    for layer in adj.iter_mut().take(MAX_HEIGHTS) {
        for e in layer.iter_mut() {
            let mut buf = [0u8; 4];
            if file.read(&mut buf).map_err(|_| TgaError::Read("file"))? != 4 {
                return Err(TgaError::Read("file"));
            }
            e.all = u32::from_le_bytes(buf);
        }
    }
    for layer in hgt.iter_mut().take(MAX_HEIGHTS) {
        for h in layer.iter_mut() {
            let mut buf = [0u8; 4];
            if file.read(&mut buf).map_err(|_| TgaError::Read("file"))? != 4 {
                return Err(TgaError::Read("file"));
            }
            *h = f32::from_le_bytes(buf);
        }
    }
    Ok(())
}

fn parse_tga_header(raw: &[u8]) -> TgaHeader {
    let words = &raw[18..18 + 16];
    TgaHeader {
        extra_length: raw[0],
        colour_map_type: raw[1],
        image_type: raw[2],
        colour_map_start: u16::from_le_bytes(raw[3..5].try_into().unwrap()),
        colour_map_length: u16::from_le_bytes(raw[5..7].try_into().unwrap()),
        colour_map_depth: raw[7],
        x: u16::from_le_bytes(raw[8..10].try_into().unwrap()),
        y: u16::from_le_bytes(raw[10..12].try_into().unwrap()),
        width: u16::from_le_bytes(raw[12..14].try_into().unwrap()),
        height: u16::from_le_bytes(raw[14..16].try_into().unwrap()),
        bpp: raw[16],
        image_descriptor: raw[17],
        grid_min: Vector3::new(
            f32::from_le_bytes(words[0..4].try_into().unwrap()),
            f32::from_le_bytes(words[4..8].try_into().unwrap()),
            f32::from_le_bytes(words[8..12].try_into().unwrap()),
        ),
        grid_resolution: f32::from_le_bytes(words[12..16].try_into().unwrap()),
    }
}

/// `identifier_` gets the filename with everything up to and including
/// the last `/` dropped (`waypoint_generator.cpp:326-331`).
fn identifier_from_path(filename: &str) -> String {
    match filename.rfind('/') {
        Some(i) => filename[i + 1..].to_string(),
        None => filename.to_string(),
    }
}

// ---------------------------------------------------------------------------
// WaypointGenerator::toVector3
// ---------------------------------------------------------------------------

impl WaypointGenerator {
    /// `toVector3` (`waypoint_generator.cpp:2345-2360`): a vertex's grid
    /// coordinates back to the chunk's local frame.
    ///
    /// The y lookup walks the 16 height layers in order and keeps the
    /// first height inside the polygon's own range; a vertex no layer
    /// covers lands `0.01` above the polygon's top. The `navgen_` prefix
    /// separates this workflow face from the generator's own `to_vector3`
    /// once both generations share the crate.
    pub(crate) fn navgen_to_vector3(&self, v: (f32, f32), polygon: &PolygonDef) -> Vector3 {
        let (origin, resolution) = self.grid_origin();
        let hgt = self.hgt_grids();
        let width = self.grid_x();
        let height = self.grid_z();
        let mut p = Vector3::new(
            v.0 * resolution + origin.x,
            0.0,
            v.1 * resolution + origin.z,
        );
        // the reference indexes `hgtGrids_[g]` with raw `(int)v.x +
        // gridX_*(int)v.y`, reading out of bounds for vertices outside
        // the grid; the port clamps into the grid so the lookup stays
        // defined (the y value only feeds the in-range test).
        let cell = |v: (f32, f32)| -> usize {
            let ix = (v.0 as i32).clamp(0, width as i32 - 1) as usize;
            let iz = (v.1 as i32).clamp(0, height as i32 - 1) as usize;
            ix + iz * width as usize
        };
        let mut found = false;
        for layer in hgt.iter().take(MAX_HEIGHTS) {
            p.y = layer[cell(v)];
            if p.y >= polygon.min_height && p.y <= polygon.max_height {
                found = true;
                break;
            }
        }
        if !found {
            p.y = polygon.max_height + 0.01;
        }
        p
    }

    /// `toVector3` with the chunk's inverse transform applied
    /// (`waypoint_generator.cpp:2359`).
    fn to_world(&self, v: (f32, f32), polygon: &PolygonDef, probe: &dyn NavmeshProbe) -> Vector3 {
        probe
            .transform_inverse()
            .apply_point(&self.navgen_to_vector3(v, polygon))
    }
}

// ---------------------------------------------------------------------------
// WaypointGenerator::extendThroughUnboundPortals
// ---------------------------------------------------------------------------

impl WaypointGenerator {
    /// `extendThroughUnboundPortals` (`waypoint_generator.cpp:2369-2628`):
    /// grow one step past each unbound, external portal.
    ///
    /// `unbound_portals` is the reference's collected `ubPortals` list —
    /// every portal in `pChunk->bounds()`'s `unboundPortals_` that is
    /// not `isExtern` was skipped there, so the caller filters. Planes
    /// sit in the chunk's local frame, as `toWorld` returns them.
    ///
    /// The reference's two error branches (`extraExtendPoints.size() !=
    /// 1` at `:2498-2502`, and non-consecutive edges at
    /// `:2515-2524` — reachable when one edge qualifies for two
    /// portals and is pushed twice) each abort the polygon with an
    /// `ERROR_MSG` and `continue`; the port records them in `skipped`
    /// so a caller can see that the run was not clean.
    pub fn extend_through_unbound_portals(
        &mut self,
        unbound_portals: &[PlaneEq],
        probe: &dyn NavmeshProbe,
    ) -> Vec<&'static str> {
        if unbound_portals.is_empty() {
            return Vec::new();
        }

        let mut skipped: Vec<&'static str> = Vec::new();
        let mut new_polys: Vec<PolygonDef> = Vec::new();

        for p in 0..self.polygons.len() {
            let polygon = self.polygons[p].clone();
            let vertex_count = polygon.vertices.len();
            let mut extend_points: Vec<(usize, Vector2)> = Vec::new();

            for v in 0..vertex_count {
                let c = &polygon.vertices[v];
                let n = &polygon.vertices[(v + 1) % vertex_count];

                if c.adj_to_another_chunk {
                    continue;
                }
                if c.adj_nav_poly != 0 {
                    continue;
                }
                if c.angles % 2 != 0 {
                    continue;
                }

                let c_tv = self.to_world(c.pos, &polygon, probe);
                let n_tv = self.to_world(n.pos, &polygon, probe);

                for portal in unbound_portals {
                    if portal.distance_to(&c_tv) < 0.45 && portal.distance_to(&n_tv) < 0.45 {
                        // the line direction rotated 90 degrees
                        // anticlockwise
                        let mut extend_dir = Vector2::new(-(n.pos.1 - c.pos.1), n.pos.0 - c.pos.0);
                        extend_dir.normalise();
                        extend_points.push((v, extend_dir));
                    }
                }
            }

            if extend_points.is_empty() {
                continue;
            }

            // the end points of edges that are not already start points
            let mut extra: Vec<(usize, Vector2)> = Vec::new();
            for i in 0..extend_points.len() {
                let found = (0..extend_points.len())
                    .any(|j| extend_points[j].0 == (extend_points[i].0 + 1) % vertex_count);
                if !found {
                    extra.push(((extend_points[i].0 + 1) % vertex_count, extend_points[i].1));
                }
            }

            if extra.len() != 1 {
                skipped.push(EXTEND_NEEDS_ONE_END);
                continue;
            }

            // step back from the single end point to the start
            let mut ordered: Vec<(usize, Vector2)> = Vec::new();
            let mut had_error = false;
            for i in 0..extend_points.len() {
                let want = ((extra[0].0 as i32 - i as i32 - 1 + vertex_count as i32)
                    % vertex_count as i32) as usize;
                let hit = extend_points
                    .iter()
                    .find(|ep| ep.0 == want)
                    .copied()
                    .unwrap_or((0, Vector2::new(0.0, 0.0)));
                if extend_points.iter().any(|ep| ep.0 == want) {
                    ordered.push(hit);
                } else {
                    had_error = true;
                }
            }
            if had_error {
                skipped.push(NON_CONSECUTIVE_EDGES);
                continue;
            }

            let new_index = self.polygons.len() + new_polys.len() + 1;
            let mut new_poly = PolygonDef {
                min_height: polygon.min_height,
                max_height: polygon.max_height,
                set: polygon.set,
                vertices: Vec::new(),
            };

            // the most right hand edge, adjacent to the existing polygon
            {
                let mut to_add = polygon.vertices[extra[0].0];
                to_add.angles = 0;
                to_add.adj_nav_poly = p as i32 + 1;
                to_add.adj_to_another_chunk = false;
                let target = ordered[0].0;
                self.polygons[p].vertices[target].adj_nav_poly = new_index as i32;
                self.polygons[p].vertices[target].adj_to_another_chunk = false;
                new_poly.vertices.push(to_add);
            }

            // the remaining edges, plus the left hand vertical edge
            for i in 0..ordered.len() {
                let mut to_add = polygon.vertices[ordered[i].0];
                to_add.angles = 0;
                to_add.adj_nav_poly = 0;
                to_add.adj_to_another_chunk = false;
                if i < ordered.len() - 1 {
                    to_add.adj_nav_poly = p as i32 + 1;
                    let next = ordered[i + 1].0;
                    self.polygons[p].vertices[next].adj_nav_poly = new_index as i32;
                    self.polygons[p].vertices[next].adj_to_another_chunk = false;
                }
                new_poly.vertices.push(to_add);
            }

            // the edges that face the other chunk
            for i in (0..ordered.len()).rev() {
                let mut to_add = polygon.vertices[ordered[i].0];
                to_add.pos = (to_add.pos.0 + ordered[i].1.x, to_add.pos.1 + ordered[i].1.y);
                to_add.angles = 0;
                to_add.adj_nav_poly = 0;
                to_add.adj_to_another_chunk = true;
                new_poly.vertices.push(to_add);
            }

            // the right hand vertical edge
            {
                let mut to_add = polygon.vertices[extra[0].0];
                to_add.pos = (to_add.pos.0 + ordered[0].1.x, to_add.pos.1 + ordered[0].1.y);
                to_add.angles = 0;
                to_add.adj_to_another_chunk = false;
                to_add.adj_nav_poly = 0;
                new_poly.vertices.push(to_add);
            }

            new_polys.push(new_poly);
        }

        for poly in new_polys {
            self.polygons.push(poly);
        }

        skipped
    }
}

// ---------------------------------------------------------------------------
// WaypointGenerator::determineEdgesAdjacentToOtherChunks
// ---------------------------------------------------------------------------

impl WaypointGenerator {
    /// `determineEdgesAdjacentToOtherChunks`
    /// (`waypoint_generator.cpp:2632-2730`): mark the edges that really
    /// sit outside the chunk.
    ///
    /// Returns the reference's per-polygon portal crossing counts — all
    /// zero on this side, because the field the reference increments is
    /// never assigned anywhere in the method, so the odd-count warning
    /// at `:2721` is unreachable here as well. The `navgen_` prefix
    /// separates this workflow face from the generator's own
    /// `determine_edges_adjacent_to_other_chunks` once both generations
    /// share the crate.
    pub fn navgen_determine_edges_adjacent_to_other_chunks(
        &mut self,
        probe: &dyn NavmeshProbe,
        physics: &mut dyn DropPhysics,
    ) -> Vec<&'static str> {
        for poly in &mut self.polygons {
            for vertex in &mut poly.vertices {
                vertex.adj_to_another_chunk = false;
            }
        }

        let (min, max) = probe.local_bounds();

        for p in 0..self.polygons.len() {
            let polygon = self.polygons[p].clone();
            let vertex_count = polygon.vertices.len();

            // a polygon wrapping all four corners owns the whole chunk
            let mut outsides = 0;
            for v in 0..vertex_count {
                let p1 = self.to_world(polygon.vertices[v].pos, &polygon, probe);
                if p1.x < min.x && p1.z < min.z {
                    outsides |= 1;
                }
                if p1.x < min.x && p1.z > max.z {
                    outsides |= 2;
                }
                if p1.x > max.x && p1.z < min.z {
                    outsides |= 4;
                }
                if p1.x > max.x && p1.z > max.z {
                    outsides |= 8;
                }
            }
            if outsides == 1 + 2 + 4 + 8 {
                for vertex in &mut self.polygons[p].vertices {
                    vertex.adj_to_another_chunk = true;
                }
                continue;
            }

            for v in 0..vertex_count {
                if polygon.vertices[v].adj_nav_poly > 0 {
                    continue;
                }

                let p1 = self.to_world(polygon.vertices[v].pos, &polygon, probe);
                let p2 = self.to_world(
                    polygon.vertices[(v + 1) % vertex_count].pos,
                    &polygon,
                    probe,
                );

                let (mut p1, mut p2) = (
                    probe.transform().apply_point(&p1),
                    probe.transform().apply_point(&p2),
                );
                p1.y = polygon.max_height + 1.0;
                p2.y = polygon.max_height + 1.0;

                if let Some(y) = physics.drop_point(p1) {
                    p1.y = y;
                }
                if let Some(y) = physics.drop_point(p2) {
                    p2.y = y;
                }

                if !probe.owns(p1) && !probe.owns(p2) {
                    let mid = (p1 + p2) / 2.0;
                    if !probe.owns(mid) {
                        self.polygons[p].vertices[v].adj_to_another_chunk = true;
                    }
                }
            }
        }

        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// the per-girth workflow
// ---------------------------------------------------------------------------

/// One `NavGenUDO::s_girthSeeds` entry (`navgen.cpp:1611-1633`).
#[derive(Clone, Copy, Debug)]
pub struct GirthSeed {
    /// The seed's world position.
    pub position: Vector3,
    /// The girth to generate.
    pub girth: f32,
    /// The half-extent of the box the seed claims.
    pub generate_range: f32,
}

/// `compileGirthsList` (`navgen.cpp:1601-1655`): the girths every chunk
/// gets, plus one per girth seed whose box reaches the chunk.
///
/// `always_generate` is `g_girthsAlwaysGenerate`; the returned list keeps
/// the always list first, in order, and appends a seed's girth only when
/// no earlier entry already carries that exact float.
pub fn compile_girths_list(
    always_generate: &[f32],
    seeds: &[GirthSeed],
    chunk_bbox: &BoundBox,
) -> Vec<f32> {
    let mut girths = always_generate.to_vec();

    for seed in seeds {
        let range = seed.generate_range;
        let seed_box = BoundBox::new(
            Vector3::new(
                seed.position.x - range / 2.0,
                seed.position.y - range / 2.0,
                seed.position.z - range / 2.0,
            ),
            Vector3::new(
                seed.position.x + range / 2.0,
                seed.position.y + range / 2.0,
                seed.position.z + range / 2.0,
            ),
        );

        if !seed_box.intersects(chunk_bbox) {
            continue;
        }
        if !girths.contains(&seed.girth) {
            girths.push(seed.girth);
        }
    }

    girths
}

/// `NavGenGenerating`'s flood half (`chunk_waypoint_generator.cpp:139`):
/// the grid the generator will be seeded with.
pub trait FloodStage {
    fn flooded(&self) -> bool;
    fn width(&self) -> i32;
    fn height(&self) -> i32;
    fn min_bounds(&self) -> Vector3;
    fn resolution(&self) -> f32;
    fn adjacency(&self) -> &[Vec<AdjGridElt>];
    fn heights(&self) -> &[Vec<f32>];
}

impl<T: FloodStage + ?Sized> FloodStage for &T {
    fn flooded(&self) -> bool {
        T::flooded(self)
    }
    fn width(&self) -> i32 {
        T::width(self)
    }
    fn height(&self) -> i32 {
        T::height(self)
    }
    fn min_bounds(&self) -> Vector3 {
        T::min_bounds(self)
    }
    fn resolution(&self) -> f32 {
        T::resolution(self)
    }
    fn adjacency(&self) -> &[Vec<AdjGridElt>] {
        T::adjacency(self)
    }
    fn heights(&self) -> &[Vec<f32>] {
        T::heights(self)
    }
}

impl FloodStage for crate::flood::WaypointFlood {
    fn flooded(&self) -> bool {
        !self.adj_grids()[0].is_empty()
    }
    fn width(&self) -> i32 {
        self.xsize()
    }
    fn height(&self) -> i32 {
        self.zsize()
    }
    fn min_bounds(&self) -> Vector3 {
        *self.min()
    }
    fn resolution(&self) -> f32 {
        self.resolution()
    }
    fn adjacency(&self) -> &[Vec<AdjGridElt>] {
        self.adj_grids()
    }
    fn heights(&self) -> &[Vec<f32>] {
        self.hgt_grids()
    }
}

/// `ChunkWaypointGenerator::generate` (`chunk_waypoint_generator.cpp:150-189`):
/// seed the generator from the flood result, build the polygons, decide
/// which edges border another chunk, streamline, extend the unbound
/// portals and split the sets.
#[allow(clippy::too_many_arguments)]
pub fn generate_from_flood(
    gener: &mut WaypointGenerator,
    flood: &dyn FloodStage,
    probe: &dyn NavmeshProbe,
    physics: &mut dyn DropPhysics,
    portal_planes: &[PlaneEq],
    annotate: bool,
    frontier_pts: &[Vector3],
    chunk_name: &str,
) -> Vec<&'static str> {
    gener.init(
        flood.width(),
        flood.height(),
        &flood.min_bounds(),
        flood.resolution(),
    );
    for g in 0..MAX_HEIGHTS {
        gener.adj_grids_mut()[g] = flood.adjacency()[g].clone();
        gener.hgt_grids_mut()[g] = flood.heights()[g].clone();
    }

    // annotation is a client-side visualisation pass with no server
    // effect (`chunk_waypoint_generator.cpp:172-177`); the flag is kept
    // so the caller sees the reference's call shape.
    let _ = annotate;

    gener.generate();

    let mut skipped = gener.navgen_determine_edges_adjacent_to_other_chunks(probe, physics);

    gener.streamline();

    skipped.extend(gener.extend_through_unbound_portals(portal_planes, probe));

    gener.calculate_set_membership(frontier_pts, chunk_name);

    skipped
}

/// `doGenerate`'s per-girth loop (`navgen.cpp:1886-1899`), with the
/// chunk section, message box and error log arms left to the caller:
/// for each girth, flood, generate, then output — the first girth
/// writing over the old records — and clear the dirty flag when the run
/// completes.
pub struct GirthWorkflow<F> {
    flood: F,
    dirtied: bool,
}

impl<F: FloodStage> GirthWorkflow<F> {
    /// `ChunkWaypointGenerator`'s `modified_ = true` start
    /// (`chunk_waypoint_generator.cpp:78-107`).
    pub fn new(flood: F) -> Self {
        Self {
            flood,
            dirtied: true,
        }
    }

    /// `outputDirtyFlag(false)` (`navgen.cpp:1898`).
    pub fn output_dirty_flag(&mut self, dirty: bool) {
        self.dirtied = dirty;
    }

    /// The dirty flag's current value.
    pub fn dirty(&self) -> bool {
        self.dirtied
    }

    /// Run the loop. `generate` and `output` are the caller's
    /// `ChunkWaypointGenerator::generate`/`output`; `should_stop` is
    /// `g_pleaseShutdown` and tripping it aborts the run.
    pub fn run<G, O>(
        &mut self,
        girths: &[f32],
        mut generate: G,
        mut output: O,
        should_stop: &dyn Fn() -> bool,
    ) -> Result<usize, GirthWorkflowError>
    where
        G: FnMut(&mut WaypointGenerator, &F, f32),
        O: FnMut(&WaypointGenerator, f32, bool),
    {
        if !self.flood.flooded() {
            return Err(GirthWorkflowError::NotFlooded);
        }

        let mut count = 0usize;
        for (gi, &girth) in girths.iter().enumerate() {
            if should_stop() {
                return Err(GirthWorkflowError::Shutdown);
            }
            let mut gener = WaypointGenerator::new();
            generate(&mut gener, &self.flood, girth);
            if should_stop() {
                return Err(GirthWorkflowError::Shutdown);
            }
            output(&gener, girth, gi == 0);
            count += 1;
        }

        self.output_dirty_flag(false);
        Ok(count)
    }
}

/// Why the girth loop stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GirthWorkflowError {
    /// `g_pleaseShutdown` tripped between stages.
    Shutdown,
    /// `waitChunkLoaded` gave an empty flood result.
    NotFlooded,
}

#[cfg(test)]
mod tests;
