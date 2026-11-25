//! BW-07-05-02/03/04: write the replay data file — the header and metadata
//! blocks, the compressed signed tick chunks, and the finalisation and
//! failure paths.
//!
//! The reference writes a recording through `ReplayDataFileWriter`
//! (`server/baseapp/replay_data_file_writer.{hpp,cpp}`): the constructor
//! streams the header block (`lib/connection/replay_header.cpp`) and the
//! signed metadata block (`lib/connection/replay_metadata.cpp`) as the
//! file's first write, `addTickData` accumulates per-cell tick data and
//! writes signed chunks of `numTicksToSign` ticks each
//! (`signAndWriteCompletedTickData`), and `finalise` writes the last chunk,
//! patches the header's `numTicks` field in place, marks the file
//! read-only, and closes (`finalise`/`closeWriter`). Every signed block is
//! sealed by the checksum scheme, and the scheme itself is chained: the
//! previous signature is fed in (`readBlob`) before each block is summed,
//! so a signature covers `previous signature || block bytes`.
//!
//! Schemes: the port's reader (`client::replay`) already registers the
//! keyed cryptographic half of `ReplayChecksumScheme::create` (SHA-256 +
//! ECDSA, `replay_checksum_scheme.cpp`) as a supplied-scheme seam. The
//! writer keeps the same seam and provides the two unkeyed faces the
//! reference itself ships for tests and lighter uses: [`Md5Checksum`]
//! (`MD5SumScheme`, 16-byte signature — the scheme the original replay
//! unit tests use) and [`Sha256Checksum`] (`SHAChecksumScheme`, the
//! 32-byte SHA-256 half of the recording scheme).
//!
//! Registered deviations:
//! * the reference queues writes to a background thread
//!   (`BackgroundFileWriter`); the port writes synchronously, so each
//!   write's completion bookkeeping and listener notification run inline,
//!   in the reference's order: once per completed write, tick accounting
//!   for chunk writes only (`userArg != 0`), and a finalising writer that
//!   has no earlier tick data left closes itself on the completing call.
//! * the static active-writer registry (`s_activeWriters_`,
//!   `existsForPath`/`closeAll`) is consumed by the baseapp shutdown
//!   wiring, which does not exist yet; it arrives with that wiring.
//! * the listener receives snapshot data rather than a writer reference
//!   (a reference back would alias the writer mid-callback in Rust).
//! * an invalid compression type falls back to `BW_COMPRESSION_NONE` the
//!   way the reference's `CRITICAL_MSG` path does
//!   (`compression_stream.cpp:60-97`).

use md5::Digest as _;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `ReplayHeader::NONCE_SIZE` (`replay_header.hpp:30`).
pub const NONCE_SIZE: usize = 8;

/// The current protocol version streamed subpatch, patch, minor, major —
/// `stream << subpatch_ << patch_ << minor_ << major_`
/// (`client_server_protocol_version.hpp:138`), i.e. 2.9.0.0 as
/// `00 00 09 02`.
pub const PROTOCOL_VERSION_BYTES: [u8; 4] = [0, 0, 9, 2];

/// Field sizes of the header block before the signature
/// (`ReplayHeader::streamSize`, `replay_header.cpp:210-220`): version,
/// digest, game update frequency, timestamp, nonce, signature length.
const HEADER_PREFIX_SIZE: usize = 4 + 16 + 1 + 4 + NONCE_SIZE + 2;

/// The total header block size: the prefix, the signature, and the
/// trailing `numTicks` (`GameTime`).
fn header_stream_size(signature_length: u16) -> usize {
    HEADER_PREFIX_SIZE + signature_length as usize + 4
}

/// The offset of the `numTicks` field from the start of the file
/// (`ReplayHeader::numTicksFieldOffset`, `replay_header.cpp:255-261`): the
/// whole header minus the field itself.
fn num_ticks_field_offset(signature_length: u16) -> usize {
    header_stream_size(signature_length) - 4
}

// -----------------------------------------------------------------------------
// Section: checksum schemes (`lib/cstdmf/checksum_stream.hpp`)
// -----------------------------------------------------------------------------

/// The write half of `ChecksumScheme` (`checksum_stream.hpp:24-57`): a
/// resettable accumulator that produces the signature bytes appended after
/// a signed block. `read_blob` feeds the bytes to sum — for a replay file
/// the writer always feeds the previous signature first, which is what
/// chains the signatures together.
pub trait SigningScheme {
    /// `streamSize()`: the signature length appended to each block.
    fn stream_size(&self) -> usize;
    /// `reset()`: start a fresh round of checksumming.
    fn reset(&mut self);
    /// `readBlob()`: feed data to sum.
    fn read_blob(&mut self, data: &[u8]);
    /// `addToStream()`: the signature over everything fed since `reset`.
    fn add_to_stream(&mut self) -> Vec<u8>;
}

/// `MD5SumScheme` (`checksum_stream.hpp:141-160`): the MD5 of the fed
/// bytes, 16-byte signature. The scheme the original replay unit tests
/// sign with.
#[derive(Default)]
pub struct Md5Checksum {
    hasher: md5::Md5,
}

impl Md5Checksum {
    /// `MD5SumScheme::create()`.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SigningScheme for Md5Checksum {
    fn stream_size(&self) -> usize {
        16
    }

    fn reset(&mut self) {
        self.hasher = md5::Md5::new();
    }

    fn read_blob(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    fn add_to_stream(&mut self) -> Vec<u8> {
        self.hasher.finalize_reset().to_vec()
    }
}

/// `SHAChecksumScheme` (`sha_checksum_scheme.cpp`): the SHA-256 of the fed
/// bytes, 32-byte signature — the digest half of the recording scheme.
#[derive(Default)]
pub struct Sha256Checksum {
    hasher: Sha256,
}

impl Sha256Checksum {
    /// `SHAChecksumScheme::create()`.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SigningScheme for Sha256Checksum {
    fn stream_size(&self) -> usize {
        32
    }

    fn reset(&mut self) {
        self.hasher = Sha256::new();
    }

    fn read_blob(&mut self, data: &[u8]) {
        use sha2::Digest as _;
        self.hasher.update(data);
    }

    fn add_to_stream(&mut self) -> Vec<u8> {
        use sha2::Digest as _;
        self.hasher.finalize_reset().to_vec()
    }
}

// -----------------------------------------------------------------------------
// Section: stream helpers (`lib/cstdmf/binary_stream.hpp`)
// -----------------------------------------------------------------------------

/// `BinaryOStream::writePackedInt` (`binary_stream.hpp:254-272`): one byte
/// below 255, otherwise `0xFF` and the low three bytes little-endian
/// (`BW_PACK3`, `binary_stream.ipp:155-161`).
fn write_packed_int(value: u32, out: &mut Vec<u8>) {
    if value >= 255 {
        out.push(0xff);
        out.extend_from_slice(&value.to_le_bytes()[0..3]);
    } else {
        out.push(value as u8);
    }
}

/// `ReplayMetaData::calculateStringPackedSize`
/// (`replay_metadata.cpp:240-245`): the length prefix is one byte below
/// 256, three above.
fn string_packed_size(s: &str) -> usize {
    (if s.len() < 256 { 1 } else { 3 }) + s.len()
}

/// `BinaryOStream::appendString` (`binary_stream.hpp:389-396`): a packed
/// length, then the bytes. No terminator — BW strings may hold NULs.
fn append_string(s: &str, out: &mut Vec<u8>) {
    write_packed_int(s.len() as u32, out);
    out.extend_from_slice(s.as_bytes());
}

// -----------------------------------------------------------------------------
// Section: the metadata block (`lib/connection/replay_metadata.cpp`)
// -----------------------------------------------------------------------------

/// The user-supplied key/value collection embedded in a recording
/// (`ReplayMetaData`). The collection is ordered by key, the way the
/// reference's `BW::map` iterates it into the stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayMetaData {
    collection: BTreeMap<String, String>,
    /// `streamSize_`: the packed size of the collection alone, tracked as
    /// pairs are added (`replay_metadata.cpp:154-169`).
    stream_size: u32,
}

impl ReplayMetaData {
    /// `ReplayMetaData::ReplayMetaData()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `add()` (`replay_metadata.cpp:154-169`): insert the pair, replacing
    /// an existing key and adjusting the tracked size.
    pub fn add(&mut self, key: &str, value: &str) {
        if let Some(old) = self.collection.insert(key.to_string(), value.to_string()) {
            self.stream_size -= (string_packed_size(key) + string_packed_size(&old)) as u32;
        }
        self.stream_size += (string_packed_size(key) + string_packed_size(value)) as u32;
    }

