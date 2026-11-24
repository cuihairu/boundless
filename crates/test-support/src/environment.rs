//! The four per-environment path roots (BW-00-03-04).
//!
//! One *environment* is one running tree: its local resources, its
//! configuration, the original BigWorld artifacts it reads and the test
//! fixtures it writes. The reference couples most of these to the
//! executable directory and `$HOME` (`BWResource::init` falls back to
//! `~/.bwmachined.conf`, and the shipped binaries sit next to their
//! resources), so two checkouts on one machine read and write through
//! shared paths. The port separates them: every writable root an
//! environment owns lives under one uniquely named directory, and the
//! read-only original tree is named by `BW_REFERENCE` -- the same
//! variable `ci/parity.sh` reads -- instead of a baked-in location.
//!
//! The four roots of one environment:
//!
//! - `<base>/<label>-<pid>-<seq>/res` -- local resources; `server/bw.xml`
//!   inside it is the configuration, the reference's config-under-res
//!   shape (`bwconfig.cpp` opens `server/bw.xml` through `BWResource`);
//! - `BW_REFERENCE` -- the original artifact tree, resolved once and only
//!   read: two environments may share it because neither writes there;
//! - the fixture base itself -- `BOUNDLESS_TEST_TMP` redirects it, so two
//!   environments can be pointed at disjoint scratch roots.
//!
//! "Two environments never overwrite each other" is a property of the
//! writable roots, so the acceptance tests below drive two `Environment`
//! instances through the real `BwResource` and `BwConfig`.

use std::path::{Path, PathBuf};

use resources::bwresource::BwResource;
use server_runtime::config::BwConfig;

use crate::workspace::TempWorkspace;

/// Name of the environment variable naming the original artifact tree.
/// The same variable `ci/parity.sh` reads (`CI_PARITY_REFERENCE` is its
/// CI-side alias), so a local environment and the parity lane agree on
/// where the reference lives.
pub const REFERENCE_TREE_ENV: &str = "BW_REFERENCE";

/// One running tree: an isolated root plus the four paths above.
pub struct Environment {
    workspace: TempWorkspace,
}

impl Environment {
    /// Create an environment with a uniquely named root, so a kept
    /// failure archive says which environment owned it.
    pub fn new(label: &str) -> Self {
        Self {
            workspace: TempWorkspace::with_label(label),
        }
    }

    /// The environment's own root: writable state never lands anywhere
    /// else.
    pub fn root(&self) -> &Path {
        self.workspace.root()
    }

    /// The resource root (the first entry to pass as `BW_RES_PATH` or
    /// `--res`).
    pub fn res_root(&self) -> PathBuf {
        self.workspace.root().join("res")
    }

    /// The base configuration file, in the reference's config-under-res
    /// layout: `<res>/server/bw.xml`.
    pub fn config_file(&self) -> PathBuf {
        self.res_root().join("server/bw.xml")
    }

    /// The original artifact tree, when `BW_REFERENCE` names one. Read
    /// only; two environments sharing it do not overwrite each other.
    pub fn reference_tree(&self) -> Option<PathBuf> {
        reference_tree()
    }

    /// Drop `bytes` at `res/<relative>` in the environment's resource
    /// tree.
    pub fn add_res(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        self.workspace.write_file(&self.join_res(relative), bytes)
    }

    /// Write `text` at `res/<relative>`.
    pub fn add_res_text(&self, relative: &str, text: &str) -> PathBuf {
        self.add_res(relative, text.as_bytes())
    }

    /// Write `text` as `<res>/server/bw.xml`.
    pub fn add_config(&self, text: &str) -> PathBuf {
        self.add_res_text("server/bw.xml", text)
    }

    /// True when `relative` exists inside the resource tree.
    pub fn res_exists(&self, relative: &str) -> bool {
        self.workspace.exists(&self.join_res(relative))
    }

    /// Read `relative` back from the resource tree.
    pub fn res_read(&self, relative: &str) -> Vec<u8> {
        self.workspace.read(&self.join_res(relative))
    }

    /// The [`BwResource`] over this environment's res root, for the
    /// per-file APIs.
    pub fn resource(&self) -> BwResource {
        let root = self.res_root().to_string_lossy().into_owned();
        BwResource::init_paths(&[&root])
    }

    /// Load the configuration chain the way a role binary does: the
    /// personalised file comes from the passwd entry of the uid and falls
    /// back to `server/bw.xml`.
    pub fn load_config(&self) -> BwConfig {
        let mut config = BwConfig::new();
        config.init(&self.resource());
        config
    }

    /// The `join_res` path: `res/<relative>`.
    fn join_res(&self, relative: &str) -> String {
        format!("res/{relative}")
    }

