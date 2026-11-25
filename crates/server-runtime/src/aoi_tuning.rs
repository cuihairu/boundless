//! OPT-08-01-02: AOI indexing and witness scheduling, evaluated.
//!
//! Two comparisons over the dense baseline's own load (same seed, same
//! movement schedule, the shipped faces end to end):
//!
//! 1. **The sector-prefilter index** — before the shipped seeing pass
//!    runs, drop targets whose horizontal angle from facing exceeds
//!    `vision_angle + margin`. Horizontal angle ≥ the pass's 3D cone
//!    angle (a horizontal facing makes the cone angle the smaller one),
//!    so the prefilter is conservative: the visible set and the seeing
//!    callbacks must come back identical, tick for tick. The gain is
//!    the entities the pass no longer walks.
//! 2. **The witness scheduling sweep** — the scheduler's operator face
//!    is the packet budget (`setWitnessCapacity`). Sweep it at 1×, 2×
//!    and 4× the dense baseline's capacity and report the scheduling
//!    trade-off: ticks to full per-entity coverage (the priority
//!    window starves nobody), and the steady broadcast volume the
//!    budget buys.
//!
//! Equalities and monotonicity are asserted; wall-clock is probe data,
//! printed for the review doc, never asserted.

use std::collections::BTreeSet;

use crate::aoi_baseline::{
    CAPACITY_BITS_PER_SECOND, CAPACITY_HERTZ, DENSE_ENTITIES, DenseAoi, viewer_start,
};

/// The sector margin over the vision half-angle, in radians.
pub const SECTOR_MARGIN: f32 = 0.25;

/// The index comparison: shipped full pass vs sector-prefiltered pass
/// over the identical dense load.
#[derive(Debug)]
pub struct IndexComparison {
    pub ticks: usize,
    /// Target rows the shipped pass walked.
    pub walked_shipped: usize,
    /// Target rows the candidate pass walked after the prefilter.
    pub walked_candidate: usize,
    /// Ticks where the candidate's visible set differed from the
    /// shipped one — always zero.
    pub set_mismatches: usize,
    /// Seeing starts that differed — always zero.
    pub start_mismatches: usize,
    /// Probe sums in milliseconds — printed, never asserted.
    pub shipped_ms: f64,
    pub candidate_ms: f64,
}

/// Runs the index comparison: two worlds, the same 48-tick schedule,
/// one through [`DenseAoi::tick`], one through
/// [`DenseAoi::tick_sectored`].
pub fn run_index_comparison() -> IndexComparison {
    let ticks = crate::aoi_baseline::BASELINE_TICKS;
    let mut shipped = DenseAoi::new(viewer_start());
    let mut candidate = DenseAoi::new(viewer_start());

    let mut walked_shipped = 0usize;
    let mut walked_candidate = 0usize;
    let mut set_mismatches = 0usize;
    let mut start_mismatches = 0usize;

    for tick in 0..ticks {
        let (ledger, _walked, _full) = shipped.tick_inner(tick, None);
        let (sectored, walked, _full) = candidate.tick_sectored(tick, SECTOR_MARGIN);
        walked_shipped += full_of(&shipped);
        walked_candidate += walked;
        if shipped.ev.visible_entities != candidate.ev.visible_entities {
            set_mismatches += 1;
        }
        if ledger.started_ids != sectored.started_ids {
            start_mismatches += 1;
        }
    }

    IndexComparison {
        ticks,
        walked_shipped,
        walked_candidate,
        set_mismatches,
        start_mismatches,
        shipped_ms: shipped.seeing_ms,
        candidate_ms: candidate.seeing_ms,
    }
}

/// The shipped pass walks every entity the vision trigger holds in
/// range — the same count each tick, read off the shipped world.
fn full_of(world: &DenseAoi) -> usize {
    world.ev.entities_in_vision_range.len()
}

/// One capacity's scheduling run.
#[derive(Debug)]
pub struct CoverageRun {
    pub bits_per_second: i32,
    /// The packet size the capacity face computed.
    pub packet_size: i32,
    /// Ticks until every dense entity received at least one entity
    /// update (`NEW_TICKS_MAX` when it never completes — asserted away).
    pub coverage_ticks: usize,
    /// Entities covered when the run ended — always the whole field.
    pub entities_covered: usize,
    /// Broadcast volume over the 16 ticks after coverage.
    pub steady_bytes: usize,
    pub steady_messages: usize,
}

