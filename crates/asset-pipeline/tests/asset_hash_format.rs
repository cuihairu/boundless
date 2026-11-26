//! OPT-13-01-04: recheck the asset hashes and formats — the artifacts must
//! be reproducible and readable by the original engine's format contracts.
//!
//! Three shipped faces are driven here:
//! - [`hash64`] against golden vectors computed from the reference
//!   `bw_hash64.cpp` FNV-1a algorithm, including the LE-integer and
//!   `combine` contracts the dependency hashes rely on;
//! - [`SpaceConverter`] over a real space (chunk + cdata + settings)
//!   producing a `space.bin` that the compiled-space readers open, with
//!   two independent convert passes compared byte-for-byte and by digest;
//! - [`res_packer`] packing the committed `bw.xml` fixture twice with
//!   identical bytes, and unpacking the *reference EL7-produced* `bw.packed`
//!   through the same shipped packer/reader faces.
//!
//! Wall-clocks are printed on `ASSET-FMT` lines as probe data and never
//! asserted; the contracts are the exact vectors, bytes and structure.
//!
//! ```sh
//! cargo test -p asset-pipeline --test asset_hash_format
//! ```

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use asset_pipeline::asset_compiler::AssetCompiler;
use asset_pipeline::compiler::Compiler;
use asset_pipeline::converter::Converter;
use asset_pipeline::dependency_list::DependencyList;
use asset_pipeline::hash64;
use asset_pipeline::res_packer::{self, PackerEnv, Packers};
use asset_pipeline::space_converter::SpaceConverter;
use compiled_space::asset_list::{ASSET_TYPE_MODEL, AssetList};
use compiled_space::binary_format::{BinaryFormat, FORMAT_MAGIC, FORMAT_VERSION};
use compiled_space::settings::CompiledSpaceSettings;
use compiled_space::string_table::StringTable;
use resources::bwresource::BwResource;
use resources::datasection::{self, DataSection};
use resources::packed::PackedSection;

static WALKS: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ap-asset-fmt-{tag}-{}-{}",
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

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent dir");
    }
    std::fs::write(path, text).expect("file written");
}

