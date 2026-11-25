//! OPT-08-01-01: the dense-AoI baseline.
//!
//! One viewer stands in the middle of a 10×12 grid of 120 entities at
//! 4 m spacing — everything inside one AoI — and the whole shipped
//! stack runs per tick: the cross-range list shuffles every moved
//! entity (`RangeListWorld::move_entity`), the AoI trigger events feed
//! the witness (`add_to_aoi`), the vision trigger feeds the seeing
//! pass (`VisionWorld::vision_update`), the seeing starts drive the
//! witness entries, and the budget scheduler sends what it can
//! (`Witness::update` at `setWitnessCapacity(64000, 10)` — 772 payload
//! bytes per tick, which binds at this density).
//!
//! The acceptance saves **tick, broadcast volume and migration
//! duration**. The ledger counts are structural and asserted: the
//! per-tick message and byte volumes, the creation burst, the seeing
//! churn, and the migration leg — a mid-stream handover seeds the
//! visible set on the destination (`vision_restore` + `vision_start_real`)
//! and converges to the set a viewer that had always stood at the new
//! position sees, re-announcing only genuinely new ids. Wall-clock of
//! the seeing and scheduling passes is probe data, printed for the
//! review doc, never asserted.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::range_list::{RangeListEvent, TriggerRef};
use crate::vision::{
    EntityVision, TargetFacts, TrapCallback, ViewerFacts, VisionController, VisionWorld, add_vision,
};
use crate::witness::{ClientMessage, DEFAULT_AOI_RADIUS, EntitySnapshot, VolatileInfo, Witness};

/// The ledger seed — every movement draw comes from it.
pub const AOI_SEED: u64 = 1_618_033;

/// The viewer's entity id; the observed field is 2..=DENSE_LAST.
pub const VIEWER: u32 = 1;
/// The dense field: 120 entities, ids 2..=121.
pub const DENSE_ENTITIES: usize = 120;
pub const DENSE_LAST: u32 = (DENSE_ENTITIES + 1) as u32;
/// Grid shape and spacing — a 36 m × 44 m field.
pub const GRID_COLS: usize = 10;
pub const GRID_ROWS: usize = 12;
pub const SPACING: f32 = 4.0;
/// Ticks the baseline runs before the migration handover.
pub const BASELINE_TICKS: usize = 48;
/// Ticks the migrated destination runs after the handover.
pub const NEW_TICKS: usize = 16;
/// Creation settles inside this many ticks; the tail is pure churn.
pub const STEADY_FROM: usize = 8;
/// `setWitnessCapacity(64000, 10)` — the dense case binds this budget.
pub const CAPACITY_BITS_PER_SECOND: i32 = 64_000;
pub const CAPACITY_HERTZ: i32 = 10;
/// The seeing pass: a wide-but-finite cone, 28 m range, seen every tick.
pub const VISION_ANGLE: f32 = 1.2;
pub const VISION_RANGE: f32 = 28.0;
pub const SEEING_HEIGHT: f32 = 1.7;
pub const TARGET_HEIGHT: f32 = 1.8;
/// Per-tick step quantum: each axis moves −1, 0 or +1 of this.
pub const STEP: f32 = 0.75;

/// The viewer stands at the grid centre; after the handover it is
/// 8 m east — the "other cell" position.
pub fn viewer_start() -> (f32, f32, f32) {
    (
        (GRID_COLS / 2) as f32 * SPACING,
        0.0,
        (GRID_ROWS / 2) as f32 * SPACING,
    )
}

pub fn viewer_migrated() -> (f32, f32, f32) {
    let (x, y, z) = viewer_start();
    (x + 8.0, y, z)
}

/// Field entity i's id and grid position.
pub fn field_entity(index: usize) -> (u32, (f32, f32, f32)) {
    (
        (index + 2) as u32,
        (
            (index % GRID_COLS) as f32 * SPACING,
            0.0,
            (index / GRID_COLS) as f32 * SPACING,
        ),
    )
}

