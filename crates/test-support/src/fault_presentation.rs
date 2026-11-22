//! BW-25-03-01 / BW-25-03-02: showing a process as not ready, overloaded, or
//! with a stale heart.
//!
//! A fault view derives the conditions a process is seen in from its
//! collected status, and the conditions stay separate: a process that never
//! announced readiness is not ready, a process reporting a load above its
//! budget is overloaded, and a process whose last beat is older than its
//! timeout has a stale heart. Each is visible on its own — with the value the
//! condition was measured from — so a presentation never merges them, and a
//! fresh beat (even one taken this instant) is never mistaken for a stale
//! one.
//!
//! A cluster query also tolerates partial failure ([`PartialQuery`]): an
//! expected node that did not answer is marked unreachable, and the nodes
//! that did answer keep their results readable. A node on record with no
//! readings is not unreachable — its empty status is a readable zero value.

use crate::process_status::{ProcessStatus, StatusCollector};

/// One fault condition a process can be seen in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// The process never announced readiness.
    NotReady,
    /// The process reports a load above its budget.
    Overloaded,
    /// The process's last beat is older than its heartbeat timeout.
    HeartbeatStale,
}

impl FaultKind {
    /// The label a presentation shows for the condition.
    pub fn label(self) -> &'static str {
        match self {
            FaultKind::NotReady => "not ready",
            FaultKind::Overloaded => "overloaded",
            FaultKind::HeartbeatStale => "heartbeat stale",
        }
    }
}

/// How a process's heart reads right now: on record or not, and whether the
/// last beat is still fresh. A heart that beat this instant is fresh — its
/// age is zero, which is not stale; a heart that never beat is not on record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatState {
    /// No heartbeat was ever recorded.
    None,
    /// The last beat is at most the timeout ago.
    Fresh {
        /// How long ago the last beat was.
        age: u64,
    },
    /// The last beat is older than the timeout.
    Stale {
        /// How long ago the last beat was.
        age: u64,
    },
}

impl HeartbeatState {
    /// Whether the heart is stale.
    pub fn is_stale(&self) -> bool {
        matches!(self, HeartbeatState::Stale { .. })
    }

