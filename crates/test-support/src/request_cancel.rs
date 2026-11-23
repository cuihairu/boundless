//! BW-27-01-08: cancelling a long language-tool request.
//!
//! A full rescan walks every `.def` file in the workspace and can outlast the
//! editor's patience: the client gives up, and the request must stop instead of
//! running on. The work is checked before every file — cancelling between two
//! files keeps the first file's result and drops the rest — and a flag shared
//! by clone is the whole protocol.

use crate::entitydef_index::EntityDefIndex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A cancellation flag, shared by every clone: the request's owner raises it,
/// the worker watches it.
#[derive(Clone, Debug, Default)]
pub struct Cancel {
    raised: Arc<AtomicBool>,
}

impl Cancel {
    /// A flag nobody has raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise the flag; work in flight stops at its next checkpoint.
    pub fn cancel(&self) {
        self.raised.store(true, Ordering::Relaxed);
    }

    /// Whether the flag has been raised.
    pub fn is_cancelled(&self) -> bool {
        self.raised.load(Ordering::Relaxed)
    }
}

/// How far a rescan got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// How many files were indexed before the rescan stopped.
    pub indexed: usize,
    /// Whether every file was indexed — false only when cancelled.
    pub completed: bool,
}

/// Re-index a whole workspace's `.def` files, checking for cancellation
/// before each one. A cancelled rescan keeps what it already indexed.
pub fn rescan(index: &mut EntityDefIndex, files: &[(&str, &str)], cancel: &Cancel) -> Outcome {
    let mut indexed = 0;
    for (path, content) in files {
        if cancel.is_cancelled() {
            return Outcome {
                indexed,
                completed: false,
            };
        }
        index.refresh(path, content);
        indexed += 1;
    }
    Outcome {
        indexed,
        completed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVATAR: &str = "<root>\n\t<Properties>\n\t\t<hp>\n\t\t\t<Type>\tUINT32\t</Type>\n\t\t</hp>\n\t</Properties>\n</root>\n";

    fn files() -> Vec<(&'static str, &'static str)> {
        vec![
            ("res/scripts/entity_defs/Avatar.def", AVATAR),
            ("res/scripts/entity_defs/Npc.def", AVATAR),
            ("res/scripts/entity_defs/Player.def", AVATAR),
        ]
    }

    #[test]
    fn an_uncancelled_rescan_indexes_every_file() {
        let mut index = EntityDefIndex::new();
        let outcome = rescan(&mut index, &files(), &Cancel::new());

        assert_eq!(
            outcome,
            Outcome {
                indexed: 3,
                completed: true
            }
        );
        assert_eq!(index.count(), 3);
    }

    #[test]
    fn a_raised_flag_stops_the_rescan_before_the_first_file() {
        let mut index = EntityDefIndex::new();
        let cancel = Cancel::new();
        cancel.cancel();

        let outcome = rescan(&mut index, &files(), &cancel);
        assert_eq!(
            outcome,
            Outcome {
                indexed: 0,
                completed: false
            }
        );
        assert_eq!(index.count(), 0);
    }

    #[test]
    fn a_rescan_stops_between_files_and_keeps_what_it_indexed() {
        let mut index = EntityDefIndex::new();
        let files = files();
        let cancel = Cancel::new();

        // The first chunk completes; the client gives up before the rest.
        let first = rescan(&mut index, &files[..2], &cancel);
        assert!(first.completed);
        cancel.cancel();
        let second = rescan(&mut index, &files[2..], &cancel);

        assert_eq!(
            second,
            Outcome {
                indexed: 0,
                completed: false
            }
        );
        // What the first chunk indexed stays: a cancelled rescan keeps its
        // partial results rather than undoing them.
        assert_eq!(index.count(), 2);
        assert_eq!(index.entities(), vec!["Avatar", "Npc"]);
    }

    #[test]
    fn clones_share_one_flag() {
        // The owner holds one clone, the worker another: raising either is
        // seen by both, and a fresh flag is unraised.
        let owner = Cancel::new();
        let worker = owner.clone();
        assert!(!worker.is_cancelled());
        owner.cancel();
        assert!(worker.is_cancelled());
        assert!(!Cancel::new().is_cancelled());
    }
}
