//! Workspace dependency-direction checker (BW-16-01-01, BW-16-02-05).
//!
//! The direction rule is the one fixed in the root `Cargo.toml` comment:
//! `apps -> role runtimes -> contracts & domain libs -> foundation`.
//! Crates are assigned to numbered layers; every internal dependency edge
//! must point at an equal or lower layer, and the graph must stay acyclic.
//! Same-layer edges are only allowed between domain libraries (layer 2),
//! which may compose among themselves; service runtimes (layer 3) and
//! application entries (layer 4) must never depend on a peer crate.
//!
//! The check runs as part of `cargo test` (see [`check_workspace`]) so new
//! crates are forced to register a layer and keep the direction rule.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Layer index: foundation sits at the bottom.
pub type Layer = u8;

/// Foundation: ids, clocks, math, byte codecs. Depends on nothing internal.
pub const LAYER_FOUNDATION: Layer = 0;

/// Platform: OS interfaces. May depend on foundation.
pub const LAYER_PLATFORM: Layer = 1;

/// Contracts and domain libraries. May compose among themselves, acyclic.
pub const LAYER_DOMAIN: Layer = 2;

/// Role and tool runtimes. Never depend on a peer runtime.
pub const LAYER_ROLE_RUNTIME: Layer = 3;

/// Application entries and test support. Top of the stack.
pub const LAYER_APPLICATION: Layer = 4;

/// The layer registry. Every workspace member crate must appear here, so a
/// new crate fails the check until its layer is decided (BW-16-02-05).
pub fn layer_of(crate_name: &str) -> Option<Layer> {
    match crate_name {
        "foundation" => Some(LAYER_FOUNDATION),
        "platform" => Some(LAYER_PLATFORM),
        "contracts" | "entitydef" | "mercury" | "python-host" | "resources" | "db"
        | "db-postgres" | "asset-pipeline" | "plugin-host" | "chunk" | "load"
        | "compiled-space" | "physics" | "scene" | "waypoint" => {
            Some(LAYER_DOMAIN)
        }
        "server-runtime" => Some(LAYER_ROLE_RUNTIME),
        "boundless-server" | "client" | "test-support" => Some(LAYER_APPLICATION),
        _ => None,
    }
}

/// Why a dependency edge or workspace member was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// A dependency names a workspace crate missing from the layer registry.
    UnregisteredCrate,
    /// An edge points at a higher layer, against the dependency direction.
    UpwardDependency,
    /// Same-layer edge outside the domain layer.
    SameTierDependency,
    /// The graph contains a dependency cycle.
    DependencyCycle,
    /// Scan and `cargo metadata` disagree about the member set.
    MemberMismatch,
}

impl fmt::Display for ViolationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnregisteredCrate => write!(f, "unregistered crate"),
            Self::UpwardDependency => write!(f, "upward dependency"),
            Self::SameTierDependency => write!(f, "same-layer dependency"),
            Self::DependencyCycle => write!(f, "dependency cycle"),
            Self::MemberMismatch => write!(f, "member mismatch"),
        }
    }
}

/// One rule violation, anchored at a manifest line when possible so the
/// report is directly locatable (BW-16-02-05).
#[derive(Debug, Clone)]
pub struct Violation {
    pub kind: ViolationKind,
    pub manifest: PathBuf,
    pub line: usize,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}",
            self.manifest.display(),
            self.line,
            self.kind,
            self.detail
        )
    }
}

/// The dependency facts extracted from one member manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Manifest path, for reports.
    pub path: PathBuf,
    /// `[package] name`.
    pub name: Option<String>,
    /// Normal dependency entries as `(dep name, line)`.
    pub deps: Vec<(String, usize)>,
    /// Dev dependency entries as `(dep name, line)`.
    pub dev_deps: Vec<(String, usize)>,
}

impl Manifest {
    /// Parse a manifest from its text, tracking line numbers of entries.
    pub fn parse(path: PathBuf, text: &str) -> Self {
        let mut name = None;
        let mut deps = Vec::new();
        let mut dev_deps = Vec::new();
        let mut section = String::new();

        for (index, raw) in text.lines().enumerate() {
            let line_no = index + 1;
            let trimmed = raw.trim();

            if trimmed.starts_with('[') {
                section = trimmed.trim_matches(|c| c == '[' || c == ']').to_string();
                continue;
            }

            if section == "package" {
                if let Some(value) = quoted_value(trimmed, "name") {
                    name = Some(value);
                }
            } else if is_dependency_table(&section)
                && let Some(key) = entry_key(trimmed)
            {
                if section.contains("dev-dependencies") {
                    dev_deps.push((key, line_no));
                } else {
                    deps.push((key, line_no));
                }
            }
        }

        Self {
            path,
            name,
            deps,
            dev_deps,
        }
    }
}

