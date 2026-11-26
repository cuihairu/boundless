//! OPT-07-01-04: the takeover rerun.
//!
//! The acceptance: **restored entities and shared state correct**
//! (`恢复实体和共享状态正确`). The cluster's first three rows landed the
//! backup/archive baseline (07-01-01), the compression-wrapped backup
//! transport (07-01-02) and the quantified shared-data propagation
//! (07-01-03); this row re-runs the takeover with all of them in place
//! and checks both halves of the acceptance:
//!
//! - **Entities.** A BaseApp dies — its backed-up population rebuilds
//!   through the wrapped frames (`BackupReceiver::restore_wrapped`,
//!   `backed_up_base_app.cpp:176`), every entity equal to the live one.
//!   A CellApp dies — the `DeadCellApps` walk re-points every base whose
//!   cell part was on the dead app to its space's new CellApp
//!   (`dead_cell_apps.cpp:234`). Archived entities rebuild through the
//!   compressed archive bytes (`restore_archived_with_compression`,
//!   `createBaseFromStream`, `baseapp.cpp:2589`).
//! - **Shared state.** The managers' authoritative maps survive a
//!   manager restart through the recovery stream
//!   (`recoverSharedDataFromStream`, `baseappmgr.cpp:1249`), and a
//!   replacement app joining the recovered manager receives the whole
//!   authoritative set — one batched message (the OPT-07-01-03 form)
//!   instead of one per entry — rebuilding the same maps.
//!
//! The drill runs twice (复跑, the rerun); both runs must produce
//! identical entity and shared-state digests.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `TAKEOVER_RERUN_OUT` is set, to that path:
//!
//! ```sh
//! TAKEOVER_RERUN_OUT=<abs path> cargo test -p server-runtime \
//!     --test takeover_rerun -- --nocapture
//! ```

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::time::Instant;

use foundation::stream::BinaryStream;
use server_runtime::archiver::{
    ArchiveStore, encode_with_compression, restore_archived_with_compression,
};
use server_runtime::base_backup::{BackupMessage, BackupReceiver, write_backup_data_wrapped};
use server_runtime::baseappmgr::{BaseAppScheduler, RegisterOutcome, ToBaseApp};
use server_runtime::cell_data::BaseAddr;
use server_runtime::cellappmgr::{AddAppOutcome, CellAppMgr};
use server_runtime::dead_cell_app::{DeadCellApps, RestoreOutcome};
use server_runtime::entity_creator::{BaseEntity, EntityType, PropValue};
use server_runtime::replay_recording::{
    COMPRESSION_ZIP_BEST_COMPRESSION, COMPRESSION_ZIP_BEST_SPEED,
};
use server_runtime::shared_data::{
    SHARED_DATA_TYPE_BASE_APP, SHARED_DATA_TYPE_CELL_APP, SharedDataSync,
};

/// The BaseApps of the fixture (the baseline's fleet).
const APPS: [Addr; 4] = [
    (0x7f000001, 4001),
    (0x7f000002, 4002),
    (0x7f000003, 4003),
    (0x7f000004, 4004),
];

/// The CellApps of the fixture.
const CELLS: [Addr; 4] = [
    (0x7f000001, 3001),
    (0x7f000002, 3002),
    (0x7f000003, 3003),
    (0x7f000004, 3004),
];

/// The app that dies in the drill: its backed-up population rebuilds on
/// the holder.
const DEAD_BASE_APP: Addr = APPS[1];
/// The CellApp that dies in the drill: its bases re-point.
const DEAD_CELL_APP: Addr = CELLS[2];

/// The replacement apps that join the recovered managers.
const REPLACEMENT_CELL: Addr = (0x7f000021, 3021);
const REPLACEMENT_BASE: Addr = (0x7f000031, 3031);

/// The base-entity population (the baseline's load).
const POP: usize = 96;

/// The authoritative CELL_APP entries of the fixture.
const CELL_ENTRIES: usize = 24;
/// The authoritative GLOBAL entries of the fixture.
const GLOBAL_ENTRIES: usize = 16;
/// The authoritative BASE_APP entries of the fixture.
const BASE_ENTRIES: usize = 12;

