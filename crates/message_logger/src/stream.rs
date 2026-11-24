//! The minimal `BinaryOStream`/`BinaryIStream` face the log protocol needs
//! (`lib/cstdmf/binary_stream.hpp`): little-endian scalars, strings length
//! prefixed with the packed int, and `addBlob` as a raw memcpy with no
//! prefix. The same semantics the watcher crate ports; carried here because
//! the transport-facing stream crates are out of this lane's reach.

/// The append-only output stream (`MemoryOStream`).
#[derive(Default)]
pub struct MemoryOStream {
    data: Vec<u8>,
}

impl MemoryOStream {
    /// `MemoryOStream()`.
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }

    /// `reserve(n)`: room for `n` bytes, returned as a mutable slice.
    pub fn reserve(&mut self, n: usize) -> &mut [u8] {
        let start = self.data.len();
        self.data.resize(start + n, 0);
        &mut self.data[start..]
    }

    /// `addBlob(data, len)`: a raw memcpy, no length prefix
    /// (`binary_stream.hpp:105-130`).
    pub fn add_blob(&mut self, blob: &[u8]) -> &mut Self {
        self.data.extend_from_slice(blob);
        self
    }

    /// `writeStringLength`/`writePackedInt`: one byte below 255, otherwise
    /// the 0xFF marker plus three little-endian bytes.
    pub fn write_string_length(&mut self, len: i32) -> &mut Self {
        if len < 255 {
            self.write_u8(len as u8);
        } else {
            self.write_u8(0xff);
            let bytes = len.to_le_bytes();
            self.add_blob(&bytes[..3]);
        }
        self
    }

    /// `operator<<( BW::string )`: packed length then raw bytes.
    pub fn write_string(&mut self, text: &str) -> &mut Self {
        self.write_string_length(text.len() as i32);
        self.add_blob(text.as_bytes())
    }

    /// `operator<<( T )` for the scalar widths the protocol uses; every
    /// scalar is a little-endian memcpy (`binary_stream.ipp:250-330`).
    pub fn write_u8(&mut self, value: u8) -> &mut Self {
        self.reserve(1)[0] = value;
        self
    }

    /// A two-byte little-endian scalar (`short`/`uint16`).
    pub fn write_u16(&mut self, value: u16) -> &mut Self {
        let bytes = value.to_le_bytes();
        self.reserve(2).copy_from_slice(&bytes);
        self
    }

    /// A four-byte little-endian scalar (`int32`/`uint32`/`float`).
    pub fn write_u32(&mut self, value: u32) -> &mut Self {
        let bytes = value.to_le_bytes();
        self.reserve(4).copy_from_slice(&bytes);
        self
    }

    /// An eight-byte little-endian scalar (`int64`/`uint64`/`double`).
    pub fn write_u64(&mut self, value: u64) -> &mut Self {
        let bytes = value.to_le_bytes();
        self.reserve(8).copy_from_slice(&bytes);
        self
    }

    /// The current size.
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// The contents.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// The read side (`MemoryIStream`): reads past the end flag `error` and
/// leave the stream exhausted, like the reference.
pub struct MemoryIStream<'a> {
    data: &'a [u8],
    offset: usize,
    error: bool,
}

