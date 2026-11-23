//! Release-script and RPM-target catalog (BW-00-01-03).
//!
//! The reference's release machinery is a set of build scripts under
//! `build/bw_internal/scripts/` (a thin `makeRPM.py` wrapper, the server and
//! solution builders, and the Windows client/unit-test/archive drivers) plus a
//! version trio (`lib/cstdmf/bwversion.hpp`, the bundled Python version in
//! `build/make/third_party_python.mak`, and the Linux distribution tags in
//! `build/make/platform_info.py`). The actual `make rpm` target the wrapper
//! invokes is not in this checkout — `makeRPM.py` `cd`s to `bigworld/src/server`
//! and runs `make rpm` there, a path that belongs to the internal BigWorld
//! build repo, so the target is registered as out-of-tree.
//!
//! This module scans those sources into one row per release artifact and
//! renders the catalog the acceptance requires: each artifact's kind, source
//! path, purpose, the deliverables it produces, and the version it pins. The
//! committed catalog (`cases/release_artifacts.txt`) is regenerated with the
//! `scan-release-artifacts` binary and re-parsed by the catalog test, so the
//! file and the scanner cannot drift.
//!
//! Reference model:
//!
//! * Scripts — every `*.py` / `*.bat` in `build/bw_internal/scripts/` is one
//!   row. `kind` is `python-script` or `batch`. `purpose` is the first
//!   docstring line (Python) or the first `rem` comment (batch). `deliverables`
//!   are inferred from the body: `make rpm`/`makeRPM` → `rpm`;
//!   `make unit_tests` → `unit-tests`; `make %s` with a config → `server-build`;
//!   `robocopy` → `windows-archive`; `cmake` → `cmake-solution`; a mention of
//!   `res_packer` → `res_packer`.
//! * Engine version — `bwversion.hpp`'s `BW_VERSION_MAJOR/MINOR/PATCH` as
//!   `major.minor.patch`, plus the `BW_COPYRIGHT_NOTICE`.
//! * Bundled Python — `BW_PYTHON_VERSION` from `third_party_python.mak`.
//! * Platform tags — the `Distribution → tag` table in `platform_info.py`'s
//!   docstring.
//! * Out-of-tree targets — `makeRPM.py`'s `make rpm` (server) and
//!   `makeServer.py`'s server + `res_packer` build are registered as rows whose
//!   path names the internal build-repo directory, flagged `out-of-tree`.

use std::fmt::Write as _;
use std::path::Path;

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/release_artifacts.txt");

/// The release scripts directory, relative to the bigworld source dir.
const SCRIPTS_DIR: &str = "build/bw_internal/scripts";

/// One release artifact row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifact {
    /// The artifact name (script file name, `rpm`, `server-build`,
    /// `engine-version`, `bundled-python`, `platform-tags`).
    pub artifact: String,
    /// `python-script`, `batch`, `make-target`, `version`, `platform-tags`.
    pub kind: String,
    /// In-tree relative path, or an out-of-tree directory with `(out-of-tree)`.
    pub path: String,
    /// One-line purpose.
    pub purpose: String,
    /// Comma-separated deliverables, or `-`.
    pub deliverables: String,
    /// The version the artifact pins, or `-`.
    pub version: String,
}

/// The first purpose-bearing comment line: the first `#`/`///`/docstring
/// comment (Python) or `rem` comment (batch) whose text is neither the shebang
/// nor a trivial marker (the file's own name — the trailing `# <scriptname>`
/// convention — a `set JOB_NAME=...` batch assignment, an import). Lines are
/// considered in file order.
fn first_purpose(text: &str, is_batch: bool, name: &str) -> String {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if is_batch {
            if let Some(rest) = line.strip_prefix("rem ") {
                let rest = rest.trim();
                if !rest.is_empty() && !rest.starts_with("set ") {
                    return rest.to_string();
                }
            }
        } else {
            let content = if let Some(rest) = line.strip_prefix("///") {
                rest
            } else if let Some(rest) = line.strip_prefix("//") {
                rest
            } else if let Some(rest) = line.strip_prefix("\"\"\"") {
                rest
            } else if let Some(rest) = line.strip_prefix('#') {
                rest.trim_start_matches('#')
            } else {
                continue;
            };
            let content = content.trim();
            if !content.is_empty()
                && !line.starts_with("#!")
                && !content.starts_with("import ")
                && !content.starts_with("from ")
                && content != name
            {
                return content.to_string();
            }
        }
    }
    "-".to_string()
}

