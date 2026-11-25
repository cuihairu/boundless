//! BW-08-06-05 — the replay data collector.
//!
//! Reference faces: `server/cellapp/replay_data_collector.{hpp,cpp}`,
//! `lib/connection/replay_data.hpp` (the block types), `Cell::
//! startRecording` (`cell.cpp:1011`), `Cell::tickRecording`
//! (`cell.cpp:1078`), `Cell::stopRecording` (`cell.cpp:1144`),
//! `Cells::tickRecordings` (`cells.cpp:183`), the game-tick tail
//! (`cellapp.cpp:1432`), and the volatile queue at the position
//! internals (`entity.cpp:3949`).

use crate::cellapp_space::{CellApp, Entity};
use crate::cellappmgr::SpaceId;
use std::collections::BTreeSet;
use std::rc::Rc;

/// `ReplayData::BLOCK_TYPE_ENTITY_CREATE`.
pub const BLOCK_TYPE_ENTITY_CREATE: u8 = 0;
/// `ReplayData::BLOCK_TYPE_ENTITY_VOLATILE`.
pub const BLOCK_TYPE_ENTITY_VOLATILE: u8 = 1;
/// `ReplayData::BLOCK_TYPE_ENTITY_VOLATILE_ON_GROUND`.
pub const BLOCK_TYPE_ENTITY_VOLATILE_ON_GROUND: u8 = 2;
/// `ReplayData::BLOCK_TYPE_ENTITY_METHOD`.
pub const BLOCK_TYPE_ENTITY_METHOD: u8 = 3;
/// `ReplayData::BLOCK_TYPE_ENTITY_PROPERTY_CHANGE`.
pub const BLOCK_TYPE_ENTITY_PROPERTY_CHANGE: u8 = 4;
/// `ReplayData::BLOCK_TYPE_ENTITY_DELETE`.
pub const BLOCK_TYPE_ENTITY_DELETE: u8 = 5;
/// `ReplayData::BLOCK_TYPE_ENTITY_PLAYER_STATE_CHANGE`.
pub const BLOCK_TYPE_ENTITY_PLAYER_STATE_CHANGE: u8 = 6;
/// `ReplayData::BLOCK_TYPE_PLAYER_AOI_CHANGE`.
pub const BLOCK_TYPE_PLAYER_AOI_CHANGE: u8 = 7;
/// `ReplayData::BLOCK_TYPE_SPACE_DATA`.
pub const BLOCK_TYPE_SPACE_DATA: u8 = 8;
/// `ReplayData::BLOCK_TYPE_FINAL`.
pub const BLOCK_TYPE_FINAL: u8 = 9;

/// `ReplayDataCollector`: one space's recording buffer, cleared each
/// tick (`Cell::tickRecording`).
#[derive(Clone, Debug, Default)]
pub struct ReplayCollector {
    /// `name_`: the recording name.
    pub name: String,
    /// `shouldRecordAoIEvents_`.
    pub should_record_aoi_events: bool,
    /// `recordingSpaceEntryID_`.
    pub recording_space_entry_id: u32,
    /// `data_`: the tick's blocks so far.
    pub data: Vec<u8>,
    /// `movedEntities_`: the ids queued for volatile this tick; the
    /// flush walks them id-ascending and reads the live positions.
    pub moved_entities: BTreeSet<u32>,
    /// The entities `addEntityState` already put in this recording (the
    /// reference's per-real `recordingSpaceEntryID` stamp).
    pub recorded_entities: BTreeSet<u32>,
    /// `Cell::stoppingState_` rode the collector's lifetime; the
    /// collector is dropped on the next tick after this is set.
    pub stopping: bool,
}

impl ReplayCollector {
    /// The constructor (`replay_data_collector.cpp:33`).
    pub fn new(name: &str, should_record_aoi_events: bool) -> Self {
        Self {
            name: name.to_string(),
            should_record_aoi_events,
            ..Default::default()
        }
    }

