//! OPT-13-01-02: evaluate the conversion cache and the concurrency
//! faces over the fixed asset set.
//!
//! The pipeline cache leg drives the shipped replay path end to end: a
//! converter that declares the shared palette as a secondary source
//! dependency, a cold build that stores every dependency list in the
//! content cache, a flip of the palette that misses and converts, and
//! a flip back that **hits** — the cached dependency list is restored
//! and validated against the current file hashes, so the walk
//! re-converts nothing. Two hot-restore walks rebuild from the cache
//! alone: with the output directories kept, everything restores and
//! nothing converts; with a bare output root, the restore stage (a
//! temp copy inside the target file's directory, then a rename) cannot
//! create directories, so exactly one task per missing output
//! subdirectory falls back to conversion — which recreates the tree —
//! and the remaining tasks restore. A cache-blind rebuild re-converts
//! everything. The direct cache face drive asserts the bucket layout
//! (`{cachePath}{low byte %hhX}/{basename}.{hash %X}`), the
//! basename-keyed restore, the hit/miss events and the disabled
//! gates. The concurrency faces store their state and the single
//! threaded loop reruns to identical artifacts regardless of the
//! stored thread count.
//!
//! Wall-clocks are printed (`CACHE-EVAL` lines) as probe data only;
//! the contracts are asserted.
//!
//! ```sh
//! cargo test -p asset-pipeline --test cache_concurrency_evaluation
//! ```

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use asset_pipeline::batch_compiler::BatchCompiler;
use asset_pipeline::compiler::Compiler;
use asset_pipeline::content_addressable_cache::{CacheEvents, ContentAddressableCache};
use asset_pipeline::conversion_rule::ConversionRule;
use asset_pipeline::conversion_task::ConversionTask;
use asset_pipeline::converter::Converter;
use asset_pipeline::converter_info::{ConverterCreator, ConverterFlags, ConverterInfo};
use asset_pipeline::dependency_list::DependencyList;
use asset_pipeline::hash64::compute_bytes;
use resources::bwresource::BwResource;

// ---------------------------------------------------------------------------
// The scoped temp dir (the channel-cost tests' pattern).
// ---------------------------------------------------------------------------

static WALKS: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ap-cacheeval-{name}-{}-{}",
            std::process::id(),
            WALKS.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// The fixed asset set plus the shared palette dependency.
// ---------------------------------------------------------------------------

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/baseline");

/// The palette every asset's converter declares as a secondary
/// dependency (a `.palette` file, so no conversion rule claims it).
const PALETTE_V1: &str = "palette v1: 16 deterministic colors";
const PALETTE_V2: &str = "palette v2: 16 different deterministic colors";

fn fixture_relpaths() -> Vec<String> {
    let mut rels = Vec::new();
    for sub in ["models", "textures", "ui"] {
        let dir = Path::new(FIXTURES).join(sub);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("fixture subdirectory")
            .map(|e| {
                e.expect("fixture entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        for name in names {
            rels.push(format!("{sub}/{name}"));
        }
    }
    rels
}

/// Plant the 12 fixtures plus the palette into a fresh source tree.
fn plant_assets(root: &Path, palette_content: &str) -> String {
    let assets = root.join("assets");
    for rel in fixture_relpaths() {
        let target = assets.join(&rel);
        std::fs::create_dir_all(target.parent().expect("parent")).expect("asset directory");
        std::fs::copy(Path::new(FIXTURES).join(&rel), &target).expect("asset copied");
    }
    let palette_dir = assets.join("palettes");
    std::fs::create_dir_all(&palette_dir).expect("palette directory");
    std::fs::write(palette_dir.join("common.palette"), palette_content).expect("palette written");
    assets.to_string_lossy().into_owned()
}

fn set_palette(assets: &str, content: &str) {
    std::fs::write(Path::new(assets).join("palettes/common.palette"), content)
        .expect("palette rewritten");
}

// ---------------------------------------------------------------------------
// The pipeline seams: a copy converter that declares the palette.
// ---------------------------------------------------------------------------

/// The counting converter's convert calls (test 1 owns this counter).
static CONVERTS: AtomicUsize = AtomicUsize::new(0);

/// A rule that claims `.txt` source files as root tasks for `type_id`.
struct DepRule(u64);

impl ConversionRule for DepRule {
    fn create_root_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
        if source_file.ends_with(".txt") {
            task.converter_id = self.0;
            task.converter_version = "1.0".to_string();
            true
        } else {
            false
        }
    }

    fn create_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
        self.create_root_task(source_file, task)
    }
}

