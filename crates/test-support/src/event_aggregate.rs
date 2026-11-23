//! BW-26-02-06: aggregating and reporting custom events.
//!
//! A business-custom event is a named event with free-form fields. The
//! aggregate groups them by name — how many, and the sum of one numeric field
//! over those that carry it — and the report renders the rows. Both are pure
//! functions of the input, so the same input always recomputes the same
//! report.

use crate::event_contract::{EventKind, GameEvent};
use std::collections::BTreeMap;

/// One named custom event, aggregated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameAggregate {
    /// The custom event's name.
    pub name: String,
    /// How many events carried the name.
    pub count: usize,
    /// The summed field's total over the events that carry it, when any do.
    pub total: Option<i64>,
}

/// Aggregate custom events by name: each row counts the events of a name and
/// sums one numeric field over those that carry it. Rows come back in name
/// order, so a fixed input always gives the same rows.
pub fn aggregate(events: &[GameEvent], sum_field: &str) -> Vec<NameAggregate> {
    let mut rows: BTreeMap<String, (usize, i64, usize)> = BTreeMap::new();
    for event in events {
        if event.kind != EventKind::Custom {
            continue;
        }
        let name = match event.field("name") {
            Some(name) => name.to_string(),
            None => continue,
        };
        let row = rows.entry(name).or_insert((0, 0, 0));
        row.0 += 1;
        if let Some(value) = event
            .field(sum_field)
            .and_then(|value| value.parse::<i64>().ok())
        {
            row.1 += value;
            row.2 += 1;
        }
    }
    rows.into_iter()
        .map(|(name, (count, sum, carried))| NameAggregate {
            name,
            count,
            total: (carried > 0).then_some(sum),
        })
        .collect()
}

/// Render an aggregate as a report: one line per row, in name order.
pub fn render(rows: &[NameAggregate]) -> String {
    rows.iter()
        .map(|row| match row.total {
            Some(total) => format!("{}: count={} total={}", row.name, row.count, total),
            None => format!("{}: count={}", row.name, row.count),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_contract::EventRecorder;

    #[test]
    fn aggregates_custom_events_by_name_with_totals() {
        let mut recorder = EventRecorder::new();
        let events = vec![
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
            recorder.custom(1_200, "cellapp01", "telemetry", &[], &[]),
            recorder.custom(1_300, "cellapp01", "purchase", &[], &["price"]),
        ];

        // Purchase counts three and sums the price it carried; telemetry counts
        // one and carries no price.
        let rows = aggregate(&events, "price");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "purchase");
        assert_eq!(rows[0].count, 3);
        assert_eq!(rows[0].total, Some(250));
        assert_eq!(rows[1].name, "telemetry");
        assert_eq!(rows[1].count, 1);
        assert_eq!(rows[1].total, None);
    }

    #[test]
    fn the_report_is_recomputed_identically_for_the_same_input() {
        let mut recorder = EventRecorder::new();
        let events = vec![
            recorder.custom(
                1_000,
                "cellapp01",
                "purchase",
                &[("price", "100")],
                &["price"],
            ),
            recorder.custom(1_100, "baseapp01", "join", &[("ms", "50")], &["ms"]),
            recorder.custom(
                1_200,
                "cellapp01",
                "purchase",
                &[("price", "200")],
                &["price"],
            ),
            recorder.login(1_300, "baseapp01", "alice"),
        ];

        // Aggregating and rendering twice over the same input gives the same
        // rows and the same report.
        let once = aggregate(&events, "price");
        let twice = aggregate(&events, "price");
        assert_eq!(once, twice);
        assert_eq!(render(&once), render(&twice));
        assert_eq!(render(&once), "join: count=1\npurchase: count=2 total=300");
    }

    #[test]
    fn non_custom_events_are_ignored() {
        let mut recorder = EventRecorder::new();
        let events = vec![
            recorder.login(1_000, "baseapp01", "alice"),
            recorder.session(1_100, "baseapp01", "s-1", "alice"),
            recorder.movement(1_200, "cellapp01", "alice", 1, 2),
            recorder.custom(
                1_300,
                "cellapp01",
                "purchase",
                &[("price", "100")],
                &["price"],
            ),
        ];

        let rows = aggregate(&events, "price");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "purchase");
        assert_eq!(rows[0].count, 1);
    }
}
