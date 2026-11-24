//! BW-08-06-05: recording replay events.
//!
//! Each tick, a CellApp records what happened into a replay collector:
//! entity creations, volatile moves, exposed methods and property changes,
//! deletions, player state and AoI changes, and space data — typed blocks
//! appended in the order they happened. Events for an entity that has not
//! been created yet are buffered and flushed right after its creation
//! block, even across tick boundaries. The tick boundary itself is the
//! collector's clear: the bytes gathered between two clears are one tick,
//! in recording order, and the final block ends the recording.
//!
//! The acceptance, "Tick 边界与事件排序一致", is that the events of a tick
//! come out in the order they were recorded, straddling a boundary puts an
//! event in the tick it was recorded in, and buffered events land after
//! their entity's creation block in whichever tick that creation lands.
//!
//! Reference:
//! - `replay_data.hpp:16-27` — the `ReplayData` block types, `0` through
//!   `9` in the order the enum lists them.
//! - `replay_data_collector.cpp:64-71` — the collector's name and its
//!   `shouldRecordAoIEvents` flag.
//! - `replay_data_collector.cpp:79-175` `addEntityState` — an entity is
//!   only recorded once per recording (`:81-84`), the create block opens
//!   the record, and any buffered data is transferred straight after it
//!   (`:163-170`).
//! - `replay_data_collector.cpp:177-224` — `queueEntityVolatile` defers to
//!   `addVolatileDataForQueuedEntities`, which emits one block per queued
//!   entity in id order, the on-ground form for grounded entities.
//! - `replay_data_collector.cpp:229-247` `addEntityMethod` — the method
//!   block goes to the entity's stream, buffered until the entity exists.
//! - `replay_data_collector.cpp:430-447` `addEntityPlayerStateChange` and
//!   `:449-472` `addEntityAoIChange` — both only when `shouldRecordAoIEvents`;
//!   the state change carries the witness flag and the AoI ids; an AoI entry
//!   is buffered on the player's stream (`:461-465`), an exit goes straight
//!   out — it is recorded before the entity deletion.
//! - `replay_data_collector.cpp:474-485` `deleteEntity` — the delete block,
//!   and the entity's queued volatile dies with it.
//! - `replay_data_collector.cpp:487-492` `addSpaceData`, `:494-504`
//!   `addFinish`, `:506-510` `clear`.
//! - `replay_data_collector.cpp:535-562` `getStreamForEntity` — the
//!   buffering decision.

use std::collections::{BTreeMap, BTreeSet};

use crate::range_list::EntityId;

/// The `ReplayData` block types (`replay_data.hpp:16-27`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// `BLOCK_TYPE_ENTITY_CREATE`.
    EntityCreate = 0,
    /// `BLOCK_TYPE_ENTITY_VOLATILE`.
    EntityVolatile = 1,
    /// `BLOCK_TYPE_ENTITY_VOLATILE_ON_GROUND`.
    EntityVolatileOnGround = 2,
    /// `BLOCK_TYPE_ENTITY_METHOD`.
    EntityMethod = 3,
    /// `BLOCK_TYPE_ENTITY_PROPERTY_CHANGE`.
    EntityPropertyChange = 4,
    /// `BLOCK_TYPE_ENTITY_DELETE`.
    EntityDelete = 5,
    /// `BLOCK_TYPE_ENTITY_PLAYER_STATE_CHANGE`.
    EntityPlayerStateChange = 6,
    /// `BLOCK_TYPE_PLAYER_AOI_CHANGE`.
    PlayerAoiChange = 7,
    /// `BLOCK_TYPE_SPACE_DATA`.
    SpaceData = 8,
    /// `BLOCK_TYPE_FINAL`.
    Final = 9,
}

impl BlockType {
    /// The tag on the wire (`BlockType` is streamed as its value).
    pub fn tag(self) -> u8 {
        self as u8
    }

    /// The block type of a tag.
    pub fn from_tag(tag: u8) -> Option<BlockType> {
        Some(match tag {
            0 => BlockType::EntityCreate,
            1 => BlockType::EntityVolatile,
            2 => BlockType::EntityVolatileOnGround,
            3 => BlockType::EntityMethod,
            4 => BlockType::EntityPropertyChange,
            5 => BlockType::EntityDelete,
            6 => BlockType::EntityPlayerStateChange,
            7 => BlockType::PlayerAoiChange,
            8 => BlockType::SpaceData,
            9 => BlockType::Final,
            _ => return None,
        })
    }
}

