//! BW-21-06-04: the unified engine interop fixture — one server, one entity
//! definition and one behavior fixture, run against every engine.
//!
//! The acceptance: each engine has login and sync evidence.  The fixture is
//! the same for all three engines: the SDK session server (the one server
//! every engine connects to), the shared `BaseData` entity fields (`hp`,
//! `position`), and the one behavior the example performs (log in, make an
//! RPC round trip, sync `position.x`, log out).  Unity and Godot drive the
//! SDK session directly and report their steps; Unreal compiles its adapter
//! and runs the example against the scripted server stand-in, reporting its
//! transcript.  The engine-side runs (the Unity Editor, the Godot engine)
//! are recorded as gaps by their own lanes and are not re-gapped here.

/// The entity definition shared by every engine: the base fields.
pub const ENTITY_DEF: &str = "BaseData(hp,position)";

/// The property the shared fixture syncs.
pub const SYNC_PROPERTY: &str = "position.x";

/// The value the shared fixture syncs.
pub const SYNC_VALUE: &str = "1.5";

/// The user the shared fixture logs in as.
pub const LOGON_USER: &str = "alice";

/// One engine's login and sync evidence from the shared fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteropEvidence {
    /// The engine.
    pub engine: &'static str,
    /// Whether the engine logged in.
    pub logged_in: bool,
    /// Whether the engine synced the shared property.
    pub synced: bool,
    /// The login evidence, as the engine reported it.
    pub login_evidence: String,
    /// The sync evidence, as the engine reported it.
    pub sync_evidence: String,
}

/// Run the shared fixture against every engine, in the release order, and
/// collect each engine's login and sync evidence.
pub fn run_interop() -> Vec<InteropEvidence> {
    vec![unity(), unreal(), godot()]
}

/// The Unity evidence: the example's login and property steps.
fn unity() -> InteropEvidence {
    let steps = crate::unity_sdk::run_unity_example();
    let login = steps
        .iter()
        .find(|step| step.action == "login")
        .map(|step| step.result.clone())
        .unwrap_or_default();
    let property = steps
        .iter()
        .find(|step| step.action == "property")
        .map(|step| step.result.clone())
        .unwrap_or_default();
    InteropEvidence {
        engine: "unity",
        logged_in: login.starts_with("connected"),
        synced: property == format!("x={SYNC_VALUE}"),
        login_evidence: login,
        sync_evidence: property,
    }
}

/// The Unreal evidence: the compiled example's transcript.
fn unreal() -> InteropEvidence {
    let run = crate::unreal_sdk::run_cpp_example();
    let stdout = run.map(|run| run.stdout.join("\n")).unwrap_or_default();
    InteropEvidence {
        engine: "unreal",
        logged_in: stdout.contains("LOGIN ok"),
        synced: stdout.contains("SYNC ok"),
        login_evidence: if stdout.contains("LOGIN ok") {
            "LOGIN ok".to_string()
        } else {
            "the example did not run".to_string()
        },
        sync_evidence: if stdout.contains("SYNC ok") {
            "SYNC ok".to_string()
        } else {
            "the example did not run".to_string()
        },
    }
}

/// The Godot evidence: the example's login and sync steps.
fn godot() -> InteropEvidence {
    let steps = crate::godot_sdk::run_godot_example();
    let login = steps
        .iter()
        .find(|step| step.action == "login")
        .map(|step| step.result.clone())
        .unwrap_or_default();
    let sync = steps
        .iter()
        .find(|step| step.action == "sync")
        .map(|step| step.result.clone())
        .unwrap_or_default();
    InteropEvidence {
        engine: "godot",
        logged_in: login.starts_with("logged in"),
        synced: sync == format!("{SYNC_PROPERTY}={SYNC_VALUE}"),
        login_evidence: login,
        sync_evidence: sync,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_engine_runs_the_same_fixture() {
        // The fixture is one server, one entity definition and one behavior
        // for all three engines.
        let evidence = run_interop();
        let engines: Vec<_> = evidence.iter().map(|e| e.engine).collect();
        assert_eq!(engines, vec!["unity", "unreal", "godot"]);
        assert!(ENTITY_DEF.starts_with("BaseData"));
        assert!(ENTITY_DEF.contains("hp") && ENTITY_DEF.contains("position"));
    }

    #[test]
    fn every_engine_has_login_and_sync_evidence() {
        // The acceptance: each engine has login and sync evidence.
        for evidence in run_interop() {
            assert!(
                evidence.logged_in,
                "{} has no login evidence: {}",
                evidence.engine, evidence.login_evidence,
            );
            assert!(
                evidence.synced,
                "{} has no sync evidence: {}",
                evidence.engine, evidence.sync_evidence,
            );
            assert!(!evidence.login_evidence.is_empty());
            assert!(!evidence.sync_evidence.is_empty());
        }
    }

    #[test]
    fn the_synced_property_matches_across_engines() {
        // Every engine synced the same property to the same value.  The
        // session-driven engines print the value they read back; Unreal's
        // transcript prints `SYNC ok` (its example asserts the synced path
        // and value in its own bookkeeping, verified by BW-21-04-02's run).
        for evidence in run_interop() {
            match evidence.engine {
                "unreal" => assert_eq!(evidence.sync_evidence, "SYNC ok"),
                _ => assert!(
                    evidence.sync_evidence.contains(SYNC_VALUE),
                    "{} synced something else: {}",
                    evidence.engine,
                    evidence.sync_evidence,
                ),
            }
        }
    }

    #[test]
    fn the_unreal_evidence_is_a_real_compiled_run() {
        // Unreal's evidence comes from a real compile-and-run, not a model.
        let run = crate::unreal_sdk::run_cpp_example()
            .expect("the C++ toolchain compiles and runs the example");
        assert_eq!(run.status, 0);
        for line in crate::unreal_sdk::EXPECTED_EXAMPLE_TRANSCRIPT {
            assert!(
                run.stdout.iter().any(|printed| printed == *line),
                "the run did not print {line}",
            );
        }
    }
}
