//! The `scan-startup-baseline` CLI, driven as a child process against a
//! scratch checkout: it writes the standalone-cluster startup-baseline catalog
//! next to it, recording the topology (eight components and their built
//! binaries), the config chain (fantasydemo `bw.xml` with the full defaults
//! absent) and the frozen startup-attempt log, and it refuses a checkout that
//! has no `programming/fantasydemo/res/server/bw.xml`.

use std::path::Path;

/// The compiled `scan-startup-baseline`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-startup-baseline"),
            dir.join("deps").join("scan-startup-baseline"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-startup-baseline not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-startup-baseline-cli-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, text).expect("write");
}

/// A minimal BigWorld source tree whose baseline facts are all deterministic
/// (the scratch root is not a git repository, so `reference-commit` is `-`,
/// and the full defaults are absent).
fn make_source_tree(root: &Path) {
    write_file(
        root,
        "programming/fantasydemo/res/server/bw.xml",
        "<root>\n\t<personality>\tfantasydemo\t</personality>\n\t<baseApp>\n\t\t<archivePeriod>\t0\t</archivePeriod>\n\t</baseApp>\n\t<billingSystem>\n\t\t<shouldAcceptUnknownUsers>\ttrue\t</shouldAcceptUnknownUsers>\n\t\t<entityTypeForUnknownUsers>\tAvatar\t</entityTypeForUnknownUsers>\n\t</billingSystem>\n</root>\n",
    );
    // One built binary, so the topology records its size.
    write_file(root, "game/bin/server/el7/server/baseapp", &"x".repeat(100));
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
        .expect("run scan-startup-baseline");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("8 components"), "{stdout}");
    assert!(stdout.contains("12 log lines"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    assert_eq!(
        written,
        test_support::startup_baseline::render(&test_support::startup_baseline::scan_reference(
            &root
        ))
    );

    let records = test_support::startup_baseline::parse_catalog(&written);
    let get = |item: &str| {
        records
            .iter()
            .find(|f| f.item == item)
            .unwrap_or_else(|| panic!("{item} row missing"))
    };

    // Topology: the eight components, the built baseapp's size, the absent
    // binaries recorded as `-`.
    assert_eq!(get("reference-commit").value, "-");
    assert_eq!(get("topology.1.role").value, "baseapp");
    assert_eq!(
        get("topology.1.source-dir").value,
        "programming/bigworld/server/baseapp"
    );
    assert_eq!(
        get("topology.1.binary").value,
        "game/bin/server/el7/server/baseapp"
    );
    assert_eq!(get("topology.1.bytes").value, "100");
    assert_eq!(get("topology.8.role").value, "reviver");
    assert_eq!(get("topology.8.bytes").value, "-");

    // Config: the fantasydemo chain, with the full defaults absent.
    assert_eq!(get("config.personality").value, "fantasydemo");
    assert_eq!(get("config.baseApp.archivePeriod").value, "0");
    assert_eq!(
        get("config.billingSystem.entityTypeForUnknownUsers").value,
        "Avatar"
    );
    assert!(
        get("config.development-defaults-xml")
            .value
            .contains("ABSENT")
    );

    // The frozen startup log is retained (the committed artifact, not the
    // synthetic tree).
    assert_eq!(
        get("log.1").value,
        test_support::startup_baseline::log_lines()[0]
    );
    assert!(get("log.1").value.contains("Added res path"));

    // The named blocker is recorded.
    assert!(get("blocker.standalone-start").value.contains("bwmachined"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_bw_xml() {
    let root = scratch("bare");
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-startup-baseline");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld checkout"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
