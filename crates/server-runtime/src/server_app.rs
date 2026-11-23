//! `ServerAppConfig` + the `ServerApp` startup/exit-code pipeline
//! (`lib/server/server_app_config.cpp`, `server_app.cpp`, `bwservice.hpp`).
//!
//! Reference quirks preserved:
//! - `doBWMainT` returns **-1** (not `EXIT_FAILURE`) when the config
//!   options fail their post-init validation, and only
//!   `EXIT_SUCCESS`/`EXIT_FAILURE` (0/1) from the app lifecycle;
//! - the `BIGWORLD_MAIN` macro body *discards* both `BWResource::init`
//!   and `BWConfig::init` return values -- a missing config does not stop
//!   a server app, it runs on the option defaults (only `isBad()`
//!   reporting tools like consolidate_dbs treat it as an early-out with
//!   `EXIT_SUCCESS`);
//! - each option missing from the bw.xml chain logs a per-option warning
//!   and keeps its default (`ServerAppOptionIniterT::init`);
//! - `ServerApp::init` sets `runFromMachined` for every argument that is
//!   *not* `-machined` (`if (strcmp(...))` -- a real reference bug, kept);
//! - `runApp` always calls `fini()` then `interface_.prepareForShutdown()`,
//!   init-success or not, and `bwMainT` prints "%s has shut down." for
//!   every exit path -- the baseline exit order is init ->
//!   `---- %s is running ----` -> run (`processUntilBreak` +
//!   `onRunComplete`) -> fini -> prepareForShutdown;

use crate::config::BwConfig;
use crate::signals::{HandlerId, SignalHandler, SignalHandlerFn, SignalProcessor};
use resources::bwresource::BwResource;
use std::cell::RefCell;
use std::rc::Rc;

/// `DEFAULT_GAME_UPDATE_HERTZ` (server/common.hpp).
pub const DEFAULT_GAME_UPDATE_HERTZ: i32 = 10;

/// `DEFAULT_PERSONALITY_NAME` (pyscript/personality.hpp).
pub const DEFAULT_PERSONALITY_NAME: &str = "BWPersonality";

const DEFAULT_SERVER_MODE: &str = "standalone";
const CENTER_SERVER_MODE: &str = "center";
const PERIPHERY_SERVER_MODE: &str = "periphery";

/// `EXIT_SUCCESS`.
pub const EXIT_SUCCESS: i32 = 0;
/// `EXIT_FAILURE`.
pub const EXIT_FAILURE: i32 = 1;

/// The `ServerAppConfig` option set (`BW_OPTION_RO` statics), read from
/// the bw.xml chain on `init`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerAppConfig {
    /// `gameUpdateHertz` (10).
    pub update_hertz: i32,
    /// `personality` ("BWPersonality").
    pub personality: String,
    /// `serverMode` ("standalone").
    pub server_mode: String,
    /// `production` (true) -- the `isProduction` static reads this path.
    pub is_production: bool,
    /// `timeSyncPeriod` (60).
    pub time_sync_period: f32,
    /// `timeSyncPeriodInTicks` -- derived, no config path of its own.
    pub time_sync_period_in_ticks: i32,
    /// `useDefaultSpace` (false).
    pub use_default_space: bool,
    /// `maxOpenFileDescriptors` (-1).
    pub max_open_file_descriptors: i64,
    /// `maxSharedDataValueSize` (10240).
    pub max_shared_data_value_size: u32,
    /// `maxMgrRegisterStagger` (0.0).
    pub max_mgr_register_stagger: f32,
    /// `numStartupRetries` (60).
    pub num_startup_retries: i32,

    /// `CONFIG_INFO_MSG` capture of `ServerAppOptionIniter::printAll` --
    /// one line per configured option.
    pub print_lines: Vec<String>,
    /// Option paths that were not read from the chain (each got a
    /// `CONFIG_WARNING_MSG` and kept its default).
    pub unread_options: Vec<String>,
}

impl Default for ServerAppConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerAppConfig {
    /// Every static at its C++ initialiser value.
    pub fn new() -> Self {
        Self {
            update_hertz: DEFAULT_GAME_UPDATE_HERTZ,
            personality: DEFAULT_PERSONALITY_NAME.to_string(),
            server_mode: DEFAULT_SERVER_MODE.to_string(),
            is_production: true,
            time_sync_period: 60.0,
            time_sync_period_in_ticks: 0,
            use_default_space: false,
            max_open_file_descriptors: -1,
            max_shared_data_value_size: 10240,
            max_mgr_register_stagger: 0.0,
            num_startup_retries: 60,
            print_lines: Vec::new(),
            unread_options: Vec::new(),
        }
    }

    /// `ServerAppConfig::init`: read every option from the chain (missing
    /// ones warn and keep their default), run the post-init validation
    /// (which can fail the whole startup), then print the option table.
    /// The per-app `postInitFn` hook arrives with the role apps.
    pub fn init(&mut self, config: &BwConfig) -> bool {
        let mut unread = Vec::new();
        read_option_string(config, "personality", &mut self.personality, &mut unread);
        read_option_string(config, "serverMode", &mut self.server_mode, &mut unread);
        read_option_bool(config, "production", &mut self.is_production, &mut unread);
        read_option_i32(
            config,
            "gameUpdateHertz",
            &mut self.update_hertz,
            &mut unread,
        );
        read_option_f32(
            config,
            "timeSyncPeriod",
            &mut self.time_sync_period,
            &mut unread,
        );
        read_option_bool(
            config,
            "useDefaultSpace",
            &mut self.use_default_space,
            &mut unread,
        );
        read_option_i64(
            config,
            "maxOpenFileDescriptors",
            &mut self.max_open_file_descriptors,
            &mut unread,
        );
        read_option_u32(
            config,
            "maxSharedDataValueSize",
            &mut self.max_shared_data_value_size,
            &mut unread,
        );
        read_option_f32(
            config,
            "maxMgrRegisterStagger",
            &mut self.max_mgr_register_stagger,
            &mut unread,
        );
        read_option_i32(
            config,
            "numStartupRetries",
            &mut self.num_startup_retries,
            &mut unread,
        );
        self.unread_options = unread;

        let result = self.post_init();

        self.print_all();
        result
    }

    /// `ServerAppConfig::postInit`: derive `timeSyncPeriodInTicks`, check
    /// the server mode range, and clamp `maxMgrRegisterStagger`.
    fn post_init(&mut self) -> bool {
        self.time_sync_period_in_ticks =
            Self::seconds_to_ticks(self.time_sync_period, self.update_hertz, 1);

        if self.server_mode != DEFAULT_SERVER_MODE
            && self.server_mode != CENTER_SERVER_MODE
            && self.server_mode != PERIPHERY_SERVER_MODE
        {
            eprintln!(
                "ServerAppConfig::init: server_mode attribute in server/bw.xml \
                 is not in range: \"standalone\" \"center\" \"periphery\""
            );
            return false;
        }

        let max_mgr_register_stagger_period = 1.0 / self.update_hertz as f32;
        if self.max_mgr_register_stagger > max_mgr_register_stagger_period {
            self.max_mgr_register_stagger = max_mgr_register_stagger_period;
        }

        true
    }

    /// `ServerAppConfig::secondsToTicks`: `max(lowerBound,
    /// floor(seconds * hertz + 0.5))`.
    pub fn seconds_to_ticks(seconds: f32, hertz: i32, lower_bound: i32) -> i32 {
        lower_bound.max((seconds * hertz as f32 + 0.5).floor() as i32)
    }

