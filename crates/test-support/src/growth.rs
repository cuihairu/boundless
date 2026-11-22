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

/// One allocation site's accounting: what it holds now and what it has
/// allocated over its lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteAccounting {
    /// The site's name, the known source the sample came from.
    pub site: String,
    /// The bytes the site currently holds.
    pub live_bytes: u64,
    /// The bytes the site has allocated since sampling began.
    pub total_bytes: u64,
}

/// Sampling-based hotspot attribution: each sample names the site it was
/// allocated from, and the sampler attributes live bytes to that site, so the
/// dominant allocation source — the hotspot — is identified by ranking sites
/// by what they hold.
#[derive(Debug, Default)]
pub struct HotspotSampler {
    sites: std::collections::BTreeMap<String, (u64, u64)>,
}

impl HotspotSampler {
    /// A sampler with no sites yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a sample: `bytes` allocated at `site`.
    pub fn sample(&mut self, site: &str, bytes: u64) {
        let entry = self.sites.entry(site.to_string()).or_insert((0, 0));
        entry.0 += bytes;
        entry.1 += bytes;
    }

    /// Record a release: `bytes` let go from `site`. A site drained to zero
    /// stays listed with zero live bytes, so the report still names it.
    pub fn release(&mut self, site: &str, bytes: u64) {
        if let Some(entry) = self.sites.get_mut(site) {
            entry.0 = entry.0.saturating_sub(bytes);
        }
    }

    /// The bytes `site` currently holds.
    pub fn live_bytes(&self, site: &str) -> u64 {
        self.sites.get(site).map_or(0, |entry| entry.0)
    }

    /// The bytes `site` has allocated since sampling began.
    pub fn total_bytes(&self, site: &str) -> u64 {
        self.sites.get(site).map_or(0, |entry| entry.1)
    }

    /// The sites ranked by live bytes, descending: the front is the hotspot.
    /// Ties fall back to name order so the ranking is stable.
    pub fn hotspots(&self) -> Vec<SiteAccounting> {
        let mut ranked: Vec<SiteAccounting> = self
            .sites
            .iter()
            .map(|(site, (live, total))| SiteAccounting {
                site: site.clone(),
                live_bytes: *live,
                total_bytes: *total,
            })
            .collect();
        ranked.sort_by(|a, b| b.live_bytes.cmp(&a.live_bytes).then(a.site.cmp(&b.site)));
        ranked
    }

    /// The site holding the most live bytes, when there is one.
    pub fn dominant(&self) -> Option<String> {
        self.hotspots()
            .first()
            .filter(|top| top.live_bytes > 0)
            .map(|top| top.site.clone())
    }

    /// The share of all live bytes held by `site`, in whole per cent.
    pub fn live_share_per_cent(&self, site: &str) -> u64 {
        let total: u64 = self.sites.values().map(|entry| entry.0).sum();
        if total == 0 {
            return 0;
        }
        self.live_bytes(site) * 100 / total
    }
}

/// A reclamation-trend recorder: it samples the active object count over time,
/// so the fall after a release is visible as a trend — the count rises while
/// objects load, then falls back toward the baseline when they are released.
#[derive(Debug, Default)]
pub struct ReclamationTrend {
    samples: Vec<(u64, u64)>,
}

impl ReclamationTrend {
    /// A trend with no samples.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the active count at `when`.
    pub fn sample(&mut self, when: u64, active: u64) {
        self.samples.push((when, active));
    }

    /// The (when, active) samples, in the order recorded.
    pub fn samples(&self) -> &[(u64, u64)] {
        &self.samples
    }

    /// The highest active count seen.
    pub fn peak(&self) -> u64 {
        self.samples
            .iter()
            .map(|(_, active)| *active)
            .max()
            .unwrap_or(0)
    }

    /// The last active count, when there is one.
    pub fn last(&self) -> Option<u64> {
        self.samples.last().map(|(_, active)| *active)
    }

    /// Whether the count fell from its peak to the last reading: the release
    /// reclaimed some of what the load held.
    pub fn fell(&self) -> bool {
        match self.last() {
            Some(last) => last < self.peak(),
            None => false,
        }
    }

    /// How much the count reclaimed from its peak to the last reading.
    pub fn reclaimed(&self) -> Option<u64> {
        self.last().map(|last| self.peak() - last)
    }
}

/// One round of a long-stability sampling run, as Bots drives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StabilitySample {
    /// The round's index, from the first.
    pub round: u64,
    /// How long the snapshot collection took, in clock cycles.
    pub snapshot_duration_cycles: u64,
    /// The active object count at the round.
    pub active_objects: u64,
    /// The live bytes held at the round.
    pub live_bytes: u64,
}

