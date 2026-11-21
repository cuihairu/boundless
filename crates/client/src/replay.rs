//! Replay header and metadata parsing (BW-11-03-01).
//!
//! The reference stores a recording's header and metadata in the replay file's
//! header block (`lib/connection/replay_header.{hpp,cpp}`) and metadata block
//! (`lib/connection/replay_metadata.{hpp,cpp}`).
//!
//! Header block (`replay_header.cpp`):
//! * `initFromStream` (`:140-204`) reads, in order: `version` (4×uint8),
//!   `digest` (16 bytes), `gameUpdateFrequency` (uint8), `timestamp` (uint32),
//!   `nonce` (NONCE_SIZE = 8), `signatureLength` (uint16), the signature, then
//!   `numTicks` (GameTime = uint32). A version that is not the current
//!   `ClientServerProtocolVersion` (2.9.0.0) fails with "version mismatch"
//!   (`:164-171`; `supports` at
//!   `client_server_protocol_version.hpp:151-158` is an equality).
//! * `streamSize` (`:210-220`): `4 + 16 + 1 + 4 + 8 + 2 + signatureLength + 4`.
//! * `checkSufficientLength` (`:276-297`): reads `signatureLength` at its
//!   field offset and computes the total header size.
//!
//! Metadata block (`replay_metadata.cpp`):
//! * `readFromStream` (`:78-145`) reads `streamSize` (uint32), validates it
//!   against the remaining length, then reads the key/value string pairs (each
//!   a `BW::string`: a `readPackedInt` length prefix
//!   [`binary_stream.hpp:287-296`, 1 byte for a value below 255 else `0xFF` +
//!   3 little-endian bytes] followed by the bytes), then verifies (or reads)
//!   the signature.
//! * `calculateStringPackedSize` (`:240-245`): a small string (< 256) has a
//!   1-byte length prefix, a larger one a 3-byte prefix.
//!
//! The acceptance is reading the same fields out of a known recording: the
//! port parses the header and metadata blocks and produces the reference's
//! fields — version, digest, frequency, timestamp, nonce, signature length,
//! numTicks, and the metadata key/value collection.
//!
//! BW-11-03-02 is the tick chunk decode and value validation: [`parse_tick_chunks`]
//! reads the signed `[chunkLength][payload][signature]` chunks and applies the
//! reader's value checks (`replay_data_file_reader.cpp:716-948` — zero chunk
//! length, sequential game time, the declared tick count, entry length bounds);
//! [`decompress_tick`] decodes a `CompressionIStream` blob (raw or zlib via
//! `ZipIStream`); and [`scan_block_types`] validates the `BlockType` bytes of
//! the decompressed tick data (`replay_controller.cpp:1001-1062`).

/// The current client-server protocol version (2.9.0.0,
/// `client_server_protocol_version.hpp:51-54`).
pub const CURRENT_VERSION: (u8, u8, u8, u8) = (2, 9, 0, 0);

/// `ReplayHeader::NONCE_SIZE` (`replay_header.hpp:30`).
pub const NONCE_SIZE: usize = 8;

/// A parsed replay header, `ReplayHeader` (`replay_header.hpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHeader {
    /// `version()`, as major/minor/patch/subpatch.
    pub version: (u8, u8, u8, u8),
    /// `digest()`, the MD5 digest (16 bytes).
    pub digest: [u8; 16],
    /// `gameUpdateFrequency()`.
    pub game_update_frequency: u8,
    /// `timestamp()`, seconds after epoch.
    pub timestamp: u32,
    /// The nonce (`NONCE_SIZE` bytes).
    pub nonce: [u8; NONCE_SIZE],
    /// `reportedSignatureLength()`.
    pub signature_length: u16,
    /// The signature bytes, the `pSignature` out-parameter of `initFromStream`.
    pub signature: Vec<u8>,
    /// `numTicks()`.
    pub num_ticks: u32,
}

/// A parsed replay metadata block, `ReplayMetaData` (`replay_metadata.hpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayMetaData {
    /// The key/value collection (`collection_`), in insertion order.
    pub entries: Vec<(String, String)>,
    /// `streamSize()`, the packed size of the collection.
    pub stream_size: u32,
    /// The signature bytes (`pSignature` of `readFromStream`).
    pub signature: Vec<u8>,
}

/// Why parsing a replay header or metadata block failed, mirroring the
/// reference error strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// `initFromStream`: "Read replay header error: version mismatch".
    VersionMismatch,
    /// `checkSufficientLength`: the fragment is too short for a full header.
    InsufficientHeader,
    /// `readFromStream`: "Insufficient data for meta-data".
    MetadataTooLarge,
    /// `readFromStream`: "Meta-data corrupted".
    MetadataCorrupted,
    /// A header/metadata signature failed the checksum scheme's verify.
    ChecksumMismatch,
    /// `readNextChunkLength`: "Got zero chunk length".
    ZeroChunkLength,
    /// `readTickData`: "File corrupted: Could not read game time data header".
    TickDataHeader,
    /// `readTickData`: "File corrupted: Game time mismatch".
    GameTimeMismatch,
    /// `readTickData`: "File corrupted: Extraneous tick data".
    ExtraneousTickData,
    /// `CompressionIStream`: "Invalid compression type: %d".
    InvalidCompressionType(u8),
    /// `ZipIStream::init`/`uncompress` failed to inflate (or overflowed the
    /// declared decompressed size).
    DecompressionFailed,
    /// `processTick`: "Could not read valid block type".
    InvalidBlockType(u8),
    /// A block's declared data length exceeds the remaining tick data.
    BlockDataOverrun,
}

/// A checksum scheme for a replay file, `ChecksumSchemePtr`
/// (`lib/connection/checksum_scheme.hpp`): produces a signature over a byte
/// range and verifies a stored signature against it.
///
/// The reference's `ReplayChecksumScheme` (`replay_checksum_scheme.cpp`) is a
/// keyed scheme (an HMAC over the streamed bytes); the port models the
/// interface and leaves the keyed cryptographic digest as a registered
/// difference — a caller supplies its own scheme, or `None` for no
/// verification (the reference reads the signature without verifying when
/// `pChecksumScheme_ == NULL`, `replay_header.cpp:145-148`).
pub trait ChecksumScheme {
    /// The signature length in bytes (`streamSize`).
    fn stream_size(&self) -> usize;
    /// Verify a signature against the bytes it was computed over.
    fn verify(&self, data: &[u8], signature: &[u8]) -> bool;
}

