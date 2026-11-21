//! CMake and Windows build-target catalog (BW-00-01-02).
//!
//! The reference's CMake build (`programming/bigworld/CMakeLists.txt`) does not
//! list its targets in one place. A `BW_CMAKE_TARGET` option selects one of
//! the named configurations, each defined by a
//! `build/cmake/BWConfiguration_<name>.cmake` file that is globbed from
//! [`CONFIG_DIR`] (`CMAKE_MODULE_PATH`). This module scans those files and
//! renders the inventory the acceptance requires: for every configuration it
//! records the build platforms, the `ADD_DEFINITIONS` macros, the feature /
//! memory-hook switches, the library and binary project lists, the unit-test
//! gate, the Linux start/stop/display commands and the conditional guards.
//!
//! Reference model (the parts the catalog mirrors):
//!
//! * Platforms — [`platforms`] derives the set from the root `CMakeLists.txt`:
//!   `BW_IS_REMOTE_ONLY` selects a remote 64-bit Linux build
//!   (`BW_PLATFORM_LINUX`/`BW_ARCH_64`), `WIN32` with pointer size 32 selects
//!   `win32` (`BW_PLATFORM_WINDOWS`/`BW_ARCH_32`), `WIN32` with pointer size 64
//!   selects `win64` (`BW_PLATFORM_WINDOWS`/`BW_ARCH_64`), anything else is a
//!   fatal error. Every configuration is buildable in all three, so the catalog
//!   records `win32,win64,linux` for each; the *conditional* platform guards
//!   (e.g. server's `IF( NOT MSVC )` cellapp_extension) are recorded in
//!   [`CmakeTarget::conditions`] instead.
//! * Configurations — one record per `BWConfiguration_<name>.cmake`, named
//!   `<name>`. The `*_blob` configurations are thin wrappers that
//!   `INCLUDE()` a base configuration and set `BW_BLOB_CONFIG ON`; the base is
//!   recorded in [`CmakeTarget::includes`].
//! * Defines — every `ADD_DEFINITIONS( -DX )` value (top-level and
//!   conditional) becomes one entry in [`CmakeTarget::defines`], e.g.
//!   `MF_SERVER`, `MF_CONFIG="Hybrid"`, `_SECURE_SCL=1`, `USE_MEMHOOK`,
//!   `BWCLIENT_AS_PYTHON_MODULE=1`.
//! * Flags — every `SET( BW_<FLAG> <val> )` and `SET( USE_MEMHOOK <val> )`
//!   becomes `FLAG=val` in [`CmakeTarget::flags`], e.g.
//!   `BW_PYTHON_AS_SOURCE=OFF`, `BW_PCH_SUPPORT=OFF`, `USE_MEMHOOK=ON`,
//!   `BW_BLOB_CONFIG=ON`.
//! * Libraries/binaries — the *first* `SET( BW_LIBRARY_PROJECTS ... )` /
//!   `SET( BW_BINARY_PROJECTS ... )` block is the base list; each `name dir`
//!   pair becomes `name@dir` in [`CmakeTarget::libraries`] /
//!   [`CmakeTarget::binaries`]. Later conditional appends
//!   (`IF( BW_FMOD_SUPPORT )` etc.) are captured as conditions rather than
//!   merged, so the base list stays a faithful snapshot of the unconditional
//!   project set.
//! * Unit tests — the root default is `OPTION( BW_UNIT_TESTS_ENABLED ... ON )`;
//!   a configuration that gates on it records `on`, one that drops it records
//!   `off`, and one that leaves the default alone records `default`.
//! * Linux commands — `BW_TARGET_LINUX_COMMAND( name "cmd" )` becomes
//!   `name:cmd` in [`CmakeTarget::linux_commands`].
//!
//! The committed catalog (`cases/cmake_targets.txt`) is regenerated with the
//! `scan-cmake-targets` binary and re-parsed by the catalog test, so the file
//! and the scanner cannot drift.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/cmake_targets.txt");

/// `CMAKE_MODULE_PATH` — where the configuration files live, relative to the
/// bigworld source dir.
const CONFIG_DIR: &str = "build/cmake";

/// The directory the reference globs its `BWConfiguration_*.cmake` from.
fn config_dir(bw_source: &Path) -> PathBuf {
    bw_source.join(CONFIG_DIR)
}

/// The platforms the root `CMakeLists.txt` selects between, in catalog order.
///
/// `BW_IS_REMOTE_ONLY` → `linux` (64-bit remote); `WIN32` + 32-bit pointer →
/// `win32`; `WIN32` + 64-bit pointer → `win64`; anything else is fatal. Every
/// configuration participates in all three.
const PLATFORMS: [&str; 3] = ["win32", "win64", "linux"];

