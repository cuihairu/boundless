//! `server/tools/bots/bots_config.{hpp,cpp}`: the Bots tool's
//! configuration, read through the `ServerAppOption` machinery
//! (`lib/server/server_app_option.{hpp,cpp}`) the way `BWConfig::update`
//! reads the bw.xml chain.
//!
//! The reference's options are statics registered at start-up by the
//! `BW_OPTION` macros with `BW_CONFIG_PREFIX "bots/"`; the port keeps
//! them in declaration order inside [`BotsConfig`], so `init` walks them
//! in the same order the C++ initers fire and `print_all` lists them the
//! same way. The derived `transport` option carries an empty config path
//! and prints nothing, exactly as the reference's `DERIVED_BW_OPTION`
//! does.

use resources::datasection::DataSection;
use watcher::WatcherValue;

use crate::bot::transport_from_config;

/// `Watcher::Mode` (cstdmf/watcher.hpp): the two modes the option macros
/// hand out -- plain options are watched read-write, `BW_OPTION_RO` and
/// derived options read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherMode {
    /// `Watcher::WT_READ_WRITE`, the default.
    ReadWrite,
    /// `Watcher::WT_READ_ONLY`.
    ReadOnly,
}

/// The typed value of an option: the five types the Bots' option list
/// uses (`BW::string`, `bool`, `int`, `uint16`, `float`).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `BW::string`.
    Str(String),
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i32),
    /// `uint16` (the `port` option).
    Uint(u16),
    /// `float`.
    Float(f32),
}

impl Value {
    /// `watcherValueToString` per type (`watcher.hpp:686-705`): strings
    /// verbatim, `bool` as `true`/`false`, the rest through the numeric
    /// `ostream` face the watcher crate already ports.
    pub fn to_cpp_string(&self) -> String {
        match self {
            Value::Str(value) => value.clone(),
            Value::Bool(value) => value.to_cpp_string(),
            Value::Int(value) => value.to_cpp_string(),
            Value::Uint(value) => (*value as u32).to_cpp_string(),
            Value::Float(value) => value.to_cpp_string(),
        }
    }

    /// `print`'s equality: floats compare through `almostEqual`'s 0.0004
    /// epsilon (`stdmf.hpp:606`), the rest compare exactly
    /// (`server_app_option.hpp:91`, `server_app_option.cpp:158`).
    pub fn same_for_print(&self, default: &Value) -> bool {
        match (self, default) {
            (Value::Float(a), Value::Float(b)) => (a - b).abs() < 0.0004,
            _ => self == default,
        }
    }

    /// `BWConfig::update`'s per-type read into the current value: a
    /// present section replaces the value; a present-but-unparseable
    /// section still counts as read and falls back to the current one,
    /// because the reference's `DataSection::asBool`/`asInt` family take
    /// the incoming value as their parse default and report success
    /// either way.
    fn update_from(&mut self, section: &DataSection) {
        match self {
            Value::Str(value) => *value = section.as_string().to_string(),
            Value::Bool(value) => *value = section.as_bool(*value),
            Value::Int(value) => *value = section.as_int(*value),
            Value::Uint(value) => *value = section.as_uint(*value as u32) as u16,
            Value::Float(value) => *value = section.as_float(*value),
        }
    }
}

/// One configured option: the `ServerAppOption<TYPE>` static plus the
/// `ServerAppOptionIniterT` that reads and prints it.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionEntry {
    /// The option's name, e.g. `username`.
    pub name: &'static str,
    /// `configPath_`: the bw.xml chain path, `bots/`-prefixed; empty for
    /// derived options.
    pub config_path: String,
    /// `watcherPath_`: the `config/`-prefixed watcher entry.
    pub watcher_path: String,
    /// `watcherMode_`.
    pub mode: WatcherMode,
    /// `defaultValue_`, the macro's literal.
    pub default_value: Value,
    /// `value_`, the live value.
    pub value: Value,
}

impl OptionEntry {
    /// `ServerAppOptionIniterT::init` (`server_app_option.hpp:50-64`):
    /// read the option from the chain; a path that opens nowhere warns
    /// and keeps the default. The reference emits `CONFIG_WARNING_MSG`
    /// and registers the watcher; the port hands the warning line to the
    /// caller's sink so a test can lock its text.
    fn init_from(&mut self, sections: &[DataSection], warnings: &mut Vec<String>) {
        if self.config_path.is_empty() {
            // `DERIVED_BW_OPTION`: no bw.xml path to read.
            return;
        }
        match find_section(sections, &self.config_path) {
            Some(section) => self.value.update_from(section),
            None => warnings.push(format!(
                "ServerAppConfig::init: {} not read from bw.xml chain. Using default ({})",
                self.config_path,
                self.default_value.to_cpp_string()
            )),
        }
    }

