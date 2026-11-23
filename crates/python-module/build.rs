//! BW-00-03-03 build glue: link the cdylib against the CPython the build
//! machine has.
//!
//! This mirrors `python-host`'s build script — same probe, same link flags —
//! so a single CPython revision drives both crates. The cdylib tolerates
//! undefined symbols at link time (they resolve when the interpreter loads
//! the extension), but the crate's *tests* are executables, which do not:
//! linking the library the same way the runtime links it is what lets the
//! layout-pin and import tests run at all.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The interpreter probe result, kept in one type so the two readbacks are
/// declared together (clippy otherwise flags each unused field).
#[derive(Debug)]
struct Probe {
    library_dir: PathBuf,
    loader_version: String,
}

fn command_output(program: &std::ffi::OsStr, args: &[&str]) -> Option<Output> {
    Command::new(program).args(args).output().ok()
}

fn python_probe(program: &std::ffi::OsStr) -> Option<Probe> {
    let python = command_output(program, &["-c", PYPROBE])?;
    if !python.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&python.stdout);
    let mut lines = stdout.lines();
    let library_dir = lines.next()?.trim();
    let loader_version = lines.next()?.trim();
    if library_dir.is_empty() || loader_version.is_empty() || !loader_version.starts_with("3.") {
        return None;
    }
    Some(Probe {
        library_dir: PathBuf::from(library_dir),
        loader_version: loader_version.to_owned(),
    })
}

/// Reads the development library directory and the loader version
/// (`python3.14` -> `3.14`) from one interpreter process.
const PYPROBE: &str = r#"
import os
import sysconfig
name = sysconfig.get_config_var("LIBPL")
if not name:
    name = sysconfig.get_config_var("LIBDIR")
if not name:
    name = os.path.join(sys.prefix, "lib")
version = sysconfig.get_config_var("LDVERSION") or sysconfig.get_config_var("VERSION")
print(name)
print(version)
"#;

fn probe_cpython() -> Probe {
    for program in ["BOUNDLESS_PYTHON", "python3"] {
        let interpreter: std::ffi::OsString =
            std::env::var_os(program).unwrap_or_else(|| program.into());
        if let Some(probe) = python_probe(&interpreter) {
            return probe;
        }
    }
    panic!(
        "python-module needs a CPython development installation: set \
         BOUNDLESS_PYTHON to an interpreter exposing sysconfig (3.13 and \
         later; the reference tree pins 3.13.16)"
    )
}

fn main() {
    println!("cargo:rerun-if-env-changed=BOUNDLESS_PYTHON");
    let probe = probe_cpython();
    println!(
        "cargo:rustc-link-search=native={}",
        probe.library_dir.display()
    );
    println!("cargo:rustc-link-lib=dylib=python{}", probe.loader_version);
}
