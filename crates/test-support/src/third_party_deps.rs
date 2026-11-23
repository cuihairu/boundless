//! Third-party dependency and resource-provenance catalog (BW-00-01-04).
//!
//! The reference vendors every external dependency it needs under
//! `third_party/` — source trees (curl, png, re2, sqlite, the bundled CPython,
//! its pip modules, ...), header-plus-binary packages (the Visual Studio
//! OpenSSL builds, the Windows CMake toolchain, the PuTTY tools) and
//! archive-distributed sources (the MongoDB C++ driver, the Python modules).
//! This module registers that inventory as one row per dependency, carrying the
//! three facts the acceptance demands: the license, whether a `NOTICE` is
//! shipped, and the reuse decision (how the tree is reused — built from
//! vendored sources, linked as precompiled binaries, or extracted from a
//! vendored archive).
//!
//! This module scans those trees into one row per dependency and renders the
//! catalog the acceptance requires: each dependency's license (from its
//! license/README text, falling back to a curated, upstream-known reading where
//! the vendored tree carries no license text), its `NOTICE` flag, its reuse
//! decision and its source. The committed catalog
//! (`cases/third_party_deps.txt`) is regenerated with the
//! `scan-third-party-deps` binary and re-parsed by the catalog test, so the
//! file and the scanner cannot drift.
//!
//! Reference model:
//!
//! * License — the dependency directory is searched for the well-known license
//!   file names (`LICENSE`, `LICENSE.txt`, `COPYING`, `License.txt`,
//!   `COPYRIGHT`, `MS-PL.txt`, `NVIDIA_Texture_Tools_LICENSE.txt`,
//!   `MODULE_LICENSE_BSD_LIKE`, `readme.txt`, `readme.html`, `README`, ...)
//!   and the first whose text matches a license keyword identifies the
//!   license. Trees that ship no license text (the precompiled OpenSSL builds,
//!   PuTTY, the MongoDB driver) fall back to a curated upstream reading so
//!   every row carries a license.
//! * NOTICE — `yes` when a `NOTICE*` file sits at the top of the tree
//!   (only `openssl_android` does in the reference).
//! * Reuse decision — `vendored-source` when the tree carries a root build
//!   manifest (CMake/Make/autotools/Android/Solution), so the reference builds
//!   it from the vendored sources; `vendored-archive` when the tree ships the
//!   sources as an archive (`*.tar.gz` / `*.tar.bz2` / `*.zip`); `precompiled`
//!   when the tree is headers plus prebuilt binaries, with no manifest.
//! * Source — `third_party/<name>` for everything (all are vendored in-tree);
//!   the provenance is the reference checkout's `third_party` directory.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/third_party_deps.txt");

/// The vendored dependency trees, relative to the bigworld source dir.
const DEPS_DIR: &str = "third_party";

/// License file names, in priority order, tried in each dependency directory.
const LICENSE_FILES: &[&str] = &[
    "LICENSE",
    "LICENSE.txt",
    "COPYING",
    "License.txt",
    "COPYRIGHT",
    "MS-PL.txt",
    "NVIDIA_Texture_Tools_LICENSE.txt",
    "MODULE_LICENSE_BSD_LIKE",
    "readme.txt",
    "readme.html",
    "README",
    "README.android",
    "README.txt",
    "README.BigWorld",
];

/// One third-party dependency row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyDep {
    /// The dependency name (its directory under `third_party/`).
    pub dep: String,
    /// The license, or `unidentified (see <file>)` when the tree carries none.
    pub license: String,
    /// `yes` when a `NOTICE*` file is present, else `no`.
    pub notice: String,
    /// `vendored-source`, `vendored-archive` or `precompiled`.
    pub reuse_decision: String,
    /// The in-tree source (`third_party/<name>`).
    pub source: String,
}

