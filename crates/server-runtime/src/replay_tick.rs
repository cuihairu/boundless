//! BW-07-05-03: write compressed tick blocks.
//!
//! Each game tick's recorded events arrive at the BaseApp per cell
//! (`ReplayDataFileWriter::addTickData`, `replay_data_file_writer.cpp:179`).
//! A tick is complete once every expected cell has reported
//! (`TickReplayData::numCellsLeft`, `:258`), at which point the cell data is
//! frozen: `TickReplayData::finalise` (`:821`) wraps it in an 8-byte header of
//! game time and data length (`streamTickHeader`, `:890`,
//! `tickHeaderSize`, `:876`) followed by a `CompressionOStream` — a
//! compression-type tag byte and, for zip levels, the zlib stream framed with
//! packed original and compressed lengths (`compression_stream.cpp:95`,
//! `zip_stream.cpp:281`).
//!
//! Completed ticks are written out in signed chunks
//! (`signAndWriteCompletedTickData`, `:314`): once `numTicksToSign_` ticks have
//! accumulated, the chunk carries a `SignedChunkLength`, the tick blocks back
//! to back (ticks with no cell data are header-only placeholders, `:367`) and
//! a signature chained off the previous signature (`:337`). The reader
//! (`replay_data_file_reader.cpp:764`) verifies each chunk against that chain
//! and walks the ticks (`:845`), rejecting a game time that is not one past
//! the previous tick (`:877`) or a data length overrunning the chunk
//! (`:867`), then decompresses each payload, so the events recorded per tick
//! come back byte for byte.
//!
//! This module models the per-tick accumulator, the chunk writer and the
//! reader that recovers the recorded events losslessly.

use crate::replay_header::{ReplayChecksumScheme, ReplayError};
use foundation::stream::{BinaryStream, StreamError};
use std::collections::BTreeMap;
use std::io::{Read, Write};

/// `ReplayData::SignedChunkLength` (`replay_data.hpp:12`).
pub type SignedChunkLength = u32;

/// `ReplayData::TickDataLength` (`replay_data.hpp:13`).
pub type TickDataLength = u32;

/// `TickReplayData::tickHeaderSize` (`replay_data_file_writer.cpp:876`): the
/// game time and data length of a tick with no data.
pub const TICK_HEADER_SIZE: usize = 8;

/// `ReplayData::BLOCK_TYPE_*` (`replay_data.hpp:18`): the block types a
/// recorded cell data stream starts each record with (`replay_data_collector.cpp:83`).
pub mod block_type {
    /// `BLOCK_TYPE_ENTITY_CREATE`.
    pub const ENTITY_CREATE: u8 = 0;
    /// `BLOCK_TYPE_ENTITY_VOLATILE`.
    pub const ENTITY_VOLATILE: u8 = 1;
    /// `BLOCK_TYPE_ENTITY_VOLATILE_ON_GROUND`.
    pub const ENTITY_VOLATILE_ON_GROUND: u8 = 2;
    /// `BLOCK_TYPE_ENTITY_METHOD`.
    pub const ENTITY_METHOD: u8 = 3;
    /// `BLOCK_TYPE_ENTITY_PROPERTY_CHANGE`.
    pub const ENTITY_PROPERTY_CHANGE: u8 = 4;
    /// `BLOCK_TYPE_ENTITY_DELETE`.
    pub const ENTITY_DELETE: u8 = 5;
    /// `BLOCK_TYPE_ENTITY_PLAYER_STATE_CHANGE`.
    pub const ENTITY_PLAYER_STATE_CHANGE: u8 = 6;
    /// `BLOCK_TYPE_PLAYER_AOI_CHANGE`.
    pub const PLAYER_AOI_CHANGE: u8 = 7;
    /// `BLOCK_TYPE_SPACE_DATA`.
    pub const SPACE_DATA: u8 = 8;
    /// `BLOCK_TYPE_FINAL`.
    pub const FINAL: u8 = 9;
}

/// The compression a tick block's payload uses (`BWCompressionType`,
/// `compression_type.hpp:8`): none, or a zip level 1..=9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionType {
    /// `BW_COMPRESSION_NONE`: the payload follows the tag byte raw.
    None,
    /// A `BW_COMPRESSION_ZIP_n` level, 1 (`BEST_SPEED`) to 9 (`BEST_COMPRESSION`).
    Zip(u8),
}

impl CompressionType {
    /// The tag byte the `CompressionOStream` writes
    /// (`compression_stream.cpp:95`).
    pub fn tag(self) -> u8 {
        match self {
            CompressionType::None => 0,
            CompressionType::Zip(level) => level,
        }
    }

    /// `BW_COMPRESSION_ZIP_BEST_SPEED` (`compression_type.hpp:22`).
    pub fn zip_best_speed() -> Self {
        CompressionType::Zip(1)
    }

    /// `BW_COMPRESSION_ZIP_BEST_COMPRESSION` (`compression_type.hpp:23`).
    pub fn zip_best_compression() -> Self {
        CompressionType::Zip(9)
    }

    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(CompressionType::None),
            1..=9 => Some(CompressionType::Zip(tag)),
            _ => None,
        }
    }
}

