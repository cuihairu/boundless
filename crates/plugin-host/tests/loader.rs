//! BW-13-06-03: the reference plugin_system's load success and load failure
//! behaviour, reproduced against real shared libraries the build compiles
//! (see `build.rs`).

#![cfg(unix)]

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use plugin_host::{DEBUG_FINI_SYMBOL, DEBUG_INIT_SYMBOL, INIT_SYMBOL, LoadError, PluginLoader};

/// The `.so` a fixture source compiles to, under the build's `OUT_DIR`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("PLUGIN_FIXTURE_DIR")).join(format!("lib{name}.so"))
}

#[test]
fn a_plugin_that_initialises_is_loaded_and_kept() {
    let mut loader = PluginLoader::new();
    let path = fixture("plugin_fixture_ok");
    let index = loader.load_plugin(path.to_str().unwrap()).expect("load");
    assert_eq!(index, 0);
    assert_eq!(loader.len(), 1);
    assert!(!loader.is_empty());
    assert_eq!(loader.plugin_names()[0], path.to_str().unwrap());
    let released = loader.fini_plugins();
    assert_eq!(released, vec![path.to_str().unwrap()]);
    assert!(loader.is_empty());
}

#[test]
fn a_missing_library_is_an_open_failure() {
    let mut loader = PluginLoader::default();
    let err = loader
        .load_plugin("/nonexistent/libboundless-no-such-plugin.so")
        .unwrap_err();
    assert!(matches!(err, LoadError::Open(_)), "{err}");
    assert!(loader.is_empty());
}

#[test]
fn a_library_without_the_requested_init_is_rolled_back() {
    let mut loader = PluginLoader::with_symbols("no_such_init", "no_such_fini");
    let path = fixture("plugin_fixture_ok");
    let err = loader.load_plugin(path.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, LoadError::NoInit(_)), "{err}");
    assert!(loader.is_empty());
}

#[test]
fn a_plugin_whose_init_refuses_is_not_registered() {
    let mut loader = PluginLoader::new();
    let path = fixture("plugin_fixture_fail");
    let err = loader.load_plugin(path.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, LoadError::InitVetoed), "{err}");
    assert!(loader.is_empty());
}

#[test]
fn unload_releases_a_loaded_plugin() {
    let mut loader = PluginLoader::new();
    let path = fixture("plugin_fixture_ok");
    loader.load_plugin(path.to_str().unwrap()).unwrap();
    assert!(loader.unload_plugin(0));
    assert!(loader.is_empty());
    assert!(loader.plugin_names().is_empty());
}

#[test]
fn a_plugin_that_refuses_to_unload_stays_loaded() {
    let mut loader = PluginLoader::with_symbols(INIT_SYMBOL, "plugin_fini_veto");
    let path = fixture("plugin_fixture_ok");
    loader.load_plugin(path.to_str().unwrap()).unwrap();
    assert!(!loader.unload_plugin(0), "the fini vetoes the unload");
    assert_eq!(loader.len(), 1);
}

#[test]
fn unloading_an_unknown_index_is_a_no_op_success() {
    let mut loader = PluginLoader::new();
    assert!(loader.unload_plugin(0));
    assert!(loader.unload_plugin(7));
}

#[test]
fn a_plugin_without_a_fini_entry_point_unloads_anyway() {
    let mut loader = PluginLoader::with_symbols(INIT_SYMBOL, "no_such_fini");
    let path = fixture("plugin_fixture_ok");
    loader.load_plugin(path.to_str().unwrap()).unwrap();
    assert!(
        loader.unload_plugin(0),
        "no fini to veto, so the release proceeds"
    );
    assert!(loader.is_empty());
}

#[test]
fn finishing_keeps_a_plugin_that_vetoes_unloading() {
    let mut loader = PluginLoader::with_symbols(INIT_SYMBOL, "plugin_fini_veto");
    let path = fixture("plugin_fixture_ok");
    loader.load_plugin(path.to_str().unwrap()).unwrap();
    let released = loader.fini_plugins();
    assert!(
        released.is_empty(),
        "the vetoed plugin is not reported released"
    );
    assert_eq!(loader.len(), 1, "the vetoed plugin stays loaded");
}

#[test]
fn finishing_unloads_in_reverse_load_order() {
    let mut loader = PluginLoader::new();
    let ok = fixture("plugin_fixture_ok").to_str().unwrap().to_owned();
    let alt = fixture("plugin_fixture_alt").to_str().unwrap().to_owned();
    loader.load_plugin(&ok).unwrap();
    loader.load_plugin(&alt).unwrap();
    assert_eq!(loader.plugin_names(), &[ok.as_str(), alt.as_str()]);
    let released = loader.fini_plugins();
    assert_eq!(released, vec![alt, ok], "last loaded is released first");
    assert!(loader.is_empty());
}

#[test]
fn the_plugin_list_loads_each_entry_and_reports_failures() {
    let dir = std::env::temp_dir().join(format!("boundless-plugin-list-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::copy(fixture("plugin_fixture_ok"), dir.join("ok.so")).expect("copy ok");
    std::fs::copy(fixture("plugin_fixture_fail"), dir.join("fail.so")).expect("copy fail");
    let list = dir.join("plugins.txt");
    std::fs::write(&list, "ok\nfail\n").expect("write list");
    let mut loader = PluginLoader::new();
    let (loaded, failed) = loader.init_plugins(&list, &dir).expect("init plugins");
    assert_eq!(loaded, 1, "ok joins the host");
    assert_eq!(failed, vec!["fail".to_string()], "fail is reported");
    assert_eq!(loader.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_plugin_list_is_an_error() {
    let mut loader = PluginLoader::new();
    let err = loader
        .init_plugins(Path::new("/nonexistent/plugins.txt"), Path::new("/tmp"))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn the_debug_symbol_pair_names_the_d_entry_points() {
    assert_eq!(DEBUG_INIT_SYMBOL, "plugin_init_d");
    assert_eq!(DEBUG_FINI_SYMBOL, "plugin_fini_d");
    // The ok fixture exports no _d entry points, so the debug pair selects a
    // name the library does not carry and the load rolls back.
    let mut loader = PluginLoader::with_debug_symbols();
    let path = fixture("plugin_fixture_ok");
    let err = loader.load_plugin(path.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, LoadError::NoInit(_)), "{err}");
}

#[test]
fn error_variants_name_their_failure_mode() {
    let open = LoadError::Open(std::io::Error::new(ErrorKind::NotFound, "nope"));
    let no_init = LoadError::NoInit(std::io::Error::new(ErrorKind::NotFound, "nope"));
    assert!(open.to_string().contains("could not open"));
    assert!(no_init.to_string().contains("no init"));
    assert!(LoadError::InitVetoed.to_string().contains("refused"));
}
