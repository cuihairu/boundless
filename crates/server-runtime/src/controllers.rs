//! The per-entity controller collection and the entity extras
//! (`server/cellapp/controllers.*`, `controller.*`, `entity_extra.*`).
//!
//! Every entity — real or ghost — owns a `Controllers` map from
//! `ControllerID` to a running controller (`Controllers::container_`).
//! Controllers register in one of three domains (`ControllerDomain`):
//! `DOMAIN_GHOST` runs on ghost copies only, `DOMAIN_REAL` on the real
//! entity only, `DOMAIN_GHOST_AND_REAL` on both. Adding a controller on a
//! real entity fans a `ghostControllerCreate` message out to every haunt,
//! deleting fans `ghostControllerDelete`, and a running ghost-domain
//! controller pushes `ghostControllerUpdate` — the ghost side replays the
//! same faces through `createGhost`/`deleteGhost`/`updateGhost`.
//!
//! Two id regimes share the u16 `ControllerID` space: plain controllers
//! get `nextControllerID()` — `lastAllocatedID_` wraps 1..=0x7fff
//! (`EXCLUSIVE_CONTROLLER_ID_MASK`), skipping ids still in the container
//! and never handing out 0 — while exclusive controllers claim ids above
//! the mask via `Controller::getExclusiveID(category)`, and adding one
//! preempt-deletes the previous controller of the same category.
//!
//! The acceptance for BW-08-02-02 is "反复注册不会留存旧对象": registering
//! and clearing over and over leaves nothing stale behind. The faces that
//! guarantee it, all ported here:
//!
//! - an exclusive add deletes the old same-category controller first
//!   (`controllers.cpp:286-291`);
//! - `nextControllerID` never collides with a live id nor repeats 0
//!   (`controllers.cpp:549-557`);
//! - `delController` erases from the container *before* stopping, so a
//!   script callback fired by the stop cannot re-enter and double-delete
//!   (`controllers.cpp:355-428`);
//! - `stopReals` erases real-only controllers before stopping them for the
//!   same re-entrancy reason (`controllers.cpp:490-527`);
//! - `Entity` init throws the whole collection away on reuse —
//!   `delete pControllers_; pControllers_ = new Controllers;`
//!   (`entity.cpp:1288-1290`) — so `lastAllocatedID_` restarts at 0;
//! - `Controller::create`'s insertion into the map is std::map::insert
//!   semantics: an id already in the container keeps its original record
//!   and the newcomer is not stored (`controllers.cpp:81-84, 246-248`);
//! - entity extras are owned by the entity: `EntityExtra::Instance::clear`
//!   deletes and NULLs the slot, `Entity::destroy` (first pass,
//!   `entity.cpp:2984-2989`) and `~Entity` (second pass,
//!   `entity.cpp:1331-1346`) clear every slot, and `Entity::relocated`
//!   notifies the surviving ones (`entity.cpp:4207-4214`).
//!
//! The start/stop faces are recorded as [`ControllerEvent`]s: the timers
//! and navigation behind `startReal`/`stopReal` land with BW-08-02-03/04,
//! so this module pins the registration lifecycle and the exact order the
//! reference fires those calls in.

use std::collections::BTreeMap;

use crate::cellappmgr::Addr;
use crate::entity_dispatch::EntityId;
use foundation::stream::{BinaryStream, StreamError};

// -----------------------------------------------------------------------------
// Section: Constants (`controller.hpp`, `cellapp_config.cpp`)
// -----------------------------------------------------------------------------

/// `DOMAIN_GHOST`: the controller runs on ghost copies of the entity.
pub const DOMAIN_GHOST: u8 = 1;

/// `DOMAIN_REAL`: the controller runs on the real entity only.
pub const DOMAIN_REAL: u8 = 2;

/// `DOMAIN_GHOST_AND_REAL`: both halves run it.
pub const DOMAIN_GHOST_AND_REAL: u8 = DOMAIN_GHOST | DOMAIN_REAL;

/// `Controller::EXCLUSIVE_CONTROLLER_ID_MASK` (`controller.hpp:43`): ids at
/// or below this come from `nextControllerID`; exclusive categories claim
/// the ids above it.
pub const EXCLUSIVE_CONTROLLER_ID_MASK: u16 = 0x7fff;

/// `CellAppConfig::expectedMaxControllers` (`cellapp_config.cpp:63`).
pub const EXPECTED_MAX_CONTROLLERS: usize = 25;

/// `CellAppConfig::absoluteMaxControllers` (`cellapp_config.cpp:64`): at
/// this many `addController` refuses with an error and returns 0.
pub const ABSOLUTE_MAX_CONTROLLERS: usize = 50;

/// `ControllerID`/`ControllerType` (`controller.hpp:31-32`).
pub type ControllerId = u16;
pub type ControllerType = u16;

// -----------------------------------------------------------------------------
// Section: The controller record (`controller.hpp:66-160`)
// -----------------------------------------------------------------------------

/// One registered controller (`Controller`). The concrete behaviours —
/// movement, turning, acceleration — arrive with BW-08-02-03; this record
/// carries the registration face this item needs: identity, domain,
/// exclusive category, attachment and the two streamed payloads
/// (`writeRealToStream`/`writeGhostToStream` overrides), kept as blobs
/// since the C++ side reads them type-driven.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Controller {
    /// `controllerID_`, assigned by `init`.
    pub id: ControllerId,
    /// `type_`, the registered controller type number.
    pub type_id: ControllerType,
    /// `userArg_`, the script's user data (`init` overwrites whatever the
    /// record carried in).
    pub user_arg: i32,
    /// `domain_`.
    pub domain: u8,
    /// `exclusiveID_`: non-zero only for exclusive-category controllers
    /// (`Controller(const char * exclusiveClass, ...)` at
    /// `controller.hpp:151`).
    pub exclusive_id: ControllerId,
    /// `pEntity_ != NULL` — set by `init`, cleared by `disowned`
    /// (`controller.cpp:44-58`).
    pub attached: bool,
    /// The `readRealFromStream`/`writeRealToStream` payload.
    pub real_state: Vec<u8>,
    /// The `readGhostFromStream`/`writeGhostToStream` payload.
    pub ghost_state: Vec<u8>,
}

impl Controller {
    /// `Controller::Controller( type )`: fresh and unattached.
    pub fn new(type_id: ControllerType, domain: u8) -> Self {
        Self {
            id: 0,
            type_id,
            user_arg: 0,
            domain,
            exclusive_id: 0,
            attached: false,
            real_state: Vec::new(),
            ghost_state: Vec::new(),
        }
    }

    /// `Controller::Controller( exclusiveClass, ... )`
    /// (`controller.cpp` via `controller.hpp:151`): claims the category's
    /// id out of the registry, creating it.
    pub fn with_exclusive_category(
        type_id: ControllerType,
        domain: u8,
        category: &str,
        ids: &mut ExclusiveControllerIds,
    ) -> Self {
        let mut controller = Self::new(type_id, domain);
        controller.exclusive_id = ids
            .get_id(category, true)
            .expect("createIfNecessary always yields an id");
        controller
    }