/// Infer the deliverables a script body mentions.
fn infer_deliverables(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |d: &str| {
        if !out.iter().any(|x| x == d) {
            out.push(d.to_string());
        }
    };
    if text.contains("make rpm") {
        push("rpm");
    }
    if text.contains("unit_tests") {
        push("unit-tests");
    }
    if text.contains("robocopy") || text.contains("ARCHIVE_DIR") {
        push("windows-archive");
    }
    if text.contains("cmake") {
        push("cmake-solution");
    }
    if text.contains("res_packer") {
        push("res_packer");
    }
    if text.contains("makeServer") || text.contains("make %s") || text.contains("makeWithConfig") {
        push("server-build");
    }
    if out.is_empty() {
        vec!["-".to_string()]
    } else {
        out
    }
}

/// Scan a reference checkout's release scripts and version sources.
pub fn scan_reference(bw_source: &Path) -> Vec<ReleaseArtifact> {
    let mut out = Vec::new();
    let scripts = bw_source.join(SCRIPTS_DIR);

    // Scripts, sorted by name.
    let mut script_names: Vec<String> = std::fs::read_dir(&scripts)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", scripts.display()))
        .map(|e| {
            e.expect("read_dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|n| n.ends_with(".py") || n.ends_with(".bat"))
        .collect();
    script_names.sort();

    for name in script_names {
        let path = scripts.join(&name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let is_batch = name.ends_with(".bat");
        let purpose = first_purpose(&text, is_batch, &name);
        let deliverables = infer_deliverables(&text).join(",");
        out.push(ReleaseArtifact {
            artifact: name.clone(),
            kind: if is_batch { "batch" } else { "python-script" }.to_string(),
            path: format!("{SCRIPTS_DIR}/{name}"),
            purpose,
            deliverables,
            version: "-".to_string(),
        });
    }

    // Engine version from bwversion.hpp.
    let version_hpp = bw_source.join("lib/cstdmf/bwversion.hpp");
    let vtext = std::fs::read_to_string(&version_hpp)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", version_hpp.display()));
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
    let copyright = vtext
        .lines()
        .find(|l| l.contains("BW_COPYRIGHT_NOTICE"))
        .and_then(|l| l.split('"').nth(1))
        .unwrap_or_default()
        .to_string();
    out.push(ReleaseArtifact {
        artifact: "engine-version".to_string(),
        kind: "version".to_string(),
        path: "lib/cstdmf/bwversion.hpp".to_string(),
        purpose: format!("engine version {version}; copyright {copyright}"),
        deliverables: "-".to_string(),
        version: version.clone(),
    });

    // Bundled Python version.
    let py_mak = bw_source.join("build/make/third_party_python.mak");
    let ptext = std::fs::read_to_string(&py_mak)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", py_mak.display()));
    let py_version = ptext
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("BW_PYTHON_VERSION")
                .map(|v| v.trim().trim_start_matches(":=").trim().to_string())
        })
        .unwrap_or_default();
    out.push(ReleaseArtifact {
        artifact: "bundled-python".to_string(),
        kind: "version".to_string(),
        path: "build/make/third_party_python.mak".to_string(),
        purpose: "bundled CPython version".to_string(),
        deliverables: "-".to_string(),
        version: py_version.clone(),
    });

    // Platform tags from platform_info.py's docstring table.
    let pf_py = bw_source.join("build/make/platform_info.py");
    let ptext = std::fs::read_to_string(&pf_py)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", pf_py.display()));
    // The table is the leading `#`-comment block of platform_info.py:
    // `# Distribution     Expected output` then `# CentOS 5         el5`
    // etc. A row is kept when its tag matches the el/fedora tag pattern, so
    // the TODO prose in the same block cannot leak in.
    let mut block: Vec<&str> = Vec::new();
    for line in ptext.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with("#!") {
            continue;
        }
        if let Some(rest) = l.strip_prefix('#') {
            block.push(rest.trim());
        } else {
            break; // the first code line ends the comment block
        }
    }
    let digits_after = |tag: &str, prefix: &str| -> bool {
        tag.len() > prefix.len() && tag[prefix.len()..].chars().all(|c| c.is_ascii_digit())
    };
    let tags: Vec<String> = block
        .iter()
        .filter_map(|l| {
            let tokens: Vec<&str> = l.split_whitespace().collect();
            if tokens.len() < 2 {
                return None;
            }
            let distro = tokens[..tokens.len() - 1].join(" ");
            let tag = tokens[tokens.len() - 1];
            let tag_ok = digits_after(tag, "el") || digits_after(tag, "fedora");
            if tag_ok && distro.chars().next().is_some_and(char::is_uppercase) {
                Some(format!("{distro}:{tag}"))
            } else {
                None
            }
        })
        .collect();
    out.push(ReleaseArtifact {
        artifact: "platform-tags".to_string(),
        kind: "platform-tags".to_string(),
        path: "build/make/platform_info.py".to_string(),
        purpose: "Linux distribution to RPM/archive platform tag".to_string(),
        deliverables: "-".to_string(),
        version: tags.join(","),
    });

    // Out-of-tree targets (registered, not scanned).
    out.push(ReleaseArtifact {
        artifact: "rpm".to_string(),
        kind: "make-target".to_string(),
        path: "bigworld/src/server (out-of-tree)".to_string(),
        purpose: "make rpm builds the server RPM (Indie with BIGWORLD_INDIE=1)".to_string(),
        deliverables: "rpm".to_string(),
        version: version.clone(),
    });
    out.push(ReleaseArtifact {
        artifact: "server-build".to_string(),
        kind: "make-target".to_string(),
        path: "bigworld/src/server + bigworld/src/tools/res_packer (out-of-tree)".to_string(),
        purpose: "make builds server binaries and res_packer per MF_CONFIG".to_string(),
        deliverables: "server-build,res_packer".to_string(),
        version: "-".to_string(),
    });

    out
}

/// Render the scan into the tab-separated catalog text.
pub fn render(artifacts: &[ReleaseArtifact]) -> String {
    let mut out = String::new();
    out.push_str("artifact\tkind\tpath\tpurpose\tdeliverables\tversion\n");
    for a in artifacts {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}",
            a.artifact, a.kind, a.path, a.purpose, a.deliverables, a.version
        );
    }
    out
}