/// A curated license reading for a dependency tree that ships no license text.
/// Only used for upstreams whose license is unambiguous and stable, and only
/// when the scan finds no matching license file.
fn curated_license(dep: &str) -> Option<&'static str> {
    Some(match dep {
        "mongodb" => "Apache-2.0 (upstream mongo-cxx-driver, archive)",
        "putty" => "MIT (upstream PuTTY, precompiled tools)",
        "openssl-vs2010" | "openssl-vs2012" | "openssl-vs2013" | "openssl-vs2015" => {
            "OpenSSL license (precompiled, version in tree)"
        }
        "openssl_android" => "OpenSSL license (BSD-like, Android port)",
        "cmake-win32-x86" => "BSD-3-Clause (CMake, licenses in share/cmake-3.23/Licenses)",
        "sqlite" => "public domain (SQLite amalgamation)",
        "WTL" => "MS-PL (Windows Template Library)",
        "python_modules" => "mixed Python package licenses (Twisted MIT, Zope ZPL, Pympler BSD)",
        "nvmeshmender" => "NVIDIA proprietary (SDK license, header AS-IS disclaimer)",
        _ => return None,
    })
}

/// Classify a license text by keyword; `None` when no keyword matches.
///
/// Order matters: the zlib notice text (shared by libpng and recastnavigation)
/// also carries the "permission notice" sentence of the curl license, so the
/// narrower recognisers come first.
fn classify_license(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if lower.contains("boost software license") {
        Some("Boost-1.0".to_string())
    } else if lower.contains("microsoft public license") || lower.contains("ms-pl") {
        Some("MS-PL".to_string())
    } else if lower.contains("python software foundation") {
        Some("PSF-2.0".to_string())
    } else if lower.contains("apache license") {
        Some("Apache-2.0".to_string())
    } else if lower.contains("gnu general public") {
        Some("GPL".to_string())
    } else if lower.contains("this copy of the libpng notices") {
        Some("zlib (libpng notice)".to_string())
    } else if lower.contains("openautomate") {
        Some("NVIDIA OpenAutomate SDK EULA (proprietary)".to_string())
    } else if lower.contains("nvidia corporation") && lower.contains("all rights reserved") {
        Some("NVIDIA proprietary (SDK license)".to_string())
    } else if lower.contains("as-is") && lower.contains("acknowledg") {
        Some("zlib".to_string())
    } else if lower.contains("mit license")
        || (lower.contains("mit") && lower.contains("permission is hereby granted"))
    {
        Some("MIT".to_string())
    } else if lower.contains("copy") && lower.contains("public domain") {
        Some("public domain".to_string())
    } else if lower.contains("copy") && lower.contains("permission notice") {
        Some("MIT-style (curl COPYING)".to_string())
    } else if lower.contains("redistribution and use in source and binary forms") {
        Some("BSD-3-Clause".to_string())
    } else {
        None
    }
}

/// Whether a file name is a prebuilt binary or library.
fn is_prebuilt(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".exe", ".lib", ".a", ".so", ".dll", ".dylib"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Whether a file name is a vendored source archive.
fn is_archive(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".tar.gz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tgz")
        || lower.ends_with(".zip")
}

/// Whether a file name is a build manifest marking a tree as source.
fn is_build_manifest(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "cmakelists.txt"
        || lower == "makefile"
        || lower == "makefile.in"
        || lower == "makefile.am"
        || lower == "configure"
        || lower == "android.mk"
        || lower.ends_with(".sln")
}

/// The license for a dependency directory, from its license/README text.
fn license_of(dep: &str, dep_dir: &Path) -> String {
    for name in LICENSE_FILES {
        let path = dep_dir.join(name);
        if path.is_file() {
            // Lossy read: some license texts are not valid UTF-8 (the
            // OpenAutomate EULA), and a failed read would drop the classifier.
            let text = std::fs::read(&path)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            if let Some(license) = classify_license(&text) {
                return license;
            }
            // The file is present but unclassifiable — remember it so the
            // fallback can reference it, but keep scanning for a classifier.
        }
    }
    if let Some(curated) = curated_license(dep) {
        return curated.to_string();
    }
    // Report the file we looked at if one exists, else the directory itself.
    let examined = LICENSE_FILES
        .iter()
        .map(|n| dep_dir.join(n))
        .find(|p| p.is_file())
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "<no license file in tree>".to_string());
    format!("unidentified (see {examined})")
}