    /// `ReplayDataCollector::addEntityState` (`:69`): the entity's
    /// initial state. The reference stamps the real and compresses the
    /// client-property stream (entitydef machinery); the port carries
    /// the block shape — vehicle, id, type, position, direction —
    /// uncompressed.
    pub fn add_entity_state(&mut self, entity: &Entity) -> bool {
        if !self.recorded_entities.insert(entity.id) {
            return false;
        }
        self.data.push(BLOCK_TYPE_ENTITY_CREATE);
        self.data
            .extend_from_slice(&entity.vehicle.unwrap_or(0).to_le_bytes());
        self.data.extend_from_slice(&entity.id.to_le_bytes());
        self.data
            .extend_from_slice(&(entity.type_id as u16).to_le_bytes());
        for value in entity.position.iter().copied() {
            self.data.extend_from_slice(&value.to_le_bytes());
        }
        for value in entity.direction.iter().copied() {
            self.data.extend_from_slice(&value.to_le_bytes());
        }
        true
    }

    /// `ReplayDataCollector::queueEntityVolatile` (`:183`): the moved
    /// set gains the id; the values are read at the flush.
    pub fn queue_entity_volatile(&mut self, id: u32) {
        self.moved_entities.insert(id);
    }

    /// `ReplayDataCollector::addVolatileDataForQueuedEntities` (`:197`):
    /// one volatile block per queued id, id-ascending, values read off
    /// the live entities.
    pub fn add_volatile_data_for_queued_entities(
        &mut self,
        population: &std::collections::BTreeMap<u32, Rc<std::cell::RefCell<Entity>>>,
    ) {
        let ids: Vec<u32> = self.moved_entities.iter().copied().collect();
        for id in ids {
            let Some(entity_rc) = population.get(&id) else {
                // Vanished between the queue and the flush.
                continue;
            };
            let entity = entity_rc.borrow();
            self.data.push(if entity.is_on_ground {
                BLOCK_TYPE_ENTITY_VOLATILE_ON_GROUND
            } else {
                BLOCK_TYPE_ENTITY_VOLATILE
            });
            self.data.extend_from_slice(&entity.id.to_le_bytes());
            if entity.is_on_ground {
                self.data
                    .extend_from_slice(&entity.position[0].to_le_bytes());
                self.data
                    .extend_from_slice(&entity.position[2].to_le_bytes());
            } else {
                for value in entity.position.iter().copied() {
                    self.data.extend_from_slice(&value.to_le_bytes());
                }
            }
            for value in entity.direction.iter().copied() {
                self.data.extend_from_slice(&value.to_le_bytes());
            }
            self.data
                .extend_from_slice(&entity.vehicle.unwrap_or(0).to_le_bytes());
        }
    }

    /// `ReplayDataCollector::deleteEntity` (`:481`).
    pub fn delete_entity(&mut self, id: u32) {
        self.data.push(BLOCK_TYPE_ENTITY_DELETE);
        self.data.extend_from_slice(&id.to_le_bytes());
        self.moved_entities.remove(&id);
    }

    /// `ReplayDataCollector::addSpaceData` (`:497`).
    pub fn add_space_data(&mut self, entry_id: u32, key: u16, value: &[u8]) {
        self.data.push(BLOCK_TYPE_SPACE_DATA);
        self.data.extend_from_slice(&entry_id.to_le_bytes());
        self.data.extend_from_slice(&key.to_le_bytes());
        self.data
            .extend_from_slice(&(value.len() as u32).to_le_bytes());
        self.data.extend_from_slice(value);
    }

    /// `ReplayDataCollector::addFinish` (`:518`).
    pub fn add_finish(&mut self) {
        self.data.push(BLOCK_TYPE_FINAL);
    }

    /// `ReplayDataCollector::clear` (`:508`): the tick's buffer and the
    /// moved set reset.
    pub fn clear(&mut self) {
        self.data.clear();
        self.moved_entities.clear();
    }
}

