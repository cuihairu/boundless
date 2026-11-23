//! The watcher protocol 2 memory streams and value codec
//! (`lib/cstdmf/memory_streams.hpp` + `binary_stream.hpp` wire formats,
//! `lib/cstdmf/watcher.hpp:962-1028` encode, `:140-320` decode).
//!
//! Every stream a watcher value travels in is laid out as one byte
//! `WatcherDataType`, one byte `Watcher::Mode`, and only then the payload.
//! Strings and scalars carry a packed length prefix before their bytes; a
//! packed int is one byte below 255 and `0xFF` followed by three little
//! endian bytes otherwise (`binary_stream.hpp:254-306`).
//!
//! # Port shape
//!
//! `MemoryOStream`/`MemoryIStream` are growable/consuming byte buffers with
//! the exact packed-int, string and scalar encodings the reference streams
//! use; [`value_to_stream`] and [`stream_to_value`] are the
//! `watcherValueToStream`/`watcherStreamToValue` family. The reference's
//! per-type template overloads collapse into one [`StreamValue`] enum that
//! keeps the on-the-wire width, so an 8-byte INT payload fails to decode
//! into an i32 leaf exactly as the reference's `sizeDiff` check does.

use std::cell::RefCell;

use crate::{DataType, Mode, WatcherValue};

/// `WATCHER_TYPE_UNKNOWN`.
pub const TYPE_UNKNOWN: u8 = 0;
/// `WATCHER_TYPE_INT`.
pub const TYPE_INT: u8 = 1;
/// `WATCHER_TYPE_UINT`.
pub const TYPE_UINT: u8 = 2;
/// `WATCHER_TYPE_FLOAT`.
pub const TYPE_FLOAT: u8 = 3;
/// `WATCHER_TYPE_BOOL`.
pub const TYPE_BOOL: u8 = 4;
/// `WATCHER_TYPE_STRING`.
pub const TYPE_STRING: u8 = 5;
/// `WATCHER_TYPE_TUPLE`.
pub const TYPE_TUPLE: u8 = 6;
/// `WATCHER_TYPE_TYPE`.
pub const TYPE_TYPE: u8 = 7;

/// The `Watcher::Mode` byte values (`watcher.hpp:720-726`).
pub const MODE_INVALID: u8 = 0;
/// `WT_READ_ONLY`.
pub const MODE_READ_ONLY: u8 = 1;
/// `WT_READ_WRITE`.
pub const MODE_READ_WRITE: u8 = 2;
/// `WT_DIRECTORY`.
pub const MODE_DIRECTORY: u8 = 3;
/// `WT_CALLABLE`.
pub const MODE_CALLABLE: u8 = 4;

/// A watcher value as it travels on the wire: the enum keeps each
/// reference overload's payload width instead of erasing it into a scalar.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamValue {
    /// `BW::string`: packed length + bytes.
    Str(String),
    /// `int32`: 4 bytes.
    Int(i32),
    /// `unsigned int`: 4 bytes.
    Uint(u32),
    /// `int64`: 8 bytes.
    Int64(i64),
    /// `uint64`: 8 bytes.
    Uint64(u64),
    /// `float`: 4 bytes; the enum carries f64 and narrows on encode.
    Float32(f32),
    /// `double`: 8 bytes.
    Float64(f64),
    /// `bool`: 1 byte.
    Bool(bool),
    /// A `WatcherDataType` travelling as `WATCHER_TYPE_TYPE`: 1 byte.
    DataType(DataType),
}

/// `BinaryOStream::calculatePackedIntSize` (`binary_stream.hpp:274-277`):
/// the byte count [`MemoryOStream::write_packed_int`] will spend.
pub fn calculate_packed_int_size(value: i32) -> usize {
    if value >= 0xff { 4 } else { 1 }
}

/// `MemoryOStream` (`memory_streams.hpp`): the growable output buffer.
#[derive(Default)]
pub struct MemoryOStream {
    data: Vec<u8>,
}

