//! OPT-10-01-01: the map load and query baseline.
//!
//! The cold start is the shipped streaming sequence over the original
//! fixture space (`docs/review/fixtures/chunk_space`, the 18×2 grid the
//! reference settings.xml describes): parse the mapping, scan from the
//! camera, load and bind every candidate, focus the bound chunks, and
//! register the terrain slabs through the focussed-obstacle gate. The
//! region is bracketed by a counting allocator (the OPT-01-01-01
//! pattern — thread-local, calling thread only) and a VmRSS reading.
//!
//! The queries are the two shipped lookup faces over the loaded map:
//! `findChunkFromPoint` across a 5×5 sample grid per loaded cell, and
//! the client's findDropPoint shape — `ChunkSpace::collide` rays from
//! 50 m down to −10 m, stopping at the 10 m slab — timed per call and
//! reported at p50/p99/max.
//!
//! Wall-clock and memory are probe data, printed for the review doc,
//! never asserted. The structural ledger (the bound set, the find
//! answers, the collide distances, the allocation counts) is asserted
//! and must rerun identically.
//!
//! ```sh
//! cargo test -p chunk --test map_load_baseline
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chunk::{ChunkObstacle, ChunkSpace, GeometryMapping};
use foundation::math::{BoundBox, Matrix, Vector3};
use physics::{
    BSP_FILE_CUR_TOKEN, BSP_NODE_PACKED_SIZE, BspTree, COLLIDE_STOP, CollisionCallback,
    CollisionObstacle, OFFSET_32BIT_NULL, WorldTriangle,
};
use resources::datasection;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// The resident set in kB, read the way `memory_snapshot` reads it on
/// Linux; `None` where the platform has no /proc.
#[cfg(target_os = "linux")]
fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn rss_kb() -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// The fixture space (the BW-10-01-01 originals).
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("a crates/ parent")
        .parent()
        .expect("the repository root")
        .to_path_buf()
}

fn space_root() -> PathBuf {
    repo_root().join("docs/review/fixtures/chunk_space/res/space_00010001")
}

fn load_mapping() -> GeometryMapping {
    let raw = std::fs::read(space_root().join("settings.xml")).expect("settings.xml readable");
    let settings = datasection::parse("", &raw).expect("settings.xml parses");
    GeometryMapping::from_settings("space_00010001/", &settings)
}

fn resolver(name: &str) -> Option<datasection::DataSection> {
    let raw = std::fs::read(space_root().join(name)).ok()?;
    datasection::parse("", &raw).ok()
}

/// The terrain slab over one 100 m cell: the reference fixture blob's
/// binary face, one triangle at `y`, normal +z. The same blob format
/// the crate's `obstacle::test_bsp` builds (that module is pub(crate),
/// so the bytes are rebuilt here).
fn words(vals: &[u32]) -> Vec<u8> {
    vals.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn v3_bytes(v: Vector3) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&v.x.to_le_bytes());
    b.extend_from_slice(&v.y.to_le_bytes());
    b.extend_from_slice(&v.z.to_le_bytes());
    b
}

fn tri_bytes(v0: Vector3, v1: Vector3, v2: Vector3) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(&v3_bytes(v0));
    b.extend(&v3_bytes(v1));
    b.extend(&v3_bytes(v2));
    b.extend(&0u16.to_le_bytes());
    b.extend(&[0u8; 2]);
    b
}

fn node_bytes(front: i32, back: i32, normal: Vector3, d: f32, num_tris: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&front.to_le_bytes());
    b.extend_from_slice(&back.to_le_bytes());
    b.extend(&v3_bytes(normal));
    b.extend_from_slice(&d.to_le_bytes());
    b.extend_from_slice(&0i32.to_le_bytes());
    b.extend_from_slice(&num_tris.to_le_bytes());
    b.extend_from_slice(&OFFSET_32BIT_NULL.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(b.len(), BSP_NODE_PACKED_SIZE as usize);
    b
}

/// The slab blob over one cell half: `first` builds the lower triangle
/// (origin, +x corner, +z corner), `!first` the complementary upper one —
/// the reference fixture blob's binary face, one triangle at `y`,
/// normal +z.
fn slab_bsp(origin_x: f32, origin_z: f32, y: f32, first: bool) -> BspTree {
    let mut b = Vec::new();
    b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 1, 1, 0]));
    b.extend(&v3_bytes(Vector3::new(origin_x, 0.0, origin_z)));
    b.extend(&v3_bytes(Vector3::new(
        origin_x + 100.0,
        y,
        origin_z + 100.0,
    )));
    b.extend(&[0u8; 8]);
    let (v0, v1, v2) = if first {
        (
            Vector3::new(origin_x, y, origin_z),
            Vector3::new(origin_x + 100.0, y, origin_z),
            Vector3::new(origin_x, y, origin_z + 100.0),
        )
    } else {
        (
            Vector3::new(origin_x + 100.0, y, origin_z),
            Vector3::new(origin_x + 100.0, y, origin_z + 100.0),
            Vector3::new(origin_x, y, origin_z + 100.0),
        )
    };
    b.extend(&tri_bytes(v0, v1, v2));
    b.extend(&node_bytes(
        OFFSET_32BIT_NULL,
        OFFSET_32BIT_NULL,
        Vector3::new(0.0, 0.0, 1.0),
        y,
        1,
    ));
    BspTree::load(&b).expect("the slab blob loads")
}

