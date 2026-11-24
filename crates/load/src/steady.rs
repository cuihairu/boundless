//! The steady-state movement scenario: every bot walks its idle circle at
//! full concurrency, and the trajectory is a pure function of the seed.
//!
//! Reference face: a logged-in Bots client with no controller auto-moves on
//! the radius-10 circle (`server/tools/bots/client_app.cpp:1106-1134`),
//! stepping once per mini-tick (`TICK_PERIOD`), with the circle's period set
//! by the constructor's speed draw (`:385-387`). The fleet draws those speeds
//! from the seeded xorshift stream (BW-17-01-03), so a fixed seed fixes every
//! period, and the circle law is pure float math off the client clock — the
//! recorded trajectory reproduces bit for bit.

use crate::bot::{FD_PER_BOT, TICK_PERIOD};
use crate::fleet::Fleet;
use crate::scenario::Scenario;

/// One bot's steady-state track: its speed and the sampled circle points.
#[derive(Debug, Clone, PartialEq)]
pub struct SteadyTrack {
    /// The bot's fleet name.
    pub name: String,
    /// The constructor's speed draw (the seed's fingerprint).
    pub speed: f32,
    /// Sampled x coordinates, one per mini-tick of the steady window.
    pub xs: Vec<f32>,
    /// Sampled z coordinates, one per mini-tick of the steady window.
    pub zs: Vec<f32>,
    /// Sampled yaws, one per mini-tick of the steady window.
    pub yaws: Vec<f32>,
}

/// The steady-state run's record: one track per bot, in fleet order.
#[derive(Debug, Clone, PartialEq)]
pub struct SteadyLedger {
    /// The wall duration each sample covers (the mini-tick).
    pub sample_period: f32,
    /// The tracks, in fleet admission order.
    pub tracks: Vec<SteadyTrack>,
}

/// Run the steady-state movement scenario: bring `concurrency` bots online,
/// walk them through the warmup, sample every mini-tick of the load window.
pub fn run_steady_movement(scenario: &Scenario, fd_limit: i32, seed: u64) -> SteadyLedger {
    let mut fleet = Fleet::with_seed("steady", fd_limit, seed);
    let peak = scenario.concurrency as usize;
    let mut fd_open = 0i32;
    for _ in 0..peak {
        if fleet.add_bot() {
            fd_open += FD_PER_BOT;
            fleet.set_fd_open(fd_open);
        }
    }
    // The steady scenario starts from a settled fleet: the server answers
    // every log-on (results are the connection model's to feed), each bot
    // gets its entity and space, and the idle circles start turning.
    let entity_ids: Vec<u32> = (1..=peak as u32).collect();
    for (index, bot) in fleet.bots_mut().iter_mut().enumerate() {
        bot.feed_log_on_result(entity_ids[index], 1);
    }

    let warmup_ticks = (scenario.warmup.as_secs_f32() / TICK_PERIOD) as u32;
    let steady_ticks = (scenario.duration.as_secs_f32() / TICK_PERIOD) as u32;
    for _ in 0..warmup_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
    }

    let mut ledger = SteadyLedger {
        sample_period: TICK_PERIOD,
        tracks: Vec::with_capacity(peak),
    };
    for bot in fleet.bots() {
        ledger.tracks.push(SteadyTrack {
            name: bot.user_name().to_owned(),
            speed: bot.speed(),
            xs: Vec::with_capacity(steady_ticks as usize),
            zs: Vec::with_capacity(steady_ticks as usize),
            yaws: Vec::with_capacity(steady_ticks as usize),
        });
    }
    for _ in 0..steady_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
        for (index, bot) in fleet.bots().iter().enumerate() {
            let track = &mut ledger.tracks[index];
            track.xs.push(bot.position().x);
            track.zs.push(bot.position().z);
            track.yaws.push(bot.yaw());
        }
    }
    ledger
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::scenario::AccountPool;

    fn scenario(concurrency: u64, warmup: u64, duration: u64, seed: u64) -> Scenario {
        Scenario {
            name: "steady-movement".to_string(),
            fixed_seed: seed,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate: 0,
            warmup: Duration::from_secs(warmup),
            duration: Duration::from_secs(duration),
        }
    }

    /// The acceptance face: a fixed seed generates the same trajectory.
    /// Every coordinate is pure seeded math, so the ledgers compare equal
    /// bit for bit.
    #[test]
    fn the_fixed_seed_generates_the_same_trajectory() {
        let scenario = scenario(4, 1, 2, 99);
        let a = run_steady_movement(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        let b = run_steady_movement(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(a, b);
    }

    /// A different seed redraws the speeds, and with them the circle
    /// periods — the trajectories diverge while the shape stays.
    #[test]
    fn another_seed_redraws_the_trajectories() {
        let first = scenario(4, 1, 2, 5);
        let second = scenario(4, 1, 2, 6);
        let a = run_steady_movement(&first, 500 * FD_PER_BOT, first.fixed_seed);
        let b = run_steady_movement(&second, 500 * FD_PER_BOT, second.fixed_seed);
        assert!(a != b);
        assert!(a.tracks.iter().all(|t| (t.speed - 6.0).abs() < 2.0));
    }

    /// The ledger's shape follows the scenario: one track per bot, in
    /// fleet order, sampled once per mini-tick of the load window.
    #[test]
    fn the_ledger_shape_follows_the_scenario() {
        let scenario = scenario(3, 1, 2, 11);
        let ledger = run_steady_movement(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.sample_period, TICK_PERIOD);
        assert_eq!(ledger.tracks.len(), 3);
        assert_eq!(ledger.tracks[0].name, "Bot_1_steady");
        // 2 seconds at 10 mini-ticks per second.
        for track in &ledger.tracks {
            assert_eq!(track.xs.len(), 20);
            assert_eq!(track.zs.len(), 20);
            assert_eq!(track.yaws.len(), 20);
            // The idle circle: every point sits on the radius-10 ring.
            for (x, z) in track.xs.iter().zip(&track.zs) {
                assert!((x * x + z * z - 100.0).abs() < 1e-2);
            }
        }
    }

    /// The FD budget bounds the settled fleet: a refused admission simply
    /// never joins the track list.
    #[test]
    fn the_fd_budget_bounds_the_fleet() {
        let scenario = scenario(3, 0, 1, 8);
        // Two bots fit the budget; the third admission is refused.
        let ledger = run_steady_movement(&scenario, 2 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.tracks.len(), 2);
    }

    /// The warmup is walked but not sampled: the first recorded point is
    /// the circle position one mini-tick into the load window, matching
    /// the reference law θ = clientTime·2π/period (client_app.cpp:1106).
    #[test]
    fn the_first_sample_lands_one_tick_into_the_load_window() {
        let scenario = scenario(1, 1, 1, 42);
        let ledger = run_steady_movement(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        let track = &ledger.tracks[0];
        // Speed draw for seed 42, first draw: 6 + unit·2.
        let speed = track.speed;
        let period = 10.0 * speed / 7.0;
        // Warmup 1s + one load tick = 11 mini-ticks of client clock.
        let client_time = 11.0_f32 * TICK_PERIOD;
        let angle = client_time * 2.0 * std::f32::consts::PI / period;
        let expected_x = 10.0 * angle.sin();
        let expected_z = 10.0 * angle.cos();
        assert!((track.xs[0] - expected_x).abs() < 1e-4);
        assert!((track.zs[0] - expected_z).abs() < 1e-4);
        // The yaw faces the travel direction: θ + π/2.
        assert!((track.yaws[0] - (angle + std::f32::consts::PI / 2.0)).abs() < 1e-4);
    }
}
