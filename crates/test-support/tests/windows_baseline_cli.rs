//! The `scan-windows-baseline` CLI, driven as a child process against a
//! scratch checkout: it writes the Windows client/tool baseline catalog next
//! to it, recording per-target result-or-gap (no Windows binary built), the
//! runner capability and the named blocker, and it refuses a checkout that has
//! no `programming/bigworld/tools`.

use std::path::Path;

/// The compiled `scan-windows-baseline`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-windows-baseline"),
            dir.join("deps").join("scan-windows-baseline"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-windows-baseline not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-windows-baseline-cli-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn mkdir(root: &Path, rel: &str) {
    std::fs::create_dir_all(root.join(rel)).expect("mkdir");
}

/// A minimal BigWorld source tree: a client source dir, a couple of tool
/// source dirs, and one Linux-built tool binary.
fn make_source_tree(root: &Path) {
    mkdir(root, "programming/bigworld/client");
    mkdir(root, "programming/bigworld/tools/res_packer");
    mkdir(root, "programming/bigworld/tools/modeleditor");
    mkdir(root, "programming/bigworld/tools/common");
    mkdir(root, "game/bin/server/el7/tools");
    std::fs::write(root.join("game/bin/server/el7/tools/res_packer"), "elf").expect("write");
}

#[test]
fn the_cli_writes_the_baseline_catalog() {
    let root = scratch("cli");
    make_source_tree(&root);

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-windows-baseline");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("Windows targets"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    assert_eq!(
        written,
        test_support::windows_baseline::render(&test_support::windows_baseline::scan_reference(
            &root
        ))
    );

    let records = test_support::windows_baseline::parse_catalog(&written);
    let get = |item: &str| {
        records
            .iter()
            .find(|f| f.item == item)
            .unwrap_or_else(|| panic!("{item} row missing"))
    };

    // The scratch root is not a git repository, so the commit is `-`.
    assert_eq!(get("reference-commit").value, "-");
    assert_eq!(get("runner.os").value, "linux");
    assert!(get("windows-binary-dirs").value.contains("absent"));
    assert!(get("blocker.windows-baseline").value.contains("wine"));

    // Per target: the client and the tools, each a gap, with source present.
    let targets = test_support::windows_baseline::targets();
    assert!(targets.iter().any(|t| t.name == "client"));
    let res_packer = targets
        .iter()
        .find(|t| t.name == "res_packer")
        .expect("res_packer");
    assert_eq!(res_packer.result, "GAP");
    assert!(res_packer.linux_binary.contains("res_packer"));
    assert!(targets.iter().all(|t| t.result == "GAP"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_tools() {
    let root = scratch("bare");
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-windows-baseline");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld checkout"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