    /// `Controller::init` (`controller.cpp:44-51`): bind identity and
    /// attach.
    fn init(&mut self, id: ControllerId, user_arg: i32) {
        self.id = id;
        self.user_arg = user_arg;
        self.attached = true;
    }

    /// Whether the controller carries a domain bit.
    fn in_domain(&self, domain: u8) -> bool {
        self.domain & domain != 0
    }
}

// -----------------------------------------------------------------------------
// Section: Lifecycle events and ghost traffic
// -----------------------------------------------------------------------------

/// One `Controller::startReal`/`stopReal`/`startGhost`/`stopGhost`/
/// `disowned`/stream read observation (`controller.cpp:181-206` and the
/// vtable overrides). The timer faces behind start/stop land with
/// BW-08-02-03/04; the order recorded here is the reference's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControllerEvent {
    /// `startReal(isInitialStart)`.
    StartReal {
        /// The controller's id.
        id: ControllerId,
        /// True on the first start, false off an offload/restore.
        initial: bool,
    },
    /// `stopReal(isFinalStop)`.
    StopReal {
        /// The controller's id.
        id: ControllerId,
        /// True on entity destruction, false when it becomes a ghost.
        final_stop: bool,
    },
    /// `startGhost`.
    StartGhost {
        /// The controller's id.
        id: ControllerId,
    },
    /// `stopGhost`.
    StopGhost {
        /// The controller's id.
        id: ControllerId,
    },
    /// `disowned` (`controller.cpp:55-58`).
    Disowned {
        /// The controller's id.
        id: ControllerId,
    },
    /// `readRealFromStream` ran (offload/restore replay).
    ReadReal {
        /// The controller's id.
        id: ControllerId,
    },
    /// `readGhostFromStream` ran.
    ReadGhost {
        /// The controller's id.
        id: ControllerId,
    },
}

/// One haunt-directed message: `CellAppInterface::ghostControllerCreate` /
/// `ghostControllerDelete` / `ghostControllerUpdate`
/// (`controllers.cpp:305-317, 394-404, 432-445`), one entry per haunt per
/// call, exactly the reference's per-haunt bundle loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhostControllerOutbound {
    /// `ghostControllerCreate`: entity id, controller id, type, user arg,
    /// then the controller's ghost payload.
    Create {
        /// The haunt this copy is bound for.
        haunt: Addr,
        /// `pEntity->id()`.
        entity_id: EntityId,
        /// `pController->id()`.
        id: ControllerId,
        /// `pController->type()`.
        type_id: ControllerType,
        /// `pController->userArg()`.
        user_arg: i32,
        /// `writeGhostToStream`.
        payload: Vec<u8>,
    },
    /// `ghostControllerDelete`: entity id, controller id.
    Delete {
        /// The haunt this copy is bound for.
        haunt: Addr,
        /// `pEntity->id()`.
        entity_id: EntityId,
        /// The controller id being deleted.
        id: ControllerId,
    },
    /// `ghostControllerUpdate`: entity id, controller id, ghost payload.
    Update {
        /// The haunt this copy is bound for.
        haunt: Addr,
        /// `pEntity->id()`.
        entity_id: EntityId,
        /// The controller id being updated.
        id: ControllerId,
        /// `writeGhostToStream`.
        payload: Vec<u8>,
    },
}

// -----------------------------------------------------------------------------
// Section: The exclusive-id registry (`controller.cpp:498-525`)
// -----------------------------------------------------------------------------

/// `Controller::getExclusiveID`'s category map. Process-global in the
/// reference (a function-local static); a value here so tests and callers
/// hold their own. Ids are `EXCLUSIVE_CONTROLLER_ID_MASK + size + 1`,
/// strictly above `nextControllerID`'s range.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExclusiveControllerIds {
    /// The `s_exclusiveIDs` map.
    ids: BTreeMap<String, ControllerId>,
}

impl ExclusiveControllerIds {
    /// `getExclusiveID(exclusiveClass, createIfNecessary)`: the category's
    /// id, `None` (the reference's 0) when absent and not creating.
    pub fn get_id(&mut self, category: &str, create_if_necessary: bool) -> Option<ControllerId> {
        if let Some(id) = self.ids.get(category) {
            return Some(*id);
        }
        if create_if_necessary {
            let id = EXCLUSIVE_CONTROLLER_ID_MASK + self.ids.len() as u16 + 1;
            self.ids.insert(category.to_owned(), id);
            return Some(id);
        }
        None
    }
}

// -----------------------------------------------------------------------------
// Section: The collection (`controllers.hpp:34-64`)
// -----------------------------------------------------------------------------

/// The per-entity controller map (`Controllers`). Owned by the entity;
/// `Entity` init replaces it wholesale on reuse.
#[derive(Clone, Debug, Default)]
pub struct Controllers {
    /// `container_`, keyed by controller id like the reference's `BW::map`.
    container: BTreeMap<ControllerId, Controller>,
    /// `lastAllocatedID_`, starts at 0 (`controllers.cpp:39`).
    last_allocated_id: ControllerId,
    /// The lifecycle event log, in the reference's firing order.
    pub history: Vec<ControllerEvent>,
    /// Del ids the caller asked to be warned about
    /// (`WARNING_MSG` at `controllers.cpp:360-366`) — the
    /// `warnOnFailure` face.
    pub missed_deletes: Vec<ControllerId>,
    /// Adds refused by the absolute-max guard
    /// (`ERROR_MSG` at `controllers.cpp:278-290`).
    pub overflow_refusals: u32,
}

impl Controllers {
    /// `Controllers::Controllers()`: an empty container, id counter at 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// The live controller for an id.
    pub fn controller(&self, id: ControllerId) -> Option<&Controller> {
        self.container.get(&id)
    }

    /// The live controller for an id, mutable.
    pub fn controller_mut(&mut self, id: ControllerId) -> Option<&mut Controller> {
        self.container.get_mut(&id)
    }

    /// The live controller ids, in id order.
    pub fn ids(&self) -> Vec<ControllerId> {
        self.container.keys().copied().collect()
    }

    /// `container_.size()`.
    pub fn len(&self) -> usize {
        self.container.len()
    }

    /// Whether no controller is registered.
    pub fn is_empty(&self) -> bool {
        self.container.is_empty()
    }

    /// `lastAllocatedID_` — streamed on offload so the new owner's
    /// `nextControllerID` continues where this one left off.
    pub fn last_allocated_id(&self) -> ControllerId {
        self.last_allocated_id
    }

    /// `Controllers::nextControllerID` (`controllers.cpp:549-557`): the
    /// masked counter wraps, skipping live ids and 0.
    fn next_controller_id(&mut self) -> ControllerId {
        loop {
            self.last_allocated_id = (self.last_allocated_id + 1) & EXCLUSIVE_CONTROLLER_ID_MASK;
            if self.last_allocated_id != 0 && !self.container.contains_key(&self.last_allocated_id)
            {
                return self.last_allocated_id;
            }
        }
    }

