//! BW-13 asset pipeline: dependency discovery, conversion and compilation
//! (`tools/asset_pipeline`).
//!
//! Ported modules:
//! - [`hash64`] — the `Hash64` FNV-1a hash used for dependency and file
//!   hashes (`bw_hash64.cpp`);
//! - [`dependency`] — the six dependency kinds and their DataSection
//!   serialisation (`dependency/*.hpp|cpp`);
//! - [`dependency_list`] — the per-task input/output dependency list
//!   (`dependency_list.hpp|cpp`).

pub mod dependency;
pub mod dependency_list;
pub mod hash64;
