//! BW-07-05-02 and BW-07-05-03: write the replay header and meta-data blocks,
//! and the compressed tick chunks.
//!
//! `ReplayDataFileWriter` opens a recording by streaming the header and the
//! meta-data block ahead of every tick chunk
//! (`baseapp/replay_data_file_writer.cpp:40-87`): the header is written with
//! the current client-server protocol version and stamped with the server's
//! update hertz and the wall clock, then the meta-data block follows, and the
//! header's signature primes the checksum scheme that signs the meta-data.
//! Each later tick chunk is signed the same chained way
//! (`signAndWriteCompletedTickData`, `:314-405`).
//!
//! * `ReplayHeader::addToStream` (`connection/replay_header.ipp:13-42`) writes
//!   version, digest, `uint8(gameUpdateFrequency)`, timestamp, the 8-byte
//!   nonce blob, then `reportedSignatureLength` — all of it inside the
//!   checksummed prefix — and only afterwards the signature and, outside the
//!   signature, `uint32(numTicks)`. `numTicks` is excluded because finalise
//!   rewrites it (`finalise`, `replay_data_file_writer.cpp:534-564`).
//! * `ReplayMetaData::addToStream` (`connection/replay_metadata.cpp:52-74`)
//!   writes `streamSize` (a `uint32` of the packed key/value bytes) and the
//!   pairs themselves, appends the signature, and hands the same signature
//!   back to the caller.
//! * `ReplayHeader::streamSize` (`replay_header.cpp:210-233`) and
//!   `ReplayMetaData::checkSufficientLength` (`replay_metadata.cpp:203-217`)
//!   are what a reader uses to decide whether a fragment holds a complete
//!   block, so both are modelled here as well as
//!   `numTicksFieldOffset` (`replay_header.cpp:255-261`), which is where
//!   finalise rewrites the tick count.
//!
//! The tick half:
//! * `TickReplayData` (`baseapp/replay_data_file_writer.hpp:27-48`) accumulates
//!   one tick's cell blobs until `numExpectedCells()` of them have arrived,
//!   then `finalise` (`replay_data_file_writer.cpp:817-852`) streams the 8-byte
//!   tick header, pushes the data through a `CompressionOStream`, and patches
//!   the `TickDataLength` field.
//! * `compress_frame` is `CompressionOStream` (`compression_stream.cpp:66-100`)
//!   and `ZipOStream`'s destructor (`zip_stream.cpp:261-286`): the type byte,
//!   then for a zip type a packed uncompressed length, a packed compressed
//!   length, and the zlib payload at level `type - ZIP_BEST_SPEED +
//!   Z_BEST_SPEED`, which for `ZIP_1..ZIP_9` is the type byte itself.
//! * `ReplayRecorder::sign_and_write_completed_tick_data`
//!   (`replay_data_file_writer.cpp:314-405`) groups the completed ticks into
//!   chunks of `numTicksToSign` (all of them when `is_last_chunk`), pads a
//!   tick that never got data with an empty 8-byte header, and signs the
//!   `SignedChunkLength` field plus the payload from the previous block's
//!   signature.
//!
//! The signature scheme is the reference's `ChecksumScheme` face
//! (`cstdmf/checksum_stream.cpp:222-360`): a size, a signature over the bytes
//! read so far, and the chaining primitives `reset()`/`readBlob()` that make
//! each block's signature cover the previous one. `MD5SumScheme` (a 16-byte
//! MD5 signature, the scheme the reference unit tests use) is modelled as
//! [`Md5Signature`]; the production
//! `ReplayChecksumScheme::create()` (chained SHA-256 over an elliptic-curve
//! private key, `connection/replay_checksum_scheme.cpp`) is not modelled, and
//! takes the same trait position.

use std::collections::BTreeMap;

use md5::{Digest as _, Md5 as Md5Core};

/// `ReplayHeader::NONCE_SIZE` (`replay_header.hpp:24`).
pub const NONCE_SIZE: usize = 8;

/// `ClientServerProtocolVersion::currentVersion()`
/// (`connection/client_server_protocol_version.hpp:160-168`), as
/// `(major, minor, patch, subpatch)`.
pub const CURRENT_PROTOCOL_VERSION: (u8, u8, u8, u8) = (2, 9, 0, 0);

/// The version's stream size: four `uint8`s
/// (`client_server_protocol_version.hpp:179-184`).
const VERSION_STREAM_SIZE: usize = 4;

/// `MD5::Digest::streamSize()` (`cstdmf/md5.hpp:59`).
const DIGEST_STREAM_SIZE: usize = 16;

/// `sizeof(SignatureLength)` — a `uint16`
/// (`connection/replay_data.hpp`, `HeaderSignatureLength`).
const SIGNATURE_LENGTH_FIELD_SIZE: usize = 2;

/// `sizeof(GameTime)` — a `uint32`.
const GAME_TIME_SIZE: usize = 4;

/// `sizeof(ReplayMetaData::StreamSize)` — a `uint32`.
const STREAM_SIZE_FIELD_SIZE: usize = 4;

/// `ClientServerProtocolVersion::writeToStream`
/// (`client_server_protocol_version.hpp:135-139`): the version goes on the wire
/// as `subpatch, patch, minor, major`, so the current version is the four
/// bytes `[0, 0, 9, 2]`.
pub fn protocol_version_bytes() -> [u8; 4] {
    let (major, minor, patch, subpatch) = CURRENT_PROTOCOL_VERSION;
    [subpatch, patch, minor, major]
}

/// The reference's signature-scheme face (`ChecksumScheme`,
/// `cstdmf/checksum_stream.cpp:22-110` and `MD5SumScheme`, `:222-360`).
///
/// A scheme accumulates the bytes it has been shown — `read_blob` is the
/// scheme's `readBlob`, which is how a recording chains: the header's
/// signature is fed back in before the meta-data block is signed, and each
/// tick chunk's before the next. `reset` is the scheme's `reset`.
pub trait ReplaySignatureScheme {
    /// `ChecksumScheme::streamSize()`.
    fn signature_size(&self) -> usize;
    /// `ChecksumOStream::finalise` (`:690-705`) plus
    /// `MD5SumScheme::Impl::addToStream` (`:331-336`): the signature over
    /// everything the scheme has accumulated together with `data`.
    fn sign(&self, data: &[u8]) -> Vec<u8>;
    /// `ChecksumIStream::verify` (`:772-800`): the signature matches the
    /// accumulated bytes plus `data`.
    fn verify(&self, data: &[u8], signature: &[u8]) -> bool;
    /// `ChecksumScheme::reset` — start a fresh chain.
    fn reset(&mut self);
    /// `ChecksumScheme::readBlob` — prime the chain with the previous block's
    /// signature.
    fn read_blob(&mut self, blob: &[u8]);
}

/// `MD5SumScheme` (`checksum_stream.cpp:222-360`): a 16-byte MD5 signature
/// over the bytes read so far. The reference's `Impl` keeps an `MD5 md5_`,
/// appends to it in `readBlob` and finalises it in `addToStream`.
#[derive(Debug, Clone, Default)]
pub struct Md5Signature {
    /// The bytes accumulated by `read_blob` since the last `reset`.
    accumulated: Vec<u8>,
}

impl Md5Signature {
    /// `MD5SumScheme::create()` (`checksum_stream.cpp:251-254`).
    pub fn new() -> Self {
        Self::default()
    }
}

impl ReplaySignatureScheme for Md5Signature {
    fn signature_size(&self) -> usize {
        DIGEST_STREAM_SIZE
    }

    fn sign(&self, data: &[u8]) -> Vec<u8> {
        let mut hasher = Md5Core::new();
        hasher.update(&self.accumulated);
        hasher.update(data);
        hasher.finalize().to_vec()
    }

    fn verify(&self, data: &[u8], signature: &[u8]) -> bool {
        self.sign(data) == signature
    }

    fn reset(&mut self) {
        self.accumulated.clear();
    }

    fn read_blob(&mut self, blob: &[u8]) {
        self.accumulated.extend_from_slice(blob);
    }
}

/// `ReplayHeader` (`connection/replay_header.hpp:21`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHeader {
    /// The four streamed version bytes, in wire order
    /// (see [`protocol_version_bytes`]).
    pub version: [u8; 4],
    /// The definition digest, 16 raw MD5 bytes.
    pub digest: [u8; 16],
    /// `gameUpdateFrequency_`, the server's update hertz.
    pub game_update_frequency: u8,
    /// `timestamp_`, seconds after the epoch
    /// (`replay_header.cpp:66-70`, `time(NULL)`).
    pub timestamp: u32,
    /// `nonce_`, always `NONCE_SIZE` bytes.
    pub nonce: [u8; NONCE_SIZE],
    /// `reportedSignatureLength_`, the scheme's stream size.
    pub signature_length: u16,
    /// `numTicks_`, rewritten by finalise.
    pub num_ticks: u32,
}

impl ReplayHeader {
    /// The writing constructor (`replay_header.cpp:48-73`): the current
    /// protocol version, the digest, the update hertz and the nonce, with the
    /// signature length taken from the scheme.
    pub fn new(
        scheme: &dyn ReplaySignatureScheme,
        digest: [u8; 16],
        game_update_frequency: u8,
        timestamp: u32,
        nonce: [u8; NONCE_SIZE],
    ) -> Self {
        Self {
            version: protocol_version_bytes(),
            digest,
            game_update_frequency,
            timestamp,
            nonce,
            signature_length: scheme.signature_size() as u16,
            num_ticks: 0,
        }
    }

    /// `ReplayHeader::signatureLengthFieldOffset`
    /// (`replay_header.cpp:239-246`).
    pub fn signature_length_field_offset() -> usize {
        VERSION_STREAM_SIZE
            + DIGEST_STREAM_SIZE
            + 1 /* gameUpdateFrequency */
            + GAME_TIME_SIZE /* timestamp */
            + NONCE_SIZE
    }

    /// `ReplayHeader::streamSize` (`replay_header.cpp:210-220`) /
    /// `calculateStreamSize` (`:223-233`): the whole block including the
    /// trailing `numTicks`.
    pub fn stream_size(signature_size: usize) -> usize {
        Self::signature_length_field_offset()
            + SIGNATURE_LENGTH_FIELD_SIZE
            + signature_size
            + GAME_TIME_SIZE
    }

