//! A plugin fixture whose **debug** init entry point refuses the host, so
//! the loader must roll the load back — and run the debug fini on the way
//! out. The fini leaves a marker file so the test can observe it ran.

use core::ffi::c_void;

/// Where `plugin_fini_d` records that the rollback reached it: the standard
/// temp directory, keyed by the (shared) test process id.
fn marker_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "boundless-plugin-debug-fini-{}.flag",
        std::process::id()
    ))
}

/// `plugin_init_d`: refuses to join the host through the debug name.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_init_d(_host: *mut c_void) -> bool {
    false
}

/// `plugin_fini_d`: runs during the load rollback and leaves its marker.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_fini_d(_host: *mut c_void) -> bool {
    let _ = std::fs::write(marker_path(), b"");
    true
}
