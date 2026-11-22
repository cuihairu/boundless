//! `lib/network/condemned_channels.cpp` -- where condemned channels wait
//! to be deleted (BW-02-03-02).
//!
//! A condemned channel is kept alive only until its unacked packets
//! drain: a sweep timer ages the collections out at the 60-second limit,
//! and `deleteFinishedChannels` destroys whatever is ready.

use std::collections::HashMap;
use std::rc::Rc;

use foundation::time_queue::{TimerHandle, TimerHandlerFn};

use crate::event_dispatcher::EventDispatcher;
use crate::misc::{ChannelID, SEQ_NULL};
use crate::network_interface::NetworkInterface;
use crate::udp_channel::{ChannelPtr, UdpChannel};

/// `AGE_LIMIT` (condemned_channels.cpp:14): how long a condemned channel
/// may hold unacked data before the sweep forces it out. The port's
/// stamps are micros.
const AGE_LIMIT_MICROS: u64 = 60_000_000;

/// `Mercury::CondemnedChannels` (condemned_channels.hpp:23).
#[derive(Default)]
pub struct CondemnedChannels {
    /// `channels_`: the non-indexed condemned channels.
    non_indexed: Vec<ChannelPtr>,
    /// `map_`: the indexed condemned channels, by id.
    indexed: HashMap<ChannelID, ChannelPtr>,
    /// `timerHandle_`: the one-shot sweep.
    timer_handle: TimerHandle<u64>,
}

impl CondemnedChannels {
    /// `CondemnedChannels()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `CondemnedChannels::add( pChannel )` (condemned_channels.cpp:20):
    /// destroy the channel outright when there is nothing to wait for,
    /// else file it under its id or push it on the list, and arm the
    /// sweep timer once.
    pub fn add(
        &mut self,
        channel: &ChannelPtr,
        dispatcher: &Rc<EventDispatcher>,
        interface: &std::rc::Weak<NetworkInterface>,
    ) {
        let now = dispatcher.now();
        if Self::should_delete(channel, now) {
            UdpChannel::destroy(channel);
            return;
        }

        let (is_indexed, id) = {
            let this = channel.borrow();
            (this.is_indexed(), this.id())
        };

        if is_indexed {
            if let Some(existing) = self.indexed.remove(&id) {
                eprintln!("CondemnedChannels::add: Already have a channel with id {id}");
                UdpChannel::destroy(&existing);
            }
            self.indexed.insert(id, channel.clone());
        } else {
            self.non_indexed.push(channel.clone());
        }

        if !self.timer_handle.is_set() {
            let interface = interface.clone();
            let handler = Rc::new(TimerHandlerFn::new(
                move |_handle, _user| {
                    // `handleTimeout`: a full sweep; the timer cancels
                    // itself through the empty-collection path.
                    if let Some(interface) = interface.upgrade() {
                        interface.delete_finished_channels();
                    }
                },
                "CondemnedChannels",
            ));
            self.timer_handle = dispatcher.add_once_off_timer(
                AGE_LIMIT_MICROS as i64,
                handler,
                0,
                "CondemnedChannels",
            );
        }
    }

    /// `CondemnedChannels::shouldDelete( pChannel )`
    /// (condemned_channels.cpp:51): ready when nothing is outstanding --
    /// or when the peer has gone quiet past the age limit on both the
    /// receive and the reliable-send clocks, which forces the loss.
    fn should_delete(channel: &ChannelPtr, now: u64) -> bool {
        let (has_unacked, has_remote_failed, last_received, last_reliable_send, label) = {
            let this = channel.borrow();
            (
                this.oldest_unacked_seq(),
                this.has_remote_failed(),
                this.last_received_time(),
                this.last_reliable_send_time(),
                this.c_str(),
            )
        };

        if has_unacked == SEQ_NULL || has_remote_failed {
            return true;
        }

        if now.saturating_sub(last_received) > AGE_LIMIT_MICROS
            && now.saturating_sub(last_reliable_send) > AGE_LIMIT_MICROS
        {
            // The reference tags this warning with the timeout handler's
            // name even though it fires from `add` too; kept as-is.
            eprintln!(
                "CondemnedChannels::handleTimeout: Channel ({label}) hasn't acked for {:.1} seconds. Forcing deletion (may result in data loss)",
                now.saturating_sub(last_received) as f64 / 1_000_000.0
            );
            return true;
        }

        false
    }