/// True inside `[dependencies]`, `[build-dependencies]`, `[dev-dependencies]`
/// and `[target.'cfg(...)'.dependencies]` style tables.
fn is_dependency_table(section: &str) -> bool {
    section == "dependencies"
        || section == "dev-dependencies"
        || section == "build-dependencies"
        || section.ends_with(".dependencies")
}

/// The key of `key = value`, or `None` for blank or comment lines.
fn entry_key(trimmed: &str) -> Option<String> {
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(key.to_string())
}

/// Extract `key = "value"` when the value is a quoted string.
fn quoted_value(trimmed: &str, key: &str) -> Option<String> {
    let (name, value) = trimmed.split_once('=')?;
    if name.trim() != key {
        return None;
    }
    let value = value.trim().strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_string())
}

/// Paths listed in `[workspace] members`, relative to the root.
fn member_paths(root_text: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_members = false;

    for raw in root_text.lines() {
        let trimmed = raw.trim();

        if trimmed.starts_with('[') {
            // A new table closes any open members array.
            in_members = false;
            continue;
        }

        if trimmed.starts_with("members") && trimmed.contains('[') {
            in_members = true;
        }

        if in_members {
            for token in trimmed.split(',') {
                for piece in token.split('"') {
                    let candidate = piece.trim();
                    if !candidate.is_empty() && !candidate.starts_with('#') {
                        members.push(candidate.to_string());
                    }
                }
            }
            if trimmed.contains(']') {
                in_members = false;
            }
        }
    }

    members
        .into_iter()
        .filter(|m| !m.contains(']') && !m.contains('[') && !m.starts_with('='))
        .collect()
}

/// Read the root manifest and every member manifest it lists.
pub fn scan_workspace(root: &Path) -> Vec<Manifest> {
    let root_manifest = root.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&root_manifest) else {
        return Vec::new();
    };

    member_paths(&text)
        .into_iter()
        .filter_map(|relative| {
            let path = root.join(relative).join("Cargo.toml");
            let text = std::fs::read_to_string(&path).ok()?;
            Some(Manifest::parse(path, &text))
        })
        .collect()
}

/// Run every layering rule over the scanned manifests.
pub fn check(manifests: &[Manifest]) -> Vec<Violation> {
    let by_name: BTreeMap<&str, &Manifest> = manifests
        .iter()
        .filter_map(|m| m.name.as_deref().map(|n| (n, m)))
        .collect();

    let mut violations = Vec::new();

    // Registry completeness and edge direction.
    for manifest in manifests {
        let own = manifest.name.as_deref().unwrap_or_default();
        for (dep, line) in &manifest.deps {
            if !by_name.contains_key(dep.as_str()) {
                continue; // external dependency
            }
            let (Some(own_layer), Some(dep_layer)) = (layer_of(own), layer_of(dep)) else {
                violations.push(Violation {
                    kind: ViolationKind::UnregisteredCrate,
                    manifest: manifest.path.clone(),
                    line: *line,
                    detail: format!("'{own}' or '{dep}' is missing from the layer registry"),
                });
                continue;
            };
            if dep_layer > own_layer {
                violations.push(Violation {
                    kind: ViolationKind::UpwardDependency,
                    manifest: manifest.path.clone(),
                    line: *line,
                    detail: format!(
                        "'{own}' (layer {own_layer}) depends on '{dep}' (layer {dep_layer}); \
                         direction is apps -> runtimes -> contracts & domain -> foundation"
                    ),
                });
            } else if dep_layer == own_layer && own_layer != LAYER_DOMAIN {
                violations.push(Violation {
                    kind: ViolationKind::SameTierDependency,
                    manifest: manifest.path.clone(),
                    line: *line,
                    detail: format!(
                        "'{own}' and '{dep}' share layer {own_layer}; \
                         only domain libraries (layer {LAYER_DOMAIN}) may compose"
                    ),
                });
            }
        }
    }

    // Cycles over the internal edge set.
    let edges: BTreeMap<&str, Vec<&str>> = manifests
        .iter()
        .filter_map(|m| {
            m.name.as_deref().map(|n| {
                (
                    n,
                    m.deps
                        .iter()
                        .filter(|(dep, _)| by_name.contains_key(dep.as_str()))
                        .map(|(dep, _)| dep.as_str())
                        .collect::<Vec<_>>(),
                )
            })
        })
        .collect();
    for cycle in find_cycles(&edges) {
        let detail = format!("cycle: {}", cycle.join(" -> "));
        for name in &cycle {
            for manifest in manifests {
                if manifest.name.as_deref() == Some(*name) {
                    violations.push(Violation {
                        kind: ViolationKind::DependencyCycle,
                        manifest: manifest.path.clone(),
                        line: 1,
                        detail: detail.clone(),
                    });
                }
            }
        }
    }

    violations
}