/// A byte cursor over a slice, reading little-endian like `BinaryIStream`.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.remaining() < n {
            return None;
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        self.take(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Option<f32> {
        self.take(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// `readPackedInt` (`binary_stream.hpp:287-296`): 1 byte, or `0xFF` + 3
    /// little-endian bytes.
    fn packed_len(&mut self) -> Option<usize> {
        let first = self.u8()?;
        if first == 0xFF {
            let bytes = self.take(3)?;
            let v = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]);
            Some(v as usize)
        } else {
            Some(first as usize)
        }
    }
    /// A `BW::string`: a packed length then the bytes.
    fn string(&mut self) -> Option<String> {
        let len = self.packed_len()?;
        let bytes = self.take(len)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// The stream size of the header block (`streamSize`,
/// `replay_header.cpp:210-220`).
pub fn header_stream_size(signature_length: u16) -> usize {
    4 + 16 + 1 + 4 + NONCE_SIZE + 2 + signature_length as usize + 4
}

/// `ReplayHeader::checkSufficientLength` (`replay_header.cpp:276-297`): the
/// total header size, if the fragment is long enough to read `signatureLength`.
pub fn check_sufficient_header(data: &[u8]) -> Option<usize> {
    let offset = 4 + 16 + 1 + 4 + NONCE_SIZE; // signatureLength field offset
    if data.len() < offset + 2 {
        return None;
    }
    let sig_len = u16::from_le_bytes([data[offset], data[offset + 1]]);
    Some(header_stream_size(sig_len))
}

/// `ReplayHeader::initFromStream` (`replay_header.cpp:140-204`).
///
/// Returns the parsed header and the number of bytes consumed. When
/// `scheme` is `Some`, the signature is verified and a mismatch is
/// `ReplayError::ChecksumMismatch`; when `None`, the signature is read
/// unverified.
pub fn parse_header(
    data: &[u8],
    scheme: Option<&dyn ChecksumScheme>,
) -> Result<(ReplayHeader, usize), ReplayError> {
    let mut c = Cursor::new(data);

    let version = (
        c.u8().ok_or(ReplayError::InsufficientHeader)?,
        c.u8().ok_or(ReplayError::InsufficientHeader)?,
        c.u8().ok_or(ReplayError::InsufficientHeader)?,
        c.u8().ok_or(ReplayError::InsufficientHeader)?,
    );
    if version != CURRENT_VERSION {
        return Err(ReplayError::VersionMismatch);
    }

    let mut digest = [0u8; 16];
    digest.copy_from_slice(c.take(16).ok_or(ReplayError::InsufficientHeader)?);

    let game_update_frequency = c.u8().ok_or(ReplayError::InsufficientHeader)?;
    let timestamp = c.u32().ok_or(ReplayError::InsufficientHeader)?;

    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(c.take(NONCE_SIZE).ok_or(ReplayError::InsufficientHeader)?);

    let signature_length = c.u16().ok_or(ReplayError::InsufficientHeader)?;

    // The bytes the checksum is computed over: everything up to and including
    // the signatureLength field.
    let checksummed = data[..c.pos].to_vec();
    let signature_bytes = c
        .take(signature_length as usize)
        .ok_or(ReplayError::InsufficientHeader)?;

    if let Some(scheme) = scheme
        && (scheme.stream_size() != signature_length as usize
            || !scheme.verify(&checksummed, signature_bytes))
    {
        return Err(ReplayError::ChecksumMismatch);
    }

    let num_ticks = c.u32().ok_or(ReplayError::InsufficientHeader)?;

    Ok((
        ReplayHeader {
            version,
            digest,
            game_update_frequency,
            timestamp,
            nonce,
            signature_length,
            signature: signature_bytes.to_vec(),
            num_ticks,
        },
        c.pos,
    ))
}

/// `ReplayMetaData::readFromStream` (`replay_metadata.cpp:78-145`).
///
/// Reads `streamSize`, the key/value collection, then the signature (of
/// `signature_length` bytes, verified when `scheme` is `Some`).
pub fn parse_metadata(
    data: &[u8],
    signature_length: usize,
    scheme: Option<&dyn ChecksumScheme>,
) -> Result<(ReplayMetaData, usize), ReplayError> {
    let mut c = Cursor::new(data);
    let stream_size = c.u32().ok_or(ReplayError::MetadataTooLarge)?;
    if stream_size as usize > c.remaining() {
        return Err(ReplayError::MetadataTooLarge);
    }
    let collection_bytes = c
        .take(stream_size as usize)
        .ok_or(ReplayError::MetadataTooLarge)?;

    // The checksum covers streamSize + the collection bytes.
    let checksummed = data[..c.pos].to_vec();

    let mut entries = Vec::new();
    let mut sub = Cursor::new(collection_bytes);
    while sub.remaining() > 0 {
        let key = sub.string().ok_or(ReplayError::MetadataCorrupted)?;
        let value = sub.string().ok_or(ReplayError::MetadataCorrupted)?;
        entries.push((key, value));
    }

    let signature = c
        .take(signature_length)
        .ok_or(ReplayError::MetadataCorrupted)?;
    if let Some(scheme) = scheme
        && (scheme.stream_size() != signature_length || !scheme.verify(&checksummed, signature))
    {
        return Err(ReplayError::ChecksumMismatch);
    }

    Ok((
        ReplayMetaData {
            entries,
            stream_size,
            signature: signature.to_vec(),
        },
        c.pos,
    ))
}

/// A no-op checksum scheme for tests: an empty signature, always verified.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoChecksum;

impl ChecksumScheme for NoChecksum {
    fn stream_size(&self) -> usize {
        0
    }
    fn verify(&self, _data: &[u8], _signature: &[u8]) -> bool {
        true
    }
}

/// A deterministic checksum scheme for tests: a single signature byte equal to
/// `data.len() as u8`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ByteLenChecksum;

impl ChecksumScheme for ByteLenChecksum {
    fn stream_size(&self) -> usize {
        1
    }
    fn verify(&self, data: &[u8], signature: &[u8]) -> bool {
        signature.first() == Some(&(data.len() as u8))
    }
}

/// Compression types (`BWCompressionType`, `lib/network/compression_type.hpp`):
/// `BW_COMPRESSION_NONE` = 0, `BW_COMPRESSION_ZIP_1..9` = 1..9.
pub const COMPRESSION_NONE: u8 = 0;
/// `BW_COMPRESSION_ZIP_BEST_SPEED` (`BW_COMPRESSION_ZIP_1`).
pub const COMPRESSION_ZIP_MIN: u8 = 1;
/// `BW_COMPRESSION_ZIP_BEST_COMPRESSION` (`BW_COMPRESSION_ZIP_9`).
pub const COMPRESSION_ZIP_MAX: u8 = 9;

/// `ServerConnection::NO_POSITION` (`server_connection.cpp:71`): the y a
/// ground-volatile update stores when the entity is on the ground.
pub const NO_POSITION: f32 = -13000.0;

/// A tick-data block type (`ReplayData` block enum, `replay_data.hpp:16-28`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    EntityCreate,
    EntityVolatile,
    EntityVolatileOnGround,
    EntityMethod,
    EntityPropertyChange,
    EntityDelete,
    EntityPlayerStateChange,
    PlayerAoiChange,
    SpaceData,
    Final,
}

impl BlockType {
    /// The block type for a wire byte; `None` when the byte names no block
    /// (`processTick`'s "Invalid block type %d" default case).
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::EntityCreate),
            1 => Some(Self::EntityVolatile),
            2 => Some(Self::EntityVolatileOnGround),
            3 => Some(Self::EntityMethod),
            4 => Some(Self::EntityPropertyChange),
            5 => Some(Self::EntityDelete),
            6 => Some(Self::EntityPlayerStateChange),
            7 => Some(Self::PlayerAoiChange),
            8 => Some(Self::SpaceData),
            9 => Some(Self::Final),
            _ => None,
        }
    }
    /// `BLOCK_TYPE_FINAL` terminates the block loop (`processTick` `:1052-55`).
    pub fn is_final(self) -> bool {
        matches!(self, Self::Final)
    }
}

/// One tick entry of a chunk payload: the game time and the (possibly
/// compressed) tick-data blob — `[GameTime][TickDataLength][data]`
/// (`readTickData`, `replay_data_file_reader.cpp:860-905`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickEntry {
    pub game_time: u32,
    pub blob: Vec<u8>,
}

/// One signed tick chunk: `[SignedChunkLength][payload][signature]`
/// (`readNextChunkLength`/`readNextChunk`, `replay_data_file_reader.cpp:722-837`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickChunk {
    /// The tick entries in the chunk payload.
    pub entries: Vec<TickEntry>,
    /// The chunk signature (`header_.reportedSignatureLength()` bytes).
    pub signature: Vec<u8>,
}

/// Parse the signed tick chunks from the data after the header and metadata
/// blocks, applying the reference's value checks
/// (`ReplayDataFileReader::Impl::readNextChunkLength`, `readNextChunk` and
/// `readTickData`, `replay_data_file_reader.cpp:716-948`):
///
/// * a zero `chunkLength` is "Got zero chunk length";
/// * each payload entry is `[gameTime u32][dataLength u32][dataLength bytes]`;
///   a truncated entry header is "Could not read game time data header";
/// * `gameTime` must be `lastGameTime + 1` once a first tick was read
///   ("Game time mismatch"; the reference's `lastGameTime_ != 0` guard);
/// * once `numTicks` ticks were read, another entry is "Extraneous tick data";
/// * a trailing incomplete chunk is not an error — the streaming reader simply
///   waits for more data (`readNextChunkLength` returns false).
///
/// Returns the parsed chunks and the number of bytes consumed.
pub fn parse_tick_chunks(
    data: &[u8],
    signature_length: usize,
    num_ticks: u32,
) -> Result<(Vec<TickChunk>, usize), ReplayError> {
    let mut chunks = Vec::new();
    let mut c = Cursor::new(data);
    let mut last_game_time: u32 = 0;
    let mut num_ticks_read: u32 = 0;
    // The end of the last complete chunk (the streaming reader's numBytesRead_
    // only counts fully parsed chunks).
    let mut consumed: usize = 0;

    loop {
        if c.remaining() < 4 {
            break; // no (complete) chunkLength field left
        }
        let chunk_length = c.u32().ok_or(ReplayError::TickDataHeader)? as usize;
        if chunk_length == 0 {
            return Err(ReplayError::ZeroChunkLength);
        }
        if c.remaining() < chunk_length + signature_length {
            break; // incomplete chunk: wait for more data
        }
        let payload = c.take(chunk_length).ok_or(ReplayError::TickDataHeader)?;
        let signature = c
            .take(signature_length)
            .ok_or(ReplayError::TickDataHeader)?;

        let mut entries = Vec::new();
        let mut p = Cursor::new(payload);
        while p.remaining() > 0 {
            let game_time = p.u32().ok_or(ReplayError::TickDataHeader)?;
            let data_length = p.u32().ok_or(ReplayError::TickDataHeader)? as usize;
            if data_length > p.remaining() {
                return Err(ReplayError::TickDataHeader);
            }
            if last_game_time != 0 && game_time != last_game_time.wrapping_add(1) {
                return Err(ReplayError::GameTimeMismatch);
            }
            if num_ticks != 0 && num_ticks_read == num_ticks {
                return Err(ReplayError::ExtraneousTickData);
            }
            let blob = p.take(data_length).ok_or(ReplayError::TickDataHeader)?;
            last_game_time = game_time;
            num_ticks_read += 1;
            entries.push(TickEntry {
                game_time,
                blob: blob.to_vec(),
            });
        }

        chunks.push(TickChunk {
            entries,
            signature: signature.to_vec(),
        });
        consumed = c.pos;
    }

    Ok((chunks, consumed))
}

/// Decompress one tick-data blob, `CompressionIStream`
/// (`lib/network/compression_stream.cpp:19-51`):
///
/// * byte 0 is the compression type — `BW_COMPRESSION_NONE` (0) passes the
///   remaining bytes through raw; `BW_COMPRESSION_ZIP_1..9` (1..9) is a
///   `ZipIStream` (`lib/network/zip_stream.cpp:124-184`): a packed
///   decompressed length, a packed compressed length, then that many
///   zlib-compressed bytes inflated by `uncompress`;
/// * any other type is "Invalid compression type: %d";
/// * a stream that fails to inflate — or inflates past the declared
///   decompressed size (`uncompress`'s `Z_BUF_ERROR`) — is a corrupted block.
pub fn decompress_tick(blob: &[u8]) -> Result<Vec<u8>, ReplayError> {
    decompress_tick_consumed(blob).map(|(data, _)| data)
}

