//! OPT-07-01-04: re-run the takeover and check what comes back.
//!
//! The acceptance asks that the restored entities and the shared state are
//! correct. This module runs the whole takeover once per seed: a seeded
//! fleet streams through the real backup faces, the source app dies, the
//! `BackupReceiver::restore` rebuilds every stored backup and each is
//! compared against its source properties; in parallel the two managers
//! build authoritative shared-data maps through the real `set_shared_data`
//! chain, and a fresh manager pair recovers both from the recovery stream
//! (`recoverSharedDataFromStream`'s count-then-pairs sections). The ledger
//! records the entity matches and whether every recovered map equals its
//! original. No wall-clock value is sampled; the rerun reproduces from the
//! seed.

use std::collections::{BTreeMap, HashMap};

use foundation::stream::BinaryStream;

use crate::backup_hash::{BackupHash, BackupHashChain};
use crate::base_backup::{BackupMessage, BackupReceiver, backup_destination, write_backup_data};
use crate::baseappmgr::{
    Addr, BaseAppScheduler, RegisterOutcome, SHARED_DATA_TYPE_BASE_APP, SHARED_DATA_TYPE_CELL_APP,
    SHARED_DATA_TYPE_GLOBAL, SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP,
};
use crate::cellappmgr::{CellAppMgr, CellAppRecord};
use crate::entity_creator::{BaseEntity, EntityType, PropValue};

/// The default rerun seed.
pub const RERUN_SEED: u64 = 424_242;
/// The fleet the takeover rebuilds.
pub const FLEET: u32 = 12;
/// The shared-data mutations the run drives before the recovery.
pub const MUTATIONS: u32 = 16;

/// One takeover rerun's correctness snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeoverRerunLedger {
    /// The fleet the stream carried.
    pub entities: usize,
    /// Entities the takeover rebuilt from the stored backups.
    pub restored_entities: usize,
    /// Restored entities whose properties equal the source's.
    pub property_matches: usize,
    /// BASE_APP entries the fresh BaseAppMgr recovered.
    pub base_shared_entries: usize,
    /// GLOBAL entries the fresh BaseAppMgr recovered.
    pub base_global_entries: usize,
    /// CELL_APP entries the fresh CellAppMgr recovered.
    pub cell_shared_entries: usize,
    /// GLOBAL entries the fresh CellAppMgr recovered.
    pub cell_global_entries: usize,
    /// The recovered BaseApp-scope map equals the original.
    pub base_map_ok: bool,
    /// The recovered BaseAppMgr global map equals the original.
    pub base_global_ok: bool,
    /// The recovered CellApp-scope map equals the original.
    pub cell_map_ok: bool,
    /// The recovered CellAppMgr global map equals the original.
    pub cell_global_ok: bool,
}

/// The entity type every fleet member carries.
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

/// One seeded fleet entity: the seed drives its database id (every second
/// entity persists) and its properties.
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
    base_props.insert("alive".to_string(), PropValue::Bool(id.is_multiple_of(2)));
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
        cell_entity_addr: None,
        cell_space_id: 0,
        callbacks: Vec::new(),
    }
}

/// One seeded shared-data mutation.
struct SharedMutation {
    data_type: u8,
    key: String,
    value: &'static str,
    is_del: bool,
}

fn shared_mutation(draw: u64) -> SharedMutation {
    let data_type = match draw % 4 {
        0 => SHARED_DATA_TYPE_CELL_APP,
        1 => SHARED_DATA_TYPE_BASE_APP,
        2 => SHARED_DATA_TYPE_GLOBAL,
        _ => SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP,
    };
    let scope = match data_type {
        SHARED_DATA_TYPE_CELL_APP => "cell",
        SHARED_DATA_TYPE_BASE_APP => "base",
        SHARED_DATA_TYPE_GLOBAL => "global",
        _ => "from-base",
    };
    SharedMutation {
        data_type,
        key: format!("key-{scope}-{}", (draw / 4) % 3),
        value: match (draw / 4 / 3) % 2 {
            0 => "v0",
            _ => "v1",
        },
        is_del: (draw / 4 / 3 / 2) % 4 == 3,
    }
}

