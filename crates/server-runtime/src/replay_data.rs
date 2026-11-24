//! Recording and replaying: what a cellapp with a recording space writes
//! each tick, how those tick blobs are framed into a replay file, and how
//! the file is walked back out — tick boundaries and block order intact.
//!
//! Ported from `cellapp/replay_data_collector.cpp` (the record side),
//! `baseapp/replay_data_file_writer.cpp` (tick framing and chunking), and
//! `lib/connection/replay_data_file_reader.cpp` + `replay_controller.cpp`
//! (the playback parse). The tick boundary is `Cell::tickRecording`
//! (`cell.cpp:1089-1136`): volatile updates drain first, the accumulated
//! blob becomes the tick's data, then the collector clears — buffered
//! per-entity data survives.
//!
//! Harness simplifications, matching lane precedent: compression is
//! identity (`CompressionOStream` with `BW_COMPRESSION_NONE` is a pure
//! pass-through in the reference too), checksum signatures are opaque
//! fixed-length byte runs, and the background file writer is an
//! in-memory buffer whose write completions apply immediately.

use std::collections::BTreeMap;

use foundation::stream::BinaryStream;
use mercury::endpoint::Address;

/// `ReplayData::BlockType` (`connection/replay_data.hpp`).
pub mod block_type {
    pub const ENTITY_CREATE: u8 = 0;
    pub const ENTITY_VOLATILE: u8 = 1;
    pub const ENTITY_VOLATILE_ON_GROUND: u8 = 2;
    pub const ENTITY_METHOD: u8 = 3;
    pub const ENTITY_PROPERTY_CHANGE: u8 = 4;
    pub const ENTITY_DELETE: u8 = 5;
    pub const ENTITY_PLAYER_STATE_CHANGE: u8 = 6;
    pub const PLAYER_AOI_CHANGE: u8 = 7;
    pub const SPACE_DATA: u8 = 8;
    pub const FINAL: u8 = 9;
}

/// `SpaceEntryID` — a `Mercury::Address` subclass in the reference.
pub type SpaceEntryId = Address;

pub type EntityId = u32;
pub type GameTime = u32;

/// `TickReplayData::tickHeaderSize` — a game time plus a data length.
const TICK_HEADER_SIZE: usize = 8;

/// `(yaw, pitch, roll)` in field order; `Direction3D` streams as roll,
/// pitch, yaw (`basictypes.cpp:29-32`).
type Direction = (f32, f32, f32);

fn put_direction(out: &mut BinaryStream, direction: Direction) {
    let (yaw, pitch, roll) = direction;
    out.write_f32(roll);
    out.write_f32(pitch);
    out.write_f32(yaw);
}

/// A client-server property exposed for replay, already serialized.
/// `None` is an exposed base property whose value is not available yet
/// (`replay_data_collector.cpp:109-116`).
pub type ReplayProperty = Option<Vec<u8>>;

/// The entity face the collector records against. The reference reads
/// these off `Entity`/`RealEntity`; the harness supplies them.
#[derive(Clone, Debug)]
pub struct ReplayEntity {
    pub id: EntityId,
    pub client_type_id: u16,
    pub vehicle_id: EntityId,
    pub position: (f32, f32, f32),
    pub direction: Direction,
    pub is_on_ground: bool,
    /// `EntityDescription::canBeOnClient`.
    pub can_be_on_client: bool,
    /// `EntityDescription::isClientType`.
    pub is_client_type: bool,
    /// `entity.pReal()->pWitness() != NULL`.
    pub has_witness: bool,
    /// `RealEntity::recordingSpaceEntryID()` — the recording span this
    /// entity's state was last stamped into.
    pub recording_space_entry_id: SpaceEntryId,
    /// Client-server properties in `clientServerProperty` order.
    pub replay_properties: Vec<ReplayProperty>,
    /// The witness's AoI roster in visit order, recorded as the create's
    /// inline player state change (`AoIEntityIDCollector` in the
    /// reference).
    pub aoi_entity_ids: Vec<EntityId>,
}

impl ReplayEntity {
    pub fn new(id: EntityId, client_type_id: u16) -> Self {
        ReplayEntity {
            id,
            client_type_id,
            vehicle_id: 0,
            position: (0.0, 0.0, 0.0),
            direction: (0.0, 0.0, 0.0),
            is_on_ground: false,
            can_be_on_client: true,
            is_client_type: true,
            has_witness: false,
            recording_space_entry_id: SpaceEntryId::default(),
            replay_properties: Vec::new(),
            aoi_entity_ids: Vec::new(),
        }
    }
}

/// What `queueEntityVolatile` needs at drain time.
#[derive(Clone, Debug)]
struct VolatileEntity {
    position: (f32, f32, f32),
    direction: Direction,
    is_on_ground: bool,
    vehicle_id: EntityId,
}

/// The per-tick collector (`ReplayDataCollector`).
#[derive(Debug)]
pub struct ReplayDataCollector {
    name: String,
    should_record_aoi_events: bool,
    data: BinaryStream,
    recording_space_entry_id: SpaceEntryId,
    /// `BW::map` — drained in entity id order.
    moved_entities: BTreeMap<EntityId, VolatileEntity>,
    /// Blocks held back until their entity's create lands. Survives
    /// `clear()` — buffering is not tick-scoped.
    buffered_entity_data: BTreeMap<EntityId, BinaryStream>,
}

