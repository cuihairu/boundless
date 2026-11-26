//! OPT-08-01-01: the dense-AoI baseline.
//!
//! The acceptance: **tick cost, broadcast volume and migration duration
//! recorded** (`保存 Tick、广播量和迁移耗时`). The cluster assesses the
//! AoI index, the Witness scheduling and safe spatial concurrency by
//! measuring a dense region's tick, broadcast and cross-boundary
//! migration; the baseline pins the live faces those rows will compare
//! against:
//!
//! - **The index** (`RangeList`, `cell_range_list.cpp`): a fixed
//!   200×200 dense block walked by an observer whose AoI trigger
//!   (radius `DEFAULT_AOI_RADIUS` = 500, `cellapp_config.cpp:21`)
//!   sweeps the sorted X/Z chains; a density ladder of 49 / 100 / 196 /
//!   400 entities in the same block drives the chain length. Per tick
//!   the observer shuffles 8 units and the trigger fires the boundary
//!   crossings; the shuffle wall time and the event counts are the
//!   index's cost.
//! - **The scheduler** (`Witness`, `witness.cpp`): the same walk feeds
//!   the witness — each trigger enter admits the entity through the
//!   enterAoI/create handshake, each leave removes it, and `update()`
//!   walks the priority queue under a 50 kbit/s, 10 Hz packet budget.
//!   Per tick the messages and bytes are the broadcast volume.
//! - **The migration** (the AoI boundary): the window from the first
//!   crossing to the last admission, the ticks that carry crossings
//!   against the quiet ones, and the per-crossing-tick admission cost.
//!
//! Everything is deterministic (fixed grid, fixed walk), so the run
//! also records a digest over the whole event ledger for the later
//! rows to re-run against.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `AOI_BASELINE_OUT` is set, to that path:
//!
//! ```sh
//! AOI_BASELINE_OUT=<abs path> cargo test -p server-runtime \
//!     --test aoi_baseline -- --nocapture
//! ```

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use server_runtime::cellapp_range_list::{
    FLAG_LOWER_AOI_TRIGGER, FLAG_NO_TRIGGERS, FLAG_UPPER_AOI_TRIGGER, RangeList, TriggerId,
    TriggerTarget,
};
use server_runtime::cellapp_space::Entity;
use server_runtime::witness::{
    DEFAULT_AOI_RADIUS, EntitySnapshot, VolatileInfo, Witness, bps_to_packet_size,
};

/// The dense block's width and depth in metres.
const BLOCK: f32 = 200.0;
/// The observer's metres per tick.
const STEP: f32 = 8.0;
/// The walk's first and last observer x (the z lane is the block's
/// mid-row).
const WALK_START: f32 = -600.0;
const WALK_END: f32 = 800.0;
/// The walk's z lane: between the block's rows so no entity sits on it.
const WALK_Z: f32 = BLOCK / 2.0 - SPACING_400 / 2.0;
/// Half the densest grid's spacing — a fixed offset off the lattice.
const SPACING_400: f32 = 10.0;
/// The density ladder: the grid side per rung (side² entities in the
/// same block — the chain length is the variable).
const SIDES: [usize; 4] = [7, 10, 14, 20];

/// The witness's budget: 50 kbit/s at 10 updates/s
/// (`bpsToPacketSize`, `lib/server/util.hpp`).
const BUDGET_BPS: i32 = 50_000;
const UPDATE_HERTZ: i32 = 10;

/// The observer is entity 1; the block's entities follow.
const OBSERVER_ID: u32 = 1;

type EventLog = Rc<RefCell<Vec<(u32, bool)>>>;

