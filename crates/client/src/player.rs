//! The Player entity switch and its script callback order (BW-11-02-01).
//!
//! The reference fires a fixed order of script callbacks when the Player
//! entity enters, leaves, or changes space:
//!
//! * `Entity::onEnterWorld` (`client/entity.cpp:423-480`): if the entity is
//!   still in a space it first runs `onLeaveWorld()` (the defensive assert
//!   at `:428-431`), then `onEnterSpace(/*transient*/ false)` (`:437`), then
//!   the script `onEnterWorld(prerequisites)` if the script declares it
//!   (`:460`) — otherwise the deprecated `enterWorld()` (`:465`) with a
//!   deprecation warning.
//! * `Entity::onLeaveWorld` (`:593-640`): script `onLeaveWorld` if declared
//!   (`:597`) else the deprecated `leaveWorld()` (`:601`); then
//!   `entityPicker().clear()` if the entity is a pick target (`:612-615`);
//!   then `resourceRefs_.clear()` and `onLeaveSpace(/*transient*/ false)`
//!   (`:619`); traps and physics are disposed.
//! * `Entity::onChangeSpace` (`:537-567`), the space change of a player
//!   already in the world: script `onLeaveSpace` (`:551`) →
//!   `onLeaveSpace(/*transient*/ true)` (`:554`) → `pSpace_ = NULL` (`:557`)
//!   → `onEnterSpace(/*transient*/ true)` (`:560`) →
//!   `entityPicker().check()` if a pick target (`:562-565`) → script
//!   `onEnterSpace` (`:567`).
//! * `ScriptApp::tick` (`client/script_app.cpp:198-214`): the personality
//!   environment callback `Personality.onChangeEnvironments(inside)` fires
//!   once when `!isPlayerOutside()` flips (`:202-212`).
//!
//! The acceptance is the **order** of those callbacks when the Player entity
//! switches: a switch is the old entity leaving the world and the new one
//! entering it, so the recorded events must match the reference order.

/// The id of a client entity (a `ScriptObjectPtr` slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId(pub u64);

/// A fired callback, in the order the reference dispatches it. Script
/// callbacks are the `pPyEntity_.callMethod` calls; the C++ hooks
/// (`onEnterSpace`/`onLeaveSpace`) and the entity picker run between them.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// `Entity.onLeaveWorld` script callback (or the deprecated `leaveWorld`).
    LeaveWorld { script: bool },
    /// `Entity.onEnterWorld` script callback (or the deprecated `enterWorld`).
    EnterWorld { script: bool },
    /// `Entity.onLeaveSpace`, with the `transient` flag the reference passes.
    LeaveSpace { transient: bool },
    /// `Entity.onEnterSpace`, with the `transient` flag.
    EnterSpace { transient: bool },
    /// `Personality.onChangeEnvironments(inside)` from `ScriptApp::tick`.
    ChangeEnvironments { inside: bool },
    /// `CameraApp::instance().entityPicker().clear()`.
    PickerClear,
    /// `CameraApp::instance().entityPicker().check()`.
    PickerCheck,
}

/// A client entity's script surface, as `pPyEntity_.hasAttribute` sees it.
#[derive(Debug, Clone, Copy)]
pub struct EntityScripts {
    /// The script declares `onEnterWorld` (else the deprecated `enterWorld`).
    pub on_enter_world: bool,
    /// The script declares `onLeaveWorld` (else the deprecated `leaveWorld`).
    pub on_leave_world: bool,
}

impl EntityScripts {
    /// A script declaring the modern callbacks.
    pub fn modern() -> Self {
        Self {
            on_enter_world: true,
            on_leave_world: true,
        }
    }
    /// A script declaring only the deprecated `enterWorld`/`leaveWorld`.
    pub fn deprecated() -> Self {
        Self {
            on_enter_world: false,
            on_leave_world: false,
        }
    }
}

/// A client-side entity, the `Entity` the reference dispatches to.
#[derive(Debug, Clone)]
pub struct ClientEntity {
    pub id: EntityId,
    pub scripts: EntityScripts,
    /// `isPlayer() || isLocalEntity()` → gets a Physics on enter (`:445-451`).
    pub is_player: bool,
    /// `isATarget()` → the entity picker clears/checks it.
    pub is_target: bool,
    /// Whether the entity is in the world (`pSpace_` is set).
    in_world: bool,
    /// The current space (`SpaceID`).
    space_id: Option<u32>,
}

