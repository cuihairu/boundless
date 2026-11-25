//! BW-10-02-02 integration: the reference-generated `scene.bin` fixture
//! (`docs/review/fixtures/compiled_space/scene/`, produced by the
//! unmodified BigWorld `StaticSceneWriter` pipeline and dumped with the
//! reference readers) must load through the port and answer the same
//! fixed-region queries with the same object sets. The printed ground
//! truth lives next to the fixture in `gen_scene.cpp` / `scene_out.txt`.

use std::collections::BTreeSet;

use compiled_space::{
    BinaryFormat, DataSpan, INVALID_DATA_REFERENCE, INVALID_NODE, LEAF_REF_FLAG,
    STATIC_OBJECT_STATIC_MODEL, STATIC_SCENE_FORMAT_MAGIC, StaticScene, StringTable, TypeHeader,
};
use foundation::math::{BoundBox, Vector3};

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/review/fixtures/compiled_space/scene/scene.bin"
    ))
    .expect("scene.bin fixture")
}

fn bb(min: [f32; 3], max: [f32; 3]) -> BoundBox {
    BoundBox::new(
        Vector3::new(min[0], min[1], min[2]),
        Vector3::new(max[0], max[1], max[2]),
    )
}

#[test]
fn container_has_the_reference_section_table() {
    let format = BinaryFormat::open("scene.bin", fixture_bytes()).unwrap();
    let magics: Vec<_> = (0..format.num_sections())
        .map(|i| format.section_magic(i).unwrap())
        .collect();
    assert_eq!(magics.len(), 5);
    assert_eq!(&magics[0], b"BWST");
    assert_eq!(&magics[1], b"BWAL");
    assert_eq!(&magics[2], b"BWCS");
    assert_eq!(&magics[3], &STATIC_SCENE_FORMAT_MAGIC);
    assert_eq!(&magics[4], b"BWSM");
    for i in 0..format.num_sections() {
        assert_eq!(format.section_version(i), Some(1));
    }
    let lengths: Vec<_> = (0..format.num_sections())
        .map(|i| format.section_length(i).unwrap())
        .collect();
    assert_eq!(lengths, vec![262, 40, 28, 936, 476]);
}

#[test]
fn string_table_matches_the_reference_dump() {
    let format = BinaryFormat::open("scene.bin", fixture_bytes()).unwrap();
    let strings = StringTable::read(&format).unwrap();
    assert_eq!(strings.size(), 10);
    let expect = [
        "spaces/sample_set/textures/grass.tga",
        "shaders/std_effects/light_only.fx",
        "spaces/sample_set/chunks/001.chunk",
        "models/box_a.model",
        "models/box_b.model",
        "walk",
        "hull",
        "red",
        "turret",
        "blue",
    ];
    for (idx, name) in expect.iter().enumerate() {
        assert_eq!(strings.entry_str(idx), Some(*name));
    }
}

#[test]
fn static_scene_loads_the_spatial_data() {
    let format = BinaryFormat::open("scene.bin", fixture_bytes()).unwrap();
    let scene = StaticScene::load(&format).unwrap();
    assert!(scene.warnings.is_empty());

    let config = &scene.octree.config;
    assert_eq!(config.max_depth, 2);
    assert_eq!(config.num_data_entries, 5);
    assert_eq!(config.world_center, Vector3::new(-1.5, 4.75, 0.0));
    assert_eq!(config.world_size, 88.0);

    assert_eq!(scene.octree.nodes.len(), 10);
    assert_eq!(scene.octree.bounds.len(), 10);
    assert_eq!(scene.octree.centers.len(), 10);
    assert_eq!(scene.octree.data_references.len(), 10);
    assert_eq!(scene.octree.parents.len(), 10);

    // Root: loose union over the whole scene, DFS-compressed children
    // [8, 6, 4, 1], no parent, no data of its own.
    assert_eq!(
        scene.octree.bounds[0],
        bb([-44.0, 1.0, -44.0], [41.0, 8.5, 44.0])
    );
    assert_eq!(scene.octree.centers[0], Vector3::new(-1.5, 4.75, 0.0));
    assert_eq!(
        scene.octree.nodes[0].child,
        [
            8,
            6,
            4,
            1,
            INVALID_NODE,
            INVALID_NODE,
            INVALID_NODE,
            INVALID_NODE
        ]
    );
    assert_eq!(scene.octree.parents[0], INVALID_NODE);
    assert_eq!(scene.octree.data_references[0], INVALID_DATA_REFERENCE);

    // The internal nodes each own a single leaf reference; the leaves
    // carry the data references.
    assert_eq!(scene.octree.nodes[1].child[0], LEAF_REF_FLAG | 3);
    assert_eq!(scene.octree.nodes[1].child[1], LEAF_REF_FLAG | 2);
    assert_eq!(scene.octree.parents[1], 0);
    assert_eq!(scene.octree.nodes[4].child[0], LEAF_REF_FLAG | 5);
    assert_eq!(scene.octree.nodes[6].child[0], LEAF_REF_FLAG | 7);
    assert_eq!(scene.octree.nodes[8].child[0], LEAF_REF_FLAG | 9);
    assert_eq!(
        scene.octree.data_references,
        vec![
            INVALID_DATA_REFERENCE,
            INVALID_DATA_REFERENCE,
            4,
            0,
            INVALID_DATA_REFERENCE,
            2,
            INVALID_DATA_REFERENCE,
            3,
            INVALID_DATA_REFERENCE,
            1,
        ]
    );
    assert_eq!(
        scene.octree.parents,
        vec![INVALID_NODE, 0, 1, 1, 0, 4, 0, 6, 0, 8]
    );
    // One sampled leaf bound and center from the dump.
    assert_eq!(
        scene.octree.bounds[2],
        bb([39.0, 5.5, 39.0], [41.0, 6.5, 41.0])
    );
    assert_eq!(scene.octree.centers[9], Vector3::new(-34.5, -6.25, -33.0));
}

