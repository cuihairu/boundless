//! BW-08-02-02: attach and clear controllers and extras.
//!
//! A controller is attached through `Entity::addController` (`entity.cpp:7184`),
//! which calls `Controllers::addController` (`controllers.cpp:274`): the
//! collection refuses to grow past `absoluteMaxControllers`, gives an
//! exclusive controller its class id and any other a fresh id
//! (`nextControllerID`, `:592`), and inserts it into the container. Inserting
//! under a key that is already present **replaces** the old controller — so
//! re-registering a movement controller stops the previous one first
//! (`delController`, `:391`) and never leaves a stale object behind. That is
//! this task's acceptance, "反复注册不会留存旧对象". `Controller::getExclusiveID`
//! (`controller.cpp:498`) hands out the class ids above
//! `EXCLUSIVE_CONTROLLER_ID_MASK` (`controller.hpp:43`, `0x7fff`).
//!
//! An extra is `EntityExtra::Instance` (`entity_extra.hpp`): it registers a
//! type index with the entity (`Entity::registerEntityExtra`, `entity.cpp:7543`)
//! and owns at most one instance per entity — `operator()` creates it on
//! first touch, `clear()` deletes it and nulls the slot, and clearing again is
//! safe. No registration or re-registration can leave a second object behind.

use std::collections::BTreeMap;

/// `ControllerID` (`controller.hpp`).
pub type ControllerId = i32;

/// `ControllerType`.
pub type ControllerType = i32;

/// The id passed to the script (`userArg`).
pub type UserArg = i32;

/// The controller domains, bit flags (`controller.hpp:30`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain(pub u8);

impl Domain {
    /// `DOMAIN_GHOST`.
    pub const GHOST: Domain = Domain(1);
    /// `DOMAIN_REAL`.
    pub const REAL: Domain = Domain(2);
    /// `DOMAIN_GHOST_AND_REAL`.
    pub const GHOST_AND_REAL: Domain = Domain(3);

    /// Whether the ghost domain bit is set.
    pub fn has_ghost(self) -> bool {
        self.0 & Domain::GHOST.0 != 0
    }

    /// Whether the real domain bit is set.
    pub fn has_real(self) -> bool {
        self.0 & Domain::REAL.0 != 0
    }
}

/// `EXCLUSIVE_CONTROLLER_ID_MASK` (`controller.hpp:43`): the exclusive class
/// ids live above this mask; ordinary ids are in `1..=mask`.
pub const EXCLUSIVE_CONTROLLER_ID_MASK: ControllerId = 0x7fff;

/// The registry of controller classes and their exclusive ids
/// (`Controller::getExclusiveID`, `controller.cpp:498`).
#[derive(Debug, Clone, Default)]
pub struct ControllerTypes {
    /// `s_exclusiveIDs`: class name -> exclusive id.
    exclusive: BTreeMap<String, ControllerId>,
}

impl ControllerTypes {
    /// An empty registry.
    pub fn new() -> Self {
        ControllerTypes::default()
    }

    /// `Controller::getExclusiveID(class, false)`: the exclusive id a class is
    /// known by, `0` when it is not an exclusive class.
    pub fn exclusive_id(&self, exclusive_class: &str) -> ControllerId {
        self.exclusive.get(exclusive_class).copied().unwrap_or(0)
    }

    /// `Controller::getExclusiveID(class, true)`: register an exclusive class,
    /// handing out the next id above the mask
    /// (`EXCLUSIVE_CONTROLLER_ID_MASK + size + 1`).
    pub fn register_exclusive(&mut self, exclusive_class: &str) -> ControllerId {
        if let Some(id) = self.exclusive.get(exclusive_class) {
            return *id;
        }
        let id = EXCLUSIVE_CONTROLLER_ID_MASK + self.exclusive.len() as ControllerId + 1;
        self.exclusive.insert(exclusive_class.to_string(), id);
        id
    }

    /// The number of registered exclusive classes.
    pub fn len(&self) -> usize {
        self.exclusive.len()
    }

