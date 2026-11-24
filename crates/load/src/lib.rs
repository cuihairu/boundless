//! Load-test layer (BW-17): scenario input definitions and result aggregation.
//!
//! The reference entry for Bots is `server/tools/bots/`; BW-12-04 aligns the
//! *behaviour* of those bots, while this crate carries the *load-test* face:
//! the fixed input a run is driven by (seed, account pool, concurrency shape)
//! and the metrics the results are aggregated into.

pub mod beeline;
pub mod bot;
pub mod bots_config;
pub mod dense;
pub mod executor;
pub mod fleet;

pub mod bot_watchers;
pub mod hurricane;
pub mod patrol;
pub mod report;
pub mod scenario;
pub mod steady;

pub use bot::{
    Behaviour, Bot, BotView, ConnState, Directive, FD_PER_BOT, MATH_PI, MovementController,
    MovementFactory, NULL_ENTITY_ID, TICK_FREQUENCY, TICK_PERIOD, Transport, transport_from_config,
};
pub use bots_config::{BotsConfig, OptionEntry, Value as BotOptionValue, WatcherMode};
pub use dense::{DenseAoiLedger, run_dense_aoi};
pub use executor::{Executor, StageTimeline};
pub use fleet::{ControllerRegistry, Fleet, FleetSettings, MINI_TICK_PERIOD, TICK_FRAGMENTS};
pub use hurricane::{HurricaneLedger, run_login_hurricane};
pub use scenario::{AccountPool, Scenario, ScenarioError};
pub use steady::{SteadyLedger, SteadyTrack, run_steady_movement};
