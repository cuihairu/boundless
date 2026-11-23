//! The `scan-release-artifacts` CLI, driven as a child process against a
//! scratch checkout: it writes the catalog next to it, recording each release
//! script, the engine/bundled-python versions and the platform tags, and it
//! refuses a checkout that has no `programming/bigworld/build/bw_internal/scripts`
//! directory.

use std::path::Path;

/// The compiled `scan-release-artifacts`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-release-artifacts"),
            dir.join("deps").join("scan-release-artifacts"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-release-artifacts not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-release-artifacts-cli-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let bw = root.join("programming").join("bigworld");
    std::fs::create_dir_all(bw.join("build").join("bw_internal").join("scripts")).expect("mkdir");
    std::fs::create_dir_all(bw.join("build").join("make")).expect("mkdir make");
    std::fs::create_dir_all(bw.join("lib").join("cstdmf")).expect("mkdir cstdmf");
    root
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, text).expect("write");
}

#[test]
fn the_cli_writes_the_catalog_for_each_release_artifact() {
    let root = scratch("cli");
    write_file(
        &root,
        "programming/bigworld/build/bw_internal/scripts/makeRPM.py",
        "#!/usr/bin/env python\n\
         # Build the server RPM.\n\
         build_common.runCmd( \"make rpm\" )\n",
    );
    write_file(
        &root,
        "programming/bigworld/lib/cstdmf/bwversion.hpp",
        "#define BW_VERSION_MAJOR 14\n\
         #define BW_VERSION_MINOR 4\n\
         #define BW_VERSION_PATCH 1\n\
         #define BW_COPYRIGHT_NOTICE \"(c) 1999 - 2014, BigWorld Pty. Ltd. All Rights Reserved\"\n",
    );
    write_file(
        &root,
        "programming/bigworld/build/make/third_party_python.mak",
        "BW_PYTHON_VERSION := 3.13\n",
    );
    write_file(
        &root,
        "programming/bigworld/build/make/platform_info.py",
        "#!/usr/bin/env python\n\
         # Distribution     Expected output\n\
         # CentOS 7         el7\n\
         # Fedora 18        fedora18\n\
         import os\n",
    );

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-release-artifacts");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("6 release artifacts"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    let bw_source = root.join("programming").join("bigworld");
    assert_eq!(
        written,
        test_support::release_artifacts::render(&test_support::release_artifacts::scan_reference(
            &bw_source
        ))
    );

    let records = test_support::release_artifacts::parse_catalog(&written);
    let version = records
        .iter()
        .find(|a| a.artifact == "engine-version")
        .expect("engine version");
    assert_eq!(version.version, "14.4.1");
    let script = records
        .iter()
        .find(|a| a.artifact == "makeRPM.py")
        .expect("makeRPM.py");
    assert!(script.deliverables.contains("rpm"));
    let platform = records
        .iter()
        .find(|a| a.artifact == "platform-tags")
        .expect("platform tags");
    assert_eq!(platform.version, "CentOS 7:el7,Fedora 18:fedora18");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_a_release_scripts_dir() {
    let root = std::env::temp_dir().join(format!(
        "bw00-release-artifacts-cli-bare-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-release-artifacts");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("not a BigWorld release-scripts dir"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