impl<'a> MemoryIStream<'a> {
    /// `MemoryIStream( data, len )`.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            offset: 0,
            error: false,
        }
    }

    /// `retrieve(n)`: the next `n` bytes without advancing (`NULL` past the
    /// end, which flags the stream).
    pub fn retrieve(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.remaining() < n {
            self.error = true;
            return None;
        }
        let out = &self.data[self.offset..self.offset + n];
        self.offset += n;
        Some(out)
    }

    /// `readPackedInt`/`readStringLength`.
    pub fn read_string_length(&mut self) -> i32 {
        match self.read_u8() {
            Some(0xff) => {
                let rest = self.retrieve(3).unwrap_or(&[0, 0, 0]);
                let mut bytes = [0u8; 4];
                bytes[..3].copy_from_slice(rest);
                i32::from_le_bytes(bytes)
            }
            Some(first) => first as i32,
            None => -1,
        }
    }

    /// `operator>>( BW::string )`.
    pub fn read_string(&mut self) -> String {
        let len = self.read_string_length();
        if len < 0 {
            return String::new();
        }
        let bytes = self.retrieve(len as usize).unwrap_or(&[]);
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// A one-byte little-endian scalar.
    pub fn read_u8(&mut self) -> Option<u8> {
        self.retrieve(1).map(|b| b[0])
    }

    /// A two-byte little-endian scalar.
    pub fn read_u16(&mut self) -> Option<u16> {
        self.retrieve(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    /// A four-byte little-endian scalar.
    pub fn read_u32(&mut self) -> Option<u32> {
        self.retrieve(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// An eight-byte little-endian scalar.
    pub fn read_u64(&mut self) -> Option<u64> {
        self.retrieve(8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    /// `tell()` — where the next read starts.
    pub fn position(&self) -> usize {
        self.offset
    }

    /// `remainingLength()`.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.offset
    }

    /// `error()`: set by any read past the end.
    pub fn error(&self) -> bool {
        self.error
    }

    /// `finish()`: drain any tail so destructors do not warn.
    pub fn finish(&mut self) {
        self.offset = self.data.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_stream_starts_empty() {
        let stream = MemoryOStream::default();
        assert_eq!(stream.size(), 0);
        assert_eq!(stream.data(), &[] as &[u8]);
    }

    #[test]
    fn strings_use_the_packed_length_prefix() {
        let mut stream = MemoryOStream::new();
        stream.write_string("");
        stream.write_string("hello");
        let mut big = String::new();
        big.push_str(&"x".repeat(300));
        stream.write_string(&big);

        let mut input = MemoryIStream::new(stream.data());
        assert_eq!(input.read_string(), "");
        assert_eq!(input.read_string(), "hello");
        assert_eq!(input.read_string(), big);
        assert_eq!(input.remaining(), 0);
        assert!(!input.error());
    }

    #[test]
    fn scalars_round_trip_little_endian() {
        let mut stream = MemoryOStream::new();
        stream
            .write_u8(0xab)
            .write_u16(0x1234)
            .write_u32(0x0102_0304);
        stream.write_u64(0x0102_0304_0506_0708);
        assert_eq!(
            stream.data(),
            &[
                0xab, 0x34, 0x12, 0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02,
                0x01
            ]
        );

        let mut input = MemoryIStream::new(stream.data());
        assert_eq!(input.read_u8(), Some(0xab));
        assert_eq!(input.read_u16(), Some(0x1234));
        assert_eq!(input.read_u32(), Some(0x0102_0304));
        assert_eq!(input.read_u64(), Some(0x0102_0304_0506_0708));
    }

    #[test]
    fn reads_past_the_end_flag_the_stream() {
        let mut stream = MemoryOStream::new();
        stream.write_u32(7);
        let mut input = MemoryIStream::new(stream.data());
        assert_eq!(input.read_u64(), None);
        assert!(input.error());
        // The failed read did not advance, so the four bytes are still
        // there and the sticky error stays set.
        assert_eq!(input.read_u32(), Some(7));
        assert!(input.error());
        input.finish();
        assert_eq!(input.remaining(), 0);
    }

    #[test]
    fn blobs_copy_without_a_prefix() {
        let mut stream = MemoryOStream::new();
        stream.write_string_length(3);
        stream.add_blob(&[1, 2, 3]);
        assert_eq!(stream.data(), &[3, 1, 2, 3]);

        let mut input = MemoryIStream::new(stream.data());
        assert_eq!(input.read_string_length(), 3);
        assert_eq!(input.retrieve(3), Some(&[1u8, 2, 3][..]));
    }
}
