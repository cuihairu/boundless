//! BW-10-01-01 fixture evidence: a hand-authored space directory parses into
//! the same chunk set and object set the reference loader would enumerate.
//! The fixtures live in `docs/review/fixtures/chunk_space`; their derivation
//! from the reference `chunk format.txt` and `settings.xml` layout is
//! documented in `docs/review/bw10-01-chunk-load.md`.

use std::path::{Path, PathBuf};

use chunk::{Chunk, ChunkSpace, GeometryMapping, GridBounds, PortalTarget};
use foundation::math::{Matrix, Vector3};
use resources::datasection;

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

/// Load a resource section by name from the fixture space root, the same
/// lookup `BWResource` performs for the reference (includes and shell visuals).
fn resource_resolver(name: &str) -> Option<datasection::DataSection> {
    let path = space_root().join(name);
    let raw = std::fs::read(path).ok()?;
    datasection::parse("", &raw).ok()
}

/// The single portal of a boundary, wherever it landed: earth portals are
/// born in `bound_portals`, everything else in `unbound_portals`.
fn sole_portal(b: &chunk::ChunkBoundary) -> &chunk::Portal {
    b.bound_portals
        .first()
        .or_else(|| b.unbound_portals.first())
        .unwrap()
}

fn load_chunk_file(path: &Path, mapping: &GeometryMapping, id: &str) -> Chunk {
    let raw = std::fs::read(path).unwrap_or_else(|_| panic!("{} readable", path.display()));
    let section = datasection::parse("", &raw).expect("chunk file parses");
    let mut chunk = Chunk::new(id, mapping, &Matrix::identity());
    assert!(
        chunk.load(&section, &Matrix::identity(), &resource_resolver),
        "{} loaded",
        id
    );
    chunk
}

#[test]
fn settings_yield_the_reference_grid() {
    let mapping = load_mapping();
    assert_eq!(mapping.grid_size, 100.0);
    assert!(!mapping.single_dir);
    assert_eq!(
        mapping.bounds,
        GridBounds {
            min_l_grid_x: 0,
            min_l_grid_y: 0,
            max_l_grid_x: 17,
            max_l_grid_y: 1,
            min_grid_x: 0,
            min_grid_y: 0,
            max_grid_x: 17,
            max_grid_y: 1,
        }
    );
}

#[test]
fn gather_chunks_enumerates_every_cell_plus_inside_chunks() {
    let mapping = load_mapping();
    let root = space_root();
    let chunks = mapping.gather_chunks(&root);

    // 18 x 2 outside cells plus the one inside chunk found on disk.
    assert_eq!(chunks.len(), 18 * 2 + 1, "set: {chunks:?}");

    // The flat cells keep their bare identifier.
    assert!(chunks.contains("00000000o"));
    assert!(chunks.contains("00010001o"));
    // The cell at grid (17, 0) trips the 4-bit prefix level.
    assert!(chunks.contains("001x000x/sep/00110000o"));
    // The inside chunk is discovered by the directory scan.
    assert!(chunks.contains("00000000i"));

    // Every cell in the rect is enumerated even without a file.
    assert!(chunks.contains("00030000o"));
    // A cell outside the rect is not enumerated.
    assert!(!chunks.contains("ffff0000o"));
}