/// The long-stability report: what Bots' sampling run leaves behind. It names
/// the snapshot duration (how long a collection took), the growth between the
/// first and last round, and the collection overhead (the total time sampling
/// itself spent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongStabilityReport {
    /// One entry per round, in round order.
    pub samples: Vec<StabilitySample>,
    /// The highest active object count seen.
    pub peak_active_objects: u64,
    /// Last round minus first round's active objects.
    pub growth_objects: i64,
    /// Last round minus first round's live bytes.
    pub growth_bytes: i64,
    /// The longest a snapshot collection took.
    pub max_snapshot_duration_cycles: u64,
    /// The mean snapshot collection time, rounded.
    pub avg_snapshot_duration_cycles: u64,
    /// The total time sampling spent: the run's collection overhead.
    pub collection_overhead_cycles: u64,
}

/// The sampler a long-stability Bots run feeds: it records each round's
/// snapshot duration, active count and live bytes, then folds them into a
/// report.
#[derive(Debug, Default)]
pub struct LongStabilitySampler {
    samples: Vec<StabilitySample>,
}

impl LongStabilitySampler {
    /// A sampler with no rounds recorded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one round of the run.
    pub fn record(
        &mut self,
        round: u64,
        snapshot_duration_cycles: u64,
        active_objects: u64,
        live_bytes: u64,
    ) -> &mut Self {
        self.samples.push(StabilitySample {
            round,
            snapshot_duration_cycles,
            active_objects,
            live_bytes,
        });
        self
    }

    /// The run's report. With no rounds, growth and the averages are zero.
    pub fn report(&self) -> LongStabilityReport {
        let peak_active_objects = self
            .samples
            .iter()
            .map(|sample| sample.active_objects)
            .max()
            .unwrap_or(0);
        let max_snapshot_duration_cycles = self
            .samples
            .iter()
            .map(|sample| sample.snapshot_duration_cycles)
            .max()
            .unwrap_or(0);
        let collection_overhead_cycles: u64 = self
            .samples
            .iter()
            .map(|sample| sample.snapshot_duration_cycles)
            .sum();
        let count = self.samples.len() as u64;
        let avg_snapshot_duration_cycles =
            collection_overhead_cycles.checked_div(count).unwrap_or(0);
        let (growth_objects, growth_bytes) = match (self.samples.first(), self.samples.last()) {
            (Some(first), Some(last)) => (
                signed_delta(first.active_objects, last.active_objects),
                signed_delta(first.live_bytes, last.live_bytes),
            ),
            _ => (0, 0),
        };
        LongStabilityReport {
            samples: self.samples.clone(),
            peak_active_objects,
            growth_objects,
            growth_bytes,
            max_snapshot_duration_cycles,
            avg_snapshot_duration_cycles,
            collection_overhead_cycles,
        }
    }
}

