//! BW-07-05-02: write the replay header and its meta-data.
//!
//! A replay file opens with two signed blocks. `ReplayHeader::addToStream`
//! (`replay_header.ipp:8`) writes a `ChecksumOStream` of the protocol version,
//! the defs `MD5::Digest`, the game update frequency, the creation timestamp,
//! the 8-byte nonce and the reported signature length; the signature the scheme
//! produces is appended after those fields, and `numTicks` is written *outside*
//! the checksum because it changes when the file is finalised
//! (`replay_header.ipp:36`). `ReplayMetaData::addToStream`
//! (`replay_metadata.cpp:60`) follows with a `u32` block length, the packed
//! key/value pairs and another signature; `ReplayDataFileWriter`'s constructor
//! (`replay_data_file_writer.cpp:65`) writes the two together and primes the
//! scheme with the header's signature so the meta-data signature covers both
//! (`:78`).
//!
//! The reader side mirrors `ReplayHeader::initFromStream` (`replay_header.cpp:150`)
//! and `ReplayMetaData::readFromStream` (`replay_metadata.cpp:87`): the version
//! must match the current one or the header is rejected, the signature must
//! verify, and the meta-data block length bounds the pair loop. The checksum
//! scheme is `ChainedChecksumScheme(SHA-256, ECDSA)`
//! (`replay_checksum_scheme.cpp:26`), whose signature length is the second
//! scheme's `streamSize`; this module models that fixed length with a
//! deterministic stand-in so the byte layout is exercised exactly.

use foundation::stream::{BinaryStream, StreamError};
use std::collections::BTreeMap;

/// `ReplayHeader::NONCE_SIZE` (`replay_header.hpp:33`).
pub const NONCE_SIZE: usize = 8;

/// `ReplayData::Timestamp` (`replay_data.hpp:10`): seconds after epoch.
pub type Timestamp = u32;

/// `ReplayData::HeaderSignatureLength` (`replay_data.hpp:11`): the length of
/// the signature carried in the header, `u16`.
pub type HeaderSignatureLength = u16;

/// `MD5::Digest` (`md5.hpp:26`): the 16-byte defs digest.
pub type Digest = [u8; 16];

/// `CLIENT_SERVER_PROTOCOL_VERSION_*` (`client_server_protocol_version.hpp:51`):
/// the current client-server protocol version, 2.9.0.0.
pub const CURRENT_VERSION: ProtocolVersion = ProtocolVersion {
    subpatch: 0,
    patch: 0,
    minor: 9,
    major: 2,
};

/// The failure of parsing a replay header or its meta-data block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// The stream was truncated or malformed.
    Stream(StreamError),
    /// The written version does not match the current protocol version
    /// (`initFromStream`, `replay_header.cpp:168`).
    VersionMismatch,
    /// The signature does not verify.
    BadSignature,
    /// The meta-data block length exceeds what the stream holds
    /// (`readFromStream`, `replay_metadata.cpp:96`).
    InsufficientData,
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stream(err) => write!(f, "replay stream error: {err}"),
            Self::VersionMismatch => {
                write!(f, "replay header version mismatch")
            }
            Self::BadSignature => write!(f, "replay signature does not verify"),
            Self::InsufficientData => {
                write!(f, "insufficient data for meta-data")
            }
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<StreamError> for ReplayError {
    fn from(err: StreamError) -> Self {
        ReplayError::Stream(err)
    }
}

/// The protocol version (`ClientServerProtocolVersion`,
/// `client_server_protocol_version.hpp:60`): four `u8`s streamed as subpatch,
/// patch, minor, major.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolVersion {
    /// `subpatch_`.
    pub subpatch: u8,
    /// `patch_`.
    pub patch: u8,
    /// `minor_`.
    pub minor: u8,
    /// `major_`.
    pub major: u8,
}

impl ProtocolVersion {
    /// `ClientServerProtocolVersion::currentVersion` (`:164`).
    pub fn current() -> Self {
        CURRENT_VERSION
    }