impl ReplayDataCollector {
    pub fn new(name: &str, should_record_aoi_events: bool) -> Self {
        ReplayDataCollector {
            name: name.to_string(),
            should_record_aoi_events,
            data: BinaryStream::new(),
            recording_space_entry_id: SpaceEntryId::default(),
            moved_entities: BTreeMap::new(),
            buffered_entity_data: BTreeMap::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The blocks accumulated since the last clear.
    pub fn data(&self) -> &[u8] {
        self.data.unread_bytes()
    }

    pub fn recording_space_entry_id(&self) -> SpaceEntryId {
        self.recording_space_entry_id
    }

    pub fn set_recording_space_entry_id(&mut self, entry_id: SpaceEntryId) {
        self.recording_space_entry_id = entry_id;
    }

    /// Entity ids with blocks still held back.
    pub fn buffered_entity_ids(&self) -> Vec<EntityId> {
        self.buffered_entity_data.keys().copied().collect()
    }

    /// `getStreamForEntity` — the entity's own buffer while it is not
    /// yet recorded, otherwise the main tick stream.
    fn stream_for_entity(&mut self, entity_id: EntityId, is_recorded: bool) -> &mut BinaryStream {
        if is_recorded {
            return &mut self.data;
        }
        self.buffered_entity_data.entry(entity_id).or_default()
    }

    /// `addEntityState` — the entity's full create block. Stamps the
    /// recording span first, so a non-client entity is still marked as
    /// recorded and later property sets skip.
    pub fn add_entity_state(&mut self, entity: &mut ReplayEntity) {
        if entity.recording_space_entry_id == self.recording_space_entry_id {
            return;
        }

        entity.recording_space_entry_id = self.recording_space_entry_id;

        if !entity.can_be_on_client {
            return;
        }

        self.data.write_u8(block_type::ENTITY_CREATE);
        self.data.write_u32(entity.vehicle_id);
        // The nested compressed section; identity in this port.
        self.data.write_u32(entity.id);
        self.data.write_u16(entity.client_type_id);
        self.data.write_f32(entity.position.0);
        self.data.write_f32(entity.position.1);
        self.data.write_f32(entity.position.2);
        // Same order as what Witness::sendCreate would generate: yaw,
        // pitch, roll (`replay_data_collector.cpp:91-93`).
        self.data.write_f32(entity.direction.0);
        self.data.write_f32(entity.direction.1);
        self.data.write_f32(entity.direction.2);

        let to_send: Vec<(u8, &[u8])> = entity
            .replay_properties
            .iter()
            .enumerate()
            .filter_map(|(i, prop)| prop.as_ref().map(|bytes| (i as u8, bytes.as_slice())))
            .collect();
        debug_assert!(to_send.len() < 256, "MF_ASSERT( numToSend < 256 )");
        self.data.write_u8(to_send.len() as u8);
        for (index, bytes) in to_send {
            self.data.write_u8(index);
            self.data.write_bytes(bytes);
        }

        if entity.has_witness {
            let roster = entity.aoi_entity_ids.clone();
            self.add_entity_player_state_change(entity.id, entity.has_witness, &roster);
        }

        if let Some(buffer) = self.buffered_entity_data.remove(&entity.id) {
            self.data.write_bytes(buffer.as_bytes());
        }
    }

    /// `queueEntityVolatile` — the volatile block is emitted at the tick
    /// boundary, in entity id order.
    pub fn queue_entity_volatile(&mut self, entity: &ReplayEntity) {
        if !entity.is_client_type {
            return;
        }

        self.moved_entities.insert(
            entity.id,
            VolatileEntity {
                position: entity.position,
                direction: entity.direction,
                is_on_ground: entity.is_on_ground,
                vehicle_id: entity.vehicle_id,
            },
        );
    }

    /// `addVolatileDataForQueuedEntities`.
    pub fn add_volatile_data_for_queued_entities(&mut self) {
        for (id, entity) in &self.moved_entities {
            self.data.write_u8(if entity.is_on_ground {
                block_type::ENTITY_VOLATILE_ON_GROUND
            } else {
                block_type::ENTITY_VOLATILE
            });
            self.data.write_u32(*id);
            if entity.is_on_ground {
                self.data.write_f32(entity.position.0);
                self.data.write_f32(entity.position.2);
            } else {
                self.data.write_f32(entity.position.0);
                self.data.write_f32(entity.position.1);
                self.data.write_f32(entity.position.2);
            }
            put_direction(&mut self.data, entity.direction);
            self.data.write_u32(entity.vehicle_id);
        }
    }

    /// `addEntityMethod`. A non-client entity's payload is dropped.
    pub fn add_entity_method(&mut self, entity: &ReplayEntity, exposed_msg_id: u8, payload: &[u8]) {
        if !entity.can_be_on_client {
            // `data.finish()` — the payload is discarded.
            return;
        }

        let is_recorded = entity.recording_space_entry_id == self.recording_space_entry_id;
        let stream = self.stream_for_entity(entity.id, is_recorded);
        stream.write_u8(block_type::ENTITY_METHOD);
        stream.write_u32(entity.id);
        stream.write_u8(exposed_msg_id);
        stream.write_i32(payload.len() as i32);
        stream.write_bytes(payload);
    }

    /// `addEntityProperty`. Not exposed for replay, or the entity not
    /// yet recorded (its create will carry the state), means skip.
    pub fn add_entity_property(
        &mut self,
        entity: &ReplayEntity,
        exposed_for_replay: bool,
        is_slice: bool,
        payload: &[u8],
    ) {
        if !entity.can_be_on_client {
            return;
        }

        if !exposed_for_replay {
            return;
        }

        if entity.recording_space_entry_id != self.recording_space_entry_id {
            // Hasn't been added yet, e.g. a property set in __init__;
            // addEntityState() will be called soon.
            return;
        }

        self.data.write_u8(block_type::ENTITY_PROPERTY_CHANGE);
        self.data.write_u32(entity.id);
        self.data.write_u8(u8::from(is_slice));
        self.data.write_i32(payload.len() as i32);
        self.data.write_bytes(payload);
    }

    /// `addEntityProperties` — exposed base properties at create time,
    /// one property change block per present value.
    pub fn add_entity_properties(&mut self, entity_id: EntityId, properties: Vec<(u8, Vec<u8>)>) {
        for (index, value) in properties {
            self.data.write_u8(block_type::ENTITY_PROPERTY_CHANGE);
            self.data.write_u32(entity_id);
            self.data.write_u8(0); // SinglePropertyChange: not a slice
            self.data.write_i32(value.len() as i32);
            self.data.write_bytes(&value);
            let _ = index;
        }
    }

    /// `addEntityPlayerStateChange`. `aoi_entity_ids` is the witness's
    /// AoI roster in visit order; pass it only for a witness-bearing
    /// player.
    pub fn add_entity_player_state_change(
        &mut self,
        entity_id: EntityId,
        has_witness: bool,
        aoi_entity_ids: &[EntityId],
    ) {
        if !self.should_record_aoi_events {
            return;
        }

        self.data.write_u8(block_type::ENTITY_PLAYER_STATE_CHANGE);
        self.data.write_u32(entity_id);

        if !has_witness {
            self.data.write_u8(0);
            return;
        }

        self.data.write_u8(1);
        self.data.write_u32(aoi_entity_ids.len() as u32);
        for id in aoi_entity_ids {
            self.data.write_u32(*id);
        }
    }

    /// `addEntityAoIChange`. Entry events buffer behind the player's
    /// create; exit events go straight to the tick stream (`before the
    /// entity deletion event, so no need to buffer`).
    pub fn add_entity_aoi_change(
        &mut self,
        player_id: EntityId,
        player_is_recorded: bool,
        entity_id: EntityId,
        has_entered_aoi: bool,
    ) {
        if !self.should_record_aoi_events {
            return;
        }

        let stream = if has_entered_aoi {
            self.stream_for_entity(player_id, player_is_recorded)
        } else {
            &mut self.data
        };
        stream.write_u8(block_type::PLAYER_AOI_CHANGE);
        stream.write_u32(player_id);
        stream.write_u32(entity_id);
        stream.write_u8(u8::from(has_entered_aoi));
    }

    /// `deleteEntity`.
    pub fn delete_entity(&mut self, entity_id: EntityId) {
        self.data.write_u8(block_type::ENTITY_DELETE);
        self.data.write_u32(entity_id);
        self.moved_entities.remove(&entity_id);
    }

    /// `addSpaceData`.
    pub fn add_space_data(&mut self, entry_id: SpaceEntryId, key: u16, value: &str) {
        self.data.write_u8(block_type::SPACE_DATA);
        // SpaceEntryID is a Mercury::Address: ip, port, salt.
        self.data.write_u32(entry_id.ip);
        self.data.write_u16(entry_id.port);
        self.data.write_u16(entry_id.salt);
        self.data.write_u16(key);
        let _ = self.data.write_string(value);
    }

    /// `addFinish` — the message that marks the final tick.
    pub fn add_finish(&mut self) {
        self.data.write_u8(block_type::FINAL);
    }

    /// `clear` — the tick's stream and volatile queue go; buffered
    /// per-entity data stays until its create lands.
    pub fn clear(&mut self) {
        self.data = BinaryStream::new();
        self.moved_entities.clear();
    }
}

/// `Cell::tickRecording` — drain the volatile queue, hand back this
/// tick's blob, then clear the collector.
pub fn tick_recording(collector: &mut ReplayDataCollector) -> Vec<u8> {
    collector.add_volatile_data_for_queued_entities();
    let blob = collector.data().to_vec();
    collector.clear();
    blob
}

// -----------------------------------------------------------------------------
// Section: TickReplayData
// -----------------------------------------------------------------------------

/// One game tick's worth of cell data being gathered (`TickReplayData`).
#[derive(Debug)]
pub struct TickReplayData {
    tick: GameTime,
    num_expected_cells: u32,
    num_cells_left: u32,
    /// Raw concatenated cell blobs until framed, then the final
    /// `[tick][length][payload]` record.
    record: Vec<u8>,
}

impl TickReplayData {
    fn new(tick: GameTime, num_cells: u32) -> Self {
        TickReplayData {
            tick,
            num_expected_cells: num_cells,
            num_cells_left: num_cells,
            record: Vec::new(),
        }
    }

    /// `numExpectedCells` — can only grow (MF_ASSERT in the reference).
    fn set_num_expected_cells(&mut self, new_value: u32) {
        debug_assert!(new_value > self.num_expected_cells);
        let delta = new_value - self.num_expected_cells;
        self.num_cells_left += delta;
        self.num_expected_cells = new_value;
    }

    /// `transferFromCell`.
    fn transfer_from_cell(&mut self, data: &[u8]) -> bool {
        if self.num_cells_left == 0 {
            // Already have a complete set of the cell data.
            return false;
        }
        self.record.extend_from_slice(data);
        self.num_cells_left -= 1;
        true
    }

    /// `finalise` — frame the tick record: game time, then the length
    /// of everything after it. Compression is identity in this port.
    fn finalise(&mut self) {
        let mut out = BinaryStream::new();
        out.write_u32(self.tick);
        out.write_u32(0);
        out.write_bytes(&self.record);
        let bytes = out.as_bytes().to_vec();
        let length = (bytes.len() - TICK_HEADER_SIZE) as u32;
        let mut record = bytes;
        record[4..8].copy_from_slice(&length.to_le_bytes());
        self.record = record;
    }

    fn framed_bytes(&self) -> &[u8] {
        &self.record
    }

    fn size(&self) -> usize {
        self.record.len()
    }

    /// `streamTickHeader` — an empty tick is just the header, length 0.
    fn stream_empty_header(out: &mut BinaryStream, tick: GameTime) {
        out.write_u32(tick);
        out.write_u32(0);
    }
}

// -----------------------------------------------------------------------------
// Section: ReplayDataFileWriter
// -----------------------------------------------------------------------------

/// Gathers tick data from the recording cells and frames it into signed
/// chunks of consecutive ticks (`ReplayDataFileWriter`). The file is an
/// in-memory buffer; write completions apply immediately.
#[derive(Debug)]
pub struct ReplayDataFileWriter {
    num_ticks_to_sign: u32,
    /// Opaque signature length — the reference's checksum stream size.
    signature_length: usize,
    /// Where the header's numTicks field sits in the file.
    num_ticks_field_offset: usize,
    /// The pre-serialized header + meta data the file starts with.
    header: Vec<u8>,
    /// Everything written after the header.
    body: BinaryStream,
    current_tick: GameTime,
    last_tick_pending_write: GameTime,
    last_tick_written: GameTime,
    last_chunk_position: usize,
    last_chunk_length: usize,
    buffered_tick_data: BTreeMap<GameTime, TickReplayData>,
    num_ticks_written: u32,
    is_finalising: bool,
    /// ERROR_MSG / NOTICE_MSG records, in order.
    pub warnings: Vec<String>,
}

impl ReplayDataFileWriter {
    /// Constructor for new replay data files. `header` is the caller's
    /// pre-serialized ReplayHeader + meta data chunk.
    pub fn new(
        header: Vec<u8>,
        num_ticks_to_sign: u32,
        signature_length: usize,
        num_ticks_field_offset: usize,
    ) -> Self {
        let start = header.len();
        ReplayDataFileWriter {
            num_ticks_to_sign,
            signature_length,
            num_ticks_field_offset,
            header,
            body: BinaryStream::new(),
            current_tick: 0,
            last_tick_pending_write: 0,
            last_tick_written: 0,
            last_chunk_position: start,
            last_chunk_length: 0,
            buffered_tick_data: BTreeMap::new(),
            num_ticks_written: 0,
            is_finalising: false,
            warnings: Vec::new(),
        }
    }

    pub fn is_finalising(&self) -> bool {
        self.is_finalising
    }

    pub fn num_ticks_written(&self) -> u32 {
        self.num_ticks_written
    }

    pub fn last_tick_written(&self) -> GameTime {
        self.last_tick_written
    }

    pub fn last_chunk_position(&self) -> usize {
        self.last_chunk_position
    }

    /// The file so far: header, then every completed chunk.
    pub fn file(&self) -> Vec<u8> {
        let mut out = self.header.clone();
        out.extend_from_slice(self.body.as_bytes());
        out
    }

    /// `addTickData` — one cell's contribution for one tick.
    pub fn add_tick_data(&mut self, tick: GameTime, num_cells: u32, blob: &[u8]) {
        if self.is_finalising {
            self.warnings
                .push(format!("addTickData: already finalising (tick {tick})"));
            return;
        }

        if tick < self.current_tick {
            // A new cell entering the space reported itself in a count
            // for a tick already written out.
            if !blob.is_empty() {
                self.warnings.push(format!(
                    "addTickData: dropped {} bytes from tick {tick} already written",
                    blob.len()
                ));
            }
            return;
        }

        if self.current_tick == 0 && self.buffered_tick_data.is_empty() {
            if blob.is_empty() {
                self.warnings.push(format!(
                    "addTickData: ignored the first tick with empty data (tick {tick})"
                ));
                return;
            }
            self.current_tick = tick;
            // Pretend we have written previous ticks.
            self.last_tick_written = tick - 1;
            self.last_tick_pending_write = tick - 1;
        }

        let is_new = !self.buffered_tick_data.contains_key(&tick);
        if is_new {
            self.buffered_tick_data
                .insert(tick, TickReplayData::new(tick, num_cells));
        }
        let entry = self.buffered_tick_data.get_mut(&tick).unwrap();
        if num_cells > entry.num_expected_cells {
            let delta = num_cells - entry.num_expected_cells;
            if delta > 1 {
                self.warnings.push(format!(
                    "numExpectedCells: {delta} cells were added, expected at most 1"
                ));
            }
            entry.set_num_expected_cells(num_cells);
        }
        if !entry.transfer_from_cell(blob) {
            self.warnings.push(format!(
                "transferFromCell: already have a complete set (tick {tick})"
            ));
            return;
        }

        if entry.num_cells_left == 0 {
            self.finalise_completed_tick_data(tick);
        }
    }

    /// `finaliseCompletedTickData` — frame this tick and every earlier
    /// pending one, even if earlier ticks are incomplete.
    fn finalise_completed_tick_data(&mut self, completed_tick: GameTime) {
        while self.current_tick <= completed_tick {
            if let Some(entry) = self.buffered_tick_data.get_mut(&self.current_tick) {
                if entry.num_cells_left > 0 {
                    self.warnings.push(format!(
                        "writing out incomplete tick {} before completed tick {}",
                        self.current_tick, completed_tick
                    ));
                }
                entry.finalise();
            }
            self.current_tick += 1;
        }

        self.sign_and_write_completed_tick_data(false);
    }

    /// `signAndWriteCompletedTickData` — emit chunks of consecutive
    /// ticks once enough have accumulated. Missing ticks become empty
    /// tick headers.
    fn sign_and_write_completed_tick_data(&mut self, is_last_chunk: bool) {
        if self.current_tick == 0 {
            // No data has been added yet.
            return;
        }

        let mut next_tick_to_write = self.last_tick_pending_write as u64 + 1;
        while next_tick_to_write < self.current_tick as u64
            && (is_last_chunk
                || (self.current_tick as u64 - next_tick_to_write) >= self.num_ticks_to_sign as u64)
        {
            let end = next_tick_to_write
                + self
                    .num_ticks_to_sign
                    .min((self.current_tick as u64 - next_tick_to_write) as u32)
                    as u64;

            // First pass: how big the chunk is (gaps are empty ticks,
            // all the same size).
            let mut chunk_length: u32 = 0;
            {
                let mut chunk_time = next_tick_to_write;
                let mut iter = self
                    .buffered_tick_data
                    .range(next_tick_to_write as GameTime..);
                let mut cur = iter.next();
                while chunk_time < end {
                    match cur {
                        Some((&tick, entry)) if tick as u64 == chunk_time => {
                            chunk_length += entry.size() as u32;
                            chunk_time += 1;
                            cur = iter.next();
                        }
                        Some((&tick, _)) => {
                            chunk_length +=
                                ((tick as u64 - chunk_time) * TICK_HEADER_SIZE as u64) as u32;
                            chunk_time = tick as u64;
                        }
                        None => {
                            chunk_length += TICK_HEADER_SIZE as u32;
                            chunk_time += 1;
                        }
                    }
                }
            }

            // Second pass: write the chunk out.
            let mut file_data = BinaryStream::new();
            file_data.write_u32(chunk_length);
            let mut chunk_time = next_tick_to_write;
            {
                let mut iter = self
                    .buffered_tick_data
                    .range(next_tick_to_write as GameTime..);
                let mut cur = iter.next();
                while chunk_time < end {
                    match cur {
                        Some((&tick, entry)) if tick as u64 == chunk_time => {
                            file_data.write_bytes(entry.framed_bytes());
                            chunk_time += 1;
                            cur = iter.next();
                        }
                        Some((&tick, _)) => {
                            while (chunk_time as GameTime) < tick {
                                TickReplayData::stream_empty_header(
                                    &mut file_data,
                                    chunk_time as GameTime,
                                );
                                chunk_time += 1;
                            }
                        }
                        None => {
                            while chunk_time < end {
                                TickReplayData::stream_empty_header(
                                    &mut file_data,
                                    chunk_time as GameTime,
                                );
                                chunk_time += 1;
                            }
                        }
                    }
                }
            }
            // The checksum signature rides at the end of the chunk.
            file_data.write_blob_raw(&vec![0u8; self.signature_length]);

            self.last_tick_pending_write = (chunk_time - 1) as GameTime;

            // queueWrite + the background completion, applied inline.
            let position_before = self.header.len() + self.body.length();
            self.body.write_bytes(file_data.as_bytes());
            let new_last_tick_written = self.last_tick_pending_write;
            let num_new_ticks = new_last_tick_written - self.last_tick_written;
            self.num_ticks_written += num_new_ticks;
            self.last_chunk_position = position_before + file_data.length();
            self.last_chunk_length = 4; // sizeof(SignedChunkLength)
            let mut last_chunk_length = 4usize;
            let written_range = (self.last_tick_written + 1)..=new_last_tick_written;
            let written: Vec<GameTime> = self
                .buffered_tick_data
                .range(written_range)
                .map(|(&tick, _)| tick)
                .collect();
            for tick in &written {
                last_chunk_length += self.buffered_tick_data[tick].size();
            }
            for tick in written {
                self.buffered_tick_data.remove(&tick);
            }
            last_chunk_length += self.signature_length;
            self.last_chunk_length = last_chunk_length;
            self.last_tick_written = new_last_tick_written;

            next_tick_to_write = self.last_tick_pending_write as u64 + 1;
        }
    }

    /// `finalise` — write the last chunk (signed regardless of the tick
    /// threshold) and patch the header's numTicks field.
    pub fn finalise(&mut self) {
        self.is_finalising = true;

        self.sign_and_write_completed_tick_data(true);

        let num_ticks_will_be_written =
            self.num_ticks_written + (self.last_tick_pending_write - self.last_tick_written);
        let offset = self.num_ticks_field_offset;
        let file = self.file();
        let mut out = file;
        out[offset..offset + 4].copy_from_slice(&num_ticks_will_be_written.to_le_bytes());
        self.header = out[..self.header.len()].to_vec();
        let body_start = self.header.len();
        self.body = BinaryStream::from_slice(&out[body_start..]);
    }
}

// -----------------------------------------------------------------------------
// Section: ReplayDataFileReader
// -----------------------------------------------------------------------------

/// What the reader reports as it walks the file.
#[derive(Clone, Debug, PartialEq)]
pub enum ReaderEvent {
    /// One game tick's data, decompressed (identity here).
    TickData {
        game_time: GameTime,
        payload: Vec<u8>,
    },
    /// `IReplayDataFileReaderListener::onReplayDataFileReaderError`.
    Error { kind: ReaderError, message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderError {
    /// The chunk signature did not verify.
    SignatureMismatch,
    /// Structural damage: bad headers, a game time gap, too many ticks.
    FileCorrupted,
}

/// Walks a replay file's chunk stream and hands back per-tick payloads
/// (`ReplayDataFileReader`'s Impl, verification elided).
#[derive(Debug)]
pub struct ReplayDataFileReader {
    signature_length: usize,
    /// `header_.numTicks()` — zero means the count is not known yet.
    num_ticks_expected: u32,
    first_game_time: GameTime,
    last_game_time: GameTime,
    num_ticks_read: u32,
    num_chunks_read: u32,
    num_bytes_read: usize,
    /// Bytes received but not yet parsed into a whole chunk.
    pending: Vec<u8>,
    pub events: Vec<ReaderEvent>,
}

impl ReplayDataFileReader {
    pub fn new(num_ticks_expected: u32, signature_length: usize) -> Self {
        ReplayDataFileReader {
            signature_length,
            num_ticks_expected,
            first_game_time: 0,
            last_game_time: 0,
            num_ticks_read: 0,
            num_chunks_read: 0,
            num_bytes_read: 0,
            pending: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn num_ticks_read(&self) -> u32 {
        self.num_ticks_read
    }

    pub fn num_chunks_read(&self) -> u32 {
        self.num_chunks_read
    }

    pub fn first_game_time(&self) -> GameTime {
        self.first_game_time
    }

    pub fn has_error(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, ReaderEvent::Error { .. }))
    }

    /// `addData` — parse every whole chunk now available.
    pub fn add_data(&mut self, data: &[u8]) {
        if self.has_error() {
            return;
        }

        self.pending.extend_from_slice(data);

        while self.pending.len() >= 4 {
            let (payload, consumed) = {
                let mut s = BinaryStream::from_slice(&self.pending);
                let chunk_length = s.read_u32().unwrap() as usize;
                if s.remaining() < chunk_length + self.signature_length {
                    return;
                }
                let payload = s.read_blob_raw(chunk_length).unwrap();
                let _signature = s.read_blob_raw(self.signature_length).unwrap();
                let consumed = self.pending.len() - s.remaining();
                (payload, consumed)
            };

            self.num_bytes_read += 4 + payload.len() + self.signature_length;
            self.num_chunks_read += 1;

            self.read_tick_data(payload);
            if self.has_error() {
                self.pending.clear();
                return;
            }
            self.pending.drain(..consumed);
        }
    }

    /// `readTickData` — walk one chunk's tick records. Game times must
    /// arrive contiguously (the writer fills gaps with empty ticks).
    fn read_tick_data(&mut self, payload: Vec<u8>) {
        let mut s = BinaryStream::from_slice(&payload);

        while s.remaining() > 0 {
            let game_time = match s.read_u32() {
                Ok(v) => v,
                Err(_) => {
                    self.push_error(
                        ReaderError::FileCorrupted,
                        "Could not read game time data header",
                    );
                    return;
                }
            };
            let data_length = match s.read_u32() {
                Ok(v) => v as usize,
                Err(_) => {
                    self.push_error(
                        ReaderError::FileCorrupted,
                        "Could not read game time data header",
                    );
                    return;
                }
            };

            if data_length > s.remaining() {
                self.push_error(
                    ReaderError::FileCorrupted,
                    "Could not read game time data header",
                );
                return;
            }

            if self.last_game_time != 0 && game_time != self.last_game_time + 1 {
                self.push_error(ReaderError::FileCorrupted, "Game time mismatch");
                return;
            }

            if self.num_ticks_expected != 0 && self.num_ticks_read == self.num_ticks_expected {
                self.push_error(ReaderError::FileCorrupted, "Extraneous tick data");
                return;
            }

            if self.first_game_time == 0 {
                self.first_game_time = game_time;
            }

            self.last_game_time = game_time;
            self.num_ticks_read += 1;

            // read_blob_raw cannot fail: the length was checked above.
            let tick_payload = s.read_blob_raw(data_length).unwrap();
            self.events.push(ReaderEvent::TickData {
                game_time,
                payload: tick_payload,
            });
        }
    }

    fn push_error(&mut self, kind: ReaderError, message: &str) {
        self.events.push(ReaderEvent::Error {
            kind,
            message: message.to_string(),
        });
    }
}

// -----------------------------------------------------------------------------
// Section: Tick block parser
// -----------------------------------------------------------------------------

/// The entity-definition knowledge `ReplayController` gets from the
/// client defs: per client type, the serialized size of each exposed
/// replay property, indexed by its `clientServerProperty` index. The
/// create block's property values carry no length prefix on the wire —
/// only the defs can delimit them.
#[derive(Debug, Default)]
pub struct ReplayDefs {
    pub property_sizes: BTreeMap<u16, Vec<usize>>,
}

/// A parsed tick block (`ReplayController::processTick`'s dispatch).
#[derive(Clone, Debug, PartialEq)]
pub enum ReplayBlock {
    EntityCreate {
        vehicle_id: EntityId,
        id: EntityId,
        client_type_id: u16,
        position: (f32, f32, f32),
        yaw: f32,
        pitch: f32,
        roll: f32,
        properties: Vec<(u8, Vec<u8>)>,
    },
    EntityVolatile {
        id: EntityId,
        position: (f32, f32, f32),
        direction: Direction,
        vehicle_id: EntityId,
    },
    EntityVolatileOnGround {
        id: EntityId,
        x: f32,
        z: f32,
        direction: Direction,
        vehicle_id: EntityId,
    },
    EntityMethod {
        id: EntityId,
        exposed_msg_id: u8,
        payload: Vec<u8>,
    },
    EntityPropertyChange {
        id: EntityId,
        is_slice: bool,
        payload: Vec<u8>,
    },
    EntityPlayerStateChange {
        id: EntityId,
        has_witness: bool,
        entity_ids: Vec<EntityId>,
    },
    PlayerAoiChange {
        player_id: EntityId,
        entity_id: EntityId,
        has_entered_aoi: bool,
    },
    EntityDelete(EntityId),
    SpaceData {
        entry_id: SpaceEntryId,
        key: u16,
        value: String,
    },
    Final,
    /// An unknown block type — the reference destroys the controller.
    Invalid(u8),
}

/// `ReplayController::processTick` — parse one tick's blocks in order.
/// An empty tick is expected (all cellapps in a space died at once).
/// Stops at `Final`.
pub fn process_tick(data: &[u8], defs: &ReplayDefs) -> Vec<ReplayBlock> {
    let mut blocks = Vec::new();

    if data.is_empty() {
        // Empty ticks are expected if CellApps die in a space all at
        // one time.
        return blocks;
    }

    let mut s = BinaryStream::from_slice(data);

    while s.remaining() > 1 {
        let block_type = match s.read_u8() {
            Ok(v) => v,
            Err(_) => break,
        };

        let block = match block_type {
            block_type::ENTITY_CREATE => parse_entity_create(&mut s, defs),
            block_type::ENTITY_VOLATILE => parse_entity_volatile(&mut s),
            block_type::ENTITY_VOLATILE_ON_GROUND => parse_entity_volatile_on_ground(&mut s),
            block_type::ENTITY_METHOD => parse_entity_method(&mut s),
            block_type::ENTITY_PROPERTY_CHANGE => parse_entity_property_change(&mut s),
            block_type::ENTITY_PLAYER_STATE_CHANGE => parse_entity_player_state_change(&mut s),
            block_type::PLAYER_AOI_CHANGE => parse_player_aoi_change(&mut s),
            block_type::ENTITY_DELETE => s.read_u32().ok().map(ReplayBlock::EntityDelete),
            block_type::SPACE_DATA => parse_space_data(&mut s),
            block_type::FINAL => {
                // Return as this is final.
                blocks.push(ReplayBlock::Final);
                return blocks;
            }
            _ => Some(ReplayBlock::Invalid(block_type)),
        };

        match block {
            Some(block) => blocks.push(block),
            // A read failure poisons the rest, like tickData.error().
            None => break,
        }
    }

    blocks
}

fn parse_entity_create(s: &mut BinaryStream, defs: &ReplayDefs) -> Option<ReplayBlock> {
    let vehicle_id = s.read_u32().ok()?;
    let id = s.read_u32().ok()?;
    let client_type_id = s.read_u16().ok()?;
    let position = (s.read_f32().ok()?, s.read_f32().ok()?, s.read_f32().ok()?);
    let yaw = s.read_f32().ok()?;
    let pitch = s.read_f32().ok()?;
    let roll = s.read_f32().ok()?;

    let count = s.read_u8().ok()? as usize;
    let sizes = defs.property_sizes.get(&client_type_id);

    let mut properties = Vec::with_capacity(count);
    for _ in 0..count {
        let index = s.read_u8().ok()?;
        let size = sizes.and_then(|table| table.get(index as usize)).copied()?;
        let value = s.read_blob_raw(size).ok()?;
        properties.push((index, value));
    }

    Some(ReplayBlock::EntityCreate {
        vehicle_id,
        id,
        client_type_id,
        position,
        yaw,
        pitch,
        roll,
        properties,
    })
}

fn parse_entity_volatile(s: &mut BinaryStream) -> Option<ReplayBlock> {
    let id = s.read_u32().ok()?;
    let position = (s.read_f32().ok()?, s.read_f32().ok()?, s.read_f32().ok()?);
    let direction = (s.read_f32().ok()?, s.read_f32().ok()?, s.read_f32().ok()?);
    let vehicle_id = s.read_u32().ok()?;
    Some(ReplayBlock::EntityVolatile {
        id,
        position,
        direction,
        vehicle_id,
    })
}

fn parse_entity_volatile_on_ground(s: &mut BinaryStream) -> Option<ReplayBlock> {
    let id = s.read_u32().ok()?;
    let x = s.read_f32().ok()?;
    let z = s.read_f32().ok()?;
    let direction = (s.read_f32().ok()?, s.read_f32().ok()?, s.read_f32().ok()?);
    let vehicle_id = s.read_u32().ok()?;
    Some(ReplayBlock::EntityVolatileOnGround {
        id,
        x,
        z,
        direction,
        vehicle_id,
    })
}

fn parse_entity_method(s: &mut BinaryStream) -> Option<ReplayBlock> {
    let id = s.read_u32().ok()?;
    let exposed_msg_id = s.read_u8().ok()?;
    let length = s.read_i32().ok()? as usize;
    let payload = s.read_blob_raw(length).ok()?;
    Some(ReplayBlock::EntityMethod {
        id,
        exposed_msg_id,
        payload,
    })
}

fn parse_entity_property_change(s: &mut BinaryStream) -> Option<ReplayBlock> {
    let id = s.read_u32().ok()?;
    let is_slice = s.read_u8().ok()? != 0;
    let length = s.read_i32().ok()? as usize;
    let payload = s.read_blob_raw(length).ok()?;
    Some(ReplayBlock::EntityPropertyChange {
        id,
        is_slice,
        payload,
    })
}

fn parse_entity_player_state_change(s: &mut BinaryStream) -> Option<ReplayBlock> {
    let id = s.read_u32().ok()?;
    let has_witness = s.read_u8().ok()? != 0;
    let mut entity_ids = Vec::new();
    if has_witness {
        let count = s.read_u32().ok()?;
        for _ in 0..count {
            entity_ids.push(s.read_u32().ok()?);
        }
    }
    Some(ReplayBlock::EntityPlayerStateChange {
        id,
        has_witness,
        entity_ids,
    })
}

fn parse_player_aoi_change(s: &mut BinaryStream) -> Option<ReplayBlock> {
    let player_id = s.read_u32().ok()?;
    let entity_id = s.read_u32().ok()?;
    let has_entered_aoi = s.read_u8().ok()? != 0;
    Some(ReplayBlock::PlayerAoiChange {
        player_id,
        entity_id,
        has_entered_aoi,
    })
}

fn parse_space_data(s: &mut BinaryStream) -> Option<ReplayBlock> {
    let entry_id = SpaceEntryId {
        ip: s.read_u32().ok()?,
        port: s.read_u16().ok()?,
        salt: s.read_u16().ok()?,
    };
    let key = s.read_u16().ok()?;
    let value = s.read_string().ok()?;
    Some(ReplayBlock::SpaceData {
        entry_id,
        key,
        value,
    })
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn entry_id(port: u16) -> SpaceEntryId {
        // Distinct ports: Mercury::Address equality ignores the salt,
        // so two entry ids on the same ip:port are the same id.
        SpaceEntryId {
            ip: 0x0a00_0001,
            port: 20000 + port,
            salt: 0,
        }
    }

    fn defs() -> ReplayDefs {
        // Type 1 exposes one 4-byte property; type 2 exposes three
        // (indices 0..=2, 4 bytes each).
        ReplayDefs {
            property_sizes: BTreeMap::from([(1, vec![4]), (2, vec![4, 4, 4])]),
        }
    }

    fn collector() -> ReplayDataCollector {
        let mut collector = ReplayDataCollector::new("recording", true);
        collector.set_recording_space_entry_id(entry_id(7));
        collector
    }

    /// Read back the block stream with the type dispatch, for tests
    /// that only care about block-type ordering.
    fn block_types(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut s = BinaryStream::from_slice(data);
        while s.remaining() > 1 {
            match s.read_u8() {
                Ok(block_type::ENTITY_CREATE) => {
                    out.push(block_type::ENTITY_CREATE);
                    s.finish();
                }
                Ok(block_type::ENTITY_DELETE) => {
                    out.push(block_type::ENTITY_DELETE);
                    let _ = s.read_u32();
                }
                Ok(block_type::FINAL) => {
                    out.push(block_type::FINAL);
                    break;
                }
                Ok(t) => {
                    out.push(t);
                    break;
                }
                Err(_) => break,
            }
        }
        out
    }

    #[test]
    fn the_create_block_carries_the_full_state_in_order() {
        let mut collector = collector();
        assert_eq!(collector.recording_space_entry_id(), entry_id(7));
        let mut entity = ReplayEntity::new(10, 1);
        entity.vehicle_id = 3;
        entity.position = (1.5, 2.0, 3.5);
        entity.direction = (0.25, 0.5, 0.75);
        entity.replay_properties = vec![Some(vec![1, 2, 3, 4])];

        collector.add_entity_state(&mut entity);

        let blocks = process_tick(collector.data(), &defs());
        assert_eq!(
            blocks,
            vec![ReplayBlock::EntityCreate {
                vehicle_id: 3,
                id: 10,
                client_type_id: 1,
                position: (1.5, 2.0, 3.5),
                yaw: 0.25,
                pitch: 0.5,
                roll: 0.75,
                properties: vec![(0, vec![1, 2, 3, 4])],
            }]
        );
    }

    #[test]
    fn the_same_recording_span_does_not_recreate_the_entity() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);

        collector.add_entity_state(&mut entity);
        assert_eq!(
            block_types(collector.data()),
            vec![block_type::ENTITY_CREATE]
        );

        collector.clear();
        collector.add_entity_state(&mut entity);
        assert!(collector.data().is_empty());
    }

    #[test]
    fn a_new_recording_span_recreates_the_entity() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);
        collector.add_entity_state(&mut entity);

        let mut collector2 = ReplayDataCollector::new("second", true);
        collector2.set_recording_space_entry_id(entry_id(9));
        collector2.add_entity_state(&mut entity);
        assert_eq!(
            block_types(collector2.data()),
            vec![block_type::ENTITY_CREATE]
        );
    }

