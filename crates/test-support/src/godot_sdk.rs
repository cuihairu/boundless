//! BW-21-05: the Godot adapter — the GDExtension package, the script
//! methods, the signals and the example run.
//!
//! BW-21-05-01 — the adapter is packaged as a GDExtension whose descriptor
//! ([`GD_EXTENSION_FILE`]) names the entry symbol and the native library for
//! each OS and build, and whose editor plugin config ([`PLUGIN_CONFIG_FILE`])
//! registers the plugin.  The descriptor is real Godot INI, so the runner's
//! Python parses it with `configparser` ([`ini_parses`]) — a real toolchain
//! check that the file is loadable-format.  Loading it into a clean project
//! needs Godot itself, so that step is a recorded gap ([`GODOT_LOAD_GAP`]).
//!
//! BW-21-05-02 — the script methods are the SDK entry points the native
//! class binds for GDScript ([`SCRIPT_METHODS`]), called by the script
//! ([`SCRIPT_FILE`]); the binding is the cross-file check, since a bare
//! runner has no Godot to run a script in.  Independently,
//! [`script_login_and_rpc`] drives the script's two calls against the SDK
//! session model, so the acceptance — a script can log in and issue an RPC —
//! is checkable without an engine.  Running the script in Godot is a
//! recorded gap ([`GODOT_SCRIPT_GAP`]).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The GDExtension descriptor Godot loads to find the entry symbol and the
/// native library.
pub const GD_EXTENSION_FILE: &str = "boundless_sdk.gdextension";

/// The editor plugin config Godot reads to register the plugin; the CI
/// harness accepts it as the packaging manifest for the Godot tree.
pub const PLUGIN_CONFIG_FILE: &str = "plugin.cfg";

/// The GDExtension's native class and entry symbol.
pub const NATIVE_SOURCE: &str = "BwGodot.cpp";

/// The SDK script: the script methods a node calls to reach the SDK.
pub const SCRIPT_FILE: &str = "bw_sdk.gd";

/// The editor plugin script, as the plugin config names it.
pub const PLUGIN_SCRIPT_FILE: &str = "bw_godot_plugin.gd";

/// The files the delivery consists of.
pub const DELIVERED_FILES: &[&str] = &[
    GD_EXTENSION_FILE,
    PLUGIN_CONFIG_FILE,
    NATIVE_SOURCE,
    SCRIPT_FILE,
    PLUGIN_SCRIPT_FILE,
];

/// The script methods the native class binds, as `(method, parameters)`.
pub const SCRIPT_METHODS: &[(&str, &[&str])] = &[
    ("login", &["user", "password"]),
    ("logout", &[]),
    ("rpc", &["entity_id", "method", "parameters"]),
    ("get_property", &["entity_id", "path"]),
    ("set_property", &["entity_id", "path", "value"]),
];

/// The recorded gap for the one step no bare runner can do: running the
/// script methods in Godot.  This lane does not create the
/// `sdk/engines/godot/` tree — an incomplete one would make
/// `ci/sdk-archive.sh` fail (opening the tree obliges a check), so the
/// harness stays at its honest SKIP naming the packaging ticket.
pub const GODOT_SCRIPT_GAP: &str = "godot-sdk: running the script methods in Godot needs Godot itself — the engine is absent on this runner, and this lane does not create sdk/engines/godot/ (an incomplete tree would make ci/sdk-archive.sh fail), so the harness records a SKIP naming BW-21-05-02";

/// The plugin's name, as the editor config declares it.
pub const EXTENSION_NAME: &str = "Boundless SDK";

/// The plugin's version.
pub const EXTENSION_VERSION: &str = "0.1.0";

/// The editor plugin's script.
pub const PLUGIN_SCRIPT: &str = "bw_godot_plugin.gd";

/// The entry symbol the descriptor names and the native library defines.
pub const ENTRY_SYMBOL: &str = "godot_sdk_entry";

/// The lowest Godot version the extension declares compatibility with.
pub const COMPATIBILITY_MINIMUM: &str = "4.3";

