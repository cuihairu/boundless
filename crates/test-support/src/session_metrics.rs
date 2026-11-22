//! BW-26-02-02: online population and peak concurrency from session windows.
//!
//! A session is a window: when it opened, and when it closed — or still open.
//! How many players are online at a moment is how many windows cover it, and
//! the peak concurrency is the highest such number reachable. Both are pure
//! functions of the windows, so a fixed set of windows always gives the same
//! result.

use crate::event_contract::GameEvent;
use std::collections::BTreeMap;

/// One session window: when it opened and closed, and who it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWindow {
    /// The session id naming the window.
    pub session: String,
    /// The player the session belongs to.
    pub player: String,
    /// When the session opened.
    pub started: u64,
    /// When it closed; None while it is still open.
    pub ended: Option<u64>,
}

impl SessionWindow {
    /// Whether the session is still open.
    pub fn is_open(&self) -> bool {
        self.ended.is_none()
    }
}

/// The session windows seen, keyed by session id.
#[derive(Default)]
pub struct SessionRegistry {
    windows: BTreeMap<String, SessionWindow>,
}

impl SessionRegistry {
    /// A registry with no sessions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a window directly.
    pub fn add(&mut self, window: SessionWindow) -> &mut Self {
        self.windows.insert(window.session.clone(), window);
        self
    }

    /// Apply a session event: one that names the state `ended` closes the
    /// window its session id names, any other opens the window.
    pub fn apply(&mut self, event: &GameEvent) -> &mut Self {
        let session = match event.field("session") {
            Some(session) => session.to_string(),
            None => return self,
        };
        let player = event.field("player").unwrap_or("").to_string();
        if event.field("state") == Some("ended") {
            if let Some(window) = self.windows.get_mut(&session) {
                window.ended = Some(event.at);
            }
        } else {
            self.windows.insert(
                session.clone(),
                SessionWindow {
                    session,
                    player,
                    started: event.at,
                    ended: None,
                },
            );
        }
        self
    }

    /// The windows held, in session id order.
    pub fn windows(&self) -> Vec<&SessionWindow> {
        self.windows.values().collect()
    }

    /// How many players are online at a moment: those whose window opened at
    /// or before it and has not closed at or before it, an open window
    /// included. A closed window stops counting at the moment it closed.
    pub fn online_at(&self, at: u64) -> usize {
        self.windows
            .values()
            .filter(|window| window.started <= at && window.ended.is_none_or(|end| at < end))
            .count()
    }

    /// The peak concurrency: the highest number online at any moment. Swept
    /// over the moments sessions open — the online count only ever rises there,
    /// so the peak is reached at one of them.
    pub fn peak(&self) -> usize {
        let mut moments: Vec<u64> = self.windows.values().map(|window| window.started).collect();
        moments.sort_unstable();
        moments
            .iter()
            .map(|moment| self.online_at(*moment))
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(session: &str, player: &str, started: u64, ended: Option<u64>) -> SessionWindow {
        SessionWindow {
            session: session.to_string(),
            player: player.to_string(),
            started,
            ended,
        }
    }

    #[test]
    fn online_counts_only_the_windows_that_cover_a_moment() {
        let mut registry = SessionRegistry::new();
        registry.add(window("s-1", "alice", 100, Some(300)));
        registry.add(window("s-2", "bob", 200, Some(250)));
        registry.add(window("s-3", "carol", 400, None));

        // Only the windows that cover a moment count.
        assert_eq!(registry.online_at(150), 1);
        assert_eq!(registry.online_at(200), 2);
        // A closed window stops counting at the moment it closed.
        assert_eq!(registry.online_at(250), 1);
        assert_eq!(registry.online_at(350), 0);
        // An open window counts from its start, whenever asked.
        assert_eq!(registry.online_at(400), 1);
        assert_eq!(registry.online_at(1_000), 1);
    }

    #[test]
    fn the_peak_is_the_highest_moment_online() {
        let mut registry = SessionRegistry::new();
        registry.add(window("s-1", "alice", 100, Some(300)));
        registry.add(window("s-2", "bob", 200, Some(250)));
        registry.add(window("s-3", "carol", 120, Some(260)));
        registry.add(window("s-4", "dan", 500, Some(600)));

        // Three windows overlap at 200: that is the peak.
        assert_eq!(registry.online_at(199), 2);
        assert_eq!(registry.online_at(200), 3);
        assert_eq!(registry.peak(), 3);
        // And a single-session registry peaks at one.
        let mut alone = SessionRegistry::new();
        alone.add(window("s-9", "eve", 1, Some(2)));
        assert_eq!(alone.peak(), 1);
        assert_eq!(SessionRegistry::new().peak(), 0);
    }

    #[test]
    fn sessions_apply_from_events_and_an_open_one_still_counts() {
        let mut recorder = crate::event_contract::EventRecorder::new();
        // A session event names its state to open or close the window.
        let mut opened = recorder.session(100, "baseapp01", "s-1", "alice");
        opened
            .fields
            .push(("state".to_string(), "started".to_string()));
        let ended = {
            let mut ended = recorder.session(300, "baseapp01", "s-1", "alice");
            ended
                .fields
                .push(("state".to_string(), "ended".to_string()));
            ended
        };

        let mut registry = SessionRegistry::new();
        registry.apply(&opened);
        assert_eq!(registry.online_at(150), 1);
        registry.apply(&ended);
        assert_eq!(registry.online_at(150), 1);
        assert_eq!(registry.online_at(300), 0);

        // A session never closed is still open, so it still counts.
        let open = recorder.session(400, "baseapp01", "s-2", "bob");
        registry.apply(&open);
        assert_eq!(registry.online_at(400), 1);
        assert_eq!(registry.online_at(900), 1);

        // Exactly one session is still open: the one never closed.
        let open = registry
            .windows()
            .iter()
            .filter(|window| window.is_open())
            .count();
        assert_eq!(open, 1);
    }
}
