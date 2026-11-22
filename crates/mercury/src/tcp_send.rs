//! The TCP short-write and pending-send-buffer machinery: `TCPChannel::
//! writeFrom` (lib/network/tcp_channel.cpp:707-764) and `sendBufferedData`
//! (:818-858), with `basicSend` (:780-810) as the socket seam.
//!
//! A frame is written as one blob (`doSend`, :414-452, `frameData` = length
//! prefix + bundle bytes). The socket can take some of it, none of it, or all
//! of it:
//!
//! * all of it -- nothing to remember, the write is done;
//! * none of it, with a busy socket (`EAGAIN`/`EWOULDBLOCK`/`EINTR`, the
//!   `isErrnoIgnorable` arm) -- the whole blob goes to the send buffer and the
//!   channel asks the dispatcher to call it when the socket is writable;
//! * some of it -- the rest goes to the send buffer, the same way.
//!
//! While a buffer exists, every later frame appends to its tail, so the bytes
//! leave in exactly the order they were written -- which is the whole
//! acceptance: frames queue whole behind a busy socket and drain whole, in
//! order, as the socket takes them. When a drain empties the buffer the write
//! registration is dropped, and a shut-down that was waiting on the pending
//! data proceeds (the `isShuttingDown_` arm of :850-854).
//!
//! The port has no socket yet, so `basicSend` is a seam: a [`TcpSender`]
//! stands in for `Endpoint::send`, answering a byte count or the failure class
//! `writeFrom` splits by errno. The `TCP_CORK` option `basicSend` toggles for
//! `shouldCork` is not here -- `doSend` always writes a frame uncorked, and
//! corking belongs to the stream-filter layer (`pStreamFilter_->writeFrom`,
//! :443-451), which rides with the other transports.

use std::cell::Cell;

/// One `basicSend` attempt, as `writeFrom` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendAttempt {
    /// `Endpoint::send` wrote this many bytes, possibly fewer than asked.
    Sent(usize),
    /// `send` failed with an ignorable errno -- the socket is busy, not broken.
    /// `writeFrom` treats this as writing nothing and buffers everything.
    WouldBlock,
    /// `send` failed hard: the channel destroys itself.
    Failed,
}

/// `basicSend`: write `data`, answering how much went out. This is the
/// `Endpoint::send` seam.
pub trait TcpSender {
    /// The uncorked send, in the `basicSend` shape: bytes written, or the
    /// failure class.
    fn send(&mut self, data: &[u8]) -> SendAttempt;
}

/// The outcome of `write`, as `doSend` would read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteResult {
    /// Every byte went out immediately; nothing is pending.
    Sent,
    /// Some or all of the bytes are queued; the channel waits for the socket to
    /// be writable.
    Buffered,
    /// A hard send error: the channel must destroy itself.
    Failed,
}

/// The outcome of `flush`, as `sendBufferedData` would read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushResult {
    /// The buffer drained completely. `shutdown_now` is true when a shutdown
    /// had been waiting on the pending data and may now proceed.
    Drained { shutdown_now: bool },
    /// Some data remains; keep waiting for the socket.
    StillBuffered,
    /// A hard send error: the channel must destroy itself.
    Failed,
}

/// The pending-send buffer of one TCP channel: `pSendBuffer_` plus the write
/// registration it implies, and the deferred shutdown `sendBufferedData` fires
/// once the buffer empties.
pub struct TcpSendBuffer {
    /// The queued bytes, front at index 0: `pSendBuffer_`.
    buffer: Vec<u8>,
    /// The socket seam `basicSend` goes through.
    sender: Box<dyn TcpSender>,
    /// `isShuttingDown_`: a shutdown waiting on the pending data. The
    /// reference keeps this on the channel; it rides here so the deferred
    /// `doShutDown` is observable by the same test.
    shutting_down: Cell<bool>,
}

impl TcpSendBuffer {
    /// An empty buffer with nothing pending and nothing shutting down.
    pub fn new(sender: Box<dyn TcpSender>) -> Self {
        Self {
            buffer: Vec::new(),
            sender,
            shutting_down: Cell::new(false),
        }
    }

    /// `isShuttingDown_ = true`: the next time the buffer drains, `flush`
    /// reports `shutdown_now`.
    pub fn begin_shutdown(&mut self) {
        self.shutting_down.set(true);
    }

