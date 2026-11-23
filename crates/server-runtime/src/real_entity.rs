//! BW-08-02-01: create a real entity and advance the tick.
//!
//! `Cell::createEntityInternal` (`cell.cpp:302`) builds the real on a cell:
//! it reads the id and type (allocating an id when the request wants one),
//! returns an existing non-destroyed entity rather than a duplicate, then
//! constructs the entity and calls `initReal` (`entity.cpp:1425`), which adds
//! it to the space, creates the real, and finally runs the script initialiser
//! (`callScriptInit`, `entity.cpp:1916`): `onRestore` when restoring,
//! `__init__` otherwise, then `onEntityInitialised` on the delegate
//! (`:1945`). Only then does `addRealEntity` (`cell.cpp:237`) add the real to
//! the cell's collection and inform the base of the address.
//!
//! The tick is `CellApp::handleGameTickTimeSlice` (`cellapp.cpp:1395`):
//! update the load, inform the manager, advance the game time
//! (`advanceTime`), back up, then check offloads and reclaim old spaces.
//! Driving the tick is what fires the timers: `TimerController::handleTimeout`
//! (`timer_controller.cpp:161`) calls `standardCallback("onTimer")`
//! (`controller.cpp:139`), which invokes the entity's `onTimer(controllerID,
//! userArg)` script method every `interval_` ticks from `start_`.
//!
//! The acceptance is that the script callbacks arrive in the reference order —
//! [`EntityCallbacks`] records them in the exact sequence the reference fires
//! them, so a test can compare against the expected order.

use std::collections::BTreeMap;

use crate::space::SpaceId;

/// `EntityID` (`server/common.hpp`).
pub type EntityId = u32;

/// `EntityTypeID` (`server/common.hpp`): the index of an entity description.
pub type EntityTypeId = i16;

/// `GameTime` (`server/common.hpp`).
pub type GameTime = u32;

/// `ControllerID` / `UserArg` (`controller.hpp`).
pub type ControllerId = i32;
pub type UserArg = i32;

/// The mode the real was created in (`Entity::initReal`, `entity.cpp:1425`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateMode {
    /// `CREATE_REAL_FROM_INIT`.
    New,
    /// `CREATE_REAL_FROM_RESTORE`.
    Restore,
}

/// A script callback the entity fired, in the order it fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptCallback {
    /// `__init__` (`entity.cpp:1924`).
    Init,
    /// `onRestore` (`entity.cpp:1920`).
    Restore,
    /// `onEntityInitialised` on the delegate (`entity.cpp:1945`).
    EntityInitialised,
    /// `onTimer(controllerID, userArg)` (`controller.cpp:150`).
    Timer {
        controller: ControllerId,
        user_arg: UserArg,
    },
    /// The script constructor finished but the entity was destroyed during
    /// `__init__` (`entity.cpp:210`).
    Destroyed,
}

/// The ordered script-callback record of one entity's lifecycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityCallbacks {
    sequence: Vec<ScriptCallback>,
}

impl EntityCallbacks {
    /// An empty record.
    pub const fn new() -> Self {
        EntityCallbacks {
            sequence: Vec::new(),
        }
    }

    /// The callbacks, in the order they fired.
    pub fn sequence(&self) -> &[ScriptCallback] {
        &self.sequence
    }

    /// Record one callback.
    pub fn push(&mut self, callback: ScriptCallback) {
        self.sequence.push(callback);
    }

    /// The number of callbacks recorded.
    pub fn len(&self) -> usize {
        self.sequence.len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.sequence.is_empty()
    }
}

/// The request to create a real (`Cell::createEntityInternal`,
/// `cell.cpp:302`).
#[derive(Debug, Clone, PartialEq)]
pub struct CreateRequest {
    /// The requested id, `0` to allocate one.
    pub id: EntityId,
    /// `entityTypeID`.
    pub entity_type: EntityTypeId,
    /// `localPosition_`.
    pub position: [f32; 3],
    /// `isRestore`.
    pub mode: CreateMode,
    /// The space the entity is created in.
    pub space_id: SpaceId,
}

