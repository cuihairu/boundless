//! `MyPerlin::noise1` replica.
//!
//! The reference flora animation grids sample `MyPerlin::noise1`
//! (`lib/math/perlin_noise.cpp`) for every vertex column every frame. That
//! path is purely arithmetic: `IntRand` is an integer hash and `cosInterp`
//! blends with a cosine, so it is bit-replayable without any libc random.
//!
//! The reference constructor also calls `srand(0)` and fills a 3 x 256
//! table of `rand()` draws. Nothing in the flora path ever reads that
//! table (`noise1` only ever touches `IntRand`), so this port registers
//! it as skipped rather than reproducing 768 dead draws.

/// One octave of the interpolation hash, `IntRand`
/// (`perlin_noise.cpp:238`): `((x*(x*x*15731 + 789221) + 1376312589) &
/// 0x7fffffff) / 1073741824.0f / 2.0f`, range [0, 1].
fn int_rand(x: i32) -> f32 {
    let x = x.wrapping_mul(8192) ^ x;
    let hashed = x
        .wrapping_mul(x.wrapping_mul(x).wrapping_mul(15731).wrapping_add(789221))
        .wrapping_add(1_376_312_589);
    // The reference masks to a non-negative int, then divides as float.
    let masked = (hashed & 0x7fff_ffff) as f32;
    masked / 1073741824.0f32 / 2.0f32
}

/// `cosInterp`: cosine-eased blend of two hashes.
#[allow(clippy::approx_constant)] // the reference's literal (perlin_noise.cpp cosInterp)
fn cos_interp(a: f32, b: f32, t: f32) -> f32 {
    let ft = t * 3.1415927f32;
    let f = (1.0f32 - ft.cos()) * 0.5f32;
    a * (1.0f32 - f) + b * f
}

/// A flora noise source: `noise1` only, with the reference default of six
/// octaves and 0.25 persistence.
pub struct FloraNoise {
    octaves: i32,
    persistence: f32,
}

impl FloraNoise {
    /// Reference defaults (`perlin_noise.cpp:244` and friends).
    pub fn new() -> FloraNoise {
        FloraNoise {
            octaves: 6,
            persistence: 0.25,
        }
    }

    /// The borrowed-time setup: the reference assigns both fields through
    /// setters after construction.
    pub fn with_octaves(mut self, octaves: i32) -> FloraNoise {
        self.octaves = octaves;
        self
    }

    pub fn with_persistence(mut self, persistence: f32) -> FloraNoise {
        self.persistence = persistence;
        self
    }

    /// The reference's post-construction setters (`MyPerlin::octaves` /
    /// `persistence`), which the flora renderer drives with watcher values.
    pub fn set_octaves(&mut self, octaves: i32) {
        self.octaves = octaves;
    }

    pub fn set_persistence(&mut self, persistence: f32) {
        self.persistence = persistence;
    }

    /// `MyPerlin::noise1`: an octave blend of hashed cosine curves,
    /// normalised by the accumulated amplitude.
    pub fn noise1(&self, t: f32) -> f32 {
        let mut total_amp = 0.0f32;
        let mut amp = self.persistence;
        let mut freq = 1.0f32;
        let mut noise = 0.0f32;
        let mut i_octave = 0;
        while i_octave < self.octaves {
            let curt = t * freq;
            let i0 = curt as i32;
            let k = curt - curt.floor();
            noise += cos_interp(int_rand(i0), int_rand(i0 + 1), k) * amp;
            total_amp += amp;
            amp *= self.persistence;
            freq *= 2.0f32;
            i_octave += 1;
        }
        noise / total_amp
    }
}

impl Default for FloraNoise {
    fn default() -> FloraNoise {
        FloraNoise::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_rand_is_bounded_and_deterministic() {
        // The reference masks to 2^31-1 and divides twice, so the result
        // lies in [0, 1].
        for i in -2000..2000 {
            let v = int_rand(i);
            assert!((0.0..=1.0).contains(&v), "{i} -> {v}");
        }
        assert_eq!(int_rand(7), int_rand(7));
        assert_ne!(int_rand(7), int_rand(8));
    }

    #[test]
    fn int_rand_wraps_like_signed_int() {
        // `x << 13` on a negative int is an arithmetic shift: 8192 * x.
        // The hash must not saturate on the extremes of the domain.
        assert!((0.0..=1.0).contains(&int_rand(i32::MIN)));
        assert!((0.0..=1.0).contains(&int_rand(i32::MAX)));
    }

    #[test]
    fn cos_interp_is_end_point_exact() {
        assert_eq!(cos_interp(1.0, 2.0, 0.0), 1.0);
        // t = 1 gives f = 1, so the blend lands exactly on b.
        assert_eq!(cos_interp(0.0, 2.0, 1.0), 2.0);
    }

    #[test]
    fn noise1_stays_within_the_amplitude_envelope() {
        let noise = FloraNoise::new();
        for i in 0..1000 {
            let v = noise.noise1(i as f32 * 0.013);
            assert!(v.is_finite());
            assert!(v.abs() <= 1.0 + 1e-6, "i={i} v={v}");
        }
    }

    #[test]
    fn noise1_is_deterministic_and_phase_dependent() {
        let noise = FloraNoise::new();
        assert_eq!(noise.noise1(123.0), noise.noise1(123.0));
        assert_ne!(noise.noise1(123.0), noise.noise1(124.0));
        // Default is the same constructor the renderer's grids fall back
        // to.
        assert_eq!(FloraNoise::default().noise1(123.0), noise.noise1(123.0));
    }

    #[test]
    fn negative_time_truncates_like_the_reference_cast() {
        // `(int)curt` truncates toward zero while `floor` steps down, so a
        // negative offset still yields a fractional part in [0, 1). With a
        // single octave and persistence 1 the normalisation divides by one.
        let one = FloraNoise::new().with_octaves(1).with_persistence(1.0);
        let curt = -3.7f32;
        let i0 = curt as i32; // -3, not -4
        assert_eq!(i0, -3);
        let k = curt - curt.floor();
        assert!((0.0f32..1.0f32).contains(&k));
        let expected = cos_interp(int_rand(i0), int_rand(i0 + 1), k);
        assert_eq!(one.noise1(curt), expected);
    }

    #[test]
    fn octaves_and_persistence_are_honoured() {
        // One octave at persistence 1.0 is exactly the two-hash blend.
        let one = FloraNoise::new().with_octaves(1).with_persistence(1.0);
        let expected = cos_interp(int_rand(10), int_rand(11), 0.0);
        assert_eq!(one.noise1(10.0), expected);
        // And it differs from the six-octave default.
        assert_ne!(one.noise1(10.0), FloraNoise::new().noise1(10.0));
    }
}
