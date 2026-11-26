//! BW-08-06-04 — the emergency load throttle.
//!
//! Reference faces: `server/cellapp/emergency_throttle.{hpp,cpp}`,
//! `server/cellapp/throttle_config.{hpp,cpp}` (the option defaults),
//! `CellApp::updateLoad` (`cellapp.cpp:1270`), `CellApp::addToLoad`
//! (`cellapp.cpp:1258`), and `CellApp::checkTickWarnings`
//! (`cellapp.cpp:1167`). The value feeds the witness' desired packet
//! size (`witness.cpp:1104`) through
//! [`crate::cellapp_space::CellApp::emergency_throttle`].

use crate::cellapp_space::CellApp;

/// `ThrottleConfig::behindThreshold` (default 0.1): how many seconds
/// behind the tick delivery may run before scaling back.
pub const THROTTLE_BEHIND_THRESHOLD: f32 = 0.1;
/// `ThrottleConfig::spareTimeThreshold` (default 0.001): the spare-time
/// floor under which the app counts as overloaded.
pub const THROTTLE_SPARE_TIME_THRESHOLD: f32 = 0.001;
/// `ThrottleConfig::scaleForwardTime` (default 1.0): the seconds a full
/// recovery takes.
pub const THROTTLE_SCALE_FORWARD_TIME: f32 = 1.0;
/// `ThrottleConfig::scaleBackTime` (default 1.0): the seconds a full
/// scale-back takes.
pub const THROTTLE_SCALE_BACK_TIME: f32 = 1.0;
/// `ThrottleConfig::min` (default 0.1): the fully-scaled-back value.
pub const THROTTLE_MIN: f32 = 0.1;
/// `CellAppConfig::loadSmoothingBias` (default 0.05): the share of each
/// load sample the smoothed value adopts.
pub const LOAD_SMOOTHING_BIAS: f32 = 0.05;

#[derive(Debug)]
/// `EmergencyThrottle` (`emergency_throttle.hpp`): the load throttle
/// riding on the witness' outgoing packet budget. 1.0 is no throttle.
pub struct EmergencyThrottle {
    /// `value_`: the throttle (1.0 none, `THROTTLE_MIN` fully scaled
    /// back).
    pub value: f32,
    /// `shouldPrintScaleForward_`: the NOTICE fires once per outage.
    should_print_scale_forward: bool,
    /// `startTime_` in game ticks: when the scale-back began.
    start_tick: u32,
    /// `maxTimeBehind_`: the worst lateness this outage saw.
    pub max_time_behind: f32,
    /// The ERROR/NOTICE faces, in delivery order.
    pub notes: Vec<String>,
}

impl Default for EmergencyThrottle {
    fn default() -> Self {
        Self {
            value: 1.0,
            should_print_scale_forward: false,
            start_tick: 0,
            max_time_behind: 0.0,
            notes: Vec::new(),
        }
    }
}

impl EmergencyThrottle {
    /// `EmergencyThrottle::update` (`emergency_throttle.cpp:29`).
    pub fn update(
        &mut self,
        num_seconds_behind: f32,
        spare_time: f32,
        tick_period: f32,
        current_tick: u32,
    ) -> bool {
        self.max_time_behind = self.max_time_behind.max(num_seconds_behind);

        if self.should_scale_back(num_seconds_behind, spare_time) {
            if self.scale_back(tick_period / THROTTLE_SCALE_BACK_TIME, current_tick) {
                self.notes.push(format!(
                    "EmergencyThrottle::update: Fully scaled back. spareTime \
                     {:.6} seconds. numSecondsBehind {:.3} seconds",
                    spare_time, num_seconds_behind
                ));
                self.should_print_scale_forward = true;
                true
            } else {
                false
            }
        } else {
            if self.scale_forward(tick_period / THROTTLE_SCALE_FORWARD_TIME) {
                if self.should_print_scale_forward {
                    let back = (current_tick - self.start_tick) as f32 * tick_period;
                    self.notes.push(format!(
                        "EmergencyThrottle::update: Fully scaled forward. \
                         Scaled back for {:.2} seconds. Max behind: {:.3} \
                         seconds",
                        back, self.max_time_behind
                    ));
                    self.should_print_scale_forward = false;
                }
                self.max_time_behind = 0.0;
                true
            } else {
                false
            }
        }
    }

