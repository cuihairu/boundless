//! BW-08-02-01 creating a real entity and advancing the tick.
//!
//! `Cell::createEntityInternal` (`server/cellapp/cell.cpp:302`) builds the
//! entity: it reads the id and the type id (an id of 0 allocates from the
//! `IDClient` pool), refuses a live duplicate (`%s %u already exists!`,
//! returning the existing entity), then asks `Space::newEntity`
//! (`space.cpp:523`) for the entity — a destroyed population entry is
//! revived via `setToInitialState`, a fresh one needs a type with a cell
//! entity class — and only then does it open the buffered-callbacks
//! section. `Entity::initReal` (`entity.cpp:1425`) streams the position,
//! the `AddEntityData` flags, the direction, the last event number and the
//! base address, adds the entity to the space, reads the Witness indicator
//! (`RealEntity::init`), and — inside high-priority buffering — calls the
//! scripted `__init__` (`callScriptInit`; `onRestore` is the restore
//! face). The cell then adds its real (`Cell::addRealEntity`; the
//! `currentCell` send is delayed so it rides the backup bundle) and closes
//! the section: the buffered `__init__` runs first, and everything the
//! script raised in it follows in call order.
//!
//! `EntityCallbackBuffer` (`entity_callback_buffer.{hpp,cpp}`) is the
//! ordering machinery: a call made while a section is open is stored —
//! high priority while `allowCallbacksOverride_ >= callbacksPrevented_`
//! — and on the last `leaveBufferedSection` both buffers swap out and
//! execute, high priority first. A stored call whose entity was destroyed
//! in the meantime is skipped with a WARNING; a call arriving with no
//! section open runs immediately.
//!
//! The tick face is `TimerController` (`timer_controller.cpp`):
//! `Entity.addTimer(start, repeat, userArg)` rounds the offsets to game
//! ticks at `updateHertz` (`TimerController::New`: `(GameTime)(offset *
//! hertz + 0.5f)`, a non-positive start rounds up to 1, a positive repeat
//! below one tick warns and rounds up to 1, then the app time is added),
//! `Controllers::addController` (`controllers.cpp:277`) refuses over
//! `absoluteMaxControllers` with 0, warns over `expectedMaxControllers`,
//! allocates the id (`nextControllerID`: step under
//! `EXCLUSIVE_CONTROLLER_ID_MASK`, skipping ids in use and 0) and starts
//! the controller on the app's time queue. Each expiry bumps `start_` by
//! the interval and calls the script `onTimer(controllerID, userArg)`
//! (`Controller::standardCallback`, `okIfFnNull` false); a single-shot
//! timer is auto-released by the queue and `onHandlerRelease` cancels the
//! controller. `Entity.cancel` → `Controllers::delController` stops the
//! real face and drops it; the restore face (`Entity::startRealControllers`
//! → `Controllers::startReals`) restarts with `isInitialStart` false,
//! clamping a stale start to the current time.
//!
//! The attach/clear faces (`Entity.addController` / `Entity.cancel`)
//! handle the exclusive families too (`controller.cpp:498`): a category
//! mints an id above `EXCLUSIVE_CONTROLLER_ID_MASK` so it can never
//! collide with the plain allocator, and a repeated registration of the
//! same category deletes the incumbent before inserting — "repeated
//! registration will not leave the old object behind". Entity extras
//! (`entity_extra.hpp`) hang off the entity the same way: each type
//! registers for a slot index (eeid), the slot is lazily constructed on
//! touch and answered as-is while alive, and `clear` deletes it safe-when
//! absent — every deletion lands in the destruction ledger the tests read.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::cellapp_space::{CellApp, Entity, Space};
use crate::cellappmgr::SpaceId;
use crate::timers::{TimerHandle, TimerHandler};

/// `ControllerID` (`controller.hpp:16`): a `uint16`.
pub type ControllerId = u16;

/// `Controller::EXCLUSIVE_CONTROLLER_ID_MASK`.
pub const EXCLUSIVE_CONTROLLER_ID_MASK: u16 = 0x7fff;

/// `Controller::DOMAIN_REAL`.
pub const DOMAIN_REAL: u8 = 2;

/// `Controller::DOMAIN_GHOST` — the ghost domain the reference broadcasts
/// to; no ghost faces here (that is the ghost cluster, not this ticket).
pub const DOMAIN_GHOST: u8 = 1;

// ---------------------------------------------------------------------------
// Section: the callback buffer (entity_callback_buffer.cpp)
// ---------------------------------------------------------------------------

/// A stored script callback: the entity, the method name and, for a
/// controller callback, the `(controllerID, userArg)` tuple the reference
/// builds with `Py_BuildValue`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferedCallback {
    /// The entity the call was made on.
    pub entity: u32,
    /// The method name (`__init__`, `onTimer`, ...).
    pub name: &'static str,
    /// `standardCallback`'s `(controllerID, userArg)` arguments.
    pub timer: Option<(ControllerId, i32)>,
}

/// `EntityCallbackBuffer`: the buffer of script calls deferred while a
/// section is open.
#[derive(Debug, Default)]
pub struct CallbackBuffer {
    /// `callbacksPrevented_`: how many buffered sections are open.
    pub prevented: u32,
    /// `allowCallbacksOverride_`: how many high-priority windows are open.
    pub override_depth: u32,
    /// The ordinary buffer.
    pub buffer: Vec<BufferedCallback>,
    /// The high-priority buffer.
    pub high_priority: Vec<BufferedCallback>,
}

impl CallbackBuffer {
    /// `Entity::callbacksPermitted()` (the getter): no section is open.
    pub fn permitted(&self) -> bool {
        self.prevented == 0
    }

    /// `enterBufferedSection`.
    pub fn enter_section(&mut self) {
        self.prevented += 1;
    }

    /// `leaveBufferedSection`: decrements, and reports whether the last
    /// section closed so the caller flushes.
    pub fn leave_section(&mut self) -> bool {
        debug_assert!(self.prevented > 0, "unbalanced buffered section");
        self.prevented -= 1;
        self.prevented == 0
    }

    /// `enableHighPriorityBuffering`.
    pub fn enable_high_priority(&mut self) {
        self.override_depth += 1;
    }

    /// `disableHighPriorityBuffering`.
    pub fn disable_high_priority(&mut self) {
        self.override_depth -= 1;
    }

    /// `storeCallback`: the call goes to the high-priority buffer while
    /// `allowCallbacksOverride_ >= callbacksPrevented_`.
    pub fn store(&mut self, callback: BufferedCallback) {
        let high_priority = self.override_depth >= self.prevented;
        debug_assert!(!self.permitted(), "storing outside a buffered section");
        if high_priority {
            self.high_priority.push(callback);
        } else {
            self.buffer.push(callback);
        }
    }

    /// `flush`'s first half: both buffers swap out, high priority ahead of
    /// the ordinary one, leaving the members free for whatever the flush
    /// itself queues (the reference's re-entrancy note).
    pub fn take_buffers(&mut self) -> Vec<BufferedCallback> {
        let mut due = std::mem::take(&mut self.high_priority);
        due.append(&mut std::mem::take(&mut self.buffer));
        due
    }
}

/// One recorded script call, in the order the reference would have made
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptCall {
    /// The entity the call was made on.
    pub entity: u32,
    /// The method name.
    pub name: &'static str,
    /// The `(controllerID, userArg)` of a controller callback.
    pub timer: Option<(ControllerId, i32)>,
}

/// What an entity's scripted `__init__` does, in order.
#[derive(Clone, Debug)]
pub enum InitAction {
    /// `self.someMethod()` — a script callback raised during `__init__`.
    Callback(&'static str),
    /// `self.addTimer(start, repeat, userArg)`.
    AddTimer {
        /// Seconds until the first callback.
        start: f32,
        /// Seconds between repeats; zero for a single shot.
        repeat: f32,
        /// The integer user argument.
        user_arg: i32,
    },
}

/// What a scripted callback body does when invoked — the port's fixture
/// for method bodies other than `__init__` (whose fixture is
/// [`InitAction`]). The NavigateStepController drive loop depends on the
/// script's `onMove` re-entering the navigation API, so the fixture has
/// to be able to say that.
#[derive(Clone, Copy, Debug)]
pub enum MethodAction {
    /// `self.navigateStep(destination, velocity, maxMoveDistance, ...)` —
    /// the re-navigation that chains the walk across waypoints.
    NavigateStep {
        /// The point the script asks to navigate toward.
        destination: [f32; 3],
        /// Metres per second.
        velocity: f32,
        /// The per-call movement cap.
        max_move_distance: f32,
    },
    /// `self.cancel("Movement")` — the script cancels the movement
    /// category, taking the running controller down.
    CancelMovement,
}

/// An entity type's script fixture: the methods it defines and what its
/// `__init__` does. The reference reads both off the generated Python type.
#[derive(Clone, Debug, Default)]
pub struct EntityScript {
    /// The methods defined on the type.
    pub methods: BTreeSet<&'static str>,
    /// What the scripted `__init__` runs, in order.
    pub init_actions: Vec<InitAction>,
    /// What the scripted callback bodies run, by method name.
    pub method_actions: BTreeMap<&'static str, Vec<MethodAction>>,
}

impl EntityScript {
    /// A script whose only defined face is `__init__`.
    pub fn with_init(init_actions: Vec<InitAction>) -> Self {
        let mut methods = BTreeSet::new();
        methods.insert("__init__");
        Self {
            methods,
            init_actions,
            method_actions: BTreeMap::new(),
        }
    }

    /// A script with the given methods and callback bodies.
    pub fn with_methods(
        methods: BTreeSet<&'static str>,
        method_actions: BTreeMap<&'static str, Vec<MethodAction>>,
    ) -> Self {
        Self {
            methods,
            init_actions: Vec::new(),
            method_actions,
        }
    }
}

// ---------------------------------------------------------------------------
// Section: controllers (controllers.cpp, timer_controller.cpp)
// ---------------------------------------------------------------------------

/// `BaseAccelerationController::Facing` (`base_acceleration_controller.hpp`):
/// which way the acceleration families face the entity while it moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccelerationFacing {
    /// `FACING_NONE`: the controller leaves the direction alone.
    None,
    /// `FACING_VELOCITY`: face along the new velocity.
    Velocity,
    /// `FACING_ACCELERATION`: face along the acceleration vector.
    Acceleration,
}

