//! `TCPChannel`'s connection lifecycle (`lib/network/tcp_channel.cpp`):
//! the client/server handshake header, the shutdown handshake, the
//! peer-disconnect and the timeout cleanup. The framing is
//! [`TcpFrameParser`] and the send machinery is [`TcpSendBuffer`]; this is
//! the channel that holds them for one connection and that fails the
//! connection's pending requests when it dies.
//!
//! The reference's `handleInput` (tcp_channel.cpp:149) reads whatever the
//! socket has; a zero-length read is the peer going away
//! (`handlePeerDisconnect`, :164-168), anything else is framed. The first
//! frame a server accepts must be `CLIENT_HEADER` (`readHeader`, :268-315)
//! -- a mismatch destroys the connection, which is the handshake's gate.
//! A client sends that same header as its first frame (`doSend`, :420-427).
//!
//! The shutdown (`shutDown` :596 / `doShutDown` :620) halves the
//! connection in the write direction, deferring to `sendBufferedData`
//! (:850-854) while data is still queued -- the [`TcpSendBuffer`] reports
//! `shutdown_now` when the drain empties. The timeout cleanup
//! (`startInactivityDetection` :641) is the kernel keepalive the reference
//! installs instead of a timer; `onChannelInactivityTimeout` (:658) is a
//! `CRITICAL_MSG` the reference never expects to fire, because the kernel
//! surfaces the dead peer as a read the channel sees as a disconnect.
//!
//! Destroying (`Channel::destroy`, channel.cpp:217) is the whole cleanup:
//! the send buffer goes with `doDestroy` (tcp_channel.cpp:543), the channel
//! is marked destroyed, and `onChannelGone` fails every request still
//! riding it (`REASON_CHANNEL_LOST`) -- which is the pending-reply half of
//! this item's acceptance.
//!
//! The port has no socket, so the `SHUT_WR` and the keepalive socket
//! options are recorded rather than issued; the socket read is a seam the
//! caller drives through [`on_read`](TcpChannel::on_read).

use std::cell::Cell;
use std::rc::Rc;
use std::rc::Weak;

use crate::endpoint::Address;
use crate::misc::Reason;
use crate::network_interface::NetworkInterface;
use crate::tcp_bundle_dispatch;
use crate::tcp_frame::TcpFrameParser;
use crate::tcp_send::{FlushResult, TcpSendBuffer};

/// `CLIENT_HEADER` (tcp_channel.cpp:29): the first frame a client sends,
/// and the only frame a server accepts before any real bundle.
pub const CLIENT_HEADER: &[u8] = b"TCP\r\n\r\n";

/// `CLIENT_HEADER_SIZE` (tcp_channel.cpp:30): the payload width.
pub const CLIENT_HEADER_SIZE: usize = CLIENT_HEADER.len();

/// The width of a small frame's length prefix (`sizeof( uint16 )`), the
/// header frame's `frameHeaderLength_` in `readHeader`.
const SMALL_FRAME_HEADER_SIZE: usize = 2;

/// `HTTP_HEADER_START` (tcp_channel.cpp:202): the beginning of the HTTP
/// handshake line a WebSockets client would send instead of the header
/// frame. A server expecting raw TCP drops the connection on it.
const HTTP_HEADER_START: &[u8] = b"GET ";

/// `headerState_` (tcp_channel.hpp:123).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderState {
    /// No handshake in progress: the connection is fully established.
    None,
    /// A server awaiting the client's header frame.
    ServerWaitForHeader,
    /// A client that still owes its header frame on its first send.
    ClientSendHeader,
}

/// The outcome of one socket read, as `handleInput` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The peer went away: a zero-length read, or an error the channel
    /// cannot continue past. The channel calls `handlePeerDisconnect`.
    Disconnected,
    /// Data to be framed.
    Data,
}

/// The handshake verdict of one first frame on the server side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderVerdict {
    /// The frame was the `CLIENT_HEADER`; the connection is established.
    /// The frame is consumed.
    Accepted,
    /// The frame was not the header; the connection is unusable and the
    /// reference destroys it.
    Rejected,
}