#[test]
fn static_scene_loads_spans_contents_and_object_data() {
    let format = BinaryFormat::open("scene.bin", fixture_bytes()).unwrap();
    let scene = StaticScene::load(&format).unwrap();

    assert_eq!(
        scene.node_contents_spans,
        vec![
            DataSpan { first: 0, last: 0 },
            DataSpan { first: 1, last: 1 },
            DataSpan { first: 2, last: 2 },
            DataSpan { first: 3, last: 3 },
            DataSpan { first: 4, last: 4 },
        ]
    );
    assert_eq!(scene.contents, vec![0, 1, 2, 3, 4]);
    assert_eq!(
        scene.type_headers,
        vec![TypeHeader {
            type_id: STATIC_OBJECT_STATIC_MODEL,
            num_objects: 5,
        }]
    );

    assert_eq!(
        scene.object_bounds,
        vec![
            bb([0.0, 4.0, 0.0], [20.0, 6.0, 20.0]),
            bb([-44.0, 1.0, -44.0], [-36.0, 3.0, -36.0]),
            bb([39.0, 7.5, -41.0], [41.0, 8.5, -39.0]),
            bb([-44.0, 2.0, 36.0], [-36.0, 4.0, 44.0]),
            bb([39.0, 5.5, 39.0], [41.0, 6.5, 41.0]),
        ]
    );
    let spheres: Vec<(Vector3, f32)> = scene
        .object_spheres
        .iter()
        .map(|s| (s.center, s.radius))
        .collect();
    let want = [
        (Vector3::new(10.0, 5.0, 10.0), 14.177447),
        (Vector3::new(-40.0, 2.0, -40.0), 5.744563),
        (Vector3::new(40.0, 8.0, -40.0), 1.5),
        (Vector3::new(-40.0, 3.0, 40.0), 5.744563),
        (Vector3::new(40.0, 6.0, 40.0), 1.5),
    ];
    assert_eq!(spheres.len(), want.len());
    for ((center, radius), (want_center, want_radius)) in spheres.iter().zip(want.iter()) {
        assert_eq!(center, want_center);
        assert!(
            (radius - want_radius).abs() < 1e-4,
            "{radius} != {want_radius}"
        );
    }
}

