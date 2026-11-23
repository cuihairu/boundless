//! BW-08-02-06: destroy a real entity.
//!
//! `Entity::destroy` (`entity.cpp:2896`) tears a real down in order: it fires
//! `onDestroy` (a real) or `onGhostDestroyed` (a ghost), marks the entity
//! destroyed, then deletes the controllers collection with `bw_safe_delete`
//! (`entity.cpp:2982`) — whose destructor (`controllers.cpp:29`) stops every
//! controller and disowns it — removes the entity from the space
//! (`pSpace_->removeEntity`, `entity.cpp:2988`), and deletes the entity extras
//! (`entity.cpp:2995`). `Entity::destroy` is guarded against re-entrancy by
//! `inDestroy_` (`entity.cpp:2899`) and asserts the entity was not already
//! destroyed.
//!
//! The acceptance, "实体状态和活动控制器正确清理", is that after a destroy the
//! entity is marked destroyed, every active controller and extra is cleared,
//! and the entity is no longer in its space.

use crate::controller_extra::{Controllers, EntityExtras};
use crate::real_entity::EntityId;
use crate::space::SpaceId;

/// A destroy-lifecycle event, in the order it happened
/// (`Entity::destroy`, `entity.cpp:2896`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestroyEvent {
    /// `callback("onDestroy")` (a real entity, `entity.cpp:2919`).
    OnDestroy,
    /// `callback("onGhostDestroyed")` (a ghost, `entity.cpp:2949`).
    OnGhostDestroyed,
    /// `bw_safe_delete(pControllers_)` cleared the controllers
    /// (`entity.cpp:2982`).
    ControllersCleared,
    /// The entity extras were deleted (`entity.cpp:2995`).
    ExtrasCleared,
    /// `pSpace_->removeEntity` (`entity.cpp:2988`).
    RemovedFromSpace,
}

/// An entity that can be destroyed.
#[derive(Debug, Clone)]
pub struct Entity {
    id: EntityId,
    /// `isReal()`.
    is_real: bool,
    /// `isDestroyed_`.
    destroyed: bool,
    /// `inDestroy_` re-entrancy guard (`entity.cpp:2899`).
    in_destroy: bool,
    /// `pControllers_`.
    controllers: Controllers,
    /// `extras_`.
    extras: EntityExtras,
    /// `pSpace_`.
    space_id: Option<SpaceId>,
    /// The destroy events, in order.
    events: Vec<DestroyEvent>,
}

impl Entity {
    /// An entity with its controller collection and extras.
    pub fn new(
        id: EntityId,
        is_real: bool,
        controllers: Controllers,
        extras: EntityExtras,
        space_id: SpaceId,
    ) -> Self {
        Entity {
            id,
            is_real,
            destroyed: false,
            in_destroy: false,
            controllers,
            extras,
            space_id: Some(space_id),
            events: Vec::new(),
        }
    }

    /// The entity's id.
    pub fn id(&self) -> EntityId {
        self.id
    }

    /// `Entity::isReal`.
    pub fn is_real(&self) -> bool {
        self.is_real
    }

    /// `Entity::isDestroyed`.
    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// The controller collection.
    pub fn controllers(&self) -> &Controllers {
        &self.controllers
    }

    /// The entity extras.
    pub fn extras(&self) -> &EntityExtras {
        &self.extras
    }

    /// The space the entity is in, `None` once destroyed.
    pub fn space_id(&self) -> Option<SpaceId> {
        self.space_id
    }

    /// The destroy events, in order.
    pub fn events(&self) -> &[DestroyEvent] {
        &self.events
    }