/// Parse the catalog text back into [`ReleaseArtifact`]s.
pub fn parse_catalog(text: &str) -> Vec<ReleaseArtifact> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(ReleaseArtifact {
            artifact: f.next().unwrap_or("").to_string(),
            kind: f.next().unwrap_or("").to_string(),
            path: f.next().unwrap_or("").to_string(),
            purpose: f.next().unwrap_or("").to_string(),
            deliverables: f.next().unwrap_or("-").to_string(),
            version: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<ReleaseArtifact> {
    parse_catalog(CATALOG_TEXT)
}

/// The release scripts directory name (for the CLI's report line).
pub fn scripts_dir_name() -> &'static str {
    SCRIPTS_DIR
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog registers the version, the bundled Python, the
    /// out-of-tree RPM target, and the release scripts (the acceptance's
    /// "独有交付物及版本").
    #[test]
    fn committed_catalog_records_deliverables_and_versions() {
        let catalog = catalog();
        let version = catalog
            .iter()
            .find(|a| a.artifact == "engine-version")
            .expect("engine version");
        assert_eq!(version.version, "14.4.1");
        assert!(version.purpose.contains("14.4.1"));

        let python = catalog
            .iter()
            .find(|a| a.artifact == "bundled-python")
            .expect("bundled python");
        assert_eq!(python.version, "3.13");

        let rpm = catalog
            .iter()
            .find(|a| a.artifact == "rpm")
            .expect("rpm target");
        assert_eq!(rpm.kind, "make-target");
        assert!(rpm.path.contains("out-of-tree"));
        assert_eq!(rpm.deliverables, "rpm");

        let scripts = catalog
            .iter()
            .filter(|a| a.kind == "python-script" || a.kind == "batch")
            .map(|a| a.artifact.as_str())
            .collect::<Vec<_>>();
        assert!(scripts.contains(&"makeRPM.py"));
        assert!(scripts.contains(&"makeServer.py"));
        assert!(scripts.contains(&"winArchiveBuild.bat"));

        let make_rpm = catalog
            .iter()
            .find(|a| a.artifact == "makeRPM.py")
            .expect("makeRPM.py");
        assert!(make_rpm.deliverables.contains("rpm"));
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped in CI.
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let bw_source = root.join("programming").join("bigworld");
        if !bw_source.join(SCRIPTS_DIR).is_dir() {
            eprintln!("reference checkout absent; skipping rescan");
            return;
        }
        let scan = scan_reference(&bw_source);
        assert_eq!(render(&scan), CATALOG_TEXT);
    }
}
