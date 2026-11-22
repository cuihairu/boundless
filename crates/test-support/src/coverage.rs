//! The coverage-ledger row schema (BW-00-05-01).
//!
//! The feature coverage ledger cross-references the reference's Linux Make
//! build catalog ([`crate::make_targets`], BW-00-01-01) against the port's
//! task numbers, verification fixtures and coverage status. This module
//! establishes the row schema — the acceptance's five fields per row — and
//! derives the two fields the reference itself owns: the reference target
//! (原版目标: name, kind, source) and the enable condition (启用条件), read
//! from the reference's own gate machinery. The mapping fields (任务号 task,
//! 夹具 fixture, 状态 status) are carried as [`STATUS_UNMAPPED`] until the
//! BW-00-05-02…05-05 mapping items fill them.
//!
//! The enable conditions are the *complete* gate machinery, verified in the
//! reference — every target falls in exactly one of four buckets:
//!
//! * [`ENABLE_ALWAYS`] — nothing gates the registration; the driver walks
//!   `lib`, `server`, `third_party`, `tools` unconditionally
//!   (`programming/bigworld/Makefile.rules`).
//! * [`ENABLE_EXAMPLES`] — the top-level rules append `examples` to the
//!   subdirs only when `user_shouldBuildExamples` is non-zero (`ifneq ...
//!   else $(info not building examples ...)`); targets under `examples/`
//!   are therefore not built by default.
//! * [`ENABLE_CELLAPPMGR`] — `server/Makefile.rules` sets `LICENSE_SERVER`
//!   when the wildcard test for `cellappmgr/Makefile` fails (a licensing
//!   server source drop) and then filters `cellappmgr` out of the subdirs;
//!   the component and its unit test are enabled by that file's presence.
//!   The checkout ships the file, so the gate is satisfied.
//! * [`ENABLE_MYSQL`] — six components gate their own registration:
//!   `ifneq ($(SYSTEM_HAS_MYSQL),0)` skips with a
//!   `MySQL development libraries not installed` warning and registers in
//!   the `else` branch, so the target builds only when the probe
//!   (`build/make/third_party_mysql.mak`'s `test_mysql.sh` run) succeeds.
//!   db_storage_mysql, bwengine_mysql, sync_db, clear_auto_load,
//!   snapshot_helper and consolidate_dbs.
//!
//! No other conditional `subdirs` modification exists in the tree, and the
//! remaining conditionals (`usePyURLRequest`, `SYSTEM_HAS_PRLIMIT`,
//! `useCustomBillingSystem`) only add sources or flags inside components
//! that already register — they shape a build, they never disable a target,
//! so none of them is an enable condition.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/coverage.txt");

/// The enable condition of a target the driver always walks.
pub const ENABLE_ALWAYS: &str = "always";

/// The enable condition of a target under `examples/` (`user_shouldBuildExamples != 0`).
pub const ENABLE_EXAMPLES: &str = "user_shouldBuildExamples != 0";

/// The enable condition of a target under `server/cellappmgr/`: the
/// component's own `Makefile` must exist (its absence marks a licensing
/// server drop and filters the subdirectory out).
pub const ENABLE_CELLAPPMGR: &str = "cellappmgr/Makefile present";

/// The enable condition of a component that wraps its registration in
/// `ifneq ($(SYSTEM_HAS_MYSQL),0) … else … endif`.
pub const ENABLE_MYSQL: &str = "MySQL development libraries present";

/// The warning line the six MySQL-gated files print in their skip branch —
/// the marker this scanner keys on (no other file in the tree carries it).
const MYSQL_SKIP_MARKER: &str = "MySQL development libraries not installed";

/// The status of a row the mapping items (BW-00-05-02…05-05) have not
/// filled yet: every field carries `-` until then.
pub const STATUS_UNMAPPED: &str = "-";

/// The status of an enabled target with a runnable acceptance through a
/// fixture.
pub const STATUS_COVERED: &str = "covered";

