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
//! `BIGWORLD_MAIN` half) and hands the config in here: the loop then
//! builds the role's Mercury interface over the loop's own dispatcher --
//! the machined query for the internal interface (`bwservice.hpp:113-119`,
//! `machined_utils.cpp:477-505`), the `internalInterface` setting chain
//! (`bwconfig.cpp:455-462`), the interface construction
//! (`bwservice.hpp:123-125`) and the shutdown drain
//! (`network_interface.cpp:178-185`). The per-role apps (`doBWMainT`'s
//! `SERVER_APP`) arrive with their role clusters. This module fixes the
//! role set, the startup contract and the shutdown contract.

use crate::config::BwConfig;
#[cfg(unix)]
use crate::machined::{MachinedClient, PORT_MACHINED, loopback_ip};
#[cfg(unix)]
use crate::server_app::{base_on_signalled, entity_app_quit_signal};
#[cfg(unix)]
use crate::signals::{SignalDispatchTask, SignalHandlerFn, SignalProcessor};
#[cfg(unix)]
use contracts::machined as mg;

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

/// Each role's reference `appName()` and `configPath()` -- the strings
/// the header macros pass (`SERVER_APP_HEADER( LoginApp, loginApp )`
/// `server_app.hpp:41-42`; `SERVER_APP_HEADER_CUSTOM_NAME( BaseApp,
/// baseApp )` `baseapp.hpp:79`; `ENTITY_APP_HEADER` is
/// `SERVER_APP_HEADER` `entity_app.hpp:25`, so `cellapp.hpp:94` gives
/// `cellApp`; `MANAGER_APP_HEADER` likewise `manager_app.hpp:16`, so
/// `baseappmgr.hpp:210`, `cellappmgr.hpp:60`, `dbappmgr.hpp:50` give the
/// `*AppMgr` strings; `dbapp.hpp:82`, `loginapp.hpp:62`, `reviver.hpp:42`
/// give the rest). The startup line, the `"%s has shut down."` tail
/// (`bwservice.hpp:145`) and the SIGQUIT CRITICAL text carry the app
/// name; the `internalInterface` option is read under the config path
/// (`bwservice.hpp:120-121`).
#[cfg(unix)]
const ROLE_APP_NAMES: &[(&str, &str, &str)] = &[
    ("baseapp", "BaseApp", "baseApp"),
    ("baseappmgr", "BaseAppMgr", "baseAppMgr"),
    ("cellapp", "CellApp", "cellApp"),
    ("cellappmgr", "CellAppMgr", "cellAppMgr"),
    ("dbapp", "DBApp", "dbApp"),
    ("dbappmgr", "DBAppMgr", "dbAppMgr"),
    ("loginapp", "LoginApp", "loginApp"),
    ("reviver", "Reviver", "reviver"),
];

/// The reference `appName()` for a validated role.
#[cfg(unix)]
fn reference_app_name(role: &str) -> &'static str {
    role_strings(role)
        .map(|(_, app_name, _)| app_name)
        .expect("a validated role has a reference app name")
}

/// The reference `configPath()` for a validated role -- the
/// `SERVER_APP_HEADER` macro's `CONFIG_PATH` string
/// (`server_app.hpp:42`), the root the app's `bw.xml` options live
/// under.
#[cfg(unix)]
fn reference_config_path(role: &str) -> &'static str {
    role_strings(role)
        .map(|(_, _, config_path)| config_path)
        .expect("a validated role has a reference config path")
}

/// The `(role, appName, configPath)` row for a validated role.
#[cfg(unix)]
fn role_strings(role: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    ROLE_APP_NAMES
        .iter()
        .find(|(name_role, _, _)| *name_role == role)
}

/// `Mercury::NetworkInterface::USE_BWMACHINED` (`network_interface.cpp:68`,
/// the string `"bwmachined"`): the default `internalInterface` setting,
/// meaning "the same interface as bwmachined".
#[cfg(unix)]
const USE_BWMACHINED: &str = "bwmachined";

/// `ServerApp::discoveredInternalIP` (`server_app.hpp:97`,
/// `server_app.cpp:90`): the loopback machined query's answer, 0 until a
/// daemon names one. Its consumers arrive with the role clusters
/// (`python_server.cpp:560` reads it).
#[cfg(unix)]
pub static DISCOVERED_INTERNAL_IP: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// Why `run` refused to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// Role not in `KNOWN_ROLES`.
    UnknownRole(String),
    /// loginapp's external interface failed its `init` bind checks
    /// (`loginapp_interface::check`); the verbatim reference ERROR line
    /// is logged where the arm fires.
    LoginExternalBind(crate::loginapp_interface::BindError),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::UnknownRole(role) => write!(
                f,
                "unknown server role '{role}' (expected one of: {})",
                KNOWN_ROLES.join(", ")
            ),
            AppError::LoginExternalBind(err) => {
                write!(f, "loginapp: {err}")
            }
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

