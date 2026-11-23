//! `NetworkInterface`'s rate limiter (`network_interface.cpp:936-1004`,
//! BW-02-05-04): drop traffic from an address that exceeds a per-period
//! budget.
//!
//! The reference counts every packet against its source address and
//! against its source address+port, inside one rate-limiting period. A
//! packet that crosses the budget is refused (`REASON_GENERAL_NETWORK` at
//! the receive path), and the rest of the period keeps refusing. When the
//! period expires the counts are cleared and the accumulated over-budget
//! counts are sampled into a five-sample EMA (`AccumulatingEMA`).
//!
//! A zero period means no rate limiting: `incrementAndCheckRateLimit`
//! answers false immediately, exactly the reference's `isZero` guard.
//!
//! Deviation: the reference lets the dispatcher's teardown reclaim the
//! period timer; this port cancels it in `Drop`, so a limiter dropped
//! while its dispatcher lives leaves nothing armed (the same choice
//! `RecentlyDeadChannels` makes).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use foundation::math::ema::{AccumulatingEma, Ema};
use foundation::time_queue::{TimerHandle, TimerHandlerFn};

use crate::endpoint::Address;
use crate::event_dispatcher::EventDispatcher;

/// `EMA::calculateBiasFromNumSamples( 5 )` at the default 0.95 weighting
/// (network_interface.cpp:99-105).
const SAMPLES: f32 = 5.0;
/// `EMA::calculateBiasFromNumSamples`'s default weighting.
const WEIGHTING: f32 = 0.95;

/// `NetworkInterface`'s rate-limiting state: the configured budgets, the
/// per-period counters, and the two over-budget averages.
pub struct RateLimiter {
    /// `dispatcher_`: the period timer is armed on the interface's
    /// dispatcher.
    dispatcher: Rc<EventDispatcher>,
    /// `rateLimitPeriod_` (seconds).
    period: Cell<f32>,
    /// `rateLimitTimer_`.
    timer: RefCell<TimerHandle<u64>>,
    /// `rateLimitPerIPAddress_`.
    per_ip_limit: Cell<u32>,
    /// `rateLimitedIPAddresses_`: per source IP, the port zeroed.
    ip_counts: RefCell<HashMap<Address, u32>>,
    /// `ipAddressRateLimitAverage_`.
    ip_average: RefCell<AccumulatingEma<u32>>,
    /// `rateLimitPerIPAddressPort_`.
    per_ip_port_limit: Cell<u32>,
    /// `rateLimitedIPAddressPorts_`: per source address.
    ip_port_counts: RefCell<HashMap<Address, u32>>,
    /// `ipAddressPortRateLimitAverage_`.
    ip_port_average: RefCell<AccumulatingEma<u32>>,
}

impl RateLimiter {
    /// The constructor's rate-limiting defaults (network_interface.cpp:99-105):
    /// no period (limiting off) and no budgets, the averages starting from
    /// the five-sample bias.
    pub fn new(dispatcher: Rc<EventDispatcher>) -> Rc<Self> {
        let bias = Ema::calculate_bias_from_num_samples(SAMPLES, WEIGHTING);
        Rc::new(RateLimiter {
            dispatcher,
            period: Cell::new(0.0),
            timer: RefCell::new(TimerHandle::default()),
            per_ip_limit: Cell::new(0),
            ip_counts: RefCell::new(HashMap::new()),
            ip_average: RefCell::new(AccumulatingEma::new(bias, 0.0, 0u32)),
            per_ip_port_limit: Cell::new(0),
            ip_port_counts: RefCell::new(HashMap::new()),
            ip_port_average: RefCell::new(AccumulatingEma::new(bias, 0.0, 0u32)),
        })
    }

    /// `rateLimitPeriod()`.
    pub fn rate_limit_period(&self) -> f32 {
        self.period.get()
    }

    /// `rateLimitPeriod( rateLimitPeriod )` (network_interface.cpp:936):
    /// cancel any period timer, clear the counters, store the new period,
    /// and arm the recurring timer that samples and resets when it is
    /// positive. A zero or negative period leaves limiting off.
    pub fn set_rate_limit_period(this: &Rc<Self>, period: f32) {
        this.timer.borrow_mut().cancel();
        this.clear_rate_limit_counts();
        this.period.set(period);
        if period != 0.0 && period > 0.0 {
            let weak: Weak<Self> = Rc::downgrade(this);
            let handle = this.dispatcher.add_timer(
                (period as f64 * 1_000_000.0) as i64,
                Rc::new(TimerHandlerFn::new(
                    move |_handle, _user| {
                        if let Some(limiter) = weak.upgrade() {
                            limiter.handle_timeout();
                        }
                    },
                    "NetworkInterface::rateLimitPeriod",
                )),
                0,
                "NetworkInterface::rateLimitPeriod",
            );
            *this.timer.borrow_mut() = handle;
        }
    }