    /// `ServerAppOptionIniter::printAll` capture. The reference pads the
    /// name to 34 columns; values equal to the default are printed
    /// without a default suffix.
    fn print_all(&mut self) {
        let mut lines = Vec::new();
        push_line(
            &mut lines,
            "gameUpdateHertz",
            &self.update_hertz.to_string(),
            "10",
        );
        push_line(
            &mut lines,
            "personality",
            &self.personality,
            DEFAULT_PERSONALITY_NAME,
        );
        push_line(
            &mut lines,
            "serverMode",
            &self.server_mode,
            DEFAULT_SERVER_MODE,
        );
        push_line(
            &mut lines,
            "production",
            &self.is_production.to_string(),
            "true",
        );
        push_line(
            &mut lines,
            "timeSyncPeriod",
            &format!("{:.6}", self.time_sync_period),
            "60.000000",
        );
        push_line(
            &mut lines,
            "useDefaultSpace",
            &self.use_default_space.to_string(),
            "false",
        );
        push_line(
            &mut lines,
            "maxOpenFileDescriptors",
            &self.max_open_file_descriptors.to_string(),
            "-1",
        );
        push_line(
            &mut lines,
            "maxSharedDataValueSize",
            &self.max_shared_data_value_size.to_string(),
            "10240",
        );
        push_line(
            &mut lines,
            "maxMgrRegisterStagger",
            &format!("{:.6}", self.max_mgr_register_stagger),
            "0.000000",
        );
        push_line(
            &mut lines,
            "numStartupRetries",
            &self.num_startup_retries.to_string(),
            "60",
        );
        self.print_lines = lines;
    }
}

/// One `%-34s = %s` line, with `(default: %s)` when the value differs
/// from the initialiser.
fn push_line(lines: &mut Vec<String>, name: &str, value: &str, default: &str) {
    if value == default {
        lines.push(format!("  {name:<34} = {value}"));
    } else {
        lines.push(format!("  {name:<34} = {value} (default: {default})"));
    }
}

// The `ServerAppOptionIniterT<TYPE>::init` flavours: `update` from the
// chain (recording a miss for the per-option warning), default otherwise.
fn read_option_string(config: &BwConfig, path: &str, value: &mut String, unread: &mut Vec<String>) {
    let mut updated = value.clone();
    if config.update_string(path, &mut updated) {
        *value = updated;
    } else {
        unread.push(path.to_string());
    }
}

fn read_option_bool(config: &BwConfig, path: &str, value: &mut bool, unread: &mut Vec<String>) {
    if !config.update_bool(path, value) {
        unread.push(path.to_string());
    }
}

fn read_option_i32(config: &BwConfig, path: &str, value: &mut i32, unread: &mut Vec<String>) {
    if !config.update_int(path, value) {
        unread.push(path.to_string());
    }
}

fn read_option_i64(config: &BwConfig, path: &str, value: &mut i64, unread: &mut Vec<String>) {
    let mut updated = *value;
    if config.update_long(path, &mut updated) {
        *value = updated;
    } else {
        unread.push(path.to_string());
    }
}

fn read_option_u32(config: &BwConfig, path: &str, value: &mut u32, unread: &mut Vec<String>) {
    let mut updated = *value;
    if config.update_uint(path, &mut updated) {
        *value = updated;
    } else {
        unread.push(path.to_string());
    }
}

fn read_option_f32(config: &BwConfig, path: &str, value: &mut f32, unread: &mut Vec<String>) {
    let mut updated = *value;
    if config.update_float(path, &mut updated) {
        *value = updated;
    } else {
        unread.push(path.to_string());
    }
}

/// The `ServerApp` lifecycle surface used by the startup pipeline
/// (`runApp`'s calls into the app) plus the control surface: shutdown
/// requests and signals (`BW-01-04-04`). The Mercury interface joins at
/// `BW-02`.
pub trait ServerApp {
    /// `getAppName`.
    fn app_name(&self) -> &str;

    /// `ServerApp::init`: returns false to abort the startup.
    fn init(&mut self, args: &[String]) -> bool;

    /// `ServerApp::run`. The base body (`processUntilBreak` +
    /// `onRunComplete` + success) is [`base_run`]; role apps that own a
    /// dispatcher inherit it through that helper.
    fn run(&mut self) -> bool;

    /// `ServerApp::onRunComplete` (a no-op in the base class): runs
    /// inside `run`, after the processing loop unwinds.
    fn on_run_complete(&mut self) {}

    /// `ServerApp::fini` (a no-op in the base class).
    fn fini(&mut self) {}

    /// `interface_.prepareForShutdown()`: the Mercury interface's
    /// shutdown preparation, flattened onto the app until the interface
    /// joins at BW-02. `runApp` always reaches it, right after `fini`.
    fn prepare_for_shutdown(&mut self) {}

    /// `ServerApp::shutDown`: the base class logs the request and breaks
    /// `mainDispatcher_`; the break flag lives in the app (it owns its
    /// dispatcher), so overrides log + break themselves -- this default
    /// keeps the base INFO line, exactly what
    /// `ServerApp::shutDown()` does before `breakProcessing()`.
    fn shut_down(&mut self) {
        eprintln!("ServerApp::shutDown: shutting down");
    }

    /// `ServerApp::onSignalled`: the default only reacts to SIGINT (the
    /// base implementation's switch has no other arm). Role apps that
    /// need more (e.g. the EntityApp SIGQUIT CRITICAL) override this and
    /// call [`base_on_signalled`] for the base arm.
    fn on_signalled(&mut self, sig_num: i32) {
        base_on_signalled(sig_num);
    }

    /// `ServerApp::onTickPeriod`: "gives subclasses the tick period each
    /// time advanceTime is called." The default is the base body -- the
    /// slow-tick warning ("Last game tick took %.2f seconds. Expected
    /// %.2f."). The hertz arrives as a parameter until the config
    /// statics go global.
    fn on_tick_period(&mut self, tick_period: f64, update_hertz: i32) {
        base_on_tick_period(tick_period, update_hertz);
    }

    /// "This method gives subclasses a chance to act at the end of a
    /// tick immediately before the current server time is incremented."
    fn on_end_of_tick(&mut self) {}

    /// "This method gives subclasses a chance to act at the beginning of
    /// a tick immediately after the current server time is incremented."
    fn on_start_of_tick(&mut self) {}

    /// "This method gives subclasses a chance to act at the end of
    /// ServerApp's tick processing, immediately before control returns
    /// to the caller of ServerApp::advanceTime()."
    fn on_tick_processing_complete(&mut self) {}

    /// `ServerApp::onSetStartTime` (a no-op in the base class).
    fn on_set_start_time(&mut self, _old_time: u32, _new_time: u32) {}
}

/// `ServerApp::onTickPeriod`'s base body: warn when the last game tick
/// took more than twice its budget.
pub fn base_on_tick_period(tick_period: f64, update_hertz: i32) {
    if tick_period * update_hertz as f64 > 2.0 {
        eprintln!(
            "ServerApp::onTickPeriod: Last game tick took \
             {:.2} seconds. Expected {:.2}.",
            tick_period,
            1.0 / update_hertz as f64
        );
    }
}

