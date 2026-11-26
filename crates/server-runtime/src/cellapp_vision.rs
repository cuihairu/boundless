//! The script traps and the seer's cone: proximity triggers, the
//! periodic vision pass and the visibility gate.
//!
//! Reference faces:
//! - `server/cellapp/proximity_controller.cpp` — [`CellApp::add_proximity`]
//!   and the `ProximityRangeTrigger` faces: `standardEnter`/`standardLeave`
//!   (`onEnterTrap`/`onLeaveTrap`), `nonstandardLeave`
//!   (`onLeaveTrapID`), the restore's `OnloadHandler` scope and the
//!   stop's `NoOpHandler` swallow.
//! - `server/cellapp/vision_controller.cpp` — [`CellApp::add_vision`],
//!   `VisionController::startReal`/`stopReal`/`update` and the
//!   `VisionRangeTrigger` whose destructor drops without contracting
//!   once disabled.
//! - `server/cellapp/entity_vision.cpp` — the `EntityVision` extra:
//!   `updateVisibleEntities`' occlusion ladder (the two-try collide +
//!   cone check with the three-quarter-height retry), the sorted
//!   old/new diff (`onStartSeeing`/`onStopSeeing`), `setVisionRange`,
//!   `canBeSeen`, and the restore's `setVisibleEntities`.
//! - `server/cellapp/visibility_controller.cpp` — [`CellApp::add_visibility`],
//!   the ghost-domain controller whose presence alone opens `canBeSeen`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use crate::cellapp_range_list::{FLAG_ENTITY_TRIGGER, FLAG_NO_TRIGGERS, TriggerId, TriggerTarget};
use crate::cellapp_real_entity::{ControllerId, ControllerKind};
use crate::cellapp_space::{CellApp, Entity};

// ---------------------------------------------------------------------------
// Section: the ledger and state types
// ---------------------------------------------------------------------------

/// One trap callback the dispatch ran: the payload tuple the script
/// call alone cannot carry — `(who, range, controllerID, userArg)` for
/// `onEnterTrap`/`onLeaveTrap`, and the bare `(whoID, range,
/// controllerID, userArg)` for `onLeaveTrapID`.
#[derive(Clone, Debug, PartialEq)]
pub struct TrapCall {
    /// The entity owning the trap.
    pub entity: u32,
    /// The entity that crossed (`pWho`), or the raw id for
    /// `onLeaveTrapID`.
    pub who: u32,
    /// The callback name.
    pub name: &'static str,
    /// `this->range()`: the trap's range at dispatch.
    pub range: f32,
    /// `id_`: the controller id the script can cancel by.
    pub controller_id: ControllerId,
    /// `userArg_` — nonzero only when the script passed one.
    pub user_arg: i32,
}

/// One vision diff callback: `(target)` for
/// `onStartSeeing`/`onStopSeeing`, and the bare id for
/// `onStopSeeingID` (the restore's not-found face).
#[derive(Clone, Debug, PartialEq)]
pub struct VisionCall {
    /// The entity doing the seeing.
    pub entity: u32,
    /// The entity the callback names (the raw id for
    /// `onStopSeeingID`).
    pub target: u32,
    /// The callback name.
    pub name: &'static str,
}

/// A live trap: the trigger handle in the space's range list, the trap
/// range at start, and the recorder sink the range-list crossings land
/// in until a dispatch drains them.
#[derive(Clone, Debug)]
pub struct TrapTriggerState {
    /// The trap's owner.
    pub entity: u32,
    /// The trigger in [`CellApp::range_lists`].
    pub trigger: TriggerId,
    /// `range_` at start.
    pub range: f32,
    /// The attach-time `userArg_` — the payload tuple's last member.
    pub user_arg: i32,
    /// The `(entity, entering)` events the crossings pushed.
    pub sink: Rc<RefCell<Vec<(u32, bool)>>>,
}

/// A seer's `pVisionTrigger_`: the trigger handle and the range it was
/// last sized to.
#[derive(Clone, Debug)]
pub struct VisionTriggerState {
    /// The trigger in [`CellApp::range_lists`].
    pub trigger: TriggerId,
    /// `visionRange_`.
    pub range: f32,
    /// The `(entity, entering)` events the crossings pushed.
    pub sink: Rc<RefCell<Vec<(u32, bool)>>>,
}

/// `EntityVision` (`entity_vision.hpp`): the per-entity vision extra.
/// The stored heights and cone mirror the owning controllers — the
/// reference's accessors delegate to `vision_`/`visibility_` and read
/// `0.f` when the controller is absent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VisionExtra {
    /// `seeingHeight()` — the vision controller's seeing height.
    pub seeing_height: f32,
    /// `visibleHeight()` — the visibility controller's model height.
    pub visible_height: f32,
    /// `visionAngle()` — the cone's half-angle in radians.
    pub vision_angle: f32,
    /// `visionRange()`.
    pub vision_range: f32,
    /// `shouldDropVision_`: ground-snap the sight points (the ground
    /// query itself is the terrain lane's face — this port sights from
    /// the plain position).
    pub should_drop_vision: bool,
    /// `vision_`: the owning vision controller, when any. Its presence
    /// is the script factory's dedup guard.
    pub vision: Option<ControllerId>,
    /// `visibility_`: the visibility controller, when any. Its presence
    /// is `canBeSeen()`.
    pub visibility: Option<ControllerId>,
}

// ---------------------------------------------------------------------------
// Section: the script factories
// ---------------------------------------------------------------------------

impl CellApp {
    /// `Entity.addProximity` (`proximity_controller.cpp:136`'s `New`):
    /// the range must be positive, else the factory's AttributeError.
    pub fn add_proximity(
        &mut self,
        entity: u32,
        range: f32,
        user_arg: i32,
    ) -> Result<ControllerId, String> {
        if range <= 0.0 {
            return Err("Can't add proximity controller 0 or negative range".to_string());
        }
        let id = self.attach_controller(
            entity,
            crate::cellapp_real_entity::Controller {
                kind: ControllerKind::Proximity {
                    range,
                    onloaded: Vec::new(),
                },
                user_arg,
            },
        );
        if id == 0 {
            return Err("controller attach failed".to_string());
        }
        Ok(id)
    }

    /// `Entity.addVision` (`entity_vision.cpp:692`): real-only, the
    /// range must be positive, and one vision controller at a time.
    pub fn add_vision(
        &mut self,
        entity: u32,
        vision_angle: f32,
        vision_range: f32,
        seeing_height: f32,
        update_period: u32,
        user_arg: i32,
    ) -> Result<ControllerId, String> {
        let real = self
            .entity_population
            .get(&entity)
            .is_some_and(|entity_rc| entity_rc.borrow().is_real);
        if !real {
            return Err("This method can only be called on a real entity.".to_string());
        }
        if vision_range <= 0.0 {
            return Err("Can't start vision system with 0 or negative range".to_string());
        }
        let incumbent = self
            .vision_extras
            .get(&entity)
            .is_some_and(|extra| extra.vision.is_some());
        if incumbent {
            return Err("Already have a vision controller".to_string());
        }
        let id = self.attach_controller(
            entity,
            crate::cellapp_real_entity::Controller {
                kind: ControllerKind::Vision {
                    vision_angle,
                    vision_range,
                    seeing_height,
                    update_period,
                    ticks_since_last: 0,
                    onloaded_visible: Vec::new(),
                },
                user_arg,
            },
        );
        if id == 0 {
            return Err("controller attach failed".to_string());
        }
        Ok(id)
    }

