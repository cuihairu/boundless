//! Load-test layer (BW-17): scenario input definitions and result aggregation.
//!
//! The reference entry for Bots is `server/tools/bots/`; BW-12-04 aligns the
//! *behaviour* of those bots, while this crate carries the *load-test* face:
//! the fixed input a run is driven by (seed, account pool, concurrency shape)
//! and the metrics the results are aggregated into.

pub mod bot;
pub mod executor;
pub mod fleet;
pub mod scenario;

pub use bot::{
    Bot, ConnState, ControllerSpec, FD_PER_BOT, NULL_ENTITY_ID, TICK_FREQUENCY, TICK_PERIOD,
    Transport, transport_from_config,
};
pub use executor::{Executor, StageTimeline};
pub use fleet::{Fleet, FleetSettings, MINI_TICK_PERIOD, TICK_FRAGMENTS};
pub use scenario::{AccountPool, Scenario, ScenarioError};
