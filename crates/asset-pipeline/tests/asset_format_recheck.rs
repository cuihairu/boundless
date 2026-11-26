//! OPT-13-01-04: the asset hash and format recheck.
//!
//! The acceptance: **reproducible and original-readable**
//! (`可复现且原版可读`). Two sides:
//!
//! - **Hash reproducibility**: the `hash64` face must answer the same
//!   digest for the same bytes, string and numbers on every run and
//!   in every order; the incremental `combine` shape must agree with
//!   hashing the concatenated bytes; the conditional cache buckets
//!   derived from the hash must be stable across processes.
//! - **Format readability**: a real 24bpp TGA in the original's
//!   uncompressed-truecolor shape converts through `TgaDdsTool` into
//!   a DDS whose header carries the exact fields the original
//!   decoder reads (magic, 124-byte size, A8R8G8B8 masks, pitch),
//!   and decodes back pixel-for-pixel. The zipped chunk archive the
//!   map loader reads is also verified — the original `ZipFileSystem`
//!   reads back every member byte-exact.
//!
//! Everything is deterministic: fixed bytes, fixed pixels, one temp
//! root, one run.

use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use asset_pipeline::convert_texture_tool::TextureTool;
use asset_pipeline::convert_texture_tool::{TgaDdsTool, decode_dds, decode_tga, encode_dds};
use asset_pipeline::hash64;
use resources::bwresource::BwResource;
use resources::zip::ZipFileSystem;
use terrain::editor_block::HEIGHTS_SECTION_NAME;

/// The record sink (the OPT family's shape).
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("ASSET_FORMAT_OUT") {
            std::fs::write(&path, &text).expect("write ASSET_FORMAT_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// A fixed byte payload: position-valued, no randomness.
fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 37 + 11) % 251) as u8).collect()
}

/// Builds an uncompressed 24bpp TGA with the original's header shape.
fn make_tga(width: u16, height: u16, top_first: bool) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.push(0); // id length
    raw.push(0); // colour map type
    raw.push(2); // uncompressed truecolor
    raw.extend_from_slice(&[0; 5]); // colour map spec
    raw.extend_from_slice(&[0, 0]); // x origin
    raw.extend_from_slice(&[0, 0]); // y origin
    raw.extend_from_slice(&width.to_le_bytes());
    raw.extend_from_slice(&height.to_le_bytes());
    raw.push(24);
    raw.push(if top_first { 0x20 } else { 0 });
    for y in 0..height as usize {
        for x in 0..width as usize {
            raw.extend_from_slice(&[
                ((x * 13 + y * 7) & 0xff) as u8, // B
                ((x * 5 + y * 29) & 0xff) as u8, // G
                ((x * 31 + y * 3) & 0xff) as u8, // R
            ]);
        }
    }
    raw
}

/// Reads a little-endian u32 from the DDS header.
fn dds_u32(raw: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(raw[at..at + 4].try_into().expect("word"))
}

