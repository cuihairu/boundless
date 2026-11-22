//! BW-24-02: the interactive diagnostic query interface.
//!
//! The diagnostic answers a local interface's questions the way the reference
//! answers its Telnet console (`telnet_handler.cpp`) and the local diagnostic
//! interface of the port: list the queryable targets, query one by process or
//! subsystem, generate a memory snapshot, compare two snapshots, export the
//! structured statistics, and enforce the access boundary. The default scope
//! is the local machine and read-only; access beyond what is granted is
//! refused (BW-24-02-06).

use std::collections::BTreeSet;

/// What a diagnostic target is: a process the diagnostic can reach, or a
/// subsystem inside the process it reports on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A process, named by its role (a `cellapp`, a `baseapp`).
    Process,
    /// A subsystem inside a process (the entity system, the Python layer).
    Subsystem,
}

/// One target the diagnostic can be asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticTarget {
    /// The target's name (a role or subsystem name).
    pub name: String,
    /// What kind of target it is.
    pub kind: TargetKind,
    /// The process id, when the target is a process.
    pub pid: Option<u32>,
}

/// What the caller is allowed to see. By default nothing is granted, so a
/// caller sees only what the operator authorized: the least privilege the
/// access boundary is built on.
#[derive(Debug, Default)]
pub struct Authorization {
    granted: BTreeSet<String>,
}

impl Authorization {
    /// An authorization with no targets granted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant access to a target.
    pub fn grant(&mut self, target: &str) {
        self.granted.insert(target.to_string());
    }

    /// Whether access to a target is granted.
    pub fn is_granted(&self, target: &str) -> bool {
        self.granted.contains(target)
    }

    /// The granted target names, ascending.
    pub fn granted(&self) -> Vec<&str> {
        self.granted.iter().map(String::as_str).collect()
    }
}

/// Why a query was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    /// The caller was not granted access to the target.
    AccessDenied,
    /// No such target is registered.
    UnknownTarget,
}

/// What a query returns: the target's identity, when it was collected, and
/// its structured statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResult {
    /// The queried target's name.
    pub target: String,
    /// What kind of target it is.
    pub kind: TargetKind,
    /// The process id, when the target is a process.
    pub pid: Option<u32>,
    /// When the statistics were collected.
    pub collected_at: u64,
    /// The structured statistics, name then value.
    pub readings: Vec<(String, u64)>,
}

/// The diagnostic's target registry and its query surface.
pub struct Diagnostic {
    targets: Vec<DiagnosticTarget>,
    readings: std::collections::BTreeMap<String, Vec<(String, u64)>>,
    clock: Box<dyn Fn() -> u64>,
}

impl Default for Diagnostic {
    fn default() -> Self {
        Self::new()
    }
}