/// The connection lifecycle of one TCP channel.
pub struct TcpChannel {
    /// `headerState_`.
    header_state: HeaderState,
    /// `isShuttingDown_`.
    is_shutting_down: bool,
    /// `isDestroyed_`.
    is_destroyed: bool,
    /// `pSendBuffer_`: present only while data is queued behind a busy
    /// socket. `doDestroy` deletes it; dropping the channel does too.
    send_buffer: Option<TcpSendBuffer>,
    /// The receive framing state (`pFrameData_` + `frameLength_` +
    /// `frameHeaderLength_`).
    parser: TcpFrameParser,
    /// `pNetworkInterface_`, weak so a channel cannot keep its interface
    /// alive, and the `onChannelGone` target of `destroy`.
    interface: Weak<NetworkInterface>,
    /// `pEndpoint_->getRemoteAddress()`, the key the pending requests ride
    /// on and that `onChannelGone` cancels by.
    remote: Address,
    /// Whether a stream filter still owes its own shutdown (the
    /// `pStreamFilter_->didFinishShuttingDown()` gate of `shutDown`).
    filter_owes_shutdown: bool,
    /// The keepalive count `startInactivityDetection` was given
    /// (`TCP_KEEPCNT`), recorded since the port has no socket to set
    /// `SO_KEEPALIVE`/`TCP_KEEPIDLE`/`TCP_KEEPINTVL` on.
    keepalive_check_period: Option<f32>,
    /// `shutDown` has half-closed the write direction (`SHUT_WR`), issued
    /// by `doShutDown`. Recorded, not sent.
    shut_down_sent: bool,
    /// `onChannelInactivityTimeout` was called -- the reference's
    /// `CRITICAL_MSG`, which the port records instead of aborting.
    inactivity_timed_out: Cell<bool>,
    /// The server-side bytes held for the WebSockets sniff
    /// (tcp_channel.cpp:194-223): the sniff must see a first frame's head
    /// before the parser frames it, so until it resolves, the raw bytes
    /// sit here rather than in the parser.
    pending_header: Vec<u8>,
}

impl TcpChannel {
    /// `TCPChannel( networkInterface, endpoint, isServer )`: the header
    /// state follows the side, nothing is buffered, nothing is destroyed.
    pub fn new(interface: &Rc<NetworkInterface>, remote: Address, is_server: bool) -> Self {
        Self {
            header_state: if is_server {
                HeaderState::ServerWaitForHeader
            } else {
                HeaderState::ClientSendHeader
            },
            is_shutting_down: false,
            is_destroyed: false,
            send_buffer: None,
            parser: TcpFrameParser::new(),
            interface: Rc::downgrade(interface),
            remote,
            filter_owes_shutdown: false,
            keepalive_check_period: None,
            shut_down_sent: false,
            inactivity_timed_out: Cell::new(false),
            pending_header: Vec::new(),
        }
    }

    // -- handshake --------------------------------------------------------

    /// `doSend`'s client arm (tcp_channel.cpp:420-427): the header frame a
    /// client owes -- a small frame carrying `CLIENT_HEADER`. The header is
    /// sent once; afterwards the state is `None`.
    pub fn client_header_frame(&mut self) -> Vec<u8> {
        debug_assert_eq!(self.header_state, HeaderState::ClientSendHeader);
        let mut frame = Vec::with_capacity(SMALL_FRAME_HEADER_SIZE + CLIENT_HEADER_SIZE);
        frame.extend_from_slice(&(CLIENT_HEADER_SIZE as u16).to_le_bytes());
        frame.extend_from_slice(CLIENT_HEADER);
        self.header_state = HeaderState::None;
        frame
    }

    /// `readHeader` (tcp_channel.cpp:268-315): the server's verdict on its
    /// first frame. The frame must be exactly the header; the parser has
    /// already counted it, so the channel only consumes it and flips the
    /// state, exactly as `readHeader` does after the `memcmp`.
    pub fn read_header(&mut self, frame: &[u8]) -> HeaderVerdict {
        debug_assert_eq!(self.header_state, HeaderState::ServerWaitForHeader);
        if frame != CLIENT_HEADER {
            return HeaderVerdict::Rejected;
        }
        self.header_state = HeaderState::None;
        HeaderVerdict::Accepted
    }

    // -- shutdown ---------------------------------------------------------

    /// `shutDown` (tcp_channel.cpp:596): half-close the write direction. A
    /// stream filter that still owes its own shutdown holds the channel
    /// back; otherwise the shutdown proceeds now, or when the pending send
    /// buffer empties.
    pub fn shut_down(&mut self) {
        if self.filter_owes_shutdown {
            return;
        }
        self.is_shutting_down = true;
        if self.send_buffer.is_none() {
            self.do_shut_down();
        }
    }

    /// `doShutDown` (tcp_channel.cpp:620): `SHUT_WR` -- half the
    /// connection. Recorded; the peer's subsequent zero-length read is the
    /// other half, handled as a disconnect.
    pub fn do_shut_down(&mut self) {
        self.shut_down_sent = true;
    }

    /// Whether a stream filter still owes its own shutdown, settable by a
    /// filter (the `didFinishShuttingDown` gate).
    pub fn set_filter_owes_shutdown(&mut self, owes: bool) {
        self.filter_owes_shutdown = owes;
    }

    /// `sendBufferedData`'s tail (tcp_channel.cpp:850-854): drive the
    /// pending send buffer. When the drain empties, the buffer is dropped
    /// (the write registration is gone) and a shutdown that was waiting on
    /// it proceeds.
    pub fn flush_send_buffer(&mut self) -> Option<FlushResult> {
        let buffer = self.send_buffer.as_mut()?;
        let result = buffer.flush();
        if matches!(result, FlushResult::Drained { .. }) {
            self.send_buffer = None;
            if self.is_shutting_down {
                self.do_shut_down();
            }
        }
        Some(result)
    }

    // -- disconnect and cleanup ------------------------------------------

