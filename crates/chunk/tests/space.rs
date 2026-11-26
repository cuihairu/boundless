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
    assert!(chunks.contains("001xx000xx/sep/00110000o"));
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
        &root.join("001xx000xx/sep/00110000o.chunk"),
        &mapping,
        "001xx000xx/sep/00110000o",
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
    let far = chunk::create_boundaries("001xx000xx/sep/00110000o", &mapping);
    let far_targets: Vec<PortalTarget> =
        far.iter().map(|b| sole_portal(b).target.clone()).collect();
    assert_eq!(
        far_targets,
        vec![
            // Grid 16 also trips the 4-bit prefix level (16 >= 16).
            PortalTarget::Chunk("001xx000xx/sep/00100000o".into()),
            PortalTarget::Extern,
            PortalTarget::Earth,
            PortalTarget::Heaven,
            PortalTarget::Extern,
            PortalTarget::Chunk("001xx000xx/sep/00110001o".into()),
        ]
    );

    // Grid (17, 1): the max-z edge is extern on the front face.
    let corner = chunk::create_boundaries("001xx001xx/sep/00110001o", &mapping);
    let corner_targets: Vec<PortalTarget> = corner
        .iter()
        .map(|b| sole_portal(b).target.clone())
        .collect();
    assert_eq!(
        corner_targets,
        vec![
            // right -> grid (16, 1), back -> grid (17, 0).
            PortalTarget::Chunk("001xx000xx/sep/00100001o".into()),
            PortalTarget::Extern,
            PortalTarget::Earth,
            PortalTarget::Heaven,
            PortalTarget::Chunk("001xx000xx/sep/00110000o".into()),
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

/// BW-10-01-05: the spatial index. Chunks focussed into the focus grid answer
/// point queries through the column of their cell — the inside shell displaces
/// the outside chunk inside its hull, the outside chunk answers in the rest of
/// its cell, and a cell no chunk holds (and a point past the mapping bounds)
/// answers nothing. The blank region is the reference's empty-column contract:
/// `column()` returns NULL out of span and a column with no hulls answers its
/// outside chunk or nothing.
///
/// The fixture stream loads its five chunks the BW-10-01-03 way; the loaded
/// and bound chunks land in the index through the same `bind` path the
/// reference's `noticeChunk` feeds.
#[test]
fn the_focus_index_answers_point_queries_across_chunks_and_void() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);

    // Stream the four loadable cells in scan order, then the inside shell.
    let camera = Vector3::new(50.0, 0.0, 50.0);
    let candidates = space.scan(&camera, 200.0);
    let bound = space.load_and_bind(&candidates, &resource_resolver);
    assert_eq!(bound.len(), 4, "the four file-backed cells bind");
    assert!(space.load_chunk("00000000i", &resource_resolver));
    assert!(space.bind("00000000i"));
    // The just-bound chunks wait in the blurred queue until the pump runs.
    assert_eq!(space.blurred(), &["00000000i".to_string()]);
    space.focus();
    assert!(space.blurred().is_empty());

    // The index holds the four outside chunks by their centre cell plus the
    // inside shell's hull in the columns its box corners touch.
    let grid = space.focus_grid();
    assert_eq!(
        grid.get(0, 0).and_then(|c| c.outside_chunk()),
        Some("00000000o")
    );
    assert_eq!(
        grid.get(1, 0).and_then(|c| c.outside_chunk()),
        Some("00010000o")
    );
    assert_eq!(
        grid.get(0, 1).and_then(|c| c.outside_chunk()),
        Some("00000001o")
    );
    assert_eq!(
        grid.get(1, 1).and_then(|c| c.outside_chunk()),
        Some("00010001o")
    );
    // The shell (world box 140..160 x 240..260) lives in grid row 2, past
    // the mapping's bounds: its corner columns fall out of the focus span
    // and its hull lands nowhere. That is the reference contract too —
    // `Chunk::focus` filters the NULL columns out of its column set — so the
    // in-span hull check needs a shell that sits inside the mapping. A shell
    // at the origin cell works.
    let mut shell = Chunk::new("00010000i", &load_mapping(), &Matrix::identity());
    shell.bounding_box = foundation::math::BoundBox::new(
        foundation::math::Vector3::new(20.0, -5.0, 20.0),
        foundation::math::Vector3::new(40.0, 5.0, 40.0),
    );
    shell.is_loaded = true;
    space.add_chunk(shell);
    space.focus();
    assert!(space.focus_grid().get(0, 0).unwrap().has_inside_chunks());

    // Cross-chunk queries: every answer is the chunk whose world box — or,
    // for the shell, whose hull — holds the point.
    let answers = [
        (Vector3::new(50.0, 0.0, 50.0), "00000000o"),
        (Vector3::new(150.0, 0.0, 50.0), "00010000o"),
        (Vector3::new(50.0, 0.0, 150.0), "00000001o"),
        (Vector3::new(150.0, 0.0, 150.0), "00010001o"),
    ];
    for (point, want) in answers {
        let found = space
            .find_chunk_from_point(&point)
            .map(|c| c.identifier.clone());
        assert_eq!(found.as_deref(), Some(want), "at {point:?}");
        let exact = space
            .find_chunk_from_point_exact(&point)
            .map(|c| c.identifier.clone());
        assert_eq!(exact.as_deref(), Some(want), "exact at {point:?}");
    }

    // Inside the in-bounds shell the hull displaces the outside chunk of
    // cell (0,0).
    let inside = space
        .find_chunk_from_point(&Vector3::new(30.0, 0.0, 30.0))
        .map(|c| c.identifier.clone());
    assert_eq!(inside.as_deref(), Some("00010000i"));
    // Just outside the shell, same grid cell: back to the outside chunk.
    let outside = space
        .find_chunk_from_point(&Vector3::new(80.0, 0.0, 30.0))
        .map(|c| c.identifier.clone());
    assert_eq!(outside.as_deref(), Some("00000000o"));
    // The outside-chunk face skips the hull walk entirely.
    let plain = space
        .find_outside_chunk_from_point(&Vector3::new(30.0, 0.0, 30.0))
        .map(|c| c.identifier.clone());
    assert_eq!(plain.as_deref(), Some("00000000o"));

    // The void: a cell in the focus grid that no loaded chunk owns —
    // 00020000o exists in the map (scan created it) but never loaded, so its
    // column never took a chunk.
    assert_eq!(
        space.find_chunk_from_point(&Vector3::new(250.0, 0.0, 50.0)),
        None
    );
    // The exact face adds nothing for a void cell.
    assert_eq!(
        space.find_chunk_from_point_exact(&Vector3::new(250.0, 0.0, 50.0)),
        None
    );
    // And a point past the mapping bounds is out of the focus grid span
    // entirely: no column, no answer.
    assert_eq!(
        space.find_chunk_from_point(&Vector3::new(-50.0, 0.0, 50.0)),
        None
    );
    assert_eq!(
        space.find_chunk_from_point(&Vector3::new(25000.0, 0.0, 50.0)),
        None
    );

    // `column()` creates on request and reports the same column the queries
    // walked; `column_at` reads without creating.
    let created = space.column(&Vector3::new(250.0, 0.0, 50.0), true);
    assert_eq!(
        created.and_then(|c| c.outside_chunk()),
        None,
        "the void column is empty"
    );
    assert!(
        space
            .column(&Vector3::new(-50.0, 0.0, 50.0), true)
            .is_none()
    );
}

