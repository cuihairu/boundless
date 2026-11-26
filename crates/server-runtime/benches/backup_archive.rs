//! OPT-07-01-01 backup and archive baseline benchmark.
//!
//! The three recorded faces of the BaseApp fault-tolerance data path, one
//! group each:
//! * `backup_write` — the bandwidth face: one full backup cycle for a
//!   primary app's bases (`writeBackupData` per entity, the bytes the
//!   `backupBaseEntity` messages carry). Throughput is bytes, so the
//!   recorded median reads as per-cycle bandwidth.
//! * `backup_restore` — the takeover recovery face: a dead app's stored
//!   backups rebuilt into bases (`BackedUpEntities::restore`).
//! * `archive_tick` — the archive queue face: one full `Archiver` cycle
//!   draining the persisted set over the period's ticks.
//! * `archive_restore` — the restart recovery face: every persisted entry
//!   read back out of the `ArchiveStore` and rebuilt.
//!
//! Wire sizes and queue slices are deterministic (pinned by the module
//! tests); the wall-clock medians are machine-local readings, recorded in
//! the evidence page, never asserted as bands.

use std::collections::HashMap;
use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use foundation::stream::BinaryStream;
use server_runtime::archiver::restore_archived;
use server_runtime::archiver::{ArchiveStore, Archiver};
use server_runtime::base_backup::{BackupMessage, BackupReceiver, write_backup_data};
use server_runtime::baseappmgr::Addr;
use server_runtime::cell_data::BaseAddr;
use server_runtime::entity_creator::{BaseEntity, EntityType, PropValue};

/// Entities per bench set (a small app and a busy one).
const SET_SIZES: [usize; 2] = [64, 512];

/// The archiver period: a cycle drains the persisted set over this many
/// ticks (`archivePeriodInTicks`).
const ARCHIVE_PERIOD: usize = 8;

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

fn lookup() -> impl Fn(&str) -> Option<EntityType> {
    let ty = account_type();
    move |queried: &str| (queried == ty.name).then(|| ty.clone())
}

fn props() -> HashMap<String, PropValue> {
    [
        ("health", PropValue::Int(200)),
        ("name", PropValue::Str("alice".to_string())),
        ("pos", PropValue::Vec3([1.0, 2.0, 3.0])),
        ("alive", PropValue::Bool(true)),
        ("speed", PropValue::Float(2.5)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn persisted_base(id: u32, db_id: u64) -> BaseEntity {
    BaseEntity {
        id,
        type_id: 1,
        type_name: "Account".to_string(),
        db_id,
        is_proxy: false,
        destroyed: false,
        in_destroy: false,
        base_props: props(),
        cell_props: props(),
        is_create_cell_pending: true,
        is_get_cell_pending: false,
        cell_entity_addr: Some(BaseAddr {
            ip: 0x7f00_0001,
            port: 5000,
        }),
        cell_space_id: 3,
        callbacks: Vec::new(),
    }
}

/// One entity's backup bytes, the payload of a `backupBaseEntity` message.
fn backup_bytes(base: &BaseEntity) -> Vec<u8> {
    let mut stream = BinaryStream::new();
    write_backup_data(base, &mut stream);
    stream.as_bytes().to_vec()
}

/// The dead app's source address.
const DEAD_ADDR: Addr = (1, 2);

/// The dead app's receiver pre-loaded with `count` backups.
fn loaded_receiver(count: usize) -> BackupReceiver {
    let mut receiver = BackupReceiver::new();
    let dead = DEAD_ADDR;
    for i in 0..count {
        let base = persisted_base((i + 1) as u32, (i + 1) as u64);
        let message = BackupMessage {
            dest: dead,
            is_offload: false,
            entity_id: base.id,
            data: backup_bytes(&base),
        };
        receiver.back_up_entity(dead, &message);
    }
    receiver
}

fn bench_backup(c: &mut Criterion) {
    let mut group = c.benchmark_group("backup_archive");
    for count in SET_SIZES {
        let bases: Vec<BaseEntity> = (0..count)
            .map(|i| persisted_base((i + 1) as u32, (i + 1) as u64))
            .collect();
        let cycle_bytes: usize = bases.iter().map(|base| backup_bytes(base).len()).sum();

        // The bandwidth face: one full backup cycle, bytes throughput.
        group.throughput(Throughput::Bytes(cycle_bytes as u64));
        group.bench_function(format!("backup_write_{count}"), |bench| {
            bench.iter(|| {
                let mut stream = BinaryStream::new();
                for base in &bases {
                    write_backup_data(base, &mut stream);
                }
                black_box(stream.as_bytes().len())
            });
        });

        // The takeover recovery face: rebuild every stored backup.
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(format!("backup_restore_{count}"), |bench| {
            bench.iter_batched(
                || loaded_receiver(count),
                |receiver| {
                    let rebuilt = receiver.restore(DEAD_ADDR, &lookup());
                    black_box(rebuilt.len())
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    // The archive queue face: one full cycle draining the persisted set.
    let count = SET_SIZES[1];
    let bases: std::collections::BTreeMap<u32, BaseEntity> = (0..count)
        .map(|i| {
            let base = persisted_base((i + 1) as u32, (i + 1) as u64);
            (base.id, base)
        })
        .collect();
    group.throughput(Throughput::Elements(count as u64));
    group.bench_function(format!("archive_tick_{count}"), |bench| {
        bench.iter(|| {
            let mut archiver = Archiver::new(ARCHIVE_PERIOD);
            let mut store = ArchiveStore::new();
            for _ in 0..ARCHIVE_PERIOD {
                black_box(archiver.tick(&bases, &mut store));
            }
            black_box(store.len())
        });
    });

    // The restart recovery face: read every persisted entry back.
    let mut archiver = Archiver::new(ARCHIVE_PERIOD);
    let mut store = ArchiveStore::new();
    for _ in 0..ARCHIVE_PERIOD {
        archiver.tick(&bases, &mut store);
    }
    group.bench_function(format!("archive_restore_{count}"), |bench| {
        bench.iter(|| {
            let mut rebuilt = 0;
            for db_id in 1..=count as u64 {
                if restore_archived(&store, db_id, &lookup()).is_some() {
                    rebuilt += 1;
                }
            }
            black_box(rebuilt)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_backup);
criterion_main!(benches);
