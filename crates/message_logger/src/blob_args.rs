//! `LogStringWriter` (`log_string_writer.hpp/.cpp`) — the ingest-time
//! handler that re-encodes each conversion token from the wire argument
//! stream into the args file, driven by
//! `LogStringInterpolator::interpolate` (`log_string_interpolator.cpp:173`).
//!
//! `LogStringWriter::onInt<T>` writes `sizeof(T)` bytes, `onFloat<double>`
//! 8, `onChar` 1, `onPointer` 8 (`int64`), `onString` the packed length
//! plus the bytes, and the variable widths `WidthType` (`short`,
//! `bsd_snprintf.h:27`) as two bytes. A record whose argument stream runs
//! out mid-way still leaves the bytes it did write in the args file — the
//! reference does not roll the file back on a failed record, so the args
//! file carries the partial bytes while `addEntry` drops the entry itself.

use crate::format_data::FormatStringRecord;
use crate::format_string::variable_flags;
use crate::stream::{MemoryIStream, MemoryOStream};

/// The protocol version from which `long` conversions carry 64 bits
/// (`MESSAGE_LOGGER_VERSION_64BIT`, `log_string_interpolator.cpp:22`).
const MESSAGE_LOGGER_VERSION_64BIT: u8 = 7;

/// `DP_C_LDOUBLE` storage on the wire: `sizeof(long double)` on x86-64.
const LONG_DOUBLE_SIZE: usize = 16;

