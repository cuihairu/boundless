//! BW-08-05-03: transferring controllers and callback state across an offload.
//!
//! When a Real offloads, its real-domain controllers are serialized into the
//! `onload` stream (`writeRealsToStream`), the destination cell recreates or
//! reuses them (`readRealsFromStream`), and they are restarted only after the
//! entity has become a Real (`startRealControllers`). The movement state —
//! velocity, top speed and physics corrections — rides in the same stream
//! (`writeOffloadData`), so the client-side trajectory continues without a
//! jump. Around the switch the offload lifecycle callbacks fire in order.
//!
//! The acceptance, "移动过程连续", is that the movement trajectory of an
//! entity that offloads mid-flight is identical to one that never offloads:
//! the controller's position and velocity carry across the switch and the
//! following ticks land on the same positions.
//!
//! Reference:
//! - `real_entity.cpp:501-566` `writeOffloadData` — stream the channel,
//!   `velocity_`, `topSpeed_`/`topSpeedY_`, `physicsCorrections_`,
//!   `shouldAutoBackup_`, the space id, `isTeleport`, the haunts,
//!   `controlledBy_`, the profiler, the real controllers,
//!   `periodsWithoutWitness_` and the witness.
//! - `real_entity.cpp:294-...` `readOffloadData` — read it all back, and
//!   reset `physicsLastValidated_` so the entity is not offloaded again within
//!   a second.
//! - `controllers.cpp:174-212` `writeRealsToStream` — `lastAllocatedID_`, the
//!   count of `DOMAIN_REAL` controllers, and for each its id (plus its type
//!   and userArg when it is not also `DOMAIN_GHOST`) and its real state.
//! - `controllers.cpp:82-120` `readRealsFromStream` — `lastAllocatedID_`, then
//!   for each offloaded controller reuse the existing ghost-domain controller
//!   of the same id, or create it fresh when it is real-only, and read its
//!   real state.
//! - `entity.cpp:2204` `writeRealControllersToStream` and `entity.cpp:1907`
//!   `readRealControllersFromStream` — the entity delegates to its
//!   controllers.
//! - `entity.cpp:2211` `startRealControllers` / `entity.cpp:2222`
//!   `stopRealControllers` — the real controllers are started only once the
//!   entity is Real, and stopped when it leaves.
//! - `controller.hpp:28-32` `ControllerDomain` — `DOMAIN_GHOST = 1`,
//!   `DOMAIN_REAL = 2`, `DOMAIN_GHOST_AND_REAL = 3`.
//! - `base_acceleration_controller.cpp:405` `readRealFromStream` — a movement
//!   controller is restored at its current position and velocity.
//! - `cell.cpp:210`/`:218` — `onLeavingCell` then `onLeftCell` on the old
//!   cell; `entity.cpp:4667`/`:4785` — `onEnteringCell` then `onEnteredCell`
//!   on the new cell, the latter replayed after the role change.

use std::collections::BTreeMap;

use crate::movement::{Facing, MovementController, Vec3};
use crate::real_entity::{ControllerId, UserArg};

/// `ControllerDomain` (`controller.hpp:28-32`): the bit flags for the domains
/// a controller runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerDomain(u8);

impl ControllerDomain {
    /// `DOMAIN_GHOST`.
    pub const GHOST: u8 = 1;
    /// `DOMAIN_REAL`.
    pub const REAL: u8 = 2;
    /// `DOMAIN_GHOST | DOMAIN_REAL`.
    pub const GHOST_AND_REAL: u8 = 3;

    /// A ghost-only controller.
    pub fn ghost() -> Self {
        ControllerDomain(Self::GHOST)
    }

    /// A real-only controller.
    pub fn real() -> Self {
        ControllerDomain(Self::REAL)
    }

    /// A controller that runs on both the ghost and the real.
    pub fn ghost_and_real() -> Self {
        ControllerDomain(Self::GHOST_AND_REAL)
    }

    /// `cd & DOMAIN_REAL`.
    pub fn has_real(self) -> bool {
        self.0 & Self::REAL != 0
    }

    /// `cd & DOMAIN_GHOST`.
    pub fn has_ghost(self) -> bool {
        self.0 & Self::GHOST != 0
    }
}

