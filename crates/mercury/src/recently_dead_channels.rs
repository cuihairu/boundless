//! `Mercury::RecentlyDeadChannels` (lib/network/recently_dead_channels.
//! {hpp,cpp}): the external interface's short memory of deregistered
//! channels (BW-02-05-03).
//!
//! A channel that dies -- an auth failure tears a client's session down --
//! leaves its address in this collection for a minute, and the receive
//! path drops datagrams from those addresses
//! (packet_receiver.cpp:474-487). Without the guard, packets from a
//! recently disconnected client would be processed raw once the channel
//! is gone, generating spurious corruption warnings. The guard sits
//! before the connection filter, so a dead session neither dispatches
//! nor re-auths.

use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use foundation::time_queue::{TimerHandle, TimerHandlerFn};

use crate::endpoint::Address;
use crate::event_dispatcher::EventDispatcher;
use crate::network_interface::NetworkInterface;

/// The once-off timeout `add` arms (`60 * 1000000`,
/// recently_dead_channels.cpp:38): a minute, in micros.
const RECENTLY_DEAD_MICROS: i64 = 60_000_000;

/// `Mercury::RecentlyDeadChannels` (recently_dead_channels.hpp:21).
pub struct RecentlyDeadChannels {
    /// `channels_`: addresses of recently deregistered channels mapped to
    /// their timeout handles. The port keeps the reference's ordered map
    /// (`BW::map<Address, TimerHandle>`), so `begin()` is the lowest
    /// address.
    channels: BTreeMap<Address, TimerHandle<u64>>,
}

impl Default for RecentlyDeadChannels {
    fn default() -> Self {
        RecentlyDeadChannels::new()
    }
}

impl RecentlyDeadChannels {
    /// `RecentlyDeadChannels( dispatcher )`.
    pub fn new() -> Self {
        RecentlyDeadChannels {
            channels: BTreeMap::new(),
        }
    }

    /// `add( addr )` (recently_dead_channels.cpp:33): remember the
    /// address and arm its minute-long timeout. The reference cancels the
    /// *first* entry's timer before inserting -- not the address's own
    /// previous one -- so the lowest-addressed entry loses its expiry and
    /// lingers until something else removes it. The port keeps that
    /// behaviour as it stands.
    ///
    /// `handleTimeout` (the reference's own method, :48) erases the entry
    /// whose timer fired; the timer reaches it the way every other port
    /// timer does, through a weak handle of the interface.
    pub fn add(
        &mut self,
        addr: &Address,
        dispatcher: &Rc<EventDispatcher>,
        interface: &Weak<NetworkInterface>,
    ) {
        if let Some((_, first)) = self.channels.iter_mut().next() {
            first.cancel();
        }

        let interface = interface.clone();
        let handler = Rc::new(TimerHandlerFn::new(
            move |handle: TimerHandle<u64>, _user: usize| {
                if let Some(interface) = interface.upgrade() {
                    interface.forget_recently_dead(handle);
                }
            },
            "RecentlyDeadChannels",
        ));
        let handle =
            dispatcher.add_once_off_timer(RECENTLY_DEAD_MICROS, handler, 0, "RecentlyDeadChannels");
        self.channels.insert(*addr, handle);
    }

    /// `isDead( addr )` (recently_dead_channels.cpp:68).
    pub fn is_dead(&self, addr: &Address) -> bool {
        self.channels.contains_key(addr)
    }

    /// `handleTimeout` (recently_dead_channels.cpp:48): the timer fired,
    /// so the address it armed for is forgotten.
    pub fn forget_by_handle(&mut self, handle: TimerHandle<u64>) {
        let expired = self
            .channels
            .iter()
            .find(|(_, other)| **other == handle)
            .map(|(addr, _)| *addr);
        if let Some(addr) = expired {
            self.channels.remove(&addr);
        }
    }
}

