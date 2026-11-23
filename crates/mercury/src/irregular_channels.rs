//! `lib/network/irregular_channels.cpp` + its `MonitoredChannels` base --
//! the one shared timer that drives irregular channels' resends
//! (BW-02-03-04).
//!
//! The reference has no per-channel resend timer: a channel whose sends
//! are not paced by a game tick (`isLocalRegular( false )`) files itself
//! into this collection, and this collection's single repeating timer
//! wakes every period to give each member its resend check. The
//! `MonitoredChannels` base is folded in, its `BW::list` iterator
//! bookkeeping replaced by membership search -- the same trade the
//! condemned collection made.

use std::rc::{Rc, Weak};

use foundation::time_queue::{TimerHandle, TimerHandlerFn};

use crate::event_dispatcher::EventDispatcher;
use crate::network_interface::NetworkInterface;
use crate::udp_channel::{ChannelPtr, UdpChannel};

/// `Mercury::IrregularChannels` (irregular_channels.hpp:20), with
/// `Mercury::MonitoredChannels` (monitored_channels.hpp:43) folded in.
#[derive(Default)]
pub struct IrregularChannels {
    /// `channels_`.
    channels: Vec<ChannelPtr>,
    /// `period_`, in seconds.
    period: f32,
    /// `timerHandle_`.
    timer_handle: TimerHandle<u64>,
}

impl IrregularChannels {
    /// `MonitoredChannels()`: `period_( 0.f ), timerHandle_()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `defaultPeriod()` (irregular_channels.cpp:46): the period the first
    /// member arms the timer with, when nobody configured one.
    pub fn default_period() -> f32 {
        1.0
    }

    /// `MonitoredChannels::setPeriod( seconds, dispatcher )`
    /// (monitored_channels.cpp:24): re-arm the shared timer, cancelling
    /// whatever ran before; zero or less stops the monitoring.
    pub fn set_period(
        &mut self,
        seconds: f32,
        dispatcher: &Rc<EventDispatcher>,
        interface: &Weak<NetworkInterface>,
    ) {
        self.timer_handle.cancel();

        if seconds > 0.0 {
            let interface = interface.clone();
            let handler = Rc::new(TimerHandlerFn::new(
                move |_handle, _user| {
                    // `handleTimeout`: the whole collection gets its resend
                    // check; the timer cancels itself through the
                    // empty-collection path.
                    if let Some(interface) = interface.upgrade() {
                        interface.check_irregular_channels();
                    }
                },
                "IrregularChannels",
            ));
            self.timer_handle = dispatcher.add_timer(
                (seconds * 1_000_000.0) as i64,
                handler,
                0,
                "IrregularChannels",
            );
        }
        self.period = seconds;
    }

    // `MonitoredChannels::stopMonitoring( dispatcher )`
    // (monitored_channels.cpp:38) is spelled `set_period( 0, ... )` here:
    // the tree's only `stopMonitoring` call targets the keep-alive
    // collection (network_interface.cpp:259), which lands with its own
    // task.

    /// `IrregularChannels::addIfNecessary( channel )`
    /// (irregular_channels.cpp:26): remember the channel for resend
    /// checking only if it is irregular and owes packets -- and only if it
    /// is not stored already. The first member arms the timer at the
    /// default period if no period was set.
    pub fn add_if_necessary(
        &mut self,
        channel: &ChannelPtr,
        dispatcher: &Rc<EventDispatcher>,
        interface: &Weak<NetworkInterface>,
    ) {
        let (is_local_regular, has_unacked) = {
            let this = channel.borrow();
            (
                UdpChannel::is_local_regular(channel),
                this.has_unacked_packets(),
            )
        };

        if is_local_regular || !has_unacked {
            return;
        }

        if !self.contains(channel) {
            self.channels.push(channel.clone());

            if !self.timer_handle.is_set() {
                self.set_period(Self::default_period(), dispatcher, interface);
            }
        }
    }

    /// `MonitoredChannels::delIfNecessary( channel )`
    /// (monitored_channels.cpp:63): forget the channel if it is stored.
    pub fn del_if_necessary(&mut self, channel: &ChannelPtr) {
        self.channels.retain(|held| !Rc::ptr_eq(held, channel));
    }