/// One recorded block: its type and payload bytes, the unit the tick
/// stream is read back in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The block's type.
    pub block_type: BlockType,
    /// The entity the block is about, where it has one.
    pub entity: Option<EntityId>,
    /// The block's remaining fields, as recorded.
    pub payload: Vec<u8>,
}

/// `ReplayDataCollector` (`replay_data_collector.hpp:37`): one recording's
/// event stream.
#[derive(Debug, Clone, Default)]
pub struct ReplayDataCollector {
    name: String,
    should_record_aoi_events: bool,
    data: Vec<u8>,
    /// The entities this recording has already created
    /// (`recordingSpaceEntryID_`, `replay_data_collector.cpp:81-84`).
    recorded: BTreeSet<EntityId>,
    /// The entities whose volatile move is queued (`movedEntities_`).
    moved: BTreeSet<EntityId>,
    /// The blocks held back until their entity is created
    /// (`bufferedEntityData_`).
    buffered: BTreeMap<EntityId, Vec<u8>>,
}

impl ReplayDataCollector {
    /// A collector under the given name, recording AoI events only when
    /// `should_record_aoi_events` (`replay_data_collector.cpp:38-49`).
    pub fn new(name: &str, should_record_aoi_events: bool) -> Self {
        ReplayDataCollector {
            name: name.to_string(),
            should_record_aoi_events,
            ..ReplayDataCollector::default()
        }
    }

    /// The recording's name (`name()`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The bytes recorded so far this tick (`data()`).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// `clear` (`replay_data_collector.cpp:506-510`) with the tick's bytes
    /// returned: the tick boundary. The moved queue dies with the tick; the
    /// buffered blocks survive until their entity is created.
    pub fn take_tick(&mut self) -> Vec<u8> {
        self.moved.clear();
        std::mem::take(&mut self.data)
    }

    /// `addEntityState` (`replay_data_collector.cpp:79`): record the
    /// creation once per recording, then transfer anything buffered for
    /// the entity right after the block (`:163-170`).
    pub fn add_entity_state(&mut self, entity: EntityId) {
        if self.recorded.contains(&entity) {
            return;
        }
        self.recorded.insert(entity);
        self.push_block(BlockType::EntityCreate, Some(entity), &[]);
        if let Some(buffered) = self.buffered.remove(&entity) {
            self.data.extend_from_slice(&buffered);
        }
    }

    /// `queueEntityVolatile` (`replay_data_collector.cpp:177`): defer the
    /// move to the flush.
    pub fn queue_entity_volatile(&mut self, entity: EntityId) {
        self.moved.insert(entity);
    }

    /// `addVolatileDataForQueuedEntities` (`replay_data_collector.cpp:189`):
    /// one block per queued entity in id order, the on-ground form for
    /// grounded entities.
    pub fn add_volatile_data_for_queued_entities<F>(&mut self, on_ground: F)
    where
        F: Fn(EntityId) -> bool,
    {
        let queued: Vec<EntityId> = self.moved.iter().copied().collect();
        for entity in queued {
            let block_type = if on_ground(entity) {
                BlockType::EntityVolatileOnGround
            } else {
                BlockType::EntityVolatile
            };
            self.push_block(block_type, Some(entity), &[]);
        }
    }

    /// `addEntityMethod` (`replay_data_collector.cpp:229`): the exposed
    /// method call with its payload, buffered until the entity exists.
    pub fn add_entity_method(&mut self, entity: EntityId, method_id: u16, payload: &[u8]) {
        let mut block = Vec::new();
        block.extend_from_slice(&method_id.to_be_bytes());
        block.extend_from_slice(&(payload.len() as i32).to_be_bytes());
        block.extend_from_slice(payload);
        let stream = self.stream_for_entity(entity);
        stream.push(BlockType::EntityMethod.tag());
        stream.extend_from_slice(&entity.to_be_bytes());
        stream.extend_from_slice(&block);
    }

