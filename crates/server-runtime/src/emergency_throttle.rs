//! The emergency throttle: what a cellapp does when ticks run late or
//! spare time dries up — it scales the per-witness update budget down,
//! then back up once the pressure clears.
//!
//! Ported from `cellapp/emergency_throttle.cpp` and
//! `cellapp/throttle_config.cpp` (the `throttle/` options). The consumer
//! is `Witness::update` (`witness.cpp:1104-1105`): the throttle value
//! multiplies `maxPacketSize_` — see the witness side in `witness.rs`.
//!
//! Float notes: the reference's `isEqual(float)` is exact equality
//! (`stdmf_minimal.hpp:119`), so the bottoms-out/full-forward checks
//! rely on the `max`/`min` clamps landing on the boundary values
//! exactly. The port reproduces the arithmetic op-for-op in f32.

/// `ThrottleConfig` — the `cellApp/throttle/` server options.
#[derive(Clone, Debug)]
pub struct ThrottleConfig {
    /// `throttle/behindThreshold` — running this many seconds behind
    /// trips the throttle (strictly greater).
    pub behind_threshold: f32,
    /// `throttle/spareTimeThreshold` — less spare time than this trips
    /// the throttle (strictly less).
    pub spare_time_threshold: f32,
    /// `throttle/scaleForwardTime` — seconds from fully scaled back to
    /// fully scaled forward.
    pub scale_forward_time: f32,
    /// `throttle/scaleBackTime` — seconds the other way.
    pub scale_back_time: f32,
    /// `throttle/min` — the floor the value scales back to.
    pub min: f32,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        ThrottleConfig {
            behind_threshold: 0.1,
            spare_time_threshold: 0.001,
            scale_forward_time: 1.0,
            scale_back_time: 1.0,
            min: 0.1,
        }
    }
}

/// The ERROR_MSG / NOTICE_MSG records the reference logs.
#[derive(Clone, Debug, PartialEq)]
pub enum ThrottleEvent {
    /// `Fully scaled back` — the value reached `min` while behind.
    FullyScaledBack,
    /// `Fully scaled forward` — a dive recovered; the stamps are the
    /// dive's length (`timestamp() - startTime_`) and `max_behind` the
    /// worst lag seen during it.
    FullyScaledForward {
        scaled_back_for_stamps: u64,
        max_behind: f32,
    },
}

/// The throttling state machine.
#[derive(Debug)]
pub struct EmergencyThrottle {
    /// `value_` — 1.0 is no throttling, `min` is the floor.
    value: f32,
    /// `shouldPrintScaleForward_` — set when a dive bottoms out, so the
    /// recovery gets the summary notice.
    should_print_scale_forward: bool,
    /// `startTime_` — CPU stamps when the last dive began.
    start_time: u64,
    /// `maxTimeBehind_` — the worst lag since the last full recovery.
    max_time_behind: f32,
    /// The logged messages, in order.
    pub events: Vec<ThrottleEvent>,
}

impl Default for EmergencyThrottle {
    fn default() -> Self {
        EmergencyThrottle {
            value: 1.0,
            should_print_scale_forward: false,
            start_time: 0,
            max_time_behind: 0.0,
            events: Vec::new(),
        }
    }
}

impl EmergencyThrottle {
    /// `EmergencyThrottle::update` (`emergency_throttle.cpp:29-62`).
    /// `now_stamps` stands in for the reference's `timestamp()` calls.
    pub fn update(
        &mut self,
        now_stamps: u64,
        num_seconds_behind: f32,
        spare_time: f32,
        tick_period: f32,
        config: &ThrottleConfig,
    ) {
        self.max_time_behind = self.max_time_behind.max(num_seconds_behind);

        if self.should_scale_back(num_seconds_behind, spare_time, config) {
            if self.scale_back(tick_period / config.scale_back_time, now_stamps, config) {
                // Fully scaled back.
                self.events.push(ThrottleEvent::FullyScaledBack);
                self.should_print_scale_forward = true;
            }
        } else if self.scale_forward(tick_period / config.scale_forward_time, config) {
            if self.should_print_scale_forward {
                self.events.push(ThrottleEvent::FullyScaledForward {
                    scaled_back_for_stamps: now_stamps - self.start_time,
                    max_behind: self.max_time_behind,
                });
                self.should_print_scale_forward = false;
            }
            self.max_time_behind = 0.0;
        }
    }

    /// `EmergencyThrottle::value`.
    pub fn value(&self) -> f32 {
        self.value
    }

    /// The worst lag since the last full recovery.
    pub fn max_time_behind(&self) -> f32 {
        self.max_time_behind
    }

