//! BW-06-04-03 the client's dispatch of RPCs and entity properties.
//!
//! `ServerConnection::entityMethod` / `entityProperty` / `nestedEntityProperty`
//! (`lib/connection/server_connection.cpp`) route server messages to the
//! selected entity's callbacks. A message id encodes the exposed method or
//! property id:
//!
//! * `ExposedMethodMessageRange` (`lib/network/exposed_message_range.cpp`)
//!   decodes a method id from a message id; when there are more exposed
//!   methods than the range has slots, the overflow message ids carry a
//!   sub-id byte read from the front of the stream;
//! * `ExposedPropertyMessageRange` decodes a property id straight from the
//!   message id (no sub-id);
//! * every message targets `selectedEntityID_` — the entity the server
//!   selected (via `selectEntity` / `selectAliasedEntity` / the player) — and
//!   `BWEntities` discards it when that entity is unknown or is neither the
//!   player nor in the world; the method/property callback carries the
//!   remaining stream as its data.
//!
//! Messages are dispatched in arrival order, so the callbacks the fixture
//! produces match the reference's order and data.

use std::collections::HashMap;

/// `EntityID` (`entitydef/common`): a client entity id.
pub type EntityId = u32;

/// `Mercury::MessageID`: a message id in the interface range.
pub type MessageId = u8;

/// The exposed method or property id carried by a message.
pub type ExposedId = i32;

/// `ExposedMessageRange` (`exposed_message_range.hpp`): a range of message ids
/// handled by one handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExposedMessageRange {
    first_msg_id: MessageId,
    last_msg_id: MessageId,
}

impl ExposedMessageRange {
    /// Constructor (`firstMsgID`, `lastMsgID`).
    pub const fn new(first_msg_id: MessageId, last_msg_id: MessageId) -> Self {
        Self {
            first_msg_id,
            last_msg_id,
        }
    }

    /// `contains`.
    pub fn contains(&self, msg_id: MessageId) -> bool {
        self.first_msg_id <= msg_id && msg_id <= self.last_msg_id
    }

    /// `numSlots`: the number of message ids in the range.
    pub fn num_slots(&self) -> i32 {
        self.last_msg_id as i32 - self.first_msg_id as i32 + 1
    }

    /// `simpleExposedIDFromMsgID`.
    pub fn simple_exposed_id_from_msg_id(&self, msg_id: MessageId) -> ExposedId {
        msg_id as ExposedId - self.first_msg_id as ExposedId
    }
}

/// `ExposedMethodMessageRange`: a range of message ids used to call exposed
/// methods. Overflow message ids carry a sub-id byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExposedMethodMessageRange {
    range: ExposedMessageRange,
}

impl ExposedMethodMessageRange {
    /// Constructor.
    pub const fn new(first_msg_id: MessageId, last_msg_id: MessageId) -> Self {
        Self {
            range: ExposedMessageRange::new(first_msg_id, last_msg_id),
        }
    }

    /// `exposedIDFromMsgID`: decode the exposed method id. When the range
    /// overflows, a sub-id byte is read from the front of `data`.
    pub fn exposed_id_from_msg_id(
        &self,
        msg_id: MessageId,
        data: &mut StreamReader<'_>,
        num_exposed: i32,
    ) -> ExposedId {
        let num_remaining_slots = self.num_remaining_slots(num_exposed);
        let offset = msg_id as ExposedId - self.range.first_msg_id as ExposedId;
        if offset < num_remaining_slots {
            offset
        } else {
            let sub_index = data.read_u8().unwrap_or(0) as ExposedId;
            num_remaining_slots + 256 * (offset - num_remaining_slots) + sub_index
        }
    }

    /// `msgIDFromExposedID`: the message id (and sub-id, or -1) for an exposed
    /// method id.
    pub fn msg_id_from_exposed_id(
        &self,
        exposed_id: ExposedId,
        num_exposed: i32,
    ) -> (MessageId, i32) {
        let num_remaining_slots = self.num_remaining_slots(num_exposed);
        if exposed_id < num_remaining_slots {
            (
                (self.range.first_msg_id as ExposedId + exposed_id) as MessageId,
                -1,
            )
        } else {
            let overflow = exposed_id - num_remaining_slots;
            let first_overflow_slot = self.range.first_msg_id as ExposedId + num_remaining_slots;
            (
                (first_overflow_slot + overflow / 256) as MessageId,
                overflow % 256,
            )
        }
    }

    /// `needsSubID`.
    pub fn needs_sub_id(&self, exposed_id: ExposedId, num_exposed: i32) -> bool {
        exposed_id >= self.num_remaining_slots(num_exposed)
    }

