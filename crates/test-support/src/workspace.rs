//! Temporary workspaces and resource roots for tests (BW-18-01-01).
//!
//! The reference harness synthesises its resource root from the build
//! directory (`lib/unit_test_lib/base_resmgr_unit_test_harness.cpp`), which
//! couples tests to the checkout layout. Here a fixture creates an isolated
//! directory instead, so tests run in parallel without touching each other
//! or the working tree.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Leaf name of the fixture base directory.
pub const BASE_DIRECTORY: &str = "boundless-test";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// The per-environment fixture base directory (BW-00-03-04).
///
/// Resolution order:
/// 1. `BOUNDLESS_TEST_BASE` -- the explicit separation for two environments
///    that deliberately share one checkout (or one build tree): each side
///    points at its own scratch root and they cannot overlap;
/// 2. this checkout's own `target/`. The workspace `target/` is gitignored
///    and belongs to this checkout alone, so two checkouts on one machine
///    are disjoint **by construction** -- the "same machine, two
///    environments, no overwrite" acceptance needs no environment variable
///    in the common case.
pub fn base_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BOUNDLESS_TEST_BASE")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // `<workspace>/crates/test-support` -> `<workspace>/target`.
    manifest.join("../../target").join(BASE_DIRECTORY)
}

/// A throwaway directory tree, removed on drop unless kept on failure.
pub struct TempWorkspace {
    root: Option<PathBuf>,
    /// Failures leave the directory in place for inspection.
    keep: bool,
}

impl TempWorkspace {
    /// Create a unique directory under the per-environment fixture base.
    pub fn new() -> Self {
        Self::with_label("test")
    }

    /// Create a unique directory named after `label`, so an archived failure
    /// directory is readable (`label`-`pid`-`sequence`).
    pub fn with_label(label: &str) -> Self {
        Self::with_base(&base_dir(), label)
    }

    /// Create a unique directory under an explicit `base`, so a test can pin
    /// its environment's scratch root instead of the process default.
    pub fn with_base(base: &Path, label: &str) -> Self {
        std::fs::create_dir_all(base).expect("fixture base directory");

        let mut counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut root = base.join(qualified_name(label, counter));
        while root.exists() {
            // The counter makes a collision unreachable in practice; the
            // `create_dir` below is the safety net.
            counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            root = base.join(qualified_name(label, counter));
        }
        std::fs::create_dir(&root).expect("test workspace");

        Self {
            root: Some(root),
            keep: false,
        }
    }

    /// Path to the workspace root.
    pub fn root(&self) -> &Path {
        self.root.as_deref().expect("workspace still owned")
    }

    /// Create `relative` and write `bytes` into it.
    pub fn write_file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent directories");
        }
        std::fs::write(&path, bytes).expect("write fixture file");
        path
    }

    /// Create `relative` with text content.
    pub fn write_text(&self, relative: &str, text: &str) -> PathBuf {
        self.write_file(relative, text.as_bytes())
    }

    /// True when `relative` exists.
    pub fn exists(&self, relative: &str) -> bool {
        self.root().join(relative).exists()
    }

    /// Read a fixture file back.
    pub fn read(&self, relative: &str) -> Vec<u8> {
        std::fs::read(self.root().join(relative)).expect("read fixture file")
    }

    /// Keep the directory on drop so a failure can be inspected.
    pub fn keep(&mut self) {
        self.keep = true;
    }

    /// Detach the root from the fixture without removing it.
    pub fn take_root(&mut self) -> Option<PathBuf> {
        self.root.take()
    }
}