    /// `perIPAddressRateLimit()`.
    pub fn per_ip_address_rate_limit(&self) -> u32 {
        self.per_ip_limit.get()
    }

    /// `perIPAddressRateLimit( limitPerPeriod )`.
    pub fn set_per_ip_address_rate_limit(&self, limit_per_period: u32) {
        self.per_ip_limit.set(limit_per_period);
    }

    /// `perIPAddressPortRateLimit()`.
    pub fn per_ip_address_port_rate_limit(&self) -> u32 {
        self.per_ip_port_limit.get()
    }

    /// `perIPAddressPortRateLimit( limitPerPeriod )`.
    pub fn set_per_ip_address_port_rate_limit(&self, limit_per_period: u32) {
        self.per_ip_port_limit.set(limit_per_period);
    }

    /// `incrementAndCheckRateLimit( addr )` (network_interface.cpp:970):
    /// count the packet against the source IP and the source address, and
    /// answer whether either budget was crossed. A zero period means no
    /// limiting.
    pub fn increment_and_check_rate_limit(&self, addr: &Address) -> bool {
        // `isZero( rateLimitPeriod_ )` is an exact zero test.
        if self.period.get() == 0.0 {
            return false;
        }

        let mut should_rate_limit = false;

        // Check and update IP address.
        let per_ip = self.per_ip_limit.get();
        if per_ip > 0 {
            let zeroed_port_addr = Address::new(addr.ip, 0);
            let mut counts = self.ip_counts.borrow_mut();
            let count = counts.entry(zeroed_port_addr).or_insert(0);
            *count += 1;
            if *count > per_ip {
                self.ip_average.borrow_mut().increment_value();
                should_rate_limit = true;
            }
        }

        // Check and update IP address and port pair.
        let per_ip_port = self.per_ip_port_limit.get();
        if per_ip_port > 0 {
            let mut counts = self.ip_port_counts.borrow_mut();
            let count = counts.entry(*addr).or_insert(0);
            *count += 1;
            if *count > per_ip_port {
                self.ip_port_average.borrow_mut().increment_value();
                should_rate_limit = true;
            }
        }

        should_rate_limit
    }

    /// `ipAddressRateLimitAverage()`: the EMA of the over-budget IP counts.
    pub fn ip_address_rate_limit_average(&self) -> f32 {
        self.ip_average.borrow().average()
    }

    /// `ipAddressPortRateLimitAverage()`: the EMA of the over-budget
    /// address counts.
    pub fn ip_address_port_rate_limit_average(&self) -> f32 {
        self.ip_port_average.borrow().average()
    }

    /// `handleTimeout` (network_interface.cpp:540): the period expired.
    /// Sample both accumulated over-budget counts into their averages,
    /// then clear the counters for the next period.
    fn handle_timeout(&self) {
        self.ip_average.borrow_mut().sample(true);
        self.ip_port_average.borrow_mut().sample(true);
        self.clear_rate_limit_counts();
    }

    /// `clearRateLimitCounts` (network_interface.cpp:957): forget every
    /// counter and reset both accumulated values.
    fn clear_rate_limit_counts(&self) {
        self.ip_counts.borrow_mut().clear();
        self.ip_port_counts.borrow_mut().clear();
        *self.ip_average.borrow_mut().value_mut() = 0;
        *self.ip_port_average.borrow_mut().value_mut() = 0;
    }
}

