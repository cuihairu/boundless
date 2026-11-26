//! OPT-13-01-01: record the cold-build, incremental and editor-load
//! baselines over the fixed asset set (`fixtures/baseline`).
//!
//! The fixed assets (12 nested `.txt` sources) drive the shipped batch
//! pipeline through its three baseline legs: a **cold build** (empty
//! output tree, every asset converted, a `{source}.deps` sidecar per
//! source in the intermediate tree), an **incremental build** (a fresh
//! pipeline session over the same tree — every task answers up-to-date
//! from the on-disk dependency lists and file hashes, zero conversions,
//! byte-identical outputs), and an **editor load** (a fresh
//! `BwResource` opened over the output tree reads every artifact back
//! through the multi-file-system face, bytes equal to the source). A
//! second independent cold build reruns to identical digests — the
//! artifacts are reproducible.
//!
//! The incremental leg registers a fresh session deliberately: the
//! shipped `queue_task` only queues tasks whose status is `New`, so a
//! second `build` on the same session re-queues nothing and would
//! exercise no mechanism at all. A fresh session re-hashes every
//! source, finds each task's `.deps` list and outputs intact, and
//! answers up-to-date through the real dependency checks.
//!
//! Wall-clocks are printed (`BASELINE` lines) as probe data only; the
//! contracts are asserted.
//!
//! ```sh
//! cargo test -p asset-pipeline --test build_baselines
//! ```

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use asset_pipeline::batch_compiler::BatchCompiler;
use asset_pipeline::compiler::Compiler;
use asset_pipeline::conversion_rule::ConversionRule;
use asset_pipeline::conversion_task::ConversionTask;
use asset_pipeline::converter::Converter;
use asset_pipeline::converter_info::{ConverterCreator, ConverterFlags, ConverterInfo};
use asset_pipeline::dependency_list::DependencyList;
use asset_pipeline::hash64::compute_bytes;
use resources::bwresource::BwResource;
use resources::zip::FileType;

// ---------------------------------------------------------------------------
// The scoped temp dir (the channel-cost tests' pattern).
// ---------------------------------------------------------------------------

static WALKS: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ap-baseline-{name}-{}-{}",
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
// The fixed asset set: fixtures/baseline copied into a fresh source tree.
// ---------------------------------------------------------------------------

/// The committed fixture root (12 nested `.txt` assets).
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/baseline");

/// One fixture's relative path (the fixed asset set, sorted).
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