    /// Keep the environment's root on drop so a failure can be
    /// inspected.
    pub fn keep(&mut self) {
        self.workspace.keep();
    }
}

/// Resolve the original artifact tree from a `BW_REFERENCE` value.
///
/// Pure, so the rule is testable without mutating process-wide state:
/// unset or empty means "no reference tree" (the parity lane reports
/// SKIP for that case).
pub fn reference_tree_from_env(value: Option<&str>) -> Option<PathBuf> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The original artifact tree named by `BW_REFERENCE`, if any.
pub fn reference_tree() -> Option<PathBuf> {
    reference_tree_from_env(std::env::var(REFERENCE_TREE_ENV).ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_env_names_the_original_tree() {
        assert_eq!(reference_tree_from_env(None), None);
        assert_eq!(reference_tree_from_env(Some("")), None);
        assert_eq!(reference_tree_from_env(Some("   ")), None);
        assert_eq!(
            reference_tree_from_env(Some("/home/me/BigWorld")),
            Some(PathBuf::from("/home/me/BigWorld"))
        );
        assert_eq!(
            reference_tree_from_env(Some("  /home/me/BigWorld  ")),
            Some(PathBuf::from("/home/me/BigWorld"))
        );
    }

    #[test]
    fn two_environments_never_share_a_writable_root() {
        let one = Environment::new("alpha");
        let two = Environment::new("beta");

        assert_ne!(one.root(), two.root());
        assert_ne!(one.res_root(), two.res_root());
        assert_ne!(one.config_file(), two.config_file());
        assert!(one.res_root().is_absolute());
        assert!(one.config_file().is_absolute());
        // Both res roots are still inside the environment's own root, the
        // containment that keeps two environments from touching each
        // other's writable state.
        assert!(one.res_root().starts_with(one.root()));
        assert!(two.res_root().starts_with(two.root()));
    }

    #[test]
    fn a_write_in_one_environment_is_invisible_to_the_other() {
        let one = Environment::new("alpha");
        let two = Environment::new("beta");

        one.add_res_text("scripts/probe.py", "VALUE = 1");
        one.add_config("<root><app><port>5000</port></app></root>");
        two.add_config("<root><app><port>6000</port></app></root>");

        // Each environment reads back only its own resource tree.
        assert!(one.res_exists("scripts/probe.py"));
        assert!(!two.res_exists("scripts/probe.py"));
        assert_eq!(one.res_read("scripts/probe.py"), b"VALUE = 1");

        // And the real config chain resolves each environment's file:
        // the section *name* is the same (`server/bw.xml`), but each chain
        // read its own copy from its own res root.
        let first = one.load_config();
        let second = two.load_config();
        assert_eq!(first.get_int("app/port", 0), 5000);
        assert_eq!(second.get_int("app/port", 0), 6000);
        // Each chain took exactly its own configuration file.
        assert_eq!(first.section_names(), ["server/bw.xml"]);
        assert_eq!(second.section_names(), ["server/bw.xml"]);
    }

    #[test]
    fn each_environment_keeps_the_config_under_its_res_root() {
        let environment = Environment::new("layout");
        let file = environment.add_config("<root/>");

        assert_eq!(file, environment.config_file());
        assert!(file.is_absolute());
        assert_eq!(file, environment.res_root().join("server/bw.xml"));
        // The reference opens configuration through the resource system,
        // so the chain resolves it from the environment's res root and is
        // not marked bad.
        let config = environment.load_config();
        assert!(!config.is_bad(&environment.resource()));
    }

    /// Both `reference_tree` arms delegate to the process-wide reader.
    /// Comparing the two instead of pinning a value keeps the test
    /// environment-independent: `BW_REFERENCE` is a process-global, and no
    /// test in this crate mutates the process environment.
    #[test]
    fn the_reference_tree_delegates_to_the_process_reader() {
        let environment = Environment::new("delegate");

        assert_eq!(
            environment.reference_tree(),
            reference_tree(),
            "the environment's view and the process reader agree"
        );
    }

    /// `keep` leaves the root behind so a failing run can be inspected,
    /// instead of the drop-driven cleanup removing the evidence.
    #[test]
    fn keep_leaves_the_root_behind_for_inspection() {
        let mut environment = Environment::new("kept-root");
        environment.add_res_text("scripts/probe.py", "VALUE = 1");
        environment.keep();

        assert!(
            environment.root().exists(),
            "a kept environment stays readable"
        );
        assert_eq!(
            environment.res_read("scripts/probe.py"),
            b"VALUE = 1",
            "the contents survive the keep"
        );
        // Drop the evidence the test itself created: `keep` only exists so a
        // *failing* run can be inspected.
        let root = environment.root().to_path_buf();
        std::fs::remove_dir_all(&root).expect("remove the kept root");
    }
}