    /// `ServerAppOptionIniterT::print` (`server_app_option.hpp:81-99`):
    /// `  %-34s = <value>`, with a ` (default: <value>)` suffix when the
    /// value moved off its default. An option with an empty config path
    /// (the derived `transport`) prints nothing
    /// (`server_app_option.hpp:85-89`). The float arm compares through
    /// `almostEqual` (`server_app_option.cpp:153-178`).
    fn print_line(&self) -> String {
        if self.config_path.is_empty() {
            return String::new();
        }
        let name = &self.config_path;
        let value = self.value.to_cpp_string();
        if self.value.same_for_print(&self.default_value) {
            format!("  {name:<34} = {value}\n")
        } else {
            format!(
                "  {name:<34} = {value} (default: {})\n",
                self.default_value.to_cpp_string()
            )
        }
    }
}

/// `BotsConfig` (`bots_config.hpp`): the seventeen options with the
/// defaults `bots_config.cpp:26-48` spells out.
#[derive(Debug, Clone, PartialEq)]
pub struct BotsConfig {
    /// The options in the reference's declaration order.
    pub options: Vec<OptionEntry>,
}

impl Default for BotsConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl BotsConfig {
    /// The option defaults of `bots_config.cpp:26-48`, verbatim, in the
    /// order the reference's statics are declared. `transport` is the
    /// `DERIVED_BW_OPTION` (`bots_config.cpp:36`): zero-initialised,
    /// empty config path, read-only watcher.
    pub fn new() -> Self {
        fn plain(name: &'static str, default: Value) -> OptionEntry {
            OptionEntry {
                name,
                config_path: format!("bots/{name}"),
                watcher_path: format!("config/{name}"),
                mode: WatcherMode::ReadWrite,
                default_value: default.clone(),
                value: default,
            }
        }
        fn read_only(name: &'static str, default: Value) -> OptionEntry {
            let mut option = plain(name, default);
            option.mode = WatcherMode::ReadOnly;
            option
        }
        Self {
            options: vec![
                plain("username", Value::Str("Bot".to_string())),
                plain("password", Value::Str(String::new())),
                plain("serverName", Value::Str(String::new())),
                plain("tag", Value::Str("Default".to_string())),
                plain("shouldUseTCP", Value::Bool(false)),
                plain("shouldUseWebSockets", Value::Bool(false)),
                OptionEntry {
                    name: "transport",
                    config_path: String::new(),
                    watcher_path: "config/transport".to_string(),
                    mode: WatcherMode::ReadOnly,
                    default_value: Value::Int(0),
                    value: Value::Int(0),
                },
                plain("port", Value::Uint(0)),
                plain("shouldUseRandomName", Value::Bool(true)),
                plain("shouldUseScripts", Value::Bool(true)),
                plain("standinEntity", Value::Str("DefaultEntity".to_string())),
                plain("controllerType", Value::Str("Patrol".to_string())),
                plain(
                    "controllerData",
                    Value::Str("server/bots/test.bwp".to_string()),
                ),
                plain("publicKey", Value::Str(String::new())),
                plain("logOnRetryPeriod", Value::Float(0.0)),
                read_only("shouldListenForGeometryMappings", Value::Bool(false)),
                read_only("chunkLoadingPeriod", Value::Float(0.02)),
            ],
        }
    }

    /// The named option, for watcher wiring and tests.
    pub fn option(&self, name: &str) -> Option<&OptionEntry> {
        self.options.iter().find(|option| option.name == name)
    }

    fn value_of(&self, name: &str) -> &Value {
        let found = self
            .options
            .iter()
            .find(|option| option.name == name)
            .map(|option| &option.value);
        found.expect("an option by its declared name")
    }

    fn string(&self, name: &str) -> String {
        match self.value_of(name) {
            Value::Str(value) => value.clone(),
            _ => String::new(),
        }
    }

    fn boolean(&self, name: &str) -> bool {
        match self.value_of(name) {
            Value::Bool(value) => *value,
            _ => false,
        }
    }

    /// `username()`, default `"Bot"`.
    pub fn username(&self) -> String {
        self.string("username")
    }

