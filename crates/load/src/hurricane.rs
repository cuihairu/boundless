//! The login hurricane: drive the fleet's log-on sends along the ramp curve.
//!
//! Reference face: a Bots client sends its log-on from its constructor
//! (`server/tools/bots/client_app.cpp:405`), so the number of sends per
//! second is exactly the number of bots the fleet admits that second, plus
//! any retry re-log-ons the fleet tick dispatches (`main_app.cpp:636-657`).
//! The per-second admission target is the executor's activation curve
//! (BW-17-02-02), so the send rate follows the scenario's `ramp_rate` up to
//! `concurrency`, bounded by the fleet's FD budget. The ledger records every
//! send, name, and speed draw; a fixed seed reproduces it bit for bit.

use std::time::Duration;

use crate::bot::FD_PER_BOT;
use crate::executor::StageTimeline;
use crate::fleet::{Fleet, MINI_TICK_PERIOD};
use crate::scenario::Scenario;

/// The dispatch record of one hurricane run.
///
/// Repeatable means two runs over the same scenario and seed produce equal
/// ledgers: the same per-second send counts, the same bot names in the same
/// order, and the same constructor speed draws.
#[derive(Debug, Clone, PartialEq)]
pub struct HurricaneLedger {
    /// Log-on sends issued during each second of the run (warmup included).
    pub per_second: Vec<u32>,
    /// The admitted bots in add order (the fleet's random-name counter).
    pub names: Vec<String>,
    /// The admitted bots' constructor speed draws (the seed's fingerprint).
    pub speeds: Vec<f32>,
    /// Bots the FD budget refused to admit.
    pub refused: u32,
}

impl HurricaneLedger {
    /// Total log-on sends across the whole run.
    pub fn total_sends(&self) -> u32 {
        self.per_second.iter().sum()
    }
}

/// Run the login hurricane: admit bots along the ramp curve and tick the
/// fleet once per second, recording every log-on send.
pub fn run_login_hurricane(scenario: &Scenario, fd_limit: i32, seed: u64) -> HurricaneLedger {
    let stages = StageTimeline::for_scenario(scenario);
    let mut fleet = Fleet::with_seed("hurricane", fd_limit, seed);
    let mut ledger = HurricaneLedger {
        per_second: Vec::new(),
        names: Vec::new(),
        speeds: Vec::new(),
        refused: 0,
    };

    let total_secs = stages.total().as_secs();
    let mut fd_open = 0i32;
    for second in 0..total_secs {
        let target = stages.active_bots_at(Duration::from_secs(second)) as usize;
        let mut sends = 0u32;
        while fleet.len() < target {
            if fleet.add_bot() {
                // Each admitted bot holds its connection's descriptors; the
                // connection model owns the real fds, the hurricane books
                // the budget the reference charges per bot.
                fd_open += FD_PER_BOT;
                fleet.set_fd_open(fd_open);
                sends += 1;
            } else {
                // The budget is spent; nothing more can go out this second.
                ledger.refused += 1;
                break;
            }
        }
        // One mini-tick per second boundary: the round-robin pass also
        // dispatches retry re-log-ons for bots the server turned away.
        let due = fleet.retry_queue_len();
        fleet.tick(fleet.local_time() + MINI_TICK_PERIOD);
        sends += (due - fleet.retry_queue_len()) as u32;
        ledger.per_second.push(sends);
    }

    ledger.names = fleet
        .bots()
        .iter()
        .map(|b| b.user_name().to_owned())
        .collect();
    ledger.speeds = fleet.bots().iter().map(|b| b.speed()).collect();
    ledger
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::AccountPool;

    fn scenario(ramp_rate: u64, concurrency: u64, seed: u64) -> Scenario {
        Scenario {
            name: "login-hurricane".to_string(),
            fixed_seed: seed,
            account_pool: AccountPool::Generated {
                prefix: "Bot".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate,
            warmup: Duration::from_secs(1),
            duration: Duration::from_secs(9),
        }
    }

    /// The acceptance face: sends go out at the specified rate, second by
    /// second — `ramp_rate` per second during the ramp, the capped tail at
    /// the peak, nothing during warmup.
    #[test]
    fn the_hurricane_sends_at_the_specified_rate() {
        let scenario = scenario(5, 12, 7);
        let ledger = run_login_hurricane(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        // 1 warmup second + 9 load seconds = 10 buckets; the ramp's first
        // second is still inside the warmup boundary the executor uses.
        assert_eq!(ledger.per_second.len(), 10);
        assert_eq!(ledger.per_second, vec![0, 0, 5, 5, 2, 0, 0, 0, 0, 0]);
        assert_eq!(ledger.total_sends(), 12);
        assert_eq!(ledger.names.len(), 12);
        assert_eq!(ledger.refused, 0);
    }

    /// A fixed seed reproduces the run bit for bit; a different seed keeps
    /// the schedule and names but redraws the constructor speeds.
    #[test]
    fn the_hurricane_is_repeatable_from_the_fixed_seed() {
        let seeded = scenario(4, 9, 123);
        let a = run_login_hurricane(&seeded, 500 * FD_PER_BOT, seeded.fixed_seed);
        let b = run_login_hurricane(&seeded, 500 * FD_PER_BOT, seeded.fixed_seed);
        assert_eq!(a, b);

        let redrawn = scenario(4, 9, 456);
        let c = run_login_hurricane(&redrawn, 500 * FD_PER_BOT, redrawn.fixed_seed);
        assert_eq!(a.per_second, c.per_second);
        assert_eq!(a.names, c.names);
        assert!(a.speeds != c.speeds);
    }

    /// The random-name counter drives the ledger's names: username,
    /// `_id`, then the app id, in admission order (main_app.cpp:401-422).
    #[test]
    fn the_ledger_names_follow_the_fleet_counter() {
        let scenario = scenario(3, 3, 1);
        let ledger = run_login_hurricane(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.names[0], "Bot_1_hurricane");
        assert_eq!(ledger.names[1], "Bot_2_hurricane");
        assert_eq!(ledger.names[2], "Bot_3_hurricane");
    }

    /// The FD budget bounds the send rate: once spent, admissions stop and
    /// every unmet second is counted as refused.
    #[test]
    fn the_fd_budget_bounds_the_send_rate() {
        let scenario = scenario(2, 6, 3);
        // Three bots fit (`fdOpen + FD_PER_BOT > fdLimit` refuses the fourth).
        let ledger = run_login_hurricane(&scenario, 3 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.names.len(), 3);
        assert!(ledger.refused > 0);
        assert_eq!(ledger.total_sends(), 3);
    }
}
