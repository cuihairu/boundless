//! OPT-10-01-03: collision and navigation queries, optimized — the
//! shipped cache face against the recompute face, tails saved.
//!
//! The navigation walk is the waypoint chain an NPC follows: six
//! single-waypoint sets wired permissive both ways (the reference
//! `bind()` shape), all outside chunks so no shell boundary bypasses
//! the set-path cache. The cached face is `Navigator::find_path` with
//! `ReadWriteCache` riding the member `NavigatorCache` hop by hop; the
//! recompute face is `navigatePathPoints` (`findFullPath`, a scratch
//! cache per call) answering the same route from the same starts. The
//! routes must agree structurally; the cache report (per-hop
//! allocations, the set-path size the cursor pops down) and the
//! per-call tails are printed for the review doc, never asserted.
//!
//! The collision leg replays the clientquery harness world (BW-10-07-04
//! fixtures: two shell chunks, the two slats and the platform BSPs) —
//! `find_drop_point` over the six RAY probes, forty repeats each, every
//! repeat bit-identical, tails per probe printed.
//!
//! ```sh
//! cargo test -p client --test query_cache_opt
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use chunk::{Chunk, ChunkBoundary, ChunkObstacle, ChunkSpace, GeometryMapping, GridBounds};
use client::navigation::find_drop_point;
use client::navigation::navigate_path_points;
use foundation::math::{BoundBox, Matrix, PlaneEq, Vector3};
use physics::{BspTree, ClosestTriangle};
use waypoint::{CacheState, ChunkWaypointSetData, NavLoc, Navigator, Portal};

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

/// A percentile of a sorted latency list, in nanoseconds.
fn percentile(sorted: &mut [u128], fraction: usize) -> u128 {
    sorted.sort_unstable();
    sorted[sorted.len() * fraction / 100]
}

// ---------------------------------------------------------------------------
// The navigation walk world: six single-waypoint sets in a chain.
// ---------------------------------------------------------------------------

/// One single-waypoint square set (the navigation tests' fixture blob),
/// mounted on chunk identity `identity`.
fn one_set(identity: usize) -> Rc<ChunkWaypointSetData> {
    let mut b = Vec::new();
    let put_f = |out: &mut Vec<u8>, f: f32| out.extend(&f.to_le_bytes());
    let put_i = |out: &mut Vec<u8>, i: i32| out.extend(&i.to_le_bytes());
    put_i(&mut b, 0); // version
    put_f(&mut b, 0.6); // girth
    put_i(&mut b, 1); // waypoints
    put_i(&mut b, 4); // edges
    put_f(&mut b, 0.2); // waypoint x
    put_f(&mut b, 0.6); // waypoint y
    put_i(&mut b, 4); // the waypoint's edge count
    for (x, y) in [(0.0f32, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)] {
        put_f(&mut b, x);
        put_f(&mut b, y);
        put_i(&mut b, 65535);
    }
    let set = Rc::new(waypoint::parse_world_navmesh(&b).expect("decode").remove(0));
    set.set_chunk_identity(identity);
    set
}

/// The walk world: six sets in a chain, every link a permissive portal
/// wired both ways, every chunk outside so the saved set path is
/// reusable instead of bypassed.
fn walk_world() -> Vec<Rc<ChunkWaypointSetData>> {
    let sets: Vec<Rc<ChunkWaypointSetData>> = (0..6).map(one_set).collect();
    let portal = Rc::new(Portal {
        permissive: true,
        has_chunk_item: false,
    });
    for w in 0..5 {
        let west = sets[w].clone();
        let east = sets[w + 1].clone();
        west.connect(
            east.clone(),
            portal.clone(),
            2,
            0,
            Vector3::new(0.5, 0.5, 0.5),
        );
        east.connect(west, portal.clone(), 0, 0, Vector3::new(0.5, 0.5, 0.5));
    }
    for set in &sets {
        set.set_outside_chunk(true);
    }
    sets
}

/// The walk's per-set sample points (inside the unit-square waypoint,
/// distinct per set so the route's navlocs are distinguishable).
const WALK_X: [f32; 6] = [0.2, 0.3, 0.4, 0.5, 0.6, 0.8];

fn at(set: &Rc<ChunkWaypointSetData>, x: f32) -> NavLoc {
    NavLoc::from_parts(Rc::clone(set), 0, Vector3::new(x, 0.4, 0.5), 0.0)
}

/// The `almostEqual(Vector3, Vector3)` of the navigation module: the
/// prepend rule's epsilon 0.0004, strict `<` — the port's private
/// helper re-declared because the guard is what the route shape
/// depends on.
fn almost_equal_point(a: Vector3, b: Vector3) -> bool {
    let epsilon = 0.0004f32;
    (a.x - b.x).abs() < epsilon && (a.y - b.y).abs() < epsilon && (a.z - b.z).abs() < epsilon
}

