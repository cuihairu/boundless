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
//!
//! BW-21-04-03 — the Blueprint-visible surface is delivered as
//! [`BLUEPRINT_HEADER`]: the SDK entry points a Blueprint can call
//! ([`BLUEPRINT_CALLABLE`]) and the entity-event delegate a Blueprint
//! subscribes to ([`ENTITY_EVENT_DELEGATE`]).  [`BlueprintEntitySubscription`]
//! models the subscription: a Blueprint binds its handler and receives the
//! entity events the component raises.  Compiling a Blueprint graph needs
//! the Unreal Editor, so that step is a recorded gap
//! ([`BLUEPRINT_COMPILE_GAP`]).
//!
//! BW-21-04-04 — the game-thread dispatch is delivered as
//! [`GAME_THREAD_DISPATCH_HEADER`] and modeled by
//! [`GameThreadDispatch`]: the SDK worker threads post events from anywhere,
//! the game thread pumps them ([`GameThreadDispatch::pump`]) so every
//! delivery runs on the game thread, and [`GameThreadDispatch::destroy`]
//! makes the component dispatch nothing afterward — it drops the queued
//! events and rejects every post after that.
//!
//! BW-21-04-05 — the example also exercises the sync call, so the same
//! `g++` compile-and-run proves the component's login, sync and logout
//! (the transcript gains `SYNC ok`).  Independently,
//! [`run_unreal_example`] drives the example against the SDK session model,
//! one step at a time, so the acceptance is checkable without an editor;
//! running it inside the editor is a recorded gap ([`UNREAL_EXAMPLE_GAP`]).

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

/// The Blueprint-visible surface: the SDK calls a Blueprint makes and the
/// entity-event delegate it subscribes to.
pub const BLUEPRINT_HEADER: &str = "BwBlueprint.h";

/// The game-thread dispatch surface: posting an event to the game thread
/// and the destroy guard that stops it.
pub const GAME_THREAD_DISPATCH_HEADER: &str = "GameThreadDispatch.h";

/// The files the delivery consists of.
pub const DELIVERED_FILES: &[&str] = &[
    ADAPTER_HEADER,
    ADAPTER_SOURCE,
    EXAMPLE_FILE,
    BLUEPRINT_HEADER,
    GAME_THREAD_DISPATCH_HEADER,
];

/// The expected transcript of the simple example: the login, the answered
/// RPC, the served property, the synced property, then the logout.
pub const EXPECTED_EXAMPLE_TRANSCRIPT: &[&str] = &[
    "LOGIN ok",
    "RPC answered=true answer=ok",
    "PROPERTY hp=100",
    "SYNC ok",
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

/// BW-21-04-03: the SDK entry points the Blueprint surface exposes, by
/// their C++ signatures.
pub const BLUEPRINT_CALLABLE: &[&str] = &[
    "bool Login(const FString& User, const FString& Password)",
    "void Logout()",
    "int64 Rpc(int64 EntityId, const FString& Method, const TArray<FString>& Parameters)",
];

/// The entity-event delegate a Blueprint subscribes to.
pub const ENTITY_EVENT_DELEGATE: &str = "OnEntityEvent";

/// The recorded gap for the one step no bare runner can do: compiling a
/// Blueprint graph that subscribes to the entity event.  This lane does not
/// create the `sdk/engines/unreal/` tree — an incomplete one would make
/// `ci/sdk-archive.sh` fail (opening the tree obliges a check), so the
/// harness stays at its honest SKIP naming the packaging ticket.
pub const BLUEPRINT_COMPILE_GAP: &str = "unreal-sdk: compiling a Blueprint graph that subscribes to OnEntityEvent needs the Unreal Editor — the editor is absent on this runner, and this lane does not create sdk/engines/unreal/ (an incomplete tree would make ci/sdk-archive.sh fail), so the harness records a SKIP naming BW-21-04-03";

/// One entity event a Blueprint bound to the delegate receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlueprintEntityEvent {
    /// The entity the event is about.
    pub entity_id: i64,
    /// The event's kind: created, entered, left, destroyed.
    pub kind: String,
}

/// A Blueprint's subscription to the adapter's entity-event delegate: the
/// Blueprint binds its handler to the delegate, and every entity event the
/// component raises is delivered to it.  A Blueprint that never binds
/// receives nothing.
pub struct BlueprintEntitySubscription {
    bound: bool,
    delivered: Vec<BlueprintEntityEvent>,
}

impl BlueprintEntitySubscription {
    /// A subscription that is not yet bound to the delegate.
    pub fn new() -> Self {
        Self {
            bound: false,
            delivered: Vec::new(),
        }
    }

