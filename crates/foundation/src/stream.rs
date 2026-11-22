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
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "stream read would run past the end of the buffer"),
            Self::PackedIntTooLarge => write!(f, "packed int length exceeds 2^24 - 1"),
        }
    }
}

impl std::error::Error for StreamError {}

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
}
