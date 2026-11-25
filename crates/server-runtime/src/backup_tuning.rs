//! OPT-07-01-02: compare backup queue and recovery under one load.
//!
//! The acceptance compares the queue and the recovery under the same load.
//! The harness runs the OPT-07-01-01 seeded fleet twice through the real
//! backup faces — once with the shipped pacing and once with a tuned one —
//! and records both ledgers whole. The load is identical (same fleet, same
//! seeded properties, same payload bytes), so every difference in the
//! ledger is the pacing's effect: the queue drains in fewer ticks, the
//! recovery cycle re-persists in fewer ticks, and the price is a higher
//! per-tick bandwidth peak. No wall-clock value is sampled; the comparison
//! reproduces from the seed.

use crate::backup_baseline::{BackupBaselineLedger, BackupTuning, run_backup_baseline_with};

/// The tuned pacing: the sender backs up twice the bases per tick and the
/// archiver cycle is half as long, so the queue and the recovery both
/// finish in half the ticks at twice the per-tick bandwidth.
pub fn tuned_backup_pacing() -> BackupTuning {
    BackupTuning {
        backup_slice: 16,
        archive_period: 4,
    }
}

/// Both pacing ledgers, run over the same seeded load.
#[derive(Clone, Debug, PartialEq)]
pub struct BackupComparison {
    /// The shipped pacing — the OPT-07-01-01 baseline ledger.
    pub shipped: BackupBaselineLedger,
    /// The tuned pacing.
    pub tuned: BackupBaselineLedger,
}

impl BackupComparison {
    /// Whether both runs carried exactly the same load: same fleet, same
    /// persisted set, same payload bytes, same restored fleet.
    pub fn same_load(&self) -> bool {
        self.shipped.entities == self.tuned.entities
            && self.shipped.persisted_entities == self.tuned.persisted_entities
            && self.shipped.backup_bytes_total == self.tuned.backup_bytes_total
            && self.shipped.restored_entities == self.tuned.restored_entities
            && self.shipped.property_matches == self.tuned.property_matches
    }
}

/// Run the seeded comparison once.
pub fn run_backup_comparison(seed: u64) -> BackupComparison {
    BackupComparison {
        shipped: run_backup_baseline_with(seed, BackupTuning::default()),
        tuned: run_backup_baseline_with(seed, tuned_backup_pacing()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup_baseline::BACKUP_SEED;

    #[test]
    fn the_comparison_reproduces_from_its_seed() {
        assert_eq!(
            run_backup_comparison(BACKUP_SEED),
            run_backup_comparison(BACKUP_SEED)
        );
    }

    #[test]
    fn the_load_is_identical_across_the_pacing() {
        let comparison = run_backup_comparison(BACKUP_SEED);
        assert!(comparison.same_load());
    }

    #[test]
    fn the_tuned_pacing_halves_the_queue_and_the_recovery() {
        let comparison = run_backup_comparison(BACKUP_SEED);
        assert_eq!(comparison.shipped.backup_ticks, 6);
        assert_eq!(comparison.tuned.backup_ticks, 3);
        assert_eq!(comparison.shipped.archive_recovery_ticks, 8);
        assert_eq!(comparison.tuned.archive_recovery_ticks, 4);
    }

    #[test]
    fn the_tuned_pacing_pays_a_higher_bandwidth_peak() {
        let comparison = run_backup_comparison(BACKUP_SEED);
        let shipped_peak = comparison
            .shipped
            .bandwidth_bytes
            .iter()
            .max()
            .copied()
            .unwrap_or(0);
        let tuned_peak = comparison
            .tuned
            .bandwidth_bytes
            .iter()
            .max()
            .copied()
            .unwrap_or(0);
        assert!(tuned_peak > shipped_peak);
        // The extra per-tick bandwidth is the same bytes moved in fewer
        // ticks, not more bytes.
        let shipped_total: usize = comparison.shipped.bandwidth_bytes.iter().sum();
        let tuned_total: usize = comparison.tuned.bandwidth_bytes.iter().sum();
        assert_eq!(shipped_total, tuned_total);
    }
}
