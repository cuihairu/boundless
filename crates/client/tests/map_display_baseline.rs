//! BW-15-02-03's Linux client leg: the fixed map, displayed.
//!
//! The acceptance is "固定地图可显示并编辑"; this test pins the display
//! half over the reference fixture space (`docs/review/fixtures/
//! chunk_space`, the same 18×2 grid the chunk crate's load baseline
//! streams). The run loads the space through the shipped
//! `load_and_bind` sequence, snapshots the map into a [`MapScene`] and
//! presents frames through the software device.
//!
//! The structural ledger is asserted and must rerun identically: the six
//! bound identifiers (the same ledger the load baseline pins), a frame
//! series whose consecutive checksums differ (the marker moves) but
//! whose *map* pixels are bit-identical between frames (the map is
//! static), item dots at the world positions the fixture's chunk files
//! declare, and byte-identical checksums on an independent second run.
//!
//! ```sh
//! cargo test -p client --test map_display_baseline
//! ```

use std::path::{Path, PathBuf};

use client::map_view::{
    self, BACKGROUND, INSET_FILL, LIGHT_DOT, MARKER, MODEL_DOT, MapScene, OTHER_DOT, TILE_BORDER,
};
use client::render::software::SoftwareDevice;

/// The loaded extents (grid 0..=17 x 0..=1) plus the inside chunk's
/// transform origin (150, 0, 250 → grid 1..=2) make a 18×3 view.
const WIDTH: u32 = 32 + 18 * 48;
const HEIGHT: u32 = 32 + 3 * 48;

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

/// The six identifiers a cold start binds: every `.chunk` file on disk
/// (four origin cells, the nested prefix cell, the inside chunk) — the
/// same ledger the chunk crate's load baseline pins.
fn expected_bound() -> Vec<String> {
    let mut ids: Vec<String> = vec![
        "00000000i",
        "00000000o",
        "00000001o",
        "00010000o",
        "00010001o",
        "001x000x/sep/00110000o",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    ids.sort();
    ids
}

#[test]
fn fixed_map_display_run_is_deterministic() {
    let frames = 8;
    let first = map_view::run_map_display(&space_root(), frames).expect("display runs");
    let second = map_view::run_map_display(&space_root(), frames).expect("display reruns");

    // The bound ledger: the same six files the load baseline binds.
    let mut bound = first.bound.clone();
    bound.sort();
    assert_eq!(
        bound,
        expected_bound(),
        "the cold start binds the six files"
    );

    // The buffer sizes the loaded extents imply (an 18×3 view: the
    // nested prefix cell reaches grid x 17, the inside chunk grid z 2).
    assert_eq!((first.width, first.height), (WIDTH, HEIGHT));
    assert_eq!((second.width, second.height), (first.width, first.height));

    // Every frame actually rendered, and consecutive frames differ —
    // the marker moved.
    assert_eq!(first.frames.len(), frames as usize);
    for (index, pair) in first.frames.windows(2).enumerate() {
        assert_eq!(pair[0].frame_index, index as u64 + 1);
        assert_ne!(
            pair[0].checksum, pair[1].checksum,
            "frame {} differs from its successor",
            index
        );
    }

    // Byte-deterministic: an independent run produces the same checksum
    // series and the same final back buffer.
    let checksums: Vec<u64> = first.frames.iter().map(|f| f.checksum).collect();
    let again: Vec<u64> = second.frames.iter().map(|f| f.checksum).collect();
    assert_eq!(checksums, again, "the frame series reruns identically");
    assert_eq!(first.back_buffer.pixels, second.back_buffer.pixels);
}

#[test]
fn fixed_map_pixels_show_the_fixture_content() {
    let space = map_view::load_fixture_space(&space_root()).expect("space loads");
    let mut scene = MapScene::from_space(&space);
    let (width, height) = scene.buffer_size();
    let mut device = SoftwareDevice::new(width, height);

    // Two consecutive frames: the map pixels are identical, the marker
    // moved, the checksums differ.
    scene.step();
    let first = device.present_with(|fb| scene.draw(fb));
    let map_pixels: Vec<u32> = device.framebuffer().pixels.clone();
    let (mx0, my0) = scene.marker_px();
    scene.step();
    let second = device.present_with(|fb| scene.draw(fb));
    let (mx1, my1) = scene.marker_px();
    assert_ne!(first.checksum, second.checksum, "the frame changed");
    assert!(
        (mx0 - mx1).abs() > 1.0 || (my0 - my1).abs() > 1.0,
        "the marker advances between frames"
    );

    // The inside chunk drew its inset at its transform origin. The
    // fixture's inside transform is (150, 0, 250) world metres, outside
    // the drawn outside-chunk extents; the view expands to include it.
    let inside = scene.inside_origin.expect("the inside chunk loaded");
    let (ix, iz) = scene.world_to_px(&inside);
    assert_eq!(
        device.framebuffer().get_pixel(ix as u32, iz as u32),
        INSET_FILL,
        "the inside chunk inset is drawn at its transform origin"
    );

    // Every tile's fill/border pixel is where the grid puts it and
    // unchanged between frames; every item dot sits exactly where the
    // fixture's chunk files declare its world position.
    for tile in &scene.tiles {
        let x0 = 16 + (i32::from(tile.x) - scene.min_grid_x) as u32 * 48;
        let z0 = 16 + (i32::from(tile.z) - scene.min_grid_z) as u32 * 48;
        assert_eq!(
            device.framebuffer().get_pixel(x0, z0),
            TILE_BORDER,
            "tile {} border on the grid line",
            tile.identifier
        );
        let fill_mid = device.framebuffer().get_pixel(x0 + 2, z0 + 2);
        assert_ne!(fill_mid, BACKGROUND, "tile {} drew a fill", tile.identifier);
        assert_eq!(
            map_pixels[(z0 + 2) as usize * width as usize + (x0 + 2) as usize],
            fill_mid,
            "tile {} fill stable between frames",
            tile.identifier
        );
        for item in &tile.items {
            let (fx, fz) = scene.world_to_px(&item.world);
            let rgba = device.framebuffer().get_pixel(fx as u32, fz as u32);
            let expected = match item.tag.as_str() {
                "model" => MODEL_DOT,
                "light" => LIGHT_DOT,
                _ => OTHER_DOT,
            };
            assert_eq!(
                rgba, expected,
                "the {} dot of {} sits at its world position ({fx:.1},{fz:.1})",
                item.tag, tile.identifier
            );
        }
    }

    // A cell with no bound chunk stays background: grid (17, 1) is in
    // the 18×2 bounds but has no file on disk.
    let empty_x = 16 + (17 - scene.min_grid_x) as u32 * 48 + 24;
    let empty_z = 16 + (1 - scene.min_grid_z) as u32 * 48 + 24;
    assert_eq!(
        device.framebuffer().get_pixel(empty_x, empty_z),
        BACKGROUND,
        "the unbound cell stays background"
    );

    // The marker is where the stepper says it is, and painted over the
    // map (its pixel is the marker colour).
    let (mx, my) = scene.marker_px();
    assert_eq!(
        device.framebuffer().get_pixel(mx as u32, my as u32),
        MARKER,
        "the marker paints at its phase position"
    );
}