impl Drop for RateLimiter {
    /// Cancel the period timer before the limiter goes, so nothing fires
    /// into a dropped limiter (its dispatcher may outlive it).
    fn drop(&mut self) {
        self.timer.get_mut().cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    /// A pinned microsecond clock, shared by every dispatcher built from it.
    #[derive(Clone)]
    struct PinnedClock(Rc<Cell<u64>>);

    impl PinnedClock {
        fn at(micros: u64) -> Self {
            Self(Rc::new(Cell::new(micros)))
        }

        fn get(&self) -> u64 {
            self.0.get()
        }

        fn set(&self, micros: u64) {
            self.0.set(micros);
        }
    }

    fn pinned_dispatcher(clock: &PinnedClock) -> Rc<EventDispatcher> {
        let handle = Rc::clone(&clock.0);
        Rc::new(EventDispatcher::with_clock(Box::new(move || handle.get())))
    }

    fn limiter(clock: &PinnedClock) -> Rc<RateLimiter> {
        RateLimiter::new(pinned_dispatcher(clock))
    }

    fn addr(port: u16) -> Address {
        Address::new(0x7f00_0001, port)
    }

    /// The default limiter has no period and no budgets: nothing is ever
    /// rate limited, and the averages sit at zero.
    #[test]
    fn the_default_limiter_rate_limits_nothing() {
        let clock = PinnedClock::at(1_000_000);
        let limiter = limiter(&clock);

        assert_eq!(limiter.rate_limit_period(), 0.0);
        assert_eq!(limiter.per_ip_address_rate_limit(), 0);
        assert_eq!(limiter.per_ip_address_port_rate_limit(), 0);

        for _ in 0..10 {
            assert!(!limiter.increment_and_check_rate_limit(&addr(9)));
        }
        assert_eq!(limiter.ip_address_rate_limit_average(), 0.0);
        assert_eq!(limiter.ip_address_port_rate_limit_average(), 0.0);
    }

    /// A zero period disables the limiter even with budgets set: the
    /// reference's `isZero` guard answers false first.
    #[test]
    fn a_zero_period_disables_the_limiter() {
        let clock = PinnedClock::at(1_000_000);
        let limiter = limiter(&clock);
        limiter.set_per_ip_address_rate_limit(2);

        for _ in 0..10 {
            assert!(!limiter.increment_and_check_rate_limit(&addr(9)));
        }
    }

    /// The acceptance boundary: a source IP may send up to the budget in
    /// one period; the packet that crosses it is the first one refused.
    /// The reference compares `++count > limit` (strictly), so exactly
    /// `limit` packets pass.
    #[test]
    fn the_boundary_is_exactly_the_budget() {
        let clock = PinnedClock::at(1_000_000);
        let limiter = limiter(&clock);
        RateLimiter::set_rate_limit_period(&limiter, 1.0);
        limiter.set_per_ip_address_rate_limit(3);

        let source = addr(9);
        assert!(!limiter.increment_and_check_rate_limit(&source), "first");
        assert!(!limiter.increment_and_check_rate_limit(&source), "second");
        assert!(!limiter.increment_and_check_rate_limit(&source), "third");
        assert!(
            limiter.increment_and_check_rate_limit(&source),
            "the fourth crosses the budget"
        );
        assert!(
            limiter.increment_and_check_rate_limit(&source),
            "the rest of the period stays refused"
        );
    }

    /// The per-IP budget zeroes the port, so addresses sharing an IP draw
    /// on one budget; the per-address budget is per address.
    #[test]
    fn the_ip_budget_is_shared_and_the_address_budget_is_per_address() {
        let clock = PinnedClock::at(1_000_000);
        let limiter = limiter(&clock);
        RateLimiter::set_rate_limit_period(&limiter, 1.0);
        limiter.set_per_ip_address_rate_limit(2);
        limiter.set_per_ip_address_port_rate_limit(1);

        // Two addresses, same IP: the IP budget of 2 is shared.
        assert!(!limiter.increment_and_check_rate_limit(&addr(9)), "a");
        assert!(!limiter.increment_and_check_rate_limit(&addr(10)), "b");
        assert!(
            limiter.increment_and_check_rate_limit(&addr(11)),
            "the IP budget is exhausted"
        );

        // A fresh IP starts a fresh IP budget, but the address budget of
        // one still holds for it: its first packet passes, the second is
        // over the per-address budget.
        let other = Address::new(0x7f00_0002, 9);
        assert!(!limiter.increment_and_check_rate_limit(&other), "fresh IP");
        assert!(
            limiter.increment_and_check_rate_limit(&other),
            "the address budget is crossed"
        );
    }

    /// When the period expires the counters are cleared: the source that
    /// was refused starts the next period with a clean budget.
    #[test]
    fn the_period_expiry_resets_the_counts() {
        let clock = PinnedClock::at(1_000_000);
        let limiter = limiter(&clock);
        RateLimiter::set_rate_limit_period(&limiter, 1.0);
        limiter.set_per_ip_address_rate_limit(2);

        let source = addr(9);
        assert!(!limiter.increment_and_check_rate_limit(&source));
        assert!(!limiter.increment_and_check_rate_limit(&source));
        assert!(
            limiter.increment_and_check_rate_limit(&source),
            "over budget"
        );

        // A second later the timer has fired and reset the counters.
        clock.set(clock.get() + 1_000_000);
        limiter.dispatcher.process_timers();
        assert!(!limiter.increment_and_check_rate_limit(&source), "cleared");
    }

    /// Each period's over-budget count is sampled into the EMA: the
    /// average reflects the counts of the periods that have elapsed.
    #[test]
    fn the_over_budget_count_sampled_into_the_average() {
        let clock = PinnedClock::at(1_000_000);
        let limiter = limiter(&clock);
        RateLimiter::set_rate_limit_period(&limiter, 1.0);
        limiter.set_per_ip_address_rate_limit(1);

        let source = addr(9);
        assert!(!limiter.increment_and_check_rate_limit(&source), "first");
        assert!(limiter.increment_and_check_rate_limit(&source), "second");
        assert!(limiter.increment_and_check_rate_limit(&source), "third");

        clock.set(clock.get() + 1_000_000);
        limiter.dispatcher.process_timers();

        // Two over-budget counts were accumulated and sampled.
        let bias = Ema::calculate_bias_from_num_samples(SAMPLES, WEIGHTING);
        assert_eq!(
            limiter.ip_address_rate_limit_average(),
            (1.0 - bias) * 2.0,
            "the first sample folds 2 over-budget counts in"
        );
        // The address average saw nothing over budget.
        assert_eq!(limiter.ip_address_port_rate_limit_average(), 0.0);
    }

    /// Changing the period cancels the old timer and starts clean: the
    /// counts from the old period are gone before the new one begins.
    #[test]
    fn a_new_period_clears_the_counts_and_rearms() {
        let clock = PinnedClock::at(1_000_000);
        let limiter = limiter(&clock);
        RateLimiter::set_rate_limit_period(&limiter, 1.0);
        limiter.set_per_ip_address_rate_limit(1);

        let source = addr(9);
        assert!(!limiter.increment_and_check_rate_limit(&source));
        assert!(
            limiter.increment_and_check_rate_limit(&source),
            "over budget"
        );

        // A new period clears the counts first (network_interface.cpp:938).
        RateLimiter::set_rate_limit_period(&limiter, 2.0);
        assert_eq!(limiter.rate_limit_period(), 2.0);
        assert!(!limiter.increment_and_check_rate_limit(&source), "cleared");
        assert!(
            limiter.increment_and_check_rate_limit(&source),
            "the budget of one is crossed again"
        );
    }

    /// Clearing the period leaves no timer armed: the limiter reverts to
    /// off, exactly as setting the period to zero does in the reference.
    #[test]
    fn setting_the_period_to_zero_stops_limiting() {
        let clock = PinnedClock::at(1_000_000);
        let limiter = limiter(&clock);
        RateLimiter::set_rate_limit_period(&limiter, 1.0);
        limiter.set_per_ip_address_rate_limit(1);

        let source = addr(9);
        assert!(!limiter.increment_and_check_rate_limit(&source), "first");
        assert!(
            limiter.increment_and_check_rate_limit(&source),
            "over budget"
        );

        RateLimiter::set_rate_limit_period(&limiter, 0.0);
        assert_eq!(limiter.rate_limit_period(), 0.0);
        assert!(!limiter.increment_and_check_rate_limit(&source));
        assert!(!limiter.increment_and_check_rate_limit(&source));
    }

    /// Dropping the limiter cancels its timer: nothing fires into it after
    /// it is gone.
    #[test]
    fn dropping_the_limiter_cancels_its_timer() {
        let clock = PinnedClock::at(1_000_000);
        let dispatcher = pinned_dispatcher(&clock);
        let limiter = RateLimiter::new(dispatcher.clone());
        RateLimiter::set_rate_limit_period(&limiter, 1.0);

        drop(limiter);
        clock.set(clock.get() + 1_000_000);
        dispatcher.process_timers();
        assert_eq!(
            dispatcher.num_timer_calls(),
            0,
            "the cancelled timer never fired"
        );
    }
}
