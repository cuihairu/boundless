//! `TCPChannel::writeFrom` / `basicSend` / `sendBufferedData`
//! (`tcp_channel.cpp:707-858`) -- the send side's short write and the
//! buffer that holds what the socket would not take (BW-02-04-02).
//!
//! A non-blocking TCP socket accepts only part of a frame. The reference
//! hands whatever was left over to a send buffer, registers the socket for
//! write events, and `sendBufferedData` drains the buffer when the socket
//! is writable again. Everything the send path wants on the wire leaves in
//! order: what could not be sent is queued behind what is still waiting,
//! and a new frame's bytes never overtake them.
//!
//! The socket's `send` is a seam rather than a live `Endpoint`, so the
//! buffer is testable without a socket; the cork flag the reference's
//! `basicSend` carries is folded into the seam (a `shouldCork` send is
//! `TCP_CORK` on unix and a deferred `send` otherwise).

use platform::socket;

/// `isErrnoIgnorable` (`network_utils.cpp:25-33`): the errno that means
/// "try again later" rather than "the connection is broken".
pub fn is_errno_ignorable(errno: i32) -> bool {
    errno == socket::EAGAIN
        || errno == socket::EWOULDBLOCK
        || errno == socket::EINPROGRESS
        || errno == socket::EINTR
}

/// The send call a real socket backs: the bytes to send, and either how
/// many the socket accepted or the errno it failed with.
pub type SendFn<'a> = dyn FnMut(&[u8]) -> Result<usize, i32> + 'a;

/// The pending bytes a TCP channel could not send, the reference's
/// `pSendBuffer_`.
///
/// The reference also keeps the count of sent data units and bytes on the
/// channel; those counters belong to the channel's `doSend` and stay with
/// it (BW-02-04-05). This holds only what has not left.
#[derive(Default)]
pub struct TcpSendBuffer {
    /// `pSendBuffer_`: the bytes not yet accepted, oldest first.
    pending: Vec<u8>,
}

impl TcpSendBuffer {
    /// The empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether anything is waiting for the socket to open.
    pub fn has_unsent_data(&self) -> bool {
        !self.pending.is_empty()
    }

    /// How many bytes are waiting.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// `writeFrom` (`tcp_channel.cpp:707-765`): hand one frame's bytes to
    /// the socket, queuing whatever it would not take. `input` is
    /// consumed either way -- the caller's frame has left the channel's
    /// hands, whether the socket took it now or later.
    ///
    /// Returns false only on a hard error, when the channel must be
    /// destroyed; a short write or an ignorable errno is not an error.
    pub fn write_from(&mut self, input: &[u8], send: &mut SendFn<'_>) -> bool {
        if !self.pending.is_empty() {
            // `if (pSendBuffer_)`: already waiting on the socket, add this
            // to the end and do not try to send now.
            self.pending.extend_from_slice(input);
            return true;
        }

        let sent = match self.basic_send(input, send) {
            Ok(sent) => sent,
            Err(errno) => {
                eprintln!(
                    "TCPChannel::writeFrom: write error, destroying; errno: {}",
                    errno
                );
                return false;
            }
        };

        if sent < input.len() {
            // Got short count when sending. Queue the rest; registering the
            // socket for write events is the channel's act.
            self.pending.extend_from_slice(&input[sent..]);
        }

        true
    }

    /// `sendBufferedData` (`tcp_channel.cpp:817-855`): send as much of the
    /// buffer as the socket will take, returning false only when a hard
    /// error means the channel is gone.
    ///
    /// True covers every other outcome: nothing moved, some moved, or all
    /// moved. When the buffer empties, the caller stops waiting for write
    /// events (and, in the reference, runs a deferred shutdown).
    pub fn send_buffered_data(&mut self, send: &mut SendFn<'_>) -> bool {
        if self.pending.is_empty() {
            return true;
        }

        let sent = match self.basic_send(&self.pending, send) {
            Ok(sent) => sent,
            Err(errno) => {
                eprintln!(
                    "TCPChannel::sendBufferedData: send failed, disconnecting; errno: {}",
                    errno
                );
                return false;
            }
        };

        if sent == 0 {
            // Still no room; wait for the next write event.
            return true;
        }

        if sent < self.pending.len() {
            // Still data left, advance the cursor and wait again.
            self.pending.drain(..sent);
        } else {
            self.pending.clear();
        }

        true
    }