#[test]
fn chunk_files_load_into_the_same_objects() {
    let mapping = load_mapping();
    let root = space_root();

    // Origin cell: two models, a light and a sound.
    let chunk = load_chunk_file(&root.join("00000000o.chunk"), &mapping, "00000000o");
    assert!(chunk.is_outside);
    assert_eq!((chunk.x, chunk.z), (0, 0));
    let tags: Vec<&str> = chunk.items.iter().map(|i| i.tag.as_str()).collect();
    assert_eq!(tags, vec!["model", "model", "light", "sound"]);
    assert_eq!(chunk.items[0].resource.as_deref(), Some("tree00.model"));
    assert_eq!(
        chunk.items[0].position, None,
        "model position is its transform, not <position>"
    );
    assert_eq!(chunk.items[2].tag, "light");

    // The prefixed far cell resolves to grid (17, 0) with its own objects.
    let far = load_chunk_file(
        &root.join("001x000x/sep/00110000o.chunk"),
        &mapping,
        "001x000x/sep/00110000o",
    );
    assert_eq!((far.x, far.z), (17, 0));
    assert_eq!(far.items.len(), 2);
    assert_eq!(far.items[0].resource.as_deref(), Some("tree17.model"));

    // The inside chunk reads its transform, bounding box and objects.
    let inside = load_chunk_file(&root.join("00000000i.chunk"), &mapping, "00000000i");
    assert!(!inside.is_outside);
    assert_eq!(inside.items.len(), 3);
    assert_eq!(inside.items[0].tag, "model");
    assert_eq!(inside.items[1].tag, "entity");
    assert_eq!(inside.items[2].tag, "light");
    // Local transform row3 is the chunk origin, so the world box is shifted.
    assert_eq!(
        inside.bounding_box.min,
        foundation::math::Vector3::new(140.0, -5.0, 240.0)
    );
    assert_eq!(
        inside.bounding_box.max,
        foundation::math::Vector3::new(160.0, 5.0, 260.0)
    );
}

/// BW-10-01-02: the six boundaries of an outside chunk are synthesised from
/// its grid coordinates, with the reference's neighbour targets
/// (`createBoundaries`, chunk.cpp:695-806). Order is right, left, bottom,
/// top, back, front.
#[test]
fn outside_chunk_boundaries_match_the_reference_grid() {
    let mapping = load_mapping();

    // Grid (0, 0): the min-x and min-z edges are mapping boundaries, so the
    // right and back faces leave the space.
    let origin = chunk::create_boundaries("00000000o", &mapping);
    assert_eq!(origin.len(), 6);
    let targets: Vec<PortalTarget> = origin
        .iter()
        .map(|b| sole_portal(b).target.clone())
        .collect();
    assert_eq!(
        targets,
        vec![
            PortalTarget::Extern,
            PortalTarget::Chunk("00010000o".into()),
            PortalTarget::Earth,
            PortalTarget::Heaven,
            PortalTarget::Extern,
            PortalTarget::Chunk("00000001o".into()),
        ]
    );
    // Planes: right x=0, left x=100, bottom y=-10000, top y=10000,
    // back z=0, front z=100.
    assert_eq!(origin[0].plane.normal(), &Vector3::new(1.0, 0.0, 0.0));
    assert_eq!(origin[1].plane.normal(), &Vector3::new(-1.0, 0.0, 0.0));
    assert_eq!(origin[1].plane.d(), -100.0);
    assert_eq!(origin[2].plane.normal(), &Vector3::new(0.0, 1.0, 0.0));
    assert_eq!(origin[2].plane.d(), -10000.0);
    assert_eq!(origin[3].plane.normal(), &Vector3::new(0.0, -1.0, 0.0));
    assert_eq!(origin[4].plane.normal(), &Vector3::new(0.0, 0.0, 1.0));
    assert_eq!(origin[5].plane.normal(), &Vector3::new(0.0, 0.0, -1.0));
    assert_eq!(origin[5].plane.d(), -100.0);
    // u and v axes, then the local centres of the six faces.
    assert_eq!(sole_portal(&origin[0]).u_axis, Vector3::new(0.0, 1.0, 0.0));
    assert_eq!(sole_portal(&origin[0]).v_axis, Vector3::new(0.0, 0.0, 1.0));
    assert_eq!(sole_portal(&origin[1]).u_axis, Vector3::new(0.0, 0.0, 1.0));
    assert_eq!(sole_portal(&origin[5]).u_axis, Vector3::new(0.0, 1.0, 0.0));
    let centres: Vec<_> = origin.iter().map(|b| sole_portal(b).lcentre).collect();
    assert_eq!(
        centres,
        vec![
            Vector3::new(0.0, 0.0, 50.0),
            Vector3::new(100.0, 0.0, 50.0),
            Vector3::new(50.0, -10000.0, 50.0),
            Vector3::new(50.0, 10000.0, 50.0),
            Vector3::new(50.0, 0.0, 0.0),
            Vector3::new(50.0, 0.0, 100.0),
        ]
    );
    // The polygon runs counter-clockwise seen from the plane's front:
    // (minY, 0) -> (maxY, 0) -> (maxY, gridSize) -> (minY, gridSize).
    assert_eq!(
        sole_portal(&origin[0]).points,
        vec![
            foundation::math::Vector2::new(-10000.0, 0.0),
            foundation::math::Vector2::new(10000.0, 0.0),
            foundation::math::Vector2::new(10000.0, 100.0),
            foundation::math::Vector2::new(-10000.0, 100.0),
        ]
    );

    // Grid (17, 0): the far edge and the min-z edge; the prefixed cells name
    // their neighbours with the hashed subdirectory prefix.
    let far = chunk::create_boundaries("001x000x/sep/00110000o", &mapping);
    let far_targets: Vec<PortalTarget> =
        far.iter().map(|b| sole_portal(b).target.clone()).collect();
    assert_eq!(
        far_targets,
        vec![
            // Grid 16 also trips the 4-bit prefix level (16 >= 16).
            PortalTarget::Chunk("001x000x/sep/00100000o".into()),
            PortalTarget::Extern,
            PortalTarget::Earth,
            PortalTarget::Heaven,
            PortalTarget::Extern,
            PortalTarget::Chunk("001x000x/sep/00110001o".into()),
        ]
    );

    // Grid (17, 1): the max-z edge is extern on the front face.
    let corner = chunk::create_boundaries("001x001x/sep/00110001o", &mapping);
    let corner_targets: Vec<PortalTarget> = corner
        .iter()
        .map(|b| sole_portal(b).target.clone())
        .collect();
    assert_eq!(
        corner_targets,
        vec![
            // right -> grid (16, 1), back -> grid (17, 0).
            PortalTarget::Chunk("001x000x/sep/00100001o".into()),
            PortalTarget::Extern,
            PortalTarget::Earth,
            PortalTarget::Heaven,
            PortalTarget::Chunk("001x000x/sep/00110000o".into()),
            PortalTarget::Extern,
        ]
    );
}

