//! BW-25-03-01: showing a process as not ready or as overloaded.
//!
//! A fault view derives the conditions a process is seen in from its
//! collected status, and the conditions stay separate: a process that never
//! announced readiness is not ready, a process reporting a load above its
//! budget is overloaded, and each is visible on its own — with the value the
//! condition was measured from, so a presentation never merges them.

use crate::process_status::StatusCollector;

/// One fault condition a process can be seen in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// The process never announced readiness.
    NotReady,
    /// The process reports a load above its budget.
    Overloaded,
}

impl FaultKind {
    /// The label a presentation shows for the condition.
    pub fn label(self) -> &'static str {
        match self {
            FaultKind::NotReady => "not ready",
            FaultKind::Overloaded => "overloaded",
        }
    }
}

/// One observed fault: the condition, the value it was measured from, and
/// when it was observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// The condition the process is in.
    pub kind: FaultKind,
    /// The value the condition was measured from.
    pub value: String,
    /// When the condition was observed.
    pub observed_at: u64,
}

/// Every fault a process shows.
#[derive(Debug, Default)]
pub struct FaultReport {
    /// The process the report is about.
    pub role: String,
    /// The faults it shows, not ready first.
    pub faults: Vec<Fault>,
}

impl FaultReport {
    /// Whether the process shows no fault at all.
    pub fn is_healthy(&self) -> bool {
        self.faults.is_empty()
    }

    /// Whether it shows a given condition.
    pub fn shows(&self, kind: FaultKind) -> bool {
        self.faults.iter().any(|fault| fault.kind == kind)
    }

    /// The line a presentation prints: the role and its conditions, in order.
    pub fn render(&self) -> String {
        if self.faults.is_empty() {
            return format!("{}: ok", self.role);
        }
        let labels: Vec<&str> = self.faults.iter().map(|fault| fault.kind.label()).collect();
        format!("{}: {}", self.role, labels.join(", "))
    }
}

/// The fault view over a cluster's collected status. It derives two
/// independent conditions per process: readiness never announced, and a load
/// above its budget.
pub struct FaultView<'a> {
    status: &'a StatusCollector,
    load_budget: u64,
}

impl<'a> FaultView<'a> {
    /// A view with a load budget of 100 — a load is a percent of it.
    pub fn new(status: &'a StatusCollector) -> Self {
        Self::with_budget(status, 100)
    }

    /// A view comparing reported loads against `load_budget`.
    pub fn with_budget(status: &'a StatusCollector, load_budget: u64) -> Self {
        Self {
            status,
            load_budget,
        }
    }

    /// Report the faults of every process, ascending by role.
    pub fn reports(&self) -> Vec<FaultReport> {
        self.status
            .roles()
            .into_iter()
            .map(|role| self.report(role))
            .collect()
    }

    /// The faults one process shows.
    pub fn report(&self, role: &str) -> FaultReport {
        let mut faults = Vec::new();
        let Some(status) = self.status.status(role) else {
            return FaultReport {
                role: role.to_string(),
                faults,
            };
        };
        if !status.is_ready() {
            faults.push(Fault {
                kind: FaultKind::NotReady,
                value: "no readiness announcement".to_string(),
                observed_at: self.status.now(),
            });
        }
        let load = status
            .reading("load")
            .and_then(|reading| reading.value.parse::<u64>().ok());
        if load.is_some_and(|load| load > self.load_budget) {
            faults.push(Fault {
                kind: FaultKind::Overloaded,
                value: load.unwrap().to_string(),
                observed_at: status.reading("load").unwrap().collected_at,
            });
        }
        FaultReport {
            role: role.to_string(),
            faults,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_ready_and_overloaded_are_each_visible_on_their_own() {
        let mut status = StatusCollector::with_clock(|| 1_000);
        status
            .register("cellapp01", Some(100))
            .register("cellapp02", Some(101));
        // cellapp01 never announces readiness but is lightly loaded; cellapp02
        // is ready but reports a load well above the budget.
        status.record("cellapp01", "load", "60", "process load");
        status.record("cellapp02", "ready", "true", "readiness announcement");
        status.record("cellapp02", "load", "140", "process load");

        let view = FaultView::new(&status);

        // Each condition is visible by itself, and neither one implies the
        // other.
        let not_ready = view.report("cellapp01");
        assert!(not_ready.shows(FaultKind::NotReady));
        assert!(!not_ready.shows(FaultKind::Overloaded));
        assert_eq!(not_ready.render(), "cellapp01: not ready");

        let overloaded = view.report("cellapp02");
        assert!(overloaded.shows(FaultKind::Overloaded));
        assert!(!overloaded.shows(FaultKind::NotReady));
        assert_eq!(overloaded.render(), "cellapp02: overloaded");
    }

    #[test]
    fn a_process_can_show_both_conditions_with_their_own_values() {
        let mut status = StatusCollector::with_clock(|| 2_000);
        status.register("loginapp01", Some(300));
        status.record("loginapp01", "load", "220", "process load");

        let view = FaultView::new(&status);
        let report = view.report("loginapp01");

        // Both conditions show, each carrying the value it was measured from.
        assert_eq!(report.faults.len(), 2);
        assert_eq!(report.faults[0].kind, FaultKind::NotReady);
        assert_eq!(report.faults[0].value, "no readiness announcement");
        assert_eq!(report.faults[0].observed_at, 2_000);
        assert_eq!(report.faults[1].kind, FaultKind::Overloaded);
        assert_eq!(report.faults[1].value, "220");
        assert_eq!(report.faults[1].observed_at, 2_000);
        assert_eq!(report.render(), "loginapp01: not ready, overloaded");
    }

    #[test]
    fn a_ready_process_within_its_budget_is_healthy() {
        let mut status = StatusCollector::with_clock(|| 0);
        status.register("baseapp01", Some(200));
        status.record("baseapp01", "ready", "true", "readiness announcement");
        status.record("baseapp01", "load", "95", "process load");

        let view = FaultView::new(&status);
        let report = view.report("baseapp01");
        assert!(report.is_healthy());
        assert_eq!(report.render(), "baseapp01: ok");
        assert_eq!(view.reports().len(), 1);

        // The same load is a fault under a tighter budget.
        let tight = FaultView::with_budget(&status, 80);
        assert!(tight.report("baseapp01").shows(FaultKind::Overloaded));
    }
}
