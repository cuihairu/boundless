//! BW-01-02-04 zlib compression stream.
//!
//! The container the reference puts on the wire is: a string-encoded original
//! length, a string-encoded compressed length, then the raw zlib stream from
//! `compress2` at level 1 (`lib/network/zip_stream.cpp`). The reader refuses a
//! compressed length past the end of the stream, refuses a declared original
//! length beyond DEFLATE's 1032:1 worst-case expansion before it allocates
//! (CR-P2-21), and supports a caller-supplied destination buffer.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::stream::{BinaryStream, StreamError};

/// The compression level the reference uses by default (`ZipOStream`).
pub const DEFAULT_COMPRESS_LEVEL: u32 = 1;

/// Worst-case expansion of a DEFLATE stream (RFC 1951).
pub const MAX_DEFLATE_EXPANSION: u64 = 1032;

impl BinaryStream {
    /// Append a zlib container for `data`: packed original length, packed
    /// compressed length, then the raw zlib stream.
    pub fn write_compressed(&mut self, data: &[u8], level: u32) -> Result<&mut Self, StreamError> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
        encoder
            .write_all(data)
            .map_err(|_| StreamError::CompressionFailed)?;
        let compressed = encoder
            .finish()
            .map_err(|_| StreamError::CompressionFailed)?;
        self.write_packed_int(data.len() as u32)?;
        self.write_packed_int(compressed.len() as u32)?;
        self.write_blob_raw(&compressed);
        Ok(self)
    }

    /// Inflate a container written by `write_compressed`, allocating the
    /// payload. See `read_compressed_into` for the caller-supplied-buffer
    /// form the reference offers.
    pub fn read_compressed(&mut self) -> Result<Vec<u8>, StreamError> {
        let (orig_len, payload) = self.read_container()?;
        let out = self.inflate(payload.as_slice())?;
        if out.len() > orig_len {
            self.mark_error();
            return Err(StreamError::CompressionFailed);
        }
        Ok(out)
    }

    /// Inflate a container into `out`, which the caller owns and sizes. The
    /// reference refuses to inflate into a buffer too small for the declared
    /// original length, and so do we.
    pub fn read_compressed_into(&mut self, out: &mut [u8]) -> Result<usize, StreamError> {
        let (orig_len, payload) = self.read_container()?;
        if out.len() < orig_len {
            self.mark_error();
            return Err(StreamError::CompressionFailed);
        }
        let decoded = self.inflate(payload.as_slice())?;
        if decoded.len() > orig_len {
            self.mark_error();
            return Err(StreamError::CompressionFailed);
        }
        out[..decoded.len()].copy_from_slice(&decoded);
        Ok(decoded.len())
    }

    /// Read and validate the container header, returning the declared
    /// original length and the compressed payload. On failure the stream is
    /// left in the reference's error state.
    fn read_container(&mut self) -> Result<(usize, Vec<u8>), StreamError> {
        if self.has_error() {
            return Err(StreamError::Truncated);
        }
        let orig_len = self.read_packed_int()? as usize;
        let comp_len = self.read_packed_int()? as usize;
        if comp_len > self.remaining() {
            // `ZipIStream::init`: a declared length past the end of the
            // stream is reported rather than read out of bounds.
            self.mark_error();
            return Err(StreamError::Truncated);
        }
        // `ZipIStream::init`: the peer-declared size is checked against
        // DEFLATE's worst-case expansion before anything is allocated.
        if (orig_len as u64) > (comp_len as u64) * MAX_DEFLATE_EXPANSION {
            self.mark_error();
            return Err(StreamError::CompressionRatioExceeded);
        }
        let payload = self.read_blob_raw(comp_len)?;
        Ok((orig_len, payload))
    }

    /// Inflate a zlib payload. zlib's `uncompress` verifies the Adler-32
    /// trailer and reports a corrupt stream as an error.
    fn inflate(&mut self, payload: &[u8]) -> Result<Vec<u8>, StreamError> {
        let mut decoder = ZlibDecoder::new(payload);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|_| StreamError::CompressionFailed)?;
        Ok(out)
    }
}

