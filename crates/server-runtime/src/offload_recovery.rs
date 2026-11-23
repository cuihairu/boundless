//! BW-08-05-06: the migration target loses contact and recovers.
//!
//! The entity has just been offloaded to a cell when that cell's CellApp dies.
//! The population forgets the dead channel (`EntityPopulation::handleCellAppDeath`,
//! `entity_population.cpp:255`): it removes the channel from both the current and
//! the previous tick's lists, so `findRealChannel` comes back empty and a call
//! that would be forwarded to the Real is failed to its sender
//! (`message_handlers.cpp:207-211`).
//!
//! Recovery runs on the BaseApp side: the bases of the affected entities are
//! notified (`notifyBasesOfCellAppDeath`, `entity_population.cpp:234`, which also
//! carries the recent teleport data for entities whose ghosts the teleport already
//! destroyed), the entity is restored onto a new cell, and the new Real sends
//! `ghostSetReal` with the advanced subsequence id, so every remaining Ghost
//! re-points at it (`entity.cpp:4927`).
//!
//! The acceptance, "角色及消息状态对照通过", is the reconciliation: exactly one
//! Real, locatable from every observation point, and every call that could not
//! reach the lost Real is re-issued once to the restored one.
//!
//! Reference:
//! - `entity_population.cpp:255` `handleCellAppDeath` — forget the dead channel
//!   from `currChannels_` and `prevChannels_`.
//! - `entity_population.cpp:267` `forgetRealChannel` — erase every mapping whose
//!   channel address is the dead app.
//! - `entity_population.cpp:160-183` `findRealChannel` — the current tick's
//!   channel and then the previous tick's; empty after the forget.
//! - `message_handlers.cpp:192-216` `DISPOSITION_FORWARD_TO_REAL` — a NULL
//!   channel drops the message and calls `sendFailure` to the sender.
//! - `entity_population.cpp:234` `notifyBasesOfCellAppDeath` — the bases of the
//!   affected entities are told, including the recently teleported ones.
//! - `entity.cpp:4927` `ghostSetReal` — the restored Real's re-pointing,
//!   validated against the monotonic subsequence id.
//! - `real_entity.cpp:265-283` `RealEntity::destroy` — the sign-off that tells the
//!   remaining ghosts about the pending Real.

use std::collections::BTreeSet;

use crate::ghost_data::Address;
use crate::message_redirect::Call;
use crate::offload_role::{CellView, EntityRole, SwitchOutcome};

/// A call that could not reach the lost Real: it was failed to its sender and
/// is buffered for re-issue after the restore
/// (`sendFailure`, `message_handlers.cpp:207-211`).
pub type PendingCall = Call;

/// Where one call landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// The calling cell holds the Real: the handler runs there.
    Dispatched,
    /// Re-issued on the Real's channel (`forwardMessageToReal`).
    Forwarded(Address),
    /// The Real's channel is gone: the call is failed to its sender and buffered.
    Failed(Address),
}

/// One answer delivered after the restore.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    /// The caller the answer was sent to.
    pub sender: Address,
    /// The message id answered.
    pub message_id: u8,
    /// The cell that answered.
    pub answered_by: Address,
}

/// What `MigrationRecovery::restore` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// The entity was restored onto `cell` and its calls re-issued.
    Restored(Address),
    /// No cell has died, so there is nothing to restore.
    NothingLost,
    /// The restore would put the entity back onto the dead cell itself.
    DeadCell,
}

/// The reconciliation the acceptance asks for: the role state and the message
/// state agree after a loss and a recovery.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconciliation {
    /// The unique Real (`EntityRole::real_cell`).
    pub real_cell: Option<Address>,
    /// Every observation point locates that same Real.
    pub all_points_locate_it: bool,
    /// The population still knows the Real's channel, so `findRealChannel`
    /// (`entity_population.cpp:160-183`) resolves it to that Real.
    pub real_channel_known: bool,
    /// No buffered call is left outstanding.
    pub no_call_outstanding: bool,
    /// Every buffered call was re-issued exactly once.
    pub one_answer_per_call: bool,
}

