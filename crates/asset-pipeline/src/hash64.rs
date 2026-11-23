//! `Hash64` port (`bw_hash64.cpp`): the 64-bit FNV-1a hash the asset
//! pipeline uses for dependency and file hashes.
//!
//! Reference quirks preserved:
//! - `compute` over bytes is FNV-1a with the standard 64-bit offset basis
//!   `14695981039346656037` and prime `1099511628211`;
//! - `compute` over an integer hashes its little-endian 8 bytes (the
//!   reference hashes `__int64`/`unsigned __int64` by memory, not digits);
//! - `combine` mixes a value into a running seed with the golden-ratio
//!   constant `0x9E3779B97F4A7C15` plus `seed << 5 + seed >> 3`, all
//!   wrapping (the reference relies on unsigned wraparound).

/// `FNV_prime` (64-bit).
const FNV_PRIME: u64 = 1_099_511_628_211;
/// `FNV_offset_basis` (64-bit).
const FNV_OFFSET_BASIS: u64 = 14_695_981_039_346_656_037;
/// `2^64/phi`, the golden-ratio constant used by `combine`.
const GOLDEN_RATIO: u64 = 0x9E37_79B9_7F4A_7C15;

/// `Hash64::compute( data, length )`: FNV-1a over raw bytes.
pub fn compute_bytes(data: &[u8]) -> u64 {
    let mut result = FNV_OFFSET_BASIS;
    for &byte in data {
        result = (result ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
    }
    result
}

/// `Hash64::compute( value )` for a byte string.
pub fn compute_str(value: &str) -> u64 {
    compute_bytes(value.as_bytes())
}

/// `Hash64::compute( const __int64 & )` / `( const unsigned __int64 & )`:
/// hash the integer's little-endian 8 bytes.
pub fn compute_i64(value: i64) -> u64 {
    compute_bytes(&value.to_le_bytes())
}

/// `Hash64::compute( const unsigned __int64 & )`.
pub fn compute_u64(value: u64) -> u64 {
    compute_bytes(&value.to_le_bytes())
}

/// `Hash64::combine( seed, value )`: mix a value into a running seed.
pub fn combine(seed: &mut u64, value: u64) {
    // seed ^= compute(value) + 0x9E3779B97F4A7C15 + (seed << 5) + (seed >> 3)
    *seed ^= value
        .wrapping_add(GOLDEN_RATIO)
        .wrapping_add(seed.wrapping_shl(5))
        .wrapping_add(seed.wrapping_shr(3));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference known-value check: FNV-1a of the empty input is the offset
    /// basis, and a single zero byte shifts it deterministically.
    #[test]
    fn compute_empty_is_offset_basis() {
        assert_eq!(compute_bytes(&[]), 14_695_981_039_346_656_037);
    }

    #[test]
    fn compute_single_zero_byte() {
        // (basis ^ 0) * prime == basis * prime (wrapping).
        let expected = FNV_OFFSET_BASIS.wrapping_mul(FNV_PRIME);
        assert_eq!(compute_bytes(&[0]), expected);
    }

    /// FNV-1a of "a" is the classic published value.
    #[test]
    fn compute_known_fnv1a() {
        assert_eq!(compute_str("a"), 0xAF63_DC4C_8601_EC8C);
    }

    #[test]
    fn compute_str_equals_bytes() {
        assert_eq!(compute_str("hello"), compute_bytes(b"hello"));
    }

    #[test]
    fn compute_i64_uses_le_bytes() {
        // compute_i64 hashes the same bytes as compute_bytes of the LE bytes.
        assert_eq!(
            compute_i64(0x1234_5678_9ABC_DEF0),
            compute_bytes(&0x1234_5678_9ABC_DEF0u64.to_le_bytes())
        );
        // A negative value hashes its two's-complement bytes, not its digits.
        assert_eq!(compute_i64(-1), compute_u64(u64::MAX));
    }

    /// `combine` mixes deterministically: same seed and value give the same
    /// result, and a different value changes the outcome.
    #[test]
    fn combine_is_deterministic_and_sensitive() {
        let mut a = 0u64;
        combine(&mut a, 0x1111);
        let mut b = 0u64;
        combine(&mut b, 0x1111);
        assert_eq!(a, b);

        let mut c = 0u64;
        combine(&mut c, 0x2222);
        assert_ne!(a, c);
    }

    /// A combine of the same value on different seeds diverges (the `seed << 5`
    /// term mixes the seed itself).
    #[test]
    fn combine_mixes_seed() {
        let mut a = 1u64;
        combine(&mut a, 0x1111);
        let mut b = 2u64;
        combine(&mut b, 0x1111);
        assert_ne!(a, b);
    }

    /// `combine` wraps like unsigned C++ arithmetic (no overflow panic).
    #[test]
    fn combine_wraps_on_overflow() {
        let mut seed = u64::MAX;
        combine(&mut seed, u64::MAX);
        // Just assert it did not panic and produced a value.
        let _ = seed;
    }
}
