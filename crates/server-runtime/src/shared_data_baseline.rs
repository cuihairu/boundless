//! OPT-07-01-03: a seeded baseline for shared-data propagation traffic.
//!
//! The acceptance asks to quantify the message-count change. This module
//! drives a seeded sequence of shared-data mutations through the real
//! propagation chain — the `BaseAppScheduler` and `CellAppMgr`
//! `set_shared_data` / `del_shared_data` faces with their broadcast,
//! forward and relay hops (`shared_data.rs` documents the order) — and
//! counts every message the chain produces. No wall-clock value is
//! sampled, so the ledger reproduces from the seed.

use std::collections::BTreeMap;

use crate::baseappmgr::Addr;
use crate::baseappmgr::{
    BaseAppScheduler, RegisterOutcome, SHARED_DATA_TYPE_BASE_APP, SHARED_DATA_TYPE_CELL_APP,
    SHARED_DATA_TYPE_GLOBAL, SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP, ToBaseApp,
};
use crate::cellappmgr::{CellAppMgr, CellAppRecord};

/// The default baseline seed.
pub const SHARED_DATA_SEED: u64 = 1_618_033;
/// The BaseApps every BASE_APP/GLOBAL mutation broadcasts to.
pub const BASE_APPS: u32 = 4;
/// The CellApps every CELL_APP/GLOBAL mutation broadcasts to.
pub const CELL_APPS: u32 = 3;
/// The keys each data scope cycles over.
pub const KEYS: u32 = 8;
/// The distinct values a key can hold (repeats make no-op writes).
pub const VALUES: u32 = 4;
/// The mutations the sequence drives.
pub const MUTATIONS: u32 = 96;

/// The value pool: a key's value repeats often enough to make no-op
/// writes.
static VALUES_POOL: [&str; VALUES as usize] = ["v0", "v1", "v2", "v3"];

/// One run's propagation-traffic snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedDataLedger {
    /// The mutations the sequence carried.
    pub mutations: usize,
    /// Broadcasts the BaseAppMgr sent to its BaseApps.
    pub base_app_messages: usize,
    /// Broadcasts the CellAppMgr sent to its CellApps.
    pub cell_app_messages: usize,
    /// GLOBAL-FROM-BASE-APP forwards, BaseAppMgr -> CellAppMgr.
    pub forwards: usize,
    /// GLOBAL/BASE_APP relays, CellAppMgr -> BaseAppMgr.
    pub relays: usize,
    /// Every message the chain produced.
    pub messages_total: usize,
    /// Mutations skipped before they produced a message (tuned runs only;
    /// the shipped chain always answers a mutation with traffic).
    pub suppressed_no_ops: usize,
    /// The authoritative BaseApp-scope map at the end.
    pub base_data: BTreeMap<String, String>,
    /// The authoritative CellApp-scope map at the end.
    pub cell_data: BTreeMap<String, String>,
    /// The authoritative Global-scope map at the end (the two managers'
    /// copies agree — pinned by the harness).
    pub global_data: BTreeMap<String, String>,
}

impl SharedDataLedger {
    /// The load both runs carry: the same mutation count and the same
    /// final authoritative maps.
    pub fn same_load(&self, other: &SharedDataLedger) -> bool {
        self.mutations == other.mutations
            && self.base_data == other.base_data
            && self.cell_data == other.cell_data
            && self.global_data == other.global_data
    }

    /// Whether the two managers' global copies agree.
    pub fn global_copies_agree(&self, base_copy: &BTreeMap<String, String>) -> bool {
        self.global_data == *base_copy
    }
}

/// One seeded mutation.
#[derive(Clone, Debug)]
struct Mutation {
    data_type: u8,
    key: String,
    value: &'static str,
    is_del: bool,
}

