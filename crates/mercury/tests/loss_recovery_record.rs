//! The loss-recovery record (OPT-02-01-01).
//!
//! The wall-clock faces in `benches/network.rs` carry the host's
//! scheduler noise; loss recovery does not have to. Its timings run off
//! the channel's RTT average and the resend period
//! `max(2 * roundTripTime_, minInactivityResendDelay)`
//! (`udp_channel.cpp:1113`), both simulated-time machinery, so this
//! record pins the exact stamps on a pinned clock:
//!
//! - the strict staleness inequality (an unacked packet goes back out
//!   when its age is strictly past the period, not at it),
//! - the resend period's two arms (the RTT arm vs the minimum-delay
//!   arm),
//! - a resent packet's ack never samples the RTT average
//!   (udp_channel.cpp:987),
//! - the per-scan throttle `windowSize / 8`,
//! - and the window stats the recovery leaves behind.

use std::cell::Cell;
use std::rc::Rc;

use mercury::endpoint::Address;
use mercury::event_dispatcher::EventDispatcher;
use mercury::misc::CHANNEL_ID_NULL;
use mercury::network_interface::{InterfaceType, NetworkInterface};
use mercury::packet::Packet;
use mercury::udp_channel::{DEFAULT_INACTIVITY_RESEND_DELAY, Traits, UdpChannel};

/// An interface on a pinned clock with no socket: the channel machinery
/// needs only the dispatcher and the maps.
fn interface(clock: &Rc<Cell<u64>>) -> Rc<NetworkInterface> {
    let clock_cell = Rc::clone(clock);
    let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
        clock_cell.get()
    })));
    NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher)
}

fn addr(ip: u32) -> Address {
    Address::new(ip, 20)
}

/// The reference send path's window leg: one seq, one packet snapshot
/// into the window, exactly the way `udp_channel.rs`'s own tests drive
/// it (no reliable orders, no bundle).
fn send_one(channel: &mercury::udp_channel::ChannelPtr) -> bool {
    let seq = UdpChannel::use_next_sequence_id(channel);
    let mut packet = Packet::new();
    packet.set_seq(seq);
    UdpChannel::add_resend_timer(channel, seq, Rc::new(packet), &[])
}

/// Walk `checkResendTimers` one stamp at a time and report the first
/// stamp where the given seq goes back out, bounded by a sane walk
/// length.
fn first_resend_stamp(
    clock: &Rc<Cell<u64>>,
    channel: &mercury::udp_channel::ChannelPtr,
    from: u64,
    to: u64,
) -> Option<u64> {
    let mut stamp = from;
    while stamp <= to {
        clock.set(stamp);
        UdpChannel::check_resend_timers(channel);
        if channel.borrow().num_packets_resent() > 0 {
            return Some(stamp);
        }
        stamp += 1;
    }
    None
}

/// The minimum-delay arm: an internal channel (RTT average starts at
/// 0.1s, so 2*RTT = 0.2s) whose `minInactivityResendDelay` is 2s. The
/// period is the delay's 2s, and the resend lands strictly past it --
/// an age exactly equal to the period is not yet stale.
#[test]
fn the_resend_waits_out_the_minimum_delay_arm() {
    let clock = Rc::new(Cell::new(1_000_000u64));
    let interface = interface(&clock);
    let channel = UdpChannel::new(
        &interface,
        addr(0x0100_00a1),
        Traits::Internal,
        2.0,
        CHANNEL_ID_NULL,
    );
    UdpChannel::set_is_remote_regular(&channel, false);

    let sent_at = clock.get();
    assert!(send_one(&channel));
    assert!(channel.borrow().has_unacked_packets());

    // period = max(2*0.1s, 2s) = 2s. An age exactly at the period is not
    // stale; one microsecond past it is.
    assert_eq!(
        first_resend_stamp(&clock, &channel, sent_at + 1_999_999, sent_at + 2_000_002),
        Some(sent_at + 2_000_001),
        "strictly past max(2*RTT, minDelay), not at it"
    );
    {
        let this = channel.borrow();
        assert_eq!(this.num_packets_resent(), 1, "one packet, one resend");
        assert_eq!(
            this.last_reliable_resend_time(),
            sent_at + 2_000_001,
            "the resend stamp is recorded"
        );
        assert_eq!(
            this.round_trip_time_in_seconds(),
            0.1,
            "no ack has sampled the average yet"
        );
    }

    // The resent packet's ack (0.4s after the resend) must NOT fold into
    // the average: its round trip was not a round trip.
    clock.set(sent_at + 2_400_001);
    assert!(UdpChannel::handle_ack(&channel, 0));
    {
        let this = channel.borrow();
        assert_eq!(
            this.round_trip_time_in_seconds(),
            0.1,
            "a resent packet's ack does not sample"
        );
        assert!(
            !this.has_unacked_packets(),
            "the ack cleared the window: recovery complete"
        );
    }
    // Recovery window: send -> resend detection -> ack, in simulated
    // stamps.
    assert_eq!(
        clock.get() - sent_at,
        2_400_001,
        "2s detection + 0.4s ack return"
    );
}

