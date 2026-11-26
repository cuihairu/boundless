//! OPT-07-01-03: the shared-data propagation comparison.
//!
//! The acceptance quantifies **the message-count change**
//! (`量化消息量变化`). The OPT-07-01-02 row landed the backup transport;
//! this row lands the shared-data propagation faces and compares the
//! forms:
//!
//! - **Join fill (the landed gap + optimization).** The reference
//!   streams one `setSharedData` message per authoritative entry into a
//!   newly joined app's bundle (`CellAppMgr::addApp`,
//!   `cellappmgr.cpp:1351-1377` — cell data then global data;
//!   `BaseAppMgr::synchronize`, `baseappmgr.cpp:1414-1430` — base-app
//!   data then global data). The port now fills both joins (the
//!   `BaseAppMgr` side was missing the global map; the `CellAppMgr`
//!   side had no fill at all), and ships the **batched** form
//!   alongside: `join_sync()` carries every entry in one sync message,
//!   so a join costs one message instead of one per entry.
//! - **Per-change broadcast (unchanged).** A set or delete is one
//!   message per app per change — the minimal form, which the
//!   optimization does not touch. The `GLOBAL_FROM_BASE_APP` chain
//!   keeps its fixed four steps (forward, cell broadcast, relay, base
//!   broadcast) and is quantified end to end.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `SHARED_DATA_TUNING_OUT` is set, to that path:
//!
//! ```sh
//! SHARED_DATA_TUNING_OUT=<abs path> cargo test -p server-runtime \
//!     --test shared_data_tuning -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::Instant;

use server_runtime::baseappmgr::BaseAppScheduler;
use server_runtime::cellappmgr::{AddAppOutcome, CellAppMgr};
use server_runtime::shared_data::{
    SHARED_DATA_TYPE_BASE_APP, SHARED_DATA_TYPE_CELL_APP, SHARED_DATA_TYPE_GLOBAL,
    SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP, SharedDataSync,
};

/// The authoritative CELL_APP entries of the fixture.
const CELL_ENTRIES: usize = 24;
/// The authoritative GLOBAL entries of the fixture.
const GLOBAL_ENTRIES: usize = 16;
/// The authoritative BASE_APP entries of the fixture.
const BASE_ENTRIES: usize = 12;

/// The CellApps that join across the windows.
const CELL_ADDRS: [(u32, u16); 4] = [
    (0x7f000001, 3001),
    (0x7f000002, 3002),
    (0x7f000003, 3003),
    (0x7f000004, 3004),
];
/// The BaseApps that join across the windows.
const BASE_ADDRS: [(u32, u16); 3] = [(0x7f000011, 3011), (0x7f000012, 3012), (0x7f000013, 3013)];

/// The entry-count ladder the join fill is quantified along.
const LADDER: [usize; 4] = [8, 24, 64, 256];

