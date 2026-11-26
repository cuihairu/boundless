//! OPT-07-01-03: quantify what no-op suppression saves on the shared-data
//! chain.
//!
//! The acceptance is a message-count comparison. The harness runs the
//! OPT-07-01-03 seeded mutation sequence twice through the real
//! propagation chain — once shipped (every mutation answers with
//! traffic) and once with the tuned suppression (a set whose value the
//! authoritative map already holds, or a del of an absent key, is
//! skipped before it can produce a message) — and records both ledgers
//! whole. The load is identical (same mutation sequence, same final
//! authoritative maps), so the message-count difference is purely the
//! suppression's effect. No wall-clock value is sampled; the comparison
//! reproduces from the seed.

use std::collections::BTreeMap;

use crate::shared_data_baseline::{
    SharedDataLedger, run_shared_data_baseline, run_shared_data_propagation,
};

/// The tuned chain: skip the writes that would not change the
/// authoritative map.
pub fn tuned_shared_data_suppression() -> bool {
    true
}

/// Both propagation ledgers, run over the same seeded sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedDataComparison {
    /// The shipped chain — the OPT-07-01-03 baseline ledger.
    pub shipped: SharedDataLedger,
    /// The tuned chain with no-op suppression.
    pub tuned: SharedDataLedger,
}

impl SharedDataComparison {
    /// The message-count change the suppression buys: how many fewer
    /// messages the tuned chain sent, and how many mutations it answered
    /// with none.
    pub fn messages_saved(&self) -> usize {
        self.shipped.messages_total - self.tuned.messages_total
    }

    /// Whether both runs carried exactly the same load: same mutation
    /// count and the same final authoritative maps.
    pub fn same_load(&self) -> bool {
        self.shipped.same_load(&self.tuned)
    }
}

/// Run the seeded comparison once.
pub fn run_shared_data_comparison(seed: u64) -> SharedDataComparison {
    SharedDataComparison {
        shipped: run_shared_data_baseline(seed),
        tuned: run_shared_data_propagation(seed, tuned_shared_data_suppression()),
    }
}

/// The final authoritative maps a comparison's two runs must agree on.
pub fn final_maps(comparison: &SharedDataComparison) -> [(&BTreeMap<String, String>, &str); 3] {
    [
        (&comparison.shipped.base_data, "base"),
        (&comparison.shipped.cell_data, "cell"),
        (&comparison.shipped.global_data, "global"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_data_baseline::SHARED_DATA_SEED;

    #[test]
    fn the_comparison_reproduces_from_its_seed() {
        assert_eq!(
            run_shared_data_comparison(SHARED_DATA_SEED),
            run_shared_data_comparison(SHARED_DATA_SEED)
        );
    }

    #[test]
    fn the_load_is_identical_across_the_suppression() {
        let comparison = run_shared_data_comparison(SHARED_DATA_SEED);
        assert!(comparison.same_load());
    }

    #[test]
    fn the_shipped_chain_never_suppresses() {
        let comparison = run_shared_data_comparison(SHARED_DATA_SEED);
        assert_eq!(comparison.shipped.suppressed_no_ops, 0);
    }

    #[test]
    fn the_tuned_chain_sends_fewer_messages_over_the_same_load() {
        let comparison = run_shared_data_comparison(SHARED_DATA_SEED);
        assert!(comparison.tuned.suppressed_no_ops > 0);
        assert!(comparison.tuned.messages_total < comparison.shipped.messages_total);
        assert_eq!(
            comparison.messages_saved(),
            comparison.shipped.messages_total - comparison.tuned.messages_total
        );
        assert_eq!(final_maps(&comparison).len(), 3);
    }

    #[test]
    fn the_message_count_change_is_recorded() {
        // Seed 161_803: the shipped chain answers all 96 mutations with
        // 575 messages; the tuned chain suppresses 21 no-op writes and
        // sends 444 — 131 fewer over an identical load (same final maps).
        let comparison = run_shared_data_comparison(SHARED_DATA_SEED);
        assert_eq!(comparison.shipped.messages_total, 575);
        assert_eq!(comparison.shipped.suppressed_no_ops, 0);
        assert_eq!(comparison.tuned.messages_total, 444);
        assert_eq!(comparison.tuned.suppressed_no_ops, 21);
        assert_eq!(comparison.messages_saved(), 131);
        assert!(comparison.same_load());
    }

    #[test]
    fn the_suppression_holds_across_seeds() {
        for seed in [
            SHARED_DATA_SEED,
            SHARED_DATA_SEED + 7,
            SHARED_DATA_SEED + 31,
        ] {
            let comparison = run_shared_data_comparison(seed);
            assert!(comparison.same_load(), "the load holds at {seed}");
            assert_eq!(comparison.shipped.suppressed_no_ops, 0);
            assert!(
                comparison.tuned.suppressed_no_ops > 0,
                "no-ops suppressed at {seed}"
            );
            assert!(
                comparison.tuned.messages_total < comparison.shipped.messages_total,
                "fewer messages at {seed}"
            );
        }
    }
}