/// One recorded hop of a walk: the answer, the region's allocation
/// count and the call latency.
struct Hop {
    next: NavLoc,
    count: Count,
    ns: u128,
}

/// The walk's ledger: the hops, the set-path size after each hop's
/// search (the cache cursor popping down), and the route's set
/// identities.
struct WalkLedger {
    hops: Vec<Hop>,
    set_sizes: Vec<usize>,
}

/// The cached face: one navigator, `find_path` `ReadWriteCache` riding
/// the member cache hop by hop from the chain's first set to its last.
fn cached_walk(sets: &[Rc<ChunkWaypointSetData>]) -> WalkLedger {
    let navigator = Navigator::default();
    let dst = at(&sets[5], WALK_X[5]);
    let mut current = at(&sets[0], WALK_X[0]);

    let mut hops = Vec::new();
    let mut set_sizes = Vec::new();
    let mut act = false;
    while hops.len() < 10 {
        let mut next = NavLoc::new();
        let started = Instant::now();
        let (found, count) = tracked(|| {
            navigator.find_path(
                &current,
                &dst,
                1_000_000.0,
                false,
                &mut next,
                &mut act,
                CacheState::ReadWriteCache,
            )
        });
        let ns = started.elapsed().as_nanos();
        assert!(found, "hop {} finds the route", hops.len());
        set_sizes.push(navigator.get_way_set_path_size());
        let done = next.waypoints_equal(&dst);
        hops.push(Hop { next, count, ns });
        if done {
            break;
        }
        current = hops.last().expect("a hop").next.clone();
    }
    WalkLedger { hops, set_sizes }
}

/// The recompute face: `navigatePathPoints` from each of the cached
/// walk's own starts — `findFullPath`'s scratch cache recomputes the
/// whole route every call. The camera space plays the lookup gate.
fn recompute_walk(
    space: &ChunkSpace,
    sets: &[Rc<ChunkWaypointSetData>],
    starts: &[NavLoc],
) -> Vec<(Vec<Vector3>, Count, u128)> {
    let navigator = Navigator::default();
    let dst = at(&sets[5], WALK_X[5]);
    let mut answers = Vec::new();
    for start in starts {
        let started = Instant::now();
        let (path, count) = tracked(|| {
            navigate_path_points(
                Some(space),
                &navigator,
                start,
                &dst,
                start.point(),
                dst.point(),
                1_000_000.0,
            )
            .expect("the recompute finds the route")
        });
        let ns = started.elapsed().as_nanos();
        answers.push((path, count, ns));
    }
    answers
}

#[test]
fn the_cached_walk_answers_the_route_the_recompute_walk_answers() {
    let sets = walk_world();
    let dst = at(&sets[5], WALK_X[5]);

    // The cached walk: every hop advances one set down the chain and
    // the last hop answers the destination itself.
    let cached = cached_walk(&sets);
    assert_eq!(cached.hops.len(), 5, "the chain walk takes five hops");
    for (index, hop) in cached.hops.iter().enumerate() {
        let set_position = sets
            .iter()
            .position(|s| Rc::ptr_eq(s, hop.next.p_set().expect("the next has a set")))
            .expect("the next rides a walk-world set");
        if index + 1 < 5 {
            assert_eq!(set_position, index + 1, "hop {index} advances one set");
        }
    }
    assert!(
        cached.hops.last().expect("hops").next.waypoints_equal(&dst),
        "the last hop answers the destination"
    );

    // The recompute walk from the cached walk's own starts: every
    // answer ends at the destination, and the first answer's points
    // reproduce the cached walk's route.
    let space = camera_space();
    let starts: Vec<NavLoc> = {
        let mut starts = Vec::new();
        let mut current = at(&sets[0], WALK_X[0]);
        starts.push(current.clone());
        for hop in &cached.hops[..4] {
            current = hop.next.clone();
            starts.push(current.clone());
        }
        starts
    };
    let answers = recompute_walk(&space, &sets, &starts);
    assert_eq!(answers.len(), 5);

    // The route equality: the recompute face's answer per step is the
    // remaining crossing sequence, then the hidden crossing into the
    // destination set — `findPath`'s final-hop replacement hides that
    // one from the cached walk's answer, so it is taken from the first
    // recompute — then the clipped terminus (`findFullPath` clips;
    // the raw source is prepended only when it differs from the first
    // crossing, the almostEqual guard).
    let mut clipped_dst = dst.clone();
    clipped_dst.clip();
    let crossings: Vec<Vector3> = cached.hops.iter().take(4).map(|h| h.next.point()).collect();
    let hidden = answers[0].0[answers[0].0.len() - 2];
    for (index, (path, _, _)) in answers.iter().enumerate() {
        let mut expected: Vec<Vector3> = crossings[index..].to_vec();
        expected.push(hidden);
        if !almost_equal_point(expected[0], starts[index].point()) {
            expected.insert(0, starts[index].point());
        }
        expected.push(clipped_dst.point());
        assert_eq!(path, &expected, "recompute {index}: the route shape");
    }

    // The cache report: the fill hop pays the set-level search, later
    // hops pop the saved set path (six sets down to two); the cached
    // walk's total work is strictly under the recompute walk's.
    assert_eq!(cached.set_sizes, [6, 5, 4, 3, 2], "the set path pops");
    let cached_total: usize = cached.hops.iter().map(|h| h.count.allocs).sum();
    let recompute_total: usize = answers.iter().map(|(_, c, _)| c.allocs).sum();
    assert!(
        cached_total < recompute_total,
        "the cached walk does strictly less work: {cached_total} vs {recompute_total}"
    );

    for (index, hop) in cached.hops.iter().enumerate() {
        println!(
            "NAV-CACHED hop={} allocs={} bytes={} ns={}",
            index, hop.count.allocs, hop.count.bytes, hop.ns
        );
    }
    println!("NAV-CACHED set_sizes={:?}", cached.set_sizes);
    for (index, (path, count, ns)) in answers.iter().enumerate() {
        println!(
            "NAV-RECOMPUTE step={} points={} allocs={} bytes={} ns={}",
            index,
            path.len(),
            count.allocs,
            count.bytes,
            ns
        );
    }
    println!("NAV-TOTAL cached_allocs={cached_total} recompute_allocs={recompute_total}");
}

