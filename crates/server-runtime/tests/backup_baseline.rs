//! OPT-07-01-01: the backup and archive baseline.
//!
//! The acceptance records the **bandwidth, queue peaks and recovery
//! time** of the fault-tolerance write path (`BW-07-04` port: the
//! backup hash, the backup sender/receiver, the offloaded backups and
//! the periodic archiver). Every ladder runs on fixed fixtures with a
//! synthetic clock, so every recorded number is exactly reproducible;
//! wall-clock rows are recorded, never asserted tight (the
//! entitydef/bridge/cellappmgr baseline discipline).
//!
//! The harness drives the shipped modules the way the reference's
//! `BackupSender::tick` does (`backup_sender.cpp:109`): the per-tick
//! slice is `bases.size()/backupPeriodInTicks + remainder` from the
//! cycle queue, each entity's destination comes from the `BackupHash`,
//! and the payload is `write_backup_data` — the same stream the
//! archiver persists. Three legs:
//!
//! 1. **Backup bandwidth** — one full backup cycle over a fixed
//!    population, recording bytes and messages per tick, the split by
//!    destination BaseApp (the `BundleSendingMap` face) and the payload
//!    size distribution. Pins the cycle coverage: over exactly one
//!    period every entity is backed up exactly once.
//! 2. **Queue peaks** — the sender's cycle queue draining within the
//!    period, the receiver's held bytes per source app, and the
//!    archiver's slice schedule writing an `ArchiveStore` up to its
//!    peak. The three queues are the queue-peak face of the
//!    acceptance.
//! 3. **Recovery** — a BaseApp dies: the backup holder restores every
//!    backed-up entity and each comes back with its properties equal
//!    to the live entity; the archiver restarts its cycle after the
//!    death (secondary database) and reports
//!    `informOfArchiveComplete` one period later. Restore op counts
//!    are asserted, wall milliseconds are recorded.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `BACKUP_BASELINE_OUT` is set, to that path:
//!
//! ```sh
//! BACKUP_BASELINE_OUT=<abs path> cargo test -p server-runtime \
//!     --test backup_baseline -- --nocapture
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::time::Instant;

use foundation::stream::BinaryStream;
use server_runtime::archiver::{ArchiveStore, Archiver, AutoArchivePolicy, restore_archived};
use server_runtime::backup_hash::{BackupHash, BackupHashChain};
use server_runtime::base_backup::{
    BackupMessage, BackupReceiver, backup_destination, write_backup_data,
};
use server_runtime::baseappmgr::Addr;
use server_runtime::entity_creator::{BaseEntity, EntityType, PropValue};

/// The four BaseApps of the fixture, on different machines.
const APPS: [Addr; 4] = [
    (0x7f000001, 4001),
    (0x7f000002, 4002),
    (0x7f000003, 4003),
    (0x7f000004, 4004),
];

/// The base-entity population of every window.
const POP: usize = 96;
/// The backup period (`BaseAppConfig::backupPeriodInTicks`) the window
/// runs at.
const BACKUP_PERIOD: usize = 10;
/// The archive period (`BaseAppConfig::archivePeriodInTicks`).
const ARCHIVE_PERIOD: usize = 8;
/// The port part of the fixture's cell-entity addresses.
const CELL_PORT: u16 = 5000;

/// The app that owns entity `1000 + i`, the fixture's population rule.
fn owner_of(id: u32) -> Addr {
    APPS[(id - 1000) as usize % APPS.len()]
}

