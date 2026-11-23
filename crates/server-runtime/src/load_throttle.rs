//! BW-08-06-04: load throttling.
//!
//! A CellApp that runs over its tick budget does not just fall behind — it
//! is measured and throttled. Each tick, the app splits its time into
//! persistent load (the recurring work), transient load (the one-off work,
//! such as slowly loading chunks) and spare time; the persistent share is
//! smoothed into the load value it reports to the CellAppMgr, compensated
//! for what the emergency throttle has already suppressed. When the app
//! runs behind or its spare time runs out, the emergency throttle scales
//! its work back towards a floor, and scales forward again once healthy.
//! Ticks that take too long, exceed the timeout, or report impossible
//! timings raise warnings.
//!
//! The acceptance, "慢加载及超预算 Tick 有可对照结果", is that both a
//! slow-loading tick and an over-budget tick leave comparable, reproducible
//! results: the same inputs produce the same throttle value, load values
//! and warnings, with transient loading tracked apart from persistent load.
//!
//! Reference:
//! - `emergency_throttle.hpp:12-37`, `emergency_throttle.cpp:23-129` —
//!   `EmergencyThrottle`: the value starts at 1 and moves by
//!   `(1 - min) * tickPeriod / scaleTime` per tick; scaling back triggers
//!   on `numSecondsBehind > behindThreshold` or `spareTime <
//!   spareTimeThreshold` (`:56-61`); `estimatePersistentLoadTime` adds the
//!   suppressed share `(1/value - 1) * throttledLoadTime` (`:121-129`).
//! - `throttle_config.cpp:23-27` — the option defaults: `behindThreshold`
//!   0.1, `spareTimeThreshold` 0.001, the scale times 1, `min` 0.1.
//! - `cellapp.cpp:1268-1300` `CellApp::updateLoad` —
//!   `persistentLoadTime = tickTime - spareTime - transientLoadTime`
//!   (`:1280`), the estimate compensates the persistent share
//!   (`:1282-1284`), the throttle updates on the seconds behind and the
//!   spare time (`:1286-1287`), and `addToLoad` smooths each share into its
//!   load value (`:1262-1265`) with `loadSmoothingBias` 0.05
//!   (`cellapp_config.cpp:28`).
//! - `cellapp.cpp:1161-1222` `CellApp::checkTickWarnings` — a tick longer
//!   than two tick periods warns (`:1167-1175`), a tick past
//!   `cellAppMgr/cellAppTimeout` is critical (`:1177-1190`), and a
//!   negative persistent share or a tick shorter than it is invalid
//!   timing (`:1206-1221`).
//! - `cellapp.cpp:1238-1250` `numSecondsBehind` — the seconds past the
//!   tick's due time, the measure of running late.

/// The `isEqual` epsilon of `cstdmf` (`mathdef.hpp`), the tolerance the
/// throttle compares values at.
pub const EPSILON: f64 = 0.0001;

fn is_equal(a: f64, b: f64) -> bool {
    (a - b).abs() < EPSILON
}

/// The throttle options (`ThrottleConfig`, `throttle_config.hpp:15`) with
/// the defaults of `throttle_config.cpp:23-27`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThrottleConfig {
    /// Seconds behind that triggers scaling back (`behindThreshold`).
    pub behind_threshold: f64,
    /// Spare time below which scaling back triggers (`spareTimeThreshold`).
    pub spare_time_threshold: f64,
    /// The time scale of scaling forward (`scaleForwardTime`).
    pub scale_forward_time: f64,
    /// The time scale of scaling back (`scaleBackTime`).
    pub scale_back_time: f64,
    /// The floor the value scales back to (`min`).
    pub min: f64,
}

impl Default for ThrottleConfig {
    /// `throttle_config.cpp:23-27`.
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

/// What one throttle update did, the logged transitions of
/// `EmergencyThrottle::update` (`emergency_throttle.cpp:36-76`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleChange {
    /// The value stayed put — already at the end it was moving towards.
    Unchanged,
    /// Scaled back, not yet at the floor.
    ScaledBack,
    /// Reached the floor — "Fully scaled back" (`:50-55`).
    FullyBack,
    /// Scaled forward, not yet at 1.
    ScaledForward,
    /// Reached 1 — "Fully scaled forward" (`:60-73`).
    FullyForward,
}

