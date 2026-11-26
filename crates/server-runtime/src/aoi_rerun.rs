//! OPT-08-01-04: rerun the visible set, the event order and the
//! migration.
//!
//! The rerun harness drives the dense baseline and its migration leg
//! twice, comparing the whole **ordered** event record tick for tick:
//! the seeing starts and stops in callback order, the AoI enters and
//! leaves in trigger-firing order, and the raw bundle in send order.
//!
//! The acceptance **状态正确** (the state is correct) is a per-tick
//! audit that recomputes the shipped incremental state from the raw
//! positions and demands equality:
//!
//! - the vision trigger's population is the shipped window predicate
//!   `(sub − range < x ≤ sub + range)` on both axes,
//! - the visible set is the seeing pass's own cone ground truth on
//!   the ticks the pass runs (its gate is read off the controller's
//!   public tick counter), and exactly the carried set on the ticks
//!   it skips,
//! - the witness's AoI map is the 500 m window, entered and left
//!   exactly once per crossing,
//! - the seeing state machine is closed: starts are always new, stops
//!   always held, and the diff reproduces the visible set,
//! - the bundle's per-entity order is `EnterAoI` before `CreateEntity`
//!   before `EntityUpdate`, and no update ever precedes the entity's
//!   creation,
//! - after the migration the destination's visible set, vision range
//!   population and AoI map all equal the native viewer's — the state,
//!   not just the ledger.

use std::collections::{BTreeMap, BTreeSet};

use crate::aoi_baseline::{
    BASELINE_TICKS, DenseAoi, NEW_TICKS, SEEING_HEIGHT, TARGET_HEIGHT, TickLedger, VISION_ANGLE,
    VISION_RANGE, viewer_migrated, viewer_start,
};
use crate::vision::forward_vector;
use crate::witness::{ClientMessage, DEFAULT_AOI_RADIUS};

/// The range trigger's own window predicate — the shipped
/// `is_in_x_range`/`is_in_z_range` shape `(sub − range < x ≤ sub + range)`
/// on both axes.
pub fn window_truth(
    positions: &BTreeMap<u32, (f32, f32, f32)>,
    viewer: (f32, f32, f32),
    range: f32,
) -> BTreeSet<u32> {
    positions
        .iter()
        .filter(|(_, p)| {
            let dx = p.0 - viewer.0;
            let dz = p.2 - viewer.2;
            dx > -range && dx <= range && dz > -range && dz <= range
        })
        .map(|(&id, _)| id)
        .collect()
}

/// The seeing pass's cone ground truth: inside the vision window and
/// inside the cone from the viewer's eye at the full target height or,
/// on the pass's second try, at three quarters of it — the shipped
/// geometry (`forward_vector(0, 0)`, clear terrain, strict angle).
pub fn cone_truth(
    positions: &BTreeMap<u32, (f32, f32, f32)>,
    viewer: (f32, f32, f32),
    window: &BTreeSet<u32>,
) -> BTreeSet<u32> {
    let head = (viewer.0, viewer.1 + SEEING_HEIGHT, viewer.2);
    let direction = forward_vector(0.0, 0.0);
    let mut seen = BTreeSet::new();
    for (&id, pos) in positions {
        if !window.contains(&id) {
            continue;
        }
        for height in [TARGET_HEIGHT, TARGET_HEIGHT * 0.75] {
            let to = (pos.0 - head.0, pos.1 + height - head.1, pos.2 - head.2);
            let len = (to.0 * to.0 + to.1 * to.1 + to.2 * to.2).sqrt();
            let unit = (to.0 / len, to.1 / len, to.2 / len);
            let dot = (direction.0 * unit.0 + direction.1 * unit.1 + direction.2 * unit.2)
                .clamp(-1.0, 1.0);
            if dot.acos() < VISION_ANGLE {
                seen.insert(id);
                break;
            }
        }
    }
    seen
}

/// The expected-state threads the audit replays alongside a shipped
/// world: what the client has been told, what the seeing pass last
/// derived, and where the field stood at the previous tick.
#[derive(Default)]
struct ExpectedState {
    /// Ids a `CreateEntity` has gone out for.
    created: BTreeSet<u32>,
    /// Ids currently inside the witness's AoI.
    aoi: BTreeSet<u32>,
    /// What the seeing pass last derived (empty until its first run).
    visible: BTreeSet<u32>,
}