/// Like [`decompress_tick`], but also returns how many bytes of `blob` the
/// compression header and payload occupy. For `BW_COMPRESSION_NONE` the blob is
/// `[NONE][data]` and the raw data is the whole remainder, so the whole blob is
/// consumed; for a zip type the consumed size is the header plus the declared
/// compressed length. This is what an entity-create's nested `CompressionIStream`
/// needs to bound its block (`handleEntityCreate`, `replay_controller.cpp:1081-1116`).
fn decompress_tick_consumed(blob: &[u8]) -> Result<(Vec<u8>, usize), ReplayError> {
    let (&compression_type, rest) = blob.split_first().ok_or(ReplayError::DecompressionFailed)?;
    if compression_type == COMPRESSION_NONE {
        return Ok((rest.to_vec(), blob.len()));
    }
    if !(COMPRESSION_ZIP_MIN..=COMPRESSION_ZIP_MAX).contains(&compression_type) {
        return Err(ReplayError::InvalidCompressionType(compression_type));
    }

    let mut c = Cursor::new(rest);
    let orig_len = c.packed_len().ok_or(ReplayError::DecompressionFailed)?;
    let compress_len = c.packed_len().ok_or(ReplayError::DecompressionFailed)?;
    let compressed = c
        .take(compress_len)
        .ok_or(ReplayError::DecompressionFailed)?;

    let mut out = Vec::with_capacity(orig_len);
    use std::io::Read;
    let mut decoder = flate2::read::ZlibDecoder::new(compressed);
    decoder
        .read_to_end(&mut out)
        .map_err(|_| ReplayError::DecompressionFailed)?;
    if out.len() > orig_len {
        // uncompress would have overflowed its origLen-sized buffer.
        return Err(ReplayError::DecompressionFailed);
    }
    // 1 byte compression type + packed origLen + packed compressLen + payload.
    Ok((out, 1 + c.pos))
}

/// Scan the block types of a decompressed tick, `ReplayController::processTick`
/// (`replay_controller.cpp:1001-1062`): read a `BlockType` byte while more
/// than one byte remains, error on an unknown type, and stop at `FINAL`.
///
/// The per-block field layouts are read by the reference's `handle*` methods;
/// this layer treats block data as opaque, so the caller supplies
/// `block_data_len`, which returns how many bytes the block's data occupies in
/// the remaining slice (the entity-state reconstruction supplies the real
/// per-block reader).
pub fn scan_block_types(
    decompressed: &[u8],
    block_data_len: impl Fn(u8, &[u8]) -> Result<usize, ReplayError>,
) -> Result<Vec<BlockType>, ReplayError> {
    let mut types = Vec::new();
    let mut c = Cursor::new(decompressed);
    while c.remaining() > 1 {
        // `while (remainingLength() > sizeof(BlockType))`, `:1003-1006`.
        let byte = c.u8().ok_or(ReplayError::InvalidBlockType(0))?;
        let block_type = BlockType::from_u8(byte).ok_or(ReplayError::InvalidBlockType(byte))?;
        if block_type.is_final() {
            break;
        }
        let remaining = &decompressed[c.pos..];
        let len = block_data_len(byte, remaining)?;
        if len > remaining.len() {
            return Err(ReplayError::BlockDataOverrun);
        }
        c.pos += len;
        types.push(block_type);
    }
    Ok(types)
}

/// A parsed tick block — the field layouts the reference's `ReplayController`
/// `handle*` methods read (`replay_controller.cpp:1080-1372`).
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayBlock {
    /// `handleEntityCreate` (`:1081-1116`): `[vehicleID i32]` then a nested
    /// `CompressionIStream` blob decompressing to `[id i32][type u16][pos
    /// 3×f32][yaw f32][pitch f32][roll f32][properties…]`.
    EntityCreate {
        /// `vehicleID` (the ID of the vehicle the entity is riding).
        vehicle_id: i32,
        /// `id`, the new entity's ID.
        id: i32,
        /// `type`, the entity type index.
        type_id: u16,
        /// `pos` (x, y, z).
        position: [f32; 3],
        /// `dir.yaw`, `dir.pitch`, `dir.roll`.
        direction: [f32; 3],
        /// The remaining entity properties, opaque (entity-def-driven).
        properties: Vec<u8>,
    },
    /// `handleEntityVolatile` (`:1125-1143`): `[entityID i32][pos 3×f32][dir
    /// 3×f32][vehicle i32]`.
    EntityVolatile {
        entity_id: i32,
        position: [f32; 3],
        direction: [f32; 3],
        vehicle_id: i32,
    },
    /// `handleEntityVolatileOnGround` (`:1148-1170`): `[entityID i32][x f32]
    /// [z f32][dir 3×f32][vehicle i32]`; the reconstructed y is
    /// `ServerConnection::NO_POSITION`.
    EntityVolatileOnGround {
        entity_id: i32,
        x: f32,
        z: f32,
        direction: [f32; 3],
        vehicle_id: i32,
    },
    /// `handleEntityMethod` (`:1178-1200`): `[entityID i32][messageID u8]
    /// [streamSize i32][streamSize bytes]`.
    EntityMethod {
        entity_id: i32,
        message_id: u8,
        data: Vec<u8>,
    },
    /// `handleEntityPropertyChange` (`:1207-1226`): `[entityID i32][isSlice
    /// u8][streamSize i32][streamSize bytes]`.
    EntityPropertyChange {
        entity_id: i32,
        is_slice: bool,
        data: Vec<u8>,
    },
    /// `handleEntityPlayerStateChange` (`:1231-1277`): `[playerID i32]
    /// [hasBecomePlayer u8]` then, when becoming a player,
    /// `[numInAoI u32][numInAoI × entityID i32]`.
    EntityPlayerStateChange {
        player_id: i32,
        has_become_player: bool,
        /// The `numInAoI` entity IDs when becoming a player.
        aoi: Vec<i32>,
    },
    /// `handleEntityAoIChange` (`:1280-1304`): `[playerID i32][entityID i32]
    /// [hasEnteredAoI u8]`.
    PlayerAoiChange {
        player_id: i32,
        entity_id: i32,
        has_entered_aoi: bool,
    },
    /// `handleEntityDelete` (`:1307-1320`): `[entityID i32]`.
    EntityDelete { entity_id: i32 },
    /// `handleSpaceData` (`:1323-1344`): `[spaceEntryID (ip u32, port u16)]
    /// [key u16][value string]`.
    SpaceData {
        /// `(ip, port)`, the `Mercury::Address` layout of `SpaceEntryID`.
        entry_id: (u32, u16),
        key: u16,
        value: String,
    },
    /// `handleFinal` (`:1349-1353`): the end-of-replay marker.
    Final,
}

/// `ReplayController::processTick` (`replay_controller.cpp:1001-1062`): parse
/// the block sequence of one decompressed tick, reading each block's field
/// layout. `EntityCreate`'s properties are opaque (the reference reads them
/// entity-def-driven via the `onEntityCreate` stream); the nested
/// `CompressionIStream` bounds them.
pub fn parse_blocks(decompressed: &[u8]) -> Result<Vec<ReplayBlock>, ReplayError> {
    let mut blocks = Vec::new();
    let mut c = Cursor::new(decompressed);
    while c.remaining() > 1 {
        // `while (remainingLength() > sizeof(BlockType))`, `:1003-1006`.
        let byte = c.u8().ok_or(ReplayError::InvalidBlockType(0))?;
        let block_type = BlockType::from_u8(byte).ok_or(ReplayError::InvalidBlockType(byte))?;
        let block = match block_type {
            BlockType::EntityCreate => {
                let vehicle_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let (payload, consumed) = decompress_tick_consumed(&decompressed[c.pos..])?;
                c.pos += consumed;
                let mut p = Cursor::new(&payload);
                let id = p.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let type_id = p.u16().ok_or(ReplayError::BlockDataOverrun)?;
                let position = [
                    p.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    p.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    p.f32().ok_or(ReplayError::BlockDataOverrun)?,
                ];
                let direction = [
                    p.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    p.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    p.f32().ok_or(ReplayError::BlockDataOverrun)?,
                ];
                let properties = payload[p.pos..].to_vec();
                ReplayBlock::EntityCreate {
                    vehicle_id,
                    id,
                    type_id,
                    position,
                    direction,
                    properties,
                }
            }
            BlockType::EntityVolatile => {
                let entity_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let position = [
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                ];
                let direction = [
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                ];
                let vehicle_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                ReplayBlock::EntityVolatile {
                    entity_id,
                    position,
                    direction,
                    vehicle_id,
                }
            }
            BlockType::EntityVolatileOnGround => {
                let entity_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let x = c.f32().ok_or(ReplayError::BlockDataOverrun)?;
                let z = c.f32().ok_or(ReplayError::BlockDataOverrun)?;
                let direction = [
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                    c.f32().ok_or(ReplayError::BlockDataOverrun)?,
                ];
                let vehicle_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                ReplayBlock::EntityVolatileOnGround {
                    entity_id,
                    x,
                    z,
                    direction,
                    vehicle_id,
                }
            }
            BlockType::EntityMethod => {
                let entity_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let message_id = c.u8().ok_or(ReplayError::BlockDataOverrun)?;
                let stream_size = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                if stream_size < 0 {
                    return Err(ReplayError::BlockDataOverrun);
                }
                let data = c
                    .take(stream_size as usize)
                    .ok_or(ReplayError::BlockDataOverrun)?
                    .to_vec();
                ReplayBlock::EntityMethod {
                    entity_id,
                    message_id,
                    data,
                }
            }
            BlockType::EntityPropertyChange => {
                let entity_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let is_slice = c.u8().ok_or(ReplayError::BlockDataOverrun)?;
                let stream_size = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                if stream_size < 0 {
                    return Err(ReplayError::BlockDataOverrun);
                }
                let data = c
                    .take(stream_size as usize)
                    .ok_or(ReplayError::BlockDataOverrun)?
                    .to_vec();
                ReplayBlock::EntityPropertyChange {
                    entity_id,
                    is_slice: is_slice == 1,
                    data,
                }
            }
            BlockType::EntityPlayerStateChange => {
                let player_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let has_become_player = c.u8().ok_or(ReplayError::BlockDataOverrun)? != 0;
                let mut aoi = Vec::new();
                if has_become_player {
                    let num_in_aoi = c.u32().ok_or(ReplayError::BlockDataOverrun)?;
                    for _ in 0..num_in_aoi {
                        aoi.push(c.i32().ok_or(ReplayError::BlockDataOverrun)?);
                    }
                }
                ReplayBlock::EntityPlayerStateChange {
                    player_id,
                    has_become_player,
                    aoi,
                }
            }
            BlockType::PlayerAoiChange => {
                let player_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let entity_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                let has_entered_aoi = c.u8().ok_or(ReplayError::BlockDataOverrun)? != 0;
                ReplayBlock::PlayerAoiChange {
                    player_id,
                    entity_id,
                    has_entered_aoi,
                }
            }
            BlockType::EntityDelete => {
                let entity_id = c.i32().ok_or(ReplayError::BlockDataOverrun)?;
                ReplayBlock::EntityDelete { entity_id }
            }
            BlockType::SpaceData => {
                let ip = c.u32().ok_or(ReplayError::BlockDataOverrun)?;
                let port = c.u16().ok_or(ReplayError::BlockDataOverrun)?;
                let key = c.u16().ok_or(ReplayError::BlockDataOverrun)?;
                let value = c.string().ok_or(ReplayError::BlockDataOverrun)?;
                ReplayBlock::SpaceData {
                    entry_id: (ip, port),
                    key,
                    value,
                }
            }
            BlockType::Final => ReplayBlock::Final,
        };
        let is_final = block_type.is_final();
        blocks.push(block);
        if is_final {
            break;
        }
    }
    Ok(blocks)
}