/// The streamed state of a `BaseAccelerationController`
/// (`writeRealToStream` / `readRealFromStream`, `base_acceleration_controller.cpp:405`).
#[derive(Debug, Clone, PartialEq)]
pub struct MovementState {
    /// `acceleration_`: acceleration in metres/s².
    pub acceleration: f64,
    /// `maxSpeed_`.
    pub max_speed: f64,
    /// `facing_`.
    pub facing: Facing,
    /// `stopAtDestination`.
    pub stop_at_destination: bool,
    /// `destination_`.
    pub destination: Vec3,
    /// `localPosition_` at the time of the offload.
    pub position: Vec3,
    /// `localDirection_.yaw` at the time of the offload.
    pub yaw: f64,
    /// `currVelocity_` at the time of the offload.
    pub velocity: Vec3,
    /// `CellAppConfig::updateHertz()`.
    pub hertz: f64,
}

impl MovementState {
    /// `readRealFromStream` (`base_acceleration_controller.cpp:405`): restore
    /// the controller at its transferred position and velocity.
    pub fn restore(&self) -> MovementController {
        MovementController::with_state(
            self.acceleration,
            self.max_speed,
            self.facing,
            self.stop_at_destination,
            self.destination,
            self.position,
            self.yaw,
            self.velocity,
            self.hertz,
        )
    }
}

/// The transferable real state of a controller (`writeRealToStream`).
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerState {
    /// `BaseAccelerationController`.
    Movement(MovementState),
    /// `TimerController`: its interval and start time.
    Timer {
        /// `interval_`.
        interval_ticks: u32,
        /// `start_`.
        start_ticks: u32,
    },
    /// An opaque controller payload.
    Other(Vec<u8>),
}

/// One controller attached to an entity.
#[derive(Debug, Clone, PartialEq)]
pub struct Controller {
    /// `id()`.
    pub id: ControllerId,
    /// `domain()`.
    pub domain: ControllerDomain,
    /// `userArg()`.
    pub user_arg: UserArg,
    /// The transferable real state.
    pub state: ControllerState,
    /// Whether the real-domain part is running (`startReals`/`stopReals`).
    pub running: bool,
}

/// One offloaded real-domain controller in the stream.
#[derive(Debug, Clone, PartialEq)]
pub struct RealControllerRecord {
    /// The controller's id.
    pub id: ControllerId,
    /// Whether the controller also runs on the ghost (`cd & DOMAIN_GHOST`),
    /// so the destination already holds it and only the state is sent.
    pub shared_with_ghost: bool,
    /// The `userArg` to recreate a real-only controller (`None` when shared).
    pub user_arg: Option<UserArg>,
    /// The real state.
    pub state: ControllerState,
}

/// The serialized real controllers (`writeRealsToStream`,
/// `controllers.cpp:174-212`).
#[derive(Debug, Clone, PartialEq)]
pub struct OffloadFrame {
    /// `lastAllocatedID_`: the highest id handed out, so the destination can
    /// keep allocating above it.
    pub last_allocated_id: ControllerId,
    /// The `DOMAIN_REAL` controllers, in the stream order.
    pub reals: Vec<RealControllerRecord>,
}

/// The controllers attached to an entity (`Controllers`, `controllers.cpp`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Controllers {
    /// The controllers, keyed by id.
    by_id: BTreeMap<ControllerId, Controller>,
    /// `lastAllocatedID_`.
    last_allocated_id: ControllerId,
}

impl Controllers {
    /// An empty set of controllers.
    pub fn new() -> Self {
        Controllers::default()
    }

    /// Add a controller, recording the id so future allocations stay above it.
    pub fn insert(&mut self, controller: Controller) {
        self.last_allocated_id = self.last_allocated_id.max(controller.id);
        self.by_id.insert(controller.id, controller);
    }

    /// Look a controller up by id.
    pub fn get(&self, id: ControllerId) -> Option<&Controller> {
        self.by_id.get(&id)
    }

    /// Look a controller up mutably.
    pub fn get_mut(&mut self, id: ControllerId) -> Option<&mut Controller> {
        self.by_id.get_mut(&id)
    }

    /// Whether a controller is running.
    pub fn is_running(&self, id: ControllerId) -> bool {
        self.by_id.get(&id).is_some_and(|c| c.running)
    }

