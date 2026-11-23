//! The BW-02-04-05 mixed-run fixture: the TCP transport components running
//! together in one scenario.
//!
//! The four pieces BW-02-04-01..04 delivered each had their own fixtures;
//! this one drives them all at once, the way a channel would. A client
//! opens with its connection header and a request bundle on one write
//! (`doSend`, `tcp_channel.cpp:418-439`), the server's opening gate admits
//! the header and hands the bundle to the frame assembler, the request is
//! dispatched and answered, the reply travels back over the server's send
//! path under backpressure, and a disconnect tears both channels down with
//! their queues and pending replies cleared together.
//!
//! The third enabled transport, WebSockets, is recorded as blocked rather
//! than fixture-tested: the reference's `WebSocketStreamFilter`
//! (`websocket_stream_filter.cpp`, 1411 lines) plus the handshake handler
//! is a transport of its own, and the raw-TCP opening gate here already
//! rejects the `GET ` handshake the way `handleInput` does
//! (`tcp_channel.cpp:207-229`). A full WebSocket transport is its own
//! port, not a mode of the raw TCP channel; see the todo tick.

#![cfg(test)]

use std::cell::RefCell;
use std::rc::Rc;

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interface_element::{
    InterfaceElement, REPLY_MESSAGE_IDENTIFIER, VARIABLE_LENGTH_MESSAGE,
};
use crate::interface_table::InterfaceTable;
use crate::interfaces::{InputMessageHandler, ReplyMessageHandler};
use crate::misc::{MessageID, ReplyID};
use crate::nub_exception::NubException;
use crate::tcp_bundle::{TcpBundle, TcpFrameAssembler, dispatch_messages};
use crate::tcp_channel_lifecycle::{PeerDisconnectKind, ReadOutcome, TcpChannelLifecycle};
use crate::tcp_connection_header::client_header_frame;
use crate::unpacked_message_header::UnpackedMessageHeader;

const MAX_SEGMENT: usize = 1460;
const REQ_MSG_ID: u8 = 0x44;

fn request_element() -> InterfaceElement {
    InterfaceElement::new("TestReq", REQ_MSG_ID, VARIABLE_LENGTH_MESSAGE, 1, None)
}

fn source() -> Address {
    Address::new(0x7f000001, 2000)
}

fn read_le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

/// A delivery recorder: the request the server's dispatch handed it.
struct Recorder {
    notes: RefCell<Vec<(MessageID, ReplyID, Vec<u8>)>>,
}

impl InputMessageHandler for Recorder {
    fn handle_message(
        &self,
        _source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        let body = data.unread_bytes().to_vec();
        data.finish();
        self.notes
            .borrow_mut()
            .push((header.identifier(), header.reply_id(), body));
    }
}

/// A reply handler stub: the request fixtures never deliver a reply.
struct StubReplyHandler;

impl ReplyMessageHandler for StubReplyHandler {
    fn handle_message(
        &self,
        _source: &Address,
        _header: &UnpackedMessageHeader,
        _data: &mut BinaryStream,
        _arg: usize,
    ) {
    }

    fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
}

fn stub_reply_handler() -> Rc<dyn ReplyMessageHandler> {
    Rc::new(StubReplyHandler)
}

/// A seam that takes at most `budget` bytes and records what it took.
fn budgeted_send(
    budget: usize,
    emitted: &RefCell<Vec<u8>>,
) -> impl FnMut(&[u8]) -> Result<usize, i32> {
    move |data: &[u8]| {
        let take = data.len().min(budget);
        emitted.borrow_mut().extend_from_slice(&data[..take]);
        Ok(take)
    }
}

/// A seam that takes nothing (the socket is closed to writes).
fn blocked_send() -> impl FnMut(&[u8]) -> Result<usize, i32> {
    |_data: &[u8]| Ok(0)
}