    /// `numRemainingSlots`: the direct slots left when `num_exposed` methods
    /// must fit.
    fn num_remaining_slots(&self, num_exposed: i32) -> i32 {
        self.range.num_slots() - self.num_sub_slots(num_exposed)
    }

    /// `numSubSlots`: the message ids allocated to overflow indexes.
    fn num_sub_slots(&self, num_exposed: i32) -> i32 {
        let num_excess = num_exposed - self.range.num_slots();
        if num_excess > 0 {
            (num_excess / 255) + 1
        } else {
            0
        }
    }
}

/// `ExposedPropertyMessageRange`: a range of message ids used to send property
/// updates. A property id maps straight from the message id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExposedPropertyMessageRange {
    range: ExposedMessageRange,
}

impl ExposedPropertyMessageRange {
    /// Constructor.
    pub const fn new(first_msg_id: MessageId, last_msg_id: MessageId) -> Self {
        Self {
            range: ExposedMessageRange::new(first_msg_id, last_msg_id),
        }
    }

    /// `exposedIDFromMsgID`: no sub-id is read.
    pub fn exposed_id_from_msg_id(&self, msg_id: MessageId) -> ExposedId {
        self.range.simple_exposed_id_from_msg_id(msg_id)
    }

    /// `msgIDFromExposedID`: the message id, or -1 when it does not fit.
    pub fn msg_id_from_exposed_id(&self, exposed_id: ExposedId) -> i32 {
        if exposed_id < self.range.num_slots() {
            self.range.first_msg_id as ExposedId + exposed_id
        } else {
            -1
        }
    }

    /// `numSlots`.
    pub fn num_slots(&self) -> i32 {
        self.range.num_slots()
    }
}

/// A read cursor over the message payload, standing in for `BinaryIStream`.
#[derive(Clone, Debug)]
pub struct StreamReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StreamReader<'a> {
    /// Constructor.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Read one byte; `None` when the stream is exhausted.
    pub fn read_u8(&mut self) -> Option<u8> {
        let byte = self.data.get(self.pos).copied();
        if byte.is_some() {
            self.pos += 1;
        }
        byte
    }

    /// The bytes not yet read — the callback's data.
    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

/// The registry state of one entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntityState {
    /// `isPlayer()`.
    pub is_player: bool,
    /// `isInWorld()`.
    pub is_in_world: bool,
}

impl EntityState {
    /// Constructor.
    pub fn new(is_player: bool, is_in_world: bool) -> Self {
        Self {
            is_player,
            is_in_world,
        }
    }
}

/// One dispatched callback: `BWEntities` routes to the entity's
/// `onMethod`/`onProperty`/`onNestedProperty`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntityCallback {
    /// `onMethod`: the exposed method id and the method arguments.
    Method {
        /// The selected entity.
        entity_id: EntityId,
        /// The exposed method id.
        method_id: ExposedId,
        /// The method arguments (the stream after any sub-id byte).
        data: Vec<u8>,
    },
    /// `onProperty`: the exposed property id and the property data.
    Property {
        /// The selected entity.
        entity_id: EntityId,
        /// The exposed property id.
        property_id: ExposedId,
        /// The property data.
        data: Vec<u8>,
    },
    /// `onNestedProperty`: the nested property data (a slice when `is_slice`).
    NestedProperty {
        /// The selected entity.
        entity_id: EntityId,
        /// The nested property data.
        data: Vec<u8>,
        /// Whether this is a slice update.
        is_slice: bool,
    },
}

/// The client's entity message dispatch: the selected entity, the message
/// ranges and the entity registry.
#[derive(Debug)]
pub struct ClientEntityDispatch {
    selected_entity_id: EntityId,
    method_range: ExposedMethodMessageRange,
    property_range: ExposedPropertyMessageRange,
    max_client_method_count: i32,
    entities: HashMap<EntityId, EntityState>,
}

impl ClientEntityDispatch {
    /// Constructor with the ranges and `maxClientMethodCount_`.
    pub fn new(
        method_range: ExposedMethodMessageRange,
        property_range: ExposedPropertyMessageRange,
        max_client_method_count: i32,
    ) -> Self {
        Self {
            selected_entity_id: 0,
            method_range,
            property_range,
            max_client_method_count,
            entities: HashMap::new(),
        }
    }

    /// `selectEntity`: the entity subsequent messages target.
    pub fn set_selected_entity(&mut self, entity_id: EntityId) {
        self.selected_entity_id = entity_id;
    }

    /// `selectPlayerEntity`: subsequent messages target the given player.
    pub fn set_selected_player(&mut self, entity_id: EntityId) {
        self.selected_entity_id = entity_id;
    }

    /// The entity currently selected.
    pub fn selected_entity(&self) -> EntityId {
        self.selected_entity_id
    }

