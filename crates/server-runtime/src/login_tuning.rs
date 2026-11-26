//! OPT-06-01-02: compare tail latency across login queueing tunings.
//!
//! The acceptance compares tail latency in a login flood scenario. The
//! harness runs the same seeded flood twice through the real login faces —
//! once with the shipped queueing shape and once with the tuned shape — and
//! records both ledgers whole, so every decision (admission, rejection,
//! ignore, session switch) is on the record. No wall-clock value is sampled:
//! the latency is the whole-tick delay the faces produce, and the comparison
//! reproduces from the seed.
//!
//! The tail the shipped shape pays is created by the cached-request record a
//! gate-rejected attempt leaves behind. The record reads as "in flight", so
//! a later arrival at that address is ignored (`IgnoreInProgress`) and answers
//! two ticks late even though no login ever proceeded from the phantom. The
//! tuned shape stops recording gate-rejected attempts; only an attempt that
//! passed the gates can hold an address, and the flood keeps its re-send
//! protection — an attempt genuinely waiting on the DBApp still ignores a
//! duplicate. The session switch adds no tail under either shape: it
//! completes within its arrival's tick in both runs, which the ledgers
//! record as identical per-arrival switch behaviour.

use crate::login_baseline::{LoginBaselineLedger, LoginTuning, run_login_baseline_with};

/// The tuned queueing shape: the flood's twelve arrivals per second stay
/// under the sixteen-per-window budget with a one-second window, and
/// gate-rejected attempts no longer occupy the cached-request record.
pub fn tuned_login_queueing() -> LoginTuning {
    LoginTuning {
        rate_limit: 16,
        rate_limit_seconds: 1.0,
        cache_rejected_attempts: false,
    }
}

/// Both flood ledgers, run from the same seed and arrival schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginComparison {
    /// The shipped queueing shape — the OPT-06-01-01 baseline ledger.
    pub shipped: LoginBaselineLedger,
    /// The tuned queueing shape.
    pub tuned: LoginBaselineLedger,
}

impl LoginComparison {
    /// Whether the tuned shape cut the whole-tick tail the shipped shape
    /// pays. The tail is the peak latency the two runs recorded.
    pub fn tail_cut(&self) -> bool {
        self.tuned.latency_peak() < self.shipped.latency_peak()
    }
}

/// Run the seeded comparison once.
pub fn run_login_comparison(seed: u64) -> LoginComparison {
    LoginComparison {
        shipped: run_login_baseline_with(seed, LoginTuning::default()),
        tuned: run_login_baseline_with(seed, tuned_login_queueing()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: u64 = crate::login_baseline::SCENARIO_SEED;

    #[test]
    fn the_comparison_reproduces_from_its_seed() {
        assert_eq!(run_login_comparison(SEED), run_login_comparison(SEED));
    }

    #[test]
    fn the_tuned_shape_cuts_the_tail() {
        let comparison = run_login_comparison(SEED);
        assert_eq!(comparison.shipped.latency_peak(), 2);
        assert_eq!(comparison.shipped.latency_percentile(95.0), Some(2.0));
        assert_eq!(comparison.tuned.latency_peak(), 1);
        assert_eq!(comparison.tuned.latency_percentile(95.0), Some(1.0));
        assert!(comparison.tail_cut());
    }

    #[test]
    fn the_tuned_shape_stops_the_spurious_rejections() {
        let comparison = run_login_comparison(SEED);
        assert!(comparison.shipped.rate_rejected > 0);
        assert_eq!(comparison.tuned.rate_rejected, 0);
        assert!(
            comparison.tuned.sessions.len() > comparison.shipped.sessions.len(),
            "the tuned shape admits more of the flood"
        );
    }

    #[test]
    fn the_tuned_shape_ignores_no_arrival_behind_a_phantom() {
        let comparison = run_login_comparison(SEED);
        // The shipped shape answers some arrivals late because a phantom
        // in-flight record ignores them; the tuned shape has no phantom, so
        // no arrival is ignored or answered from a stale record.
        assert!(comparison.shipped.outcomes.iter().any(|o| o.replayed));
        assert!(comparison.tuned.outcomes.iter().all(|o| !o.replayed));
    }

    #[test]
    fn both_shapes_complete_every_accepted_switch() {
        let comparison = run_login_comparison(SEED);
        for ledger in [&comparison.shipped, &comparison.tuned] {
            let accepted: Vec<u32> = ledger
                .outcomes
                .iter()
                .filter(|o| o.status == 0)
                .map(|o| o.client)
                .collect();
            assert_eq!(&ledger.sessions, &accepted);
        }
    }
}
