//! OPT-16-01-03: re-run of the load and install paths.
//!
//! The acceptance: **external interfaces and artifacts stay compatible**
//! (`外部接口与产物兼容`). One fixture pair drives both directions:
//!
//! - **Install** — the tool face (`asset_pipeline::res_packer`) packs
//!   the fixture `server/bw.xml` through `BwResource`; the artifact must
//!   be byte-identical to the reference EL7 `res_packer` output
//!   (`el7_res_packer/bw.packed`, recorded by
//!   `scripts/el7-interop/run-el7-res-packer.sh`).
//! - **Load** — fresh readers walk both artifacts back against the XML
//!   source: every node name, child order and leaf value must match,
//!   plus the typed spot-checks (personality, billing system).
//! - **Re-install idempotence** — a packed artifact re-packs to
//!   identical bytes (`canPack` is true for packed input).
//! - **Interface faces** — `copy_file`/`file_exists` and the
//!   message-exact failure strings (`ERROR: ` lines, the error flag).

use asset_pipeline::res_packer::{self, PackerEnv};
use resources::bwresource::BwResource;
use resources::datasection::{self, DataSection};
use resources::packed::PackedSection;
use std::io::Write as _;
use std::path::PathBuf;

fn fixtures() -> std::path::PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/review/fixtures")
}

/// The record sink (the OPT family's shape).
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }
}

/// The XML source both artifacts come from.
fn xml_source() -> Vec<u8> {
    std::fs::read(fixtures().join("fantasydemo_def/res/server/bw.xml"))
        .expect("the xml source fixture is in the repo")
}

/// The reference EL7 `res_packer` artifact.
fn el7_artifact() -> Vec<u8> {
    std::fs::read(fixtures().join("el7_res_packer/bw.packed"))
        .expect("the EL7 fixture is in the repo")
}

/// Total node count of an XML tree (itself plus descendants).
fn count_nodes(section: &DataSection) -> usize {
    let mut total = 1;
    for i in 0..section.count_children() {
        total += count_nodes(section.open_child(i).expect("xml child"));
    }
    total
}

/// Deep-compares an XML tree with a packed tree: node names, child
/// order, leaf values. Returns (mismatches, leaves compared).
fn compare_tree(
    xml: &DataSection,
    packed: &PackedSection,
    path: &str,
    mismatches: &mut usize,
    leaves: &mut usize,
) {
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
    if xml.count_children() == 0 {
        *leaves += 1;
        if xml.as_string() != packed.as_string() {
            println!(
                "value mismatch at {path}: xml {:?} packed {:?}",
                xml.as_string(),
                packed.as_string()
            );
            *mismatches += 1;
        }
    }
    for i in 0..xml.count_children() {
        let x = xml.open_child(i).expect("xml child");
        let p = packed.open_child(i).expect("packed child");
        compare_tree(
            x,
            p,
            &format!("{path}/{}", x.section_name()),
            mismatches,
            leaves,
        );
    }
}

/// The scratch install root: `res/server/bw.xml` on disk.
fn install_root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("opt16-install-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("res/server")).expect("scratch root");
    std::fs::write(dir.join("res/server/bw.xml"), xml_source()).expect("bw.xml");
    dir
}

