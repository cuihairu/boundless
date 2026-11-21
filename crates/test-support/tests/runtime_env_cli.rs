//! The `scan-runtime-env` CLI, driven as a child process against a scratch
//! checkout: it writes the runtime-environment catalog next to it, recording
//! the engine/Python/DB versions, the C++ standard, the build configurations
//! and whether prebuilt server binaries exist, and it refuses a checkout that
//! has no `programming/bigworld/lib/cstdmf/bwversion.hpp`.

use std::path::Path;

/// The compiled `scan-runtime-env`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-runtime-env"),
            dir.join("deps").join("scan-runtime-env"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-runtime-env not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root =
        std::env::temp_dir().join(format!("bw00-runtime-env-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, text).expect("write");
}

/// A minimal BigWorld source tree whose runtime facts are all deterministic
/// (the scratch root is not a git repository, so `reference-commit` is `-`).
fn make_source_tree(root: &Path) {
    write_file(
        root,
        "programming/bigworld/lib/cstdmf/bwversion.hpp",
        "#define BW_VERSION_MAJOR 14\n#define BW_VERSION_MINOR 4\n#define BW_VERSION_PATCH 1\n",
    );
    write_file(
        root,
        "programming/bigworld/build/make/third_party_python.mak",
        "BW_PYTHON_VERSION := 3.13\n",
    );
    write_file(
        root,
        "programming/bigworld/lib/db_storage_mysql/versions.hpp",
        "const uint32\tDBAPP_CURRENT_VERSION = DBAPP_VERSION_LOGON_MAPPING_2;\n",
    );
    write_file(
        root,
        "programming/bigworld/build/make/platform_el7.mak",
        "CXX11_CXXFLAGS = -std=c++23 -Wall\n",
    );
}

#[test]
fn the_cli_writes_the_catalog_for_the_reference() {
    let root = scratch("cli");
    make_source_tree(&root);

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-runtime-env");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("8 runtime facts"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    assert_eq!(
        written,
        test_support::runtime_env::render(&test_support::runtime_env::scan_reference(&root))
    );

    let records = test_support::runtime_env::parse_catalog(&written);
    let get = |item: &str| {
        records
            .iter()
            .find(|f| f.item == item)
            .unwrap_or_else(|| panic!("{item} row missing"))
    };
    assert_eq!(get("reference-commit").value, "-");
    assert_eq!(get("engine-version").value, "14.4.1");
    assert_eq!(get("bundled-python").value, "3.13");
    assert_eq!(get("db-backend").value, "MySQL");
    assert_eq!(
        get("db-schema-version").value,
        "DBAPP_VERSION_LOGON_MAPPING_2"
    );
    assert_eq!(get("cxx-standard").value, "-std=c++23");
    assert_eq!(
        get("prebuilt-server-binaries").value,
        "none (source-only checkout)"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_bwversion() {
    let root = scratch("bare");
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-runtime-env");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld checkout"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