/// BW-10-01-02: an inside chunk takes its boundaries from the shell visual,
/// and `form_boundaries` splits them into joints and bounds the way the
/// reference does (chunk.cpp:867-905).
#[test]
fn inside_chunk_boundaries_come_from_the_shell_visual() {
    let mapping = load_mapping();
    let root = space_root();
    let raw = std::fs::read(root.join("00000000i.chunk")).expect("inside chunk readable");
    let section = datasection::parse("", &raw).expect("inside chunk parses");
    let mut chunk = Chunk::new("00000000i", &mapping, &Matrix::identity());
    assert!(chunk.load(&section, &Matrix::identity(), &resource_resolver));

    let boundaries = &chunk.boundaries;
    assert_eq!(boundaries.len(), 6);
    // +x (external portal), -x (wall), +z (internal portal), -z (wall),
    // then the earth and heaven faces of the shell box.
    assert_eq!(boundaries[0].plane.normal(), &Vector3::new(1.0, 0.0, 0.0));
    let external = &boundaries[0].unbound_portals[0];
    assert_eq!(external.target, PortalTarget::Chunk("00000000o".into()));
    assert!(!external.internal);
    assert!(external.permissive);

    // The -x face has no portal at all: a wall, so a bound but not a joint.
    assert!(boundaries[1].unbound_portals.is_empty());
    assert!(boundaries[1].bound_portals.is_empty());

    // The +z portal is written with the wrong winding; the parser reversed
    // it, so it now runs counter-clockwise from the plane's front.
    let internal = &boundaries[2].unbound_portals[0];
    assert!(internal.internal);
    assert_eq!(internal.target, PortalTarget::Chunk("00000001i".into()));
    assert_eq!(
        internal.points,
        vec![
            foundation::math::Vector2::new(-10.0, -10.0),
            foundation::math::Vector2::new(10.0, -10.0),
            foundation::math::Vector2::new(10.0, 10.0),
            foundation::math::Vector2::new(-10.0, 10.0),
        ]
    );
    // Portal geometry: the u/v basis, the origin on the plane and the local
    // centre are all derived from the plane and uAxis (chunk_boundary.cpp:424-460).
    assert_eq!(internal.u_axis, Vector3::new(1.0, 0.0, 0.0));
    assert_eq!(internal.v_axis, Vector3::new(0.0, 1.0, 0.0));
    assert_eq!(internal.origin, Vector3::new(0.0, 0.0, 10.0));
    assert_eq!(internal.lcentre, Vector3::new(0.0, 0.0, 10.0));
    // `inside` is an in-plane polygon test: it projects the point onto the
    // u/v basis, so a point off the plane that projects outside is rejected.
    assert!(internal.inside(&Vector3::new(0.0, 0.0, 10.0)));
    assert!(!internal.inside(&Vector3::new(20.0, 0.0, 10.0)));

    // Earth starts bound; heaven is unbound. Both are joints and bounds.
    assert_eq!(boundaries[4].bound_portals.len(), 1);
    assert_eq!(boundaries[4].bound_portals[0].target, PortalTarget::Earth);
    assert_eq!(
        boundaries[5].unbound_portals[0].target,
        PortalTarget::Heaven
    );

    // Right face: an unbound external portal -> joint and bound.
    // -x wall: bound only.  +z internal portal: joint only.
    assert!(chunk.joints.contains(&0));
    assert!(chunk.bounds.contains(&0));
    assert!(chunk.bounds.contains(&1));
    assert!(!chunk.joints.contains(&1));
    assert!(chunk.joints.contains(&2));
    assert!(!chunk.bounds.contains(&2));
}

