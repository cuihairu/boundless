//! BW-08-06-01: the script triggers over the range list — proximity traps
//! and vision, with the seeing and occlusion callbacks.
//!
//! Reference: `cellapp/proximity_controller.*` (`ProximityController` and
//! its `ProximityRangeTrigger`), `cellapp/vision_controller.*`
//! (`VisionController` and its `VisionRangeTrigger`), `cellapp/
//! entity_vision.*` (the `EntityVision` extra: the seeing pass over
//! `entitiesInVisionRange_`, the occlusion and cone tests, and the
//! `onStartSeeing`/`onStopSeeing` diff), and `cellapp/
//! visibility_controller.*` (the ghost-domain `VisibilityController` whose
//! presence is `canBeSeen`).
//!
//! The trigger shuffles themselves live in [`crate::range_list`]; this
//! module routes the crossings those shuffles fire into the script
//! callbacks. `ChunkSpace::collide` (the occlusion query) is an injected
//! face: the distance to the first obstacle between two points, or `<= 0`
//! for a clear line.

use std::collections::{BTreeMap, BTreeSet};

use crate::range_list::{RangeListEvent, RangeListWorld, TriggerRef};

/// The script callbacks the proximity and vision faces emit
/// (`onEnterTrap`/`onLeaveTrap`/`onLeaveTrapID` on `proximity_controller.cpp:478-570`,
/// `onStartSeeing`/`onStopSeeing`/`onStopSeeingID` on `entity_vision.cpp:152-185`).
/// The reference omits `userArg` from the trap callbacks while it is 0;
/// the port always carries it.
#[derive(Clone, Debug, PartialEq)]
pub enum TrapCallback {
    /// `onEnterTrap(entityEntering, range, controllerID[, userArg])`.
    EnterTrap {
        who: u32,
        range: f32,
        controller_id: u32,
        user_arg: i32,
    },
    /// `onLeaveTrap(entityLeaving, range, controllerID[, userArg])`.
    LeaveTrap {
        who: u32,
        range: f32,
        controller_id: u32,
        user_arg: i32,
    },
    /// `onLeaveTrapID(entityID, range, controllerID[, userArg])` — the
    /// rare leave for an entity that is no longer on this app.
    LeaveTrapId {
        who: u32,
        range: f32,
        controller_id: u32,
        user_arg: i32,
    },
    /// `onStartSeeing(entityNowSeen)`.
    StartSeeing { who: u32 },
    /// `onStopSeeing(entityNoLongerSeen)`.
    StopSeeing { who: u32 },
    /// `onStopSeeingID(entityIDNoLongerSeen)` — the onload case where the
    /// seen entity is not on this cell.
    StopSeeingId { who: u32 },
}

/// `ProximityController` (`proximity_controller.hpp:20-47`): the
/// real-domain trap that fires `onEnterTrap`/`onLeaveTrap` as entities
/// cross its square range around the owning entity.
#[derive(Clone, Debug)]
pub struct ProximityController {
    /// `range_`.
    pub range: f32,
    /// `Controller::id()`.
    pub controller_id: u32,
    /// `userArg_`.
    pub user_arg: i32,
    /// `pProximityTrigger_`.
    pub trigger: Option<TriggerRef>,
    /// `pOnloadedSet_`: the trapped ids streamed over during an offload,
    /// reconciled by the destination's first non-initial `startReal`.
    pub onloaded_set: Option<Vec<u32>>,
}

/// `ProximityController::New` (`proximity_controller.cpp:136-147`): the
/// factory rejects a zero or negative range.
pub fn add_proximity(range: f32, controller_id: u32, user_arg: i32) -> Option<ProximityController> {
    if range <= 0.0 {
        return None;
    }
    Some(ProximityController {
        range,
        controller_id,
        user_arg,
        trigger: None,
        onloaded_set: None,
    })
}

/// `VisionController` (`vision_controller.hpp:17-56`): the real-domain
/// controller that keeps the entity's seeing set and re-runs the seeing
/// pass every `update_period` ticks.
#[derive(Clone, Debug)]
pub struct VisionController {
    /// `visionAngle_`: the half-angle of the vision cone in radians (the
    /// field of view is double this).
    pub vision_angle: f32,
    /// `visionRange_`.
    pub vision_range: f32,
    /// `seeingHeight_`: the height above the position to see from.
    pub seeing_height: f32,
    /// `updatePeriod_`: ticks between seeing passes.
    pub update_period: u32,
    /// `tickSinceLast_`.
    pub tick_since_last: u32,
    /// `Controller::id()`.
    pub controller_id: u32,
    /// `pVisionTrigger_`.
    pub trigger: Option<TriggerRef>,
    /// `pOnloadedVisible_`: the visible ids streamed over during an
    /// offload, seeded into the seeing set on the destination.
    pub onloaded_visible: Option<Vec<u32>>,
    /// `CellApp::registerForUpdate` — the controller updates while set.
    pub registered_for_update: bool,
    /// `VisionRangeTrigger::disabled_`: a mid-offload stop removes the
    /// trigger without contracting.
    pub trigger_disabled: bool,
}

/// `EntityVision::addVision` (`entity_vision.cpp:669-702`): the factory
/// rejects a zero or negative range.
pub fn add_vision(
    vision_angle: f32,
    vision_range: f32,
    seeing_height: f32,
    update_period: u32,
    controller_id: u32,
) -> Option<VisionController> {
    if vision_range <= 0.0 {
        return None;
    }
    Some(VisionController {
        vision_angle,
        vision_range,
        seeing_height,
        update_period,
        tick_since_last: 0,
        controller_id,
        trigger: None,
        onloaded_visible: None,
        registered_for_update: false,
        trigger_disabled: false,
    })
}

/// `EntityVision` (`entity_vision.hpp:13-126`): the per-entity seeing
/// state — who is in vision range, who is visible, and who controls the
/// extra.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityVision {
    /// `entitiesInVisionRange_`.
    pub entities_in_vision_range: BTreeSet<u32>,
    /// `visibleEntities_`.
    pub visible_entities: BTreeSet<u32>,
    /// `vision_`: the owning vision controller's id (`setVision`).
    pub vision: Option<u32>,
    /// `visibility_`: the `VisibilityController`'s visible height — its
    /// presence is `canBeSeen` (`entity_vision.cpp:896-899`).
    pub visibility: Option<f32>,
    /// `shouldDropVision_`.
    pub should_drop_vision: bool,
    /// `lastDropPosition_` (`entity_vision.cpp:198`).
    pub last_drop_position: (f32, f32, f32),
    /// `iterating_`.
    pub iterating: bool,
    /// `iterationCancelled_`.
    pub iteration_cancelled: bool,
}