/// BW-10-01-05: unloading takes the chunk's hull out of the index. The
/// reference ignores the chunk (`ChunkSpace::ignoreChunk`) before disposing
/// of it, deleting the nine columns around its centre so nothing ever finds
/// it in the focus grid again, and the deleted columns' other holdings are
/// smudged into the blurred queue for the next focus.
#[test]
fn unloading_removes_the_hull_and_blurs_the_neighbours() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);
    let camera = Vector3::new(50.0, 0.0, 50.0);
    let candidates = space.scan(&camera, 200.0);
    space.load_and_bind(&candidates, &resource_resolver);
    space.focus();
    assert!(
        space
            .find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0))
            .is_some()
    );

    // An unbound chunk is refused (`ignoreChunk` errors out and returns).
    space.chunk_mut("00010000o").unwrap().is_bound = false;
    assert!(!space.ignore_chunk("00010000o"));
    space.chunk_mut("00010000o").unwrap().is_bound = true;

    // Ignoring the camera's chunk deletes the nine columns around (0,0):
    // the neighbours it held are smudged into the blurred queue.
    assert!(space.ignore_chunk("00000000o"));
    assert!(space.chunk("00000000o").is_none(), "the chunk left the map");
    assert!(
        space
            .find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0))
            .is_none(),
        "the query answers nothing where the chunk stood"
    );
    // The deleted columns' surviving holdings wait for the pump: every cell
    // the chunk's hull overlapped answers nothing.
    assert!(space.focus_grid().get(0, 0).is_none());
    // The blurred queue carries the neighbours the deleted columns held
    // (deduped: a chunk smudges once per queue entry, not per column).
    assert!(
        space.blurred().contains(&"00000001o".to_string()),
        "blurred: {:?}",
        space.blurred()
    );
    let blurred_before = space.blurred().len();
    space.focus();
    // The refocus re-adds the still-standing neighbours to the rebuilt
    // columns; the queries answer again where they still stand.
    let right = space
        .find_chunk_from_point(&Vector3::new(150.0, 0.0, 50.0))
        .map(|c| c.identifier.clone());
    assert_eq!(right.as_deref(), Some("00010000o"));
    let _ = blurred_before;
}

