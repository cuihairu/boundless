//! Windows client and tool baseline (BW-00-02-05).
//!
//! Records, per Windows target, the re-test result or the gap that prevented
//! it. The targets are the reference's Windows client (`programming/bigworld/
//! client`) and its tools (`programming/bigworld/tools/*`). The committed
//! catalog (`cases/windows_baseline.txt`) is regenerated with the
//! `scan-windows-baseline` binary and re-parsed by the catalog test, so the
//! file and the scanner cannot drift.
//!
//! The checkout is a server/source checkout: `game/bin/client` and
//! `game/bin/tools` are absent, so no Windows binary is built and every target
//! records a gap. The runner has wine (so Windows binaries could be executed)
//! but nothing to run, and the GUI editors would additionally need a display.
//!
//! Catalog line grammar (tab-separated `item`, `value`, `source`):
//!
//! * `reference-commit` — the reference checkout's git HEAD.
//! * `runner.os` — the host OS (`linux`).
//! * `runner.wine` — the wine version, or `absent`.
//! * `windows-binary-dirs` — `game/bin/client`, `game/bin/tools` and whether
//!   each is present in the checkout.
//! * `target.<n>.name` / `.source-dir` / `.windows-dir` / `.linux-binary` /
//!   `.result` / `.gap` — one indexed row per Windows target.
//! * `blocker.windows-baseline` — the named reason the baselines cannot be
//!   re-tested here.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/windows_baseline.txt");

/// The Windows client source directory (relative to the reference root).
const CLIENT_SOURCE: &str = "programming/bigworld/client";

/// The tools source directory (relative to the reference root).
const TOOLS_SOURCE: &str = "programming/bigworld/tools";

/// The Windows client binary directory (absent in this checkout).
const WIN_CLIENT_DIR: &str = "game/bin/client";

/// The Windows tools binary directory (absent in this checkout).
const WIN_TOOLS_DIR: &str = "game/bin/tools";

/// The Linux (el7) tools binary directory.
const LINUX_TOOLS_DIR: &str = "game/bin/server/el7/tools";

/// One Windows client/tool target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The target name (`client`, `animationexporter`, ...).
    pub name: String,
    /// The source directory in the reference checkout.
    pub source_dir: String,
    /// The Windows binary directory the target would build into (absent here).
    pub windows_dir: String,
    /// The built Linux binary, or `ABSENT` when the target is Windows-only.
    pub linux_binary: String,
    /// The re-test result (`GAP` when no Windows binary is built).
    pub result: String,
    /// The named reason the result is a gap.
    pub gap: String,
}

/// The named blocker: no Windows binaries are built, so the Windows client and
/// tool baselines cannot be re-tested on this runner.
pub const WINDOWS_BLOCKER: &str = "windows-baseline: the checkout is server/source-only — game/bin/client and game/bin/tools are absent, so no Windows binary is built and every Windows target records a gap; wine-10.0 is available to execute Windows binaries but there is nothing to run, and the GUI editors would additionally need a display";

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

/// The wine version on this runner, or `absent`.
fn wine_version() -> String {
    match Command::new("wine").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if v.is_empty() {
                "absent".to_string()
            } else {
                v
            }
        }
        _ => "absent".to_string(),
    }
}

/// The tools' source subdirectories, sorted (the `common` shared library is
/// excluded), as target names.
fn tool_targets(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(TOOLS_SOURCE))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().and_then(|d| d.file_name().into_string().ok()))
        .filter(|n| n != "common")
        .collect();
    names.sort();
    names
}

/// The per-target gap reason.
fn gap_for(windows_dir: &str) -> String {
    format!(
        "no Windows binary built in the checkout ({windows_dir} absent); source present, cannot re-test on this Linux runner"
    )
}

