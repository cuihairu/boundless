//! BW-14-02-04: the Controller/Extra sample extension
//! (`examples/cellapp_extension/`), the reference's worked example for the
//! controller/extra machinery: `EgExtra`/`EgController`,
//! `SpeedExtra`/`SpeedController`, `BounceGrenadeExtra` and
//! `DumpControllersExtra`.
//!
//! The sample runs on the ported machinery: controllers attach through
//! `controller_extra::Controllers` (BW-08-02-02), a real-domain attach
//! surfaces as `ControllerEvent::Started` (`startReal`),
//! `EgController::startReal`/`stopReal` register/deregister with the update
//! registry (`egcontroller.cpp:17-23`, `CellApp::registerForUpdate`),
//! `SpeedExtra` rides the extra slots and streams its ghost state
//! (`speed_controller.cpp:74-95`), `BounceGrenadeExtra` simulates the
//! pinball loop over a collision query (`bounce_grenade_extra.cpp:105`)
//! with `physics::WorldTriangle::bounce` (`worldtri.cpp:504-512`), and
//! `DumpControllersExtra` walks the attached controllers
//! (`dump_controllers_extra.cpp:190`).
//!
//! The reference sample is a load-time-resolved shared library driven by a
//! running CellApp; the port drives a [`SampleEntity`] harness that stands
//! in for the Entity + CellApp pair, which is what makes registration,
//! callbacks and unload verifiable here. `debug_extension.cpp` (the BWDebug
//! streamed-property module) is not ported: it needs the cell-data property
//! streaming infrastructure, not the controller/extra machinery this task
//! is about.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use foundation::math::Vector3;
use physics::WorldTriangle;

use crate::controller_extra::{
    Controller, ControllerEvent, ControllerId, ControllerType, ControllerTypes, Controllers,
    Domain, EntityExtras, ExtraRegistry, UserArg,
};
use crate::tick::{Updatable, Updatables};

/// The sample controller types. The reference's `IMPLEMENT_CONTROLLER_TYPE`
/// assigns ids at load time; the port fixes them as constants.
pub const EG_CONTROLLER_TYPE: ControllerType = 1;
/// `SpeedController`'s type id.
pub const SPEED_CONTROLLER_TYPE: ControllerType = 2;

/// `CellAppConfig::absoluteMaxControllers()`: the collection's hard cap.
/// The reference default comes from the config; the harness fixes one.
const ABSOLUTE_MAX_CONTROLLERS: usize = 512;

/// `stampsPerSecondD()`: the reference reads the CPU counter's frequency;
/// the port fixes one scale, since the value only ever appears as a ratio
/// in the movement math (`speed_extra.cpp:154`).
pub const STAMPS_PER_SECOND: f64 = 1_000_000.0;

/// The failure modes the sample exposes to script, carrying the reference's
/// messages (`speed_extra.cpp:56`, `bounce_grenade_extra.cpp:98`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleError {
    /// `trackSpeed` on a ghost entity.
    TrackSpeedNotReal,
    /// `bounceGrenade` on an entity without a chunk space.
    NoPhysicalSpace,
}

impl SampleError {
    /// The reference's script-facing message.
    pub fn message(&self) -> &'static str {
        match self {
            SampleError::TrackSpeedNotReal => {
                "Entity.trackSpeed can only be called on a real entity"
            }
            SampleError::NoPhysicalSpace => {
                "BounceGrenadeExtra::bounceGrenade(): \
                 Entity physical space type is not supported "
            }
        }
    }
}

impl std::fmt::Display for SampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// The sample's four extra types, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleExtra {
    /// `EgExtra`.
    Eg,
    /// `SpeedExtra`.
    Speed,
    /// `BounceGrenadeExtra`.
    BounceGrenade,
    /// `DumpControllersExtra`.
    DumpControllers,
}

/// The collision query the grenade simulation rides on:
/// `ChunkSpace::collide(startPos, endPos, closestTriangle)`
/// (`bounce_grenade_extra.cpp:110`). The distance is the hit's distance
/// along the segment in world units, `0.0` when the segment is clear; the
/// triangle is the hit, transformed to world space
/// (`common/closest_triangle.hpp`).
pub trait CollideSpace {
    /// The segment query; `0.0` means clear.
    fn collide(&self, from: Vector3, to: Vector3) -> (f32, WorldTriangle);
}

/// `EgExtra::helloWorld` (`egextra.cpp:35`): the sample's debug greeting.
pub fn hello_world() -> &'static str {
    "egextra: hello world"
}

/// `EgController` (`egcontroller.cpp`): the DOMAIN_REAL sample controller.
/// `startReal` registers with the update registry and `stopReal`
/// deregisters (`egcontroller.cpp:17-23`); every update is a trace line,
/// which the port counts.
#[derive(Debug, Default)]
pub struct EgController {
    /// The number of `update()` calls since the controller started.
    pub ticks: u64,
}

impl Updatable for EgController {
    /// `EgController::update` (`egcontroller.cpp:27`).
    fn update(&mut self, _updatables: &mut Updatables) {
        self.ticks += 1;
    }
}

/// `SpeedExtra` (`speed_extra.hpp`): the per-entity speed tracker. The
/// read-only script attributes are `mySpeed`/`myVelocity`
/// (`speed_extra.cpp:23-26`).
#[derive(Debug, Clone, PartialEq)]
pub struct SpeedExtra {
    speed: f32,
    velocity: Vector3,
    /// `lastTime_`: the stamp of the last movement; `0` means never moved.
    last_time: u64,
    /// The attached `SpeedController` (`pController_`), `None` when
    /// tracking is off.
    controller: Option<ControllerId>,
}

impl SpeedExtra {
    /// The constructor (`speed_extra.cpp:37`): all zero.
    pub fn new() -> Self {
        SpeedExtra {
            speed: 0.0,
            velocity: Vector3::ZERO,
            last_time: 0,
            controller: None,
        }
    }

