//! glibc `rand()` replica.
//!
//! The reference flora system seeds libc `srand(0)` before building its
//! placement tables (`lib/romp/flora.cpp:1070`), so a replayable port needs
//! bit-exact libc `rand()`. glibc's implementation is the TYPE_3 additive
//! feedback generator: 31 words of state seeded by Schrage multiplication of
//! 16807 mod 2147483647, then 310 discarded draws, and every draw executes
//! `*fptr += *rptr` with `fptr` starting at state slot 3 and `rptr` at slot 0,
//! returning the stored word's top bit dropped. `srand(0)` is remapped to
//! seed 1 by the reference libc.

/// Number of state words in the TYPE_3 generator.
const STATE: usize = 31;

/// Discarded draws between seeding and the first returned value.
const WARMUP: usize = 310;

/// RAND_MAX on the reference platform.
pub const RAND_MAX: i32 = 2147483647;

/// A seeded libc-compatible generator. Any seed works; the reference tables
/// only ever call `srand(0)`.
pub struct LibcRand {
    state: [u32; STATE],
    /// glibc's `fptr` — the slot the draw is added into, starting at 3.
    fptr: usize,
    /// glibc's `rptr` — the slot the draw is added from, starting at 0.
    rptr: usize,
}

impl LibcRand {
    pub fn new(seed: u32) -> LibcRand {
        // glibc maps a zero seed to one.
        let seed = if seed == 0 { 1 } else { seed };

        let mut state = [0u32; STATE];
        state[0] = seed;
        for i in 1..STATE {
            // Schrage multiplication of 16807 mod 2147483647, as the
            // reference libc does it: hi = prev / 127773, lo = prev % 127773,
            // word = 16807*lo - 2836*hi, negative words wrap by 2^31 - 1.
            let prev = state[i - 1];
            let hi = prev / 127773;
            let lo = prev - hi * 127773;
            let mut word = 16807u32
                .wrapping_mul(lo)
                .wrapping_sub(2836u32.wrapping_mul(hi));
            if word > RAND_MAX as u32 {
                word = word.wrapping_add(RAND_MAX as u32);
            }
            state[i] = word;
        }

        let mut ret = LibcRand {
            state,
            fptr: 3,
            rptr: 0,
        };
        // The reference discards the first 310 draws of the TYPE_3 schedule.
        for _ in 0..WARMUP {
            ret.next_word();
        }
        ret
    }

    /// One step of `*fptr += *rptr`, returning the stored word.
    fn next_word(&mut self) -> u32 {
        let sum = self.state[self.fptr].wrapping_add(self.state[self.rptr]);
        self.state[self.fptr] = sum;
        self.fptr = (self.fptr + 1) % STATE;
        self.rptr = (self.rptr + 1) % STATE;
        sum
    }

    /// One `rand()` draw: the feedback word with its top bit dropped.
    // Named `next` after the reference's `rand()` draw contract, not an
    // Iterator item: call sites consume the bare word, not an Option.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> i32 {
        (self.next_word() >> 1) as i32
    }

    /// The `(float)rand() / (float)RAND_MAX` idiom the reference uses for
    /// every table entry.
    pub fn next_unit(&mut self) -> f32 {
        self.next() as f32 / RAND_MAX as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values produced by the machine libc: `srand(0); rand()` forty times.
    const GLIBC_SRAND0: [i32; 40] = [
        1804289383, 846930886, 1681692777, 1714636915, 1957747793, 424238335, 719885386,
        1649760492, 596516649, 1189641421, 1025202362, 1350490027, 783368690, 1102520059,
        2044897763, 1967513926, 1365180540, 1540383426, 304089172, 1303455736, 35005211, 521595368,
        294702567, 1726956429, 336465782, 861021530, 278722862, 233665123, 2145174067, 468703135,
        1101513929, 1801979802, 1315634022, 635723058, 1369133069, 1125898167, 1059961393,
        2089018456, 628175011, 1656478042,
    ];

    #[test]
    fn srand_zero_matches_the_libc_sequence() {
        let mut rng = LibcRand::new(0);
        for expected in GLIBC_SRAND0 {
            assert_eq!(rng.next(), expected);
        }
    }

    #[test]
    fn srand_zero_and_srand_one_agree() {
        // The reference tables seed zero; the libc maps that to one.
        let mut a = LibcRand::new(0);
        let mut b = LibcRand::new(1);
        for _ in 0..10 {
            assert_eq!(a.next(), b.next());
        }
    }

    #[test]
    fn unit_draws_stay_in_range_and_match_the_first_ratio() {
        let mut rng = LibcRand::new(0);
        for _ in 0..1000 {
            let u = rng.next_unit();
            assert!((0.0..=1.0).contains(&u));
        }
        // Spot-check the first ratio: (float)1804289383 / (float)RAND_MAX.
        let mut rng = LibcRand::new(0);
        let first = 1804289383.0f64 / RAND_MAX as f64;
        assert!((rng.next_unit() as f64 - first).abs() < 1e-6);
    }

    #[test]
    fn the_schrage_wrap_wraps_negative_words() {
        // Seed 12's state walk hits a negative intermediate at word 29
        // (16807*lo - 2836*hi <= 0): the wrap branch fires during the
        // 31-word fill, and the first draw lands on 1687063760.
        let mut rng = LibcRand::new(12);
        assert_eq!(rng.next(), 1687063760);
        assert_eq!(rng.next(), 945274514);
        // Every draw stays inside RAND_MAX and the state replays.
        for _ in 0..1000 {
            let v = rng.next();
            assert!((0..=RAND_MAX).contains(&v));
        }
        let mut a = LibcRand::new(12);
        let mut b = LibcRand::new(12);
        for _ in 0..100 {
            assert_eq!(a.next(), b.next());
        }
    }

    #[test]
    fn a_different_seed_diverges() {
        let mut zero = LibcRand::new(0);
        let mut seven = LibcRand::new(7);
        assert_ne!(zero.next(), seven.next());
    }
}