    /// How long ago the last beat was, when there is one.
    pub fn age(&self) -> Option<u64> {
        match self {
            HeartbeatState::None => None,
            HeartbeatState::Fresh { age } | HeartbeatState::Stale { age } => Some(*age),
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

/// The fault view over a cluster's collected status. It derives three
/// independent conditions per process: readiness never announced, a load
/// above its budget, and a last beat older than the heartbeat timeout.
pub struct FaultView<'a> {
    status: &'a StatusCollector,
    load_budget: u64,
    heartbeat_timeout: u64,
}

impl<'a> FaultView<'a> {
    /// A view with a load budget of 100 — a load is a percent of it — and a
    /// heartbeat timeout of 5000 clock units.
    pub fn new(status: &'a StatusCollector) -> Self {
        Self::with_config(status, 100, 5_000)
    }

    /// A view comparing reported loads against `load_budget`, with the
    /// default heartbeat timeout.
    pub fn with_budget(status: &'a StatusCollector, load_budget: u64) -> Self {
        Self::with_config(status, load_budget, 5_000)
    }

    /// A view comparing reported loads against `load_budget`, and heart
    /// ages older than `heartbeat_timeout` as stale.
    pub fn with_config(
        status: &'a StatusCollector,
        load_budget: u64,
        heartbeat_timeout: u64,
    ) -> Self {
        Self {
            status,
            load_budget,
            heartbeat_timeout,
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

    /// How a process's heart reads: on record or not, and fresh or stale at
    /// the collector's current time.
    pub fn heartbeat(&self, role: &str) -> HeartbeatState {
        let Some(age) = self
            .status
            .status(role)
            .and_then(|status| status.heartbeat_age_at(self.status.now()))
        else {
            return HeartbeatState::None;
        };
        if age > self.heartbeat_timeout {
            HeartbeatState::Stale { age }
        } else {
            HeartbeatState::Fresh { age }
        }
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
        if let HeartbeatState::Stale { age } = self.heartbeat(role) {
            faults.push(Fault {
                kind: FaultKind::HeartbeatStale,
                value: format!("{age} units stale"),
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

/// The result of querying one expected node of a cluster.
#[derive(Debug)]
pub enum NodeResult<'a> {
    /// The node answered; its collected status is readable.
    Status(&'a ProcessStatus),
    /// The node did not answer: it is unreachable, or the query to it failed.
    Unreachable(&'a str),
}

impl<'a> NodeResult<'a> {
    /// The role the result is about.
    pub fn role(&self) -> &'a str {
        match self {
            NodeResult::Status(status) => &status.role,
            NodeResult::Unreachable(role) => role,
        }
    }

    /// Whether the node did not answer.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, NodeResult::Unreachable(_))
    }

    /// The node's status, when it answered.
    pub fn status(&self) -> Option<&'a ProcessStatus> {
        match self {
            NodeResult::Status(status) => Some(status),
            NodeResult::Unreachable(_) => None,
        }
    }
}

/// A cluster query over the expected nodes that tolerates partial failure: a
/// node that did not answer is marked unreachable, and the nodes that did
/// answer keep their results readable.
pub struct PartialQuery<'a> {
    status: &'a StatusCollector,
}

impl<'a> PartialQuery<'a> {
    /// A partial query over the collected status.
    pub fn new(status: &'a StatusCollector) -> Self {
        Self { status }
    }

    /// Query the expected nodes, in the order given: each answers with its
    /// status, or is marked unreachable when it is not on record. A node on
    /// record with no readings is not unreachable — its empty status is a
    /// zero value, and it is still readable.
    pub fn query<'b>(&self, expected: &[&'b str]) -> Vec<NodeResult<'a>>
    where
        'b: 'a,
    {
        expected
            .iter()
            .map(|role| match self.status.status(role) {
                Some(status) => NodeResult::Status(status),
                None => NodeResult::Unreachable(role),
            })
            .collect()
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

    #[test]
    fn a_stale_heart_shows_its_age_and_a_fresh_one_does_not() {
        let now = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000));
        let mut status = {
            let now = std::sync::Arc::clone(&now);
            StatusCollector::with_clock(move || now.load(std::sync::atomic::Ordering::SeqCst))
        };
        status
            .register("cellapp01", Some(100))
            .register("baseapp01", Some(200));
        for role in ["cellapp01", "baseapp01"] {
            status.record(role, "ready", "true", "readiness announcement");
            status.beat(role);
        }

        // The clock runs on: baseapp01 beats again, cellapp01 does not.
        now.store(10_000, std::sync::atomic::Ordering::SeqCst);
        status.beat("baseapp01");

        let view = FaultView::new(&status);

        // cellapp01's last beat was nine thousand units ago, past the timeout:
        // stale, and the fault names the age it was measured from.
        assert_eq!(
            view.heartbeat("cellapp01"),
            HeartbeatState::Stale { age: 9_000 }
        );
        let stale = view.report("cellapp01");
        assert!(stale.shows(FaultKind::HeartbeatStale));
        let fault = stale
            .faults
            .iter()
            .find(|fault| fault.kind == FaultKind::HeartbeatStale)
            .unwrap();
        assert_eq!(fault.value, "9000 units stale");
        assert_eq!(fault.observed_at, 10_000);

        // baseapp01 beat this instant: fresh, and no fault.
        assert_eq!(
            view.heartbeat("baseapp01"),
            HeartbeatState::Fresh { age: 0 }
        );
        assert!(!view.report("baseapp01").shows(FaultKind::HeartbeatStale));
    }

    #[test]
    fn an_age_of_zero_is_fresh_not_stale() {
        let mut status = StatusCollector::with_clock(|| 5_000);
        status.register("cellapp01", Some(100));
        status.record("cellapp01", "ready", "true", "readiness announcement");
        status.beat("cellapp01");

        // A heart that beat this instant has an age of zero, and zero is not
        // stale: the process is healthy.
        let view = FaultView::new(&status);
        assert_eq!(
            view.heartbeat("cellapp01"),
            HeartbeatState::Fresh { age: 0 }
        );
        assert!(!view.heartbeat("cellapp01").is_stale());
        assert!(view.report("cellapp01").is_healthy());
    }

    #[test]
    fn a_zero_value_heartbeat_is_a_beat_but_no_heartbeat_is_not() {
        let mut status = StatusCollector::with_clock(|| 1_000);
        status
            .register("cellapp01", Some(100))
            .register("cellapp02", Some(101));
        status.record("cellapp01", "ready", "true", "readiness announcement");
        status.record("cellapp01", "heartbeat", "0", "process heartbeat");
        status.record("cellapp02", "ready", "true", "readiness announcement");

        let view = FaultView::new(&status);

        // A beat of zero is still a beat: the value is zero, the age is zero,
        // and the heart is fresh.
        assert_eq!(
            view.heartbeat("cellapp01"),
            HeartbeatState::Fresh { age: 0 }
        );
        assert!(!view.report("cellapp01").shows(FaultKind::HeartbeatStale));

        // A process that never beat has no heart on record: its age is
        // unknown, which is not stale either.
        assert_eq!(view.heartbeat("cellapp02"), HeartbeatState::None);
        assert_eq!(view.heartbeat("cellapp02").age(), None);
        assert!(!view.report("cellapp02").shows(FaultKind::HeartbeatStale));
    }

    #[test]
    fn a_query_with_a_lost_node_still_reads_the_healthy_ones() {
        let mut status = StatusCollector::with_clock(|| 1_000);
        status
            .register("cellapp01", Some(100))
            .register("baseapp01", Some(200));
        for role in ["cellapp01", "baseapp01"] {
            status.record(role, "version", "0.1.0", "build metadata");
            status.record(role, "ready", "true", "readiness announcement");
        }
        // cellapp02 is expected but never answered: the query to it fails.
        let query = PartialQuery::new(&status);
        let results = query.query(&["cellapp01", "cellapp02", "baseapp01"]);

        // Every expected node answers with a result; the lost one is marked.
        assert_eq!(results.len(), 3);
        assert!(!results[0].is_unreachable());
        assert_eq!(results[0].role(), "cellapp01");
        assert!(results[1].is_unreachable());
        assert_eq!(results[1].role(), "cellapp02");
        assert!(results[1].status().is_none());
        assert!(!results[2].is_unreachable());

        // The healthy nodes' results stay readable through the failure.
        let cellapp = results[0].status().unwrap();
        assert_eq!(cellapp.pid, Some(100));
        assert!(cellapp.is_ready());
        assert_eq!(cellapp.reading("version").unwrap().value, "0.1.0");
        let baseapp = results[2].status().unwrap();
        assert_eq!(baseapp.role, "baseapp01");
        assert!(baseapp.is_ready());
    }

    #[test]
    fn a_silent_node_is_readable_but_not_unreachable() {
        let mut status = StatusCollector::with_clock(|| 0);
        status.register("cellapp01", Some(100));

        // On record even with no readings: reachable, and its empty status is
        // a readable zero value — not an unreachable node.
        let query = PartialQuery::new(&status);
        let results = query.query(&["cellapp01", "ghost"]);
        assert!(!results[0].is_unreachable());
        let silent = results[0].status().unwrap();
        assert!(!silent.is_ready());
        assert!(silent.readings.is_empty());
        assert_eq!(silent.role, "cellapp01");

        // Never on record: unreachable.
        assert!(results[1].is_unreachable());
    }

    #[test]
    fn querying_no_expected_nodes_yields_nothing() {
        let status = StatusCollector::with_clock(|| 0);
        let query = PartialQuery::new(&status);
        assert!(query.query(&[]).is_empty());
    }
}
