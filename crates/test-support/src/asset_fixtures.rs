//! Asset fixture catalog (BW-00-04-03).
//!
//! Records the tracked resource, entity and database fixtures the reference
//! repository carries, each one traceable to the reference version and the
//! input it came from:
//!
//! * **Version** — every row hangs off the `reference-commit` row, the
//!   checkout's git HEAD when the catalog was scanned.
//! * **Input** — each fixture row carries the reference source path
//!   (`file.<tree>.<n>.input`, `tree.<root>.root`), and the fantasydemo
//!   fixture tree additionally names the committed generator that reproduces
//!   it byte-for-byte (`gen.generator`), which the
//!   `generator_regenerates_the_fantasydemo_tree` test re-runs to prove.
//!
//! Two tracked fixture trees are recorded, both under `programming/`:
//!
//! 1. `programming/fantasydemo/res` — the fantasydemo placeholder asset tree
//!    (21 files: `server/bw.xml`, `scripts/db.xml`, `entities.xml`,
//!    `entity_defs/*.def`, `entity_defs/alias.xml` and one script per entity
//!    part in `scripts/{base,cell,client}/`), written byte-for-byte by
//!    `programming/bigworld/scripts/generate-fantasydemo-assets.py`.
//! 2. `programming/bigworld/examples/client_integration/python/simple/res` —
//!    the client-integration example fixture tree (24 files, no generator).
//!
//! Untracked, git-ignored trees (e.g. `game/res/...`, build-time
//! `copy-res/` output) are deliberately excluded: they are local build
//! artifacts with no committed version, so they cannot be traced to the
//! reference.
//!
//! Per fixture file the catalog records kind (`resource`, `entity`,
//! `database`), the reference input path, the byte size and the SHA-256 of
//! the file content. Per `.def` file it also records the entity's semantic
//! shape (property and method counts across `Properties`, `ClientMethods`,
//! `CellMethods`, `BaseMethods`, plus the `Exposed` marker count), read from
//! the reference's own definition files. Per tree it records the file count,
//! total bytes, a tree-shape digest (SHA-256 over the sorted
//! `relpath\tbytes` list), the entity list from `entities.xml`, and the
//! database facts from `db.xml` (log-on mapping entries and seeded row
//! types).
//!
//! The committed catalog (`cases/asset_fixtures.txt`) is regenerated with the
//! `scan-asset-fixtures` binary and re-parsed by the catalog test, so the
//! file and the scanner cannot drift.
//!
//! Catalog line grammar (tab-separated `item`, `value`, `source`):
//!
//! * `reference-commit` — the reference checkout's git HEAD.
//! * `gen.generator` / `gen.bytes` / `gen.sha256` — the committed
//!   generator that writes the fantasydemo tree.
//! * `tree.<id>.root` / `.generated` / `.generator` / `.files` / `.bytes` /
//!   `.digest` / `.entities` / `.db-logons` / `.db-rowtypes` — one indexed
//!   row group per tracked fixture tree.
//! * `file.<id>.<n>.kind` / `.input` / `.bytes` / `.sha256` — one indexed row
//!   group per fixture file, in sorted path order.
//! * `entity.<id>.<Name>.def` / `.properties` / `.clientmethods` /
//!   `.cellmethods` / `.basemethods` / `.exposed` — one indexed row group per
//!   entity definition.
//! * `blocker.live-asset-capture` — why no live server/database capture is
//!   recorded.

use std::fmt::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/asset_fixtures.txt");

/// The committed generator that writes the fantasydemo fixture tree, and the
/// generator's own input anchor.
pub const GENERATOR: &str = "programming/bigworld/scripts/generate-fantasydemo-assets.py";

/// The tracked fixture trees, in catalog order.
const TREES: &[(&str, &str, bool)] = &[
    ("fantasydemo", "programming/fantasydemo/res", true),
    (
        "client-integration-example",
        "programming/bigworld/examples/client_integration/python/simple/res",
        false,
    ),
];

