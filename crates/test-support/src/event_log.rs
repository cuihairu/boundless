//! BW-26-01-04: the event log that deduplicates and orders arrivals.
//!
//! Events reach the recorder once at emission, but a retry can re-send the
//! same event and a delivery can arrive late — after events that happened
//! later have already been recorded. The log keeps each emission once, keyed
//! by its id, and hands events back in chronological order, so a fixed set of
//! events yields the same view however it arrives.

use crate::event_contract::GameEvent;
use std::collections::BTreeMap;

/// How an ingested event was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    /// The event is new to the log and stored.
    Stored,
    /// An event of the same id was already stored; the retry is not stored
    /// again.
    Duplicate,
}

/// The events seen so far, each kept once and handed back in order.
///
/// A retry re-sends an event with the id it already carries, so the log keeps
/// the first copy and names the re-send a duplicate. A late event is stored
/// like any other and takes its place in chronological order — the log never
/// drops an event because it arrived after a later one.
#[derive(Default)]
pub struct EventLog {
    events: BTreeMap<String, GameEvent>,
}

impl EventLog {
    /// A log with no events.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest an event: store it unless an event of the same id is already
    /// stored. Returns how the ingest was handled.
    pub fn ingest(&mut self, event: GameEvent) -> Ingest {
        if self.events.contains_key(&event.id) {
            return Ingest::Duplicate;
        }
        self.events.insert(event.id.clone(), event);
        Ingest::Stored
    }

    /// How many distinct events the log holds.
    pub fn count(&self) -> usize {
        self.events.len()
    }

    /// Whether the log holds an event of the given id.
    pub fn has(&self, id: &str) -> bool {
        self.events.contains_key(id)
    }

    /// The event an id names, when the log holds it.
    pub fn by_id(&self, id: &str) -> Option<&GameEvent> {
        self.events.get(id)
    }

    /// The held events in chronological order: by time, then by id for the
    /// events of one moment. Total for a fixed set — an event's id is unique
    /// to its emission, so the ordering is the same however the set arrived.
    pub fn in_order(&self) -> Vec<&GameEvent> {
        let mut events: Vec<&GameEvent> = self.events.values().collect();
        events.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_contract::EventRecorder;

    #[test]
    fn duplicate_events_are_ingested_once() {
        let mut recorder = EventRecorder::new();
        let login = recorder.login(1_000, "baseapp01", "alice");
        let mut log = EventLog::new();

        // The first copy is stored; the re-send of the same emission is a
        // duplicate and is not stored again.
        assert_eq!(log.ingest(login.clone()), Ingest::Stored);
        assert_eq!(log.ingest(login), Ingest::Duplicate);
        assert_eq!(log.count(), 1);
        assert!(log.has("login:1"));
        assert_eq!(
            log.by_id("login:1").unwrap().field("account"),
            Some("alice")
        );
    }

    #[test]
    fn late_events_take_their_place_in_order() {
        let mut recorder = EventRecorder::new();
        let mut log = EventLog::new();

        // The first event happened at 100, then a later one at 150 arrives,
        // then an earlier one at 50 arrives late. The late event is stored and
        // takes its chronological place — never dropped for being late.
        assert_eq!(
            log.ingest(recorder.login(100, "baseapp01", "alice")),
            Ingest::Stored
        );
        assert_eq!(
            log.ingest(recorder.movement(150, "cellapp01", "alice", 1, 2)),
            Ingest::Stored
        );
        assert_eq!(
            log.ingest(recorder.session(50, "baseapp01", "s-1", "alice")),
            Ingest::Stored
        );

        let order: Vec<&str> = log.in_order().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, vec!["session:1", "login:1", "movement:1"]);
    }

    #[test]
    fn events_of_one_moment_are_ordered_by_id() {
        let mut recorder = EventRecorder::new();
        let mut log = EventLog::new();
        log.ingest(recorder.login(200, "baseapp01", "bob"));
        log.ingest(recorder.login(200, "baseapp01", "alice"));

        // Two events of the same moment: the id breaks the tie, so the order
        // is deterministic.
        let order: Vec<&str> = log.in_order().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, vec!["login:1", "login:2"]);
    }

    #[test]
    fn a_fixed_set_yields_the_same_order_however_it_arrives() {
        let mut recorder = EventRecorder::new();
        // A fixed set of events: mixed kinds, mixed moments, one moment
        // shared, and one event re-sent to test dedup.
        let events = vec![
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
            recorder.login(1_200, "baseapp01", "bob"),
        ];
        let duplicate = events[0].clone();

        // Ingest the set forward, then again in reverse with a re-send: the
        // two logs hold the same distinct events and hand them back in the
        // same order.
        let mut forward = EventLog::new();
        for event in &events {
            forward.ingest(event.clone());
        }
        let mut reversed = EventLog::new();
        for event in events.iter().rev() {
            reversed.ingest(event.clone());
        }
        assert_eq!(reversed.ingest(duplicate), Ingest::Duplicate);

        let forward_order: Vec<&str> = forward.in_order().iter().map(|e| e.id.as_str()).collect();
        let reversed_order: Vec<&str> = reversed.in_order().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(forward_order, reversed_order);
        assert_eq!(forward.count(), 5);
        assert_eq!(reversed.count(), 5);
    }
}
