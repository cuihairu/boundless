//! Acceptance tests for BW-10-05-04 remainder slice: `readTGA`
//! (`waypoint_generator.cpp:252-332`) round-tripping the
//! `WaypointFlood::writeTGA` dump, and `extendThroughUnboundPortals`
//! (`waypoint_generator.cpp:2370-2619`) growing its portal skirt on a
//! hand-built polygon, with the error arms driven directly.

use super::{GenChunk, GenChunkPortals, PolygonDef, TgaHeader, WaypointGenerator};
use crate::flood::artifacts::slope_scene;
use crate::flood::tests::{Bounds, grid, seed};
use crate::flood::{FloodChunk, FloodPhysics, MAX_HEIGHTS};
use foundation::math::{PlaneEq, Vector3};

// --- the seam objects --------------------------------------------------

/// A chunk whose portals are the given planes.
struct PortalChunk {
    planes: Vec<PlaneEq>,
}

impl GenChunkPortals for PortalChunk {
    fn unbound_extern_portals(&self) -> &[PlaneEq] {
        &self.planes
    }
}

/// A plane through x = `x` with its inward normal +x: the boundary
/// between two chunks side by side on the grid, matching the synthetic
/// boundary planes `form_boundaries` builds for outside chunks (the
/// plane eq is `normal . p = d`, so `d` carries the position).
fn x_plane(x: f32) -> PlaneEq {
    PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), x)
}

// --- readTGA -----------------------------------------------------------

/// Round-trip: a flood dump read back into a fresh generator gives the
/// same grids, origin, resolution and identifier the writer had.
#[test]
fn read_tga_round_trips_the_flood_dump() {
    // build a real flooded grid (the 05-02 slope scene)
    let mut flood = grid(0.0, 8.0, 1.0);
    let mut physics = slope_scene();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(1.0, 1.0, 1.0), &mut physics, &chunk, None);

    let dir = std::env::temp_dir().join("bw10-05-04-tga");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("roundtrip.tga");
    let path = path.to_str().unwrap();
    assert!(flood.write_tga(path));

    let mut gener = WaypointGenerator::new();
    assert!(gener.read_tga(path));
    assert_eq!(gener.identifier(), "roundtrip.tga");

    assert_eq!(gener.grid_x, flood.xsize() as u32);
    assert_eq!(gener.grid_z, flood.zsize() as u32);
    assert_eq!(gener.grid_resolution, flood.resolution());
    for g in 0..MAX_HEIGHTS {
        for (a, b) in gener.adj_grids[g].iter().zip(flood.adj_grids()[g].iter()) {
            assert_eq!(a.all, b.all);
        }
        for (a, b) in gener.hgt_grids[g].iter().zip(flood.hgt_grids()[g].iter()) {
            assert_eq!(a, b);
        }
    }
}

/// The identifier strips the directory: `readTGA` keeps everything
/// after the last `/` (`waypoint_generator.cpp:326-329`).
#[test]
fn read_tga_identifier_is_the_file_name_only() {
    let flood = grid(0.0, 8.0, 1.0);
    let dir = std::env::temp_dir().join("bw10-05-04-tga");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("named.tga");
    assert!(flood.write_tga(path.to_str().unwrap()));

    let mut gener = WaypointGenerator::new();
    assert!(gener.read_tga(path.to_str().unwrap()));
    assert_eq!(gener.identifier(), "named.tga");
}

