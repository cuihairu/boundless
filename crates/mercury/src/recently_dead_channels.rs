//! `lib/network/recently_dead_channels.cpp` (BW-02-05-01): an external
//! interface's memory of recently deregistered channel addresses.
//!
//! Straggler packets from a channel that used to carry a filter cannot be
//! read raw -- they would only trip the header walk's corruption checks --
//! so an external interface remembers each deregistered channel's address
//! for sixty seconds and drops its leftover packets instead
//! (recently_dead_channels.hpp:20).
//!
//! Deviation: the reference's `add` cancels the first entry's timer but
//! leaves the entry in the map, so the lowest address never expires and
//! stays dead forever (recently_dead_channels.cpp:38-43); this port lets
//! every entry expire on its own timer.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use foundation::time_queue::{TimerHandle, TimerHandlerFn};

use crate::endpoint::Address;
use crate::event_dispatcher::EventDispatcher;

/// The sixty seconds a dead address is remembered for
/// (recently_dead_channels.cpp:38: `60 * 1000000`).
const DEAD_PERIOD_MICROS: i64 = 60 * 1_000_000;

/// `Mercury::RecentlyDeadChannels` (recently_dead_channels.hpp:24).
pub struct RecentlyDeadChannels {
    /// `channels_`: dead addresses and the once-off timers that forget
    /// them. A vector: one entry per recent death, never large.
    channels: RefCell<Vec<(Address, TimerHandle<u64>)>>,
    /// `dispatcher_`: the timers are armed on the interface's dispatcher.
    dispatcher: Rc<EventDispatcher>,
}

impl RecentlyDeadChannels {
    /// The constructor (recently_dead_channels.cpp:21): keep the
    /// dispatcher the expiry timers are armed on.
    pub fn new(dispatcher: Rc<EventDispatcher>) -> Rc<Self> {
        Rc::new(RecentlyDeadChannels {
            channels: RefCell::new(Vec::new()),
            dispatcher,
        })
    }

    /// `add` (recently_dead_channels.cpp:34): mark the address dead and
    /// arm the once-off timer that forgets it. Re-adding an address that
    /// is already dead resets its deadline, exactly the reference's
    /// `channels_[ addr ] = timeoutHandle` overwrite.
    pub fn add(this: &Rc<Self>, addr: Address) {
        let weak: Weak<Self> = Rc::downgrade(this);
        let handle = this.dispatcher.add_once_off_timer(
            DEAD_PERIOD_MICROS,
            Rc::new(TimerHandlerFn::new(
                move |handle, _user| {
                    if let Some(this) = weak.upgrade() {
                        this.forget(handle);
                    }
                },
                "RecentlyDeadChannels",
            )),
            0,
            "RecentlyDeadChannels",
        );
        let mut channels = this.channels.borrow_mut();
        channels.retain(|(dead, _)| *dead != addr);
        channels.push((addr, handle));
    }

    /// `isDead` (recently_dead_channels.cpp:77).
    pub fn is_dead(&self, addr: &Address) -> bool {
        self.channels.borrow().iter().any(|(dead, _)| dead == addr)
    }

    /// `handleTimeout` (recently_dead_channels.cpp:47): forget the entry
    /// whose timer fired.
    fn forget(&self, handle: TimerHandle<u64>) {
        self.channels
            .borrow_mut()
            .retain(|(_, timer)| *timer != handle);
    }
}

impl Drop for RecentlyDeadChannels {
    /// The destructor (recently_dead_channels.cpp:26): cancel every
    /// pending timer before the set goes.
    fn drop(&mut self) {
        for (_, mut timer) in self.channels.get_mut().drain(..) {
            timer.cancel();
        }
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

    fn addr(port: u16) -> Address {
        Address::new(0x7f00_0001, port)
    }

    /// The acceptance: an address is dead the moment it is added, and is
    /// forgotten when the sixty-second timer fires.
    #[test]
    fn a_dead_address_is_remembered_then_forgotten() {
        let clock = PinnedClock::at(1_000_000);
        let dispatcher = pinned_dispatcher(&clock);
        let dead = RecentlyDeadChannels::new(dispatcher.clone());
        let target = addr(9);

        assert!(!dead.is_dead(&target));
        RecentlyDeadChannels::add(&dead, target);
        assert!(dead.is_dead(&target), "just deregistered");

        // A minute later the timer has fired and the address is forgotten.
        clock.set(clock.get() + 60 * 1_000_000);
        dispatcher.process_timers();
        assert!(!dead.is_dead(&target), "the deadline passed");
        assert_eq!(dispatcher.num_timer_calls(), 1);
    }

    /// Re-adding a dead address resets the deadline: the address stays
    /// dead for a full period past the last add.
    #[test]
    fn re_adding_extends_the_dead_period() {
        let clock = PinnedClock::at(1_000_000);
        let dispatcher = pinned_dispatcher(&clock);
        let dead = RecentlyDeadChannels::new(dispatcher.clone());
        let target = addr(9);

        RecentlyDeadChannels::add(&dead, target);
        clock.set(clock.get() + 59 * 1_000_000);
        dispatcher.process_timers();
        assert!(dead.is_dead(&target));

        // The re-add arms a fresh timer; the first one's expiry no longer
        // forgets the address.
        RecentlyDeadChannels::add(&dead, target);
        clock.set(clock.get() + 40 * 1_000_000);
        dispatcher.process_timers();
        assert!(dead.is_dead(&target), "the first deadline is superseded");

        clock.set(clock.get() + 21 * 1_000_000);
        dispatcher.process_timers();
        assert!(!dead.is_dead(&target), "the second deadline passed");
    }

    /// Dropping the set cancels its timers: nothing fires afterwards
    /// (recently_dead_channels.cpp:26-36).
    #[test]
    fn dropping_the_set_cancels_its_timers() {
        let clock = PinnedClock::at(1_000_000);
        let dispatcher = pinned_dispatcher(&clock);
        let dead = RecentlyDeadChannels::new(dispatcher.clone());

        RecentlyDeadChannels::add(&dead, addr(9));
        RecentlyDeadChannels::add(&dead, addr(10));
        drop(dead);

        clock.set(clock.get() + 60 * 1_000_000);
        dispatcher.process_timers();
        assert_eq!(
            dispatcher.num_timer_calls(),
            0,
            "the cancelled timers never fired"
        );
    }
}