    /// Whether no exclusive class has been registered.
    pub fn is_empty(&self) -> bool {
        self.exclusive.is_empty()
    }
}

/// A controller object (`Controller`, `controller.hpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct Controller {
    /// The controller's id, `0` until it is added.
    id: ControllerId,
    /// `type_`: the controller type.
    controller_type: ControllerType,
    /// `typeName_`.
    name: String,
    /// The exclusive class, when the controller is exclusive (`exclusiveID`).
    exclusive_class: Option<String>,
    /// The id the script sees.
    user_arg: UserArg,
    /// The domains the controller runs on.
    domain: Domain,
    /// Whether the controller is running (`startReal` was called).
    running: bool,
}

impl Controller {
    /// A controller of a given type and name (`Controller::Controller`).
    pub fn new(
        controller_type: ControllerType,
        name: &str,
        exclusive_class: Option<&str>,
        domain: Domain,
    ) -> Self {
        Controller {
            id: 0,
            controller_type,
            name: name.to_string(),
            exclusive_class: exclusive_class.map(str::to_string),
            user_arg: 0,
            domain,
            running: false,
        }
    }

    /// The controller's id.
    pub fn id(&self) -> ControllerId {
        self.id
    }

    /// `type_`.
    pub fn controller_type(&self) -> ControllerType {
        self.controller_type
    }

    /// `typeName_`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `userArg()`.
    pub fn user_arg(&self) -> UserArg {
        self.user_arg
    }

    /// `domain()`.
    pub fn domain(&self) -> Domain {
        self.domain
    }

    /// `exclusiveID()`: the class id, resolved through the registry
    /// (`Controller::getExclusiveID`).
    pub fn exclusive_id(&self, types: &ControllerTypes) -> ControllerId {
        match &self.exclusive_class {
            Some(class) => types.exclusive_id(class),
            None => 0,
        }
    }

    /// Whether the controller is running (`startReal` was called).
    pub fn is_running(&self) -> bool {
        self.running
    }
}

/// A controller-lifecycle event, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerEvent {
    /// `startReal(isInitialStart)`.
    Started {
        controller: ControllerId,
        initial: bool,
    },
    /// `stopReal(isFinalStop)`.
    Stopped { controller: ControllerId },
}

/// `Controllers` (`controllers.hpp`): the per-entity collection of attached
/// controllers.
#[derive(Debug, Clone)]
pub struct Controllers {
    /// `container_`: id -> controller.
    container: BTreeMap<ControllerId, Controller>,
    /// `lastAllocatedID_`.
    last_allocated: ControllerId,
    /// `CellAppConfig::absoluteMaxControllers()`.
    absolute_max: usize,
    /// The lifecycle events, in order.
    events: Vec<ControllerEvent>,
}

impl Controllers {
    /// A collection that rejects growth past `absolute_max`.
    pub fn new(absolute_max: usize) -> Self {
        Controllers {
            container: BTreeMap::new(),
            last_allocated: 0,
            absolute_max,
            events: Vec::new(),
        }
    }

    /// `Controllers::addController` (`controllers.cpp:274`): attach a
    /// controller. Re-registering under an exclusive id stops and replaces the
    /// previous one. Returns the controller id, `0` when the collection is
    /// full.
    pub fn add_controller(
        &mut self,
        controller: &mut Controller,
        user_arg: UserArg,
        types: &ControllerTypes,
    ) -> ControllerId {
        if self.container.len() >= self.absolute_max {
            return 0;
        }

        let mut id = controller.exclusive_id(types);
        if id != 0 {
            // An exclusive controller replaces the previous one: stop it first
            // (`controllers.cpp:302`).
            self.del_controller(id);
        } else {
            id = self.next_controller_id();
        }

        controller.id = id;
        controller.user_arg = user_arg;
        controller.running = false;
        if controller.domain().has_real() {
            controller.running = true;
            self.events.push(ControllerEvent::Started {
                controller: id,
                initial: true,
            });
        }
        self.container.insert(id, controller.clone());
        id
    }

