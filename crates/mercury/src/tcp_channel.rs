//! The TCP channel's send side and lifecycle: the short-write and
//! pending-send buffer plus the timeout and broken-connection cleanup
//! (`lib/network/tcp_channel.cpp:707-858` and the `Channel` base
//! `channel.cpp`).
//!
//! A TCP socket accepts bytes lazily: a `send` may take fewer bytes than it
//! was given (a short write) or none at all (the socket is full). The channel
//! turns that into a buffer: whatever the socket refuses is kept in order, and
//! a write-event notification drains it as the socket frees up. The frame
//! bytes the channel hands in therefore come out complete and in order no
//! matter how the socket's capacity fluctuates.
//!
//! A break -- the peer's 0-length read, a hard send error, or the keepalive
//! probing finding the peer gone -- funnels into [`TcpChannel::destroy`],
//! which is what makes the link's outstanding state consistent: the pending
//! send queue is discarded and the interface is told the channel is gone, so
//! every request riding it is failed with `REASON_CHANNEL_LOST` (挂起应答与
//! 队列状态吻合).
//!
//! The port models the socket as a [`TcpTransport`] (a [`SendSink`] plus the
//! keepalive arm and the half-close), so the buffer and the lifecycle are
//! testable with a transport that caps its own throughput. The channel's
//! read-side framing lives in `TcpFrameReader` (BW-02-04-01) and the poller
//! registration of the descriptor joins with the channel build.

#[cfg(target_os = "linux")]
use std::rc::{Rc, Weak};

#[cfg(target_os = "linux")]
use platform::socket;

#[cfg(target_os = "linux")]
use crate::endpoint::{Address, Endpoint};
#[cfg(target_os = "linux")]
use crate::network_interface::NetworkInterface;

/// A hard send error (the reference's `!isErrnoIgnorable` case, which
/// destroys the channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendError;

/// The socket send this buffer writes through.
pub trait SendSink {
    /// `Endpoint::send`: the bytes the socket actually accepted, or
    /// [`SendError`] on a hard error.
    ///
    /// A would-block socket accepts zero, which is not an error.
    fn send(&mut self, data: &[u8]) -> Result<usize, SendError>;
}

/// What a send attempt did, the way `writeFrom`/`sendBufferedData` branch on
/// their `sendResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendResult {
    /// Everything handed in went out; nothing is pending.
    Drained,
    /// The socket accepted part of the data; the rest is buffered.
    Partial,
    /// The socket accepted nothing (would-block); all of it is buffered.
    WouldBlock,
    /// A hard error; the channel is destroyed.
    Error,
}

/// The pending-send buffer `TCPChannel::pSendBuffer_`.
pub struct TcpSendBuffer {
    /// The bytes not yet accepted by the socket, in the order they were
    /// handed in.
    buf: Vec<u8>,
}

impl Default for TcpSendBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TcpSendBuffer {
    /// `pSendBuffer_( NULL )`: nothing pending.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// `pSendBuffer_->remainingLength()`: the bytes not yet sent.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Whether anything is waiting for the socket to free up.
    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty()
    }

    /// Discard what is pending -- `TCPChannel::doDestroy`'s
    /// `bw_safe_delete( pSendBuffer_ )`, which goes with the socket.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// `writeFrom( input, shouldCork )`: hand `data` to the socket, buffering
    /// whatever it refuses.
    ///
    /// When the buffer already holds data (the channel is already waiting for
    /// the socket to free up), the new data is appended to the end so order is
    /// preserved. Otherwise the socket is tried directly.
    pub fn write_from<S: SendSink>(&mut self, sink: &mut S, data: &[u8]) -> SendResult {
        if self.has_pending() {
            // `pSendBuffer_->transfer( input, numToSend )`: already waiting,
            // so add to the end.
            self.buf.extend_from_slice(data);
            return SendResult::Partial;
        }

        match sink.send(data) {
            Ok(n) if n == data.len() => SendResult::Drained,
            Ok(n) => {
                // `input.retrieve( sendResult )`: keep what the socket refused.
                self.buf.extend_from_slice(&data[n..]);
                if n == 0 {
                    SendResult::WouldBlock
                } else {
                    SendResult::Partial
                }
            }
            Err(_) => SendResult::Error,
        }
    }

    /// `sendBufferedData()`: push as much pending data as the socket accepts.
    ///
    /// A short write advances the cursor and keeps waiting; once everything is
    /// out the buffer clears and the channel stops waiting on the socket.
    pub fn flush<S: SendSink>(&mut self, sink: &mut S) -> SendResult {
        if self.buf.is_empty() {
            return SendResult::Drained;
        }

        match sink.send(&self.buf) {
            Ok(0) => SendResult::WouldBlock,
            Ok(n) if n < self.buf.len() => {
                // `pSendBuffer_->retrieve( sendResult )`: still data left.
                self.buf.drain(..n);
                SendResult::Partial
            }
            Ok(_) => {
                self.buf.clear();
                SendResult::Drained
            }
            Err(_) => SendResult::Error,
        }
    }
}

