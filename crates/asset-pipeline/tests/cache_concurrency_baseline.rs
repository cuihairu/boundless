//! OPT-13-01-02: the cache-and-concurrency evaluation.
//!
//! The acceptance: **cold/hot timings compared** (`比较冷/热耗时`). The
//! build baseline (13-01-01) measured one cache instance; this eval
//! compares the cache's operating modes and its behaviour under
//! concurrent conversion:
//!
//! - **The mode legs**: the no-cache floor (both gates off), the cold
//!   build (reads off, writes on) and the hot build (reads on) run
//!   sequentially over the same 48-asset tree — the cold/hot ratio is
//!   the acceptance's comparison, and the no-cache floor prices the
//!   store overhead separately.
//! - **The concurrent legs**: four worker threads, each with its own
//!   `ContentAddressableCache` instance aimed at one shared cache
//!   directory (per-process temp names keep the copies unique), run
//!   the cold and hot builds in parallel against the sequential walls.
//! - **The cross-instance ledger**: the concurrent legs populate a
//!   fresh cache directory; a brand-new sequential instance must then
//!   serve all 48 assets from it byte-exact — the content addressing
//!   carries across writers and instances.
//!
//! Everything is deterministic: fixed position-valued asset bytes, one
//! temp root, one run.

use std::cell::Cell;
use std::io::Write as _;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use asset_pipeline::content_addressable_cache::{
    CacheEvents, ContentAddressableCache, NoCacheEvents,
};
use asset_pipeline::hash64;