/// A tick warning of `CellApp::checkTickWarnings`
/// (`cellapp.cpp:1161-1222`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TickWarning {
    /// The last tick took more than two tick periods
    /// (`cellapp.cpp:1167-1175`).
    TickTooLong { tick_period: f64 },
    /// The last tick passed the timeout — the process should already have
    /// been stopped (`cellapp.cpp:1177-1190`).
    TimedOut { tick_period: f64, timeout: f64 },
    /// The timing result is impossible: a negative persistent share or a
    /// tick shorter than it (`cellapp.cpp:1206-1221`).
    InvalidTiming { tick_period: f64 },
}

/// `EmergencyThrottle` (`emergency_throttle.hpp:12`): the value between
/// `min` and 1 that the app scales its work by.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmergencyThrottle {
    value: f64,
}

impl Default for EmergencyThrottle {
    /// `emergency_throttle.cpp:23-30`: the value starts at 1.
    fn default() -> Self {
        Self::new()
    }
}

impl EmergencyThrottle {
    /// A throttle at full value.
    pub fn new() -> Self {
        EmergencyThrottle { value: 1.0 }
    }

    /// The current value (`value()`).
    pub fn value(&self) -> f64 {
        self.value
    }

    /// `EmergencyThrottle::update` (`emergency_throttle.cpp:36`): scale
    /// back when behind or out of spare time (`:56-61`), otherwise forward,
    /// by `(1 - min) * tickPeriod / scaleTime` per update
    /// (`:82-100`, `:104-118`).
    pub fn update(
        &mut self,
        behind: f64,
        spare_time: f64,
        tick_period: f64,
        config: &ThrottleConfig,
    ) -> ThrottleChange {
        if self.should_scale_back(behind, spare_time, config) {
            self.scale_back(tick_period / config.scale_back_time, config)
        } else {
            self.scale_forward(tick_period / config.scale_forward_time, config)
        }
    }

    /// `shouldScaleBack` (`emergency_throttle.cpp:56-61`).
    fn should_scale_back(&self, behind: f64, spare_time: f64, config: &ThrottleConfig) -> bool {
        behind > config.behind_threshold || spare_time < config.spare_time_threshold
    }

    /// `scaleBack` (`emergency_throttle.cpp:63-83`): step towards the floor,
    /// reporting the arrival.
    fn scale_back(&mut self, fraction: f64, config: &ThrottleConfig) -> ThrottleChange {
        if self.value <= config.min {
            // Already fully scaled back.
            return ThrottleChange::Unchanged;
        }

        self.value = (config.min).max(self.value - (1.0 - config.min) * fraction);
        if is_equal(self.value, config.min) {
            ThrottleChange::FullyBack
        } else {
            ThrottleChange::ScaledBack
        }
    }

    /// `scaleForward` (`emergency_throttle.cpp:88-108`): step back towards
    /// 1, reporting the arrival.
    fn scale_forward(&mut self, fraction: f64, config: &ThrottleConfig) -> ThrottleChange {
        if is_equal(self.value, 1.0) {
            // Already fully scaled forward.
            return ThrottleChange::Unchanged;
        }

        self.value = (self.value + (1.0 - config.min) * fraction).min(1.0);
        if is_equal(self.value, 1.0) {
            ThrottleChange::FullyForward
        } else {
            ThrottleChange::ScaledForward
        }
    }

    /// `estimatePersistentLoadTime` (`emergency_throttle.cpp:113-129`):
    /// what the persistent load would be without the throttling — the
    /// suppressed share added back.
    pub fn estimate_persistent_load_time(&self, persistent: f64, throttled: f64) -> f64 {
        persistent + (1.0 / self.value - 1.0) * throttled
    }
}

/// One CellApp's load bookkeeping and throttle (`CellApp::updateLoad`,
/// `cellapp.cpp:1268-1300`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadTracker {
    throttle: EmergencyThrottle,
    config: ThrottleConfig,
    /// `Config::updateHertz()`, ticks per second.
    update_hertz: f64,
    /// `Config::loadSmoothingBias()` (`cellapp_config.cpp:28`).
    load_smoothing_bias: f64,
    /// `cellAppMgr/cellAppTimeout`, the critical tick length.
    timeout_period: f64,
    /// `totalAddedLoad_`, the artificial load added by hand
    /// (`CellApp::addLoad`).
    artificial_load: f64,
    /// `persistentLoad_`.
    persistent_load: f64,
    /// `transientLoad_`.
    transient_load: f64,
    /// `totalLoad_`.
    total_load: f64,
}