    /// `EmergencyThrottle::shouldScaleBack` — either tripping condition
    /// is strict.
    fn should_scale_back(
        &self,
        num_seconds_behind: f32,
        spare_time: f32,
        config: &ThrottleConfig,
    ) -> bool {
        (num_seconds_behind > config.behind_threshold) || (spare_time < config.spare_time_threshold)
    }

    /// `EmergencyThrottle::scaleBack` — returns whether the floor was
    /// just reached. The reference reads `timestamp()` for `startTime_`;
    /// the harness clock rides in as `now_stamps`.
    fn scale_back(&mut self, fraction: f32, now_stamps: u64, config: &ThrottleConfig) -> bool {
        if self.value <= config.min {
            // Already fully scaled back.
            return false;
        }

        if self.value == 1.0 {
            // Starting to scale back.
            self.start_time = now_stamps;
        }

        let min_val = config.min;
        self.value = (min_val).max(self.value - (1.0 - min_val) * fraction);
        self.value == min_val
    }

    /// `EmergencyThrottle::scaleForward` — returns whether full value
    /// was just restored.
    fn scale_forward(&mut self, fraction: f32, config: &ThrottleConfig) -> bool {
        if self.value == 1.0 {
            // Already fully scaled forward.
            return false;
        }

        self.value = 1.0f32.min(self.value + (1.0 - config.min) * fraction);
        self.value == 1.0
    }

    /// `EmergencyThrottle::estimatePersistentLoadTime` — what the
    /// persistent load would be without the throttling that stretched
    /// the throttled work over more ticks.
    pub fn estimate_persistent_load_time(
        &self,
        persistent_load_time: f32,
        throttled_load_time: f32,
    ) -> f32 {
        persistent_load_time + (1.0 / self.value - 1.0) * throttled_load_time
    }
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ThrottleConfig {
        ThrottleConfig::default()
    }

    #[test]
    fn an_overrun_tick_scales_back_by_the_tick_fraction() {
        let cfg = config();
        let mut throttle = EmergencyThrottle::default();

        // 0.2 s behind the 0.1 s threshold, tick period 0.1 s against a
        // 1 s scale-back time: one step is a tenth of the range.
        throttle.update(100, 0.2, 0.5, 0.1, &cfg);
        let want = 1.0f32 - (1.0 - cfg.min) * 0.1;
        assert_eq!(throttle.value(), want);
        assert!(throttle.events.is_empty());
    }

    #[test]
    fn sustained_overrun_bottoms_out_and_reports_it_once() {
        let cfg = config();
        let mut throttle = EmergencyThrottle::default();

        let mut step = 1.0f32;
        for tick in 0..12u64 {
            throttle.update(tick, 0.5, 0.5, 0.1, &cfg);
            if throttle.value() <= cfg.min {
                break;
            }
            let want = (cfg.min).max(step - (1.0 - cfg.min) * 0.1);
            assert_eq!(throttle.value(), want);
            step = throttle.value();
        }
        // The max clamp landed exactly on the floor.
        assert_eq!(throttle.value(), cfg.min);
        assert_eq!(throttle.events, vec![ThrottleEvent::FullyScaledBack]);

        // Further overruns hold the floor and stay quiet.
        for tick in 12..20u64 {
            throttle.update(tick, 0.5, 0.5, 0.1, &cfg);
        }
        assert_eq!(throttle.value(), cfg.min);
        assert_eq!(throttle.events.len(), 1);
    }

    #[test]
    fn recovery_scales_forward_and_reports_the_dive() {
        let cfg = config();
        let mut throttle = EmergencyThrottle::default();

        // The dive starts at stamp 0.
        for tick in 0..12u64 {
            throttle.update(tick, 0.5, 0.5, 0.1, &cfg);
        }
        assert_eq!(throttle.events, vec![ThrottleEvent::FullyScaledBack]);
        assert_eq!(throttle.max_time_behind(), 0.5);

        // Pressure clears at stamp 100: steps of 0.09 climb from the
        // floor, and f32 accumulation puts full value on the 11th tick.
        // The notice carries the dive length.
        for tick in 100..=110u64 {
            throttle.update(tick, 0.01, 0.5, 0.1, &cfg);
        }
        assert_eq!(throttle.value(), 1.0);
        assert_eq!(
            throttle.events,
            vec![
                ThrottleEvent::FullyScaledBack,
                ThrottleEvent::FullyScaledForward {
                    scaled_back_for_stamps: 110,
                    max_behind: 0.5
                }
            ]
        );
        // The worst-lag tracker resets with the recovery — then starts
        // re-accumulating the healthy ticks' small lags again (the max
        // runs before the scale decision).
        assert_eq!(throttle.max_time_behind(), 0.0);
        throttle.update(111, 0.01, 0.5, 0.1, &cfg);
        assert_eq!(throttle.max_time_behind(), 0.01);
    }