/// Detect dependency cycles as strongly connected components.
fn find_cycles<'a>(edges: &BTreeMap<&'a str, Vec<&'a str>>) -> Vec<Vec<&'a str>> {
    struct Node {
        index: Option<usize>,
        low: usize,
        on_stack: bool,
    }

    let mut nodes: BTreeMap<&str, Node> = edges
        .keys()
        .map(|k| {
            (
                *k,
                Node {
                    index: None,
                    low: 0,
                    on_stack: false,
                },
            )
        })
        .collect();
    let mut stack: Vec<&str> = Vec::new();
    let mut counter = 0usize;
    let mut cycles = Vec::new();

    fn visit<'a>(
        name: &'a str,
        edges: &BTreeMap<&'a str, Vec<&'a str>>,
        nodes: &mut BTreeMap<&'a str, Node>,
        stack: &mut Vec<&'a str>,
        counter: &mut usize,
        cycles: &mut Vec<Vec<&'a str>>,
    ) {
        let index = *counter;
        *counter += 1;
        {
            let node = nodes.get_mut(name).expect("node exists");
            node.index = Some(index);
            node.low = index;
            node.on_stack = true;
        }
        stack.push(name);

        if let Some(neighbors) = edges.get(name) {
            for &next in neighbors {
                match nodes.get(next).and_then(|n| n.index) {
                    // Not visited yet: recurse and fold its low-link back.
                    None => {
                        visit(next, edges, nodes, stack, counter, cycles);
                        let next_low = nodes.get(next).map(|n| n.low).unwrap_or(0);
                        let node = nodes.get_mut(name).expect("node exists");
                        node.low = node.low.min(next_low);
                    }
                    // On the current stack: part of the same component.
                    Some(next_index) => {
                        if nodes.get(next).is_some_and(|n| n.on_stack) {
                            let node = nodes.get_mut(name).expect("node exists");
                            node.low = node.low.min(next_index);
                        }
                    }
                }
            }
        }

        if nodes
            .get(name)
            .is_some_and(|n| n.low == n.index.expect("set on entry"))
        {
            let mut component = Vec::new();
            while let Some(top) = stack.pop() {
                nodes.get_mut(top).expect("node exists").on_stack = false;
                component.push(top);
                if top == name {
                    break;
                }
            }
            let is_cycle = component.len() > 1
                || edges
                    .get(name)
                    .is_some_and(|neighbors| neighbors.contains(&name));
            if is_cycle {
                component.reverse();
                cycles.push(component);
            }
        }
    }

    let roots: Vec<&str> = edges.keys().copied().collect();
    for name in roots {
        if nodes.get(name).is_some_and(|n| n.index.is_none()) {
            visit(
                name,
                edges,
                &mut nodes,
                &mut stack,
                &mut counter,
                &mut cycles,
            );
        }
    }

    cycles
}

