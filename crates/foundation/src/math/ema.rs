//! `BW::EMA` and `BW::AccumulatingEMA` (`lib/math/ema.hpp`, `lib/math/ema.cpp`).
//!
//! The network statistics keep exponentially weighted per-second averages of
//! what an interface element received; the accumulating variant sums a raw
//! counter between periodic samples.

use std::ops::AddAssign;

/// `BW::EMA`: `avg(n) = (1 - bias) * sample(n) + bias * avg(n-1)`.
#[derive(Debug, Clone, Copy)]
pub struct Ema {
    /// `bias_`: how much the previous average weighs in.
    bias: f32,
    /// `average_`: the current value of the average.
    average: f32,
}

impl Ema {
    /// `EMA( bias, initial = 0 )`.
    pub fn new(bias: f32, initial: f32) -> Self {
        Self {
            bias,
            average: initial,
        }
    }

    /// Sample a value into the average.
    pub fn sample(&mut self, value: f32) {
        self.average = (1.0 - self.bias) * value + self.bias * self.average;
    }

    /// The current value of the average.
    pub fn average(&self) -> f32 {
        self.average
    }

    /// Reset the average to `new_value` (0 by default).
    pub fn reset_average(&mut self, new_value: f32) {
        self.average = new_value;
    }

    /// `highWaterSample`: take the value over the average when it is
    /// larger, otherwise sample it normally.
    pub fn high_water_sample(&mut self, value: f32) {
        if value > self.average {
            self.reset_average(value);
        } else {
            self.sample(value);
        }
    }

    /// The bias used for weighting.
    pub fn bias(&self) -> f32 {
        self.bias
    }

    /// `calculateBiasFromNumSamples( numSamples, weighting = 0.95 )`.
    ///
    /// `isZero` in the reference is an exact zero test
    /// (`fabs(v) <= 0`), so a zero sample count yields 0.
    pub fn calculate_bias_from_num_samples(num_samples: f32, weighting: f32) -> f32 {
        if num_samples == 0.0 {
            0.0
        } else {
            ((1.0 - weighting).ln() / num_samples).exp()
        }
    }
}

/// `AccumulatingEMA< TYPE >`: accumulate a raw counter, then sample the
/// accumulated total into an `Ema` on a fixed cadence.
#[derive(Debug, Clone)]
pub struct AccumulatingEma<T> {
    /// `average_`.
    average: Ema,
    /// `accum_`.
    accum: T,
}

impl<T> AccumulatingEma<T>
where
    T: Default + Copy + AddAssign + Accumulator,
{
    /// `AccumulatingEMA( bias, initialAverage = 0, initialValue = TYPE() )`.
    pub fn new(bias: f32, initial_average: f32, initial_value: T) -> Self {
        Self {
            average: Ema::new(bias, initial_average),
            accum: initial_value,
        }
    }

    /// `value()` (const).
    pub fn value(&self) -> T {
        self.accum
    }

    /// `value()` (mutable): read the accumulated total.
    pub fn value_mut(&mut self) -> &mut T {
        &mut self.accum
    }

    /// `++value()`: the reference increments the counter for every
    /// received message.
    pub fn increment_value(&mut self) {
        *self.value_mut() += T::one();
    }

    /// `value() += delta`: the reference adds a byte count to the counter.
    pub fn add_to_value(&mut self, delta: T) {
        *self.value_mut() += delta;
    }

    /// `average()`.
    pub fn average(&self) -> f32 {
        self.average.average()
    }

    /// `sample( shouldReset = true )`: fold the accumulated total into the
    /// average, resetting it afterwards unless asked otherwise.
    pub fn sample(&mut self, should_reset: bool) {
        self.average.sample(T::into_f32(self.accum));
        if should_reset {
            self.accum = T::default();
        }
    }
}

/// The two arithmetic edges of the counters the reference accumulates.
///
/// This Rust toolchain removed `f32: From<u32>`/`From<i32>`, so the
/// `float( accum_ )` conversion the reference gets for free needs naming.
pub trait Accumulator: Sized {
    /// The value one more than zero.
    fn one() -> Self;
    /// `float( value )`.
    fn into_f32(value: Self) -> f32;
}

impl Accumulator for u32 {
    fn one() -> Self {
        1
    }

    fn into_f32(value: Self) -> f32 {
        value as f32
    }
}

impl Accumulator for u64 {
    fn one() -> Self {
        1
    }

    fn into_f32(value: Self) -> f32 {
        value as f32
    }
}