    #[test]
    fn spare_time_starvation_also_trips_the_throttle() {
        let cfg = config();
        let mut throttle = EmergencyThrottle::default();

        // Barely behind, but the spare time has collapsed below a
        // millisecond.
        throttle.update(10, 0.01, 0.0005, 0.1, &cfg);
        assert_eq!(throttle.value(), 1.0f32 - (1.0 - cfg.min) * 0.1);
    }

    #[test]
    fn the_thresholds_are_strict() {
        let cfg = config();
        let mut throttle = EmergencyThrottle::default();

        // Exactly at the behind threshold with ample spare time: no.
        throttle.update(10, cfg.behind_threshold, 0.5, 0.1, &cfg);
        assert_eq!(throttle.value(), 1.0);

        // Exactly at the spare-time threshold while behind nothing: no.
        throttle.update(11, 0.0, cfg.spare_time_threshold, 0.1, &cfg);
        assert_eq!(throttle.value(), 1.0);
        assert!(throttle.events.is_empty());
    }

    #[test]
    fn a_partial_dive_recovers_without_the_notice() {
        let cfg = config();
        let mut throttle = EmergencyThrottle::default();

        // One bad tick, then pressure clears: the value never bottoms
        // out, so no pair of notices — but the tracker still resets once
        // full value is back.
        throttle.update(10, 0.2, 0.5, 0.1, &cfg);
        assert!(throttle.value() < 1.0);
        for tick in 11..30u64 {
            throttle.update(tick, 0.0, 0.5, 0.1, &cfg);
        }
        assert_eq!(throttle.value(), 1.0);
        assert!(throttle.events.is_empty());
        assert_eq!(throttle.max_time_behind(), 0.0);
    }

    #[test]
    fn max_time_behind_tracks_the_worst_tick_of_the_dive() {
        let cfg = config();
        let mut throttle = EmergencyThrottle::default();

        throttle.update(10, 0.05, 0.5, 0.1, &cfg);
        assert_eq!(throttle.max_time_behind(), 0.05);
        throttle.update(11, 0.3, 0.5, 0.1, &cfg);
        assert_eq!(throttle.max_time_behind(), 0.3);
        // A mild tick starts the climb (a half-size step can't complete
        // it), and the tracker only resets on full recovery.
        throttle.update(12, 0.08, 0.5, 0.05, &cfg);
        assert!(throttle.value() < 1.0);
        assert_eq!(throttle.max_time_behind(), 0.3);
    }

    #[test]
    fn estimate_persistent_load_reports_the_unthrottled_load() {
        let mut throttle = EmergencyThrottle::default();
        // No throttling: the estimate is the persistent load itself.
        assert_eq!(throttle.estimate_persistent_load_time(2.0, 3.0), 2.0);

        // Half-throttled: the throttled work gets stretched across
        // twice the ticks. A 0.5 floor with a full-tick step lands the
        // value on exactly 0.5.
        let cfg = ThrottleConfig {
            min: 0.5,
            ..ThrottleConfig::default()
        };
        throttle.update(0, 0.5, 0.5, 1.0, &cfg);
        assert_eq!(throttle.value(), 0.5);
        assert_eq!(throttle.estimate_persistent_load_time(2.0, 3.0), 5.0);
    }

    #[test]
    fn the_throttle_shrinks_the_witness_budget() {
        // The consumer face: `Witness::update` multiplies maxPacketSize
        // by this value (`witness.cpp:1104-1105`), so a throttled app
        // hands its witnesses a smaller budget — the comparable result
        // the acceptance asks for.
        let cfg = ThrottleConfig {
            min: 0.5,
            ..ThrottleConfig::default()
        };
        let mut throttle = EmergencyThrottle::default();
        for _ in 0..20 {
            throttle.update(10, 0.5, 0.5, 0.1, &cfg);
        }
        assert_eq!(throttle.value(), 0.5);

        let max_packet_size = 1000i32;
        let unthrottled_desired = (max_packet_size as f32 * 1.0) as i32;
        let throttled_desired = (max_packet_size as f32 * throttle.value()) as i32;
        assert_eq!(unthrottled_desired, 1000);
        assert_eq!(throttled_desired, 500);
    }
}
