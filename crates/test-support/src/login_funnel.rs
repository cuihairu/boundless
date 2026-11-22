//! BW-26-02-04: the login funnel — how many players reach each stage.
//!
//! A player who logs in and then starts a session, then makes a first move,
//! progresses through the funnel. A stage is reached or not reached, so a
//! retried login — the same player's account logging in again — counts the
//! player once in the stage, never once per attempt.

use crate::event_contract::{EventKind, GameEvent};
use std::collections::{BTreeMap, BTreeSet};

/// The funnel stages a login can reach, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FunnelStage {
    /// The player logged in.
    Login,
    /// The player started a session.
    Session,
    /// The player made a first move between spaces.
    FirstMove,
}

/// The login funnel across a set of events: how many distinct players reached
/// each stage. A player is keyed by their account on login and by their player
/// name on session and movement, so a retried login is the same player.
#[derive(Debug, Default)]
pub struct LoginFunnel {
    players: BTreeMap<String, BTreeSet<FunnelStage>>,
}

impl LoginFunnel {
    /// A funnel with no players.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an event: a login, session or movement moves its player to the
    /// stage the event names.
    pub fn record(&mut self, event: &GameEvent) -> &mut Self {
        let (identity, stage) = match event.kind {
            EventKind::Login => (event.field("account"), FunnelStage::Login),
            EventKind::Session => (event.field("player"), FunnelStage::Session),
            EventKind::Movement => (event.field("player"), FunnelStage::FirstMove),
            EventKind::Custom => return self,
        };
        if let Some(identity) = identity {
            self.players
                .entry(identity.to_string())
                .or_default()
                .insert(stage);
        }
        self
    }

    /// How many distinct players reached a stage.
    pub fn count(&self, stage: FunnelStage) -> usize {
        self.players
            .values()
            .filter(|reached| reached.contains(&stage))
            .count()
    }

    /// The stage counts, in funnel order.
    pub fn stages(&self) -> Vec<(FunnelStage, usize)> {
        [
            FunnelStage::Login,
            FunnelStage::Session,
            FunnelStage::FirstMove,
        ]
        .iter()
        .map(|&stage| (stage, self.count(stage)))
        .collect()
    }

    /// The funnel as one line: `login=.. session=.. first_move=..`.
    pub fn render(&self) -> String {
        format!(
            "login={} session={} first_move={}",
            self.count(FunnelStage::Login),
            self.count(FunnelStage::Session),
            self.count(FunnelStage::FirstMove),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_contract::EventRecorder;

    #[test]
    fn a_retried_login_counts_once_per_stage() {
        let mut recorder = EventRecorder::new();
        let mut funnel = LoginFunnel::new();

        // Alice logs in three times: each retry is the same account.
        funnel.record(&recorder.login(1_000, "baseapp01", "alice"));
        funnel.record(&recorder.login(1_100, "baseapp01", "alice"));
        funnel.record(&recorder.login(1_200, "baseapp01", "alice"));
        // Bob logs in once.
        funnel.record(&recorder.login(1_000, "baseapp01", "bob"));

        // The retried login counts the player once in the stage, not once per
        // attempt.
        assert_eq!(funnel.count(FunnelStage::Login), 2);
        assert_eq!(funnel.count(FunnelStage::Session), 0);

        // Alice then starts a session and moves: she is still counted once in
        // the login stage as she progresses.
        funnel.record(&recorder.session(1_300, "baseapp01", "s-1", "alice"));
        funnel.record(&recorder.movement(1_400, "cellapp01", "alice", 1, 2));
        assert_eq!(funnel.count(FunnelStage::Login), 2);
        assert_eq!(funnel.count(FunnelStage::Session), 1);
        assert_eq!(funnel.count(FunnelStage::FirstMove), 1);
        assert_eq!(
            funnel.stages(),
            vec![
                (FunnelStage::Login, 2),
                (FunnelStage::Session, 1),
                (FunnelStage::FirstMove, 1),
            ]
        );
        assert_eq!(funnel.render(), "login=2 session=1 first_move=1");
    }

    #[test]
    fn players_progress_through_the_stages() {
        let mut recorder = EventRecorder::new();
        let mut funnel = LoginFunnel::new();
        for (index, account) in ["alice", "bob"].iter().enumerate() {
            let at = 1_000 + index as u64;
            funnel.record(&recorder.login(at, "baseapp01", account));
            funnel.record(&recorder.session(at + 100, "baseapp01", "s-1", account));
            funnel.record(&recorder.movement(at + 200, "cellapp01", account, 1, 2));
        }

        // Every player that logged in also started and moved.
        assert_eq!(funnel.count(FunnelStage::Login), 2);
        assert_eq!(funnel.count(FunnelStage::Session), 2);
        assert_eq!(funnel.count(FunnelStage::FirstMove), 2);
    }

    #[test]
    fn a_player_is_counted_once_even_across_many_events() {
        let mut recorder = EventRecorder::new();
        let mut funnel = LoginFunnel::new();
        // Carol logs in and moves twice, but each stage counts her once.
        funnel.record(&recorder.login(1_000, "baseapp01", "carol"));
        funnel.record(&recorder.movement(1_100, "cellapp01", "carol", 1, 2));
        funnel.record(&recorder.movement(1_200, "cellapp01", "carol", 2, 3));

        assert_eq!(funnel.count(FunnelStage::Login), 1);
        assert_eq!(funnel.count(FunnelStage::FirstMove), 1);
        assert_eq!(funnel.count(FunnelStage::Session), 0);
    }
}
