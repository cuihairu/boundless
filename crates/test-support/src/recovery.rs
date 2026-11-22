//! BW-25-01-05: the fault-recovery phase of a process.
//!
//! A fault drill moves a process through its recovery: it starts, runs, a
//! fault takes it down, recovery brings it back, and it rejoins the cluster.
//! The tracker records the phase each event moved the process to and when,
//! so a drill's state is visible as the chain of events that produced it —
//! and a phase only moves along the drill's own transitions.

/// The phase a process is in during a fault drill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPhase {
    /// Coming up, not yet in service.
    Starting,
    /// In service.
    Running,
    /// A fault took the process down.
    Faulted,
    /// Recovery is bringing the process back.
    Recovering,
    /// The process rejoined the cluster.
    Recovered,
}

impl RecoveryPhase {
    /// Whether `to` may follow this phase in a drill.
    fn may_follow(self, to: RecoveryPhase) -> bool {
        use RecoveryPhase::*;
        matches!(
            (self, to),
            (Starting, Running)
                | (Running, Faulted)
                | (Faulted, Recovering)
                | (Recovering, Recovered)
                | (Recovering, Faulted)
                | (Recovered, Running)
                | (Recovered, Faulted)
        )
    }
}

/// One recorded phase transition: the phase the process moved to, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseEvent {
    /// The phase the process moved to.
    pub phase: RecoveryPhase,
    /// When the event happened.
    pub at: u64,
}

/// Why a transition was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionError {
    /// The move is not one of the drill's transitions.
    Illegal,
}

/// A drill's phase tracker, one timeline per process.
pub struct RecoveryTracker {
    phases: std::collections::BTreeMap<String, RecoveryPhase>,
    timelines: std::collections::BTreeMap<String, Vec<PhaseEvent>>,
    clock: Box<dyn Fn() -> u64>,
}

impl Default for RecoveryTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryTracker {
    /// A tracker stamping events from the wall clock.
    pub fn new() -> Self {
        Self::with_clock(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos() as u64)
                .unwrap_or(0)
        })
    }

    /// A tracker whose event times come from `clock`, so a test can drive a
    /// drill's timeline.
    pub fn with_clock(clock: impl Fn() -> u64 + 'static) -> Self {
        Self {
            phases: std::collections::BTreeMap::new(),
            timelines: std::collections::BTreeMap::new(),
            clock: Box::new(clock),
        }
    }

    /// Begin a process's drill at `Starting`.
    pub fn begin(&mut self, role: &str) -> &mut Self {
        let at = (self.clock)();
        self.phases
            .insert(role.to_string(), RecoveryPhase::Starting);
        self.timelines
            .entry(role.to_string())
            .or_default()
            .push(PhaseEvent {
                phase: RecoveryPhase::Starting,
                at,
            });
        self
    }

    /// Move a process to the next phase of its drill. The move is refused
    /// when it is not one of the drill's transitions, leaving the phase and
    /// the timeline untouched.
    pub fn transition(&mut self, role: &str, to: RecoveryPhase) -> Result<(), TransitionError> {
        let Some(from) = self.phases.get(role).copied() else {
            return Err(TransitionError::Illegal);
        };
        if !from.may_follow(to) {
            return Err(TransitionError::Illegal);
        }
        let at = (self.clock)();
        self.phases.insert(role.to_string(), to);
        self.timelines
            .entry(role.to_string())
            .or_default()
            .push(PhaseEvent { phase: to, at });
        Ok(())
    }

    /// The process's current phase.
    pub fn phase(&self, role: &str) -> Option<RecoveryPhase> {
        self.phases.get(role).copied()
    }

    /// The process's recorded events, oldest first.
    pub fn timeline(&self, role: &str) -> &[PhaseEvent] {
        self.timelines
            .get(role)
            .map(|events| events.as_slice())
            .unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fault_drill_moves_the_phase_with_each_event() {
        let mut tracker = RecoveryTracker::with_clock(|| 0);
        tracker.begin("cellapp01");
        assert_eq!(tracker.phase("cellapp01"), Some(RecoveryPhase::Starting));

        // Announce readiness: in service.
        tracker
            .transition("cellapp01", RecoveryPhase::Running)
            .unwrap();
        assert_eq!(tracker.phase("cellapp01"), Some(RecoveryPhase::Running));

        // The fault takes the process down.
        tracker
            .transition("cellapp01", RecoveryPhase::Faulted)
            .unwrap();
        assert_eq!(tracker.phase("cellapp01"), Some(RecoveryPhase::Faulted));

        // Recovery brings it back and it rejoins.
        tracker
            .transition("cellapp01", RecoveryPhase::Recovering)
            .unwrap();
        tracker
            .transition("cellapp01", RecoveryPhase::Recovered)
            .unwrap();

        // Back in service after the drill.
        tracker
            .transition("cellapp01", RecoveryPhase::Running)
            .unwrap();

        // The state changed with each event: the timeline holds the whole
        // chain, oldest first.
        let phases: Vec<RecoveryPhase> = tracker
            .timeline("cellapp01")
            .iter()
            .map(|event| event.phase)
            .collect();
        assert_eq!(
            phases,
            vec![
                RecoveryPhase::Starting,
                RecoveryPhase::Running,
                RecoveryPhase::Faulted,
                RecoveryPhase::Recovering,
                RecoveryPhase::Recovered,
                RecoveryPhase::Running,
            ]
        );
        assert_eq!(tracker.phase("cellapp01"), Some(RecoveryPhase::Running));
    }

    #[test]
    fn an_illegal_transition_is_refused_and_changes_nothing() {
        let mut tracker = RecoveryTracker::with_clock(|| 5);
        tracker.begin("cellapp01");

        // A starting process cannot skip ahead to recovered.
        assert_eq!(
            tracker.transition("cellapp01", RecoveryPhase::Recovered),
            Err(TransitionError::Illegal)
        );
        assert_eq!(tracker.phase("cellapp01"), Some(RecoveryPhase::Starting));
        assert_eq!(tracker.timeline("cellapp01").len(), 1);

        // An unknown process has no drill to move.
        assert_eq!(
            tracker.transition("ghost", RecoveryPhase::Running),
            Err(TransitionError::Illegal)
        );
        assert_eq!(tracker.phase("ghost"), None);
    }

    #[test]
    fn a_failed_recovery_can_fault_again() {
        let mut tracker = RecoveryTracker::with_clock(|| 1);
        tracker.begin("cellapp01");
        tracker
            .transition("cellapp01", RecoveryPhase::Running)
            .unwrap();
        tracker
            .transition("cellapp01", RecoveryPhase::Faulted)
            .unwrap();
        tracker
            .transition("cellapp01", RecoveryPhase::Recovering)
            .unwrap();
        // The first recovery attempt fails: the process faults again, and a
        // second attempt rejoins.
        tracker
            .transition("cellapp01", RecoveryPhase::Faulted)
            .unwrap();
        tracker
            .transition("cellapp01", RecoveryPhase::Recovering)
            .unwrap();
        tracker
            .transition("cellapp01", RecoveryPhase::Recovered)
            .unwrap();
        assert_eq!(tracker.phase("cellapp01"), Some(RecoveryPhase::Recovered));
        assert_eq!(tracker.timeline("cellapp01").len(), 7);
    }
}