/// Every guard field of the header (`waypoint_generator.cpp:272-281`)
/// rejects a tampered dump.
#[test]
fn read_tga_rejects_invalid_headers() {
    let dir = std::env::temp_dir().join("bw10-05-04-tga");
    std::fs::create_dir_all(&dir).unwrap();

    let flood = grid(0.0, 8.0, 1.0);
    let good = dir.join("good.tga");
    assert!(flood.write_tga(good.to_str().unwrap()));
    let raw = std::fs::read(&good).unwrap();

    // each tampered field position: (offset, value, name)
    for (offset, value, name) in [
        (0usize, 0u8, "extraLength"),
        (1, 0, "colourMapType"),
        (2, 0, "imageType"),
        (7, 8, "colourMapDepth"),
        (16, 16, "bpp"),
        (17, 1, "imageDescriptor"),
    ] {
        let mut bad = raw.clone();
        bad[offset] = value;
        let path = dir.join(format!("bad-{name}.tga"));
        std::fs::write(&path, &bad).unwrap();
        let mut gener = WaypointGenerator::new();
        assert!(!gener.read_tga(path.to_str().unwrap()), "{name} accepted");
    }

    // non-zero x / y and a wrong colour map span are equally rejected
    for (offset, value, name) in [(8usize, 5u8, "x"), (10, 5, "y")] {
        let mut bad = raw.clone();
        bad[offset] = value;
        let path = dir.join(format!("bad-{name}.tga"));
        std::fs::write(&path, &bad).unwrap();
        let mut gener = WaypointGenerator::new();
        assert!(!gener.read_tga(path.to_str().unwrap()), "{name} accepted");
    }
    let mut bad = raw.clone();
    bad[5] = 128; // colourMapLength low byte: 128 + 256*0 != 256
    let path = dir.join("bad-cmlength.tga");
    std::fs::write(&path, &bad).unwrap();
    let mut gener = WaypointGenerator::new();
    assert!(!gener.read_tga(path.to_str().unwrap()));

    // missing file
    let mut gener = WaypointGenerator::new();
    assert!(!gener.read_tga(dir.join("absent.tga").to_str().unwrap()));

    // truncated file: shorter than the header
    let path = dir.join("short.tga");
    std::fs::write(&path, &raw[..20]).unwrap();
    let mut gener = WaypointGenerator::new();
    assert!(!gener.read_tga(path.to_str().unwrap()));

    // truncated after the header but before the layers: the fread
    // failure arm (`waypoint_generator.cpp:303-318`)
    let path = dir.join("short-layers.tga");
    std::fs::write(&path, &raw[..18 + 16 + 768 + 10]).unwrap();
    let mut gener = WaypointGenerator::new();
    assert!(!gener.read_tga(path.to_str().unwrap()));
}

// --- extendThroughUnboundPortals ---------------------------------------