    /// std::map::insert semantics (`controllers.cpp:84, 246-248`): the
    /// original record stays when the id is already present.
    fn insert_if_absent(&mut self, controller: Controller) -> bool {
        if self.container.contains_key(&controller.id) {
            return false;
        }
        self.container.insert(controller.id, controller);
        true
    }

    /// `Controllers::addController` (`controllers.cpp:270-369`), the
    /// real-entity script face. Returns the new controller id, or 0 when
    /// the absolute-max guard refused. A ghost-domain controller fans a
    /// `ghostControllerCreate` to every haunt before `startGhost`;
    /// a real-domain one then gets `startReal(true)`.
    pub fn add_controller(
        &mut self,
        mut controller: Controller,
        user_arg: i32,
        entity_id: EntityId,
        haunts: &[Addr],
        out: &mut Vec<GhostControllerOutbound>,
    ) -> ControllerId {
        let count = self.container.len();
        if count >= ABSOLUTE_MAX_CONTROLLERS {
            self.overflow_refusals += 1;
            return 0;
        }

        let mut controller_id = controller.exclusive_id;
        if controller_id != 0 {
            // Replacing an exclusive controller: stop the old one of the
            // same category first, without the warning
            // (controllers.cpp:286-291).
            self.del_controller(controller_id, entity_id, true, false, haunts, out);
        } else {
            controller_id = self.next_controller_id();
        }

        controller.init(controller_id, user_arg);
        self.insert_if_absent(controller.clone());

        if controller.in_domain(DOMAIN_GHOST) {
            for &haunt in haunts {
                out.push(GhostControllerOutbound::Create {
                    haunt,
                    entity_id,
                    id: controller.id,
                    type_id: controller.type_id,
                    user_arg: controller.user_arg,
                    payload: controller.ghost_state.clone(),
                });
            }
            self.history
                .push(ControllerEvent::StartGhost { id: controller.id });
        }

        if controller.in_domain(DOMAIN_REAL) {
            self.history.push(ControllerEvent::StartReal {
                id: controller.id,
                initial: true,
            });
        }

        controller.id
    }

    /// `Controllers::delController` (`controllers.cpp:371-428`): erase
    /// first, then the haunt deletes, then stop real, stop ghost, disown —
    /// the order keeps a re-entrant delete from inside a stop callback
    /// honest, because the container no longer holds the id.
    pub fn del_controller(
        &mut self,
        id: ControllerId,
        entity_id: EntityId,
        is_real_entity: bool,
        warn_on_failure: bool,
        haunts: &[Addr],
        out: &mut Vec<GhostControllerOutbound>,
    ) -> bool {
        let Some(controller) = self.container.remove(&id) else {
            if warn_on_failure {
                self.missed_deletes.push(id);
            }
            return false;
        };

        if is_real_entity && controller.in_domain(DOMAIN_GHOST) {
            for &haunt in haunts {
                out.push(GhostControllerOutbound::Delete {
                    haunt,
                    entity_id,
                    id,
                });
            }
        }

        if controller.in_domain(DOMAIN_REAL) && is_real_entity {
            self.history.push(ControllerEvent::StopReal {
                id,
                final_stop: true,
            });
        }

        if controller.in_domain(DOMAIN_GHOST) {
            self.history.push(ControllerEvent::StopGhost { id });
        }

        self.history.push(ControllerEvent::Disowned { id });
        true
    }

    /// `Controllers::modController` (`controllers.cpp:430-446`): push the
    /// controller's fresh ghost payload to every haunt. A no-op answer for
    /// an unknown id (the reference asserts).
    pub fn mod_controller(
        &mut self,
        id: ControllerId,
        entity_id: EntityId,
        haunts: &[Addr],
        out: &mut Vec<GhostControllerOutbound>,
    ) -> bool {
        let Some(controller) = self.container.get(&id) else {
            return false;
        };
        debug_assert!(controller.in_domain(DOMAIN_GHOST));
        let payload = controller.ghost_state.clone();
        for &haunt in haunts {
            out.push(GhostControllerOutbound::Update {
                haunt,
                entity_id,
                id,
                payload: payload.clone(),
            });
        }
        true
    }

    /// `Controllers::startReals` (`controllers.cpp:448-461`):
    /// `Entity::startRealControllers` after an offload/restore — every
    /// real-domain controller restarts, not as its initial start.
    pub fn start_reals(&mut self) {
        let ids: Vec<ControllerId> = self
            .container
            .values()
            .filter(|c| c.in_domain(DOMAIN_REAL))
            .map(|c| c.id)
            .collect();
        for id in ids {
            self.history
                .push(ControllerEvent::StartReal { id, initial: false });
        }
    }

    /// `Controllers::stopReals` (`controllers.cpp:463-527`): stop the
    /// real-domain controllers as the entity turns ghost, erasing the
    /// real-only ones *before* the stop so a re-entrant delete inside the
    /// stop's script callback sees a clean container. Ghost-domain
    /// controllers survive (stopped at most, never erased).
    pub fn stop_reals(&mut self, is_final_stop: bool) {
        let mut to_stop = Vec::new();
        let keys: Vec<ControllerId> = self.container.keys().copied().collect();
        for id in keys {
            let controller = &self.container[&id];
            if controller.in_domain(DOMAIN_REAL) {
                to_stop.push(controller.clone());
            }
            if controller.domain == DOMAIN_REAL {
                self.container.remove(&id);
            }
        }

        for controller in to_stop {
            if controller.attached {
                self.history.push(ControllerEvent::StopReal {
                    id: controller.id,
                    final_stop: is_final_stop,
                });
                if !controller.in_domain(DOMAIN_GHOST) {
                    // Only real-only controllers disown here; a both-domain
                    // controller keeps its ghost half running
                    // (controllers.cpp:509-516).
                    self.history
                        .push(ControllerEvent::Disowned { id: controller.id });
                }
            }
        }
    }

    /// `Controllers::createGhost` (`controllers.cpp:186-205`), the
    /// ghost-side receiver of `ghostControllerCreate`. Insertion is
    /// keep-the-original: a repeated create for a live id leaves the old
    /// record untouched.
    pub fn create_ghost(
        &mut self,
        id: ControllerId,
        type_id: ControllerType,
        user_arg: i32,
        payload: Vec<u8>,
    ) -> bool {
        let mut controller = Controller::new(type_id, DOMAIN_GHOST);
        controller.init(id, user_arg);
        controller.ghost_state = payload;
        let inserted = self.insert_if_absent(controller);
        if inserted {
            self.history.push(ControllerEvent::StartGhost { id });
        }
        inserted
    }

    /// `Controllers::deleteGhost` (`controllers.cpp:207-216`): the
    /// ghost-side receiver of `ghostControllerDelete` — a plain local
    /// delete, warning on a miss.
    pub fn delete_ghost(
        &mut self,
        id: ControllerId,
        entity_id: EntityId,
        out: &mut Vec<GhostControllerOutbound>,
    ) -> bool {
        self.del_controller(id, entity_id, false, true, &[], out)
    }

