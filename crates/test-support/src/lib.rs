//! BW-18 shared test framework: fixtures, virtual time, multiprocess harness
//! and fault injection, reusable by every other lane.
//!
//! The crate also hosts the workspace dependency-direction checker
//! ([`layering`], BW-16-01-01 / BW-16-02-05), so the layering rule is guarded
//! by plain `cargo test` runs.

pub mod allocation;
pub mod case_registry;
pub mod channel;
pub mod clock;
pub mod cmake_targets;
pub mod config_fixture;
pub mod harness;
pub mod layering;
pub mod multiproc;
pub mod release_artifacts;
pub mod rng;
pub mod standalone;
pub mod timer_scenarios;
pub mod workspace;
