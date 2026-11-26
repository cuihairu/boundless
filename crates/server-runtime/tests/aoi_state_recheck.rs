//! OPT-08-01-04: the state recheck over the cluster's re-runs.
//!
//! The acceptance: **re-run the visible sets, the event order and the
//! migrations — the states are correct** (`复跑可见集、事件顺序和迁移。
//! 验收 状态正确。`). The harness replays the faces the cluster
//! measured and asserts their state after every tick and every stage:
//!
//! - **The visible set + the event order.** The OPT-08-01-01 witness
//!   walk, re-run: per tick the witness's admitted set must equal the
//!   geometric truth (the square `(central − range, central + range]` on
//!   both axes, `is_in_x_range`'s comparisons), every flip into or out
//!   of the range must produce exactly one enter/leave event, every
//!   entity's event ticks must equal the ticks its containment flipped,
//!   aliases exist exactly inside the set, and the whole ledger must
//!   replay the baseline's digest.
//! - **The hysteresis state.** The OPT-08-01-02 C2 form, re-run with two
//!   live triggers (radius, radius + hyst): per tick everything inside
//!   the radius is admitted, everything admitted is inside the release
//!   band, the inner ledger alternates enter/leave, and the admission
//!   ledger conserves (enters = releases + still admitted).
//! - **The migration states.** The OPT-08-01-03 sequence, re-run with a
//!   per-tick message pump: B applies its channel's create as it
//!   arrives, and after every stage the reals, the ghost tables, the
//!   ghost addresses and the channels must hold the reference state;
//!   the teleport ends with one real, no ghosts anywhere.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `AOI_STATE_RECHECK_OUT` is set, to that path:
//!
//! ```sh
//! AOI_STATE_RECHECK_OUT=<abs path> cargo test -p server-runtime \
//!     --test aoi_state_recheck -- --nocapture
//! ```

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use foundation::math::Vector3;
use server_runtime::cellapp_range_list::{
    FLAG_LOWER_AOI_TRIGGER, FLAG_NO_TRIGGERS, FLAG_UPPER_AOI_TRIGGER, RangeList, TriggerId,
    TriggerTarget,
};
use server_runtime::cellapp_space::Entity;
use server_runtime::cellappmgr::{Addr, Rect};
use server_runtime::controllers::Controllers;
use server_runtime::entity_creator::PropValue;
use server_runtime::ghost::{
    GhostChannel, GhostConfig, GhostControllerSeed, GhostCreateOutcome, GhostEntityTypeDesc,
    GhostHost, GhostMessage, GhostSetRealOutcome, GhostedEntityView, OffloadChecker, TeleportLoad,
    convert_real_to_ghost, teleport_entity_to_space,
};
use server_runtime::real_entity::{EntityCallbackBuffer, Haunt, RealEntity};
use server_runtime::space::CellInfoRecord;
use server_runtime::witness::{DEFAULT_AOI_RADIUS, EntitySnapshot, VolatileInfo, Witness};

// ---------------------------------------------------------------------------
// The dense-block fixtures (the baseline's shapes)
// ---------------------------------------------------------------------------

/// The dense block's width and depth in metres.
const BLOCK: f32 = 200.0;
/// The observer's metres per tick.
const STEP: f32 = 8.0;
/// The walk's first and last observer x.
const WALK_START: f32 = -600.0;
const WALK_END: f32 = 800.0;
/// The walk's z lane: between the block's rows so no entity sits on it.
const WALK_Z: f32 = BLOCK / 2.0 - SPACING_400 / 2.0;
/// Half the densest grid's spacing — a fixed offset off the lattice.
const SPACING_400: f32 = 10.0;
/// The densest grid's side (the d400 block the baseline pinned).
const SIDE: usize = 20;
/// The witness's capacity shape, as the baseline fed it.
const BUDGET_BPS: i32 = 50_000;
const UPDATE_HERTZ: i32 = 10;

/// The observer is entity 1; the block's entities follow.
const OBSERVER_ID: u32 = 1;
/// The baseline's d400 ledger digest — the rerun must replay it.
const BASELINE_D400_DIGEST: &str = "49bf554c6f1c96b7";