    /// `Entity.addVisibility` (`visibility_controller.cpp:88`'s `New`):
    /// one visibility controller at a time ("Controller has already
    /// checked that entity is real" — the factory skips the real check).
    pub fn add_visibility(
        &mut self,
        entity: u32,
        visible_height: f32,
    ) -> Result<ControllerId, String> {
        let incumbent = self
            .vision_extras
            .get(&entity)
            .is_some_and(|extra| extra.visibility.is_some());
        if incumbent {
            return Err(
                "Entity.addVisibilityController: Entity already has a visibility controller"
                    .to_string(),
            );
        }
        let id = self.attach_controller(
            entity,
            crate::cellapp_real_entity::Controller {
                kind: ControllerKind::Visibility { visible_height },
                user_arg: 0,
            },
        );
        if id == 0 {
            return Err("controller attach failed".to_string());
        }
        Ok(id)
    }

    /// `EntityVision::canBeSeen( bool )` (`entity_vision.cpp:900`): the
    /// attribute setter — adding arms a default-height visibility
    /// controller (the reference's `new VisibilityController()`), removing
    /// cancels the current one. Idempotent by the early return.
    pub fn set_can_be_seen(&mut self, entity: u32, can_be_seen: bool) -> bool {
        let real = self
            .entity_population
            .get(&entity)
            .is_some_and(|entity_rc| entity_rc.borrow().is_real);
        if !real {
            // MF_ASSERT( entity_.isReal() ) — scripts never get this far.
            return false;
        }
        let current = self
            .vision_extras
            .get(&entity)
            .is_some_and(|extra| extra.visibility.is_some());
        if current == can_be_seen {
            return true;
        }
        if can_be_seen {
            self.add_visibility(entity, 2.0).is_ok()
        } else {
            if let Some(id) = self
                .vision_extras
                .get(&entity)
                .and_then(|extra| extra.visibility)
            {
                self.cancel_controller(entity, id);
            }
            true
        }
    }

    // -----------------------------------------------------------------------
    // Section: the start faces
    // -----------------------------------------------------------------------

    /// `ProximityController::startReal` (`proximity_controller.cpp:231`):
    /// build the trap, then either the restore's `OnloadHandler` scope
    /// (a non-initial start carrying a set — the reference allocates the
    /// set only for a nonzero count) or the plain start whose initial
    /// expansion fires the standard enters.
    pub fn start_proximity_controller(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        is_initial_start: bool,
        range: f32,
        onloaded: Vec<u32>,
    ) {
        let entity = entity_rc.borrow().id;
        let (node, space_id) = {
            let borrowed = entity_rc.borrow();
            (borrowed.range_node, borrowed.space_id)
        };
        let Some(node) = node else {
            return; // no space holds the entity — no list to trigger on
        };
        let sink = Rc::new(RefCell::new(Vec::new()));
        let user_arg = entity_rc.borrow().controllers.container[&id].user_arg;
        let list = self.range_lists.entry(space_id).or_default();
        let trigger = list.create_trigger(
            node,
            range,
            FLAG_ENTITY_TRIGGER,
            FLAG_ENTITY_TRIGGER,
            FLAG_NO_TRIGGERS,
            FLAG_NO_TRIGGERS,
            TriggerTarget::Recorder(Rc::clone(&sink)),
        );
        list.trigger_insert(trigger);
        list.entity_add_trigger(entity_rc, trigger);
        self.trap_triggers.insert(
            id,
            TrapTriggerState {
                entity,
                trigger,
                range,
                user_arg,
                sink,
            },
        );
        if !is_initial_start && !onloaded.is_empty() {
            self.reconcile_trap_onload(entity, id, onloaded);
        } else {
            // The initial expansion's enters ARE the standard enters
            // (`init()` ran with no alternate handler).
            self.dispatch_trap_events(entity, id);
        }
    }

    /// `VisionController::startReal` (`vision_controller.cpp:66`): the
    /// restore's visible set (`setVisibleEntities`), the incumbent
    /// error face, `setVision`, `registerForUpdate`, and
    /// `setVisionRange( visionAngle_, visionRange_ )` — whose
    /// constructor-insert folds the initial in-range membership.
    pub fn start_vision_controller(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        is_initial_start: bool,
    ) {
        let entity = entity_rc.borrow().id;
        let (vision_angle, vision_range, seeing_height, onloaded_visible) = {
            let borrowed = entity_rc.borrow();
            let Some(controller) = borrowed.controllers.container.get(&id) else {
                return;
            };
            match &controller.kind {
                ControllerKind::Vision {
                    vision_angle,
                    vision_range,
                    seeing_height,
                    onloaded_visible,
                    ..
                } => (
                    *vision_angle,
                    *vision_range,
                    *seeing_height,
                    onloaded_visible.clone(),
                ),
                _ => return,
            }
        };
        let _ = is_initial_start;
        // `setVisibleEntities` first, while the set is still empty.
        if !onloaded_visible.is_empty() {
            let mut restored = BTreeSet::new();
            for target in onloaded_visible {
                let found = self
                    .entity_population
                    .get(&target)
                    .is_some_and(|entity_rc| !entity_rc.borrow().destroyed);
                if found {
                    restored.insert(target);
                } else {
                    // WARNING "Could not find entity %d" + the bare-id
                    // stop callback (`onStopSeeingID`, okIfFnNull).
                    self.vision_notes
                        .push((entity, "setVisibleEntities: could not find entity"));
                    self.vision_callback(entity, target, "onStopSeeingID");
                }
            }
            self.visible_entities.insert(entity, restored);
        }
        let incumbent = self
            .vision_extras
            .get(&entity)
            .is_some_and(|extra| extra.vision.is_some());
        if incumbent {
            // ERROR_MSG "Entity %u already has vision controller. old=%d, new=%d"
            self.vision_notes
                .push((entity, "startReal: entity already has vision controller"));
        }
        let extra = self.vision_extras.entry(entity).or_default();
        extra.vision = Some(id);
        extra.vision_angle = vision_angle;
        extra.vision_range = vision_range;
        extra.seeing_height = seeing_height;
        self.register_updatable(entity, id);
        self.reset_vision_trigger(entity_rc, id, entity, vision_angle, vision_range);
    }

    /// `VisibilityController::startGhost`
    /// (`visibility_controller.cpp:88`): the extra slot takes the
    /// controller's backlink and height — the port starts ghost-domain
    /// kinds on the real start face, the passenger marker's precedent.
    pub fn visibility_start(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        visible_height: f32,
    ) {
        let entity = entity_rc.borrow().id;
        let extra = self.vision_extras.entry(entity).or_default();
        extra.visibility = Some(id);
        extra.visible_height = visible_height;
    }

    // -----------------------------------------------------------------------
    // Section: the stop faces
    // -----------------------------------------------------------------------

    /// `ProximityController::stopReal` (`proximity_controller.cpp:281`):
    /// the `NoOpHandler` rides the trigger while `setRange(0)`'s delete
    /// contracts it — the leaves drain into nothing.
    pub fn stop_proximity_controller(&mut self, entity_rc: &Rc<RefCell<Entity>>, id: ControllerId) {
        let Some(state) = self.trap_triggers.remove(&id) else {
            return;
        };
        self.trap_members.remove(&id);
        let space_id = entity_rc.borrow().space_id;
        // The reference's own note: this could just remove the trigger
        // instead of shuffling it back to range 0 — the port removes.
        if entity_rc.borrow().range_node.is_some()
            && let Some(list) = self.range_lists.get_mut(&space_id)
        {
            list.entity_del_trigger(entity_rc, state.trigger);
            list.trigger_remove(state.trigger);
        }
        let _ = std::mem::take(&mut *state.sink.borrow_mut());
    }

