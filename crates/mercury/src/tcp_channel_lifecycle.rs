//! `TCPChannel` lifetime: inactivity detection, peer-disconnect handling and
//! the destroy cleanup (`tcp_channel.cpp`, `channel.cpp`) (BW-02-04-04).
//!
//! A TCP channel's timeout is the kernel's, not the channel's:
//! `startInactivityDetection` only configures `SO_KEEPALIVE` and the
//! `TCP_KEEPIDLE`/`TCP_KEEPINTVL`/`TCP_KEEPCNT` probes
//! (`tcp_channel.cpp:645-663`), and no user-space inactivity timer is
//! installed. When the probes fail, the kernel closes the socket and the
//! channel's next read reports zero bytes, which is the peer disconnect
//! (`handleInput`, `tcp_channel.cpp:155-166`).
//!
//! Destroying the channel tears down everything it owns at once: the send
//! queue and the outstanding requests waiting on replies are dropped
//! together, so a dead channel reports no unsent data and no pending
//! replies -- the two states always agree.

use crate::tcp_send_buffer::{SendFn, TcpSendBuffer};

/// What a socket read's byte count meant for the channel
/// (`handleInput`, `tcp_channel.cpp:143-172`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The peer sent data; the channel proceeds to read it.
    MoreData,
    /// The count was negative; the reference logs the error and continues.
    ReadError,
    /// The peer closed the connection (or finished a clean shutdown).
    PeerDisconnected(PeerDisconnectKind),
}

/// Why the peer went away (`handlePeerDisconnect`, `tcp_channel.cpp:864-881`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerDisconnectKind {
    /// The channel was shut down cleanly first, so the peer's close is the
    /// expected end of a graceful shutdown.
    CleanShutdown,
    /// The peer disconnected without a graceful shutdown.
    PeerDisconnect,
}

/// The keepalive configuration `startInactivityDetection` asks for on a TCP
/// channel (`tcp_channel.cpp:645-663`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepaliveOptions {
    /// `SO_KEEPALIVE = true`.
    pub keepalive: bool,
    /// `TCP_KEEPIDLE = 1`: idle seconds before the first probe.
    pub keepidle_secs: i32,
    /// `TCP_KEEPINTVL = 1`: seconds between probes.
    pub keepintvl_secs: i32,
    /// `TCP_KEEPCNT = checkPeriod`: probes before the kernel drops it.
    pub keepcnt: i32,
}

/// What `shutDown` decided (`tcp_channel.cpp:606-627`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutDownAction {
    /// The send buffer is empty, so the write half is closed now.
    ShutdownWrite,
    /// Data is still queued; the drain closes it when it empties.
    WaitForDrain,
}

/// TCP's `onChannelInactivityTimeout` (`tcp_channel.cpp:658-665`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactivityTimeoutOutcome {
    /// The reference asserts this is never reached: a TCP channel relies on
    /// the kernel keepalive, so it never installs the user-space timer that
    /// would call this. Reported as a violation, not acted on.
    Unexpected,
}

/// The TCP channel's lifetime: the connection state and the state that is
/// torn down with it.
pub struct TcpChannelLifecycle {
    /// `isDestroyed_`.
    is_destroyed: bool,
    /// `isShuttingDown_`.
    is_shutting_down: bool,
    /// `lastReceivedTime_`, in the caller's timebase.
    last_received_time: u64,
    /// The send queue, dropped when the channel is destroyed.
    send_buffer: TcpSendBuffer,
    /// The outstanding requests awaiting a reply; dropped on destroy.
    pending_replies: usize,
    /// Whether a user-space inactivity timer is installed. Always false for
    /// a TCP channel, which leaves timeout to the kernel keepalive.
    timer_installed: bool,
}

impl TcpChannelLifecycle {
    /// The fresh, open channel.
    pub fn new() -> Self {
        TcpChannelLifecycle {
            is_destroyed: false,
            is_shutting_down: false,
            last_received_time: 0,
            send_buffer: TcpSendBuffer::new(),
            pending_replies: 0,
            timer_installed: false,
        }
    }

