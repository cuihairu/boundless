//! The `scan-cmake-targets` CLI, driven as a child process against a scratch
//! checkout: it writes the catalog next to it, recording each configuration's
//! platforms, macros, project lists and Linux commands, and it refuses a
//! checkout that has no `programming/bigworld/build/cmake` directory.

use std::path::Path;

/// The compiled `scan-cmake-targets`. `CARGO_BIN_EXE_*` is only published for
/// a package's `src/main.rs` binary, not for the `src/bin/*.rs` tools this
/// crate ships, so the artifact is found by walking up from the test
/// executable: this crate's build script places the integration-test binary in
/// `target/debug/build/test-support/<hash>/out/`, several levels below the
/// `target/debug/` directory where cargo puts the `src/bin` tools.
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-cmake-targets"),
            dir.join("deps").join("scan-cmake-targets"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-cmake-targets not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-cmake-targets-cli-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(
        root.join("programming")
            .join("bigworld")
            .join("build")
            .join("cmake"),
    )
    .expect("mkdir cmake");
    root
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, text).expect("write");
}

/// A synthetic configuration exercising every field the catalog records.
const SAMPLE_CONFIG: &str = "ADD_DEFINITIONS( -DMF_SERVER )\n\
    ADD_DEFINITIONS( -DMF_CONFIG=\"Hybrid\" )\n\
    SET( BW_PCH_SUPPORT OFF )\n\
    SET( BW_LIBRARY_PROJECTS\n\
    \tnetwork\t\tlib/network\n\
    \tresmgr\t\tlib/resmgr\n\
    \t)\n\
    SET( BW_BINARY_PROJECTS\n\
    \tcellapp\t\tserver/cellapp\n\
    \tbaseapp\t\tserver/baseapp\n\
    \t)\n\
    IF( BW_UNIT_TESTS_ENABLED )\n\
    \tSET( BW_SERVER_UNIT_TEST_LIBRARIES\n\
    \t\tunit_test_lib\t\tlib/unit_test_lib\n\
    \t\t)\n\
    ENDIF()\n\
    BW_TARGET_LINUX_COMMAND( start_server\n\
    \t\"python ctl start\" )\n";

#[test]
fn the_cli_writes_the_catalog_for_each_configuration() {
    let root = scratch("cli");
    write_file(
        &root,
        "programming/bigworld/build/cmake/BWConfiguration_sample.cmake",
        SAMPLE_CONFIG,
    );

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-cmake-targets");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("1 CMake configurations"), "{stdout}");
    assert!(stdout.contains("sample"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    let bw_source = root.join("programming").join("bigworld");
    assert_eq!(
        written,
        test_support::cmake_targets::render(&test_support::cmake_targets::scan_reference(
            &bw_source
        ))
    );

    // The record carries the platforms, the macros and the project lists.
    let records = test_support::cmake_targets::parse_catalog(&written);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.name, "sample");
    assert_eq!(record.platforms, vec!["win32", "win64", "linux"]);
    assert_eq!(record.defines, vec!["MF_SERVER", "MF_CONFIG=\"Hybrid\""]);
    assert_eq!(record.flags, vec!["BW_PCH_SUPPORT=OFF"]);
    assert_eq!(
        record.libraries,
        vec!["network@lib/network", "resmgr@lib/resmgr"]
    );
    assert_eq!(
        record.binaries,
        vec!["cellapp@server/cellapp", "baseapp@server/baseapp"]
    );
    assert_eq!(record.unit_tests, "on");
    assert_eq!(record.linux_commands, vec!["start_server:python ctl start"]);
    assert_eq!(record.conditions, vec!["BW_UNIT_TESTS_ENABLED"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_a_cmake_config_dir() {
    let root = std::env::temp_dir().join(format!(
        "bw00-cmake-targets-cli-bare-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-cmake-targets");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("not a BigWorld CMake config dir"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