    /// `Controllers::updateGhost` (`controllers.cpp:218-230`): replace the
    /// ghost payload of a live controller.
    pub fn update_ghost(&mut self, id: ControllerId, payload: Vec<u8>) -> bool {
        let Some(controller) = self.container.get_mut(&id) else {
            return false;
        };
        controller.ghost_state = payload;
        self.history.push(ControllerEvent::ReadGhost { id });
        true
    }

    /// `Controllers::writeRealsToStream` -> `readRealsFromStream`
    /// (`controllers.cpp:172-210, 213-253`) as an offload: the id counter,
    /// then every real-domain controller — its id, and (for real-only ones,
    /// which the far side cannot already hold as a ghost) the type and user
    /// arg — then the real payload.
    pub fn write_reals_to_stream(&self, stream: &mut BinaryStream) {
        stream.write_u16(self.last_allocated_id);

        let reals: Vec<&Controller> = self
            .container
            .values()
            .filter(|c| c.in_domain(DOMAIN_REAL))
            .collect();
        stream.write_u16(reals.len() as u16);
        for controller in reals {
            stream.write_u16(controller.id);
            if !controller.in_domain(DOMAIN_GHOST) {
                stream.write_u16(controller.type_id);
                stream.write_i32(controller.user_arg);
            }
            stream
                .write_blob(&controller.real_state)
                .expect("u32 length fits");
        }
    }

    /// The onload handover bridge: the reference holds ONE `pControllers_`
    /// container for the entity across its ghost and real lives; the port
    /// keeps the ghost record's own, so its ghost-domain halves move into
    /// the entity container before the streamed reals are read
    /// (`readRealsFromStream` finds them by id). Keep-the-original on an
    /// id clash.
    pub fn merge_ghost_halves(&mut self, other: Controllers) {
        for (id, controller) in other.container {
            if controller.in_domain(DOMAIN_GHOST) {
                self.container.entry(id).or_insert(controller);
            }
        }
    }

    /// The receiving half of the offload: controllers already here as
    /// ghosts keep their records and take the fresh real payload;
    /// real-only ones are constructed from the stream. `startReal` is
    /// deliberately *not* called — that waits for `start_reals`
    /// (`controllers.cpp:249-251`).
    pub fn read_reals_from_stream(&mut self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.last_allocated_id = stream.read_u16()?;

        let num_offloaded = stream.read_u16()?;
        for _ in 0..num_offloaded {
            let cid = stream.read_u16()?;
            if let Some(controller) = self.container.get_mut(&cid) {
                debug_assert!(controller.attached);
                debug_assert!(controller.in_domain(DOMAIN_GHOST));
                let payload = stream.read_blob()?;
                controller.real_state = payload;
            } else {
                let type_id = stream.read_u16()?;
                let user_arg = stream.read_i32()?;
                let payload = stream.read_blob()?;
                let mut controller = Controller::new(type_id, DOMAIN_REAL);
                controller.init(cid, user_arg);
                controller.real_state = payload;
                self.insert_if_absent(controller);
            }
            self.history.push(ControllerEvent::ReadReal { id: cid });
        }
        Ok(())
    }

    /// `Controllers::writeGhostsToStream` (`controllers.cpp:101-141`): the
    /// count, then per ghost-domain controller the id, type, user arg and
    /// ghost payload.
    pub fn write_ghosts_to_stream(&self, stream: &mut BinaryStream) {
        let ghosts: Vec<&Controller> = self
            .container
            .values()
            .filter(|c| c.in_domain(DOMAIN_GHOST))
            .collect();
        stream.write_u16(ghosts.len() as u16);
        for controller in ghosts {
            stream.write_u16(controller.id);
            stream.write_u16(controller.type_id);
            stream.write_i32(controller.user_arg);
            stream
                .write_blob(&controller.ghost_state)
                .expect("u32 length fits");
        }
    }

    /// `Controllers::readGhostsFromStream` (`controllers.cpp:61-98`):
    /// construct, attach, store (keep-the-original), read the ghost
    /// payload, then `startGhost`.
    pub fn read_ghosts_from_stream(
        &mut self,
        stream: &mut BinaryStream,
    ) -> Result<(), StreamError> {
        let num_controllers = stream.read_u16()?;
        for _ in 0..num_controllers {
            let cid = stream.read_u16()?;
            let type_id = stream.read_u16()?;
            let user_arg = stream.read_i32()?;

            let mut controller = Controller::new(type_id, DOMAIN_GHOST);
            controller.init(cid, user_arg);
            let inserted = self.insert_if_absent(controller);
            if inserted {
                let payload = stream.read_blob()?;
                if let Some(controller) = self.container.get_mut(&cid) {
                    controller.ghost_state = payload;
                }
                self.history.push(ControllerEvent::ReadGhost { id: cid });
                self.history.push(ControllerEvent::StartGhost { id: cid });
            } else {
                // The constructed controller was not stored —
                // std::map::insert kept the original record — so its
                // payload is read into the discarded object and dropped
                // with it; the stored record is untouched.
                let _ = stream.read_blob()?;
            }
        }
        Ok(())
    }

    /// `Controllers::py_cancel`'s found-controller face
    /// (`controllers.cpp:530-577`): `Controller::cancel`
    /// (`controller.cpp:107-117`) deletes the controller by id — stopping
    /// it in whatever domains it runs. `None`-adjacent id parsing lives
    /// with the script layer.
    pub fn cancel(
        &mut self,
        id: ControllerId,
        entity_id: EntityId,
        haunts: &[Addr],
        out: &mut Vec<GhostControllerOutbound>,
    ) -> bool {
        self.del_controller(id, entity_id, true, true, haunts, out)
    }

    /// `Controllers::~Controllers` (`controllers.cpp:42-55`): whatever is
    /// still attached is ghost-domain (the reference asserts) — stop it and
    /// disown it.
    pub fn destroy(&mut self) {
        let controllers: Vec<Controller> = self.container.values().cloned().collect();
        for controller in controllers {
            debug_assert!(controller.in_domain(DOMAIN_GHOST));
            self.history
                .push(ControllerEvent::StopGhost { id: controller.id });
            self.history
                .push(ControllerEvent::Disowned { id: controller.id });
        }
        self.container.clear();
    }
}

// -----------------------------------------------------------------------------
// Section: Entity extras (`entity_extra.hpp`)
// -----------------------------------------------------------------------------

/// The entity-extra slot index (`eeid_`, `Entity::registerEntityExtra`'s
/// return value).
pub type EntityExtraId = usize;

/// One live extra (`Derived`, stored in `Entity::extras_[eeid_]`). The
/// derived extras are arbitrary state machines; their identity here is the
/// registered name plus their bytes, and `relocated()`'s call count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtraSlot {
    /// The registered type's name (`Derived`'s class name in C++).
    pub name: String,
    /// The extra's state bytes.
    pub data: Vec<u8>,
    /// `relocated()` calls seen (`entity.cpp:4207-4214`).
    pub relocations: u32,
}