impl Default for EntityVision {
    fn default() -> Self {
        EntityVision {
            entities_in_vision_range: BTreeSet::new(),
            visible_entities: BTreeSet::new(),
            vision: None,
            visibility: None,
            should_drop_vision: false,
            last_drop_position: (100000.0, 0.0, 100000.0),
            iterating: false,
            iteration_cancelled: false,
        }
    }
}

impl EntityVision {
    /// `EntityVision::isNeeded` (`entity_vision.hpp:109`).
    pub fn is_needed(&self) -> bool {
        self.visibility.is_some() || self.vision.is_some()
    }

    /// `EntityVision::setVision` (`entity_vision.cpp:813-844`): swap the
    /// owning controller; a cancelled iteration defers the outcome to the
    /// end of the seeing pass. Dropping the last controller clears the
    /// extra (`instance.clear`).
    pub fn set_vision(&mut self, vision: Option<u32>) {
        if !self.iterating {
            self.vision = vision;
            if !self.is_needed() {
                // `instance.clear( entity_ )`: the extra is destroyed and
                // the next access starts fresh.
                *self = Self::default();
                return;
            }
        } else {
            self.iteration_cancelled = true;
            self.vision = vision;
        }

        // Reset vision stuff.
        self.entities_in_vision_range.clear();
        self.should_drop_vision = false;
        if vision.is_none() {
            // `onStopSeeing` is not called for the remaining entities
            // when vision is cancelled.
            self.visible_entities.clear();
            self.last_drop_position = (100000.0, 0.0, 100000.0);
        }
    }

    /// `EntityVision::setVisibleEntities` (`entity_vision.cpp:419-448`):
    /// the onload seed. Entities still on this app go straight into the
    /// visible set; the rest get the id-only stop callback.
    pub fn set_visible_entities(
        &mut self,
        visible_entities: &[u32],
        exists: &dyn Fn(u32) -> bool,
        callbacks: &mut Vec<TrapCallback>,
    ) {
        for &id in visible_entities {
            if exists(id) {
                self.visible_entities.insert(id);
            } else {
                callbacks.push(TrapCallback::StopSeeingId { who: id });
            }
        }
    }

    /// `EntityVision::canBeSeen` static (`entity_vision.cpp:928-932`).
    pub fn entity_can_be_seen(entity: Option<&EntityVision>) -> bool {
        match entity {
            Some(ev) => ev.visibility.is_some(),
            None => false,
        }
    }

    /// `EntityVision::getDroppedPosition` (`entity_vision.cpp:939-953`):
    /// the position, or the ground position re-sampled whenever x/z moved.
    pub fn dropped_position(
        &mut self,
        position: (f32, f32, f32),
        ground: (f32, f32, f32),
    ) -> (f32, f32, f32) {
        if !self.should_drop_vision {
            return position;
        }
        let (lx, _, lz) = self.last_drop_position;
        if lx != position.0 || lz != position.2 {
            self.last_drop_position = ground;
        }
        self.last_drop_position
    }
}

/// The viewer's entity facts the seeing pass reads off the entity
/// (`entity_vision.cpp:275-283`).
#[derive(Clone, Copy, Debug)]
pub struct ViewerFacts {
    /// `entity_.position()`.
    pub position: (f32, f32, f32),
    /// `entity_.getGroundPosition()` for `shouldDropVision`.
    pub ground_position: (f32, f32, f32),
    /// `entity_.direction().yaw`.
    pub yaw: f32,
    /// `entity_.direction().pitch`.
    pub pitch: f32,
}

/// The per-target facts the seeing pass reads (`entity_vision.cpp:294-328`).
#[derive(Clone, Copy, Debug)]
pub struct TargetFacts {
    /// The target's position.
    pub position: (f32, f32, f32),
    /// The target's ground position for `shouldDropVision`.
    pub ground_position: (f32, f32, f32),
    /// `EntityVision::visibleHeight` of the target
    /// (`entity_vision.cpp:861-866`).
    pub visible_height: f32,
    /// `EntityVision::canBeSeen` of the target.
    pub can_be_seen: bool,
    /// `shouldDropVision` of the target.
    pub should_drop_vision: bool,
}

/// `Vector3::setPitchYaw` (`vector3.ipp:520-531`): the forward unit
/// vector for a pitch/yaw pair. Yaw 0 faces +Z.
pub fn forward_vector(pitch: f32, yaw: f32) -> (f32, f32, f32) {
    let cos_pitch = pitch.cos();
    (cos_pitch * yaw.sin(), (-pitch).sin(), cos_pitch * yaw.cos())
}

/// `ChunkSpace::collide` as an injected face: the distance to the first
/// obstacle between the two points, or `<= 0` for a clear line
/// (`entity_vision.cpp:303` — any obstacle will do).
pub type CollideFn<'a> = dyn Fn((f32, f32, f32), (f32, f32, f32)) -> f32 + 'a;

/// The vision half of a cell app: the range-list world plus the callback
/// sink. The crossings a shuffle fires are dispatched by the faces below,
/// in reference firing order.
pub struct VisionWorld {
    /// The cell's range list world.
    pub world: RangeListWorld,
    /// The script callbacks in reference firing order.
    pub callbacks: Vec<TrapCallback>,
}

impl Default for VisionWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl VisionWorld {
    pub fn new() -> Self {
        VisionWorld {
            world: RangeListWorld::new(),
            callbacks: Vec::new(),
        }
    }

    /// The crossings a shuffle fired for one trigger, taken out of the
    /// world log (other triggers' events keep their order).
    fn drain_trigger_events(&mut self, trigger: TriggerRef) -> Vec<(bool, u32)> {
        let mut taken = Vec::new();
        let mut kept = Vec::new();
        for event in self.world.events.drain(..) {
            match event {
                RangeListEvent::TriggerEnter {
                    trigger: t,
                    entity_id,
                } if t == trigger => taken.push((true, entity_id)),
                RangeListEvent::TriggerLeave {
                    trigger: t,
                    entity_id,
                } if t == trigger => taken.push((false, entity_id)),
                other => kept.push(other),
            }
        }
        self.world.events = kept;
        taken
    }

    // ------------------------------------------------------------------
    // ProximityController
    // ------------------------------------------------------------------

