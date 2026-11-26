//! OPT-08-01-03: the space-parallelism evaluation.
//!
//! The acceptance reports **the cross-boundary sequence and its cost**
//! (`报告跨边界时序及耗时`). The harness drives the live migration
//! faces and timestamps every stage:
//!
//! - **The cell-boundary migration (same space).** A real walks from
//!   cell A into cell B across a two-cell space: the per-tick
//!   `OffloadChecker::run` plans the move, the interest ghost precedes
//!   it, and the plan tick runs `convert_real_to_ghost` →
//!   `receive_onload` → the settle (`receive_ghost_set_real`). Each
//!   stage is timed and its message count recorded.
//! - **The space-boundary teleport.** `teleport_entity_to_space` →
//!   `receive_onload_teleported_entity`, the same instrumentation.
//! - **The parallelism envelope.** P independent worlds each migrating
//!   one real: the per-migration cost across P measures the
//!   data-parallel claim, and the shared-budget run over P reals in one
//!   `OffloadChecker` measures the cross-entity coupling (the one face
//!   that serializes).
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `SPACE_PARALLEL_OUT` is set, to that path:
//!
//! ```sh
//! SPACE_PARALLEL_OUT=<abs path> cargo test -p server-runtime \
//!     --test space_parallel_eval -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::Instant;

use foundation::math::Vector3;
use server_runtime::cellappmgr::{Addr, Rect};
use server_runtime::controllers::Controllers;
use server_runtime::entity_creator::PropValue;
use server_runtime::ghost::{
    GhostChannel, GhostConfig, GhostControllerSeed, GhostCreateOutcome, GhostEntityTypeDesc,
    GhostHost, GhostMessage, GhostSetRealOutcome, GhostedEntityView, OffloadChecker, TeleportLoad,
    convert_real_to_ghost, offload_sign_off, teleport_entity_to_space,
};
use server_runtime::real_entity::{EntityCallbackBuffer, Haunt, RealEntity};
use server_runtime::space::CellInfoRecord;
use server_runtime::witness::VolatileInfo;

/// The two cells and the spare channel address, as the unit fixtures
/// use.
const APP_A: Addr = (1, 1);
const APP_B: Addr = (2, 2);
const APP_C: Addr = (3, 3);
/// The space id the fixtures share.
const SPACE: server_runtime::cellappmgr::SpaceId = 7;
/// The entity type every fixture registers.
const TYPE_ID: u16 = 4;
/// The walk: one 5-unit step per tick from mid-A across the boundary.
const STEP: f32 = 5.0;
const START_X: f32 = 50.0;
/// The boundary sits at x = 100.
const BOUNDARY: f32 = 100.0;