/// One CMake configuration target (`BWConfiguration_<name>.cmake`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmakeTarget {
    /// The configuration name (`<name>` from `BWConfiguration_<name>.cmake`).
    pub name: String,
    /// The build platforms, in catalog order (see [`PLATFORMS`]).
    pub platforms: Vec<String>,
    /// `ADD_DEFINITIONS( -DX )` values, with the leading `-D` stripped.
    pub defines: Vec<String>,
    /// `SET( BW_* / USE_MEMHOOK <val> )` as `NAME=val`.
    pub flags: Vec<String>,
    /// `BW_LIBRARY_PROJECTS` base entries as `name@dir`.
    pub libraries: Vec<String>,
    /// `BW_BINARY_PROJECTS` base entries as `name@dir`.
    pub binaries: Vec<String>,
    /// Unit-test gate: `on`, `off` (explicit `BW_UNIT_TESTS_ENABLED`), or
    /// `default` (the root `OPTION ... ON` is inherited).
    pub unit_tests: String,
    /// `BW_TARGET_LINUX_COMMAND( name "cmd" )` as `name:cmd`.
    pub linux_commands: Vec<String>,
    /// Base configurations an `INCLUDE()`d (the `*_blob` wrappers).
    pub includes: Vec<String>,
    /// The `IF( ... )` guard expressions the configuration uses, in file order.
    pub conditions: Vec<String>,
}

impl CmakeTarget {
    /// A configuration with its name set and every list empty (`default`
    /// unit-test state, no guard).
    fn new(name: String) -> Self {
        Self {
            name,
            platforms: PLATFORMS.iter().map(|s| s.to_string()).collect(),
            defines: Vec::new(),
            flags: Vec::new(),
            libraries: Vec::new(),
            binaries: Vec::new(),
            unit_tests: "default".to_string(),
            linux_commands: Vec::new(),
            includes: Vec::new(),
            conditions: Vec::new(),
        }
    }
}

/// Strip `#` line comments, keeping the rest of each line.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let cut = line.find('#').unwrap_or(line.len());
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// Take the token starting at byte `pos` (after optional whitespace) and return
/// it with the next position.
///
/// `(` and `)` are standalone tokens: CMake writes `ADD_DEFINITIONS( ... )`
/// and `SET( ... )` with no space before the paren, so a whitespace-only split
/// would yield `ADD_DEFINITIONS(` as one token and never match the function
/// name.
fn next_token(text: &str, mut pos: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    while pos < bytes.len() && (bytes[pos] as char).is_whitespace() {
        pos += 1;
    }
    if pos >= bytes.len() {
        return None;
    }
    if bytes[pos] == b'(' || bytes[pos] == b')' {
        return Some((&text[pos..pos + 1], pos + 1));
    }
    let start = pos;
    while pos < bytes.len()
        && !(bytes[pos] as char).is_whitespace()
        && bytes[pos] != b'('
        && bytes[pos] != b')'
    {
        pos += 1;
    }
    Some((&text[start..pos], pos))
}

/// Find the `(` matching the open paren at `open` (byte index) in `text`,
/// returning the byte index just past the matching `)`.
fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes[open], b'(');
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract the contents of the first base `SET( <var> ... )` call in `text`
/// whose variable is `var`. Returns the value list with the variable itself
/// stripped. An append-form call (`SET( <var> ${<var>} more ... )`) is a
/// conditional append, not the base list, so it is skipped in favour of the
/// first unconditional assignment.
fn first_set_body(text: &str, var: &str) -> Option<String> {
    let self_ref = format!("${{{}}}", var);
    let mut pos = 0usize;
    while let Some((token, next)) = next_token(text, pos) {
        if token == "SET" {
            let (paren_open, next2) = next_token(text, next)?;
            if paren_open == "(" {
                let close = matching_paren(text, next2 - 1)?;
                let inner = &text[next2..close - 1];
                if let Some((head, head_end)) = next_token(inner, 0) {
                    if head == var {
                        let body = inner[head_end..].to_string();
                        let is_append =
                            next_token(&body, 0).is_some_and(|(first, _)| first == self_ref);
                        if !is_append {
                            return Some(body);
                        }
                    }
                }
                pos = close;
                continue;
            }
        }
        pos = next;
    }
    None
}