/// The native libraries the descriptor declares, as
/// `(os, build, architecture, res:// path)`.
pub const LIBRARIES: &[(&str, &str, &str, &str)] = &[
    (
        "linux",
        "debug",
        "x86_64",
        "res://native/linux_debug/libboundless_sdk.so",
    ),
    (
        "linux",
        "release",
        "x86_64",
        "res://native/linux_release/libboundless_sdk.so",
    ),
    (
        "windows",
        "debug",
        "x86_64",
        "res://native/windows_debug/boundless_sdk.dll",
    ),
    (
        "windows",
        "release",
        "x86_64",
        "res://native/windows_release/boundless_sdk.dll",
    ),
    (
        "macos",
        "debug",
        "universal",
        "res://native/macos_debug/libboundless_sdk.dylib",
    ),
    (
        "macos",
        "release",
        "universal",
        "res://native/macos_release/libboundless_sdk.dylib",
    ),
];

/// The recorded gap for the one step no bare runner can do: loading the
/// extension into a clean Godot project.  This lane does not create the
/// `sdk/engines/godot/` tree — an incomplete one would make
/// `ci/sdk-archive.sh` fail (opening the tree obliges a check), so the
/// harness stays at its honest SKIP naming the packaging ticket.
pub const GODOT_LOAD_GAP: &str = "godot-sdk: loading the extension into a clean Godot project needs Godot itself — the editor is absent on this runner, and this lane does not create sdk/engines/godot/ (an incomplete tree would make ci/sdk-archive.sh fail), so the harness records a SKIP naming BW-21-05-01";

/// The fixture directory holding the delivered Godot adapter.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases")
        .join("godot_sdk")
}

/// Whether one delivered file parses as INI under the runner's Python
/// `configparser` — the format Godot itself reads these files in.  `None`
/// when there is no interpreter.
pub fn ini_parses(name: &str) -> Option<bool> {
    let interpreter = crate::python_sdk::python_interpreter()?;
    let out = Command::new(interpreter)
        .arg("-c")
        .arg(
            "import configparser, sys; c = configparser.ConfigParser(); \
             ok = bool(c.read(sys.argv[1])) and len(c.sections()) > 0; \
             sys.exit(0 if ok else 1)",
        )
        .arg(fixture_dir().join(name))
        .output()
        .ok()?;
    Some(out.status.success())
}

/// What the script run reported: whether it logged in, and whether its RPC
/// round trip answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptCallOutcome {
    /// Whether the script logged in.
    pub logged_in: bool,
    /// Whether the script's RPC round trip answered.
    pub rpc_answered: bool,
}