    /// `ProximityController::setRange` (`proximity_controller.cpp:384-412`):
    /// delete the trigger on a zero range, create-and-init when there is
    /// none, else `setRangeAndMod` (the range-change shuffle plus
    /// `Entity::modTrigger`). All three can fire script; the reference
    /// buffers them with `callbacksPermitted(false)` and replays after —
    /// the net order is the same.
    pub fn proximity_set_range(&mut self, trap: &mut ProximityController, around: u32, range: f32) {
        trap.range = range;
        let trigger = if range <= 0.0 {
            match trap.trigger.take() {
                Some(t) => {
                    self.world.remove_script_trigger(around, t);
                    Some(t)
                }
                None => None,
            }
        } else {
            match trap.trigger {
                None => {
                    let t = self.world.add_script_trigger(around, range);
                    trap.trigger = Some(t);
                    Some(t)
                }
                Some(t) => {
                    self.world.set_trigger_range(t, range);
                    self.world.mod_trigger(around, t);
                    Some(t)
                }
            }
        };

        if let Some(t) = trigger {
            let events = self.drain_trigger_events(t);
            let (controller_id, user_arg, range) = (trap.controller_id, trap.user_arg, trap.range);
            for (is_enter, who) in events {
                self.callbacks.push(if is_enter {
                    TrapCallback::EnterTrap {
                        who,
                        range,
                        controller_id,
                        user_arg,
                    }
                } else {
                    TrapCallback::LeaveTrap {
                        who,
                        range,
                        controller_id,
                        user_arg,
                    }
                });
            }
        }
    }

    /// `ProximityController::startReal` (`proximity_controller.cpp:234-262`):
    /// the initial start just sets the range; an onload start builds the
    /// trigger under the `OnloadHandler` — trapped ids from the stream
    /// match silently, anything else entering is a normal enter, and the
    /// unmatched ids leave at the close of the scope (`onLeaveTrap`, or
    /// `onLeaveTrapID` when the entity is no longer here).
    pub fn proximity_start_real(
        &mut self,
        trap: &mut ProximityController,
        around: u32,
        is_initial_start: bool,
    ) {
        if !is_initial_start && trap.onloaded_set.is_some() {
            let onloaded = trap.onloaded_set.take().unwrap();
            let t = self.world.add_script_trigger(around, trap.range);
            trap.trigger = Some(t);

            let mut matched = BTreeSet::new();
            let (controller_id, user_arg, range) = (trap.controller_id, trap.user_arg, trap.range);
            for (is_enter, who) in self.drain_trigger_events(t) {
                if is_enter {
                    if onloaded.contains(&who) {
                        matched.insert(who);
                    } else {
                        self.callbacks.push(TrapCallback::EnterTrap {
                            who,
                            range,
                            controller_id,
                            user_arg,
                        });
                    }
                }
                // The handler asserts isEnter: an initial expansion only
                // fires enters.
            }
            for id in onloaded {
                if matched.contains(&id) {
                    continue;
                }
                let callback = if self.world.has_entity(id) {
                    TrapCallback::LeaveTrap {
                        who: id,
                        range,
                        controller_id,
                        user_arg,
                    }
                } else {
                    TrapCallback::LeaveTrapId {
                        who: id,
                        range,
                        controller_id,
                        user_arg,
                    }
                };
                self.callbacks.push(callback);
            }
            return;
        }
        let range = trap.range;
        self.proximity_set_range(trap, around, range);
    }

    /// `ProximityController::stopReal` (`proximity_controller.cpp:281-294`):
    /// the `NoOpHandler` is attached first, so the contraction leaves fire
    /// but call nothing.
    pub fn proximity_stop_real(&mut self, trap: &mut ProximityController, around: u32) {
        if let Some(t) = trap.trigger.take() {
            self.world.remove_script_trigger(around, t);
            // Swallowed by the NoOpHandler.
            let _ = self.drain_trigger_events(t);
        }
        let range = 0.0;
        self.proximity_set_range(trap, around, range);
    }

    /// The crossings an entity movement fired, routed to the trap
    /// (`ProximityRangeTrigger::triggerEnter`/`triggerLeave`,
    /// `proximity_controller.cpp:439-461`/`495-517` — the reference fires
    /// these synchronously inside the shuffle; the port dispatches them
    /// right after the move that caused them).
    pub fn dispatch_proximity(&mut self, trap: &ProximityController) {
        let Some(t) = trap.trigger else { return };
        let events = self.drain_trigger_events(t);
        let (controller_id, user_arg, range) = (trap.controller_id, trap.user_arg, trap.range);
        for (is_enter, who) in events {
            self.callbacks.push(if is_enter {
                TrapCallback::EnterTrap {
                    who,
                    range,
                    controller_id,
                    user_arg,
                }
            } else {
                TrapCallback::LeaveTrap {
                    who,
                    range,
                    controller_id,
                    user_arg,
                }
            });
        }
    }

    /// `ProximityController::writeRealToStream` (`proximity_controller.cpp:302-348`):
    /// the handover is the range plus the trapped set.
    pub fn proximity_handover(&self, trap: &ProximityController) -> (f32, Vec<u32>) {
        let trapped = trap
            .trigger
            .map(|t| self.world.entities_between(t))
            .unwrap_or_default();
        (trap.range, trapped)
    }

    /// `ProximityController::readRealFromStream` (`proximity_controller.cpp:357-377`):
    /// the range lands immediately; the trapped set is stashed for the
    /// destination's first `startReal` (sorted, as the reference sorts).
    pub fn proximity_restore(&mut self, trap: &mut ProximityController, stream: (f32, Vec<u32>)) {
        let (range, trapped) = stream;
        trap.range = range;
        if !trapped.is_empty() {
            let mut sorted = trapped;
            sorted.sort_unstable();
            trap.onloaded_set = Some(sorted);
        }
    }

    // ------------------------------------------------------------------
    // VisionController
    // ------------------------------------------------------------------

    /// The crossings a shuffle fired, routed into the seeing range set
    /// (`VisionRangeTrigger::triggerEnter`/`triggerLeave` forwarding to
    /// `EntityVision::triggerVisionEnter`/`triggerVisionLeave`,
    /// `entity_vision.cpp:457-567` — set bookkeeping only; the seeing
    /// callbacks come from the periodic pass).
    pub fn dispatch_vision(&mut self, vision: &VisionController, ev: &mut EntityVision) {
        let Some(t) = vision.trigger else { return };
        for (is_enter, who) in self.drain_trigger_events(t) {
            if is_enter {
                ev.entities_in_vision_range.insert(who);
            } else {
                ev.entities_in_vision_range.remove(&who);
            }
        }
    }

