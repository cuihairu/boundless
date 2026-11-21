//! Temporary workspaces and resource roots for tests (BW-18-01-01).
//!
//! The reference harness synthesises its resource root from the build
//! directory (`lib/unit_test_lib/base_resmgr_unit_test_harness.cpp`), which
//! couples tests to the checkout layout. Here a fixture creates an isolated
//! directory instead, so tests run in parallel without touching each other
//! or the working tree.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Directory name shared by every fixture of this test process.
pub const BASE_DIRECTORY: &str = "boundless-test";

/// Environment variable that redirects the fixture base (BW-00-03-04):
/// set it to give one environment its own scratch root, so two heavy
/// runs on one machine write through disjoint trees. Empty or unset
/// keeps the system default.
pub const TEST_TMP_ENV: &str = "BOUNDLESS_TEST_TMP";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A throwaway directory tree, removed on drop unless kept on failure.
pub struct TempWorkspace {
    root: Option<PathBuf>,
    /// Failures leave the directory in place for inspection.
    keep: bool,
}

impl TempWorkspace {
    /// Create a unique directory.
    pub fn new() -> Self {
        Self::with_label("test")
    }

    /// Create a unique directory named after `label`, so an archived failure
    /// directory is readable (`label`-`pid`-`sequence`). The base is
    /// [`test_base`], so the fixture honours `BOUNDLESS_TEST_TMP`.
    pub fn with_label(label: &str) -> Self {
        let base = test_base();
        std::fs::create_dir_all(&base).expect("fixture base directory");

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

/// Resolve the fixture base from a `BOUNDLESS_TEST_TMP` value (BW-00-03-04).
///
/// A non-empty value replaces the base wholesale; empty or `None` keeps
/// the system default under the temporary directory. Pure so the rule is
/// testable without mutating process-wide state.
pub fn base_from_env(value: Option<&str>) -> PathBuf {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => std::env::temp_dir().join(BASE_DIRECTORY),
    }
}

/// The directory every fixture of this process is created under: the
/// `BOUNDLESS_TEST_TMP` override when set, else the system temp directory.
pub fn test_base() -> PathBuf {
    base_from_env(std::env::var(TEST_TMP_ENV).ok().as_deref())
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

    #[test]
    fn the_base_env_replaces_the_default_wholesale() {
        assert_eq!(
            base_from_env(Some("/scratch/boundless")),
            PathBuf::from("/scratch/boundless")
        );
        // A stray amount of whitespace is still a real override.
        assert_eq!(
            base_from_env(Some("  /scratch/boundless  ")),
            PathBuf::from("/scratch/boundless")
        );
        // Whitespace only is not a path, so it falls back to the default.
        let default = std::env::temp_dir().join(BASE_DIRECTORY);
        assert_eq!(base_from_env(Some("   ")), default);
        assert_eq!(base_from_env(None), default);
    }
}