impl ClientEntity {
    pub fn new(id: EntityId, scripts: EntityScripts) -> Self {
        Self {
            id,
            scripts,
            is_player: true,
            is_target: false,
            in_world: false,
            space_id: None,
        }
    }
    pub fn in_world(&self) -> bool {
        self.in_world
    }
    pub fn space_id(&self) -> Option<u32> {
        self.space_id
    }
    /// `Entity::isPlayer()` (`entity.cpp:1780`): the player is the entity the
    /// client controls.
    pub fn is_player(&self) -> bool {
        self.is_player
    }
    /// The entity pick target (`isATarget`), which drives the entity picker.
    pub fn set_target(&mut self, target: bool) {
        self.is_target = target;
    }
}

/// The client world, the entity-manager view that owns the active Player
/// entity and records the callback order.
#[derive(Debug, Clone, Default)]
pub struct ClientWorld {
    player: Option<ClientEntity>,
    events: Vec<PlayerEvent>,
}

impl ClientWorld {
    pub fn new() -> Self {
        Self::default()
    }

    /// The events fired since the last `clear_events`.
    pub fn events(&self) -> &[PlayerEvent] {
        &self.events
    }
    pub fn clear_events(&mut self) {
        self.events.clear();
    }
    pub fn player(&self) -> Option<&ClientEntity> {
        self.player.as_ref()
    }

    fn record(&mut self, event: PlayerEvent) {
        self.events.push(event);
    }

    /// `Entity::onLeaveWorld` (`entity.cpp:593-640`): the script callback (or
    /// its deprecated twin), the picker clear, the resource refs, then
    /// `onLeaveSpace(false)`.
    pub fn leave_world(&mut self, entity: &mut ClientEntity) {
        let script = entity.scripts.on_leave_world;
        self.record(PlayerEvent::LeaveWorld { script });
        if entity.is_target {
            self.record(PlayerEvent::PickerClear);
        }
        // resourceRefs_.clear() has no observable callback; recorded by the
        // `LeaveSpace` hook that follows.
        entity.in_world = false;
        entity.space_id = None;
        self.record(PlayerEvent::LeaveSpace { transient: false });
    }

    /// `Entity::onEnterWorld` (`entity.cpp:423-480`): the defensive
    /// leave-world, `onEnterSpace(false)`, then the script callback (or its
    /// deprecated twin).
    pub fn enter_world(&mut self, entity: &mut ClientEntity) {
        if entity.in_world {
            // `IF_NOT_MF_ASSERT_DEV( !pSpace_ )` (`:428-431`).
            self.leave_world(entity);
        }
        entity.in_world = true;
        self.record(PlayerEvent::EnterSpace { transient: false });
        let script = entity.scripts.on_enter_world;
        self.record(PlayerEvent::EnterWorld { script });
    }

    /// `Entity::onChangeSpace` (`entity.cpp:537-567`): the script leave,
    /// the C++ transient leave, the C++ transient enter, the picker check,
    /// then the script enter.
    pub fn change_space(&mut self, new_space: u32) {
        if let Some(mut entity) = self.player.take() {
            assert!(
                entity.in_world && entity.is_player(),
                "only a player changes space"
            );
            self.record(PlayerEvent::LeaveSpace { transient: true });
            entity.space_id = None;
            self.record(PlayerEvent::EnterSpace { transient: true });
            if entity.is_target {
                self.record(PlayerEvent::PickerCheck);
            }
            entity.space_id = Some(new_space);
            self.record(PlayerEvent::EnterSpace { transient: true });
            self.player = Some(entity);
        }
    }

    /// `ScriptApp::tick` (`script_app.cpp:198-214`): fire
    /// `Personality.onChangeEnvironments(inside)` once per change of the
    /// inside/outside flag.
    pub fn update_environment(&mut self, player_outside: bool) {
        let inside = !player_outside;
        // The reference keeps the old value in a static; here the last
        // recorded value stands in for it.
        let changed = match self.events.last() {
            Some(PlayerEvent::ChangeEnvironments { inside: last }) => *last != inside,
            _ => true,
        };
        if changed {
            self.record(PlayerEvent::ChangeEnvironments { inside });
        }
    }