/// Two ready BaseApps and two registered CellApps, with the BaseAppMgr
/// registered so the GLOBAL relay fires.
fn wired_managers() -> (BaseAppScheduler, CellAppMgr) {
    let mut scheduler = BaseAppScheduler::new();
    scheduler.set_cell_app_mgr_established(true);
    scheduler.set_has_init_data(true);
    scheduler.set_db_apps_empty(false);
    for index in 0..2 {
        let addr: Addr = (0x7f00_0030 + index, 23_000 + index as u16);
        assert!(
            matches!(
                scheduler.register(addr, (addr.0, addr.1 + 1_000), false, 0),
                RegisterOutcome::Registered(_)
            ),
            "every BaseApp registers"
        );
        scheduler.finished_init(addr);
    }
    let mut cell_mgr = CellAppMgr::new();
    cell_mgr.set_base_app((0x7f00_0039, 23_009));
    for index in 0..2 {
        let addr: Addr = (0x7f00_0040 + index, 24_000 + index as u16);
        cell_mgr
            .apps
            .add(CellAppRecord::new(addr, index + 1, 0, 5_000));
    }
    (scheduler, cell_mgr)
}

/// Drive one seeded mutation through the real chain (the hop order
/// `shared_data.rs` documents). The global scopes land on both managers
/// through the forward/relay loop.
fn drive_mutation(
    scheduler: &mut BaseAppScheduler,
    cell_mgr: &mut CellAppMgr,
    m: &SharedMutation,
    src: Addr,
) {
    match m.data_type {
        SHARED_DATA_TYPE_CELL_APP => {
            if m.is_del {
                cell_mgr.del_shared_data(m.data_type, &m.key, src);
            } else {
                cell_mgr.set_shared_data(m.data_type, &m.key, m.value, src);
            }
        }
        SHARED_DATA_TYPE_BASE_APP => {
            if m.is_del {
                scheduler.del_shared_data(m.data_type, &m.key, src);
            } else {
                scheduler.set_shared_data(m.data_type, &m.key, m.value, src);
            }
        }
        _ => {
            if m.data_type == SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP {
                if m.is_del {
                    scheduler.del_shared_data(m.data_type, &m.key, src);
                } else {
                    scheduler.set_shared_data(m.data_type, &m.key, m.value, src);
                }
            }
            if m.is_del {
                cell_mgr.del_shared_data(SHARED_DATA_TYPE_GLOBAL, &m.key, src);
            } else {
                cell_mgr.set_shared_data(SHARED_DATA_TYPE_GLOBAL, &m.key, m.value, src);
            }
            if m.is_del {
                scheduler.del_shared_data(SHARED_DATA_TYPE_GLOBAL, &m.key, src);
            } else {
                scheduler.set_shared_data(SHARED_DATA_TYPE_GLOBAL, &m.key, m.value, src);
            }
        }
    }
}

/// Write one recovery section: a count, then count key/value pairs — the
/// shape `read_shared_section` reads back.
fn write_shared_section(map: &BTreeMap<String, String>, stream: &mut BinaryStream) {
    stream.write_u32(map.len() as u32);
    for (key, value) in map {
        stream.write_string(key).expect("the key encodes");
        stream.write_string(value).expect("the value encodes");
    }
}

