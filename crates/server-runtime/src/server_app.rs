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
//! - `runApp` always calls `fini()`, init-success or not.

use crate::config::BwConfig;
use resources::bwresource::BwResource;

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
        push_line(&mut lines, "gameUpdateHertz", &self.update_hertz.to_string(), "10");
        push_line(&mut lines, "personality", &self.personality, DEFAULT_PERSONALITY_NAME);
        push_line(&mut lines, "serverMode", &self.server_mode, DEFAULT_SERVER_MODE);
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
fn read_option_string(
    config: &BwConfig,
    path: &str,
    value: &mut String,
    unread: &mut Vec<String>,
) {
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
/// (`runApp`'s calls into the app). Signal handling (`BW-01-04-04`) and
/// the Mercury interface (`BW-02`) join later.
pub trait ServerApp {
    /// `getAppName`.
    fn app_name(&self) -> &str;

    /// `ServerApp::init`: returns false to abort the startup.
    fn init(&mut self, args: &[String]) -> bool;

    /// `ServerApp::run`.
    fn run(&mut self) -> bool;

    /// `ServerApp::fini` (a no-op in the base class).
    fn fini(&mut self) {}
}

/// `ServerApp::runApp`: init gates run; fini always happens; the bool is
/// `doBWMainT`'s success/failure input. `prepareForShutdown` belongs to
/// the Mercury interface (BW-02).
pub fn run_app(app: &mut dyn ServerApp, args: &[String]) -> bool {
    // stampsPerSecond() -- clock warm-up, nothing observable here.
    let mut result = false;

    if app.init(args) {
        result = app.run();
    } else {
        eprintln!("Failed to initialise {}", app.app_name());
    }

    app.fini();
    result
}

/// `doBWMainT` without the Mercury wiring (dispatcher/interface arrive
/// with BW-02): config options first -- their failure is the **-1**
/// return -- then the app lifecycle, whose bool maps to 0/1.
pub fn do_bw_main_t(
    app: &mut dyn ServerApp,
    config: &BwConfig,
    args: &[String],
) -> i32 {
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
    do_bw_main_t(app, config, args)
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
        assert!(lines.iter().any(|l| l.starts_with("  gameUpdateHertz")
            && l.contains("(default: 10)")));
        // Only the five options absent from the fixture warn.
        assert_eq!(
            app_config.unread_options,
            ["serverMode"]
        );
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
}
