//! The vehicle/passenger cluster (BW-08-02-05): the vehicle-side
//! `Passengers` extra (`passengers.cpp`), the rider-side
//! `PassengerExtra` (`passenger_extra.cpp`) and the ghost-domain
//! `PassengerController` (`passenger_controller.cpp`), plus the local
//! faces of the teleport pair (`Entity::setGlobalPosition`,
//! `Entity::onTeleportSuccess`) and the mount-aware split of the
//! position funnel. The app-side maps stand in for the reference's
//! extras: the state lives on the [`CellApp`], keyed by entity id, the
//! way every other cluster in this port keeps it.
//!
//! Reference quirk kept out of the port: `PassengerController` is
//! `DOMAIN_GHOST`, so its start/stop ride the ghost half of
//! `addController`'s dispatch and it never streams in a real offload
//! block. The reference's `MF_ASSERT( !isReal() )` guards in the limbo
//! faces assume ghost entities exist; this port's white-box drivers use
//! ghost-flavoured entities (`Entity::is_real == false`) for exactly
//! those faces instead of dropping the asserts.

use std::cell::RefCell;
use std::rc::Rc;

use foundation::math::angle::normalise as normalise_angle;
use foundation::math::{Matrix, Vector3};

use crate::cellapp_real_entity::{Controller, ControllerId, ControllerKind};
use crate::cellapp_space::{CellApp, Entity, NO_SPACE_REMOVAL_HANDLE};

/// `Entity::INVALID_POSITION`: the position a fresh teleport target
/// carries until the script sets one.
pub const INVALID_POSITION: [f32; 3] = [f32::MAX; 3];

/// `Entity::SetVehicleParam` (`entity.cpp:7598`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetVehicleParam {
    /// The global position is kept and the local position is
    /// recalculated.
    KeepGlobalPosition,
    /// The local position is kept and the global position is calculated
    /// from this.
    KeepLocalPosition,
    /// The actual vehicle is no longer available.
    InLimbo,
}

/// The app-side state of a vehicle's `Passengers` extra: the destructor
/// flag, the rider list, and the global-from-local transform.
#[derive(Debug)]
pub struct PassengersState {
    /// `inDestructor_`.
    pub in_destructor: bool,
    /// `passengers_`, in boarding order (the reference's vector).
    pub passengers: Vec<u32>,
    /// `transform_`: the vehicle's global-from-local matrix.
    pub transform: Matrix,
}

fn new_passengers_state() -> PassengersState {
    PassengersState {
        in_destructor: false,
        passengers: Vec::new(),
        transform: Matrix::identity(),
    }
}

/// `Passengers::transform()` — the vehicle's global-from-local matrix.
pub(crate) fn passengers_transform(app: &CellApp, vehicle: u32) -> Matrix {
    // MF_ASSERT( Passengers::instance.exists( *pVehicle_ ) ): every
    // mounted rider's flow guarantees the vehicle's instance first.
    let state = app
        .passengers_state
        .get(&vehicle)
        .expect("MF_ASSERT( Passengers::instance.exists )");
    state.transform
}

/// `pVehicle_->direction().yaw` — the vehicle's global yaw, the only
/// direction component the reference transforms ("TODO: What do we do
/// here? For now, just doing yaw.").
pub(crate) fn vehicle_yaw(app: &CellApp, vehicle: u32) -> f32 {
    app.entity_population[&vehicle].borrow().direction[0]
}

/// `PassengerExtra::isInLimbo` (`passenger_extra.cpp:241`): no vehicle,
/// the extra alive, a controller set.
pub fn is_in_limbo(app: &CellApp, entity_rc: &Rc<RefCell<Entity>>) -> bool {
    let slot = app
        .extra_types
        .iter()
        .position(|name| *name == "PassengerExtra");
    let borrowed = entity_rc.borrow();
    match slot {
        Some(eeid) => {
            borrowed.vehicle.is_none()
                && app.extra_exists(borrowed.id, eeid)
                && app.passenger_controllers.contains_key(&borrowed.id)
        }
        None => false,
    }
}

/// `Passengers::adjustTransform`'s body (`passengers.cpp:165`): the
/// frame follows the vehicle's global direction and position.
fn adjust_transform_into(state: &mut PassengersState, direction: [f32; 3], position: [f32; 3]) {
    let mut transform = Matrix::identity();
    transform.set_rotate_ypr(direction[0], direction[1], direction[2]);
    transform.translation(&Vector3::new(position[0], position[1], position[2]));
    state.transform = transform;
}

/// `PassengerExtra::instance.clear( entity_ )` at the refusal tails.
fn passenger_extra_clear(app: &mut CellApp, entity: u32, eeid: usize) {
    app.clear_extra(entity, eeid);
}

/// `Passengers::transformToVehiclePos` (`passengers.cpp:113`): the
/// global position into the vehicle's frame.
pub fn transform_to_vehicle_pos(app: &CellApp, vehicle: u32, position: [f32; 3]) -> [f32; 3] {
    let mut inverse = passengers_transform(app, vehicle);
    inverse.invert();
    let local = inverse.apply_point(&Vector3::new(position[0], position[1], position[2]));
    [local.x, local.y, local.z]
}

/// `Passengers::transformToVehicleDir` (`passengers.cpp:125`): yaw only.
pub fn transform_to_vehicle_dir(app: &CellApp, vehicle: u32, yaw: f32) -> f32 {
    normalise_angle(yaw - vehicle_yaw(app, vehicle))
}

impl CellApp {
    /// `Passengers`' slot: the extra type's eeid, registered on first
    /// use the way the reference's static `Instance<Passengers>` does.
    pub fn passengers_extra_eeid(&mut self) -> usize {
        if let Some(index) = self
            .extra_types
            .iter()
            .position(|name| *name == "Passengers")
        {
            return index;
        }
        self.register_entity_extra("Passengers")
    }

    /// `PassengerExtra`'s slot.
    pub fn passenger_extra_eeid(&mut self) -> usize {
        if let Some(index) = self
            .extra_types
            .iter()
            .position(|name| *name == "PassengerExtra")
        {
            return index;
        }
        self.register_entity_extra("PassengerExtra")
    }

    /// `Entity::isRealToScript`'s twin: the entity is live and real —
    /// the face every script entry point checks first (the navigation
    /// faces share it).
    pub(crate) fn is_real_to_script(&self, entity: u32) -> bool {
        self.entity_population.get(&entity).is_some_and(|rc| {
            let borrowed = rc.borrow();
            borrowed.is_real && !borrowed.destroyed
        })
    }

    /// `Passengers::instance( entity )` (`entity_extra.hpp`'s
    /// auto-creating face): touch the vehicle's extra, make sure the
    /// app-side state exists, and adjust its frame to the vehicle's
    /// current position and direction. Answers the eeid.
    pub fn passengers_instance(&mut self, vehicle_rc: &Rc<RefCell<Entity>>) -> usize {
        let (vehicle, direction, position) = {
            let borrowed = vehicle_rc.borrow();
            (borrowed.id, borrowed.direction, borrowed.position)
        };
        let eeid = self.passengers_extra_eeid();
        self.touch_extra(vehicle, eeid);
        let state = self
            .passengers_state
            .entry(vehicle)
            .or_insert_with(new_passengers_state);
        adjust_transform_into(state, direction, position);
        eeid
    }

    /// `Passengers::add` (`passengers.cpp:57`).
    fn passengers_add(&mut self, vehicle: u32, passenger: u32) {
        let state = self
            .passengers_state
            .entry(vehicle)
            .or_insert_with(new_passengers_state);
        // MF_ASSERT( not already a passenger ).
        debug_assert!(
            !state.passengers.contains(&passenger),
            "MF_ASSERT( std::find( ... ) == passengers_.end() )"
        );
        state.passengers.push(passenger);
    }

    /// `Passengers::remove` (`passengers.cpp:72`): the brute-force
    /// search; the last rider out deletes the extra (its state goes with
    /// it) unless the destructor is walking the list.
    fn passengers_remove(&mut self, vehicle: u32, passenger: u32) -> bool {
        let outcome = match self.passengers_state.get_mut(&vehicle) {
            Some(state) => match state
                .passengers
                .iter()
                .position(|rider| *rider == passenger)
            {
                Some(index) => {
                    state.passengers.remove(index);
                    (true, state.passengers.is_empty() && !state.in_destructor)
                }
                None => (false, false),
            },
            None => (false, false),
        };
        let (found, detach) = outcome;
        if !found {
            // WARNING_MSG( "Passengers::remove: Failed to remove %u from %u\n" )
            self.passenger_warnings.push((
                passenger,
                format!("Passengers::remove: Failed to remove {passenger} from {vehicle}"),
            ));
            return false;
        }
        if detach {
            // Note: This needs to be the last thing before the return
            // statement as it deletes this object.
            let eeid = self.passengers_extra_eeid();
            self.clear_extra(vehicle, eeid);
            self.passengers_state.remove(&vehicle);
        }
        true
    }

