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

pub mod capi;
pub mod interpreter;
pub mod pyref;

pub use interpreter::{InitConfig, InitError, Interpreter};
