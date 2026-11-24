//! Passengers, vehicles and teleport (`server/cellapp/passengers.*`,
//! `passenger_extra.*`, `passenger_controller.*`, plus the vehicle and
//! teleport faces of `entity.cpp` / `real_entity.cpp`).
//!
//! A vehicle keeps a `Passengers` extra whose transform is rebuilt from
//! its global position/direction on every move
//! (`Passengers::adjustTransform`, `passengers.cpp:139-146`); every rider
//! stores a *local* position/direction on the vehicle and recomputes its
//! global coordinates through that transform
//! (`Entity::updateGlobalPosition`, `entity.cpp:7692-7726`). Boarding and
//! alighting pick which side of the pair stays put
//! (`Entity::SetVehicleParam`: `KEEP_LOCAL_POSITION`,
//! `KEEP_GLOBAL_POSITION`, `IN_LIMBO`, `entity.hpp:585-590`), and the
//! same-space teleport transforms the global target into the vehicle's
//! local space before setting it, so the entity lands exactly on the
//! target (`real_entity.cpp:1199-1211`).
//!
//! The BW-08-02-05 acceptance is "父子变换、脱离和目标位置吻合": the
//! parent→child transform rides the passenger with the vehicle, alighting
//! keeps the global position, and a teleport lands exactly on its target
//! whether or not the entity is riding.

use std::collections::BTreeMap;

use foundation::math::angle::normalise as angle_normalise;
use foundation::math::{Matrix, Vector3};
use foundation::stream::{BinaryStream, StreamError};

use crate::controllers::ControllerId;
use crate::entity_dispatch::EntityId;
use crate::movement::{Direction3D, INVALID_POSITION};

/// `Entity::isValidPosition` (`entity.hpp:150-157`): strictly inside
/// ±`MAX_ENTITY_POS` (1e9) per axis, keeping NaN out.
const MAX_ENTITY_POS: f32 = 1000000000.0;

/// `Entity::isValidPosition`.
pub fn is_valid_position(c: Vector3) -> bool {
    -MAX_ENTITY_POS < c.x
        && c.x < MAX_ENTITY_POS
        && -MAX_ENTITY_POS < c.y
        && c.y < MAX_ENTITY_POS
        && -MAX_ENTITY_POS < c.z
        && c.z < MAX_ENTITY_POS
}

/// `Entity::SetVehicleParam` (`entity.hpp:585-590`): which side of the
/// local/global pair a vehicle change keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetVehicleParam {
    /// `KEEP_LOCAL_POSITION`: the local pair stays, global recomputes.
    KeepLocalPosition,
    /// `KEEP_GLOBAL_POSITION`: the global pair stays, local recomputes.
    KeepGlobalPosition,
    /// `IN_LIMBO`: neither (ghost waiting for its vehicle to appear).
    InLimbo,
}

/// `EntityMailBoxRef::Component` — only the CELL face matters to the
/// teleport guard (`real_entity.cpp:1232-1238`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailBoxComponent {
    Cell,
    Other,
}

/// A reduced `EntityMailBoxRef`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailBoxRef {
    /// The referenced entity id (`0` = a same-space position teleport).
    pub id: EntityId,
    /// Which component the mailbox points at.
    pub component: MailBoxComponent,
}

impl MailBoxRef {
    /// A same-space mailbox (`id == 0`): the teleport lands on `position`.
    pub fn same_space() -> Self {
        Self {
            id: 0,
            component: MailBoxComponent::Cell,
        }
    }

    /// A cross-cell "teleport near entity `id`" mailbox.
    pub fn near_cell(id: EntityId) -> Self {
        Self {
            id,
            component: MailBoxComponent::Cell,
        }
    }
}

