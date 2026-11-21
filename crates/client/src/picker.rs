//! Entity selection and the scene binding (BW-11-02-03).
//!
//! The reference selects an entity with `EntityPicker`
//! (`client/entity_picker.cpp`), whose target is the entity the camera is
//! bound to (`pTarget_`, `entity_picker.ipp:353`). The view source is the
//! scene binding: `EntityPicker::prepareViewMatrix` uses `pSource_->matrix`
//! (`:808-830`) to place the selection frustum. The target changes go through
//! `EntityPicker::setCurrentEntity` (`:738-775`):
//!
//! 1. if `(pTarget_ == pEntity) && (isFull == isFull_) && (isHidden ==
//!    isHidden_)` → no change (`:742-747`);
//! 2. the old target gets `targetBlur()` (`:749-751`);
//! 3. `pTarget_ = pEntity`; `if (noPartial_ && !isFull) pTarget_ = NULL`
//!    (`:753-755`);
//! 4. `hiddenTime_` resets when becoming hidden (`:757-759`);
//! 5. the new target gets `targetFocus()` (`:763-765`).
//!
//! `EntityPicker::check` (`:599-645`) re-evaluates the target each frame: a
//! target that is no longer selectable is either kept — with a grace period
//! when only the line of sight is blocked (`hiddenTime_ < maxHiddenTime_`,
//! `:624-632`) — or released (`setCurrentEntity(NULL,…)`, the unbind). A
//! previously-hidden target that is selectable again is re-focused. The
//! geometry inputs (`isEntitySelectable`, `capsCheck`, the line of sight) are
//! engine-side and are passed into the model's `check`; the port captures the
//! state machine they drive.
//!
//! `lockOn`/`clear` (`:759-765`, `:728-731`) are the direct hit/unbind, and
//! `exclude` (`:689-705`) clears the target if the excluded entity is it.
//!
//! The acceptance is the hit and unbind behavior: a hit focuses the target, a
//! release blurs it, re-setting the same target is idempotent, and the hidden
//! grace period keeps a line-of-sight-blocked target before it is released.
//! Note that only the *triple*-equal case is idempotent: a change of the
//! hidden or full flag on the same entity is not, so it blurs the old target
//! and then re-focuses the same one (`:750-770`).

use crate::player::EntityId;

/// A script event the picker fires on the entity (`targetFocus` /
/// `targetBlur`), in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickEvent {
    /// `pTarget_->targetFocus()` (`entity_picker.cpp:763-765`).
    TargetFocus(EntityId),
    /// `pTarget_->targetBlur()` (`:749-751`).
    TargetBlur(EntityId),
}

/// The entity picker (`client/entity_picker.cpp`).
#[derive(Debug, Clone)]
pub struct EntityPicker {
    /// The source of the selection view (`pSource_`, the scene binding).
    source: Option<EntityId>,
    /// The currently picked entity (`pTarget_`).
    target: Option<EntityId>,
    /// Whether the target is a "full" target (`isFull_`).
    is_full: bool,
    /// Whether the target is held but the line of sight is blocked
    /// (`isHidden_`).
    is_hidden: bool,
    /// `noPartial_`: a non-full target is not kept.
    no_partial: bool,
    /// `isLocked_`.
    is_locked: bool,
    /// `hiddenTime_`: how long the target has been line-of-sight-blocked.
    hidden_time: f32,
    /// `maxHiddenTime_` (2.0s): the grace period.
    max_hidden_time: f32,
    /// The exclusion (`pExclude_`).
    exclude: Option<EntityId>,
    /// The fired events, in order.
    events: Vec<PickEvent>,
}

impl Default for EntityPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityPicker {
    /// The reference defaults: `maxHiddenTime_(2.f)` (`:60`), `noPartial_`
    /// false (`:57`).
    pub fn new() -> Self {
        Self {
            source: None,
            target: None,
            is_full: false,
            is_hidden: false,
            no_partial: false,
            is_locked: false,
            hidden_time: 0.0,
            max_hidden_time: 2.0,
            exclude: None,
            events: Vec::new(),
        }
    }

    pub fn events(&self) -> &[PickEvent] {
        &self.events
    }
    pub fn clear_events(&mut self) {
        self.events.clear();
    }
    /// `EntityPicker::pTarget()` (`entity_picker.ipp:353`).
    pub fn target(&self) -> Option<EntityId> {
        self.target
    }
    /// Whether the given entity is the current target.
    pub fn is_target(&self, entity: EntityId) -> bool {
        self.target == Some(entity)
    }
    pub fn is_full(&self) -> bool {
        self.is_full
    }
    pub fn is_hidden(&self) -> bool {
        self.is_hidden
    }
    /// `noPartialLocks(bool)` (`entity_picker.hpp:106`).
    pub fn set_no_partial(&mut self, no_partial: bool) {
        self.no_partial = no_partial;
    }
    /// `isLocked_` (`entity_picker.hpp:219`).
    pub fn is_locked(&self) -> bool {
        self.is_locked
    }

