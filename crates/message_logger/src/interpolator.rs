//! `LogStringInterpolator` + `LogStringPrinter`
//! (`server/tools/message_logger/log_string_interpolator.cpp`,
//! `log_string_printer.cpp`): the read side of an ingested log record. The
//! interpolator splits a format string into literal segments and conversion
//! tokens, then walks the argument stream — with the protocol-version
//! dependent widths — handing each piece to a handler. The string printer is
//! the handler that renders the pieces through the `bsd_format` helpers.

use crate::bsd_format::{bsd_format_float, bsd_format_int, bsd_format_string};
use crate::format_string::{FormatStringHandler, handle_format_string, variable_flags};
use crate::stream::MemoryIStream;

/// The protocol version from which `long` conversions carry 64 bits
/// (`MESSAGE_LOGGER_VERSION_64BIT`, `log_string_interpolator.cpp:22`).
const MESSAGE_LOGGER_VERSION_64BIT: u8 = 7;

/// One conversion token, as the reference's runtime `FormatData`.
#[derive(Clone, Debug)]
pub struct FormatData {
    /// The conversion character (`d`/`u`/`x`/`f`/`e`/`g`/`s`/`c`/`p`).
    pub type_char: char,
    /// The length modifier (`DP_C_*`).
    pub cflags: i32,
    /// The radix for integer conversions.
    pub base: u8,
    /// The field width (`%10d`).
    pub min: i32,
    /// The precision (`%.3s`), -1 when unset.
    pub max: i32,
    /// The format flags (`DP_F_*`).
    pub flags: i32,
    /// The variable-width markers (`VARIABLE_*`).
    pub vflags: i32,
}

/// The callbacks `LogStringInterpolator::interpolate` reports through — the
/// `LogStringPrinter`/`LogStringWriter` seam (`log_string_printer.hpp`).
pub trait InterpolationHandler {
    /// A literal slice of the format string, `[start, end)`.
    fn on_fmt_string_section(&mut self, _format: &str, _start: usize, _end: usize) {}

    /// A variable field width read off the argument stream; the handler may
    /// rewrite the token's width.
    fn on_min_width(&mut self, _width: i16, _fd: &mut FormatData) {}

    /// A variable precision read off the argument stream.
    fn on_max_width(&mut self, _width: i16, _fd: &mut FormatData) {}

    /// An integer conversion (`d`/`o`/`u`/`x`), widened to 64 bits.
    fn on_int(&mut self, _value: i64, _fd: &FormatData) {}

    /// A float conversion (`f`/`e`/`g`).
    fn on_float(&mut self, _value: f64, _fd: &FormatData) {}

    /// A string conversion.
    fn on_string(&mut self, _value: &str, _fd: &FormatData) {}

    /// A pointer conversion, carried as an `int64`.
    fn on_pointer(&mut self, _value: i64, _fd: &FormatData) {}

    /// A character conversion.
    fn on_char(&mut self, _value: u8, _fd: &FormatData) {}

    /// The argument stream ran out or a conversion was unhandled.
    fn on_error(&mut self) {}

    /// All conversions consumed.
    fn on_parse_complete(&mut self) {}
}

/// The reference promotes 32-bit `long` reads from old versions to the
/// 64-bit value it would have read today.
fn read_i64_long(stream: &mut MemoryIStream, version: u8) -> Option<i64> {
    if version < MESSAGE_LOGGER_VERSION_64BIT {
        stream.read_u32().map(|v| v as i32 as i64)
    } else {
        stream.read_u64().map(|v| v as i64)
    }
}

/// The unsigned twin of `read_i64_long`.
fn read_u64_long(stream: &mut MemoryIStream, version: u8) -> Option<u64> {
    if version < MESSAGE_LOGGER_VERSION_64BIT {
        stream.read_u32().map(u64::from)
    } else {
        stream.read_u64()
    }
}

/// `LogStringInterpolator`: the parsed format string plus the walking logic.
#[derive(Clone, Debug)]
pub struct LogStringInterpolator {
    format_string: String,
    /// `'s'` for a literal segment, `'t'` for a conversion token, one per
    /// piece in order.
    components: Vec<u8>,
    /// `[start, end)` of each literal segment, in order.
    string_offsets: Vec<(usize, usize)>,
    /// Each conversion token, in order.
    fmt_data: Vec<FormatData>,
    /// False when `handleFormatString` rejected the format.
    is_ok: bool,
}