    /// `EmergencyThrottle::shouldScaleBack` (`:68`): late or idle-starved
    /// both count as overloaded.
    pub fn should_scale_back(&self, num_seconds_behind: f32, spare_time: f32) -> bool {
        (num_seconds_behind > THROTTLE_BEHIND_THRESHOLD)
            || (spare_time < THROTTLE_SPARE_TIME_THRESHOLD)
    }

    /// `EmergencyThrottle::scaleBack` (`:79`): true once fully scaled
    /// back.
    fn scale_back(&mut self, fraction: f32, current_tick: u32) -> bool {
        if self.value <= THROTTLE_MIN {
            // Already fully scaled back.
            return false;
        }

        if self.value == 1.0 {
            // Starting to scale back.
            self.start_tick = current_tick;
        }

        self.value = (self.value - (1.0 - THROTTLE_MIN) * fraction).max(THROTTLE_MIN);

        self.value == THROTTLE_MIN
    }

    /// `EmergencyThrottle::scaleForward` (`:104`): true once fully
    /// scaled forward.
    fn scale_forward(&mut self, fraction: f32) -> bool {
        if self.value == 1.0 {
            // Already fully scaled forward.
            return false;
        }

        self.value = (self.value + (1.0 - THROTTLE_MIN) * fraction).min(1.0);

        self.value == 1.0
    }

    /// `EmergencyThrottle::estimatePersistentLoadTime` (`:124`): what
    /// the persistent load would be without the throttle.
    pub fn estimate_persistent_load_time(
        &self,
        persistent_load_time: f32,
        throttled_load_time: f32,
    ) -> f32 {
        persistent_load_time + (1.0 / self.value - 1.0) * throttled_load_time
    }
}

/// The `updateLoad` samples one real frame measured: the tick period,
/// the spare time, the seconds behind the delivery, the transient load
/// time and the throttled (CLIENT_UPDATE) load time. The reference
/// measures these off `mainDispatcher_` and the profiler scopes; the
/// port's synthetic tick takes them from
/// [`CellApp::load_sample`](crate::cellapp_space::CellApp::load_sample)
/// — the frame that carries one drives `update_load` at its tail.
#[derive(Debug)]
pub struct LoadSample {
    /// `calcTickPeriod()`: the seconds the last game tick took.
    pub tick_period: f32,
    /// `calcSpareTime()`: the seconds spent idling since the last read.
    pub spare_time: f32,
    /// `numSecondsBehind()`: how late the tick delivery ran.
    pub num_seconds_behind: f32,
    /// `calcTransientLoadTime()`: the transient share.
    pub transient_load_time: f32,
    /// `calcThrottledLoadTime()`: the CLIENT_UPDATE profile slice.
    pub throttled_load_time: f32,
}