/// The original length declared at the head of a container, without consuming
/// anything — the reference's `ZipIStream::peekOriginalLength`.
pub fn peek_original_length(bytes: &[u8]) -> Option<u32> {
    let head = *bytes.first()?;
    if head < crate::stream::PACKED_INT_EXTEND {
        return Some(head as u32);
    }
    let tail = bytes.get(1..4)?;
    Some(tail[0] as u32 | ((tail[1] as u32) << 8) | ((tail[2] as u32) << 16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::StreamError;

    /// The compressible pattern the reference test writes.
    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 7 + (i >> 8)) & 0xff) as u8).collect()
    }

    fn decode_hex(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }

    // Containers captured from the original ZipOStream.
    const ZIP_N100: &str = "646f78010164009bff00070e151c232a31383f464d545b626970777e858c939aa1a8afb6bdc4cbd2d9e0e7eef5fc030a11181f262d343b424950575e656c737a81888f969da4abb2b9c0c7ced5dce3eaf1f8ff060d141b222930373e454c535a61686f767d848b9299a0a7aeb58eb22e5b";
    const ZIP_N5000: &str = "ff881300ff99010078016360e7139551d632b4b077f30d894eca2c28af6bed993c6be18af5dbf61e397de9e683e7efbefe61e612949057d335b176f20c088f4bcd29ae6aece89f3677c9ea4d3b0f1c3f77f5cee3571f7ffc67e3159156d23430b773f5098e4accc82fab6de99e3473c1f2755bf71c3e75f1c6fd676fbffc66e214109753d531b672f4f00f8b4dc92eaa6c68ef9b3a67f1aa8d3bf61f3b7be5f6a3971fbeff63e5119652d4d037b375f10e8a4c48cf2bad69ee9a3863feb2b55b761f3a79e1fabda76f3eff62e4e0179355d136b27470f70b8d49ce2aaca86feb9d327bd1ca0ddbf71d3d73f9d6c317efbffd65e116925450d733b571f60a8c884fcb2da96eea9c307dded2359b771d3c71fedadd27af3ffda49a410c433320075f8cd037208771d2262a2089c923441934440392ae85cd200cc8a1596a532f208766a94dbd5a8b5af528f56284be2972d03548e81c90c3b765475c4012d14424cea0211a90f46c6b0fc6801c929d162a06e4c8eefd31b053a91b49c518a16f8a1c6cfd717a07e4b01dd8203220098f901069d0100d483a0e35e10d4800498ab824";
    // Two string-encoded lengths (1024, 999999) and no payload behind them.
    const BOGUS_OVERRUN: &str = "ff000400ff3f420f";

    #[test]
    fn decodes_the_original_container_n100() {
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex(ZIP_N100));
        s.rewind();
        assert_eq!(peek_original_length(&decode_hex(ZIP_N100)), Some(100));
        assert_eq!(s.read_compressed(), Ok(pattern(100)));
        assert!(s.at_end());
    }

    #[test]
    fn decodes_the_original_container_n5000() {
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex(ZIP_N5000));
        s.rewind();
        assert_eq!(peek_original_length(&decode_hex(ZIP_N5000)), Some(5000));
        assert_eq!(s.read_compressed(), Ok(pattern(5000)));
        assert!(s.at_end());
    }

    #[test]
    fn peek_does_not_consume() {
        let bytes = decode_hex(ZIP_N100);
        let total = bytes.len();
        assert_eq!(peek_original_length(&bytes), Some(100));
        assert_eq!(peek_original_length(&bytes), Some(100));
        // The whole container is still untouched.
        let mut s = BinaryStream::new();
        s.write_bytes(&bytes);
        s.rewind();
        assert_eq!(s.remaining(), total);
    }

    #[test]
    fn rust_written_container_round_trips() {
        let mut s = BinaryStream::new();
        s.write_compressed(&pattern(100), DEFAULT_COMPRESS_LEVEL)
            .unwrap();
        s.rewind();
        assert_eq!(s.read_compressed(), Ok(pattern(100)));
        assert!(s.at_end());
    }

    #[test]
    fn rust_container_has_the_reference_layout() {
        // The container starts with the packed original length, then the packed
        // compressed length, then the raw zlib stream (header 0x78).
        let mut s = BinaryStream::new();
        s.write_compressed(&pattern(5000), DEFAULT_COMPRESS_LEVEL)
            .unwrap();
        let bytes = s.as_bytes().to_vec();
        assert_eq!(peek_original_length(&bytes), Some(5000));
        let mut probe = BinaryStream::new();
        probe.write_bytes(&bytes);
        probe.rewind();
        let orig = probe.read_packed_int().unwrap();
        let comp = probe.read_packed_int().unwrap();
        assert_eq!(orig, 5000);
        assert!(comp > 0 && comp < 5000);
        assert_eq!(probe.read_u8().unwrap(), 0x78, "zlib header");
    }

    #[test]
    fn rejects_compressed_length_past_end_of_stream() {
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex(BOGUS_OVERRUN));
        s.rewind();
        assert_eq!(s.read_compressed(), Err(StreamError::Truncated));
        assert!(s.has_error());
        assert_eq!(s.remaining(), 0);
    }

    #[test]
    fn rejects_original_length_beyond_deflate_ratio() {
        // A real zlib blob of the small pattern, declared as the largest
        // packed int (2^24 - 1) — far beyond what a ~100-byte DEFLATE payload
        // could ever expand to, so the declaration is bogus.
        let mut inner = BinaryStream::new();
        inner
            .write_compressed(&pattern(64), DEFAULT_COMPRESS_LEVEL)
            .unwrap();
        let bytes = inner.as_bytes().to_vec();
        let comp_len = bytes.len();

        let mut container = BinaryStream::new();
        container.write_packed_int((1 << 24) - 1).unwrap();
        container.write_packed_int(comp_len as u32).unwrap();
        container.write_blob_raw(&bytes);

        container.rewind();
        assert_eq!(
            container.read_compressed(),
            Err(StreamError::CompressionRatioExceeded)
        );
        assert!(container.has_error());
    }

    #[test]
    fn caller_supplied_buffer_too_small_is_rejected() {
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex(ZIP_N100));
        s.rewind();
        let mut small = [0u8; 16];
        assert_eq!(
            s.read_compressed_into(&mut small),
            Err(StreamError::CompressionFailed)
        );
        assert!(s.has_error());
    }

    #[test]
    fn caller_supplied_buffer_large_enough_holds_payload() {
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex(ZIP_N100));
        s.rewind();
        let mut big = vec![0u8; 200];
        let n = s.read_compressed_into(&mut big).unwrap();
        assert_eq!(n, 100);
        assert_eq!(&big[..100], &pattern(100)[..]);
    }

    /// For this payload the Rust compressor and the reference zlib emit the
    /// very same container, prefix and all — the strongest possible form of
    /// "both sides decode the same fixture".
    #[test]
    fn matches_the_original_container_byte_for_byte() {
        let mut s = BinaryStream::new();
        s.write_compressed(&pattern(100), DEFAULT_COMPRESS_LEVEL)
            .unwrap();
        assert_eq!(&decode_hex(ZIP_N100), s.as_bytes());
    }
}
