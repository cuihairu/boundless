//! BW-05-06-02 process time synchronisation via the `TimeKeeper`.
//!
//! The reference `lib/server/time_keeper.cpp` keeps one process's game clock
//! running at `idealTickFrequency_` ticks per second and adjusts its own
//! tick interval to track a master. The CellAppMgr is the master
//! (`CellAppMgr::startTimer`); the BaseAppMgr and each BaseApp are slaves
//! that periodically request `gameTimeReading` from it. A slave measures the
//! offset of the master's reading against its own clock, half a round-trip
//! ago, and steps its interval by `nominal/20` (`THRESHOLD = nominal/20 +
//! rtt/2`) when it runs too slow or too fast, reverting to the nominal
//! interval once it is back within tolerance.
//!
//! The Mercury traffic and the wall clock are not modelled here: the module
//! is driven by a caller-supplied `now` in seconds, so a fixed sample
//! sequence is fully deterministic. The caller owns the game-time counter
//! (fed through [`TimeKeeper::advance_game_time`]) and reads it back with
//! [`TimeKeeper::reading_at_last_tick`].

/// The default `timeStagger/prime`.
pub const STAGGER_PRIME: i32 = 37;
/// The default `timeStagger/multiplier`.
pub const STAGGER_MULTIPLIER: i32 = 5;

/// What a master reading did to the slave's tick interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    /// No sync request was in flight; the reading was ignored.
    NoSyncInFlight,
    /// The slave was slow: the interval was shortened by `nominal/20`.
    Shortened,
    /// The slave was fast: the interval was lengthened by `nominal/20`.
    Lengthened,
    /// The interval was back to the nominal value.
    Reverted,
    /// The reading was within tolerance and the interval was already
    /// nominal.
    WithinTolerance,
    /// The interval stays at its non-nominal value; a re-check is scheduled.
    KeepAdjusting,
}

/// `TimeKeeper` (`lib/server/time_keeper.cpp`).
#[derive(Debug, Clone, Copy)]
pub struct TimeKeeper {
    /// `idealTickFrequency_`, ticks per second.
    ideal_tick_frequency: f64,
    /// `nominalIntervalStamps_`, seconds per tick.
    nominal_interval: f64,
    /// The current tick interval, seconds per tick.
    interval: f64,
    /// `isMaster_`: `true` for the CellAppMgr's clock.
    is_master: bool,
    /// `staggerOffset_`, seconds added to an incoming master reading.
    stagger_offset: f64,
    /// `lastSyncRequestStamps_`: the wall time the in-flight sync request
    /// went out, `None` when none is in flight.
    last_sync_request: Option<f64>,
    /// `gameTime_`, ticks elapsed.
    game_time: u64,
    /// The wall time at which `game_time + 1` is delivered.
    next_tick_delivery: f64,
}

impl TimeKeeper {
    /// A master clock (`CellAppMgr::startTimer`).
    pub fn master(ideal_tick_frequency: f64, now: f64) -> Self {
        let nominal_interval = 1.0 / ideal_tick_frequency;
        Self {
            ideal_tick_frequency,
            nominal_interval,
            interval: nominal_interval,
            is_master: true,
            stagger_offset: 0.0,
            last_sync_request: None,
            game_time: 0,
            next_tick_delivery: now + nominal_interval,
        }
    }

    /// A slave clock (`BaseAppMgr::startTimer` / `BaseApp::onStartOfTick`).
    /// `stagger_index` (a per-process id) and `max_stagger` spread the
    /// slaves so their sync requests do not collide.
    pub fn slave(
        ideal_tick_frequency: f64,
        now: f64,
        stagger_index: i32,
        max_stagger: f64,
    ) -> Self {
        let nominal_interval = 1.0 / ideal_tick_frequency;
        let index = (stagger_index * STAGGER_MULTIPLIER).rem_euclid(STAGGER_PRIME);
        let fraction = index as f64 / STAGGER_PRIME as f64 - 0.5;
        let stagger_offset = fraction / ideal_tick_frequency * max_stagger;
        Self {
            ideal_tick_frequency,
            nominal_interval,
            interval: nominal_interval,
            is_master: false,
            stagger_offset,
            last_sync_request: None,
            game_time: 0,
            next_tick_delivery: now + nominal_interval,
        }
    }