/// `Controller`: what the controller is. BW-08-02-01 models the timer kind;
/// BW-08-02-03 adds the four movement-family kinds
/// (`move_controller.cpp` / `turn_controller.cpp` /
/// `base_acceleration_controller.cpp` /
/// `accelerate_to_point_controller.cpp`); BW-08-02-04 adds the
/// navigation pair (`move_controller.cpp`'s `NavigateStepController` and
/// `navigation_controller.cpp`'s `NavigationController`). Not `Copy`:
/// the full-path navigation controller carries a waypoint path and the
/// resolved destination navloc, exactly like the reference's heap state.
#[derive(Clone, Debug, PartialEq)]
pub enum ControllerKind {
    /// `TimerController`: `start_` is the absolute game tick of the next
    /// expiry, `interval_` the repeat in ticks (0 = single shot).
    Timer {
        /// The next expiry tick.
        start: u32,
        /// The repeat interval in ticks; 0 for a single shot.
        interval: u32,
    },
    /// The exclusive-category families whose concrete drive mechanics a
    /// later ticket owns (navigation is BW-08-02-04): `category` is the id
    /// `Controller::getExclusiveID` minted for the type's category —
    /// always above the mask, so the plain allocator can never hand out
    /// the same id. Attaching it hangs inert: no updatable, no queue.
    Exclusive {
        /// The `getExclusiveID` id of the controller type's category.
        category: ControllerId,
    },
    /// `MoveToPointController`: walk toward `destination` at a constant
    /// `metres_per_tick` (`MoveController::metresPerTick_` = velocity /
    /// updateHertz, frozen at construction).
    MoveToPoint {
        /// `destination_`, world space.
        destination: [f32; 3],
        /// `metresPerTick_`.
        metres_per_tick: f32,
        /// `faceMovement_`.
        face_movement: bool,
        /// `moveVertically_`.
        move_vertically: bool,
    },
    /// `MoveToEntityController`: walk toward `target` until within
    /// `range`, orbiting it at a fixed random `offset` around the target.
    /// `resolved`/`offset` are the controller's own runtime state: the
    /// reference resolves `destEntityID_` to a pointer and recalcs the
    /// offset exactly once, on the first update.
    MoveToEntity {
        /// `destEntityID_`.
        target: u32,
        /// `range_`.
        range: f32,
        /// `metresPerTick_`.
        metres_per_tick: f32,
        /// `faceMovement_`.
        face_movement: bool,
        /// `moveVertically_`.
        move_vertically: bool,
        /// `offset_` in the target's x/z plane; recalculated once.
        offset: [f32; 2],
        /// Whether the lazy resolve + `recalcOffset` already ran.
        resolved: bool,
    },
    /// `YawRotatorController`: turn toward `target_yaw` at
    /// `radians_per_tick` (velocity / updateHertz, frozen at
    /// construction; `startReal` negates it when the anti-clockwise way
    /// is shorter).
    YawRotator {
        /// `targetYaw_`, normalised at construction.
        target_yaw: f32,
        /// `radiansPerTick_`, sign-flipped by `startReal` if needed.
        radians_per_tick: f32,
    },
    /// `AccelerateToPointController`: accelerate toward `destination`
    /// under the shared `BaseAccelerationController` motion laws.
    AccelerateToPoint {
        /// `destination_`, world space.
        destination: [f32; 3],
        /// `acceleration_`, units/second².
        acceleration: f32,
        /// `maxSpeed_`.
        max_speed: f32,
        /// `facing_`.
        facing: AccelerationFacing,
        /// `stopAtDestination_`.
        stop_at_destination: bool,
    },
    /// `NavigateStepController` (`move_controller.cpp:286`): the
    /// navigateStep walk — a `MoveController` toward `destination_`
    /// driven by a per-update tick budget, re-aimed by the script's
    /// `onMove`. The third `MoveController` parameter is the reference
    /// constructor's hardcoded `moveVertically = false`.
    NavigateStep {
        /// `destination_`; the invalid sentinel is `(FLT_MAX)×3`.
        destination: [f32; 3],
        /// `metresPerTick_` (velocity / updateHertz).
        metres_per_tick: f32,
        /// `faceMovement_`.
        face_movement: bool,
        /// `moveVertically_`, always false from the script factory.
        move_vertically: bool,
    },
    /// `NavigationController` (`navigation_controller.cpp`): the full
    /// `Entity.navigate` walk along a resolved waypoint path. The runtime
    /// state (`nextPosition_`, `path_`, `currentNode_`, `pDstLoc_`) is
    /// the controller's own; only `closeEnough_` is neither streamed nor
    /// re-derived (the read face reconstructs the constructor default).
    Navigation {
        /// `destination_`, adjusted by `startReal`'s `dstLoc.clip()`.
        destination: [f32; 3],
        /// `metresPerTick_`.
        metres_per_tick: f32,
        /// `faceMovement_`.
        face_movement: bool,
        /// `maxDistance_` (the -1 → maxAoIRadius substitution already
        /// applied at construction).
        max_distance: f32,
        /// `girth_`.
        girth: f32,
        /// `closeEnough_`.
        close_enough: f32,
        /// `nextPosition_`.
        next_position: [f32; 3],
        /// `path_`: the traversal-distributor waypoints.
        path: Vec<[f32; 3]>,
        /// `currentNode_`: the index of the *next* path entry.
        current_node: usize,
        /// `pDstLoc_`: the destination navloc, `None` when the reference's
        /// resolution failed (`valid()` false).
        dst_loc: Option<crate::cellapp_navigate::NavLoc>,
    },
    /// `PassengerController` (`passenger_controller.cpp`): the
    /// ghost-domain membership marker a boarding entity attaches. It
    /// drives nothing — start/stop maintain the vehicle link — and the
    /// real offload stream skips it (`Controller::domain()` without
    /// `DOMAIN_REAL`).
    Passenger {
        /// `vehicleID_`.
        vehicle_id: u32,
    },
}

/// The category string every movement-family controller registers under
/// (`IMPLEMENT_EXCLUSIVE_CONTROLLER_TYPE`'s last argument) — they all
/// share one exclusive id and replace each other.
pub const MOVEMENT_CATEGORY: &str = "Movement";

impl ControllerKind {
    /// A `MoveToPointController` construction (`MoveController`'s ctor:
    /// `metresPerTick_ = velocity / updateHertz`).
    pub fn move_to_point(
        destination: [f32; 3],
        velocity: f32,
        face_movement: bool,
        move_vertically: bool,
        hertz: u32,
    ) -> Self {
        Self::MoveToPoint {
            destination,
            metres_per_tick: velocity / hertz as f32,
            face_movement,
            move_vertically,
        }
    }

    /// A `MoveToEntityController` construction.
    pub fn move_to_entity(
        target: u32,
        range: f32,
        velocity: f32,
        face_movement: bool,
        move_vertically: bool,
        hertz: u32,
    ) -> Self {
        Self::MoveToEntity {
            target,
            range,
            metres_per_tick: velocity / hertz as f32,
            face_movement,
            move_vertically,
            offset: [0.0, 0.0],
            resolved: false,
        }
    }

    /// A `YawRotatorController` construction: `radiansPerTick_ = velocity
    /// / updateHertz`, `targetYaw_ = Angle(targetYaw)` (normalised).
    pub fn yaw_rotator(target_yaw: f32, velocity: f32, hertz: u32) -> Self {
        Self::YawRotator {
            target_yaw: foundation::math::angle::normalise(target_yaw),
            radians_per_tick: velocity / hertz as f32,
        }
    }

    /// An `AccelerateToPointController` construction (its parameters are
    /// stored raw; the per-tick division happens on every move).
    pub fn accelerate_to_point(
        destination: [f32; 3],
        acceleration: f32,
        max_speed: f32,
        facing: AccelerationFacing,
        stop_at_destination: bool,
    ) -> Self {
        Self::AccelerateToPoint {
            destination,
            acceleration,
            max_speed,
            facing,
            stop_at_destination,
        }
    }

    /// A `NavigateStepController` construction
    /// (`MoveController(velocity, faceMovement, false)`).
    pub fn navigate_step(
        destination: [f32; 3],
        velocity: f32,
        face_movement: bool,
        hertz: u32,
    ) -> Self {
        Self::NavigateStep {
            destination,
            metres_per_tick: velocity / hertz as f32,
            face_movement,
            move_vertically: false,
        }
    }

    /// A `NavigationController` construction: `metresPerTick_` is
    /// velocity / updateHertz and the `-1` `maxDistance` becomes
    /// `maxAoIRadius` here (the ctor's `isEqual(maxDistance_, -1.f)`
    /// substitution).
    #[allow(clippy::too_many_arguments)]
    pub fn navigation(
        destination: [f32; 3],
        velocity: f32,
        face_movement: bool,
        max_distance: f32,
        girth: f32,
        close_enough: f32,
        hertz: u32,
        max_aoi_radius: f32,
    ) -> Self {
        let max_distance = if max_distance == -1.0 {
            max_aoi_radius
        } else {
            max_distance
        };
        Self::Navigation {
            destination,
            metres_per_tick: velocity / hertz as f32,
            face_movement,
            max_distance,
            girth,
            close_enough,
            next_position: [0.0; 3],
            path: Vec::new(),
            current_node: 0,
            dst_loc: None,
        }
    }

    /// The controller type's exclusive category
    /// (`IMPLEMENT_EXCLUSIVE_CONTROLLER_TYPE`'s argument): timers have
    /// none; a pre-resolved `Exclusive` carries only its id; the movement
    /// families — navigation included — all register as `"Movement"`.
    pub fn exclusive_category(&self) -> Option<&'static str> {
        match self {
            ControllerKind::MoveToPoint { .. }
            | ControllerKind::MoveToEntity { .. }
            | ControllerKind::YawRotator { .. }
            | ControllerKind::AccelerateToPoint { .. }
            | ControllerKind::NavigateStep { .. }
            | ControllerKind::Navigation { .. } => Some(MOVEMENT_CATEGORY),
            _ => None,
        }
    }
}