    /// `handleInput`'s read (tcp_channel.cpp:149-168): a zero-length read
    /// is the peer disconnecting; the data path is the framing loop, which
    /// the transport wiring (04-05) owns. The socket read is a seam, so
    /// the caller hands the outcome in.
    pub fn on_read(&mut self, read: ReadOutcome) {
        if let ReadOutcome::Disconnected = read {
            self.handle_peer_disconnect();
        }
    }

    /// `handleInput`'s data arm (tcp_channel.cpp:190-263): a server's
    /// first bytes go through the WebSockets sniff first, then whatever
    /// bytes resolve it are framed and dispatched.
    pub fn on_data(&mut self, bytes: &[u8]) {
        if self.header_state == HeaderState::ServerWaitForHeader {
            self.pending_header.extend_from_slice(bytes);
            if self.pending_header.len() < HTTP_HEADER_START.len() {
                // `pFrameData_->remainingLength() < HTTP_HEADER_START_SIZE`:
                // wait for more data.
                return;
            }
            if &self.pending_header[..HTTP_HEADER_START.len()] == HTTP_HEADER_START {
                // It definitely doesn't match the first header frame, so we
                // can assume it was a WebSockets frame and drop the
                // connection here (tcp_channel.cpp:215-223).
                self.destroy();
                return;
            }
            // The sniff is passed; the held bytes are a header frame (or a
            // header frame plus real bundles), so they frame and dispatch.
            let wire = std::mem::take(&mut self.pending_header);
            self.feed_and_dispatch(&wire);
        } else {
            self.feed_and_dispatch(bytes);
        }
    }

    /// The framing loop of `handleInput` (tcp_channel.cpp:247-260): frame
    /// whatever bytes are in hand, then hand each complete frame to the
    /// header gate or the bundle dispatch.
    fn feed_and_dispatch(&mut self, bytes: &[u8]) {
        let frames = self.parser.feed(bytes);
        for frame in frames {
            if self.header_state == HeaderState::ServerWaitForHeader {
                // The first frame is the header frame. `readHeader` destroys
                // the channel on a mismatch, so a rejected frame stops the
                // loop.
                if self.read_header(&frame) == HeaderVerdict::Rejected {
                    self.destroy();
                    return;
                }
            } else {
                self.process_received_frame(&frame);
            }
        }
    }

    /// `processReceivedFrame` (tcp_channel.cpp:360-378): a complete frame
    /// is one bundle; the interface's message table dispatches it. Nothing
    /// is wired yet when the interface has no table, in which case there
    /// is nothing served and nothing to do.
    pub fn process_received_frame(&mut self, frame: &[u8]) {
        let Some(interface) = self.interface.upgrade() else {
            return;
        };
        let Some(table) = interface.interface_table() else {
            return;
        };
        let mut frame = frame.to_vec();
        let reason = tcp_bundle_dispatch::dispatch_messages(&table, &self.remote, &mut frame);
        if reason != Reason::Success {
            eprintln!(
                "TCPChannel( {} )::processReceivedFrame: Error while dispatching messages: {}",
                self.remote.c_str(),
                reason.to_string()
            );
        }
    }

    /// `handleInput`'s zero-length read (tcp_channel.cpp:164-168): the
    /// peer went away.
    pub fn handle_peer_disconnect(&mut self) {
        self.destroy();
    }

    /// `Channel::destroy` (channel.cpp:217): cancel the inactivity timer,
    /// tear down the socket half (`doDestroy`), mark destroyed, then tell
    /// the interface -- `onChannelGone` fails every request still riding
    /// this channel, which is the cleanup this item accepts.
    pub fn destroy(&mut self) {
        if self.is_destroyed {
            return;
        }
        // `doDestroy` (tcp_channel.cpp:543): deregister the fds, delete
        // the send buffer, close the endpoint. The buffer is the only
        // state here; it drops with the assignment.
        self.send_buffer = None;
        self.is_destroyed = true;
        if let Some(interface) = self.interface.upgrade() {
            interface.on_channel_gone(&self.remote);
        }
    }

    /// `handlePeerDisconnect`'s bookkeeping, minus the trace: the channel
    /// was destroyed by `handle_peer_disconnect`. Kept for the two traces
    /// the reference prints ("Shutdown cleanly" vs "Disconnected by
    /// peer"), so a test can tell the arms apart.
    pub fn was_shutting_down(&self) -> bool {
        self.is_shutting_down
    }

    // -- timeout ----------------------------------------------------------

    /// `startInactivityDetection` (tcp_channel.cpp:641): the kernel
    /// keepalive the reference installs instead of an inactivity timer --
    /// `SO_KEEPALIVE`, `TCP_KEEPIDLE=1`, `TCP_KEEPINTVL=1`,
    /// `TCP_KEEPCNT=checkPeriod`. Recorded, since the port has no socket.
    pub fn start_inactivity_detection(&mut self, check_period: f32) {
        self.keepalive_check_period = Some(check_period);
    }

    /// `onChannelInactivityTimeout` (tcp_channel.cpp:658): the reference's
    /// `CRITICAL_MSG` -- the kernel, not this, detects the dead peer, so
    /// the channel must never be called. Recorded instead of aborting.
    pub fn on_channel_inactivity_timeout(&self) {
        self.inactivity_timed_out.set(true);
    }