// ---------------------------------------------------------------------------
// The record sink (the cellappmgr baseline harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$BACKUP_BASELINE_OUT` when set.
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
        if let Ok(path) = std::env::var("BACKUP_BASELINE_OUT") {
            std::fs::write(&path, &text).expect("write BACKUP_BASELINE_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The fixtures
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

/// A lookup resolving both fixture types, as the restore path wants.
fn lookup(name: &str) -> Option<EntityType> {
    match name {
        "Account" => Some(account_type()),
        "Mailbox" => Some(mailbox_type()),
        _ => None,
    }
}

/// The base props of an `Account` (the `base_backup` test idiom).
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

/// The population: `POP` entities, id `1000 + i`, alternating types,
/// each persisted (`db_id != 0`) and owned by app `i % 4` (the cell
/// entity address points at its owner). Deterministic per entity.
fn population() -> BTreeMap<u32, BaseEntity> {
    let mut bases = BTreeMap::new();
    for i in 0..POP {
        let owner = owner_of(1000 + i as u32);
        let (type_id, type_name, props) = if i % 2 == 0 {
            (1, "Account", account_props(i))
        } else {
            (2, "Mailbox", mailbox_props(i))
        };
        bases.insert(
            1000 + i as u32,
            BaseEntity {
                id: 1000 + i as u32,
                type_id,
                type_name: type_name.to_string(),
                db_id: (i + 1) as u64,
                is_proxy: false,
                destroyed: false,
                in_destroy: false,
                base_props: props.iter().cloned().collect(),
                cell_props: std::collections::HashMap::new(),
                is_create_cell_pending: false,
                is_get_cell_pending: false,
                cell_entity_addr: Some(server_runtime::cell_data::BaseAddr {
                    ip: owner.0,
                    port: CELL_PORT,
                }),
                cell_space_id: (i % 3) as u32,
                callbacks: Vec::new(),
            },
        );
    }
    bases
}

/// The props map a fixture built, back in the comparable form.
fn props_of(base: &BaseEntity) -> Vec<(String, PropValue)> {
    let mut pairs: Vec<(String, PropValue)> = base
        .base_props
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
}

/// Serialise one entity, the way `BackupSender::backupBase` does.
fn payload_of(base: &BaseEntity) -> Vec<u8> {
    let mut stream = BinaryStream::new();
    write_backup_data(base, &mut stream);
    stream.as_bytes().to_vec()
}

/// The median of a sorted copy.
fn median(values: &mut [usize]) -> usize {
    values.sort_unstable();
    values[values.len() / 2]
}

/// The per-tick slice of `BackupSender::tick`
/// (`backup_sender.cpp:125`): `size/period + floating remainder`.
fn tick_slice(remaining_size: usize, period: usize, remainder: &mut f32) -> usize {
    let float = remaining_size as f32 / period as f32 + *remainder;
    let count = float as usize;
    *remainder = float - count as f32;
    count
}

/// The backup hash of the fixture: all four apps as targets.
fn backup_hash() -> BackupHash {
    let mut hash = BackupHash::new();
    for addr in APPS {
        hash.push_back(addr);
    }
    hash
}

// ---------------------------------------------------------------------------
// Leg 1: the backup bandwidth of one full cycle
// ---------------------------------------------------------------------------

/// Runs one backup cycle the way `BackupSender::tick` slices it,
/// returning `(bytes per tick, dest bytes, dest messages, payload
/// sizes, backed-up ids)`.
#[allow(clippy::type_complexity)]
fn backup_cycle(
    bases: &BTreeMap<u32, BaseEntity>,
) -> (Vec<usize>, [usize; 4], [usize; 4], Vec<usize>, Vec<u32>) {
    let hash = backup_hash();
    let chain = BackupHashChain::default();
    let mut pending: VecDeque<u32> = bases.keys().copied().collect();
    let mut remainder = 0.0f32;

    let mut per_tick = Vec::new();
    let mut dest_bytes = [0usize; 4];
    let mut dest_msgs = [0usize; 4];
    let mut payload_sizes = Vec::new();
    let mut backed = Vec::new();

    for _ in 0..BACKUP_PERIOD {
        let num = tick_slice(bases.len(), BACKUP_PERIOD, &mut remainder).min(pending.len());
        let mut tick_bytes = 0usize;
        for _ in 0..num {
            let id = pending.pop_back().unwrap();
            let base = &bases[&id];
            let dest = backup_destination(&hash, &chain, id, false);
            let payload = payload_of(base);
            let slot = APPS.iter().position(|a| *a == dest).expect("fixture dest");
            dest_bytes[slot] += payload.len();
            dest_msgs[slot] += 1;
            payload_sizes.push(payload.len());
            tick_bytes += payload.len();
            backed.push(id);
        }
        per_tick.push(tick_bytes);
    }
    (per_tick, dest_bytes, dest_msgs, payload_sizes, backed)
}

fn bandwidth_leg(record: &mut Record) {
    let bases = population();

    // The population hashes across all four destinations; nothing may
    // land on the address-less `HASH_NONE_ADDR`.
    let (per_tick, dest_bytes, dest_msgs, mut payload_sizes, backed) = backup_cycle(&bases);

    // The cycle coverage: one period backs every entity up exactly
    // once — the remainder arithmetic of `backup_sender.cpp:125`.
    let mut seen = backed.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), POP, "every entity is backed up in one cycle");
    assert_eq!(backed.len(), POP, "no duplicate backups in one cycle");

    let total: usize = per_tick.iter().sum();
    assert_eq!(total, payload_sizes.iter().sum::<usize>());
    assert_eq!(
        dest_bytes.iter().sum::<usize>(),
        total,
        "the per-destination split covers every byte"
    );
    assert_eq!(dest_msgs.iter().sum::<usize>(), POP);

    let min_size = *payload_sizes.iter().min().unwrap();
    let max_size = *payload_sizes.iter().max().unwrap();
    let med_size = median(&mut payload_sizes);

    for (tick, bytes) in per_tick.iter().enumerate() {
        record.put(format!("backup_tick_{:02}_bytes", tick + 1), bytes);
    }
    for (slot, bytes) in dest_bytes.iter().enumerate() {
        record.put(format!("backup_dest_{:02}_bytes", slot + 1), bytes);
    }
    for (slot, msgs) in dest_msgs.iter().enumerate() {
        record.put(format!("backup_dest_{:02}_messages", slot + 1), msgs);
    }
    record.put("backup_window_bytes", total);
    record.put("backup_window_messages", POP);
    record.put("backup_payload_min_bytes", min_size);
    record.put("backup_payload_median_bytes", med_size);
    record.put("backup_payload_max_bytes", max_size);
}

