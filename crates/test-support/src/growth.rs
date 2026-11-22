//! BW-24-03: growth investigation fixtures.
//!
//! A controlled allocation-and-release scaffold over a live tracker. It holds
//! known byte counts, so the difference between any two readings is
//! predictable: the live reading moves exactly by the bytes held or let go,
//! the high-water mark ratchets on the way up, and the cumulative total only
//! ever grows. That predictability is what the rest of the growth
//! investigation (hotspots, reclamation trend, long-stability sampling)
//! measures against.

use crate::memory_snapshot::{LiveAllocationTracker, TrackedAllocation};

/// A controlled allocation-and-release fixture.
pub struct GrowthFixture {
    tracker: LiveAllocationTracker,
}

impl Default for GrowthFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl GrowthFixture {
    /// A fixture with nothing held and no high-water mark.
    pub fn new() -> Self {
        Self {
            tracker: LiveAllocationTracker::new(),
        }
    }

    /// The fixture's live tracker, for a caller that wants the raw readings.
    pub fn tracker(&self) -> &LiveAllocationTracker {
        &self.tracker
    }

    /// The bytes currently held by unreleased allocations.
    pub fn live_bytes(&self) -> u64 {
        self.tracker.live_bytes()
    }

    /// The round's high-water mark.
    pub fn peak_bytes(&self) -> u64 {
        self.tracker.peak_bytes()
    }

    /// The bytes allocated since the fixture began, cumulatively.
    pub fn total_allocated_bytes(&self) -> u64 {
        self.tracker.total_allocated_bytes()
    }

    /// Hold a known allocation of `bytes`; the live reading rises by exactly
    /// that many bytes and stays until the guard is dropped.
    pub fn hold(&self, bytes: usize) -> TrackedAllocation<'_> {
        self.tracker.allocate(bytes)
    }

    /// The live reading the fixture predicts after holding `bytes` from its
    /// current state: current live plus the bytes.
    pub fn predicted_live_after_holding(&self, bytes: usize) -> u64 {
        self.live_bytes().saturating_add(bytes as u64)
    }

    /// The live reading the fixture predicts after releasing `held` bytes
    /// from its current state: current live minus the bytes, floored at zero
    /// (the tracker never under-counts).
    pub fn predicted_live_after_releasing(&self, held: u64) -> u64 {
        self.live_bytes().saturating_sub(held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_allocation_and_release_makes_the_delta_predictable() {
        let fixture = GrowthFixture::new();
        let baseline = fixture.live_bytes();
        assert_eq!(baseline, 0);

        // Hold a known 1000 bytes: predict the reading first, then hold, and
        // the live reading lands exactly where the prediction says.
        let predicted = fixture.predicted_live_after_holding(1000);
        assert_eq!(predicted, baseline + 1000);
        let allocation = fixture.hold(1000);
        assert_eq!(fixture.live_bytes(), predicted);

        // The high-water mark recorded the hold.
        let peak = fixture.peak_bytes();
        assert_eq!(peak, baseline + 1000);

        // Hold a second known block: the prediction is additive, and the
        // reading again lands exactly where it says.
        let predicted = fixture.predicted_live_after_holding(500);
        assert_eq!(predicted, baseline + 1500);
        let second = fixture.hold(500);
        assert_eq!(fixture.live_bytes(), predicted);

        // Release the first block: predict before letting go, and the reading
        // falls by exactly the bytes released.
        let predicted = fixture.predicted_live_after_releasing(1000);
        assert_eq!(predicted, baseline + 500);
        drop(allocation);
        assert_eq!(fixture.live_bytes(), predicted);

        // The high-water mark held at its peak, and the cumulative total kept
        // only growing: a release is not a reclamation of the high-water.
        assert_eq!(fixture.peak_bytes(), baseline + 1500);
        assert_eq!(fixture.total_allocated_bytes(), 1500);

        // Release the last block: the reading is back to the baseline, fully
        // predictable end to end.
        drop(second);
        assert_eq!(fixture.live_bytes(), baseline);
        assert_eq!(fixture.total_allocated_bytes(), 1500);
        assert_eq!(fixture.peak_bytes(), baseline + 1500);
    }

    #[test]
    fn releasing_more_than_held_floors_at_zero() {
        let fixture = GrowthFixture::new();
        let allocation = fixture.hold(400);
        drop(allocation);
        // The predicted release reading never goes below zero.
        assert_eq!(fixture.predicted_live_after_releasing(400), 0);
        assert_eq!(fixture.live_bytes(), 0);
    }
}
