//! The BW-09-05 operational tools (`server/tools/` in the reference).
//!
//! Each tool is ported as a library module: the reference executables are
//! thin `main()` wrappers (option parsing, logging, exit codes) around the
//! logic that lives here, and the acceptance cases drive that logic through
//! the same seams the `main()`s use. The MySQL wire level stays behind the
//! [`crate::tools::sync_db::SyncDatabase`] seam — the statement log a test
//! double records is the reviewable artifact the acceptance clauses ask for.

pub mod clear_auto_load;
pub mod consolidate_dbs;
pub mod dry_run;
pub mod remove_db;
pub mod snapshot_helper;
pub mod sync_db;
pub mod transfer_db;