/// The named blocker: no live server or database capture is recorded, because
/// the cluster cannot start here (BW-00-02-03) and loginapp.pubkey is absent
/// (BW-00-02-04). The recorded fixtures are the reference's own committed
/// asset trees; the fantasydemo tree is additionally pinned to its committed
/// generator, which reproduces it byte-for-byte.
pub const CAPTURE_BLOCKER: &str = "live-asset-capture: no live server or database capture is recorded — the full cluster cannot start on this machine (bwmachined and the full server configuration are absent, BW-00-02-03) and loginapp.pubkey is absent repo-wide (BW-00-02-04), so no asset tree is loaded or database seeded that could be captured; the recorded fixtures are the reference's own committed tracked asset trees, each pinned to its input path and content digest, and the fantasydemo tree is further pinned to the committed generator that reproduces it byte-for-byte";

/// One tracked fixture tree, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// The tree's slug (`fantasydemo`, ...).
    pub id: String,
    /// The tree's root path within the reference checkout.
    pub root: String,
    /// `yes` when a committed generator writes the tree.
    pub generated: String,
    /// The committed generator path, or `-`.
    pub generator: String,
    /// The number of tracked files under the tree.
    pub files: u32,
    /// The total bytes of all tracked files.
    pub bytes: u64,
    /// A tree-shape digest: SHA-256 over the sorted `relpath\tbytes` list.
    pub digest: String,
    /// The entity names from `entities.xml`, comma-joined, or `-`.
    pub entities: String,
    /// The log-on mapping entries in `db.xml`, or `-`.
    pub db_logons: String,
    /// The seeded row types in `db.xml`, comma-joined, or `-`.
    pub db_rowtypes: String,
}

/// One tracked fixture file, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureFile {
    /// The owning tree's slug.
    pub tree: String,
    /// The kind: `resource`, `entity` or `database`.
    pub kind: String,
    /// The reference input path (checkout-relative).
    pub input: String,
    /// The file's byte size.
    pub bytes: u64,
    /// The SHA-256 of the file content, lowercase hex.
    pub sha256: String,
}

/// One entity definition's semantic shape, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDef {
    /// The owning tree's slug.
    pub tree: String,
    /// The entity name (`Avatar`, ...).
    pub name: String,
    /// The `.def` input path (checkout-relative).
    pub def: String,
    /// The number of `Properties` entries.
    pub properties: u32,
    /// The number of `ClientMethods` entries.
    pub client_methods: u32,
    /// The number of `CellMethods` entries.
    pub cell_methods: u32,
    /// The number of `BaseMethods` entries.
    pub base_methods: u32,
    /// The number of `<Exposed/>` markers across all method groups.
    pub exposed: u32,
}

/// One catalog-fact row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// The fact name (`reference-commit`, `file.fantasydemo.1.sha256`, ...).
    pub item: String,
    /// The recorded value.
    pub value: String,
    /// The file, probe or rule the value came from.
    pub source: String,
}

// ---------------------------------------------------------------------------
// SHA-256 (self-contained so the crate's Cargo.lock stays untouched)
// ---------------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 of `data`, lowercase hex (FIPS 180-4).
fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

// ---------------------------------------------------------------------------
// Reference parsers
// ---------------------------------------------------------------------------

/// The fixture kind a tree-relative path belongs to.
fn classify(rel: &str) -> &'static str {
    if rel == "scripts/db.xml" {
        "database"
    } else if rel == "server/bw.xml" {
        "resource"
    } else if rel == "scripts/entities.xml"
        || rel.starts_with("scripts/entity_defs/")
        || rel.starts_with("scripts/base/")
        || rel.starts_with("scripts/cell/")
        || rel.starts_with("scripts/client/")
        || rel.starts_with("scripts/common/")
    {
        "entity"
    } else {
        "resource"
    }
}