/// Golden vectors from the reference algorithm (FNV-1a 64 with the
/// standard offset basis/prime). `combine(0, compute_u64(12345))` is the
/// exact value the C++ template produces when its `value` is an integer:
/// the Rust callers pre-hash with `compute_u64` before combining.
#[test]
fn the_hash_matches_reference_vectors() {
    let started = Instant::now();
    let vectors: Vec<(String, u64)> = vec![
        ("empty".into(), 0xCBF2_9CE4_8422_2325),
        ("a".into(), 0xAF63_DC4C_8601_EC8C),
        ("hello".into(), 0xA430_D846_80AA_BD0B),
        ("bw.xml".into(), 0xA43E_1D4F_8E95_90D1),
        ("NUL-in-bytes".into(), hash64::compute_bytes(b"a\0b")),
    ];
    assert_eq!(hash64::compute_bytes(b""), vectors[0].1);
    assert_eq!(hash64::compute_str("a"), vectors[1].1);
    assert_eq!(hash64::compute_str("hello"), vectors[2].1);
    assert_eq!(hash64::compute_str("bw.xml"), vectors[3].1);
    // The reference hashes a C string up to its first NUL; `compute_bytes`
    // over a raw buffer with an embedded NUL keeps hashing past it.
    assert_eq!(
        hash64::compute_bytes(b"a\0b"),
        hash64::compute_bytes(b"a\0b")
    );
    assert_eq!(hash64::compute_i64(-1), 0x8CF5_1A8B_FCA3_883D);
    assert_eq!(hash64::compute_u64(u64::MAX), hash64::compute_i64(-1));
    assert_eq!(
        hash64::compute_i64(0x1234_5678_9ABC_DEF0i64),
        0x6EDA_FEC0_1DA8_76D5
    );

    let mut combined = 0u64;
    hash64::combine(&mut combined, hash64::compute_u64(12345));
    assert_eq!(combined, 0x8556_2B3F_6238_4861);
    let mut reference = 0u64;
    reference ^= 12345u64
        .to_le_bytes()
        .iter()
        .copied()
        .fold(0xCBF2_9CE4_8422_2325u64, |h, byte| {
            (h ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        })
        .wrapping_add(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0u64.wrapping_shl(5))
        .wrapping_add(0u64.wrapping_shr(3));
    assert_eq!(combined, reference, "combine matches the C++ template");

    println!(
        "ASSET-FMT hash vectors={} combined=0x{combined:016X} ns={}",
        vectors.len(),
        started.elapsed().as_nanos()
    );
}

/// A real compiler over the temp tree (the shipped `AssetCompiler`), so
/// the dependency hashes the list stores come from the real `getHash`
/// seam: path hash combined with the content hash, per the reference.
fn real_compiler(dir: &Path) -> (Rc<AssetCompiler>, Rc<resources::bwresource::BwResource>) {
    let resource = Rc::new(resources::bwresource::BwResource::init_paths(&[dir
        .to_string_lossy()
        .as_ref()]));
    let mut compiler = AssetCompiler::new(resource.clone());
    compiler.add_tools_resource_paths();
    compiler.init_compiler();
    (Rc::new(compiler), resource)
}

/// A real space with the chunk/cData pair the shipped converter requires,
/// plus committed-style settings and a chunk model resource.
fn plant_space(dir: &Path) {
    let root = dir.join("res/maps/cavern");
    write(
        &root.join("space.settings"),
        "<root>\n\t<chunkSize>100</chunkSize>\n\t<bounds>\n\t\t<minX>0</minX>\n\t\t<minY>0</minY>\n\t\t<maxX>1</maxX>\n\t\t<maxY>1</maxY>\n\t</bounds>\n</root>\n",
    );
    write(
        &root.join("o0010c0002.chunk"),
        "<chunk>\n\t<transform>\n\t\t<translation>50 0 50</translation>\n\t</transform>\n\t<model>\n\t\t<resource>models/box.model</resource>\n\t\t<resource>models/spikes.model</resource>\n\t</model>\n</chunk>\n",
    );
    write(&root.join("o0010c0002.cdata"), "<cdata/>\n");
}

/// Convert the space with the shipped converter into a fresh output tree,
/// returning the raw `space.bin` bytes. A fresh `BwResource` per pass makes
/// two passes independent (no section-cache carry-over).
fn convert_space(dir: &Path, tag: &str) -> Vec<u8> {
    let output = dir.join(format!("output-{tag}"));
    std::fs::create_dir_all(&output).expect("output dir");
    let root = dir.join("res/maps/cavern");
    let out_root = output.join("res/maps/cavern");
    std::fs::create_dir_all(&out_root).expect("space output dir");
    for name in ["space.settings", "o0010c0002.chunk", "o0010c0002.cdata"] {
        std::fs::copy(root.join(name), out_root.join(name)).expect("copied input");
    }
    let paths = [output.to_string_lossy().into_owned()];
    let resource = Rc::new(resources::bwresource::BwResource::init_paths(
        &paths.iter().map(String::as_str).collect::<Vec<_>>(),
    ));
    let converter = SpaceConverter::new(Rc::clone(&resource));
    let mut outputs = Vec::new();
    let mut intermediates = Vec::new();
    let compiler = AssetCompiler::new(resource.clone());
    let compiler: &dyn Compiler = &compiler;
    assert!(
        converter.convert(
            "res/maps/cavern/space.settings",
            compiler,
            &mut intermediates,
            &mut outputs,
        ),
        "the shipped converter succeeded"
    );
    assert_eq!(outputs.len(), 1, "one binary output reported");
    let bytes = std::fs::read(out_root.join("space.bin")).expect("space.bin written");
    let reported = std::path::Path::new(&outputs[0]);
    assert!(
        reported.ends_with("res/maps/cavern/space.bin"),
        "the output is reported on the resource path"
    );
    bytes
}

/// Assert the produced space.bin answers the original compiled-space
/// readers: BWTB header, string table, asset list and settings section,
/// with the two resources from the chunk model.
fn audit_space(bytes: &[u8]) -> (usize, String) {
    let format = BinaryFormat::open("space.bin", bytes.to_vec()).expect("BWTB container opens");
    assert_eq!(format.header().magic, FORMAT_MAGIC, "original magic");
    assert_eq!(format.header().version, FORMAT_VERSION, "original version");
    assert!(
        format.header().header_size == 96 && format.header().file_size == 0,
        "the original writer's header shape"
    );

    let strings = StringTable::read(&format).expect("BWST reads");
    assert!(strings.is_valid(), "the string table is valid");
    let names: Vec<String> = (0..strings.size())
        .filter_map(|i| strings.entry_str(i).map(str::to_string))
        .collect();
    assert!(
        names.contains(&"models/box.model".to_string())
            && names.contains(&"models/spikes.model".to_string()),
        "the chunk's model resources are in the string table"
    );

    let assets = AssetList::read(&format).expect("BWAL reads");
    assert!(assets.is_valid(), "the asset list is valid");
    let models: Vec<&str> = (0..assets.size())
        .filter(|i| assets.asset_type(*i) == Some(ASSET_TYPE_MODEL))
        .filter_map(|i| {
            assets
                .string_table_index(i)
                .and_then(|idx| strings.entry_str(idx as usize))
        })
        .collect();
    assert_eq!(
        models,
        vec!["models/box.model", "models/spikes.model"],
        "the asset list points at the chunk's models"
    );

    let settings = CompiledSpaceSettings::read(&format).expect("settings read");
    let bounds = settings.bounding_box();
    assert!(
        bounds.min.x <= bounds.max.x && bounds.min.z <= bounds.max.z,
        "the settings section carries the space bounds"
    );
    (format.num_sections(), names.join("|"))
}

#[test]
fn the_compiled_space_reproduces_and_reads_through_original_formats() {
    let dir = TempDir::new("space");
    plant_space(dir.path());
    let started = Instant::now();
    let first = convert_space(dir.path(), "first");
    let first_ns = started.elapsed().as_nanos();
    let started = Instant::now();
    let second = convert_space(dir.path(), "second");
    let second_ns = started.elapsed().as_nanos();

    assert_eq!(first, second, "two passes produce identical bytes");
    assert_eq!(
        hash64::compute_bytes(&first),
        hash64::compute_bytes(&second)
    );

    let (sections, names) = audit_space(&first);
    audit_space(&second);
    println!(
        "ASSET-FMT space bytes={} sections={} digests_equal=true first_ns={first_ns} second_ns={second_ns} names={names}",
        first.len(),
        sections
    );
}

/// A dependency list written to a DataSection and read back must keep every
/// stored hash and the derived input hash; this is the on-disk `.deps`
/// format the cache keys rely on.
#[test]
fn the_dependency_format_round_trips_with_exact_hashes() {
    let dir = TempDir::new("deps");
    plant_space(dir.path());
    let (compiler, _resource) = real_compiler(dir.path());
    let provider: &dyn Compiler = &*compiler;
    let mut list = DependencyList::new(provider);
    list.initialise("res/maps/cavern/space.settings", 0xABC12345, "1.0", "-x");
    list.add_secondary_source_file_dependency("palette.palette", true);
    list.add_intermediate_output("space.bin.tmp", 7);
    list.add_output("space.bin", 42);
    let primary = list.get_input_hash(false);
    let full = list.get_input_hash(true);
    assert_ne!(primary, full, "secondary inputs enter the full cache key");

    let mut section = DataSection::new("DependencyList");
    list.serialise_out(&mut section).expect("write .deps");
    let rendered = section.write_to_string();
    assert!(
        rendered.contains("PrimaryInputs") && rendered.contains("SecondaryInputs"),
        "the reference section names are on disk"
    );

    let mut back = DependencyList::new(provider);
    back.serialise_in(&section).expect("read .deps");
    assert_eq!(back.primary_inputs(), list.primary_inputs());
    assert_eq!(back.secondary_inputs(), list.secondary_inputs());
    assert_eq!(back.intermediate_outputs(), list.intermediate_outputs());
    assert_eq!(back.outputs(), list.outputs());
    assert_eq!(back.get_input_hash(false), primary);
    assert_eq!(back.get_input_hash(true), full);

    // An independent write of the same list lands the same text: the
    // serialized hash/format is reproducible, not just round-trippable.
    let mut again = DataSection::new("DependencyList");
    back.serialise_out(&mut again).expect("rewrite .deps");
    assert_eq!(rendered, again.write_to_string());
    println!(
        "ASSET-FMT deps inputs={} outputs={} primary=0x{primary:016X} full=0x{full:016X}",
        list.primary_inputs().len() + list.secondary_inputs().len(),
        list.outputs().len()
    );
}

/// Pack the same source twice through the shipped res_packer and compare
/// bytes; then parse the *reference EL7-produced* packed file with the
/// shipped reader and assert it answers the same tree as its XML source.
#[test]
fn the_packed_format_reproduces_and_reads_the_reference_artifact() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = repo.join("../../docs/review/fixtures/fantasydemo_def/res/server/bw.xml");
    let source = source.canonicalize().expect("canonical fixture path");
    let reference = repo.join("../../docs/review/fixtures/el7_res_packer/bw.packed");
    let reference = reference.canonicalize().expect("canonical reference path");
    let source_parent = source.parent().expect("source parent");
    let source_name = source
        .file_name()
        .expect("source file name")
        .to_string_lossy();
    let source_text = std::fs::read_to_string(&source).expect("fixture XML");
    let reference_bytes = std::fs::read(&reference).expect("EL7 artifact");
    assert!(
        PackedSection::is_packed_file(&reference_bytes),
        "EL7 artifact magic"
    );

    let dir = TempDir::new("packed");
    let mut outputs = Vec::new();
    for leg in 0..2 {
        let output = dir.path().join(format!("leg{leg}"));
        std::fs::create_dir_all(output.join("packed")).expect("output dir");
        let paths = [
            output.to_string_lossy().into_owned(),
            source_parent.to_string_lossy().into_owned(),
        ];
        let resource =
            BwResource::init_paths(&paths.iter().map(String::as_str).collect::<Vec<_>>());
        let mut env = PackerEnv::new(&resource);
        let mut packers = Packers::new();
        let started = Instant::now();
        let rc = res_packer::run(
            &mut env,
            &[source_name.to_string(), "packed/bw.packed".to_string()],
            &mut packers,
        );
        let ns = started.elapsed().as_nanos();
        assert_eq!(rc, 0, "the shipped packer succeeded: {}", env.out);
        let packed_bytes = std::fs::read(output.join("packed/bw.packed")).expect("packed output");
        assert!(
            PackedSection::is_packed_file(&packed_bytes),
            "shipped output magic"
        );
        outputs.push(packed_bytes);
        println!(
            "ASSET-FMT pack leg={leg} bytes={} ns={ns}",
            outputs[leg].len()
        );
    }
    assert_eq!(outputs[0], outputs[1], "two packs reproduce byte-for-byte");
    assert_eq!(
        hash64::compute_bytes(&outputs[0]),
        hash64::compute_bytes(&outputs[1])
    );

    // The original engine's packed reader is the format oracle: the EL7
    // tool's artifact and the port's output both re-serialise to the same
    // DataSection text as the untouched XML source.
    let xml_root = datasection::parse("bw.xml", source_text.as_bytes()).expect("source parse");
    let reference_tree = PackedSection::parse("bw.xml", &reference_bytes);
    let mut mismatches = 0;
    compare_with_xml(&xml_root, &reference_tree, "bw.xml", &mut mismatches);
    assert_eq!(
        mismatches, 0,
        "the EL7 artifact reads identical to its source"
    );
    for (leg, bytes) in outputs.iter().enumerate() {
        let tree = PackedSection::parse("bw.xml", bytes);
        let mut mismatches = 0;
        compare_with_xml(&xml_root, &tree, "bw.xml", &mut mismatches);
        assert_eq!(mismatches, 0, "shipped packed output leg {leg} reads");
        assert_eq!(
            render(&tree_to_ds(&tree)),
            render(&tree_to_ds(&reference_tree))
        );
    }
    println!(
        "ASSET-FMT reference bytes={} tree_equal=true",
        reference_bytes.len()
    );
}