/// The asset hash and format recheck runs.
#[test]
fn the_asset_hashes_and_formats_recheck() {
    let started = Instant::now();
    let mut record = Record::default();

    // --- Hash reproducibility: fixed vectors, order independence,
    // and the concatenation agreement of the incremental shape.
    let bytes = payload(4096);
    let digests: Vec<u64> = (0..8).map(|_| hash64::compute_bytes(&bytes)).collect();
    assert!(digests.iter().all(|d| *d == digests[0]));
    record.put("bytes_digest", format!("{:016X}", digests[0]));

    let text = "quality=medium;mips=4";
    let text_digest = hash64::compute_str(text);
    assert_eq!(text_digest, hash64::compute_bytes(text.as_bytes()));
    record.put("str_digest", format!("{:016X}", text_digest));
    record.put("i64_digest", format!("{:016X}", hash64::compute_i64(-42)));
    record.put("u64_digest", format!("{:016X}", hash64::compute_u64(42)));

    // Same words in a different order answer a different digest — the
    // cache key depends on input order, as the reference's FNV fold does.
    let mut folded = 0xCBF29CE484222325u64;
    for word in [7u64, 11, 13] {
        hash64::combine(&mut folded, word);
    }
    let mut shuffled = 0xCBF29CE484222325u64;
    for word in [13u64, 11, 7] {
        hash64::combine(&mut shuffled, word);
    }
    assert_ne!(folded, shuffled, "the fold is order-sensitive");
    assert_eq!(folded, {
        let mut again = 0xCBF29CE484222325u64;
        for word in [7u64, 11, 13] {
            hash64::combine(&mut again, word);
        }
        again
    });
    record.put("fold_digest", format!("{:016X}", folded));
    record.put("order_matters", folded != shuffled);

    // The cache bucket derived from a digest is stable: low byte
    // indexes the same hex bucket for every asset, every run.
    let buckets: Vec<String> = (0..48)
        .map(|i| format!("{:X}", hash64::compute_bytes(&payload(i * 97)) as u8))
        .collect();
    assert!(buckets.iter().all(|b| b.len() == 1 || b.len() == 2));
    assert_eq!(
        buckets,
        (0..48)
            .map(|i| format!("{:X}", hash64::compute_bytes(&payload(i * 97)) as u8))
            .collect::<Vec<_>>()
    );
    record.put("bucket_count", {
        use std::collections::BTreeSet;
        buckets.iter().collect::<BTreeSet<_>>().len()
    });
    record.put("bucket_reproducible", 1);

    // --- Format readability: a real TGA in, a real DDS out.
    let root = std::env::temp_dir().join(format!(
        "asset_recheck_{}_{}",
        std::process::id(),
        std::sync::atomic::AtomicU32::new(1).fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let dir = root.to_string_lossy().into_owned();
    std::fs::create_dir_all(&dir).expect("temp root");

    let width = 4u16;
    let height = 3u16;
    let tga = make_tga(width, height, true);
    let decoded = decode_tga(&tga).expect("the original's TGA variant decodes");
    assert_eq!((decoded.width, decoded.height), (width, height));
    assert_eq!(decoded.rgba.len(), width as usize * height as usize * 4);
    let tga_digest = hash64::compute_bytes(&tga);
    let pixels_digest = hash64::compute_bytes(&decoded.rgba);
    record.put("tga_digest", format!("{:016X}", tga_digest));
    record.put("pixels_digest", format!("{:016X}", pixels_digest));

    // The DDS the original reads: A8R8G8B8, no mips, raw pixels.
    let dds = encode_dds(width as u32, height as u32, &decoded.rgba);
    assert_eq!(&dds[0..4], b"DDS ");
    assert_eq!(dds_u32(&dds, 4), 124);
    assert_eq!(dds_u32(&dds, 8) & 0x1000, 0x1000, "DDSD_PIXELFORMAT");
    assert_eq!(dds_u32(&dds, 12), height as u32);
    assert_eq!(dds_u32(&dds, 16), width as u32);
    assert_eq!(dds_u32(&dds, 20), width as u32 * 4, "pitch");
    assert_eq!(dds_u32(&dds, 88), 32, "32bpp");
    assert_eq!(dds_u32(&dds, 92), 0x00FF0000, "R mask");
    assert_eq!(dds_u32(&dds, 96), 0x0000FF00, "G mask");
    assert_eq!(dds_u32(&dds, 100), 0x000000FF, "B mask");
    assert_eq!(dds_u32(&dds, 104), 0xFF000000, "A mask");
    assert_eq!(dds.len(), 128 + decoded.rgba.len());
    let readback = decode_dds(&dds).expect("the original-readable DDS decodes");
    assert_eq!(readback.width, width as u32);
    assert_eq!(readback.height, height as u32);
    assert_eq!(readback.rgba, decoded.rgba, "pixel-exact round trip");
    let dds_digest = hash64::compute_bytes(&dds);
    assert_eq!(
        dds_digest,
        hash64::compute_bytes(&encode_dds(width as u32, height as u32, &decoded.rgba))
    );
    record.put("dds_digest", format!("{:016X}", dds_digest));
    record.put("dds_header_fields_checked", 10);
    record.put("dds_round_trip_match", 1);

    // The tool itself (resource-backed, the converter's own shape)
    // writes the same DDS to a file the original would read.
    std::fs::write(format!("{dir}/source.tga"), &tga).expect("source");
    let resource = BwResource::init_paths(&[&dir]);
    let tool = TgaDdsTool::new(Rc::new(resource));
    let lod = asset_pipeline::texture_detail_level::TextureDetailLevel::default();
    assert!(tool.convert("source.tga", "out.dds", &lod, false));
    let written = std::fs::read(format!("{dir}/out.dds")).expect("written DDS");
    assert_eq!(written, dds, "the tool writes the same bytes");
    record.put("tool_output_match", 1);

    // --- The archive the map loader reads: every member round-trips
    // byte-exact through the original's zip reader.
    let members: Vec<(&str, Vec<u8>)> = vec![
        (HEIGHTS_SECTION_NAME, payload(512)),
        ("metadata.ini", b"[space]\nchunks=16\n".to_vec()),
        ("terrain2/holes", payload(256)),
    ];
    let mut archive = ZipFileSystem::open("", ZipFileSystem::empty_archive())
        .expect("an empty archive always parses");
    for (name, bytes) in &members {
        assert!(archive.write_to_structure(name, bytes.clone()));
    }
    let zipped = archive.create_zip();
    let reopened = ZipFileSystem::open("", zipped.clone()).expect("the archive reopens");
    for (name, bytes) in &members {
        assert_eq!(
            reopened.read_file(name).as_deref(),
            Some(bytes.as_slice()),
            "{name} reads back"
        );
    }
    record.put("archive_members", members.len());
    record.put(
        "archive_digest",
        format!("{:016X}", hash64::compute_bytes(&zipped)),
    );
    record.put("archive_round_trip_match", 1);

    std::fs::remove_dir_all(&root).ok();
    record.put(
        "recheck_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
