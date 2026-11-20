//! Shared test entry core (BW-18-04-01).
//!
//! Local and CI drive the same entry (`cargo run -p test-support --bin
//! run-tests -- ...`), and this module holds the decision logic the entry
//! is built on: which workspace crates exist and which `cargo test`
//! invocation a requested module maps to. Keeping it as library code makes
//! the mapping unit-testable without spawning anything; the binary only
//! wires arguments, runs the plan, and propagates the exit status.

/// Workspace crate names parsed from the root manifest's `members` array.
/// `apps/server` contributes `server` -- the path tail is the package name.
pub fn workspace_modules(root_text: &str) -> Vec<String> {
    let mut modules = Vec::new();
    let mut in_members = false;
    for line in root_text.lines() {
        let trimmed = line.trim();
        if in_members {
            if trimmed.starts_with('[') {
                // A new table header closes the array.
                in_members = false;
            } else {
                let entries = trimmed.split(']').next().unwrap_or(trimmed);
                push_entries(entries, &mut modules);
                if trimmed.contains(']') {
                    in_members = false;
                }
                continue;
            }
        }
        if trimmed.starts_with("members")
            && trimmed.contains('[')
            && let Some(entries) = trimmed.split('[').nth(1)
        {
            push_entries(entries.split(']').next().unwrap_or(entries), &mut modules);
            in_members = !trimmed.contains(']');
        }
    }
    modules
}

/// Split one run of comma-separated quoted paths into package names.
fn push_entries(entries: &str, modules: &mut Vec<String>) {
    for part in entries.split(',') {
        let candidate = part.trim().trim_matches('"');
        if candidate.is_empty() {
            continue;
        }
        let name = candidate.rsplit('/').next().unwrap_or(candidate);
        modules.push(name.to_string());
    }
}

/// Which slice of the suite a run executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// One crate, optionally narrowed to one test module by name filter.
    Single {
        package: String,
        filter: Option<String>,
    },
    /// The whole workspace.
    All,
}

impl Plan {
    /// The plan for a named module: it must be a workspace crate, and the
    /// error names the known ones so a typo is fixable in one read.
    pub fn for_module(
        module: &str,
        modules: &[String],
        filter: Option<&str>,
    ) -> Result<Self, String> {
        if modules.iter().any(|m| m == module) {
            Ok(Plan::Single {
                package: module.to_string(),
                filter: filter.map(str::to_string),
            })
        } else {
            Err(format!(
                "unknown module {module}; workspace crates are: {}",
                modules.join(", ")
            ))
        }
    }

    /// The cargo arguments (everything after `cargo`) this plan runs.
    pub fn cargo_args(&self) -> Vec<String> {
        match self {
            Plan::Single { package, filter } => {
                let mut args = vec!["test".to_string(), "-p".to_string(), package.clone()];
                args.extend(filter.clone());
                args
            }
            Plan::All => vec!["test".to_string(), "--workspace".to_string()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_MANIFEST: &str = r#"
[workspace]
resolver = "2"
members = [
    "crates/foundation",
    "crates/platform",
    "crates/resources",
    "crates/mercury",
    "crates/contracts",
    "crates/entitydef",
    "crates/python-host",
    "crates/server-runtime",
    "crates/test-support",
    "apps/server",
]

[workspace.package]
version = "0.1.0"
"#;

    #[test]
    fn modules_list_every_workspace_crate() {
        assert_eq!(
            workspace_modules(ROOT_MANIFEST),
            [
                "foundation",
                "platform",
                "resources",
                "mercury",
                "contracts",
                "entitydef",
                "python-host",
                "server-runtime",
                "test-support",
                "server",
            ]
        );
    }

    #[test]
    fn a_table_after_members_closes_the_array() {
        let text = "[workspace]\nmembers = [\"a\"]\n\n[profile.release]\nfoo = \"b\"\n";
        assert_eq!(workspace_modules(text), ["a"]);
    }

    #[test]
    fn a_known_module_maps_to_a_single_crate_run() {
        let modules = workspace_modules(ROOT_MANIFEST);
        assert_eq!(
            Plan::for_module("server-runtime", &modules, None),
            Ok(Plan::Single {
                package: "server-runtime".to_string(),
                filter: None,
            })
        );
    }

    #[test]
    fn an_unknown_module_error_names_the_known_ones() {
        let modules = workspace_modules(ROOT_MANIFEST);
        let error = Plan::for_module("clock", &modules, None).unwrap_err();
        assert!(error.contains("unknown module clock"));
        assert!(error.contains("test-support"), "lists known crates");
    }

    #[test]
    fn single_module_args_target_only_that_crate() {
        let plan = Plan::for_module("server-runtime", &["server-runtime".to_string()], None)
            .expect("known");
        assert_eq!(plan.cargo_args(), ["test", "-p", "server-runtime"]);
    }

    #[test]
    fn a_filter_narrows_to_one_test_module_inside_the_crate() {
        let plan = Plan::for_module("test-support", &["test-support".to_string()], Some("clock"))
            .expect("known");
        assert_eq!(plan.cargo_args(), ["test", "-p", "test-support", "clock"]);
    }

    #[test]
    fn the_full_plan_runs_the_whole_workspace() {
        assert_eq!(Plan::All.cargo_args(), ["test", "--workspace"]);
    }
}