#[test]
fn static_scene_loads_the_model_type() {
    let format = BinaryFormat::open("scene.bin", fixture_bytes()).unwrap();
    let scene = StaticScene::load(&format).unwrap();
    let strings = StringTable::read(&format).unwrap();
    assert_eq!(scene.models.len(), 5);
    assert_eq!(scene.resource_refs, vec![3, 4, 6, 7, 8, 9, 3, 4, 3, 4]);

    let scales = [10.0f32, 4.0, 2.0, 4.0, 2.0];
    let translates = [
        Vector3::new(10.0, 5.0, 10.0),
        Vector3::new(-40.0, 2.0, -40.0),
        Vector3::new(40.0, 8.0, -40.0),
        Vector3::new(-40.0, 3.0, 40.0),
        Vector3::new(40.0, 6.0, 40.0),
    ];
    for (idx, model) in scene.models.iter().enumerate() {
        assert_eq!(model.world_transform[0], [scales[idx], 0.0, 0.0, 0.0]);
        assert_eq!(model.world_transform[1], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(model.world_transform[2], [0.0, 0.0, scales[idx], 0.0]);
        assert_eq!(
            model.world_transform[3],
            [translates[idx].x, translates[idx].y, translates[idx].z, 1.0]
        );
    }

    // MODEL 0: two resources, the "walk" animation, two dyes, no flags.
    assert_eq!(scene.models[0].resource_ids.first, 0);
    assert_eq!(scene.models[0].resource_ids.last, 1);
    assert_eq!(scene.models[0].animation_name, 5);
    assert_eq!(scene.models[0].animation_multiplier, 2.5);
    assert_eq!(scene.models[0].flags, 0);
    assert_eq!(
        scene.models[0].tint_dyes,
        compiled_space::IndexRange { first: 2, last: 5 }
    );
    // MODEL 1: one resource, no animation, casts shadow + reflection.
    assert_eq!(scene.models[1].resource_ids.first, 6);
    assert_eq!(scene.models[1].resource_ids.last, 6);
    assert_eq!(scene.models[1].animation_name, u32::MAX);
    assert_eq!(scene.models[1].animation_multiplier, 0.0);
    assert_eq!(scene.models[1].flags, 3);
    assert_eq!(scene.models[1].tint_dyes, compiled_space::IndexRange::EMPTY);
    for model in &scene.models[2..] {
        assert_eq!(model.flags, 1);
    }

    // Resources resolve through the string table like loadSuperModel.
    assert_eq!(
        scene.model_resource_strings(0, &strings),
        Some(vec!["models/box_a.model", "models/box_b.model"])
    );
    assert_eq!(
        scene.model_resource_strings(1, &strings),
        Some(vec!["models/box_a.model"])
    );
    assert_eq!(
        scene.model_resource_strings(2, &strings),
        Some(vec!["models/box_b.model"])
    );
    assert_eq!(scene.model_scene_index(4), Some(4));
}

#[test]
fn fixed_regions_return_the_reference_object_sets() {
    let format = BinaryFormat::open("scene.bin", fixture_bytes()).unwrap();
    let scene = StaticScene::load(&format).unwrap();
    // Probe regions copied from gen_scene.cpp; the expected sets are the
    // reference reader's printed ground truth (scene_out.txt PROBE lines).
    let probes: [(&str, BoundBox, &[usize]); 8] = [
        (
            "whole",
            bb([-45.0, -2.0, -45.0], [42.0, 10.0, 45.0]),
            &[0, 1, 2, 3, 4],
        ),
        ("m01", bb([5.0, 3.0, 5.0], [15.0, 7.0, 15.0]), &[0]),
        ("m02", bb([-45.0, 0.0, -45.0], [-37.0, 4.0, -37.0]), &[1]),
        ("m03", bb([38.0, 6.0, -42.0], [42.0, 10.0, -38.0]), &[2]),
        ("m04", bb([-45.0, -2.0, 35.0], [-37.0, 8.0, 45.0]), &[3]),
        ("m05", bb([38.0, 4.0, 38.0], [42.0, 8.0, 42.0]), &[4]),
        ("empty", bb([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]), &[]),
        ("two", bb([-45.0, 0.0, -45.0], [42.0, 10.0, -38.0]), &[1, 2]),
    ];
    for (tag, region, want) in probes {
        let hits = scene.query_region(&region);
        let want: BTreeSet<usize> = want.iter().copied().collect();
        assert_eq!(hits, want, "probe {tag}");
    }
}

#[test]
fn repeated_queries_return_the_same_object_set() {
    // Acceptance: a fixed spatial query always returns the same object
    // collection.
    let format = BinaryFormat::open("scene.bin", fixture_bytes()).unwrap();
    let scene = StaticScene::load(&format).unwrap();
    let region = bb([-45.0, -2.0, -45.0], [42.0, 10.0, 45.0]);
    let first = scene.query_region(&region);
    for _ in 0..3 {
        assert_eq!(scene.query_region(&region), first);
    }
    let narrow = bb([5.0, 3.0, 5.0], [15.0, 7.0, 15.0]);
    let first = scene.query_region(&narrow);
    for _ in 0..3 {
        assert_eq!(scene.query_region(&narrow), first);
    }
}
