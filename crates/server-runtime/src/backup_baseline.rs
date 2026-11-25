//! OPT-07-01-01: a seeded baseline for backup, takeover and archive.
//!
//! The acceptance asks to save the backup bandwidth, the queue and the
//! recovery duration. This module streams a seeded fleet of bases through
//! the real faces — `write_backup_data` payload, the hash-chain destination
//! choice, the receiver's stored backups, the takeover `restore`, and the
//! periodic archiver — and records the whole ledger. Every number is a
//! deterministic count: bytes per backup tick (bandwidth), the outstanding
//! queue depths, and the ticks until the archiver has re-persisted every
//! restored entity (recovery duration). No wall-clock value is sampled, so
//! the ledger reproduces from the seed.

use std::collections::{BTreeMap, HashMap, VecDeque};

use foundation::stream::BinaryStream;

use crate::archiver::{ArchiveStore, Archiver, restore_archived};
use crate::backup_hash::{BackupHash, BackupHashChain};
use crate::base_backup::{BackupMessage, BackupReceiver, backup_destination, write_backup_data};
use crate::baseappmgr::Addr;
use crate::cell_data::BaseAddr;
use crate::entity_creator::{BaseEntity, EntityType, PropValue};

/// The default baseline seed.
pub const BACKUP_SEED: u64 = 161_803;
/// The fleet the baseline streams.
pub const FLEET: u32 = 48;
/// The bases the sender backs up per tick.
pub const BACKUP_SLICE: usize = 8;
/// The archiver cycle the recovery is measured against.
pub const ARCHIVE_PERIOD: usize = 8;

/// The type every entity in the fleet carries.
pub fn fleet_type() -> EntityType {
    EntityType {
        name: "Account".to_string(),
        id: 1,
        can_be_on_base: true,
        is_service: false,
        is_proxy: false,
        can_be_on_cell: true,
        properties: vec![],
        methods: vec![],
    }
}

/// One run's bandwidth, queue and recovery snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct BackupBaselineLedger {
    /// The fleet size.
    pub entities: usize,
    /// How many of them carry a database id.
    pub persisted_entities: usize,
    /// Backup ticks until the sender's queue drained.
    pub backup_ticks: u32,
    /// Payload bytes sent per backup tick (the bandwidth shape).
    pub bandwidth_bytes: Vec<usize>,
    /// Payload bytes across the whole backup stream.
    pub backup_bytes_total: usize,
    /// The largest outstanding-sender queue seen.
    pub sender_queue_peak: usize,
    /// The largest receiver store depth seen.
    pub receiver_queue_peak: usize,
    /// Entities the takeover rebuilt from the stored backups.
    pub restored_entities: usize,
    /// Restored entities whose properties equal the source's.
    pub property_matches: usize,
    /// Archiver ticks until every restored persisted entity was re-written.
    pub archive_recovery_ticks: u32,
    /// Entities the recovery cycle wrote to the archive store.
    pub archived_entities: usize,
    /// Archived entities that round-trip out of the store byte-identical.
    pub archive_restore_ok: usize,
}

/// Build one seeded fleet entity: the seed drives its id-derived database
/// id (every second entity persists), its cell binding and its properties.
fn fleet_entity(index: u32, rng: &mut u64) -> BaseEntity {
    *rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let draw = (*rng >> 33) as u32;
    let id = index + 1;
    let mut base_props = HashMap::new();
    base_props.insert(
        "health".to_string(),
        PropValue::Int(100 + (draw % 200) as i64),
    );
    base_props.insert("name".to_string(), PropValue::Str(format!("Ent{id}")));
    base_props.insert("alive".to_string(), PropValue::Bool(!id.is_multiple_of(3)));
    base_props.insert(
        "speed".to_string(),
        PropValue::Float(1.0 + (draw % 1000) as f64 / 1000.0),
    );
    base_props.insert(
        "pos".to_string(),
        PropValue::Vec3([id as f32, (draw % 1000) as f32, (draw % 500) as f32]),
    );
    BaseEntity {
        id,
        type_id: 1,
        type_name: "Account".to_string(),
        db_id: if id.is_multiple_of(2) {
            u64::from(id) * 10
        } else {
            0
        },
        is_proxy: false,
        destroyed: false,
        in_destroy: false,
        base_props: base_props.clone(),
        cell_props: base_props,
        is_create_cell_pending: false,
        is_get_cell_pending: false,
        cell_entity_addr: if id.is_multiple_of(4) {
            Some(BaseAddr {
                ip: 0x7f00_0001,
                port: 5000 + id as u16,
            })
        } else {
            None
        },
        cell_space_id: id % 7,
        callbacks: Vec::new(),
    }
}

