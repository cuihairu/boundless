//! Loads every saved scenario under `tests/load/scenarios/` (BW-17-02-01).
//!
//! The task book fixes the scenario location at `tests/load/scenarios/`; this
//! test pins the fixture set to the loader so a saved scenario that stops
//! parsing fails here rather than at run time.

use std::fs;
use std::path::Path;

#[test]
fn every_saved_scenario_loads() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/load/scenarios");
    assert!(
        dir.is_dir(),
        "missing saved scenarios dir: {}",
        dir.display()
    );

    let mut count = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map(|e| e == "xml").unwrap_or(false) {
            let raw = fs::read(&path).unwrap();
            let scenario = load::Scenario::load_xml(&raw)
                .unwrap_or_else(|e| panic!("scenario {} failed to load: {e}", path.display()));
            assert!(!scenario.name.is_empty());
            count += 1;
        }
    }

    // The task book names six scenario kinds (login-hurricane, steady-move,
    // dense-aoi, cross-cell, reconnect, long-stability); all must be present
    // and loadable.
    assert!(
        count >= 6,
        "expected the six saved scenarios, found {count}"
    );
}