impl Default for LoadTracker {
    /// A 10 Hz app with the default options.
    fn default() -> Self {
        Self::new(10.0)
    }
}

impl LoadTracker {
    /// An app ticking at `update_hertz` with the default throttle options
    /// and a 30-second timeout.
    pub fn new(update_hertz: f64) -> Self {
        LoadTracker {
            throttle: EmergencyThrottle::new(),
            config: ThrottleConfig::default(),
            update_hertz,
            load_smoothing_bias: 0.05,
            timeout_period: 30.0,
            artificial_load: 0.0,
            persistent_load: 0.0,
            transient_load: 0.0,
            total_load: 0.0,
        }
    }

    /// The throttle.
    pub fn throttle(&self) -> &EmergencyThrottle {
        &self.throttle
    }

    /// The smoothed persistent load, as reported to the CellAppMgr
    /// (`persistentLoad_`).
    pub fn persistent_load(&self) -> f64 {
        self.persistent_load
    }

    /// The smoothed transient load (`transientLoad_`).
    pub fn transient_load(&self) -> f64 {
        self.transient_load
    }

    /// The smoothed total load (`totalLoad_`).
    pub fn total_load(&self) -> f64 {
        self.total_load
    }

    /// Add artificial load, counted in ticks' worth
    /// (`CellApp::addLoad`, `cellapp.cpp:1295-1296`).
    pub fn add_artificial_load(&mut self, load: f64) {
        self.artificial_load += load;
    }

    /// `CellApp::updateLoad` (`cellapp.cpp:1268`) plus
    /// `checkTickWarnings` (`:1219`): one tick's times — the whole tick,
    /// the spare idle time, the transient one-off work (the slow loading)
    /// and the throttled work — and the seconds the app runs behind,
    /// folded into the load values, the throttle and the warnings.
    pub fn tick(
        &mut self,
        tick_time: f64,
        spare_time: f64,
        transient_load_time: f64,
        throttled_load_time: f64,
        seconds_behind: f64,
    ) -> Vec<TickWarning> {
        // `cellapp.cpp:1280`.
        let persistent_load_time = tick_time - spare_time - transient_load_time;

        // The estimate compensates for what the throttle suppressed
        // (`cellapp.cpp:1282-1284`).
        let estimated_persistent_load_time = self
            .throttle
            .estimate_persistent_load_time(persistent_load_time, throttled_load_time);

        // The throttle sees the lateness and the spare time
        // (`cellapp.cpp:1286-1287`), with `expectedTickPeriod` the tick.
        let tick_period = 1.0 / self.update_hertz;
        self.throttle
            .update(seconds_behind, spare_time, tick_period, &self.config);

        let warnings = self.check_tick_warnings(persistent_load_time, tick_time);

        // `cellapp.cpp:1293-1300`: each share smoothed into its value.
        let artificial_time = self.artificial_load / self.update_hertz;
        self.add_to_load(
            estimated_persistent_load_time + artificial_time,
            LoadShare::Persistent,
        );
        self.add_to_load(transient_load_time, LoadShare::Transient);
        self.add_to_load(tick_time - spare_time, LoadShare::Total);

        warnings
    }

    /// `CellApp::checkTickWarnings` (`cellapp.cpp:1161`).
    fn check_tick_warnings(&self, persistent_load_time: f64, tick_time: f64) -> Vec<TickWarning> {
        let mut warnings = Vec::new();

        // More than two tick periods (`cellapp.cpp:1167-1175`).
        if tick_time * self.update_hertz > 2.0 {
            warnings.push(TickWarning::TickTooLong {
                tick_period: tick_time,
            });
        }

        // Past the timeout (`cellapp.cpp:1177-1190`).
        if tick_time > self.timeout_period {
            warnings.push(TickWarning::TimedOut {
                tick_period: tick_time,
                timeout: self.timeout_period,
            });
        }

        // An impossible timing result (`cellapp.cpp:1206-1221`).
        if persistent_load_time < 0.0 || tick_time < persistent_load_time {
            warnings.push(TickWarning::InvalidTiming {
                tick_period: tick_time,
            });
        }

        warnings
    }

    /// `CellApp::addToLoad` (`cellapp.cpp:1258-1265`): the smoothing
    /// `result = (1 - bias) * result + bias * time * updateHertz`.
    fn add_to_load(&mut self, time_spent: f64, share: LoadShare) {
        let bias = self.load_smoothing_bias;
        let smoothed = (1.0 - bias) * share.of(self) + bias * time_spent * self.update_hertz;
        share.set(self, smoothed);
    }
}