    /// `isEnabled()`: whether a controller is attached.
    pub fn is_enabled(&self) -> bool {
        self.controller.is_some()
    }

    /// `mySpeed`.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// `myVelocity`.
    pub fn velocity(&self) -> Vector3 {
        self.velocity
    }

    /// `onMovement(oldPosition)` (`speed_extra.cpp:150`): measure the
    /// velocity since the previous movement. The first movement only
    /// records the stamp (`lastTime_ == 0` skips the math).
    pub fn on_movement(
        &mut self,
        old_position: Vector3,
        new_position: Vector3,
        this_time: u64,
        stamps_per_second: f64,
    ) {
        if self.last_time != 0 && this_time > self.last_time {
            let delta = (this_time - self.last_time) as f64 / stamps_per_second;
            self.velocity = (new_position - old_position) / delta as f32;
            self.speed = (self.velocity.length() as f64 / delta) as f32;
        }
        self.last_time = this_time;
    }

    /// `writeToStream` (`speed_extra.cpp:106`): speed, velocity, then the
    /// remaining time offset — little-endian, like the port's other
    /// controller streams.
    pub fn write_to_stream(&self, now: u64) -> Vec<u8> {
        let time_offset = if self.last_time != 0 {
            now - self.last_time
        } else {
            0
        };
        let mut out = Vec::with_capacity(24);
        out.extend_from_slice(&self.speed.to_le_bytes());
        for component in [self.velocity.x, self.velocity.y, self.velocity.z] {
            out.extend_from_slice(&component.to_le_bytes());
        }
        out.extend_from_slice(&time_offset.to_le_bytes());
        out
    }

    /// `readFromStream` (`speed_extra.cpp:118`): restore speed and
    /// velocity; a non-zero time offset shifts `lastTime_` to
    /// `timestamp() + timeOffset`, since the stamp belongs to another
    /// machine's counter.
    pub fn read_from_stream(&mut self, bytes: &[u8], now: u64) -> Option<()> {
        if bytes.len() != 24 {
            return None;
        }
        self.speed = f32::from_le_bytes(bytes[0..4].try_into().ok()?);
        self.velocity = Vector3::new(
            f32::from_le_bytes(bytes[4..8].try_into().ok()?),
            f32::from_le_bytes(bytes[8..12].try_into().ok()?),
            f32::from_le_bytes(bytes[12..16].try_into().ok()?),
        );
        let time_offset = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
        if time_offset != 0 {
            self.last_time = now + time_offset;
        }
        Some(())
    }
}

impl Default for SpeedExtra {
    fn default() -> Self {
        SpeedExtra::new()
    }
}

/// `BounceGrenadeExtra::bounceGrenade` (`bounce_grenade_extra.cpp:96`):
/// estimate where a projectile comes to rest — pinball physics with the
/// reference's half-gravity per sample (`vel[1] += -9.8 * dTime * 0.5`,
/// `:116`), the contact handling of `:124-159` and the rest threshold of
/// `:168-176`. Returns the rest position and the simulated time
/// (`loopCount * timeSample`, `:196`), or [`SampleError::NoPhysicalSpace`]
/// when the entity has no chunk space (`:95`).
// The eight parameters are the sample's script signature
// (`bounce_grenade_extra.hpp:66-73`).
#[allow(clippy::too_many_arguments)]
pub fn bounce_grenade(
    space: Option<&dyn CollideSpace>,
    source_pos: Vector3,
    velocity: Vector3,
    elasticity: f32,
    radius: f32,
    time_sample: f32,
    max_samples: i32,
    max_bounces: i32,
) -> Result<(Vector3, f32), SampleError> {
    let Some(space) = space else {
        return Err(SampleError::NoPhysicalSpace);
    };

    let mut world_pos = source_pos;
    let mut vel = velocity;
    let d_time = time_sample;

    let mut done = false;
    let mut loop_count: i32 = 0;
    let mut bounce_count: i32 = 0;
    while !done {
        loop_count += 1;
        // The reference's hard-coded gravity constant (`:116`).
        vel.set(1, vel.get(1) + -9.8 * d_time * 0.5);

        let mut check_for_bounce = true;
        let mut bounced = false;
        let mut end_pos = world_pos;
        while check_for_bounce {
            check_for_bounce = false;

            let new_pos = end_pos + vel * d_time;
            let (distance, hit) = space.collide(end_pos, new_pos);

            if distance > 0.0 && distance <= (new_pos - end_pos).length() {
                bounced = true;
                let mut proportion_before_bounce = (distance - radius) / vel.length();
                if proportion_before_bounce < 0.0 {
                    proportion_before_bounce = 0.0;
                }
                end_pos = end_pos + vel * proportion_before_bounce;

                if max_bounces != -1 && bounce_count == max_bounces {
                    // Out of bounces: hold at the contact point.
                } else {
                    check_for_bounce = true;
                    hit.bounce(&mut vel, 1.0);
                    vel = vel * elasticity;
                    end_pos = end_pos + vel * (d_time - proportion_before_bounce);
                }
            } else {
                end_pos = new_pos;
            }
        }

        if bounced {
            bounce_count += 1;
            if max_bounces != -1 && bounce_count > max_bounces {
                done = true;
            }
        }

        // Revise the new position: below 0.25 units per second is rest
        // (`:168`), and `maxSamples` bounds the simulation (`:169`).
        if (end_pos - world_pos).length() / d_time < 0.25
            || (max_samples > 0 && loop_count >= max_samples)
        {
            done = true;
        }
        world_pos = end_pos;

        // Update velocity: at rest, the horizontal part clamps to zero
        // (`:174-176`).
        if vel.length() < 0.25 {
            vel = Vector3::new(0.0, vel.get(1), 0.0);
        }
    }

    Ok((world_pos, loop_count as f32 * time_sample))
}

