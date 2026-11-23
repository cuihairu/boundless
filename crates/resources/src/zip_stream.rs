//! BW-01-02-04 the compressed stream: the reference's `lib/network/zip_stream.cpp`
//! and `compression_stream.cpp` framing, read and written through the port's
//! own [`foundation::stream::BinaryStream`].
//!
//! The wire form is:
//!
//! ```text
//! [uint8 tag][packed-int original length][packed-int compressed length][zlib blob]
//! ```
//!
//! * tag `0` is `BW_COMPRESSION_NONE`: the payload follows the tag inline and
//!   unframed (no lengths, no blob).
//! * tags `1..=9` are `BW_COMPRESSION_ZIP_1`..`ZIP_9`; the zlib level is the
//!   tag itself (`(tag - BW_COMPRESSION_ZIP_BEST_SPEED) + Z_BEST_SPEED`).
//!
//! The blob is a zlib stream (`compress2`/`uncompress`, RFC 1950 wrapper), so
//! the port and the reference can decode each other's frames. The reader
//! enforces the reference's two bounds: the compressed length may not exceed
//! the stream's remaining bytes, and the declared original length may not
//! exceed `compressed length * 1032` (DEFLATE's worst-case expansion,
//! RFC 1951) — the reference's CR-P2-21 guard.

use flate2::Compression;
use flate2::write::{ZlibDecoder, ZlibEncoder};
use foundation::stream::{BinaryStream, StreamError};
use std::io::Write;

/// `BW_COMPRESSION_NONE`: the payload is written inline, unframed.
pub const COMPRESSION_NONE: u8 = 0;

/// `BW_COMPRESSION_ZIP_BEST_SPEED`: the fastest zlib level.
pub const COMPRESSION_ZIP_BEST_SPEED: u8 = 1;

/// `BW_COMPRESSION_ZIP_BEST_COMPRESSION`: the best zlib level.
pub const COMPRESSION_ZIP_BEST_COMPRESSION: u8 = 9;

/// The DEFLATE worst-case expansion bound the reader enforces (RFC 1951): a
/// declared original length beyond `compressed length * 1032` cannot come
/// from a real zlib stream of that compressed length.
pub const MAX_DEFLATE_EXPANSION: u64 = 1032;

/// The tag is a ZIP level (`BW_COMPRESSION_ZIP_1`..`ZIP_9`).
pub fn is_zip_tag(tag: u8) -> bool {
    (COMPRESSION_ZIP_BEST_SPEED..=COMPRESSION_ZIP_BEST_COMPRESSION).contains(&tag)
}

/// A read compressed stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionPayload {
    /// `BW_COMPRESSION_NONE`: the bytes after the tag, unframed.
    Passthrough { bytes: Vec<u8> },
    /// `BW_COMPRESSION_ZIP_n`: the inflated payload.
    Zip { original: Vec<u8> },
}

/// A compressed-stream error (the reference logs a `CRITICAL_MSG` on an
/// invalid tag and sets the stream error on a bad frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZipStreamError {
    /// The framing ran past the end of the buffer.
    Truncated,
    /// The tag is neither `BW_COMPRESSION_NONE` nor a ZIP level.
    InvalidTag(u8),
    /// A packed length exceeds what a packed int can carry (`2^24 - 1`).
    LengthTooLarge,
    /// The declared original length is beyond DEFLATE's worst-case expansion.
    ImpossibleLengths,
    /// The zlib inflate failed, or the inflated length exceeds the declared
    /// original length (the reference's `Z_BUF_ERROR`).
    InflateFailed,
}

impl std::fmt::Display for ZipStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "compressed stream read would run past the end"),
            Self::InvalidTag(tag) => write!(f, "invalid compression type tag: {tag}"),
            Self::LengthTooLarge => {
                write!(f, "compressed stream length exceeds 2^24 - 1")
            }
            Self::ImpossibleLengths => {
                write!(
                    f,
                    "declared original length exceeds the deflate expansion bound"
                )
            }
            Self::InflateFailed => write!(f, "zlib inflate failed"),
        }
    }
}

impl std::error::Error for ZipStreamError {}

impl From<StreamError> for ZipStreamError {
    fn from(err: StreamError) -> Self {
        match err {
            StreamError::Truncated => Self::Truncated,
            StreamError::PackedIntTooLarge => Self::LengthTooLarge,
            // The bw00-generation stream carries compression-side failures
            // (encode/decode failure, impossible declared ratio); a zip
            // stream reading path can only surface them as corrupt data.
            StreamError::CompressionFailed | StreamError::CompressionRatioExceeded => {
                Self::InflateFailed
            }
        }
    }
}

/// The `CompressionOStream` path: write the tag byte, then the payload —
/// inline and unframed for `BW_COMPRESSION_NONE`, zlib-compressed and framed
/// for a ZIP level.
pub fn write_compressed(
    dst: &mut BinaryStream,
    tag: u8,
    payload: &[u8],
) -> Result<(), ZipStreamError> {
    dst.write_u8(tag);
    if tag == COMPRESSION_NONE {
        dst.write_blob_raw(payload);
        return Ok(());
    }
    if !is_zip_tag(tag) {
        return Err(ZipStreamError::InvalidTag(tag));
    }
    write_zip_frame(dst, payload, tag)
}