impl CreateRequest {
    /// A request with an id to allocate.
    pub fn with_id(entity_type: EntityTypeId, space_id: SpaceId) -> Self {
        CreateRequest {
            id: 0,
            entity_type,
            position: [0.0, 0.0, 0.0],
            mode: CreateMode::New,
            space_id,
        }
    }
}

/// The outcome of `Cell::createEntityInternal`.
#[derive(Debug, Clone, PartialEq)]
pub enum CreateOutcome {
    /// A fresh real was built; `addRealEntity` still has to be called
    /// (`cell.cpp:237`).
    Created(RealEntity),
    /// The entity already exists and the existing one is returned instead
    /// (`cell.cpp:348`).
    AlreadyExists(EntityId),
    /// No free ids (`cell.cpp:320`).
    NoFreeIds,
}

/// The ID allocator (`IDClient`): hands out distinct ids. The reference
/// guarantees uniqueness across the cluster; here the monotonic counter does.
#[derive(Debug, Clone)]
pub struct IdAllocator {
    next: EntityId,
}

impl IdAllocator {
    /// An allocator starting after the given id.
    pub const fn new(first: EntityId) -> Self {
        IdAllocator { next: first }
    }

    /// `IDClient::getID` (`cell.cpp:312`): the next free id, `0` when exhausted.
    pub fn get_id(&mut self) -> EntityId {
        if self.next == 0 {
            return 0; // 0 is the "no ids" sentinel.
        }
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        id
    }
}

/// A `TimerController` (`timer_controller.hpp`): fires `onTimer` every
/// `interval` ticks from `start` (`timer_controller.cpp:107`).
#[derive(Debug, Clone, PartialEq)]
pub struct TimerController {
    controller_id: ControllerId,
    user_arg: UserArg,
    /// `start_`: the first tick it fires.
    start: GameTime,
    /// `interval_`: ticks between fires.
    interval: GameTime,
    /// The next tick it will fire.
    next: GameTime,
    /// Whether the timer is running (`timerHandle_`).
    active: bool,
}

impl TimerController {
    /// `TimerController::TimerController(start, interval)` and the `userArg`
    /// from the factory (`timer_controller.cpp`, `New`).
    pub fn new(
        controller_id: ControllerId,
        user_arg: UserArg,
        start: GameTime,
        interval: GameTime,
    ) -> Self {
        TimerController {
            controller_id,
            user_arg,
            start,
            interval,
            next: start,
            active: true,
        }
    }

    /// The controller id.
    pub fn controller_id(&self) -> ControllerId {
        self.controller_id
    }

    /// The id the script sees (`standardCallback`).
    pub fn user_arg(&self) -> UserArg {
        self.user_arg
    }

    /// `timerHandle_.isSet()`.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// `TimerController::stopReal` (`timer_controller.cpp:115`): cancel the
    /// timer.
    pub fn stop(&mut self) {
        self.active = false;
    }

    /// `TimerController::startReal` (`timer_controller.cpp:90`): start or
    /// restart the timer.
    pub fn start(&mut self, now: GameTime) {
        // On a re-start after an offload, a start time in the past becomes
        // now (`timer_controller.cpp:99`).
        if self.start < now {
            self.start = now;
            self.next = now;
        }
        self.active = true;
    }

    /// `TimerController::handleTimeout` (`timer_controller.cpp:161`): consume
    /// this fire and report the next one.
    pub fn handle_timeout(&mut self) {
        self.start += self.interval;
        self.next = self.start;
    }
}

/// A real entity (`Entity`, `entity.cpp`; `Real`, `real.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct RealEntity {
    id: EntityId,
    entity_type: EntityTypeId,
    /// `localPosition_`.
    position: [f32; 3],
    /// `isReal_`: this app owns the real (the ghost is the other end).
    is_real: bool,
    /// `isDestroyed_`.
    destroyed: bool,
    /// `pSpace_` / the space this entity lives in.
    space_id: SpaceId,
    /// The script callbacks, in order.
    callbacks: EntityCallbacks,
    /// The timers this entity owns (`TimerController`), by controller id.
    timers: BTreeMap<ControllerId, TimerController>,
    /// The next controller id to hand out.
    next_controller: ControllerId,
}

