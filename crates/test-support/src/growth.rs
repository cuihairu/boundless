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
}
