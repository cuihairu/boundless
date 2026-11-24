//! BW-07-05 the server-side replay data file writer.
//!
//! The reference is `server/baseapp/replay_data_file_writer.cpp` over the
//! shared `lib/connection` header and meta-data blocks. A recording is one
//! header chunk, one meta-data chunk, then a stream of signed tick chunks:
//!
//! ```text
//! header:    [version 4u8][digest 16][updateHertz u8][timestamp u32]
//!            [nonce 8][signatureLength u16][signature][numTicks u32]
//! meta-data: [streamSize u32][key/value pairs][signature]
//! tick chunk:[chunkLength u32][per tick: [gameTime u32][dataLength u32][blob]]
//!            [signature]
//! ```
//!
//! The signatures chain: the meta-data signature continues the scheme's state
//! from the header's signature, and each tick chunk's signature continues
//! from the previous chunk's — so a tampered prefix invalidates every later
//! chunk, not just its own range.
//!
//! `numTicks` is written unsigned inside the header but *outside* its
//! signature, and patched on finalise once the total is known
//! (`replay_header.ipp:34-41`). Finalise also chmods the file read-only
//! (`S_IRUSR | S_IRGRP | S_IROTH`), the "recording finished" marker.
//!
//! The reading side is the client seat's `crates/client::replay` (BW-11-03);
//! a server-side crate may not depend on it, so this module pins the written
//! layout with golden vectors derived from the reference reader's parse
//! rules (byte offsets and the checksumming order of `initFromStream` /
//! `readFromStream` / `readTickData`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use crate::loginapp::ClientServerProtocolVersion;

/// `ReplayHeader::NONCE_SIZE` (`replay_header.hpp:30`).
pub const NONCE_SIZE: usize = 8;

/// `BW_COMPRESSION_NONE` (`lib/network/compression_type.hpp`).
pub const COMPRESSION_NONE: u8 = 0;
/// `BW_COMPRESSION_ZIP_BEST_SPEED` (`BW_COMPRESSION_ZIP_1`).
pub const COMPRESSION_ZIP_MIN: u8 = 1;
/// `BW_COMPRESSION_ZIP_BEST_COMPRESSION` (`BW_COMPRESSION_ZIP_9`).
pub const COMPRESSION_ZIP_MAX: u8 = 9;

/// `DEFAULT_REPLAY_NUM_TICKS_TO_SIGN` (`script_bigworld.cpp:1941`): how many
/// ticks a chunk signature covers before a chunk is flushed.
pub const DEFAULT_NUM_TICKS_TO_SIGN: u32 = 10;

/// `TickReplayData::tickHeaderSize()` — `sizeof(GameTime)` plus the reserved
/// `sizeof(TickDataLength)`.
pub const TICK_HEADER_SIZE: usize = 8;

/// The read-only mode a finalised recording is chmod'ed to
/// (`replay_data_file_writer.cpp:553-560`).
pub const READ_ONLY_MODE: u32 = 0o444;

/// `GameTime` (`lib/stdmf/game_time.hpp`), a tick counter.
pub type GameTime = u32;
/// `ReplayData::SignedChunkLength`.
pub type SignedChunkLength = u32;
/// `ReplayData::TickDataLength`.
pub type TickDataLength = u32;

// ---------------------------------------------------------------------------
// Section: The checksum scheme
// ---------------------------------------------------------------------------

/// `ChecksumSchemePtr` (`lib/cstdmf/checksum_scheme.hpp`), the writing face:
/// fold bytes into a running value, then emit it as a fixed-length signature.
///
/// The reference's replay files sign with a keyed scheme (an HMAC over the
/// streamed bytes, `lib/connection/replay_checksum_scheme.cpp`); the reading
/// port registered that as a difference and models the interface, and so does
/// this side — a caller supplies its own scheme.
pub trait ReplayChecksumScheme {
    /// The signature length in bytes (`streamSize`).
    fn stream_size(&self) -> usize;
    /// `reset()`: clear the running value.
    fn reset(&mut self);
    /// `readBlob()`: fold the bytes into the running value.
    fn read_blob(&mut self, blob: &[u8]);
    /// `addToStream()`: the running value as signature bytes.
    fn sign(&mut self) -> Vec<u8>;
}

/// `XORChecksumScheme` (`lib/cstdmf/checksum_stream.cpp:117-218`): the
/// signature is the XOR of the data's little-endian `u32` words, with the
/// final partial word zero-padded on the low side.
#[derive(Debug, Clone)]
pub struct XorChecksumScheme {
    /// `value_`: the running XOR of complete words.
    value: u32,
    /// `remainder_`: the partial word carried between blobs.
    remainder: u32,
    /// `remainderLength_`: how many bytes of the partial word are filled.
    remainder_len: usize,
}

impl XorChecksumScheme {
    /// `XORChecksumScheme( initialValue )`.
    pub fn new(initial_value: u32) -> Self {
        Self {
            value: initial_value,
            remainder: 0,
            remainder_len: 0,
        }
    }

    /// `padRemainder()`: fold the trailing partial word in, zero-padded.
    fn pad_remainder(&mut self) {
        if self.remainder_len != 0 {
            self.value ^= self.remainder;
            self.remainder = 0;
            self.remainder_len = 0;
        }
    }
}

impl Default for XorChecksumScheme {
    fn default() -> Self {
        Self::new(0)
    }
}

impl ReplayChecksumScheme for XorChecksumScheme {
    fn stream_size(&self) -> usize {
        std::mem::size_of::<u32>()
    }

    fn reset(&mut self) {
        self.value = 0;
        self.remainder = 0;
        self.remainder_len = 0;
    }

    /// `readBlob()` (`checksum_stream.cpp:145-172`): bytes fold into the
    /// partial word little-endian first; every complete word is XORed in and
    /// the partial word carries over to the next blob.
    fn read_blob(&mut self, blob: &[u8]) {
        let mut word = self.remainder;
        let mut word_len = self.remainder_len;
        for &byte in blob {
            word |= u32::from(byte) << (word_len * 8);
            word_len += 1;
            if word_len == 4 {
                self.value ^= word;
                word = 0;
                word_len = 0;
            }
        }
        self.remainder = word;
        self.remainder_len = word_len;
    }

    /// `addToStream()`: pad the partial word, then the running value.
    fn sign(&mut self) -> Vec<u8> {
        self.pad_remainder();
        self.value.to_le_bytes().to_vec()
    }
}

// ---------------------------------------------------------------------------
// Section: The packed-int and string helpers
// ---------------------------------------------------------------------------

/// `BinaryOStream::writePackedInt` (`binary_stream.hpp:254-269`): values
/// under 255 are one byte, anything else is `0xFF` followed by the low three
/// bytes little-endian. (The reference writer extends from 255 while the
/// reader only recognises `0xFF`, so the pair only meets at exactly 255 —
/// reproduced here as written.)
fn write_packed_int(out: &mut Vec<u8>, value: usize) {
    if value < 0xFF {
        out.push(value as u8);
    } else {
        out.push(0xFF);
        let bytes = (value as u32).to_le_bytes();
        out.extend_from_slice(&bytes[..3]);
    }
}

/// `operator<<( BinaryOStream &, const BW::string & )`: a packed length then
/// the bytes.
fn write_packed_string(out: &mut Vec<u8>, text: &str) {
    write_packed_int(out, text.len());
    out.extend_from_slice(text.as_bytes());
}

// ---------------------------------------------------------------------------
// Section: ReplayHeader
// ---------------------------------------------------------------------------

/// `ReplayHeader` (`lib/connection/replay_header.cpp`), the writing
/// constructor's face. The header is signed over everything up to and
/// including the signature-length field; `numTicks` sits outside the
/// signature because finalise patches it once the total is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHeader {
    /// `digest_`: the defs digest (`MD5::Digest`, 16 bytes).
    pub digest: [u8; 16],
    /// `gameUpdateFrequency_`: `ServerAppConfig::updateHertz()`.
    pub game_update_frequency: u8,
    /// `timestamp_`: the file creation time, seconds after epoch.
    pub timestamp: u32,
    /// `nonce_`: `NONCE_SIZE` bytes.
    pub nonce: Vec<u8>,
    /// `numTicks_`: written as 0, patched by finalise.
    pub num_ticks: GameTime,
    /// `reportedSignatureLength_`: the scheme's stream size.
    signature_length: u16,
}

impl ReplayHeader {
    /// The writing constructor (`replay_header.cpp:46-82`). The reference
    /// stamps `time(NULL)` under `MF_SERVER`; the caller supplies the
    /// timestamp so a recording is reproducible. An empty `nonce` generates
    /// one (`initNonce`).
    pub fn new(
        scheme: &dyn ReplayChecksumScheme,
        digest: [u8; 16],
        game_update_frequency: u8,
        nonce: &[u8],
        timestamp: u32,
    ) -> Self {
        Self {
            digest,
            game_update_frequency,
            timestamp,
            nonce: Self::init_nonce(nonce),
            num_ticks: 0,
            signature_length: scheme.stream_size() as u16,
        }
    }

    /// `initNonce()` (`replay_header.cpp:105-123`): an empty nonce is
    /// generated, otherwise used as given. The reference draws the nonce from
    /// `BWOpenSSL::RAND_bytes`; the port's stand-in is a fresh process-wide
    /// counter, which keeps every recording's nonce distinct without pulling
    /// a cryptographic source into the server.
    fn init_nonce(nonce: &[u8]) -> Vec<u8> {
        if nonce.is_empty() {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let value = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return value.to_le_bytes().to_vec();
        }
        nonce.to_vec()
    }

    /// `reportedSignatureLength()`.
    pub fn signature_length(&self) -> u16 {
        self.signature_length
    }