impl CellApp {
    /// `CellApp::updateLoad` (`cellapp.cpp:1270`): the throttle pump and
    /// the smoothed load trio. The port's dispatcher-less tick supplies
    /// the samples: `tick_period` the last tick took, `spare_time` the
    /// idle time since the last read, `num_seconds_behind` how late the
    /// tick delivery ran, `transient_load_time` the transient share,
    /// `throttled_load_time` the CLIENT_UPDATE profile slice the
    /// throttle unwinds.
    pub fn update_load(&mut self, sample: crate::cellapp_throttle::LoadSample) {
        let crate::cellapp_throttle::LoadSample {
            tick_period,
            spare_time,
            num_seconds_behind,
            transient_load_time,
            throttled_load_time,
        } = sample;

        let persistent_load_time = tick_period - spare_time - transient_load_time;
        let estimated_persistent_load_time = self
            .throttle
            .estimate_persistent_load_time(persistent_load_time, throttled_load_time);

        self.throttle
            .update(num_seconds_behind, spare_time, tick_period, self.time);
        self.emergency_throttle = self.throttle.value;

        self.check_tick_warnings(persistent_load_time, tick_period, spare_time);

        let artificial_time = self.total_added_load / self.update_hertz as f32;

        self.persistent_load = self.add_to_load(
            estimated_persistent_load_time + artificial_time,
            self.persistent_load,
        );
        self.transient_load = self.add_to_load(transient_load_time, self.transient_load);
        self.total_load = self.add_to_load(tick_period - spare_time, self.total_load);
    }

    /// `CellApp::addToLoad` (`cellapp.cpp:1258`): fold one sample into a
    /// smoothed load.
    pub fn add_to_load(&self, time_spent: f32, result: f32) -> f32 {
        let bias = LOAD_SMOOTHING_BIAS;
        (1.0 - bias) * result + bias * time_spent * self.update_hertz as f32
    }