impl CellApp {
    /// `Cell::startRecording` (`cell.cpp:1011`): mount the collector.
    /// The initial face snapshots the cell-only-filtered space data and
    /// the cell's real entities, then declares the recording in the
    /// space data.
    pub fn start_recording(
        &mut self,
        space_id: SpaceId,
        entry_id: u32,
        name: &str,
        should_record_aoi_events: bool,
        is_initial: bool,
    ) -> bool {
        if self.replay_collectors.contains_key(&space_id) {
            // `Cell::startRecording`'s WARNING — the recording name
            // already mounted wins.
            self.recording_warnings.push(format!(
                "Cell::startRecording: space {}, already recording with \
                 name \"{}\", got request to start for \"{}\"",
                space_id, self.replay_collectors[&space_id].name, name
            ));
            return false;
        }
        // `Cell::startRecording` is a cell method — the port's cell is
        // the mounted `Space`; there is no collector without one.
        if !self.spaces.contains_key(&space_id) {
            return false;
        }
        let mut collector = ReplayCollector::new(name, should_record_aoi_events);
        collector.recording_space_entry_id = entry_id;

        // The space data snapshot — user-visible keys only, the
        // cell-only keys never reach the replay (`cell.cpp:1042`).
        let entries: Vec<(u32, u16, Vec<u8>)> = self.spaces[&space_id]
            .space_data
            .entries()
            .map(|(entry_id, value)| (entry_id, value.key, value.data.clone()))
            .collect();
        for (entry_id_, key, value) in entries {
            if key >= crate::cellapp_space::SPACE_DATA_FIRST_CELL_ONLY_KEY {
                continue;
            }
            collector.add_space_data(entry_id_, key, &value);
        }

        // The cell's real entities, in their list order.
        let entity_ids: Vec<u32> = self.spaces[&space_id]
            .entities()
            .iter()
            .map(|entity| entity.borrow().id)
            .collect();
        for id in entity_ids {
            if let Some(entity_rc) = self.entity_population.get(&id) {
                collector.add_entity_state(&entity_rc.borrow());
            }
        }

        // The recording declaration and the start script event.
        if is_initial {
            let mut spaces = std::mem::take(&mut self.spaces);
            let space = spaces
                .get_mut(&space_id)
                .expect("the mounted cell checked above");
            space.space_data_entry(
                self,
                entry_id,
                crate::cellapp_space::SPACE_DATA_RECORDING,
                name.as_bytes().to_vec(),
                crate::cellapp_space::UpdateCellAppMgr::Update,
                crate::cellapp_space::DataEffected::NeedToEffect,
            );
            self.spaces = spaces;
            self.recording_events
                .push((space_id, self.time, name.to_string(), Vec::new()));
        }

        self.replay_collectors.insert(space_id, collector);
        true
    }

    /// `Cell::stopRecording` (`cell.cpp:1144`): append the FINAL block;
    /// the collector leaves on the next tick's flush.
    pub fn stop_recording(&mut self, space_id: SpaceId, is_initial: bool) {
        let Some(collector) = self.replay_collectors.get_mut(&space_id) else {
            return;
        };
        collector.add_finish();
        collector.stopping = is_initial;
    }

    /// `Cells::tickRecordings` (`cells.cpp:183`) — the game-tick tail
    /// (`cellapp.cpp:1432`): flush the volatile blocks, hand the tick
    /// data to the recording event, clear, and drop the collectors that
    /// stopped last tick.
    pub fn tick_recordings(&mut self) {
        let mut stopped = Vec::new();
        for (space_id, collector) in self.replay_collectors.iter_mut() {
            collector.add_volatile_data_for_queued_entities(&self.entity_population);
            let payload = std::mem::take(&mut collector.data);
            let name = collector.name.clone();
            self.recording_events
                .push((*space_id, self.time, name.clone(), payload));
            collector.clear();
            if collector.stopping {
                stopped.push((*space_id, name, collector.should_record_aoi_events));
            }
        }
        for (space_id, name, _) in stopped {
            self.replay_collectors.remove(&space_id);
            // `STOPPING_STATE_STOPPING_SHOULD_USE_CALLBACKS` carries the
            // space data deletion and the stopped event; the other
            // stopping state just leaves.
            self.recording_events
                .push((space_id, self.time, name, Vec::new()));
        }
    }