/// The status of an enabled target with a traceable blocker instead of a
/// runnable acceptance (the preamble's 显式缺口).
pub const STATUS_GAP: &str = "gap";

/// One ledger row: one reference build target plus its coverage mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageRow {
    /// The reference target's component name (`name := <x>`).
    pub target: String,
    /// The declared kind (`type := <y>`); together with [`CoverageRow::name`]
    /// and [`CoverageRow::source`] this is the 原版目标 field.
    pub kind: String,
    /// The registering file, relative to the bigworld source dir — the
    /// identity of the target (a library and its same-named unit test).
    pub source: String,
    /// The 启用条件: one of [`ENABLE_ALWAYS`], [`ENABLE_EXAMPLES`],
    /// [`ENABLE_CELLAPPMGR`], [`ENABLE_MYSQL`].
    pub enable: String,
    /// The 任务号 — the port task that owns the target's porting; `-` until
    /// the mapping items fill it.
    pub task: String,
    /// The 夹具 — the verification fixture exercising the target; `-` until
    /// the mapping items fill it.
    pub fixture: String,
    /// The 状态 — [`STATUS_COVERED`], [`STATUS_GAP`], or [`STATUS_UNMAPPED`].
    pub status: String,
}

/// The enable condition for one registration: the subtree gates key on the
/// source path (the driver decides by directory), the component gate on the
/// registering file's own text (the MySQL skip warning).
fn derive_enable(source: &str, text: &str) -> &'static str {
    if source.starts_with("examples/") {
        ENABLE_EXAMPLES
    } else if source.starts_with("server/cellappmgr/") {
        ENABLE_CELLAPPMGR
    } else if text.contains(MYSQL_SKIP_MARKER) {
        ENABLE_MYSQL
    } else {
        ENABLE_ALWAYS
    }
}

/// The rows under the scope of the mapping item BW-00-05-02: the engine
/// libraries (`lib/`) and the server targets (`server/`, which also covers
/// `server/tools/` and `server/dbapp_extensions/`).
const LANE_SCOPES: [&str; 2] = ["lib/", "server/"];

/// The engine directories a lane claims in prose rather than on its `源码`
/// line. `lib/urlrequest/` is named by BW-03-03's preamble —「网络/URL 请求
/// （`lib/urlrequest/`）」— while the lane's `源码` line lists only
/// `lib/script/`、`lib/pyscript/`、`common/`; the supplement carries the claim
/// the prose makes.
const EXTRA_CLAIMS: [(&str, &str); 1] = [("lib/urlrequest", "03")];

/// The repo's own `todo.md`, two levels above the crate directory — the lane
/// claims are read from it.
pub fn todo_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("todo.md")
}

/// Expand one directory token's brace group: `lib/{a,b}/` (already stripped
/// of its trailing slash) carries `lib/a` and `lib/b`.
fn expand_braces(token: &str) -> Vec<String> {
    let Some(open) = token.find('{') else {
        return vec![token.to_string()];
    };
    let Some(close_rel) = token[open..].find('}') else {
        return vec![token.to_string()];
    };
    let close = open + close_rel;
    let head = &token[..open];
    let alts = &token[open + 1..close];
    let rest = &token[close + 1..];
    alts.split(',')
        .map(|alt| format!("{head}{alt}{rest}"))
        .collect()
}