    /// `CellApp::checkTickWarnings` (`cellapp.cpp:1167`), the WARNING
    /// arm: a tick that took more than twice its budget. The CRITICAL
    /// arms (the cellAppMgr timeout and the unsynchronised-TSC
    /// detection) are process-terminating faces without a port home.
    pub fn check_tick_warnings(
        &mut self,
        persistent_load_time: f32,
        tick_period: f32,
        spare_time: f32,
    ) {
        if tick_period * self.update_hertz as f32 > 2.0 {
            self.tick_warnings.push(format!(
                "CellApp::checkTickWarnings: Last game tick took {:.2} \
                 seconds. Persistent load time = {:.2}. Spare time {:.2}.",
                tick_period, persistent_load_time, spare_time
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cellapp_throttle::*;

    #[test]
    fn an_over_budget_tick_scales_back_and_a_quiet_one_does_not() {
        let mut throttle = EmergencyThrottle::default();
        // 0.2 seconds behind at a 0.1 budget: over
        // `behindThreshold` (0.1) — one tick of
        // tickPeriod/scaleBackTime = 0.1 steps 0.9 of the gap.
        throttle.update(0.2, 0.5, 0.1, 1);
        assert_eq!(throttle.value, 0.91);
        assert!(throttle.notes.is_empty());
        let mut idle = EmergencyThrottle::default();
        idle.update(0.0, 0.5, 0.1, 1);
        assert_eq!(idle.value, 1.0);
        assert!(idle.notes.is_empty());
    }

    #[test]
    fn a_starved_spare_time_budget_scales_back_even_when_on_time() {
        let mut throttle = EmergencyThrottle::default();
        // `spareTimeThreshold` is 0.001: 0.0005 idle seconds is under
        // it, with the delivery dead on time.
        throttle.update(0.0, 0.0005, 0.1, 1);
        assert_eq!(throttle.value, 0.91);
    }

    #[test]
    fn a_full_scale_back_lands_on_the_minimum_once() {
        let mut throttle = EmergencyThrottle::default();
        // fraction 1.0 (tickPeriod == scaleBackTime): the reference's own
        // float path steps 1.0 - 0.9 = 0.100000024 on the first tick —
        // not the minimum yet — and the second step clamps exactly onto
        // it, reporting once. Further overloaded ticks hold silently.
        assert!(!throttle.update(0.5, 0.0001, 1.0, 7));
        assert!((throttle.value - 0.1).abs() < 1e-6);
        assert!(throttle.update(0.5, 0.0001, 1.0, 8));
        assert_eq!(throttle.value, crate::cellapp_throttle::THROTTLE_MIN);
        assert_eq!(throttle.notes.len(), 1);
        assert!(throttle.notes[0].starts_with(
            "EmergencyThrottle::update: Fully scaled back. spareTime 0.000100 \
             seconds. numSecondsBehind 0.500 seconds"
        ));
        assert!(!throttle.update(0.5, 0.0001, 1.0, 9));
        assert_eq!(throttle.notes.len(), 1);
    }

    #[test]
    fn scaling_forward_recovers_and_reports_the_outage_once() {
        let mut throttle = EmergencyThrottle::default();
        // Two overloaded ticks reach the minimum (see the scale-back
        // test); the outage's worst lateness tracks along.
        assert!(!throttle.update(0.5, 0.0001, 1.0, 7));
        assert!(throttle.update(0.5, 0.0001, 1.0, 8));
        assert_eq!(throttle.max_time_behind, 0.5);
        // The recovery: 0.1f + 0.9f rounds to exactly 1.0f — one good
        // tick lands fully forward. The NOTICE carries the outage
        // length and the worst lateness, and the trackers reset.
        assert!(throttle.update(0.0, 0.5, 1.0, 10));
        assert_eq!(throttle.value, 1.0);
        assert_eq!(throttle.notes.len(), 2);
        assert_eq!(
            throttle.notes[1],
            "EmergencyThrottle::update: Fully scaled forward. Scaled back \
             for 3.00 seconds. Max behind: 0.500 seconds"
        );
        assert_eq!(throttle.max_time_behind, 0.0);
        // The ERROR re-arms the NOTICE: the next outage reports its own
        // — two scale-back ticks, one recovery tick, four notes total.
        assert!(!throttle.update(0.5, 0.0001, 1.0, 12));
        assert!(throttle.update(0.5, 0.0001, 1.0, 13));
        assert!(throttle.update(0.0, 0.5, 1.0, 14));
        assert_eq!(throttle.notes.len(), 4);
        assert_eq!(
            throttle.notes[3],
            "EmergencyThrottle::update: Fully scaled forward. Scaled back \
             for 2.00 seconds. Max behind: 0.500 seconds"
        );
    }

    #[test]
    fn the_estimated_persistent_load_unwinds_the_throttle() {
        let mut throttle = EmergencyThrottle::default();
        // Fully forward: the estimate is the sample.
        assert_eq!(throttle.estimate_persistent_load_time(0.08, 0.02), 0.08);
        // Fully back (0.1): the throttled slice counts nine times.
        assert!(!throttle.update(0.5, 0.0001, 1.0, 7));
        assert!(throttle.update(0.5, 0.0001, 1.0, 8));
        let estimated = throttle.estimate_persistent_load_time(0.08, 0.02);
        assert!((estimated - (0.08 + 9.0 * 0.02)).abs() < 1e-5);
    }

    #[test]
    fn add_to_load_smooths_toward_the_sample() {
        let app = crate::cellapp_space::CellApp::default();
        // (1-bias)*0 + bias*0.05*10 = 0.025 on a zero history.
        let folded = app.add_to_load(0.05, 0.0);
        assert!((folded - 0.025).abs() < 1e-6);
    }

    fn a_sample(
        tick_period: f32,
        spare_time: f32,
        num_seconds_behind: f32,
        transient_load_time: f32,
        throttled_load_time: f32,
    ) -> crate::cellapp_throttle::LoadSample {
        crate::cellapp_throttle::LoadSample {
            tick_period,
            spare_time,
            num_seconds_behind,
            transient_load_time,
            throttled_load_time,
        }
    }

    #[test]
    fn a_slow_tick_warns_and_feeds_the_smoothed_loads() {
        let mut app = crate::cellapp_space::CellApp::default();
        // 0.25 s tick at 10 Hz = 2.5 budgets — the WARNING arm; 0.05 s
        // idle, 0.01 transient: persistent 0.19, total 0.20.
        app.update_load(a_sample(0.25, 0.05, 0.0, 0.01, 0.02));
        assert_eq!(app.tick_warnings.len(), 1);
        assert_eq!(
            app.tick_warnings[0],
            "CellApp::checkTickWarnings: Last game tick took 0.25 seconds. \
             Persistent load time = 0.19. Spare time 0.05."
        );
        // (1-bias)*0 + bias*0.19*10 = 0.095.
        assert!((app.persistent_load - 0.095).abs() < 1e-5);
        assert!((app.transient_load - 0.005).abs() < 1e-6);
        assert!((app.total_load - 0.1).abs() < 1e-5);
        assert_eq!(app.emergency_throttle, 1.0);
    }

    #[test]
    fn an_overloaded_tick_throttles_and_estimates_the_persistent_load() {
        let mut app = crate::cellapp_space::CellApp::default();
        // Both overload gates trip: late 0.2 s (over 0.1) and spare
        // 0.0005 (under 0.001) — one scale-back step, no full back yet.
        app.update_load(a_sample(0.1, 0.0005, 0.2, 0.01, 0.02));
        assert!((app.emergency_throttle - 0.91).abs() < 1e-6);
        assert_eq!(app.throttle.value, app.emergency_throttle);
        // The estimate reads the PRE-update throttle (still 1.0 — the
        // reference computes it before `throttle_.update`): the fold is
        // 0.05 * 0.0895 * 10 = 0.04475. No tick warning at one budget.
        assert!(app.tick_warnings.is_empty());
        let expected = 0.05 * 0.0895 * 10.0;
        assert!((app.persistent_load - expected).abs() < 1e-5);
        // Artificial load is carried in ticks, folded at updateHertz.
        app.total_added_load = 0.5;
        app.update_load(a_sample(0.1, 0.02, 0.0, 0.01, 0.02));
        assert!(app.persistent_load > expected);
    }

    #[test]
    fn a_tick_pump_consumes_one_sample() {
        let mut app = crate::cellapp_space::CellApp::default();
        // No sample: the pump holds the throttle and loads.
        app.advance_tick();
        assert_eq!(app.emergency_throttle, 1.0);
        assert_eq!(app.throttle.notes.len(), 0);
        // One frame carries one sample and drives the pump at its tail.
        app.load_sample = Some(a_sample(0.1, 0.0005, 0.2, 0.01, 0.02));
        app.advance_tick();
        assert!((app.emergency_throttle - 0.91).abs() < 1e-6);
        assert!(app.load_sample.is_none());
        // The sample is consumed, not replayed.
        app.advance_tick();
        assert!((app.emergency_throttle - 0.91).abs() < 1e-6);
    }
    #[test]
    fn slow_loading_scales_the_witness_budget_down() {
        let mut app = crate::cellapp_space::CellApp::default();
        // The acceptance contrast: overloaded frames at a 0.1 budget
        // step the budget down by 0.09 a tick, and ten of them hold at
        // the floor — the witness' `maxPacketSize * throttle`
        // (`witness.cpp:1104`) then sends a tenth of the entities per
        // update.
        for _ in 0..10 {
            app.update_load(a_sample(0.1, 0.0005, 0.2, 0.01, 0.02));
        }
        // The min is approached asymptotically in float — the
        // reference's exact `isEqual` may never fire the ERROR with
        // these particular numbers; the practical floor is what the
        // witness budget sees.
        assert!((app.emergency_throttle - THROTTLE_MIN).abs() < 1e-5);
        assert!((THROTTLE_MIN * 200.0 - 20.0).abs() < 1e-5);
        // The recovery climbs back the same step size; the outage logs
        // its ERROR and its NOTICE.
        for _ in 0..10 {
            app.update_load(a_sample(0.1, 0.5, 0.0, 0.01, 0.02));
        }
        assert_eq!(app.emergency_throttle, 1.0);
        // No ERROR was logged on the asymptotic approach, so the
        // recovery's NOTICE stays silent too.
        assert!(app.throttle.notes.is_empty());
    }
}