impl Reconciliation {
    /// The acceptance: "角色及消息状态对照通过".
    pub fn passes(&self) -> bool {
        self.real_cell.is_some()
            && self.all_points_locate_it
            && self.real_channel_known
            && self.no_call_outstanding
            && self.one_answer_per_call
    }
}

/// The role and message state of one entity across a migration-target death and
/// the restore that follows.
pub struct MigrationRecovery {
    /// The entity's cell views (`offload_role::EntityRole`).
    roles: EntityRole,
    /// `currChannels_`: the Real's channel for this tick.
    current_channel: Option<Address>,
    /// `prevChannels_`: the previous tick's channel
    /// (`entity_population.cpp:172`).
    previous_channel: Option<Address>,
    /// The cell whose CellApp died, when one has.
    lost_target: Option<Address>,
    /// Calls that failed because the Real was lost, still to be re-issued.
    pending: Vec<PendingCall>,
    /// The calls already forwarded, keyed by (sender, message id), so each is
    /// issued once.
    forwarded: BTreeSet<(Address, u8)>,
    /// The answers delivered after the restore.
    delivered: Vec<Delivery>,
}

impl MigrationRecovery {
    /// An entity whose Real is on `real_cell`, with the population's channel
    /// pointed at it.
    pub fn new(real_cell: Address) -> Self {
        MigrationRecovery {
            roles: EntityRole::with_real(real_cell),
            current_channel: Some(real_cell),
            previous_channel: None,
            lost_target: None,
            pending: Vec::new(),
            forwarded: BTreeSet::new(),
            delivered: Vec::new(),
        }
    }

    /// Add a Ghost on a cell.
    pub fn add_ghost(&mut self, cell: Address) {
        let real = self
            .roles
            .real_cell()
            .expect("a recovery starts with a Real");
        self.roles.add_ghost(CellView::ghost(cell, real));
    }

    /// The cell views.
    pub fn roles(&self) -> &EntityRole {
        &self.roles
    }

    /// The cell whose CellApp died, when one has.
    pub fn lost_target(&self) -> Option<Address> {
        self.lost_target
    }

    /// The calls still waiting to be re-issued.
    pub fn pending(&self) -> &[PendingCall] {
        &self.pending
    }

    /// The answers delivered after the restore, in order.
    pub fn deliveries(&self) -> &[Delivery] {
        &self.delivered
    }

    /// `Cell::offloadEntity` + `Entity::onload`: the Real moves to `new_cell`.
    pub fn switch_to(&mut self, new_cell: Address) -> SwitchOutcome {
        let old = self.roles.real_cell().expect("a Real before a switch");
        let outcome = self.roles.switch_roles(old, new_cell);
        if outcome == SwitchOutcome::Switched {
            self.current_channel = Some(new_cell);
        }
        outcome
    }

    /// `EntityPopulation::handleCellAppDeath` (`entity_population.cpp:255`): the
    /// CellApp holding `target` has died, so its channel is forgotten from the
    /// current and the previous tick's lists.
    pub fn target_lost(&mut self, target: Address) {
        // `forgetRealChannel` over both `currChannels_` and `prevChannels_`
        // (`entity_population.cpp:267`).
        if self.current_channel == Some(target) {
            self.current_channel = None;
        }
        if self.previous_channel == Some(target) {
            self.previous_channel = None;
        }
        self.lost_target = Some(target);
    }

    /// End of the tick: today's channel becomes the previous one
    /// (`prevChannels_`, `entity_population.cpp:172`), which still resolves a
    /// Real that just moved.
    pub fn tick(&mut self) {
        self.previous_channel = self.current_channel.take();
    }