impl LogStringInterpolator {
    /// `LogStringInterpolator( formatString )`.
    pub fn new(format: &str) -> Self {
        let mut interpolator = Self {
            format_string: format.to_string(),
            components: Vec::new(),
            string_offsets: Vec::new(),
            fmt_data: Vec::new(),
            is_ok: false,
        };
        interpolator.is_ok = handle_format_string(format, &mut interpolator);
        interpolator
    }

    /// `formatString()`.
    pub fn format_string(&self) -> &str {
        &self.format_string
    }

    /// Whether the format string parsed cleanly.
    pub fn is_ok(&self) -> bool {
        self.is_ok
    }

    /// `interpolate( handler, inputStream, version )`
    /// (`log_string_interpolator.cpp:200-330`): walk the pieces, pulling
    /// argument bytes from `stream`. Returns false (after `on_error`) when
    /// the stream is too short or a conversion is unhandled.
    pub fn interpolate<H: InterpolationHandler>(
        &self,
        stream: &mut MemoryIStream,
        version: u8,
        handler: &mut H,
    ) -> bool {
        let mut string_iter = self.string_offsets.iter();
        let mut fmt_iter = self.fmt_data.iter();

        for component in &self.components {
            if *component == b's' {
                let (start, end) = string_iter.next().copied().unwrap_or((0, 0));
                handler.on_fmt_string_section(&self.format_string, start, end);
                continue;
            }

            let mut fd = fmt_iter.next().cloned().unwrap_or(FormatData {
                type_char: 'd',
                cflags: 0,
                base: 10,
                min: 0,
                max: -1,
                flags: 0,
                vflags: 0,
            });

            macro_rules! check_stream {
                () => {
                    if stream.error() {
                        handler.on_error();
                        return false;
                    }
                };
            }

            if fd.vflags & variable_flags::VARIABLE_MIN_WIDTH != 0 {
                let width = stream.read_u16().unwrap_or(0);
                check_stream!();
                handler.on_min_width(width as i16, &mut fd);
            }

            if fd.vflags & variable_flags::VARIABLE_MAX_WIDTH != 0 {
                let width = stream.read_u16().unwrap_or(0);
                check_stream!();
                handler.on_max_width(width as i16, &mut fd);
            }

            // The reference promotes 32-bit `long` reads from old versions
            // to the 64-bit value it would have read today.
            match fd.type_char {
                'd' => match fd.cflags {
                    crate::format_string::conv_flags::DP_C_SHORT => {
                        let value = stream.read_u16().unwrap_or(0) as i16;
                        check_stream!();
                        handler.on_int(value as i64, &fd);
                    }
                    crate::format_string::conv_flags::DP_C_LONG => {
                        let value = read_i64_long(stream, version);
                        check_stream!();
                        handler.on_int(value.unwrap_or(0), &fd);
                    }
                    crate::format_string::conv_flags::DP_C_LLONG => {
                        let value = stream.read_u64().unwrap_or(0) as i64;
                        check_stream!();
                        handler.on_int(value, &fd);
                    }
                    _ => {
                        let value = stream.read_u32().unwrap_or(0) as i32;
                        check_stream!();
                        handler.on_int(value as i64, &fd);
                    }
                },
                'o' | 'u' | 'x' => match fd.cflags {
                    crate::format_string::conv_flags::DP_C_SHORT => {
                        let value = stream.read_u16().unwrap_or(0);
                        check_stream!();
                        handler.on_int(value as i64, &fd);
                    }
                    crate::format_string::conv_flags::DP_C_LONG => {
                        let value = read_u64_long(stream, version);
                        check_stream!();
                        handler.on_int(value.unwrap_or(0) as i64, &fd);
                    }
                    crate::format_string::conv_flags::DP_C_LLONG => {
                        let value = stream.read_u64().unwrap_or(0);
                        check_stream!();
                        handler.on_int(value as i64, &fd);
                    }
                    _ => {
                        let value = stream.read_u32().unwrap_or(0);
                        check_stream!();
                        handler.on_int(value as i64, &fd);
                    }
                },
                'f' | 'e' | 'g' => {
                    // LDOUBLE travels as eight bytes on every ABI we port.
                    let value = stream.read_u64().unwrap_or(0);
                    check_stream!();
                    handler.on_float(f64::from_bits(value), &fd);
                }
                's' => {
                    let value = stream.read_string();
                    check_stream!();
                    handler.on_string(&value, &fd);
                }
                'p' => {
                    let value = stream.read_u64().unwrap_or(0) as i64;
                    check_stream!();
                    handler.on_pointer(value, &fd);
                }
                'c' => {
                    let value = stream.read_u8().unwrap_or(0);
                    check_stream!();
                    handler.on_char(value, &fd);
                }
                _ => {
                    handler.on_error();
                    return false;
                }
            }
        }

        handler.on_parse_complete();
        true
    }
}