/// BW-10-01-05: a query against a space whose mappings never covered the
/// point is the blank-space contract — no mapping, no focus span, no answer,
/// and the focus grid stays empty so nothing allocates.
#[test]
fn an_empty_space_answers_nothing_and_resizes_with_its_mappings() {
    let mut space = ChunkSpace::new(7, Vec::new());
    assert_eq!(space.focus_grid().span(), (0, 0, 0, 0));
    assert!(space.blurred().is_empty());
    assert_eq!(
        space.find_chunk_from_point(&Vector3::new(0.0, 0.0, 0.0)),
        None
    );
    assert!(space.column(&Vector3::new(0.0, 0.0, 0.0), true).is_none());

    // Recalculating over the fixture mapping sizes the window to the
    // mapping's bounds (the reference runs this whenever a mapping lands).
    space.mappings.push(load_mapping());
    space.recalc_grid_bounds();
    assert_eq!(space.focus_grid().span(), (0, 0, 18, 2));
    // The new columns are empty: the queued chunks are refocused by the
    // pump, and a query in a cell that owns no chunk answers nothing.
    assert_eq!(
        space.find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0)),
        None
    );
    // `column(point, canCreate)`: the reading face answers nothing for a cell
    // that has no column yet, and the same cell comes into being when the
    // caller asks for it.
    assert!(space.column_at(&Vector3::new(50.0, 0.0, 50.0)).is_none());
    let column = space
        .column(&Vector3::new(50.0, 0.0, 50.0), true)
        .expect("the cell is in the resized span");
    assert!(column.holdings().is_empty(), "a fresh column holds nothing");
    assert!(space.column_at(&Vector3::new(50.0, 0.0, 50.0)).is_some());
    // A chunk entering now lands in the resized index.
    let mapping = space.mappings[0].clone();
    let mut chunk = Chunk::new("00000000o", &mapping, &Matrix::identity());
    chunk.is_loaded = true;
    chunk.is_bound = true;
    space.add_chunk(chunk);
    let found = space
        .find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0))
        .map(|c| c.identifier.clone());
    assert_eq!(found.as_deref(), Some("00000000o"));
}

/// BW-10-01-05: the loader's own focus face. `EdgeGeometryMapping::
/// postBoundChunk` is what focusses a chunk in the reference loader
/// (`chunk_loading/edge_geometry_mapping.cpp:213-216`): it pulls the chunk
/// out of the blurred queue, focusses the hull and marks the nine columns
/// around the centre as having seen it — so a chunk bound this way never
/// waits for the pump.
#[test]
fn see_chunk_focusses_directly_and_pumps_skip_it() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);
    let camera = Vector3::new(50.0, 0.0, 50.0);
    let candidates = space.scan(&camera, 200.0);
    space.load_and_bind(&candidates, &resource_resolver);

    // The loader face: focus + see in one step, bypassing the queue.
    space.focus_chunk("00000000o");
    space.see_chunk("00000000o");
    assert!(
        space
            .find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0))
            .is_some()
    );
    // The chunk is not in the queue: the pump over an empty blurred queue
    // (the loader face already pulled it out) is a no-op.
    assert!(
        space.blurred().is_empty(),
        "the loader face consumed the blur"
    );
    space.focus();
    assert!(
        space
            .find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0))
            .is_some()
    );
    // A chunk the map never saw focusses nothing: the face skips the
    // unknown identifier and the queue stays empty.
    space.focus_chunk("00090009o");
    assert!(space.blurred().is_empty());

    // The pump ignores an identifier that left the map: it is dropped from
    // the queue, not crashed on (`removeFromBlurred` returns false, the
    // reference's while loop just moves on).
    space.chunk_mut("00000000o").unwrap().is_loaded = true;
    assert!(space.ignore_chunk("00000000o"));
    assert_eq!(
        space.find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0)),
        None
    );
}