/// Split a `BW_*_PROJECTS` list body into `name@dir` pairs. The list is
/// whitespace-separated `name dir` pairs; a lone trailing token (a variable
/// reference such as `${BW_SERVER_UNIT_TEST_LIBRARIES}`) is kept as a bare
/// entry.
fn project_pairs(body: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut pos = 0usize;
    while let Some((tok, next)) = next_token(body, pos) {
        tokens.push(tok.to_string());
        pos = next;
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < tokens.len() {
        out.push(format!("{}@{}", tokens[i], tokens[i + 1]));
        i += 2;
    }
    if i < tokens.len() {
        out.push(tokens[i].clone());
    }
    out
}

/// Parse one `BWConfiguration_<name>.cmake` file into a [`CmakeTarget`].
fn parse_config(name: &str, text: &str) -> CmakeTarget {
    let mut target = CmakeTarget::new(name.to_string());
    let clean = strip_comments(text);

    // `ADD_DEFINITIONS( -DX ... )` — each `-D`-prefixed token.
    let mut pos = 0usize;
    while let Some((token, next)) = next_token(&clean, pos) {
        if token == "ADD_DEFINITIONS" {
            if let Some((paren_open, next2)) = next_token(&clean, next) {
                if paren_open == "(" {
                    if let Some(close) = matching_paren(&clean, next2 - 1) {
                        let inner = &clean[next2..close - 1];
                        let mut ipos = 0usize;
                        while let Some((tok, inext)) = next_token(inner, ipos) {
                            if let Some(def) = tok.strip_prefix("-D") {
                                target.defines.push(def.to_string());
                            }
                            ipos = inext;
                        }
                        pos = close;
                        continue;
                    }
                }
            }
        }
        pos = next;
    }

    // `SET( BW_* <val> )` / `SET( USE_MEMHOOK <val> )` — scalar assignments
    // only (exactly one value token); list assignments such as
    // `BW_SERVER_UNIT_TEST_LIBRARIES` are not feature flags.
    pos = 0usize;
    while let Some((token, next)) = next_token(&clean, pos) {
        if token == "SET" {
            if let Some((paren_open, next2)) = next_token(&clean, next) {
                if paren_open == "(" {
                    if let Some(close) = matching_paren(&clean, next2 - 1) {
                        let inner = &clean[next2..close - 1];
                        let mut tokens = Vec::new();
                        let mut ipos = 0usize;
                        while let Some((tok, inext)) = next_token(inner, ipos) {
                            tokens.push(tok);
                            ipos = inext;
                        }
                        if tokens.len() == 2
                            && (tokens[0].starts_with("BW_") || tokens[0] == "USE_MEMHOOK")
                            && !tokens[0].ends_with("_PROJECTS")
                        {
                            target.flags.push(format!("{}={}", tokens[0], tokens[1]));
                        }
                        pos = close;
                        continue;
                    }
                }
            }
        }
        pos = next;
    }

    // Base project lists — the first SET of each.
    if let Some(body) = first_set_body(&clean, "BW_LIBRARY_PROJECTS") {
        target.libraries = project_pairs(&body);
    }
    if let Some(body) = first_set_body(&clean, "BW_BINARY_PROJECTS") {
        target.binaries = project_pairs(&body);
    }

    // Unit-test gate.
    if clean.contains("BW_UNIT_TESTS_ENABLED") {
        // The gate may be `IF( BW_UNIT_TESTS_ENABLED )` (on) or
        // `IF( NOT BW_UNIT_TESTS_ENABLED )` (off); the enclosing config decides.
        target.unit_tests = if clean.contains("IF( NOT BW_UNIT_TESTS_ENABLED )")
            || clean.contains("IF( NOT BW_UNIT_TESTS_ENABLED)")
        {
            "off".to_string()
        } else {
            "on".to_string()
        };
    }

    // Linux commands: `BW_TARGET_LINUX_COMMAND( name "cmd" )`.
    pos = 0usize;
    while let Some((token, next)) = next_token(&clean, pos) {
        if token == "BW_TARGET_LINUX_COMMAND" {
            if let Some((paren_open, next2)) = next_token(&clean, next) {
                if paren_open == "(" {
                    if let Some(close) = matching_paren(&clean, next2 - 1) {
                        let inner = &clean[next2..close - 1];
                        if let Some((name, after)) = next_token(inner, 0) {
                            let cmd = inner[after..].trim().trim_matches('"');
                            target.linux_commands.push(format!("{}:{}", name, cmd));
                        }
                        pos = close;
                        continue;
                    }
                }
            }
        }
        pos = next;
    }

    // `INCLUDE( BWConfiguration_<x> )` base configs.
    pos = 0usize;
    while let Some((token, next)) = next_token(&clean, pos) {
        if token == "INCLUDE" {
            if let Some((paren_open, next2)) = next_token(&clean, next) {
                if paren_open == "(" {
                    if let Some(close) = matching_paren(&clean, next2 - 1) {
                        let inner = &clean[next2..close - 1];
                        if let Some((inc, _)) = next_token(inner, 0) {
                            if let Some(base) = inc.strip_prefix("BWConfiguration_") {
                                let base = base.strip_suffix(".cmake").unwrap_or(base);
                                target.includes.push(base.to_string());
                            }
                        }
                        pos = close;
                        continue;
                    }
                }
            }
        }
        pos = next;
    }

    // `IF( ... )` guard expressions (in file order, deduped).
    let mut seen = std::collections::BTreeSet::new();
    let mut ipos = 0usize;
    while let Some((token, next)) = next_token(&clean, ipos) {
        if token == "IF" {
            if let Some((paren_open, next2)) = next_token(&clean, next) {
                if paren_open == "(" {
                    if let Some(close) = matching_paren(&clean, next2 - 1) {
                        let inner = clean[next2..close - 1].trim().to_string();
                        if seen.insert(inner.clone()) {
                            target.conditions.push(inner);
                        }
                        ipos = close;
                        continue;
                    }
                }
            }
        }
        ipos = next;
    }

    target
}

/// Scan a reference checkout's CMake configurations.
///
/// `bw_source` is the bigworld source directory (`programming/bigworld`). The
/// configurations are globbed from `build/cmake/BWConfiguration_*.cmake`,
/// sorted by name for a deterministic catalog.
pub fn scan_reference(bw_source: &Path) -> Vec<CmakeTarget> {
    let dir = config_dir(bw_source);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.expect("read_dir entry").path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("BWConfiguration_") && n.ends_with(".cmake"))
        })
        .collect();
    files.sort();

    let mut out = Vec::new();
    for file in files {
        let file_name = file.file_name().unwrap().to_string_lossy().into_owned();
        let name = file_name
            .trim_start_matches("BWConfiguration_")
            .trim_end_matches(".cmake")
            .to_string();
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        out.push(parse_config(&name, &text));
    }
    out
}

