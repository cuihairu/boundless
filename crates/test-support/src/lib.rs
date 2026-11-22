//! BW-18 shared test framework: fixtures, virtual time, multiprocess harness
//! and fault injection, reusable by every other lane.
//!
//! The crate also hosts the workspace dependency-direction checker
//! ([`layering`], BW-16-01-01 / BW-16-02-05), so the layering rule is guarded
//! by plain `cargo test` runs.

pub mod allocation;
pub mod asset_fixtures;
pub mod bots_compare;
pub mod capture_session;
pub mod case_registry;
pub mod channel;
pub mod client_login;
pub mod clock;
pub mod cluster_query;
pub mod cmake_targets;
pub mod config_fixture;
pub mod cpu_sampler;
pub mod cross_reference;
pub mod diagnostic;
pub mod event_contract;
pub mod fault_presentation;
pub mod flamegraph;
pub mod full_stack;
pub mod growth;
pub mod harness;
pub mod layering;
pub mod memory_snapshot;
pub mod multiproc;
pub mod process_status;
pub mod profile_link;
pub mod protocol_fixtures;
pub mod python_callbacks;
pub mod python_profile;
pub mod queue_state;
pub mod recovery;
pub mod release_artifacts;
pub mod request_capture;
pub mod rng;
pub mod runtime_env;
pub mod space_cell;
pub mod standalone;
pub mod startup_baseline;
pub mod stats_report;
pub mod third_party_deps;
pub mod timer_scenarios;
pub mod trace_json;
pub mod trace_timeline;
pub mod wait_capture;
pub mod windows_baseline;
pub mod workspace;
