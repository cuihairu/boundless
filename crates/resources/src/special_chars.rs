//! XML escape sequences, an exact port of `resmgr/xml_special_chars.cpp`.
//!
//! The reference operates on NUL-terminated byte buffers, not UTF-8: bytes
//! >= 0x80 are escaped individually on write and character-code escapes
//! truncate to `i8` on read. Everything here is therefore byte-oriented.

/// Reduce escape sequences in place, ported from `XmlSpecialChars::reduce`.
///
/// Recognised: `&amp; &lt; &gt; &quot; &apos;`, decimal `&#65;` and hex
/// `&#x41;` character codes. Unknown or malformed sequences stay literal.
/// Character codes outside 1..=255 still replace (truncated through `i8`,
/// matching the C cast); a zero result truncates the value, because the
/// reference observes strings through `strlen`.
pub fn reduce(input: &str) -> String {
    let mut buf: Vec<u8> = input.bytes().collect();
    let mut len = buf.len();
    let mut i = 0usize;

    while i < len {
        if buf[i] != b'&' {
            i += 1;
            continue;
        }

        // Collect the escape body up to the terminator. `special_len`
        // counts the '&' plus every consumed byte including ';'.
        let mut special = [0u8; 32];
        let mut special_len = 1usize;
        let mut j = 0usize;
        let mut ii = i + 1;
        while ii < len && j < 31 {
            special_len += 1;
            if buf[ii] == b';' {
                break;
            }
            special[j] = buf[ii];
            j += 1;
            ii += 1;
        }

        // "Too short" (&; or &<EOL>) and "too long" (no ';' within the
        // scan window) leave the '&' literal and keep scanning.
        if special_len <= 2 || special_len >= 31 {
            i += 1;
            continue;
        }

        let body = &special[..j];
        let replace: u8 = match body {
            b"amp" => b'&',
            b"lt" => b'<',
            b"gt" => b'>',
            b"quot" => b'"',
            b"apos" => b'\'',
            [b'#', rest @ ..] => {
                let val: i32 = if rest.first() == Some(&b'x') {
                    parse_hex(&rest[1..]).unwrap_or(0)
                } else {
                    atoi(rest)
                };
                // Out-of-range codes warn in the reference but still
                // replace, truncated through a signed char.
                (val as i8) as u8
            }
            // Unknown named sequence: stays literal.
            _ => {
                i += 1;
                continue;
            }
        };

        // Replace the '&' and remove the rest of the sequence.
        buf[i] = replace;
        let width = special_len - 1;
        let mut c = i + 1;
        while c + width < len {
            buf[c] = buf[c + width];
            c += 1;
        }
        len -= width;

        // The reference reads values back through strlen: an inserted NUL
        // truncates everything after it.
        if replace == 0 {
            len = i;
            break;
        }

        // Forward single-pass scan: the replaced byte is never re-reduced.
        i += 1;
    }

    String::from_utf8(buf[..len].to_vec()).expect("reduce preserves ASCII-compatible bytes")
}

/// Expand special characters for output, ported from `XmlSpecialChars::expand`.
///
/// The five named characters plus any byte >= 0x80 (as unsigned) become
/// escape sequences; other bytes pass through.
pub fn expand(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'"' => out.extend_from_slice(b"&quot;"),
            b'\'' => out.extend_from_slice(b"&apos;"),
            0x80..=0xFF => {
                out.extend_from_slice(format!("&#{};", b as u16).as_bytes());
            }
            _ => out.push(b),
        }
    }
    String::from_utf8(out).expect("expand emits ASCII only")
}

/// `atoi` semantics: skip whitespace, optional sign, decimal digits,
/// saturating through i64.
fn atoi(bytes: &[u8]) -> i32 {
    strtol(bytes, 10) as i32
}

/// `strtol`-style prefix parse: leading C whitespace, optional sign, digits
/// in `base`, saturating at i64 bounds (ERANGE behaviour).
fn strtol(bytes: &[u8], base: u32) -> i64 {
    let mut i = 0usize;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
        i += 1;
    }
    let mut negative = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        negative = bytes[i] == b'-';
        i += 1;
    }
    let mut any = false;
    let mut acc: i64 = 0;
    while i < bytes.len() {
        let digit = match bytes[i] {
            b'0'..=b'9' => (bytes[i] - b'0') as u32,
            b'a'..=b'z' => (bytes[i] - b'a' + 10) as u32,
            b'A'..=b'Z' => (bytes[i] - b'A' + 10) as u32,
            _ => break,
        };
        if digit >= base {
            break;
        }
        any = true;
        acc = acc.saturating_mul(base as i64).saturating_add(digit as i64);
        i += 1;
    }
    if !any {
        return 0;
    }
    if negative { -acc } else { acc }
}

