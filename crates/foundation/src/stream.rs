//! BW-01-02 fixed-width and variable-length binary streaming.
//!
//! Wire format is little-endian. The reference implementation documents this
//! explicitly in `lib/cstdmf/binary_stream.ipp`: `BW_HTON*` convert host
//! order to *little*-endian, and are identity on little-endian hosts, so every
//! integer and float is written as-is on x86-64.
//!
//! Other verified encodings:
//! * strings are a packed-int length prefix plus the raw bytes, with no NUL
//!   terminator (`binary_stream.hpp` `appendString`);
//! * a packed int is one byte for `0..=254`, otherwise `0xFF` followed by the
//!   three low bytes little-endian, capped at `1 << 24`;
//! * a container is a fixed 4-byte element count followed by its elements;
//! * a compressed payload is a packed uncompressed length, a packed compressed
//!   length, then the raw zlib bytes of `compress2` (reference `ZipOStream`
//!   destructor / `ZipIStream::init`, framing via `writeStringLength` =
//!   `writePackedInt`);
//! * an over-long read sets the stream error and clamps the read cursor to the
//!   end instead of aborting, so following reads fail the same way.

/// Maximum size of the zero buffer a failed bounded read hands out.
///
/// `BS_BUFF_MAX_SIZE` in `binary_stream.hpp`.
pub const STREAM_ERROR_BUFFER_MAX: usize = 2048;

/// The maximum length representable by a packed int.
pub const MAX_PACKED_INT: u32 = (1 << 24) - 1;

/// The marker byte introducing the four-byte form of a packed int.
pub const PACKED_INT_EXTEND: u8 = 0xFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    /// The read would run past the end of the buffer.
    Truncated,
    /// The requested length exceeds what a packed int can carry.
    PackedIntTooLarge,
    /// The requested level is outside the reference ZIP_1..ZIP_9 range.
    InvalidCompressionLevel,
    /// zlib compress or uncompress failed.
    Zlib,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "stream read would run past the end of the buffer"),
            Self::PackedIntTooLarge => write!(f, "packed int length exceeds 2^24 - 1"),
            Self::InvalidCompressionLevel => write!(
                f,
                "compression level must be 1..=9 (reference ZIP_1..ZIP_9)"
            ),
            Self::Zlib => write!(f, "zlib compress or uncompress failed"),
        }
    }
}

impl std::error::Error for StreamError {}

/// Compression level, matching the reference `BWCompressionType` ZIP_1..ZIP_9.
///
/// The reference default internal/external compression levels are configuration
/// resolved; `ZipOStream` itself defaults to level 1 (`Z_BEST_SPEED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionLevel(u8);

impl CompressionLevel {
    /// Reference `BW_COMPRESSION_ZIP_BEST_SPEED` and the `ZipOStream` default.
    pub const BEST_SPEED: Self = Self(1);
    /// Reference `BW_COMPRESSION_ZIP_BEST_COMPRESSION`.
    pub const BEST_COMPRESSION: Self = Self(9);

    /// Level 1..=9; anything else is rejected as the reference would.
    pub fn new(level: u8) -> Result<Self, StreamError> {
        if (1..=9).contains(&level) {
            Ok(Self(level))
        } else {
            Err(StreamError::InvalidCompressionLevel)
        }
    }

    /// The level as passed through to zlib.
    pub fn get(self) -> u8 {
        self.0
    }
}

impl Default for CompressionLevel {
    fn default() -> Self {
        Self::BEST_SPEED
    }
}

/// A byte buffer that supports sequential reading and sequential writing,
/// mirroring the reference `BinaryIStream` / `BinaryOStream` pair.
#[derive(Debug, Clone, Default)]
pub struct BinaryStream {
    buf: Vec<u8>,
    read: usize,
    error: bool,
}