/// A converter that declares the shared palette as a secondary source
/// dependency and copies the source to the resolved output path.
struct DepConverter;

impl Converter for DepConverter {
    fn create_dependencies(
        &self,
        _source_file: &str,
        _compiler: &dyn Compiler,
        dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        // resource-relative: the dependency resolves through the
        // resource search paths (the reference's dep paths are relative)
        dependencies.add_secondary_source_file_dependency("palettes/common.palette", true);
        true
    }

    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        CONVERTS.fetch_add(1, Ordering::SeqCst);
        let source = source_file.trim_end_matches('/');
        let mut out = source.to_string();
        if !compiler.resolve_output_path(&mut out) {
            return false;
        }
        let content = std::fs::read(source).expect("source readable");
        if let Some(parent) = Path::new(&out).parent() {
            std::fs::create_dir_all(parent).expect("output directory");
        }
        std::fs::write(&out, &content).expect("output written");
        output_files.push(out);
        true
    }
}

/// A plain copy converter (no dependencies) for the concurrency leg.
struct PlainConverter;

impl Converter for PlainConverter {
    fn create_dependencies(
        &self,
        _source_file: &str,
        _compiler: &dyn Compiler,
        _dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        true
    }

    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        let source = source_file.trim_end_matches('/');
        let mut out = source.to_string();
        if !compiler.resolve_output_path(&mut out) {
            return false;
        }
        let content = std::fs::read(source).expect("source readable");
        if let Some(parent) = Path::new(&out).parent() {
            std::fs::create_dir_all(parent).expect("output directory");
        }
        std::fs::write(&out, &content).expect("output written");
        output_files.push(out);
        true
    }
}

fn plain_converter(_params: &str) -> Box<dyn Converter> {
    Box::new(PlainConverter)
}

fn dep_converter(_params: &str) -> Box<dyn Converter> {
    Box::new(DepConverter)
}

/// One evaluation tree: the fixed assets plus the palette planted, the
/// persistent `out/`/`intermediate/`/`cache/` roots created. Each walk
/// registers a FRESH session over these persistent trees — the shipped
/// `queue_task` only queues `status == New` tasks, so a second build on
/// the same session re-queues nothing and would exercise no mechanism.
struct EvalTree {
    _dir: TempDir,
    assets: String,
    output: String,
    intermediate: String,
    cache: String,
}

fn eval_tree(tag: &str, palette: &str) -> EvalTree {
    let dir = TempDir::new(tag);
    let assets = plant_assets(dir.path(), palette);
    let output = format!("{}/out/", dir.path().to_string_lossy());
    let intermediate = format!("{}/intermediate/", dir.path().to_string_lossy());
    let cache = format!("{}/cache/", dir.path().to_string_lossy());
    // the shipped write path requires the cache ROOT to pre-exist (only
    // the bucket subdirectories are auto-created)
    std::fs::create_dir_all(&cache).expect("cache root");
    EvalTree {
        _dir: dir,
        assets,
        output,
        intermediate,
        cache,
    }
}

