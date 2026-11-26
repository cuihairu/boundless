//! `ForwardingStringHandler` (`lib/network/forwarding_string_handler.*`): the
//! per-format-string handler on the component side. The reference caches the
//! tokens `handleFormatString` reported, then `parseArgs` walks a C
//! `va_list` in that order; the port walks an explicit [`ArgValue`] slice in
//! the same order and writes exactly the bytes `operator<<` would. The
//! read-side mirror (`decode_args`) is what a MessageLogger needs to accept
//! a record it has no va_list for.

use crate::format_string::{FormatStringHandler, conv_flags, handle_format_string, variable_flags};
use crate::stream::{MemoryIStream, MemoryOStream};

/// One format argument, typed the way `parseArgs` would have pulled it from
/// the va_list (`forwarding_string_handler.cpp:88-235`).
#[derive(Clone, PartialEq, Debug)]
pub enum ArgValue {
    /// A signed value: sent as `short`/`int32`/`int64` per the conversion's
    /// length modifier.
    Int(i64),
    /// An unsigned value: sent as `unsigned short`/`uint32`/`uint64`.
    Uint(u64),
    /// A floating-point value: always sent as an eight-byte double.
    Float(f64),
    /// A string: packed length plus bytes, `(null)` for a NULL pointer.
    Str(String),
    /// A single character: one byte.
    Char(u8),
    /// A pointer, sent as an eight-byte signed value since protocol
    /// version 7.
    Pointer(i64),
}

/// The cached token for one conversion (`ForwardingStringHandler::FormatData`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FormatData {
    /// The conversion character, normalised (`%X` reports as `x`).
    pub type_char: char,
    /// The length modifier.
    pub cflags: i32,
    /// The variable-width markers.
    pub vflags: i32,
}

/// The per-format-string handler (`ForwardingStringHandler`,
/// `forwarding_string_handler.hpp:23-63`).
pub struct ForwardingStringHandler {
    fmt: String,
    fmt_data: Vec<FormatData>,
    num_recent_calls: u32,
    is_suppressible: bool,
    is_ok: bool,
}

impl ForwardingStringHandler {
    /// `ForwardingStringHandler( fmt, isSuppressible )`
    /// (`forwarding_string_handler.cpp:26-46`).
    pub fn new(fmt: &str, is_suppressible: bool) -> Self {
        let mut handler = Self {
            fmt: fmt.to_string(),
            fmt_data: Vec::new(),
            num_recent_calls: 0,
            is_suppressible,
            is_ok: false,
        };
        handler.is_ok = handle_format_string(fmt, &mut handler);
        handler
    }

    /// `fmt()`.
    pub fn fmt(&self) -> &str {
        &self.fmt
    }

    /// `isOk()`: false when the format string had an illegal conversion.
    pub fn is_ok(&self) -> bool {
        self.is_ok
    }

    /// `isSuppressible()`.
    pub fn is_suppressible(&self) -> bool {
        self.is_suppressible
    }

    /// `numRecentCalls()`.
    pub fn num_recent_calls(&self) -> u32 {
        self.num_recent_calls
    }

    /// `addRecentCall()`.
    pub fn add_recent_call(&mut self) {
        self.num_recent_calls += 1;
    }

    /// `clearRecentCalls()`.
    pub fn clear_recent_calls(&mut self) {
        self.num_recent_calls = 0;
    }

    /// The cached tokens, in argument order.
    pub fn fmt_data(&self) -> &[FormatData] {
        &self.fmt_data
    }

