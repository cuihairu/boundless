//! The `scan-python-callbacks` CLI, driven as a child process against a
//! scratch checkout: it writes the Python callback fixture catalog next to
//! it, recording each callback's owning type, order, component(s), parameter
//! list and optionality, and it refuses a checkout that has no server source.

use std::path::Path;

/// The compiled `scan-python-callbacks`, found by walking up from the test
/// executable (see `cmake_targets_cli` for the crate's build-script layout).
fn scanner() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let mut dir = exe.parent().expect("test exe parent").to_path_buf();
    loop {
        for candidate in [
            dir.join("scan-python-callbacks"),
            dir.join("deps").join("scan-python-callbacks"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    panic!("compiled scan-python-callbacks not found");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bw00-python-callbacks-cli-{}-{tag}",
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

/// A minimal BigWorld server source tree with one callback block per file.
fn make_source_tree(root: &Path) {
    write(
        root,
        "programming/bigworld/server/baseapp/base.cpp",
        "/*~ callback Base.onDestroy\n\
         *\t@components{ base }\n\
         *\tIf present, this method is called on the base entity just prior to\n\
         *\tit being destroyed. This method has no arguments.\n\
         */\n\
         /*~\tcallback Base.onTimer\n\
         *\t@components{ base }\n\
         *\tThis method is called when a timer associated with this entity is\n\
         *\ttriggered.\n\
         *\t@param timerHandle\tThe id of the timer.\n\
         *\t@param userData\tThe integer user data passed into Base.addTimer.\n\
         */\n",
    );
    write(
        root,
        "programming/bigworld/server/baseapp/proxy.cpp",
        "/*~ callback Proxy.onLogOnAttempt\n\
         *\t@components{ base }\n\
         *\tIf present, this method is called if a client attempts to log in.\n\
         *\t@param ip\tThe IP address of the client.\n\
         *\t@param port\tThe port number of the client.\n\
         *\t@param logOnData\tA string provided by the billing system.\n\
         */\n",
    );
    write(
        root,
        "programming/bigworld/server/cellapp/entity.cpp",
        "/*~ callback Entity.onEnteredCell\n\
         *  @components{ cell }\n\
         *\tThis method is called on a real entity when it has just entered a\n\
         *\tnew cell.\n\
         */\n\
         /*~\tcallback Entity onPassengerAlightAttempt\n\
         *  @components{ cell }\n\
         *\tIf present, this method is called on a vehicle when an entity\n\
         *\tattempts to get off it.\n\
         */\n",
    );
}

#[test]
fn the_cli_writes_the_callback_catalog() {
    let root = scratch("cli");
    make_source_tree(&root);

    let output = root.join("catalog.tsv");
    let result = std::process::Command::new(scanner())
        .args([
            root.to_str().expect("root path"),
            output.to_str().expect("output path"),
        ])
        .output()
        .expect("run scan-python-callbacks");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("5 callbacks"), "{stdout}");

    // The file on disk is what the module embeds and validates.
    let written = std::fs::read_to_string(&output).expect("catalog file");
    assert_eq!(
        written,
        test_support::python_callbacks::render(&test_support::python_callbacks::scan_reference(
            &root
        ))
    );

    let records = test_support::python_callbacks::parse_catalog(&written);
    let get = |item: &str| {
        records
            .iter()
            .find(|f| f.item == item)
            .unwrap_or_else(|| panic!("{item} row missing"))
    };

    // The scratch root is not a git repository, so the commit is `-`.
    assert_eq!(get("reference-commit").value, "-");

    // The Base callbacks in file order: onDestroy (optional) then onTimer.
    assert_eq!(get("cb.1.type").value, "Base");
    assert_eq!(get("cb.1.name").value, "onDestroy");
    assert_eq!(get("cb.1.sequence").value, "1");
    assert_eq!(get("cb.1.params").value, "-");
    assert_eq!(get("cb.1.ignorable").value, "yes");
    assert_eq!(get("cb.2.name").value, "onTimer");
    assert_eq!(get("cb.2.params").value, "timerHandle,userData");
    assert_eq!(get("cb.2.ignorable").value, "no");

    // The space-separated `Type name` header form is parsed as the fifth
    // callback, after the cell `onEnteredCell`.
    assert_eq!(get("cb.4.name").value, "onEnteredCell");
    assert_eq!(get("cb.4.type").value, "Entity");
    let passenger = records
        .iter()
        .find(|f| f.item == "cb.5.name")
        .expect("fifth callback");
    assert_eq!(passenger.value, "onPassengerAlightAttempt");
    assert_eq!(get("cb.5.type").value, "Entity");
    assert_eq!(get("cb.5.ignorable").value, "yes");

    // The blocker is recorded.
    assert!(
        get("blocker.python-callback-capture")
            .value
            .contains("python-callback-capture")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cli_refuses_a_checkout_without_server_source() {
    let root = scratch("bare");
    std::fs::create_dir_all(&root).expect("mkdir");

    let result = std::process::Command::new(scanner())
        .arg(&root)
        .output()
        .expect("run scan-python-callbacks");
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("not a BigWorld checkout"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}