/// `ServerApp::onSignalled`, the base arm: on SIGINT reset the
/// disposition to `SIG_DFL` and re-raise. The reference comment says it
/// all: "This is really just to make sure that Python does not install
/// its own SIGINT handler." The re-raised signal kills the process with
/// the default action (during `dispatch` it stays blocked until the
/// Blocker restores the mask).
pub fn base_on_signalled(sig_num: i32) {
    if sig_num == libc::SIGINT {
        unsafe {
            libc::signal(sig_num, libc::SIG_DFL);
            libc::raise(sig_num);
        }
    }
}

/// The base `ServerApp::run` body: `mainDispatcher_.processUntilBreak()`,
/// then `onRunComplete()`, then success. Role apps that do not override
/// `run` inherit exactly this.
pub fn base_run(app: &mut dyn ServerApp, dispatcher: &mut crate::dispatcher::Dispatcher) -> bool {
    dispatcher.process_until_break();
    app.on_run_complete();
    true
}

/// The anonymous-namespace `ServerAppSignalHandler`: forwards every
/// dispatched signal into `app.on_signalled`.
pub fn app_signal_handler(app: &Rc<RefCell<dyn ServerApp>>) -> Rc<RefCell<dyn SignalHandler>> {
    let app = Rc::clone(app);
    Rc::new(RefCell::new(SignalHandlerFn(move |sig_num| {
        app.borrow_mut().on_signalled(sig_num);
    })))
}

/// `ServerApp::init`'s signal setup in one call:
/// `pSignalHandler_.reset( this->createSignalHandler() );` followed by
/// `this->enableSignalHandler( SIGINT );`. The returned id mirrors the
/// stored `pSignalHandler_` for later `clearSignalHandler` calls.
pub fn install_app_signal_handler(
    processor: &mut SignalProcessor,
    app: &Rc<RefCell<dyn ServerApp>>,
) -> HandlerId {
    processor.add_signal_handler(libc::SIGINT, app_signal_handler(app))
}

/// `ServerApp::runApp`: init gates run; fini always happens, followed by
/// `prepareForShutdown`; the bool is `doBWMainT`'s success/failure input.
/// The exit order is the baseline the acceptance pins: init ->
/// `---- %s is running ----` -> run -> fini -> prepareForShutdown.
pub fn run_app(app: &mut dyn ServerApp, args: &[String]) -> bool {
    // stampsPerSecond() -- clock warm-up, nothing observable here.
    let mut result = false;

    if app.init(args) {
        eprintln!("---- {} is running ----", app.app_name());
        result = app.run();
    } else {
        eprintln!("Failed to initialise {}", app.app_name());
    }

    app.fini();
    app.prepare_for_shutdown();
    result
}

/// `doBWMainT` without the Mercury wiring (dispatcher/interface arrive
/// with BW-02): config options first -- their failure is the **-1**
/// return -- then the app lifecycle, whose bool maps to 0/1.
pub fn do_bw_main_t(app: &mut dyn ServerApp, config: &BwConfig, args: &[String]) -> i32 {
    let mut app_config = ServerAppConfig::new();
    if !app_config.init(config) {
        eprintln!(
            "Failed to initialise configuration options. \
             See earlier error message for details."
        );
        return -1;
    }

    // SERVER_APP serverApp( dispatcher, interface ) -- the role apps
    // construct with their interfaces; the trait keeps construction out.
    if run_app(app, args) {
        EXIT_SUCCESS
    } else {
        EXIT_FAILURE
    }
}

/// `bwMainT` without the Mercury wiring (the dispatcher/interface join
/// at BW-02): run `do_bw_main_t`, then the unconditional tail line --
/// `INFO_MSG( "%s has shut down.\n", appName )` prints for every exit
/// path, the `-1` config failure included.
pub fn bw_main_t(app: &mut dyn ServerApp, config: &BwConfig, args: &[String]) -> i32 {
    // MachineDaemon::queryForInternalInterface, the deprecated
    // internalInterface warning and the message forwarder join with
    // BW-02.
    let result = do_bw_main_t(app, config, args);
    eprintln!("{} has shut down.", app.app_name());
    result
}