    /// The current tick interval, seconds per tick.
    pub fn interval(&self) -> f64 {
        self.interval
    }

    /// `nominalIntervalStamps_`, seconds per tick.
    pub fn nominal_interval(&self) -> f64 {
        self.nominal_interval
    }

    /// `isMaster_`.
    pub fn is_master(&self) -> bool {
        self.is_master
    }

    /// `staggerOffset_`, seconds.
    pub fn stagger_offset(&self) -> f64 {
        self.stagger_offset
    }

    /// `gameTime_`.
    pub fn game_time(&self) -> u64 {
        self.game_time
    }

    /// `readingAtLastTick()`: `gameTime_ / idealTickFrequency_`.
    pub fn reading_at_last_tick(&self) -> f64 {
        self.game_time as f64 / self.ideal_tick_frequency
    }

    /// `readingAtNextTick()`: `(gameTime_ + 1) / idealTickFrequency_`.
    pub fn reading_at_next_tick(&self) -> f64 {
        (self.game_time + 1) as f64 / self.ideal_tick_frequency
    }

    /// `readingNow()`: interpolate the reading at the current wall time
    /// between the last delivered tick and the next one.
    pub fn reading_now(&self, now: f64) -> f64 {
        let ticks_since_next = (now - self.next_tick_delivery) / self.interval;
        (self.game_time as f64 + 1.0 + ticks_since_next) / self.ideal_tick_frequency
    }

    /// Advance the game clock past `now`: every delivery time reached adds a
    /// tick and pushes the next delivery out by the current interval.
    pub fn advance_game_time(&mut self, now: f64) {
        while now >= self.next_tick_delivery {
            self.game_time += 1;
            self.next_tick_delivery += self.interval;
        }
    }

    /// `synchroniseWithMaster()`: record that a sync request just went out,
    /// so the round-trip can be measured when the reply lands.
    pub fn synchronise_with_master(&mut self, now: f64) -> bool {
        if self.is_master || self.last_sync_request.is_some() {
            return false;
        }
        self.last_sync_request = Some(now);
        true
    }

