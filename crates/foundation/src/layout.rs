//! BW-00-03-04 the directory layout one `boundless` environment owns.
//!
//! An environment keeps four *disjoint* directory trees under one root: the
//! resource tree, the configuration tree, the reference (original BigWorld)
//! artifacts, and the test fixtures. The four trees are direct children of
//! the root with distinct names, so within one environment no tree can ever
//! be a prefix of another — and two environments rooted at different,
//! non-nested bases can never overwrite each other on the same machine. That
//! last property is the acceptance: two environments coexist without one
//! clobbering the other's resources, config, reference artifacts or fixtures.
//!
//! The reference keeps everything under `BW_ROOT/game` (`BW_INSTALL_DIR`):
//! `game/bin` for binaries and `game/res` for the resource tree, with the
//! `server/bw.xml` configuration chain searched through the resource path.
//! The port splits those concerns into named trees so an environment can be
//! laid out, wiped and re-created without touching a neighbouring one.

use std::path::{Path, PathBuf};

/// The four directory trees one environment owns, in a stable order.
pub const TREE_NAMES: [&str; 4] = ["res", "config", "reference", "test-fixtures"];

/// A `boundless` environment root and the four disjoint trees under it.
///
/// Constructing a [`Layout`] only computes paths; nothing is created on disk.
/// The root is stored as given (absolute or relative), and every tree is a
/// direct child of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// The environment rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The environment root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The resource tree (`<root>/res`).
    pub fn resources(&self) -> PathBuf {
        self.root.join("res")
    }

    /// The configuration tree (`<root>/config`).
    pub fn config(&self) -> PathBuf {
        self.root.join("config")
    }

    /// The reference (original BigWorld) artifacts (`<root>/reference`).
    pub fn reference_artifacts(&self) -> PathBuf {
        self.root.join("reference")
    }

    /// The test fixtures (`<root>/test-fixtures`).
    pub fn test_fixtures(&self) -> PathBuf {
        self.root.join("test-fixtures")
    }

    /// All four trees in [`TREE_NAMES`] order.
    pub fn trees(&self) -> [PathBuf; 4] {
        [
            self.resources(),
            self.config(),
            self.reference_artifacts(),
            self.test_fixtures(),
        ]
    }

    /// Whether any tree of this environment is a prefix of, or a prefix of
    /// any tree of, `other`.
    ///
    /// Two environments overlap only when one is rooted inside a tree the
    /// other owns (or at the same root); sibling roots never do. This lets a
    /// caller assert the acceptance before two environments are used side by
    /// side on one machine.
    pub fn overlaps(&self, other: &Layout) -> bool {
        self.trees().iter().any(|a| {
            other
                .trees()
                .iter()
                .any(|b| a.starts_with(b) || b.starts_with(a))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_trees_are_pairwise_disjoint() {
        // The acceptance within one environment: no tree is a prefix of
        // another, so writing resources can never reach the config, the
        // reference artifacts or the test fixtures.
        let layout = Layout::new("/srv/boundless");
        let trees = layout.trees();
        for (i, a) in trees.iter().enumerate() {
            for b in trees.iter().skip(i + 1) {
                assert!(
                    !a.starts_with(b) && !b.starts_with(a),
                    "{a:?} and {b:?} overlap"
                );
            }
        }
    }

    #[test]
    fn the_trees_are_direct_children_of_the_root() {
        let layout = Layout::new("/srv/boundless");
        assert_eq!(layout.resources(), PathBuf::from("/srv/boundless/res"));
        assert_eq!(layout.config(), PathBuf::from("/srv/boundless/config"));
        assert_eq!(
            layout.reference_artifacts(),
            PathBuf::from("/srv/boundless/reference")
        );
        assert_eq!(
            layout.test_fixtures(),
            PathBuf::from("/srv/boundless/test-fixtures")
        );
        assert_eq!(layout.root(), Path::new("/srv/boundless"));
    }

    #[test]
    fn the_tree_names_are_distinct() {
        let mut names = TREE_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TREE_NAMES.len());
    }

    #[test]
    fn two_sibling_environments_do_not_overlap() {
        // The acceptance itself: two environments on one machine, rooted at
        // sibling directories, never collide.
        let a = Layout::new("/srv/boundless-a");
        let b = Layout::new("/srv/boundless-b");
        assert!(!a.overlaps(&b));
        assert!(!b.overlaps(&a));
    }

    #[test]
    fn an_environment_rooted_inside_a_tree_overlaps() {
        // Rooting one environment inside another's resource tree is exactly
        // the collision the layout must report, so a caller can refuse it.
        let outer = Layout::new("/srv/boundless");
        let inside = Layout::new("/srv/boundless/res");
        assert!(outer.overlaps(&inside));
        assert!(inside.overlaps(&outer));
    }

    #[test]
    fn the_same_root_overlaps() {
        let a = Layout::new("/srv/boundless");
        let b = Layout::new("/srv/boundless");
        assert!(a.overlaps(&b));
    }
}