    #[test]
    fn a_non_client_entity_is_stamped_but_not_recorded() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);
        entity.can_be_on_client = false;

        collector.add_entity_state(&mut entity);
        assert!(collector.data().is_empty());
        // The stamp happened before the client-type check, so a later
        // property set knows the create is coming and skips.
        assert!(entity.recording_space_entry_id == entry_id(7));

        collector.add_entity_property(&entity, true, false, &[1, 2]);
        assert!(collector.data().is_empty());
    }

    #[test]
    fn an_exposed_base_property_without_a_value_is_omitted() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 2);
        entity.replay_properties = vec![None, Some(vec![7, 0, 0, 0]), None];

        collector.add_entity_state(&mut entity);

        let blocks = process_tick(collector.data(), &defs());
        let ReplayBlock::EntityCreate { properties, .. } = &blocks[0] else {
            panic!("expected create, got {:?}", blocks[0]);
        };
        assert_eq!(properties, &[(1u8, vec![7u8, 0, 0, 0])]);
    }

    #[test]
    fn volatile_updates_drain_in_entity_order_at_the_tick_boundary() {
        let mut collector = collector();

        // A non-client entity never queues.
        let mut offscreen = ReplayEntity::new(40, 1);
        offscreen.is_client_type = false;
        collector.queue_entity_volatile(&offscreen);

        // Queue out of id order; both variants.
        let mut air = ReplayEntity::new(30, 1);
        air.position = (3.0, 40.0, 5.0);
        air.direction = (0.1, 0.2, 0.3);
        air.vehicle_id = 9;
        let mut ground = ReplayEntity::new(20, 1);
        ground.position = (7.0, 0.0, 8.0);
        ground.is_on_ground = true;
        collector.queue_entity_volatile(&air);
        collector.queue_entity_volatile(&ground);

        let blob = tick_recording(&mut collector);

        assert_eq!(
            blob,
            [
                &[block_type::ENTITY_VOLATILE_ON_GROUND][..],
                &20u32.to_le_bytes()[..],
                &7.0f32.to_le_bytes()[..],
                &8.0f32.to_le_bytes()[..],
                &0.0f32.to_le_bytes()[..], // roll
                &0.0f32.to_le_bytes()[..], // pitch
                &0.0f32.to_le_bytes()[..], // yaw
                &0u32.to_le_bytes()[..],
                &[block_type::ENTITY_VOLATILE][..],
                &30u32.to_le_bytes()[..],
                &3.0f32.to_le_bytes()[..],
                &40.0f32.to_le_bytes()[..],
                &5.0f32.to_le_bytes()[..],
                // Direction3D streams roll, pitch, yaw: direction
                // (yaw, pitch, roll) = (0.1, 0.2, 0.3) goes out reversed.
                &0.3f32.to_le_bytes()[..],
                &0.2f32.to_le_bytes()[..],
                &0.1f32.to_le_bytes()[..],
                &9u32.to_le_bytes()[..],
            ]
            .concat()[..]
        );

        // The queue cleared with the tick.
        let blob = tick_recording(&mut collector);
        assert!(blob.is_empty());
    }

    #[test]
    fn a_deleted_entity_leaves_the_volatile_queue() {
        let mut collector = collector();
        let entity = ReplayEntity::new(5, 1);
        collector.queue_entity_volatile(&entity);
        collector.delete_entity(5);

        let blob = tick_recording(&mut collector);
        // The delete block is there; the volatile block is not.
        assert_eq!(
            blob,
            [&[block_type::ENTITY_DELETE][..], &5u32.to_le_bytes()[..]].concat()[..]
        );
    }

    #[test]
    fn method_and_property_blocks_are_length_prefixed() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);
        collector.add_entity_state(&mut entity);
        collector.clear();

        collector.add_entity_method(&entity, 42, &[9, 9, 9]);
        collector.add_entity_property(&entity, true, false, &[4, 5, 6, 7]);
        // Not exposed for replay: skipped.
        collector.add_entity_property(&entity, false, false, &[1]);
        // Exposed but the entity is not recorded yet: skipped, the
        // create will carry it.
        let fresh = ReplayEntity::new(11, 1);
        collector.add_entity_property(&fresh, true, false, &[1]);

        let blocks = process_tick(collector.data(), &defs());
        assert_eq!(
            blocks,
            vec![
                ReplayBlock::EntityMethod {
                    id: 10,
                    exposed_msg_id: 42,
                    payload: vec![9, 9, 9],
                },
                ReplayBlock::EntityPropertyChange {
                    id: 10,
                    is_slice: false,
                    payload: vec![4, 5, 6, 7],
                },
            ]
        );
    }

    #[test]
    fn a_non_client_entity_drops_methods_and_skips_properties() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);
        entity.can_be_on_client = false;

        collector.add_entity_method(&entity, 42, &[9]);
        assert!(collector.data().is_empty());
    }

    #[test]
    fn pre_create_data_is_buffered_behind_the_create() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);

        // Before the create: the method and an entry AoI change buffer.
        collector.add_entity_method(&entity, 42, &[1, 2]);
        collector.add_entity_aoi_change(10, false, 20, true);
        assert_eq!(collector.buffered_entity_ids(), vec![10]);

        // Buffering survives a tick boundary.
        let blob = tick_recording(&mut collector);
        assert!(blob.is_empty());
        assert_eq!(collector.buffered_entity_ids(), vec![10]);

        collector.add_entity_state(&mut entity);
        let blocks = process_tick(collector.data(), &defs());
        assert_eq!(
            blocks,
            vec![
                ReplayBlock::EntityCreate {
                    vehicle_id: 0,
                    id: 10,
                    client_type_id: 1,
                    position: (0.0, 0.0, 0.0),
                    yaw: 0.0,
                    pitch: 0.0,
                    roll: 0.0,
                    properties: vec![],
                },
                ReplayBlock::EntityMethod {
                    id: 10,
                    exposed_msg_id: 42,
                    payload: vec![1, 2],
                },
                ReplayBlock::PlayerAoiChange {
                    player_id: 10,
                    entity_id: 20,
                    has_entered_aoi: true,
                },
            ]
        );
        assert!(collector.buffered_entity_ids().is_empty());
    }

    #[test]
    fn aoi_exit_events_do_not_buffer() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);

        collector.add_entity_aoi_change(10, false, 20, false);
        let blocks = process_tick(collector.data(), &defs());
        assert_eq!(
            blocks,
            vec![ReplayBlock::PlayerAoiChange {
                player_id: 10,
                entity_id: 20,
                has_entered_aoi: false,
            }]
        );

        // A recorded player's entry goes straight to the tick stream.
        collector.add_entity_state(&mut entity);
        collector.clear();
        collector.add_entity_aoi_change(10, true, 21, true);
        let blocks = process_tick(collector.data(), &defs());
        assert!(matches!(blocks[0], ReplayBlock::PlayerAoiChange { .. }));
    }

    #[test]
    fn the_aoi_recording_gate_silences_player_events() {
        let mut collector = ReplayDataCollector::new("no aoi", false);
        collector.set_recording_space_entry_id(entry_id(7));

        let mut entity = ReplayEntity::new(10, 1);
        entity.has_witness = true;
        collector.add_entity_state(&mut entity);
        collector.add_entity_aoi_change(10, false, 20, true);

        // Only the create block; the create's inline player state
        // change is gated off too.
        assert_eq!(
            block_types(collector.data()),
            vec![block_type::ENTITY_CREATE]
        );
    }

    #[test]
    fn the_player_state_change_carries_the_aoi_roster() {
        let mut collector = collector();

        collector.add_entity_player_state_change(10, true, &[3, 1, 2]);
        collector.add_entity_player_state_change(11, false, &[]);

        let blocks = process_tick(collector.data(), &defs());
        assert_eq!(
            blocks,
            vec![
                ReplayBlock::EntityPlayerStateChange {
                    id: 10,
                    has_witness: true,
                    // Visitor order preserved, not sorted.
                    entity_ids: vec![3, 1, 2],
                },
                ReplayBlock::EntityPlayerStateChange {
                    id: 11,
                    has_witness: false,
                    entity_ids: vec![],
                },
            ]
        );
    }

    #[test]
    fn the_tick_boundary_keeps_ticks_disjoint_and_ordered() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);

        // Tick 1: create plus a method, and a volatile queued mid-tick.
        collector.add_entity_state(&mut entity);
        collector.add_entity_method(&entity, 42, &[1]);
        collector.queue_entity_volatile(&entity);
        let tick1 = tick_recording(&mut collector);

        // Tick 2: a property change only.
        collector.add_entity_property(&entity, true, true, &[8]);
        let tick2 = tick_recording(&mut collector);

        // Ordering inside tick 1: create, method, then the drained
        // volatile.
        let blocks1 = process_tick(&tick1, &defs());
        assert!(matches!(blocks1[0], ReplayBlock::EntityCreate { .. }));
        assert!(matches!(blocks1[1], ReplayBlock::EntityMethod { .. }));
        assert!(matches!(blocks1[2], ReplayBlock::EntityVolatile { .. }));

        // Tick 2 holds only its own block.
        assert_eq!(
            process_tick(&tick2, &defs()),
            vec![ReplayBlock::EntityPropertyChange {
                id: 10,
                is_slice: true,
                payload: vec![8],
            }]
        );
    }

    #[test]
    fn the_final_block_stops_the_tick_parse() {
        let mut collector = collector();
        let mut entity = ReplayEntity::new(10, 1);
        collector.add_entity_state(&mut entity);
        collector.delete_entity(10);
        collector.add_finish();
        // Post-FINAL noise must not parse.
        collector.add_space_data(entry_id(1), 3, "late");

        let blob = tick_recording(&mut collector);
        let blocks = process_tick(&blob, &defs());
        assert_eq!(
            blocks,
            vec![
                ReplayBlock::EntityCreate {
                    vehicle_id: 0,
                    id: 10,
                    client_type_id: 1,
                    position: (0.0, 0.0, 0.0),
                    yaw: 0.0,
                    pitch: 0.0,
                    roll: 0.0,
                    properties: vec![],
                },
                ReplayBlock::EntityDelete(10),
                ReplayBlock::Final,
            ]
        );
    }

    #[test]
    fn space_data_blocks_roundtrip_the_entry_and_value() {
        let mut collector = collector();
        collector.add_space_data(entry_id(5), 42, "gravity");

        let blocks = process_tick(collector.data(), &defs());
        assert_eq!(
            blocks,
            vec![ReplayBlock::SpaceData {
                entry_id: entry_id(5),
                key: 42,
                value: "gravity".to_string(),
            }]
        );
    }

    #[test]
    fn the_writer_gathers_cells_and_fills_missing_ticks() {
        let mut writer = ReplayDataFileWriter::new(b"hdr".to_vec(), 2, 0, 0);

        // First tick with an empty blob is ignored.
        writer.add_tick_data(1, 1, &[]);
        assert_eq!(writer.warnings.len(), 1);

        // Tick 1 needs two cells; one arrival is held.
        writer.add_tick_data(1, 2, &[1, 1]);
        assert_eq!(writer.last_tick_written(), 0);
        // Completed, but below the two-tick signing threshold: held.
        writer.add_tick_data(1, 2, &[1, 2]);
        assert_eq!(writer.last_tick_written(), 0);

        // Tick 3 completes and tips it over: a chunk of at most two
        // ticks goes out — tick 1 and an empty tick 2 — with tick 3
        // held for the next chunk.
        writer.add_tick_data(3, 1, &[3]);
        assert_eq!(writer.last_tick_written(), 2);

        // A late straggler for tick 2 is dropped.
        writer.add_tick_data(2, 1, &[2, 2]);
        assert!(writer.warnings.iter().any(|w| w.contains("dropped")));

        let file = writer.file();
        // Chunk: length, tick 1 record, tick 2 empty header.
        let s = &mut BinaryStream::from_slice(&file[3..]);
        let chunk_length = s.read_u32().unwrap();
        let tick1 = s.read_u32().unwrap();
        let tick1_len = s.read_u32().unwrap() as usize;
        let _ = s.read_blob_raw(tick1_len).unwrap();
        assert_eq!(tick1, 1);
        let tick2 = s.read_u32().unwrap();
        let tick2_len = s.read_u32().unwrap();
        assert_eq!(tick2, 2);
        assert_eq!(tick2_len, 0);
        // chunk_length covers the two records.
        assert_eq!(chunk_length as usize, (8 + tick1_len) + 8);
        assert_eq!(s.remaining(), 0);
    }

    #[test]
    fn an_incomplete_tick_flushes_when_a_later_tick_completes() {
        // Sign every completed tick straight away.
        let mut writer = ReplayDataFileWriter::new(b"hdr".to_vec(), 1, 0, 0);
        writer.add_tick_data(1, 1, &[1]);
        assert_eq!(writer.last_tick_written(), 1);

        // Tick 2 expects two cells but only one arrives...
        writer.add_tick_data(2, 2, &[2]);
        assert_eq!(writer.last_tick_written(), 1);

        // ...and tick 3 completing pushes the incomplete tick 2 out.
        writer.add_tick_data(3, 1, &[3]);
        assert_eq!(writer.last_tick_written(), 3);
        assert!(
            writer
                .warnings
                .iter()
                .any(|w| w.contains("incomplete tick 2"))
        );

        // Reading back: three ticks, tick 2 carries just cell one's blob.
        let file = writer.file();
        let mut reader = ReplayDataFileReader::new(0, 0);
        reader.add_data(&file[3..]);
        assert!(!reader.has_error());
        assert_eq!(reader.num_ticks_read(), 3);
        let payloads: Vec<Vec<u8>> = reader
            .events
            .iter()
            .filter_map(|e| match e {
                ReaderEvent::TickData { payload, .. } => Some(payload.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(payloads, vec![vec![1], vec![2], vec![3]]);
    }

    #[test]
    fn the_writer_only_signs_when_enough_ticks_accumulate() {
        let mut writer = ReplayDataFileWriter::new(b"hdr".to_vec(), 4, 0, 0);

        // Ticks 1..3: fewer than the four to sign, nothing written yet.
        for tick in 1..=3u32 {
            writer.add_tick_data(tick, 1, &[tick as u8]);
        }
        assert_eq!(writer.last_tick_written(), 0);

        // Tick 4 makes four ticks: the chunk goes out.
        writer.add_tick_data(4, 1, &[4]);
        assert_eq!(writer.last_tick_written(), 4);
    }

    #[test]
    fn finalise_patches_the_tick_count_and_writes_the_tail() {
        let mut writer = ReplayDataFileWriter::new(b"header!!".to_vec(), 4, 2, 4);
        for tick in 1..=2u32 {
            writer.add_tick_data(tick, 1, &[tick as u8]);
        }
        assert_eq!(writer.last_tick_written(), 0); // held for signing

        writer.finalise();
        assert!(writer.is_finalising());

        // numTicks patched at offset 4.
        let file = writer.file();
        assert_eq!(&file[4..8], &2u32.to_le_bytes());

        // A late tick after finalising is dropped with a warning.
        writer.add_tick_data(9, 1, &[9]);
        assert!(writer.warnings.iter().any(|w| w.contains("finalising")));
    }

    #[test]
    fn the_full_roundtrip_preserves_tick_boundaries_and_event_order() {
        // The acceptance face: record three ticks, frame them, read
        // them back, and check every tick's block sequence — boundaries
        // and event order.
        let entry = entry_id(7);
        let mut collector = collector();

        // Tick 1: initial space data, the player create (with witness —
        // the roster rides along as the player state change), another
        // entity's create.
        collector.add_space_data(entry, 1, "settings");
        let mut player = ReplayEntity::new(10, 2);
        player.has_witness = true;
        player.position = (1.0, 2.0, 3.0);
        player.replay_properties = vec![Some(vec![10, 0, 0, 0]), None, None];
        player.aoi_entity_ids = vec![20];
        collector.add_entity_state(&mut player);
        let mut npc = ReplayEntity::new(20, 1);
        npc.position = (4.0, 0.0, 5.0);
        collector.add_entity_state(&mut npc);
        let tick1 = tick_recording(&mut collector);

        // Tick 2: a method, a volatile, a property change.
        collector.add_entity_method(&npc, 7, &[0xAA]);
        npc.position = (6.0, 0.0, 8.0);
        npc.is_on_ground = true;
        collector.queue_entity_volatile(&npc);
        collector.add_entity_property(&npc, true, false, &[1, 2, 3, 4]);
        let tick2 = tick_recording(&mut collector);

        // Tick 3: an AoI exit, the delete, then the recording stops.
        collector.add_entity_aoi_change(10, true, 20, false);
        collector.delete_entity(20);
        collector.add_finish();
        let tick3 = tick_recording(&mut collector);

        // Frame: two cells per tick, one chunk of three ticks.
        let mut writer = ReplayDataFileWriter::new(b"hdr".to_vec(), 3, 0, 0);
        for (tick, blob) in [(1u32, &tick1), (2, &tick2), (3, &tick3)] {
            writer.add_tick_data(tick, 2, blob);
            writer.add_tick_data(tick, 2, &[]);
        }

        // Read back and parse each tick.
        let file = writer.file();
        let mut reader = ReplayDataFileReader::new(0, 0);
        reader.add_data(&file[3..]);
        assert!(!reader.has_error());
        assert_eq!(reader.num_ticks_read(), 3);
        assert_eq!(reader.num_chunks_read(), 1);
        assert_eq!(reader.first_game_time(), 1);
        assert_eq!(writer.num_ticks_written(), 3);
        // The chunk write completed through to the end of the file.
        assert_eq!(writer.last_chunk_position(), file.len());
        assert_eq!(collector.name(), "recording");

        let ticks: Vec<Vec<ReplayBlock>> = reader
            .events
            .iter()
            .filter_map(|event| match event {
                ReaderEvent::TickData { payload, .. } => Some(process_tick(payload, &defs())),
                _ => None,
            })
            .collect();

        // Tick 1: space data, player create, roster, npc create.
        assert_eq!(
            ticks[0],
            vec![
                ReplayBlock::SpaceData {
                    entry_id: entry,
                    key: 1,
                    value: "settings".to_string(),
                },
                ReplayBlock::EntityCreate {
                    vehicle_id: 0,
                    id: 10,
                    client_type_id: 2,
                    position: (1.0, 2.0, 3.0),
                    yaw: 0.0,
                    pitch: 0.0,
                    roll: 0.0,
                    properties: vec![(0, vec![10, 0, 0, 0])],
                },
                ReplayBlock::EntityPlayerStateChange {
                    id: 10,
                    has_witness: true,
                    entity_ids: vec![20],
                },
                ReplayBlock::EntityCreate {
                    vehicle_id: 0,
                    id: 20,
                    client_type_id: 1,
                    position: (4.0, 0.0, 5.0),
                    yaw: 0.0,
                    pitch: 0.0,
                    roll: 0.0,
                    properties: vec![],
                },
            ]
        );

        // Tick 2: method, property change, then the on-ground volatile —
        // volatiles drain at the tick boundary, after every direct
        // block queued during the tick.
        assert_eq!(
            ticks[1],
            vec![
                ReplayBlock::EntityMethod {
                    id: 20,
                    exposed_msg_id: 7,
                    payload: vec![0xAA],
                },
                ReplayBlock::EntityPropertyChange {
                    id: 20,
                    is_slice: false,
                    payload: vec![1, 2, 3, 4],
                },
                ReplayBlock::EntityVolatileOnGround {
                    id: 20,
                    x: 6.0,
                    z: 8.0,
                    direction: (0.0, 0.0, 0.0),
                    vehicle_id: 0,
                },
            ]
        );

        // Tick 3: exit, delete — and the trailing lone FINAL byte is
        // NOT dispatched: the dispatch loop runs while more than one
        // byte remains (`replay_controller.cpp:1005-1007`), so a FINAL
        // with nothing after it stays unread. The replay really ends
        // when the stream does.
        assert_eq!(
            ticks[2],
            vec![
                ReplayBlock::PlayerAoiChange {
                    player_id: 10,
                    entity_id: 20,
                    has_entered_aoi: false,
                },
                ReplayBlock::EntityDelete(20),
            ]
        );
    }

    #[test]
    fn the_reader_rejects_a_game_time_gap() {
        // Two chunks whose ticks are not contiguous.
        let mut a = BinaryStream::new();
        a.write_u32(1);
        a.write_u32(1);
        a.write_bytes(&[1]);
        let mut chunk_a = BinaryStream::new();
        chunk_a.write_u32(a.length() as u32);
        chunk_a.write_bytes(a.as_bytes());

        let mut b = BinaryStream::new();
        b.write_u32(5); // should have been 2
        b.write_u32(0);
        let mut chunk_b = BinaryStream::new();
        chunk_b.write_u32(b.length() as u32);
        chunk_b.write_bytes(b.as_bytes());

        let mut reader = ReplayDataFileReader::new(0, 0);
        reader.add_data(chunk_a.as_bytes());
        assert!(!reader.has_error());
        reader.add_data(chunk_b.as_bytes());
        assert!(reader.has_error());
        assert!(reader.events.contains(&ReaderEvent::Error {
            kind: ReaderError::FileCorrupted,
            message: "Game time mismatch".to_string(),
        }));
    }

    #[test]
    fn the_reader_rejects_a_truncated_tick_length() {
        let mut s = BinaryStream::new();
        s.write_u32(1);
        s.write_u32(50); // claims more than the chunk holds
        s.write_bytes(&[1, 2, 3]);

        let mut chunk = BinaryStream::new();
        chunk.write_u32(s.length() as u32);
        chunk.write_bytes(s.as_bytes());

        let mut reader = ReplayDataFileReader::new(0, 0);
        reader.add_data(chunk.as_bytes());
        assert!(reader.has_error());
        assert_eq!(
            reader.events.last(),
            Some(&ReaderEvent::Error {
                kind: ReaderError::FileCorrupted,
                message: "Could not read game time data header".to_string(),
            })
        );
    }

    #[test]
    fn the_reader_holds_partial_chunks_until_they_complete() {
        let mut s = BinaryStream::new();
        s.write_u32(1);
        s.write_u32(1);
        s.write_bytes(&[7]);

        let mut chunk = BinaryStream::new();
        chunk.write_u32(s.length() as u32);
        chunk.write_bytes(s.as_bytes());

        let bytes = chunk.as_bytes();
        let mut reader = ReplayDataFileReader::new(0, 0);
        // Half now, half later.
        reader.add_data(&bytes[..4]);
        assert_eq!(reader.num_ticks_read(), 0);
        reader.add_data(&bytes[4..7]);
        assert_eq!(reader.num_ticks_read(), 0);
        reader.add_data(&bytes[7..]);
        assert_eq!(reader.num_ticks_read(), 1);
        assert_eq!(
            reader.events[0],
            ReaderEvent::TickData {
                game_time: 1,
                payload: vec![7],
            }
        );
    }

    #[test]
    fn base_property_changes_go_out_at_create_time() {
        // `addEntityProperties` — exposed base properties handed over at
        // create time, one property change block per present value.
        let mut collector = collector();
        collector.add_entity_properties(10, vec![(0, vec![1, 2, 3, 4]), (2, vec![5, 6, 7, 8])]);

        let blocks = process_tick(collector.data(), &defs());
        assert_eq!(
            blocks,
            vec![
                ReplayBlock::EntityPropertyChange {
                    id: 10,
                    is_slice: false,
                    payload: vec![1, 2, 3, 4],
                },
                ReplayBlock::EntityPropertyChange {
                    id: 10,
                    is_slice: false,
                    payload: vec![5, 6, 7, 8],
                },
            ]
        );
    }

    #[test]
    fn a_later_cell_count_grows_the_expected_set() {
        let mut writer = ReplayDataFileWriter::new(b"hdr".to_vec(), 1, 0, 0);

        // Two cells reported — the tick stays open — but the space
        // then grows to four, so one report adds two cells at once
        // and draws the error record.
        writer.add_tick_data(1, 2, &[1]);
        writer.add_tick_data(1, 4, &[2]);
        assert!(
            writer
                .warnings
                .iter()
                .any(|w| w.contains("2 cells were added, expected at most 1"))
        );

        // The grown tick now wants three more cells and is still held.
        assert_eq!(writer.last_tick_written(), 0);
        assert_eq!(writer.num_ticks_written(), 0);
        assert_eq!(writer.buffered_tick_data[&1].num_expected_cells, 4);

        // Finishing the set writes the tick (the one-tick threshold),
        // and a straggler cell still reporting into it is dropped
        // with the byte count on record.
        writer.add_tick_data(1, 4, &[3]);
        writer.add_tick_data(1, 4, &[4]);
        assert_eq!(writer.last_tick_written(), 1);
        writer.add_tick_data(1, 4, &[9, 9, 9, 9]);
        assert!(
            writer
                .warnings
                .iter()
                .any(|w| w.contains("dropped 4 bytes from tick 1 already written"))
        );
    }

    #[test]
    fn the_reader_ignores_data_after_an_error() {
        let mut good = BinaryStream::new();
        good.write_u32(1);
        good.write_u32(1);
        good.write_bytes(&[1]);
        let mut chunk_a = BinaryStream::new();
        chunk_a.write_u32(good.length() as u32);
        chunk_a.write_bytes(good.as_bytes());

        let mut bad = BinaryStream::new();
        bad.write_u32(9); // gap: should have been 2
        bad.write_u32(0);
        let mut chunk_b = BinaryStream::new();
        chunk_b.write_u32(bad.length() as u32);
        chunk_b.write_bytes(bad.as_bytes());

        let mut reader = ReplayDataFileReader::new(0, 0);
        reader.add_data(chunk_a.as_bytes());
        reader.add_data(chunk_b.as_bytes());
        assert!(reader.has_error());
        let events_after_error = reader.events.len();

        // Further data is not even parsed.
        reader.add_data(chunk_a.as_bytes());
        assert_eq!(reader.events.len(), events_after_error);
    }

    #[test]
    fn the_reader_rejects_truncated_tick_headers() {
        // A two-byte tail: the game time itself cannot be read.
        let mut a = BinaryStream::new();
        a.write_u32(1);
        a.write_u32(1);
        a.write_bytes(&[1]);
        a.write_bytes(&[0xAB, 0xCD]);
        let mut chunk_a = BinaryStream::new();
        chunk_a.write_u32(a.length() as u32);
        chunk_a.write_bytes(a.as_bytes());

        let mut reader = ReplayDataFileReader::new(0, 0);
        reader.add_data(chunk_a.as_bytes());
        assert_eq!(
            reader.events.last(),
            Some(&ReaderEvent::Error {
                kind: ReaderError::FileCorrupted,
                message: "Could not read game time data header".to_string(),
            })
        );

        // A four-byte tail: game time reads, the length does not.
        let mut b = BinaryStream::new();
        b.write_u32(1);
        b.write_u32(1);
        b.write_bytes(&[1]);
        b.write_u32(2);
        b.write_bytes(&[0xAB, 0xCD, 0xEF]);
        let mut chunk_b = BinaryStream::new();
        chunk_b.write_u32(b.length() as u32);
        chunk_b.write_bytes(b.as_bytes());

        let mut reader = ReplayDataFileReader::new(0, 0);
        reader.add_data(chunk_b.as_bytes());
        assert_eq!(
            reader.events.last(),
            Some(&ReaderEvent::Error {
                kind: ReaderError::FileCorrupted,
                message: "Could not read game time data header".to_string(),
            })
        );
    }

    #[test]
    fn ticks_beyond_the_header_count_are_extraneous() {
        let mut s = BinaryStream::new();
        for tick in 1..=2u32 {
            s.write_u32(tick);
            s.write_u32(0);
        }
        let mut chunk = BinaryStream::new();
        chunk.write_u32(s.length() as u32);
        chunk.write_bytes(s.as_bytes());

        let mut reader = ReplayDataFileReader::new(1, 0);
        reader.add_data(chunk.as_bytes());
        assert_eq!(reader.num_ticks_read(), 1);
        assert_eq!(
            reader.events.last(),
            Some(&ReaderEvent::Error {
                kind: ReaderError::FileCorrupted,
                message: "Extraneous tick data".to_string(),
            })
        );
    }

    #[test]
    fn odd_blocks_stop_the_tick_parse() {
        // An empty tick parses to nothing (all cellapps died at once).
        assert!(process_tick(&[], &defs()).is_empty());

        // An unknown block type is reported and stops the parse.
        assert_eq!(
            process_tick(&[0x7F, 0, 0, 0, 0], &defs()),
            vec![ReplayBlock::Invalid(0x7F)]
        );

        // A block whose body is truncated stops the parse silently.
        assert!(process_tick(&[block_type::ENTITY_DELETE, 1, 2], &defs()).is_empty());

        // Same through the block-type walker.
        assert_eq!(
            block_types(&[block_type::ENTITY_DELETE, 1, 2]),
            vec![block_type::ENTITY_DELETE]
        );
        // FINAL is dispatched here only because a byte follows it —
        // the walker stops while more than one byte remains.
        assert_eq!(
            block_types(&[
                block_type::ENTITY_DELETE,
                9,
                0,
                0,
                0,
                block_type::FINAL,
                block_type::SPACE_DATA,
                0,
                0,
                0,
                0,
            ]),
            vec![block_type::ENTITY_DELETE, block_type::FINAL]
        );
    }

    #[test]
    fn finalise_with_no_data_is_a_noop() {
        let mut writer = ReplayDataFileWriter::new(b"header!!".to_vec(), 4, 0, 4);
        writer.finalise();
        assert!(writer.is_finalising());
        assert_eq!(writer.num_ticks_written(), 0);
        // numTicks patched to zero; nothing else written.
        assert_eq!(&writer.file()[4..8], &0u32.to_le_bytes());
        assert_eq!(writer.file().len(), 8);
    }
}