impl Accumulator for f32 {
    fn one() -> Self {
        1.0
    }

    fn into_f32(value: Self) -> f32 {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `calculateBiasFromNumSamples` is the exact reference formula, and a
    /// zero sample count takes the `isZero` guard to 0.
    #[test]
    fn bias_from_num_samples_matches_the_reference_formula() {
        let bias = Ema::calculate_bias_from_num_samples(5.0, 0.95);
        // `1 - weighting` is not exactly 0.05 in f32, so compare against
        // the same expression rather than a decimal literal.
        assert_eq!(bias, ((1.0f32 - 0.95f32).ln() / 5.0f32).exp());
        assert!((bias - 0.54928f32).abs() < 1e-6, "{bias}");

        assert_eq!(
            Ema::calculate_bias_from_num_samples(0.0, 0.95),
            0.0,
            "isZero guard"
        );
    }

    /// The recursive form of the average, with bias 0 taking the newest
    /// sample entirely.
    #[test]
    fn the_average_folds_in_each_sample_with_the_bias() {
        let mut ema = Ema::new(0.5, 0.0);
        ema.sample(10.0);
        assert_eq!(ema.average(), 5.0, "(1-0.5)*10 + 0.5*0");
        ema.sample(20.0);
        assert_eq!(ema.average(), 12.5, "(1-0.5)*20 + 0.5*5");
        assert_eq!(ema.bias(), 0.5);
    }

    /// `resetAverage` takes the value outright, with the documented 0
    /// default.
    #[test]
    fn reset_average_takes_the_value() {
        let mut ema = Ema::new(0.5, 3.0);
        ema.reset_average(7.0);
        assert_eq!(ema.average(), 7.0);
        ema.reset_average(0.0);
        assert_eq!(ema.average(), 0.0);
    }

    /// `highWaterSample` overrides on the way up and folds on the way down.
    #[test]
    fn high_water_sample_overrides_only_when_larger() {
        let mut ema = Ema::new(0.5, 0.0);
        ema.high_water_sample(4.0);
        assert_eq!(ema.average(), 4.0, "higher than the average");
        ema.high_water_sample(2.0);
        assert_eq!(ema.average(), 3.0, "(1-0.5)*2 + 0.5*4");
    }

    /// `AccumulatingEMA::sample` folds the accumulated total and clears it;
    /// `shouldReset = false` keeps it.
    #[test]
    fn sample_folds_the_accumulation_and_resets_it() {
        let mut accum = AccumulatingEma::new(0.5, 0.0, 0u32);
        accum.increment_value();
        accum.increment_value();
        accum.add_to_value(8u32);
        assert_eq!(accum.value(), 10);

        accum.sample(true);
        assert_eq!(accum.value(), 0, "reset after sampling");
        assert_eq!(accum.average(), 5.0, "(1-0.5)*10 + 0.5*0");

        accum.add_to_value(4u32);
        accum.sample(false);
        assert_eq!(accum.value(), 4, "not reset");
        assert_eq!(accum.average(), 4.5, "(1-0.5)*4 + 0.5*5");
    }

    /// The interface statistics bias comes from 5 samples at the default
    /// weighting; this is the constant `InterfaceElementWithStats` starts
    /// its averages from.
    #[test]
    fn the_five_sample_bias_is_the_default_weighting() {
        let bias = Ema::calculate_bias_from_num_samples(5.0, 0.95);
        let mut ema = Ema::new(bias, 0.0);
        ema.sample(100.0);
        // First sample with a zero prior: (1 - bias) * 100.
        assert_eq!(ema.average(), (1.0 - bias) * 100.0);
    }

    /// The counters the reference accumulates are not only `uint`: wide
    /// counters and a float counter convert and increment the same way.
    #[test]
    fn wide_and_float_counters_accumulate_like_the_uint_one() {
        let mut wide = AccumulatingEma::new(0.5, 0.0, 0u64);
        wide.increment_value();
        wide.add_to_value(9u64);
        assert_eq!(wide.value(), 10u64);
        wide.sample(true);
        assert_eq!(wide.value(), 0u64);
        assert_eq!(wide.average(), 5.0);

        let mut float = AccumulatingEma::new(0.5, 0.0, 0f32);
        float.increment_value();
        float.add_to_value(8.0f32);
        assert_eq!(float.value(), 9.0f32);
        float.sample(true);
        assert_eq!(float.value(), 0.0f32);
        assert_eq!(float.average(), 4.5);
    }
}