/// The reconstructed state of one entity, as the server's entity state after
/// the create/volatile/delete/player-state blocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityState {
    /// The latest position (x, y, z); `NO_POSITION` in y for on-ground.
    pub position: [f32; 3],
    /// yaw, pitch, roll.
    pub direction: [f32; 3],
    /// The vehicle the entity is riding, or 0 when on the ground (`NULL_ENTITY_ID`).
    pub vehicle_id: i32,
    /// Whether the entity is currently a player.
    pub is_player: bool,
}

/// The entity state reconstruction, the `onEntityCreate`/`onEntityMove`/
/// `onEntityLeave` side of the replay handler: applying the create, volatile,
/// on-ground, delete and player-state blocks to a map keyed by entity id.
#[derive(Debug, Clone, Default)]
pub struct ReplayWorld {
    entities: std::collections::BTreeMap<i32, EntityState>,
}

impl ReplayWorld {
    pub fn new() -> Self {
        Self::default()
    }
    /// `handleEntityCreate`/`handleEntityVolatile`/… → the entity map.
    pub fn apply(&mut self, block: &ReplayBlock) {
        match block {
            ReplayBlock::EntityCreate {
                id,
                position,
                direction,
                vehicle_id,
                ..
            } => {
                self.entities.insert(
                    *id,
                    EntityState {
                        position: *position,
                        direction: *direction,
                        vehicle_id: *vehicle_id,
                        is_player: false,
                    },
                );
            }
            ReplayBlock::EntityVolatile {
                entity_id,
                position,
                direction,
                vehicle_id,
            } => {
                if let Some(e) = self.entities.get_mut(entity_id) {
                    e.position = *position;
                    e.direction = *direction;
                    e.vehicle_id = *vehicle_id;
                }
            }
            ReplayBlock::EntityVolatileOnGround {
                entity_id,
                x,
                z,
                direction,
                vehicle_id,
            } => {
                if let Some(e) = self.entities.get_mut(entity_id) {
                    e.position = [*x, NO_POSITION, *z];
                    e.direction = *direction;
                    e.vehicle_id = *vehicle_id;
                }
            }
            ReplayBlock::EntityPlayerStateChange {
                player_id,
                has_become_player,
                ..
            } => {
                if let Some(e) = self.entities.get_mut(player_id) {
                    e.is_player = *has_become_player;
                }
            }
            ReplayBlock::EntityDelete { entity_id } => {
                self.entities.remove(entity_id);
            }
            // Method / property / AoI / space-data / final do not change the
            // reconstructed position state.
            _ => {}
        }
    }
    /// `onEntitiesReset` — a rewind clears the world before the replay.
    pub fn reset(&mut self) {
        self.entities.clear();
    }
    /// The current state of an entity, or `None` when it is not present.
    pub fn get(&self, id: i32) -> Option<&EntityState> {
        self.entities.get(&id)
    }
    pub fn entities(&self) -> &std::collections::BTreeMap<i32, EntityState> {
        &self.entities
    }
}

/// The replay handler notifications the player emits (`ReplayControllerHandler`,
/// `replay_controller.hpp:226-242`). A no-op default keeps the player usable
/// without a handler; the real client handler is a later item.
pub trait ReplayHandler {
    /// `onReplayControllerPostTick(*this, currentTick_)`
    /// (`replay_controller.cpp:750`).
    fn on_post_tick(&mut self, _tick: u32) {}
    /// `onReplayControllerIncreaseTotalTime(*this, 1.0/frequency)`, called once
    /// per tick processed while seeking (`:736-740`).
    fn on_increase_total_time(&mut self, _delta: f64) {}
    /// `onReplayControllerFinish(*this)` (`handleFinal`, `:1384-1386`) — the
    /// end-of-replay event, fired exactly once per playback segment.
    fn on_finish(&mut self) {}
}

impl ReplayHandler for () {}

/// One loaded tick: the game time and the decompressed block data.
#[derive(Debug, Clone)]
pub struct Tick {
    /// The tick's game time (`GameTime`).
    pub game_time: u32,
    /// The decompressed tick data (the `processTick` block stream).
    pub data: Vec<u8>,
}

/// The playback controller, `ReplayController` (`replay_controller.cpp`):
/// steps through loaded ticks at the game-update rate and supports seeking to
/// a target tick (`setCurrentTick`, `:500-518`).
///
/// The port loads all ticks up front (the reference streams them through a
/// tick loader), so a backward seek rewinds the world and replays from tick 0,
/// and a forward seek fast-forwards — the same `tick()` stepping
/// (`:658-777`) with `numTicksLoaded_` fixed at the remaining count.
///
/// BW-11-03-04 adds the end-of-replay event: `handleFinal`
/// (`:1384-1386`) fires `onReplayControllerFinish` from both the `FINAL`
/// block (`processTick`, `:1067-1069`) and the all-ticks-consumed condition
/// (`tick`, `:770-776`). The port latches it on `finished` so the finish
/// event fires exactly once per playback segment, and honours `isReceiving_`
/// (`!isReceiving_` in the condition) so a downloading replay does not
/// finish early.
pub struct ReplayPlayer<H: ReplayHandler = ()> {
    ticks: Vec<Tick>,
    game_update_frequency: u8,
    first_game_time: u32,
    /// The number of ticks processed (the next tick index).
    current_tick: u32,
    /// The seek target; `None` when not seeking.
    seek_to: Option<u32>,
    time_since_last_tick: f64,
    /// Whether playback is enabled (the caller started the stream).
    playing: bool,
    /// Whether the stream is still receiving (`isReceiving_`); a receiving
    /// replay does not fire the finish event.
    receiving: bool,
    world: ReplayWorld,
    handler: H,
    /// The `onReplayControllerFinish` latch — the finish event fires exactly
    /// once, on the first `tick()` that meets the end condition.
    finished: bool,
}

impl<H: ReplayHandler> ReplayPlayer<H> {
    /// `ReplayController` with all ticks loaded and a first game time.
    pub fn new(
        ticks: Vec<Tick>,
        first_game_time: u32,
        game_update_frequency: u8,
        handler: H,
    ) -> Self {
        Self {
            ticks,
            game_update_frequency,
            first_game_time,
            current_tick: 0,
            seek_to: None,
            time_since_last_tick: 0.0,
            playing: false,
            receiving: false,
            world: ReplayWorld::new(),
            handler,
            finished: false,
        }
    }

    /// `isPlayingStream()` / start playback (`isPlayingStream_ = true`).
    pub fn play(&mut self) {
        self.playing = true;
    }
    pub fn pause(&mut self) {
        self.playing = false;
    }
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// `isReceiving()` / `isReceiving_` — a receiving (downloading) replay
    /// does not fire the finish event (`tick`, `:772`).
    pub fn set_receiving(&mut self, receiving: bool) {
        self.receiving = receiving;
    }
    pub fn is_receiving(&self) -> bool {
        self.receiving
    }

    /// `setCurrentTick(newTick)` (`replay_controller.cpp:500-518`): seek to a
    /// target tick. A backward seek (target below the current tick) rewinds:
    /// the world is reset and playback restarts from tick 0, then fast-forwards
    /// to the target (the reference prepends the earlier ticks and calls
    /// `resetEntities`).
    pub fn set_current_tick(&mut self, target: u32) {
        if target < self.current_tick {
            self.world.reset();
            self.current_tick = 0;
            self.finished = false;
        }
        self.seek_to = Some(target);
    }

    /// `isSeekingToTick()`.
    pub fn is_seeking(&self) -> bool {
        self.seek_to.is_some()
    }

    /// `currentTick_`.
    pub fn current_tick(&self) -> u32 {
        self.current_tick
    }

    /// `getCurrentTickTime()` (`:1421-1429`): `firstGameTime_ + currentTick_`.
    pub fn current_tick_time(&self) -> u32 {
        self.first_game_time + self.current_tick
    }