    /// `ClientServerProtocolVersion::streamSize` (`:179`): four `u8`s.
    pub fn stream_size() -> usize {
        4
    }

    /// `ClientServerProtocolVersion::supports` (`:151`): whether `other` is the
    /// same version as this one.
    pub fn supports(&self, other: &Self) -> bool {
        self.subpatch == other.subpatch
            && self.patch == other.patch
            && self.minor == other.minor
            && self.major == other.major
    }
}

/// The signature scheme used for replay signing
/// (`ReplayChecksumScheme::create`, `replay_checksum_scheme.cpp:26`): a
/// `ChainedChecksumScheme(SHA-256, EllipticCurveChecksumScheme)`.
///
/// The real scheme signs a SHA-256 digest with ECDSA, so its `streamSize` is
/// the second scheme's signature length (`checksum_stream.hpp:191`). This model
/// keeps that fixed length and stands in for the cryptography with a
/// deterministic expansion of a 64-bit hash of the signed blob, so written
/// replay bytes are reproducible and verifiable without a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayChecksumScheme {
    signature_length: usize,
}

impl ReplayChecksumScheme {
    /// A scheme producing signatures of `signature_length` bytes.
    pub fn new(signature_length: usize) -> Self {
        ReplayChecksumScheme { signature_length }
    }

    /// The scheme's `streamSize` (`checksum_stream.hpp:191`): the length of the
    /// signature the scheme appends and strips.
    pub fn stream_size(&self) -> usize {
        self.signature_length
    }

    /// The signature over `blob`, expanded to the scheme's stream size. A
    /// deterministic stand-in for SHA-256 + ECDSA.
    pub fn sign(&self, blob: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.signature_length);
        let mut counter = 0u64;
        while out.len() < self.signature_length {
            out.extend_from_slice(&fnv1a(blob, counter).to_le_bytes());
            counter += 1;
        }
        out.truncate(self.signature_length);
        out
    }

    /// Whether `signature` is the signature the scheme would produce over
    /// `blob`.
    pub fn verify(&self, blob: &[u8], signature: &[u8]) -> bool {
        signature.len() == self.signature_length && self.sign(blob) == signature
    }
}

/// A 64-bit FNV-1a hash of `bytes` seeded by `seed`, used as the
/// deterministic digest the signature stand-in expands from.
fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = 0xcbf29ce484222325u64 ^ seed.wrapping_mul(0x9e3779b97f4a7c15);
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x00000100000001b3);
    }
    hash
}

/// The replay file's header block (`ReplayHeader`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHeader {
    /// The protocol version the file was written with.
    pub version: ProtocolVersion,
    /// The defs `MD5::Digest`.
    pub digest: Digest,
    /// `ServerAppConfig::updateHertz` (`replay_data_file_writer.cpp:68`).
    pub game_update_frequency: u8,
    /// The file creation time in seconds after epoch
    /// (`timestamp_ = time(NULL)`, `replay_header.cpp:74`).
    pub timestamp: Timestamp,
    /// The 8-byte header nonce (`initNonce`, `replay_header.cpp`).
    pub nonce: [u8; NONCE_SIZE],
    /// The reported signature length, `pChecksumScheme->streamSize()`
    /// (`replay_header.cpp:66`).
    pub reported_signature_length: HeaderSignatureLength,
    /// The reported tick count, written outside the signature
    /// (`replay_header.ipp:36`).
    pub num_ticks: u32,
}

impl ReplayHeader {
    /// A header for writing, matching the reference constructor
    /// (`replay_header.cpp:47`): the current protocol version, the supplied
    /// digest and frequency, the given timestamp and nonce, and the scheme's
    /// stream size as the reported signature length.
    pub fn new(
        scheme: &ReplayChecksumScheme,
        digest: Digest,
        game_update_frequency: u8,
        timestamp: Timestamp,
        nonce: [u8; NONCE_SIZE],
    ) -> Self {
        ReplayHeader {
            version: ProtocolVersion::current(),
            digest,
            game_update_frequency,
            timestamp,
            nonce,
            reported_signature_length: scheme.stream_size() as u16,
            num_ticks: 0,
        }
    }