/// BW-10-01-03: the scan walks the grid offsets around the camera cell
/// nearest-first (`ChunkManager::scan`, chunk_manager.cpp:2132-2222), creates
/// the outside chunks for in-bounds cells and returns the not-yet-loaded ones
/// as the candidates to load.
#[test]
fn streaming_scan_loads_the_grid_nearest_first() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);
    assert_eq!(space.grid_size(), 100.0);
    assert_eq!(space.point_to_grid(250.0), 2);
    assert_eq!(space.grid_to_point(3), 300.0);

    // Camera at the origin cell, load radius two cells.
    let camera = Vector3::new(50.0, 0.0, 50.0);
    let candidates = space.scan(&camera, 200.0);

    // Nearest first: (0,0), then the radius-1 ring in-bounds cells, then
    // (1,1), then (2,0); the out-of-bounds cells never appear.
    let expected = [
        "00000000o",
        "00000001o",
        "00010000o",
        "00010001o",
        "00020000o",
    ];
    assert_eq!(candidates, expected.map(String::from).to_vec());
    // The scan created every visited chunk but loaded none of them
    // (`findChunkByGrid` + `new Chunk` + `addChunk`).
    for id in &candidates {
        let chunk = space.chunk(id).expect("scan created the chunk");
        assert!(!chunk.is_loaded, "{id} not loaded yet");
    }
    assert!(
        space.chunk("00000000i").is_none(),
        "scan never leaves the grid"
    );

    // A second scan returns nothing new: everything is already created and
    // none is loaded, so they are still candidates — but creating is
    // idempotent.
    let again = space.scan(&camera, 200.0);
    assert_eq!(again, candidates);
}

