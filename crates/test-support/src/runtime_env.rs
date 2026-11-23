//! Original runtime-environment catalog (BW-00-02-01).
//!
//! This module records the facts that pin down the reference's runtime
//! environment — the revision it was taken from, the engine/bundled-Python
//! versions, the database backend and its schema version, the C++ standard the
//! server is built with, the build configurations, and whether prebuilt server
//! binaries are present in the checkout. The committed catalog
//! (`cases/runtime_env.txt`) is regenerated with the `scan-runtime-env` binary
//! and re-parsed by the catalog test, so the file and the scanner cannot
//! drift.
//!
//! Reference model:
//!
//! * `reference-commit` — the git HEAD of the checkout that holds the source
//!   (a `git -C` probe; `-` when the checkout is not a git repository).
//! * `engine-version` — `lib/cstdmf/bwversion.hpp`'s `BW_VERSION_MAJOR/MINOR/
//!   PATCH`.
//! * `bundled-python` — `build/make/third_party_python.mak`'s
//!   `BW_PYTHON_VERSION`.
//! * `db-backend` — `MySQL` when `lib/db_storage_mysql` is present, else the
//!   first `db_storage_*` directory name, else `-`.
//! * `db-schema-version` — `DBAPP_CURRENT_VERSION`'s name from
//!   `lib/db_storage_mysql/versions.hpp`.
//! * `cxx-standard` — the `-std=` flag from `build/make/platform_el7.mak`.
//! * `build-configs` — the `MF_CONFIG` variants the server Makefile accepts
//!   (Hybrid, Debug, Hybrid64, Debug64, plus their `_SingleThreaded` and
//!   `_SystemPython` variants), documented in `common_footer_config.mak`.
//! * `prebuilt-server-binaries` — a hash of each server binary found in the
//!   checkout (`baseapp`, `cellapp`, ...), or `none (source-only checkout)`
//!   when the checkout carries no built server binaries.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/runtime_env.txt");

/// One runtime-environment fact row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnvFact {
    /// The fact name (`reference-commit`, `engine-version`, ...).
    pub item: String,
    /// The recorded value.
    pub value: String,
    /// The file the value came from (or the probe, for the git commit).
    pub source: String,
}

/// The git HEAD of a checkout, or `-` when it is not a git repository.
fn git_head(root: &Path) -> String {
    match Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--short=12")
        .arg("HEAD")
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "-".to_string(),
    }
}

/// The `-std=` flag from a platform makefile, or `-`.
fn cxx_standard(bw_source: &Path) -> String {
    let mk = bw_source.join("build/make/platform_el7.mak");
    let text = std::fs::read_to_string(&mk).unwrap_or_default();
    text.lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("CXX11_CXXFLAGS")
                .and_then(|v| v.split_whitespace().find(|w| w.starts_with("-std=")))
                .map(|w| w.trim_start_matches("-std=").to_string())
        })
        .unwrap_or_else(|| "-".to_string())
}

/// The server binaries present in the checkout, with their SHA-256 hash.
fn prebuilt_binaries(bw_source: &Path) -> String {
    let mut found: Vec<String> = Vec::new();
    let bins = [
        "baseapp",
        "cellapp",
        "baseappmgr",
        "cellappmgr",
        "loginapp",
        "dbmgr",
        "serviceapp",
        "reviver",
        "bwmachined",
        "message_logger",
        "res_packer",
    ];
    for name in bins {
        for dir in ["src/server", "src/tools/res_packer"] {
            let path = bw_source.join(dir).join(name);
            if !path.is_file() {
                continue;
            }
            let Ok(out) = Command::new("sha256sum").arg(&path).output() else {
                continue;
            };
            let hash = String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("?")
                .to_string();
            found.push(format!("{name}:{hash}"));
        }
    }
    if found.is_empty() {
        "none (source-only checkout)".to_string()
    } else {
        found.join(",")
    }
}

