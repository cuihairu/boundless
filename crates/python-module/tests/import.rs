//! BW-00-03-03 acceptance: after the build's module artifact is installed on
//! the Python path, the minimal module imports.
//!
//! Cargo emits the `cdylib` as `libboundless.so`, but CPython's extension
//! loader only looks for `boundless.so` (the `lib` prefix is a shared-library
//! convention Python does not use for module discovery). The *install* step
//! is exactly that rename, which is what this test performs into a scratch
//! directory before asking a real interpreter to `import boundless`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The directory the built `cdylib` lands in, derived the way Cargo lays it
/// out: `CARGO_TARGET_DIR` when set, else `<workspace>/target`, under the
/// profile the test binary was built for.
fn built_module_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../../target"));
    let profile = {
        let exe = std::env::current_exe().expect("test executable path");
        if exe.to_string_lossy().contains("/release/") {
            "release"
        } else {
            "debug"
        }
    };
    target.join(profile)
}

/// The interpreter the build linked against: `BOUNDLESS_PYTHON` first, then
/// `python3` on PATH, matching `python-host`'s `build.rs` probe.
fn interpreter() -> PathBuf {
    if let Ok(path) = std::env::var("BOUNDLESS_PYTHON")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    PathBuf::from("python3")
}

/// Copies `libboundless.so` into a scratch install dir under the name CPython
/// resolves (`boundless.so`), returning that dir.
fn install_dir() -> PathBuf {
    let built = built_module_dir().join("libboundless.so");
    assert!(
        built.is_file(),
        "the built module is missing; run `cargo build -p python-module` first \
         (looked at {})",
        built.display()
    );
    let dir = std::env::temp_dir().join("boundless-python-module-install");
    std::fs::create_dir_all(&dir).expect("create scratch install dir");
    let installed = dir.join("boundless.so");
    std::fs::copy(&built, &installed).expect("install (copy) the module");
    dir
}

#[test]
fn the_minimal_module_imports_after_install() {
    let dir = install_dir();
    let py = interpreter();
    // The acceptance is the import; the two extra assertions make the import
    // verifiable as more than a symbol resolve.
    let script = r#"
import boundless
assert boundless.__name__ == "boundless", boundless.__name__
assert boundless.__version__, "missing __version__"
assert callable(boundless.version), "missing version()"
assert boundless.version() == boundless.__version__, "version() != __version__"
print("boundless", boundless.__version__, "imported")
"#;
    let out = Command::new(&py)
        .arg("-c")
        .arg(script)
        .env("PYTHONPATH", &dir)
        .output()
        .expect("spawn the interpreter");
    assert!(
        out.status.success(),
        "import failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("imported"),
        "unexpected interpreter output: {stdout}"
    );
}

/// The install rename is the piece the acceptance depends on, so it is pinned
/// on its own: the module must be discoverable as `boundless.so`, not only as
/// `libboundless.so`.
#[test]
fn the_install_step_renames_libboundless_so() {
    let built = built_module_dir().join("libboundless.so");
    assert!(
        built.is_file(),
        "libboundless.so not built at {}",
        built.display()
    );
    let dir = install_dir();
    let installed = dir.join("boundless.so");
    assert!(
        installed.is_file(),
        "boundless.so not installed at {}",
        installed.display()
    );
    // The two names are the same artifact.
    assert!(
        Path::new(&built).file_name().is_some(),
        "expected a lib-prefixed artifact name"
    );
    assert_eq!(
        installed
            .file_name()
            .map(|s| s.to_string_lossy().into_owned()),
        Some("boundless.so".into())
    );
}