    /// `numTicksLoaded_` — the remaining, not-yet-processed ticks.
    pub fn num_ticks_loaded(&self) -> u32 {
        self.ticks.len().saturating_sub(self.current_tick as usize) as u32
    }

    /// `numTicksTotal()` (`:1404-1420`): the total tick count.
    pub fn num_ticks_total(&self) -> u32 {
        self.ticks.len() as u32
    }

    /// The reconstructed world after the applied ticks.
    pub fn world(&self) -> &ReplayWorld {
        &self.world
    }

    /// Whether the replay has played through its last tick
    /// (`handleFinal` would have fired; the finish event is 03-04).
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// `tick(timeDelta)` (`replay_controller.cpp:658-777`): step the playback
    /// by a real-time delta. During a seek, up to the remaining loaded ticks
    /// are processed towards the target; otherwise the game-update rate
    /// converts the delta to a tick count (the first tick is forced).
    pub fn tick(&mut self, time_delta: f64) -> Result<(), ReplayError> {
        if !self.playing && !self.is_seeking() {
            return Ok(());
        }

        let ticks_left = if self.is_seeking() {
            let target = self.seek_to.expect("is_seeking implies a target");
            let remaining = self.ticks.len().saturating_sub(self.current_tick as usize) as u32;
            // `std::min(numTicksLoaded_, seekToTick_ - currentTick_)`, `:692`.
            self.time_since_last_tick = 0.0;
            remaining.min(target.saturating_sub(self.current_tick))
        } else if self.current_tick == 0 {
            // `currentTick_ == 0` forces one tick, `:697-701`.
            self.time_since_last_tick = 0.0;
            1
        } else {
            self.time_since_last_tick += time_delta;
            let ticks_left = (self.time_since_last_tick * self.game_update_frequency as f64) as u32;
            if ticks_left == 0 {
                return Ok(());
            }
            self.time_since_last_tick -=
                f64::from(ticks_left) / f64::from(self.game_update_frequency);
            ticks_left
        };

        let mut remaining = ticks_left;
        while (self.is_seeking() || self.playing)
            && remaining > 0
            && (self.current_tick as usize) < self.ticks.len()
        {
            let data = self.ticks[self.current_tick as usize].data.clone();
            let blocks = parse_blocks(&data)?;
            if self.is_seeking() {
                // `onReplayControllerIncreaseTotalTime(*this, 1.0/frequency)`
                // per seeked tick, `:736-740`.
                self.handler
                    .on_increase_total_time(1.0 / f64::from(self.game_update_frequency));
            }
            for block in &blocks {
                self.world.apply(block);
            }
            // `processTick`'s `BLOCK_TYPE_FINAL` case (`:1067-1069`) fires
            // `handleFinal` for the end-of-replay block (the `FINAL` block is
            // only read under the loop's playing/seeking gate).
            if blocks.iter().any(|b| matches!(b, ReplayBlock::Final)) {
                self.finish_once(self.playing || self.is_seeking());
            }
            self.handler.on_post_tick(self.current_tick);
            self.current_tick += 1;
            remaining -= 1;
        }

        if self.seek_to == Some(self.current_tick) {
            self.seek_to = None;
        }

        // `handleFinal` end condition (`:770-776`): no tick left to read,
        // playing, not receiving, and all ticks consumed.
        if self.playing && !self.receiving && self.num_ticks_total() <= self.current_tick {
            self.finish_once(true);
        }

        Ok(())
    }

