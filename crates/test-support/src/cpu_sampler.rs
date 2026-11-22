//! BW-23-02-02: CPU sampling.
//!
//! A sampler reads the profiler's current call stacks on a fixed period and
//! aggregates the samples by stack signature, so a fixed load's hot spot —
//! the stack the time went to — is located. Sampling happens when the caller
//! drives the shared clock, which keeps the deterministic tests exact.

use foundation::profiler::{Profiler, StackSample};

/// One stack signature aggregated over samples: the sampled stack and how
/// many samples landed on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotStack {
    /// The sampled stack, outermost section first.
    pub stack: Vec<String>,
    /// How many samples landed on this exact stack.
    pub samples: u32,
}

/// A periodic CPU sampler over one or more profilers.
pub struct CpuSampler<'a> {
    profilers: Vec<&'a Profiler>,
    /// Stamps between samples.
    period: u64,
    /// The shared clock the sampler samples on; the same source the profiled
    /// load advances.
    clock: Box<dyn Fn() -> u64>,
    /// The stamp the next sample is due at.
    next_due: u64,
}

impl<'a> CpuSampler<'a> {
    /// A sampler taking one sample every `period` stamps on `clock`.
    pub fn with_clock(
        profilers: &[&'a Profiler],
        period: u64,
        clock: impl Fn() -> u64 + 'static,
    ) -> Self {
        assert!(period > 0, "sampling period must be positive");
        Self {
            profilers: profilers.to_vec(),
            period,
            clock: Box::new(clock),
            next_due: period,
        }
    }

    /// Take the samples due at the current clock: one per `period` stamps
    /// since the last due sample, from every profiler. The caller advances
    /// the load and calls this as the clock moves, so each sample reads the
    /// stacks open at its due instant.
    pub fn collect_due(&mut self, samples: &mut Vec<StackSample>) {
        while self.next_due <= (self.clock)() {
            self.next_due += self.period;
            for profiler in &self.profilers {
                samples.extend(profiler.sample_stacks());
            }
        }
    }
}

/// Aggregate samples into hot stacks: one entry per (thread, stack)
/// signature with its sample count, most-sampled first.
pub fn aggregate(samples: &[StackSample]) -> Vec<HotStack> {
    use std::collections::HashMap;

    let mut counts: HashMap<(u32, Vec<String>), u32> = HashMap::new();
    for sample in samples {
        *counts
            .entry((sample.thread, sample.stack.clone()))
            .or_insert(0) += 1;
    }
    let mut hot: Vec<HotStack> = counts
        .into_iter()
        .map(|((_, stack), samples)| HotStack { stack, samples })
        .collect();
    hot.sort_by(|a, b| b.samples.cmp(&a.samples).then(a.stack.cmp(&b.stack)));
    hot
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    type Pinned = Arc<AtomicU64>;

    fn pinned() -> Pinned {
        Arc::new(AtomicU64::new(0))
    }

    fn profiler_with(source: &Pinned) -> Profiler {
        let source = source.clone();
        Profiler::with_clock(move || source.load(Ordering::SeqCst))
    }

    fn advance(source: &Pinned, by: u64) {
        source.fetch_add(by, Ordering::SeqCst);
    }

    #[test]
    fn a_fixed_load_locates_its_hot_spot() {
        let source = pinned();
        let prof = profiler_with(&source);
        let sampler_clock = source.clone();
        let mut sampler =
            CpuSampler::with_clock(&[&prof], 2, move || sampler_clock.load(Ordering::SeqCst));

        // The fixed load: `hot` is busy for 9 stamps, then a 1-stamp `cold`
        // blip, ten times. Samples fall every 2 stamps, so four land in the
        // hot window for each one that lands in the cold blip.
        let mut samples = Vec::new();
        for _ in 0..10 {
            let _hot = prof.scoped("hot");
            for _ in 0..9 {
                advance(&source, 1);
                sampler.collect_due(&mut samples);
            }
            {
                let _cold = prof.scoped("cold");
                advance(&source, 1);
                sampler.collect_due(&mut samples);
            }
        }

        // 100 stamps / period 2 = 50 samples; every sample saw `hot` open,
        // and only the cold blips stacked `cold` on top.
        assert_eq!(samples.len(), 50);
        let hot = aggregate(&samples);
        assert_eq!(hot.len(), 2, "two distinct stacks sampled: {hot:?}");
        assert_eq!(hot[0].stack, vec!["hot".to_string()]);
        assert_eq!(hot[0].samples, 40);
        assert_eq!(hot[1].stack, vec!["hot".to_string(), "cold".to_string()]);
        assert_eq!(hot[1].samples, 10);

        // The hot spot: the heavy section's stack tops the ranking, and the
        // cold blip is nowhere near it.
        let top = &hot[0];
        assert_eq!(top.stack.last().map(String::as_str), Some("hot"));
        assert!(top.samples > hot[1].samples);
    }

    #[test]
    fn an_idle_load_samples_nothing() {
        let source = pinned();
        let prof = profiler_with(&source);
        let sampler_clock = source.clone();
        let mut sampler =
            CpuSampler::with_clock(&[&prof], 5, move || sampler_clock.load(Ordering::SeqCst));

        // Nothing open: every due sample sees empty stacks and records none.
        let mut samples = Vec::new();
        advance(&source, 20);
        sampler.collect_due(&mut samples);
        assert!(samples.is_empty(), "{samples:?}");
        assert!(aggregate(&samples).is_empty());
    }

    #[test]
    fn nested_loads_attribute_samples_to_their_full_stack() {
        let source = pinned();
        let prof = profiler_with(&source);
        let sampler_clock = source.clone();
        let mut sampler =
            CpuSampler::with_clock(&[&prof], 10, move || sampler_clock.load(Ordering::SeqCst));

        // A three-deep nesting; the due sample reads the whole stack.
        let mut samples = Vec::new();
        {
            let _outer = prof.scoped("outer");
            advance(&source, 10);
            sampler.collect_due(&mut samples);
            let _inner = prof.scoped("inner");
            advance(&source, 10);
            sampler.collect_due(&mut samples);
        }

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].stack, vec!["outer".to_string()]);
        assert_eq!(
            samples[1].stack,
            vec!["outer".to_string(), "inner".to_string()]
        );
    }
}