    /// `Controllers::delController` (`controllers.cpp:391`): remove a
    /// controller. Returns whether it was found.
    pub fn del_controller(&mut self, id: ControllerId) -> bool {
        let Some(removed) = self.container.remove(&id) else {
            return false;
        };
        if removed.domain().has_real() {
            self.events
                .push(ControllerEvent::Stopped { controller: id });
        }
        true
    }

    /// `Controllers::nextControllerID` (`controllers.cpp:592`): the next free
    /// ordinary id, wrapping within the mask and skipping used ids and 0.
    pub fn next_controller_id(&self) -> ControllerId {
        let mut id = self.last_allocated;
        loop {
            id = (id + 1) & EXCLUSIVE_CONTROLLER_ID_MASK;
            if id != 0 && !self.container.contains_key(&id) {
                return id;
            }
        }
    }

    /// The number of attached controllers.
    pub fn len(&self) -> usize {
        self.container.len()
    }

    /// Whether no controller is attached.
    pub fn is_empty(&self) -> bool {
        self.container.is_empty()
    }

    /// The controller attached under an id.
    pub fn find(&self, id: ControllerId) -> Option<&Controller> {
        self.container.get(&id)
    }

    /// The lifecycle events, in order.
    pub fn events(&self) -> &[ControllerEvent] {
        &self.events
    }

    /// The entity's destructor stops and clears every controller.
    pub fn destroy(&mut self) {
        let ids: Vec<ControllerId> = self.container.keys().copied().collect();
        for id in ids {
            self.del_controller(id);
        }
        self.container.clear();
    }
}

impl Drop for Controllers {
    /// The collection destructor clears the controllers it owns.
    fn drop(&mut self) {
        self.destroy();
    }
}

/// The registry of entity extra types (`Entity::s_entityExtraInfo`,
/// `entity.cpp:7531`): every registration gets a stable index.
#[derive(Debug, Clone, Default)]
pub struct ExtraRegistry {
    /// The registered extra type names, by index.
    names: Vec<String>,
}

impl ExtraRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        ExtraRegistry::default()
    }

    /// `Entity::registerEntityExtra` (`entity.cpp:7543`): register an extra
    /// type, returning its index.
    pub fn register(&mut self, name: &str) -> usize {
        self.names.push(name.to_string());
        self.names.len() - 1
    }

    /// The number of registered extra types.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether no extra type has been registered.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// The per-entity extra instances (`extras_`, `entity.cpp:1292`): one slot per
/// registered extra type, each holding at most one instance.
#[derive(Debug, Clone)]
pub struct EntityExtras {
    /// One slot per registered extra index; `Some(())` means an instance is
    /// attached.
    slots: Vec<Option<()>>,
}

impl EntityExtras {
    /// A set of extras with one slot per registered type (`extras_ = new
    /// EntityExtra*[size]`).
    pub fn with_count(count: usize) -> Self {
        EntityExtras {
            slots: vec![None; count],
        }
    }

    /// `EntityExtra::Instance::operator()` (`entity_extra.hpp`): touch the
    /// extra, creating the instance on first access. Returns whether it was
    /// created now.
    pub fn touch(&mut self, index: usize) -> bool {
        if let Some(slot) = self.slots.get_mut(index)
            && slot.is_none()
        {
            *slot = Some(());
            return true;
        }
        false
    }

    /// `EntityExtra::Instance::exists`: whether an instance is attached.
    pub fn exists(&self, index: usize) -> bool {
        self.slots.get(index).is_some_and(|slot| slot.is_some())
    }

    /// `EntityExtra::Instance::clear` (`entity_extra.hpp`): delete the
    /// instance and null the slot. Safe to call when there is none. Returns
    /// whether an instance was released.
    pub fn clear(&mut self, index: usize) -> bool {
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        if slot.is_some() {
            *slot = None;
            return true;
        }
        false
    }

