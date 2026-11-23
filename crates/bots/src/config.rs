//! `BotsConfig` (`server/tools/bots/bots_config.{hpp,cpp}`): the
//! `bots/`-prefixed option set the tool reads from the bw.xml chain.
//!
//! Reference quirks preserved:
//! - the tool boots through `ServerAppConfig::init( BotsConfig::postInit )`
//!   (`bwservice.hpp:93` with `SERVER_APP::Config = BotsConfig`), so the
//!   *derived* option is the tool's own transport and the base config's
//!   server-mode validation does not run here;
//! - a value the chain does not carry logs a per-option warning and
//!   keeps its default (`ServerAppOptionIniterT::init`); a value the
//!   chain carries but cannot parse falls back through the same `as`
//!   conversion and *counts as read* -- no warning, no unread record;
//! - the derived option has no config path, so `printAll` skips it
//!   (`configPath_[0] == '\0'` early return);
//! - `BotsConfig::postInit` returns true unconditionally: the `-1`
//!   "Failed to initialise configuration options" pipeline arm exists
//!   but is unreachable for this tool;
//! - `uint16` options promote through `int` and narrow on assignment
//!   (`BUILTIN_TEMPLATES_PROMOTE( uint16, int, asInt, setInt )`), so a
//!   wide value wraps;
//! - `bots/shouldLog` is not an option static: `main` reads it straight
//!   off the chain with a `BWConfig::get` default
//!   (`bots/main.cpp:31`) before anything else runs.

use server_runtime::config::BwConfig;

/// `ConnectionTransport` (`lib/connection/connection_transport.hpp:9-14`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(i32)]
pub enum ConnectionTransport {
    /// `CONNECTION_TRANSPORT_UDP`.
    #[default]
    Udp = 0,
    /// `CONNECTION_TRANSPORT_TCP`.
    Tcp = 1,
    /// `CONNECTION_TRANSPORT_WEBSOCKETS`.
    Websockets = 2,
}

/// The `BotsConfig` option set (`BW_OPTION` statics), read from the
/// bw.xml chain on `init`.
#[derive(Debug, Clone, PartialEq)]
pub struct BotsConfig {
    /// `bots/username` ("Bot").
    pub username: String,
    /// `bots/password` ("").
    pub password: String,
    /// `bots/serverName` ("").
    pub server_name: String,
    /// `bots/tag` ("Default").
    pub tag: String,
    /// `bots/shouldUseTCP` (false).
    pub should_use_tcp: bool,
    /// `bots/shouldUseWebSockets` (false).
    pub should_use_web_sockets: bool,
    /// `transport` -- derived in `postInit`, no config path of its own
    /// (`DERIVED_BW_OPTION`).
    pub transport: ConnectionTransport,
    /// `bots/port` (0) -- a `uint16` that reads through `int`.
    pub port: u16,
    /// `bots/shouldUseRandomName` (true).
    pub should_use_random_name: bool,
    /// `bots/shouldUseScripts` (true).
    pub should_use_scripts: bool,
    /// `bots/standinEntity` ("DefaultEntity").
    pub standin_entity: String,
    /// `bots/controllerType` ("Patrol").
    pub controller_type: String,
    /// `bots/controllerData` ("server/bots/test.bwp").
    pub controller_data: String,
    /// `bots/publicKey` ("").
    pub public_key: String,
    /// `bots/logOnRetryPeriod` (0.0).
    pub log_on_retry_period: f32,
    /// `bots/shouldListenForGeometryMappings` (false) --
    /// `BW_OPTION_RO`.
    pub should_listen_for_geometry_mappings: bool,
    /// `bots/chunkLoadingPeriod` (0.02) -- `BW_OPTION_RO`.
    pub chunk_loading_period: f32,

    /// `CONFIG_INFO_MSG` capture of `ServerAppOptionIniter::printAll` --
    /// one line per declared option (the derived `transport` has no
    /// config path and prints nothing).
    pub print_lines: Vec<String>,
    /// Option paths that were not read from the chain (each got a
    /// `CONFIG_WARNING_MSG` and kept its default).
    pub unread_options: Vec<String>,
}

