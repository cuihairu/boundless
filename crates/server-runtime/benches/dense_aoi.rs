//! OPT-08-01-01 dense-AOI baseline benchmark.
//!
//! The structural broadcast volume — the bytes the three dense faces charge
//! the client bundle — is pinned by
//! `the_dense_area_broadcast_volume_is_pinned` (enter 16 bytes, steady-tick
//! update 16 bytes, migration 24 bytes per entity). What the wall clock adds
//! is the cost of producing that traffic at dense scale:
//!
//! * `tick` — one witness update pass over N in-AoI entities on a spacing-5
//!   grid, every entity moved since the last pass, under a packet budget
//!   large enough that the whole queue drains (the dense worst case:
//!   everyone broadcasts every tick).
//! * `migration` — one boundary-crossing wave: every entity leaves the AoI
//!   and is re-observed (leave + enter + create, three messages each), the
//!   witness-side cost a cross-cell migration charges per wave.
//! * `offload_decision` — the per-tick `collect_offloads` sweep over N reals
//!   straddling a cell boundary, half of which must migrate.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use server_runtime::cellappmgr::Rect as CellRect;
use server_runtime::ghost_data::Address;
use server_runtime::offload::{Branch, Cell, OffloadGate, SpaceNode, collect_offloads};
use server_runtime::witness::{EntitySnapshot, VolatileInfo, Witness};

/// Dense fleet scales: entities packed at spacing 5, all in AoI.
const FLEET_SIZES: [u32; 2] = [64, 512];

/// Grid spacing of the dense packing (`dense-aoi` scenario's value).
const SPACING: f32 = 5.0;

/// Witness packet budget: 2 Mbit/s at 10 Hz minus UDP overhead — generous
/// enough that the whole dense queue drains every pass.
const BUDGET_BITS_PER_SECOND: i32 = 2_000_000;
const UPDATE_HERTZ: i32 = 10;

/// A dense grid of `count` volatile entities at [`SPACING`], centred on the
/// witness's own position.
fn dense_positions(count: u32) -> Vec<(f32, f32, f32)> {
    let side = (count as f32).sqrt().ceil() as usize;
    let centre = (side as f32 - 1.0) * SPACING / 2.0;
    (0..count)
        .map(|i| {
            let col = (i as usize % side) as f32;
            let row = (i as usize / side) as f32;
            (col * SPACING - centre, 0.0, row * SPACING - centre)
        })
        .collect()
}

fn dense_witness(count: u32) -> (Witness, Vec<EntitySnapshot>) {
    let mut self_entity = EntitySnapshot::new(1, 1, (0.0, 0.0, 0.0));
    self_entity.volatile = VolatileInfo::NONE;
    let mut witness = Witness::new(1, self_entity);
    witness.set_witness_capacity(BUDGET_BITS_PER_SECOND, UPDATE_HERTZ);
    let mut entities = Vec::new();
    for (i, position) in dense_positions(count).into_iter().enumerate() {
        let entity = EntitySnapshot::new(i as u32 + 2, 2, position);
        witness.upsert_entity(entity.clone());
        entities.push(entity);
    }
    // First observation: enter, client request, create — the steady state
    // every measured pass starts from.
    for id in 2..count + 2 {
        witness.add_to_aoi(id, false);
    }
    witness.flush_state_changes();
    for id in 2..count + 2 {
        witness.request_entity_update(id, &[]);
    }
    witness.flush_state_changes();
    let _ = witness.take_bundle();
    (witness, entities)
}

fn bench_dense_aoi(c: &mut Criterion) {
    let mut group = c.benchmark_group("dense_aoi");
    for count in FLEET_SIZES {
        // The tick face: every entity moved since the last pass (the
        // harness bumps the volatile stamp and shifts position), one full
        // update pass plus the bundle drain.
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(format!("tick_{count}"), |bench| {
            let (mut witness, entities) = dense_witness(count);
            let mut outward = true;
            bench.iter(|| {
                for entity in &entities {
                    let mut moved = entity.clone();
                    moved.volatile_update_number += 1;
                    moved.position.0 += if outward { 0.25 } else { -0.25 };
                    witness.upsert_entity(moved);
                }
                outward = !outward;
                witness.update();
                let bundle = witness.take_bundle();
                black_box(bundle.len())
            });
        });

        // The migration face: one full boundary-crossing wave per pass —
        // leave, flush, re-enter, flush, client request, create flush.
        group.bench_function(format!("migration_{count}"), |bench| {
            let (mut witness, _entities) = dense_witness(count);
            bench.iter(|| {
                for id in 2..count + 2 {
                    witness.remove_from_aoi(id, false);
                }
                witness.flush_state_changes();
                for id in 2..count + 2 {
                    witness.add_to_aoi(id, false);
                }
                witness.flush_state_changes();
                for id in 2..count + 2 {
                    witness.request_entity_update(id, &[]);
                }
                witness.flush_state_changes();
                let bundle = witness.take_bundle();
                black_box(bundle.len())
            });
        });

        // The cross-boundary decision sweep: half the reals sit across the
        // x = 100 plane from their owning cell, so every pass offloads a
        // fixed half.
        group.bench_function(format!("offload_decision_{count}"), |bench| {
            let tree = SpaceNode::Branch(Branch {
                position: 100.0,
                horizontal: false,
                left: Box::new(SpaceNode::Cell(Cell::new(
                    Address(100),
                    CellRect::new(0.0, 0.0, 100.0, 100.0),
                ))),
                right: Box::new(SpaceNode::Cell(Cell::new(
                    Address(200),
                    CellRect::new(100.0, 0.0, 200.0, 100.0),
                ))),
            });
            let gate = OffloadGate::default();
            let channels = [(Address(100), true), (Address(200), true)]
                .into_iter()
                .collect();
            // Alternating owners and positions: the x values straddle the
            // x = 100 plane, so exactly half the reals must offload.
            let reals: Vec<(u32, (f64, f64), Address)> = (0..count)
                .map(|i| {
                    let x = 70.0 + (i % 10) as f64 * 4.0;
                    let owner = if i % 2 == 0 {
                        Address(100)
                    } else {
                        Address(200)
                    };
                    (i + 1, (x, 50.0), owner)
                })
                .collect();
            bench.iter(|| {
                let moved = collect_offloads(&reals, &tree, &gate, &channels);
                black_box(moved.len())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_dense_aoi);
criterion_main!(benches);
