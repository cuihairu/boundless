//! OPT-10-01-02: spatial streaming load, optimized — the candidate
//! evaluated against the OPT-10-01-01 baseline.
//!
//! The streaming cycle the server actually runs is load, unload behind
//! the camera, and reload on return — the reload re-reads and re-parses
//! every `.chunk` file. The candidate is the shipped section cache
//! (`BwResource::open_section`, the OPT-04-01-02 LRU) serving the same
//! `load_and_bind` resolver face: the first load fills the cache, every
//! reload is a hit.
//!
//! The same script runs through both resolvers — full cold start,
//! unload two cells, reload them through the shipped scan, then query
//! the whole probe grid again — and the maps must come back identical:
//! same bound sets, same find answers, same collide distances. The
//! acceptance **报告峰值内存及加载速度** is the region peak (a
//! live-bytes high-water in the counting allocator) and the reload
//! speed, printed for the review doc, never asserted.
//!
//! ```sh
//! cargo test -p chunk --test map_stream_opt
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
use resources::bwresource::BwResource;
use resources::datasection::{self, DataSection};

// ---------------------------------------------------------------------------
// The peak-tracking allocator: live-bytes high-water per region.
// ---------------------------------------------------------------------------

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            LIVE.with(|l| l.set(l.get() + layout.size()));
            let live = LIVE.with(Cell::get);
            PEAK.with(|p| p.set(p.get().max(live)));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Blocks allocated outside the region overstay in the live count;
        // the peak stays an upper bound.
        if TRACKING.with(Cell::get) {
            LIVE.with(|l| l.set(l.get().saturating_sub(layout.size())));
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocation count and live-bytes high-water of the region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Stats {
    allocs: usize,
    peak_bytes: usize,
}

fn tracked<T>(body: impl FnOnce() -> T) -> (T, Stats) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    let stats = Stats {
        allocs: ALLOCS.with(Cell::get),
        peak_bytes: PEAK.with(Cell::get),
    };
    (value, stats)
}

// ---------------------------------------------------------------------------
// The fixture space and the two resolvers.
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

/// The shipped baseline resolver: every call reads the file and parses.
fn raw_resolver(name: &str) -> Option<DataSection> {
    let raw = std::fs::read(space_root().join(name)).ok()?;
    datasection::parse("", &raw).ok()
}

/// The candidate resolver: the shipped section cache. The first open
/// fills the LRU, every later open of the same file is a hit.
fn cached_resolver() -> impl Fn(&str) -> Option<DataSection> {
    let root = space_root();
    let resource = BwResource::init_paths(&[root.to_str().expect("the fixture root is utf-8")]);
    move |name: &str| {
        resource
            .open_section(name, false)
            .map(|section| (*section).clone())
    }
}

// The terrain slabs (two half-cell plates at 10 m per queried cell),
// the reference fixture blob's binary face.
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

/// The loaded outside cells the queries run over.
const LOADED_CELLS: [(i32, i32); 4] = [(0, 0), (0, 1), (1, 0), (1, 1)];

/// The two cells the streaming cycle unloads behind the camera.
const RELOADED_CELLS: [(i32, i32); 2] = [(0, 1), (1, 0)];

fn expected_bound() -> Vec<String> {
    let mut ids: Vec<String> = LOADED_CELLS
        .iter()
        .map(|(x, z)| format!("{x:04x}{z:04x}o"))
        .collect();
    ids.push("001x000x/sep/00110000o".to_string());
    ids.push("00000000i".to_string());
    ids.sort();
    ids
}

/// The full streaming script through one resolver: cold start, unload
/// two cells, reload them through the shipped scan, then query the
/// whole grid again. Every phase is measured; the query answers all
/// three times must agree.
struct Script {
    /// The six identifiers the cold start bound.
    bound: Vec<String>,
    /// The identifiers the reload bound (the scan's two candidates).
    reloaded: Vec<String>,
    /// The probe grid's find answers, after reload.
    finds: Vec<Option<String>>,
    /// The probe grid's collide distances, after reload.
    dists: Vec<f32>,
    cold: (Stats, f64),
    reload: (Stats, f64),
}

fn run_script(resolver: &dyn Fn(&str) -> Option<DataSection>) -> Script {
    // Cold start.
    let started = Instant::now();
    let ((mut space, cold_bound), cold_stats) = tracked(|| {
        let mapping = load_mapping();
        let gathered = mapping.gather_chunks(&space_root());
        let candidates: Vec<String> = gathered.into_iter().collect();
        let mut space = ChunkSpace::new(1, vec![mapping]);
        let bound = space.load_and_bind(&candidates, resolver);
        for (gx, gz) in LOADED_CELLS {
            space.focus_chunk(&format!("{gx:04x}{gz:04x}o"));
            for obstacle in cell_slabs(gx, gz) {
                space.add_obstacle(obstacle);
            }
        }
        space.focus_chunk("00000000i");
        (space, bound)
    });
    let cold_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(cold_bound.len(), 6, "the cold start binds the six files");
    let bound: Vec<String> = space.bind_order().to_vec();

    // The streaming unload: two cells go out behind the camera.
    for (gx, gz) in RELOADED_CELLS {
        let id = format!("{gx:04x}{gz:04x}o");
        space.remove_chunk(&id);
    }

    // The shipped scan re-lists them; the reload rebinds both.
    let started = Instant::now();
    let (reloaded, reload_stats) = tracked(|| {
        let camera = Vector3::new(50.0, 0.0, 50.0);
        let worthy = space.scan(&camera, 2000.0);
        let reloaded = space.load_and_bind(&worthy, resolver);
        for (gx, gz) in RELOADED_CELLS {
            let id = format!("{gx:04x}{gz:04x}o");
            assert!(reloaded.contains(&id), "{id} reloaded");
            space.focus_chunk(&id);
            for obstacle in cell_slabs(gx, gz) {
                space.add_obstacle(obstacle);
            }
        }
        reloaded
    });
    let reload_ms = started.elapsed().as_secs_f64() * 1_000.0;

    // The probe grid over the whole loaded window, after the cycle.
    let mut finds = Vec::new();
    let mut dists = Vec::new();
    for (gx, gz) in LOADED_CELLS {
        let (ox, oz) = ((gx as f32) * 100.0, (gz as f32) * 100.0);
        for i in 0..5 {
            for j in 0..5 {
                let point = Vector3::new(
                    ox + 10.0 + (i as f32) * 20.0,
                    0.0,
                    oz + 10.0 + (j as f32) * 20.0,
                );
                finds.push(
                    space
                        .find_chunk_from_point(&point)
                        .map(|c| c.identifier.clone()),
                );
                dists.push(space.collide(
                    Vector3::new(point.x, 50.0, point.z),
                    Vector3::new(point.x, -10.0, point.z),
                    &mut StoppingCallback,
                ));
            }
        }
    }

    Script {
        bound,
        reloaded,
        finds,
        dists,
        cold: (cold_stats, cold_ms),
        reload: (reload_stats, reload_ms),
    }
}