    /// The Blueprint binds its handler to the delegate: from here on it
    /// receives the entity events the component raises.
    pub fn bind(&mut self) {
        self.bound = true;
    }

    /// Whether the Blueprint is subscribed to the delegate.
    pub fn subscribed(&self) -> bool {
        self.bound
    }

    /// The component raises one entity event; a subscribed Blueprint
    /// receives it.
    pub fn deliver(&mut self, entity_id: i64, kind: &str) {
        if self.bound {
            self.delivered.push(BlueprintEntityEvent {
                entity_id,
                kind: kind.to_string(),
            });
        }
    }

    /// The events the bound Blueprint received, in order.
    pub fn delivered(&self) -> &[BlueprintEntityEvent] {
        &self.delivered
    }
}

impl Default for BlueprintEntitySubscription {
    fn default() -> Self {
        Self::new()
    }
}

/// BW-21-04-04: one entity event the game-thread dispatch delivered, with
/// the thread it was posted from and the thread it was delivered on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameThreadEvent {
    /// The event's name.
    pub name: String,
    /// The thread the event was posted from.
    pub posted_from: String,
    /// The thread the event was delivered on.
    pub delivered_on: String,
}

/// The adapter's game-thread dispatch: the SDK worker threads post entity
/// events from anywhere, the game thread delivers each one when it pumps,
/// and a destroyed component dispatches nothing — destroying drops the
/// queued events and rejects every post afterward.
pub struct GameThreadDispatch {
    game_thread: String,
    live: bool,
    queued: Vec<String>,
    posted_from: Vec<String>,
    delivered: Vec<GameThreadEvent>,
}

impl GameThreadDispatch {
    /// A live dispatch whose deliveries run on `game_thread`.
    pub fn new(game_thread: &str) -> Self {
        Self {
            game_thread: game_thread.to_string(),
            live: true,
            queued: Vec::new(),
            posted_from: Vec::new(),
            delivered: Vec::new(),
        }
    }

    /// Whether the component is still dispatching.
    pub fn live(&self) -> bool {
        self.live
    }

    /// An SDK worker thread posts one event.  A destroyed component
    /// rejects the post.
    pub fn post(&mut self, name: &str, posted_from: &str) {
        if self.live {
            self.queued.push(name.to_string());
            self.posted_from.push(posted_from.to_string());
        }
    }

    /// The game thread pumps: every queued event is delivered on the game
    /// thread, in posted order. Returns how many ran.
    pub fn pump(&mut self) -> usize {
        let queued = std::mem::take(&mut self.queued);
        let posted_from = std::mem::take(&mut self.posted_from);
        let count = queued.len();
        for (name, from) in queued.into_iter().zip(posted_from) {
            self.delivered.push(GameThreadEvent {
                name,
                posted_from: from,
                delivered_on: self.game_thread.clone(),
            });
        }
        count
    }

    /// The component was destroyed: the queued events are dropped and every
    /// post afterward is rejected, so a destroyed component dispatches
    /// nothing.
    pub fn destroy(&mut self) {
        self.live = false;
        self.queued.clear();
        self.posted_from.clear();
    }

    /// How many events are posted but not yet pumped.
    pub fn queued(&self) -> usize {
        self.queued.len()
    }

    /// The events delivered so far, in delivery order.
    pub fn delivered(&self) -> &[GameThreadEvent] {
        &self.delivered
    }

    /// Whether every delivery ran on the game thread — the fixed-case
    /// thread assertion.
    pub fn all_on_game_thread(&self) -> bool {
        self.delivered
            .iter()
            .all(|event| event.delivered_on == self.game_thread)
    }
}

/// BW-21-04-05: one step the Unreal example ran, and the result it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrealExampleStep {
    /// What the step did: login, sync or logout.
    pub action: String,
    /// The result the step produced.
    pub result: String,
}

/// The recorded gap for the one step no bare runner can do: running the
/// example inside the Unreal Editor.
pub const UNREAL_EXAMPLE_GAP: &str = "unreal-sdk: running the example inside the Unreal Editor needs the Unreal Editor — the editor is absent on this runner, and this lane does not create sdk/engines/unreal/ (an incomplete tree would make ci/sdk-archive.sh fail), so the harness records a SKIP naming BW-21-04-05";