/// The entity names an `entities.xml` lists under `ClientServerEntities`,
/// in document order.
fn entities_from_xml(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_cs = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "<ClientServerEntities>" {
            in_cs = true;
            continue;
        }
        if trimmed == "</ClientServerEntities>" {
            in_cs = false;
            continue;
        }
        if !in_cs {
            continue;
        }
        // A self-closing or open entry tag, e.g. `<Guard/>` or `<Account>`.
        if let Some(inner) = trimmed.strip_prefix('<').filter(|i| !i.starts_with('/')) {
            let name: String = inner
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

/// The number of immediate children of `<section>`, counting the open tags
/// at the entry depth (the shallowest indented tag inside the section, which
/// the reference's `.def` files indent one level below the section tag).
/// Grandchildren (`<Type>`, `<Exposed/>`) are deeper-indented and excluded.
fn section_child_count(text: &str, section: &str) -> u32 {
    let open = format!("<{section}>");
    let close = format!("</{section}>");
    let mut indents = Vec::new();
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == open {
            in_section = true;
            continue;
        }
        if trimmed == close {
            in_section = false;
            continue;
        }
        if !in_section {
            continue;
        }
        if trimmed.starts_with('<') && !trimmed.starts_with("</") && !trimmed.starts_with("<!--") {
            indents.push(line.len() - line.trim_start().len());
        }
    }
    match indents.iter().min() {
        Some(min) => indents.iter().filter(|i| *i == min).count() as u32,
        None => 0,
    }
}

/// The number of `<Exposed/>` markers across a `.def` file.
fn exposed_count(text: &str) -> u32 {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed == "<Exposed/>"
                || trimmed.starts_with("<Exposed ")
                || trimmed.starts_with("<Exposed>")
        })
        .count() as u32
}

/// The seeded row types a `db.xml` carries (depth-1 entity tags outside
/// `_BigWorldInfo`), in document order. Depth 1 is one leading tab (or four
/// leading spaces); the rows' own fields sit deeper and are excluded.
fn db_rowtypes(text: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let tabs = line.chars().take_while(|c| *c == '\t').count();
        let spaces = line.chars().take_while(|c| *c == ' ').count();
        let depth1 = (tabs == 1 && spaces == 0) || (spaces == 4 && tabs == 0);
        if !depth1 {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with('<') && !trimmed.starts_with("</") && !trimmed.starts_with("<!--") {
            let name: String = trimmed
                .trim_start_matches('<')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name == "_BigWorldInfo" || name == "db.xml" {
                continue;
            }
            if !name.is_empty() {
                rows.push(name);
            }
        }
    }
    rows
}

/// The log-on mapping entries a `db.xml` carries.
fn db_logons(text: &str) -> u32 {
    text.lines()
        .filter(|line| line.trim().starts_with("<logOnName>"))
        .count() as u32
}