    /// `addToStream` (`replay_header.ipp:14-42`): the header bytes and the
    /// signature. The signature covers the version through the
    /// signature-length field; `numTicks` follows outside it.
    pub fn to_bytes(&self, scheme: &mut dyn ReplayChecksumScheme) -> (Vec<u8>, Vec<u8>) {
        let mut bytes = Vec::with_capacity(Self::signature_length_field_offset() + 2);
        let version = ClientServerProtocolVersion::current();
        bytes.extend_from_slice(&[
            version.subpatch,
            version.patch,
            version.minor,
            version.major,
        ]);
        bytes.extend_from_slice(&self.digest);
        bytes.push(self.game_update_frequency);
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.signature_length.to_le_bytes());

        scheme.reset();
        scheme.read_blob(&bytes);
        let signature = scheme.sign();

        bytes.extend_from_slice(&signature);
        bytes.extend_from_slice(&self.num_ticks.to_le_bytes());
        (bytes, signature)
    }

    /// `signatureLengthFieldOffset()` (`replay_header.cpp:236-245`): where
    /// the signature-length field starts in the file.
    pub fn signature_length_field_offset() -> usize {
        ClientServerProtocolVersion::stream_size()
            + 16 // MD5::Digest
            + 1 // gameUpdateFrequency
            + 4 // timestamp
            + NONCE_SIZE
    }

    /// `numTicksFieldOffset()`: where finalise patches the tick count.
    pub fn num_ticks_field_offset(signature_length: usize) -> usize {
        Self::stream_size(signature_length) - std::mem::size_of::<GameTime>()
    }

    /// `streamSize()` / `calculateStreamSize()` (`replay_header.cpp:210-228`).
    pub fn stream_size(signature_length: usize) -> usize {
        ClientServerProtocolVersion::stream_size()
            + 16
            + 1
            + 4
            + NONCE_SIZE
            + 2 // signatureLength
            + signature_length
            + std::mem::size_of::<GameTime>() // numTicks
    }
}

// ---------------------------------------------------------------------------
// Section: ReplayMetaData
// ---------------------------------------------------------------------------

/// `ReplayMetaData` (`lib/connection/replay_metadata.cpp`): user-supplied
/// key/value pairs embedded after the header. The collection is a `BW::map`,
/// so pairs are written in key order regardless of insertion order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayMetaData {
    /// `collection_`, keyed and ordered by the key string.
    collection: BTreeMap<String, String>,
    /// `streamSize_`: the packed size of the collection.
    stream_size: u32,
}

impl ReplayMetaData {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `add()` (`replay_metadata.cpp:154-172`): a repeated key replaces its
    /// pair, backing the old pair's size out of `streamSize_` first.
    pub fn add(&mut self, key: &str, value: &str) {
        if let Some(old) = self.collection.remove(key) {
            self.stream_size -=
                (Self::packed_string_size(key) + Self::packed_string_size(&old)) as u32;
        }
        self.stream_size +=
            (Self::packed_string_size(key) + Self::packed_string_size(value)) as u32;
        self.collection.insert(key.to_string(), value.to_string());
    }

    /// The key/value pairs in the order they are written.
    pub fn entries(&self) -> Vec<(&str, &str)> {
        self.collection
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect()
    }

    /// `streamSize()`: the packed size of the collection, the field the
    /// reader bounds its collection parse by.
    pub fn stream_size(&self) -> u32 {
        self.stream_size
    }

    /// `hasKey()`.
    pub fn has_key(&self, key: &str) -> bool {
        self.collection.contains_key(key)
    }

    /// `get()`: the value for `key`, `None` when the key is absent. The
    /// reference hands the caller a supplied default instead; the caller's
    /// own fallback keeps this one allocation-free.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.collection.get(key).map(|value| value.as_str())
    }

    /// `calculateStringPackedSize()` (`replay_metadata.cpp:186-192`).
    ///
    /// The reference formula counts three bytes of length prefix for strings
    /// of 256 or more, but the packed-int prefix written for those is four
    /// bytes (`0xFF` plus three); reproduced verbatim, real meta-data keys
    /// and values are well under the boundary.
    fn packed_string_size(text: &str) -> usize {
        (if text.len() < 256 { 1 } else { 3 }) + text.len()
    }

    /// `addToStream()` (`replay_metadata.cpp:60-78`): the meta-data bytes and
    /// the signature. The signature continues the scheme's state — the writer
    /// primes it with the header's signature (`replay_data_file_writer.cpp:76-82`)
    /// — so this does not reset the scheme.
    pub fn to_bytes(&self, scheme: &mut dyn ReplayChecksumScheme) -> (Vec<u8>, Vec<u8>) {
        let mut bytes = Vec::with_capacity(4 + self.stream_size as usize);
        bytes.extend_from_slice(&self.stream_size.to_le_bytes());
        for (key, value) in &self.collection {
            write_packed_string(&mut bytes, key);
            write_packed_string(&mut bytes, value);
        }
        scheme.read_blob(&bytes);
        let signature = scheme.sign();
        (bytes, signature)
    }
}

// ---------------------------------------------------------------------------
// Section: TickReplayData
// ---------------------------------------------------------------------------

/// `TickReplayData` (`replay_data_file_writer.cpp:771-898`): one tick's cell
/// data, buffered until the expected number of cells has reported, then
/// stamped with the tick header and compression wrapper.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReplayData {
    /// `tick_`.
    tick: GameTime,
    /// `numExpectedCells_`.
    num_expected_cells: u32,
    /// `numCellsLeft_`.
    num_cells_left: u32,
    /// `pBlob_`: the accumulated cell bytes, then the finalised entry.
    blob: Vec<u8>,
}

impl TickReplayData {
    /// `init()` (`replay_data_file_writer.cpp:772-799`).
    pub fn init(&mut self, tick: GameTime, num_cells: u32) {
        self.tick = tick;
        self.num_expected_cells = num_cells;
        self.num_cells_left = num_cells;
        self.blob.clear();
    }

    /// `tick()`.
    pub fn tick(&self) -> GameTime {
        self.tick
    }

    /// `numExpectedCells()`.
    pub fn num_expected_cells(&self) -> u32 {
        self.num_expected_cells
    }

    /// `numCellsLeft()`.
    pub fn num_cells_left(&self) -> u32 {
        self.num_cells_left
    }

    /// `size()`: the entry's current size.
    pub fn size(&self) -> usize {
        self.blob.len()
    }

    /// The entry's bytes (`data()`).
    pub fn blob(&self) -> &[u8] {
        &self.blob
    }

    /// `numExpectedCells( newValue )` (`replay_data_file_writer.cpp:854-870`):
    /// a later cell may report a higher expected count. The reference
    /// asserts the count only grows and complains when it jumps by more than
    /// the one a newly-arriving cell accounts for.
    pub fn grow_expected_cells(&mut self, new_value: u32) {
        debug_assert!(new_value > self.num_expected_cells);
        let delta = new_value - self.num_expected_cells;
        self.num_cells_left += delta;
        if delta > 1 {
            eprintln!(
                "TickReplayData::numExpectedCells: {delta} cells were added, \
                 expected at most 1"
            );
        }
        self.num_expected_cells = new_value;
    }

    /// `transferFromCell()` (`replay_data_file_writer.cpp:801-818`): append
    /// one cell's bytes. A transfer after the complete set has arrived is
    /// dropped with a notice.
    pub fn transfer_from_cell(&mut self, data: &[u8]) {
        if self.num_cells_left == 0 {
            eprintln!(
                "TickReplayData::transferFromCell: Already have complete set \
                 of the cell data"
            );
            return;
        }
        self.blob.extend_from_slice(data);
        self.num_cells_left -= 1;
    }

    /// `finalise()` (`replay_data_file_writer.cpp:820-852`): stamp the tick
    /// header, wrap the buffered cell data in the compression stream, then
    /// patch the reserved length field.
    pub fn finalise(&mut self, compression_type: u8) {
        let uncompressed = std::mem::take(&mut self.blob);
        Self::stream_tick_header(&mut self.blob, self.tick);
        write_compressed_blob(&mut self.blob, &uncompressed, compression_type);
        // Patch the reserved TickDataLength at sizeof(GameTime) with the
        // wrapped payload's length.
        let length = (self.blob.len() - TICK_HEADER_SIZE) as TickDataLength;
        self.blob[std::mem::size_of::<GameTime>()..TICK_HEADER_SIZE]
            .copy_from_slice(&length.to_le_bytes());
    }

    /// `tickHeaderSize()` (`replay_data_file_writer.cpp:876-879`).
    pub fn tick_header_size() -> usize {
        TICK_HEADER_SIZE
    }