/// `Controller`: the kind and the `userArg_` the script passed.
#[derive(Clone, Debug, PartialEq)]
pub struct Controller {
    /// What the controller is.
    pub kind: ControllerKind,
    /// `userArg_`.
    pub user_arg: i32,
}

impl Controller {
    /// `Controller::exclusiveID()` for the kinds that carry a resolved
    /// id: the base (timers) and the movement families answer 0 — the
    /// plain `nextControllerID` path unless the category resolves on the
    /// app; a pre-resolved `Exclusive` returns its category's id above
    /// the mask.
    pub fn presolved_exclusive_id(&self) -> ControllerId {
        match &self.kind {
            ControllerKind::Exclusive { category } => *category,
            _ => 0,
        }
    }

    /// `Controller::domain()`: the `PassengerController` is
    /// `DOMAIN_GHOST` (its `CONTROLLER_DECLARE_GHOST_ONLY`), every other
    /// ported kind is real.
    pub fn domain(&self) -> u8 {
        match self.kind {
            ControllerKind::Passenger { .. } => DOMAIN_GHOST,
            _ => DOMAIN_REAL,
        }
    }
}

/// `Controllers`: one entity's attached controllers, the id allocator and
/// the armed `TimeQueue` handles (the reference keeps each handle on its
/// `TimerController`).
#[derive(Clone, Debug, Default)]
pub struct Controllers {
    /// The container: controller id → controller.
    pub container: BTreeMap<ControllerId, Controller>,
    /// `lastAllocatedID_`.
    pub last_allocated_id: ControllerId,
    /// The armed `TimeQueue` handles, by controller id.
    pub handles: BTreeMap<ControllerId, Option<TimerHandle<u32>>>,
    /// The `Too many controllers on this entity` ERROR refusals.
    pub over_max_adds: usize,
    /// The `#controllers = %d` WARNINGs above `expectedMaxControllers`.
    pub over_expected_adds: usize,
    /// The `Could not find controller id = %d.` WARNINGs (only the
    /// warn-on-failure face counts).
    pub failed_dels: usize,
    /// The successful `delController` calls — the exclusive replacement's
    /// incumbent deletion is one of these.
    pub dels: usize,
}

impl Controllers {
    /// `Controllers::nextControllerID` (`controllers.cpp:592`): step under
    /// the exclusive mask, skipping ids in use and 0.
    pub fn next_controller_id(&mut self) -> ControllerId {
        loop {
            self.last_allocated_id =
                (self.last_allocated_id.wrapping_add(1)) & EXCLUSIVE_CONTROLLER_ID_MASK;
            if !self.container.contains_key(&self.last_allocated_id) && self.last_allocated_id != 0
            {
                return self.last_allocated_id;
            }
        }
    }

    /// `Controllers::addController` (`controllers.cpp:277`): refuse over
    /// `absoluteMax` with 0, warn over `expectedMax`, allocate under the
    /// caller-resolved exclusive id and insert, reporting the id so the
    /// caller performs `startReal( true )`.
    pub fn add_controller(
        &mut self,
        controller: Controller,
        exclusive: ControllerId,
        expected_max: usize,
        absolute_max: usize,
    ) -> ControllerId {
        let count = self.container.len();
        if count >= absolute_max {
            // ERROR "Too many controllers on this entity"
            self.over_max_adds += 1;
            return 0;
        } else if count > expected_max {
            // WARNING "#controllers = %d"
            self.over_expected_adds += 1;
        }

        // The reference first consults pController->exclusiveID(): a
        // nonzero id (the exclusive families above the mask) deletes the
        // incumbent of that id first — "if another movement controller
        // already exists, stop it now before adding the new one" — and
        // inserts under that same id, so a repeated registration never
        // leaves the old object behind. Timers (and any family without a
        // resolved id) take the plain next id.
        let id = match exclusive {
            0 => self.next_controller_id(),
            resolved => {
                self.del_controller(resolved, false);
                resolved
            }
        };
        self.container.insert(id, controller);
        id
    }

    /// `Controllers::delController`: whether the id was attached. A miss
    /// with `warn_on_failure` counts the WARNING; every hit counts a del.
    pub fn del_controller(&mut self, id: ControllerId, warn_on_failure: bool) -> bool {
        match self.container.remove(&id) {
            Some(_) => {
                self.dels += 1;
                true
            }
            None => {
                if warn_on_failure {
                    // WARNING "Could not find controller id = %d."
                    self.failed_dels += 1;
                }
                false
            }
        }
    }

    /// `TimerController::handleTimeout`'s bookkeeping: `start_ += interval_`.
    /// The controller is gone when the timer fires after its cancel
    /// released it mid-drain, in which case nothing steps.
    pub fn advance_timer_start(&mut self, id: ControllerId) {
        let Some(controller) = self.container.get_mut(&id) else {
            return;
        };
        let ControllerKind::Timer { start, interval } = controller.kind else {
            // Only timer controllers step their start on expiry; the
            // exclusive families drive themselves (BW-08-02-03).
            return;
        };
        controller.kind = ControllerKind::Timer {
            start: start.wrapping_add(interval),
            interval,
        };
    }

    /// The queue released the timer: drop the armed handle without
    /// cancelling (the release already happened).
    pub fn release_handle(&mut self, id: ControllerId) {
        *self.handles.entry(id).or_default() = None;
    }
}

// ---------------------------------------------------------------------------
// Section: the timer's time-queue router
// ---------------------------------------------------------------------------

/// An event the time queue produced for one timer controller. The router
/// records these while [`crate::timers::TimeQueue::process`] runs; the app
/// replays them in order right after, the way the reference's callbacks
/// re-enter it from the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouterEvent {
    /// `TimerController::handleTimeout`.
    Timeout {
        /// The entity the controller is on.
        entity: u32,
        /// The controller id.
        id: ControllerId,
        /// The controller's user argument.
        user_arg: i32,
    },
    /// `TimerController::Handler::onRelease` → `onHandlerRelease` → the
    /// controller cancels itself.
    Release {
        /// The entity the controller was on.
        entity: u32,
        /// The controller id.
        id: ControllerId,
    },
}

/// The per-controller time-queue handler (`TimerController::Handler`).
struct TimerRouter {
    events: Rc<RefCell<Vec<RouterEvent>>>,
    entity: u32,
    id: ControllerId,
    user_arg: i32,
}

impl TimerHandler<u32> for TimerRouter {
    fn handle_timeout(&self, _handle: TimerHandle<u32>, _user: usize) {
        self.events.borrow_mut().push(RouterEvent::Timeout {
            entity: self.entity,
            id: self.id,
            user_arg: self.user_arg,
        });
    }

    fn on_release(&self, _handle: TimerHandle<u32>, _user: usize) {
        self.events.borrow_mut().push(RouterEvent::Release {
            entity: self.entity,
            id: self.id,
        });
    }
}

// ---------------------------------------------------------------------------
// Section: creation (Cell::createEntityInternal, Entity::initReal)
// ---------------------------------------------------------------------------

/// What [`CellApp::create_real_entity`] concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateRealOutcome {
    /// The entity was created with this id.
    Created(u32),
    /// A live entity with this id already existed and is returned as is.
    Existed(u32),
    /// The id pool was empty (`No free IDs!`).
    NoFreeIds,
    /// `Space::newEntity`: the type "has no cell entity class".
    UnknownType,
    /// `Entity::initReal` failed on the stream; the half-built entity is
    /// destroyed and left in the population for revival.
    BadStream,
    /// The caller named a space id this map does not hold (the reference's
    /// `Cell` is always bound to its space).
    NoSpace,
}

/// The creation stream's head: id + type id (`createEntityInternal`).
const CREATE_HEAD_LEN: usize = 4 + 4;
/// `initReal`'s fixed body: position, the flags byte, direction, the last
/// event number and the base address (ip + port).
const INIT_REAL_LEN: usize = 12 + 1 + 12 + 4 + 6;
/// The full minimum: head + body + the Witness byte `RealEntity::init`
/// reads.
const CREATE_STREAM_LEN: usize = CREATE_HEAD_LEN + INIT_REAL_LEN + 1;
/// `StreamHelper::AddEntityData::isOnGroundFlag`
/// (`lib/server/stream_helper.hpp`).
pub const ADD_ENTITY_DATA_IS_ON_GROUND: u8 = 0x1;
/// `StreamHelper::AddEntityData::hasTemplateFlag` — the template body is a
/// separate stream section no fixture carries.
pub const ADD_ENTITY_DATA_HAS_TEMPLATE: u8 = 0x2;

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes"))
}

fn read_f32(bytes: &[u8]) -> f32 {
    f32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes"))
}

impl CellApp {
    /// `Entity::callbacksPermitted( permitted )` (`entity.cpp:8074`): open
    /// or close the buffered section; closing the last one flushes.
    pub fn callbacks_permitted(&mut self, permitted: bool) {
        let due_flush = permitted && self.callback_buffer.leave_section();
        if due_flush {
            self.flush_callbacks();
        }
        if !permitted {
            self.callback_buffer.enter_section();
        }
    }

    /// `EntityCallbackBuffer::flush`, driven by the app so executing calls
    /// can reach it: both buffers swap out, then each stored call runs in
    /// order, high priority first. A call arriving mid-flush with no
    /// section open runs immediately — the reference's re-entrancy note.
    pub fn flush_callbacks(&mut self) {
        let due = self.callback_buffer.take_buffers();
        for callback in due {
            let BufferedCallback {
                entity,
                name,
                timer,
            } = callback;
            let known_destroyed = self
                .entity_population
                .get(&entity)
                .is_some_and(|e| e.borrow().destroyed);
            if known_destroyed {
                // WARNING "%s entity %u has been destroyed"
                self.destroyed_callbacks.push((entity, name));
                continue;
            }
            self.run_callback(entity, name, timer);
        }
    }

