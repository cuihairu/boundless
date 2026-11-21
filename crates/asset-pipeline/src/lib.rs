//! BW-13 asset pipeline: dependency discovery, conversion and compilation
//! (`tools/asset_pipeline`).
//!
//! Ported modules:
//! - [`hash64`] — the `Hash64` FNV-1a hash used for dependency and file
//!   hashes (`bw_hash64.cpp`);
//! - [`dependency`] — the six dependency kinds and their DataSection
//!   serialisation (`dependency/*.hpp|cpp`);
//! - [`dependency_list`] — the per-task input/output dependency list
//!   (`dependency_list.hpp|cpp`);
//! - [`conversion_task`] — the conversion record and its status machine
//!   (`conversion/conversion_task.hpp`);
//! - [`conversion_rule`] — the abstract rule that maps a source file to a
//!   task (`discovery/conversion_rule.hpp`);
//! - [`task_finder`] — the recursive root-task discovery walk
//!   (`discovery/task_finder.cpp`).

pub mod conversion_rule;
pub mod conversion_task;
pub mod dependency;
pub mod dependency_list;
pub mod hash64;
pub mod task_finder;