    /// `VisionController::stopReal` (`vision_controller.cpp:95`): the
    /// deregistration is the caller's container teardown; here the
    /// disabled trigger's destructor drops it without contracting —
    /// "the notification methods are not called when vision is
    /// cancelled" — and `setVision(NULL)`.
    pub fn stop_vision_controller(&mut self, entity_rc: &Rc<RefCell<Entity>>, _id: ControllerId) {
        let entity = entity_rc.borrow().id;
        let space_id = entity_rc.borrow().space_id;
        if entity_rc.borrow().range_node.is_some() {
            if let Some(state) = self.vision_triggers.remove(&entity) {
                if let Some(list) = self.range_lists.get_mut(&space_id) {
                    list.entity_del_trigger(entity_rc, state.trigger);
                    list.trigger_remove_without_contracting(state.trigger);
                }
                let _ = std::mem::take(&mut *state.sink.borrow_mut());
            }
        } else {
            self.vision_triggers.remove(&entity);
        }
        if let Some(extra) = self.vision_extras.get_mut(&entity) {
            extra.vision = None;
        }
    }

    /// `VisibilityController::stopGhost` (`visibility_controller.cpp:98`):
    /// the extra slot loses the backlink.
    pub fn visibility_stop(&mut self, entity_rc: &Rc<RefCell<Entity>>, id: ControllerId) {
        let entity = entity_rc.borrow().id;
        if let Some(extra) = self.vision_extras.get_mut(&entity)
            && extra.visibility == Some(id)
        {
            extra.visibility = None;
        }
    }

    // -----------------------------------------------------------------------
    // Section: the crossing dispatch
    // -----------------------------------------------------------------------

    /// The port's pump for the sinks: every trap drain and every
    /// vision-membership fold, in controller then entity order. The
    /// reference fires these synchronously inside the shuffle; the
    /// port's recorder sinks hold them until the movement pass ends.
    pub fn dispatch_sensor_events(&mut self) {
        let traps: Vec<(ControllerId, u32)> = self
            .trap_triggers
            .iter()
            .map(|(id, state)| (*id, state.entity))
            .collect();
        for (id, entity) in traps {
            self.dispatch_trap_events(entity, id);
        }
        let seers: Vec<u32> = self.vision_triggers.keys().copied().collect();
        for entity in seers {
            self.fold_vision_membership(entity);
        }
    }

    /// `ProximityRangeTrigger::triggerEnter`/`triggerLeave`
    /// (`proximity_controller.cpp:437/487`): the destroyed-owner guard,
    /// then the standard callback pair. Answers the number of events
    /// the sink had held.
    pub fn dispatch_trap_events(&mut self, entity: u32, id: ControllerId) -> usize {
        let Some(state) = self.trap_triggers.get(&id) else {
            return 0;
        };
        let events = std::mem::take(&mut *state.sink.borrow_mut());
        let held = events.len();
        // The membership tracks the range list whether or not the
        // owner is alive to hear about it.
        for (who, entered) in &events {
            let members = self.trap_members.entry(id).or_default();
            if *entered {
                members.insert(*who);
            } else {
                members.remove(who);
            }
        }
        let destroyed = self
            .entity_population
            .get(&entity)
            .is_some_and(|entity_rc| entity_rc.borrow().destroyed);
        if destroyed {
            // "Called on destroyed entity" — silent on both faces.
            return held;
        }
        for (who, entered) in events {
            if entered {
                self.trap_callback(entity, who, "onEnterTrap", false, id);
            } else {
                self.trap_callback(entity, who, "onLeaveTrap", true, id);
            }
        }
        held
    }

    /// The restore's `OnloadHandler` scope
    /// (`proximity_controller.cpp:173`): the sweep's enters match the
    /// set silently, strangers take the standard enter, and the
    /// destructor leaves every unmatched member — with the live
    /// entity's `standardLeave` or, for a stranger gone from the cell,
    /// `nonstandardLeave`'s bare-id callback.
    fn reconcile_trap_onload(&mut self, entity: u32, id: ControllerId, onloaded: Vec<u32>) {
        let events = self
            .trap_triggers
            .get(&id)
            .map(|state| std::mem::take(&mut *state.sink.borrow_mut()))
            .unwrap_or_default();
        // The offload lost the runtime ledger: the restore rebuilds it
        // from the streamed set, not from the pre-offload state.
        self.trap_members
            .insert(id, onloaded.iter().copied().collect());
        let mut matched = vec![false; onloaded.len()];
        for (who, entered) in &events {
            debug_assert!(*entered, "the restore sweep only expands");
            self.trap_members.entry(id).or_default().insert(*who);
            match onloaded.iter().position(|member| member == who) {
                Some(index) => matched[index] = true,
                None => self.trap_callback(entity, *who, "onEnterTrap", false, id),
            }
        }
        for (index, seen) in matched.iter().enumerate() {
            if *seen {
                continue;
            }
            let member = onloaded[index];
            self.trap_members.entry(id).or_default().remove(&member);
            let alive = self
                .entity_population
                .get(&member)
                .is_some_and(|entity_rc| !entity_rc.borrow().destroyed);
            if alive {
                self.trap_callback(entity, member, "onLeaveTrap", true, id);
            } else {
                self.trap_callback(entity, member, "onLeaveTrapID", true, id);
            }
        }
    }