/// The reuse decision: how the vendored tree is consumed.
///
/// Precedence is archive (vendored archive, extracted at build time) over
/// implementation source near the top of the tree (built from vendored
/// sources) over prebuilt binaries (headers plus libraries/executables only).
/// Implementation sources buried under an `include*` directory (the
/// OpenSSL `applink.c` helper) or below a source-only tree (the
/// recastnavigation sources) do not turn a precompiled distribution into a
/// source tree.
fn reuse_decision_of(dep_dir: &Path) -> String {
    // A root build manifest (CMake/Make/autotools/Android/Solution) marks a
    // tree that is built from its vendored sources. Precompiled distributions
    // (the Visual-Studio OpenSSL trees, the Windows CMake toolchain, the PuTTY
    // tools) have no such manifest — they are headers plus prebuilt binaries.
    // Vendored archives (the MongoDB driver tarball, the Python module bundles)
    // are distribution artifacts at the top of the tree, not test fixtures
    // buried deep inside a source tree.
    let mut archive = false;
    let mut manifest = false;
    let mut prebuilt = false;
    let mut binlib_dir = false;
    let mut stack: Vec<(PathBuf, usize)> = vec![(dep_dir.to_path_buf(), 0)];
    while let Some((dir, d)) = stack.pop() {
        if d > 1 {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            let path = entry.path();
            if path.is_dir() {
                if lower == "bin" || lower == "bin64" || lower == "lib" || lower == "lib64" {
                    binlib_dir = true;
                }
                stack.push((path, d + 1));
            } else {
                if is_archive(&name) {
                    archive = true;
                }
                if is_build_manifest(&name) {
                    manifest = true;
                }
                if is_prebuilt(&name) {
                    prebuilt = true;
                }
            }
        }
    }
    if archive {
        "vendored-archive".to_string()
    } else if manifest {
        "vendored-source".to_string()
    } else if prebuilt || binlib_dir {
        "precompiled".to_string()
    } else {
        // Headers only (e.g. the WTL template headers).
        "vendored-source".to_string()
    }
}

