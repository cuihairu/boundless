//! Base64 codec, an exact port of `cstdmf/base64.cpp`.
//!
//! Two decode variants exist in the reference and they disagree on failure:
//! the buffer variant (`Base64::decode(data, results, bufSize)`) silently
//! maps unknown characters through the decode table (yielding `0xFF`
//! garbage) and never checks the length, while the string variant
//! (`Base64::decode(inData, outData)`) requires a length divisible by four
//! and rejects unknown characters. Both stop early at `=`.

const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const FILLCHAR: u8 = b'=';

/// Decode-table entry: index of the character in `BASE64_TABLE`, or `None`.
fn decode_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Standard encode: three bytes to four characters, `=` padded, no line
/// breaks (`Base64::encode`).
pub fn encode(data: &[u8]) -> String {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    let len = data.len();
    let mut i = 0usize;
    while i < len {
        out.push(BASE64_TABLE[((data[i] >> 2) & 0x3f) as usize]);
        let mut c = ((data[i] << 4) & 0x3f) as u32;
        i += 1;
        if i < len {
            c |= ((data[i] >> 4) & 0x0f) as u32;
        }
        out.push(BASE64_TABLE[c as usize]);
        if i < len {
            let mut c = ((data[i] << 2) & 0x3f) as u32;
            i += 1;
            if i < len {
                c |= ((data[i] >> 6) & 0x03) as u32;
            }
            out.push(BASE64_TABLE[c as usize]);
        } else {
            i += 1;
            out.push(FILLCHAR);
        }

        if i < len {
            out.push(BASE64_TABLE[(data[i] & 0x3f) as usize]);
        } else {
            out.push(FILLCHAR);
        }
        i += 1;
    }
    String::from_utf8(out).expect("base64 alphabet is ASCII")
}

/// String-variant decode (`Base64::decode(inData, outData)`): the length
/// must be divisible by four, unknown characters fail, and a `=` at the
/// third or fourth position of a quad ends the parse early (any trailing
/// bytes after it are ignored, matching the reference `break`).
pub fn decode_strict(data: &str) -> Option<Vec<u8>> {
    let bytes = data.as_bytes();
    let len = bytes.len();
    if !len.is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(len / 4 * 3);
    // The length check makes every chunk a full quad, so the reference's
    // per-index walk collapses to direct quad iteration.
    for quad in bytes.as_chunks::<4>().0 {
        let c0 = decode_value(quad[0])?;
        let c1 = decode_value(quad[1])?;
        out.push((c0 << 2) | ((c1 >> 4) & 0x3));

        if quad[2] == FILLCHAR {
            break;
        }
        let c2 = decode_value(quad[2])?;
        out.push(((c1 << 4) & 0xf0) | ((c2 >> 2) & 0xf));

        if quad[3] == FILLCHAR {
            break;
        }
        let c3 = decode_value(quad[3])?;
        out.push((c2 << 6) | c3);
    }
    Some(out)
}

/// Buffer-variant decode (`Base64::decode(data, results, bufSize)`): no
/// length or validity checks; unknown characters feed `0xFF` (the `(char)
/// npos` decode-table entry) into the bit shifts. A `=` stops the parse.
pub fn decode_lenient(data: &str) -> Vec<u8> {
    // The reference casts `npos` to char: 0xFF on every relevant platform.
    const BAD: u8 = 0xFF;
    let bytes = data.as_bytes();
    let len = bytes.len();
    let mut out = Vec::with_capacity(len);
    let mut i = 0usize;
    while i < len {
        let c = decode_value(bytes[i]).unwrap_or(BAD);
        i += 1;
        // The buffer variant reads data[i] unconditionally; with a
        // NUL-terminated input this is the terminator byte. Rust strings
        // cannot contain NUL here (DataSection values are NUL-free), so the
        // index is guarded instead.
        let c1 = if i < len {
            decode_value(bytes[i]).unwrap_or(BAD)
        } else {
            BAD
        };
        out.push((c << 2) | ((c1 >> 4) & 0x3));
        let mut c2 = 0u8;
        i += 1;
        if i < len {
            if bytes[i] == FILLCHAR {
                break;
            }
            c2 = decode_value(bytes[i]).unwrap_or(BAD);
            out.push(((c1 << 4) & 0xf0) | ((c2 >> 2) & 0xf));
        }
        i += 1;
        if i < len {
            if bytes[i] == FILLCHAR {
                break;
            }
            let c3 = decode_value(bytes[i]).unwrap_or(BAD);
            out.push((c2 << 6) | c3);
        }
        // The reference's for-loop increment lands on the next quad.
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_standard_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn strict_decode_roundtrips_encode() {
        for raw in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "a&b<c>"] {
            assert_eq!(
                decode_strict(&encode(raw.as_bytes())).unwrap(),
                raw.as_bytes()
            );
        }
    }

    #[test]
    fn strict_decode_rejects_bad_lengths_and_chars() {
        assert!(decode_strict("Zm9vYg").is_none(), "length % 4 != 0");
        assert!(decode_strict("Zm9vYg**").is_none(), "unknown character");
        assert!(decode_strict("Z=").is_none(), "bad length wins");
    }

    #[test]
    fn strict_decode_stops_at_the_first_fillchar() {
        // "=" at position 2 of a quad ends the parse; every following quad
        // is ignored, exactly like the reference `break`.
        assert_eq!(decode_strict("Zg==Zg==").unwrap(), b"f");
        assert_eq!(decode_strict("Zm9vYg==Zm9vYg==").unwrap(), b"foob");
    }

    #[test]
    fn lenient_decode_has_no_length_check() {
        // "Zm9vYg" is 6 chars: one full quad plus an unterminated pair.
        assert_eq!(decode_lenient("Zm9vYg"), b"foob");
        // "Zg=" breaks at the fillchar in the second slot.
        assert_eq!(decode_lenient("Zg="), b"f");
        assert_eq!(decode_lenient("Zm8="), b"fo");
        assert!(decode_lenient("").is_empty());
    }

    #[test]
    fn alphabet_plus_and_slash_roundtrip() {
        // Bytes chosen so the encoded form contains '+' and '/'.
        let raw = [0xFBu8, 0xFF, 0xFE, 0xEF];
        let text = encode(&raw);
        assert!(text.contains('+') && text.contains('/'), "{text}");
        assert_eq!(decode_strict(&text).unwrap(), raw);
        assert_eq!(decode_lenient(&text), raw);
    }

    #[test]
    fn lenient_final_lone_char_feeds_the_bad_value() {
        // A lone trailing character reads past the buffer in the reference
        // (the NUL terminator decodes to 0xFF): "Z" → (25<<2)|(0xFF>>4)&3.
        assert_eq!(decode_lenient("Z"), vec![(25u8 << 2) | ((0xFF >> 4) & 0x3)]);
    }

    #[test]
    fn lenient_decode_maps_unknown_chars_to_ff() {
        // "Z?" → c=25, c1=0xFF → byte (25<<2)|((0xFF>>4)&3) = 100|3 = 103.
        assert_eq!(decode_lenient("Z?==")[0], (25u8 << 2) | ((0xFF >> 4) & 0x3));
    }
}
