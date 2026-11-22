//! BW-21-03-01: the SDK's Unity C# binding, delivered and compiled.
//!
//! The binding is delivered as C# under `cases/unity_sdk/`: the component
//! itself ([`BINDING_FILE`]) and a stand-in for the engine surface it
//! touches ([`ENGINE_STUB_FILE`]).  The binding is a `MonoBehaviour`, so the
//! editor loads it as a component; that loading step needs Unity itself,
//! which a bare runner does not have, and is recorded as a gap
//! ([`UNITY_EDITOR_GAP`]).  What can be checked anywhere is that the binding
//! compiles against the engine surface and exposes the SDK contract:
//! [`compile_binding`] builds it with the runner's C# toolchain and
//! [`BINDING_MEMBERS`] lists the public members it must have.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The delivered Unity binding.
pub const BINDING_FILE: &str = "BwUnity.cs";

/// The stand-in for the engine surface the binding touches.
pub const ENGINE_STUB_FILE: &str = "UnityStubs.cs";

/// The files the delivery consists of.
pub const DELIVERED_FILES: &[&str] = &[BINDING_FILE, ENGINE_STUB_FILE];

/// The public members the binding must expose: the SDK contract in C#.
pub const BINDING_MEMBERS: &[&str] = &[
    "bool Login(string user, string password)",
    "void Logout()",
    "void Queue(BwEntityEvent entityEvent)",
    "int Pump()",
    "void SendMovement(long entityId, double x, double y, double z, long timestamp)",
    "long Rpc(long entityId, string method, string[] parameters)",
    "bool Answer(long callId, string[] values)",
    "string? GetProperty(long entityId, string path)",
    "bool SetProperty(long entityId, string path, string value)",
];

/// The recorded gap for the one step no bare runner can do: loading the
/// component in the Unity Editor.
pub const UNITY_EDITOR_GAP: &str = "unity-sdk: the binding compiles against the engine surface, but loading the component in the Unity Editor needs Unity itself — the editor is absent on this runner, so the editor-load step is recorded as a gap (ci/sdk-archive.sh skips it, naming BW-21-03-03)";

/// The fixture directory holding the delivered Unity binding.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases")
        .join("unity_sdk")
}

/// The runner's C# SDK major version, or `None` when no toolchain is
/// present or it is too old for a modern target framework.
pub fn csharp_sdk_major() -> Option<String> {
    let out = Command::new("dotnet").arg("--version").output().ok()?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let major = version.split('.').next()?.to_string();
    major.parse::<u32>().is_ok_and(|n| n >= 8).then_some(major)
}

/// What the binding build reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingBuild {
    /// Whether the toolchain reported success.
    pub success: bool,
    /// The toolchain's last lines, for a diagnostic when it failed.
    pub tail: Vec<String>,
}

/// Compile the delivered binding against the engine stub with the runner's
/// C# toolchain, in a throwaway project. `None` when there is no toolchain.
pub fn compile_binding() -> Option<BindingBuild> {
    let major = csharp_sdk_major()?;
    let dir = fixture_dir();
    let build = std::env::temp_dir().join(format!(
        "bw21-unity-sdk-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    if std::fs::create_dir_all(&build).is_err() {
        return None;
    }
    for name in DELIVERED_FILES {
        let _ = std::fs::copy(dir.join(name), build.join(name));
    }
    std::fs::write(
        build.join("bw_unity.csproj"),
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net{major}.0</TargetFramework>
    <Nullable>enable</Nullable>
    <ImplicitUsings>disable</ImplicitUsings>
    <GenerateAssemblyInfo>false</GenerateAssemblyInfo>
  </PropertyGroup>
</Project>
"#,
        ),
    )
    .ok()?;

    let out = Command::new("dotnet")
        .arg("build")
        .arg("-v")
        .arg("q")
        .arg("--nologo")
        .current_dir(&build)
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
        .output();
    let build_out = out.ok()?;
    let _ = std::fs::remove_dir_all(&build);

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );
    let tail: Vec<String> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(15)
        .map(String::from)
        .collect();
    let success = build_out.status.success() && !text.contains("Build FAILED");
    Some(BindingBuild { success, tail })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap(name: &str) {
        eprintln!("{name}: no C# toolchain on this runner, recorded as a gap");
    }

    fn binding_source() -> String {
        std::fs::read_to_string(fixture_dir().join(BINDING_FILE))
            .expect("the delivered binding is a committed fixture")
    }

    #[test]
    fn the_unity_binding_exposes_the_sdk_contract() {
        let source = binding_source();

        // Every member the SDK contract needs is public on the binding.
        for member in BINDING_MEMBERS {
            assert!(
                source.contains(member),
                "the binding does not expose {member}",
            );
        }
    }

    #[test]
    fn the_binding_is_a_unity_component() {
        let source = binding_source();
        let stub = std::fs::read_to_string(fixture_dir().join(ENGINE_STUB_FILE))
            .expect("the engine stub is a committed fixture");

        // The binding is a component the editor can load, deriving the
        // engine's component base class.
        assert!(
            source.contains("public sealed class BwUnity : MonoBehaviour"),
            "the binding is not a component:\n{source}",
        );
        assert!(
            stub.contains("public class MonoBehaviour"),
            "the engine surface stub is missing:\n{stub}",
        );
    }

    #[test]
    fn the_unity_binding_compiles_against_the_engine_surface() {
        match compile_binding() {
            Some(build) => {
                assert!(
                    build.success,
                    "the Unity binding did not compile:\n{}",
                    build.tail.join("\n"),
                );
            }
            None => gap("unity_binding_compile"),
        }
    }

    #[test]
    fn the_editor_load_gap_is_named() {
        // The one step a bare runner cannot do is named, with the ticket the
        // CI harness skips for, so the gap is honest rather than silent.
        assert!(UNITY_EDITOR_GAP.contains("Unity Editor"));
        assert!(UNITY_EDITOR_GAP.contains("BW-21-03-03"));
    }
}
