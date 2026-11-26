//! OPT-13-01-01: the cold-build, incremental and editor-load baseline.
//!
//! The acceptance: **a fixed-asset baseline forms** (`形成固定资产
//! 基线`). The three legs measure the pipeline on a real tree — real
//! files, real cache copies, real dependency hashing:
//!
//! - **The cold build**: an empty content-addressable cache; every
//!   asset pays hash → cache miss → write output → store into the
//!   cache. The miss and write counts come from the cache's own event
//!   seam.
//! - **The incremental build**: a fresh output tree against the warm
//!   cache; every asset pays hash → cache hit (the copy replaces the
//!   conversion). The speedup against cold is the cache's worth.
//! - **The editor load**: the JIT's bookkeeping — a `TaskStore` scan
//!   (info records + requested set + scanning callbacks) and a
//!   `DependencyList::initialise` per asset (source, converter and
//!   params each hashed through a real file read). This is the editor's
//!   wait-to-ready tax before any conversion starts.
//!
//! Everything is deterministic: fixed 4 KiB asset bytes, one temp root,
//! one run.

use std::cell::Cell;
use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use asset_pipeline::content_addressable_cache::{CacheEvents, ContentAddressableCache};
use asset_pipeline::conversion_task::ConversionTask;
use asset_pipeline::dependency::Dependency;
use asset_pipeline::dependency_list::{DependencyList, HashProvider};
use asset_pipeline::hash64;
use asset_pipeline::task_store::{TaskInfoState, TaskStore};

