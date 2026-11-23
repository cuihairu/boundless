//! The module build-and-import acceptance (BW-00-03-03): after
//! [`python_module::install`] copies the built cdylib into a site-packages
//! directory, the interpreter the build machine has imports the module and
//! calls its methods.

use std::path::PathBuf;
use std::process::Command;

/// The interpreter the build machine has: `BOUNDLESS_PYTHON` first (the same
/// probe `python-host`'s build script honours), then `python3`.
fn python() -> std::ffi::OsString {
    std::env::var_os("BOUNDLESS_PYTHON").unwrap_or_else(|| "python3".into())
}

fn scratch(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("bw00-python-module-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");
    root
}

#[test]
fn the_installed_module_imports_and_answers() {
    // The built cdylib exists (this test binary is built alongside it).
    assert!(
        python_module::built_artifact().is_file(),
        "built module missing: {}",
        python_module::built_artifact().display()
    );

    let prefix = scratch("import");
    let installed = python_module::install(&prefix).expect("install");
    assert!(installed.is_file(), "{}", installed.display());
    assert_eq!(
        installed.file_name().and_then(|n| n.to_str()),
        Some("boundless_core.so"),
        "Python imports a file named after the module, without the lib prefix"
    );

    let version = std::str::from_utf8(
        &python_module::VERSION_BYTES[..python_module::VERSION_BYTES.len() - 1],
    )
    .expect("version is UTF-8");
    let script = format!(
        "import boundless_core as m\n\
         assert m.answer() == 42, m.answer()\n\
         assert m.engine_version() == \"14.4.1\", m.engine_version()\n\
         assert m.__version__ == \"{version}\", m.__version__\n\
         print(\"imported\", m.__version__)\n"
    );

    let output = Command::new(python())
        .arg("-c")
        .arg(&script)
        .env("PYTHONPATH", prefix.join("site-packages"))
        .output()
        .expect("run python");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // An empty stderr also proves the module carried the interpreter's own
    // `PYTHON_API_VERSION`: a mismatch warns on import instead of failing, so
    // this is the only way the check is visible.
    assert!(
        String::from_utf8_lossy(&output.stderr).is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("imported"), "{stdout}");
    assert!(stdout.contains(version), "{stdout}");
    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn the_import_fails_cleanly_without_the_install() {
    // A site-packages directory without the module must not import it: the
    // install step, not the interpreter, is what makes the module available.
    let prefix = scratch("not-installed");
    std::fs::create_dir_all(prefix.join("site-packages")).expect("mkdir");
    let output = Command::new(python())
        .arg("-c")
        .arg("import boundless_core")
        .env("PYTHONPATH", prefix.join("site-packages"))
        .output()
        .expect("run python");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ModuleNotFoundError"), "{stderr}");
    let _ = std::fs::remove_dir_all(&prefix);
}