/// `LogStringPrinter` (`log_string_printer.cpp`): renders every piece into
/// one owned string through the `bsd_format` helpers.
#[derive(Default)]
pub struct StringPrinter {
    result: String,
}

impl StringPrinter {
    /// The rendered text so far.
    pub fn result(&self) -> &str {
        &self.result
    }

    /// `onString`: pad-and-copy via `bsdFormatString`.
    pub fn render_string(&mut self, value: &str, fd: &FormatData) {
        bsd_format_string(value, fd.flags, fd.min, fd.max, &mut self.result);
    }
}

impl InterpolationHandler for StringPrinter {
    fn on_fmt_string_section(&mut self, format: &str, start: usize, end: usize) {
        bsd_format_string(
            &format[start..end],
            0,
            0,
            (end - start) as i32,
            &mut self.result,
        );
    }

    fn on_min_width(&mut self, width: i16, fd: &mut FormatData) {
        fd.min = width as i32;
    }

    fn on_max_width(&mut self, width: i16, fd: &mut FormatData) {
        fd.max = width as i32;
    }

    fn on_int(&mut self, value: i64, fd: &FormatData) {
        bsd_format_int(value, fd.base, fd.min, fd.max, fd.flags, &mut self.result);
    }

    fn on_float(&mut self, value: f64, fd: &FormatData) {
        bsd_format_float(value, fd.min, fd.max, fd.flags, &mut self.result);
    }

    fn on_string(&mut self, value: &str, fd: &FormatData) {
        self.render_string(value, fd);
    }

    fn on_pointer(&mut self, value: i64, fd: &FormatData) {
        let rendered = format!("0x{:x}", value as u64);
        self.render_string(&rendered, fd);
    }

    fn on_char(&mut self, value: u8, fd: &FormatData) {
        let rendered = (value as char).to_string();
        self.render_string(&rendered, fd);
    }
}

impl FormatStringHandler for LogStringInterpolator {
    fn on_string(&mut self, start: usize, end: usize) {
        self.string_offsets.push((start, end));
        self.components.push(b's');
    }

    fn on_token(
        &mut self,
        type_char: char,
        cflags: i32,
        min: i32,
        max: i32,
        flags: i32,
        base: u8,
        vflags: i32,
    ) {
        self.fmt_data.push(FormatData {
            type_char,
            cflags,
            base,
            min,
            max,
            flags,
            vflags,
        });
        self.components.push(b't');
    }
}