    /// `Entity::setVehicle` (`entity.cpp:7606`): link or unlink the
    /// vehicle, bump `vehicleChangeNum_`, and resolve whichever face the
    /// caller asked to keep.
    pub fn set_vehicle(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        vehicle: Option<u32>,
        keep_who: SetVehicleParam,
    ) {
        let unchanged = {
            let borrowed = entity_rc.borrow();
            borrowed.vehicle == vehicle
        };
        if unchanged {
            return;
        }
        {
            let mut borrowed = entity_rc.borrow_mut();
            borrowed.vehicle = vehicle;
            borrowed.vehicle_change_num += 1;
        }
        match keep_who {
            SetVehicleParam::KeepGlobalPosition => {
                // "if we're in the range list already then keeping the
                // global position is a fine idea". The witness's
                // vehicleChanged send on this branch is BW-08-03's face.
                let in_range_list = {
                    let borrowed = entity_rc.borrow();
                    borrowed.removal_handle != NO_SPACE_REMOVAL_HANDLE
                };
                if in_range_list {
                    self.update_local_position(entity_rc);
                } else {
                    // "... but if we're not in the list yet then we must
                    // be a ghost that has just been created - in which
                    // case our local position is in fact already correct
                    // but our global position is wrong".
                    if let Some(vehicle) = vehicle {
                        let transform = passengers_transform(self, vehicle);
                        let (local, local_yaw) = {
                            let borrowed = entity_rc.borrow();
                            (borrowed.local_position, borrowed.local_direction[0])
                        };
                        let global =
                            transform.apply_point(&Vector3::new(local[0], local[1], local[2]));
                        let mut borrowed = entity_rc.borrow_mut();
                        borrowed.position = [global.x, global.y, global.z];
                        borrowed.direction[0] =
                            normalise_angle(local_yaw + vehicle_yaw(self, vehicle));
                    }
                }
            }
            SetVehicleParam::KeepLocalPosition => {
                // `updateGlobalPosition( false /* shouldUpdateGhosts */,
                //  true /* isVehicleMovement */ )` — the vehicle-movement
                // flag rides the witness/history faces (BW-08-03).
                self.update_global_position(entity_rc, false);
            }
            SetVehicleParam::InLimbo => {
                // MF_ASSERT( !this->isReal() ): only a non-real entity
                // goes limbo; the port's ghost-flavoured drivers hold it.
                let is_real = entity_rc.borrow().is_real;
                debug_assert!(!is_real, "MF_ASSERT( !this->isReal() )");
            }
        }
        // `this->onPositionChanged()` — the range-list shuffle is the
        // AoI lane's face.
    }

    /// `Entity::updateLocalPosition` (`entity.cpp:7674`): the local face
    /// from the global one. Mounted, only the yaw is rewritten — the
    /// reference leaves the stale pitch/roll in place.
    pub fn update_local_position(&mut self, entity_rc: &Rc<RefCell<Entity>>) {
        let (vehicle, global, global_direction) = {
            let borrowed = entity_rc.borrow();
            (borrowed.vehicle, borrowed.position, borrowed.direction)
        };
        match vehicle {
            None => {
                let mut borrowed = entity_rc.borrow_mut();
                borrowed.local_position = global;
                borrowed.local_direction = global_direction;
            }
            Some(vehicle) => {
                let mut inverse = passengers_transform(self, vehicle);
                inverse.invert();
                let yaw = vehicle_yaw(self, vehicle);
                let global_point = Vector3::new(global[0], global[1], global[2]);
                let local = inverse.apply_point(&global_point);
                let mut borrowed = entity_rc.borrow_mut();
                borrowed.local_position = [local.x, local.y, local.z];
                borrowed.local_direction[0] = normalise_angle(global_direction[0] - yaw);
            }
        }
    }

    /// `Entity::updateGlobalPosition` (`entity.cpp:7703`): the global
    /// face from the local one, then the movement internals. The
    /// reference's `isVehicleMovement` argument only feeds the witness
    /// and history faces (BW-08-03). Answers `isInGlobalCoords`.
    pub fn update_global_position(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        should_update_ghosts: bool,
    ) -> bool {
        // The pre-write global position — the range-list shuffle's
        // `oldPosition`.
        let old_position = entity_rc.borrow().position;
        let (entity, vehicle, local, local_direction, limbo) = {
            let limbo = is_in_limbo(self, entity_rc);
            let borrowed = entity_rc.borrow();
            (
                borrowed.id,
                borrowed.vehicle,
                borrowed.local_position,
                borrowed.local_direction,
                limbo,
            )
        };
        let mut in_global_coords = false;
        let mut in_limbo = false;
        if vehicle.is_none() && !limbo {
            let mut borrowed = entity_rc.borrow_mut();
            borrowed.position = local;
            borrowed.direction = local_direction;
            in_global_coords = true;
        } else if let Some(vehicle) = vehicle {
            let transform = passengers_transform(self, vehicle);
            let yaw = vehicle_yaw(self, vehicle);
            let global = transform.apply_point(&Vector3::new(local[0], local[1], local[2]));
            let mut borrowed = entity_rc.borrow_mut();
            borrowed.position = [global.x, global.y, global.z];
            borrowed.direction[0] = normalise_angle(local_direction[0] + yaw);
        } else {
            // INFO_MSG( "Entity::updateGlobalPosition(%u): Not updating
            // position since entity is in limbo\n" )
            self.passenger_warnings.push((
                entity,
                format!(
                    "Entity::updateGlobalPosition({entity}): Not updating position since entity is in limbo"
                ),
            ));
            in_limbo = true;
        }

        if !in_limbo {
            let is_real = entity_rc.borrow().is_real;
            if is_real && should_update_ghosts {
                // `updateInternalsForNewPositionOfReal`: the ghost haunts
                // are the AoI lane; `newPosition` samples the velocity;
                // the internals run the shuffle and walk the passengers.
                self.real_new_position(entity_rc);
                self.entity_position_changed_internals(entity_rc, old_position);
            } else {
                // `updateInternalsForNewPosition`: the shuffle is the AoI
                // lane; the tail walks the passengers.
                self.entity_position_changed_internals(entity_rc, old_position);
            }
        }
        in_global_coords
    }

    /// `Entity::onVehicleMove` (`entity.cpp:7789`).
    pub(crate) fn on_vehicle_move(&mut self, entity_rc: &Rc<RefCell<Entity>>) {
        // MF_ASSERT( pVehicle_ ) — the caller contract.
        self.update_global_position(entity_rc, false);
    }

    /// `Passengers::updateInternalsForNewPosition`
    /// (`passengers.cpp:136`): refresh the vehicle's frame, then walk
    /// the riders.
    pub fn vehicle_position_changed(&mut self, vehicle_rc: &Rc<RefCell<Entity>>) {
        let (vehicle, direction, position) = {
            let borrowed = vehicle_rc.borrow();
            (borrowed.id, borrowed.direction, borrowed.position)
        };
        let Some(state) = self.passengers_state.get_mut(&vehicle) else {
            return;
        };
        adjust_transform_into(state, direction, position);
        let riders = state.passengers.clone();
        let rider_handles = riders
            .iter()
            .filter_map(|rider| self.entity_population.get(rider).cloned())
            .collect::<Vec<_>>();
        for rider_rc in rider_handles {
            self.on_vehicle_move(&rider_rc);
        }
    }

    /// `Entity::updateInternalsForNewPosition`
    /// (`entity.cpp:3960`): the leading triggers expand, the entity node
    /// shuffles through its space's range list, the trailing triggers
    /// contract (the reference's `callbacksPermitted( false )` guard
    /// means no trigger may be added, deleted or modified across the
    /// events — the port's event targets are recorders, keeping that
    /// contract by construction); then the tail (`entity.cpp:4021`) —
    /// a vehicle that just moved walks its riders.
    pub(crate) fn entity_position_changed_internals(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        old_position: [f32; 3],
    ) {
        let (range_node, range_triggers, space_id) = {
            let borrowed = entity_rc.borrow();
            (
                borrowed.range_node,
                borrowed.range_triggers.clone(),
                borrowed.space_id,
            )
        };
        if let Some(node) = range_node {
            self.shuffle_range_face(node, &range_triggers, space_id, old_position);
        }
        let is_vehicle = {
            let borrowed = entity_rc.borrow();
            self.passengers_state.contains_key(&borrowed.id)
        };
        if is_vehicle {
            self.vehicle_position_changed(entity_rc);
        }
    }

    /// The range face of the internals: the leading triggers expand,
    /// the node shuffles, the trailing triggers contract.
    fn shuffle_range_face(
        &mut self,
        node: crate::cellapp_range_list::NodeId,
        range_triggers: &[crate::cellapp_range_list::TriggerId],
        space_id: crate::cellappmgr::SpaceId,
        old_position: [f32; 3],
    ) {
        if let Some(list) = self.range_lists.get_mut(&space_id) {
            list.shuffle_entity(node, range_triggers, old_position[0], old_position[2]);
        }
    }