/// One Windows target's re-test record.
fn target(name: &str, root: &Path) -> Target {
    let (source_dir, windows_dir, linux_binary) = if name == "client" {
        (
            CLIENT_SOURCE.to_string(),
            WIN_CLIENT_DIR.to_string(),
            if root.join(WIN_CLIENT_DIR).is_dir() {
                WIN_CLIENT_DIR.to_string()
            } else {
                "ABSENT".to_string()
            },
        )
    } else {
        (
            format!("{TOOLS_SOURCE}/{name}"),
            WIN_TOOLS_DIR.to_string(),
            {
                let bin = root.join(LINUX_TOOLS_DIR).join(name);
                if bin.is_file() {
                    format!("{LINUX_TOOLS_DIR}/{name}")
                } else {
                    "ABSENT".to_string()
                }
            },
        )
    };
    let gap = gap_for(&windows_dir);
    Target {
        name: name.to_string(),
        source_dir,
        windows_dir,
        linux_binary,
        result: "GAP".to_string(),
        gap,
    }
}

/// Scan a reference checkout's Windows client and tool baseline.
pub fn scan_reference(root: &Path) -> Vec<crate::windows_baseline::WindowsFact> {
    let mut facts = Vec::new();

    // Reference revision.
    facts.push(WindowsFact {
        item: "reference-commit".to_string(),
        value: git_head(root),
        source: "git rev-parse HEAD".to_string(),
    });

    // Runner capability.
    facts.push(WindowsFact {
        item: "runner.os".to_string(),
        value: std::env::consts::OS.to_string(),
        source: "std::env::consts::OS".to_string(),
    });
    facts.push(WindowsFact {
        item: "runner.wine".to_string(),
        value: wine_version(),
        source: "wine --version".to_string(),
    });

    // Windows binary directories.
    let client_present = root.join(WIN_CLIENT_DIR).is_dir();
    let tools_present = root.join(WIN_TOOLS_DIR).is_dir();
    facts.push(WindowsFact {
        item: "windows-binary-dirs".to_string(),
        value: format!(
            "{WIN_CLIENT_DIR} ({}), {WIN_TOOLS_DIR} ({})",
            if client_present { "present" } else { "absent" },
            if tools_present { "present" } else { "absent" }
        ),
        source: "game/bin (checkout layout)".to_string(),
    });

    // Per-target records: the client, then the tools (sorted).
    let mut names = vec!["client".to_string()];
    names.extend(tool_targets(root));
    for (i, name) in names.iter().enumerate() {
        let idx = i + 1;
        let t = target(name, root);
        for (suffix, value, source) in [
            (
                "name",
                t.name.clone(),
                "programming/bigworld (target name)".to_string(),
            ),
            (
                "source-dir",
                t.source_dir.clone(),
                "reference source tree".to_string(),
            ),
            ("windows-dir", t.windows_dir.clone(), "game/bin".to_string()),
            (
                "linux-binary",
                t.linux_binary.clone(),
                "game/bin/server/el7/tools".to_string(),
            ),
            ("result", t.result.clone(), "re-test outcome".to_string()),
            ("gap", t.gap.clone(), "runner limitation".to_string()),
        ] {
            facts.push(WindowsFact {
                item: format!("target.{idx}.{suffix}"),
                value,
                source,
            });
        }
    }

    // The named blocker.
    facts.push(WindowsFact {
        item: "blocker.windows-baseline".to_string(),
        value: WINDOWS_BLOCKER.to_string(),
        source: "runner limitation".to_string(),
    });

    facts
}

/// One Windows-baseline fact row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsFact {
    /// The fact name (`reference-commit`, `target.1.name`, ...).
    pub item: String,
    /// The recorded value.
    pub value: String,
    /// The file or probe the value came from.
    pub source: String,
}

/// Render the scan into the tab-separated catalog text.
pub fn render(facts: &[WindowsFact]) -> String {
    let mut out = String::new();
    out.push_str("item\tvalue\tsource\n");
    for f in facts {
        let _ = writeln!(out, "{}\t{}\t{}", f.item, f.value, f.source);
    }
    out
}

