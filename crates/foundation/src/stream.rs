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
//! * a possibly-null blob is either a length-prefixed blob or, when null or
//!   empty, a zero length plus a one-byte flag telling them apart
//!   (`blob_or_null.cpp`);
//! * an over-long read sets the stream error and clamps the read cursor to the
//!   end instead of aborting, so following reads fail the same way (`retrieve`
//!   in `lib/cstdmf/memory_stream.ipp`).

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
    /// The zlib container could not be encoded or decoded.
    CompressionFailed,
    /// A compressed container declares an original length beyond what its
    /// compressed payload could ever inflate to (DEFLATE's worst case is
    /// 1032:1), so the declaration cannot come from a real zlib stream.
    CompressionRatioExceeded,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "stream read would run past the end of the buffer"),
            Self::PackedIntTooLarge => write!(f, "packed int length exceeds 2^24 - 1"),
            Self::CompressionFailed => write!(f, "zlib compression container is invalid"),
            Self::CompressionRatioExceeded => {
                write!(
                    f,
                    "declared original length exceeds the deflate expansion ratio"
                )
            }
        }
    }
}

impl std::error::Error for StreamError {}

/// A byte buffer that supports sequential reading and sequential writing,
/// mirroring the reference `BinaryIStream` / `BinaryOStream` pair.
#[derive(Debug, Clone)]
pub struct BinaryStream {
    buf: Vec<u8>,
    read: usize,
    error: bool,
}

impl Default for BinaryStream {
    fn default() -> Self {
        Self {
            // `MemoryOStream( int size = 64 )`: the reference output
            // stream constructor starts with a 64-byte buffer, so small
            // payloads never pay growth reallocations.
            buf: Vec::with_capacity(64),
            read: 0,
            error: false,
        }
    }
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

    /// Create a read stream over an existing byte slice, cursor at the start.
    /// Used by `MGMPacket::read` to hand each enclosed message its own
    /// bounded stream, mirroring the reference `MemoryIStream msgstream`.
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self {
            buf: bytes.to_vec(),
            read: 0,
            error: false,
        }
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