impl RealEntity {
    /// `Cell::createEntityInternal`'s construction of the entity and its
    /// `initReal` callbacks (`cell.cpp:302`, `entity.cpp:1916`).
    pub fn new(request: &CreateRequest, id: EntityId) -> Self {
        let mut callbacks = EntityCallbacks::new();
        // `callScriptInit` (`entity.cpp:1916`): the first script callback.
        match request.mode {
            CreateMode::Restore => callbacks.push(ScriptCallback::Restore),
            CreateMode::New => callbacks.push(ScriptCallback::Init),
        }
        // `pEntityDelegate_->onEntityInitialised()` (`entity.cpp:1945`).
        callbacks.push(ScriptCallback::EntityInitialised);

        RealEntity {
            id,
            entity_type: request.entity_type,
            position: request.position,
            is_real: true,
            destroyed: false,
            space_id: request.space_id,
            callbacks,
            timers: BTreeMap::new(),
            next_controller: 1,
        }
    }

    /// The entity's id.
    pub fn id(&self) -> EntityId {
        self.id
    }

    /// The entity's type.
    pub fn entity_type(&self) -> EntityTypeId {
        self.entity_type
    }

    /// `localPosition_`.
    pub fn position(&self) -> [f32; 3] {
        self.position
    }

    /// The space the entity lives in.
    pub fn space_id(&self) -> SpaceId {
        self.space_id
    }

    /// `Entity::isReal`.
    pub fn is_real(&self) -> bool {
        self.is_real
    }

    /// `Entity::isDestroyed`.
    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// The script callbacks, in order (`callScriptInit`).
    pub fn callbacks(&self) -> &[ScriptCallback] {
        self.callbacks.sequence()
    }

    /// `Cell::createEntityInternal`'s `initReal` failure (`cell.cpp:366`):
    /// mark the half-built entity destroyed so the destructor cleans up.
    pub fn fail_init(&mut self) {
        self.destroyed = true;
        self.callbacks.push(ScriptCallback::Destroyed);
    }

    /// `BigWorld.addTimer` / `PY_AUTO_CONTROLLER_FACTORY` (`timer_controller`):
    /// add a timer that fires `onTimer` every `interval` ticks from `start`.
    pub fn add_timer(
        &mut self,
        start: GameTime,
        interval: GameTime,
        user_arg: UserArg,
    ) -> ControllerId {
        let controller_id = self.next_controller;
        self.next_controller += 1;
        self.timers.insert(
            controller_id,
            TimerController::new(controller_id, user_arg, start, interval),
        );
        controller_id
    }

    /// `TimerController::stopReal` via `BigWorld.cancelTimer`:
    /// stop a timer.
    pub fn cancel_timer(&mut self, controller_id: ControllerId) {
        if let Some(timer) = self.timers.get_mut(&controller_id) {
            timer.stop();
        }
    }

    /// The timers this entity owns, by controller id.
    pub fn timers(&self) -> impl Iterator<Item = &TimerController> {
        self.timers.values()
    }

    /// Drive the tick forward to `now`, firing every timer whose `onTimer` is
    /// due, and record each `onTimer` script callback in order
    /// (`TimerController::handleTimeout`).
    pub fn tick(&mut self, now: GameTime) {
        if self.destroyed {
            return;
        }
        // The timers due at or before `now`, in the order the reference's
        // time queue would fire them (by next fire time, then controller id).
        let mut due: Vec<(GameTime, ControllerId)> = self
            .timers
            .iter()
            .filter(|(_, t)| t.is_active() && t.next <= now)
            .map(|(id, t)| (t.next, *id))
            .collect();
        due.sort_unstable();
        for (_, controller_id) in due {
            let timer = self
                .timers
                .get_mut(&controller_id)
                .expect("key collected from the map");
            if !timer.is_active() || timer.next > now {
                continue;
            }
            timer.handle_timeout();
            self.callbacks.push(ScriptCallback::Timer {
                controller: controller_id,
                user_arg: timer.user_arg(),
            });
        }
    }