    /// `streamTickHeader()` (`replay_data_file_writer.cpp:890-898`): the game
    /// time and a zero-reserved length field.
    pub fn stream_tick_header(out: &mut Vec<u8>, tick: GameTime) {
        out.extend_from_slice(&tick.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Section: The compression wrapper
// ---------------------------------------------------------------------------

/// `CompressionOStream` + `ZipOStream` (`lib/network/compression_stream.cpp:60-113`,
/// `lib/network/zip_stream.cpp:261-286`): the compression-type byte, then —
/// for zip levels — the packed original and compressed lengths and the
/// `compress2` output. A type outside the zip range is a critical message in
/// the reference and degrades to no compression.
fn write_compressed_blob(out: &mut Vec<u8>, data: &[u8], compression_type: u8) {
    let compression_type = if compression_type == COMPRESSION_NONE {
        COMPRESSION_NONE
    } else if (COMPRESSION_ZIP_MIN..=COMPRESSION_ZIP_MAX).contains(&compression_type) {
        // BW_COMPRESSION_ZIP_1..9 map straight onto zlib levels 1..9
        // (compression_stream.cpp:95-97).
        compression_type
    } else {
        eprintln!("CompressionOStream: Invalid compression type: {compression_type}");
        COMPRESSION_NONE
    };

    out.push(compression_type);
    if compression_type == COMPRESSION_NONE {
        out.extend_from_slice(data);
        return;
    }

    use std::io::Write as _;
    let mut encoder = flate2::write::ZlibEncoder::new(
        Vec::new(),
        flate2::Compression::new(u32::from(compression_type)),
    );
    encoder.write_all(data).expect("in-memory zlib write");
    let compressed = encoder.finish().expect("in-memory zlib finish");
    write_packed_int(out, data.len());
    write_packed_int(out, compressed.len());
    out.extend_from_slice(&compressed);
}

// ---------------------------------------------------------------------------
// Section: The background file sink
// ---------------------------------------------------------------------------

/// `IBackgroundFileWriter` (`lib/cstdmf/background_file_writer.hpp:69-162`),
/// the face the writer queues its file operations through. The port runs the
/// queue synchronously; the completion callbacks stay on the writer
/// ([`ReplayFileWriter::on_background_file_writing_complete`]) so the driving
/// side keeps the reference's sequencing.
pub trait ReplayFileSink {
    /// `path()`.
    fn path(&self) -> &str;
    /// `queueWrite( data, userData )`.
    fn queue_write(&mut self, data: &[u8], user_data: i32);
    /// `queueSeek( offset, SEEK_SET, userData )`.
    fn queue_seek(&mut self, offset: u64);
    /// `queueWriteBlob( data, size, userData )`: finalise's numTicks patch.
    fn queue_write_blob(&mut self, data: &[u8]);
    /// `queueChmod( mode, userData )`.
    fn queue_chmod(&mut self, mode: u32);
    /// `setListener( pListener )`: whether this writer's callbacks are armed.
    fn set_listener(&mut self, is_set: bool);
    /// `hasError()`.
    fn has_error(&self) -> bool;
    /// `errorString()`.
    fn error_string(&self) -> String;
}

/// `ReplayDataFileWriterListener` (`replay_data_file_writer.hpp:193-216`):
/// what the app hears about a recording. The reference passes the writer
/// itself; the port passes the recording's path, the identity the app keys
/// its writers by.
pub trait ReplayDataFileWriterListener {
    /// `onReplayDataFileWritingError`: the file hit an error.
    fn on_replay_data_file_writing_error(&mut self, path: &str);
    /// `onReplayDataFileWritingComplete`: a queued write completed.
    fn on_replay_data_file_writing_complete(&mut self, path: &str);
    /// `onReplayDataFileWriterDestroyed`: the writer went away.
    fn on_replay_data_file_writer_destroyed(&mut self, path: &str);
}

// ---------------------------------------------------------------------------
// Section: ReplayDataFileWriter
// ---------------------------------------------------------------------------

/// `RecordingRecoveryData` (`server/baseapp/recording_replay_data.cpp`): the
/// state a partially-written recording resumes from after a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingRecoveryData {
    /// `compressionType()`.
    pub compression_type: u8,
    /// `numTicks()`: ticks already flushed to the file.
    pub num_ticks: u32,
    /// `lastTickWritten()`: the last tick of the flushed chunk.
    pub last_tick_written: GameTime,
    /// `nextTickPosition()`: where the next chunk starts.
    pub next_tick_position: u64,
    /// `lastSignature()`: the previous chunk's signature.
    pub last_signature: Vec<u8>,
    /// `numTicksToSign()`.
    pub num_ticks_to_sign: u32,
}

/// The tick data buffered for signing, keyed by game time (`bufferedTickData_`).
type TickData = BTreeMap<GameTime, TickReplayData>;

thread_local! {
    /// `ReplayDataFileWriter::s_activeWriters_`: the paths with a live
    /// writer. The reference keeps this set process-wide; the port keeps it
    /// per-thread, which is the single-threaded baseapp's face of it.
    static ACTIVE_WRITERS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// `ReplayDataFileWriter` (`server/baseapp/replay_data_file_writer.cpp`):
/// collects per-tick cell data, groups it into signed chunks, and finalises
/// the file's header and read-only mode when the recording ends.
pub struct ReplayFileWriter {
    /// `compressionType_`.
    compression_type: u8,
    /// `currentTick_`: the next tick expected.
    current_tick: GameTime,
    /// `lastTickPendingWrite_`: the last tick of the newest signed chunk.
    last_tick_pending_write: GameTime,
    /// `lastTickWritten_`: the last tick the sink confirmed written.
    last_tick_written: GameTime,
    /// `lastChunkPosition_`: where the newest chunk starts on the file.
    last_chunk_position: u64,
    /// `lastChunkLength_`: the newest chunk's total size.
    last_chunk_length: u64,
    /// `lastSignature_`: the newest chunk's signature.
    last_signature: Vec<u8>,
    /// `numTicksToSign_`: ticks per chunk signature.
    num_ticks_to_sign: u32,
    /// `pChecksumScheme_`.
    scheme: Box<dyn ReplayChecksumScheme>,
    /// `bufferedTickData_`.
    buffered_tick_data: TickData,
    /// `numTicksWritten_`: ticks the sink confirmed written.
    num_ticks_written: u32,
    /// `path_`.
    path: String,
    /// `isFinalising_`.
    is_finalising: bool,
    /// `pWriter_`: `None` once released by a clean `closeWriter()`. The
    /// reference keeps the writer on an error so the failure stays visible.
    sink: Option<Box<dyn ReplayFileSink>>,
    /// `listeners_`.
    listeners: Vec<Box<dyn ReplayDataFileWriterListener>>,
}

impl ReplayFileWriter {
    /// The constructor for new recordings (`replay_data_file_writer.cpp:40-92`):
    /// write the header and meta-data as the file's first chunk, priming the
    /// checksum scheme with the header's signature for the meta-data
    /// signature. The reference reads the update frequency from
    /// `ServerAppConfig::updateHertz()` and the timestamp from `time(NULL)`;
    /// the caller supplies both.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sink: Box<dyn ReplayFileSink>,
        mut scheme: Box<dyn ReplayChecksumScheme>,
        compression_type: u8,
        digest: [u8; 16],
        num_ticks_to_sign: u32,
        meta_data: &ReplayMetaData,
        nonce: &[u8],
        game_update_frequency: u8,
        timestamp: u32,
    ) -> Self {
        let header = ReplayHeader::new(
            scheme.as_ref(),
            digest,
            game_update_frequency,
            nonce,
            timestamp,
        );

        let mut file_stream = Vec::new();
        scheme.reset();
        let (header_bytes, header_signature) = header.to_bytes(scheme.as_mut());
        file_stream.extend_from_slice(&header_bytes);
        // Prime the checksum scheme with the header's signature for writing
        // out the meta-data chunk.
        scheme.reset();
        scheme.read_blob(&header_signature);
        let (meta_bytes, last_signature) = meta_data.to_bytes(scheme.as_mut());
        file_stream.extend_from_slice(&meta_bytes);

        let mut writer = Self::common_init(
            sink,
            scheme,
            compression_type,
            num_ticks_to_sign,
            last_signature,
        );
        writer.last_chunk_position = file_stream.len() as u64;
        writer.queue_write(&file_stream, 0);
        writer
    }

    /// The constructor for recovering from existing partial recordings
    /// (`replay_data_file_writer.cpp:98-133`): resume from the flushed
    /// chunk's end and re-seek the file there.
    pub fn new_recovered(
        sink: Box<dyn ReplayFileSink>,
        scheme: Box<dyn ReplayChecksumScheme>,
        recovery: &RecordingRecoveryData,
    ) -> Self {
        let next_tick_position = recovery.next_tick_position;
        let mut writer = Self::common_init(
            sink,
            scheme,
            recovery.compression_type,
            recovery.num_ticks_to_sign,
            recovery.last_signature.clone(),
        );
        writer.current_tick = recovery.last_tick_written + 1;
        writer.last_tick_pending_write = recovery.last_tick_written;
        writer.last_tick_written = recovery.last_tick_written;
        writer.last_chunk_position = next_tick_position;
        writer.num_ticks_written = recovery.num_ticks;
        writer.queue_seek(next_tick_position);
        writer
    }

    /// `commonInit()` (`replay_data_file_writer.cpp:137-151`): register the
    /// path and arm the sink's callbacks.
    fn common_init(
        mut sink: Box<dyn ReplayFileSink>,
        scheme: Box<dyn ReplayChecksumScheme>,
        compression_type: u8,
        num_ticks_to_sign: u32,
        last_signature: Vec<u8>,
    ) -> Self {
        let path = sink.path().to_string();
        ACTIVE_WRITERS.with(|writers| writers.borrow_mut().insert(path.clone()));
        sink.set_listener(true);

        Self {
            compression_type,
            current_tick: 0,
            last_tick_pending_write: 0,
            last_tick_written: 0,
            last_chunk_position: 0,
            last_chunk_length: 0,
            last_signature,
            num_ticks_to_sign,
            scheme,
            buffered_tick_data: TickData::new(),
            num_ticks_written: 0,
            path,
            is_finalising: false,
            sink: Some(sink),
            listeners: Vec::new(),
        }
    }

    /// Queue a write through the sink, if it is still held.
    fn queue_write(&mut self, data: &[u8], user_data: i32) {
        if let Some(sink) = self.sink.as_mut() {
            sink.queue_write(data, user_data);
        }
    }

    /// Queue a seek through the sink, if it is still held.
    fn queue_seek(&mut self, offset: u64) {
        if let Some(sink) = self.sink.as_mut() {
            sink.queue_seek(offset);
        }
    }

    /// `path()`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// `existsForPath()` (`replay_data_file_writer.cpp:623-637`): whether a
    /// live writer already owns the path.
    pub fn exists_for_path(path: &str) -> bool {
        ACTIVE_WRITERS.with(|writers| writers.borrow().contains(path))
    }

    /// `haveAllClosed()`: whether every live writer has closed. The
    /// reference's `closeAll` walks the same set to shut recordings down; the
    /// port's app owns its writer instances and closes them itself, so the
    /// static face stops at the query.
    pub fn have_all_closed() -> bool {
        ACTIVE_WRITERS.with(|writers| writers.borrow().is_empty())
    }

    /// `isFinalising()`.
    pub fn is_finalising(&self) -> bool {
        self.is_finalising
    }

    /// `hasError()`.
    pub fn has_error(&self) -> bool {
        self.sink.as_ref().is_some_and(|sink| sink.has_error())
    }

    /// `errorString()`.
    pub fn error_string(&self) -> String {
        match &self.sink {
            Some(sink) if sink.has_error() => sink.error_string(),
            _ => String::new(),
        }
    }

    /// `numTicksWritten()`.
    pub fn num_ticks_written(&self) -> u32 {
        self.num_ticks_written
    }

    /// `lastTickWritten()`.
    pub fn last_tick_written(&self) -> GameTime {
        self.last_tick_written
    }

    /// How many ticks are still buffered for the next chunk.
    pub fn buffered_tick_count(&self) -> usize {
        self.buffered_tick_data.len()
    }

    /// `addListener()` (`replay_data_file_writer.cpp:439-446`).
    pub fn add_listener(&mut self, listener: Box<dyn ReplayDataFileWriterListener>) {
        self.listeners.push(listener);
    }

    /// `addTickData()` (`replay_data_file_writer.cpp:179-273`): one cell's
    /// bytes for one tick. The tick completes once its expected number of
    /// cells has reported.
    pub fn add_tick_data(&mut self, tick: GameTime, num_cells: u32, blob: &[u8]) {
        if self.is_finalising {
            eprintln!(
                "ReplayDataFileWriter::addTickData: Already finalising for {}",
                self.path
            );
            return;
        }

        if tick < self.current_tick {
            // A new cell entering a space can report a tick that has already
            // been written out; its bytes are dropped.
            eprintln!(
                "ReplayDataFileWriter::addTickData: Dropped {} bytes from \
                 new cell for tick #{tick} already written out, now at \
                 #{} (for recording \"{}\")",
                blob.len(),
                self.current_tick,
                self.path
            );
            return;
        }

        if self.current_tick == 0 && self.buffered_tick_data.is_empty() {
            if blob.is_empty() {
                eprintln!(
                    "ReplayDataFileWriter::addTickData: Ignored the first \
                     tick with empty data tick #{tick} (for recording \
                     \"{}\")",
                    self.path
                );
                return;
            }
            self.current_tick = tick;
            // Pretend we have written previous ticks.
            self.last_tick_written = tick.wrapping_sub(1);
            self.last_tick_pending_write = self.last_tick_written;
        }

        let completed = {
            let tick_replay_data = self.buffered_tick_data.entry(tick).or_insert_with(|| {
                let mut data = TickReplayData::default();
                data.init(tick, num_cells);
                data
            });
            if num_cells > tick_replay_data.num_expected_cells() {
                tick_replay_data.grow_expected_cells(num_cells);
            }
            tick_replay_data.transfer_from_cell(blob);
            tick_replay_data.num_cells_left() == 0
        };
        if completed {
            self.finalise_completed_tick_data(tick);
        }
    }

    /// `finaliseCompletedTickData()` (`replay_data_file_writer.cpp:275-313`):
    /// every buffered tick up to and including the completed one is stamped
    /// — incomplete ones too, since a completed later tick proves the space
    /// has moved on — then a chunk is signed if enough ticks have
    /// accumulated.
    fn finalise_completed_tick_data(&mut self, completed_tick: GameTime) {
        while self.current_tick <= completed_tick {
            if let Some(tick_replay_data) = self.buffered_tick_data.get_mut(&self.current_tick) {
                if tick_replay_data.num_cells_left() > 0 {
                    eprintln!(
                        "ReplayDataFileWriter::finaliseCompletedTickData: \
                         Writing out incomplete tick {} before completed \
                         tick {completed_tick}",
                        self.current_tick
                    );
                }
                tick_replay_data.finalise(self.compression_type);
            }
            self.current_tick += 1;
        }
        self.sign_and_write_completed_tick_data(false);
    }

    /// `signAndWriteCompletedTickData()` (`replay_data_file_writer.cpp:314-438`):
    /// sign chunks of completed ticks until fewer than `numTicksToSign_`
    /// ticks are waiting. Each chunk's signature continues from the previous
    /// chunk's (`lastSignature_`), chaining the file. Missing ticks — every
    /// CellApp in a space dying at once — are written as empty entries so
    /// the game times stay sequential.
    ///
    /// The reference walks each window from
    /// `bufferedTickData_.find(nextTickToWrite)` and asserts the entry is
    /// there — a window that *opens* on a missing tick dereferences `end()`.
    /// The port fills leading and trailing gaps exactly like mid-window
    /// ones, so such a window writes its empty entries instead.
    fn sign_and_write_completed_tick_data(&mut self, is_last_chunk: bool) {
        if self.current_tick == 0 {
            // No data has been added yet.
            return;
        }

        let mut next_tick_to_write = self.last_tick_pending_write + 1;
        while next_tick_to_write < self.current_tick
            && (is_last_chunk || self.current_tick - next_tick_to_write >= self.num_ticks_to_sign)
        {
            debug_assert_eq!(
                self.last_signature.len(),
                self.scheme.stream_size(),
                "the previous chunk's signature is primed"
            );
            self.scheme.reset();
            self.scheme.read_blob(&self.last_signature);

            let num_ticks_to_actually_sign = self
                .num_ticks_to_sign
                .min(self.current_tick - next_tick_to_write);
            let window_end = next_tick_to_write + num_ticks_to_actually_sign;

            // First pass: the chunk length, counting missing ticks as empty
            // entries.
            let mut chunk_length: SignedChunkLength = 0;
            let mut chunk_time = next_tick_to_write;
            for (&tick, data) in self
                .buffered_tick_data
                .range(next_tick_to_write..window_end)
            {
                if chunk_time < tick {
                    chunk_length += (tick - chunk_time) as SignedChunkLength
                        * TickReplayData::tick_header_size() as SignedChunkLength;
                    chunk_time = tick;
                }
                chunk_length += data.size() as SignedChunkLength;
                chunk_time += 1;
            }
            // Buffered ticks can end before the window does; the tail is
            // empty entries, same as any other gap.
            chunk_length += (window_end - chunk_time) as SignedChunkLength
                * TickReplayData::tick_header_size() as SignedChunkLength;

            // Second pass: collect the chunk, primed with the previous
            // chunk's signature.
            let mut chunk_bytes = Vec::new();
            chunk_bytes.extend_from_slice(&chunk_length.to_le_bytes());
            let mut chunk_time = next_tick_to_write;
            for (&tick, data) in self
                .buffered_tick_data
                .range(next_tick_to_write..window_end)
            {
                while chunk_time < tick {
                    TickReplayData::stream_tick_header(&mut chunk_bytes, chunk_time);
                    chunk_time += 1;
                }
                chunk_bytes.extend_from_slice(data.blob());
                chunk_time += 1;
            }
            while chunk_time < window_end {
                TickReplayData::stream_tick_header(&mut chunk_bytes, chunk_time);
                chunk_time += 1;
            }
            self.last_tick_pending_write = chunk_time - 1;

            self.scheme.read_blob(&chunk_bytes);
            self.last_signature = self.scheme.sign();
            let signature = self.last_signature.clone();
            chunk_bytes.extend_from_slice(&signature);

            let user_arg = self.last_tick_pending_write as i32;
            self.queue_write(&chunk_bytes, user_arg);
            next_tick_to_write = self.last_tick_pending_write + 1;
        }
    }

    /// `close()` (`replay_data_file_writer.cpp:518-532`): finalise, or
    /// release the file without the closing patch.
    pub fn close(&mut self, should_finalise: bool) {
        if should_finalise {
            self.finalise();
        } else {
            self.close_writer();
        }
    }

    /// `finalise()` (`replay_data_file_writer.cpp:534-564`): flush the
    /// remaining ticks as the last chunk, patch the header's tick count, and
    /// chmod the file read-only.
    pub fn finalise(&mut self) {
        self.is_finalising = true;
        self.sign_and_write_completed_tick_data(true);

        let field_offset = ReplayHeader::num_ticks_field_offset(self.scheme.stream_size());
        let num_ticks_will_be_written =
            self.num_ticks_written + (self.last_tick_pending_write - self.last_tick_written);

        if let Some(sink) = self.sink.as_mut() {
            sink.queue_seek(field_offset as u64);
            sink.queue_write_blob(&num_ticks_will_be_written.to_le_bytes());
            sink.queue_chmod(READ_ONLY_MODE);
        }
    }

    /// `closeWriter()` (`replay_data_file_writer.cpp:566-590`): disarm the
    /// callbacks, release the sink on a clean file, and free the path. A
    /// file that errored keeps the sink so the failure stays inspectable.
    fn close_writer(&mut self) {
        self.is_finalising = true;
        if let Some(sink) = self.sink.as_mut() {
            sink.set_listener(false);
        }
        if !self.has_error() {
            self.sink = None;
        }
        ACTIVE_WRITERS.with(|writers| writers.borrow_mut().remove(&self.path));
    }

    /// `onBackgroundFileWritingError()` (`replay_data_file_writer.cpp:449-458`):
    /// free the path and tell the listeners.
    pub fn on_background_file_writing_error(&mut self) {
        ACTIVE_WRITERS.with(|writers| writers.borrow_mut().remove(&self.path));
        self.notify_listeners_of_error();
    }

    /// `onBackgroundFileWritingComplete()` (`replay_data_file_writer.cpp:460-516`):
    /// a queued write finished. `file_position` is where the write *starts* —
    /// the reference snapshots `ftell` when the request is queued
    /// (`background_file_writer.cpp:381`) — so a chunk's position plus its
    /// recomputed length is where the next chunk goes. The tick chunk carries
    /// its last signed tick as the user argument; those ticks are confirmed
    /// written and dropped from the buffer. Once finalising with nothing more
    /// to write, the writer closes.
    pub fn on_background_file_writing_complete(&mut self, file_position: u64, user_arg: i32) {
        if user_arg != 0 {
            let new_last_tick_written = user_arg as GameTime;
            let num_new_ticks_written = new_last_tick_written - self.last_tick_written;
            self.num_ticks_written += num_new_ticks_written;

            self.last_chunk_position = file_position;
            self.last_chunk_length = std::mem::size_of::<SignedChunkLength>() as u64;

            // The chunk's ticks walk by game time: a buffered entry
            // contributes its size and is dropped, a missing one was an
            // empty slot and contributes its header. (The reference's
            // `find(lastTickWritten_ + 1)` asserts the entry is there —
            // a chunk that ends on an empty slot dereferences `end()`.)
            let mut written = Vec::new();
            for tick in (self.last_tick_written + 1)..=new_last_tick_written {
                match self.buffered_tick_data.get(&tick) {
                    Some(data) => {
                        self.last_chunk_length += data.size() as u64;
                        written.push(tick);
                    }
                    None => {
                        self.last_chunk_length += TickReplayData::tick_header_size() as u64;
                    }
                }
            }
            for tick in written {
                self.buffered_tick_data.remove(&tick);
            }
            self.last_tick_written = new_last_tick_written;
            self.last_chunk_length += self.scheme.stream_size() as u64;
        }

        // Finalising ends when the buffer is empty, or the earliest buffered
        // tick is one we're still waiting for cells on.
        let have_more_tick_data_to_write = !self.buffered_tick_data.is_empty()
            && self
                .buffered_tick_data
                .keys()
                .next()
                .is_some_and(|tick| *tick < self.current_tick);

        if self.is_finalising && !have_more_tick_data_to_write {
            self.close_writer();
        }
        self.notify_listeners_of_completion();
    }

    /// `recoveryData()` (`replay_data_file_writer.cpp:734-747`): the state
    /// to resume this recording from.
    pub fn recovery_data(&self) -> RecordingRecoveryData {
        RecordingRecoveryData {
            compression_type: self.compression_type,
            num_ticks: self.num_ticks_written,
            last_tick_written: self.last_tick_written,
            next_tick_position: self.last_chunk_position + self.last_chunk_length,
            last_signature: self.last_signature.clone(),
            num_ticks_to_sign: self.num_ticks_to_sign,
        }
    }

    /// Take the released sink back for inspection. Only a cleanly closed
    /// writer (or one never given a write) can hand it over.
    pub fn take_sink(&mut self) -> Option<Box<dyn ReplayFileSink>> {
        self.sink.take()
    }

    /// `notifyListenersOfError()` (`replay_data_file_writer.cpp:685-700`).
    /// The reference iterates a copy of the listener list so a listener may
    /// add listeners while being notified; index iteration is the same face.
    fn notify_listeners_of_error(&mut self) {
        let path = self.path.clone();
        for index in 0..self.listeners.len() {
            self.listeners[index].on_replay_data_file_writing_error(&path);
        }
    }

    /// `notifyListenersOfCompletion()` (`replay_data_file_writer.cpp:702-716`).
    fn notify_listeners_of_completion(&mut self) {
        let path = self.path.clone();
        for index in 0..self.listeners.len() {
            self.listeners[index].on_replay_data_file_writing_complete(&path);
        }
    }

    /// `notifyListenersOfDestruction()` (`replay_data_file_writer.cpp:718-732`).
    fn notify_listeners_of_destruction(&mut self) {
        let path = self.path.clone();
        for index in 0..self.listeners.len() {
            self.listeners[index].on_replay_data_file_writer_destroyed(&path);
        }
    }
}

impl Drop for ReplayFileWriter {
    /// `~ReplayDataFileWriter()` (`replay_data_file_writer.cpp:153-177`).
    fn drop(&mut self) {
        if let Some(sink) = self.sink.as_mut() {
            sink.set_listener(false);
        }
        self.notify_listeners_of_destruction();
        if !self.buffered_tick_data.is_empty() {
            eprintln!(
                "ReplayDataFileWriter::~ReplayDataFileWriter: Still have {} \
                 ticks to write out.",
                self.buffered_tick_data.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Section: Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::rc::Rc;

    const DIGEST: [u8; 16] = [7; 16];
    const FREQ: u8 = 5;
    const TIMESTAMP: u32 = 0x0303_0303;
    const NONCE: &[u8] = &[9; NONCE_SIZE];

    // Acceptance faces:
    // - BW-07-05-02: the header and meta-data chunks the original reader
    //   parses (golden layouts, chained signatures).
    // - BW-07-05-03: a known event sequence reads back losslessly (chunk
    //   bytes, batching, gaps, compression round trip).
    // - BW-07-05-04: close and the callback counts match the reference.

    // -- The sink double ---------------------------------------------------

    /// One queued file operation, in queue order.
    #[derive(Debug, PartialEq, Eq)]
    enum Op {
        Write {
            offset: u64,
            data: Vec<u8>,
            user_arg: i32,
        },
        WriteBlob {
            offset: u64,
            data: Vec<u8>,
        },
        Seek {
            offset: u64,
        },
        Chmod {
            mode: u32,
        },
    }

    /// What the sink has seen. Shared through an `Rc` so the test keeps
    /// reading it after the writer has taken the sink over.
    #[derive(Default)]
    struct SinkLog {
        ops: Vec<Op>,
        position: u64,
        listener_armed: bool,
        error: Option<String>,
    }

    #[derive(Clone)]
    struct RecordingSink {
        path: String,
        log: Rc<RefCell<SinkLog>>,
    }

    impl RecordingSink {
        fn new(path: &str) -> Self {
            Self {
                path: path.to_string(),
                log: Rc::new(RefCell::new(SinkLog::default())),
            }
        }

        fn fail(&self, message: &str) {
            self.log.borrow_mut().error = Some(message.to_string());
        }

        fn log(&self) -> std::cell::RefMut<'_, SinkLog> {
            self.log.borrow_mut()
        }
    }

    impl ReplayFileSink for RecordingSink {
        fn path(&self) -> &str {
            &self.path
        }

        fn queue_write(&mut self, data: &[u8], user_data: i32) {
            let mut log = self.log.borrow_mut();
            let offset = log.position;
            log.position += data.len() as u64;
            log.ops.push(Op::Write {
                offset,
                data: data.to_vec(),
                user_arg: user_data,
            });
        }

        fn queue_seek(&mut self, offset: u64) {
            let mut log = self.log.borrow_mut();
            log.position = offset;
            log.ops.push(Op::Seek { offset });
        }

        fn queue_write_blob(&mut self, data: &[u8]) {
            let mut log = self.log.borrow_mut();
            let offset = log.position;
            log.position += data.len() as u64;
            log.ops.push(Op::WriteBlob {
                offset,
                data: data.to_vec(),
            });
        }

        fn queue_chmod(&mut self, mode: u32) {
            self.log.borrow_mut().ops.push(Op::Chmod { mode });
        }

        fn set_listener(&mut self, is_set: bool) {
            self.log.borrow_mut().listener_armed = is_set;
        }

        fn has_error(&self) -> bool {
            self.log.borrow().error.is_some()
        }

        fn error_string(&self) -> String {
            self.log.borrow().error.clone().unwrap_or_default()
        }
    }

    /// The listener double: records each event's path.
    #[derive(Default)]
    struct ListenerLog {
        errors: Vec<String>,
        completions: Vec<String>,
        destroyed: Vec<String>,
    }

    #[derive(Clone, Default)]
    struct CountingListener {
        log: Rc<RefCell<ListenerLog>>,
    }

    impl ReplayDataFileWriterListener for CountingListener {
        fn on_replay_data_file_writing_error(&mut self, path: &str) {
            self.log.borrow_mut().errors.push(path.to_string());
        }

        fn on_replay_data_file_writing_complete(&mut self, path: &str) {
            self.log.borrow_mut().completions.push(path.to_string());
        }

        fn on_replay_data_file_writer_destroyed(&mut self, path: &str) {
            self.log.borrow_mut().destroyed.push(path.to_string());
        }
    }

    // -- Fixtures ----------------------------------------------------------

    fn meta_data() -> ReplayMetaData {
        let mut meta = ReplayMetaData::new();
        meta.add("key", "value");
        meta.add("other", "pair");
        meta
    }

    fn make_writer(
        sink: RecordingSink,
        compression_type: u8,
        num_ticks_to_sign: u32,
    ) -> ReplayFileWriter {
        ReplayFileWriter::new(
            Box::new(sink),
            Box::new(XorChecksumScheme::default()),
            compression_type,
            DIGEST,
            num_ticks_to_sign,
            &meta_data(),
            NONCE,
            FREQ,
            TIMESTAMP,
        )
    }

    fn add_complete_tick(writer: &mut ReplayFileWriter, tick: GameTime, blob: &[u8]) {
        writer.add_tick_data(tick, 1, blob);
    }

    /// An independent XOR fold over a sequence of blobs, the same rules the
    /// scheme applies with its carried partial word.
    fn xor_of(blobs: &[&[u8]]) -> u32 {
        let mut value = 0u32;
        let mut word = 0u32;
        let mut word_len = 0usize;
        for blob in blobs {
            for &byte in *blob {
                word |= u32::from(byte) << (word_len * 8);
                word_len += 1;
                if word_len == 4 {
                    value ^= word;
                    word = 0;
                    word_len = 0;
                }
            }
        }
        if word_len != 0 {
            value ^= word;
        }
        value
    }

    /// The writer's first write: the header and meta-data chunks, with the
    /// meta-data signature continuing the header's.
    fn expected_header_and_meta() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut scheme = XorChecksumScheme::default();
        let header = ReplayHeader::new(&scheme, DIGEST, FREQ, NONCE, TIMESTAMP);
        let (header_bytes, header_signature) = header.to_bytes(&mut scheme);
        scheme.reset();
        scheme.read_blob(&header_signature);
        let (meta_bytes, meta_signature) = meta_data().to_bytes(&mut scheme);
        let mut bytes = header_bytes;
        bytes.extend_from_slice(&meta_bytes);
        (bytes, header_signature, meta_signature)
    }

    /// A tick entry written without compression.
    fn expected_plain_entry(tick: GameTime, payload: &[u8]) -> Vec<u8> {
        let mut entry = Vec::new();
        entry.extend_from_slice(&tick.to_le_bytes());
        entry.extend_from_slice(&((1 + payload.len()) as u32).to_le_bytes());
        entry.push(COMPRESSION_NONE);
        entry.extend_from_slice(payload);
        entry
    }

    /// A signed chunk over the given entries, chaining `previous_signature`.
    fn expected_chunk(entries: &[Vec<u8>], previous_signature: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::new();
        let length: u32 = entries.iter().map(|entry| entry.len() as u32).sum();
        chunk.extend_from_slice(&length.to_le_bytes());
        for entry in entries {
            chunk.extend_from_slice(entry);
        }
        let signature = xor_of(&[previous_signature, &chunk]).to_le_bytes();
        chunk.extend_from_slice(&signature);
        chunk
    }

    /// The nth queued write: its start offset, bytes and user argument.
    fn nth_write(log: &SinkLog, index: usize) -> (u64, Vec<u8>, i32) {
        match &log.ops[index] {
            Op::Write {
                offset,
                data,
                user_arg,
            } => (*offset, data.clone(), *user_arg),
            other => panic!("expected a queued write at {index}, got {other:?}"),
        }
    }

    /// `readPackedInt` (`binary_stream.hpp`): the value and how many bytes
    /// it consumed.
    fn read_packed_int(bytes: &[u8]) -> (usize, usize) {
        if bytes[0] == 0xFF {
            (
                (bytes[1] as usize) | ((bytes[2] as usize) << 8) | ((bytes[3] as usize) << 16),
                4,
            )
        } else {
            (bytes[0] as usize, 1)
        }
    }

    // -- The checksum scheme and the packed helpers ------------------------

    #[test]
    fn the_xor_scheme_carries_partial_words_between_blobs() {
        let mut scheme = XorChecksumScheme::new(0);
        scheme.read_blob(&[0x01, 0x02]); // a partial word
        scheme.read_blob(&[0x03, 0x04, 0x05, 0x06]); // completes, leaves 0x0605
        // Signing pads the carried word in.
        assert_eq!(scheme.sign(), 0x0403_0404u32.to_le_bytes().to_vec());
        // After a sign the next blob starts on a fresh word.
        scheme.read_blob(&[0x06]);
        assert_eq!(scheme.sign(), 0x0403_0402u32.to_le_bytes().to_vec());

        // The running value starts from the seed.
        let mut seeded = XorChecksumScheme::new(0xAAAA_AAAA);
        assert_eq!(seeded.sign(), 0xAAAA_AAAAu32.to_le_bytes().to_vec());
    }

    #[test]
    fn packed_ints_extend_from_255() {
        let mut out = Vec::new();
        write_packed_int(&mut out, 254);
        assert_eq!(out, vec![254]);
        out.clear();
        write_packed_int(&mut out, 255);
        assert_eq!(out, vec![0xFF, 0xFF, 0x00, 0x00]);
        out.clear();
        write_packed_int(&mut out, 0x010203);
        assert_eq!(out, vec![0xFF, 0x03, 0x02, 0x01]);
    }

    // -- BW-07-05-02: the header and meta-data chunks ----------------------

    #[test]
    fn header_bytes_match_the_reference_layout() {
        let mut scheme = XorChecksumScheme::default();
        let header = ReplayHeader::new(&scheme, DIGEST, FREQ, NONCE, TIMESTAMP);
        assert_eq!(header.signature_length(), 4);
        let (bytes, signature) = header.to_bytes(&mut scheme);

        // `subpatch << patch << minor << major`: 2.9.0.0 reads [0, 0, 9, 2].
        assert_eq!(&bytes[0..4], &[0, 0, 9, 2]);
        assert_eq!(&bytes[4..20], &DIGEST);
        assert_eq!(bytes[20], FREQ);
        assert_eq!(&bytes[21..25], &TIMESTAMP.to_le_bytes());
        assert_eq!(&bytes[25..33], NONCE);
        assert_eq!(&bytes[33..35], &4u16.to_le_bytes());
        assert_eq!(&bytes[35..39], &signature);
        // numTicks is written outside the signature, zero until finalise.
        assert_eq!(&bytes[39..43], &0u32.to_le_bytes());
        assert_eq!(bytes.len(), ReplayHeader::stream_size(4));
        assert_eq!(ReplayHeader::signature_length_field_offset(), 33);
        assert_eq!(ReplayHeader::num_ticks_field_offset(4), 39);

        // The signature covers the version through the signature length.
        assert_eq!(signature, xor_of(&[&bytes[0..35]]).to_le_bytes().to_vec());
    }

    #[test]
    fn an_empty_nonce_is_generated_distinctly() {
        let scheme = XorChecksumScheme::default();
        let first = ReplayHeader::new(&scheme, DIGEST, FREQ, &[], TIMESTAMP);
        let second = ReplayHeader::new(&scheme, DIGEST, FREQ, &[], TIMESTAMP);
        assert_eq!(first.nonce.len(), NONCE_SIZE);
        assert_ne!(first.nonce, second.nonce);
    }

    #[test]
    fn metadata_bytes_follow_the_header_signature() {
        let mut meta = ReplayMetaData::new();
        meta.add("zebra", "last");
        meta.add("alpha", "first");

        let mut scheme = XorChecksumScheme::default();
        let header = ReplayHeader::new(&scheme, DIGEST, FREQ, NONCE, TIMESTAMP);
        let (_, header_signature) = header.to_bytes(&mut scheme);
        scheme.reset();
        scheme.read_blob(&header_signature);
        let (bytes, signature) = meta.to_bytes(&mut scheme);

        // streamSize, then the pairs in key order.
        let mut expected = Vec::new();
        expected.extend_from_slice(&meta.stream_size().to_le_bytes());
        write_packed_string(&mut expected, "alpha");
        write_packed_string(&mut expected, "first");
        write_packed_string(&mut expected, "zebra");
        write_packed_string(&mut expected, "last");
        assert_eq!(bytes, expected);
        // Four strings of 5/5/5/4 characters.
        assert_eq!(meta.stream_size(), 23);

        // The signature continues the primed scheme: the header's signature
        // is folded in ahead of the meta-data bytes.
        assert_eq!(
            signature,
            xor_of(&[&header_signature, &bytes]).to_le_bytes().to_vec()
        );

        // Without the priming the signature would differ — the chaining is
        // what makes a tampered header invalidate the meta-data.
        let mut fresh = XorChecksumScheme::default();
        let (_, unprimed) = meta.to_bytes(&mut fresh);
        assert_ne!(signature, unprimed);
    }

    #[test]
    fn replacing_a_metadata_key_backs_its_old_size_out() {
        let mut meta = ReplayMetaData::new();
        meta.add("key", "value"); // (1+3) + (1+5)
        assert_eq!(meta.stream_size(), 10);
        meta.add("key", "other"); // the old pair is backed out first
        assert_eq!(meta.stream_size(), 10);
        assert_eq!(meta.entries(), vec![("key", "other")]);
        assert_eq!(meta.get("key"), Some("other"));
        assert!(meta.has_key("key"));
        assert!(!meta.has_key("missing"));
        assert_eq!(meta.get("missing"), None);

        // The reference's packed-size formula charges three length bytes
        // from 256 characters, reproduced verbatim (the writer emits four).
        let mut long = ReplayMetaData::new();
        long.add(&"k".repeat(256), "v");
        assert_eq!(long.stream_size(), (3 + 256 + 1 + 1) as u32);
    }

    #[test]
    fn a_new_writer_streams_header_then_metadata() {
        let path = "first-chunk.replay";
        let sink = RecordingSink::new(path);
        let writer = make_writer(sink.clone(), COMPRESSION_NONE, DEFAULT_NUM_TICKS_TO_SIGN);

        assert!(ReplayFileWriter::exists_for_path(path));
        assert!(!ReplayFileWriter::have_all_closed());
        assert_eq!(writer.path(), path);
        assert!(!writer.is_finalising());
        assert!(!writer.has_error());
        assert_eq!(writer.error_string(), "");
        assert_eq!(writer.num_ticks_written(), 0);
        assert_eq!(writer.last_tick_written(), 0);
        assert_eq!(writer.buffered_tick_count(), 0);

        let (expected_bytes, _, _) = expected_header_and_meta();
        let log = sink.log();
        assert_eq!(log.ops.len(), 1);
        let (offset, data, user_arg) = nth_write(&log, 0);
        assert_eq!(offset, 0);
        assert_eq!(user_arg, 0); // the header/meta-data write is userArg 0
        assert_eq!(data, expected_bytes);
        assert!(log.listener_armed);
    }

    #[test]
    fn closing_without_finalise_releases_the_file_quietly() {
        let path = "quiet-close.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, DEFAULT_NUM_TICKS_TO_SIGN);
        add_complete_tick(&mut writer, 1, b"x");
        let ops_before = sink.log().ops.len();

        writer.close(false);
        assert!(writer.is_finalising());
        // No numTicks patch, no chmod.
        assert_eq!(sink.log().ops.len(), ops_before);
        assert!(!sink.log().listener_armed);
        // A clean close frees the path and releases the sink.
        assert!(!ReplayFileWriter::exists_for_path(path));
        assert!(writer.take_sink().is_none());
    }

    #[test]
    fn only_a_closed_writer_frees_its_path() {
        let path = "lifecycle.replay";
        let sink = RecordingSink::new(path);
        let writer = make_writer(sink, COMPRESSION_NONE, DEFAULT_NUM_TICKS_TO_SIGN);
        assert!(ReplayFileWriter::exists_for_path(path));
        assert!(!ReplayFileWriter::have_all_closed());
        drop(writer);
        // The destructor does not free the path; close() does
        // (replay_data_file_writer.cpp:153-177).
        assert!(ReplayFileWriter::exists_for_path(path));
    }

    // -- Tick entries ------------------------------------------------------

    #[test]
    fn a_finalised_tick_entry_patches_its_length_field() {
        let mut data = TickReplayData::default();
        data.init(9, 1);
        data.transfer_from_cell(b"body");
        data.finalise(COMPRESSION_NONE);

        assert_eq!(&data.blob()[..4], &9u32.to_le_bytes());
        // The reserved length field: the compression wrapper's size.
        assert_eq!(&data.blob()[4..8], &5u32.to_le_bytes());
        assert_eq!(data.blob()[8], COMPRESSION_NONE);
        assert_eq!(&data.blob()[9..], b"body");
        assert_eq!(data.size(), TickReplayData::tick_header_size() + 5);
        assert_eq!(data.tick(), 9);
        assert_eq!(data.num_expected_cells(), 1);
        assert_eq!(data.num_cells_left(), 0);
    }

    #[test]
    fn a_transfer_after_the_full_set_is_dropped() {
        let mut data = TickReplayData::default();
        data.init(1, 1);
        data.transfer_from_cell(b"only");
        data.transfer_from_cell(b"late");
        assert_eq!(data.blob(), b"only");
    }

    #[test]
    fn a_later_cell_may_raise_the_expected_cell_count() {
        let mut data = TickReplayData::default();
        data.init(7, 1);
        data.grow_expected_cells(3); // two newly-arrived cells report too
        assert_eq!(data.num_expected_cells(), 3);
        assert_eq!(data.num_cells_left(), 3);
        data.transfer_from_cell(b"a");
        data.transfer_from_cell(b"b");
        data.transfer_from_cell(b"c");
        assert_eq!(data.blob(), b"abc");
        assert_eq!(data.num_cells_left(), 0);
    }

    // -- BW-07-05-03: the signed tick chunks -------------------------------

    #[test]
    fn a_completed_tick_is_flushed_as_a_signed_chunk() {
        let path = "one-chunk.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 1);
        let (_, _, meta_signature) = expected_header_and_meta();
        let first_write_len = ReplayHeader::stream_size(4) + 4 + meta_data().stream_size() as usize;

        add_complete_tick(&mut writer, 100, b"cell-data");

        let log = sink.log();
        assert_eq!(log.ops.len(), 2);
        let (chunk_offset, chunk_bytes, user_arg) = nth_write(&log, 1);
        assert_eq!(chunk_offset, first_write_len as u64);
        assert_eq!(user_arg, 100); // the chunk's last tick
        let expected = expected_chunk(&[expected_plain_entry(100, b"cell-data")], &meta_signature);
        assert_eq!(chunk_bytes, expected);
        drop(log);

        // The ticks stay buffered until the completion confirms them; the
        // recording's first tick pretends the ones before it were written.
        assert_eq!(writer.buffered_tick_count(), 1);
        assert_eq!(writer.num_ticks_written(), 0);
        assert_eq!(writer.last_tick_written(), 99);
    }

    #[test]
    fn cells_from_several_cellapps_accumulate_into_one_entry() {
        let path = "multi-cell.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 1);
        writer.add_tick_data(1, 3, b"aaa");
        writer.add_tick_data(1, 3, b"bbb");
        writer.add_tick_data(1, 3, b"ccc"); // the set is complete here

        let log = sink.log();
        let (_, chunk, arg) = nth_write(&log, 1);
        assert_eq!(arg, 1);
        let expected = expected_chunk(
            &[expected_plain_entry(1, b"aaabbbccc")],
            &expected_header_and_meta().2,
        );
        assert_eq!(chunk, expected);
    }

    #[test]
    fn a_missing_tick_between_buffered_ones_is_filled_mid_chunk() {
        let path = "mid-gap.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 3);
        add_complete_tick(&mut writer, 1, b"aa");
        add_complete_tick(&mut writer, 3, b"cc"); // tick 2 never reports

        // One three-tick window: entry(1), the empty slot, entry(3).
        let log = sink.log();
        assert_eq!(log.ops.len(), 2);
        let (_, chunk, arg) = nth_write(&log, 1);
        assert_eq!(arg, 3);
        let mut empty = Vec::new();
        TickReplayData::stream_tick_header(&mut empty, 2);
        let expected = expected_chunk(
            &[
                expected_plain_entry(1, b"aa"),
                empty,
                expected_plain_entry(3, b"cc"),
            ],
            &expected_header_and_meta().2,
        );
        assert_eq!(chunk, expected);
    }

    #[test]
    fn chunks_flush_every_num_ticks_to_sign() {
        let path = "batched.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 2);
        for tick in 1..=5u32 {
            add_complete_tick(&mut writer, tick, &[tick as u8; 4]);
        }

        let log = sink.log();
        // header/meta, chunk(1,2), chunk(3,4); tick 5 waits for company.
        assert_eq!(log.ops.len(), 3);
        let (offset1, first, arg) = nth_write(&log, 1);
        assert_eq!(arg, 2);
        assert_eq!(first.len(), 4 + 2 * 13 + 4);
        let (offset2, second, arg) = nth_write(&log, 2);
        assert_eq!(arg, 4);
        assert_eq!(second.len(), 4 + 2 * 13 + 4);
        drop(log);
        assert_eq!(writer.buffered_tick_count(), 5);

        // The completions confirm the flushed ticks and drop them.
        writer.on_background_file_writing_complete(offset1, 2);
        writer.on_background_file_writing_complete(offset2, 4);
        assert_eq!(writer.buffered_tick_count(), 1);
        assert_eq!(writer.num_ticks_written(), 4);
        assert_eq!(writer.last_tick_written(), 4);
    }

    #[test]
    fn missing_ticks_are_written_as_empty_entries() {
        let path = "gaps.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 1);
        add_complete_tick(&mut writer, 1, b"aaaa");
        add_complete_tick(&mut writer, 3, b"cccc");

        let log = sink.log();
        // header/meta, chunk(1), chunk(empty 2), chunk(3): each window is
        // one tick wide, so the missing tick gets its own all-empty chunk.
        assert_eq!(log.ops.len(), 4);
        let (_, first, arg) = nth_write(&log, 1);
        assert_eq!(arg, 1);
        let (_, gap, arg) = nth_write(&log, 2);
        assert_eq!(arg, 2);
        let mut empty = Vec::new();
        TickReplayData::stream_tick_header(&mut empty, 2);
        let expected_gap = expected_chunk(&[empty], &first[first.len() - 4..]);
        assert_eq!(gap, expected_gap);
        let (_, third, arg) = nth_write(&log, 3);
        assert_eq!(arg, 3);
        let expected_third = expected_chunk(
            &[expected_plain_entry(3, b"cccc")],
            &expected_gap[expected_gap.len() - 4..],
        );
        assert_eq!(third, expected_third);
        drop(log);

        // A completion spanning the empty slot still books the ticks: the
        // empty entry contributes its header to the chunk length.
        writer.on_background_file_writing_complete(0, 1);
        writer.on_background_file_writing_complete(0, 2);
        writer.on_background_file_writing_complete(0, 3);
        assert_eq!(writer.num_ticks_written(), 3);
        assert_eq!(writer.buffered_tick_count(), 0);
        assert_eq!(
            writer.recovery_data().last_signature,
            third[third.len() - 4..].to_vec()
        );
    }

    #[test]
    fn an_incomplete_tick_is_flushed_before_a_completed_later_one() {
        let path = "incomplete.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 2);
        writer.add_tick_data(1, 2, b"half"); // tick 1 waits for a second cell
        // Two more cellapps join the space mid-tick.
        writer.add_tick_data(1, 4, b"more");
        add_complete_tick(&mut writer, 2, b"done");

        // The completed later tick proves the space has moved on; tick 1
        // goes out with only the cell data it had.
        let log = sink.log();
        assert_eq!(log.ops.len(), 2);
        let (_, chunk, arg) = nth_write(&log, 1);
        assert_eq!(arg, 2);
        let expected = expected_chunk(
            &[
                expected_plain_entry(1, b"halfmore"),
                expected_plain_entry(2, b"done"),
            ],
            &expected_header_and_meta().2,
        );
        assert_eq!(chunk, expected);
    }

    #[test]
    fn a_zip_tick_chunk_round_trips_losslessly() {
        let path = "zip.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_ZIP_MAX, 1);
        // Big enough that the compressed length passes 255 and takes the
        // extended packed-int form in the chunk.
        let payload: Vec<u8> = (0..1024u32).map(|i| (i * 7 % 251) as u8).collect();

        add_complete_tick(&mut writer, 1, &payload);

        let log = sink.log();
        let (_, chunk, user_arg) = nth_write(&log, 1);
        assert_eq!(user_arg, 1);
        let entry = &chunk[4..chunk.len() - 4]; // strip length + signature

        // [tick][dataLength][type][packed original][packed compressed][zlib]
        assert_eq!(&entry[..4], &1u32.to_le_bytes());
        let data_length = u32::from_le_bytes(entry[4..8].try_into().unwrap()) as usize;
        assert_eq!(data_length, entry.len() - 8);
        assert_eq!(entry[8], COMPRESSION_ZIP_MAX);
        let (original_len, first) = read_packed_int(&entry[9..]);
        assert_eq!(original_len, payload.len());
        let (compressed_len, second) = read_packed_int(&entry[9 + first..]);

        let zlib = &entry[9 + first + second..];
        assert_eq!(zlib.len(), compressed_len);
        // The payload survives the round trip.
        let mut restored = Vec::new();
        flate2::read::ZlibDecoder::new(zlib)
            .read_to_end(&mut restored)
            .unwrap();
        assert_eq!(restored, payload);
    }

    #[test]
    fn an_invalid_compression_type_degrades_to_none() {
        let path = "bad-compression.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), 42, 1);
        add_complete_tick(&mut writer, 1, b"raw");

        let log = sink.log();
        let (_, chunk, _) = nth_write(&log, 1);
        let entry = &chunk[4..chunk.len() - 4];
        assert_eq!(entry[8], COMPRESSION_NONE);
        assert_eq!(&entry[9..], b"raw");
    }

    #[test]
    fn the_first_tick_with_empty_data_is_ignored() {
        let path = "empty-first.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 1);
        writer.add_tick_data(1, 1, b"");
        assert_eq!(writer.buffered_tick_count(), 0);

        // The recording starts at the first tick that carries data.
        add_complete_tick(&mut writer, 5, b"x");
        let log = sink.log();
        let (_, chunk, arg) = nth_write(&log, 1);
        assert_eq!(arg, 5);
        let expected = expected_chunk(
            &[expected_plain_entry(5, b"x")],
            &expected_header_and_meta().2,
        );
        assert_eq!(chunk, expected);
    }

    #[test]
    fn a_tick_already_written_out_is_dropped() {
        let path = "late-cell.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 1);
        add_complete_tick(&mut writer, 1, b"x");

        // A new cell entering the space can report an already-written tick.
        writer.add_tick_data(1, 1, b"late");

        let log = sink.log();
        assert_eq!(log.ops.len(), 2); // no extra chunk
        assert_eq!(writer.buffered_tick_count(), 1);
    }

    // -- BW-07-05-04: finalise, close and the callbacks --------------------

    #[test]
    fn finalise_patches_the_tick_count_and_chmods_read_only() {
        let path = "finalise.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 1);
        add_complete_tick(&mut writer, 10, b"x"); // chunk flushed, unconfirmed
        writer.finalise();

        assert!(writer.is_finalising());
        let log = sink.log();
        assert_eq!(log.ops.len(), 5); // header/meta, chunk, seek, patch, chmod
        assert_eq!(
            log.ops[2],
            Op::Seek {
                offset: ReplayHeader::num_ticks_field_offset(4) as u64
            }
        );
        match &log.ops[3] {
            // numTicksWritten (0) plus the chunk's unconfirmed tick (10 - 9).
            Op::WriteBlob { data, .. } => assert_eq!(data, &1u32.to_le_bytes()),
            other => panic!("expected the numTicks patch, got {other:?}"),
        }
        assert_eq!(
            log.ops[4],
            Op::Chmod {
                mode: READ_ONLY_MODE
            }
        );
    }

    #[test]
    fn a_recording_with_no_ticks_finalises_to_zero() {
        let path = "empty-finalise.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, DEFAULT_NUM_TICKS_TO_SIGN);
        writer.close(true); // close(finalise) is finalise()

        assert!(writer.is_finalising());
        let log = sink.log();
        assert_eq!(log.ops.len(), 4); // header/meta, seek, patch, chmod
        match &log.ops[2] {
            Op::WriteBlob { data, .. } => assert_eq!(data, &0u32.to_le_bytes()),
            other => panic!("expected the numTicks patch, got {other:?}"),
        }
        assert_eq!(
            log.ops[3],
            Op::Chmod {
                mode: READ_ONLY_MODE
            }
        );
    }

    #[test]
    fn adding_after_finalise_is_dropped() {
        let path = "late-add.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, DEFAULT_NUM_TICKS_TO_SIGN);
        add_complete_tick(&mut writer, 1, b"x");
        writer.finalise();
        let ops_after_finalise = sink.log().ops.len();

        writer.add_tick_data(2, 1, b"y");
        assert_eq!(sink.log().ops.len(), ops_after_finalise);
        assert_eq!(writer.buffered_tick_count(), 1);
    }

    #[test]
    fn a_chunk_completion_confirms_its_ticks() {
        let path = "confirmed.replay";
        let sink = RecordingSink::new(path);
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 2);
        add_complete_tick(&mut writer, 1, b"aa");
        add_complete_tick(&mut writer, 2, b"bb"); // chunk [1,2] flushed
        let (chunk_offset, chunk, arg) = {
            let log = sink.log();
            nth_write(&log, 1)
        };
        assert_eq!(arg, 2);

        writer.on_background_file_writing_complete(chunk_offset, arg);
        assert_eq!(writer.num_ticks_written(), 2);
        assert_eq!(writer.last_tick_written(), 2);
        assert_eq!(writer.buffered_tick_count(), 0);

        // A userArg of 0 — the header/meta-data write — moves nothing.
        writer.on_background_file_writing_complete(0, 0);
        assert_eq!(writer.num_ticks_written(), 2);

        // The recovery state points past the confirmed chunk.
        assert_eq!(
            writer.recovery_data(),
            RecordingRecoveryData {
                compression_type: COMPRESSION_NONE,
                num_ticks: 2,
                last_tick_written: 2,
                next_tick_position: chunk_offset + chunk.len() as u64,
                last_signature: chunk[chunk.len() - 4..].to_vec(),
                num_ticks_to_sign: 2,
            }
        );
    }

    #[test]
    fn a_finalised_writer_closes_once_its_last_chunk_completes() {
        let path = "closing.replay";
        let sink = RecordingSink::new(path);
        let listener = CountingListener::default();
        let listener_log = listener.log.clone();
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, 1);
        writer.add_listener(Box::new(listener));
        add_complete_tick(&mut writer, 1, b"x");
        let (chunk_offset, _, arg) = {
            let log = sink.log();
            nth_write(&log, 1)
        };

        writer.finalise();
        writer.on_background_file_writing_complete(chunk_offset, arg);

        // Nothing buffered and finalising: the writer closed itself.
        assert!(!ReplayFileWriter::exists_for_path(path));
        assert!(writer.take_sink().is_none());
        assert!(!sink.log().listener_armed);
        assert_eq!(writer.num_ticks_written(), 1);

        drop(writer);
        assert_eq!(listener_log.borrow().completions, vec![path.to_string()]);
        assert!(listener_log.borrow().errors.is_empty());
        assert_eq!(listener_log.borrow().destroyed, vec![path.to_string()]);
    }

    #[test]
    fn an_error_frees_the_path_but_keeps_the_sink() {
        let path = "errored.replay";
        let sink = RecordingSink::new(path);
        let listener = CountingListener::default();
        let listener_log = listener.log.clone();
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, DEFAULT_NUM_TICKS_TO_SIGN);
        writer.add_listener(Box::new(listener));
        sink.fail("disk full");

        assert!(writer.has_error());
        assert_eq!(writer.error_string(), "disk full");
        writer.on_background_file_writing_error();
        assert!(!ReplayFileWriter::exists_for_path(path));
        assert_eq!(listener_log.borrow().errors, vec![path.to_string()]);

        writer.close(false);
        // A failed file keeps its sink so the failure stays inspectable.
        assert!(writer.has_error());
        assert!(writer.take_sink().is_some());
    }