/// The assets in the fixture tree.
const ASSETS: usize = 48;
/// The bytes per asset.
const ASSET_BYTES: usize = 4096;
/// The workers in the concurrent legs.
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
        if let Ok(path) = std::env::var("CACHE_CONCURRENCY_OUT") {
            std::fs::write(&path, &text).expect("write CACHE_CONCURRENCY_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// The sequential legs' event counter (single-threaded).
#[derive(Default)]
struct Counter {
    reads: Cell<usize>,
    read_misses: Cell<usize>,
    writes: Cell<usize>,
    write_misses: Cell<usize>,
}

/// The shared counter the sequential caches report through.
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

/// The concurrent legs' event counter (thread-safe).
#[derive(Default)]
struct AtomicCounter {
    reads: AtomicUsize,
    read_misses: AtomicUsize,
    writes: AtomicUsize,
    write_misses: AtomicUsize,
}

/// The shared counter the concurrent caches report through.
#[derive(Clone)]
struct ConcurrentCounter(Arc<AtomicCounter>);

impl CacheEvents for ConcurrentCounter {
    fn on_cache_read(&self, _filename: &str) {
        self.0.reads.fetch_add(1, Ordering::Relaxed);
    }
    fn on_cache_read_miss(&self, _filename: &str) {
        self.0.read_misses.fetch_add(1, Ordering::Relaxed);
    }
    fn on_cache_write(&self, _filename: &str) {
        self.0.writes.fetch_add(1, Ordering::Relaxed);
    }
    fn on_cache_write_miss(&self, _filename: &str) {
        self.0.write_misses.fetch_add(1, Ordering::Relaxed);
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

/// Creates the temp root with the source tree and every leg's dirs.
fn make_root(tag: &str) -> String {
    static RUN: AtomicUsize = AtomicUsize::new(0);
    let id = RUN.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "cache_concurrency_{tag}_{}_{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("temp root");
    for dir in [
        "src",
        "out-nocache",
        "out-cold",
        "out-hot",
        "out-ccold",
        "out-chot",
        "out-verify",
        "cache",
        "cache-c",
    ] {
        std::fs::create_dir_all(root.join(dir)).expect("tree dir");
    }
    for i in 0..ASSETS {
        std::fs::write(root.join(format!("src/asset{i}.model")), asset_bytes(i))
            .expect("asset file");
    }
    root.to_string_lossy().into_owned()
}

/// Counts the bucket directories a cache directory grew.
fn bucket_dirs(cache_root: &str) -> usize {
    std::fs::read_dir(cache_root)
        .expect("cache root reads")
        .filter(|entry| {
            entry
                .as_ref()
                .expect("entry")
                .file_type()
                .expect("type")
                .is_dir()
        })
        .count()
}

/// The cache-and-concurrency evaluation runs.
#[test]
fn the_cache_and_concurrency_evaluation_compares() {
    let started = Instant::now();
    let mut record = Record::default();
    let root = make_root("tree");
    let sources: Vec<Vec<u8>> = (0..ASSETS)
        .map(|i| std::fs::read(format!("{root}/src/asset{i}.model")).expect("source reads"))
        .collect();

    // --- The mode legs: no cache, cold, hot — sequential.
    let plain = Rc::new(ContentAddressableCache::new(NoCacheEvents));
    let nocache_ns = time_leg(ASSETS, |i| {
        let output = format!("{root}/out-nocache/asset{i}.model");
        let bytes = &sources[i];
        let hash = hash64::compute_bytes(bytes);
        assert!(
            !plain.read_from_cache(&output, hash),
            "a gate-off cache reads nothing"
        );
        assert!(
            !plain.write_to_cache(&output, hash),
            "a gate-off cache stores nothing"
        );
        std::fs::write(&output, bytes).expect("output writes");
    });

    let counter = Rc::new(Counter::default());
    let mut cold_cache = ContentAddressableCache::new(SharedCounter(Rc::clone(&counter)));
    cold_cache.set_cache_path(format!("{root}/cache/"));
    cold_cache.set_read_enabled(false);
    let cold_ns = time_leg(ASSETS, |i| {
        let output = format!("{root}/out-cold/asset{i}.model");
        let bytes = &sources[i];
        let hash = hash64::compute_bytes(bytes);
        assert!(
            !cold_cache.read_from_cache(&output, hash),
            "a cold cache serves nothing"
        );
        std::fs::write(&output, bytes).expect("output writes");
        assert!(cold_cache.write_to_cache(&output, hash), "the store lands");
    });
    // The disabled read gate returns before the event seam — a real
    // miss event needs the read gate on against an empty cache (the
    // 13-01-01 baseline's cold leg measures it).
    assert_eq!(counter.read_misses.get(), 0);
    assert_eq!(counter.writes.get(), ASSETS);
    record.put("nocache_ns_avg", format!("{nocache_ns:.0}"));
    record.put("cold_ns_avg", format!("{cold_ns:.0}"));
    record.put(
        "store_overhead_ratio",
        format!("{:.2}", cold_ns / nocache_ns),
    );

    let mut hot_cache = ContentAddressableCache::new(SharedCounter(Rc::clone(&counter)));
    hot_cache.set_cache_path(format!("{root}/cache/"));
    let hot_ns = time_leg(ASSETS, |i| {
        let output = format!("{root}/out-hot/asset{i}.model");
        let bytes = &sources[i];
        let hash = hash64::compute_bytes(bytes);
        assert!(
            hot_cache.read_from_cache(&output, hash),
            "a hot cache serves"
        );
        let served = std::fs::read(&output).expect("the served output reads");
        assert_eq!(&served, bytes, "the cache restores the bytes");
    });
    assert_eq!(counter.reads.get(), ASSETS);
    assert_eq!(counter.write_misses.get(), 0);
    record.put("hot_ns_avg", format!("{hot_ns:.0}"));
    record.put("cold_cache_writes", counter.writes.get());
    record.put("hot_cache_hits", counter.reads.get());
    record.put("cold_to_hot_speedup", format!("{:.2}", cold_ns / hot_ns));
    record.put("seq_buckets", bucket_dirs(&format!("{root}/cache")));

    // --- The concurrent legs: four instances, one shared cache dir.
    let shared = Arc::new(AtomicCounter::default());
    let chunk = ASSETS / WORKERS;
    let ccold_t0 = Instant::now();
    thread::scope(|scope| {
        for w in 0..WORKERS {
            let events = ConcurrentCounter(Arc::clone(&shared));
            let mut cache = ContentAddressableCache::new(events);
            cache.set_cache_path(format!("{root}/cache-c/"));
            cache.set_read_enabled(false);
            let sources = &sources;
            let root = &root;
            scope.spawn(move || {
                for (i, bytes) in sources
                    .iter()
                    .enumerate()
                    .take((w + 1) * chunk)
                    .skip(w * chunk)
                {
                    let output = format!("{root}/out-ccold/asset{i}.model");
                    let hash = hash64::compute_bytes(bytes);
                    assert!(!cache.read_from_cache(&output, hash));
                    std::fs::write(&output, bytes).expect("output writes");
                    assert!(cache.write_to_cache(&output, hash), "the store lands");
                }
            });
        }
    });
    let ccold_wall_ms = ccold_t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(shared.write_misses.load(Ordering::Relaxed), 0);
    assert_eq!(shared.writes.load(Ordering::Relaxed), ASSETS);
    assert_eq!(shared.read_misses.load(Ordering::Relaxed), 0);

    let chot_t0 = Instant::now();
    thread::scope(|scope| {
        for w in 0..WORKERS {
            let events = ConcurrentCounter(Arc::clone(&shared));
            let cache = ContentAddressableCache::new(events);
            let mut cache = cache;
            cache.set_cache_path(format!("{root}/cache-c/"));
            let sources = &sources;
            let root = &root;
            scope.spawn(move || {
                for (i, bytes) in sources
                    .iter()
                    .enumerate()
                    .take((w + 1) * chunk)
                    .skip(w * chunk)
                {
                    let output = format!("{root}/out-chot/asset{i}.model");
                    let hash = hash64::compute_bytes(bytes);
                    assert!(cache.read_from_cache(&output, hash), "a hot cache serves");
                    let served = std::fs::read(&output).expect("the served output reads");
                    assert_eq!(&served, bytes, "the cache restores the bytes");
                }
            });
        }
    });
    let chot_wall_ms = chot_t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        shared.reads.load(Ordering::Relaxed),
        ASSETS,
        "every concurrent serve hits"
    );
    record.put("workers", WORKERS);
    record.put("concurrent_stores", shared.writes.load(Ordering::Relaxed));
    record.put("concurrent_serves", shared.reads.load(Ordering::Relaxed));
    record.put("ccold_wall_ms", format!("{ccold_wall_ms:.3}"));
    record.put("chot_wall_ms", format!("{chot_wall_ms:.3}"));
    record.put(
        "concurrent_buckets",
        bucket_dirs(&format!("{root}/cache-c")),
    );

    // --- The cross-instance ledger: a fresh sequential instance serves
    // everything the concurrent writers stored.
    let counter2 = Rc::new(Counter::default());
    let mut verifier = ContentAddressableCache::new(SharedCounter(Rc::clone(&counter2)));
    verifier.set_cache_path(format!("{root}/cache-c/"));
    let verify_hits = Rc::new(Cell::new(0usize));
    let verify_ref = Rc::clone(&verify_hits);
    time_leg(ASSETS, |i| {
        let output = format!("{root}/out-verify/asset{i}.model");
        let bytes = &sources[i];
        let hash = hash64::compute_bytes(bytes);
        assert!(
            verifier.read_from_cache(&output, hash),
            "the fresh instance serves the concurrent store"
        );
        let served = std::fs::read(&output).expect("the served output reads");
        assert_eq!(&served, bytes, "the cross-instance restore is byte-exact");
        verify_ref.set(verify_ref.get() + 1);
    });
    assert_eq!(counter2.reads.get(), ASSETS);
    record.put("cross_instance_serves", verify_hits.get());

    std::fs::remove_dir_all(&root).ok();
    record.put(
        "eval_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