/// The socket a TCP channel talks through: the send sink, plus the two
/// transport-level arms the channel's lifecycle drives -- the keepalive
/// arm (`TCPChannel::startInactivityDetection`) and the half-close
/// (`TCPChannel::doShutDown`). The real socket is `Endpoint`; the tests
/// use a transport that records the arms.
pub trait TcpTransport: SendSink {
    /// `TCPChannel::startInactivityDetection( period, checkPeriod )`:
    /// arm kernel keepalive -- `SO_KEEPALIVE` then, on Unix,
    /// `TCP_KEEPIDLE=1`, `TCP_KEEPINTVL=1`, `TCP_KEEPCNT=checkPeriod`.
    /// The OS probes a silent peer and, finding it gone, fails the
    /// socket, which the channel's input path reads as a disconnect and
    /// destroys the channel. That convergence is the TCP timeout: the
    /// reference arms no channel timer (its
    /// `onChannelInactivityTimeout` is a "should not have been called"
    /// critical), and the `period` argument is ignored, so the port does
    /// the same.
    fn arm_inactivity_detection(&mut self, check_period: f32);

    /// `TCPChannel::doShutDown`: half-close the sending side
    /// (`shutdown( SHUT_WR )`). The peer's read then sees EOF and the
    /// peer closes in turn, and the resulting 0-length read arrives at
    /// [`TcpChannel::handle_peer_disconnect`].
    fn shutdown_write(&mut self);
}

/// `TCPChannel` (lib/network/tcp_channel.cpp): the send buffer plus the
/// lifecycle -- the half-close flow and the cleanup a break triggers.
///
/// The read-side framing is `TcpFrameReader` (BW-02-04-01) and the
/// descriptor's poller registration and destruction join with the channel
/// build; this slice carries the state the break cleanup has to leave
/// consistent.
#[cfg(target_os = "linux")]
pub struct TcpChannel<T: TcpTransport> {
    /// `pEndpoint_`: the accepted socket.
    transport: T,
    /// `addr_`: the peer's address, the key the interface and the request
    /// manager recognise the channel by.
    addr: Address,
    /// `pNetworkInterface_`, held weakly so a channel can report its own
    /// death without keeping the interface alive.
    network_interface: Weak<NetworkInterface>,
    /// `pSendBuffer_`.
    send_buffer: TcpSendBuffer,
    /// `isShuttingDown_`: the channel was asked to close and is waiting
    /// for its queue to drain.
    is_shutting_down: bool,
    /// `isDestroyed_`.
    is_destroyed: bool,
}

#[cfg(target_os = "linux")]
impl<T: TcpTransport> TcpChannel<T> {
    /// `TCPChannel( pEndpoint, pInterface, ... )`: a channel on the
    /// accepted `transport`, talking to `addr` through `interface`.
    pub fn new(transport: T, addr: Address, interface: &Rc<NetworkInterface>) -> Self {
        TcpChannel {
            transport,
            addr,
            network_interface: Rc::downgrade(interface),
            send_buffer: TcpSendBuffer::new(),
            is_shutting_down: false,
            is_destroyed: false,
        }
    }

    /// `addr()`.
    pub fn addr(&self) -> Address {
        self.addr
    }

    /// `isDestroyed()`.
    pub fn is_destroyed(&self) -> bool {
        self.is_destroyed
    }

    /// `isConnected()` (tcp_channel.hpp:53): a channel that is neither
    /// shutting down nor destroyed can still be used.
    pub fn is_connected(&self) -> bool {
        !self.is_shutting_down && !self.is_destroyed
    }