    /// `isConnected` (`tcp_channel.hpp:53-55`): usable for sending only when
    /// neither shut down nor destroyed.
    pub fn is_connected(&self) -> bool {
        !self.is_shutting_down && !self.is_destroyed
    }

    /// `isDestroyed`.
    pub fn is_destroyed(&self) -> bool {
        self.is_destroyed
    }

    /// `isShuttingDown_`.
    pub fn is_shutting_down(&self) -> bool {
        self.is_shutting_down
    }

    /// `hasUnsentData`: whether the send queue still holds bytes.
    pub fn has_unsent_data(&self) -> bool {
        self.send_buffer.has_unsent_data()
    }

    /// The outstanding requests awaiting a reply.
    pub fn pending_reply_count(&self) -> usize {
        self.pending_replies
    }

    /// `lastReceivedTime_`.
    pub fn last_received_time(&self) -> u64 {
        self.last_received_time
    }

    /// A request was sent that expects a reply; the count grows.
    pub fn note_request_started(&mut self) {
        self.pending_replies += 1;
    }

    /// A reply arrived for one of them; the count shrinks, floor at zero.
    pub fn note_reply_arrived(&mut self) {
        self.pending_replies = self.pending_replies.saturating_sub(1);
    }

    /// `writeFrom`: hand a frame to the send path (BW-02-04-02).
    pub fn write_from(&mut self, input: &[u8], send: &mut SendFn<'_>) -> bool {
        self.send_buffer.write_from(input, send)
    }

    /// `sendBufferedData`: drain the queue; false only on a hard error.
    pub fn send_buffered_data(&mut self, send: &mut SendFn<'_>) -> bool {
        self.send_buffer.send_buffered_data(send)
    }

    /// Whether the write half should be closed now: a clean shutdown whose
    /// queue just emptied (`sendBufferedData`, `tcp_channel.cpp:817-855`).
    pub fn should_shutdown_write(&self) -> bool {
        self.is_shutting_down && !self.send_buffer.has_unsent_data()
    }

    /// `handleInput`'s byte count (`tcp_channel.cpp:143-172`): zero is the
    /// peer shutting down, negative is a read error (logged, not fatal),
    /// positive is data and updates the last-received time.
    pub fn note_received(&mut self, available: i64, now: u64) -> ReadOutcome {
        if available > 0 {
            self.last_received_time = now;
            return ReadOutcome::MoreData;
        }
        if available < 0 {
            return ReadOutcome::ReadError;
        }
        ReadOutcome::PeerDisconnected(self.handle_peer_disconnect())
    }

    /// `handlePeerDisconnect` (`tcp_channel.cpp:864-881`): name the end and
    /// destroy the channel.
    pub fn handle_peer_disconnect(&mut self) -> PeerDisconnectKind {
        let kind = if self.is_shutting_down {
            PeerDisconnectKind::CleanShutdown
        } else {
            PeerDisconnectKind::PeerDisconnect
        };
        self.destroy();
        kind
    }

    /// `startInactivityDetection` (`tcp_channel.cpp:645-663`): a TCP channel
    /// configures the kernel keepalive and installs no user-space timer.
    pub fn start_inactivity_detection(
        &mut self,
        _period_secs: f32,
        check_period_secs: f32,
    ) -> KeepaliveOptions {
        self.timer_installed = false;
        KeepaliveOptions {
            keepalive: true,
            keepidle_secs: 1,
            keepintvl_secs: 1,
            keepcnt: check_period_secs as i32,
        }
    }

    /// The base `InactivityTimeoutChecker` predicate
    /// (`channel.cpp:53-59`): whether the inactivity period has elapsed
    /// since the last received data.
    pub fn is_inactive(&self, now: u64, inactivity_period: u64) -> bool {
        now.saturating_sub(self.last_received_time) > inactivity_period
    }

    /// TCP's `onChannelInactivityTimeout` (`tcp_channel.cpp:658-665`): the
    /// reference asserts this is never reached. Reported, not acted on.
    pub fn on_channel_inactivity_timeout(&self) -> InactivityTimeoutOutcome {
        InactivityTimeoutOutcome::Unexpected
    }

