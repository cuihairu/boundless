//! BW-08-05-04: redirecting messages during a Real switch.
//!
//! A call made to an entity is routed to its Real. When the calling cell only
//! holds a Ghost, the message is re-issued on the Real's channel
//! (`forwardMessageToReal`), and a reply is forwarded back to the original
//! caller by a `ReplyForwarder`, so the caller hears the answer once. When the
//! Real is switching, the Ghost's `pRealChannel_` already points at the new
//! cell (`convertRealToGhost`), so the forward lands on the migration target.
//!
//! The acceptance, "调用到达迁移目标且响应一次", is that a call made while
//! the Real is switching reaches the new cell, and the caller receives exactly
//! one response.
//!
//! Reference:
//! - `message_handlers.cpp:192-216` `DISPOSITION_FORWARD_TO_REAL` — forward
//!   through the entity's `pRealChannel()`, or the entity population's channel
//!   when the cell holds no entity; fail back to the sender when neither exists.
//! - `entity.cpp:2278` `forwardMessageToReal` — re-issue the message on the
//!   Real's channel, wrapping the reply in a `ReplyForwarder` when the call
//!   wants one.
//! - `entity.cpp:2225-2252` `ReplyForwarder` — forward the Real's reply to the
//!   original sender and reply id, once.
//! - `entity_population.cpp:160-183` `findRealChannel` — the population's real
//!   channel, the current tick's first and then the previous tick's.
//! - `entity.cpp:1962` `isOffloadingTo` — the entity is offloading when
//!   `realAddr() == nextRealAddr_`.
//! - `entity.cpp:7838` `shouldBufferMessagesFrom` — a Ghost buffers messages
//!   that are not from its pending Real.
//! - `entity.cpp:1998` `convertRealToGhost` — `pRealChannel_` is set to the
//!   destination before the Real part is destroyed, so a Ghost routes to the
//!   new Real.

use std::collections::BTreeSet;

use crate::ghost_data::Address;
use crate::offload_role::EntityRole;

/// One call a cell wants routed to the entity's Real.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// The message id (`header.identifier`).
    pub message_id: u8,
    /// The sender, who must receive the single response.
    pub sender: Address,
    /// The payload.
    pub payload: Vec<u8>,
}

/// Where a routed call lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The calling cell holds the Real: the handler runs there
    /// (`DISPOSITION_DISPATCH`, `message_handlers.cpp:251-253`).
    Dispatched,
    /// Re-issued to the cell holding the Real (`forwardMessageToReal`,
    /// `entity.cpp:2278`).
    Forwarded(Address),
    /// The call was already forwarded, so it is not re-issued and the response
    /// stays one.
    AlreadyForwarded,
    /// No channel to the Real: the call is failed back to the sender
    /// (`sendFailure`, `message_handlers.cpp:207-211`).
    Failure,
}

/// The single response a forwarded call produces.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// The caller the reply was forwarded to (`ReplyForwarder::destAddr_`).
    pub reply_to: Address,
    /// The cell that answered.
    pub answered_by: Address,
    /// The reply payload.
    pub payload: Vec<u8>,
}

/// Routes entity calls to the Real across a switch, keeping one response per
/// call.
#[derive(Debug, Clone, Default)]
pub struct MessageRedirector {
    /// The entity's cell views (`offload_role::EntityRole`).
    roles: EntityRole,
    /// The population's real channel for the current tick
    /// (`EntityPopulation::currChannels_`).
    current_channel: Option<Address>,
    /// The previous tick's real channel (`prevChannels_`,
    /// `entity_population.cpp:172`).
    previous_channel: Option<Address>,
    /// The calls already forwarded, keyed by (sender, message id).
    forwarded: BTreeSet<(Address, u8)>,
    /// The calls that already answered.
    answered: BTreeSet<(Address, u8)>,
    /// The responses, one per forwarded call.
    responses: Vec<Response>,
}

impl MessageRedirector {
    /// A redirector over the entity's cell views.
    pub fn new(roles: EntityRole) -> Self {
        MessageRedirector {
            roles,
            ..MessageRedirector::default()
        }
    }

    /// The cell views.
    pub fn roles(&self) -> &EntityRole {
        &self.roles
    }

    /// Record the switch (`Cell::offloadEntity` + `Entity::onload`): the
    /// forward target moves onto the new Real.
    pub fn switch_roles(
        &mut self,
        old_cell: Address,
        new_cell: Address,
    ) -> crate::offload_role::SwitchOutcome {
        self.roles.switch_roles(old_cell, new_cell)
    }

    /// `EntityPopulation::currChannels_`: the cell now holding the Real, when
    /// the calling cell holds no entity at all.
    pub fn set_current_channel(&mut self, channel: Option<Address>) {
        self.current_channel = channel;
    }

    /// End of the tick: today's channel becomes the previous one
    /// (`prevChannels_`, `entity_population.cpp:172`).
    pub fn tick(&mut self) {
        self.previous_channel = self.current_channel.take();
    }

