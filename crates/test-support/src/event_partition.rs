//! BW-26-03-03: partitioning an event stream into day batches.
//!
//! A data job imports the stream in batches — one per day, or one per chunk —
//! and the aggregate must not change with how the stream is split. Partitioning
//! groups events by the day they happened, in day order, keeping each day's
//! events in arrival order.

use crate::event_contract::GameEvent;
use std::collections::BTreeMap;

/// Group events into day batches by a day index function. Batches come back in
/// day order, and each holds its day's events in arrival order.
pub fn partition_by_day(
    events: &[GameEvent],
    day: impl Fn(u64) -> u64,
) -> Vec<Vec<&GameEvent>> {
    let mut days: BTreeMap<u64, Vec<&GameEvent>> = BTreeMap::new();
    for event in events {
        days.entry(day(event.at)).or_default().push(event);
    }
    days.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_contract::EventRecorder;
    use crate::event_import::Import;

    fn sample() -> Vec<GameEvent> {
        let mut recorder = EventRecorder::new();
        // Events across three days (a day is 1,000 ticks in this fixture).
        vec![
            recorder.custom(
                1_050,
                "cellapp01",
                "purchase",
                &[("price", "100")],
                &["price"],
            ),
            recorder.custom(
                1_150,
                "cellapp01",
                "purchase",
                &[("price", "150")],
                &["price"],
            ),
            recorder.custom(1_250, "cellapp01", "telemetry", &[], &[]),
            recorder.custom(2_050, "baseapp01", "join", &[("ms", "50")], &["ms"]),
            recorder.custom(
                3_050,
                "cellapp01",
                "purchase",
                &[("price", "30")],
                &["price"],
            ),
        ]
    }

    fn into_owned(events: &[&GameEvent]) -> Vec<GameEvent> {
        events.iter().map(|event| (*event).clone()).collect()
    }

    #[test]
    fn a_day_batch_holds_only_its_days_events() {
        let events = sample();
        let day = |at: u64| at / 1_000;

        let days = partition_by_day(&events, day);
        assert_eq!(days.len(), 3);
        // Each batch holds only its day's events, in arrival order.
        assert_eq!(days[0].len(), 3);
        assert_eq!(days[1].len(), 1);
        assert_eq!(days[2].len(), 1);
        assert_eq!(days[1][0].field("name"), Some("join"));
    }

    #[test]
    fn date_partitioning_and_batch_size_do_not_change_the_counts() {
        let events = sample();
        let day = |at: u64| at / 1_000;

        // One-shot: everything in a single batch.
        let mut one_shot = Import::new();
        one_shot.import(&events);
        let whole = one_shot.report("price");
        let whole_ids: Vec<&str> = one_shot.in_order().iter().map(|e| e.id.as_str()).collect();

        // Day-partitioned: one batch per day, imported in day order.
        let mut by_day = Import::new();
        for batch in partition_by_day(&events, day) {
            by_day.import(&into_owned(&batch));
        }
        assert_eq!(by_day.report("price"), whole);
        assert_eq!(by_day.count(), one_shot.count());

        // Chunked: fixed-size batches, however the stream splits.
        let mut chunked = Import::new();
        for chunk in events.chunks(2) {
            chunked.import(chunk);
        }
        assert_eq!(chunked.report("price"), whole);
        let chunked_ids: Vec<&str> = chunked.in_order().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(chunked_ids, whole_ids);
    }
}
