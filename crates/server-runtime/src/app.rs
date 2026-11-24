//! BW-00-03-05 process lifecycle: validate the requested role and run it
//! through the reference's `bwMainT` loop, keeping `main` free of role
//! logic.
//!
//! The loop is the reference's shape (`lib/server/bwservice.hpp:108-146`):
//! a `Dispatcher`, a `SignalProcessor` registered as its frequent task,
//! three signal faces -- SIGINT dies through the base re-raise
//! (`server_app.cpp:556-570`), SIGQUIT takes the immediate CRITICAL slot
//! (`entity_app.cpp:91,131-138`), and SIGUSR1 is the controlled stop
//! (`machine_guard.hpp:714` `setControlledShutdown` -> `shutDown`
//! `breakProcessing`, `server_app.cpp:542-546`) -- and then
//! `processUntilBreak` into the shutdown tail. The quit signals are not
//! blocked: the `sigwait` entry this module once carried was the minimal
//! entry's documented deviation, and the full loop replaces it.
//!
//! `bw.xml`/resource init happens in `main` (the reference's
//! `BIGWORLD_MAIN` half); the Mercury interface construction and its
//! `prepareForShutdown` body join with BW-02-02, and the per-role apps
//! (`doBWMainT`'s `SERVER_APP`) arrive with their role clusters. This
//! module fixes the role set, the startup contract and the shutdown
//! contract.

#[cfg(unix)]
use crate::server_app::{base_on_signalled, entity_app_quit_signal};
#[cfg(unix)]
use crate::signals::{SignalDispatchTask, SignalHandlerFn, SignalProcessor};

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

/// Each role's reference `appName()` -- the strings the header macros
/// pass (`SERVER_APP_HEADER( BaseApp, baseApp )` `server_app.hpp:41,45`,
/// `MANAGER_APP_HEADER( BaseAppMgr, baseAppMgr )` `baseappmgr.hpp:210`,
/// `ENTITY_APP_HEADER( CellApp, cellApp )` `cellapp.hpp:94`, and the
/// same macros in `cellappmgr.hpp:60`, `dbappmgr.hpp:50`,
/// `dbapp.hpp`, `loginapp.hpp`, `reviver.hpp`). The startup line, the
/// `"%s has shut down."` tail (`bwservice.hpp:145`) and the SIGQUIT
/// CRITICAL text all carry it.
#[cfg(unix)]
const ROLE_APP_NAMES: &[(&str, &str)] = &[
    ("baseapp", "BaseApp"),
    ("baseappmgr", "BaseAppMgr"),
    ("cellapp", "CellApp"),
    ("cellappmgr", "CellAppMgr"),
    ("dbapp", "DBApp"),
    ("dbappmgr", "DBAppMgr"),
    ("loginapp", "LoginApp"),
    ("reviver", "Reviver"),
];

/// The reference `appName()` for a validated role.
#[cfg(unix)]
fn reference_app_name(role: &str) -> &'static str {
    ROLE_APP_NAMES
        .iter()
        .find(|(name_role, _)| *name_role == role)
        .map(|(_, app_name)| *app_name)
        .expect("a validated role has a reference app name")
}

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