    /// `IrregularChannels::handleTimeout` (irregular_channels.cpp:56):
    /// every member still owing packets and still irregular gets its
    /// resend check; the rest, and any member the check found dead, leave
    /// the collection.
    pub fn check(&mut self) {
        // The reference post-increments its list iterator before
        // `delIfNecessary`, so erasing inside the loop is safe; the port
        // rebuilds the remainder instead, which is the same walk.
        let channels = std::mem::take(&mut self.channels);
        let mut remaining = Vec::with_capacity(channels.len());

        for channel in channels {
            let (has_unacked, is_local_regular) = {
                let this = channel.borrow();
                (
                    this.has_unacked_packets(),
                    UdpChannel::is_local_regular(&channel),
                )
            };

            if has_unacked && !is_local_regular {
                // `channel.send()` in the reference: the send path runs its
                // own resend check, piggybacking onto the channel's bundle.
                UdpChannel::send(&channel);

                // checkResendTimers() sets remote failure status
                if channel.borrow().has_remote_failed() {
                    eprintln!(
                        "IrregularChannels::handleTimeout: Removing dead channel to {}",
                        channel.borrow().c_str()
                    );
                } else {
                    remaining.push(channel);
                }
            }
            // else: `delIfNecessary` -- the drained and the regular leave.
        }

        self.channels = remaining;
    }

    /// Whether the collection holds this channel. The reference answers
    /// through the channel's stored list iterator; the port searches.
    fn contains(&self, channel: &ChannelPtr) -> bool {
        self.channels.iter().any(|held| Rc::ptr_eq(held, channel))
    }

    /// `period_`, in seconds.
    pub fn period(&self) -> f32 {
        self.period
    }

    /// How many channels the collection holds, for tests.
    pub fn size(&self) -> usize {
        self.channels.len()
    }

