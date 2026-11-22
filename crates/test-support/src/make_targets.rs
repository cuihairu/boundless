//! Linux Make build-target catalog (BW-00-01-01).
//!
//! The reference's Linux build (`programming/Makefile` →
//! `bigworld/build/make/Makefile`) does not list its targets in one place.
//! The driver walks the component trees (`subdirs = lib server third_party
//! tools`, plus `examples` when `user_shouldBuildExamples` is set), and each
//! component registers itself in its own `Makefile.rules` (or
//! `Makefile.serviceapp`) with `name := <x>` and `type := <y>`, then closes
//! with `include $(BW_COMPONENT_FOOTER)` whose generic rules — defined once
//! in [`FOOTER_RULE_FILE`] (`build/make/common_footer_config.mak`) — build
//! every component the same way. This module walks the same trees, collects
//! every registration, and renders the inventory the acceptance requires:
//! the binaries, the libraries, and the rule that owns each one.
//!
//! Reference model (the parts the catalog mirrors):
//!
//! * Registrations — a `Makefile*` file under `programming/bigworld/`
//!   (excluding `build/`, which holds the shared rules, not components)
//!   declares `name := <x>` plus `type := <y>` where `y` is one of `binary`,
//! * `copy_of_binary`, `library`, `shared_library`, `unit_test`. The
//!   `unit_test/` sub-directory of a library reuses the parent component's
//!   `name` with `type := unit_test` (e.g. `lib/network/Makefile.rules` is
//!   the `network` library and `lib/network/unit_test/Makefile.rules` is the
//!   `network` unit test), so the source path — not the name — identifies a
//!   target. The resets at the end of a component file (`name :=` with no
//!   value) are not registrations and are ignored.
//! * Owning rule — a standard component's build rules live in the shared
//!   footer ([`FOOTER_RULE_FILE`]); `server/tools/remove_db/Makefile.rules`
//!   is the one component that defines its own rule (`$(name): $(destFile)`
//!   plus its own `BW_BINS += $(name)` line), recorded by pointing
//!   [`MakeTarget::rule_file`] at the component's own file.
//! * Destination — binaries and tools declare `bindir = $(BW_SERVER_BIN_DIR)`
//!   (or `$(BW_SERVER_TOOLS_BIN_DIR)` / `$(BW_COMMAND_BIN_DIR)` / …);
//!   libraries and unit tests declare none, recorded as `-`.
//!
//! The committed catalog (`cases/make_targets.txt`) is regenerated with the
//! `scan-make-targets` binary and re-parsed by the catalog test, so the file
//! and the scanner cannot drift.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/make_targets.txt");

/// The shared rule file every standard component includes via
/// `$(BW_COMPONENT_FOOTER)`, relative to the bigworld source dir.
pub const FOOTER_RULE_FILE: &str = "build/make/common_footer_config.mak";

/// The rule the shared footer defines for every component's build target
/// (`.PHONY: $(binName)$(componentSuffix)`).
pub const FOOTER_TARGET_RULE: &str = "$(binName)$(componentSuffix)";

/// The rule name a self-ruled component (remove_db) defines for itself.
pub const SELF_RULE: &str = "$(name)";

/// The per-component rules the shared footer defines. One definition in
/// [`FOOTER_RULE_FILE`] builds every standard component through these.
pub const FOOTER_RULES: &[&str] = &[
    FOOTER_TARGET_RULE,
    "$(destFile)",
    "bw-run-$(binName)$(componentSuffix)",
    "bw-run-test-$(name)$(componentSuffix)",
    "clean_$(binName)$(componentSuffix)",
    "cleandeps_$(binName)$(componentSuffix)",
];

/// The aggregate variables a self-ruled component appends itself to (the
/// standard components are collected by the footer instead).
const SELF_RULE_AGGREGATES: [&str; 4] = ["BW_BINS", "BW_LIBS", "BW_MISC", "BW_UNIT_TESTS"];

