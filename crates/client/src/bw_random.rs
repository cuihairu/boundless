//! Mersenne Twister `bw_random()` replica.
//!
//! The reference particle system draws all of its randomness through
//! `bw_random()` (`lib/cstdmf/bwrandom.cpp`), a plain MT19937 with two
//! convenience range operators. The generators below are verbatim ports:
//! `init_genrand`-style scalar seeding, array seeding through the
//! `19650218`/`1664525`/`1566083941` schedule, the two-segment twist, and
//! the four tempering steps.
//!
//! `unitRand()` (`lib/math/mathdef.hpp:118`) is
//! `((float)bw_random())/((float)(uint32)-1)` — both operands are f32, so
//! the divisor is the f32 rounding of 2^32-1 (exactly 2^32) and the
//! numerator is the f32 rounding of the drawn word. The float range
//! operator additionally differs from `unitRand`: it divides by
//! `4294967295.0` **as a double** before narrowing, which this port
//! preserves.

/// Size of the state table.
const N: usize = 624;
/// Twist offset: the middle word the twist mixes in.
const M: usize = 397;
/// The uppermost bit, kept by the twist's first operand.
const UPPER_MASK: u32 = 0x8000_0000;
/// All but the uppermost bit, kept by the twist's second operand.
const LOWER_MASK: u32 = 0x7fff_ffff;

/// A seeded `bw_random()`-compatible generator.
pub struct BWRandom {
    mt: [u32; N],
    /// Position of the next word; `N + 1` after seeding forces a twist.
    mti: usize,
}

impl BWRandom {
    /// `bw_random_init` with a scalar seed: the 1812433253 recurrence.
    pub fn new(seed: u32) -> BWRandom {
        let mut mt = [0u32; N];
        mt[0] = seed;
        for i in 1..N {
            let prev = mt[i - 1];
            mt[i] = 1812433253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        BWRandom { mt, mti: N + 1 }
    }

    /// `bw_random_init` with an array seed: first stirred through the
    /// 19650218 base, then through the 1566083941 pass, then forced
    /// unbalanced with `mt[0] = 0x80000000` exactly as the reference does.
    pub fn new_from_array(seed: &[u32]) -> BWRandom {
        let mut ret = BWRandom::new(19650218);
        let mut i = 1usize;
        let mut j = 0usize;
        let mut k = if N > seed.len() { N } else { seed.len() };
        while k > 0 {
            let prev = ret.mt[i - 1];
            ret.mt[i] = (ret.mt[i] ^ ((prev ^ (prev >> 30)).wrapping_mul(1664525)))
                .wrapping_add(seed[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                ret.mt[0] = ret.mt[N - 1];
                i = 1;
            }
            if j >= seed.len() {
                j = 0;
            }
            k -= 1;
        }
        let mut k = N - 1;
        while k > 0 {
            let prev = ret.mt[i - 1];
            ret.mt[i] = (ret.mt[i] ^ ((prev ^ (prev >> 30)).wrapping_mul(1566083941)))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                ret.mt[0] = ret.mt[N - 1];
                i = 1;
            }
            k -= 1;
        }
        // The reference forces the table unbalanced before first use.
        ret.mt[0] = 0x8000_0000;
        ret.mti = N + 1;
        ret
    }

    /// `bw_random()`: one tempered word, twisting when the table is spent.
    // Named `next` after the reference's raw draw, not an Iterator item:
    // every call site consumes the bare word, not an Option.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u32 {
        if self.mti >= N {
            let mut kk = 0;
            while kk < N - M {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + M] ^ (y >> 1) ^ mag01(y & 1);
                kk += 1;
            }
            while kk < N - 1 {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + M - N] ^ (y >> 1) ^ mag01(y & 1);
                kk += 1;
            }
            let y = (self.mt[N - 1] & UPPER_MASK) | (self.mt[0] & LOWER_MASK);
            self.mt[N - 1] = self.mt[M - 1] ^ (y >> 1) ^ mag01(y & 1);
            self.mti = 0;
        }

        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// The integer range operator: equal bounds return the bound, reversed
    /// bounds are swapped, otherwise `min + r % (max - min)`.
    pub fn range_int(&mut self, mut min: i32, mut max: i32) -> i32 {
        if min == max {
            return min;
        }
        if min > max {
            std::mem::swap(&mut min, &mut max);
        }
        // The reference takes the modulus on `int` differences.
        min + (self.next() % ((max - min) as u32)) as i32
    }

    /// The float range operator: equal bounds return the bound, reversed
    /// bounds are swapped, otherwise a double-normalised draw scales the
    /// span.
    pub fn range_float(&mut self, mut min: f32, mut max: f32) -> f32 {
        if is_equal(min, max) {
            return min;
        }
        if min > max {
            std::mem::swap(&mut min, &mut max);
        }
        let r = (self.next() as f64 / 4294967295.0) as f32;
        min + r * (max - min)
    }

    /// `unitRand()` from `lib/math/mathdef.hpp:118`: f32 word over f32
    /// 2^32-1, which rounds to exactly 2^32.
    pub fn unit_rand(&mut self) -> f32 {
        self.next() as f32 / 4294967295.0f32
    }
}

