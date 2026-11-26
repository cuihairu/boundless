//! OPT-08-01-02: the AOI index and Witness scheduling candidates.
//!
//! The acceptance reports **candidate gains** (`报告候选收益`). The
//! OPT-08-01-01 baseline fixed the dense-AoI cost; this harness evaluates
//! three optimization candidates against it, each emulated on the live
//! faces without touching production behavior:
//!
//! - **C1 — move-threshold shuffle elision (index).** The trigger only
//!   detects a crossing when its node shuffles, so shuffling every `k`
//!   ticks delays detection by at most `k` ticks while cutting the
//!   shuffle calls `k`-fold. Measured: calls, nanoseconds saved, and
//!   the per-entity detection lag the delay costs.
//! - **C2 — the AoI exit hysteresis (index + witness).** The reference
//!   enters at the AoI radius and leaves at radius + hysteresis
//!   (`Witness::setAoIRadius`'s `hyst`). The port keeps `aoi_hyst` as
//!   bookkeeping; the candidate policy is emulated with two live
//!   triggers — enter from the radius trigger, leave from the
//!   radius+hyst trigger. Measured: the boundary-flutter churn it
//!   suppresses (an observer oscillating at the boundary re-admits
//!   entities every crossing under hyst 0), and the leave-detection
//!   delay it costs on a clean monotonic walk.
//! - **C3 — skip the no-change update frames (witness).** `update()`
//!   pushes an `EntityUpdate` for every popped queue entry even when
//!   the volatile gate produced no position, no direction and no
//!   events — a stationary population spends its budget on id-only
//!   frames. Measured: how many of the baseline's update frames and
//!   budget bytes such a skip would reclaim, and the player-position
//!   floor that stays.
//!
//! The record sink writes `metric<TAB>value` rows to stdout and, when
//! `AOI_CANDIDATES_OUT` is set, to that path:
//!
//! ```sh
//! AOI_CANDIDATES_OUT=<abs path> cargo test -p server-runtime \
//!     --test aoi_candidates -- --nocapture
//! ```

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use server_runtime::cellapp_range_list::{
    FLAG_LOWER_AOI_TRIGGER, FLAG_NO_TRIGGERS, FLAG_UPPER_AOI_TRIGGER, RangeList, TriggerTarget,
};
use server_runtime::cellapp_space::Entity;
use server_runtime::witness::{
    ClientMessage, DEFAULT_AOI_RADIUS, DirProjection, PosProjection, VolatileInfo, Witness,
    bps_to_packet_size,
};

