//! OPT-07-01-02: the backup serialization and archive compression
//! comparison.
//!
//! The acceptance compares **queues and recovery at the same load**
//! (`按同负载比较队列与恢复`). The OPT-07-01-01 baseline fixed the write
//! path's three windows at the shipped (uncompressed) payload; this
//! harness re-runs the same windows at the same 96-entity load with the
//! payload behind the reference's `CompressionOStream` frame
//! (`base.cpp:2322-2327`) — the transport an operator picks per entity
//! type through `internalNetworkCompressionType`
//! (`entity_description.cpp:147`, default `NONE`, zip levels `1..9`
//! mapping `compressionType - ZIP_1 + Z_BEST_SPEED`,
//! `compression_stream.cpp:85-89`). Four variants:
//!
//! - **raw** — the shipped `write_backup_data` (no frame);
//! - **none** — the wrapped frame at `BW_COMPRESSION_NONE` (the body
//!   passes through behind the type byte);
//! - **zip1** — `BW_COMPRESSION_ZIP_1` (`Z_BEST_SPEED`);
//! - **zip9** — `BW_COMPRESSION_ZIP_9` (`Z_BEST_COMPRESSION`).
//!
//! The sender's cycle schedule is serialization-blind (it slices entity
//! counts, not bytes), so every variant runs the identical
//! `BackupSender::tick` arithmetic the baseline pinned; what the
//! comparison reads is the byte faces: the per-cycle bandwidth, the
//! receiver's held bytes and the archive store, and the recovery
//! windows (takeover restore, archive restore) with the restored state
//! asserted equal to the live entities in every variant. Wall
//! milliseconds are recorded, never asserted.
//!
//! The fleet's bodies are small (84–116 bytes at the baseline
//! fixture), and deflate cannot recover the zlib frame's fixed cost
//! from a body that small — the zip arms **grow** the wire and the
//! store at this load, and the harness pins that overhead face. The
//! serialization ladder leg then grows one entity's body and records
//! where the zip arms cross under the framed NONE payload, which is
//! the load regime the knob is for.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `BACKUP_TUNING_OUT` is set, to that path:
//!
//! ```sh
//! BACKUP_TUNING_OUT=<abs path> cargo test -p server-runtime \
//!     --test backup_tuning -- --nocapture
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::time::Instant;

use foundation::stream::BinaryStream;
use server_runtime::archiver::{
    ArchiveStore, encode_with_compression, restore_archived, restore_archived_with_compression,
};
use server_runtime::backup_hash::{BackupHash, BackupHashChain};
use server_runtime::base_backup::{
    BackupMessage, BackupReceiver, backup_destination, header_len, write_backup_data,
    write_backup_data_wrapped,
};
use server_runtime::baseappmgr::Addr;
use server_runtime::entity_creator::{BaseEntity, EntityType, PropValue};
use server_runtime::replay_recording::{
    COMPRESSION_NONE, COMPRESSION_ZIP_BEST_COMPRESSION, COMPRESSION_ZIP_BEST_SPEED,
};

/// The four BaseApps of the fixture, on different machines.
const APPS: [Addr; 4] = [
    (0x7f000001, 4001),
    (0x7f000002, 4002),
    (0x7f000003, 4003),
    (0x7f000004, 4004),
];

/// The base-entity population of every window (the baseline's load).
const POP: usize = 96;
/// The backup period (`BaseAppConfig::backupPeriodInTicks`).
const BACKUP_PERIOD: usize = 10;
/// The archive period (`BaseAppConfig::archivePeriodInTicks`).
const ARCHIVE_PERIOD: usize = 8;
/// The port part of the fixture's cell-entity addresses.
const CELL_PORT: u16 = 5000;

/// One payload variant under comparison.
#[derive(Clone, Copy)]
struct Variant {
    label: &'static str,
    /// `None` = the shipped unwrapped payload; `Some(t)` = the wrapped
    /// frame at `BWCompressionType` `t`.
    compression: Option<u8>,
}

/// The comparison's variants: the shipped payload, the framed NONE, and
/// the two ends of the zip ladder.
const VARIANTS: [Variant; 4] = [
    Variant {
        label: "raw",
        compression: None,
    },
    Variant {
        label: "none",
        compression: Some(COMPRESSION_NONE),
    },
    Variant {
        label: "zip1",
        compression: Some(COMPRESSION_ZIP_BEST_SPEED),
    },
    Variant {
        label: "zip9",
        compression: Some(COMPRESSION_ZIP_BEST_COMPRESSION),
    },
];