    // -- state ------------------------------------------------------------

    /// `headerState_`.
    pub fn header_state(&self) -> HeaderState {
        self.header_state
    }

    /// `isShuttingDown_`.
    pub fn is_shutting_down(&self) -> bool {
        self.is_shutting_down
    }

    /// `isDestroyed_`.
    pub fn is_destroyed(&self) -> bool {
        self.is_destroyed
    }

    /// `shutDown` has reached `doShutDown` (`SHUT_WR` sent).
    pub fn shut_down_sent(&self) -> bool {
        self.shut_down_sent
    }

    /// The keepalive count `startInactivityDetection` was given.
    pub fn keepalive_check_period(&self) -> Option<f32> {
        self.keepalive_check_period
    }

    /// `onChannelInactivityTimeout` was called.
    pub fn inactivity_timed_out(&self) -> bool {
        self.inactivity_timed_out.get()
    }

    /// The receive framing state, mutable, for the `handleInput` framing
    /// loop -- the 04-05 wiring consumes complete frames through it.
    pub fn parser_mut(&mut self) -> &mut TcpFrameParser {
        &mut self.parser
    }

    /// `pSendBuffer_`: the queued bytes, if any.
    pub fn send_buffer(&self) -> Option<&TcpSendBuffer> {
        self.send_buffer.as_ref()
    }