// ---------------------------------------------------------------------------
// Leg 2: the queue peaks of the write path
// ---------------------------------------------------------------------------

fn queue_leg(record: &mut Record) {
    let bases = population();
    let hash = backup_hash();
    let chain = BackupHashChain::default();

    // The sender's cycle queue (`basesToBackUp_`): filled at the cycle
    // start, drained by the per-tick slice. The depth per tick is the
    // sender-side queue peak face.
    let mut pending: VecDeque<u32> = bases.keys().copied().collect();
    let mut remainder = 0.0f32;
    let mut peak_depth = 0usize;
    let mut receiver = BackupReceiver::new();
    let mut held_bytes = [0usize; 4];
    let mut held_msgs = [0usize; 4];
    for _ in 0..BACKUP_PERIOD {
        peak_depth = peak_depth.max(pending.len());
        let num = tick_slice(bases.len(), BACKUP_PERIOD, &mut remainder).min(pending.len());
        for _ in 0..num {
            let id = pending.pop_back().unwrap();
            let base = &bases[&id];
            let owner = owner_of(id);
            let dest = backup_destination(&hash, &chain, id, false);
            let mut stream = BinaryStream::new();
            write_backup_data(base, &mut stream);
            let data = stream.as_bytes().to_vec();
            let message = BackupMessage {
                dest,
                is_offload: false,
                entity_id: id,
                data,
            };
            let slot = APPS.iter().position(|a| *a == owner).unwrap();
            held_bytes[slot] += message.data.len();
            held_msgs[slot] += 1;
            receiver.back_up_entity(owner, &message);
        }
    }
    assert!(pending.is_empty(), "the cycle drains its queue");
    assert_eq!(peak_depth, POP, "the queue peaks at the population");

    // The receiver's held bytes per source app: the receiving-side
    // peak (one window, no replacements).
    let mut held_total = 0usize;
    for (slot, bytes) in held_bytes.iter().enumerate() {
        record.put(format!("queue_held_app_{:02}_bytes", slot + 1), bytes);
        record.put(
            format!("queue_held_app_{:02}_messages", slot + 1),
            held_msgs[slot],
        );
        held_total += bytes;
    }
    record.put("queue_held_total_bytes", held_total);
    let receiver_total: usize = receiver
        .backups()
        .values()
        .map(|per_app| per_app.values().map(|d| d.len()).sum::<usize>())
        .sum();
    assert_eq!(receiver_total, held_total, "the receiver holds every byte");

    // The archiver's slice schedule (`Archiver::tick`): the same
    // `size*index/period` arithmetic, writing the store up to its
    // peak.
    let mut archiver = Archiver::new(ARCHIVE_PERIOD);
    let mut store = ArchiveStore::new();
    let mut per_tick_archived = Vec::new();
    for _ in 0..ARCHIVE_PERIOD {
        let outcome = archiver.tick(&bases, &mut store);
        per_tick_archived.push(outcome.archived.len());
    }
    let archived_total: usize = per_tick_archived.iter().sum();
    assert_eq!(archived_total, POP, "one archive cycle covers everyone");
    assert_eq!(store.len(), POP, "every entity is persisted once");
    for (tick, count) in per_tick_archived.iter().enumerate() {
        record.put(format!("archive_tick_{:02}_entities", tick + 1), count);
    }
    let store_bytes: usize = store.entries().values().map(|d| d.len()).sum();
    record.put("queue_archive_peak_bytes", store_bytes);
    record.put("queue_cycle_peak_depth", peak_depth);
}

