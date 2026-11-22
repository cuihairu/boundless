//! The `scan-client-login` CLI, driven as a child process against a scratch
//! checkout: it writes the minimal client-login baseline catalog next to it,
//! recording the login messages and statuses re-derived from the source and
//! the frozen request/callback sequence, and it refuses a checkout that has no
//! `programming/bigworld/lib/connection/login_interface.hpp`.

use std::path::Path;

/// The compiled `scan-client-login`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-client-login"),
            dir.join("deps").join("scan-client-login"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-client-login not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-client-login-cli-{}-{tag}",
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

/// A minimal BigWorld source tree whose login facts are all deterministic (the
/// scratch root is not a git repository, so `reference-commit` is `-`).
fn make_source_tree(root: &Path) {
    write_file(
        root,
        "programming/bigworld/lib/connection/login_interface.hpp",
        "MERCURY_VARIABLE_MESSAGE( login, 2, &gLoginHandler )\n\
         MERCURY_EMPTY_MESSAGE( probe, &gProbeHandler )\n\
         MERCURY_VARIABLE_MESSAGE( challengeResponse, 2, &gChallengeResponseHandler )\n",
    );
    write_file(
        root,
        "programming/bigworld/lib/connection/log_on_status.hpp",
        "enum Status\n{\n\tNOT_SET,\n\tLOGGED_ON,\n\tLOGIN_REJECTED_NO_SUCH_USER,\n};\n",
    );
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
        .expect("run scan-client-login");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("4 request steps"), "{stdout}");
    assert!(stdout.contains("5 callback steps"), "{stdout}");
    assert!(stdout.contains("11 attempt lines"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    assert_eq!(
        written,
        test_support::client_login::render(&test_support::client_login::scan_reference(&root))
    );

    let records = test_support::client_login::parse_catalog(&written);
    let get = |item: &str| {
        records
            .iter()
            .find(|f| f.item == item)
            .unwrap_or_else(|| panic!("{item} row missing"))
    };

    // Re-derived facts: the login messages and statuses, and the frozen commit.
    assert_eq!(get("reference-commit").value, "-");
    assert_eq!(
        get("login-messages").value,
        "login, probe, challengeResponse"
    );
    assert!(get("logon-statuses").value.contains("LOGGED_ON"));

    // The frozen request/callback sequence is retained.
    assert!(get("request.1").value.contains("logOnTo"));
    assert!(get("request.4").value.contains("LoginReplyRecord"));
    assert!(
        get("callback.1")
            .value
            .contains("onLoginAppLoginChallengeIssued")
    );
    assert!(get("callback.5").value.contains("onLoggedOff"));

    // The named blockers and the real client run are retained.
    assert!(get("blocker.loginapp-pubkey").value.contains("public key"));
    assert!(get("blocker.cluster-start").value.contains("BW-00-02-03"));
    assert!(get("attempt.11").value.contains("Failed to initialise"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_login_interface() {
    let root = scratch("bare");
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-client-login");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld checkout"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