    /// `password()`, default `""`.
    pub fn password(&self) -> String {
        self.string("password")
    }

    /// `serverName()`, default `""`.
    pub fn server_name(&self) -> String {
        self.string("serverName")
    }

    /// `tag()`, default `"Default"`.
    pub fn tag(&self) -> String {
        self.string("tag")
    }

    /// `shouldUseTCP()`, default false.
    pub fn should_use_tcp(&self) -> bool {
        self.boolean("shouldUseTCP")
    }

    /// `shouldUseWebSockets()`, default false.
    pub fn should_use_web_sockets(&self) -> bool {
        self.boolean("shouldUseWebSockets")
    }

    /// `transport()`, the derived value
    /// (`CONNECTION_TRANSPORT_UDP`/`TCP`/`WEBSOCKETS` as 0/1/2,
    /// connection_transport.hpp:11-13).
    pub fn transport(&self) -> i32 {
        match self.value_of("transport") {
            Value::Int(value) => *value,
            _ => 0,
        }
    }

    /// `port()`, default 0.
    pub fn port(&self) -> u16 {
        match self.value_of("port") {
            Value::Uint(value) => *value,
            _ => 0,
        }
    }

    /// `shouldUseRandomName()`, default true.
    pub fn should_use_random_name(&self) -> bool {
        self.boolean("shouldUseRandomName")
    }

    /// `shouldUseScripts()`, default true.
    pub fn should_use_scripts(&self) -> bool {
        self.boolean("shouldUseScripts")
    }

    /// `standinEntity()`, default `"DefaultEntity"`.
    pub fn standin_entity(&self) -> String {
        self.string("standinEntity")
    }

    /// `controllerType()`, default `"Patrol"`.
    pub fn controller_type(&self) -> String {
        self.string("controllerType")
    }

    /// `controllerData()`, default `"server/bots/test.bwp"`.
    pub fn controller_data(&self) -> String {
        self.string("controllerData")
    }

    /// `publicKey()`, default `""`.
    pub fn public_key(&self) -> String {
        self.string("publicKey")
    }

    /// `logOnRetryPeriod()`, default 0.
    pub fn log_on_retry_period(&self) -> f32 {
        match self.value_of("logOnRetryPeriod") {
            Value::Float(value) => *value,
            _ => 0.0,
        }
    }

    /// `shouldListenForGeometryMappings()`, read-only, default false.
    pub fn should_listen_for_geometry_mappings(&self) -> bool {
        self.boolean("shouldListenForGeometryMappings")
    }

    /// `chunkLoadingPeriod()`, read-only, default 0.02.
    pub fn chunk_loading_period(&self) -> f32 {
        match self.value_of("chunkLoadingPeriod") {
            Value::Float(value) => *value,
            _ => 0.02,
        }
    }

    /// `BotsConfig::postInit` (`bots_config.cpp:56-67`): the transport
    /// rides the two flags -- websockets only on top of TCP.
    pub fn post_init(&mut self) -> bool {
        let should_use_tcp = self.should_use_tcp();
        let should_use_web_sockets = self.should_use_web_sockets();
        let derived = transport_from_config(should_use_tcp, should_use_web_sockets) as i32;
        if let Some(option) = self.options.iter_mut().find(|o| o.name == "transport") {
            option.value = Value::Int(derived);
        }
        true
    }

    /// `ServerAppConfig::init( postInitFn )` for the Bots set
    /// (`server_app_config.cpp:121-131`): read every option from the
    /// chain (missing paths warn with the reference's line), derive, and
    /// return the post-init verdict. `print_all` stays the caller's --
    /// the C++ prints and drops its initers right after this.
    pub fn init(&mut self, sections: &[DataSection], warnings: &mut Vec<String>) -> bool {
        for option in &mut self.options {
            option.init_from(sections, warnings);
        }
        self.post_init()
    }

    /// `ServerAppOptionIniter::printAll` (`server_app_option.cpp:62-79`):
    /// the header line, then one padded line per option with a bw.xml
    /// path. The derived `transport` contributes nothing, exactly as the
    /// reference's empty-config-path check does.
    pub fn print_all(&self) -> String {
        let mut out = String::from("Configuration options (from server/bw.xml):\n");
        for option in &self.options {
            out.push_str(&option.print_line());
        }
        out
    }
}