impl BinaryStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a stream pre-filled with a fixed-width value.
    pub fn from_u32_le(value: u32) -> Self {
        let mut s = Self::default();
        s.write_u32(value);
        s
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Bytes written so far.
    pub fn length(&self) -> usize {
        self.buf.len()
    }

    /// Bytes left to read.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.read
    }

    /// Zero once the read cursor is exhausted.
    pub fn at_end(&self) -> bool {
        self.read >= self.buf.len()
    }

    /// The stream is in an error state after a failed read.
    pub fn has_error(&self) -> bool {
        self.error
    }

    /// Raw bytes written so far, including already-read prefix.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Bytes not yet read, from the read cursor on.
    pub fn unread_bytes(&self) -> &[u8] {
        &self.buf[self.read..]
    }

    /// Reset the read cursor to the start without changing the contents.
    pub fn rewind(&mut self) {
        self.read = 0;
    }

    /// Run the read cursor to the end.
    pub fn finish(&mut self) {
        self.read = self.buf.len();
    }

    // ------------------------------------------------------------------
    // Writing
    // ------------------------------------------------------------------

    pub fn write_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(bytes);
        self
    }

    pub fn write_u8(&mut self, value: u8) -> &mut Self {
        self.buf.push(value);
        self
    }

    pub fn write_i8(&mut self, value: i8) -> &mut Self {
        self.buf.push(value as u8);
        self
    }

    pub fn write_bool(&mut self, value: bool) -> &mut Self {
        self.buf.push(value as u8);
        self
    }

    pub fn write_u16(&mut self, value: u16) -> &mut Self {
        self.buf.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn write_i16(&mut self, value: i16) -> &mut Self {
        self.write_u16(value as u16);
        self
    }

    pub fn write_u32(&mut self, value: u32) -> &mut Self {
        self.buf.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn write_i32(&mut self, value: i32) -> &mut Self {
        self.write_u32(value as u32);
        self
    }

    pub fn write_u64(&mut self, value: u64) -> &mut Self {
        self.buf.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn write_i64(&mut self, value: i64) -> &mut Self {
        self.write_u64(value as u64);
        self
    }

    pub fn write_f32(&mut self, value: f32) -> &mut Self {
        self.write_u32(value.to_bits());
        self
    }

    pub fn write_f64(&mut self, value: f64) -> &mut Self {
        self.write_u64(value.to_bits());
        self
    }

    /// A packed integer length or index.
    pub fn write_packed_int(&mut self, value: u32) -> Result<&mut Self, StreamError> {
        if value <= MAX_PACKED_INT {
            if value < PACKED_INT_EXTEND as u32 {
                self.write_u8(value as u8);
            } else {
                // `BW_PACK3`: the three low bytes in little-endian order.
                self.buf.push(PACKED_INT_EXTEND);
                self.buf.extend_from_slice(&[
                    (value & 0xFF) as u8,
                    ((value >> 8) & 0xFF) as u8,
                    ((value >> 16) & 0xFF) as u8,
                ]);
            }
            Ok(self)
        } else {
            Err(StreamError::PackedIntTooLarge)
        }
    }

    /// A string: packed length prefix plus the raw bytes, no terminator.
    pub fn write_string(&mut self, text: &str) -> Result<&mut Self, StreamError> {
        self.write_packed_int(text.len() as u32)?;
        self.buf.extend_from_slice(text.as_bytes());
        Ok(self)
    }

    /// A fixed-size container element count, written as a plain u32.
    pub fn write_count(&mut self, count: u32) -> &mut Self {
        self.write_u32(count)
    }

    /// Append a raw digest or blob with no length prefix.
    pub fn write_blob_raw(&mut self, blob: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(blob);
        self
    }

    /// A length-prefixed blob, used for binary payloads.
    pub fn write_blob(&mut self, blob: &[u8]) -> Result<&mut Self, StreamError> {
        self.write_packed_int(blob.len() as u32)?;
        self.buf.extend_from_slice(blob);
        Ok(self)
    }

    // ------------------------------------------------------------------
    // Reading
    // ------------------------------------------------------------------

    fn take(&mut self, n: usize) -> Result<&[u8], StreamError> {
        let end = self.read + n;
        if end <= self.buf.len() {
            let out = &self.buf[self.read..end];
            self.read = end;
            Ok(out)
        } else {
            // Reference behaviour: mark the error and clamp the read cursor to
            // the end so subsequent reads fail identically.
            self.error = true;
            self.read = self.buf.len();
            Err(StreamError::Truncated)
        }
    }

    /// Read a fixed-width little-endian value of exactly `N` bytes.
    fn take_n<const N: usize>(&mut self) -> Result<[u8; N], StreamError> {
        if self.read + N <= self.buf.len() {
            let mut out = [0u8; N];
            out.copy_from_slice(&self.buf[self.read..self.read + N]);
            self.read += N;
            Ok(out)
        } else {
            self.error = true;
            self.read = self.buf.len();
            Err(StreamError::Truncated)
        }
    }

    /// Next byte without consuming it. Returns `None` at the end.
    pub fn peek(&self) -> Option<u8> {
        self.buf.get(self.read).copied()
    }

    pub fn read_u8(&mut self) -> Result<u8, StreamError> {
        Ok(self.take(1)?[0])
    }

    pub fn read_i8(&mut self) -> Result<i8, StreamError> {
        self.read_u8().map(|v| v as i8)
    }

    pub fn read_bool(&mut self) -> Result<bool, StreamError> {
        self.read_u8().map(|v| v != 0)
    }

    pub fn read_u16(&mut self) -> Result<u16, StreamError> {
        Ok(u16::from_le_bytes(self.take_n()?))
    }

    pub fn read_i16(&mut self) -> Result<i16, StreamError> {
        self.read_u16().map(|v| v as i16)
    }

    pub fn read_u32(&mut self) -> Result<u32, StreamError> {
        Ok(u32::from_le_bytes(self.take_n()?))
    }

    pub fn read_i32(&mut self) -> Result<i32, StreamError> {
        self.read_u32().map(|v| v as i32)
    }

    pub fn read_u64(&mut self) -> Result<u64, StreamError> {
        Ok(u64::from_le_bytes(self.take_n()?))
    }

    pub fn read_i64(&mut self) -> Result<i64, StreamError> {
        self.read_u64().map(|v| v as i64)
    }

    pub fn read_f32(&mut self) -> Result<f32, StreamError> {
        self.read_u32().map(f32::from_bits)
    }

    pub fn read_f64(&mut self) -> Result<f64, StreamError> {
        self.read_u64().map(f64::from_bits)
    }

    pub fn read_packed_int(&mut self) -> Result<u32, StreamError> {
        let head = self.read_u8()?;
        if head < PACKED_INT_EXTEND {
            Ok(head as u32)
        } else {
            let tail = self.take(3)?;
            Ok(tail[0] as u32 | ((tail[1] as u32) << 8) | ((tail[2] as u32) << 16))
        }
    }

    pub fn read_string(&mut self) -> Result<String, StreamError> {
        let len = self.read_packed_int()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| StreamError::Truncated)
    }

    pub fn read_count(&mut self) -> Result<u32, StreamError> {
        self.read_u32()
    }

    /// A raw, unprefixed blob of exactly `n` bytes.
    pub fn read_blob_raw(&mut self, n: usize) -> Result<Vec<u8>, StreamError> {
        Ok(self.take(n)?.to_vec())
    }

    pub fn read_blob(&mut self) -> Result<Vec<u8>, StreamError> {
        let len = self.read_packed_int()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    // ------------------------------------------------------------------
    // Compression stream (reference ZipOStream / ZipIStream)
    // ------------------------------------------------------------------

    /// Append a compressed payload in the reference framing: a packed
    /// uncompressed length, a packed compressed length, then the zlib bytes
    /// produced by `compress2(src, level)`.
    ///
    /// Compresses into a temporary buffer first, so a failed encode leaves the
    /// stream untouched.
    pub fn write_compressed(
        &mut self,
        plaintext: &[u8],
        level: CompressionLevel,
    ) -> Result<&mut Self, StreamError> {
        let mut encoder = flate2::write::ZlibEncoder::new(
            Vec::new(),
            flate2::Compression::new(level.get() as u32),
        );
        use std::io::Write as _;
        encoder
            .write_all(plaintext)
            .map_err(|_| StreamError::Zlib)?;
        let body = encoder.finish().map_err(|_| StreamError::Zlib)?;

        self.write_packed_int(plaintext.len() as u32)?;
        self.write_packed_int(body.len() as u32)?;
        self.buf.extend_from_slice(&body);
        Ok(self)
    }

    /// Read a payload written by [`BinaryStream::write_compressed`],
    /// decompressing the zlib body and returning the plaintext.
    pub fn read_compressed(&mut self) -> Result<Vec<u8>, StreamError> {
        let orig_len = self.read_packed_int()? as usize;
        let body_len = self.read_packed_int()? as usize;
        let body = self.read_blob_raw(body_len)?;

        let mut decoder = flate2::write::ZlibDecoder::new(Vec::new());
        use std::io::Write as _;
        let plaintext = match decoder.write_all(&body).and_then(|_| decoder.finish()) {
            Ok(out) => out,
            Err(_) => {
                self.error = true;
                return Err(StreamError::Zlib);
            }
        };
        // zlib already reports the true decompressed size, so a mismatch means
        // the stream is corrupt or the buffer is too small.
        if plaintext.len() != orig_len {
            self.error = true;
            return Err(StreamError::Zlib);
        }
        Ok(plaintext)
    }
}

/// Number of bytes a packed int occupies.
pub fn packed_int_size(value: u32) -> usize {
    if value < PACKED_INT_EXTEND as u32 {
        1
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_width_roundtrip() {
        let mut s = BinaryStream::new();
        s.write_i8(-7)
            .write_bool(true)
            .write_i16(-1234)
            .write_i32(-1_000_000)
            .write_i64(-9_000_000_000)
            .write_f32(-1.5)
            .write_f64(-2.75);
        s.rewind();

        assert_eq!(s.read_i8(), Ok(-7));
        assert_eq!(s.read_bool(), Ok(true));
        assert_eq!(s.read_i16(), Ok(-1234));
        assert_eq!(s.read_i32(), Ok(-1_000_000));
        assert_eq!(s.read_i64(), Ok(-9_000_000_000));
        assert_eq!(s.read_f32(), Ok(-1.5));
        assert_eq!(s.read_f64(), Ok(-2.75));
        assert!(s.at_end());
    }

    #[test]
    fn integers_are_little_endian() {
        let mut s = BinaryStream::new();
        s.write_u32(0x0102_0304);
        assert_eq!(s.as_bytes(), &[0x04, 0x03, 0x02, 0x01]);
        s.rewind();
        assert_eq!(s.read_u32(), Ok(0x0102_0304));
    }

    #[test]
    fn packed_int_boundaries() {
        for value in [0u32, 1, 254, 255, 256, 1000, 65535, 65536, MAX_PACKED_INT] {
            let mut s = BinaryStream::new();
            s.write_packed_int(value).unwrap();
            assert_eq!(s.as_bytes().len(), packed_int_size(value));
            s.rewind();
            assert_eq!(s.read_packed_int(), Ok(value));
        }
    }

    #[test]
    fn packed_int_length_prefix_forms() {
        let mut small = BinaryStream::new();
        small.write_packed_int(0xAB).unwrap();
        assert_eq!(small.as_bytes(), &[0xAB]);

        let mut big = BinaryStream::new();
        big.write_packed_int(0x0012_3456).unwrap();
        assert_eq!(big.as_bytes(), &[0xFF, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn packed_int_rejects_too_large() {
        let mut s = BinaryStream::new();
        let result = s
            .write_packed_int(MAX_PACKED_INT + 1)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(result, StreamError::PackedIntTooLarge);
    }

    #[test]
    fn strings_are_length_prefixed_without_terminator() {
        let mut s = BinaryStream::new();
        s.write_string("").unwrap();
        s.write_string("a").unwrap();
        s.write_string("hello").unwrap();
        s.rewind();
        assert_eq!(s.read_string(), Ok(String::new()));
        assert_eq!(s.read_string(), Ok("a".to_string()));
        assert_eq!(s.read_string(), Ok("hello".to_string()));

        // 'hello' contributes 1 byte of length plus 5 bytes.
        assert_eq!(s.length(), 1 + 1 + 1 + 1 + 5);
    }

    #[test]
    fn non_ascii_strings_roundtrip() {
        let mut s = BinaryStream::new();
        s.write_string("实体与太空").unwrap();
        s.rewind();
        assert_eq!(s.read_string(), Ok("实体与太空".to_string()));
    }

    /// Length prefixing preserves embedded zeros: no terminator is written,
    /// so `a\0b` round-trips instead of truncating at the NUL.
    #[test]
    fn embedded_zeros_survive_string_roundtrip() {
        let mut s = BinaryStream::new();
        s.write_string("a\0b").unwrap();
        s.rewind();
        assert_eq!(s.read_string(), Ok("a\0b".to_string()));
        assert_eq!(s.length(), 1 + 3);
    }

    #[test]
    fn strings_embed_zero_and_track_remaining() {
        // The reference string has no terminator, so an embedded NUL is data,
        // not an end marker.
        let mut s = BinaryStream::new();
        s.write_string("a\0b\0").unwrap();
        s.write_string("").unwrap();
        assert_eq!(s.as_bytes(), &[0x04, b'a', 0, b'b', 0, 0x00]);
        s.rewind();
        assert_eq!(s.remaining(), 6);
        assert_eq!(s.read_string(), Ok("a\0b\0".to_string()));
        assert_eq!(s.remaining(), 1);
        assert_eq!(s.read_string(), Ok(String::new()));
        assert_eq!(s.remaining(), 0);
        assert!(s.at_end());
    }

    #[test]
    fn counts_are_plain_u32() {
        let mut s = BinaryStream::new();
        s.write_count(3).write_u8(1).write_u8(2).write_u8(3);
        s.rewind();
        assert_eq!(s.read_count(), Ok(3));
        for expected in [1u8, 2, 3] {
            assert_eq!(s.read_u8(), Ok(expected));
        }
    }

    #[test]
    fn truncated_read_marks_error_and_clamps() {
        let mut s = BinaryStream::new();
        s.write_u32(7).write_u8(8);
        s.rewind();
        assert_eq!(s.read_u8(), Ok(7));
        // Only 4 bytes of the requested 8 remain.
        assert_eq!(s.read_u64(), Err(StreamError::Truncated));
        assert!(s.has_error());
        assert!(s.at_end());
        // Following reads keep failing rather than recovering.
        assert_eq!(s.read_u8(), Err(StreamError::Truncated));
    }

    #[test]
    fn peek_does_not_consume() {
        let mut s = BinaryStream::new();
        s.write_u8(41);
        s.rewind();
        assert_eq!(s.peek(), Some(41));
        assert_eq!(s.remaining(), 1);
        assert_eq!(s.read_u8(), Ok(41));
        assert_eq!(s.peek(), None);
    }

    #[test]
    fn finish_consumes_everything() {
        let mut s = BinaryStream::new();
        s.write_string("payload").unwrap();
        s.finish();
        assert!(s.at_end());
        assert!(s.unread_bytes().is_empty());
    }

    #[test]
    fn raw_blobs_have_no_prefix() {
        let mut s = BinaryStream::new();
        s.write_blob_raw(&[0xDE, 0xAD, 0xBE, 0xEF]);
        s.rewind();
        assert_eq!(s.read_blob_raw(4), Ok(vec![0xDE, 0xAD, 0xBE, 0xEF]));
    }

    #[test]
    fn prefixed_blobs_roundtrip() {
        let mut s = BinaryStream::new();
        s.write_blob(&[1, 2, 3]).unwrap();
        s.rewind();
        assert_eq!(s.read_blob(), Ok(vec![1, 2, 3]));
    }
    #[test]
    fn error_messages_display() {
        let t = StreamError::Truncated.to_string();
        assert!(t.contains("past the end"), "{t}");
        let p = StreamError::PackedIntTooLarge.to_string();
        assert!(p.contains("2^24"), "{p}");
        // Both are usable as std::error::Error via Display.
        let boxed: &dyn std::error::Error = &StreamError::Truncated;
        assert_eq!(boxed.to_string(), t);
    }

    #[test]
    fn from_u32_le_prefills_and_capacity_tracks() {
        let s = BinaryStream::from_u32_le(0x01020304);
        assert_eq!(s.as_bytes(), [0x04, 0x03, 0x02, 0x01]);
        assert_eq!(s.capacity(), 4);
    }

    #[test]
    fn write_bytes_appends_raw() {
        let mut s = BinaryStream::new();
        s.write_bytes(b"ab").write_bytes(&[0x00, 0xFF]);
        assert_eq!(s.as_bytes(), b"ab\x00\xff");
    }

    // ------------------------------------------------------------------
    // BW-01-02-01: golden bytes captured from the reference wire format.
    // ------------------------------------------------------------------

    /// Fill the same sequence as `fixed_width_roundtrip` and compare against
    /// bytes produced independently (little-endian host, reference `BW_HTON*`
    /// identity). Guards the encoding, not just self-consistency.
    #[test]
    fn golden_fixed_width_bytes() {
        let mut s = BinaryStream::new();
        s.write_i8(-7)
            .write_bool(true)
            .write_i16(-1234)
            .write_i32(-1_000_000)
            .write_i64(-9_000_000_000)
            .write_f32(-1.5)
            .write_f64(-2.75);

        let golden: [u8; 28] = [
            0xF9, // i8  -7
            0x01, // bool true
            0x2E, 0xFB, // i16 -1234
            0xC0, 0xBD, 0xF0, 0xFF, // i32 -1000000
            0x00, 0xE6, 0x8E, 0xE7, 0xFD, 0xFF, 0xFF, 0xFF, // i64 -9000000000
            0x00, 0x00, 0xC0, 0xBF, // f32 -1.5
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0xC0, // f64 -2.75
        ];
        assert_eq!(s.as_bytes(), &golden);

        // Byte-exact roundtrip of the same buffer.
        s.rewind();
        assert_eq!(s.read_i8(), Ok(-7));
        assert_eq!(s.read_bool(), Ok(true));
        assert_eq!(s.read_i16(), Ok(-1234));
        assert_eq!(s.read_i32(), Ok(-1_000_000));
        assert_eq!(s.read_i64(), Ok(-9_000_000_000));
        assert_eq!(s.read_f32(), Ok(-1.5));
        assert_eq!(s.read_f64(), Ok(-2.75));
        assert!(s.at_end());
    }

    #[test]
    fn golden_fixed_width_bytes_unsigned_bounds() {
        let mut s = BinaryStream::new();
        s.write_u8(0xFF)
            .write_u16(0xFFFF)
            .write_u32(0xFFFF_FFFF)
            .write_u64(u64::MAX);
        assert_eq!(
            s.as_bytes(),
            &[
                0xFF, //
                0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            ]
        );
        s.rewind();
        assert_eq!(s.read_u8(), Ok(0xFF));
        assert_eq!(s.read_u16(), Ok(0xFFFF));
        assert_eq!(s.read_u32(), Ok(0xFFFF_FFFF));
        assert_eq!(s.read_u64(), Ok(u64::MAX));
    }

    #[test]
    fn golden_float_negative_zero_and_infinity() {
        let mut s = BinaryStream::new();
        s.write_f32(-0.0)
            .write_f32(f32::INFINITY)
            .write_f64(f64::NEG_INFINITY);
        assert_eq!(
            s.as_bytes(),
            &[
                0x00, 0x00, 0x00, 0x80, // f32 -0.0
                0x00, 0x00, 0x80, 0x7F, // f32 +inf
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF0, 0xFF, // f64 -inf
            ]
        );
        s.rewind();
        assert!(s.read_f32().unwrap().is_sign_negative());
        assert_eq!(s.read_f32(), Ok(f32::INFINITY));
        assert_eq!(s.read_f64(), Ok(f64::NEG_INFINITY));
    }

    // ------------------------------------------------------------------
    // BW-01-02-04: compression stream (reference ZipOStream / ZipIStream).
    // ------------------------------------------------------------------

    /// Plaintext behind the reference zlib fixture below.
    fn reference_plaintext() -> Vec<u8> {
        let mut v = b"BW compression fixture ".to_vec();
        v.extend(0u8..=255);
        v
    }

    /// Bytes captured from the system zlib (the same library `compress2` uses)
    /// at level 1 = `Z_BEST_SPEED`: `[packed 279][packed 290][zlib body]`.
    fn reference_zlib_fixture() -> BinaryStream {
        let body = hex_to_bytes(concat!(
            "7801011701e8fe425720636f6d7072657373696f6e20666978747572652000",
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
            "2122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40",
            "4142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f60",
            "6162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f80",
            "8182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fa0",
            "a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf",
            "c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf",
            "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
            "c72f8833"
        ));
        let mut s = BinaryStream::new();
        s.write_packed_int(279).unwrap();
        s.write_packed_int(body.len() as u32).unwrap();
        s.write_blob_raw(&body);
        s
    }

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn compressed_reads_reference_zlib_fixture() {
        // Both partners can decode the same fixture: decoding the reference
        // bytes must yield the reference plaintext.
        let mut s = reference_zlib_fixture();
        s.rewind();
        assert_eq!(s.read_compressed(), Ok(reference_plaintext()));
        assert!(s.at_end());
    }

    #[test]
    fn compressed_frame_shape_matches_reference() {
        let mut ours = BinaryStream::new();
        ours.write_compressed(&reference_plaintext(), CompressionLevel::BEST_SPEED)
            .unwrap();

        // Same framing, and the zlib body is byte-identical to the system
        // zlib output the reference `compress2` produces.
        assert_eq!(ours.as_bytes(), reference_zlib_fixture().as_bytes());
    }

    #[test]
    fn compressed_roundtrip_uncompressible_and_empty() {
        for payload in [
            Vec::new(),
            vec![0u8; 5000],
            (0..2000u32)
                .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
                .collect(),
        ] {
            let mut s = BinaryStream::new();
            s.write_compressed(&payload, CompressionLevel::BEST_COMPRESSION)
                .unwrap();
            let len = s.length();
            s.rewind();
            assert_eq!(s.read_compressed(), Ok(payload));
            assert_eq!(s.remaining(), 0);
            // Two packed prefixes plus a non-empty zlib stream.
            assert!(len >= 2);
        }
    }

    #[test]
    fn compressed_roundtrip_all_levels() {
        let payload = reference_plaintext();
        for level in 1..=9u8 {
            let mut s = BinaryStream::new();
            s.write_compressed(&payload, CompressionLevel::new(level).unwrap())
                .unwrap();
            s.rewind();
            assert_eq!(s.read_compressed(), Ok(payload.clone()), "level {level}");
        }
    }

    #[test]
    fn compression_level_rejects_out_of_range() {
        assert_eq!(
            CompressionLevel::new(0).unwrap_err(),
            StreamError::InvalidCompressionLevel
        );
        assert_eq!(
            CompressionLevel::new(10).unwrap_err(),
            StreamError::InvalidCompressionLevel
        );
        assert_eq!(CompressionLevel::default(), CompressionLevel::BEST_SPEED);
        assert_eq!(CompressionLevel::BEST_COMPRESSION.get(), 9);
    }

    #[test]
    fn compressed_rejects_corrupt_body() {
        let mut s = BinaryStream::new();
        s.write_packed_int(64).unwrap();
        s.write_packed_int(4).unwrap();
        s.write_blob_raw(&[0xDE, 0xAD, 0xBE, 0xEF]);
        s.rewind();
        assert_eq!(s.read_compressed(), Err(StreamError::Zlib));
        assert!(s.has_error());
    }

    #[test]
    fn compressed_rejects_length_mismatch() {
        // Valid zlib body but a lying original length.
        let mut encoded = BinaryStream::new();
        encoded
            .write_compressed(b"hello", CompressionLevel::BEST_SPEED)
            .unwrap();
        let body = encoded.as_bytes()[2..].to_vec();

        let mut s = BinaryStream::new();
        s.write_packed_int(99).unwrap();
        s.write_packed_int(body.len() as u32).unwrap();
        s.write_blob_raw(&body);
        s.rewind();
        assert_eq!(s.read_compressed(), Err(StreamError::Zlib));
    }

    #[test]
    fn compressed_truncated_body_reports_truncation() {
        let s = reference_zlib_fixture();
        let cut = s.length() - 10;
        let mut truncated = BinaryStream::new();
        truncated.write_blob_raw(&s.as_bytes()[..cut]);
        truncated.rewind();
        assert_eq!(truncated.read_compressed(), Err(StreamError::Truncated));
    }
}