/// The mixed run: connection header, frame parsing, the send path and the
/// disconnect cleanup all in one scenario, under backpressure.
#[test]
fn the_tcp_transports_run_together_under_backpressure() {
    // ---- client opens and sends one request bundle on its first write ----
    // `doSend` (tcp_channel.cpp:418-439): the client header pseudo-frame
    // then the bundle frame, back to back in one frameData.
    let mut client_bundle = TcpBundle::new(MAX_SEGMENT);
    client_bundle.start_request(&request_element(), stub_reply_handler(), 0, 0);
    client_bundle.write_bytes(&[0xde, 0xad]);
    client_bundle.finalise();
    let mut client_wire = client_header_frame();
    client_wire.extend_from_slice(&client_bundle.frame_encode());

    let mut client = TcpChannelLifecycle::new();
    let sent = RefCell::new(Vec::new());

    // Backpressure: the first write only half leaves.
    {
        let mut send = budgeted_send(5, &sent);
        assert!(client.write_from(&client_wire, &mut send));
    }
    assert!(client.has_unsent_data(), "the rest is queued");

    {
        let mut send = budgeted_send(usize::MAX, &sent);
        assert!(client.send_buffered_data(&mut send));
    }
    assert!(!client.has_unsent_data(), "the whole write has left");
    assert_eq!(
        *sent.borrow(),
        client_wire,
        "the header and frame left in order"
    );
    client.note_request_started();
    assert_eq!(client.pending_reply_count(), 1);

    // ---- server admits the header and dispatches the request ----
    let server_table = InterfaceTable::new();
    let recorder = Rc::new(Recorder {
        notes: RefCell::new(Vec::new()),
    });
    server_table.serve(
        &request_element(),
        Some(Rc::clone(&recorder) as Rc<dyn InputMessageHandler>),
    );

    let mut server = TcpChannelLifecycle::new();
    let mut opening = crate::tcp_connection_header::ServerOpening::new();
    // The gate takes the header (and hands back the bundle frame), the way
    // `HEADER_STATE_SERVER_WAIT_FOR_HEADER` runs once then falls through.
    let decision = opening.filter(&sent.borrow());
    use crate::tcp_connection_header::HeaderDecision;
    assert_eq!(decision, HeaderDecision::Accept);
    let leftover = opening.take_leftover_frames();
    assert_eq!(leftover.len(), 1, "the request bundle follows the header");

    let reason = dispatch_messages(&server_table, &source(), &leftover[0]);
    assert_eq!(reason, crate::misc::Reason::Success);
    let notes = recorder.notes.borrow();
    assert_eq!(notes.len(), 1);
    let (id, reply_id, body) = notes[0].clone();
    assert_eq!(id, REQ_MSG_ID);
    assert_eq!(reply_id, 0x1234_5678 as ReplyID, "the placeholder id");
    assert_eq!(body, vec![0xde, 0xad]);

    // ---- server answers; the reply is queued under backpressure ----
    let mut reply_bundle = TcpBundle::new(MAX_SEGMENT);
    reply_bundle.start_reply(reply_id);
    reply_bundle.finalise();
    let reply_wire = reply_bundle.frame_encode();

    {
        let mut send = blocked_send();
        assert!(server.write_from(&reply_wire, &mut send));
    }
    assert!(server.has_unsent_data(), "the reply waits for the socket");

    let replied = RefCell::new(Vec::new());
    {
        let mut send = budgeted_send(usize::MAX, &replied);
        assert!(server.send_buffered_data(&mut send));
    }
    assert!(!server.has_unsent_data());
    assert_eq!(*replied.borrow(), reply_wire);

    // ---- client assembler decodes the reply, resolving its request ----
    let mut client_assembler = TcpFrameAssembler::new();
    let frames = client_assembler.push(&replied.borrow());
    assert_eq!(frames.len(), 1);
    let reply_frame = &frames[0];
    assert_eq!(reply_frame[0] & crate::tcp_bundle::FLAG_HAS_REQUESTS, 0);
    assert_eq!(reply_frame[1], REPLY_MESSAGE_IDENTIFIER);
    assert_eq!(read_le32(&reply_frame[2..6]), 4);
    assert_eq!(read_le32(&reply_frame[6..10]), 0x1234_5678);
    client.note_reply_arrived();
    assert_eq!(client.pending_reply_count(), 0);

    // ---- the peer disconnects; both channels tear down consistently ----
    let outcome = client.note_received(0, 1000);
    assert_eq!(
        outcome,
        ReadOutcome::PeerDisconnected(PeerDisconnectKind::PeerDisconnect)
    );
    assert!(!client.is_connected());
    assert_eq!(client.pending_reply_count(), 0);

    let outcome = server.note_received(0, 1000);
    assert_eq!(
        outcome,
        ReadOutcome::PeerDisconnected(PeerDisconnectKind::PeerDisconnect)
    );
    assert!(!server.is_connected());
    assert!(!server.has_unsent_data());
    assert_eq!(server.pending_reply_count(), 0);
}
