//! The `scan-third-party-deps` CLI, driven as a child process against a
//! scratch checkout: it writes the catalog next to it, recording each vendored
//! dependency's license, NOTICE flag and reuse decision, and it refuses a
//! checkout that has no `programming/bigworld/third_party` directory.

use std::path::Path;

/// The compiled `scan-third-party-deps`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-third-party-deps"),
            dir.join("deps").join("scan-third-party-deps"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-third-party-deps not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-third-party-deps-cli-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let bw = root.join("programming").join("bigworld");
    std::fs::create_dir_all(bw.join("third_party")).expect("mkdir");
    root
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, text).expect("write");
}

#[test]
fn the_cli_writes_the_catalog_for_each_dependency() {
    let root = scratch("cli");
    // A source tree with a build manifest and an explicit license.
    write_file(
        &root,
        "programming/bigworld/third_party/curl/CMakeLists.txt",
        "project(curl)\n",
    );
    write_file(
        &root,
        "programming/bigworld/third_party/curl/COPYING",
        "COPYRIGHT AND PERMISSION NOTICE\nPermission to use, copy, modify,\n",
    );
    // A precompiled distribution: headers plus prebuilt binaries, no manifest.
    write_file(
        &root,
        "programming/bigworld/third_party/putty/plink.exe",
        "",
    );
    // A vendored archive at the top of the tree.
    write_file(
        &root,
        "programming/bigworld/third_party/mongodb/mongo-cxx-driver-legacy.tar.gz",
        "",
    );
    // A NOTICE-bearing tree with no license text (identified by the module's
    // curated fallback is covered by the module test; here the notice flags).
    write_file(
        &root,
        "programming/bigworld/third_party/openssl_android/NOTICE",
        "notice",
    );

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-third-party-deps");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("4 third party dependencies"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    let bw_source = root.join("programming").join("bigworld");
    assert_eq!(
        written,
        test_support::third_party_deps::render(&test_support::third_party_deps::scan_reference(
            &bw_source
        ))
    );

    let records = test_support::third_party_deps::parse_catalog(&written);
    let curl = records.iter().find(|d| d.dep == "curl").expect("curl row");
    assert_eq!(curl.license, "MIT-style (curl COPYING)");
    assert_eq!(curl.reuse_decision, "vendored-source");
    assert_eq!(curl.notice, "no");
    let putty = records
        .iter()
        .find(|d| d.dep == "putty")
        .expect("putty row");
    assert_eq!(putty.reuse_decision, "precompiled");
    let mongodb = records
        .iter()
        .find(|d| d.dep == "mongodb")
        .expect("mongodb row");
    assert_eq!(mongodb.reuse_decision, "vendored-archive");
    let openssl = records
        .iter()
        .find(|d| d.dep == "openssl_android")
        .expect("openssl_android row");
    assert_eq!(openssl.notice, "yes");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_a_third_party_dir() {
    let root = std::env::temp_dir().join(format!(
        "bw00-third-party-deps-cli-bare-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-third-party-deps");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("not a BigWorld third-party dir"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
