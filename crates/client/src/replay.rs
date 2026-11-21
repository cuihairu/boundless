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
    let (&compression_type, rest) = blob.split_first().ok_or(ReplayError::DecompressionFailed)?;
    if compression_type == COMPRESSION_NONE {
        return Ok(rest.to_vec());
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
    Ok(out)
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
}