/// The bundle's per-entity order and its agreement with the client's
/// accumulated state. Panics on the first violation.
fn audit_bundle(tick: usize, ledger: &TickLedger, expected: &mut ExpectedState) {
    let mut enter_at: BTreeMap<u32, usize> = BTreeMap::new();
    let mut create_at: BTreeMap<u32, usize> = BTreeMap::new();
    let mut update_at: BTreeMap<u32, usize> = BTreeMap::new();
    for (index, message) in ledger.bundle.iter().enumerate() {
        match message {
            ClientMessage::EnterAoI { id, .. } => {
                enter_at.entry(*id).or_insert(index);
                assert!(
                    expected.aoi.insert(*id),
                    "tick {tick}: {id} entered an AoI it was already in"
                );
            }
            ClientMessage::LeaveAoI { id, .. } => {
                assert!(
                    expected.aoi.remove(id),
                    "tick {tick}: {id} left an AoI it never joined"
                );
            }
            ClientMessage::CreateEntity { id, .. }
            | ClientMessage::CreateEntityDetailed { id, .. } => {
                create_at.entry(*id).or_insert(index);
            }
            ClientMessage::EntityUpdate { id, .. } => {
                update_at.entry(*id).or_insert(index);
            }
            _ => {}
        }
    }
    for (id, &at) in &create_at {
        if let Some(&entered) = enter_at.get(id) {
            assert!(
                entered < at,
                "{id} created at {at} before its EnterAoI at {entered}"
            );
        }
    }
    for (id, &at) in &update_at {
        assert!(
            expected.created.contains(id) || create_at.get(id).is_some_and(|&c| c < at),
            "{id} updated at {at} before its CreateEntity"
        );
    }
    expected.created.extend(create_at.keys());
}