    /// Register (or update) an entity.
    pub fn register_entity(&mut self, entity_id: EntityId, state: EntityState) {
        self.entities.insert(entity_id, state);
    }

    /// Remove an entity.
    pub fn remove_entity(&mut self, entity_id: EntityId) {
        self.entities.remove(&entity_id);
    }

    /// `entityMethod`: decode the exposed method id and dispatch to the
    /// selected entity.
    pub fn dispatch_method(&mut self, msg_id: MessageId, payload: &[u8]) -> Option<EntityCallback> {
        let mut data = StreamReader::new(payload);
        let method_id = self.method_range.exposed_id_from_msg_id(
            msg_id,
            &mut data,
            self.max_client_method_count,
        );
        self.emit(EntityCallback::Method {
            entity_id: self.selected_entity_id,
            method_id,
            data: data.remaining().to_vec(),
        })
    }

    /// `entityProperty`: decode the exposed property id and dispatch.
    pub fn dispatch_property(
        &mut self,
        msg_id: MessageId,
        payload: &[u8],
    ) -> Option<EntityCallback> {
        let property_id = self.property_range.exposed_id_from_msg_id(msg_id);
        self.emit(EntityCallback::Property {
            entity_id: self.selected_entity_id,
            property_id,
            data: payload.to_vec(),
        })
    }

    /// `nestedEntityProperty` / `sliceEntityProperty`.
    pub fn dispatch_nested_property(
        &mut self,
        payload: &[u8],
        is_slice: bool,
    ) -> Option<EntityCallback> {
        self.emit(EntityCallback::NestedProperty {
            entity_id: self.selected_entity_id,
            data: payload.to_vec(),
            is_slice,
        })
    }

