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

/// A point-in-time capture of a target's memory statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// Which target was snapshotted.
    pub target: String,
    /// When the snapshot was taken.
    pub collected_at: u64,
    /// The captured statistics, field name then value.
    pub readings: Vec<(String, u64)>,
}

impl MemorySnapshot {
    /// The snapshot's stable identity: a hex digest of its content, so the
    /// same readings collected at the same time always yield the same id. Two
    /// snapshots with identical ids are interchangeable; a changed reading
    /// changes the id.
    pub fn id(&self) -> String {
        let mut hasher = FnvHasher::new();
        hasher.write_str(&self.target);
        hasher.write_u64(self.collected_at);
        for (name, value) in &self.readings {
            hasher.write_str(name);
            hasher.write_u64(*value);
        }
        hasher.finish_hex()
    }

    /// A reading by field name, when the snapshot carries it.
    pub fn reading(&self, field: &str) -> Option<u64> {
        self.readings
            .iter()
            .find(|(name, _)| name == field)
            .map(|(_, value)| *value)
    }
}

/// A 64-bit FNV-1a over the snapshot's canonical byte stream, hex-encoded: a
/// deterministic digest with no allocation in the hot loop.
struct FnvHasher {
    state: u64,
}

impl FnvHasher {
    fn new() -> Self {
        Self {
            state: 0xcbf29ce484222325,
        }
    }

    fn write_u64(&mut self, value: u64) {
        let bytes = value.to_le_bytes();
        for byte in bytes {
            self.state ^= byte as u64;
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn write_str(&mut self, value: &str) {
        for byte in value.as_bytes() {
            self.state ^= *byte as u64;
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn finish_hex(&self) -> String {
        format!("{:016x}", self.state)
    }
}

/// One field's movement between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDelta {
    /// The field's name.
    pub field: String,
    /// The value in the earlier snapshot, or zero when it had no such field.
    pub before: u64,
    /// The value in the later snapshot, or zero when it dropped the field.
    pub after: u64,
    /// Later minus earlier, signed.
    pub delta: i64,
}

/// How two snapshots differ, shown field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDelta {
    /// The target both snapshots describe.
    pub target: String,
    /// The earlier snapshot's stable id.
    pub before_id: String,
    /// The later snapshot's stable id.
    pub after_id: String,
    /// One entry per field present in either snapshot, in ascending field
    /// order so the difference reads the same way every time.
    pub fields: Vec<FieldDelta>,
}

/// Compare two snapshots of the same target field by field: each field present
/// in either snapshot gets its before and after values and the signed movement
/// between them. Fields in only one snapshot read as zero on the other side.
pub fn compare_snapshots(before: &MemorySnapshot, after: &MemorySnapshot) -> SnapshotDelta {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for (name, _) in &before.readings {
        names.insert(name.as_str());
    }
    for (name, _) in &after.readings {
        names.insert(name.as_str());
    }
    let fields = names
        .into_iter()
        .map(|name| {
            let before_value = before.reading(name).unwrap_or(0);
            let after_value = after.reading(name).unwrap_or(0);
            FieldDelta {
                field: name.to_string(),
                before: before_value,
                after: after_value,
                delta: signed_delta(before_value, after_value),
            }
        })
        .collect();
    SnapshotDelta {
        target: after.target.clone(),
        before_id: before.id(),
        after_id: after.id(),
        fields,
    }
}

/// The signed movement from one reading to another, saturating at the edges
/// of `i64` (readings never approach it in practice).
fn signed_delta(from: u64, to: u64) -> i64 {
    if to >= from {
        i64::try_from(to - from).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(from - to).unwrap_or(i64::MAX)
    }
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
        let found = self.authorized_target(authorization, target)?;
        let readings = self.readings.get(target).cloned().unwrap_or_default();
        Ok(QueryResult {
            target: found.name.clone(),
            kind: found.kind,
            pid: found.pid,
            collected_at: (self.clock)(),
            readings,
        })
    }

    /// Capture a memory snapshot of a target, carrying its statistics and when
    /// they were collected. The snapshot's [`MemorySnapshot::id`] is stable:
    /// the same content always yields the same id.
    pub fn snapshot(
        &self,
        authorization: &Authorization,
        target: &str,
    ) -> Result<MemorySnapshot, QueryError> {
        let found = self.authorized_target(authorization, target)?;
        let readings = self.readings.get(target).cloned().unwrap_or_default();
        Ok(MemorySnapshot {
            target: found.name.clone(),
            collected_at: (self.clock)(),
            readings,
        })
    }

    /// Compare two snapshots of the same target, field by field. Both targets
    /// must be authorized, so the read boundary holds here too.
    pub fn compare(
        &self,
        authorization: &Authorization,
        before: &MemorySnapshot,
        after: &MemorySnapshot,
    ) -> Result<SnapshotDelta, QueryError> {
        self.authorized_target(authorization, &before.target)?;
        self.authorized_target(authorization, &after.target)?;
        Ok(compare_snapshots(before, after))
    }

    /// The access boundary shared by every read: an unauthorized target is
    /// refused before anything is looked at, so the interface stays read-only
    /// for callers outside the grant.
    fn authorized_target<'a>(
        &'a self,
        authorization: &Authorization,
        target: &str,
    ) -> Result<&'a DiagnosticTarget, QueryError> {
        if !authorization.is_granted(target) {
            return Err(QueryError::AccessDenied);
        }
        self.targets
            .iter()
            .find(|registered| registered.name == target)
            .ok_or(QueryError::UnknownTarget)
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

