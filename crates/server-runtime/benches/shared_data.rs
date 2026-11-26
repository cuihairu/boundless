//! OPT-07-01-03 shared-data propagation benchmark.
//!
//! The message volume itself is structural — one mutation sends exactly one
//! message to every subscriber app of the scope, plus the manager relay pair
//! on the global routes — and is pinned by the
//! `the_shared_data_message_volume_is_one_per_subscriber` ledger test. What
//! the wall clock can still say something about is the manager-side
//! construction cost of one mutation, measured here per route:
//!
//! * `base_set` — a BASE_APP set broadcast to every ready BaseApp.
//! * `global_from_base` — the full cross-manager route (forward, CellAppMgr
//!   broadcast + relay, BaseAppMgr broadcast), `2*n + 2` messages.
//! * `join_sync` — one new BaseApp finishing init with an existing shared
//!   map: the manager hands it every entry (one message per entry).

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use server_runtime::baseappmgr::{
    BaseAppScheduler, SHARED_DATA_TYPE_BASE_APP, SHARED_DATA_TYPE_GLOBAL,
    SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP,
};
use server_runtime::cellappmgr::{CellAppMgr, CellAppRecord};

/// Ready apps per fleet scale.
const SET_SIZES: [u16; 2] = [64, 512];

/// Shared entries handed to a late-joining app.
const SYNC_ENTRIES: usize = 64;

fn addr(i: u16) -> (u32, u16) {
    (1, 1000 + i)
}

fn client_addr(i: u16) -> (u32, u16) {
    (1, 2000 + i)
}

fn ready_scheduler() -> BaseAppScheduler {
    let mut scheduler = BaseAppScheduler::new();
    scheduler.set_cell_app_mgr_established(true);
    scheduler.set_has_init_data(true);
    scheduler.set_db_apps_empty(false);
    scheduler
}

fn add_base_apps(scheduler: &mut BaseAppScheduler, count: u16) {
    for i in 0..count {
        let _ = scheduler.register(addr(i), client_addr(i), false, 10);
        scheduler.finished_init(addr(i));
    }
}

fn cell_mgr(count: u16) -> CellAppMgr {
    let mut mgr = CellAppMgr::new();
    mgr.base_app_addr = Some((0x7f00_0001, 100));
    mgr.db_app_alpha = Some((0x7f00_0001, 200));
    mgr.allow_new_cell_apps = true;
    for i in 0..count {
        let rec = CellAppRecord::new((2, 3000 + i), (i + 1) as u32, 0, 10);
        mgr.apps.add(rec);
    }
    mgr
}

fn bench_shared_data(c: &mut Criterion) {
    let mut group = c.benchmark_group("shared_data");
    for count in SET_SIZES {
        // One BASE_APP mutation: `count` broadcast messages.
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(format!("base_set_{count}"), |bench| {
            let mut scheduler = ready_scheduler();
            add_base_apps(&mut scheduler, count);
            bench.iter(|| {
                let out = scheduler.set_shared_data(
                    SHARED_DATA_TYPE_BASE_APP,
                    "fleet.key",
                    "fleet.value",
                    (5, 1),
                );
                black_box(out.base_apps.len())
            });
        });

        // The full global-from-BaseApp route: forward + CellAppMgr broadcast
        // and relay + BaseAppMgr broadcast = `2*count + 2` messages.
        group.throughput(Throughput::Elements((2 * count as u64) + 2));
        group.bench_function(format!("global_from_base_{count}"), |bench| {
            let mut scheduler = ready_scheduler();
            add_base_apps(&mut scheduler, count);
            let mut mgr = cell_mgr(count);
            bench.iter(|| {
                let fwd = scheduler.set_shared_data(
                    SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP,
                    "fleet.key",
                    "fleet.value",
                    (5, 1),
                );
                let cell_out = mgr.set_shared_data(
                    SHARED_DATA_TYPE_GLOBAL,
                    "fleet.key",
                    "fleet.value",
                    (5, 1),
                );
                let base_out = scheduler.set_shared_data(
                    SHARED_DATA_TYPE_GLOBAL,
                    "fleet.key",
                    "fleet.value",
                    (0, 0),
                );
                black_box(fwd.base_apps.len() + cell_out.cell_apps.len() + base_out.base_apps.len())
            });
        });
    }

    // The join-sync face: a late BaseApp is handed every existing entry.
    group.throughput(Throughput::Elements(SYNC_ENTRIES as u64));
    group.bench_function(format!("join_sync_{SYNC_ENTRIES}"), |bench| {
        bench.iter_batched(
            || {
                let mut scheduler = ready_scheduler();
                add_base_apps(&mut scheduler, 1);
                for e in 0..SYNC_ENTRIES {
                    scheduler.set_shared_data(
                        SHARED_DATA_TYPE_BASE_APP,
                        &format!("entry{e}"),
                        "v",
                        (0, 0),
                    );
                }
                let late = addr(9_000);
                let _ = scheduler.register(late, client_addr(9_000), false, 10);
                (scheduler, late)
            },
            |(mut scheduler, late)| {
                let sync = scheduler.finished_init(late);
                black_box(sync.len())
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_shared_data);
criterion_main!(benches);