    /// `EntityVision::triggerVisionEnter`/`triggerVisionLeave`
    /// (`entity_vision.cpp:463/527`): the in-range membership fold —
    /// membership only, no script.
    fn fold_vision_membership(&mut self, entity: u32) {
        let Some(state) = self.vision_triggers.get(&entity) else {
            return;
        };
        let events = std::mem::take(&mut *state.sink.borrow_mut());
        for (who, entered) in events {
            let members = self.entities_in_vision_range.entry(entity).or_default();
            if entered {
                members.insert(who);
            } else {
                members.remove(&who);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Section: the vision pass
    // -----------------------------------------------------------------------

    /// `VisionController::update` (`vision_controller.cpp:122`): the
    /// period gate — run the pass and reset the tally when
    /// `tickSinceLast_` has reached `updatePeriod_`, otherwise tick it.
    pub fn update_vision_controller(&mut self, entity_rc: &Rc<RefCell<Entity>>, id: ControllerId) {
        let (due, seeing_height) = {
            let mut borrowed = entity_rc.borrow_mut();
            let Some(controller) = borrowed.controllers.container.get_mut(&id) else {
                return;
            };
            let ControllerKind::Vision {
                ticks_since_last,
                update_period,
                seeing_height,
                ..
            } = &mut controller.kind
            else {
                return;
            };
            // The period gate: run the pass and reset the tally when
            // `tickSinceLast_` has reached `updatePeriod_`, else tick it.
            let due = *ticks_since_last >= *update_period;
            *ticks_since_last = if due { 0 } else { *ticks_since_last + 1 };
            (due, *seeing_height)
        };
        if due {
            // `getYawOffset()` is the base controller's constant 0.
            self.update_visible_entities(entity_rc, seeing_height, 0.0);
        }
    }

    /// `EntityVision::updateVisibleEntities` (`entity_vision.cpp:258`):
    /// the occlusion ladder over the in-range membership — the
    /// `canBeSeen` gate, the two-try collide + cone check with the
    /// three-quarter-height retry, and the sorted old/new diff.
    pub fn update_visible_entities(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        seeing_height: f32,
        yaw_offset: f32,
    ) {
        let entity = entity_rc.borrow().id;
        self.fold_vision_membership(entity);
        let space_id = entity_rc.borrow().space_id;
        let (position, yaw, pitch) = {
            let borrowed = entity_rc.borrow();
            (
                borrowed.position,
                borrowed.direction[0],
                borrowed.direction[1],
            )
        };
        // `(vision_ != NULL) ? vision_->visionAngle() : 1.f` — the pass
        // only runs under a controller, so the default is the bare
        // extra's reading.
        let vision_angle = self
            .vision_extras
            .get(&entity)
            .map(|extra| extra.vision_angle)
            .unwrap_or(1.0);
        // `getDroppedPosition` + the seeing height. The ground snap
        // (`shouldDropVision_`) is the terrain lane's query; this port
        // sights from the plain position.
        let head = [position[0], position[1] + seeing_height, position[2]];
        // `Vector3::setPitchYaw` (vector3.ipp:520): x = cosP·sinY,
        // y = sin(-pitch), z = cosP·cosY.
        let sight_yaw = yaw + yaw_offset;
        let (cos_pitch, sin_pitch) = (pitch.cos(), (-pitch).sin());
        let direction = [
            cos_pitch * sight_yaw.sin(),
            sin_pitch,
            cos_pitch * sight_yaw.cos(),
        ];
        let candidates: Vec<u32> = self
            .entities_in_vision_range
            .get(&entity)
            .map(|members| members.iter().copied().collect())
            .unwrap_or_default();
        let physical = self
            .spaces
            .get(&space_id)
            .and_then(|space| space.physical());
        let space_ref = physical.as_ref().map(|backend| backend.borrow());
        let Some(chunk) = space_ref.as_ref().and_then(|physical| physical.as_chunk()) else {
            // ERROR "Delegate space type is not supported" — the set is
            // returned unchanged.
            self.vision_notes.push((
                entity,
                "updateVisibleEntities: delegate space not supported",
            ));
            return;
        };
        let mut new_visible = BTreeSet::new();
        for who in candidates {
            let Some(who_rc) = self.entity_population.get(&who) else {
                continue;
            };
            if who_rc.borrow().destroyed {
                continue;
            }
            let mut can_see = false;
            if self.vision_can_be_seen(who) {
                let who_pos = who_rc.borrow().position;
                let who_height = self
                    .vision_extras
                    .get(&who)
                    .map(|extra| extra.visible_height)
                    .unwrap_or(0.0);
                let mut target = [who_pos[0], who_pos[1] + who_height, who_pos[2]];
                for _attempt in 0..2 {
                    let dist = chunk.collide(head, target);
                    // "any obstacle will do (i.e. dist may be too big)"
                    if dist > 0.0 {
                        // Blocked: the next attempt replays the same
                        // point — the retry height only arms after an
                        // angle failure.
                        continue;
                    }
                    let to = [
                        target[0] - head[0],
                        target[1] - head[1],
                        target[2] - head[2],
                    ];
                    let len_sq = to[0] * to[0] + to[1] * to[1] + to[2] * to[2];
                    // `normalise()`'s divide-by-zero on a zero vector:
                    // every component reads NaN, and NaN < angle is
                    // false — the degenerate same-spot target is not
                    // seen.
                    let unit = if len_sq > 0.0 {
                        let len = len_sq.sqrt();
                        [to[0] / len, to[1] / len, to[2] / len]
                    } else {
                        [f32::NAN; 3]
                    };
                    let dot =
                        direction[0] * unit[0] + direction[1] * unit[1] + direction[2] * unit[2];
                    if dot.acos() < vision_angle {
                        can_see = true;
                        break;
                    }
                    // Second time through, try a point.
                    target = [who_pos[0], who_pos[1] + 3.0 / 4.0 * who_height, who_pos[2]];
                }
            }
            if can_see {
                new_visible.insert(who);
            }
        }
        // The sorted old/new diff — stops then starts, in id order
        // within each kind.
        let old = self
            .visible_entities
            .get(&entity)
            .cloned()
            .unwrap_or_default();
        let stops: Vec<u32> = old.difference(&new_visible).copied().collect();
        let starts: Vec<u32> = new_visible.difference(&old).copied().collect();
        for target in stops {
            self.vision_callback(entity, target, "onStopSeeing");
        }
        for target in starts {
            self.vision_callback(entity, target, "onStartSeeing");
        }
        self.visible_entities.insert(entity, new_visible);
    }

    /// `EntityVision::canBeSeen( Entity * )` (`entity_vision.cpp:928`):
    /// the static gate — the target must carry the extra AND its
    /// visibility controller.
    pub fn vision_can_be_seen(&self, target: u32) -> bool {
        self.vision_extras
            .get(&target)
            .is_some_and(|extra| extra.visibility.is_some())
    }

    /// `EntityVision::setVisionRange` (`entity_vision.cpp:597`), the
    /// script face: real-only, and it needs a live vision controller.
    pub fn set_vision_range(&mut self, entity: u32, vision_angle: f32, range: f32) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        if !entity_rc.borrow().is_real {
            // TypeError "This method can only be called on a real entity."
            return false;
        }
        let Some(vision_id) = self
            .vision_extras
            .get(&entity)
            .and_then(|extra| extra.vision)
        else {
            // TypeError "This method requires an entity with a vision
            // controller."
            return false;
        };
        self.reset_vision_trigger(&entity_rc, vision_id, entity, vision_angle, range);
        true
    }

    /// `VisionController::setVisionRange` (`vision_controller.cpp:216`):
    /// the controller fields first, then the three trigger cases —
    /// delete (contracting), create (the constructor's insert folds the
    /// initial membership), or resize (the shuffle fires whatever the
    /// move crosses). Every outcome folds the membership.
    fn reset_vision_trigger(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        entity: u32,
        vision_angle: f32,
        range: f32,
    ) {
        if let Some(controller) = entity_rc.borrow_mut().controllers.container.get_mut(&id)
            && let ControllerKind::Vision {
                vision_angle: current_angle,
                vision_range: current_range,
                ..
            } = &mut controller.kind
        {
            *current_angle = vision_angle;
            *current_range = range;
        }
        if let Some(extra) = self.vision_extras.get_mut(&entity) {
            extra.vision_angle = vision_angle;
            extra.vision_range = range;
        }
        let space_id = entity_rc.borrow().space_id;
        if range <= 0.0 {
            // `delete pVisionTrigger_` — never disabled on this face, so
            // the destructor removes with contracting; the leaves fold
            // the membership down.
            if let Some(state) = self.vision_triggers.remove(&entity) {
                if entity_rc.borrow().range_node.is_some()
                    && let Some(list) = self.range_lists.get_mut(&space_id)
                {
                    list.entity_del_trigger(entity_rc, state.trigger);
                    list.trigger_remove(state.trigger);
                }
                let events = std::mem::take(&mut *state.sink.borrow_mut());
                let members = self.entities_in_vision_range.entry(entity).or_default();
                for (who, entered) in events {
                    if entered {
                        members.insert(who);
                    } else {
                        members.remove(&who);
                    }
                }
            }
            return;
        }
        if let Some(state) = self.vision_triggers.get(&entity) {
            // `setRangeAndMod`: resize, re-sort the entity's trigger
            // list, fire whatever the move crossed.
            let trigger = state.trigger;
            if let Some(list) = self.range_lists.get_mut(&space_id) {
                list.trigger_set_range(trigger, range);
                list.entity_del_trigger(entity_rc, trigger);
                list.entity_add_trigger(entity_rc, trigger);
            }
            if let Some(state) = self.vision_triggers.get_mut(&entity) {
                state.range = range;
            }
        } else {
            // The `VisionRangeTrigger` constructor: insert + addTrigger —
            // the initial expansion's enters fold straight in.
            let Some(node) = entity_rc.borrow().range_node else {
                return;
            };
            let sink = Rc::new(RefCell::new(Vec::new()));
            let list = self.range_lists.entry(space_id).or_default();
            let trigger = list.create_trigger(
                node,
                range,
                FLAG_ENTITY_TRIGGER,
                FLAG_ENTITY_TRIGGER,
                FLAG_NO_TRIGGERS,
                FLAG_NO_TRIGGERS,
                TriggerTarget::Recorder(Rc::clone(&sink)),
            );
            list.trigger_insert(trigger);
            list.entity_add_trigger(entity_rc, trigger);
            self.vision_triggers.insert(
                entity,
                VisionTriggerState {
                    trigger,
                    range,
                    sink,
                },
            );
        }
        self.fold_vision_membership(entity);
    }

    // -----------------------------------------------------------------------
    // Section: the callback helpers
    // -----------------------------------------------------------------------

    /// `ProximityController::writeRealToStream`'s membership walk
    /// (`proximity_controller.cpp:306`): the reference scans the range
    /// list between the trap bounds; this port keeps the same live set
    /// in the crossing ledger (the sink itself is drained by dispatch).
    /// Sorted ascending — the restore sorts its copy anyway.
    pub fn trap_live_members(&self, id: ControllerId) -> Vec<u32> {
        self.trap_members
            .get(&id)
            .map(|members| members.iter().copied().collect())
            .unwrap_or_default()
    }

    /// `ProximityRangeTrigger::standardEnter`/`standardLeave`/
    /// `nonstandardLeave`: the ledger entry carries the payload tuple,
    /// the script call carries `(controllerID, userArg)`. Enter requires
    /// the method (`okIfFnNull` false); the leaves do not. The payload
    /// is the attach-time snapshot on the trap state — the reference
    /// reads the same immutable `range_`/`userArg_`.
    fn trap_callback(
        &mut self,
        entity: u32,
        who: u32,
        name: &'static str,
        ok_if_fn_null: bool,
        id: ControllerId,
    ) {
        let Some(state) = self.trap_triggers.get(&id) else {
            return;
        };
        let (range, user_arg) = (state.range, state.user_arg);
        self.trap_calls.push(TrapCall {
            entity,
            who,
            name,
            range,
            controller_id: id,
            user_arg,
        });
        self.entity_callback(entity, name, ok_if_fn_null, Some((id, user_arg)));
    }

    /// `EntityVision::callback` (`entity_vision.cpp:391`): the diff
    /// callbacks pass the seen entity and tolerate a missing method.
    fn vision_callback(&mut self, entity: u32, target: u32, name: &'static str) {
        self.vision_calls.push(VisionCall {
            entity,
            target,
            name,
        });
        self.entity_callback(entity, name, true, None);
    }
}

// ---------------------------------------------------------------------------
// Section: tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_real_entity::{ADD_ENTITY_DATA_IS_ON_GROUND, EntityScript};
    use crate::cellapp_space::{PhysicalSpace, Space};
    use crate::cellappmgr::SpaceId;
    use std::collections::BTreeMap;

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;

    /// A script that defines every callback face the sensors fire, so
    /// the `okIfFnNull` gate never eats a call.
    fn sensor_script() -> EntityScript {
        let mut methods = std::collections::BTreeSet::new();
        methods.insert("__init__");
        for name in [
            "onEnterTrap",
            "onLeaveTrap",
            "onLeaveTrapID",
            "onStartSeeing",
            "onStopSeeing",
            "onStopSeeingID",
        ] {
            methods.insert(name);
        }
        EntityScript::with_methods(methods, BTreeMap::new())
    }

    /// The creation stream: id + type, position, the on-ground flag,
    /// direction, last event number, base address, witness byte.
    fn create_stream(id: u32, pos: [f32; 3], direction: [f32; 3]) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&TYPE.to_le_bytes());
        for value in pos {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in direction {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(b'-');
        stream
    }

    /// The seer (1) at the origin facing yaw 0.25, the stray (2) far
    /// out of every range.
    fn fixture() -> CellApp {
        let mut app = CellApp::default();
        let mut spaces = BTreeMap::new();
        spaces.insert(SID, Space::new(SID));
        app.entity_scripts.insert(TYPE, sensor_script());
        app.create_real_entity(
            &mut spaces,
            SID,
            &create_stream(1, [0.0; 3], [0.25, 0.0, 0.0]),
        );
        app.create_real_entity(
            &mut spaces,
            SID,
            &create_stream(2, [3000.0, 0.0, 0.0], [0.0; 3]),
        );
        app.spaces = spaces;
        app
    }

    /// Drops entity `id` at `pos` with no own triggers: the plain range
    /// list shuffle the movement pass would have driven.
    fn move_entity(app: &mut CellApp, id: u32, pos: [f32; 3]) {
        let entity = app.entity_population[&id].clone();
        let node = entity.borrow().range_node.expect("a ranged entity");
        let old = entity.borrow().position;
        entity.borrow_mut().position = pos;
        app.range_lists
            .get_mut(&SID)
            .expect("the space list")
            .shuffle_entity(node, &[], old[0], old[2]);
    }

    fn members_of(app: &CellApp, entity: u32) -> BTreeSet<u32> {
        app.entities_in_vision_range
            .get(&entity)
            .cloned()
            .unwrap_or_default()
    }

    fn visible_of(app: &CellApp, entity: u32) -> BTreeSet<u32> {
        app.visible_entities
            .get(&entity)
            .cloned()
            .unwrap_or_default()
    }

    fn calls_of(app: &CellApp, name: &str) -> Vec<u32> {
        app.vision_calls
            .iter()
            .filter(|call| call.name == name)
            .map(|call| call.target)
            .collect()
    }

    /// A target at (4, 0, 0) with the visibility controller on: in the
    /// origin seer's default cone, at a 77° bearing the 1.4 rad (80°)
    /// cone accepts.
    fn fixture_with_target() -> CellApp {
        let mut app = fixture();
        let mut spaces = std::mem::take(&mut app.spaces);
        app.create_real_entity(
            &mut spaces,
            SID,
            &create_stream(3, [4.0, 0.0, 0.0], [0.0; 3]),
        );
        app.spaces = spaces;
        app.add_visibility(3, 2.0).unwrap();
        app
    }

    /// The vision controller over [`fixture_with_target`], wide enough
    /// for the 77° bearing, one seeing-height-2 eye.
    fn seer_with_target() -> (CellApp, ControllerId) {
        let mut app = fixture_with_target();
        let id = app.add_vision(1, 1.4, 10.0, 2.0, 5, 0).unwrap();
        (app, id)
    }

    fn run_pass(app: &mut CellApp, entity: u32) {
        let entity_rc = app.entity_population[&entity].clone();
        app.update_visible_entities(&entity_rc, 2.0, 0.0);
    }

    #[test]
    fn a_trap_fires_enter_and_leave_as_entities_cross() {
        let mut app = fixture();
        let id = app.add_proximity(1, 5.0, 0).unwrap();
        // The start sweep found nobody in range.
        assert!(app.trap_calls.is_empty());
        // The crossing waits for the pump, then fires once with the
        // payload tuple.
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        assert!(app.trap_calls.is_empty());
        app.advance_tick();
        assert_eq!(app.trap_calls.len(), 1);
        let call = &app.trap_calls[0];
        assert_eq!((call.entity, call.who, call.name), (1, 2, "onEnterTrap"));
        assert_eq!(
            (call.range, call.controller_id, call.user_arg),
            (5.0, id, 0)
        );
        assert_eq!(
            app.script_calls
                .last()
                .map(|call| (call.entity, call.name, call.timer)),
            Some((1, "onEnterTrap", Some((id, 0))))
        );
        assert_eq!(app.trap_live_members(id), vec![2]);
        // Out again.
        move_entity(&mut app, 2, [30.0, 0.0, 30.0]);
        app.advance_tick();
        assert_eq!(app.trap_calls.len(), 2);
        assert_eq!(app.trap_calls[1].name, "onLeaveTrap");
        assert_eq!(app.trap_live_members(id), Vec::<u32>::new());
    }

    #[test]
    fn a_trap_refuses_zero_or_negative_range() {
        let mut app = fixture();
        assert_eq!(
            app.add_proximity(1, 0.0, 0).unwrap_err(),
            "Can't add proximity controller 0 or negative range"
        );
        assert_eq!(
            app.add_proximity(1, -1.0, 0).unwrap_err(),
            "Can't add proximity controller 0 or negative range"
        );
        assert!(app.trap_triggers.is_empty());
    }

    #[test]
    fn a_restored_trap_reconciles_its_onload_set() {
        let mut app = fixture();
        let id = app.add_proximity(1, 5.0, 3).unwrap();
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.dispatch_sensor_events();
        assert_eq!(app.trap_calls.len(), 1);
        // The pre-offload enter predates this restore; clear the ledger
        // so only the restore's own diff is asserted.
        app.trap_calls.clear();
        let stream = app.write_real_controllers_to_stream(1).unwrap();

        // The offload's stop tears the trap down.
        app.stop_reals_for_offload(1);
        assert!(app.trap_triggers.is_empty());
        assert!(app.read_real_controllers_from_stream(1, &stream));
        let entity_rc = app.entity_population[&1].clone();
        crate::cellapp_real_entity::start_real_controllers(&mut app, &entity_rc);
        // The sweep's enter matched the restored set — silent, and the
        // userArg survived the stream. The membership rebuilds to the
        // restored set.
        assert!(app.trap_calls.is_empty());
        assert_eq!(app.trap_live_members(id), vec![2]);
        // The restored trap keeps firing: the leave carries userArg 3.
        move_entity(&mut app, 2, [30.0, 0.0, 30.0]);
        app.dispatch_sensor_events();
        assert_eq!(app.trap_calls.len(), 1);
        assert_eq!(app.trap_calls[0].name, "onLeaveTrap");
        assert_eq!(app.trap_calls[0].user_arg, 3);
    }

    /// The restore set names ids the cell no longer agrees with: a
    /// stranger inside takes the standard enter, a member the cell
    /// knows leaves through the standard leave, and a member gone from
    /// the cell entirely leaves through the bare-id callback.
    #[test]
    fn a_restored_trap_tells_on_strangers_and_stale_ids() {
        let mut app = fixture();
        let mut spaces = std::mem::take(&mut app.spaces);
        app.create_real_entity(
            &mut spaces,
            SID,
            &create_stream(3, [1.0, 0.0, 1.0], [0.0; 3]),
        );
        app.spaces = spaces;
        // Hand-stream the restore block: one proximity controller whose
        // onload set names 2 (alive, far out) and 99 (gone).
        let mut data = Vec::new();
        data.extend_from_slice(&10u32.to_le_bytes()); // last allocated
        data.extend_from_slice(&1u32.to_le_bytes()); // controller count
        data.extend_from_slice(&20u32.to_le_bytes()); // controller id
        data.extend_from_slice(&10u32.to_le_bytes()); // type: proximity
        data.extend_from_slice(&0i32.to_le_bytes()); // userArg
        data.extend_from_slice(&5.0f32.to_le_bytes()); // range
        data.extend_from_slice(&2u32.to_le_bytes()); // onload set size
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&99u32.to_le_bytes());
        assert!(app.read_real_controllers_from_stream(1, &data));
        let entity_rc = app.entity_population[&1].clone();
        crate::cellapp_real_entity::start_real_controllers(&mut app, &entity_rc);
        let names: Vec<(&'static str, u32)> = app
            .trap_calls
            .iter()
            .map(|call| (call.name, call.who))
            .collect();
        assert_eq!(
            names,
            vec![
                ("onEnterTrap", 3),
                ("onLeaveTrap", 2),
                ("onLeaveTrapID", 99),
            ]
        );
    }

    #[test]
    fn a_vision_pass_sees_the_cone_and_respects_blockers() {
        let (mut app, _id) = seer_with_target();
        // The trigger's constructor insert folded the in-range target.
        assert_eq!(members_of(&app, 1), [3u32].into_iter().collect());
        run_pass(&mut app, 1);
        assert_eq!(visible_of(&app, 1), [3u32].into_iter().collect());
        assert_eq!(calls_of(&app, "onStartSeeing"), vec![3]);
        // A wall on the sight line: the full-height try is replayed
        // blocked on both attempts — unseen, and the drop fires.
        {
            let physical = app.spaces.get_mut(&SID).unwrap().physical().unwrap();
            let mut backend = physical.borrow_mut();
            if let PhysicalSpace::Chunk(chunk) = &mut *backend {
                chunk.blockers.push(crate::cellapp_space::Blocker {
                    center: [2.0, 2.0, 0.0],
                    radius: 0.5,
                });
            }
        }
        app.vision_calls.clear();
        run_pass(&mut app, 1);
        assert!(visible_of(&app, 1).is_empty());
        assert_eq!(calls_of(&app, "onStopSeeing"), vec![3]);
    }

    /// The retry height only arms after an angle failure: a first try
    /// blocked by a wall that only covers the full-height ray replays
    /// the same point and stays blocked, even though the
    /// three-quarter ray would have been clear.
    #[test]
    fn a_blocked_first_try_does_not_retarget_lower() {
        let (mut app, _id) = seer_with_target();
        // A tall target so the three-quarter point sits well below the
        // full-height one.
        app.vision_extras.get_mut(&3).unwrap().visible_height = 4.0;
        let target = app.entity_population[&3].clone();
        target.borrow_mut().position = [8.0, 0.0, 0.0];
        {
            let physical = app.spaces.get_mut(&SID).unwrap().physical().unwrap();
            let mut backend = physical.borrow_mut();
            if let PhysicalSpace::Chunk(chunk) = &mut *backend {
                // Covers the ray to (8, 4, 0) at its midpoint (4, 3),
                // leaves the ray to (8, 3, 0) (passing (4, 2.5)) clear.
                chunk.blockers.push(crate::cellapp_space::Blocker {
                    center: [4.0, 3.0, 0.0],
                    radius: 0.3,
                });
            }
        }
        move_entity(&mut app, 3, [8.0, 0.0, 0.0]);
        app.entities_in_vision_range
            .insert(1, [3u32].into_iter().collect());
        run_pass(&mut app, 1);
        assert!(visible_of(&app, 1).is_empty());
        assert!(app.vision_calls.is_empty());
    }

    /// The cone rejects the full-height point, the retry point at
    /// three-quarter height comes back into a slightly downward cone:
    /// the second try sees what the first refused.
    #[test]
    fn the_three_quarter_height_retry_rescues_an_angle_failure() {
        let mut app = fixture_with_target();
        let target = app.entity_population[&3].clone();
        target.borrow_mut().position = [8.0, 0.0, 0.0];
        let extra = app.vision_extras.get_mut(&3).unwrap();
        extra.visible_height = 4.0;
        // The seer faces +x, pitched down ~3.4°, cone 0.26 rad (~14.9°):
        // the full-height point sits ~17.5° above the sight line
        // (rejected), the three-quarter point ~3.7° below (accepted).
        let seer = app.entity_population[&1].clone();
        seer.borrow_mut().direction = [std::f32::consts::FRAC_PI_2, 0.06, 0.0];
        let id = app.add_vision(1, 0.26, 10.0, 2.0, 5, 0).unwrap();
        let _ = id;
        move_entity(&mut app, 3, [8.0, 0.0, 0.0]);
        app.entities_in_vision_range
            .insert(1, [3u32].into_iter().collect());
        run_pass(&mut app, 1);
        assert_eq!(visible_of(&app, 1), [3u32].into_iter().collect());
        assert_eq!(calls_of(&app, "onStartSeeing"), vec![3]);
    }

    #[test]
    fn a_target_without_visibility_is_never_seen() {
        let mut app = fixture_with_target();
        // Close the gate before the seer arms: the pass sees nothing.
        assert!(app.set_can_be_seen(3, false));
        assert_eq!(app.vision_extras[&3].visibility, None);
        app.add_vision(1, 1.4, 10.0, 2.0, 5, 0).unwrap();
        run_pass(&mut app, 1);
        assert!(visible_of(&app, 1).is_empty());
        assert!(app.vision_calls.is_empty());
    }

    /// The gate opens again through the attribute setter: the default
    /// height controller comes back, and the next pass sees.
    #[test]
    fn the_visibility_gate_reopens_and_the_height_reads_back() {
        let mut app = fixture_with_target();
        let vision_id = app.add_vision(1, 1.4, 10.0, 2.0, 5, 0).unwrap();
        let _ = vision_id;
        run_pass(&mut app, 1);
        assert_eq!(calls_of(&app, "onStartSeeing"), vec![3]);
        app.set_can_be_seen(3, false);
        app.vision_calls.clear();
        run_pass(&mut app, 1);
        assert_eq!(calls_of(&app, "onStopSeeing"), vec![3]);
        // Idempotent when already closed.
        assert!(app.set_can_be_seen(3, false));
        // Reopen: the default 2.f controller.
        assert!(app.set_can_be_seen(3, true));
        assert_eq!(app.vision_extras[&3].visible_height, 2.0);
        assert!(app.set_can_be_seen(3, true)); // still true, no double add
        app.vision_calls.clear();
        run_pass(&mut app, 1);
        assert_eq!(calls_of(&app, "onStartSeeing"), vec![3]);
    }

    #[test]
    fn a_missing_chunk_space_cannot_run_the_vision_pass() {
        let (mut app, _id) = seer_with_target();
        // The port's stand-in for the delegate arm: no space at all.
        app.spaces.remove(&SID);
        run_pass(&mut app, 1);
        assert_eq!(
            app.vision_notes,
            vec![(1, "updateVisibleEntities: delegate space not supported")]
        );
        assert!(app.vision_calls.is_empty());
        assert!(!app.visible_entities.contains_key(&1));
    }

    #[test]
    fn a_vision_controller_runs_on_its_period() {
        let mut app = fixture_with_target();
        app.add_vision(1, 1.4, 10.0, 2.0, 2, 0).unwrap();
        // tickSinceLast 0 → 1 → 2 across the first three ticks; the
        // pass runs on the third (2 >= 2), exactly the reference's
        // tally.
        app.advance_tick();
        app.advance_tick();
        assert!(app.vision_calls.is_empty());
        app.advance_tick();
        assert_eq!(calls_of(&app, "onStartSeeing"), vec![3]);
    }

    #[test]
    fn a_cancelled_vision_keeps_its_set_silent() {
        let (mut app, id) = seer_with_target();
        run_pass(&mut app, 1);
        assert_eq!(calls_of(&app, "onStartSeeing"), vec![3]);
        app.vision_calls.clear();
        assert!(app.cancel_controller(1, id));
        // The trigger is gone, the extra lost the backlink, but the
        // visible set stays as it was — no stop callbacks.
        assert!(app.vision_triggers.is_empty());
        assert_eq!(app.vision_extras[&1].vision, None);
        assert_eq!(visible_of(&app, 1), [3u32].into_iter().collect());
        app.advance_tick();
        app.advance_tick();
        assert!(app.vision_calls.is_empty());
    }

    #[test]
    fn a_vision_stream_round_trips_the_visible_set() {
        let (mut app, id) = seer_with_target();
        run_pass(&mut app, 1);
        assert_eq!(visible_of(&app, 1), [3u32].into_iter().collect());
        let stream = app.write_real_controllers_to_stream(1).unwrap();
        app.stop_reals_for_offload(1);
        assert!(app.vision_triggers.is_empty());
        assert_eq!(app.vision_extras[&1].vision, None);
        assert!(app.read_real_controllers_from_stream(1, &stream));
        let entity_rc = app.entity_population[&1].clone();
        crate::cellapp_real_entity::start_real_controllers(&mut app, &entity_rc);
        // `setVisibleEntities` restored the set before the controller
        // re-armed; the unknown-id face never fired for 3.
        assert_eq!(visible_of(&app, 1), [3u32].into_iter().collect());
        assert_eq!(app.vision_extras[&1].vision, Some(id));
        // The first pass's on-start callback predates this restore;
        // clear it so only the restore's diff is asserted.
        app.vision_calls.clear();
        assert!(app.vision_calls.is_empty());
    }

    /// The restore names a visible id the cell cannot find: the bare
    /// `onStopSeeingID` callback stands in for it.
    #[test]
    fn a_restored_visible_stranger_tells_on_stop_seeing_id() {
        let mut app = seer_with_target().0;
        let mut data = Vec::new();
        data.extend_from_slice(&10u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&30u32.to_le_bytes()); // controller id
        data.extend_from_slice(&11u32.to_le_bytes()); // type: vision
        data.extend_from_slice(&0i32.to_le_bytes()); // userArg
        data.extend_from_slice(&1.4f32.to_le_bytes()); // visionAngle
        data.extend_from_slice(&10.0f32.to_le_bytes()); // visionRange
        data.extend_from_slice(&0u32.to_le_bytes()); // tickSinceLast
        data.extend_from_slice(&5u32.to_le_bytes()); // updatePeriod
        data.extend_from_slice(&1u32.to_le_bytes()); // visible size
        data.extend_from_slice(&99u32.to_le_bytes()); // gone from the cell
        assert!(app.read_real_controllers_from_stream(1, &data));
        let entity_rc = app.entity_population[&1].clone();
        crate::cellapp_real_entity::start_real_controllers(&mut app, &entity_rc);
        assert!(
            app.vision_notes
                .contains(&(1, "setVisibleEntities: could not find entity"))
        );
        assert_eq!(calls_of(&app, "onStopSeeingID"), vec![99]);
    }

    #[test]
    fn a_vision_range_resize_moves_the_trigger() {
        let (mut app, _id) = seer_with_target();
        assert_eq!(members_of(&app, 1), [3u32].into_iter().collect());

        // Shrink past the target: the contract's leave folds the
        // membership down.
        assert!(app.set_vision_range(1, 1.2, 3.0));

        assert_eq!(app.vision_extras[&1].vision_range, 3.0);
        assert_eq!(app.vision_extras[&1].vision_angle, 1.2);
        assert!(members_of(&app, 1).is_empty());
        // Widen again: the resize's expansion folds the target back.
        assert!(app.set_vision_range(1, 1.2, 10.0));
        assert_eq!(members_of(&app, 1), [3u32].into_iter().collect());
        // Zero deletes the trigger outright — membership only, no
        // callbacks.
        assert!(app.set_vision_range(1, 1.2, 0.0));
        assert!(app.vision_triggers.is_empty());
        assert!(members_of(&app, 1).is_empty());
        assert!(app.vision_calls.is_empty());
        // The script guards: no vision controller, no resize.
        assert!(!app.set_vision_range(2, 1.0, 5.0));
        assert!(!app.set_vision_range(99, 1.0, 5.0));
    }

    #[test]
    fn a_second_vision_controller_is_refused() {
        let (mut app, id) = seer_with_target();
        assert_eq!(
            app.add_vision(1, 1.0, 5.0, 2.0, 5, 0).unwrap_err(),
            "Already have a vision controller"
        );
        assert_eq!(
            app.add_vision(1, 1.0, 0.0, 2.0, 5, 0).unwrap_err(),
            "Can't start vision system with 0 or negative range"
        );
        assert_eq!(
            app.add_vision(99, 1.0, 5.0, 2.0, 5, 0).unwrap_err(),
            "This method can only be called on a real entity."
        );
        let _ = id;
    }

    #[test]
    fn a_second_visibility_controller_is_refused() {
        let mut app = fixture();
        app.add_visibility(2, 3.5).unwrap();
        assert_eq!(
            app.add_visibility(2, 1.0).unwrap_err(),
            "Entity.addVisibilityController: Entity already has a visibility controller"
        );
        assert_eq!(app.vision_extras[&2].visible_height, 3.5);
    }

    /// The sensor starts degrade quietly on an entity no space holds:
    /// no node, no trigger, no state.
    #[test]
    fn a_sensor_on_an_unspaced_entity_stays_inert() {
        let mut app = fixture();
        // Entity 9: in the population, no range node.
        app.entity_population.insert(
            9,
            Rc::new(RefCell::new(crate::cellapp_space::Entity::new(9, true))),
        );
        let id = app.add_proximity(9, 5.0, 0).unwrap();
        assert!(app.trap_triggers.is_empty());
        let vision_id = app.add_vision(9, 1.0, 5.0, 2.0, 5, 0).unwrap();
        assert!(app.vision_triggers.is_empty());
        app.cancel_controller(9, id);
        app.cancel_controller(9, vision_id);
        assert!(app.vision_extras[&9].vision.is_none());
    }

    /// The defensive dispatch faces: a state-less id, a kind-less id
    /// and a kind-mismatched id all answer nothing.
    #[test]
    fn sensor_dispatch_ignores_bogus_controller_ids() {
        let (mut app, id) = seer_with_target();
        assert_eq!(app.dispatch_trap_events(1, 999), 0);
        let entity_rc = app.entity_population[&1].clone();
        app.update_vision_controller(&entity_rc, 999);
        app.update_visible_entities(&app.entity_population[&2].clone(), 2.0, 0.0);
        app.fold_vision_membership(2);
        // A timer controller's id dispatched as a sensor hangs inert.
        let timer_id = app.add_timer(1, 1.0, 0.0, 0);
        app.start_vision_controller(&entity_rc, timer_id, true);
        app.update_vision_controller(&entity_rc, timer_id);
        // A stale start and a post-cancel dispatch answer nothing.
        app.start_vision_controller(&entity_rc, 999, true);
        assert_eq!(app.dispatch_trap_events(1, id), 0);
        assert_eq!(app.vision_extras[&1].vision, Some(id));
        let _ = id;
    }

    /// Cancel with the entity's range node already gone (the space
    /// removed it first): the stop faces skip the list teardown.
    #[test]
    fn sensor_stops_skip_the_list_when_the_node_is_gone() {
        let (mut app, trap_id) = {
            let mut app = fixture();
            let id = app.add_proximity(1, 5.0, 0).unwrap();
            (app, id)
        };
        let vision_id = app.add_vision(1, 1.4, 10.0, 2.0, 5, 0).unwrap();
        let mut spaces = std::mem::take(&mut app.spaces);
        let entity_rc = app.entity_population[&1].clone();
        spaces
            .get_mut(&SID)
            .unwrap()
            .remove_entity(&entity_rc, &mut app);
        app.spaces = spaces;
        assert_eq!(entity_rc.borrow().range_node, None);
        assert!(app.cancel_controller(1, trap_id));
        assert!(app.cancel_controller(1, vision_id));
        assert!(app.trap_triggers.is_empty());
        assert!(app.vision_triggers.is_empty());
    }

    /// Destroyed or vanished members of the in-range set are skipped,
    /// and a zero-length sight vector degenerates to the NaN bearing
    /// the reference's divide-by-zero normalise produces.
    #[test]
    fn the_vision_pass_skips_dead_members_and_degenerate_targets() {
        let (mut app, _id) = seer_with_target();
        // Park the target on the seer's seeing point and zero its
        // height: the sight vector collapses to zero length.
        move_entity(&mut app, 3, [0.0, 2.0, 0.0]);
        app.vision_extras.get_mut(&3).unwrap().visible_height = 0.0;
        // A second member that is destroyed outright.
        let mut spaces = std::mem::take(&mut app.spaces);
        app.create_real_entity(
            &mut spaces,
            SID,
            &create_stream(4, [4.0, 0.0, 0.0], [0.0; 3]),
        );
        app.spaces = spaces;
        app.add_visibility(4, 2.0).unwrap();
        app.entity_population[&4].borrow_mut().destroyed = true;
        // Rebuild the membership by hand (the shuffle ran before both
        // edits): 3 co-located, 4 in range.
        app.entities_in_vision_range
            .insert(1, [3u32, 4].into_iter().collect());
        run_pass(&mut app, 1);
        assert!(visible_of(&app, 1).is_empty());
    }

    /// A vanished member id skips without a population lookup panic.
    #[test]
    fn the_vision_pass_skips_vanished_members() {
        let (mut app, _id) = seer_with_target();
        app.entity_population.remove(&3);
        run_pass(&mut app, 1);
        assert!(visible_of(&app, 1).is_empty());
        assert!(app.vision_calls.is_empty());
    }

    /// `canBeSeen(bool)`: ghosts and strangers refuse, a real entity
    /// arms the default-height controller, and dropping it cancels the
    /// current one.
    #[test]
    fn can_be_seen_flips_the_visibility_gate() {
        let mut app = fixture();
        assert!(!app.set_can_be_seen(99, true));
        // A ghost never gets this far in the reference (MF_ASSERT).
        app.entity_population[&2].borrow_mut().is_real = false;
        assert!(!app.set_can_be_seen(2, true));
        assert!(app.set_can_be_seen(1, true));
        assert_eq!(app.vision_extras[&1].visible_height, 2.0);
        assert!(app.vision_can_be_seen(1));
        // Idempotent.
        assert!(app.set_can_be_seen(1, true));
        // Dropping cancels the current controller.
        assert!(app.set_can_be_seen(1, false));
        assert!(app.vision_extras[&1].visibility.is_none());
        assert!(!app.vision_can_be_seen(1));
    }

    /// The trap's destroyed owner hears nothing: the dispatch folds the
    /// membership but answers the held count without a callback.
    #[test]
    fn a_destroyed_trap_owner_stays_silent() {
        let mut app = fixture();
        let id = app.add_proximity(1, 5.0, 0).unwrap();
        app.entity_population[&1].borrow_mut().destroyed = true;
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.advance_tick();
        assert!(app.trap_calls.is_empty());
        let _ = id;
    }

    /// The ghost arm of the vision-range script face.
    #[test]
    fn a_ghost_cannot_resize_its_vision() {
        let mut app = fixture();
        app.entity_population[&2].borrow_mut().is_real = false;
        assert!(!app.set_vision_range(2, 1.0, 5.0));
    }

    /// The `id == 0` attach failures: fill the controller container to
    /// its expected max, and every factory refuses.
    #[test]
    fn sensor_factories_refuse_when_the_container_is_full() {
        let mut app = fixture();
        // 25 is only the reference's warning threshold; the hard cap is
        // absolute_max_controllers (50).
        for _ in 0..50 {
            app.add_proximity(2, 1000.0, 0).unwrap();
        }
        assert_eq!(
            app.add_proximity(2, 5.0, 0).unwrap_err(),
            "controller attach failed"
        );
        assert_eq!(
            app.add_vision(2, 1.0, 5.0, 2.0, 5, 0).unwrap_err(),
            "controller attach failed"
        );
        assert_eq!(
            app.add_visibility(2, 2.0).unwrap_err(),
            "controller attach failed"
        );
    }
}
