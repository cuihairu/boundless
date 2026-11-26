//! A plugin fixture written against the debug entry point names — the
//! reference `plugin.hpp` under `_DEBUG` (`PLUGIN_INIT` → `PluginInit_d`).
//!
//! This is the SDK sample for the debug combination: a plugin that joins
//! the host through `plugin_init_d` and releases through `plugin_fini_d`.

use core::ffi::c_void;

/// `plugin_init_d`: joins the host through the debug entry point name.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_init_d(_host: *mut c_void) -> bool {
    true
}

/// `plugin_fini_d`: lets the host release it through the debug name.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_fini_d(_host: *mut c_void) -> bool {
    true
}