    /// `Entity::callback( funcName, args, errorPrefix, okIfFnNull )`
    /// (`entity.cpp:7922`).
    pub fn entity_callback(
        &mut self,
        entity: u32,
        name: &'static str,
        ok_if_fn_null: bool,
        timer: Option<(ControllerId, i32)>,
    ) -> bool {
        let known = self.entity_population.get(&entity).cloned();
        let Some(entity_rc) = known else {
            // The reference only reaches here through the population; an
            // unknown id is a caller contract breach.
            return false;
        };
        if entity_rc.borrow().destroyed {
            // ERROR "%sto %s entity %u is destroyed"
            self.destroyed_callbacks.push((entity, name));
            return false;
        }

        let type_id = entity_rc.borrow().type_id;
        let has_method = self
            .entity_scripts
            .get(&type_id)
            .is_some_and(|script| script.methods.contains(name));
        if !has_method {
            if !ok_if_fn_null {
                // WARNING "[%s %u] has no method %s"
                self.missing_callbacks.push((entity, name));
            }
            return ok_if_fn_null;
        }

        if self.callback_buffer.permitted() {
            self.run_callback(entity, name, timer);
            return true;
        }
        self.callback_buffer.store(BufferedCallback {
            entity,
            name,
            timer,
        });
        true
    }

    /// The call actually happening: record it, and run the script
    /// fixture's actions — `__init__`'s body, then the invoked method's
    /// body.
    fn run_callback(
        &mut self,
        entity: u32,
        name: &'static str,
        timer: Option<(ControllerId, i32)>,
    ) {
        self.script_calls.push(ScriptCall {
            entity,
            name,
            timer,
        });
        if name == "__init__" {
            let actions = self.init_actions_of(entity);
            for action in actions {
                match action {
                    InitAction::Callback(callback_name) => {
                        self.entity_callback(entity, callback_name, true, None);
                    }
                    InitAction::AddTimer {
                        start,
                        repeat,
                        user_arg,
                    } => {
                        self.add_timer(entity, start, repeat, user_arg);
                    }
                }
            }
        }
        let actions = self.method_actions_of(entity, name);
        for action in actions {
            match action {
                MethodAction::NavigateStep {
                    destination,
                    velocity,
                    max_move_distance,
                } => {
                    self.navigate_step(
                        entity,
                        destination,
                        velocity,
                        max_move_distance,
                        -1.0,
                        true,
                        0.5,
                        0,
                    );
                }
                MethodAction::CancelMovement => {
                    let movement = self.get_exclusive_id(MOVEMENT_CATEGORY);
                    self.cancel_controller(entity, movement);
                }
            }
        }
    }

    /// The callback body of `entity`'s scripted `name` method.
    fn method_actions_of(&self, entity: u32, name: &'static str) -> Vec<MethodAction> {
        self.entity_population
            .get(&entity)
            .and_then(|e| self.entity_scripts.get(&e.borrow().type_id))
            .and_then(|script| script.method_actions.get(name))
            .cloned()
            .unwrap_or_default()
    }

    /// The `__init__` actions of `entity`'s type.
    fn init_actions_of(&self, entity: u32) -> Vec<InitAction> {
        self.entity_population
            .get(&entity)
            .and_then(|e| self.entity_scripts.get(&e.borrow().type_id))
            .map(|script| script.init_actions.clone())
            .unwrap_or_default()
    }

    /// `Cell::createEntityInternal` (`cell.cpp:302`) + `Entity::initReal`
    /// (`entity.cpp:1425`): build a real entity from a creation stream.
    ///
    /// The stream: id + type id, then `initReal`'s position, the
    /// `AddEntityData` flags byte, the direction, the last event number
    /// and the base address, then the Witness byte (`RealEntity::init`).
    /// All host-endian, like every other server stream in this port.
    pub fn create_real_entity(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        space_id: SpaceId,
        stream: &[u8],
    ) -> CreateRealOutcome {
        self.build_real_entity(spaces, space_id, stream, false)
    }

    /// The onload/offload reply face (`Entity::initReal` with
    /// `isRestore`, entity.cpp:1486): the same creation stream the
    /// `create` face reads, followed by the offloaded controller block
    /// — `readRealDataFromStreamForOnloadInternal`'s
    /// `readRealControllersFromStream` + the `startRealControllers` that
    /// follows "now that pReal_ has been set" — with `callScriptInit`
    /// answering `onRestore` instead of `__init__`.
    pub fn restore_real_entity(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        space_id: SpaceId,
        stream: &[u8],
    ) -> CreateRealOutcome {
        self.build_real_entity(spaces, space_id, stream, true)
    }

    /// `build_real_entity`: the shared `initReal` body; the `is_restore`
    /// flag swaps the init callback for `onRestore` and replays the
    /// trailing controller block.
    fn build_real_entity(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        space_id: SpaceId,
        stream: &[u8],
        is_restore: bool,
    ) -> CreateRealOutcome {
        if stream.len() < CREATE_HEAD_LEN {
            return CreateRealOutcome::BadStream;
        }
        let mut id = read_u32(&stream[0..4]);
        let type_id = read_u32(&stream[4..8]);

        // If the ID is unknown, allocate one (`shouldAllocateID`).
        if id == 0 {
            match self.id_pool.get_id() {
                Some(allocated) => id = allocated,
                None => return CreateRealOutcome::NoFreeIds, // ERROR "No free IDs!"
            }
        }

        // If the (non-destroyed) entity already exists, bail early. The
        // zombie-ghost restore branch is the restore cluster's face.
        let existing_live = self
            .entity_population
            .get(&id)
            .is_some_and(|existing| !existing.borrow().destroyed);
        if existing_live {
            self.duplicate_creates.push(id); // WARNING "%s %u already exists!"
            return CreateRealOutcome::Existed(id);
        }

        let Some(space) = spaces.get_mut(&space_id) else {
            return CreateRealOutcome::NoSpace;
        };

        // `Space::newEntity`: revive the destroyed population entry or
        // build a fresh one (the type must have a cell entity class).
        let entity_rc = match self.entity_population.get(&id).cloned() {
            Some(revived) => {
                // MF_ASSERT( isDestroyed ) + MF_ASSERT( same type ).
                let snapshot = revived.borrow();
                debug_assert!(snapshot.destroyed);
                debug_assert_eq!(snapshot.type_id, type_id);
                drop(snapshot);
                // `setToInitialState( id, this )`: the wholesale reset —
                // a new Controllers object and every extra slot cleared
                // (the old extras delete as the reset replaces them), so
                // the app-level navigate state dies with them.
                let mut reborn = Entity::new(id, true);
                reborn.type_id = type_id;
                *revived.borrow_mut() = reborn;
                self.navigate_step_controllers.remove(&id);
                self.navigators.remove(&id);
                revived
            }
            None => {
                if !self.entity_scripts.contains_key(&type_id) {
                    // ERROR "Entity type %d has no cell entity class"
                    return CreateRealOutcome::UnknownType;
                }
                let mut fresh = Entity::new(id, true);
                fresh.type_id = type_id;
                let fresh = Rc::new(RefCell::new(fresh));
                // `setID` adds the entity to the population.
                self.entity_population.insert(id, Rc::clone(&fresh));
                fresh
            }
        };

        // `Entity::callbacksPermitted( false )` around the whole build.
        self.callbacks_permitted(false);

        // `Entity::initReal`: the fixed creation head.
        if stream.len() < CREATE_STREAM_LEN {
            // initReal's failure path: setDestroyed, forget the rest of
            // the creation data, and leave the population entry destroyed
            // for revival (the reference's decRef only drops the local
            // ref; setShouldReturnID hands the allocated id back to the
            // IDClient).
            entity_rc.borrow_mut().destroyed = true;
            self.callbacks_permitted(true);
            return CreateRealOutcome::BadStream;
        }
        let mut offset = CREATE_HEAD_LEN;
        {
            let mut entity = entity_rc.borrow_mut();
            let position = &stream[offset..offset + 12];
            entity.position = [
                read_f32(&position[0..4]),
                read_f32(&position[4..8]),
                read_f32(&position[8..12]),
            ];
            offset += 12;
            let flags = stream[offset];
            entity.is_on_ground = flags & ADD_ENTITY_DATA_IS_ON_GROUND != 0;
            offset += 1;
            let direction = &stream[offset..offset + 12];
            entity.direction = [
                read_f32(&direction[0..4]),
                read_f32(&direction[4..8]),
                read_f32(&direction[8..12]),
            ];
            // `initReal` reads `localPosition_`/`localDirection_` first;
            // the global frame takes them over until a vehicle mounts
            // (`updateLocalPosition`'s unmounted copy).
            entity.local_position = entity.position;
            entity.local_direction = entity.direction;
            offset += 12;
            entity.last_event_number = read_u32(&stream[offset..offset + 4]);
            offset += 4;
            let ip = read_u32(&stream[offset..offset + 4]);
            offset += 4;
            let port = u16::from_le_bytes(stream[offset..offset + 2].try_into().expect("2 bytes"));
            offset += 2;
            entity.base_addr = Some((ip, port));
            // `RealEntity`'s ctor seeds the velocity sampler from the
            // owner's position (`positionSample_( owner.position() )`), so
            // the first sampled window is the streamed position, not the
            // origin.
            entity.position_sample = entity.position;
        }

        // `pSpace_->addEntity( this )`.
        space.add_entity(Rc::clone(&entity_rc));

        // `RealEntity::init` (CREATE_REAL_FROM_INIT): the Witness byte.
        // readBasePropertiesExposedForReplay is the recording face and
        // updateGlobalPosition/checkChunkCrossing are the chunk domain's.
        let witness = stream[offset];
        entity_rc.borrow_mut().has_witness = witness == b'W';
        // Anything other than 'W'/'-' is the ERROR "Illegal Witness
        // indicator"; the build carries on without a witness either way.

        if is_restore {
            // The offload tail the creation head precedes: the
            // controller block, read back without starting anything —
            // `startRealControllers` waits until the entity is real.
            let controllers = stream.get(offset + 1..).unwrap_or(&[]);
            self.read_real_controllers_from_stream(id, controllers);
        }

        // `callScriptInit( isRestore )` inside high-priority buffering
        // ("we do not yet want to replay any queued callback"): `__init__`
        // on the create face, `onRestore` on the restore face.
        self.callback_buffer.enable_high_priority();
        let init_name = if is_restore { "onRestore" } else { "__init__" };
        self.entity_callback(id, init_name, true, None);
        self.callback_buffer.disable_high_priority();

        // `Cell::addRealEntity( shouldSendNow=false )`: the currentCell
        // send waits so it rides the same bundle as the backup.
        self.current_cell_sends.push((id, space_id));

        if is_restore {
            // `if (isRestore) this->startRealControllers()` — the
            // restored controllers arm their drives against the entity
            // that just became real.
            start_real_controllers(self, &entity_rc);
            // `Entity::relocated()` — the extras notification tail; a
            // real passenger still limbo after its restore alights.
            self.relocate_entity_extras(&entity_rc);
        }

        // The caller's `Entity::callbacksPermitted( true )`: the last
        // section closes and the buffered calls run — `__init__` (or
        // `onRestore`) first.
        self.callbacks_permitted(true);

        // `Entity::population().onEntityAdd( ... )` — the population
        // notification. The observers are the passenger controllers that
        // went limbo waiting for this vehicle.
        self.entity_entered_population(id);

        CreateRealOutcome::Created(id)
    }