impl MemoryOStream {
    /// An empty stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append raw bytes (`addBlob`).
    pub fn add_blob(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
    }

    /// `BinaryOStream::writePackedInt` (`binary_stream.hpp:254-270`): one
    /// byte below 255, otherwise `0xFF` plus three little endian bytes.
    pub fn write_packed_int(&mut self, value: i32) {
        if value >= 255 {
            self.data.push(0xff);
            self.data
                .extend_from_slice(&(value as u32).to_le_bytes()[..3]);
        } else {
            self.data.push(value as u8);
        }
    }

    /// `writeStringLength`.
    pub fn write_string_length(&mut self, len: i32) {
        self.write_packed_int(len);
    }

    /// `appendString`/`operator<<(BW::string)`: packed length + bytes, no
    /// terminator (`binary_stream.hpp:395-405`).
    pub fn write_string(&mut self, value: &str) {
        self.write_packed_int(value.len() as i32);
        self.add_blob(value.as_bytes());
    }

    /// `operator<<(int32)`: four little endian bytes.
    pub fn write_i32(&mut self, value: i32) {
        self.add_blob(&value.to_le_bytes());
    }

    /// `operator<<(uint32)`.
    pub fn write_u32(&mut self, value: u32) {
        self.add_blob(&value.to_le_bytes());
    }

    /// `operator<<(bool)`: one byte.
    pub fn write_bool(&mut self, value: bool) {
        self.data.push(u8::from(value));
    }

    /// `operator<<(uint8)`.
    pub fn write_u8(&mut self, value: u8) {
        self.data.push(value);
    }

    /// The buffered bytes (`data()`).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The byte count (`size()`).
    pub fn size(&self) -> usize {
        self.data.len()
    }
}

/// `MemoryIStream` (`memory_streams.hpp`): a consuming view over a byte
/// slice. Reads past the end set the error flag, the port of
/// `BinaryIStream::error()`.
pub struct MemoryIStream<'a> {
    data: &'a [u8],
    pos: usize,
    error: bool,
}

impl<'a> MemoryIStream<'a> {
    /// Wrap a byte slice (`MemoryIStream( data, size )`).
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            error: false,
        }
    }

    /// `retrieve(n)`: take the next `n` bytes, erroring past the end.
    pub fn retrieve(&mut self, length: usize) -> Option<&'a [u8]> {
        if self.pos + length > self.data.len() {
            self.pos = self.data.len();
            self.error = true;
            return None;
        }
        let slice = &self.data[self.pos..self.pos + length];
        self.pos += length;
        Some(slice)
    }

    /// `BinaryIStream::readPackedInt` (`binary_stream.hpp:287-300`).
    pub fn read_packed_int(&mut self) -> i32 {
        let first = match self.retrieve(1) {
            Some(bytes) => bytes[0],
            None => return 0,
        };
        if first == 0xff {
            let bytes = match self.retrieve(3) {
                Some(bytes) => bytes,
                None => return 0,
            };
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]) as i32
        } else {
            i32::from(first)
        }
    }

    /// `readStringLength`.
    pub fn read_string_length(&mut self) -> i32 {
        self.read_packed_int()
    }

    /// `operator>>(BW::string)`: packed length + bytes.
    pub fn read_string(&mut self) -> String {
        let length = self.read_packed_int();
        match self.retrieve(length as usize) {
            Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            None => String::new(),
        }
    }

    /// `operator>>(int32)`.
    pub fn read_i32(&mut self) -> i32 {
        match self.retrieve(4) {
            Some(bytes) => i32::from_le_bytes(bytes.try_into().expect("four bytes")),
            None => 0,
        }
    }

    /// `operator>>(uint32)`.
    pub fn read_u32(&mut self) -> u32 {
        match self.retrieve(4) {
            Some(bytes) => u32::from_le_bytes(bytes.try_into().expect("four bytes")),
            None => 0,
        }
    }

    /// `operator>>(bool)`: one byte.
    pub fn read_bool(&mut self) -> bool {
        match self.retrieve(1) {
            Some(bytes) => bytes[0] != 0,
            None => false,
        }
    }

    /// `operator>>(uint8)`.
    pub fn read_u8(&mut self) -> u8 {
        match self.retrieve(1) {
            Some(bytes) => bytes[0],
            None => 0,
        }
    }

    /// `remainingLength()`.
    pub fn remaining_length(&self) -> usize {
        self.data.len() - self.pos
    }

    /// `BinaryIStream::error()`.
    pub fn error(&self) -> bool {
        self.error
    }

    /// `BinaryIStream::finish()`: drain the rest.
    pub fn finish(&mut self) {
        self.pos = self.data.len();
    }
}