/// The AoI exit hysteresis band (the C2 evaluation's value).
const HYST: f32 = 50.0;
/// The flutter sweep the hysteresis leg walks: an approach, then
/// `PERIODS` full spans at `SWING_STEP` per tick, ending at `SWING_LOW`.
const SWING_LOW: f32 = -520.0;
const SWING_HIGH: f32 = -460.0;
const SWING_STEP: f32 = 5.0;
const PERIODS: usize = 10;
/// The approach: 80 units at 8 per tick.
const APPROACH_TICKS: usize = 10;
/// One swing leg: 60 units at 5 per tick.
const LEG_TICKS: usize = 12;

type EventLog = Rc<RefCell<Vec<(u32, bool)>>>;

/// The record sink (the baseline's shape).
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
        if let Ok(path) = std::env::var("AOI_STATE_RECHECK_OUT") {
            std::fs::write(&path, &text).expect("write AOI_STATE_RECHECK_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// An entity at `(x, y, z)`, as the range list wants it.
fn entity_at(id: u32, x: f32, y: f32, z: f32) -> Rc<RefCell<Entity>> {
    let mut entity = Entity::new(id, true);
    entity.position = [x, y, z];
    Rc::new(RefCell::new(entity))
}

/// The d400 block's entities: `(id, x, z)` with ids from 2, row-major.
fn block_entities() -> Vec<(u32, f32, f32)> {
    let spacing = BLOCK / SIDE as f32;
    let mut entities = Vec::with_capacity(SIDE * SIDE);
    let mut id = OBSERVER_ID + 1;
    for row in 0..SIDE {
        for col in 0..SIDE {
            entities.push((id, col as f32 * spacing, row as f32 * spacing));
            id += 1;
        }
    }
    entities
}

/// The observer's AoI trigger (`witness.cpp:73` `AoITrigger`).
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

/// The trigger's containment: the square `(central − range,
/// central + range]` on both axes (`is_in_x_range`/`is_in_z_range`).
fn contains(central: (f32, f32), other: (f32, f32), range: f32) -> bool {
    (central.0 - range) < other.0
        && other.0 <= central.0 + range
        && (central.1 - range) < other.1
        && other.1 <= central.1 + range
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
// Leg 1: the visible sets and the event order, re-run on the d400 walk
// ---------------------------------------------------------------------------

/// What the rerun checked.
#[derive(Debug, Default)]
struct WalkOutcome {
    ticks: usize,
    enters: usize,
    leaves: usize,
    set_checks: usize,
    flip_events: usize,
    alias_checks: usize,
    snapshot_checks: usize,
    warnings: usize,
    final_aoi: usize,
    ledger_digest: String,
}

/// Replays the baseline's witness walk; per tick the witness's visible
/// set, the aliases, the snapshots and the event log must match the
/// geometric truth.
fn visible_set_and_order_walk() -> WalkOutcome {
    let mut list = RangeList::new();
    let observer = entity_at(OBSERVER_ID, WALK_START, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    let entities = block_entities();
    for (id, x, z) in &entities {
        list.add_entity_node(entity_at(*id, *x, 0.0, *z));
    }
    let (trigger, events) = make_aoi_trigger(&mut list, observer_node, DEFAULT_AOI_RADIUS);
    let grid: BTreeMap<u32, (f32, f32)> =
        entities.iter().map(|(id, x, z)| (*id, (*x, *z))).collect();

    let mut self_entity = EntitySnapshot::new(OBSERVER_ID, 1, (WALK_START, 0.0, WALK_Z));
    self_entity.volatile = VolatileInfo::NONE;
    let mut witness = Witness::new(OBSERVER_ID, self_entity);
    witness.set_aoi_radius(DEFAULT_AOI_RADIUS, 0.0);
    witness.set_witness_capacity(BUDGET_BPS, UPDATE_HERTZ);

    let mut out = WalkOutcome::default();
    let mut enter_tick: BTreeMap<u32, usize> = BTreeMap::new();
    let mut leave_tick: BTreeMap<u32, usize> = BTreeMap::new();
    let mut prev_expected: BTreeSet<u32> = BTreeSet::new();
    let mut cursor = 0usize;
    let mut x = WALK_START;
    let mut tick = 0usize;
    while x < WALK_END {
        let old_x = x;
        let old_z = WALK_Z;
        x += STEP;
        tick += 1;
        witness.upsert_entity(EntitySnapshot::new(OBSERVER_ID, 1, (x, 0.0, WALK_Z)));
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        list.shuffle_entity(observer_node, &[trigger], old_x, old_z);

        // The geometric truth for this tick's observer position.
        let expected: BTreeSet<u32> = grid
            .iter()
            .filter(|(_, (ex, ez))| contains((x, WALK_Z), (*ex, *ez), DEFAULT_AOI_RADIUS))
            .map(|(id, _)| *id)
            .collect();

        // The event order: this tick's events are exactly the set flips,
        // each kind matching its direction.
        let tick_events: Vec<(u32, bool)> = {
            let log = events.borrow();
            log.iter().skip(cursor).copied().collect()
        };
        cursor += tick_events.len();
        let mut flipped_in = expected
            .difference(&prev_expected)
            .copied()
            .collect::<BTreeSet<_>>();
        let mut flipped_out = prev_expected
            .difference(&expected)
            .copied()
            .collect::<BTreeSet<_>>();
        for (id, entering) in &tick_events {
            let pending = if *entering {
                flipped_in.remove(id)
            } else {
                flipped_out.remove(id)
            };
            assert!(
                pending,
                "tick {tick}: {id}'s {} matches a set flip",
                if *entering { "enter" } else { "leave" }
            );
        }
        assert!(
            flipped_in.is_empty() && flipped_out.is_empty(),
            "tick {tick}: every flip produced its event"
        );
        assert_eq!(
            tick_events.len(),
            expected.symmetric_difference(&prev_expected).count(),
            "tick {tick}: one event per flip"
        );
        out.flip_events += tick_events.len();

        for (id, entering) in &tick_events {
            if *entering {
                assert!(
                    enter_tick.insert(*id, tick).is_none(),
                    "entity {id} enters once"
                );
                let (px, pz) = grid[id];
                witness.upsert_entity(EntitySnapshot::new(*id, 1, (px, 0.0, pz)));
                witness.add_to_aoi(*id, false);
                witness.flush_state_changes();
                witness.request_entity_update(*id, &[]);
                witness.flush_state_changes();
            } else {
                assert!(
                    leave_tick.insert(*id, tick).is_none(),
                    "entity {id} leaves once"
                );
                witness.remove_from_aoi(*id, false);
                witness.flush_state_changes();
            }
        }
        out.enters = enter_tick.len();
        out.leaves = leave_tick.len();

        // The visible set: the witness holds exactly the geometric set,
        // aliases exist exactly inside it, snapshots sit on the grid.
        let actual: BTreeSet<u32> = witness.aoi_ids().into_iter().collect();
        assert_eq!(
            actual, expected,
            "tick {tick}: the witness holds the geometric set"
        );
        out.set_checks += 1;
        for (id, (ex, ez)) in &grid {
            let inside = expected.contains(id);
            let alias = witness.cache_flags(*id);
            assert_eq!(
                alias.is_some(),
                inside,
                "entity {id}'s alias matches its membership"
            );
            out.alias_checks += 1;
            if inside {
                let snap = witness.entity(*id).expect("an admitted entity registers");
                assert_eq!(
                    (snap.position.0, snap.position.2),
                    (*ex, *ez),
                    "entity {id}'s snapshot sits on its grid cell"
                );
                out.snapshot_checks += 1;
            }
        }
        prev_expected = expected;
    }
    out.ticks = tick;
    assert!(list.is_sorted(), "the chains stay sorted through the walk");

    // The event ticks are the containment transitions, reconstructed
    // from the geometry alone.
    let mut rec_enter: BTreeMap<u32, usize> = BTreeMap::new();
    let mut rec_leave: BTreeMap<u32, usize> = BTreeMap::new();
    for t in 1..=tick {
        let ox = WALK_START + STEP * t as f32;
        for (id, (ex, ez)) in &grid {
            let inside = contains((ox, WALK_Z), (*ex, *ez), DEFAULT_AOI_RADIUS);
            if inside {
                rec_enter.entry(*id).or_insert(t);
            } else if rec_enter.contains_key(id) {
                rec_leave.entry(*id).or_insert(t);
            }
        }
    }
    assert_eq!(
        enter_tick, rec_enter,
        "the enters fire on entering the range"
    );
    assert_eq!(
        leave_tick, rec_leave,
        "the leaves fire on leaving the range"
    );
    // Every enter precedes its leave, and (this walk's geometry) every
    // enter precedes every leave: the sweep admits the whole block
    // before it releases any of it.
    for id in enter_tick.keys() {
        assert!(
            enter_tick[id] < leave_tick[id],
            "entity {id}: enter then leave"
        );
    }
    let max_enter = enter_tick.values().copied().max().expect("enters recorded");
    let min_leave = leave_tick.values().copied().min().expect("leaves recorded");
    assert!(
        max_enter < min_leave,
        "the sweep admits the whole block before releasing any of it"
    );

    // End state: the observer passed the whole block — the AoI is empty,
    // every alias is back in the pool, and the ledger replays the
    // baseline's digest.
    let final_aoi = witness.aoi_ids().len();
    assert_eq!(final_aoi, 0, "the observer passed the whole block");
    for id in grid.keys() {
        assert_eq!(witness.cache_flags(*id), None, "entity {id} released");
    }
    assert!(witness.warnings().is_empty(), "the rerun warns nothing");
    out.warnings = witness.warnings().len();

    let mut text = String::new();
    for (i, (id, entering)) in events.borrow().iter().enumerate() {
        text.push_str(&format!("{i}\t{id}\t{entering}\n"));
    }
    out.ledger_digest = format!("{:016x}", fnv1a64(text.as_bytes()));
    assert_eq!(
        out.ledger_digest, BASELINE_D400_DIGEST,
        "the rerun replays the baseline's ledger"
    );
    out
}

// ---------------------------------------------------------------------------
// Leg 2: the hysteresis state, re-run on the flutter sweep
// ---------------------------------------------------------------------------

/// What the hysteresis rerun checked.
#[derive(Debug, Default)]
struct HystOutcome {
    ticks: usize,
    inner_enters: usize,
    inner_leaves: usize,
    outer_enters: usize,
    outer_leaves: usize,
    admitted_enters: usize,
    released: usize,
    suppressed: usize,
    state_checks: usize,
    final_admitted: usize,
}

/// Replays the C2 form: an inner trigger at the radius and an outer
/// trigger at radius + hyst drive the admission policy; per tick the
/// admitted set must sit inside the hysteresis band's two implications.
fn hysteresis_state_walk() -> HystOutcome {
    let mut list = RangeList::new();
    let observer = entity_at(OBSERVER_ID, WALK_START, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    let entities = block_entities();
    for (id, x, z) in &entities {
        list.add_entity_node(entity_at(*id, *x, 0.0, *z));
    }
    // The largest range leads: the outermost trigger's bound moves
    // first (`entity_add_trigger`'s ordering).
    let (outer, outer_events) =
        make_aoi_trigger(&mut list, observer_node, DEFAULT_AOI_RADIUS + HYST);
    let (inner, inner_events) = make_aoi_trigger(&mut list, observer_node, DEFAULT_AOI_RADIUS);
    let grid: BTreeMap<u32, (f32, f32)> =
        entities.iter().map(|(id, x, z)| (*id, (*x, *z))).collect();

    let mut admitted: BTreeSet<u32> = BTreeSet::new();
    let mut last_inner: BTreeMap<u32, bool> = BTreeMap::new();
    let mut out = HystOutcome::default();
    let mut cursor_i = 0usize;
    let mut cursor_o = 0usize;
    let mut x = WALK_START;
    let mut tick = 0usize;

    // One tick of movement plus the policy over the two logs.
    macro_rules! swing_tick {
        ($delta:expr) => {{
            let old_x = x;
            let old_z = WALK_Z;
            x += $delta;
            tick += 1;
            observer.borrow_mut().position = [x, 0.0, WALK_Z];
            list.shuffle_entity(observer_node, &[outer, inner], old_x, old_z);

            let inner_events_now: Vec<(u32, bool)> = {
                let log = inner_events.borrow();
                log.iter().skip(cursor_i).copied().collect()
            };
            cursor_i += inner_events_now.len();
            for (id, entering) in &inner_events_now {
                let prev = last_inner.insert(*id, *entering);
                if let Some(prev) = prev {
                    assert_ne!(prev, *entering, "entity {id}: the inner ledger alternates");
                } else {
                    assert!(
                        *entering,
                        "entity {id}: the inner ledger starts with an enter"
                    );
                }
                if *entering {
                    out.inner_enters += 1;
                    if admitted.insert(*id) {
                        out.admitted_enters += 1;
                    } else {
                        out.suppressed += 1;
                    }
                } else {
                    out.inner_leaves += 1;
                    if admitted.contains(id) {
                        // The policy suppresses the inner leave: the
                        // entity stays admitted inside the band.
                        out.suppressed += 1;
                    }
                }
            }
            let outer_events_now: Vec<(u32, bool)> = {
                let log = outer_events.borrow();
                log.iter().skip(cursor_o).copied().collect()
            };
            cursor_o += outer_events_now.len();
            for (id, entering) in &outer_events_now {
                if *entering {
                    out.outer_enters += 1;
                } else {
                    out.outer_leaves += 1;
                    if admitted.remove(id) {
                        out.released += 1;
                    }
                }
            }

            // The hysteresis band's two implications, per entity.
            for (id, (ex, ez)) in &grid {
                let inside_inner = contains((x, WALK_Z), (*ex, *ez), DEFAULT_AOI_RADIUS);
                let inside_outer = contains((x, WALK_Z), (*ex, *ez), DEFAULT_AOI_RADIUS + HYST);
                if inside_inner {
                    assert!(
                        admitted.contains(id),
                        "tick {tick}: entity {id} is inside the radius but unadmitted"
                    );
                }
                if admitted.contains(id) {
                    assert!(
                        inside_outer,
                        "tick {tick}: entity {id} is admitted but outside the band"
                    );
                }
                out.state_checks += 1;
            }
        }};
    }

    // The approach: the walk start to the swing's low end.
    for _ in 0..APPROACH_TICKS {
        swing_tick!(STEP);
    }
    assert!(
        (x - SWING_LOW).abs() < 1e-4,
        "the approach ends at the low swing"
    );
    // The periods: out and back, ending at the low end.
    for _ in 0..PERIODS {
        for _ in 0..LEG_TICKS {
            swing_tick!(SWING_STEP);
        }
        assert!(
            (x - SWING_HIGH).abs() < 1e-4,
            "the swing reaches the high end"
        );
        for _ in 0..LEG_TICKS {
            swing_tick!(-SWING_STEP);
        }
        assert!(
            (x - SWING_LOW).abs() < 1e-4,
            "the swing returns to the low end"
        );
    }
    out.ticks = tick;
    assert!(list.is_sorted(), "the chains stay sorted through the sweep");

    // End state: everything admitted sits in the release band and
    // nothing inside the radius is outside the set.
    let expected_final: BTreeSet<u32> = grid
        .iter()
        .filter(|(_, (ex, ez))| contains((x, WALK_Z), (*ex, *ez), DEFAULT_AOI_RADIUS + HYST))
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        admitted, expected_final,
        "the settled admitted set is the release band"
    );
    out.final_admitted = admitted.len();
    // Conservation: every admission ends released or still admitted.
    assert_eq!(
        out.admitted_enters,
        out.released + out.final_admitted,
        "the admission ledger conserves"
    );
    out
}

// ---------------------------------------------------------------------------
// Leg 3: the migration states, re-run stage by stage
// ---------------------------------------------------------------------------

/// The two cells and the spare address (the parallel-eval fixtures).
const APP_A: Addr = (1, 1);
const APP_B: Addr = (2, 2);
const APP_C: Addr = (3, 3);
/// The space id the fixtures share.
const SPACE: server_runtime::cellappmgr::SpaceId = 7;
/// The entity type every fixture registers.
const TYPE_ID: u16 = 4;
/// The boundary walk: 5 units per tick from mid-A; the boundary at 100.
const MIG_STEP: f32 = 5.0;
const MIG_START: f32 = 50.0;
const MIG_BOUNDARY: f32 = 100.0;

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

fn channels() -> BTreeMap<Addr, GhostChannel> {
    [APP_A, APP_B, APP_C]
        .into_iter()
        .map(|addr| (addr, GhostChannel::new(addr)))
        .collect()
}

fn config() -> GhostConfig {
    GhostConfig {
        ghost_distance: 30.0,
        ..GhostConfig::default()
    }
}

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

/// What the migration rerun recorded.
#[derive(Debug, Default)]
struct MigrationState {
    ghost_tick: usize,
    plan_tick: usize,
    applied_ghosts: usize,
    approach_state_checks: usize,
    offload_msgs_b: usize,
    onload_msgs_a: usize,
    onload_msgs_c: usize,
    final_reals: usize,
}

/// Replays the boundary migration with a per-tick message pump: B
/// applies each create as it arrives, and every stage's end state is
/// pinned before the next stage runs.
fn migration_state_walk() -> MigrationState {
    let cells = two_cells();
    let mut channels = channels();
    let mut host_a = host();
    let mut host_b = host();
    let cfg = config();
    let (real, view) = walking_real(10, MIG_START);
    let mut reals = vec![(real, view)];
    let mut controllers = Controllers::new();
    let mut entity_controllers = Controllers::new();
    let mut callbacks = EntityCallbackBuffer::new();
    let mut checker = OffloadChecker {
        should_offload: true,
        ..OffloadChecker::default()
    };

    let mut out = MigrationState::default();
    let mut x = MIG_START;
    let mut tick = 0usize;

    // The approach: per tick the checker runs, B pumps its channel, and
    // both ghost tables hold the honest state.
    let id = 10u32;
    loop {
        x += MIG_STEP;
        tick += 1;
        reals[0].1.position = Vector3::new(x, 0.0, 50.0);
        let plans = checker.run(
            &cells,
            &mut channels,
            &mut reals,
            &cfg,
            APP_A,
            SPACE,
            tick as u32,
        );
        for m in std::mem::take(&mut channels.get_mut(&APP_B).expect("B's channel").out) {
            match m {
                GhostMessage::CreateGhost(p) => {
                    assert_eq!(p.entity_id, id, "the interest ghost names the walker");
                    assert_eq!(
                        host_b.receive_create_ghost(APP_A, p, tick as u32),
                        GhostCreateOutcome::Created,
                        "B takes the interest ghost"
                    );
                    out.applied_ghosts += 1;
                    out.ghost_tick = tick;
                    assert_eq!(
                        host_b.ghosts[&id].real_addr, APP_A,
                        "the interest ghost points home"
                    );
                }
                other => panic!("unexpected message to B on tick {tick}: {other:?}"),
            }
        }
        // The per-tick states: A owns the one real; A holds no ghosts;
        // B holds the interest ghost from its ghost tick on; C is out.
        assert_eq!(reals.len(), 1, "tick {tick}: the walker is the one real");
        assert!(host_a.ghosts.is_empty(), "tick {tick}: A holds no ghosts");
        assert!(
            channels[&APP_C].out.is_empty(),
            "tick {tick}: the spare app stays silent"
        );
        let expected_b_ghosts = if out.ghost_tick > 0 { 1 } else { 0 };
        assert_eq!(
            host_b.ghosts.len(),
            expected_b_ghosts,
            "tick {tick}: B's ghost table holds the interest ghost from its tick on"
        );
        out.approach_state_checks += 1;

        if let Some(p) = plans.first() {
            assert_eq!(p.destination, APP_B, "the plan names the home cell");
            assert_eq!(p.entity_id, id, "the plan names the walker");
            break;
        }
        assert!(tick < 40, "the walker crosses the boundary");
    }
    out.plan_tick = tick;
    assert!(
        out.ghost_tick > 0 && out.ghost_tick < out.plan_tick,
        "the ghost leads the migration"
    );
    let crossing_tick = ((MIG_BOUNDARY - MIG_START) / MIG_STEP) as usize;
    assert_eq!(
        out.plan_tick, crossing_tick,
        "the plan fires on the crossing"
    );

    // The offload: the real streams and dies on A; A's ghost points at
    // B; nothing rides B's channel; the payload names the haunts.
    let (mut real, view) = reals.pop().expect("the walker stays in the run");
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
    out.offload_msgs_b = channels[&APP_B].out.len();
    assert_eq!(
        out.offload_msgs_b, 0,
        "the offload streams on the payload only"
    );
    assert!(reals.is_empty(), "A holds no real after the offload");
    assert_eq!(payload.entity_id, id);
    assert_eq!(payload.haunt_addrs, vec![APP_B, APP_A], "the haunt list");
    host_a.ghosts.insert(id, ghost_record);
    assert_eq!(
        host_a.ghosts[&id].real_addr, APP_B,
        "A's ghost points at the destination"
    );

    // The onload: B's ghost becomes the one real; the re-point rides to
    // A; the spare stays silent; B's ghost table empties.
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
    out.onload_msgs_a = channels[&APP_A].out.len();
    out.onload_msgs_c = channels[&APP_C].out.len();
    assert!(outcome.are_we_haunted, "B was in the haunt list");
    assert_eq!(outcome.real.id, id, "the real keeps its id");
    assert!(host_b.ghosts.is_empty(), "the ghost became the real");
    assert!(out.onload_msgs_a > 0, "the re-point rides to A");
    assert_eq!(out.onload_msgs_c, 0, "the spare app stays silent");

    // The settle: A relocates its ghost — one real in the world, at B.
    assert_eq!(
        host_a.receive_ghost_set_real(id, 1, APP_B),
        GhostSetRealOutcome::Relocated,
        "A relocates its ghost"
    );
    assert_eq!(
        host_a.ghosts[&id].real_addr, APP_B,
        "A's ghost settles on B"
    );
    assert!(
        host_b.ghosts.is_empty(),
        "B holds no ghosts after the settle"
    );
    out.final_reals = reals.len() + 1; // B's real, from the onload
    assert_eq!(out.final_reals, 1, "exactly one real remains");
    out
}

/// What the teleport rerun recorded.
#[derive(Debug, Default)]
struct TeleportState {
    del_addrs: usize,
    del_msgs_c: usize,
    loaded: usize,
    space_id: u32,
    haunted: usize,
    msgs_b_after: usize,
    final_ghosts_b: usize,
}

/// Replays the space-boundary teleport with the end states pinned.
fn teleport_state_walk() -> TeleportState {
    let mut channels = channels();
    let mut host_b = host();
    let (mut real, view) = walking_real(20, MIG_START);
    let mut controllers = Controllers::new();
    let mut entity_controllers = Controllers::new();
    let mut callbacks = EntityCallbackBuffer::new();
    real.haunts = vec![Haunt {
        addr: APP_C,
        creation_time: 100,
    }];

    assert!(host_b.ghosts.is_empty(), "the destination starts empty");
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
    let del_addrs = del_addrs.len();
    assert_eq!(del_addrs, 1, "the stale ghost deletes first");
    let del_msgs_c = channels[&APP_C].out.len();
    assert_eq!(
        channels[&APP_C].out,
        vec![GhostMessage::DelGhost { entity_id: 20 }],
        "the delGhost rides before the teleport payload"
    );
    assert!(
        host_b.ghosts.is_empty(),
        "the destination still holds no ghosts"
    );
    let _ = ghost_record;

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
    let TeleportLoad::Loaded(outcome) = loaded else {
        panic!("the nearby entity exists: the teleport loads");
    };
    assert_eq!(outcome.nearby_id, 21);
    assert_eq!(
        outcome.create.space_id, SPACE,
        "the destination rewrites the space"
    );
    let haunted = usize::from(outcome.onload.are_we_haunted);
    assert_eq!(haunted, 1, "the nearby haunt takes the real");
    let msgs_b_after = channels[&APP_B].out.len();
    let final_ghosts_b = host_b.ghosts.len();
    assert_eq!(final_ghosts_b, 0, "the real took the ghost's place");
    TeleportState {
        del_addrs,
        del_msgs_c,
        loaded: 1,
        space_id: outcome.create.space_id,
        haunted,
        msgs_b_after,
        final_ghosts_b,
    }
}

// ---------------------------------------------------------------------------
// The recheck run
// ---------------------------------------------------------------------------

/// Runs the three legs and leaves the recheck record.
#[test]
fn the_rerun_holds_the_visible_sets_and_the_event_order() {
    let started = Instant::now();
    let mut record = Record::default();
    let out = visible_set_and_order_walk();
    record.put("vis_ticks", out.ticks);
    record.put("vis_enters", out.enters);
    record.put("vis_leaves", out.leaves);
    record.put("vis_flip_events", out.flip_events);
    record.put("vis_set_checks", out.set_checks);
    record.put("vis_alias_checks", out.alias_checks);
    record.put("vis_snapshot_checks", out.snapshot_checks);
    record.put("vis_warnings", out.warnings);
    record.put("vis_final_aoi", out.final_aoi);
    record.put("vis_ledger_digest", out.ledger_digest.as_str());
    record.put(
        "recheck_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}

/// Runs the hysteresis leg and leaves its record.
#[test]
fn the_hysteresis_form_keeps_its_state_band() {
    let started = Instant::now();
    let mut record = Record::default();
    let out = hysteresis_state_walk();
    record.put("hyst_ticks", out.ticks);
    record.put("hyst_inner_enters", out.inner_enters);
    record.put("hyst_inner_leaves", out.inner_leaves);
    record.put("hyst_outer_enters", out.outer_enters);
    record.put("hyst_outer_leaves", out.outer_leaves);
    record.put("hyst_admitted_enters", out.admitted_enters);
    record.put("hyst_released", out.released);
    record.put("hyst_suppressed", out.suppressed);
    record.put("hyst_state_checks", out.state_checks);
    record.put("hyst_final_admitted", out.final_admitted);
    record.put(
        "recheck_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}

/// Runs the migration legs and leaves their record.
#[test]
fn the_migration_stages_end_in_the_reference_state() {
    let started = Instant::now();
    let mut record = Record::default();
    let mig = migration_state_walk();
    record.put("mig_ghost_tick", mig.ghost_tick);
    record.put("mig_plan_tick", mig.plan_tick);
    record.put("mig_applied_ghosts", mig.applied_ghosts);
    record.put("mig_approach_state_checks", mig.approach_state_checks);
    record.put("mig_offload_msgs_b", mig.offload_msgs_b);
    record.put("mig_onload_msgs_a", mig.onload_msgs_a);
    record.put("mig_onload_msgs_c", mig.onload_msgs_c);
    record.put("mig_final_reals", mig.final_reals);

    let tp = teleport_state_walk();
    record.put("tspace_del_addrs", tp.del_addrs);
    record.put("tspace_del_msgs_c", tp.del_msgs_c);
    record.put("tspace_loaded", tp.loaded);
    record.put("tspace_space_id", tp.space_id);
    record.put("tspace_haunted", tp.haunted);
    record.put("tspace_msgs_b_after", tp.msgs_b_after);
    record.put("tspace_final_ghosts_b", tp.final_ghosts_b);
    record.put(
        "recheck_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