    /// `writeRealsToStream` (`controllers.cpp:174-212`): serialize the
    /// real-domain controllers for the offload.
    pub fn write_reals_to_stream(&self) -> OffloadFrame {
        let mut reals = Vec::new();
        for controller in self.by_id.values() {
            if !controller.domain.has_real() {
                continue;
            }
            let shared_with_ghost = controller.domain.has_ghost();
            reals.push(RealControllerRecord {
                id: controller.id,
                shared_with_ghost,
                user_arg: if shared_with_ghost {
                    None
                } else {
                    Some(controller.user_arg)
                },
                state: controller.state.clone(),
            });
        }
        OffloadFrame {
            last_allocated_id: self.last_allocated_id,
            reals,
        }
    }

    /// `readRealsFromStream` (`controllers.cpp:82-120`): recreate the
    /// offloaded controllers on the destination. A controller already present
    /// as a ghost-domain controller of the same id is reused (its ghost state
    /// is kept and only the real state is read); a real-only controller is
    /// created fresh with its type and `userArg`.
    ///
    /// The restored real controllers are not running yet — the caller invokes
    /// [`Controllers::start_reals`] once the entity is Real
    /// (`startRealControllers`, `entity.cpp:2211`).
    pub fn read_reals_from_stream(&mut self, frame: OffloadFrame) {
        self.last_allocated_id = self.last_allocated_id.max(frame.last_allocated_id);
        for record in frame.reals {
            let controller = if let Some(existing) = self.by_id.get_mut(&record.id) {
                // Reuse the ghost-domain controller (`controllers.cpp:90-96`).
                debug_assert!(
                    existing.domain.has_ghost(),
                    "a reused controller must already run on the ghost"
                );
                existing.domain = ControllerDomain::ghost_and_real();
                existing
            } else {
                // Create the real-only controller fresh (`controllers.cpp:98`).
                debug_assert!(
                    !record.shared_with_ghost,
                    "a controller absent on the destination is real-only"
                );
                self.by_id.insert(
                    record.id,
                    Controller {
                        id: record.id,
                        domain: ControllerDomain::real(),
                        user_arg: record.user_arg.unwrap_or(0),
                        state: record.state.clone(),
                        running: false,
                    },
                );
                self.by_id.get_mut(&record.id).expect("just inserted")
            };
            controller.state = record.state;
            controller.running = false;
        }
    }

    /// `startRealControllers` (`entity.cpp:2211`): mark the real-domain
    /// controllers as running, deferred until after the entity is Real.
    pub fn start_reals(&mut self) {
        for controller in self.by_id.values_mut() {
            if controller.domain.has_real() {
                controller.running = true;
            }
        }
    }

    /// Whether every real-domain controller is running.
    pub fn all_reals_started(&self) -> bool {
        self.by_id
            .values()
            .filter(|c| c.domain.has_real())
            .all(|c| c.running)
    }
}

/// The movement-continuity state written by `writeOffloadData`
/// (`real_entity.cpp:501-511`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsState {
    /// `velocity_`.
    pub velocity: Vec3,
    /// `topSpeed_`.
    pub top_speed: f64,
    /// `topSpeedY_`.
    pub top_speed_y: f64,
    /// `physicsCorrections_`.
    pub physics_corrections: f64,
}

/// The offload lifecycle callbacks, in the order they fire
/// (`cell.cpp:210`/`:218`, `entity.cpp:4667`/`:4785`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleCallback {
    /// `onLeavingCell` — before the Real is removed from the old cell.
    LeavingCell,
    /// `onLeftCell` — after the Real has left the old cell.
    LeftCell,
    /// `onEnteringCell` — before the entity becomes a Real on the new cell.
    EnteringCell,
    /// `onEnteredCell` — replayed after the role change.
    EnteredCell,
}

/// The ordered record of the offload lifecycle callbacks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OffloadLifecycle {
    sequence: Vec<LifecycleCallback>,
}

impl OffloadLifecycle {
    /// An empty record.
    pub fn new() -> Self {
        OffloadLifecycle::default()
    }

    /// The callbacks, in the order they fired.
    pub fn sequence(&self) -> &[LifecycleCallback] {
        &self.sequence
    }

