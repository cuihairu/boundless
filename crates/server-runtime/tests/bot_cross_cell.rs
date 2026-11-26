//! BW-10-07-05: a bot crossing the boundary between two Cells.
//!
//! Three faces already ported compose the reference's two-cell traversal:
//! the simulated client's movement (`ClientApp::tick`,
//! `server/tools/bots/client_app.cpp:441-487`, carried by [`load::Bot`]); the
//! cell regions that decide which app hosts the real entity
//! (`CellInfo::contains` over `BW::Rect`, `cell_info.hpp:58`); and the
//! per-cell range lists that fire the AoI enter/leave events the Witness
//! forwards (`server/cellapp/range_list.cpp`, `witness.cpp:3586-3614`).
//!
//! This harness composes them without a live server: a real `load::Bot`
//! supplies the tick-by-tick positions, two adjacent `Rect`s stand for the
//! cells, and each cell's [`RangeListWorld`] holds a static guard's AoI. The
//! bot's position feeds the owning cell's range list every tick; a boundary
//! crossing hands the entity from one cell's world to the other's.

use foundation::math::Vector3;
use load::{Bot, Transport};
use server_runtime::cellappmgr::Rect;
use server_runtime::range_list::{RangeListEvent, RangeListWorld};

const GUARD_A: u32 = 900;
const GUARD_B: u32 = 901;
const BOT_ID: u32 = 902;
const AOI_RADIUS: f32 = 500.0;

/// Cell 0 covers x < 0, cell 1 covers x >= 0.
const CELL_A: usize = 0;
const CELL_B: usize = 1;

fn owning_cell(x: f32) -> usize {
    if x < 0.0 { CELL_A } else { CELL_B }
}

fn enter(trigger: usize) -> RangeListEvent {
    RangeListEvent::TriggerEnter {
        trigger,
        entity_id: BOT_ID,
    }
}

fn leave(trigger: usize) -> RangeListEvent {
    RangeListEvent::TriggerLeave {
        trigger,
        entity_id: BOT_ID,
    }
}

/// Run one fixed bot trajectory from x=-300 to x=+300 across the two cells,
/// feeding each tick's position to the owning cell's range list and moving
/// the entity between the worlds at the crossing. Returns the two event
/// vectors and the recorded crossing.
fn run_crossing() -> (Vec<RangeListEvent>, Vec<RangeListEvent>, Option<f32>) {
    // The two cell regions.
    let cell_a = Rect::new(-1000.0, -1000.0, 0.0, 1000.0);
    let cell_b = Rect::new(0.0, -1000.0, 1000.0, 1000.0);
    assert!(cell_a.contains_rect(&Rect::new(-300.0, 0.0, -300.0, 0.0)));
    assert!(!cell_b.contains_rect(&Rect::new(-300.0, 0.0, -300.0, 0.0)));
    assert!(cell_b.contains_rect(&Rect::new(300.0, 0.0, 300.0, 0.0)));

    // One guard's AoI per cell.
    let mut worlds = [RangeListWorld::new(), RangeListWorld::new()];
    worlds[CELL_A].add_entity(GUARD_A, -400.0, 0.0);
    worlds[CELL_A].add_aoi_trigger(GUARD_A, AOI_RADIUS);
    worlds[CELL_B].add_entity(GUARD_B, 400.0, 0.0);
    worlds[CELL_B].add_aoi_trigger(GUARD_B, AOI_RADIUS);

    // A real bot, logged on with its player entity.
    let mut bot = Bot::new("CrossBot", "cross", Transport::Udp, "cross-cell");
    bot.feed_log_on_result(BOT_ID, 1);
    assert!(bot.tick(0.0));
    assert!(bot.in_world());
    bot.snap_to(Vector3::new(-300.0, 0.0, 0.0));
    bot.move_to(Vector3::new(300.0, 0.0, 0.0));

    // Entry: the bot appears inside A's guard's AoI. The initial shuffle
    // announces one enter.
    let start = owning_cell(-300.0);
    assert_eq!(start, CELL_A);
    worlds[CELL_A].add_entity(BOT_ID, -300.0, 0.0);
    assert_eq!(worlds[CELL_A].events, vec![enter(0)]);

    // Walk the bot across. Each tick moves it `speed * 0.2` units; stop once
    // it is within a unit of the destination.
    let mut owner = start;
    let mut crossing_at = None;
    for _ in 0..600 {
        assert!(bot.tick(0.2));
        let p = bot.position();
        let new_owner = owning_cell(p.x);
        if new_owner != owner {
            // The entity leaves the old cell and enters the new one.
            worlds[owner].remove_entity(BOT_ID);
            worlds[new_owner].add_entity(BOT_ID, p.x, p.z);
            crossing_at = Some(p.x);
            owner = new_owner;
        } else {
            worlds[owner].move_entity(BOT_ID, p.x, p.z);
        }
        if (p.x - 300.0).abs() < 1.0 {
            break;
        }
    }
    assert_eq!(owner, CELL_B, "the bot ends in cell B");

    let events_a = worlds[CELL_A].events.clone();
    let events_b = worlds[CELL_B].events.clone();
    // Leaving A: the removal shuffles the bot to max-z, so A's guard fires
    // the leave. Then A's world drops the bot.
    assert_eq!(events_a, vec![enter(0), leave(0)], "A's guard events");
    // Entering B inside B's guard's AoI: one enter.
    assert_eq!(events_b, vec![enter(0)], "B's guard events");
    (events_a, events_b, crossing_at)
}

#[test]
fn bot_crosses_two_cells_and_records_the_enter_and_leave_events() {
    let (events_a, events_b, crossing_at) = run_crossing();
    // Exactly one crossing, and it happens at the boundary (x >= 0).
    let x = crossing_at.expect("the bot crosses the boundary");
    // The first tick after x=0 lands at x ≈ 1.2 (speed * 0.2s).
    assert!((0.0..2.0).contains(&x), "crossing at x={x}");

    // A lost the bot, B gained it.
    assert!(matches!(
        events_a.last(),
        Some(RangeListEvent::TriggerLeave { .. })
    ));
    assert!(matches!(
        events_b.first(),
        Some(RangeListEvent::TriggerEnter { .. })
    ));
}

#[test]
fn replaying_the_trajectory_yields_the_same_events() {
    let first = run_crossing();
    let second = run_crossing();
    assert_eq!(first, second, "same trajectory, same events");
}