/// The failure of reading recorded tick data
/// (`ReplayDataFileReader::Impl::readTickData` and `readNextChunk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayTickError {
    /// The stream was truncated or malformed.
    Stream(StreamError),
    /// The header or meta-data of the file failed to verify.
    Header(ReplayError),
    /// A chunk reported a zero length (`replay_data_file_reader.cpp:745`).
    ChunkLengthZero,
    /// A chunk's signature does not verify against the chain
    /// (`replay_data_file_reader.cpp:799`).
    BadSignature,
    /// A tick's data length overran the chunk (`:867`).
    CorruptTickHeader,
    /// A tick's game time is not one past the previous tick (`:877`).
    GameTimeMismatch {
        /// The previous tick read.
        expected: u32,
        /// The tick the chunk claimed.
        got: u32,
    },
    /// More ticks than the header reported (`:887`).
    ExtraneousTickData,
    /// A tick payload carried an unknown compression tag (`:36`).
    BadCompressionTag(u8),
    /// A tick payload's zlib stream did not decompress.
    BadZlib,
}

impl std::fmt::Display for ReplayTickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stream(err) => write!(f, "replay tick stream error: {err}"),
            Self::Header(err) => write!(f, "replay tick header error: {err}"),
            Self::ChunkLengthZero => write!(f, "got zero chunk length"),
            Self::BadSignature => write!(f, "chunk signature does not verify"),
            Self::CorruptTickHeader => write!(f, "could not read game time data header"),
            Self::GameTimeMismatch { expected, got } => {
                write!(f, "game time mismatch: expected {expected}, got {got}")
            }
            Self::ExtraneousTickData => write!(f, "extraneous tick data"),
            Self::BadCompressionTag(tag) => {
                write!(f, "invalid compression type: {tag}")
            }
            Self::BadZlib => write!(f, "tick payload zlib stream failed"),
        }
    }
}

impl std::error::Error for ReplayTickError {}

impl From<StreamError> for ReplayTickError {
    fn from(err: StreamError) -> Self {
        ReplayTickError::Stream(err)
    }
}

/// Frame a payload the way `ZipOStream` does (`zip_stream.cpp:281`): the
/// packed original length, the packed compressed length and the zlib stream.
fn zip_frame(data: &[u8], level: u8) -> Vec<u8> {
    let mut encoder =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(u32::from(level)));
    encoder.write_all(data).expect("in-memory zlib write");
    let compressed = encoder.finish().expect("in-memory zlib flush");

    let mut out = Vec::new();
    append_packed_int(&mut out, data.len() as u32);
    append_packed_int(&mut out, compressed.len() as u32);
    out.extend_from_slice(&compressed);
    out
}

/// Unframe and decompress a `ZipOStream` payload (`zip_stream.cpp:135`).
fn zip_unframe(payload: &[u8]) -> Result<Vec<u8>, ReplayTickError> {
    let mut stream = BinaryStream::from_slice(payload);
    let _orig_len = stream.read_packed_int()?;
    let compress_len = stream.read_packed_int()? as usize;
    let deflate = stream.read_blob_raw(compress_len)?;

    let mut decoder = flate2::read::ZlibDecoder::new(&deflate[..]);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|_| ReplayTickError::BadZlib)?;
    Ok(out)
}