/// Entity `id`'s step at `tick`: deterministic, the same schedule every
/// world that lives through that tick — two worlds comparing visible
/// sets watched identical trajectories.
pub fn step_of(id: u32, tick: usize) -> (f32, f32) {
    let mut s = (id as u64) ^ ((tick as u64) << 32) ^ AOI_SEED;
    s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let dx = ((s >> 33) % 3) as i32 - 1;
    s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let dz = ((s >> 33) % 3) as i32 - 1;
    (dx as f32 * STEP, dz as f32 * STEP)
}

/// What one tick broadcast, and the churn behind it.
#[derive(Debug, Default, Clone)]
pub struct TickLedger {
    /// Client messages the witness sent this tick.
    pub messages: usize,
    /// Their `wire_size` sum — the broadcast volume.
    pub bytes: usize,
    /// Seeing callbacks fired this tick.
    pub starts: usize,
    pub stops: usize,
    /// AoI trigger crossings this tick.
    pub enters: usize,
    pub leaves: usize,
    /// `CreateEntity` messages this tick (the creation burst).
    pub creates: usize,
    /// The ids the seeing pass started this tick.
    pub started_ids: Vec<u32>,
}

/// The migration leg: handover, seed, convergence to the native set.
#[derive(Debug)]
pub struct MigrationLedger {
    /// The visible set the stream carried off the old cell.
    pub handover_visible: usize,
    /// The set the destination seeded without re-announcing.
    pub seeded_visible: usize,
    /// Seeing starts on the destination for ids the client did not
    /// hold at the tick's start.
    pub re_announced: usize,
    /// Seeing starts that hit an id already visible at the tick's
    /// start — the seed/diff contract makes this impossible; always
    /// zero.
    pub duplicate_announcements: usize,
    /// Ticks until the destination's visible set equals the native
    /// viewer's set (same entities, same schedule, viewer always at
    /// the new position).
    pub convergence_ticks: usize,
    /// Both final visible sets' size — equality is asserted.
    pub converged_size: usize,
}

/// The saved baseline: tick count, broadcast volume, migration.
#[derive(Debug)]
pub struct DenseAoiBaseline {
    pub ticks: usize,
    pub per_tick: Vec<TickLedger>,
    pub messages_total: usize,
    pub bytes_total: usize,
    pub enters_total: usize,
    pub leaves_total: usize,
    pub starts_total: usize,
    pub stops_total: usize,
    pub creates_total: usize,
    /// Broadcast volume over the steady window (ticks `STEADY_FROM..`).
    pub steady_bytes: usize,
    /// Probe sums in milliseconds — printed, never asserted.
    pub seeing_ms: f64,
    pub scheduling_ms: f64,
    pub migration: MigrationLedger,
}

/// One wired dense-AoI world: the range list, the vision controller
/// with its seeing state, and the viewer's witness.
pub struct DenseAoi {
    pub world: VisionWorld,
    pub vision: VisionController,
    pub ev: EntityVision,
    pub witness: Witness,
    pub viewer_pos: (f32, f32, f32),
    positions: BTreeMap<u32, (f32, f32, f32)>,
    /// The viewer's AoI trigger in the range list.
    aoi_trigger: TriggerRef,
    /// Probe sums in milliseconds — printed, never asserted.
    seeing_ms: f64,
    scheduling_ms: f64,
}

