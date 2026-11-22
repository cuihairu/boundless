//! BW-26-02-03: session duration, with cross-day and still-open rules.
//!
//! A session is one uninterrupted run. Its duration is the elapsed time
//! between opening and closing, counted in full even when the session crosses
//! a day boundary — never sliced at midnight. A session that has not ended has
//! no duration yet: it is surfaced as open and left out of the counted totals
//! until it closes.

use crate::session_metrics::SessionWindow;

/// Whether a session spans a day boundary, under a day index function: the day
/// its session opened in is not the day it closed in.
pub fn crosses_day(started: u64, ended: u64, day: impl Fn(u64) -> u64) -> bool {
    day(started) != day(ended)
}

/// The duration rules applied to a set of session windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationSummary {
    /// Sessions closed and counted.
    pub closed: usize,
    /// Sessions still open, with no duration yet.
    pub open: usize,
    /// The total elapsed time of the closed sessions, in full.
    pub total: u64,
    /// The longest closed session's elapsed time.
    pub longest: u64,
}

impl DurationSummary {
    /// The summary of the windows: each closed session counts its elapsed time
    /// in full, and each open one is surfaced, not counted.
    pub fn of(windows: &[SessionWindow]) -> Self {
        let mut summary = Self {
            closed: 0,
            open: 0,
            total: 0,
            longest: 0,
        };
        for window in windows {
            if let Some(duration) = window.duration() {
                summary.closed += 1;
                summary.total += duration;
                summary.longest = summary.longest.max(duration);
            } else {
                summary.open += 1;
            }
        }
        summary
    }

    /// The average duration of a closed session, when any closed.
    pub fn average(&self) -> Option<u64> {
        (self.closed > 0).then(|| self.total / self.closed as u64)
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
    fn a_closed_sessions_duration_is_its_elapsed_time() {
        // The duration is the time between opening and closing.
        let brief = window("s-1", "alice", 100, Some(250));
        assert_eq!(brief.duration(), Some(150));
        assert!(!brief.is_open());
    }

    #[test]
    fn a_cross_day_session_counts_its_whole_length() {
        // A day is 1,000 ticks in this fixture. The session opens at 995 in
        // one day and closes at 1,010 in the next.
        let day = |at: u64| at / 1_000;
        let crossing = window("s-1", "alice", 995, Some(1_010));
        assert!(crosses_day(995, 1_010, day));
        assert_eq!(crossing.duration(), Some(15));

        // The whole 15 ticks count, not a slice per day: slicing at midnight
        // would read 5 + 10 and lose the session's run as one length.
        let same_day = window("s-2", "bob", 900, Some(999));
        assert!(!crosses_day(900, 999, day));
        assert_eq!(same_day.duration(), Some(99));

        // And the summary counts the crossing session in full.
        let summary = DurationSummary::of(&[crossing, same_day]);
        assert_eq!(summary.closed, 2);
        assert_eq!(summary.total, 114);
        assert_eq!(summary.longest, 99);
    }

    #[test]
    fn an_open_session_has_no_duration_until_it_closes() {
        let mut session = window("s-1", "alice", 100, None);

        // Still open: no duration, surfaced as open, counted nowhere.
        assert!(session.is_open());
        assert_eq!(session.duration(), None);
        let before = DurationSummary::of(&[session.clone()]);
        assert_eq!(before.open, 1);
        assert_eq!(before.closed, 0);
        assert_eq!(before.total, 0);
        assert_eq!(before.average(), None);

        // Once it closes, its elapsed time counts.
        session.ended = Some(340);
        let after = DurationSummary::of(&[session]);
        assert_eq!(after.closed, 1);
        assert_eq!(after.open, 0);
        assert_eq!(after.total, 240);
        assert_eq!(after.average(), Some(240));
    }

    #[test]
    fn the_summary_averages_the_closed_sessions() {
        let sessions = [
            window("s-1", "alice", 100, Some(200)),
            window("s-2", "bob", 300, Some(400)),
            window("s-3", "carol", 500, None),
        ];

        // Two closed at 100 each, one still open: the totals count the closed
        // and surface the open.
        let summary = DurationSummary::of(&sessions);
        assert_eq!(summary.closed, 2);
        assert_eq!(summary.open, 1);
        assert_eq!(summary.total, 200);
        assert_eq!(summary.longest, 100);
        assert_eq!(summary.average(), Some(100));
    }
}
