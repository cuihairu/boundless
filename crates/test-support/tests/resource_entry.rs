//! BW-16-04-02: the unified resource entry (`crates/resources`) serves
//! the editor, tool and app families from one artifact.
//!
//! One `BwResource` over one scratch root drives the same published space
//! through the faces each application family takes in the shipped
//! product:
//! - the editor face (`chunk::space_publish`) publishes `space_new/` and
//!   boots it back as a chunk space through the resource multiplexer
//!   (the BW-13-03-06 acceptance, now over the shared entry);
//! - the tool face (`asset_pipeline::res_packer`) packs a copy of the
//!   published `space.settings` through the same entry and reads it back
//!   typed-equal;
//! - the app face (`server_runtime::config`) boots the server config
//!   chain from the same entry — the `apps/server` startup order,
//!   resource first, config second.
//!
//! A second test walks the message-exact misses: a source without
//! settings refuses the publish, the packer refuses a missing section,
//! and a config chain with nothing to find is a bad config.

use asset_pipeline::res_packer::{self, PackerEnv};
use chunk::space_publish::{SourceSpace, SpaceRecreator, TerrainInfo};
use chunk::{ChunkSpace, GeometryMapping};
use resources::bwresource::BwResource;
use resources::datasection::{self, DataSection};
use resources::file_system::NativeFileSystem;
use resources::packed::PackedSection;
use server_runtime::config::BwConfig;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("bw16-resource-entry-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch root");
    dir
}

/// The version-200 terrain settings a real source `space.settings` saves.
fn source_settings_xml() -> String {
    [
        "<space.settings>",
        "  <chunkSize>100.0</chunkSize>",
        "  <bounds>",
        "    <minX>2</minX><minY>2</minY><maxX>3</maxX><maxY>3</maxY>",
        "  </bounds>",
        "  <terrain>",
        "    <version>200</version>",
        "    <heightMapEditorSize>64</heightMapEditorSize>",
        "    <heightMapSize>128</heightMapSize>",
        "    <normalMapSize>128</normalMapSize>",
        "    <holeMapSize>32</holeMapSize>",
        "    <shadowMapSize>32</shadowMapSize>",
        "    <blendMapSize>32</blendMapSize>",
        "  </terrain>",
        "</space.settings>",
    ]
    .concat()
}

/// Seed the source space the editor face recreates from.
fn seed_source_space(root: &std::path::Path) -> SourceSpace {
    std::fs::create_dir_all(root.join("space_src")).expect("source dir");
    std::fs::write(root.join("space_src/space.settings"), source_settings_xml()).expect("settings");
    std::fs::write(
        root.join("space_src/locations.xml"),
        "<locations.xml></locations.xml>",
    )
    .expect("locations");
    SourceSpace {
        grid_size: 100.0,
        min_x: 2,
        min_z: 2,
        max_x: 3,
        max_z: 3,
    }
}

fn terrain_info() -> TerrainInfo {
    TerrainInfo {
        version: 200,
        height_map_editor_size: 64,
        height_map_size: 128,
        normal_map_size: 128,
        hole_map_size: 32,
        shadow_map_size: 32,
        blend_map_size: 32,
    }
}