    /// `PassengerExtra::boardVehicle` (`passenger_extra.cpp:129`), the
    /// script's `Entity.boardVehicle(vehicleID, shouldUpdateClient=true)`.
    /// Failure answers false with the Python error state on the
    /// [`CellApp::script_errors`] ledger — the formatted refusals leak
    /// their text once, the port's stand-in for a raised Python error.
    pub fn board_vehicle(
        &mut self,
        entity: u32,
        vehicle_id: u32,
        should_update_client: bool,
    ) -> bool {
        let eeid = self.passenger_extra_eeid();
        if !self.is_real_to_script(entity) {
            // PyErr_SetString( PyExc_TypeError, ... )
            self.script_errors
                .push((entity, "This method may only be called on a real"));
            passenger_extra_clear(self, entity, eeid);
            return false;
        }
        // `PassengerExtra::instance( entity )`'s auto-creating face: the
        // lookup lazily constructs the extra, and the refusal paths that
        // clear it are exactly the ones the reference clears.
        self.touch_extra(entity, eeid);
        if self.passenger_controllers.contains_key(&entity) {
            // PyErr_SetString( PyExc_ValueError, "Already on a vehicle" )
            self.script_errors.push((entity, "Already on a vehicle"));
            return false;
        }
        // The population entry `is_real_to_script` approved: it holds for
        // the whole call, exactly as the reference's borrowed entity
        // reference does.
        let entity_rc = self
            .entity_population
            .get(&entity)
            .cloned()
            .expect("is_real_to_script above");
        let Some(vehicle_rc) = self.entity_population.get(&vehicle_id).cloned() else {
            // WARNING_MSG( "%u is trying to board non-existent entity %u\n" )
            self.passenger_warnings.push((
                entity,
                format!("{entity} is trying to board non-existent entity {vehicle_id}"),
            ));
            // PyErr_Format( PyExc_ValueError, "Entity.boardVehicle: No
            // entity id %d", ... )
            self.script_errors.push((
                entity,
                Box::leak(
                    format!("Entity.boardVehicle: No entity id {vehicle_id}").into_boxed_str(),
                ),
            ));
            passenger_extra_clear(self, entity, eeid);
            return false;
        };

        // "make sure that the potential vehicle is not already one of our
        // passengers! .... yes someone did do this!"
        let mut walk = Some(vehicle_id);
        while let Some(current) = walk {
            if current == entity {
                // WARNING_MSG( "%u is trying to board its own passenger %d!\n" )
                self.passenger_warnings.push((
                    entity,
                    format!("{entity} is trying to board its own passenger {vehicle_id}!"),
                ));
                // PyErr_Format( PyExc_TypeError, "Entity.boardVehicle:
                // Cannot board own passenger %d", ... )
                self.script_errors.push((
                    entity,
                    Box::leak(
                        format!("Entity.boardVehicle: Cannot board own passenger {vehicle_id}")
                            .into_boxed_str(),
                    ),
                ));
                passenger_extra_clear(self, entity, eeid);
                return false;
            }
            walk = self
                .entity_population
                .get(&current)
                .and_then(|rc| rc.borrow().vehicle);
        }

        // Make sure this vehicle is set up with Passengers extension.
        self.passengers_instance(&vehicle_rc);
        self.set_vehicle(
            &entity_rc,
            Some(vehicle_id),
            SetVehicleParam::KeepGlobalPosition,
        );
        let controller_id = self.attach_controller(
            entity,
            Controller {
                kind: ControllerKind::Passenger { vehicle_id },
                user_arg: 0,
            },
        );
        // MF_ASSERT( pController_ )
        debug_assert!(self.passenger_controllers.get(&entity) == Some(&controller_id));
        let controlled = entity_rc.borrow().controlled_by != 0;
        if controlled && should_update_client {
            // `pReal->sendPhysicsCorrection()`
            self.physics_corrections.push(entity);
        }
        true
    }

    /// `PassengerExtra::alightVehicle` (`passenger_extra.cpp:245`), the
    /// script's `Entity.alightVehicle(shouldUpdateClient=true)`.
    pub fn alight_vehicle(&mut self, entity: u32, should_update_client: bool) -> bool {
        let eeid = self.passenger_extra_eeid();
        self.touch_extra(entity, eeid);
        let controller_id = self.passenger_controllers.get(&entity).copied();
        if controller_id.is_none() {
            // PyErr_SetString( PyExc_TypeError, "Not on a vehicle" )
            self.script_errors.push((entity, "Not on a vehicle"));
            return false;
        }
        if !self.is_real_to_script(entity) {
            self.script_errors
                .push((entity, "This method may only be called on a real"));
            return false;
        }
        let controller_id = controller_id.expect("checked above");
        if !self.cancel_controller(entity, controller_id) {
            // PyErr_SetString( PyExc_RuntimeError, ... ) — the guard
            // race: the controller was just resolved off the extra.
            self.script_errors
                .push((entity, "Failed to delete the passenger controller"));
            return false;
        }
        // NOTE: The PassengerExtra might have been deleted by here.
        let controlled = self
            .entity_population
            .get(&entity)
            .is_some_and(|rc| rc.borrow().controlled_by != 0);
        if controlled && should_update_client {
            self.physics_corrections.push(entity);
        }
        true
    }

    /// `PassengerExtra::onVehicleGone` (`passenger_extra.cpp:296`):
    /// forward to the controller.
    pub(crate) fn passenger_on_vehicle_gone(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        vehicle_id: u32,
    ) {
        let (entity, is_real, vehicle) = {
            let borrowed = entity_rc.borrow();
            (borrowed.id, borrowed.is_real, borrowed.vehicle)
        };
        if is_real {
            let present = if vehicle.is_some() {
                "present"
            } else {
                "missing"
            };
            // ERROR_MSG( "PassengerController::onVehicleGone( %u ): Real
            // entity alighting since it has lost its vehicle id %u (was
            // %s on this cell)\n" )
            self.passenger_warnings.push((
                entity,
                format!(
                    "PassengerController::onVehicleGone( {entity} ): Real entity alighting since it has lost its vehicle id {vehicle_id} (was {present} on this cell)"
                ),
            ));
            // `alightVehicle()` — the default shouldUpdateClient is true;
            // "not much that we can do about it if it fails 'tho".
            self.alight_vehicle(entity, true);
        } else {
            if let Some(vehicle) = vehicle {
                debug_assert!(
                    self.passengers_remove(vehicle, entity),
                    "MF_VERIFY( Passengers::instance( *pVehicle ).remove( entity ) )"
                );
            }
            // The passenger is now in limbo. If we are being called from
            // startReal, then we do have some idea of our position,
            // passed in as pBackupPosition. Otherwise we just leave it
            // where it lies.
            self.set_vehicle(entity_rc, None, SetVehicleParam::InLimbo);
            // `Entity::population().addObserver( vehicleID_, this )`
            self.population_observers
                .entry(vehicle_id)
                .or_default()
                .push(entity);
        }
    }

    /// `PassengerController::startGhost` (`passenger_controller.cpp:75`):
    /// "We either got on a vehicle or we are just learning about this
    /// entity." The reference's initial-position faces stream off ghost
    /// data the ghost cluster owns; this port's real face never carries
    /// them.
    pub(crate) fn passenger_start(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        vehicle_id: u32,
    ) {
        let entity = entity_rc.borrow().id;
        // `PassengerExtra::instance( entity ).setController( this )` —
        // MF_ASSERT( (!pController) ^ (!pController_) ).
        debug_assert!(
            !self.passenger_controllers.contains_key(&entity),
            "MF_ASSERT( (!pController) ^ (!pController_) )"
        );
        let eeid = self.passenger_extra_eeid();
        self.touch_extra(entity, eeid);
        self.passenger_controllers.insert(entity, id);
        match self.entity_population.get(&vehicle_id).cloned() {
            Some(vehicle_rc) => {
                self.passengers_instance(&vehicle_rc);
                self.passengers_add(vehicle_id, entity);
                self.set_vehicle(
                    entity_rc,
                    Some(vehicle_id),
                    SetVehicleParam::KeepLocalPosition,
                );
            }
            None => {
                // WARNING_MSG( "PassengerController::startGhost(%u):
                // Could not find vehicle %u\n" )
                self.passenger_warnings.push((
                    entity,
                    format!(
                        "PassengerController::startGhost({entity}): Could not find vehicle {vehicle_id}"
                    ),
                ));
                // The reference's MF_ASSERT( !isReal() ) guards the ghost
                // face; the port drives the real alighting chain below.
                self.passenger_on_vehicle_gone(entity_rc, vehicle_id);
            }
        }
    }

    /// `PassengerController::stopGhost` (`passenger_controller.cpp:118`):
    /// "We either got off the vehicle or the (passenger) entity is going
    /// away." Runs from the cancel path with the captured kind, the way
    /// `delController` runs the stop face while the controller is still
    /// in reach.
    pub(crate) fn passenger_stop(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        vehicle_id: u32,
    ) {
        let entity = entity_rc.borrow().id;
        let eeid = self.passenger_extra_eeid();
        // MF_ASSERT( PassengerExtra::instance.exists( entity ) ).
        debug_assert!(
            self.extra_exists(entity, eeid),
            "MF_ASSERT( PassengerExtra::instance.exists( entity ) )"
        );
        // `setController( NULL )` — MF_ASSERT either-or: the entry must
        // be ours.
        debug_assert!(self.passenger_controllers.get(&entity) == Some(&id));
        self.passenger_controllers.remove(&entity);
        if entity_rc.borrow().vehicle.is_some() {
            if self.passengers_state.contains_key(&vehicle_id) {
                debug_assert!(
                    self.passengers_remove(vehicle_id, entity),
                    "MF_VERIFY( Passengers::instance( *pVehicle ).remove( entity ) )"
                );
            }
            self.set_vehicle(entity_rc, None, SetVehicleParam::KeepGlobalPosition);
        } else {
            // DEBUG_MSG( "PassengerController::stopGhost(%u): Passenger
            // was in limbo for vehicle id %u\n" )
            self.passenger_warnings.push((
                entity,
                format!(
                    "PassengerController::stopGhost({entity}): Passenger was in limbo for vehicle id {vehicle_id}"
                ),
            ));
            // MF_VERIFY( Entity::population().removeObserver( ... ) );
            // "note: position will be wrong if we get onloaded to right
            // after this, but hey... that should be pretty rare :)"
            if let Some(observers) = self.population_observers.get_mut(&vehicle_id) {
                observers.retain(|observer| *observer != entity);
            }
        }
        // We no longer need the passenger extra.
        self.clear_extra(entity, eeid);
    }