/// A fresh pipeline session over the persistent tree: paths set, the
/// content cache pointed at `{root}/cache/`, rule + converter
/// registered, `init_compiler` done.
fn register_session(tree: &EvalTree, type_id: u64, creator: ConverterCreator) -> BatchCompiler {
    let mut resource = BwResource::init_paths(&[tree.assets.as_str()]);
    resource.enable_editor_default_root();
    let resource = Rc::new(resource);

    let mut bc = BatchCompiler::new(resource.clone());
    // The shipped setters (what the options' apply pushes through): the
    // intermediate path first, the output path after it, then the cache.
    bc.compiler().set_intermediate_path(&tree.intermediate);
    bc.compiler().set_output_path(&tree.output);
    bc.compiler().set_cache_path(tree.cache.clone());
    bc.compiler()
        .register_conversion_rule(Box::new(DepRule(type_id)));
    bc.compiler().register_converter(ConverterInfo {
        name: "eval".to_string(),
        type_id,
        version: "1.0".to_string(),
        flags: ConverterFlags::DEFAULT_FLAGS,
        creator,
    });
    bc.init_compiler();
    bc
}

/// Wipe the intermediate and output trees, keep the cache.
fn wipe_build_trees(tree: &EvalTree) {
    std::fs::remove_dir_all(&tree.intermediate).expect("intermediate wiped");
    std::fs::remove_dir_all(&tree.output).expect("output wiped");
}

/// The output digests for every fixture.
fn output_digests(tree: &EvalTree) -> Vec<u64> {
    fixture_relpaths()
        .iter()
        .map(|rel| {
            compute_bytes(&std::fs::read(format!("{}/{}", tree.output, rel)).expect("output"))
        })
        .collect()
}

