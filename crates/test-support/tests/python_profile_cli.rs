//! The `capture-python-profile` CLI, driven as a child process: it runs the
//! committed known script inside the embedded interpreter under the profile
//! shim and writes per-function timings, so the acceptance (a known script can
//! locate the function the time went to) is verified end to end.

/// The compiled `capture-python-profile`, found by walking up from the test
/// executable (the same layout the other `scan-*` CLI tests use).
fn capture() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("capture-python-profile"),
            dir.join("deps").join("capture-python-profile"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled capture-python-profile not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw23-python-profile-cli-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");
    root
}

#[test]
fn the_cli_captures_the_known_script_and_locates_the_hot_function() {
    let root = scratch("capture");
    let output = root.join("capture.tsv");

    let result = std::process::Command::new(capture())
        .arg(&output)
        .output()
        .expect("run capture-python-profile");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    // The known script's functions are captured (the shim's own `_bwp_stop`
    // may also appear; the acceptance is that the script's functions are
    // found by name).
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("burn"), "{stdout}");
    assert!(stdout.contains("cheap"), "{stdout}");

    let text = std::fs::read_to_string(&output).expect("capture.tsv");
    let rows = test_support::python_profile::parse_rows(&text);
    let burn = rows
        .iter()
        .find(|r| r.name == "burn")
        .expect("burn captured");
    let cheap = rows
        .iter()
        .find(|r| r.name == "cheap")
        .expect("cheap captured");
    assert_eq!(burn.calls, 1);
    assert_eq!(cheap.calls, 1);

    // The acceptance: the function the time went to is located — the heavy
    // loop dominates the cheap multiply by orders of magnitude.
    assert!(
        burn.total_nanos > cheap.total_nanos,
        "burn ({}) should dominate cheap ({})",
        burn.total_nanos,
        cheap.total_nanos
    );
    assert!(burn.total_nanos > 1_000_000, "burn should be measurable");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_script_file_it_cannot_read() {
    let root = scratch("missing");
    let output = root.join("capture.tsv");
    let missing = root.join("does-not-exist.py");

    let result = std::process::Command::new(capture())
        .arg(&output)
        .arg(&missing)
        .output()
        .expect("run capture-python-profile");
    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("cannot read"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