/// The app that owns entity `1000 + i`, the fixture's population rule.
fn owner_of(id: u32) -> Addr {
    APPS[(id - 1000) as usize % APPS.len()]
}

// ---------------------------------------------------------------------------
// The record sink (the backup-baseline harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$BACKUP_TUNING_OUT` when set.
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
        if let Ok(path) = std::env::var("BACKUP_TUNING_OUT") {
            std::fs::write(&path, &text).expect("write BACKUP_TUNING_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The fixtures (the backup-baseline load, unchanged)
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

/// The population: `POP` entities, id `1000 + i`, alternating types,
/// each persisted and owned by app `i % 4`. Deterministic per entity.
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

/// Serialise one entity in `variant`'s payload form, the way
/// `BackupSender::backupBase` does for the entity type's compression
/// choice.
fn payload_of(base: &BaseEntity, variant: Variant) -> Vec<u8> {
    let mut stream = BinaryStream::new();
    match variant.compression {
        None => write_backup_data(base, &mut stream),
        Some(t) => write_backup_data_wrapped(base, &mut stream, t)
            .expect("the fixture's frame always compresses"),
    }
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

/// One backup cycle at the baseline's slice arithmetic, in `variant`'s
/// payload form: `(bytes per tick, dest bytes, dest msgs, payload sizes,
/// payload sizes by type, backed ids)`.
#[allow(clippy::type_complexity)]
fn backup_cycle(
    bases: &BTreeMap<u32, BaseEntity>,
    variant: Variant,
) -> (
    Vec<usize>,
    [usize; 4],
    [usize; 4],
    Vec<usize>,
    Vec<usize>,
    Vec<u32>,
) {
    let hash = backup_hash();
    let chain = BackupHashChain::default();
    let mut pending: VecDeque<u32> = bases.keys().copied().collect();
    let mut remainder = 0.0f32;

    let mut per_tick = Vec::new();
    let mut dest_bytes = [0usize; 4];
    let mut dest_msgs = [0usize; 4];
    let mut sizes = Vec::new();
    let mut account_sizes = Vec::new();
    let mut backed = Vec::new();

    for _ in 0..BACKUP_PERIOD {
        let num = tick_slice(bases.len(), BACKUP_PERIOD, &mut remainder).min(pending.len());
        let mut tick_bytes = 0usize;
        for _ in 0..num {
            let id = pending.pop_back().unwrap();
            let base = &bases[&id];
            let dest = backup_destination(&hash, &chain, id, false);
            let payload = payload_of(base, variant);
            let slot = APPS.iter().position(|a| *a == dest).expect("fixture dest");
            dest_bytes[slot] += payload.len();
            dest_msgs[slot] += 1;
            sizes.push(payload.len());
            if base.type_id == 1 {
                account_sizes.push(payload.len());
            }
            tick_bytes += payload.len();
            backed.push(id);
        }
        per_tick.push(tick_bytes);
    }
    (
        per_tick,
        dest_bytes,
        dest_msgs,
        sizes,
        account_sizes,
        backed,
    )
}

// ---------------------------------------------------------------------------
// Leg 1: the per-variant bandwidth of one full cycle at the fleet load
// ---------------------------------------------------------------------------

fn bandwidth_leg(record: &mut Record, window_bytes: &mut BTreeMap<&'static str, usize>) {
    let bases = population();

    let mut sizes_by_variant = BTreeMap::new();
    for variant in VARIANTS {
        let (per_tick, dest_bytes, dest_msgs, mut sizes, account_sizes, backed) =
            backup_cycle(&bases, variant);

        // The schedule is serialization-blind: the same entities in the
        // same per-tick split, every entity exactly once per cycle.
        assert_eq!(backed.len(), POP, "one backup per entity in a cycle");
        let mut seen = backed.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), POP);
        assert_eq!(dest_msgs.iter().sum::<usize>(), POP);

        let total: usize = per_tick.iter().sum();
        assert_eq!(total, sizes.iter().sum::<usize>());
        for (tick, bytes) in per_tick.iter().enumerate() {
            record.put(
                format!("bw_{}_tick_{:02}_bytes", variant.label, tick + 1),
                bytes,
            );
        }
        for (slot, bytes) in dest_bytes.iter().enumerate() {
            record.put(
                format!("bw_{}_dest_{:02}_bytes", variant.label, slot + 1),
                bytes,
            );
        }
        let min_size = *sizes.iter().min().unwrap();
        let max_size = *sizes.iter().max().unwrap();
        let med_size = median(&mut sizes);
        record.put(format!("bw_{}_window_bytes", variant.label), total);
        record.put(format!("bw_{}_window_messages", variant.label), POP);
        record.put(format!("bw_{}_payload_min_bytes", variant.label), min_size);
        record.put(
            format!("bw_{}_payload_median_bytes", variant.label),
            med_size,
        );
        record.put(format!("bw_{}_payload_max_bytes", variant.label), max_size);
        let account_med = median(&mut account_sizes.clone());
        record.put(
            format!("bw_{}_account_median_bytes", variant.label),
            account_med,
        );
        sizes_by_variant.insert(variant.label, sizes);
    }

    // Byte fidelity of the framing: wrapped NONE is the shipped payload
    // plus exactly the type byte, entity for entity.
    let raw: usize = sizes_by_variant["raw"].iter().sum();
    let none: usize = sizes_by_variant["none"].iter().sum();
    assert_eq!(none, raw + POP, "the NONE frame costs the type byte");
    {
        let base = &bases[&1000];
        let raw_payload = payload_of(base, VARIANTS[0]);
        let none_payload = payload_of(base, VARIANTS[1]);
        let split = header_len(base);
        assert_eq!(&none_payload[..split], &raw_payload[..split]);
        assert_eq!(none_payload[split], COMPRESSION_NONE);
        assert_eq!(&none_payload[split + 1..], &raw_payload[split..]);
    }

    // The overhead face at this load: the fleet's bodies are 84–116
    // bytes and deflate cannot recover the zlib frame's fixed cost from
    // bodies that small, so both zip arms land above the framed NONE —
    // pinned, because it is the regime the knob is *not* for.
    for label in ["zip1", "zip9"] {
        let zip_total: usize = sizes_by_variant[label].iter().sum();
        assert!(
            zip_total > none,
            "{label} pays the frame overhead at this load: {zip_total} vs {none}"
        );
    }

    for variant in VARIANTS {
        window_bytes.insert(variant.label, sizes_by_variant[variant.label].iter().sum());
    }
}

// ---------------------------------------------------------------------------
// Leg 2: the serialization ladder — where the zip arms cross under NONE
// ---------------------------------------------------------------------------

/// One 64-char log line of the ladder entities' `log` prop.
const LADDER_LINE: &str = "tick 0001 advance pos 12.500,12.500,0.000 hp 200/200\n";

/// The body repeat counts the ladder walks.
const LADDER_REPEATS: [usize; 5] = [1, 8, 64, 512, 4096];

/// The ladder entity: an `Account`-shaped base with one large `log`
/// prop of `repeats` lines.
fn ladder_base(repeats: usize) -> BaseEntity {
    let mut base = BaseEntity {
        id: 9000,
        type_id: 1,
        type_name: "Account".to_string(),
        db_id: 9,
        is_proxy: false,
        destroyed: false,
        in_destroy: false,
        base_props: std::collections::HashMap::new(),
        cell_props: std::collections::HashMap::new(),
        is_create_cell_pending: false,
        is_get_cell_pending: false,
        cell_entity_addr: Some(server_runtime::cell_data::BaseAddr {
            ip: APPS[0].0,
            port: CELL_PORT,
        }),
        cell_space_id: 0,
        callbacks: Vec::new(),
    };
    let mut log = String::with_capacity(repeats * LADDER_LINE.len());
    for n in 0..repeats {
        log.push_str(&format!("tick {n:04} {LADDER_LINE}"));
    }
    base.base_props
        .insert("log".to_string(), PropValue::Str(log));
    base
}

fn ladder_leg(record: &mut Record) {
    let ty = account_type();
    for repeats in LADDER_REPEATS {
        let base = ladder_base(repeats);
        let mut sizes = BTreeMap::new();
        for variant in VARIANTS {
            let payload = payload_of(&base, variant);
            sizes.insert(variant.label, payload.len());

            // Every rung stays lossless: the wrapped parse rebuilds the
            // same state the shipped parse does.
            let mut stream = BinaryStream::from_slice(&payload);
            let parsed = match variant.compression {
                None => server_runtime::base_backup::read_backup_data(&mut stream, &ty).unwrap(),
                Some(_) => {
                    server_runtime::base_backup::read_backup_data_wrapped(&mut stream, &ty).unwrap()
                }
            };
            assert_eq!(
                parsed.base_props, base.base_props,
                "rung {repeats} restores the log prop ({})",
                variant.label
            );
        }
        for variant in VARIANTS {
            record.put(
                format!("ladder_repeat_{repeats:04}_{}_bytes", variant.label),
                sizes[variant.label],
            );
        }
        if repeats == *LADDER_REPEATS.last().unwrap() {
            assert!(
                sizes["zip9"] < sizes["none"] && sizes["zip1"] < sizes["none"],
                "the biggest rung is where the zip arms pay off"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Leg 3: the per-variant queue peaks at the fleet load
// ---------------------------------------------------------------------------

fn queue_leg(record: &mut Record, store_bytes: &mut BTreeMap<&'static str, usize>) {
    let bases = population();
    let hash = backup_hash();
    let chain = BackupHashChain::default();

    for variant in VARIANTS {
        // The sender's cycle queue peaks at the population and drains —
        // identical for every payload form.
        let mut pending: VecDeque<u32> = bases.keys().copied().collect();
        let mut remainder = 0.0f32;
        let mut peak_depth = 0usize;
        let mut receiver = BackupReceiver::new();
        let mut held_bytes = [0usize; 4];
        for _ in 0..BACKUP_PERIOD {
            peak_depth = peak_depth.max(pending.len());
            let num = tick_slice(bases.len(), BACKUP_PERIOD, &mut remainder).min(pending.len());
            for _ in 0..num {
                let id = pending.pop_back().unwrap();
                let base = &bases[&id];
                let owner = owner_of(id);
                let dest = backup_destination(&hash, &chain, id, false);
                let message = BackupMessage {
                    dest,
                    is_offload: false,
                    entity_id: id,
                    data: payload_of(base, variant),
                };
                let slot = APPS.iter().position(|a| *a == owner).unwrap();
                held_bytes[slot] += message.data.len();
                receiver.back_up_entity(owner, &message);
            }
        }
        assert!(pending.is_empty());
        assert_eq!(peak_depth, POP);
        let held_total: usize = held_bytes.iter().sum();
        let receiver_total: usize = receiver
            .backups()
            .values()
            .map(|per_app| per_app.values().map(|d| d.len()).sum::<usize>())
            .sum();
        assert_eq!(receiver_total, held_total);
        for (slot, bytes) in held_bytes.iter().enumerate() {
            record.put(
                format!("queue_{}_held_app_{:02}_bytes", variant.label, slot + 1),
                bytes,
            );
        }
        record.put(
            format!("queue_{}_held_total_bytes", variant.label),
            held_total,
        );
        record.put(format!("queue_{}_peak_depth", variant.label), peak_depth);

        // The archive store at the archiver's slice schedule
        // (`Archiver::tick`, pinned exact in the baseline): the same
        // entities per tick, each persisted in the variant's form.
        let mut store = ArchiveStore::new();
        let mut order: Vec<u32> = bases.keys().copied().collect();
        let mut seed = 0x1234_5678u64;
        shuffle_like_archiver(&mut order, &mut seed);
        let mut arch_remainder = 0.0f32;
        let mut index = 0usize;
        for _ in 0..ARCHIVE_PERIOD {
            let num = tick_slice(bases.len(), ARCHIVE_PERIOD, &mut arch_remainder);
            for id in &order[index..index + num] {
                store.write(bases[id].db_id, payload_of(&bases[id], variant));
            }
            index += num;
        }
        assert_eq!(index, POP, "one archive cycle covers everyone");
        assert_eq!(store.len(), POP);
        let archive_total: usize = store.entries().values().map(|d| d.len()).sum();
        record.put(
            format!("queue_{}_archive_peak_bytes", variant.label),
            archive_total,
        );
        store_bytes.insert(variant.label, archive_total);
    }
}

/// The archiver's Fisher–Yates shuffle (`archiver.rs`'s LCG), replicated
/// so every variant persists the same entities in the same order.
fn shuffle_like_archiver(items: &mut [u32], seed: &mut u64) {
    for i in (1..items.len()).rev() {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (*seed as usize) % (i + 1);
        items.swap(i, j);
    }
}

// ---------------------------------------------------------------------------
// Leg 4: the per-variant recovery windows at the fleet load
// ---------------------------------------------------------------------------

fn recovery_leg(record: &mut Record) {
    let bases = population();
    let hash = backup_hash();
    let chain = BackupHashChain::default();
    let dead = APPS[0];
    let expected: Vec<u32> = (0..POP)
        .filter(|i| i % APPS.len() == 0)
        .map(|i| 1000 + i as u32)
        .collect();

    for variant in VARIANTS {
        // One receiver models the holder's view, filled at the same load.
        let mut receiver = BackupReceiver::new();
        for (id, base) in &bases {
            let message = BackupMessage {
                dest: backup_destination(&hash, &chain, *id, false),
                is_offload: false,
                entity_id: *id,
                data: payload_of(base, variant),
            };
            receiver.back_up_entity(owner_of(*id), &message);
        }

        // Takeover: every entity the dead app owned comes back, equal to
        // the live entity — the payload form may not lose state.
        let started = Instant::now();
        let restored = match variant.compression {
            None => receiver.restore(dead, &lookup),
            Some(_) => receiver.restore_wrapped(dead, &lookup),
        };
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(
            restored.len(),
            expected.len(),
            "the takeover restores every owned entity ({})",
            variant.label
        );
        let mut consumed = 0usize;
        for (entity, id) in restored.iter().zip(&expected) {
            let base = entity.as_ref().expect("every fixture backup applies");
            assert_eq!(base.id, *id);
            assert_eq!(
                props_of(base),
                props_of(&bases[id]),
                "the restored state matches ({})",
                variant.label
            );
            consumed += receiver.backups()[&dead][id].len();
        }
        record.put(
            format!("recover_{}_entities", variant.label),
            restored.len(),
        );
        record.put(format!("recover_{}_bytes", variant.label), consumed);
        record.put(
            format!("recover_{}_wall_ms", variant.label),
            format!("{wall_ms:.3}"),
        );

        // Archive recovery: the variant's store restores the same state
        // through the matching reader.
        let mut store = ArchiveStore::new();
        for base in bases.values() {
            match variant.compression {
                None => store.write(base.db_id, payload_of(base, variant)),
                Some(t) => store.write(base.db_id, encode_with_compression(base, t).unwrap()),
            }
        }
        let probe = &bases[&1004];
        let rebuilt = match variant.compression {
            None => restore_archived(&store, probe.db_id, &lookup),
            Some(_) => restore_archived_with_compression(&store, probe.db_id, &lookup),
        }
        .expect("the probe was archived")
        .expect("the probe applies");
        assert_eq!(
            props_of(&rebuilt),
            props_of(probe),
            "the archived state restores equal ({})",
            variant.label
        );
        record.put(
            format!("recover_{}_archive_restore", variant.label),
            "equal",
        );
    }
}

/// Runs every window in fixed order and leaves the comparison record.
#[test]
fn the_backup_tuning_compares_the_payload_variants_at_the_same_load() {
    let started = Instant::now();
    let mut record = Record::default();
    let mut window_bytes = BTreeMap::new();
    let mut store_bytes = BTreeMap::new();
    bandwidth_leg(&mut record, &mut window_bytes);
    ladder_leg(&mut record);
    queue_leg(&mut record, &mut store_bytes);
    let raw_store = store_bytes["raw"];
    for variant in VARIANTS {
        record.put(
            format!("result_{}_bw_ratio", variant.label),
            format!(
                "{:.3}",
                window_bytes[variant.label] as f64 / window_bytes["raw"] as f64
            ),
        );
        record.put(
            format!("result_{}_store_ratio", variant.label),
            format!(
                "{:.3}",
                store_bytes[variant.label] as f64 / raw_store as f64
            ),
        );
    }
    recovery_leg(&mut record);
    record.put(
        "tuning_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