    #[test]
    fn a_recovered_writer_resumes_at_the_next_tick_position() {
        let path = "recovered.replay";
        let sink = RecordingSink::new(path);
        let recovery = RecordingRecoveryData {
            compression_type: COMPRESSION_NONE,
            num_ticks: 40,
            last_tick_written: 40,
            next_tick_position: 1234,
            last_signature: vec![1, 2, 3, 4],
            num_ticks_to_sign: 1,
        };
        let mut writer = ReplayFileWriter::new_recovered(
            Box::new(sink.clone()),
            Box::new(XorChecksumScheme::default()),
            &recovery,
        );
        assert_eq!(writer.path(), path);
        assert_eq!(writer.num_ticks_written(), 40);
        assert_eq!(writer.last_tick_written(), 40);
        assert_eq!(writer.recovery_data(), recovery);

        // The file is re-seeked to where the flushed chunk ended.
        assert_eq!(sink.log().ops, vec![Op::Seek { offset: 1234 }]);

        // Tick 41 chains its signature off the recovered one.
        add_complete_tick(&mut writer, 41, b"zz");
        let log = sink.log();
        assert_eq!(log.ops.len(), 2);
        let (offset, chunk, arg) = nth_write(&log, 1);
        assert_eq!(offset, 1234);
        assert_eq!(arg, 41);
        let expected = expected_chunk(&[expected_plain_entry(41, b"zz")], &[1, 2, 3, 4]);
        assert_eq!(chunk, expected);
        drop(log);

        // The chunk is unconfirmed until its completion lands.
        writer.on_background_file_writing_complete(1234, 41);
        assert_eq!(writer.num_ticks_written(), 41);
        assert_eq!(writer.recovery_data().next_tick_position, 1234 + 19);
    }

    #[test]
    fn every_listener_hears_every_event() {
        let path = "audience.replay";
        let sink = RecordingSink::new(path);
        let first = CountingListener::default();
        let second = CountingListener::default();
        let first_log = first.log.clone();
        let second_log = second.log.clone();
        let mut writer = make_writer(sink.clone(), COMPRESSION_NONE, DEFAULT_NUM_TICKS_TO_SIGN);
        writer.add_listener(Box::new(first));
        writer.add_listener(Box::new(second));

        add_complete_tick(&mut writer, 1, b"x"); // buffered: no chunk yet
        writer.on_background_file_writing_complete(0, 0); // the header write
        sink.fail("disk gone");
        writer.on_background_file_writing_error();

        // Dropping with ticks still buffered warns; the destruction still
        // reaches every listener.
        drop(writer);
        for log in [&first_log, &second_log] {
            assert_eq!(log.borrow().completions, vec![path.to_string()]);
            assert_eq!(log.borrow().errors, vec![path.to_string()]);
            assert_eq!(log.borrow().destroyed, vec![path.to_string()]);
        }
    }
}