#[test]
fn one_resource_entry_serves_the_editor_tool_and_app_faces() {
    let root = scratch("faces");
    let root_str = root.to_str().expect("utf8 root");
    // The app face reads server/bw.xml from the same root.
    std::fs::create_dir_all(root.join("server")).expect("server dir");
    std::fs::write(
        root.join("server/bw.xml"),
        "<root>\n\t<app>\n\t\t<port>5000</port>\n\t</app>\n\t<shared><limit>10</limit></shared>\n</root>\n",
    )
    .expect("bw.xml");

    // The unified entry over the scratch root.
    let resource = BwResource::init_paths(&[root_str]);

    // ---- editor face: publish the space through the entry's file system.
    // `create` takes the raw `FileSystem` seam; mount the same root the
    // entry mounted (`refresh_paths` builds this exact file system).
    let src = seed_source_space(&root);
    let native = NativeFileSystem::new(&format!("{root_str}/"));
    let mut recreator =
        SpaceRecreator::new("space_src", "space_new", 100.0, 4, 4, terrain_info(), src);
    assert!(!recreator.changing_chunk_size(&src));
    assert!(
        recreator.create(&native),
        "create failed: {}",
        recreator.error_message()
    );

    // ---- the same entry boots the published space back: the settings
    // come through the multiplexer, and every chunk load+bind rides the
    // mapping over it.
    let settings_raw = resource
        .file_system()
        .read_file("space_new/space.settings")
        .expect("settings through the entry");
    let doc = datasection::parse("space.settings", &settings_raw).expect("settings parse");
    let mapping = GeometryMapping::from_settings("space_new/", &doc);
    let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
    let resolver = |file: &str| -> Option<DataSection> {
        resource
            .file_system()
            .read_file(&format!("{}{}", mapping.path, file))
            .and_then(|raw| datasection::parse("root", &raw).ok())
    };
    let mut bound = 0;
    for x in -2..=1 {
        for z in -2..=1 {
            let identifier = mapping.outside_chunk_identifier(x, z, false);
            assert!(
                space.load_chunk(&identifier, &resolver),
                "load {identifier}"
            );
            assert!(space.bind(&identifier), "bind {identifier}");
            bound += 1;
        }
    }
    assert_eq!(bound, 16, "every grid cell booted");

    // ---- tool face: pack a copy of the published settings through the
    // same entry, then read it back typed-equal.
    let mut env = PackerEnv::new(&resource);
    assert!(res_packer::convert(
        &mut env,
        "space_new/space.settings",
        "space_new/settings.packed",
        &[],
        false,
        0
    ));
    assert!(!env.errors_occurred);
    let packed_raw = resource
        .file_system()
        .read_file("space_new/settings.packed")
        .expect("packed through the entry");
    assert!(PackedSection::is_packed_file(&packed_raw));
    let packed = PackedSection::parse("space.settings", &packed_raw);
    assert_eq!(packed.find_child("chunkSize").unwrap().as_float(0.0), 100.0);
    let bounds = packed.find_child("bounds").expect("bounds child");
    for (tag, want) in [("minX", -2), ("maxX", 1), ("minY", -2), ("maxY", 1)] {
        assert_eq!(bounds.find_child(tag).unwrap().as_int(99), want, "{tag}");
    }
    let xml_terrain = doc.open_section("terrain").expect("xml terrain");
    let packed_terrain = packed.find_child("terrain").expect("packed terrain");
    assert_eq!(packed_terrain.find_child("version").unwrap().as_int(0), 200);
    assert_eq!(
        packed_terrain
            .find_child("heightMapEditorSize")
            .unwrap()
            .as_int(0),
        64
    );
    assert_eq!(
        xml_terrain.count_children(),
        packed_terrain.count_children(),
        "the packed tree carries every terrain child"
    );

    // ---- app face: the role binary's boot order — resource first,
    // config second — over the same entry.
    let mut config = BwConfig::new();
    config.init_from_sources(&resource, None);
    assert!(!config.is_bad(&resource));
    assert_eq!(config.get_int("shared/limit", 0), 10);
    assert_eq!(config.get_int("app/port", 0), 5000);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_entry_reports_misses_to_every_family() {
    let root = scratch("misses");
    let root_str = root.to_str().expect("utf8 root");

    // ---- editor face: a source space without settings refuses the
    // publish with the reference's copy failure.
    let native = NativeFileSystem::new(&format!("{root_str}/"));
    let src = SourceSpace {
        grid_size: 100.0,
        min_x: 2,
        min_z: 2,
        max_x: -1,
        max_z: -1,
    };
    let mut recreator =
        SpaceRecreator::new("space_src", "space_new", 100.0, 4, 4, terrain_info(), src);
    assert!(!recreator.create(&native));
    assert_eq!(recreator.error_message(), "Unable to copy space.settings\n");

    let resource = BwResource::init_paths(&[root_str]);

    // ---- tool face: converting a section the entry cannot find is the
    // message-exact open failure.
    let mut env = PackerEnv::new(&resource);
    assert!(!res_packer::convert(
        &mut env,
        "space_new/space.settings",
        "space_new/settings.packed",
        &[],
        false,
        0
    ));
    assert!(env.errors_occurred);
    assert!(
        env.out
            .contains("Failed to open data section space_new/space.settings"),
        "{}",
        env.out
    );

    // ---- app face: a config chain with nothing to find is a bad config.
    std::fs::create_dir_all(root.join("server")).expect("server dir");
    let mut config = BwConfig::new();
    config.init_from_sources(&resource, None);
    assert!(config.is_bad(&resource));

    let _ = std::fs::remove_dir_all(&root);
}
