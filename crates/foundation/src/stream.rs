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

    // BW-01-02-01: the fixed-width wire bytes the reference produces. The
    // reference `lib/cstdmf/binary_stream.ipp` writes each primitive through
    // `bw_netbyte/netshort/netlong/netlonglong` with `BW_HTON*`, which is
    // identity on little-endian hosts, and float/double through a union with a
    // `u32`/`u64` member (`n.f = x; out << n`) — i.e. the raw IEEE bit pattern
    // in little-endian byte order. These fixtures pin those bytes for boundary
    // and sentinel values, and the round-trip back reads the exact value.
    //
    // Every byte below was confirmed against a real reference build: a
    // generator compiled against the reference's `lib/cstdmf/binary_stream.hpp`
    // + `memory_stream.hpp` (linked to `build/el7/lib/libcstdmf.a`) emitted
    // each of these value/byte pairs, and the port reproduces them exactly.
    #[test]
    fn reference_wire_fixtures_fixed_width() {
        // u8 / i8 / bool — one byte each.
        let mut s = BinaryStream::new();
        s.write_u8(0xAB);
        assert_eq!(s.as_bytes(), &[0xAB]);
        let mut s = BinaryStream::new();
        s.write_i8(-7);
        assert_eq!(s.as_bytes(), &[0xF9]);
        let mut s = BinaryStream::new();
        s.write_bool(true);
        assert_eq!(s.as_bytes(), &[0x01]);
        let mut s = BinaryStream::new();
        s.write_bool(false);
        assert_eq!(s.as_bytes(), &[0x00]);

        // u16 / i16 — two bytes little-endian.
        let mut s = BinaryStream::new();
        s.write_u16(0x1234);
        assert_eq!(s.as_bytes(), &[0x34, 0x12]);
        let mut s = BinaryStream::new();
        s.write_i16(-1234);
        assert_eq!(s.as_bytes(), &[0x2E, 0xFB]); // 0xFB2E

        // u32 / i32 — four bytes little-endian, boundary sentinels.
        let mut s = BinaryStream::new();
        s.write_u32(0x0102_0304);
        assert_eq!(s.as_bytes(), &[0x04, 0x03, 0x02, 0x01]);
        let mut s = BinaryStream::new();
        s.write_i32(-1);
        assert_eq!(s.as_bytes(), &[0xFF, 0xFF, 0xFF, 0xFF]);
        let mut s = BinaryStream::new();
        s.write_u32(0);
        assert_eq!(s.as_bytes(), &[0, 0, 0, 0]);

        // u64 / i64 — eight bytes little-endian.
        let mut s = BinaryStream::new();
        s.write_u64(0x0102_0304_0506_0708);
        assert_eq!(
            s.as_bytes(),
            &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
        let mut s = BinaryStream::new();
        s.write_i64(-1);
        assert_eq!(s.as_bytes(), &[0xFF; 8]);

        // f32 — raw IEEE bit pattern little-endian (union n.f, BW_HTONL identity).
        let mut s = BinaryStream::new();
        s.write_f32(-1.5);
        assert_eq!(s.as_bytes(), &[0x00, 0x00, 0xC0, 0xBF]); // 0xBFC00000
        let mut s = BinaryStream::new();
        s.write_f32(0.0);
        assert_eq!(s.as_bytes(), &[0x00, 0x00, 0x00, 0x00]);
        let mut s = BinaryStream::new();
        s.write_f32(-0.0);
        assert_eq!(s.as_bytes(), &[0x00, 0x00, 0x00, 0x80]); // sign bit only

        // f64 — raw IEEE bit pattern little-endian (union n.d, BW_HTONLL identity).
        let mut s = BinaryStream::new();
        s.write_f64(-2.75);
        assert_eq!(
            s.as_bytes(),
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0xC0]
        );
    }

    #[test]
    fn reference_wire_fixtures_floats_special() {
        // NaN, ±infinity and denormals keep their exact bit pattern — the
        // reference stores the raw union bits, so no canonicalization happens.
        let mut s = BinaryStream::new();
        s.write_f32(f32::NAN);
        assert_eq!(s.as_bytes(), &[0x00, 0x00, 0xC0, 0x7F]); // 0x7FC00000 quiet NaN
        let mut s = BinaryStream::new();
        s.write_f32(f32::INFINITY);
        assert_eq!(s.as_bytes(), &[0x00, 0x00, 0x80, 0x7F]); // 0x7F800000
        let mut s = BinaryStream::new();
        s.write_f32(f32::NEG_INFINITY);
        assert_eq!(s.as_bytes(), &[0x00, 0x00, 0x80, 0xFF]); // 0xFF800000
        let mut s = BinaryStream::new();
        s.write_f32(f32::from_bits(0x0000_0001)); // denormal
        assert_eq!(s.as_bytes(), &[0x01, 0x00, 0x00, 0x00]);
        let mut s = BinaryStream::new();
        s.write_f64(f64::NAN);
        assert_eq!(
            s.as_bytes(),
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF8, 0x7F]
        );
    }

    #[test]
    fn reference_wire_fixtures_roundtrip_each_width() {
        // Byte-exact round-trip: write the wire bytes, read them back, and
        // recover the exact value for every fixed-width primitive.
        //
        // The expected byte sequence is the reference's own output: it was
        // produced by compiling a fixture generator against the reference's
        // cstdmf (`lib/cstdmf/binary_stream.ipp` + `memory_stream.hpp`, linked
        // against `build/el7/lib/libcstdmf.a`) and writing the same value set
        // through a `BW::MemoryOStream`. The port reproduces those 43 bytes
        // exactly, then reads each value back.
        let mut s = BinaryStream::new();
        s.write_u8(0xFE)
            .write_i8(-3)
            .write_bool(true)
            .write_u16(0xBEEF)
            .write_i16(-32_000)
            .write_u32(0xDEAD_BEEF)
            .write_i32(-2_000_000_000)
            .write_u64(0x0123_4567_89AB_CDEF)
            .write_i64(-9_000_000_000)
            .write_f32(3.25)
            .write_f64(-123.456);
        assert_eq!(
            s.as_bytes(),
            &[
                0xFE, 0xFD, 0x01, // u8, i8, bool
                0xEF, 0xBE, // u16 0xBEEF
                0x00, 0x83, // i16 -32000 (0x8300)
                0xEF, 0xBE, 0xAD, 0xDE, // u32 0xDEADBEEF
                0x00, 0x6C, 0xCA, 0x88, // i32 -2000000000
                0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01, // u64
                0x00, 0xE6, 0x8E, 0xE7, 0xFD, 0xFF, 0xFF, 0xFF, // i64 -9000000000
                0x00, 0x00, 0x50, 0x40, // f32 3.25
                0x77, 0xBE, 0x9F, 0x1A, 0x2F, 0xDD, 0x5E, 0xC0, // f64 -123.456
            ]
        );
        s.rewind();
        assert_eq!(s.read_u8(), Ok(0xFE));
        assert_eq!(s.read_i8(), Ok(-3));
        assert_eq!(s.read_bool(), Ok(true));
        assert_eq!(s.read_u16(), Ok(0xBEEF));
        assert_eq!(s.read_i16(), Ok(-32_000));
        assert_eq!(s.read_u32(), Ok(0xDEAD_BEEF));
        assert_eq!(s.read_i32(), Ok(-2_000_000_000));
        assert_eq!(s.read_u64(), Ok(0x0123_4567_89AB_CDEF));
        assert_eq!(s.read_i64(), Ok(-9_000_000_000));
        assert_eq!(s.read_f32(), Ok(3.25));
        assert_eq!(s.read_f64(), Ok(-123.456));
        assert!(s.at_end());

        // The concatenated stream is exactly the sum of the primitive bytes.
        assert_eq!(
            s.as_bytes().len(),
            1 + 1 + 1 + 2 + 2 + 4 + 4 + 8 + 8 + 4 + 8
        );
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
}