/// BW-10-01-03: the space map API. `findChunkByGrid` resolves a cell to the
/// chunk owning it; `addChunk`/`chunk_mut`/`chunks` maintain the map; a
/// missing `.chunk` file and a not-yet-loaded chunk both fail cleanly.
#[test]
fn space_map_accessors_track_created_chunks() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
    assert!(space.bind_order().is_empty());

    let camera = Vector3::new(50.0, 0.0, 50.0);
    let candidates = space.scan(&camera, 200.0);

    // `findChunkByGrid`: the cell that owns grid (0, 0) is 00000000o; a cell
    // outside the mapping has no chunk.
    let by_grid = space
        .find_chunk_by_grid(0, 0)
        .expect("grid (0,0) owns a chunk");
    assert_eq!(by_grid.identifier, "00000000o");
    assert!(space.find_chunk_by_grid(-1, 0).is_none());
    assert!(space.find_chunk_by_grid(0, -1).is_none());

    // `chunk_mut` reaches the created chunk.
    space.chunk_mut("00000000o").unwrap().is_loaded = true;
    assert!(space.chunk("00000000o").unwrap().is_loaded);

    // `chunks` iterates the map, `add_chunk` registers a new one.
    assert_eq!(space.chunks().count(), candidates.len());
    space.add_chunk(Chunk::new("00030000o", &mapping, &Matrix::identity()));
    assert_eq!(space.chunks().count(), candidates.len() + 1);
    assert!(space.chunk("00030000o").is_some());

    // A missing `.chunk` file fails to load; a created-but-not-loaded chunk
    // refuses to bind.
    assert!(!space.load_chunk("00030000o", &resource_resolver));
    assert!(!space.bind("00020000o"));
    assert!(!space.bind("00000000i"));
    assert!(space.bind_order().is_empty());
}

/// BW-10-01-03: the bind sequence. Chunks load and bind in scan order; a
/// portal binds when its target chunk is already bound, and the connection is
/// two-way (`bindPortals` + `Chunk::bind(Chunk*)`).
#[test]
fn streaming_bind_sequence_connects_neighbours() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);
    let camera = Vector3::new(50.0, 0.0, 50.0);
    let candidates = space.scan(&camera, 200.0);

    let bound = space.load_and_bind(&candidates, &resource_resolver);
    // `00020000o` has no .chunk file in the fixture, so it fails to load and
    // never binds; the other four bind in scan order.
    let order = ["00000000o", "00000001o", "00010000o", "00010001o"];
    let order: Vec<String> = order.iter().map(|s| s.to_string()).collect();
    assert_eq!(space.bind_order(), &order[..]);
    assert_eq!(bound, order);

    let targets = |id: &str, boundary: usize| -> Vec<PortalTarget> {
        space.chunk(id).unwrap().boundaries[boundary]
            .bound_portals
            .iter()
            .map(|p| p.target.clone())
            .collect()
    };
    let unbound_targets = |id: &str, boundary: usize| -> Vec<PortalTarget> {
        space.chunk(id).unwrap().boundaries[boundary]
            .unbound_portals
            .iter()
            .map(|p| p.target.clone())
            .collect()
    };

    // 00000000o bound first: only heaven bound at its own turn, but the
    // neighbours' binds moved its left and front portals over (`Chunk::bind`).
    assert_eq!(
        targets("00000000o", 1),
        vec![PortalTarget::Chunk("00010000o".into())]
    );
    assert_eq!(targets("00000000o", 3), vec![PortalTarget::Heaven]);
    assert_eq!(
        targets("00000000o", 5),
        vec![PortalTarget::Chunk("00000001o".into())]
    );
    // The extern faces stay unbound: resolving them needs the space column.
    assert_eq!(unbound_targets("00000000o", 0), vec![PortalTarget::Extern]);
    assert_eq!(unbound_targets("00000000o", 4), vec![PortalTarget::Extern]);

    // 00000001o connected back to 00000000o and later to 00010001o.
    assert_eq!(
        targets("00000001o", 1),
        vec![PortalTarget::Chunk("00010001o".into())]
    );
    assert_eq!(
        targets("00000001o", 4),
        vec![PortalTarget::Chunk("00000000o".into())]
    );

    // 00010001o connected to both bound neighbours at its own turn.
    assert_eq!(
        targets("00010001o", 0),
        vec![PortalTarget::Chunk("00000001o".into())]
    );
    assert_eq!(
        targets("00010001o", 4),
        vec![PortalTarget::Chunk("00010000o".into())]
    );

    // The failed load left 00020000o created but not loaded, so 00010000o's
    // left face never bound and 00010000o's own portals to it stay unbound.
    assert!(unbound_targets("00010000o", 1).contains(&PortalTarget::Chunk("00020000o".into())));
    assert!(!space.chunk("00020000o").unwrap().is_loaded);
    assert!(!space.chunk("00020000o").unwrap().is_bound);

    // The world centre is the chunk transform applied to the local centre
    // (chunk.cpp:3339): grid (1,0) sits at x=100.
    let right = &space.chunk("00010000o").unwrap().boundaries[0].bound_portals[0];
    assert_eq!(right.lcentre, Vector3::new(0.0, 0.0, 50.0));
    assert_eq!(right.centre, Vector3::new(100.0, 0.0, 50.0));

    // Binding again is a no-op and does not duplicate the bind order.
    assert!(space.bind("00010001o"));
    assert_eq!(space.bind_order(), &order[..]);
}