impl Diagnostic {
    /// A diagnostic with no targets, stamping collection times from the
    /// wall clock.
    pub fn new() -> Self {
        Self::with_clock(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos() as u64)
                .unwrap_or(0)
        })
    }

    /// A diagnostic whose collection time comes from `clock`, so a test can
    /// drive when a query was made.
    pub fn with_clock(clock: impl Fn() -> u64 + 'static) -> Self {
        Self {
            targets: Vec::new(),
            readings: std::collections::BTreeMap::new(),
            clock: Box::new(clock),
        }
    }

    /// Register a process target.
    pub fn add_process(&mut self, name: &str, pid: u32) -> &mut Self {
        self.targets.push(DiagnosticTarget {
            name: name.to_string(),
            kind: TargetKind::Process,
            pid: Some(pid),
        });
        self
    }

    /// Register a subsystem target.
    pub fn add_subsystem(&mut self, name: &str) -> &mut Self {
        self.targets.push(DiagnosticTarget {
            name: name.to_string(),
            kind: TargetKind::Subsystem,
            pid: None,
        });
        self
    }

    /// Attach structured statistics to a target, what a query of it returns.
    pub fn set_readings(&mut self, target: &str, readings: Vec<(&str, u64)>) -> &mut Self {
        let readings = readings
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect();
        self.readings.insert(target.to_string(), readings);
        self
    }

    /// The local diagnostic targets the caller is authorized to see: the
    /// default scope is the local machine, and only granted targets are
    /// listed. Ordered by name so the output is stable.
    pub fn list_local(&self, authorization: &Authorization) -> Vec<&DiagnosticTarget> {
        let mut visible: Vec<&DiagnosticTarget> = self
            .targets
            .iter()
            .filter(|target| authorization.is_granted(&target.name))
            .collect();
        visible.sort_by(|a, b| a.name.cmp(&b.name));
        visible
    }

    /// Query a selected process or subsystem. The caller must be authorized;
    /// the result names the target, when it was collected, and the target's
    /// statistics.
    pub fn query(
        &self,
        authorization: &Authorization,
        target: &str,
    ) -> Result<QueryResult, QueryError> {
        if !authorization.is_granted(target) {
            return Err(QueryError::AccessDenied);
        }
        let found = self
            .targets
            .iter()
            .find(|registered| registered.name == target)
            .ok_or(QueryError::UnknownTarget)?;
        let readings = self.readings.get(target).cloned().unwrap_or_default();
        Ok(QueryResult {
            target: found.name.clone(),
            kind: found.kind,
            pid: found.pid,
            collected_at: (self.clock)(),
            readings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic() -> Diagnostic {
        let mut diagnostic = Diagnostic::new();
        diagnostic
            .add_process("cellapp01", 100)
            .add_process("baseapp01", 200)
            .add_subsystem("entities")
            .add_subsystem("python");
        diagnostic
    }

    #[test]
    fn an_unauthorized_caller_sees_nothing() {
        let diagnostic = diagnostic();
        let auth = Authorization::new();
        assert_eq!(
            diagnostic.list_local(&auth),
            Vec::<&DiagnosticTarget>::new()
        );
        assert_eq!(auth.granted(), Vec::<&str>::new());
    }

    #[test]
    fn only_granted_targets_are_listed() {
        let diagnostic = diagnostic();
        let mut auth = Authorization::new();
        auth.grant("cellapp01");
        let visible = diagnostic.list_local(&auth);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "cellapp01");
        assert_eq!(visible[0].kind, TargetKind::Process);
        assert_eq!(visible[0].pid, Some(100));
        assert_eq!(auth.granted(), vec!["cellapp01"]);
    }

    #[test]
    fn granted_targets_come_out_sorted_and_filtered() {
        let diagnostic = diagnostic();
        let mut auth = Authorization::new();
        // Grant a subsystem and a process; the unlisted role stays hidden.
        auth.grant("python");
        auth.grant("cellapp01");
        auth.grant("entities");

        let visible = diagnostic.list_local(&auth);
        let names: Vec<&str> = visible.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["cellapp01", "entities", "python"]);

        // The unlisted baseapp was never granted.
        assert!(!auth.is_granted("baseapp01"));
        assert!(visible.iter().all(|t| t.name != "baseapp01"));
    }

    #[test]
    fn querying_a_target_returns_it_with_its_collection_time() {
        // A driven clock stamps when the query was made.
        let mut diagnostic = Diagnostic::with_clock(|| 42);
        diagnostic.add_process("cellapp01", 100).set_readings(
            "cellapp01",
            vec![("rss_bytes", 8_000_000), ("entities", 512)],
        );
        let mut auth = Authorization::new();
        auth.grant("cellapp01");

        let result = diagnostic.query(&auth, "cellapp01").unwrap();
        assert_eq!(result.target, "cellapp01");
        assert_eq!(result.kind, TargetKind::Process);
        assert_eq!(result.pid, Some(100));
        // The acceptance: the result names the target and when it was collected.
        assert_eq!(result.collected_at, 42);
        assert_eq!(
            result.readings,
            vec![
                ("rss_bytes".to_string(), 8_000_000),
                ("entities".to_string(), 512)
            ]
        );
    }

    #[test]
    fn an_unauthorized_query_is_refused() {
        let diagnostic = diagnostic();
        let auth = Authorization::new();
        assert_eq!(
            diagnostic.query(&auth, "cellapp01"),
            Err(QueryError::AccessDenied)
        );
    }

    #[test]
    fn an_unknown_target_is_reported_even_when_granted() {
        let diagnostic = diagnostic();
        let mut auth = Authorization::new();
        auth.grant("missing");
        assert_eq!(
            diagnostic.query(&auth, "missing"),
            Err(QueryError::UnknownTarget)
        );
    }

    #[test]
    fn a_subsystem_query_reports_no_pid_and_no_readings_by_default() {
        let mut diagnostic = Diagnostic::with_clock(|| 7);
        diagnostic.add_subsystem("entities");
        let mut auth = Authorization::new();
        auth.grant("entities");

        let result = diagnostic.query(&auth, "entities").unwrap();
        assert_eq!(result.kind, TargetKind::Subsystem);
        assert_eq!(result.pid, None);
        assert_eq!(result.collected_at, 7);
        assert!(result.readings.is_empty());
    }
}
