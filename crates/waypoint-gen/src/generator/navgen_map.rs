//! Acceptance artifacts for BW-10-07-02: the ×10 fixed map (ground 4.0
//! with the hole strip x∈[2,3) z∈[0,5), ramp steps z∈[5,10) rising
//! 4.5→9.5, obstacle slab x∈[6.5,8.5) z∈[4,6) h13, chunk seam at x=5)
//! re-run through the Rust flood + Rust BSP generator in the navgen
//! per-chunk pipeline order (`chunk_waypoint_generator.cpp:139-194`:
//! flood, generate, determine edges adjacent to other chunks,
//! streamline, set membership, asBinary), dumped in the reference
//! harness `gen_navgen_map.cpp`'s exact format and compared line by
//! line against the committed truth log `gen_navgen_map.log`. The
//! original reader face consumes the written streams in
//! `gen_navgen_paths.log`; the waypoint crate replays that against the
//! port's export bytes (`crates/waypoint/tests/genmap.rs`).
//!
//! Pipeline boundary: the reference also calls
//! `extendThroughUnboundPortals` (`:190`) between streamline and set
//! membership. That call is a no-op in the harness — the stub chunk's
//! `bounds()` is empty, i.e. both chunks are bound like navgen's
//! `canProcess` 3×3 neighbourhood guarantees — and the port omits the
//! gate entirely (`waypoint_generator.cpp:2370-2607` is registered
//! unported).

use super::WaypointGenerator;
use crate::flood::tests::{FieldPhysics, Slab};
use crate::flood::{FloodChunk, MAX_HEIGHTS, WaypointFlood};
use crate::generator::GenChunk;
use crate::view::{NavView, as_binary};
use foundation::math::vector::Vector3;

const TRUTH: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint/gen_navgen_map.log"
));

/// The ×10 fixed map world: the same slab list as
/// `gen_navgen_map.cpp:fillWorld`.
fn fixed_map_world() -> FieldPhysics {
    let mut physics = FieldPhysics {
        slabs: Vec::new(),
        max_step: 0.6,
    };
    // chunk A south: ground either side of the hole
    physics.slabs.push(Slab(0.0, 0.0, 2.0, 5.0, 4.0));
    physics.slabs.push(Slab(3.0, 0.0, 5.0, 5.0, 4.0));
    // chunk B ground: south strip, mid ring either side of the obstacle,
    // north strip
    physics.slabs.push(Slab(5.0, 0.0, 10.0, 4.0, 4.0));
    physics.slabs.push(Slab(5.0, 4.0, 6.5, 6.0, 4.0));
    physics.slabs.push(Slab(8.5, 4.0, 10.0, 6.0, 4.0));
    physics.slabs.push(Slab(5.0, 6.0, 10.0, 10.0, 4.0));
    // chunk A ramp: ten half-metre steps z∈[5,10), x∈[0,5), 4.5→9.5
    for k in 0..10i32 {
        let k = k as f32;
        let z = 5.0 + 0.5 * k;
        physics
            .slabs
            .push(Slab(0.0, z, 5.0, z + 0.5, 4.5 + 0.5 * k));
    }
    // chunk B obstacle: 9 above ground — the flood can never climb it
    physics.slabs.push(Slab(6.5, 4.0, 8.5, 6.0, 13.0));
    physics
}

/// The stub chunk both pipeline stages read: the flood probes the
/// ownership box and the space coordinate gate, the generator's
/// determine-edges pass probes the same half-open ownership box and the
/// local bounding box (`ChunkFlooder`'s box is the chunk bounding box
/// expanded by 2×reso, `chunk_flooder.cpp:98-110`; the stub `owns` is
/// half-open like the shadow `chunk/chunk.hpp` the harness compiles).
struct NavGenChunk {
    owns_box: (f32, f32, f32, f32),
    space: (f32, f32, f32, f32),
    bb: (Vector3, Vector3),
}

impl NavGenChunk {
    fn new(min_x: f32, max_x: f32) -> Self {
        Self {
            owns_box: (min_x, 0.0, max_x, 10.0),
            space: (-100.0, 100.0, -100.0, 100.0),
            bb: (
                Vector3::new(min_x, 0.0, 0.0),
                Vector3::new(max_x, 0.0, 10.0),
            ),
        }
    }
}

impl FloodChunk for NavGenChunk {
    fn owns(&self, point: &Vector3) -> bool {
        let (x0, z0, x1, z1) = self.owns_box;
        point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1
    }
    fn min_coord_x(&self) -> f32 {
        self.space.0
    }
    fn max_coord_x(&self) -> f32 {
        self.space.1
    }
    fn min_coord_z(&self) -> f32 {
        self.space.2
    }
    fn max_coord_z(&self) -> f32 {
        self.space.3
    }
}