    /// `ReplayHeader::numTicksFieldOffset` (`replay_header.cpp:255-261`):
    /// where finalise rewrites the tick count.
    pub fn num_ticks_field_offset(signature_size: usize) -> usize {
        Self::stream_size(signature_size) - GAME_TIME_SIZE
    }

    /// `ReplayHeader::checkSufficientLength` (`replay_header.cpp:276-297`):
    /// the expected total length once the `signatureLength` field is readable,
    /// or `None` when the fragment is too short to hold it.
    pub fn expected_length(data: &[u8]) -> Option<usize> {
        let offset = Self::signature_length_field_offset();
        if data.len() < offset + SIGNATURE_LENGTH_FIELD_SIZE {
            return None;
        }
        let signature_length = u16::from_le_bytes([data[offset], data[offset + 1]]);
        Some(offset + SIGNATURE_LENGTH_FIELD_SIZE + signature_length as usize + GAME_TIME_SIZE)
    }

    /// Whether `data` holds a complete header (the reference returns `true`
    /// from `checkSufficientLength`).
    pub fn is_complete(data: &[u8]) -> bool {
        Self::expected_length(data).is_some_and(|expected| data.len() >= expected)
    }

    /// `ReplayHeader::addToStream` (`replay_header.ipp:13-42`): the signed
    /// block followed by `numTicks`, which sits outside the signature.
    pub fn to_bytes(&self, scheme: &dyn ReplaySignatureScheme) -> Vec<u8> {
        assert_eq!(
            self.signature_length as usize,
            scheme.signature_size(),
            "the reported signature length must match the scheme"
        );

        // The checksummed prefix: everything up to and including the
        // signatureLength field.
        let mut prefix =
            Vec::with_capacity(Self::signature_length_field_offset() + SIGNATURE_LENGTH_FIELD_SIZE);
        prefix.extend_from_slice(&self.version);
        prefix.extend_from_slice(&self.digest);
        prefix.push(self.game_update_frequency);
        prefix.extend_from_slice(&self.timestamp.to_le_bytes());
        prefix.extend_from_slice(&self.nonce);
        prefix.extend_from_slice(&self.signature_length.to_le_bytes());

        let signature = scheme.sign(&prefix);
        let mut out = prefix;
        out.extend_from_slice(&signature);

        // Outside the signature: it is rewritten when the file is finalised.
        out.extend_from_slice(&self.num_ticks.to_le_bytes());
        out
    }
}

/// `ReplayMetaData::calculateStringPackedSize`
/// (`replay_metadata.cpp:248-253`): a string of fewer than 256 bytes costs
/// one length byte, a longer one three.
fn packed_string_size(s: &str) -> u32 {
    let n = s.len();
    let prefix = if n < 256 { 1 } else { 3 };
    prefix + n as u32
}

/// `BinaryOStream::appendString` (`cstdmf/binary_stream.hpp:390-397`) over
/// `writePackedInt` (`:254-268`): one length byte below 255, otherwise
/// `0xFF` plus the three low little-endian bytes of the value.
fn append_packed_string(out: &mut Vec<u8>, s: &str) {
    let n = s.len();
    if n >= 255 {
        debug_assert!(n < (1 << 24), "a packed length holds 24 bits");
        out.push(0xFF);
        out.extend_from_slice(&(n as u32).to_le_bytes()[..3]);
    } else {
        out.push(n as u8);
    }
    out.extend_from_slice(s.as_bytes());
}

/// `ReplayMetaData` (`connection/replay_metadata.hpp:19`).
///
/// The reference keeps the pairs in a `BW::map` and streams them in map order,
/// so a [`BTreeMap`] is the same collection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayMetaData {
    /// `collection_`.
    entries: BTreeMap<String, String>,
    /// `streamSize_`: the packed size of every pair, tracked as pairs are
    /// added and replaced.
    stream_size: u32,
}

impl ReplayMetaData {
    /// `ReplayMetaData::init(ChecksumSchemePtr)` /
    /// `init(size_t)` (`replay_metadata.cpp:19-44`): both forms clear the
    /// collection and the tracked size, and differ only in which signature
    /// length a reader of the block expects — which the writer does not
    /// record. A writer therefore starts from an empty block.
    pub fn new() -> Self {
        Self::default()
    }

    /// `ReplayMetaData::add` (`replay_metadata.cpp:147-166`): a repeated key
    /// replaces the value and the tracked size rather than adding a pair.
    pub fn add(&mut self, key: &str, value: &str) {
        if let Some(old) = self.entries.get(key) {
            self.stream_size -= packed_string_size(key) + packed_string_size(old);
        }
        self.stream_size += packed_string_size(key) + packed_string_size(value);
        self.entries.insert(key.to_string(), value.to_string());
    }

    /// `ReplayMetaData::hasKey` (`replay_metadata.cpp:171-174`).
    pub fn has_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// `ReplayMetaData::get` (`replay_metadata.cpp:181-190`).
    pub fn get<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.entries.get(key).map_or(default, |v| v.as_str())
    }

    /// `ReplayMetaData::clear` (`replay_metadata.hpp:45-49`).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.stream_size = 0;
    }

    /// `ReplayMetaData::swap` (`replay_metadata.cpp:260-266`).
    pub fn swap(&mut self, other: &mut Self) {
        std::mem::swap(&mut self.entries, &mut other.entries);
        std::mem::swap(&mut self.stream_size, &mut other.stream_size);
    }

    /// `ReplayMetaData::size` (`replay_metadata.hpp:42`).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `ReplayMetaData::size` (`replay_metadata.hpp:42`), for emptiness.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `ReplayMetaData::begin`/`end` (`replay_metadata.hpp:57-58`): the pairs
    /// in the order they are streamed.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// `ReplayMetaData::streamSize` (`replay_metadata.hpp:62`).
    pub fn stream_size(&self) -> u32 {
        self.stream_size
    }

    /// `ReplayMetaData::checkSufficientLength`
    /// (`replay_metadata.cpp:203-217`): the block length once the `streamSize`
    /// field is readable, or `None` when the fragment is too short.
    pub fn expected_length(data: &[u8], signature_length: usize) -> Option<usize> {
        if data.len() < STREAM_SIZE_FIELD_SIZE {
            return None;
        }
        let stream_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        Some(STREAM_SIZE_FIELD_SIZE + stream_size + signature_length)
    }

    /// Whether `data` holds a complete meta-data block.
    pub fn is_complete(data: &[u8], signature_length: usize) -> bool {
        Self::expected_length(data, signature_length).is_some_and(|expected| data.len() >= expected)
    }

    /// `ReplayMetaData::addToStream` (`replay_metadata.cpp:52-74`): the
    /// `streamSize` field, the packed pairs, and the signature appended to the
    /// block itself.
    pub fn to_bytes(&self, scheme: &dyn ReplaySignatureScheme) -> Vec<u8> {
        // The tracked size is not asserted against the body here on purpose:
        // for a string of 255 bytes or more the reference's size rule counts
        // fewer length bytes than `writePackedInt` writes (see
        // `packed_string_size`), so the two legitimately differ there.
        let mut body = Vec::with_capacity(self.stream_size as usize);
        for (key, value) in &self.entries {
            append_packed_string(&mut body, key);
            append_packed_string(&mut body, value);
        }

        let mut prefix = Vec::with_capacity(STREAM_SIZE_FIELD_SIZE + body.len());
        prefix.extend_from_slice(&self.stream_size.to_le_bytes());
        prefix.extend_from_slice(&body);

        let signature = scheme.sign(&prefix);
        let mut out = prefix;
        out.extend_from_slice(&signature);
        out
    }
}

// ----------------------------------------------------------------------------
// Section: compression (BW-07-05-03)
// ----------------------------------------------------------------------------

/// `BW_COMPRESSION_NONE` (`compression_type.hpp:10`).
pub const COMPRESSION_NONE: u8 = 0;
/// `BW_COMPRESSION_ZIP_1..ZIP_9` (`compression_type.hpp:12-20`).
pub const COMPRESSION_ZIP_MIN: u8 = 1;
/// `BW_COMPRESSION_ZIP_9` (`compression_type.hpp:20`).
pub const COMPRESSION_ZIP_MAX: u8 = 9;
/// `BW_COMPRESSION_ZIP_BEST_SPEED` = `BW_COMPRESSION_ZIP_1`
/// (`compression_type.hpp:22`).
pub const COMPRESSION_ZIP_BEST_SPEED: u8 = COMPRESSION_ZIP_MIN;
/// `BW_COMPRESSION_ZIP_BEST_COMPRESSION` = `BW_COMPRESSION_ZIP_9`
/// (`compression_type.hpp:23`).
pub const COMPRESSION_ZIP_BEST_COMPRESSION: u8 = COMPRESSION_ZIP_MAX;
/// `BW_COMPRESSION_DEFAULT_INTERNAL` (`compression_type.hpp:25`).
pub const COMPRESSION_DEFAULT_INTERNAL: u8 = 10;
/// `BW_COMPRESSION_DEFAULT_EXTERNAL` (`compression_type.hpp:26`).
pub const COMPRESSION_DEFAULT_EXTERNAL: u8 = 11;

/// A `CompressionOStream` frame the writer cannot produce
/// (`compression_stream.cpp:91-97`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressError {
    /// "Invalid compression type: %d".
    InvalidCompressionType(u8),
    /// The zlib encoder failed, which `compress2` reports as a non-`Z_OK`
    /// result (`zip_stream.cpp:275-279`).
    Zlib,
}

/// `CompressionOStream::s_defaultInternalCompression` /
/// `s_defaultExternalCompression` (`compression_stream.cpp:53-58`): both
/// defaults start at `BW_COMPRESSION_NONE`, and the recording writer never
/// reconfigures them, so the two DEFAULT sentinels resolve to `NONE`.
fn resolve_compression_type(compression_type: u8) -> u8 {
    if compression_type == COMPRESSION_DEFAULT_INTERNAL
        || compression_type == COMPRESSION_DEFAULT_EXTERNAL
    {
        COMPRESSION_NONE
    } else {
        compression_type
    }
}

/// `BinaryOStream::writePackedInt` (`binary_stream.hpp:254-268`): the packed
/// form of a length.
fn append_packed_length(out: &mut Vec<u8>, value: usize) {
    debug_assert!(value < (1 << 24), "a packed length holds 24 bits");
    if value >= 255 {
        out.push(0xFF);
        out.extend_from_slice(&(value as u32).to_le_bytes()[..3]);
    } else {
        out.push(value as u8);
    }
}