/// Hex prefix parse used by `&#x..;` bodies; no digits yields `None`
/// (the reference warning path, which leaves the code at 0).
fn parse_hex(bytes: &[u8]) -> Option<i32> {
    let mut any = false;
    let mut acc: u32 = 0;
    for &b in bytes {
        let digit = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => break,
        };
        any = true;
        acc = acc.wrapping_mul(16).wrapping_add(digit);
    }
    if any { Some(acc as i32) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_entities_reduce() {
        assert_eq!(reduce("&amp;"), "&");
        assert_eq!(reduce("&lt;&gt;"), "<>");
        assert_eq!(reduce("&quot;x&quot;"), "\"x\"");
        assert_eq!(reduce("&apos;"), "'");
        assert_eq!(reduce("a&amp;b&lt;c"), "a&b<c");
    }

    #[test]
    fn char_code_entities_reduce() {
        assert_eq!(reduce("&#65;"), "A");
        assert_eq!(reduce("&#x41;"), "A");
        // Only lower-case 'x' selects hex: "&#X41;" goes through the
        // decimal atoi("X41") = 0 path → NUL truncation.
        assert_eq!(reduce("&#X41;"), "");
        // atoi("0a") is the prefix-parse 0 → NUL replacement truncates.
        assert_eq!(reduce("&#0a;"), "");
    }

    #[test]
    fn malformed_sequences_stay_literal() {
        assert_eq!(reduce("&;"), "&;", "too short");
        assert_eq!(reduce("&z"), "&z", "unterminated short body → unknown name");
        assert_eq!(reduce("&foo;"), "&foo;", "unknown name");
        // 30+ chars without ';' exceeds the scan window.
        let long = format!("&{}", "a".repeat(31));
        assert_eq!(reduce(&long), long);
        // But a complete name at end-of-string DOES reduce — no ';'
        // required.
        assert_eq!(reduce("x&amp"), "x&");
    }

    #[test]
    fn out_of_range_codes_replace_with_truncation() {
        // 300 as i8 is 44 = ','.
        assert_eq!(reduce("&#300;"), ",");
        // Code 0 replaces with NUL, truncating the rest (strlen view).
        assert_eq!(reduce("a&#0;b"), "a");
    }

    #[test]
    fn char_code_uses_strtol_prefix_rules() {
        // Leading whitespace is skipped before the digits.
        assert_eq!(reduce("&# 5;"), "\u{5}");
        // A bare sign consumes then finds no digits: atoi → 0 → NUL cut.
        assert_eq!(reduce("&#-;"), "");
        // "-0" parses (any=true), negates to 0, and truncates via NUL.
        assert_eq!(reduce("&#-0;"), "");
        // A non-digit body never parses: `_ => break`, 0 → NUL cut.
        assert_eq!(reduce("&#.5;"), "");
    }

    #[test]
    fn hex_body_needs_at_least_one_hex_digit() {
        // parse_hex finds no digits → None → 0 → NUL truncation.
        assert_eq!(reduce("&#xg1;"), "");
        assert_eq!(reduce("&#x;"), "");
    }

    #[test]
    fn hex_accepts_both_letter_cases() {
        assert_eq!(reduce("&#x6a;"), "j", "lowercase a-f digits");
        assert_eq!(reduce("&#x4A;"), "J", "uppercase A-F digits");
    }

    #[test]
    fn reduce_is_single_pass() {
        // The '&' produced by reducing "&amp;" is not re-reduced.
        assert_eq!(reduce("&amp;amp;"), "&amp;");
    }

    #[test]
    fn expand_escapes_named_chars_and_high_bytes() {
        assert_eq!(expand("&<>\"'"), "&amp;&lt;&gt;&quot;&apos;");
        assert_eq!(expand("ok"), "ok");
        // Bytes >= 0x80 are escaped individually (UTF-8 bytes explode).
        assert_eq!(expand("é"), "&#195;&#169;");
        // U+0080 is two UTF-8 bytes (0xC2 0x80).
        assert_eq!(expand("\u{80}"), "&#194;&#128;");
    }

    #[test]
    fn expand_reduce_roundtrip_ascii() {
        for raw in ["plain", "a&b<c>d\"e'f", "tab\tkept", ""] {
            assert_eq!(reduce(&expand(raw)), raw);
        }
    }
}