/// The three load values `updateLoad` keeps (`cellapp.cpp:1293-1300`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadShare {
    Persistent,
    Transient,
    Total,
}

impl LoadShare {
    fn of(self, tracker: &LoadTracker) -> f64 {
        match self {
            LoadShare::Persistent => tracker.persistent_load,
            LoadShare::Transient => tracker.transient_load,
            LoadShare::Total => tracker.total_load,
        }
    }

    fn set(self, tracker: &mut LoadTracker, value: f64) {
        match self {
            LoadShare::Persistent => tracker.persistent_load = value,
            LoadShare::Transient => tracker.transient_load = value,
            LoadShare::Total => tracker.total_load = value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance, first half: an over-budget tick leaves a comparable,
    /// reproducible result.
    #[test]
    fn over_budget_ticks_leave_comparable_results() {
        let run = || {
            let mut tracker = LoadTracker::new(10.0);
            // A quarter-second tick at 10 Hz with nothing spare: over
            // budget and behind.
            let warnings = tracker.tick(0.25, 0.0, 0.0, 0.03, 0.2);
            (
                warnings,
                tracker.throttle().value(),
                tracker.persistent_load(),
                tracker.total_load(),
            )
        };

        let (warnings, value, persistent, total) = run();
        let (warnings2, value2, persistent2, total2) = run();
        assert_eq!(
            (value, persistent, total),
            (value2, persistent2, total2),
            "the same inputs give the same results"
        );
        assert_eq!(warnings, warnings2);

        // The tick ran at 2.5 tick periods: the too-long warning.
        assert_eq!(
            warnings,
            vec![TickWarning::TickTooLong { tick_period: 0.25 }]
        );

        // The throttle stepped back from 1 by (1 - 0.1) * 0.1.
        assert!((value - 0.91).abs() < EPSILON, "value {value}");

        // The estimate runs before the throttle update
        // (`cellapp.cpp:1282-1287`), so the suppressed share is still zero.
        assert!(
            (persistent - 0.05 * 0.25 * 10.0).abs() < EPSILON,
            "persistent {persistent}"
        );
        assert!(
            (total - 0.05 * 0.25 * 10.0).abs() < EPSILON,
            "total {total}"
        );
    }

    /// The acceptance, second half: a slowly loading tick keeps the loading
    /// apart from the persistent load, unlike the same tick without it.
    #[test]
    fn slow_loading_stays_transient() {
        let mut loading = LoadTracker::new(10.0);
        // A quarter-second tick, 0.15 of it spent loading chunks.
        loading.tick(0.25, 0.0, 0.15, 0.0, 0.2);

        let mut steady = LoadTracker::new(10.0);
        // The same tick with the loading counted as persistent work.
        steady.tick(0.25, 0.0, 0.0, 0.0, 0.2);

        assert!(
            loading.persistent_load() < steady.persistent_load(),
            "the loading does not inflate the persistent load"
        );
        assert!(
            loading.transient_load() > steady.transient_load(),
            "the loading shows up as transient load"
        );
        assert_eq!(loading.total_load(), steady.total_load());
        assert!((loading.transient_load() - 0.05 * 0.15 * 10.0).abs() < EPSILON);
    }

    /// Scaling back floors at `min` and reports fully back
    /// (`emergency_throttle.cpp:50-55`).
    #[test]
    fn scaling_back_floors_at_the_minimum() {
        let config = ThrottleConfig::default();
        let mut throttle = EmergencyThrottle::new();

        let mut changes = Vec::new();
        for _ in 0..30 {
            changes.push(throttle.update(0.2, 0.0, 0.1, &config));
        }
        assert!(changes.contains(&ThrottleChange::FullyBack));
        assert_eq!(
            changes.last(),
            Some(&ThrottleChange::Unchanged),
            "further over-budget ticks leave it at the floor"
        );
        assert!((throttle.value() - 0.1).abs() < EPSILON);

        // Further over-budget ticks leave it there, unchanged.
        assert_eq!(
            throttle.update(0.5, 0.0, 0.1, &config),
            ThrottleChange::Unchanged
        );
        assert!((throttle.value() - 0.1).abs() < EPSILON);
    }

    /// Scaling forward returns to 1 and reports fully forward
    /// (`emergency_throttle.cpp:60-73`).
    #[test]
    fn scaling_forward_returns_to_one() {
        let config = ThrottleConfig::default();
        let mut throttle = EmergencyThrottle::new();

        // Out of spare time: back off three steps to 0.73.
        throttle.update(0.0, 0.0, 0.1, &config);
        throttle.update(0.0, 0.0, 0.1, &config);
        throttle.update(0.0, 0.0, 0.1, &config);
        assert!((throttle.value() - 0.73).abs() < EPSILON);

        // Healthy again: three forward steps of 0.09 reach 1.
        let mut fully_at = None;
        for tick in 0..12 {
            if throttle.update(0.0, 0.05, 0.1, &config) == ThrottleChange::FullyForward {
                fully_at = Some(tick);
                break;
            }
        }
        assert_eq!(fully_at, Some(2), "three forward steps from 0.73 to 1");
        assert!((throttle.value() - 1.0).abs() < EPSILON);

        // Already at 1: no further movement.
        assert_eq!(
            throttle.update(0.0, 0.05, 0.1, &config),
            ThrottleChange::Unchanged
        );
    }

    /// The estimate adds the suppressed share back
    /// (`emergency_throttle.cpp:121-129`).
    #[test]
    fn the_estimate_compensates_the_throttled_share() {
        let mut throttle = EmergencyThrottle::new();
        assert_eq!(throttle.estimate_persistent_load_time(0.05, 0.03), 0.05);

        // At half value, the throttled work counts double.
        let config = ThrottleConfig::default();
        for _ in 0..30 {
            throttle.update(0.2, 0.0, 0.1, &config);
        }
        assert!((throttle.value() - 0.1).abs() < EPSILON);
        // 1/0.1 - 1 = 9 times the throttled time.
        assert!((throttle.estimate_persistent_load_time(0.05, 0.01) - 0.14).abs() < EPSILON);
    }

    /// The artificial load is folded into the persistent share in tick
    /// units (`cellapp.cpp:1293-1296`).
    #[test]
    fn artificial_load_folds_into_the_persistent_share() {
        let mut tracker = LoadTracker::new(10.0);
        tracker.add_artificial_load(2.0);

        let warnings = tracker.tick(0.1, 0.05, 0.0, 0.0, 0.0);
        assert!(warnings.is_empty(), "a healthy tick warns nothing");

        // 0.2s of artificial time on top of the 0.05s persistent share.
        assert!((tracker.persistent_load() - 0.05 * 0.25 * 10.0).abs() < EPSILON);
        assert!((tracker.total_load() - 0.05 * 0.05 * 10.0).abs() < EPSILON);
    }

    /// The impossible-timing warnings: a negative persistent share and a
    /// tick shorter than its persistent share
    /// (`cellapp.cpp:1206-1221`).
    #[test]
    fn impossible_timings_warn() {
        let mut tracker = LoadTracker::new(10.0);

        // More spare time than tick time: a negative persistent share.
        let warnings = tracker.tick(0.1, 0.2, 0.0, 0.0, 0.0);
        assert_eq!(
            warnings,
            vec![TickWarning::InvalidTiming { tick_period: 0.1 }]
        );

        // A quarter-second tick whose transient share alone overflows it.
        let warnings = tracker.tick(0.25, 0.0, 0.3, 0.0, 0.0);
        assert_eq!(
            warnings,
            vec![
                TickWarning::TickTooLong { tick_period: 0.25 },
                TickWarning::InvalidTiming { tick_period: 0.25 },
            ][..]
        );
    }

    /// The defaults: a throttle at 1, a 10 Hz tracker.
    #[test]
    fn the_defaults() {
        let throttle = EmergencyThrottle::default();
        assert!((throttle.value() - 1.0).abs() < EPSILON);

        let tracker = LoadTracker::default();
        assert_eq!(tracker.persistent_load(), 0.0);
        assert_eq!(tracker.total_load(), 0.0);
        assert_eq!(tracker.transient_load(), 0.0);
    }

    /// A tick past the timeout is critical (`cellapp.cpp:1177-1190`).
    #[test]
    fn a_timed_out_tick_is_critical() {
        let mut tracker = LoadTracker::new(10.0);
        let warnings = tracker.tick(30.5, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(
            warnings,
            vec![
                TickWarning::TickTooLong { tick_period: 30.5 },
                TickWarning::TimedOut {
                    tick_period: 30.5,
                    timeout: 30.0
                },
            ][..]
        );
    }
}