    /// `ReplayHeader::streamSize` (`replay_header.cpp:211`): the number of
    /// bytes the header occupies, including the signature.
    pub fn stream_size(&self) -> usize {
        Self::signed_prefix_size() + self.reported_signature_length as usize + 4
    }

    /// The number of bytes written inside the checksum: version, digest,
    /// frequency, timestamp, nonce and signature length.
    fn signed_prefix_size() -> usize {
        ProtocolVersion::stream_size() + 16 + 1 + 4 + NONCE_SIZE + 2
    }

    /// `ReplayHeader::numTicksFieldOffset` (`replay_header.cpp:255`): the file
    /// offset of the header's tick-count field, the whole header minus that
    /// field. Finalisation rewrites the count there.
    pub fn num_ticks_field_offset(scheme: &ReplayChecksumScheme) -> u64 {
        (Self::signed_prefix_size() + scheme.stream_size()) as u64
    }

    /// `ReplayHeader::addToStream` (`replay_header.ipp:8`): write the header to
    /// `out` and return the signature the scheme produced over the signed
    /// prefix.
    pub fn write_to(&self, scheme: &ReplayChecksumScheme, out: &mut BinaryStream) -> Vec<u8> {
        let body = self.signed_prefix();
        let signature = scheme.sign(&body);
        out.write_bytes(&body);
        out.write_bytes(&signature);
        out.write_u32(self.num_ticks);
        signature
    }

    /// The bytes fed to the checksum scheme: the signed prefix. `numTicks` is
    /// excluded (`replay_header.ipp:36`).
    fn signed_prefix(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(Self::signed_prefix_size());
        body.extend_from_slice(&[
            self.version.subpatch,
            self.version.patch,
            self.version.minor,
            self.version.major,
        ]);
        body.extend_from_slice(&self.digest);
        body.push(self.game_update_frequency);
        body.extend_from_slice(&self.timestamp.to_le_bytes());
        body.extend_from_slice(&self.nonce);
        body.extend_from_slice(&self.reported_signature_length.to_le_bytes());
        body
    }

    /// `ReplayHeader::initFromStream` (`replay_header.cpp:150`): parse a header
    /// from `stream`, returning the header and its signature. The version must
    /// match the current one and, when `should_verify`, the signature must
    /// verify against `scheme`.
    pub fn read_from(
        stream: &mut BinaryStream,
        scheme: &ReplayChecksumScheme,
        should_verify: bool,
    ) -> Result<(Self, Vec<u8>), ReplayError> {
        let subpatch = stream.read_u8()?;
        let patch = stream.read_u8()?;
        let minor = stream.read_u8()?;
        let major = stream.read_u8()?;
        let version = ProtocolVersion {
            subpatch,
            patch,
            minor,
            major,
        };
        if !version.supports(&ProtocolVersion::current()) {
            return Err(ReplayError::VersionMismatch);
        }

        let mut digest = [0u8; 16];
        digest.copy_from_slice(&stream.read_blob_raw(16)?);
        let game_update_frequency = stream.read_u8()?;
        let timestamp = stream.read_u32()?;
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&stream.read_blob_raw(NONCE_SIZE)?);
        let reported_signature_length = stream.read_u16()?;

        let mut signature = vec![0u8; reported_signature_length as usize];
        signature.copy_from_slice(&stream.read_blob_raw(reported_signature_length as usize)?);
        if should_verify
            && !scheme.verify(
                &Self::prefix_bytes(
                    version,
                    &digest,
                    game_update_frequency,
                    timestamp,
                    nonce,
                    reported_signature_length,
                ),
                &signature,
            )
        {
            return Err(ReplayError::BadSignature);
        }