impl GenChunk for NavGenChunk {
    fn owns(&self, point: &Vector3) -> bool {
        FloodChunk::owns(self, point)
    }
    fn local_bb(&self) -> (Vector3, Vector3) {
        self.bb
    }
}

/// The workflow seam (`workflow::NavmeshProbe`) over the stub chunk:
/// the harness runs untransformed (navgen's fixed map), so both
/// transforms are identity and the box/ownership faces are the stub's
/// own — the same answers `GenChunk` gives this module's earlier stages.
impl crate::workflow::NavmeshProbe for NavGenChunk {
    fn local_bounds(&self) -> (Vector3, Vector3) {
        self.bb
    }
    fn transform(&self) -> foundation::math::matrix::Matrix {
        foundation::math::matrix::Matrix::identity()
    }
    fn transform_inverse(&self) -> foundation::math::matrix::Matrix {
        foundation::math::matrix::Matrix::identity()
    }
    fn owns(&self, point: Vector3) -> bool {
        FloodChunk::owns(self, &point)
    }
}

/// The workflow seam (`workflow::DropPhysics`) over the flood fixture:
/// the same slab walk `FloodPhysics::find_drop_point` rides, in the
/// Option shape the workflow arm expects.
impl crate::workflow::DropPhysics for FieldPhysics {
    fn drop_point(&mut self, point: Vector3) -> Option<f32> {
        self.top_at(point.x, point.z, point.y)
    }
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for b in data {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

/// `dumpPolys` (`gen_navgen_map.cpp`), as `generator::artifacts` does.
fn dump_polys(what: &str, gener: &WaypointGenerator, out: &mut String) {
    out.push_str(&format!(
        "{} polys {} sets {}\n",
        what,
        gener.polygon_count(),
        gener.sets()
    ));
    for p in 0..gener.polygon_count() {
        out.push_str(&format!(
            "  P {p} set {} minh {:.1} maxh {:.1} vc {}\n",
            gener.poly_set(p),
            gener.min_height(p),
            gener.max_height(p),
            gener.vertex_count(p) as u32
        ));
        for v in 0..gener.vertex_count(p) {
            let vr = gener.vertex(p, v);
            out.push_str(&format!(
                "  V {v} 0 x {:.2} z {:.2} adj {} chunk {}\n",
                vr.x,
                vr.z,
                vr.adj_nav_poly,
                i32::from(vr.adj_to_another_chunk)
            ));
        }
    }
}

/// `dumpBinary` (`gen_navgen_map.cpp`), as `generator::artifacts` does.
fn dump_binary(what: &str, bin: &[u8], out: &mut String) {
    out.push_str(&format!(
        "{} bytes {} fnv {:016x}\n",
        what,
        bin.len(),
        fnv1a64(bin)
    ));
    let mut i = 0usize;
    while i < bin.len() {
        let mut hex = String::new();
        for j in 0..16 {
            if i + j < bin.len() {
                hex.push_str(&format!("{:02x}", bin[i + j]));
            }
        }
        out.push_str(&format!("HX {:04x} {}\n", i, hex));
        i += 16;
    }

    let mut ptr = 0usize;
    let mut record = 0;
    while ptr < bin.len() {
        let version = i32::from_le_bytes(bin[ptr..ptr + 4].try_into().unwrap());
        let girth = f32::from_le_bytes(bin[ptr + 4..ptr + 8].try_into().unwrap());
        let nav_poly_elts = i32::from_le_bytes(bin[ptr + 8..ptr + 12].try_into().unwrap());
        let nav_poly_edges = i32::from_le_bytes(bin[ptr + 12..ptr + 16].try_into().unwrap());
        ptr += 16;
        out.push_str(&format!(
            "R {record} version {version} girth {girth:.1} nwp {nav_poly_elts} ne {nav_poly_edges}\n"
        ));
        let ctrl = ptr;
        ptr += nav_poly_elts as usize * 12;
        for p in 0..nav_poly_elts {
            let c = ctrl + p as usize * 12;
            let minh = f32::from_le_bytes(bin[c..c + 4].try_into().unwrap());
            let maxh = f32::from_le_bytes(bin[c + 4..c + 8].try_into().unwrap());
            let vc = i32::from_le_bytes(bin[c + 8..c + 12].try_into().unwrap());
            out.push_str(&format!("  P {p} minh {minh:.1} maxh {maxh:.1} vc {vc}\n"));
        }
        for v in 0..nav_poly_edges {
            let x = f32::from_le_bytes(bin[ptr..ptr + 4].try_into().unwrap());
            let z = f32::from_le_bytes(bin[ptr + 4..ptr + 8].try_into().unwrap());
            let adj = i32::from_le_bytes(bin[ptr + 8..ptr + 12].try_into().unwrap());
            ptr += 12;
            out.push_str(&format!("  V {v} x {x:.1} z {z:.1} adj {adj}\n"));
        }
        record += 1;
    }
}

/// `runChunk` (`gen_navgen_map.cpp`): the navgen per-chunk pipeline in
/// reference order (`chunk_waypoint_generator.cpp:139-194`).
fn run_chunk(
    name: &str,
    min_x: f32,
    max_x: f32,
    physics: &mut FieldPhysics,
    seed: &Vector3,
    out: &mut String,
) -> Vec<u8> {
    // --- flood stage: ChunkFlooder::reset's face
    let mut flood = WaypointFlood::default();
    flood.set_area(
        &Vector3::new(min_x - 1.0, 0.0, -1.0),
        &Vector3::new(max_x + 1.0, 0.0, 11.0),
        0.5,
    );
    let chunk = NavGenChunk::new(min_x, max_x);
    let count = flood.fill(seed, physics, &chunk, None);
    out.push_str(&format!("FILL 0 count {count}\n"));
    flood.postfilter_add();
    flood.postfilter_remove();

    // the flood grid dump from gen_flood.cpp:146-181
    out.push_str(&format!("SCENE {name} count {count}\n"));
    let cells = (flood.xsize() * flood.zsize()) as usize;
    for g in 0..MAX_HEIGHTS {
        for i in 0..cells {
            let h = flood.hgt_grids()[g][i];
            if h > -9999.0 {
                out.push_str(&format!("H {g} {i} {h:.1}\n"));
            }
        }
    }
    for g in 0..MAX_HEIGHTS {
        for i in 0..cells {
            let all = flood.adj_grids()[g][i].all;
            for a in 0..8usize {
                let v = (all >> (a << 2)) & 0xF;
                if v != 0 {
                    out.push_str(&format!("A {g} {i} {a} {v}\n"));
                }
            }
        }
    }
    for (index, grid_id) in flood.boundary_set() {
        out.push_str(&format!("B {index} {grid_id}\n"));
    }
    out.push_str(&format!(
        "SEEDAREA {name} min {:.1} {:.1} size {} {}\n",
        flood.min().x,
        flood.min().z,
        flood.xsize(),
        flood.zsize()
    ));

    // --- generator stage
    let mut gener = WaypointGenerator::default();
    gener.init(
        flood.xsize(),
        flood.zsize(),
        flood.min(),
        flood.resolution(),
    );
    let cells = (flood.xsize() * flood.zsize()) as usize;
    for g in 0..MAX_HEIGHTS {
        gener.hgt_grids_mut()[g][..cells].copy_from_slice(&flood.hgt_grids()[g][..cells]);
        gener.adj_grids_mut()[g][..cells].copy_from_slice(&flood.adj_grids()[g][..cells]);
    }

    gener.generate();
    dump_polys("GENERATED", &gener, out);
    gener.determine_edges_adjacent_to_other_chunks(&chunk, physics);
    dump_polys("DETERMINED", &gener, out);
    gener.streamline();
    dump_polys("STREAMLINED", &gener, out);
    // extendThroughUnboundPortals is a no-op here (both chunks bound,
    // stub bounds() empty) — see the module header.
    gener.calculate_set_membership(&[Vector3::new(seed.x, seed.y, seed.z)], name);
    dump_polys("SETMEM", &gener, out);

    let final_stream = as_binary(&gener, 0.6);
    dump_binary(name, &final_stream, out);
    out.push_str(&format!("WROTE {name} {} bytes\n", final_stream.len()));
    final_stream
}

#[test]
fn the_reference_navgen_harness_reproduces_the_truth_log() {
    let mut physics = fixed_map_world();

    let mut out = String::new();
    let a = run_chunk(
        "mapA",
        0.0,
        5.0,
        &mut physics,
        &Vector3::new(1.0, 6.0, 1.0),
        &mut out,
    );
    let b = run_chunk(
        "mapB",
        5.0,
        10.0,
        &mut physics,
        &Vector3::new(6.0, 6.0, 2.0),
        &mut out,
    );
    out.push_str("RESULT OK\n");

    // the port's export is byte-identical to the reference's: the
    // committed consumer assets are these streams (md5
    // f77acd44c519b90d926d700526e1577a / e0c5fc7f1c10a2a78bcef3c6d8728ee7)
    // and gen_navgen_paths.cpp reloads them.
    assert_eq!(fnv1a64(&a), 0x3f4c_4a26_e986_5b1f, "mapA export");
    assert_eq!(fnv1a64(&b), 0xc9b5_fe18_53f8_6a48, "mapB export");

    let truth: Vec<&str> = TRUTH.lines().collect();
    let mine: Vec<&str> = out.lines().collect();
    assert_eq!(mine.len(), truth.len(), "line counts");
    for (i, (m, t)) in mine.iter().zip(truth.iter()).enumerate() {
        assert_eq!(m, t, "line {}", i + 1);
    }
}
