//! BW-25-02-05: linking profile artifacts into the diagnostic output.
//!
//! A profiling run is taken per process and named by a session id. The id is
//! sampled into the process's collected status, so the diagnostic output —
//! the JSON snapshot a viewer parses — carries the session id of the profile
//! artifact that belongs to the process being diagnosed: an operator reads
//! the id off the diagnosis and pulls the artifact it names.

use crate::process_status::StatusCollector;
use std::collections::BTreeMap;

/// One profiling run: the process it profiled, its session id, and when it
/// ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRun {
    /// The session id naming the run's artifact.
    pub session: String,
    /// The process the run profiled.
    pub role: String,
    /// When the run began.
    pub started_at: u64,
    /// When the run ended, when it has.
    pub ended_at: Option<u64>,
}

impl ProfileRun {
    /// Whether the run has ended.
    pub fn is_finished(&self) -> bool {
        self.ended_at.is_some()
    }
}

/// The profiling runs taken across the cluster. Each run is named by a
/// session id minted in the order the runs began, so every run's artifact is
/// named and no two runs share an id.
#[derive(Default)]
pub struct ProfileSessions {
    runs: BTreeMap<String, Vec<ProfileRun>>,
    next_serial: u64,
}

impl ProfileSessions {
    /// No runs yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a profiling run on a process, minting the run's session id.
    pub fn begin(&mut self, role: &str, at: u64) -> String {
        self.next_serial += 1;
        let session = format!("profile-{:04}", self.next_serial);
        self.runs
            .entry(role.to_string())
            .or_default()
            .push(ProfileRun {
                session: session.clone(),
                role: role.to_string(),
                started_at: at,
                ended_at: None,
            });
        session
    }

    /// End a process's open run, when one is open. Returns the session id of
    /// the run that ended.
    pub fn end(&mut self, role: &str, at: u64) -> Option<String> {
        let runs = self.runs.get_mut(role)?;
        let open = runs.iter_mut().rev().find(|run| run.ended_at.is_none())?;
        open.ended_at = Some(at);
        Some(open.session.clone())
    }

    /// A process's latest run, when it has one.
    pub fn latest(&self, role: &str) -> Option<&ProfileRun> {
        self.runs.get(role).and_then(|runs| runs.last())
    }

    /// The run a session id names, across every process.
    pub fn by_session(&self, session: &str) -> Option<&ProfileRun> {
        self.runs
            .values()
            .flatten()
            .find(|run| run.session == session)
    }

    /// Sample every profiled process's latest session id into its status, so
    /// the diagnostic output carries it. Each sample replaces the session a
    /// previous one left, so the current run's id is what shows.
    pub fn record_sessions(&self, collector: &mut StatusCollector) -> &Self {
        for role in self.runs.keys() {
            if let Some(run) = self.latest(role) {
                collector.record_latest(role, "profile.session", &run.session, "profile run");
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster_query::ClusterQuery;

    #[test]
    fn profile_runs_are_named_by_minted_session_ids() {
        let mut sessions = ProfileSessions::new();

        // Two runs, one per process: each is named, in the order it began.
        let cell = sessions.begin("cellapp01", 100);
        let base = sessions.begin("baseapp01", 150);
        assert_eq!(cell, "profile-0001");
        assert_eq!(base, "profile-0002");

        // Ending a run closes it and names the session it ended.
        assert_eq!(
            sessions.end("cellapp01", 200).as_deref(),
            Some("profile-0001")
        );
        let run = sessions.latest("cellapp01").unwrap();
        assert!(run.is_finished());
        assert_eq!(run.ended_at, Some(200));

        // The session ids resolve the artifact, across processes.
        let named = sessions.by_session("profile-0002").unwrap();
        assert_eq!(named.role, "baseapp01");
        assert_eq!(named.started_at, 150);
        assert!(!named.is_finished());

        // A process with no open run ends nothing.
        assert_eq!(
            sessions.end("baseapp01", 300).as_deref(),
            Some("profile-0002")
        );
        assert_eq!(sessions.end("cellapp01", 400), None);
        assert!(sessions.by_session("profile-9999").is_none());
    }

    #[test]
    fn the_diagnostic_output_carries_the_session_id() {
        let mut sessions = ProfileSessions::new();
        let session = sessions.begin("cellapp01", 1_000);
        let mut collector = StatusCollector::with_clock(|| 1_000);
        collector.register("cellapp01", Some(100));

        // The session id is sampled into the process's status.
        sessions.record_sessions(&mut collector);
        let status = collector.status("cellapp01").unwrap();
        assert_eq!(status.reading("profile.session").unwrap().value, session);

        // The diagnostic output — the JSON snapshot — carries the id, so a
        // viewer reads the artifact id off the diagnosis.
        let doc = ClusterQuery::new(&collector).snapshot_json();
        assert!(doc.contains("\"profile.session\""));
        assert!(doc.contains("profile-0001"));

        // The refresh view surfaces the id as an update too.
        let updates = ClusterQuery::new(&collector).updates_since(0);
        let session_update = updates
            .iter()
            .find(|update| update.reading.item == "profile.session")
            .unwrap();
        assert_eq!(session_update.reading.value, "profile-0001");
    }

    #[test]
    fn a_reprofiled_process_carries_its_latest_session() {
        let mut sessions = ProfileSessions::new();
        sessions.begin("cellapp01", 100);
        let mut collector = StatusCollector::with_clock(|| 100);
        collector.register("cellapp01", Some(100));
        sessions.record_sessions(&mut collector);

        // The process is profiled again: a new run, a new session id.
        let second = sessions.begin("cellapp01", 200);
        assert_eq!(second, "profile-0002");
        sessions.record_sessions(&mut collector);

        // The latest sample stands in: the old session no longer shows.
        let status = collector.status("cellapp01").unwrap();
        assert_eq!(
            status.reading("profile.session").unwrap().value,
            "profile-0002"
        );
        let doc = ClusterQuery::new(&collector).snapshot_json();
        assert!(doc.contains("profile-0002"));
        assert!(!doc.contains("profile-0001"));
    }
}