    /// `get()`: the value under `key`, or the default.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.collection.get(key).map(String::as_str)
    }

    /// `hasKey()`.
    pub fn has_key(&self, key: &str) -> bool {
        self.collection.contains_key(key)
    }

    /// `size()`: the number of pairs.
    pub fn size(&self) -> usize {
        self.collection.len()
    }

    /// `streamSize()`: the packed collection size.
    pub fn stream_size(&self) -> u32 {
        self.stream_size
    }

    /// `clear()`.
    pub fn clear(&mut self) {
        self.collection.clear();
        self.stream_size = 0;
    }

    /// `addToStream` (`replay_metadata.cpp:60-78`): the `streamSize` word,
    /// the pairs, then the signature appended by finalising. The scheme is
    /// *not* reset here — the caller seeds it (the reference constructs the
    /// wrapping `ChecksumOStream` with `shouldReset == false`), so the
    /// signature chains off whatever was fed before.
    ///
    /// Returns the block bytes (payload and signature, as the file holds
    /// them) and the signature.
    pub fn signed_block(&self, scheme: &mut dyn SigningScheme) -> (Vec<u8>, Vec<u8>) {
        let mut payload = Vec::with_capacity(4 + self.stream_size as usize);
        payload.extend_from_slice(&self.stream_size.to_le_bytes());
        for (key, value) in &self.collection {
            append_string(key, &mut payload);
            append_string(value, &mut payload);
        }

        scheme.read_blob(&payload);
        let signature = scheme.add_to_stream();

        let mut block = payload;
        block.extend_from_slice(&signature);
        (block, signature)
    }
}

// -----------------------------------------------------------------------------
// Section: the header block (`lib/connection/replay_header.cpp`)
// -----------------------------------------------------------------------------

/// The header block of a replay file (`ReplayHeader`). Built with the
/// current protocol version, the definition digest, the game update
/// frequency, and a nonce (`replay_header.cpp:48-73`); the server-side
/// writer stamps it with the current time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHeader {
    digest: [u8; 16],
    game_update_frequency: u8,
    timestamp: u32,
    nonce: [u8; NONCE_SIZE],
    reported_signature_length: u16,
    num_ticks: u32,
}

impl ReplayHeader {
    /// The writing constructor (`replay_header.cpp:48-73`). The nonce must
    /// be [`NONCE_SIZE`] bytes — the reference only generates one itself on
    /// the server, and takes it explicitly for unit tests.
    pub fn new(
        signature_length: usize,
        digest: [u8; 16],
        game_update_frequency: u8,
        nonce: [u8; NONCE_SIZE],
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        Self {
            digest,
            game_update_frequency,
            timestamp,
            nonce,
            reported_signature_length: signature_length as u16,
            num_ticks: 0,
        }
    }

    /// `numTicks()`: how many ticks the recording was finalised with. Zero
    /// until `ReplayDataFileWriter::finalise` patches the field.
    pub fn num_ticks(&self) -> u32 {
        self.num_ticks
    }

    /// `addToStream` (`replay_header.ipp:13-42`): the version, digest,
    /// frequency, timestamp, nonce and signature length summed and signed
    /// (the wrapping `ChecksumOStream` resets the scheme —
    /// `shouldReset` defaults to true), the signature appended, then the
    /// `numTicks` word written outside the signature: it changes when the
    /// file is finalised, and the signature is not rewritten for it.
    ///
    /// Returns the block bytes and the signature.
    pub fn signed_block(&self, scheme: &mut dyn SigningScheme) -> (Vec<u8>, Vec<u8>) {
        let mut payload = Vec::with_capacity(HEADER_PREFIX_SIZE);
        payload.extend_from_slice(&PROTOCOL_VERSION_BYTES);
        payload.extend_from_slice(&self.digest);
        payload.push(self.game_update_frequency);
        payload.extend_from_slice(&self.timestamp.to_le_bytes());
        payload.extend_from_slice(&self.nonce);
        payload.extend_from_slice(&self.reported_signature_length.to_le_bytes());

        scheme.reset();
        scheme.read_blob(&payload);
        let signature = scheme.add_to_stream();

        let mut block = payload;
        block.extend_from_slice(&signature);
        block.extend_from_slice(&self.num_ticks.to_le_bytes());
        (block, signature)
    }
}

// -----------------------------------------------------------------------------
// Section: tick compression (`lib/network/compression_stream.cpp`)
// -----------------------------------------------------------------------------

/// `BWCompressionType` (`lib/network/compression_type.hpp`): no
/// compression, or zlib levels 1 (best speed) through 9 (best
/// compression).
pub mod compression {
    /// `BW_COMPRESSION_NONE`.
    pub const NONE: u8 = 0;
    /// `BW_COMPRESSION_ZIP_BEST_SPEED` (`BW_COMPRESSION_ZIP_1`).
    pub const ZIP_MIN: u8 = 1;
    /// `BW_COMPRESSION_ZIP_BEST_COMPRESSION` (`BW_COMPRESSION_ZIP_9`).
    pub const ZIP_MAX: u8 = 9;
}

/// `CompressionOStream` over a tick's cell data
/// (`compression_stream.cpp:60-97` + `ZipOStream` destructor,
/// `zip_stream.cpp:261-289`): the compression type byte, then for a zip
/// level the packed raw length, the packed compressed length, and the
/// zlib bytes (`compress2` at the level the type names). An invalid type
/// falls back to `BW_COMPRESSION_NONE`, the way the reference's
/// `CRITICAL_MSG` path does.
fn compress_tick_payload(data: &[u8], compression_type: u8) -> Vec<u8> {
    let mut blob = vec![compression_type];
    if (compression::ZIP_MIN..=compression::ZIP_MAX).contains(&compression_type) {
        let mut encoder = flate2::write::ZlibEncoder::new(
            Vec::new(),
            flate2::Compression::new(compression_type as u32),
        );
        encoder.write_all(data).expect("in-memory zlib write");
        let compressed = encoder.finish().expect("in-memory zlib finish");
        write_packed_int(data.len() as u32, &mut blob);
        write_packed_int(compressed.len() as u32, &mut blob);
        blob.extend_from_slice(&compressed);
    } else {
        // Anything but NONE or a zip level streams raw, typed as NONE.
        blob[0] = compression::NONE;
        blob.extend_from_slice(data);
    }
    blob
}

// -----------------------------------------------------------------------------
// Section: one tick's accumulation (`TickReplayData`,
// `replay_data_file_writer.hpp:27-55`)
// -----------------------------------------------------------------------------

/// One game tick's data as it accumulates from the space's cells
/// (`TickReplayData`). A tick is complete once every expected cell has
/// transferred its blob; finalising wraps the concatenated cell data in
/// the compression stream behind the tick's two-word header.
#[derive(Debug, Clone, Default)]
pub struct TickReplayData {
    tick: u32,
    num_expected_cells: u32,
    num_cells_left: u32,
    cells: Vec<u8>,
    blob: Option<Vec<u8>>,
}

impl TickReplayData {
    /// `init(tick, numCells)` (`replay_data_file_writer.cpp:772-784`).
    pub fn init(tick: u32, num_cells: u32) -> Self {
        Self {
            tick,
            num_expected_cells: num_cells,
            num_cells_left: num_cells,
            cells: Vec::new(),
            blob: None,
        }
    }

    /// `transferFromCell` (`replay_data_file_writer.cpp:801-813`): append
    /// the cell's whole blob and count the cell. Data arriving after the
    /// set is already complete is dropped, as the reference's NOTICE path
    /// does.
    pub fn transfer_from_cell(&mut self, data: &[u8]) {
        if self.num_cells_left == 0 {
            return;
        }
        self.cells.extend_from_slice(data);
        self.num_cells_left -= 1;
    }

    /// `numExpectedCells(newValue)` (`replay_data_file_writer.cpp:854-869`):
    /// only ever raised — a cell joining the space raises the expected
    /// count of the tick it reports first.
    pub fn raise_expected_cells(&mut self, new_value: u32) {
        if new_value > self.num_expected_cells {
            self.num_cells_left += new_value - self.num_expected_cells;
            self.num_expected_cells = new_value;
        }
    }

    /// `numCellsLeft()`.
    pub fn num_cells_left(&self) -> u32 {
        self.num_cells_left
    }

    /// `tick()`.
    pub fn tick(&self) -> u32 {
        self.tick
    }

