//! `TCPChannel::readHeader` and the `HEADER_STATE_*` opening handshake
//! (`tcp_channel.cpp:23-30`, `276-315`, `418-427`) -- the connection
//! filter that tells a raw TCP connection apart from a WebSockets one
//! (BW-02-04-03).
//!
//! The client's first frame is its connection header, `CLIENT_HEADER`. A
//! server waiting for it allows the connection only when that first frame
//! matches, rejects a WebSockets handshake (`GET `) outright, and drops
//! anything else. Once the header is in, the connection is ordinary and
//! the frame assembler takes over; this gate guards only the opening.

use crate::tcp_bundle::TcpFrameAssembler;

/// `CLIENT_HEADER` (`tcp_channel.cpp:29`): the client's opening frame
/// body, the one thing the server's first-frame filter accepts.
pub const CLIENT_HEADER: &[u8] = b"TCP\r\n\r\n";

/// `CLIENT_HEADER_SIZE` (`tcp_channel.cpp:30`).
pub const CLIENT_HEADER_SIZE: usize = CLIENT_HEADER.len();

/// `HTTP_HEADER_START` (`tcp_channel.cpp:207-208`): the WebSockets
/// handshake line, rejected because raw TCP does not begin with it.
const HTTP_HEADER_START: &[u8] = b"GET ";

/// What the opening filter decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderDecision {
    /// Not enough of the first frame has arrived to tell.
    NeedMore,
    /// The first frame is the connection header; the connection proceeds.
    Accept,
    /// The bytes are a WebSockets handshake, not a raw TCP connection.
    RejectWebSocket,
    /// The first frame is not the connection header; drop the connection.
    RejectInvalidHeader,
}

/// The server's opening gate: it consumes the bytes of the client's first
/// frame and answers [`HeaderDecision`] for the connection.
///
/// The reference drives this inside `handleInput` under
/// `HEADER_STATE_SERVER_WAIT_FOR_HEADER`: the `GET ` sniff runs on the raw
/// bytes before the frame length is read, and once a frame completes
/// `readHeader` compares it to `CLIENT_HEADER`. This holds both steps.
pub struct ServerOpening {
    /// The frame under construction, the header being one frame.
    assembler: TcpFrameAssembler,
    /// The raw bytes so far, for the `GET ` sniff.
    buffered: Vec<u8>,
    /// The decision once made; later feeds answer it again.
    decision: Option<HeaderDecision>,
    /// Frames that finished alongside the header: this gate does not judge
    /// them, the channel does.
    leftover: Vec<Vec<u8>>,
}

impl ServerOpening {
    /// The gate waiting for the client's header.
    pub fn new() -> Self {
        ServerOpening {
            assembler: TcpFrameAssembler::new(),
            buffered: Vec::new(),
            decision: None,
            leftover: Vec::new(),
        }
    }

    /// Feed the bytes a socket read returned and get the decision. The
    /// first call that stops returning [`HeaderDecision::NeedMore`] is
    /// final.
    pub fn filter(&mut self, bytes: &[u8]) -> HeaderDecision {
        if let Some(decision) = self.decision {
            return decision;
        }

        self.buffered.extend_from_slice(bytes);

        // `handleInput` (tcp_channel.cpp:207-229): before reading a frame
        // length, a raw-TCP connection's first bytes can never be a
        // WebSockets handshake line.
        if self.buffered.len() >= HTTP_HEADER_START.len()
            && &self.buffered[..HTTP_HEADER_START.len()] == HTTP_HEADER_START
        {
            self.decision = Some(HeaderDecision::RejectWebSocket);
            return HeaderDecision::RejectWebSocket;
        }

        let frames = self.assembler.push(bytes);
        match frames.first() {
            None => HeaderDecision::NeedMore,
            Some(first) => {
                // The frames after the header are not this gate's to judge;
                // the channel consumes them (cleanUpAfterFrameReceive).
                self.leftover.extend_from_slice(&frames[1..]);
                // `readHeader` (tcp_channel.cpp:276-315): the first frame
                // must be exactly the connection header.
                let decision = if first == CLIENT_HEADER {
                    HeaderDecision::Accept
                } else {
                    HeaderDecision::RejectInvalidHeader
                };
                self.decision = Some(decision);
                decision
            }
        }
    }

    /// The frames that completed alongside the header, if the bytes fed in
    /// carried more than the header frame. The channel hands these to its
    /// own assembler.
    pub fn take_leftover_frames(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.leftover)
    }
}

impl Default for ServerOpening {
    fn default() -> Self {
        Self::new()
    }
}