    /// `addEntityProperty` (`replay_data_collector.cpp:249`): the property
    /// change, dropped for entities this recording has not created yet —
    /// their `addEntityState` will carry it (`:267-271`). The wire shape is
    /// the slice flag, the length, the value (`:291-297`).
    pub fn add_entity_property(&mut self, entity: EntityId, payload: &[u8]) {
        if !self.recorded.contains(&entity) {
            return;
        }
        let wire = [
            [0u8].as_slice(),
            (payload.len() as u32).to_be_bytes().as_slice(),
            payload,
        ]
        .concat();
        self.push_block(BlockType::EntityPropertyChange, Some(entity), &wire);
    }

    /// `addEntityPlayerStateChange` (`replay_data_collector.cpp:421`): the
    /// witness flag and the AoI ids — only when AoI events are recorded.
    pub fn add_entity_player_state_change(&mut self, entity: EntityId, aoi_ids: &[EntityId]) {
        if !self.should_record_aoi_events {
            return;
        }
        let mut payload = vec![u8::from(!aoi_ids.is_empty())];
        payload.extend_from_slice(&(aoi_ids.len() as u32).to_be_bytes());
        for id in aoi_ids {
            payload.extend_from_slice(&id.to_be_bytes());
        }
        self.push_block(BlockType::EntityPlayerStateChange, Some(entity), &payload);
    }

    /// `addEntityAoIChange` (`replay_data_collector.cpp:449`): the AoI
    /// entry or exit. An entry is buffered on the player's stream
    /// (`:461-465`); an exit goes straight out, ahead of the deletion.
    pub fn add_entity_aoi_change(&mut self, player: EntityId, entity: EntityId, entered: bool) {
        if !self.should_record_aoi_events {
            return;
        }
        let payload = [
            entity.to_be_bytes().as_slice(),
            [u8::from(entered)].as_slice(),
        ]
        .concat();
        let stream = if entered {
            // The player may not be created yet.
            self.stream_for_entity(player)
        } else {
            &mut self.data
        };
        stream.push(BlockType::PlayerAoiChange.tag());
        stream.extend_from_slice(&player.to_be_bytes());
        stream.extend_from_slice(&payload);
    }

    /// `deleteEntity` (`replay_data_collector.cpp:474`): the deletion, and
    /// the entity's queued volatile dies with it.
    pub fn delete_entity(&mut self, entity: EntityId) {
        self.push_block(BlockType::EntityDelete, Some(entity), &[]);
        self.moved.remove(&entity);
    }

    /// `addSpaceData` (`replay_data_collector.cpp:487`): the keyed space
    /// datum, its value length-prefixed as a streamed string.
    pub fn add_space_data(&mut self, key: u16, value: &str) {
        let wire = [
            key.to_be_bytes().as_slice(),
            (value.len() as u16).to_be_bytes().as_slice(),
            value.as_bytes(),
        ]
        .concat();
        self.push_block(BlockType::SpaceData, None, &wire);
    }

    /// `addFinish` (`replay_data_collector.cpp:498`): the final block that
    /// ends the recording.
    pub fn add_finish(&mut self) {
        self.push_block(BlockType::Final, None, &[]);
    }

    /// The stream a block for `entity` belongs on
    /// (`getStreamForEntity`, `replay_data_collector.cpp:535`): the tick
    /// stream once created, the entity's buffer until then.
    fn stream_for_entity(&mut self, entity: EntityId) -> &mut Vec<u8> {
        if self.recorded.contains(&entity) {
            return &mut self.data;
        }
        self.buffered.entry(entity).or_default()
    }

    /// Append a block: the tag, the entity where it has one, the payload.
    fn push_block(&mut self, block_type: BlockType, entity: Option<EntityId>, payload: &[u8]) {
        self.data.push(block_type.tag());
        if let Some(entity) = entity {
            self.data.extend_from_slice(&entity.to_be_bytes());
        }
        self.data.extend_from_slice(payload);
    }
}