    /// `PassengerController::onEntityAdded` (`passenger_controller.cpp:196`),
    /// fired by the population notification — the observer list for
    /// `vehicle` is drained and every waiting controller re-boards with
    /// the local position kept.
    pub fn entity_entered_population(&mut self, vehicle: u32) {
        let Some(observers) = self.population_observers.remove(&vehicle) else {
            return;
        };
        let vehicle_rc = self
            .entity_population
            .get(&vehicle)
            .cloned()
            .expect("the entity just entered the population");
        self.passengers_instance(&vehicle_rc);
        let rider_handles = observers
            .into_iter()
            .filter_map(|rider| self.entity_population.get(&rider).cloned())
            .collect::<Vec<_>>();
        for rider_rc in rider_handles {
            // MF_VERIFY( Passengers::instance( vehicle ).add( this->entity() ) )
            self.passengers_add(vehicle, rider_rc.borrow().id);
            self.set_vehicle(&rider_rc, Some(vehicle), SetVehicleParam::KeepLocalPosition);
        }
    }

    /// `~Passengers` (`passengers.cpp:36`): the extra's destructor —
    /// `destroy_entity` runs it before the extras wipe. Every rider
    /// without a `PassengerExtra` is the CRITICAL face.
    pub fn vehicle_destroyed(&mut self, vehicle: u32) {
        let Some(state) = self.passengers_state.get_mut(&vehicle) else {
            return;
        };
        state.in_destructor = true;
        while let Some(next) = self
            .passengers_state
            .get(&vehicle)
            .and_then(|state| state.passengers.last().copied())
        {
            // MF_ASSERT( size == passengers_.size() ) — the reference's
            // per-iteration bookkeeping identity, held by construction.
            if self.passenger_controllers.contains_key(&next) {
                let Some(rider_rc) = self.entity_population.get(&next).cloned() else {
                    // The rider left the population mid-walk (its own
                    // destroy ran inside the alighting chain).
                    break;
                };
                // "it will remove itself from our list"
                self.passenger_on_vehicle_gone(&rider_rc, vehicle);
            } else {
                // CRITICAL_MSG( "Passengers::~Passengers: PassengerExtra
                // does not exist\n" )
                self.passenger_warnings.push((
                    next,
                    "Passengers::~Passengers: PassengerExtra does not exist".to_string(),
                ));
                // `passengers_.pop()` — the in_destructor latch taken on
                // entry keeps the state alive for the whole walk; the
                // lookup only re-borrows across the mutable calls.
                let state = self
                    .passengers_state
                    .get_mut(&vehicle)
                    .expect("the in_destructor latch holds the state");
                state.passengers.pop();
            }
        }
        // The destructor completed: the app-side state goes with it, and
        // the vehicle's own `Passengers` slot is gone.
        self.passengers_state.remove(&vehicle);
        let eeid = self.passengers_extra_eeid();
        self.clear_extra(vehicle, eeid);
    }

    /// `PassengerExtra::relocated` (`passenger_extra.cpp:253`): the
    /// onload tail notification — a real entity still limbo after its
    /// restore alights, because its vehicle did not come to this cell.
    pub fn relocate_entity_extras(&mut self, entity_rc: &Rc<RefCell<Entity>>) {
        let (entity, is_real) = {
            let borrowed = entity_rc.borrow();
            (borrowed.id, borrowed.is_real)
        };
        // "Note: This call is called pretty much last in the process of
        // an onload, albeit fortunately before script calls are
        // permitted again."
        if is_real && is_in_limbo(self, entity_rc) {
            // INFO_MSG( "PassengerExtra::relocated(%u): Getting off our
            // vehicle since we have just been onloaded into a cell where
            // it does not exist\n" )
            self.passenger_warnings.push((
                entity,
                format!(
                    "PassengerExtra::relocated({entity}): Getting off our vehicle since we have just been onloaded into a cell where it does not exist"
                ),
            ));
            // "onVehicleGone will call through to our alightVehicle (and
            // if we are a controlled by a client then the physics
            // correction will be automatically sent to it by that
            // method)" — the marker carries the vehicle the callback
            // names.
            let vehicle_id =
                entity_rc
                    .borrow()
                    .controllers
                    .container
                    .values()
                    .find_map(|controller| match controller.kind {
                        ControllerKind::Passenger { vehicle_id } => Some(vehicle_id),
                        _ => None,
                    });
            if let Some(vehicle_id) = vehicle_id {
                self.passenger_on_vehicle_gone(entity_rc, vehicle_id);
            }
        }
    }

    /// `Entity::setGlobalPosition` (`entity.cpp:6392`): the script
    /// position setter — the write is global, the local face derives
    /// from it, and the real internals run before the physics
    /// correction.
    pub fn set_global_position(&mut self, entity_rc: &Rc<RefCell<Entity>>, new_position: Vector3) {
        let (entity, current) = {
            let borrowed = entity_rc.borrow();
            (borrowed.id, borrowed.position)
        };
        if current == [new_position.x, new_position.y, new_position.z] {
            return;
        }
        {
            let mut borrowed = entity_rc.borrow_mut();
            borrowed.position = [new_position.x, new_position.y, new_position.z];
        }
        self.update_local_position(entity_rc);
        // `updateInternalsForNewPositionOfReal` — the ghost haunts are
        // the AoI lane; the sampler and the shuffle and the passenger
        // walk run here (`current` is the pre-write global position —
        // the shuffle's `oldPosition`).
        self.real_new_position(entity_rc);
        self.entity_position_changed_internals(entity_rc, current);
        // `pReal_->sendPhysicsCorrection()`
        self.physics_corrections.push(entity);
    }

    /// `Entity::setAndDropGlobalPositionAndDirection`
    /// (`entity.cpp:7797`): the restore/teleport position write. The
    /// chunk-space `findDropPoint` is the terrain lane's face, so every
    /// space this port builds takes the reference's unsupported-space
    /// warning. The position is a global intent on an unmounted entity,
    /// which is the funnel's local frame.
    pub fn set_and_drop_global_position_and_direction(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        position: Vector3,
        direction: Vector3,
    ) {
        let (entity, vehicle) = {
            let borrowed = entity_rc.borrow();
            (borrowed.id, borrowed.vehicle)
        };
        if let Some(vehicle) = vehicle {
            // IF_NOT_MF_ASSERT_DEV( pVehicle_ == NULL )
            self.passenger_warnings.push((
                entity,
                format!(
                    "Entity::setAndDropGlobalPositionAndDirection({entity}): Unable to set position while on vehicle {vehicle}"
                ),
            ));
            return;
        }
        if self.should_navigation_drop_position {
            // WARNING_MSG( "Entity::setAndDropGlobalPositionAndDirection():
            // Physical space of this type does not support findDropPoint(),
            // using entity position\n" )
            self.passenger_warnings.push((
                entity,
                "Entity::setAndDropGlobalPositionAndDirection(): Physical space of this type does not support findDropPoint(), using entity position".to_string(),
            ));
        }
        self.set_entity_position_and_direction(entity_rc, position, direction);
    }