/// `CompressionOStream` (`compression_stream.cpp:66-100`) plus
/// `ZipOStream`'s destructor (`zip_stream.cpp:261-286`): the type byte first;
/// for a zip type a packed uncompressed length, a packed compressed length,
/// then the zlib payload compressed at level
/// `(type - ZIP_BEST_SPEED) + Z_BEST_SPEED`, which for `ZIP_1..ZIP_9` is the
/// type byte itself (`compression_stream.cpp:87-89`).
pub fn compress_frame(compression_type: u8, data: &[u8]) -> Result<Vec<u8>, CompressError> {
    let compression_type = resolve_compression_type(compression_type);

    if compression_type == COMPRESSION_NONE {
        let mut out = Vec::with_capacity(data.len() + 1);
        out.push(COMPRESSION_NONE);
        out.extend_from_slice(data);
        return Ok(out);
    }

    if !(COMPRESSION_ZIP_MIN..=COMPRESSION_ZIP_MAX).contains(&compression_type) {
        return Err(CompressError::InvalidCompressionType(compression_type));
    }

    use std::io::Write as _;
    let mut encoder = flate2::write::ZlibEncoder::new(
        Vec::new(),
        flate2::Compression::new(compression_type as u32),
    );
    encoder.write_all(data).map_err(|_| CompressError::Zlib)?;
    let compressed = encoder.finish().map_err(|_| CompressError::Zlib)?;

    let mut out = Vec::with_capacity(compressed.len() + 10);
    out.push(compression_type);
    append_packed_length(&mut out, data.len());
    append_packed_length(&mut out, compressed.len());
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Inflate a `ZipOStream` frame the way `ZipIStream` does
/// (`zip_stream.cpp:124-184`), as the tests' independent read-back probe: a
/// packed uncompressed length, a packed compressed length, then the zlib
/// payload.
#[cfg(test)]
fn decompress_frame(frame: &[u8]) -> Result<Vec<u8>, CompressError> {
    let (&compression_type, rest) = frame
        .split_first()
        .ok_or(CompressError::InvalidCompressionType(0))?;
    if compression_type == COMPRESSION_NONE {
        return Ok(rest.to_vec());
    }
    if !(COMPRESSION_ZIP_MIN..=COMPRESSION_ZIP_MAX).contains(&compression_type) {
        return Err(CompressError::InvalidCompressionType(compression_type));
    }

    let read_length = |bytes: &[u8]| -> Option<(usize, usize)> {
        let first = *bytes.first()?;
        if first == 0xFF {
            let rest = bytes.get(1..4)?;
            Some((
                u32::from_le_bytes([rest[0], rest[1], rest[2], 0]) as usize,
                4,
            ))
        } else {
            Some((first as usize, 1))
        }
    };

    let (src_len, used) = read_length(rest).ok_or(CompressError::Zlib)?;
    let rest = &rest[used..];
    let (compressed_len, used) = read_length(rest).ok_or(CompressError::Zlib)?;
    let compressed = rest
        .get(used..used + compressed_len)
        .ok_or(CompressError::Zlib)?;

    use std::io::Read as _;
    let mut out = Vec::with_capacity(src_len);
    flate2::read::ZlibDecoder::new(compressed)
        .read_to_end(&mut out)
        .map_err(|_| CompressError::Zlib)?;
    if out.len() > src_len {
        // `uncompress` would have overflowed its srcLen-sized buffer.
        return Err(CompressError::Zlib);
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// Section: tick data (BW-07-05-03)
// ----------------------------------------------------------------------------

/// `TickReplayData` (`baseapp/replay_data_file_writer.hpp:27-48`): one tick's
/// accumulated cell blobs, and its final on-wire form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReplayData {
    /// `tick_`.
    tick: u32,
    /// `numExpectedCells_`.
    num_expected_cells: u32,
    /// `numCellsLeft_`.
    num_cells_left: u32,
    /// `pBlob_`: the accumulated cell data, then the finalised block.
    blob: Vec<u8>,
}

impl TickReplayData {
    /// `TickReplayData::tickHeaderSize` (`replay_data_file_writer.cpp:884-888`):
    /// `sizeof(GameTime) + sizeof(TickDataLength)`.
    pub const TICK_HEADER_SIZE: usize = GAME_TIME_SIZE + GAME_TIME_SIZE;

    /// `TickReplayData::init` (`replay_data_file_writer.cpp:772-780`).
    pub fn init(tick: u32, num_cells: u32) -> Self {
        Self {
            tick,
            num_expected_cells: num_cells,
            num_cells_left: num_cells,
            blob: Vec::new(),
        }
    }

    /// `TickReplayData::transferFromCell` (`:793-810`): appends a cell's blob;
    /// a call once the set is already complete drops the data.
    pub fn transfer_from_cell(&mut self, data: &[u8]) {
        if self.num_cells_left == 0 {
            // NOTICE: "Already have complete set of the cell data".
            return;
        }
        self.blob.extend_from_slice(data);
        self.num_cells_left -= 1;
    }

    /// `TickReplayData::numExpectedCells` (`:854-877`): the count only grows
    /// (`addTickData` calls it when a cell reports a larger space, `:252-256`).
    pub fn set_num_expected_cells(&mut self, new_value: u32) {
        debug_assert!(new_value > self.num_expected_cells);
        self.num_cells_left += new_value - self.num_expected_cells;
        self.num_expected_cells = new_value;
    }

    /// `TickReplayData::streamTickHeader` (`:890-899`): the game time and a
    /// zero `TickDataLength` placeholder.
    pub fn stream_tick_header(out: &mut Vec<u8>, tick: u32) {
        out.extend_from_slice(&tick.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }

    /// `TickReplayData::finalise` (`:817-852`): the tick header followed by
    /// the cell data through a `CompressionOStream`, then the
    /// `TickDataLength` field patched to the frame size.
    pub fn finalise(&mut self, compression_type: u8) -> Result<(), CompressError> {
        let uncompressed = std::mem::take(&mut self.blob);

        let mut blob = Vec::new();
        Self::stream_tick_header(&mut blob, self.tick);
        blob.extend_from_slice(&compress_frame(compression_type, &uncompressed)?);

        // "Finalise the tick data length header field" (`:842-846`): the
        // length covers the frame but not its own 8-byte header.
        let length = (blob.len() - Self::TICK_HEADER_SIZE) as u32;
        blob[GAME_TIME_SIZE..Self::TICK_HEADER_SIZE].copy_from_slice(&length.to_le_bytes());

        self.blob = blob;
        Ok(())
    }

    /// `TickReplayData::tick` (`:40`).
    pub fn tick(&self) -> u32 {
        self.tick
    }

    /// `TickReplayData::numExpectedCells` (`:41`).
    pub fn num_expected_cells(&self) -> u32 {
        self.num_expected_cells
    }

    /// `TickReplayData::numCellsLeft` (`:43`).
    pub fn num_cells_left(&self) -> u32 {
        self.num_cells_left
    }

    /// `TickReplayData::data` (`:44`).
    pub fn data(&self) -> &[u8] {
        &self.blob
    }

    /// `TickReplayData::size` (`:45`).
    pub fn size(&self) -> usize {
        self.blob.len()
    }
}

// ----------------------------------------------------------------------------
// Section: the recording writer's tick half (BW-07-05-03)
// ----------------------------------------------------------------------------

/// One signed chunk queued for the file: the on-disk bytes
/// `[SignedChunkLength][payload][signature]` (`ReplayData::SignedChunkLength`,
/// `replay_data.hpp:8`; `signAndWriteCompletedTickData`,
/// `replay_data_file_writer.cpp:314-405`) and the last tick it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedChunk {
    /// The on-disk bytes of the chunk.
    pub bytes: Vec<u8>,
    /// The last tick in the chunk (`lastTickPendingWrite_`, the queue write's
    /// `userArg`, `:403`).
    pub last_tick: u32,
    /// The payload length, the `SignedChunkLength` field's value.
    pub chunk_length: usize,
}

/// What [`ReplayRecorder::add_tick_data`] did with the blob
/// (`addTickData`, `replay_data_file_writer.cpp:179-271`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddTickDataOutcome {
    /// The blob joined its tick's buffer.
    Accepted,
    /// The tick was already written out (`tick < currentTick_`, `:192-210`).
    DroppedAlreadyWritten,
    /// The first tick arrived with no data and was ignored (`:206-216`).
    IgnoredEmptyFirstTick,
}

/// The tick-writing half of `ReplayDataFileWriter`
/// (`baseapp/replay_data_file_writer.hpp:66-105`): buffers per-tick cell data,
/// finalises complete ticks into compressed blobs, and signs chunks of
/// `numTicksToSign` ticks, each signature chaining from the previous block.
///
/// The file half — the background writer, the listener callbacks, finalise's
/// `numTicks` rewrite — is BW-07-05-04.
pub struct ReplayRecorder {
    /// `compressionType_`.
    compression_type: u8,
    /// `numTicksToSign_`.
    num_ticks_to_sign: usize,
    /// `currentTick_`: the next tick expected.
    current_tick: u32,
    /// `lastTickPendingWrite_`: the last tick in the newest signed chunk.
    last_tick_pending_write: u32,
    /// `lastSignature_`: the signature the next chunk primes from — the
    /// metadata block's signature after construction
    /// (`replay_data_file_writer.cpp:77-86`).
    last_signature: Vec<u8>,
    /// `bufferedTickData_` (`BW::map< GameTime, TickReplayData >`).
    buffered: BTreeMap<u32, TickReplayData>,
    /// The chunks produced by [`Self::sign_and_write_completed_tick_data`]
    /// and waiting for the file writer (`pWriter_->queueWrite`, `:398-403`).
    queued_chunks: Vec<SignedChunk>,
    /// `pChecksumScheme_`.
    scheme: Box<dyn ReplaySignatureScheme>,
}

impl ReplayRecorder {
    /// The writing constructor's tick half
    /// (`replay_data_file_writer.cpp:40-87`).
    pub fn new(
        scheme: Box<dyn ReplaySignatureScheme>,
        compression_type: u8,
        num_ticks_to_sign: usize,
    ) -> Self {
        Self {
            scheme,
            compression_type,
            num_ticks_to_sign,
            current_tick: 0,
            last_tick_pending_write: 0,
            last_signature: Vec::new(),
            buffered: BTreeMap::new(),
            queued_chunks: Vec::new(),
        }
    }

    /// Prime the chain from the metadata block's signature, as the writing
    /// constructor does after streaming the header and the meta-data
    /// (`replay_data_file_writer.cpp:69-86`).
    pub fn prime_signature(&mut self, signature: &[u8]) {
        self.scheme.reset();
        self.scheme.read_blob(signature);
        self.last_signature = signature.to_vec();
    }

    /// `ReplayDataFileWriter::addTickData`
    /// (`replay_data_file_writer.cpp:179-271`).
    pub fn add_tick_data(&mut self, tick: u32, num_cells: u32, blob: &[u8]) -> AddTickDataOutcome {
        if tick < self.current_tick {
            // A new cell entering a space can report one extra tick's count;
            // the reference drops the data (TODO in the original, `:192-210`).
            return AddTickDataOutcome::DroppedAlreadyWritten;
        }

        if self.current_tick == 0 && self.buffered.is_empty() {
            if blob.is_empty() {
                return AddTickDataOutcome::IgnoredEmptyFirstTick;
            }
            self.current_tick = tick;
            // Pretend we have written previous ticks (`:219`). The reference's
            // unsigned `tick - 1` wraps for a recording that starts at tick 0.
            self.last_tick_pending_write = tick.wrapping_sub(1);
        }

        let entry = self
            .buffered
            .entry(tick)
            .or_insert_with(|| TickReplayData::init(tick, num_cells));

        if num_cells > entry.num_expected_cells() {
            entry.set_num_expected_cells(num_cells);
        }

        // A redundant report lands on `transferFromCell`'s own guard —
        // "Already have complete set of the cell data" (`:793-800`).
        entry.transfer_from_cell(blob);
        if entry.num_cells_left() == 0 {
            self.finalise_completed_tick_data(tick);
            return AddTickDataOutcome::Accepted;
        }
        AddTickDataOutcome::Accepted
    }

    /// `ReplayDataFileWriter::finaliseCompletedTickData`
    /// (`replay_data_file_writer.cpp:275-311`): finalise every buffered tick
    /// up to `completed_tick` — including incomplete ones — then sign and
    /// write any chunks the new completion unblocked.
    pub fn finalise_completed_tick_data(&mut self, completed_tick: u32) {
        while self.current_tick <= completed_tick {
            if let Some(entry) = self.buffered.get_mut(&self.current_tick) {
                // An incomplete tick still in the map is written out as-is
                // ("Writing out incomplete tick %u", `:296-303`).
                let _ = entry.finalise(self.compression_type);
            }
            self.current_tick += 1;
        }

        self.sign_and_write_completed_tick_data(false);
    }

    /// `ReplayDataFileWriter::signAndWriteCompletedTickData`
    /// (`replay_data_file_writer.cpp:314-405`): groups the completed ticks
    /// into chunks of `numTicksToSign` (all of them when `is_last_chunk`),
    /// and signs each from the previous chunk's signature. The chunks are
    /// queued for the file writer, in order — take them with
    /// [`Self::take_written_chunks`], which is the consumer side of
    /// `pWriter_->queueWrite` (`:398-403`).
    ///
    /// Like the reference, the written ticks stay in the buffer: they are
    /// erased only once the background file write completes
    /// (`onBackgroundFileWritingComplete`, `:478-489` — BW-07-05-04).
    pub fn sign_and_write_completed_tick_data(&mut self, is_last_chunk: bool) {
        if self.current_tick == 0 {
            // No data has been added yet (`:317-321`).
            return;
        }
        debug_assert!(self.current_tick > self.last_tick_pending_write);

        // The reference's unsigned `+1` wraps a 0xffffffff last written tick
        // back to tick 0 (`:324`).
        let mut next_tick_to_write = self.last_tick_pending_write.wrapping_add(1);
        while (next_tick_to_write < self.current_tick)
            && (is_last_chunk
                || (self.current_tick - next_tick_to_write) >= self.num_ticks_to_sign as u32)
        {
            // Prime from the previous block's signature (`:325-330`).
            let last_signature = std::mem::take(&mut self.last_signature);
            self.scheme.reset();
            self.scheme.read_blob(&last_signature);

            let num_ticks_to_sign = std::cmp::min(
                self.num_ticks_to_sign as u32,
                self.current_tick - next_tick_to_write,
            );

            // First pass: the chunk length, counting `tickHeaderSize` bytes
            // for every tick with missing data (`:344-374`).
            let range_end = next_tick_to_write + num_ticks_to_sign;
            let mut chunk_length = 0usize;
            for chunk_time in next_tick_to_write..range_end {
                chunk_length += match self.buffered.get(&chunk_time) {
                    Some(entry) => entry.size(),
                    None => TickReplayData::TICK_HEADER_SIZE,
                };
            }

            // Second pass: the signed region — the length field and the tick
            // data (`:380-401`).
            let mut region = Vec::with_capacity(chunk_length + GAME_TIME_SIZE);
            region.extend_from_slice(&(chunk_length as u32).to_le_bytes());
            for chunk_time in next_tick_to_write..range_end {
                match self.buffered.get(&chunk_time) {
                    Some(entry) => region.extend_from_slice(entry.data()),
                    None => TickReplayData::stream_tick_header(&mut region, chunk_time),
                }
            }

            self.last_tick_pending_write = range_end.wrapping_sub(1);
            let signature = self.scheme.sign(&region);
            self.last_signature = signature.clone();

            let mut bytes = region;
            bytes.extend_from_slice(&signature);
            self.queued_chunks.push(SignedChunk {
                bytes,
                last_tick: self.last_tick_pending_write,
                chunk_length,
            });

            next_tick_to_write = self.last_tick_pending_write + 1;
        }
    }

    /// The chunks queued since the last drain — the consumer side of
    /// `pWriter_->queueWrite` (`replay_data_file_writer.cpp:398-403`). The
    /// file half (BW-07-05-04) hands each one to the background writer and
    /// accounts it in `onBackgroundFileWritingComplete` (`:460-489`).
    pub fn take_written_chunks(&mut self) -> Vec<SignedChunk> {
        std::mem::take(&mut self.queued_chunks)
    }

    /// `ReplayDataFileWriter::numTicksToSign` (`replay_data_file_writer.hpp:99`).
    pub fn num_ticks_to_sign(&self) -> usize {
        self.num_ticks_to_sign
    }

    /// `ReplayDataFileWriter::compressionType` (`replay_data_file_writer.hpp:97`).
    pub fn compression_type(&self) -> u8 {
        self.compression_type
    }

    /// `currentTick_` (`replay_data_file_writer.hpp:107`).
    pub fn current_tick(&self) -> u32 {
        self.current_tick
    }

    /// `lastTickPendingWrite` (`replay_data_file_writer.hpp:101`).
    pub fn last_tick_pending_write(&self) -> u32 {
        self.last_tick_pending_write
    }

    /// `lastSignature_` (`replay_data_file_writer.hpp:103`), for the recovery
    /// data (`RecordingRecoveryData`, `:734-748`).
    pub fn last_signature(&self) -> &[u8] {
        &self.last_signature
    }

    /// `bufferedTickData_` (`replay_data_file_writer.hpp:111`).
    pub fn buffered(&self) -> &BTreeMap<u32, TickReplayData> {
        &self.buffered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's fixture digest (`test_replay_header.cpp:17-30`): the
    /// 16 raw MD5 bytes of "BigWorld replay header test".
    fn fixture_digest() -> [u8; 16] {
        let mut hasher = Md5Core::new();
        hasher.update(b"BigWorld replay header test");
        hasher.finalize().into()
    }

    /// The reference's fixture nonce (`test_replay_header.cpp:35-39`).
    fn fixture_nonce() -> [u8; NONCE_SIZE] {
        [1, 2, 3, 4, 5, 6, 7, 8]
    }

    /// A header with the reference's fixture fields (`:43-57`).
    fn fixture_header() -> ReplayHeader {
        ReplayHeader::new(
            &Md5Signature::new(),
            fixture_digest(),
            10,
            1,
            fixture_nonce(),
        )
    }

    /// The fields a header decodes into, at the reference's offsets
    /// (`ReplayHeader::initFromStream`, `replay_header.cpp:140-204`). This
    /// probe is written independently of the writer: it is the acceptance
    /// that a reader following the reference reads the block back.
    struct DecodedHeader {
        version: [u8; 4],
        digest: [u8; 16],
        frequency: u8,
        timestamp: u32,
        nonce: [u8; NONCE_SIZE],
        signature_length: u16,
        signature: Vec<u8>,
        num_ticks: u32,
        consumed: usize,
    }

    /// Decode a header in the reference reader's order: the version, then the
    /// digest, `gameUpdateFrequency`, `timestamp`, the nonce, the
    /// `signatureLength` the signature is verified against, then `numTicks`.
    fn decode_header(data: &[u8]) -> Option<DecodedHeader> {
        if data.len() < 35 {
            return None;
        }
        let frequency = data[20];
        let timestamp = u32::from_le_bytes(data[21..25].try_into().ok()?);
        let nonce = data[25..33].try_into().ok()?;
        let signature_length = u16::from_le_bytes(data[33..35].try_into().ok()?) as usize;
        let signature = data.get(35..35 + signature_length)?.to_vec();
        let num_ticks_start = 35 + signature_length;
        let num_ticks = u32::from_le_bytes(
            data.get(num_ticks_start..num_ticks_start + 4)?
                .try_into()
                .ok()?,
        );
        Some(DecodedHeader {
            version: data[..4].try_into().ok()?,
            digest: data[4..20].try_into().ok()?,
            frequency,
            timestamp,
            nonce,
            signature_length: signature_length as u16,
            signature,
            num_ticks,
            consumed: num_ticks_start + 4,
        })
    }

    /// The pairs and signature a meta-data block decodes into.
    #[derive(Debug, PartialEq)]
    struct DecodedMetaData {
        entries: Vec<(String, String)>,
        signature: Vec<u8>,
        consumed: usize,
    }

    /// Decode a meta-data block in the reference reader's order: the
    /// `streamSize` field, then that many bytes of packed pairs, then the
    /// signature (`ReplayMetaData::readFromStream`, `replay_metadata.cpp:76-145`).
    fn decode_metadata(data: &[u8], signature_length: usize) -> Option<DecodedMetaData> {
        if data.len() < 4 {
            return None;
        }
        let stream_size = u32::from_le_bytes(data[..4].try_into().ok()?) as usize;
        let body = data.get(4..4 + stream_size)?;

        // `readPackedInt` (`binary_stream.hpp:287-296`): a string is a packed
        // length and its bytes. Returns the string and the bytes consumed.
        let read_string = |bytes: &[u8]| -> Option<(String, usize)> {
            let first = *bytes.first()?;
            let (len, prefix) = if first == 0xFF {
                let rest = bytes.get(1..4)?;
                let len = u32::from_le_bytes([rest[0], rest[1], rest[2], 0]) as usize;
                (len, 4)
            } else {
                (first as usize, 1)
            };
            let text = String::from_utf8_lossy(bytes.get(prefix..prefix + len)?).into_owned();
            Some((text, prefix + len))
        };

        let mut entries = Vec::new();
        let mut pos = 0;
        while pos < body.len() {
            let (key, used) = read_string(&body[pos..])?;
            pos += used;
            let (value, used) = read_string(&body[pos..])?;
            pos += used;
            entries.push((key, value));
        }

        let signature = data
            .get(4 + stream_size..4 + stream_size + signature_length)?
            .to_vec();
        Some(DecodedMetaData {
            entries,
            signature,
            consumed: 4 + stream_size + signature_length,
        })
    }

    // ------------------------------------------------------------------------
    // Section: header
    // ------------------------------------------------------------------------

    // A written header round trips: every field reads back the same, the
    // whole block is consumed, and the stream-size arithmetic agrees with the
    // bytes actually written (`test_replay_header.cpp:56-80`).
    #[test]
    fn a_written_header_round_trips() {
        let scheme = Md5Signature::new();
        let header = fixture_header();
        let bytes = header.to_bytes(&scheme);

        assert_eq!(bytes.len(), ReplayHeader::stream_size(16));
        assert!(bytes.len() > 33 + 2);

        let decoded = decode_header(&bytes).expect("the header decodes");
        assert_eq!(decoded.consumed, bytes.len());
        assert_eq!(decoded.version, protocol_version_bytes());
        assert_eq!(decoded.digest, fixture_digest());
        assert_eq!(decoded.frequency, 10);
        assert_eq!(decoded.timestamp, 1);
        assert_eq!(decoded.nonce, fixture_nonce());
        assert_eq!(decoded.signature_length, 16);
        assert_eq!(decoded.signature.len(), 16);
        assert_eq!(decoded.num_ticks, 0);

        // The signature is the MD5 of the checksummed prefix alone.
        let prefix = &bytes[..35];
        assert_eq!(decoded.signature, Md5Signature::new().sign(prefix));
    }

    // The version streams subpatch first, so the current version is the four
    // bytes `[0, 0, 9, 2]` at the head of every recording
    // (`ClientServerProtocolVersion::writeToStream`,
    // `client_server_protocol_version.hpp:135-139`).
    #[test]
    fn the_version_is_streamed_subpatch_first() {
        assert_eq!(protocol_version_bytes(), [0, 0, 9, 2]);

        let scheme = Md5Signature::new();
        let bytes = fixture_header().to_bytes(&scheme);
        assert_eq!(&bytes[..4], &[0, 0, 9, 2]);
    }

    // Every field sits exactly where the reference lays it out.
    #[test]
    fn every_field_sits_at_the_reference_offset() {
        let scheme = Md5Signature::new();
        let mut header = ReplayHeader::new(&scheme, [0xAB; 16], 20, 1_700_000_000, [0x22; 8]);
        header.num_ticks = 12_345;
        let bytes = header.to_bytes(&scheme);

        assert_eq!(&bytes[0..4], &[0, 0, 9, 2]); // version, subpatch first
        assert_eq!(&bytes[4..20], &[0xAB; 16]); // digest
        assert_eq!(bytes[20], 20); // gameUpdateFrequency
        assert_eq!(&bytes[21..25], &1_700_000_000u32.to_le_bytes());
        assert_eq!(&bytes[25..33], &[0x22; 8]); // nonce
        assert_eq!(&bytes[33..35], &16u16.to_le_bytes()); // signatureLength
        assert_eq!(&bytes[35..51], &Md5Signature::new().sign(&bytes[..35])); // signature
        assert_eq!(&bytes[51..55], &12_345u32.to_le_bytes()); // numTicks
        assert_eq!(bytes.len(), 55);
    }

    // Flipping a byte in the signature fails the scheme's verify
    // (`test_replay_header.cpp:83-98`).
    #[test]
    fn a_tampered_signature_is_detected() {
        let scheme = Md5Signature::new();
        let bytes = fixture_header().to_bytes(&scheme);
        let decoded = decode_header(&bytes).expect("the header decodes");

        let mut tampered = bytes.clone();
        let last = tampered.len() - 6;
        tampered[last] ^= 0xFF;

        let tampered_sig = decode_header(&tampered)
            .expect("the tampered header still decodes")
            .signature;
        assert_eq!(tampered_sig.len(), decoded.signature.len());
        assert!(!Md5Signature::new().verify(&tampered[..35], &tampered_sig));
    }

    // A header from another protocol version is rejected before the signature
    // is even checked (`test_replay_header.cpp:101-149`).
    #[test]
    fn a_header_from_another_version_is_rejected() {
        let scheme = Md5Signature::new();
        let mut bytes = fixture_header().to_bytes(&scheme);

        // Bumping the subpatch byte (offset 0) makes it differ.
        bytes[0] += 1;
        let decoded = decode_header(&bytes).expect("the header decodes");
        assert_ne!(decoded.version, protocol_version_bytes());
    }

    // checkSufficientLength reports the exact header size once the
    // signatureLength field is readable, and refuses otherwise
    // (`test_replay_header.cpp:186-205`).
    #[test]
    fn check_sufficient_length_reports_the_header_size() {
        let scheme = Md5Signature::new();
        let bytes = fixture_header().to_bytes(&scheme);

        assert_eq!(ReplayHeader::expected_length(&bytes), Some(bytes.len()));
        assert!(ReplayHeader::is_complete(&bytes));

        // One byte short: the expected size is still reported.
        let short = &bytes[..bytes.len() - 1];
        assert_eq!(ReplayHeader::expected_length(short), Some(bytes.len()));
        assert!(!ReplayHeader::is_complete(short));

        // Too short to hold the signatureLength field at all.
        assert_eq!(ReplayHeader::expected_length(&bytes[..10]), None);
        assert!(!ReplayHeader::is_complete(&bytes[..10]));
    }

    // The numTicks field sits immediately before the end of the header, which
    // is where finalise rewrites it (`test_replay_header.cpp:208-217`).
    #[test]
    fn the_num_ticks_field_sits_at_the_end() {
        let scheme = Md5Signature::new();
        let mut header = fixture_header();
        header.num_ticks = 7;
        let bytes = header.to_bytes(&scheme);

        let offset = ReplayHeader::num_ticks_field_offset(16);
        assert_eq!(offset, bytes.len() - 4);
        assert_eq!(&bytes[offset..offset + 4], &7u32.to_le_bytes());
    }

    // ------------------------------------------------------------------------
    // Section: meta-data
    // ------------------------------------------------------------------------

    // Adding a key tracks the packed size of the pair, and re-adding a key
    // replaces the pair rather than adding a second one
    // (`test_replay_metadata.cpp:83-132`).
    #[test]
    fn adding_a_pair_tracks_its_packed_size() {
        let mut meta = ReplayMetaData::new();

        assert_eq!(meta.len(), 0);
        assert_eq!(meta.stream_size(), 0);
        assert!(!meta.has_key("key"));
        assert_eq!(meta.get("key", ""), "");
        assert_eq!(meta.get("key", "fallback"), "fallback");

        meta.add("key", "value");
        assert_eq!(meta.len(), 1);
        assert!(meta.has_key("key"));
        assert_eq!(meta.get("key", ""), "value");
        // packedSize("key") + packedSize("value") = (1+3) + (1+5).
        assert_eq!(meta.stream_size(), 4 + 6);

        // Re-adding the key replaces the value and the tracked size.
        meta.add("key", "a much longer value");
        assert_eq!(meta.len(), 1);
        assert_eq!(meta.get("key", ""), "a much longer value");
        assert_eq!(meta.stream_size(), 4 + (1 + 19));

        // A string of 256 characters or more costs three length bytes.
        let long_key = "k".repeat(300);
        meta.add(&long_key, "v");
        assert_eq!(meta.len(), 2);
        assert_eq!(meta.stream_size(), 4 + 20 + (3 + 300) + (1 + 1));

        // Iteration walks the same pairs that get()/has_key() see.
        let collected: Vec<_> = meta.iter().collect();
        assert_eq!(collected.len(), 2);
        for (key, value) in &collected {
            assert!(meta.has_key(key));
            assert_eq!(meta.get(key, ""), *value);
        }

        meta.clear();
        assert_eq!(meta.len(), 0);
        assert_eq!(meta.stream_size(), 0);
        assert!(!meta.has_key("key"));
    }

    // A signed block round trips: the pairs and the signature read back, and
    // the block carries its own signature as its tail
    // (`test_replay_metadata.cpp:137-202`).
    #[test]
    fn a_signed_block_round_trips() {
        let mut scheme = Md5Signature::new();
        let mut writer = ReplayMetaData::new();
        writer.add("app", "bigworld");
        writer.add("map", "Shark");

        // Prime the scheme with the header's signature, as the writer does
        // (`replay_data_file_writer.cpp:69-76`).
        let header = fixture_header();
        let header_bytes = header.to_bytes(&scheme);
        let header_signature = header_bytes[35..35 + 16].to_vec();
        scheme.reset();
        scheme.read_blob(&header_signature);

        let wire = writer.to_bytes(&scheme);

        // 4 bytes of block size, the two pairs, and an MD5 signature.
        assert_eq!(wire.len(), 4 + writer.stream_size() as usize + 16);

        let decoded = decode_metadata(&wire, 16).expect("the block decodes");
        assert_eq!(
            decoded.entries,
            vec![
                ("app".to_string(), "bigworld".to_string()),
                ("map".to_string(), "Shark".to_string()),
            ]
        );
        assert_eq!(decoded.consumed, wire.len());
        assert_eq!(decoded.signature, &wire[wire.len() - 16..]);
        assert_eq!(decoded.signature.len(), 16);

        // The signature chains from the header's: the MD5 of the header
        // signature followed by the block prefix.
        assert_eq!(decoded.signature, scheme.sign(&wire[..wire.len() - 16]));

        // An empty block is legal: just the size field and the signature.
        let empty = ReplayMetaData::new();
        assert!(empty.is_empty());
        let empty_wire = empty.to_bytes(&Md5Signature::new());
        assert_eq!(empty_wire.len(), 4 + 16);
        let decoded = decode_metadata(&empty_wire, 16).expect("the empty block decodes");
        assert_eq!(decoded.consumed, empty_wire.len());
        assert!(decoded.entries.is_empty());
    }

    // Altering the meta-data itself invalidates the signature it carries
    // (`test_replay_metadata.cpp:206-234`).
    #[test]
    fn a_tampered_block_fails_its_signature() {
        let scheme = Md5Signature::new();
        let mut writer = ReplayMetaData::new();
        writer.add("app", "bigworld");
        let wire = writer.to_bytes(&scheme);

        // Keep every length intact and change a character of the value, so
        // the block still parses and only the checksum disagrees.
        let mut tampered = wire.clone();
        tampered[4 + 1 + 1 + 3] = b'X';
        assert_eq!(wire.len(), tampered.len());

        let signature = decode_metadata(&tampered, 16)
            .expect("the tampered block still decodes")
            .signature;
        let chained = Md5Signature::new();
        assert!(!chained.verify(&tampered[..tampered.len() - 16], &signature));
    }

    // A block that claims more data than the stream holds is refused
    // (`test_replay_metadata.cpp:280-301`).
    #[test]
    fn an_oversized_block_is_rejected() {
        let mut truncated = Vec::new();
        truncated.extend_from_slice(&1000u32.to_le_bytes());
        assert!(
            decode_metadata(&truncated, 16).is_none(),
            "the block must not decode"
        );
    }

    // Fragments too short for either block fail to decode at all.
    #[test]
    fn fragments_shorter_than_a_block_do_not_decode() {
        let scheme = Md5Signature::new();
        let bytes = fixture_header().to_bytes(&scheme);

        assert!(decode_header(&bytes[..10]).is_none());
        assert!(decode_header(&[]).is_none());
        // A fragment that ends inside the numTicks field does not decode
        // either: the header is 55 bytes and numTicks starts at 51.
        assert!(decode_header(&bytes[..53]).is_none());
        assert!(decode_metadata(&[0, 0, 0], 16).is_none());
        assert!(decode_metadata(&[], 16).is_none());
    }

    // Because of the size/wire disagreement at 255 bytes and up (see
    // `the_packed_length_prefix_switches_at_255`), a pair whose key is long
    // enough to need the `0xFF` length form writes one byte more than its
    // own `streamSize` field claims, so the reader reads a truncated body and
    // refuses the block. The writer reproduces the reference here rather than
    // correcting it.
    #[test]
    fn a_long_pair_carries_a_too_small_stream_size() {
        let scheme = Md5Signature::new();
        let mut writer = ReplayMetaData::new();
        writer.add(&"k".repeat(300), "v");
        let wire = writer.to_bytes(&scheme);

        // The accounting says (3 + 300) for the key and (1 + 1) for the value...
        let claimed = u32::from_le_bytes(wire[..4].try_into().unwrap()) as usize;
        assert_eq!(claimed, 3 + 300 + 1 + 1);

        // ...while the wire holds the extra length byte the key costs, so the
        // body is one byte longer than the block claims and it does not decode.
        let actual_body = wire.len() - 16 - 4;
        assert_eq!(actual_body, 4 + 300 + 1 + 1);
        assert_ne!(actual_body, claimed);
        assert!(decode_metadata(&wire, 16).is_none());
    }

    // The static length check reports the block size once the size field is
    // readable, and refuses otherwise (`test_replay_metadata.cpp:306-332`).
    #[test]
    fn check_sufficient_length_reports_the_block_size() {
        let scheme = Md5Signature::new();
        let mut writer = ReplayMetaData::new();
        writer.add("app", "bigworld");
        let wire = writer.to_bytes(&scheme);

        assert_eq!(ReplayMetaData::expected_length(&wire, 16), Some(wire.len()));
        assert!(ReplayMetaData::is_complete(&wire, 16));

        // One byte short of the signature is one byte short of the block.
        let short = &wire[..wire.len() - 1];
        assert_eq!(ReplayMetaData::expected_length(short, 16), Some(wire.len()));
        assert!(!ReplayMetaData::is_complete(short, 16));

        // Not even the size header yet.
        assert_eq!(ReplayMetaData::expected_length(&wire[..3], 16), None);
        assert_eq!(ReplayMetaData::expected_length(&[], 16), None);
    }

    // swap() exchanges the contents of two blocks wholesale
    // (`test_replay_metadata.cpp:337-362`).
    #[test]
    fn swap_exchanges_the_blocks_wholesale() {
        let mut first = ReplayMetaData::new();
        first.add("a", "1");

        let mut second = ReplayMetaData::new();
        second.add("b", "2");
        second.add("c", "3");

        let first_size = first.stream_size();
        let second_size = second.stream_size();

        first.swap(&mut second);

        assert_eq!(first.len(), 2);
        assert!(first.has_key("b"));
        assert_eq!(first.get("b", ""), "2");
        assert_eq!(first.stream_size(), second_size);

        assert_eq!(second.len(), 1);
        assert!(second.has_key("a"));
        assert_eq!(second.get("a", ""), "1");
        assert_eq!(second.stream_size(), first_size);
    }

    // The two rules the reference uses disagree from 255 bytes up:
    // `writePackedInt` switches to `0xFF` + 3 bytes at a value of 255, while
    // `calculateStringPackedSize` only switches to three length bytes at a
    // size of 256 (`binary_stream.hpp:254-268` vs
    // `replay_metadata.cpp:248-253`). The reference's own unit test only
    // exercises a 300-character key, which both rules agree on. The writer
    // follows the wire rule when writing and the size rule when accounting,
    // and reproduces the disagreement rather than repairing it, so a block
    // that holds a string of 255 bytes or more carries a `streamSize` field
    // that is 3 bytes (at exactly 255) or 1 byte (above) too small.
    #[test]
    fn the_packed_length_prefix_switches_at_255() {
        // Below the boundary both rules agree.
        let short = "k".repeat(254);
        let mut bytes = Vec::new();
        append_packed_string(&mut bytes, &short);
        assert_eq!(bytes.len(), 1 + 254);
        assert_eq!(bytes[0], 254);
        assert_eq!(packed_string_size(&short), 1 + 254);

        // At exactly 255 the wire has switched to `0xFF` + 3 bytes...
        let exact = "k".repeat(255);
        let mut bytes = Vec::new();
        append_packed_string(&mut bytes, &exact);
        assert_eq!(bytes.len(), 4 + 255);
        assert_eq!(bytes[0], 0xFF);
        assert_eq!(bytes[1..4], [0xFF, 0x00, 0x00]);

        // ...while the accounting still counts a single length byte.
        assert_eq!(packed_string_size(&exact), 1 + 255);

        // Above the boundary both rules switch, and disagree by one byte.
        let long = "k".repeat(300);
        let mut bytes = Vec::new();
        append_packed_string(&mut bytes, &long);
        assert_eq!(bytes.len(), 4 + 300);
        assert_eq!(packed_string_size(&long), 3 + 300);
    }

    // ------------------------------------------------------------------------
    // Section: compression and tick data (BW-07-05-03)
    // ------------------------------------------------------------------------

    // A NONE frame is the type byte and the raw payload
    // (`CompressionOStream`, `compression_stream.cpp:66-100`).
    #[test]
    fn a_none_frame_carries_its_payload_raw() {
        let frame = compress_frame(COMPRESSION_NONE, b"hello").unwrap();
        assert_eq!(frame, [COMPRESSION_NONE, b'h', b'e', b'l', b'l', b'o']);
        assert_eq!(decompress_frame(&frame).unwrap(), b"hello");
    }

    // A zip frame is the type byte, a packed uncompressed length, a packed
    // compressed length, then the zlib payload (`ZipOStream`'s destructor,
    // `zip_stream.cpp:261-286`), and it inflates back to what was written.
    #[test]
    fn a_zip_frame_matches_the_reference_layout() {
        // 600 near-constant bytes: the declared source length still needs the
        // packed 4-byte form, while the compressed length fits a single byte.
        let payload: Vec<u8> = vec![b'A'; 600];
        let frame = compress_frame(COMPRESSION_ZIP_BEST_SPEED, &payload).unwrap();

        assert_eq!(frame[0], COMPRESSION_ZIP_BEST_SPEED);
        // 600 needs the 4-byte packed form of `writePackedInt`.
        let read_length = |bytes: &[u8]| -> (usize, usize) {
            if bytes[0] == 0xFF {
                (
                    u32::from_le_bytes([bytes[1], bytes[2], bytes[3], 0]) as usize,
                    4,
                )
            } else {
                (bytes[0] as usize, 1)
            }
        };
        let (src_len, used) = read_length(&frame[1..]);
        let (compressed_len, used2) = read_length(&frame[1 + used..]);
        assert_eq!(src_len, payload.len());
        assert_eq!(
            frame.len(),
            1 + used + used2 + compressed_len,
            "the declared compressed length must bound the frame"
        );

        // A zlib stream at the frame's level inflates to the payload.
        use std::io::Read as _;
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(&frame[1 + used + used2..1 + used + used2 + compressed_len])
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, payload);
        assert_eq!(decompress_frame(&frame).unwrap(), payload);
    }

    // The two DEFAULT sentinels resolve against the configured defaults,
    // which the recording writer leaves at NONE
    // (`compression_stream.cpp:53-58, 71-79`).
    #[test]
    fn the_default_sentinels_resolve_to_none() {
        assert_eq!(
            compress_frame(COMPRESSION_DEFAULT_INTERNAL, b"x").unwrap(),
            compress_frame(COMPRESSION_NONE, b"x").unwrap()
        );
        assert_eq!(
            compress_frame(COMPRESSION_DEFAULT_EXTERNAL, b"x").unwrap(),
            compress_frame(COMPRESSION_NONE, b"x").unwrap()
        );
    }

    // Any other type byte is refused.
    #[test]
    fn an_unknown_compression_type_is_rejected() {
        assert_eq!(
            compress_frame(99, b"x"),
            Err(CompressError::InvalidCompressionType(99))
        );
    }

    // A tick's block is its 8-byte header and the compression frame, with
    // the `TickDataLength` field patched to the frame size
    // (`TickReplayData::finalise`, `replay_data_file_writer.cpp:817-852`).
    #[test]
    fn a_tick_block_round_trips_uncompressed() {
        assert_eq!(TickReplayData::TICK_HEADER_SIZE, 8);

        let mut tick = TickReplayData::init(100, 2);
        assert_eq!(tick.tick(), 100);
        assert_eq!(tick.num_cells_left(), 2);
        tick.transfer_from_cell(b"cell-A ");
        assert_eq!(tick.num_cells_left(), 1);
        tick.transfer_from_cell(b"cell-B");
        assert_eq!(tick.num_cells_left(), 0);
        tick.finalise(COMPRESSION_NONE).unwrap();

        let blob = tick.data();
        assert_eq!(&blob[..4], &100u32.to_le_bytes()); // gameTime
        let frame_len = 1 + b"cell-A ".len() + b"cell-B".len();
        assert_eq!(&blob[4..8], &(frame_len as u32).to_le_bytes());
        assert_eq!(blob[8], COMPRESSION_NONE);
        assert_eq!(&blob[9..], b"cell-A cell-B");
        assert_eq!(tick.size(), 8 + frame_len);
    }

    // The same block compressed: the frame inflates back to the cells'
    // concatenated data — the known event sequence, losslessly
    // (`TickReplayData::finalise` over `CompressionOStream`).
    #[test]
    fn a_compressed_tick_block_decompresses_losslessly() {
        let events = b"create entity 1; move to (10, 20); create entity 2;";
        let mut tick = TickReplayData::init(7, 1);
        tick.transfer_from_cell(events);
        tick.finalise(COMPRESSION_ZIP_BEST_SPEED).unwrap();

        let blob = tick.data();
        assert_eq!(&blob[..4], &7u32.to_le_bytes());
        let frame_len = u32::from_le_bytes(blob[4..8].try_into().unwrap());
        assert_eq!(frame_len as usize, blob.len() - 8);
        assert_eq!(
            decompress_frame(&blob[8..]).unwrap(),
            events,
            "the tick's event sequence must read back identically"
        );
    }

    // A transfer onto a complete set drops the data
    // (`TickReplayData::transferFromCell`, `:793-800`).
    #[test]
    fn a_transfer_on_a_complete_set_is_dropped() {
        let mut tick = TickReplayData::init(7, 1);
        tick.transfer_from_cell(b"first");
        assert_eq!(tick.num_cells_left(), 0);
        tick.transfer_from_cell(b"second");
        assert_eq!(tick.data(), b"first");
    }

    // A cell reporting a larger space grows the expected count
    // (`TickReplayData::numExpectedCells`, `:854-877`; `addTickData`
    // `:252-256`).
    #[test]
    fn a_new_cell_grows_the_expected_count() {
        let mut tick = TickReplayData::init(7, 1);
        tick.transfer_from_cell(b"first");
        tick.set_num_expected_cells(2);
        assert_eq!(tick.num_expected_cells(), 2);
        assert_eq!(tick.num_cells_left(), 1);
        tick.transfer_from_cell(b"second");
        assert_eq!(tick.num_cells_left(), 0);
        assert_eq!(tick.data(), b"firstsecond");
    }

    /// One tick entry read back out of a chunk.
    struct DecodedTick {
        game_time: u32,
        blob: Vec<u8>,
    }

    /// The entries and signature of a chunk, and how many bytes it occupied.
    struct DecodedChunk {
        entries: Vec<DecodedTick>,
        signature: Vec<u8>,
        consumed: usize,
    }

    /// `ReplayDataFileReader::Impl::readNextChunkLength`/`readNextChunk`/
    /// `readTickData` (`replay_data_file_reader.cpp:722-905`), for the
    /// writer's read-back probe: `[chunkLength u32][entries][signature]`,
    /// each entry `[gameTime u32][dataLength u32][data]`, the game times
    /// consecutive. This probe is written independently of the writer.
    fn decode_chunk(bytes: &[u8], signature_length: usize) -> Option<DecodedChunk> {
        if bytes.len() < 4 {
            return None;
        }
        let chunk_length = u32::from_le_bytes(bytes[..4].try_into().ok()?) as usize;
        if bytes.len() < 4 + chunk_length + signature_length {
            return None; // incomplete chunk: the reader waits for more data
        }

        let mut entries = Vec::new();
        let mut pos = 4;
        let payload_end = 4 + chunk_length;
        while pos < payload_end {
            let game_time = u32::from_le_bytes(bytes.get(pos..pos + 4)?.try_into().ok()?);
            let data_length =
                u32::from_le_bytes(bytes.get(pos + 4..pos + 8)?.try_into().ok()?) as usize;
            let blob = bytes.get(pos + 8..pos + 8 + data_length)?.to_vec();
            if pos + 8 + data_length > payload_end {
                return None;
            }
            pos += 8 + data_length;
            entries.push(DecodedTick { game_time, blob });
        }

        // Game times are consecutive once a first entry was read (the
        // reference's `lastGameTime_ != 0` guard).
        for pair in entries.windows(2) {
            if pair[0].game_time != 0 {
                assert_eq!(
                    pair[1].game_time,
                    pair[0].game_time + 1,
                    "the reference requires consecutive game times"
                );
            }
        }

        Some(DecodedChunk {
            entries,
            signature: bytes
                .get(payload_end..payload_end + signature_length)?
                .to_vec(),
            consumed: payload_end + signature_length,
        })
    }

    /// A recorder over an MD5 scheme, primed from a known metadata signature,
    /// with the given `numTicksToSign` (`ReplayDataFileWriter`'s writing
    /// constructor, `replay_data_file_writer.cpp:40-87`).
    fn recorder(num_ticks_to_sign: usize) -> ReplayRecorder {
        let mut recorder = ReplayRecorder::new(
            Box::new(Md5Signature::new()),
            COMPRESSION_ZIP_BEST_SPEED,
            num_ticks_to_sign,
        );
        // The metadata block's signature ends in ...the block it signed.
        let mut meta = ReplayMetaData::new();
        meta.add("app", "bigworld");
        let meta_signature = {
            let wire = meta.to_bytes(&Md5Signature::new());
            wire[wire.len() - 16..].to_vec()
        };
        recorder.prime_signature(&meta_signature);
        recorder
    }

    // Ticks are grouped into chunks of `numTicksToSign`, in game-time order,
    // each chunk laid out `[chunkLength][entries][signature]`
    // (`signAndWriteCompletedTickData`, `replay_data_file_writer.cpp:314-405`).
    #[test]
    fn ticks_are_chunked_by_num_ticks_to_sign() {
        let mut recorder = recorder(2);
        assert_eq!(recorder.num_ticks_to_sign(), 2);
        assert_eq!(recorder.compression_type(), COMPRESSION_ZIP_BEST_SPEED);

        // Tick 10 completes on its single cell.
        assert_eq!(
            recorder.add_tick_data(10, 1, b"tick-ten "),
            AddTickDataOutcome::Accepted
        );
        // Not yet a full chunk of two.
        recorder.sign_and_write_completed_tick_data(false);
        assert!(recorder.take_written_chunks().is_empty());

        assert_eq!(
            recorder.add_tick_data(11, 1, b"tick-eleven"),
            AddTickDataOutcome::Accepted
        );
        // Adding tick 11 unblocked the first chunk of two.
        let chunks = recorder.take_written_chunks();
        assert_eq!(chunks.len(), 1);
        let chunk = &chunks[0];
        assert_eq!(chunk.last_tick, 11);
        assert_eq!(recorder.last_tick_pending_write(), 11);

        let decoded = decode_chunk(&chunk.bytes, 16).unwrap();
        assert_eq!(decoded.consumed, chunk.bytes.len());
        // The reader consumed the signature off the end of the chunk.
        assert_eq!(decoded.signature.len(), 16);
        assert_eq!(decoded.entries.len(), 2);
        assert_eq!(decoded.entries[0].game_time, 10);
        assert_eq!(decoded.entries[1].game_time, 11);
        assert_eq!(
            decompress_frame(&decoded.entries[0].blob).unwrap(),
            b"tick-ten "
        );
        assert_eq!(
            decompress_frame(&decoded.entries[1].blob).unwrap(),
            b"tick-eleven"
        );

        // The chunk length is the payload size: each entry's block.
        let expected: usize = decoded.entries.iter().map(|e| 8 + e.blob.len()).sum();
        assert_eq!(chunk.chunk_length, expected);
        assert_eq!(
            u32::from_le_bytes(chunk.bytes[..4].try_into().unwrap()) as usize,
            expected
        );

        // The written ticks stay buffered until the file write completes.
        let remaining: Vec<u32> = recorder.buffered().keys().copied().collect();
        assert_eq!(remaining, vec![10, 11]);
    }

    // A tick with no data at all contributes just its 8-byte header, which is
    // how the writer bridges a gap in the game times
    // (`signAndWriteCompletedTickData`, `:334-346` and `:382-387`).
    #[test]
    fn a_gap_writes_an_empty_tick_header() {
        let mut recorder = recorder(10);
        recorder.add_tick_data(10, 1, b"tick-ten");
        recorder.add_tick_data(12, 1, b"tick-twelve");

        recorder.sign_and_write_completed_tick_data(true);
        let chunks = recorder.take_written_chunks();
        assert_eq!(chunks.len(), 1);
        let decoded = decode_chunk(&chunks[0].bytes, 16).unwrap();
        assert_eq!(decoded.entries.len(), 3);
        assert_eq!(decoded.entries[0].game_time, 10);
        assert_eq!(decoded.entries[1].game_time, 11);
        // The gap carries no data at all: just its zero-length entry.
        assert!(decoded.entries[1].blob.is_empty());
        assert_eq!(decoded.entries[2].game_time, 12);
        assert_eq!(
            decompress_frame(&decoded.entries[2].blob).unwrap(),
            b"tick-twelve"
        );

        // The empty tick costs exactly `tickHeaderSize` bytes.
        let expected: usize = decoded.entries.iter().map(|e| 8 + e.blob.len()).sum();
        assert_eq!(chunks[0].chunk_length, expected);
    }

    // The last chunk signs whatever is left, below the threshold
    // (`finalise`'s `isLastChunk`, `:534-540`).
    #[test]
    fn the_last_chunk_signs_the_remaining_ticks() {
        let mut recorder = recorder(10);
        for (tick, data) in [(4u32, b"a" as &[u8]), (5, b"b"), (6, b"c")] {
            recorder.add_tick_data(tick, 1, data);
        }
        // Below ten ticks: nothing is written out yet.
        recorder.sign_and_write_completed_tick_data(false);
        assert!(recorder.take_written_chunks().is_empty());

        recorder.sign_and_write_completed_tick_data(true);
        let chunks = recorder.take_written_chunks();
        assert_eq!(chunks.len(), 1);
        let decoded = decode_chunk(&chunks[0].bytes, 16).unwrap();
        assert_eq!(decoded.entries.len(), 3);
        assert_eq!(chunks[0].last_tick, 6);
        assert_eq!(recorder.buffered().len(), 3);
    }

    // Each chunk's signature chains from the previous block's signature: the
    // first from the metadata's, the second from the first chunk's
    // (`replay_data_file_writer.cpp:69-86, 325-330, 398-401`).
    #[test]
    fn chunk_signatures_chain_from_the_previous_block() {
        let mut recorder = recorder(2);

        let mut meta = ReplayMetaData::new();
        meta.add("app", "bigworld");
        let meta_signature = meta.to_bytes(&Md5Signature::new());
        let meta_signature = meta_signature[meta_signature.len() - 16..].to_vec();

        recorder.add_tick_data(10, 1, b"tick-ten ");
        recorder.add_tick_data(11, 1, b"tick-eleven");
        let first = recorder.take_written_chunks();

        recorder.add_tick_data(12, 1, b"tick-twelve");
        recorder.add_tick_data(13, 1, b"tick-thirteen");
        let second = recorder.take_written_chunks();

        // Independently: the first signature covers the metadata signature
        // and the first chunk's signed region.
        let md5_of = |parts: &[&[u8]]| -> Vec<u8> {
            let mut hasher = Md5Core::new();
            for part in parts {
                hasher.update(part);
            }
            hasher.finalize().to_vec()
        };
        let region = |chunk: &SignedChunk| chunk.bytes[..chunk.bytes.len() - 16].to_vec();

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(
            &first[0].bytes[first[0].bytes.len() - 16..],
            &md5_of(&[&meta_signature, &region(&first[0])])
        );
        assert_eq!(
            &second[0].bytes[second[0].bytes.len() - 16..],
            &md5_of(&[
                &first[0].bytes[first[0].bytes.len() - 16..],
                &region(&second[0])
            ])
        );

        // The recorder's own view of the chain agrees.
        assert_eq!(
            recorder.last_signature(),
            &second[0].bytes[second[0].bytes.len() - 16..]
        );
    }

    // An incomplete tick still in the buffer is written out once a later tick
    // completes ("Writing out incomplete tick %u", `:296-303`).
    #[test]
    fn an_incomplete_tick_is_written_out_when_a_later_tick_completes() {
        let mut recorder = recorder(10);

        // Tick 10 expects two cells but only one arrives.
        assert_eq!(
            recorder.add_tick_data(10, 2, b"half-a "),
            AddTickDataOutcome::Accepted
        );
        recorder.sign_and_write_completed_tick_data(false);
        assert!(recorder.take_written_chunks().is_empty());

        recorder.add_tick_data(11, 1, b"tick-eleven");
        recorder.sign_and_write_completed_tick_data(true);
        let chunks = recorder.take_written_chunks();
        assert_eq!(chunks.len(), 1);

        let decoded = decode_chunk(&chunks[0].bytes, 16).unwrap();
        assert_eq!(decoded.entries.len(), 2);
        // Tick 10 went out with only the data it had.
        assert_eq!(
            decompress_frame(&decoded.entries[0].blob).unwrap(),
            b"half-a "
        );
        assert_eq!(decoded.entries[1].game_time, 11);
    }

    // A tick already written out is dropped with its data counted
    // (`addTickData`, `:192-210`).
    #[test]
    fn a_tick_already_written_is_dropped() {
        let mut recorder = recorder(1);
        recorder.add_tick_data(10, 1, b"tick-ten");
        recorder.add_tick_data(11, 1, b"tick-eleven");
        // With one tick per chunk, each completion queued a chunk.
        assert_eq!(recorder.take_written_chunks().len(), 2);

        // The new cell's late report for tick 10.
        assert_eq!(
            recorder.add_tick_data(10, 1, b"late"),
            AddTickDataOutcome::DroppedAlreadyWritten
        );
        // The dropped report changes nothing buffered.
        assert_eq!(recorder.buffered().len(), 2);
    }

    // The first tick with no data is ignored, and the recording starts at the
    // next tick that has some (`addTickData`, `:206-219`).
    #[test]
    fn the_first_tick_with_no_data_is_ignored() {
        let mut recorder = recorder(10);
        assert_eq!(
            recorder.add_tick_data(9, 1, b""),
            AddTickDataOutcome::IgnoredEmptyFirstTick
        );
        assert_eq!(recorder.current_tick(), 0);

        recorder.add_tick_data(10, 1, b"tick-ten");
        assert_eq!(recorder.current_tick(), 11);
        assert_eq!(recorder.last_tick_pending_write(), 9);
    }

    // An empty recorder signs nothing, even on the last chunk
    // (`signAndWriteCompletedTickData`'s early return, `:317-321`).
    #[test]
    fn an_empty_recorder_writes_nothing() {
        let mut recorder = recorder(10);
        recorder.sign_and_write_completed_tick_data(true);
        assert!(recorder.take_written_chunks().is_empty());
    }

    // End to end, a known event sequence survives the whole pipeline: cell
    // blobs in, chunks out, read back and inflated through the reference
    // reader's order with no bytes lost.
    #[test]
    fn a_known_event_sequence_round_trips_end_to_end() {
        let mut recorder = recorder(2);
        let sequence: &[(u32, &[u8])] = &[
            (100, b"entity 1 created; entity 1 moved to (3, 4); "),
            (101, b"entity 1 called method strike(2); entity 2 created; "),
            (
                102,
                b"entity 2 volatile (5.5, 6.5, 7.5); entity 1 deleted; ",
            ),
        ];
        for &(tick, events) in sequence {
            recorder.add_tick_data(tick, 1, events);
        }
        recorder.sign_and_write_completed_tick_data(true);
        let chunks = recorder.take_written_chunks();
        assert_eq!(chunks.len(), 2);

        let mut read_back: Vec<(u32, Vec<u8>)> = Vec::new();
        for chunk in &chunks {
            let decoded = decode_chunk(&chunk.bytes, 16).unwrap();
            assert_eq!(decoded.consumed, chunk.bytes.len());
            for entry in decoded.entries {
                let data = decompress_frame(&entry.blob).unwrap();
                read_back.push((entry.game_time, data));
            }
        }

        assert_eq!(
            read_back,
            sequence
                .iter()
                .map(|&(tick, events)| (tick, events.to_vec()))
                .collect::<Vec<_>>(),
            "the event sequence must read back identically"
        );
    }

    // A frame whose type byte is no compression the reference knows does not
    // inflate (`CompressionIStream`, `compression_stream.cpp:91-97`).
    #[test]
    fn a_frame_with_an_unknown_type_does_not_inflate() {
        assert_eq!(
            decompress_frame(&[99, b'x']),
            Err(CompressError::InvalidCompressionType(99))
        );
    }

    // A frame that lies — a zlib payload that will not inflate, or a declared
    // uncompressed length smaller than the data — is refused, the way
    // `uncompress` would have overflowed its `srcLen`-sized buffer
    // (`zip_stream.cpp:124-184`).
    #[test]
    fn a_frame_lying_about_its_lengths_is_rejected() {
        let mut frame = compress_frame(COMPRESSION_ZIP_BEST_SPEED, b"payload").unwrap();

        // Flip the last byte of the zlib checksum: the payload will not
        // inflate.
        let tail = frame.len() - 1;
        frame[tail] ^= 0xFF;
        assert_eq!(decompress_frame(&frame), Err(CompressError::Zlib));

        // Rebuild the frame and undershoot the declared source length.
        let mut frame = compress_frame(COMPRESSION_ZIP_BEST_SPEED, b"payload").unwrap();
        assert_eq!(frame[1], 7); // a one-byte packed length
        frame[1] = 3;
        assert_eq!(decompress_frame(&frame), Err(CompressError::Zlib));
    }

    // A repeated report for a still-buffered tick grows the expected cell
    // count (`addTickData`, `:252-256`), and the tick goes out with every
    // report it gathered.
    #[test]
    fn a_repeated_tick_can_grow_its_expected_cells() {
        let mut recorder = recorder(10);
        recorder.add_tick_data(10, 2, b"one");
        recorder.add_tick_data(10, 3, b"two");

        recorder.add_tick_data(11, 1, b"tick-eleven");
        recorder.sign_and_write_completed_tick_data(true);
        let chunks = recorder.take_written_chunks();
        assert_eq!(chunks.len(), 1);

        let decoded = decode_chunk(&chunks[0].bytes, 16).unwrap();
        assert_eq!(decoded.entries.len(), 2);
        assert_eq!(
            decompress_frame(&decoded.entries[0].blob).unwrap(),
            b"onetwo"
        );
        assert_eq!(decoded.entries[1].game_time, 11);
    }

    // The read-back probe mirrors the reference reader's guards: it waits
    // for a whole chunk and refuses an entry whose data runs past the chunk's
    // payload (`readNextChunkLength`/`readNextChunk`,
    // `replay_data_file_reader.cpp:722-763`).
    #[test]
    fn the_chunk_probe_refuses_incomplete_or_overrunning_input() {
        // Shorter than a chunk length field.
        assert!(decode_chunk(&[1, 2], 16).is_none());

        let mut recorder = recorder(10);
        recorder.add_tick_data(10, 1, b"tick-ten");
        recorder.sign_and_write_completed_tick_data(true);
        let chunk = recorder.take_written_chunks().remove(0);

        // One byte short of the signature waits for more data.
        assert!(decode_chunk(&chunk.bytes[..chunk.bytes.len() - 1], 16).is_none());

        // An entry claiming data that reaches into the signature is refused:
        // the entry's length field sits at [8..12], right after its game time.
        let mut bogus = chunk.bytes.clone();
        bogus[8..12].copy_from_slice(&(chunk.chunk_length as u32).to_le_bytes());
        assert!(decode_chunk(&bogus, 16).is_none());
    }
}