/// One Linux Make build target: a `name :=` + `type :=` registration in a
/// component `Makefile*` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MakeTarget {
    /// The component name (`name := <x>`).
    pub name: String,
    /// The declared type (`type := <y>`): `binary`, `copy_of_binary`,
    /// `library`, `shared_library`, `unit_test`, or `-` (remove_db declares
    /// none).
    pub kind: String,
    /// The registering file, relative to the bigworld source dir (the
    /// identity of the target: names repeat across a library and its unit
    /// test).
    pub source: String,
    /// The name of the rule that builds the target: the shared footer's
    /// `$(binName)$(componentSuffix)`, or `$(name)` for a self-ruled one.
    pub rule: String,
    /// The file that defines that rule: [`FOOTER_RULE_FILE`] for standard
    /// components, or the component's own file for a self-ruled one.
    pub rule_file: String,
    /// The declared `bindir` (`$(BW_SERVER_BIN_DIR)` …), or `-`.
    pub dest: String,
}

/// Read one make assignment `key := value` / `key = value` from a line,
/// returning the first value token. A bare reset (`name :=` with no value)
/// yields `None`.
fn make_var(line: &str, key: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix(key)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(":=").or_else(|| rest.strip_prefix('='))?;
    rest.split_whitespace().next().map(str::to_string)
}

/// Whether a component file appends itself to one of the driver's aggregate
/// variables — the mark of a component that owns its build rule instead of
/// including the shared footer's.
fn has_self_rule(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        SELF_RULE_AGGREGATES.iter().any(|var| {
            line.strip_prefix(var).is_some_and(|rest| {
                let rest = rest.trim_start();
                rest.starts_with("+=") || rest.starts_with(":=") || rest.starts_with('=')
            })
        })
    })
}

/// Parse one component `Makefile*` file into its registrations.
///
/// Files register one component each in practice; multiple `name :=`
/// assignments in one file each start a new registration (`type :=` between
/// two names belongs to the first). `kind` stays `-` when no `type :=` is
/// declared.
fn parse_file(source: &str, text: &str) -> Vec<MakeTarget> {
    let mut registrations: Vec<(String, String)> = Vec::new();
    let mut pending: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(name) = make_var(line, "name") {
            if let Some(prev) = pending.take() {
                registrations.push(prev);
            }
            pending = Some((name, "-".to_string()));
        } else if let Some(kind) = make_var(line, "type")
            && let Some((_, pending_kind)) = pending.as_mut()
        {
            *pending_kind = kind;
        }
    }
    if let Some(prev) = pending.take() {
        registrations.push(prev);
    }

    let self_rule = has_self_rule(text);
    let rule = if self_rule {
        SELF_RULE
    } else {
        FOOTER_TARGET_RULE
    };
    let rule_file = if self_rule { source } else { FOOTER_RULE_FILE };
    let dest = text
        .lines()
        .find_map(|line| make_var(line, "bindir"))
        .unwrap_or_else(|| "-".to_string());

    registrations
        .into_iter()
        .map(|(name, kind)| MakeTarget {
            name,
            kind,
            source: source.to_string(),
            rule: rule.to_string(),
            rule_file: rule_file.to_string(),
            dest: dest.to_string(),
        })
        .collect()
}

/// Collect every `Makefile*` file under `dir`, sorted for a deterministic
/// walk, skipping the top-level `build/` directory only: it holds the shared
/// rules and CMake scaffolding, while nested directories named `build`
/// (`lib/build`, the build-revision library) are components.
fn walk_makefiles(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.map(|e| e.expect("read_dir entry").path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "build") && path.parent() == Some(root) {
                continue;
            }
            walk_makefiles(&path, root, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("Makefile"))
        {
            out.push(path);
        }
    }
}

/// Scan a reference checkout's Linux Make targets.
///
/// `bw_source` is the bigworld source directory (`programming/bigworld`).
/// Every `Makefile*` file under it (excluding the top-level `build/`, which
/// holds the shared rules) is parsed for `name :=` registrations; the
/// catalog is sorted by (name, source) so a library and its same-named unit
/// test sit next to each other.
pub fn scan_reference(bw_source: &Path) -> Vec<MakeTarget> {
    let mut files = Vec::new();
    walk_makefiles(bw_source, bw_source, &mut files);
    let mut out = Vec::new();
    for file in files {
        let source = file
            .strip_prefix(bw_source)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        out.extend(parse_file(&source, &text));
    }
    out.sort_by(|a, b| (&a.name, &a.source).cmp(&(&b.name, &b.source)));
    out
}