    /// `Entity::onTeleportSuccess` (`entity.cpp:5224`): the script
    /// callback, then the fallback — an unset position lands at the
    /// nearby entity's.
    pub fn on_teleport_success(&mut self, entity_rc: &Rc<RefCell<Entity>>, nearby: Option<u32>) {
        let entity = entity_rc.borrow().id;
        self.entity_callback(entity, "onTeleportSuccess", false, None);
        // Destroyed by onTeleportSuccess.
        if entity_rc.borrow().destroyed {
            return;
        }
        // MF_ASSERT( pNearbyEntity ): the teleport contract always knows
        // its nearby entity when the position was left unset.
        debug_assert!(
            entity_rc.borrow().position != INVALID_POSITION || nearby.is_some(),
            "MF_ASSERT( pNearbyEntity )"
        );
        if let Some(nearby_id) = nearby.filter(|_| entity_rc.borrow().position == INVALID_POSITION)
        {
            // WARNING_MSG( "Entity::onTeleportSuccess( %u ): Entity
            // position was not set in onTeleportSuccess. Setting to
            // position of nearby entity %u\n" )
            self.passenger_warnings.push((
                entity,
                format!(
                    "Entity::onTeleportSuccess( {entity} ): Entity position was not set in onTeleportSuccess. Setting to position of nearby entity {nearby_id}"
                ),
            ));
            let nearby_position = self.entity_population[&nearby_id].borrow().position;
            self.set_global_position(
                entity_rc,
                Vector3::new(nearby_position[0], nearby_position[1], nearby_position[2]),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Section: tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_real_entity::{
        ADD_ENTITY_DATA_IS_ON_GROUND, AccelerationFacing, CreateRealOutcome, DOMAIN_GHOST,
        EntityScript,
    };
    use crate::cellapp_space::{NO_SPACE_REMOVAL_HANDLE, Space};
    use crate::cellappmgr::SpaceId;
    use std::collections::{BTreeMap, BTreeSet};

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;
    /// The vehicle's test frame: yaw π/2 at position [10, 0, 5].
    const VEHICLE_YAW: f32 = std::f32::consts::FRAC_PI_2;

    fn create_stream(id: u32, type_id: u32, position: [f32; 3]) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&type_id.to_le_bytes());
        for value in position {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in [0.0f32, 0.0, 0.0] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(b'-');
        stream
    }

    fn spaces_with() -> BTreeMap<SpaceId, Space> {
        let mut spaces = BTreeMap::new();
        spaces.insert(SID, Space::new(SID));
        spaces
    }

    fn plain_app() -> CellApp {
        let mut app = CellApp::default();
        app.entity_scripts
            .insert(TYPE, EntityScript::with_init(Vec::new()));
        app
    }

    fn create(
        app: &mut CellApp,
        spaces: &mut BTreeMap<SpaceId, Space>,
        id: u32,
        position: [f32; 3],
    ) {
        let outcome = app.create_real_entity(spaces, SID, &create_stream(id, TYPE, position));
        assert!(matches!(outcome, CreateRealOutcome::Created(_)));
    }

    fn v(components: [f32; 3]) -> Vector3 {
        Vector3::new(components[0], components[1], components[2])
    }

    fn position_of(app: &CellApp, id: u32) -> [f32; 3] {
        app.entity_population[&id].borrow().position
    }

    fn local_of(app: &CellApp, id: u32) -> [f32; 3] {
        app.entity_population[&id].borrow().local_position
    }

    fn almost(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "{a} != {b}");
    }

    fn almost_at(actual: [f32; 3], expected: [f32; 3]) {
        almost(actual[0], expected[0]);
        almost(actual[1], expected[1]);
        almost(actual[2], expected[2]);
    }

    /// An `AccelerateToPoint` controller for the sampler faces.
    fn accel_controller() -> Controller {
        Controller {
            kind: ControllerKind::AccelerateToPoint {
                destination: [5.0, 0.0, 0.0],
                acceleration: 2.0,
                max_speed: 4.0,
                facing: AccelerationFacing::None,
                stop_at_destination: true,
            },
            user_arg: 0,
        }
    }

    /// The standard scene: a vehicle yawed π/2 at [10, 0, 5] and a rider
    /// created at [1, 0, 0], already boarded.
    fn boarded_scene() -> (CellApp, BTreeMap<SpaceId, Space>) {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [10.0, 0.0, 5.0]);
        app.entity_population[&10].borrow_mut().direction[0] = VEHICLE_YAW;
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        assert!(app.board_vehicle(11, 10, false));
        (app, spaces)
    }

    #[test]
    fn the_passenger_marker_is_ghost_domain() {
        let (app, _spaces) = boarded_scene();

        // `PassengerController` is `CONTROLLER_DECLARE_GHOST_ONLY`:
        // the offload stream's domain gate reads this.
        let marker = app.entity_population[&11]
            .borrow()
            .controllers
            .container
            .values()
            .next()
            .cloned()
            .expect("the boarding marker");
        assert_eq!(marker.domain(), DOMAIN_GHOST);
    }

    #[test]
    fn the_vehicle_frame_round_trips_through_the_transform() {
        let (app, _spaces) = boarded_scene();

        // The transform maps the child frame out: a local point comes
        // back through the inverse unchanged, and the yaw-only direction
        // transform subtracts the vehicle's yaw.
        let local = transform_to_vehicle_pos(&app, 10, [12.0, 3.0, 6.0]);
        let global = passengers_transform(&app, 10).apply_point(&v(local));
        almost_at([global.x, global.y, global.z], [12.0, 3.0, 6.0]);
        almost(transform_to_vehicle_dir(&app, 10, VEHICLE_YAW), 0.0);
        almost(
            transform_to_vehicle_dir(&app, 10, VEHICLE_YAW + 0.5),
            normalise_angle(0.5),
        );
    }

    #[test]
    fn boarding_keeps_the_global_position_and_derives_the_local() {
        let (mut app, _spaces) = boarded_scene();

        // The rider's global position survived the board; its local face
        // is the inverse transform of it; the yaw split follows.
        assert_eq!(position_of(&app, 11), [1.0, 0.0, 0.0]);
        let local = transform_to_vehicle_pos(&app, 10, [1.0, 0.0, 0.0]);
        almost_at(local_of(&app, 11), local);
        let rider = app.entity_population[&11].borrow();
        almost(
            rider.local_direction[0],
            normalise_angle(rider.direction[0] - VEHICLE_YAW),
        );
        assert_eq!(rider.vehicle, Some(10));
        assert_eq!(rider.vehicle_change_num, 1);
        drop(rider);
        assert_eq!(
            app.passengers_state[&10].passengers,
            vec![11],
            "the rider is on the vehicle's list"
        );
        // Both extras are live, and exactly one controller holds the
        // marker.
        let passengers_eeid = app.passengers_extra_eeid();
        let passenger_eeid = app.passenger_extra_eeid();
        assert!(app.extra_exists(10, passengers_eeid));
        assert!(app.extra_exists(11, passenger_eeid));
        assert_eq!(app.passenger_controllers.len(), 1);
    }

    #[test]
    fn a_second_board_is_a_value_error() {
        let (mut app, _spaces) = boarded_scene();
        let drops_before = app.extra_drops.borrow().len();

        assert!(!app.board_vehicle(11, 10, false));
        assert_eq!(
            app.script_errors.last().map(|e| e.1),
            Some("Already on a vehicle")
        );
        // The reference does not clear the extra on this path.
        assert_eq!(app.extra_drops.borrow().len(), drops_before);
        // And no second controller appeared.
        assert_eq!(app.passenger_controllers.len(), 1);
    }