    /// The number of instances attached.
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// Whether no instance is attached.
    pub fn is_empty(&self) -> bool {
        !self.slots.iter().any(|slot| slot.is_some())
    }

    /// Clear every instance (`extras_` freed on destroy).
    pub fn clear_all(&mut self) {
        self.slots.fill(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movement(domain: Domain) -> Controller {
        Controller::new(1, "MovementController", Some("Movement"), domain)
    }

    fn plain() -> Controller {
        Controller::new(2, "TimerController", None, Domain::REAL)
    }

    /// A fresh registry and collection for a test.
    fn setup(types: &mut ControllerTypes, controllers: &mut Controllers) {
        types.register_exclusive("Movement");
        *controllers = Controllers::new(100);
    }

    /// Registering an exclusive class hands out ids above the mask
    /// (`controller.cpp:498`).
    #[test]
    fn exclusive_classes_get_ids_above_the_mask() {
        let mut types = ControllerTypes::new();
        let movement = types.register_exclusive("Movement");
        let portal = types.register_exclusive("PortalConfig");
        assert_eq!(movement, EXCLUSIVE_CONTROLLER_ID_MASK + 1);
        assert_eq!(portal, EXCLUSIVE_CONTROLLER_ID_MASK + 2);
        // Re-registering returns the same id.
        assert_eq!(types.register_exclusive("Movement"), movement);
        assert_eq!(types.len(), 2);
    }

    /// An ordinary controller gets a fresh id within the mask.
    #[test]
    fn an_ordinary_controller_gets_a_fresh_id() {
        let types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        let mut a = plain();
        let mut b = plain();

        let a_id = controllers.add_controller(&mut a, 5, &types);
        let b_id = controllers.add_controller(&mut b, 6, &types);
        assert_eq!(a_id, 1);
        assert_eq!(b_id, 2);
        assert_eq!(controllers.len(), 2);
        assert_ne!(a_id, b_id);
        assert!(a_id > 0 && a_id <= EXCLUSIVE_CONTROLLER_ID_MASK);
    }

    /// The acceptance: re-registering a controller of the same exclusive class
    /// stops the old one and replaces it — no stale object is left behind.
    #[test]
    fn re_registering_an_exclusive_replaces_the_old() {
        let mut types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        setup(&mut types, &mut controllers);
        let exclusive = types.exclusive_id("Movement");

        let mut first = movement(Domain::REAL);
        let first_id = controllers.add_controller(&mut first, 7, &types);
        assert_eq!(first_id, exclusive);
        assert!(controllers.find(first_id).unwrap().is_running());

        let mut second = movement(Domain::REAL);
        let second_id = controllers.add_controller(&mut second, 8, &types);
        assert_eq!(second_id, exclusive, "the same exclusive id");

        // Exactly one controller remains: the old one was replaced, not kept.
        assert_eq!(controllers.len(), 1, "no stale controller lingers");
        let current = controllers.find(exclusive).unwrap();
        assert_eq!(current.user_arg(), 8, "the new controller replaced the old");
        assert!(current.is_running());
        assert_eq!(
            controllers.events(),
            &[
                ControllerEvent::Started {
                    controller: exclusive,
                    initial: true,
                },
                // The old one was stopped before the new one started.
                ControllerEvent::Stopped {
                    controller: exclusive
                },
                ControllerEvent::Started {
                    controller: exclusive,
                    initial: true,
                },
            ]
        );
    }

    /// The collection refuses to grow past its maximum
    /// (`controllers.cpp:280`).
    #[test]
    fn a_full_collection_rejects_new_controllers() {
        let types = ControllerTypes::new();
        let mut controllers = Controllers::new(2);
        let mut a = plain();
        let mut b = plain();
        let mut c = plain();

        assert_eq!(controllers.add_controller(&mut a, 1, &types), 1);
        assert_eq!(controllers.add_controller(&mut b, 2, &types), 2);
        assert_eq!(controllers.add_controller(&mut c, 3, &types), 0, "rejected");
        assert_eq!(controllers.len(), 2);
    }

    /// Deleting a controller stops it and frees the slot
    /// (`delController`).
    #[test]
    fn deleting_a_controller_stops_and_frees_it() {
        let types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        let mut a = plain();
        let id = controllers.add_controller(&mut a, 1, &types);

        assert!(controllers.del_controller(id));
        assert!(!controllers.del_controller(id), "already gone");
        assert_eq!(controllers.len(), 0);
        assert_eq!(
            controllers.events(),
            &[
                ControllerEvent::Started {
                    controller: id,
                    initial: true,
                },
                ControllerEvent::Stopped { controller: id },
            ]
        );

        // The freed id is handed out again.
        let mut b = plain();
        assert_eq!(controllers.add_controller(&mut b, 2, &types), id);
    }

    /// `next_controller_id` wraps within the mask and skips used ids.
    #[test]
    fn next_controller_id_wraps_within_the_mask() {
        let types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        // Fill the id space around the wrap by reserving ids.
        let mut held = Vec::new();
        for _ in 0..5 {
            let mut c = plain();
            let id = controllers.add_controller(&mut c, 0, &types);
            held.push(id);
        }
        assert_eq!(held, vec![1, 2, 3, 4, 5]);
        // Deleting id 1 frees it; the next id handed out is the next free one.
        controllers.del_controller(1);
        let mut c = plain();
        let id = controllers.add_controller(&mut c, 0, &types);
        assert_eq!(id, 1, "the freed id is reused");
    }

    /// The acceptance for extras: touching creates one instance, touching again
    /// does not duplicate, and clearing is safe and leaves nothing behind.
    #[test]
    fn touching_an_extra_creates_once_and_clearing_leaves_nothing() {
        let mut registry = ExtraRegistry::new();
        let extra = registry.register("Passenger");
        let mut extras = EntityExtras::with_count(registry.len());

        assert!(extras.touch(extra), "created on first touch");
        assert!(extras.exists(extra));
        assert_eq!(extras.len(), 1);
        assert!(
            !extras.touch(extra),
            "touching again does not create a second instance"
        );
        assert_eq!(extras.len(), 1, "no duplicate instance");

        assert!(extras.clear(extra), "cleared the instance");
        assert!(!extras.exists(extra));
        assert_eq!(extras.len(), 0);
        assert!(
            !extras.clear(extra),
            "clearing again is safe and releases nothing"
        );
    }

    /// Extras are independent per entity, and the registry hands out stable
    /// indices (`registerEntityExtra`).
    #[test]
    fn extras_are_stable_and_per_entity() {
        let mut registry = ExtraRegistry::new();
        let passenger = registry.register("Passenger");
        let combat = registry.register("Combat");
        assert_eq!(passenger, 0);
        assert_eq!(combat, 1);

        let mut one = EntityExtras::with_count(registry.len());
        let mut two = EntityExtras::with_count(registry.len());
        one.touch(passenger);
        one.touch(combat);
        two.touch(combat);

        assert!(one.exists(passenger) && one.exists(combat));
        assert!(!two.exists(passenger), "each entity has its own extras");
        assert!(two.exists(combat));
        assert_eq!(one.len(), 2);
        assert_eq!(two.len(), 1);
    }

    /// Destroying a controller collection stops every controller
    /// (the entity destructor).
    #[test]
    fn destroying_the_collection_stops_every_controller() {
        let mut types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        setup(&mut types, &mut controllers);
        let mut a = movement(Domain::REAL);
        let mut b = plain();
        let a_id = controllers.add_controller(&mut a, 1, &types);
        let b_id = controllers.add_controller(&mut b, 2, &types);

        controllers.destroy();
        assert_eq!(controllers.len(), 0);

        // The destruction stops every controller; the reference gives no
        // order for the stops, so compare the sets.
        let mut started: Vec<ControllerId> = controllers
            .events()
            .iter()
            .filter_map(|e| match e {
                ControllerEvent::Started { controller, .. } => Some(*controller),
                _ => None,
            })
            .collect();
        started.sort_unstable();
        let mut stopped: Vec<ControllerId> = controllers
            .events()
            .iter()
            .filter_map(|e| match e {
                ControllerEvent::Stopped { controller } => Some(*controller),
                _ => None,
            })
            .collect();
        stopped.sort_unstable();
        let mut expected = vec![a_id, b_id];
        expected.sort_unstable();
        assert_eq!(started, expected);
        assert_eq!(stopped, expected);
    }

    /// A ghost-only controller starts but never runs real on this app.
    #[test]
    fn a_ghost_domain_controller_does_not_start_real() {
        let types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        let mut g = movement(Domain::GHOST);
        let id = controllers.add_controller(&mut g, 3, &types);
        assert!(!controllers.find(id).unwrap().is_running());
        assert!(controllers.events().is_empty());
    }

    /// The controller's own accessors (`controller.hpp`): the id it was
    /// given, its type, name, user arg and domain; and the domain bits.
    #[test]
    fn a_controller_reports_its_own_identity() {
        let types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        let mut c = Controller::new(9, "Probe", Some("ProbeClass"), Domain::GHOST_AND_REAL);
        assert_eq!(c.id(), 0, "0 until added");
        let id = controllers.add_controller(&mut c, 77, &types);

        let found = controllers.find(id).unwrap();
        assert_eq!(found.id(), id);
        assert_eq!(found.controller_type(), 9);
        assert_eq!(found.name(), "Probe");
        assert_eq!(found.user_arg(), 77);
        assert_eq!(found.domain(), Domain::GHOST_AND_REAL);
        assert!(found.domain().has_ghost() && found.domain().has_real());
        assert_eq!(
            Controller::new(1, "Real", None, Domain::REAL).domain(),
            Domain::REAL
        );
        assert!(!Domain::REAL.has_ghost());
        assert!(!Domain::GHOST.has_real());
    }

    /// The length predicates on every container: empty when empty, and
    /// non-empty after a registration (`controller.hpp`,
    /// `entity_extra.hpp`).
    #[test]
    fn the_length_predicates_track_the_containers() {
        let mut types = ControllerTypes::new();
        let mut controllers = Controllers::new(100);
        let mut registry = ExtraRegistry::new();
        let mut extras = EntityExtras::with_count(1);

        assert!(types.is_empty());
        assert!(controllers.is_empty());
        assert!(registry.is_empty());
        assert!(extras.is_empty());

        types.register_exclusive("Movement");
        let mut c = plain();
        controllers.add_controller(&mut c, 0, &types);
        registry.register("Passenger");
        extras.touch(0);

        assert!(!types.is_empty());
        assert!(!controllers.is_empty());
        assert!(!registry.is_empty());
        assert!(!extras.is_empty());
        assert_eq!(types.len(), 1);
        assert_eq!(controllers.len(), 1);
        assert_eq!(registry.len(), 1);
        assert_eq!(extras.len(), 1);
    }

    /// `Instance::clear` on a slot index that has no registered type
    /// reports nothing released (`entity_extra.hpp`).
    #[test]
    fn clearing_an_unregistered_extra_slot_releases_nothing() {
        let mut extras = EntityExtras::with_count(1);
        assert!(!extras.clear(5), "out of range");
        assert!(!extras.exists(5));
    }

    /// `clear_all` releases every attached instance at once (the entity
    /// destructor's `extras_` teardown).
    #[test]
    fn clear_all_releases_every_instance() {
        let mut registry = ExtraRegistry::new();
        let a = registry.register("Passenger");
        let b = registry.register("Combat");
        let mut extras = EntityExtras::with_count(registry.len());
        extras.touch(a);
        extras.touch(b);
        assert_eq!(extras.len(), 2);

        extras.clear_all();
        assert!(extras.is_empty());
        assert!(!extras.exists(a) && !extras.exists(b));
    }
}