/// One recorded outcome, in firing order: the script callbacks, the
/// witness notifications, the physics corrections and the warnings.
#[derive(Clone, Debug, PartialEq)]
pub enum PassengerEvent {
    /// `entity_.callback( name )`.
    Callback(EntityId, &'static str),
    /// `pWitness()->vehicleChanged()` — the witness must learn the vehicle
    /// state changed (`entity.cpp:7627-7630`).
    WitnessVehicleChanged(EntityId),
    /// `pReal->sendPhysicsCorrection()`.
    PhysicsCorrection(EntityId),
    /// `population().addObserver( vehicleID, ... )` — a limbo passenger
    /// watching for its vehicle to enter the population
    /// (`passenger_controller.cpp:158-160`).
    WatchingForVehicle(EntityId, EntityId),
    /// `CellApp::offloadEntity( entity, channel, isTeleport = true )` —
    /// the cross-cell teleport hand-off.
    TeleportOffload(EntityId),
    /// A `WARNING_MSG`/`ERROR_MSG`.
    Warning(&'static str),
}

/// `Passengers` (`passengers.hpp/.cpp`): the vehicle-side extra — the
/// global-to-child transform plus the rider list.
#[derive(Clone, Debug)]
pub struct PassengersExtra {
    /// `transform_`: rotate(yaw, pitch, roll) · translation(position)
    /// (`passengers.cpp:139-146`).
    transform: Matrix,
    /// `passengers_`.
    riders: Vec<EntityId>,
    /// `inDestructor_`: the destruction cascade does not clear the extra.
    in_destructor: bool,
}

impl PassengersExtra {
    /// `Passengers::Passengers( Entity & )` — the constructor calls
    /// `adjustTransform` right away (`passengers.cpp:29-33`).
    pub fn new(position: Vector3, direction: Direction3D) -> Self {
        let mut extra = Self {
            transform: Matrix::identity(),
            riders: Vec::new(),
            in_destructor: false,
        };
        extra.adjust_transform(position, direction);
        extra
    }

    /// `Passengers::transform()`.
    pub fn transform(&self) -> &Matrix {
        &self.transform
    }

    /// `passengers_` (for tests and the world's clear face).
    pub fn riders(&self) -> &[EntityId] {
        &self.riders
    }

    /// `Passengers::add` (`passengers.cpp:44-55`).
    pub fn add(&mut self, rider: EntityId) {
        debug_assert!(
            !self.riders.contains(&rider),
            "MF_ASSERT: this entity is not already a passenger"
        );
        self.riders.push(rider);
    }

    /// `Passengers::remove` (`passengers.cpp:57-88`): brute-force find and
    /// erase; the caller clears the extra when the last rider leaves and
    /// the extra is not in its destructor.
    pub fn remove(&mut self, rider: EntityId) -> bool {
        if let Some(index) = self.riders.iter().position(|id| *id == rider) {
            self.riders.remove(index);
            true
        } else {
            // WARNING_MSG( "Passengers::remove: Failed to remove ..." )
            false
        }
    }

    /// `passengers_.empty()` — the extra clears itself when true.
    pub fn is_empty(&self) -> bool {
        self.riders.is_empty()
    }

    /// Mark the destructor face (`~Passengers`), so the clear is skipped.
    pub fn enter_destructor(&mut self) {
        self.in_destructor = true;
    }

    /// `Passengers::transformToVehiclePos` (`passengers.cpp:90-96`):
    /// input global position, output local on the vehicle space.
    pub fn transform_to_vehicle_pos(&self, pos: &mut Vector3) {
        let mut inverse = self.transform;
        inverse.invert();
        *pos = inverse.apply_point(pos);
    }

    /// `Passengers::transformToVehicleDir` (`passengers.cpp:98-104`):
    /// "TODO: What do we do here? For now, just doing yaw" — the yaw
    /// difference, wrapped into `(-π, π]`.
    pub fn transform_to_vehicle_dir(&self, dir: &mut Direction3D, vehicle_yaw: f32) {
        dir.yaw = angle_normalise(dir.yaw - vehicle_yaw);
    }

    /// `Passengers::updateInternalsForNewPosition`: the answers the
    /// changed-transform faces need — rebuilt transform, then every rider
    /// gets `onVehicleMove`.
    pub fn update_internals_for_new_position(&mut self, position: Vector3, direction: Direction3D) {
        self.adjust_transform(position, direction);
    }

    /// `Passengers::adjustTransform` (`passengers.cpp:139-146`).
    fn adjust_transform(&mut self, position: Vector3, direction: Direction3D) {
        self.transform
            .set_rotate_ypr(direction.yaw, direction.pitch, direction.roll);
        self.transform.translation(&position);
    }
}

/// The `PassengerExtra` state (`passenger_extra.hpp`): the controller
/// that keeps the rider on its vehicle.
#[derive(Clone, Debug)]
pub struct RiderExtra {
    /// `pController_`.
    pub controller_id: Option<ControllerId>,
    /// The vehicle this rider boarded (`PassengerController::vehicleID_`).
    pub vehicle_id: EntityId,
}

/// The entity slice the vehicle/teleport state machine reads and writes:
/// the local/global position-direction pair, the vehicle link, the
/// range-list flag (`removalHandle_ != NO_SPACE_REMOVAL_HANDLE`), and the
/// witness/client flags the physics correction branches on.
#[derive(Clone, Debug)]
pub struct EntityBody {
    /// `id_`.
    pub id: EntityId,
    /// `pReal_ != NULL`.
    pub is_real: bool,
    /// `localPosition_` / `localDirection_`.
    pub local_position: Vector3,
    pub local_direction: Direction3D,
    /// `globalPosition_` / `globalDirection_`.
    pub global_position: Vector3,
    pub global_direction: Direction3D,
    /// `pVehicle_`.
    pub vehicle: Option<EntityId>,
    /// `vehicleChangeNum_`.
    pub vehicle_change_num: u32,
    /// `removalHandle_ != NO_SPACE_REMOVAL_HANDLE` — the entity sits in
    /// the range list.
    pub in_range_list: bool,
    /// `pReal() && pReal()->pWitness()`.
    pub witness: bool,
    /// `pReal->controlledByRef().id != 0`.
    pub controlled_by_client: bool,
}

impl EntityBody {
    /// A real entity at a position (`global == local`, on the ground of
    /// the range list by default — the common boarding shape).
    pub fn real(id: EntityId, position: Vector3, direction: Direction3D) -> Self {
        Self {
            id,
            is_real: true,
            local_position: position,
            local_direction: direction,
            global_position: position,
            global_direction: direction,
            vehicle: None,
            vehicle_change_num: 0,
            in_range_list: true,
            witness: false,
            controlled_by_client: false,
        }
    }

    /// A ghost: not in the range list yet, which is the branch
    /// `setVehicle`'s `KEEP_GLOBAL_POSITION` fast path serves
    /// (`entity.cpp:7631-7646`).
    pub fn ghost(id: EntityId, local_position: Vector3, local_direction: Direction3D) -> Self {
        Self {
            id,
            is_real: false,
            local_position,
            local_direction,
            global_position: INVALID_POSITION,
            global_direction: Direction3D::default(),
            vehicle: None,
            vehicle_change_num: 0,
            in_range_list: false,
            witness: false,
            controlled_by_client: false,
        }
    }
}

/// The world: the entity population plus the two extras, standing in for
/// `CellApp::instance().findEntity`, `Passengers::instance` and
/// `PassengerExtra::instance`.
#[derive(Default)]
pub struct PassengerWorld {
    entities: BTreeMap<EntityId, EntityBody>,
    vehicles: BTreeMap<EntityId, PassengersExtra>,
    riders: BTreeMap<EntityId, RiderExtra>,
    pub events: Vec<PassengerEvent>,
}

impl PassengerWorld {
    pub fn new() -> Self {
        Self::default()
    }

    /// `CellApp::instance().findEntity( id ) != NULL` — insert the body.
    pub fn add_entity(&mut self, body: EntityBody) {
        self.entities.insert(body.id, body);
    }

    /// `Entity::population()` lookup.
    pub fn entity(&self, id: EntityId) -> Option<&EntityBody> {
        self.entities.get(&id)
    }

    /// Mutable lookup (module-private uses).
    fn entity_mut(&mut self, id: EntityId) -> Option<&mut EntityBody> {
        self.entities.get_mut(&id)
    }

    /// `Passengers::instance.exists( vehicle )` — the vehicle has riders.
    pub fn has_passengers_extra(&self, vehicle: EntityId) -> bool {
        self.vehicles.contains_key(&vehicle)
    }

    /// `Passengers::instance( vehicle ).transform()`.
    pub fn vehicle_transform(&self, vehicle: EntityId) -> Option<&Matrix> {
        self.vehicles.get(&vehicle).map(|extra| extra.transform())
    }

    /// `PassengerExtra::isInLimbo` (`passenger_extra.cpp:169-173`): no
    /// vehicle, extra exists, controller set.
    pub fn is_in_limbo(&self, id: EntityId) -> bool {
        self.entities
            .get(&id)
            .is_some_and(|body| body.vehicle.is_none())
            && self
                .riders
                .get(&id)
                .is_some_and(|rider| rider.controller_id.is_some())
    }

    /// `Entity::setVehicle` (`entity.cpp:7606-7660`).
    pub fn set_vehicle(&mut self, id: EntityId, vehicle: Option<EntityId>, keep: SetVehicleParam) {
        if self.entities.get(&id).map(|b| b.vehicle) == Some(vehicle) {
            return;
        }
        let Some(body) = self.entity_mut(id) else {
            return;
        };
        body.vehicle = vehicle;
        body.vehicle_change_num += 1;

        match keep {
            SetVehicleParam::KeepGlobalPosition => {
                if body.in_range_list {
                    // "if we're in the range list already then keeping the
                    // global position is a fine idea"
                    let witness = body.is_real && body.witness;
                    if witness {
                        self.events.push(PassengerEvent::WitnessVehicleChanged(id));
                    }
                    self.update_local_position(id);
                } else if let Some(vehicle_id) = vehicle {
                    // "... a ghost that has just been created - our local
                    // position is already correct but our global position
                    // is wrong": compute inline, no range-list update.
                    let (vehicle_yaw, transform) = {
                        let extra = self
                            .vehicles
                            .get(&vehicle_id)
                            .expect("MF_ASSERT( Passengers::instance.exists )");
                        (
                            self.entities[&vehicle_id].global_direction.yaw,
                            extra.transform,
                        )
                    };
                    let body = self.entity_mut(id).expect("checked above");
                    body.global_position = transform.apply_point(&body.local_position);
                    body.global_direction.yaw =
                        angle_normalise(body.local_direction.yaw + vehicle_yaw);
                } else {
                    // "this is a ghost that has suddenly become... hmm." —
                    // no vehicle to pull the transform from: copy the
                    // local pair across.
                    let body = self.entity_mut(id).expect("checked above");
                    body.global_position = body.local_position;
                    body.global_direction = body.local_direction;
                }
            }
            SetVehicleParam::KeepLocalPosition => {
                self.update_global_position(id, false, true);
            }
            SetVehicleParam::InLimbo => {
                debug_assert!(!body.is_real, "MF_ASSERT( !this->isReal() )");
            }
        }

        // `this->onPositionChanged()` — the AOI face is BW-08-03.
    }

    /// `Entity::updateLocalPosition` (`entity.cpp:7663-7685`): global
    /// stays, local recomputes. Off a vehicle the pair copies; on one, the
    /// inverse transform applies and only the yaw difference is taken.
    fn update_local_position(&mut self, id: EntityId) {
        let vehicle = self.entities.get(&id).and_then(|b| b.vehicle);
        let Some(vehicle) = vehicle else {
            let Some(body) = self.entity_mut(id) else {
                return;
            };
            body.local_position = body.global_position;
            body.local_direction = body.global_direction;
            return;
        };
        let (mut transform, vehicle_yaw) = {
            let extra = self
                .vehicles
                .get(&vehicle)
                .expect("MF_ASSERT( Passengers::instance.exists )");
            (
                extra.transform,
                self.entities[&vehicle].global_direction.yaw,
            )
        };
        transform.invert();
        let Some(body) = self.entity_mut(id) else {
            return;
        };
        body.local_position = transform.apply_point(&body.global_position);
        // TODO: What do we do here? For now, just doing yaw.
        body.local_direction.yaw = angle_normalise(body.global_direction.yaw - vehicle_yaw);
    }

    /// `Entity::updateGlobalPosition` (`entity.cpp:7688-7740`). Answers
    /// `isInGlobalCoords`.
    pub fn update_global_position(
        &mut self,
        id: EntityId,
        _should_update_ghosts: bool,
        _is_vehicle_movement: bool,
    ) -> bool {
        let (on_vehicle, in_limb0) = {
            let Some(body) = self.entities.get(&id) else {
                return false;
            };
            let limbo = body.vehicle.is_none() && self.is_in_limbo(id);
            (body.vehicle, limbo)
        };

        let mut in_global_coords = false;
        if on_vehicle.is_none() && !in_limb0 {
            let body = self.entity_mut(id).expect("checked above");
            body.global_position = body.local_position;
            body.global_direction = body.local_direction;
            in_global_coords = true;
        } else if let Some(vehicle) = on_vehicle {
            let (transform, vehicle_yaw) = {
                let extra = self
                    .vehicles
                    .get(&vehicle)
                    .expect("MF_ASSERT( Passengers::instance.exists )");
                (
                    extra.transform,
                    self.entities[&vehicle].global_direction.yaw,
                )
            };
            let body = self.entity_mut(id).expect("checked above");
            body.global_position = transform.apply_point(&body.local_position);
            // TODO: What do we do here? For now, just doing yaw.
            body.global_direction.yaw = angle_normalise(body.local_direction.yaw + vehicle_yaw);
        } else {
            // INFO_MSG: "Not updating position since entity is in limbo"
            self.events.push(PassengerEvent::Warning(
                "Entity::updateGlobalPosition: entity is in limbo",
            ));
        }

        if !in_limb0 {
            // The range-list reseat (`updateInternalsForNewPosition(OfReal)`)
            // reduces here to the passengers cascade: the vehicle's
            // transform is rebuilt and every rider re-derives its global
            // pair (`Passengers::updateInternalsForNewPosition`,
            // `passengers.cpp:106-121`).
            if self.vehicles.contains_key(&id) {
                let rider_ids = {
                    let (position, direction) = {
                        let body = &self.entities[&id];
                        (body.global_position, body.global_direction)
                    };
                    let extra = self.vehicles.get_mut(&id).expect("checked above");
                    extra.update_internals_for_new_position(position, direction);
                    extra.riders().to_vec()
                };
                for rider in rider_ids {
                    self.on_vehicle_move(rider);
                }
            }
        }

        // `g_entityMovementCallback` is the profile hook — no port face.
        in_global_coords
    }

    /// `Entity::onVehicleMove` (`entity.cpp:7743-7750`): the vehicle
    /// moved, re-derive the global pair from the untouched local pair.
    fn on_vehicle_move(&mut self, id: EntityId) {
        debug_assert!(
            self.entities.get(&id).is_some_and(|b| b.vehicle.is_some()),
            "MF_ASSERT( pVehicle_ )"
        );
        self.update_global_position(id, false, true);
    }

    /// `Entity::setPositionAndDirection` (`entity.cpp:7752-7789`): the
    /// local pair set, then the global pair (and the cascade) follows.
    /// Answers false for an invalid position (the reference errors and
    /// returns).
    pub fn set_position_and_direction(
        &mut self,
        id: EntityId,
        position: Vector3,
        direction: Direction3D,
    ) -> bool {
        debug_assert!(
            self.entities.get(&id).is_some_and(|b| b.is_real),
            "MF_ASSERT( this->isReal() )"
        );
        if !is_valid_position(position) {
            // ERROR_MSG "... is not a valid position"
            self.events.push(PassengerEvent::Warning(
                "Entity::setPositionAndDirection: not a valid position",
            ));
            return false;
        }
        let Some(body) = self.entity_mut(id) else {
            return false;
        };
        body.local_position = position;
        body.local_direction = direction;
        self.update_global_position(id, true, false);
        true
    }

    /// `PassengerExtra::boardVehicle` (`passenger_extra.cpp:95-163`).
    pub fn board_vehicle(
        &mut self,
        rider: EntityId,
        vehicle_id: EntityId,
        should_update_client: bool,
    ) -> Result<(), &'static str> {
        if !self.entities.get(&rider).is_some_and(|b| b.is_real) {
            return Err("This method may only be called on a real");
        }
        if self
            .riders
            .get(&rider)
            .is_some_and(|r| r.controller_id.is_some())
        {
            return Err("Already on a vehicle");
        }
        if !self.entities.contains_key(&vehicle_id) {
            // WARNING_MSG "... is trying to board non-existent entity ..."
            self.events.push(PassengerEvent::Warning(
                "PassengerExtra::boardVehicle: trying to board non-existent entity",
            ));
            return Err("Entity.boardVehicle: No entity id");
        }

        // "make sure that the potential vehicle is not already one of our
        // passengers!" — walk the pVehicle chain.
        let mut walker = Some(vehicle_id);
        while let Some(current) = walker {
            if current == rider {
                self.events.push(PassengerEvent::Warning(
                    "PassengerExtra::boardVehicle: trying to board its own passenger",
                ));
                return Err("Entity.boardVehicle: Cannot board own passenger");
            }
            walker = self.entities.get(&current).and_then(|b| b.vehicle);
        }

        // Make sure this vehicle is set up with Passengers extension.
        if !self.vehicles.contains_key(&vehicle_id) {
            let (position, direction) = {
                let body = &self.entities[&vehicle_id];
                (body.global_position, body.global_direction)
            };
            self.vehicles
                .insert(vehicle_id, PassengersExtra::new(position, direction));
        }
        self.set_vehicle(rider, Some(vehicle_id), SetVehicleParam::KeepGlobalPosition);
        // The controller the reference adds here runs `startGhost` even on
        // a real, and that is what joins the `Passengers` list:
        // `Passengers::instance( *pVehicle ).add( entity.id() )`.
        self.vehicles
            .get_mut(&vehicle_id)
            .expect("just ensured")
            .add(rider);

        // `entity_.addController( new PassengerController( vehicleID ), 0 )`.
        let controller_id = self.allocate_controller_id();
        self.riders.insert(
            rider,
            RiderExtra {
                controller_id: Some(controller_id),
                vehicle_id,
            },
        );

        // `pReal->controlledByRef().id != 0 && shouldUpdateClient`.
        if self.entities[&rider].controlled_by_client && should_update_client {
            self.events.push(PassengerEvent::PhysicsCorrection(rider));
        }
        Ok(())
    }

    /// `PassengerExtra::alightVehicle` (`passenger_extra.cpp:181-217`):
    /// `delController` runs the ghost stop faces — leave the list, keep
    /// the global position.
    pub fn alight_vehicle(
        &mut self,
        rider: EntityId,
        should_update_client: bool,
    ) -> Result<(), &'static str> {
        if !self
            .riders
            .get(&rider)
            .is_some_and(|r| r.controller_id.is_some())
        {
            return Err("Not on a vehicle");
        }
        if !self.entities.get(&rider).is_some_and(|b| b.is_real) {
            return Err("This method may only be called on a real");
        }

        self.stop_rider_controller(rider);

        if self.entities[&rider].controlled_by_client && should_update_client {
            self.events.push(PassengerEvent::PhysicsCorrection(rider));
        }
        Ok(())
    }