/// The terrain over one 100 m cell: two half-slab obstacles, both at
/// `y`, so every point of the cell lands on the plate.
fn cell_slabs(gx: i32, gz: i32) -> Vec<ChunkObstacle> {
    let id = format!("{gx:04x}{gz:04x}o");
    let (ox, oz) = ((gx as f32) * 100.0, (gz as f32) * 100.0);
    let bb = BoundBox::new(
        Vector3::new(ox, 0.0, oz),
        Vector3::new(ox + 100.0, 10.0, oz + 100.0),
    );
    vec![
        ChunkObstacle::new(
            slab_bsp(ox, oz, 10.0, true),
            Matrix::identity(),
            bb,
            Some(id.clone()),
        ),
        ChunkObstacle::new(
            slab_bsp(ox, oz, 10.0, false),
            Matrix::identity(),
            bb,
            Some(id),
        ),
    ]
}

/// `COLLIDE_STOP` on every visit — the sweep stops at the first answer.
struct StoppingCallback;

impl CollisionCallback for StoppingCallback {
    fn do_visit(
        &mut self,
        _obstacle: &CollisionObstacle,
        _triangle: &WorldTriangle,
        _dist: f32,
    ) -> i32 {
        COLLIDE_STOP
    }
}

// ---------------------------------------------------------------------------
// The baseline.
// ---------------------------------------------------------------------------

/// The loaded outside cells the queries run over: the four cells with
/// a top-level `.chunk` file (the sixth file, the 4-bit-prefix cell at
/// grid (17,0), sits outside the probe window).
const LOADED_CELLS: [(i32, i32); 4] = [(0, 0), (0, 1), (1, 0), (1, 1)];

/// The six identifiers a cold start binds: every `.chunk` file on disk
/// (four origin cells, the nested prefix cell, the inside chunk).
fn expected_bound() -> Vec<String> {
    let mut ids: Vec<String> = LOADED_CELLS
        .iter()
        .map(|(x, z)| format!("{x:04x}{z:04x}o"))
        .collect();
    ids.push("001x000x/sep/00110000o".to_string());
    ids.push("00000000i".to_string());
    ids
}

/// The whole cold start — mapping parse, scan, load, bind, focus,
/// obstacle registration — measured as one region.
fn cold_start() -> (ChunkSpace, Count, Option<u64>, f64) {
    let rss_before = rss_kb();
    let started = Instant::now();
    let (space, count) = tracked(|| {
        let mapping = load_mapping();
        // The cold start enumerates the whole space directory (37
        // candidates — cells plus the inside chunk) and loads what has
        // a file.
        let gathered = mapping.gather_chunks(&space_root());
        let mut space = ChunkSpace::new(1, vec![mapping]);
        let candidates: Vec<String> = gathered.into_iter().collect();
        let bound = space.load_and_bind(&candidates, &resolver);
        assert_eq!(bound.len(), 6, "the cold start binds the six files");

        // The streaming tail: focus the loaded chunks and register the
        // terrain slabs (the focussed gate admits them into columns).
        for (gx, gz) in LOADED_CELLS {
            space.focus_chunk(&format!("{gx:04x}{gz:04x}o"));
            for obstacle in cell_slabs(gx, gz) {
                space.add_obstacle(obstacle);
            }
        }
        space.focus_chunk("00000000i");
        space
    });
    let ms = started.elapsed().as_secs_f64() * 1_000.0;
    let rss_delta = match (rss_before, rss_kb()) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };
    (space, count, rss_delta, ms)
}

