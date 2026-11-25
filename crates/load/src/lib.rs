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
pub mod hurricane;
pub mod metrics;
pub mod patrol;
pub mod report;
pub mod resource;
pub mod scenario;
pub mod smoke;
pub mod steady;
pub mod watchers;

pub use bot::{
    Behaviour, Bot, BotView, ConnState, Directive, FD_PER_BOT, MATH_PI, MovementController,
    MovementFactory, NULL_ENTITY_ID, TICK_FREQUENCY, TICK_PERIOD, Transport, transport_from_config,
};
pub use bots_config::{BotsConfig, OptionEntry, Value as BotOptionValue, WatcherMode};
pub use dense::{DenseAoiLedger, run_dense_aoi};
pub use executor::{Executor, StageTimeline};
pub use fleet::{ControllerRegistry, Fleet, FleetSettings, MINI_TICK_PERIOD, TICK_FRAGMENTS};
pub use hurricane::{HurricaneLedger, run_login_hurricane};
pub use metrics::{LoginAggregate, OnlineSummary, online_report_entries, online_summary};
pub use patrol::{Graph, GraphTraverser, Graphs, PatrolFactory, PatrolRandom};
pub use report::{REPORT_SCHEMA, ReportEntry, ReportValue, ScenarioReport};
pub use resource::{ResourceSample, ResourceTracker};
pub use scenario::{AccountPool, Scenario, ScenarioError};
pub use smoke::{
    MetricValue, SmokeCaps, SmokeError, SmokeRun, cap_scenario, run_smoke, smoke_metrics,
};
pub use steady::{SteadyLedger, SteadyTrack, run_steady_movement};
pub use watchers::{FleetCommandWatcher, register_fleet_commands};
