//! OPT-13-01-03: the map-load baseline.
//!
//! The acceptance: **compare editor wait times** (`比较编辑等待时间`).
//! Opening a space in the editor waits on the space settings parse and
//! then, per chunk, on the `.cdata` archive read plus the height-map
//! PNG decode. This harness builds a real space — a real
//! `space.settings`, real zipped chunk archives from real
//! `TerrainHeightMap2` saves — and compares the loading strategies:
//!
//! - **One-shot load**: the reference's shape — init the helper, then
//!   every chunk's archive read + height decode in one blocking pass.
//! - **Sequential re-load**: the helper re-inits and every chunk
//!   re-reads and re-decodes — the editor reopening the same space,
//!   with nothing carried over.
//! - **The reused decode**: the same chunks served from a kept
//!   image cache — the optimization candidate: pay the archive +
//!   decode once, then the wait per visible chunk is a cache lookup.
//! - **Parallel chunks**: four workers each decoding a disjoint
//!   quarter of the grid — the reference's BgTaskManager-shaped
//!   concurrency the JIT's single-threaded port doesn't have.
//!
//! Everything is deterministic: one temp root, fixed space geometry,
//! fixed pole heights, one run.

use std::cell::RefCell;
use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use asset_pipeline::offline_processor::outside_chunk_identifier;
use asset_pipeline::world_machine_import::{FloatImage, SpaceHelper, load_height_map};
use resources::zip::ZipFileSystem;
use terrain::editor_block::{HEIGHTS_SECTION_NAME, TERRAIN2_SECTION_NAME};
use terrain::height_map::{TerrainHeightMap2, quantise, unquantise};

