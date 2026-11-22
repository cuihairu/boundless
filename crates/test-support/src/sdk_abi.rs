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

/// One event the dispatcher delivered, and the thread it ran on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackEvent {
    /// The event's name.
    pub name: String,
    /// The thread the callback ran on.
    pub thread: String,
}

/// The ABI's callback dispatcher: workers queue events from any thread, and
/// draining delivers every one of them on the one callback thread the host
/// registered — never on the worker that posted it.
#[derive(Debug)]
pub struct CallbackDispatcher {
    thread: String,
    queue: Vec<String>,
    delivered: Vec<CallbackEvent>,
}

impl CallbackDispatcher {
    /// A dispatcher that delivers on `thread`.
    pub fn new(thread: &str) -> Self {
        Self {
            thread: thread.to_string(),
            queue: Vec::new(),
            delivered: Vec::new(),
        }
    }

    /// The callback thread the host registered.
    pub fn thread(&self) -> &str {
        &self.thread
    }

    /// A worker queues one event, from whatever thread it runs on.
    pub fn queue(&mut self, name: &str) {
        self.queue.push(name.to_string());
    }

    /// How many events are queued but not yet delivered.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// The callback thread runs every queued event, in queue order, and
    /// each runs on the callback thread. Returns how many ran.
    pub fn drain(&mut self) -> usize {
        let queued = std::mem::take(&mut self.queue);
        let count = queued.len();
        for name in queued {
            self.delivered.push(CallbackEvent {
                name,
                thread: self.thread.clone(),
            });
        }
        count
    }

    /// The events delivered so far, in delivery order.
    pub fn delivered(&self) -> &[CallbackEvent] {
        &self.delivered
    }

    /// The fixed-case assertion: every delivered event ran on the callback
    /// thread.
    pub fn all_on_callback_thread(&self) -> bool {
        self.delivered
            .iter()
            .all(|event| event.thread == self.thread)
    }
}

/// The ABI's version: a major change breaks the contract, a minor addition
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AbiVersion {
    /// The major version.
    pub major: u32,
    /// The minor version.
    pub minor: u32,
}

impl AbiVersion {
    /// The version as `major.minor`, for messages.
    pub fn name(&self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

/// The ABI version this SDK speaks.
pub const ABI_VERSION: AbiVersion = AbiVersion { major: 1, minor: 0 };

/// Why the ABI handshake failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiError {
    /// The host and the SDK disagree on their major version.
    MajorMismatch,
    /// The host speaks a newer minor than the SDK knows.
    HostNewer,
}

impl AbiError {
    /// The error's stable code.
    pub fn code(&self) -> &'static str {
        match self {
            AbiError::MajorMismatch => "abi-major-mismatch",
            AbiError::HostNewer => "abi-host-newer",
        }
    }

    /// The message a person reads: names the host and the SDK versions, so a
    /// mismatch is diagnosable, not just reported.
    pub fn message(&self, host: AbiVersion, sdk: AbiVersion) -> String {
        match self {
            AbiError::MajorMismatch => format!(
                "the host speaks {} but the SDK is {}; ABI majors must agree",
                host.name(),
                sdk.name(),
            ),
            AbiError::HostNewer => format!(
                "the host speaks {} but the SDK is {}; the SDK does not \
                 know that minor",
                host.name(),
                sdk.name(),
            ),
        }
    }
}