// ---------------------------------------------------------------------------
// The record sink (the backup-tuning harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$SHARED_DATA_TUNING_OUT` when set.
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: impl AsRef<str>, value: impl std::fmt::Display) {
        self.0
            .push((metric.as_ref().to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("SHARED_DATA_TUNING_OUT") {
            std::fs::write(&path, &text).expect("write SHARED_DATA_TUNING_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

/// A CellAppMgr with the fixture's authoritative maps and `num` joined
/// CellApps.
fn cell_mgr_with_maps(num: usize) -> CellAppMgr {
    let mut mgr = CellAppMgr::new();
    mgr.set_base_app((0x7f000009, 100));
    mgr.set_db_app_alpha((0x7f000009, 200));
    mgr.allow_new_cell_apps = true;
    for i in 0..CELL_ENTRIES {
        mgr.set_shared_data(
            SHARED_DATA_TYPE_CELL_APP,
            &format!("cell-key-{i:02}"),
            &format!("cell-value-{i:02}"),
            (0, 0),
        );
    }
    for i in 0..GLOBAL_ENTRIES {
        mgr.set_shared_data(
            SHARED_DATA_TYPE_GLOBAL,
            &format!("global-key-{i:02}"),
            &format!("global-value-{i:02}"),
            (0, 0),
        );
    }
    for addr in CELL_ADDRS.iter().take(num) {
        assert!(matches!(
            mgr.add_app(*addr, 5000, 1000),
            AddAppOutcome::Accepted { .. }
        ));
    }
    mgr
}

/// A BaseAppMgr with the fixture's authoritative maps and `num` ready
/// BaseApps.
fn base_mgr_with_maps(num: usize) -> BaseAppScheduler {
    let mut mgr = BaseAppScheduler::new();
    mgr.set_cell_app_mgr_established(true);
    mgr.set_has_init_data(true);
    mgr.set_db_apps_empty(false);
    for i in 0..BASE_ENTRIES {
        mgr.set_shared_data(
            SHARED_DATA_TYPE_BASE_APP,
            &format!("base-key-{i:02}"),
            &format!("base-value-{i:02}"),
            (0, 0),
        );
    }
    for i in 0..GLOBAL_ENTRIES {
        mgr.set_shared_data(
            SHARED_DATA_TYPE_GLOBAL,
            &format!("global-key-{i:02}"),
            &format!("global-value-{i:02}"),
            (0, 0),
        );
    }
    for addr in BASE_ADDRS.iter().take(num) {
        assert!(matches!(
            mgr.register(*addr, *addr, false, 0),
            server_runtime::baseappmgr::RegisterOutcome::Registered(_)
        ));
        mgr.finished_init(*addr);
    }
    mgr
}

/// Applies `sync`'s entries into a fresh map pair, the way a joined app
/// builds its local copies.
fn apply_sync(sync: &SharedDataSync) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut cell = BTreeMap::new();
    let mut global = BTreeMap::new();
    for (data_type, key, value) in &sync.entries {
        let map = match *data_type {
            SHARED_DATA_TYPE_CELL_APP | SHARED_DATA_TYPE_BASE_APP => &mut cell,
            _ => &mut global,
        };
        map.insert(key.clone(), value.clone());
    }
    (cell, global)
}

// ---------------------------------------------------------------------------
// Leg 1: the join fill — per entry vs the batched sync
// ---------------------------------------------------------------------------

fn join_leg(record: &mut Record) {
    // Cell side: every joining app receives the whole authoritative set.
    for (join_index, addr) in CELL_ADDRS.iter().enumerate() {
        let mut mgr = cell_mgr_with_maps(join_index);
        let expected_entries = CELL_ENTRIES + GLOBAL_ENTRIES;

        let per_entry = match mgr.add_app(*addr, 5000, 1000) {
            AddAppOutcome::Accepted { shared_data, .. } => shared_data,
            AddAppOutcome::Rejected { .. } => panic!("the fixture app joins"),
        };
        assert_eq!(
            per_entry.len(),
            expected_entries,
            "the per-entry fill is one message per entry"
        );
        let batch = mgr.join_sync();
        assert_eq!(batch.entries.len(), expected_entries);
        record.put(
            format!("join_cell_app_{}_per_entry_msgs", join_index + 1),
            per_entry.len(),
        );
        record.put(
            format!("join_cell_app_{}_batch_msgs", join_index + 1),
            1_usize,
        );
        record.put(
            format!("join_cell_app_{}_per_entry_bytes", join_index + 1),
            batch.per_entry_wire_bytes(),
        );
        record.put(
            format!("join_cell_app_{}_batch_bytes", join_index + 1),
            batch.batch_wire_bytes(),
        );

        // The two forms carry the same state: the batch's expansion is
        // the per-entry fill, and applying either rebuilds the
        // authoritative maps.
        assert_eq!(batch.expand(), per_entry);
        let (cell, global) = apply_sync(&batch);
        assert_eq!(&cell, mgr.shared_cell_app_data());
        assert_eq!(&global, mgr.shared_global_data());
    }

    // Base side: the synchronize fill now carries the global map too.
    for (join_index, addr) in BASE_ADDRS.iter().enumerate() {
        let mut mgr = base_mgr_with_maps(join_index);
        let expected_entries = BASE_ENTRIES + GLOBAL_ENTRIES;

        assert!(matches!(
            mgr.register(*addr, *addr, false, 0),
            server_runtime::baseappmgr::RegisterOutcome::Registered(_)
        ));
        let per_entry = mgr.finished_init(*addr);
        assert_eq!(
            per_entry
                .iter()
                .filter(|(_, m)| matches!(
                    m,
                    server_runtime::baseappmgr::ToBaseApp::SetSharedData { .. }
                ))
                .count(),
            expected_entries,
            "the synchronize fill is one message per shared entry"
        );
        let batch = mgr.join_sync();
        assert_eq!(batch.entries.len(), expected_entries);
        record.put(
            format!("join_base_app_{}_per_entry_msgs", join_index + 1),
            expected_entries,
        );
        record.put(
            format!("join_base_app_{}_batch_msgs", join_index + 1),
            1_usize,
        );
        record.put(
            format!("join_base_app_{}_per_entry_bytes", join_index + 1),
            batch.per_entry_wire_bytes(),
        );
        record.put(
            format!("join_base_app_{}_batch_bytes", join_index + 1),
            batch.batch_wire_bytes(),
        );

        let (base, global) = apply_sync(&batch);
        assert_eq!(&base, mgr.shared_base_app_data());
        assert_eq!(&global, mgr.shared_global_data());
    }

    record.put("join_state_equal", "true");
}

// ---------------------------------------------------------------------------
// Leg 2: the per-change broadcast — unchanged by the optimization
// ---------------------------------------------------------------------------

fn broadcast_leg(record: &mut Record) {
    let cell_apps = 4;
    let base_apps = 3;

    // A CELL_APP set: one message per CellApp, no relay.
    let mut mgr = cell_mgr_with_maps(cell_apps);
    let src = (0x7f000001, 7);
    let out = mgr.set_shared_data(SHARED_DATA_TYPE_CELL_APP, "live", "x", src);
    assert_eq!(out.cell_apps.len(), cell_apps);
    assert_eq!(out.base_app_mgr, None);
    record.put("bc_cell_app_set_msgs", out.cell_apps.len());

    // The GLOBAL_FROM_BASE_APP chain: forward, cell broadcast, relay,
    // base broadcast — four steps, quantified end to end.
    let mut base = base_mgr_with_maps(base_apps);
    let fwd = base.set_shared_data(
        SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP,
        "chain",
        "v",
        BASE_ADDRS[0],
    );
    assert!(
        fwd.cell_app_mgr.is_some(),
        "the forward leaves for the CellAppMgr"
    );
    assert!(fwd.base_apps.is_empty(), "the BaseApps wait for the relay");
    let mut msgs = usize::from(fwd.cell_app_mgr.is_some());

    if let Some(relayed) = fwd.cell_app_mgr {
        let server_runtime::shared_data::SharedDataMessage::Set {
            data_type,
            key,
            value,
            original_src,
        } = relayed
        else {
            panic!("the forward is a set");
        };
        let cell_out = mgr.set_shared_data(data_type, &key, &value, original_src);
        msgs += cell_out.cell_apps.len();
        msgs += usize::from(cell_out.base_app_mgr.is_some());
        if let Some(relay) = cell_out.base_app_mgr {
            let server_runtime::shared_data::SharedDataMessage::Set {
                data_type,
                key,
                value,
                original_src,
            } = relay
            else {
                panic!("the relay is a set");
            };
            let base_out = base.set_shared_data(data_type, &key, &value, original_src);
            msgs += base_out.base_apps.len();
        }
    }
    assert_eq!(msgs, 1 + cell_apps + 1 + base_apps, "the four-step chain");
    record.put("bc_global_from_base_chain_msgs", msgs);

    // A BASE_APP set: one message per BaseApp, straight.
    let out = base.set_shared_data(SHARED_DATA_TYPE_BASE_APP, "own", "y", BASE_ADDRS[0]);
    assert_eq!(out.base_apps.len(), base_apps);
    assert_eq!(out.cell_app_mgr, None);
    record.put("bc_base_app_set_msgs", out.base_apps.len());

    // The delete chain mirrors the set chain (the forward already made
    // the entry global on both managers).
    let mut del_msgs = 0usize;
    let out = mgr.del_shared_data(SHARED_DATA_TYPE_GLOBAL, "chain", (0, 0));
    del_msgs += out.cell_apps.len();
    del_msgs += usize::from(out.base_app_mgr.is_some());
    let out = base.del_shared_data(SHARED_DATA_TYPE_GLOBAL, "chain", (0, 0));
    del_msgs += out.base_apps.len();
    assert_eq!(
        del_msgs,
        cell_apps + 1 + base_apps,
        "the delete chain minus its forward"
    );
    record.put("bc_del_chain_msgs", del_msgs);
    assert!(mgr.shared_global_data().get("chain").is_none());
}

// ---------------------------------------------------------------------------
// Leg 3: the entry-count ladder
// ---------------------------------------------------------------------------

fn ladder_leg(record: &mut Record) {
    for entries in LADDER {
        let mut mgr = CellAppMgr::new();
        mgr.set_base_app((0x7f000009, 100));
        mgr.set_db_app_alpha((0x7f000009, 200));
        mgr.allow_new_cell_apps = true;
        for i in 0..entries {
            mgr.set_shared_data(
                SHARED_DATA_TYPE_CELL_APP,
                &format!("k{i:04}"),
                &format!("v{i:04}"),
                (0, 0),
            );
        }
        let batch = mgr.join_sync();
        assert_eq!(batch.entries.len(), entries);
        assert_eq!(batch.expand().len(), entries);
        record.put(format!("ladder_e{entries:03}_per_entry_msgs"), entries);
        record.put(format!("ladder_e{entries:03}_batch_msgs"), 1_usize);
        record.put(
            format!("ladder_e{entries:03}_per_entry_bytes"),
            batch.per_entry_wire_bytes(),
        );
        record.put(
            format!("ladder_e{entries:03}_batch_bytes"),
            batch.batch_wire_bytes(),
        );
        // Payload bytes are recorded, not ranked: the batch's saving is
        // the (entries - 1) per-message header costs the payload-only
        // model excludes — the message rows above carry the verdict.
    }
}

/// Runs every window in fixed order and leaves the comparison record.
#[test]
fn the_shared_data_tuning_quantifies_the_propagation_messages() {
    let started = Instant::now();
    let mut record = Record::default();
    join_leg(&mut record);
    broadcast_leg(&mut record);
    ladder_leg(&mut record);
    record.put(
        "tuning_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