    /// `size()`: the finalised blob length.
    pub fn size(&self) -> usize {
        self.blob.as_ref().map_or(0, Vec::len)
    }

    /// `data()`: the finalised blob.
    pub fn data(&self) -> Option<&[u8]> {
        self.blob.as_deref()
    }

    /// `finalise(compressionType)` (`replay_data_file_writer.cpp:821-845`):
    /// the tick header (`streamTickHeader`: the game time, then the
    /// length word reserved at zero) and the compressed cell data, with
    /// the length word patched to the payload size behind it.
    pub fn finalise(&mut self, compression_type: u8) {
        let mut blob = Vec::new();
        blob.extend_from_slice(&self.tick.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        let compressed = compress_tick_payload(&self.cells, compression_type);
        blob.extend_from_slice(&compressed);
        let length = (blob.len() - 8) as u32;
        blob[4..8].copy_from_slice(&length.to_le_bytes());
        self.blob = Some(blob);
    }
}

/// `TickReplayData::tickHeaderSize()` (`replay_data_file_writer.cpp:876-879`):
/// the game time and length words of a tick with no data.
const TICK_HEADER_SIZE: usize = 8;

// -----------------------------------------------------------------------------
// Section: the file writer (`ReplayDataFileWriter`,
// `server/baseapp/replay_data_file_writer.cpp`)
// -----------------------------------------------------------------------------

/// The write events of a replay file (`ReplayDataFileWriterListener`,
/// `replay_data_file_writer.hpp:62-85`). The writer hands snapshot data
/// rather than a reference to itself.
pub trait ReplayDataFileWriterListener {
    /// `onReplayDataFileWritingError`: a write failed; the writer keeps
    /// its error state.
    fn on_writing_error(&mut self, path: &str, error: &str);
    /// `onReplayDataFileWritingComplete`: a write landed. Fired once per
    /// completed write, in the reference's completion-handler position.
    fn on_writing_complete(&mut self, ticks_written: u32, last_tick_written: u32);
    /// `onReplayDataFileWriterDestroyed`: the writer is gone.
    fn on_writer_destroyed(&mut self, path: &str);
}

/// Writes a replay data file (`ReplayDataFileWriter`): the header and
/// metadata blocks up front, then signed chunks of completed ticks, then
/// the finalisation pass that patches `numTicks`, marks the file
/// read-only, and closes.
pub struct ReplayDataFileWriter {
    path: PathBuf,
    file: Option<std::fs::File>,
    scheme: Box<dyn SigningScheme>,
    compression_type: u8,
    header_signature_length: u16,
    last_signature: Vec<u8>,
    num_ticks_to_sign: u32,
    buffered: BTreeMap<u32, TickReplayData>,
    current_tick: u32,
    last_tick_pending_write: u32,
    last_tick_written: u32,
    last_chunk_position: u64,
    last_chunk_length: u64,
    num_ticks_written: u32,
    bytes_written: u64,
    is_finalising: bool,
    /// Set while `finalise` is between the last chunk and the `numTicks`
    /// patch: the completing chunk must not close the file the patch
    /// still needs (the synchronous spelling of the reference's async
    /// ordering).
    patch_pending: bool,
    error: Option<String>,
    listeners: Vec<Box<dyn ReplayDataFileWriterListener>>,
}

impl ReplayDataFileWriter {
    /// The new-file constructor (`replay_data_file_writer.cpp:40-88`):
    /// open the file, stream the signed header block, seed the scheme with
    /// the header's signature, and stream the signed metadata block after
    /// it. The reference queues this first write to the background thread;
    /// the port writes it synchronously and runs the completion
    /// bookkeeping inline (a `userArg` of 0, so no tick accounting).
    pub fn new(
        path: &Path,
        scheme: Box<dyn SigningScheme>,
        compression_type: u8,
        digest: [u8; 16],
        num_ticks_to_sign: u32,
        metadata: &ReplayMetaData,
        nonce: [u8; NONCE_SIZE],
    ) -> Self {
        let header_signature_length = scheme.stream_size() as u16;
        // `fopen(path, "wb")` (`replay_data_file_writer.cpp:63-71`): a
        // fresh or truncated file. A failure here is the writer's first
        // error state.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path);
        let mut writer = Self {
            path: path.to_path_buf(),
            file: None,
            scheme,
            compression_type,
            header_signature_length,
            last_signature: Vec::new(),
            num_ticks_to_sign,
            buffered: BTreeMap::new(),
            current_tick: 0,
            last_tick_pending_write: 0,
            last_tick_written: 0,
            last_chunk_position: 0,
            last_chunk_length: 0,
            num_ticks_written: 0,
            bytes_written: 0,
            is_finalising: false,
            patch_pending: false,
            error: None,
            listeners: Vec::new(),
        };

        writer.file = match file {
            Ok(file) => Some(file),
            Err(err) => {
                writer.fail(&err.to_string());
                return writer;
            }
        };

        let header = ReplayHeader::new(header_signature_length as usize, digest, 30, nonce);
        let (header_block, header_signature) = header.signed_block(writer.scheme.as_mut());

        // Prime the scheme with the header's signature for the metadata
        // chunk (`replay_data_file_writer.cpp:78-80`).
        writer.scheme.reset();
        writer.scheme.read_blob(&header_signature);

        let (metadata_block, metadata_signature) = metadata.signed_block(writer.scheme.as_mut());
        writer.last_signature = metadata_signature;

        writer.last_chunk_position = (header_block.len() + metadata_block.len()) as u64;

        let mut first_write = header_block;
        first_write.extend_from_slice(&metadata_block);
        writer.write_file(first_write, 0);
        writer
    }

    /// `addListener()` (`replay_data_file_writer.cpp:439-443`).
    pub fn add_listener(&mut self, listener: Box<dyn ReplayDataFileWriterListener>) {
        self.listeners.push(listener);
    }

    /// `path()`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `isFinalising()`.
    pub fn is_finalising(&self) -> bool {
        self.is_finalising
    }

    /// `isClosed()` (`replay_data_file_writer.hpp:116`): finalising with
    /// the file released.
    pub fn is_closed(&self) -> bool {
        self.is_finalising && self.file.is_none()
    }

    /// `hasError()`.
    pub fn has_error(&self) -> bool {
        self.error.is_some()
    }

    /// `errorString()`.
    pub fn error_string(&self) -> String {
        self.error.clone().unwrap_or_default()
    }

    /// `compressionType()`.
    pub fn compression_type(&self) -> u8 {
        self.compression_type
    }

    /// `numTicksToSign()`.
    pub fn num_ticks_to_sign(&self) -> u32 {
        self.num_ticks_to_sign
    }

    /// `numTicksWritten()`.
    pub fn num_ticks_written(&self) -> u32 {
        self.num_ticks_written
    }

    /// `lastTickWritten()`.
    pub fn last_tick_written(&self) -> u32 {
        self.last_tick_written
    }

    /// `lastTickPendingWrite()`.
    pub fn last_tick_pending_write(&self) -> u32 {
        self.last_tick_pending_write
    }

    /// `lastChunkPosition()`.
    pub fn last_chunk_position(&self) -> u64 {
        self.last_chunk_position
    }

    /// `lastChunkLength()`.
    pub fn last_chunk_length(&self) -> u64 {
        self.last_chunk_length
    }

    /// The last signature the scheme produced — the chain head a recovered
    /// writer would resume from (`recoveryData`,
    /// `replay_data_file_writer.cpp:734-747`).
    pub fn last_signature(&self) -> &[u8] {
        &self.last_signature
    }

    /// `addTickData` (`replay_data_file_writer.cpp:179-262`): one cell's
    /// blob for one tick. A finalising writer drops it; a tick older than
    /// the write cursor is dropped; the very first tick cannot be empty;
    /// once a tick has every expected cell's data, the write cursor
    /// advances over it and completed ticks are chunked out.
    pub fn add_tick_data(&mut self, tick: u32, num_cells: u32, blob: &[u8]) {
        if self.is_finalising {
            return;
        }

        if tick < self.current_tick {
            return;
        }

        if self.current_tick == 0 && self.buffered.is_empty() {
            if blob.is_empty() {
                // The reference ignores a first tick with no data
                // (`replay_data_file_writer.cpp:216-226`).
                return;
            }
            self.current_tick = tick;
            // Pretend the earlier ticks were written.
            self.last_tick_written = tick.wrapping_sub(1);
            self.last_tick_pending_write = tick.wrapping_sub(1);
        }

        let entry = self.buffered.entry(tick).or_insert_with(|| {
            let mut tick_data = TickReplayData::init(tick, num_cells);
            tick_data.raise_expected_cells(num_cells);
            tick_data
        });
        entry.raise_expected_cells(num_cells);
        entry.transfer_from_cell(blob);

        if entry.num_cells_left() == 0 {
            let completed_tick = tick;
            self.finalise_completed_tick_data(completed_tick);
        }
    }