/// The app holding the backups (a surviving BaseApp).
const HOLDER: Addr = APPS[0];

type Addr = (u32, u16);

/// The `internalNetworkCompressionType` choice per entity type
/// (`entity_description.cpp:147`): the `Account` type ships at the zip
/// speed level, the `Mailbox` type at the zip best level.
fn type_compression(type_name: &str) -> u8 {
    match type_name {
        "Mailbox" => COMPRESSION_ZIP_BEST_COMPRESSION,
        _ => COMPRESSION_ZIP_BEST_SPEED,
    }
}

// ---------------------------------------------------------------------------
// The record sink (the tuning-harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$TAKEOVER_RERUN_OUT` when set.
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
        if let Ok(path) = std::env::var("TAKEOVER_RERUN_OUT") {
            std::fs::write(&path, &text).expect("write TAKEOVER_RERUN_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The fixtures (the backup-tuning load, cell parts on the CellApps)
// ---------------------------------------------------------------------------

/// The `Account` type: five base props of every value kind.
fn account_type() -> EntityType {
    EntityType {
        id: 1,
        name: "Account".to_string(),
        can_be_on_base: true,
        is_service: false,
        is_proxy: false,
        can_be_on_cell: true,
        properties: vec![],
        methods: vec![],
    }
}

/// The `Mailbox` type: three base props, a smaller payload.
fn mailbox_type() -> EntityType {
    EntityType {
        id: 2,
        name: "Mailbox".to_string(),
        can_be_on_base: true,
        is_service: false,
        is_proxy: false,
        can_be_on_cell: true,
        properties: vec![],
        methods: vec![],
    }
}

/// A lookup resolving both fixture types, as the restore paths want.
fn lookup(name: &str) -> Option<EntityType> {
    match name {
        "Account" => Some(account_type()),
        "Mailbox" => Some(mailbox_type()),
        _ => None,
    }
}

/// The base props of an `Account` (the baseline's values).
fn account_props(seed: usize) -> Vec<(String, PropValue)> {
    vec![
        ("alive".to_string(), PropValue::Bool(seed.is_multiple_of(2))),
        (
            "health".to_string(),
            PropValue::Int(100 + (seed % 50) as i64),
        ),
        ("name".to_string(), PropValue::Str(format!("user{seed:04}"))),
        (
            "pos".to_string(),
            PropValue::Vec3([seed as f32, (seed % 7) as f32, 0.0]),
        ),
        ("speed".to_string(), PropValue::Float((seed % 9) as f64)),
    ]
}

/// The base props of a `Mailbox`.
fn mailbox_props(seed: usize) -> Vec<(String, PropValue)> {
    vec![
        (
            "owner".to_string(),
            PropValue::Str(format!("user{seed:04}")),
        ),
        ("unread".to_string(), PropValue::Int((seed % 12) as i64)),
        ("paid".to_string(), PropValue::Bool(seed.is_multiple_of(3))),
    ]
}

/// The app that owns entity `1000 + i`, the fixture's population rule.
fn owner_of(id: u32) -> Addr {
    APPS[((id - 1000) as usize) % APPS.len()]
}

/// The CellApp hosting entity `1000 + i`'s cell part, and its space id.
fn cell_of(i: usize) -> (Addr, u32) {
    (CELLS[i % CELLS.len()], (i % 3) as u32)
}

/// The population: `POP` entities, id `1000 + i`, alternating types,
/// each persisted, owned by app `i % 4`, its cell part hosted by the
/// CellApp `i % 4` in space `i % 3`. Deterministic per entity.
fn population() -> BTreeMap<u32, BaseEntity> {
    let mut bases = BTreeMap::new();
    for i in 0..POP {
        let id = 1000 + i as u32;
        let (cell, space) = cell_of(i);
        let (type_id, type_name, props) = if i % 2 == 0 {
            (1, "Account", account_props(i))
        } else {
            (2, "Mailbox", mailbox_props(i))
        };
        bases.insert(
            id,
            BaseEntity {
                id,
                type_id,
                type_name: type_name.to_string(),
                db_id: (i + 1) as u64,
                is_proxy: false,
                destroyed: false,
                in_destroy: false,
                base_props: props.iter().cloned().collect(),
                cell_props: HashMap::new(),
                is_create_cell_pending: false,
                is_get_cell_pending: false,
                cell_entity_addr: Some(BaseAddr {
                    ip: cell.0,
                    port: cell.1,
                }),
                cell_space_id: space,
                callbacks: Vec::new(),
            },
        );
    }
    bases
}

/// The props map of a base, back in the comparable form.
fn props_of(base: &BaseEntity) -> Vec<(String, PropValue)> {
    let mut pairs: Vec<(String, PropValue)> = base
        .base_props
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
}

/// The comparable projection of a base entity: every field the restore
/// paths rebuild.
fn projection(base: &BaseEntity) -> String {
    let cell = base
        .cell_entity_addr
        .as_ref()
        .map(|a| format!("{:x}:{}", a.ip, a.port))
        .unwrap_or_else(|| "none".to_string());
    let props = props_of(base)
        .iter()
        .map(|(k, v)| format!("{k}={v:?}"))
        .collect::<Vec<_>>()
        .join(";");
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        base.id,
        base.type_id,
        base.type_name,
        base.db_id,
        base.is_proxy,
        cell,
        base.cell_space_id,
        props
    )
}

/// FNV-1a 64, the digest over a rendered state.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The digest of a fleet: every base's projection in id order.
fn fleet_digest(bases: &BTreeMap<u32, BaseEntity>) -> String {
    let mut text = String::new();
    for base in bases.values() {
        text.push_str(&projection(base));
        text.push('\n');
    }
    format!("{:016x}", fnv1a64(text.as_bytes()))
}

/// The authoritative shared-data maps of the fixture, in the same
/// entries the OPT-07-01-03 harness carried.
fn shared_maps() -> (
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
) {
    let cell = (0..CELL_ENTRIES)
        .map(|i| (format!("cell-key-{i:02}"), format!("cell-value-{i:02}")))
        .collect::<BTreeMap<_, _>>();
    let base = (0..BASE_ENTRIES)
        .map(|i| (format!("base-key-{i:02}"), format!("base-value-{i:02}")))
        .collect::<BTreeMap<_, _>>();
    let global = (0..GLOBAL_ENTRIES)
        .map(|i| (format!("global-key-{i:02}"), format!("global-value-{i:02}")))
        .collect::<BTreeMap<_, _>>();
    (cell, base, global)
}

/// One recovery section: a count, then that many `(key, value)` pairs —
/// the `recoverSharedDataFromStream` shape.
fn write_section(stream: &mut BinaryStream, map: &BTreeMap<String, String>) {
    stream.write_count(map.len() as u32);
    for (key, value) in map {
        let _ = stream.write_string(key);
        let _ = stream.write_string(value);
    }
}

/// The digest of the shared state: both managers' authoritative maps.
fn shared_digest(
    cell: &BTreeMap<String, String>,
    base: &BTreeMap<String, String>,
    global: &BTreeMap<String, String>,
) -> String {
    let mut text = String::new();
    for (label, map) in [("cell", cell), ("base", base), ("global", global)] {
        text.push_str(label);
        for (key, value) in map {
            text.push('\t');
            text.push_str(key);
            text.push('\t');
            text.push_str(value);
        }
        text.push('\n');
    }
    format!("{:016x}", fnv1a64(text.as_bytes()))
}

/// Applies a sync's entries into a fresh map pair, the way a joined app
/// builds its local copies: the app-local map (CELL_APP or BASE_APP
/// data) and the global map.
fn apply_sync(sync: &SharedDataSync) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut local = BTreeMap::new();
    let mut global = BTreeMap::new();
    for (data_type, key, value) in &sync.entries {
        let map = match *data_type {
            SHARED_DATA_TYPE_CELL_APP | SHARED_DATA_TYPE_BASE_APP => &mut local,
            _ => &mut global,
        };
        map.insert(key.clone(), value.clone());
    }
    (local, global)
}