/// The find and collide probes over the loaded cells: 25 points per
/// cell, each queried through both faces. Returns the find answers,
/// the collide distances, and the per-call latencies in nanoseconds.
fn run_queries(space: &ChunkSpace) -> (Vec<Option<String>>, Vec<f32>, Vec<u128>, Vec<u128>) {
    let mut finds = Vec::new();
    let mut dists = Vec::new();
    let mut find_ns = Vec::new();
    let mut collide_ns = Vec::new();

    for (gx, gz) in LOADED_CELLS {
        let (ox, oz) = ((gx as f32) * 100.0, (gz as f32) * 100.0);
        for i in 0..5 {
            for j in 0..5 {
                let point = Vector3::new(
                    ox + 10.0 + (i as f32) * 20.0,
                    0.0,
                    oz + 10.0 + (j as f32) * 20.0,
                );

                let started = Instant::now();
                let found = space
                    .find_chunk_from_point(&point)
                    .map(|c| c.identifier.clone());
                find_ns.push(started.elapsed().as_nanos());
                finds.push(found);

                let started = Instant::now();
                let dist = space.collide(
                    Vector3::new(point.x, 50.0, point.z),
                    Vector3::new(point.x, -10.0, point.z),
                    &mut StoppingCallback,
                );
                collide_ns.push(started.elapsed().as_nanos());
                dists.push(dist);
            }
        }
    }
    (finds, dists, find_ns, collide_ns)
}

/// A percentile of a sorted latency list, in nanoseconds.
fn percentile(sorted: &mut [u128], fraction: usize) -> u128 {
    sorted.sort_unstable();
    sorted[sorted.len() * fraction / 100]
}

#[test]
fn the_cold_start_saves_time_memory_and_query_tails() {
    let (space, count, rss_delta, ms) = cold_start();

    // The bound set is the five files, all bound.
    let bound: Vec<String> = space.bind_order().to_vec();
    let mut expected = expected_bound();
    expected.sort();
    let mut bound_sorted = bound.clone();
    bound_sorted.sort();
    assert_eq!(bound_sorted, expected, "the cold start binds the files");
    for id in &bound {
        assert!(space.chunk(id).is_some_and(|c| c.is_bound), "{id} bound");
    }

    // The queries: every sample point finds a chunk, and every ray
    // stops at the slab 40 m down the ray.
    let (finds, dists, mut find_ns, mut collide_ns) = run_queries(&space);
    assert_eq!(finds.len(), LOADED_CELLS.len() * 25);
    for (index, found) in finds.iter().enumerate() {
        let cell = &LOADED_CELLS[index / 25];
        let outside = format!("{:04x}{:04x}o", cell.0, cell.1);
        let acceptable = [outside, "00000000i".to_string()];
        assert!(
            found.as_ref().is_some_and(|id| acceptable.contains(id)),
            "probe {index}: {found:?} is not the owning chunk"
        );
    }
    for (index, dist) in dists.iter().enumerate() {
        assert!(
            (dist - 40.0).abs() < 1e-3,
            "probe {index}: the ray stops at the slab, got {dist}"
        );
    }

    let (find_p50, find_p99, find_max) = (
        percentile(&mut find_ns, 50),
        percentile(&mut find_ns, 99),
        *find_ns.last().unwrap(),
    );
    let (collide_p50, collide_p99, collide_max) = (
        percentile(&mut collide_ns, 50),
        percentile(&mut collide_ns, 99),
        *collide_ns.last().unwrap(),
    );
    println!(
        "MAP-COLD chunks=6 ms={ms:.3} allocs={} bytes={} rss_delta_kb={rss_delta:?}",
        count.allocs, count.bytes
    );
    println!(
        "MAP-TAIL find_p50_us={:.3} find_p99_us={:.3} find_max_us={:.3} collide_p50_us={:.3} collide_p99_us={:.3} collide_max_us={:.3}",
        find_p50 as f64 / 1_000.0,
        find_p99 as f64 / 1_000.0,
        find_max as f64 / 1_000.0,
        collide_p50 as f64 / 1_000.0,
        collide_p99 as f64 / 1_000.0,
        collide_max as f64 / 1_000.0,
    );
}

#[test]
fn the_cold_start_reruns_to_the_same_map() {
    let (first, first_count, _, first_ms) = cold_start();
    let (first_finds, first_dists, _, _) = run_queries(&first);

    let (second, second_count, _, second_ms) = cold_start();
    let (second_finds, second_dists, _, _) = run_queries(&second);

    // The map reproduces: same bind order, same find answers, same
    // collide distances, same allocation count. The latencies are
    // probe data, printed, never asserted.
    assert_eq!(first.bind_order(), second.bind_order());
    assert_eq!(first_finds, second_finds);
    for (a, b) in first_dists.iter().zip(&second_dists) {
        assert!((a - b).abs() < 1e-5, "the collide answers reproduce");
    }
    assert_eq!(first_count, second_count);
    println!(
        "MAP-RERUN ms={first_ms:.3}/{second_ms:.3} allocs={}/{} bytes={}/{}",
        first_count.allocs, second_count.allocs, first_count.bytes, second_count.bytes
    );
}