    /// The scene binding: set the view source (`pSource_`, `:808-830`).
    pub fn set_source(&mut self, source: Option<EntityId>) {
        self.source = source;
    }
    pub fn source(&self) -> Option<EntityId> {
        self.source
    }

    /// `EntityPicker::setCurrentEntity` (`:738-775`).
    fn set_current_entity(&mut self, entity: Option<EntityId>, is_full: bool, is_hidden: bool) {
        if self.target == entity && self.is_full == is_full && self.is_hidden == is_hidden {
            // `:742-747`: unchanged.
            return;
        }
        if let Some(old) = self.target {
            self.events.push(PickEvent::TargetBlur(old));
        }
        self.target = entity;
        if self.no_partial && !is_full {
            // `:753-755`: a partial target is not kept.
            self.target = None;
        }
        if is_hidden && !self.is_hidden {
            self.hidden_time = 0.0;
        }
        self.is_hidden = is_hidden;
        self.is_full = is_full;
        if let Some(new) = self.target {
            self.events.push(PickEvent::TargetFocus(new));
        }
    }

    /// `EntityPicker::lockOn` (`:759-765`).
    pub fn lock_on(&mut self, entity: Option<EntityId>) {
        self.is_locked = entity.is_some();
        self.set_current_entity(entity, self.is_locked, false);
    }

    /// `EntityPicker::clear` (`:728-731`) — the unbind.
    pub fn clear(&mut self) {
        self.lock_on(None);
    }

    /// `EntityPicker::exclude` (`:689-705`): set the exclusion; an excluded
    /// target is cleared.
    pub fn exclude(&mut self, entity: Option<EntityId>) {
        self.exclude = entity;
        if entity.is_some() && entity == self.target {
            self.clear();
        }
    }

