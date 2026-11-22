//! A plugin fixture whose init refuses the host, so the loader must roll the
//! load back.

use core::ffi::c_void;

/// `plugin_init`: refuses to join the host.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_init(_host: *mut c_void) -> bool {
    false
}

/// `plugin_fini`: the loader runs it while rolling the load back.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_fini(_host: *mut c_void) -> bool {
    true
}