    /// `Entity::updateInternalsForNewPositionOfReal`'s replay queue
    /// (`entity.cpp:3949`): the real entities that moved this tick join
    /// the volatile set.
    pub fn queue_entity_volatile(&mut self, entity: u32) {
        if let Some(entity_rc) = self.entity_population.get(&entity) {
            let space_id = entity_rc.borrow().space_id;
            let is_real = entity_rc.borrow().is_real;
            if is_real && let Some(collector) = self.replay_collectors.get_mut(&space_id) {
                collector.queue_entity_volatile(entity);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cellapp_real_entity::EntityScript;
    use crate::cellapp_replay::*;
    use crate::cellapp_space::{Cell, CellInfo, CellInfoNode, Space};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;

    fn a_cell() -> Rc<RefCell<Cell>> {
        Rc::new(RefCell::new(Cell {
            info: CellInfo {
                space_id: SID,
                app_addr: 42,
                node: CellInfoNode::first(),
                rect: (0.0, 0.0, 0.0, 0.0),
                is_delete_pending: false,
            },
            space_gone_count: 0,
            reuse_count: 0,
            should_offload: true,
            is_removed: false,
        }))
    }

    /// The creation stream: id + type, position, the on-ground flag,
    /// direction, last event number, base address, witness byte.
    fn create_stream(id: u32, pos: [f32; 3]) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&TYPE.to_le_bytes());
        for value in pos {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(crate::cellapp_real_entity::ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in [0.0_f32; 3] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(b'-');
        stream
    }

    /// A started space with a cell and two reals at known positions.
    fn fixture() -> CellApp {
        let mut app = CellApp::default();
        let mut spaces = BTreeMap::new();
        let mut space = Space::new(SID);
        space.set_cell(a_cell());
        spaces.insert(SID, space);
        app.entity_scripts
            .insert(TYPE, EntityScript::with_init(vec![]));
        app.create_real_entity(&mut spaces, SID, &create_stream(1, [1.0, 2.0, 3.0]));
        app.create_real_entity(&mut spaces, SID, &create_stream(2, [4.0, 5.0, 6.0]));
        app.spaces = spaces;
        app
    }

    #[test]
    fn starting_a_recording_snapshots_the_space_and_the_reals() {
        let mut app = fixture();
        assert!(app.start_recording(SID, 5, "rec", true, true));
        let collector = app.replay_collectors.get(&SID).unwrap();
        // Two create blocks, in the cell's entity order: vehicle, id,
        // type, position, direction.
        let mut expected = Vec::new();
        for (id, pos) in [(1u32, [1.0_f32, 2.0, 3.0]), (2u32, [4.0, 5.0, 6.0])] {
            expected.push(BLOCK_TYPE_ENTITY_CREATE);
            expected.extend_from_slice(&0u32.to_le_bytes());
            expected.extend_from_slice(&id.to_le_bytes());
            expected.extend_from_slice(&(TYPE as u16).to_le_bytes());
            for value in pos {
                expected.extend_from_slice(&value.to_le_bytes());
            }
            for value in [0.0_f32; 3] {
                expected.extend_from_slice(&value.to_le_bytes());
            }
        }
        assert_eq!(collector.data, expected);
        // The declaration and the start event ride the space data.
        let recording = app.spaces[&SID]
            .space_data
            .data_retrieve_first(crate::cellapp_space::SPACE_DATA_RECORDING)
            .expect("the recording entry");
        assert_eq!(recording.key, crate::cellapp_space::SPACE_DATA_RECORDING);
        assert_eq!(recording.data, b"rec".to_vec());
        assert_eq!(
            app.recording_events,
            vec![(SID, 0, "rec".to_string(), Vec::new())]
        );
    }

    #[test]
    fn a_second_start_is_refused() {
        let mut app = fixture();
        assert!(app.start_recording(SID, 5, "rec", true, true));
        assert!(!app.start_recording(SID, 5, "rec2", true, true));
        assert_eq!(app.replay_collectors.len(), 1);
        assert_eq!(app.recording_warnings.len(), 1);
        assert_eq!(
            app.recording_warnings[0],
            "Cell::startRecording: space 3, already recording with name \
             \"rec\", got request to start for \"rec2\""
        );
    }

    #[test]
    fn moved_reals_flush_in_id_order_with_the_live_position() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        // The initial states ride the first tick's flush; drain it.
        app.tick_recordings();
        app.recording_events.clear();
        // Queue in reverse order, move after the queue: the flush
        // reads the live positions and walks the ids ascending.
        app.queue_entity_volatile(2);
        app.queue_entity_volatile(1);
        app.entity_population[&2].borrow_mut().position[0] = 40.0;
        app.tick_recordings();
        let payload = &app.recording_events.last().unwrap().3;
        // Entity 1's block: on-ground — type, id, x, z, direction, vehicle.
        let mut expected = Vec::new();
        expected.push(BLOCK_TYPE_ENTITY_VOLATILE_ON_GROUND);
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&1.0_f32.to_le_bytes());
        expected.extend_from_slice(&3.0_f32.to_le_bytes());
        for value in [0.0_f32; 3] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&0u32.to_le_bytes());
        expected.push(BLOCK_TYPE_ENTITY_VOLATILE_ON_GROUND);
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(&40.0_f32.to_le_bytes());
        expected.extend_from_slice(&6.0_f32.to_le_bytes());
        for value in [0.0_f32; 3] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(payload, &expected);
        // The moved set resets for the next tick.
        assert!(app.replay_collectors[&SID].moved_entities.is_empty());
    }

