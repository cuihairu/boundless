//! The reconnect scenario (BW-17-02-07): the server drops online sessions
//! and the run reports how long each bot takes to come back online.
//!
//! Reference face: `ClientApp::onLoggedOff` (`client_app.cpp:1193-1235`) —
//! the transport cut is delivered as an event, the personality script
//! decides whether the bot parks dormant for another attempt or is
//! destroyed, and the bot that stays re-issues `logOn`. The offline model
//! composes the same faces the other load scenarios do: the real
//! [`load::Bot`] connection state machine (`ConnState` over queued
//! `ConnEvent`s, applied by the tick's connection update,
//! `client_app.cpp:431-503`) and the fleet's seeded admission. Every bot is
//! brought online, then dropped once at a seeded tick inside the load
//! window; the reconnect behaviour re-issues `logOn` right after the drop
//! lands, the server answers one tick later, and the ledger records per bot
//! the drop tick, the re-login tick and the tick it was online again — the
//! recovery length the acceptance asks to count.
//!
//! A fixed seed fixes the fleet's speed draws and each bot's drop tick, so
//! a re-run of the same scenario reproduces the ledger bit for bit — the
//! same reproducibility contract [`crate::smoke`] enforces on the other
//! faces.

use crate::bot::{ConnState, FD_PER_BOT, TICK_PERIOD};
use crate::fleet::Fleet;
use crate::scenario::Scenario;

/// The cut the server delivers: the session ends, and the personality
/// script's dormant decision arrives with the event (`feed_logged_off`,
/// `client_app.cpp:1193-1235`). `Some(true)` parks the bot for another
/// attempt — the behaviour every reconnect bot installs.
pub const RECONNECT_KEEP_ALIVE: Option<bool> = Some(true);

/// The server's answer latency for a re-login attempt, in ticks: the reply
/// is queued one tick after the attempt is issued, so it lands on the
/// update after that — the offline floor for a recovery is
/// `1 + RELOGIN_LATENCY_TICKS` ticks.
const RELOGIN_LATENCY_TICKS: u32 = 1;

/// One bot's recovery track: when it was dropped, when it re-logged in, and
/// when it was online again.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryTrack {
    /// The bot's fleet name.
    pub name: String,
    /// The tick index at whose update the session was dropped.
    pub dropped_at_tick: u32,
    /// The tick index at which the re-login attempt was issued.
    pub relogin_at_tick: u32,
    /// The tick index at whose update the bot was online again
    /// (`None` = still down at the end of the window).
    pub back_online_at_tick: Option<u32>,
}

/// The reconnect run's record: one track per bot, in fleet order.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconnectLedger {
    /// The wall duration each sample covers (the mini-tick).
    pub sample_period: f32,
    /// The tracks, in fleet admission order.
    pub tracks: Vec<RecoveryTrack>,
}

impl ReconnectLedger {
    /// How many bots were dropped and came back online inside the window.
    pub fn recoveries_completed(&self) -> usize {
        self.tracks
            .iter()
            .filter(|t| t.back_online_at_tick.is_some())
            .count()
    }

    /// The mean recovery length in ticks over the bots that came back —
    /// the acceptance's statistic. A run with no completed recovery has no
    /// length to report (`0.0`).
    pub fn mean_recovery_ticks(&self) -> f64 {
        let mut total = 0.0f64;
        let mut done = 0usize;
        for track in &self.tracks {
            if let Some(back) = track.back_online_at_tick {
                total += (back - track.dropped_at_tick) as f64;
                done += 1;
            }
        }
        if done == 0 { 0.0 } else { total / done as f64 }
    }
}