impl Default for BotsConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl BotsConfig {
    /// Every static at its C++ initialiser value
    /// (`bots_config.cpp:17-33`), the derived transport at its
    /// zero-initialised `int()` value until `postInit` sets it.
    pub fn new() -> Self {
        Self {
            username: "Bot".to_string(),
            password: String::new(),
            server_name: String::new(),
            tag: "Default".to_string(),
            should_use_tcp: false,
            should_use_web_sockets: false,
            transport: ConnectionTransport::Udp,
            port: 0,
            should_use_random_name: true,
            should_use_scripts: true,
            standin_entity: "DefaultEntity".to_string(),
            controller_type: "Patrol".to_string(),
            controller_data: "server/bots/test.bwp".to_string(),
            public_key: String::new(),
            log_on_retry_period: 0.0,
            should_listen_for_geometry_mappings: false,
            chunk_loading_period: 0.02,
            print_lines: Vec::new(),
            unread_options: Vec::new(),
        }
    }

    /// `ServerAppConfig::init( BotsConfig::postInit )` for this tool:
    /// read every declared option from the chain in declaration order
    /// (missing ones are recorded and keep their default), run the
    /// tool's post-init hook, then print the option table. The base
    /// config's own statics are the shared pipeline's read
    /// (`do_bw_main_t`); only the failure of *this* post-init can fail
    /// the startup, and it cannot.
    pub fn init(&mut self, config: &BwConfig) -> bool {
        let mut unread = Vec::new();

        read_option_string(config, "bots/username", &mut self.username, &mut unread);
        read_option_string(config, "bots/password", &mut self.password, &mut unread);
        read_option_string(
            config,
            "bots/serverName",
            &mut self.server_name,
            &mut unread,
        );
        read_option_string(config, "bots/tag", &mut self.tag, &mut unread);
        read_option_bool(
            config,
            "bots/shouldUseTCP",
            &mut self.should_use_tcp,
            &mut unread,
        );
        read_option_bool(
            config,
            "bots/shouldUseWebSockets",
            &mut self.should_use_web_sockets,
            &mut unread,
        );
        // `DERIVED_BW_OPTION( int, transport )` has an empty config path
        // (`server_app_option_macros.hpp:41-43`) and reads nothing here.
        read_option_u16(config, "bots/port", &mut self.port, &mut unread);
        read_option_bool(
            config,
            "bots/shouldUseRandomName",
            &mut self.should_use_random_name,
            &mut unread,
        );
        read_option_bool(
            config,
            "bots/shouldUseScripts",
            &mut self.should_use_scripts,
            &mut unread,
        );
        read_option_string(
            config,
            "bots/standinEntity",
            &mut self.standin_entity,
            &mut unread,
        );
        read_option_string(
            config,
            "bots/controllerType",
            &mut self.controller_type,
            &mut unread,
        );
        read_option_string(
            config,
            "bots/controllerData",
            &mut self.controller_data,
            &mut unread,
        );
        read_option_string(config, "bots/publicKey", &mut self.public_key, &mut unread);
        read_option_f32(
            config,
            "bots/logOnRetryPeriod",
            &mut self.log_on_retry_period,
            &mut unread,
        );
        read_option_bool(
            config,
            "bots/shouldListenForGeometryMappings",
            &mut self.should_listen_for_geometry_mappings,
            &mut unread,
        );
        read_option_f32(
            config,
            "bots/chunkLoadingPeriod",
            &mut self.chunk_loading_period,
            &mut unread,
        );
        self.unread_options = unread;

        let result = self.post_init();

        self.print_all();
        result
    }

    /// `BotsConfig::postInit` (`bots_config.cpp:36-50`): the transport
    /// follows the TCP and WebSocket switches, and the tool's config
    /// stage never fails -- the hook's `return true` is unconditional.
    pub fn post_init(&mut self) -> bool {
        self.transport = if self.should_use_tcp {
            if self.should_use_web_sockets {
                ConnectionTransport::Websockets
            } else {
                ConnectionTransport::Tcp
            }
        } else {
            ConnectionTransport::Udp
        };

        true
    }