    /// `PassengerController::stopGhost` (`passenger_controller.cpp:120-150`):
    /// the controller stop that leaves the vehicle behind.
    fn stop_rider_controller(&mut self, rider: EntityId) {
        let Some(vehicle) = self.entities.get(&rider).and_then(|b| b.vehicle) else {
            // DEBUG_MSG: "Passenger was in limbo for vehicle id ..." — the
            // observer detaches; the position note is the reference's own
            // comment.
            self.riders.remove(&rider);
            return;
        };

        if self.vehicles.contains_key(&vehicle) {
            debug_assert!(
                self.vehicles[&vehicle].riders().contains(&rider),
                "MF_VERIFY( Passengers::instance( *pVehicle ).remove( entity ) )"
            );
            self.vehicles
                .get_mut(&vehicle)
                .expect("checked")
                .remove(rider);
            self.drop_empty_extra(vehicle);
        }
        self.set_vehicle(rider, None, SetVehicleParam::KeepGlobalPosition);
        // "We no longer need the passenger extra."
        self.riders.remove(&rider);
    }

    /// `Passengers::remove`'s self-clear: the extra goes away when the
    /// last rider leaves.
    fn drop_empty_extra(&mut self, vehicle: EntityId) {
        let empty = self
            .vehicles
            .get(&vehicle)
            .is_some_and(|extra| extra.is_empty());
        if empty && !self.vehicles[&vehicle].in_destructor_face() {
            self.vehicles.remove(&vehicle);
        }
    }