/// `Mercury::MachineDaemon::queryForInternalInterface` minus its fixed
/// destination (`machined_utils.cpp:477-505`): one `QueryInterface`
/// round trip to machined; `Some(address)` when the daemon answered,
/// `None` otherwise. A send/recv/decode failure takes the reference's
/// failure arm (`machined_utils.cpp:497-500`); a reply that is not a
/// `QueryInterface` takes its silent no-response arm (`:502-504`).
#[cfg(unix)]
fn query_machined_for_internal_interface(
    client: &MachinedClient,
    ip: u32,
    port: u16,
) -> Option<u32> {
    let message = mg::MachineGuardMessage::QueryInterface(mg::QueryInterfaceMessage::default());
    match client.query(ip, port, &message) {
        Ok(mg::MachineGuardMessage::QueryInterface(m)) => Some(m.address),
        Ok(_) => None,
        Err(_) => {
            eprintln!(
                "MachineDaemon::queryForInternalInterface: \
                 Failed to send interface discovery message to bwmachined."
            );
            None
        }
    }
}

/// `queryForInternalInterface` with its destination restored
/// (`machined_utils.cpp:477-505`): bwmachined on the loopback address,
/// the machine-local daemon. A machined-less host pays the query's
/// receive timeout once and gets `None` -- the reference's
/// `sendAndRecv` timer expiry is the port's `MachinedClient` receive
/// timeout (`machined.rs`, two seconds).
#[cfg(unix)]
fn query_for_internal_interface() -> Option<u32> {
    let client = MachinedClient::new().ok()?;
    query_machined_for_internal_interface(&client, loopback_ip(), PORT_MACHINED)
}

/// `getBWInternalInterfaceSetting` (`bwconfig.cpp:455-462`): the role's
/// `<configPath>/internalInterface` option, falling back to the global
/// `internalInterface`, falling back to [`USE_BWMACHINED`]. The
/// reference reads it from the config tree `BIGWORLD_MAIN` initialised;
/// `run`'s caller hands that tree in.
#[cfg(unix)]
fn internal_interface_setting(config_path: &str, config: &BwConfig) -> String {
    config.get_string(
        &format!("{config_path}/internalInterface"),
        &config.get_string("internalInterface", USE_BWMACHINED),
    )
}

/// The role loop's network pass over the shared Mercury dispatcher. The
/// reference ran one dispatcher whose pass (`processForTimeout`) walked
/// frequent tasks, timers and the poller together; the port has the
/// role loop's [`crate::dispatcher::Dispatcher`] (signals, break flag)
/// and Mercury's `EventDispatcher` (interface timers, rate limiter,
/// channels), and this pass is the join: each loop iteration drains the
/// Mercury dispatcher's own faces before polling its sockets for up to
/// the wait the role dispatcher computed. Linux only, like the
/// `EventPoller` pass in `dispatcher.rs` (the poller's port gate).
#[cfg(target_os = "linux")]
struct MercuryPass {
    dispatcher: std::rc::Rc<mercury::event_dispatcher::EventDispatcher>,
}