/// The client's opening frame, the first thing a client sends: the
/// two-byte length of the header and the header itself (`doSend`,
/// `tcp_channel.cpp:418-427`).
pub fn client_header_frame() -> Vec<u8> {
    let mut frame = Vec::with_capacity(2 + CLIENT_HEADER_SIZE);
    frame.extend_from_slice(&(CLIENT_HEADER_SIZE as u16).to_le_bytes());
    frame.extend_from_slice(CLIENT_HEADER);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_sends_the_header_as_its_first_frame() {
        let mut expected = vec![CLIENT_HEADER_SIZE as u8, 0x00];
        expected.extend_from_slice(b"TCP\r\n\r\n");
        assert_eq!(client_header_frame(), expected);
    }

    /// A header that arrives whole is allowed.
    #[test]
    fn a_correct_header_is_allowed() {
        let mut opening = ServerOpening::new();
        assert_eq!(
            opening.filter(&client_header_frame()),
            HeaderDecision::Accept
        );
    }

    /// A header arriving one byte at a time is still allowed.
    #[test]
    fn a_header_that_arrives_a_byte_at_a_time_is_allowed() {
        let mut opening = ServerOpening::new();
        let frame = client_header_frame();
        for byte in &frame[..frame.len() - 1] {
            assert_eq!(
                opening.filter(std::slice::from_ref(byte)),
                HeaderDecision::NeedMore
            );
        }
        assert_eq!(
            opening.filter(&frame[frame.len() - 1..]),
            HeaderDecision::Accept
        );
    }

    /// The `GET ` WebSockets handshake is rejected as soon as its fourth
    /// byte is in.
    #[test]
    fn a_websocket_handshake_is_rejected() {
        let mut opening = ServerOpening::new();
        assert_eq!(opening.filter(b"GET "), HeaderDecision::RejectWebSocket);
    }

    /// The default gate behaves like the constructed one.
    #[test]
    fn a_default_gate_waits_like_a_new_one() {
        let mut opening = ServerOpening::default();
        assert_eq!(
            opening.filter(&[CLIENT_HEADER_SIZE as u8, 0x00]),
            HeaderDecision::NeedMore
        );
    }

    /// A WebSockets handshake arriving split is rejected once the fourth
    /// byte completes the prefix.
    #[test]
    fn a_split_websocket_prefix_is_rejected_on_the_fourth_byte() {
        let mut opening = ServerOpening::new();
        assert_eq!(opening.filter(b"GE"), HeaderDecision::NeedMore);
        assert_eq!(opening.filter(b"T "), HeaderDecision::RejectWebSocket);
    }

    /// A frame whose body is not the connection header is rejected.
    #[test]
    fn a_wrong_first_frame_is_rejected() {
        let mut opening = ServerOpening::new();
        let mut frame = vec![8u8, 0x00];
        frame.extend_from_slice(b"HTTP/1.1");
        assert_eq!(opening.filter(&frame), HeaderDecision::RejectInvalidHeader);
    }

    /// A first frame of the wrong length is rejected too.
    #[test]
    fn a_short_first_frame_is_rejected() {
        let mut opening = ServerOpening::new();
        let frame = [3u8, 0x00, b'T', b'C', b'P'];
        assert_eq!(opening.filter(&frame), HeaderDecision::RejectInvalidHeader);
    }

    /// A partial header waits; the rest of the length prefix and the body
    /// are not enough until the frame is complete.
    #[test]
    fn a_partial_header_waits_for_more() {
        let mut opening = ServerOpening::new();
        let len = [CLIENT_HEADER_SIZE as u8, 0x00];
        assert_eq!(opening.filter(&len), HeaderDecision::NeedMore);
        assert_eq!(opening.filter(b"TCP\r\n"), HeaderDecision::NeedMore);
        assert_eq!(opening.filter(b"\r\n"), HeaderDecision::Accept);
    }

    /// A header followed by a real frame: only the header is judged, the
    /// frame after it comes back for the channel's assembler.
    #[test]
    fn a_header_with_more_after_it_is_still_allowed() {
        let mut opening = ServerOpening::new();
        let mut bytes = client_header_frame();
        bytes.extend_from_slice(&[3, 0, 1, 2, 3]);
        assert_eq!(opening.filter(&bytes), HeaderDecision::Accept);
        assert_eq!(opening.take_leftover_frames(), vec![vec![1, 2, 3]]);
    }

    /// After the decision the gate stays as it was; the channel owns the
    /// next reads.
    #[test]
    fn a_decided_gate_answers_the_same_decision() {
        let mut opening = ServerOpening::new();
        assert_eq!(opening.filter(b"GET "), HeaderDecision::RejectWebSocket);
        assert_eq!(opening.filter(b"more"), HeaderDecision::RejectWebSocket);

        let mut accepting = ServerOpening::new();
        assert_eq!(
            accepting.filter(&client_header_frame()),
            HeaderDecision::Accept
        );
        assert_eq!(accepting.filter(&[1, 2, 3]), HeaderDecision::Accept);
    }
}