    /// `PassengerExtra::onVehicleGone` (`passenger_extra.cpp:220-231`):
    /// route through the controller.
    pub fn on_vehicle_gone(&mut self, rider: EntityId) {
        debug_assert!(
            self.riders.contains_key(&rider),
            "MF_ASSERT( pController_ )"
        );
        let is_real = self.entities.get(&rider).is_some_and(|b| b.is_real);
        if is_real {
            // ERROR_MSG "Real entity alighting since it has lost its
            // vehicle" — "not much that we can do about it if it fails".
            self.events.push(PassengerEvent::Warning(
                "PassengerExtra::onVehicleGone: Real entity alighting since it has lost its vehicle",
            ));
            if let Err(message) = self.alight_vehicle(rider, true) {
                // PyErr_Print(); PyErr_Clear();
                self.events.push(PassengerEvent::Warning(message));
            }
        } else {
            let vehicle = self.entities.get(&rider).and_then(|b| b.vehicle);
            if let Some(vehicle) = vehicle {
                debug_assert!(
                    self.vehicles.contains_key(&vehicle),
                    "MF_ASSERT( Passengers::instance.exists( *pVehicle ) )"
                );
                self.vehicles
                    .get_mut(&vehicle)
                    .expect("checked")
                    .remove(rider);
                self.drop_empty_extra(vehicle);
            }
            // "The passenger is now in limbo"; `setVehicle( NULL, IN_LIMBO )`
            // is a no-op when it is already off any vehicle.
            self.set_vehicle(rider, None, SetVehicleParam::InLimbo);
            let observer_vehicle = self.riders[&rider].vehicle_id;
            self.events
                .push(PassengerEvent::WatchingForVehicle(rider, observer_vehicle));
        }
    }

    /// `~Passengers` (`passengers.cpp:35-52`): the vehicle is going away
    /// — every rider is forced to alight.
    pub fn vehicle_destroyed(&mut self, vehicle: EntityId) {
        let Some(extra) = self.vehicles.get_mut(&vehicle) else {
            return;
        };
        extra.enter_destructor();
        let mut doomed: Vec<EntityId> = extra.riders().to_vec();
        while let Some(rider) = doomed.pop() {
            if self.riders.contains_key(&rider) {
                // "it will remove itself from our list"
                self.on_vehicle_gone(rider);
            } else {
                // CRITICAL_MSG "PassengerExtra does not exist"
                self.events.push(PassengerEvent::Warning(
                    "Passengers::~Passengers: PassengerExtra does not exist",
                ));
            }
        }
        self.vehicles.remove(&vehicle);
    }

    /// `Entity::teleport` (`entity.cpp:5201-5221`) + `RealEntity::teleport`
    /// (`real_entity.cpp:1183-1282`): the script `Entity.teleport`
    /// face. `mailbox.id == 0` is the same-space position teleport; a
    /// mailbox with an id teleports near the referenced entity (the
    /// cross-cell hand-off ends in an offload).
    pub fn teleport(
        &mut self,
        id: EntityId,
        mailbox: MailBoxRef,
        position: Vector3,
        direction: Direction3D,
    ) -> Result<(), &'static str> {
        // `MF_ASSERT( pReal_ )` — Entity::teleport is real-only.
        debug_assert!(
            self.entities.get(&id).is_some_and(|b| b.is_real),
            "MF_ASSERT( pReal_ )"
        );
        // `pThis->callback( "onTeleport" )`.
        self.events.push(PassengerEvent::Callback(id, "onTeleport"));

        // Make sure the input position is valid: a real target position,
        // or the INVALID sentinel with a real mailbox.
        let valid =
            is_valid_position(position) || (position == INVALID_POSITION && mailbox.id != 0);
        if !valid {
            return Err("Entity.teleport() Cannot teleport to position");
        }

        if mailbox.id == 0 {
            // Same space: land exactly on the target.
            let mut local_position = position;
            let mut local_direction = direction;

            // A rider transforms the global target onto its vehicle.
            if let Some(vehicle) = self.entities[&id].vehicle {
                let vehicle_yaw = self.entities[&vehicle].global_direction.yaw;
                let extra = self
                    .vehicles
                    .get(&vehicle)
                    .expect("MF_ASSERT( Passengers::instance.exists )");
                extra.transform_to_vehicle_pos(&mut local_position);
                extra.transform_to_vehicle_dir(&mut local_direction, vehicle_yaw);
            }

            self.set_position_and_direction(id, local_position, local_direction);
            self.on_teleport_success(id, None);
            // (The volatile detailed-position history face is the client
            // sync lane.)
            if self.entities[&id].controlled_by_client {
                self.events.push(PassengerEvent::PhysicsCorrection(id));
            }
            return Ok(());
        }

        // Cross cell.
        if mailbox.component != MailBoxComponent::Cell {
            return Err("Entity.teleport() Cannot teleport near to non-cell entities");
        }
        if self.vehicles.contains_key(&id) {
            return Err(
                "Entity.teleport() Cannot teleport vehicles. Consider just setting position if not changing spaces",
            );
        }
        // (The CellAppChannels lookup is the Mercury lane; the port has no
        // channel registry to refuse against.)

        // `entity().relocated()` — the onload notification face.
        self.events.push(PassengerEvent::Callback(id, "relocated"));

        // Reset the vehicle: "for normal NPC teleported to another
        // vehicle, script should make sure another boardVehicle is
        // called."
        if self.entities[&id].vehicle.is_some() {
            let _ = self.alight_vehicle(id, true);
            // PyErr_Clear() on failure.
        }

        // "Save our old pos and dir and temporarily set to the teleport
        // ones" — the destination cell reads them from the ghost data.
        {
            let body = self.entity_mut(id).expect("checked above");
            body.local_position = position;
            body.local_direction = direction;
        }

        // (Haunt deletion is the ghost lane.)

        // `entity_.callback( "onLeavingCell" )`.
        self.events
            .push(PassengerEvent::Callback(id, "onLeavingCell"));

