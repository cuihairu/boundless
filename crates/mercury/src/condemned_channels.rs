//! `Mercury::CondemnedChannels` (`lib/network/condemned_channels.hpp/.cpp`):
//! the recycling lane for condemned channels.
//!
//! A channel that has been condemned (detached from its owner, awaiting
//! death) is held here until it has no unacked packets, then destroyed. A
//! condemned channel that has been idle for [`AGE_LIMIT`] seconds is
//! recycled regardless.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::misc::ChannelID;
use crate::network_interface::ChannelRef;
use crate::udp_channel::UDPChannel;
use foundation::time_queue::{TimerHandle, TimerHandler, TimerHandlerFn};

/// `CondemnedChannels::AGE_LIMIT` (condemned_channels.hpp:31): seconds a
/// condemned channel may sit idle before it is recycled.
pub const AGE_LIMIT: u64 = 60;

/// `stampsPerSecond()`: this dispatcher's microsecond stamps.
const STAMPS_PER_SECOND: u64 = 1_000_000;

/// `AGE_LIMIT` in stamps, the comparison `shouldDelete` makes against a
/// channel's last activity.
const AGE_LIMIT_STAMPS: u64 = AGE_LIMIT * STAMPS_PER_SECOND;

/// `CondemnedChannels`: condemned channels awaiting recycling.
///
/// The collections and the timer handle are behind a [`RefCell`]: the
/// timer that fires this recycle runs while the interface holds an `Rc` to
/// this object, so a plain `&mut self` would not be reachable.
pub struct CondemnedChannels {
    /// `nonIndexedChannels_`.
    non_indexed: RefCell<Vec<Rc<UDPChannel>>>,
    /// `indexedChannels_`.
    indexed: RefCell<HashMap<ChannelID, Rc<UDPChannel>>>,
    /// `timerHandle_`: the recurring recycle timer.
    timer_handle: RefCell<TimerHandle<u64>>,
}

impl CondemnedChannels {
    /// `CondemnedChannels()`.
    pub fn new() -> Self {
        Self {
            non_indexed: RefCell::new(Vec::new()),
            indexed: RefCell::new(HashMap::new()),
            timer_handle: RefCell::new(TimerHandle::default()),
        }
    }

    /// `add( pChannel )` (condemned_channels.cpp:22): take the channel for
    /// recycling -- immediately when it has nothing to wait for, otherwise
    /// queued and watched by the recurring timer.
    pub fn add(self: &Rc<Self>, channel: Rc<UDPChannel>, now: u64) {
        if self.should_delete(&channel, now) {
            channel.destroy();
            return;
        }

        if channel.is_indexed() {
            if let Some(old) = self
                .indexed
                .borrow_mut()
                .insert(channel.id(), Rc::clone(&channel))
            {
                old.destroy();
                eprintln!(
                    "CondemnedChannels::add( {} ): Already have a channel with id {}",
                    channel.c_str(),
                    channel.id()
                );
            }
        } else {
            self.non_indexed.borrow_mut().push(Rc::clone(&channel));
        }

        if !self.timer_handle.borrow().is_set() {
            let weak = Rc::downgrade(self);
            let dispatcher = channel.network_interface().dispatcher();
            let weak_dispatcher = Rc::downgrade(&dispatcher);
            let handler: Rc<dyn TimerHandler<u64>> = Rc::new(TimerHandlerFn::new(
                move |_handle: TimerHandle<u64>, _user: usize| {
                    let Some(channels) = weak.upgrade() else {
                        return;
                    };
                    let now = weak_dispatcher
                        .upgrade()
                        .map(|dispatcher| dispatcher.now())
                        .unwrap_or(0);
                    channels.delete_finished_channels(now);
                },
                "CondemnedChannels",
            ));
            let handle = dispatcher.add_timer(
                (AGE_LIMIT * 1_000_000) as i64,
                handler,
                0,
                "CondemnedChannels",
            );
            *self.timer_handle.borrow_mut() = handle;
        }
    }

    /// `find( channelID )` (condemned_channels.cpp:46).
    pub fn find(&self, channel_id: ChannelID) -> Option<Rc<UDPChannel>> {
        self.indexed.borrow().get(&channel_id).cloned()
    }

