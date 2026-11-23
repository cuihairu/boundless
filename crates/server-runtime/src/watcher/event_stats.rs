//! Event history statistics (BW-12-01-05): the statistics fields exposed
//! to the watcher tree.
//!
//! Ported from `programming/bigworld/lib/server/event_history_stats.hpp`
//! (the `EventHistoryStats` class — "Statistics are exposed as watchers")
//! and the `init` in `event_history_stats.cpp`, including the anonymous
//! `BaseIntegerWatcher` leaf the reference mounts under each counter map.
//! The reference binds the maps live (`MapWatcher` with `*` children plus
//! a `MemberWatcher<bool>`); this port mounts a stable snapshot of the
//! current statistics instead — the value layout, access modes and the
//! enable-clears side effect are preserved, and taking the snapshot again
//! refreshes it.

use std::collections::BTreeMap;

use super::path_tree::{Mode, WatcherNode, WatcherTree, WatcherValue};

/// `EventHistoryStats` (event_history_stats.hpp:22-91): tracks EventHistory
/// usage and exposes the statistics as watchers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventHistoryStats {
    is_enabled: bool,
    counts: BTreeMap<String, u32>,
    sizes: BTreeMap<String, u32>,
    oversizes: BTreeMap<String, u32>,
}

impl EventHistoryStats {
    /// A stats collector that starts disabled
    /// (event_history_stats.hpp:45, `isEnabled_( false )`).
    pub fn new() -> Self {
        Self::default()
    }

    /// `trackEvent` (event_history_stats.hpp:52-75): accumulate one event
    /// under the `typeName.memberName` key. A no-op while disabled.
    pub fn track_event(&mut self, type_name: &str, member_name: &str, size: u32, stream_size: i16) {
        if !self.is_enabled {
            return;
        }
        let key = format!("{type_name}.{member_name}");
        *self.counts.entry(key.clone()).or_insert(0) += 1;
        *self.sizes.entry(key.clone()).or_insert(0) += size;
        if stream_size < 0 && stream_size > -4 {
            // See InterfaceElement::canHandleLength (event_history_stats.hpp:65).
            let oversize_size = (1u32 << (-8 * stream_size) as u32) - 1;
            if size >= oversize_size {
                *self.oversizes.entry(key).or_insert(0) += 1;
            }
        }
    }

    /// `isEnabled` (event_history_stats.hpp:77).
    pub const fn is_enabled(&self) -> bool {
        self.is_enabled
    }

    /// `setEnabled` (event_history_stats.hpp:78-90): enabling clears the
    /// accumulated statistics, so people can perform individual actions and
    /// look at how much they affect the amount (and size) of events.
    pub fn set_enabled(&mut self, enable: bool) {
        if enable {
            self.counts.clear();
            self.sizes.clear();
            self.oversizes.clear();
        }
        self.is_enabled = enable;
    }

    /// The accumulated event counts, keyed `typeName.memberName`.
    pub const fn counts(&self) -> &BTreeMap<String, u32> {
        &self.counts
    }

    /// The accumulated event sizes, keyed `typeName.memberName`.
    pub const fn sizes(&self) -> &BTreeMap<String, u32> {
        &self.sizes
    }

    /// The oversize event counts, keyed `typeName.memberName`.
    pub const fn oversizes(&self) -> &BTreeMap<String, u32> {
        &self.oversizes
    }

    /// `init` (event_history_stats.cpp:80-100): mount the statistics under
    /// `parent_path` — read-only `counts`/`sizes`/`oversizes` directories
    /// (one integer leaf per counter) plus the read-write `enabled` toggle.
    /// Returns false when the tree refuses the path.
    pub fn init(&self, parent_path: &str, tree: &mut WatcherTree) -> bool {
        tree.add_node(parent_path, self.snapshot())
    }

    /// The stable snapshot of the current statistics: the directory the
    /// reference's `init` builds (event_history_stats.cpp:84-99), with one
    /// leaf per live counter at snapshot time.
    pub fn snapshot(&self) -> WatcherNode {
        let mut dir = WatcherNode::directory();
        dir.add("counts", counter_map_node(&self.counts));
        dir.add("sizes", counter_map_node(&self.sizes));
        dir.add("oversizes", counter_map_node(&self.oversizes));
        dir.add(
            "enabled",
            WatcherNode::value(WatcherValue::Bool(self.is_enabled), Mode::ReadWrite),
        );
        dir
    }
}

