//! The `scan-asset-fixtures` CLI, driven as a child process against a
//! scratch checkout: it writes the resource/entity/database fixture catalog,
//! recording each fixture's input path, byte size and content digest, and it
//! refuses a checkout that has no tracked fixture tree.

use std::path::Path;

/// The compiled `scan-asset-fixtures`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-asset-fixtures"),
            dir.join("deps").join("scan-asset-fixtures"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-asset-fixtures not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-asset-fixtures-cli-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

/// A minimal BigWorld source tree with one small fixture tree: a resource
/// config, a database fixture, one entity def and one entity script.
fn make_source_tree(root: &Path) {
    write(
        root,
        "programming/fantasydemo/res/server/bw.xml",
        "<root>\n\t<personality>\tfantasydemo\t</personality>\n</root>\n",
    );
    write(
        root,
        "programming/fantasydemo/res/scripts/entities.xml",
        "<root>\n\t<ClientServerEntities>\n\t\t<Guard/>\n\t\t<Avatar/>\n\t</ClientServerEntities>\n</root>\n",
    );
    write(
        root,
        "programming/fantasydemo/res/scripts/db.xml",
        "<db.xml>\n\t<_BigWorldInfo>\n\t\t<LogOnMapping>\n\t\t\t<item>\n\t\t\t\t<logOnName>\tG\t</logOnName>\n\t\t\t</item>\n\t\t</LogOnMapping>\n\t</_BigWorldInfo>\n\t<Guard>\n\t\t<name>\tG\t</name>\n\t</Guard>\n</db.xml>\n",
    );
    write(
        root,
        "programming/fantasydemo/res/scripts/entity_defs/Guard.def",
        "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t</Properties>\n\t<ClientMethods>\n\t\t<say>\n\t\t\t<Arg>\tSTRING\t</Arg>\n\t\t</say>\n\t</ClientMethods>\n\t<BaseMethods>\n\t</BaseMethods>\n</root>\n",
    );
    write(
        root,
        "programming/fantasydemo/res/scripts/base/Guard.py",
        "class Guard:\n\tpass\n",
    );
    write(
        root,
        "programming/fantasydemo/res/scripts/client/Guard.py",
        "class Guard:\n\tpass\n",
    );
}

#[test]
fn the_cli_writes_the_asset_catalog() {
    let root = scratch("cli");
    make_source_tree(&root);

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-asset-fixtures");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("6 files under fantasydemo"), "{stdout}");
    assert!(stdout.contains("1 entities"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    assert_eq!(
        written,
        test_support::asset_fixtures::render(&test_support::asset_fixtures::scan_reference(&root))
    );

    let records = test_support::asset_fixtures::parse_catalog(&written);
    let get = |item: &str| {
        records
            .iter()
            .find(|f| f.item == item)
            .unwrap_or_else(|| panic!("{item} row missing"))
    };

    // The scratch root is not a git repository, so the commit is `-`.
    assert_eq!(get("reference-commit").value, "-");

    // The tree and its per-file rows.
    assert_eq!(get("tree.fantasydemo.files").value, "6");
    assert_eq!(get("tree.fantasydemo.entities").value, "Guard,Avatar");
    assert_eq!(get("tree.fantasydemo.db-logons").value, "1");
    assert_eq!(get("tree.fantasydemo.db-rowtypes").value, "Guard");

    // The database fixture is classified database; the file rows carry the
    // input path and a full content digest.
    assert_eq!(get("file.fantasydemo.3.kind").value, "database");
    assert_eq!(
        get("file.fantasydemo.3.input").value,
        "programming/fantasydemo/res/scripts/db.xml"
    );
    assert_eq!(get("file.fantasydemo.3.sha256").value.len(), 64);
    assert_eq!(get("file.fantasydemo.6.kind").value, "resource");
    assert_eq!(get("file.fantasydemo.5.kind").value, "entity");

    // The entity def semantics.
    assert_eq!(get("entity.fantasydemo.Guard.properties").value, "1");
    assert_eq!(get("entity.fantasydemo.Guard.clientmethods").value, "1");
    assert_eq!(get("entity.fantasydemo.Guard.basemethods").value, "0");
    assert_eq!(get("entity.fantasydemo.Guard.exposed").value, "0");

    // The blocker is recorded.
    assert!(
        get("blocker.live-asset-capture")
            .value
            .contains("live-asset-capture")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_fixture_trees() {
    let root = scratch("bare");
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-asset-fixtures");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld checkout"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