/// The load-and-install recheck.
#[test]
fn the_load_and_install_paths_stay_compatible() {
    let mut record = Record::default();

    // ---- Install: the tool face packs the fixture through the
    // resource entry, with a clean message log.
    let dir = install_root("compat");
    let root_str = dir.to_str().expect("utf8 root");
    let resource = BwResource::init_paths(&[root_str]);
    let mut env = PackerEnv::new(&resource);
    assert!(res_packer::convert(
        &mut env,
        "res/server/bw.xml",
        "res/server/bw.packed",
        &[],
        false,
        0
    ));
    assert!(!env.errors_occurred);
    assert!(env.out.is_empty(), "a clean install says nothing");
    let ours = std::fs::read(dir.join("res/server/bw.packed")).expect("packed output");
    record.put("install_ok", 1);
    record.put("our_artifact_bytes", ours.len());

    // ---- Artifact compatibility: byte-identical to the reference EL7
    // tool's output for the same input.
    let el7 = el7_artifact();
    assert_eq!(
        ours, el7,
        "our packed artifact is byte-identical to the EL7 tool's"
    );
    record.put("el7_artifact_bytes", el7.len());
    record.put("artifacts_byte_identical", 1);

    // ---- Load: fresh readers walk both artifacts against the XML
    // source. The XML tree shape first.
    let xml = datasection::parse("bw.xml", &xml_source()).expect("the xml source parses");
    let nodes = count_nodes(&xml);

    let mut mismatches = 0;
    let mut leaves = 0;
    let ours_packed = PackedSection::parse("bw.xml", &ours);
    compare_tree(&xml, &ours_packed, "ours", &mut mismatches, &mut leaves);
    assert_eq!(mismatches, 0, "our artifact reads back identical");
    let el7_packed = PackedSection::parse("bw.xml", &el7);
    let mut el7_mismatches = 0;
    let mut el7_leaves = 0;
    compare_tree(
        &xml,
        &el7_packed,
        "el7",
        &mut el7_mismatches,
        &mut el7_leaves,
    );
    assert_eq!(el7_mismatches, 0, "the EL7 artifact reads back identical");
    assert_eq!(leaves, el7_leaves);
    record.put("tree_nodes", nodes);
    record.put("leaf_values_compared", leaves);
    record.put("tree_mismatches", mismatches);

    // Typed spot-checks through the packed reader (the values the
    // app face consumes).
    assert_eq!(
        ours_packed
            .find_child("personality")
            .expect("personality")
            .as_string(),
        "fantasydemo"
    );
    let billing = ours_packed.find_child("billingSystem").expect("billing");
    assert!(
        billing
            .find_child("shouldAcceptUnknownUsers")
            .expect("accept")
            .as_bool(false),
        "unknown users are accepted"
    );
    assert_eq!(
        billing
            .find_child("entityTypeForUnknownUsers")
            .expect("type")
            .as_string(),
        "Avatar"
    );
    assert_eq!(
        ours_packed
            .find_child("baseApp")
            .expect("baseApp")
            .find_child("archivePeriod")
            .expect("archive")
            .as_int(-1),
        0
    );
    record.put("typed_spot_checks", 4);

    // ---- Re-install idempotence: packed input re-packs to the same
    // bytes (packed sections can pack; strips need unpacked input).
    let repacked =
        res_packer::convert_bytes(&mut env, &ours, &[], false, 0).expect("re-pack succeeds");
    assert_eq!(repacked, ours, "re-packing the artifact is a no-op");
    record.put("repack_identical", 1);

    // ---- Interface faces: copy and existence, then the message-exact
    // failure strings.
    assert!(res_packer::copy_file(
        &mut env,
        "res/server/bw.xml",
        "res/server/copied.xml",
        false
    ));
    assert!(res_packer::file_exists(&env, "res/server/copied.xml"));
    assert!(!res_packer::file_exists(&env, "res/server/missing.xml"));
    assert!(!res_packer::copy_file(
        &mut env,
        "res/server/missing.xml",
        "res/server/out.xml",
        false
    ));
    assert!(
        env.out
            .contains("Error: Cannot open source file res/server/missing.xml."),
        "the copy failure message is exact: {:?}",
        env.out
    );
    assert!(!res_packer::convert(
        &mut env,
        "res/server/missing.xml",
        "res/server/missing.packed",
        &[],
        false,
        0
    ));
    assert!(
        env.out.contains(
            "ERROR: PackedSection::convert: Failed to open data section \
             res/server/missing.xml"
        ),
        "the convert failure message is exact: {:?}",
        env.out
    );
    assert!(env.errors_occurred, "the ERROR line sets the error flag");
    // `remove_trailing_slash`: one slash, either separator, no more.
    assert_eq!(
        res_packer::remove_trailing_slash("res/server/"),
        "res/server"
    );
    assert_eq!(
        res_packer::remove_trailing_slash("res/server\\"),
        "res/server"
    );
    assert_eq!(
        res_packer::remove_trailing_slash("res//"),
        "res/",
        "only one trailing slash comes off"
    );
    assert_eq!(res_packer::remove_trailing_slash("res"), "res");
    record.put("interface_faces_ok", 1);
    record.put("error_messages_exact", 2);

    let _ = std::fs::remove_dir_all(&dir);

    let mut text = String::new();
    for (k, v) in &record.0 {
        text.push_str(&format!("{k}\t{v}\n"));
    }
    print!("{text}");
    if let Ok(path) = std::env::var("INSTALL_RECHECK_OUT") {
        std::fs::write(&path, &text).expect("write INSTALL_RECHECK_OUT");
        println!("record: {path}");
    }
    let _ = std::io::stdout().flush();
}