/// The RTT arm: a regular-ish peer whose acks come back at 0.3s, driving
/// the average off its 0.1s seed (0.12, then 0.138), so 2*RTT = 0.276s
/// outruns the 50ms minimum delay and sets the period.
#[test]
fn the_resend_period_follows_the_rtt_arm() {
    let clock = Rc::new(Cell::new(1_000_000u64));
    let interface = interface(&clock);
    let channel = UdpChannel::new(
        &interface,
        addr(0x0100_00a2),
        Traits::Internal,
        0.05,
        CHANNEL_ID_NULL,
    );
    UdpChannel::set_is_remote_regular(&channel, false);

    let sent_at = clock.get();
    assert!(send_one(&channel)); // seq 0
    assert!(send_one(&channel)); // seq 1
    assert!(send_one(&channel)); // seq 2: the one whose ack never comes
    assert_eq!(channel.borrow().small_out_seq_at(), 3);

    // Two acks at 0.3s fold into the average: (0.1*9+0.3)/10 = 0.12,
    // then (0.12*9+0.3)/10 = 0.138. The lost packet's ack never comes;
    // the two acked packets leave the window.
    clock.set(sent_at + 300_000);
    assert!(UdpChannel::handle_ack(&channel, 0));
    assert!(UdpChannel::handle_ack(&channel, 1));
    {
        let this = channel.borrow();
        assert_eq!(
            this.round_trip_time_in_seconds(),
            0.138,
            "(0.1*9+0.3)/10 twice"
        );
        assert_eq!(this.oldest_unacked_seq(), 2);
    }

    // period = max(2*0.138s, 0.05s) = 0.276s, counted from each packet's
    // own last-sent stamp. The lost packet resent strictly past it.
    assert_eq!(
        first_resend_stamp(&clock, &channel, sent_at + 276_000, sent_at + 276_005),
        Some(sent_at + 276_001),
        "the RTT arm sets the period"
    );
    {
        let this = channel.borrow();
        assert_eq!(this.num_packets_resent(), 1);
        assert_eq!(
            this.num_reliable_packets_sent(),
            0,
            "this window leg is below the bundle send path, and a resend never re-counts"
        );
    }

    // The resent packet's ack leaves the average where the healthy
    // traffic put it.
    clock.set(sent_at + 276_001 + 400_000);
    assert!(UdpChannel::handle_ack(&channel, 2));
    {
        let this = channel.borrow();
        assert_eq!(
            this.round_trip_time_in_seconds(),
            0.138,
            "the resend's ack did not sample"
        );
        assert!(!this.has_unacked_packets());
    }
    assert_eq!(
        clock.get() - sent_at,
        676_001,
        "0.276s detection + 0.4s ack return"
    );
}

/// The per-scan throttle: `windowSize / 8` resends at most in one
/// `checkResendTimers` pass (`udp_channel.cpp:1113`). An internal
/// channel's window is 4096, so one scan of 600 stale packets sends 512
/// back out and defers the rest to the next scan.
#[test]
fn one_scan_resends_at_most_window_over_eight() {
    let clock = Rc::new(Cell::new(1_000_000u64));
    let interface = interface(&clock);
    let channel = UdpChannel::new(
        &interface,
        addr(0x0100_00a3),
        Traits::Internal,
        DEFAULT_INACTIVITY_RESEND_DELAY,
        CHANNEL_ID_NULL,
    );
    UdpChannel::set_is_remote_regular(&channel, false);

    let window = channel.borrow().window_size();
    assert_eq!(window, 4096, "the internal window, so the cap is 512");
    let cap = window / 8;
    let resents = || channel.borrow().num_packets_resent() as usize;

    let sent_at = clock.get();
    for _ in 0..600 {
        assert!(send_one(&channel));
    }

    // Everything is long stale by this stamp; the first scan sends
    // exactly the cap, the next scan picks up the remainder.
    clock.set(sent_at + DEFAULT_INACTIVITY_RESEND_DELAY as u64 * 1_000_000 + 1);
    UdpChannel::check_resend_timers(&channel);
    assert_eq!(resents(), cap, "one scan sends windowSize/8");
    UdpChannel::check_resend_timers(&channel);
    assert_eq!(
        resents(),
        cap + (600 - cap),
        "the next scan takes the remainder"
    );
}
