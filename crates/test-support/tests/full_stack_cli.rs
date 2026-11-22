//! The `run-full-stack` CLI, driven as a child process against a scratch
//! original binary dir and a fake new stack binary: it writes two evidence
//! tables (`original.tsv` and `new.tsv`) for one shared input, records the
//! named blocker on the original side and the real startup output on the new
//! side, and refuses a missing input or a missing new binary.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The compiled `run-full-stack`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn harness() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("run-full-stack"),
            dir.join("deps").join("run-full-stack"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled run-full-stack not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root =
        std::env::temp_dir().join(format!("bw00-full-stack-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn write_file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, text).expect("write");
}

#[test]
fn the_cli_writes_two_evidence_files_for_one_input() {
    let root = scratch("cli");
    // The shared input: three reference roles.
    write_file(&root, "input/roles.txt", "baseapp\nloginapp\nreviver\n");
    // A scratch original binary dir with two of the three binaries present.
    write_file(&root, "orig/bin/baseapp", "baseapp-fake");
    write_file(&root, "orig/bin/loginapp", "loginapp-fake");
    // A fake new stack binary that prints a startup line and exits cleanly.
    let new_bin = root.join("new/boundless-server");
    write_file(
        &root,
        "new/boundless-server",
        "#!/bin/sh\necho \"[$1] starting (test entry)\" >&2\nexit 0\n",
    );
    let mut perms = std::fs::metadata(&new_bin).expect("new meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&new_bin, perms).expect("chmod new");
    let out_dir = root.join("out");

    let result = std::process::Command::new(harness())
        .args([
            root.join("input/roles.txt").to_str().expect("input"),
            root.join("orig/bin").to_str().expect("orig"),
            new_bin.to_str().expect("new"),
            out_dir.to_str().expect("out"),
        ])
        .output()
        .expect("run run-full-stack");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("3 roles probed"), "{stdout}");
    assert!(stdout.contains("started cleanly for 3/3"), "{stdout}");

    // Two evidence files, from one input.
    let original = std::fs::read_to_string(out_dir.join("original.tsv")).expect("original.tsv");
    let new = std::fs::read_to_string(out_dir.join("new.tsv")).expect("new.tsv");
    let orig_rows = test_support::full_stack::parse_side(&original);
    let new_rows = test_support::full_stack::parse_side(&new);

    // Same role set on both sides.
    let orig_roles: Vec<_> = orig_rows.iter().map(|e| e.role.clone()).collect();
    let new_roles: Vec<_> = new_rows.iter().map(|e| e.role.clone()).collect();
    assert_eq!(orig_roles, new_roles);
    assert_eq!(orig_roles, vec!["baseapp", "loginapp", "reviver"]);

    // Original side records the named blocker; a missing binary is distinct.
    let baseapp = orig_rows.iter().find(|e| e.role == "baseapp").unwrap();
    assert_eq!(baseapp.status, test_support::full_stack::Status::Blocked);
    assert!(baseapp.detail.contains("original-full-stack"));
    let missing = orig_rows.iter().find(|e| e.role == "reviver").unwrap();
    assert!(missing.detail.contains("absent"));

    // New side records the real startup line for every role.
    for ev in &new_rows {
        assert_eq!(ev.status, test_support::full_stack::Status::Ran);
        assert_eq!(ev.detail, format!("[{}] starting (test entry)", ev.role));
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_replacement_mode_notes_the_replaced_role() {
    let root = scratch("repl");
    write_file(&root, "input/roles.txt", "baseapp\ncellapp\n");
    // The original binary dir holds the non-replaced role's binary.
    write_file(&root, "orig/bin/baseapp", "baseapp-fake");
    // The replaced role runs as the new stack.
    let new_bin = root.join("new/boundless-server");
    write_file(
        &root,
        "new/boundless-server",
        "#!/bin/sh\necho \"[$1] starting (test entry)\" >&2\nexit 0\n",
    );
    let mut perms = std::fs::metadata(&new_bin).expect("new meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&new_bin, perms).expect("chmod new");
    let out_dir = root.join("out");

    let result = std::process::Command::new(harness())
        .args([
            root.join("input/roles.txt").to_str().expect("input"),
            root.join("orig/bin").to_str().expect("orig"),
            new_bin.to_str().expect("new"),
            out_dir.to_str().expect("out"),
            "--replace",
            "cellapp",
        ])
        .output()
        .expect("run run-full-stack");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("'cellapp' runs as the new stack"),
        "{stdout}"
    );

    // The report names the replaced role, then rows.
    let text = std::fs::read_to_string(out_dir.join("replacement.tsv")).expect("replacement.tsv");
    assert!(text.starts_with("replaced-role\tcellapp\n"), "{text}");
    let report = test_support::full_stack::parse_replacement(&text);
    assert_eq!(report.replaced_role, "cellapp");

    // The non-replaced role stays as the original; the replaced role ran as
    // the new stack.
    let baseapp = report
        .rows
        .iter()
        .find(|e| e.role == "baseapp")
        .expect("baseapp row");
    assert_eq!(baseapp.side, test_support::full_stack::Side::Original);
    let cellapp = report
        .rows
        .iter()
        .find(|e| e.role == "cellapp")
        .expect("cellapp row");
    assert_eq!(cellapp.side, test_support::full_stack::Side::New);
    assert_eq!(cellapp.status, test_support::full_stack::Status::Ran);
    assert_eq!(cellapp.detail, "[cellapp] starting (test entry)");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_missing_input() {
    let root = scratch("noinput");
    let result = std::process::Command::new(harness())
        .args([
            root.join("roles.txt").to_str().expect("input"),
            root.join("orig").to_str().expect("orig"),
            root.join("new").to_str().expect("new"),
            root.join("out").to_str().expect("out"),
        ])
        .output()
        .expect("run run-full-stack");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("cannot read"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_missing_new_binary() {
    let root = scratch("nonew");
    write_file(&root, "input/roles.txt", "baseapp\n");
    let result = std::process::Command::new(harness())
        .args([
            root.join("input/roles.txt").to_str().expect("input"),
            root.join("orig").to_str().expect("orig"),
            root.join("new/boundless-server").to_str().expect("new"),
            root.join("out").to_str().expect("out"),
        ])
        .output()
        .expect("run run-full-stack");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("new stack binary"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
