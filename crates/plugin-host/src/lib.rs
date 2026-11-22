//! BW-13-06-03: the dynamic plugin host, a port of the reference
//! `tools/plugin_system/` (BigWorld's `PluginLoader`).
//!
//! The host opens a shared library, resolves its init and fini entry points
//! and calls them, keeping a plugin only when its init accepts the host and
//! releasing it otherwise. See [`loader`] for the contract, the reference
//! mapping and the new-tree deviations.

#[cfg(unix)]
pub mod loader;

#[cfg(unix)]
pub use loader::{
    DEBUG_FINI_SYMBOL, DEBUG_INIT_SYMBOL, FINI_SYMBOL, INIT_SYMBOL, LoadError, PluginFiniFn,
    PluginInitFn, PluginLoader,
};
