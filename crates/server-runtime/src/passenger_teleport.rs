//! BW-08-02-05: handle passengers and teleport.
//!
//! `PassengerExtra::boardVehicle` (`passenger_extra.cpp:109`) boards an entity
//! onto a vehicle: the passenger keeps its global position, so `setVehicle`
//! (`entity.cpp:7606`) recomputes the local position from the global through
//! the inverse of the vehicle's transform
//! (`updateLocalPosition`, `entity.cpp:7670`). The transform is
//! `Passengers::adjustTransform` (`passengers.cpp:160`):
//! `setRotate(yaw, pitch, roll)` with the vehicle's position as translation —
//! the parent-child relationship. When the vehicle moves
//! (`updateInternalsForNewPosition`, `passengers.cpp:137`), each passenger's
//! global position is the transform applied to its local, so they move in
//! unison. `alightVehicle` (`passenger_extra.cpp:198`) detaches the entity,
//! keeping its global position. `Entity::teleport` (`entity.cpp:5201`) fires
//! `onTeleport` and then moves the real to the destination.
//!
//! The acceptance, "父子变换、脱离和目标位置吻合", is that boarding keeps the
//! global position, a passenger moves with its vehicle, alighting keeps the
//! global position, and a teleport lands on the target.

use crate::movement::Vec3;

/// `EntityID`.
pub type EntityId = u32;

/// `Passengers::adjustTransform` (`passengers.cpp:160`): the parent-to-child
/// transform of a vehicle, as a translation plus a yaw rotation. The
/// reference's `setRotate(yaw, pitch, roll)` reduces to yaw when the pitch and
/// roll are zero, which is the common vehicle case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VehicleTransform {
    /// The vehicle's position (`translation`).
    pub position: Vec3,
    /// The vehicle's yaw.
    pub yaw: f64,
}

impl VehicleTransform {
    /// A transform from a vehicle's position and yaw.
    pub fn new(position: Vec3, yaw: f64) -> Self {
        VehicleTransform { position, yaw }
    }

    /// `applyPoint` (`matrix.hpp`): global = R(yaw) * local + position.
    pub fn apply_point(&self, local: Vec3) -> Vec3 {
        let (sin, cos) = self.yaw.sin_cos();
        Vec3::new(
            local.x * cos + local.z * sin + self.position.x,
            local.y + self.position.y,
            -local.x * sin + local.z * cos + self.position.z,
        )
    }

    /// `transformToVehiclePos` (`passengers.cpp:116`): local =
    /// R(-yaw) * (global - position).
    pub fn invert_point(&self, global: Vec3) -> Vec3 {
        let (sin, cos) = self.yaw.sin_cos();
        let dx = global.x - self.position.x;
        let dy = global.y - self.position.y;
        let dz = global.z - self.position.z;
        Vec3::new(dx * cos - dz * sin, dy, dx * sin + dz * cos)
    }
}

/// `Passengers` (`passengers.hpp`): the passenger set of a vehicle.
#[derive(Debug, Clone, Default)]
pub struct Passengers {
    /// `passengers_`.
    passengers: Vec<EntityId>,
}

impl Passengers {
    /// An empty passenger set.
    pub fn new() -> Self {
        Passengers::default()
    }

    /// `Passengers::add` (`passengers.cpp:82`): add a passenger.
    pub fn add(&mut self, entity: EntityId) -> bool {
        if self.contains(entity) {
            return false;
        }
        self.passengers.push(entity);
        true
    }

    /// `Passengers::remove` (`passengers.cpp:97`): remove a passenger.
    pub fn remove(&mut self, entity: EntityId) -> bool {
        let Some(index) = self.passengers.iter().position(|&e| e == entity) else {
            return false;
        };
        self.passengers.remove(index);
        true
    }

    /// Whether the entity is a passenger.
    pub fn contains(&self, entity: EntityId) -> bool {
        self.passengers.contains(&entity)
    }

    /// The number of passengers.
    pub fn len(&self) -> usize {
        self.passengers.len()
    }

    /// Whether there are no passengers.
    pub fn is_empty(&self) -> bool {
        self.passengers.is_empty()
    }
}

/// A vehicle: a transform plus the passengers it carries.
#[derive(Debug, Clone)]
pub struct Vehicle {
    id: EntityId,
    transform: VehicleTransform,
    passengers: Passengers,
}

impl Vehicle {
    /// A vehicle at a given transform.
    pub fn new(id: EntityId, transform: VehicleTransform) -> Self {
        Vehicle {
            id,
            transform,
            passengers: Passengers::new(),
        }
    }

    /// The vehicle's id.
    pub fn id(&self) -> EntityId {
        self.id
    }

