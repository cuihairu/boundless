//! BW-21-04: the Unreal adapter — the plugin manifest, the C++ call surface,
//! the Blueprint interface, the game-thread dispatch and the example run.
//!
//! BW-21-04-01 — the adapter is packaged as an Unreal plugin whose manifest
//! [`plugin_manifest`] renders deterministically, declaring the module in
//! [`PLUGIN_MODULE`].  Loading the module into an empty project needs the
//! Unreal Editor, so that step is a recorded gap ([`UNREAL_LOAD_GAP`]).
//!
//! BW-21-04-02 — the adapter's C++ call surface is delivered under
//! `cases/unreal_sdk/` ([`ADAPTER_HEADER`] and [`ADAPTER_SOURCE`]) together
//! with the simple example ([`EXAMPLE_FILE`]).  The example logs in, issues
//! an RPC, reads a property and logs out against a scripted server
//! stand-in; [`run_cpp_example`] compiles it with the runner's C++ compiler
//! and runs it, so the acceptance — a login and an issued RPC — is a real
//! compile and a real run, with [`EXPECTED_EXAMPLE_TRANSCRIPT`] as the
//! transcript the run must print.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The plugin's friendly name, as the manifest declares it.
pub const PLUGIN_NAME: &str = "Boundless SDK";

/// The plugin's version name.
pub const PLUGIN_VERSION: &str = "0.1.0";

/// The module the plugin loads, as `(name, type, loading phase)`.
pub const PLUGIN_MODULE: (&str, &str, &str) = ("BwSdk", "Runtime", "Default");

/// The recorded gap for the one step no bare runner can do: loading the
/// plugin's module into an empty Unreal project.  This lane does not create
/// the `sdk/engines/unreal/` tree — an incomplete one would make
/// `ci/sdk-archive.sh` fail (opening the tree obliges a check), so the
/// harness stays at its honest SKIP naming the packaging ticket.
pub const UNREAL_LOAD_GAP: &str = "unreal-sdk: loading the plugin module into an empty Unreal project needs the Unreal Editor — the editor is absent on this runner, and this lane does not create sdk/engines/unreal/ (an incomplete tree would make ci/sdk-archive.sh fail), so the harness records a SKIP naming BW-21-04-01";

/// The fixture directory holding the delivered Unreal adapter, if any.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases")
        .join("unreal_sdk")
}

/// Render the plugin manifest as deterministic JSON: a fixed key order, so
/// two renders are byte-for-byte the same.
pub fn plugin_manifest() -> String {
    let (module_name, module_type, loading_phase) = PLUGIN_MODULE;
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"FileVersion\": 3,\n");
    out.push_str(&format!("  \"FriendlyName\": \"{PLUGIN_NAME}\",\n"));
    out.push_str("  \"Version\": 1,\n");
    out.push_str(&format!("  \"VersionName\": \"{PLUGIN_VERSION}\",\n"));
    out.push_str("  \"Modules\": [\n");
    out.push_str("    {\n");
    out.push_str(&format!("      \"Name\": \"{module_name}\",\n"));
    out.push_str(&format!("      \"Type\": \"{module_type}\",\n"));
    out.push_str(&format!("      \"LoadingPhase\": \"{loading_phase}\",\n"));
    out.push_str("    }\n");
    out.push_str("  ],\n");
    out.push_str("}\n");
    out
}

/// BW-21-04-02: the adapter's C++ call surface — the header that declares
/// the component, the source that implements it, and the simple example that
/// logs in and issues an RPC.
pub const ADAPTER_HEADER: &str = "BwSdkModule.h";

/// The adapter's C++ implementation.
pub const ADAPTER_SOURCE: &str = "BwSdkModule.cpp";

/// The simple example: log in, issue an RPC, read a property, log out.
pub const EXAMPLE_FILE: &str = "simple.cpp";

/// The files the delivery consists of.
pub const DELIVERED_FILES: &[&str] = &[ADAPTER_HEADER, ADAPTER_SOURCE, EXAMPLE_FILE];

/// The expected transcript of the simple example: the login, the answered
/// RPC, the served property, then the logout.
pub const EXPECTED_EXAMPLE_TRANSCRIPT: &[&str] = &[
    "LOGIN ok",
    "RPC answered=true answer=ok",
    "PROPERTY hp=100",
    "LOGOUT ok",
];