/// Parse `todo.md`'s lane `源码` preamble lines into (directory, lane) claims.
///
/// Each `### BW-NN …` lane is followed by a `源码 …` line whose backtick-
/// quoted tokens name the source it owns. Directory tokens are the quoted
/// pieces ending in `/`; brace groups expand (`lib/{db,db_storage}/` →
/// `lib/db`, `lib/db_storage`). File tokens (`lib/server/bwservice.hpp`) and
/// globs (`server/dbapp/*login*`) are not directories, so they claim
/// nothing. An absent `todo.md` yields no claims.
pub fn todo_lane_claims(todo: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(todo) else {
        return Vec::new();
    };
    let mut claims = Vec::new();
    let mut lane: Option<&str> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("### BW-") {
            let id_end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            lane = if id_end == 0 {
                None
            } else {
                Some(&rest[..id_end])
            };
            continue;
        }
        let Some(src) = line.strip_prefix("源码") else {
            continue;
        };
        let Some(lane_id) = lane else { continue };
        for (i, piece) in src.split('`').enumerate() {
            if i % 2 == 0 {
                continue;
            }
            let piece = piece.trim().trim_start_matches("../");
            if !piece.ends_with('/') {
                continue;
            }
            for expanded in expand_braces(piece.trim_end_matches('/')) {
                claims.push((expanded, lane_id.to_string()));
            }
        }
    }
    // The prose-only claims ride along (see [`EXTRA_CLAIMS`]).
    claims.extend(
        EXTRA_CLAIMS
            .iter()
            .map(|(dir, lane)| ((*dir).to_string(), (*lane).to_string())),
    );
    claims
}