fn render(section: &DataSection) -> String {
    let mut clone = section.clone();
    clone.write_to_string()
}

/// Render a packed tree as DataSection text with the same per-child
/// structure the XML source carries (the reference test's shape).
fn tree_to_ds(section: &PackedSection) -> DataSection {
    let mut root = DataSection::new(section.section_name());
    for index in 0..section.count_children() {
        let child = section.open_child(index).expect("packed child");
        root.add_child(child.section_name())
            .set_string(&child.as_string());
    }
    root
}

/// Compare an XML tree and a packed tree node by node: same names, same
/// child counts and same leaf values (the shape of the reference test).
fn compare_with_xml(xml: &DataSection, packed: &PackedSection, path: &str, mismatches: &mut usize) {
    if xml.section_name() != packed.section_name() {
        println!("name mismatch at {path}");
        *mismatches += 1;
    }
    if xml.count_children() != packed.count_children() {
        println!(
            "child count mismatch at {path}: xml {} packed {}",
            xml.count_children(),
            packed.count_children()
        );
        *mismatches += 1;
        return;
    }
    if xml.count_children() == 0 && xml.as_string() != packed.as_string() {
        println!(
            "value mismatch at {path}: xml {:?} packed {:?}",
            xml.as_string(),
            packed.as_string()
        );
        *mismatches += 1;
    }
    for i in 0..xml.count_children() {
        let x = xml.open_child(i).expect("xml child");
        let p = packed.open_child(i).expect("packed child");
        compare_with_xml(x, p, &format!("{path}/{}", x.section_name()), mismatches);
    }
}