impl DenseAoi {
    /// Wires a world: the viewer and the whole field on the range
    /// list, the witness's AoI trigger at the default radius, and the
    /// witness itself at the shipped capacity. Entities stand at
    /// `positions`; the trigger creations fire their initial crossings
    /// into the sinks.
    ///
    /// `build_vision_trigger` defers the vision trigger to a later
    /// `restore` — what a migrating entity does on its destination:
    /// `startReal` builds the trigger fresh, so its initial range
    /// population lands in `entitiesInVisionRange_` for real.
    pub fn with_positions(
        viewer_pos: (f32, f32, f32),
        positions: &BTreeMap<u32, (f32, f32, f32)>,
        build_vision_trigger: bool,
    ) -> Self {
        let mut world = VisionWorld::new();
        world.world.add_entity(VIEWER, viewer_pos.0, viewer_pos.2);
        for (id, pos) in positions {
            world.world.add_entity(*id, pos.0, pos.2);
        }

        let mut vision = add_vision(VISION_ANGLE, VISION_RANGE, SEEING_HEIGHT, 1, 7)
            .expect("the vision controller builds");
        let mut ev = EntityVision::default();
        if build_vision_trigger {
            world.vision_set_vision_range(&mut vision, &mut ev, VIEWER, VISION_ANGLE, VISION_RANGE);
        }
        let aoi_trigger = world.world.add_aoi_trigger(VIEWER, DEFAULT_AOI_RADIUS);

        let mut self_entity = EntitySnapshot::new(VIEWER, 1, viewer_pos);
        self_entity.volatile = VolatileInfo::NONE;
        let mut witness = Witness::new(VIEWER, self_entity);
        witness.set_witness_capacity(CAPACITY_BITS_PER_SECOND, CAPACITY_HERTZ);
        witness.set_aoi_radius(DEFAULT_AOI_RADIUS, 5.0);
        for (id, pos) in positions {
            witness.upsert_entity(EntitySnapshot::new(*id, 2, *pos));
        }

        DenseAoi {
            world,
            vision,
            ev,
            witness,
            viewer_pos,
            positions: positions.clone(),
            aoi_trigger,
            seeing_ms: 0.0,
            scheduling_ms: 0.0,
        }
    }

    /// The whole field on its starting grid.
    pub fn new(viewer_pos: (f32, f32, f32)) -> Self {
        let positions = (0..DENSE_ENTITIES)
            .map(|index| {
                let (id, pos) = field_entity(index);
                (id, pos)
            })
            .collect();
        Self::with_positions(viewer_pos, &positions, true)
    }

    /// The seeing pass's target facts for the current positions.
    fn targets(&self) -> BTreeMap<u32, TargetFacts> {
        self.positions
            .iter()
            .map(|(&id, &pos)| {
                (
                    id,
                    TargetFacts {
                        position: pos,
                        ground_position: pos,
                        visible_height: TARGET_HEIGHT,
                        can_be_seen: true,
                        should_drop_vision: false,
                    },
                )
            })
            .collect()
    }

    fn viewer_facts(&self) -> ViewerFacts {
        ViewerFacts {
            position: self.viewer_pos,
            ground_position: self.viewer_pos,
            yaw: 0.0,
            pitch: 0.0,
        }
    }

