//! The `scan-unit-tests` CLI, driven as a child process against a scratch
//! checkout: it writes the catalog next to it, listing only the `unit_test`
//! components with their run command and gap, and it refuses a checkout that
//! has no `programming/bigworld/Makefile.rules`.

use std::path::Path;

/// The compiled `scan-unit-tests`. `CARGO_BIN_EXE_*` is only published for
/// a package's `src/main.rs` binary, not for the `src/bin/*.rs` tools this
/// crate ships, so the artifact is found by walking up from the test
/// executable: this crate's build script places the integration-test binary
/// in `target/debug/build/test-support/<hash>/out/`, several levels below
/// the `target/debug/` directory where cargo puts the `src/bin` tools.
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-unit-tests"),
            dir.join("deps").join("scan-unit-tests"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-unit-tests not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root =
        std::env::temp_dir().join(format!("bw00-unit-tests-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("programming").join("bigworld")).expect("mkdir bigworld");
    root
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, text).expect("write");
}

/// The driver's own subdirs walker (its presence is what the CLI gates on).
const TOP_RULES: &str = "subdirs = lib server third_party tools\n";

/// A unit-test component — participates in the run.
const UNIT_TEST_COMPONENT: &str = "name := network\n\
    type := unit_test\n\
    \n\
    include $(BW_COMPONENT_FOOTER)\n";

/// A library sharing the component name — does not participate.
const LIBRARY_COMPONENT: &str = "name := network\n\
    type := library\n\
    \n\
    include $(BW_COMPONENT_FOOTER)\n";

/// A server binary — does not participate either.
const BINARY_COMPONENT: &str = "name := baseapp\n\
    type := binary\n\
    \n\
    bindir = $(BW_SERVER_BIN_DIR)\n\
    include $(BW_COMPONENT_FOOTER)\n";

#[test]
fn the_cli_lists_only_the_participating_unit_tests() {
    let root = scratch("cli");
    let bigworld = "programming/bigworld";
    write_file(&root, &format!("{bigworld}/Makefile.rules"), TOP_RULES);
    write_file(
        &root,
        &format!("{bigworld}/lib/network/unit_test/Makefile.rules"),
        UNIT_TEST_COMPONENT,
    );
    write_file(
        &root,
        &format!("{bigworld}/lib/network/Makefile.rules"),
        LIBRARY_COMPONENT,
    );
    write_file(
        &root,
        &format!("{bigworld}/server/baseapp/Makefile.rules"),
        BINARY_COMPONENT,
    );

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-unit-tests");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("1 unit-test entries"), "{stdout}");
    assert!(stdout.contains("gap: 1"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    let bw_source = root.join("programming").join("bigworld");
    assert_eq!(
        written,
        test_support::unit_tests::render(&test_support::unit_tests::scan_reference(&bw_source))
    );

    // One entry: the unit test only, carrying its command and its gap.
    let records = test_support::unit_tests::parse_catalog(&written);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.name, "network");
    assert_eq!(record.source_dir, "lib/network/unit_test");
    assert_eq!(
        record.binary,
        format!("{}/network", test_support::unit_tests::UNIT_TEST_DIR)
    );
    assert_eq!(
        record.run_command,
        "cd lib/network/unit_test; timeout 1800 \
         $(BW_INSTALL_DIR)/bin/server/$(BW_PLATFORM_CONFIG)/unit_tests/network \
         --root $(BW_ROOT)"
    );
    assert_eq!(record.result, "gap");
    assert_eq!(record.gap, test_support::unit_tests::RUN_GAP);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_makefile_rules() {
    let root =
        std::env::temp_dir().join(format!("bw00-unit-tests-cli-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-unit-tests");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld source dir"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
