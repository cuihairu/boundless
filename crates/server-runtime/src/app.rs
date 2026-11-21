//! BW-00-03-05 minimal process lifecycle: validate the requested role and
//! bring it up, keeping `main` free of role logic.
//!
//! Full init chains (bw.xml, network interfaces) arrive with BW-01-04;
//! signal handling is wired here at the minimal level (BW-15-02-01): the
//! process blocks the quit signals and shuts down when one arrives. This
//! module fixes the role set, the startup contract and the shutdown
//! contract.

use crate::signals::{Blocker, Set, SignalProcessor};

/// Server processes of this BigWorld revision (`server/*/` directories),
/// in canonical order.
pub const KNOWN_ROLES: &[&str] = &[
    "baseapp",
    "baseappmgr",
    "cellapp",
    "cellappmgr",
    "dbapp",
    "dbappmgr",
    "loginapp",
    "reviver",
];

/// Why `run` refused to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// Role not in `KNOWN_ROLES`.
    UnknownRole(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::UnknownRole(role) => write!(
                f,
                "unknown server role '{role}' (expected one of: {})",
                KNOWN_ROLES.join(", ")
            ),
        }
    }
}

impl std::error::Error for AppError {}

/// Validates `role` against [`KNOWN_ROLES`]. Pure, so the role tests
/// exercise it without blocking on the signal wait.
fn validate_role(role: &str) -> Result<(), AppError> {
    if KNOWN_ROLES.contains(&role) {
        Ok(())
    } else {
        Err(AppError::UnknownRole(role.to_string()))
    }
}

/// Runs the server process for `role`: validate, announce startup, block
/// the quit signals, and shut down cleanly when one arrives.
///
/// The reference starts each process with a startup log line naming the
/// app, and its `server_app.cpp` prints `"%s has shut down."` from
/// `fini()`; the minimal entry keeps both observable contracts. The
/// wait is the reference's `SignalProcessor::waitForSignals`
/// (`TableSynchroniser::run`) path: the quit signals are held
/// (`pthread_sigmask`) so `sigwaitinfo` can retrieve them, and the first
/// arrival is the shutdown -- the minimal server has no tick loop yet.
pub fn run(role: &str) -> Result<(), AppError> {
    validate_role(role)?;
    eprintln!("[{role}] starting (boundless minimal entry)");

    let mut processor = SignalProcessor::new();
    let mut set = Set::empty();
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGQUIT] {
        set.set(sig);
    }
    let _blocker = Blocker::new(&set);
    eprintln!("[{role}] running");

    let sig = processor.wait_for_signals(&set);
    if sig < 0 {
        // `sigwaitinfo` failed for a reason other than `EINTR` (which
        // it retries); nothing was delivered, so end the run rather
        // than spin. `wait_for_signals` already dispatched a signal
        // when one arrived, printing
        // `SignalProcessor::dispatchSignal: <name>`.
        eprintln!("[{role}] signal wait ended without a signal");
    }
    eprintln!("[{role}] has shut down.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reference_role_is_accepted() {
        // `validate_role` only: `run` blocks on the quit signals, so the
        // role check is tested without starting a process.
        for role in KNOWN_ROLES {
            assert_eq!(validate_role(role), Ok(()), "role {role} must start");
        }
    }

    #[test]
    fn unknown_role_is_rejected_with_directions() {
        let err = validate_role("cellap").unwrap_err();
        assert_eq!(err, AppError::UnknownRole("cellap".to_string()));
        let msg = err.to_string();
        assert!(msg.contains("cellap"), "echoes the bad role: {msg}");
        assert!(msg.contains("cellapp"), "suggests the correct role: {msg}");
    }

    #[test]
    fn empty_role_is_unknown() {
        assert_eq!(validate_role(""), Err(AppError::UnknownRole(String::new())));
    }

    #[test]
    fn role_list_covers_the_reference_server_dirs() {
        // The dirs under programming/bigworld/server minus build plumbing.
        let expected = [
            "baseapp",
            "baseappmgr",
            "cellapp",
            "cellappmgr",
            "dbapp",
            "dbappmgr",
            "loginapp",
            "reviver",
        ];
        assert_eq!(KNOWN_ROLES, expected);
    }
}
