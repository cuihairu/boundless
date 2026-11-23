//! Python callback fixture catalog (BW-00-04-02).
//!
//! Records the Python callbacks the reference server fires on its entities,
//! with the order they are documented in (the reference's own lifecycle
//! ordering within each source file), the callback's parameter list (the
//! `@param` fields) and whether the callback may be absent (the "If present"
//! / "not present" marker in the reference's callback documentation).
//!
//! The callbacks are the ones the server side declares, in the reference's
//! `/*~ callback <Type>.<name>` documentation blocks: the Base and Proxy
//! callbacks in `server/baseapp/` and the cell Entity callbacks in
//! `server/cellapp/entity.cpp`. Each block names the callback, its
//! component(s), its parameters and whether it is optional; the blocks are
//! read in file order, which is the order the reference documents them.
//!
//! The committed catalog (`cases/python_callbacks.txt`) is regenerated with
//! the `scan-python-callbacks` binary and re-parsed by the catalog test, so
//! the file and the scanner cannot drift.
//!
//! Catalog line grammar (tab-separated `item`, `value`, `source`):
//!
//! * `reference-commit` — the reference checkout's git HEAD.
//! * `cb.<n>.type` / `.name` / `.component` / `.sequence` / `.params` /
//!   `.ignorable` / `.source` — one indexed row per callback, in source-file
//!   order.
//! * `blocker.python-callback-capture` — why no live capture is recorded.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/python_callbacks.txt");

/// The scanned sources: the reference source file, in order.
const SOURCES: &[&str] = &[
    "programming/bigworld/server/baseapp/base.cpp",
    "programming/bigworld/server/baseapp/proxy.cpp",
    "programming/bigworld/server/cellapp/entity.cpp",
];

/// The named blocker: no live callback capture is recorded, because the
/// cluster cannot start here (BW-00-02-03) and loginapp.pubkey is absent
/// (BW-00-02-04). The recorded callbacks are the reference's own documented
/// callback contract (name, parameters, optionality) read from its source.
pub const CAPTURE_BLOCKER: &str = "python-callback-capture: no live callback capture is recorded — the full cluster cannot start on this machine (bwmachined and the full server configuration are absent, BW-00-02-03) and loginapp.pubkey is absent repo-wide (BW-00-02-04), so no entity script callback fires that could be captured; the recorded callbacks are the reference's own documented callback contract (name, parameters, optionality) read from its source, in the order it documents them";

/// One Python callback, as documented by the reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    /// The owning type (`Base`, `Entity`, `Proxy`).
    pub r#type: String,
    /// The callback name (`onLogOnAttempt`, ...).
    pub name: String,
    /// The component(s) the callback fires in (`base`, `cell`).
    pub component: String,
    /// The order the reference documents the callback in (1-based within the
    /// source file).
    pub sequence: u32,
    /// The parameter names, comma-joined (from the `@param` lines), or `-`.
    pub params: String,
    /// `yes` when the callback may be absent (the "If present" marker), else
    /// `no`.
    pub ignorable: String,
    /// The reference source file.
    pub source: String,
}

/// One parsed callback block.
struct Block {
    r#type: String,
    name: String,
    component: String,
    params: Vec<String>,
    ignorable: bool,
}

/// The `@components{ ... }` inside a callback block.
fn components(line: &str) -> Option<String> {
    let start = line.find("@components{")?;
    let rest = &line[start + "@components{".len()..];
    let end = rest.find('}')?;
    Some(rest[..end].trim().to_string())
}

/// True when the block's documentation marks the callback as optional ("if
/// present" / "not present"). Word-matched, so "representation" does not
/// trigger it.
fn mentions_presence(body: &str) -> bool {
    body.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("present"))
}

/// The `@param <name>` inside a callback block.
fn param_name(line: &str) -> Option<String> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix("@param")?;
    Some(rest.split_whitespace().next()?.to_string())
}