    /// `deleteFinishedChannels()` (condemned_channels.cpp:95): destroy every
    /// condemned channel that is finished, and cancel the timer once none
    /// are left. Returns whether the collection is now empty.
    pub fn delete_finished_channels(&self, now: u64) -> bool {
        let mut index = 0;
        while index < self.non_indexed.borrow().len() {
            let channel = Rc::clone(&self.non_indexed.borrow()[index]);
            if self.should_delete(&channel, now) {
                channel.destroy();
                self.non_indexed.borrow_mut().remove(index);
            } else {
                index += 1;
            }
        }

        let finished: Vec<ChannelID> = {
            let indexed = self.indexed.borrow();
            indexed
                .iter()
                .filter(|(_id, channel)| self.should_delete(channel, now))
                .map(|(id, _channel)| *id)
                .collect()
        };
        for id in finished {
            if let Some(channel) = self.indexed.borrow_mut().remove(&id) {
                channel.destroy();
            }
        }

        let is_empty = self.non_indexed.borrow().is_empty() && self.indexed.borrow().is_empty();
        if is_empty {
            self.timer_handle.borrow_mut().cancel();
        }
        is_empty
    }

    /// `numCriticalChannels()` (condemned_channels.cpp:133): condemned
    /// channels with unacked criticals.
    pub fn num_critical_channels(&self) -> usize {
        let non_indexed = self
            .non_indexed
            .borrow()
            .iter()
            .filter(|channel| channel.has_unacked_criticals())
            .count();
        let indexed = self
            .indexed
            .borrow()
            .values()
            .filter(|channel| channel.has_unacked_criticals())
            .count();
        non_indexed + indexed
    }

    /// `hasUnackedPackets()` (condemned_channels.cpp:158): any condemned
    /// channel still has outstanding packets.
    pub fn has_unacked_packets(&self) -> bool {
        self.non_indexed
            .borrow()
            .iter()
            .any(|channel| channel.has_unacked_packets())
            || self
                .indexed
                .borrow()
                .values()
                .any(|channel| channel.has_unacked_packets())
    }

    /// How many channels are condemned (the tests' window on the list).
    pub fn count(&self) -> usize {
        self.non_indexed.borrow().len() + self.indexed.borrow().len()
    }

    /// `timerHandle_.isSet()`: the recurring recycle timer is armed.
    pub fn has_timer(&self) -> bool {
        self.timer_handle.borrow().is_set()
    }

    /// `shouldDelete( pChannel, now )` (condemned_channels.cpp:66): a
    /// condemned channel with nothing outstanding is finished, and one idle
    /// for [`AGE_LIMIT`] seconds is recycled regardless.
    fn should_delete(&self, p_channel: &Rc<UDPChannel>, now: u64) -> bool {
        let mut should_delete = !p_channel.has_unacked_packets() || p_channel.has_remote_failed();

        // Timed out if nothing was sent or received for a while.
        if !should_delete
            && now.wrapping_sub(p_channel.last_received_time()) > AGE_LIMIT_STAMPS
            && now.wrapping_sub(p_channel.last_reliable_send_time()) > AGE_LIMIT_STAMPS
        {
            eprintln!(
                "CondemnedChannels::handleTimeout: Condemned channel {} has timed out.",
                p_channel.c_str()
            );
            should_delete = true;
        }

        should_delete
    }
}

impl Default for CondemnedChannels {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::Address;
    use crate::misc::CHANNEL_ID_NULL;
    use crate::network_interface::{InterfaceType, NetworkInterface};
    use crate::udp_channel::ChannelTraits;

    const LOOPBACK: u32 = 0x0100_007f;

    fn interface() -> Rc<NetworkInterface> {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        interface
    }

    fn channel(interface: &Rc<NetworkInterface>, port: u16) -> Rc<UDPChannel> {
        UDPChannel::new(
            interface,
            Address::new(LOOPBACK, port),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        )
    }

