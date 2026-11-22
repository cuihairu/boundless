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

/// The diagnostic's target registry and its query surface.
#[derive(Debug, Default)]
pub struct Diagnostic {
    targets: Vec<DiagnosticTarget>,
}

impl Diagnostic {
    /// A diagnostic with no targets.
    pub fn new() -> Self {
        Self::default()
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
}