/// Read a tick's bytes back as the recorded blocks: the tag, the entity,
/// and the block's remaining fields as payload. This is the reading side
/// the acceptance's ordering is asserted against.
pub fn read_blocks(tick: &[u8]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut at = 0;
    while at < tick.len() {
        let block_type = BlockType::from_tag(tick[at]).expect("a known block type");
        at += 1;
        let entity = match block_type {
            BlockType::SpaceData | BlockType::Final => None,
            _ => {
                let id = u32::from_be_bytes(tick[at..at + 4].try_into().expect("four bytes"));
                at += 4;
                Some(id)
            }
        };
        let payload_len = match block_type {
            BlockType::EntityCreate
            | BlockType::EntityVolatile
            | BlockType::EntityVolatileOnGround
            | BlockType::EntityDelete
            | BlockType::Final => 0,
            // The method: msg id, body length, body
            // (`replay_data_collector.cpp:243-246`).
            BlockType::EntityMethod => {
                2 + 4 + read_u32(tick, at + 2, "the method body length") as usize
            }
            // The property change: slice flag, length, value (`:291-297`).
            BlockType::EntityPropertyChange => {
                1 + 4 + read_u32(tick, at + 1, "the property length") as usize
            }
            // The player state change: witness flag, count, ids
            // (`replay_data_collector.cpp:437-447`).
            BlockType::EntityPlayerStateChange => {
                5 + 4 * read_u32(tick, at + 1, "the AoI count") as usize
            }
            // The AoI change: entity, entered flag (`:466-469`).
            BlockType::PlayerAoiChange => 5,
            // The space datum: key, value length, value (`:487-491`).
            BlockType::SpaceData => {
                4 + u16::from_be_bytes(tick[at + 2..at + 4].try_into().expect("two")) as usize
            }
        };
        let payload = tick[at..at + payload_len].to_vec();
        at += payload_len;
        blocks.push(Block {
            block_type,
            entity,
            payload,
        });
    }
    blocks
}

/// A big-endian u32 at `at`, for the length fields of the blocks.
fn read_u32(bytes: &[u8], at: usize, what: &str) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|slice| slice.try_into().ok())
        .map(u32::from_be_bytes)
        .unwrap_or_else(|| panic!("{what} needs four bytes"))
}