#[cfg(target_os = "linux")]
impl MercuryPass {
    fn new(dispatcher: std::rc::Rc<mercury::event_dispatcher::EventDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[cfg(target_os = "linux")]
impl crate::dispatcher::NetworkPass for MercuryPass {
    fn process_pending_events(&mut self, max_wait: f64) -> i32 {
        // `processForTimeout`'s task and timer legs ahead of the poll
        // (event_dispatcher.cpp:624-652): the interface's own frequent
        // work (channel bookkeeping) and timers (rate limiter period)
        // run on the same cadence the reference's single loop gave them.
        self.dispatcher.process_frequent_tasks();
        self.dispatcher.process_timers();

        // The poll leg, up to the role dispatcher's computed wait.
        self.dispatcher.poller().process_pending_events(max_wait)
    }
}

/// Runs the server process for `role`: validate, build the dispatcher and
/// its signal faces, construct the role's Mercury interface over the
/// loop's own dispatcher, run the reference's loop, and shut down on the
/// controlled stop.
///
/// This is the generic `bwMainT` body (`bwservice.hpp:111-146`) with the
/// role apps flattened away: the dispatcher, the machined query for the
/// internal interface (`bwservice.hpp:113-119`), the
/// `internalInterface` setting chain (`bwservice.hpp:120-121`,
/// `bwconfig.cpp:455-462`), the interface construction
/// (`bwservice.hpp:123-125`), the `SignalProcessor` as the loop's
/// frequent task (`bwservice.hpp:127`), the signal faces each init
/// chain registers (`ServerApp::init` SIGINT `server_app.cpp:240`,
/// `EntityApp::init` SIGQUIT `entity_app.cpp:91`, the controlled-stop
/// SIGUSR1 `loginapp.cpp:432` pattern), the deprecation warning on a
/// configured interface (`bwservice.hpp:134-140`), `processUntilBreak`
/// (`server_app.cpp:255-262`), and the shutdown tail: `fini()` then
/// `interface_.prepareForShutdown()` (`server_app.cpp:286-288`,
/// `network_interface.cpp:178-185`), then the `"%s has shut down."`
/// line (`bwservice.hpp:145`).
///
/// The reference reads the `internalInterface` chain from the config
/// tree `BIGWORLD_MAIN` initialised; the caller hands that tree in as
/// `config`.
///
/// Signal handling is the POSIX spelling, so a non-unix entry keeps the
/// startup smoke only; the Windows stop path (console control events)
/// arrives with BW-15-03.
pub fn run(role: &str, config: &BwConfig) -> Result<(), AppError> {
    validate_role(role)?;
    eprintln!("[{role}] starting");

    #[cfg(unix)]
    {
        let app_name = reference_app_name(role);

        // Mercury::EventDispatcher dispatcher; (bwservice.hpp:111) --
        // one dispatcher in the reference; the port's role loop and
        // Mercury layers join in the network pass below.
        let mut dispatcher = crate::dispatcher::Dispatcher::new();
        let mercury_dispatcher =
            std::rc::Rc::new(mercury::event_dispatcher::EventDispatcher::new());

        // Find the internal interface IP by querying BWMachined
        // (bwservice.hpp:113-119): the answer lands in
        // `ServerApp::discoveredInternalIP`; a machined-less host pays
        // the query's timeout once and takes the WARNING.
        match query_for_internal_interface() {
            Some(ip) => DISCOVERED_INTERNAL_IP.store(ip, std::sync::atomic::Ordering::Relaxed),
            None => {
                eprintln!("bwMainT: Unable to determine internal interface via BWMachineD query.")
            }
        }

        // getBWInternalInterfaceSetting( SERVER_APP::configPath() )
        // (bwservice.hpp:120-121; bwconfig.cpp:455-462)
        let interface_name = internal_interface_setting(reference_config_path(role), config);

        // Mercury::NetworkInterface interface( &dispatcher,
        // NETWORK_INTERFACE_INTERNAL, 0, internalInterfaceName ) over
        // the loop's own dispatcher (bwservice.hpp:123-125). Port 0 is
        // the reference's argument here; each app binds its own faces in
        // its init. The interface joins the role apps with their
        // clusters (doBWMainT's `SERVER_APP` constructor takes it by
        // reference).
        let (_interface, _receiver) =
            mercury::network_interface::NetworkInterface::new_on_dispatcher(
                mercury::network_interface::InterfaceType::Internal,
                std::rc::Rc::clone(&mercury_dispatcher),
                0,
                &interface_name,
            );

        // The loop's network pass: the interface rides the same
        // dispatcher, and the pass joins the two dispatcher layers
        // (MercuryPass). Linux only, like the poller's port gate.
        #[cfg(target_os = "linux")]
        dispatcher.set_network_pass(Box::new(MercuryPass::new(std::rc::Rc::clone(
            &mercury_dispatcher,
        ))));

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
            platform::signals::SIGINT,
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
            platform::signals::SIGUSR1,
            std::rc::Rc::new(std::cell::RefCell::new(SignalHandlerFn(move |_| {
                eprintln!("ServerApp::shutDown: shutting down");
                break_flag.set(true);
            }))),
        );

        // CONFIG_WARNING_MSG on a configured interface
        // (bwservice.hpp:134-140): the setting is deprecated, and the
        // default (`USE_BWMACHINED`) rides the broadcast route instead.
        if interface_name != USE_BWMACHINED {
            eprintln!(
                "internalInterface set to '{interface_name}' in bw.xml. \
                 This option is deprecated. It is recommended to not set this \
                 value. The default behaviour is to use the same interface as \
                 bwmachined. This is controlled by the broadcast route."
            );
        }

        // loginapp's own external interface: the constructor opens it on
        // the configured port (loginapp.cpp:114-135), riding the same
        // dispatcher the loop pass drives, and serves the LoginInterface
        // messages on it. The reference's ctor runs at doBWMainT entry,
        // before ServerApp::init's startup line; the init arms that
        // check the bind run after it (loginapp.cpp:199-218), so the
        // check sits behind the running line below. The binding lives
        // until run's scope ends, like the reference member.
        let _login_external = if role == "loginapp" {
            let external_config = crate::loginapp_interface::LoginAppConfig::from_config(config);
            let external = crate::loginapp_interface::open(
                &external_config,
                crate::loginapp_login::LoginFacesConfig::from_config(config),
                std::rc::Rc::clone(&mercury_dispatcher),
            );
            Some(external)
        } else {
            None
        };

        // INFO_MSG( "---- %s is running ----\n" ) (server_app.cpp:278)
        eprintln!("---- {app_name} is running ----");

        // LoginApp::init's bind arms: the ERROR lines and the refusal.
        // bwMainT prints the shutdown line on any exit from runApp, an
        // init failure included (bwservice.hpp:141-145).
        if let Some(external) = _login_external.as_ref()
            && let Err(bind_error) = crate::loginapp_interface::check(external)
        {
            eprintln!("{app_name} has shut down.");
            return Err(AppError::LoginExternalBind(bind_error));
        }

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
                    let _ = platform::signals::thread_kill(main, platform::signals::SIGUSR1);
                });
        }

        // doBWMainT -> runApp -> run: mainDispatcher_.processUntilBreak()
        // (server_app.cpp:255-262). SIGTERM has no face here -- like the
        // reference, the default disposition takes the process.
        dispatcher.process_until_break();

        // runApp's tail walks fini() then interface_.prepareForShutdown()
        // (server_app.cpp:286-288; network_interface.cpp:178-185): the
        // drain keeps processing until the poller's handlers are ready
        // for shutdown, bounded by two real-clock seconds. The reference
        // also finalises the request manager there
        // (network_interface.cpp:183, 192); the port's request manager is
        // `None` until Mercury requests arrive, so the drain is the
        // whole face. Then:
        // INFO_MSG( "%s has shut down.\n", appName ) (bwservice.hpp:145)
        mercury_dispatcher.prepare_for_shutdown();

        eprintln!("{app_name} has shut down.");
    }

    #[cfg(not(unix))]
    {
        // The role is already validated; nothing to do beyond the
        // startup line until Windows console-event handling lands.
        let _ = config;
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
    /// the header macros' strings -- app names and config paths both.
    #[cfg(unix)]
    #[test]
    fn reference_app_names_cover_the_known_roles() {
        let names: Vec<&str> = ROLE_APP_NAMES.iter().map(|(role, _, _)| *role).collect();
        assert_eq!(names, KNOWN_ROLES, "one app name per role, in order");
        for (role, app_name, config_path) in ROLE_APP_NAMES {
            assert_eq!(validate_role(role), Ok(()));
            assert_eq!(reference_app_name(role), *app_name);
            assert_eq!(reference_config_path(role), *config_path);
        }
    }

    /// `getBWInternalInterfaceSetting`'s default arm
    /// (`bwconfig.cpp:455-462`): with nothing set, every role reads
    /// `USE_BWMACHINED`.
    #[cfg(unix)]
    #[test]
    fn the_interface_setting_defaults_to_bwmachined() {
        // An uninitialised tree is the no-bw.xml shape: `BIGWORLD_MAIN`
        // logged the failure and left the options at their defaults.
        let config = BwConfig::new();
        for role in KNOWN_ROLES {
            assert_eq!(
                internal_interface_setting(reference_config_path(role), &config),
                USE_BWMACHINED,
                "role {role} falls back to the default name"
            );
        }
    }

    /// The setting chain: the global `internalInterface` fills every
    /// role, and a role's own section hides it
    /// (`bwconfig.cpp:455-462`, the nested `BWConfig::get`).
    #[cfg(unix)]
    #[test]
    fn the_interface_setting_walks_the_config_chain() {
        let (global, _keep) = fixture_config(
            "<root>\n\
             \t<internalInterface>eth9</internalInterface>\n\
             </root>\n",
            "ii-global",
        );
        assert_eq!(internal_interface_setting("dbApp", &global), "eth9");
        assert_eq!(internal_interface_setting("loginApp", &global), "eth9");

        let (per_role, _keep) = fixture_config(
            "<root>\n\
             \t<dbApp>\n\
             \t\t<internalInterface>eth7</internalInterface>\n\
             \t</dbApp>\n\
             \t<internalInterface>eth9</internalInterface>\n\
             </root>\n",
            "ii-role",
        );
        assert_eq!(internal_interface_setting("dbApp", &per_role), "eth7");
        assert_eq!(internal_interface_setting("loginApp", &per_role), "eth9");
    }

    /// A scratch resource tree holding one `server/bw.xml`, the way
    /// `config.rs`'s fixture drives the chained sources.
    #[cfg(unix)]
    fn fixture_config(xml: &str, tag: &str) -> (BwConfig, std::path::PathBuf) {
        use resources::bwresource::BwResource;

        let root = std::env::temp_dir().join(format!("app-ii-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("server")).expect("mkdir the fixture root");
        std::fs::write(root.join("server/bw.xml"), xml).expect("write the fixture bw.xml");
        let resource = BwResource::init_paths(&[&root.to_string_lossy()]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);
        (config, root)
    }

    /// The query's success arm (`machined_utils.cpp:489-495`): a
    /// machined that answers `QueryInterface` hands the reply's address
    /// back. The responder sits on an ephemeral port so the production
    /// query to `PORT_MACHINED` -- and the child processes that make it
    /// -- are untouched.
    #[cfg(unix)]
    #[test]
    fn the_machined_query_reads_the_reply_address() {
        use std::net::UdpSocket;

        let responder = UdpSocket::bind("127.0.0.1:0").expect("bind the responder");
        let port = responder.local_addr().expect("responder addr").port();
        let reader = std::thread::spawn(move || {
            let mut buf = vec![0u8; mg::MgmPacket::MAX_SIZE];
            let (n, from) = responder.recv_from(&mut buf).expect("recv the query");
            let packet = mg::MgmPacket::from_bytes(&buf[..n]).expect("decode the query");
            // The reply echoes the query's own message, so the type is
            // asserted rather than matched; only the address changes.
            assert!(
                matches!(
                    packet.messages.first(),
                    Some(mg::MachineGuardMessage::QueryInterface(_))
                ),
                "the query is one QueryInterface: {:?}",
                packet.messages
            );
            let m = mg::QueryInterfaceMessage {
                address: 0x0100_007f,
                ..Default::default()
            };
            let reply = mg::MgmPacket::new(vec![mg::MachineGuardMessage::QueryInterface(m)]);
            responder
                .send_to(&reply.to_bytes().expect("encode the reply"), from)
                .expect("send the reply");
        });

        let client = MachinedClient::new().expect("the machined client");
        assert_eq!(
            query_machined_for_internal_interface(&client, loopback_ip(), port),
            Some(0x0100_007f)
        );
        reader.join().expect("the responder joins");
    }

    /// A reply that is not a `QueryInterface` takes the silent
    /// no-response arm (`machined_utils.cpp:502-504`).
    #[cfg(unix)]
    #[test]
    fn the_machined_query_ignores_a_foreign_reply() {
        use std::net::UdpSocket;

        let responder = UdpSocket::bind("127.0.0.1:0").expect("bind the responder");
        let port = responder.local_addr().expect("responder addr").port();
        let reader = std::thread::spawn(move || {
            let mut buf = vec![0u8; mg::MgmPacket::MAX_SIZE];
            let (n, from) = responder.recv_from(&mut buf).expect("recv the query");
            let _ = mg::MgmPacket::from_bytes(&buf[..n]).expect("decode the query");
            let foreign = mg::MgmPacket::new(vec![mg::MachineGuardMessage::MachinePlatform(
                mg::MachinePlatformMessage::default(),
            )]);
            responder
                .send_to(&foreign.to_bytes().expect("encode"), from)
                .expect("send the reply");
        });

        let client = MachinedClient::new().expect("the machined client");
        assert_eq!(
            query_machined_for_internal_interface(&client, loopback_ip(), port),
            None
        );
        reader.join().expect("the responder joins");
    }

    /// The join the loop lives on: a datagram to the role's interface
    /// surfaces through the role dispatcher's network pass -- the pass
    /// drains the Mercury dispatcher's faces, the poller fires the
    /// receiver, and the datagram reaches the interface's sink.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_network_pass_delivers_a_datagram_through_the_loop() {
        use mercury::endpoint::Address;
        use mercury::misc::Reason;
        use mercury::packet::Packet;
        use mercury::packet_receiver::PacketSink;

        const LOOPBACK: u32 = 0x0100_007f;

        struct RecordingSink {
            packets: std::cell::RefCell<Vec<Vec<u8>>>,
        }
        impl PacketSink for RecordingSink {
            fn process_packet(&self, _addr: &Address, packet: &mut Packet) -> Reason {
                self.packets.borrow_mut().push(packet.data().to_vec());
                Reason::Success
            }
        }

        let mercury_dispatcher =
            std::rc::Rc::new(mercury::event_dispatcher::EventDispatcher::new());
        let (interface, _receiver) =
            mercury::network_interface::NetworkInterface::new_on_dispatcher(
                mercury::network_interface::InterfaceType::Internal,
                std::rc::Rc::clone(&mercury_dispatcher),
                0,
                "127.0.0.1",
            );

        let sink = std::rc::Rc::new(RecordingSink {
            packets: std::cell::RefCell::new(Vec::new()),
        });
        interface.set_packet_sink(Some(sink.clone()));

        let mut dispatcher = crate::dispatcher::Dispatcher::new();
        dispatcher.set_network_pass(Box::new(MercuryPass::new(std::rc::Rc::clone(
            &mercury_dispatcher,
        ))));

        // One datagram from a fresh socket to the interface's port.
        let mut sender = mercury::endpoint::Endpoint::new();
        sender.socket(platform::socket::SOCK_DGRAM);
        assert_eq!(
            sender.sendto(b"ping", interface.address().port, LOOPBACK),
            4,
            "the datagram is queued"
        );

        dispatcher.process_once(true);
        assert_eq!(
            sink.packets.borrow().as_slice(),
            [b"ping".to_vec()],
            "the pass delivered the datagram to the interface"
        );
    }

    /// A loginapp whose external port is already taken refuses to run:
    /// the ctor's bind fails, the init arms log the reference's ERROR
    /// and `run` returns the refusal after the shutdown line
    /// (loginapp.cpp:199-206, bwservice.hpp:141-145). The reference's
    /// `shouldShutDownIfPortUsed` default takes the same shape -- no
    /// random-port fallback.
    #[cfg(unix)]
    #[test]
    fn a_taken_external_port_refuses_the_loginapp_run() {
        use mercury::endpoint::Endpoint;

        // `run` installs the single SIGQUIT slot; the lock keeps the
        // other run-driving test off it while this one runs.
        let _guard = crate::signals::disposition_guard();

        // Hold the default external port (PORT_LOGIN, portmap.hpp:7).
        let mut blocker = Endpoint::new();
        blocker.socket(platform::socket::SOCK_DGRAM);
        assert_eq!(
            blocker.bind(
                platform::socket::htons(crate::loginapp_interface::PORT_LOGIN),
                0, // INADDR_ANY, the interface's own bind target
            ),
            0,
            "the blocker holds the port"
        );

        let err = run("loginapp", &BwConfig::new()).expect_err("the bind is refused");
        assert_eq!(
            err,
            AppError::LoginExternalBind(crate::loginapp_interface::BindError::ExternalInterface)
        );
        assert_eq!(
            err.to_string(),
            "loginapp: unable to bind the external interface to any specified port"
        );
    }

    /// The loop is the acceptance: the dispatcher runs its frequent pass,
    /// the pass drains the pending SIGUSR1 into the controlled-stop arm,
    /// and the break unwinds into the shutdown tail. The signal is raised
    /// from a helper thread once the loop is up: `run`'s startup pays the
    /// machined query's two-second receive timeout on a machined-less
    /// host before the signal faces are installed, so an early SIGUSR1
    /// would hit the default disposition and kill the test -- the kicker
    /// waits past that timeout before raising.
    #[cfg(unix)]
    #[test]
    fn the_controlled_signal_ends_the_dispatch_loop() {
        let _guard = crate::signals::disposition_guard();
        let target = platform::signals::thread_self();
        let kicker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            let _ = platform::signals::thread_kill(target, platform::signals::SIGUSR1);
        });
        run("dbapp", &BwConfig::new()).expect("the controlled stop ends the loop");
        kicker.join().expect("the kicker joins");
    }
}