    /// One tick: move the field, feed the AoI crossings to the
    /// witness, run the seeing pass, complete the enter/create cycle
    /// for new arrivals, run the budget scheduler, and take the
    /// bundle. Returns what the tick broadcast.
    pub fn tick(&mut self, tick: usize) -> TickLedger {
        let mut ledger = TickLedger::default();

        // The field moves; the range list shuffles and logs crossings.
        for (id, pos) in self.positions.iter_mut() {
            let (dx, dz) = step_of(*id, tick);
            pos.0 += dx;
            pos.2 += dz;
            self.world.world.move_entity(*id, pos.0, pos.2);
            let mut snapshot = EntitySnapshot::new(*id, 2, *pos);
            snapshot.volatile = crate::witness::VolatileInfo::ALWAYS_ALL;
            self.witness.upsert_entity(snapshot);
        }

        // AoI crossings feed the witness (enter/create on arrival).
        let mut arrivals = Vec::new();
        let mut departures = Vec::new();
        for event in std::mem::take(&mut self.world.world.events) {
            match event {
                RangeListEvent::TriggerEnter { trigger, entity_id }
                    if trigger == self.aoi_trigger =>
                {
                    arrivals.push(entity_id)
                }
                RangeListEvent::TriggerLeave { trigger, entity_id }
                    if trigger == self.aoi_trigger =>
                {
                    departures.push(entity_id)
                }
                other => self.world.world.events.push(other),
            }
        }
        for id in &arrivals {
            self.witness.add_to_aoi(*id, false);
        }
        for id in &departures {
            self.witness.remove_from_aoi(*id, false);
        }
        ledger.enters = arrivals.len();
        ledger.leaves = departures.len();
        self.witness.flush_state_changes();

        // The seeing pass over the vision trigger's crossings.
        self.world.dispatch_vision(&self.vision, &mut self.ev);
        let targets = self.targets();
        let viewer = self.viewer_facts();
        let clear = |_: (f32, f32, f32), _: (f32, f32, f32)| 0.0f32;
        let started = Instant::now();
        self.world
            .vision_update(&mut self.vision, &mut self.ev, &viewer, &targets, &clear);
        let seeing = started.elapsed();

        // Seeing starts join the arrivals in the enter/create cycle:
        // one flush sends the EnterAoI messages, one reply per new
        // cache, one flush sends the CreateEntity messages.
        let mut created = Vec::new();
        for callback in self.world.callbacks.drain(..) {
            match callback {
                TrapCallback::StartSeeing { who } => {
                    self.witness.add_to_aoi(who, false);
                    created.push(who);
                    ledger.starts += 1;
                    ledger.started_ids.push(who);
                }
                TrapCallback::StopSeeing { .. } => {
                    ledger.stops += 1;
                }
                _ => {}
            }
        }
        if !created.is_empty() {
            self.witness.flush_state_changes();
        }
        for id in arrivals.iter().chain(created.iter()) {
            self.witness.request_entity_update(*id, &[]);
        }
        if !arrivals.is_empty() || !created.is_empty() {
            self.witness.flush_state_changes();
        }

        // The budget scheduler sends what the packet size allows.
        let started = Instant::now();
        self.witness.update();
        let scheduling = started.elapsed();

        let bundle = self.witness.take_bundle();
        ledger.messages = bundle.len();
        ledger.bytes = bundle.iter().map(|m| m.wire_size()).sum();
        ledger.creates = bundle
            .iter()
            .filter(|m| matches!(m, ClientMessage::CreateEntity { .. }))
            .count();

        self.seeing_ms += seeing.as_secs_f64() * 1_000.0;
        self.scheduling_ms += scheduling.as_secs_f64() * 1_000.0;
        ledger
    }

    /// The migration handover: the stream the old cell sends.
    pub fn handover(&self) -> (f32, f32, u32, u32, Vec<u32>) {
        self.world.vision_handover(&self.vision, &self.ev)
    }

    /// The destination: restore the stream and start real; the seeded
    /// visible set lands without re-announcing.
    pub fn restore(&mut self, stream: (f32, f32, u32, u32, Vec<u32>)) -> usize {
        self.world.vision_restore(&mut self.vision, stream.clone());
        self.world
            .vision_start_real(&mut self.vision, &mut self.ev, VIEWER);
        self.ev.visible_entities.len()
    }
}