/// The tags of a tick's blocks, in order.
pub fn block_tags(tick: &[u8]) -> Vec<BlockType> {
    read_blocks(tick)
        .into_iter()
        .map(|block| block.block_type)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: u32) -> EntityId {
        id
    }

    /// The tags of the blocks with an entity, for compact assertions.
    fn kinds(tick: &[u8]) -> Vec<(BlockType, Option<EntityId>)> {
        read_blocks(tick)
            .into_iter()
            .map(|block| (block.block_type, block.entity))
            .collect()
    }

    /// The acceptance: the events of a tick come out in recording order,
    /// and the boundary puts each event in the tick it was recorded in.
    #[test]
    fn the_events_of_a_tick_come_out_in_order() {
        let mut collector = ReplayDataCollector::new("lane7", true);

        // Tick 0: a create, a queued move flushed, a method, a property
        // change, then the delete of another entity.
        collector.add_entity_state(e(1));
        collector.queue_entity_volatile(e(1));
        collector.add_volatile_data_for_queued_entities(|_| true);
        collector.add_entity_method(e(1), 42, b"arg");
        collector.add_entity_property(e(1), b"value");
        collector.delete_entity(e(2));
        let tick0 = collector.take_tick();

        assert_eq!(
            kinds(&tick0),
            vec![
                (BlockType::EntityCreate, Some(e(1))),
                (BlockType::EntityVolatileOnGround, Some(e(1))),
                (BlockType::EntityMethod, Some(e(1))),
                (BlockType::EntityPropertyChange, Some(e(1))),
                (BlockType::EntityDelete, Some(e(2))),
            ]
        );

        // Tick 1: space data, then the finish block ends the recording.
        collector.add_space_data(7, "snOW");
        collector.add_finish();
        let tick1 = collector.take_tick();
        assert_eq!(
            kinds(&tick1),
            vec![(BlockType::SpaceData, None), (BlockType::Final, None)]
        );

        // The boundary cleared the stream: the next tick starts empty.
        let tick2 = collector.take_tick();
        assert!(tick2.is_empty());
    }

    /// The acceptance, buffering half: an event for an entity that does not
    /// exist yet is held back and lands right after its creation block —
    /// even when the creation happens in a later tick.
    #[test]
    fn buffered_events_land_after_their_creation() {
        let mut collector = ReplayDataCollector::new("lane7", true);

        // The method fires before the entity has been created.
        collector.add_entity_method(e(5), 3, b"early");
        assert!(collector.data().is_empty(), "nothing is out yet");

        // The boundary passes; the buffer survives it.
        let tick0 = collector.take_tick();
        assert!(tick0.is_empty());

        // The creation flushes the buffer after its own block.
        collector.add_entity_state(e(5));
        collector.add_entity_method(e(5), 4, b"late");
        let tick1 = collector.take_tick();
        assert_eq!(
            block_tags(&tick1),
            vec![
                BlockType::EntityCreate,
                BlockType::EntityMethod,
                BlockType::EntityMethod,
            ]
        );

        // The early method's payload is the one right after the create.
        let blocks = read_blocks(&tick1);
        assert_eq!(blocks[1].entity, Some(e(5)));
        assert!(blocks[1].payload.ends_with(b"early"));
        assert!(blocks[2].payload.ends_with(b"late"));
    }

    /// An AoI entry is buffered on the player's stream; an exit goes
    /// straight out, ahead of the deletion it precedes
    /// (`replay_data_collector.cpp:455-471`).
    #[test]
    fn aoi_entries_buffer_and_exits_do_not() {
        let mut collector = ReplayDataCollector::new("lane7", true);

        // The player does not exist yet: the entry waits.
        collector.add_entity_aoi_change(e(9), e(1), true);
        assert!(collector.data().is_empty());

        // The exit of another entity goes straight out.
        collector.add_entity_aoi_change(e(9), e(2), false);
        collector.delete_entity(e(2));
        let tick0 = collector.take_tick();
        assert_eq!(
            kinds(&tick0),
            vec![
                (BlockType::PlayerAoiChange, Some(e(9))),
                (BlockType::EntityDelete, Some(e(2))),
            ]
        );

        // The player's creation flushes the buffered entry after it.
        collector.add_entity_state(e(9));
        let tick1 = collector.take_tick();
        assert_eq!(
            kinds(&tick1),
            vec![
                (BlockType::EntityCreate, Some(e(9))),
                (BlockType::PlayerAoiChange, Some(e(9))),
            ]
        );
        // The payload names the entity that entered.
        let blocks = read_blocks(&tick1);
        assert!(blocks[1].payload.starts_with(&e(1).to_be_bytes()));
        assert_eq!(blocks[1].payload[4], 1, "the entered flag");
    }

    /// AoI events and player state changes are only recorded when the
    /// collector was made with the flag (`replay_data_collector.cpp:426`,
    /// `:455`).
    #[test]
    fn aoi_events_need_the_flag() {
        let mut collector = ReplayDataCollector::new("lane7", false);
        collector.add_entity_state(e(1));
        collector.add_entity_player_state_change(e(1), &[e(2), e(3)]);
        collector.add_entity_aoi_change(e(1), e(2), true);
        let tick = collector.take_tick();
        assert_eq!(kinds(&tick), vec![(BlockType::EntityCreate, Some(e(1)))]);
    }

    /// The player state change carries the witness flag and the AoI ids
    /// (`replay_data_collector.cpp:430-447`).
    #[test]
    fn the_player_state_change_carries_the_aoi() {
        let mut collector = ReplayDataCollector::new("lane7", true);
        collector.add_entity_state(e(1));
        collector.add_entity_player_state_change(e(1), &[e(2), e(3)]);
        let tick = collector.take_tick();

        let blocks = read_blocks(&tick);
        assert_eq!(blocks[0].block_type, BlockType::EntityCreate);
        assert_eq!(blocks[1].block_type, BlockType::EntityPlayerStateChange);
        assert_eq!(blocks[1].payload[0], 1, "has a witness");
        let count = u32::from_be_bytes(blocks[1].payload[1..5].try_into().expect("four"));
        assert_eq!(count, 2);
        assert_eq!(
            blocks[1].payload[5..],
            [e(2).to_be_bytes(), e(3).to_be_bytes()].concat()
        );

        // Without a witness the flag is clear.
        let mut collector = ReplayDataCollector::new("lane7", true);
        collector.add_entity_state(e(4));
        collector.add_entity_player_state_change(e(4), &[]);
        let blocks = read_blocks(&collector.take_tick());
        assert_eq!(blocks[1].payload[0], 0, "no witness");
    }

    /// An entity is created once per recording; the second state is a
    /// no-op (`replay_data_collector.cpp:81-84`).
    #[test]
    fn an_entity_is_created_once() {
        let mut collector = ReplayDataCollector::new("lane7", true);
        collector.add_entity_state(e(1));
        collector.add_entity_state(e(1));
        let tick = collector.take_tick();
        assert_eq!(kinds(&tick), vec![(BlockType::EntityCreate, Some(e(1)))]);
    }

    /// A property change for an entity the recording has not created is
    /// dropped — the creation will carry it
    /// (`replay_data_collector.cpp:267-271`).
    #[test]
    fn an_uncreated_entitys_property_change_is_dropped() {
        let mut collector = ReplayDataCollector::new("lane7", true);
        collector.add_entity_property(e(8), b"early");
        assert!(collector.data().is_empty());
        collector.add_entity_state(e(8));
        collector.add_entity_property(e(8), b"late");
        let tick = collector.take_tick();
        assert_eq!(
            kinds(&tick),
            vec![
                (BlockType::EntityCreate, Some(e(8))),
                (BlockType::EntityPropertyChange, Some(e(8))),
            ]
        );
        assert!(read_blocks(&tick)[1].payload.ends_with(b"late"));
    }

    /// A deleted entity's queued volatile dies with it
    /// (`replay_data_collector.cpp:481`).
    #[test]
    fn a_deleted_entitys_volatile_dies_with_it() {
        let mut collector = ReplayDataCollector::new("lane7", true);
        collector.add_entity_state(e(1));
        collector.queue_entity_volatile(e(1));
        collector.queue_entity_volatile(e(2));
        collector.delete_entity(e(2));
        collector.add_volatile_data_for_queued_entities(|id| id == 1);
        let tick = collector.take_tick();
        assert_eq!(
            kinds(&tick),
            vec![
                (BlockType::EntityCreate, Some(e(1))),
                (BlockType::EntityDelete, Some(e(2))),
                (BlockType::EntityVolatileOnGround, Some(e(1))),
            ]
        );
    }

    /// The off-ground volatile takes the full-position form
    /// (`replay_data_collector.cpp:211-218`).
    #[test]
    fn an_offground_volatile_takes_the_full_form() {
        let mut collector = ReplayDataCollector::new("lane7", true);
        collector.add_entity_state(e(1));
        collector.queue_entity_volatile(e(1));
        collector.add_volatile_data_for_queued_entities(|_| false);
        let tick = collector.take_tick();
        assert_eq!(
            kinds(&tick),
            vec![
                (BlockType::EntityCreate, Some(e(1))),
                (BlockType::EntityVolatile, Some(e(1))),
            ]
        );
    }

    /// The tags round-trip through their wire values
    /// (`replay_data.hpp:16-27`).
    #[test]
    fn the_block_tags_round_trip() {
        for tag in 0..=9u8 {
            let block_type = BlockType::from_tag(tag).expect("a known tag");
            assert_eq!(block_type.tag(), tag);
        }
        assert_eq!(BlockType::from_tag(10), None);
    }

    /// A truncated block fails loudly rather than misparsing.
    #[test]
    #[should_panic(expected = "the method body length needs four bytes")]
    fn a_truncated_block_fails_loudly() {
        let mut tick = vec![BlockType::EntityMethod.tag()];
        tick.extend_from_slice(&e(1).to_be_bytes());
        tick.extend_from_slice(&7u16.to_be_bytes());
        tick.extend_from_slice(&[0, 0]);
        read_blocks(&tick);
    }

    /// The collector reports its name.
    #[test]
    fn the_collector_reports_its_name() {
        let collector = ReplayDataCollector::new("space3", false);
        assert_eq!(collector.name(), "space3");
        assert_eq!(ReplayDataCollector::new("x", true).name(), "x");
    }
}