    /// `handleFinal` (`replay_controller.cpp:1384-1386`): fire
    /// `onReplayControllerFinish` at most once per playback segment. `allowed`
    /// is the call-site condition (the `FINAL` block path is gated on
    /// playing/seeking, the end-condition path on
    /// `playing && !receiving && numTicksTotal() <= currentTick_`).
    fn finish_once(&mut self, allowed: bool) {
        if allowed && !self.finished {
            self.finished = true;
            self.handler.on_finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a header byte buffer from fields, matching `addToStream`.
    fn header_bytes(
        version: (u8, u8, u8, u8),
        digest: [u8; 16],
        freq: u8,
        timestamp: u32,
        nonce: [u8; 8],
        signature: &[u8],
        num_ticks: u32,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&[version.0, version.1, version.2, version.3]);
        b.extend_from_slice(&digest);
        b.push(freq);
        b.extend_from_slice(&timestamp.to_le_bytes());
        b.extend_from_slice(&nonce);
        b.extend_from_slice(&(signature.len() as u16).to_le_bytes());
        b.extend_from_slice(signature);
        b.extend_from_slice(&num_ticks.to_le_bytes());
        b
    }

    /// A known recording header (current version, fixed fields). The
    /// `ByteLenChecksum` signature byte is the checksummed-prefix length:
    /// 4 (version) + 16 (digest) + 1 (freq) + 4 (timestamp) + 8 (nonce) + 2
    /// (signatureLength field) = 35.
    fn known_header() -> Vec<u8> {
        header_bytes(
            CURRENT_VERSION,
            [0x11; 16],
            20,
            1_700_000_000,
            [0x22; 8],
            &[35],
            12_345,
        )
    }

    /// The acceptance: parsing a known recording header reads out the same
    /// fields — version, digest, frequency, timestamp, nonce, signature
    /// length, signature and numTicks.
    #[test]
    fn a_known_header_reads_out_the_same_fields() {
        let (header, consumed) = parse_header(&known_header(), Some(&ByteLenChecksum))
            .expect("a known recording header parses");
        assert_eq!(header.version, CURRENT_VERSION);
        assert_eq!(header.digest, [0x11; 16]);
        assert_eq!(header.game_update_frequency, 20);
        assert_eq!(header.timestamp, 1_700_000_000);
        assert_eq!(header.nonce, [0x22; 8]);
        assert_eq!(header.signature_length, 1);
        assert_eq!(header.signature, vec![35]);
        assert_eq!(header.num_ticks, 12_345);
        assert_eq!(consumed, header_stream_size(1));
    }

    /// A version other than the current one fails with "version mismatch".
    #[test]
    fn an_old_version_is_rejected() {
        let bytes = header_bytes((2, 8, 0, 0), [0; 16], 20, 0, [0; 8], &[], 0);
        assert_eq!(
            parse_header(&bytes, None),
            Err(ReplayError::VersionMismatch)
        );
    }

    /// A fragment too short for the header fails cleanly.
    #[test]
    fn a_short_fragment_is_insufficient() {
        let bytes = known_header();
        for len in 0..bytes.len() {
            assert!(
                parse_header(&bytes[..len], None).is_err(),
                "a {len}-byte fragment must fail"
            );
        }
    }

    /// `checkSufficientLength` reports the header size once the
    /// `signatureLength` field is present.
    #[test]
    fn check_sufficient_length_reports_the_header_size() {
        let bytes = known_header();
        assert_eq!(check_sufficient_header(&bytes), Some(header_stream_size(1)));
        assert_eq!(check_sufficient_header(&bytes[..10]), None);
    }

    /// A signature that fails the scheme's verify is a checksum mismatch.
    #[test]
    fn a_bad_signature_is_rejected() {
        let bytes = header_bytes(
            CURRENT_VERSION,
            [0; 16],
            20,
            0,
            [0; 8],
            &[0x00], // 0 != data.len() as u8
            0,
        );
        assert_eq!(
            parse_header(&bytes, Some(&ByteLenChecksum)),
            Err(ReplayError::ChecksumMismatch)
        );
    }

    /// Build a metadata block from key/value pairs.
    fn metadata_bytes(pairs: &[(&str, &str)]) -> (Vec<u8>, u32) {
        let mut body = Vec::new();
        for (k, v) in pairs {
            body.push(k.len() as u8);
            body.extend_from_slice(k.as_bytes());
            body.push(v.len() as u8);
            body.extend_from_slice(v.as_bytes());
        }
        let mut b = Vec::new();
        b.extend_from_slice(&(body.len() as u32).to_le_bytes());
        b.extend_from_slice(&body);
        (b, body.len() as u32)
    }

    /// The metadata block reads out the key/value collection.
    #[test]
    fn a_metadata_block_reads_out_the_same_fields() {
        let (mut bytes, stream_size) = metadata_bytes(&[
            ("version", "2"),
            ("mapName", "testmap"),
            ("gameTime", "123.5"),
        ]);
        bytes.push(45); // signature: 4 + 10 + 16 + 15 (ByteLenChecksum)
        let (meta, consumed) =
            parse_metadata(&bytes, 1, Some(&ByteLenChecksum)).expect("a metadata block parses");
        assert_eq!(meta.stream_size, stream_size);
        assert_eq!(
            meta.entries,
            vec![
                ("version".to_string(), "2".to_string()),
                ("mapName".to_string(), "testmap".to_string()),
                ("gameTime".to_string(), "123.5".to_string()),
            ]
        );
        assert_eq!(meta.signature, vec![45]);
        assert_eq!(consumed, bytes.len());
    }

    /// A metadata block whose declared size exceeds the available data fails.
    #[test]
    fn an_oversized_metadata_block_is_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&100u32.to_le_bytes());
        assert_eq!(
            parse_metadata(&bytes, 0, None),
            Err(ReplayError::MetadataTooLarge)
        );
    }

    /// `calculateStringPackedSize`: a small string has a 1-byte prefix.
    #[test]
    fn packed_string_size_matches_the_reference() {
        // A 1-byte length prefix + the bytes, for small strings.
        let (mut bytes, _) = metadata_bytes(&[("k", "v")]);
        assert_eq!(bytes.len(), 4 + 1 + 1 + 1 + 1);
        bytes.push(0x00);
        let (meta, _) = parse_metadata(&bytes, 0, None).expect("parses");
        assert_eq!(meta.entries, vec![("k".to_string(), "v".to_string())]);
    }

    /// One tick entry: `[gameTime u32][dataLength u32][blob]`.
    fn tick_entry_bytes(game_time: u32, blob: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&game_time.to_le_bytes());
        b.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        b.extend_from_slice(blob);
        b
    }

    /// One signed chunk: `[chunkLength u32][payload][signature]`.
    fn chunk_bytes(payload: &[u8], signature: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(payload);
        b.extend_from_slice(signature);
        b
    }

    /// A raw (uncompressed) tick blob: `[BW_COMPRESSION_NONE][data]`.
    fn raw_blob(data: &[u8]) -> Vec<u8> {
        let mut b = vec![COMPRESSION_NONE];
        b.extend_from_slice(data);
        b
    }

    /// A ZIP-compressed tick blob: `[ZIP_9][packed origLen][packed
    /// compressLen][zlib bytes]`, matching `ZipOStream`'s write.
    fn zip_blob(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(data).expect("compress");
        let compressed = enc.finish().expect("finish");
        assert!(
            data.len() < 255 && compressed.len() < 255,
            "test data small"
        );
        let mut b = vec![COMPRESSION_ZIP_MAX];
        b.push(data.len() as u8); // packed origLen
        b.push(compressed.len() as u8); // packed compressLen
        b.extend_from_slice(&compressed);
        b
    }

    /// A block = `[type][len][len bytes]`; the scan's `block_data_len` reads
    /// the length from the first byte of the block data.
    fn block(type_byte: u8, data: &[u8]) -> Vec<u8> {
        let mut b = vec![type_byte, data.len() as u8];
        b.extend_from_slice(data);
        b
    }

    /// The test `block_data_len`: the first data byte is the length.
    fn test_block_data_len(_type_byte: u8, data: &[u8]) -> Result<usize, ReplayError> {
        if data.is_empty() {
            return Err(ReplayError::BlockDataOverrun);
        }
        Ok(1 + data[0] as usize)
    }

    /// The acceptance: a correct tick frame decodes end to end — the chunk
    /// framing and tick value checks pass, the raw blob decompresses, and the
    /// block types scan through to `FINAL`.
    #[test]
    fn a_correct_frame_reads_end_to_end() {
        let blocks = [
            block(0, &[1, 2, 3]), // EntityCreate
            block(8, &[4, 5]),    // SpaceData
            block(9, &[]),        // Final
        ];
        let payload = tick_entry_bytes(100, &raw_blob(&blocks.concat()));
        let data = chunk_bytes(&payload, &[0x07]);
        let (chunks, consumed) = parse_tick_chunks(&data, 1, 1).expect("chunks parse");
        assert_eq!(chunks.len(), 1);
        assert_eq!(consumed, data.len());
        assert_eq!(chunks[0].signature, vec![0x07]);
        assert_eq!(chunks[0].entries.len(), 1);
        let entry = &chunks[0].entries[0];
        assert_eq!(entry.game_time, 100);
        let decompressed = decompress_tick(&entry.blob).expect("decompress");
        let types = scan_block_types(&decompressed, test_block_data_len).expect("blocks scan");
        assert_eq!(types, vec![BlockType::EntityCreate, BlockType::SpaceData]);
    }

    /// A zlib-compressed frame reads out the same blocks.
    #[test]
    fn a_zipped_frame_reads_out_the_same_blocks() {
        let blocks = [block(0, &[9]), block(9, &[])];
        let payload = tick_entry_bytes(7, &zip_blob(&blocks.concat()));
        let data = chunk_bytes(&payload, &[]);
        let (chunks, _) = parse_tick_chunks(&data, 0, 1).expect("chunks parse");
        let decompressed = decompress_tick(&chunks[0].entries[0].blob).expect("decompress");
        assert_eq!(decompressed, blocks.concat());
        let types = scan_block_types(&decompressed, test_block_data_len).expect("blocks scan");
        assert_eq!(types, vec![BlockType::EntityCreate]);
    }

    /// `readNextChunkLength`: "Got zero chunk length".
    #[test]
    fn a_zero_chunk_length_is_rejected() {
        let mut data = Vec::new();
        data.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            parse_tick_chunks(&data, 0, 0),
            Err(ReplayError::ZeroChunkLength)
        );
    }

    /// `readTickData`: a non-sequential game time is "Game time mismatch".
    #[test]
    fn a_game_time_gap_is_rejected() {
        let payload = [
            tick_entry_bytes(100, &raw_blob(&[9])),
            tick_entry_bytes(102, &raw_blob(&[9])), // gap: 100 then 102
        ]
        .concat();
        let data = chunk_bytes(&payload, &[]);
        assert_eq!(
            parse_tick_chunks(&data, 0, 0),
            Err(ReplayError::GameTimeMismatch)
        );
    }

    /// `readTickData`: an entry beyond the declared tick count is "Extraneous
    /// tick data".
    #[test]
    fn an_extraneous_tick_is_rejected() {
        let payload = [
            tick_entry_bytes(100, &raw_blob(&[9])),
            tick_entry_bytes(101, &raw_blob(&[9])),
        ]
        .concat();
        let data = chunk_bytes(&payload, &[]);
        assert_eq!(
            parse_tick_chunks(&data, 0, 1),
            Err(ReplayError::ExtraneousTickData),
            "one declared tick, two entries"
        );
    }

    /// `readTickData`: a truncated entry header is "Could not read game time
    /// data header".
    #[test]
    fn a_truncated_entry_header_is_rejected() {
        let mut payload = tick_entry_bytes(100, &raw_blob(&[9]));
        payload.extend_from_slice(&1u32.to_le_bytes()); // a game time, no length
        let data = chunk_bytes(&payload, &[]);
        assert_eq!(
            parse_tick_chunks(&data, 0, 0),
            Err(ReplayError::TickDataHeader)
        );
    }

    /// A trailing incomplete chunk is not an error — the streaming reader waits
    /// for more data, so the parsed prefix is returned.
    #[test]
    fn an_incomplete_trailing_chunk_is_not_an_error() {
        let full = chunk_bytes(&tick_entry_bytes(100, &raw_blob(&[9])), &[]);
        let mut data = full.clone();
        data.extend_from_slice(&100u32.to_le_bytes()); // a partial chunkLength
        let (chunks, consumed) = parse_tick_chunks(&data, 0, 0).expect("parses the prefix");
        assert_eq!(chunks.len(), 1);
        assert_eq!(consumed, full.len());
    }

    /// `BW_COMPRESSION_NONE` passes the remaining bytes through raw.
    #[test]
    fn raw_compression_passes_through() {
        assert_eq!(
            decompress_tick(&raw_blob(&[1, 2, 3])).expect("raw"),
            vec![1, 2, 3]
        );
    }

    /// An unknown compression type is "Invalid compression type".
    #[test]
    fn an_invalid_compression_type_is_rejected() {
        assert_eq!(
            decompress_tick(&[42, 1, 2, 3]),
            Err(ReplayError::InvalidCompressionType(42))
        );
    }

    /// A corrupt zlib stream fails to inflate.
    #[test]
    fn a_corrupt_zlib_stream_is_rejected() {
        let mut blob = vec![COMPRESSION_ZIP_MAX, 4, 3];
        blob.extend_from_slice(&[0xDE, 0xAD, 0xBE]); // not a zlib stream
        assert_eq!(
            decompress_tick(&blob),
            Err(ReplayError::DecompressionFailed)
        );
    }

    /// A block data that overruns the declared size fails.
    #[test]
    fn a_block_data_overrun_is_rejected() {
        // `[type=0][len=100][one byte]`: the block claims 100 data bytes.
        let decompressed = [0u8, 100, 1];
        assert_eq!(
            scan_block_types(&decompressed, test_block_data_len),
            Err(ReplayError::BlockDataOverrun)
        );
    }

    /// An unknown block type is "Could not read valid block type".
    #[test]
    fn an_invalid_block_type_is_rejected() {
        assert_eq!(
            scan_block_types(&[42, 0], test_block_data_len),
            Err(ReplayError::InvalidBlockType(42))
        );
    }

    /// `FINAL` terminates the block scan.
    #[test]
    fn final_terminates_the_block_scan() {
        let decompressed = [
            block(0, &[1, 2, 3]),
            block(1, &[4]),
            block(9, &[]), // Final
        ]
        .concat();
        let types = scan_block_types(&decompressed, test_block_data_len).expect("scan");
        assert_eq!(
            types,
            vec![BlockType::EntityCreate, BlockType::EntityVolatile]
        );
    }

    // --- BW-11-03-03: block field layouts, entity state, playback/seek ---

    /// An entity-create block: `[vehicleID i32][nested blob]`. The nested blob
    /// decompresses to `[id i32][type u16][pos 3×f32][dir 3×f32][properties]`;
    /// `nested_zip` chooses the nested `CompressionIStream` encoding.
    fn entity_create_block(
        vehicle_id: i32,
        id: i32,
        type_id: u16,
        pos: [f32; 3],
        dir: [f32; 3],
        props: &[u8],
        nested_zip: bool,
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&id.to_le_bytes());
        payload.extend_from_slice(&type_id.to_le_bytes());
        for v in pos {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        for v in dir {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        payload.extend_from_slice(props);
        let nested = if nested_zip {
            zip_blob(&payload)
        } else {
            raw_blob(&payload)
        };
        let mut b = Vec::new();
        b.extend_from_slice(&vehicle_id.to_le_bytes());
        b.extend_from_slice(&nested);
        b
    }

    fn volatile_block(entity_id: i32, pos: [f32; 3], dir: [f32; 3], vehicle: i32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&entity_id.to_le_bytes());
        for v in pos {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in dir {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&vehicle.to_le_bytes());
        b
    }

    fn ground_volatile_block(
        entity_id: i32,
        x: f32,
        z: f32,
        dir: [f32; 3],
        vehicle: i32,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&entity_id.to_le_bytes());
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&z.to_le_bytes());
        for v in dir {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&vehicle.to_le_bytes());
        b
    }

    fn method_block(entity_id: i32, message_id: u8, data: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&entity_id.to_le_bytes());
        b.push(message_id);
        b.extend_from_slice(&(data.len() as i32).to_le_bytes());
        b.extend_from_slice(data);
        b
    }

    fn property_block(entity_id: i32, is_slice: u8, data: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&entity_id.to_le_bytes());
        b.push(is_slice);
        b.extend_from_slice(&(data.len() as i32).to_le_bytes());
        b.extend_from_slice(data);
        b
    }

    fn player_state_block(player_id: i32, has_become: u8, aoi: &[i32]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&player_id.to_le_bytes());
        b.push(has_become);
        if has_become != 0 {
            b.extend_from_slice(&(aoi.len() as u32).to_le_bytes());
            for id in aoi {
                b.extend_from_slice(&id.to_le_bytes());
            }
        }
        b
    }

    fn aoi_block(player_id: i32, entity_id: i32, entered: u8) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&player_id.to_le_bytes());
        b.extend_from_slice(&entity_id.to_le_bytes());
        b.push(entered);
        b
    }

    fn space_data_block(ip: u32, port: u16, key: u16, value: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&ip.to_le_bytes());
        b.extend_from_slice(&port.to_le_bytes());
        b.extend_from_slice(&key.to_le_bytes());
        b.push(value.len() as u8);
        b.extend_from_slice(value.as_bytes());
        b
    }

    /// The full block stream of one tick: the type byte then the block data.
    fn tick_blocks(blocks: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut b = Vec::new();
        for (ty, data) in blocks {
            b.push(*ty);
            b.extend_from_slice(data);
        }
        b
    }

    /// The acceptance: `parse_blocks` reads every block's field layout from a
    /// decompressed tick, matching `ReplayController::processTick`'s `handle*`
    /// methods.
    #[test]
    fn a_tick_parses_all_block_field_layouts() {
        let create = entity_create_block(
            0,
            100,
            3,
            [1.5, 2.5, 3.5],
            [0.1, 0.2, 0.3],
            &[9, 9, 9],
            /*nested_zip*/ true,
        );
        let data = tick_blocks(&[
            (0, create),
            (
                1,
                volatile_block(100, [10.0, 20.0, 30.0], [0.4, 0.5, 0.6], 7),
            ),
            (
                2,
                ground_volatile_block(101, 11.0, 12.0, [0.7, 0.8, 0.9], 0),
            ),
            (3, method_block(100, 5, &[1, 2, 3])),
            (4, property_block(100, 1, &[4, 5])),
            (5, 100i32.to_le_bytes().to_vec()), // EntityDelete
            (6, player_state_block(100, 1, &[101, 102])),
            (7, aoi_block(100, 103, 1)),
            (8, space_data_block(0x01020304, 0x0506, 0x0708, "abc")),
            (9, Vec::new()),
        ]);
        let blocks = parse_blocks(&data).expect("parses");
        assert_eq!(
            blocks,
            vec![
                ReplayBlock::EntityCreate {
                    vehicle_id: 0,
                    id: 100,
                    type_id: 3,
                    position: [1.5, 2.5, 3.5],
                    direction: [0.1, 0.2, 0.3],
                    properties: vec![9, 9, 9],
                },
                ReplayBlock::EntityVolatile {
                    entity_id: 100,
                    position: [10.0, 20.0, 30.0],
                    direction: [0.4, 0.5, 0.6],
                    vehicle_id: 7,
                },
                ReplayBlock::EntityVolatileOnGround {
                    entity_id: 101,
                    x: 11.0,
                    z: 12.0,
                    direction: [0.7, 0.8, 0.9],
                    vehicle_id: 0,
                },
                ReplayBlock::EntityMethod {
                    entity_id: 100,
                    message_id: 5,
                    data: vec![1, 2, 3],
                },
                ReplayBlock::EntityPropertyChange {
                    entity_id: 100,
                    is_slice: true,
                    data: vec![4, 5],
                },
                ReplayBlock::EntityDelete { entity_id: 100 },
                ReplayBlock::EntityPlayerStateChange {
                    player_id: 100,
                    has_become_player: true,
                    aoi: vec![101, 102],
                },
                ReplayBlock::PlayerAoiChange {
                    player_id: 100,
                    entity_id: 103,
                    has_entered_aoi: true,
                },
                ReplayBlock::SpaceData {
                    entry_id: (0x01020304, 0x0506),
                    key: 0x0708,
                    value: "abc".to_string(),
                },
            ]
        );
    }

    /// An entity-create whose nested `CompressionIStream` is raw (`NONE`)
    /// consumes the remaining tick data, so it is the last block.
    #[test]
    fn a_raw_nested_entity_create_consumes_the_rest() {
        let create = entity_create_block(
            0,
            200,
            1,
            [5.0, 6.0, 7.0],
            [0.0; 3],
            &[1, 2],
            /*nested_zip*/ false,
        );
        let data = tick_blocks(&[(0, create), (9, Vec::new())]);
        let blocks = parse_blocks(&data).expect("parses");
        // The raw nested blob is `[NONE][id][type][pos][dir][props][FINAL]` —
        // `CompressionIStream` for `NONE` is the whole remainder, so the
        // trailing `FINAL` byte lands inside the create's properties.
        assert_eq!(
            blocks,
            vec![ReplayBlock::EntityCreate {
                vehicle_id: 0,
                id: 200,
                type_id: 1,
                position: [5.0, 6.0, 7.0],
                direction: [0.0, 0.0, 0.0],
                properties: vec![1, 2, 9],
            }]
        );
    }

    /// An unknown block type is "Could not read valid block type".
    #[test]
    fn an_unknown_block_type_in_parse_is_rejected() {
        assert_eq!(
            parse_blocks(&[42, 0]),
            Err(ReplayError::InvalidBlockType(42))
        );
    }

    /// A block that runs off the end of the tick is `BlockDataOverrun`.
    #[test]
    fn a_truncated_block_is_rejected() {
        // A volatile block needs 4+12+12+4 = 32 bytes; give it only 3.
        let data = tick_blocks(&[(1, vec![0, 0, 0])]);
        assert_eq!(parse_blocks(&data), Err(ReplayError::BlockDataOverrun));
    }

    /// The world follows create/volatile/on-ground/delete/player-state.
    #[test]
    fn entity_state_follows_create_and_volatile_blocks() {
        let mut world = ReplayWorld::new();
        world.apply(&ReplayBlock::EntityCreate {
            vehicle_id: 0,
            id: 1,
            type_id: 3,
            position: [0.0, 0.0, 0.0],
            direction: [0.0, 0.0, 0.0],
            properties: vec![],
        });
        assert_eq!(
            world.get(1),
            Some(&EntityState {
                position: [0.0, 0.0, 0.0],
                direction: [0.0, 0.0, 0.0],
                vehicle_id: 0,
                is_player: false,
            })
        );
        world.apply(&ReplayBlock::EntityVolatile {
            entity_id: 1,
            position: [10.0, 20.0, 30.0],
            direction: [0.1, 0.2, 0.3],
            vehicle_id: 7,
        });
        assert_eq!(world.get(1).map(|e| e.position), Some([10.0, 20.0, 30.0]));
        assert_eq!(world.get(1).map(|e| e.vehicle_id), Some(7));
    }

    /// An on-ground volatile stores `NO_POSITION` in the reconstructed y.
    #[test]
    fn a_ground_volatile_sets_no_position_y() {
        let mut world = ReplayWorld::new();
        world.apply(&ReplayBlock::EntityCreate {
            vehicle_id: 0,
            id: 1,
            type_id: 3,
            position: [0.0, 0.0, 0.0],
            direction: [0.0, 0.0, 0.0],
            properties: vec![],
        });
        world.apply(&ReplayBlock::EntityVolatileOnGround {
            entity_id: 1,
            x: 11.0,
            z: 12.0,
            direction: [0.7, 0.8, 0.9],
            vehicle_id: 0,
        });
        assert_eq!(
            world.get(1).map(|e| e.position),
            Some([11.0, NO_POSITION, 12.0])
        );
    }

    /// A delete removes the entity from the world.
    #[test]
    fn a_delete_removes_the_entity() {
        let mut world = ReplayWorld::new();
        world.apply(&ReplayBlock::EntityCreate {
            vehicle_id: 0,
            id: 1,
            type_id: 3,
            position: [0.0, 0.0, 0.0],
            direction: [0.0, 0.0, 0.0],
            properties: vec![],
        });
        world.apply(&ReplayBlock::EntityDelete { entity_id: 1 });
        assert!(world.get(1).is_none());
    }

    /// A player-state change toggles the entity's `is_player`.
    #[test]
    fn a_player_state_change_marks_the_player() {
        let mut world = ReplayWorld::new();
        world.apply(&ReplayBlock::EntityCreate {
            vehicle_id: 0,
            id: 1,
            type_id: 3,
            position: [0.0, 0.0, 0.0],
            direction: [0.0, 0.0, 0.0],
            properties: vec![],
        });
        world.apply(&ReplayBlock::EntityPlayerStateChange {
            player_id: 1,
            has_become_player: true,
            aoi: vec![2],
        });
        assert_eq!(world.get(1).map(|e| e.is_player), Some(true));
        world.apply(&ReplayBlock::EntityPlayerStateChange {
            player_id: 1,
            has_become_player: false,
            aoi: vec![],
        });
        assert_eq!(world.get(1).map(|e| e.is_player), Some(false));
    }

    /// Build a player over 4 ticks moving entity 1 along +x each tick.
    fn moving_player(freq: u8) -> ReplayPlayer<()> {
        moving_player_with(freq, ())
    }

    /// [`moving_player`] with a caller-supplied handler.
    fn moving_player_with<H: ReplayHandler>(freq: u8, handler: H) -> ReplayPlayer<H> {
        let make_ticks = |steps: &[[f32; 3]]| -> Vec<Tick> {
            steps
                .iter()
                .enumerate()
                .map(|(i, pos)| {
                    let mut data = Vec::new();
                    if i == 0 {
                        data.push(0); // EntityCreate
                        data.extend_from_slice(&entity_create_block(
                            0,
                            1,
                            3,
                            *pos,
                            [0.0; 3],
                            &[],
                            true,
                        ));
                    } else {
                        data.push(1); // EntityVolatile
                        data.extend_from_slice(&volatile_block(1, *pos, [0.0; 3], 0));
                    }
                    data.push(9); // Final
                    Tick {
                        game_time: i as u32,
                        data,
                    }
                })
                .collect()
        };
        ReplayPlayer::new(
            make_ticks(&[
                [0.0, 0.0, 0.0],
                [10.0, 0.0, 0.0],
                [20.0, 0.0, 0.0],
                [30.0, 0.0, 0.0],
            ]),
            100,
            freq,
            handler,
        )
    }

    /// The acceptance: seeking to a target tick, then playing it, reconstructs
    /// the entity state at that tick.
    #[test]
    fn forward_seek_reconstructs_the_target_tick_state() {
        let mut p = moving_player(10);
        p.play();
        p.set_current_tick(2);
        // The seek processes ticks 0..2 (strictly before the target).
        p.tick(0.0).expect("seek");
        assert!(!p.is_seeking(), "the seek completed");
        assert_eq!(p.current_tick(), 2);
        assert_eq!(p.world().get(1).map(|e| e.position), Some([10.0, 0.0, 0.0]));
        // Play the target tick itself.
        p.tick(1.0 / 10.0).expect("play target tick");
        assert_eq!(p.world().get(1).map(|e| e.position), Some([20.0, 0.0, 0.0]));
    }

    /// A backward seek rewinds the world and replays from tick 0 to the target.
    #[test]
    fn backward_seek_rewinds_and_replays() {
        let mut p = moving_player(10);
        p.play();
        // Play all four ticks: the first call is forced to one tick
        // (`currentTick_ == 0`), then a full second accumulates the rest.
        p.tick(0.0).expect("first tick");
        p.tick(1.0).expect("play through");
        assert_eq!(p.current_tick(), 4);
        assert_eq!(p.world().get(1).map(|e| e.position), Some([30.0, 0.0, 0.0]));
        // Rewind to tick 1.
        p.set_current_tick(1);
        p.tick(0.0).expect("seek back");
        assert_eq!(p.current_tick(), 1);
        assert_eq!(p.world().get(1).map(|e| e.position), Some([0.0, 0.0, 0.0]));
        p.tick(1.0 / 10.0).expect("play tick 1");
        assert_eq!(p.world().get(1).map(|e| e.position), Some([10.0, 0.0, 0.0]));
    }

    /// Normal playback steps by the game-update rate; a sub-tick delta
    /// accumulates instead of dropping.
    #[test]
    fn normal_playback_advances_by_the_update_frequency() {
        let mut p = moving_player(10);
        p.play();
        // The first tick is forced.
        p.tick(0.0).expect("first tick");
        assert_eq!(p.current_tick(), 1);
        // A half-tick delta does not advance yet.
        p.tick(0.05).expect("half tick");
        assert_eq!(p.current_tick(), 1);
        // Two half-ticks make a full tick.
        p.tick(0.05).expect("full tick");
        assert_eq!(p.current_tick(), 2);
    }

    /// `getCurrentTickTime()` is `firstGameTime_ + currentTick_`.
    #[test]
    fn current_tick_time_offsets_by_the_first_game_time() {
        let mut p = moving_player(10);
        p.play();
        assert_eq!(p.current_tick_time(), 100);
        p.tick(0.0).expect("first tick");
        assert_eq!(p.current_tick_time(), 101);
    }

    /// A corrupt tick (an invalid block type) propagates from `tick()`.
    #[test]
    fn a_corrupt_tick_is_propagated_from_tick() {
        let ticks = vec![Tick {
            game_time: 0,
            // Two bytes so the `remaining > 1` loop reads the block type.
            data: vec![42, 0],
        }];
        let mut p = ReplayPlayer::new(ticks, 0, 10, ());
        p.play();
        assert_eq!(p.tick(0.0), Err(ReplayError::InvalidBlockType(42)));
    }

    /// An empty replay reaches the finish condition once played.
    #[test]
    fn an_empty_replay_reaches_the_finish_condition() {
        let mut p = ReplayPlayer::new(vec![], 0, 10, ());
        p.play();
        p.tick(0.0).expect("tick");
        assert!(p.is_finished());
    }

    // --- BW-11-03-04: the end-of-replay finish event fires exactly once ---

    /// A handler that counts the finish and post-tick notifications.
    #[derive(Debug, Default)]
    struct FinishCounter {
        finish: usize,
        post_tick: usize,
    }

    impl ReplayHandler for FinishCounter {
        fn on_post_tick(&mut self, _tick: u32) {
            self.post_tick += 1;
        }
        fn on_finish(&mut self) {
            self.finish += 1;
        }
    }

    /// The acceptance: the finish event fires exactly once, however many
    /// further `tick()` calls follow the end of the stream.
    #[test]
    fn finish_fires_exactly_once() {
        let mut p = moving_player_with(10, FinishCounter::default());
        p.play();
        // Play through all four ticks.
        p.tick(0.0).expect("first tick");
        p.tick(1.0).expect("play through");
        assert_eq!(p.current_tick(), 4);
        assert_eq!(p.world().get(1).map(|e| e.position), Some([30.0, 0.0, 0.0]));
        assert_eq!(p.handler.finish, 1, "the finish event fired once");
        // Keep ticking past the end — it must not re-fire.
        for _ in 0..10 {
            p.tick(1.0).expect("tick past the end");
        }
        assert_eq!(p.handler.finish, 1, "the finish event fired exactly once");
        assert!(p.is_finished());
    }

    /// The finish event does not fire before the stream is consumed.
    #[test]
    fn finish_does_not_fire_before_the_end() {
        let mut p = moving_player_with(10, FinishCounter::default());
        p.play();
        p.tick(0.0).expect("first tick"); // one of four
        assert_eq!(p.handler.finish, 0, "not at the end yet");
        assert!(!p.is_finished());
    }

    /// A `FINAL` block read from a tick fires the finish event
    /// (`processTick`'s `BLOCK_TYPE_FINAL` case).
    #[test]
    fn finish_fires_for_a_final_block() {
        // A tick whose data ends with `[FINAL][padding]` so the FINAL byte is
        // read (a lone FINAL is not read — `remaining > 1`).
        let mut data = Vec::new();
        data.push(0); // EntityCreate
        data.extend_from_slice(&entity_create_block(
            0,
            1,
            3,
            [0.0, 0.0, 0.0],
            [0.0; 3],
            &[],
            true,
        ));
        data.push(9); // Final
        data.push(0); // padding so the loop reads the Final
        let ticks = vec![Tick { game_time: 0, data }];
        let mut p = ReplayPlayer::new(ticks, 0, 10, FinishCounter::default());
        p.play();
        p.tick(0.0).expect("tick");
        assert_eq!(p.handler.finish, 1, "the FINAL block fired the finish");
    }

    /// A receiving (downloading) replay does not fire the finish event
    /// (`tick`'s `!isReceiving_` gate).
    #[test]
    fn a_receiving_replay_does_not_finish() {
        let mut p = moving_player_with(10, FinishCounter::default());
        p.set_receiving(true);
        p.play();
        p.tick(0.0).expect("first tick");
        p.tick(1.0).expect("play through");
        assert_eq!(p.current_tick(), 4, "ticks still play while receiving");
        assert_eq!(p.handler.finish, 0, "a receiving stream does not finish");
        assert!(!p.is_finished());
    }

    /// Pausing before the end suppresses the finish event.
    #[test]
    fn pausing_before_the_end_suppresses_finish() {
        let mut p = moving_player_with(10, FinishCounter::default());
        p.play();
        p.tick(0.0).expect("first tick");
        p.pause();
        p.tick(1.0).expect("paused tick");
        assert_eq!(p.handler.finish, 0, "paused playback does not finish");
        assert_eq!(p.current_tick(), 1, "no tick advanced while paused");
    }

    /// A backward rewind re-arms the finish event for the next segment.
    #[test]
    fn a_rewind_rearms_the_finish_event() {
        let mut p = moving_player_with(10, FinishCounter::default());
        p.play();
        p.tick(0.0).expect("first tick");
        p.tick(1.0).expect("play through");
        assert_eq!(p.handler.finish, 1);
        // Rewind to the start and replay — a second segment re-fires once.
        p.set_current_tick(0);
        p.tick(0.0).expect("the seek to tick 0 completes");
        p.tick(0.0).expect("the forced first tick");
        p.tick(1.0).expect("replay the rest");
        assert_eq!(p.current_tick(), 4, "replayed to the end");
        assert_eq!(
            p.handler.finish, 2,
            "the finish re-fired for the new segment"
        );
    }

    /// An empty replay fires the finish event exactly once.
    #[test]
    fn an_empty_replay_finishes_exactly_once() {
        let mut p = ReplayPlayer::new(vec![], 0, 10, FinishCounter::default());
        p.play();
        p.tick(0.0).expect("tick");
        assert_eq!(p.handler.finish, 1);
        p.tick(0.0).expect("tick again");
        assert_eq!(p.handler.finish, 1, "still exactly once");
    }
}