fn mutation(draw: u64) -> Mutation {
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
    let key_index = (draw / 4) % u64::from(KEYS);
    let is_del = (draw / 4 / u64::from(KEYS)) % 5 == 4;
    Mutation {
        data_type,
        key: format!("key-{scope}-{key_index}"),
        value: VALUES_POOL[((draw / 4 / u64::from(KEYS) / 5) % u64::from(VALUES)) as usize],
        is_del,
    }
}

/// The two managers wired the way the chain expects: four ready BaseApps,
/// three registered CellApps, and the BaseAppMgr registered with the
/// CellAppMgr so the GLOBAL relay fires.
fn wired_managers() -> (BaseAppScheduler, CellAppMgr, Addr) {
    let mut scheduler = BaseAppScheduler::new();
    scheduler.set_cell_app_mgr_established(true);
    scheduler.set_has_init_data(true);
    scheduler.set_db_apps_empty(false);
    for index in 0..BASE_APPS {
        let addr: Addr = (0x7f00_0010 + index, 21_000 + index as u16);
        assert!(
            matches!(
                scheduler.register(addr, (addr.0, addr.1 + 1_000), false, 0),
                RegisterOutcome::Registered(_)
            ),
            "every BaseApp registers"
        );
        // The sync stream is empty until the maps hold entries; the app
        // moves to ready regardless.
        scheduler.finished_init(addr);
    }
    assert_eq!(scheduler.num_base_apps(), BASE_APPS as usize);

    let mut cell_mgr = CellAppMgr::new();
    cell_mgr.set_base_app((0x7f00_0009, 20_009));
    for index in 0..CELL_APPS {
        let addr: Addr = (0x7f00_0020 + index, 22_000 + index as u16);
        cell_mgr
            .apps
            .add(CellAppRecord::new(addr, index + 1, 0, 5_000));
    }
    assert_eq!(cell_mgr.apps.len(), CELL_APPS as usize);
    (scheduler, cell_mgr, base_addrs_src())
}

/// The original invoker address the mutations carry.
fn base_addrs_src() -> Addr {
    (0x7f00_0010, 21_000)
}

/// The authoritative map a mutation's scope answers to.
fn authority<'a>(
    scheduler: &'a BaseAppScheduler,
    cell_mgr: &'a CellAppMgr,
    data_type: u8,
) -> &'a BTreeMap<String, String> {
    match data_type {
        SHARED_DATA_TYPE_CELL_APP => cell_mgr.shared_cell_app_data(),
        SHARED_DATA_TYPE_BASE_APP => scheduler.shared_base_app_data(),
        _ => cell_mgr.shared_global_data(),
    }
}