    /// `parseArgs( argPtr, os )` (`forwarding_string_handler.cpp:88-235`):
    /// encode the arguments into the record stream. Extra arguments beyond
    /// the cached tokens are ignored, like a va_list the format never
    /// reads; missing arguments emit nothing the way a malformed va_list
    /// would truncate.
    pub fn encode_args(&self, args: &[ArgValue], stream: &mut MemoryOStream) {
        let mut iter = args.iter();

        for data in &self.fmt_data {
            if data.vflags & variable_flags::VARIABLE_MIN_WIDTH != 0 {
                // `os << (WidthType)va_arg( args, int )` — WidthType is
                // `short` (`bsd_snprintf.h:27`).
                let width = next_i64(&mut iter);
                stream.write_u16(width as i16 as u16);
            }

            if data.vflags & variable_flags::VARIABLE_MAX_WIDTH != 0 {
                let width = next_i64(&mut iter);
                stream.write_u16(width as i16 as u16);
            }

            match data.type_char {
                'd' => match data.cflags {
                    conv_flags::DP_C_SHORT => {
                        stream.write_u16(next_i64(&mut iter) as i16 as u16);
                    }
                    // Since protocol version 7 a long travels as eight
                    // bytes (`forwarding_string_handler.cpp:111-115`).
                    conv_flags::DP_C_LONG | conv_flags::DP_C_LLONG => {
                        stream.write_u64(next_i64(&mut iter) as u64);
                    }
                    _ => {
                        stream.write_u32(next_i64(&mut iter) as i32 as u32);
                    }
                },
                'o' | 'u' | 'x' => match data.cflags {
                    conv_flags::DP_C_SHORT => {
                        stream.write_u16(next_u64(&mut iter) as u16);
                    }
                    conv_flags::DP_C_LONG | conv_flags::DP_C_LLONG => {
                        stream.write_u64(next_u64(&mut iter));
                    }
                    _ => {
                        stream.write_u32(next_u64(&mut iter) as u32);
                    }
                },
                // Doubles always travel as eight bytes; the long-double arm
                // reads back as a double on the builds this protocol ships.
                'f' | 'e' | 'g' => {
                    stream.write_u64(next_f64(&mut iter).to_bits());
                }
                's' => {
                    let text = match iter.next() {
                        Some(ArgValue::Str(text)) => text.clone(),
                        _ => "(null)".to_string(),
                    };
                    stream.write_string(&text);
                }
                'p' => {
                    stream.write_u64(next_i64(&mut iter) as u64);
                }
                'c' => {
                    stream.write_u8(next_i64(&mut iter) as u8);
                }
                _ => {
                    // Any other conversion cannot reach here:
                    // handleFormatString rejects unknown conversions before
                    // a token is cached.
                }
            }
        }
    }

    /// The read-side mirror a MessageLogger runs: decode the arguments a
    /// record carries, in token order (`log_string_interpolator`'s input).
    /// Returns `None` when the stream runs out.
    pub fn decode_args(&self, stream: &mut MemoryIStream) -> Option<Vec<ArgValue>> {
        let mut args = Vec::new();

        for data in &self.fmt_data {
            if data.vflags & variable_flags::VARIABLE_MIN_WIDTH != 0 {
                let width = stream.read_u16()?;
                args.push(ArgValue::Int(width as i16 as i64));
            }

            if data.vflags & variable_flags::VARIABLE_MAX_WIDTH != 0 {
                let width = stream.read_u16()?;
                args.push(ArgValue::Int(width as i16 as i64));
            }

            match data.type_char {
                'd' => match data.cflags {
                    conv_flags::DP_C_SHORT => {
                        args.push(ArgValue::Int(stream.read_u16()? as i16 as i64));
                    }
                    conv_flags::DP_C_LONG | conv_flags::DP_C_LLONG => {
                        args.push(ArgValue::Int(stream.read_u64()? as i64));
                    }
                    _ => args.push(ArgValue::Int(stream.read_u32()? as i32 as i64)),
                },
                'o' | 'u' | 'x' => match data.cflags {
                    conv_flags::DP_C_SHORT => {
                        args.push(ArgValue::Uint(stream.read_u16()? as u64));
                    }
                    conv_flags::DP_C_LONG | conv_flags::DP_C_LLONG => {
                        args.push(ArgValue::Uint(stream.read_u64()?));
                    }
                    _ => args.push(ArgValue::Uint(stream.read_u32()? as u64)),
                },
                'f' | 'e' | 'g' => {
                    args.push(ArgValue::Float(f64::from_bits(stream.read_u64()?)));
                }
                's' => args.push(ArgValue::Str(stream.read_string())),
                'p' => args.push(ArgValue::Pointer(stream.read_u64()? as i64)),
                'c' => args.push(ArgValue::Char(stream.read_u8()?)),
                _ => return None,
            }
        }

        Some(args)
    }
}

impl FormatStringHandler for ForwardingStringHandler {
    /// `onToken` caches just the fields `parseArgs` needs
    /// (`forwarding_string_handler.cpp:48-53`).
    fn on_token(
        &mut self,
        type_char: char,
        cflags: i32,
        _min: i32,
        _max: i32,
        _flags: i32,
        _base: u8,
        vflags: i32,
    ) {
        self.fmt_data.push(FormatData {
            type_char,
            cflags,
            vflags,
        });
    }
}