/// The `ZipOStream` destructor path: the two packed length prefixes plus the
/// zlib blob.
fn write_zip_frame(dst: &mut BinaryStream, payload: &[u8], tag: u8) -> Result<(), ZipStreamError> {
    let blob = compress_at_level(payload, tag)?;
    dst.write_packed_int(payload.len() as u32)?;
    dst.write_packed_int(blob.len() as u32)?;
    dst.write_blob_raw(&blob);
    Ok(())
}

/// Zlib-compress `payload` at the level a ZIP tag maps to (`tag` is the level).
pub fn compress_at_level(payload: &[u8], tag: u8) -> Result<Vec<u8>, ZipStreamError> {
    if !is_zip_tag(tag) {
        return Err(ZipStreamError::InvalidTag(tag));
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(tag as u32));
    encoder
        .write_all(payload)
        .map_err(|_| ZipStreamError::InflateFailed)?;
    encoder.finish().map_err(|_| ZipStreamError::InflateFailed)
}

/// The `CompressionIStream` path: read the tag, then the payload — inline and
/// unframed for `BW_COMPRESSION_NONE`, inflated for a ZIP level.
pub fn read_compressed(src: &mut BinaryStream) -> Result<CompressionPayload, ZipStreamError> {
    let tag = src.read_u8()?;
    if tag == COMPRESSION_NONE {
        let bytes = src.read_blob_raw(src.remaining())?;
        return Ok(CompressionPayload::Passthrough { bytes });
    }
    if !is_zip_tag(tag) {
        return Err(ZipStreamError::InvalidTag(tag));
    }

    let orig_len = src.read_packed_int()? as usize;
    let compress_len = src.read_packed_int()? as usize;

    // The reference's two bounds, checked before allocating.
    if compress_len > src.remaining() {
        return Err(ZipStreamError::Truncated);
    }
    if orig_len as u64 > (compress_len as u64) * MAX_DEFLATE_EXPANSION {
        return Err(ZipStreamError::ImpossibleLengths);
    }

    let blob = src.read_blob_raw(compress_len)?;
    let mut decoder = ZlibDecoder::new(Vec::with_capacity(orig_len));
    decoder
        .write_all(&blob)
        .map_err(|_| ZipStreamError::InflateFailed)?;
    let inflated = decoder
        .finish()
        .map_err(|_| ZipStreamError::InflateFailed)?;

    // A stream longer than the declared original length cannot fit the
    // reference's sized buffer — its `uncompress` returns Z_BUF_ERROR, so the
    // reference rejects it. A shorter one is accepted at its actual length.
    if inflated.len() > orig_len {
        return Err(ZipStreamError::InflateFailed);
    }
    Ok(CompressionPayload::Zip { original: inflated })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 272-byte payload the reference fixture compresses.
    fn fixture_payload() -> Vec<u8> {
        b"BigWorld compressed stream payload".repeat(8)
    }

    /// The uncompressed length the reference frame declares.
    const FIXTURE_ORIGINAL_LENGTH: u32 = 272;

    /// The reference fixture for the compressible payload at level 1
    /// (`zlib.compress(payload, 1)` — the same `compress2` the reference
    /// links), plus the framing around it.
    const FIXTURE_BLOB_LEVEL_1: [u8; 45] = [
        0x78, 0x01, 0x73, 0xca, 0x4c, 0x0f, 0xcf, 0x2f, 0xca, 0x49, 0x51, 0x48, 0xce, 0xcf, 0x2d,
        0x28, 0x4a, 0x2d, 0x2e, 0x4e, 0x4d, 0x51, 0x28, 0x2e, 0x29, 0x4a, 0x4d, 0xcc, 0x55, 0x28,
        0x48, 0xac, 0xcc, 0xc9, 0x4f, 0x4c, 0x71, 0x1a, 0x51, 0x2a, 0x00, 0x03, 0xf0, 0x69, 0x29,
    ];

    /// The BW-01-02-04 acceptance: the port decodes the reference's frame
    /// byte for byte, and its own encoding decodes the same payload — both
    /// sides of the original handle the same fixture.
    #[test]
    fn both_sides_decode_the_reference_fixture() {
        // The reference frame: tag 1, packed origLen 272, packed compressLen
        // 45, then the zlib blob.
        let mut frame = BinaryStream::new();
        frame.write_u8(COMPRESSION_ZIP_BEST_SPEED);
        frame
            .write_u8(0xFF)
            .write_u8(0x10)
            .write_u8(0x01)
            .write_u8(0x00); // packed 272
        frame.write_u8(0x2D); // packed 45
        frame.write_blob_raw(&FIXTURE_BLOB_LEVEL_1);

        // The port's decoder recovers the original payload from the
        // reference's own bytes.
        frame.rewind();
        assert_eq!(
            read_compressed(&mut frame),
            Ok(CompressionPayload::Zip {
                original: fixture_payload(),
            })
        );
        assert!(frame.at_end());

        // The port's encoder produces a frame the reference's layout
        // describes — tag, packed original length, packed compressed length,
        // blob — and it decodes the same payload. The blob itself need not
        // match byte for byte: deflate output is not canonical, only its
        // decodability is.
        let mut encoded = BinaryStream::new();
        write_compressed(&mut encoded, COMPRESSION_ZIP_BEST_SPEED, &fixture_payload()).unwrap();
        let mut check = encoded.clone();
        check.rewind();
        assert_eq!(check.read_u8(), Ok(COMPRESSION_ZIP_BEST_SPEED));
        assert_eq!(check.read_packed_int(), Ok(FIXTURE_ORIGINAL_LENGTH));
        let compress_len = check.read_packed_int().unwrap();
        assert_eq!(compress_len as usize, check.remaining());

        encoded.rewind();
        assert_eq!(
            read_compressed(&mut encoded),
            Ok(CompressionPayload::Zip {
                original: fixture_payload(),
            })
        );
    }

    /// The reference fixture for an empty payload (`zlib.compress(b"", 1)`).
    #[test]
    fn empty_payload_round_trips() {
        let mut frame = BinaryStream::new();
        frame
            .write_u8(COMPRESSION_ZIP_BEST_SPEED)
            .write_u8(0x00) // packed origLen 0
            .write_u8(0x08) // packed compressLen 8
            .write_blob_raw(&[0x78, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        frame.rewind();
        assert_eq!(
            read_compressed(&mut frame),
            Ok(CompressionPayload::Zip {
                original: Vec::new()
            })
        );
    }

    /// `BW_COMPRESSION_NONE`: the payload follows the tag inline, unframed.
    #[test]
    fn none_passes_the_payload_through() {
        let mut s = BinaryStream::new();
        write_compressed(&mut s, COMPRESSION_NONE, b"raw bytes").unwrap();
        assert_eq!(
            s.as_bytes(),
            &[0x00, b'r', b'a', b'w', b' ', b'b', b'y', b't', b'e', b's']
        );
        s.rewind();
        assert_eq!(
            read_compressed(&mut s),
            Ok(CompressionPayload::Passthrough {
                bytes: b"raw bytes".to_vec(),
            })
        );
    }

    /// A bad tag is rejected rather than silently treated as passthrough (the
    /// reference logs a critical message instead).
    #[test]
    fn invalid_tag_is_rejected() {
        let mut s = BinaryStream::new();
        s.write_u8(42);
        s.rewind();
        assert_eq!(read_compressed(&mut s), Err(ZipStreamError::InvalidTag(42)));
        assert_eq!(
            write_compressed(&mut s, 42, b"x"),
            Err(ZipStreamError::InvalidTag(42))
        );
    }

    /// The reference's CR-P2-21 bound: a declared original length beyond
    /// DEFLATE's worst-case expansion is rejected before any allocation.
    #[test]
    fn impossible_declared_length_is_rejected() {
        let mut s = BinaryStream::new();
        s.write_u8(COMPRESSION_ZIP_BEST_SPEED);
        s.write_packed_int(MAX_DEFLATE_EXPANSION as u32 * 2)
            .unwrap();
        s.write_packed_int(1).unwrap();
        s.write_u8(0x00);
        s.rewind();
        assert_eq!(
            read_compressed(&mut s),
            Err(ZipStreamError::ImpossibleLengths)
        );
    }

    /// A frame declaring more compressed bytes than remain is truncated.
    #[test]
    fn truncated_frame_is_rejected() {
        let mut s = BinaryStream::new();
        s.write_u8(COMPRESSION_ZIP_BEST_SPEED);
        s.write_packed_int(10).unwrap();
        s.write_packed_int(100).unwrap(); // more than the remaining bytes
        s.rewind();
        assert_eq!(read_compressed(&mut s), Err(ZipStreamError::Truncated));
    }

    /// The port compresses and inflates at every ZIP level.
    #[test]
    fn every_zip_level_round_trips() {
        for tag in COMPRESSION_ZIP_BEST_SPEED..=COMPRESSION_ZIP_BEST_COMPRESSION {
            let payload = b"round trip payload ".repeat(40);
            let mut s = BinaryStream::new();
            write_compressed(&mut s, tag, &payload).unwrap();
            s.rewind();
            assert_eq!(
                read_compressed(&mut s),
                Ok(CompressionPayload::Zip {
                    original: payload.to_vec(),
                }),
                "tag {tag}"
            );
        }
    }

    #[test]
    fn error_display_is_informative() {
        let e = ZipStreamError::InvalidTag(9);
        assert!(e.to_string().contains("compression type"), "{e}");
        let e = ZipStreamError::ImpossibleLengths;
        assert!(e.to_string().contains("expansion"), "{e}");
        let boxed: &dyn std::error::Error = &ZipStreamError::Truncated;
        assert!(boxed.to_string().contains("end"));
    }
}
