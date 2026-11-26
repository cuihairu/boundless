//! BW-13-05-06: the plugin host SDK combination matrix — every **enabled**
//! combination of the SDK's axes has a sample (a test that runs it against a
//! real fixture library) or a recorded gap.
//!
//! The SDK axes (`plugin.hpp` + `plugin_loader.cpp`):
//! - entry point pair: release (`plugin_init`/`plugin_fini`) vs debug
//!   (`plugin_init_d`/`plugin_fini_d`, the reference's `_DEBUG` macros);
//! - host action: load / unload / finish / list-driven init;
//! - plugin answer: accept, refuse (init veto), fini veto, no entry point,
//!   no library.
//!
//! | Combination | Sample |
//! |---|---|
//! | release pair, load accept | `tests/loader.rs` `a_plugin_that_initialises_is_loaded_and_kept` |
//! | release pair, init veto | `tests/loader.rs` `a_plugin_whose_init_refuses_is_not_registered` |
//! | release pair, no init entry | `tests/loader.rs` `a_library_without_the_requested_init_is_rolled_back` |
//! | release pair, no library | `tests/loader.rs` `a_missing_library_is_an_open_failure` |
//! | release pair, unload | `tests/loader.rs` `unload_releases_a_loaded_plugin` |
//! | release pair, fini veto | `tests/loader.rs` `a_plugin_that_refuses_to_unload_stays_loaded` |
//! | release pair, no fini entry | `tests/loader.rs` `a_plugin_without_a_fini_entry_point_unloads_anyway` |
//! | release pair, unknown handle | `tests/loader.rs` `unloading_an_unknown_index_is_a_no_op_success` |
//! | release pair, finish order | `tests/loader.rs` `finishing_unloads_in_reverse_load_order` |
//! | release pair, plugin list | `tests/loader.rs` `the_plugin_list_loads_each_entry_and_reports_failures` |
//! | either pair, missing list | `tests/loader.rs` `a_missing_plugin_list_is_an_error` |
//! | debug pair, no init entry | `tests/loader.rs` `the_debug_symbol_pair_names_the_d_entry_points` |
//! | debug pair, load accept | here: `a_debug_plugin_joins_the_host` |
//! | debug pair, unload + finish | here: `a_debug_plugin_releases_through_the_debug_fini` |
//! | debug pair, init veto (rollback runs the debug fini) | here: `a_debug_plugin_refusing_the_host_is_rolled_back_through_the_debug_fini` |
//!
//! Gaps (not enabled on this host, recorded against the reference):
//! - **Windows** (`LoadLibraryW`/`GetProcAddress`, `.dll` suffix): the
//!   reference is Windows-only; this host is `dlopen`-based and the crate is
//!   `#[cfg(unix)]`. The Windows load combination is BW-15-03.
//! - The reference's `PLUGIN_INIT_FUNC`/`PLUGIN_FINI_FUNC` export macros and
//!   `PLUGIN_GET_PROC_ADDRESS` are preprocessor faces; the Rust SDK samples
//!   are the fixture files themselves (`#[unsafe(no_mangle)] extern "C"`).

#![cfg(unix)]

use std::path::PathBuf;

use plugin_host::{LoadError, PluginLoader};

/// The `.so` a fixture source compiles to, under the build's `OUT_DIR`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("PLUGIN_FIXTURE_DIR")).join(format!("lib{name}.so"))
}

/// The marker `plugin_fixture_debug_fail`'s fini writes (same process, so the
/// same pid namespaces both sides).
fn debug_fini_marker() -> PathBuf {
    std::env::temp_dir().join(format!(
        "boundless-plugin-debug-fini-{}.flag",
        std::process::id()
    ))
}

#[test]
fn a_debug_plugin_joins_the_host() {
    let mut loader = PluginLoader::with_debug_symbols();
    let path = fixture("plugin_fixture_debug");
    let index = loader.load_plugin(path.to_str().unwrap()).expect("load");
    assert_eq!(index, 0);
    assert_eq!(loader.len(), 1);
    assert_eq!(loader.plugin_names(), &[path.to_str().unwrap()]);

    // The release pair must not resolve this library's entry points.
    let mut release = PluginLoader::new();
    let err = release.load_plugin(path.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, LoadError::NoInit(_)), "{err}");
}

#[test]
fn a_debug_plugin_releases_through_the_debug_fini() {
    let mut loader = PluginLoader::with_debug_symbols();
    let path = fixture("plugin_fixture_debug");
    loader.load_plugin(path.to_str().unwrap()).expect("load");

    // unloadPlugin resolves the fini under the loader's (debug) name.
    assert!(loader.unload_plugin(0));
    assert!(loader.is_empty());

    // finiPlugins reports the same release for a freshly loaded copy.
    loader.load_plugin(path.to_str().unwrap()).expect("reload");
    let released = loader.fini_plugins();
    assert_eq!(released, vec![path.to_str().unwrap()]);
    assert!(loader.is_empty());
}

#[test]
fn a_debug_plugin_refusing_the_host_is_rolled_back_through_the_debug_fini() {
    let marker = debug_fini_marker();
    let _ = std::fs::remove_file(&marker);

    let mut loader = PluginLoader::with_debug_symbols();
    let path = fixture("plugin_fixture_debug_fail");
    let err = loader.load_plugin(path.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, LoadError::InitVetoed), "{err}");
    assert!(
        loader.is_empty(),
        "the refused plugin leaves nothing loaded"
    );

    // The rollback ran the plugin's debug fini on the way out.
    assert!(
        marker.exists(),
        "plugin_fini_d must run while the load is rolled back"
    );
    let _ = std::fs::remove_file(&marker);
}