/// Append `value` as a packed int: one byte below 255, `0xFF` plus three low
/// bytes otherwise (`writePackedInt`, `binary_stream.hpp:254`).
fn append_packed_int(out: &mut Vec<u8>, value: u32) {
    if value < 255 {
        out.push(value as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&[
            (value & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
        ]);
    }
}

/// One tick's accumulated cell data (`TickReplayData`,
/// `replay_data_file_writer.hpp:27`).
#[derive(Debug, Clone)]
pub struct TickReplayData {
    /// The game tick this data belongs to.
    tick: u32,
    /// `numExpectedCells_`: how many cells report for the tick.
    num_expected_cells: u32,
    /// `numCellsLeft_`: how many are still outstanding.
    num_cells_left: u32,
    /// The cell data received so far, uncompressed.
    blob: Vec<u8>,
}

impl TickReplayData {
    /// `TickReplayData::init` (`replay_data_file_writer.cpp:772`).
    pub fn new(tick: u32, num_cells: u32) -> Self {
        TickReplayData {
            tick,
            num_expected_cells: num_cells,
            num_cells_left: num_cells,
            blob: Vec::new(),
        }
    }

    /// The tick this data belongs to.
    pub fn tick(&self) -> u32 {
        self.tick
    }

    /// `numExpectedCells` (`replay_data_file_writer.hpp:39`).
    pub fn num_expected_cells(&self) -> u32 {
        self.num_expected_cells
    }

    /// `numCellsLeft` (`replay_data_file_writer.hpp:42`).
    pub fn num_cells_left(&self) -> u32 {
        self.num_cells_left
    }

    /// `TickReplayData::numExpectedCells` setter (`replay_data_file_writer.cpp:854`):
    /// raise the expected count when a new cell joins the space, extending the
    /// outstanding count by the same delta.
    pub fn set_num_expected_cells(&mut self, new_value: u32) {
        if new_value > self.num_expected_cells {
            let delta = new_value - self.num_expected_cells;
            self.num_cells_left += delta;
            self.num_expected_cells = new_value;
        }
    }

    /// `TickReplayData::transferFromCell` (`replay_data_file_writer.cpp:801`):
    /// append a cell's data for this tick. Returns `false` if the tick was
    /// already complete, in which case the data is dropped.
    pub fn transfer_from_cell(&mut self, data: &[u8]) -> bool {
        if self.num_cells_left == 0 {
            return false;
        }
        self.blob.extend_from_slice(data);
        self.num_cells_left -= 1;
        true
    }

    /// `TickReplayData::finalise` (`replay_data_file_writer.cpp:821`): the
    /// tick block — the header of game time and data length followed by the
    /// compressed cell data. The data length covers the compression tag and
    /// the compressed payload, not the header (`:840`).
    pub fn finalise(&self, compression: CompressionType) -> Vec<u8> {
        let payload = compress_payload(compression, &self.blob);
        let mut out = Vec::with_capacity(TICK_HEADER_SIZE + payload.len());
        out.extend_from_slice(&self.tick.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    /// `TickReplayData::size` (`replay_data_file_writer.hpp:44`): the number of
    /// bytes the finalised tick block occupies.
    pub fn size(&self, compression: CompressionType) -> usize {
        TICK_HEADER_SIZE + compress_payload(compression, &self.blob).len()
    }
}

/// The compression payload a `CompressionOStream` writes
/// (`compression_stream.cpp:95`): the tag byte and, for zip levels, the
/// `ZipOStream` frame (`zip_stream.cpp:281`).
fn compress_payload(compression: CompressionType, data: &[u8]) -> Vec<u8> {
    let mut out = vec![compression.tag()];
    if let CompressionType::Zip(level) = compression {
        out.extend_from_slice(&zip_frame(data, level));
    } else {
        out.extend_from_slice(data);
    }
    out
}

/// Decompress a tick block payload (`CompressionIStream`,
/// `compression_stream.cpp:16`): the tag byte picks raw or zlib.
fn decompress_payload(payload: &[u8]) -> Result<Vec<u8>, ReplayTickError> {
    let Some(&tag) = payload.first() else {
        return Err(ReplayTickError::CorruptTickHeader);
    };
    let compression =
        CompressionType::from_tag(tag).ok_or(ReplayTickError::BadCompressionTag(tag))?;
    match compression {
        CompressionType::None => Ok(payload[1..].to_vec()),
        CompressionType::Zip(_) => zip_unframe(&payload[1..]),
    }
}

/// A tick read back out of the recording: its game time and the
/// decompressed cell data, with whether the payload had been compressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayTick {
    /// The game tick.
    pub tick: u32,
    /// The decompressed cell data.
    pub data: Vec<u8>,
    /// Whether the payload was compressed (`isCompressed` handed to the
    /// reader listener, `replay_data_file_reader.cpp:927`).
    pub is_compressed: bool,
}

/// The BaseApp's replay tick writer (`ReplayDataFileWriter`'s tick data path,
/// `replay_data_file_writer.cpp:179`): buffers per-tick cell data until each
/// tick is complete, then signs and writes chunks of `numTicksToSign` ticks.
#[derive(Debug)]
pub struct ReplayTickWriter {
    /// The scheme the chunk signatures are produced with.
    scheme: ReplayChecksumScheme,
    /// The compression level tick payloads use (`compressionType_`).
    compression: CompressionType,
    /// `numTicksToSign_`: how many completed ticks make up a chunk.
    num_ticks_to_sign: u32,
    /// `bufferedTickData_`, by tick.
    buffered: BTreeMap<u32, TickReplayData>,
    /// `currentTick_`, the next tick expected.
    current_tick: u32,
    /// `lastTickPendingWrite_`, the last tick in a queued chunk.
    last_tick_pending_write: u32,
    /// `lastTickWritten_`, the last tick whose chunk write completed.
    last_tick_written: u32,
    /// `numTicksWritten_`, ticks whose writes have completed.
    num_ticks_written: u32,
    /// `lastSignature_`, the signature the next chunk chains off.
    last_signature: Vec<u8>,
    /// `isFinalising_`.
    is_finalising: bool,
    /// The queued chunk bytes, standing in for the background file writer.
    chunks: Vec<Vec<u8>>,
    /// The `queueWrite` user data of each queued chunk: the last tick the
    /// chunk carries (`replay_data_file_writer.cpp:411`).
    chunk_user_data: Vec<u32>,
    /// Cell data bytes dropped for ticks already written
    /// (`replay_data_file_writer.cpp:200`).
    dropped_bytes: u64,
}

impl ReplayTickWriter {
    /// A writer for new recordings. `primed_with` is the signature the first
    /// chunk chains off — the meta-data block's (`replay_data_file_writer.cpp:83`).
    pub fn new(
        scheme: ReplayChecksumScheme,
        compression: CompressionType,
        num_ticks_to_sign: u32,
        primed_with: Vec<u8>,
    ) -> Self {
        ReplayTickWriter {
            scheme,
            compression,
            num_ticks_to_sign,
            buffered: BTreeMap::new(),
            current_tick: 0,
            last_tick_pending_write: 0,
            last_tick_written: 0,
            num_ticks_written: 0,
            last_signature: primed_with,
            is_finalising: false,
            chunks: Vec::new(),
            chunk_user_data: Vec::new(),
            dropped_bytes: 0,
        }
    }

    /// The queued chunk bytes.
    pub fn chunks(&self) -> &[Vec<u8>] {
        &self.chunks
    }

    /// Take the queued chunk bytes with their `queueWrite` user data, in the
    /// order they were produced. The file writer hands these to the background
    /// file writer as they are signed (`replay_data_file_writer.cpp:411`).
    pub fn take_chunks(&mut self) -> Vec<(Vec<u8>, u32)> {
        let chunks = std::mem::take(&mut self.chunks);
        let user_data = std::mem::take(&mut self.chunk_user_data);
        chunks.into_iter().zip(user_data).collect()
    }

    /// `currentTick_`.
    pub fn current_tick(&self) -> u32 {
        self.current_tick
    }

    /// `numTicksToSign_`.
    pub fn num_ticks_to_sign(&self) -> u32 {
        self.num_ticks_to_sign
    }

    /// `compressionType_`.
    pub fn compression(&self) -> CompressionType {
        self.compression
    }

    /// `lastSignature_`: the signature the next chunk chains off.
    pub fn last_signature(&self) -> &[u8] {
        &self.last_signature
    }

    /// `onBackgroundFileWritingComplete`'s drain test
    /// (`replay_data_file_writer.cpp:492`): the earliest buffered tick is
    /// still inside the tick range being written out.
    pub fn has_more_tick_data_to_write(&self) -> bool {
        self.buffered
            .first_key_value()
            .is_some_and(|(first, _)| *first < self.current_tick)
    }

    /// `numTicksWritten_` (`replay_data_file_writer.hpp`).
    pub fn num_ticks_written(&self) -> u32 {
        self.num_ticks_written
    }

    /// `lastTickWritten_`.
    pub fn last_tick_written(&self) -> u32 {
        self.last_tick_written
    }

    /// `lastTickPendingWrite_`.
    pub fn last_tick_pending_write(&self) -> u32 {
        self.last_tick_pending_write
    }

    /// `isFinalising_`.
    pub fn is_finalising(&self) -> bool {
        self.is_finalising
    }

    /// The cell data bytes dropped because their tick was already written.
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }

    /// `ReplayDataFileWriter::addTickData` (`replay_data_file_writer.cpp:179`):
    /// buffer one cell's data for `tick`. `num_cells` is the reporting cell's
    /// view of how many cells share the space.
    pub fn add_tick_data(&mut self, tick: u32, num_cells: u32, blob: &[u8]) {
        if self.is_finalising {
            return;
        }

        if tick < self.current_tick {
            // A new cell reporting for a tick already written out
            // (`replay_data_file_writer.cpp:191`).
            self.dropped_bytes += blob.len() as u64;
            return;
        }

        if self.current_tick == 0 && self.buffered.is_empty() {
            if blob.is_empty() {
                // The first tick carrying no data is ignored (`:218`).
                return;
            }
            self.current_tick = tick;
            // Pretend we have written previous ticks (`:230`).
            self.last_tick_written = tick - 1;
            self.last_tick_pending_write = tick - 1;
        }

        let entry = self
            .buffered
            .entry(tick)
            .or_insert_with(|| TickReplayData::new(tick, num_cells));
        entry.set_num_expected_cells(num_cells);
        if !entry.transfer_from_cell(blob) {
            return;
        }

        if entry.num_cells_left() == 0 {
            self.finalise_completed_tick_data(tick);
        }
    }

    /// `ReplayDataFileWriter::finaliseCompletedTickData`
    /// (`replay_data_file_writer.cpp:275`): every tick up to `tick` is now
    /// writable — complete or not — so advance past them and flush chunks.
    fn finalise_completed_tick_data(&mut self, tick: u32) {
        while self.current_tick <= tick {
            self.current_tick += 1;
        }
        self.sign_and_write_completed_tick_data(false);
    }

    /// `ReplayDataFileWriter::signAndWriteCompletedTickData`
    /// (`replay_data_file_writer.cpp:314`): queue chunks of completed ticks
    /// until fewer than `numTicksToSign_` remain, or all of them when this is
    /// the last chunk.
    pub fn sign_and_write_completed_tick_data(&mut self, is_last_chunk: bool) {
        if self.current_tick == 0 {
            // No data has been added yet (`:317`).
            return;
        }

        let mut next_tick_to_write = self.last_tick_pending_write + 1;
        while next_tick_to_write < self.current_tick
            && (is_last_chunk || self.current_tick - next_tick_to_write >= self.num_ticks_to_sign)
        {
            let num_ticks_to_sign = self
                .num_ticks_to_sign
                .min(self.current_tick - next_tick_to_write);
            let blocks = chunk_tick_blocks(
                &self.buffered,
                self.compression,
                next_tick_to_write,
                num_ticks_to_sign,
            );

            let chunk: Vec<u8> = blocks
                .iter()
                .flat_map(|(_, block)| block.iter().copied())
                .collect();

            let mut out = Vec::with_capacity(4 + chunk.len() + self.scheme.stream_size());
            out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(&chunk);

            let mut blob = self.last_signature.clone();
            blob.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            blob.extend_from_slice(&chunk);
            self.last_signature = self.scheme.sign(&blob);

            out.extend_from_slice(&self.last_signature);
            self.chunks.push(out);

            self.last_tick_pending_write = next_tick_to_write + num_ticks_to_sign - 1;
            // The chunk's user data is the last tick it carries (`:411`).
            self.chunk_user_data.push(self.last_tick_pending_write);
            next_tick_to_write = self.last_tick_pending_write + 1;
        }
    }

    /// `ReplayDataFileWriter::onBackgroundFileWritingComplete`
    /// (`replay_data_file_writer.cpp:460`): a queued chunk reached the file, so
    /// the ticks it covered are written and their buffers released.
    pub fn on_chunk_write_complete(&mut self, new_last_tick_written: u32) {
        let num_new_ticks_written = new_last_tick_written - self.last_tick_written;
        self.num_ticks_written += num_new_ticks_written;
        self.last_tick_written = new_last_tick_written;

        let first = self.last_tick_written - num_new_ticks_written + 1;
        let keys: Vec<u32> = self
            .buffered
            .range(first..=new_last_tick_written)
            .map(|(tick, _)| *tick)
            .collect();
        for tick in keys {
            self.buffered.remove(&tick);
        }
    }

    /// `ReplayDataFileWriter::finalise` (`replay_data_file_writer.cpp:534`):
    /// start finalising — the last chunk is signed even if it is short. The
    /// header's tick-count rewrite and the read-only chmod are the close
    /// path's (BW-07-05-04).
    pub fn finalise(&mut self) {
        self.is_finalising = true;
        self.sign_and_write_completed_tick_data(true);
    }
}

/// `TickReplayData::streamTickHeader` (`replay_data_file_writer.cpp:890`):
/// the header of a tick that carries no data — its game time and a zero data
/// length. Such a tick has no compression tag, because nothing was written
/// into the cell data stream for it.
pub fn stream_tick_header(tick: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(TICK_HEADER_SIZE);
    out.extend_from_slice(&tick.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// The tick blocks a chunk covers: `num_ticks` blocks from `first_tick`, with
/// header-only placeholders for ticks that never received cell data — the cell
/// apps in the space all died and a new cell takes over mid-replay
/// (`replay_data_file_writer.cpp:367`).
fn chunk_tick_blocks(
    buffered: &BTreeMap<u32, TickReplayData>,
    compression: CompressionType,
    first_tick: u32,
    num_ticks: u32,
) -> Vec<(u32, Vec<u8>)> {
    let mut blocks = Vec::new();
    let end = first_tick.saturating_add(num_ticks);
    let mut cursor = first_tick;
    while cursor < end {
        match buffered.get(&cursor) {
            Some(entry) => blocks.push((cursor, entry.finalise(compression))),
            None => blocks.push((cursor, stream_tick_header(cursor))),
        }
        cursor += 1;
    }
    blocks
}

/// Read every chunk out of a recorded tick stream
/// (`ReplayDataFileReader::Impl::readNextChunk` + `readTickData`,
/// `replay_data_file_reader.cpp:764`). `primed_with` is the signature the
/// first chunk chains off — the meta-data block's. Each tick's payload is
/// decompressed, so the recorded events come back losslessly.
pub fn read_replay_ticks(
    bytes: &[u8],
    scheme: &ReplayChecksumScheme,
    should_verify: bool,
    primed_with: &[u8],
    expected_num_ticks: Option<u32>,
) -> Result<Vec<ReplayTick>, ReplayTickError> {
    let signature_length = scheme.stream_size();
    let mut last_signature = primed_with.to_vec();
    let mut ticks = Vec::new();
    let mut last_game_time = 0u32;
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        // `readNextChunkLength` (`replay_data_file_reader.cpp:722`).
        if bytes.len() - cursor < 4 {
            return Err(ReplayTickError::Stream(StreamError::Truncated));
        }
        let chunk_length = u32::from_le_bytes([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
        ]);
        if chunk_length == 0 {
            return Err(ReplayTickError::ChunkLengthZero);
        }
        let chunk_length = chunk_length as usize;
        let chunk_end = cursor + 4 + chunk_length;
        if bytes.len() < chunk_end + signature_length {
            return Err(ReplayTickError::Stream(StreamError::Truncated));
        }
        let payload = &bytes[cursor + 4..chunk_end];
        let signature = &bytes[chunk_end..chunk_end + signature_length];

        if should_verify {
            let mut blob = last_signature.clone();
            blob.extend_from_slice(&(chunk_length as u32).to_le_bytes());
            blob.extend_from_slice(payload);
            if !scheme.verify(&blob, signature) {
                return Err(ReplayTickError::BadSignature);
            }
        }
        last_signature = signature.to_vec();
        cursor = chunk_end + signature_length;

        // `readTickData` (`replay_data_file_reader.cpp:845`).
        let mut payload_stream = BinaryStream::from_slice(payload);
        while !payload_stream.at_end() {
            let game_time = payload_stream.read_u32()?;
            let data_length = payload_stream.read_u32()? as usize;
            if data_length > payload_stream.remaining() {
                return Err(ReplayTickError::CorruptTickHeader);
            }
            if last_game_time != 0 && game_time != last_game_time + 1 {
                return Err(ReplayTickError::GameTimeMismatch {
                    expected: last_game_time + 1,
                    got: game_time,
                });
            }
            if let Some(num_ticks) = expected_num_ticks
                && ticks.len() == num_ticks as usize
            {
                return Err(ReplayTickError::ExtraneousTickData);
            }
            last_game_time = game_time;

            let tick_payload = payload_stream.read_blob_raw(data_length)?;
            // A placeholder tick carries no payload at all, so there is no
            // compression tag to read (`streamTickHeader` writes none).
            let (data, is_compressed) = if tick_payload.is_empty() {
                (Vec::new(), false)
            } else {
                let is_compressed = tick_payload[0] != 0;
                (decompress_payload(&tick_payload)?, is_compressed)
            };
            ticks.push(ReplayTick {
                tick: game_time,
                data,
                is_compressed,
            });
        }
    }

    Ok(ticks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay_header::ReplayMetaData;

    fn scheme() -> ReplayChecksumScheme {
        ReplayChecksumScheme::new(8)
    }

    /// A recorded event: a block type byte then its payload
    /// (`replay_data_collector.cpp:83`).
    fn event(block: u8, entity_id: u32) -> Vec<u8> {
        let mut out = vec![block];
        out.extend_from_slice(&entity_id.to_le_bytes());
        out
    }

    /// The events of one tick, as a cell would report them.
    fn cell_events(events: &[(u8, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (block, entity_id) in events {
            out.extend_from_slice(&event(*block, *entity_id));
        }
        out
    }

    /// The meta-data signature the first chunk chains off.
    fn prime() -> Vec<u8> {
        let mut meta_data = ReplayMetaData::new();
        meta_data.add("server", "baseapp");
        let scheme = scheme();
        meta_data.write_to(&scheme, &[0u8; 8], &mut BinaryStream::new())
    }

    /// Hand-build a chunk of tick blocks with a valid signature off `seed`,
    /// mirroring `signAndWriteCompletedTickData`
    /// (`replay_data_file_writer.cpp:388`).
    fn signed_chunk(blocks: &[(u32, Vec<u8>)], seed: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        for (tick, data) in blocks {
            payload.extend_from_slice(&tick.to_le_bytes());
            payload.extend_from_slice(&(data.len() as u32).to_le_bytes());
            payload.extend_from_slice(data);
        }
        let scheme = scheme();
        let mut blob = seed.to_vec();
        blob.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        blob.extend_from_slice(&payload);
        let signature = scheme.sign(&blob);

        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        chunk.extend_from_slice(&payload);
        chunk.extend_from_slice(&signature);
        chunk
    }

    /// A complete tick is written out as one block and reads back losslessly.
    #[test]
    fn a_completed_tick_reads_back_losslessly() {
        let compression = CompressionType::zip_best_speed();
        let mut writer = ReplayTickWriter::new(scheme(), compression, 1, prime());
        let events = cell_events(&[
            (block_type::ENTITY_CREATE, 7),
            (block_type::ENTITY_METHOD, 7),
            (block_type::ENTITY_PROPERTY_CHANGE, 9),
        ]);
        writer.add_tick_data(100, 1, &events);
        writer.sign_and_write_completed_tick_data(false);
        assert_eq!(writer.chunks().len(), 1);

        let mut bytes = Vec::new();
        for chunk in writer.chunks() {
            bytes.extend_from_slice(chunk);
        }
        let ticks = read_replay_ticks(&bytes, &scheme(), true, &prime(), None).expect("ticks read");
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].tick, 100);
        assert_eq!(ticks[0].data, events, "the event sequence is lossless");
        assert!(ticks[0].is_compressed);
    }

    /// An uncompressed tick also round-trips, and reports as not compressed.
    #[test]
    fn an_uncompressed_tick_reads_back_losslessly() {
        let mut writer = ReplayTickWriter::new(scheme(), CompressionType::None, 1, prime());
        let events = cell_events(&[(block_type::ENTITY_CREATE, 3)]);
        writer.add_tick_data(5, 1, &events);
        writer.sign_and_write_completed_tick_data(false);

        let mut bytes = Vec::new();
        for chunk in writer.chunks() {
            bytes.extend_from_slice(chunk);
        }
        let ticks = read_replay_ticks(&bytes, &scheme(), true, &prime(), None).expect("ticks read");
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].data, events);
        assert!(!ticks[0].is_compressed);
    }

    /// The tick block follows the reference layout: game time, data length,
    /// compression tag, then the zlib frame of packed lengths and stream.
    #[test]
    fn a_tick_block_follows_the_reference_layout() {
        let entry = TickReplayData::new(7, 1);
        let mut data = entry.clone();
        data.transfer_from_cell(b"hello replay");
        let block = data.finalise(CompressionType::Zip(1));

        assert_eq!(&block[0..4], &7u32.to_le_bytes(), "game time");
        let data_length = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
        assert_eq!(data_length as usize, block.len() - TICK_HEADER_SIZE);
        assert_eq!(block[8], 1, "compression tag is the zip level");

        let payload = &block[TICK_HEADER_SIZE + 1..];
        let mut stream = BinaryStream::from_slice(payload);
        let orig_len = stream.read_packed_int().expect("original length");
        let compress_len = stream.read_packed_int().expect("compressed length");
        assert_eq!(orig_len, 12);
        assert_eq!(
            compress_len as usize,
            payload.len() - 2,
            "one byte each packed length"
        );
        assert_eq!(TICK_HEADER_SIZE, 8);
    }

    /// A tick only completes once every expected cell has reported; the
    /// accumulated data is the cells' concatenation.
    #[test]
    fn a_tick_completes_once_every_cell_reports() {
        let compression = CompressionType::zip_best_speed();
        let mut writer = ReplayTickWriter::new(scheme(), compression, 1, prime());

        let first = cell_events(&[(block_type::ENTITY_CREATE, 1)]);
        let second = cell_events(&[(block_type::ENTITY_METHOD, 2)]);
        writer.add_tick_data(10, 2, &first);
        assert!(
            writer.chunks().is_empty(),
            "one cell of two is not complete"
        );

        writer.add_tick_data(10, 2, &second);
        assert_eq!(writer.chunks().len(), 1, "complete once both report");

        let mut bytes = Vec::new();
        for chunk in writer.chunks() {
            bytes.extend_from_slice(chunk);
        }
        let ticks = read_replay_ticks(&bytes, &scheme(), true, &prime(), None).expect("ticks read");
        let mut data = first.clone();
        data.extend_from_slice(&second);
        assert_eq!(ticks[0].data, data);
    }

    /// A new cell raising the expected count extends the outstanding count
    /// (`replay_data_file_writer.cpp:854`).
    #[test]
    fn a_new_cell_raises_the_expected_count() {
        let mut entry = TickReplayData::new(1, 1);
        entry.transfer_from_cell(b"one");
        assert_eq!(entry.num_cells_left(), 0);

        entry.set_num_expected_cells(2);
        assert_eq!(entry.num_expected_cells(), 2);
        assert_eq!(entry.num_cells_left(), 1);
        assert!(entry.transfer_from_cell(b"two"));
        assert_eq!(entry.num_cells_left(), 0);
    }

    /// Ticks with no cell data are header-only placeholders, and the game
    /// times still read back in sequence (`replay_data_file_writer.cpp:400`).
    #[test]
    fn a_missing_tick_reads_back_as_an_empty_placeholder() {
        let compression = CompressionType::None;
        let mut writer = ReplayTickWriter::new(scheme(), compression, 3, prime());
        // Tick 12 carries data; tick 13 is never reported at all; tick 14 has a
        // cell reporting nothing, so it keeps its compression tag.
        writer.add_tick_data(12, 1, b"events for 12");
        writer.add_tick_data(14, 1, b"");
        assert_eq!(writer.chunks().len(), 1);

        let bytes = &writer.chunks()[0];
        let chunk_length = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        // 22 (header, tag, 13 bytes) + 8 (missing) + 9 (tag of an empty cell).
        assert_eq!(chunk_length as usize, 22 + 8 + 9, "headers plus payloads");
        // The placeholder is a bare `streamTickHeader`: game time, then a zero
        // data length, and no compression tag.
        assert_eq!(&bytes[26..30], &13u32.to_le_bytes(), "missing tick time");
        assert_eq!(&bytes[30..34], &0u32.to_le_bytes(), "missing tick length");
        assert_eq!(
            bytes.len(),
            4 + 22 + 8 + 9 + 8,
            "signature closes the chunk"
        );

        let ticks = read_replay_ticks(bytes, &scheme(), true, &prime(), None).expect("ticks read");
        assert_eq!(ticks.len(), 3);
        assert_eq!(ticks[0].tick, 12);
        assert_eq!(ticks[0].data, b"events for 12");
        assert!(!ticks[0].is_compressed);
        assert_eq!(ticks[1].tick, 13);
        assert_eq!(ticks[1].data, b"", "the missing tick is empty");
        assert!(!ticks[1].is_compressed, "a placeholder has no tag");
        assert_eq!(ticks[2].tick, 14);
        assert_eq!(ticks[2].data, b"", "the cell reported nothing");
    }

    /// Chunks are cut every `numTicksToSign` ticks, in order.
    #[test]
    fn chunks_are_cut_every_num_ticks_to_sign() {
        let compression = CompressionType::None;
        let mut writer = ReplayTickWriter::new(scheme(), compression, 2, prime());
        for tick in 1..=5u32 {
            writer.add_tick_data(tick, 1, &[tick as u8]);
        }
        // 1..=4 are complete chunks; 5 waits for the last chunk.
        assert_eq!(writer.chunks().len(), 2);
        writer.finalise();
        assert_eq!(writer.chunks().len(), 3, "the last chunk carries tick 5");

        let mut bytes = Vec::new();
        for chunk in writer.chunks() {
            bytes.extend_from_slice(chunk);
        }
        let ticks = read_replay_ticks(&bytes, &scheme(), true, &prime(), None).expect("ticks read");
        assert_eq!(
            ticks.iter().map(|tick| tick.tick).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(
            ticks.iter().map(|tick| tick.data[0]).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
    }

    /// Chunk signatures chain: corrupting one chunk's bytes breaks only its
    /// own verification, and the chain seed is the meta-data signature.
    #[test]
    fn chunk_signatures_chain_off_the_previous_signature() {
        let compression = CompressionType::None;
        let mut writer = ReplayTickWriter::new(scheme(), compression, 1, prime());
        for tick in 1..=3u32 {
            writer.add_tick_data(tick, 1, &[tick as u8]);
        }
        assert_eq!(writer.chunks().len(), 3);

        let mut bytes = Vec::new();
        for chunk in writer.chunks() {
            bytes.extend_from_slice(chunk);
        }
        // Corrupt the one data byte of the second chunk's payload: chunk length
        // field, game time, data length, compression tag, then the byte.
        let scheme = scheme();
        let signature_length = scheme.stream_size();
        let first_chunk_end = writer.chunks()[0].len();
        let second_chunk_payload = first_chunk_end + 4 + 8 + 1;
        bytes[second_chunk_payload] ^= 0xFF;

        let read = &bytes[..];
        let err = read_replay_ticks(read, &scheme, true, &prime(), None)
            .expect_err("the corrupted chunk fails");
        assert_eq!(err, ReplayTickError::BadSignature);

        // Without verification the corruption passes through, so the payload
        // is the corrupted byte.
        let ticks = read_replay_ticks(read, &scheme, false, &prime(), None)
            .expect("an unverified read still parses");
        assert_eq!(ticks.len(), 3);
        assert_eq!(ticks[1].data[0], 2u8 ^ 0xFF);
        assert_eq!(
            ticks[2].data[0], 3,
            "the chain seed is still the meta-data one"
        );
        let _ = signature_length;
    }

    /// Cell data for a tick already written out is dropped
    /// (`replay_data_file_writer.cpp:191`), and so is data arriving after
    /// finalising.
    #[test]
    fn late_cell_data_is_dropped() {
        let compression = CompressionType::None;
        let mut writer = ReplayTickWriter::new(scheme(), compression, 1, prime());
        writer.add_tick_data(5, 1, b"on time");

        writer.add_tick_data(4, 1, b"too late");
        assert_eq!(writer.dropped_bytes(), 8, "the late blob is dropped");

        writer.finalise();
        writer.add_tick_data(6, 1, b"after finalise");
        assert_eq!(writer.chunks().len(), 1, "nothing new is queued");
    }

    /// A zero chunk length is rejected (`replay_data_file_reader.cpp:745`).
    #[test]
    fn a_zero_chunk_length_is_rejected() {
        let scheme = scheme();
        let bytes = 0u32.to_le_bytes();
        let err = read_replay_ticks(&bytes, &scheme, true, &prime(), None)
            .expect_err("the zero length is rejected");
        assert_eq!(err, ReplayTickError::ChunkLengthZero);
    }

    /// A game time that is not one past the previous tick is rejected
    /// (`replay_data_file_reader.cpp:877`).
    #[test]
    fn a_game_time_gap_is_rejected() {
        // A chunk carrying ticks 5 then 9, with a valid signature.
        let chunk = signed_chunk(&[(5, vec![0, b'x']), (9, vec![0, b'y'])], &prime());

        let err = read_replay_ticks(&chunk, &scheme(), true, &prime(), None)
            .expect_err("the skipped tick is rejected");
        assert_eq!(
            err,
            ReplayTickError::GameTimeMismatch {
                expected: 6,
                got: 9
            }
        );
    }

    /// An over-long tick data length is rejected
    /// (`replay_data_file_reader.cpp:867`).
    #[test]
    fn an_over_long_tick_length_is_rejected() {
        // A validly signed chunk whose second tick claims four bytes of data
        // but carries one.
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.push(0);
        payload.push(b'x');
        payload.extend_from_slice(&6u32.to_le_bytes());
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.push(0);
        payload.push(b'y');

        let scheme = scheme();
        let seed = prime();
        let mut blob = seed.clone();
        blob.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        blob.extend_from_slice(&payload);
        let signature = scheme.sign(&blob);

        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        chunk.extend_from_slice(&payload);
        chunk.extend_from_slice(&signature);

        let err = read_replay_ticks(&chunk, &scheme, true, &seed, None)
            .expect_err("the over-long length is rejected");
        assert_eq!(err, ReplayTickError::CorruptTickHeader);
    }

    /// Reading more ticks than the header reports is rejected
    /// (`replay_data_file_reader.cpp:887`).
    #[test]
    fn extraneous_ticks_are_rejected() {
        let compression = CompressionType::None;
        let mut writer = ReplayTickWriter::new(scheme(), compression, 1, prime());
        writer.add_tick_data(1, 1, b"one");
        writer.add_tick_data(2, 1, b"two");
        writer.sign_and_write_completed_tick_data(false);

        let mut bytes = Vec::new();
        for chunk in writer.chunks() {
            bytes.extend_from_slice(chunk);
        }
        let err = read_replay_ticks(&bytes, &scheme(), true, &prime(), Some(1))
            .expect_err("the second tick is extraneous");
        assert_eq!(err, ReplayTickError::ExtraneousTickData);
    }

    /// A chunk write completing releases the ticks it covered
    /// (`replay_data_file_writer.cpp:460`).
    #[test]
    fn a_chunk_write_completion_releases_its_ticks() {
        let compression = CompressionType::None;
        let mut writer = ReplayTickWriter::new(scheme(), compression, 2, prime());
        for tick in 1..=4u32 {
            writer.add_tick_data(tick, 1, &[tick as u8]);
        }
        assert_eq!(writer.chunks().len(), 2);
        assert_eq!(writer.num_ticks_written(), 0);

        writer.on_chunk_write_complete(2);
        assert_eq!(writer.num_ticks_written(), 2);
        assert_eq!(writer.last_tick_written(), 2);

        writer.on_chunk_write_complete(4);
        assert_eq!(writer.num_ticks_written(), 4);
        assert!(!writer.is_finalising());
    }
}
