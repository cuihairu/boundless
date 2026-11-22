//! BW-00-03-05 minimal process lifecycle: validate the requested role and
//! bring it up, keeping `main` free of role logic.
//!
//! Full init chains (bw.xml, network interfaces) arrive with BW-01-04;
//! signal handling is wired here at the minimal level (BW-15-02-01): the
//! process blocks the quit signals and shuts down when one arrives. This
//! module fixes the role set, the startup contract and the shutdown
//! contract.

#[cfg(unix)]
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

/// The runners lane's budget (`ci/runners.sh` starts each role with
/// `CI_RUNNERS_TIMEOUT`): when it is set the process shuts down on it
/// instead of waiting for an operator signal, so the lane records a role
/// that both started and stopped instead of holding the runner until its
/// job timeout.
#[cfg(unix)]
fn ci_runners_budget() -> Option<std::time::Duration> {
    let budget = std::env::var("CI_RUNNERS_TIMEOUT").ok()?;
    parse_budget(&budget)
}

/// Parse the lane's budget in seconds; anything that is not a whole
/// number of seconds is ignored, so a server never shuts itself down on a
/// typo in the variable.
///
/// The budget only exists where the POSIX watchdog does, so both the
/// parser and its callers are unix-scoped; a non-unix entry keeps the
/// startup smoke only.
#[cfg(unix)]
fn parse_budget(text: &str) -> Option<std::time::Duration> {
    text.trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
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
///
/// Signal handling is the POSIX spelling, so a non-unix entry keeps the
/// startup smoke only; the Windows stop path (console control events)
/// arrives with BW-15-03.
pub fn run(role: &str) -> Result<(), AppError> {
    validate_role(role)?;
    eprintln!("[{role}] starting (boundless minimal entry)");

    #[cfg(unix)]
    {
        let mut processor = SignalProcessor::new();
        let mut set = Set::empty();
        for sig in [
            platform::signals::SIGTERM,
            platform::signals::SIGINT,
            platform::signals::SIGQUIT,
        ] {
            set.set(sig);
        }
        let _blocker = Blocker::new(&set);
        eprintln!("[{role}] running");

        // The lane that starts the server with a budget targets the main
        // thread: it holds the quit set blocked, so the signal becomes
        // pending there and `wait_for_signals` consumes it, which is the
        // same clean stop an operator signal takes.
        if let Some(budget) = ci_runners_budget() {
            let main = platform::signals::thread_self();
            let _ = std::thread::Builder::new()
                .name("runners-timeout".into())
                .spawn(move || {
                    std::thread::sleep(budget);
                    let _ = platform::signals::thread_kill(main, platform::signals::SIGTERM);
                });
        }

        let sig = processor.wait_for_signals(&set);
        if sig < 0 {
            // `sigwaitinfo` failed for a reason other than `EINTR`
            // (which it retries); nothing was delivered, so end the run
            // rather than spin. `wait_for_signals` already dispatched a
            // signal when one arrived, printing
            // `SignalProcessor::dispatchSignal: <name>`.
            eprintln!("[{role}] signal wait ended without a signal");
        }
        eprintln!("[{role}] has shut down.");
    }

    #[cfg(not(unix))]
    {
        // The role is already validated; nothing to do beyond the
        // startup line until Windows console-event handling lands.
        eprintln!("[{role}] has shut down.");
    }

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

    #[cfg(unix)]
    #[test]
    fn budget_parses_a_whole_number_of_seconds() {
        use std::time::Duration;
        assert_eq!(parse_budget("1"), Some(Duration::from_secs(1)));
        assert_eq!(parse_budget(" 20 "), Some(Duration::from_secs(20)));
        assert_eq!(parse_budget("0"), Some(Duration::from_secs(0)));
        // A malformed budget is ignored rather than read as a tiny wait,
        // so a typo never makes the server stop itself mid-startup.
        assert_eq!(parse_budget(""), None);
        assert_eq!(parse_budget("2.5"), None);
        assert_eq!(parse_budget("soon"), None);
        assert_eq!(parse_budget("-3"), None);
    }
}
