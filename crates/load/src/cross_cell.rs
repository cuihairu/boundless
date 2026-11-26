//! The cross-cell scenario (BW-17-02-06): bots walk a boundary between two
//! adjacent cells and the run reports the crossing success rate.
//!
//! Reference face: a bot crossing a cell boundary hands its entity from one
//! cell's range list to the other's — the AoI enter/leave events the Witness
//! forwards (`server/cellapp/range_list.cpp`, `witness.cpp:3586-3614`).
//! The offline model here composes the same three faces the BW-10-07-05
//! crossing harness composes: the real [`load::Bot`] movement
//! (`ClientApp::tick`, `server/tools/bots/client_app.cpp:441-487`), two
//! adjacent rectangular cell regions (`CellInfo::contains` over `BW::Rect`,
//! `cell_info.hpp:58`), and a per-cell AoI sentinel per bot so the crossing
//! is observable as "the bot left one cell's side and arrived in the other's".
//!
//! The scenario's own shape drives the run: every bot starts at the same
//! offset inside cell A, walks east through the boundary into cell B, and the
//! ledger records, per bot, whether its trajectory crossed the boundary and
//! ended on the far side. A fixed seed fixes the fleet's speed draws, so a
//! re-run of the same scenario reproduces the ledger bit for bit — the same
//! reproducibility contract [`crate::smoke`] enforces on the other faces.

use crate::bot::{FD_PER_BOT, TICK_PERIOD};
use crate::fleet::Fleet;
use crate::scenario::Scenario;
use foundation::math::Vector3;

/// The cell boundary this scenario walks across (`x = 0`; cell A covers
/// `x < 0`, cell B covers `x >= 0`).
pub const CELL_BOUNDARY_X: f32 = 0.0;

/// One bot's crossing track: where it started, where it ended, and whether
/// its trajectory crossed the boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossingTrack {
    /// The bot's fleet name.
    pub name: String,
    /// The bot's starting x (the same cell-A offset, before its speed draw).
    pub start_x: f32,
    /// The bot's final x, east of the boundary for a completed crossing.
    pub end_x: f32,
    /// The first tick index at which the bot stood east of the boundary
    /// (`None` = it never crossed).
    pub crossed_at_tick: Option<u32>,
    /// The bot's speed draw — the seed's fingerprint in the ledger.
    pub speed: f32,
}

/// The cross-cell run's record: one track per bot, in fleet order.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossingLedger {
    /// The wall duration each sample covers (the mini-tick).
    pub sample_period: f32,
    /// The cell boundary the bots walk across.
    pub boundary_x: f32,
    /// The tracks, in fleet admission order.
    pub tracks: Vec<CrossingTrack>,
}

impl CrossingLedger {
    /// How many bots completed the crossing (crossed the boundary and ended
    /// east of it).
    pub fn crossings_completed(&self) -> usize {
        self.tracks
            .iter()
            .filter(|t| t.crossed_at_tick.is_some() && t.end_x > self.boundary_x)
            .count()
    }

    /// The crossing success rate: completed crossings over bots that set
    /// out. A fleet that never set out has no rate to report (`0.0`).
    pub fn crossing_success_rate(&self) -> f64 {
        if self.tracks.is_empty() {
            return 0.0;
        }
        self.crossings_completed() as f64 / self.tracks.len() as f64
    }
}