/// The `Type.name` or `Type name` a callback block header names.
fn block_header(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("/*")?;
    let rest = rest.trim_start_matches(['~', '\t', ' ']);
    let rest = rest.strip_prefix("callback")?;
    let rest = rest.trim_start();
    // `Type.name` or (a reference typo) `Type name`.
    let mut it = rest.splitn(2, '.');
    let type_name = it.next()?.trim();
    if let Some(rest) = it.next() {
        return Some((type_name.to_string(), rest.trim().to_string()));
    }
    // Fall back to the whitespace-separated form.
    let mut it = rest.splitn(2, char::is_whitespace);
    let type_name = it.next()?.trim();
    let name = it.next()?.trim();
    if type_name.is_empty() || name.is_empty() {
        return None;
    }
    Some((type_name.to_string(), name.to_string()))
}

/// Parse one source file into its callback blocks, in file order.
fn parse_source(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cur: Option<Block> = None;
    for raw in text.lines() {
        if let Some(block) = cur.as_mut() {
            let line = raw.trim_start();
            if line.starts_with('*') || line.starts_with("/*") {
                let body = line.trim_start_matches('*').trim_start();
                if let Some(comp) = components(body) {
                    block.component = comp;
                }
                if let Some(p) = param_name(body) {
                    block.params.push(p);
                }
                if mentions_presence(body) {
                    block.ignorable = true;
                }
            }
            if raw.contains("*/") {
                blocks.push(cur.take().expect("open callback block"));
            }
            continue;
        }
        if let Some((r#type, name)) = block_header(raw) {
            cur = Some(Block {
                r#type,
                name,
                component: String::new(),
                params: Vec::new(),
                ignorable: false,
            });
        }
    }
    // A block without a closing `*/` is malformed; drop it rather than emit a
    // half-documented callback.
    blocks
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

/// Scan a reference checkout's Python callback fixtures.
pub fn scan_reference(root: &Path) -> Vec<CallbackFact> {
    let mut facts = Vec::new();

    facts.push(CallbackFact {
        item: "reference-commit".to_string(),
        value: git_head(root),
        source: "git rev-parse HEAD".to_string(),
    });

    let mut n = 0usize;
    for source in SOURCES {
        let path = root.join(source);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("cannot read reference source {path:?}"));
        let blocks = parse_source(&text);
        for (i, block) in blocks.iter().enumerate() {
            n += 1;
            let params = if block.params.is_empty() {
                "-".to_string()
            } else {
                block.params.join(",")
            };
            for (suffix, value, fact_source) in [
                (
                    "type",
                    block.r#type.clone(),
                    "documented owning type".to_string(),
                ),
                ("name", block.name.clone(), source.to_string()),
                (
                    "component",
                    if block.component.is_empty() {
                        "-".to_string()
                    } else {
                        block.component.clone()
                    },
                    "@components{...}".to_string(),
                ),
                (
                    "sequence",
                    (i + 1).to_string(),
                    "source-file order".to_string(),
                ),
                ("params", params, "@param fields".to_string()),
                (
                    "ignorable",
                    if block.ignorable { "yes" } else { "no" }.to_string(),
                    "If present / not present marker".to_string(),
                ),
                (
                    "source",
                    source.to_string(),
                    "reference source tree".to_string(),
                ),
            ] {
                facts.push(CallbackFact {
                    item: format!("cb.{n}.{suffix}"),
                    value,
                    source: fact_source,
                });
            }
        }
    }

    facts.push(CallbackFact {
        item: "blocker.python-callback-capture".to_string(),
        value: CAPTURE_BLOCKER.to_string(),
        source: "runner limitation".to_string(),
    });

    facts
}

/// One callback-fact row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackFact {
    /// The fact name (`reference-commit`, `cb.1.name`, ...).
    pub item: String,
    /// The recorded value.
    pub value: String,
    /// The file or probe the value came from.
    pub source: String,
}

/// Render the scan into the tab-separated catalog text.
pub fn render(facts: &[CallbackFact]) -> String {
    let mut out = String::new();
    out.push_str("item\tvalue\tsource\n");
    for f in facts {
        let _ = writeln!(out, "{}\t{}\t{}", f.item, f.value, f.source);
    }
    out
}

