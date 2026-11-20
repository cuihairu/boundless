//! Fixed random seeds for tests (BW-18-01-02).
//!
//! The reference seeds itself from the wall clock (`NetworkApp::run` calls
//! `srand((int)timestamp())`), which makes repeated runs diverge. Tests need
//! the opposite: the same seed must always yield the same sequence, on every
//! platform and build, so a scenario can be replayed exactly.
//!
//! The generator is `SplitMix64`, chosen for being platform independent and
//! for spreading a small number of seeds well.

/// A deterministic 64-bit generator with a reproducible seed.
#[derive(Debug, Clone, Copy)]
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    /// Build a generator from `seed`; identical seeds repeat identically.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    /// Next 32-bit value, the low bits of the 64-bit stream.
    pub fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    /// Next value in `range`, excluding the upper bound.
    pub fn next_below(&mut self, range: u64) -> u64 {
        if range == 0 {
            return 0;
        }
        self.next_u64() % range
    }

    /// Next floating value in `[0, 1)`.
    pub fn next_float(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// True for the share `numerator / denominator`.
    pub fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        denominator == 0 || self.next_below(denominator) < numerator
    }

    /// Draw `count` 64-bit values, a helper for replaying a scenario.
    pub fn draw(&mut self, count: usize) -> Vec<u64> {
        (0..count).map(|_| self.next_u64()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_repeats_the_same_sequence() {
        let mut first = SeededRng::new(2026_1120);
        let mut second = SeededRng::new(2026_1120);
        assert_eq!(first.draw(64), second.draw(64));
    }

    #[test]
    fn different_seeds_diverge() {
        let mut first = SeededRng::new(1);
        let mut second = SeededRng::new(2);
        let one = first.draw(16);
        let two = second.draw(16);
        assert_ne!(one, two);
    }

    #[test]
    fn the_zero_seed_is_usable() {
        let mut rng = SeededRng::new(0);
        // Every splitmix state is valid, including the all-zero one.
        assert_eq!(rng.next_u64(), 16294208416658607535);
    }

    #[test]
    fn bounded_values_stay_in_range() {
        let mut rng = SeededRng::new(77);
        assert_eq!(rng.next_below(0), 0);
        for _ in 0..200 {
            assert!(rng.next_below(1000) < 1000);
            assert!((0.0..1.0).contains(&rng.next_float()));
        }
    }

    #[test]
    fn narrow_words_follow_the_wide_stream() {
        // Both generators share the same draw, so the low 32 bits of the next
        // 64-bit value are exactly what `next_u32` would hand out.
        let mut wide = SeededRng::new(5);
        let _ = wide.next_u64();
        let first_64 = wide.next_u64();

        let mut narrow = SeededRng::new(5);
        let _ = narrow.next_u64();
        assert_eq!(narrow.next_u32(), first_64 as u32);
    }

    #[test]
    fn chances_respect_the_share() {
        let mut rng = SeededRng::new(3);
        let mut hits = 0;
        for _ in 0..10_000 {
            if rng.chance(1, 1_000) {
                hits += 1;
            }
        }
        // Roughly 1%, with generous slack for a small test suite.
        assert!(hits > 5 && hits < 25, "hits: {hits}");
    }
}