    /// Install the send buffer the socket seam was built over.
    pub fn set_send_buffer(&mut self, buffer: TcpSendBuffer) {
        self.send_buffer = Some(buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tcp_send::SendAttempt;
    use crate::tcp_send::TcpSender;

    /// A sender that blocks the first write and takes everything after,
    /// so a channel can be given a non-empty send buffer, and so the
    /// buffer drains once the socket catches up.
    struct BlockThenSend {
        blocked: bool,
    }

    impl BlockThenSend {
        fn new() -> Self {
            Self { blocked: true }
        }
    }

    impl TcpSender for BlockThenSend {
        fn send(&mut self, data: &[u8]) -> SendAttempt {
            if self.blocked {
                self.blocked = false;
                SendAttempt::WouldBlock
            } else {
                SendAttempt::Sent(data.len())
            }
        }
    }

    /// A socketless interface, for the `onChannelGone` coupling.
    fn interface() -> Rc<NetworkInterface> {
        use crate::event_dispatcher::EventDispatcher;
        let clock = Rc::new(Cell::new(1_000_000u64));
        let cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || cell.get())));
        NetworkInterface::new_with_dispatcher(
            crate::network_interface::InterfaceType::Internal,
            dispatcher,
        )
    }

    /// The header frame a client sends and a server accepts.
    #[test]
    fn the_client_header_frame_is_a_small_frame_carrying_the_header() {
        let channel = &mut TcpChannel::new(&interface(), Address::new(1, 2), false);
        let frame = channel.client_header_frame();
        assert_eq!(frame, b"\x07\x00TCP\r\n\r\n", "len prefix then header");
        assert_eq!(channel.header_state(), HeaderState::None);
    }

    /// The server accepts exactly the header and rejects anything else.
    #[test]
    fn the_server_accepts_the_header_and_rejects_others() {
        let mut server = TcpChannel::new(&interface(), Address::new(1, 2), true);
        assert_eq!(server.header_state(), HeaderState::ServerWaitForHeader);
        assert_eq!(server.read_header(CLIENT_HEADER), HeaderVerdict::Accepted);
        assert_eq!(server.header_state(), HeaderState::None);

        let mut other = TcpChannel::new(&interface(), Address::new(1, 2), true);
        assert_eq!(other.read_header(b"GET /"), HeaderVerdict::Rejected);
        assert_eq!(
            other.header_state(),
            HeaderState::ServerWaitForHeader,
            "the state only moves on acceptance"
        );
    }

    /// End to end: the frame the client sends is the one the server
    /// accepts, through the channel's own receive state -- the parser
    /// hands the gate the frame `readHeader` reads off it.
    #[test]
    fn the_client_header_frame_passes_the_server_gate() {
        let mut client = TcpChannel::new(&interface(), Address::new(1, 2), false);
        let frame = client.client_header_frame();

        let mut server = TcpChannel::new(&interface(), Address::new(3, 4), true);
        let frames = server.parser_mut().feed(&frame);
        assert_eq!(
            frames,
            vec![CLIENT_HEADER.to_vec()],
            "one frame, and it is the header"
        );
        assert_eq!(server.read_header(&frames[0]), HeaderVerdict::Accepted);
        assert_eq!(
            server.header_state(),
            HeaderState::None,
            "the connection is established"
        );
    }

    /// A shutdown with nothing pending half-closes immediately.
    #[test]
    fn a_shutdown_with_nothing_pending_half_closes_now() {
        let mut channel = TcpChannel::new(&interface(), Address::new(1, 2), false);
        channel.shut_down();
        assert!(channel.is_shutting_down());
        assert!(channel.shut_down_sent(), "the SHUT_WR went out");
    }

    /// A shutdown while a send buffer is queued waits for the drain. The
    /// reference's gate is the channel's own `isShuttingDown_`, not the
    /// buffer's: `sendBufferedData` deletes the emptied buffer and, `IF(
    /// isShuttingDown_ )`, calls `doShutDown` -- tcp_channel.cpp:842-854.
    #[test]
    fn a_shutdown_waits_for_the_pending_send_buffer() {
        let mut channel = TcpChannel::new(&interface(), Address::new(1, 2), false);
        let mut buffer = TcpSendBuffer::new(Box::new(BlockThenSend::new()));
        // A busy socket queues the frame, so the buffer is non-empty.
        buffer.write(b"queued frame");
        channel.set_send_buffer(buffer);

        channel.shut_down();
        assert!(channel.is_shutting_down());
        assert!(!channel.shut_down_sent(), "the drain still owns the data");

        // The socket catches up: the drain empties, the buffer is deleted,
        // and the shutdown that was waiting on it proceeds.
        assert!(
            matches!(
                channel.flush_send_buffer().expect("a buffer was queued"),
                FlushResult::Drained { .. }
            ),
            "the queued frame went out"
        );
        assert!(channel.shut_down_sent(), "the deferred doShutDown ran");
        assert!(channel.send_buffer().is_none(), "the drain deleted it");
    }

    /// A stream filter that still owes its shutdown holds the channel
    /// back, exactly as `shutDown` returns early.
    #[test]
    fn a_stream_filter_owing_shutdown_holds_the_channel() {
        let mut channel = TcpChannel::new(&interface(), Address::new(1, 2), false);
        channel.set_filter_owes_shutdown(true);
        channel.shut_down();
        assert!(!channel.is_shutting_down(), "the gate held it");
        assert!(!channel.shut_down_sent());

        channel.set_filter_owes_shutdown(false);
        channel.shut_down();
        assert!(channel.is_shutting_down());
        assert!(channel.shut_down_sent());
    }

    /// A peer disconnect destroys the channel: the send buffer is gone,
    /// the channel is marked destroyed, and the interface is told so the
    /// pending requests on it fail.
    #[test]
    fn a_peer_disconnect_tears_down_the_channel() {
        let interface = interface();
        let remote = Address::new(1, 2);
        let mut channel = TcpChannel::new(&interface, remote, true);
        let mut buffer = TcpSendBuffer::new(Box::new(BlockThenSend::new()));
        buffer.write(b"in flight");
        channel.set_send_buffer(buffer);

        assert!(!channel.is_destroyed());
        channel.handle_peer_disconnect();
        assert!(channel.is_destroyed());
        assert!(
            channel.send_buffer().is_none(),
            "the in-flight data is gone"
        );
        assert!(
            !channel.was_shutting_down(),
            "the reference traces 'Disconnected by peer'"
        );
    }

    /// `on_read`'s arms: a zero-length read is the peer going away and
    /// destroys the channel, plain data is framed -- the framing loop's
    /// job, not a fatal one -- and leaves the channel alone.
    #[test]
    fn a_zero_length_read_disconnects_and_data_does_not() {
        let interface = interface();
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), false);
        channel.on_read(ReadOutcome::Data);
        assert!(!channel.is_destroyed(), "data is framed, not fatal");

        channel.on_read(ReadOutcome::Disconnected);
        assert!(channel.is_destroyed(), "a zero-length read is the peer");
    }

    /// Destroy is idempotent, as the reference's
    /// `IF_NOT_MF_ASSERT_DEV( !isDestroyed_ )` allows a re-call to be a
    /// no-op.
    #[test]
    fn destroy_is_idempotent() {
        let interface = interface();
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), false);
        channel.destroy();
        assert!(channel.is_destroyed());
        channel.destroy();
        assert!(channel.is_destroyed());
    }

    /// The timeout setup records the kernel keepalive parameters and never
    /// lets the channel's own inactivity handler fire, since the kernel
    /// surfaces the dead peer as a disconnect instead.
    #[test]
    fn the_timeout_setup_is_the_kernel_keepalive() {
        let mut channel = TcpChannel::new(&interface(), Address::new(1, 2), false);
        assert_eq!(channel.keepalive_check_period(), None);
        channel.start_inactivity_detection(2.5);
        assert_eq!(channel.keepalive_check_period(), Some(2.5));
        channel.on_channel_inactivity_timeout();
        assert!(
            channel.inactivity_timed_out(),
            "the never-expected CRITICAL_MSG is recorded"
        );
    }

    // -- the acceptance: pending replies and queue state match -----------

    use crate::event_dispatcher::EventDispatcher;
    use crate::interface_element::{
        FIXED_LENGTH_MESSAGE, InterfaceElement, REPLY_MESSAGE_IDENTIFIER,
    };
    use crate::interface_table::InterfaceTable;
    use crate::interfaces::{InputMessageHandler, ReplyMessageHandler};
    use crate::misc::{Reason, ReplyID};
    use crate::network_interface::InterfaceType;
    use crate::reply_order::ReplyOrder;
    use crate::request_manager::{DEFAULT_REQUEST_TIMEOUT, RequestManager};
    use crate::unpacked_message_header::UnpackedMessageHeader;
    use foundation::stream::BinaryStream;
    use std::cell::Cell;

    /// A reply handler that records what it got, so a test can tell two
    /// channels' requests apart.
    #[derive(Clone, Default)]
    struct Recorder {
        /// `( arg, payload )` of each reply delivered.
        delivered: std::cell::RefCell<Vec<(usize, Vec<u8>)>>,
        /// `( reason, arg )` of each failure reported.
        failures: std::cell::RefCell<Vec<(Reason, usize)>>,
    }

    impl ReplyMessageHandler for Recorder {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &UnpackedMessageHeader,
            data: &mut BinaryStream,
            arg: usize,
        ) {
            let payload = data.unread_bytes().to_vec();
            self.delivered.borrow_mut().push((arg, payload));
        }

        fn handle_exception(&self, exception: &crate::nub_exception::NubException, arg: usize) {
            self.failures.borrow_mut().push((exception.reason(), arg));
        }
    }

    impl Recorder {
        fn delivered(&self) -> Vec<(usize, Vec<u8>)> {
            self.delivered.borrow().clone()
        }

        fn failures(&self) -> Vec<(Reason, usize)> {
            self.failures.borrow().clone()
        }
    }

    /// An interface whose request manager is registered, on a pinned
    /// clock, so a disconnect can be watched through the queues.
    fn interface_with_manager(next_id: ReplyID) -> (Rc<NetworkInterface>, Rc<RequestManager>) {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || cell.get())));
        let interface =
            NetworkInterface::new_with_dispatcher(InterfaceType::Internal, Rc::clone(&dispatcher));
        let manager = Rc::new(RequestManager::with_next_reply_id(
            false, next_id, dispatcher,
        ));
        interface.set_request_manager(Some(Rc::clone(&manager)));
        (interface, manager)
    }

    /// One pending request riding `channel`, answered to `handler`.
    fn pending(
        manager: &RequestManager,
        handler: &Rc<Recorder>,
        arg: usize,
        channel: Option<Address>,
    ) -> ReplyID {
        let order = ReplyOrder::new(
            Rc::clone(handler) as Rc<dyn ReplyMessageHandler>,
            arg,
            DEFAULT_REQUEST_TIMEOUT,
            2,
        );
        let mut frame = vec![0u8; 12];
        manager
            .add_reply_order(&order, channel, &mut frame)
            .unwrap()
    }

    /// Deliver a reply for `id` through the manager's receive path, to
    /// prove the request is still in the queue.
    fn reply(manager: &RequestManager, id: ReplyID, payload: &[u8], source: &Address) {
        let mut data = BinaryStream::new();
        data.write_i32(id);
        data.write_bytes(payload);
        let mut header = UnpackedMessageHeader::new(REPLY_MESSAGE_IDENTIFIER);
        header.set_length((std::mem::size_of::<ReplyID>() + payload.len()) as i32);
        InputMessageHandler::handle_message(manager, source, &mut header, &mut data);
    }

    /// The acceptance: a peer disconnect fails exactly the channel's
    /// pending replies, in `REASON_CHANNEL_LOST`, and leaves the rest of
    /// the queue and the in-flight send bytes in their reference state.
    #[test]
    fn a_disconnect_fails_the_channels_replies_and_leaves_the_rest() {
        let (interface, manager) = interface_with_manager(10);
        let remote = Address::new(1, 2);
        let other = Address::new(3, 4);
        let handler = Rc::new(Recorder::default());
        let gone = pending(&manager, &handler, 1, Some(remote));
        let kept = pending(&manager, &handler, 2, Some(other));
        assert_eq!(manager.num_pending(), 2);

        let mut channel = TcpChannel::new(&interface, remote, true);
        let mut buffer = TcpSendBuffer::new(Box::new(BlockThenSend::new()));
        buffer.write(b"in flight");
        channel.set_send_buffer(buffer);

        channel.handle_peer_disconnect();

        // The channel's queue state: destroyed, buffer torn down, and its
        // request failed with the channel-lost reason.
        assert!(channel.is_destroyed());
        assert!(channel.send_buffer().is_none(), "in-flight bytes deleted");
        assert_eq!(
            handler.failures(),
            vec![(Reason::ChannelLost, 1)],
            "only the channel's request failed, and by channel loss"
        );
        assert_eq!(manager.num_pending(), 1, "the other request survives");

        // The survivor is still answerable: its queue entry is intact.
        reply(&manager, kept, b"still here", &other);
        assert_eq!(handler.delivered(), vec![(2, b"still here".to_vec())]);
        assert_eq!(manager.num_pending(), 0);

        // And the failed id is gone: a late reply for it is dropped.
        reply(&manager, gone, b"late", &remote);
        assert_eq!(handler.delivered().len(), 1);
    }

    /// The handshake gate destroys as well: a server whose first frame is
    /// not the header tears the channel down through the same cleanup.
    #[test]
    fn a_rejected_header_destroys_the_channel_and_fails_its_requests() {
        let (interface, manager) = interface_with_manager(10);
        let remote = Address::new(1, 2);
        let handler = Rc::new(Recorder::default());
        pending(&manager, &handler, 1, Some(remote));

        let mut channel = TcpChannel::new(&interface, remote, true);
        assert_eq!(channel.read_header(b"GET /"), HeaderVerdict::Rejected);
        // `handleInput`'s arm after the failed `readHeader`.
        channel.destroy();

        assert!(channel.is_destroyed());
        assert_eq!(handler.failures(), vec![(Reason::ChannelLost, 1)]);
        assert_eq!(manager.num_pending(), 0);
    }

    // -- the receive path: sniff, header gate, dispatch --------------------

    /// A handler that records each `( id, body )` it was handed.
    #[derive(Clone, Default)]
    struct MsgRecorder {
        delivered: std::cell::RefCell<Vec<(u8, Vec<u8>)>>,
    }

    impl InputMessageHandler for MsgRecorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            self.delivered
                .borrow_mut()
                .push((header.identifier(), data.unread_bytes().to_vec()));
        }
    }

    impl MsgRecorder {
        fn delivered(&self) -> Vec<(u8, Vec<u8>)> {
            self.delivered.borrow().clone()
        }
    }

    /// An interface whose message table serves `id` to `recorder`, wired
    /// the way an owner hands its receive path a table.
    fn interface_with_table(
        id: u8,
        length: i32,
        recorder: &Rc<MsgRecorder>,
    ) -> (Rc<NetworkInterface>, Rc<InterfaceTable>) {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || cell.get())));
        let interface = NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher);
        let table = Rc::new(InterfaceTable::new());
        let ie = InterfaceElement::new("msg", id, FIXED_LENGTH_MESSAGE, length, None);
        table.serve(
            &ie,
            Some(Rc::clone(recorder) as Rc<dyn InputMessageHandler>),
        );
        interface.set_interface_table(Some(Rc::clone(&table)));
        (interface, table)
    }

    /// The wire bytes a client sends as its first frame: a small length
    /// prefix over `CLIENT_HEADER`.
    fn client_header_wire() -> Vec<u8> {
        let mut wire = Vec::new();
        wire.extend_from_slice(&(CLIENT_HEADER_SIZE as u16).to_le_bytes());
        wire.extend_from_slice(CLIENT_HEADER);
        wire
    }

    /// A real bundle frame: a small length prefix over `[flags][id][body]`.
    fn small_bundle_wire(id: u8, body: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8, id];
        frame.extend_from_slice(body);
        let mut wire = Vec::new();
        wire.extend_from_slice(&(frame.len() as u16).to_le_bytes());
        wire.extend_from_slice(&frame);
        wire
    }

    /// The WebSockets sniff: a server expecting raw TCP drops a first
    /// frame that begins with the HTTP handshake a WebSockets client sends
    /// (tcp_channel.cpp:194-223).
    #[test]
    fn the_websockets_sniff_drops_a_get_request() {
        let recorder = Rc::new(MsgRecorder::default());
        let (interface, _table) = interface_with_table(0x07, 4, &recorder);
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), true);

        channel.on_data(b"GET / HTTP/1.1\r\nHost: game\r\n\r\n");
        assert!(channel.is_destroyed(), "the handshake is dropped");
        assert!(recorder.delivered().is_empty(), "nothing was framed");
    }

    /// A split WebSockets handshake still trips the sniff once four bytes
    /// are in hand, and waits for more until then.
    #[test]
    fn the_websockets_sniff_waits_then_drops_a_split_request() {
        let recorder = Rc::new(MsgRecorder::default());
        let (interface, _table) = interface_with_table(0x07, 4, &recorder);
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), true);

        channel.on_data(b"GET");
        assert!(!channel.is_destroyed(), "fewer than four bytes wait");
        channel.on_data(b" / HTTP/1.1\r\n");
        assert!(channel.is_destroyed(), "four bytes are enough to sniff");
    }

    /// The server's first real exchange: the header frame passes the gate
    /// and the bundle behind it dispatches through the interface's table.
    #[test]
    fn a_server_channel_dispatches_a_bundle_after_the_header() {
        let recorder = Rc::new(MsgRecorder::default());
        let (interface, _table) = interface_with_table(0x07, 4, &recorder);
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), true);

        let mut wire = Vec::new();
        wire.extend_from_slice(&client_header_wire());
        wire.extend_from_slice(&small_bundle_wire(0x07, b"ABCD"));
        channel.on_data(&wire);

        assert_eq!(channel.header_state(), HeaderState::None);
        assert_eq!(
            recorder.delivered(),
            vec![(0x07, b"ABCD".to_vec())],
            "the frame behind the header frame dispatched"
        );
    }

    /// The acceptance: one table serves a TCP frame and a UDP bundle the
    /// same way -- two transports, one message table, both walks delivering.
    #[test]
    fn one_table_serves_both_a_tcp_frame_and_a_udp_bundle() {
        use crate::bundle_dispatch;

        let clock = Rc::new(Cell::new(1_000_000u64));
        let cell = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || cell.get())));
        let interface = NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher);
        let tcp_recorder = Rc::new(MsgRecorder::default());
        let udp_recorder = Rc::new(MsgRecorder::default());

        // The table serves two ids, so the two transports' messages are
        // told apart in the delivered logs.
        let table = Rc::new(InterfaceTable::new());
        let tcp_ie = InterfaceElement::new("tcp", 0x07, FIXED_LENGTH_MESSAGE, 4, None);
        table.serve(
            &tcp_ie,
            Some(Rc::clone(&tcp_recorder) as Rc<dyn InputMessageHandler>),
        );
        let udp_ie = InterfaceElement::new("udp", 0x08, FIXED_LENGTH_MESSAGE, 4, None);
        table.serve(
            &udp_ie,
            Some(Rc::clone(&udp_recorder) as Rc<dyn InputMessageHandler>),
        );
        interface.set_interface_table(Some(Rc::clone(&table)));

        // The TCP frame, through the TCP walk: `[flags][id][body]`.
        let mut tcp_frame = vec![0u8, 0x07, 0xAA, 0xBB, 0xCC, 0xDD];
        assert_eq!(
            tcp_bundle_dispatch::dispatch_messages(&table, &Address::new(1, 2), &mut tcp_frame),
            Reason::Success
        );

        // The UDP bundle, through the UDP walk: a body with no footer.
        let mut udp_body = vec![0x08, 0x11, 0x22, 0x33, 0x44];
        assert_eq!(
            bundle_dispatch::dispatch_messages(&table, &Address::new(1, 2), &mut udp_body, None),
            Reason::Success
        );

        assert_eq!(
            tcp_recorder.delivered(),
            vec![(0x07, vec![0xAA, 0xBB, 0xCC, 0xDD])]
        );
        assert_eq!(
            udp_recorder.delivered(),
            vec![(0x08, vec![0x11, 0x22, 0x33, 0x44])]
        );
    }

    /// A client channel (no header gate to pass) dispatches a bundle the
    /// moment its bytes frame -- the else arm of `on_data`.
    #[test]
    fn a_client_channel_dispatches_its_frames_directly() {
        let recorder = Rc::new(MsgRecorder::default());
        let (interface, _table) = interface_with_table(0x07, 4, &recorder);
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), false);

        channel.on_data(&small_bundle_wire(0x07, b"WXYZ"));
        assert_eq!(recorder.delivered(), vec![(0x07, b"WXYZ".to_vec())]);
    }

    /// The header gate inside the framing loop rejects a first frame that
    /// is not the header -- and a frame that is not a WebSockets handshake
    /// reaches it, since the sniff only tripped on `GET `.
    #[test]
    fn a_non_header_first_frame_is_destroyed_in_the_framing_loop() {
        let recorder = Rc::new(MsgRecorder::default());
        let (interface, _table) = interface_with_table(0x07, 4, &recorder);
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), true);

        // A small frame whose payload is not the header and does not begin
        // with "GET ", so the sniff passes and `readHeader` rejects it.
        let mut wire = Vec::new();
        wire.extend_from_slice(&4u16.to_le_bytes());
        wire.extend_from_slice(b"ABCD");
        channel.on_data(&wire);

        assert!(channel.is_destroyed(), "the mismatched header is fatal");
        assert!(recorder.delivered().is_empty());
    }

    /// With no message table wired, a finished frame has nothing to serve
    /// and nothing to do -- the channel's dispatch is a no-op.
    #[test]
    fn a_frame_with_no_table_is_not_delivered() {
        let interface = interface();
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), false);
        channel.process_received_frame(&[0u8, 0x07, 0xAA, 0xBB]);
        assert!(!channel.is_destroyed(), "no table is not an error");
    }

    /// A channel whose interface is gone cannot reach a table either; the
    /// weak pointer simply does not upgrade.
    #[test]
    fn a_frame_with_a_dropped_interface_is_not_delivered() {
        let interface = interface();
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), false);
        drop(interface);
        channel.process_received_frame(&[0u8, 0x07, 0xAA, 0xBB]);
        assert!(!channel.is_destroyed());
    }

    /// A frame the walk stops on (here an unhandled id) is reported by
    /// the channel and the rest of the frame stays undelivered.
    #[test]
    fn a_dispatch_failure_is_reported_and_stops_the_frame() {
        let recorder = Rc::new(MsgRecorder::default());
        let (interface, _table) = interface_with_table(0x07, 4, &recorder);
        let mut channel = TcpChannel::new(&interface, Address::new(1, 2), false);

        // id 0x2F is served by nobody, so the walk stops before the message
        // after it would have been delivered.
        let mut frame = vec![0u8, 0x2F, 0x01];
        frame.extend_from_slice(&[0x07, 0xAA, 0xBB, 0xCC, 0xDD]);
        let mut wire = Vec::new();
        wire.extend_from_slice(&(frame.len() as u16).to_le_bytes());
        wire.extend_from_slice(&frame);
        channel.on_data(&wire);

        assert!(recorder.delivered().is_empty(), "nothing past the bad id");
    }
}
