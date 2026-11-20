//! BW-18 shared test framework: fixtures, virtual time, multiprocess harness
//! and fault injection, reusable by every other lane.
//!
//! The crate also hosts the workspace dependency-direction checker
//! ([`layering`], BW-16-01-01 / BW-16-02-05), so the layering rule is guarded
//! by plain `cargo test` runs.

pub mod layering;
