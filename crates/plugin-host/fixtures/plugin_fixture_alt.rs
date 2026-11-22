//! A second accepting plugin fixture, so two different libraries can be
//! loaded and their unload order observed.

use core::ffi::c_void;

/// `plugin_init`: joins the host.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_init(_host: *mut c_void) -> bool {
    true
}

/// `plugin_fini`: lets the host release it.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_fini(_host: *mut c_void) -> bool {
    true
}