        let num_ticks = stream.read_u32()?;
        Ok((
            ReplayHeader {
                version,
                digest,
                game_update_frequency,
                timestamp,
                nonce,
                reported_signature_length,
                num_ticks,
            },
            signature,
        ))
    }

    /// The signed-prefix bytes a reader reconstructs before verifying.
    fn prefix_bytes(
        version: ProtocolVersion,
        digest: &Digest,
        game_update_frequency: u8,
        timestamp: Timestamp,
        nonce: [u8; NONCE_SIZE],
        reported_signature_length: HeaderSignatureLength,
    ) -> Vec<u8> {
        let mut body = Vec::with_capacity(Self::signed_prefix_size());
        body.extend_from_slice(&[
            version.subpatch,
            version.patch,
            version.minor,
            version.major,
        ]);
        body.extend_from_slice(digest);
        body.push(game_update_frequency);
        body.extend_from_slice(&timestamp.to_le_bytes());
        body.extend_from_slice(&nonce);
        body.extend_from_slice(&reported_signature_length.to_le_bytes());
        body
    }

    /// `ReplayHeader::checkSufficientLength` (`replay_header.cpp:273`): whether
    /// `data` holds a complete header, filling in its total length.
    pub fn check_sufficient_length(data: &[u8], header_size: &mut usize) -> bool {
        let signature_length_offset = 4 + 16 + 1 + 4 + NONCE_SIZE;
        if data.len() < signature_length_offset + 2 {
            return false;
        }
        let mut length_stream =
            BinaryStream::from_slice(&data[signature_length_offset..signature_length_offset + 2]);
        let reported_signature_length = match length_stream.read_u16() {
            Ok(value) => value,
            Err(_) => return false,
        };
        *header_size = signature_length_offset + 2 + reported_signature_length as usize + 4;
        data.len() >= *header_size
    }
}

/// The replay file's user-supplied meta-data block (`ReplayMetaData`,
/// `replay_metadata.hpp:19`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayMetaData {
    /// The key/value pairs, in insertion order (a `BTreeMap` orders by key, as
    /// the reference's `BW::map` does).
    collection: BTreeMap<String, String>,
    /// The packed size of the key/value pairs, `streamSize_`
    /// (`replay_metadata.hpp:75`).
    stream_size: u32,
}

impl ReplayMetaData {
    /// An empty meta-data block.
    pub fn new() -> Self {
        Self::default()
    }

    /// `ReplayMetaData::size` (`replay_metadata.hpp:43`): the number of pairs.
    pub fn size(&self) -> usize {
        self.collection.len()
    }

    /// `ReplayMetaData::streamSize` (`replay_metadata.hpp:62`): the size of the
    /// meta-data block when streamed.
    pub fn stream_size(&self) -> u32 {
        self.stream_size
    }

    /// `ReplayMetaData::hasKey` (`replay_metadata.hpp:184`): whether `key` is
    /// present.
    pub fn has_key(&self, key: &str) -> bool {
        self.collection.contains_key(key)
    }

    /// `ReplayMetaData::get` (`replay_metadata.cpp:195`): the value for `key`,
    /// or `default_value` when absent.
    pub fn get(&self, key: &str, default_value: &str) -> String {
        self.collection
            .get(key)
            .cloned()
            .unwrap_or_else(|| default_value.to_string())
    }

    /// `ReplayMetaData::add` (`replay_metadata.cpp:154`): set `key` to `value`,
    /// replacing any existing entry and keeping the streamed size in step.
    pub fn add(&mut self, key: &str, value: &str) {
        if let Some(old) = self.collection.insert(key.to_string(), value.to_string()) {
            self.stream_size = self
                .stream_size
                .saturating_sub(packed_size(key) + packed_size(&old));
        }
        self.stream_size += packed_size(key) + packed_size(value);
    }