// ---------------------------------------------------------------------------
// Leg 3: the recovery windows
// ---------------------------------------------------------------------------

fn recovery_leg(record: &mut Record) {
    let bases = population();
    let hash = backup_hash();
    let chain = BackupHashChain::default();

    // Every app sends its backups to the hash destinations; one
    // receiver models the holder's view.
    let mut receiver = BackupReceiver::new();
    for (id, base) in &bases {
        let owner = owner_of(*id);
        let dest = backup_destination(&hash, &chain, *id, false);
        let mut stream = BinaryStream::new();
        write_backup_data(base, &mut stream);
        let message = BackupMessage {
            dest,
            is_offload: false,
            entity_id: *id,
            data: stream.as_bytes().to_vec(),
        };
        receiver.back_up_entity(owner, &message);
    }

    // Takeover: the first app dies; the holder restores every entity
    // that app owned, and each one comes back with its properties
    // equal to the live entity (`Base::writeBackupData` round trip).
    let dead = APPS[0];
    let started = Instant::now();
    let restored = receiver.restore(dead, &lookup);
    let restore_wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let expected: Vec<u32> = (0..POP)
        .filter(|i| i % APPS.len() == 0)
        .map(|i| 1000 + i as u32)
        .collect();
    assert_eq!(
        restored.len(),
        expected.len(),
        "the takeover restores every owned entity"
    );
    let mut restore_bytes = 0usize;
    for (entity, id) in restored.iter().zip(&expected) {
        let base = entity.as_ref().expect("every fixture backup applies");
        assert_eq!(base.id, *id);
        assert_eq!(
            props_of(base),
            props_of(&bases[id]),
            "the restored state matches"
        );
        restore_bytes += receiver.backups()[&dead][id].len();
    }
    record.put("recover_backup_entities", restored.len());
    record.put("recover_backup_bytes", restore_bytes);
    record.put("recover_backup_wall_ms", format!("{restore_wall_ms:.3}"));

    // The archiver's death handshake: with a secondary database the
    // cycle restarts at once and `informOfArchiveComplete` fires one
    // period after the death.
    let mut archiver = Archiver::new(ARCHIVE_PERIOD).with_secondary_db(true);
    let mut store = ArchiveStore::new();
    for _ in 0..ARCHIVE_PERIOD {
        archiver.tick(&bases, &mut store);
    }
    assert!(archiver.handle_base_app_death(dead, &bases));
    let mut ticks_to_inform = 0usize;
    loop {
        ticks_to_inform += 1;
        let outcome = archiver.tick(&bases, &mut store);
        if outcome.inform_archive_complete.is_some() {
            break;
        }
        assert!(
            ticks_to_inform <= ARCHIVE_PERIOD + 1,
            "the archive inform arrives"
        );
    }
    record.put("recover_archive_ticks_to_inform", ticks_to_inform);

    // The persisted state restores with equal properties for every
    // entity, and the `NEXT_ONLY` policy archives exactly once.
    let mut policy_archiver = Archiver::new(ARCHIVE_PERIOD);
    let sample = bases.clone();
    let probe = 1004u32;
    policy_archiver.set_auto_archive(probe, AutoArchivePolicy::NextOnly);
    let mut store2 = ArchiveStore::new();
    let mut probe_writes = 0usize;
    for _ in 0..(2 * ARCHIVE_PERIOD) {
        let outcome = policy_archiver.tick(&sample, &mut store2);
        probe_writes += outcome.archived.iter().filter(|id| **id == probe).count();
    }
    assert_eq!(probe_writes, 1, "NEXT_ONLY archives once");
    let rebuilt = restore_archived(&store, bases[&probe].db_id, &lookup)
        .expect("the probe was archived")
        .expect("the probe applies");
    assert_eq!(props_of(&rebuilt), props_of(&bases[&probe]));
    record.put("recover_next_only_writes", probe_writes);
    record.put("recover_archive_restore", "equal");
}

/// Runs every window in fixed order and leaves the baseline record.
#[test]
fn the_backup_baseline_records_the_write_path_windows() {
    let started = Instant::now();
    let mut record = Record::default();
    bandwidth_leg(&mut record);
    queue_leg(&mut record);
    recovery_leg(&mut record);
    record.put(
        "baseline_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