/// Compare the scan against `cargo metadata` so the manifest parser cannot
/// silently drift from cargo's view of the workspace (BW-16-01-01).
pub fn cross_check_members(root: &Path, manifests: &[Manifest]) -> Vec<Violation> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output();

    let Ok(output) = output else {
        return vec![Violation {
            kind: ViolationKind::MemberMismatch,
            manifest: root.join("Cargo.toml"),
            line: 1,
            detail: format!("failed to run '{cargo} metadata --no-deps'"),
        }];
    };
    if !output.status.success() {
        return vec![Violation {
            kind: ViolationKind::MemberMismatch,
            manifest: root.join("Cargo.toml"),
            line: 1,
            detail: format!(
                "'{cargo} metadata --no-deps' failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        }];
    }

    let json = String::from_utf8_lossy(&output.stdout).into_owned();
    let cargo_members: BTreeSet<PathBuf> = extract_manifest_paths(&json)
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    let scanned: BTreeSet<PathBuf> = manifests
        .iter()
        .filter_map(|m| m.path.canonicalize().ok())
        .collect();

    let mut violations = Vec::new();
    for manifest in manifests {
        let Ok(path) = manifest.path.canonicalize() else {
            continue;
        };
        if !cargo_members.contains(&path) {
            violations.push(Violation {
                kind: ViolationKind::MemberMismatch,
                manifest: manifest.path.clone(),
                line: 1,
                detail: format!(
                    "scanned manifest {} is not a cargo workspace member",
                    manifest.name.as_deref().unwrap_or("?")
                ),
            });
        }
    }
    for cargo_path in cargo_members.difference(&scanned) {
        violations.push(Violation {
            kind: ViolationKind::MemberMismatch,
            manifest: cargo_path.clone(),
            line: 1,
            detail: "cargo workspace member was not discovered by the scan".to_string(),
        });
    }
    violations
}

/// Pull every `"manifest_path":"..."` value out of a metadata document.
fn extract_manifest_paths(json: &str) -> Vec<PathBuf> {
    let marker = "\"manifest_path\":\"";
    let mut paths = Vec::new();
    let mut cursor = 0;
    while let Some(start) = json[cursor..].find(marker) {
        let value_start = cursor + start + marker.len();
        if let Some(end) = json[value_start..].find('"') {
            paths.push(PathBuf::from(&json[value_start..value_start + end]));
            cursor = value_start + end + 1;
        } else {
            break;
        }
    }
    paths
}

/// Walk up from `start` to the directory holding the root workspace manifest.
pub fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    if !dir.is_dir() {
        dir = start.parent()?.to_path_buf();
    }
    loop {
        let is_workspace = std::fs::read_to_string(dir.join("Cargo.toml"))
            .is_ok_and(|text| text.lines().any(|line| line.trim() == "[workspace]"));
        if is_workspace {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// Scan the workspace at `root`, run all rules and cross-check with cargo.
pub fn check_workspace(root: &Path) -> Vec<Violation> {
    let manifests = scan_workspace(root);
    let mut violations = check(&manifests);
    violations.extend(cross_check_members(root, &manifests));
    violations
}

/// Render violations as a locatable report, one line each.
pub fn report(violations: &[Violation]) -> String {
    match violations.len() {
        0 => "layering check passed: no cycles, all edges point downward\n".to_string(),
        count => {
            let mut text = format!("layering check found {count} violation(s)\n");
            for violation in violations {
                text.push_str(&violation.to_string());
                text.push('\n');
            }
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: &str, deps: &[(&str, usize)]) -> Manifest {
        Manifest {
            path: PathBuf::from(format!("crates/{name}/Cargo.toml")),
            name: Some(name.to_string()),
            deps: deps.iter().map(|(d, l)| (d.to_string(), *l)).collect(),
            dev_deps: Vec::new(),
        }
    }

    #[test]
    fn find_workspace_root_from_a_nested_directory() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = find_workspace_root(crate_dir).expect("workspace root");
        assert!(root.join("Cargo.toml").exists());
        assert!(root.join("crates/test-support").is_dir());
    }

    #[test]
    fn layer_registry_assigns_every_current_member() {
        for name in [
            "foundation",
            "platform",
            "contracts",
            "entitydef",
            "mercury",
            "python-host",
            "resources",
            "plugin-host",
            "chunk",
            "physics",
            "scene",
            "waypoint",
            "server-runtime",
            "boundless-server",
            "test-support",
        ] {
            assert!(layer_of(name).is_some(), "{name} has no layer");
        }
    }

    #[test]
    fn upward_dependency_is_reported_at_the_dep_line() {
        // foundation reaching up into a domain library breaks the direction.
        let manifests = [
            manifest("foundation", &[("resources", 7)]),
            manifest("resources", &[]),
            manifest("server-runtime", &[("resources", 6)]),
            manifest("boundless-server", &[("server-runtime", 7)]),
        ];
        let violations = check(&manifests);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].kind, ViolationKind::UpwardDependency);
        assert_eq!(violations[0].line, 7);
        assert!(violations[0].to_string().contains("foundation"));
    }

    #[test]
    fn same_layer_edge_is_only_legal_between_domain_libs() {
        // Two registered application crates must not depend on each other.
        let manifests = [
            manifest("foundation", &[]),
            manifest("resources", &[("foundation", 7)]),
            manifest("entitydef", &[("resources", 7)]),
            manifest("boundless-server", &[("test-support", 8)]),
            manifest("test-support", &[("resources", 8)]),
        ];
        let violations = check(&manifests);
        let kinds: Vec<_> = violations.iter().map(|v| v.kind).collect();
        assert_eq!(kinds, vec![ViolationKind::SameTierDependency]);
    }

    #[test]
    fn cycles_are_detected_and_listed() {
        // A three-crate cycle among registered domain libraries.
        let manifests = [
            manifest("foundation", &[]),
            manifest("resources", &[("entitydef", 7)]),
            manifest("entitydef", &[("contracts", 7)]),
            manifest("contracts", &[("resources", 7)]),
        ];
        let violations = check(&manifests);
        assert!(
            violations
                .iter()
                .any(|v| v.kind == ViolationKind::DependencyCycle)
        );
    }

    #[test]
    fn parse_manifest_reads_sections_and_lines() {
        let text = "[package]\nname = \"demo\"\n\n[dependencies]\nfoundation = { workspace = true }\nlibc = \"0.2\"\n\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n\n[dev-dependencies]\ntest-support = { workspace = true }\n";
        let parsed = Manifest::parse(PathBuf::from("Cargo.toml"), text);
        assert_eq!(parsed.name.as_deref(), Some("demo"));
        assert_eq!(parsed.deps.len(), 3);
        assert_eq!(parsed.deps[0], ("foundation".to_string(), 5));
        assert_eq!(parsed.deps[2], ("libc".to_string(), 9));
        assert_eq!(parsed.dev_deps, vec![("test-support".to_string(), 12)]);
    }

    #[test]
    fn member_paths_reads_multiline_arrays() {
        let text = "[workspace]\nresolver = \"2\"\nmembers = [\n    \"crates/foundation\",\n    \"apps/server\",\n]\n";
        assert_eq!(
            member_paths(text),
            vec!["crates/foundation".to_string(), "apps/server".to_string()]
        );
    }

    /// BW-16-02-02: the server build must not load the graphics backend.
    /// `crates/client/src/render/` is the new-tree graphics backend (the
    /// software framebuffer; `winit-backend` is the optional windowing
    /// face). A server-side crate (`boundless-server`, `server-runtime`)
    /// may not reach `client` through *any* internal dependency edge,
    /// direct or transitive. The layering direction forbids a direct edge
    /// already; this test pins the reachable set so a transitively
    /// introduced graphics dependency (e.g. a domain lib pulling client)
    /// is caught here rather than only at the next direction check.
    #[test]
    fn server_build_loads_no_graphics_backend() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifests = scan_workspace(&root);
        let by_name: BTreeMap<&str, &Manifest> = manifests
            .iter()
            .filter_map(|m| m.name.as_deref().map(|n| (n, m)))
            .collect();

        for start in ["boundless-server", "server-runtime"] {
            // BFS over internal (workspace-member) dependencies only.
            let mut seen = BTreeSet::new();
            let mut queue = vec![start];
            let mut head = 0;
            while head < queue.len() {
                let crate_name = queue[head];
                head += 1;
                if !seen.insert(crate_name) {
                    continue;
                }
                if let Some(manifest) = by_name.get(crate_name) {
                    for (dep, _line) in &manifest.deps {
                        if by_name.contains_key(dep.as_str()) {
                            queue.push(dep.as_str());
                        }
                    }
                }
            }
            assert!(
                !seen.contains("client"),
                "{start} reaches the graphics backend 'client' through the \
                 dependency set: {seen:?}"
            );
        }
    }

    #[test]
    fn workspace_rules_hold() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let violations = check_workspace(&root);
        assert!(
            violations.is_empty(),
            "layering violations:\n{}",
            report(&violations)
        );
    }
}