impl Default for TempWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let Some(root) = self.root.take() else {
            return;
        };
        if self.keep {
            return;
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A resource root fixture: an isolated directory laid out like the
/// reference `--res` tree.
pub struct ResourceRoot {
    workspace: TempWorkspace,
}

impl Default for ResourceRoot {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceRoot {
    /// Create an empty resource root.
    pub fn new() -> Self {
        Self {
            workspace: TempWorkspace::new(),
        }
    }

    /// The absolute path to pass as a resource root, mirroring how the
    /// reference builds `<root>/game/res/bigworld`.
    pub fn path(&self) -> PathBuf {
        self.workspace.root().to_path_buf()
    }

    /// Drop a resource file at `relative` inside the root.
    pub fn add(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        self.workspace.write_file(relative, bytes)
    }

    /// Drop text at `relative`.
    pub fn add_text(&self, relative: &str, text: &str) -> PathBuf {
        self.workspace.write_text(relative, text)
    }

    /// True when `relative` exists.
    pub fn exists(&self, relative: &str) -> bool {
        self.workspace.exists(relative)
    }

    /// Keep the root on drop for failure inspection.
    pub fn keep(&mut self) {
        self.workspace.keep();
    }
}

/// Directory name of one fixture: sanitised label, pid and sequence.
fn qualified_name(label: &str, counter: u64) -> String {
    format!("{}-{}-{counter}", sanitize(label), std::process::id())
}

/// Keep only path-safe characters so the label is safe on every platform.
fn sanitize(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "test".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_are_isolated() {
        let one = TempWorkspace::new();
        let two = TempWorkspace::new();
        assert_ne!(one.root(), two.root());

        one.write_text("a/b/c.txt", "one");
        assert!(one.exists("a/b/c.txt"));
        assert!(!two.exists("a/b/c.txt"));
        assert_eq!(one.read("a/b/c.txt"), b"one");
    }

    #[test]
    fn sequential_fixtures_get_distinct_directories() {
        let made: Vec<PathBuf> = (0..4)
            .map(|_| TempWorkspace::new().root().to_path_buf())
            .collect();
        let unique: std::collections::BTreeSet<_> = made.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn dropped_workspaces_are_removed() {
        let workspace = TempWorkspace::new();
        let root = workspace.root().to_path_buf();
        assert!(root.exists());
        drop(workspace);
        assert!(!root.exists());
    }

    #[test]
    fn kept_workspace_survives_drop() {
        let mut workspace = TempWorkspace::new();
        workspace.keep();
        let root = workspace.root().to_path_buf();
        drop(workspace);
        assert!(root.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn labels_become_safe_directory_names() {
        let workspace = TempWorkspace::with_label("layering::tests::workspace_rules_hold");
        let name = workspace.root().file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("layering__tests__workspace_rules_hold-"));
        assert!(!name.contains("::"));
    }

    #[test]
    fn resource_root_holds_files() {
        let root = ResourceRoot::new();
        root.add_text("bw.xml", "<bw/>");
        assert!(root.exists("bw.xml"));
        assert!(root.path().is_absolute());
    }

    /// BW-00-03-04 acceptance: two environments on one machine, each with
    /// its own fixture base, never overwrite each other -- even when both
    /// drop a *same-named* file. The roots are disjoint and each side keeps
    /// its own content.
    #[test]
    fn two_environments_do_not_overwrite_each_other() {
        let scratch = std::env::temp_dir().join("boundless-bw03-04-acceptance");
        let _ = std::fs::remove_dir_all(&scratch);

        let env_a = scratch.join("env-a");
        let env_b = scratch.join("env-b");
        let a = TempWorkspace::with_base(&env_a, "baseapp");
        let mut b = TempWorkspace::with_base(&env_b, "baseapp");

        assert_ne!(a.root(), b.root(), "two environments are disjoint");
        assert!(a.root().starts_with(&env_a));
        assert!(b.root().starts_with(&env_b));

        // The same fixture name in both environments, different content: the
        // second write must not clobber the first.
        a.write_text("state.db", "env-a");
        b.write_text("state.db", "env-b");
        assert_eq!(a.read("state.db"), b"env-a");
        assert_eq!(b.read("state.db"), b"env-b");

        // A dropped fixture in one environment leaves the other's file alone.
        let a_path = a.root().to_path_buf();
        drop(a);
        assert!(!a_path.exists());
        assert_eq!(b.read("state.db"), b"env-b");
        b.keep(); // leave for the cleanup below
        drop(b);
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
