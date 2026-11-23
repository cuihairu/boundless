//! Load-test layer (BW-17): scenario input definitions and result aggregation.
//!
//! The reference entry for Bots is `server/tools/bots/`; BW-12-04 aligns the
//! *behaviour* of those bots, while this crate carries the *load-test* face:
//! the fixed input a run is driven by (seed, account pool, concurrency shape)
//! and the metrics the results are aggregated into.

pub mod executor;
pub mod scenario;

pub use executor::{Executor, StageTimeline};
pub use scenario::{AccountPool, Scenario, ScenarioError};