/// Run the seeded backup-and-recovery baseline once.
///
/// The sender drains its fleet through the real backup payload and
/// destination faces a slice per tick; the receiver stores the messages.
/// The source app then dies, the takeover restores every stored backup, and
/// the archiver re-persists the restored entities a slice per tick until
/// the store holds them all.
pub fn run_backup_baseline(seed: u64) -> BackupBaselineLedger {
    let ty = fleet_type();
    let lookup = {
        let name = ty.name.clone();
        move |queried: &str| (queried == name).then(|| ty.clone())
    };
    let src_addr: Addr = (0x7f00_0001, 20_000);

    let mut rng = seed;
    let fleet: Vec<BaseEntity> = (0..FLEET)
        .map(|index| fleet_entity(index, &mut rng))
        .collect();
    let persisted = fleet.iter().filter(|b| b.db_id != 0).count();

    // The backup stream: a slice per tick through the real payload and
    // destination faces, queued on the receiver.
    let hash = BackupHash::new();
    let chain = BackupHashChain::new();
    let mut queue: VecDeque<&BaseEntity> = fleet.iter().collect();
    let mut receiver = BackupReceiver::new();
    let mut ledger = BackupBaselineLedger {
        entities: FLEET as usize,
        persisted_entities: persisted,
        backup_ticks: 0,
        bandwidth_bytes: Vec::new(),
        backup_bytes_total: 0,
        sender_queue_peak: queue.len(),
        receiver_queue_peak: 0,
        restored_entities: 0,
        property_matches: 0,
        archive_recovery_ticks: 0,
        archived_entities: 0,
        archive_restore_ok: 0,
    };

    while !queue.is_empty() {
        ledger.backup_ticks += 1;
        let mut tick_bytes = 0usize;
        for base in queue.drain(..BACKUP_SLICE.min(queue.len())) {
            let mut stream = BinaryStream::new();
            write_backup_data(base, &mut stream);
            let data = stream.as_bytes().to_vec();
            tick_bytes += data.len();
            let dest = backup_destination(&hash, &chain, base.id, false);
            let message = BackupMessage {
                dest,
                is_offload: false,
                entity_id: base.id,
                data,
            };
            receiver.back_up_entity(src_addr, &message);
        }
        ledger.bandwidth_bytes.push(tick_bytes);
        ledger.backup_bytes_total += tick_bytes;
        ledger.sender_queue_peak = ledger.sender_queue_peak.max(queue.len());
        let stored: usize = receiver.backups().values().map(|m| m.len()).sum();
        ledger.receiver_queue_peak = ledger.receiver_queue_peak.max(stored);
    }
    assert_eq!(
        ledger.backup_ticks,
        (FLEET as usize).div_ceil(BACKUP_SLICE) as u32,
        "the queue drains one slice per tick"
    );

    // The takeover: the source app died; every stored backup is rebuilt and
    // checked against the source's properties.
    let restored = receiver.restore(src_addr, &lookup);
    let mut bases: BTreeMap<u32, BaseEntity> = BTreeMap::new();
    for base in restored {
        assert!(
            base.is_ok(),
            "every stored backup rebuilds: {:?}",
            base.as_ref().err()
        );
        let base = base.expect("checked above");
        if let Some(source) = fleet.iter().find(|b| b.id == base.id)
            && source.base_props == base.base_props
        {
            ledger.property_matches += 1;
        }
        bases.insert(base.id, base);
    }
    ledger.restored_entities = bases.len();
    assert_eq!(
        ledger.restored_entities, ledger.entities,
        "the takeover rebuilds the whole fleet"
    );
    assert_eq!(
        ledger.property_matches, ledger.entities,
        "every restored entity carries the source's properties"
    );

    // The recovery duration: the taking-over app's archiver re-persists the
    // restored entities a slice per tick until the store holds them all.
    let mut archiver = Archiver::new(ARCHIVE_PERIOD);
    let mut store = ArchiveStore::new();
    while store.len() < ledger.persisted_entities {
        ledger.archive_recovery_ticks += 1;
        assert!(
            ledger.archive_recovery_ticks <= 10_000,
            "the recovery cycle stalled"
        );
        let outcome = archiver.tick(&bases, &mut store);
        ledger.archived_entities += outcome.archived.len();
    }
    assert_eq!(
        ledger.archived_entities, ledger.persisted_entities,
        "one cycle writes every persisted entity once"
    );

    // The archive round-trip: every persisted entity restores out of the
    // store with its properties intact.
    for base in bases.values() {
        if base.db_id == 0 {
            continue;
        }
        match restore_archived(&store, base.db_id, &lookup) {
            Some(Ok(archived)) => {
                if archived.base_props == base.base_props {
                    ledger.archive_restore_ok += 1;
                }
            }
            other => panic!("the archived entity restores: {other:?}"),
        }
    }
    assert_eq!(
        ledger.archive_restore_ok, ledger.persisted_entities,
        "every persisted entity round-trips the archive"
    );

    ledger
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_baseline_reproduces_from_its_seed() {
        assert_eq!(
            run_backup_baseline(BACKUP_SEED),
            run_backup_baseline(BACKUP_SEED)
        );
    }

    #[test]
    fn the_backup_stream_drains_the_queue() {
        let run = run_backup_baseline(BACKUP_SEED);
        assert_eq!(run.backup_ticks, 6);
        assert!(run.sender_queue_peak > 0);
        assert!(run.receiver_queue_peak > 0);
        assert_eq!(run.bandwidth_bytes.len(), 6);
        assert!(run.bandwidth_bytes.iter().all(|b| *b > 0));
        assert!(run.backup_bytes_total > 0);
        assert_eq!(
            run.bandwidth_bytes.iter().sum::<usize>(),
            run.backup_bytes_total
        );
    }

    #[test]
    fn the_takeover_restores_every_entity_with_its_properties() {
        let run = run_backup_baseline(BACKUP_SEED);
        assert_eq!(run.restored_entities, run.entities);
        assert_eq!(run.property_matches, run.entities);
    }

    #[test]
    fn the_archive_recovery_repersists_every_persisted_entity() {
        let run = run_backup_baseline(BACKUP_SEED);
        assert_eq!(run.persisted_entities, (FLEET / 2) as usize);
        assert!(run.archive_recovery_ticks > 0);
        assert_eq!(run.archived_entities, run.persisted_entities);
        assert_eq!(run.archive_restore_ok, run.persisted_entities);
    }
}
