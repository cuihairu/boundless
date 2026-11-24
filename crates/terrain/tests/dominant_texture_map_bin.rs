//! BW-10-03-03 integration: the reference-generated dominant texture map
//! fixture (`docs/review/fixtures/terrain/dominant_texture_map/`, produced
//! with the original `DominantTextureMapHeader` struct, the save routine's
//! strncpy name windows and the original `BinaryBlock::compress`) must load
//! through the port and answer every probe in the printed ground truth
//! (`gen_dominant_texture_map.log`, dumped by the original `load` +
//! `texture`/`groundType` faces).

use terrain::DominantTextureMap2;

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/terrain/dominant_texture_map"
);

const BLOCK_SIZE: f32 = 100.0;

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/dominant.bin")).expect("fixture")
}

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_dominant_texture_map.log"))
        .expect("truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn reference_fixture_answers_every_truth_probe() {
    let mut map = DominantTextureMap2::new(BLOCK_SIZE);
    map.load(Some(&fixture_bytes())).expect("load");

    let mut names: Vec<String> = Vec::new();
    let mut checked = 0u32;
    for line in truth_lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.first().copied() {
            // "MAP width %u height %u numTextures %u textureNameSize %u"
            Some("MAP") => {
                assert_eq!(map.width(), fields[2].parse::<u32>().unwrap(), "MAP width");
                assert_eq!(
                    map.height(),
                    fields[4].parse::<u32>().unwrap(),
                    "MAP height"
                );
                checked += 1;
            }
            // "NAME %u %s" — the pre-window strings must round trip.
            Some("NAME") => {
                names.push(fields[2].to_owned());
            }
            // "TEX %.2f %.2f %s"
            Some("TEX") => {
                let x: f32 = fields[1].parse().unwrap();
                let z: f32 = fields[2].parse().unwrap();
                assert_eq!(map.texture(x, z), fields[3], "TEX ({x},{z})");
                checked += 1;
            }
            // "GT %.2f %.2f %u"
            Some("GT") => {
                let x: f32 = fields[1].parse().unwrap();
                let z: f32 = fields[2].parse().unwrap();
                assert_eq!(
                    map.ground_type(x, z) as u32,
                    fields[3].parse::<u32>().unwrap(),
                    "GT ({x},{z})"
                );
                checked += 1;
            }
            _ => {}
        }
    }
    assert_eq!(names.len(), 3, "three names");
    assert_eq!(map.texture_names(), names.as_slice());
    // 1024 TEX + 1024 GT + 1 MAP.
    assert!(checked > 2000, "only {checked} probes checked");
}

#[test]
fn fixture_layout_matches_the_save_face() {
    // 32-byte header + 3 x 18 name bytes + a zip sub-block over the 6x4
    // image = 122 bytes in total.
    let blob = fixture_bytes();
    assert_eq!(blob.len(), 122, "total size");
    assert_eq!(&blob[0..4], &terrain::DOMINANT_MAGIC.to_le_bytes());
    assert_eq!(&blob[4..8], &terrain::DOMINANT_VERSION_ZIP.to_le_bytes());
    assert_eq!(&blob[8..12], &3u32.to_le_bytes(), "numTextures");
    assert_eq!(&blob[12..16], &18u32.to_le_bytes(), "textureNameSize");
    assert_eq!(&blob[16..20], &6u32.to_le_bytes(), "width");
    assert_eq!(&blob[20..24], &4u32.to_le_bytes(), "height");
    // "rock" sits in a window NUL-padded out to 18 bytes.
    assert_eq!(&blob[32 + 18..32 + 18 + 4], b"rock");
    assert!(blob[32 + 18 + 4..32 + 2 * 18].iter().all(|&b| b == 0));
    // The image sub-block is a BinaryBlock zip over exactly 24 bytes.
    assert_eq!(&blob[86..90], &terrain::ZIP_MAGIC1.to_le_bytes());
    assert_eq!(&blob[90..94], &terrain::ZIP_MAGIC2.to_le_bytes());
    assert_eq!(&blob[94..98], &(6u32 * 4).to_le_bytes());
}