/// The assets in the fixture tree.
const ASSETS: usize = 48;
/// The bytes per asset.
const ASSET_BYTES: usize = 4096;
/// The converter identity the dependency leg declares.
const CONVERTER_ID: u64 = 7;
const CONVERTER_VERSION: &str = "1.2";
const CONVERTER_PARAMS: &str = "quality=medium;mips=4";

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
        if let Ok(path) = std::env::var("PIPELINE_BASELINE_OUT") {
            std::fs::write(&path, &text).expect("write PIPELINE_BASELINE_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// The cache's event seam, counting hits and misses.
#[derive(Default)]
struct Counter {
    reads: Cell<usize>,
    read_misses: Cell<usize>,
    writes: Cell<usize>,
    write_misses: Cell<usize>,
}

/// The shared counter the cache reports through.
struct SharedCounter(Rc<Counter>);

impl CacheEvents for SharedCounter {
    fn on_cache_read(&self, _filename: &str) {
        self.0.reads.set(self.0.reads.get() + 1);
    }
    fn on_cache_read_miss(&self, _filename: &str) {
        self.0.read_misses.set(self.0.read_misses.get() + 1);
    }
    fn on_cache_write(&self, _filename: &str) {
        self.0.writes.set(self.0.writes.get() + 1);
    }
    fn on_cache_write_miss(&self, _filename: &str) {
        self.0.write_misses.set(self.0.write_misses.get() + 1);
    }
}

/// The dependency hash oracle: hashes the file's bytes for source
/// dependencies, the identity text for the others (the reference's
/// `getHash` per dependency kind).
struct FileHasher;

impl HashProvider for FileHasher {
    fn get_hash(&self, dependency: &Dependency) -> u64 {
        match dependency {
            Dependency::SourceFile(source) => {
                let bytes = std::fs::read(source.file_name()).unwrap_or_default();
                hash64::compute_bytes(&bytes)
            }
            Dependency::Converter(converter) => hash64::compute_str(&format!(
                "{}:{}",
                converter.converter_id(),
                converter.converter_version()
            )),
            Dependency::ConverterParams(params) => hash64::compute_str(params.converter_params()),
            _ => 0,
        }
    }
}

/// Averages a timed loop into ns per op.
fn avg_ns(total: u64, ops: usize) -> f64 {
    total as f64 / ops as f64
}

/// Times one leg over `ops` items.
fn time_leg(ops: usize, mut op: impl FnMut(usize)) -> f64 {
    let t0 = Instant::now();
    for i in 0..ops {
        op(i);
    }
    avg_ns(t0.elapsed().as_nanos() as u64, ops)
}

/// The deterministic asset bytes: position-valued, no randomness.
fn asset_bytes(index: usize) -> Vec<u8> {
    (0..ASSET_BYTES)
        .map(|b| ((index * 31 + b) % 251) as u8)
        .collect()
}

/// Creates the temp root with the source tree and the output dirs.
fn make_root(tag: &str) -> String {
    static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "pipeline_baseline_{tag}_{}_{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("temp root");
    for dir in ["src", "out-cold", "out-warm", "cache"] {
        std::fs::create_dir_all(root.join(dir)).expect("tree dir");
    }
    for i in 0..ASSETS {
        std::fs::write(root.join(format!("src/asset{i}.model")), asset_bytes(i))
            .expect("asset file");
    }
    root.to_string_lossy().into_owned()
}

/// Baselines the three pipeline legs and leaves the record.
#[test]
fn the_build_pipeline_baseline_forms() {
    let started = Instant::now();
    let mut record = Record::default();
    let root = make_root("tree");

    // The content-hashing face on its own: FNV-1a over the source bytes,
    // with every asset pre-read so the hash leg carries no I/O.
    let sources: Vec<Vec<u8>> = (0..ASSETS)
        .map(|i| std::fs::read(format!("{root}/src/asset{i}.model")).expect("source reads"))
        .collect();
    let hash_ns = time_leg(ASSETS, |i| {
        let hash = hash64::compute_bytes(&sources[i]);
        assert_ne!(hash, 0);
    });
    record.put("hash_ns_avg", format!("{hash_ns:.0}"));

    // The cold leg: hash → miss → write output → store in the cache.
    let counter = Rc::new(Counter::default());
    let mut cache = ContentAddressableCache::new(SharedCounter(Rc::clone(&counter)));
    cache.set_cache_path(format!("{root}/cache/"));
    let cold_ns = time_leg(ASSETS, |i| {
        let output = format!("{root}/out-cold/asset{i}.model");
        let bytes = &sources[i];
        let hash = hash64::compute_bytes(bytes);
        assert!(
            !cache.read_from_cache(&output, hash),
            "a cold cache serves nothing"
        );
        std::fs::write(&output, bytes).expect("output writes");
        assert!(cache.write_to_cache(&output, hash), "the store lands");
    });
    assert_eq!(counter.read_misses.get(), ASSETS);
    assert_eq!(counter.writes.get(), ASSETS);
    assert_eq!(counter.reads.get(), 0);
    record.put("cold_assets", ASSETS);
    record.put("cold_ns_avg", format!("{cold_ns:.0}"));
    record.put("cold_cache_misses", counter.read_misses.get());
    record.put("cold_cache_writes", counter.writes.get());

    // The incremental leg: a fresh output tree against the warm cache —
    // every asset serves from the cache.
    let warm_ns = time_leg(ASSETS, |i| {
        let output = format!("{root}/out-warm/asset{i}.model");
        let bytes = &sources[i];
        let hash = hash64::compute_bytes(bytes);
        assert!(cache.read_from_cache(&output, hash), "a warm cache serves");
        let served = std::fs::read(&output).expect("the served output reads");
        assert_eq!(&served, bytes, "the cache restores the bytes");
    });
    assert_eq!(counter.reads.get(), ASSETS);
    assert_eq!(counter.write_misses.get(), 0);
    record.put("warm_ns_avg", format!("{warm_ns:.0}"));
    record.put("warm_cache_hits", counter.reads.get());
    record.put("cold_to_warm_speedup", format!("{:.2}", cold_ns / warm_ns));

    // The editor load: the JIT bookkeeping — a TaskStore scan plus a
    // DependencyList::initialise per asset.
    let store = Rc::new(TaskStore::new());
    let started_count = Rc::new(Cell::new(0usize));
    let stopped_count = Rc::new(Cell::new(0usize));
    let scan_started = Rc::clone(&started_count);
    let scan_stopped = Rc::clone(&stopped_count);
    store.set_status_callbacks(
        Some(Box::new(move || scan_started.set(scan_started.get() + 1))),
        Some(Box::new(move || scan_stopped.set(scan_stopped.get() + 1))),
    );
    let tasks: Vec<ConversionTask> = (0..ASSETS)
        .map(|i| ConversionTask::new(format!("src/asset{i}.model")))
        .collect();

    store.scanning_started();
    let scan_ns = time_leg(ASSETS, |i| {
        let info = store.find_or_create_task_info(&tasks[i]);
        assert_eq!(info.source, format!("src/asset{i}.model"));
        store.add_requested_task(&tasks[i]);
    });
    store.scanning_stopped();
    assert_eq!(started_count.get(), 1);
    assert_eq!(stopped_count.get(), 1);
    record.put("editor_scan_ns_avg", format!("{scan_ns:.0}"));
    record.put("editor_scan_callbacks", started_count.get());

    let convert_ns = time_leg(ASSETS, |i| {
        store.set_current_task_state(&tasks[i], TaskInfoState::Converting);
        store.set_task_current(&tasks[i]);
        store.set_task_complete(&tasks[i]);
    });
    let infos: Vec<_> = (0..ASSETS)
        .map(|i| store.find_task_info(&tasks[i]).expect("the record exists"))
        .collect();
    assert!(infos.iter().all(|info| info.source.ends_with(".model")));
    record.put("editor_convert_ns_avg", format!("{convert_ns:.0}"));
    record.put("editor_task_records", infos.len());

    let mut deps = DependencyList::new(FileHasher);
    let deps_ns = time_leg(ASSETS, |i| {
        deps.initialise(
            format!("{root}/src/asset{i}.model"),
            CONVERTER_ID,
            CONVERTER_VERSION,
            CONVERTER_PARAMS,
        );
        assert_eq!(deps.primary_inputs().len(), 3);
    });
    record.put("editor_deps_ns_avg", format!("{deps_ns:.0}"));
    record.put("editor_deps_per_task", 3);

    std::fs::remove_dir_all(&root).ok();
    record.put(
        "baseline_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