fn next_i64<'a, I>(iter: &mut I) -> i64
where
    I: Iterator<Item = &'a ArgValue>,
{
    match iter.next() {
        Some(ArgValue::Int(value)) => *value,
        Some(ArgValue::Uint(value)) => *value as i64,
        Some(ArgValue::Float(value)) => *value as i64,
        Some(ArgValue::Char(value)) => *value as i64,
        Some(ArgValue::Pointer(value)) => *value,
        None | Some(ArgValue::Str(_)) => 0,
    }
}

fn next_u64<'a, I>(iter: &mut I) -> u64
where
    I: Iterator<Item = &'a ArgValue>,
{
    match iter.next() {
        Some(ArgValue::Uint(value)) => *value,
        Some(ArgValue::Int(value)) => *value as u64,
        Some(ArgValue::Float(value)) => *value as u64,
        Some(ArgValue::Char(value)) => *value as u64,
        Some(ArgValue::Pointer(value)) => *value as u64,
        None | Some(ArgValue::Str(_)) => 0,
    }
}

fn next_f64<'a, I>(iter: &mut I) -> f64
where
    I: Iterator<Item = &'a ArgValue>,
{
    match iter.next() {
        Some(ArgValue::Float(value)) => *value,
        Some(ArgValue::Int(value)) => *value as f64,
        Some(ArgValue::Uint(value)) => *value as f64,
        _ => 0.0,
    }
}

