//! BW-03 CPython host and object bridging.
//!
//! A port of BigWorld's `lib/pyscript/` and `lib/script/` embedding layer:
//! interpreter lifecycle, the Python import path, per-role native modules,
//! script logging, the object bridge, the public script modules, and the
//! per-process script entry points with reload.
//!
//! CPython baseline: the reference tree is pinned to 3.13.16
//! (`BigWorld/docs/python-313-migration.md`). This host links the CPython the
//! build machine has and reproduces the reference's observable behaviour
//! rather than its exact revision, so the one thing that differs is recorded
//! here instead of hidden: the reference configures the interpreter through
//! the legacy pre-init globals (`Py_FrozenFlag` and friends) and deliberately
//! did not migrate to `PyConfig`; that migration is tracked as an open,
//! behaviour-level change in the ledger, and this port keeps the globals to
//! match.

pub mod binding;
pub mod capi;
pub mod gil;
pub mod import_paths;
pub mod init_time_job;
pub mod interpreter;
pub mod keepalive;
pub mod manifest;
pub mod math;
pub mod output_writer;
pub mod pyref;
pub mod sequence;
pub mod wrapper;

pub use binding::BindError;
pub use import_paths::ImportPaths;
pub use init_time_job::{
    JobError, add_fini_time_job, add_fini_time_job_at, add_init_time_job, run_fini_time_jobs,
    run_init_time_jobs,
};
pub use interpreter::{InitConfig, InitError, Interpreter};
pub use output_writer::{HookError, LogSink, ScriptOutputHook, set_log_sink};
pub use wrapper::WrapError;