    /// A test hook dropping every held channel and stopping the timer,
    /// so a test can leave nothing behind.
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.channels.clear();
        self.timer_handle.cancel();
        self.period = 0.0;
    }

    /// Test access to the member list.
    #[cfg(test)]
    pub fn channels_for_test(&self) -> &[ChannelPtr] {
        &self.channels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::Address;
    use crate::misc::{CHANNEL_ID_NULL, SEQ_NULL, SeqNum};
    use crate::network_interface::InterfaceType;
    use crate::packet::Packet;
    use crate::udp_channel::{DEFAULT_INACTIVITY_RESEND_DELAY, Traits};
    use std::cell::Cell;

    /// An interface on a pinned clock with no socket.
    fn interface(clock: &Rc<Cell<u64>>) -> Rc<NetworkInterface> {
        let clock_cell = Rc::clone(clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_cell.get()
        })));
        NetworkInterface::new_with_dispatcher(InterfaceType::External, dispatcher)
    }

    fn addr(ip: u32) -> Address {
        Address::new(ip, platform::socket::htons(20))
    }

    fn make_channel(interface: &Rc<NetworkInterface>, ip: u32) -> ChannelPtr {
        UdpChannel::new(
            interface,
            addr(ip),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        )
    }

    /// Send one reliable packet: the bookkeeping a real send's finalise
    /// does, minus the wire (the send path is BW-02-03-06).
    fn send(channel: &ChannelPtr) -> SeqNum {
        let seq = UdpChannel::use_next_sequence_id(channel);
        let mut packet = Packet::new();
        packet.set_seq(seq);
        assert!(
            UdpChannel::add_resend_timer(channel, seq, Rc::new(packet), &[]),
            "the send window has room"
        );
        seq
    }

    /// A regular channel never joins: only irregular ones are tracked.
    #[test]
    fn a_regular_channel_never_joins() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0001);
        send(&channel);

        interface.add_irregular_channel_if_necessary(&channel);
        assert_eq!(interface.irregular_channels().borrow().size(), 0);
    }

    /// An irregular channel with nothing outstanding never joins either:
    /// there is nothing to resend.
    #[test]
    fn an_irregular_channel_with_nothing_unacked_never_joins() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0002);
        UdpChannel::set_local_regular(&channel, false);

        interface.add_irregular_channel_if_necessary(&channel);
        assert_eq!(interface.irregular_channels().borrow().size(), 0);
    }

    /// An irregular channel that owes packets joins once -- a second ask
    /// is a no-op -- and the first join arms the shared timer at the
    /// default period.
    #[test]
    fn joining_arms_the_shared_timer_at_the_default_period() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0003);
        UdpChannel::set_local_regular(&channel, false);
        send(&channel);

        interface.add_irregular_channel_if_necessary(&channel);
        interface.add_irregular_channel_if_necessary(&channel);
        {
            let irregular = interface.irregular_channels().borrow();
            assert_eq!(irregular.size(), 1);
            assert_eq!(irregular.period(), IrregularChannels::default_period());
            assert!(Rc::ptr_eq(&irregular.channels_for_test()[0], &channel));
        }

        interface.irregular_clear_for_test();
        assert_eq!(interface.irregular_channels().borrow().period(), 0.0);
    }

    /// `stopMonitoring` -- spelled `set_irregular_channels_resend_period(0)`
    /// on the interface -- cancels the timer and zeroes the period without
    /// touching the membership.
    #[test]
    fn stopping_monitoring_cancels_the_timer() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = make_channel(&interface, 0x0100_0004);
        UdpChannel::set_local_regular(&channel, false);
        send(&channel);
        interface.add_irregular_channel_if_necessary(&channel);
        assert_eq!(interface.irregular_channels().borrow().size(), 1);

        interface.set_irregular_channels_resend_period(0.0);

        let irregular = interface.irregular_channels().borrow();
        assert_eq!(irregular.period(), 0.0);
        assert_eq!(
            irregular.size(),
            1,
            "membership survives, the timer does not"
        );
        drop(irregular);
        interface.irregular_clear_for_test();
    }

    /// `delIfNecessary` forgets a member and ignores a stranger.
    #[test]
    fn deleting_forgets_members_and_ignores_strangers() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let member = make_channel(&interface, 0x0100_0005);
        UdpChannel::set_local_regular(&member, false);
        send(&member);
        interface.add_irregular_channel_if_necessary(&member);

        let stranger = make_channel(&interface, 0x0100_0006);
        interface.del_irregular_channel_if_necessary(&stranger);
        assert_eq!(interface.irregular_channels().borrow().size(), 1);

        interface.del_irregular_channel_if_necessary(&member);
        assert_eq!(interface.irregular_channels().borrow().size(), 0);
        interface.irregular_clear_for_test();
    }

    /// `handleTimeout`: a member still owing packets and still irregular
    /// keeps its resend check and its seat; a drained member leaves.
    #[test]
    fn the_check_keeps_debtors_and_drops_the_drained() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);

        let debtor = make_channel(&interface, 0x0100_0007);
        UdpChannel::set_local_regular(&debtor, false);
        send(&debtor);
        interface.add_irregular_channel_if_necessary(&debtor);

        let drained = make_channel(&interface, 0x0100_0008);
        UdpChannel::set_local_regular(&drained, false);
        send(&drained);
        interface.add_irregular_channel_if_necessary(&drained);
        assert_eq!(interface.irregular_channels().borrow().size(), 2);

        // The drained one pays its debt off the books.
        drained.borrow_mut().force_unacked(SEQ_NULL);
        interface.check_irregular_channels();

        let irregular = interface.irregular_channels().borrow();
        assert_eq!(irregular.size(), 1);
        assert!(Rc::ptr_eq(&irregular.channels_for_test()[0], &debtor));
        drop(irregular);
        interface.irregular_clear_for_test();
    }

    /// A member the check found dead leaves the collection -- the
    /// `Removing dead channel` arm.
    #[test]
    fn a_dead_member_leaves_the_collection() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);

        let channel = make_channel(&interface, 0x0100_0009);
        UdpChannel::set_local_regular(&channel, false);
        send(&channel);
        interface.add_irregular_channel_if_necessary(&channel);

        UdpChannel::set_remote_failed(&channel);
        interface.check_irregular_channels();

        assert_eq!(interface.irregular_channels().borrow().size(), 0);
    }

    /// The shared timer itself drives the checks: a member whose packet
    /// has aged past the resend period is resent by the dispatcher
    /// -- 验收: the resend clock runs on the interface's timer.
    #[test]
    fn the_shared_timer_drives_the_resends() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);
        interface.set_irregular_channels_resend_period(0.1);

        let channel = make_channel(&interface, 0x0100_000a);
        // Both regularity flags off, like the reference's own
        // lose-one-packet fixture (test_reliable.cpp:221-222): the local
        // flag files the channel here, the remote one makes the resend
        // decision time-based.
        UdpChannel::set_local_regular(&channel, false);
        UdpChannel::set_remote_regular(&channel, false);
        send(&channel);
        interface.add_irregular_channel_if_necessary(&channel);

        // Walk a little under two periods: nothing yet (the period is
        // max( RTT*2, minInactivityResendDelay ) = 2s on this channel).
        for _ in 0..19 {
            clock.set(clock.get() + 100_000);
            interface.dispatcher().process_timers();
        }
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // Exactly two seconds old is not "older than we'd like": the
        // reference's test is strict (`>`).
        clock.set(clock.get() + 100_000);
        interface.dispatcher().process_timers();
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // Past two seconds, the check resends -- once, and only once per
        // period, however many periods pile onto the same process_timers
        // catch-up pass: the resend re-stamps the slot's clock.
        clock.set(clock.get() + 100_000);
        interface.dispatcher().process_timers();
        assert_eq!(channel.borrow().num_packets_resent(), 1);

        clock.set(clock.get() + 100_000);
        interface.dispatcher().process_timers();
        assert_eq!(channel.borrow().num_packets_resent(), 1);

        interface.irregular_clear_for_test();
    }
}