/// The harness that stands in for the reference's Entity + CellApp pair:
/// the controller collection (`entity_.addController`,
/// `controllers.cpp:274`) and exclusive-class registry, the extra registry
/// and slots (`entity.cpp:7531`), the update registry
/// (`CellApp::registerForUpdate`) and the clock (`timestamp()`).
///
/// The controller collection's lifecycle events are translated into the
/// sample callbacks as they arrive (`SampleEntity::pump`): a `Started`
/// runs the started controller's `startReal` (and, for the
/// ghost-and-real `SpeedController`, its `startGhost` too — the harness
/// has no ghost entities), a `Stopped` runs `stopReal`/`stopGhost`.
pub struct SampleEntity {
    /// `Entity::isReal()`.
    real: bool,
    /// `Entity::position()`.
    position: Vector3,
    controllers: Controllers,
    types: ControllerTypes,
    extras: EntityExtras,
    updatables: Updatables,
    /// The registered extra type indices, by sample name.
    slots: [usize; 4],
    /// The `SpeedExtra::instance` payload, `Some` while the extra exists.
    speed: Option<SpeedExtra>,
    /// The live `EgController` behavior objects, by controller id.
    eg_controllers: BTreeMap<ControllerId, Rc<RefCell<EgController>>>,
    /// The attached controllers' types, by id — the container drops the
    /// controller before the `Stopped` event, so the type is remembered
    /// from the `Started`.
    live_types: BTreeMap<ControllerId, ControllerType>,
    /// How many `Controllers` events have been translated.
    pumped_events: usize,
    /// `timestamp()`.
    now: u64,
    /// `stampsPerSecondD()`.
    stamps_per_second: f64,
}

impl SampleEntity {
    /// A real entity with the sample's four extra types registered
    /// (`Entity::registerEntityExtra`, `entity.cpp:7543`).
    pub fn new() -> Self {
        let mut registry = ExtraRegistry::new();
        let slots = [
            registry.register("EgExtra"),
            registry.register("SpeedExtra"),
            registry.register("BounceGrenadeExtra"),
            registry.register("DumpControllersExtra"),
        ];
        SampleEntity {
            real: true,
            position: Vector3::ZERO,
            controllers: Controllers::new(ABSOLUTE_MAX_CONTROLLERS),
            types: ControllerTypes::new(),
            extras: EntityExtras::with_count(registry.len()),
            updatables: Updatables::new(2),
            slots,
            speed: None,
            eg_controllers: BTreeMap::new(),
            live_types: BTreeMap::new(),
            pumped_events: 0,
            now: 0,
            stamps_per_second: STAMPS_PER_SECOND,
        }
    }

    /// The extra's registered slot index.
    pub fn extra_slot(&self, which: SampleExtra) -> usize {
        self.slots[match which {
            SampleExtra::Eg => 0,
            SampleExtra::Speed => 1,
            SampleExtra::BounceGrenade => 2,
            SampleExtra::DumpControllers => 3,
        }]
    }

    /// `EntityExtra::Instance::operator()`: attach the extra's instance.
    /// Returns whether it was created now.
    pub fn touch_extra(&mut self, which: SampleExtra) -> bool {
        self.extras.touch(self.extra_slot(which))
    }

    /// `EntityExtra::Instance::exists`.
    pub fn extra_exists(&self, which: SampleExtra) -> bool {
        self.extras.exists(self.extra_slot(which))
    }

    /// `EntityExtra::Instance::clear`.
    pub fn clear_extra(&mut self, which: SampleExtra) -> bool {
        self.extras.clear(self.extra_slot(which))
    }

    /// `Entity::isReal()`.
    pub fn is_real(&self) -> bool {
        self.real
    }

    /// Turn the entity into a ghost or a real one (the test seam for
    /// `entity_.isReal()`).
    pub fn set_real(&mut self, real: bool) {
        self.real = real;
    }

    /// `Entity::position()`.
    pub fn position(&self) -> Vector3 {
        self.position
    }

    /// Advance `timestamp()`.
    pub fn advance(&mut self, stamps: u64) {
        self.now += stamps;
    }

    /// The number of attached controllers.
    pub fn controller_count(&self) -> usize {
        self.controllers.len()
    }

    /// The registered controller classes, for tests that need an
    /// exclusive controller.
    pub fn controller_types_mut(&mut self) -> &mut ControllerTypes {
        &mut self.types
    }

    /// The attached controllers' lifecycle events, in order.
    pub fn controller_events(&self) -> &[ControllerEvent] {
        self.controllers.events()
    }

    /// The number of objects registered for update
    /// (`CellApp::registerForUpdate` bookkeeping).
    pub fn updatable_count(&self) -> usize {
        self.updatables.size()
    }

    /// The CellApp tick: run one update pass over the registered objects.
    pub fn call_updatables(&mut self) {
        self.updatables.call();
    }

    /// The `EgController`'s update count, by controller id.
    pub fn eg_ticks(&self, id: ControllerId) -> Option<u64> {
        self.eg_controllers.get(&id).map(|cell| cell.borrow().ticks)
    }

