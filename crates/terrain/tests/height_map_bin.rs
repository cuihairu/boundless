//! BW-10-03-01 integration: the reference-generated height map fixtures
//! (`docs/review/fixtures/terrain/height_map/`, produced by the unmodified
//! BigWorld `compressHeightMap`/libpng pipeline) must load through the port
//! and answer every probe in the printed ground truth (`gen_height_map.log`,
//! dumped by the reference `TerrainHeightMap2::load` + `heightAt` faces).

use terrain::{LoadError, TerrainHeightMap2};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/terrain/height_map"
);

const BLOCK_SIZE: f32 = 100.0;

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).expect("fixture")
}

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_height_map.log"))
        .expect("truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_reference_fixtures_are_told_apart_by_magic() {
    let mut map = TerrainHeightMap2::new(BLOCK_SIZE, 0);
    assert_eq!(
        map.load(b"garbage data that is not a height map at all!"),
        Err(LoadError::NotHeightMap),
    );
    assert_eq!(map.load(&[]), Err(LoadError::Truncated));
    // Bad magic on a full-size blob.
    let mut blob = fixture_bytes("heightmap_qfloat.bin");
    blob[0] = b'x';
    assert_eq!(map.load(&blob), Err(LoadError::NotHeightMap));
    // The unmodified blob loads.
    map.load(&fixture_bytes("heightmap_qfloat.bin"))
        .expect("load");
}

#[test]
fn reference_qfloat_fixture_answers_every_truth_probe() {
    probe_truth("qfloat");
}

#[test]
fn reference_absfloat_fixture_answers_every_truth_probe() {
    probe_truth("absfloat");
}

fn probe_truth(label: &str) {
    let name = match label {
        "qfloat" => "heightmap_qfloat.bin",
        _ => "heightmap_absfloat.bin",
    };
    let blob = fixture_bytes(name);
    let mut map = TerrainHeightMap2::new(BLOCK_SIZE, 0);
    map.load(&blob).unwrap_or_else(|e| panic!("{label}: {e}"));

    let mut checked = 0u32;
    for line in truth_lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.first().copied() {
            Some("MAP") if fields[1] == label => match fields[2] {
                "poles" => {
                    // "poles %u x %u"
                    assert_eq!(fields[4], "x");
                    assert_eq!(map.poles_width(), fields[3].parse::<u32>().unwrap());
                    assert_eq!(map.poles_height(), fields[5].parse::<u32>().unwrap());
                    checked += 1;
                }
                "min" => {
                    let expect_min: f32 = fields[3].parse().unwrap();
                    let expect_max: f32 = fields[5].parse().unwrap();
                    assert_eq!(map.min_height(), expect_min, "{label} min");
                    assert_eq!(map.max_height(), expect_max, "{label} max");
                    checked += 1;
                }
                "spacing" => {
                    let sx: f32 = fields[3].parse().unwrap();
                    let sz: f32 = fields[4].parse().unwrap();
                    assert_eq!(map.spacing_x(), sx, "{label} spacing x");
                    assert_eq!(map.spacing_z(), sz, "{label} spacing z");
                    checked += 1;
                }
                "blocks" => {
                    assert_eq!(map.blocks_width(), fields[3].parse::<u32>().unwrap());
                    assert_eq!(map.blocks_height(), fields[4].parse::<u32>().unwrap());
                    assert_eq!(map.vertices_width(), fields[6].parse::<u32>().unwrap());
                    assert_eq!(map.vertices_height(), fields[7].parse::<u32>().unwrap());
                    assert_eq!(map.x_visible_offset(), fields[9].parse::<u32>().unwrap());
                    assert_eq!(map.z_visible_offset(), fields[10].parse::<u32>().unwrap());
                    checked += 1;
                }
                _ => {}
            },
            Some("GRID") if fields[1] == label => {
                let x: i32 = fields[2].parse().unwrap();
                let z: i32 = fields[3].parse().unwrap();
                // The truth log prints %.6f, which rounds: the reference
                // f32 -6.2230005… prints as -6.223001. Compare within the
                // print rounding instead of bit equality.
                let expect: f64 = fields[4].parse().unwrap();
                let got = map.height_at(x, z) as f64;
                assert!(
                    (got - expect).abs() < 1e-6,
                    "{label} grid ({x},{z}): got {got}, want {expect}"
                );
                checked += 1;
            }
            Some("INTERP") if fields[1] == label => {
                let x: f32 = fields[2].parse().unwrap();
                let z: f32 = fields[3].parse().unwrap();
                // The truth log prints %.6f of the reference float; allow a
                // half-ulp of print rounding (possible FMA contraction in
                // the reference build adds a little more).
                let expect: f64 = fields[4].parse().unwrap();
                let got = map.height_at_interpolated(x, z) as f64;
                assert!(
                    (got - expect).abs() < 1e-4,
                    "{label} interp ({x},{z}): got {got}, want {expect}"
                );
                checked += 1;
            }
            _ => {}
        }
    }
    // Every fixture produced one poles/min/spacing/blocks line each, 164
    // grid probes and 882 interpolated ones across the two fixtures.
    assert!(checked > 500, "{label}: only {checked} probes checked");
}

#[test]
fn quantised_and_absolute_fixtures_hold_the_same_heights() {
    // Both fixtures come from one image; the millimetre quantisation must
    // round-trip exactly, so every grid height matches bit for bit.
    let mut quantised = TerrainHeightMap2::new(BLOCK_SIZE, 0);
    quantised
        .load(&fixture_bytes("heightmap_qfloat.bin"))
        .expect("qfloat");
    let mut absolute = TerrainHeightMap2::new(BLOCK_SIZE, 0);
    absolute
        .load(&fixture_bytes("heightmap_absfloat.bin"))
        .expect("absfloat");

    assert_eq!(quantised.min_height(), absolute.min_height());
    assert_eq!(quantised.max_height(), absolute.max_height());
    // Millimetre quantisation keeps every grid height within half a
    // millimetre of the absolute floats (the two formats differ by at most
    // one ulp through the quantise/unquantise round trip, as the truth log
    // itself shows: 12.556001 vs 12.556).
    for z in 0..quantised.vertices_height() as i32 {
        for x in 0..quantised.vertices_width() as i32 {
            let q = quantised.height_at(x, z);
            let a = absolute.height_at(x, z);
            assert!(
                (q - a).abs() < 5.1e-4,
                "grid ({x},{z}): quantised {q} vs absolute {a}"
            );
        }
    }
}