/// Run the seeded propagation once. When `suppress_no_ops` is set, a
/// mutation whose write would not change the authoritative map (a set of
/// the value it already holds, a del of an absent key) is skipped before
/// it can produce a message.
pub fn run_shared_data_propagation(seed: u64, suppress_no_ops: bool) -> SharedDataLedger {
    let (mut scheduler, mut cell_mgr, src) = wired_managers();
    let mut rng = seed;
    let mut ledger = SharedDataLedger {
        mutations: 0,
        base_app_messages: 0,
        cell_app_messages: 0,
        forwards: 0,
        relays: 0,
        messages_total: 0,
        suppressed_no_ops: 0,
        base_data: BTreeMap::new(),
        cell_data: BTreeMap::new(),
        global_data: BTreeMap::new(),
    };

    for _ in 0..MUTATIONS {
        rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let m = mutation(rng >> 33);
        ledger.mutations += 1;

        if suppress_no_ops {
            let map = authority(&scheduler, &cell_mgr, m.data_type);
            let is_no_op = if m.is_del {
                !map.contains_key(&m.key)
            } else {
                map.get(&m.key).map(String::as_str) == Some(m.value)
            };
            if is_no_op {
                ledger.suppressed_no_ops += 1;
                continue;
            }
        }

        // The hop order `shared_data.rs` documents: the BaseAppMgr side,
        // then the CellAppMgr side, then the BaseAppMgr mirror.
        match m.data_type {
            SHARED_DATA_TYPE_CELL_APP => {
                let outcome = if m.is_del {
                    cell_mgr.del_shared_data(m.data_type, &m.key, src)
                } else {
                    cell_mgr.set_shared_data(m.data_type, &m.key, m.value, src)
                };
                ledger.cell_app_messages += outcome.cell_apps.len();
            }
            SHARED_DATA_TYPE_BASE_APP => {
                let outcome = if m.is_del {
                    scheduler.del_shared_data(m.data_type, &m.key, src)
                } else {
                    scheduler.set_shared_data(m.data_type, &m.key, m.value, src)
                };
                ledger.base_app_messages += count_base_messages(&outcome.base_apps);
            }
            SHARED_DATA_TYPE_GLOBAL | SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP => {
                if m.data_type == SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP {
                    let forward = if m.is_del {
                        scheduler.del_shared_data(m.data_type, &m.key, src)
                    } else {
                        scheduler.set_shared_data(m.data_type, &m.key, m.value, src)
                    };
                    if forward.cell_app_mgr.is_some() {
                        ledger.forwards += 1;
                    }
                }
                // The CellAppMgr holds the Global authority: it stores,
                // broadcasts to the CellApps and relays back.
                let outcome = if m.is_del {
                    cell_mgr.del_shared_data(SHARED_DATA_TYPE_GLOBAL, &m.key, src)
                } else {
                    cell_mgr.set_shared_data(SHARED_DATA_TYPE_GLOBAL, &m.key, m.value, src)
                };
                ledger.cell_app_messages += outcome.cell_apps.len();
                if outcome.base_app_mgr.is_some() {
                    ledger.relays += 1;
                }
                // The BaseAppMgr mirror: store and broadcast to the BaseApps.
                let mirror = if m.is_del {
                    scheduler.del_shared_data(SHARED_DATA_TYPE_GLOBAL, &m.key, src)
                } else {
                    scheduler.set_shared_data(SHARED_DATA_TYPE_GLOBAL, &m.key, m.value, src)
                };
                ledger.base_app_messages += count_base_messages(&mirror.base_apps);
            }
            _ => unreachable!("the sequence only draws the four scopes"),
        }
    }

    ledger.messages_total =
        ledger.base_app_messages + ledger.cell_app_messages + ledger.forwards + ledger.relays;
    ledger.base_data = scheduler.shared_base_app_data().clone();
    ledger.cell_data = cell_mgr.shared_cell_app_data().clone();
    ledger.global_data = cell_mgr.shared_global_data().clone();
    assert!(
        ledger.global_copies_agree(scheduler.shared_global_data()),
        "both managers' global copies agree"
    );
    ledger
}

fn count_base_messages(messages: &[(Addr, ToBaseApp)]) -> usize {
    messages.len()
}

/// Run the shipped propagation: every mutation answers with traffic.
pub fn run_shared_data_baseline(seed: u64) -> SharedDataLedger {
    run_shared_data_propagation(seed, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_baseline_reproduces_from_its_seed() {
        assert_eq!(
            run_shared_data_baseline(SHARED_DATA_SEED),
            run_shared_data_baseline(SHARED_DATA_SEED)
        );
    }

    #[test]
    fn the_shipped_chain_answers_every_mutation_with_traffic() {
        let run = run_shared_data_baseline(SHARED_DATA_SEED);
        assert_eq!(run.mutations, MUTATIONS as usize);
        assert_eq!(run.suppressed_no_ops, 0);
        assert!(run.base_app_messages > 0);
        assert!(run.cell_app_messages > 0);
        assert!(run.forwards > 0);
        assert!(run.relays > 0);
        assert_eq!(
            run.messages_total,
            run.base_app_messages + run.cell_app_messages + run.forwards + run.relays
        );
        assert!(run.base_data.contains_key("key-base-0"));
        assert!(run.cell_data.contains_key("key-cell-0"));
        assert!(run.global_data.contains_key("key-global-0"));
    }
}
