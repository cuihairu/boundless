//! The original unit-test run (BW-00-02-02): the participating list, the
//! execution command and the results.
//!
//! The reference runs its unit tests through the Make driver
//! (`programming/bigworld/build/make/Makefile`): every component registering
//! `type := unit_test` lands in the `BW_UNIT_TESTS` / `BW_RUN_UNIT_TESTS` /
//! `BW_UNIT_TEST_BIN_SRC` aggregates via the shared footer, and
//! `make bw-run-all-unit-tests` walks `BW_UNIT_TEST_BIN_SRC` — one
//! `$(destFile):$(pathToSource)` pair per test — running each binary as
//! `cd <source dir>; timeout 1800 <binary> --root $(BW_ROOT)`. This module
//! records that list, the command, and the per-test result.
//!
//! The reference checkout is source-only (`runtime_env`: no prebuilt server
//! binaries): nothing is built under the unit-test output directory, so the
//! binaries cannot be re-run here without building the whole reference
//! server first. Every entry therefore records its named gap instead of a
//! fabricated result — the same honesty the Windows baseline
//! ([`crate::windows_baseline`]) uses for its targets.

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/unit_tests.txt");

/// The driver's run-all target (`bw-run-all-unit-tests`): runs every entry in
/// `BW_UNIT_TEST_BIN_SRC` and reports failed modules at the end.
pub const RUN_ALL_TARGET: &str = "bw-run-all-unit-tests";

/// The stop-at-first-failure alias (`bw-run-unit-tests`, sets
/// `UNIT_TEST_STOP_AT_FAILURE=1`).
pub const RUN_STOP_TARGET: &str = "bw-run-unit-tests";

/// The unit-test output directory the footer always uses for unit tests:
/// `destDir := $(BW_BINDIR)/$(BW_UNIT_TEST_DIR)` with `BW_BINDIR :=
/// $(BW_INSTALL_DIR)` and `BW_UNIT_TEST_DIR :=
/// bin/server/$(BW_PLATFORM_CONFIG)/unit_tests`. Recorded symbolically — the
/// checkout resolves it only at build time.
pub const UNIT_TEST_DIR: &str = "$(BW_INSTALL_DIR)/bin/server/$(BW_PLATFORM_CONFIG)/unit_tests";

/// The per-test timeout the driver's run loop applies (`$(TIMEOUT_BIN) 1800`).
pub const TEST_TIMEOUT_SECS: u32 = 1800;

/// The root argument every unit test is invoked with (`--root $(BW_ROOT)`).
pub const ROOT_FLAG: &str = "--root $(BW_ROOT)";

/// The named gap recorded for every entry: the binaries are not built.
pub const RUN_GAP: &str = "source-only checkout: no unit-test binaries are built under $(BW_INSTALL_DIR)/bin/server/*/unit_tests, and re-running them requires building the whole reference server (third_party + lib + server components) first";

/// One participating unit test: a `BW_UNIT_TEST_BIN_SRC` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitTestEntry {
    /// The component name (`name := <x>`; the binary name too).
    pub name: String,
    /// The registering file, relative to the bigworld source dir.
    pub source: String,
    /// The directory the run `cd`s into (`$(pathToSource)`).
    pub source_dir: String,
    /// The symbolic binary path the footer computes
    /// (`$(destFile)`, the entry `BW_RUN_UNIT_TESTS` runs).
    pub binary: String,
    /// The per-test invocation, exactly the driver loop's command.
    pub run_command: String,
    /// The result: `gap` for every entry (see [`RUN_GAP`]).
    pub result: String,
    /// The named blocker (never empty).
    pub gap: String,
}

/// Derive one entry from a `unit_test` component registration.
fn entry_of(target: &crate::make_targets::MakeTarget) -> UnitTestEntry {
    let source_dir = match target.source.rfind('/') {
        Some(cut) => target.source[..cut].to_string(),
        None => String::from("."),
    };
    let binary = format!("{UNIT_TEST_DIR}/{}", target.name);
    UnitTestEntry {
        name: target.name.clone(),
        source: target.source.clone(),
        run_command: format!("cd {source_dir}; timeout {TEST_TIMEOUT_SECS} {binary} {ROOT_FLAG}"),
        source_dir,
        binary,
        result: String::from("gap"),
        gap: String::from(RUN_GAP),
    }
}

/// Scan a reference checkout's unit-test run list.
///
/// `bw_source` is the bigworld source directory (`programming/bigworld`).
/// The participating list is exactly the `type := unit_test` component
/// registrations ([`crate::make_targets`] scans the same files), sorted by
/// source path for a deterministic catalog.
pub fn scan_reference(bw_source: &Path) -> Vec<UnitTestEntry> {
    let mut out: Vec<UnitTestEntry> = crate::make_targets::scan_reference(bw_source)
        .into_iter()
        .filter(|target| target.kind == "unit_test")
        .map(|target| entry_of(&target))
        .collect();
    out.sort_by(|a, b| a.source.cmp(&b.source));
    out
}