/// The per-tick state audit: the shipped incremental state must equal
/// the ground truth recomputed from the raw positions, and the event
/// record must close over itself. Panics on the first violation.
fn audit_tick(tick: usize, cell: &DenseAoi, ledger: &TickLedger, expected: &mut ExpectedState) {
    let positions = cell.positions().clone();
    let viewer = cell.viewer_pos;

    // The seeing state machine closes over itself: the diff walk
    // emits ascending, starts only new ids, stops only held ones,
    // and the diff reproduces the visible set.
    ascending(&ledger.started_ids, "starts");
    ascending(&ledger.stopped_ids, "stops");
    let stopped: BTreeSet<u32> = ledger.stopped_ids.iter().copied().collect();
    let started: BTreeSet<u32> = ledger.started_ids.iter().copied().collect();
    assert!(
        started.is_disjoint(&expected.visible),
        "a start hit an id the client already sees"
    );
    assert!(
        stopped.is_subset(&expected.visible),
        "a stop hit an id the client does not see"
    );
    let mut diff = expected.visible.clone();
    diff.extend(started);
    for id in &stopped {
        diff.remove(id);
    }
    assert_eq!(
        diff, cell.ev.visible_entities,
        "the seeing diff does not reproduce the visible set"
    );

    // The vision trigger's population is the window predicate.
    assert_eq!(
        cell.ev.entities_in_vision_range,
        window_truth(&positions, viewer, VISION_RANGE),
        "the vision range population is not the window"
    );

    // The visible set is the pass's own cone truth on its run ticks,
    // and exactly the carried set on its skip ticks (the gate read
    // off the controller's tick counter: 0 = it ran).
    if cell.vision.tick_since_last == 0 {
        expected.visible = cone_truth(&positions, viewer, &cell.ev.entities_in_vision_range);
    }
    assert_eq!(
        cell.ev.visible_entities, expected.visible,
        "the visible set is not the cone ground truth"
    );

    // The trigger crossings and the client's AoI messages agree, and
    // the witness's AoI map is the default-radius window.
    let bundle_enters: BTreeSet<u32> = ledger
        .bundle
        .iter()
        .filter_map(|m| match m {
            ClientMessage::EnterAoI { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    let bundle_leaves: BTreeSet<u32> = ledger
        .bundle
        .iter()
        .filter_map(|m| match m {
            ClientMessage::LeaveAoI { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(
        bundle_enters,
        ledger
            .entered_ids
            .iter()
            .copied()
            .collect::<BTreeSet<u32>>(),
        "tick {tick}: the EnterAoI messages are the trigger crossings"
    );
    assert_eq!(
        bundle_leaves,
        ledger.left_ids.iter().copied().collect::<BTreeSet<u32>>(),
        "tick {tick}: the LeaveAoI messages are the trigger crossings"
    );
    let mut held = cell.witness.aoi_ids();
    held.sort_unstable();
    let mut truth: Vec<u32> = window_truth(&positions, viewer, DEFAULT_AOI_RADIUS)
        .into_iter()
        .collect();
    truth.sort_unstable();
    assert_eq!(held, truth, "the witness AoI map is not the window");

    audit_bundle(tick, ledger, expected);
}

fn ascending(ids: &[u32], what: &str) {
    for pair in ids.windows(2) {
        assert!(
            pair[0] < pair[1],
            "{what} not in ascending callback order: {:?}",
            ids
        );
    }
}

/// One audited pass: the dense baseline, then the migration leg on a
/// destination beside a native viewer. Every tick of every world is
/// audited; the final states must be equal, not merely converged.
pub struct AuditedRun {
    pub ledgers: Vec<TickLedger>,
    pub migrated_ledgers: Vec<TickLedger>,
    pub handover_visible: usize,
    pub seeded_visible: usize,
    /// Seeing starts that hit an id the client already held — always
    /// zero (the seed/diff contract).
    pub duplicates: usize,
    pub final_destination: BTreeSet<u32>,
    pub final_native: BTreeSet<u32>,
}

/// Runs the audited baseline end to end.
pub fn run_audited() -> AuditedRun {
    let mut cell = DenseAoi::new(viewer_start());
    let mut expected = ExpectedState::default();
    let mut ledgers = Vec::with_capacity(BASELINE_TICKS);
    for tick in 0..BASELINE_TICKS {
        let ledger = cell.tick(tick);
        audit_tick(tick, &cell, &ledger, &mut expected);
        ledgers.push(ledger);
    }

    // The migration: the stream carries the visible set; the
    // destination's expected visible state starts exactly there.
    let stream = cell.handover();
    let handover_visible = cell.ev.visible_entities.len();
    let final_positions = cell.positions().clone();

    let mut destination = DenseAoi::with_positions(viewer_migrated(), &final_positions, false);
    let seeded = destination.restore(stream.clone());
    assert_eq!(seeded, handover_visible, "the seed carries the whole set");
    assert_eq!(
        destination.ev.visible_entities, cell.ev.visible_entities,
        "the seed lands the handover's set"
    );
    assert!(destination.world.callbacks.is_empty(), "the seed is silent");

    let mut native = DenseAoi::with_positions(viewer_migrated(), &final_positions, true);
    let mut destination_state = ExpectedState {
        visible: destination.ev.visible_entities.clone(),
        ..ExpectedState::default()
    };
    let mut native_state = ExpectedState::default();

    let mut migrated_ledgers = Vec::with_capacity(NEW_TICKS);
    let duplicates = 0usize;
    for tick in BASELINE_TICKS..BASELINE_TICKS + NEW_TICKS {
        let held_at_start = destination.ev.visible_entities.clone();
        let ledger = destination.tick(tick);
        audit_tick(tick, &destination, &ledger, &mut destination_state);
        for who in &ledger.started_ids {
            assert!(
                !held_at_start.contains(who),
                "tick {tick}: {who} re-announced while held"
            );
        }

        let native_ledger = native.tick(tick);
        audit_tick(tick, &native, &native_ledger, &mut native_state);
        migrated_ledgers.push(ledger);
    }

    // The acceptance's state correctness: not just the visible set —
    // the whole per-world state is equal and equal to the ground truth.
    let final_positions_after = destination.positions().clone();
    let truth_window = window_truth(&final_positions_after, viewer_migrated(), VISION_RANGE);
    assert_eq!(
        destination.ev.visible_entities, native.ev.visible_entities,
        "the destination's visible set equals the native viewer's"
    );
    assert_eq!(
        destination.ev.entities_in_vision_range, native.ev.entities_in_vision_range,
        "the destination's vision range population equals the native one"
    );
    assert_eq!(
        destination.ev.entities_in_vision_range, truth_window,
        "the destination's vision range population is the window"
    );
    let mut destination_aoi = destination.witness.aoi_ids();
    let mut native_aoi = native.witness.aoi_ids();
    destination_aoi.sort_unstable();
    native_aoi.sort_unstable();
    assert_eq!(destination_aoi, native_aoi, "the AoI maps are equal");

    AuditedRun {
        ledgers,
        migrated_ledgers,
        handover_visible,
        seeded_visible: seeded,
        duplicates,
        final_destination: destination.ev.visible_entities,
        final_native: native.ev.visible_entities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rerun_reproduces_the_event_order_tick_for_tick() {
        let first = run_audited();
        let second = run_audited();

        assert_eq!(first.ledgers.len(), second.ledgers.len());
        for (tick, (a, b)) in first.ledgers.iter().zip(&second.ledgers).enumerate() {
            assert_eq!(a.bundle, b.bundle, "tick {tick}: the send order reproduces");
            assert_eq!(a.started_ids, b.started_ids, "tick {tick}: starts");
            assert_eq!(a.stopped_ids, b.stopped_ids, "tick {tick}: stops");
            assert_eq!(a.entered_ids, b.entered_ids, "tick {tick}: enters");
            assert_eq!(a.left_ids, b.left_ids, "tick {tick}: leaves");
        }
        assert_eq!(
            first.migrated_ledgers, second.migrated_ledgers,
            "the migration leg's event order reproduces"
        );

        let starts: usize = first.ledgers.iter().map(|l| l.starts).sum::<usize>()
            + first
                .migrated_ledgers
                .iter()
                .map(|l| l.starts)
                .sum::<usize>();
        let stops: usize = first.ledgers.iter().map(|l| l.stops).sum::<usize>()
            + first
                .migrated_ledgers
                .iter()
                .map(|l| l.stops)
                .sum::<usize>();
        println!(
            "STATE-AUDIT ticks={} migration_ticks={} starts={} stops={} handover={} seeded={} duplicates={} final={}",
            first.ledgers.len(),
            first.migrated_ledgers.len(),
            starts,
            stops,
            first.handover_visible,
            first.seeded_visible,
            first.duplicates,
            first.final_destination.len(),
        );
    }

    #[test]
    fn the_migrated_state_is_correct() {
        let run = run_audited();

        assert_eq!(run.seeded_visible, run.handover_visible);
        assert_eq!(run.duplicates, 0, "no duplicate announcements");
        assert_eq!(
            run.final_destination, run.final_native,
            "the destination's final state equals the native state"
        );
        assert!(!run.final_destination.is_empty());
    }
}