/// Parse the catalog text back into [`WindowsFact`]s.
pub fn parse_catalog(text: &str) -> Vec<WindowsFact> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(WindowsFact {
            item: f.next().unwrap_or("").to_string(),
            value: f.next().unwrap_or("-").to_string(),
            source: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<WindowsFact> {
    parse_catalog(CATALOG_TEXT)
}

/// The committed per-target records, in the catalog's indexed order.
pub fn targets() -> Vec<Target> {
    let facts = catalog();
    let get = |item: &str| -> String {
        facts
            .iter()
            .find(|f| f.item == item)
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    let count = facts
        .iter()
        .filter(|f| f.item.ends_with(".name") && f.item.starts_with("target."))
        .count();
    (1..=count)
        .map(|i| Target {
            name: get(&format!("target.{i}.name")),
            source_dir: get(&format!("target.{i}.source-dir")),
            windows_dir: get(&format!("target.{i}.windows-dir")),
            linux_binary: get(&format!("target.{i}.linux-binary")),
            result: get(&format!("target.{i}.result")),
            gap: get(&format!("target.{i}.gap")),
        })
        .collect()
}

/// The committed named blocker for the Windows baselines.
pub fn blocker() -> String {
    catalog()
        .into_iter()
        .find(|f| f.item == "blocker.windows-baseline")
        .map(|f| f.value)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog records every Windows target's result or gap:
    /// the client and the tools, each with source present, no Windows binary
    /// built, and a named gap.
    #[test]
    fn committed_catalog_records_per_target_gap() {
        let targets = targets();
        assert!(
            targets.len() > 20,
            "client plus the tools: {}",
            targets.len()
        );
        let client = targets.iter().find(|t| t.name == "client").expect("client");
        assert!(client.source_dir.ends_with("/client"));
        assert_eq!(client.result, "GAP");
        assert!(client.gap.contains("game/bin/client"));

        let res_packer = targets
            .iter()
            .find(|t| t.name == "res_packer")
            .expect("res_packer");
        assert_eq!(res_packer.result, "GAP");
        assert!(res_packer.gap.contains("game/bin/tools"));
        // Cross-platform tools have a Linux build; the Windows baseline is
        // still a gap.
        assert!(res_packer.linux_binary.contains("res_packer"));

        // Every target has source present, no Windows binary, and a gap.
        for t in &targets {
            assert!(!t.source_dir.is_empty(), "{t:?}");
            assert!(!t.windows_dir.is_empty(), "{t:?}");
            assert_eq!(t.result, "GAP", "{t:?}");
            assert!(!t.gap.is_empty(), "{t:?}");
            assert!(t.gap.contains("no Windows binary built"), "{t:?}");
        }

        let blocker = blocker();
        assert!(blocker.contains("windows-baseline"), "{blocker}");
        assert!(blocker.contains("wine"), "{blocker}");
        assert!(blocker.contains("game/bin/client"), "{blocker}");
    }

    /// Every committed fact row carries a value and a source.
    #[test]
    fn every_fact_has_value_and_source() {
        for fact in catalog() {
            assert!(!fact.value.is_empty(), "{} has no value", fact.item);
            assert!(!fact.source.is_empty(), "{} has no source", fact.item);
        }
    }

    /// The catalog round-trips through render and parse.
    #[test]
    fn render_parse_round_trip() {
        assert_eq!(parse_catalog(&render(&catalog())), catalog());
    }

    /// The target enumerator finds the client and the tools, sorted.
    #[test]
    fn target_enumerator_finds_client_and_tools() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let mut names = vec!["client".to_string()];
        names.extend(tool_targets(&root));
        assert!(names.contains(&"client".to_string()));
        assert!(names.contains(&"res_packer".to_string()));
        assert!(names.contains(&"modeleditor".to_string()));
        assert!(
            !names.contains(&"common".to_string()),
            "shared lib excluded"
        );
        assert_eq!(names[0], "client");
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped in CI.
    ///
    /// The `reference-commit` row is excluded from both sides (the reference
    /// checkout's HEAD is a moving target, as in the other catalogs).
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let tools = root.join("programming/bigworld/tools");
        if !tools.is_dir() {
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