    /// `ReplayMetaData::addToStream` (`replay_metadata.cpp:60`): write the block
    /// to `out` and return the signature. The signature is computed over
    /// `primed_with` (the header's signature, fed into the scheme first by
    /// `ReplayDataFileWriter`, `replay_data_file_writer.cpp:78`) followed by
    /// the block length and the pairs.
    pub fn write_to(
        &self,
        scheme: &ReplayChecksumScheme,
        primed_with: &[u8],
        out: &mut BinaryStream,
    ) -> Vec<u8> {
        let body = self.block_bytes();
        let mut blob = primed_with.to_vec();
        blob.extend_from_slice(&body);
        let signature = scheme.sign(&blob);
        out.write_bytes(&body);
        out.write_bytes(&signature);
        signature
    }

    /// The bytes inside the checksum: the block length and the pairs.
    fn block_bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&self.stream_size.to_le_bytes());
        for (key, value) in &self.collection {
            append_packed_string(&mut body, key);
            append_packed_string(&mut body, value);
        }
        body
    }

    /// `ReplayMetaData::readFromStream` (`replay_metadata.cpp:87`): parse a block
    /// from `stream`, priming the signature with `primed_with` (the header's
    /// signature). `signature_length` is the length of the trailing signature.
    pub fn read_from(
        stream: &mut BinaryStream,
        scheme: &ReplayChecksumScheme,
        primed_with: &[u8],
        signature_length: usize,
    ) -> Result<Self, ReplayError> {
        let stream_size = stream.read_u32()?;
        if usize::try_from(stream_size).unwrap_or(usize::MAX) > stream.remaining() {
            return Err(ReplayError::InsufficientData);
        }
        let block = stream.read_blob_raw(stream_size as usize)?;
        let signature = stream.read_blob_raw(signature_length)?;
        let mut blob = primed_with.to_vec();
        blob.extend_from_slice(&stream_size.to_le_bytes());
        blob.extend_from_slice(&block);
        if !scheme.verify(&blob, &signature) {
            return Err(ReplayError::BadSignature);
        }

        let mut pair_stream = BinaryStream::from_slice(&block);
        let mut collection = BTreeMap::new();
        while !pair_stream.at_end() && !pair_stream.has_error() {
            let key = match pair_stream.read_string() {
                Ok(key) => key,
                Err(_) => break,
            };
            let value = match pair_stream.read_string() {
                Ok(value) => value,
                Err(_) => return Err(ReplayError::Stream(StreamError::Truncated)),
            };
            collection.insert(key, value);
        }
        if pair_stream.has_error() {
            return Err(ReplayError::Stream(StreamError::Truncated));
        }

        Ok(ReplayMetaData {
            collection,
            stream_size,
        })
    }

    /// `ReplayMetaData::checkSufficientLength` (`replay_metadata.cpp:222`):
    /// whether `data` holds a complete block of the given signature length,
    /// filling in its total length.
    pub fn check_sufficient_length(
        data: &[u8],
        signature_length: usize,
        meta_data_length: &mut usize,
    ) -> bool {
        if data.len() < 4 {
            return false;
        }
        let mut size_stream = BinaryStream::from_slice(&data[..4]);
        let stream_size = match size_stream.read_u32() {
            Ok(value) => value,
            Err(_) => return false,
        };
        *meta_data_length = 4 + stream_size as usize + signature_length;
        data.len() >= *meta_data_length
    }
}

/// The packed size of `text` when streamed, matching the reference's
/// `calculateStringPackedSize` (`replay_metadata.cpp:240`) and the true
/// `writePackedInt` byte count (`binary_stream.hpp:254`): one byte of length
/// prefix for a string under 255 bytes, four beyond it.
fn packed_size(text: &str) -> u32 {
    if text.len() < 255 {
        (1 + text.len()) as u32
    } else {
        (4 + text.len()) as u32
    }
}

/// Append `text` to `out` as a packed string: length prefix then bytes.
fn append_packed_string(out: &mut Vec<u8>, text: &str) {
    if text.len() < 255 {
        out.push(text.len() as u8);
    } else {
        out.push(0xFF);
        let len = text.len() as u32;
        out.extend_from_slice(&[
            (len & 0xFF) as u8,
            ((len >> 8) & 0xFF) as u8,
            ((len >> 16) & 0xFF) as u8,
        ]);
    }
    out.extend_from_slice(text.as_bytes());
}