fn probe_answers_are_the_map(script: &Script) {
    let mut bound = script.bound.clone();
    bound.sort();
    assert_eq!(
        bound,
        expected_bound(),
        "the cold start binds the six files"
    );
    assert_eq!(script.reloaded.len(), 2, "the reload rebinds two cells");
    for (index, found) in script.finds.iter().enumerate() {
        let cell = &LOADED_CELLS[index / 25];
        let outside = format!("{:04x}{:04x}o", cell.0, cell.1);
        let acceptable = [outside, "00000000i".to_string()];
        assert!(
            found.as_ref().is_some_and(|id| acceptable.contains(id)),
            "probe {index}: {found:?} is not the owning chunk"
        );
    }
    for (index, dist) in script.dists.iter().enumerate() {
        assert!(
            (dist - 40.0).abs() < 1e-3,
            "probe {index}: the ray stops at the slab, got {dist}"
        );
    }
}

#[test]
fn the_streaming_reload_reports_peak_memory_and_load_speed() {
    // The candidate's cached parse equals the shipped raw parse.
    let raw = raw_resolver("00000000o.chunk").expect("the chunk file parses");
    let cached = cached_resolver()("00000000o.chunk").expect("the cache serves it");
    assert_eq!(raw, cached, "the cached section is the same tree");

    // The same script through both resolvers; the candidate keeps one
    // cache across its whole script, so its warm phase actually hits.
    let resolver = cached_resolver();
    let shipped = run_script(&raw_resolver);
    let candidate = run_script(&resolver);
    probe_answers_are_the_map(&shipped);
    probe_answers_are_the_map(&candidate);
    assert_eq!(shipped.bound, candidate.bound);
    assert_eq!(shipped.reloaded, candidate.reloaded);
    assert_eq!(shipped.finds, candidate.finds);
    for (a, b) in shipped.dists.iter().zip(&candidate.dists) {
        assert!((a - b).abs() < 1e-5, "the collide answers agree");
    }

    // A second space over the same geometry, fully warm: the shared
    // geometry's load speed is the streaming win's headline.
    let started = Instant::now();
    let (warm_bound, warm_stats) = tracked(|| {
        let mapping = load_mapping();
        let gathered = mapping.gather_chunks(&space_root());
        let candidates: Vec<String> = gathered.into_iter().collect();
        let mut space = ChunkSpace::new(2, vec![mapping]);
        space.load_and_bind(&candidates, &resolver)
    });
    let warm_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(
        warm_bound.len(),
        6,
        "the warm load binds the same six files"
    );

    println!(
        "STREAM-COLD-RAW ms={:.3} allocs={} peak_bytes={}",
        shipped.cold.1, shipped.cold.0.allocs, shipped.cold.0.peak_bytes
    );
    println!(
        "STREAM-COLD-CACHED ms={:.3} allocs={} peak_bytes={}",
        candidate.cold.1, candidate.cold.0.allocs, candidate.cold.0.peak_bytes
    );
    println!(
        "STREAM-RELOAD-RAW ms={:.3} allocs={} peak_bytes={}",
        shipped.reload.1, shipped.reload.0.allocs, shipped.reload.0.peak_bytes
    );
    println!(
        "STREAM-RELOAD-CACHED ms={:.3} allocs={} peak_bytes={}",
        candidate.reload.1, candidate.reload.0.allocs, candidate.reload.0.peak_bytes
    );
    println!(
        "STREAM-WARM-FULL ms={:.3} allocs={} peak_bytes={}",
        warm_ms, warm_stats.allocs, warm_stats.peak_bytes
    );
}

#[test]
fn the_cycle_reruns_to_the_same_map() {
    let first = run_script(&cached_resolver());
    let second = run_script(&cached_resolver());
    probe_answers_are_the_map(&first);
    probe_answers_are_the_map(&second);
    assert_eq!(first.bound, second.bound);
    assert_eq!(first.reloaded, second.reloaded);
    assert_eq!(first.finds, second.finds);
    for (a, b) in first.dists.iter().zip(&second.dists) {
        assert!((a - b).abs() < 1e-5, "the collide answers reproduce");
    }
}
