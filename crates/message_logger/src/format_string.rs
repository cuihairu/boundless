//! The printf format-string scanner behind `handleFormatString`
//! (`lib/network/bsd_snprintf.cpp:139-340`, from OpenSSH's bsd-snprintf).
//! It walks the states `DEFAULT → FLAGS → MIN → DOT → MAX → MOD → CONV` and
//! reports literal segments through `on_string` and each conversion through
//! `on_token`.

/// Conversion-length modifiers (`bsd_snprintf.h:19-22`).
pub mod conv_flags {
    /// `%hd`/`%h` (and `%hh`).
    pub const DP_C_SHORT: i32 = 1;
    /// `%ld`/`%lz`/`%lt` — all map to `long`, sent as eight bytes since
    /// protocol version 7.
    pub const DP_C_LONG: i32 = 2;
    /// `%Lf`.
    pub const DP_C_LDOUBLE: i32 = 3;
    /// `%lld`.
    pub const DP_C_LLONG: i32 = 4;
}

/// Variable-width markers (`bsd_snprintf.h:25-26`).
pub mod variable_flags {
    /// `%-*d`: the width arrives as an argument before the value.
    pub const VARIABLE_MIN_WIDTH: i32 = 1;
    /// `%.*s`: the precision arrives as an argument before the value.
    pub const VARIABLE_MAX_WIDTH: i32 = 2;
}

/// Format flags (`bsd_snprintf.cpp:53-60`).
pub mod format_flags {
    /// `-`
    pub const DP_F_MINUS: i32 = 1 << 0;
    /// `+`
    pub const DP_F_PLUS: i32 = 1 << 1;
    /// ` `
    pub const DP_F_SPACE: i32 = 1 << 2;
    /// `#`
    pub const DP_F_NUM: i32 = 1 << 3;
    /// `0`
    pub const DP_F_ZERO: i32 = 1 << 4;
    /// uppercase `X`/`E`/`G`
    pub const DP_F_UP: i32 = 1 << 5;
    /// set for `o`/`u`/`x`
    pub const DP_F_UNSIGNED: i32 = 1 << 6;
}

/// The callbacks `handleFormatString` reports through
/// (`format_string_handler.hpp:14-29`).
pub trait FormatStringHandler {
    /// A literal segment `[start, end)` of the format string.
    fn on_string(&mut self, _start: usize, _end: usize) {}

    /// A conversion token, fully parsed. `base` is 10 for `d`/`u`, 8 for
    /// `o`, 16 for `x`/`p`, and 0 for the rest. The signature mirrors the
    /// reference's `FormatStringHandler::onToken`.
    #[allow(clippy::too_many_arguments)]
    fn on_token(
        &mut self,
        type_char: char,
        cflags: i32,
        min: i32,
        max: i32,
        flags: i32,
        base: u8,
        vflags: i32,
    );
}

// Format read states (`bsd_snprintf.cpp:45-52`).
const DP_S_DEFAULT: i32 = 0;
const DP_S_FLAGS: i32 = 1;
const DP_S_MIN: i32 = 2;
const DP_S_DOT: i32 = 3;
const DP_S_MAX: i32 = 4;
const DP_S_MOD: i32 = 5;
const DP_S_CONV: i32 = 6;
const DP_S_DONE: i32 = 7;