// ---------------------------------------------------------------------------
// The record sink (the baseline harness shape)
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows, printed and written to
/// `$AOI_CANDIDATES_OUT` when set.
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
        if let Ok(path) = std::env::var("AOI_CANDIDATES_OUT") {
            std::fs::write(&path, &text).expect("write AOI_CANDIDATES_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The fixtures (the baseline harness shapes)
// ---------------------------------------------------------------------------

/// The dense block's side and the walk, exactly as OPT-08-01-01 drove it.
const SIDE: usize = 20;
const BLOCK: f32 = 200.0;
const STEP: f32 = 8.0;
const WALK_START: f32 = -600.0;
const WALK_END: f32 = 800.0;
const WALK_Z: f32 = BLOCK / 2.0 - 5.0;
/// The AoI radius (`DEFAULT_AOI_RADIUS`) and the candidate hysteresis.
const RADIUS: f32 = DEFAULT_AOI_RADIUS;
const HYST: f32 = 50.0;
/// The flutter window: the observer oscillates ±25 around x = −520, so
/// the nearest entities sit at distance 495…545 — inside `RADIUS` at the
/// swing's near end, outside it at the far end, never beyond
/// `RADIUS + HYST`.
const FLUTTER_CENTER: f32 = -520.0;
const FLUTTER_AMPLITUDE: f32 = 25.0;
const FLUTTER_PERIOD_TICKS: usize = 10;
const FLUTTER_PERIODS: usize = 10;

/// The witness scheduler's packet budget, as the baseline set it.
const BUDGET_BPS: i32 = 50_000;
const UPDATE_HERTZ: i32 = 10;

/// One boundary event, attributed to the tick it fired in.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TickEvent {
    id: u32,
    entering: bool,
    tick: usize,
}

/// An entity at `(x, y, z)`, as the range list wants it.
fn entity_at(id: u32, x: f32, y: f32, z: f32) -> Rc<RefCell<Entity>> {
    let mut entity = Entity::new(id, true);
    entity.position = [x, y, z];
    Rc::new(RefCell::new(entity))
}

/// The block's entities: `(id, x, z)` with ids from 2, row-major.
fn block_entities() -> Vec<(u32, f32, f32)> {
    let spacing = BLOCK / SIDE as f32;
    let mut entities = Vec::with_capacity(SIDE * SIDE);
    let mut id = 2;
    for row in 0..SIDE {
        for col in 0..SIDE {
            entities.push((id, col as f32 * spacing, row as f32 * spacing));
            id += 1;
        }
    }
    entities
}

/// The observer's AoI trigger (`witness.cpp:73` `AoITrigger`): the
/// bounds want AoI crossings and make none of their own.
type EventLog = Rc<RefCell<Vec<(u32, bool)>>>;

fn make_aoi_trigger(list: &mut RangeList, central: usize, range: f32) -> (u32, EventLog) {
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

/// Drains `events` past `cursor` into tick-attributed records.
fn drain_tick(events: &EventLog, cursor: &mut usize, tick: usize, into: &mut Vec<TickEvent>) {
    for &(id, entering) in events.borrow().iter().skip(*cursor) {
        into.push(TickEvent { id, entering, tick });
        *cursor += 1;
    }
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

/// Groups a walk's events per entity, in fire order.
fn per_entity(events: &[TickEvent]) -> BTreeMap<u32, Vec<(usize, bool)>> {
    let mut map: BTreeMap<u32, Vec<(usize, bool)>> = BTreeMap::new();
    for e in events {
        map.entry(e.id).or_default().push((e.tick, e.entering));
    }
    map
}

// ---------------------------------------------------------------------------
// C1 — the move-threshold shuffle elision
// ---------------------------------------------------------------------------

/// What one strided walk recorded.
#[derive(Debug)]
struct ElisionOutcome {
    ticks: usize,
    shuffle_calls: usize,
    shuffle_ns_total: f64,
    events_total: usize,
    max_lag_ticks: usize,
    ledger_digest: u64,
}

/// Walks the observer across the block, shuffling every `stride` ticks
/// instead of every tick. Returns the walk's attributed events with the
/// shuffle cost.
fn strided_walk(
    stride: usize,
    baseline: Option<&BTreeMap<u32, Vec<(usize, bool)>>>,
) -> (ElisionOutcome, Vec<TickEvent>) {
    let mut list = RangeList::new();
    let observer = entity_at(1, WALK_START, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    for (id, x, z) in block_entities() {
        list.add_entity_node(entity_at(id, x, 0.0, z));
    }
    let (trigger, events) = make_aoi_trigger(&mut list, observer_node, RADIUS);

    let mut cursor = 0usize;
    let mut tick = 0usize;
    let mut shuffle_calls = 0usize;
    let mut shuffle_ns_total = 0.0;
    let mut attributed: Vec<TickEvent> = Vec::new();
    let mut x = WALK_START;
    let mut last_shuffle_x = x;
    let walk_ticks = 175 + stride; // the lagged leaves still fire
    while tick < walk_ticks {
        x += STEP;
        tick += 1;
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        if tick.is_multiple_of(stride) || x >= WALK_END {
            let t0 = Instant::now();
            list.shuffle_entity(observer_node, &[trigger], last_shuffle_x, WALK_Z);
            shuffle_ns_total += t0.elapsed().as_nanos() as f64;
            shuffle_calls += 1;
            last_shuffle_x = x;
        }
        drain_tick(&events, &mut cursor, tick, &mut attributed);
    }
    assert!(
        list.is_sorted(),
        "stride {stride}: the chains stay sorted through the walk"
    );

    let entities = block_entities();
    let events_total = attributed.len();
    assert_eq!(
        events_total,
        2 * entities.len(),
        "stride {stride}: every entity enters and leaves exactly once"
    );

    let mut max_lag = 0usize;
    if let Some(base) = baseline {
        let mine = per_entity(&attributed);
        for (id, base_events) in base {
            let my_events = &mine[id];
            assert_eq!(
                my_events.len(),
                base_events.len(),
                "stride {stride}: entity {id} fires the same event count"
            );
            for ((my_tick, my_entering), (base_tick, base_entering)) in
                my_events.iter().zip(base_events)
            {
                assert_eq!(
                    my_entering, base_entering,
                    "stride {stride}: entity {id} fires the same event kinds in order"
                );
                let lag = my_tick.saturating_sub(*base_tick);
                assert!(
                    lag <= stride,
                    "stride {stride}: entity {id}'s lag {lag} stays within the stride"
                );
                max_lag = max_lag.max(lag);
            }
        }
    }

    let mut text = String::new();
    for (i, e) in attributed.iter().enumerate() {
        text.push_str(&format!("{}\t{}\t{}\n", i, e.id, e.entering));
    }

    let outcome = ElisionOutcome {
        ticks: tick,
        shuffle_calls,
        shuffle_ns_total,
        events_total,
        max_lag_ticks: max_lag,
        ledger_digest: fnv1a64(text.as_bytes()),
    };
    (outcome, attributed)
}

// ---------------------------------------------------------------------------
// C2 — the AoI exit hysteresis
// ---------------------------------------------------------------------------

/// What one hysteresis walk recorded: the plain radius ledger, the
/// candidate policy's ledger, and the per-entity leave lag.
#[derive(Debug)]
struct HysteresisOutcome {
    base_events: usize,
    base_enters: usize,
    base_leaves: usize,
    hyst_events: usize,
    hyst_enters: usize,
    hyst_leaves: usize,
    leave_lag_ticks_max: usize,
    leave_lag_ticks_min: usize,
    still_admitted: usize,
    flutter_entities: usize,
    ledger_digest: u64,
}

/// Runs `walk` with two live triggers — enter from the radius trigger,
/// leave from the radius+hyst trigger — and compares the plain
/// single-trigger ledger (hyst 0) against the candidate policy.
fn hysteresis_compare(
    inner_events: Vec<TickEvent>,
    outer_events: Vec<TickEvent>,
    flutter_window: Option<(usize, usize)>,
    label: &str,
) -> HysteresisOutcome {
    // The plain form (hyst 0): the radius trigger alone.
    let base_enters = inner_events.iter().filter(|e| e.entering).count();
    let base_leaves = inner_events.iter().filter(|e| !e.entering).count();

    // The candidate policy: inner enters admit; inner leaves are
    // suppressed; outer leaves (and only those) release. Inner and
    // outer crossings sit 50 units apart, so they never share a tick.
    let mut merged: Vec<(usize, bool, bool, u32)> = Vec::new(); // (tick, entering, is_inner, id)
    for e in &inner_events {
        merged.push((e.tick, e.entering, true, e.id));
    }
    for e in &outer_events {
        merged.push((e.tick, e.entering, false, e.id));
    }
    merged.sort_by_key(|&(tick, _, is_inner, id)| (tick, !is_inner, id));

    let mut admitted: BTreeMap<u32, bool> = BTreeMap::new();
    let mut hyst: Vec<TickEvent> = Vec::new();
    for &(tick, entering, is_inner, id) in &merged {
        let adm = admitted.entry(id).or_insert(false);
        if is_inner {
            if entering && !*adm {
                *adm = true;
                hyst.push(TickEvent {
                    id,
                    entering: true,
                    tick,
                });
            }
        } else if !entering && *adm {
            *adm = false;
            hyst.push(TickEvent {
                id,
                entering: false,
                tick,
            });
        }
    }

    let hyst_enters = hyst.iter().filter(|e| e.entering).count();
    let hyst_leaves = hyst.iter().filter(|e| !e.entering).count();
    let still_admitted = admitted.values().filter(|a| **a).count();

    // The leave-detection cost: per entity, the policy's leave fires
    // one hysteresis band later than the plain form's. On a clean
    // monotonic pass the totals must match event for event; under
    // flutter the suppressed events ARE the candidate's gain, so only
    // the kind order is pinned.
    let base_map = per_entity(&inner_events);
    let hyst_map = per_entity(&hyst);
    let expect_equal = flutter_window.is_none();
    let mut lag_max = 0usize;
    let mut lag_min = usize::MAX;
    let mut enter_lags_zero = true;
    for (id, base_events) in &base_map {
        let hyst_events = hyst_map
            .get(id)
            .unwrap_or_else(|| panic!("{label}: entity {id} fires no policy events"));
        if expect_equal {
            assert_eq!(
                hyst_events.len(),
                base_events.len(),
                "{label}: entity {id} still fires enter and leave exactly once"
            );
        } else {
            assert!(
                hyst_events.len() <= base_events.len(),
                "{label}: entity {id} only loses events under the policy"
            );
        }
        for ((my_tick, my_entering), (base_tick, base_entering)) in
            hyst_events.iter().zip(base_events)
        {
            assert_eq!(my_entering, base_entering, "{label}: same kind, in order");
            if *my_entering {
                enter_lags_zero &= my_tick == base_tick;
            } else {
                let lag = my_tick.saturating_sub(*base_tick);
                lag_max = lag_max.max(lag);
                lag_min = lag_min.min(lag);
            }
        }
    }
    assert!(enter_lags_zero, "{label}: the policy never delays an enter");

    // The flutter churn set: entities the plain form re-crossed during
    // the oscillation window.
    let flutter_entities = match flutter_window {
        Some((from, to)) => {
            let mut churn: Vec<u32> = Vec::new();
            for (id, events) in &base_map {
                if events.iter().any(|&(tick, _)| tick > from && tick <= to) && events.len() > 2 {
                    churn.push(*id);
                }
            }
            churn.len()
        }
        None => 0,
    };

    let mut text = String::new();
    for (i, e) in hyst.iter().enumerate() {
        text.push_str(&format!("{}\t{}\t{}\n", i, e.id, e.entering));
    }

    let outcome = HysteresisOutcome {
        base_events: inner_events.len(),
        base_enters,
        base_leaves,
        hyst_events: hyst.len(),
        hyst_enters,
        hyst_leaves,
        leave_lag_ticks_max: lag_max,
        leave_lag_ticks_min: lag_min,
        still_admitted,
        flutter_entities,
        ledger_digest: fnv1a64(text.as_bytes()),
    };
    assert_eq!(
        outcome.still_admitted, 0,
        "{label}: every entity is released by the walk's end"
    );
    if expect_equal {
        assert_eq!(
            outcome.hyst_events, outcome.base_events,
            "{label}: the clean pass fires the same events under both forms"
        );
    } else {
        assert_eq!(
            outcome.hyst_enters, outcome.hyst_leaves,
            "{label}: every suppressed churn admit still releases"
        );
    }
    outcome
}

/// The monotonic walk with both triggers — the clean-pass cost face.
fn mono_walk() -> (Vec<TickEvent>, Vec<TickEvent>) {
    let mut list = RangeList::new();
    let observer = entity_at(1, WALK_START, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    for (id, x, z) in block_entities() {
        list.add_entity_node(entity_at(id, x, 0.0, z));
    }
    let (inner, inner_log) = make_aoi_trigger(&mut list, observer_node, RADIUS);
    let (outer, outer_log) = make_aoi_trigger(&mut list, observer_node, RADIUS + HYST);

    let mut inner_attributed = Vec::new();
    let mut outer_attributed = Vec::new();
    let (mut inner_cursor, mut outer_cursor) = (0usize, 0usize);
    let mut x = WALK_START;
    let mut tick = 0usize;
    while x < WALK_END {
        let old_x = x;
        let old_z = WALK_Z;
        x += STEP;
        tick += 1;
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        list.shuffle_entity(observer_node, &[inner, outer], old_x, old_z);
        drain_tick(&inner_log, &mut inner_cursor, tick, &mut inner_attributed);
        drain_tick(&outer_log, &mut outer_cursor, tick, &mut outer_attributed);
    }
    assert!(list.is_sorted(), "the mono walk keeps the chains sorted");
    (inner_attributed, outer_attributed)
}

/// The flutter walk: approach, oscillate at the boundary, then pass.
fn flutter_walk() -> (Vec<TickEvent>, Vec<TickEvent>, usize, usize) {
    let mut list = RangeList::new();
    let observer = entity_at(1, FLUTTER_CENTER - FLUTTER_AMPLITUDE - 200.0, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    for (id, x, z) in block_entities() {
        list.add_entity_node(entity_at(id, x, 0.0, z));
    }
    let (inner, inner_log) = make_aoi_trigger(&mut list, observer_node, RADIUS);
    let (outer, outer_log) = make_aoi_trigger(&mut list, observer_node, RADIUS + HYST);

    let mut inner_attributed = Vec::new();
    let mut outer_attributed = Vec::new();
    let (mut inner_cursor, mut outer_cursor) = (0usize, 0usize);
    let mut tick = 0usize;

    // Approach to the oscillation's far end — nothing crosses yet.
    let mut x = FLUTTER_CENTER - FLUTTER_AMPLITUDE - 200.0;
    let approach_end = FLUTTER_CENTER - FLUTTER_AMPLITUDE;
    while x < approach_end {
        x += STEP;
        tick += 1;
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        list.shuffle_entity(observer_node, &[inner, outer], x - STEP, WALK_Z);
        drain_tick(&inner_log, &mut inner_cursor, tick, &mut inner_attributed);
        drain_tick(&outer_log, &mut outer_cursor, tick, &mut outer_attributed);
    }

    // The oscillation: a triangle wave — each period sweeps the full
    // FLUTTER_CENTER ± FLUTTER_AMPLITUDE span one way, and the next
    // period sweeps it back.
    let flutter_from = tick;
    for period in 0..FLUTTER_PERIODS {
        let step = 2.0 * FLUTTER_AMPLITUDE / FLUTTER_PERIOD_TICKS as f32;
        let delta = if period % 2 == 0 { step } else { -step };
        for _phase in 0..FLUTTER_PERIOD_TICKS {
            let old = x;
            x += delta;
            tick += 1;
            observer.borrow_mut().position = [x, 0.0, WALK_Z];
            list.shuffle_entity(observer_node, &[inner, outer], old, WALK_Z);
            drain_tick(&inner_log, &mut inner_cursor, tick, &mut inner_attributed);
            drain_tick(&outer_log, &mut outer_cursor, tick, &mut outer_attributed);
        }
    }
    let flutter_to = tick;

    // The pass: walk through and past the block; every entity releases.
    while x < WALK_END {
        let old_x = x;
        x += STEP;
        tick += 1;
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        list.shuffle_entity(observer_node, &[inner, outer], old_x, WALK_Z);
        drain_tick(&inner_log, &mut inner_cursor, tick, &mut inner_attributed);
        drain_tick(&outer_log, &mut outer_cursor, tick, &mut outer_attributed);
    }
    assert!(list.is_sorted(), "the flutter walk keeps the chains sorted");
    (inner_attributed, outer_attributed, flutter_from, flutter_to)
}

// ---------------------------------------------------------------------------
// C3 — the no-change update frames
// ---------------------------------------------------------------------------

/// What the frame-skip candidate would reclaim from one witness walk.
#[derive(Debug)]
struct FrameSkipOutcome {
    update_msgs_total: usize,
    update_bytes_total: usize,
    empty_msgs: usize,
    empty_bytes: usize,
    player_msgs: usize,
    empty_bytes_max_tick: usize,
    aoi_final: usize,
    warnings: usize,
    ledger_digest: u64,
}

/// Replays the baseline's scheduler walk, classifying each post-update
/// bundle: the id-only frames the candidate would drop, and the
/// player-position floor that stays.
fn frame_skip_walk() -> FrameSkipOutcome {
    let mut list = RangeList::new();
    let observer = entity_at(1, WALK_START, 0.0, WALK_Z);
    let observer_node = list.add_entity_node(Rc::clone(&observer));
    let entities = block_entities();
    for (id, x, z) in &entities {
        list.add_entity_node(entity_at(*id, *x, 0.0, *z));
    }
    let (trigger, events) = make_aoi_trigger(&mut list, observer_node, RADIUS);

    let mut self_entity =
        server_runtime::witness::EntitySnapshot::new(1, 1, (WALK_START, 0.0, WALK_Z));
    self_entity.volatile = VolatileInfo::NONE;
    let mut witness = Witness::new(1, self_entity);
    witness.set_aoi_radius(RADIUS, 0.0);
    witness.set_witness_capacity(BUDGET_BPS, UPDATE_HERTZ);
    let budget = bps_to_packet_size(BUDGET_BPS, UPDATE_HERTZ);
    let watchees: BTreeMap<u32, (f32, f32)> =
        entities.iter().map(|(id, x, z)| (*id, (*x, *z))).collect();

    let mut out = FrameSkipOutcome {
        update_msgs_total: 0,
        update_bytes_total: 0,
        empty_msgs: 0,
        empty_bytes: 0,
        player_msgs: 0,
        empty_bytes_max_tick: 0,
        aoi_final: 0,
        warnings: 0,
        ledger_digest: 0,
    };
    let mut admitted = 0usize;
    let mut leaves = 0usize;
    let mut cursor = 0usize;
    let mut x = WALK_START;
    let mut ledger = String::new();
    let mut tick = 0usize;
    while x < WALK_END {
        let old_x = x;
        let old_z = WALK_Z;
        x += STEP;
        tick += 1;

        witness.upsert_entity(server_runtime::witness::EntitySnapshot::new(
            1,
            1,
            (x, 0.0, WALK_Z),
        ));
        observer.borrow_mut().position = [x, 0.0, WALK_Z];
        list.shuffle_entity(observer_node, &[trigger], old_x, old_z);

        let mut tick_events: Vec<(u32, bool)> = Vec::new();
        for &(id, entering) in events.borrow().iter().skip(cursor) {
            tick_events.push((id, entering));
            cursor += 1;
        }
        for (id, entering) in &tick_events {
            if *entering {
                let (px, pz) = watchees[id];
                witness.upsert_entity(server_runtime::witness::EntitySnapshot::new(
                    *id,
                    1,
                    (px, 0.0, pz),
                ));
                witness.add_to_aoi(*id, false);
                witness.flush_state_changes();
                witness.request_entity_update(*id, &[]);
                witness.flush_state_changes();
                admitted += 1;
            } else {
                witness.remove_from_aoi(*id, false);
                witness.flush_state_changes();
                leaves += 1;
            }
        }
        let _ = witness.take_bundle();

        // The scheduled update pass; classify what it spent.
        witness.update();
        let bundle = witness.take_bundle();
        let mut tick_empty_bytes = 0usize;
        for m in &bundle {
            match m {
                ClientMessage::EntityUpdate {
                    pos: PosProjection::None,
                    dir: DirProjection::None,
                    events,
                    ..
                } if events.is_empty() => {
                    out.empty_msgs += 1;
                    out.empty_bytes += m.wire_size();
                    tick_empty_bytes += m.wire_size();
                }
                ClientMessage::AvatarUpdatePlayerDetailed { .. }
                | ClientMessage::RelativePosition { .. }
                | ClientMessage::RelativePositionReference { .. } => out.player_msgs += 1,
                _ => {}
            }
        }
        out.update_msgs_total += bundle.len();
        let update_bytes = bundle.iter().map(|m| m.wire_size()).sum::<usize>();
        out.update_bytes_total += update_bytes;
        out.empty_bytes_max_tick = out.empty_bytes_max_tick.max(tick_empty_bytes);
        // The overshoot contract from the baseline: the pass checks the
        // budget before each send, so the tick total stays within the
        // budget plus its largest message.
        let max_msg = bundle.iter().map(|m| m.wire_size()).max().unwrap_or(0);
        assert!(
            update_bytes <= budget as usize + max_msg,
            "tick {tick}: the update pass stays within budget plus one send"
        );

        for (id, entering) in &tick_events {
            ledger.push_str(&format!("{tick}\t{id}\t{entering}\n"));
        }
    }

    assert_eq!(admitted, entities.len(), "every entity is admitted once");
    assert_eq!(leaves, entities.len(), "every entity leaves once");
    assert!(
        witness.aoi_ids().is_empty(),
        "the observer passed the whole block"
    );
    out.aoi_final = witness.aoi_ids().len();
    out.warnings = witness.warnings().len();
    assert_eq!(out.warnings, 0, "the walk raises no warnings");
    out.ledger_digest = fnv1a64(ledger.as_bytes());
    out
}

// ---------------------------------------------------------------------------
// The evaluation run
// ---------------------------------------------------------------------------

/// Runs the three candidate evaluations and leaves the record.
#[test]
fn the_aoi_candidates_report_the_gains() {
    let started = Instant::now();
    let mut record = Record::default();
    let entities = SIDE * SIDE;

    // C1 — the shuffle elision ladder. The stride-1 walk is the
    // baseline; its ledger digest pins it to the OPT-08-01-01 event
    // stream.
    let (base_out, base_events) = strided_walk(1, None);
    record.put("c1_k1_shuffle_calls", base_out.shuffle_calls);
    record.put(
        "c1_k1_shuffle_ns_total",
        format!("{:.0}", base_out.shuffle_ns_total),
    );
    record.put("c1_k1_events_total", base_out.events_total);
    record.put("c1_k1_ticks", base_out.ticks);
    record.put(
        "c1_k1_ledger_digest",
        format!("{:016x}", base_out.ledger_digest),
    );
    let baseline = per_entity(&base_events);
    for stride in [2usize, 4usize] {
        let (out, _) = strided_walk(stride, Some(&baseline));
        record.put(format!("c1_k{stride}_shuffle_calls"), out.shuffle_calls);
        record.put(
            format!("c1_k{stride}_shuffle_ns_total"),
            format!("{:.0}", out.shuffle_ns_total),
        );
        record.put(
            format!("c1_k{stride}_shuffle_ns_saved"),
            format!("{:.0}", base_out.shuffle_ns_total - out.shuffle_ns_total),
        );
        record.put(format!("c1_k{stride}_events_total"), out.events_total);
        record.put(format!("c1_k{stride}_max_lag_ticks"), out.max_lag_ticks);
    }

    // C2 — the exit hysteresis: the clean pass and the flutter.
    let (mono_inner, mono_outer) = mono_walk();
    let mono = hysteresis_compare(mono_inner, mono_outer, None, "mono");
    record.put("c2_mono_base_events", mono.base_events);
    record.put("c2_mono_base_enters", mono.base_enters);
    record.put("c2_mono_base_leaves", mono.base_leaves);
    record.put("c2_mono_hyst_events", mono.hyst_events);
    record.put("c2_mono_hyst_enters", mono.hyst_enters);
    record.put("c2_mono_hyst_leaves", mono.hyst_leaves);
    record.put("c2_mono_leave_lag_ticks_min", mono.leave_lag_ticks_min);
    record.put("c2_mono_leave_lag_ticks_max", mono.leave_lag_ticks_max);
    record.put(
        "c2_mono_ledger_digest",
        format!("{:016x}", mono.ledger_digest),
    );

    let (flutter_inner, flutter_outer, flutter_from, flutter_to) = flutter_walk();
    let flutter = hysteresis_compare(
        flutter_inner,
        flutter_outer,
        Some((flutter_from, flutter_to)),
        "flutter",
    );
    let suppressed = flutter.base_events - flutter.hyst_events;
    let suppressed_enters = flutter.base_enters - flutter.hyst_enters;
    let suppressed_leaves = flutter.base_leaves - flutter.hyst_leaves;
    // The admission handshake the suppression saves: enterAoI +
    // createEntity per re-admit, leaveAoI per leave (the baseline's
    // 24 bytes per entity face).
    let msgs_saved = suppressed_enters * 2 + suppressed_leaves;
    record.put("c2_flutter_base_events", flutter.base_events);
    record.put("c2_flutter_base_enters", flutter.base_enters);
    record.put("c2_flutter_base_leaves", flutter.base_leaves);
    record.put("c2_flutter_hyst_events", flutter.hyst_events);
    record.put("c2_flutter_churn_entities", flutter.flutter_entities);
    record.put("c2_flutter_suppressed_events", suppressed);
    record.put("c2_flutter_msgs_saved", msgs_saved);
    record.put("c2_flutter_bytes_saved", msgs_saved * 8);
    assert!(
        suppressed > 0,
        "the hysteresis suppresses the flutter churn"
    );
    assert!(
        mono.leave_lag_ticks_min >= (HYST / STEP) as usize - 1
            && mono.leave_lag_ticks_max <= (HYST / STEP) as usize + 2,
        "the leave lag tracks the hysteresis band"
    );
    assert_eq!(
        mono.base_events, mono.hyst_events,
        "the clean pass fires the same events under both forms"
    );

    // C3 — the no-change update frames.
    let frames = frame_skip_walk();
    record.put("c3_update_msgs_total", frames.update_msgs_total);
    record.put("c3_update_bytes_total", frames.update_bytes_total);
    record.put("c3_empty_msgs", frames.empty_msgs);
    record.put("c3_empty_bytes", frames.empty_bytes);
    record.put(
        "c3_empty_share_msgs",
        format!(
            "{:.4}",
            frames.empty_msgs as f64 / frames.update_msgs_total as f64
        ),
    );
    record.put(
        "c3_empty_share_bytes",
        format!(
            "{:.4}",
            frames.empty_bytes as f64 / frames.update_bytes_total as f64
        ),
    );
    record.put("c3_player_msgs", frames.player_msgs);
    record.put("c3_empty_bytes_max_tick", frames.empty_bytes_max_tick);
    record.put(
        "c3_budget_bytes",
        bps_to_packet_size(BUDGET_BPS, UPDATE_HERTZ),
    );
    record.put("c3_ledger_digest", format!("{:016x}", frames.ledger_digest));
    assert!(
        frames.empty_msgs as f64 > 0.5 * frames.update_msgs_total as f64,
        "the stationary population spends most update frames on id-only sends"
    );
    assert!(frames.player_msgs > 0, "the player floor stays");

    record.put("candidates_entities", entities);
    record.put(
        "candidates_wall_ms",
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    record.flush();
}