    /// `EntityPicker::check` (`:599-645`): re-evaluate the target. `selectable`,
    /// `line_of_sight` and `is_now_full` are the engine-side results of
    /// `isEntitySelectable` and `capsCheck` (`:612-616`) — the raycast and
    /// bounding-box geometry is a deferred delta, so they come in as inputs.
    pub fn check(
        &mut self,
        delta_time: f32,
        selectable: bool,
        line_of_sight: bool,
        is_now_full: bool,
    ) {
        if self.target.is_none() {
            // `:602-604`: no target → nothing to check.
            return;
        }
        if !selectable {
            if line_of_sight {
                // `:611-632`: only the line of sight blocks it — a grace
                // period keeps the target hidden.
                if !self.is_hidden {
                    self.set_current_entity(self.target, self.is_full, true);
                }
                self.hidden_time += delta_time;
                if self.hidden_time < self.max_hidden_time {
                    return;
                }
            }
            // `:634-636`: release the target.
            self.set_current_entity(None, false, false);
        } else if self.is_hidden || self.is_full != is_now_full {
            // `:638-642`: selectable again (or the fullness changed).
            self.set_current_entity(self.target, is_now_full, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance: a hit focuses the target and an unbind blurs it, in
    /// the reference order.
    #[test]
    fn hit_focuses_and_unbind_blurs() {
        let mut picker = EntityPicker::new();
        picker.lock_on(Some(EntityId(7)));
        assert_eq!(
            picker.events(),
            &[PickEvent::TargetFocus(EntityId(7))],
            "the hit focuses the target"
        );
        assert!(picker.is_target(EntityId(7)));
        assert!(picker.is_full());

        picker.clear_events();
        picker.clear();
        assert_eq!(
            picker.events(),
            &[PickEvent::TargetBlur(EntityId(7))],
            "the unbind blurs the target"
        );
        assert_eq!(picker.target(), None);
        assert!(!picker.is_target(EntityId(7)));
    }

    /// `setCurrentEntity` is idempotent (`:742-747`): re-setting the same
    /// target fires nothing.
    #[test]
    fn setting_the_same_target_is_idempotent() {
        let mut picker = EntityPicker::new();
        picker.lock_on(Some(EntityId(7)));
        picker.clear_events();
        picker.lock_on(Some(EntityId(7)));
        assert!(picker.events().is_empty(), "unchanged → no focus/blur");
        picker.lock_on(None);
        picker.clear_events();
        picker.lock_on(None);
        assert!(picker.events().is_empty(), "unchanged clear → nothing");
    }

    /// `exclude` clears the target when the excluded entity is it
    /// (`entity_picker.cpp:699-704`).
    #[test]
    fn excluding_the_target_unbinds_it() {
        let mut picker = EntityPicker::new();
        picker.lock_on(Some(EntityId(7)));
        picker.clear_events();
        picker.exclude(Some(EntityId(7)));
        assert_eq!(
            picker.events(),
            &[PickEvent::TargetBlur(EntityId(7))],
            "excluding the target clears it"
        );
        // Excluding a different entity leaves the target alone.
        let mut picker = EntityPicker::new();
        picker.lock_on(Some(EntityId(7)));
        picker.clear_events();
        picker.exclude(Some(EntityId(9)));
        assert!(picker.is_target(EntityId(7)));
        assert!(picker.events().is_empty());
    }

    /// The hidden grace period: a line-of-sight-blocked target is kept (and
    /// marked hidden) until `maxHiddenTime_` elapses, then released
    /// (`entity_picker.cpp:611-636`).
    #[test]
    fn hidden_grace_keeps_then_releases_the_target() {
        let mut picker = EntityPicker::new();
        picker.lock_on(Some(EntityId(7)));
        picker.clear_events();
        // Not selectable, but the line of sight blocks it → hidden, kept. The
        // state changed (isHidden), so `setCurrentEntity` blurs then re-focuses
        // the same entity (`entity_picker.cpp:750-770`).
        picker.check(0.5, false, true, true);
        assert!(
            picker.is_target(EntityId(7)),
            "kept during the grace period"
        );
        assert!(picker.is_hidden(), "marked hidden");
        assert_eq!(
            picker.events(),
            &[
                PickEvent::TargetBlur(EntityId(7)),
                PickEvent::TargetFocus(EntityId(7))
            ],
            "the hidden transition blurs then re-focuses the target"
        );
        // Still under the 2s grace.
        picker.clear_events();
        picker.check(0.5, false, true, true);
        assert!(picker.is_target(EntityId(7)));
        assert!(picker.events().is_empty(), "still hidden, no re-focus");
        // Past the grace: release.
        picker.clear_events();
        picker.check(1.1, false, true, true);
        assert_eq!(picker.target(), None, "released after the grace");
        assert_eq!(picker.events(), &[PickEvent::TargetBlur(EntityId(7))]);
    }

    /// A selectable target that was hidden is re-focused
    /// (`entity_picker.cpp:638-642`).
    #[test]
    fn a_recovered_target_is_refocused() {
        let mut picker = EntityPicker::new();
        picker.lock_on(Some(EntityId(7)));
        picker.check(0.5, false, true, true); // hide it
        picker.clear_events();
        picker.check(0.1, true, false, true); // selectable again
        assert!(picker.is_target(EntityId(7)));
        assert!(!picker.is_hidden(), "no longer hidden");
        assert_eq!(
            picker.events(),
            &[
                PickEvent::TargetBlur(EntityId(7)),
                PickEvent::TargetFocus(EntityId(7))
            ],
            "the un-hide transition blurs then re-focuses the target"
        );
    }

    /// `noPartial_` drops a non-full target (`entity_picker.cpp:753-755`): a
    /// partial target (from `capsCheck` false in `check`) is not kept.
    #[test]
    fn no_partial_drops_a_non_full_target() {
        let mut picker = EntityPicker::new();
        picker.set_no_partial(true);
        picker.lock_on(Some(EntityId(7)));
        picker.clear_events();
        // `capsCheck` now returns false → a partial target.
        picker.check(0.1, true, false, false);
        assert_eq!(picker.target(), None, "a non-full target is dropped");
        assert_eq!(picker.events(), &[PickEvent::TargetBlur(EntityId(7))]);
    }

    /// `check` with no target is a no-op (`:602-604`).
    #[test]
    fn check_with_no_target_is_a_noop() {
        let mut picker = EntityPicker::new();
        picker.check(0.5, false, false, true);
        assert!(picker.events().is_empty());
        assert_eq!(picker.target(), None);
    }

    /// The scene binding: the view source (`pSource_`).
    #[test]
    fn source_is_the_scene_binding() {
        let mut picker = EntityPicker::new();
        assert_eq!(picker.source(), None);
        picker.set_source(Some(EntityId(3)));
        assert_eq!(picker.source(), Some(EntityId(3)));
    }
}