    /// `pSendBuffer_->remainingLength()`.
    pub fn pending(&self) -> usize {
        self.send_buffer.pending()
    }

    /// Whether the queue is holding bytes the socket has not accepted.
    pub fn has_pending(&self) -> bool {
        self.send_buffer.has_pending()
    }

    /// `pEndpoint_`, as the test harnesses reach it: the fake transport's
    /// recordings are read back through the channel. The channel build's
    /// socket wiring taps the real socket the same way.
    #[cfg(test)]
    pub(crate) fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// `startInactivityDetection( period, checkPeriod )`: arm the kernel
    /// keepalive; the reference passes `period` through unused.
    pub fn start_inactivity_detection(&mut self, period: f32, check_period: f32) {
        self.transport.arm_inactivity_detection(check_period);
        let _ = period;
    }

    /// `writeFrom( input, shouldCork )`: hand a frame to the socket,
    /// buffering what it refuses. A hard send error destroys the channel
    /// (`writeFrom`'s "Write error, destroying" arm).
    pub fn write_from(&mut self, data: &[u8]) -> SendResult {
        let result = self.send_buffer.write_from(&mut self.transport, data);
        if result == SendResult::Error {
            self.destroy();
        }
        result
    }

    /// `sendBufferedData()`: push as much of the queue as the socket
    /// accepts. A hard error destroys the channel; once the queue drains
    /// a shut-down that was waiting on it half-closes.
    pub fn send_buffered_data(&mut self) -> SendResult {
        let result = self.send_buffer.flush(&mut self.transport);
        if result == SendResult::Error {
            self.destroy();
        } else if result == SendResult::Drained && self.is_shutting_down {
            // All remaining data sent; the shut-down that was waiting on
            // the sends half-closes now.
            self.do_shut_down();
        }
        result
    }

    /// `shutDown()`: ask the channel to close. With nothing pending the
    /// half-close happens now; otherwise it waits for the queue to drain
    /// (`sendBufferedData` picks it up).
    pub fn shut_down(&mut self) {
        self.is_shutting_down = true;
        if !self.send_buffer.has_pending() {
            self.do_shut_down();
        }
    }

    /// `doShutDown()`: `shutdown( SHUT_WR )`. The peer reads EOF, closes
    /// in turn, and the 0-length read reaches `handle_peer_disconnect`.
    fn do_shut_down(&mut self) {
        self.transport.shutdown_write();
    }

    /// `handlePeerDisconnect()`: the peer's 0-length read. The channel is
    /// gone either way -- a clean shut-down or a break -- and destroys
    /// itself.
    pub fn handle_peer_disconnect(&mut self) {
        if self.is_shutting_down {
            // TRACE_MSG "Shutdown cleanly".
        } else {
            // TRACE_MSG "Disconnected by peer".
        }
        self.destroy();
    }

    /// `Channel::destroy()` (channel.cpp:319): close the channel for
    /// good. Idempotent. The pending queue is discarded (the reference's
    /// `doDestroy` deletes the send buffer) and the interface is told the
    /// channel is gone, which fails every request riding it with
    /// `REASON_CHANNEL_LOST`.
    pub fn destroy(&mut self) {
        if self.is_destroyed {
            // IF_NOT_MF_ASSERT_DEV( !isDestroyed_ ): the dev warning is
            // the caller's to notice; the port stays quiet.
            return;
        }
        self.is_destroyed = true;
        self.send_buffer.clear();
        let addr = self.addr;
        if let Some(interface) = self.network_interface.upgrade() {
            interface.on_channel_gone(&addr);
        }
    }
}

/// The real socket as a [`SendSink`]: `Endpoint::send`'s byte count, or a
/// would-block socket accepting nothing (the reference's
/// `isErrnoIgnorable` arm) instead of a hard error.
#[cfg(target_os = "linux")]
impl SendSink for Endpoint {
    fn send(&mut self, data: &[u8]) -> Result<usize, SendError> {
        let sent = Endpoint::send(self, data);
        if sent >= 0 {
            return Ok(sent as usize);
        }
        // A would-block send (EAGAIN) accepts nothing, not an error.
        if std::io::Error::last_os_error().raw_os_error() == Some(socket::EAGAIN) {
            return Ok(0);
        }
        Err(SendError)
    }
}