/// The signed movement from one reading to another, saturating at the edges
/// of `i64`.
fn signed_delta(from: u64, to: u64) -> i64 {
    if to >= from {
        i64::try_from(to - from).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(from - to).unwrap_or(i64::MAX)
    }
}

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

    #[test]
    fn sampling_identifies_the_known_dominant_source() {
        let mut sampler = HotspotSampler::new();
        // Three known sources, with the loader known to allocate the most.
        sampler.sample("entity_loader", 8_000);
        sampler.sample("packet_pool", 1_500);
        sampler.sample("python_heap", 500);

        // Ranked by live bytes descending: the known dominant source is first.
        let hotspots = sampler.hotspots();
        assert_eq!(hotspots.len(), 3);
        assert_eq!(hotspots[0].site, "entity_loader");
        assert_eq!(hotspots[0].live_bytes, 8_000);
        assert_eq!(hotspots[1].site, "packet_pool");
        assert_eq!(hotspots[2].site, "python_heap");
        assert_eq!(sampler.dominant(), Some("entity_loader".to_string()));
        assert_eq!(sampler.live_share_per_cent("entity_loader"), 80);
        assert_eq!(sampler.total_bytes("entity_loader"), 8_000);
    }

    #[test]
    fn releasing_from_the_dominant_site_shifts_the_hotspot() {
        let mut sampler = HotspotSampler::new();
        sampler.sample("entity_loader", 8_000);
        sampler.sample("packet_pool", 1_500);
        assert_eq!(sampler.dominant(), Some("entity_loader".to_string()));

        // Draining the loader leaves the next source dominant; the drained
        // site stays listed with zero live bytes.
        sampler.release("entity_loader", 8_000);
        assert_eq!(sampler.dominant(), Some("packet_pool".to_string()));
        let hotspots = sampler.hotspots();
        assert_eq!(hotspots[0].site, "packet_pool");
        assert_eq!(hotspots[1].site, "entity_loader");
        assert_eq!(hotspots[1].live_bytes, 0);
        assert_eq!(hotspots[1].total_bytes, 8_000);
        assert_eq!(sampler.live_share_per_cent("packet_pool"), 100);
    }

    #[test]
    fn an_empty_sampler_reports_no_dominant_site() {
        let sampler = HotspotSampler::new();
        assert_eq!(sampler.dominant(), None);
        assert!(sampler.hotspots().is_empty());
        assert_eq!(sampler.live_share_per_cent("entity_loader"), 0);
    }

    #[test]
    fn releasing_objects_makes_the_active_count_fall_back() {
        let mut counts = crate::memory_snapshot::ObjectCounts::new();
        let mut trend = ReclamationTrend::new();

        // Nothing active at the baseline.
        trend.sample(0, counts.entity_total());
        assert_eq!(trend.samples().len(), 1);

        // Load ten known entities: the count climbs to its peak, and it has
        // not fallen yet — every reading is the highest so far.
        for _ in 0..10 {
            counts.add_entity("Character");
        }
        trend.sample(1, counts.entity_total());
        assert_eq!(counts.entity_total(), 10);
        assert_eq!(trend.peak(), 10);
        assert!(!trend.fell());

        // Release eight: the active count falls back, and the trend reports
        // how much it reclaimed.
        for _ in 0..8 {
            assert!(counts.remove_entity("Character"));
        }
        trend.sample(2, counts.entity_total());
        assert_eq!(counts.entity_total(), 2);
        assert_eq!(trend.last(), Some(2));
        assert!(trend.fell());
        assert_eq!(trend.reclaimed(), Some(8));

        // Release the rest: the count is back at the baseline, fully
        // reclaimed from the peak.
        for _ in 0..2 {
            assert!(counts.remove_entity("Character"));
        }
        trend.sample(3, counts.entity_total());
        assert_eq!(counts.entity_total(), 0);
        assert_eq!(counts.entity_count("Character"), 0);
        assert_eq!(trend.last(), Some(0));
        assert_eq!(trend.reclaimed(), Some(10));
        assert_eq!(trend.samples().len(), 4);
    }

    #[test]
    fn a_still_growing_count_has_not_fallen() {
        let mut trend = ReclamationTrend::new();
        trend.sample(0, 4);
        trend.sample(1, 8);
        assert_eq!(trend.peak(), 8);
        assert!(!trend.fell());
        assert_eq!(trend.reclaimed(), Some(0));
    }

    #[test]
    fn an_empty_trend_has_no_fall() {
        let trend = ReclamationTrend::new();
        assert_eq!(trend.peak(), 0);
        assert_eq!(trend.last(), None);
        assert!(!trend.fell());
        assert_eq!(trend.reclaimed(), None);
        assert!(trend.samples().is_empty());
    }

    #[test]
    fn a_long_stability_run_reports_duration_growth_and_overhead() {
        let fixture = GrowthFixture::new();
        let mut sampler = LongStabilitySampler::new();

        // A five-round run as Bots drives it: the load grows round over round,
        // and each round's snapshot took a known number of cycles.
        for (round, (objects, bytes, duration)) in [
            (10, 1_000, 50),
            (20, 3_000, 60),
            (30, 6_000, 40),
            (30, 6_000, 55),
            (40, 8_000, 45),
        ]
        .into_iter()
        .enumerate()
        {
            let allocation = fixture.hold(bytes);
            sampler.record(round as u64, duration, objects, fixture.live_bytes());
            drop(allocation);
        }

        let report = sampler.report();
        assert_eq!(report.samples.len(), 5);
        assert_eq!(report.peak_active_objects, 40);

        // The growth between the first and last round: objects and bytes.
        assert_eq!(report.growth_objects, 30);
        assert_eq!(report.growth_bytes, 7_000);

        // The snapshot durations: the worst round, the mean round, and the
        // run's total collection overhead.
        assert_eq!(report.max_snapshot_duration_cycles, 60);
        assert_eq!(report.avg_snapshot_duration_cycles, 50);
        assert_eq!(report.collection_overhead_cycles, 250);
    }

    #[test]
    fn a_single_round_reports_no_growth() {
        let mut sampler = LongStabilitySampler::new();
        sampler.record(0, 40, 12, 2_400);
        let report = sampler.report();
        assert_eq!(report.samples.len(), 1);
        assert_eq!(report.growth_objects, 0);
        assert_eq!(report.growth_bytes, 0);
        assert_eq!(report.max_snapshot_duration_cycles, 40);
        assert_eq!(report.avg_snapshot_duration_cycles, 40);
        assert_eq!(report.collection_overhead_cycles, 40);
    }

    #[test]
    fn an_empty_long_stability_run_reports_nothing() {
        let report = LongStabilitySampler::new().report();
        assert_eq!(report.samples.len(), 0);
        assert_eq!(report.peak_active_objects, 0);
        assert_eq!(report.growth_objects, 0);
        assert_eq!(report.growth_bytes, 0);
        assert_eq!(report.max_snapshot_duration_cycles, 0);
        assert_eq!(report.avg_snapshot_duration_cycles, 0);
        assert_eq!(report.collection_overhead_cycles, 0);
    }
}