    #[test]
    fn an_off_ground_mover_takes_the_full_position_layout() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        // The initial states ride the first tick's flush; drain it.
        app.tick_recordings();
        app.recording_events.clear();
        app.entity_population[&2].borrow_mut().is_on_ground = false;
        app.entity_population[&2].borrow_mut().position = [7.0, 8.0, 9.0];
        app.queue_entity_volatile(2);
        app.tick_recordings();
        let payload = &app.recording_events.last().unwrap().3;
        let mut expected = Vec::new();
        expected.push(BLOCK_TYPE_ENTITY_VOLATILE);
        expected.extend_from_slice(&2u32.to_le_bytes());
        for value in [7.0_f32, 8.0, 9.0] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        for value in [0.0_f32; 3] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(payload, &expected);
    }

    #[test]
    fn a_deleted_entity_records_a_delete_and_drops_its_volatile() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        // The initial states ride the first tick's flush; drain it.
        app.tick_recordings();
        app.recording_events.clear();
        app.queue_entity_volatile(1);
        app.replay_collectors
            .get_mut(&SID)
            .unwrap()
            .delete_entity(1);
        app.tick_recordings();
        let payload = &app.recording_events.last().unwrap().3;
        let mut expected = Vec::new();
        expected.push(BLOCK_TYPE_ENTITY_DELETE);
        expected.extend_from_slice(&1u32.to_le_bytes());
        assert_eq!(payload, &expected);
    }

    #[test]
    fn each_tick_carries_its_own_blocks() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        app.recording_events.clear(); // the started event is pinned above
        app.queue_entity_volatile(1);
        // The first tick flushes the initial snapshot plus the move.
        app.advance_tick();
        assert_eq!(app.recording_events.len(), 1);
        assert_eq!(app.recording_events[0].1, 1); // the post-advance time
        assert!(!app.recording_events[0].3.is_empty());
        // A quiet tick: only the volatile blocks are gone.
        app.advance_tick();
        assert_eq!(app.recording_events.len(), 2);
        assert!(app.recording_events[1].3.is_empty());
    }

    #[test]
    fn stopping_marks_final_and_drops_the_collector_next_tick() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        app.tick_recordings();
        app.recording_events.clear();
        app.stop_recording(SID, true);
        app.tick_recordings();
        // The FINAL block rides the stopping tick's flush; the stopped
        // event follows it.
        assert_eq!(app.recording_events.len(), 2);
        assert_eq!(app.recording_events[0].3, &[BLOCK_TYPE_FINAL]);
        assert_eq!(app.recording_events[1].3, &[]);
        // The collector is gone; the next ticks are silent.
        assert!(!app.replay_collectors.contains_key(&SID));
        app.tick_recordings();
        assert_eq!(app.recording_events.len(), 2);
    }

    #[test]
    fn stopping_without_callbacks_skips_the_stopped_event() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        app.tick_recordings();
        app.recording_events.clear();
        app.stop_recording(SID, false);
        app.tick_recordings();
        assert_eq!(app.recording_events.last().unwrap().3, &[BLOCK_TYPE_FINAL]);
        // One event per tick: the tick data carries the FINAL.
        assert_eq!(app.recording_events.len(), 1);
    }

    #[test]
    fn the_position_internals_feed_the_queue_at_the_tick_tail() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        app.tick_recordings();
        app.recording_events.clear();
        let entity_rc = app.entity_population[&2].clone();
        // The movement face the position internals ride.
        app.set_entity_position_and_direction(
            &entity_rc,
            foundation::math::Vector3::new(10.0, 0.0, 0.0),
            foundation::math::Vector3::new(0.0, 0.0, 0.0),
        );
        app.advance_tick();
        // The block landed in the tick's recording event — the queue
        // happened inside the position internals, the flush at the
        // tick tail, in order.
        let payload = &app.recording_events.last().unwrap().3;
        let mut expected = Vec::new();
        expected.push(BLOCK_TYPE_ENTITY_VOLATILE_ON_GROUND);
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(&10.0_f32.to_le_bytes());
        expected.extend_from_slice(&0.0_f32.to_le_bytes());
        for value in [0.0_f32; 3] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(payload, &expected);
    }

    #[test]
    fn a_vanished_entity_is_skipped_at_the_flush() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        app.tick_recordings();
        app.recording_events.clear();
        app.queue_entity_volatile(1);
        app.entity_population.remove(&1);
        app.tick_recordings();
        assert!(app.recording_events.last().unwrap().3.is_empty());
    }

    #[test]
    fn a_state_already_recorded_is_skipped() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        app.tick_recordings();
        app.recording_events.clear();
        let collector = app.replay_collectors.get_mut(&SID).unwrap();
        let entity_rc = app.entity_population[&1].clone();
        // The initial snapshot already recorded both reals — the
        // reference stamps the real per recording, so repeats are
        // refused without writing a block.
        assert!(!collector.add_entity_state(&entity_rc.borrow()));
        assert!(!collector.add_entity_state(&entity_rc.borrow()));
        assert!(collector.data.is_empty());
    }

    #[test]
    fn a_space_without_a_cell_refuses_the_recording() {
        let mut app = fixture();
        assert!(!app.start_recording(99, 5, "rec", true, true));
        assert!(app.replay_collectors.is_empty());
        assert!(app.recording_events.is_empty());
    }

    #[test]
    fn the_space_data_snapshot_keeps_user_keys_only() {
        let mut app = fixture();
        {
            let mut spaces = std::mem::take(&mut app.spaces);
            let space = spaces.get_mut(&SID).unwrap();
            // A user-visible key and a cell-only key ride the space
            // data; only the former reaches the replay.
            space.space_data_entry(
                &mut app,
                9,
                100,
                b"user".to_vec(),
                crate::cellapp_space::UpdateCellAppMgr::Update,
                crate::cellapp_space::DataEffected::NeedToEffect,
            );
            space.space_data_entry(
                &mut app,
                10,
                crate::cellapp_space::SPACE_DATA_FIRST_CELL_ONLY_KEY + 1,
                b"cell-only".to_vec(),
                crate::cellapp_space::UpdateCellAppMgr::Update,
                crate::cellapp_space::DataEffected::NeedToEffect,
            );
            app.spaces = spaces;
        }
        app.start_recording(SID, 5, "rec", true, true);
        let collector = app.replay_collectors.get(&SID).unwrap();
        let mut expected = Vec::new();
        expected.push(BLOCK_TYPE_SPACE_DATA);
        expected.extend_from_slice(&9u32.to_le_bytes());
        expected.extend_from_slice(&100u16.to_le_bytes());
        expected.extend_from_slice(&(b"user".len() as u32).to_le_bytes());
        expected.extend_from_slice(b"user");
        for (id, pos) in [(1u32, [1.0_f32, 2.0, 3.0]), (2u32, [4.0, 5.0, 6.0])] {
            expected.push(BLOCK_TYPE_ENTITY_CREATE);
            expected.extend_from_slice(&0u32.to_le_bytes());
            expected.extend_from_slice(&id.to_le_bytes());
            expected.extend_from_slice(&(TYPE as u16).to_le_bytes());
            for value in pos {
                expected.extend_from_slice(&value.to_le_bytes());
            }
            for value in [0.0_f32; 3] {
                expected.extend_from_slice(&value.to_le_bytes());
            }
        }
        assert_eq!(collector.data, expected);
    }

    #[test]
    fn a_non_initial_start_skips_the_declaration() {
        let mut app = fixture();
        assert!(app.start_recording(SID, 5, "rec", true, false));
        // No start event, no recording space data entry — only the
        // originating CellApp answers that one.
        assert!(app.recording_events.is_empty());
        assert!(
            app.spaces[&SID]
                .space_data
                .data_retrieve_first(crate::cellapp_space::SPACE_DATA_RECORDING)
                .is_none()
        );
    }

    #[test]
    fn stopping_an_unmounted_recording_is_inert() {
        let mut app = fixture();
        app.stop_recording(SID, true);
        assert!(app.replay_collectors.is_empty());
        assert!(app.recording_events.is_empty());
    }

    #[test]
    fn ghost_movers_do_not_join_the_volatile_set() {
        let mut app = fixture();
        app.start_recording(SID, 5, "rec", true, true);
        app.tick_recordings();
        app.recording_events.clear();
        app.entity_population[&2].borrow_mut().is_real = false;
        let entity_rc = app.entity_population[&2].clone();
        app.set_entity_position_and_direction(
            &entity_rc,
            foundation::math::Vector3::new(10.0, 0.0, 0.0),
            foundation::math::Vector3::new(0.0, 0.0, 0.0),
        );
        app.advance_tick();
        assert!(app.recording_events.last().unwrap().3.is_empty());
    }

    #[test]
    fn a_space_entity_missing_from_the_population_is_skipped_and_strangers_stay_unqueued() {
        let mut app = fixture();
        // A space-list id the population no longer holds: the snapshot
        // skips it, the cell keeps the survivor.
        app.entity_population.remove(&1);
        assert!(app.start_recording(SID, 5, "rec", true, true));
        let collector = app.replay_collectors.get(&SID).unwrap();
        assert_eq!(collector.data.len(), 35); // entity 2's create block only
        assert_eq!(collector.recorded_entities, [2].into_iter().collect());
        // A queue request for a stranger is inert.
        app.queue_entity_volatile(999);
        assert!(app.replay_collectors[&SID].moved_entities.is_empty());
    }
}