    #[test]
    fn snapshotting_a_target_returns_a_stable_id() {
        let mut diagnostic = Diagnostic::with_clock(|| 99);
        diagnostic.add_process("cellapp01", 100).set_readings(
            "cellapp01",
            vec![("rss_bytes", 8_000_000), ("entities", 512)],
        );
        let mut auth = Authorization::new();
        auth.grant("cellapp01");

        let snapshot = diagnostic.snapshot(&auth, "cellapp01").unwrap();
        assert_eq!(snapshot.target, "cellapp01");
        assert_eq!(snapshot.collected_at, 99);
        assert_eq!(snapshot.reading("rss_bytes"), Some(8_000_000));
        assert_eq!(snapshot.reading("entities"), Some(512));
        assert_eq!(snapshot.reading("missing"), None);

        // The id is stable: the same content yields the same id, and it is a
        // compact hex digest.
        let again = diagnostic.snapshot(&auth, "cellapp01").unwrap();
        assert_eq!(snapshot.id(), again.id());
        assert_eq!(snapshot.id().len(), 16);
        assert!(snapshot.id().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_changed_reading_changes_the_snapshot_id() {
        let mut diagnostic = Diagnostic::with_clock(|| 5);
        diagnostic
            .add_process("cellapp01", 100)
            .set_readings("cellapp01", vec![("rss_bytes", 8_000_000)]);
        let mut auth = Authorization::new();
        auth.grant("cellapp01");

        let first = diagnostic.snapshot(&auth, "cellapp01").unwrap();
        diagnostic.set_readings("cellapp01", vec![("rss_bytes", 9_000_000)]);
        let second = diagnostic.snapshot(&auth, "cellapp01").unwrap();
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn an_unauthorized_snapshot_is_refused() {
        let diagnostic = diagnostic();
        let auth = Authorization::new();
        assert_eq!(
            diagnostic.snapshot(&auth, "cellapp01"),
            Err(QueryError::AccessDenied)
        );
    }

    #[test]
    fn comparing_two_snapshots_shows_each_field_delta() {
        let before = MemorySnapshot {
            target: "cellapp01".to_string(),
            collected_at: 10,
            readings: vec![
                ("entities".to_string(), 512),
                ("rss_bytes".to_string(), 8_000_000),
                ("python_objects".to_string(), 64),
            ],
        };
        let after = MemorySnapshot {
            target: "cellapp01".to_string(),
            collected_at: 20,
            readings: vec![
                ("caches".to_string(), 8),
                ("entities".to_string(), 600),
                ("rss_bytes".to_string(), 9_000_000),
            ],
        };

        let delta = compare_snapshots(&before, &after);
        assert_eq!(delta.target, "cellapp01");
        assert_eq!(delta.before_id, before.id());
        assert_eq!(delta.after_id, after.id());

        // Field by field, ascending, with a delta for every field in either
        // snapshot; a field only in one side reads as zero on the other.
        let shown: Vec<(&str, u64, u64, i64)> = delta
            .fields
            .iter()
            .map(|f| (f.field.as_str(), f.before, f.after, f.delta))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("caches", 0, 8, 8),
                ("entities", 512, 600, 88),
                ("python_objects", 64, 0, -64),
                ("rss_bytes", 8_000_000, 9_000_000, 1_000_000),
            ]
        );
    }

    #[test]
    fn comparing_snapshots_of_same_readings_reports_zero_deltas() {
        let before = MemorySnapshot {
            target: "cellapp01".to_string(),
            collected_at: 10,
            readings: vec![("rss_bytes".to_string(), 8_000_000)],
        };
        let after = MemorySnapshot {
            target: "cellapp01".to_string(),
            collected_at: 20,
            readings: vec![("rss_bytes".to_string(), 8_000_000)],
        };
        let delta = compare_snapshots(&before, &after);
        assert_eq!(delta.fields.len(), 1);
        assert_eq!(delta.fields[0].field, "rss_bytes");
        assert_eq!(delta.fields[0].delta, 0);
    }

    #[test]
    fn compare_via_the_diagnostic_enforces_authorization() {
        let mut diagnostic = Diagnostic::with_clock(|| 3);
        diagnostic
            .add_process("cellapp01", 100)
            .set_readings("cellapp01", vec![("rss_bytes", 8_000_000)]);
        let auth = Authorization::new();

        let before = MemorySnapshot {
            target: "cellapp01".to_string(),
            collected_at: 1,
            readings: vec![("rss_bytes".to_string(), 8_000_000)],
        };
        let after = MemorySnapshot {
            target: "cellapp01".to_string(),
            collected_at: 2,
            readings: vec![("rss_bytes".to_string(), 9_000_000)],
        };
        assert_eq!(
            diagnostic.compare(&auth, &before, &after),
            Err(QueryError::AccessDenied)
        );

        // Granting the target lets the same read through.
        let mut granted = auth;
        granted.grant("cellapp01");
        let delta = diagnostic.compare(&granted, &before, &after).unwrap();
        assert_eq!(delta.fields[0].delta, 1_000_000);
    }
}
