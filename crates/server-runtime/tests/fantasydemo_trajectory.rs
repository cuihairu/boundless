//! BW-14-01-03: running the fantasydemo's entry, movement and AoI, and
//! proving that one trajectory yields the same observation events every run.
//!
//! The reference faces, per leg:
//!
//! * entry — `Space::addEntity`'s `addToRangeList` tail adds the entity's
//!   range-list node; an entity already inside an observer's AoI radius
//!   announces `TriggerEnter` from the initial shuffle
//!   (`server/cellapp/entity.cpp:2406`, `range_list.cpp`);
//! * movement — `Entity::updateInternalsForNewPosition` shuffles the
//!   entity's leading triggers, the entity node and its trailing triggers
//!   in that order, and each list crossing is gated by `isIn*Range` (enter)
//!   or `wasIn*Range` (leave) so a move that never crosses a bound fires
//!   nothing (`range_list.cpp`, `cellapp_range_list.cpp`);
//! * AoI — `AoITrigger::triggerEnter`/`triggerLeave` forward the crossings
//!   to the witness's `addToAoI`/`removeFromAoI`
//!   (`server/cellapp/witness.cpp:3586-3614`); the default AoI radius is
//!   `DEFAULT_AOI_RADIUS = 500.f`
//!   (`server/cellapp/cellapp_config.cpp:21`), and the bounds build a
//!   half-open square `(centre - radius, centre + radius]` in both axes.
//!
//! The acceptance is comparability: the same trajectory must produce the
//! same observation events, so the test replays one fixed trajectory twice
//! on fresh worlds and compares the event vectors, then checks each event
//! against the geometry gates that produced it.

use server_runtime::range_list::{RangeListEvent, RangeListWorld};

const DEFAULT_AOI_RADIUS: f32 = 500.0;

fn enter(trigger: usize, entity_id: u32) -> RangeListEvent {
    RangeListEvent::TriggerEnter { trigger, entity_id }
}

fn leave(trigger: usize, entity_id: u32) -> RangeListEvent {
    RangeListEvent::TriggerLeave { trigger, entity_id }
}

/// A static guard observes an AoI square of `DEFAULT_AOI_RADIUS`; the avatar
/// walks the fixed trajectory. Returns the collected observation events.
fn run_trajectory() -> Vec<RangeListEvent> {
    let mut world = RangeListWorld::new();

    // The static guard (the fantasydemo's `cell/Guard.py` placeholder) at the
    // demo space's origin-ish point, with the reference default AoI radius.
    world.add_entity(900, 1000.0, 1000.0);
    world.add_aoi_trigger(900, DEFAULT_AOI_RADIUS);

    // Entry: the avatar appears inside the guard's AoI — the initial shuffle
    // announces the enter. It is kept in the collected vector.
    let _avatar = world.add_entity(901, 1400.0, 1000.0);
    assert_eq!(
        world.events,
        vec![enter(0, 901)],
        "entry must fire one enter"
    );

    // Movement: a fixed polyline. Each step is one tick's position update.
    let trajectory = [
        // Stays well inside: no crossing, no event.
        (1300.0, 1000.0),
        // Exactly on the upper x bound: the entity node (order 100) sorts
        // ahead of the bound (order 200), so the half-open window still
        // contains it and nothing fires.
        (1500.0, 1000.0),
        // One metre past the bound: the leave fires on the x crossing.
        (1501.0, 1000.0),
        // Far outside: no events.
        (1700.0, 1200.0),
        (1700.0, 1400.0),
        // Back across the x bound while z is inside: re-enters.
        (1400.0, 1400.0),
        // Out through the z bound: leaves again.
        (1000.0, 1600.0),
        // Back to the guard's own point: re-enters from the z sweep.
        (1000.0, 1000.0),
    ];
    for (x, z) in trajectory {
        world.move_entity(901, x, z);
    }

    assert!(world.is_sorted(), "the lists stay ordered after the walk");
    world.events
}

/// The acceptance: the same trajectory, replayed, yields the same observation
/// events. The expected vector is also pinned so a regression in the crossing
/// semantics shows up as a diff rather than as two equally-wrong runs.
#[test]
fn one_trajectory_yields_the_same_observation_events_on_every_run() {
    let first = run_trajectory();
    let second = run_trajectory();
    assert_eq!(first, second, "the trajectory must be reproducible");

    assert_eq!(
        first,
        vec![
            // The entry shuffle.
            enter(0, 901),
            // One metre past the x bound, then back in through x, out
            // through z, and back in to the guard's own point.
            leave(0, 901),
            enter(0, 901),
            leave(0, 901),
            enter(0, 901),
        ],
        "the entry plus the four crossings of the fixed polyline, in reference firing order"
    );
}

/// Every enter/leave the walk announces agrees with the geometry gates that
/// were consulted: no enter while the avatar was already contained, no leave
/// while it was already outside.
#[test]
fn every_observation_event_agrees_with_the_range_gates() {
    let mut world = RangeListWorld::new();
    world.add_entity(900, 1000.0, 1000.0);
    let trigger = world.add_aoi_trigger(900, DEFAULT_AOI_RADIUS);
    let avatar = world.add_entity(901, 1400.0, 1000.0);

    let trajectory = [
        (1500.0, 1000.0),
        (1501.0, 1000.0),
        (1700.0, 1400.0),
        (1400.0, 1400.0),
        (1000.0, 1600.0),
        (1000.0, 1000.0),
    ];

    let mut contained = true;
    world.events.clear();
    for (x, z) in trajectory {
        let was_contained = contained;
        world.move_entity(901, x, z);
        contained = world.contains(trigger, avatar);

        let enters = world
            .events
            .iter()
            .filter(|e| matches!(e, RangeListEvent::TriggerEnter { .. }))
            .count();
        let leaves = world
            .events
            .iter()
            .filter(|e| matches!(e, RangeListEvent::TriggerLeave { .. }))
            .count();

        if !was_contained && contained {
            assert_eq!(enters, 1, "a fresh containment is one enter");
            assert_eq!(leaves, 0, "an enter move announces no leave");
        } else if was_contained && !contained {
            assert_eq!(leaves, 1, "a lost containment is one leave");
            assert_eq!(enters, 0, "a leave move announces no enter");
        } else {
            assert!(
                world.events.is_empty(),
                "a move that keeps containment unchanged observes nothing"
            );
        }
        world.events.clear();
    }
}

/// Entry at the AoI edge: two guards stand at the centre and outside the
/// radius; the avatar appears inside only one of them, so exactly one enter
/// is observed and its `entities_between` snapshot agrees.
#[test]
fn entry_fires_for_the_guard_that_contains_the_avatar() {
    let mut world = RangeListWorld::new();
    world.add_entity(900, 1000.0, 1000.0);
    world.add_aoi_trigger(900, DEFAULT_AOI_RADIUS);
    world.add_entity(902, 2000.0, 1000.0);
    world.add_aoi_trigger(902, DEFAULT_AOI_RADIUS);

    world.add_entity(901, 1400.0, 1000.0);

    assert_eq!(
        world.events,
        vec![enter(0, 901)],
        "only the guard whose square contains the avatar sees it"
    );
    assert_eq!(world.entities_between(0), vec![901]);
    assert!(world.entities_between(1).is_empty());
}