/// Render the scan into the tab-separated catalog text.
///
/// One record per configuration, fields in [`CmakeTarget`] field order. A
/// multi-valued field is comma-separated; an empty one is `-`. A record is a
/// single line (commands and values may contain no tabs or newlines).
pub fn render(targets: &[CmakeTarget]) -> String {
    let mut out = String::new();
    let header = [
        "config",
        "platforms",
        "defines",
        "flags",
        "libraries",
        "binaries",
        "unit_tests",
        "linux_commands",
        "includes",
        "conditions",
    ];
    out.push_str(&header.join("\t"));
    out.push('\n');
    for t in targets {
        let join = |v: &[String]| {
            if v.is_empty() {
                "-".to_string()
            } else {
                v.join(",")
            }
        };
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            t.name,
            t.platforms.join(","),
            join(&t.defines),
            join(&t.flags),
            join(&t.libraries),
            join(&t.binaries),
            t.unit_tests,
            join(&t.linux_commands),
            join(&t.includes),
            join(&t.conditions),
        );
    }
    out
}

/// Parse the catalog text back into [`CmakeTarget`]s (inverse of [`render`]).
pub fn parse_catalog(text: &str) -> Vec<CmakeTarget> {
    let mut lines = text.lines();
    // Skip the header line.
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let name = fields.next().unwrap_or("").to_string();
        let platforms = fields
            .next()
            .unwrap_or("")
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let defines = split_list(fields.next().unwrap_or(""));
        let flags = split_list(fields.next().unwrap_or(""));
        let libraries = split_list(fields.next().unwrap_or(""));
        let binaries = split_list(fields.next().unwrap_or(""));
        let unit_tests = fields.next().unwrap_or("default").to_string();
        let linux_commands = split_list(fields.next().unwrap_or(""));
        let includes = split_list(fields.next().unwrap_or(""));
        let conditions = split_list(fields.next().unwrap_or(""));
        out.push(CmakeTarget {
            name,
            platforms,
            defines,
            flags,
            libraries,
            binaries,
            unit_tests,
            linux_commands,
            includes,
            conditions,
        });
    }
    out
}