    /// `Entity::destroy` (`entity.cpp`): mark the entity destroyed.
    pub fn destroy(&mut self) {
        self.destroyed = true;
    }
}

/// `Cell::createEntityInternal` (`cell.cpp:302`): the real-entity creation
/// flow. Returns the entity or the reference's early-out outcomes.
pub fn create_real(
    request: &CreateRequest,
    ids: &mut IdAllocator,
    existing: &[EntityId],
) -> CreateOutcome {
    let id = if request.id == 0 {
        let allocated = ids.get_id();
        if allocated == 0 {
            return CreateOutcome::NoFreeIds;
        }
        allocated
    } else {
        request.id
    };

    // An existing, non-destroyed entity is returned rather than duplicated
    // (`cell.cpp:336`). The zombie-ghost restore branch is not modelled here.
    if existing.contains(&id) {
        return CreateOutcome::AlreadyExists(id);
    }

    CreateOutcome::Created(RealEntity::new(request, id))
}

/// One step of `CellApp::handleGameTickTimeSlice` (`cellapp.cpp:1395`), in
/// the order the reference runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickStep {
    /// `updateLoad`.
    UpdateLoad,
    /// `cellAppMgr_.informOfLoad`.
    InformOfLoad,
    /// `updateBoundary`.
    UpdateBoundary,
    /// `advanceTime` — the tick advances.
    AdvanceTime,
    /// `tickBackup`.
    TickBackup,
    /// `checkSendWindowOverflows`.
    CheckSendWindowOverflows,
    /// `checkOffloads`.
    CheckOffloads,
    /// `pSpaces_->deleteOldSpaces`.
    DeleteOldSpaces,
    /// `syncTime`.
    SyncTime,
    /// `tickStats`.
    TickStats,
    /// `bufferedEntityMessages().playBufferedMessages`.
    PlayBufferedEntityMessages,
    /// `bufferedInputMessages().playBufferedMessages`.
    PlayBufferedInputMessages,
    /// `cells_.tickRecordings`.
    TickRecordings,
}

/// The tick steps `CellApp::handleGameTickTimeSlice` runs, in order.
pub fn game_tick_steps() -> &'static [TickStep] {
    &[
        TickStep::UpdateLoad,
        TickStep::InformOfLoad,
        TickStep::UpdateBoundary,
        TickStep::AdvanceTime,
        TickStep::TickBackup,
        TickStep::CheckSendWindowOverflows,
        TickStep::CheckOffloads,
        TickStep::DeleteOldSpaces,
        TickStep::SyncTime,
        TickStep::TickStats,
        TickStep::PlayBufferedEntityMessages,
        TickStep::PlayBufferedInputMessages,
        TickStep::TickRecordings,
    ]
}

/// The per-tick controller of one cell app: advances the game time and ticks
/// every entity's timers.
#[derive(Debug, Clone)]
pub struct GameTicker {
    /// `time_`.
    time: GameTime,
}

impl GameTicker {
    /// A ticker starting at game time 0.
    pub const fn new() -> Self {
        GameTicker { time: 0 }
    }

    /// The current game time.
    pub fn time(&self) -> GameTime {
        self.time
    }

    /// `CellApp::advanceTime`: move to the next tick and return the steps the
    /// game tick runs (`handleGameTickTimeSlice`).
    pub fn advance(&mut self) -> &'static [TickStep] {
        self.time += 1;
        game_tick_steps()
    }

    /// Tick the given entity to the current time (fires its due timers).
    pub fn tick_entity(&self, entity: &mut RealEntity) {
        entity.tick(self.time);
    }
}

