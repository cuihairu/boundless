//! Entity destruction (`Entity::destroy`, `entity.cpp:2896-3010`, plus
//! `Entity::destroyZombie` and `Cell::entityDestroyed`).
//!
//! Destroying a real runs one fixed teardown order: the `onDestroy`
//! callback, the cell-data hand-off to the base, `setDestroyed`, the
//! per-haunt del-ghost messages, `Cell::entityDestroyed`, then
//! `convertRealToGhost`'s destroy branch — which is where the real half
//! dies and `Controllers::stopReals(isDestroyed())` final-stops every
//! real-domain controller (`~RealEntity`, `real_entity.cpp:237-256`).
//! The common tail then deletes the controllers outright
//! (`~Controllers`, `controllers.cpp:29-44`), removes the entity from
//! the space, deletes every entity extra (the vehicle extra's destructor
//! forces its riders off), and drops the replay record.
//!
//! The BW-08-02-06 acceptance is "实体状态和活动控制器正确清理": after
//! `destroy` the population record is destroyed, the real record and its
//! collection slot are gone, every controller has been stopped in the
//! right order with the right final-stop flag, and the extras are empty.

use std::collections::BTreeSet;

use crate::cellappmgr::{Addr, SpaceId};
use crate::controllers::{Controllers, EntityExtras};
use crate::entity_dispatch::EntityId;
use crate::real_entity::CellEntities;

/// What `Entity::destroy` did, in firing order. Controller starts/stops
/// are not repeated here — they land on `Controllers::history`
/// (`ControllerEvent`) and the two logs read interleaved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DestroyEvent {
    /// `entity_.callback( name )` — `"onDestroy"` on the real branch,
    /// `"onGhostDestroyed"` on the ghost branch.
    Callback(EntityId, &'static str),
    /// `BaseAppIntInterface::cellEntityLost` with the CELL_DATA dict and
    /// the position/direction/space triple (`sendCellEntityLostToBase`,
    /// `entity.cpp:3267-3305`; the dict face is the entitydef lane).
    CellEntityLost {
        /// The entity the base is losing.
        entity_id: EntityId,
        /// `bundle << this->position() << ... << this->space().id()`.
        space_id: SpaceId,
    },
    /// The ungraceful fallback (`entity.cpp:2908-2917`): the dict or the
    /// stream failed, so only `setClientArgs` + `cellEntityLost` go out.
    CellEntityLostUngraceful(EntityId),
    /// `RealEntity::deleteGhosts` — one del-ghost per haunt
    /// (`real_entity.cpp:540-548`).
    DelGhost {
        /// The entity whose ghost is going away.
        entity_id: EntityId,
        /// The app that hosts the ghost.
        haunt: Addr,
    },
    /// `~RealEntity`: `delete pWitness_` (`real_entity.cpp:244-250`).
    WitnessDeleted(EntityId),
    /// A cell-only entity returns its id to the recycled pool
    /// (`shouldReturnID_`, `entity.cpp:2932-2936`).
    ReturnId(EntityId),
    /// `population_.rememberRealChannel( id_, *pRealChannel_ )` — the
    /// ghost branch keeps where the real was, for late messages
    /// (`entity.cpp:2946-2951`).
    RealChannelRemembered {
        /// The destroyed entity's id.
        entity_id: EntityId,
        /// The remembered real address.
        addr: Addr,
    },
    /// `population_.forgetRealChannel( id_ )`
    /// (`entity.cpp:2941-2942`).
    ForgetRealChannel(EntityId),
    /// `delete extras_[i]` — every extra's destructor body ran here (the
    /// vehicle extra's `~Passengers` forces its riders off), then the
    /// slots cleared (`entity.cpp:2984-2989`).
    ExtrasDeleted(EntityId),
    /// `pSpace_->removeEntity( this )` (+ the chunk bookkeeping fold,
    /// `entity.cpp:2970-2981`).
    SpaceRemoval(EntityId),
    /// `cell.pReplayData()->deleteEntity( id_ )`
    /// (`entity.cpp:3002-3005`).
    ReplayDeleted(EntityId),
    /// An `ERROR_MSG`/`WARNING_MSG` face.
    Warning(&'static str),
}

/// The per-entity facts `Entity::destroy` branches on. The caller reads
/// them off its own entity record; the machine never reaches back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityDestroyState {
    /// The entity being destroyed.
    pub id: EntityId,
    /// `this->isReal()`.
    pub is_real: bool,
    /// `this->hasBase()` — the base mailbox is set.
    pub has_base: bool,
    /// `sendCellEntityLostToBase` succeeded (the CELL_DATA dict and the
    /// stream both built).
    pub cell_entity_lost_ok: bool,
    /// `this->space().id()` — the cellEntityLost payload face.
    pub space_id: SpaceId,
    /// `pRealChannel_` on the ghost branch (`entity.cpp:2946-2951`).
    pub real_channel: Option<Addr>,
    /// `cell.pReplayData() != NULL`.
    pub in_replay: bool,
}