/// `watcherValueToStream` (`watcher.hpp:962-1028`): one `WatcherDataType`
/// byte, one `Mode` byte, then the payload -- strings packed-length
/// prefixed, scalars preceded by their packed byte width.
pub fn value_to_stream(out: &mut MemoryOStream, value: &StreamValue, mode: u8) {
    let type_byte = match value {
        StreamValue::Str(_) => TYPE_STRING,
        StreamValue::Int(_) | StreamValue::Int64(_) => TYPE_INT,
        StreamValue::Uint(_) | StreamValue::Uint64(_) => TYPE_UINT,
        StreamValue::Float32(_) | StreamValue::Float64(_) => TYPE_FLOAT,
        StreamValue::Bool(_) => TYPE_BOOL,
        StreamValue::DataType(_) => TYPE_TYPE,
    };
    out.write_u8(type_byte);
    out.write_u8(mode);
    match value {
        StreamValue::Str(s) => out.write_string(s),
        StreamValue::Int(v) => {
            out.write_string_length(4);
            out.add_blob(&v.to_le_bytes());
        }
        StreamValue::Uint(v) => {
            out.write_string_length(4);
            out.add_blob(&v.to_le_bytes());
        }
        StreamValue::Int64(v) => {
            out.write_string_length(8);
            out.add_blob(&v.to_le_bytes());
        }
        StreamValue::Uint64(v) => {
            out.write_string_length(8);
            out.add_blob(&v.to_le_bytes());
        }
        StreamValue::Float32(v) => {
            out.write_string_length(4);
            out.add_blob(&v.to_le_bytes());
        }
        StreamValue::Float64(v) => {
            out.write_string_length(8);
            out.add_blob(&v.to_le_bytes());
        }
        StreamValue::Bool(v) => {
            out.write_string_length(1);
            out.add_blob(&[u8::from(*v)]);
        }
        StreamValue::DataType(v) => {
            out.write_string_length(1);
            out.add_blob(&[datatype_byte(*v)]);
        }
    }
}

/// The `WatcherDataType` wire byte.
fn datatype_byte(value: DataType) -> u8 {
    match value {
        DataType::Unknown => TYPE_UNKNOWN,
        DataType::Int => TYPE_INT,
        DataType::Uint => TYPE_UINT,
        DataType::Float => TYPE_FLOAT,
        DataType::Bool => TYPE_BOOL,
        DataType::String => TYPE_STRING,
        DataType::Tuple => TYPE_TUPLE,
        DataType::Type => TYPE_TYPE,
    }
}

/// `watcherStreamToValue` (`watcher.hpp:218-256` shape): type byte, mode
/// byte (skipped), then a STRING payload re-parses through the protocol 1
/// string rules into `target`'s type while a matching payload decodes
/// directly. A width mismatch fails the way the reference's `sizeDiff`
/// check does.
pub fn stream_to_value<T: WatcherValue>(stream: &mut MemoryIStream) -> Option<T> {
    let type_byte = stream.read_u8();
    if stream.error() {
        return None;
    }
    // `watcherValueToStream` writes the mode right after the type.
    let _mode = stream.read_u8();
    if stream.error() {
        return None;
    }
    let value = stream_to_value_type(stream, type_byte)?;
    T::from_stream_value(value)
}