/// `LoggerMessageForwarder::parseAndSend` minus the logger fan-out
/// (`logger_message_forwarder.cpp:819-872`): the transport message ID, the
/// header, the format string, then the encoded arguments. The metadata
/// variants a metadata-aware logger receives are appended after these bytes
/// and arrive with the record (BW-12-02-02).
pub fn build_record(
    header: &crate::message_header::MessageHeader,
    handler: &ForwardingStringHandler,
    args: &[ArgValue],
) -> Vec<u8> {
    let mut stream = MemoryOStream::new();
    stream.write_u32(crate::message_id::MESSAGE_LOGGER_MSG as u32);
    header.write(&mut stream);
    stream.write_string(handler.fmt());
    handler.encode_args(args, &mut stream);
    stream.data().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_header::MessageHeader;
    use crate::message_priority;
    use crate::stream::MemoryOStream;

    #[test]
    fn an_invalid_format_string_is_not_ok() {
        let handler = ForwardingStringHandler::new("%y", false);
        assert!(!handler.is_ok());

        let handler = ForwardingStringHandler::new("%d apples", false);
        assert!(handler.is_ok());
        assert_eq!(handler.fmt(), "%d apples");
        assert_eq!(handler.num_recent_calls(), 0);
        assert!(!handler.is_suppressible());
    }

    #[test]
    fn the_token_cache_matches_the_format() {
        let handler = ForwardingStringHandler::new("%hd %llu %s", false);
        assert_eq!(handler.fmt_data().len(), 3);
        assert_eq!(handler.fmt_data()[0].type_char, 'd');
        assert_eq!(handler.fmt_data()[0].cflags, conv_flags::DP_C_SHORT);
        assert_eq!(handler.fmt_data()[1].cflags, conv_flags::DP_C_LLONG);
        assert_eq!(handler.fmt_data()[2].type_char, 's');
    }

    #[test]
    fn recent_call_bookkeeping_counts_and_clears() {
        let mut handler = ForwardingStringHandler::new("%d", false);
        handler.add_recent_call();
        handler.add_recent_call();
        handler.add_recent_call();
        assert_eq!(handler.num_recent_calls(), 3);
        handler.clear_recent_calls();
        assert_eq!(handler.num_recent_calls(), 0);
    }

    #[test]
    fn scalar_arguments_encode_the_reference_widths() {
        let handler = ForwardingStringHandler::new("%d %hd %ld %u %lu %f", false);
        let mut stream = MemoryOStream::new();
        handler.encode_args(
            &[
                ArgValue::Int(-1),
                ArgValue::Int(300), // wraps into a short
                ArgValue::Int(-2),  // eight bytes
                ArgValue::Uint(7),
                ArgValue::Uint(8),
                ArgValue::Float(1.5),
            ],
            &mut stream,
        );
        assert_eq!(
            stream.data(),
            &[
                0xff, 0xff, 0xff, 0xff, // %d: int32 -1
                0x2c, 0x01, // %hd: short 300
                0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // %ld: int64 -2
                0x07, 0x00, 0x00, 0x00, // %u: uint32 7
                0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // %lu: uint64 8
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf8, 0x3f // %f: double 1.5
            ]
        );
    }

    #[test]
    fn strings_chars_pointers_and_variable_widths_encode() {
        let handler = ForwardingStringHandler::new("%*d %.*s %c %p %s", false);
        let mut stream = MemoryOStream::new();
        handler.encode_args(
            &[
                ArgValue::Int(6),                    // variable min width
                ArgValue::Int(42),                   // %d
                ArgValue::Int(3),                    // variable max width
                ArgValue::Str("abcdef".to_string()), // %.*s: the whole
                // string travels; the
                // precision only trims
                // at display time
                ArgValue::Char(b'x'),
                ArgValue::Pointer(0x1234),
                ArgValue::Str("tail".to_string()),
            ],
            &mut stream,
        );
        let mut expected = Vec::new();
        expected.extend_from_slice(&6i16.to_le_bytes());
        expected.extend_from_slice(&42i32.to_le_bytes());
        expected.extend_from_slice(&3i16.to_le_bytes());
        expected.push(6);
        expected.extend_from_slice(b"abcdef");
        expected.push(b'x');
        expected.extend_from_slice(&0x1234i64.to_le_bytes());
        expected.push(4);
        expected.extend_from_slice(b"tail");
        assert_eq!(stream.data(), &expected[..]);
    }

    #[test]
    fn decode_args_reads_back_what_encode_args_wrote() {
        let handler = ForwardingStringHandler::new("%d %s %u %f", false);
        let args = [
            ArgValue::Int(-7),
            ArgValue::Str("hello".to_string()),
            ArgValue::Uint(9),
            ArgValue::Float(2.25),
        ];
        let mut stream = MemoryOStream::new();
        handler.encode_args(&args, &mut stream);
        let mut input = MemoryIStream::new(stream.data());
        let back = handler.decode_args(&mut input).expect("decodes");
        assert_eq!(back, args.to_vec());
        assert_eq!(input.remaining(), 0);
    }

    #[test]
    fn decoding_past_the_end_fails_cleanly() {
        let handler = ForwardingStringHandler::new("%d %d", false);
        let mut stream = MemoryOStream::new();
        handler.encode_args(&[ArgValue::Int(1), ArgValue::Int(2)], &mut stream);
        for cut in 1..stream.size() {
            let mut input = MemoryIStream::new(&stream.data()[..cut]);
            assert!(handler.decode_args(&mut input).is_none());
        }
    }

    #[test]
    fn a_record_carries_id_header_format_and_args() {
        let header = MessageHeader {
            message_priority: message_priority::INFO,
            ..Default::default()
        };
        let handler = ForwardingStringHandler::new("%s=%d", false);
        let record = build_record(
            &header,
            &handler,
            &[ArgValue::Str("hp".to_string()), ArgValue::Int(100)],
        );

        // The leading int is the transport message ID, then the header
        // (priority, priority, source, packed category), the packed format
        // string, and the arguments.
        let mut expected = Vec::new();
        expected.extend_from_slice(&107u32.to_le_bytes());
        expected.push(0); // component priority, pinned zero
        expected.push(message_priority::INFO);
        expected.push(0); // C++ source
        expected.push(0); // empty category
        expected.push(5);
        expected.extend_from_slice(b"%s=%d");
        expected.push(2);
        expected.extend_from_slice(b"hp");
        expected.extend_from_slice(&100i32.to_le_bytes());
        assert_eq!(record, expected);
    }

    #[test]
    fn a_null_string_argument_travels_as_the_placeholder() {
        let handler = ForwardingStringHandler::new("%s", false);
        let mut stream = MemoryOStream::new();
        // Any non-string argument decodes to the NULL placeholder.
        handler.encode_args(&[ArgValue::Int(1)], &mut stream);
        assert_eq!(stream.data(), &[6, b'(', b'n', b'u', b'l', b'l', b')']);
    }

    #[test]
    fn unsigned_short_conversions_encode_two_bytes() {
        let handler = ForwardingStringHandler::new("%hu %ho %hx", false);
        let mut stream = MemoryOStream::new();
        handler.encode_args(
            &[ArgValue::Uint(0x1234), ArgValue::Uint(5), ArgValue::Uint(6)],
            &mut stream,
        );
        assert_eq!(stream.data(), &[0x34, 0x12, 5, 0, 6, 0]);
    }

    #[test]
    fn coercions_between_argument_kinds_match_the_va_list_pull() {
        // A C va_list delivers whatever type the caller pushed; the port
        // converts explicitly. Each conversion exercises one arm.
        let mut stream = MemoryOStream::new();

        let int_handler = ForwardingStringHandler::new("%d %d %d %d", false);
        int_handler.encode_args(
            &[
                ArgValue::Float(2.75), // float pulled as an int
                ArgValue::Char(b'z'),  // char pulled as an int
                ArgValue::Int(-9),     // the normal arm
                ArgValue::Pointer(66), // pointer pulled as an int
            ],
            &mut stream,
        );
        let mut expected = 2i32.to_le_bytes().to_vec();
        expected.extend_from_slice(&(b'z' as i32).to_le_bytes());
        expected.extend_from_slice(&(-9i32).to_le_bytes());
        expected.extend_from_slice(&66i32.to_le_bytes());
        assert_eq!(stream.data(), &expected[..]);

        let mut stream = MemoryOStream::new();
        let uint_handler = ForwardingStringHandler::new("%u %u %u %u %u", false);
        uint_handler.encode_args(
            &[
                ArgValue::Int(5),               // signed pulled as unsigned
                ArgValue::Float(3.5),           // float pulled as unsigned
                ArgValue::Char(b'a'),           // char pulled as unsigned
                ArgValue::Pointer(-1),          // pointer pulled as unsigned
                ArgValue::Str("x".to_string()), // no string arg ever reaches %u
            ],
            &mut stream,
        );
        assert_eq!(stream.data()[..4], 5u32.to_le_bytes());
        assert_eq!(stream.data()[4..8], 3u32.to_le_bytes());
        assert_eq!(stream.data()[8..12], (b'a' as u32).to_le_bytes());
        assert_eq!(stream.data()[12..16], u32::MAX.to_le_bytes());

        let mut stream = MemoryOStream::new();
        let float_handler = ForwardingStringHandler::new("%f %f %f", false);
        float_handler.encode_args(
            &[
                ArgValue::Int(4),
                ArgValue::Uint(5),                 // unsigned pulled as a float
                ArgValue::Str("nope".to_string()), // no string arg ever reaches %f
            ],
            &mut stream,
        );
        assert_eq!(stream.data()[..8], 4f64.to_bits().to_le_bytes());
        assert_eq!(stream.data()[8..16], 5f64.to_bits().to_le_bytes());
        assert_eq!(stream.data()[16..], 0f64.to_bits().to_le_bytes());
    }

    #[test]
    fn decode_reads_variable_widths_and_every_length_modifier() {
        let handler = ForwardingStringHandler::new("%*d %.*s %hd %ld %hu %lu", false);
        let args = [
            ArgValue::Int(10),
            ArgValue::Int(1),
            ArgValue::Int(3),
            ArgValue::Str("x".to_string()),
            ArgValue::Int(-2),
            ArgValue::Int(-3),
            ArgValue::Uint(4),
            ArgValue::Uint(5),
        ];
        let mut stream = MemoryOStream::new();
        handler.encode_args(&args, &mut stream);
        let mut input = MemoryIStream::new(stream.data());
        let back = handler.decode_args(&mut input).expect("decodes");
        assert_eq!(
            back,
            vec![
                ArgValue::Int(10),
                ArgValue::Int(1),
                ArgValue::Int(3),
                ArgValue::Str("x".to_string()),
                ArgValue::Int(-2),
                ArgValue::Int(-3),
                ArgValue::Uint(4),
                ArgValue::Uint(5),
            ]
        );
        assert_eq!(input.remaining(), 0);
    }

    #[test]
    fn extra_arguments_are_ignored_like_an_unread_va_list() {
        let handler = ForwardingStringHandler::new("%d", false);
        let mut stream = MemoryOStream::new();
        handler.encode_args(&[ArgValue::Int(5), ArgValue::Int(9)], &mut stream);
        assert_eq!(stream.data(), &5u32.to_le_bytes());
    }
}