/// BW-10-01-05: a negative-coordinate space. The reference's centre-cell
/// maths truncates toward zero and then decrements negatives — the port
/// pins the same cells so a space west of the origin indexes the same way.
#[test]
fn negative_grid_cells_index_like_the_reference_truncation() {
    let bounds = GridBounds {
        min_l_grid_x: -2,
        min_l_grid_y: -2,
        max_l_grid_x: -1,
        max_l_grid_y: -1,
        min_grid_x: -2,
        min_grid_y: -2,
        max_grid_x: -1,
        max_grid_y: -1,
    };
    let mapping = GeometryMapping::new("west/", 100.0, bounds, false);
    let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
    // Centre cell of (-50, -50) is (-1, -1) after the decrement; (-150,-50)
    // is (-2, -1).
    let mut west = Chunk::new("ffffffffo", &mapping, &Matrix::identity());
    west.bounding_box = foundation::math::BoundBox::new(
        Vector3::new(-100.0, -1.0, -100.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    west.is_loaded = true;
    west.is_bound = true;
    space.add_chunk(west);
    space.focus();
    // The chunk's centre (-50, 0, -50) sits in cell (-1, -1) and the query
    // in that cell answers it.
    let found = space
        .find_chunk_from_point(&Vector3::new(-50.0, 0.0, -50.0))
        .map(|c| c.identifier.clone());
    assert_eq!(found.as_deref(), Some("ffffffffo"));
    // A cell east of everything answers nothing.
    assert_eq!(
        space.find_chunk_from_point(&Vector3::new(50.0, 0.0, -50.0)),
        None
    );
}

/// BW-10-01-05: the focus window is a window. When the mappings' grid bounds
/// shrink, `ColumnGrid::rect` deletes the columns that fall out of the new
/// span — the reference destroys them, and destroying a column smudges the
/// chunks it held, so they must wait for the next focus rather than vanish
/// from the queue. The port has no obstacle face yet (the scene surface), so
/// it reports the deleted columns' holdings and blurs them.
#[test]
fn shrinking_the_space_blurs_the_displaced_columns() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);
    let camera = Vector3::new(50.0, 0.0, 50.0);
    let candidates = space.scan(&camera, 200.0);
    space.load_and_bind(&candidates, &resource_resolver);
    assert!(space.blurred().is_empty(), "the batch's pump already ran");

    // The cell east of the origin holds its own outside chunk.
    let displaced = space
        .focus_grid()
        .get(1, 0)
        .and_then(|c| c.outside_chunk())
        .map(str::to_string);
    assert_eq!(displaced.as_deref(), Some("00010000o"));

    // The space shrinks to the origin cell: every other column is deleted.
    space.mappings[0].bounds = GridBounds {
        min_l_grid_x: 0,
        min_l_grid_y: 0,
        max_l_grid_x: 0,
        max_l_grid_y: 0,
        min_grid_x: 0,
        min_grid_y: 0,
        max_grid_x: 0,
        max_grid_y: 0,
    };
    space.recalc_grid_bounds();
    assert_eq!(space.focus_grid().span(), (0, 0, 1, 1));
    assert!(
        space
            .blurred()
            .iter()
            .any(|id| Some(id.as_str()) == displaced.as_deref()),
        "the displaced chunk waits for a refocus: {:?}",
        space.blurred()
    );
    // The deleted columns are gone from the index, so a query past the new
    // span answers nothing instead of returning a dead hull.
    assert_eq!(
        space.find_chunk_from_point(&Vector3::new(150.0, 0.0, 50.0)),
        None
    );
}

/// BW-10-01-05: the pump over a queue entry whose chunk left the map. A bound
/// chunk waits for `focus`; if the map entry goes away without clearing the
/// queue, the pump must not reach for the gone chunk — it drops the entry,
/// the way the reference's loop moves on when the identifier does not
/// resolve.
#[test]
fn the_pump_drops_a_blurred_chunk_that_left_the_map() {
    let mapping = load_mapping();
    let mut space = ChunkSpace::new(0, vec![mapping]);
    assert!(space.load_chunk("00000000o", &resource_resolver));
    assert!(space.bind("00000000o"));
    // A bound chunk waits in the blurred queue for the focus pump.
    assert_eq!(space.blurred(), &["00000000o".to_string()]);

    // The map entry leaves behind the queue: the pump must not resolve it.
    space.remove_chunk("00000000o");
    space.focus();
    assert!(
        space.blurred().is_empty(),
        "the dangling entry is dropped: {:?}",
        space.blurred()
    );

    // Ignoring an identifier the map never held refuses and leaves the index
    // alone.
    assert!(!space.ignore_chunk("00090009o"));
}
