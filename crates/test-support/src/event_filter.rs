//! BW-26-02-01: validating and filtering events for the analysis CLI.
//!
//! The CLI reads events and hands an analyst two things: the events that fail
//! the contract (each named with the first reason it failed) and the events a
//! filter selects. Both are deterministic — the same input and the same
//! filter yield the same sets, in the order given — so a run is reproducible.

use crate::event_contract::{ContractError, EventKind, GameEvent, verify};

/// One validation failure: the event's position in the input and the reason it
/// failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidEvent {
    /// The event's position in the input.
    pub index: usize,
    /// Why the event did not meet the contract.
    pub error: ContractError,
}

/// Validate events: the contract-valid ones pass, and each invalid one is
/// named with the first reason it failed. Deterministic for a fixed input.
pub fn validate(events: &[GameEvent]) -> (Vec<&GameEvent>, Vec<InvalidEvent>) {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for (index, event) in events.iter().enumerate() {
        match verify(event) {
            Ok(()) => valid.push(event),
            Err(error) => invalid.push(InvalidEvent { index, error }),
        }
    }
    (valid, invalid)
}

/// A filter over events: every criterion set must match; a criterion unset
/// matches everything.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    kind: Option<EventKind>,
    source: Option<String>,
    since: Option<u64>,
    until: Option<u64>,
    field: Option<(String, String)>,
}

impl Filter {
    /// A filter that matches every event.
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep only events of a kind.
    pub fn kind(mut self, kind: EventKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Keep only events from a source.
    pub fn source(mut self, source: &str) -> Self {
        self.source = Some(source.to_string());
        self
    }

    /// Keep only events that happened at or after a time.
    pub fn since(mut self, since: u64) -> Self {
        self.since = Some(since);
        self
    }

    /// Keep only events that happened at or before a time.
    pub fn until(mut self, until: u64) -> Self {
        self.until = Some(until);
        self
    }

    /// Keep only events whose contract field carries a value.
    pub fn field(mut self, name: &str, value: &str) -> Self {
        self.field = Some((name.to_string(), value.to_string()));
        self
    }

    /// Whether an event meets every criterion set.
    pub fn matches(&self, event: &GameEvent) -> bool {
        if let Some(kind) = self.kind
            && event.kind != kind
        {
            return false;
        }
        if let Some(source) = &self.source
            && &event.source != source
        {
            return false;
        }
        if let Some(since) = self.since
            && event.at < since
        {
            return false;
        }
        if let Some(until) = self.until
            && event.at > until
        {
            return false;
        }
        if let Some((name, value)) = &self.field
            && event.field(name) != Some(value.as_str())
        {
            return false;
        }
        true
    }

    /// The events matching the filter, in the order given.
    pub fn select<'a>(&self, events: &'a [GameEvent]) -> Vec<&'a GameEvent> {
        events.iter().filter(|event| self.matches(event)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_contract::EventRecorder;

    fn sample() -> Vec<GameEvent> {
        let mut recorder = EventRecorder::new();
        vec![
            recorder.login(1_000, "baseapp01", "alice"),
            recorder.movement(1_200, "cellapp01", "alice", 1, 2),
            recorder.session(900, "baseapp01", "s-1", "alice"),
            recorder.custom(
                1_000,
                "cellapp01",
                "purchase",
                &[("item", "sword")],
                &["item"],
            ),
        ]
    }

    #[test]
    fn validation_names_each_bad_event_with_its_reason() {
        let mut recorder = EventRecorder::new();
        let mut bad = recorder.login(1_000, "baseapp01", "alice");
        bad.version = "2".to_string();
        let mut missing = recorder.movement(1_200, "cellapp01", "alice", 1, 2);
        missing.fields.retain(|(name, _)| name != "to_space");
        let events = vec![
            recorder.login(1_000, "baseapp01", "alice"),
            bad.clone(),
            missing.clone(),
            recorder.session(1_100, "baseapp01", "s-1", "alice"),
        ];

        // The valid ones pass in order; each bad one is named with the reason
        // that failed it.
        let (valid, invalid) = validate(&events);
        assert_eq!(valid.len(), 2);
        assert_eq!(valid[0].id, "login:2");
        assert_eq!(valid[1].id, "session:1");
        assert_eq!(
            invalid,
            vec![
                InvalidEvent {
                    index: 1,
                    error: ContractError::UnknownVersion
                },
                InvalidEvent {
                    index: 2,
                    error: ContractError::MissingField("to_space")
                },
            ]
        );
    }

    #[test]
    fn a_filter_selects_only_matching_events_in_order() {
        let events = sample();

        // By kind: only the movements.
        let movements = Filter::new().kind(EventKind::Movement).select(&events);
        assert_eq!(movements.len(), 1);
        assert_eq!(movements[0].id, "movement:1");

        // By source: only the baseapp events.
        let base = Filter::new().source("baseapp01").select(&events);
        let ids: Vec<&str> = base.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["login:1", "session:1"]);

        // By time window: the events of [1000, 1000].
        let window = Filter::new().since(1_000).until(1_000).select(&events);
        let ids: Vec<&str> = window.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["login:1", "custom:1"]);

        // By contract field value.
        let purchases = Filter::new()
            .kind(EventKind::Custom)
            .field("name", "purchase")
            .select(&events);
        assert_eq!(purchases.len(), 1);
        assert_eq!(purchases[0].id, "custom:1");

        // The empty filter matches everything.
        assert_eq!(Filter::new().select(&events).len(), 4);
    }

    #[test]
    fn bad_events_and_valid_filters_are_reproducible() {
        let mut recorder = EventRecorder::new();
        let mut bad = recorder.login(1_000, "baseapp01", "alice");
        bad.version = "2".to_string();
        let events = vec![bad, recorder.login(1_000, "baseapp01", "alice")];

        // Running validation twice over the same input yields the same verdict.
        let (valid_a, invalid_a) = validate(&events);
        let (valid_b, invalid_b) = validate(&events);
        let ids_a: Vec<&str> = valid_a.iter().map(|e| e.id.as_str()).collect();
        let ids_b: Vec<&str> = valid_b.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids_a, ids_b);
        assert_eq!(invalid_a, invalid_b);

        // A filter applied twice selects the same events.
        let filter = Filter::new().kind(EventKind::Login).source("baseapp01");
        let one: Vec<&str> = filter
            .select(&events)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        let two: Vec<&str> = filter
            .select(&events)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(one, two);
    }
}