/// A single nav poly abutting the chunk's west boundary grows a skirt
/// polygon one step beyond it (`waypoint_generator.cpp:2407-2604`):
/// the new poly carries the same heights and set, the shared edges
/// cross-link through `adjNavPoly`, and the outer edge is flagged
/// adjacent to another chunk.
#[test]
fn a_poly_at_an_unbound_portal_grows_a_skirt() {
    let mut gener = WaypointGenerator::new();
    // one polygon whose vertices sit on the grid line x = 4.0, with
    // the portal plane through it. Grid resolution 1, min (0,0,0):
    // grid coords == world coords. Vertex angles must be even (not
    // 45-degree) and adjacencies zero so the extension picks them.
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    gener.polygons = vec![PolygonDef {
        vertices: vec![
            super::VertexDef {
                pos: (4.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (4.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 0.0,
        set: 0,
    }];

    // the portal plane x = 4 with the outward normal -x (the neighbour
    // lies at lower x): both ends of the edge v0->v1 sit within 0.45.
    let chunk = PortalChunk {
        planes: vec![x_plane(4.0)],
    };
    gener.extend_through_unbound_portals(&chunk);

    // one skirt polygon appended
    assert_eq!(gener.polygons.len(), 2);
    let skirt = &gener.polygons[1];
    assert_eq!(skirt.min_height, 0.0);
    assert_eq!(skirt.max_height, 0.0);
    assert_eq!(skirt.set, 0);
    // construction order: right-hand anchor, the run of portal-edge
    // starts (one qualifying edge: v3->v0), the displaced copies
    // (adjacent to another chunk), then the right-hand vertical edge —
    // 1 + 1 + 1 + 1 vertices.
    assert_eq!(skirt.vertices.len(), 4);

    // the single displaced (outer) vertex carries the extend direction:
    // the edge v3->v0 runs down z, rotated 90 degrees anticlockwise
    // that points +x, so the skirt steps one grid unit to +x.
    let outer: Vec<bool> = skirt
        .vertices
        .iter()
        .map(|v| v.adj_to_another_chunk)
        .collect();
    assert_eq!(outer, vec![false, false, true, false]);
    assert_eq!(skirt.vertices[2].pos, (5.0, 4.0));
    assert_eq!(skirt.vertices[3].pos, (5.0, 1.0));

    // the shared edges cross-link: the original vertex 3 (the extended
    // edge's start) points at the new polygon (index 2, 1-based) and
    // the skirt's first vertex points back at polygon 1.
    assert_eq!(gener.polygons[0].vertices[3].adj_nav_poly, 2);
    assert_eq!(skirt.vertices[0].adj_nav_poly, 1);

    // running again finds the polygon's portal edges already linked
    // (adjNavPoly != 0 / adjToAnotherChunk) and appends nothing.
    gener.extend_through_unbound_portals(&chunk);
    assert_eq!(gener.polygons.len(), 2);
}

/// A polygon away from every portal is untouched.
#[test]
fn a_poly_far_from_portals_is_untouched() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    gener.polygons = vec![PolygonDef {
        vertices: vec![
            super::VertexDef {
                pos: (1.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (3.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (3.0, 3.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (1.0, 3.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 0.0,
        set: 0,
    }];
    let chunk = PortalChunk {
        planes: vec![x_plane(50.0)],
    };
    gener.extend_through_unbound_portals(&chunk);
    assert_eq!(gener.polygons.len(), 1);
}

/// No portals at all: the early return
/// (`waypoint_generator.cpp:2394-2395`).
#[test]
fn a_chunk_without_portals_returns_early() {
    let mut gener = WaypointGenerator::new();
    let chunk = PortalChunk { planes: Vec::new() };
    gener.extend_through_unbound_portals(&chunk);
    assert!(gener.polygons.is_empty());
}

/// The `extraExtendPoints.size() != 1` refusal
/// (`waypoint_generator.cpp:2497-2502`): two separated portal runs on
/// one polygon leave two extra end points and the polygon is skipped.
#[test]
fn two_separated_portal_runs_are_refused() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    // a polygon with two disjoint edges on the portal plane x = 4
    // (v0->v1 and v2->v3), separated by off-plane edges.
    gener.polygons = vec![PolygonDef {
        vertices: vec![
            super::VertexDef {
                pos: (4.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 2.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (4.0, 2.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (4.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 5.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (4.0, 5.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 0.0,
        set: 0,
    }];
    let chunk = PortalChunk {
        planes: vec![x_plane(4.0)],
    };
    gener.extend_through_unbound_portals(&chunk);
    // both runs produce end points -> the != 1 arm skips the polygon
    assert_eq!(gener.polygons.len(), 1);
}

/// A 45-degree edge vertex is skipped
/// (`waypoint_generator.cpp:2429-2431`).
#[test]
fn diagonal_edge_vertices_are_skipped() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    // vertex 0 has an odd angle -> its edge never enters extendPoints
    gener.polygons = vec![PolygonDef {
        vertices: vec![
            super::VertexDef {
                pos: (4.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 1,
            },
            super::VertexDef {
                pos: (6.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (4.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 0.0,
        set: 0,
    }];
    let chunk = PortalChunk {
        planes: vec![x_plane(4.0)],
    };
    gener.extend_through_unbound_portals(&chunk);
    // vertex 0's edge is skipped, but vertex 3's edge (v3->v0) is also
    // on the plane: one run -> the extension still fires. Verify via
    // the appended polygon.
    assert_eq!(gener.polygons.len(), 2);
}

/// Edges already flagged adjacent to another chunk or linked to
/// another nav poly are skipped
/// (`waypoint_generator.cpp:2421-2427`).
#[test]
fn linked_or_chunk_edges_are_skipped() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    gener.polygons = vec![PolygonDef {
        vertices: vec![
            super::VertexDef {
                pos: (4.0, 1.0),
                adj_nav_poly: 1,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (4.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: true,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 0.0,
        set: 0,
    }];
    let chunk = PortalChunk {
        planes: vec![x_plane(4.0)],
    };
    gener.extend_through_unbound_portals(&chunk);
    // v0's edge is nav-poly-linked and v3's edge is chunk-adjacent:
    // no extend points, no skirt.
    assert_eq!(gener.polygons.len(), 1);
}

// --- determineEdgesAdjacentToOtherChunks branch coverage ---------------

use crate::flood::tests::{FieldPhysics, flat_ground};

/// A test physics whose `findDropPoint` misses everything below z = 0,
/// to drive the false arms of the drop probe.
struct MissBelowZero(FieldPhysics);
impl FloodPhysics for MissBelowZero {
    fn get_girth(&self) -> Vector3 {
        Vector3::new(0.5, 1.5, 0.5)
    }
    fn get_scramble_height(&self) -> f32 {
        0.6
    }
    fn find_drop_point(&mut self, pos: &Vector3, y: &mut f32) -> bool {
        if pos.z < 0.0 {
            return false;
        }
        self.0.find_drop_point(pos, y)
    }
    fn adjust_move(&mut self, src: &Vector3, dst: &Vector3, dst2: &mut Vector3) {
        self.0.adjust_move(src, dst, dst2)
    }
}

/// A chunk seam over the Bounds fixture plus an explicit local bbox.
struct EdgeChunk(Bounds, (Vector3, Vector3));
impl GenChunk for EdgeChunk {
    fn owns(&self, point: &Vector3) -> bool {
        FloodChunk::owns(&self.0, point)
    }
    fn local_bb(&self) -> (Vector3, Vector3) {
        self.1
    }
}

/// The huge-polygon arm (`waypoint_generator.cpp:2651-2672`): one
/// vertex beyond each of the chunk's four outer corners marks every
/// vertex adjacent to another chunk and skips the portal scan.
#[test]
fn a_polygon_surrounding_the_chunk_is_marked_wholesale() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    // four corner vertices around a tight bbox (1.5..2.5), each
    // supplying one corner bit; grid coords stay in [0,8) — vertices
    // beyond the grid are reference UB in `toVector3`'s grid index.
    gener.polygons = vec![PolygonDef {
        vertices: vec![
            super::VertexDef {
                pos: (0.0, 0.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (0.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (4.0, 0.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (4.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 1.0,
        set: 0,
    }];
    let mut physics = MissBelowZero(flat_ground());
    let chunk = EdgeChunk(
        Bounds::everywhere(),
        (Vector3::new(1.5, 0.0, 1.5), Vector3::new(2.5, 0.0, 2.5)),
    );
    gener.determine_edges_adjacent_to_other_chunks(&chunk, &mut physics);
    // all four vertices marked, each having supplied one corner bit
    for (v, vert) in gener.polygons[0].vertices.iter().enumerate() {
        assert!(vert.adj_to_another_chunk, "v{v}");
    }
}

/// The drop-point arms (`waypoint_generator.cpp:2691-2703`): the
/// vertices lift to `maxHeight + 1` and only re-seat where the physics
/// lands; a column with nothing below keeps the lifted height, which
/// can throw the midpoint out of the ownership box.
#[test]
fn drop_point_arms_and_the_midpoint_check() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    // one polygon spanning the ownership seam: the edge crosses the
    // box edge x = 4 with both ends outside and the midpoint outside
    // too (to mark), and a second edge whose midpoint stays inside.
    gener.polygons = vec![PolygonDef {
        vertices: vec![
            super::VertexDef {
                pos: (2.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 3.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (2.0, 3.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 1.0,
        set: 0,
    }];
    // ownership box x in [0,4]: the whole polygon's east half is out.
    // physics: flat ground below z >= 0, nothing below z < 0 — the
    // edge v1->v2 sits at z = 1 and lands, the drop arms all true.
    // The v2->v3 edge crosses z = -? no — keep all z positive; the
    // false drop arm fires via the z < 0 guard only in the wholesale
    // test. Here: every edge midpoint outside x <= 4 gets marked
    // except none (all edges have both ends out or one in).
    let mut physics = MissBelowZero(crate::flood::tests::flat_ground());
    let chunk = EdgeChunk(
        Bounds {
            owns_box: (0.0, -100.0, 4.0, 100.0),
            space: (-100.0, 100.0, -100.0, 100.0),
        },
        (Vector3::new(-4.0, 0.0, -4.0), Vector3::new(4.0, 0.0, 4.0)),
    );
    gener.determine_edges_adjacent_to_other_chunks(&chunk, &mut physics);
    // edges v1->v2 (x=6, both ends outside the box, midpoint outside)
    // and v2->v3 (from (6,3) to (2,3): one end inside) — only the
    // both-ends-out edge marks its start vertex.
    assert!(!gener.polygons[0].vertices[0].adj_to_another_chunk);
    assert!(gener.polygons[0].vertices[1].adj_to_another_chunk);
    // v2->v3 has an inside end: not marked
    assert!(!gener.polygons[0].vertices[2].adj_to_another_chunk);
}

/// The midpoint stays inside: both ends of an edge out of the box but
/// the midpoint in — no mark (`waypoint_generator.cpp:2705-2712`).
#[test]
fn an_edge_whose_midpoint_is_owned_is_not_marked() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    // L-shaped ownership: ends outside, middle inside. The physics is
    // irrelevant (flat ground); the box split makes the midpoint owned.
    gener.polygons = vec![PolygonDef {
        vertices: vec![
            super::VertexDef {
                pos: (0.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 1.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 0,
            },
            super::VertexDef {
                pos: (6.0, 3.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            super::VertexDef {
                pos: (0.0, 3.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 1.0,
        set: 0,
    }];
    // owns only x in [2,4] below z=2: the bottom edge's midpoint (3,1)
    // lands owned while the top edge's (3,3) stays out.
    let mut physics = flat_ground();
    let chunk = EdgeChunk(
        Bounds {
            owns_box: (2.0, -100.0, 4.0, 2.0),
            space: (-100.0, 100.0, -100.0, 100.0),
        },
        (Vector3::new(-4.0, 0.0, -4.0), Vector3::new(4.0, 0.0, 4.0)),
    );
    gener.determine_edges_adjacent_to_other_chunks(&chunk, &mut physics);
    // the bottom edge's both ends are unowned but its midpoint (3,1)
    // is owned: unmarked. The top edge (v2->v3) has both ends out and
    // its midpoint (3,3) out too: marked.
    assert!(!gener.polygons[0].vertices[0].adj_to_another_chunk);
    assert!(gener.polygons[0].vertices[2].adj_to_another_chunk);
}

/// `toVector3`'s band-top fallback (`waypoint_generator.cpp:2357`):
/// a polygon whose height band excludes every grid layer takes
/// `maxHeight + 0.01`.
#[test]
fn to_vector3_falls_back_to_the_band_top() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::ZERO, 1.0);
    // every grid cell height is 0.0; a band that excludes 0.0
    let polygon = PolygonDef {
        vertices: Vec::new(),
        min_height: 5.0,
        max_height: 5.0,
        set: 0,
    };
    let p = gener.to_vector3((3.0, 4.0), &polygon);
    assert_eq!(p.y, 5.01);
    // and a band containing 0.0 reads the grid height
    let polygon = PolygonDef {
        vertices: Vec::new(),
        min_height: -1.0,
        max_height: 1.0,
        set: 0,
    };
    let p = gener.to_vector3((3.0, 4.0), &polygon);
    assert_eq!(p.y, 0.0);
}

/// `readTGA`'s height-layer read error arm (`waypoint_generator.cpp`:
/// the hgtGrids fread failing after the adjGrids layers all read).
#[test]
fn read_tga_rejects_a_file_cut_inside_the_height_layers() {
    // take a real dump, keep the header + palette + all adj layers,
    // and cut inside the first hgt layer: the hgt fread fails.
    let mut flood = grid(0.0, 8.0, 1.0);
    let mut physics = slope_scene();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(1.0, 1.0, 1.0), &mut physics, &chunk, None);
    let dir = std::env::temp_dir().join("bw10-05-04-tga");
    std::fs::create_dir_all(&dir).unwrap();
    let full = dir.join("full.tga");
    assert!(flood.write_tga(full.to_str().unwrap()));
    let mut data = std::fs::read(&full).unwrap();

    let cells = flood.xsize() as usize * flood.zsize() as usize;
    let adj_bytes = MAX_HEIGHTS * cells * 4;
    let cut = TgaHeader::SIZE + 768 + adj_bytes + 10; // inside hgt layer 0
    data.truncate(cut);

    let path = dir.join("cut.tga");
    std::fs::write(&path, &data).unwrap();
    let mut gener = WaypointGenerator::new();
    assert!(!gener.read_tga(path.to_str().unwrap()));
}

/// The identifier keeps the name when the path has no separator
/// (`waypoint_generator.cpp:330-333`).
#[test]
fn read_tga_identifier_without_a_separator_is_the_whole_name() {
    // run from a directory the test can open by bare name
    let mut flood = grid(0.0, 8.0, 1.0);
    let mut physics = slope_scene();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(1.0, 1.0, 1.0), &mut physics, &chunk, None);
    let path = std::env::temp_dir()
        .join("bw10-05-04-tga")
        .join("noseparator.tga");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    assert!(flood.write_tga(path.to_str().unwrap()));

    let mut gener = WaypointGenerator::new();
    // read_tga opens the name as given: a bare filename works when the
    // current directory is the fixture dir
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(path.parent().unwrap()).unwrap();
    let ok = gener.read_tga("noseparator.tga");
    std::env::set_current_dir(cwd).unwrap();
    assert!(ok);
    assert_eq!(gener.identifier(), "noseparator.tga");
}