    /// A call to the entity from `from_cell`. While the Real is lost the call
    /// cannot reach it: `findRealChannel` comes back empty and the forward is
    /// failed to the sender, so it is buffered for the restore.
    pub fn call(&mut self, from_cell: Address, call: &Call) -> CallState {
        let Some(view) = self.roles.views.get(&from_cell) else {
            // `pEntity ? pEntity->pRealChannel() : population().findRealChannel`
            // (`message_handlers.cpp:198-200`); after the forget there is none.
            return match self.current_channel.or(self.previous_channel) {
                Some(target) => {
                    self.forwarded.insert((call.sender, call.message_id));
                    CallState::Forwarded(target)
                }
                None => {
                    self.pending.push(call.clone());
                    CallState::Failed(call.sender)
                }
            };
        };

        if view.is_real() {
            return CallState::Dispatched;
        }

        // A Ghost forwards to its Real's channel
        // (`forwardMessageToReal`, `entity.cpp:2278`).
        let target = view.route_to();
        if self.lost_target == Some(target) {
            // The channel is dead: the call never lands, and the sender is told
            // so it can retry once the Real is restored.
            self.pending.push(call.clone());
            return CallState::Failed(call.sender);
        }
        self.forwarded.insert((call.sender, call.message_id));
        CallState::Forwarded(target)
    }

    /// The restore: the BaseApp side puts the entity back onto a new cell, which
    /// becomes the Real again (`Base::restoreTo`, `base.cpp:3819`). The old Real's
    /// cell is gone, so its view is dropped, and the remaining Ghosts are
    /// re-pointed at the new Real with the advanced subsequence id.
    pub fn restore(&mut self, new_cell: Address) -> RestoreOutcome {
        let Some(dead) = self.lost_target else {
            return RestoreOutcome::NothingLost;
        };
        if new_cell == dead {
            // The cell is gone; the restore must land on a surviving cell.
            return RestoreOutcome::DeadCell;
        }

        // The dead Real's subsequence id, then the dead cell's view is gone.
        let old_count = self
            .roles
            .views
            .get(&dead)
            .map(|view| view.num_times_real_offloaded)
            .unwrap_or(0);
        self.roles.views.remove(&dead);

        // The new cell holds the Real again (`++numTimesRealOffloaded_`,
        // `convertGhostToReal`, `entity.cpp:4753`).
        let count = old_count + 1;
        match self.roles.views.get_mut(&new_cell) {
            Some(view) => {
                view.role = crate::offload_role::Role::Real;
                view.real_address = new_cell;
                view.next_real_address = None;
                view.num_times_real_offloaded = count;
            }
            None => {
                // The entity is created fresh on the new cell.
                let mut view = CellView::real(new_cell);
                view.num_times_real_offloaded = count;
                self.roles.add_ghost(view);
            }
        }
        self.current_channel = Some(new_cell);
        self.previous_channel = None;
        self.lost_target = None;

        // The restored Real signs the pending ghosts off and sends `ghostSetReal`
        // (`real_entity.cpp:265-283`, `entity.cpp:4927`).
        for view in self.roles.views.values_mut() {
            if view.role == crate::offload_role::Role::Ghost {
                view.set_next_real(new_cell);
                view.set_real(new_cell, count);
            }
        }

        // Re-issue every buffered call to the restored Real. A buffered call was
        // never forwarded, so each is delivered exactly once.
        for call in std::mem::take(&mut self.pending) {
            self.delivered.push(Delivery {
                sender: call.sender,
                message_id: call.message_id,
                answered_by: new_cell,
            });
        }

        RestoreOutcome::Restored(new_cell)
    }

