//! The `scan-make-targets` CLI, driven as a child process against a scratch
//! checkout: it writes the catalog next to it, recording each component's
//! name, type and owning rule, and it refuses a checkout that has no
//! `programming/bigworld/Makefile.rules`.

use std::path::Path;

/// The compiled `scan-make-targets`. `CARGO_BIN_EXE_*` is only published for
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
            dir.join("scan-make-targets"),
            dir.join("deps").join("scan-make-targets"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-make-targets not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-make-targets-cli-{}-{tag}",
        std::process::id()
    ));
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

/// A standard binary component, laid out like the real files.
const BINARY_COMPONENT: &str = "name := baseapp\n\
    type := binary\n\
    \n\
    bindir = $(BW_SERVER_BIN_DIR)\n\
    \n\
    include $(BW_COMPONENT_FOOTER)\n\
    \n\
    name :=\n\
    type :=\n";

/// A component living in a directory named `build` (the real lib/build).
const NESTED_BUILD_LIBRARY: &str = "name := build\n\
    type := library\n\
    \n\
    include $(BW_COMPONENT_FOOTER)\n\
    \n\
    name :=\n";

/// The self-ruled component: no type, its own `BW_BINS +=` line.
const SELF_RULED: &str = "name := remove_db\n\
    \n\
    .PHONY: $(name)\n\
    $(name): $(destFile)\n\
    \n\
    BW_BINS += $(name)\n\
    \n\
    name :=\n";

/// The shared rules dir holds bare resets, not registrations.
const SHARED_FOOTER: &str = "name :=\n\
    type :=\n";

#[test]
fn the_cli_writes_the_catalog_with_the_owning_rules() {
    let root = scratch("cli");
    let bigworld = "programming/bigworld";
    write_file(&root, &format!("{bigworld}/Makefile.rules"), TOP_RULES);
    write_file(
        &root,
        &format!("{bigworld}/server/baseapp/Makefile.rules"),
        BINARY_COMPONENT,
    );
    write_file(
        &root,
        &format!("{bigworld}/lib/build/Makefile.rules"),
        NESTED_BUILD_LIBRARY,
    );
    write_file(
        &root,
        &format!("{bigworld}/server/tools/remove_db/Makefile.rules"),
        SELF_RULED,
    );
    // The top-level build/ directory is shared rules; its bare `name :=`
    // reset must not register a target.
    write_file(
        &root,
        &format!("{bigworld}/build/make/common_footer_config.mak"),
        SHARED_FOOTER,
    );

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-make-targets");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("3 Make targets"), "{stdout}");
    assert!(stdout.contains("binary: 1"), "{stdout}");
    assert!(stdout.contains("library: 1"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    let bw_source = root.join("programming").join("bigworld");
    assert_eq!(
        written,
        test_support::make_targets::render(&test_support::make_targets::scan_reference(&bw_source))
    );

    // The records carry the name, the type, the source and the owning rule.
    let records = test_support::make_targets::parse_catalog(&written);
    assert_eq!(records.len(), 3);
    let baseapp = records
        .iter()
        .find(|t| t.name == "baseapp")
        .expect("baseapp record");
    assert_eq!(baseapp.kind, "binary");
    assert_eq!(baseapp.source, "server/baseapp/Makefile.rules");
    assert_eq!(baseapp.rule, test_support::make_targets::FOOTER_TARGET_RULE);
    assert_eq!(
        baseapp.rule_file,
        test_support::make_targets::FOOTER_RULE_FILE
    );
    assert_eq!(baseapp.dest, "$(BW_SERVER_BIN_DIR)");
    let build = records.iter().find(|t| t.name == "build").expect("build");
    assert_eq!(build.kind, "library");
    assert_eq!(build.source, "lib/build/Makefile.rules");
    let remove_db = records
        .iter()
        .find(|t| t.name == "remove_db")
        .expect("remove_db");
    assert_eq!(remove_db.kind, "-");
    assert_eq!(remove_db.rule, test_support::make_targets::SELF_RULE);
    assert_eq!(remove_db.rule_file, "server/tools/remove_db/Makefile.rules");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_makefile_rules() {
    let root =
        std::env::temp_dir().join(format!("bw00-make-targets-cli-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-make-targets");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld source dir"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