    /// `Entity::addTimer` → `TimerController::New` →
    /// `Controllers::addController` → `TimerController::startReal( true )`.
    /// Returns the controller id, or 0 when the add was refused.
    pub fn add_timer(
        &mut self,
        entity: u32,
        start: f32,
        repeat: f32,
        user_arg: i32,
    ) -> ControllerId {
        let hertz = self.update_hertz as f32;
        // (GameTime)( initialOffset * updateHertz() + 0.5f ); a
        // non-positive result rounds up to 1.
        let mut initial_ticks = (start * hertz + 0.5) as u32;
        if initial_ticks == 0 {
            initial_ticks = 1;
        }
        initial_ticks = initial_ticks.wrapping_add(self.time);
        let interval = if repeat > 0.0 {
            let ticks = (repeat * hertz + 0.5) as u32;
            if ticks < 1 {
                1 // WARNING "Rounding up repeatTicks to 1"
            } else {
                ticks
            }
        } else {
            0
        };

        let controller = Controller {
            kind: ControllerKind::Timer {
                start: initial_ticks,
                interval,
            },
            user_arg,
        };
        self.attach_controller(entity, controller)
    }

    /// The generic `Entity.addController` attach face: the script factory
    /// builds the controller, `Controllers::addController` attaches it —
    /// an exclusive category replaces its incumbent — and
    /// `startReal( true )` runs. `addTimer` is this face specialized to
    /// timers; the movement factories are the exclusive callers.
    pub fn attach_controller(&mut self, entity: u32, controller: Controller) -> ControllerId {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return 0;
        };
        // Resolve the controller type's exclusive id on the app: the
        // movement families mint (or reuse) their category's id here —
        // `IMPLEMENT_EXCLUSIVE_CONTROLLER_TYPE` hardcodes the category
        // string per type, so the same string always answers the same id.
        let mut exclusive = controller.presolved_exclusive_id();
        if let (0, Some(category)) = (exclusive, controller.kind.exclusive_category()) {
            exclusive = self.get_exclusive_id(category);
        }
        // The incumbent deletion inside addController must also stop the
        // old controller's updatable drive (its stopReal face).
        if exclusive != 0 {
            self.deregister_updatable(entity, exclusive);
        }
        let id = entity_rc.borrow_mut().controllers.add_controller(
            controller,
            exclusive,
            self.expected_max_controllers,
            self.absolute_max_controllers,
        );
        if id == 0 {
            return 0;
        }
        self.start_real_controller(&entity_rc, id, true);
        id
    }

    /// `Controller::startReal` dispatch (`controllers.cpp:477`): each
    /// family arms its own drive — timers join the `TimeQueue`, the
    /// movement families join the `Updatables` registry, and the
    /// pre-resolved `Exclusive` placeholder hangs inert.
    pub fn start_real_controller(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        is_initial_start: bool,
    ) {
        let kind = entity_rc
            .borrow()
            .controllers
            .container
            .get(&id)
            .map(|c| c.kind.clone());
        let Some(kind) = kind else {
            return;
        };
        match kind {
            ControllerKind::Timer { .. } => {
                self.start_timer_controller(entity_rc, id, is_initial_start)
            }
            ControllerKind::Exclusive { .. } => {}
            ControllerKind::NavigateStep { .. } => {
                self.start_navigate_step_controller(entity_rc, id, kind)
            }
            ControllerKind::Navigation {
                close_enough,
                destination,
                metres_per_tick,
                face_movement,
                max_distance,
                girth,
                ..
            } => self.start_full_navigation_controller(
                entity_rc,
                id,
                close_enough,
                destination,
                metres_per_tick,
                face_movement,
                max_distance,
                girth,
            ),
            // The ghost-domain face: `addController`'s dispatch starts
            // the ghost half for `CONTROLLER_DECLARE_GHOST_ONLY` kinds.
            ControllerKind::Passenger { vehicle_id } => {
                self.passenger_start(entity_rc, id, vehicle_id)
            }
            _ => self.start_movement_controller(entity_rc, id, kind),
        }
    }

    /// `TimerController::startReal( isInitialStart )`: arm the queue
    /// entry; a restart (`startReals`, the restore/onboard face) clamps a
    /// stale start to the current time.
    pub fn start_timer_controller(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        is_initial_start: bool,
    ) {
        let (start, interval, user_arg, entity_id) = {
            let mut entity = entity_rc.borrow_mut();
            let Some(controller) = entity.controllers.container.get_mut(&id) else {
                return;
            };
            let ControllerKind::Timer { start, interval } = controller.kind else {
                // The exclusive families arm their own drive timer inside
                // their startReal (BW-08-02-03); there is nothing on the
                // time queue for them yet.
                return;
            };
            let start = if is_initial_start || start >= self.time {
                start
            } else {
                // "if we were offloaded just as our timer was going off -
                // we set the timer start time to be now"
                self.time
            };
            controller.kind = ControllerKind::Timer { start, interval };
            (start, interval, controller.user_arg, entity.id)
        };
        let router = Rc::new(TimerRouter {
            events: Rc::clone(&self.router_events),
            entity: entity_id,
            id,
            user_arg,
        });
        let handle = self.time_queue.add(
            start,
            interval,
            router,
            user_arg as usize,
            "TimerController",
        );
        entity_rc
            .borrow_mut()
            .controllers
            .handles
            .insert(id, Some(handle));
    }

    /// `Entity.cancel` → `Controllers::delController`: stop the real face
    /// (`stopReal` → the queue's release) and drop the controller.
    pub fn cancel_controller(&mut self, entity: u32, id: ControllerId) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        let kind = entity_rc
            .borrow()
            .controllers
            .container
            .get(&id)
            .map(|c| c.kind.clone());
        let mut armed = entity_rc
            .borrow_mut()
            .controllers
            .handles
            .get_mut(&id)
            .and_then(|slot| slot.take());
        let removed = entity_rc.borrow_mut().controllers.del_controller(id, true);
        if !removed {
            // delController's warn-on-failure WARNING counted.
            return false;
        }
        // The ghost-domain half of `stop`: the passenger marker runs its
        // stop face while the controller is still in reach (the
        // reference's `PassengerController::stopGhost`).
        if let Some(ControllerKind::Passenger { vehicle_id }) = kind {
            self.passenger_stop(&entity_rc, id, vehicle_id);
        }
        // The updatable half of `stopReal`: a movement controller leaves
        // the per-tick registry with its container entry.
        self.deregister_updatable(entity, id);
        // NavigateStepController::stopReal: deregister the entity's
        // navigate-step pointer when it names this controller.
        crate::cellapp_navigate::deregister_navigate_step(self, entity, id);
        // TimerController::stopReal( true ): cancelling fires the queue's
        // release, whose on_release records a Release event the next tick
        // drains (the controller is already gone by then, exactly like the
        // reference's detached handler).
        for handle in armed.iter_mut() {
            handle.cancel();
        }
        true
    }

    /// `EntityApp::advanceTime` + `callTimers`
    /// (`lib/server/entity_app.cpp:157`): bump the game time, run the
    /// updatables pass (`ServerApp::advanceTime`'s `callUpdatables` — the
    /// movement families drive here), then the queue, then replay the
    /// router events in order.
    pub fn advance_tick(&mut self) {
        self.time += 1;
        self.run_movement_pass();
        self.time_queue.process(self.time);
        let events = std::mem::take(&mut *self.router_events.borrow_mut());
        for event in events {
            match event {
                RouterEvent::Timeout {
                    entity,
                    id,
                    user_arg,
                } => self.handle_timeout_event(entity, id, user_arg),
                RouterEvent::Release { entity, id } => self.handle_release_event(entity, id),
            }
        }
    }

    /// `TimerController::handleTimeout`: `start_ += interval_`, then the
    /// script callback.
    fn handle_timeout_event(&mut self, entity: u32, id: ControllerId, user_arg: i32) {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return; // the entity is gone from the population
        };
        entity_rc.borrow_mut().controllers.advance_timer_start(id);
        self.entity_callback(entity, "onTimer", false, Some((id, user_arg)));
    }

    /// `onHandlerRelease` → `cancel` →
    /// `Controllers::delController( warnOnFailure=false )`.
    fn handle_release_event(&mut self, entity: u32, id: ControllerId) {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return; // the entity is gone from the population
        };
        let mut borrowed = entity_rc.borrow_mut();
        borrowed.controllers.release_handle(id);
        borrowed.controllers.del_controller(id, false);
    }

    /// `~Entity`'s controller half as a minimal destroy: stop every real
    /// controller (`stopRealControllers`) and mark the entity destroyed.
    /// It stays in the population — that is what `Space::newEntity`'s
    /// revival picks up. The full destroy chain (ghosts, AoI, space
    /// removal) is BW-08-02-06.
    pub fn destroy_entity(&mut self, entity: u32) {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return;
        };
        let ids: Vec<ControllerId> = entity_rc
            .borrow()
            .controllers
            .container
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.cancel_controller(entity, id);
        }
        // `~Passengers` (`passengers.cpp:36`): a destroyed vehicle
        // alights every rider before the extras wipe takes the slot.
        self.vehicle_destroyed(entity);
        let mut borrowed = entity_rc.borrow_mut();
        // Entity::destroy: "Get rid of any entity extras (for the first
        // time)" — every live extra deletes and its slot clears.
        // (~Entity deletes them a second time with a debug assert that
        // none came back; one wipe is the observable face here.) The
        // EntityNavigate extra dies with the entity, so its nav system
        // and the navigate-step pointer go too.
        borrowed.extras = Vec::new();
        borrowed.destroyed = true;
        drop(borrowed);
        self.navigate_step_controllers.remove(&entity);
        self.navigators.remove(&entity);
    }
}