/// The entity's extra slots (`Entity::extras_` plus the static
/// `s_entityExtraInfo()` type registry). Extras are owned by the entity:
/// clearing is delete-and-NULL, entity destruction clears everything, and
/// a cleared slot touched again constructs a fresh instance
/// (`EntityExtra::Instance`, `entity_extra.hpp:47-86`).
#[derive(Clone, Debug, Default)]
pub struct EntityExtras {
    /// The slots, keyed by eeid; an absent key is a NULL slot.
    slots: BTreeMap<EntityExtraId, ExtraSlot>,
    /// `s_entityExtraInfo().size()`: the registered type count.
    type_count: usize,
}

impl EntityExtras {
    /// `Entity::registerEntityExtra` (`entity.cpp:7543-7557`): register a
    /// derived extra type, handing back its slot index.
    pub fn register_extra_type(&mut self) -> EntityExtraId {
        let eeid = self.type_count;
        self.type_count += 1;
        eeid
    }

    /// The registered type count.
    pub fn type_count(&self) -> usize {
        self.type_count
    }

    /// `EntityExtra::Instance::operator()` (`entity_extra.hpp:53-61`):
    /// touch the slot — the first touch constructs, later ones return the
    /// same instance.
    pub fn instance(
        &mut self,
        eeid: EntityExtraId,
        construct: impl FnOnce() -> ExtraSlot,
    ) -> &mut ExtraSlot {
        self.slots.entry(eeid).or_insert_with(construct)
    }

    /// `EntityExtra::Instance::exists` (`entity_extra.hpp:64-67`).
    pub fn exists(&self, eeid: EntityExtraId) -> bool {
        self.slots.contains_key(&eeid)
    }

    /// The live slot, if any.
    pub fn slot(&self, eeid: EntityExtraId) -> Option<&ExtraSlot> {
        self.slots.get(&eeid)
    }

    /// `EntityExtra::Instance::clear` (`entity_extra.hpp:69-74`): delete
    /// and NULL the slot, safe when it was never there. Answers whether
    /// something was cleared.
    pub fn clear(&mut self, eeid: EntityExtraId) -> bool {
        self.slots.remove(&eeid).is_some()
    }

    /// `Entity::relocated` (`entity.cpp:4206-4214`): tell every live extra.
    pub fn relocated(&mut self) {
        for slot in self.slots.values_mut() {
            slot.relocations += 1;
        }
    }

    /// The `Entity::destroy` first pass (`entity.cpp:2984-2989`) and the
    /// `~Entity` second pass (`entity.cpp:1331-1339`): every slot cleared.
    pub fn destroy_extras(&mut self) {
        self.slots.clear();
    }
}

