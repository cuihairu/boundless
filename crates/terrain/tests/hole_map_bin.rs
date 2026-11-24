//! BW-10-03-02 integration: the reference-generated hole map fixtures
//! (`docs/review/fixtures/terrain/hole_map/`, produced by the unmodified
//! BigWorld `TerrainHoleMap2::save`/`BinaryBlock` compress pipeline) must
//! load through the port and answer every probe in the printed ground
//! truth (`gen_hole_map.log`, dumped by the original `load` + `holeAt`
//! faces through the authentic compressed-section path).

use terrain::{HoleLoadError, TerrainHoleMap2};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/terrain/hole_map"
);

const BLOCK_SIZE: f32 = 100.0;

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).expect("fixture")
}

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_hole_map.log"))
        .expect("truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_null_section_path_reports_the_reference_error() {
    let mut map = TerrainHoleMap2::new(BLOCK_SIZE);
    assert_eq!(map.load(None), Err(HoleLoadError::NullDataSection));
}

#[test]
fn reference_hole_fixtures_answer_every_truth_probe() {
    let mut checked = 0u32;
    let mut fixtures = 0u32;
    for line in truth_lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first().copied() == Some("MAP") {
            let label = fields[1];
            let name = format!("holes_{label}.bin");
            let blob = fixture_bytes(&name);
            let mut map = TerrainHoleMap2::new(BLOCK_SIZE);
            map.load(Some(&blob))
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            // "MAP <label> width %u height %u holesSize %u
            // holesMapSize %u noHoles %d allHoles %d"
            assert_eq!(
                map.width(),
                fields[3].parse::<u32>().unwrap(),
                "{label} width"
            );
            assert_eq!(
                map.height(),
                fields[5].parse::<u32>().unwrap(),
                "{label} height"
            );
            assert_eq!(
                map.holes_size(),
                fields[7].parse::<u32>().unwrap(),
                "{label} holesSize"
            );
            assert_eq!(
                map.holes_map_size(),
                fields[9].parse::<u32>().unwrap(),
                "{label} holesMapSize"
            );
            assert_eq!(
                map.no_holes(),
                fields[11].parse::<u32>().unwrap() != 0,
                "{label} noHoles"
            );
            assert_eq!(
                map.all_holes(),
                fields[13].parse::<u32>().unwrap() != 0,
                "{label} allHoles"
            );
            fixtures += 1;
            checked += 1;
            // Keep the loaded map for this label's probes.
            probe_map(label, &map, &mut checked);
        }
    }
    // Three fixtures: mixed, none, all — ~318 probes total.
    assert_eq!(fixtures, 3, "three MAP lines");
    assert!(checked > 300, "only {checked} probes checked");
}

fn probe_map(label: &str, map: &TerrainHoleMap2, checked: &mut u32) {
    for line in truth_lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first().copied() == Some("HOLE") && fields[1] == label {
            let x: f32 = fields[2].parse().unwrap();
            let z: f32 = fields[3].parse().unwrap();
            let expect = fields[4].parse::<u32>().unwrap() != 0;
            assert_eq!(map.hole_at(x, z), expect, "{label} hole ({x},{z})");
            *checked += 1;
        }
        if fields.first().copied() == Some("RECT") && fields[1] == label {
            let xs: f32 = fields[2].parse().unwrap();
            let zs: f32 = fields[3].parse().unwrap();
            let xe: f32 = fields[4].parse().unwrap();
            let ze: f32 = fields[5].parse().unwrap();
            let expect = fields[6].parse::<u32>().unwrap() != 0;
            assert_eq!(
                map.hole_at_rect(xs, zs, xe, ze),
                expect,
                "{label} rect ({xs},{zs})-({xe},{ze})"
            );
            *checked += 1;
        }
    }
}

#[test]
fn fixtures_round_trip_through_the_reference_layout() {
    // The dumped .bin is the decompressed payload: 16-byte header plus the
    // packed rows of a 10x10 map (ceil(10/8) = 2 bytes per row).
    for name in ["holes_mixed.bin", "holes_none.bin", "holes_all.bin"] {
        let blob = fixture_bytes(name);
        assert_eq!(blob.len(), 16 + 10 * 2, "{name} size");
        assert_eq!(
            &blob[0..4],
            &terrain::HOLES_MAGIC.to_le_bytes(),
            "{name} magic"
        );
        assert_eq!(&blob[4..8], &10u32.to_le_bytes(), "{name} width");
        assert_eq!(&blob[8..12], &10u32.to_le_bytes(), "{name} height");
        assert_eq!(&blob[12..16], &1u32.to_le_bytes(), "{name} version");
    }
    // The three fixtures differ in their packed rows.
    let mixed = fixture_bytes("holes_mixed.bin");
    let none = fixture_bytes("holes_none.bin");
    let all = fixture_bytes("holes_all.bin");
    assert_ne!(&mixed[16..], &none[16..]);
    assert_ne!(&mixed[16..], &all[16..]);
    assert!(all[16..].iter().all(|b| *b == 0xff));
    assert!(none[16..].iter().all(|b| *b == 0));
}