    /// `basicSend` (`tcp_channel.cpp:780-816`): the socket send, with an
    /// ignorable errno flattened to "nothing was sent" the way the
    /// reference flattens `-1` to `0`.
    fn basic_send(&self, data: &[u8], send: &mut SendFn<'_>) -> Result<usize, i32> {
        match send(data) {
            Err(errno) if is_errno_ignorable(errno) => Ok(0),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// The reference's cork emulation on a platform without `TCP_CORK`:
    /// `basicSend` returns 0 and the caller defers the send.
    #[test]
    fn a_full_send_leaves_nothing_pending() {
        let mut buffer = TcpSendBuffer::new();
        let emitted = RefCell::new(Vec::new());
        let mut send = |data: &[u8]| {
            emitted.borrow_mut().extend_from_slice(data);
            Ok(data.len())
        };

        assert!(buffer.write_from(&[1, 2, 3], &mut send));
        assert!(!buffer.has_unsent_data());
        assert_eq!(*emitted.borrow(), vec![1, 2, 3]);
    }

    /// A short write queues the remainder and a following write queues
    /// behind it, and the buffer drains in the order it was sent in.
    #[test]
    fn a_short_write_queues_and_drains_in_send_order() {
        let mut buffer = TcpSendBuffer::new();
        let emitted = RefCell::new(Vec::new());
        let budget = Cell::new(3usize);
        let mut send = |data: &[u8]| {
            let take = data.len().min(budget.get());
            emitted.borrow_mut().extend_from_slice(&data[..take]);
            Ok(take)
        };

        // First frame (6 bytes) hits a budget of 3: half queued.
        assert!(buffer.write_from(&[1, 2, 3, 4, 5, 6], &mut send));
        assert_eq!(buffer.pending_len(), 3);
        assert!(buffer.has_unsent_data());

        // Second frame (4 bytes) queues behind the first's remainder; the
        // budget is spent, so nothing of it leaves now.
        assert!(buffer.write_from(&[7, 8, 9, 10], &mut send));
        assert_eq!(buffer.pending_len(), 7);

        // The socket opens: the whole queue drains in send order.
        budget.set(usize::MAX);
        assert!(buffer.send_buffered_data(&mut send));
        assert!(!buffer.has_unsent_data());
        assert_eq!(*emitted.borrow(), vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    /// A write event that still only moves part of the queue keeps waiting
    /// with the rest.
    #[test]
    fn a_partial_drain_keeps_the_remainder() {
        let mut buffer = TcpSendBuffer::new();
        let emitted = RefCell::new(Vec::new());
        let budget = Cell::new(2usize);
        let mut send = |data: &[u8]| {
            let take = data.len().min(budget.get());
            emitted.borrow_mut().extend_from_slice(&data[..take]);
            Ok(take)
        };

        assert!(buffer.write_from(&[1, 2, 3, 4, 5, 6], &mut send));
        assert_eq!(buffer.pending_len(), 4);

        assert!(buffer.send_buffered_data(&mut send));
        assert_eq!(buffer.pending_len(), 2, "two bytes still wait");
        assert!(buffer.has_unsent_data());

        budget.set(usize::MAX);
        assert!(buffer.send_buffered_data(&mut send));
        assert!(!buffer.has_unsent_data());
        assert_eq!(*emitted.borrow(), vec![1, 2, 3, 4, 5, 6]);
    }

    /// A hard errno consumes the input and reports failure, the channel
    /// being destroyed; nothing is queued.
    #[test]
    fn a_hard_errno_reports_failure_and_consumes_the_input() {
        let mut buffer = TcpSendBuffer::new();
        let mut send = |_data: &[u8]| Err(socket::ECONNREFUSED);

        assert!(!buffer.write_from(&[1, 2, 3], &mut send));
        assert!(!buffer.has_unsent_data());
    }

    /// `sendBufferedData` reports a hard error the same way.
    #[test]
    fn a_hard_errno_during_a_drain_reports_the_disconnect() {
        let mut buffer = TcpSendBuffer::new();
        let mut send = |_data: &[u8]| Ok(0);
        assert!(buffer.write_from(&[1, 2, 3], &mut send));

        let mut send = |_data: &[u8]| Err(socket::ECONNREFUSED);
        assert!(!buffer.send_buffered_data(&mut send));
        assert!(buffer.has_unsent_data(), "the buffer keeps what it held");
    }

    /// An ignorable errno is not an error: nothing moved, everything
    /// waits.
    #[test]
    fn an_ignorable_errno_defers_the_send() {
        let mut buffer = TcpSendBuffer::new();
        let mut send = |_data: &[u8]| Err(socket::EAGAIN);

        assert!(buffer.write_from(&[1, 2, 3], &mut send));
        assert_eq!(buffer.pending_len(), 3);
    }

    /// The retry set is exactly the reference's: the would-block trio and
    /// the interrupted call, and not the real errors.
    #[test]
    fn the_retry_set_is_the_reference_list() {
        assert!(is_errno_ignorable(socket::EAGAIN));
        assert!(is_errno_ignorable(socket::EWOULDBLOCK));
        assert!(is_errno_ignorable(socket::EINPROGRESS));
        assert!(is_errno_ignorable(socket::EINTR));
        assert!(!is_errno_ignorable(socket::ECONNREFUSED));
        assert!(!is_errno_ignorable(socket::ENOBUFS));
    }

    /// The platform's cork emulation (no `TCP_CORK`): `basicSend` answers
    /// 0 and the whole frame waits, then leaves on the next drain.
    #[test]
    fn cork_emulation_defers_the_whole_send() {
        let mut buffer = TcpSendBuffer::new();
        let emitted = RefCell::new(Vec::new());
        // The emulation sends nothing, so it leaves nothing behind on the
        // wire: the frame is only logged once, at the drain.
        let mut send = |_data: &[u8]| Ok(0);

        assert!(buffer.write_from(&[5, 6, 7], &mut send));
        assert_eq!(buffer.pending_len(), 3);

        let mut send = |data: &[u8]| {
            emitted.borrow_mut().extend_from_slice(data);
            Ok(data.len())
        };
        assert!(buffer.send_buffered_data(&mut send));
        assert!(!buffer.has_unsent_data());
        assert_eq!(*emitted.borrow(), vec![5, 6, 7]);
    }

    /// The acceptance for BW-02-04-02: two framed bundles leave the send
    /// path in order under backpressure -- a first frame that only half
    /// sends, then a second that queues whole, then the drain.
    #[test]
    fn frames_leave_in_order_under_backpressure() {
        use crate::interface_element::InterfaceElement;
        use crate::interface_element::VARIABLE_LENGTH_MESSAGE;
        use crate::tcp_bundle::TcpBundle;

        fn bundle(payload: &[u8]) -> Vec<u8> {
            let element = InterfaceElement::new("m", 0x22, VARIABLE_LENGTH_MESSAGE, 1, None);
            let mut b = TcpBundle::new(1460);
            b.start_message(&element);
            b.write_bytes(payload);
            b.finalise();
            b.frame_encode()
        }

        let first = bundle(b"first");
        let second = bundle(b"second");

        let mut buffer = TcpSendBuffer::new();
        let emitted = RefCell::new(Vec::new());
        let budget = Cell::new(5usize);
        let mut send = |data: &[u8]| {
            let take = data.len().min(budget.get());
            emitted.borrow_mut().extend_from_slice(&data[..take]);
            Ok(take)
        };

        assert!(buffer.write_from(&first, &mut send));
        assert!(buffer.write_from(&second, &mut send));

        budget.set(usize::MAX);
        assert!(buffer.send_buffered_data(&mut send));

        let mut expected = first.clone();
        expected.extend_from_slice(&second);
        assert_eq!(*emitted.borrow(), expected, "the frames left in send order");
        assert!(!buffer.has_unsent_data());
    }
}