/// Scan a reference checkout's runtime-environment facts.
pub fn scan_reference(root: &Path) -> Vec<RuntimeEnvFact> {
    let bw_source = root.join("programming").join("bigworld");
    let mut out = Vec::new();

    // Reference revision.
    out.push(RuntimeEnvFact {
        item: "reference-commit".to_string(),
        value: git_head(root),
        source: "git rev-parse HEAD".to_string(),
    });

    // Engine version from bwversion.hpp.
    let vtext =
        std::fs::read_to_string(bw_source.join("lib/cstdmf/bwversion.hpp")).unwrap_or_default();
    let get = |name: &str| -> String {
        let needle = format!("#define {name} ");
        vtext
            .lines()
            .find_map(|l| l.trim().strip_prefix(&needle))
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    let version = format!(
        "{}.{}.{}",
        get("BW_VERSION_MAJOR"),
        get("BW_VERSION_MINOR"),
        get("BW_VERSION_PATCH")
    );
    out.push(RuntimeEnvFact {
        item: "engine-version".to_string(),
        value: version,
        source: "lib/cstdmf/bwversion.hpp".to_string(),
    });

    // Bundled Python version.
    let ptext = std::fs::read_to_string(bw_source.join("build/make/third_party_python.mak"))
        .unwrap_or_default();
    let py_version = ptext
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("BW_PYTHON_VERSION")
                .map(|v| v.trim().trim_start_matches(":=").trim().to_string())
        })
        .unwrap_or_default();
    out.push(RuntimeEnvFact {
        item: "bundled-python".to_string(),
        value: py_version,
        source: "build/make/third_party_python.mak".to_string(),
    });

    // Database backend and schema version.
    let db_dir = "lib/db_storage_mysql";
    let db_backend = if bw_source.join(db_dir).is_dir() {
        "MySQL".to_string()
    } else {
        std::fs::read_dir(bw_source.join("lib"))
            .ok()
            .and_then(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .find(|n| n.starts_with("db_storage_"))
            })
            .unwrap_or_else(|| "-".to_string())
    };
    out.push(RuntimeEnvFact {
        item: "db-backend".to_string(),
        value: db_backend.clone(),
        source: db_dir.to_string(),
    });
    let versions_hpp = bw_source.join(db_dir).join("versions.hpp");
    let schema = std::fs::read_to_string(&versions_hpp)
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let l = l.trim();
                l.strip_prefix("const uint32\tDBAPP_CURRENT_VERSION")
                    .map(|v| {
                        v.trim()
                            .trim_start_matches('=')
                            .trim()
                            .trim_end_matches(';')
                            .trim()
                            .to_string()
                    })
            })
        })
        .unwrap_or_else(|| "-".to_string());
    out.push(RuntimeEnvFact {
        item: "db-schema-version".to_string(),
        value: schema,
        source: format!("{db_dir}/versions.hpp"),
    });

    // C++ standard.
    let cxx = cxx_standard(&bw_source);
    out.push(RuntimeEnvFact {
        item: "cxx-standard".to_string(),
        value: if cxx == "-" {
            "-".to_string()
        } else {
            format!("-std={cxx}")
        },
        source: "build/make/platform_el7.mak".to_string(),
    });

    // Build configurations.
    let configs = "Hybrid,Debug,Hybrid64,Debug64,SingleThreaded,SystemPython";
    out.push(RuntimeEnvFact {
        item: "build-configs".to_string(),
        value: configs.to_string(),
        source: "build/make/common_footer_config.mak (MF_CONFIG variants)".to_string(),
    });

    // Prebuilt server binaries (a gap when the checkout is source-only).
    out.push(RuntimeEnvFact {
        item: "prebuilt-server-binaries".to_string(),
        value: prebuilt_binaries(&bw_source),
        source: "src/server + src/tools/res_packer".to_string(),
    });

    out
}

/// Render the scan into the tab-separated catalog text.
pub fn render(facts: &[RuntimeEnvFact]) -> String {
    let mut out = String::new();
    out.push_str("item\tvalue\tsource\n");
    for f in facts {
        let _ = writeln!(out, "{}\t{}\t{}", f.item, f.value, f.source);
    }
    out
}

/// Parse the catalog text back into [`RuntimeEnvFact`]s.
pub fn parse_catalog(text: &str) -> Vec<RuntimeEnvFact> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(RuntimeEnvFact {
            item: f.next().unwrap_or("").to_string(),
            value: f.next().unwrap_or("-").to_string(),
            source: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<RuntimeEnvFact> {
    parse_catalog(CATALOG_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog records the runtime-environment facts the
    /// acceptance demands (commit, binary hashes, build params, Python and DB
    /// versions).
    #[test]
    fn committed_catalog_records_runtime_environment() {
        let catalog = catalog();
        let get = |item: &str| {
            catalog
                .iter()
                .find(|f| f.item == item)
                .unwrap_or_else(|| panic!("{item} missing from catalog"))
        };

        // Every fact row carries a value and a source.
        for fact in &catalog {
            assert!(!fact.value.is_empty(), "{} has no value", fact.item);
            assert!(!fact.source.is_empty(), "{} has no source", fact.item);
        }

        assert_eq!(get("engine-version").value, "14.4.1");
        assert_eq!(get("bundled-python").value, "3.13");
        assert_eq!(get("db-backend").value, "MySQL");
        assert_eq!(get("cxx-standard").value, "-std=c++23");
        assert!(
            get("build-configs").value.contains("Hybrid")
                && get("build-configs").value.contains("Debug64")
        );
        // The reference checkout is source-only: no prebuilt server binaries.
        assert_eq!(
            get("prebuilt-server-binaries").value,
            "none (source-only checkout)"
        );
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped in CI.
    ///
    /// The `reference-commit` row is excluded from both sides: the reference
    /// checkout's HEAD is a moving target (porting work commits into it), so
    /// the committed catalog records the snapshot the facts were taken from
    /// rather than a value the rescan can reproduce.
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let bw_source = root.join("programming").join("bigworld");
        if !bw_source.join("lib/cstdmf/bwversion.hpp").is_file() {
            eprintln!("reference checkout absent; skipping rescan");
            return;
        }
        let strip_commit = |text: String| -> String {
            text.lines()
                .filter(|l| !l.starts_with("reference-commit\t"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let scan = strip_commit(render(&scan_reference(&root)));
        let catalog = strip_commit(CATALOG_TEXT.to_string());
        assert_eq!(scan, catalog);
    }
}