/// Scan `format`, reporting segments and tokens to `handler`. Returns false
/// on an illegal conversion character — the caller (`ForwardingStringHandler`)
/// then refuses to forward records using this format string
/// (`bsd_snprintf.cpp:139-340`).
pub fn handle_format_string(format: &str, handler: &mut dyn FormatStringHandler) -> bool {
    let bytes = format.as_bytes();
    let mut pos = 0usize;

    let mut state = DP_S_DEFAULT;
    let mut flags = 0;
    let mut cflags = 0;
    let mut vflags = 0;
    let mut min = 0;
    let mut max = -1;

    let mut ch = next_char(bytes, &mut pos);

    let mut strstart = 0usize;
    let mut strend;

    while state != DP_S_DONE {
        if ch == '\0' {
            state = DP_S_DONE;
        }

        match state {
            DP_S_DEFAULT => {
                if ch == '%' {
                    strend = pos - 1;
                    if strstart != strend {
                        handler.on_string(strstart, strend);
                    }
                    state = DP_S_FLAGS;
                }
                ch = next_char(bytes, &mut pos);
            }
            DP_S_FLAGS => match ch {
                '-' => {
                    flags |= format_flags::DP_F_MINUS;
                    ch = next_char(bytes, &mut pos);
                }
                '+' => {
                    flags |= format_flags::DP_F_PLUS;
                    ch = next_char(bytes, &mut pos);
                }
                ' ' => {
                    flags |= format_flags::DP_F_SPACE;
                    ch = next_char(bytes, &mut pos);
                }
                '#' => {
                    flags |= format_flags::DP_F_NUM;
                    ch = next_char(bytes, &mut pos);
                }
                '0' => {
                    flags |= format_flags::DP_F_ZERO;
                    ch = next_char(bytes, &mut pos);
                }
                _ => {
                    state = DP_S_MIN;
                }
            },
            DP_S_MIN => {
                if ch.is_ascii_digit() {
                    min = 10 * min + (ch as i32 - '0' as i32);
                    ch = next_char(bytes, &mut pos);
                } else if ch == '*' {
                    min = 0;
                    vflags |= variable_flags::VARIABLE_MIN_WIDTH;
                    ch = next_char(bytes, &mut pos);
                    state = DP_S_DOT;
                } else {
                    state = DP_S_DOT;
                }
            }
            DP_S_DOT => {
                if ch == '.' {
                    state = DP_S_MAX;
                    ch = next_char(bytes, &mut pos);
                } else {
                    state = DP_S_MOD;
                }
            }
            DP_S_MAX => {
                if ch.is_ascii_digit() {
                    if max < 0 {
                        max = 0;
                    }
                    max = 10 * max + (ch as i32 - '0' as i32);
                    ch = next_char(bytes, &mut pos);
                } else if ch == '*' {
                    max = -1;
                    vflags |= variable_flags::VARIABLE_MAX_WIDTH;
                    ch = next_char(bytes, &mut pos);
                    state = DP_S_MOD;
                } else {
                    state = DP_S_MOD;
                }
            }
            DP_S_MOD => {
                match ch {
                    'h' => {
                        cflags = conv_flags::DP_C_SHORT;
                        ch = next_char(bytes, &mut pos);
                        if ch == 'h' {
                            // A char-sized value promotes safely into the
                            // short handling.
                            ch = next_char(bytes, &mut pos);
                        }
                    }
                    'l' => {
                        cflags = conv_flags::DP_C_LONG;
                        ch = next_char(bytes, &mut pos);
                        if ch == 'l' {
                            cflags = conv_flags::DP_C_LLONG;
                            ch = next_char(bytes, &mut pos);
                        }
                    }
                    'z' | 't' => {
                        // size_t and ptrdiff_t both travel as eight bytes
                        // since protocol version 7.
                        cflags = conv_flags::DP_C_LONG;
                        ch = next_char(bytes, &mut pos);
                    }
                    'L' => {
                        cflags = conv_flags::DP_C_LDOUBLE;
                        ch = next_char(bytes, &mut pos);
                    }
                    _ => {}
                }
                state = DP_S_CONV;
            }
            DP_S_CONV => {
                match ch {
                    'd' | 'i' => handler.on_token('d', cflags, min, max, flags, 10, vflags),
                    'o' => {
                        flags |= format_flags::DP_F_UNSIGNED;
                        handler.on_token('o', cflags, min, max, flags, 8, vflags);
                    }
                    'u' => {
                        flags |= format_flags::DP_F_UNSIGNED;
                        handler.on_token('u', cflags, min, max, flags, 10, vflags);
                    }
                    'X' => {
                        flags |= format_flags::DP_F_UP | format_flags::DP_F_UNSIGNED;
                        handler.on_token('x', cflags, min, max, flags, 16, vflags);
                    }
                    'x' => {
                        flags |= format_flags::DP_F_UNSIGNED;
                        handler.on_token('x', cflags, min, max, flags, 16, vflags);
                    }
                    'f' => handler.on_token('f', cflags, min, max, flags, 0, vflags),
                    'E' => {
                        flags |= format_flags::DP_F_UP;
                        handler.on_token('e', cflags, min, max, flags, 0, vflags);
                    }
                    'e' => handler.on_token('e', cflags, min, max, flags, 0, vflags),
                    'G' => {
                        flags |= format_flags::DP_F_UP;
                        handler.on_token('g', cflags, min, max, flags, 0, vflags);
                    }
                    'g' => handler.on_token('g', cflags, min, max, flags, 0, vflags),
                    'c' => handler.on_token('c', cflags, min, max, flags, 0, vflags),
                    's' => handler.on_token('s', cflags, min, max, flags, 0, vflags),
                    'p' => handler.on_token('p', cflags, min, max, flags, 16, vflags),
                    // A literal percent: the segment holding it is reported
                    // through on_string by the reference's index arithmetic.
                    '%' => {
                        handler.on_string(pos - 1, pos);
                    }
                    // Not supported yet: skipped like one more character
                    // (the position advances twice, as the C walk does).
                    'w' => {
                        next_char(bytes, &mut pos);
                    }
                    _ => {
                        // Unknown conversion: the whole format is invalid.
                        return false;
                    }
                }
                ch = next_char(bytes, &mut pos);
                state = DP_S_DEFAULT;
                flags = 0;
                cflags = 0;
                vflags = 0;
                min = 0;
                max = -1;
                strstart = pos - 1;
            }
            _ => {}
        }
    }

    strend = pos - 1;
    if strstart != strend {
        handler.on_string(strstart, strend);
    }

    true
}

