//! Standalone-cluster startup baseline (BW-00-02-03).
//!
//! Records what a standalone BigWorld cluster start looks like in this
//! checkout — the topology (the eight server components and their built
//! binaries), the configuration chain (the fantasydemo `bw.xml` the standalone
//! cluster is configured with, and the full `development_defaults.xml` that is
//! absent from the repository), and the captured startup-attempt log (a
//! bounded run of `baseapp` against that config). The committed catalog
//! (`cases/startup_baseline.txt`) is regenerated with the
//! `scan-startup-baseline` binary and re-parsed by the catalog test, so the
//! file and the scanner cannot drift.
//!
//! Topology and config are re-derived from the live reference checkout; the
//! log is a frozen artifact (`cases/startup_baseline/log.txt`), re-emitted
//! verbatim by the scanner.
//!
//! Catalog line grammar (tab-separated `item`, `value`, `source`):
//!
//! * `reference-commit` — the reference checkout's git HEAD.
//! * `topology.<i>.role` / `.source-dir` / `.binary` / `.bytes` — one indexed
//!   row per server component; `.bytes` is the built binary's size, `-` when
//!   the binary is absent.
//! * `config.<key>` — the standalone config chain (res path, `bw.xml`, its
//!   settings, and `development-defaults-xml` = `ABSENT (not in repository)`).
//! * `log.<i>` — one fact per line of the frozen startup-attempt log.
//! * `blocker.standalone-start` — the named reason a full clean start cannot
//!   complete on this runner.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/startup_baseline.txt");

/// The frozen startup-attempt log, committed as captured (bounded `baseapp`
/// run against the fantasydemo config).
const LOG_TEXT: &str = include_str!("../cases/startup_baseline/log.txt");

/// The reference server components, in canonical start order.
const ROLES: &[&str] = &[
    "baseapp",
    "baseappmgr",
    "cellapp",
    "cellappmgr",
    "dbapp",
    "dbappmgr",
    "loginapp",
    "reviver",
];

/// The named blocker: the captured start gets as far as the config chain and
/// interface discovery, then stops because bwmachined, the full config and a
/// database are not provisioned here.
pub const STARTUP_BLOCKER: &str = "standalone-start: the captured baseapp run shows the cluster config chain loads (res path + server/bw.xml), then interface discovery times out because bwmachined is not running, so baseapp falls back to the default-route interface and continues on config defaults; a full clean start needs bwmachined running, the full server/development_defaults.xml (absent from this repository) and a provisioned database schema — none is configured here, so this is the recorded startup baseline, not a completed start";

/// One startup-baseline fact row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupFact {
    /// The fact name (`reference-commit`, `topology.1.role`, ...).
    pub item: String,
    /// The recorded value.
    pub value: String,
    /// The file the value came from (or the probe/command, for the commit and
    /// the log).
    pub source: String,
}

/// One server component of the cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// The role name (`baseapp`, `cellapp`, ...).
    pub role: String,
    /// The component's source directory in the reference checkout.
    pub source_dir: String,
    /// The built binary's path.
    pub binary: String,
    /// The binary's size in bytes; `0` when it is absent.
    pub bytes: u64,
}

/// One configuration fact of the standalone cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFact {
    /// The setting key (`personality`, `baseApp.archivePeriod`, ...).
    pub key: String,
    /// The recorded value.
    pub value: String,
    /// The config file the value came from.
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