impl Drop for RecentlyDeadChannels {
    /// `~RecentlyDeadChannels` (recently_dead_channels.cpp:19): cancel
    /// every outstanding timer.
    fn drop(&mut self) {
        for handle in self.channels.values_mut() {
            handle.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::Reason;
    use crate::packet_receiver::tests_support::RecordingSink;
    use crate::udp_channel::{
        DEFAULT_INACTIVITY_RESEND_DELAY, Traits as ChannelTraits, UdpChannel,
    };
    use platform::socket;
    use std::cell::Cell;

    /// A socketless external interface with a pinned clock.
    fn external_interface() -> (Rc<NetworkInterface>, Rc<Cell<u64>>) {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || cell.get())));
        let interface = NetworkInterface::new_with_dispatcher(
            crate::network_interface::InterfaceType::External,
            dispatcher,
        );
        (interface, clock)
    }

    /// Register a channel at `port` and deregister it: the address lands
    /// in the recently-dead collection the way `deregisterChannel` puts
    /// it there.
    fn dead_at(interface: &Rc<NetworkInterface>, port: u16) -> Address {
        let addr = Address::new(0x0100_007f, socket::htons(port));
        let channel = UdpChannel::new(
            interface,
            addr,
            ChannelTraits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        );
        interface.deregister_channel(&channel);
        addr
    }

    /// `add` then `isDead` -- the address is remembered immediately, and
    /// the minute-long once-off is what forgets it.
    #[test]
    fn a_dead_address_is_forgotten_when_its_timer_fires() {
        let (interface, clock) = external_interface();
        let addr = dead_at(&interface, 9301);
        assert!(interface.is_dead(&addr));

        clock.set(1_000_000 + 59_999_999);
        interface.dispatcher().process_timers();
        assert!(interface.is_dead(&addr), "a minute has not passed");

        clock.set(1_000_000 + 60_000_001);
        interface.dispatcher().process_timers();
        assert!(!interface.is_dead(&addr), "the entry expired");
    }

    /// `add` again for an already-dead address replaces its timer: the
    /// expiry counts from the most recent add. The second deregistration
    /// is the reference's unconditional `add` -- it runs even though the
    /// map has already forgotten the channel.
    #[test]
    fn re_adding_an_address_renews_its_timer() {
        let (interface, clock) = external_interface();
        let addr = Address::new(0x0100_007f, socket::htons(9302));
        let channel = UdpChannel::new(
            &interface,
            addr,
            ChannelTraits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            crate::misc::CHANNEL_ID_NULL,
        );
        interface.deregister_channel(&channel);
        assert!(interface.is_dead(&addr));

        clock.set(1_000_000 + 30_000_000);
        interface.deregister_channel(&channel);

        // The original timer's minute passes without effect (it was
        // cancelled); the renewal's does not.
        clock.set(1_000_000 + 60_000_001);
        interface.dispatcher().process_timers();
        assert!(interface.is_dead(&addr), "the renewal still runs");

        clock.set(1_000_000 + 90_000_001);
        interface.dispatcher().process_timers();
        assert!(!interface.is_dead(&addr), "the renewal expired");
    }

    /// The reference's `add` cancels the *first* entry's timer: adding a
    /// second address strands the first (its timer no longer fires) while
    /// the second expires normally.
    #[test]
    fn a_new_add_cancels_the_first_entrys_timer() {
        let (interface, clock) = external_interface();
        let first = dead_at(&interface, 9303);
        let second = dead_at(&interface, 9304);

        clock.set(1_000_000 + 60_000_001);
        interface.dispatcher().process_timers();
        assert!(
            !interface.is_dead(&second),
            "the second entry's timer fired"
        );
        assert!(
            interface.is_dead(&first),
            "the first entry's timer was cancelled, so it lingers"
        );
    }

    /// `~RecentlyDeadChannels` cancels its timers: dropping the interface
    /// leaves nothing to fire.
    #[test]
    fn dropping_the_interface_cancels_its_timers() {
        let (interface, clock) = external_interface();
        let dispatcher = interface.dispatcher();
        dead_at(&interface, 9305);
        clock.set(1_000_000 + 60_000_001);
        drop(interface);
        assert_eq!(
            dispatcher.process_timers(),
            0,
            "the cancelled timer never fires"
        );
    }

    /// The guard arm: a dead address's datagram is dropped with
    /// `REASON_SUCCESS` and never reaches the dispatch.
    #[test]
    fn a_dead_address_drops_its_datagram() {
        let (interface, _clock) = external_interface();
        let sink = Rc::new(RecordingSink::default());
        interface.set_packet_sink(Some(sink.clone()));
        let addr = dead_at(&interface, 9306);

        let mut packet = crate::packet::Packet::new();
        packet.set_flags(crate::packet::flags::ON_CHANNEL);
        packet.grow(3);
        packet.buffer_mut()[2] = 0x41;
        assert_eq!(
            interface.process_packet(&addr, &mut packet),
            Reason::Success,
            "the guard answers before the connection filter"
        );
        assert!(sink.packets.borrow().is_empty());
    }
}