    /// `CondemnedChannels::deleteFinishedChannels()`
    /// (condemned_channels.cpp:81): destroy every channel ready to go in
    /// both collections, and cancel the sweep when nothing is left.
    pub fn delete_finished_channels(&mut self, now: u64) {
        self.non_indexed.retain(|channel| {
            if Self::should_delete(channel, now) {
                UdpChannel::destroy(channel);
                false
            } else {
                true
            }
        });

        self.indexed.retain(|_id, channel| {
            if Self::should_delete(channel, now) {
                UdpChannel::destroy(channel);
                false
            } else {
                true
            }
        });

        if self.non_indexed.is_empty() && self.indexed.is_empty() {
            // Don't return a cancelling code like the reference's timer
            // comment says: the explicit cancel is the port's one shot.
            self.timer_handle.cancel();
        }
    }

    /// How many channels the collections hold, for tests.
    pub fn size(&self) -> usize {
        self.non_indexed.len() + self.indexed.len()
    }

    /// A test hook dropping every held channel without destroying them,
    /// so a test can drain a collection that has nothing to release.
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.non_indexed.clear();
        self.indexed.clear();
        self.timer_handle.cancel();
    }

    /// Test access to the non-indexed list.
    #[cfg(test)]
    pub fn non_indexed_for_test(&self) -> &[ChannelPtr] {
        &self.non_indexed
    }

    /// Test access to the indexed map.
    #[cfg(test)]
    pub fn indexed_for_test(&self) -> &HashMap<ChannelID, ChannelPtr> {
        &self.indexed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{Address, NONE};
    use crate::event_dispatcher::EventDispatcher;
    use crate::misc::CHANNEL_ID_NULL;
    use crate::network_interface::InterfaceType;
    use crate::udp_channel::{DEFAULT_INACTIVITY_RESEND_DELAY, Traits};
    use std::cell::Cell;

    /// An interface on a pinned clock with no socket.
    fn interface(clock: &Rc<Cell<u64>>) -> Rc<NetworkInterface> {
        let clock_cell = Rc::clone(clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_cell.get()
        })));
        NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher)
    }

    fn addr(ip: u32) -> Address {
        Address::new(ip, platform::socket::htons(20))
    }

    fn make_channel(interface: &Rc<NetworkInterface>, ip: u32) -> ChannelPtr {
        UdpChannel::new(
            interface,
            addr(ip),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        )
    }

    /// A channel with nothing outstanding destroys on the spot: there is
    /// nothing the sweep could wait for.
    #[test]
    fn a_quiet_channel_destroys_on_condemn() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0001);

        UdpChannel::condemn(&channel);
        assert!(channel.borrow().is_condemned());
        assert!(
            channel.borrow().is_destroyed(),
            "nothing unacked, nothing to wait for"
        );
        assert!(
            interface.find_channel(addr(0x0100_0001), false).is_none(),
            "the destroyed channel left the live map"
        );
        assert_eq!(interface.condemned_size(), 0);
    }

    /// An unacked channel survives the condemn and the first sweep, then
    /// leaves when its send window drains -- the age limit is not what
    /// frees it, the drain is.
    #[test]
    fn an_unacked_channel_is_held_until_it_drains() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0002);
        channel.borrow_mut().force_unacked(5);

        UdpChannel::condemn(&channel);
        assert!(
            !channel.borrow().is_destroyed(),
            "unacked data holds the channel"
        );
        assert_eq!(interface.condemned_size(), 1);
        assert!(
            interface.find_channel(addr(0x0100_0002), false).is_some(),
            "the condemned channel is still registered while it drains"
        );

        // A sweep inside the age limit: still held.
        clock.set(2_000_000);
        interface.delete_finished_channels();
        assert_eq!(interface.condemned_size(), 1);
        assert!(!channel.borrow().is_destroyed());

        // The window drains; the next sweep frees it.
        channel.borrow_mut().force_unacked(SEQ_NULL);
        clock.set(3_000_000);
        interface.delete_finished_channels();
        assert_eq!(interface.condemned_size(), 0);
        assert!(channel.borrow().is_destroyed());
    }

    /// A remote-failed channel has nothing to wait for either, however
    /// much is unacked.
    #[test]
    fn a_remote_failed_channel_destroys_on_condemn() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0003);
        channel.borrow_mut().force_unacked(9);

        UdpChannel::set_remote_failed(&channel);
        UdpChannel::condemn(&channel);
        assert!(channel.borrow().is_destroyed());
        assert_eq!(interface.condemned_size(), 0);
    }

    /// An indexed condemned channel files under its id, and a successor
    /// with the same id destroys it -- one condemned slot per id.
    #[test]
    fn an_indexed_slot_replaces_its_condemned_predecessor() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);

        let first = UdpChannel::new(
            &interface,
            addr(0x0100_0004),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            7,
        );
        first.borrow_mut().force_unacked(1);
        UdpChannel::condemn(&first);
        assert!(!first.borrow().is_destroyed());
        assert_eq!(interface.condemned_size(), 1);

        let second = UdpChannel::new(
            &interface,
            addr(0x0100_0005),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            7,
        );
        second.borrow_mut().force_unacked(2);
        UdpChannel::condemn(&second);
        assert!(
            first.borrow().is_destroyed(),
            "the id's previous tenant was destroyed"
        );
        assert!(!second.borrow().is_destroyed());
        assert_eq!(interface.condemned_size(), 1, "one condemned slot per id");
        interface.condemned_clear_for_test();
    }

    /// The sweep timer destroys channels that have been silent on both
    /// clocks past the 60-second age limit -- 验收: the condemned sweep
    /// releases on the dispatcher itself.
    #[test]
    fn the_sweep_timer_releases_aged_channels() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0006);
        channel.borrow_mut().force_unacked(3);

        UdpChannel::condemn(&channel);
        assert_eq!(interface.condemned_size(), 1);

        // 59 seconds of silence: inside the limit, still held.
        clock.set(1_000_000 + 59_000_000);
        interface.dispatcher().process_timers();
        assert!(!channel.borrow().is_destroyed());

        // The 60-second one-shot fires: the channel has not received
        // since before the condemn and never sent reliably, so the sweep
        // forces it out.
        clock.set(1_000_000 + 60_000_000 + 1);
        interface.dispatcher().process_timers();
        assert!(channel.borrow().is_destroyed());
        assert_eq!(interface.condemned_size(), 0);

        // The timer cancelled itself: a later pass has nothing to do.
        clock.set(1_000_000 + 120_000_000);
        interface.dispatcher().process_timers();
        assert_eq!(interface.condemned_size(), 0);
    }

    /// A channel that kept receiving stays through the sweep: the age
    /// limit counts from the last receive, not the condemn.
    #[test]
    fn fresh_traffic_survives_the_sweep() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0007);
        channel.borrow_mut().force_unacked(4);

        UdpChannel::condemn(&channel);

        // Received reliably two seconds ago, but also sent reliably two
        // seconds ago: the pair of clocks is what forces, and one is
        // young.
        clock.set(3_000_000);
        channel.borrow_mut().force_last_received_time(1_000_000);
        channel
            .borrow_mut()
            .force_last_reliable_send_time(1_000_000);
        interface.delete_finished_channels();
        assert!(!channel.borrow().is_destroyed());
        assert_eq!(interface.condemned_size(), 1);

        // Drop the holds directly to leave nothing dangling for the
        // interface's own timer.
        interface.condemned_clear_for_test();
    }

    /// An indexed channel rides out the sweep while it holds unacked
    /// data, then leaves when its window drains -- the indexed half of
    /// `deleteFinishedChannels`.
    #[test]
    fn an_indexed_channel_is_destroyed_by_the_sweep_when_it_drains() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_000b),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            21,
        );
        channel.borrow_mut().force_unacked(6);
        UdpChannel::condemn(&channel);
        assert!(!channel.borrow().is_destroyed());
        assert_eq!(interface.condemned_size(), 1);

        // Inside the age limit the sweep holds it.
        clock.set(2_000_000);
        interface.delete_finished_channels();
        assert!(!channel.borrow().is_destroyed());

        // The window drains; the sweep takes it from the indexed map.
        channel.borrow_mut().force_unacked(SEQ_NULL);
        clock.set(3_000_000);
        interface.delete_finished_channels();
        assert!(channel.borrow().is_destroyed());
        assert_eq!(interface.condemned_size(), 0);
    }

    /// The lookup helper answers a held channel by address, live or
    /// indexed, and answers nothing for strangers.
    #[test]
    fn the_lookup_answers_by_address() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);

        let plain = make_channel(&interface, 0x0100_0008);
        plain.borrow_mut().force_unacked(1);
        UdpChannel::condemn(&plain);

        let indexed = UdpChannel::new(
            &interface,
            addr(0x0100_0009),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            11,
        );
        indexed.borrow_mut().force_unacked(2);
        UdpChannel::condemn(&indexed);

        let condemned = &interface.condemned_channels();
        let held = condemned.borrow();
        let find = |addr: Address| {
            held.non_indexed_for_test()
                .iter()
                .find(|channel| channel.borrow().addr() == addr)
                .or_else(|| {
                    held.indexed_for_test()
                        .values()
                        .find(|channel| channel.borrow().addr() == addr)
                })
        };
        assert!(Rc::ptr_eq(find(addr(0x0100_0008)).unwrap(), &plain));
        assert!(Rc::ptr_eq(find(addr(0x0100_0009)).unwrap(), &indexed));
        assert!(find(addr(0x0100_000a)).is_none());
        assert!(find(NONE).is_none());
        drop(held);
        interface.condemned_clear_for_test();
    }
}