    /// `finaliseCompletedTickData` (`replay_data_file_writer.cpp:275-304`):
    /// finalise every buffered tick up to the completed one — earlier
    /// incomplete ticks are written out as they are — then chunk.
    fn finalise_completed_tick_data(&mut self, completed_tick: u32) {
        while self.current_tick <= completed_tick {
            if let Some(tick_data) = self.buffered.get_mut(&self.current_tick) {
                tick_data.finalise(self.compression_type);
            }
            self.current_tick += 1;
        }
        self.sign_and_write_completed_tick_data(false);
    }

    /// `signAndWriteCompletedTickData` (`replay_data_file_writer.cpp:314-426`):
    /// emit chunks of `numTicksToSign` ticks each — unless this is the
    /// last chunk, which signs whatever remains. Each chunk is the chunk
    /// length word, the ticks (empty ones as bare headers), and the
    /// signature; the scheme is seeded with the previous signature first,
    /// chaining every block in the file together.
    fn sign_and_write_completed_tick_data(&mut self, is_last_chunk: bool) {
        if self.current_tick == 0 {
            // No data has been added yet.
            return;
        }

        let mut next_tick_to_write = self.last_tick_pending_write.wrapping_add(1);
        while next_tick_to_write < self.current_tick
            && (is_last_chunk || self.current_tick - next_tick_to_write >= self.num_ticks_to_sign)
        {
            let num_ticks_to_actually_sign = self
                .num_ticks_to_sign
                .min(self.current_tick - next_tick_to_write);

            // First pass: the chunk length over the tick data, accounting
            // for ticks with no data as bare headers
            // (`replay_data_file_writer.cpp:344-381`).
            let mut chunk_length: u32 = 0;
            let mut chunk_time = next_tick_to_write;
            for (tick, tick_data) in self.buffered.range(next_tick_to_write..) {
                if chunk_time >= next_tick_to_write + num_ticks_to_actually_sign {
                    break;
                }
                if chunk_time < *tick {
                    // Missing ticks are all-empty and all identical size.
                    chunk_length += (*tick - chunk_time) * TICK_HEADER_SIZE as u32;
                    chunk_time = *tick;
                }
                chunk_length += tick_data.size() as u32;
                chunk_time += 1;
            }

            // Second pass: the payload, signed over the previous
            // signature.
            let mut payload = Vec::with_capacity(4 + chunk_length as usize);
            payload.extend_from_slice(&chunk_length.to_le_bytes());
            let mut chunk_time = next_tick_to_write;
            for (tick, tick_data) in self.buffered.range(next_tick_to_write..) {
                if chunk_time >= next_tick_to_write + num_ticks_to_actually_sign {
                    break;
                }
                while chunk_time < *tick {
                    payload.extend_from_slice(&chunk_time.to_le_bytes());
                    payload.extend_from_slice(&0u32.to_le_bytes());
                    chunk_time += 1;
                }
                payload.extend_from_slice(tick_data.data().expect("finalised tick data"));
                chunk_time += 1;
            }

            self.scheme.reset();
            self.scheme.read_blob(&self.last_signature);
            self.scheme.read_blob(&payload);
            let signature = self.scheme.add_to_stream();
            self.last_signature = signature.clone();

            self.last_tick_pending_write = chunk_time.wrapping_sub(1);

            let mut file_data = payload;
            file_data.extend_from_slice(&signature);
            self.write_file(file_data, self.last_tick_pending_write);

            next_tick_to_write = self.last_tick_pending_write.wrapping_add(1);
        }
    }

    /// `close()` (`replay_data_file_writer.cpp:518-528`): finalise, or
    /// release the file without patching it.
    pub fn close(&mut self, should_finalise: bool) {
        if should_finalise {
            self.finalise();
        } else {
            self.close_writer();
        }
    }

    /// `finalise()` (`replay_data_file_writer.cpp:534-559`): the header's
    /// `numTicks` field is patched in place with the count the file will
    /// hold, the file is marked read-only, and the last chunk signs
    /// whatever is left. The reference queues the last chunk to the
    /// background thread, so its `pFile_` is still open when the patch
    /// lands; the synchronous port patches first for the same reason —
    /// the completing last chunk closes the writer — and seeks back to
    /// the tail before the chunk goes out. The bytes on the file are the
    /// same either way.
    pub fn finalise(&mut self) {
        self.is_finalising = true;

        // The reference queues the last chunk to the background thread,
        // so its `pFile_` is still open when the patch lands and the
        // count is only known once `signAndWriteCompletedTickData` has
        // advanced `lastTickPendingWrite_` (`numTicksWillBeWritten`,
        // `replay_data_file_writer.cpp:542-548`). The synchronous port
        // holds the close that a completing last chunk would do, writes
        // the chunk, patches, and then lets the tail completion close the
        // writer — the bytes on the file are the same either way.
        self.patch_pending = true;
        self.sign_and_write_completed_tick_data(true);

        let num_ticks_will_be_written = self.num_ticks_written.wrapping_add(
            self.last_tick_pending_write
                .wrapping_sub(self.last_tick_written),
        );

        if let Some(file) = self.file.as_mut() {
            let field_offset = num_ticks_field_offset(self.header_signature_length) as u64;
            let patch = file
                .seek(std::io::SeekFrom::Start(field_offset))
                .and_then(|_| file.write_all(&num_ticks_will_be_written.to_le_bytes()))
                // The chunk writes already continue at the tail; restore
                // the cursor for any later write.
                .and_then(|_| file.seek(std::io::SeekFrom::Start(self.bytes_written)))
                .and_then(|_| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(std::fs::Permissions::from_mode(0o444))
                    }
                    #[cfg(not(unix))]
                    {
                        Ok(())
                    }
                });
            if let Err(err) = patch {
                self.fail(&err.to_string());
            }
        }
        self.patch_pending = false;