/// `BWConfig::getSection` over a chain: the first chain entry that opens
/// `path` wins -- the most-derived file's value hides the parents'
/// (server-runtime keeps the same rule).
fn find_section<'a>(sections: &'a [DataSection], path: &str) -> Option<&'a DataSection> {
    for section in sections {
        let mut current = section;
        let mut opened = true;
        for part in path.split('/') {
            match current.find_child(part) {
                Some(child) => current = child,
                None => {
                    opened = false;
                    break;
                }
            }
        }
        if opened {
            return Some(current);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::Transport;
    use resources::datasection::parse;

    /// The Bots defaults (`bots_config.cpp:26-48`), all seventeen
    /// options, names and paths and modes included.
    #[test]
    fn the_defaults_match_the_reference_verbatim() {
        let config = BotsConfig::new();
        assert_eq!(config.username(), "Bot");
        assert_eq!(config.password(), "");
        assert_eq!(config.server_name(), "");
        assert_eq!(config.tag(), "Default");
        assert!(!config.should_use_tcp());
        assert!(!config.should_use_web_sockets());
        assert_eq!(config.transport(), 0);
        assert_eq!(config.port(), 0);
        assert!(config.should_use_random_name());
        assert!(config.should_use_scripts());
        assert_eq!(config.standin_entity(), "DefaultEntity");
        assert_eq!(config.controller_type(), "Patrol");
        assert_eq!(config.controller_data(), "server/bots/test.bwp");
        assert_eq!(config.public_key(), "");
        assert_eq!(config.log_on_retry_period(), 0.0);
        assert!(!config.should_listen_for_geometry_mappings());
        assert_eq!(config.chunk_loading_period(), 0.02);

        // The declared order, the paths the macros build, and the modes
        // (`bots_config.cpp:26-48`, `server_app_option_macros.hpp:15-33`).
        let names: Vec<&str> = config.options.iter().map(|o| o.name).collect();
        assert_eq!(
            names,
            vec![
                "username",
                "password",
                "serverName",
                "tag",
                "shouldUseTCP",
                "shouldUseWebSockets",
                "transport",
                "port",
                "shouldUseRandomName",
                "shouldUseScripts",
                "standinEntity",
                "controllerType",
                "controllerData",
                "publicKey",
                "logOnRetryPeriod",
                "shouldListenForGeometryMappings",
                "chunkLoadingPeriod",
            ]
        );
        let username = config.option("username").unwrap();
        assert_eq!(username.config_path, "bots/username");
        assert_eq!(username.watcher_path, "config/username");
        assert_eq!(username.mode, WatcherMode::ReadWrite);
        let geometry = config.option("shouldListenForGeometryMappings").unwrap();
        assert_eq!(geometry.mode, WatcherMode::ReadOnly);
        let chunk = config.option("chunkLoadingPeriod").unwrap();
        assert_eq!(chunk.mode, WatcherMode::ReadOnly);
        let transport = config.option("transport").unwrap();
        assert_eq!(transport.config_path, "");
        assert_eq!(transport.watcher_path, "config/transport");
        assert_eq!(transport.mode, WatcherMode::ReadOnly);
    }

    /// `postInit` derives the transport from the two flags
    /// (`bots_config.cpp:56-67`).
    #[test]
    fn post_init_derives_the_transport_three_ways() {
        let mut config = BotsConfig::new();
        assert!(config.post_init());
        assert_eq!(config.transport(), Transport::Udp as i32);

        let mut config = BotsConfig::new();
        if let Some(option) = config.options.iter_mut().find(|o| o.name == "shouldUseTCP") {
            option.value = Value::Bool(true);
        }
        config.post_init();
        assert_eq!(config.transport(), Transport::Tcp as i32);

        let mut config = BotsConfig::new();
        for name in ["shouldUseTCP", "shouldUseWebSockets"] {
            if let Some(option) = config.options.iter_mut().find(|o| o.name == name) {
                option.value = Value::Bool(true);
            }
        }
        config.post_init();
        assert_eq!(config.transport(), Transport::WebSockets as i32);
    }

    /// A path that opens nowhere warns with the reference's line and
    /// keeps the default (`server_app_option.hpp:50-58`).
    #[test]
    fn a_missing_section_warns_and_keeps_the_default() {
        let mut config = BotsConfig::new();
        let mut warnings = Vec::new();
        assert!(config.init(&[], &mut warnings));

        assert_eq!(
            warnings,
            vec![
                "ServerAppConfig::init: bots/username not read from bw.xml chain. Using default (Bot)",
                "ServerAppConfig::init: bots/password not read from bw.xml chain. Using default ()",
                "ServerAppConfig::init: bots/serverName not read from bw.xml chain. Using default ()",
                "ServerAppConfig::init: bots/tag not read from bw.xml chain. Using default (Default)",
                "ServerAppConfig::init: bots/shouldUseTCP not read from bw.xml chain. Using default (false)",
                "ServerAppConfig::init: bots/shouldUseWebSockets not read from bw.xml chain. Using default (false)",
                "ServerAppConfig::init: bots/port not read from bw.xml chain. Using default (0)",
                "ServerAppConfig::init: bots/shouldUseRandomName not read from bw.xml chain. Using default (true)",
                "ServerAppConfig::init: bots/shouldUseScripts not read from bw.xml chain. Using default (true)",
                "ServerAppConfig::init: bots/standinEntity not read from bw.xml chain. Using default (DefaultEntity)",
                "ServerAppConfig::init: bots/controllerType not read from bw.xml chain. Using default (Patrol)",
                "ServerAppConfig::init: bots/controllerData not read from bw.xml chain. Using default (server/bots/test.bwp)",
                "ServerAppConfig::init: bots/publicKey not read from bw.xml chain. Using default ()",
                "ServerAppConfig::init: bots/logOnRetryPeriod not read from bw.xml chain. Using default (0)",
                "ServerAppConfig::init: bots/shouldListenForGeometryMappings not read from bw.xml chain. Using default (false)",
                "ServerAppConfig::init: bots/chunkLoadingPeriod not read from bw.xml chain. Using default (0.02)",
            ]
        );
        assert_eq!(config.username(), "Bot");
        assert_eq!(config.port(), 0);
        assert_eq!(config.transport(), Transport::Udp as i32);
    }

    /// A chain that carries the section overrides the defaults and
    /// silences the warnings (`BWConfig::update`).
    #[test]
    fn the_chain_overrides_the_defaults() {
        let section = parse(
            "root",
            b"<root><bots><username>Alice</username><port>20361</port>\
              <shouldUseRandomName>false</shouldUseRandomName>\
              <chunkLoadingPeriod>0.05</chunkLoadingPeriod></bots></root>",
        )
        .unwrap();
        let mut config = BotsConfig::new();
        let mut warnings = Vec::new();
        config.init(&[section], &mut warnings);

        assert_eq!(config.username(), "Alice");
        assert_eq!(config.port(), 20361);
        assert!(!config.should_use_random_name());
        assert_eq!(config.chunk_loading_period(), 0.05);
        assert_eq!(config.tag(), "Default");
        // Only the paths the chain does not carry warn: sixteen non-derived
        // options, four of them read.
        assert_eq!(warnings.len(), 12);
    }

    /// A present-but-unparseable section still counts as read: the
    /// value falls back to the current one and no warning fires (the
    /// reference's `DataSection::asBool`/`asInt` family reports success
    /// either way).
    #[test]
    fn a_present_but_unparseable_section_counts_as_read() {
        let section = parse(
            "root",
            b"<root><bots><port>not-a-number</port></bots></root>",
        )
        .unwrap();
        let mut config = BotsConfig::new();
        let mut warnings = Vec::new();
        config.init(&[section], &mut warnings);

        assert_eq!(config.port(), 0);
        assert!(!warnings.is_empty());
        assert!(
            warnings
                .iter()
                .all(|line| !line.contains("bots/port not read"))
        );
    }

    /// `printAll`'s face: the header, `%-34s` padded lines, the derived
    /// option contributing nothing, and the ` (default: ...) ` suffix
    /// only on moved options (`server_app_option.cpp:62-79`,
    /// `server_app_option.hpp:81-99`).
    #[test]
    fn print_all_matches_the_reference_face() {
        let section = parse(
            "root",
            b"<root><bots><username>Alice</username></bots></root>",
        )
        .unwrap();
        let mut config = BotsConfig::new();
        let mut warnings = Vec::new();
        config.init(&[section], &mut warnings);

        let printed = config.print_all();
        let mut lines = printed.lines();
        assert_eq!(
            lines.next(),
            Some("Configuration options (from server/bw.xml):")
        );
        assert_eq!(
            lines.next(),
            Some("  bots/username                      = Alice (default: Bot)")
        );
        assert_eq!(
            lines.next(),
            Some("  bots/password                      = ")
        );
        assert_eq!(
            lines.next(),
            Some("  bots/serverName                    = ")
        );
        assert_eq!(
            lines.next(),
            Some(format!("  {:<34} = Default", "bots/tag").as_str())
        );
        assert_eq!(printed.lines().count(), 17, "header plus sixteen options");
        let chunk_line = printed
            .lines()
            .find(|line| line.contains("bots/chunkLoadingPeriod"))
            .unwrap();
        assert_eq!(chunk_line, "  bots/chunkLoadingPeriod            = 0.02");
        // A name wider than the 34 columns wins: no padding, and the
        // reference's single literal space before the `=`.
        assert!(printed.contains("  bots/shouldListenForGeometryMappings = false"));
        assert!(
            !printed.contains("transport"),
            "derived options print nothing"
        );
    }

    /// The float print arm compares through `almostEqual`'s 0.0004
    /// epsilon (`stdmf.hpp:606`): a nudge inside it keeps the plain
    /// line.
    #[test]
    fn float_print_compares_with_the_reference_epsilon() {
        let section = parse(
            "root",
            b"<root><bots><chunkLoadingPeriod>0.020001</chunkLoadingPeriod></bots></root>",
        )
        .unwrap();
        let mut config = BotsConfig::new();
        let mut warnings = Vec::new();
        config.init(&[section], &mut warnings);
        let printed = config.print_all();
        let chunk_line = printed
            .lines()
            .find(|line| line.contains("bots/chunkLoadingPeriod"))
            .unwrap();
        assert_eq!(
            chunk_line,
            "  bots/chunkLoadingPeriod            = 0.020001"
        );

        let section = parse(
            "root",
            b"<root><bots><chunkLoadingPeriod>0.03</chunkLoadingPeriod></bots></root>",
        )
        .unwrap();
        let mut config = BotsConfig::new();
        let mut warnings = Vec::new();
        config.init(&[section], &mut warnings);
        let printed = config.print_all();
        let chunk_line = printed
            .lines()
            .find(|line| line.contains("bots/chunkLoadingPeriod"))
            .unwrap();
        assert_eq!(
            chunk_line,
            "  bots/chunkLoadingPeriod            = 0.03 (default: 0.02)"
        );
    }

    /// Every `Value` variant moves through `update_from` and prints the
    /// way its watcher type does, `Default` builds the same set, and an
    /// accessor asked for the wrong type falls back to the reference's
    /// zero instead of panicking (the typed C++ accessors cannot
    /// mismatch; the port stays total).
    #[test]
    fn value_variants_and_accessor_fallbacks_are_total() {
        assert_eq!(BotsConfig::default(), BotsConfig::new());

        let section = parse(
            "root",
            b"<root><username>Load</username><shouldUseTCP>true</shouldUseTCP>\
              <transport>1</transport><port>20361</port>\
              <logOnRetryPeriod>1.5</logOnRetryPeriod></root>",
        )
        .unwrap();
        let mut str_value = Value::Str(String::new());
        str_value.update_from(section.find_child("username").unwrap());
        assert_eq!(str_value.to_cpp_string(), "Load");

        let mut bool_value = Value::Bool(false);
        bool_value.update_from(section.find_child("shouldUseTCP").unwrap());
        assert_eq!(bool_value.to_cpp_string(), "true");

        let mut int_value = Value::Int(0);
        int_value.update_from(section.find_child("transport").unwrap());
        assert_eq!(int_value.to_cpp_string(), "1");

        let mut uint_value = Value::Uint(0);
        uint_value.update_from(section.find_child("port").unwrap());
        assert_eq!(uint_value.to_cpp_string(), "20361");

        let mut float_value = Value::Float(0.0);
        float_value.update_from(section.find_child("logOnRetryPeriod").unwrap());
        assert_eq!(float_value.to_cpp_string(), "1.5");

        // The accessors answer the fallback when the stored value is of
        // another type.
        let mut config = BotsConfig::new();
        for (name, value) in [
            ("username", Value::Int(7)),
            ("shouldUseTCP", Value::Int(1)),
            ("transport", Value::Bool(true)),
            ("port", Value::Bool(true)),
            ("logOnRetryPeriod", Value::Str("slow".to_string())),
            ("chunkLoadingPeriod", Value::Str("fast".to_string())),
        ] {
            if let Some(option) = config.options.iter_mut().find(|o| o.name == name) {
                option.value = value;
            }
        }
        assert_eq!(config.username(), "");
        assert!(!config.should_use_tcp());
        assert_eq!(config.transport(), 0);
        assert_eq!(config.port(), 0);
        assert_eq!(config.log_on_retry_period(), 0.0);
        assert_eq!(config.chunk_loading_period(), 0.02);
    }
}