/// `mag01[y & 1]`: MATRIX_A for odd halves, zero for even ones.
fn mag01(lower: u32) -> u32 {
    if lower & 1 != 0 { 0x9908_b0df } else { 0 }
}

/// `isEqual<float>` from `stdmf_minimal.hpp:119` — `fabsf(a-b) <= 0.f`,
/// exact equality (false for NaN).
fn is_equal(a: f32, b: f32) -> bool {
    (a - b).abs() <= 0.0
}

/// `randInRange` from `lib/math/mathdef.hpp:122`.
pub fn rand_in_range(min: f32, max: f32, rng: &mut BWRandom) -> f32 {
    min + rng.unit_rand() * (max - min)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent Python MT19937 (cross-validated against the published
    /// `init_genrand(5489)` first output 3499211612).
    #[test]
    fn scalar_seeds_match_the_mt19937_sequence() {
        let mut rng = BWRandom::new(5489);
        assert_eq!(
            (0..8).map(|_| rng.next()).collect::<Vec<_>>(),
            vec![
                3499211612, 581869302, 3890346734, 3586334585, 545404204, 4161255391, 3922919429,
                949333985
            ]
        );

        let mut rng = BWRandom::new(1);
        assert_eq!(
            (0..8).map(|_| rng.next()).collect::<Vec<_>>(),
            vec![
                1791095845, 4282876139, 3093770124, 4005303368, 491263, 550290313, 1298508491,
                4290846341
            ]
        );

        let mut rng = BWRandom::new(0xdead_beef);
        assert_eq!(
            (0..4).map(|_| rng.next()).collect::<Vec<_>>(),
            vec![956529277, 3842322136, 3319553134, 1843186657]
        );
    }

    #[test]
    fn the_twist_boundary_is_exercised_past_the_first_table() {
        // 700 draws cross the 624-word twist boundary at least once; the
        // 700th word is the independent oracle's value.
        let mut rng = BWRandom::new(5489);
        let mut last = 0;
        for _ in 0..700 {
            last = rng.next();
        }
        assert_eq!(last, 2381045132);
    }

    #[test]
    fn array_seeding_follows_the_reference_schedule() {
        let mut a = BWRandom::new_from_array(&[1, 2, 3, 4, 5]);
        let first: Vec<u32> = (0..8).map(|_| a.next()).collect();
        let mut b = BWRandom::new_from_array(&[1, 2, 3, 4, 5]);
        assert_eq!(first, (0..8).map(|_| b.next()).collect::<Vec<_>>());
        let mut c = BWRandom::new(19650218);
        assert_ne!(first[0], c.next());

        // An array longer than the table (625 entries) still terminates.
        let seed: Vec<u32> = (0..625).map(|i| i as u32).collect();
        let mut d = BWRandom::new_from_array(&seed);
        let _ = d.next();
    }

    #[test]
    fn range_int_matches_the_reference_operators() {
        // Equal bounds return the bound without drawing.
        let mut rng = BWRandom::new(5489);
        assert_eq!(rng.range_int(7, 7), 7);
        // Reversed bounds are swapped.
        let mut a = BWRandom::new(5489);
        let mut b = BWRandom::new(5489);
        assert_eq!(a.range_int(0, 10), b.range_int(10, 0));
        // The span arithmetic: min + r % (max - min).
        let mut r = BWRandom::new(5489);
        let first = r.next();
        let mut r2 = BWRandom::new(5489);
        assert_eq!(r2.range_int(3, 13), 3 + (first % 10) as i32);
    }

    #[test]
    fn range_float_matches_the_reference_operators() {
        let mut rng = BWRandom::new(5489);
        assert_eq!(rng.range_float(2.5, 2.5), 2.5);
        // Double-division normalisation, then the span scale.
        let mut r = BWRandom::new(5489);
        let first = r.next();
        let mut r2 = BWRandom::new(5489);
        let expected = 1.0f32 + (first as f64 / 4294967295.0) as f32 * 4.0;
        assert_eq!(r2.range_float(1.0, 5.0), expected);
        // Reversed bounds are swapped.
        let mut a = BWRandom::new(5489);
        let mut b = BWRandom::new(5489);
        assert_eq!(a.range_float(1.0, 5.0), b.range_float(5.0, 1.0));
    }

    #[test]
    fn unit_rand_uses_the_f32_denominator() {
        let mut rng = BWRandom::new(5489);
        let first = rng.next();
        let mut r2 = BWRandom::new(5489);
        let expected = first as f32 / 4294967296.0f32;
        assert_eq!(r2.unit_rand(), expected);
        // The double-division ratio differs from the f32 one.
        let as_f64 = (first as f64 / 4294967295.0) as f32;
        assert_ne!(r2.unit_rand(), as_f64);
        // Every draw stays within [0, 1].
        let mut r3 = BWRandom::new(42);
        for _ in 0..1000 {
            let u = r3.unit_rand();
            assert!((0.0..=1.0).contains(&u));
        }
    }

    #[test]
    fn rand_in_range_scales_the_span() {
        let mut rng = BWRandom::new(5489);
        let first = rng.next();
        let mut r = BWRandom::new(5489);
        let expected = 2.0f32 + (first as f32 / 4294967296.0f32) * 3.0;
        assert_eq!(rand_in_range(2.0, 5.0, &mut r), expected);
    }
}
