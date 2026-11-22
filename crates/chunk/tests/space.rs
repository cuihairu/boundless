//! BW-10-01-01 fixture evidence: a hand-authored space directory parses into
//! the same chunk set and object set the reference loader would enumerate.
//! The fixtures live in `docs/review/fixtures/chunk_space`; their derivation
//! from the reference `chunk format.txt` and `settings.xml` layout is
//! documented in `docs/review/bw10-01-chunk-load.md`.

use std::path::{Path, PathBuf};

use chunk::{Chunk, GeometryMapping, GridBounds};
use foundation::math::Matrix;
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

fn load_chunk_file(path: &Path, mapping: &GeometryMapping, id: &str) -> Chunk {
    let raw = std::fs::read(path).unwrap_or_else(|_| panic!("{} readable", path.display()));
    let section = datasection::parse("", &raw).expect("chunk file parses");
    let mut chunk = Chunk::new(id, mapping, &Matrix::identity());
    assert!(chunk.load(&section, &Matrix::identity()), "{} loaded", id);
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
