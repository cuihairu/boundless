//! BW-10-05-06: the NEW artifacts — the port's own final record
//! streams — reload in the ORIGINAL reader target. The test
//! regenerates the two harness scenes (slope + clipped shell)
//! through the ported flood + BSP generator pipeline, writes the
//! final streams (the bytes navgen saves into
//! `.cdata/worldNavmesh`) into `<tmp>/bw10-05-06/`, and pins the
//! length + fnv of both files. Those hashes are the ones of the
//! original-generator assets the el7 reader harness
//! `gen_navpaths.cpp` reloaded to produce the committed truth
//! `gen_navpaths.log` (docs/review/bw10-05-05-old-navmesh-reading.md):
//! the port-written files are byte-for-byte the assets the original
//! target already proved loadable, and the evidence run feeds the
//! written files to that same harness unchanged
//! (docs/review/bw10-05-06-new-artifact-roundtrip.md).

use crate::flood::MAX_HEIGHTS;
use crate::flood::artifacts::{clipped_chunk, shell_scene, slope_scene};
use crate::flood::tests::{Bounds, FieldPhysics, grid, seed};
use crate::generator::WaypointGenerator;
use crate::view::as_binary;
use foundation::math::vector::Vector3;

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for b in data {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

/// The final stream of one scene, the runScene tail without the dump:
/// flood -> grid copy -> generate -> streamline -> set membership ->
/// asBinary.
fn final_stream(
    name: &str,
    physics: &mut FieldPhysics,
    clip_chunk: bool,
    seeds: &[Vector3],
) -> Vec<u8> {
    let mut flood = grid(0.0, 8.0, 1.0);
    let chunk: Bounds = if clip_chunk {
        clipped_chunk()
    } else {
        Bounds::everywhere()
    };
    for s in seeds {
        flood.fill(s, physics, &chunk, None);
    }
    flood.postfilter_add();
    flood.postfilter_remove();

    let mut gener = WaypointGenerator::new();
    gener.init(
        flood.xsize(),
        flood.zsize(),
        &Vector3::new(0.0, 0.0, 0.0),
        1.0,
    );
    for g in 0..MAX_HEIGHTS {
        gener.hgt_grids_mut()[g] = flood.hgt_grids()[g].to_vec();
        gener.adj_grids_mut()[g] = flood.adj_grids()[g].to_vec();
    }

    gener.generate();
    gener.streamline();
    gener.calculate_set_membership(seeds, name);
    as_binary(&gener, 0.8)
}

/// slope's final stream: the 136-byte asset of the truth log.
const SLOPE_LEN: usize = 136;
const SLOPE_FNV: u64 = 0x3a23247435af66a6;
/// shell's final stream: the 152-byte asset of the truth log.
const SHELL_LEN: usize = 152;
const SHELL_FNV: u64 = 0xe01640188f0c6115;

#[test]
fn the_port_written_assets_match_the_reloaded_original_assets() {
    let mut slope = slope_scene();
    let slope_stream = final_stream("slope", &mut slope, false, &[seed(0.5, 10.0, 0.5)]);
    let mut shell = shell_scene();
    let shell_stream = final_stream(
        "shell",
        &mut shell,
        true,
        &[
            seed(0.5, 10.0, 0.5),
            seed(3.0, 10.0, 3.5),
            seed(2.5, 1.0, 4.5),
        ],
    );

    // The pinned hashes are the original-generator asset hashes the
    // el7 reader reloaded in BW-10-05-05.
    assert_eq!(slope_stream.len(), SLOPE_LEN, "slope asset length");
    assert_eq!(fnv1a64(&slope_stream), SLOPE_FNV, "slope asset fnv");
    assert_eq!(shell_stream.len(), SHELL_LEN, "shell asset length");
    assert_eq!(fnv1a64(&shell_stream), SHELL_FNV, "shell asset fnv");

    // Write the port's own artifacts where the reader harness picks
    // them up (it reads ./<scene>.navmesh from its working
    // directory), then read them back: the file bytes are the
    // stream, and the on-disk assets the harness reloads are the
    // port-written ones.
    let dir = std::env::temp_dir().join("bw10-05-06");
    std::fs::create_dir_all(&dir).expect("create the asset dir");
    for (name, stream) in [("slope", &slope_stream), ("shell", &shell_stream)] {
        let path = dir.join(format!("{name}.navmesh"));
        std::fs::write(&path, stream).expect("write the asset");
        assert_eq!(
            std::fs::read(&path).expect("read the asset back"),
            *stream,
            "the written {name} asset is the final stream"
        );
    }
}