/// The cache entries on disk (`{basename}.deps.{hash}` per asset).
fn cache_entry_count(cache: &str) -> usize {
    fn walk(dir: &Path, count: &mut usize) {
        for entry in std::fs::read_dir(dir).expect("cache directory") {
            let entry = entry.expect("cache entry");
            if entry.file_type().expect("entry type").is_dir() {
                walk(&entry.path(), count);
            } else {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(Path::new(cache), &mut count);
    count
}

/// The cache file path the shipped formula produces for `filename`.
fn cache_filename_for(cache: &str, filename: &str, hash: u64) -> String {
    let basename = Path::new(filename).file_name().expect("basename");
    format!(
        "{}{:X}/{}.{:X}",
        cache,
        hash as u8,
        basename.to_string_lossy(),
        hash
    )
}

#[test]
fn the_cache_replays_the_dependency_list_on_a_reverted_input() {
    let tree = eval_tree("replay", PALETTE_V1);

    // --- the cold walk: every asset converts; the cache ends with three
    // entries per asset (the pre-conversion dependency list under the
    // primary-only hash, the post-conversion full list under the
    // secondary hash, and the output under its content hash)
    let started = Instant::now();
    let cold = register_session(&tree, 9, dep_converter);
    cold.build(std::slice::from_ref(&tree.assets));
    let cold_ns = started.elapsed().as_nanos();
    assert_eq!(cold.get_failed_count(), 0, "the cold walk passed");
    assert_eq!(
        CONVERTS.load(Ordering::SeqCst),
        12,
        "the cold walk converted every fixed asset once"
    );
    let cold_digests = output_digests(&tree);
    assert_eq!(
        cache_entry_count(&tree.cache),
        36,
        "three cache entries per asset after the cold walk"
    );

    // --- the flip walk: the palette changes — a fresh session re-hashes
    // it from disk, the stored v1 hash misses, the cache has no entry
    // under the new input hash, everything re-converts and the new full
    // lists land next to the old ones
    set_palette(&tree.assets, PALETTE_V2);
    let started = Instant::now();
    let flip = register_session(&tree, 9, dep_converter);
    flip.build(std::slice::from_ref(&tree.assets));
    let miss_ns = started.elapsed().as_nanos();
    assert_eq!(flip.get_failed_count(), 0, "the flip walk passed");
    assert_eq!(
        CONVERTS.load(Ordering::SeqCst),
        24,
        "the flipped palette re-converted every asset"
    );
    assert_eq!(
        cache_entry_count(&tree.cache),
        48,
        "one new entry per asset (the v2 full list; the primary list and the output hashes are unchanged, so their entries overwrite)"
    );
    let flip_digests = output_digests(&tree);
    assert_eq!(flip_digests, cold_digests, "the outputs carry the sources");

    // --- the revert walk: the palette returns to v1 — the cached full
    // list under the v1 input hash restores, validates against the
    // current hashes, and nothing re-converts
    set_palette(&tree.assets, PALETTE_V1);
    let started = Instant::now();
    let revert = register_session(&tree, 9, dep_converter);
    revert.build(std::slice::from_ref(&tree.assets));
    let hit_ns = started.elapsed().as_nanos();
    assert_eq!(revert.get_failed_count(), 0, "the revert walk passed");
    assert_eq!(
        CONVERTS.load(Ordering::SeqCst),
        24,
        "the reverted palette replayed from the cache (zero conversions)"
    );
    assert_eq!(
        output_digests(&tree),
        cold_digests,
        "the replayed walk left the outputs untouched"
    );
    assert_eq!(
        cache_entry_count(&tree.cache),
        48,
        "the replay wrote nothing new"
    );

    // --- the hot restore walk: wipe the intermediate AND output trees
    // but keep their DIRECTORY SKELETON (keep the cache) — the walk
    // rebuilds nothing: the cached primary list restores first, the
    // cached full list under the v1 input hash answers the secondary
    // check, and the cached outputs restore under their stored hashes
    wipe_build_trees(&tree);
    for sub in ["models", "textures", "ui"] {
        std::fs::create_dir_all(format!("{}/{}", tree.output, sub)).expect("output directory");
    }
    let started = Instant::now();
    let restore = register_session(&tree, 9, dep_converter);
    restore.build(std::slice::from_ref(&tree.assets));
    let restore_ns = started.elapsed().as_nanos();
    assert_eq!(restore.get_failed_count(), 0, "the hot restore walk passed");
    assert_eq!(
        CONVERTS.load(Ordering::SeqCst),
        24,
        "the hot restore walk re-converted nothing (everything restored)"
    );
    assert_eq!(
        output_digests(&tree),
        cold_digests,
        "the outputs were restored from the cache byte-identical"
    );
    assert_eq!(
        cache_entry_count(&tree.cache),
        48,
        "the restore consumed the cache without growing it"
    );

    // --- the bare-root restore walk: the same wipe WITHOUT the output
    // directories — the cache restore stage copies through a temporary
    // file inside the target file's directory, so a missing output
    // directory cannot be restored into; the FIRST task per missing
    // output subdirectory falls back to conversion (which recreates the
    // directory) and the remaining tasks in it restore from the cache.
    // Exactly one re-conversion per missing output directory.
    wipe_build_trees(&tree);
    let started = Instant::now();
    let bare = register_session(&tree, 9, dep_converter);
    bare.build(std::slice::from_ref(&tree.assets));
    let bare_ns = started.elapsed().as_nanos();
    assert_eq!(bare.get_failed_count(), 0, "the bare-root walk passed");
    assert_eq!(
        CONVERTS.load(Ordering::SeqCst),
        27,
        "one re-conversion per missing output directory (3 subdirectories)"
    );
    assert_eq!(
        output_digests(&tree),
        cold_digests,
        "the bare-root walk still reproduced every artifact"
    );
    assert_eq!(
        cache_entry_count(&tree.cache),
        48,
        "the fallback conversions overwrote their entries in place"
    );

    // --- the cache-blind contrast: the same wiped state, reads
    // disabled — the walk re-derives and re-converts everything
    wipe_build_trees(&tree);
    let started = Instant::now();
    let mut blind = register_session(&tree, 9, dep_converter);
    blind.compiler().set_cache_read(false);
    blind.build(std::slice::from_ref(&tree.assets));
    let blind_ns = started.elapsed().as_nanos();
    assert_eq!(blind.get_failed_count(), 0, "the blind walk passed");
    assert_eq!(
        CONVERTS.load(Ordering::SeqCst),
        39,
        "the cache-blind walk re-converted every asset"
    );
    assert_eq!(
        output_digests(&tree),
        cold_digests,
        "the blind walk produced the same outputs"
    );

    println!(
        "CACHE-EVAL cold_ns={cold_ns} flip_miss_ns={miss_ns} revert_hit_ns={hit_ns} hot_restore_ns={restore_ns} bare_restore_ns={bare_ns} blind_ns={blind_ns} converts_cold=12 converts_miss=12 converts_hit=0 converts_restore=0 converts_bare=3 converts_blind=12 entries=48"
    );
}

// ---------------------------------------------------------------------------
// The direct cache face and the concurrency faces (test 2).
// ---------------------------------------------------------------------------

/// The cache callbacks, counted (clones share the cells, so the test's
/// handle sees the events the cache instance fires).
#[derive(Default, Clone)]
struct Counts {
    read: Rc<Cell<usize>>,
    miss: Rc<Cell<usize>>,
    write: Rc<Cell<usize>>,
    write_miss: Rc<Cell<usize>>,
}

impl CacheEvents for Counts {
    fn on_cache_read(&self, _filename: &str) {
        self.read.set(self.read.get() + 1);
    }
    fn on_cache_read_miss(&self, _filename: &str) {
        self.miss.set(self.miss.get() + 1);
    }
    fn on_cache_write(&self, _filename: &str) {
        self.write.set(self.write.get() + 1);
    }
    fn on_cache_write_miss(&self, _filename: &str) {
        self.write_miss.set(self.write_miss.get() + 1);
    }
}

/// One direct-cache walk: write a source under its hash, restore it to
/// a same-basename path, miss on a second hash, gate both faces off.
/// Returns the events and the stored bucket path.
fn cache_walk(tag: &str) -> (Counts, String, u64) {
    let dir = TempDir::new(tag);
    let counts = Counts::default();
    std::fs::create_dir_all(dir.path().join("cache")).expect("cache root");
    let mut cache = ContentAddressableCache::new(counts.clone());
    assert!(cache.cache_path().is_empty(), "a fresh cache has no path");
    cache.set_cache_path(format!("{}/cache/", dir.path().to_string_lossy()));
    assert!(cache.read_enabled(), "reads default on");
    assert!(cache.write_enabled(), "writes default on");

    let source = dir.path().join("asset.txt");
    std::fs::write(&source, "asset v1").expect("source written");
    let hash = compute_bytes(b"asset v1");

    // the write lands the bucket file at the shipped layout
    assert!(
        cache.write_to_cache(source.to_string_lossy().as_ref(), hash),
        "the write stored the file"
    );
    let stored = cache_filename_for(
        &format!("{}/cache/", dir.path().to_string_lossy()),
        "asset.txt",
        hash,
    );
    assert!(
        Path::new(&stored).exists(),
        "the bucket file landed at the shipped layout"
    );
    assert_eq!(counts.write.get(), 1, "the write event fired");

    // a same-basename restore is served from the cache
    let restored_dir = dir.path().join("restored");
    std::fs::create_dir_all(&restored_dir).expect("restore directory");
    let restored = restored_dir.join("asset.txt");
    assert!(
        cache.read_from_cache(restored.to_string_lossy().as_ref(), hash),
        "the same basename restores"
    );
    assert_eq!(
        std::fs::read(&restored).expect("restored readable"),
        b"asset v1",
        "the restored bytes are the stored bytes"
    );
    assert_eq!(counts.read.get(), 1, "the read event fired");

    // a different basename is a miss (the cache keys on basename+hash)
    let other = dir.path().join("renamed.txt");
    assert!(
        !cache.read_from_cache(other.to_string_lossy().as_ref(), hash),
        "a different basename misses"
    );
    assert_eq!(counts.miss.get(), 1, "the miss event fired");

    // a hash with no entry misses
    let hash2 = compute_bytes(b"never stored");
    assert!(
        !cache.read_from_cache(source.to_string_lossy().as_ref(), hash2),
        "an unknown hash misses"
    );
    assert_eq!(counts.miss.get(), 2, "the second miss fired");

    // the gates close both faces without touching the disk
    cache.set_read_enabled(false);
    cache.set_write_enabled(false);
    assert!(
        !cache.read_from_cache(source.to_string_lossy().as_ref(), hash),
        "reads disabled answer false"
    );
    let extra = dir.path().join("extra.txt");
    std::fs::write(&extra, "extra").expect("extra written");
    assert!(
        !cache.write_to_cache(extra.to_string_lossy().as_ref(), hash),
        "writes disabled answer false"
    );
    assert_eq!(
        counts.read.get() + counts.miss.get(),
        3,
        "the disabled faces fire no callbacks"
    );

    (counts, stored, hash)
}

#[test]
fn the_concurrency_faces_store_their_state_and_the_cache_face_reruns() {
    // the concurrency faces: stored observables on a single-threaded
    // loop (the reference's multi-threaded variant is a dropped Windows
    // concern — see the module docs)
    let tree = eval_tree("threads", PALETTE_V1);
    let mut session = register_session(&tree, 10, plain_converter);
    assert_eq!(
        session.compiler().get_thread_count(),
        1,
        "one thread by default"
    );
    session.compiler().set_thread_count(4);
    assert_eq!(session.compiler().get_thread_count(), 4, "the count stores");
    session.compiler().set_thread_count(1);
    assert!(
        !session.compiler().get_force_rebuild(),
        "no force rebuild by default"
    );

    // a build with four threads stored runs the same single-threaded
    // loop and produces the same artifacts as a fresh one-thread walk
    session.compiler().set_thread_count(4);
    let started = Instant::now();
    session.build(std::slice::from_ref(&tree.assets));
    let four_thread_ns = started.elapsed().as_nanos();
    assert_eq!(session.get_failed_count(), 0, "the 4-store walk passed");
    let four_thread_digests = output_digests(&tree);

    let fresh_tree = eval_tree("threads-plain", PALETTE_V1);
    let fresh = register_session(&fresh_tree, 10, plain_converter);
    let started = Instant::now();
    fresh.build(std::slice::from_ref(&fresh_tree.assets));
    let one_thread_ns = started.elapsed().as_nanos();
    assert_eq!(fresh.get_failed_count(), 0, "the plain walk passed");
    assert_eq!(
        output_digests(&fresh_tree),
        four_thread_digests,
        "the stored thread count does not change the artifacts"
    );
    println!(
        "CACHE-EVAL-CONCURRENCY stored_thread_count=4 digests_equal=true four_store_ns={four_thread_ns} one_thread_ns={one_thread_ns}"
    );

    // the direct cache face reruns to the same bucket layout and events
    let (counts_a, stored_a, hash_a) = cache_walk("direct-a");
    let (counts_b, stored_b, hash_b) = cache_walk("direct-b");
    let bucket = |stored: &str| -> String {
        stored
            .split("/cache/")
            .nth(1)
            .expect("the bucket path under the cache root")
            .to_string()
    };
    assert_eq!(
        bucket(&stored_a),
        bucket(&stored_b),
        "the bucket path reruns"
    );
    assert_eq!(hash_a, hash_b, "the content hash reruns");
    for (a, b) in [
        (counts_a.read.get(), counts_b.read.get()),
        (counts_a.miss.get(), counts_b.miss.get()),
        (counts_a.write.get(), counts_b.write.get()),
        (counts_a.write_miss.get(), counts_b.write_miss.get()),
    ] {
        assert_eq!(a, b, "the event ledger reruns");
    }
    assert_eq!(counts_a.write.get(), 1, "one write per walk");
    assert_eq!(counts_a.read.get(), 1, "one served read per walk");
    assert_eq!(counts_a.miss.get(), 2, "two misses per walk");
    println!(
        "CACHE-EVAL-DIRECT bucket={} events=[read=1 miss=2 write=1 write_miss=0]",
        bucket(&stored_a)
    );
}