/// The chunks along each axis (a 4x4 grid).
const GRID: i32 = 4;
/// The total chunks.
const CHUNKS: usize = (GRID * GRID) as usize;
/// The visible workers for the parallel leg.
const WORKERS: usize = 4;

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
        if let Ok(path) = std::env::var("MAP_LOAD_OUT") {
            std::fs::write(&path, &text).expect("write MAP_LOAD_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// A deterministic pole height per chunk-local coordinate.
fn pole_height(x: f32, z: f32) -> f32 {
    (x * 37.0 + z * 11.0).sin() * 0.5
}

/// Builds the space directory: a real `space.settings` and a real
/// zipped `.cdata` per grid cell, each carrying a real quantised
/// height-map save (the same shape the editor's exporter writes).
fn make_space(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).expect("space dir");
    let settings = format!(
        "<space.settings><bounds><minX>0</minX><maxX>{}</maxX><minY>0</minY>\
         <maxY>{}</maxY></bounds><chunkSize>100</chunkSize><singleDir>false\
         </singleDir></space.settings>",
        GRID - 1,
        GRID - 1
    );
    std::fs::write(dir.join("space.settings"), settings).expect("settings");

    for chunk_z in 0..GRID {
        for chunk_x in 0..GRID {
            let mut map = TerrainHeightMap2::with_size(100.0, 32, 0);
            let poles = map.image_data().len();
            let width = map.poles_width();
            for index in 0..poles {
                let x = (index as u32 % width) as f32;
                let z = (index as u32 / width) as f32;
                map.set_pole(x as i32, z as i32, pole_height(x, z));
            }
            let mut inner = ZipFileSystem::open("", ZipFileSystem::empty_archive())
                .expect("an empty archive always parses");
            assert!(inner.write_to_structure(HEIGHTS_SECTION_NAME, map.save()));
            let mut cdata = ZipFileSystem::open("", ZipFileSystem::empty_archive())
                .expect("an empty archive always parses");
            assert!(cdata.write_to_structure(TERRAIN2_SECTION_NAME, inner.create_zip()));
            let name = format!(
                "{}.cdata",
                outside_chunk_identifier(chunk_x, chunk_z, false)
            );
            std::fs::write(dir.join(name), cdata.create_zip()).expect("chunk archive");
        }
    }
}

/// Loads every chunk of the space: the reference's per-chunk wait —
/// archive read then height decode.
fn load_all_chunks(helper: &SpaceHelper) -> Vec<Option<FloatImage>> {
    (0..CHUNKS)
        .map(|index| {
            let x = index as i32 % GRID;
            let z = index as i32 / GRID;
            load_height_map(helper.get_cdata_for_chunk(x, z).as_deref())
        })
        .collect()
}

/// The map-load baseline runs.
#[test]
fn the_map_load_baseline_compares() {
    let started = Instant::now();
    let mut record = Record::default();

    let root = std::env::temp_dir().join(format!(
        "map_load_{}_{}",
        std::process::id(),
        std::sync::atomic::AtomicU32::new(1).fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let dir = root.to_string_lossy().into_owned();
    make_space(std::path::Path::new(&dir));

    // The one-shot load: init once, decode every chunk.
    let one_shot_t0 = Instant::now();
    let mut helper = SpaceHelper::new();
    assert!(
        helper.init(&format!("{dir}/space.settings")),
        "the space settings parse"
    );
    let maps = load_all_chunks(&helper);
    let one_shot_ms = one_shot_t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(maps.len(), CHUNKS);
    assert!(maps.iter().all(Option::is_some), "every chunk decodes");

    // The decode values are the quantised source heights: re-derive
    // the oracle for one chunk and compare every pole against the
    // bilinear of the quantised grid (the port's own decode shape).
    let first = maps[0].as_ref().expect("decoded");
    assert_eq!((first.width, first.height), (36, 36));
    let width = first.width as usize;
    let expected: Vec<f32> = (0..width * first.height as usize)
        .map(|index| {
            let x = (index % width) as f32;
            let z = (index / width) as f32;
            unquantise(quantise(pole_height(x, z)))
        })
        .collect();
    assert_eq!(
        first.pixels, expected,
        "the decoded poles match the quantised source"
    );

    // The settings fields the editor waits on: bounds, chunk size.
    assert_eq!(helper.x_min(), 0);
    assert_eq!(helper.x_max(), GRID - 1);
    assert_eq!(helper.z_min(), 0);
    assert_eq!(helper.z_max(), GRID - 1);
    assert_eq!(helper.chunk_size(), 100.0);

    // The sequential re-load: a fresh helper re-inits and every chunk
    // re-reads and re-decodes — the editor reopening with nothing kept.
    let re_init_t0 = Instant::now();
    let mut reloaded = SpaceHelper::new();
    assert!(reloaded.init(&format!("{dir}/space.settings")));
    let re_init_ns = re_init_t0.elapsed().as_nanos() as u64;
    let reread_t0 = Instant::now();
    let reread = load_all_chunks(&reloaded);
    let reread_ms = reread_t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(reread.len(), CHUNKS);
    assert!(
        reread
            .iter()
            .zip(&maps)
            .all(|(a, b)| a.as_ref().unwrap().pixels == b.as_ref().unwrap().pixels),
        "the re-load decodes the same maps"
    );
    record.put("space_settings_ns_avg", format!("{re_init_ns}"));
    record.put("one_shot_load_ms", format!("{one_shot_ms:.3}"));
    record.put("reload_load_ms", format!("{reread_ms:.3}"));
    record.put(
        "reload_to_oneshot_ratio",
        format!("{:.2}", reread_ms / one_shot_ms),
    );

    // The reused decode: the same chunks served from a kept image
    // cache — the wait per visible chunk is a lookup, not a decode.
    let cache: Rc<RefCell<Vec<Option<FloatImage>>>> = Rc::new(RefCell::new(maps.clone()));
    let lookup_t0 = Instant::now();
    let lookup: Vec<Option<FloatImage>> = cache.borrow().clone();
    let lookup_ms = lookup_t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(lookup.len(), CHUNKS);
    record.put("cached_lookup_ms", format!("{lookup_ms:.3}"));
    record.put(
        "lookup_to_load_ratio",
        format!("{:.4}", lookup_ms / one_shot_ms),
    );

    // The parallel chunks: four workers, each decoding a disjoint
    // quarter of the grid (the archive reads and PNG decodes are
    // per-chunk independent).
    let parallel_t0 = Instant::now();
    let per_worker = CHUNKS / WORKERS;
    let decoded: Vec<Option<FloatImage>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..WORKERS)
            .map(|w| {
                let dir = dir.clone();
                scope.spawn(move || {
                    // Each worker owns its helper and its disjoint
                    // quarter of the grid — the archive reads and the
                    // PNG decodes have no shared state.
                    let mut worker = SpaceHelper::new();
                    assert!(worker.init(&format!("{dir}/space.settings")));
                    ((w * per_worker)..((w + 1) * per_worker))
                        .map(|index| {
                            let x = index as i32 % GRID;
                            let z = index as i32 / GRID;
                            load_height_map(worker.get_cdata_for_chunk(x, z).as_deref())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("worker completes"))
            .collect()
    });
    let parallel_ms = parallel_t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(decoded.len(), CHUNKS);
    assert!(
        decoded
            .iter()
            .zip(&maps)
            .all(|(a, b)| a.as_ref().unwrap().pixels == b.as_ref().unwrap().pixels),
        "the parallel decode agrees with the sequential one"
    );
    record.put("workers", WORKERS);
    record.put("parallel_load_ms", format!("{parallel_ms:.3}"));
    record.put(
        "parallel_speedup",
        format!("{:.2}", one_shot_ms / parallel_ms),
    );

    std::fs::remove_dir_all(&root).ok();
    record.put("chunks", CHUNKS);
    record.put(
        "map_load_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
