//! BW-10-07-05: the two-cell Bot crossing recorder.
//!
//! A Bot walks a `MoveToPointController` from one cell into its neighbour.
//! The recorder logs every tick's position with the cell whose rect owns
//! it and the reference's four cell-transition callbacks
//! (`cell.cpp:212`/`:225`, `entity.cpp:4693`/`:4791`) in the order the
//! engine fires them. The offload decision is the live ghost maintainer's;
//! the controller that arrives on the destination is the one the source
//! streamed, so the walk continues from the same state.

use std::collections::BTreeMap;

use crate::cellappmgr::{Addr, Rect, SpaceId};
use crate::controllers::{Controller, Controllers, DOMAIN_REAL};
use crate::entity_dispatch::EntityId;
use crate::ghost::{
    GhostChannel, GhostConfig, GhostHost, GhostMaintainer, GhostMessage, GhostedEntityView,
    convert_real_to_ghost,
};
use crate::movement::{Direction3D, EntityState, MoveToPointController, ScriptCallback};
use crate::real_entity::{EntityCallbackBuffer, RealEntity};
use crate::space::CellInfoRecord;
use crate::witness::VolatileInfo;
use foundation::math::Vector3;
use foundation::stream::BinaryStream;

/// One recorded event in the Bot's crossing.
#[derive(Clone, Debug, PartialEq)]
pub enum CrossingEvent {
    /// A tick's position and the cell whose rect owns it.
    Move {
        cell: Addr,
        tick: u32,
        position: Vector3,
    },
    /// A script callback delivered on the cell named by `cell`.
    Callback { cell: Addr, method: String },
}

/// The complete trace of one crossing.
#[derive(Clone, Debug, PartialEq)]
pub struct BotCrossingTrace {
    /// The cell the Bot started on.
    pub source_cell: Addr,
    /// The cell the Bot offloaded to.
    pub destination_cell: Addr,
    /// The tick whose offload check picked the destination.
    pub crossing_tick: u32,
    /// The position the Bot finished at.
    pub final_position: Vector3,
    /// Movement and callback events, in firing order.
    pub events: Vec<CrossingEvent>,
}