impl Default for GameTicker {
    fn default() -> Self {
        GameTicker::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(entity_type: EntityTypeId) -> CreateRequest {
        CreateRequest::with_id(entity_type, 7)
    }

    /// The creation callbacks are `__init__` then `onEntityInitialised`, in
    /// the reference order (`entity.cpp:1916`, `:1945`).
    #[test]
    fn creation_fires_init_then_entity_initialised() {
        let mut ids = IdAllocator::new(1);
        let outcome = create_real(&request(3), &mut ids, &[]);

        let CreateOutcome::Created(entity) = outcome else {
            panic!("expected a created entity");
        };
        assert_eq!(entity.id(), 1);
        assert_eq!(entity.entity_type(), 3);
        assert!(entity.is_real());
        assert!(!entity.is_destroyed());
        assert_eq!(
            entity.callbacks(),
            &[ScriptCallback::Init, ScriptCallback::EntityInitialised]
        );
    }

    /// A restore fires `onRestore` first, not `__init__` (`entity.cpp:1918`).
    #[test]
    fn a_restore_fires_on_restore_first() {
        let mut req = request(3);
        req.mode = CreateMode::Restore;
        let mut ids = IdAllocator::new(1);
        let CreateOutcome::Created(entity) = create_real(&req, &mut ids, &[]) else {
            panic!("expected a created entity");
        };
        assert_eq!(
            entity.callbacks(),
            &[ScriptCallback::Restore, ScriptCallback::EntityInitialised]
        );
    }

    /// An explicit id is honoured, and the allocator is not advanced.
    #[test]
    fn an_explicit_id_is_honoured() {
        let mut req = request(3);
        req.id = 42;
        let mut ids = IdAllocator::new(1);
        let CreateOutcome::Created(entity) = create_real(&req, &mut ids, &[]) else {
            panic!("expected a created entity");
        };
        assert_eq!(entity.id(), 42);
        assert_eq!(ids.get_id(), 1, "the allocator was not touched");
    }

    /// An existing entity is returned instead of duplicated (`cell.cpp:348`).
    #[test]
    fn an_existing_entity_is_returned() {
        let mut req = request(3);
        req.id = 9;
        let mut ids = IdAllocator::new(1);
        let outcome = create_real(&req, &mut ids, &[9]);
        assert_eq!(outcome, CreateOutcome::AlreadyExists(9));
    }

    /// Running out of ids is reported (`cell.cpp:320`).
    #[test]
    fn running_out_of_ids_is_reported() {
        let mut ids = IdAllocator::new(0);
        let outcome = create_real(&request(3), &mut ids, &[]);
        assert_eq!(outcome, CreateOutcome::NoFreeIds);
    }

    /// A timer fires `onTimer` every interval from its start
    /// (`timer_controller.cpp:161`).
    #[test]
    fn a_timer_fires_on_timer_every_interval() {
        let mut ids = IdAllocator::new(1);
        let CreateOutcome::Created(mut entity) = create_real(&request(3), &mut ids, &[]) else {
            panic!("expected a created entity");
        };
        entity.add_timer(2, 3, 7); // fires at 2, 5, 8, ...

        let mut ticker = GameTicker::new();
        ticker.advance();
        ticker.tick_entity(&mut entity);
        assert_eq!(entity.callbacks().len(), 2, "nothing due before tick 2");

        ticker.advance();
        ticker.tick_entity(&mut entity);
        assert_eq!(
            entity.callbacks(),
            &[
                ScriptCallback::Init,
                ScriptCallback::EntityInitialised,
                ScriptCallback::Timer {
                    controller: 1,
                    user_arg: 7,
                },
            ]
        );

        // Fires again at 2 + 3 = 5.
        for _ in 0..2 {
            ticker.advance();
            ticker.tick_entity(&mut entity);
        }
        assert_eq!(entity.callbacks().len(), 3, "not yet due at 3 or 4");
        ticker.advance();
        ticker.tick_entity(&mut entity);
        assert_eq!(entity.callbacks().len(), 4);
        ticker.advance();
        ticker.tick_entity(&mut entity);
        assert_eq!(entity.callbacks().len(), 4, "the next fire is at 8");
    }

    /// Two timers fire in the order of their due ticks
    /// (`handleTimeout`/`timeQueue`).
    #[test]
    fn two_timers_fire_in_due_order() {
        let mut ids = IdAllocator::new(1);
        let CreateOutcome::Created(mut entity) = create_real(&request(3), &mut ids, &[]) else {
            panic!("expected a created entity");
        };
        entity.add_timer(1, 10, 100); // due at 1, 11, 21...
        entity.add_timer(3, 5, 200); // due at 3, 8, 13...

        let mut ticker = GameTicker::new();
        for _ in 0..12 {
            ticker.advance();
            ticker.tick_entity(&mut entity);
        }

        // At tick 1 the first timer fired once; at tick 3 the second; at tick
        // 8 the second again; at tick 11 the first.
        assert_eq!(
            entity.callbacks(),
            &[
                ScriptCallback::Init,
                ScriptCallback::EntityInitialised,
                ScriptCallback::Timer {
                    controller: 1,
                    user_arg: 100,
                },
                ScriptCallback::Timer {
                    controller: 2,
                    user_arg: 200,
                },
                ScriptCallback::Timer {
                    controller: 2,
                    user_arg: 200,
                },
                ScriptCallback::Timer {
                    controller: 1,
                    user_arg: 100,
                },
            ]
        );
    }

    /// Cancelling a timer stops it (`stopReal`).
    #[test]
    fn cancelling_a_timer_stops_it() {
        let mut ids = IdAllocator::new(1);
        let CreateOutcome::Created(mut entity) = create_real(&request(3), &mut ids, &[]) else {
            panic!("expected a created entity");
        };
        let timer = entity.add_timer(1, 1, 5);
        entity.cancel_timer(timer);

        let mut ticker = GameTicker::new();
        for _ in 0..5 {
            ticker.advance();
            ticker.tick_entity(&mut entity);
        }
        assert_eq!(
            entity.callbacks().len(),
            2,
            "the cancelled timer never fired"
        );
    }

    /// A destroyed entity no longer ticks (`entity.cpp`).
    #[test]
    fn a_destroyed_entity_does_not_tick() {
        let mut ids = IdAllocator::new(1);
        let CreateOutcome::Created(mut entity) = create_real(&request(3), &mut ids, &[]) else {
            panic!("expected a created entity");
        };
        entity.add_timer(1, 1, 5);
        entity.destroy();

        let mut ticker = GameTicker::new();
        for _ in 0..5 {
            ticker.advance();
            ticker.tick_entity(&mut entity);
        }
        assert_eq!(entity.callbacks().len(), 2, "no timer fired after destroy");
        assert!(entity.is_destroyed());
    }

    /// `advanceTime` moves the game time and returns the tick steps in the
    /// reference order (`cellapp.cpp:1395`).
    #[test]
    fn advancing_a_tick_reports_the_steps_in_order() {
        let mut ticker = GameTicker::new();
        assert_eq!(ticker.time(), 0);
        let steps = ticker.advance();
        assert_eq!(ticker.time(), 1);
        assert_eq!(steps[0], TickStep::UpdateLoad);
        assert_eq!(steps[3], TickStep::AdvanceTime);
        assert_eq!(
            steps,
            &[
                TickStep::UpdateLoad,
                TickStep::InformOfLoad,
                TickStep::UpdateBoundary,
                TickStep::AdvanceTime,
                TickStep::TickBackup,
                TickStep::CheckSendWindowOverflows,
                TickStep::CheckOffloads,
                TickStep::DeleteOldSpaces,
                TickStep::SyncTime,
                TickStep::TickStats,
                TickStep::PlayBufferedEntityMessages,
                TickStep::PlayBufferedInputMessages,
                TickStep::TickRecordings,
            ]
        );
    }
}