/// Negotiate the ABI between the host's version and the SDK's. The same
/// major and a host not newer than the SDK agree; anything else is an error
/// that names both versions.
pub fn negotiate(host: AbiVersion, sdk: AbiVersion) -> Result<AbiVersion, AbiError> {
    if host.major != sdk.major {
        return Err(AbiError::MajorMismatch);
    }
    if host.minor > sdk.minor {
        return Err(AbiError::HostNewer);
    }
    Ok(sdk)
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

    #[test]
    fn a_fixed_case_can_assert_the_callback_thread() {
        let mut dispatcher = CallbackDispatcher::new("ui-thread");
        assert_eq!(dispatcher.thread(), "ui-thread");

        // Queue three events from workers, drain them, and every callback
        // ran on the one thread the host registered.
        dispatcher.queue("created");
        dispatcher.queue("entered");
        dispatcher.queue("destroyed");
        assert_eq!(dispatcher.drain(), 3);

        let threads: Vec<&str> = dispatcher
            .delivered()
            .iter()
            .map(|event| event.thread.as_str())
            .collect();
        assert_eq!(threads, vec!["ui-thread", "ui-thread", "ui-thread"]);
        assert!(dispatcher.all_on_callback_thread());
    }

    #[test]
    fn queued_events_are_delivered_in_order() {
        let mut dispatcher = CallbackDispatcher::new("ui-thread");
        dispatcher.queue("alpha");
        dispatcher.queue("beta");
        dispatcher.queue("gamma");
        dispatcher.drain();

        let names: Vec<&str> = dispatcher
            .delivered()
            .iter()
            .map(|event| event.name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn pending_events_are_not_dispatched_until_drained() {
        let mut dispatcher = CallbackDispatcher::new("ui-thread");
        dispatcher.queue("created");
        dispatcher.queue("entered");

        // Two queued, none delivered yet.
        assert_eq!(dispatcher.pending(), 2);
        assert!(dispatcher.delivered().is_empty());

        // Draining delivers both and empties the queue.
        assert_eq!(dispatcher.drain(), 2);
        assert_eq!(dispatcher.pending(), 0);
        assert_eq!(dispatcher.delivered().len(), 2);
    }

    #[test]
    fn events_from_different_threads_land_on_one_callback_thread() {
        let mut dispatcher = CallbackDispatcher::new("ui-thread");

        // Two workers post to the same dispatcher; both land on the one
        // callback thread, not on their own.
        dispatcher.queue("from-worker-1");
        dispatcher.queue("from-worker-2");
        dispatcher.drain();

        assert!(dispatcher.all_on_callback_thread());
        assert_eq!(dispatcher.delivered().len(), 2);
    }

    #[test]
    fn an_empty_dispatcher_delivers_nothing() {
        let mut dispatcher = CallbackDispatcher::new("ui-thread");
        assert_eq!(dispatcher.drain(), 0);
        assert!(dispatcher.delivered().is_empty());
        assert_eq!(dispatcher.pending(), 0);
        // Nothing was delivered, so the thread assertion holds vacuously.
        assert!(dispatcher.all_on_callback_thread());
    }

    #[test]
    fn an_abi_handshake_agrees_on_the_same_version() {
        let host = AbiVersion { major: 1, minor: 0 };

        // Same major, host not newer: the handshake agrees.
        assert_eq!(negotiate(host, host), Ok(host));
        assert_eq!(ABI_VERSION.name(), "1.0");
    }

    #[test]
    fn a_host_older_than_the_sdk_still_agrees() {
        let host = AbiVersion { major: 1, minor: 0 };
        let sdk = AbiVersion { major: 1, minor: 2 };

        // An older host on the same major is backward compatible.
        assert_eq!(negotiate(host, sdk), Ok(sdk));
    }

    #[test]
    fn a_major_mismatch_names_both_versions() {
        let host = AbiVersion { major: 2, minor: 0 };
        let sdk = AbiVersion { major: 1, minor: 3 };

        // The error names the two versions, so the mismatch is diagnosable.
        let error = negotiate(host, sdk).unwrap_err();
        assert_eq!(error, AbiError::MajorMismatch);
        assert_eq!(error.code(), "abi-major-mismatch");
        let message = error.message(host, sdk);
        assert!(message.contains("2.0") && message.contains("1.3"));
        assert!(message.contains("majors must agree"));
    }

    #[test]
    fn a_host_newer_minor_is_diagnosable() {
        let host = AbiVersion { major: 1, minor: 5 };
        let sdk = AbiVersion { major: 1, minor: 3 };

        let error = negotiate(host, sdk).unwrap_err();
        assert_eq!(error, AbiError::HostNewer);
        assert_eq!(error.code(), "abi-host-newer");
        // The message names both versions again.
        let message = error.message(host, sdk);
        assert!(message.contains("1.5") && message.contains("1.3"));
    }

    #[test]
    fn the_two_abi_errors_are_distinguishable() {
        let older = AbiVersion { major: 1, minor: 0 };
        let host = AbiVersion { major: 2, minor: 0 };

        let major = negotiate(host, older).unwrap_err();
        let minor = negotiate(AbiVersion { major: 1, minor: 5 }, older).unwrap_err();
        assert_ne!(major, minor);
        assert_ne!(major.code(), minor.code());
    }
}