    /// `ServerAppOptionIniter::printAll` capture. The reference prints
    /// the full config path (`configPath_`) padded to 34 columns,
    /// values equal to the default without a default suffix; the
    /// derived `transport` prints nothing (`configPath_[0] == '\0'`).
    fn print_all(&mut self) {
        let mut lines = Vec::new();
        push_line(&mut lines, "bots/username", &self.username, "Bot");
        push_line(&mut lines, "bots/password", &self.password, "");
        push_line(&mut lines, "bots/serverName", &self.server_name, "");
        push_line(&mut lines, "bots/tag", &self.tag, "Default");
        push_line(
            &mut lines,
            "bots/shouldUseTCP",
            &self.should_use_tcp.to_string(),
            "false",
        );
        push_line(
            &mut lines,
            "bots/shouldUseWebSockets",
            &self.should_use_web_sockets.to_string(),
            "false",
        );
        push_line(&mut lines, "bots/port", &self.port.to_string(), "0");
        push_line(
            &mut lines,
            "bots/shouldUseRandomName",
            &self.should_use_random_name.to_string(),
            "true",
        );
        push_line(
            &mut lines,
            "bots/shouldUseScripts",
            &self.should_use_scripts.to_string(),
            "true",
        );
        push_line(
            &mut lines,
            "bots/standinEntity",
            &self.standin_entity,
            "DefaultEntity",
        );
        push_line(
            &mut lines,
            "bots/controllerType",
            &self.controller_type,
            "Patrol",
        );
        push_line(
            &mut lines,
            "bots/controllerData",
            &self.controller_data,
            "server/bots/test.bwp",
        );
        push_line(&mut lines, "bots/publicKey", &self.public_key, "");
        push_line(
            &mut lines,
            "bots/logOnRetryPeriod",
            &format!("{:.6}", self.log_on_retry_period),
            "0.000000",
        );
        push_line(
            &mut lines,
            "bots/shouldListenForGeometryMappings",
            &self.should_listen_for_geometry_mappings.to_string(),
            "false",
        );
        push_line(
            &mut lines,
            "bots/chunkLoadingPeriod",
            &format!("{:.6}", self.chunk_loading_period),
            "0.020000",
        );
        self.print_lines = lines;
    }
}