    /// BW-02-03-02: a condemned channel with nothing outstanding is
    /// recycled at the `add` pass itself (condemned_channels.cpp:22-27),
    /// so it never sits in the collection.
    #[test]
    fn a_condemned_channel_with_nothing_outstanding_is_recycled_at_once() {
        let interface = interface();
        let channel = channel(&interface, 1000);
        let addr = channel.addr();

        channel.condemn();
        assert!(channel.is_condemned());
        assert!(channel.is_destroyed(), "nothing was outstanding");
        assert_eq!(
            interface.condemned_channels().count(),
            0,
            "the channel never queued"
        );
        assert!(
            interface.find_channel(&addr).is_none(),
            "the destroyed channel deregistered"
        );
    }

    /// A condemned channel that still has unacked packets is kept and
    /// recycled only once the window drains -- the reference drives this
    /// off the recurring timer (condemned_channels.cpp:104-155).
    #[test]
    fn a_condemned_channel_is_kept_while_it_has_unacked_packets() {
        let interface = interface();
        let channel = channel(&interface, 1001);
        channel.debug_set_unacked(true);

        interface.condemned_channels_add(Rc::clone(&channel));
        assert_eq!(interface.condemned_channels().count(), 1, "it was queued");
        assert!(!channel.is_destroyed());
        assert!(
            interface.condemned_channels().has_timer(),
            "the recurring timer is armed while a channel is condemned"
        );
        assert!(interface.condemned_channels().has_unacked_packets());

        // The recycle pass leaves a still-unacked channel in place.
        assert!(
            !interface
                .condemned_channels()
                .delete_finished_channels(interface.dispatcher().now())
        );
        assert!(!channel.is_destroyed());

        // The window drains: the next pass destroys it and cancels the timer.
        channel.debug_set_unacked(false);
        assert!(
            interface
                .condemned_channels()
                .delete_finished_channels(interface.dispatcher().now())
        );
        assert!(channel.is_destroyed());
        assert_eq!(interface.condemned_channels().count(), 0);
        assert!(!interface.condemned_channels().has_timer());
    }

    /// An anonymous channel whose remote end failed is condemned and
    /// destroyed on the spot, through the interface's `delAnonymousChannel`
    /// (network_interface.cpp:711, condemned_channels.cpp:22-27).
    #[test]
    fn a_dead_anonymous_channel_is_cleaned_up() {
        let interface = interface();
        let addr = Address::new(LOOPBACK, 1002);
        let channel = interface
            .find_or_create_channel(&addr, true)
            .expect("an anonymous channel is created for the address");
        assert!(channel.is_anonymous());

        interface.on_address_dead(&addr);

        assert!(
            channel.has_remote_failed(),
            "the anonymous channel was marked remote-failed"
        );
        assert!(channel.is_condemned());
        assert!(
            channel.is_destroyed(),
            "a remote-failed channel is recycled at the add pass"
        );
        assert_eq!(interface.condemned_channels().count(), 0);
        assert!(
            interface.find_channel_owned(&addr).is_none(),
            "the channel deregistered"
        );
    }

    /// An indexed channel replaces the older one with the same id
    /// (condemned_channels.cpp:34-42).
    #[test]
    fn an_indexed_channel_replaces_the_older_one() {
        let interface = interface();
        let first = UDPChannel::new(
            &interface,
            Address::new(LOOPBACK, 1003),
            ChannelTraits::Internal,
            42,
        );
        let second = UDPChannel::new(
            &interface,
            Address::new(LOOPBACK, 1004),
            ChannelTraits::Internal,
            42,
        );
        first.debug_set_unacked(true);
        second.debug_set_unacked(true);

        interface.condemned_channels_add(Rc::clone(&first));
        interface.condemned_channels_add(Rc::clone(&second));

        assert_eq!(
            interface.condemned_channels().count(),
            1,
            "one id, one channel"
        );
        assert!(
            first.is_destroyed(),
            "the older indexed channel is recycled"
        );
        assert!(!second.is_destroyed());
        assert!(
            interface
                .find_condemned_channel(42)
                .is_some_and(|found| found.id() == 42),
            "the replacement is findable by id"
        );
        assert_eq!(
            interface.condemned_channels().num_critical_channels(),
            0,
            "neither has unacked criticals"
        );
    }
}