// ---------------------------------------------------------------------------
// Section: entity extras (entity_extra.hpp, entity.cpp:7531)
// ---------------------------------------------------------------------------

/// One live `EntityExtra` instance (`entity_extra.hpp`): the base holds
/// its entity backlink and the slot knows its own coordinates, so the
/// deletion is recorded when the instance drops — the observable face of
/// the reference's merciless `delete pExtra`.
#[derive(Clone, Debug)]
pub struct ExtraSlot {
    /// The slot index this instance occupies (its eeid).
    pub eeid: usize,
    /// The entity id the instance is attached to (`entity_`).
    pub entity: u32,
    /// Which instance of this slot this is: every touch after a clear
    /// constructs a new one, and a repeated registration leaves no old
    /// object behind.
    pub generation: u64,
    /// The shared destruction ledger the Drop feeds
    /// `(entity, eeid, generation)` into.
    drops: Rc<RefCell<Vec<(u32, usize, u64)>>>,
}

impl Drop for ExtraSlot {
    fn drop(&mut self) {
        self.drops
            .borrow_mut()
            .push((self.entity, self.eeid, self.generation));
    }
}

impl CellApp {
    /// `Controller::getExclusiveID` (`controller.cpp:498`): the exclusive
    /// category registry. A category new to the registry mints
    /// `EXCLUSIVE_CONTROLLER_ID_MASK + size + 1` — always above the mask,
    /// so `nextControllerID` (which stays masked) can never collide —
    /// and a known category answers its existing id.
    pub fn get_exclusive_id(&mut self, category: &'static str) -> ControllerId {
        if let Some(id) = self.exclusive_ids.get(category) {
            return *id;
        }
        let id = EXCLUSIVE_CONTROLLER_ID_MASK + self.exclusive_ids.len() as u16 + 1;
        self.exclusive_ids.insert(category, id);
        id
    }

    /// `Entity::registerEntityExtra` (`entity.cpp:7531`): register a type
    /// of entity extra; the returned eeid is the slot index. The
    /// reference demands every type be registered before the first
    /// entity is created.
    pub fn register_entity_extra(&mut self, name: &'static str) -> usize {
        self.extra_types.push(name);
        self.extra_types.len() - 1
    }

    /// `EntityExtra::Instance<Derived>::operator()` (`entity_extra.hpp`):
    /// touch — lazily construct the instance in its slot. An instance
    /// already there is answered as is, so repeated registration never
    /// constructs a second object; a touched-back slot constructs a new
    /// one with a fresh generation.
    pub fn touch_extra(&mut self, entity: u32, eeid: usize) -> Option<u64> {
        if eeid >= self.extra_types.len() {
            return None; // the reference registers every type up front
        }
        let entity_rc = self.entity_population.get(&entity).cloned()?;
        let mut borrowed = entity_rc.borrow_mut();
        if borrowed.extras.len() <= eeid {
            borrowed.extras.resize(eeid + 1, None);
        }
        if let Some(slot) = borrowed.extras[eeid].as_ref() {
            return Some(slot.generation);
        }
        let generation = self.next_extra_generation;
        self.next_extra_generation += 1;
        borrowed.extras[eeid] = Some(ExtraSlot {
            eeid,
            entity,
            generation,
            drops: Rc::clone(&self.extra_drops),
        });
        Some(generation)
    }

    /// `EntityExtra::Instance<Derived>::exists`: a live instance — a slot
    /// that was merely sized (or already cleared) is not one.
    pub fn extra_exists(&self, entity: u32, eeid: usize) -> bool {
        self.entity_population.get(&entity).is_some_and(|existing| {
            existing
                .borrow()
                .extras
                .get(eeid)
                .is_some_and(|slot| slot.is_some())
        })
    }

    /// `EntityExtra::Instance<Derived>::clear`: delete the instance and
    /// clear the slot — safe when absent.
    pub fn clear_extra(&mut self, entity: u32, eeid: usize) {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return;
        };
        let mut borrowed = entity_rc.borrow_mut();
        if eeid < borrowed.extras.len() {
            borrowed.extras[eeid] = None; // the Drop records the deletion
        }
    }
}

/// `Entity::startRealControllers` → `Controllers::startReals`: start every
/// real controller with `isInitialStart` false (the restore/onboard face).
pub fn start_real_controllers(app: &mut CellApp, entity_rc: &Rc<RefCell<Entity>>) {
    let ids: Vec<ControllerId> = entity_rc
        .borrow()
        .controllers
        .container
        .keys()
        .copied()
        .collect();
    for id in ids {
        app.start_real_controller(entity_rc, id, false);
    }
}