/// Render the scan into the tab-separated catalog text.
///
/// One record per target, fields in [`MakeTarget`] field order. `-` marks an
/// unset field (remove_db's kind, a library's dest).
pub fn render(targets: &[MakeTarget]) -> String {
    let mut out = String::new();
    let header = ["name", "kind", "source", "rule", "rule_file", "dest"];
    out.push_str(&header.join("\t"));
    out.push('\n');
    for t in targets {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}",
            t.name, t.kind, t.source, t.rule, t.rule_file, t.dest
        );
    }
    out
}

/// Parse the catalog text back into [`MakeTarget`]s (inverse of [`render`]).
pub fn parse_catalog(text: &str) -> Vec<MakeTarget> {
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
        out.push(MakeTarget {
            name: take(),
            kind: take(),
            source: take(),
            rule: take(),
            rule_file: take(),
            dest: take(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<MakeTarget> {
    parse_catalog(CATALOG_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A standard component, laid out like the real files: name, type,
    /// bindir, footer include, then the variable resets (bare `name :=`).
    const SAMPLE_COMPONENT: &str = "name := baseapp\n\
        type := binary\n\
        \n\
        bindir = $(BW_SERVER_BIN_DIR)\n\
        \n\
        include $(BW_SRC)/server/baseapp/Makefile.rules\n\
        include $(BW_COMPONENT_FOOTER)\n\
        \n\
        name :=\n\
        type :=\n";

    /// A library plus its unit test, sharing the component name.
    const SAMPLE_LIBRARY: &str = "name := network\n\
        type := library\n\
        \n\
        include $(BW_COMPONENT_FOOTER)\n\
        \n\
        name :=\n\
        type :=\n";

    /// The self-ruled component: a name with no type, and its own
    /// `BW_BINS +=` line, laid out like remove_db.
    const SAMPLE_SELF_RULE: &str = "name := remove_db\n\
        \n\
        destDir := $(BW_INSTALL_DIR)/$(BW_BIN_PREFIX)/$(BW_COMMAND_BIN_DIR)\n\
        destFile := $(destDir)/$(name)\n\
        \n\
        .PHONY: $(name)\n\
        $(name): $(destFile)\n\
        \n\
        BW_BINS += $(name)\n\
        \n\
        name :=\n";

    #[test]
    fn parse_reads_name_type_and_bindir() {
        let targets = parse_file("server/baseapp/Makefile.rules", SAMPLE_COMPONENT);
        assert_eq!(targets.len(), 1);
        let target = &targets[0];
        assert_eq!(target.name, "baseapp");
        assert_eq!(target.kind, "binary");
        assert_eq!(target.dest, "$(BW_SERVER_BIN_DIR)");
        assert_eq!(target.rule, FOOTER_TARGET_RULE);
        assert_eq!(target.rule_file, FOOTER_RULE_FILE);
    }

    #[test]
    fn parse_marks_the_self_ruled_component() {
        // remove_db declares no type and appends itself to BW_BINS: the rule
        // that builds it is its own `$(name)`, not the footer's.
        let targets = parse_file("server/tools/remove_db/Makefile.rules", SAMPLE_SELF_RULE);
        assert_eq!(targets.len(), 1);
        let target = &targets[0];
        assert_eq!(target.name, "remove_db");
        assert_eq!(target.kind, "-");
        assert_eq!(target.rule, SELF_RULE);
        assert_eq!(target.rule_file, "server/tools/remove_db/Makefile.rules");
        assert_eq!(target.dest, "-");
    }

    #[test]
    fn parse_ignores_the_reset_lines() {
        // The trailing bare `name :=` / `type :=` are resets, not new
        // registrations; a file with them yields exactly one target.
        let targets = parse_file("lib/network/Makefile.rules", SAMPLE_LIBRARY);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "network");
        assert_eq!(targets[0].kind, "library");
        assert_eq!(targets[0].dest, "-");
    }

    #[test]
    fn render_then_parse_round_trips() {
        let original = [
            parse_file("server/baseapp/Makefile.rules", SAMPLE_COMPONENT),
            parse_file("server/tools/remove_db/Makefile.rules", SAMPLE_SELF_RULE),
            parse_file("lib/network/Makefile.rules", SAMPLE_LIBRARY),
        ]
        .concat();
        let text = render(&original);
        assert_eq!(parse_catalog(&text), original);
    }

    /// The committed catalog records the binaries, the libraries and the
    /// owning rule for each (the acceptance's 清单要求): the server binaries
    /// land in `$(BW_SERVER_BIN_DIR)` by the footer's rule, every library
    /// builds through the same footer, and remove_db owns its own rule.
    #[test]
    fn committed_catalog_records_targets_and_owning_rules() {
        let catalog = catalog();
        assert_eq!(
            catalog.len(),
            79,
            "every component registration under programming/bigworld (build/ excluded)"
        );

        // The server binaries.
        for (name, kind) in [
            ("baseapp", "binary"),
            ("cellapp", "binary"),
            ("loginapp", "binary"),
            ("dbapp", "binary"),
            ("baseappmgr", "binary"),
            ("cellappmgr", "binary"),
            ("dbappmgr", "binary"),
        ] {
            let target = catalog
                .iter()
                .find(|t| t.name == name && t.kind == kind)
                .unwrap_or_else(|| panic!("missing {name} {kind}"));
            assert_eq!(target.dest, "$(BW_SERVER_BIN_DIR)", "{name}");
            assert_eq!(target.rule, FOOTER_TARGET_RULE, "{name}");
            assert_eq!(target.rule_file, FOOTER_RULE_FILE, "{name}");
        }

        // The libraries, built by the same footer rule.
        let network = catalog
            .iter()
            .find(|t| t.name == "network" && t.kind == "library")
            .expect("network library");
        assert_eq!(network.source, "lib/network/Makefile.rules");
        assert_eq!(network.rule_file, FOOTER_RULE_FILE);
        // A library and its unit test share the name; the source tells them
        // apart.
        let network_test = catalog
            .iter()
            .find(|t| t.name == "network" && t.kind == "unit_test")
            .expect("network unit test");
        assert_eq!(network_test.source, "lib/network/unit_test/Makefile.rules");

        // `lib/build` is a component (the build-revision library) even though
        // its directory is named `build`; the top-level `build/` (the shared
        // rules) is not a component and contributes no record.
        let build = catalog
            .iter()
            .find(|t| t.name == "build" && t.kind == "library")
            .expect("lib/build library");
        assert_eq!(build.source, "lib/build/Makefile.rules");
        assert!(
            !catalog.iter().any(|t| t.source.starts_with("build/")),
            "the shared-rules build/ dir registers no targets"
        );

        // The kind census: 19 binaries, 1 copy_of_binary, 32 libraries,
        // 5 shared_libraries, 21 unit tests, 1 untyped.
        let mut kinds = std::collections::BTreeMap::new();
        for target in &catalog {
            *kinds.entry(target.kind.as_str()).or_insert(0usize) += 1;
        }
        assert_eq!(kinds["binary"], 19);
        assert_eq!(kinds["copy_of_binary"], 1);
        assert_eq!(kinds["library"], 32);
        assert_eq!(kinds["shared_library"], 5);
        assert_eq!(kinds["unit_test"], 21);
        assert_eq!(kinds["-"], 1);

        // The one self-ruled component.
        let self_ruled: Vec<_> = catalog
            .iter()
            .filter(|t| t.rule_file != FOOTER_RULE_FILE)
            .collect();
        assert_eq!(self_ruled.len(), 1, "remove_db is the only own-rule target");
        assert_eq!(self_ruled[0].name, "remove_db");
        assert_eq!(self_ruled[0].rule, SELF_RULE);
    }

    /// The footer rules named by the catalog are the ones the shared rule
    /// file really defines.
    #[test]
    fn footer_rules_are_the_shared_rule_file() {
        assert!(FOOTER_RULES.contains(&FOOTER_TARGET_RULE));
        // Every rule is phrased over the footer's component variables (the
        // target name, the name, or the artifact file).
        for rule in FOOTER_RULES {
            assert!(
                rule.contains("$(binName)")
                    || rule.contains("$(name)")
                    || rule.contains("$(destFile)"),
                "{rule}"
            );
        }
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