/// The unix `BIGWORLD_MAIN` macro body: resource init, config init, then
/// the app's `bwMain`. Both init return values are discarded here --
/// exactly like the reference, where a missing configuration does not
/// abort the process (the options fall back to their defaults).
///
/// `bwParseCommandLine` only touches the watcher/console logging flags
/// and has no effect without the debug filter; it joins when logging is
/// ported.
pub fn bigworld_main(
    resource: &mut BwResource,
    config: &mut BwConfig,
    app: &mut dyn ServerApp,
    args: &[String],
) -> i32 {
    let _ = resource.init_from_args(args);
    let _ = config.init_from_args(resource, args);
    bw_main_t(app, config, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BwConfig;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "boundless_bwa_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn write(path: &str, content: &str) {
        if let Some(parent) = std::path::Path::new(path).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    /// A resource + config over `server/bw.xml` with the given body.
    fn config_over(tag: &str, body: &str) -> (BwResource, BwConfig, String) {
        let root = temp_root(tag);
        write(&format!("{root}/server/bw.xml"), body);
        let resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);
        (resource, config, root)
    }

    fn valid_bw_xml() -> String {
        "<root><serverMode>standalone</serverMode></root>".to_string()
    }

    /// An app that records the lifecycle order and can fail on demand.
    struct ScriptedApp {
        name: &'static str,
        init_ok: bool,
        run_ok: bool,
        log: Rc<RefCell<Vec<String>>>,
        received_args: Vec<String>,
    }

    impl ScriptedApp {
        fn new(init_ok: bool, run_ok: bool, log: &Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                name: "cellapp",
                init_ok,
                run_ok,
                log: Rc::clone(log),
                received_args: Vec::new(),
            }
        }
    }

    impl ServerApp for ScriptedApp {
        fn app_name(&self) -> &str {
            self.name
        }

        fn init(&mut self, args: &[String]) -> bool {
            self.received_args = args.to_vec();
            self.log.borrow_mut().push("init".to_string());
            self.init_ok
        }

        fn run(&mut self) -> bool {
            self.log.borrow_mut().push("run".to_string());
            self.run_ok
        }

        fn fini(&mut self) {
            self.log.borrow_mut().push("fini".to_string());
        }
    }

    /// Acceptance: an abnormal entry (invalid server_mode) exits -1, a
    /// failing app exits 1, a healthy app exits 0.
    #[test]
    fn abnormal_entries_map_to_the_reference_exit_codes() {
        // Config options fail their post-init validation -> -1, and the
        // app never starts.
        let (_r, config, _k) = config_over(
            "exit_badmode",
            "<root><serverMode>bogus</serverMode></root>",
        );
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = ScriptedApp::new(true, true, &log);
        assert_eq!(do_bw_main_t(&mut app, &config, &[]), -1);
        assert!(log.borrow().is_empty(), "app must not start: {:?}", log);

        // The app's init failing -> 1, with fini still run.
        let (_r, config, _k) = config_over("exit_initfail", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = ScriptedApp::new(false, true, &log);
        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_FAILURE);
        assert_eq!(*log.borrow(), ["init", "fini"]);

        // The app's run failing -> 1.
        let mut app = ScriptedApp::new(true, false, &log);
        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_FAILURE);
        assert_eq!(*log.borrow(), ["init", "fini", "init", "run", "fini"]);

        // Healthy startup -> 0.
        let mut app = ScriptedApp::new(true, true, &log);
        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_SUCCESS);
        assert_eq!(
            *log.borrow(),
            ["init", "fini", "init", "run", "fini", "init", "run", "fini"]
        );
    }

    /// Acceptance: a missing configuration does not stop a server app --
    /// the BIGWORLD_MAIN body discards the init results and every option
    /// keeps its default; `isBad` is the tool-side signal.
    #[test]
    fn missing_config_still_runs_on_defaults() {
        let root = temp_root("exit_noconfig");
        let mut resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = ScriptedApp::new(true, true, &log);

        assert_eq!(
            bigworld_main(&mut resource, &mut config, &mut app, &[]),
            EXIT_SUCCESS
        );
        assert_eq!(*log.borrow(), ["init", "run", "fini"]);
        assert!(config.is_bad(&resource), "the missing file is recorded");

        let app_config = ServerAppConfig::new();
        assert_eq!(app_config.update_hertz, DEFAULT_GAME_UPDATE_HERTZ);
        assert_eq!(app_config.server_mode, DEFAULT_SERVER_MODE);
        assert_eq!(app_config.personality, DEFAULT_PERSONALITY_NAME);
        assert!(app_config.is_production);
    }

    /// Options read from the chain override their defaults; the print
    /// table marks the changed ones.
    #[test]
    fn options_read_from_the_chain_and_print_their_changes() {
        let (_r, config, _k) = config_over(
            "opt_values",
            "<root>\n\
             \t<gameUpdateHertz>20</gameUpdateHertz>\n\
             \t<personality>MyPersonality</personality>\n\
             \t<timeSyncPeriod>30</timeSyncPeriod>\n\
             \t<maxMgrRegisterStagger>0.5</maxMgrRegisterStagger>\n\
             \t<numStartupRetries>5</numStartupRetries>\n\
             \t<production>false</production>\n\
             \t<useDefaultSpace>true</useDefaultSpace>\n\
             \t<maxOpenFileDescriptors>4096</maxOpenFileDescriptors>\n\
             \t<maxSharedDataValueSize>2048</maxSharedDataValueSize>\n\
             </root>",
        );
        let mut app_config = ServerAppConfig::new();
        assert!(app_config.init(&config));

        assert_eq!(app_config.update_hertz, 20);
        assert_eq!(app_config.personality, "MyPersonality");
        // Derived: max(1, floor(30 * 20 + 0.5)) = 600.
        assert_eq!(app_config.time_sync_period_in_ticks, 600);
        // Clamped to 1/hertz = 0.05.
        assert!((app_config.max_mgr_register_stagger - 0.05).abs() < 1e-6);
        assert_eq!(app_config.num_startup_retries, 5);
        assert!(!app_config.is_production);
        assert!(app_config.use_default_space);
        assert_eq!(app_config.max_open_file_descriptors, 4096);
        assert_eq!(app_config.max_shared_data_value_size, 2048);

        // Unchanged options print bare; changed ones carry the default.
        let lines = &app_config.print_lines;
        assert!(lines.contains(&format!("  {:<34} = standalone", "serverMode")));
        assert!(lines.iter().any(|l| l.starts_with("  personality")
            && l.contains("MyPersonality")
            && l.contains("(default: BWPersonality)")));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("  gameUpdateHertz") && l.contains("(default: 10)"))
        );
        // Only the five options absent from the fixture warn.
        assert_eq!(app_config.unread_options, ["serverMode"]);
    }

    /// `Default` mirrors `new`.
    #[test]
    fn default_matches_new() {
        assert_eq!(ServerAppConfig::default(), ServerAppConfig::new());
    }

    /// An app without a `fini` override uses the trait's no-op default;
    /// the lifecycle still records init/run around it.
    #[test]
    fn bare_app_uses_the_default_finis() {
        struct BareApp {
            ran: Cell<bool>,
        }

        impl ServerApp for BareApp {
            fn app_name(&self) -> &str {
                "bareapp"
            }
            fn init(&mut self, _args: &[String]) -> bool {
                true
            }
            fn run(&mut self) -> bool {
                self.ran.set(true);
                true
            }
            // No fini override: the base-class no-op.
        }

        let mut app = BareApp {
            ran: Cell::new(false),
        };
        assert_eq!(ServerApp::app_name(&app), "bareapp");
        assert_eq!(run_app(&mut app, &[]), true);
        assert!(app.ran.get());

        // The base shutDown: the INFO line only; the override breaks the
        // loop (the app owns its dispatcher).
        ServerApp::shut_down(&mut app);

        // The pipeline hooks are no-ops by default too.
        ServerApp::on_run_complete(&mut app);
        ServerApp::prepare_for_shutdown(&mut app);
    }

    /// Every option missing from the chain warns and keeps its default.
    #[test]
    fn absent_options_warn_and_keep_defaults() {
        let (_r, config, _k) = config_over("opt_empty", "<root />");
        let mut app_config = ServerAppConfig::new();
        assert!(app_config.init(&config));

        assert_eq!(app_config.update_hertz, DEFAULT_GAME_UPDATE_HERTZ);
        assert_eq!(app_config.time_sync_period_in_ticks, 600); // 60 * 10
        assert_eq!(
            app_config.unread_options,
            [
                "personality",
                "serverMode",
                "production",
                "gameUpdateHertz",
                "timeSyncPeriod",
                "useDefaultSpace",
                "maxOpenFileDescriptors",
                "maxSharedDataValueSize",
                "maxMgrRegisterStagger",
                "numStartupRetries",
            ]
        );
    }

    /// `secondsToTicks`: floor with the 0.5 nudge and the lower bound.
    #[test]
    fn seconds_to_ticks_matches_the_reference_rounding() {
        assert_eq!(ServerAppConfig::seconds_to_ticks(60.0, 10, 1), 600);
        assert_eq!(ServerAppConfig::seconds_to_ticks(0.25, 10, 1), 3); // 3.0
        assert_eq!(ServerAppConfig::seconds_to_ticks(0.05, 10, 1), 1); // 1.0
        assert_eq!(ServerAppConfig::seconds_to_ticks(-5.0, 10, 1), 1); // bound
        assert_eq!(ServerAppConfig::seconds_to_ticks(0.0, 10, 1), 1); // bound
    }

    /// `ServerApp::init` marks `runFromMachined` for any argument that is
    /// not `-machined` -- the reference's `if (strcmp(...))` reads args,
    /// not flags. The port passes argv through unchanged.
    #[test]
    fn app_init_receives_the_full_command_line() {
        let (_r, config, _k) = config_over("argv_pass", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = ScriptedApp::new(true, true, &log);

        let args: Vec<String> = ["prog".to_string(), "-machined".to_string()].to_vec();
        assert_eq!(do_bw_main_t(&mut app, &config, &args), EXIT_SUCCESS);
        assert_eq!(app.received_args, args);
    }

    // ---- BW-01-04-04: stop signals and control events ----

    use crate::dispatcher::{Dispatcher, FrequentTaskFn};
    use crate::signals::{
        Blocker, HandlerId, Set, SignalDispatchTask, SignalProcessor, disposition_of,
        kernel_pending_signals, pending_signals, signal_number_to_string,
    };

    /// A role-app skeleton: owns its dispatcher (the `mainDispatcher_`
    /// member) and registers a control-event handler as a frequent task
    /// -- the BW-02 stand-in for the Mercury shutDown message handler.
    /// The handler body is `ServerApp::shutDown`'s two effects (INFO
    /// line + breakProcessing), flattened through the shared break flag
    /// because the app is `&mut`-borrowed while its loop runs (the C++
    /// original reaches its own members re-entrantly).
    struct ControlApp {
        dispatcher: Dispatcher,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl ControlApp {
        fn new(log: &Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                dispatcher: Dispatcher::new(),
                log: Rc::clone(log),
            }
        }
    }

    impl ServerApp for ControlApp {
        fn app_name(&self) -> &str {
            "cellapp"
        }

        fn init(&mut self, _args: &[String]) -> bool {
            self.log.borrow_mut().push("init".to_string());

            let log = Rc::clone(&self.log);
            let break_flag = self.dispatcher.break_flag();
            self.dispatcher
                .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                    move || {
                        log.borrow_mut().push("control event".to_string());
                        log.borrow_mut()
                            .push("ServerApp::shutDown: shutting down".to_string());
                        break_flag.set(true);
                    },
                    "control event",
                ))));
            true
        }

        fn run(&mut self) -> bool {
            self.dispatcher.process_until_break();
            true
        }

        fn shut_down(&mut self) {
            self.log
                .borrow_mut()
                .push("ServerApp::shutDown: shutting down".to_string());
            self.dispatcher.break_processing(true);
        }
    }

    /// Acceptance (BW-01-04-04): a control event breaks the processing
    /// loop, the app leaves `processUntilBreak` and exits 0 -- the app
    /// entered its shutdown stage.
    #[test]
    fn control_event_enters_the_shutdown_stage() {
        let (_r, config, _k) = config_over("ctl_event", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = ControlApp::new(&log);

        assert_eq!(ServerApp::app_name(&app), "cellapp");
        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_SUCCESS);
        assert_eq!(
            *log.borrow(),
            [
                "init",
                "control event",
                "ServerApp::shutDown: shutting down",
            ]
        );
    }

    /// The trait's control surface: an app that owns its dispatcher
    /// implements `shut_down` as the base body (INFO line) plus breaking
    /// the loop via the shared flag.
    #[test]
    fn shut_down_override_breaks_via_the_shared_flag() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = ControlApp::new(&log);

        let break_flag = app.dispatcher.break_flag();
        ServerApp::shut_down(&mut app);
        assert!(break_flag.get());
        assert_eq!(*log.borrow(), ["ServerApp::shutDown: shutting down"]);
    }

    /// The base `onSignalled` SIGINT arm: reset the disposition to
    /// SIG_DFL and re-raise ("make sure that Python does not install its
    /// own SIGINT handler"). The re-raise happens while SIGINT is blocked
    /// -- exactly like inside `dispatch()` -- so it waits kernel-pending
    /// instead of killing the process.
    #[test]
    fn sigint_resets_the_disposition_and_reraises() {
        let _guard = crate::signals::disposition_guard();

        let mut blocked = Set::empty();
        blocked.set(libc::SIGINT);
        let _blocker = Blocker::new(&blocked);

        base_on_signalled(libc::SIGINT);
        assert_eq!(disposition_of(libc::SIGINT), libc::SIG_DFL as usize);
        assert!(
            kernel_pending_signals() & (1 << libc::SIGINT) != 0,
            "the re-raised SIGINT waits blocked"
        );

        // Consume the pending signal so unblocking is safe (the
        // reference lets the default action kill the process; the test
        // must survive).
        let mut wait_set = Set::empty();
        wait_set.set(libc::SIGINT);
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            assert_eq!(
                libc::sigtimedwait(wait_set.raw(), &mut info, &timeout),
                libc::SIGINT
            );
        }
    }

    /// The default `on_signalled` (and the base arm for non-SIGINT
    /// signals) does nothing: the switch has no other arm.
    #[test]
    fn on_signalled_default_ignores_other_signals() {
        let _guard = crate::signals::disposition_guard();

        let before = kernel_pending_signals();
        base_on_signalled(libc::SIGTERM);
        base_on_signalled(libc::SIGUSR2);
        assert_eq!(kernel_pending_signals(), before);
        assert_eq!(disposition_of(libc::SIGTERM), libc::SIG_DFL as usize);

        // The trait default routes straight into the base arm.
        struct BareSignalApp;
        impl ServerApp for BareSignalApp {
            fn app_name(&self) -> &str {
                "baresignalapp"
            }
            fn init(&mut self, _args: &[String]) -> bool {
                true
            }
            fn run(&mut self) -> bool {
                true
            }
        }
        let mut bare = BareSignalApp;
        assert_eq!(ServerApp::app_name(&bare), "baresignalapp");
        assert!(ServerApp::init(&mut bare, &[]));
        assert!(ServerApp::run(&mut bare));
        ServerApp::on_signalled(&mut bare, libc::SIGUSR1);
        assert_eq!(kernel_pending_signals(), before);
        assert_eq!(pending_signals(), 0);
    }

    /// A role app whose onSignalled body runs through the dispatcher:
    /// the raised signal is drained by the SignalProcessor frequent
    /// task, the handler runs on the loop thread and breaks processing.
    /// This is the signal half of the BW-01-04-04 acceptance.
    #[test]
    fn signal_delivers_through_the_dispatcher_into_the_app() {
        let _guard = crate::signals::disposition_guard();

        // The SignalProcessor-as-FrequentTask binding uses the
        // reference's profiler label.
        let probe = Rc::new(RefCell::new(SignalProcessor::new()));
        let probe_task = SignalDispatchTask::new_task(&probe);
        assert_eq!(probe_task.borrow().task_name(), "SignalProcessor");

        struct SignalApp {
            dispatcher: Dispatcher,
            log: Rc<RefCell<Vec<String>>>,
            _processor: Option<Rc<RefCell<SignalProcessor>>>,
            _handler_id: Option<HandlerId>,
        }

        impl ServerApp for SignalApp {
            fn app_name(&self) -> &str {
                "cellapp"
            }

            fn init(&mut self, _args: &[String]) -> bool {
                self.log.borrow_mut().push("init".to_string());

                let processor = Rc::new(RefCell::new(SignalProcessor::new()));
                let log = Rc::clone(&self.log);
                let break_flag = self.dispatcher.break_flag();
                let handler = Rc::new(RefCell::new(SignalHandlerFn(move |sig| {
                    log.borrow_mut()
                        .push(format!("onSignalled {}", signal_number_to_string(sig)));
                    break_flag.set(true);
                })));
                let id = processor
                    .borrow_mut()
                    .add_signal_handler(libc::SIGTERM, handler);

                self.dispatcher
                    .add_frequent_task(SignalDispatchTask::new_task(&processor));

                // The stop signal arrives while the loop runs (an async
                // SIGTERM raised mid-processing, exactly when a real
                // stop request lands).
                let raised = Cell::new(false);
                self.dispatcher
                    .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                        move || {
                            if !raised.get() {
                                raised.set(true);
                                unsafe {
                                    libc::raise(libc::SIGTERM);
                                }
                            }
                        },
                        "stop signal",
                    ))));

                self._processor = Some(processor);
                self._handler_id = Some(id);
                true
            }

            fn run(&mut self) -> bool {
                self.dispatcher.process_until_break();
                true
            }
        }

        let (_r, config, _k) = config_over("signal_app", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = SignalApp {
            dispatcher: Dispatcher::new(),
            log: Rc::clone(&log),
            _processor: None,
            _handler_id: None,
        };

        assert_eq!(ServerApp::app_name(&app), "cellapp");
        // The raise happens mid-loop (inside init's "stop signal" task);
        // the next pass drains it into the handler and the app exits its
        // loop cleanly.
        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_SUCCESS);
        assert_eq!(*log.borrow(), ["init", "onSignalled SIGTERM"]);
        assert_eq!(pending_signals(), 0, "the drain empties the set");
    }

    /// `install_app_signal_handler` routes dispatched signals into
    /// `app.on_signalled` (the `ServerAppSignalHandler` wiring), and the
    /// routed SIGINT hits the base arm's disposition reset + re-raise.
    #[test]
    fn app_signal_handler_routes_into_on_signalled() {
        let _guard = crate::signals::disposition_guard();

        // SIGINT stays blocked for the whole test: the base arm's
        // re-raise must park kernel-pending instead of killing us.
        let mut blocked = Set::empty();
        blocked.set(libc::SIGINT);
        let _blocker = Blocker::new(&blocked);

        struct RoutedApp {
            log: Rc<RefCell<Vec<String>>>,
        }
        impl ServerApp for RoutedApp {
            fn app_name(&self) -> &str {
                "routedapp"
            }
            fn init(&mut self, _args: &[String]) -> bool {
                true
            }
            fn run(&mut self) -> bool {
                true
            }
            fn on_signalled(&mut self, sig_num: i32) {
                self.log
                    .borrow_mut()
                    .push(format!("onSignalled {}", signal_number_to_string(sig_num)));
                base_on_signalled(sig_num);
            }
        }

        let log = Rc::new(RefCell::new(Vec::new()));
        let app: Rc<RefCell<dyn ServerApp>> = Rc::new(RefCell::new(RoutedApp {
            log: Rc::clone(&log),
        }));

        let mut processor = SignalProcessor::new();
        let id = install_app_signal_handler(&mut processor, &app);

        assert_eq!(ServerApp::app_name(&*app.borrow()), "routedapp");
        // The wiring test's app still runs a plain lifecycle through
        // run_app (init -> run -> fini) before the signal round.
        {
            let mut borrowed = app.borrow_mut();
            assert!(run_app(&mut *borrowed, &[]));
        }

        // The libtest harness blocks SIGINT on its worker threads; a
        // real server thread does not. Unblock so the raise reaches the
        // C handler.
        let mut unblock = Set::empty();
        unblock.set(libc::SIGINT);
        unsafe {
            libc::pthread_sigmask(libc::SIG_UNBLOCK, unblock.raw(), std::ptr::null_mut());
        }

        // The raise must be unblocked so the C handler marks the pending
        // bit; the block starts before dispatch, so the routed base arm's
        // re-raise parks instead of killing us, and stays up until the
        // parked signal is consumed.
        unsafe {
            libc::raise(libc::SIGINT);
        }
        let mut blocked = Set::empty();
        blocked.set(libc::SIGINT);
        let blocker = Blocker::new(&blocked);
        processor.dispatch();

        assert_eq!(*log.borrow(), ["onSignalled SIGINT"]);
        assert_eq!(disposition_of(libc::SIGINT), libc::SIG_DFL as usize);
        assert!(
            kernel_pending_signals() & (1 << libc::SIGINT) != 0,
            "the routed re-raise parks blocked"
        );

        // Consume the parked signal while still blocked.
        let mut wait_set = Set::empty();
        wait_set.set(libc::SIGINT);
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            assert_eq!(
                libc::sigtimedwait(wait_set.raw(), &mut info, &timeout),
                libc::SIGINT
            );
        }
        drop(blocker);
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
        assert!(processor.clear_signal_handler(libc::SIGINT, id));
    }

    // ---- BW-01-04-05: staged shutdown and the baseline exit order ----

    use crate::shutdown::{ShutDownStage, ShutdownState, TaskManager, shut_down_stage_to_string};

    /// The base `ServerApp::run` body: `processUntilBreak` unwinds on the
    /// breaker, `onRunComplete` runs, success returns.
    #[test]
    fn base_run_runs_the_base_run_body() {
        struct HookProbeApp {
            completed: Cell<bool>,
        }
        impl ServerApp for HookProbeApp {
            fn app_name(&self) -> &str {
                "probe"
            }
            fn init(&mut self, _args: &[String]) -> bool {
                true
            }
            fn run(&mut self) -> bool {
                true
            }
            fn on_run_complete(&mut self) {
                self.completed.set(true);
            }
        }

        let mut app = HookProbeApp {
            completed: Cell::new(false),
        };
        // The probe's required trait methods behave like the reference's
        // defaults: the name reports, init/run succeed.
        assert_eq!(ServerApp::app_name(&app), "probe");
        assert!(ServerApp::init(&mut app, &[]));
        assert!(ServerApp::run(&mut app));

        let mut dispatcher = Dispatcher::new();
        let break_flag = dispatcher.break_flag();
        dispatcher.add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
            move || break_flag.set(true),
            "breaker",
        ))));

        assert_eq!(base_run(&mut app, &mut dispatcher), true);
        assert!(app.completed.get());
    }

    /// The full pipeline with every hook recorded. `run` here is the base
    /// body (loop + onRunComplete), so the log pins the baseline order:
    /// init -> run -> onRunComplete -> fini -> prepareForShutdown.
    struct LifecycleApp {
        log: Rc<RefCell<Vec<String>>>,
        dispatcher: Dispatcher,
        init_ok: bool,
        run_ok: bool,
    }

    impl LifecycleApp {
        fn new(init_ok: bool, run_ok: bool, log: &Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                log: Rc::clone(log),
                dispatcher: Dispatcher::new(),
                init_ok,
                run_ok,
            }
        }
    }

    impl ServerApp for LifecycleApp {
        fn app_name(&self) -> &str {
            "cellapp"
        }

        fn init(&mut self, _args: &[String]) -> bool {
            self.log.borrow_mut().push("init".to_string());
            let break_flag = self.dispatcher.break_flag();
            self.dispatcher
                .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                    move || break_flag.set(true),
                    "breaker",
                ))));
            self.init_ok
        }

        fn run(&mut self) -> bool {
            self.log.borrow_mut().push("run".to_string());
            self.dispatcher.process_until_break();
            self.on_run_complete();
            self.run_ok
        }

        fn on_run_complete(&mut self) {
            self.log.borrow_mut().push("onRunComplete".to_string());
        }

        fn fini(&mut self) {
            self.log.borrow_mut().push("fini".to_string());
        }

        fn prepare_for_shutdown(&mut self) {
            self.log.borrow_mut().push("prepareForShutdown".to_string());
        }
    }

    /// Acceptance: a healthy run ends in the baseline order -- init, run,
    /// onRunComplete, fini, prepareForShutdown -- and exits 0.
    #[test]
    fn the_baseline_exit_order_is_pinned_for_a_healthy_run() {
        let (_r, config, _k) = config_over("exit_order_ok", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = LifecycleApp::new(true, true, &log);

        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_SUCCESS);
        assert_eq!(
            *log.borrow(),
            ["init", "run", "onRunComplete", "fini", "prepareForShutdown",]
        );
    }

    /// A failing init skips run and onRunComplete entirely, but fini and
    /// prepareForShutdown still run -- runApp reaches both unconditionally.
    #[test]
    fn fini_and_prepare_for_shutdown_run_even_when_init_fails() {
        let (_r, config, _k) = config_over("exit_order_initfail", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = LifecycleApp::new(false, true, &log);

        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_FAILURE);
        assert_eq!(*log.borrow(), ["init", "fini", "prepareForShutdown"]);
    }

    /// A failing run still unwinds through the same tail: onRunComplete
    /// happened inside run, fini and prepareForShutdown follow.
    #[test]
    fn a_failing_run_keeps_the_tail_order() {
        let (_r, config, _k) = config_over("exit_order_runfail", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = LifecycleApp::new(true, false, &log);

        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_FAILURE);
        assert_eq!(
            *log.borrow(),
            ["init", "run", "onRunComplete", "fini", "prepareForShutdown",]
        );
    }

    /// `bwMainT` runs `doBWMainT` and then prints the shutdown tail for
    /// every exit path -- the -1 config failure, the 1 init/run failure
    /// and the 0 success all flow through it.
    #[test]
    fn bw_main_t_returns_every_exit_code_through_the_tail() {
        // Config options fail -> -1 straight through bwMainT.
        let (_r, config, _k) = config_over(
            "bwmain_badmode",
            "<root><serverMode>bogus</serverMode></root>",
        );
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = ScriptedApp::new(true, true, &log);
        assert_eq!(bw_main_t(&mut app, &config, &[]), -1);
        assert!(log.borrow().is_empty());

        // App init fails -> 1.
        let (_r, config, _k) = config_over("bwmain_initfail", &valid_bw_xml());
        let mut app = ScriptedApp::new(false, true, &log);
        assert_eq!(bw_main_t(&mut app, &config, &[]), EXIT_FAILURE);

        // Healthy -> 0.
        let mut app = ScriptedApp::new(true, true, &log);
        assert_eq!(bw_main_t(&mut app, &config, &[]), EXIT_SUCCESS);
    }

    /// Acceptance (background half): a background task submitted during
    /// the loop runs its phase inline, its completion drains on the next
    /// tick, and a completion queued after the last tick still finishes
    /// inside `stopAll`'s final tick -- during fini, before
    /// prepareForShutdown and before the process ends.
    #[test]
    fn background_tasks_end_in_the_baseline_order() {
        struct BgApp {
            log: Rc<RefCell<Vec<String>>>,
            dispatcher: Dispatcher,
            tasks: Rc<RefCell<TaskManager>>,
        }

        impl ServerApp for BgApp {
            fn app_name(&self) -> &str {
                "cellapp"
            }

            fn init(&mut self, _args: &[String]) -> bool {
                self.log.borrow_mut().push("init".to_string());

                // "queue work" (pass 1): submit a background task. Its
                // phase runs inline now; its completion waits for a tick.
                let log = Rc::clone(&self.log);
                let tasks = Rc::clone(&self.tasks);
                let queued = Cell::new(false);
                self.dispatcher
                    .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                        move || {
                            if queued.get() {
                                return;
                            }
                            queued.set(true);
                            let task = LabelledTask::new("bg work", "bg cleanup", &log);
                            task.borrow_mut()
                                .self_rc
                                .borrow_mut()
                                .replace(Rc::clone(&task));
                            tasks
                                .borrow_mut()
                                .add_background_task(task as Rc<RefCell<dyn BackgroundTask>>);
                        },
                        "queue work",
                    ))));

                // The game-tick stand-in: drain completions every pass.
                let tasks = Rc::clone(&self.tasks);
                self.dispatcher
                    .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                        move || tasks.borrow_mut().tick(),
                        "tick",
                    ))));

                // "late work" (pass 2, after the tick): one more
                // background task, still pending when the loop breaks.
                let log = Rc::clone(&self.log);
                let tasks = Rc::clone(&self.tasks);
                let passes = Cell::new(0);
                self.dispatcher
                    .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                        move || {
                            passes.set(passes.get() + 1);
                            if passes.get() < 2 {
                                return;
                            }
                            let task = LabelledTask::new("late bg work", "late cleanup", &log);
                            assert_eq!(task.borrow().name(), "late bg work");
                            task.borrow_mut()
                                .self_rc
                                .borrow_mut()
                                .replace(Rc::clone(&task));
                            tasks
                                .borrow_mut()
                                .add_background_task(task as Rc<RefCell<dyn BackgroundTask>>);
                        },
                        "late work",
                    ))));

                // Break at the end of pass 2.
                let break_flag = self.dispatcher.break_flag();
                let passes = Cell::new(0);
                self.dispatcher
                    .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                        move || {
                            passes.set(passes.get() + 1);
                            if passes.get() >= 2 {
                                break_flag.set(true);
                            }
                        },
                        "breaker",
                    ))));
                true
            }

            fn run(&mut self) -> bool {
                self.dispatcher.process_until_break();
                true
            }

            fn fini(&mut self) {
                self.log.borrow_mut().push("fini".to_string());
                // The task manager's shutdown drain: stopAll's final tick
                // finishes whatever is still queued.
                self.tasks.borrow_mut().stop_all();
            }

            fn prepare_for_shutdown(&mut self) {
                self.log.borrow_mut().push("prepareForShutdown".to_string());
            }
        }

        /// A task that works in the background phase, then "adds itself
        /// back" so its completion runs on the main thread with a
        /// different label.
        struct LabelledTask {
            work: &'static str,
            cleanup: &'static str,
            log: Rc<RefCell<Vec<String>>>,
            self_rc: RefCell<Option<Rc<RefCell<LabelledTask>>>>,
        }

        impl LabelledTask {
            fn new(
                work: &'static str,
                cleanup: &'static str,
                log: &Rc<RefCell<Vec<String>>>,
            ) -> Rc<RefCell<Self>> {
                Rc::new(RefCell::new(Self {
                    work,
                    cleanup,
                    log: Rc::clone(log),
                    self_rc: RefCell::new(None),
                }))
            }
        }

        impl BackgroundTask for LabelledTask {
            fn do_background_task(&mut self, mgr: &mut TaskManager) {
                self.log.borrow_mut().push(self.work.to_string());
                let rc = Rc::clone(self.self_rc.borrow().as_ref().unwrap());
                mgr.add_main_thread_task(rc);
            }

            fn do_main_thread_task(&mut self, _mgr: &mut TaskManager) {
                self.log.borrow_mut().push(self.cleanup.to_string());
            }

            fn name(&self) -> &str {
                self.work
            }
        }

        use crate::shutdown::BackgroundTask;

        let (_r, config, _k) = config_over("bg_order", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = BgApp {
            log: Rc::clone(&log),
            dispatcher: Dispatcher::new(),
            tasks: Rc::new(RefCell::new(TaskManager::new())),
        };

        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_SUCCESS);
        assert_eq!(
            *log.borrow(),
            [
                "init",
                "bg work",      // pass 1: inline background phase
                "bg cleanup",   // pass 2: tick drains the completion
                "late bg work", // pass 2: queued after the last tick
                "fini",
                "late cleanup", // stopAll's final tick, inside fini
                "prepareForShutdown",
            ]
        );
    }

    /// Acceptance (drain half): work queued by the loop and its
    /// completions are drained in `fini`, before
    /// `prepareForShutdown` -- the reference's `~BaseApp` runs
    /// `bgTaskManager_.stopAll()` before
    /// `extInterface().prepareForShutdown()`. The threaded pool is used,
    /// so whichever of "worker still running" and "shutdown started"
    /// happens first, the end state is the same.
    #[test]
    fn the_shutdown_drains_the_threaded_task_pool() {
        use crate::task_manager::{BackgroundTask, TaskManager, TaskPtr};
        use std::sync::Arc;

        type SharedLog = std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>;

        struct DrainingTask {
            work: &'static str,
            cleanup: &'static str,
            log: SharedLog,
            me: std::sync::Mutex<Option<TaskPtr>>,
        }

        impl BackgroundTask for DrainingTask {
            fn do_background_task(&self, mgr: &TaskManager) {
                self.log.lock().expect("log").push(self.work);
                let me = self.me.lock().expect("me").clone().expect("self set");
                mgr.add_main_thread_task(me);
            }

            fn do_main_thread_task(&self, _mgr: &TaskManager) {
                self.log.lock().expect("log").push(self.cleanup);
            }

            fn name(&self) -> &str {
                self.work
            }
        }

        struct DrainedApp {
            log: SharedLog,
            dispatcher: Dispatcher,
            tasks: std::sync::Arc<TaskManager>,
        }

        impl ServerApp for DrainedApp {
            fn app_name(&self) -> &str {
                "cellapp"
            }

            fn init(&mut self, _args: &[String]) -> bool {
                self.log.lock().expect("log").push("init");

                let log = Arc::clone(&self.log);
                let tasks = Arc::clone(&self.tasks);
                let passes = Cell::new(0);
                let queued = Cell::new(false);

                // Pass 1: submit the work that is still running when the
                // shutdown starts.
                self.dispatcher
                    .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                        move || {
                            if queued.get() {
                                return;
                            }
                            queued.set(true);
                            let task = Arc::new(DrainingTask {
                                work: "bg work",
                                cleanup: "bg cleanup",
                                log: Arc::clone(&log),
                                me: std::sync::Mutex::new(None),
                            });
                            *task.me.lock().expect("me") = Some(Arc::clone(&task) as TaskPtr);
                            assert_eq!(task.name(), "bg work");
                            tasks.add_background_task(task as TaskPtr);
                        },
                        "queue work",
                    ))));

                // Pass 2: queue one more, then break.
                let log = Arc::clone(&self.log);
                let tasks = Arc::clone(&self.tasks);
                let break_flag = self.dispatcher.break_flag();
                self.dispatcher
                    .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                        move || {
                            passes.set(passes.get() + 1);
                            if passes.get() < 2 {
                                return;
                            }
                            let task = Arc::new(DrainingTask {
                                work: "late work",
                                cleanup: "late cleanup",
                                log: Arc::clone(&log),
                                me: std::sync::Mutex::new(None),
                            });
                            *task.me.lock().expect("me") = Some(Arc::clone(&task) as TaskPtr);
                            tasks.add_background_task(task as TaskPtr);
                            break_flag.set(true);
                        },
                        "queue late work",
                    ))));
                true
            }

            fn run(&mut self) -> bool {
                self.dispatcher.process_until_break();
                true
            }

            fn fini(&mut self) {
                self.log.lock().expect("log").push("fini");
                // `bgTaskManager_.stopAll()`: the queued work finishes and
                // every completion dispatches on this thread.
                self.tasks.drain();
                assert!(!self.tasks.has_tasks());
                assert_eq!(self.tasks.num_running_threads(), 0);
            }

            fn prepare_for_shutdown(&mut self) {
                self.log.lock().expect("log").push("prepareForShutdown");
            }
        }

        let (_r, config, _k) = config_over("drained_pool", &valid_bw_xml());
        let log: SharedLog = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut app = DrainedApp {
            log: Arc::clone(&log),
            dispatcher: Dispatcher::new(),
            tasks: Arc::new(TaskManager::new()),
        };
        app.tasks.start_threads(1);

        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_SUCCESS);

        // The interleaving of the worker and the shutdown is not
        // pinned, but the end state is: every phase ran exactly once,
        // the work before its own completion, both completions after
        // "fini" (they dispatch inside the drain), and the drain before
        // prepareForShutdown.
        let entries: Vec<&'static str> = log.lock().expect("log").clone();
        assert_eq!(entries.first().copied(), Some("init"));
        assert_eq!(entries.last().copied(), Some("prepareForShutdown"));
        for (work, cleanup) in [("bg work", "bg cleanup"), ("late work", "late cleanup")] {
            let work_at = entries.iter().position(|entry| *entry == work);
            let cleanup_at = entries.iter().position(|entry| *entry == cleanup);
            assert!(
                work_at.is_some() && cleanup_at.is_some(),
                "missing phase in {entries:?}"
            );
            assert!(work_at.unwrap() < cleanup_at.unwrap());
            assert!(
                entries.iter().filter(|entry| **entry == cleanup).count() == 1,
                "{cleanup} dispatched more than once in {entries:?}"
            );
        }
        let fini_at = entries
            .iter()
            .position(|entry| *entry == "fini")
            .expect("fini");
        assert!(
            fini_at
                < entries
                    .iter()
                    .position(|entry| *entry == "bg cleanup")
                    .unwrap()
        );
        assert!(
            fini_at
                < entries
                    .iter()
                    .position(|entry| *entry == "late cleanup")
                    .unwrap()
        );
    }

    /// Acceptance (staged half): a control event carrying
    /// `SHUTDOWN_INFORM` records the stage and the deadline tick on the
    /// app's shutdown state, then breaks processing -- the app leaves the
    /// loop in a shutting-down stage and exits 0.
    #[test]
    fn a_control_event_walks_the_staged_shutdown() {
        struct StagedApp {
            state: Rc<RefCell<ShutdownState>>,
            dispatcher: Dispatcher,
            log: Rc<RefCell<Vec<String>>>,
        }

        impl ServerApp for StagedApp {
            fn app_name(&self) -> &str {
                "cellapp"
            }

            fn init(&mut self, _args: &[String]) -> bool {
                self.log.borrow_mut().push("init".to_string());

                // The BW-02 stand-in for the controlledShutDown message
                // handler.
                let log = Rc::clone(&self.log);
                let state = Rc::clone(&self.state);
                let break_flag = self.dispatcher.break_flag();
                self.dispatcher
                    .add_frequent_task(Rc::new(RefCell::new(FrequentTaskFn(
                        move || {
                            log.borrow_mut().push(format!(
                                "controlledShutDown {}",
                                shut_down_stage_to_string(ShutDownStage::Inform as i32)
                            ));
                            state
                                .borrow_mut()
                                .handle_controlled_shut_down(ShutDownStage::Inform, 120);
                            log.borrow_mut()
                                .push("ServerApp::shutDown: shutting down".to_string());
                            break_flag.set(true);
                        },
                        "control event",
                    ))));
                true
            }

            fn run(&mut self) -> bool {
                self.dispatcher.process_until_break();
                true
            }
        }

        let (_r, config, _k) = config_over("staged_ctl", &valid_bw_xml());
        let log = Rc::new(RefCell::new(Vec::new()));
        let state = Rc::new(RefCell::new(ShutdownState::new()));
        let mut app = StagedApp {
            state: Rc::clone(&state),
            dispatcher: Dispatcher::new(),
            log: Rc::clone(&log),
        };

        assert_eq!(do_bw_main_t(&mut app, &config, &[]), EXIT_SUCCESS);
        assert_eq!(
            *log.borrow(),
            [
                "init",
                "controlledShutDown SHUTDOWN_INFORM",
                "ServerApp::shutDown: shutting down",
            ]
        );
        let state = state.borrow();
        assert_eq!(state.stage(), ShutDownStage::Inform);
        assert_eq!(state.shut_down_time(), 120);
        assert!(state.is_shutting_down());
    }
}