/// Run the takeover rerun once.
pub fn run_takeover_rerun(seed: u64) -> TakeoverRerunLedger {
    let ty = fleet_type();
    let lookup = {
        let name = ty.name.clone();
        move |queried: &str| (queried == name).then(|| ty.clone())
    };
    let src_addr: Addr = (0x7f00_0031, 23_001);

    // The backup stream: the whole fleet through the real payload and
    // destination faces into the receiver.
    let mut rng = seed;
    let fleet: Vec<BaseEntity> = (0..FLEET)
        .map(|index| fleet_entity(index, &mut rng))
        .collect();
    let hash = BackupHash::new();
    let chain = BackupHashChain::new();
    let mut receiver = BackupReceiver::new();
    for base in &fleet {
        let mut stream = BinaryStream::new();
        write_backup_data(base, &mut stream);
        let message = BackupMessage {
            dest: backup_destination(&hash, &chain, base.id, false),
            is_offload: false,
            entity_id: base.id,
            data: stream.as_bytes().to_vec(),
        };
        receiver.back_up_entity(src_addr, &message);
    }

    // The takeover: the source app died; every stored backup rebuilds and
    // is compared against its source's properties.
    let restored = receiver.restore(src_addr, &lookup);
    let mut restored_entities = 0usize;
    let mut property_matches = 0usize;
    for base in restored {
        let base = base.expect("every stored backup rebuilds");
        if let Some(source) = fleet.iter().find(|b| b.id == base.id)
            && source.base_props == base.base_props
        {
            property_matches += 1;
        }
        restored_entities += 1;
    }

    // The shared state: a seeded mutation sequence builds the two
    // managers' authoritative maps through the real chain.
    let (mut scheduler, mut cell_mgr) = wired_managers();
    let src: Addr = (0x7f00_0032, 23_010);
    for _ in 0..MUTATIONS {
        rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let m = shared_mutation(rng >> 33);
        drive_mutation(&mut scheduler, &mut cell_mgr, &m, src);
    }
    let original_base = scheduler.shared_base_app_data().clone();
    let original_base_global = scheduler.shared_global_data().clone();
    let original_cell = cell_mgr.shared_cell_app_data().clone();
    let original_cell_global = cell_mgr.shared_global_data().clone();

    // The recovery: both maps stream out and a fresh manager pair reads
    // them back through the real recovery faces.
    let mut stream = BinaryStream::new();
    write_shared_section(&original_base, &mut stream);
    write_shared_section(&original_base_global, &mut stream);
    let mut fresh_scheduler = BaseAppScheduler::new();
    fresh_scheduler
        .recover_shared_data_from_stream(&mut stream)
        .expect("the BaseAppMgr recovery reads");

    let mut cell_stream = BinaryStream::new();
    write_shared_section(&original_cell, &mut cell_stream);
    write_shared_section(&original_cell_global, &mut cell_stream);
    let mut fresh_cell_mgr = CellAppMgr::default();
    fresh_cell_mgr
        .recover_shared_data_from_stream(&mut cell_stream)
        .expect("the CellAppMgr recovery reads");

    TakeoverRerunLedger {
        entities: FLEET as usize,
        restored_entities,
        property_matches,
        base_shared_entries: fresh_scheduler.shared_base_app_data().len(),
        base_global_entries: fresh_scheduler.shared_global_data().len(),
        cell_shared_entries: fresh_cell_mgr.shared_cell_app_data().len(),
        cell_global_entries: fresh_cell_mgr.shared_global_data().len(),
        base_map_ok: *fresh_scheduler.shared_base_app_data() == original_base,
        base_global_ok: *fresh_scheduler.shared_global_data() == original_base_global,
        cell_map_ok: *fresh_cell_mgr.shared_cell_app_data() == original_cell,
        cell_global_ok: *fresh_cell_mgr.shared_global_data() == original_cell_global,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rerun_reproduces_from_its_seed() {
        assert_eq!(
            run_takeover_rerun(RERUN_SEED),
            run_takeover_rerun(RERUN_SEED)
        );
    }

    #[test]
    fn the_takeover_restores_every_entity_with_its_properties() {
        let run = run_takeover_rerun(RERUN_SEED);
        assert_eq!(run.restored_entities, run.entities);
        assert_eq!(run.property_matches, run.entities);
    }

    #[test]
    fn the_recovered_shared_state_matches_the_originals() {
        let run = run_takeover_rerun(RERUN_SEED);
        assert!(run.base_shared_entries > 0);
        assert!(run.cell_shared_entries > 0);
        assert!(run.base_global_entries > 0);
        assert!(run.cell_global_entries > 0);
        assert!(run.base_map_ok);
        assert!(run.base_global_ok);
        assert!(run.cell_map_ok);
        assert!(run.cell_global_ok);
    }

    #[test]
    fn the_correctness_holds_across_seeds() {
        for seed in [RERUN_SEED, RERUN_SEED + 5, RERUN_SEED + 11] {
            let run = run_takeover_rerun(seed);
            assert_eq!(
                run.restored_entities, run.entities,
                "fleet restored at {seed}"
            );
            assert_eq!(
                run.property_matches, run.entities,
                "properties match at {seed}"
            );
            assert!(run.base_map_ok && run.base_global_ok, "base maps at {seed}");
            assert!(run.cell_map_ok && run.cell_global_ok, "cell maps at {seed}");
        }
    }
}