/// Split a comma-separated catalog field; `-` means empty.
fn split_list(field: &str) -> Vec<String> {
    if field == "-" {
        return Vec::new();
    }
    field
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<CmakeTarget> {
    parse_catalog(CATALOG_TEXT)
}

/// The configuration files the reference globs (for the CLI's report line).
pub fn config_dir_name() -> &'static str {
    CONFIG_DIR
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal configuration exercising every construct the parser reads,
    /// laid out like the real files: base project lists unconditional, the
    /// unit-test libraries under their own variable, and feature-flag appends
    /// in self-referencing form after the base.
    const SAMPLE: &str = "ADD_DEFINITIONS( -DMF_SERVER )\n\
        ADD_DEFINITIONS( -DMF_CONFIG=\"Hybrid\" )\n\
        ADD_DEFINITIONS( -D_SECURE_SCL=1 )\n\
        BW_TARGET_LINUX_COMMAND( start_server\n\
        \t\"python ctl start\" )\n\
        SET( BW_PYTHON_AS_SOURCE OFF )\n\
        SET( USE_MEMHOOK ON )\n\
        SET( BW_LIBRARY_PROJECTS\n\
        \tbase\t\tserver/base\n\
        \tcellapp\tserver/cellapp\n\
        \t)\n\
        SET( BW_BINARY_PROJECTS\n\
        \tbaseapp\tservice/baseapp\n\
        \tcellapp\tservice/cellapp\n\
        \t)\n\
        IF( BW_UNIT_TESTS_ENABLED )\n\
        \tSET( BW_SERVER_UNIT_TEST_LIBRARIES\n\
        \t\tunit_test_lib\t\tlib/unit_test_lib\n\
        \t\t)\n\
        ENDIF()\n\
        IF( BW_FMOD_SUPPORT )\n\
        \tSET( BW_LIBRARY_PROJECTS ${BW_LIBRARY_PROJECTS}\n\
        \tfmod\tsounds/fmod )\n\
        ENDIF()\n";

    #[test]
    fn parse_extracts_defines_flags_and_commands() {
        let target = parse_config("sample", SAMPLE);
        assert_eq!(
            target.defines,
            vec!["MF_SERVER", "MF_CONFIG=\"Hybrid\"", "_SECURE_SCL=1"]
        );
        assert!(
            target
                .flags
                .contains(&"BW_PYTHON_AS_SOURCE=OFF".to_string())
        );
        assert!(target.flags.contains(&"USE_MEMHOOK=ON".to_string()));
        assert_eq!(target.linux_commands, vec!["start_server:python ctl start"]);
        assert_eq!(target.unit_tests, "on");
    }

    #[test]
    fn parse_takes_first_library_block_and_keeps_conditions() {
        let target = parse_config("sample", SAMPLE);
        assert_eq!(
            target.libraries,
            vec!["base@server/base", "cellapp@server/cellapp"]
        );
        assert_eq!(
            target.binaries,
            vec!["baseapp@service/baseapp", "cellapp@service/cellapp"]
        );
        assert_eq!(
            target.conditions,
            vec!["BW_UNIT_TESTS_ENABLED", "BW_FMOD_SUPPORT"]
        );
    }

    #[test]
    fn render_then_parse_round_trips() {
        let original = vec![parse_config("a", SAMPLE), parse_config("b", "")];
        let text = render(&original);
        assert_eq!(parse_catalog(&text), original);
    }

    /// The committed catalog records every configuration with its platforms and
    /// the key server/client macros (the acceptance's "平台及启用宏").
    #[test]
    fn committed_catalog_records_platforms_and_macros() {
        let catalog = catalog();
        assert_eq!(catalog.len(), 11, "all BWConfiguration_*.cmake files");
        for target in &catalog {
            assert_eq!(target.platforms, vec!["win32", "win64", "linux"]);
        }
        let server = catalog
            .iter()
            .find(|t| t.name == "server")
            .expect("server config");
        assert!(server.defines.contains(&"MF_SERVER".to_string()));
        assert!(server.defines.iter().any(|d| d.starts_with("MF_CONFIG=")));
        assert_eq!(server.unit_tests, "on");
        assert!(server.binaries.iter().any(|b| b.starts_with("cellapp@")));
        let client = catalog
            .iter()
            .find(|t| t.name == "client")
            .expect("client config");
        assert!(client.defines.contains(&"BW_CLIENT".to_string()));
        let client_blob = catalog
            .iter()
            .find(|t| t.name == "client_blob")
            .expect("client_blob");
        assert!(client_blob.flags.contains(&"BW_BLOB_CONFIG=ON".to_string()));
        assert_eq!(client_blob.includes, vec!["client"]);
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
        if !bw_source.join(CONFIG_DIR).is_dir() {
            eprintln!("reference checkout absent; skipping rescan");
            return;
        }
        let scan = scan_reference(&bw_source);
        assert_eq!(render(&scan), CATALOG_TEXT);
    }
}