    /// The `BWEntities` gate: dispatch only when the selected entity exists
    /// and is the player or is in the world.
    fn emit(&self, callback: EntityCallback) -> Option<EntityCallback> {
        let should_call_callback = match self.entities.get(&self.selected_entity_id) {
            Some(state) => state.is_player || state.is_in_world,
            None => false,
        };
        should_call_callback.then_some(callback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ten method slots and four property slots. With 260 exposed methods the
    // method range keeps 9 direct slots (msg ids 0..8) and allocates 1 slot
    // (msg id 9) to overflow indexes, so both paths are exercised.
    const METHOD_RANGE: ExposedMethodMessageRange = ExposedMethodMessageRange::new(0, 9);
    const PROPERTY_RANGE: ExposedPropertyMessageRange = ExposedPropertyMessageRange::new(0, 3);
    const NUM_METHODS: i32 = 260;

    const PLAYER: EntityId = 100;
    const OTHER: EntityId = 200;
    const NOT_READY: EntityId = 300;

    fn dispatch() -> ClientEntityDispatch {
        let mut dispatch = ClientEntityDispatch::new(METHOD_RANGE, PROPERTY_RANGE, NUM_METHODS);
        dispatch.register_entity(PLAYER, EntityState::new(true, true));
        dispatch.register_entity(OTHER, EntityState::new(false, true));
        dispatch.register_entity(NOT_READY, EntityState::new(false, false));
        dispatch
    }

    #[test]
    fn a_direct_method_id_decodes_without_a_sub_id() {
        let mut dispatch = dispatch();
        dispatch.set_selected_entity(PLAYER);
        // msg_id 0 is a direct slot: exposed id 0, no byte consumed.
        let callback = dispatch.dispatch_method(0, &[1, 2, 3]).unwrap();
        assert_eq!(
            callback,
            EntityCallback::Method {
                entity_id: PLAYER,
                method_id: 0,
                data: vec![1, 2, 3],
            }
        );
    }

    #[test]
    fn an_overflow_method_id_reads_the_sub_id_from_the_stream() {
        let mut dispatch = dispatch();
        dispatch.set_selected_entity(PLAYER);
        // With 260 methods in 10 slots, msg id 9 is the overflow slot; the
        // first byte is the sub-id and the rest is the method data.
        let callback = dispatch.dispatch_method(9, &[5, 7, 9]).unwrap();
        assert_eq!(
            callback,
            EntityCallback::Method {
                entity_id: PLAYER,
                method_id: 9 + 5,
                data: vec![7, 9],
            }
        );
    }

    #[test]
    fn a_property_id_decodes_directly() {
        let mut dispatch = dispatch();
        dispatch.set_selected_entity(PLAYER);
        let callback = dispatch.dispatch_property(2, &[4, 5]).unwrap();
        assert_eq!(
            callback,
            EntityCallback::Property {
                entity_id: PLAYER,
                property_id: 2,
                data: vec![4, 5],
            }
        );
    }

    #[test]
    fn a_nested_property_targets_the_selected_entity() {
        let mut dispatch = dispatch();
        dispatch.set_selected_entity(OTHER);
        let callback = dispatch.dispatch_nested_property(&[6], false).unwrap();
        assert_eq!(
            callback,
            EntityCallback::NestedProperty {
                entity_id: OTHER,
                data: vec![6],
                is_slice: false,
            }
        );
        let slice = dispatch.dispatch_nested_property(&[6, 7], true).unwrap();
        assert_eq!(
            slice,
            EntityCallback::NestedProperty {
                entity_id: OTHER,
                data: vec![6, 7],
                is_slice: true,
            }
        );
    }

    #[test]
    fn an_unknown_entity_is_discarded() {
        let mut dispatch = dispatch();
        dispatch.set_selected_entity(999);
        assert_eq!(dispatch.dispatch_method(0, &[1]), None);
        assert_eq!(dispatch.dispatch_property(0, &[1]), None);
        assert_eq!(dispatch.dispatch_nested_property(&[1], false), None);
    }

    #[test]
    fn a_non_player_entity_outside_the_world_is_discarded() {
        let mut dispatch = dispatch();
        dispatch.set_selected_entity(NOT_READY);
        assert_eq!(dispatch.dispatch_method(0, &[1]), None);
        assert_eq!(dispatch.dispatch_property(1, &[2]), None);
        assert_eq!(dispatch.dispatch_nested_property(&[3], false), None);
    }

    #[test]
    fn an_in_world_entity_still_receives_callbacks() {
        let mut dispatch = dispatch();
        dispatch.set_selected_entity(OTHER);
        assert!(dispatch.dispatch_property(3, &[8]).is_some());
    }

    #[test]
    fn the_fixture_produces_the_same_callback_order_and_data() {
        let mut dispatch = dispatch();
        let mut callbacks = Vec::new();

        // The server selects the player and streams a method, an overflow
        // method, a property and a nested property.
        dispatch.set_selected_entity(PLAYER);
        callbacks.push(dispatch.dispatch_method(0, &[1, 2]).unwrap());
        callbacks.push(dispatch.dispatch_method(9, &[5, 3, 4]).unwrap());
        callbacks.push(dispatch.dispatch_property(2, &[6]).unwrap());
        callbacks.push(dispatch.dispatch_nested_property(&[7], false).unwrap());

        // An entity that is not ready yet is skipped.
        dispatch.set_selected_entity(NOT_READY);
        assert_eq!(dispatch.dispatch_property(1, &[8]), None);

        // An entity that left the world is skipped.
        dispatch.set_selected_entity(OTHER);
        dispatch.remove_entity(OTHER);
        assert_eq!(dispatch.dispatch_property(1, &[8]), None);

        // A second entity in the world still gets its callbacks.
        dispatch.register_entity(OTHER, EntityState::new(false, true));
        callbacks.push(dispatch.dispatch_property(3, &[9]).unwrap());

        assert_eq!(
            callbacks,
            vec![
                EntityCallback::Method {
                    entity_id: PLAYER,
                    method_id: 0,
                    data: vec![1, 2],
                },
                EntityCallback::Method {
                    entity_id: PLAYER,
                    method_id: 14,
                    data: vec![3, 4],
                },
                EntityCallback::Property {
                    entity_id: PLAYER,
                    property_id: 2,
                    data: vec![6],
                },
                EntityCallback::NestedProperty {
                    entity_id: PLAYER,
                    data: vec![7],
                    is_slice: false,
                },
                EntityCallback::Property {
                    entity_id: OTHER,
                    property_id: 3,
                    data: vec![9],
                },
            ]
        );
    }

    #[test]
    fn the_method_id_round_trips() {
        // For a non-overflow range, direct ids map one-to-one.
        let range = ExposedMethodMessageRange::new(0, 9);
        let (msg_id, sub) = range.msg_id_from_exposed_id(3, 8);
        assert_eq!((msg_id, sub), (3, -1));
        assert_eq!(
            range.exposed_id_from_msg_id(3, &mut StreamReader::new(&[]), 8),
            3
        );

        // Overflow ids map to the overflow slots with a sub-id.
        let (msg_id, sub) = range.msg_id_from_exposed_id(20, 8);
        assert_eq!((msg_id, sub), (10, 10));
        let mut data = StreamReader::new(&[10]);
        assert_eq!(range.exposed_id_from_msg_id(10, &mut data, 8), 20);
    }

    #[test]
    fn the_property_id_round_trips_and_rejects_overflow() {
        let range = ExposedPropertyMessageRange::new(0, 3);
        assert_eq!(range.exposed_id_from_msg_id(2), 2);
        assert_eq!(range.msg_id_from_exposed_id(2), 2);
        assert_eq!(range.msg_id_from_exposed_id(4), -1);
    }
}
