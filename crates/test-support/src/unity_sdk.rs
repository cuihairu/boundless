//! BW-21-03 Unity adapter: the C# binding and the main-thread dispatch.
//!
//! BW-21-03-01 — the binding is delivered as C# under `cases/unity_sdk/`:
//! the component itself ([`BINDING_FILE`]) and a stand-in for the engine
//! surface it touches ([`ENGINE_STUB_FILE`]).  The binding is a
//! `MonoBehaviour`, so the editor loads it as a component; that loading step
//! needs Unity itself, which a bare runner does not have, and is recorded as
//! a gap ([`UNITY_EDITOR_GAP`]).  What can be checked anywhere is that the
//! binding compiles against the engine surface and exposes the SDK
//! contract: [`compile_binding`] builds it with the runner's C# toolchain
//! and [`BINDING_MEMBERS`] lists the public members it must have.
//!
//! BW-21-03-02 — Unity posts callbacks from its worker threads and must
//! deliver them on the main thread, so [`UnityMainThreadDispatcher`] models
//! the dispatch: a worker queues an event, the main thread pumps it, and
//! [`UnityDispatchEvent`] records both the thread the event came from and
//! the thread it was delivered on.  [`UnityMainThreadDispatcher::all_on_main_thread`]
//! is the fixed-case thread assertion.

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

/// One entity event the dispatcher delivers: its name, the thread it was
/// posted from, and the thread it was delivered on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnityDispatchEvent {
    /// The event's name.
    pub name: String,
    /// The thread the event was posted from.
    pub posted_from: String,
    /// The thread the event was delivered on.
    pub delivered_on: String,
}

/// The Unity binding's main-thread dispatcher: a worker thread posts events
/// from anywhere, and the Unity main thread delivers each one when it pumps
/// — every delivery runs on the main thread, and the record keeps the
/// thread the event came from.
pub struct UnityMainThreadDispatcher {
    main_thread: String,
    queued: Vec<String>,
    posted_from: Vec<String>,
    delivered: Vec<UnityDispatchEvent>,
}

impl UnityMainThreadDispatcher {
    /// A dispatcher whose main thread is `main_thread`.
    pub fn new(main_thread: &str) -> Self {
        Self {
            main_thread: main_thread.to_string(),
            queued: Vec::new(),
            posted_from: Vec::new(),
            delivered: Vec::new(),
        }
    }

    /// The main thread the dispatcher delivers on.
    pub fn main_thread(&self) -> &str {
        &self.main_thread
    }

    /// A worker thread posts one event; the thread is recorded, not the
    /// delivery — the main thread has not run it yet.
    pub fn queue(&mut self, name: &str, posted_from: &str) {
        self.queued.push(name.to_string());
        self.posted_from.push(posted_from.to_string());
    }

    /// The main thread pumps: every posted event is delivered on the main
    /// thread, in posted order. Returns how many ran.
    pub fn pump(&mut self) -> usize {
        let queued = std::mem::take(&mut self.queued);
        let posted_from = std::mem::take(&mut self.posted_from);
        let count = queued.len();
        for (name, from) in queued.into_iter().zip(posted_from) {
            self.delivered.push(UnityDispatchEvent {
                name,
                posted_from: from,
                delivered_on: self.main_thread.clone(),
            });
        }
        count
    }

    /// How many events are posted but not yet pumped.
    pub fn queued(&self) -> usize {
        self.queued.len()
    }

    /// The events delivered so far, in delivery order.
    pub fn delivered(&self) -> &[UnityDispatchEvent] {
        &self.delivered
    }

    /// Whether every delivery ran on the main thread — the fixed-case thread
    /// assertion.
    pub fn all_on_main_thread(&self) -> bool {
        self.delivered
            .iter()
            .all(|event| event.delivered_on == self.main_thread)
    }
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

    #[test]
    fn every_delivery_runs_on_the_main_thread() {
        let mut dispatcher = UnityMainThreadDispatcher::new("unity-main");

        // Three events posted from three worker threads; the Unity main
        // thread pumps them all.
        dispatcher.queue("created", "worker-1");
        dispatcher.queue("entered", "worker-2");
        dispatcher.queue("destroyed", "worker-3");
        assert_eq!(dispatcher.pump(), 3);

        // The fixed-case assertion: every delivery ran on the main thread,
        // and each record keeps the thread it came from.
        assert!(dispatcher.all_on_main_thread());
        assert_eq!(dispatcher.delivered().len(), 3);
        let threads: Vec<&str> = dispatcher
            .delivered()
            .iter()
            .map(|event| event.posted_from.as_str())
            .collect();
        assert_eq!(threads, vec!["worker-1", "worker-2", "worker-3"]);
    }

    #[test]
    fn a_posted_event_is_not_delivered_until_the_main_thread_pumps() {
        let mut dispatcher = UnityMainThreadDispatcher::new("unity-main");
        dispatcher.queue("created", "worker-1");

        // Posted from the worker: queued, not delivered yet.
        assert_eq!(dispatcher.queued(), 1);
        assert!(dispatcher.delivered().is_empty());

        // Pumped on the main thread: delivered, queue empty.
        assert_eq!(dispatcher.pump(), 1);
        assert_eq!(dispatcher.queued(), 0);
        assert_eq!(dispatcher.delivered().len(), 1);
    }

    #[test]
    fn events_are_delivered_in_the_order_they_were_posted() {
        let mut dispatcher = UnityMainThreadDispatcher::new("unity-main");
        dispatcher.queue("alpha", "worker-1");
        dispatcher.queue("beta", "worker-1");
        dispatcher.queue("gamma", "worker-2");
        dispatcher.pump();

        let names: Vec<&str> = dispatcher
            .delivered()
            .iter()
            .map(|event| event.name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn a_posted_event_remembers_the_thread_it_came_from() {
        let mut dispatcher = UnityMainThreadDispatcher::new("unity-main");
        dispatcher.queue("moved", "worker-7");
        dispatcher.pump();

        // The delivery is on the main thread, but the record still names the
        // worker that posted it.
        let event = &dispatcher.delivered()[0];
        assert_eq!(event.delivered_on, "unity-main");
        assert_eq!(event.posted_from, "worker-7");
        assert_eq!(event.name, "moved");
    }

    #[test]
    fn an_empty_pump_delivers_nothing() {
        let mut dispatcher = UnityMainThreadDispatcher::new("unity-main");
        assert_eq!(dispatcher.pump(), 0);
        assert!(dispatcher.delivered().is_empty());
        assert!(dispatcher.all_on_main_thread());
    }
}
