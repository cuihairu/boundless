//! The `scan-protocol-fixtures` CLI, driven as a child process against a
//! scratch checkout: it writes the protocol binary fixture catalog next to
//! it, recording each interface's messages with ids, direction and raw frame
//! bytes, and it refuses a checkout that has no interface header.

use std::path::Path;

/// The compiled `scan-protocol-fixtures`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-protocol-fixtures"),
            dir.join("deps").join("scan-protocol-fixtures"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-protocol-fixtures not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-protocol-fixtures-cli-{}-{tag}",
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

/// A minimal BigWorld source tree with one small header per curated
/// interface.
fn make_source_tree(root: &Path) {
    write(
        root,
        "programming/bigworld/server/baseapp/baseapp_int_interface.hpp",
        "BEGIN_MERCURY_INTERFACE( BaseAppIntInterface )\n\
         BW_STREAM_MSG_EX( BaseApp, createBaseWithCellData )\n\
         BW_BEGIN_STRUCT_MSG( BaseApp, startup )\n\
         \tbool\tbootstrap;\n\
         END_STRUCT_MESSAGE()\n\
         END_MERCURY_INTERFACE()\n",
    );
    write(
        root,
        "programming/bigworld/lib/connection/baseapp_ext_interface.hpp",
        "BEGIN_MERCURY_INTERFACE( BaseAppExtInterface )\n\
         MF_BEGIN_BLOCKABLE_PROXY_MSG( baseAppLogin )\n\
         END_STRUCT_MESSAGE()\n\
         END_MERCURY_INTERFACE()\n",
    );
    write(
        root,
        "programming/bigworld/server/cellapp/cellapp_interface.hpp",
        "BEGIN_MERCURY_INTERFACE( CellAppInterface )\n\
         MF_VARLEN_ENTITY_MSG( requestEntityUpdate, WITNESS_ONLY )\n\
         END_MERCURY_INTERFACE()\n",
    );
    write(
        root,
        "programming/bigworld/server/baseappmgr/baseappmgr_interface.hpp",
        "BEGIN_MERCURY_INTERFACE( BaseAppMgrInterface )\n\
         BW_EMPTY_MSG( BaseAppMgr, ping )\n\
         END_MERCURY_INTERFACE()\n",
    );
}

#[test]
fn the_cli_writes_the_protocol_catalog() {
    let root = scratch("cli");
    make_source_tree(&root);

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-protocol-fixtures");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("4 interfaces"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    assert_eq!(
        written,
        test_support::protocol_fixtures::render(&test_support::protocol_fixtures::scan_reference(
            &root
        ))
    );

    let records = test_support::protocol_fixtures::parse_catalog(&written);
    let get = |item: &str| {
        records
            .iter()
            .find(|f| f.item == item)
            .unwrap_or_else(|| panic!("{item} row missing"))
    };

    // The scratch root is not a git repository, so the commit is `-`.
    assert_eq!(get("reference-commit").value, "-");

    // The first interface: two messages in declaration order, with ids and
    // frames.
    assert_eq!(get("interface.1.name").value, "BaseAppIntInterface");
    assert_eq!(get("interface.1.direction").value, "internal");
    assert_eq!(get("interface.1.messages").value, "2");
    assert_eq!(get("msg.1.1.name").value, "createBaseWithCellData");
    assert_eq!(get("msg.1.1.id").value, "0");
    assert_eq!(get("msg.1.1.frame").value, "000000");
    assert_eq!(get("msg.1.2.name").value, "startup");
    assert_eq!(get("msg.1.2.id").value, "1");
    assert_eq!(get("msg.1.2.frame").value, "01");
    assert_eq!(get("msg.1.2.length-style").value, "fixed");

    // The external interface is direction external.
    assert_eq!(get("interface.2.name").value, "BaseAppExtInterface");
    assert_eq!(get("interface.2.direction").value, "external");

    // The blocker is recorded.
    assert!(
        get("blocker.protocol-capture")
            .value
            .contains("protocol-capture")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_interfaces() {
    let root = scratch("bare");
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-protocol-fixtures");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld checkout"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