// ---------------------------------------------------------------------------
// The record sink (the tuning-harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$AOI_BASELINE_OUT` when set.
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
        if let Ok(path) = std::env::var("AOI_BASELINE_OUT") {
            std::fs::write(&path, &text).expect("write AOI_BASELINE_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The dense-block fixtures
// ---------------------------------------------------------------------------

/// An entity at `(x, y, z)`, as the range list wants it.
fn entity_at(id: u32, x: f32, y: f32, z: f32) -> Rc<RefCell<Entity>> {
    let mut entity = Entity::new(id, true);
    entity.position = [x, y, z];
    Rc::new(RefCell::new(entity))
}

/// The block's entities for a grid side: `(id, x, z)` with ids from 2,
/// laid out row-major over the fixed block.
fn block_entities(side: usize) -> Vec<(u32, f32, f32)> {
    let spacing = BLOCK / side as f32;
    let mut entities = Vec::with_capacity(side * side);
    let mut id = OBSERVER_ID + 1;
    for row in 0..side {
        for col in 0..side {
            entities.push((id, col as f32 * spacing, row as f32 * spacing));
            id += 1;
        }
    }
    entities
}

/// The observer's AoI trigger (`witness.cpp:73` `AoITrigger`): the
/// bounds want AoI crossings and make none of their own.
fn make_aoi_trigger(list: &mut RangeList, central: usize, range: f32) -> (TriggerId, EventLog) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let target = TriggerTarget::Recorder(Rc::clone(&events));
    let trigger = list.create_trigger(
        central,
        range,
        FLAG_LOWER_AOI_TRIGGER,
        FLAG_UPPER_AOI_TRIGGER,
        FLAG_NO_TRIGGERS,
        FLAG_NO_TRIGGERS,
        target,
    );
    list.trigger_insert(trigger);
    (trigger, events)
}

/// FNV-1a 64, the digest over a rendered state.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Leg 1: the index — the dense ladder's per-tick shuffle cost
// ---------------------------------------------------------------------------

/// What one index walk recorded.
#[derive(Debug, Default)]
struct IndexOutcome {
    entities: usize,
    build_ms: f64,
    shuffle_ns_avg: f64,
    shuffle_ns_max: f64,
    ticks: usize,
    enters: usize,
    leaves: usize,
    ledger_digest: String,
    window_ms: f64,
}

/// Walks the observer across the `side`-sided block, shuffling its node
/// and trigger per tick; returns the per-tick shuffle cost and the
/// event ledger.
fn index_walk(side: usize) -> IndexOutcome {
    let mut list = RangeList::new();
    let observer = entity_at(OBSERVER_ID, WALK_START, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    let entities = block_entities(side);
    let build_started = Instant::now();
    for (id, x, z) in &entities {
        list.add_entity_node(entity_at(*id, *x, 0.0, *z));
    }
    let build_ms = build_started.elapsed().as_secs_f64() * 1000.0;
    let (trigger, events) = make_aoi_trigger(&mut list, observer_node, DEFAULT_AOI_RADIUS);

    let mut outcome = IndexOutcome {
        entities: entities.len(),
        build_ms,
        ..IndexOutcome::default()
    };
    let mut shuffle_ns = Vec::new();
    let mut cursor = 0usize;
    let mut x = WALK_START;
    let window_started = Instant::now();
    while x < WALK_END {
        let old_x = x;
        let old_z = WALK_Z;
        x += STEP;
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        let t0 = Instant::now();
        list.shuffle_entity(observer_node, &[trigger], old_x, old_z);
        shuffle_ns.push(t0.elapsed().as_nanos() as f64);
        outcome.ticks += 1;
        for &(_, entering) in events.borrow().iter().skip(cursor) {
            if entering {
                outcome.enters += 1;
            } else {
                outcome.leaves += 1;
            }
            cursor += 1;
        }
    }
    outcome.window_ms = window_started.elapsed().as_secs_f64() * 1000.0;
    outcome.shuffle_ns_avg = shuffle_ns.iter().sum::<f64>() / shuffle_ns.len() as f64;
    outcome.shuffle_ns_max = shuffle_ns.iter().cloned().fold(0.0, f64::max);

    // The pinned invariants: the index stays sorted, and a monotonic
    // walk through the block enters and leaves every entity exactly
    // once.
    assert!(list.is_sorted(), "the chains stay sorted through the walk");
    assert_eq!(outcome.enters, entities.len(), "every entity enters once");
    assert_eq!(outcome.leaves, entities.len(), "every entity leaves once");

    let mut text = String::new();
    for (tick, (id, entering)) in events.borrow().iter().enumerate() {
        text.push_str(&format!("{tick}\t{id}\t{entering}\n"));
    }
    outcome.ledger_digest = format!("{:016x}", fnv1a64(text.as_bytes()));
    outcome
}

// ---------------------------------------------------------------------------
// Leg 2: the scheduler — the same walk feeding a witness
// ---------------------------------------------------------------------------

/// What one witness walk recorded.
#[derive(Debug, Default)]
struct WitnessOutcome {
    admitted: usize,
    admission_msgs: usize,
    admission_bytes: usize,
    update_msgs_total: usize,
    update_bytes_total: usize,
    update_msgs_max: usize,
    update_bytes_max: usize,
    leaves: usize,
    aoi_final: usize,
    warnings: usize,
    ledger_digest: String,
}

/// The witness-side walk: the trigger's events admit and release the
/// block's entities, and `update()` spends the packet budget each tick.
fn witness_walk(side: usize) -> WitnessOutcome {
    let mut list = RangeList::new();
    let observer = entity_at(OBSERVER_ID, WALK_START, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    let entities = block_entities(side);
    for (id, x, z) in &entities {
        list.add_entity_node(entity_at(*id, *x, 0.0, *z));
    }
    let (trigger, events) = make_aoi_trigger(&mut list, observer_node, DEFAULT_AOI_RADIUS);

    let mut self_entity =
        server_runtime::witness::EntitySnapshot::new(OBSERVER_ID, 1, (WALK_START, 0.0, WALK_Z));
    self_entity.volatile = VolatileInfo::NONE;
    let mut witness = Witness::new(OBSERVER_ID, self_entity);
    witness.set_aoi_radius(DEFAULT_AOI_RADIUS, 0.0);
    witness.set_witness_capacity(BUDGET_BPS, UPDATE_HERTZ);
    let budget = bps_to_packet_size(BUDGET_BPS, UPDATE_HERTZ);
    let watchees: BTreeMap<u32, (f32, f32)> =
        entities.iter().map(|(id, x, z)| (*id, (*x, *z))).collect();

    let mut out = WitnessOutcome::default();
    let mut cursor = 0usize;
    let mut x = WALK_START;
    let mut tick = 0usize;
    while x < WALK_END {
        let old_x = x;
        let old_z = WALK_Z;
        x += STEP;
        tick += 1;

        // The observer's own projection moved; the watched entities are
        // stationary.
        witness.upsert_entity(server_runtime::witness::EntitySnapshot::new(
            OBSERVER_ID,
            1,
            (x, 0.0, WALK_Z),
        ));
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        list.shuffle_entity(observer_node, &[trigger], old_x, old_z);

        // This tick's boundary crossings drive the AoI churn.
        let mut tick_events: Vec<(u32, bool)> = Vec::new();
        for (id, entering) in events.borrow().iter().skip(cursor) {
            tick_events.push((*id, *entering));
            cursor += 1;
        }
        for (id, entering) in &tick_events {
            if *entering {
                // The cell's entity becomes the witness's snapshot, the
                // way `add_to_aoi`'s caller registers it.
                let (px, pz) = watchees[id];
                witness.upsert_entity(EntitySnapshot::new(*id, 1, (px, 0.0, pz)));
                witness.add_to_aoi(*id, false);
                witness.flush_state_changes();
                witness.request_entity_update(*id, &[]);
                witness.flush_state_changes();
            } else {
                witness.remove_from_aoi(*id, false);
                witness.flush_state_changes();
            }
        }
        // The admission bundle: the enterAoI/create/leaveAoI handshake
        // messages, which ride outside the volatile budget.
        let bundle = witness.take_bundle();
        let admission_msgs = bundle.len();
        let admission_bytes: usize = bundle.iter().map(|m| m.wire_size()).sum();
        out.admission_msgs += admission_msgs;
        out.admission_bytes += admission_bytes;
        out.admitted += tick_events.iter().filter(|(_, e)| *e).count();
        out.leaves += tick_events.iter().filter(|(_, e)| !*e).count();

        // The scheduled update pass under the packet budget.
        witness.update();
        let bundle = witness.take_bundle();
        let update_bytes = bundle.iter().map(|m| m.wire_size()).sum::<usize>();
        out.update_msgs_total += bundle.len();
        out.update_bytes_total += update_bytes;
        out.update_msgs_max = out.update_msgs_max.max(bundle.len());
        out.update_bytes_max = out.update_bytes_max.max(update_bytes);
        // The pass checks the budget before each send, so one in-flight
        // element may land after the cut: the tick total overshoots by at
        // most its largest message.
        let max_msg = bundle.iter().map(|m| m.wire_size()).max().unwrap_or(0);
        assert!(
            update_bytes <= budget as usize + max_msg,
            "tick {tick}: the update pass stays within budget plus one send"
        );
    }

    // The ledger is the raw event log in the index walk's format — the
    // digest comparison below pins that the scheduler saw exactly the
    // crossings the index fired, byte for byte.
    let mut ledger = String::new();
    for (i, (id, entering)) in events.borrow().iter().enumerate() {
        ledger.push_str(&format!("{i}\t{id}\t{entering}\n"));
    }

    // Every entity completed the enter/create/…/leave cycle exactly
    // once, and the observer passed them all, so the AoI is empty and
    // every alias is back in the pool.
    assert_eq!(
        out.admitted,
        entities.len(),
        "every entity is admitted once"
    );
    assert_eq!(out.leaves, entities.len(), "every entity leaves once");
    assert!(
        witness.aoi_ids().is_empty(),
        "the observer passed the whole block"
    );
    for (id, _, _) in &entities {
        assert_eq!(
            witness.cache_flags(*id),
            None,
            "entity {id}'s cache is released"
        );
    }
    // The stationary watchees were never projected by the witness
    // (their snapshots never changed after admission); their positions
    // still match the grid.
    for (id, _, _) in &entities {
        let (px, pz) = watchees[id];
        let entity = witness.entity(*id).expect("watchee registered");
        assert_eq!((entity.position.0, entity.position.2), (px, pz));
    }

    out.warnings = witness.warnings().len();
    out.aoi_final = witness.aoi_ids().len();
    out.ledger_digest = format!("{:016x}", fnv1a64(ledger.as_bytes()));
    out
}

// ---------------------------------------------------------------------------
// Leg 3: the migration — the boundary-crossing window
// ---------------------------------------------------------------------------

/// What one migration window recorded, derived from the index walk's
/// ledger.
#[derive(Debug, Default)]
struct MigrationOutcome {
    first_enter_tick: usize,
    last_enter_tick: usize,
    window_ticks: usize,
    crossing_ticks: usize,
    events_total: usize,
    ledger_digest: String,
}

/// Replays the walk's ledger as a per-tick crossing schedule and
/// measures the admission window.
fn migration_window(side: usize) -> MigrationOutcome {
    // Re-run the deterministic walk to collect the per-tick ledger.
    let mut list = RangeList::new();
    let observer = entity_at(OBSERVER_ID, WALK_START, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    let entities = block_entities(side);
    for (id, x, z) in &entities {
        list.add_entity_node(entity_at(*id, *x, 0.0, *z));
    }
    let (trigger, events) = make_aoi_trigger(&mut list, observer_node, DEFAULT_AOI_RADIUS);

    let mut per_tick: Vec<(usize, usize)> = Vec::new(); // (events, enters)
    let mut cursor = 0usize;
    let mut x = WALK_START;
    while x < WALK_END {
        let old_x = x;
        let old_z = WALK_Z;
        x += STEP;
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        list.shuffle_entity(observer_node, &[trigger], old_x, old_z);
        let tick_events = events.borrow().len() - cursor;
        let tick_enters = events.borrow().iter().skip(cursor).filter(|e| e.1).count();
        cursor = events.borrow().len();
        per_tick.push((tick_events, tick_enters));
    }

    let mut out = MigrationOutcome {
        events_total: per_tick.iter().map(|(e, _)| e).sum(),
        ..MigrationOutcome::default()
    };
    assert_eq!(
        out.events_total,
        2 * entities.len(),
        "enter plus leave per entity"
    );
    out.crossing_ticks = per_tick.iter().filter(|(e, _)| *e > 0).count();
    out.first_enter_tick = per_tick
        .iter()
        .position(|(_, en)| *en > 0)
        .map(|i| i + 1)
        .unwrap_or(0);
    out.last_enter_tick = per_tick
        .iter()
        .rposition(|(_, en)| *en > 0)
        .map(|i| i + 1)
        .unwrap_or(0);
    out.window_ticks = out.last_enter_tick - out.first_enter_tick + 1;

    let mut text = String::new();
    for (i, (events, enters)) in per_tick.iter().enumerate() {
        text.push_str(&format!("{}\t{}\t{}\n", i + 1, events, enters));
    }
    out.ledger_digest = format!("{:016x}", fnv1a64(text.as_bytes()));
    out
}

// ---------------------------------------------------------------------------
// The baseline run
// ---------------------------------------------------------------------------

/// Runs the three legs and leaves the baseline record.
#[test]
fn the_dense_aoi_baseline_records_tick_broadcast_and_migration() {
    let started = Instant::now();
    let mut record = Record::default();

    // The index ladder.
    let mut ladder_digests = Vec::new();
    for side in SIDES {
        let out = index_walk(side);
        let label = format!("idx_d{}", out.entities);
        record.put(format!("{label}_entities"), out.entities);
        record.put(format!("{label}_build_ms"), format!("{:.3}", out.build_ms));
        record.put(
            format!("{label}_shuffle_ns_avg"),
            format!("{:.1}", out.shuffle_ns_avg),
        );
        record.put(
            format!("{label}_shuffle_ns_max"),
            format!("{:.1}", out.shuffle_ns_max),
        );
        record.put(format!("{label}_ticks"), out.ticks);
        record.put(format!("{label}_enters"), out.enters);
        record.put(format!("{label}_leaves"), out.leaves);
        record.put(
            format!("{label}_window_ms"),
            format!("{:.3}", out.window_ms),
        );
        record.put(format!("{label}_ledger_digest"), out.ledger_digest.clone());
        ladder_digests.push(out.ledger_digest);
    }

    // The scheduler on the densest block.
    let densest = *SIDES.last().expect("ladder present");
    let out = witness_walk(densest);
    record.put("wit_entities", densest * densest);
    record.put(
        "wit_budget_bytes",
        bps_to_packet_size(BUDGET_BPS, UPDATE_HERTZ),
    );
    record.put("wit_admission_msgs", out.admission_msgs);
    record.put("wit_admission_bytes", out.admission_bytes);
    record.put("wit_update_msgs_total", out.update_msgs_total);
    record.put("wit_update_bytes_total", out.update_bytes_total);
    record.put("wit_update_msgs_max", out.update_msgs_max);
    record.put("wit_update_bytes_max", out.update_bytes_max);
    record.put("wit_leaves_total", out.leaves);
    record.put("wit_aoi_final", out.aoi_final);
    record.put("wit_warnings", out.warnings);
    record.put("wit_ledger_digest", out.ledger_digest.as_str());

    // The migration window on the densest block.
    let mig = migration_window(densest);
    record.put("mig_first_enter_tick", mig.first_enter_tick);
    record.put("mig_last_enter_tick", mig.last_enter_tick);
    record.put("mig_window_ticks", mig.window_ticks);
    record.put("mig_crossing_ticks", mig.crossing_ticks);
    record.put("mig_events_total", mig.events_total);
    record.put("mig_ledger_digest", mig.ledger_digest);

    // The walk is deterministic: the ladder's densest digest and the
    // scheduler's digest must describe the same event stream.
    assert_eq!(
        ladder_digests.last(),
        Some(&out.ledger_digest),
        "the scheduler sees the same crossings the index fired"
    );
    record.put(
        "baseline_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