/// Parse the catalog text back into [`CallbackFact`]s.
pub fn parse_catalog(text: &str) -> Vec<CallbackFact> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(CallbackFact {
            item: f.next().unwrap_or("").to_string(),
            value: f.next().unwrap_or("-").to_string(),
            source: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<CallbackFact> {
    parse_catalog(CATALOG_TEXT)
}

/// The committed callbacks, in source-file order.
pub fn callbacks() -> Vec<Callback> {
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
        .filter(|f| f.item.ends_with(".name") && f.item.starts_with("cb."))
        .count();
    (1..=count)
        .map(|i| Callback {
            r#type: get(&format!("cb.{i}.type")),
            name: get(&format!("cb.{i}.name")),
            component: get(&format!("cb.{i}.component")),
            sequence: get(&format!("cb.{i}.sequence")).parse().unwrap_or(0),
            params: get(&format!("cb.{i}.params")),
            ignorable: get(&format!("cb.{i}.ignorable")),
            source: get(&format!("cb.{i}.source")),
        })
        .collect()
}

/// The committed named blocker for the Python callback capture.
pub fn blocker() -> String {
    catalog()
        .into_iter()
        .find(|f| f.item == "blocker.python-callback-capture")
        .map(|f| f.value)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog records each callback with its type, order,
    /// parameters and optionality.
    #[test]
    fn committed_catalog_records_callbacks() {
        let callbacks = callbacks();
        assert!(callbacks.len() > 30, "the server callback set is large");

        let logon = callbacks
            .iter()
            .find(|c| c.name == "onLogOnAttempt")
            .expect("onLogOnAttempt");
        assert_eq!(logon.r#type, "Proxy");
        assert_eq!(logon.component, "base");
        assert_eq!(logon.params, "ip,port,logOnData");
        assert_eq!(logon.ignorable, "yes");

        let on_timer = callbacks
            .iter()
            .find(|c| c.name == "onTimer" && c.r#type == "Base")
            .expect("Base.onTimer");
        assert_eq!(on_timer.params, "timerHandle,userData");
        assert_eq!(on_timer.ignorable, "no");

        // The cell-side lifecycle is documented in order.
        let cell = callbacks
            .iter()
            .filter(|c| c.source.ends_with("entity.cpp"))
            .collect::<Vec<_>>();
        assert!(!cell.is_empty());
        assert!(cell.iter().all(|c| c.r#type == "Entity"));

        // Every callback has a name, an order and a source.
        for c in &callbacks {
            assert!(!c.name.is_empty(), "{c:?}");
            assert!(!c.r#type.is_empty(), "{c:?}");
            assert!(c.sequence >= 1, "{c:?}");
            assert!(c.ignorable == "yes" || c.ignorable == "no", "{c:?}");
            assert!(!c.source.is_empty(), "{c:?}");
        }

        let blocker = blocker();
        assert!(blocker.contains("python-callback-capture"), "{blocker}");
        assert!(blocker.contains("BW-00-02-03"), "{blocker}");
    }

    /// The cell entity lifecycle order is as the reference documents it.
    #[test]
    fn cell_lifecycle_sequence_matches_the_reference() {
        let callbacks = callbacks();
        let entered_cell = callbacks
            .iter()
            .find(|c| c.name == "onEnteredCell")
            .expect("onEnteredCell");
        let left_cell = callbacks
            .iter()
            .find(|c| c.name == "onLeftCell")
            .expect("onLeftCell");
        assert!(
            entered_cell.sequence < left_cell.sequence,
            "entered before left"
        );
    }

    /// The catalog round-trips through render and parse.
    #[test]
    fn render_parse_round_trip() {
        assert_eq!(parse_catalog(&render(&catalog())), catalog());
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped in CI.
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let first = root.join(SOURCES[0]);
        if !first.is_file() {
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