    /// The reconciliation: does the role state and the message state agree?
    pub fn reconcile(&self) -> Reconciliation {
        let real_cell = self.roles.real_cell();
        let all_points_locate_it = real_cell.is_some_and(|real| {
            self.roles
                .views
                .keys()
                .all(|cell| self.roles.locate_real(*cell) == Some(real))
        });
        let real_channel_known = self.current_channel.or(self.previous_channel) == real_cell;
        let no_call_outstanding = self.pending.is_empty();
        let one_answer_per_call = self.delivered.iter().all(|delivery| {
            self.delivered
                .iter()
                .filter(|other| {
                    other.sender == delivery.sender && other.message_id == delivery.message_id
                })
                .count()
                == 1
        });
        Reconciliation {
            real_cell,
            all_points_locate_it,
            real_channel_known,
            no_call_outstanding,
            one_answer_per_call,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a() -> Address {
        Address(1)
    }
    fn b() -> Address {
        Address(2)
    }
    fn c() -> Address {
        Address(3)
    }
    fn caller() -> Address {
        Address(9)
    }

    fn a_call(message_id: u8) -> Call {
        Call {
            message_id,
            sender: caller(),
            payload: vec![1],
        }
    }

    /// An entity with a Real on A and Ghosts on B and C.
    fn entity() -> MigrationRecovery {
        let mut recovery = MigrationRecovery::new(a());
        recovery.add_ghost(b());
        recovery.add_ghost(c());
        recovery
    }

    /// The acceptance: the migration target loses contact, and after the
    /// restore the role state and the message state agree.
    #[test]
    fn a_lost_target_recovers_to_a_consistent_role_and_message_state() {
        let mut recovery = entity();
        let call = a_call(4);

        // The Real is offloaded to B, and B's CellApp dies before it answers.
        assert_eq!(recovery.switch_to(b()), SwitchOutcome::Switched);
        recovery.target_lost(b());

        // With the channel forgotten the call cannot reach the Real.
        assert_eq!(recovery.call(caller(), &call), CallState::Failed(caller()));
        assert_eq!(recovery.pending().len(), 1, "the call is buffered");

        // The restore puts the entity back onto a new cell.
        assert_eq!(recovery.restore(c()), RestoreOutcome::Restored(c()));

        let reconciliation = recovery.reconcile();
        assert!(
            reconciliation.passes(),
            "角色及消息状态对照通过: {reconciliation:?}"
        );
        assert_eq!(reconciliation.real_cell, Some(c()));
        assert_eq!(
            recovery.deliveries(),
            &[Delivery {
                sender: caller(),
                message_id: 4,
                answered_by: c(),
            }]
        );
    }

    /// Before a death the calls forward normally and the state still reconciles.
    #[test]
    fn a_healthy_migration_reconciles_without_a_loss() {
        let mut recovery = entity();
        let call = a_call(1);

        assert_eq!(recovery.call(caller(), &call), CallState::Forwarded(a()));

        let reconciliation = recovery.reconcile();
        assert!(reconciliation.passes());
        assert_eq!(reconciliation.real_cell, Some(a()));
        assert!(reconciliation.all_points_locate_it);
    }

    /// After the loss the Real is no longer reachable, and the calls are
    /// buffered rather than answered.
    #[test]
    fn a_lost_target_buffers_its_calls_instead_of_answering() {
        let mut recovery = entity();
        let call = a_call(7);

        assert_eq!(recovery.switch_to(b()), SwitchOutcome::Switched);
        recovery.target_lost(b());
        assert_eq!(recovery.lost_target(), Some(b()));

        assert_eq!(recovery.call(caller(), &call), CallState::Failed(caller()));
        assert!(recovery.deliveries().is_empty(), "nothing is answered");
        assert!(
            !recovery.reconcile().passes(),
            "the state does not yet agree"
        );
    }

    /// The remaining Ghosts re-point at the restored Real, so the Real is
    /// locatable from every observation point again.
    #[test]
    fn the_remaining_ghosts_repoint_at_the_restored_real() {
        let mut recovery = entity();

        assert_eq!(recovery.switch_to(b()), SwitchOutcome::Switched);
        recovery.target_lost(b());
        recovery.restore(c());

        assert_eq!(recovery.roles().real_cell(), Some(c()));
        for cell in [a(), c()] {
            assert_eq!(
                recovery.roles().locate_real(cell),
                Some(c()),
                "the Real is locatable from the surviving cell {cell:?}"
            );
        }
        assert_eq!(
            recovery.roles().locate_real(b()),
            None,
            "the dead cell holds no view"
        );
    }

    /// A second restore without a death is a no-op, and restoring onto the
    /// dead cell itself is rejected.
    #[test]
    fn restoring_without_a_death_is_a_no_op() {
        let mut recovery = entity();
        assert_eq!(recovery.restore(c()), RestoreOutcome::NothingLost);
        assert_eq!(recovery.roles().real_cell(), Some(a()));
        assert!(recovery.pending().is_empty());
    }

    /// The restore may land on a cell that held no view of the entity: the
    /// entity is created fresh there, with the advanced subsequence id.
    #[test]
    fn the_restore_creates_the_entity_fresh_on_an_unknown_cell() {
        let mut recovery = entity();
        let d = Address(4);

        assert_eq!(recovery.switch_to(b()), SwitchOutcome::Switched);
        recovery.target_lost(b());

        assert_eq!(recovery.restore(d), RestoreOutcome::Restored(d));
        assert_eq!(recovery.roles().real_cell(), Some(d));
        assert!(recovery.reconcile().passes());
    }

    /// Restoring onto the dead cell is meaningless; the restore must land on a
    /// surviving cell.
    #[test]
    fn restoring_onto_the_dead_cell_is_rejected() {
        let mut recovery = entity();
        recovery.switch_to(b());
        recovery.target_lost(b());

        assert_eq!(recovery.restore(b()), RestoreOutcome::DeadCell);
        assert_eq!(
            recovery.roles().real_cell(),
            Some(b()),
            "the role still records the lost Real"
        );
        assert!(
            !recovery.reconcile().passes(),
            "the channel is forgotten, so the state does not agree"
        );
    }

    /// A call from the cell holding a Ghost re-issues on the Real's channel
    /// (`forwardMessageToReal`, `entity.cpp:2278`).
    #[test]
    fn a_call_from_a_ghost_cell_forwards_to_the_real() {
        let mut recovery = entity();

        assert_eq!(recovery.call(b(), &a_call(3)), CallState::Forwarded(a()));
        assert!(recovery.pending().is_empty());
    }

    /// While the Real is lost, a Ghost's forward lands on the dead channel:
    /// the call is failed to its sender and buffered for the restore.
    #[test]
    fn a_call_from_a_ghost_cell_fails_while_the_real_is_lost() {
        let mut recovery = entity();
        let call = a_call(5);

        assert_eq!(recovery.switch_to(b()), SwitchOutcome::Switched);
        recovery.target_lost(b());

        // The ghost on C routes to the dead Real on B.
        assert_eq!(recovery.call(c(), &call), CallState::Failed(caller()));
        assert_eq!(recovery.pending().len(), 1, "the call is buffered");
    }

    /// The previous tick's channel still resolves a Real that just moved
    /// (`prevChannels_`, `entity_population.cpp:172`), and the forget covers
    /// it when its app dies.
    #[test]
    fn the_previous_tick_channel_covers_a_just_moved_real() {
        let mut recovery = entity();

        // The Real moves to B, and the tick turns B's channel into the
        // previous tick's.
        assert_eq!(recovery.switch_to(b()), SwitchOutcome::Switched);
        recovery.tick();

        // The previous channel still resolves the Real for a call from a cell
        // with no view.
        assert_eq!(
            recovery.call(caller(), &a_call(6)),
            CallState::Forwarded(b())
        );

        // B's CellApp dies: the forget removes the previous channel too, so
        // the call is failed and buffered.
        recovery.target_lost(b());
        assert_eq!(
            recovery.call(caller(), &a_call(7)),
            CallState::Failed(caller())
        );
        assert_eq!(recovery.pending().len(), 1);
    }

    /// A switch that does not happen keeps the Real where the current channel
    /// stays pointed at the old cell.
    #[test]
    fn a_failed_switch_keeps_the_current_channel() {
        let mut recovery = entity();

        // The destination holds no Ghost of the entity
        // (`NoGhostAtDestination`, `real_entity.cpp:395`).
        assert_eq!(
            recovery.switch_to(Address(42)),
            SwitchOutcome::NoGhostAtDestination
        );
        assert_eq!(
            recovery.call(caller(), &a_call(8)),
            CallState::Forwarded(a()),
            "the old channel still carries the calls"
        );
    }

    /// A call from the Real's own cell dispatches locally, even after a loss
    /// and restore to that cell.
    #[test]
    fn a_call_at_the_real_cell_dispatches_locally() {
        let mut recovery = entity();
        assert_eq!(recovery.call(a(), &a_call(2)), CallState::Dispatched);
        assert!(recovery.deliveries().is_empty());
    }
}