        // The finalise tail completes as one write (`userArg` 0); a
        // drained writer closes itself in the completing call.
        self.on_write_complete(0);
    }

    /// `closeWriter()` (`replay_data_file_writer.cpp:566-584`): release
    /// the file. Finalising stays set either way, which is what
    /// `isClosed` reports.
    fn close_writer(&mut self) {
        self.is_finalising = true;
        self.file = None;
    }

    /// Write bytes to the file and run the completion bookkeeping. The
    /// reference's background writer reports the position a write started
    /// at (`onBackgroundFileWritingComplete`); the synchronous port knows
    /// it up front.
    fn write_file(&mut self, data: Vec<u8>, user_arg: u32) {
        if self.error.is_some() {
            return;
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let start_position = self.bytes_written;
        match file.write_all(&data) {
            Ok(()) => {
                self.bytes_written += data.len() as u64;
                self.on_write_complete_position(start_position, user_arg);
            }
            Err(err) => self.fail(&err.to_string()),
        }
    }

    /// The reference's error path
    /// (`onBackgroundFileWritingError`, `replay_data_file_writer.cpp:449-454`):
    /// record the error and notify once.
    fn fail(&mut self, error: &str) {
        if self.error.is_none() {
            self.error = Some(error.to_string());
            self.close_writer();
            let path = self.path_str();
            for listener in &mut self.listeners {
                listener.on_writing_error(&path, error);
            }
        }
    }

    /// `onBackgroundFileWritingComplete` (`replay_data_file_writer.cpp:460-507`)
    /// for a write with no tick argument (the first block, the finalise
    /// tail).
    fn on_write_complete(&mut self, _file_position: u64) {
        let have_more_tick_data_to_write = self
            .buffered
            .keys()
            .next()
            .is_some_and(|first| *first < self.current_tick);
        if self.is_finalising && !self.patch_pending && !have_more_tick_data_to_write {
            self.close_writer();
        }
        self.notify_complete();
    }

    /// The tick-accounting half of
    /// `onBackgroundFileWritingComplete` (`replay_data_file_writer.cpp:463-493`):
    /// count the ticks the chunk carried, remember the chunk's position
    /// and length, and drop its ticks from the buffer.
    fn on_write_complete_position(&mut self, file_position: u64, user_arg: u32) {
        if user_arg != 0 {
            let new_last_tick_written = user_arg;
            let num_new_ticks = new_last_tick_written.wrapping_sub(self.last_tick_written);
            self.num_ticks_written = self.num_ticks_written.wrapping_add(num_new_ticks);

            self.last_chunk_position = file_position;
            self.last_chunk_length = 4; // the SignedChunkLength word

            let mut last_chunk_length = self.last_chunk_length;
            let after = self.last_tick_written.wrapping_add(1);
            let drained: Vec<u32> = self
                .buffered
                .range(after..=new_last_tick_written)
                .map(|(tick, data)| {
                    last_chunk_length += data.size() as u64;
                    *tick
                })
                .collect();
            for tick in drained {
                self.buffered.remove(&tick);
            }
            self.last_chunk_length = last_chunk_length + self.scheme.stream_size() as u64;
            self.last_tick_written = new_last_tick_written;
        }

        self.on_write_complete(file_position);
    }

    /// `notifyListenersOfCompletion` (`replay_data_file_writer.cpp:702-712`).
    fn notify_complete(&mut self) {
        let ticks = self.num_ticks_written;
        let last = self.last_tick_written;
        for listener in &mut self.listeners {
            listener.on_writing_complete(ticks, last);
        }
    }

    fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for ReplayDataFileWriter {
    /// The destructor (`replay_data_file_writer.cpp:153-168`): notify the
    /// listeners of destruction, noting ticks still buffered.
    fn drop(&mut self) {
        if !self.buffered.is_empty() {
            eprintln!(
                "[replay_write] Still have {} ticks to write out.",
                self.buffered.len()
            );
        }
        let path = self.path_str();
        for listener in &mut self.listeners {
            listener.on_writer_destroyed(&path);
        }
    }
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay_write::compression;
    use client::replay::{
        ChecksumScheme, ReplayError, decompress_tick, parse_header, parse_metadata,
        parse_metadata_chained, parse_tick_chunks,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    /// `fixtureDigest()` (`test_replay_header.cpp:22-31`): the MD5 of the
    /// 27-byte text.
    fn fixture_digest() -> [u8; 16] {
        let mut hasher = md5::Md5::new();
        hasher.update(b"BigWorld replay header test");
        hasher.finalize().into()
    }

    /// `fixtureNonce()` (`test_replay_header.cpp:37-42`).
    fn fixture_nonce() -> [u8; NONCE_SIZE] {
        [1, 2, 3, 4, 5, 6, 7, 8]
    }

    /// A raw MD5 over bytes, for recomputing signatures externally.
    fn md5(data: &[u8]) -> Vec<u8> {
        let mut hasher = md5::Md5::new();
        hasher.update(data);
        hasher.finalize().to_vec()
    }

    /// The verify half of `MD5SumScheme`, on the reader's trait, so the
    /// written blocks can be checked the way the reference reader checks
    /// them.
    struct Md5Verify;

    impl ChecksumScheme for Md5Verify {
        fn stream_size(&self) -> usize {
            16
        }

        fn verify(&self, data: &[u8], signature: &[u8]) -> bool {
            signature == md5(data)
        }
    }

    /// A listener that counts the callbacks, sharing the destroyed count
    /// so a test can observe it after the writer is dropped.
    struct CountingListener {
        completes: Cell<u32>,
        errors: Cell<u32>,
        destroyed: Rc<Cell<u32>>,
    }

    impl CountingListener {
        fn new() -> (Self, Rc<Cell<u32>>) {
            let destroyed = Rc::new(Cell::new(0));
            (
                Self {
                    completes: Cell::new(0),
                    errors: Cell::new(0),
                    destroyed: Rc::clone(&destroyed),
                },
                destroyed,
            )
        }
    }

    impl ReplayDataFileWriterListener for CountingListener {
        fn on_writing_error(&mut self, _path: &str, _error: &str) {
            self.errors.set(self.errors.get() + 1);
        }

        fn on_writing_complete(&mut self, _ticks: u32, _last: u32) {
            self.completes.set(self.completes.get() + 1);
        }

        fn on_writer_destroyed(&mut self, _path: &str) {
            self.destroyed.set(self.destroyed.get() + 1);
        }
    }

    /// A fresh recording file in a unique temp directory.
    fn temp_file(name: &str) -> (tempdir::TempDir, PathBuf) {
        let dir = tempdir::TempDir::new("replay-write").expect("temp dir");
        let path = dir.path().join(name);
        (dir, path)
    }

    /// A tiny scoped temp directory (the workspace keeps test dependencies
    /// to the standard set, so the directory cleans itself up on drop).
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new(prefix: &str) -> std::io::Result<Self> {
                let dir = std::env::temp_dir().join(format!(
                    "{prefix}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("clock")
                        .as_nanos()
                ));
                std::fs::create_dir_all(&dir)?;
                Ok(Self(dir))
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    // -------------------------------------------------------------------------
    // BW-07-05-02: the header block.
    // -------------------------------------------------------------------------

    /// The header block streams the version subpatch-first, then the
    /// digest, frequency, timestamp, nonce and signature length, the MD5
    /// signature over those bytes, and the trailing `numTicks` word.
    #[test]
    fn the_header_block_matches_the_reference_layout() {
        let mut scheme = Md5Checksum::new();
        let header = ReplayHeader::new(16, fixture_digest(), 10, fixture_nonce());
        let (block, signature) = header.signed_block(&mut scheme);

        assert_eq!(block.len(), 55, "4 + 16 + 1 + 4 + 8 + 2 + 16 + 4");
        assert_eq!(&block[0..4], &[0, 0, 9, 2], "subpatch, patch, minor, major");
        assert_eq!(&block[4..20], &fixture_digest());
        assert_eq!(block[20], 10, "gameUpdateFrequency");
        assert_gt_bytes(&block[21..25]);
        assert_eq!(&block[25..33], &fixture_nonce());
        assert_eq!(&block[33..35], &16u16.to_le_bytes());
        assert_eq!(signature, md5(&block[0..35]));
        assert_eq!(&block[35..51], &signature[..]);
        assert_eq!(&block[51..55], &0u32.to_le_bytes(), "numTicks starts at 0");
        assert_eq!(header_stream_size(16), 55);
        assert_eq!(
            num_ticks_field_offset(16),
            51,
            "numTicks sits before the end of the header"
        );
    }

    /// The timestamp is stamped with the current time, like the reference
    /// server's constructor does.
    fn assert_gt_bytes(bytes: &[u8]) {
        let timestamp = u32::from_le_bytes(bytes.try_into().expect("4 bytes"));
        assert!(timestamp > 0, "the header is stamped with the time");
    }

    /// The written header parses through the ported reference reader with
    /// every field restored and the whole block consumed.
    #[test]
    fn the_written_header_parses_through_the_reference_reader() {
        let mut scheme = Md5Checksum::new();
        let header = ReplayHeader::new(16, fixture_digest(), 10, fixture_nonce());
        let (block, _) = header.signed_block(&mut scheme);

        let (parsed, consumed) = parse_header(&block, Some(&Md5Verify)).expect("parses");
        assert_eq!(parsed.version, (2, 9, 0, 0));
        assert_eq!(parsed.digest, fixture_digest());
        assert_eq!(parsed.game_update_frequency, 10);
        assert!(parsed.timestamp > 0);
        assert_eq!(parsed.nonce, fixture_nonce());
        assert_eq!(parsed.signature_length, 16);
        assert_eq!(parsed.num_ticks, 0);
        assert_eq!(consumed, 55);
    }

    /// Flipping a signature byte fails the read, like
    /// `ReplayHeader_tamperDetection`.
    #[test]
    fn a_tampered_header_signature_fails_verification() {
        let mut scheme = Md5Checksum::new();
        let header = ReplayHeader::new(16, fixture_digest(), 10, fixture_nonce());
        let (mut block, _) = header.signed_block(&mut scheme);
        let last = block.len() - 1 - 4; // inside the signature
        block[last] ^= 0xff;

        assert_eq!(
            parse_header(&block, Some(&Md5Verify)),
            Err(ReplayError::ChecksumMismatch)
        );
    }

    // -------------------------------------------------------------------------
    // BW-07-05-02: the metadata block.
    // -------------------------------------------------------------------------

    /// The signed metadata block round trips: the pairs come back ordered,
    /// the block carries its own signature, and the whole block is
    /// consumed — the shape `ReplayMetaData_signedRoundTrip` checks.
    #[test]
    fn the_metadata_block_round_trips_through_the_reference_reader() {
        let mut metadata = ReplayMetaData::new();
        metadata.add("app", "bigworld");
        metadata.add("map", "Shark");
        assert_eq!(metadata.size(), 2);
        // 4 (app) + 9 (bigworld) + 4 (map) + 6 (Shark)
        assert_eq!(metadata.stream_size(), 23);

        let mut scheme = Md5Checksum::new();
        let (block, signature) = metadata.signed_block(&mut scheme);
        assert_eq!(block.len(), 4 + 23 + 16);
        assert_eq!(signature, md5(&block[0..4 + 23]));
        assert_eq!(&block[block.len() - 16..], &signature[..]);

        let (parsed, consumed) = parse_metadata(&block, 16, Some(&Md5Verify)).expect("parses");
        assert_eq!(
            parsed.entries,
            vec![
                ("app".to_string(), "bigworld".to_string()),
                ("map".to_string(), "Shark".to_string()),
            ]
        );
        assert_eq!(parsed.stream_size, 23);
        assert_eq!(parsed.signature, signature);
        assert_eq!(consumed, block.len());
    }

    /// Re-adding a key replaces the pair and the tracked size; a long key
    /// costs the three-byte length prefix
    /// (`ReplayMetaData_collection`).
    #[test]
    fn re_adding_a_key_replaces_it_and_long_keys_pack_three_bytes() {
        let mut metadata = ReplayMetaData::new();
        metadata.add("key", "value");
        assert_eq!(metadata.stream_size(), (1 + 3) + (1 + 5) as u32);

        metadata.add("key", "a much longer value");
        assert_eq!(metadata.size(), 1);
        assert_eq!(metadata.get("key"), Some("a much longer value"));
        // 19 bytes is still below 256, so the prefix stays one byte.
        assert_eq!(metadata.stream_size(), (1 + 3) as u32 + (1 + 19) as u32);

        let long_key = "k".repeat(300);
        metadata.add(&long_key, "v");
        assert_eq!(metadata.size(), 2);
        assert_eq!(
            metadata.stream_size(),
            (1 + 3) as u32 + (1 + 19) as u32 + (3 + 300) as u32 + (1 + 1) as u32
        );
    }

    /// An empty block is just the size word and the signature
    /// (`ReplayMetaData_signedRoundTrip`'s empty leg).
    #[test]
    fn an_empty_metadata_block_is_size_and_signature_only() {
        let metadata = ReplayMetaData::new();
        let mut scheme = Md5Checksum::new();
        let (block, _) = metadata.signed_block(&mut scheme);
        assert_eq!(block.len(), 4 + 16);

        let (parsed, consumed) = parse_metadata(&block, 16, Some(&Md5Verify)).expect("parses");
        assert!(parsed.entries.is_empty());
        assert_eq!(consumed, block.len());
    }

    // -------------------------------------------------------------------------
    // BW-07-05-03: tick compression and the signed chunks.
    // -------------------------------------------------------------------------

    /// Cell payloads compress under NONE and the zip levels and come back
    /// byte-identical through the reference reader's decompression — the
    /// lossless read the acceptance names.
    #[test]
    fn tick_payloads_compress_and_round_trip_losslessly() {
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

        for compression_type in [
            compression::NONE,
            compression::ZIP_MIN,
            5,
            compression::ZIP_MAX,
        ] {
            let blob = compress_tick_payload(&payload, compression_type);
            assert_eq!(blob[0], compression_type, "type byte first");
            let restored = decompress_tick(&blob).expect("inflates");
            assert_eq!(restored, payload, "level {compression_type} is lossless");
        }

        // A compressible payload actually shrinks under a zip level.
        let repetitive = vec![7u8; 4096];
        let blob = compress_tick_payload(&repetitive, 9);
        assert!(blob.len() < repetitive.len(), "zlib shrinks the payload");

        // An invalid type falls back to raw NONE, typed as NONE.
        let blob = compress_tick_payload(&repetitive, 42);
        assert_eq!(blob[0], compression::NONE);
        assert_eq!(decompress_tick(&blob).expect("raw"), repetitive);
    }

    /// A tick gathers every cell's blob behind its two-word header, the
    /// length word naming the payload after it.
    #[test]
    fn a_tick_collects_its_cells_then_finalises_with_a_length_header() {
        let mut tick_data = TickReplayData::init(7, 2);
        assert_eq!(tick_data.num_cells_left(), 2);

        tick_data.transfer_from_cell(&[1, 2, 3]);
        tick_data.transfer_from_cell(&[4, 5]);
        assert_eq!(tick_data.num_cells_left(), 0);

        // Data past the complete set is dropped.
        tick_data.transfer_from_cell(&[9, 9, 9]);

        tick_data.finalise(compression::NONE);
        let blob = tick_data.data().expect("finalised").to_vec();
        assert_eq!(u32::from_le_bytes(blob[0..4].try_into().unwrap()), 7);
        let length = u32::from_le_bytes(blob[4..8].try_into().unwrap());
        assert_eq!(length as usize, blob.len() - 8);
        // NONE: the type byte then the concatenated cell bytes.
        assert_eq!(&blob[8..], &[compression::NONE, 1, 2, 3, 4, 5]);
        assert_eq!(tick_data.size(), blob.len());
    }

    /// A recording with `numTicksToSign = 2` signs chunks of two ticks,
    /// each signature chaining off the previous one: metadata off the
    /// header's, first chunk off the metadata's, second off the first's.
    #[test]
    fn signed_chunks_carry_a_chained_signature() {
        let (dir, path) = temp_file("chain.replay");
        let (listener, _) = CountingListener::new();

        let mut metadata = ReplayMetaData::new();
        metadata.add("app", "bigworld");

        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::ZIP_MIN,
            fixture_digest(),
            2,
            &metadata,
            fixture_nonce(),
        );
        writer.add_listener(Box::new(listener));

        for tick in 100u32..104 {
            writer.add_tick_data(tick, 1, &[tick as u8; 16]);
        }
        writer.finalise();
        assert!(writer.is_closed(), "finalising a drained writer closes it");

        let file = std::fs::read(&path).expect("file");
        let (header, mut pos) = parse_header(&file, None).expect("header");
        assert_eq!(header.signature_length, 16);
        let header_size = pos;

        let (metadata_parsed, used) = parse_metadata(&file[pos..], 16, None).expect("metadata");
        pos += used;
        assert_eq!(
            metadata_parsed.entries,
            vec![("app".to_string(), "bigworld".to_string())]
        );

        // The chain: metadata seeds off the header's signature, chunk one
        // off the metadata's, chunk two off the first chunk's. The
        // metadata payload here is 4 (streamSize) + 4 (app) + 9
        // (bigworld) = 17 bytes.
        let header_signature = header.signature.clone();
        let metadata_signature = metadata_parsed.signature.clone();
        assert_eq!(
            metadata_signature,
            md5(&[
                header_signature.as_slice(),
                &file[header_size..header_size + 17]
            ]
            .concat()),
            "metadata signature chains off the header signature"
        );

        let (chunks, consumed) =
            parse_tick_chunks(&file[pos..], 16, header.num_ticks).expect("chunks");
        assert_eq!(chunks.len(), 2, "four ticks in chunks of two");
        assert_eq!(pos + consumed, file.len(), "the file is fully consumed");

        // The chunk signature covers the length word and the tick data
        // (`stream << chunkLength` sits inside the `ChecksumOStream`).
        let mut chain_seed = metadata_signature;
        let mut chunk_start = pos;
        for chunk in &chunks {
            let chunk_length =
                u32::from_le_bytes(file[chunk_start..chunk_start + 4].try_into().unwrap()) as usize;
            let signed = &file[chunk_start..chunk_start + 4 + chunk_length];
            assert_eq!(chunk.signature, md5(&[&chain_seed, signed].concat()));
            chain_seed = chunk.signature.clone();
            chunk_start += 4 + chunk_length + 16;
        }
        assert_eq!(chunk_start, file.len(), "chunks tile the tail exactly");

        // The tick payloads round trip: each entry inflates back to the
        // concatenated cell bytes.
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.entries.len(), 2);
            for (j, entry) in chunk.entries.iter().enumerate() {
                assert_eq!(entry.game_time, 100 + (i * 2 + j) as u32);
                let restored = decompress_tick(&entry.blob).expect("inflates");
                let tick = entry.game_time;
                assert_eq!(restored, vec![tick as u8; 16]);
            }
        }
        let _ = dir;
    }

    /// A known event sequence recorded across chunks reads back
    /// losslessly through the reference reader faces — the acceptance
    /// run: header, metadata, every tick's every cell byte.
    #[test]
    fn the_recorded_file_parses_back_losslessly() {
        let (dir, path) = temp_file("e2e.replay");

        let mut metadata = ReplayMetaData::new();
        metadata.add("space", "deep-chain");
        metadata.add("recorded-by", "boundless");

        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::ZIP_MAX,
            fixture_digest(),
            4,
            &metadata,
            fixture_nonce(),
        );

        // Six ticks, two cells early and one late, payloads with the tick
        // and cell woven in so any mixup shows.
        let mut expected: Vec<(u32, Vec<u8>)> = Vec::new();
        for tick in 5u32..11 {
            let cells = if tick < 9 { 2 } else { 1 };
            let mut tick_bytes = Vec::new();
            for cell in 0..cells {
                let cell_payload: Vec<u8> = (0..64u32)
                    .map(|i| ((tick * 31 + cell * 7 + i) % 256) as u8)
                    .collect();
                tick_bytes.extend_from_slice(&cell_payload);
            }
            // Each cell arrives as its own addTickData call.
            for cell in 0..cells {
                let cell_payload: Vec<u8> = (0..64u32)
                    .map(|i| ((tick * 31 + cell * 7 + i) % 256) as u8)
                    .collect();
                writer.add_tick_data(tick, cells, &cell_payload);
            }
            expected.push((tick, tick_bytes));
        }
        writer.finalise();

        let file = std::fs::read(&path).expect("file");
        let (header, mut pos) = parse_header(&file, Some(&Md5Verify)).expect("header");
        assert_eq!(header.num_ticks, 6, "finalise patched the tick count");
        assert_eq!(header.digest, fixture_digest());

        let (parsed_metadata, used) =
            parse_metadata_chained(&file[pos..], 16, &Md5Verify, &header.signature)
                .expect("metadata");
        pos += used;
        // The reader sees the pairs in stream order: sorted by key.
        assert_eq!(
            parsed_metadata.entries,
            vec![
                ("recorded-by".to_string(), "boundless".to_string()),
                ("space".to_string(), "deep-chain".to_string()),
            ]
        );

        let (chunks, consumed) =
            parse_tick_chunks(&file[pos..], 16, header.num_ticks).expect("chunks");
        assert_eq!(pos + consumed, file.len(), "every byte accounted for");

        let restored: Vec<(u32, Vec<u8>)> = chunks
            .iter()
            .flat_map(|chunk| chunk.entries.iter())
            .map(|entry| {
                (
                    entry.game_time,
                    decompress_tick(&entry.blob).expect("inflates"),
                )
            })
            .collect();
        assert_eq!(restored, expected, "the sequence reads back losslessly");
        let _ = dir;
    }

    /// A tick with no data between recorded ticks is written as a bare
    /// header (`replay_data_file_writer.cpp:354-406`'s empty-tick path).
    #[test]
    fn missing_ticks_are_written_as_empty_headers() {
        let (dir, path) = temp_file("gaps.replay");

        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            3,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );
        writer.add_tick_data(1, 1, &[0xaa; 8]);
        writer.add_tick_data(3, 1, &[0xbb; 8]);
        writer.finalise();

        let file = std::fs::read(&path).expect("file");
        let (header, mut pos) = parse_header(&file, None).expect("header");
        let (_, used) = parse_metadata(&file[pos..], 16, None).expect("metadata");
        pos += used;
        let (chunks, consumed) =
            parse_tick_chunks(&file[pos..], 16, header.num_ticks).expect("chunks");
        assert_eq!(pos + consumed, file.len());

        assert_eq!(chunks.len(), 1, "three ticks sign as one chunk");
        let entries: Vec<(u32, usize)> = chunks[0]
            .entries
            .iter()
            .map(|e| (e.game_time, e.blob.len()))
            .collect();
        assert_eq!(
            entries,
            vec![(1, 9), (2, 0), (3, 9)],
            "tick 2 is a bare header between the recorded ticks"
        );
        let _ = dir;
    }

    // -------------------------------------------------------------------------
    // BW-07-05-04: finalisation, failure and the callback counts.
    // -------------------------------------------------------------------------

    /// Finalising patches `numTicks` in the file, marks it read-only,
    /// closes the writer, and the callbacks land once per write plus one
    /// destruction.
    #[test]
    fn finalise_patches_the_tick_count_and_closes_read_only() {
        let (dir, path) = temp_file("close.replay");
        let (listener, destroyed) = CountingListener::new();

        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            2,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );
        writer.add_listener(Box::new(listener));

        for tick in 1u32..=4 {
            writer.add_tick_data(tick, 1, &[tick as u8; 4]);
        }
        // Chunks of two flush as soon as two ticks complete, so both
        // chunks are already out before finalise.
        assert_eq!(writer.num_ticks_written(), 4);
        writer.finalise();

        assert!(writer.is_closed());
        assert_eq!(writer.num_ticks_written(), 4);
        assert_eq!(writer.last_tick_written(), 4);
        assert_eq!(writer.last_tick_pending_write(), 4);

        // Two chunk writes, the first block and the finalise tail.
        let file = std::fs::read(&path).expect("file");
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
        };
        assert_eq!(mode & 0o777, 0o444, "the recording is read-only");
        assert_eq!(
            u32::from_le_bytes(
                file[num_ticks_field_offset(16)..num_ticks_field_offset(16) + 4]
                    .try_into()
                    .unwrap()
            ),
            4,
            "the header's numTicks field holds the finalised count"
        );

        assert!(writer.has_listeners_after_drop_pending());
        assert_eq!(destroyed.get(), 0, "not yet dropped");
        drop(writer);
        assert_eq!(destroyed.get(), 1, "the destruction callback fires once");
        let _ = dir;
    }

    /// A write failure reports one error callback and the writer stays
    /// errored instead of panicking on later use.
    #[test]
    fn a_write_failure_reports_an_error_once() {
        let (dir, _path) = temp_file("fail.replay");
        let (listener, destroyed) = CountingListener::new();
        let unwritable = dir.path().join("no-such-dir").join("x.replay");

        let mut writer = ReplayDataFileWriter::new(
            &unwritable,
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            2,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );
        writer.add_listener(Box::new(listener));
        assert!(writer.has_error(), "the first write failed");
        assert!(!writer.error_string().is_empty());
        assert_eq!(writer.errors_after_construction(), 1);

        // Later use is inert, not a panic.
        writer.add_tick_data(1, 1, &[1, 2, 3]);
        writer.finalise();
        assert_eq!(destroyed.get(), 0);
        drop(writer);
        assert_eq!(destroyed.get(), 1);
    }

    /// Data past the finalise call is refused, the reference's
    /// `Already finalising` branch.
    #[test]
    fn data_added_after_finalising_is_refused() {
        let (dir, path) = temp_file("late.replay");
        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            2,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );
        writer.add_tick_data(1, 1, &[1; 8]);
        writer.finalise();
        assert!(writer.is_finalising());

        let size_before = std::fs::metadata(&path).expect("metadata").len();
        writer.add_tick_data(2, 1, &[2; 8]);
        let size_after = std::fs::metadata(&path).expect("metadata").len();
        assert_eq!(size_before, size_after, "no late data reached the file");
        let _ = dir;
    }

    /// The first tick cannot be empty, and a tick older than the write
    /// cursor is dropped (`replay_data_file_writer.cpp:191-231`).
    #[test]
    fn a_first_empty_tick_is_ignored_and_stale_ticks_are_dropped() {
        let (dir, path) = temp_file("stale.replay");
        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            4,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );

        writer.add_tick_data(1, 1, &[]);
        assert_eq!(
            writer.last_tick_written(),
            0,
            "the empty first tick was ignored"
        );

        writer.add_tick_data(5, 1, &[5; 8]);
        assert_eq!(
            writer.last_tick_written(),
            4,
            "the cursor pretends 1..4 written"
        );

        writer.add_tick_data(3, 1, &[3; 8]);
        assert_eq!(writer.last_tick_written(), 4, "the stale tick was dropped");
        writer.finalise();

        let file = std::fs::read(&path).expect("file");
        let (header, pos) = parse_header(&file, None).expect("header");
        assert_eq!(header.num_ticks, 1, "only tick 5 was recorded");
        let (_, used) = parse_metadata(&file[pos..], 16, None).expect("metadata");
        let (chunks, consumed) =
            parse_tick_chunks(&file[pos + used..], 16, header.num_ticks).expect("chunks");
        assert_eq!(pos + used + consumed, file.len());
        assert_eq!(chunks[0].entries.len(), 1);
        assert_eq!(chunks[0].entries[0].game_time, 5);
        let _ = dir;
    }

    /// Closing without finalising releases the writer but leaves the file
    /// untouched: `numTicks` stays zero and no read-only bit is set.
    #[test]
    fn closing_without_finalising_leaves_the_file_unpatched() {
        let (dir, path) = temp_file("abandon.replay");
        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            2,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );
        writer.add_tick_data(1, 1, &[1; 8]);
        writer.close(false);

        assert!(writer.is_closed(), "close releases the file");
        let file = std::fs::read(&path).expect("file");
        assert_eq!(
            u32::from_le_bytes(
                file[num_ticks_field_offset(16)..num_ticks_field_offset(16) + 4]
                    .try_into()
                    .unwrap()
            ),
            0,
            "numTicks was not patched"
        );
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert!(mode & 0o200 != 0, "still writable");
        }
        let _ = dir;
    }

    // -------------------------------------------------------------------------
    // BW-07-05-03: the tick-chunk face — schemes, accessors, flush timing.
    // -------------------------------------------------------------------------

    /// `SHAChecksumScheme` writes the file too: 32-byte signatures, and the
    /// reference reader parses the chunks back with the same length.
    #[test]
    fn the_sha256_scheme_signs_thirty_two_byte_signatures() {
        let (dir, path) = temp_file("sha.replay");

        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Sha256Checksum::new()),
            compression::ZIP_MAX,
            fixture_digest(),
            2,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );
        writer.add_tick_data(1, 1, &[0x11; 64]);
        writer.add_tick_data(2, 1, &[0x22; 64]);
        writer.finalise();

        let file = std::fs::read(&path).expect("file");
        let (header, pos) = parse_header(&file, None).expect("header");
        assert_eq!(header.signature_length, 32);
        let (_, used) = parse_metadata(&file[pos..], 32, None).expect("metadata");
        let (chunks, consumed) =
            parse_tick_chunks(&file[pos + used..], 32, header.num_ticks).expect("chunks");
        assert_eq!(pos + used + consumed, file.len());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].entries.len(), 2);
        let first = &chunks[0].entries[0];
        assert_eq!(first.game_time, 1);
        assert_eq!(first.blob[0], compression::ZIP_MAX, "the payload is zip-9");
        assert_eq!(
            decompress_tick(&first.blob).expect("decompress"),
            vec![0x11u8; 64],
            "the zip-9 payload reads back losslessly"
        );
        assert_eq!(
            decompress_tick(&chunks[0].entries[1].blob).expect("decompress"),
            vec![0x22u8; 64],
        );
        let _ = dir;
    }

    /// The metadata collection reports and resets its state.
    #[test]
    fn the_metadata_accessors_report_and_reset() {
        let mut metadata = ReplayMetaData::new();
        assert!(!metadata.has_key("key"));
        metadata.add("key", "value");
        metadata.add("other", "2");
        assert!(metadata.has_key("key"));
        assert_eq!(metadata.size(), 2);
        let packed = metadata.stream_size() as usize;
        assert_eq!(
            packed,
            string_packed_size("key")
                + string_packed_size("value")
                + string_packed_size("other")
                + string_packed_size("2")
        );

        metadata.clear();
        assert!(!metadata.has_key("key"));
        assert_eq!(metadata.size(), 0);
        assert_eq!(metadata.stream_size(), 0);
    }

    /// The writer reports its construction state, and a fresh header has
    /// no ticks yet.
    #[test]
    fn the_writer_accessors_report_the_construction_state() {
        let (dir, path) = temp_file("state.replay");
        let metadata = ReplayMetaData::new();

        let header = ReplayHeader::new(16, fixture_digest(), 30, fixture_nonce());
        assert_eq!(header.num_ticks(), 0, "unfinalised header");
        let (_, header_signature) = header.signed_block(&mut Md5Checksum::new());

        // The writer primes the scheme with the header's signature before
        // signing the metadata block, so the block it seeds off is
        // reproducible here.
        let mut primed = Md5Checksum::new();
        primed.reset();
        primed.read_blob(&header_signature);
        let (metadata_block, metadata_signature) = metadata.signed_block(&mut primed);

        let writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::ZIP_MIN,
            fixture_digest(),
            7,
            &metadata,
            fixture_nonce(),
        );
        assert_eq!(writer.path(), path.as_path());
        assert_eq!(writer.compression_type(), compression::ZIP_MIN);
        assert_eq!(writer.num_ticks_to_sign(), 7);
        assert_eq!(
            writer.last_chunk_position() as usize,
            header_stream_size(16) + metadata_block.len(),
            "the header and metadata blocks precede the first chunk"
        );
        assert_eq!(writer.last_chunk_length(), 0);
        assert_eq!(writer.last_signature(), &metadata_signature[..]);
        let _ = dir;
    }

    /// Raising a tick's expected count extends the outstanding count by
    /// the same delta, and the tick reports its number.
    #[test]
    fn a_raise_extends_the_outstanding_count() {
        let mut tick_data = TickReplayData::init(9, 1);
        assert_eq!(tick_data.tick(), 9);
        tick_data.raise_expected_cells(3);
        assert_eq!(tick_data.num_cells_left(), 3, "1 outstanding + 2 more");
        tick_data.raise_expected_cells(2);
        assert_eq!(tick_data.num_cells_left(), 3, "a lower count is refused");
        tick_data.transfer_from_cell(b"a");
        tick_data.transfer_from_cell(b"b");
        tick_data.transfer_from_cell(b"c");
        assert_eq!(tick_data.num_cells_left(), 0);
    }

    /// A chunk flush leaves later buffered ticks in place, so the
    /// completing write must not close a writer that still owes tick
    /// data (`onBackgroundFileWritingComplete`'s drain test).
    #[test]
    fn a_chunk_flush_defers_the_close_while_ticks_remain_buffered() {
        let (dir, path) = temp_file("defer.replay");
        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            2,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );

        // Tick 3 completes before 1 and 2 do, so the chunk covering 1-2
        // flushes with tick 3 still buffered.
        writer.add_tick_data(1, 2, &[0xaa; 8]);
        writer.add_tick_data(2, 2, &[0xbb; 8]);
        writer.add_tick_data(3, 1, &[0xcc; 8]);
        assert_eq!(writer.num_ticks_written(), 2, "ticks 1-2 landed");
        assert_eq!(writer.last_tick_written(), 2);
        assert!(!writer.is_closed(), "tick 3 is still buffered");

        writer.add_tick_data(4, 1, &[0xdd; 8]);
        assert_eq!(writer.num_ticks_written(), 4);
        writer.close(true);

        assert!(writer.is_closed());
        let _ = dir;
    }

    /// Finalising after a releasing close is inert — the file is gone,
    /// the remaining buffered ticks are dropped without a panic
    /// (`writeFile` on a released writer).
    #[test]
    fn finalising_after_a_releasing_close_is_inert() {
        let (dir, path) = temp_file("inert.replay");
        let mut writer = ReplayDataFileWriter::new(
            &path,
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            2,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );
        writer.add_tick_data(1, 1, &[1; 8]);
        writer.close(false);
        assert!(writer.is_closed());

        let size_before = std::fs::metadata(&path).expect("metadata").len();
        writer.finalise();
        assert!(writer.is_closed());
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").len(),
            size_before,
            "nothing reached the released file"
        );
        let _ = dir;
    }

    /// A write that the device refuses mid-file — `/dev/full` — lands in
    /// the error path once and stays there (`onBackgroundFileWritingError`).
    #[cfg(unix)]
    #[test]
    fn a_refused_write_reports_one_error() {
        if !std::path::Path::new("/dev/full").exists() {
            return;
        }
        let (listener, destroyed) = CountingListener::new();
        let mut writer = ReplayDataFileWriter::new(
            std::path::Path::new("/dev/full"),
            Box::new(Md5Checksum::new()),
            compression::NONE,
            fixture_digest(),
            2,
            &ReplayMetaData::new(),
            fixture_nonce(),
        );
        writer.add_listener(Box::new(listener));
        assert!(writer.has_error(), "the first write was refused");
        assert!(writer.error_string().contains("No space left"));
        assert_eq!(writer.errors_after_construction(), 1);

        writer.add_tick_data(1, 1, &[1; 8]);
        writer.finalise();
        drop(writer);
        assert_eq!(destroyed.get(), 1);
    }

    // Small helpers the tests above lean on.
    impl ReplayDataFileWriter {
        fn has_listeners_after_drop_pending(&self) -> bool {
            !self.listeners.is_empty()
        }

        fn errors_after_construction(&self) -> u32 {
            u32::from(self.error.is_some())
        }
    }
}