/// The teardown seams `Entity::destroy` reaches outside its own state:
/// the space bookkeeping and the per-extra destructor bodies. The
/// vehicle cascade (`~Passengers` forcing every rider off) is the
/// `delete_extra` body in the reference; pass the passengers world's
/// `vehicle_destroyed` here.
pub struct DestroyFaces<'a> {
    /// `pSpace_->removeEntity( this )`; the chunk removal folds in.
    pub remove_from_space: &'a mut dyn FnMut(EntityId) -> bool,
    /// One extra destructor's body, run before the slots clear.
    pub delete_extra: &'a mut dyn FnMut(EntityId),
}

/// The destroy machine: `Entity::inDestroy_` re-entry bookkeeping plus
/// the `Entity::destroy` order.
#[derive(Default)]
pub struct EntityDestroyer {
    /// `Entity::inDestroy_`, per entity, for the length of the call.
    in_destroy: BTreeSet<EntityId>,
}

impl EntityDestroyer {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Entity::destroy` (`entity.cpp:2896-3010`). Answers false on the
    /// re-entry guard.
    pub fn destroy(
        &mut self,
        state: &EntityDestroyState,
        cell: &mut CellEntities,
        controllers: &mut Controllers,
        extras: &mut EntityExtras,
        faces: &mut DestroyFaces,
        out: &mut Vec<DestroyEvent>,
    ) -> bool {
        if !self.in_destroy.insert(state.id) {
            return false;
        }
        debug_assert!(
            !cell.population().is_destroyed(state.id),
            "MF_ASSERT( !isDestroyed_ )"
        );

        // Remember the recording state for later.
        let was_real = state.is_real;
        let haunts: Vec<Addr> = cell
            .real(state.id)
            .map(|real| real.haunts.iter().map(|h| h.addr).collect())
            .unwrap_or_default();
        let has_witness = cell.real(state.id).is_some_and(|real| real.has_witness);

        if state.is_real {
            // TODO (reference): an option to skip the database information
            // when the base will not write it.
            out.push(DestroyEvent::Callback(state.id, "onDestroy"));

            if state.has_base {
                if state.cell_entity_lost_ok {
                    out.push(DestroyEvent::CellEntityLost {
                        entity_id: state.id,
                        space_id: state.space_id,
                    });
                } else {
                    // ERROR_MSG "Unable to gracefully inform base entity of
                    // destroy" — inform the base ungracefully.
                    out.push(DestroyEvent::Warning(
                        "Entity::destroy: unable to gracefully inform base entity of destroy",
                    ));
                    out.push(DestroyEvent::CellEntityLostUngraceful(state.id));
                }
            }

            // this->setDestroyed();
            cell.population().mark_destroyed(state.id);

            // pReal_->deleteGhosts();
            for haunt in &haunts {
                out.push(DestroyEvent::DelGhost {
                    entity_id: state.id,
                    haunt: *haunt,
                });
            }
            if let Some(real) = cell.real_mut(state.id) {
                real.haunts.clear();
            }

            // this->cell().entityDestroyed( this );
            if let Err(message) = cell.entity_destroyed(state.id) {
                out.push(DestroyEvent::Warning(message));
            }

            // this->convertRealToGhost() with no stream and no channel is
            // the destroy branch: destroyReal() → RealEntity::destroy(NULL)
            // → ~RealEntity, whose witness delete and
            // stopRealControllers(isFinalStop = isDestroyed() = true) land
            // here. The real record and its collection slot went with
            // entity_destroyed.
            if has_witness {
                out.push(DestroyEvent::WitnessDeleted(state.id));
            }
            controllers.stop_reals(true);

            // if this is a cell only entity then add the id to our
            // recycled id list; else the base takes care of it.
            if !state.has_base {
                out.push(DestroyEvent::ReturnId(state.id));
            }

            // This shouldn't be in there but is a sanity check.
            out.push(DestroyEvent::ForgetRealChannel(state.id));
        } else {
            out.push(DestroyEvent::Callback(state.id, "onGhostDestroyed"));

            // Remember where the ghost thought the real is in case a
            // message is received for this entity.
            if let Some(addr) = state.real_channel {
                out.push(DestroyEvent::RealChannelRemembered {
                    entity_id: state.id,
                    addr,
                });
            }
        }

        // this->setDestroyed(); — the ghost branch's mark (idempotent).
        cell.population().mark_destroyed(state.id);

        // Entity::callbacksPermitted( false ) … bw_safe_delete(
        // pControllers_ ) — ~Controllers stopGhosts and disowns whatever
        // is still attached (all ghost-domain now; the reference asserts).
        controllers.destroy();

        // pSpace_->removeEntity( this ), then the chunk bookkeeping.
        let _ = (faces.remove_from_space)(state.id);
        out.push(DestroyEvent::SpaceRemoval(state.id));

        // Get rid of any entity extras — each destructor body first (the
        // vehicle extra forces its riders off), then the slots clear.
        (faces.delete_extra)(state.id);
        extras.destroy_extras();
        out.push(DestroyEvent::ExtrasDeleted(state.id));

        if was_real && state.in_replay {
            out.push(DestroyEvent::ReplayDeleted(state.id));
        }

        // The buffered ghost messages' lifespan replay is the
        // buffered-ghost lane; nothing to record here yet.
        self.in_destroy.remove(&state.id);
        true
    }

    /// `Entity::destroyZombie` (`entity.cpp:4218-4227`): a real demotes
    /// first — `convertRealToGhost` with no stream is the destroy branch,
    /// and at that point `isDestroyed()` is still false, so the
    /// controllers' real halves final-stop with `false` — and then
    /// `destroy()` takes the ghost branch.
    pub fn destroy_zombie(
        &mut self,
        state: &EntityDestroyState,
        cell: &mut CellEntities,
        controllers: &mut Controllers,
        extras: &mut EntityExtras,
        faces: &mut DestroyFaces,
        out: &mut Vec<DestroyEvent>,
    ) -> bool {
        if state.is_real {
            // ~RealEntity's witness delete, then stopRealControllers with
            // isFinalStop = isDestroyed() = false.
            if cell.real(state.id).is_some_and(|real| real.has_witness) {
                out.push(DestroyEvent::WitnessDeleted(state.id));
            }
            controllers.stop_reals(false);
            cell.convert_real_to_ghost(state.id);
        }
        let ghost_state = EntityDestroyState {
            is_real: false,
            ..state.clone()
        };
        self.destroy(&ghost_state, cell, controllers, extras, faces, out)
    }
}

// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::controllers::{
        Controller, ControllerEvent, DOMAIN_GHOST, DOMAIN_GHOST_AND_REAL, DOMAIN_REAL, ExtraSlot,
    };
    use crate::real_entity::{CreateEntityOutcome, CreateRealRequest};
    use crate::space::Space;

    const HAUNT_A: Addr = (0x0a00_0001, 3001);
    const HAUNT_B: Addr = (0x0a00_0002, 3002);
    const BASE: Addr = (0x0a00_0009, 4000);

    fn base_state(id: EntityId) -> EntityDestroyState {
        EntityDestroyState {
            id,
            is_real: true,
            has_base: true,
            cell_entity_lost_ok: true,
            space_id: 7,
            real_channel: None,
            in_replay: true,
        }
    }

    /// A real with a base, two haunts, a witness, replay data, and three
    /// controllers: a real-only movement controller, a both-domain one,
    /// and a ghost-only one. The teardown runs in the reference order and
    /// every structure comes out clean.
    #[test]
    fn destroying_a_real_runs_the_reference_teardown_order() {
        let mut space = Space::new(7, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(5u32);

        let outcome = cell.create_entity(
            &mut space,
            CreateRealRequest::default(),
            BASE,
            &mut alloc,
            100,
            7,
            &mut out,
        );
        assert!(matches!(
            outcome,
            CreateEntityOutcome::Created { id: 5, .. }
        ));
        out.clear();

        let mut controllers = Controllers::new();
        let mut ghost_out = Vec::new();
        let haunts = vec![HAUNT_A, HAUNT_B];
        controllers.add_controller(
            Controller::new(1, DOMAIN_REAL),
            0,
            5,
            &haunts,
            &mut ghost_out,
        );
        controllers.add_controller(
            Controller::new(2, DOMAIN_GHOST_AND_REAL),
            0,
            5,
            &haunts,
            &mut ghost_out,
        );
        controllers.add_controller(
            Controller::new(3, DOMAIN_GHOST),
            0,
            5,
            &haunts,
            &mut ghost_out,
        );
        ghost_out.clear();
        controllers.history.clear();

        // The real record carries the witness and the two haunts.
        let real = cell.real_mut(5).unwrap();
        real.has_witness = true;
        real.haunts = vec![
            crate::real_entity::Haunt {
                addr: HAUNT_A,
                creation_time: 100,
            },
            crate::real_entity::Haunt {
                addr: HAUNT_B,
                creation_time: 101,
            },
        ];

        let mut extras = EntityExtras::default();
        let vehicle_eeid = extras.register_extra_type();
        extras.instance(vehicle_eeid, || ExtraSlot {
            name: "Passengers".to_owned(),
            data: Vec::new(),
            relocations: 0,
        });

        let mut space_removed = Vec::new();
        let mut extra_bodies = Vec::new();
        let mut faces = DestroyFaces {
            remove_from_space: &mut |id: EntityId| {
                space_removed.push(id);
                let mut space_out = crate::space::SpaceOutbound::default();
                space.remove_entity(id, &crate::space::CellAppContext::default(), &mut space_out)
            },
            delete_extra: &mut |id: EntityId| extra_bodies.push(id),
        };

        let mut destroyer = EntityDestroyer::new();
        let mut events = Vec::new();
        assert!(destroyer.destroy(
            &base_state(5),
            &mut cell,
            &mut controllers,
            &mut extras,
            &mut faces,
            &mut events,
        ));

        // The teardown order: callback, base hand-off, del-ghost per
        // haunt, witness, forgetRealChannel, space, extras, replay.
        assert_eq!(
            events,
            vec![
                DestroyEvent::Callback(5, "onDestroy"),
                DestroyEvent::CellEntityLost {
                    entity_id: 5,
                    space_id: 7
                },
                DestroyEvent::DelGhost {
                    entity_id: 5,
                    haunt: HAUNT_A
                },
                DestroyEvent::DelGhost {
                    entity_id: 5,
                    haunt: HAUNT_B
                },
                DestroyEvent::WitnessDeleted(5),
                DestroyEvent::ForgetRealChannel(5),
                DestroyEvent::SpaceRemoval(5),
                DestroyEvent::ExtrasDeleted(5),
                DestroyEvent::ReplayDeleted(5),
            ]
        );

        // No cell-only id return with a base present.
        assert!(!events.contains(&DestroyEvent::ReturnId(5)));

        // The controllers: the real halves final-stopped (the both-domain
        // one disowns nothing here), then ~Controllers ghosts out the
        // survivors. Id order: 1 real-only, 2 both-domain, 3 ghost-only.
        assert_eq!(
            &controllers.history[..],
            &[
                ControllerEvent::StopReal {
                    id: 1,
                    final_stop: true
                },
                ControllerEvent::Disowned { id: 1 },
                ControllerEvent::StopReal {
                    id: 2,
                    final_stop: true
                },
                ControllerEvent::StopGhost { id: 2 },
                ControllerEvent::Disowned { id: 2 },
                ControllerEvent::StopGhost { id: 3 },
                ControllerEvent::Disowned { id: 3 },
            ]
        );
        assert!(controllers.is_empty());

        // The entity state: population destroyed, real record and
        // collection slot gone, haunts not lingering, extras empty.
        assert!(cell.population().is_destroyed(5));
        assert!(cell.reals().is_empty());
        assert_eq!(cell.collection_size(), 0);
        assert!(extras.slot(vehicle_eeid).is_none());

        // The seams: the space closure ran (the space actually emptied),
        // and the extra destructor body ran once.
        assert_eq!(space_removed, vec![5]);
        assert_eq!(space.entities().len(), 0);
        assert_eq!(extra_bodies, vec![5]);
    }

    /// A cell-only real: no base hand-off at all, and its id returns to
    /// the recycled pool.
    #[test]
    fn a_cell_only_real_recycles_its_id_and_skips_the_base() {
        let mut space = Space::new(3, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(6u32);
        cell.create_entity(
            &mut space,
            CreateRealRequest::default(),
            (0, 0),
            &mut alloc,
            100,
            3,
            &mut out,
        );
        out.clear();

        let mut controllers = Controllers::new();
        let mut ghost_out = Vec::new();
        controllers.add_controller(Controller::new(1, DOMAIN_REAL), 0, 6, &[], &mut ghost_out);
        controllers.history.clear();

        let mut extras = EntityExtras::default();
        let mut no_space: Vec<EntityId> = Vec::new();
        let mut no_extras: Vec<EntityId> = Vec::new();
        let mut faces = DestroyFaces {
            remove_from_space: &mut |id: EntityId| {
                no_space.push(id);
                let mut space_out = crate::space::SpaceOutbound::default();
                space.remove_entity(id, &crate::space::CellAppContext::default(), &mut space_out)
            },
            delete_extra: &mut |id: EntityId| no_extras.push(id),
        };

        let mut destroyer = EntityDestroyer::new();
        let mut events = Vec::new();
        let state = EntityDestroyState {
            id: 6,
            is_real: true,
            has_base: false,
            ..EntityDestroyState::default()
        };
        assert!(destroyer.destroy(
            &state,
            &mut cell,
            &mut controllers,
            &mut extras,
            &mut faces,
            &mut events
        ));

        assert_eq!(
            events,
            vec![
                DestroyEvent::Callback(6, "onDestroy"),
                DestroyEvent::ReturnId(6),
                DestroyEvent::ForgetRealChannel(6),
                DestroyEvent::SpaceRemoval(6),
                DestroyEvent::ExtrasDeleted(6),
            ]
        );
        assert_eq!(
            controllers.history,
            vec![
                ControllerEvent::StopReal {
                    id: 1,
                    final_stop: true
                },
                ControllerEvent::Disowned { id: 1 },
            ]
        );
        assert!(cell.population().is_destroyed(6));
        assert!(cell.reals().is_empty());
    }

    /// The ungraceful fallback: a failed CELL_DATA stream sends only the
    /// bare `cellEntityLost`.
    #[test]
    fn the_ungraceful_fallback_replaces_the_lost_message() {
        let mut space = Space::new(1, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(2u32);
        cell.create_entity(
            &mut space,
            CreateRealRequest::default(),
            (0, 0),
            &mut alloc,
            100,
            1,
            &mut out,
        );
        out.clear();

        let mut controllers = Controllers::new();
        let mut extras = EntityExtras::default();
        let mut noop_space = |_: EntityId| true;
        let mut noop_extra = |_: EntityId| ();
        let mut faces = DestroyFaces {
            remove_from_space: &mut noop_space,
            delete_extra: &mut noop_extra,
        };

        let mut destroyer = EntityDestroyer::new();
        let mut events = Vec::new();
        let state = EntityDestroyState {
            id: 2,
            is_real: true,
            has_base: true,
            cell_entity_lost_ok: false,
            space_id: 1,
            ..EntityDestroyState::default()
        };
        assert!(destroyer.destroy(
            &state,
            &mut cell,
            &mut controllers,
            &mut extras,
            &mut faces,
            &mut events
        ));

        assert_eq!(
            events,
            vec![
                DestroyEvent::Callback(2, "onDestroy"),
                DestroyEvent::Warning(
                    "Entity::destroy: unable to gracefully inform base entity of destroy"
                ),
                DestroyEvent::CellEntityLostUngraceful(2),
                DestroyEvent::ForgetRealChannel(2),
                DestroyEvent::SpaceRemoval(2),
                DestroyEvent::ExtrasDeleted(2),
            ]
        );
    }

    /// A ghost destroy takes the ghost branch: `onGhostDestroyed`, the
    /// remembered real channel, and the common tail — no del-ghosts, no
    /// id return, no forgetRealChannel.
    #[test]
    fn destroying_a_ghost_runs_the_ghost_branch() {
        let mut cell = CellEntities::new();
        cell.population().insert(9, false, None);

        let mut controllers = Controllers::new();
        let mut ghost_out = Vec::new();
        controllers.add_controller(Controller::new(4, DOMAIN_GHOST), 0, 9, &[], &mut ghost_out);
        controllers.history.clear();

        let mut extras = EntityExtras::default();
        let mut removed: Vec<EntityId> = Vec::new();
        let mut bodies: Vec<EntityId> = Vec::new();
        let mut faces = DestroyFaces {
            remove_from_space: &mut |id: EntityId| {
                removed.push(id);
                true
            },
            delete_extra: &mut |id: EntityId| bodies.push(id),
        };

        let mut destroyer = EntityDestroyer::new();
        let mut events = Vec::new();
        let state = EntityDestroyState {
            id: 9,
            is_real: false,
            real_channel: Some(HAUNT_A),
            ..EntityDestroyState::default()
        };
        assert!(destroyer.destroy(
            &state,
            &mut cell,
            &mut controllers,
            &mut extras,
            &mut faces,
            &mut events
        ));

        assert_eq!(
            events,
            vec![
                DestroyEvent::Callback(9, "onGhostDestroyed"),
                DestroyEvent::RealChannelRemembered {
                    entity_id: 9,
                    addr: HAUNT_A
                },
                DestroyEvent::SpaceRemoval(9),
                DestroyEvent::ExtrasDeleted(9),
            ]
        );
        assert_eq!(
            controllers.history,
            vec![
                ControllerEvent::StopGhost { id: 1 },
                ControllerEvent::Disowned { id: 1 },
            ]
        );
        assert!(cell.population().is_destroyed(9));
        assert_eq!(removed, vec![9]);
        assert_eq!(bodies, vec![9]);
    }

    /// `destroyZombie`: the real demotes first — controllers final-stop
    /// with `false`, the witness goes, the real record leaves — and the
    /// destroy itself then takes the ghost branch.
    #[test]
    fn destroy_zombie_demotes_the_real_before_the_ghost_branch() {
        let mut space = Space::new(4, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(8u32);
        cell.create_entity(
            &mut space,
            CreateRealRequest::default(),
            (0, 0),
            &mut alloc,
            100,
            4,
            &mut out,
        );
        out.clear();
        cell.real_mut(8).unwrap().has_witness = true;

        let mut controllers = Controllers::new();
        let mut ghost_out = Vec::new();
        let haunts = vec![HAUNT_A];
        controllers.add_controller(
            Controller::new(1, DOMAIN_GHOST_AND_REAL),
            0,
            8,
            &haunts,
            &mut ghost_out,
        );
        ghost_out.clear();
        controllers.history.clear();

        let mut extras = EntityExtras::default();
        let mut removed: Vec<EntityId> = Vec::new();
        let mut bodies: Vec<EntityId> = Vec::new();
        let mut faces = DestroyFaces {
            remove_from_space: &mut |id: EntityId| {
                removed.push(id);
                true
            },
            delete_extra: &mut |id: EntityId| bodies.push(id),
        };

        let mut destroyer = EntityDestroyer::new();
        let mut events = Vec::new();
        let state = EntityDestroyState {
            id: 8,
            is_real: true,
            real_channel: Some(BASE),
            ..EntityDestroyState::default()
        };
        assert!(destroyer.destroy_zombie(
            &state,
            &mut cell,
            &mut controllers,
            &mut extras,
            &mut faces,
            &mut events
        ));

        // The demotion ran first (witness + the not-final stop), then the
        // ghost branch of destroy.
        assert_eq!(
            events,
            vec![
                DestroyEvent::WitnessDeleted(8),
                DestroyEvent::Callback(8, "onGhostDestroyed"),
                DestroyEvent::RealChannelRemembered {
                    entity_id: 8,
                    addr: BASE
                },
                DestroyEvent::SpaceRemoval(8),
                DestroyEvent::ExtrasDeleted(8),
            ]
        );
        // isDestroyed() was still false at the demotion: not a final stop.
        // The both-domain controller then ghost-stops at ~Controllers.
        assert_eq!(
            controllers.history,
            vec![
                ControllerEvent::StopReal {
                    id: 1,
                    final_stop: false
                },
                ControllerEvent::StopGhost { id: 1 },
                ControllerEvent::Disowned { id: 1 },
            ]
        );
        assert!(cell.population().is_destroyed(8));
        assert!(cell.reals().is_empty());
        assert_eq!(cell.collection_size(), 0);
    }

    /// `Cell::entityDestroyed` (`cell.cpp:287-312`): destroying a living
    /// entity is refused, and after the removal the record is gone.
    #[test]
    fn entity_destroyed_refuses_living_entities_and_ghosts() {
        let mut space = Space::new(1, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(3u32);
        cell.create_entity(
            &mut space,
            CreateRealRequest::default(),
            (0, 0),
            &mut alloc,
            100,
            1,
            &mut out,
        );

        // Still alive: the murder guard fires.
        assert_eq!(
            cell.entity_destroyed(3),
            Err("Cell::entityDestroyed: someone tried to murder a living entity")
        );

        cell.population().mark_destroyed(3);
        assert_eq!(cell.entity_destroyed(3), Ok(()));
        assert!(cell.reals().is_empty());
        assert_eq!(cell.collection_size(), 0);

        // A destroyed id with no real record is the ghost misuse face.
        cell.population().insert(11, false, None);
        cell.population().mark_destroyed(11);
        assert_eq!(
            cell.entity_destroyed(11),
            Err("Cell::entityDestroyed: called on ghost entity")
        );
    }
}
