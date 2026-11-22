//! A plugin fixture that accepts its host and releases itself on unload.

use core::ffi::c_void;

/// `plugin_init`: joins the host (the reference `PluginInit` returning true).
#[unsafe(no_mangle)]
pub extern "C" fn plugin_init(_host: *mut c_void) -> bool {
    true
}

/// `plugin_fini`: lets the host release it.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_fini(_host: *mut c_void) -> bool {
    true
}

/// `plugin_fini_veto`: refuses the release, so the host must keep it loaded.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_fini_veto(_host: *mut c_void) -> bool {
    false
}