/// The sorted tracked files under a tree root: `(relpath, absolute path)`.
/// Python bytecode cache (`__pycache__/`, `*.pyc`) is excluded: it is
/// ignored build output, not a fixture, and it would otherwise drift the
/// scan when the tree has been loaded.
fn tree_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), root.to_path_buf())];
    while let Some((rel, dir)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "__pycache__" || name.ends_with(".pyc") {
                continue;
            }
            let child_rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if path.is_dir() {
                stack.push((child_rel, path));
            } else {
                out.push((child_rel, path));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

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

/// Scan a reference checkout's tracked asset fixtures.
pub fn scan_reference(root: &Path) -> Vec<Fact> {
    let mut facts = Vec::new();

    facts.push(Fact {
        item: "reference-commit".to_string(),
        value: git_head(root),
        source: "git rev-parse HEAD".to_string(),
    });

    // The committed generator: the input anchor for the fantasydemo tree.
    let gen_path = root.join(GENERATOR);
    if let Ok(gen_bytes) = std::fs::read(&gen_path) {
        facts.push(Fact {
            item: "gen.generator".to_string(),
            value: GENERATOR.to_string(),
            source: "reference source tree".to_string(),
        });
        facts.push(Fact {
            item: "gen.bytes".to_string(),
            value: gen_bytes.len().to_string(),
            source: "reference source tree".to_string(),
        });
        facts.push(Fact {
            item: "gen.sha256".to_string(),
            value: sha256_hex(&gen_bytes),
            source: "reference source tree".to_string(),
        });
    }

    for (id, tree_root, generated) in TREES {
        let tree_dir = root.join(tree_root);
        let files = tree_files(&tree_dir);
        let mut total_bytes = 0u64;
        let mut shape = String::new();
        for (rel, path) in &files {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            total_bytes += size;
            let _ = writeln!(shape, "{rel}\t{size}");
        }
        let digest = sha256_hex(shape.as_bytes());

        facts.push(Fact {
            item: format!("tree.{id}.root"),
            value: (*tree_root).to_string(),
            source: "reference source tree".to_string(),
        });
        facts.push(Fact {
            item: format!("tree.{id}.generated"),
            value: if *generated { "yes" } else { "no" }.to_string(),
            source: "generator comment / git history".to_string(),
        });
        facts.push(Fact {
            item: format!("tree.{id}.generator"),
            value: if *generated {
                GENERATOR.to_string()
            } else {
                "-".to_string()
            },
            source: "generator anchor".to_string(),
        });
        facts.push(Fact {
            item: format!("tree.{id}.files"),
            value: files.len().to_string(),
            source: "reference source tree".to_string(),
        });
        facts.push(Fact {
            item: format!("tree.{id}.bytes"),
            value: total_bytes.to_string(),
            source: "reference source tree".to_string(),
        });
        facts.push(Fact {
            item: format!("tree.{id}.digest"),
            value: digest,
            source: "sha256 of sorted relpath/bytes list".to_string(),
        });

        // The tree-level facts: the entity list and the database contents.
        let entities_xml =
            std::fs::read_to_string(tree_dir.join("scripts/entities.xml")).unwrap_or_default();
        let entities = entities_from_xml(&entities_xml);
        facts.push(Fact {
            item: format!("tree.{id}.entities"),
            value: if entities.is_empty() {
                "-".to_string()
            } else {
                entities.join(",")
            },
            source: "scripts/entities.xml".to_string(),
        });
        let db_xml = std::fs::read_to_string(tree_dir.join("scripts/db.xml")).unwrap_or_default();
        let logons = db_logons(&db_xml);
        facts.push(Fact {
            item: format!("tree.{id}.db-logons"),
            value: if logons == 0 {
                "-".to_string()
            } else {
                logons.to_string()
            },
            source: "scripts/db.xml".to_string(),
        });
        let rows = db_rowtypes(&db_xml);
        facts.push(Fact {
            item: format!("tree.{id}.db-rowtypes"),
            value: if rows.is_empty() {
                "-".to_string()
            } else {
                rows.join(",")
            },
            source: "scripts/db.xml".to_string(),
        });

        // Per-file rows, in sorted path order.
        for (n, (rel, path)) in files.iter().enumerate() {
            let n = n + 1;
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let content = std::fs::read(path).unwrap_or_default();
            let input = format!("{tree_root}/{rel}");
            facts.push(Fact {
                item: format!("file.{id}.{n}.kind"),
                value: classify(rel).to_string(),
                source: "path classification".to_string(),
            });
            facts.push(Fact {
                item: format!("file.{id}.{n}.input"),
                value: input.clone(),
                source: "reference source tree".to_string(),
            });
            facts.push(Fact {
                item: format!("file.{id}.{n}.bytes"),
                value: size.to_string(),
                source: "reference source tree".to_string(),
            });
            facts.push(Fact {
                item: format!("file.{id}.{n}.sha256"),
                value: sha256_hex(&content),
                source: "reference source tree".to_string(),
            });

            // Per-entity rows for each `.def` file.
            if rel.ends_with(".def") {
                let name = rel
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(".def")
                    .to_string();
                let def = String::from_utf8_lossy(&content);
                for (suffix, value, source) in [
                    ("def", input, "reference source tree".to_string()),
                    (
                        "properties",
                        section_child_count(&def, "Properties").to_string(),
                        "entity_defs/.def".to_string(),
                    ),
                    (
                        "clientmethods",
                        section_child_count(&def, "ClientMethods").to_string(),
                        "entity_defs/.def".to_string(),
                    ),
                    (
                        "cellmethods",
                        section_child_count(&def, "CellMethods").to_string(),
                        "entity_defs/.def".to_string(),
                    ),
                    (
                        "basemethods",
                        section_child_count(&def, "BaseMethods").to_string(),
                        "entity_defs/.def".to_string(),
                    ),
                    (
                        "exposed",
                        exposed_count(&def).to_string(),
                        "entity_defs/.def".to_string(),
                    ),
                ] {
                    facts.push(Fact {
                        item: format!("entity.{id}.{name}.{suffix}"),
                        value,
                        source,
                    });
                }
            }
        }
    }

    facts.push(Fact {
        item: "blocker.live-asset-capture".to_string(),
        value: CAPTURE_BLOCKER.to_string(),
        source: "runner limitation".to_string(),
    });

    facts
}

// ---------------------------------------------------------------------------
// Render / parse / committed accessors
// ---------------------------------------------------------------------------

/// Render the scan into the tab-separated catalog text.
pub fn render(facts: &[Fact]) -> String {
    let mut out = String::new();
    out.push_str("item\tvalue\tsource\n");
    for f in facts {
        let _ = writeln!(out, "{}\t{}\t{}", f.item, f.value, f.source);
    }
    out
}

/// Parse the catalog text back into [`Fact`]s.
pub fn parse_catalog(text: &str) -> Vec<Fact> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(Fact {
            item: f.next().unwrap_or("").to_string(),
            value: f.next().unwrap_or("-").to_string(),
            source: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<Fact> {
    parse_catalog(CATALOG_TEXT)
}

/// The committed facts as [`Fact`] rows, in catalog order.
pub fn facts() -> Vec<Fact> {
    catalog()
}

/// The committed tracked fixture trees, in catalog order.
pub fn trees() -> Vec<Tree> {
    let facts = catalog();
    let get = |item: &str| -> String {
        facts
            .iter()
            .find(|f| f.item == item)
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    (TREES.iter().map(|(id, _, _)| *id))
        .map(|id| Tree {
            id: id.to_string(),
            root: get(&format!("tree.{id}.root")),
            generated: get(&format!("tree.{id}.generated")),
            generator: get(&format!("tree.{id}.generator")),
            files: get(&format!("tree.{id}.files")).parse().unwrap_or(0),
            bytes: get(&format!("tree.{id}.bytes")).parse().unwrap_or(0),
            digest: get(&format!("tree.{id}.digest")),
            entities: get(&format!("tree.{id}.entities")),
            db_logons: get(&format!("tree.{id}.db-logons")),
            db_rowtypes: get(&format!("tree.{id}.db-rowtypes")),
        })
        .collect()
}

/// The committed fixture files of one tree, in sorted path order.
pub fn files(id: &str) -> Vec<FixtureFile> {
    let facts = catalog();
    let get = |item: &str| -> String {
        facts
            .iter()
            .find(|f| f.item == item)
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    let n = facts
        .iter()
        .filter(|f| f.item.starts_with(&format!("file.{id}.")) && f.item.ends_with(".input"))
        .count();
    (1..=n)
        .map(|i| FixtureFile {
            tree: id.to_string(),
            kind: get(&format!("file.{id}.{i}.kind")),
            input: get(&format!("file.{id}.{i}.input")),
            bytes: get(&format!("file.{id}.{i}.bytes")).parse().unwrap_or(0),
            sha256: get(&format!("file.{id}.{i}.sha256")),
        })
        .collect()
}

/// The committed entity definitions of one tree, in sorted path order.
pub fn entity_defs(id: &str) -> Vec<EntityDef> {
    let facts = catalog();
    let get = |item: &str| -> String {
        facts
            .iter()
            .find(|f| f.item == item)
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    // One row group per `.def` file, keyed by the entity name recorded in
    // the `.def` row, discovered by scanning the committed rows.
    let names: Vec<String> = facts
        .iter()
        .filter(|f| f.item.starts_with(&format!("entity.{id}.")) && f.item.ends_with(".def"))
        .map(|f| {
            f.item
                .trim_start_matches(&format!("entity.{id}."))
                .trim_end_matches(".def")
                .to_string()
        })
        .collect();
    names
        .into_iter()
        .map(|name| {
            let def = get(&format!("entity.{id}.{name}.def"));
            let properties = get(&format!("entity.{id}.{name}.properties"))
                .parse()
                .unwrap_or(0);
            let client_methods = get(&format!("entity.{id}.{name}.clientmethods"))
                .parse()
                .unwrap_or(0);
            let cell_methods = get(&format!("entity.{id}.{name}.cellmethods"))
                .parse()
                .unwrap_or(0);
            let base_methods = get(&format!("entity.{id}.{name}.basemethods"))
                .parse()
                .unwrap_or(0);
            let exposed = get(&format!("entity.{id}.{name}.exposed"))
                .parse()
                .unwrap_or(0);
            EntityDef {
                tree: id.to_string(),
                name,
                def,
                properties,
                client_methods,
                cell_methods,
                base_methods,
                exposed,
            }
        })
        .collect()
}

/// The committed generator path.
pub fn generator() -> String {
    catalog()
        .into_iter()
        .find(|f| f.item == "gen.generator")
        .map(|f| f.value)
        .unwrap_or_default()
}

/// The committed named blocker for the live asset capture.
pub fn blocker() -> String {
    catalog()
        .into_iter()
        .find(|f| f.item == "blocker.live-asset-capture")
        .map(|f| f.value)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference path to the checkout used by the drift and generator
    /// tests, when it is present.
    fn reference_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld")
    }

    /// The committed catalog records both tracked fixture trees with their
    /// input paths, file counts and traceability facts.
    #[test]
    fn committed_catalog_records_both_trees() {
        let trees = trees();
        assert_eq!(trees.len(), 2, "two tracked fixture trees");

        let fantasy = &trees[0];
        assert_eq!(fantasy.id, "fantasydemo");
        assert!(fantasy.generated == "yes", "{fantasy:?}");
        assert!(
            fantasy
                .generator
                .ends_with("generate-fantasydemo-assets.py"),
            "{fantasy:?}"
        );
        assert_eq!(fantasy.files, 21);
        assert_eq!(
            fantasy.entities, "Account,Avatar,Guard,Space,ClientOnly",
            "entity list from entities.xml"
        );
        assert_eq!(fantasy.db_logons, "1");
        assert_eq!(fantasy.db_rowtypes, "Account,Avatar");

        let example = &trees[1];
        assert_eq!(example.id, "client-integration-example");
        assert_eq!(example.generated, "no");
        assert_eq!(example.generator, "-");
        assert_eq!(example.files, 24);
        assert_eq!(
            example.entities, "Guard,ClientAvatar,Account,Space,ClientOnly",
            "entity list from entities.xml"
        );
        assert_eq!(example.db_logons, "1");

        // Every tree and file row carries a traceable input path and digest.
        for tree in &trees {
            assert!(!tree.root.is_empty(), "{tree:?}");
            assert!(!tree.digest.is_empty(), "{tree:?}");
            for file in files(&tree.id) {
                assert!(!file.input.is_empty(), "{file:?}");
                assert_eq!(file.sha256.len(), 64, "{file:?}");
                assert!(
                    file.kind == "resource" || file.kind == "entity" || file.kind == "database",
                    "{file:?}"
                );
            }
        }
    }

    /// The fantasydemo entity definitions record the entity's semantic shape
    /// as the reference's own `.def` files define it.
    #[test]
    fn fantasydemo_entity_defs_semantics() {
        let defs = entity_defs("fantasydemo");
        assert_eq!(defs.len(), 5, "five entity definitions");

        let avatar = defs
            .iter()
            .find(|d| d.name == "Avatar")
            .expect("Avatar.def");
        assert_eq!(avatar.properties, 4);
        assert_eq!(avatar.client_methods, 2);
        assert_eq!(avatar.base_methods, 1);
        assert_eq!(avatar.exposed, 1, "logOff is exposed");
        assert!(avatar.def.ends_with("entity_defs/Avatar.def"), "{avatar:?}");

        let account = defs
            .iter()
            .find(|d| d.name == "Account")
            .expect("Account.def");
        assert_eq!(account.properties, 2);
        assert_eq!(account.client_methods, 1);
        assert_eq!(account.exposed, 0);

        // The base-only example Account has no cell part either.
        let example_account = entity_defs("client-integration-example")
            .into_iter()
            .find(|d| d.name == "Account")
            .expect("example Account.def");
        assert_eq!(example_account.properties, 2);
        assert_eq!(example_account.client_methods, 1);
    }

    /// The SHA-256 helper matches the published FIPS 180-4 vectors.
    #[test]
    fn sha256_matches_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
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
        let root = reference_root();
        let first = root.join(TREES[0].1).join("scripts/entities.xml");
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

    /// The fantasydemo fixture tree is byte-for-byte reproducible from its
    /// committed generator: re-running the generator into a scratch root and
    /// comparing every regenerated file's digest to the catalog pins the
    /// input (generator) to the output (fixtures). Skipped in CI.
    #[test]
    fn generator_regenerates_the_fantasydemo_tree() {
        let root = reference_root();
        let gen_script = root.join(GENERATOR);
        let python = which_python();
        if !gen_script.is_file() {
            eprintln!("reference checkout absent; skipping generator run");
            return;
        }
        let Some(python) = python else {
            eprintln!("python3 absent; skipping generator run");
            return;
        };

        let scratch =
            std::env::temp_dir().join(format!("bw00-asset-fixtures-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(scratch.join("programming")).expect("mkdir");
        let output = std::process::Command::new(python)
            .arg(&gen_script)
            .arg(&scratch)
            .output()
            .expect("run generator");
        assert!(
            output.status.success(),
            "generator failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Every regenerated file matches the catalog's recorded digest.
        let catalogued: Vec<(String, String)> = files("fantasydemo")
            .into_iter()
            .map(|f| (f.input, f.sha256))
            .collect();
        let generated = tree_files(&scratch.join("programming/fantasydemo/res"));
        assert_eq!(generated.len(), catalogued.len(), "file count matches");
        for (rel, path) in generated {
            let input = format!("programming/fantasydemo/res/{rel}");
            let want = catalogued
                .iter()
                .find(|(i, _)| *i == input)
                .unwrap_or_else(|| panic!("catalog row missing for {input}"));
            let content = std::fs::read(&path).expect("read generated file");
            assert_eq!(
                sha256_hex(&content),
                want.1,
                "generated {input} differs from catalog"
            );
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A python3 on `PATH`, when one exists.
    fn which_python() -> Option<String> {
        ["python3", "python"]
            .into_iter()
            .find(|candidate| {
                Command::new(candidate)
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success())
            })
            .map(|candidate| candidate.to_string())
    }
}