/// Copy the committed fixtures into a fresh `{root}/assets` tree.
fn plant_assets(root: &Path) -> String {
    let assets = root.join("assets");
    for rel in fixture_relpaths() {
        let source = Path::new(FIXTURES).join(&rel);
        let target = assets.join(&rel);
        std::fs::create_dir_all(target.parent().expect("parent")).expect("asset directory");
        std::fs::copy(&source, &target).expect("asset copied");
    }
    assets.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// The pipeline seam: a copy converter (the shipped test converter's
// shape), one counting and one plain registration.
// ---------------------------------------------------------------------------

/// The counting converter's convert calls (test 1 owns this counter).
static CONVERTS: AtomicUsize = AtomicUsize::new(0);

/// A rule that claims `.txt` source files as root tasks for `type_id`.
struct BaselineRule(u64);

impl ConversionRule for BaselineRule {
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

/// A converter that copies the source to the resolved output path.
struct CopyConverter;

impl Converter for CopyConverter {
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

/// The counting registration's converter: the copy converter plus a
/// convert-call bump (the incremental leg's observable).
struct CountingConverter;

impl Converter for CountingConverter {
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
        intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        CONVERTS.fetch_add(1, Ordering::SeqCst);
        CopyConverter.convert(source_file, compiler, intermediate_files, output_files)
    }
}

fn plain_converter(_params: &str) -> Box<dyn Converter> {
    Box::new(CopyConverter)
}

fn counting_converter(_params: &str) -> Box<dyn Converter> {
    Box::new(CountingConverter)
}

/// One build tree: the fixed assets planted and the persistent
/// `out/`/`intermediate/` roots named. Each walk registers a fresh
/// pipeline session over the tree — the shipped `queue_task` only
/// queues tasks whose status is `New`, so a second build on the same
/// session would re-queue nothing.
struct BuildTree {
    _dir: TempDir,
    assets: String,
    output: String,
    intermediate: String,
    type_id: u64,
}

fn build_tree(tag: &str, type_id: u64) -> BuildTree {
    let dir = TempDir::new(tag);
    let assets = plant_assets(dir.path());
    let output = format!("{}/out/", dir.path().to_string_lossy());
    let intermediate = format!("{}/intermediate/", dir.path().to_string_lossy());
    BuildTree {
        _dir: dir,
        assets,
        output,
        intermediate,
        type_id,
    }
}

/// A fresh pipeline session over the tree: the pipeline registered
/// (rule + converter + paths), `init_compiler` done.
fn register_session(tree: &BuildTree, creator: ConverterCreator) -> BatchCompiler {
    let mut resource = BwResource::init_paths(&[tree.assets.as_str()]);
    resource.enable_editor_default_root();
    let resource = Rc::new(resource);

    let mut bc = BatchCompiler::new(resource.clone());
    // The shipped setters (what the options' apply pushes through): the
    // intermediate path first, the output path after it.
    bc.compiler().set_intermediate_path(&tree.intermediate);
    bc.compiler().set_output_path(&tree.output);
    bc.compiler()
        .register_conversion_rule(Box::new(BaselineRule(tree.type_id)));
    bc.compiler().register_converter(ConverterInfo {
        name: "baseline".to_string(),
        type_id: tree.type_id,
        version: "1.0".to_string(),
        flags: ConverterFlags::DEFAULT_FLAGS,
        creator,
    });
    bc.init_compiler();
    bc
}

/// The output path for a fixture's relative path.
fn output_for(output: &str, rel: &str) -> String {
    format!("{output}{rel}")
}

/// The intermediate `.deps` sidecar path for a fixture.
fn deps_for(intermediate: &str, rel: &str) -> String {
    format!("{intermediate}{rel}.deps")
}

/// Walk the ledger: per fixture, the source bytes, the output bytes and
/// the output digest.
fn ledger(tree: &BuildTree) -> Vec<(String, Vec<u8>, Vec<u8>, u64)> {
    fixture_relpaths()
        .into_iter()
        .map(|rel| {
            let source = std::fs::read(format!("{}/{}", tree.assets, rel)).expect("source");
            let output = std::fs::read(output_for(&tree.output, &rel)).expect("output");
            let digest = compute_bytes(&output);
            (rel, source, output, digest)
        })
        .collect()
}

#[test]
fn the_fixed_asset_baseline_records_cold_incremental_and_editor_load() {
    let tree = build_tree("walk", 7);

    // --- the cold build: every asset converts, every source grows a
    // .deps sidecar, the outputs land in the output tree
    let started = Instant::now();
    let cold = register_session(&tree, counting_converter);
    cold.build(std::slice::from_ref(&tree.assets));
    let cold_ns = started.elapsed().as_nanos();
    assert_eq!(cold.get_failed_count(), 0, "the cold build passed");
    assert_eq!(
        CONVERTS.load(Ordering::SeqCst),
        12,
        "the cold build converted every fixed asset once"
    );
    let cold = ledger(&tree);
    assert_eq!(cold.len(), 12, "the ledger carries every fixture");
    for (rel, source, output, _) in &cold {
        assert_eq!(output, source, "{rel}: the output carries the source bytes");
        assert!(
            std::path::Path::new(&deps_for(&tree.intermediate, rel)).exists(),
            "{rel}: the .deps sidecar landed in the intermediate tree"
        );
    }

    // --- the incremental build: a fresh session over the same tree —
    // every task answers up-to-date from the on-disk dependency lists
    // and file hashes, nothing converts, the outputs hold
    let started = Instant::now();
    let incremental = register_session(&tree, counting_converter);
    incremental.build(std::slice::from_ref(&tree.assets));
    let incremental_ns = started.elapsed().as_nanos();
    assert_eq!(
        incremental.get_failed_count(),
        0,
        "the incremental build passed"
    );
    assert_eq!(
        CONVERTS.load(Ordering::SeqCst),
        12,
        "the incremental build converted nothing (all tasks up-to-date)"
    );
    let incremental = ledger(&tree);
    let cold_digests: Vec<u64> = cold.iter().map(|(_, _, _, d)| *d).collect();
    let incremental_digests: Vec<u64> = incremental.iter().map(|(_, _, _, d)| *d).collect();
    assert_eq!(
        incremental_digests, cold_digests,
        "the incremental build left the outputs byte-identical"
    );

    // --- the editor load: a fresh resource opened over the output tree
    // reads every artifact back through the multi-file-system face
    let mut load_resource = BwResource::init_paths(&[tree.output.as_str()]);
    load_resource.enable_editor_default_root();
    let started = Instant::now();
    let mut loaded = Vec::new();
    for (rel, source, _, _) in &cold {
        assert!(
            load_resource.file_exists(rel),
            "{rel}: the editor's resource face finds the artifact"
        );
        assert_eq!(
            load_resource.get_file_type(rel, None),
            FileType::File,
            "{rel}: the artifact loads as a file"
        );
        let bytes = load_resource
            .file_system()
            .read_file(rel)
            .unwrap_or_else(|| panic!("{rel}: the artifact reads"));
        assert_eq!(&bytes, source, "{rel}: the loaded bytes are the source");
        loaded.push(compute_bytes(&bytes));
    }
    let editor_load_ns = started.elapsed().as_nanos();
    assert_eq!(
        loaded, cold_digests,
        "the editor load answers the cold build's digests"
    );

    let bytes: usize = cold.iter().map(|(_, _, o, _)| o.len()).sum();
    println!(
        "BASELINE assets=12 source_bytes={bytes} cold_ns={cold_ns} incremental_ns={incremental_ns} editor_load_ns={editor_load_ns} converts=12"
    );
}

#[test]
fn the_cold_build_reruns_to_identical_artifacts() {
    let tree_a = build_tree("rerun-a", 8);
    let a = register_session(&tree_a, plain_converter);
    a.build(std::slice::from_ref(&tree_a.assets));
    assert_eq!(a.get_failed_count(), 0, "rerun a passed");

    let tree_b = build_tree("rerun-b", 8);
    let b = register_session(&tree_b, plain_converter);
    b.build(std::slice::from_ref(&tree_b.assets));
    assert_eq!(b.get_failed_count(), 0, "rerun b passed");

    let ledger_a = ledger(&tree_a);
    let ledger_b = ledger(&tree_b);
    assert_eq!(ledger_a.len(), 12, "both reruns carry every fixture");
    for ((rel_a, _, out_a, dig_a), (rel_b, _, out_b, dig_b)) in ledger_a.iter().zip(ledger_b.iter())
    {
        assert_eq!(rel_a, rel_b, "the reruns walk the same fixture set");
        assert_eq!(out_a, out_b, "{rel_a}: the artifact reruns byte-identical");
        assert_eq!(dig_a, dig_b, "{rel_a}: the digest reruns");
    }
    println!("BASELINE-RERUN files=12 digests_equal=true");
}
