//! Load-test layer (BW-17): scenario input definitions and result aggregation.
//!
//! The reference entry for Bots is `server/tools/bots/`; BW-12-04 aligns the
//! *behaviour* of those bots, while this crate carries the *load-test* face:
//! the fixed input a run is driven by (seed, account pool, concurrency shape)
//! and the metrics the results are aggregated into.

pub mod bot;
pub mod executor;
pub mod scenario;

pub use bot::{transport_from_config, Bot, ConnState, NULL_ENTITY_ID, TICK_FREQUENCY, TICK_PERIOD,
    Transport, FD_PER_BOT};
pub use executor::{Executor, StageTimeline};
pub use scenario::{AccountPool, Scenario, ScenarioError};