    /// Record an unrecoverable read fault, the same way the reference sets
    /// `error_ = true` before refusing to hand out further data.
    pub(crate) fn mark_error(&mut self) {
        self.error = true;
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

    /// A possibly-null blob (`lib/cstdmf/blob_or_null.cpp`). Present,
    /// non-empty data goes out as a length-prefixed blob; a null or an empty
    /// blob writes a zero length plus one flag byte marking which it was, so
    /// the two cannot be confused on the way back.
    pub fn write_blob_or_null(&mut self, blob: Option<&[u8]>) -> Result<&mut Self, StreamError> {
        match blob {
            Some(data) if !data.is_empty() => self.write_blob(data),
            Some(_) => {
                self.write_packed_int(0)?;
                self.buf.push(1);
                Ok(self)
            }
            None => {
                self.write_packed_int(0)?;
                self.buf.push(0);
                Ok(self)
            }
        }
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

    /// The inverse of `write_blob_or_null`: `None` for a null blob, `Some`
    /// for a present one (possibly empty).
    pub fn read_blob_or_null(&mut self) -> Result<Option<Vec<u8>>, StreamError> {
        let len = self.read_packed_int()? as usize;
        if len > 0 {
            Ok(Some(self.take(len)?.to_vec()))
        } else {
            let is_not_null = self.read_u8()?;
            Ok(if is_not_null != 0 {
                Some(Vec::new())
            } else {
                None
            })
        }
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

    /// The BW-01-02-01 acceptance: the port reproduces the reference's exact
    /// bytes for every fixed-width primitive, and reads them back — the
    /// original fixtures round-trip byte for byte.
    ///
    /// The expected bytes are derived by hand from the reference's own wire
    /// format (`lib/cstdmf/binary_stream.ipp`): the network order is
    /// little-endian (`BW_HTON*` are identity on a little-endian host, so
    /// every value is written as-is on x86-64), a byte/bool is one raw byte,
    /// an integer is its two's-complement little-endian form, and a
    /// float/double is its IEEE-754 bit pattern in little-endian order
    /// (`bw_netlong`/`bw_netlonglong`).
    #[test]
    fn reproduces_the_reference_fixed_width_bytes() {
        let mut s = BinaryStream::new();
        s.write_u8(0xAB);
        s.write_i8(-7);
        s.write_bool(true);
        s.write_bool(false);
        s.write_u16(0x1234);
        s.write_i16(-1234);
        s.write_u32(0x0102_0304);
        s.write_i32(-1_000_000);
        s.write_u64(0x0102_0304_0506_0708);
        s.write_i64(-9_000_000_000);
        s.write_f32(-1.5);
        s.write_f32(1.0);
        s.write_f64(-2.75);
        s.write_f64(2.75);

        assert_eq!(
            s.as_bytes(),
            &[
                0xAB, // u8 0xAB
                0xF9, // i8 -7
                0x01, // bool true
                0x00, // bool false
                0x34, 0x12, // u16 0x1234
                0x2E, 0xFB, // i16 -1234
                0x04, 0x03, 0x02, 0x01, // u32 0x01020304
                0xC0, 0xBD, 0xF0, 0xFF, // i32 -1_000_000
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // u64 0x0102030405060708
                0x00, 0xE6, 0x8E, 0xE7, 0xFD, 0xFF, 0xFF, 0xFF, // i64 -9_000_000_000
                0x00, 0x00, 0xC0, 0xBF, // f32 -1.5
                0x00, 0x00, 0x80, 0x3F, // f32 1.0
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0xC0, // f64 -2.75
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x40, // f64 2.75
            ]
        );
        // The reference's sizeof per primitive, in stream order.
        assert_eq!(
            s.length(),
            1 + 1 + 1 + 1 + 2 + 2 + 4 + 4 + 8 + 8 + 4 + 4 + 8 + 8
        );

        // Reading those same bytes back reproduces every value.
        s.rewind();
        assert_eq!(s.read_u8(), Ok(0xAB));
        assert_eq!(s.read_i8(), Ok(-7));
        assert_eq!(s.read_bool(), Ok(true));
        assert_eq!(s.read_bool(), Ok(false));
        assert_eq!(s.read_u16(), Ok(0x1234));
        assert_eq!(s.read_i16(), Ok(-1234));
        assert_eq!(s.read_u32(), Ok(0x0102_0304));
        assert_eq!(s.read_i32(), Ok(-1_000_000));
        assert_eq!(s.read_u64(), Ok(0x0102_0304_0506_0708));
        assert_eq!(s.read_i64(), Ok(-9_000_000_000));
        assert_eq!(s.read_f32(), Ok(-1.5));
        assert_eq!(s.read_f32(), Ok(1.0));
        assert_eq!(s.read_f64(), Ok(-2.75));
        assert_eq!(s.read_f64(), Ok(2.75));
        assert!(s.at_end());
    }

    /// The BW-01-02-02 acceptance: a packed int crosses the 1-byte form into
    /// the 4-byte form at 255, and the boundary values pack to the reference's
    /// exact bytes (`binary_stream.hpp` `writePackedInt`/`readPackedInt`):
    /// 0..=254 are a single byte; 255..=2^24-1 are `0xFF` followed by the
    /// three low bytes little-endian. A string's length prefix crosses the
    /// same boundary.
    #[test]
    fn reproduces_the_reference_packed_int_bytes() {
        let cases: [(u32, &[u8], usize); 8] = [
            (0, &[0x00], 1),
            (254, &[0xFE], 1),
            // The crossover: 255 packs as `0xFF` then its three low bytes
            // (0xFF, 0x00, 0x00), so the reader recovers 255.
            (255, &[0xFF, 0xFF, 0x00, 0x00], 4),
            (256, &[0xFF, 0x00, 0x01, 0x00], 4),
            (0x010203, &[0xFF, 0x03, 0x02, 0x01], 4),
            (0xFFFF, &[0xFF, 0xFF, 0xFF, 0x00], 4),
            (0x0001_0000, &[0xFF, 0x00, 0x00, 0x01], 4),
            (MAX_PACKED_INT, &[0xFF, 0xFF, 0xFF, 0xFF], 4),
        ];
        for (value, expected, size) in cases {
            let mut s = BinaryStream::new();
            s.write_packed_int(value).unwrap();
            assert_eq!(s.as_bytes(), expected, "value {value}");
            assert_eq!(packed_int_size(value), size, "value {value}");
            assert_eq!(s.length(), size, "value {value}");
            s.rewind();
            assert_eq!(s.read_packed_int(), Ok(value), "value {value}");
            assert!(s.at_end(), "value {value}");
        }

        // A string's packed length prefix crosses the same boundary: a 254-byte
        // string carries a 1-byte prefix, a 255-byte string a 4-byte prefix.
        let long254 = "x".repeat(254);
        let mut s = BinaryStream::new();
        s.write_string(&long254).unwrap();
        assert_eq!(s.as_bytes()[..5], [0xFE, 0x78, 0x78, 0x78, 0x78]);
        assert_eq!(s.length(), 1 + 254);
        s.rewind();
        assert_eq!(s.read_string().unwrap(), long254);

        let long255 = "y".repeat(255);
        let mut t = BinaryStream::new();
        t.write_string(&long255).unwrap();
        assert_eq!(t.as_bytes()[..5], [0xFF, 0xFF, 0x00, 0x00, 0x79]);
        assert_eq!(t.length(), 4 + 255);
        t.rewind();
        assert_eq!(t.read_string().unwrap(), long255);
    }

    /// The BW-01-02-03 acceptance: a string is length-prefixed, not
    /// NUL-terminated — the reference's own comment in `appendString`
    /// (`binary_stream.hpp`) says so because "BW::strings can contain null
    /// characters" — so an embedded NUL survives the round trip, and the read
    /// cursor's remaining length is exact.
    #[test]
    fn strings_carry_embedded_nul_and_remaining_length() {
        // Empty value: a zero length prefix, read back empty.
        let mut s = BinaryStream::new();
        s.write_string("").unwrap();
        assert_eq!(s.as_bytes(), &[0x00]);
        assert_eq!(s.length(), 1);
        s.rewind();
        assert_eq!(s.remaining(), 1);
        assert_eq!(s.read_string(), Ok(String::new()));
        assert!(s.at_end());

        // Embedded NUL survives: the length prefix counts bytes, not up to
        // the first NUL.
        let mut s = BinaryStream::new();
        s.write_string("a\u{0}b").unwrap();
        assert_eq!(s.as_bytes(), &[0x03, 0x61, 0x00, 0x62]);
        s.rewind();
        assert_eq!(s.read_string(), Ok("a\u{0}b".to_string()));
        assert!(s.at_end());

        // A string followed by a trailing value: after reading the string,
        // the remaining length is exact.
        let mut s = BinaryStream::new();
        s.write_string("hello").unwrap();
        s.write_u8(0xEE);
        s.rewind();
        assert_eq!(s.read_string(), Ok("hello".to_string()));
        assert_eq!(s.remaining(), 1);
        assert_eq!(s.read_u8(), Ok(0xEE));
        assert!(s.at_end());
    }

    /// Read a `u8`, failing the closure if the stream is over-run.
    fn over_read_u8(s: &mut BinaryStream) -> Result<(), StreamError> {
        s.read_u8().map(drop)
    }

    /// Read a `u16`, failing the closure if the stream is over-run.
    fn over_read_u16(s: &mut BinaryStream) -> Result<(), StreamError> {
        s.read_u16().map(drop)
    }

    /// Read an `f64`, failing the closure if the stream is over-run.
    fn over_read_f64(s: &mut BinaryStream) -> Result<(), StreamError> {
        s.read_f64().map(drop)
    }

    /// Read a packed int, failing the closure if the stream is over-run.
    fn over_read_packed_int(s: &mut BinaryStream) -> Result<(), StreamError> {
        s.read_packed_int().map(drop)
    }

    /// Read a string, failing the closure if the stream is over-run.
    fn over_read_string(s: &mut BinaryStream) -> Result<(), StreamError> {
        s.read_string().map(drop)
    }

    /// Read a prefixed blob, failing the closure if the stream is over-run.
    fn over_read_blob(s: &mut BinaryStream) -> Result<(), StreamError> {
        s.read_blob().map(drop)
    }

    /// Read 100 raw bytes, failing the closure if the stream is over-run.
    fn over_read_raw_100(s: &mut BinaryStream) -> Result<(), StreamError> {
        s.read_blob_raw(100).map(drop)
    }

    /// The BW-01-02-05 acceptance: abnormal inputs produce the same class of
    /// stream error as the reference. `MemoryIStream::retrieve`
    /// (`lib/cstdmf/memory_stream.ipp`) answers an over-read by clamping the
    /// read cursor to `pEnd_`, setting `error_`, and handing out a zero buffer
    /// (or NULL). Every read funnel — fixed width, packed int, string, blob —
    /// goes through it, so every kind of over-run lands in that one report.
    ///
    /// The port reports the same class: `Err(StreamError::Truncated)`, the
    /// error flag set, the cursor clamped to the end, and every following
    /// read failing identically. Unlike the reference it hands out no bytes
    /// at all, so a failed read can never masquerade as a silently-zero value.
    /// One abnormal input: a name, the fixture bytes, and the read that
    /// over-runs them.
    type OverReadCase = (
        &'static str,
        &'static [u8],
        fn(&mut BinaryStream) -> Result<(), StreamError>,
    );

    #[test]
    fn abnormal_reads_all_report_the_same_stream_error_class() {
        let cases: [OverReadCase; 7] = [
            // The cursor sits on the end already: any positive read over-runs
            // (`pCurr_ + nBytes <= pEnd_` fails in the reference).
            ("u8 read on an exhausted stream", b"", over_read_u8),
            // Partial bytes of a wider fixed-width value.
            ("u16 read with one byte left", b"\xAB", over_read_u16),
            (
                "f64 read with five bytes left",
                &[0x01, 0x02, 0x03, 0x04, 0x05],
                over_read_f64,
            ),
            // The packed int's four-byte form is declared but under-filled.
            (
                "packed int with a two-byte tail",
                &[0xFF, 0x01, 0x02],
                over_read_packed_int,
            ),
            // The length prefix declares more bytes than remain.
            (
                "string prefix beyond the remaining bytes",
                &[0x05, b'a', b'b'],
                over_read_string,
            ),
            (
                "blob prefix beyond the remaining bytes",
                &[0xFF, 0x00, 0x01, 0x00, 0x00, 0xAB],
                over_read_blob,
            ),
            (
                "raw blob request beyond the remaining bytes",
                &[0x01, 0x02, 0x03, 0x04],
                over_read_raw_100,
            ),
        ];

        for (name, bytes, read) in cases {
            let mut s = BinaryStream::new();
            s.write_bytes(bytes);
            s.rewind();

            assert_eq!(read(&mut s), Err(StreamError::Truncated), "{name}");
            assert!(s.has_error(), "{name}: the reference's error_ is set");
            assert!(
                s.at_end(),
                "{name}: the reference clamps the cursor to pEnd_"
            );
            assert_eq!(s.remaining(), 0, "{name}: no bytes left to read");

            // Following reads keep failing: the report sticks and the cursor
            // stays on the end.
            assert_eq!(s.read_u8(), Err(StreamError::Truncated), "{name}");
            assert_eq!(s.read_string(), Err(StreamError::Truncated), "{name}");
            assert!(s.has_error(), "{name}");
            assert!(s.at_end(), "{name}");
        }
    }

    /// The class boundary: a zero-length read is not an over-read. The
    /// reference's `retrieve(0)` returns the cursor pointer without touching
    /// `error_` (`pCurr_ + 0 <= pEnd_` always holds), so an empty string reads
    /// fine. The reference reports a failed *bounded* read with a zero buffer
    /// of at most `BS_BUFF_MAX_SIZE` (`binary_stream.hpp`) bytes or NULL; the
    /// port pins the same ceiling and hands out none of them.
    #[test]
    fn zero_length_reads_are_not_the_error_class() {
        assert_eq!(STREAM_ERROR_BUFFER_MAX, 2048); // `BS_BUFF_MAX_SIZE`

        let mut s = BinaryStream::new();
        s.write_u8(0x00); // packed length 0
        s.rewind();
        assert_eq!(s.read_string(), Ok(String::new()));
        assert!(!s.has_error());
        assert!(s.at_end());

        let mut s = BinaryStream::new();
        assert_eq!(s.read_blob_raw(0), Ok(Vec::new()));
        assert!(!s.has_error());
    }

    /// The report is a sticky flag, not a latch: `MemoryIStream::rewind`
    /// moves the read point back but leaves `error_` set, so a rewound stream
    /// reads successfully again while the report of the earlier over-read
    /// remains — exactly the reference's class of report.
    #[test]
    fn a_reported_over_read_still_reports_after_a_rewind() {
        let mut s = BinaryStream::new();
        s.write_u32(0x01020304);
        s.rewind();
        assert_eq!(s.read_u64(), Err(StreamError::Truncated)); // 4 of 8 bytes
        assert!(s.has_error());

        s.rewind();
        assert!(s.has_error(), "the report sticks across a rewind");
        assert_eq!(s.remaining(), 4);

        // The reference's error_ does not stop reads once the cursor is back
        // in range — only the over-run report is recorded.
        assert_eq!(s.read_u32(), Ok(0x01020304));
        assert!(s.has_error());
    }
}

/// Byte-exact fixtures captured from the reference cstdmf streams
/// (`lib/cstdmf/binary_stream.hpp`, `memory_stream.hpp`, `blob_or_null.hpp`
/// compiled against the original tree). Every record below is the exact byte
/// sequence the original wrote, so a divergence in layout fails here first.
#[cfg(test)]
mod original_fixtures {
    use super::*;

    fn decode_hex(hex: &str) -> Vec<u8> {
        assert_eq!(hex.len() % 2, 0, "odd hex length: {hex}");
        hex.as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    // --- BW-01-02-01: fixed-width integers and floats -----------------------

    /// One record per original value, mirroring the harness that produced it:
    /// each value went into its own stream, so each is checked in its own.
    #[test]
    fn fixed_width_matches_original_bytes() {
        let mut s = BinaryStream::new();
        s.write_u8(0x7f);
        assert_eq!(s.as_bytes(), decode_hex("7f").as_slice());
        s.rewind();
        assert_eq!(s.read_u8(), Ok(0x7f));

        let mut s = BinaryStream::new();
        s.write_i8(-128);
        assert_eq!(s.as_bytes(), decode_hex("80").as_slice());
        s.rewind();
        assert_eq!(s.read_i8(), Ok(-128));

        let mut s = BinaryStream::new();
        s.write_u16(0x1234);
        assert_eq!(s.as_bytes(), decode_hex("3412").as_slice());
        s.rewind();
        assert_eq!(s.read_u16(), Ok(0x1234));

        let mut s = BinaryStream::new();
        s.write_i16(-12345);
        assert_eq!(s.as_bytes(), decode_hex("c7cf").as_slice());
        s.rewind();
        assert_eq!(s.read_i16(), Ok(-12345));

        let mut s = BinaryStream::new();
        s.write_u32(0xdead_beef);
        assert_eq!(s.as_bytes(), decode_hex("efbeadde").as_slice());
        s.rewind();
        assert_eq!(s.read_u32(), Ok(0xdead_beef));

        let mut s = BinaryStream::new();
        s.write_i32(-123_456_789);
        assert_eq!(s.as_bytes(), decode_hex("eb32a4f8").as_slice());
        s.rewind();
        assert_eq!(s.read_i32(), Ok(-123_456_789));

        let mut s = BinaryStream::new();
        s.write_u64(0x0123_4567_89ab_cdef);
        assert_eq!(s.as_bytes(), decode_hex("efcdab8967452301").as_slice());
        s.rewind();
        assert_eq!(s.read_u64(), Ok(0x0123_4567_89ab_cdef));

        let mut s = BinaryStream::new();
        s.write_i64(-1);
        assert_eq!(s.as_bytes(), decode_hex("ffffffffffffffff").as_slice());
        s.rewind();
        assert_eq!(s.read_i64(), Ok(-1));

        let mut s = BinaryStream::new();
        s.write_f32(3.5);
        assert_eq!(s.as_bytes(), decode_hex("00006040").as_slice());
        s.rewind();
        assert_eq!(s.read_f32(), Ok(3.5));

        let mut s = BinaryStream::new();
        // The original fixture wrote 2.718281828459045, which is exactly
        // `f64::consts::E` at binary64 precision (bytes checked below).
        s.write_f64(std::f64::consts::E);
        assert_eq!(s.as_bytes(), decode_hex("6957148b0abf0540").as_slice());
        s.rewind();
        assert_eq!(s.read_f64(), Ok(std::f64::consts::E));
    }

    #[test]
    fn bool_is_one_byte_per_value() {
        let mut s = BinaryStream::new();
        s.write_bool(true);
        s.write_bool(false);
        assert_eq!(s.as_bytes(), decode_hex("0100").as_slice());

        s.rewind();
        assert_eq!(s.read_bool(), Ok(true));
        assert_eq!(s.read_bool(), Ok(false));
    }

    // --- BW-01-02-02: variable-length integers and length prefixes -----------

    /// Every boundary value the original packed, with the exact bytes it
    /// produced. `>= 255` becomes `0xFF` plus the three low bytes little-endian
    /// (`BW_PACK3`); everything else is a single byte.
    #[test]
    fn packed_int_matches_original_bytes() {
        let cases: &[(u32, &str)] = &[
            (0, "00"),
            (1, "01"),
            (127, "7f"),
            (128, "80"),
            (253, "fd"),
            (254, "fe"),
            (255, "ffff0000"),
            (256, "ff000100"),
            (1023, "ffff0300"),
            (65534, "fffeff00"),
            (65535, "ffffff00"),
            (65536, "ff000001"),
            (1_000_000, "ff40420f"),
            (16_777_214, "fffeffff"),
            (16_777_215, "ffffffff"),
        ];
        for &(value, hex) in cases {
            let mut s = BinaryStream::new();
            s.write_packed_int(value).unwrap();
            assert_eq!(s.as_bytes(), decode_hex(hex).as_slice(), "value {value}");
            assert_eq!(packed_int_size(value), if value < 0xff { 1 } else { 4 });
            s.rewind();
            assert_eq!(s.read_packed_int(), Ok(value), "value {value}");
        }
    }

    /// `write_count` is a plain u32 element count, exactly as the original
    /// writes a container's element count (`operator<<` for `BW::vector`).
    #[test]
    fn count_is_plain_u32_like_original_vector_writes() {
        let mut s = BinaryStream::new();
        s.write_count(0x0102_0304);
        assert_eq!(s.as_bytes(), decode_hex("04030201").as_slice());
        s.rewind();
        assert_eq!(s.read_count(), Ok(0x0102_0304));
    }

    // --- BW-01-02-03: strings and the memory-stream surface ------------------

    #[test]
    fn string_fixtures_match_original_bytes() {
        let mut s = BinaryStream::new();
        s.write_string("").unwrap();
        assert_eq!(s.as_bytes(), decode_hex("00").as_slice());

        let mut s = BinaryStream::new();
        s.write_string("abc").unwrap();
        assert_eq!(s.as_bytes(), decode_hex("03616263").as_slice());
        s.rewind();
        assert_eq!(s.read_string(), Ok("abc".to_string()));

        // Embedded NUL survives: the format is length-prefixed, never
        // terminated, so the zero inside is payload like any other byte.
        let mut s = BinaryStream::new();
        s.write_string("a\0b").unwrap();
        assert_eq!(s.as_bytes(), decode_hex("03610062").as_slice());
        s.rewind();
        assert_eq!(s.read_string(), Ok("a\0b".to_string()));

        // A 255-byte string crosses the packed-int boundary into 4-byte form.
        let long = "x".repeat(255);
        let mut s = BinaryStream::new();
        s.write_string(&long).unwrap();
        assert_eq!(&s.as_bytes()[..4], decode_hex("ffff0000").as_slice());
        assert_eq!(s.length(), 259);
        s.rewind();
        assert_eq!(s.read_string(), Ok(long));
    }

    #[test]
    fn add_blob_writes_raw_bytes_without_prefix() {
        let mut s = BinaryStream::new();
        s.write_blob_raw(b"abc");
        assert_eq!(s.as_bytes(), decode_hex("616263").as_slice());
    }

    /// `BlobOrNull` framing from `lib/cstdmf/blob_or_null.cpp`: present data
    /// is a length-prefixed blob; null and empty each write a zero length
    /// plus a one-byte flag telling them apart.
    #[test]
    fn blob_or_null_fixtures_match_original_framing() {
        // null: packed 0 + flag 0
        let mut s = BinaryStream::new();
        s.write_blob_or_null(None).unwrap();
        assert_eq!(s.as_bytes(), decode_hex("0000").as_slice());
        s.rewind();
        assert_eq!(s.read_blob_or_null(), Ok(None));

        // empty (non-null): packed 0 + flag 1
        let mut s = BinaryStream::new();
        s.write_blob_or_null(Some(&[])).unwrap();
        assert_eq!(s.as_bytes(), decode_hex("0001").as_slice());
        s.rewind();
        assert_eq!(s.read_blob_or_null(), Ok(Some(Vec::new())));

        // data: packed length + bytes, no trailing flag
        let mut s = BinaryStream::new();
        s.write_blob_or_null(Some(b"data")).unwrap();
        assert_eq!(s.as_bytes(), decode_hex("0464617461").as_slice());
        s.rewind();
        assert_eq!(s.read_blob_or_null(), Ok(Some(b"data".to_vec())));
    }

    #[test]
    fn remaining_length_after_partial_read_matches_original() {
        // The original wrote three u32s and read one back: remaining 8.
        let mut s = BinaryStream::new();
        s.write_u32(1).write_u32(2).write_u32(3);
        s.rewind();
        assert_eq!(s.read_u32(), Ok(1));
        assert_eq!(s.remaining(), 8);
        assert!(!s.has_error());
    }

    #[test]
    fn rewind_readback_returns_written_wire_bytes() {
        let mut s = BinaryStream::new();
        s.write_u16(0x0102).write_u16(0x0304);
        s.rewind();
        assert_eq!(s.as_bytes(), decode_hex("02010403").as_slice());
        assert_eq!(s.unread_bytes(), decode_hex("02010403").as_slice());
    }

    // --- BW-01-02-05: truncation and out-of-bounds reporting ----------------

    /// Every one of the original's overread records reports `error=1` with
    /// `remaining=0`, which is this stream's `has_error()` plus a clamped read
    /// cursor. The reference hands back an internal `errBuf` for reads of at
    /// most 2048 bytes so a failed fixed-width read yields a zeroed
    /// destination instead of aborting (the harness prints `value=0x00000000`
    /// for a read into `0xAAAAAAAA`); the Rust analogue is an error value, so
    /// those out-param zeroes map onto `Err` here.
    #[test]
    fn short_fixed_width_read_reports_error() {
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex("3412"));
        s.rewind();
        assert_eq!(s.read_u32(), Err(StreamError::Truncated));
        assert!(s.has_error());
        assert_eq!(s.remaining(), 0);
    }

    #[test]
    fn read_from_an_empty_stream_reports_error() {
        let mut s = BinaryStream::new();
        assert_eq!(s.read_u8(), Err(StreamError::Truncated));
        assert!(s.has_error());
        assert_eq!(s.remaining(), 0);
    }

    #[test]
    fn string_overrun_reports_error() {
        // Declares 5 bytes, has 2.
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex("056162"));
        s.rewind();
        assert_eq!(s.read_string(), Err(StreamError::Truncated));
        assert!(s.has_error());
        assert_eq!(s.remaining(), 0);
    }

    #[test]
    fn packed_int_payload_truncation_reports_error() {
        // Starts the 4-byte extension but has only one byte behind it.
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex("ff00"));
        s.rewind();
        assert_eq!(s.read_packed_int(), Err(StreamError::Truncated));
        assert!(s.has_error());
        assert_eq!(s.remaining(), 0);
    }

    #[test]
    fn error_is_sticky_across_reads() {
        // A one-byte stream: the u32 read fails, and the original clamps the
        // cursor to the end, so the surviving byte is unreachable and the later
        // u8 read reports the same error too.
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex("01"));
        s.rewind();
        assert_eq!(s.read_u32(), Err(StreamError::Truncated));
        assert_eq!(s.read_u8(), Err(StreamError::Truncated));
        assert!(s.has_error());
        assert_eq!(s.remaining(), 0);
    }

    #[test]
    fn retrieve_beyond_end_reports_error() {
        let mut s = BinaryStream::new();
        s.write_bytes(&decode_hex("0102"));
        s.rewind();
        assert_eq!(s.read_blob_raw(4), Err(StreamError::Truncated));
        assert!(s.has_error());
        assert_eq!(s.remaining(), 0);
    }
}