/// Render the scan into the tab-separated catalog text.
///
/// One record per entry, fields in [`UnitTestEntry`] field order.
pub fn render(entries: &[UnitTestEntry]) -> String {
    let mut out = String::new();
    let header = [
        "name",
        "source",
        "source_dir",
        "binary",
        "run_command",
        "result",
        "gap",
    ];
    out.push_str(&header.join("\t"));
    out.push('\n');
    for e in entries {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            e.name, e.source, e.source_dir, e.binary, e.run_command, e.result, e.gap
        );
    }
    out
}

/// Parse the catalog text back into [`UnitTestEntry`]s (inverse of
/// [`render`]).
pub fn parse_catalog(text: &str) -> Vec<UnitTestEntry> {
    let mut lines = text.lines();
    // Skip the header line.
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let mut take = || fields.next().unwrap_or("").to_string();
        out.push(UnitTestEntry {
            name: take(),
            source: take(),
            source_dir: take(),
            binary: take(),
            run_command: take(),
            result: take(),
            gap: take(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<UnitTestEntry> {
    parse_catalog(CATALOG_TEXT)
}

use std::fmt::Write as _;
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog is exactly the `unit_test` slice of the Make
    /// target catalog, so the two cannot drift apart.
    #[test]
    fn the_participating_list_is_the_make_catalogs_unit_test_slice() {
        let catalog = catalog();
        let mut from_make: Vec<(String, String)> = crate::make_targets::catalog()
            .into_iter()
            .filter(|t| t.kind == "unit_test")
            .map(|t| (t.name, t.source))
            .collect();
        from_make.sort_by(|a, b| a.1.cmp(&b.1));
        let from_unit_tests: Vec<(String, String)> = catalog
            .iter()
            .map(|e| (e.name.clone(), e.source.clone()))
            .collect();
        assert_eq!(from_unit_tests, from_make);
        assert_eq!(catalog.len(), 21, "every unit-test component");
    }

    /// The run command is the driver loop's, verbatim in shape: cd into the
    /// component source dir, the 1800 s timeout, the symbolic binary path,
    /// and `--root $(BW_ROOT)`.
    #[test]
    fn the_run_command_is_the_driver_loops() {
        let catalog = catalog();
        assert_eq!(RUN_ALL_TARGET, "bw-run-all-unit-tests");
        assert_eq!(RUN_STOP_TARGET, "bw-run-unit-tests");
        assert_eq!(TEST_TIMEOUT_SECS, 1800);
        assert_eq!(ROOT_FLAG, "--root $(BW_ROOT)");
        let network = catalog
            .iter()
            .find(|e| e.name == "network")
            .expect("network unit test");
        assert_eq!(network.source_dir, "lib/network/unit_test");
        assert_eq!(
            network.run_command,
            "cd lib/network/unit_test; timeout 1800 \
             $(BW_INSTALL_DIR)/bin/server/$(BW_PLATFORM_CONFIG)/unit_tests/network \
             --root $(BW_ROOT)"
        );
        assert_eq!(network.binary, UNIT_TEST_DIR.to_string() + "/network");
        for entry in &catalog {
            assert!(
                entry
                    .run_command
                    .starts_with(&format!("cd {};", entry.source_dir))
            );
            assert!(entry.run_command.contains("timeout 1800"));
            assert!(entry.run_command.ends_with(ROOT_FLAG));
        }
    }

    /// Every entry records the named gap, never a fabricated result.
    #[test]
    fn every_entry_records_the_source_only_gap() {
        for entry in catalog() {
            assert_eq!(entry.result, "gap", "{}", entry.name);
            assert_eq!(entry.gap, RUN_GAP);
            assert!(entry.gap.contains("source-only"));
            assert!(entry.gap.contains("unit_tests"));
        }
    }

    #[test]
    fn render_then_parse_round_trips() {
        let original = vec![
            UnitTestEntry {
                name: String::from("network"),
                source: String::from("lib/network/unit_test/Makefile.rules"),
                source_dir: String::from("lib/network/unit_test"),
                binary: format!("{UNIT_TEST_DIR}/network"),
                run_command: format!(
                    "cd lib/network/unit_test; timeout {TEST_TIMEOUT_SECS} \
                     {UNIT_TEST_DIR}/network {ROOT_FLAG}"
                ),
                result: String::from("gap"),
                gap: String::from(RUN_GAP),
            },
            UnitTestEntry {
                name: String::from("math"),
                source: String::from("lib/math/unit_test/Makefile.rules"),
                source_dir: String::from("lib/math/unit_test"),
                binary: format!("{UNIT_TEST_DIR}/math"),
                run_command: format!(
                    "cd lib/math/unit_test; timeout {TEST_TIMEOUT_SECS} \
                     {UNIT_TEST_DIR}/math {ROOT_FLAG}"
                ),
                result: String::from("gap"),
                gap: String::from(RUN_GAP),
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