    /// Translate the new `Controllers` lifecycle events into the sample's
    /// callbacks (`egcontroller.cpp:17-30`, `speed_controller.cpp:37-71`).
    fn pump(&mut self) {
        let events: Vec<ControllerEvent> = self.controllers.events()[self.pumped_events..].to_vec();
        self.pumped_events += events.len();
        for event in events {
            match event {
                ControllerEvent::Started { controller: id, .. } => {
                    match self.controllers.find(id).map(Controller::controller_type) {
                        Some(EG_CONTROLLER_TYPE) => {
                            self.live_types.insert(id, EG_CONTROLLER_TYPE);
                            // `startReal`: `CellApp::registerForUpdate(this)`.
                            let cell = Rc::new(RefCell::new(EgController::default()));
                            assert!(
                                self.updatables
                                    .add(cell.clone() as Rc<RefCell<dyn Updatable>>, 0),
                                "registerForUpdate must succeed"
                            );
                            self.eg_controllers.insert(id, cell);
                        }
                        Some(SPEED_CONTROLLER_TYPE) => {
                            self.live_types.insert(id, SPEED_CONTROLLER_TYPE);
                            // `startGhost` creates the SpeedExtra
                            // (`speed_controller.cpp:39-43`); `startReal`
                            // sets the controller (`:45-49`).
                            self.extras.touch(self.extra_slot(SampleExtra::Speed));
                            let speed = self.speed.get_or_insert_with(SpeedExtra::new);
                            speed.controller = Some(id);
                        }
                        Some(other) => {
                            self.live_types.insert(id, other);
                        }
                        // A Started whose controller already left the
                        // container cannot happen through the harness.
                        None => {}
                    }
                }
                ControllerEvent::Stopped { controller: id } => match self.live_types.remove(&id) {
                    Some(EG_CONTROLLER_TYPE) => {
                        // `stopReal`:
                        // `MF_VERIFY(CellApp::deregisterForUpdate(this))`.
                        let cell = self
                            .eg_controllers
                            .remove(&id)
                            .expect("a started EgController was never stopped");
                        assert!(
                            self.updatables
                                .remove(&(cell as Rc<RefCell<dyn Updatable>>)),
                            "MF_VERIFY(deregisterForUpdate)"
                        );
                    }
                    Some(SPEED_CONTROLLER_TYPE) => {
                        // `stopReal` clears the controller
                        // (`speed_controller.cpp:51-55`); `stopGhost`
                        // destroys the extra (`:57-63`).
                        if let Some(speed) = self.speed.as_mut() {
                            speed.controller = None;
                        }
                        self.extras.clear(self.extra_slot(SampleExtra::Speed));
                        self.speed = None;
                    }
                    _ => {}
                },
            }
        }
    }

    /// `Entity::addController` (`entity.cpp:7184`): attach any controller,
    /// returning its id (0 when the collection is full).
    pub fn add_controller(
        &mut self,
        controller: &mut Controller,
        user_arg: UserArg,
    ) -> ControllerId {
        let id = self
            .controllers
            .add_controller(controller, user_arg, &self.types);
        self.pump();
        id
    }

    /// `Entity::delController`: cancel a controller by id.
    pub fn del_controller(&mut self, id: ControllerId) -> bool {
        let removed = self.controllers.del_controller(id);
        if removed {
            self.pump();
        }
        removed
    }

    /// `EgExtra::addEgController` (`egextra.cpp:41`): attach an
    /// `EgController` with the script's `userArg`, returning its id.
    pub fn add_eg_controller(&mut self, user_arg: UserArg) -> ControllerId {
        let mut controller =
            Controller::new(EG_CONTROLLER_TYPE, "EgController", None, Domain::REAL);
        self.add_controller(&mut controller, user_arg)
    }

    /// `SpeedExtra::trackSpeed` (`speed_extra.cpp:50`): enable or disable
    /// speed tracking. A re-request of the current state warns and still
    /// succeeds — the reference returns true without touching the
    /// controller (`:64-71`).
    pub fn track_speed(&mut self, enable: bool) -> Result<bool, SampleError> {
        if !self.real {
            return Err(SampleError::TrackSpeedNotReal);
        }
        let enabled = self.speed.as_ref().is_some_and(SpeedExtra::is_enabled);
        if enable == enabled {
            // WARNING_MSG "Tracking for %d already %senabled".
            return Ok(true);
        }
        if enable {
            // `MF_ASSERT(pController_ == NULL)`, then
            // `entity_.addController(new SpeedController, 0)`.
            let mut controller = Controller::new(
                SPEED_CONTROLLER_TYPE,
                "SpeedController",
                None,
                Domain::GHOST_AND_REAL,
            );
            self.add_controller(&mut controller, 0);
        } else {
            let id = self
                .speed
                .as_ref()
                .and_then(|speed| speed.controller)
                .expect("pController_ != NULL when disabling");
            self.del_controller(id);
        }
        Ok(true)
    }

    /// `g_entityMovementCallback` (`speed_extra.cpp:167-181`): a tracked
    /// entity's SpeedExtra hears about the move.
    pub fn move_to(&mut self, new_position: Vector3) {
        let old = self.position;
        self.position = new_position;
        if self.extra_exists(SampleExtra::Speed)
            && let Some(speed) = self.speed.as_mut()
        {
            speed.on_movement(old, new_position, self.now, self.stamps_per_second);
        }
    }

    /// The tracked extra's read-only attributes (`mySpeed`/`myVelocity`).
    pub fn speed_extra(&self) -> Option<&SpeedExtra> {
        self.speed.as_ref()
    }

    /// `SpeedExtra::writeToStream` on the tracked extra, with the
    /// entity's current timestamp (`speed_controller.cpp:74-83`).
    pub fn write_speed_state(&self) -> Option<Vec<u8>> {
        self.speed
            .as_ref()
            .map(|speed| speed.write_to_stream(self.now))
    }

    /// `SpeedExtra::readFromStream` on the tracked extra, with this
    /// entity's timestamp standing in for the ghost's counter
    /// (`speed_controller.cpp:85-95`).
    pub fn read_speed_state(&mut self, bytes: &[u8]) -> Option<()> {
        let now = self.now;
        self.speed
            .as_mut()
            .and_then(|speed| speed.read_from_stream(bytes, now))
    }

