//! The `scan-coverage` CLI, driven as a child process against a scratch
//! checkout: it writes the ledger next to it, one row per Make target with
//! its enable condition derived from the reference's gate machinery, and it
//! refuses a checkout that has no `programming/bigworld/Makefile.rules`.

use std::path::Path;

/// The compiled `scan-coverage`. `CARGO_BIN_EXE_*` is only published for a
/// package's `src/main.rs` binary, not for the `src/bin/*.rs` tools this
/// crate ships, so the artifact is found by walking up from the test
/// executable: this crate's build script places the integration-test binary
/// in `target/debug/build/test-support/<hash>/out/`, several levels below
/// the `target/debug/` directory where cargo puts the `src/bin` tools.
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-coverage"),
            dir.join("deps").join("scan-coverage"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-coverage not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("bw00-coverage-cli-{}-{tag}", std::process::id()));
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

/// A plain server component — builds unconditionally.
const ALWAYS_COMPONENT: &str = "name := baseapp\n\
    type := binary\n\
    bindir = $(BW_SERVER_BIN_DIR)\n\
    include $(BW_COMPONENT_FOOTER)\n";

/// A component under `examples/` — gated by `user_shouldBuildExamples != 0`.
const EXAMPLES_COMPONENT: &str = "name := cellapp_example\n\
    type := shared_library\n\
    include $(BW_COMPONENT_FOOTER)\n";

/// The cellappmgr component — gated by its own Makefile's presence.
const CELLAPPMGR_COMPONENT: &str = "name := cellappmgr\n\
    type := binary\n\
    bindir = $(BW_SERVER_BIN_DIR)\n\
    include $(BW_COMPONENT_FOOTER)\n";

/// A component wrapping its registration in the MySQL gate — gated by the
/// MySQL development libraries being present.
const MYSQL_COMPONENT: &str = "ifneq ($(SYSTEM_HAS_MYSQL),0)\n\
    \n\
    $(warning MySQL development libraries not installed. Skipping sync_db build.)\n\
    \n\
    else\n\
    \n\
    name := sync_db\n\
    type := binary\n\
    bindir = $(BW_COMMAND_BIN_DIR)\n\
    include $(BW_COMPONENT_FOOTER)\n\
    \n\
    endif\n";

#[test]
fn the_cli_writes_one_row_per_target_with_its_enable_condition() {
    let root = scratch("cli");
    let bigworld = "programming/bigworld";
    write_file(&root, &format!("{bigworld}/Makefile.rules"), TOP_RULES);
    write_file(
        &root,
        &format!("{bigworld}/server/baseapp/Makefile.rules"),
        ALWAYS_COMPONENT,
    );
    write_file(
        &root,
        &format!("{bigworld}/examples/cellapp_extension/Makefile.rules"),
        EXAMPLES_COMPONENT,
    );
    write_file(
        &root,
        &format!("{bigworld}/server/cellappmgr/Makefile.rules"),
        CELLAPPMGR_COMPONENT,
    );
    write_file(
        &root,
        &format!("{bigworld}/server/tools/sync_db/Makefile.rules"),
        MYSQL_COMPONENT,
    );
    // The lane ledger the CLI indexes against: one lane claiming baseapp.
    let todo = root.join("todo.md");
    write_file(
        &root,
        "todo.md",
        "### BW-99 假车道\n\n源码 `server/baseapp/`、`lib/network/`。\n\n\
         - [ ] 占位任务\n",
    );

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .env("BOUNDLESS_TODO", &todo)
        .output()
        .expect("run scan-coverage");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("4 coverage rows"), "{stdout}");
    assert!(stdout.contains("always: 1"), "{stdout}");
    assert!(
        stdout.contains("user_shouldBuildExamples != 0: 1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("cellappmgr/Makefile present: 1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("MySQL development libraries present: 1"),
        "{stdout}"
    );

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    let bw_source = root.join("programming").join("bigworld");
    assert_eq!(
        written,
        test_support::coverage::render(&test_support::coverage::scan_reference(&bw_source, &todo))
    );

    // One row per target, each carrying its enable condition and the unmapped
    // mapping fields.
    let records = test_support::coverage::parse_catalog(&written);
    assert_eq!(records.len(), 4);
    let by_target: std::collections::BTreeMap<_, _> =
        records.iter().map(|r| (r.target.as_str(), r)).collect();
    assert_eq!(
        by_target["baseapp"].enable,
        test_support::coverage::ENABLE_ALWAYS
    );
    assert_eq!(
        by_target["cellapp_example"].enable,
        test_support::coverage::ENABLE_EXAMPLES
    );
    assert_eq!(
        by_target["cellappmgr"].enable,
        test_support::coverage::ENABLE_CELLAPPMGR
    );
    assert_eq!(
        by_target["sync_db"].enable,
        test_support::coverage::ENABLE_MYSQL
    );
    for record in &records {
        assert_eq!(record.fixture, test_support::coverage::STATUS_UNMAPPED);
        assert_eq!(record.status, test_support::coverage::STATUS_UNMAPPED);
    }
    // The lane that claims a row indexes it; rows no lane in this scratch
    // ledger claims, and rows outside the lane's scope, stay unmapped.
    assert_eq!(by_target["baseapp"].task, "BW-99");
    assert_eq!(
        by_target["cellapp_example"].task,
        test_support::coverage::STATUS_UNMAPPED
    );
    assert_eq!(
        by_target["cellappmgr"].task,
        test_support::coverage::STATUS_UNMAPPED
    );
    assert_eq!(
        by_target["sync_db"].task,
        test_support::coverage::STATUS_UNMAPPED
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_makefile_rules() {
    let root = std::env::temp_dir().join(format!("bw00-coverage-cli-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-coverage");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld source dir"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