/// The trimmed text between `<tag>` and `</tag>` in an XML snippet, or `-`.
fn xml_value(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    xml.split(&open)
        .nth(1)
        .and_then(|rest| rest.split(&close).next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("-")
        .to_string()
}

/// Scan a reference checkout's standalone-cluster startup baseline.
pub fn scan_reference(root: &Path) -> Vec<StartupFact> {
    let bw_source = root.join("programming").join("bigworld");
    let bin_dir = root.join("game/bin/server/el7/server");
    let bw_xml = root.join("programming/fantasydemo/res/server/bw.xml");
    let xml = std::fs::read_to_string(&bw_xml).unwrap_or_default();
    let mut facts = Vec::new();

    // Reference revision.
    facts.push(StartupFact {
        item: "reference-commit".to_string(),
        value: git_head(root),
        source: "git rev-parse HEAD".to_string(),
    });

    // Topology: one indexed row per server component.
    for (i, role) in ROLES.iter().enumerate() {
        let idx = i + 1;
        facts.push(StartupFact {
            item: format!("topology.{idx}.role"),
            value: role.to_string(),
            source: "programming/bigworld/server".to_string(),
        });
        facts.push(StartupFact {
            item: format!("topology.{idx}.source-dir"),
            value: format!("programming/bigworld/server/{role}"),
            source: "programming/bigworld/server".to_string(),
        });
        facts.push(StartupFact {
            item: format!("topology.{idx}.binary"),
            value: format!("game/bin/server/el7/server/{role}"),
            source: "game/bin/server/el7/server".to_string(),
        });
        let bytes = bin_dir
            .join(role)
            .metadata()
            .ok()
            .filter(|md| md.is_file())
            .map(|md| md.len())
            .unwrap_or(0);
        facts.push(StartupFact {
            item: format!("topology.{idx}.bytes"),
            value: if bytes == 0 {
                "-".to_string()
            } else {
                bytes.to_string()
            },
            source: "game/bin/server/el7/server (size)".to_string(),
        });
    }

    // Config: the standalone config chain.
    let res = "programming/fantasydemo/res";
    let bw_xml_rel = "programming/fantasydemo/res/server/bw.xml";
    let defaults = bw_source.join("res/server/development_defaults.xml");
    for (key, value, source) in [
        (
            "config.res-path",
            res.to_string(),
            "command line (--res)".to_string(),
        ),
        (
            "config.bw-xml",
            bw_xml_rel.to_string(),
            "BWConfig::init (Adding server/bw.xml)".to_string(),
        ),
        (
            "config.personality",
            xml_value(&xml, "personality"),
            bw_xml_rel.to_string(),
        ),
        (
            "config.baseApp.archivePeriod",
            xml_value(&xml, "archivePeriod"),
            bw_xml_rel.to_string(),
        ),
        (
            "config.billingSystem.shouldAcceptUnknownUsers",
            xml_value(&xml, "shouldAcceptUnknownUsers"),
            bw_xml_rel.to_string(),
        ),
        (
            "config.billingSystem.entityTypeForUnknownUsers",
            xml_value(&xml, "entityTypeForUnknownUsers"),
            bw_xml_rel.to_string(),
        ),
        (
            "config.development-defaults-xml",
            if defaults.is_file() {
                defaults.display().to_string()
            } else {
                "ABSENT (not in repository)".to_string()
            },
            "programming/bigworld/res/server/development_defaults.xml".to_string(),
        ),
    ] {
        facts.push(StartupFact {
            item: key.to_string(),
            value,
            source,
        });
    }

    // Log: the frozen startup-attempt artifact, re-emitted verbatim.
    for (i, line) in LOG_TEXT.lines().enumerate() {
        facts.push(StartupFact {
            item: format!("log.{}", i + 1),
            value: line.to_string(),
            source: "baseapp --res programming/fantasydemo/res (bounded 9s startup attempt)"
                .to_string(),
        });
    }

    // Blocker: the named reason a full clean start cannot complete here.
    facts.push(StartupFact {
        item: "blocker.standalone-start".to_string(),
        value: STARTUP_BLOCKER.to_string(),
        source: "startup-attempt observation".to_string(),
    });

    facts
}

/// Render the scan into the tab-separated catalog text.
pub fn render(facts: &[StartupFact]) -> String {
    let mut out = String::new();
    out.push_str("item\tvalue\tsource\n");
    for f in facts {
        let _ = writeln!(out, "{}\t{}\t{}", f.item, f.value, f.source);
    }
    out
}

/// Parse the catalog text back into [`StartupFact`]s.
pub fn parse_catalog(text: &str) -> Vec<StartupFact> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(StartupFact {
            item: f.next().unwrap_or("").to_string(),
            value: f.next().unwrap_or("-").to_string(),
            source: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<StartupFact> {
    parse_catalog(CATALOG_TEXT)
}

/// The committed topology: one [`Component`] per server component, in the
/// catalog's indexed order.
pub fn components() -> Vec<Component> {
    let facts = catalog();
    let get = |item: &str| -> String {
        facts
            .iter()
            .find(|f| f.item == item)
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for i in 1..=ROLES.len() {
        let bytes = get(&format!("topology.{i}.bytes"));
        out.push(Component {
            role: get(&format!("topology.{i}.role")),
            source_dir: get(&format!("topology.{i}.source-dir")),
            binary: get(&format!("topology.{i}.binary")),
            bytes: bytes.parse().unwrap_or(0),
        });
    }
    out
}

/// The committed configuration facts, in the catalog's order.
pub fn config_facts() -> Vec<ConfigFact> {
    catalog()
        .into_iter()
        .filter_map(|f| {
            f.item.strip_prefix("config.").map(|key| ConfigFact {
                key: key.to_string(),
                value: f.value,
                source: f.source,
            })
        })
        .collect()
}

/// The committed frozen startup-attempt log lines.
pub fn log_lines() -> Vec<String> {
    catalog()
        .into_iter()
        .filter_map(|f| f.item.strip_prefix("log.").map(|_| f.value))
        .collect()
}

/// The committed named blocker for a full clean start.
pub fn blocker() -> String {
    catalog()
        .into_iter()
        .find(|f| f.item == "blocker.standalone-start")
        .map(|f| f.value)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog retains the topology, config and log the
    /// acceptance demands: eight built components, the fantasydemo config
    /// chain (with the full defaults absent), and the real startup log.
    #[test]
    fn committed_catalog_retains_topology_config_and_log() {
        let comps = components();
        assert_eq!(comps.len(), ROLES.len(), "one component per server role");
        for comp in &comps {
            assert!(
                comp.source_dir.ends_with(&format!("/{}", comp.role)),
                "component source dir matches its role: {comp:?}"
            );
            assert!(
                comp.binary.ends_with(&comp.role),
                "component binary matches its role: {comp:?}"
            );
            assert!(
                comp.bytes > 0,
                "the built binary is present and non-empty: {comp:?}"
            );
        }

        let cfg = config_facts();
        let get = |key: &str| {
            cfg.iter()
                .find(|f| f.key == key)
                .unwrap_or_else(|| panic!("{key} missing from config"))
                .value
                .clone()
        };
        assert_eq!(get("res-path"), "programming/fantasydemo/res");
        assert_eq!(get("bw-xml"), "programming/fantasydemo/res/server/bw.xml");
        assert_eq!(get("personality"), "fantasydemo");
        assert_eq!(get("baseApp.archivePeriod"), "0");
        assert_eq!(get("billingSystem.shouldAcceptUnknownUsers"), "true");
        assert_eq!(get("billingSystem.entityTypeForUnknownUsers"), "Avatar");
        assert!(get("development-defaults-xml").contains("ABSENT"));

        let log = log_lines();
        assert!(!log.is_empty(), "the startup log is retained");
        assert!(
            log[0].contains("Added res path"),
            "log opens with the res path: {}",
            log[0]
        );
        let joined = log.join("\n");
        assert!(joined.contains("bw.xml"), "config chain logged");
        assert!(
            joined.contains("bwmachined"),
            "interface discovery attempted"
        );
        assert!(joined.contains("timed out"), "machined absence observed");
        assert!(joined.contains("bound to all interfaces"), "fallback bind");

        let blocker = blocker();
        assert!(blocker.contains("standalone-start"), "{blocker}");
        assert!(blocker.contains("bwmachined"), "{blocker}");
        assert!(blocker.contains("development_defaults.xml"), "{blocker}");
        assert!(blocker.contains("database"), "{blocker}");
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

    /// `xml_value` extracts the trimmed text between a tag pair.
    #[test]
    fn xml_value_extracts_trimmed_text() {
        let xml = "<personality>\tfantasydemo\t</personality>";
        assert_eq!(xml_value(xml, "personality"), "fantasydemo");
        assert_eq!(xml_value(xml, "absent"), "-");
        assert_eq!(xml_value("<a></a>", "a"), "-");
    }

    /// Scanning a minimal synthetic reference tree records the topology's
    /// indexed rows, the config chain and the frozen log.
    #[test]
    fn scanning_a_synthetic_reference_records_facts() {
        let root =
            std::env::temp_dir().join(format!("bw00-startup-baseline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let server = root.join("programming/bigworld/server");
        let bin = root.join("game/bin/server/el7/server");
        let res = root.join("programming/fantasydemo/res/server");
        std::fs::create_dir_all(&server).expect("mkdir server");
        std::fs::create_dir_all(&bin).expect("mkdir bin");
        std::fs::create_dir_all(&res).expect("mkdir res");
        std::fs::write(bin.join("baseapp"), "x".repeat(100)).expect("write baseapp");
        std::fs::write(
            res.join("bw.xml"),
            "<root><personality>\tfantasydemo\t</personality><baseApp><archivePeriod>\t0\t</archivePeriod></baseApp></root>",
        )
        .expect("write bw.xml");

        let facts = scan_reference(&root);
        let get = |item: &str| {
            facts
                .iter()
                .find(|f| f.item == item)
                .unwrap_or_else(|| panic!("{item} missing"))
                .value
                .clone()
        };
        assert_eq!(get("topology.1.role"), "baseapp");
        assert_eq!(get("topology.1.bytes"), "100");
        assert_eq!(get("topology.8.role"), "reviver");
        assert_eq!(get("topology.8.bytes"), "-", "absent binary recorded as -");
        assert_eq!(get("config.personality"), "fantasydemo");
        assert_eq!(get("config.baseApp.archivePeriod"), "0");
        assert!(get("config.development-defaults-xml").contains("ABSENT"));
        assert_eq!(get("log.1"), LOG_TEXT.lines().next().unwrap_or(""));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped in CI.
    ///
    /// The `reference-commit` row is excluded from both sides (the reference
    /// checkout's HEAD is a moving target, as in the runtime-environment
    /// catalog).
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let bw_xml = root.join("programming/fantasydemo/res/server/bw.xml");
        if !bw_xml.is_file() {
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