    /// `Entity::destroy` (`entity.cpp:2896`): tear the entity down and clean up
    /// its state and controllers.
    pub fn destroy(&mut self) {
        // `inDestroy_` guard; also idempotent for an already-destroyed entity
        // (the reference asserts `!isDestroyed_`, so it is called once).
        if self.in_destroy || self.destroyed {
            return;
        }
        self.in_destroy = true;

        // `onDestroy` for a real, `onGhostDestroyed` for a ghost.
        if self.is_real {
            self.events.push(DestroyEvent::OnDestroy);
        } else {
            self.events.push(DestroyEvent::OnGhostDestroyed);
        }

        self.destroyed = true;

        // `bw_safe_delete(pControllers_)` — the collection's destructor stops
        // and clears every controller (`controllers.cpp:29`).
        self.controllers.destroy();
        self.events.push(DestroyEvent::ControllersCleared);

        // The entity extras are deleted (`entity.cpp:2995`).
        self.extras.clear_all();
        self.events.push(DestroyEvent::ExtrasCleared);

        // `pSpace_->removeEntity`.
        self.space_id = None;
        self.events.push(DestroyEvent::RemovedFromSpace);

        self.in_destroy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller_extra::{Controller, ControllerTypes, Domain};

    fn make_entity(is_real: bool, num_controllers: usize, num_extras: usize) -> Entity {
        let types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        for i in 0..num_controllers {
            let mut controller = Controller::new(i as i32, "TimerController", None, Domain::REAL);
            controllers.add_controller(&mut controller, i as i32, &types);
        }
        let extras = EntityExtras::with_count(num_extras);
        Entity::new(7, is_real, controllers, extras, 3)
    }

    /// The acceptance: a destroy marks the entity destroyed, clears every
    /// controller and extra, and removes it from its space.
    #[test]
    fn destroying_a_real_cleans_up_state_and_controllers() {
        let mut entity = make_entity(true, 2, 2);
        assert!(!entity.is_destroyed());
        assert_eq!(entity.space_id(), Some(3));

        entity.destroy();

        assert!(entity.is_destroyed());
        assert_eq!(entity.controllers().len(), 0, "controllers cleared");
        assert!(entity.controllers().is_empty());
        assert_eq!(entity.extras().len(), 0, "extras cleared");
        assert_eq!(entity.space_id(), None, "removed from its space");
        assert_eq!(
            entity.events(),
            &[
                DestroyEvent::OnDestroy,
                DestroyEvent::ControllersCleared,
                DestroyEvent::ExtrasCleared,
                DestroyEvent::RemovedFromSpace,
            ]
        );
    }

    /// The stopped-controller events were recorded by the collection's own
    /// destroy (`Controllers::destroy`).
    #[test]
    fn destroying_stops_every_active_controller() {
        let mut entity = make_entity(true, 2, 0);
        entity.destroy();
        let stopped = entity
            .controllers()
            .events()
            .iter()
            .filter(|e| matches!(e, crate::controller_extra::ControllerEvent::Stopped { .. }))
            .count();
        assert_eq!(stopped, 2, "both controllers were stopped");
    }

    /// A ghost fires `onGhostDestroyed` instead of `onDestroy`.
    #[test]
    fn destroying_a_ghost_fires_on_ghost_destroyed() {
        let mut entity = make_entity(false, 1, 1);
        entity.destroy();
        assert_eq!(
            entity.events(),
            &[
                DestroyEvent::OnGhostDestroyed,
                DestroyEvent::ControllersCleared,
                DestroyEvent::ExtrasCleared,
                DestroyEvent::RemovedFromSpace,
            ]
        );
    }

    /// A destroy while already destroying (re-entrancy) is a no-op
    /// (`inDestroy_` guard).
    #[test]
    fn a_reentrant_destroy_is_a_no_op() {
        let mut entity = make_entity(true, 1, 0);
        entity.destroy();
        let events = entity.events().len();
        entity.destroy(); // already destroyed: no-op
        assert_eq!(
            entity.events().len(),
            events,
            "the second destroy did nothing"
        );
        assert!(entity.is_destroyed());
    }

    /// An entity with no controllers or extras still cleans up.
    #[test]
    fn destroying_a_bare_entity_cleans_up() {
        let mut entity = make_entity(true, 0, 0);
        entity.destroy();
        assert!(entity.is_destroyed());
        assert!(entity.controllers().is_empty());
        assert!(entity.extras().is_empty());
        assert_eq!(entity.space_id(), None);
    }
}