// ---------------------------------------------------------------------------
// The record sink (the candidate harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$SPACE_PARALLEL_OUT` when set.
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
        if let Ok(path) = std::env::var("SPACE_PARALLEL_OUT") {
            std::fs::write(&path, &text).expect("write SPACE_PARALLEL_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The fixtures (the unit-fixture shapes, lifted into the test)
// ---------------------------------------------------------------------------

/// A two-cell space: A owns x in [0,100), B owns x in [100,200).
fn two_cells() -> BTreeMap<Addr, CellInfoRecord> {
    let mut cells = BTreeMap::new();
    cells.insert(
        APP_A,
        CellInfoRecord::new(APP_A, Rect::new(0.0, 0.0, 100.0, 100.0)),
    );
    cells.insert(
        APP_B,
        CellInfoRecord::new(APP_B, Rect::new(100.0, 0.0, 200.0, 100.0)),
    );
    for info in cells.values_mut() {
        info.has_been_created = true;
    }
    cells
}

/// Channels for A, B and the spare address.
fn channels() -> BTreeMap<Addr, GhostChannel> {
    [APP_A, APP_B, APP_C]
        .into_iter()
        .map(|addr| (addr, GhostChannel::new(addr)))
        .collect()
}

/// The fixture config: a tight ghost distance keeps the interest bands
/// inside the walk.
fn config() -> GhostConfig {
    GhostConfig {
        ghost_distance: 30.0,
        ..GhostConfig::default()
    }
}

/// A ghost host that knows type 4: 2 event stamps, 2 ghost properties —
/// the view's counts.
fn host() -> GhostHost {
    let mut h = GhostHost::default();
    h.register_type(
        TYPE_ID,
        GhostEntityTypeDesc {
            prop_count_ghost: 2,
            num_event_stamped_properties: 2,
        },
    );
    h
}

/// A real of type 4 living on A at `(x, 0, 50)`, with the view the
/// migration faces stream.
fn walking_real(id: u32, x: f32) -> (RealEntity, GhostedEntityView) {
    let mut real = RealEntity::new(id, APP_A, Vector3::new(x, 0.0, 50.0), 0, 0);
    real.creation_time = 0;
    let view = GhostedEntityView {
        entity_id: id,
        entity_type_id: TYPE_ID,
        appeal_radius: 0.0,
        num_times_real_offloaded: 0,
        position: Vector3::new(x, 0.0, 50.0),
        is_on_ground: true,
        last_event_number: 0,
        volatile: VolatileInfo::NONE,
        base_addr: (9, 9),
        direction: Vector3::new(1.5, 0.25, -0.5),
        property_event_stamps: vec![7, 13],
        ghost_properties: vec![PropValue::Int(5), PropValue::Str("hi".to_string())],
        real_properties: vec![PropValue::Int(99)],
        ghost_controllers: vec![GhostControllerSeed {
            id: 3,
            type_id: 9,
            user_arg: 42,
            payload: vec![1, 2, 3],
        }],
        periods_without_witness: 4,
        aoi_update_scheme_id: 3,
    };
    (real, view)
}

// ---------------------------------------------------------------------------
// Leg 1: the cell-boundary migration — the sequence and its cost
// ---------------------------------------------------------------------------

/// What one boundary migration recorded.
#[derive(Debug, Default)]
struct MigrationOutcome {
    ghost_tick: usize,
    plan_tick: usize,
    checker_ns_total: f64,
    checker_ns_max: f64,
    offload_ns: f64,
    onload_ns: f64,
    settle_ns: f64,
    migration_total_ns: f64,
    offload_msgs_b: usize,
    onload_msgs_a: usize,
    onload_msgs_c: usize,
    ticks: usize,
}

/// Walks the real from `START_X` until the checker plans the crossing,
/// then migrates it, timing every stage.
fn cross_boundary_migration(id: u32) -> MigrationOutcome {
    let cells = two_cells();
    let mut channels = channels();
    let mut host_a = host();
    let mut host_b = host();
    let cfg = config();
    let (real, view) = walking_real(id, START_X);
    let mut reals = vec![(real, view)];
    let mut controllers = Controllers::new();
    let mut entity_controllers = Controllers::new();
    let mut callbacks = EntityCallbackBuffer::new();
    let mut checker = OffloadChecker {
        should_offload: true,
        ..OffloadChecker::default()
    };

    let mut out = MigrationOutcome::default();
    let mut x = START_X;
    let mut tick = 0usize;
    let migration_started = Instant::now();

    // The approach: per-tick checker runs until the plan appears.
    loop {
        x += STEP;
        tick += 1;
        reals[0].1.position = Vector3::new(x, 0.0, 50.0);

        let t0 = Instant::now();
        let plans = checker.run(
            &cells,
            &mut channels,
            &mut reals,
            &cfg,
            APP_A,
            SPACE,
            tick as u32,
        );
        let ns = t0.elapsed().as_nanos() as f64;
        out.checker_ns_total += ns;
        out.checker_ns_max = out.checker_ns_max.max(ns);
        out.ticks = tick;

        if out.ghost_tick == 0
            && channels[&APP_B]
                .out
                .iter()
                .any(|m| matches!(m, GhostMessage::CreateGhost(p) if p.entity_id == id))
        {
            out.ghost_tick = tick;
        }
        if let Some(p) = plans.first() {
            assert_eq!(p.destination, APP_B, "the plan names the home cell");
            assert_eq!(p.entity_id, id, "the plan names the walker");
            break;
        }
        assert!(tick < 40, "the walker crosses the boundary");
    }
    // The approach phase's interest ghost rode B's channel: B applies
    // the create before the offload body lands (`cellapp.cpp`'s create
    // then onload order).
    let mut create = None;
    for m in std::mem::take(&mut channels.get_mut(&APP_B).expect("B's channel").out) {
        if let GhostMessage::CreateGhost(p) = m {
            create = Some(p);
        }
    }
    let create = create.expect("the interest ghost rides B's channel");
    assert_eq!(
        host_b.receive_create_ghost(APP_A, create, out.ghost_tick as u32),
        GhostCreateOutcome::Created,
        "B holds the interest ghost from the approach"
    );

    out.plan_tick = tick;
    // The interest ghost precedes the offload: B held the ghost before
    // the move was planned.
    assert!(
        out.ghost_tick > 0 && out.ghost_tick < out.plan_tick,
        "the ghost leads the migration"
    );
    // The plan fires on the crossing tick.
    let crossing_tick = ((BOUNDARY - START_X) / STEP) as usize;
    assert_eq!(
        out.plan_tick, crossing_tick,
        "the plan fires on the crossing"
    );

    // Stage: the offload half — the real streams and dies on A.
    let (mut real, view) = reals.pop().expect("the walker stays in the run");
    let t0 = Instant::now();
    let (payload, ghost_record) = convert_real_to_ghost(
        &mut real,
        &view,
        &mut controllers,
        &mut callbacks,
        APP_B,
        APP_A,
        SPACE,
        false,
        tick as u32,
        &mut channels,
    );
    out.offload_ns = t0.elapsed().as_nanos() as f64;
    out.offload_msgs_b = channels[&APP_B].out.len();
    host_a.ghosts.insert(id, ghost_record);
    // The destination is skipped in the sign-off: nothing rides to B.
    assert_eq!(
        out.offload_msgs_b, 0,
        "the offload streams only on the payload"
    );
    assert_eq!(
        payload.haunt_addrs,
        vec![APP_B, APP_A],
        "the haunt list names the destination first"
    );

    // Stage: the onload half — B's ghost becomes the one real and the
    // haunt pass re-points A.
    let t0 = Instant::now();
    let outcome = host_b
        .receive_onload(
            payload,
            &mut entity_controllers,
            &mut callbacks,
            SPACE,
            None,
            &mut channels,
            APP_B,
            tick as u32,
        )
        .expect("the destination ghosted the entity first");
    out.onload_ns = t0.elapsed().as_nanos() as f64;
    assert!(outcome.are_we_haunted, "B was in the haunt list");
    assert_eq!(outcome.real.id, id);
    assert!(host_b.ghosts.is_empty(), "the ghost became the real");
    out.onload_msgs_a = channels[&APP_A].out.len();
    out.onload_msgs_c = channels[&APP_C].out.len();

    // Stage: the settle — A applies the re-point and the migration is
    // done: one real, at B.
    let t0 = Instant::now();
    assert_eq!(
        host_a.receive_ghost_set_real(id, 1, APP_B),
        GhostSetRealOutcome::Relocated,
        "A relocates its ghost"
    );
    out.settle_ns = t0.elapsed().as_nanos() as f64;
    assert_eq!(host_a.ghosts[&id].real_addr, APP_B);
    out.migration_total_ns = migration_started.elapsed().as_nanos() as f64;
    out
}

// ---------------------------------------------------------------------------
// Leg 1 + 2: the sequences and their costs
// ---------------------------------------------------------------------------

fn sequence_leg(record: &mut Record) {
    // The cell-boundary migration.
    let out = cross_boundary_migration(10);
    record.put("seq_ghost_tick", out.ghost_tick);
    record.put("seq_plan_tick", out.plan_tick);
    record.put("seq_ticks_walked", out.ticks);
    record.put(
        "seq_checker_ns_avg",
        format!("{:.0}", out.checker_ns_total / out.ticks as f64),
    );
    record.put("seq_checker_ns_max", format!("{:.0}", out.checker_ns_max));
    record.put("seq_stage_offload_ns", format!("{:.0}", out.offload_ns));
    record.put("seq_stage_onload_ns", format!("{:.0}", out.onload_ns));
    record.put("seq_stage_settle_ns", format!("{:.0}", out.settle_ns));
    record.put(
        "seq_migration_total_ns",
        format!("{:.0}", out.migration_total_ns),
    );
    record.put("seq_offload_msgs_b", out.offload_msgs_b);
    record.put("seq_onload_msgs_a", out.onload_msgs_a);
    record.put("seq_onload_msgs_c", out.onload_msgs_c);
    assert!(
        out.onload_msgs_a > 0,
        "the haunt pass re-points the source ghost"
    );
    assert_eq!(out.onload_msgs_c, 0, "the spare app stays silent");

    // The space-boundary teleport: fresh world, same walker.
    let mut channels = channels();
    let mut host_b = host();
    let (mut real, view) = walking_real(20, START_X);
    let mut controllers = Controllers::new();
    let mut entity_controllers = Controllers::new();
    let mut callbacks = EntityCallbackBuffer::new();
    real.haunts = vec![Haunt {
        addr: APP_C,
        creation_time: 100,
    }];

    let t0 = Instant::now();
    let (payload, ghost_record, del_addrs) = teleport_entity_to_space(
        &mut real,
        &view,
        &mut controllers,
        &mut callbacks,
        APP_A,
        SPACE,
        APP_B,
        21,
        300,
        &mut channels,
    );
    let send_ns = t0.elapsed().as_nanos() as f64;
    assert_eq!(del_addrs, vec![APP_C], "the stale ghost deletes first");
    assert_eq!(
        channels[&APP_C].out,
        vec![GhostMessage::DelGhost { entity_id: 20 }],
        "the delGhost rides before the teleport payload"
    );

    let t1 = Instant::now();
    let loaded = host_b.receive_onload_teleported_entity(
        APP_A,
        true,
        payload,
        &mut entity_controllers,
        &mut callbacks,
        SPACE,
        &mut channels,
        APP_B,
        300,
    );
    let onload_ns = t1.elapsed().as_nanos() as f64;
    let TeleportLoad::Loaded(outcome) = loaded else {
        panic!("the nearby entity exists: the teleport loads");
    };
    assert_eq!(outcome.nearby_id, 21);
    assert_eq!(
        outcome.create.space_id, SPACE,
        "the destination rewrites the space"
    );
    assert!(outcome.onload.are_we_haunted);
    let _ = ghost_record;

    record.put("tspace_stage_send_ns", format!("{:.0}", send_ns));
    record.put("tspace_stage_onload_ns", format!("{:.0}", onload_ns));
    record.put("tspace_total_ns", format!("{:.0}", send_ns + onload_ns));
    record.put("tspace_del_addrs", del_addrs.len());
    record.put("tspace_loaded", "true");
}

// ---------------------------------------------------------------------------
// Leg 3: the parallelism envelope
// ---------------------------------------------------------------------------

/// Migrates `p` independent worlds back to back; returns the total and
/// per-migration walls.
fn parallel_envelope(p: usize) -> (f64, f64) {
    let started = Instant::now();
    for i in 0..p {
        let out = cross_boundary_migration(100 + i as u32);
        assert_eq!(out.plan_tick, 10, "migrant {i} crosses on the same tick");
    }
    let total = started.elapsed().as_nanos() as f64;
    (total, total / p as f64)
}

/// The shared-budget face: one checker visiting `p` reals spends the
/// ghost-deletion budget across them — the one cross-entity coupling.
fn shared_budget_run(p: usize) -> (f64, usize) {
    let cells = two_cells();
    let mut channels = channels();
    let cfg = GhostConfig {
        max_ghosts_to_delete: 1,
        ..config()
    };
    let mut checker = OffloadChecker {
        should_offload: true,
        ..OffloadChecker::default()
    };
    let mut reals: Vec<(RealEntity, GhostedEntityView)> =
        (0..p).map(|i| walking_real(200 + i as u32, 50.0)).collect();

    let started = Instant::now();
    let plans = checker.run(&cells, &mut channels, &mut reals, &cfg, APP_A, SPACE, 1);
    let ns = started.elapsed().as_nanos() as f64;
    assert!(plans.is_empty(), "the mid-A reals stay home");
    let _ = &mut channels;
    (ns, plans.len())
}

fn parallel_leg(record: &mut Record) {
    let mut per_migration_base = 0.0;
    for p in [1usize, 2, 4, 8] {
        let (total, per) = parallel_envelope(p);
        if p == 1 {
            per_migration_base = per;
        }
        record.put(format!("par_p{p}_total_ns"), format!("{:.0}", total));
        record.put(format!("par_p{p}_per_migration_ns"), format!("{:.0}", per));
    }

    // One checker over 8 reals vs 8 checkers over one real each: the
    // same face count, the shared budget vs none.
    let (shared_ns, shared_plans) = shared_budget_run(8);
    record.put("par_shared_budget_run_ns", format!("{:.0}", shared_ns));
    record.put("par_shared_budget_plans", shared_plans);

    let mut split_ns = 0.0;
    for i in 0..8usize {
        let started = Instant::now();
        let (ns, plans) = shared_budget_run(1);
        let _ = i;
        assert_eq!(plans, 0);
        split_ns += ns;
        let _ = started;
    }
    record.put("par_split_run_ns_sum", format!("{:.0}", split_ns));
    record.put(
        "par_p1_per_migration_ns",
        format!("{:.0}", per_migration_base),
    );
}

/// Runs the evaluation and leaves the record.
#[test]
fn the_space_parallel_eval_reports_the_boundary_sequence_and_cost() {
    let started = Instant::now();
    let mut record = Record::default();
    sequence_leg(&mut record);
    parallel_leg(&mut record);
    record.put(
        "eval_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}

/// The offload body's shape, pinned once: the payload the migration
/// streams carries the real-only state, the sign-off tells every other
/// haunt where the real is going, and the destination is skipped.
#[test]
fn the_boundary_payloads_keep_their_reference_shape() {
    let cells = two_cells();
    let mut channels = channels();
    let cfg = config();
    let mut host_a = host();
    let _ = &cells;
    let _ = &cfg;

    let (mut real, view) = walking_real(30, 140.0);
    real.haunts = vec![Haunt {
        addr: APP_B,
        creation_time: 100,
    }];
    let mut controllers = Controllers::new();
    let mut callbacks = EntityCallbackBuffer::new();

    let (payload, ghost_record) = convert_real_to_ghost(
        &mut real,
        &view,
        &mut controllers,
        &mut callbacks,
        APP_B,
        APP_A,
        SPACE,
        false,
        300,
        &mut channels,
    );
    assert_eq!(payload.entity_id, 30);
    assert_eq!(payload.entity_type_id, TYPE_ID);
    assert_eq!(payload.real_properties, vec![PropValue::Int(99)]);
    assert_eq!(payload.space_id, SPACE);
    assert!(!payload.is_teleport);
    assert_eq!(payload.haunt_addrs, vec![APP_B, APP_A]);
    assert_eq!(ghost_record.real_addr, APP_B);
    assert!(channels[&APP_B].out.is_empty(), "the destination skips");
    host_a.ghosts.insert(30, ghost_record);

    // The sign-off tells every other haunt where the real is going.
    let (mut real2, _) = walking_real(31, 50.0);
    real2.haunts = vec![
        Haunt {
            addr: APP_B,
            creation_time: 100,
        },
        Haunt {
            addr: APP_C,
            creation_time: 100,
        },
    ];
    offload_sign_off(&real2, APP_B, &mut channels);
    assert!(channels[&APP_B].out.is_empty(), "the destination skips");
    assert!(matches!(
        channels[&APP_C].out.first(),
        Some(GhostMessage::SetNextReal(_))
    ));
}