/// BW-10-01-05: the point queries walk the focus grid — no column before a
/// chunk is focussed, the outside chunk answers after `focus_chunk`, and
/// points off the grid resolve to nothing.
#[test]
fn point_queries_walk_the_focus_grid_columns() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);
    let camera = Vector3::new(50.0, 0.0, 50.0);
    let candidates = space.scan(&camera, 200.0);
    space.load_and_bind(&candidates, &resource_resolver);

    // `column(pt, false)` finds nothing while no chunk has been focussed:
    // the columns only exist once `focusChunk` installs them.
    let centre = Vector3::new(50.0, 0.0, 50.0);
    assert!(space.column_at(&centre).is_none());
    assert!(space.find_outside_chunk_from_point(&centre).is_none());

    // Focusing 00000000o installs its column; the outside-chunk query now
    // resolves the point to that chunk.
    space.focus_chunk("00000000o");
    let column = space.column_at(&centre).expect("the focussed column");
    assert_eq!(column.outside_chunk(), Some("00000000o"));
    assert_eq!(
        space
            .find_outside_chunk_from_point(&centre)
            .map(|c| c.identifier.as_str()),
        Some("00000000o")
    );

    // A point past the mapping's grid edge has no cell in the focus grid,
    // so both queries answer None even with columns installed.
    let astray = Vector3::new(150.0, 0.0, 250.0);
    assert!(space.column_at(&astray).is_none());
    assert!(space.find_outside_chunk_from_point(&astray).is_none());
}

/// BW-10-01-03: an inside chunk binds its external portal to an already-bound
/// outside chunk, while portals to chunks that are not bound stay open.
#[test]
fn inside_chunk_binds_to_a_bound_outside_chunk() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);

    assert!(space.load_chunk("00000000o", &resource_resolver));
    assert!(space.bind("00000000o"));

    // Not loaded yet: binding a missing or unloaded chunk is refused.
    assert!(!space.bind("00000000i"));
    assert!(!space.bind("no-such-chunk"));

    assert!(space.load_chunk("00000000i", &resource_resolver));
    assert!(space.bind("00000000i"));

    let inside = space.chunk("00000000i").unwrap();
    // +x external portal -> 00000000o: bound now.
    assert_eq!(
        inside.boundaries[0].bound_portals[0].target,
        PortalTarget::Chunk("00000000o".into())
    );
    // +z internal portal -> 00000001i: never loaded, stays unbound.
    assert_eq!(
        inside.boundaries[2].unbound_portals[0].target,
        PortalTarget::Chunk("00000001i".into())
    );
    // Heaven bound at its own turn; earth was born bound.
    assert_eq!(
        inside.boundaries[5].bound_portals[0].target,
        PortalTarget::Heaven
    );
    assert_eq!(
        inside.boundaries[4].bound_portals[0].target,
        PortalTarget::Earth
    );
    let order = ["00000000o", "00000000i"].map(String::from).to_vec();
    assert_eq!(space.bind_order(), &order[..]);
}
