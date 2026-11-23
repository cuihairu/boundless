//! BW-26-02-05: space heat — how crowded each space is, and how that buckets.
//!
//! A space is hot when players are in it. Occupancy moves with movement: a
//! player leaving a space takes it out of that space's count, entering puts
//! it in the new one's. A heat bucket classifies the count, so a fixed set of
//! locations always falls into the same buckets.

use crate::event_contract::{EventKind, GameEvent};
use std::collections::BTreeMap;

/// The lowest occupancy at which a space reads as hot.
pub const HOT_AT: usize = 5;

/// A heat bucket, from cold to hot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HeatBucket {
    /// No one is in the space.
    Cold,
    /// A few players are in the space.
    Warm,
    /// The space is crowded.
    Hot,
}

/// The bucket a space falls into, by its occupancy.
pub fn bucket_of(occupancy: usize) -> HeatBucket {
    match occupancy {
        0 => HeatBucket::Cold,
        _ if occupancy < HOT_AT => HeatBucket::Warm,
        _ => HeatBucket::Hot,
    }
}

/// Space heat: who is in each space, read off movement events.
#[derive(Debug, Default)]
pub struct SpaceHeat {
    occupancy: BTreeMap<u64, usize>,
    present: BTreeMap<String, u64>,
}

impl SpaceHeat {
    /// No occupancy recorded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a movement event: the player leaves the space they were in and
    /// enters the space they moved to.
    pub fn record(&mut self, event: &GameEvent) -> &mut Self {
        if event.kind != EventKind::Movement {
            return self;
        }
        let player = match event.field("player") {
            Some(player) => player.to_string(),
            None => return self,
        };
        let from = event
            .field("from_space")
            .and_then(|space| space.parse().ok());
        let to = event.field("to_space").and_then(|space| space.parse().ok());
        if let (Some(from), Some(current)) = (from, self.present.get(&player))
            && *current == from
        {
            self.leave(from);
        }
        if let Some(to) = to {
            self.enter(to, &player);
        }
        self
    }

    /// How many players are in a space.
    pub fn occupancy(&self, space: u64) -> usize {
        self.occupancy.get(&space).copied().unwrap_or(0)
    }

    /// The bucket a space falls into, by its occupancy.
    pub fn bucket(&self, space: u64) -> HeatBucket {
        bucket_of(self.occupancy(space))
    }

    /// The spaces read out with their occupancy, in space order.
    pub fn spaces(&self) -> Vec<(u64, usize)> {
        self.occupancy
            .iter()
            .map(|(space, count)| (*space, *count))
            .collect()
    }

    /// The most occupied space, when any space holds players.
    pub fn hottest(&self) -> Option<(u64, usize)> {
        self.occupancy
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(space, count)| (*space, *count))
    }

    fn enter(&mut self, space: u64, player: &str) {
        *self.occupancy.entry(space).or_insert(0) += 1;
        self.present.insert(player.to_string(), space);
    }

    fn leave(&mut self, space: u64) {
        if let Some(count) = self.occupancy.get_mut(&space) {
            *count = count.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_contract::EventRecorder;

    #[test]
    fn a_fixed_location_fixture_falls_into_the_right_bucket() {
        let mut recorder = EventRecorder::new();
        let mut heat = SpaceHeat::new();

        // A fixed fixture: players gather into spaces 1, 2 and 3, and space 4
        // is left empty.
        let players = ["alice", "bob", "carol", "dan", "eve", "frank"];
        let targets = [2u64, 2, 3, 3, 4, 4];
        for (index, player) in players.iter().enumerate() {
            heat.record(&recorder.movement(
                1_000 + index as u64,
                "cellapp01",
                player,
                0,
                targets[index],
            ));
        }

        // Each fixed location falls into the bucket its occupancy names: two
        // players in each of spaces 2, 3 and 4, the drained start space and an
        // unseen space stay empty.
        assert_eq!(heat.occupancy(0), 0);
        assert_eq!(heat.occupancy(2), 2);
        assert_eq!(heat.occupancy(3), 2);
        assert_eq!(heat.occupancy(4), 2);
        assert_eq!(heat.bucket(0), HeatBucket::Cold);
        assert_eq!(heat.bucket(2), HeatBucket::Warm);
        assert_eq!(heat.bucket(3), HeatBucket::Warm);
        assert_eq!(heat.bucket(4), HeatBucket::Warm);
        assert_eq!(heat.bucket(99), HeatBucket::Cold);
    }

    #[test]
    fn bucket_boundaries_classify_the_counts() {
        // Empty is cold, a few is warm, crowded is hot.
        assert_eq!(bucket_of(0), HeatBucket::Cold);
        assert_eq!(bucket_of(1), HeatBucket::Warm);
        assert_eq!(bucket_of(HOT_AT - 1), HeatBucket::Warm);
        assert_eq!(bucket_of(HOT_AT), HeatBucket::Hot);
        assert_eq!(bucket_of(HOT_AT + 1), HeatBucket::Hot);
    }

    #[test]
    fn a_move_takes_presence_out_of_one_space_and_into_another() {
        let mut recorder = EventRecorder::new();
        let mut heat = SpaceHeat::new();
        heat.record(&recorder.movement(1_000, "cellapp01", "alice", 1, 2));
        assert_eq!(heat.occupancy(1), 0);
        assert_eq!(heat.occupancy(2), 1);
        assert_eq!(heat.hottest(), Some((2, 1)));

        // A second player joins space 2, then alice leaves it: the count
        // follows the players.
        heat.record(&recorder.movement(1_100, "cellapp01", "bob", 3, 2));
        assert_eq!(heat.occupancy(2), 2);
        heat.record(&recorder.movement(1_200, "cellapp01", "alice", 2, 4));
        assert_eq!(heat.occupancy(2), 1);
        assert_eq!(heat.occupancy(4), 1);
    }
}