/// Scan a reference checkout's vendored dependency trees.
pub fn scan_reference(bw_source: &Path) -> Vec<ThirdPartyDep> {
    let deps = bw_source.join(DEPS_DIR);
    let mut names: Vec<String> = std::fs::read_dir(&deps)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", deps.display()))
        .map(|e| {
            e.expect("read_dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|n| {
            // Directories (or, for the odd vendored file, any entry) count.
            deps.join(n).is_dir()
        })
        .collect();
    names.sort();

    names
        .into_iter()
        .map(|dep| {
            let dep_dir = deps.join(&dep);
            let license = license_of(&dep, &dep_dir);
            let notice = if dep_dir.join("NOTICE").is_file()
                || std::fs::read_dir(&dep_dir)
                    .map(|mut e| {
                        e.any(|f| {
                            f.map(|f| {
                                f.file_name()
                                    .to_string_lossy()
                                    .to_lowercase()
                                    .starts_with("notice")
                            })
                            .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            {
                "yes".to_string()
            } else {
                "no".to_string()
            };
            ThirdPartyDep {
                dep: dep.clone(),
                license,
                notice,
                reuse_decision: reuse_decision_of(&dep_dir),
                source: format!("{DEPS_DIR}/{dep}"),
            }
        })
        .collect()
}

/// Render the scan into the tab-separated catalog text.
pub fn render(deps: &[ThirdPartyDep]) -> String {
    let mut out = String::new();
    out.push_str("dep\tlicense\tnotice\treuse_decision\tsource\n");
    for d in deps {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            d.dep, d.license, d.notice, d.reuse_decision, d.source
        );
    }
    out
}

/// Parse the catalog text back into [`ThirdPartyDep`]s.
pub fn parse_catalog(text: &str) -> Vec<ThirdPartyDep> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(ThirdPartyDep {
            dep: f.next().unwrap_or("").to_string(),
            license: f.next().unwrap_or("-").to_string(),
            notice: f.next().unwrap_or("-").to_string(),
            reuse_decision: f.next().unwrap_or("-").to_string(),
            source: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<ThirdPartyDep> {
    parse_catalog(CATALOG_TEXT)
}

/// The vendored dependency directory name (for the CLI's report line).
pub fn deps_dir_name() -> &'static str {
    DEPS_DIR
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog registers a license, a NOTICE flag and a reuse
    /// decision for every vendored dependency (the acceptance's "每项有许可证、
    /// NOTICE 及复用决策").
    #[test]
    fn committed_catalog_registers_license_notice_and_reuse() {
        let catalog = catalog();
        assert!(catalog.len() >= 20, "{} rows", catalog.len());

        let get = |dep: &str| {
            catalog
                .iter()
                .find(|d| d.dep == dep)
                .unwrap_or_else(|| panic!("{dep} missing from catalog"))
        };

        // Every row has all three facts and an in-tree source.
        for row in &catalog {
            assert!(!row.license.is_empty(), "{} has no license", row.dep);
            assert!(row.notice == "yes" || row.notice == "no", "{}", row.dep);
            assert!(
                row.reuse_decision == "vendored-source"
                    || row.reuse_decision == "vendored-archive"
                    || row.reuse_decision == "precompiled",
                "{}",
                row.dep
            );
            assert_eq!(row.source, format!("{DEPS_DIR}/{}", row.dep));
        }

        // The well-known licenses are identified, not `unidentified`.
        assert_eq!(get("WTL").license, "MS-PL");
        assert_eq!(get("python").license, "PSF-2.0");
        assert_eq!(get("sqlite").license, "public domain (SQLite amalgamation)");
        assert_eq!(get("nedalloc").license, "Boost-1.0");
        assert_eq!(
            get("openautomate").license,
            "NVIDIA OpenAutomate SDK EULA (proprietary)"
        );
        assert_eq!(get("curl").license, "MIT-style (curl COPYING)");
        assert_eq!(get("png").license, "zlib (libpng notice)");
        assert_eq!(get("re2").license, "BSD-3-Clause");
        assert_eq!(get("CppUnitLite2").license, "MIT");

        // Only openssl_android ships a NOTICE in the reference.
        assert_eq!(get("openssl_android").notice, "yes");
        assert_eq!(get("curl").notice, "no");

        // The reuse decision follows the tree shape: buildable from source,
        // distributed as an archive, or headers plus prebuilt binaries.
        assert_eq!(get("curl").reuse_decision, "vendored-source");
        assert_eq!(get("python").reuse_decision, "vendored-source");
        assert_eq!(get("mongodb").reuse_decision, "vendored-archive");
        assert_eq!(get("python_modules").reuse_decision, "vendored-archive");
        assert_eq!(get("openssl-vs2015").reuse_decision, "precompiled");
        assert_eq!(get("putty").reuse_decision, "precompiled");
        assert_eq!(get("cmake-win32-x86").reuse_decision, "precompiled");
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped in CI.
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let bw_source = root.join("programming").join("bigworld");
        if !bw_source.join(DEPS_DIR).is_dir() {
            eprintln!("reference checkout absent; skipping rescan");
            return;
        }
        let scan = scan_reference(&bw_source);
        assert_eq!(render(&scan), CATALOG_TEXT);
    }
}