/// Ticks a sweep run may take to cover the field.
pub const NEW_TICKS_MAX: usize = 128;
/// The steady window measured after coverage.
pub const STEADY_TICKS: usize = 16;

/// Runs one capacity until the whole field is covered, then measures
/// a steady window.
pub fn run_capacity(bits_per_second: i32) -> CoverageRun {
    let mut cell = DenseAoi::new(viewer_start());
    cell.witness
        .set_witness_capacity(bits_per_second, CAPACITY_HERTZ);
    let packet_size = crate::witness::bps_to_packet_size(bits_per_second, CAPACITY_HERTZ);

    let mut covered = BTreeSet::new();
    let mut coverage_ticks = NEW_TICKS_MAX;
    let mut steady_bytes = 0usize;
    let mut steady_messages = 0usize;
    let mut steady_left = 0usize;

    for tick in 0..NEW_TICKS_MAX + STEADY_TICKS {
        let ledger = cell.tick(tick);
        if covered.len() < DENSE_ENTITIES {
            for id in &ledger.updated_ids {
                covered.insert(*id);
            }
            if covered.len() == DENSE_ENTITIES && coverage_ticks == NEW_TICKS_MAX {
                coverage_ticks = tick + 1;
                steady_left = STEADY_TICKS;
            }
        } else if steady_left > 0 {
            steady_bytes += ledger.bytes;
            steady_messages += ledger.messages;
            steady_left -= 1;
        }
    }

    CoverageRun {
        bits_per_second,
        packet_size,
        coverage_ticks,
        entities_covered: covered.len(),
        steady_bytes,
        steady_messages,
    }
}

/// The whole sweep: the dense baseline's capacity, then 2× and 4×.
pub fn run_capacity_sweep() -> Vec<CoverageRun> {
    vec![
        run_capacity(CAPACITY_BITS_PER_SECOND),
        run_capacity(CAPACITY_BITS_PER_SECOND * 2),
        run_capacity(CAPACITY_BITS_PER_SECOND * 4),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sector_index_preserves_the_seeing_pass() {
        let comparison = run_index_comparison();

        // The prefilter never changed what the shipped pass decided.
        assert_eq!(comparison.set_mismatches, 0);
        assert_eq!(comparison.start_mismatches, 0);

        // And it actually prefiltered: the cone excludes the back
        // sectors of a dense field.
        assert!(comparison.walked_candidate > 0);
        assert!(
            comparison.walked_candidate < comparison.walked_shipped,
            "the sector index must skip work: {} vs {}",
            comparison.walked_candidate,
            comparison.walked_shipped
        );

        println!(
            "INDEX ticks={} walked_shipped={} walked_candidate={} saved={:.1}% shipped_ms={:.3} candidate_ms={:.3}",
            comparison.ticks,
            comparison.walked_shipped,
            comparison.walked_candidate,
            100.0 * (1.0 - comparison.walked_candidate as f64 / comparison.walked_shipped as f64),
            comparison.shipped_ms,
            comparison.candidate_ms,
        );
    }

    #[test]
    fn the_capacity_sweep_reports_the_scheduling_tradeoff() {
        let sweep = run_capacity_sweep();

        // Every capacity covers the whole field — the priority window
        // starves nobody.
        for run in &sweep {
            assert_eq!(run.entities_covered, DENSE_ENTITIES);
            assert!(run.coverage_ticks <= NEW_TICKS_MAX);
        }

        // More budget never covers slower: monotone, and strict at the
        // ends of this sweep.
        assert!(sweep[1].coverage_ticks <= sweep[0].coverage_ticks);
        assert!(sweep[2].coverage_ticks <= sweep[1].coverage_ticks);
        assert!(sweep[2].coverage_ticks < sweep[0].coverage_ticks);

        for run in &sweep {
            println!(
                "SWEEP bps={} packet_size={}B coverage_ticks={} steady_bytes={} steady_messages={}",
                run.bits_per_second,
                run.packet_size,
                run.coverage_ticks,
                run.steady_bytes,
                run.steady_messages
            );
        }
    }
}