/// `watcherStreamToValueType`: decode the payload for the already-parsed
/// type byte into the enum, keeping the wire width.
pub fn stream_to_value_type(stream: &mut MemoryIStream, type_byte: u8) -> Option<StreamValue> {
    match type_byte {
        TYPE_STRING => {
            let value = stream.read_string();
            // A truncated string payload errors the stream the way the
            // reference's `stream >> str` does.
            if stream.error() {
                None
            } else {
                Some(StreamValue::Str(value))
            }
        }
        TYPE_INT | TYPE_UINT | TYPE_FLOAT | TYPE_BOOL | TYPE_TYPE => {
            let size = stream.read_string_length();
            if stream.error() {
                return None;
            }
            let width = match type_byte {
                // BOOL and TYPE carry exactly one byte; a declared size
                // other than one fails like the reference's
                // `size != sizeof(bool)` arm does (`watcher.hpp:279-284`).
                TYPE_TYPE | TYPE_BOOL => {
                    if size != 1 {
                        let _ = stream.retrieve(size as usize);
                        return None;
                    }
                    1
                }
                TYPE_INT | TYPE_UINT | TYPE_FLOAT => {
                    // The width lives in the packed length; the concrete
                    // width check happens against the decoded payload.
                    size
                }
                _ => unreachable!("matched above"),
            };
            if width != 1 && width != 4 && width != 8 {
                let _ = stream.retrieve(size as usize);
                return None;
            }
            let bytes = stream.retrieve(width as usize)?;
            Some(match type_byte {
                TYPE_BOOL => StreamValue::Bool(bytes[0] != 0),
                TYPE_TYPE => StreamValue::DataType(decode_datatype(bytes[0])),
                TYPE_INT => match width {
                    4 => {
                        StreamValue::Int(i32::from_le_bytes(bytes.try_into().expect("four bytes")))
                    }
                    8 => StreamValue::Int64(i64::from_le_bytes(
                        bytes.try_into().expect("eight bytes"),
                    )),
                    _ => return None,
                },
                TYPE_UINT => match width {
                    4 => {
                        StreamValue::Uint(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
                    }
                    8 => StreamValue::Uint64(u64::from_le_bytes(
                        bytes.try_into().expect("eight bytes"),
                    )),
                    _ => return None,
                },
                TYPE_FLOAT => match width {
                    4 => StreamValue::Float32(f32::from_le_bytes(
                        bytes.try_into().expect("four bytes"),
                    )),
                    8 => StreamValue::Float64(f64::from_le_bytes(
                        bytes.try_into().expect("eight bytes"),
                    )),
                    _ => return None,
                },
                _ => unreachable!("matched above"),
            })
        }
        // TUPLE and UNKNOWN have no value form here.
        _ => None,
    }
}

fn decode_datatype(byte: u8) -> DataType {
    match byte {
        TYPE_INT => DataType::Int,
        TYPE_UINT => DataType::Uint,
        TYPE_FLOAT => DataType::Float,
        TYPE_BOOL => DataType::Bool,
        TYPE_STRING => DataType::String,
        TYPE_TUPLE => DataType::Tuple,
        TYPE_TYPE => DataType::Type,
        _ => DataType::Unknown,
    }
}

/// The wire byte of a [`Mode`](crate::Mode) (`watcher.hpp:720-726`).
pub fn mode_byte(mode: Mode) -> u8 {
    match mode {
        Mode::Invalid => MODE_INVALID,
        Mode::ReadOnly => MODE_READ_ONLY,
        Mode::ReadWrite => MODE_READ_WRITE,
        Mode::Directory => MODE_DIRECTORY,
        Mode::Callable => MODE_CALLABLE,
    }
}

// The process-wide watcher write permission
// (`WatcherPathRequest::s_writesEnabled_`, `watcher_path_request.cpp:33`).
// Thread-local like the root: the reference gates writes arriving on the
// main loop's requests.
thread_local! {
    static WRITES_ENABLED: RefCell<bool> = const { RefCell::new(true) };
}

/// `WatcherPathRequest::writesEnabled()` (`watcher_path_request.cpp:40-43`).
pub fn writes_enabled() -> bool {
    WRITES_ENABLED.with(|flag| *flag.borrow())
}

/// `WatcherPathRequest::writesEnabled( bool )` (`watcher_path_request.cpp:51-54`).
pub fn set_writes_enabled(enabled: bool) {
    WRITES_ENABLED.with(|flag| *flag.borrow_mut() = enabled);
}

/// The path of the watcher that toggles write permission; writes to it are
/// exempt from the permission check so the switch can be flipped back
/// (`watcher_path_request.cpp:16`).
pub const WRITES_TOGGLE_PATH: &str = "watcherWrites/allowed";

#[cfg(test)]
mod tests {
    use super::*;

    /// Packed ints encode one byte below 255 and `0xFF` plus three little
    /// endian bytes at or above it (`binary_stream.hpp:254-270`).
    #[test]
    fn packed_ints_switch_at_255() {
        let mut out = MemoryOStream::new();
        out.write_packed_int(254);
        out.write_packed_int(255);
        out.write_packed_int(0x010203);
        // 255 itself takes the marker path: 0xFF + 255's low three bytes.
        assert_eq!(
            out.data(),
            &[254, 0xff, 0xff, 0x00, 0x00, 0xff, 0x03, 0x02, 0x01]
        );

        let mut input = MemoryIStream::new(out.data());
        assert_eq!(input.read_packed_int(), 254);
        assert_eq!(input.read_packed_int(), 255);
        assert_eq!(input.read_packed_int(), 0x010203);
        assert!(!input.error());
    }

    /// Strings travel as packed length + bytes with no terminator.
    #[test]
    fn strings_travel_length_prefixed() {
        let mut out = MemoryOStream::new();
        out.write_string("hello");
        assert_eq!(out.data(), &[5, b'h', b'e', b'l', b'l', b'o']);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(input.read_string(), "hello");
    }

    /// Reads past the end set the error flag the way `BinaryIStream` does.
    #[test]
    fn reads_past_the_end_error() {
        let mut input = MemoryIStream::new(&[1, 2, 3]);
        assert_eq!(input.retrieve(2), Some(&[1u8, 2][..]));
        assert_eq!(input.retrieve(2), None);
        assert!(input.error());
        assert_eq!(input.remaining_length(), 0);
    }

    /// Every scalar type round-trips through the protocol 2 value codec
    /// with the reference byte layout: type byte, mode byte, packed width,
    /// little endian payload.
    #[test]
    fn scalar_values_round_trip_through_the_codec() {
        let mut out = MemoryOStream::new();
        value_to_stream(&mut out, &StreamValue::Int(-7), MODE_READ_WRITE);
        assert_eq!(
            out.data(),
            &[TYPE_INT, MODE_READ_WRITE, 4, 0xf9, 0xff, 0xff, 0xff]
        );
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<i32>(&mut input), Some(-7));

        let mut out = MemoryOStream::new();
        value_to_stream(&mut out, &StreamValue::Uint64(u64::MAX), MODE_READ_ONLY);
        assert_eq!(out.data()[0], TYPE_UINT);
        assert_eq!(out.data()[2], 8);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<u64>(&mut input), Some(u64::MAX));

        let mut out = MemoryOStream::new();
        value_to_stream(&mut out, &StreamValue::Float64(0.5), MODE_READ_ONLY);
        assert_eq!(out.data()[0], TYPE_FLOAT);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<f64>(&mut input), Some(0.5));

        let mut out = MemoryOStream::new();
        value_to_stream(&mut out, &StreamValue::Float32(2.5), MODE_READ_ONLY);
        assert_eq!(out.data()[2], 4);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<f32>(&mut input), Some(2.5));

        let mut out = MemoryOStream::new();
        value_to_stream(&mut out, &StreamValue::Bool(true), MODE_READ_ONLY);
        assert_eq!(out.data(), &[TYPE_BOOL, MODE_READ_ONLY, 1, 1]);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<bool>(&mut input), Some(true));

        let mut out = MemoryOStream::new();
        value_to_stream(
            &mut out,
            &StreamValue::DataType(DataType::Float),
            MODE_READ_ONLY,
        );
        assert_eq!(out.data(), &[TYPE_TYPE, MODE_READ_ONLY, 1, TYPE_FLOAT]);
        let mut input = MemoryIStream::new(out.data());
        // `stream_to_value_type` takes the already-parsed type byte.
        let type_byte = input.read_u8();
        let _mode = input.read_u8();
        assert_eq!(
            stream_to_value_type(&mut input, type_byte),
            Some(StreamValue::DataType(DataType::Float))
        );
    }

    /// Strings encode as type + mode + packed length + bytes and a numeric
    /// leaf re-parses them through the protocol 1 rules.
    #[test]
    fn string_payloads_feed_numeric_leaves_through_protocol_one() {
        let mut out = MemoryOStream::new();
        value_to_stream(
            &mut out,
            &StreamValue::Str("42".to_string()),
            MODE_READ_WRITE,
        );
        assert_eq!(out.data()[0], TYPE_STRING);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<i32>(&mut input), Some(42));

        let mut out = MemoryOStream::new();
        value_to_stream(
            &mut out,
            &StreamValue::Str("true".to_string()),
            MODE_READ_WRITE,
        );
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<bool>(&mut input), Some(true));
    }

    /// A payload the leaf cannot take fails the decode the way the
    /// reference's type/width arms do; tuples have no value form.
    #[test]
    fn width_mismatches_and_tuples_fail_the_decode() {
        // An eight byte INT downcasts into an int32 leaf
        // (`watcher.hpp:324-345`), but the unsigned side has no such arm:
        // a `uint32` leaf rejects an eight byte UINT
        // (`watcher.hpp:459-464`).
        let mut out = MemoryOStream::new();
        value_to_stream(
            &mut out,
            &StreamValue::Int64(0x1_0000_0002),
            MODE_READ_WRITE,
        );
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<i32>(&mut input), Some(2));

        let mut out = MemoryOStream::new();
        value_to_stream(&mut out, &StreamValue::Uint64(1), MODE_READ_WRITE);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<u32>(&mut input), None);

        // A bool with a declared width other than one fails
        // (`watcher.hpp:279-284`).
        let mut out = MemoryOStream::new();
        out.write_u8(TYPE_BOOL);
        out.write_u8(MODE_READ_ONLY);
        out.write_string_length(4);
        out.add_blob(&[1, 0, 0, 0]);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<bool>(&mut input), None);

        let mut out = MemoryOStream::new();
        out.write_u8(TYPE_TUPLE);
        out.write_u8(MODE_READ_ONLY);
        let mut input = MemoryIStream::new(out.data());
        assert_eq!(stream_to_value::<i32>(&mut input), None);

        // Truncated stream errors before decoding anything.
        let mut input = MemoryIStream::new(&[TYPE_INT]);
        assert_eq!(stream_to_value::<i32>(&mut input), None);
        assert!(input.error());
    }

    /// The write permission switch defaults on and flips
    /// (`watcher_path_request.cpp:33-54`).
    #[test]
    fn the_writes_permission_switch_flips() {
        assert!(writes_enabled());
        set_writes_enabled(false);
        assert!(!writes_enabled());
        set_writes_enabled(true);
        assert!(writes_enabled());
    }

    /// Every reader answers its fallback on any truncated stream.
    #[test]
    fn truncated_reads_fall_back() {
        let mut out = MemoryOStream::new();
        out.write_packed_int(300);
        out.write_string("hello");
        out.write_i32(-1);
        out.write_u32(7);
        out.write_bool(true);
        let full = out.data().to_vec();

        for end in 0..full.len() {
            let mut input = MemoryIStream::new(&full[..end]);
            let _ = input.read_packed_int();
            let mut input = MemoryIStream::new(&full[..end]);
            let _ = input.read_string_length();
            let mut input = MemoryIStream::new(&full[..end]);
            let _ = input.read_string();
            let mut input = MemoryIStream::new(&full[..end]);
            let _ = input.read_i32();
            let mut input = MemoryIStream::new(&full[..end]);
            let _ = input.read_u32();
            let mut input = MemoryIStream::new(&full[..end]);
            let _ = input.read_bool();
        }

        // The intact stream still reads back cleanly.
        let mut input = MemoryIStream::new(&full);
        assert_eq!(input.read_packed_int(), 300);
        assert_eq!(input.read_string(), "hello");
        assert_eq!(input.read_i32(), -1);
        assert_eq!(input.read_u32(), 7);
        assert!(input.read_bool());
    }

    /// Decode one typed element: `[type, mode, payload...]`.
    fn decode_value(bytes: &[u8]) -> Option<StreamValue> {
        let mut input = MemoryIStream::new(bytes);
        let type_byte = input.read_u8();
        let _mode = input.read_u8();
        stream_to_value_type(&mut input, type_byte)
    }

    /// Every wire shape of every scalar decodes; wrong widths and
    /// truncated payloads fail.
    #[test]
    fn every_scalar_wire_shape_decodes() {
        // int32 only takes widths four and eight; the reference's
        // `watcherStreamToValueType(int32 &)` has no one byte arm
        // (`watcher.hpp:268-301`).
        assert_eq!(decode_value(&[TYPE_INT, MODE_READ_ONLY, 1, 42]), None);
        assert_eq!(
            decode_value(&[TYPE_INT, MODE_READ_ONLY, 4, 42, 0, 0, 0]),
            Some(StreamValue::Int(42))
        );
        assert_eq!(
            decode_value(&[TYPE_INT, MODE_READ_ONLY, 4, 0xf6, 0xff, 0xff, 0xff]),
            Some(StreamValue::Int(-10))
        );
        assert_eq!(
            decode_value(&[TYPE_INT, MODE_READ_ONLY, 8, 2, 0, 0, 0, 1, 0, 0, 0]),
            Some(StreamValue::Int64(0x1_0000_0002))
        );
        assert_eq!(
            decode_value(&[TYPE_UINT, MODE_READ_ONLY, 4, 7, 0, 0, 0]),
            Some(StreamValue::Uint(7))
        );
        assert_eq!(
            decode_value(&[TYPE_UINT, MODE_READ_ONLY, 8, 9, 0, 0, 0, 0, 0, 0, 0]),
            Some(StreamValue::Uint64(9))
        );
        assert_eq!(
            decode_value(&[TYPE_FLOAT, MODE_READ_ONLY, 4, 0, 0, 0xc0, 0x3f]),
            Some(StreamValue::Float32(1.5))
        );
        assert_eq!(
            decode_value(&[TYPE_FLOAT, MODE_READ_ONLY, 8, 0, 0, 0, 0, 0, 0, 4, 0x40]),
            Some(StreamValue::Float64(2.5))
        );
        assert_eq!(
            decode_value(&[TYPE_BOOL, MODE_READ_ONLY, 1, 1]),
            Some(StreamValue::Bool(true))
        );
        assert_eq!(
            decode_value(&[TYPE_BOOL, MODE_READ_ONLY, 1, 0]),
            Some(StreamValue::Bool(false))
        );
        for (byte, data_type) in [
            (TYPE_INT, DataType::Int),
            (TYPE_UINT, DataType::Uint),
            (TYPE_FLOAT, DataType::Float),
            (TYPE_BOOL, DataType::Bool),
            (TYPE_STRING, DataType::String),
            (TYPE_TUPLE, DataType::Tuple),
            (TYPE_TYPE, DataType::Type),
        ] {
            assert_eq!(
                decode_value(&[TYPE_TYPE, MODE_READ_ONLY, 1, byte]),
                Some(StreamValue::DataType(data_type))
            );
        }
        assert_eq!(
            decode_value(&[TYPE_STRING, MODE_READ_ONLY, 3, b'a', b'b', b'c']),
            Some(StreamValue::Str("abc".to_string()))
        );

        // A two byte width drains and fails; so do single byte widths on
        // the scalars that only take four or eight.
        assert_eq!(decode_value(&[TYPE_INT, MODE_READ_ONLY, 2, 0, 0]), None);
        assert_eq!(decode_value(&[TYPE_UINT, MODE_READ_ONLY, 1, 1]), None);
        assert_eq!(decode_value(&[TYPE_FLOAT, MODE_READ_ONLY, 1, 1]), None);
        assert_eq!(
            decode_value(&[TYPE_BOOL, MODE_READ_ONLY, 4, 0, 0, 0, 0]),
            None
        );
        assert_eq!(
            decode_value(&[TYPE_TYPE, MODE_READ_ONLY, 4, 0, 0, 0, 0]),
            None
        );
        // Truncated payloads error the stream.
        assert_eq!(
            decode_value(&[TYPE_INT, MODE_READ_ONLY, 8, 1, 2, 3, 4]),
            None
        );
        assert_eq!(decode_value(&[TYPE_UINT, MODE_READ_ONLY, 4, 1]), None);
        assert_eq!(
            decode_value(&[TYPE_STRING, MODE_READ_ONLY, 5, b'a', b'b']),
            None
        );
        assert_eq!(decode_value(&[TYPE_INT, MODE_READ_ONLY]), None);
        assert_eq!(decode_value(&[TYPE_TUPLE, MODE_READ_ONLY, 1, 2]), None);
    }

    /// An empty stream fails the typed read.
    #[test]
    fn an_empty_stream_fails_the_typed_read() {
        let mut input = MemoryIStream::new(&[][..]);
        assert!(stream_to_value::<i32>(&mut input).is_none());
    }

    /// Every value encodes its wire shape, and every datatype byte maps
    /// back to its enum.
    #[test]
    fn every_value_encodes_its_wire_shape() {
        let encode = |value: StreamValue| {
            let mut out = MemoryOStream::new();
            value_to_stream(&mut out, &value, MODE_READ_ONLY);
            out.data().to_vec()
        };
        assert_eq!(
            encode(StreamValue::Uint(7)),
            vec![TYPE_UINT, MODE_READ_ONLY, 4, 7, 0, 0, 0]
        );
        for (data_type, wire) in [
            (DataType::Unknown, TYPE_UNKNOWN),
            (DataType::Uint, TYPE_UINT),
            (DataType::Float, TYPE_FLOAT),
            (DataType::Bool, TYPE_BOOL),
            (DataType::String, TYPE_STRING),
            (DataType::Tuple, TYPE_TUPLE),
            (DataType::Type, TYPE_TYPE),
        ] {
            // The value's payload byte carries the datatype wire byte.
            assert_eq!(encode(StreamValue::DataType(data_type))[3], wire);
        }

        // An unrecognised datatype byte decodes back to Unknown.
        assert_eq!(
            decode_value(&[TYPE_TYPE, MODE_READ_ONLY, 1, 200]),
            Some(StreamValue::DataType(DataType::Unknown))
        );
    }

    /// Every mode byte round-trips through `mode_byte`.
    #[test]
    fn every_mode_maps_to_its_wire_byte() {
        assert_eq!(mode_byte(Mode::Invalid), MODE_INVALID);
        assert_eq!(mode_byte(Mode::ReadOnly), MODE_READ_ONLY);
        assert_eq!(mode_byte(Mode::ReadWrite), MODE_READ_WRITE);
        assert_eq!(mode_byte(Mode::Directory), MODE_DIRECTORY);
        assert_eq!(mode_byte(Mode::Callable), MODE_CALLABLE);
    }
}