    /// The bytes queued but not yet handed to the socket.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Whether a buffer exists, i.e. the channel is waiting for a writable
    /// socket: `pSendBuffer_ != NULL`.
    pub fn is_buffering(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// `writeFrom( input, shouldCork )`: hand `bytes` to the socket, or queue
    /// them when the socket is busy or takes only part of a frame.
    pub fn write(&mut self, bytes: &[u8]) -> WriteResult {
        if !self.buffer.is_empty() {
            // Already waiting for the socket to become writable: add to the
            // end, keeping the bytes in the order they were written.
            self.buffer.extend_from_slice(bytes);
            return WriteResult::Buffered;
        }

        match self.sender.send(bytes) {
            SendAttempt::Failed => WriteResult::Failed,
            SendAttempt::WouldBlock => {
                self.buffer.extend_from_slice(bytes);
                WriteResult::Buffered
            }
            SendAttempt::Sent(n) => {
                if n < bytes.len() {
                    // Short count: queue the rest.
                    self.buffer.extend_from_slice(&bytes[n..]);
                    WriteResult::Buffered
                } else {
                    WriteResult::Sent
                }
            }
        }
    }

    /// `sendBufferedData()`: push as much queued data as the socket will take.
    pub fn flush(&mut self) -> FlushResult {
        if self.buffer.is_empty() {
            // Nothing pending: a deferred shutdown has nothing left to wait for.
            let shutdown_now = self.shutting_down.replace(false);
            return FlushResult::Drained { shutdown_now };
        }

        match self.sender.send(&self.buffer) {
            SendAttempt::Failed => FlushResult::Failed,
            SendAttempt::WouldBlock => FlushResult::StillBuffered,
            SendAttempt::Sent(n) => {
                if n < self.buffer.len() {
                    // Still data left: advance the cursor and wait until the
                    // socket can take more.
                    self.buffer.drain(..n);
                    FlushResult::StillBuffered
                } else {
                    // All remaining data went out: drop the buffer and the
                    // write registration, then a deferred shutdown proceeds.
                    self.buffer.clear();
                    let shutdown_now = self.shutting_down.replace(false);
                    FlushResult::Drained { shutdown_now }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// A sender handing out a scripted sequence of attempts.
    struct Scripted {
        attempts: VecDeque<SendAttempt>,
    }

    impl Scripted {
        fn new(attempts: &[SendAttempt]) -> Self {
            Self {
                attempts: attempts.iter().copied().collect(),
            }
        }
    }

    impl TcpSender for Scripted {
        fn send(&mut self, data: &[u8]) -> SendAttempt {
            let _ = data;
            self.attempts
                .pop_front()
                .unwrap_or(SendAttempt::Sent(usize::MAX))
        }
    }

    /// A sender that takes the whole of each write and records it, but blocks
    /// the first call. The recorded bytes live in an `Rc` so the test can keep
    /// inspecting them after the sender is boxed.
    struct Recording {
        recorded: Rc<RefCell<Vec<u8>>>,
        block_first: Cell<bool>,
    }

    impl Recording {
        fn new(recorded: Rc<RefCell<Vec<u8>>>) -> Self {
            Self {
                recorded,
                block_first: Cell::new(true),
            }
        }
    }

    impl TcpSender for Recording {
        fn send(&mut self, data: &[u8]) -> SendAttempt {
            if self.block_first.replace(false) {
                return SendAttempt::WouldBlock;
            }
            self.recorded.borrow_mut().extend_from_slice(data);
            SendAttempt::Sent(data.len())
        }
    }

    /// The frame bytes `doSend` would hand over: the length prefix plus the
    /// bundle payload, as one blob.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = (payload.len() as u16).to_le_bytes().to_vec();
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn a_write_the_socket_takes_whole_leaves_nothing_pending() {
        let mut buffer =
            TcpSendBuffer::new(Box::new(Scripted::new(&[SendAttempt::Sent(usize::MAX)])));
        let f = frame(b"first");
        assert_eq!(buffer.write(&f), WriteResult::Sent);
        assert!(!buffer.is_buffering());
        assert_eq!(buffer.pending(), 0);
        assert_eq!(
            buffer.flush(),
            FlushResult::Drained {
                shutdown_now: false
            }
        );
    }

    /// A busy socket queues the whole frame and later drains it whole.
    #[test]
    fn a_busy_socket_queues_the_frame_until_it_can_take_it() {
        let mut buffer = TcpSendBuffer::new(Box::new(Scripted::new(&[SendAttempt::WouldBlock])));
        let f = frame(b"hello world");
        assert_eq!(buffer.write(&f), WriteResult::Buffered);
        assert!(buffer.is_buffering());
        assert_eq!(buffer.pending(), f.len());
        assert_eq!(
            buffer.flush(),
            FlushResult::Drained {
                shutdown_now: false
            }
        );
        assert_eq!(buffer.pending(), 0);
        assert!(!buffer.is_buffering());
    }

    /// A short write queues only the rest, and the queued bytes are exactly
    /// the frame tail.
    #[test]
    fn a_short_write_queues_the_frame_tail() {
        let f = frame(b"short write");
        let mut buffer = TcpSendBuffer::new(Box::new(Scripted::new(&[
            SendAttempt::Sent(5),
            SendAttempt::Sent(f.len() - 5),
        ])));
        assert_eq!(buffer.write(&f), WriteResult::Buffered);
        assert_eq!(buffer.pending(), f.len() - 5, "only the unsent tail queued");
        assert_eq!(
            buffer.flush(),
            FlushResult::Drained {
                shutdown_now: false
            }
        );
    }

    /// Frames written while the socket is busy queue in order, and drain in
    /// the same order: under back-pressure the frame stream is preserved.
    #[test]
    fn frames_queue_in_order_under_back_pressure_and_drain_in_order() {
        let recorded = Rc::new(RefCell::new(Vec::new()));
        let mut buffer = TcpSendBuffer::new(Box::new(Recording::new(Rc::clone(&recorded))));
        let f1 = frame(b"one");
        let f2 = frame(b"two three");
        let f3 = frame(b"four");

        // The first frame hits the busy socket and queues; the next two append
        // behind it, so the buffer holds them in write order.
        assert_eq!(buffer.write(&f1), WriteResult::Buffered);
        assert_eq!(buffer.write(&f2), WriteResult::Buffered);
        assert_eq!(buffer.write(&f3), WriteResult::Buffered);
        assert_eq!(buffer.pending(), f1.len() + f2.len() + f3.len());

        // The socket catches up and takes the whole queue at once, in order.
        assert_eq!(
            buffer.flush(),
            FlushResult::Drained {
                shutdown_now: false
            }
        );
        let mut expected = f1.clone();
        expected.extend_from_slice(&f2);
        expected.extend_from_slice(&f3);
        assert_eq!(*recorded.borrow(), expected, "the frames left in order");
    }

    /// A drain the socket does not finish keeps the rest queued, in order.
    #[test]
    fn a_partial_drain_keeps_the_rest_queued() {
        let mut buffer = TcpSendBuffer::new(Box::new(Scripted::new(&[
            SendAttempt::WouldBlock,
            SendAttempt::Sent(3),
            SendAttempt::Sent(usize::MAX),
        ])));
        let f = frame(b"abcdefgh");
        assert_eq!(buffer.write(&f), WriteResult::Buffered);
        assert_eq!(buffer.pending(), f.len());
        assert_eq!(buffer.flush(), FlushResult::StillBuffered);
        assert_eq!(buffer.pending(), f.len() - 3, "the sent part is gone");
        assert!(buffer.is_buffering());
        assert_eq!(
            buffer.flush(),
            FlushResult::Drained {
                shutdown_now: false
            }
        );
    }

    /// A hard send error surfaces as `Failed`, on the write and on the flush.
    #[test]
    fn a_hard_send_error_is_reported() {
        let mut buffer = TcpSendBuffer::new(Box::new(Scripted::new(&[SendAttempt::Failed])));
        assert_eq!(buffer.write(&frame(b"x")), WriteResult::Failed);

        let mut queued = TcpSendBuffer::new(Box::new(Scripted::new(&[
            SendAttempt::WouldBlock,
            SendAttempt::Failed,
        ])));
        assert_eq!(queued.write(&frame(b"queued")), WriteResult::Buffered);
        assert_eq!(queued.flush(), FlushResult::Failed);
    }

    /// A shutdown waiting on the queue proceeds the moment the buffer drains.
    #[test]
    fn a_deferred_shutdown_proceeds_when_the_buffer_drains() {
        let mut buffer =
            TcpSendBuffer::new(Box::new(Recording::new(Rc::new(RefCell::new(Vec::new())))));
        assert_eq!(buffer.write(&frame(b"pending")), WriteResult::Buffered);
        buffer.begin_shutdown();
        assert_eq!(
            buffer.flush(),
            FlushResult::Drained { shutdown_now: true },
            "the shutdown had been waiting on the data"
        );
        assert_eq!(
            buffer.flush(),
            FlushResult::Drained {
                shutdown_now: false
            },
            "the shutdown already fired"
        );
    }
}