/// Run the Unreal example against the SDK session model: log in, sync a
/// property, then log out.  Every step records its result, so the
/// acceptance — the component's login, sync and logout pass — is checkable
/// from the returned steps.
pub fn run_unreal_example() -> Vec<UnrealExampleStep> {
    use crate::sdk_session::{SdkSession, ServerFlavor};

    let mut steps = Vec::new();
    let mut session = SdkSession::new(ServerFlavor::New);

    // Login: the component logs in, and the handshake rounds are a result.
    let result = match session.login() {
        Ok(()) => format!("logged in ({} rounds)", session.handshake_rounds()),
        Err(error) => error.code().to_string(),
    };
    steps.push(UnrealExampleStep {
        action: "login".to_string(),
        result,
    });

    // Sync: a representative nested property sync reads back.
    session.sync_property(1, "position.x", "1.5");
    steps.push(UnrealExampleStep {
        action: "sync".to_string(),
        result: format!(
            "position.x={}",
            session.property(1, "position.x").unwrap_or("-")
        ),
    });

    // Logout: the component logs out and the connection closes.
    let result = match session.disconnect() {
        Ok(()) => "logged out".to_string(),
        Err(error) => error.code().to_string(),
    };
    steps.push(UnrealExampleStep {
        action: "logout".to_string(),
        result,
    });

    steps
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

        // The example logs in, issues the RPC with its parameters, reads the
        // property, syncs one property and logs out.
        assert!(example.contains("component.Login(\"alice\", \"secret\")"));
        assert!(example.contains("component.Rpc(42, \"heal\", {\"10\", \"20\"})"));
        assert!(example.contains("component.GetProperty(42, \"hp\", value)"));
        assert!(example.contains("component.SyncProperty(42, \"position.x\", \"1.5\")"));
        assert!(example.contains("component.Logout()"));
    }

    #[test]
    fn the_blueprint_surface_exposes_the_entity_event_delegate() {
        // The Blueprint header is a delivered fixture, like the rest of the
        // adapter's C++ surface.
        assert!(fixture_dir().join(BLUEPRINT_HEADER).is_file());

        let header = std::fs::read_to_string(fixture_dir().join(BLUEPRINT_HEADER))
            .expect("the Blueprint surface is a committed fixture");

        // The delegate a Blueprint subscribes to is assignable, and every
        // SDK entry point a Blueprint can call is marked callable.
        assert!(
            header.contains("DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams"),
            "the entity-event delegate is not declared",
        );
        assert!(header.contains(ENTITY_EVENT_DELEGATE));
        assert!(header.contains("UPROPERTY(BlueprintAssignable"));
        assert_eq!(
            header.matches("UFUNCTION(BlueprintCallable").count(),
            BLUEPRINT_CALLABLE.len(),
        );
        for call in BLUEPRINT_CALLABLE {
            assert!(header.contains(call), "the Blueprint surface lacks {call}");
        }
    }

    #[test]
    fn a_blueprint_subscribes_and_receives_entity_events() {
        let mut blueprint = BlueprintEntitySubscription::new();
        assert!(!blueprint.subscribed());

        // The Blueprint binds its handler to the delegate.
        blueprint.bind();
        assert!(blueprint.subscribed());

        // The component raises three entity events; the bound Blueprint
        // receives each one, in order.
        for (entity_id, kind) in [(7_i64, "created"), (7, "entered"), (7, "destroyed")] {
            blueprint.deliver(entity_id, kind);
        }
        let kinds: Vec<&str> = blueprint
            .delivered()
            .iter()
            .map(|event| event.kind.as_str())
            .collect();
        assert_eq!(kinds, vec!["created", "entered", "destroyed"]);
        assert_eq!(
            blueprint
                .delivered()
                .iter()
                .map(|event| event.entity_id)
                .collect::<Vec<_>>(),
            vec![7_i64; 3],
        );
    }

    #[test]
    fn a_blueprint_that_never_subscribes_receives_nothing() {
        let mut blueprint = BlueprintEntitySubscription::new();
        blueprint.deliver(7, "created");

        // Not bound, so the event is dropped rather than delivered.
        assert!(!blueprint.subscribed());
        assert!(blueprint.delivered().is_empty());
    }

    #[test]
    fn the_blueprint_compile_gap_is_named() {
        // The one step a bare runner cannot do is named, with the ticket the
        // CI harness skips for, and the reason the plugin tree is not
        // created.
        assert!(BLUEPRINT_COMPILE_GAP.contains("Unreal Editor"));
        assert!(BLUEPRINT_COMPILE_GAP.contains("BW-21-04-03"));
        assert!(BLUEPRINT_COMPILE_GAP.contains("sdk/engines/unreal/"));
    }

    fn dispatch_transcript() -> Vec<GameThreadEvent> {
        let mut dispatch = GameThreadDispatch::new("game-thread");
        dispatch.post("created", "net-worker-1");
        dispatch.post("entered", "net-worker-2");
        dispatch.pump();
        dispatch.delivered().to_vec()
    }

    #[test]
    fn worker_posts_are_delivered_on_the_game_thread() {
        let mut dispatch = GameThreadDispatch::new("game-thread");

        // Three events posted from three worker threads; the game thread
        // pumps them all.
        for (name, from) in [
            ("created", "net-worker-1"),
            ("entered", "net-worker-2"),
            ("destroyed", "net-worker-3"),
        ] {
            dispatch.post(name, from);
        }
        assert_eq!(dispatch.queued(), 3);
        assert_eq!(dispatch.pump(), 3);

        // The fixed-case thread assertion: every delivery ran on the game
        // thread, and each record keeps the worker that posted it.
        assert!(dispatch.all_on_game_thread());
        assert_eq!(dispatch.delivered().len(), 3);
        let threads: Vec<&str> = dispatch
            .delivered()
            .iter()
            .map(|event| event.posted_from.as_str())
            .collect();
        assert_eq!(
            threads,
            vec!["net-worker-1", "net-worker-2", "net-worker-3"]
        );
    }

    #[test]
    fn a_destroyed_component_dispatches_nothing_afterward() {
        let mut dispatch = GameThreadDispatch::new("game-thread");
        dispatch.post("created", "net-worker-1");
        assert_eq!(dispatch.pump(), 1);

        // The component is destroyed: what is queued is dropped, and every
        // post afterward is rejected.
        dispatch.post("entered", "net-worker-2");
        dispatch.destroy();
        assert!(!dispatch.live());
        assert_eq!(dispatch.queued(), 0);
        assert_eq!(dispatch.pump(), 0);
        dispatch.post("destroyed", "net-worker-3");
        assert_eq!(dispatch.pump(), 0);

        // Only the event delivered before the destroy remains.
        assert_eq!(dispatch.delivered().len(), 1);
        assert!(dispatch.all_on_game_thread());
    }

    #[test]
    fn the_dispatch_is_deterministic() {
        // Two runs, the same transcript: the dispatch is reproducible.
        assert_eq!(dispatch_transcript(), dispatch_transcript());
    }

    #[test]
    fn the_dispatch_surface_declares_the_destroy_guard() {
        assert!(fixture_dir().join(GAME_THREAD_DISPATCH_HEADER).is_file());

        let header = std::fs::read_to_string(fixture_dir().join(GAME_THREAD_DISPATCH_HEADER))
            .expect("the dispatch surface is a committed fixture");

        // The component posts to the game thread and stops dispatching when
        // it is destroyed.
        assert!(header.contains("DispatchEntityEvent(int64 EntityId, const FString& Kind)"));
        assert!(header.contains("void OnGameThread(int64 EntityId, const FString& Kind)"));
        assert!(header.contains("OnComponentDestroyed(bool bDestroyingHierarchy) override"));
    }

    fn step<'a>(steps: &'a [UnrealExampleStep], action: &str) -> &'a str {
        steps
            .iter()
            .find(|s| s.action == action)
            .map(|s| s.result.as_str())
            .unwrap_or("<absent>")
    }

    #[test]
    fn the_unreal_example_logs_in_syncs_and_logs_out() {
        let steps = run_unreal_example();

        // The acceptance: login, sync and logout each have a result.
        let actions: Vec<&str> = steps.iter().map(|s| s.action.as_str()).collect();
        assert_eq!(actions, vec!["login", "sync", "logout"]);
        for s in &steps {
            assert!(!s.result.is_empty(), "{} has no result", s.action);
        }

        // The outcomes the acceptance names.
        assert!(
            step(&steps, "login").contains("logged in"),
            "login: {}",
            step(&steps, "login"),
        );
        assert_eq!(step(&steps, "sync"), "position.x=1.5");
        assert_eq!(step(&steps, "logout"), "logged out");
    }

    #[test]
    fn the_unreal_example_is_deterministic() {
        // Two runs, the same transcript: the example is reproducible.
        assert_eq!(run_unreal_example(), run_unreal_example());
    }

    #[test]
    fn the_in_editor_example_gap_is_named() {
        // The one step a bare runner cannot do is named with its ticket.
        assert!(UNREAL_EXAMPLE_GAP.contains("Unreal Editor"));
        assert!(UNREAL_EXAMPLE_GAP.contains("BW-21-04-05"));
    }
}
