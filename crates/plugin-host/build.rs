//! Compile the plugin fixtures into shared libraries.
//!
//! The loader under test opens a real loadable artifact, so each `fixtures/*.rs`
//! source is built by rustc itself as a cdylib into `OUT_DIR`. The tests then
//! hand the produced `.so` path to the loader, which keeps this crate free of
//! any dependency on where cargo happens to lay out artifacts.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_sources() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read the plugin fixtures")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .collect();
    sources.sort();
    sources
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let mut built = Vec::new();
    for source in fixture_sources() {
        println!("cargo:rerun-if-changed={}", source.display());
        let crate_name = source
            .file_stem()
            .expect("a fixture has a name")
            .to_string_lossy()
            .to_string();
        let status = Command::new(std::env::var("RUSTC").expect("RUSTC"))
            .arg("--crate-name")
            .arg(&crate_name)
            .arg("--crate-type")
            .arg("cdylib")
            .arg("--edition")
            .arg("2024")
            .arg("--out-dir")
            .arg(&out)
            .arg(&source)
            .status()
            .expect("run rustc");
        assert!(
            status.success(),
            "rustc failed to compile {}",
            source.display()
        );
        built.push(format!("lib{crate_name}.so"));
    }
    assert!(!built.is_empty(), "no plugin fixtures were compiled");
    println!("cargo:rustc-env=PLUGIN_FIXTURE_DIR={}", out.display());
}