// ---------------------------------------------------------------------------
// Section: tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_space::NO_SPACE_REMOVAL_HANDLE;

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;

    /// The 44-byte creation stream: id + type id, position, flags,
    /// direction, last event number, base address, witness.
    fn create_stream(id: u32, type_id: u32, witness: u8) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&type_id.to_le_bytes());
        for value in [1.0f32, 2.0, 3.0] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in [0.25f32, 0.0, 0.0] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(witness);
        stream
    }

    /// A CellApp with the fixture type registered (plus `extra` methods)
    /// and a fresh id pool.
    fn app_with_type(init_actions: Vec<InitAction>, extra: &[&'static str]) -> CellApp {
        let mut app = CellApp::default();
        let mut script = EntityScript::with_init(init_actions);
        for name in extra {
            script.methods.insert(name);
        }
        app.entity_scripts.insert(TYPE, script);
        app.id_pool.refill(&[90, 91, 92, 93]);
        app
    }

    fn spaces_with() -> BTreeMap<SpaceId, Space> {
        let mut spaces = BTreeMap::new();
        spaces.insert(SID, Space::new(SID));
        spaces
    }

    #[test]
    fn creation_records_the_init_order_and_fields() {
        let mut app = app_with_type(
            vec![
                InitAction::Callback("onInit"),
                InitAction::AddTimer {
                    start: 0.1,
                    repeat: 0.3,
                    user_arg: 7,
                },
            ],
            &["onInit"],
        );
        let mut spaces = spaces_with();

        let outcome = app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'W'));
        assert_eq!(outcome, CreateRealOutcome::Created(77));

        // The streamed state landed on the entity.
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.type_id, TYPE);
        assert!(entity.is_real);
        assert_eq!(entity.position, [1.0, 2.0, 3.0]);
        assert!(entity.is_on_ground);
        assert_eq!(entity.direction, [0.25, 0.0, 0.0]);
        assert_eq!(entity.last_event_number, 42);
        assert_eq!(entity.base_addr, Some((7, 6502)));
        assert!(entity.has_witness);
        assert_ne!(entity.removal_handle, NO_SPACE_REMOVAL_HANDLE);
        // `__init__` registered the timer: one tick ahead, three apart.
        assert_eq!(
            entity.controllers.container[&1],
            Controller {
                kind: ControllerKind::Timer {
                    start: 1,
                    interval: 3
                },
                user_arg: 7
            }
        );
        drop(entity);

        // The space holds the entity exactly once.
        assert_eq!(spaces[&SID].num_entities(), 1);

        // The acceptance order: `__init__` first (it was buffered during
        // creation), the callback it raises in its body right after — the
        // flush runs high-priority first and the body calls ran nested.
        assert_eq!(
            app.script_calls,
            vec![
                ScriptCall {
                    entity: 77,
                    name: "__init__",
                    timer: None
                },
                ScriptCall {
                    entity: 77,
                    name: "onInit",
                    timer: None
                },
            ]
        );
        // The currentCell send is deferred to the backup bundle.
        assert_eq!(app.current_cell_sends, vec![(77, SID)]);
        // The section is closed and the buffers empty.
        assert!(app.callback_buffer.permitted());
        assert!(app.callback_buffer.buffer.is_empty());
        assert!(app.callback_buffer.high_priority.is_empty());
    }

    #[test]
    fn duplicate_create_returns_existing() {
        let mut app = app_with_type(vec![], &[]);
        let mut spaces = spaces_with();

        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'W')),
            CreateRealOutcome::Created(77)
        );
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Existed(77)
        );
        assert_eq!(app.duplicate_creates, vec![77]);
        assert_eq!(spaces[&SID].num_entities(), 1);
        // The refused create never opened a section or ran a script.
        assert_eq!(app.script_calls.len(), 1); // only the first __init__
    }

    #[test]
    fn id_zero_allocates_from_the_pool() {
        let mut app = CellApp::default();
        app.entity_scripts
            .insert(TYPE, EntityScript::with_init(vec![]));
        app.id_pool.refill(&[5]);
        let mut spaces = spaces_with();

        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(0, TYPE, b'-')),
            CreateRealOutcome::Created(5)
        );
        assert!(app.entity_population.contains_key(&5));

        // The pool is empty now.
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(0, TYPE, b'-')),
            CreateRealOutcome::NoFreeIds
        );
    }

    #[test]
    fn unknown_type_refused() {
        let mut app = app_with_type(vec![], &[]);
        let mut spaces = spaces_with();

        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, 9, b'-')),
            CreateRealOutcome::UnknownType
        );
        assert!(app.entity_population.is_empty());
        assert_eq!(spaces[&SID].num_entities(), 0);
        assert!(app.script_calls.is_empty());
    }

    #[test]
    fn unknown_space_refused() {
        let mut app = app_with_type(vec![], &[]);
        let mut spaces = spaces_with();

        assert_eq!(
            app.create_real_entity(&mut spaces, 9, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::NoSpace
        );
        assert!(app.entity_population.is_empty());
    }

    #[test]
    fn bad_stream_destroys_the_half_built_entity() {
        let mut app = app_with_type(vec![], &[]);
        let mut spaces = spaces_with();

        // Not even the head.
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &[]),
            CreateRealOutcome::BadStream
        );
        // Head but no initReal body.
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &[77, 0, 0, 0, 7, 0, 0, 0]),
            CreateRealOutcome::BadStream
        );
        // Body without the Witness byte: the half-built entity is
        // destroyed but stays in the population.
        let mut short = create_stream(77, TYPE, b'-');
        short.pop();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &short),
            CreateRealOutcome::BadStream
        );
        assert_eq!(app.entity_population.len(), 1);
        assert!(app.entity_population[&77].borrow().destroyed);
        assert!(app.script_calls.is_empty());

        // The destroyed entry revives on a full stream.
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );
        assert!(!app.entity_population[&77].borrow().destroyed);
        assert_eq!(app.entity_population.len(), 1);
    }

    #[test]
    fn destroy_then_create_revives_the_population_entry() {
        let mut app = app_with_type(vec![], &[]);
        let mut spaces = spaces_with();

        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'W')),
            CreateRealOutcome::Created(77)
        );
        app.add_timer(77, 5.0, 5.0, 0);
        app.destroy_entity(12345); // unknown entity: inert

        // The reference's full destroy also drops the space residency.
        let rc = app.entity_population[&77].clone();
        spaces
            .get_mut(&SID)
            .expect("space")
            .remove_entity(&rc, &mut app);
        app.destroy_entity(77); // stops every real controller
        assert!(rc.borrow().destroyed);

        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );
        // The same population entry, revived in place.
        assert!(Rc::ptr_eq(&rc, &app.entity_population[&77]));
        let revived = app.entity_population[&77].borrow();
        assert!(!revived.destroyed);
        assert!(revived.controllers.container.is_empty());
        assert!(!revived.has_witness); // setToInitialState
        assert_ne!(revived.removal_handle, NO_SPACE_REMOVAL_HANDLE);
        drop(revived);
        assert_eq!(spaces[&SID].num_entities(), 1);
    }

    #[test]
    fn buffered_calls_flush_high_priority_first() {
        let mut app = app_with_type(vec![], &["one", "two", "three", "four"]);
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        app.callbacks_permitted(false);
        assert!(!app.callback_buffer.permitted());
        assert!(app.entity_callback(77, "one", true, None));
        assert!(app.entity_callback(77, "two", true, None));
        app.callback_buffer.enable_high_priority();
        assert!(app.entity_callback(77, "three", true, None));
        assert!(app.entity_callback(77, "four", true, None));
        // Nothing ran yet.
        assert_eq!(app.script_calls.len(), 1); // creation's __init__ only

        app.callbacks_permitted(true);
        let names: Vec<&'static str> = app.script_calls[1..].iter().map(|call| call.name).collect();
        assert_eq!(names, vec!["three", "four", "one", "two"]);
    }

    #[test]
    fn call_runs_immediately_when_permitted() {
        let mut app = app_with_type(vec![], &["poke"]);
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        assert!(app.entity_callback(77, "poke", true, None));
        assert_eq!(app.script_calls.len(), 2);
        assert_eq!(app.script_calls[1].name, "poke");
    }

    #[test]
    fn destroyed_entity_swallows_calls() {
        let mut app = app_with_type(vec![], &["later"]);
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        // A stored call whose target died before the flush is skipped
        // with the WARNING face.
        app.callbacks_permitted(false);
        assert!(app.entity_callback(77, "later", true, None));
        app.destroy_entity(77);
        app.callbacks_permitted(true);
        let names: Vec<&'static str> = app.script_calls.iter().map(|call| call.name).collect();
        assert!(!names.contains(&"later"));

        // A direct call is refused with the ERROR face.
        assert!(!app.entity_callback(77, "later", true, None));
        assert_eq!(app.destroyed_callbacks, vec![(77, "later"), (77, "later")]);
    }

    #[test]
    fn missing_method_faces() {
        let mut app = app_with_type(vec![], &[]);
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        // okIfFnNull=true: silent success.
        assert!(app.entity_callback(77, "onFoo", true, None));
        assert!(app.missing_callbacks.is_empty());
        // okIfFnNull=false: the WARNING face.
        assert!(!app.entity_callback(77, "onFoo", false, None));
        assert_eq!(app.missing_callbacks, vec![(77, "onFoo")]);

        // An unknown id is a caller contract breach: refused.
        assert!(!app.entity_callback(12345, "poke", true, None));
    }

    #[test]
    fn single_shot_timer_fires_once_then_releases() {
        let mut app = app_with_type(vec![], &["onTimer"]);
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        let id = app.add_timer(77, 0.0, 0.0, 3);
        assert_eq!(id, 1);
        assert_eq!(
            app.entity_population[&77].borrow().controllers.container[&1],
            Controller {
                kind: ControllerKind::Timer {
                    start: 1,
                    interval: 0
                },
                user_arg: 3
            }
        );

        app.advance_tick(); // time 1: the timer fires
        assert_eq!(app.time, 1);
        assert_eq!(
            app.script_calls[1],
            ScriptCall {
                entity: 77,
                name: "onTimer",
                timer: Some((1, 3))
            }
        );
        // onHandlerRelease cancelled the controller.
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );

        app.advance_tick(); // nothing left to fire
        assert_eq!(app.script_calls.len(), 2);
    }

    #[test]
    fn repeating_timer_fires_each_interval_then_cancels() {
        let mut app = app_with_type(vec![], &["onTimer"]);
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        let id = app.add_timer(77, 0.1, 0.3, 7);
        assert_eq!(id, 1);
        assert_eq!(
            app.entity_population[&77].borrow().controllers.container[&1].kind,
            ControllerKind::Timer {
                start: 1,
                interval: 3
            }
        );

        app.advance_tick(); // t=1: fires, start bumps to 4
        app.advance_tick(); // t=2
        app.advance_tick(); // t=3
        assert_eq!(app.script_calls.len(), 2);
        assert_eq!(
            app.entity_population[&77].borrow().controllers.container[&1].kind,
            ControllerKind::Timer {
                start: 4,
                interval: 3
            }
        );

        app.advance_tick(); // t=4: fires again
        assert_eq!(app.script_calls.len(), 3);
        assert_eq!(app.script_calls[2].timer, Some((1, 7)));

        // Entity.cancel → delController → stopReal.
        assert!(app.cancel_controller(77, 1));
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
        app.advance_tick(); // t=5: nothing
        assert_eq!(app.script_calls.len(), 3);
    }

    #[test]
    fn cancel_unknown_controller_warns() {
        let mut app = app_with_type(vec![], &[]);
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        assert!(!app.cancel_controller(77, 9));
        assert_eq!(
            app.entity_population[&77].borrow().controllers.failed_dels,
            1
        );
        assert!(!app.cancel_controller(12345, 1)); // unknown entity
        // An unknown entity refuses addTimer too.
        assert_eq!(app.add_timer(12345, 0.0, 0.0, 0), 0);
        // Unknown controller ids are inert on the timer faces.
        let rc = app.entity_population[&77].clone();
        app.start_timer_controller(&rc, 99, true);
        app.entity_population[&77]
            .borrow_mut()
            .controllers
            .advance_timer_start(99);
        // Router events for an entity that left the population are inert.
        app.handle_timeout_event(12345, 1, 0);
        app.handle_release_event(12345, 1);
    }

    #[test]
    fn controller_ids_skip_used_and_zero() {
        let mut controllers = Controllers::default();
        let timer = Controller {
            kind: ControllerKind::Timer {
                start: 1,
                interval: 0,
            },
            user_arg: 0,
        };
        assert_eq!(timer.presolved_exclusive_id(), 0);
        assert_eq!(timer.domain(), DOMAIN_REAL);
        assert_eq!(controllers.next_controller_id(), 1);
        assert_eq!(controllers.next_controller_id(), 2);

        // Wrap at the exclusive mask: 0x7fff+1 folds to 0, which is
        // skipped, then the used id 1 is skipped.
        controllers.last_allocated_id = EXCLUSIVE_CONTROLLER_ID_MASK;
        controllers.container.insert(1, timer);
        assert_eq!(controllers.next_controller_id(), 2);
    }

    #[test]
    fn controller_limits() {
        let mut app = app_with_type(vec![], &[]);
        app.expected_max_controllers = 1;
        app.absolute_max_controllers = 4;
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        assert_eq!(app.add_timer(77, 0.0, 0.0, 0), 1);
        assert_eq!(app.add_timer(77, 0.0, 0.0, 0), 2);
        // Third add: count 2 > expected 1 → the WARNING face.
        assert_eq!(app.add_timer(77, 0.0, 0.0, 0), 3);
        assert_eq!(app.add_timer(77, 0.0, 0.0, 0), 4);
        assert_eq!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .over_expected_adds,
            2
        );
        // Fifth add: count 4 >= absolute 4 → refused with 0.
        assert_eq!(app.add_timer(77, 0.0, 0.0, 0), 0);
        assert_eq!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .over_max_adds,
            1
        );
        assert_eq!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .len(),
            4
        );
    }

    #[test]
    fn timer_offset_rounding() {
        let mut app = app_with_type(vec![], &[]);
        app.time = 100;
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );

        // (0.15 * 10 + 0.5) = 2 ticks ahead of the current time.
        assert_eq!(app.add_timer(77, 0.15, 0.05, 0), 1);
        assert_eq!(
            app.entity_population[&77].borrow().controllers.container[&1].kind,
            ControllerKind::Timer {
                start: 102,
                interval: 1
            }
        );
        // A negative start rounds up to 1 tick.
        assert_eq!(app.add_timer(77, -0.5, 0.0, 0), 2);
        assert_eq!(
            app.entity_population[&77].borrow().controllers.container[&2].kind,
            ControllerKind::Timer {
                start: 101,
                interval: 0
            }
        );
        // A sub-tick start rounds up to 1 too.
        assert_eq!(app.add_timer(77, 0.04, 0.0, 0), 3);
        assert_eq!(
            app.entity_population[&77].borrow().controllers.container[&3].kind,
            ControllerKind::Timer {
                start: 101,
                interval: 0
            }
        );
        // A sub-tick repeat warns and rounds up to 1 as well.
        assert_eq!(app.add_timer(77, 0.0, 0.02, 0), 4);
        assert_eq!(
            app.entity_population[&77].borrow().controllers.container[&4].kind,
            ControllerKind::Timer {
                start: 101,
                interval: 1
            }
        );
    }

    #[test]
    fn stale_start_clamped_on_restart() {
        let mut app = CellApp::default();
        let mut script = EntityScript::default();
        script.methods.insert("onTimer");
        app.entity_scripts.insert(TYPE, script);
        let mut entity = Entity::new(80, true);
        entity.type_id = TYPE;
        entity.controllers.container.insert(
            4,
            Controller {
                kind: ControllerKind::Timer {
                    start: 3,
                    interval: 2,
                },
                user_arg: 9,
            },
        );
        let rc = Rc::new(RefCell::new(entity));
        app.entity_population.insert(80, Rc::clone(&rc));

        // The onboard face: startReal( false ) clamps a stale start.
        app.time = 10;
        start_real_controllers(&mut app, &rc);
        assert_eq!(
            rc.borrow().controllers.container[&4].kind,
            ControllerKind::Timer {
                start: 10,
                interval: 2
            }
        );

        // A timeout for a controller that is already gone steps nothing.
        rc.borrow_mut().controllers.advance_timer_start(99);

        app.advance_tick(); // t=11: the clamped timer fires now
        assert_eq!(
            app.script_calls.last().copied(),
            Some(ScriptCall {
                entity: 80,
                name: "onTimer",
                timer: Some((4, 9))
            })
        );
    }

    #[test]
    fn witness_faces() {
        let mut app = app_with_type(vec![], &[]);
        let mut spaces = spaces_with();

        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(71, TYPE, b'-')),
            CreateRealOutcome::Created(71)
        );
        assert!(!app.entity_population[&71].borrow().has_witness);
        // An illegal indicator carries on without a witness.
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(72, TYPE, b'X')),
            CreateRealOutcome::Created(72)
        );
        assert!(!app.entity_population[&72].borrow().has_witness);
    }

    #[test]
    fn callback_buffer_mechanics() {
        let mut buffer = CallbackBuffer::default();
        assert!(buffer.permitted());
        buffer.enter_section();
        assert!(!buffer.permitted());
        buffer.store(BufferedCallback {
            entity: 1,
            name: "normal",
            timer: None,
        });
        buffer.enable_high_priority();
        buffer.store(BufferedCallback {
            entity: 1,
            name: "high",
            timer: Some((2, 5)),
        });
        buffer.disable_high_priority();

        let due = buffer.take_buffers();
        assert_eq!(
            due,
            vec![
                BufferedCallback {
                    entity: 1,
                    name: "high",
                    timer: Some((2, 5))
                },
                BufferedCallback {
                    entity: 1,
                    name: "normal",
                    timer: None
                },
            ]
        );
        // Taking the buffers leaves the section open.
        assert!(!buffer.permitted());
        assert!(buffer.take_buffers().is_empty());
        buffer.leave_section();
        assert!(buffer.permitted());
    }

    #[test]
    fn callback_buffer_counts_windows() {
        let mut buffer = CallbackBuffer::default();
        buffer.enter_section();
        buffer.enter_section();
        // The first leave does not flush; only the last one does.
        assert!(!buffer.leave_section());
        assert!(buffer.leave_section());
    }

    #[test]
    fn exclusive_ids_allocate_above_the_mask() {
        let mut app = app_with_type(vec![], &[]);
        // `getExclusiveID`: a new category mints
        // MASK + size + 1, above the exclusive mask.
        assert_eq!(app.get_exclusive_id("Movement"), 0x7fff + 1);
        assert_eq!(app.get_exclusive_id("Navigation"), 0x7fff + 2);
        // A known category answers its existing id.
        assert_eq!(app.get_exclusive_id("Movement"), 0x7fff + 1);
    }

    #[test]
    fn re_registering_an_exclusive_controller_replaces_the_incumbent() {
        let mut app = app_with_type(vec![], &[]);
        app.entity_population
            .insert(77, Rc::new(RefCell::new(Entity::new(77, true))));
        let movement = app.get_exclusive_id("Movement");

        let first = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::Exclusive { category: movement },
                user_arg: 1,
            },
        );
        assert_eq!(first, movement);
        let second = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::Exclusive { category: movement },
                user_arg: 2,
            },
        );
        // The same category: the same id, one entry — the incumbent was
        // deleted (silently, warnOnFailure false) before the insert.
        assert_eq!(second, movement);
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.controllers.container.len(), 1);
        assert_eq!(entity.controllers.container[&movement].user_arg, 2);
        assert_eq!(entity.controllers.dels, 1);
        assert_eq!(entity.controllers.failed_dels, 0);
        // The plain allocator never ran for the exclusive ids.
        assert_eq!(entity.controllers.last_allocated_id, 0);
    }

    #[test]
    fn attaching_an_exclusive_controller_starts_no_drive_timer() {
        let mut app = app_with_type(vec![], &[]);
        app.entity_population
            .insert(77, Rc::new(RefCell::new(Entity::new(77, true))));
        let movement = app.get_exclusive_id("Movement");
        let id = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::Exclusive { category: movement },
                user_arg: 0,
            },
        );
        // The exclusive families arm their own drive timer in BW-08-02-03;
        // attaching one records the controller but nothing on the queue.
        let entity = app.entity_population[&77].borrow();
        assert!(!entity.controllers.handles.contains_key(&id));
        drop(entity);
        // The expiry bookkeeping is a timer face: it leaves an exclusive
        // controller alone.
        app.entity_population[&77]
            .borrow_mut()
            .controllers
            .advance_timer_start(id);
        app.advance_tick();
        assert!(app.script_calls.is_empty());
        // An unknown entity refuses the attach too.
        assert_eq!(
            app.attach_controller(
                12345,
                Controller {
                    kind: ControllerKind::Timer {
                        start: 1,
                        interval: 0
                    },
                    user_arg: 0
                }
            ),
            0
        );
    }

    #[test]
    fn re_touching_an_extra_keeps_the_same_instance() {
        let mut app = app_with_type(vec![], &[]);
        let eeid = app.register_entity_extra("PassengerExtra");
        assert_eq!(eeid, 0);
        app.entity_population
            .insert(77, Rc::new(RefCell::new(Entity::new(77, true))));

        // Nothing touched yet: no instance, though the entity is known.
        assert!(!app.extra_exists(77, eeid));
        // `operator()`: the first touch constructs; the second answers
        // the same instance.
        assert_eq!(app.touch_extra(77, eeid), Some(0));
        assert_eq!(app.touch_extra(77, eeid), Some(0));
        assert!(app.extra_exists(77, eeid));
        assert!(app.extra_drops.borrow().is_empty());
        // An eeid with no registered type has no slot to touch, nor
        // does an unknown entity.
        assert_eq!(app.touch_extra(77, 4), None);
        assert_eq!(app.touch_extra(12345, eeid), None);
        assert!(!app.extra_exists(12345, eeid));
    }

    #[test]
    fn clearing_an_extra_deletes_the_old_object() {
        let mut app = app_with_type(vec![], &[]);
        let eeid = app.register_entity_extra("PassengerExtra");
        app.entity_population
            .insert(77, Rc::new(RefCell::new(Entity::new(77, true))));
        assert_eq!(app.touch_extra(77, eeid), Some(0));

        app.clear_extra(77, eeid);
        // The deletion landed in the ledger.
        assert_eq!(*app.extra_drops.borrow(), vec![(77, eeid, 0)]);
        assert!(!app.extra_exists(77, eeid));
        // Instance::clear is safe when absent: no second drop.
        app.clear_extra(77, eeid);
        assert_eq!(app.extra_drops.borrow().len(), 1);
        // Clearing an eeid the entity has no slot for, or an unknown
        // entity, is inert.
        app.clear_extra(77, 4);
        app.clear_extra(12345, eeid);
        assert_eq!(app.extra_drops.borrow().len(), 1);
        // A fresh touch constructs a NEW instance — the old object is
        // not left behind.
        assert_eq!(app.touch_extra(77, eeid), Some(1));
        assert!(app.extra_exists(77, eeid));
    }

    #[test]
    fn destroying_the_entity_clears_extras_and_controllers() {
        let mut app = app_with_type(
            vec![InitAction::AddTimer {
                start: 0.1,
                repeat: 0.0,
                user_arg: 7,
            }],
            &[],
        );
        let eeid = app.register_entity_extra("PassengerExtra");
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );
        assert_eq!(app.add_timer(77, 2.0, 0.0, 0), 2);
        assert_eq!(app.touch_extra(77, eeid), Some(0));

        app.destroy_entity(77);
        let entity = app.entity_population[&77].borrow();
        assert!(entity.destroyed);
        assert!(entity.controllers.container.is_empty());
        assert!(entity.extras.is_empty());
        drop(entity);
        // The extra's destruction was recorded exactly once.
        assert_eq!(*app.extra_drops.borrow(), vec![(77, eeid, 0)]);
        assert!(!app.extra_exists(77, eeid));
    }

    #[test]
    fn reviving_the_entity_wipes_extras_and_controllers() {
        let mut app = app_with_type(
            vec![InitAction::AddTimer {
                start: 0.1,
                repeat: 0.0,
                user_arg: 7,
            }],
            &[],
        );
        let eeid = app.register_entity_extra("PassengerExtra");
        let mut spaces = spaces_with();
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );
        assert_eq!(app.touch_extra(77, eeid), Some(0));

        // The half-built face (`setDestroyed` only — initReal's failure
        // path never reaches destroy()): the extras and controllers stay
        // in the population (the reference's decRef drops only the local
        // ref).
        app.entity_population[&77].borrow_mut().destroyed = true;
        assert!(app.extra_exists(77, eeid));
        assert!(app.extra_drops.borrow().is_empty());

        // The revival's `setToInitialState` wipes both: the old extra
        // deletes, and the init runs on a fresh Controllers object.
        assert_eq!(
            app.create_real_entity(&mut spaces, SID, &create_stream(77, TYPE, b'-')),
            CreateRealOutcome::Created(77)
        );
        assert_eq!(*app.extra_drops.borrow(), vec![(77, eeid, 0)]);
        assert!(!app.extra_exists(77, eeid));
        let entity = app.entity_population[&77].borrow();
        assert_eq!(
            entity.controllers.container[&1].kind,
            ControllerKind::Timer {
                start: 1,
                interval: 0
            }
        );
    }
}