/// Runs the server process for `role`: validate, build the dispatcher and
/// its signal faces, run the reference's loop, and shut down on the
/// controlled stop.
///
/// This is the generic `bwMainT` body (`bwservice.hpp:108-146`) with the
/// role apps flattened away: the dispatcher, the `SignalProcessor` as its
/// frequent task (`bwservice.hpp:127`), the signal faces each init
/// chain registers (`ServerApp::init` SIGINT `server_app.cpp:240`,
/// `EntityApp::init` SIGQUIT `entity_app.cpp:91`, the controlled-stop
/// SIGUSR1 `loginapp.cpp:432` pattern), `processUntilBreak`
/// (`server_app.cpp:258`), and the shutdown tail
/// (`bwservice.hpp:145`). `runApp`'s `fini()` and
/// `interface_.prepareForShutdown()` walk is the tail contract; the
/// Mercury interface itself joins with BW-02-02.
///
/// Signal handling is the POSIX spelling, so a non-unix entry keeps the
/// startup smoke only; the Windows stop path (console control events)
/// arrives with BW-15-03.
pub fn run(role: &str) -> Result<(), AppError> {
    validate_role(role)?;
    eprintln!("[{role}] starting");

    #[cfg(unix)]
    {
        let app_name = reference_app_name(role);

        // Mercury::EventDispatcher dispatcher; (bwservice.hpp:110)
        let mut dispatcher = crate::dispatcher::Dispatcher::new();

        // SignalProcessor signalProcessor( dispatcher ); the constructor's
        // `dispatcher_.addFrequentTask( this )` (bwservice.hpp:127,
        // signal_processor.cpp:94-103) is the binding: the loop's frequent
        // pass is where pending signals surface and their handlers run.
        let processor = std::rc::Rc::new(std::cell::RefCell::new(SignalProcessor::new()));
        dispatcher.add_frequent_task(SignalDispatchTask::new_task(&processor));

        // ServerApp::init: enableSignalHandler( SIGINT ) -- the forwarded
        // signal reaches `onSignalled`, whose base arm resets the
        // disposition and re-raises, killing the process under the
        // dispatch Blocker (server_app.cpp:240, 556-570). The generic role
        // has no override, so the base arm is the whole face.
        processor.borrow_mut().add_signal_handler(
            libc::SIGINT,
            std::rc::Rc::new(std::cell::RefCell::new(SignalHandlerFn(base_on_signalled))),
        );

        // EntityApp::init: enableSignalHandler( SIGQUIT ) -- SIGQUIT takes
        // the single immediate slot and the CRITICAL text plus the abort
        // run straight from signal context (entity_app.cpp:91,
        // signal_processor.cpp:158-159, 306-308).
        processor
            .borrow_mut()
            .add_quit_handler(std::sync::Arc::new(move |_| {
                entity_app_quit_signal(app_name)
            }));

        // The controlled stop: machined's setControlledShutdown signals
        // SIGUSR1 (machine_guard.hpp:714) and `ServerApp::shutDown`
        // logs then breaks the dispatcher (server_app.cpp:542-546).
        // loginapp registers the signal the same way (loginapp.cpp:432);
        // the other roles receive the stop through their manager messages,
        // whose wiring belongs to the role clusters. The generic entry
        // takes the base arm for every role.
        let break_flag = dispatcher.break_flag();
        processor.borrow_mut().add_signal_handler(
            libc::SIGUSR1,
            std::rc::Rc::new(std::cell::RefCell::new(SignalHandlerFn(move |_| {
                eprintln!("ServerApp::shutDown: shutting down");
                break_flag.set(true);
            }))),
        );

        // INFO_MSG( "---- %s is running ----\n" ) (server_app.cpp:278)
        eprintln!("---- {app_name} is running ----");

        // The lane that starts the server with a budget targets the main
        // thread with the controlled-stop signal: the same clean stop an
        // operator's machined would command, and the stop the loop drains
        // through its frequent pass.
        if let Some(budget) = ci_runners_budget() {
            let main = platform::signals::thread_self();
            let _ = std::thread::Builder::new()
                .name("runners-timeout".into())
                .spawn(move || {
                    std::thread::sleep(budget);
                    let _ = platform::signals::thread_kill(main, libc::SIGUSR1);
                });
        }

        // doBWMainT -> runApp -> run: mainDispatcher_.processUntilBreak()
        // (server_app.cpp:255-262). SIGTERM has no face here -- like the
        // reference, the default disposition takes the process.
        dispatcher.process_until_break();

        // runApp's tail walks fini() then interface_.prepareForShutdown()
        // (server_app.cpp:286-288); the base faces are no-ops until the
        // Mercury interface joins, and then:
        // INFO_MSG( "%s has shut down.\n", appName ) (bwservice.hpp:145)
        eprintln!("{app_name} has shut down.");
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

    /// The app-name table covers exactly the known roles, in order, with
    /// the header macros' strings.
    #[cfg(unix)]
    #[test]
    fn reference_app_names_cover_the_known_roles() {
        let names: Vec<&str> = ROLE_APP_NAMES.iter().map(|(role, _)| *role).collect();
        assert_eq!(names, KNOWN_ROLES, "one app name per role, in order");
        for (role, app_name) in ROLE_APP_NAMES {
            assert_eq!(validate_role(role), Ok(()));
            assert_eq!(reference_app_name(role), *app_name);
        }
    }

    /// The loop is the acceptance: the dispatcher runs its frequent pass,
    /// the pass drains the pending SIGUSR1 into the controlled-stop arm,
    /// and the break unwinds into the shutdown tail. The signal is raised
    /// from a helper thread once the loop is up (`run`'s own setup takes
    /// microseconds; the kicker waits well past that, because an early
    /// SIGUSR1 would hit the default disposition and kill the test).
    #[cfg(unix)]
    #[test]
    fn the_controlled_signal_ends_the_dispatch_loop() {
        let _guard = crate::signals::disposition_guard();
        let kicker = std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            unsafe {
                libc::kill(libc::getpid(), libc::SIGUSR1);
            }
        });
        run("dbapp").expect("the controlled stop ends the loop");
        kicker.join().expect("the kicker joins");
    }
}