// ---------------------------------------------------------------------------
// The drill: both deaths, the restore, the recovery and the replacement
// join, once.
// ---------------------------------------------------------------------------

/// What one drill run recorded (the correctness is asserted inside).
#[derive(Debug, Default, Clone)]
struct DrillOutcome {
    dead_entities: usize,
    restored_entities: usize,
    restored_bytes: usize,
    restore_wall_ms: f64,
    dead_cell_bases: usize,
    repointed_bases: usize,
    untouched_bases: usize,
    walk_wall_ms: f64,
    archive_entities: usize,
    archive_restored: usize,
    archive_bytes: usize,
    archive_wall_ms: f64,
    recover_wall_ms: f64,
    cellapp_fill_msgs: usize,
    cellapp_batch_msgs: usize,
    baseapp_fill_msgs: usize,
    baseapp_batch_msgs: usize,
    entity_digest: String,
    shared_digest: String,
}

/// Runs the takeover once: the BaseApp death and its backup restore, the
/// CellApp death and its re-point walk, the archive rebuild, the
/// manager restart recovery and the replacement apps' join.
fn run_drill() -> DrillOutcome {
    let mut out = DrillOutcome::default();
    let bases = population();

    // --- The BaseApp death: every backed-up entity rebuilds through the
    // wrapped frames on the holder, equal to the live entity.
    let mut receiver = BackupReceiver::new();
    let mut restored_bytes = 0usize;
    for base in bases.values() {
        let owner = owner_of(base.id);
        let mut stream = BinaryStream::new();
        write_backup_data_wrapped(base, &mut stream, type_compression(&base.type_name))
            .expect("the fixture's frame always compresses");
        let data = stream.as_bytes().to_vec();
        restored_bytes += data.len();
        receiver.back_up_entity(
            owner,
            &BackupMessage {
                dest: HOLDER,
                is_offload: false,
                entity_id: base.id,
                data,
            },
        );
    }

    let started = Instant::now();
    let restored = receiver.restore_wrapped(DEAD_BASE_APP, &lookup);
    out.restore_wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let dead_entities = bases
        .values()
        .filter(|base| owner_of(base.id) == DEAD_BASE_APP)
        .count();
    assert_eq!(restored.len(), dead_entities, "every dead entity rebuilds");
    for (rebuilt, live) in restored.iter().zip(
        bases
            .values()
            .filter(|base| owner_of(base.id) == DEAD_BASE_APP),
    ) {
        let rebuilt = rebuilt.as_ref().expect("the fixture's types resolve");
        assert_eq!(projection(rebuilt), projection(live), "a lossless rebuild");
    }
    out.dead_entities = dead_entities;
    out.restored_entities = restored.len();
    out.restored_bytes = restored_bytes;

    // --- The CellApp death: every base whose cell part was on the dead
    // app re-points to its space's new CellApp (the destination map the
    // CellAppMgr hands the BaseApps).
    let destinations: BTreeMap<u32, Addr> = [(0, CELLS[0]), (1, CELLS[1]), (2, CELLS[3])]
        .into_iter()
        .collect();
    let mut payload = BinaryStream::new();
    for (space, addr) in &destinations {
        payload.write_u32(*space);
        payload.write_u32(addr.0);
        payload.write_u16(addr.1);
    }
    let mut dead_cell_apps = DeadCellApps::new();
    dead_cell_apps.add_app(DEAD_CELL_APP, payload.as_bytes(), 0);

    let expected_cell: BTreeMap<u32, Addr> = bases
        .values()
        .map(|base| {
            let original = base
                .cell_entity_addr
                .as_ref()
                .map(|a| (a.ip, a.port))
                .expect("the fixture always has a cell part");
            let dest = if original == DEAD_CELL_APP {
                destinations[&base.cell_space_id]
            } else {
                original
            };
            (base.id, dest)
        })
        .collect();

    let started = Instant::now();
    let mut fleet = bases.clone();
    let outcome = dead_cell_apps.tick(&mut fleet, 1);
    out.walk_wall_ms = started.elapsed().as_secs_f64() * 1000.0;

    out.dead_cell_bases = outcome.restored.len();
    out.repointed_bases = outcome
        .restored
        .iter()
        .filter(
            |(id, o)| matches!(o, RestoreOutcome::ToCellApp(addr) if expected_cell[id] == *addr),
        )
        .count();
    out.untouched_bases = POP - outcome.restored.len();
    assert_eq!(
        out.repointed_bases, out.dead_cell_bases,
        "every base re-points"
    );
    assert!(
        outcome.completed.contains(&DEAD_CELL_APP),
        "the walk completes"
    );
    for (id, base) in &fleet {
        let expected = expected_cell[id];
        let cell_addr = base.cell_entity_addr.as_ref().expect("the cell survives");
        assert_eq!(
            (cell_addr.ip, cell_addr.port),
            expected,
            "entity {id} lands on its space's new CellApp"
        );
    }

    // --- The archive rebuild: the persisted bytes, through the wrapped
    // frame, rebuild every entity after a restart.
    let started = Instant::now();
    let mut store = ArchiveStore::new();
    let mut archive_bytes = 0usize;
    for base in bases.values() {
        let data = encode_with_compression(base, type_compression(&base.type_name))
            .expect("the fixture's frame always compresses");
        archive_bytes += data.len();
        store.write(base.db_id, data);
    }
    let mut archive_restored = 0usize;
    for base in bases.values() {
        let rebuilt = restore_archived_with_compression(&store, base.db_id, &lookup)
            .expect("every persisted entity is in the store")
            .expect("the fixture's types resolve");
        assert_eq!(projection(&rebuilt), projection(base), "a lossless rebuild");
        archive_restored += 1;
    }
    out.archive_wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    out.archive_entities = POP;
    out.archive_restored = archive_restored;
    out.archive_bytes = archive_bytes;

    // --- The manager restart: the authoritative maps come back through
    // the recovery stream, and the replacement apps join the recovered
    // managers — the whole authoritative set in one batched message.
    let (cell_map, base_map, global_map) = shared_maps();
    let started = Instant::now();

    let mut cell_mgr = CellAppMgr::new();
    cell_mgr.set_base_app((0x7f000009, 100));
    cell_mgr.set_db_app_alpha((0x7f000009, 200));
    cell_mgr.allow_new_cell_apps = true;
    let mut cell_stream = BinaryStream::new();
    write_section(&mut cell_stream, &cell_map);
    write_section(&mut cell_stream, &global_map);
    cell_mgr
        .recover_shared_data_from_stream(&mut cell_stream)
        .expect("the cell recovery reads");
    assert_eq!(cell_mgr.shared_cell_app_data(), &cell_map);
    assert_eq!(cell_mgr.shared_global_data(), &global_map);

    let mut base_mgr = BaseAppScheduler::new();
    base_mgr.set_cell_app_mgr_established(true);
    base_mgr.set_has_init_data(true);
    base_mgr.set_db_apps_empty(false);
    let mut base_stream = BinaryStream::new();
    write_section(&mut base_stream, &base_map);
    write_section(&mut base_stream, &global_map);
    base_mgr
        .recover_shared_data_from_stream(&mut base_stream)
        .expect("the base recovery reads");
    assert_eq!(base_mgr.shared_base_app_data(), &base_map);
    assert_eq!(base_mgr.shared_global_data(), &global_map);
    out.recover_wall_ms = started.elapsed().as_secs_f64() * 1000.0;

    // The replacement CellApp joins the recovered manager: the per-entry
    // fill carries the whole authoritative set, and the batched sync
    // carries the same state in one message.
    let accepted = match cell_mgr.add_app(REPLACEMENT_CELL, 6001, 0) {
        AddAppOutcome::Accepted { shared_data, .. } => shared_data,
        AddAppOutcome::Rejected { .. } => panic!("the replacement joins"),
    };
    let cell_batch = cell_mgr.join_sync();
    assert_eq!(cell_batch.expand(), accepted, "the batch is the fill");
    assert_eq!(accepted.len(), CELL_ENTRIES + GLOBAL_ENTRIES);
    let (applied_cell, applied_global) = apply_sync(&cell_batch);
    assert_eq!(applied_cell, cell_map);
    assert_eq!(applied_global, global_map);
    out.cellapp_fill_msgs = accepted.len();
    out.cellapp_batch_msgs = 1;

    // The replacement BaseApp joins the recovered scheduler the same way.
    assert!(matches!(
        base_mgr.register(REPLACEMENT_BASE, REPLACEMENT_BASE, false, 0),
        RegisterOutcome::Registered(_)
    ));
    let fill = base_mgr.finished_init(REPLACEMENT_BASE);
    let base_batch = base_mgr.join_sync();
    let fill_sets = fill
        .iter()
        .filter(|(_, m)| matches!(m, ToBaseApp::SetSharedData { .. }))
        .count();
    assert_eq!(fill_sets, BASE_ENTRIES + GLOBAL_ENTRIES);
    let (applied_base, applied_global) = apply_sync(&base_batch);
    assert_eq!(applied_base, base_map);
    assert_eq!(applied_global, global_map);
    out.baseapp_fill_msgs = fill_sets;
    out.baseapp_batch_msgs = 1;

    // --- The digests the rerun compares.
    out.entity_digest = fleet_digest(&fleet);
    out.shared_digest = shared_digest(&cell_map, &base_map, &global_map);
    out
}

