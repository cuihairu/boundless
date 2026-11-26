//! `cstdmf/md5.hpp` (`MD5` + `MD5::Digest`) as used by the definition digest.
//!
//! The reference digest of the whole definition set is
//! `MD5::Digest::quote()`: 16 raw bytes rendered as 32 *uppercase* hex
//! nibbles. That is the value the tooling compares, so everything here is
//! about appending the same raw bytes the reference appends, in the same
//! order.

use md5::{Digest as _, Md5 as Md5Core};

/// An incremental `MD5`.
#[derive(Debug, Clone, Default)]
pub struct Md5 {
    hasher: Md5Core,
}

impl Md5 {
    /// `MD5::MD5()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `MD5::append(const void * data, int numBytes)`.
    ///
    /// The reference takes a signed byte count; a negative one simply
    /// appends nothing.
    pub fn append(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    /// `md5.append( text, sizeof( text ) )`: the C string *including* its
    /// terminating NUL, which is what `sizeof("Int")` contributes.
    pub fn append_cstr(&mut self, text: &str) {
        self.hasher.update(text.as_bytes());
        self.hasher.update([0u8]);
    }

    /// `md5.append( &value, sizeof( value ) )` for a one-byte `bool`
    /// (`allowNone_`).
    pub fn append_u8(&mut self, value: u8) {
        self.hasher.update([value]);
    }

    /// `md5.append( &value, sizeof( value ) )` for a little-endian 32-bit
    /// field (`dataFlags`, stream sizes, indexes).
    pub fn append_i32(&mut self, value: i32) {
        self.hasher.update(value.to_le_bytes());
    }

    /// The hash of the text bytes, no NUL (`append( c_str(), size )`).
    pub fn append_text(&mut self, text: &str) {
        self.hasher.update(text.as_bytes());
    }

    /// `MD5::Digest digest( md5 )` followed by `digest.quote()`.
    pub fn quote(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let digest = self.hasher.clone().finalize();
        let mut out = String::with_capacity(2 * Self::NUM_BYTES);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0xF) as usize] as char);
        }
        out
    }

    /// `MD5::Digest::NUM_BYTES`.
    pub const NUM_BYTES: usize = 16;

    /// `MD5::Digest::unquote` (`md5.cpp:510-527`): exactly twice the
    /// digest bytes as characters,
    /// each pair folded through the arithmetic `unquoteNibble`
    /// (`md5.cpp:500-503`) which — like the original — validates nothing
    /// beyond the length and wraps on out-of-range bytes. `None` is the
    /// length failure, the path where the reference clears the digest.
    pub fn unquote(quoted: &str) -> Option<[u8; Self::NUM_BYTES]> {
        let bytes = quoted.as_bytes();
        if bytes.len() != Self::NUM_BYTES * 2 {
            return None;
        }
        let mut out = [0u8; Self::NUM_BYTES];
        for (i, pair) in out.iter_mut().enumerate() {
            let hi = nibble(bytes[i * 2]);
            let lo = nibble(bytes[i * 2 + 1]);
            *pair = (hi << 4) | lo;
        }
        Some(out)
    }
}

/// `unquoteNibble` (`md5.cpp:500-503`): `c > '9' ? c - ('A' - 10) : c - '0'`.
/// Lowercase hex and non-hex bytes fold into garbage values, exactly as the
/// reference does; the equality comparison downstream does the rejecting.
fn nibble(c: u8) -> u8 {
    if c > b'9' {
        c.wrapping_sub(b'A' - 10)
    } else {
        c.wrapping_sub(b'0')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors_match_rfc() {
        // MD5("") and MD5("abc") from RFC 1321 / the empty-string vector.
        let empty = Md5::new();
        assert_eq!(
            empty.quote(),
            "D41D8CD98F00B204E9800998ECF8427E",
            "empty message"
        );

        let mut abc = Md5::new();
        abc.append_text("abc");
        assert_eq!(abc.quote(), "900150983CD24FB0D6963F7D28E17F72");
    }

    #[test]
    fn quoting_is_uppercase_hex_of_sixteen_bytes() {
        let mut hasher = Md5::new();
        hasher.append_text("abc");
        assert_eq!(hasher.quote().len(), 32);
        assert_eq!(hasher.quote(), hasher.quote().to_uppercase());
        assert_eq!(Md5::NUM_BYTES, 16);
    }

    #[test]
    fn cstr_appends_the_terminating_nul() {
        // "Int" + NUL must not hash like the bare text.
        let mut with_nul = Md5::new();
        with_nul.append_cstr("Int");
        let mut without_nul = Md5::new();
        without_nul.append_text("Int");
        assert_ne!(with_nul.quote(), without_nul.quote());
    }

    #[test]
    fn integers_go_little_endian() {
        let mut small = Md5::new();
        small.append_i32(0x0102_0304);
        let mut one = Md5::new();
        one.append(&[4, 3, 2, 1]);
        assert_eq!(small.quote(), one.quote());

        let mut wide = Md5::new();
        wide.append_i32(-1);
        let mut ffs = Md5::new();
        ffs.append(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(wide.quote(), ffs.quote());
    }

    #[test]
    fn unquote_folds_pairs_like_unquote_nibble() {
        // The RFC "abc" vector, hand-folded through unquoteNibble.
        assert_eq!(
            Md5::unquote("900150983CD24FB0D6963F7D28E17F72"),
            Some([
                0x90, 0x01, 0x50, 0x98, 0x3C, 0xD2, 0x4F, 0xB0, 0xD6, 0x96, 0x3F, 0x7D, 0x28, 0xE1,
                0x7F, 0x72
            ])
        );
    }

    #[test]
    fn unquote_rejects_any_other_length() {
        assert_eq!(Md5::unquote("TOO SHORT"), None);
        assert_eq!(Md5::unquote(""), None);
        assert_eq!(
            Md5::unquote("900150983CD24FB0D6963F7D28E17F720"),
            None,
            "33 characters"
        );
    }

    #[test]
    fn unquote_folds_garbage_nibbles_like_the_reference() {
        // Lowercase and non-hex bytes are not validated: they fold through
        // the same arithmetic ('a' - ('A' - 10) = 42, 'b' - 55 = 43) and
        // the u8 fold wraps, leaving the downstream equality to reject.
        assert_eq!(Md5::unquote(&"ab".repeat(16)), Some([0xAB; 16]));
        assert_eq!(Md5::unquote(&"00".repeat(16)), Some([0; 16]));

        // quote -> unquote hands the digest bytes back.
        let mut hasher = Md5::new();
        hasher.append_text("abc");
        let digest = Md5::unquote(&hasher.quote()).expect("the digest unquotes");
        assert_eq!(digest[0], 0x90);
        assert_eq!(digest[15], 0x72);
    }
}