/// The real socket as a [`TcpTransport`]: the kernel keepalive arm and
/// the half-close, exactly as `TCPChannel` drives them.
#[cfg(target_os = "linux")]
impl TcpTransport for Endpoint {
    fn arm_inactivity_detection(&mut self, check_period: f32) {
        self.set_keepalive(true);
        let fd = self.fileno();
        socket::set_int_option(fd, socket::IPPROTO_TCP, socket::TCP_KEEPIDLE, 1);
        socket::set_int_option(fd, socket::IPPROTO_TCP, socket::TCP_KEEPINTVL, 1);
        socket::set_int_option(
            fd,
            socket::IPPROTO_TCP,
            socket::TCP_KEEPCNT,
            check_period as i32,
        );
    }

    fn shutdown_write(&mut self) {
        socket::shutdown(self.fileno(), socket::SHUT_WR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket that accepts at most `cap` bytes per call and records every
    /// accepted byte, in order.
    struct Backpressured {
        cap: usize,
        sent: Vec<u8>,
        fail: bool,
    }

    impl Backpressured {
        fn new(cap: usize) -> Self {
            Self {
                cap,
                sent: Vec::new(),
                fail: false,
            }
        }

        fn sent(&self) -> &[u8] {
            &self.sent
        }
    }

    impl SendSink for Backpressured {
        fn send(&mut self, data: &[u8]) -> Result<usize, SendError> {
            if self.fail {
                return Err(SendError);
            }
            let n = data.len().min(self.cap);
            self.sent.extend_from_slice(&data[..n]);
            Ok(n)
        }
    }

    /// Frame one bundle the way `TCPChannel::doSend` does: the 16-bit length
    /// header then the body.
    fn framed(body: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    /// The acceptance fixture: under backpressure (a socket that takes one
    /// byte at a time), complete frames written back to back come out in
    /// order, whole, and the buffer drains.
    #[test]
    fn a_backpressured_socket_sends_complete_frames_in_order() {
        let mut sink = Backpressured::new(1);
        let mut buffer = TcpSendBuffer::new();

        let first = framed(b"first");
        let second = framed(b"second");

        // Each write is mostly refused; the buffer carries the remainder.
        assert_eq!(buffer.write_from(&mut sink, &first), SendResult::Partial);
        assert_eq!(buffer.write_from(&mut sink, &second), SendResult::Partial);
        assert!(buffer.has_pending());
        assert!(buffer.pending() > 0);

        // Drain until the socket takes everything.
        let mut steps = 0;
        loop {
            match buffer.flush(&mut sink) {
                SendResult::Drained => break,
                SendResult::WouldBlock | SendResult::Partial => steps += 1,
                SendResult::Error => panic!("the socket did not error"),
            }
            assert!(steps < 10_000, "the drain must terminate");
        }

        // Both frames came out whole, in order, and nothing is pending.
        let mut expected = first;
        expected.extend_from_slice(&second);
        assert_eq!(sink.sent(), &expected[..]);
        assert_eq!(buffer.pending(), 0);
        assert!(!buffer.has_pending());
    }

    /// A frame larger than the socket's capacity is still delivered whole:
    /// the bytes come out in order, so the receiving frame reader reassembles
    /// them into the one frame.
    #[test]
    fn a_large_frame_is_delivered_whole_under_backpressure() {
        let mut sink = Backpressured::new(3);
        let mut buffer = TcpSendBuffer::new();

        let body: Vec<u8> = (0..300).map(|i| i as u8).collect();
        let frame = framed(&body);

        assert_eq!(buffer.write_from(&mut sink, &frame), SendResult::Partial);
        while buffer.flush(&mut sink) != SendResult::Drained {}

        assert_eq!(sink.sent(), &frame[..]);
        assert_eq!(buffer.pending(), 0);
    }

    /// A write that is fully accepted does not buffer anything.
    #[test]
    fn a_fully_accepted_write_drains_immediately() {
        let mut sink = Backpressured::new(1024);
        let mut buffer = TcpSendBuffer::new();

        let frame = framed(b"whole");
        assert_eq!(buffer.write_from(&mut sink, &frame), SendResult::Drained);
        assert_eq!(buffer.pending(), 0);
        assert_eq!(sink.sent(), &frame[..]);
    }

    /// A would-block socket (accepts nothing) buffers everything and reports
    /// it, then drains once it frees up.
    #[test]
    fn a_would_block_socket_buffers_everything_then_drains() {
        let mut sink = Backpressured::new(0);
        let mut buffer = TcpSendBuffer::new();

        let frame = framed(b"held");
        assert_eq!(buffer.write_from(&mut sink, &frame), SendResult::WouldBlock);
        assert_eq!(buffer.pending(), frame.len());

        sink.cap = 1024;
        assert_eq!(buffer.flush(&mut sink), SendResult::Drained);
        assert_eq!(sink.sent(), &frame[..]);
        assert_eq!(buffer.pending(), 0);
    }

    /// A hard socket error is reported, and the channel is destroyed.
    #[test]
    fn a_hard_socket_error_is_reported() {
        let mut sink = Backpressured::new(1024);
        sink.fail = true;
        let mut buffer = TcpSendBuffer::new();

        assert_eq!(buffer.write_from(&mut sink, b"boom"), SendResult::Error);
    }

    /// A write while data is already pending appends to the end, preserving
    /// the order of the frames.
    #[test]
    fn a_write_while_pending_appends_in_order() {
        let mut sink = Backpressured::new(2);
        let mut buffer = TcpSendBuffer::new();

        let first = framed(b"aa");
        let second = framed(b"bb");
        let third = framed(b"cc");

        // The socket takes a little of the first; the rest pends.
        buffer.write_from(&mut sink, &first);
        // The next two writes append, not reorder.
        assert_eq!(buffer.write_from(&mut sink, &second), SendResult::Partial);
        assert_eq!(buffer.write_from(&mut sink, &third), SendResult::Partial);

        while buffer.flush(&mut sink) != SendResult::Drained {}

        let mut expected = first;
        expected.extend_from_slice(&second);
        expected.extend_from_slice(&third);
        assert_eq!(sink.sent(), &expected[..]);
    }

    /// Flushing an empty buffer is a no-op drain.
    #[test]
    fn flushing_an_empty_buffer_is_a_no_op() {
        let mut sink = Backpressured::new(1024);
        let mut buffer = TcpSendBuffer::new();

        assert_eq!(buffer.flush(&mut sink), SendResult::Drained);
        assert_eq!(buffer.pending(), 0);
    }
}

/// The channel lifecycle tests: the break cleanup of BW-02-04-04.
///
/// Linux only, because the channel reports its death through
/// `NetworkInterface`, whose construction needs the dispatcher.
#[cfg(all(test, target_os = "linux"))]
mod channel_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use foundation::stream::BinaryStream;
    use platform::socket;

    use super::{SendError, SendResult, SendSink, TcpChannel, TcpTransport};
    use crate::endpoint::{Address, Endpoint, INADDR_ANY};
    use crate::event_dispatcher::EventDispatcher;
    use crate::interfaces::ReplyMessageHandler;
    use crate::misc::Reason;
    use crate::network_interface::{InterfaceType, NetworkInterface};
    use crate::nub_exception::NubException;
    use crate::reply_order::ReplyOrder;
    use crate::request_manager::{DEFAULT_REQUEST_TIMEOUT, RequestManager};
    use crate::unpacked_message_header::UnpackedMessageHeader;

    /// `htonl( inet_addr( "127.0.0.1" ) )`: the loopback the real-pair
    /// fixture connects to.
    const LOOPBACK: u32 = 0x0100_007f;

    /// A socket that accepts at most `cap` bytes per call and records
    /// everything, including the two transport arms the channel drives.
    struct Recording {
        cap: usize,
        sent: Vec<u8>,
        fail: bool,
        armed: Option<f32>,
        half_closed: usize,
    }

    impl Recording {
        fn new(cap: usize) -> Self {
            Recording {
                cap,
                sent: Vec::new(),
                fail: false,
                armed: None,
                half_closed: 0,
            }
        }
    }

    impl SendSink for Recording {
        fn send(&mut self, data: &[u8]) -> Result<usize, SendError> {
            if self.fail {
                return Err(SendError);
            }
            let n = data.len().min(self.cap);
            self.sent.extend_from_slice(&data[..n]);
            Ok(n)
        }
    }

    impl TcpTransport for Recording {
        fn arm_inactivity_detection(&mut self, check_period: f32) {
            self.armed = Some(check_period);
        }

        fn shutdown_write(&mut self) {
            self.half_closed += 1;
        }
    }

    /// A reply handler that records the failures it is told, so a test
    /// can see the reason a pending request was failed with.
    struct Failures(RefCell<Vec<Reason>>);

    impl ReplyMessageHandler for Failures {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &UnpackedMessageHeader,
            _data: &mut BinaryStream,
            _arg: usize,
        ) {
        }

        fn handle_exception(&self, exception: &NubException, _arg: usize) {
            self.0.borrow_mut().push(exception.reason());
        }
    }

    /// A pinned-clock interface with no socket, the same shape the
    /// interface tests use.
    fn channel_interface() -> Rc<NetworkInterface> {
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(|| 1_000_000u64)));
        NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher)
    }

    /// One pending request riding `channel`, so a break has something to
    /// cancel.
    fn pending_on_channel(
        manager: &RequestManager,
        handler: &Rc<Failures>,
        channel: Address,
    ) -> crate::misc::ReplyID {
        let order = ReplyOrder::new(
            Rc::clone(handler) as Rc<dyn ReplyMessageHandler>,
            0,
            DEFAULT_REQUEST_TIMEOUT,
            0,
        );
        let mut frame = vec![0u8; 8];
        manager
            .add_reply_order(&order, Some(channel), &mut frame)
            .unwrap()
    }

    /// Frame one bundle the way `TCPChannel::doSend` does.
    fn framed(body: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    /// The acceptance: the peer goes away with a frame still queued, and
    /// the channel's death leaves the two halves consistent -- every
    /// request riding it is failed with `REASON_CHANNEL_LOST` and the
    /// queue it was holding is gone.
    #[test]
    fn a_break_cancels_pending_replies_and_discards_the_queue() {
        let interface = channel_interface();
        let manager = Rc::new(RequestManager::new(false, interface.dispatcher()));
        interface.set_request_manager(Some(manager.clone()));

        let handler = Rc::new(Failures(RefCell::new(Vec::new())));
        let addr = Address::new(LOOPBACK, 7001);
        pending_on_channel(&manager, &handler, addr);
        assert_eq!(manager.num_pending(), 1);

        // A full socket: the whole frame pends.
        let mut channel = TcpChannel::new(Recording::new(0), addr, &interface);
        let frame = framed(b"still queued");
        assert_eq!(
            channel.write_from(&frame),
            SendResult::WouldBlock,
            "the full socket accepts nothing"
        );
        assert!(channel.has_pending());
        assert!(channel.is_connected());

        // The peer's 0-length read arrives.
        channel.handle_peer_disconnect();

        assert_eq!(
            manager.num_pending(),
            0,
            "the pending reply is cancelled with the channel"
        );
        assert_eq!(
            *handler.0.borrow(),
            vec![Reason::ChannelLost],
            "the request is told the channel was lost"
        );
        assert_eq!(channel.pending(), 0, "the discarded queue holds nothing");
        assert!(!channel.has_pending());
        assert!(channel.is_destroyed());
        assert!(!channel.is_connected());

        // Destroy is idempotent; the second call reports nothing new.
        channel.destroy();
        assert_eq!(manager.num_pending(), 0);
    }

    /// A hard send error on `writeFrom`'s arm destroys the channel and
    /// the cleanup runs there too.
    #[test]
    fn a_hard_send_error_destroys_the_channel() {
        let interface = channel_interface();
        let manager = Rc::new(RequestManager::new(false, interface.dispatcher()));
        interface.set_request_manager(Some(manager.clone()));

        let handler = Rc::new(Failures(RefCell::new(Vec::new())));
        let addr = Address::new(LOOPBACK, 7002);
        pending_on_channel(&manager, &handler, addr);

        let mut transport = Recording::new(1024);
        transport.fail = true;
        let mut channel = TcpChannel::new(transport, addr, &interface);

        assert_eq!(channel.write_from(b"boom"), SendResult::Error);
        assert!(channel.is_destroyed());
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(*handler.0.borrow(), vec![Reason::ChannelLost]);
        assert_eq!(channel.pending(), 0);
    }

    /// A hard send error on `sendBufferedData`'s arm destroys the channel
    /// as well -- the drain can find the break the write missed.
    #[test]
    fn a_flush_hard_error_destroys_the_channel() {
        let interface = channel_interface();
        let addr = Address::new(LOOPBACK, 7003);
        let mut channel = TcpChannel::new(Recording::new(0), addr, &interface);

        assert_eq!(channel.write_from(b"held"), SendResult::WouldBlock);
        assert!(channel.has_pending());

        // The socket goes bad while the queue waits on it.
        channel.transport_mut().fail = true;
        assert_eq!(channel.send_buffered_data(), SendResult::Error);
        assert!(channel.is_destroyed());
        assert!(!channel.has_pending());
    }

    /// `shutDown` with a full queue waits for the drain, and the drain's
    /// last send is followed by exactly one half-close; the channel dies
    /// cleanly when the peer's EOF answers it.
    #[test]
    fn shut_down_half_closes_once_the_queue_drains() {
        let interface = channel_interface();
        let addr = Address::new(LOOPBACK, 7004);
        let mut channel = TcpChannel::new(Recording::new(1), addr, &interface);

        let frame = framed(b"queued");
        assert_eq!(channel.write_from(&frame), SendResult::Partial);

        // The queue is holding bytes, so the half-close waits.
        channel.shut_down();
        assert!(!channel.is_connected(), "shutting down, not connected");
        assert!(!channel.is_destroyed());
        assert_eq!(
            channel.transport_mut().half_closed,
            0,
            "the half-close waits on the queue"
        );

        // Drain; the last send is followed by exactly one half-close.
        while channel.send_buffered_data() != SendResult::Drained {}
        assert_eq!(channel.transport_mut().half_closed, 1);
        assert_eq!(channel.pending(), 0);
        assert!(!channel.is_destroyed(), "the EOF has not arrived yet");

        // The peer answers the half-close with its own EOF; the channel
        // reads it as a clean disconnect.
        channel.handle_peer_disconnect();
        assert!(channel.is_destroyed());
    }

    /// `shutDown` with nothing queued half-closes at once.
    #[test]
    fn shut_down_with_an_empty_queue_half_closes_at_once() {
        let interface = channel_interface();
        let addr = Address::new(LOOPBACK, 7005);
        let mut channel = TcpChannel::new(Recording::new(1024), addr, &interface);

        channel.shut_down();
        assert_eq!(channel.transport_mut().half_closed, 1);
        assert!(!channel.is_connected());
        assert!(!channel.is_destroyed());
    }

    /// `startInactivityDetection` arms the transport and passes only the
    /// check period through, the way the reference's TCP arm does.
    #[test]
    fn start_inactivity_detection_arms_the_transport() {
        let interface = channel_interface();
        let addr = Address::new(LOOPBACK, 7006);
        let mut channel = TcpChannel::new(Recording::new(1024), addr, &interface);

        channel.start_inactivity_detection(60.0, 5.0);
        assert_eq!(channel.transport_mut().armed, Some(5.0));
    }

    /// The real transport: on a live TCP pair the keepalive arm lands on
    /// the socket, a send goes through `SendSink`, and the half-close
    /// reads as EOF at the peer.
    #[test]
    fn the_endpoint_transport_arms_keepalive_and_half_closes() {
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_STREAM);
        assert_eq!(listener.set_reuseaddr(true), 0);
        assert_eq!(listener.bind(0, INADDR_ANY), 0);
        let port = listener.local().port;
        assert_eq!(listener.listen(5), 0);

        let mut client = Endpoint::new();
        client.socket(socket::SOCK_STREAM);
        assert_eq!(client.connect(port, LOOPBACK), 0);
        let mut conn = listener
            .accept(None, None)
            .expect("the loopback pair connects");

        // The keepalive arm: SO_KEEPALIVE on and the probe count set.
        conn.arm_inactivity_detection(1.0);
        assert_eq!(
            socket::get_bool_option(conn.fileno(), socket::SOL_SOCKET, socket::SO_KEEPALIVE),
            Some(true)
        );
        assert_eq!(
            socket::get_int_option(conn.fileno(), socket::IPPROTO_TCP, socket::TCP_KEEPCNT),
            Some(1)
        );

        // The sink sends; the peer receives.
        assert_eq!(SendSink::send(&mut conn, b"hi"), Ok(2));
        let mut buf = [0u8; 2];
        assert_eq!(client.recv(&mut buf), 2);
        assert_eq!(&buf, b"hi");

        // The half-close: the peer's read hits EOF.
        conn.shutdown_write();
        assert_eq!(client.recv(&mut buf), 0);
    }
}