/// Re-encode the wire arguments of one format record into args-file bytes
/// (`LogStringInterpolator::streamToLog` + `LogStringWriter`).
///
/// Returns the bytes written — including any partial bytes emitted before
/// the argument stream ran out — and whether every token was consumed.
pub fn stream_to_log(
    record: &FormatStringRecord,
    wire: &[u8],
    version: u8,
) -> (MemoryOStream, bool) {
    let mut stream = MemoryIStream::new(wire);
    let mut out = MemoryOStream::new();
    let mut fmt = record.fmt_data.iter();

    // The reference walks the components with a `CHECK_STREAM` macro that
    // bails out with `onError`; partial writes stay in the file.
    macro_rules! check_stream {
        () => {
            if stream.error() {
                return (out, false);
            }
        };
    }

    for token in record.components.bytes() {
        if token == b's' {
            continue;
        }
        let Some(fd) = fmt.next() else {
            return (out, false);
        };
        if fd.vflags as i32 & variable_flags::VARIABLE_MIN_WIDTH != 0 {
            let width = stream.read_u16();
            check_stream!();
            if let Some(w) = width {
                out.write_u16(w);
            }
        }
        if fd.vflags as i32 & variable_flags::VARIABLE_MAX_WIDTH != 0 {
            let width = stream.read_u16();
            check_stream!();
            if let Some(w) = width {
                out.write_u16(w);
            }
        }
        match fd.type_char {
            b'd' => match fd.cflags as i32 {
                crate::format_string::conv_flags::DP_C_SHORT => {
                    let value = stream.read_u16();
                    check_stream!();
                    out.write_u16(value.unwrap_or(0));
                }
                crate::format_string::conv_flags::DP_C_LONG => {
                    let value = if version < MESSAGE_LOGGER_VERSION_64BIT {
                        stream.read_u32().map(|v| v as i32 as i64)
                    } else {
                        stream.read_u64().map(|v| v as i64)
                    };
                    check_stream!();
                    // Old 32-bit `long` values widen to the 8-byte storage
                    // the current format writes.
                    if let Some(v) = value {
                        out.write_u64(v as u64);
                    }
                }
                crate::format_string::conv_flags::DP_C_LLONG => {
                    let value = stream.read_u64();
                    check_stream!();
                    out.write_u64(value.unwrap_or(0));
                }
                _ => {
                    let value = stream.read_u32();
                    check_stream!();
                    out.write_u32(value.unwrap_or(0));
                }
            },
            b'o' | b'u' | b'x' => match fd.cflags as i32 {
                crate::format_string::conv_flags::DP_C_SHORT => {
                    let value = stream.read_u16();
                    check_stream!();
                    out.write_u16(value.unwrap_or(0));
                }
                crate::format_string::conv_flags::DP_C_LONG => {
                    let value = if version < MESSAGE_LOGGER_VERSION_64BIT {
                        stream.read_u32().map(|v| v as u32 as u64)
                    } else {
                        stream.read_u64()
                    };
                    check_stream!();
                    if let Some(v) = value {
                        out.write_u64(v);
                    }
                }
                crate::format_string::conv_flags::DP_C_LLONG => {
                    let value = stream.read_u64();
                    check_stream!();
                    out.write_u64(value.unwrap_or(0));
                }
                _ => {
                    let value = stream.read_u32();
                    check_stream!();
                    out.write_u32(value.unwrap_or(0));
                }
            },
            b'f' | b'e' | b'g' => match fd.cflags as i32 {
                crate::format_string::conv_flags::DP_C_LDOUBLE => {
                    let value = stream.retrieve(LONG_DOUBLE_SIZE).map(|b| b.to_vec());
                    check_stream!();
                    if let Some(v) = value {
                        out.add_blob(&v);
                    }
                }
                _ => {
                    let value = stream.read_u64();
                    check_stream!();
                    out.write_u64(value.unwrap_or(0));
                }
            },
            b's' => {
                let value = stream.read_string_bytes();
                check_stream!();
                out.write_string_length(value.len() as i32);
                out.add_blob(&value);
            }
            b'p' => {
                let value = stream.read_u64();
                check_stream!();
                out.write_u64(value.unwrap_or(0));
            }
            b'c' => {
                let value = stream.read_u8();
                check_stream!();
                out.write_u8(value.unwrap_or(0));
            }
            _ => {
                // An unhandled conversion reports `onError`.
                return (out, false);
            }
        }
    }

    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::MemoryOStream;

    /// The reference-written args file for the first sample
    /// (`/tmp/bwlog_sample/.../args.2026-11-23-21:11:58`, 34 bytes).
    const SAMPLE1_ARGS: [u8; 34] = [
        0x21, 0x00, 0x00, 0x00, 0x05, 0x73, 0x70, 0x61, 0x77, 0x6e, 0x6e, 0x86, 0x1b, 0xf0, 0xf9,
        0x21, 0x09, 0x40, 0x03, 0x6d, 0x69, 0x78, 0xf9, 0xff, 0xff, 0xff, 0x00, 0x28, 0x6b, 0xee,
        0xff, 0x00, 0x00, 0x00,
    ];

    /// The reference-written args file for the flag-heavy sample
    /// (`/tmp/bwlog_sample2/.../args.2026-11-23-21:28:23`, 75 bytes): the
    /// first four records again, then one record per format flag, with the
    /// two failed variable-width records leaving their partial bytes in
    /// the middle.
    const SAMPLE2_ARGS: [u8; 75] = [
        0x21, 0x00, 0x00, 0x00, 0x05, 0x73, 0x70, 0x61, 0x77, 0x6e, 0x6e, 0x86, 0x1b, 0xf0, 0xf9,
        0x21, 0x09, 0x40, 0x03, 0x6d, 0x69, 0x78, 0xf9, 0xff, 0xff, 0xff, 0x00, 0x28, 0x6b, 0xee,
        0xff, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x02, 0x61, 0x62, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xf8, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf8, 0x3f, 0xff, 0x00, 0x00,
        0x00, 0x08, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x03, 0x00,
    ];

    /// The wire arguments the sample component sent per format — encoded by
    /// the forwarder's `ForwardingStringHandler` (`int32`/`uint32` for the
    /// integer conversions, packed strings, `double` for floats).
    #[allow(clippy::approx_constant)] // the sample sent the truncated literal
    fn sample_wires() -> Vec<(String, Vec<u8>)> {
        fn wire(format: &str, build: impl FnOnce(&mut MemoryOStream)) -> (String, Vec<u8>) {
            let mut os = MemoryOStream::new();
            build(&mut os);
            (format.to_string(), os.data().to_vec())
        }
        vec![
            wire("damage %d", |os| {
                os.write_u32(33);
            }),
            wire("moved to %s", |os| {
                os.write_string("spawn");
            }),
            wire("pi is %f", |os| {
                os.write_u64(3.14159f64.to_bits());
            }),
            wire("%s did %d damage (%u raw, %x hex)", |os| {
                os.write_string("mix");
                os.write_u32(-7i32 as u32);
                os.write_u32(4_000_000_000);
                os.write_u32(255);
            }),
            wire("%5d", |os| {
                os.write_u32(9);
            }),
            wire("%-5s|", |os| {
                os.write_string("ab");
            }),
            wire("%+.2f", |os| {
                os.write_u64(1.5f64.to_bits());
            }),
            wire("%08.3f", |os| {
                os.write_u64(1.5f64.to_bits());
            }),
            wire("%#x", |os| {
                os.write_u32(255);
            }),
            wire("%o", |os| {
                os.write_u32(8);
            }),
            wire("%X", |os| {
                os.write_u32(255);
            }),
            // The two variable-width messages the sample sent arguments the
            // reference could not consume: `%*d` got a bare `int32` where a
            // `WidthType` + `int32` pair was needed, and `%.*f` a bare
            // `double` where a `WidthType` + `double` pair was needed.
            wire("%*d", |os| {
                os.write_u32(3);
            }),
            wire("%.*f", |os| {
                os.write_u64(1.5f64.to_bits());
            }),
            wire("%hd", |os| {
                os.write_u32(3);
            }),
        ]
    }

    #[test]
    fn reencodes_the_first_sample_args_file_byte_for_byte() {
        let mut file = Vec::new();
        for (format, wire) in sample_wires().into_iter().take(4) {
            let record = FormatStringRecord::parse(&format).unwrap();
            let (out, ok) = stream_to_log(&record, &wire, 10);
            assert!(ok, "{format} should have been consumed");
            file.extend_from_slice(out.data());
        }
        assert_eq!(file, SAMPLE1_ARGS);
    }

    #[test]
    fn reencodes_the_flag_sample_including_partial_failure_bytes() {
        let mut file = Vec::new();
        let mut ok_flags = Vec::new();
        for (format, wire) in sample_wires() {
            let record = FormatStringRecord::parse(&format).unwrap();
            let (out, ok) = stream_to_log(&record, &wire, 10);
            ok_flags.push(ok);
            // The partial bytes of the failed records stay in the file.
            file.extend_from_slice(out.data());
        }
        assert_eq!(
            ok_flags,
            [
                true, true, true, true, true, true, true, true, true, true, true, // the
                // eleven consumable records
                false, false, // %*d and %.*f ran out of arguments
                true
            ]
        );
        assert_eq!(file, SAMPLE2_ARGS);
    }

    #[test]
    fn short_conversions_narrow_the_wire_argument() {
        // `%hd` arrives as a four-byte `int32` on the wire and is stored as
        // the two-byte `int16` the conversion names.
        let record = FormatStringRecord::parse("%hd").unwrap();
        let (out, ok) = stream_to_log(&record, &(-7i32 as u32).to_le_bytes(), 10);
        assert!(ok);
        assert_eq!(out.data(), [0xf9, 0xff]);
    }

    #[test]
    fn long_conversions_widen_old_versions() {
        // Before the 64-bit wire, a `%ld` argument travels as an `int32`
        // but is stored in the eight-byte slot the current format uses.
        let record = FormatStringRecord::parse("%ld").unwrap();
        let (out, ok) = stream_to_log(&record, &[7, 0, 0, 0], 6);
        assert!(ok);
        assert_eq!(out.data(), &[7, 0, 0, 0, 0, 0, 0, 0]);

        let (out, ok) = stream_to_log(&record, &[7, 0, 0, 0, 0, 0, 0, 0], 10);
        assert!(ok);
        assert_eq!(out.data(), &[7, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn char_and_pointer_conversions_keep_their_widths() {
        let record = FormatStringRecord::parse("%c").unwrap();
        let (out, ok) = stream_to_log(&record, &[b'A'], 10);
        assert!(ok);
        assert_eq!(out.data(), [b'A']);

        let record = FormatStringRecord::parse("%p").unwrap();
        let (out, ok) = stream_to_log(&record, &[0x34, 0x12, 0, 0, 0, 0, 0, 0], 10);
        assert!(ok);
        assert_eq!(out.data(), &[0x34, 0x12, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn variable_widths_round_trip_their_two_byte_form() {
        // A properly-formed `%*d` wire argument carries the `WidthType`
        // (`short`) before the value.
        let record = FormatStringRecord::parse("%*d").unwrap();
        let mut wire = MemoryOStream::new();
        wire.write_u16(5).write_u32(42);
        let (out, ok) = stream_to_log(&record, wire.data(), 10);
        assert!(ok);
        assert_eq!(out.data(), &[5, 0, 42, 0, 0, 0]);
    }

    #[test]
    fn long_double_conversions_carry_their_wide_storage() {
        let record = FormatStringRecord::parse("%Lf").unwrap();
        let arg = [0u8; 16];
        let (out, ok) = stream_to_log(&record, &arg, 10);
        assert!(ok);
        assert_eq!(out.data(), &arg);
    }

    #[test]
    fn empty_strings_encode_as_a_lone_length_byte() {
        let record = FormatStringRecord::parse("%s").unwrap();
        let mut wire = MemoryOStream::new();
        wire.write_string("");
        let (out, ok) = stream_to_log(&record, wire.data(), 10);
        assert!(ok);
        assert_eq!(out.data(), [0]);
    }
}