/// Drive a Bot from `start` to `destination` over the two cells split at
/// `boundary_x`, recording the movement and cell callbacks.
#[allow(clippy::too_many_arguments)]
pub fn record_bot_crossing(
    entity_id: EntityId,
    source_cell: Addr,
    destination_cell: Addr,
    space_id: SpaceId,
    start: Vector3,
    destination: Vector3,
    boundary_x: f32,
    velocity: f32,
    hertz: f32,
) -> BotCrossingTrace {
    let mut events: Vec<CrossingEvent> = Vec::new();

    // The two cells: A owns x in [0, boundary), B owns [boundary, 2*boundary).
    let mut cells: BTreeMap<Addr, CellInfoRecord> = BTreeMap::new();
    let mut source = CellInfoRecord::new(source_cell, Rect::new(0.0, 0.0, boundary_x, 100.0));
    source.has_been_created = true;
    cells.insert(source_cell, source);
    let mut dest = CellInfoRecord::new(
        destination_cell,
        Rect::new(boundary_x, 0.0, boundary_x * 2.0, 100.0),
    );
    dest.has_been_created = true;
    cells.insert(destination_cell, dest);

    let mut channels: BTreeMap<Addr, GhostChannel> = BTreeMap::new();
    channels.insert(source_cell, GhostChannel::new(source_cell));
    channels.insert(destination_cell, GhostChannel::new(destination_cell));

    let mut maintainer = GhostMaintainer {
        config: GhostConfig {
            ghost_distance: 30.0,
            ..GhostConfig::default()
        },
        own_addr: source_cell,
        space_id,
        should_offload: true,
        budget: Default::default(),
    };

    let mut real = RealEntity::new(entity_id, source_cell, start, 0, 0);
    let mut view = ghost_view(entity_id, start);

    // The Bot's movement controller, registered as the entity's real-only
    // controller so the offload streams its live state.
    let mut controllers = Controllers::new();
    let mut out = Vec::new();
    let controller_id =
        controllers.add_controller(Controller::new(7, DOMAIN_REAL), 9, entity_id, &[], &mut out);
    let mut state = EntityState::new(entity_id, start, Direction3D::default(), 0);
    let mut controller =
        MoveToPointController::new(controller_id, 9, destination, velocity, true, false, hertz);
    let mut state_stream = BinaryStream::new();
    controller.write_real_to_stream(&mut state_stream);
    controllers
        .controller_mut(controller_id)
        .expect("registered")
        .real_state = state_stream.as_bytes().to_vec();

    // The destination app: a host with the entity's type registered, ready
    // to receive the ghost and then the onload.
    let mut dest_host = GhostHost::default();
    dest_host.register_type(
        4,
        crate::ghost::GhostEntityTypeDesc {
            prop_count_ghost: 0,
            num_event_stamped_properties: 0,
        },
    );
    let mut dest_callbacks = EntityCallbackBuffer::new();
    let mut arriving = Controllers::new();
    let mut source_callbacks = EntityCallbackBuffer::new();

    let mut crossed = false;
    let mut crossing_tick = 0;
    let mut now = 0u32;
    let mut arrived = false;
    for _ in 0..1000 {
        now += 1;
        let mut move_log: Vec<ScriptCallback> = Vec::new();
        let done = controller.update(&mut state, now, &mut move_log);
        view.position = state.position;
        let cell = cell_of(&cells, state.position);
        events.push(CrossingEvent::Move {
            cell,
            tick: now,
            position: state.position,
        });
        for callback in &move_log {
            events.push(CrossingEvent::Callback {
                cell,
                method: callback.method.to_string(),
            });
        }

        if !crossed {
            let report = maintainer.check(&cells, &mut channels, &mut real, &view, now);
            deliver_creates(&mut channels, &mut dest_host, source_cell, now);
            if let Some(dest) = report.offload_destination {
                crossed = true;
                crossing_tick = now;
                // Source cell: `onLeavingCell` before the offload,
                // `onLeftCell` after it (`cell.cpp:212`/`:225`).
                source_callbacks.callback(entity_id, "onLeavingCell", false);
                let (payload, _record) = convert_real_to_ghost(
                    &mut real,
                    &view,
                    &mut controllers,
                    &mut source_callbacks,
                    dest,
                    source_cell,
                    space_id,
                    false,
                    now,
                    &mut channels,
                );
                source_callbacks.callback(entity_id, "onLeftCell", false);
                for callback in source_callbacks.executed() {
                    events.push(CrossingEvent::Callback {
                        cell: source_cell,
                        method: callback.method.clone(),
                    });
                }
                // Destination cell: `onEnteringCell` on the ghost, then the
                // onload fires `onEnteredCell` (`entity.cpp:4693`/`:4791`).
                dest_callbacks.callback(entity_id, "onEnteringCell", false);
                dest_host
                    .receive_onload(
                        payload,
                        &mut arriving,
                        &mut dest_callbacks,
                        space_id,
                        None,
                        &mut channels,
                        dest,
                        now,
                    )
                    .expect("the ghost is here");
                for callback in dest_callbacks.executed() {
                    events.push(CrossingEvent::Callback {
                        cell: destination_cell,
                        method: callback.method.clone(),
                    });
                }
                // The walk continues on the controller the source streamed.
                let mut resumed = MoveToPointController::new(
                    controller_id,
                    9,
                    destination,
                    velocity,
                    true,
                    false,
                    hertz,
                );
                let mut reader = BinaryStream::from_slice(
                    &arriving
                        .controller(controller_id)
                        .expect("arrived")
                        .real_state,
                );
                resumed
                    .read_real_from_stream(&mut reader)
                    .expect("the state round-trips");
                controller = resumed;
            }
        }

        if done {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "the walk arrived");

    BotCrossingTrace {
        source_cell,
        destination_cell,
        crossing_tick,
        final_position: state.position,
        events,
    }
}

/// The ghost snapshot the offload and onload carry (`ghost_snapshot.cpp`):
/// zero ghosted properties, the position the real is at.
fn ghost_view(entity_id: EntityId, position: Vector3) -> GhostedEntityView {
    GhostedEntityView {
        entity_id,
        entity_type_id: 4,
        appeal_radius: 0.0,
        num_times_real_offloaded: 0,
        position,
        is_on_ground: true,
        last_event_number: 0,
        volatile: VolatileInfo::NONE,
        base_addr: (9, 9),
        direction: Vector3::new(1.5, 0.25, -0.5),
        property_event_stamps: vec![],
        ghost_properties: vec![],
        real_properties: vec![],
        ghost_controllers: vec![],
        periods_without_witness: 0,
        aoi_update_scheme_id: 0,
    }
}

/// The cell whose rect owns the position (the `pCellAt` leaf lookup).
fn cell_of(cells: &BTreeMap<Addr, CellInfoRecord>, position: Vector3) -> Addr {
    cells
        .values()
        .find(|info| {
            info.rect.xmin <= position.x
                && position.x < info.rect.xmax
                && info.rect.ymin <= position.z
                && position.z < info.rect.ymax
        })
        .map(|info| info.addr)
        .expect("the position is inside a cell")
}

/// Deliver every `createGhost` the maintainer queued to the destination
/// host, the way the channel bundle would carry it across the cellapp.
fn deliver_creates(
    channels: &mut BTreeMap<Addr, GhostChannel>,
    dest_host: &mut GhostHost,
    src: Addr,
    time: u32,
) {
    for channel in channels.values_mut() {
        let messages = std::mem::take(&mut channel.out);
        for message in messages {
            if let GhostMessage::CreateGhost(payload) = message {
                dest_host.receive_create_ghost(src, payload, time);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    fn callbacks_of(trace: &BotCrossingTrace) -> Vec<(Addr, String)> {
        trace
            .events
            .iter()
            .filter_map(|event| match event {
                CrossingEvent::Callback { cell, method } => Some((*cell, method.clone())),
                CrossingEvent::Move { .. } => None,
            })
            .collect()
    }

    #[test]
    fn a_bot_walk_records_moves_and_four_cell_callbacks_on_both_cells() {
        let trace = record_bot_crossing(
            10,
            (1, 1),
            (2, 2),
            7,
            v(97.0, 0.0, 50.0),
            v(108.0, 0.0, 50.0),
            100.0,
            5.0,
            10.0,
        );

        // The reference's four cell callbacks in firing order, then the
        // arrival's `onMove` — onLeavingCell/onLeftCell on the source,
        // onEnteringCell/onEnteredCell on the destination.
        assert_eq!(
            callbacks_of(&trace),
            vec![
                ((1, 1), "onLeavingCell".to_string()),
                ((1, 1), "onLeftCell".to_string()),
                ((2, 2), "onEnteringCell".to_string()),
                ((2, 2), "onEnteredCell".to_string()),
                ((2, 2), "onMove".to_string()),
            ]
        );

        // Movement is recorded on both cells: the walk starts on the source
        // and finishes on the destination.
        assert!(
            trace
                .events
                .iter()
                .any(|event| matches!(event, CrossingEvent::Move { cell: (1, 1), .. }))
        );
        assert!(
            trace
                .events
                .iter()
                .any(|event| matches!(event, CrossingEvent::Move { cell: (2, 2), .. }))
        );
        assert_eq!(trace.final_position, v(108.0, 0.0, 50.0));
        assert_eq!(trace.source_cell, (1, 1));
        assert_eq!(trace.destination_cell, (2, 2));
    }

    #[test]
    fn the_crossing_happens_once_the_boundary_is_crossed() {
        let trace = record_bot_crossing(
            10,
            (1, 1),
            (2, 2),
            7,
            v(97.0, 0.0, 50.0),
            v(108.0, 0.0, 50.0),
            100.0,
            5.0,
            10.0,
        );

        // 5 m/s at 10 Hz = 0.5 m/tick: the boundary at x = 100 is crossed
        // on tick 6 (97 + 3.0), the destination at x = 108 on tick 22.
        assert_eq!(trace.crossing_tick, 6);
        let cell_changes: Vec<Addr> = trace
            .events
            .iter()
            .filter_map(|event| match event {
                CrossingEvent::Move { cell, .. } => Some(*cell),
                CrossingEvent::Callback { .. } => None,
            })
            .collect();
        let first_dest = cell_changes
            .iter()
            .position(|cell| *cell == (2, 2))
            .expect("the walk reaches the destination");
        assert_eq!(first_dest, 5, "the 6th tick's move is the first on B");
    }
}