    /// `VisionController::setVisionRange` (`vision_controller.cpp:245-272`):
    /// delete on a zero range (a disabled trigger leaves without
    /// contracting), create when there is none, else `setRangeAndMod`.
    pub fn vision_set_vision_range(
        &mut self,
        vision: &mut VisionController,
        ev: &mut EntityVision,
        around: u32,
        vision_angle: f32,
        range: f32,
    ) {
        vision.vision_angle = vision_angle;
        vision.vision_range = range;
        let subject = if range <= 0.0 {
            match vision.trigger.take() {
                Some(t) => {
                    if vision.trigger_disabled {
                        self.world.remove_trigger_without_contracting(t);
                    } else {
                        self.world.remove_script_trigger(around, t);
                    }
                    vision.trigger_disabled = false;
                    vision.vision_range = 0.0;
                    Some(t)
                }
                None => None,
            }
        } else {
            match vision.trigger {
                None => {
                    let t = self.world.add_script_trigger(around, range);
                    vision.trigger = Some(t);
                    Some(t)
                }
                Some(t) => {
                    self.world.set_trigger_range(t, range);
                    self.world.mod_trigger(around, t);
                    Some(t)
                }
            }
        };
        // The removed trigger's contraction leaves still reach the
        // seeing set (`VisionRangeTrigger::~VisionRangeTrigger` fires
        // them during `setVisionRange(…, 0)`).
        if let Some(t) = subject {
            for (is_enter, who) in self.drain_trigger_events(t) {
                if is_enter {
                    ev.entities_in_vision_range.insert(who);
                } else {
                    ev.entities_in_vision_range.remove(&who);
                }
            }
        }
    }

    /// `VisionController::startReal` (`vision_controller.cpp:95-121`):
    /// seed the onloaded visible set, take the entity's vision slot,
    /// register for update, and build the trigger.
    pub fn vision_start_real(
        &mut self,
        vision: &mut VisionController,
        ev: &mut EntityVision,
        around: u32,
    ) {
        if let Some(visible) = vision.onloaded_visible.take() {
            let exists = |id: u32| self.world.has_entity(id);
            let callbacks = &mut self.callbacks;
            ev.set_visible_entities(&visible, &exists, callbacks);
        }
        ev.set_vision(Some(vision.controller_id));
        vision.registered_for_update = true;
        let (angle, range) = (vision.vision_angle, vision.vision_range);
        self.vision_set_vision_range(vision, ev, around, angle, range);
    }

    /// `VisionController::stopReal` (`vision_controller.cpp:127-143`):
    /// a mid-offload stop disables the trigger first (its removal leaves
    /// without contracting — no leave events, the visible set rides the
    /// stream instead); a final stop contracts normally. Either way the
    /// vision slot empties and the seeing callbacks are not called.
    pub fn vision_stop_real(
        &mut self,
        vision: &mut VisionController,
        ev: &mut EntityVision,
        around: u32,
        is_final_stop: bool,
    ) {
        vision.registered_for_update = false;
        if !is_final_stop && vision.trigger.is_some() {
            vision.trigger_disabled = true;
        }
        let angle = vision.vision_angle;
        self.vision_set_vision_range(vision, ev, around, angle, 0.0);
        ev.set_vision(None);
    }

    /// `VisionController::update` (`vision_controller.cpp:158-177`): the
    /// tick gate, then the seeing pass.
    pub fn vision_update(
        &mut self,
        vision: &mut VisionController,
        ev: &mut EntityVision,
        viewer: &ViewerFacts,
        targets: &BTreeMap<u32, TargetFacts>,
        collide: &CollideFn,
    ) {
        if vision.tick_since_last >= vision.update_period {
            vision.tick_since_last = 0;
            self.update_visible_entities(vision, ev, viewer, targets, collide);
        } else {
            vision.tick_since_last += 1;
        }
    }

    /// `EntityVision::updateVisibleEntities` (`entity_vision.cpp:258-401`):
    /// re-run the seeing pass over everything in vision range and diff
    /// the visible set into `onStartSeeing`/`onStopSeeing`.
    pub fn update_visible_entities(
        &mut self,
        vision: &mut VisionController,
        ev: &mut EntityVision,
        viewer: &ViewerFacts,
        targets: &BTreeMap<u32, TargetFacts>,
        collide: &CollideFn,
    ) {
        let mut new_visible = BTreeSet::new();

        let dropped = ev.dropped_position(viewer.position, viewer.ground_position);
        let head = (dropped.0, dropped.1 + vision.seeing_height, dropped.2);
        let yaw = viewer.yaw;
        let direction = forward_vector(viewer.pitch, yaw);
        let vision_angle = vision.vision_angle;

        for &id in &ev.entities_in_vision_range {
            let Some(target) = targets.get(&id) else {
                continue;
            };

            let mut can_see = false;
            if target.can_be_seen {
                let target_dropped = if target.should_drop_vision {
                    target.ground_position
                } else {
                    target.position
                };

                // Two tries: the full visible height first, then — only
                // when that ray was clear but left the cone — a point at
                // three quarters of the height. A blocked ray keeps the
                // full-height target for its second try.
                let mut target_pos = (
                    target_dropped.0,
                    target_dropped.1 + target.visible_height,
                    target_dropped.2,
                );
                for _ in 0..2 {
                    let dist = collide(head, target_pos);
                    // If sight is blocked, move on to next entity.
                    if dist > 0.0 {
                        continue;
                    }

                    let to = (
                        target_pos.0 - head.0,
                        target_pos.1 - head.1,
                        target_pos.2 - head.2,
                    );
                    let len = (to.0 * to.0 + to.1 * to.1 + to.2 * to.2).sqrt();
                    let unit = (to.0 / len, to.1 / len, to.2 / len);
                    let dot = (direction.0 * unit.0 + direction.1 * unit.1 + direction.2 * unit.2)
                        .clamp(-1.0, 1.0);
                    let angle = dot.acos();

                    if angle < vision_angle {
                        can_see = true;
                        break;
                    }

                    // Second time through, try a point.
                    target_pos = (
                        target_dropped.0,
                        target_dropped.1 + 3.0 / 4.0 * target.visible_height,
                        target_dropped.2,
                    );
                }
            }

            if can_see {
                new_visible.insert(id);
            }
        }

        // Find the difference between the old and the new visible
        // entities (both sets iterate in id order).
        let old: Vec<u32> = ev.visible_entities.iter().copied().collect();
        let new: Vec<u32> = new_visible.iter().copied().collect();
        let mut callbacks = Vec::new();
        let mut oi = 0;
        let mut ni = 0;
        ev.iterating = true;
        while oi < old.len() && ni < new.len() {
            if old[oi] < new[ni] {
                callbacks.push(TrapCallback::StopSeeing { who: old[oi] });
                oi += 1;
            } else if old[oi] > new[ni] {
                callbacks.push(TrapCallback::StartSeeing { who: new[ni] });
                ni += 1;
            } else {
                // Still visible.
                oi += 1;
                ni += 1;
            }
        }
        while oi < old.len() {
            callbacks.push(TrapCallback::StopSeeing { who: old[oi] });
            oi += 1;
        }
        while ni < new.len() {
            callbacks.push(TrapCallback::StartSeeing { who: new[ni] });
            ni += 1;
        }
        ev.iterating = false;

        if ev.iteration_cancelled {
            // No longer controlled by the same vision controller: drop
            // the extra unless a new controller has since come along.
            ev.iteration_cancelled = false;
            if !ev.is_needed() {
                *ev = EntityVision::default();
            } else if ev.vision.is_some() {
                ev.visible_entities = new_visible;
            }
        } else {
            // Still iterating for real: swap to the new visible set.
            ev.visible_entities = new_visible;
        }

        self.callbacks.extend(callbacks);
    }