    /// Route a call from `from_cell` to the entity's Real
    /// (`DISPOSITION_FORWARD_TO_REAL`, `message_handlers.cpp:192-216`).
    pub fn route(&mut self, from_cell: Address, call: &Call) -> Route {
        // The call is already in flight: forward it once more and the caller
        // would hear the answer twice.
        if self.forwarded.contains(&(call.sender, call.message_id)) {
            return Route::AlreadyForwarded;
        }

        // A cell holding the entity routes through its own view.
        let Some(view) = self.roles.views.get(&from_cell) else {
            // `pEntity ? pEntity->pRealChannel() : population().findRealChannel`
            // (`message_handlers.cpp:198-200`).
            let target = self.current_channel.or(self.previous_channel);
            return match target {
                Some(target) => {
                    self.forwarded.insert((call.sender, call.message_id));
                    Route::Forwarded(target)
                }
                None => Route::Failure,
            };
        };

        if view.is_real() {
            // The Real is here: dispatch to the handler.
            return Route::Dispatched;
        }

        // A Ghost re-issues the call on the Real's channel
        // (`forwardMessageToReal`, `entity.cpp:2278`). During a switch
        // `pRealChannel_` already points at the destination
        // (`convertRealToGhost`, `entity.cpp:2027`).
        let target = view.route_to();
        self.forwarded.insert((call.sender, call.message_id));
        Route::Forwarded(target)
    }

    /// The Real answered a forwarded call. The response is forwarded to the
    /// original sender (`ReplyForwarder`, `entity.cpp:2225-2252`) — at most
    /// once per call. Returns whether the response was delivered.
    pub fn reply(&mut self, call: &Call, answered_by: Address, payload: Vec<u8>) -> bool {
        let key = (call.sender, call.message_id);
        if self.answered.contains(&key) {
            return false;
        }
        self.answered.insert(key);
        self.responses.push(Response {
            reply_to: call.sender,
            answered_by,
            payload,
        });
        true
    }

    /// The responses delivered, in order.
    pub fn responses(&self) -> &[Response] {
        &self.responses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offload_role::{CellView, SwitchOutcome};

    fn a() -> Address {
        Address(1)
    }
    fn b() -> Address {
        Address(2)
    }
    fn caller() -> Address {
        Address(9)
    }

    /// A Real on A with a Ghost on B, as before a switch.
    fn entity_before_switch() -> MessageRedirector {
        let mut roles = EntityRole::with_real(a());
        roles.add_ghost(CellView::ghost(b(), a()));
        MessageRedirector::new(roles)
    }

    fn a_call(message_id: u8) -> Call {
        Call {
            message_id,
            sender: caller(),
            payload: vec![1, 2, 3],
        }
    }

    /// The acceptance: a call made while the Real is switching reaches the new
    /// cell and the caller hears one answer.
    #[test]
    fn a_call_during_the_switch_reaches_the_migration_target_once() {
        let mut redirector = entity_before_switch();
        let call = a_call(4);

        // The Real leaves A for B.
        assert_eq!(redirector.switch_roles(a(), b()), SwitchOutcome::Switched);

        // A call that reaches A's Ghost is re-issued on the Real's channel —
        // now B, the migration target.
        assert_eq!(
            redirector.roles().real_cell(),
            Some(b()),
            "the Real is on the migration target"
        );
        assert_eq!(redirector.route(a(), &call), Route::Forwarded(b()));

        assert!(
            redirector.reply(&call, b(), vec![7]),
            "the reply is forwarded to the caller"
        );
        assert_eq!(
            redirector.responses(),
            &[Response {
                reply_to: caller(),
                answered_by: b(),
                payload: vec![7],
            }]
        );
        assert_eq!(redirector.responses().len(), 1, "exactly one response");
    }

    /// A call from the Real's own cell dispatches locally rather than
    /// forwarding.
    #[test]
    fn a_call_at_the_real_cell_dispatches_locally() {
        let mut redirector = entity_before_switch();
        assert_eq!(redirector.route(a(), &a_call(1)), Route::Dispatched);
        assert!(redirector.responses().is_empty());
    }

    /// A call already forwarded is not forwarded a second time, so the caller
    /// still hears one answer.
    #[test]
    fn a_duplicate_call_is_not_forwarded_twice() {
        let mut redirector = entity_before_switch();
        let call = a_call(5);

        assert_eq!(redirector.route(b(), &call), Route::Forwarded(a()));
        assert_eq!(redirector.route(b(), &call), Route::AlreadyForwarded);

        assert!(redirector.reply(&call, a(), vec![9]));
        assert!(
            !redirector.reply(&call, a(), vec![9]),
            "a second answer is dropped"
        );
        assert_eq!(redirector.responses().len(), 1);
    }

    /// The population's channel carries a call from a cell that holds no
    /// entity, and the previous tick's channel covers a Real that just moved.
    #[test]
    fn a_call_from_a_cell_with_no_entity_uses_the_population_channel() {
        let mut redirector = MessageRedirector::new(EntityRole::with_real(a()));
        redirector.set_current_channel(Some(a()));
        let call = a_call(2);

        assert_eq!(redirector.route(caller(), &call), Route::Forwarded(a()));

        // After the tick the current channel is gone; the previous one resolves.
        redirector.tick();
        redirector.set_current_channel(None);
        assert_eq!(
            redirector.route(caller(), &a_call(3)),
            Route::Forwarded(a())
        );
    }

    /// A call with no channel to the Real is failed back to the sender.
    #[test]
    fn a_call_with_no_real_channel_fails() {
        let mut redirector = MessageRedirector::new(EntityRole::with_real(a()));
        assert_eq!(redirector.route(caller(), &a_call(6)), Route::Failure);
        assert!(redirector.responses().is_empty());
    }

    /// A different message id from the same sender is a different call.
    #[test]
    fn a_different_message_id_is_a_different_call() {
        let mut redirector = entity_before_switch();
        assert_eq!(redirector.route(b(), &a_call(1)), Route::Forwarded(a()));
        assert_eq!(redirector.route(b(), &a_call(2)), Route::Forwarded(a()));

        assert!(redirector.reply(&a_call(1), a(), vec![1]));
        assert!(redirector.reply(&a_call(2), a(), vec![2]));
        assert_eq!(redirector.responses().len(), 2);
    }
}
