//! `Mercury::RecentlyDeadChannels` (`lib/network/recently_dead_channels.hpp/.cpp`):
//! the external interface's short memory of deregistered channels.
//!
//! When an external channel is deregistered its address is remembered for
//! 60 seconds so that late datagrams from the just-disconnected peer are
//! dropped instead of being processed raw (which the reference says
//! produces spurious corruption warnings). The address is forgotten by a
//! once-off timer; dropping the collection cancels every timer it armed.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use foundation::time_queue::{TimerHandle, TimerHandler, TimerHandlerFn};

use crate::endpoint::Address;
use crate::event_dispatcher::EventDispatcher;

/// `60 * 1000000` (recently_dead_channels.cpp:67): the microseconds an
/// address stays dead.
const DEAD_FOR_MICROS: i64 = 60 * 1_000_000;

/// `Mercury::RecentlyDeadChannels`: addresses of recently deregistered
/// channels and the timer that forgets each one.
pub struct RecentlyDeadChannels {
    /// `channels_`: address -> the timer that will forget it.
    channels: RefCell<BTreeMap<Address, TimerHandle<u64>>>,
    /// `dispatcher_`: the dispatcher the timers are added on.
    dispatcher: Rc<EventDispatcher>,
}

impl RecentlyDeadChannels {
    /// `RecentlyDeadChannels( dispatcher )`.
    pub fn new(dispatcher: &Rc<EventDispatcher>) -> Rc<Self> {
        Rc::new(Self {
            channels: RefCell::new(BTreeMap::new()),
            dispatcher: Rc::clone(dispatcher),
        })
    }

    /// `add( addr )` (recently_dead_channels.cpp:57): remember the address
    /// for [`DEAD_FOR_MICROS`] and arm its forgetting timer.
    pub fn add(self: &Rc<Self>, addr: Address) {
        // The reference cancels the *first* entry's handle here -- it never
        // looks `addr` up -- and leaves the cancelled entry in the map, so
        // that first address stays dead past its own 60s. Ported as written.
        if let Some(handle) = self.channels.borrow().values().next().cloned() {
            let mut handle = handle;
            handle.cancel();
        }

        let weak: Weak<Self> = Rc::downgrade(self);
        let handler: Rc<dyn TimerHandler<u64>> = Rc::new(TimerHandlerFn::new(
            move |handle: TimerHandle<u64>, _user: usize| {
                if let Some(this) = weak.upgrade() {
                    this.handle_timeout(handle);
                }
            },
            "RecentlyDeadChannels",
        ));
        let timeout =
            self.dispatcher
                .add_once_off_timer(DEAD_FOR_MICROS, handler, 0, "RecentlyDeadChannels");
        self.channels.borrow_mut().insert(addr, timeout);
    }

    /// `isDead( addr )` (recently_dead_channels.cpp:94): the address was
    /// recently deregistered.
    pub fn is_dead(&self, addr: &Address) -> bool {
        self.channels.borrow().contains_key(addr)
    }

    /// How many addresses are remembered (the tests' window on the map).
    pub fn count(&self) -> usize {
        self.channels.borrow().len()
    }

    /// `handleTimeout` (recently_dead_channels.cpp:74): forget the address
    /// whose timer fired.
    fn handle_timeout(&self, handle: TimerHandle<u64>) {
        let mut channels = self.channels.borrow_mut();
        let expired = channels
            .iter()
            .find(|(_addr, timer)| **timer == handle)
            .map(|(addr, _timer)| *addr);
        if let Some(addr) = expired {
            channels.remove(&addr);
        }
    }
}

impl Drop for RecentlyDeadChannels {
    /// `~RecentlyDeadChannels()`: cancel every timer this collection armed.
    fn drop(&mut self) {
        for handle in self.channels.get_mut().values_mut() {
            handle.cancel();
        }
        self.channels.get_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const LOOPBACK: u32 = 0x0100_007f;

    fn address(port: u16) -> Address {
        Address::new(LOOPBACK, port)
    }

    /// A dispatcher on an injected clock the test advances.
    fn dispatcher() -> (Rc<EventDispatcher>, Rc<Cell<u64>>) {
        let clock = Rc::new(Cell::new(0u64));
        let injected = Rc::clone(&clock);
        let dispatcher = EventDispatcher::with_clock(Box::new(move || injected.get()));
        (Rc::new(dispatcher), clock)
    }

    /// BW-02-03-02: a deregistered address stays dead for 60s, then its
    /// timer forgets it.
    #[test]
    fn an_address_is_dead_until_its_timer_forgets_it() {
        let (dispatcher, clock) = dispatcher();
        let dead = RecentlyDeadChannels::new(&dispatcher);
        let addr = address(1234);

        dead.add(addr);
        assert!(dead.is_dead(&addr));
        assert_eq!(dead.count(), 1);

        // 60s later the once-off timer fires and forgets the address.
        clock.set(DEAD_FOR_MICROS as u64 + 1);
        dispatcher.process_timers();
        assert!(!dead.is_dead(&addr), "the address was forgotten");
        assert_eq!(dead.count(), 0);
    }

    /// The reference's `add` cancels the *first* entry's handle rather than
    /// `addr`'s, so the earliest address is never forgotten by its own
    /// timer -- ported faithfully.
    #[test]
    fn a_second_add_cancels_the_first_entry_never_the_new_address() {
        let (dispatcher, clock) = dispatcher();
        let dead = RecentlyDeadChannels::new(&dispatcher);
        let first = address(2000);
        let second = address(2001);

        dead.add(first);
        dead.add(second);
        assert!(dead.is_dead(&first));
        assert!(dead.is_dead(&second));

        // Only the second address's timer is live; the first was cancelled
        // and stays dead forever.
        clock.set(DEAD_FOR_MICROS as u64 + 1);
        dispatcher.process_timers();
        assert!(dead.is_dead(&first), "the cancelled timer never forgets it");
        assert!(!dead.is_dead(&second), "the second address was forgotten");
        assert_eq!(dead.count(), 1);
    }
}