/// Advance to the next character, returning NUL past the end — the position
/// still advances past it, exactly like the C `*format++` walk, because the
/// literal-segment bounds are computed off that pointer.
fn next_char(bytes: &[u8], pos: &mut usize) -> char {
    if *pos < bytes.len() {
        let out = bytes[*pos] as char;
        *pos += 1;
        out
    } else {
        *pos += 1;
        '\0'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every callback the scanner makes.
    #[derive(Default)]
    struct Recorder {
        strings: Vec<(usize, usize)>,
        tokens: Vec<(char, i32, i32, i32, i32, u8, i32)>,
        ok: bool,
    }

    impl FormatStringHandler for Recorder {
        fn on_string(&mut self, start: usize, end: usize) {
            self.strings.push((start, end));
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
            self.tokens
                .push((type_char, cflags, min, max, flags, base, vflags));
        }
    }

    fn scan(format: &str) -> Recorder {
        let mut recorder = Recorder::default();
        recorder.ok = handle_format_string(format, &mut recorder);
        recorder
    }

    #[test]
    fn a_plain_string_makes_no_tokens() {
        let recorder = scan("no conversions here");
        assert!(recorder.ok);
        assert!(recorder.tokens.is_empty());
        assert_eq!(recorder.strings, vec![(0, 19)]);
    }

    #[test]
    fn an_empty_format_is_accepted_with_no_callbacks() {
        let recorder = scan("");
        assert!(recorder.ok);
        assert!(recorder.tokens.is_empty());
        assert!(recorder.strings.is_empty());
    }

    #[test]
    fn simple_conversions_report_type_and_base() {
        let recorder = scan("%d %u %x %o %f %s %c %p %e %g");
        assert!(recorder.ok);
        let types: Vec<char> = recorder.tokens.iter().map(|t| t.0).collect();
        assert_eq!(
            types,
            vec!['d', 'u', 'x', 'o', 'f', 's', 'c', 'p', 'e', 'g']
        );
        let bases: Vec<u8> = recorder.tokens.iter().map(|t| t.5).collect();
        assert_eq!(bases, vec![10, 10, 16, 8, 0, 0, 0, 16, 0, 0]);
        // A one-space literal between each pair of conversions.
        assert_eq!(
            recorder.strings,
            vec![
                (2, 3),
                (5, 6),
                (8, 9),
                (11, 12),
                (14, 15),
                (17, 18),
                (20, 21),
                (23, 24),
                (26, 27)
            ]
        );
    }

    #[test]
    fn uppercase_conversions_normalise_but_flag_up() {
        let recorder = scan("%X %E %G");
        assert!(recorder.ok);
        let types: Vec<char> = recorder.tokens.iter().map(|t| t.0).collect();
        assert_eq!(types, vec!['x', 'e', 'g']);
        let flags: Vec<i32> = recorder.tokens.iter().map(|t| t.4).collect();
        assert_eq!(
            flags,
            vec![
                format_flags::DP_F_UP | format_flags::DP_F_UNSIGNED,
                format_flags::DP_F_UP,
                format_flags::DP_F_UP
            ]
        );
    }

    #[test]
    fn length_modifiers_set_the_conv_flags() {
        let recorder = scan("%hd %hhd %ld %lld %Lf");
        assert!(recorder.ok);
        let cflags: Vec<i32> = recorder.tokens.iter().map(|t| t.1).collect();
        assert_eq!(
            cflags,
            vec![
                conv_flags::DP_C_SHORT,
                conv_flags::DP_C_SHORT, // %hh promotes into short
                conv_flags::DP_C_LONG,
                conv_flags::DP_C_LLONG,
                conv_flags::DP_C_LDOUBLE
            ]
        );
        // But `%lz` and `%Lt` are illegal here: the `l`/`L` modifier
        // consumes one more character, leaving `z`/`t` to reach the
        // conversion state, where they are not conversion characters.
        assert!(!scan("%lz").ok);
        assert!(!scan("%Lt").ok);
        // %i reads as %d.
        let recorder = scan("%i");
        assert_eq!(recorder.tokens[0].0, 'd');
    }

    #[test]
    fn widths_and_precisions_travel_in_min_and_max() {
        let recorder = scan("%10d %-+5.3s %#08.2f");
        assert!(recorder.ok);
        assert_eq!(recorder.tokens[0].2, 10); // min
        assert_eq!(recorder.tokens[0].3, -1); // max unset
        assert_eq!(recorder.tokens[1].2, 5);
        assert_eq!(recorder.tokens[1].3, 3);
        assert_eq!(
            recorder.tokens[1].4,
            format_flags::DP_F_MINUS | format_flags::DP_F_PLUS
        );
        assert_eq!(recorder.tokens[2].2, 8);
        assert_eq!(recorder.tokens[2].3, 2);
        assert_eq!(
            recorder.tokens[2].4,
            format_flags::DP_F_NUM | format_flags::DP_F_ZERO
        );
    }

    #[test]
    fn star_widths_flag_the_variable_arguments() {
        let recorder = scan("%*d %.*s %*.*d");
        assert!(recorder.ok);
        assert_eq!(recorder.tokens[0].6, variable_flags::VARIABLE_MIN_WIDTH);
        assert_eq!(recorder.tokens[1].6, variable_flags::VARIABLE_MAX_WIDTH);
        assert_eq!(
            recorder.tokens[2].6,
            variable_flags::VARIABLE_MIN_WIDTH | variable_flags::VARIABLE_MAX_WIDTH
        );
    }

    #[test]
    fn a_doubled_percent_is_a_literal() {
        let recorder = scan("100%% done");
        assert!(recorder.ok);
        assert!(recorder.tokens.is_empty());
        // The reference reports the literal segment around the escaped
        // percent through on_string.
        assert_eq!(recorder.strings, vec![(0, 3), (4, 5), (5, 10)]);
    }

    #[test]
    fn an_illegal_conversion_rejects_the_format() {
        assert!(!scan("%y").ok);
        assert!(!scan("%q").ok);
        // 'w' is skipped as the next character rather than rejected.
        assert!(scan("%wd").ok);
    }

    #[test]
    fn the_space_flag_and_z_t_modifiers_are_recognised() {
        // The ` ` flag.
        let recorder = scan("% d");
        assert_eq!(
            recorder.tokens[0].4,
            format_flags::DP_F_SPACE,
            "flags reset between conversions"
        );

        // `%zd`/`%td` both travel as long.
        for format in ["%zd", "%td"] {
            let recorder = scan(format);
            assert!(recorder.ok);
            assert_eq!(recorder.tokens[0].1, conv_flags::DP_C_LONG);
            assert_eq!(recorder.tokens[0].0, 'd');
        }
    }

    #[test]
    fn flags_before_widths_accumulate() {
        let recorder = scan("%-10s");
        assert_eq!(
            recorder.tokens[0].4,
            format_flags::DP_F_MINUS,
            "flags reset between conversions"
        );
        let recorder = scan("%-0s");
        assert_eq!(
            recorder.tokens[0].4,
            format_flags::DP_F_MINUS | format_flags::DP_F_ZERO
        );
    }
}