/// Runs the takeover twice; both runs must rebuild the same entities and
/// recover the same shared state.
#[test]
fn the_takeover_rerun_restores_entities_and_shared_state() {
    let started = Instant::now();
    let mut record = Record::default();

    let mut first: Option<(String, String)> = None;
    for run in 1..=2 {
        let out = run_drill();
        record.put(format!("run{run}_dead_baseapp_entities"), out.dead_entities);
        record.put(format!("run{run}_restored_entities"), out.restored_entities);
        record.put(format!("run{run}_restored_bytes"), out.restored_bytes);
        record.put(
            format!("run{run}_restore_wall_ms"),
            format!("{:.3}", out.restore_wall_ms),
        );
        record.put(format!("run{run}_dead_cellapp_bases"), out.dead_cell_bases);
        record.put(format!("run{run}_repointed_bases"), out.repointed_bases);
        record.put(format!("run{run}_untouched_bases"), out.untouched_bases);
        record.put(
            format!("run{run}_walk_wall_ms"),
            format!("{:.3}", out.walk_wall_ms),
        );
        record.put(format!("run{run}_archive_entities"), out.archive_entities);
        record.put(format!("run{run}_archive_restored"), out.archive_restored);
        record.put(format!("run{run}_archive_bytes"), out.archive_bytes);
        record.put(
            format!("run{run}_archive_wall_ms"),
            format!("{:.3}", out.archive_wall_ms),
        );
        record.put(
            format!("run{run}_recover_wall_ms"),
            format!("{:.3}", out.recover_wall_ms),
        );
        record.put(
            format!("run{run}_cellapp_join_fill_msgs"),
            out.cellapp_fill_msgs,
        );
        record.put(
            format!("run{run}_cellapp_join_batch_msgs"),
            out.cellapp_batch_msgs,
        );
        record.put(
            format!("run{run}_baseapp_join_fill_msgs"),
            out.baseapp_fill_msgs,
        );
        record.put(
            format!("run{run}_baseapp_join_batch_msgs"),
            out.baseapp_batch_msgs,
        );
        record.put(
            format!("run{run}_entity_digest"),
            out.entity_digest.as_str(),
        );
        record.put(
            format!("run{run}_shared_digest"),
            out.shared_digest.as_str(),
        );
        if run == 1 {
            record.put("rerun_entity_digest", out.entity_digest.as_str());
            record.put("rerun_shared_digest", out.shared_digest.as_str());
        }
        // The rerun face: the second drill rebuilds the same entities
        // and recovers the same shared state as the first.
        if let Some((entities, shared)) = &first {
            assert_eq!(
                &out.entity_digest, entities,
                "the rerun rebuilds the same entities"
            );
            assert_eq!(
                &out.shared_digest, shared,
                "the rerun recovers the same shared state"
            );
        } else {
            first = Some((out.entity_digest.clone(), out.shared_digest.clone()));
        }
    }

    record.put("rerun_state_equal", "true");
    record.put(
        "tuning_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