    /// `inputMasterReading()`: the reply to a `gameTimeReading` request.
    /// The offset is measured against what our clock read half a round-trip
    /// ago, and the interval is stepped by `nominal/20` beyond the
    /// `nominal/20 + rtt/2` threshold.
    pub fn input_master_reading(&mut self, reading: f64, now: f64) -> SyncAction {
        if self.is_master {
            return SyncAction::NoSyncInFlight;
        }
        let Some(last_sync) = self.last_sync_request.take() else {
            return SyncAction::NoSyncInFlight;
        };

        let reading = reading + self.stagger_offset;
        let round_trip = now - last_sync;
        let offset = (reading + round_trip / 2.0) - self.reading_now(now);

        let increment = self.nominal_interval / 20.0;
        let threshold = increment + round_trip / 2.0;

        if offset > threshold {
            // We are slow.
            if (self.interval - self.nominal_interval).abs() < f64::EPSILON {
                self.interval = self.nominal_interval - increment;
                SyncAction::Shortened
            } else if self.interval > self.nominal_interval {
                self.interval = self.nominal_interval;
                SyncAction::Reverted
            } else {
                SyncAction::KeepAdjusting
            }
        } else if offset < -threshold {
            // We are fast.
            if (self.interval - self.nominal_interval).abs() < f64::EPSILON {
                self.interval = self.nominal_interval + increment;
                SyncAction::Lengthened
            } else if self.interval < self.nominal_interval {
                self.interval = self.nominal_interval;
                SyncAction::Reverted
            } else {
                SyncAction::KeepAdjusting
            }
        } else if (self.interval - self.nominal_interval).abs() >= f64::EPSILON {
            // Back within tolerance.
            self.interval = self.nominal_interval;
            SyncAction::Reverted
        } else {
            SyncAction::WithinTolerance
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERTZ: f64 = 100.0;
    const NOMINAL: f64 = 1.0 / HERTZ;

    /// A slave created at `now = 0` advanced until `n` ticks have delivered.
    fn slave_n_ticks(n: u64) -> TimeKeeper {
        let mut keeper = TimeKeeper::slave(HERTZ, 0.0, 0, 0.0);
        let mut t = 0.0;
        while keeper.game_time() < n {
            t += NOMINAL;
            keeper.advance_game_time(t);
        }
        keeper
    }

    #[test]
    fn readings_follow_the_game_clock() {
        let mut keeper = TimeKeeper::master(HERTZ, 0.0);
        assert_eq!(keeper.reading_at_last_tick(), 0.0);
        assert_eq!(keeper.reading_at_next_tick(), NOMINAL);
        // Nothing has run yet at `now = 0`, so the reading is 0.
        assert_eq!(keeper.reading_now(0.0), 0.0);

        keeper.advance_game_time(NOMINAL);
        assert_eq!(keeper.game_time(), 1);
        assert_eq!(keeper.reading_at_last_tick(), NOMINAL);
        assert!((keeper.reading_at_next_tick() - 2.0 * NOMINAL).abs() < 1e-9);
        assert!((keeper.reading_now(NOMINAL) - NOMINAL).abs() < 1e-9);
    }

    /// The reply to a `gameTimeReading` request: the master's reading at the
    /// moment the request reached it. With `delta` the master/slave game-time
    /// offset, the request goes out at `t0`, the master reads its clock at
    /// `t0 + rtt/2` and the reply lands at `t0 + rtt`, so
    /// `reading = slaveReading(t0) + rtt/2 + delta` and `inputMasterReading`
    /// recovers exactly `delta`.
    fn master_reading(rtt: f64, slave_reading_at_t0: f64, delta: f64) -> f64 {
        slave_reading_at_t0 + rtt / 2.0 + delta
    }

    #[test]
    fn a_slow_slave_shortens_its_interval() {
        let mut keeper = slave_n_ticks(100);
        let t0 = 1.0;
        let rtt = 0.001;
        assert!(keeper.synchronise_with_master(t0));
        // The master is 3ms ahead of the slave: it is slow, so the interval
        // is shortened by nominal/20.
        let reading = master_reading(rtt, keeper.reading_now(t0), 0.003);
        assert_eq!(
            keeper.input_master_reading(reading, t0 + rtt),
            SyncAction::Shortened
        );
        assert!((keeper.interval() - (NOMINAL - NOMINAL / 20.0)).abs() < 1e-12);
    }

    #[test]
    fn a_fast_slave_lengthens_its_interval() {
        let mut keeper = slave_n_ticks(100);
        let t0 = 1.0;
        let rtt = 0.001;
        assert!(keeper.synchronise_with_master(t0));
        // The master is 3ms behind the slave: it is fast, so the interval is
        // lengthened by nominal/20.
        let reading = master_reading(rtt, keeper.reading_now(t0), -0.003);
        assert_eq!(
            keeper.input_master_reading(reading, t0 + rtt),
            SyncAction::Lengthened
        );
        assert!((keeper.interval() - (NOMINAL + NOMINAL / 20.0)).abs() < 1e-12);
    }

    #[test]
    fn a_within_tolerance_slave_stays_nominal() {
        let mut keeper = slave_n_ticks(100);
        let t0 = 1.0;
        let rtt = 0.001;
        assert!(keeper.synchronise_with_master(t0));
        // 0.2ms is inside nominal/20 + rtt/2: the interval is left nominal.
        let reading = master_reading(rtt, keeper.reading_now(t0), 0.0002);
        assert_eq!(
            keeper.input_master_reading(reading, t0 + rtt),
            SyncAction::WithinTolerance
        );
        assert!((keeper.interval() - NOMINAL).abs() < 1e-12);
    }

    #[test]
    fn a_drifted_slave_reverts_when_back_in_tolerance() {
        let mut keeper = slave_n_ticks(100);
        keeper.interval = NOMINAL - NOMINAL / 20.0; // was slow, shortened
        let t0 = 1.0;
        let rtt = 0.001;
        assert!(keeper.synchronise_with_master(t0));
        // Back within tolerance: the interval returns to nominal.
        let reading = master_reading(rtt, keeper.reading_now(t0), 0.0002);
        assert_eq!(
            keeper.input_master_reading(reading, t0 + rtt),
            SyncAction::Reverted
        );
        assert!((keeper.interval() - NOMINAL).abs() < 1e-12);
    }

    #[test]
    fn no_sync_in_flight_is_ignored() {
        let mut keeper = slave_n_ticks(100);
        assert_eq!(
            keeper.input_master_reading(1.0, 1.0),
            SyncAction::NoSyncInFlight
        );
    }

    #[test]
    fn master_rejects_sync_requests() {
        let mut keeper = TimeKeeper::master(HERTZ, 0.0);
        assert!(!keeper.synchronise_with_master(0.0));
        assert_eq!(
            keeper.input_master_reading(1.0, 0.0),
            SyncAction::NoSyncInFlight
        );
    }

    #[test]
    fn stagger_spreads_the_slaves() {
        // stagger_index 0 and 37 are the same slot; 1 is one slot along.
        let a = TimeKeeper::slave(HERTZ, 0.0, 0, 0.1);
        let b = TimeKeeper::slave(HERTZ, 0.0, 37, 0.1);
        let c = TimeKeeper::slave(HERTZ, 0.0, 1, 0.1);
        assert_eq!(a.stagger_offset(), b.stagger_offset());
        assert!((c.stagger_offset() - a.stagger_offset()).abs() > 1e-9);
        // The fraction stays in [-0.5, 0.5) of a tick scaled by max_stagger.
        assert!(a.stagger_offset().abs() <= 0.5 / HERTZ * 0.1 + 1e-12);
    }

    /// A slave that starts four ticks behind the master catches up and then
    /// stays within the tolerance band: with a fixed sample sequence the
    /// game-time drift is bounded.
    #[test]
    fn fixed_samples_keep_drift_bounded() {
        const INITIAL_LAG: f64 = 4.0 * NOMINAL; // the slave starts 40ms behind
        const RTT: f64 = 0.001;
        let mut master = TimeKeeper::master(HERTZ, 0.0);
        // The slave's first delivery is delayed by the lag, so its clock
        // runs genuinely behind the master's.
        let mut slave = TimeKeeper::slave(HERTZ, INITIAL_LAG, 0, 0.0);

        let mut now = 0.0;
        let mut initial_seen = false;
        let mut worst_after = 0.0f64;
        for step in 0..2000 {
            now += NOMINAL;
            master.advance_game_time(now);
            slave.advance_game_time(now);

            if step % 10 == 0 {
                assert!(slave.synchronise_with_master(now));
                // The master reads its clock half a round-trip after the
                // request went out; the reply lands a round-trip later.
                let reading = master.reading_now(now + RTT / 2.0);
                slave.input_master_reading(reading, now + RTT);
            }

            let lag = (master.reading_now(now) - slave.reading_now(now)).abs();
            if step < 20 {
                initial_seen |= lag > 0.9 * INITIAL_LAG;
            } else if step > 500 {
                worst_after = worst_after.max(lag);
            }
        }

        // The sample started near the full lag and settles inside the
        // `nominal/20 + rtt/2` tolerance band plus a small margin.
        assert!(initial_seen, "sample did not start behind");
        let band = NOMINAL / 20.0 + RTT / 2.0 + 0.0005;
        assert!(
            worst_after < band,
            "drift {worst_after} exceeds band {band}"
        );
    }
}
