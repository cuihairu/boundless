//! The TCP channel's send side: the short-write and pending-send buffer
//! `TCPChannel::writeFrom`/`sendBufferedData` run
//! (`lib/network/tcp_channel.cpp:707-858`).
//!
//! A TCP socket accepts bytes lazily: a `send` may take fewer bytes than it
//! was given (a short write) or none at all (the socket is full). The channel
//! turns that into a buffer: whatever the socket refuses is kept in order, and
//! a write-event notification drains it as the socket frees up. The frame
//! bytes the channel hands in therefore come out complete and in order no
//! matter how the socket's capacity fluctuates.
//!
//! The port models the socket as a [`SendSink`], so the buffer is testable
//! with a sink that caps its own throughput. The channel's own socket wiring
//! (the write-fd registration, the `SendWaiter`) joins with the channel build
//! of BW-02-04-03/04.

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