    #[test]
    fn ghosts_and_missing_vehicles_are_refused() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        app.entity_population[&10].borrow_mut().is_real = false;
        assert!(!app.board_vehicle(10, 10, false));
        assert_eq!(
            app.script_errors.last().map(|e| e.1),
            Some("This method may only be called on a real")
        );
        app.entity_population[&10].borrow_mut().is_real = true;
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);

        assert!(!app.board_vehicle(11, 999, false));
        assert_eq!(
            app.script_errors.last().map(|e| e.1),
            Some("Entity.boardVehicle: No entity id 999")
        );
        assert!(
            app.passenger_warnings
                .iter()
                .any(|(_, text)| text == "11 is trying to board non-existent entity 999")
        );
        // The refusal cleared the extra it lazily created.
        let passenger_eeid = app.passenger_extra_eeid();
        assert!(!app.extra_exists(11, passenger_eeid));
    }

    #[test]
    fn boarding_your_own_passenger_chain_is_a_type_error() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 12, [2.0, 0.0, 0.0]);

        // 12 boards 11, then 11 tries to board 12 — the walk hits 11
        // (itself) through 12's vehicle link.
        assert!(app.board_vehicle(12, 11, false));
        assert!(!app.board_vehicle(11, 12, false));
        assert_eq!(
            app.script_errors.last().map(|e| e.1),
            Some("Entity.boardVehicle: Cannot board own passenger 12")
        );
        assert!(
            app.passenger_warnings
                .iter()
                .any(|(_, text)| text == "11 is trying to board its own passenger 12!")
        );
        assert_eq!(app.entity_population[&11].borrow().vehicle, None);
    }

    #[test]
    fn vehicle_movement_derives_the_rider_from_the_new_frame() {
        let (mut app, _spaces) = boarded_scene();
        let vehicle_rc = app.entity_population[&10].clone();
        let local_before = local_of(&app, 11);
        let rider_velocity_before = app.entity_population[&11].borrow().velocity;
        // Age the vehicle's velocity sample past the one-tick gate.
        app.advance_tick();
        app.advance_tick();

        // The vehicle drives one tick: the funnel writes its global
        // face, and the internals tail re-derives every rider from the
        // refreshed frame.
        app.set_entity_position_and_direction(
            &vehicle_rc,
            v([11.0, 0.0, 5.0]),
            v([VEHICLE_YAW, 0.0, 0.0]),
        );

        almost_at(position_of(&app, 10), [11.0, 0.0, 5.0]);
        // The rider kept its local face; its global face moved with the
        // vehicle — the acceptance's "target position" agreement.
        almost_at(local_of(&app, 11), local_before);
        let expected_global = passengers_transform(&app, 10).apply_point(&v(local_of(&app, 11)));
        almost_at(
            position_of(&app, 11),
            [expected_global.x, expected_global.y, expected_global.z],
        );
        // The vehicle's velocity sampled; the rider's own velocity did
        // not (its movement face never ran).
        assert_ne!(app.entity_population[&10].borrow().velocity, [0.0; 3]);
        assert_eq!(
            app.entity_population[&11].borrow().velocity,
            rider_velocity_before
        );
        assert_eq!(app.entity_population[&11].borrow().vehicle_change_num, 1);
    }

    #[test]
    fn alighting_returns_to_global_coordinates_and_clears_both_sides() {
        let (mut app, _spaces) = boarded_scene();
        // Drive the vehicle so the alight has a nontrivial global to
        // keep.
        let vehicle_rc = app.entity_population[&10].clone();
        app.set_entity_position_and_direction(
            &vehicle_rc,
            v([12.0, 0.0, 5.0]),
            v([VEHICLE_YAW, 0.0, 0.0]),
        );
        let global_before = position_of(&app, 11);
        let passengers_eeid = app.passengers_extra_eeid();
        let passenger_eeid = app.passenger_extra_eeid();
        let drops_before = app.extra_drops.borrow().len();

        assert!(app.alight_vehicle(11, false));

        // The global position is exactly where the ride left it, and the
        // local face is the global face again.
        almost_at(position_of(&app, 11), global_before);
        assert_eq!(local_of(&app, 11), global_before);
        let rider = app.entity_population[&11].borrow();
        assert_eq!(rider.vehicle, None);
        assert_eq!(rider.vehicle_change_num, 2);
        drop(rider);
        // Both extras deleted; the state went with the last rider.
        assert_eq!(app.extra_drops.borrow().len(), drops_before + 2);
        assert!(!app.extra_exists(10, passengers_eeid));
        assert!(!app.extra_exists(11, passenger_eeid));
        assert!(!app.passengers_state.contains_key(&10));
        assert!(app.passenger_controllers.is_empty());

        // A second alight is a TypeError.
        assert!(!app.alight_vehicle(11, false));
        assert_eq!(
            app.script_errors.last().map(|e| e.1),
            Some("Not on a vehicle")
        );
    }

    #[test]
    fn destroying_the_vehicle_alights_every_rider() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 12, [2.0, 0.0, 0.0]);
        assert!(app.board_vehicle(11, 10, false));
        assert!(app.board_vehicle(12, 10, false));
        let drops_before = app.extra_drops.borrow().len();

        app.destroy_entity(&mut spaces, 10);

        for rider in [11, 12] {
            let borrowed = app.entity_population[&rider].borrow();
            assert_eq!(borrowed.vehicle, None, "rider {rider} alighted");
            assert!(borrowed.controllers.container.is_empty());
            assert_eq!(borrowed.local_position, borrowed.position);
        }
        // The ERROR text ran per rider ("present" — the vehicle was on
        // this cell when the walk started).
        let errors = app
            .passenger_warnings
            .iter()
            .filter(|(_, text)| text.contains("Real entity alighting"))
            .count();
        assert_eq!(errors, 2);
        // The vehicle's own extras are gone and the state with them.
        assert!(app.passengers_state.is_empty());
        let passengers_eeid = app.passengers_extra_eeid();
        let passenger_eeid = app.passenger_extra_eeid();
        assert!(!app.extra_exists(10, passengers_eeid));
        assert!(!app.extra_exists(11, passenger_eeid));
        assert!(!app.extra_exists(12, passenger_eeid));
        assert_eq!(app.extra_drops.borrow().len(), drops_before + 3);
        assert!(
            app.entity_population
                .values()
                .all(|rc| rc.borrow().vehicle.is_none())
        );
    }

    #[test]
    fn a_rider_without_its_extra_is_the_critical_face() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        assert!(app.board_vehicle(11, 10, false));
        // Desynchronise: the rider loses its controller marker before
        // the vehicle dies.
        app.passenger_controllers.remove(&11);

        app.vehicle_destroyed(10);

        assert!(
            app.passenger_warnings
                .iter()
                .any(|(entity, text)| *entity == 11
                    && text == "Passengers::~Passengers: PassengerExtra does not exist")
        );
        assert!(!app.passengers_state.contains_key(&10));
    }

    #[test]
    fn a_start_without_its_vehicle_walks_the_alight_chain() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);

        // White-box: the ghost-data faces are the ghost cluster's, so
        // the missing-vehicle start is driven by attaching the marker
        // straight onto the container.
        let id = app.attach_controller(
            11,
            Controller {
                kind: ControllerKind::Passenger { vehicle_id: 999 },
                user_arg: 0,
            },
        );
        assert_ne!(id, 0);

        assert!(
            app.passenger_warnings
                .iter()
                .any(|(entity, text)| *entity == 11
                    && text == "PassengerController::startGhost(11): Could not find vehicle 999")
        );
        assert!(app.passenger_warnings.iter().any(|(entity, text)| *entity == 11
            && text.contains(
                "Real entity alighting since it has lost its vehicle id 999 (was missing on this cell)"
            )));
        // The start's own alight chain deleted the controller again.
        assert!(
            app.entity_population[&11]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
        assert!(app.passenger_controllers.is_empty());
        let passenger_eeid = app.passenger_extra_eeid();
        assert!(!app.extra_exists(11, passenger_eeid));
    }

    #[test]
    fn a_limbo_passenger_reboards_when_the_vehicle_enters_the_population() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        // A ghost-flavoured rider whose vehicle 999 is not on this cell:
        // the start lands it in limbo with an observer instead of
        // alighting.
        app.entity_population[&11].borrow_mut().is_real = false;
        app.attach_controller(
            11,
            Controller {
                kind: ControllerKind::Passenger { vehicle_id: 999 },
                user_arg: 0,
            },
        );
        assert_eq!(app.entity_population[&11].borrow().vehicle, None);
        assert!(
            app.population_observers
                .get(&999)
                .is_some_and(|observers| observers.contains(&11))
        );

        // The vehicle enters the population: the notification re-boards
        // the rider with the local position kept.
        create(&mut app, &mut spaces, 999, [8.0, 0.0, 2.0]);

        let rider = app.entity_population[&11].borrow();
        assert_eq!(rider.vehicle, Some(999));
        almost_at(rider.local_position, [1.0, 0.0, 0.0]);
        let global = passengers_transform(&app, 999).apply_point(&v(rider.local_position));
        drop(rider);
        almost_at(position_of(&app, 11), [global.x, global.y, global.z]);
        assert_eq!(app.passengers_state[&999].passengers, vec![11]);
        assert!(!app.population_observers.contains_key(&999));
    }

    #[test]
    fn the_ghost_vehicle_gone_face_leaves_the_rider_in_limbo() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        assert!(app.board_vehicle(11, 10, false));
        // The ghost-flavoured face: the vehicle vanishes under a
        // non-real rider, which goes limbo and waits as an observer. The
        // controller stays alive — that is what makes it limbo.
        app.entity_population[&11].borrow_mut().is_real = false;
        let rider_rc = app.entity_population[&11].clone();
        app.passenger_on_vehicle_gone(&rider_rc, 10);

        let rider = app.entity_population[&11].borrow();
        assert_eq!(rider.vehicle, None);
        assert!(!rider.controllers.container.is_empty());
        assert!(
            app.population_observers
                .get(&10)
                .is_some_and(|observers| observers.contains(&11))
        );
        drop(rider);
        // And the limbo predicate answers for it.
        assert!(is_in_limbo(&app, &rider_rc));

        // A plain entity in the same call shape is not limbo — nothing
        // was ever set up.
        let mut other = plain_app();
        let mut other_spaces = spaces_with();
        create(&mut other, &mut other_spaces, 20, [0.0, 0.0, 0.0]);
        assert!(!is_in_limbo(&other, &other.entity_population[&20].clone()));
        // An app where the extra type was never registered answers
        // false too.
        let bare = CellApp::default();
        let ghost = Rc::new(RefCell::new(Entity::new(30, false)));
        assert!(!is_in_limbo(&bare, &ghost));
    }

    #[test]
    fn the_in_limbo_set_vehicle_holds_the_non_real_contract() {
        let (mut app, _spaces) = boarded_scene();
        app.entity_population[&11].borrow_mut().is_real = false;
        let rider_rc = app.entity_population[&11].clone();
        // The debug assert holds for a ghost-flavoured entity; the call
        // itself only bumps the change number.
        app.set_vehicle(&rider_rc, None, SetVehicleParam::InLimbo);
        assert_eq!(rider_rc.borrow().vehicle, None);
        assert_eq!(rider_rc.borrow().vehicle_change_num, 2);
    }

    #[test]
    fn passengers_remove_warns_for_unknown_riders() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);

        // No state at all.
        assert!(!app.passengers_remove(10, 11));
        // State but the rider is not on it.
        let vehicle_rc = app.entity_population[&10].clone();
        app.passengers_instance(&vehicle_rc);
        assert!(!app.passengers_remove(10, 11));
        assert_eq!(
            app.passenger_warnings,
            vec![
                (
                    11,
                    "Passengers::remove: Failed to remove 11 from 10".to_string()
                ),
                (
                    11,
                    "Passengers::remove: Failed to remove 11 from 10".to_string()
                ),
            ]
        );
        // A successful remove of the last rider clears the extra and the
        // state.
        app.passengers_add(10, 11);
        assert!(app.passengers_remove(10, 11));
        assert!(!app.passengers_state.contains_key(&10));
    }

    #[test]
    fn a_vehicle_move_without_state_is_a_no_op() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        let vehicle_rc = app.entity_population[&10].clone();
        app.vehicle_position_changed(&vehicle_rc);
        assert!(app.passengers_state.is_empty());
        assert_eq!(position_of(&app, 10), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn unmounted_movers_keep_the_local_face_equal_and_the_sampler_working() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        let entity_rc = app.entity_population[&10].clone();

        app.advance_tick();
        app.advance_tick();
        app.set_entity_position_and_direction(&entity_rc, v([3.0, 0.0, 0.0]), v([0.0, 0.0, 0.0]));
        let borrowed = entity_rc.borrow();
        assert_eq!(borrowed.local_position, borrowed.position);
        // Two ticks since the creation sample: the delta integrates.
        almost_at(borrowed.velocity, [15.0, 0.0, 0.0]);
        drop(borrowed);
        app.advance_tick();
        app.set_entity_position_and_direction(&entity_rc, v([6.0, 0.0, 0.0]), v([0.0, 0.0, 0.0]));
        let borrowed = entity_rc.borrow();
        // One tick of separation keeps the older sample.
        almost_at(borrowed.velocity, [15.0, 0.0, 0.0]);
    }

    #[test]
    fn refresh_current_velocity_unmounted_answers_the_entity_sample() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [0.0, 0.0, 0.0]);
        app.entity_population[&10].borrow_mut().velocity = [1.0, 2.0, 3.0];
        app.attach_controller(10, accel_controller());

        let velocity = app.refresh_current_velocity(&app.entity_population[&10].clone());
        almost_at([velocity.x, velocity.y, velocity.z], [1.0, 2.0, 3.0]);
        assert_eq!(app.entity_population[&10].borrow().accel_vehicle, None);
    }

    #[test]
    fn refresh_current_velocity_restarts_the_sample_when_the_vehicle_changes() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [10.0, 0.0, 5.0]);
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        // The controller existed before the board: its vehicle sample is
        // stale (started with no vehicle).
        app.attach_controller(11, accel_controller());
        assert_eq!(app.entity_population[&11].borrow().accel_vehicle, None);

        assert!(app.board_vehicle(11, 10, false));
        let rider_rc = app.entity_population[&11].clone();
        let local_at_board = local_of(&app, 11);

        // The changed-vehicle face restarts the sample at the local
        // position with a zero velocity.
        let velocity = app.refresh_current_velocity(&rider_rc);
        almost_at([velocity.x, velocity.y, velocity.z], [0.0, 0.0, 0.0]);
        let rider = rider_rc.borrow();
        assert_eq!(rider.accel_vehicle, Some(10));
        assert_eq!(rider.accel_sample, local_at_board);
        assert_eq!(rider.accel_sample_time, app.time);
    }

    #[test]
    fn refresh_current_velocity_integrates_the_local_delta_after_a_tick() {
        let (mut app, _spaces) = boarded_scene();
        let rider_rc = app.entity_population[&11].clone();
        // Start the sampler at the mount point (the acceleration family
        // does this in startReal).
        {
            let mut rider = rider_rc.borrow_mut();
            rider.accel_vehicle = Some(10);
            rider.accel_sample = rider.local_position;
            rider.accel_sample_time = app.time;
            rider.accel_velocity = [0.0; 3];
        }
        let sample_at_start = rider_rc.borrow().accel_sample;

        // The rider drifts in the vehicle frame across more than one
        // tick, then the sampler integrates the local delta.
        app.advance_tick();
        app.advance_tick();
        rider_rc.borrow_mut().local_position = [2.0, 0.0, 0.0];
        let velocity = app.refresh_current_velocity(&rider_rc);
        let hertz = app.update_hertz as f32;
        let expected = [
            (2.0 - sample_at_start[0]) * hertz / 2.0,
            (0.0 - sample_at_start[1]) * hertz / 2.0,
            (0.0 - sample_at_start[2]) * hertz / 2.0,
        ];
        almost_at([velocity.x, velocity.y, velocity.z], expected);
        let rider = rider_rc.borrow();
        almost_at(rider.accel_sample, [2.0, 0.0, 0.0]);
        drop(rider);
        // A same-tick refresh keeps the integrated velocity.
        let again = app.refresh_current_velocity(&rider_rc);
        almost_at([again.x, again.y, again.z], expected);
    }

    #[test]
    fn set_global_position_repositions_and_corrects_physics() {
        let (mut app, _spaces) = boarded_scene();
        let rider_rc = app.entity_population[&11].clone();

        // A mounted global write re-derives the local face from the
        // vehicle frame.
        app.set_global_position(&rider_rc, v([2.0, 0.0, 0.0]));
        assert_eq!(position_of(&app, 11), [2.0, 0.0, 0.0]);
        let expected_local = transform_to_vehicle_pos(&app, 10, [2.0, 0.0, 0.0]);
        almost_at(local_of(&app, 11), expected_local);
        assert_eq!(app.physics_corrections, vec![11]);

        // The same position writes nothing.
        app.set_global_position(&rider_rc, v([2.0, 0.0, 0.0]));
        assert_eq!(app.physics_corrections, vec![11]);

        // Moving the VEHICLE through the setter walks its riders too.
        let vehicle_rc = app.entity_population[&10].clone();
        app.set_global_position(&vehicle_rc, v([20.0, 0.0, 5.0]));
        let expected_global = passengers_transform(&app, 10).apply_point(&v(local_of(&app, 11)));
        almost_at(
            position_of(&app, 11),
            [expected_global.x, expected_global.y, expected_global.z],
        );
        assert_eq!(app.physics_corrections, vec![11, 10]);
    }

    #[test]
    fn set_and_drop_refuses_while_mounted_and_warns_without_a_drop_point() {
        let (mut app, _spaces) = boarded_scene();
        let rider_rc = app.entity_population[&11].clone();

        // Mounted: the write is refused with the warning.
        app.set_and_drop_global_position_and_direction(
            &rider_rc,
            v([50.0, 0.0, 0.0]),
            v([0.0, 0.0, 0.0]),
        );
        assert_ne!(position_of(&app, 11), [50.0, 0.0, 0.0]);
        assert!(app.passenger_warnings.iter().any(|(entity, text)| *entity == 11
            && text == "Entity::setAndDropGlobalPositionAndDirection(11): Unable to set position while on vehicle 10"));

        // Unmounted with the drop flag on: the unsupported-space warning
        // rides along and the funnel write happens.
        assert!(app.alight_vehicle(11, false));
        app.should_navigation_drop_position = true;
        app.set_and_drop_global_position_and_direction(
            &rider_rc,
            v([50.0, 0.0, 0.0]),
            v([0.0, 0.0, 0.0]),
        );
        almost_at(position_of(&app, 11), [50.0, 0.0, 0.0]);
        assert!(app.passenger_warnings.iter().any(|(_, text)| text
            == "Entity::setAndDropGlobalPositionAndDirection(): Physical space of this type does not support findDropPoint(), using entity position"));
    }

    #[test]
    fn on_teleport_success_falls_back_to_the_nearby_position() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        app.entity_scripts.insert(
            TYPE,
            EntityScript::with_methods(
                BTreeSet::from(["__init__", "onTeleportSuccess"]),
                BTreeMap::new(),
            ),
        );
        create(&mut app, &mut spaces, 10, [3.0, 0.0, 4.0]);
        create(&mut app, &mut spaces, 11, [0.0, 0.0, 0.0]);
        let rider_rc = app.entity_population[&11].clone();

        // A set position: only the callback runs.
        app.on_teleport_success(&rider_rc, Some(10));
        assert_eq!(
            app.script_calls
                .iter()
                .filter(|call| call.name == "onTeleportSuccess")
                .count(),
            1
        );
        assert_eq!(position_of(&app, 11), [0.0, 0.0, 0.0]);

        // An unset position: the fallback lands at the nearby entity.
        rider_rc.borrow_mut().position = INVALID_POSITION;
        app.on_teleport_success(&rider_rc, Some(10));
        almost_at(position_of(&app, 11), [3.0, 0.0, 4.0]);
        assert_eq!(local_of(&app, 11), position_of(&app, 11));
        assert!(app.passenger_warnings.iter().any(|(entity, text)| *entity == 11
            && text == "Entity::onTeleportSuccess( 11 ): Entity position was not set in onTeleportSuccess. Setting to position of nearby entity 10"));
        assert_eq!(app.physics_corrections, vec![11]);

        // Destroyed by the callback face: nothing else runs.
        app.destroy_entity(&mut spaces, 11);
        app.on_teleport_success(&rider_rc, Some(10));
        assert!(
            app.destroyed_callbacks
                .iter()
                .any(|(entity, name)| *entity == 11 && *name == "onTeleportSuccess")
        );
        assert_eq!(app.physics_corrections, vec![11]);
    }

    #[test]
    fn a_restored_limbo_rider_alights_in_the_relocated_face() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        // The restore tail's shape: a rider whose vehicle never made it
        // to this cell — limbo with the controller set. The start face
        // needs the ghost flavour to land in limbo (a real start walks
        // the alight chain), so flip it, attach, then restore the real
        // flavour the relocated predicate (isReal && limbo) checks.
        app.entity_population[&11].borrow_mut().is_real = false;
        app.attach_controller(
            11,
            Controller {
                kind: ControllerKind::Passenger { vehicle_id: 999 },
                user_arg: 0,
            },
        );
        app.entity_population[&11].borrow_mut().is_real = true;
        let rider_rc = app.entity_population[&11].clone();

        // A non-limbo entity relocates as a no-op.
        let mut other = plain_app();
        let mut other_spaces = spaces_with();
        create(&mut other, &mut other_spaces, 20, [0.0, 0.0, 0.0]);
        other.relocate_entity_extras(&other.entity_population[&20].clone());
        assert!(other.passenger_warnings.is_empty());

        // The limbo rider alights with the INFO text.
        app.relocate_entity_extras(&rider_rc);
        assert!(app.passenger_warnings.iter().any(|(entity, text)| *entity == 11
            && text == "PassengerExtra::relocated(11): Getting off our vehicle since we have just been onloaded into a cell where it does not exist"));
        assert!(app.passenger_controllers.is_empty());
        let passenger_eeid = app.passenger_extra_eeid();
        assert!(!app.extra_exists(11, passenger_eeid));
    }

    /// `Entity::updateGlobalPosition`'s ghost faces: an unmounted ghost
    /// copies its local face out and still walks the internals; a limbo
    /// one warns and skips everything.
    #[test]
    fn update_global_position_faces_the_ghost_and_the_limbo() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        app.entity_population[&11].borrow_mut().is_real = false;
        let rider_rc = app.entity_population[&11].clone();

        // The unmounted ghost: the global face takes the local write.
        assert!(app.update_global_position(&rider_rc, true));
        assert_eq!(position_of(&app, 11), [1.0, 0.0, 0.0]);
        assert_eq!(app.physics_corrections, Vec::new());

        // A real unmounted entity asking for the ghost update runs the
        // real internals tail (the sampler and the passenger walk).
        app.entity_population[&11].borrow_mut().is_real = true;
        assert!(app.update_global_position(&rider_rc, true));
        assert_eq!(position_of(&app, 11), [1.0, 0.0, 0.0]);

        // Now a limbo ghost: vehicle none, extra and controller map live.
        let eeid = app.passenger_extra_eeid();
        assert_eq!(app.touch_extra(11, eeid), Some(0));
        app.passenger_controllers.insert(11, 1);
        assert!(!app.update_global_position(&rider_rc, false));
        assert!(app.passenger_warnings.iter().any(|(entity, text)| *entity == 11
            && text == "Entity::updateGlobalPosition(11): Not updating position since entity is in limbo"));
    }

    /// `set_vehicle`'s KeepGlobalPosition ghost face: an entity the
    /// range list never took (a ghost fresh off the create) keeps its
    /// local face and recomputes the global one from the vehicle frame.
    #[test]
    fn a_ghost_set_vehicle_recomputes_only_the_global_face() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [10.0, 0.0, 5.0]);
        app.entity_population[&10].borrow_mut().direction[0] = VEHICLE_YAW;
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        // Take 11 off the range list: the ghost-just-created shape. The
        // vehicle's Passengers instance is the rider flow's precondition.
        app.entity_population[&11].borrow_mut().removal_handle = NO_SPACE_REMOVAL_HANDLE;
        let vehicle_rc = app.entity_population[&10].clone();
        app.passengers_instance(&vehicle_rc);
        let rider_rc = app.entity_population[&11].clone();

        app.set_vehicle(&rider_rc, Some(10), SetVehicleParam::KeepGlobalPosition);

        assert_eq!(app.entity_population[&11].borrow().vehicle, Some(10));
        assert_eq!(local_of(&app, 11), [1.0, 0.0, 0.0]);
        let expected = passengers_transform(&app, 10).apply_point(&v([1.0, 0.0, 0.0]));
        almost_at(position_of(&app, 11), [expected.x, expected.y, expected.z]);

        // Unlinking a ghost the list never took: no local face to
        // resolve, the global face just stays where it lies.
        app.entity_population[&11].borrow_mut().removal_handle = NO_SPACE_REMOVAL_HANDLE;
        app.set_vehicle(&rider_rc, None, SetVehicleParam::KeepGlobalPosition);
        assert_eq!(app.entity_population[&11].borrow().vehicle, None);
        assert_eq!(local_of(&app, 11), [1.0, 0.0, 0.0]);
    }

    /// The client-facing correction faces: a controlled rider's board
    /// and alight both send the physics correction, a ghost alight is
    /// refused, and the guard race fails the alight without one.
    #[test]
    fn the_client_facing_board_and_alight_faces_correct_the_physics() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [10.0, 0.0, 5.0]);
        app.entity_population[&10].borrow_mut().direction[0] = VEHICLE_YAW;
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        app.entity_population[&11].borrow_mut().controlled_by = 1;
        assert!(app.board_vehicle(11, 10, true));
        assert_eq!(app.physics_corrections, vec![11]);
        assert!(app.alight_vehicle(11, true));
        assert_eq!(app.physics_corrections, vec![11, 11]);

        // The guard race: the marker the extra named is already gone.
        create(&mut app, &mut spaces, 12, [2.0, 0.0, 0.0]);
        app.entity_population[&12].borrow_mut().controlled_by = 1;
        assert!(app.board_vehicle(12, 10, true));
        app.passenger_controllers.remove(&12);
        let race_controller = *app.entity_population[&12]
            .borrow()
            .controllers
            .container
            .keys()
            .next()
            .expect("the boarding marker");
        app.entity_population[&12]
            .borrow_mut()
            .controllers
            .container
            .remove(&race_controller);
        app.passenger_controllers.insert(12, race_controller);
        assert!(!app.alight_vehicle(12, true));
        assert_eq!(
            app.script_errors.last().map(|e| e.1),
            Some("Failed to delete the passenger controller")
        );
        // The failed alight sent nothing further.
        assert_eq!(app.physics_corrections, vec![11, 11, 12]);
    }

    /// `PassengerExtra::alightVehicle`'s ghost refusal: a limbo ghost
    /// holds a controller but is no longer real to the script.
    #[test]
    fn a_ghost_alight_is_refused() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [10.0, 0.0, 5.0]);
        app.entity_population[&10].borrow_mut().direction[0] = VEHICLE_YAW;
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        assert!(app.board_vehicle(11, 10, false));
        // The ghost flavour rides the limbo face: still holding the
        // marker, no longer real.
        app.entity_population[&11].borrow_mut().is_real = false;

        assert!(!app.alight_vehicle(11, false));
        assert_eq!(
            app.script_errors.last().map(|e| e.1),
            Some("This method may only be called on a real")
        );
        assert_eq!(app.entity_population[&11].borrow().vehicle, Some(10));
    }

    /// `PassengerController::stop`'s missing-state face: with the
    /// vehicle's app-side state already gone, the rider still releases
    /// the link and returns to global coordinates.
    #[test]
    fn a_missing_passengers_state_entry_still_releases_the_rider() {
        let (mut app, _spaces) = boarded_scene();
        app.entity_population[&11].borrow_mut().controlled_by = 1;
        app.passengers_state.remove(&10);

        assert!(app.alight_vehicle(11, true));
        assert_eq!(app.entity_population[&11].borrow().vehicle, None);
        assert_eq!(local_of(&app, 11), position_of(&app, 11));
        assert_eq!(app.physics_corrections, vec![11]);
        assert!(app.passenger_controllers.is_empty());
    }

    /// `~Passengers`' stale-rider face: a rider that left the population
    /// before the vehicle's walk reaches it stops the walk in place.
    #[test]
    fn a_rider_gone_from_the_population_stops_the_passengers_walk() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 10, [10.0, 0.0, 5.0]);
        app.entity_population[&10].borrow_mut().direction[0] = VEHICLE_YAW;
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 12, [2.0, 0.0, 0.0]);
        assert!(app.board_vehicle(11, 10, false));
        assert!(app.board_vehicle(12, 10, false));
        // 11 rides at the list head and is gone from the population.
        app.entity_population.remove(&11);

        app.vehicle_destroyed(10);

        assert!(!app.passengers_state.contains_key(&10));
        // The tail rider 12 released through the normal alight chain;
        // the walk stopped at the head rider 11, whose controller entry
        // never got its release.
        assert_eq!(app.entity_population[&12].borrow().vehicle, None);
        assert!(app.passenger_controllers.contains_key(&11));
    }

    /// `PassengerExtra::relocated`'s marker-less face: a real limbo
    /// entity whose controllers hold no passenger marker only takes the
    /// INFO warning — there is no vehicle id to fall off.
    #[test]
    fn a_relocated_limbo_rider_without_a_marker_only_warns() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        let eeid = app.passenger_extra_eeid();
        assert_eq!(app.touch_extra(11, eeid), Some(0));
        app.add_timer(11, 2.0, 0.0, 0);
        app.passenger_controllers.insert(11, 1);
        let rider_rc = app.entity_population[&11].clone();

        app.relocate_entity_extras(&rider_rc);

        assert!(app.passenger_warnings.iter().any(|(entity, text)| *entity == 11
            && text == "PassengerExtra::relocated(11): Getting off our vehicle since we have just been onloaded into a cell where it does not exist"));
        assert!(
            !app.entity_population[&11]
                .borrow()
                .controllers
                .container
                .is_empty(),
            "no marker, no alight"
        );
    }

    /// `setAndDropGlobalPositionAndDirection` on a space that supports
    /// the drop point: no warning rides along.
    #[test]
    fn a_supported_drop_point_warns_nothing() {
        let mut app = plain_app();
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 11, [1.0, 0.0, 0.0]);
        let rider_rc = app.entity_population[&11].clone();

        app.set_and_drop_global_position_and_direction(
            &rider_rc,
            v([50.0, 0.0, 0.0]),
            v([0.0, 0.0, 0.0]),
        );
        almost_at(position_of(&app, 11), [50.0, 0.0, 0.0]);
        assert!(app.passenger_warnings.is_empty());
    }
}