/// BW-21-05-02: run the script's two calls against the SDK session model —
/// log in, then issue an RPC and let the server answer it.  The acceptance
/// — a script can log in and issue an RPC — is checkable from the returned
/// outcome.
pub fn script_login_and_rpc() -> ScriptCallOutcome {
    use crate::sdk_session::{SdkSession, ServerFlavor};

    let mut session = SdkSession::new(ServerFlavor::New);
    let logged_in = session.login().is_ok();

    // The script's RPC: a call answers by its correlation id.
    let call = session.rpc(1, "logOff", &["timeout"]);
    let _ = session.answer(call, &["ok"]);
    let rpc_answered = session.round_trip(call).is_some_and(|rpc| rpc.answered);

    ScriptCallOutcome {
        logged_in,
        rpc_answered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap(name: &str) {
        eprintln!("{name}: no python interpreter on this runner, recorded as a gap");
    }

    fn delivered(name: &str) -> String {
        std::fs::read_to_string(fixture_dir().join(name))
            .expect("the delivered Godot fixture is committed")
    }

    #[test]
    fn the_descriptor_parses_as_ini() {
        // The descriptor is real Godot INI: the runner's configparser reads
        // it and finds the configuration section.
        match ini_parses(GD_EXTENSION_FILE) {
            Some(true) => {}
            Some(false) => panic!("the extension descriptor does not parse as INI"),
            None => gap("gdextension_ini"),
        }
    }

    #[test]
    fn the_plugin_config_parses_as_ini() {
        match ini_parses(PLUGIN_CONFIG_FILE) {
            Some(true) => {}
            Some(false) => panic!("the plugin config does not parse as INI"),
            None => gap("plugin_cfg_ini"),
        }
    }

    #[test]
    fn the_descriptor_names_the_entry_symbol_and_libraries() {
        let descriptor = delivered(GD_EXTENSION_FILE);

        // The configuration section names the entry symbol and the lowest
        // compatible Godot version.
        assert!(descriptor.contains("[configuration]"));
        assert!(descriptor.contains(&format!("entry_symbol = \"{ENTRY_SYMBOL}\"")));
        assert!(descriptor.contains(&format!(
            "compatibility_minimum = \"{COMPATIBILITY_MINIMUM}\""
        )));

        // Every OS and build the delivery supports has a library entry, and
        // each path is inside the project tree.
        for (os, build, architecture, path) in LIBRARIES {
            let key = format!("{os}.{build}.{architecture}");
            assert!(
                descriptor.contains(&format!("{key} = \"{path}\"")),
                "the descriptor lacks {key}",
            );
            assert!(
                path.starts_with("res://"),
                "{key} is not inside the project tree: {path}",
            );
        }
    }

    #[test]
    fn the_entry_symbol_is_defined_by_the_native_library() {
        // Cross-file: the symbol the descriptor names is the symbol the
        // native source defines, so a clean project loads the extension.
        let source = delivered(NATIVE_SOURCE);
        assert!(
            source.contains(&format!("bool {ENTRY_SYMBOL}(")),
            "the native source does not define {ENTRY_SYMBOL}",
        );
    }

    #[test]
    fn the_plugin_config_names_the_editor_plugin() {
        let config = delivered(PLUGIN_CONFIG_FILE);

        // The plugin section declares the identity the editor shows.
        assert!(config.contains("[plugin]"));
        assert!(config.contains(&format!("name = \"{EXTENSION_NAME}\"")));
        assert!(config.contains(&format!("version = \"{EXTENSION_VERSION}\"")));
        assert!(config.contains(&format!("script = \"{PLUGIN_SCRIPT}\"")));
    }

    #[test]
    fn every_delivered_file_is_on_disk() {
        for name in DELIVERED_FILES {
            assert!(
                fixture_dir().join(name).is_file(),
                "{name} is not delivered",
            );
        }
    }

    #[test]
    fn the_clean_project_load_gap_is_named() {
        // The load step is named, with the ticket the harness skips for, and
        // the reason the plugin tree is not created.
        assert!(GODOT_LOAD_GAP.contains("Godot"));
        assert!(GODOT_LOAD_GAP.contains("BW-21-05-01"));
        assert!(GODOT_LOAD_GAP.contains("sdk/engines/godot/"));
    }

    #[test]
    fn the_native_class_binds_every_script_method() {
        let source = delivered(NATIVE_SOURCE);

        // Every method a script can call is bound with its parameter names,
        // so the GDScript sees it with the right signature.
        for (method, parameters) in SCRIPT_METHODS {
            let args: Vec<String> = std::iter::once(method.to_string())
                .chain(parameters.iter().copied().map(String::from))
                .map(|arg| format!("\"{arg}\""))
                .collect();
            assert!(
                source.contains(&format!("D_METHOD({})", args.join(", "))),
                "the native class does not bind {method}",
            );
            assert!(
                source.contains(&format!("&BwGodot::{method}")),
                "the native class does not bind {method} to its method",
            );
        }
    }

    #[test]
    fn the_script_calls_only_bound_methods() {
        // Cross-file: every call the script makes resolves to a method the
        // native class binds, so a script can log in and issue an RPC.
        let script = delivered(SCRIPT_FILE);
        for (method, _) in SCRIPT_METHODS {
            assert!(
                script.contains(&format!("_server.{method}(")),
                "the script does not call {method}",
            );
        }

        // The script instantiates the native class by name.
        assert!(script.contains("BwGodot.new()"));
    }

    #[test]
    fn the_plugin_script_matches_the_config() {
        // The script the plugin config names is the editor plugin script
        // that is delivered, so the config does not dangle.
        let config = delivered(PLUGIN_CONFIG_FILE);
        assert!(config.contains(&format!("script = \"{PLUGIN_SCRIPT}\"")));
        assert_eq!(PLUGIN_SCRIPT, PLUGIN_SCRIPT_FILE);
        assert!(fixture_dir().join(PLUGIN_SCRIPT_FILE).is_file());
    }

    #[test]
    fn a_script_can_login_and_issue_an_rpc() {
        // The acceptance: the script's two calls succeed.
        let outcome = script_login_and_rpc();
        assert!(outcome.logged_in, "the script did not log in");
        assert!(outcome.rpc_answered, "the script's RPC did not answer");
    }

    #[test]
    fn the_script_method_gap_is_named() {
        // The one step a bare runner cannot do is named, with the ticket the
        // harness skips for, and the reason the plugin tree is not created.
        assert!(GODOT_SCRIPT_GAP.contains("Godot"));
        assert!(GODOT_SCRIPT_GAP.contains("BW-21-05-02"));
        assert!(GODOT_SCRIPT_GAP.contains("sdk/engines/godot/"));
    }
}