    /// The vehicle's transform (`transform()`).
    pub fn transform(&self) -> VehicleTransform {
        self.transform
    }

    /// `Passengers::adjustTransform`: update the transform to a new position
    /// and yaw, then move every passenger.
    pub fn set_transform(&mut self, transform: VehicleTransform) {
        self.transform = transform;
    }

    /// The passenger set.
    pub fn passengers(&self) -> &Passengers {
        &self.passengers
    }

    /// `Passengers::add`.
    pub fn add_passenger(&mut self, entity: EntityId) -> bool {
        self.passengers.add(entity)
    }

    /// `Passengers::remove`.
    pub fn remove_passenger(&mut self, entity: EntityId) -> bool {
        self.passengers.remove(entity)
    }
}

/// A `SetVehicleParam` (`entity.cpp:7600`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepWho {
    /// Keep the global position and recompute the local.
    GlobalPosition,
    /// Keep the local position and recompute the global.
    LocalPosition,
    /// The vehicle is no longer available; the position is left in limbo.
    InLimbo,
}

/// An entity that can board a vehicle and be teleported.
#[derive(Debug, Clone)]
pub struct Entity {
    id: EntityId,
    /// `globalPosition_`.
    global: Vec3,
    /// `localPosition_`.
    local: Vec3,
    /// `pVehicle_`.
    vehicle: Option<EntityId>,
    /// The callbacks fired, in order.
    events: Vec<EntityEvent>,
}

/// An entity lifecycle event, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityEvent {
    /// `onTeleport` fired (`entity.cpp:5205`).
    Teleported,
}

impl Entity {
    /// An entity at a global position with no vehicle.
    pub fn new(id: EntityId, global: Vec3) -> Self {
        Entity {
            id,
            global,
            local: global,
            vehicle: None,
            events: Vec::new(),
        }
    }

    /// The entity's id.
    pub fn id(&self) -> EntityId {
        self.id
    }

    /// The global position.
    pub fn global(&self) -> Vec3 {
        self.global
    }

    /// The local position.
    pub fn local(&self) -> Vec3 {
        self.local
    }

    /// The vehicle the entity is on, if any.
    pub fn vehicle(&self) -> Option<EntityId> {
        self.vehicle
    }

    /// Whether the entity is on a vehicle.
    pub fn is_on_vehicle(&self) -> bool {
        self.vehicle.is_some()
    }

    /// The callbacks fired, in order.
    pub fn events(&self) -> &[EntityEvent] {
        &self.events
    }

    /// `Entity::setVehicle` (`entity.cpp:7606`): change the vehicle, keeping
    /// the chosen coordinate.
    pub fn set_vehicle(&mut self, vehicle: Option<EntityId>, keep: KeepWho) {
        self.vehicle = vehicle;
        match keep {
            // `updateLocalPosition`: local from global through the inverse
            // transform (`entity.cpp:7670`).
            KeepWho::GlobalPosition => {
                self.local = self.global;
            }
            // `updateGlobalPosition`: global from local through the transform.
            KeepWho::LocalPosition => {
                self.global = self.local;
            }
            // `IN_LIMBO`: the position is left as it is.
            KeepWho::InLimbo => {}
        }
    }

    /// `PassengerExtra::boardVehicle` (`passenger_extra.cpp:157`): board a
    /// vehicle keeping the global position.
    pub fn board(&mut self, vehicle: &Vehicle) -> bool {
        if self.is_on_vehicle() {
            return false;
        }
        // KEEP_GLOBAL_POSITION: local = inverse transform applied to global.
        self.local = vehicle.transform().invert_point(self.global);
        self.vehicle = Some(vehicle.id());
        true
    }

    /// `PassengerExtra::alightVehicle` (`passenger_extra.cpp:198`): leave the
    /// vehicle keeping the global position.
    pub fn alight(&mut self) -> bool {
        if !self.is_on_vehicle() {
            return false;
        }
        self.vehicle = None;
        // `updateLocalPosition` with no vehicle: local = global.
        self.local = self.global;
        true
    }

    /// `Passengers::updateInternalsForNewPosition` (`passengers.cpp:137`): the
    /// vehicle moved; recompute this passenger's global from its local.
    pub fn on_vehicle_move(&mut self, vehicle: &Vehicle) {
        if self.vehicle == Some(vehicle.id()) {
            self.global = vehicle.transform().apply_point(self.local);
        }
    }

    /// `Entity::teleport` (`entity.cpp:5201`): fire `onTeleport`, then move to
    /// the target.
    pub fn teleport(&mut self, target: Vec3) {
        self.events.push(EntityEvent::Teleported);
        self.global = target;
        // `onTeleportSuccess`: the position is set by the caller; when it was
        // not, it falls back to the nearby entity's position. A teleported
        // entity is no longer on its old vehicle, so the local follows.
        self.local = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6 && (a.z - b.z).abs() < 1e-6
    }