/// The `MapWatcher` + `*` `BaseIntegerWatcher<uint32>` pair
/// (event_history_stats.cpp:83-89): one read-only integer leaf per map
/// entry, labelled with the map key.
fn counter_map_node(map: &BTreeMap<String, u32>) -> WatcherNode {
    let mut dir = WatcherNode::directory();
    for (key, value) in map {
        dir.add(
            key,
            WatcherNode::value(WatcherValue::Uint(u64::from(*value)), Mode::ReadOnly),
        );
    }
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    /// event_history_stats.hpp:45 — the constructor starts disabled, so
    /// `trackEvent` is a no-op until enabled.
    #[test]
    fn a_disabled_collector_ignores_events() {
        let mut stats = EventHistoryStats::new();
        assert!(!stats.is_enabled());
        stats.track_event("Avatar", "health", 12, -2);
        assert!(stats.counts().is_empty());
        assert!(stats.sizes().is_empty());
        assert!(stats.oversizes().is_empty());
    }

    /// event_history_stats.hpp:56-59 — counts and sizes accumulate under
    /// the `typeName.memberName` key.
    #[test]
    fn tracking_accumulates_counts_and_sizes_per_key() {
        let mut stats = EventHistoryStats::new();
        stats.set_enabled(true);
        stats.track_event("Avatar", "health", 12, 0);
        stats.track_event("Avatar", "health", 8, 0);
        stats.track_event("Cell", "position", 40, 0);

        assert_eq!(stats.counts().get("Avatar.health"), Some(&2));
        assert_eq!(stats.sizes().get("Avatar.health"), Some(&20));
        assert_eq!(stats.counts().get("Cell.position"), Some(&1));
        assert_eq!(stats.sizes().get("Cell.position"), Some(&40));
    }

    /// event_history_stats.hpp:61-73 — an event whose size reaches the
    /// `InterfaceElement::canHandleLength` limit for its stream size is
    /// counted as oversize; stream sizes outside -3..-1 never are.
    #[test]
    fn oversize_events_match_the_can_handle_length_limits() {
        let mut stats = EventHistoryStats::new();
        stats.set_enabled(true);

        // streamSize -3: limit (1 << 24) - 1 = 16777215.
        stats.track_event("A", "x", 16777214, -3);
        stats.track_event("A", "x", 16777215, -3);
        assert_eq!(stats.oversizes().get("A.x"), Some(&1));

        // streamSize -2: limit (1 << 16) - 1 = 65535.
        stats.track_event("B", "y", 65535, -2);
        // streamSize -1: limit (1 << 8) - 1 = 255.
        stats.track_event("C", "z", 255, -1);
        assert_eq!(stats.oversizes().get("B.y"), Some(&1));
        assert_eq!(stats.oversizes().get("C.z"), Some(&1));

        // -4 and non-negative stream sizes fall outside the guard.
        stats.track_event("D", "w", u32::MAX, -4);
        stats.track_event("E", "v", u32::MAX, 0);
        assert!(stats.oversizes().get("D.w").is_none());
        assert!(stats.oversizes().get("E.v").is_none());
    }

    /// event_history_stats.hpp:81-90 — enabling clears the accumulated
    /// statistics (the documented side effect); disabling keeps them.
    #[test]
    fn enabling_clears_the_accumulated_statistics() {
        let mut stats = EventHistoryStats::new();
        stats.set_enabled(true);
        stats.track_event("Avatar", "health", 12, -1);
        stats.set_enabled(false);
        assert_eq!(stats.counts().get("Avatar.health"), Some(&1));

        stats.set_enabled(true);
        assert!(!stats.counts().contains_key("Avatar.health"));
        assert!(stats.sizes().is_empty());
        assert!(stats.oversizes().is_empty());
        assert!(stats.is_enabled());
    }

    /// event_history_stats.cpp:80-100 — `init` mounts `counts`/`sizes`/
    /// `oversizes` (read-only per-key integer leaves) and the read-write
    /// `enabled` toggle under the parent path.
    #[test]
    fn the_snapshot_mounts_counts_sizes_oversizes_and_enabled() {
        let mut stats = EventHistoryStats::new();
        stats.set_enabled(true);
        // streamSize -1 → limit 255, so 255 is oversize.
        stats.track_event("Avatar", "health", 255, -1);

        let mut tree = WatcherTree::new();
        assert!(stats.init("eventStats", &mut tree));

        assert_eq!(
            tree.get_string("eventStats/counts/Avatar.health"),
            Some(("1".to_string(), Mode::ReadOnly))
        );
        assert_eq!(
            tree.get_string("eventStats/sizes/Avatar.health"),
            Some(("255".to_string(), Mode::ReadOnly))
        );
        assert_eq!(
            tree.get_string("eventStats/oversizes/Avatar.health"),
            Some(("1".to_string(), Mode::ReadOnly))
        );
        assert_eq!(
            tree.get_string("eventStats/enabled"),
            Some(("true".to_string(), Mode::ReadWrite))
        );

        // The toggle is writable, like the MemberWatcher the reference
        // mounts; the live side effect goes through `set_enabled`.
        assert!(tree.set_string("eventStats/enabled", "false"));
        assert_eq!(
            tree.get_string("eventStats/enabled"),
            Some(("false".to_string(), Mode::ReadWrite))
        );
    }

    /// The snapshot is stable: values read after more `trackEvent` calls
    /// stay at their mount-time values until the snapshot is taken again
    /// (the reference reads the live maps; this port re-mounts to refresh).
    #[test]
    fn the_snapshot_is_stable_while_tracking_continues() {
        let mut stats = EventHistoryStats::new();
        stats.set_enabled(true);
        stats.track_event("Avatar", "health", 4, 0);

        let mut tree = WatcherTree::new();
        assert!(stats.init("stats", &mut tree));

        stats.track_event("Avatar", "health", 4, 0);
        assert_eq!(
            tree.get_string("stats/counts/Avatar.health"),
            Some(("1".to_string(), Mode::ReadOnly))
        );

        // A refreshed snapshot carries the new values.
        assert!(tree.remove("stats"));
        assert!(stats.init("stats", &mut tree));
        assert_eq!(
            tree.get_string("stats/counts/Avatar.health"),
            Some(("2".to_string(), Mode::ReadOnly))
        );
    }
}