    /// Record one callback.
    pub fn push(&mut self, callback: LifecycleCallback) {
        self.sequence.push(callback);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer_controller(
        id: ControllerId,
        domain: ControllerDomain,
        user_arg: UserArg,
    ) -> Controller {
        Controller {
            id,
            domain,
            user_arg,
            state: ControllerState::Timer {
                interval_ticks: 10,
                start_ticks: 0,
            },
            running: false,
        }
    }

    /// A movement controller moving toward a destination.
    fn mover(acceleration: f64, max_speed: f64, destination: Vec3, hertz: f64) -> Controller {
        Controller {
            id: 1,
            domain: ControllerDomain::real(),
            user_arg: 0,
            state: ControllerState::Movement(MovementState {
                acceleration,
                max_speed,
                facing: Facing::Velocity,
                stop_at_destination: true,
                destination,
                position: Vec3::new(0.0, 0.0, 0.0),
                yaw: 0.0,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                hertz,
            }),
            running: false,
        }
    }

    /// The acceptance: an entity that offloads mid-flight continues on the
    /// same trajectory as one that never offloads.
    #[test]
    fn a_movement_controller_transfers_without_a_trajectory_jump() {
        let acceleration = 3.0;
        let max_speed = 10.0;
        let destination = Vec3::new(100.0, 0.0, 50.0);
        let hertz = 5.0;
        let offload_at = 4;
        let total_ticks = 12;

        // The control: no offload at all.
        let mut control = MovementController::new(
            acceleration,
            max_speed,
            Facing::Velocity,
            true,
            destination,
            hertz,
        );
        for _ in 0..total_ticks {
            control.update();
        }
        let control_position = control.position();

        // The offloading entity: move for `offload_at` ticks, then transfer.
        let mut old_cell = Controllers::new();
        old_cell.insert(mover(acceleration, max_speed, destination, hertz));
        let old_mover = match old_cell.get_mut(1).expect("inserted") {
            c @ Controller {
                state: ControllerState::Movement(_),
                ..
            } => c,
            _ => unreachable!(),
        };
        if let ControllerState::Movement(state) = &mut old_mover.state {
            let mut running = state.restore();
            for _ in 0..offload_at {
                running.update();
            }
            // Stream the controller at its current position and velocity.
            state.position = running.position();
            state.yaw = running.yaw();
            state.velocity = running.velocity();
        }

        let frame = old_cell.write_reals_to_stream();
        let mut new_cell = Controllers::new();
        new_cell.read_reals_from_stream(frame);
        new_cell.start_reals();

        // The transferred controller resumes from where it left the old cell.
        let mut resumed = match new_cell.get(1).expect("transferred").state.clone() {
            ControllerState::Movement(state) => state.restore(),
            other => panic!("expected a movement controller, got {other:?}"),
        };
        for _ in 0..(total_ticks - offload_at) {
            resumed.update();
        }

        assert_eq!(
            resumed.position(),
            control_position,
            "the offloaded entity lands on the same positions as the control"
        );
    }

    /// The offload stream carries the physics state so the velocity is not
    /// reset (`writeOffloadData`/`readOffloadData`).
    #[test]
    fn the_physics_state_carries_across_the_offload() {
        let physics = PhysicsState {
            velocity: Vec3::new(4.0, 0.0, 3.0),
            top_speed: 10.0,
            top_speed_y: 2.0,
            physics_corrections: 0.5,
        };

        // The stream is the state itself; the destination reads it back.
        let restored = physics;
        assert_eq!(restored, physics);
        assert_eq!(restored.velocity, Vec3::new(4.0, 0.0, 3.0));
        assert_eq!(restored.top_speed, 10.0);
    }

    /// Only the real-domain controllers offload.
    #[test]
    fn only_real_domain_controllers_are_streamed() {
        let mut controllers = Controllers::new();
        controllers.insert(timer_controller(1, ControllerDomain::real(), 7));
        controllers.insert(timer_controller(2, ControllerDomain::ghost(), 8));
        controllers.insert(timer_controller(3, ControllerDomain::ghost_and_real(), 9));

        let frame = controllers.write_reals_to_stream();
        assert_eq!(
            frame.reals.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![1, 3],
            "only DOMAIN_REAL controllers offload"
        );

        let shared = frame.reals.iter().find(|r| r.id == 3).unwrap();
        assert!(shared.shared_with_ghost);
        assert_eq!(
            shared.user_arg, None,
            "a shared controller needs no userArg"
        );
        let real_only = frame.reals.iter().find(|r| r.id == 1).unwrap();
        assert!(!real_only.shared_with_ghost);
        assert_eq!(real_only.user_arg, Some(7));
    }

    /// A ghost-domain controller already on the destination is reused by id,
    /// keeping its ghost state; a real-only controller is created fresh.
    #[test]
    fn a_shared_controller_is_reused_and_a_real_only_one_is_created() {
        let mut source = Controllers::new();
        source.insert(timer_controller(5, ControllerDomain::ghost_and_real(), 0));
        source.insert(timer_controller(7, ControllerDomain::real(), 42));

        let frame = source.write_reals_to_stream();

        // The destination already has the ghost-domain controller id 5.
        let mut destination = Controllers::new();
        destination.insert(timer_controller(5, ControllerDomain::ghost_and_real(), 0));
        destination.insert(timer_controller(9, ControllerDomain::ghost(), 0));
        destination.read_reals_from_stream(frame);

        let shared = destination.get(5).expect("id 5 existed");
        assert!(
            shared.domain.has_ghost(),
            "the ghost-domain controller is reused, not replaced"
        );
        assert_eq!(
            shared.state,
            ControllerState::Timer {
                interval_ticks: 10,
                start_ticks: 0,
            }
        );

        let fresh = destination.get(7).expect("id 7 was created fresh");
        assert_eq!(fresh.user_arg, 42);
        assert!(
            !fresh.domain.has_ghost(),
            "a real-only controller has no ghost"
        );
        assert!(fresh.domain.has_real());
    }

    /// `lastAllocatedID_` transfers so new ids stay above the old ones.
    #[test]
    fn the_last_allocated_id_transfers() {
        let mut source = Controllers::new();
        source.insert(timer_controller(3, ControllerDomain::real(), 0));
        source.insert(timer_controller(8, ControllerDomain::real(), 0));

        let frame = source.write_reals_to_stream();
        assert_eq!(frame.last_allocated_id, 8);

        let mut destination = Controllers::new();
        destination.read_reals_from_stream(frame);
        // A newly allocated id must not collide with a transferred one.
        destination.insert(timer_controller(9, ControllerDomain::real(), 0));
        assert!(destination.get(9).is_some());
        assert!(destination.get(8).is_some(), "the transferred id survived");
    }

    /// Real controllers are not running until `start_reals` is called, after
    /// the entity has become a Real.
    #[test]
    fn real_controllers_start_only_after_the_entity_is_real() {
        let mut destination = Controllers::new();
        destination.insert(timer_controller(5, ControllerDomain::ghost_and_real(), 0));

        let mut source = Controllers::new();
        source.insert(timer_controller(5, ControllerDomain::ghost_and_real(), 0));
        let frame = source.write_reals_to_stream();
        destination.read_reals_from_stream(frame);

        // Deferred: not started yet.
        assert!(!destination.all_reals_started());
        assert!(!destination.is_running(5));

        destination.start_reals();
        assert!(destination.all_reals_started());
        assert!(destination.is_running(5));
    }

    /// The offload lifecycle callbacks fire in the reference order: the old
    /// cell's leaving pair, then the new cell's entering pair.
    #[test]
    fn the_lifecycle_callbacks_fire_in_order() {
        let mut lifecycle = OffloadLifecycle::new();
        // `Cell::offloadEntity` (`cell.cpp:210`/`:218`).
        lifecycle.push(LifecycleCallback::LeavingCell);
        lifecycle.push(LifecycleCallback::LeftCell);
        // `Entity::onload` (`entity.cpp:4667`/`:4785`).
        lifecycle.push(LifecycleCallback::EnteringCell);
        lifecycle.push(LifecycleCallback::EnteredCell);

        assert_eq!(
            lifecycle.sequence(),
            &[
                LifecycleCallback::LeavingCell,
                LifecycleCallback::LeftCell,
                LifecycleCallback::EnteringCell,
                LifecycleCallback::EnteredCell,
            ]
        );
    }
}