/// `ReplayDataFileWriter`'s header-writing constructor
/// (`replay_data_file_writer.cpp:65`): write the header and meta-data blocks to
/// `out` and return both signatures, the meta-data signature chained off the
/// header's.
pub struct ReplayPrefix {
    /// The header block's signature.
    pub header_signature: Vec<u8>,
    /// The meta-data block's signature.
    pub meta_data_signature: Vec<u8>,
}

/// Write a replay file's opening header and meta-data blocks
/// (`ReplayDataFileWriter` constructor, `replay_data_file_writer.cpp:65`). The
/// caller builds `header` with [`ReplayHeader::new`] and sets its tick count.
pub fn write_replay_prefix(
    scheme: &ReplayChecksumScheme,
    header: &ReplayHeader,
    meta_data: &ReplayMetaData,
    out: &mut BinaryStream,
) -> ReplayPrefix {
    let header_signature = header.write_to(scheme, out);
    let meta_data_signature = meta_data.write_to(scheme, &header_signature, out);
    ReplayPrefix {
        header_signature,
        meta_data_signature,
    }
}

/// Parse a replay file's opening header and meta-data blocks
/// (`ReplayHeader::initFromStream` + `ReplayMetaData::readFromStream`,
/// `replay_data_file_writer.cpp:65`).
pub fn read_replay_prefix(
    stream: &mut BinaryStream,
    scheme: &ReplayChecksumScheme,
    should_verify: bool,
) -> Result<(ReplayHeader, ReplayMetaData), ReplayError> {
    let (header, header_signature) = ReplayHeader::read_from(stream, scheme, should_verify)?;
    let meta_data =
        ReplayMetaData::read_from(stream, scheme, &header_signature, scheme.stream_size())?;
    Ok((header, meta_data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme() -> ReplayChecksumScheme {
        ReplayChecksumScheme::new(8)
    }

    fn digest() -> Digest {
        [0xAA; 16]
    }

    fn nonce() -> [u8; NONCE_SIZE] {
        [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
    }

    /// A header round-trips through a faithful reader with every field intact.
    #[test]
    fn a_header_round_trips_through_a_faithful_reader() {
        let scheme = scheme();
        let mut header = ReplayHeader::new(&scheme, digest(), 20, 1_700_000_000, nonce());
        header.num_ticks = 42;

        let mut out = BinaryStream::new();
        header.write_to(&scheme, &mut out);

        let mut stream = BinaryStream::from_slice(out.as_bytes());
        let (parsed, _signature) = ReplayHeader::read_from(&mut stream, &scheme, true)
            .expect("a faithful reader parses the written header");
        assert_eq!(parsed, header);
    }

    /// The written bytes carry the reference layout: the four version bytes,
    /// the digest, the frequency, the timestamp, the nonce, the signature
    /// length, the signature and `numTicks` last.
    #[test]
    fn the_header_follows_the_reference_wire_layout() {
        let scheme = scheme();
        let header = ReplayHeader::new(&scheme, digest(), 20, 1_700_000_000, nonce());

        let mut out = BinaryStream::new();
        header.write_to(&scheme, &mut out);
        let bytes = out.as_bytes();

        assert_eq!(
            &bytes[0..4],
            &[0, 0, 9, 2],
            "version is subpatch,patch,minor,major"
        );
        assert_eq!(&bytes[4..20], &[0xAA; 16], "digest");
        assert_eq!(bytes[20], 20, "game update frequency");
        assert_eq!(&bytes[21..25], &1_700_000_000u32.to_le_bytes(), "timestamp");
        assert_eq!(&bytes[25..33], &nonce(), "nonce");
        assert_eq!(&bytes[33..35], &8u16.to_le_bytes(), "signature length");
        assert_eq!(bytes.len(), 35 + 8 + 4, "prefix, signature, numTicks");
        assert_eq!(&bytes[35 + 8..], &0u32.to_le_bytes(), "numTicks last");

        let mut hint = 0;
        assert!(ReplayHeader::check_sufficient_length(bytes, &mut hint));
        assert_eq!(hint, bytes.len(), "the hint reports the full length");
    }

    /// A version that does not match the current one is rejected, as the
    /// reference reader rejects it (`replay_header.cpp:168`).
    #[test]
    fn a_version_mismatch_is_rejected() {
        let scheme = scheme();
        let header = ReplayHeader::new(&scheme, digest(), 20, 1_700_000_000, nonce());

        let mut out = BinaryStream::new();
        header.write_to(&scheme, &mut out);
        let mut bytes = out.as_bytes().to_vec();
        bytes[3] += 1; // bump major

        let mut stream = BinaryStream::from_slice(&bytes);
        let err = ReplayHeader::read_from(&mut stream, &scheme, true)
            .expect_err("the bumped version is rejected");
        assert_eq!(err, ReplayError::VersionMismatch);
    }

    /// A corrupted signature fails verification, and reading without verifying
    /// still returns the signature for a non-verifying reader.
    #[test]
    fn a_bad_signature_fails_verification() {
        let scheme = scheme();
        let header = ReplayHeader::new(&scheme, digest(), 20, 1_700_000_000, nonce());

        let mut out = BinaryStream::new();
        header.write_to(&scheme, &mut out);
        let mut bytes = out.as_bytes().to_vec();
        bytes[35] ^= 0xFF; // corrupt the signature

        let mut stream = BinaryStream::from_slice(&bytes);
        let err = ReplayHeader::read_from(&mut stream, &scheme, true)
            .expect_err("the corrupted signature fails");
        assert_eq!(err, ReplayError::BadSignature);

        // A non-verifying reader still hands back the signature bytes.
        let mut stream = BinaryStream::from_slice(&bytes);
        let (parsed, signature) =
            ReplayHeader::read_from(&mut stream, &scheme, false).expect("unverified read");
        assert_eq!(
            signature,
            bytes[35..43],
            "the read-back signature is the bytes"
        );
        assert_eq!(parsed.reported_signature_length, 8);
    }

    /// `numTicks` sits outside the signature: changing it does not invalidate
    /// the header's signature (`replay_header.ipp:36`).
    #[test]
    fn num_ticks_is_outside_the_signature() {
        let scheme = scheme();
        let mut header = ReplayHeader::new(&scheme, digest(), 20, 1_700_000_000, nonce());
        header.num_ticks = 0;

        let mut out = BinaryStream::new();
        header.write_to(&scheme, &mut out);
        let mut bytes = out.as_bytes().to_vec();
        let num_ticks_offset = 35 + 8;
        bytes[num_ticks_offset..].copy_from_slice(&999u32.to_le_bytes());

        let mut stream = BinaryStream::from_slice(&bytes);
        let (parsed, _) = ReplayHeader::read_from(&mut stream, &scheme, true)
            .expect("the signature still verifies after the tick count changes");
        assert_eq!(parsed.num_ticks, 999);
    }

    /// Meta-data pairs stream and parse back, and the streamed size tracks the
    /// packed pair bytes.
    #[test]
    fn meta_data_pairs_round_trip_and_track_stream_size() {
        let scheme = scheme();
        let mut meta_data = ReplayMetaData::new();
        meta_data.add("server", "baseapp");
        meta_data.add("map", "arena");
        assert_eq!(meta_data.size(), 2);
        assert_eq!(
            meta_data.stream_size(),
            packed_size("server")
                + packed_size("baseapp")
                + packed_size("map")
                + packed_size("arena")
        );
        assert!(meta_data.has_key("server"));
        assert_eq!(meta_data.get("missing", "fallback"), "fallback");
        assert_eq!(meta_data.get("map", "fallback"), "arena");

        let header_signature = vec![0u8; 8];
        let mut out = BinaryStream::new();
        meta_data.write_to(&scheme, &header_signature, &mut out);

        let mut stream = BinaryStream::from_slice(out.as_bytes());
        let parsed = ReplayMetaData::read_from(&mut stream, &scheme, &header_signature, 8)
            .expect("the block parses");
        assert_eq!(parsed.collection, meta_data.collection);
        assert_eq!(parsed.stream_size, meta_data.stream_size());
    }

    /// Replacing a key keeps the streamed size in step with the pairs that are
    /// actually written.
    #[test]
    fn replacing_a_key_keeps_the_stream_size_in_step() {
        let mut meta_data = ReplayMetaData::new();
        meta_data.add("a", "1");
        meta_data.add("b", "2");
        let before = meta_data.stream_size();

        meta_data.add("a", "12345");
        assert_eq!(
            meta_data.stream_size(),
            before - packed_size("a") - packed_size("1") + packed_size("a") + packed_size("12345")
        );
    }

    /// A meta-data block longer than the stream is rejected, as the reference
    /// does (`replay_metadata.cpp:96`).
    #[test]
    fn a_meta_data_block_longer_than_the_stream_is_rejected() {
        let scheme = scheme();
        let mut meta_data = ReplayMetaData::new();
        meta_data.add("server", "baseapp");

        let header_signature = vec![0u8; 8];
        let mut out = BinaryStream::new();
        meta_data.write_to(&scheme, &header_signature, &mut out);
        let bytes = out.as_bytes();

        // Claim a larger block length than is present.
        let mut lying = bytes.to_vec();
        lying[0..4].copy_from_slice(&1000u32.to_le_bytes());

        let mut stream = BinaryStream::from_slice(&lying);
        let err = ReplayMetaData::read_from(&mut stream, &scheme, &header_signature, 8)
            .expect_err("the over-long block is rejected");
        assert_eq!(err, ReplayError::InsufficientData);

        let mut hint = 0;
        assert!(!ReplayMetaData::check_sufficient_length(
            &lying, 8, &mut hint
        ));
    }

    /// The two blocks written together parse back as the header and meta-data,
    /// the meta-data signature chained off the header's.
    #[test]
    fn the_two_blocks_parse_back_as_header_and_meta_data() {
        let scheme = scheme();
        let mut meta_data = ReplayMetaData::new();
        meta_data.add("server", "baseapp");
        meta_data.add("map", "arena");

        let mut header = ReplayHeader::new(&scheme, digest(), 20, 1_700_000_000, nonce());
        header.num_ticks = 42;

        let mut out = BinaryStream::new();
        let prefix = write_replay_prefix(&scheme, &header, &meta_data, &mut out);
        assert_eq!(prefix.header_signature.len(), 8);
        assert_eq!(prefix.meta_data_signature.len(), 8);

        let mut stream = BinaryStream::from_slice(out.as_bytes());
        let (header, parsed_meta_data) =
            read_replay_prefix(&mut stream, &scheme, true).expect("both blocks parse");
        assert_eq!(header.num_ticks, 42);
        assert_eq!(parsed_meta_data.collection, meta_data.collection);
    }

    /// A short stream cannot hold a complete header, as the reference hints
    /// (`replay_header.cpp:273`).
    #[test]
    fn a_short_stream_cannot_hold_a_complete_header() {
        let scheme = scheme();
        let header = ReplayHeader::new(&scheme, digest(), 20, 1_700_000_000, nonce());
        let mut out = BinaryStream::new();
        header.write_to(&scheme, &mut out);
        let bytes = out.as_bytes();

        let mut hint = 0;
        assert!(!ReplayHeader::check_sufficient_length(
            &bytes[..10],
            &mut hint
        ));
        assert!(ReplayHeader::check_sufficient_length(bytes, &mut hint));
        assert_eq!(hint, bytes.len());
    }
}