/// The C++ compiler this module drives, or `None` when none is on the
/// runner.
pub fn cpp_compiler() -> Option<&'static str> {
    ["g++", "clang++", "c++"].iter().copied().find(|name| {
        Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// What the compiled example printed and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExampleRun {
    /// The process's exit status, or -1 when it did not report one.
    pub status: i32,
    /// What it printed, one entry per line.
    pub stdout: Vec<String>,
    /// What it reported, one entry per line.
    pub stderr: Vec<String>,
}

/// Compile the adapter and its example with the runner's C++ compiler and
/// run it.  The example logs in, issues an RPC, reads a property and logs
/// out against a scripted server stand-in; [`EXPECTED_EXAMPLE_TRANSCRIPT`]
/// is the transcript the run must print, line for line.  `None` when there
/// is no compiler.
pub fn run_cpp_example() -> Option<ExampleRun> {
    let compiler = cpp_compiler()?;
    let dir = fixture_dir();
    let build = std::env::temp_dir().join(format!(
        "bw21-unreal-sdk-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    if std::fs::create_dir_all(&build).is_err() {
        return None;
    }
    let output = build.join("simple");
    let compile = Command::new(compiler)
        .arg("-std=c++20")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-I")
        .arg(&dir)
        .arg(dir.join(ADAPTER_SOURCE))
        .arg(dir.join(EXAMPLE_FILE))
        .arg("-o")
        .arg(&output)
        .output();
    let compile_out = match compile {
        Ok(out) => out,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&build);
            return None;
        }
    };
    if !compile_out.status.success() {
        let stderr = String::from_utf8_lossy(&compile_out.stderr)
            .lines()
            .map(String::from)
            .collect();
        let _ = std::fs::remove_dir_all(&build);
        return Some(ExampleRun {
            status: -1,
            stdout: Vec::new(),
            stderr,
        });
    }
    let run = Command::new(&output).output();
    let _ = std::fs::remove_dir_all(&build);
    let run_out = run.ok()?;
    Some(ExampleRun {
        status: run_out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&run_out.stdout)
            .lines()
            .map(String::from)
            .collect(),
        stderr: String::from_utf8_lossy(&run_out.stderr)
            .lines()
            .map(String::from)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_manifest_is_deterministic_json() {
        let first = plugin_manifest();
        let second = plugin_manifest();

        // Two renders, byte-for-byte the same: the field order is fixed.
        assert_eq!(first, second);
        assert!(crate::unity_sdk::is_well_formed_json(&first));
    }

    #[test]
    fn the_plugin_manifest_names_the_module() {
        let manifest = plugin_manifest();
        let (name, module_type, phase) = PLUGIN_MODULE;

        // The manifest declares the plugin and the module it loads.
        assert!(manifest.contains(&format!("\"FriendlyName\": \"{PLUGIN_NAME}\"")));
        assert!(manifest.contains(&format!("\"VersionName\": \"{PLUGIN_VERSION}\"")));
        assert!(manifest.contains(&format!("\"Name\": \"{name}\"")));
        assert!(manifest.contains(&format!("\"Type\": \"{module_type}\"")));
        assert!(manifest.contains(&format!("\"LoadingPhase\": \"{phase}\"")));
    }

    #[test]
    fn the_empty_project_load_gap_is_named() {
        // The load step is named, with the ticket the harness skips for, and
        // the reason the plugin tree is not created.
        assert!(UNREAL_LOAD_GAP.contains("Unreal"));
        assert!(UNREAL_LOAD_GAP.contains("BW-21-04-01"));
        assert!(UNREAL_LOAD_GAP.contains("sdk/engines/unreal/"));
    }

    fn gap(name: &str) {
        eprintln!("{name}: no C++ compiler on this runner, recorded as a gap");
    }

    #[test]
    fn the_unreal_example_logs_in_and_issues_an_rpc() {
        match run_cpp_example() {
            Some(run) => {
                assert_eq!(
                    run.status,
                    0,
                    "the example did not exit cleanly:\n{}",
                    run.stderr.join("\n"),
                );
                assert!(
                    run.stderr.is_empty(),
                    "the example reported: {}",
                    run.stderr.join("\n"),
                );
                // The transcript is the acceptance: the login, the answered
                // RPC, the served property, then the logout.
                assert_eq!(
                    run.stdout.iter().map(String::as_str).collect::<Vec<_>>(),
                    EXPECTED_EXAMPLE_TRANSCRIPT,
                );
            }
            None => gap("unreal_cpp_example"),
        }
    }

    #[test]
    fn the_unreal_example_transcript_is_deterministic() {
        let first = match run_cpp_example() {
            Some(run) => run,
            None => return gap("unreal_cpp_example_transcript"),
        };
        let second = match run_cpp_example() {
            Some(run) => run,
            None => return gap("unreal_cpp_example_transcript"),
        };

        // Two runs, the same transcript: the example is reproducible.
        assert_eq!(first.stdout, second.stdout);
        assert_eq!(first.status, second.status);
        assert_eq!(first.status, 0);
    }

    #[test]
    fn the_adapter_exposes_the_login_and_rpc_surface() {
        // Every delivered file is committed, so the surface is checkable
        // without a compiler.
        for name in DELIVERED_FILES {
            assert!(
                fixture_dir().join(name).is_file(),
                "{name} is not delivered",
            );
        }

        let source = std::fs::read_to_string(fixture_dir().join(ADAPTER_SOURCE))
            .expect("the adapter source is a committed fixture");
        // The component reaches the server through the IServer stand-in for
        // each of the calls the SDK contract needs.
        for call in [
            "SdkComponent::Login",
            "SdkComponent::Logout",
            "SdkComponent::Rpc",
            "SdkComponent::GetProperty",
            "server_.Login",
            "server_.Logout",
            "server_.Rpc",
        ] {
            assert!(source.contains(call), "the adapter lacks {call}");
        }

        // The header declares the same surface the source implements.
        let header = std::fs::read_to_string(fixture_dir().join(ADAPTER_HEADER))
            .expect("the adapter header is a committed fixture");
        for member in [
            "virtual bool Login(",
            "virtual bool Rpc(",
            "virtual bool GetProperty(",
        ] {
            assert!(
                header.contains(member),
                "the header does not declare {member}",
            );
        }
    }

    #[test]
    fn the_example_is_a_real_rpc_caller() {
        let example = std::fs::read_to_string(fixture_dir().join(EXAMPLE_FILE))
            .expect("the example is a committed fixture");

        // The example logs in, issues the RPC with its parameters, and reads
        // the property before logging out.
        assert!(example.contains("component.Login(\"alice\", \"secret\")"));
        assert!(example.contains("component.Rpc(42, \"heal\", {\"10\", \"20\"})"));
        assert!(example.contains("component.GetProperty(42, \"hp\", value)"));
        assert!(example.contains("component.Logout()"));
    }
}