    /// `VisionController::writeRealToStream` (`vision_controller.cpp:185-201`):
    /// the handover is the angles, the tick counters, and the visible set.
    pub fn vision_handover(
        &self,
        vision: &VisionController,
        ev: &EntityVision,
    ) -> (f32, f32, u32, u32, Vec<u32>) {
        (
            vision.vision_angle,
            vision.vision_range,
            vision.tick_since_last,
            vision.update_period,
            ev.visible_entities.iter().copied().collect(),
        )
    }

    /// `VisionController::readRealFromStream` (`vision_controller.cpp:210-228`):
    /// the counters land immediately; the visible set is stashed for the
    /// destination's `startReal`.
    pub fn vision_restore(
        &mut self,
        vision: &mut VisionController,
        stream: (f32, f32, u32, u32, Vec<u32>),
    ) {
        let (angle, range, tick_since_last, update_period, visible) = stream;
        vision.vision_angle = angle;
        vision.vision_range = range;
        vision.tick_since_last = tick_since_last;
        vision.update_period = update_period;
        if !visible.is_empty() {
            vision.onloaded_visible = Some(visible);
        }
    }
}

// -----------------------------------------------------------------------------
// Section: VisibilityController
// -----------------------------------------------------------------------------

/// `VisibilityController::startGhost` (`visibility_controller.cpp:82-86`):
/// the ghost-domain controller joins the entity's `EntityVision` extra,
/// which makes the entity `canBeSeen` at this height.
pub fn visibility_start(ev: &mut EntityVision, visible_height: f32) {
    ev.visibility = Some(visible_height);
}