/// `LogStringInterpolator::streamToString`: render the record into `out`.
/// The printer appends as it walks, so on failure `out` keeps whatever was
/// rendered before the error; the return value reports the walk.
pub fn stream_to_string(
    interpolator: &LogStringInterpolator,
    stream: &mut MemoryIStream,
    version: u8,
    out: &mut String,
) -> bool {
    let mut printer = StringPrinter::default();
    let ok = interpolator.interpolate(stream, version, &mut printer);
    out.push_str(&printer.result);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::MemoryOStream;

    /// A handler recording every callback, for walk-shape assertions.
    #[derive(Default)]
    struct Recorder {
        pieces: Vec<String>,
        ints: Vec<i64>,
        floats: Vec<f64>,
        errored: bool,
        completed: bool,
    }

    impl InterpolationHandler for Recorder {
        fn on_fmt_string_section(&mut self, format: &str, start: usize, end: usize) {
            self.pieces.push(format[start..end].to_string());
        }

        fn on_int(&mut self, value: i64, _fd: &FormatData) {
            self.ints.push(value);
        }

        fn on_float(&mut self, value: f64, _fd: &FormatData) {
            self.floats.push(value);
        }

        fn on_error(&mut self) {
            self.errored = true;
        }

        fn on_parse_complete(&mut self) {
            self.completed = true;
        }
    }

    fn args(build: impl FnOnce(&mut MemoryOStream)) -> MemoryIStream<'static> {
        // Leak the buffer so the returned stream can borrow it for tests.
        let mut stream = MemoryOStream::new();
        build(&mut stream);
        let data: &'static [u8] = Box::leak(stream.data().to_vec().into_boxed_slice());
        MemoryIStream::new(data)
    }

    #[test]
    fn parsing_splits_literals_from_tokens() {
        let interpolator = LogStringInterpolator::new("x=%d end");
        assert!(interpolator.is_ok());
        assert_eq!(interpolator.components, vec![b's', b't', b's']);
        assert_eq!(interpolator.string_offsets, vec![(0, 2), (4, 8)]);
        assert_eq!(interpolator.fmt_data.len(), 1);
        assert_eq!(interpolator.format_string(), "x=%d end");
    }

    #[test]
    fn an_illegal_format_is_flagged() {
        let interpolator = LogStringInterpolator::new("%y");
        assert!(!interpolator.is_ok());
    }

    #[test]
    fn a_plain_string_needs_no_arguments() {
        let interpolator = LogStringInterpolator::new("hello world");
        let mut stream = args(|_| {});
        let mut out = String::new();
        assert!(stream_to_string(&interpolator, &mut stream, 10, &mut out));
        assert_eq!(out, "hello world");
    }

    #[test]
    fn conversions_render_through_the_bsd_helpers() {
        let interpolator = LogStringInterpolator::new("%d %u %x %s %c %p %f");
        let mut stream = args(|out| {
            out.write_u32((-5i32) as u32);
            out.write_u32(7);
            out.write_u32(255);
            out.write_string("mid");
            out.write_u8(b'Z');
            out.write_u64(0x1234);
            out.write_u64(1.5f64.to_bits());
        });
        let mut out = String::new();
        assert!(stream_to_string(&interpolator, &mut stream, 10, &mut out));
        assert_eq!(out, "-5 7 ff mid Z 0x1234 1.500000");
    }

    #[test]
    fn length_modifiers_change_the_argument_widths() {
        let interpolator = LogStringInterpolator::new("%hd %ld %lld");
        let mut stream = args(|out| {
            out.write_u16(300);
            out.write_u64(1 << 40);
            out.write_u64(u64::MAX);
        });
        let mut recorder = Recorder::default();
        assert!(interpolator.interpolate(&mut stream, 10, &mut recorder));
        assert_eq!(recorder.ints, vec![300, 1 << 40, -1]);
    }

    #[test]
    fn pre_version_seven_longs_travel_as_32_bits() {
        let interpolator = LogStringInterpolator::new("%ld %lu");
        let mut stream = args(|out| {
            out.write_u32((-7i32) as u32);
            out.write_u32(9);
        });
        let mut recorder = Recorder::default();
        assert!(interpolator.interpolate(&mut stream, 6, &mut recorder));
        assert_eq!(recorder.ints, vec![-7, 9]);
    }

    #[test]
    fn variable_widths_are_read_before_the_value() {
        let interpolator = LogStringInterpolator::new("%*d|%-.*s|");
        let mut stream = args(|out| {
            out.write_u16(6); // min width for %*d
            out.write_u32(42);
            out.write_u16(3); // max width for %.*s
            out.write_string("abcdef");
        });
        let mut out = String::new();
        assert!(stream_to_string(&interpolator, &mut stream, 10, &mut out));
        assert_eq!(out, "    42|abc|");
    }

    #[test]
    fn a_short_stream_errors_halfway_through() {
        let interpolator = LogStringInterpolator::new("%s and %d");
        let mut stream = args(|out| {
            out.write_string("one");
            // Missing the trailing %d payload.
        });
        let mut recorder = Recorder::default();
        assert!(!interpolator.interpolate(&mut stream, 10, &mut recorder));
        assert!(recorder.errored);
        assert!(!recorder.completed);
        // The literal before the failure still rendered (on a fresh stream).
        let mut stream = args(|out| {
            out.write_string("one");
        });
        let mut out = String::new();
        assert!(!stream_to_string(&interpolator, &mut stream, 10, &mut out));
        assert_eq!(out, "one and ");
    }

    #[test]
    fn on_parse_complete_fires_only_on_success() {
        let interpolator = LogStringInterpolator::new("%d");
        let mut stream = args(|out| {
            out.write_u32(11);
        });
        let mut recorder = Recorder::default();
        assert!(interpolator.interpolate(&mut stream, 10, &mut recorder));
        assert!(recorder.completed);
        assert_eq!(recorder.ints, vec![11]);
    }

    /// A handler keeping only the walk status, so the trait's default
    /// callback bodies run.
    #[derive(Default)]
    struct Bare {
        done: bool,
        errored: bool,
    }

    impl InterpolationHandler for Bare {
        fn on_parse_complete(&mut self) {
            self.done = true;
        }

        fn on_error(&mut self) {
            self.errored = true;
        }
    }

    #[test]
    fn a_bare_handler_exercises_the_default_callbacks() {
        let interpolator =
            LogStringInterpolator::new("w=%*d p=%.*s s=%s c=%c x=%p f=%Lf h=%hu q=%llx");
        let mut stream = args(|out| {
            out.write_u16(4); // %*d width
            out.write_u32(7);
            out.write_u16(2); // %.*s precision
            out.write_string("hello");
            out.write_string("s");
            out.write_u8(b'c');
            out.write_u64(0xab);
            out.write_u64(2.5f64.to_bits()); // %Lf travels as double bits
            out.write_u16(9); // %hu
            out.write_u64(0xfeed); // %llx
        });
        let mut bare = Bare::default();
        assert!(interpolator.interpolate(&mut stream, 10, &mut bare));
        assert!(bare.done);
        assert!(!bare.errored);
    }

    #[test]
    fn unsigned_longs_read_their_version_dependent_widths() {
        let interpolator = LogStringInterpolator::new("%lu %lu");

        // v10: longs are eight bytes.
        let mut stream = args(|out| {
            out.write_u64(u64::MAX);
            out.write_u64(5);
        });
        let mut recorder = Recorder::default();
        assert!(interpolator.interpolate(&mut stream, 10, &mut recorder));
        assert_eq!(recorder.ints, vec![-1, 5]);

        // v6: longs are four bytes.
        let mut stream = args(|out| {
            out.write_u32(9);
            out.write_u32(4);
        });
        let mut recorder = Recorder::default();
        assert!(interpolator.interpolate(&mut stream, 6, &mut recorder));
        assert_eq!(recorder.ints, vec![9, 4]);
    }

    #[test]
    fn a_recording_handler_sees_the_float_value() {
        let interpolator = LogStringInterpolator::new("%f %g");
        let mut stream = args(|out| {
            out.write_u64(2.5f64.to_bits());
            out.write_u64(0.75f64.to_bits());
        });
        let mut recorder = Recorder::default();
        assert!(interpolator.interpolate(&mut stream, 10, &mut recorder));
        assert_eq!(recorder.floats, vec![2.5, 0.75]);
    }

    #[test]
    fn the_bare_handler_reports_errors_too() {
        let interpolator = LogStringInterpolator::new("%d");
        let mut stream = args(|_| {});
        let mut bare = Bare::default();
        assert!(!interpolator.interpolate(&mut stream, 10, &mut bare));
        assert!(bare.errored);
        assert!(!bare.done);
    }

    #[test]
    fn the_string_printer_reports_its_running_output() {
        let mut printer = StringPrinter::default();
        let fd = FormatData {
            type_char: 's',
            cflags: 0,
            base: 0,
            min: 0,
            max: -1,
            flags: 0,
            vflags: 0,
        };
        printer.render_string("partial", &fd);
        assert_eq!(printer.result(), "partial");
    }
}
