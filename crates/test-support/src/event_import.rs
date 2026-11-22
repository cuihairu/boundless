//! BW-26-03-01: importing event batches exactly once.
//!
//! An import hands events to a job. Re-sending a batch — a retry, a re-run of
//! the job — must add nothing: each event is stored once, so the aggregate
//! does not grow when the same batch is imported again.

use crate::event_aggregate::{self, NameAggregate};
use crate::event_contract::GameEvent;
use crate::event_log::{EventLog, Ingest};

/// How one import handled a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    /// Events newly stored.
    pub stored: usize,
    /// Events dropped as duplicates of one already stored.
    pub duplicates: usize,
}

/// An import: events ingested once each, so a re-import of a batch adds
/// nothing and the aggregate does not grow.
#[derive(Default)]
pub struct Import {
    log: EventLog,
}

impl Import {
    /// An empty import.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a batch of events: each event is stored once; a re-send is a
    /// duplicate and does not grow the import.
    pub fn import(&mut self, batch: &[GameEvent]) -> ImportReport {
        let mut report = ImportReport {
            stored: 0,
            duplicates: 0,
        };
        for event in batch {
            match self.log.ingest(event.clone()) {
                Ingest::Stored => report.stored += 1,
                Ingest::Duplicate => report.duplicates += 1,
            }
        }
        report
    }

    /// How many distinct events the import holds.
    pub fn count(&self) -> usize {
        self.log.count()
    }

    /// The imported events, in order.
    pub fn in_order(&self) -> Vec<&GameEvent> {
        self.log.in_order()
    }

    /// The imported events aggregated by name, summing a numeric field.
    pub fn aggregates(&self, sum_field: &str) -> Vec<NameAggregate> {
        let events: Vec<GameEvent> = self
            .in_order()
            .iter()
            .map(|event| (*event).clone())
            .collect();
        event_aggregate::aggregate(&events, sum_field)
    }

    /// The report text for the aggregate: the same input yields the same text.
    pub fn report(&self, sum_field: &str) -> String {
        event_aggregate::render(&self.aggregates(sum_field))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_contract::EventRecorder;

    fn batch(recorder: &mut EventRecorder) -> Vec<GameEvent> {
        vec![
            recorder.custom(
                1_000,
                "cellapp01",
                "purchase",
                &[("price", "100")],
                &["price"],
            ),
            recorder.custom(
                1_100,
                "cellapp01",
                "purchase",
                &[("price", "150")],
                &["price"],
            ),
            recorder.custom(1_200, "baseapp01", "join", &[("ms", "50")], &["ms"]),
            recorder.custom(1_300, "cellapp01", "telemetry", &[], &[]),
        ]
    }

    #[test]
    fn re_importing_a_batch_adds_nothing() {
        let mut recorder = EventRecorder::new();
        let events = batch(&mut recorder);
        let mut import = Import::new();

        let first = import.import(&events);
        assert_eq!(
            first,
            ImportReport {
                stored: 4,
                duplicates: 0
            }
        );
        let report = import.report("price");
        assert_eq!(
            report,
            "join: count=1\npurchase: count=2 total=250\ntelemetry: count=1"
        );

        // The whole batch is sent again: every event is a duplicate and the
        // aggregate does not grow.
        let second = import.import(&events);
        assert_eq!(
            second,
            ImportReport {
                stored: 0,
                duplicates: 4
            }
        );
        assert_eq!(import.count(), 4);
        assert_eq!(import.report("price"), report);
    }

    #[test]
    fn a_new_batch_is_imported_once() {
        let mut recorder = EventRecorder::new();
        let mut import = Import::new();

        // A second batch of distinct events is stored once each.
        let second = vec![
            recorder.custom(
                2_000,
                "cellapp01",
                "purchase",
                &[("price", "20")],
                &["price"],
            ),
            recorder.custom(
                2_100,
                "cellapp01",
                "purchase",
                &[("price", "30")],
                &["price"],
            ),
        ];
        let report = import.import(&second);
        assert_eq!(
            report,
            ImportReport {
                stored: 2,
                duplicates: 0
            }
        );
        assert_eq!(import.report("price"), "purchase: count=2 total=50");
    }
}
