//! OPT-08-01-03 spatial-parallelism evaluation benchmark.
//!
//! The candidate split: one witness per client-backed entity, each with its
//! own AoI map, queue and bundle — witnesses are state-disjoint, so a
//! spatial partition (contiguous grid rows) can drive its witnesses
//! concurrently with no locks. Each measured pass drives every witness
//! through one full dense update (64 watched entities each, all moved,
//! budget-drained queue — the OPT-08-01-01 tick face scaled to a fleet of
//! witnesses):
//!
//! * `serial` — one thread, the current single-threaded tick shape.
//! * `par2` / `par4` — `std::thread::scope` workers, one chunk of
//!   witnesses each.
//!
//! Throughput is witness-passes per second; each pass covers 64 watched
//! entities.

use std::hint::black_box;
use std::thread;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use server_runtime::witness::{EntitySnapshot, VolatileInfo, Witness};

/// Witness (client) counts.
const FLEET_SIZES: [u32; 2] = [64, 512];

/// Entities watched per witness (the dense per-client AoI).
const WATCHED: u32 = 64;

/// Grid spacing of the dense packing.
const SPACING: f32 = 5.0;

/// Witness packet budget: 2 Mbit/s at 10 Hz minus UDP overhead.
const BUDGET_BITS_PER_SECOND: i32 = 2_000_000;
const UPDATE_HERTZ: i32 = 10;

fn dense_witness(first_id: u32) -> (Witness, Vec<EntitySnapshot>) {
    let mut self_entity = EntitySnapshot::new(first_id, 1, (0.0, 0.0, 0.0));
    self_entity.volatile = VolatileInfo::NONE;
    let mut witness = Witness::new(first_id, self_entity);
    witness.set_witness_capacity(BUDGET_BITS_PER_SECOND, UPDATE_HERTZ);
    let side = ((WATCHED + 1) as f32).sqrt().ceil() as usize;
    let centre = (side as f32 - 1.0) * SPACING / 2.0;
    let mut entities = Vec::new();
    for i in 0..WATCHED {
        let col = (i as usize % side) as f32;
        let row = (i as usize / side) as f32;
        let entity = EntitySnapshot::new(
            first_id + 1 + i,
            2,
            (col * SPACING - centre, 0.0, row * SPACING - centre),
        );
        witness.upsert_entity(entity.clone());
        entities.push(entity);
    }
    for id in first_id + 1..first_id + 1 + WATCHED {
        witness.add_to_aoi(id, false);
    }
    witness.flush_state_changes();
    for id in first_id + 1..first_id + 1 + WATCHED {
        witness.request_entity_update(id, &[]);
    }
    witness.flush_state_changes();
    let _ = witness.take_bundle();
    (witness, entities)
}

/// One fleet of witnesses with their watched sets, at steady state.
fn fleet(count: u32) -> Vec<(Witness, Vec<EntitySnapshot>)> {
    (0..count)
        .map(|w| dense_witness(1 + w * (WATCHED + 1)))
        .collect()
}

/// One moved-tick pass over the fleet: harness feeds movement, then the
/// witness's own update drains its queue.
fn drive_fleet(fleet: &mut [(Witness, Vec<EntitySnapshot>)], outward: bool) -> usize {
    let mut messages = 0;
    for (witness, entities) in fleet {
        for entity in entities.iter_mut() {
            entity.volatile_update_number += 1;
            entity.position.0 += if outward { 0.25 } else { -0.25 };
        }
        for entity in entities.iter() {
            witness.upsert_entity(entity.clone());
        }
        witness.update();
        messages += witness.take_bundle().len();
    }
    messages
}

fn bench_spatial_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("spatial_parallel");
    for count in FLEET_SIZES {
        let unit = format!("witnesses_{count}");
        group.throughput(Throughput::Elements(count as u64));

        group.bench_function(format!("{unit}_serial"), |bench| {
            let mut fleet = fleet(count);
            let mut outward = true;
            bench.iter(|| {
                black_box(drive_fleet(&mut fleet, outward));
                outward = !outward;
            });
        });

        for workers in [2u64, 4] {
            group.bench_function(format!("{unit}_par{workers}"), |bench| {
                let mut fleet = fleet(count);
                let mut outward = true;
                bench.iter(|| {
                    let chunk = (fleet.len() as u64).div_ceil(workers) as usize;
                    thread::scope(|scope| {
                        for slice in fleet.chunks_mut(chunk) {
                            scope.spawn(|| {
                                black_box(drive_fleet(slice, outward));
                            });
                        }
                    });
                    outward = !outward;
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_spatial_parallel);
criterion_main!(benches);