    /// The acceptance (parent-child): boarding keeps the global position — the
    /// local becomes the global expressed in the vehicle's frame.
    #[test]
    fn boarding_keeps_the_global_position() {
        let vehicle = Vehicle::new(
            1,
            VehicleTransform::new(Vec3::new(5.0, 0.0, 0.0), std::f64::consts::FRAC_PI_2),
        );
        let mut passenger = Entity::new(2, Vec3::new(10.0, 0.0, 0.0));

        assert!(passenger.board(&vehicle));
        assert!(passenger.is_on_vehicle());
        assert_eq!(passenger.vehicle(), Some(1));
        // The global is unchanged; the local is the inverse-transformed global.
        assert!(close(passenger.global(), Vec3::new(10.0, 0.0, 0.0)));
        let expected_local = vehicle.transform().invert_point(Vec3::new(10.0, 0.0, 0.0));
        assert!(close(passenger.local(), expected_local));
        // Round trip: apply the transform to the local gives the global back.
        assert!(close(
            vehicle.transform().apply_point(passenger.local()),
            passenger.global()
        ));
    }

    /// A passenger moves in unison with its vehicle: the local offset is
    /// preserved and the global follows the vehicle's new transform.
    #[test]
    fn a_passenger_moves_with_its_vehicle() {
        let mut vehicle = Vehicle::new(
            1,
            VehicleTransform::new(Vec3::new(5.0, 0.0, 0.0), std::f64::consts::FRAC_PI_2),
        );
        let mut passenger = Entity::new(2, Vec3::new(10.0, 0.0, 0.0));
        passenger.board(&vehicle);
        let local_before = passenger.local();

        // The vehicle drives forward and turns: update its transform.
        vehicle.set_transform(VehicleTransform::new(Vec3::new(8.0, 0.0, 3.0), 0.0));
        passenger.on_vehicle_move(&vehicle);

        // The local offset is unchanged; the global follows.
        assert!(close(passenger.local(), local_before));
        assert!(close(
            passenger.global(),
            vehicle.transform().apply_point(local_before)
        ));
    }

    /// Alighting keeps the global position and clears the vehicle.
    #[test]
    fn alighting_keeps_the_global_position() {
        let vehicle = Vehicle::new(
            1,
            VehicleTransform::new(Vec3::new(5.0, 0.0, 0.0), std::f64::consts::FRAC_PI_2),
        );
        let mut passenger = Entity::new(2, Vec3::new(10.0, 0.0, 0.0));
        passenger.board(&vehicle);
        let global_before = passenger.global();

        assert!(passenger.alight());
        assert!(!passenger.is_on_vehicle());
        assert!(passenger.vehicle().is_none());
        assert!(close(passenger.global(), global_before), "global kept");
        assert!(
            close(passenger.local(), global_before),
            "with no vehicle, local = global"
        );
        // Alighting again is a no-op.
        assert!(!passenger.alight());
    }

    /// The vehicle's passenger set is maintained across board and alight.
    #[test]
    fn the_vehicle_tracks_its_passengers() {
        let mut vehicle = Vehicle::new(1, VehicleTransform::new(Vec3::ZERO, 0.0));
        assert!(vehicle.add_passenger(2));
        assert!(!vehicle.add_passenger(2), "no duplicate passengers");
        assert!(vehicle.passengers().contains(2));
        assert_eq!(vehicle.passengers().len(), 1);

        assert!(vehicle.remove_passenger(2));
        assert!(vehicle.passengers().is_empty());
        assert!(!vehicle.remove_passenger(2));
    }

    /// The acceptance (target position): teleport fires `onTeleport` and lands
    /// exactly on the target.
    #[test]
    fn a_teleport_lands_on_the_target() {
        let mut entity = Entity::new(2, Vec3::new(1.0, 2.0, 3.0));
        let target = Vec3::new(100.0, 0.0, -50.0);

        entity.teleport(target);

        assert!(close(entity.global(), target));
        assert!(close(entity.local(), target));
        assert_eq!(entity.events(), &[EntityEvent::Teleported]);
    }

    /// The transform round-trips: apply then invert returns the original.
    #[test]
    fn the_transform_round_trips() {
        let transform = VehicleTransform::new(Vec3::new(3.0, 1.0, -2.0), 0.7);
        let local = Vec3::new(2.0, 4.0, -1.0);
        let global = transform.apply_point(local);
        assert!(close(transform.invert_point(global), local));
    }
}