        // writeGhostDataToStream + onloadTeleportedEntity + offload.
        self.events.push(PassengerEvent::TeleportOffload(id));
        Ok(())
    }

    /// `Entity::onTeleportSuccess` (`entity.cpp:5226-5249`): the callback
    /// with the nearby entity (or None); an unset position falls back to
    /// the nearby entity's position.
    pub fn on_teleport_success(&mut self, id: EntityId, nearby: Option<EntityId>) {
        self.events
            .push(PassengerEvent::Callback(id, "onTeleportSuccess"));
        if let Some(nearby) = nearby {
            let fallback = self.entities[&id].global_position == INVALID_POSITION;
            if fallback {
                // WARNING_MSG "Entity position was not set in
                // onTeleportSuccess. Setting to position of nearby entity"
                self.events.push(PassengerEvent::Warning(
                    "Entity::onTeleportSuccess: position was not set; using the nearby entity",
                ));
                let nearby_position = self.entities[&nearby].global_position;
                let body = self.entity_mut(id).expect("checked above");
                body.global_position = nearby_position;
            }
        }
    }

    /// The controller-id face of `Entity::addController` (`controllers.rs`
    /// owns the real registry; this world counts its own).
    fn allocate_controller_id(&mut self) -> ControllerId {
        let next = self
            .riders
            .values()
            .filter_map(|rider| rider.controller_id)
            .max()
            .unwrap_or(0);
        next + 1
    }

    /// `PassengerController::startGhost`
    /// (`passenger_controller.cpp:94-118`), driven with the world.
    pub fn start_ghost_controller(
        &mut self,
        rider: EntityId,
        controller: &mut PassengerController,
    ) {
        // `PassengerExtra::instance( entity ).setController( this )`.
        self.riders.insert(
            rider,
            RiderExtra {
                controller_id: Some(0),
                vehicle_id: controller.vehicle_id,
            },
        );

        let vehicle_found = self.entities.contains_key(&controller.vehicle_id);
        if controller.initial_local_position != INVALID_POSITION {
            // `setLocalPositionAndDirection` — the local pair set, the
            // global pair follows.
            let (position, direction) = (
                controller.initial_local_position,
                controller.initial_local_direction,
            );
            let body = self.entity_mut(rider).expect("rider exists");
            body.local_position = position;
            body.local_direction = direction;
            self.update_global_position(rider, false, false);
        }

        if vehicle_found {
            let vehicle = controller.vehicle_id;
            if !self.vehicles.contains_key(&vehicle) {
                let (position, direction) = {
                    let body = &self.entities[&vehicle];
                    (body.global_position, body.global_direction)
                };
                self.vehicles
                    .insert(vehicle, PassengersExtra::new(position, direction));
            }
            self.vehicles
                .get_mut(&vehicle)
                .expect("just inserted")
                .add(rider);
            self.set_vehicle(rider, Some(vehicle), SetVehicleParam::KeepLocalPosition);
        } else {
            // WARNING_MSG "Could not find vehicle ..."
            self.events.push(PassengerEvent::Warning(
                "PassengerController::startGhost: Could not find vehicle",
            ));
            debug_assert!(
                !self.entities[&rider].is_real,
                "MF_ASSERT( !this->entity().isReal() )"
            );
            if controller.initial_global_position != INVALID_POSITION
                && !self.entities[&rider].in_range_list
            {
                let (position, direction) = (
                    controller.initial_global_position,
                    controller.initial_global_direction,
                );
                let body = self.entity_mut(rider).expect("rider exists");
                body.global_position = position;
                body.global_direction = direction;
            }
            self.on_vehicle_gone(rider);
        }
    }

    /// `PassengerController::stopGhost`
    /// (`passenger_controller.cpp:120-150`).
    pub fn stop_ghost_controller(&mut self, rider: EntityId) {
        debug_assert!(
            self.riders.contains_key(&rider),
            "MF_ASSERT( PassengerExtra::instance.exists )"
        );
        self.stop_rider_controller(rider);
    }
}

/// `PassengerController` (`passenger_controller.hpp`): the ghost-domain
/// controller that holds the vehicle link across the ghost stream.
#[derive(Clone, Debug)]
pub struct PassengerController {
    /// `vehicleID_`.
    pub vehicle_id: EntityId,
    /// `initialLocalPosition_` / `initialLocalDirection_` (read side).
    pub initial_local_position: Vector3,
    pub initial_local_direction: Direction3D,
    /// `initialGlobalPosition_` / `initialGlobalDirection_` (read side).
    pub initial_global_position: Vector3,
    pub initial_global_direction: Direction3D,
}

impl PassengerController {
    /// `PassengerController::PassengerController( vehicleID )` — the
    /// initial positions start at `INVALID_POSITION`
    /// (`passenger_controller.cpp:32-39`).
    pub fn new(vehicle_id: EntityId) -> Self {
        Self {
            vehicle_id,
            initial_local_position: INVALID_POSITION,
            initial_local_direction: Direction3D::default(),
            initial_global_position: INVALID_POSITION,
            initial_global_direction: Direction3D::default(),
        }
    }

    /// `writeGhostToStream` (`passenger_controller.cpp:41-55`): the
    /// vehicle id, then the entity's local and global pairs.
    pub fn write_ghost_to_stream(&self, body: &EntityBody, stream: &mut BinaryStream) {
        stream.write_u32(self.vehicle_id);
        stream.write_f32(body.local_position.x);
        stream.write_f32(body.local_position.y);
        stream.write_f32(body.local_position.z);
        stream.write_f32(body.local_direction.yaw);
        stream.write_f32(body.local_direction.pitch);
        stream.write_f32(body.local_direction.roll);
        stream.write_f32(body.global_position.x);
        stream.write_f32(body.global_position.y);
        stream.write_f32(body.global_position.z);
        stream.write_f32(body.global_direction.yaw);
        stream.write_f32(body.global_direction.pitch);
        stream.write_f32(body.global_direction.roll);
    }

    /// `readGhostFromStream` (`passenger_controller.cpp:57-70`).
    pub fn read_ghost_from_stream(&mut self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.vehicle_id = stream.read_u32()?;
        self.initial_local_position =
            Vector3::new(stream.read_f32()?, stream.read_f32()?, stream.read_f32()?);
        self.initial_local_direction =
            Direction3D::new(stream.read_f32()?, stream.read_f32()?, stream.read_f32()?);
        self.initial_global_position =
            Vector3::new(stream.read_f32()?, stream.read_f32()?, stream.read_f32()?);
        self.initial_global_direction =
            Direction3D::new(stream.read_f32()?, stream.read_f32()?, stream.read_f32()?);
        Ok(())
    }
}

impl PassengersExtra {
    /// The `inDestructor_` face for the world's clear check.
    fn in_destructor_face(&self) -> bool {
        self.in_destructor
    }
}

// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(yaw: f32) -> Direction3D {
        Direction3D::new(yaw, 0.0, 0.0)
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn approx_v(a: Vector3, b: Vector3) -> bool {
        approx(a.x, b.x) && approx(a.y, b.y) && approx(a.z, b.z)
    }

    fn approx_dir(a: Direction3D, b: Direction3D) -> bool {
        approx(a.yaw, b.yaw) && approx(a.pitch, b.pitch) && approx(a.roll, b.roll)
    }

    // -------------------------------------------------------------------
    // TDD: BW-08-02-05 处理乘客和传送 — 父子变换、脱离和目标位置吻合
    // -------------------------------------------------------------------

    /// The parent→child transform: a rider keeps its local pair through a
    /// vehicle translation *and* rotation, and `onVehicleMove` re-derives
    /// the global pair through the rebuilt transform.
    #[test]
    fn a_rider_rides_the_vehicle_transform() {
        let mut world = PassengerWorld::new();
        // The vehicle sits at the origin facing +x... facing yaw 0 with a
        // pitch so the transform is non-trivial.
        let vehicle_pos = Vector3::new(100.0, 50.0, 200.0);
        let vehicle_dir = Direction3D::new(0.5, 0.25, 0.0);
        world.add_entity(EntityBody::real(1, vehicle_pos, vehicle_dir));

        // The rider boards while parked: KEEP_GLOBAL means the rider's
        // global pair stays and the local pair is derived from it. It has
        // a witness and a controlling client, so the board notifies both.
        let rider_pos = Vector3::new(103.0, 50.0, 202.0);
        let mut rider = EntityBody::real(2, rider_pos, dir(0.3));
        rider.witness = true;
        rider.controlled_by_client = true;
        world.add_entity(rider);
        world.board_vehicle(2, 1, true).expect("boards");
        assert_eq!(
            world.events,
            vec![
                PassengerEvent::WitnessVehicleChanged(2),
                PassengerEvent::PhysicsCorrection(2),
            ]
        );

        // The local pair is the inverse transform of the global pair.
        let transform = *world.vehicle_transform(1).expect("extra exists");
        let mut inverse = transform;
        inverse.invert();
        let body = world.entity(2).unwrap();
        assert!(approx_v(
            body.local_position,
            inverse.apply_point(&rider_pos)
        ));
        assert!(approx(
            body.local_direction.yaw,
            angle_normalise(0.3 - vehicle_dir.yaw)
        ));
        assert_eq!(body.vehicle_change_num, 1);

        // The vehicle drives 10 metres forward and turns: the rider rides
        // along (global recomputed through the new transform).
        world.set_position_and_direction(
            1,
            Vector3::new(110.0, 50.0, 200.0),
            Direction3D::new(0.8, 0.25, 0.0),
        );
        let body = world.entity(2).unwrap();
        let transform = *world.vehicle_transform(1).unwrap();
        assert!(
            approx_v(
                body.global_position,
                transform.apply_point(&body.local_position)
            ),
            "the rider's global pair follows the vehicle"
        );
        assert!(approx(
            body.global_direction.yaw,
            angle_normalise(body.local_direction.yaw + 0.8)
        ));
        // The vehicle keeps its own pair.
        let vehicle_body = world.entity(1).unwrap();
        assert!(approx_v(
            vehicle_body.global_position,
            Vector3::new(110.0, 50.0, 200.0)
        ));

        // The witness learns of the change and the client rides along in
        // the physics correction stream.
        world.events.clear();
        world.alight_vehicle(2, true).expect("alights");
        assert_eq!(
            world.events,
            vec![
                PassengerEvent::WitnessVehicleChanged(2),
                PassengerEvent::PhysicsCorrection(2),
            ]
        );
    }

    /// The transform helpers round-trip: `transformToVehiclePos` is the
    /// inverse of the vehicle transform applied to a global point, and the
    /// yaw difference wraps into (-π, π].
    #[test]
    fn the_transform_helpers_round_trip() {
        let mut extra = PassengersExtra::new(
            Vector3::new(10.0, 20.0, 30.0),
            Direction3D::new(2.5, -0.4, 0.3),
        );
        extra.update_internals_for_new_position(
            Vector3::new(10.0, 20.0, 30.0),
            Direction3D::new(2.5, -0.4, 0.3),
        );

        let global = Vector3::new(12.0, 18.0, 33.0);
        let mut local = global;
        extra.transform_to_vehicle_pos(&mut local);
        // Round trip through the forward transform.
        assert!(approx_v(extra.transform().apply_point(&local), global));

        // A yaw difference over the seam wraps.
        let mut yaw_dir = dir(3.0);
        extra.transform_to_vehicle_dir(&mut yaw_dir, -3.0);
        assert!(approx(yaw_dir.yaw, angle_normalise(6.0)));
        assert!(yaw_dir.yaw <= std::f32::consts::PI);

        // Removing an unknown rider warns and answers false.
        assert!(!extra.remove(99));
        assert!(extra.is_empty());
    }

    /// Alighting keeps the global position: the rider leaves the list, its
    /// local pair re-derives from the untouched global pair, and the
    /// vehicle's extra clears itself when the last rider leaves.
    #[test]
    fn alighting_keeps_the_global_position() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(1, Vector3::new(0.0, 0.0, 0.0), dir(0.25)));
        world.add_entity(EntityBody::real(2, Vector3::new(1.0, 0.0, 0.0), dir(0.0)));

        world.board_vehicle(2, 1, true).expect("boards");
        let global_before = world.entity(2).unwrap().global_position;
        assert!(world.has_passengers_extra(1));

        // The vehicle moves under the rider first.
        world.set_position_and_direction(1, Vector3::new(5.0, 0.0, 0.0), dir(0.25));
        let global_riding = world.entity(2).unwrap().global_position;
        assert!(!approx_v(global_before, global_riding), "the rider moved");

        world.alight_vehicle(2, true).expect("alights");
        let body = world.entity(2).unwrap();
        assert_eq!(body.vehicle, None);
        assert_eq!(body.vehicle_change_num, 2);
        // The global pair never moved during the alight.
        assert!(approx_v(body.global_position, global_riding));
        // The local pair re-derived from the global pair (off a vehicle
        // the copy is direct, no transform).
        assert!(approx_v(body.local_position, body.global_position));
        assert!(approx_dir(body.local_direction, body.global_direction));
        // The extra cleared with the last rider.
        assert!(!world.has_passengers_extra(1));
    }

    /// The boarding error faces: not a real, already on a vehicle, a
    /// missing vehicle, and boarding own passenger's chain.
    #[test]
    fn boarding_rejects_bad_requests() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(1, Vector3::ZERO, dir(0.0)));
        world.add_entity(EntityBody::ghost(3, Vector3::ZERO, dir(0.0)));

        // Not a real.
        assert_eq!(
            world.board_vehicle(3, 1, true),
            Err("This method may only be called on a real")
        );
        // Missing vehicle.
        assert_eq!(
            world.board_vehicle(1, 42, true),
            Err("Entity.boardVehicle: No entity id")
        );
        // Board a real vehicle.
        world.add_entity(EntityBody::real(2, Vector3::new(4.0, 0.0, 0.0), dir(0.0)));
        world.board_vehicle(2, 1, true).expect("2 boards 1");
        // Already on a vehicle.
        assert_eq!(world.board_vehicle(2, 1, true), Err("Already on a vehicle"));
        // The vehicle is one of our passengers: 1 boarding 2 would close a
        // cycle through the chain 2 -> 1.
        assert_eq!(
            world.board_vehicle(1, 2, true),
            Err("Entity.boardVehicle: Cannot board own passenger")
        );
        // A deep cycle: 4 rides 2 (which rides 1); then 1 boarding 4 has
        // to walk the whole chain 4 -> 2 -> 1 to find itself.
        world.add_entity(EntityBody::real(4, Vector3::new(6.0, 0.0, 0.0), dir(0.0)));
        world.board_vehicle(4, 2, true).expect("4 boards 2");
        assert_eq!(
            world.board_vehicle(1, 4, true),
            Err("Entity.boardVehicle: Cannot board own passenger")
        );
    }

    /// The teleport lands exactly on the target: on the ground the global
    /// pair becomes the target; on a vehicle the global target survives
    /// the local round-trip.
    #[test]
    fn teleport_lands_exactly_on_the_target() {
        let mut world = PassengerWorld::new();
        let mut first = EntityBody::real(1, Vector3::new(0.0, 0.0, 0.0), dir(0.0));
        first.controlled_by_client = true;
        world.add_entity(first);

        let target = Vector3::new(33.0, 4.0, -12.0);
        world
            .teleport(1, MailBoxRef::same_space(), target, dir(1.1))
            .expect("teleports");
        let body = world.entity(1).unwrap();
        assert!(approx_v(body.global_position, target), "目标位置吻合");
        assert!(approx_v(body.local_position, target));
        assert!(approx_dir(body.global_direction, dir(1.1)));
        assert_eq!(
            world.events,
            vec![
                PassengerEvent::Callback(1, "onTeleport"),
                PassengerEvent::Callback(1, "onTeleportSuccess"),
                PassengerEvent::PhysicsCorrection(1),
            ]
        );

        // A rider on a rotated vehicle teleports to a global target.
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(
            1,
            Vector3::new(100.0, 0.0, 100.0),
            dir(1.0),
        ));
        world.add_entity(EntityBody::real(
            2,
            Vector3::new(101.0, 0.0, 100.0),
            dir(0.0),
        ));
        world.board_vehicle(2, 1, true).expect("boards");

        let target = Vector3::new(103.0, 2.0, 98.0);
        world
            .teleport(2, MailBoxRef::same_space(), target, dir(2.0))
            .expect("teleports");
        let body = world.entity(2).unwrap();
        assert!(
            approx_v(body.global_position, target),
            "the rider lands on the global target through the vehicle transform"
        );
        // The local pair is the target pulled onto the vehicle space.
        let transform = *world.vehicle_transform(1).unwrap();
        let mut inverse = transform;
        inverse.invert();
        assert!(approx_v(body.local_position, inverse.apply_point(&target)));
        assert!(approx(body.local_direction.yaw, angle_normalise(2.0 - 1.0)));
    }

    /// The teleport refusals and the cross-cell preparation: invalid
    /// positions, non-cell mailboxes, vehicles, and a rider alighting
    /// before the offload with the stashed local target.
    #[test]
    fn teleport_refuses_and_prepares_the_cross_cell_handoff() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(1, Vector3::new(0.0, 0.0, 0.0), dir(0.0)));

        // Invalid position (the ±1e9 bound), no matter the mailbox.
        assert_eq!(
            world.teleport(
                1,
                MailBoxRef::same_space(),
                Vector3::new(1000000000.0, 0.0, 0.0),
                dir(0.0)
            ),
            Err("Entity.teleport() Cannot teleport to position")
        );
        // The sentinel position needs a real mailbox id.
        assert_eq!(
            world.teleport(1, MailBoxRef::same_space(), INVALID_POSITION, dir(0.0)),
            Err("Entity.teleport() Cannot teleport to position")
        );
        // The sentinel with a real mailbox passes the validity face.
        assert_eq!(
            world.teleport(1, MailBoxRef::near_cell(9), INVALID_POSITION, dir(0.0)),
            Ok(())
        );
        assert!(world.events.contains(&PassengerEvent::TeleportOffload(1)));

        // Non-cell mailbox.
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(1, Vector3::ZERO, dir(0.0)));
        assert_eq!(
            world.teleport(
                1,
                MailBoxRef {
                    id: 9,
                    component: MailBoxComponent::Other
                },
                INVALID_POSITION,
                dir(0.0)
            ),
            Err("Entity.teleport() Cannot teleport near to non-cell entities")
        );

        // A vehicle cannot teleport cross-cell.
        world.add_entity(EntityBody::real(2, Vector3::new(5.0, 0.0, 0.0), dir(0.0)));
        world.board_vehicle(2, 1, true).expect("boards");
        assert!(world.has_passengers_extra(1));
        assert_eq!(
            world.teleport(1, MailBoxRef::near_cell(9), INVALID_POSITION, dir(0.0)),
            Err(
                "Entity.teleport() Cannot teleport vehicles. Consider just setting position if not changing spaces"
            )
        );

        // A rider cross-cell: alights first, then the target is stashed
        // into the local pair for the destination cell.
        let target = Vector3::new(700.0, 1.0, 800.0);
        let target_dir = dir(0.75);
        world
            .teleport(2, MailBoxRef::near_cell(9), target, target_dir)
            .expect("rider teleports");
        assert!(!world.has_passengers_extra(1), "the vehicle extra cleared");
        let body = world.entity(2).unwrap();
        assert_eq!(body.vehicle, None, "alighted before the offload");
        assert!(approx_v(body.local_position, target), "the stashed target");
        assert!(approx_dir(body.local_direction, target_dir));
        let order: Vec<&'static str> = world
            .events
            .iter()
            .filter_map(|event| match event {
                PassengerEvent::Callback(_, name) => Some(*name),
                _ => None,
            })
            .collect();
        assert_eq!(
            order,
            vec![
                "onTeleport",
                "onTeleport",
                "onTeleport",
                "relocated",
                "onLeavingCell"
            ]
        );
        assert!(world.events.contains(&PassengerEvent::TeleportOffload(2)));
    }

    /// The ghost passenger controller: boarding with the vehicle present
    /// keeps the local pair; a missing vehicle sends the ghost to limbo
    /// with the streamed global snapshot; the stop leaves the vehicle with
    /// the global pair kept.
    #[test]
    fn the_ghost_passenger_controller_boards_limbo_and_stops() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(1, Vector3::new(10.0, 0.0, 0.0), dir(0.5)));
        let mut ghost = EntityBody::ghost(2, Vector3::new(1.0, 2.0, 3.0), dir(0.25));
        ghost.global_position = Vector3::new(11.0, 2.0, 3.0);
        ghost.global_direction = dir(0.75);
        world.add_entity(ghost);

        // Stream the controller off the ghost.
        let mut controller = PassengerController::new(1);
        {
            let body = world.entity(2).unwrap();
            let mut stream = BinaryStream::new();
            controller.write_ghost_to_stream(body, &mut stream);
            stream.rewind();
            let mut restored = PassengerController::new(0);
            restored.read_ghost_from_stream(&mut stream).unwrap();
            assert_eq!(restored.vehicle_id, 1);
            assert!(approx_v(
                restored.initial_local_position,
                Vector3::new(1.0, 2.0, 3.0)
            ));
            assert!(approx(restored.initial_local_direction.yaw, 0.25));
            assert!(approx_v(
                restored.initial_global_position,
                Vector3::new(11.0, 2.0, 3.0)
            ));
            controller = restored;
        }

        // startGhost with the vehicle present: the local pair is applied
        // and the ghost joins the vehicle's list.
        world.start_ghost_controller(2, &mut controller);
        let body = world.entity(2).unwrap();
        assert_eq!(body.vehicle, Some(1));
        assert!(approx_v(body.local_position, Vector3::new(1.0, 2.0, 3.0)));
        assert!(
            approx_v(
                body.global_position,
                world
                    .vehicle_transform(1)
                    .unwrap()
                    .apply_point(&Vector3::new(1.0, 2.0, 3.0))
            ),
            "global derived through the vehicle transform"
        );

        // stopGhost: off the list; the ghost is not in the range list and
        // has no vehicle any more, so the local pair is copied across
        // (the reference's "a ghost that has suddenly become... hmm").
        world.stop_ghost_controller(2);
        let body = world.entity(2).unwrap();
        assert_eq!(body.vehicle, None);
        assert!(approx_v(body.global_position, body.local_position));
        assert!(!world.has_passengers_extra(1));
    }

    /// A ghost whose vehicle never existed: limbo, the global snapshot
    /// applied, and the observer watching for the vehicle.
    #[test]
    fn a_ghost_without_its_vehicle_goes_to_limbo() {
        let mut world = PassengerWorld::new();
        let mut ghost = EntityBody::ghost(2, Vector3::new(1.0, 2.0, 3.0), dir(0.0));
        ghost.global_position = Vector3::new(50.0, 2.0, 60.0);
        ghost.global_direction = dir(1.0);
        world.add_entity(ghost);

        let mut controller = PassengerController::new(1);
        controller.initial_local_position = INVALID_POSITION;
        controller.initial_global_position = Vector3::new(50.0, 2.0, 60.0);
        controller.initial_global_direction = dir(1.0);
        world.start_ghost_controller(2, &mut controller);

        let body = world.entity(2).unwrap();
        assert_eq!(body.vehicle, None, "limbo");
        assert!(approx_v(
            body.global_position,
            Vector3::new(50.0, 2.0, 60.0)
        ));
        assert!(world.is_in_limbo(2));
        assert_eq!(
            world.events,
            vec![
                PassengerEvent::Warning("PassengerController::startGhost: Could not find vehicle"),
                PassengerEvent::WatchingForVehicle(2, 1),
            ]
        );
    }

    /// Vehicle destruction forces every rider off: reals alight with their
    /// global positions, ghosts go to limbo, and the extra is gone.
    #[test]
    fn vehicle_destruction_forces_every_rider_off() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(1, Vector3::new(0.0, 0.0, 0.0), dir(0.0)));
        world.add_entity(EntityBody::real(2, Vector3::new(1.0, 0.0, 0.0), dir(0.0)));
        let mut ghost = EntityBody::ghost(3, Vector3::new(-1.0, 0.0, 0.0), dir(0.0));
        ghost.global_position = Vector3::new(-1.0, 0.0, 0.0);
        world.add_entity(ghost);

        world.board_vehicle(2, 1, true).expect("2 boards");
        let mut controller = PassengerController::new(1);
        controller.initial_local_position = Vector3::new(-1.0, 0.0, 0.0);
        world.start_ghost_controller(3, &mut controller);
        assert_eq!(world.vehicles[&1].riders().len(), 2);

        let real_global = world.entity(2).unwrap().global_position;
        // A vehicle with no extra is a no-op.
        world.vehicle_destroyed(999);
        assert!(world.has_passengers_extra(1));
        world.vehicle_destroyed(1);

        assert!(!world.has_passengers_extra(1));
        let real = world.entity(2).unwrap();
        assert_eq!(real.vehicle, None);
        assert!(approx_v(real.global_position, real_global), "kept");
        let ghost_body = world.entity(3).unwrap();
        assert_eq!(ghost_body.vehicle, None, "the ghost is in limbo");
        assert!(world.is_in_limbo(3));
    }

    /// `setPositionAndDirection` refuses an invalid position with the
    /// ERROR face and leaves the entity untouched.
    #[test]
    fn set_position_and_direction_refuses_an_invalid_position() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(1, Vector3::new(1.0, 2.0, 3.0), dir(0.0)));

        let ok =
            world.set_position_and_direction(1, Vector3::new(2000000000.0, 2.0, 3.0), dir(1.0));
        assert!(!ok);
        assert_eq!(
            world.events,
            vec![PassengerEvent::Warning(
                "Entity::setPositionAndDirection: not a valid position"
            )]
        );
        // The pair was not touched.
        let body = world.entity(1).unwrap();
        assert!(approx_v(body.global_position, Vector3::new(1.0, 2.0, 3.0)));
        assert!(approx(body.global_direction.yaw, 0.0));
    }

    /// The alight refusals: a real that is not on a vehicle, and a ghost
    /// (only reals may alight).
    #[test]
    fn alighting_refuses_non_riders_and_ghosts() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(1, Vector3::ZERO, dir(0.0)));
        assert_eq!(world.alight_vehicle(1, true), Err("Not on a vehicle"));

        // A limbo ghost carries a controller but is not a real; its
        // vehicle (7) does not exist in this population.
        let mut ghost = EntityBody::ghost(2, Vector3::new(1.0, 2.0, 3.0), dir(0.0));
        ghost.global_position = Vector3::new(50.0, 2.0, 60.0);
        world.add_entity(ghost);
        let mut controller = PassengerController::new(7);
        controller.initial_local_position = INVALID_POSITION;
        controller.initial_global_position = Vector3::new(50.0, 2.0, 60.0);
        world.start_ghost_controller(2, &mut controller);
        assert_eq!(
            world.alight_vehicle(2, true),
            Err("This method may only be called on a real")
        );

        // stopGhost on the limbo ghost: the observer detaches quietly.
        world.stop_ghost_controller(2);
        assert!(!world.riders.contains_key(&2));
        assert!(!world.is_in_limbo(2));
        assert!(approx_v(
            world.entity(2).unwrap().global_position,
            Vector3::new(50.0, 2.0, 60.0)
        ));
    }

    /// `isValidPosition`: strictly inside ±1e9; NaN fails every bound.
    #[test]
    fn position_validity_bounds() {
        assert!(is_valid_position(Vector3::ZERO));
        assert!(is_valid_position(Vector3::new(
            900000000.0,
            -900000000.0,
            0.0
        )));
        assert!(!is_valid_position(Vector3::new(1000000000.0, 0.0, 0.0)));
        assert!(!is_valid_position(Vector3::new(0.0, 0.0, -1000000000.0)));
        assert!(!is_valid_position(Vector3::new(f32::NAN, 0.0, 0.0)));
    }

    /// A fresh ghost boarding with KEEP_GLOBAL (not in the range list) has
    /// its global pair derived inline from the vehicle transform — no
    /// range-list update, no witness.
    #[test]
    fn a_fresh_ghost_derives_its_global_pair_inline() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(
            1,
            Vector3::new(100.0, 0.0, 100.0),
            dir(1.0),
        ));
        let mut ghost = EntityBody::ghost(2, Vector3::new(1.0, 0.0, 0.0), dir(0.5));
        ghost.witness = true; // ghosts have no witness: still no event
        world.add_entity(ghost);

        // The vehicle extra pre-exists (as when the ghost's controller
        // set it up first).
        let (position, direction) = {
            let body = world.entity(1).unwrap();
            (body.global_position, body.global_direction)
        };
        world
            .vehicles
            .insert(1, PassengersExtra::new(position, direction));

        world.set_vehicle(2, Some(1), SetVehicleParam::KeepGlobalPosition);
        let body = world.entity(2).unwrap();
        assert_eq!(body.vehicle_change_num, 1);
        let transform = *world.vehicle_transform(1).unwrap();
        assert!(approx_v(
            body.global_position,
            transform.apply_point(&Vector3::new(1.0, 0.0, 0.0))
        ));
        assert!(approx(
            body.global_direction.yaw,
            angle_normalise(0.5 + 1.0)
        ));
        assert_eq!(world.events, vec![]);
    }

    /// `onTeleportSuccess`: an unset position falls back to the nearby
    /// entity's position with a warning; a set position leaves it alone.
    #[test]
    fn on_teleport_success_falls_back_to_the_nearby_entity() {
        let mut world = PassengerWorld::new();
        world.add_entity(EntityBody::real(6, Vector3::new(9.0, 9.0, 9.0), dir(0.0)));
        world.add_entity(EntityBody::real(5, INVALID_POSITION, dir(0.0)));

        world.on_teleport_success(5, Some(6));
        let body = world.entity(5).unwrap();
        assert!(approx_v(body.global_position, Vector3::new(9.0, 9.0, 9.0)));
        assert_eq!(
            world.events,
            vec![
                PassengerEvent::Callback(5, "onTeleportSuccess"),
                PassengerEvent::Warning(
                    "Entity::onTeleportSuccess: position was not set; using the nearby entity"
                ),
            ]
        );

        // Second arrival with the position already set: callback only.
        world.events.clear();
        world.on_teleport_success(5, Some(6));
        assert_eq!(
            world.events,
            vec![PassengerEvent::Callback(5, "onTeleportSuccess")]
        );
    }

    /// The ghost passenger stream refuses truncation.
    #[test]
    fn the_ghost_stream_refuses_truncation() {
        let mut short = BinaryStream::new();
        short.write_u32(1);
        short.rewind();
        let mut controller = PassengerController::new(0);
        assert!(controller.read_ghost_from_stream(&mut short).is_err());
    }

    /// A passenger in limbo skips the global update (the INFO face) and
    /// answers "not in global coords".
    #[test]
    fn a_limbo_passenger_skips_the_global_update() {
        let mut world = PassengerWorld::new();
        let mut ghost = EntityBody::ghost(2, Vector3::new(1.0, 2.0, 3.0), dir(0.0));
        ghost.global_position = Vector3::new(50.0, 2.0, 60.0);
        world.add_entity(ghost);

        let mut controller = PassengerController::new(1);
        controller.initial_local_position = INVALID_POSITION;
        controller.initial_global_position = Vector3::new(50.0, 2.0, 60.0);
        world.start_ghost_controller(2, &mut controller);
        assert!(world.is_in_limbo(2));

        world.events.clear();
        let in_global = world.update_global_position(2, false, false);
        assert!(!in_global);
        assert_eq!(
            world.events,
            vec![PassengerEvent::Warning(
                "Entity::updateGlobalPosition: entity is in limbo"
            )]
        );
        // The global pair was untouched by the skipped update.
        assert!(approx_v(
            world.entity(2).unwrap().global_position,
            Vector3::new(50.0, 2.0, 60.0)
        ));
        // An unknown id answers "not in global coords" without events.
        world.events.clear();
        assert!(!world.update_global_position(999, false, false));
        assert!(world.events.is_empty());
    }
}