// -----------------------------------------------------------------------------
// Section: Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const HAUNT_A: Addr = (0x0a00_0001, 3001);
    const HAUNT_B: Addr = (0x0a00_0002, 3002);

    fn haunts() -> Vec<Addr> {
        vec![HAUNT_A, HAUNT_B]
    }

    // -------------------------------------------------------------------
    // TDD: BW-08-02-02 挂接与清除 Controller/Extra — 反复注册不会留存旧对象
    // -------------------------------------------------------------------

    /// `nextControllerID` (`controllers.cpp:549-557`): ids climb from 1,
    /// never 0; freed ids are not reused early — the counter keeps
    /// climbing and only wraps at the exclusive mask, skipping live ids.
    #[test]
    fn next_controller_id_climbs_wraps_and_skips_live_ids() {
        let mut controllers = Controllers::new();
        let mut out = Vec::new();

        for expected in 1..=ABSOLUTE_MAX_CONTROLLERS as u16 {
            let id =
                controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 7, &[], &mut out);
            assert_ne!(id, 0, "the guard refuses only past the absolute max");
            assert_eq!(id, expected);
        }
        // Full container: further adds refuse and count.
        assert_eq!(
            controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 7, &[], &mut out),
            0
        );
        assert_eq!(controllers.overflow_refusals, 1);
        assert_eq!(controllers.len(), ABSOLUTE_MAX_CONTROLLERS);

        // Two holes opened — but the counter keeps climbing, it does not
        // jump back to freed ids before wrapping.
        assert!(controllers.del_controller(1, 7, true, false, &[], &mut out));
        assert!(controllers.del_controller(2, 7, true, false, &[], &mut out));
        let next = controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 7, &[], &mut out);
        assert_eq!(next, (ABSOLUTE_MAX_CONTROLLERS + 1) as u16);

        // The wrap itself: with 1 and 2 the only live ids, drive the
        // counter past the mask and watch it skip 0 and both live ids.
        let mut controllers = Controllers::new();
        controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 7, &[], &mut out);
        controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 7, &[], &mut out);
        assert_eq!(controllers.next_controller_id(), 3);
        let mut id = 3;
        while id != EXCLUSIVE_CONTROLLER_ID_MASK {
            id = controllers.next_controller_id();
        }
        // Mask reached; the next tick wraps through 0 and skips 1, 2.
        assert_eq!(controllers.next_controller_id(), 3);
        assert_eq!(controllers.last_allocated_id(), 3);
    }

    /// The acceptance core: register and clear the same kind of controller
    /// over and over — each cycle leaves exactly one record, the newest,
    /// with the old one stopped and disowned, and the id counter keeps
    /// moving without collisions.
    #[test]
    fn repeated_register_clear_cycles_leave_no_stale_objects() {
        let mut controllers = Controllers::new();

        for cycle in 0..3u16 {
            let before = controllers.last_allocated_id();
            let id = controllers.add_controller(
                Controller::new(7, DOMAIN_GHOST_AND_REAL),
                cycle as i32,
                42,
                &haunts(),
                &mut Vec::new(),
            );
            assert_ne!(id, 0);
            assert!(id > before || before == 0, "ids keep climbing");
            assert_eq!(controllers.len(), 1, "the old controller is gone");
            let live = controllers.controller(id).unwrap();
            assert_eq!(live.user_arg, cycle as i32);
            assert!(live.attached);

            // A running controller pushes ghost updates while it lives.
            assert!(controllers.update_ghost(id, b"tick".to_vec()));

            assert!(controllers.del_controller(id, 42, true, false, &haunts(), &mut Vec::new()));
            assert!(controllers.is_empty(), "nothing is left behind");
            // Cancelling again misses cleanly — with the warning face.
            assert!(!controllers.del_controller(id, 42, true, true, &[], &mut Vec::new()));
        }

        // The log holds the full story: per cycle add (StartGhost,
        // StartReal) then delete (StopReal, StopGhost, Disowned).
        let mut starts = 0;
        let mut stops = 0;
        for event in &controllers.history {
            match event {
                ControllerEvent::StartGhost { .. } | ControllerEvent::StartReal { .. } => {
                    starts += 1
                }
                ControllerEvent::StopReal { .. } | ControllerEvent::StopGhost { .. } => stops += 1,
                ControllerEvent::Disowned { .. } => {}
                ControllerEvent::ReadReal { .. } | ControllerEvent::ReadGhost { .. } => {}
            }
        }
        assert_eq!(starts, 6);
        assert_eq!(stops, 6);
        assert_eq!(
            controllers.missed_deletes.len(),
            3,
            "the extra cancels warned"
        );
    }

    /// An exclusive add preempt-deletes the old controller of the same
    /// category (`controllers.cpp:286-291`) — the movement-controller
    /// replacement face.
    #[test]
    fn an_exclusive_add_replaces_the_old_same_category_controller() {
        let mut ids = ExclusiveControllerIds::default();
        let movement = ids.get_id("Movement", true).unwrap();
        assert!(movement > EXCLUSIVE_CONTROLLER_ID_MASK);

        let mut controllers = Controllers::new();
        let mut out = Vec::new();

        let first = controllers.add_controller(
            Controller::with_exclusive_category(3, DOMAIN_REAL, "Movement", &mut ids),
            11,
            5,
            &[],
            &mut out,
        );
        assert_eq!(first, movement);
        assert_eq!(controllers.len(), 1);

        // Same category again: the old controller is deleted first.
        let second = controllers.add_controller(
            Controller::with_exclusive_category(3, DOMAIN_REAL, "Movement", &mut ids),
            22,
            5,
            &[],
            &mut out,
        );
        assert_eq!(second, movement, "the category id is stable");
        assert_eq!(controllers.len(), 1);
        assert_eq!(controllers.controller(movement).unwrap().user_arg, 22);

        // The old one was stopped (final stop) and disowned before the new
        // one started.
        let events: Vec<&ControllerEvent> = controllers
            .history
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    ControllerEvent::StopReal { .. } | ControllerEvent::StartReal { .. }
                )
            })
            .collect();
        assert_eq!(
            events,
            vec![
                &ControllerEvent::StartReal {
                    id: movement,
                    initial: true
                },
                &ControllerEvent::StopReal {
                    id: movement,
                    final_stop: true
                },
                &ControllerEvent::StartReal {
                    id: movement,
                    initial: true
                },
            ]
        );

        // A different category coexists.
        let other = ids.get_id("TrapTrigger", true).unwrap();
        assert_ne!(other, movement);
        let trap = controllers.add_controller(
            Controller::with_exclusive_category(4, DOMAIN_REAL, "TrapTrigger", &mut ids),
            0,
            5,
            &[],
            &mut out,
        );
        assert_eq!(trap, other);
        assert_eq!(controllers.len(), 2);

        // A known category fetched without creating answers None when
        // absent.
        let mut fresh = ExclusiveControllerIds::default();
        assert_eq!(fresh.get_id("Movement", false), None);
        assert_eq!(fresh.get_id("Movement", true), Some(movement));
    }

    /// A ghost-domain add fans a `ghostControllerCreate` per haunt before
    /// `startGhost`; `startReal` follows; the delete mirrors it.
    #[test]
    fn ghost_domain_adds_fan_out_to_every_haunt_in_reference_order() {
        let mut controllers = Controllers::new();
        let mut out = Vec::new();

        let id = controllers.add_controller(
            Controller::new(9, DOMAIN_GHOST),
            3,
            11,
            &haunts(),
            &mut out,
        );
        assert_eq!(id, 1);
        assert_eq!(
            out,
            vec![
                GhostControllerOutbound::Create {
                    haunt: HAUNT_A,
                    entity_id: 11,
                    id,
                    type_id: 9,
                    user_arg: 3,
                    payload: Vec::new(),
                },
                GhostControllerOutbound::Create {
                    haunt: HAUNT_B,
                    entity_id: 11,
                    id,
                    type_id: 9,
                    user_arg: 3,
                    payload: Vec::new(),
                },
            ]
        );
        assert_eq!(
            controllers.history,
            vec![ControllerEvent::StartGhost { id }]
        );

        // Delete: erase first, then the haunt deletes, stopGhost, disown.
        out.clear();
        assert!(controllers.del_controller(id, 11, true, false, &haunts(), &mut out));
        assert_eq!(
            out,
            vec![
                GhostControllerOutbound::Delete {
                    haunt: HAUNT_A,
                    entity_id: 11,
                    id
                },
                GhostControllerOutbound::Delete {
                    haunt: HAUNT_B,
                    entity_id: 11,
                    id
                },
            ]
        );
        assert_eq!(
            controllers.history,
            vec![
                ControllerEvent::StartGhost { id },
                ControllerEvent::StopGhost { id },
                ControllerEvent::Disowned { id },
            ]
        );
        assert!(controllers.is_empty());

        // A real-only controller produces no ghost traffic at all.
        out.clear();
        controllers.history.clear();
        let real_id =
            controllers.add_controller(Controller::new(2, DOMAIN_REAL), 0, 11, &haunts(), &mut out);
        assert!(out.is_empty());
        assert_eq!(
            controllers.history,
            vec![ControllerEvent::StartReal {
                id: real_id,
                initial: true
            }]
        );
    }

    /// `modController` (`controllers.cpp:430-446`) pushes the fresh ghost
    /// payload to every haunt.
    #[test]
    fn mod_controller_pushes_updates_to_every_haunt() {
        let mut controllers = Controllers::new();
        let mut out = Vec::new();
        let id = controllers.add_controller(
            Controller::new(9, DOMAIN_GHOST_AND_REAL),
            0,
            4,
            &haunts(),
            &mut out,
        );
        controllers.controller_mut(id).unwrap().ghost_state = b"moved".to_vec();

        out.clear();
        assert!(controllers.mod_controller(id, 4, &haunts(), &mut out));
        assert_eq!(
            out,
            vec![
                GhostControllerOutbound::Update {
                    haunt: HAUNT_A,
                    entity_id: 4,
                    id,
                    payload: b"moved".to_vec(),
                },
                GhostControllerOutbound::Update {
                    haunt: HAUNT_B,
                    entity_id: 4,
                    id,
                    payload: b"moved".to_vec(),
                },
            ]
        );
        assert!(!controllers.mod_controller(99, 4, &haunts(), &mut out));
    }

    /// `stopReals` (`controllers.cpp:463-527`): real-only controllers are
    /// erased before they stop — a re-entrant delete inside the stop sees
    /// nothing — while ghost-domain controllers survive the transition.
    #[test]
    fn stop_reals_erases_before_stopping_and_keeps_ghosts() {
        let mut controllers = Controllers::new();
        let mut out = Vec::new();
        let real_only =
            controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 6, &[], &mut out);
        let both = controllers.add_controller(
            Controller::new(2, DOMAIN_GHOST_AND_REAL),
            0,
            6,
            &[],
            &mut out,
        );
        let ghost_only =
            controllers.add_controller(Controller::new(3, DOMAIN_GHOST), 0, 6, &haunts(), &mut out);

        controllers.history.clear();
        controllers.stop_reals(false);

        // Real-only gone; both-domain kept (stopped, not disowned); ghost
        // untouched.
        assert_eq!(controllers.ids(), vec![both, ghost_only]);
        assert_eq!(
            controllers.history,
            vec![
                ControllerEvent::StopReal {
                    id: real_only,
                    final_stop: false
                },
                ControllerEvent::Disowned { id: real_only },
                ControllerEvent::StopReal {
                    id: both,
                    final_stop: false
                },
            ]
        );

        // The erased controller's delete now misses — nothing stale to
        // double-delete.
        assert!(!controllers.del_controller(real_only, 6, true, true, &[], &mut out));
        assert_eq!(controllers.missed_deletes, vec![real_only]);

        // The final stop (entity destruction) flags final and disowns the
        // both-domain controller too... no: only real-only controllers
        // disown here; the both-domain one keeps its ghost half.
        controllers.history.clear();
        controllers.stop_reals(true);
        assert_eq!(
            controllers.history,
            vec![ControllerEvent::StopReal {
                id: both,
                final_stop: true
            }]
        );

        // An unattached real-only controller is collected but not stopped.
        let mut detached = Controllers::new();
        let id = detached.add_controller(Controller::new(1, DOMAIN_REAL), 0, 6, &[], &mut out);
        detached.container.get_mut(&id).unwrap().attached = false;
        detached.history.clear();
        detached.stop_reals(false);
        assert!(detached.history.is_empty());
    }

    /// `startReals` (`controllers.cpp:448-461`) restarts every real-domain
    /// controller non-initially after an offload/restore.
    #[test]
    fn start_reals_restarts_every_real_domain_controller() {
        let mut controllers = Controllers::new();
        let mut out = Vec::new();
        let ghost =
            controllers.add_controller(Controller::new(1, DOMAIN_GHOST), 0, 6, &haunts(), &mut out);
        let real_a =
            controllers.add_controller(Controller::new(2, DOMAIN_REAL), 0, 6, &[], &mut out);
        let both = controllers.add_controller(
            Controller::new(3, DOMAIN_GHOST_AND_REAL),
            0,
            6,
            &[],
            &mut out,
        );

        controllers.history.clear();
        controllers.start_reals();
        assert_eq!(
            controllers.history,
            vec![
                ControllerEvent::StartReal {
                    id: real_a,
                    initial: false
                },
                ControllerEvent::StartReal {
                    id: both,
                    initial: false
                },
            ]
        );
        assert_eq!(controllers.controller(ghost).unwrap().domain, DOMAIN_GHOST);
    }

    /// `cancel` (`controller.cpp:107-117` via `controllers.cpp:530-577`):
    /// deletes through the same stop chain; a miss answers false.
    #[test]
    fn cancel_removes_the_controller_or_misses() {
        let mut controllers = Controllers::new();
        let mut out = Vec::new();
        let id = controllers.add_controller(
            Controller::new(7, DOMAIN_GHOST_AND_REAL),
            5,
            8,
            &[],
            &mut out,
        );

        controllers.history.clear();
        assert!(controllers.cancel(id, 8, &[], &mut out));
        assert!(controllers.is_empty());
        assert_eq!(
            controllers.history,
            vec![
                ControllerEvent::StopReal {
                    id,
                    final_stop: true
                },
                ControllerEvent::StopGhost { id },
                ControllerEvent::Disowned { id },
            ]
        );
        assert!(!controllers.cancel(id, 8, &[], &mut out));
        assert_eq!(controllers.missed_deletes, vec![id]);
    }

    /// The offload roundtrip (`controllers.cpp:172-253`): reals stream
    /// with the id counter; ghosts already on the far side keep their
    /// records and take the fresh real payload; `startReal` waits for
    /// `startReals`.
    #[test]
    fn the_offload_roundtrip_restores_reals_and_defers_start() {
        let mut source = Controllers::new();
        let mut out = Vec::new();
        source.add_controller(Controller::new(1, DOMAIN_REAL), 4, 9, &[], &mut out);
        let ghost_id = source.add_controller(
            Controller::new(2, DOMAIN_GHOST_AND_REAL),
            5,
            9,
            &[],
            &mut out,
        );
        source.add_controller(Controller::new(3, DOMAIN_GHOST), 6, 9, &haunts(), &mut out);
        source.controller_mut(1).unwrap().real_state = b"r1".to_vec();
        source.controller_mut(ghost_id).unwrap().real_state = b"r2".to_vec();
        source.controller_mut(ghost_id).unwrap().ghost_state = b"g2".to_vec();

        let mut stream = BinaryStream::new();
        source.write_reals_to_stream(&mut stream);
        stream.rewind();

        // The far side already holds the ghost half of the both-domain
        // controller.
        let mut target = Controllers::new();
        target.create_ghost(ghost_id, 2, 5, b"g2".to_vec());
        target.history.clear();
        target.read_reals_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());

        assert_eq!(target.last_allocated_id(), source.last_allocated_id());
        // Real-only 1 and 3 (ghost domain, skipped) — 3 is not streamed;
        // the both-domain controller was updated in place, not duplicated.
        assert_eq!(target.ids(), vec![1, ghost_id]);
        assert_eq!(target.controller(1).unwrap().real_state, b"r1");
        assert_eq!(target.controller(1).unwrap().user_arg, 4);
        assert_eq!(target.controller(ghost_id).unwrap().real_state, b"r2");
        assert_eq!(target.controller(ghost_id).unwrap().ghost_state, b"g2");
        assert_eq!(
            target.history,
            vec![
                ControllerEvent::ReadReal { id: 1 },
                ControllerEvent::ReadReal { id: ghost_id },
            ],
            "startReal is deferred to startReals"
        );

        // The continued allocation on the far side picks up where the
        // source left off.
        let mut out2 = Vec::new();
        let next = target.add_controller(Controller::new(4, DOMAIN_REAL), 0, 9, &[], &mut out2);
        assert_eq!(next, 4);

        // A truncated stream reports the error.
        let mut bad = BinaryStream::new();
        bad.write_u16(3);
        bad.rewind();
        assert!(Controllers::new().read_reals_from_stream(&mut bad).is_err());
    }

    /// The ghost streaming roundtrip (`controllers.cpp:101-141`,
    /// `readGhostsFromStream`): count, id, type, user arg, payload — and a
    /// repeated id keeps the original record.
    #[test]
    fn ghost_stream_roundtrip_restores_and_keeps_originals() {
        let mut source = Controllers::new();
        let mut out = Vec::new();
        source.add_controller(
            Controller::new(9, DOMAIN_GHOST_AND_REAL),
            1,
            12,
            &[],
            &mut out,
        );
        source.controller_mut(1).unwrap().ghost_state = b"one".to_vec();
        let ghost2 = source.add_controller(
            Controller::new(10, DOMAIN_GHOST),
            2,
            12,
            &haunts(),
            &mut out,
        );
        source.controller_mut(ghost2).unwrap().ghost_state = b"two".to_vec();

        let mut stream = BinaryStream::new();
        source.write_ghosts_to_stream(&mut stream);
        stream.rewind();

        let mut target = Controllers::new();
        target.read_ghosts_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());
        assert_eq!(target.ids(), vec![1, ghost2]);
        assert_eq!(target.controller(1).unwrap().ghost_state, b"one");
        assert_eq!(target.controller(1).unwrap().user_arg, 1);
        assert_eq!(target.controller(ghost2).unwrap().ghost_state, b"two");
        assert_eq!(
            target.history,
            vec![
                ControllerEvent::ReadGhost { id: 1 },
                ControllerEvent::StartGhost { id: 1 },
                ControllerEvent::ReadGhost { id: ghost2 },
                ControllerEvent::StartGhost { id: ghost2 },
            ]
        );

        // A repeated ghost stream for a live id keeps the stored record:
        // the newly constructed controller takes the payload and is
        // dropped with it — the C++ insert no-ops, so nothing stale
        // replaces the live controller and no events fire for it.
        let mut stale = BinaryStream::new();
        stale.write_u16(1);
        stale.write_u16(1);
        stale.write_u16(99);
        stale.write_i32(77);
        stale.write_blob(b"stale").expect("u32 length fits");
        stale.rewind();
        target.history.clear();
        target.read_ghosts_from_stream(&mut stale).unwrap();
        assert!(stale.at_end());
        let live = target.controller(1).unwrap();
        assert_eq!(live.type_id, 9);
        assert_eq!(live.user_arg, 1);
        assert_eq!(live.ghost_state, b"one");
        assert!(
            target.history.is_empty(),
            "no events for the discarded object"
        );

        // A repeated create for a live id keeps the original record — the
        // C++ insert no-ops, so nothing stale replaces the live controller.
        assert!(!target.create_ghost(1, 99, 77, b"stale".to_vec()));
        let live = target.controller(1).unwrap();
        assert_eq!(live.type_id, 9);
        assert_eq!(live.user_arg, 1);
        assert_eq!(live.ghost_state, b"one");

        // But a fresh id stores and starts.
        assert!(target.create_ghost(ghost2 + 1, 11, 3, b"new".to_vec()));
        assert_eq!(target.controller(ghost2 + 1).unwrap().ghost_state, b"new");
        assert_eq!(
            *target.history.last().unwrap(),
            ControllerEvent::StartGhost { id: ghost2 + 1 }
        );

        // updateGhost replaces the payload; deleteGhost removes.
        assert!(target.update_ghost(1, b"fresh".to_vec()));
        assert_eq!(target.controller(1).unwrap().ghost_state, b"fresh");
        assert!(!target.update_ghost(999, b"nope".to_vec()));
        out.clear();
        assert!(target.delete_ghost(1, 12, &mut out));
        assert!(out.is_empty(), "the ghost side sends no haunt traffic");
        assert!(!target.delete_ghost(1, 12, &mut out));
        assert_eq!(target.missed_deletes, vec![1]);
        assert_eq!(target.controller(1).map(|c| c.domain), None);

        // A truncated ghost stream reports the error.
        let mut bad = BinaryStream::new();
        bad.write_u16(1);
        bad.rewind();
        assert!(
            Controllers::new()
                .read_ghosts_from_stream(&mut bad)
                .is_err()
        );
    }

    /// The `Controllers` destructor face (`controllers.cpp:42-55`): only
    /// ghosts should remain — they are stopped and disowned.
    #[test]
    fn destroy_stops_the_surviving_ghosts() {
        let mut controllers = Controllers::new();
        let mut out = Vec::new();
        let ghost =
            controllers.add_controller(Controller::new(1, DOMAIN_GHOST), 0, 3, &haunts(), &mut out);
        // A real entity turns ghost first, so only the ghost half remains.
        controllers.stop_reals(false);

        controllers.destroy();
        assert!(controllers.is_empty());
        let tail: Vec<&ControllerEvent> = controllers.history.iter().rev().take(2).collect();
        assert_eq!(
            tail,
            vec![
                &ControllerEvent::Disowned { id: ghost },
                &ControllerEvent::StopGhost { id: ghost },
            ]
        );
    }

    /// `EntityExtra::Instance` (`entity_extra.hpp:47-86`): touch creates
    /// once, clear drops it, a re-touch constructs fresh, `relocated`
    /// reaches only the live slots, and entity destruction clears all.
    #[test]
    fn entity_extras_touch_clear_and_relocate_without_leaving_stale_slots() {
        let mut extras = EntityExtras::default();
        let movement = extras.register_extra_type();
        let trap = extras.register_extra_type();
        assert_eq!(extras.type_count(), 2);

        assert!(!extras.exists(movement));
        let slot = extras.instance(movement, || ExtraSlot {
            name: "Movement".to_owned(),
            data: b"state-a".to_vec(),
            relocations: 0,
        });
        assert_eq!(slot.name, "Movement");
        assert_eq!(slot.data, b"state-a");
        assert!(extras.exists(movement));
        assert!(!extras.exists(trap));

        // Re-touch returns the same instance — the constructor ran once, so
        // the fresh-construction record never lands.
        let slot = extras.instance(movement, || ExtraSlot {
            name: "WRONG".to_owned(),
            data: b"reconstructed".to_vec(),
            relocations: 99,
        });
        assert_eq!(slot.name, "Movement", "no reconstruction happened");
        assert_eq!(slot.data, b"state-a");
        slot.data = b"state-b".to_vec();
        assert_eq!(extras.slot(movement).unwrap().data, b"state-b");

        // relocated() reaches live slots only.
        extras.relocated();
        assert_eq!(extras.slot(movement).unwrap().relocations, 1);

        // Clear: delete and NULL; a second clear is a no-op.
        assert!(extras.clear(movement));
        assert!(!extras.exists(movement));
        assert!(!extras.clear(movement));
        assert_eq!(extras.slot(movement).map(|s| s.relocations), None);

        // A re-touch constructs a fresh instance — no stale state.
        let slot = extras.instance(movement, || ExtraSlot {
            name: "Movement".to_owned(),
            data: b"fresh".to_vec(),
            relocations: 0,
        });
        assert_eq!(slot.data, b"fresh");
        assert_eq!(slot.relocations, 0);

        // Entity destruction clears every slot (twice, as the reference
        // does at destroy and ~Entity) and none come back.
        extras.destroy_extras();
        assert!(!extras.exists(movement));
        assert!(!extras.exists(trap));
        extras.destroy_extras();
        assert_eq!(extras.type_count(), 2, "the type registry survives");
    }

    /// The `Entity` reuse face (`entity.cpp:1288-1306`): a brand-new
    /// collection plus cleared extras — repeated registration across
    /// reuses starts from a clean slate, the id counter included.
    #[test]
    fn entity_reuse_replaces_the_whole_collection() {
        let mut controllers = Controllers::new();
        let mut out = Vec::new();
        for _ in 0..5 {
            controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 2, &[], &mut out);
        }
        assert_eq!(controllers.last_allocated_id(), 5);

        // Entity init: delete pControllers_; pControllers_ = new
        // Controllers.
        let mut controllers = Controllers::new();
        assert_eq!(controllers.last_allocated_id(), 0);
        assert!(controllers.is_empty());
        let fresh_id =
            controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 2, &[], &mut out);
        assert_eq!(fresh_id, 1, "the counter restarts at zero");

        // And the extras are cleared the same way.
        let mut extras = EntityExtras::default();
        let eeid = extras.register_extra_type();
        extras.instance(eeid, || ExtraSlot {
            name: "Trap".to_owned(),
            data: Vec::new(),
            relocations: 0,
        });
        extras.destroy_extras();
        assert!(!extras.exists(eeid));
    }
}