/// `BWConfig::get( "bots/shouldLog", true )` -- the boot flag `main`
/// reads straight off the chain (`bots/main.cpp:31`), before the
/// option machinery runs. A miss keeps the default with no warning.
pub fn read_should_log(config: &BwConfig) -> bool {
    config.get_bool("bots/shouldLog", true)
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
// chain (recording a miss for the per-option warning), default
// otherwise.
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

/// The `uint16` flavour: `as< uint16 >` promotes through `int`
/// (`BUILTIN_TEMPLATES_PROMOTE( uint16, int, asInt, setInt )`) and the
/// C++ assignment narrows, so a wide value wraps the same way.
fn read_option_u16(config: &BwConfig, path: &str, value: &mut u16, unread: &mut Vec<String>) {
    let mut promoted = i32::from(*value);
    if config.update_int(path, &mut promoted) {
        *value = promoted as u16;
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

#[cfg(test)]
mod tests {
    use super::*;
    use resources::bwresource::BwResource;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A scratch resource root for one test.
    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "boundless_bots_{}_{}_{}",
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
    fn config_over(tag: &str, body: &str) -> BwConfig {
        let root = temp_root(tag);
        write(&format!("{root}/server/bw.xml"), body);
        let resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);
        config
    }

    /// Acceptance: the defaults are the reference's initialiser values
    /// (`bots_config.cpp:17-33`), all sixteen of them, the derived
    /// transport sitting at its zero value until `postInit`.
    #[test]
    fn the_defaults_are_the_reference_initialisers() {
        let config = BotsConfig::new();
        assert_eq!(BotsConfig::default().username, "Bot");
        assert_eq!(config.username, "Bot");
        assert_eq!(config.password, "");
        assert_eq!(config.server_name, "");
        assert_eq!(config.tag, "Default");
        assert!(!config.should_use_tcp);
        assert!(!config.should_use_web_sockets);
        assert_eq!(config.transport, ConnectionTransport::Udp);
        assert_eq!(config.port, 0);
        assert!(config.should_use_random_name);
        assert!(config.should_use_scripts);
        assert_eq!(config.standin_entity, "DefaultEntity");
        assert_eq!(config.controller_type, "Patrol");
        assert_eq!(config.controller_data, "server/bots/test.bwp");
        assert_eq!(config.public_key, "");
        assert_eq!(config.log_on_retry_period, 0.0);
        assert!(!config.should_listen_for_geometry_mappings);
        assert_eq!(config.chunk_loading_period, 0.02);
        assert!(config.print_lines.is_empty());
        assert!(config.unread_options.is_empty());
    }

    /// Acceptance: a chain without the `bots/` sections warns per
    /// option and keeps every default -- the failure state of a
    /// missing configuration is the quiet fallback, not a failed
    /// startup (`bots_config.cpp:36-50` cannot fail; the per-option
    /// warning is `ServerAppOptionIniterT::init`'s).
    #[test]
    fn a_chain_without_bots_sections_keeps_every_default() {
        let chain = config_over(
            "defaults",
            "<root><serverMode>standalone</serverMode></root>",
        );
        let mut config = BotsConfig::new();
        assert!(config.init(&chain), "the tool's post-init cannot fail");
        let defaults = BotsConfig::new();
        assert_eq!(config.username, defaults.username);
        assert_eq!(config.password, defaults.password);
        assert_eq!(config.server_name, defaults.server_name);
        assert_eq!(config.tag, defaults.tag);
        assert_eq!(config.should_use_tcp, defaults.should_use_tcp);
        assert_eq!(
            config.should_use_web_sockets,
            defaults.should_use_web_sockets
        );
        assert_eq!(config.transport, defaults.transport);
        assert_eq!(config.port, defaults.port);
        assert_eq!(
            config.should_use_random_name,
            defaults.should_use_random_name
        );
        assert_eq!(config.should_use_scripts, defaults.should_use_scripts);
        assert_eq!(config.standin_entity, defaults.standin_entity);
        assert_eq!(config.controller_type, defaults.controller_type);
        assert_eq!(config.controller_data, defaults.controller_data);
        assert_eq!(config.public_key, defaults.public_key);
        assert_eq!(config.log_on_retry_period, defaults.log_on_retry_period);
        assert_eq!(
            config.should_listen_for_geometry_mappings,
            defaults.should_listen_for_geometry_mappings
        );
        assert_eq!(config.chunk_loading_period, defaults.chunk_loading_period);
        // printAll ran over the full option set; every value is the
        // default, so no line carries the `(default: ...)` suffix.
        assert_eq!(config.print_lines.len(), 16);
        assert!(
            config
                .print_lines
                .iter()
                .all(|line| !line.contains("(default:"))
        );
        assert_eq!(
            config.unread_options,
            [
                "bots/username",
                "bots/password",
                "bots/serverName",
                "bots/tag",
                "bots/shouldUseTCP",
                "bots/shouldUseWebSockets",
                "bots/port",
                "bots/shouldUseRandomName",
                "bots/shouldUseScripts",
                "bots/standinEntity",
                "bots/controllerType",
                "bots/controllerData",
                "bots/publicKey",
                "bots/logOnRetryPeriod",
                "bots/shouldListenForGeometryMappings",
                "bots/chunkLoadingPeriod",
            ],
            "every declared path is warned, the derived transport is not"
        );
    }

    /// Acceptance: the chain overrides exactly the options it carries,
    /// in declaration order, and the print table names the full config
    /// paths with the `(default: ...)` suffix on the changed ones.
    #[test]
    fn the_chain_overrides_the_declared_options() {
        let chain = config_over(
            "overrides",
            "<root><bots>\
             <username>Swarm</username>\
             <port>8000</port>\
             <logOnRetryPeriod>1.5</logOnRetryPeriod>\
             <shouldListenForGeometryMappings>true</shouldListenForGeometryMappings>\
             </bots></root>",
        );
        let mut config = BotsConfig::new();
        assert!(config.init(&chain));

        assert_eq!(config.username, "Swarm");
        assert_eq!(config.port, 8000);
        assert_eq!(config.log_on_retry_period, 1.5);
        assert!(config.should_listen_for_geometry_mappings);
        // The untouched options hold.
        assert_eq!(config.tag, "Default");
        assert_eq!(config.controller_data, "server/bots/test.bwp");
        assert_eq!(
            config.unread_options,
            [
                "bots/password",
                "bots/serverName",
                "bots/tag",
                "bots/shouldUseTCP",
                "bots/shouldUseWebSockets",
                "bots/shouldUseRandomName",
                "bots/shouldUseScripts",
                "bots/standinEntity",
                "bots/controllerType",
                "bots/controllerData",
                "bots/publicKey",
                "bots/chunkLoadingPeriod",
            ]
        );

        let line = &config.print_lines[0];
        assert_eq!(
            line,
            &format!("  {:<34} = Swarm (default: Bot)", "bots/username")
        );
        // The unchanged tag prints without the suffix, the derived
        // transport prints nothing at all.
        assert!(
            config
                .print_lines
                .contains(&format!("  {:<34} = Default", "bots/tag"))
        );
        assert_eq!(config.print_lines.len(), 16);
        assert!(
            !config
                .print_lines
                .iter()
                .any(|line| line.contains("transport"))
        );
    }

    /// Acceptance: `postInit` derives the transport from the two
    /// switches (`bots_config.cpp:38-47`) -- TCP alone is TCP, TCP with
    /// WebSockets is WebSockets, and without TCP everything is UDP no
    /// matter the WebSocket switch.
    #[test]
    fn the_transport_derives_from_the_switches() {
        let cases: [(&str, ConnectionTransport); 4] = [
            (
                "<root><bots><shouldUseTCP>true</shouldUseTCP></bots></root>",
                ConnectionTransport::Tcp,
            ),
            (
                "<root><bots><shouldUseTCP>true</shouldUseTCP>\
                 <shouldUseWebSockets>true</shouldUseWebSockets></bots></root>",
                ConnectionTransport::Websockets,
            ),
            (
                "<root><bots><shouldUseWebSockets>true</shouldUseWebSockets></bots></root>",
                ConnectionTransport::Udp,
            ),
            ("<root></root>", ConnectionTransport::Udp),
        ];
        for (body, transport) in cases {
            let chain = config_over("transport", body);
            let mut config = BotsConfig::new();
            assert!(config.init(&chain));
            assert_eq!(config.transport, transport, "{body}");
        }
    }

    /// Acceptance: a carried-but-unparseable value falls back through
    /// the same `as` conversion and *counts as read* -- no unread
    /// record, no warning, default kept (`BWConfig::update` returns
    /// true on the section alone). The bool conversion only answers to
    /// true/false, so `1` falls back too.
    #[test]
    fn a_garbage_value_falls_back_without_a_warning() {
        let chain = config_over(
            "garbage",
            "<root><bots><port>abc</port><shouldUseTCP>1</shouldUseTCP>\
             <shouldUseWebSockets>true</shouldUseWebSockets></bots></root>",
        );
        let mut config = BotsConfig::new();
        assert!(config.init(&chain));
        assert_eq!(config.port, 0, "abc falls back to the carried default");
        assert!(!config.should_use_tcp, "1 is not a bool answer");
        assert!(config.should_use_web_sockets);
        assert!(
            !config
                .unread_options
                .iter()
                .any(|path| path == "bots/port" || path == "bots/shouldUseTCP")
        );
        // The WebSocket switch carried, so the transport still derives
        // -- from the flags that actually parsed.
        assert_eq!(config.transport, ConnectionTransport::Udp);
    }

    /// Acceptance: a wide `uint16` wraps -- `as< uint16 >` promotes
    /// through `int` and the C++ assignment narrows
    /// (`BUILTIN_TEMPLATES_PROMOTE( uint16, int, asInt, setInt )`).
    #[test]
    fn a_wide_port_narrows_like_the_reference() {
        let chain = config_over("wideport", "<root><bots><port>70000</port></bots></root>");
        let mut config = BotsConfig::new();
        assert!(config.init(&chain));
        assert_eq!(config.port, 70000u32 as u16);
    }

    /// Acceptance: `bots/shouldLog` is not an option static -- `main`
    /// reads it with a plain `BWConfig::get` default
    /// (`bots/main.cpp:31`), so a miss keeps true with no warning and
    /// an explicit false lands.
    #[test]
    fn should_log_reads_off_the_chain_with_its_default() {
        let plain = config_over("shouldlog_default", "<root></root>");
        assert!(read_should_log(&plain));
        assert!(plain.get_section("bots/shouldLog").is_none());

        let over = config_over(
            "shouldlog_off",
            "<root><bots><shouldLog>false</shouldLog></bots></root>",
        );
        assert!(!read_should_log(&over));
    }

    /// Acceptance: the `-1` config-failure pipeline arm
    /// (`bwservice.hpp:95-97`) is unreachable for this tool --
    /// `BotsConfig::postInit` returns true on every input
    /// (`bots_config.cpp:49`).
    #[test]
    fn the_config_stage_never_fails_the_tool() {
        let garbage = config_over(
            "never_fails",
            "<root><bots><port>abc</port><username></username></bots></root>",
        );
        let mut config = BotsConfig::new();
        assert!(config.init(&garbage));

        // The hook itself, driven directly, derives and succeeds.
        let mut direct = BotsConfig::new();
        direct.should_use_tcp = true;
        assert!(direct.post_init());
        assert_eq!(direct.transport, ConnectionTransport::Tcp);
    }
}