/// Runs the dense baseline: `BASELINE_TICKS` at the grid centre, then
/// the handover, then `NEW_TICKS` on the destination beside a native
/// viewer that had always stood there.
pub fn run_dense_aoi_baseline() -> DenseAoiBaseline {
    let mut per_tick = Vec::with_capacity(BASELINE_TICKS);
    let mut cell = DenseAoi::new(viewer_start());

    // The trigger creations fire the initial AoI population into the
    // event log; the first tick drains it, so the creation burst is
    // tick 0's ledger.
    for tick in 0..BASELINE_TICKS {
        per_tick.push(cell.tick(tick));
    }

    let messages_total: usize = per_tick.iter().map(|t| t.messages).sum();
    let bytes_total: usize = per_tick.iter().map(|t| t.bytes).sum();

    // The migration: handover off the populated cell, land on the new
    // one, and drive the destination beside a native viewer.
    let stream = cell.handover();
    let handover_visible = cell.ev.visible_entities.len();
    let final_positions = cell.positions.clone();

    let mut destination = DenseAoi::with_positions(viewer_migrated(), &final_positions, false);
    let seeded_visible = destination.restore(stream);

    let mut native = DenseAoi::with_positions(viewer_migrated(), &final_positions, true);
    let mut re_announced = 0usize;
    let mut duplicate_announcements = 0usize;
    let mut convergence_ticks = NEW_TICKS;
    for tick in BASELINE_TICKS..BASELINE_TICKS + NEW_TICKS {
        let held_at_start = destination.ev.visible_entities.clone();
        let ledger = destination.tick(tick);
        native.tick(tick);
        for who in &ledger.started_ids {
            if held_at_start.contains(who) {
                duplicate_announcements += 1;
            } else {
                re_announced += 1;
            }
        }
        if convergence_ticks == NEW_TICKS
            && destination.ev.visible_entities == native.ev.visible_entities
        {
            convergence_ticks = tick - BASELINE_TICKS + 1;
        }
    }
    let converged = destination.ev.visible_entities == native.ev.visible_entities;
    assert!(converged, "the migrated viewer converges to the native set");
    assert_eq!(
        duplicate_announcements, 0,
        "no start ever hits an id the client already holds"
    );

    let seeing_ms = cell.seeing_ms;
    let scheduling_ms = cell.scheduling_ms;
    DenseAoiBaseline {
        ticks: BASELINE_TICKS,
        steady_bytes: per_tick[STEADY_FROM..].iter().map(|t| t.bytes).sum(),
        messages_total,
        bytes_total,
        enters_total: per_tick.iter().map(|t| t.enters).sum(),
        leaves_total: per_tick.iter().map(|t| t.leaves).sum(),
        starts_total: per_tick.iter().map(|t| t.starts).sum(),
        stops_total: per_tick.iter().map(|t| t.stops).sum(),
        creates_total: per_tick.iter().map(|t| t.creates).sum(),
        per_tick,
        seeing_ms,
        scheduling_ms,
        migration: MigrationLedger {
            handover_visible,
            seeded_visible,
            re_announced,
            duplicate_announcements,
            convergence_ticks,
            converged_size: destination.ev.visible_entities.len(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dense_baseline_saves_tick_broadcast_and_migration() {
        let baseline = run_dense_aoi_baseline();

        // Tick count saved.
        assert_eq!(baseline.ticks, BASELINE_TICKS);
        assert_eq!(baseline.per_tick.len(), BASELINE_TICKS);

        // The creation burst: the initial AoI trigger population
        // enters every entity inside the square window and creates
        // it once; the field's trajectory is bounded, so the steady
        // window sees no further arrivals or creations.
        assert!(baseline.enters_total > 0);
        assert!(baseline.enters_total <= DENSE_ENTITIES);
        assert_eq!(baseline.creates_total, baseline.enters_total);
        for tick in &baseline.per_tick[STEADY_FROM..] {
            assert_eq!(tick.creates, 0, "no creation after the burst");
            assert_eq!(tick.enters, 0, "the field stays inside the AoI");
        }

        // The broadcast volume is budget-bound: every steady tick
        // carries at most the 772-byte packet plus its fixed frames,
        // and the volume is deterministic tick for tick.
        let capacity =
            crate::witness::bps_to_packet_size(CAPACITY_BITS_PER_SECOND, CAPACITY_HERTZ) as usize;
        for tick in &baseline.per_tick[STEADY_FROM..] {
            assert!(
                tick.bytes <= capacity + 64,
                "the steady tick respects the budget: {} bytes",
                tick.bytes
            );
            assert!(tick.messages > 0, "a dense steady tick always sends");
        }

        // The seeing churn is cone-filtered: some of the field is
        // behind the viewer and never seen; stops exist but never
        // exceed starts.
        assert!(baseline.starts_total > 0);
        assert!(baseline.starts_total < DENSE_ENTITIES + baseline.migration.re_announced);
        assert!(baseline.stops_total > 0);
        assert!(baseline.stops_total <= baseline.starts_total + baseline.migration.re_announced);

        // The migration leg: the handover carried the whole visible
        // set, the destination seeded it without a single duplicate
        // announcement, and it converged to the native set.
        assert_eq!(
            baseline.migration.seeded_visible,
            baseline.migration.handover_visible
        );
        assert!(baseline.migration.handover_visible > 0);
        assert!(baseline.migration.re_announced > 0, "new ids re-announce");
        assert_eq!(baseline.migration.duplicate_announcements, 0);
        assert!(baseline.migration.convergence_ticks <= NEW_TICKS);

        // The saved ledger prints for the review doc; wall-clock is
        // probe data only.
        println!(
            "AOI ticks={} messages={} bytes={} enters={} leaves={} starts={} stops={} creates={} steady_bytes={}",
            baseline.ticks,
            baseline.messages_total,
            baseline.bytes_total,
            baseline.enters_total,
            baseline.leaves_total,
            baseline.starts_total,
            baseline.stops_total,
            baseline.creates_total,
            baseline.steady_bytes,
        );
        println!(
            "AOI migration handover={} seeded={} re_announced={} duplicates={} convergence_ticks={} converged_size={} seeing_ms={:.3} scheduling_ms={:.3}",
            baseline.migration.handover_visible,
            baseline.migration.seeded_visible,
            baseline.migration.re_announced,
            baseline.migration.duplicate_announcements,
            baseline.migration.convergence_ticks,
            baseline.migration.converged_size,
            baseline.seeing_ms,
            baseline.scheduling_ms,
        );
        for (tick, ledger) in baseline.per_tick.iter().enumerate() {
            println!(
                "AOI tick={tick} messages={} bytes={} starts={} stops={} enters={} leaves={} creates={}",
                ledger.messages,
                ledger.bytes,
                ledger.starts,
                ledger.stops,
                ledger.enters,
                ledger.leaves,
                ledger.creates
            );
        }
    }

    #[test]
    fn the_baseline_reruns_to_the_same_ledger() {
        let first = run_dense_aoi_baseline();
        let second = run_dense_aoi_baseline();
        assert_eq!(first.messages_total, second.messages_total);
        assert_eq!(first.bytes_total, second.bytes_total);
        assert_eq!(first.steady_bytes, second.steady_bytes);
        assert_eq!(first.starts_total, second.starts_total);
        assert_eq!(first.stops_total, second.stops_total);
        assert_eq!(
            first.migration.seeded_visible,
            second.migration.seeded_visible
        );
        assert_eq!(first.migration.re_announced, second.migration.re_announced);
        assert_eq!(
            first.migration.duplicate_announcements,
            second.migration.duplicate_announcements
        );
        for (a, b) in first.per_tick.iter().zip(&second.per_tick) {
            assert_eq!(a.messages, b.messages);
            assert_eq!(a.bytes, b.bytes);
            assert_eq!(a.starts, b.starts);
            assert_eq!(a.stops, b.stops);
        }
    }

    #[test]
    fn the_migration_seeds_without_recreating() {
        // The seed lands ids straight into the visible set: no
        // StartSeeing, no witness re-enter, no duplicate alias.
        let mut cell = DenseAoi::new(viewer_start());
        for tick in 0..BASELINE_TICKS {
            let _ = cell.tick(tick);
        }
        let stream = cell.handover();
        let seen_before = cell.ev.visible_entities.clone();

        let mut destination = DenseAoi::with_positions(viewer_migrated(), &cell.positions, false);
        let seeded = destination.restore(stream);
        assert_eq!(seeded, seen_before.len());
        assert_eq!(destination.ev.visible_entities, seen_before);
        assert!(destination.world.callbacks.is_empty(), "the seed is silent");
    }
}