/// Run the reconnect scenario: bring `concurrency` bots online, drop every
/// session once at a seeded tick inside the load window, and record each
/// bot's recovery track.
///
/// The fd budget is sized like the other faces' so an offline run stays
/// refusal-free; a refused admission simply never joins the fleet (it has
/// no session to drop).
pub fn run_reconnect(scenario: &Scenario, fd_limit: i32, seed: u64) -> ReconnectLedger {
    let mut fleet = Fleet::with_seed("recon", fd_limit, seed);
    let peak = scenario.concurrency as usize;
    let mut fd_open = 0i32;
    for _ in 0..peak {
        if fleet.add_bot() {
            fd_open += FD_PER_BOT;
            fleet.set_fd_open(fd_open);
        }
    }

    // A settled fleet: the server answers every log-on, each bot gets its
    // entity and space. The tick loop below brings these home.
    let entity_ids: Vec<u32> = (1..=fleet.len() as u32).collect();
    for (index, bot) in fleet.bots_mut().iter_mut().enumerate() {
        bot.feed_log_on_result(entity_ids[index], 1);
    }

    let warmup_ticks = (scenario.warmup.as_secs_f32() / TICK_PERIOD) as u32;
    let window_ticks = (scenario.duration.as_secs_f32() / TICK_PERIOD) as u32;

    // The seeded drop schedule: one drop tick per bot inside the window,
    // spread over the first half so a recovery has a full half to land in.
    let span = (window_ticks / 2).max(1);
    let schedule: Vec<u32> = (0..fleet.len() as u32)
        .map(|index| (index * 37) % span)
        .collect();

    for _ in 0..warmup_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
    }

    // The per-bot reconnect bookkeeping: whether the re-login has been
    // issued, and when the bot was online again. `NOT_ISSUED` is a tick
    // index no re-login can carry (the window cannot reach it).
    const NOT_ISSUED: u32 = u32::MAX;
    let mut relogin_at: Vec<u32> = vec![NOT_ISSUED; fleet.len()];
    let mut back_online_at: Vec<Option<u32>> = vec![None; fleet.len()];

    for tick in 0..window_ticks {
        // Queue the drops scheduled for this tick; the update below
        // applies them (Idle + dormant — the reconnect bot parks).
        for (index, when) in schedule.iter().enumerate() {
            if *when == tick {
                fleet.bots_mut()[index].feed_logged_off(RECONNECT_KEEP_ALIVE);
            }
        }
        // One connection update: drops land, queued replies turn sessions
        // online.
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
        for index in 0..fleet.len() {
            let bot = &fleet.bots()[index];
            // Back online: the reply landed on this update.
            if back_online_at[index].is_none()
                && relogin_at[index] != NOT_ISSUED
                && bot.conn_state() == ConnState::Online
            {
                back_online_at[index] = Some(tick);
            }
            // The reconnect behaviour: a bot that is down at or after its
            // drop and has not re-issued yet does so now — the event has
            // already been applied, so `logOn` wakes it into LoggingIn.
            if relogin_at[index] == NOT_ISSUED
                && bot.conn_state() == ConnState::Idle
                && schedule[index] <= tick
            {
                fleet.bots_mut()[index].log_on();
                relogin_at[index] = tick;
            }
        }
        // The server answers an attempt that has been in flight for its
        // latency: the reply is queued now and lands on the next update.
        for index in 0..fleet.len() {
            if relogin_at[index] != NOT_ISSUED
                && relogin_at[index] + RELOGIN_LATENCY_TICKS <= tick
                && fleet.bots()[index].conn_state() == ConnState::LoggingIn
            {
                fleet.bots_mut()[index].feed_log_on_result(entity_ids[index], 1);
            }
        }
    }

    // Compose the tracks from what actually happened: a drop that never
    // came (its tick is outside the window) is not recorded as one.
    let tracks = fleet
        .bots()
        .iter()
        .enumerate()
        .map(|(index, bot)| RecoveryTrack {
            name: bot.user_name().to_owned(),
            dropped_at_tick: schedule[index].min(window_ticks.saturating_sub(1)),
            relogin_at_tick: relogin_at[index].min(window_ticks.saturating_sub(1)),
            back_online_at_tick: back_online_at[index],
        })
        .collect();

    ReconnectLedger {
        sample_period: TICK_PERIOD,
        tracks,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::scenario::AccountPool;

    fn scenario(concurrency: u64, duration: Duration, seed: u64) -> Scenario {
        Scenario {
            name: "reconnect".to_string(),
            fixed_seed: seed,
            account_pool: AccountPool::Generated {
                prefix: "Recon".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate: 0,
            warmup: Duration::from_secs(0),
            duration,
        }
    }

    /// The acceptance face: the ledger reports the recovery statistic.
    /// Every dropped bot comes back online inside the window, at the
    /// offline floor of drop + relogin + reply.
    #[test]
    fn the_ledger_reports_the_recovery_length() {
        let scenario = scenario(4, Duration::from_secs(60), 141421);
        let ledger = run_reconnect(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.tracks.len(), 4);
        assert_eq!(ledger.recoveries_completed(), 4);
        assert_eq!(ledger.mean_recovery_ticks(), 2.0);
        for track in &ledger.tracks {
            let back = track
                .back_online_at_tick
                .expect("every bot came back online");
            assert_eq!(track.relogin_at_tick, track.dropped_at_tick);
            assert_eq!(back, track.dropped_at_tick + 2);
        }
    }

    /// A fixed seed generates the same ledger.
    #[test]
    fn the_fixed_seed_generates_the_same_ledger() {
        let scenario = scenario(4, Duration::from_secs(60), 141421);
        let a = run_reconnect(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        let b = run_reconnect(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(a, b);
    }

    /// A window too short to serve the reply strands the recovery: the
    /// re-login is in flight when the window closes, so the recovery never
    /// completes and the mean has no length to report.
    #[test]
    fn a_short_window_strands_the_recovery() {
        // 200ms = 2 ticks: the drop lands at tick 0, the re-login is issued
        // at tick 0, the reply would land at tick 2 — outside the window.
        let scenario = scenario(4, Duration::from_millis(200), 141421);
        let ledger = run_reconnect(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.recoveries_completed(), 0);
        assert_eq!(ledger.mean_recovery_ticks(), 0.0);
        assert!(
            ledger
                .tracks
                .iter()
                .all(|t| t.back_online_at_tick.is_none())
        );
    }

    /// The ledger's shape follows the scenario: one track per bot, in
    /// fleet order, each with its drop tick inside the first half of the
    /// window.
    #[test]
    fn the_ledger_shape_follows_the_scenario() {
        let scenario = scenario(3, Duration::from_secs(60), 99);
        let ledger = run_reconnect(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.sample_period, TICK_PERIOD);
        assert_eq!(ledger.tracks.len(), 3);
        assert_eq!(ledger.tracks[0].name, "Bot_1_recon");
        for track in &ledger.tracks {
            assert!(track.dropped_at_tick < 300);
        }
    }

    /// The FD budget bounds the fleet: a refused admission never joins,
    /// so the fleet shrinks — and the one bot that did still recovers.
    #[test]
    fn a_refused_admission_shrinks_the_fleet() {
        let scenario = scenario(4, Duration::from_secs(60), 141421);
        let ledger = run_reconnect(&scenario, FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.tracks.len(), 1);
        assert_eq!(ledger.recoveries_completed(), 1);
    }
}