    /// `DumpControllersExtra::visitControllers`
    /// (`dump_controllers_extra.cpp:190`): visit every attached
    /// controller, handing the callback its type name and exclusive id;
    /// the callback returns false to terminate the visit early.
    ///
    /// The reference validates that the callback is callable
    /// (`PyCallable_Check`, `:184`) and that every script conversion
    /// succeeded; a typed `FnMut` makes both unrepresentable, which is a
    /// recorded deviation. The visit always reports success.
    pub fn visit_controllers(
        &mut self,
        callback: &mut dyn FnMut(&str, ControllerId) -> bool,
    ) -> bool {
        let ids: Vec<ControllerId> = self.live_types.keys().copied().collect();
        for id in ids {
            let Some(controller) = self.controllers.find(id) else {
                continue;
            };
            let exclusive = controller.exclusive_id(&self.types);
            if !callback(controller.name(), exclusive) {
                break;
            }
        }
        true
    }

    /// Entity destruction (`entity.cpp:2896`): stop and clear every
    /// controller, then destroy the extras. Every registration the sample
    /// made is unwound, which is what makes unload verifiable.
    pub fn unload(&mut self) {
        self.controllers.destroy();
        self.pump();
        self.extras.clear_all();
        self.speed = None;
        debug_assert!(
            self.eg_controllers.is_empty(),
            "unload must deregister every sample controller"
        );
        debug_assert!(
            self.updatable_count() == 0,
            "unload must deregister every update"
        );
    }
}