/// `VisibilityController::stopGhost` + `EntityVision::setVisibility(NULL)`
/// (`entity_vision.cpp:849-856`): the controller leaves; with no other
/// controller left the extra clears (outside an iteration).
pub fn visibility_stop(ev: &mut EntityVision) {
    ev.visibility = None;
    if !ev.iteration_cancelled && !ev.is_needed() {
        *ev = EntityVision::default();
    }
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The trap holder at the origin, as entity 1.
    fn trap_world() -> VisionWorld {
        let mut vw = VisionWorld::new();
        vw.world.add_entity(1, 0.0, 0.0);
        vw
    }

    /// A clear line of sight.
    fn clear_sight(_head: (f32, f32, f32), _target: (f32, f32, f32)) -> f32 {
        0.0
    }

    fn viewer_at(x: f32, z: f32, yaw: f32) -> ViewerFacts {
        ViewerFacts {
            position: (x, 0.0, z),
            ground_position: (x, 0.0, z),
            yaw,
            pitch: 0.0,
        }
    }

    fn target_at(
        id: u32,
        x: f32,
        z: f32,
        visible_height: f32,
        can_be_seen: bool,
    ) -> (u32, TargetFacts) {
        (
            id,
            TargetFacts {
                position: (x, 0.0, z),
                ground_position: (x, 0.0, z),
                visible_height,
                can_be_seen,
                should_drop_vision: false,
            },
        )
    }

    #[test]
    fn a_proximity_trap_fires_enter_and_leave_as_entities_cross() {
        let mut vw = trap_world();
        let mut trap = add_proximity(20.0, 7, 0).unwrap();
        vw.proximity_start_real(&mut trap, 1, true);
        assert!(vw.callbacks.is_empty());

        // The trap is square: within range on both axes.
        vw.world.add_entity(2, 50.0, 50.0);
        vw.world.move_entity(2, 5.0, 5.0);
        vw.dispatch_proximity(&trap);
        assert_eq!(
            vw.callbacks,
            vec![TrapCallback::EnterTrap {
                who: 2,
                range: 20.0,
                controller_id: 7,
                user_arg: 0
            }]
        );

        // Out along x but still inside z: the square loses the entity.
        vw.world.move_entity(2, 25.0, 5.0);
        vw.dispatch_proximity(&trap);
        assert_eq!(
            vw.callbacks[1..],
            [TrapCallback::LeaveTrap {
                who: 2,
                range: 20.0,
                controller_id: 7,
                user_arg: 0
            }]
        );

        // The userArg rides the callbacks when it is set.
        let mut user_trap = add_proximity(20.0, 8, 5).unwrap();
        vw.proximity_start_real(&mut user_trap, 1, true);
        vw.world.move_entity(2, 5.0, 5.0);
        vw.dispatch_proximity(&user_trap);
        assert_eq!(
            vw.callbacks[2],
            TrapCallback::EnterTrap {
                who: 2,
                range: 20.0,
                controller_id: 8,
                user_arg: 5
            }
        );
    }

    #[test]
    fn a_trap_range_change_retraps_across_the_ring() {
        let mut vw = trap_world();
        let mut trap = add_proximity(20.0, 7, 0).unwrap();
        vw.proximity_start_real(&mut trap, 1, true);
        vw.world.add_entity(2, 25.0, 5.0);

        // Widening the ring retraps the entity that just left it.
        vw.proximity_set_range(&mut trap, 1, 30.0);
        assert_eq!(
            vw.callbacks,
            vec![TrapCallback::EnterTrap {
                who: 2,
                range: 30.0,
                controller_id: 7,
                user_arg: 0
            }]
        );

        // Shrinking it releases the entity.
        vw.proximity_set_range(&mut trap, 1, 10.0);
        assert_eq!(
            vw.callbacks[1],
            TrapCallback::LeaveTrap {
                who: 2,
                range: 10.0,
                controller_id: 7,
                user_arg: 0
            }
        );

        // Widening again, then deleting the trap: the live contraction
        // still fires the leave.
        vw.proximity_set_range(&mut trap, 1, 30.0);
        vw.proximity_set_range(&mut trap, 1, 0.0);
        assert_eq!(
            vw.callbacks[2..],
            [
                TrapCallback::EnterTrap {
                    who: 2,
                    range: 30.0,
                    controller_id: 7,
                    user_arg: 0
                },
                TrapCallback::LeaveTrap {
                    who: 2,
                    range: 0.0,
                    controller_id: 7,
                    user_arg: 0
                },
            ]
        );
        assert!(trap.trigger.is_none());
        assert_eq!(trap.range, 0.0);
    }

    #[test]
    fn a_stopped_trap_swallows_its_contraction_leaves() {
        let mut vw = trap_world();
        let mut trap = add_proximity(20.0, 7, 0).unwrap();
        vw.proximity_start_real(&mut trap, 1, true);
        vw.world.add_entity(2, 5.0, 5.0);
        vw.dispatch_proximity(&trap);
        assert_eq!(vw.callbacks.len(), 1);

        // stopReal attaches the NoOpHandler: the contraction fires but
        // calls nothing.
        vw.proximity_stop_real(&mut trap, 1);
        assert!(vw.callbacks.len() == 1);
        assert!(trap.trigger.is_none());

        // A second stop has nothing left to remove.
        vw.proximity_stop_real(&mut trap, 1);
        assert!(vw.callbacks.len() == 1);

        // And with the trap gone, crossings call nothing either.
        vw.world.move_entity(2, 50.0, 50.0);
        vw.dispatch_proximity(&trap);
        assert!(vw.callbacks.len() == 1);
    }

    #[test]
    fn an_onloaded_trap_reconciles_its_trapped_set() {
        // The old cell: the trap holds entities 2 and 3.
        let mut old = trap_world();
        old.world.add_entity(2, 5.0, 5.0);
        old.world.add_entity(3, 10.0, 10.0);
        old.world.add_entity(5, 5.0, 5.0);
        let mut held = add_proximity(20.0, 7, 0).unwrap();
        old.proximity_start_real(&mut held, 1, true);
        old.dispatch_proximity(&held);
        assert_eq!(
            old.callbacks,
            vec![
                TrapCallback::EnterTrap {
                    who: 2,
                    range: 20.0,
                    controller_id: 7,
                    user_arg: 0
                },
                TrapCallback::EnterTrap {
                    who: 5,
                    range: 20.0,
                    controller_id: 7,
                    user_arg: 0
                },
                TrapCallback::EnterTrap {
                    who: 3,
                    range: 20.0,
                    controller_id: 7,
                    user_arg: 0
                },
            ]
        );

        // The offload streams the trapped set across.
        let (range, trapped) = old.proximity_handover(&held);
        assert_eq!((range, trapped.as_slice()), (20.0, &[2u32, 5, 3][..]));

        // The new cell: 2 made the jump, 3 did not; 4 strays into the
        // fresh trap, and 5 exists here but far outside it.
        let mut vw = trap_world();
        vw.world.add_entity(2, 5.0, 5.0);
        vw.world.add_entity(4, 10.0, 10.0);
        vw.world.add_entity(5, 50.0, 50.0);

        let mut trap = add_proximity(20.0, 7, 0).unwrap();
        vw.proximity_restore(&mut trap, (range, trapped));
        assert_eq!(trap.onloaded_set.as_deref(), Some(&[2u32, 3, 5][..]));

        vw.proximity_start_real(&mut trap, 1, false);

        // Entity 2 was on the stream and matched silently; entity 4
        // entered for real; entity 3 is gone — reported by id only —
        // while 5 is on this app but off the trap, so its channel
        // hears the leave.
        assert_eq!(
            vw.callbacks,
            vec![
                TrapCallback::EnterTrap {
                    who: 4,
                    range: 20.0,
                    controller_id: 7,
                    user_arg: 0
                },
                TrapCallback::LeaveTrapId {
                    who: 3,
                    range: 20.0,
                    controller_id: 7,
                    user_arg: 0
                },
                TrapCallback::LeaveTrap {
                    who: 5,
                    range: 20.0,
                    controller_id: 7,
                    user_arg: 0
                },
            ]
        );
        assert!(trap.onloaded_set.is_none());
    }

    #[test]
    fn a_vision_sees_in_range_targets_at_its_period() {
        let mut vw = trap_world();
        let mut vision = add_vision(3.0, 20.0, 2.0, 10, 5).unwrap();
        let mut ev = EntityVision::default();
        vw.vision_start_real(&mut vision, &mut ev, 1);
        assert!(vision.registered_for_update);

        // Entity 2 walks into vision range: the set updates at once, but
        // the seeing callback waits for the period.
        vw.world.add_entity(2, 0.0, 30.0);
        vw.world.move_entity(2, 0.0, 5.0);
        vw.dispatch_vision(&vision, &mut ev);
        assert_eq!(ev.entities_in_vision_range, BTreeSet::from([2]));

        let targets = BTreeMap::from([target_at(2, 0.0, 5.0, 2.0, true)]);
        let viewer = viewer_at(0.0, 0.0, 0.0);
        // The gate counts up to the period first: ten quiet calls.
        for _ in 0..10 {
            vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &clear_sight);
        }
        assert!(vw.callbacks.is_empty());

        // The pass fires.
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &clear_sight);
        assert_eq!(vw.callbacks, vec![TrapCallback::StartSeeing { who: 2 }]);
        assert_eq!(ev.visible_entities, BTreeSet::from([2]));

        // Still visible at the next pass: no duplicate callback.
        for _ in 0..11 {
            vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &clear_sight);
        }
        assert_eq!(vw.callbacks.len(), 1);

        // Widening the vision re-traps across the ring: the new rim
        // entity joins the seeing set at once (setRange + mod), while
        // the callback still waits for the pass.
        vw.world.add_entity(3, 0.0, 35.0);
        vw.world.move_entity(3, 0.0, 25.0);
        vw.dispatch_vision(&vision, &mut ev);
        assert_eq!(ev.entities_in_vision_range, BTreeSet::from([2]));
        vw.vision_set_vision_range(&mut vision, &mut ev, 1, 3.0, 30.0);
        assert_eq!(ev.entities_in_vision_range, BTreeSet::from([2, 3]));
        assert_eq!(vw.callbacks.len(), 1);
    }

    #[test]
    fn an_occluded_target_stays_unseen_until_the_line_clears() {
        let mut vw = trap_world();
        let mut vision = add_vision(3.0, 20.0, 2.0, 10, 5).unwrap();
        let mut ev = EntityVision::default();
        vw.vision_start_real(&mut vision, &mut ev, 1);
        vw.world.add_entity(2, 0.0, 30.0);
        vw.world.move_entity(2, 0.0, 5.0);
        vw.dispatch_vision(&vision, &mut ev);

        let targets = BTreeMap::from([
            target_at(2, 0.0, 5.0, 2.0, true),
            // Entity 3 is in range but has turned off canBeSeen.
            target_at(3, 1.0, 5.0, 2.0, false),
        ]);
        vw.world.add_entity(3, 1.0, 30.0);
        vw.world.move_entity(3, 1.0, 5.0);
        vw.dispatch_vision(&vision, &mut ev);
        assert_eq!(ev.entities_in_vision_range, BTreeSet::from([2, 3]));

        // A wall between: blocked on both tries, nothing is seen.
        let blocked = |_head: (f32, f32, f32), _target: (f32, f32, f32)| 9.0;
        let viewer = viewer_at(0.0, 0.0, 0.0);
        for _ in 0..11 {
            vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &blocked);
        }
        assert!(vw.callbacks.is_empty());
        assert!(ev.visible_entities.is_empty());

        // The wall falls: the next pass sees entity 2, never entity 3.
        for _ in 0..11 {
            vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &clear_sight);
        }
        assert_eq!(vw.callbacks, vec![TrapCallback::StartSeeing { who: 2 }]);
        assert_eq!(ev.visible_entities, BTreeSet::from([2]));
    }

    #[test]
    fn the_second_look_tries_three_quarters_height() {
        let mut vw = trap_world();
        let mut vision = add_vision(0.16, 20.0, 2.0, 1, 5).unwrap();
        let mut ev = EntityVision::default();
        vw.vision_start_real(&mut vision, &mut ev, 1);
        vw.world.add_entity(2, 0.0, 30.0);
        vw.world.move_entity(2, 0.0, 10.0);
        vw.dispatch_vision(&vision, &mut ev);

        // The viewer's head rides 2 up (ground + seeing height); the
        // target's base sits at the same height, so its full-height
        // point (4m up at 10m out, 0.197 rad) leaves the narrow cone
        // while the three-quarter point (3.5m, 0.148 rad) stays inside
        // it: the second look sees the entity.
        let (tid, mut target) = target_at(2, 0.0, 10.0, 2.0, true);
        target.position.1 = 2.0;
        target.ground_position.1 = 2.0;
        let targets = BTreeMap::from([(tid, target)]);
        let viewer = viewer_at(0.0, 0.0, 0.0);
        // The probe witnesses the two looks: full height first, then
        // three quarters.
        let heights = std::cell::RefCell::new(Vec::new());
        let probe = |_head: (f32, f32, f32), target: (f32, f32, f32)| {
            heights.borrow_mut().push(target.1);
            0.0
        };
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &probe);
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &probe);
        assert_eq!(heights.borrow().as_slice(), &[4.0, 3.5][..]);
        assert_eq!(vw.callbacks, vec![TrapCallback::StartSeeing { who: 2 }]);
    }

    #[test]
    fn a_vision_stop_keeps_mid_offload_quiet_and_final_stop_contracts() {
        // Mid-offload: the trigger is disabled first, so its removal
        // leaves without contracting — no leave events, and the visible
        // set rides the handover stream instead.
        let mut vw = trap_world();
        let mut vision = add_vision(3.0, 20.0, 2.0, 10, 5).unwrap();
        let mut ev = EntityVision::default();
        vw.vision_start_real(&mut vision, &mut ev, 1);
        vw.world.add_entity(2, 0.0, 30.0);
        vw.world.move_entity(2, 0.0, 5.0);
        vw.dispatch_vision(&vision, &mut ev);
        for _ in 0..11 {
            vw.vision_update(
                &mut vision,
                &mut ev,
                &viewer_at(0.0, 0.0, 0.0),
                &BTreeMap::from([target_at(2, 0.0, 5.0, 2.0, true)]),
                &clear_sight,
            );
        }
        let (_, _, _, _, visible) = vw.vision_handover(&vision, &ev);
        assert_eq!(visible, vec![2]);

        let callbacks_before_stop = vw.callbacks.len();
        vw.vision_stop_real(&mut vision, &mut ev, 1, false);
        // Cancelling vision calls no seeing callbacks.
        assert!(vw.world.events.is_empty());
        assert_eq!(vw.callbacks.len(), callbacks_before_stop);
        assert!(vision.trigger.is_none());
        assert!(!vision.registered_for_update);

        // Final stop: the trigger contracts normally and its leave is
        // dispatched into the seeing set.
        let mut vw2 = trap_world();
        let mut vision2 = add_vision(3.0, 20.0, 2.0, 10, 5).unwrap();
        let mut ev2 = EntityVision::default();
        vw2.vision_start_real(&mut vision2, &mut ev2, 1);
        vw2.world.add_entity(2, 0.0, 30.0);
        vw2.world.move_entity(2, 0.0, 5.0);
        vw2.dispatch_vision(&vision2, &mut ev2);

        vw2.vision_stop_real(&mut vision2, &mut ev2, 1, true);
        // The contraction fired its leave and the stop dispatched it:
        // the seeing set drained the event straight away.
        assert!(vw2.world.events.is_empty());
        assert!(ev2.entities_in_vision_range.is_empty());
        assert!(ev2.visible_entities.is_empty());
        assert!(vw2.callbacks.is_empty());

        // Stopping again with no trigger left is a no-op.
        vw2.vision_stop_real(&mut vision2, &mut ev2, 1, true);
        assert!(vw2.callbacks.is_empty());
    }

    #[test]
    fn an_onloaded_visible_set_reconciles_with_ids_for_the_lost() {
        let mut vw = trap_world();
        vw.world.add_entity(2, 0.0, 5.0);

        let mut vision = add_vision(3.0, 20.0, 2.0, 10, 5).unwrap();
        vw.vision_restore(&mut vision, (3.0, 20.0, 4, 10, vec![2, 3]));
        assert_eq!(vision.tick_since_last, 4);
        assert_eq!(vision.onloaded_visible.as_deref(), Some(&[2u32, 3][..]));

        let mut ev = EntityVision::default();
        vw.vision_start_real(&mut vision, &mut ev, 1);

        // Entity 2 seeds the visible set; entity 3 is gone and gets the
        // id-only callback.
        assert_eq!(ev.visible_entities, BTreeSet::from([2]));
        assert_eq!(vw.callbacks, vec![TrapCallback::StopSeeingId { who: 3 }]);
        assert!(vision.onloaded_visible.is_none());
    }

    #[test]
    fn dropping_the_last_controller_clears_the_extra() {
        let mut ev = EntityVision::default();
        visibility_start(&mut ev, 2.0);
        assert!(EntityVision::entity_can_be_seen(Some(&ev)));

        // Dropping vision while the visibility controller stays leaves
        // the extra alive — and the entity still seen.
        ev.set_vision(Some(5));
        assert!(ev.is_needed());
        ev.set_vision(None);
        assert!(ev.vision.is_none());
        assert!(ev.visible_entities.is_empty());
        assert!(ev.entities_in_vision_range.is_empty());
        assert_eq!(ev.visibility, Some(2.0));
        assert!(EntityVision::entity_can_be_seen(Some(&ev)));

        // The visibility controller leaving does the same.
        visibility_start(&mut ev, 2.0);
        visibility_stop(&mut ev);
        assert_eq!(ev, EntityVision::default());
    }

    #[test]
    fn the_factories_reject_zero_and_negative_ranges() {
        assert!(add_proximity(0.0, 7, 0).is_none());
        assert!(add_proximity(-1.0, 7, 0).is_none());
        assert!(add_vision(1.0, 0.0, 2.0, 10, 5).is_none());
        assert!(add_vision(1.0, -5.0, 2.0, 10, 5).is_none());
    }

    #[test]
    fn the_seeing_diff_reports_stops_when_targets_leave() {
        let mut vw = trap_world();
        let mut vision = add_vision(3.0, 20.0, 2.0, 1, 5).unwrap();
        let mut ev = EntityVision::default();
        vw.vision_start_real(&mut vision, &mut ev, 1);

        // Entities 2 and 3 walk in; entity 4 sits in range but has no
        // facts here — the pass skips it.
        vw.world.add_entity(2, 0.0, 30.0);
        vw.world.add_entity(3, 2.0, 30.0);
        vw.world.add_entity(4, 3.0, 5.0);
        vw.world.move_entity(2, 0.0, 5.0);
        vw.world.move_entity(3, 2.0, 5.0);
        vw.dispatch_vision(&vision, &mut ev);

        let targets = BTreeMap::from([
            target_at(2, 0.0, 5.0, 2.0, true),
            target_at(3, 2.0, 5.0, 2.0, true),
        ]);
        let viewer = viewer_at(0.0, 0.0, 0.0);
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &clear_sight);
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &clear_sight);
        assert_eq!(ev.visible_entities, BTreeSet::from([2, 3]));
        assert_eq!(
            vw.callbacks,
            [
                TrapCallback::StartSeeing { who: 2 },
                TrapCallback::StartSeeing { who: 3 },
            ]
        );

        // Pass two: 2 leaves the range, 3 stays visible, 5 arrives —
        // one stop, one keep, one start.
        vw.world.move_entity(2, 0.0, 50.0);
        vw.world.add_entity(5, 1.0, 30.0);
        vw.world.move_entity(5, 1.0, 5.0);
        vw.dispatch_vision(&vision, &mut ev);
        let targets2 = BTreeMap::from([
            target_at(3, 2.0, 5.0, 2.0, true),
            target_at(5, 1.0, 5.0, 2.0, true),
        ]);
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets2, &clear_sight);
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets2, &clear_sight);
        assert_eq!(ev.visible_entities, BTreeSet::from([3, 5]));
        assert_eq!(
            vw.callbacks[2..],
            [
                TrapCallback::StopSeeing { who: 2 },
                TrapCallback::StartSeeing { who: 5 },
            ]
        );

        // Pass three: 5 leaves while 2 returns (3 never left) — the old
        // set runs ahead of the new one on the first compare.
        vw.world.move_entity(5, 1.0, 50.0);
        vw.world.move_entity(2, 0.0, 5.0);
        vw.dispatch_vision(&vision, &mut ev);
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &clear_sight);
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &clear_sight);
        assert_eq!(ev.visible_entities, BTreeSet::from([2, 3]));
        assert_eq!(
            vw.callbacks[4..],
            [
                TrapCallback::StartSeeing { who: 2 },
                TrapCallback::StopSeeing { who: 5 },
            ]
        );
    }

    #[test]
    fn an_empty_stream_stashes_nothing() {
        let mut vw = VisionWorld::default();
        assert!(vw.callbacks.is_empty());

        let mut vision = add_vision(3.0, 20.0, 2.0, 10, 5).unwrap();
        vw.vision_restore(&mut vision, (3.0, 20.0, 0, 10, vec![]));
        assert!(vision.onloaded_visible.is_none());

        let mut trap = add_proximity(20.0, 7, 0).unwrap();
        vw.proximity_restore(&mut trap, (20.0, vec![]));
        assert!(trap.onloaded_set.is_none());

        // An entity without the extra is never seen.
        assert!(!EntityVision::entity_can_be_seen(None));
    }

    #[test]
    fn a_dropped_viewer_sees_from_the_ground() {
        let mut vw = trap_world();
        let mut vision = add_vision(3.0, 20.0, 2.0, 1, 5).unwrap();
        let mut ev = EntityVision::default();
        vw.vision_start_real(&mut vision, &mut ev, 1);
        // setVision resets shouldDropVision, so the attribute is set
        // after the controller starts.
        ev.should_drop_vision = true;
        vw.world.add_entity(2, 0.0, 30.0);
        vw.world.move_entity(2, 0.0, 5.0);
        vw.dispatch_vision(&vision, &mut ev);

        // The viewer hovers at y=50 but should drop to the ground: the
        // collide face sees the dropped head height (0 + seeing 2).
        // The target drops as well: its ground sits 3 below its
        // position, so the ray probes a point over that ground.
        let (tid, mut target) = target_at(2, 0.0, 5.0, 2.0, true);
        target.should_drop_vision = true;
        target.ground_position = (0.0, -3.0, 5.0);
        let targets = BTreeMap::from([(tid, target)]);
        let viewer = ViewerFacts {
            position: (0.0, 50.0, 0.0),
            ground_position: (0.0, 0.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
        };
        let seen = std::cell::Cell::new((f32::NAN, f32::NAN));
        let probe = |head: (f32, f32, f32), target: (f32, f32, f32)| {
            seen.set((head.1, target.1));
            0.0
        };
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &probe);
        vw.vision_update(&mut vision, &mut ev, &viewer, &targets, &probe);
        assert_eq!(seen.get(), (2.0, -1.0));
        assert_eq!(ev.visible_entities, BTreeSet::from([2]));
        assert_eq!(ev.last_drop_position, (0.0, 0.0, 0.0));

        // Moving re-samples the drop; standing still reuses it.
        let viewer2 = ViewerFacts {
            position: (3.0, 50.0, 0.0),
            ground_position: (3.0, 0.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
        };
        vw.vision_update(&mut vision, &mut ev, &viewer2, &targets, &probe);
        vw.vision_update(&mut vision, &mut ev, &viewer2, &targets, &probe);
        assert_eq!(ev.last_drop_position, (3.0, 0.0, 0.0));
    }
}
