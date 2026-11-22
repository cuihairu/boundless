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
use std::path::Path;

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

/// Scan a reference checkout into ledger rows.
///
/// `bw_source` is the bigworld source directory (`programming/bigworld`).
/// The rows are exactly the Make build catalog ([`crate::make_targets`])
/// with the enable condition derived from the reference's gate machinery
/// and the mapping fields left [`STATUS_UNMAPPED`]; the order is the
/// catalog's (name, source) order.
pub fn scan_reference(bw_source: &Path) -> Vec<CoverageRow> {
    crate::make_targets::scan_reference(bw_source)
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
        .collect()
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
    /// three mapping fields (present, `-` until the mapping items fill them).
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
            // The mapping fields are established but not yet filled.
            assert_eq!(row.task, STATUS_UNMAPPED, "{}", row.target);
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
        let scan = scan_reference(&bw_source);
        assert_eq!(render(&scan), CATALOG_TEXT);
    }
}