/// Run the cross-cell scenario: bring `concurrency` bots online inside cell
/// A, walk each east across the boundary into cell B for the load window,
/// and record each bot's crossing track.
///
/// The fd budget is sized like the other faces' so an offline run stays
/// refusal-free; a refused admission simply never sets out and never joins
/// the ledger (it has no crossing to report).
pub fn run_cross_cell(scenario: &Scenario, fd_limit: i32, seed: u64) -> CrossingLedger {
    let mut fleet = Fleet::with_seed("cross", fd_limit, seed);
    let peak = scenario.concurrency as usize;
    let mut fd_open = 0i32;
    for _ in 0..peak {
        if fleet.add_bot() {
            fd_open += FD_PER_BOT;
            fleet.set_fd_open(fd_open);
        }
    }
    // A settled fleet: the server answers every log-on, each bot gets its
    // entity and space, and the walk starts from the same cell-A offset.
    let entity_ids: Vec<u32> = (1..=peak as u32).collect();
    for (index, bot) in fleet.bots_mut().iter_mut().enumerate() {
        bot.feed_log_on_result(entity_ids[index], 1);
    }

    // Every bot starts at the same west offset and heads for the mirror
    // offset east of the boundary. The tick drives the real movement face
    // (moveTo stepping at the bot's own speed), so the crossing tick is the
    // bot's own speed draw, not a scheduling artefact.
    const START_X: f32 = -300.0;
    const DEST_X: f32 = 300.0;
    const LANE_WIDTH: f32 = 200.0;
    let warmup_ticks = (scenario.warmup.as_secs_f32() / TICK_PERIOD) as u32;
    let window_ticks = (scenario.duration.as_secs_f32() / TICK_PERIOD) as u32;

    let mut tracks: Vec<CrossingTrack> = Vec::with_capacity(fleet.len());
    for (lane, bot) in fleet.bots_mut().iter_mut().enumerate() {
        bot.tick(0.0);
        let z = -LANE_WIDTH + ((lane as u64 * 37) % 400) as f32;
        bot.snap_to(Vector3::new(START_X, 0.0, z));
        bot.move_to(Vector3::new(DEST_X, 0.0, z));
        tracks.push(CrossingTrack {
            name: bot.user_name().to_owned(),
            start_x: START_X,
            end_x: START_X,
            crossed_at_tick: None,
            speed: bot.speed(),
        });
    }

    for _ in 0..warmup_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
    }
    for tick in 0..window_ticks {
        for bot in fleet.bots_mut() {
            bot.tick(TICK_PERIOD);
        }
        for (index, bot) in fleet.bots().iter().enumerate() {
            let x = bot.position().x;
            let track = &mut tracks[index];
            track.end_x = x;
            if track.crossed_at_tick.is_none() && x > CELL_BOUNDARY_X {
                track.crossed_at_tick = Some(tick);
            }
        }
    }

    CrossingLedger {
        sample_period: TICK_PERIOD,
        boundary_x: CELL_BOUNDARY_X,
        tracks,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::scenario::AccountPool;

    fn scenario(concurrency: u64, warmup: u64, duration: u64, seed: u64) -> Scenario {
        Scenario {
            name: "cross-cell".to_string(),
            fixed_seed: seed,
            account_pool: AccountPool::Generated {
                prefix: "Cross".to_string(),
                count: concurrency.max(1),
                password: String::new(),
            },
            concurrency,
            ramp_rate: 0,
            warmup: Duration::from_secs(warmup),
            duration: Duration::from_secs(duration),
        }
    }

    /// The acceptance face: the ledger reports the crossing success rate.
    /// Every bot that sets out reaches the far side inside the window, so
    /// the rate is 1.0 and is carried in the recorded metrics.
    #[test]
    fn the_ledger_reports_the_crossing_success_rate() {
        let scenario = scenario(4, 0, 60, 161803);
        let ledger = run_cross_cell(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.tracks.len(), 4);
        assert_eq!(ledger.crossings_completed(), 4);
        assert!((ledger.crossing_success_rate() - 1.0).abs() < 1e-9);
        for track in &ledger.tracks {
            assert!(track.crossed_at_tick.is_some(), "every bot crossed");
            assert!(track.end_x > ledger.boundary_x);
            assert!(track.end_x <= 300.0 + 1.0);
        }
    }

    /// A fixed seed generates the same ledger. The crossing tick is the
    /// bot's own speed draw, so the whole ledger compares bit for bit.
    #[test]
    fn the_fixed_seed_generates_the_same_ledger() {
        let scenario = scenario(4, 0, 60, 161803);
        let a = run_cross_cell(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        let b = run_cross_cell(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(a, b);
    }

    /// A too-short window strands the slowest bot west of the boundary: the
    /// rate drops below 1.0, and the stranded bot is named by its track's
    /// `crossed_at_tick: None`. This is the failure the rate exists to
    /// report.
    #[test]
    fn a_short_window_strands_the_slowest_and_lowers_the_rate() {
        let scenario = scenario(4, 0, 5, 161803);
        let ledger = run_cross_cell(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert!(
            ledger.crossing_success_rate() < 1.0,
            "a 5s window cannot walk 600 units at walking speed"
        );
        assert!(ledger.tracks.iter().any(|t| t.crossed_at_tick.is_none()));
    }

    /// The ledger's shape follows the scenario: one track per bot, in
    /// fleet order, each with its starting offset west of the boundary.
    #[test]
    fn the_ledger_shape_follows_the_scenario() {
        let scenario = scenario(3, 0, 60, 99);
        let ledger = run_cross_cell(&scenario, 500 * FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.sample_period, TICK_PERIOD);
        assert_eq!(ledger.boundary_x, CELL_BOUNDARY_X);
        assert_eq!(ledger.tracks.len(), 3);
        assert_eq!(ledger.tracks[0].name, "Bot_1_cross");
        for track in &ledger.tracks {
            assert_eq!(track.start_x, -300.0);
        }
    }

    /// The FD budget bounds the fleet: a refused admission never sets out
    /// and never joins the ledger, so the fleet shrinks. The rate over the
    /// fleet that did set out stays complete — a capacity failure would
    /// show as a short ledger, not as a rate failure, which is the
    /// contract this face reports.
    #[test]
    fn a_refused_admission_shrinks_the_fleet() {
        let scenario = scenario(4, 0, 60, 161803);
        // One bot's worth of fd headroom: every admission after the first
        // is refused, so only one bot sets out.
        let ledger = run_cross_cell(&scenario, FD_PER_BOT, scenario.fixed_seed);
        assert_eq!(ledger.tracks.len(), 1);
        assert_eq!(ledger.crossing_success_rate(), 1.0);
    }
}