#[test]
fn the_walk_reruns_to_the_same_ledger() {
    let sets = walk_world();
    let first = cached_walk(&sets);
    let second = cached_walk(&sets);

    assert_eq!(first.hops.len(), second.hops.len());
    for (index, (a, b)) in first.hops.iter().zip(&second.hops).enumerate() {
        assert!(
            a.next.waypoints_equal(&b.next),
            "hop {index}: the answer reproduces"
        );
        assert_eq!(
            a.count, b.count,
            "hop {index}: the allocation count reproduces"
        );
    }
    assert_eq!(first.set_sizes, second.set_sizes);
    println!(
        "NAV-RERUN allocs={:?}",
        first
            .hops
            .iter()
            .map(|h| h.count.allocs)
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// The collision leg: the clientquery harness world, tails per probe.
// ---------------------------------------------------------------------------

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

fn asset(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// The harness's grid-100 mapping over one cell.
fn mapping() -> GeometryMapping {
    GeometryMapping::new(
        "space/",
        100.0,
        GridBounds {
            min_l_grid_x: 0,
            min_l_grid_y: 0,
            max_l_grid_x: 1,
            max_l_grid_y: 1,
            min_grid_x: 0,
            min_grid_y: 0,
            max_grid_x: 1,
            max_grid_y: 1,
        },
        false,
    )
}

/// A mapped but empty space: the camera-space gate passes and the
/// navigation walk runs (the mapping keeps the reference's
/// mapping-less crash face out).
fn camera_space() -> ChunkSpace {
    ChunkSpace::new(7, vec![mapping()])
}

/// A loaded shell chunk with the harness's six inward-facing hull
/// planes (front side = the chunk's interior).
fn shell_chunk(
    mapping: &GeometryMapping,
    name: &str,
    min: Vector3,
    max: Vector3,
    faces: [(Vector3, f32); 6],
) -> Chunk {
    let mut chunk = Chunk::new(name, mapping, &Matrix::identity());
    chunk.bounding_box = BoundBox::new(min, max);
    chunk.boundaries = faces
        .iter()
        .map(|(n, d)| ChunkBoundary {
            plane: PlaneEq::new(*n, *d),
            bound_portals: Vec::new(),
            unbound_portals: Vec::new(),
        })
        .collect();
    chunk.bounds = (0..6).collect();
    chunk.is_loaded = true;
    chunk
}

/// The clientquery world: chunk A west of the seam, chunk B east, the
/// two slats and the platform registered once both chunks are focussed.
fn harness_space() -> ChunkSpace {
    let mapping = mapping();
    let mut space = ChunkSpace::new(7, vec![mapping.clone()]);

    let a_min = Vector3::new(0.0, 0.0, 0.0);
    let a_max = Vector3::new(5.0, 15.0, 10.0);
    let b_min = Vector3::new(5.0, 0.0, 0.0);
    let b_max = Vector3::new(10.0, 20.0, 10.0);

    let chunk_a = shell_chunk(
        &mapping,
        "chunkA",
        a_min,
        a_max,
        [
            (Vector3::new(1.0, 0.0, 0.0), 5.0),
            (Vector3::new(1.0, 0.0, 0.0), 0.0),
            (Vector3::new(0.0, -1.0, 0.0), -15.0),
            (Vector3::new(0.0, 1.0, 0.0), 0.0),
            (Vector3::new(0.0, 0.0, -1.0), -10.0),
            (Vector3::new(0.0, 0.0, 1.0), 0.0),
        ],
    );
    let chunk_b = shell_chunk(
        &mapping,
        "chunkB",
        b_min,
        b_max,
        [
            (Vector3::new(1.0, 0.0, 0.0), 5.0),
            (Vector3::new(-1.0, 0.0, 0.0), -10.0),
            (Vector3::new(0.0, -1.0, 0.0), -20.0),
            (Vector3::new(0.0, 1.0, 0.0), 0.0),
            (Vector3::new(0.0, 0.0, -1.0), -10.0),
            (Vector3::new(0.0, 0.0, 1.0), 0.0),
        ],
    );
    space.add_chunk(chunk_a);
    space.add_chunk(chunk_b);
    space.focus_chunk("chunkA");
    space.focus_chunk("chunkB");

    let slat_a = ChunkObstacle::new(
        BspTree::load(&asset("clientquery_slatA.bsp")).expect("slat A decodes"),
        Matrix::identity(),
        BoundBox::new(a_min, a_max),
        Some("chunkA".into()),
    );
    let slat_b = ChunkObstacle::new(
        BspTree::load(&asset("clientquery_slatB.bsp")).expect("slat B decodes"),
        Matrix::identity(),
        BoundBox::new(b_min, b_max),
        Some("chunkB".into()),
    );
    let platform = ChunkObstacle::new(
        BspTree::load(&asset("clientquery_platform.bsp")).expect("the platform decodes"),
        Matrix::identity(),
        BoundBox::new(Vector3::new(1.0, 4.0, 1.0), Vector3::new(4.0, 4.0, 4.0)),
        Some("chunkA".into()),
    );
    space.add_obstacle(slat_a);
    space.add_obstacle(slat_b);
    space.add_obstacle(platform);

    space
}

/// The RAY probes the harness hardcodes.
fn ray_probes() -> [(&'static str, Vector3); 6] {
    [
        ("GA", Vector3::new(1.0, 5.0, 1.0)),
        ("GHOLED", Vector3::new(2.5, 4.0, 1.0)),
        ("GB", Vector3::new(6.5, 7.0, 2.5)),
        ("GPLAT", Vector3::new(2.0, 6.0, 2.0)),
        ("GNONE", Vector3::new(50.0, 5.0, 50.0)),
        ("GSEAM", Vector3::new(5.0, 3.0, 3.0)),
    ]
}

const REPEATS: usize = 40;

/// The collision tail: every probe repeated, every repeat answering
/// bit-identically, tails per probe printed.
#[test]
fn the_drop_point_tails_repeat_identically() {
    let space = harness_space();

    for (name, in_pt) in ray_probes() {
        let mut ns = Vec::with_capacity(REPEATS);
        let mut first: Option<(f32, Vector3, [Vector3; 3])> = None;
        for _ in 0..REPEATS {
            let mut fdpt = ClosestTriangle::new();
            let started = Instant::now();
            let hit = find_drop_point(Some(&space), 7, in_pt, &mut fdpt)
                .expect("the camera space is found");
            ns.push(started.elapsed().as_nanos());
            match hit {
                Some((out_pt, tri)) => {
                    let dist = in_pt.y - out_pt.y;
                    match first {
                        None => first = Some((dist, out_pt, tri)),
                        Some((d0, p0, t0)) => {
                            assert_eq!(dist, d0, "{name}: the distance reproduces");
                            assert_eq!(out_pt, p0, "{name}: the drop point reproduces");
                            assert_eq!(tri, t0, "{name}: the triangle reproduces");
                        }
                    }
                }
                None => assert!(
                    first.is_none(),
                    "{name}: the miss reproduces (a later repeat hit)"
                ),
            }
        }

        ns.sort_unstable();
        println!(
            "COLLIDE-TAIL probe={} hit={} p50_us={:.3} p99_us={:.3} max_us={:.3}",
            name,
            first.is_some(),
            percentile(&mut ns, 50) as f64 / 1_000.0,
            percentile(&mut ns, 99) as f64 / 1_000.0,
            ns[REPEATS - 1] as f64 / 1_000.0,
        );
    }
}