    /// `shutDown` (`tcp_channel.cpp:606-627`): flip to shutting down, then
    /// close the write half now if the queue is empty, else when it drains.
    pub fn shut_down(&mut self) -> ShutDownAction {
        self.is_shutting_down = true;
        if self.send_buffer.has_unsent_data() {
            ShutDownAction::WaitForDrain
        } else {
            ShutDownAction::ShutdownWrite
        }
    }

    /// `destroy` (`channel.cpp:217-235`): cancel the timer, tear down the
    /// socket-owned state and the queue, and drop the pending replies. The
    /// send queue and the pending replies always leave together.
    pub fn destroy(&mut self) {
        if self.is_destroyed {
            return;
        }
        self.timer_installed = false;
        self.send_buffer = TcpSendBuffer::new();
        self.pending_replies = 0;
        self.is_destroyed = true;
    }
}

impl Default for TcpChannelLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A default channel is open and empty, like a new one.
    #[test]
    fn a_default_channel_is_open_and_empty() {
        let channel = TcpChannelLifecycle::default();
        assert!(channel.is_connected());
        assert!(!channel.is_destroyed());
        assert!(!channel.is_shutting_down());
        assert!(!channel.has_unsent_data());
        assert_eq!(channel.pending_reply_count(), 0);
    }

    /// A seam that always takes every byte (a clear socket).
    fn full_send() -> impl FnMut(&[u8]) -> Result<usize, i32> {
        let emitted = RefCell::new(Vec::new());
        move |data: &[u8]| {
            emitted.borrow_mut().extend_from_slice(data);
            Ok(data.len())
        }
    }

    /// A seam that takes nothing (the socket is closed to writes).
    fn blocked_send() -> impl FnMut(&[u8]) -> Result<usize, i32> {
        |_data: &[u8]| Ok(0)
    }

    /// The acceptance: a peer disconnect destroys the channel and the send
    /// queue and the pending replies leave together -- the two states agree.
    #[test]
    fn a_peer_disconnect_destroys_and_clears_queue_and_replies() {
        let mut channel = TcpChannelLifecycle::new();
        channel.note_request_started();
        channel.note_request_started();

        // Queue a frame that the socket will not take yet.
        let mut send = blocked_send();
        assert!(channel.write_from(&[1, 2, 3], &mut send));
        assert!(channel.has_unsent_data());
        assert_eq!(channel.pending_reply_count(), 2);

        let outcome = channel.note_received(0, 100);
        assert_eq!(
            outcome,
            ReadOutcome::PeerDisconnected(PeerDisconnectKind::PeerDisconnect)
        );

        assert!(!channel.is_connected());
        assert!(!channel.has_unsent_data(), "the queue is dropped");
        assert_eq!(channel.pending_reply_count(), 0, "the replies are dropped");
    }

    /// The same consistency after a clean shutdown: a graceful end also
    /// clears everything at once.
    #[test]
    fn a_clean_shutdown_clears_queue_and_replies_too() {
        let mut channel = TcpChannelLifecycle::new();
        channel.note_request_started();
        let mut send = blocked_send();
        assert!(channel.write_from(&[1, 2, 3], &mut send));

        assert_eq!(channel.shut_down(), ShutDownAction::WaitForDrain);
        assert!(channel.is_shutting_down());
        assert!(!channel.is_connected());

        let outcome = channel.note_received(0, 200);
        assert_eq!(
            outcome,
            ReadOutcome::PeerDisconnected(PeerDisconnectKind::CleanShutdown)
        );
        assert!(!channel.has_unsent_data());
        assert_eq!(channel.pending_reply_count(), 0);
    }

    /// A timeout is the kernel's work for a TCP channel: the user-space
    /// timer that would declare inactivity is never installed, and the
    /// channel reports that it should not fire.
    #[test]
    fn a_tcp_channel_defers_timeout_to_the_kernel_keepalive() {
        let mut channel = TcpChannelLifecycle::new();
        let options = channel.start_inactivity_detection(30.0, 3.0);
        assert_eq!(
            options,
            KeepaliveOptions {
                keepalive: true,
                keepidle_secs: 1,
                keepintvl_secs: 1,
                keepcnt: 3,
            }
        );
        assert_eq!(
            channel.on_channel_inactivity_timeout(),
            InactivityTimeoutOutcome::Unexpected,
            "the reference asserts this is never reached"
        );
    }

    /// The inactivity predicate: no data for longer than the period, using
    /// the checker's `timestamp() - lastReceived > period` shape.
    #[test]
    fn the_inactivity_predicate_matches_the_checker() {
        let mut channel = TcpChannelLifecycle::new();
        assert!(!channel.is_inactive(0, 5), "nothing elapsed yet");

        channel.note_received(10, 100);
        assert!(!channel.is_inactive(104, 5));
        assert!(channel.is_inactive(106, 5), "one past the period");
        assert!(channel.is_inactive(1000, 5));
    }

    /// The byte-count branches: positive is data, zero is a peer
    /// disconnect, negative is a read error the channel survives.
    #[test]
    fn the_byte_count_branches_are_distinct() {
        let mut channel = TcpChannelLifecycle::new();
        assert_eq!(channel.note_received(5, 10), ReadOutcome::MoreData);
        assert_eq!(channel.last_received_time(), 10);
        assert!(channel.is_connected());

        assert_eq!(channel.note_received(-1, 11), ReadOutcome::ReadError);
        assert!(!channel.is_destroyed(), "a read error is not a disconnect");
        assert_eq!(channel.last_received_time(), 10, "no data arrived");

        assert_eq!(
            channel.note_received(0, 12),
            ReadOutcome::PeerDisconnected(PeerDisconnectKind::PeerDisconnect)
        );
        assert!(channel.is_destroyed());
    }

    /// A shutdown with nothing queued closes the write half right away.
    #[test]
    fn a_shutdown_with_an_empty_buffer_closes_the_write_half_now() {
        let mut channel = TcpChannelLifecycle::new();
        assert_eq!(channel.shut_down(), ShutDownAction::ShutdownWrite);
        assert!(channel.should_shutdown_write());
        assert!(!channel.is_connected());
    }

    /// A shutdown with data queued waits, and the drain closes the write
    /// half once the queue empties.
    #[test]
    fn a_shutdown_waits_for_the_drain_before_closing_the_write_half() {
        let mut channel = TcpChannelLifecycle::new();
        let mut send = blocked_send();
        assert!(channel.write_from(&[1, 2, 3], &mut send));
        assert_eq!(channel.shut_down(), ShutDownAction::WaitForDrain);
        assert!(!channel.should_shutdown_write(), "still queued");

        let mut send = full_send();
        assert!(channel.send_buffered_data(&mut send));
        assert!(channel.should_shutdown_write(), "emptied, close now");
    }

    /// Destroy is idempotent and the queue/replies stay cleared.
    #[test]
    fn destroy_is_idempotent_and_stays_cleared() {
        let mut channel = TcpChannelLifecycle::new();
        channel.note_request_started();
        let mut send = blocked_send();
        assert!(channel.write_from(&[1, 2, 3], &mut send));

        channel.destroy();
        channel.destroy();
        assert!(channel.is_destroyed());
        assert!(!channel.has_unsent_data());
        assert_eq!(channel.pending_reply_count(), 0);
    }

    /// Pending replies grow and shrink as requests and replies move.
    #[test]
    fn pending_replies_follow_requests_and_replies() {
        let mut channel = TcpChannelLifecycle::new();
        channel.note_request_started();
        channel.note_request_started();
        channel.note_request_started();
        assert_eq!(channel.pending_reply_count(), 3);
        channel.note_reply_arrived();
        assert_eq!(channel.pending_reply_count(), 2);
        channel.note_reply_arrived();
        channel.note_reply_arrived();
        assert_eq!(channel.pending_reply_count(), 0);
        channel.note_reply_arrived();
        assert_eq!(channel.pending_reply_count(), 0, "floors at zero");
    }
}