/// The lane index of one row: the lanes whose claimed directory contains the
/// registering file's directory, in lane order, joined with `,`. A lane
/// claims a row when its directory equals or is below the claimed directory
/// (a `unit_test` sub-directory belongs to its component's lane).
fn lane_index(claims: &[(String, String)], source: &str) -> Option<String> {
    let dir = source.rsplit_once('/').map_or("", |(d, _)| d);
    let mut lanes: Vec<&str> = claims
        .iter()
        .filter(|(claim, _)| dir == claim || dir.starts_with(&format!("{claim}/")))
        .map(|(_, lane)| lane.as_str())
        .collect();
    lanes.sort_unstable();
    lanes.dedup();
    if lanes.is_empty() {
        None
    } else {
        // The lane is stored digit-only (`01`); the index carries the todo's
        // own task numbering (`BW-01`).
        Some(
            lanes
                .iter()
                .map(|lane| format!("BW-{lane}"))
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}

/// Fill the `task` field of the rows in scope from the lane claims, returning
/// how many were indexed. Rows no lane claims stay [`STATUS_UNMAPPED`].
fn index_tasks(rows: &mut [CoverageRow], claims: &[(String, String)]) -> usize {
    let mut filled = 0;
    for row in rows.iter_mut() {
        if !LANE_SCOPES.iter().any(|s| row.source.starts_with(s)) {
            continue;
        }
        if let Some(index) = lane_index(claims, &row.source) {
            row.task = index;
            filled += 1;
        }
    }
    filled
}

/// The rows in scope that no lane claim indexes: the traceable gaps the
/// ledger has to name (BW-00-05-06's 显式缺口).
pub fn unindexed(rows: &[CoverageRow]) -> Vec<&CoverageRow> {
    rows.iter()
        .filter(|row| {
            LANE_SCOPES.iter().any(|s| row.source.starts_with(s)) && row.task == STATUS_UNMAPPED
        })
        .collect()
}

/// Scan a reference checkout into ledger rows.
///
/// `bw_source` is the bigworld source directory (`programming/bigworld`);
/// `todo` is the repo's own `todo.md` (its lane `源码` preambles index the
/// rows). The rows are exactly the Make build catalog
/// ([`crate::make_targets`]) with the enable condition derived from the
/// reference's gate machinery, the `task` field of the in-scope engine
/// library and server rows indexed to their owning lane, and the remaining
/// mapping fields left [`STATUS_UNMAPPED`]; the order is the catalog's
/// (name, source) order.
pub fn scan_reference(bw_source: &Path, todo: &Path) -> Vec<CoverageRow> {
    let mut rows: Vec<CoverageRow> = crate::make_targets::scan_reference(bw_source)
        .into_iter()
        .map(|t| {
            let text = std::fs::read_to_string(bw_source.join(&t.source)).unwrap_or_default();
            CoverageRow {
                enable: derive_enable(&t.source, &text).to_string(),
                target: t.name,
                kind: t.kind,
                source: t.source,
                task: String::from(STATUS_UNMAPPED),
                fixture: String::from(STATUS_UNMAPPED),
                status: String::from(STATUS_UNMAPPED),
            }
        })
        .collect();
    let claims = todo_lane_claims(todo);
    index_tasks(&mut rows, &claims);
    rows
}

/// Render the scan into the tab-separated catalog text.
///
/// One record per row, fields in [`CoverageRow`] field order.
pub fn render(rows: &[CoverageRow]) -> String {
    let mut out = String::new();
    let header = [
        "target", "kind", "source", "enable", "task", "fixture", "status",
    ];
    out.push_str(&header.join("\t"));
    out.push('\n');
    for r in rows {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.target, r.kind, r.source, r.enable, r.task, r.fixture, r.status
        );
    }
    out
}

/// Parse the catalog text back into [`CoverageRow`]s (inverse of [`render`]).
pub fn parse_catalog(text: &str) -> Vec<CoverageRow> {
    let mut lines = text.lines();
    // Skip the header line.
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let mut take = || fields.next().unwrap_or("-").to_string();
        out.push(CoverageRow {
            target: take(),
            kind: take(),
            source: take(),
            enable: take(),
            task: take(),
            fixture: take(),
            status: take(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<CoverageRow> {
    parse_catalog(CATALOG_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_keys_on_the_path_and_the_marker() {
        // The subtree gates key on the directory, whatever the file says.
        assert_eq!(
            derive_enable("examples/cellapp_extension/Makefile.rules", ""),
            ENABLE_EXAMPLES
        );
        assert_eq!(
            derive_enable("server/cellappmgr/unit_test/Makefile.rules", ""),
            ENABLE_CELLAPPMGR
        );
        // The component gate keys on the registering file's own text.
        let mysql_text = "ifneq ($(SYSTEM_HAS_MYSQL),0)\n\
             $(warning MySQL development libraries not installed. Skipping x build.)\n\
             else\n\
             name := sync_db\n";
        assert_eq!(
            derive_enable("server/tools/sync_db/Makefile.rules", mysql_text),
            ENABLE_MYSQL
        );
        // Everything else builds unconditionally.
        assert_eq!(
            derive_enable("server/baseapp/Makefile.rules", "name := baseapp"),
            ENABLE_ALWAYS
        );
    }

    /// The acceptance: every committed row carries the five fields — the
    /// reference target (name, kind, source), the enable condition, and the
    /// three mapping fields (present; `fixture` and `status` stay `-` until
    /// the mapping items fill them, while `task` is indexed from BW-00-05-02
    /// on).
    #[test]
    fn every_row_carries_the_five_fields() {
        let catalog = catalog();
        assert_eq!(
            catalog.len(),
            79,
            "one row per Make build target (the BW-00-01-01 catalog)"
        );
        for row in &catalog {
            assert!(!row.target.is_empty(), "{row:?}");
            assert!(!row.kind.is_empty(), "{}", row.target);
            assert!(!row.source.is_empty(), "{}", row.target);
            assert!(!row.enable.is_empty(), "{}", row.target);
            assert!(!row.task.is_empty(), "{}", row.target);
            assert!(!row.fixture.is_empty(), "{}", row.target);
            assert!(!row.status.is_empty(), "{}", row.target);
            // The fixture and status fields are established but not yet
            // filled; the task field is the mapping items' work.
            assert_eq!(row.fixture, STATUS_UNMAPPED, "{}", row.target);
            assert_eq!(row.status, STATUS_UNMAPPED, "{}", row.target);
        }
    }

    /// The enable conditions are the complete gate machinery: every row
    /// falls in one of the four buckets, with the census the reference's
    /// gates predict (69 unconditional, 2 examples, 2 cellappmgr, 6 MySQL).
    #[test]
    fn enable_conditions_partition_the_catalog() {
        let catalog = catalog();
        let mut census = std::collections::BTreeMap::new();
        for row in &catalog {
            *census.entry(row.enable.as_str()).or_insert(0usize) += 1;
        }
        assert_eq!(census.get(ENABLE_ALWAYS), Some(&69));
        assert_eq!(census.get(ENABLE_EXAMPLES), Some(&2));
        assert_eq!(census.get(ENABLE_CELLAPPMGR), Some(&2));
        assert_eq!(census.get(ENABLE_MYSQL), Some(&6));
        assert_eq!(census.len(), 4, "no fifth enable condition exists");
    }

    /// The buckets name exactly the targets the reference's gates gate.
    #[test]
    fn the_gated_rows_are_exactly_the_reference_gates() {
        let catalog = catalog();

        let find = |target: &str, source: &str| {
            catalog
                .iter()
                .find(|r| r.target == target && r.source == source)
                .unwrap_or_else(|| panic!("missing {target} at {source}"))
        };

        // examples/: the two targets the top-level rules gate.
        let cellapp_example = find(
            "cellapp_example",
            "examples/cellapp_extension/Makefile.rules",
        );
        let simple_python_client = find(
            "simple_python_client",
            "examples/client_integration/python/simple/Makefile.rules",
        );
        assert_eq!(cellapp_example.enable, ENABLE_EXAMPLES);
        assert_eq!(simple_python_client.enable, ENABLE_EXAMPLES);

        // cellappmgr: the component and its unit test, gated by the
        // Makefile's presence.
        assert_eq!(
            find("cellappmgr", "server/cellappmgr/Makefile.rules").enable,
            ENABLE_CELLAPPMGR
        );
        assert_eq!(
            find("cellappmgr", "server/cellappmgr/unit_test/Makefile.rules").enable,
            ENABLE_CELLAPPMGR
        );

        // MySQL: the six components that wrap their own registration.
        for (target, source) in [
            (
                "bwengine_mysql",
                "server/dbapp_extensions/bwengine_mysql/Makefile.rules",
            ),
            (
                "clear_auto_load",
                "server/tools/clear_auto_load/Makefile.rules",
            ),
            (
                "consolidate_dbs",
                "server/tools/consolidate_dbs/Makefile.rules",
            ),
            ("db_storage_mysql", "lib/db_storage_mysql/Makefile.rules"),
            (
                "snapshot_helper",
                "server/tools/snapshot_helper/Makefile.rules",
            ),
            ("sync_db", "server/tools/sync_db/Makefile.rules"),
        ] {
            assert_eq!(find(target, source).enable, ENABLE_MYSQL, "{target}");
        }

        // The core server binaries and libraries build unconditionally.
        assert_eq!(
            find("baseapp", "server/baseapp/Makefile.rules").enable,
            ENABLE_ALWAYS
        );
        assert_eq!(
            find("network", "lib/network/Makefile.rules").enable,
            ENABLE_ALWAYS
        );
    }

    #[test]
    fn render_then_parse_round_trips() {
        let original = vec![
            CoverageRow {
                target: String::from("baseapp"),
                kind: String::from("binary"),
                source: String::from("server/baseapp/Makefile.rules"),
                enable: String::from(ENABLE_ALWAYS),
                task: String::from("BW-01-02-01"),
                fixture: String::from("cases/full_stack/original.tsv"),
                status: String::from(STATUS_COVERED),
            },
            CoverageRow {
                target: String::from("sync_db"),
                kind: String::from("binary"),
                source: String::from("server/tools/sync_db/Makefile.rules"),
                enable: String::from(ENABLE_MYSQL),
                task: String::from(STATUS_UNMAPPED),
                fixture: String::from(STATUS_UNMAPPED),
                status: String::from(STATUS_GAP),
            },
        ];
        let text = render(&original);
        assert_eq!(parse_catalog(&text), original);
    }

    /// The lane claims parse the `源码` preambles of `todo.md`: the engine
    /// directories the lanes name, with brace groups expanded and file/glob
    /// tokens ignored.
    #[test]
    fn todo_lane_claims_parse_the_source_preambles() {
        let todo = todo_path();
        assert!(todo.is_file(), "{}", todo.display());
        let claims = todo_lane_claims(&todo);
        let has = |dir: &str, lane: &str| claims.iter().any(|(d, l)| d == dir && l == lane);

        // BW-01, BW-02, BW-03 single-directory claims.
        assert!(has("lib/cstdmf", "01"));
        assert!(has("lib/math", "01"));
        assert!(has("lib/resmgr", "01"));
        assert!(has("lib/build", "01"));
        assert!(has("lib/memhook", "01"));
        assert!(has("lib/network", "02"));
        assert!(has("lib/script", "03"));
        assert!(has("lib/pyscript", "03"));
        // Brace-group expansion.
        assert!(has("lib/connection", "02"));
        assert!(has("lib/connection_model", "06"));
        assert!(has("lib/db", "09"));
        assert!(has("lib/db_storage_mysql", "09"));
        assert!(has("lib/sqlite", "09"));
        assert!(has("server/tools/sync_db", "09"));
        assert!(has("server/tools/remove_db", "09"));
        assert!(has("lib/unit_test_lib", "12"));
        assert!(has("server/tools/bots", "12"));
        assert!(has("server/tools/message_logger", "12"));
        assert!(has("lib/moo", "11"));
        assert!(has("lib/speedtree", "11"));
        assert!(has("lib/chunk", "10"));
        assert!(has("lib/delegate_interface", "10"));
        assert!(has("tools/process_defs", "04"));
        assert!(has("tools", "13"));
        assert!(has("examples", "14"));
        assert!(has("fantasydemo", "14"));
        assert!(has("lib/urlrequest", "03"), "the prose supplement");
        // File tokens and globs claim nothing.
        assert!(
            !has("lib/server", "01"),
            "lib/server/bwservice.hpp is a file, not a dir claim"
        );
        assert!(
            !has("server/dbapp", "06"),
            "server/dbapp/*login* is a glob, not a dir claim"
        );
    }

    /// The acceptance for BW-00-05-02: every engine library and server target
    /// in the build inventory carries a lane index. The engine's own
    /// directories are claimed by the lanes; `lib/urlrequest` is the one
    /// claimed by a lane's prose rather than its `源码` line.
    #[test]
    fn every_engine_library_and_server_row_carries_a_lane_index() {
        let catalog = catalog();
        let scoped: Vec<&CoverageRow> = catalog
            .iter()
            .filter(|row| LANE_SCOPES.iter().any(|s| row.source.starts_with(s)))
            .collect();
        assert_eq!(scoped.len(), 72, "42 lib + 30 server rows");
        for row in &scoped {
            assert_ne!(
                row.task, STATUS_UNMAPPED,
                "{} at {} has no lane index",
                row.target, row.source
            );
        }
        // The prose-claimed one is indexed to the lane that names it.
        let urlrequest = catalog
            .iter()
            .find(|r| r.target == "urlrequest" && r.source == "lib/urlrequest/Makefile.rules")
            .expect("urlrequest row");
        assert_eq!(urlrequest.task, "BW-03");
        // No gap remains in scope.
        assert!(unindexed(&catalog).is_empty());
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped when the
    /// reference checkout is absent (CI).
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let bw_source = root.join("programming").join("bigworld");
        if !bw_source.join("Makefile.rules").is_file() {
            eprintln!("reference checkout absent; skipping rescan");
            return;
        }
        let scan = scan_reference(&bw_source, &todo_path());
        assert_eq!(render(&scan), CATALOG_TEXT);
    }
}