    /// Switch the Player entity: the old player leaves the world and the new
    /// one enters it (`onLeaveWorld`… then `onEnterWorld`…).
    pub fn switch_player(&mut self, new_player: ClientEntity) {
        if let Some(mut old) = self.player.take() {
            self.leave_world(&mut old);
        }
        let mut new_player = new_player;
        self.enter_world(&mut new_player);
        self.player = Some(new_player);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance: a Player-entity switch fires the reference's script
    /// event order — the old entity leaves (`onLeaveWorld` → `onLeaveSpace`),
    /// then the new one enters (`onEnterSpace` → `onEnterWorld`).
    #[test]
    fn switching_the_player_entity_fires_the_reference_order() {
        let mut world = ClientWorld::new();
        world.switch_player(ClientEntity::new(EntityId(1), EntityScripts::modern()));
        world.clear_events();

        world.switch_player(ClientEntity::new(EntityId(2), EntityScripts::modern()));

        let events: Vec<_> = world.events().iter().collect();
        assert_eq!(
            events,
            vec![
                &PlayerEvent::LeaveWorld { script: true },
                &PlayerEvent::LeaveSpace { transient: false },
                &PlayerEvent::EnterSpace { transient: false },
                &PlayerEvent::EnterWorld { script: true },
            ],
            "the switch fires onLeaveWorld → onLeaveSpace → onEnterSpace → onEnterWorld"
        );
    }

    /// `onEnterWorld`/`onLeaveWorld` prefer the script methods; without them
    /// the deprecated `enterWorld`/`leaveWorld` are called instead
    /// (`entity.cpp:465,601`).
    #[test]
    fn enter_world_falls_back_to_the_deprecated_callback() {
        let mut world = ClientWorld::new();
        world.switch_player(ClientEntity::new(EntityId(1), EntityScripts::deprecated()));
        assert!(
            world
                .events()
                .iter()
                .any(|e| matches!(e, PlayerEvent::EnterWorld { script: false })),
            "the deprecated enterWorld is used"
        );
        // A second switch leaves the first entity, exercising the deprecated
        // leaveWorld.
        world.switch_player(ClientEntity::new(EntityId(2), EntityScripts::deprecated()));
        assert!(
            world
                .events()
                .iter()
                .any(|e| matches!(e, PlayerEvent::LeaveWorld { script: false })),
            "the deprecated leaveWorld is used"
        );
    }

    /// `Entity::onChangeSpace` (`entity.cpp:537-567`): the script leave,
    /// both C++ transient hooks, the picker check, then the script enter.
    #[test]
    fn change_space_interleaves_the_transient_hooks_and_the_picker() {
        let mut world = ClientWorld::new();
        let mut entity = ClientEntity::new(EntityId(1), EntityScripts::modern());
        entity.set_target(true);
        world.switch_player(entity);
        world.clear_events();

        world.change_space(7);
        let events: Vec<_> = world.events().iter().collect();
        assert_eq!(
            events,
            vec![
                &PlayerEvent::LeaveSpace { transient: true },
                &PlayerEvent::EnterSpace { transient: true },
                &PlayerEvent::PickerCheck,
                &PlayerEvent::EnterSpace { transient: true },
            ]
        );
        assert_eq!(world.player().and_then(ClientEntity::space_id), Some(7));
    }

    /// `isATarget` drives the entity picker: clear on leave, check on a
    /// space change.
    #[test]
    fn picker_events_follow_the_target_flag() {
        let mut world = ClientWorld::new();
        let mut entity = ClientEntity::new(EntityId(1), EntityScripts::modern());
        entity.set_target(true);
        world.switch_player(entity);
        world.clear_events();
        world.switch_player(ClientEntity::new(EntityId(2), EntityScripts::modern()));
        assert!(
            world
                .events()
                .iter()
                .any(|e| matches!(e, PlayerEvent::PickerClear)),
            "leaving a target clears the picker (entity.cpp:612-615)"
        );
    }

    /// `ScriptApp::tick` fires `Personality.onChangeEnvironments` only when
    /// the inside flag flips.
    #[test]
    fn personality_environment_callback_fires_once_per_change() {
        let mut world = ClientWorld::new();
        world.update_environment(false);
        assert_eq!(
            world.events().to_vec(),
            vec![PlayerEvent::ChangeEnvironments { inside: true }],
            "outside → inside fires with the new value"
        );
        world.update_environment(false); // still inside: no event
        assert_eq!(world.events().len(), 1, "no repeat fire");
        world.update_environment(true); // back outside
        assert_eq!(
            world.events().to_vec(),
            vec![
                PlayerEvent::ChangeEnvironments { inside: true },
                PlayerEvent::ChangeEnvironments { inside: false }
            ]
        );
    }

    /// The defensive path in `onEnterWorld` (`entity.cpp:428-431`): entering
    /// the world while still in a space leaves the world first.
    #[test]
    fn enter_world_leaves_a_stale_world_first() {
        let mut world = ClientWorld::new();
        let mut entity = ClientEntity::new(EntityId(1), EntityScripts::modern());
        world.enter_world(&mut entity);
        world.clear_events();
        // The same entity re-enters while still in the world: `onEnterWorld`
        // leaves the stale world first.
        world.enter_world(&mut entity);
        assert_eq!(
            world.events().first(),
            Some(&PlayerEvent::LeaveWorld { script: true }),
            "a stale world is left before the new one is entered"
        );
        assert!(entity.in_world(), "the entity ends up in the world");
    }
}
