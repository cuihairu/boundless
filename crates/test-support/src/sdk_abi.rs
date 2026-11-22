//! BW-21-02-01: the SDK's ABI handles and their release rules.
//!
//! The ABI hands the host an opaque handle for every object it creates — a
//! session, an entity — and the host releases the handle when it is done.
//! Two release mistakes are possible, and they must be told apart: releasing
//! a handle that was already released, and releasing a number that was never
//! a handle. Each reports its own error, so a host sees which mistake it
//! made instead of a bare failure.

/// A handle number. `0` is the null handle: never issued, never valid.
pub type HandleId = u64;

/// The null handle: what the ABI hands back for "no object".
pub const NULL_HANDLE: HandleId = 0;

/// Why a release was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    /// The number was never issued as a handle.
    UnknownHandle,
    /// The handle was released once already.
    DoubleRelease,
}

impl HandleError {
    /// The error's stable code.
    pub fn code(&self) -> &'static str {
        match self {
            HandleError::UnknownHandle => "unknown-handle",
            HandleError::DoubleRelease => "double-release",
        }
    }

    /// The message a person reads.
    pub fn message(&self) -> &'static str {
        match self {
            HandleError::UnknownHandle => "that number was never a handle",
            HandleError::DoubleRelease => "that handle was already released",
        }
    }
}

/// The ABI's handle table: issues handles, releases them, and tells the two
/// release mistakes apart.
#[derive(Default)]
pub struct HandleTable {
    next: HandleId,
    live: std::collections::BTreeSet<HandleId>,
    released: std::collections::BTreeSet<HandleId>,
}

impl HandleTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh handle. The first is `1`; the null handle is never
    /// issued.
    pub fn issue(&mut self) -> HandleId {
        self.next += 1;
        let id = self.next;
        self.live.insert(id);
        id
    }

    /// Release a handle. A released handle releases again only as an error,
    /// and so does a number that was never issued.
    pub fn release(&mut self, id: HandleId) -> Result<(), HandleError> {
        if self.live.remove(&id) {
            self.released.insert(id);
            return Ok(());
        }
        if self.released.contains(&id) {
            return Err(HandleError::DoubleRelease);
        }
        Err(HandleError::UnknownHandle)
    }

    /// Whether the handle is live — issued and not yet released.
    pub fn is_live(&self, id: HandleId) -> bool {
        self.live.contains(&id)
    }

    /// How many handles are live.
    pub fn live_count(&self) -> usize {
        self.live.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_issues_live_and_releases_once() {
        let mut table = HandleTable::new();
        let id = table.issue();

        // Issued live, released once: the release succeeds and the handle
        // is no longer live.
        assert!(table.is_live(id));
        assert_eq!(table.release(id), Ok(()));
        assert!(!table.is_live(id));
    }

    #[test]
    fn a_double_release_is_recognizable() {
        let mut table = HandleTable::new();
        let id = table.issue();
        assert_eq!(table.release(id), Ok(()));

        // The second release names itself.
        let error = table.release(id).unwrap_err();
        assert_eq!(error, HandleError::DoubleRelease);
        assert_eq!(error.code(), "double-release");
        assert_eq!(error.message(), "that handle was already released");
    }

    #[test]
    fn an_unknown_number_is_a_different_error_from_a_double_release() {
        let mut table = HandleTable::new();
        let id = table.issue();
        table.release(id).unwrap();

        // A number that was never issued is not a double release.
        let unknown = table.release(99).unwrap_err();
        assert_eq!(unknown, HandleError::UnknownHandle);
        assert_eq!(unknown.code(), "unknown-handle");
        assert_ne!(unknown, HandleError::DoubleRelease);
    }

    #[test]
    fn the_null_handle_is_never_issued_or_valid() {
        let mut table = HandleTable::new();

        // Nothing was issued, so releasing the null handle is unknown, and
        // the first issued handle is 1, not 0.
        assert_eq!(table.release(NULL_HANDLE), Err(HandleError::UnknownHandle));
        assert_eq!(table.issue(), 1);
        assert!(!table.is_live(NULL_HANDLE));
    }

    #[test]
    fn handles_are_numbered_in_order() {
        let mut table = HandleTable::new();
        assert_eq!(table.issue(), 1);
        assert_eq!(table.issue(), 2);
        assert_eq!(table.issue(), 3);
    }

    #[test]
    fn the_live_count_tracks_issued_minus_released() {
        let mut table = HandleTable::new();
        let first = table.issue();
        table.issue();
        let third = table.issue();
        assert_eq!(table.live_count(), 3);

        table.release(first).unwrap();
        table.release(third).unwrap();
        assert_eq!(table.live_count(), 1);
    }
}