impl Default for SampleEntity {
    fn default() -> Self {
        SampleEntity::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A horizontal floor at `y`, standing in for the chunk space in the
    /// grenade tests (`closest_triangle.hpp` records the hit triangle).
    struct Floor {
        y: f32,
    }

    impl CollideSpace for Floor {
        fn collide(&self, from: Vector3, to: Vector3) -> (f32, WorldTriangle) {
            if from.y > self.y && to.y < self.y {
                let t = (from.y - self.y) / (from.y - to.y);
                let distance = t * (to - from).length();
                let triangle = WorldTriangle::new(
                    Vector3::new(-1000.0, self.y, 1000.0),
                    Vector3::new(1000.0, self.y, 1000.0),
                    Vector3::new(-1000.0, self.y, -1000.0),
                    0,
                );
                (distance, triangle)
            } else {
                (
                    0.0,
                    WorldTriangle::new(Vector3::ZERO, Vector3::ZERO, Vector3::ZERO, 0),
                )
            }
        }
    }

    /// `EgExtra::helloWorld` (`egextra.cpp:35`) carries the sample's exact
    /// greeting, and the extra slots behave like the reference's
    /// `EntityExtra::Instance`.
    #[test]
    fn eg_extra_greets_and_attaches_once() {
        assert_eq!(hello_world(), "egextra: hello world");

        let mut entity = SampleEntity::new();
        assert!(!entity.extra_exists(SampleExtra::Eg));
        assert!(
            entity.touch_extra(SampleExtra::Eg),
            "created on first touch"
        );
        assert!(!entity.touch_extra(SampleExtra::Eg), "no second instance");
        assert!(entity.extra_exists(SampleExtra::Eg));
        assert!(entity.clear_extra(SampleExtra::Eg));
        assert!(!entity.extra_exists(SampleExtra::Eg));
        assert!(
            !entity.clear_extra(SampleExtra::Eg),
            "clearing twice is safe"
        );
    }

    /// `EgExtra::addEgController` (`egextra.cpp:41`): the controller
    /// attaches with the script's `userArg`, `startReal` registers it with
    /// the update registry, and the CellApp tick drives `update()`.
    #[test]
    fn add_eg_controller_starts_and_ticks() {
        let mut entity = SampleEntity::new();
        let id = entity.add_eg_controller(7);

        assert!(id > 0, "a fresh controller id");
        let events = entity.controller_events();
        assert_eq!(
            events,
            &[ControllerEvent::Started {
                controller: id,
                initial: true,
            }],
            "startReal fired on the real domain"
        );
        assert_eq!(entity.updatable_count(), 1, "registerForUpdate ran");

        entity.call_updatables();
        entity.call_updatables();
        entity.call_updatables();
        assert_eq!(entity.eg_ticks(id), Some(3), "update ran per tick");
    }

    /// Cancelling the controller runs `stopReal`
    /// (`egcontroller.cpp:21-23`): the update registration is gone and
    /// the ticks freeze.
    #[test]
    fn del_eg_controller_deregisters_from_the_update_loop() {
        let mut entity = SampleEntity::new();
        let id = entity.add_eg_controller(7);
        entity.call_updatables();
        assert_eq!(entity.eg_ticks(id), Some(1));

        assert!(entity.del_controller(id));
        assert!(!entity.del_controller(id), "already gone");
        assert_eq!(
            entity.controller_events(),
            &[
                ControllerEvent::Started {
                    controller: id,
                    initial: true,
                },
                ControllerEvent::Stopped { controller: id },
            ]
        );
        assert_eq!(entity.updatable_count(), 0, "deregisterForUpdate ran");
        entity.call_updatables();
        assert_eq!(entity.eg_ticks(id), None, "the controller is gone");
    }

    /// `trackSpeed` refuses to run on a ghost
    /// (`speed_extra.cpp:55-61`), with the reference's exact message.
    #[test]
    fn track_speed_refuses_on_a_ghost() {
        let mut entity = SampleEntity::new();
        entity.set_real(false);
        assert!(!entity.is_real());

        let error = entity.track_speed(true).unwrap_err();
        assert_eq!(error, SampleError::TrackSpeedNotReal);
        assert_eq!(
            error.to_string(),
            "Entity.trackSpeed can only be called on a real entity"
        );
        assert_eq!(entity.controller_count(), 0);
    }

    /// `trackSpeed(true)` attaches a `SpeedController` (which creates the
    /// extra and takes it over), `trackSpeed(false)` cancels it and
    /// destroys the extra (`speed_extra.cpp:66-77`,
    /// `speed_controller.cpp:37-63`).
    #[test]
    fn track_speed_enable_disable_round_trip() {
        let mut entity = SampleEntity::new();

        assert!(entity.track_speed(true).unwrap());
        assert_eq!(entity.controller_count(), 1, "the SpeedController attached");
        assert!(entity.extra_exists(SampleExtra::Speed), "startGhost ran");
        assert!(entity.speed_extra().unwrap().is_enabled(), "startReal ran");
        assert!(
            entity
                .controller_events()
                .iter()
                .any(|e| matches!(e, ControllerEvent::Started { .. })),
            "the ghost-and-real controller started"
        );

        // Move the entity so the extra carries measured state.
        entity.advance(STAMPS_PER_SECOND as u64);
        entity.move_to(Vector3::new(1.0, 0.0, 0.0));
        entity.advance(STAMPS_PER_SECOND as u64 / 2);
        entity.move_to(Vector3::new(3.0, 2.0, 0.0));
        assert!(entity.speed_extra().unwrap().speed() > 0.0);

        assert!(entity.track_speed(false).unwrap());
        assert_eq!(entity.controller_count(), 0, "stop() cancelled it");
        assert!(!entity.extra_exists(SampleExtra::Speed), "stopGhost ran");
        assert!(entity.speed_extra().is_none(), "the extra was destroyed");
    }

    /// A re-request of the current tracking state warns and succeeds
    /// without touching the controller (`speed_extra.cpp:63-71`).
    #[test]
    fn track_speed_is_idempotent_at_both_ends() {
        let mut entity = SampleEntity::new();

        assert!(entity.track_speed(true).unwrap());
        assert!(entity.track_speed(true).unwrap(), "already enabled");
        assert_eq!(entity.controller_count(), 1, "no second controller");

        assert!(entity.track_speed(false).unwrap());
        assert!(entity.track_speed(false).unwrap(), "already disabled");
        assert_eq!(entity.controller_count(), 0, "nothing was created");
    }

    /// `onMovement` (`speed_extra.cpp:150-165`): the first movement only
    /// records the stamp; the next one measures velocity and speed over
    /// the elapsed stamps.
    #[test]
    fn on_movement_measures_velocity_between_stamps() {
        let mut entity = SampleEntity::new();
        assert!(entity.track_speed(true).unwrap());

        // The first movement: lastTime_ is still 0, only the stamp lands.
        entity.advance(STAMPS_PER_SECOND as u64);
        entity.move_to(Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(entity.speed_extra().unwrap().speed(), 0.0);

        // Half a second later: velocity = (2, 2, 0)/0.5, speed = |v|/0.5.
        entity.advance(STAMPS_PER_SECOND as u64 / 2);
        entity.move_to(Vector3::new(3.0, 2.0, 0.0));
        let extra = entity.speed_extra().unwrap();
        assert_eq!(extra.velocity(), Vector3::new(4.0, 4.0, 0.0));
        assert!(
            (extra.speed() - 11.313_708).abs() < 1e-3,
            "{}",
            extra.speed()
        );
    }

    /// The ghost stream (`speed_extra.cpp:106-133`): speed, velocity and
    /// the time offset round-trip; reading shifts `lastTime_` onto the
    /// reading machine's counter.
    #[test]
    fn speed_ghost_state_round_trips_through_the_stream() {
        let mut entity = SampleEntity::new();
        assert!(entity.track_speed(true).unwrap());
        entity.advance(STAMPS_PER_SECOND as u64);
        entity.move_to(Vector3::new(1.0, 0.0, 0.0));
        entity.advance(STAMPS_PER_SECOND as u64 / 2);
        entity.move_to(Vector3::new(3.0, 2.0, 0.0));
        let original = entity.speed_extra().unwrap().clone();

        entity.advance(STAMPS_PER_SECOND as u64 / 4);
        let bytes = entity.write_speed_state().unwrap();
        assert_eq!(bytes.len(), 24, "f32 + Vector3 + u64");

        // The ghost restores onto its own counter: lastTime_ = now + offset.
        let mut ghost = SampleEntity::new();
        assert!(ghost.track_speed(true).unwrap());
        ghost.advance(5);
        assert!(ghost.read_speed_state(&bytes).is_some());
        let restored = ghost.speed_extra().unwrap();
        assert_eq!(restored.velocity(), original.velocity());
        assert_eq!(restored.speed(), original.speed());
        assert_eq!(restored.velocity(), Vector3::new(4.0, 4.0, 0.0));

        // The offset was written with the source's clock: now - lastTime.
        assert_eq!(bytes[16..24], 250_000u64.to_le_bytes());
        // A short stream is refused.
        assert!(ghost.read_speed_state(&bytes[..20]).is_none());
        // A never-moved extra streams a zero offset and keeps lastTime 0.
        let fresh = SpeedExtra::new().write_to_stream(9);
        let mut target = SpeedExtra::new();
        assert!(target.read_from_stream(&fresh, 5).is_some());
        assert_eq!(
            target.write_to_stream(9),
            fresh,
            "the zero offset does not shift lastTime"
        );
    }

    /// `bounceGrenade` without a chunk space is refused
    /// (`bounce_grenade_extra.cpp:94-101`), with the reference's message.
    #[test]
    fn bounce_grenade_without_a_space_is_refused() {
        let error =
            bounce_grenade(None, Vector3::ZERO, Vector3::ZERO, 0.3, 0.1, 0.1, 100, -1).unwrap_err();
        assert_eq!(error, SampleError::NoPhysicalSpace);
        assert_eq!(
            error.to_string(),
            "BounceGrenadeExtra::bounceGrenade(): \
             Entity physical space type is not supported "
        );
    }

    /// The full simulation (`bounce_grenade_extra.cpp:105-197`): a
    /// lobbed grenade bounces on the floor, loses energy to the
    /// elasticity, and comes to rest below the 0.25 threshold.
    #[test]
    fn bounce_grenade_comes_to_rest_on_a_floor() {
        let floor = Floor { y: 0.0 };
        let (rest, simulated) = bounce_grenade(
            Some(&floor),
            Vector3::new(0.0, 10.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            0.3,
            0.1,
            0.1,
            0,  // unlimited samples
            -1, // unlimited bounces
        )
        .expect("a chunk space was given");

        assert!(
            rest.y >= -1e-3 && rest.y < 1.0,
            "rest near the floor: {rest:?}"
        );
        assert!(rest.x > 0.0, "it travelled forward: {rest:?}");
        assert!(simulated > 0.0);
        // The simulated time is the loop count times the sample.
        assert!(
            ((simulated / 0.1).round() - simulated / 0.1).abs() < 1e-3,
            "whole loops: {simulated}"
        );
    }

    /// `maxSamples` bounds the simulation (`bounce_grenade_extra.cpp:169`):
    /// exactly that many loops run, and the reported time matches.
    #[test]
    fn bounce_grenade_stops_at_max_samples() {
        let floor = Floor { y: 0.0 };
        let (rest, simulated) = bounce_grenade(
            Some(&floor),
            Vector3::new(0.0, 10.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            0.3,
            0.1,
            0.1,
            3, // three samples only
            -1,
        )
        .expect("a chunk space was given");

        assert!(
            (simulated - 0.3).abs() < 1e-5,
            "3 loops x 0.1s: {simulated}"
        );
        // Still high in the air when the samples run out.
        assert!(rest.y > 5.0, "stopped early: {rest:?}");
    }

    /// `maxBounces` stops the simulation at the contact point
    /// (`bounce_grenade_extra.cpp:143-149, 163-166`): after the cap is
    /// reached the grenade holds instead of reflecting again.
    #[test]
    fn bounce_grenade_holds_at_the_bounce_cap() {
        let floor = Floor { y: 0.0 };
        let capped = bounce_grenade(
            Some(&floor),
            Vector3::new(0.0, 10.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            0.3,
            0.1,
            0.1,
            0,
            1, // one bounce only
        )
        .expect("a chunk space was given");
        let uncapped = bounce_grenade(
            Some(&floor),
            Vector3::new(0.0, 10.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            0.3,
            0.1,
            0.1,
            0,
            -1,
        )
        .expect("a chunk space was given");

        assert!(
            capped.1 < uncapped.1,
            "the cap stops the simulation earlier: {} vs {}",
            capped.1,
            uncapped.1
        );
        // The capped grenade rests on the floor too — it just stops there.
        assert!(capped.0.y < 1.0, "near the floor: {:?}", capped.0);
    }

    /// `visitControllers` (`dump_controllers_extra.cpp:190`): every
    /// attached controller is visited with its type name and exclusive
    /// id; returning false terminates the visit early.
    #[test]
    fn visit_controllers_reports_name_and_exclusive_id() {
        let mut entity = SampleEntity::new();
        let movement_exclusive = entity.controller_types_mut().register_exclusive("Movement");
        let eg_id = entity.add_eg_controller(3);
        let mut movement = Controller::new(7, "MovementController", Some("Movement"), Domain::REAL);
        let movement_id = entity.add_controller(&mut movement, 0);
        assert_ne!(eg_id, movement_id);

        let mut visited = Vec::new();
        let complete = entity.visit_controllers(&mut |name, exclusive| {
            visited.push((name.to_string(), exclusive));
            true
        });
        assert!(complete, "the visit succeeded");
        // The reference hands the callback the exclusive id, not the
        // controller id (`dump_controllers_extra.cpp:104`).
        assert_eq!(
            visited,
            vec![
                ("EgController".to_string(), 0),
                ("MovementController".to_string(), movement_exclusive),
            ]
        );

        // Early termination: the first false stops the visit.
        let mut first_only = Vec::new();
        assert!(entity.visit_controllers(&mut |name, _| {
            first_only.push(name.to_string());
            false
        }));
        assert_eq!(first_only, vec!["EgController".to_string()]);
        // The registered exclusive class id sits above the mask.
        assert_eq!(movement_exclusive, 0x7fff + 1);

        // Unloading runs every stop: the untyped controller's stop is a
        // no-op in the harness.
        entity.unload();
        assert_eq!(entity.controller_count(), 0);
        assert_eq!(entity.updatable_count(), 0);
    }

    /// The harness defaults mirror the constructors: a real entity at the
    /// origin with the sample's extra slots, and an empty controller
    /// collection. Also exercises the machinery's small public faces
    /// (`id()`, `is_empty()`) through the harness types.
    #[test]
    fn defaults_mirror_the_constructors() {
        let entity = SampleEntity::new();
        assert_eq!(entity.position(), Vector3::ZERO);
        assert!(entity.is_real());
        assert_eq!(entity.controller_count(), 0);
        assert_eq!(entity.updatable_count(), 0);
        assert!(entity.speed_extra().is_none());
        assert!(!entity.extra_exists(SampleExtra::Speed));

        // `SpeedExtra`/`SampleEntity` `Default` == `new()`.
        assert_eq!(SpeedExtra::default(), SpeedExtra::new());
        let typed = SampleEntity::default();
        assert_eq!(typed.position(), entity.position());
        assert_eq!(SampleEntity::default().extra_slot(SampleExtra::Speed), 1);

        // The machinery's faces, straight off its types.
        let types = ControllerTypes::new();
        assert!(types.is_empty());
        assert!(ExtraRegistry::new().is_empty());
        assert!(Controllers::new(4).is_empty());
        assert!(EntityExtras::with_count(0).is_empty());
        assert!(!Domain::REAL.has_ghost() && Domain::GHOST.has_ghost());
        assert!(
            !EntityExtras::with_count(0).clear(0),
            "no slot, nothing to clear"
        );
        let mut controllers = Controllers::new(4);
        let mut controller = Controller::new(1, "X", None, Domain::REAL);
        let id = controllers.add_controller(&mut controller, 2, &types);
        assert_eq!(controllers.find(id).unwrap().id(), id);
    }

    /// A movement callback for an untracked entity measures nothing
    /// (`g_entityMovementCallback` checks the extra's existence first).
    #[test]
    fn an_untracked_entity_ignores_movements() {
        let mut entity = SampleEntity::new();
        entity.advance(STAMPS_PER_SECOND as u64);
        entity.move_to(Vector3::new(5.0, 0.0, 0.0));
        entity.advance(STAMPS_PER_SECOND as u64);
        entity.move_to(Vector3::new(0.0, 0.0, 0.0));
        assert!(entity.speed_extra().is_none(), "nothing was ever attached");
    }

    /// A hit closer than the grenade's radius clamps the pre-bounce
    /// proportion to zero (`bounce_grenade_extra.cpp:129-135`): the
    /// contact point is the segment start and the whole step runs on the
    /// reflected velocity.
    #[test]
    fn bounce_grenade_clamps_a_hit_inside_the_radius() {
        use std::cell::Cell;

        // Hits once, right at the start, then stays clear forever.
        struct OneShot {
            distance: f32,
            hit: Cell<bool>,
        }

        impl CollideSpace for OneShot {
            fn collide(&self, _from: Vector3, to: Vector3) -> (f32, WorldTriangle) {
                if self.hit.get() {
                    return (
                        0.0,
                        WorldTriangle::new(Vector3::ZERO, Vector3::ZERO, Vector3::ZERO, 0),
                    );
                }
                self.hit.set(true);
                (
                    self.distance,
                    WorldTriangle::new(
                        Vector3::new(-1000.0, to.y - 1.0, 1000.0),
                        Vector3::new(1000.0, to.y - 1.0, 1000.0),
                        Vector3::new(-1000.0, to.y - 1.0, -1000.0),
                        0,
                    ),
                )
            }
        }

        let seam = OneShot {
            distance: 0.05,
            hit: Cell::new(false),
        };
        let (rest, simulated) = bounce_grenade(
            Some(&seam),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(5.0, 0.0, 0.0),
            0.3,
            0.5, // the hit (0.05) is inside this radius
            0.1,
            5,
            -1,
        )
        .expect("a chunk space was given");

        assert!(
            (simulated - 0.5).abs() < 1e-5,
            "5 loops x 0.1s: {simulated}"
        );
        // It stopped early because the samples ran out, still flying.
        assert!(rest.y > -1e-3, "above the floor: {rest:?}");
    }

    /// A second `SpeedController` attach finds the extra already created,
    /// and each controller's stop sees the extra's real lifecycle state
    /// (`speed_controller.cpp:37-71` — a raw attach, the `MF_ASSERT`'s
    /// neighbourhood in the reference).
    #[test]
    fn a_second_speed_controller_stop_finds_the_extra_gone() {
        let mut entity = SampleEntity::new();
        assert!(entity.track_speed(true).unwrap());
        let first = entity.speed.as_ref().unwrap().controller;
        let mut second = Controller::new(
            SPEED_CONTROLLER_TYPE,
            "SpeedController",
            None,
            Domain::GHOST_AND_REAL,
        );
        let second_id = entity.add_controller(&mut second, 0);
        assert_eq!(entity.controller_count(), 2, "both attaches live");
        assert!(entity.speed_extra().unwrap().is_enabled());

        // The first stop releases the whole extra...
        assert!(entity.del_controller(first.unwrap()));
        assert!(entity.speed.is_none());
        assert!(!entity.extra_exists(SampleExtra::Speed));
        // ...so the second one finds it already gone (no panic, no leak).
        assert!(entity.del_controller(second_id));
        assert_eq!(entity.controller_count(), 0);
    }

    /// Unload (`entity.cpp:2896`): every controller stops, every extra
    /// clears, every update registration unwinds — and doing it twice is
    /// safe.
    #[test]
    fn unload_unwinds_every_registration_and_is_safe_twice() {
        let mut entity = SampleEntity::new();
        let eg_id = entity.add_eg_controller(1);
        assert!(entity.track_speed(true).unwrap());
        entity.touch_extra(SampleExtra::Eg);
        entity.call_updatables();
        entity.call_updatables();
        assert_eq!(entity.eg_ticks(eg_id), Some(2));
        assert_eq!(entity.controller_count(), 2);
        assert_eq!(entity.updatable_count(), 1);

        entity.unload();
        assert_eq!(entity.controller_count(), 0, "every controller stopped");
        assert_eq!(entity.updatable_count(), 0, "every update deregistered");
        assert_eq!(entity.eg_ticks(eg_id), None);
        assert!(entity.speed_extra().is_none());
        assert!(!entity.extra_exists(SampleExtra::Speed));
        assert!(!entity.extra_exists(SampleExtra::Eg), "extras destroyed");

        // A second unload is a no-op, and the tick loop stays empty.
        entity.unload();
        entity.call_updatables();
        assert_eq!(entity.controller_count(), 0);
        assert_eq!(entity.updatable_count(), 0);
    }

    /// The extras registry hands the sample's four types stable indices,
    /// and each extra type is an independent slot.
    #[test]
    fn extra_slots_are_stable_and_independent() {
        let mut entity = SampleEntity::new();
        let eg = entity.extra_slot(SampleExtra::Eg);
        let speed = entity.extra_slot(SampleExtra::Speed);
        let bounce = entity.extra_slot(SampleExtra::BounceGrenade);
        let dump = entity.extra_slot(SampleExtra::DumpControllers);
        assert_eq!([eg, speed, bounce, dump], [0, 1, 2, 3]);

        assert!(entity.touch_extra(SampleExtra::BounceGrenade));
        assert!(entity.extra_exists(SampleExtra::BounceGrenade));
        assert!(!entity.extra_exists(SampleExtra::DumpControllers));
        assert!(entity.clear_extra(SampleExtra::BounceGrenade));
    }
}
