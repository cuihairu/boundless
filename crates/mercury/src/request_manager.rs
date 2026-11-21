//! Request correlation and reply dispatch
//! (`lib/network/request_manager.{hpp,cpp}`, `request.{hpp,cpp}`,
//! `reply_order.hpp`).
//!
//! BW-02-02-03: a request that expects a reply carries a `ReplyID`; the
//! `RequestManager` keeps a pending map from that id to the caller's
//! handler, and a reply (whose message id is `REPLY_MESSAGE_IDENTIFIER`
//! and whose body starts with the reply id) is routed back to the one
//! request that registered it. Interleaved requests therefore each receive
//! their own reply, whatever order the replies arrive in.
//!
//! The reply body is `[reply_id u32][reply payload]`, the reply id
//! little-endian like every other Mercury wire integer. The id space wraps
//! at `REPLY_ID_MAX` back to 1 (0 is never handed out).

use crate::interface_element::InterfaceElement;
use crate::message_frame::{FrameError, body_from_frame};
use crate::misc::{MessageId, Reason};

/// `Mercury::ReplyID` (`lib/network/misc.hpp`): the identifier a request
/// embeds so its reply can be matched back to it.
pub type ReplyId = u32;

/// `Mercury::REPLY_ID_MAX` (`lib/network/misc.hpp:112`): the allocator
/// wraps back to 1 once the id passes this value.
pub const REPLY_ID_MAX: ReplyId = 1_000_000;

/// The message id of a reply (`REPLY_MESSAGE_IDENTIFIER`), which the
/// `RequestManager` is served under in the interface table.
pub const REPLY_MESSAGE_ID: MessageId = 0xFF;

/// The user-facing handler for a pending request (`ReplyMessageHandler`).
///
/// The reference passes the source address, an unpacked header and the
/// request's `void*` arg as well; the port passes just the reply payload,
/// since the source check belongs to the channel (BW-02-03) and the arg is
/// the handler object's own state.
pub trait ReplyMessageHandler {
    /// A reply arrived for this request; `payload` is the reply body after
    /// the reply id.
    fn on_reply(&mut self, payload: &[u8]);

    /// The request failed -- the reply timed out, the channel was lost, or
    /// the interface is shutting down (`Request::handleFailure`).
    fn on_exception(&mut self, reason: Reason);
}

/// A registered pending request and the handler that owns it.
struct PendingRequest {
    handler: Box<dyn ReplyMessageHandler>,
}

/// Whether [`RequestManager::dispatch_reply`] routed the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// The reply was routed to its request's handler.
    Dispatched,
    /// No pending request held the id (it timed out or was never made); the
    /// reference logs "Couldn't find handler for reply id" and drops it.
    UnknownReply,
    /// The reply id was not written at the start of the body.
    CorruptedReply,
}

/// `RequestManager`: allocates reply ids, keeps the pending request map and
/// routes replies back to the request that asked for them.
pub struct RequestManager {
    requests: std::collections::BTreeMap<ReplyId, PendingRequest>,
    next_reply_id: ReplyId,
}

impl RequestManager {
    /// `RequestManager::RequestManager`: the allocator is seeded like the
    /// reference's `( timestamp() % 100000 ) + 10101`, but the seed is
    /// taken here so tests can pin the sequence.
    #[must_use]
    pub const fn new(seed: ReplyId) -> Self {
        Self {
            requests: std::collections::BTreeMap::new(),
            next_reply_id: seed,
        }
    }

    /// `getNextReplyID`: the next id in sequence, wrapping to 1 once the id
    /// passes `REPLY_ID_MAX` (so 0 is never handed out).
    #[must_use]
    pub fn get_next_reply_id(&mut self) -> ReplyId {
        if self.next_reply_id > REPLY_ID_MAX {
            self.next_reply_id = 1;
        }
        let id = self.next_reply_id;
        self.next_reply_id += 1;
        id
    }

    /// `addReplyOrder`: register a pending request and return the reply id
    /// it will be matched on. The caller writes that id into the request
    /// frame's reply-id slot.
    pub fn add_request(&mut self, handler: Box<dyn ReplyMessageHandler>) -> ReplyId {
        let reply_id = self.get_next_reply_id();
        self.requests.insert(reply_id, PendingRequest { handler });
        reply_id
    }

    /// How many requests are pending.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.requests.len()
    }

    /// `RequestManager::handleMessage` after the reply id is read: route a
    /// reply to the pending request that registered `reply_id`, removing it
    /// from the map so a second reply for the same id is dropped as unknown.
    pub fn dispatch_reply(&mut self, reply_id: ReplyId, payload: &[u8]) -> DispatchOutcome {
        let Some(request) = self.requests.remove(&reply_id) else {
            return DispatchOutcome::UnknownReply;
        };

        // The request is out of the map before the handler runs, so the
        // handler may register or fail further requests freely.
        let mut handler = request.handler;
        handler.on_reply(payload);
        DispatchOutcome::Dispatched
    }

    /// The wire-level routing: a reply frame is
    /// `[REPLY_MESSAGE_ID][len][reply_id u32 little-endian][payload]`, so
    /// the reply id is read off the body and the rest is handed to the
    /// matching request.
    pub fn dispatch_reply_frame(&mut self, frame: &[u8]) -> Result<DispatchOutcome, FrameError> {
        let body = body_from_frame(&InterfaceElement::reply(), frame)?;
        if body.len() < size_of::<ReplyId>() {
            return Ok(DispatchOutcome::CorruptedReply);
        }
        let reply_id = ReplyId::from_le_bytes(
            body[..size_of::<ReplyId>()]
                .try_into()
                .expect("length checked above"),
        );
        Ok(self.dispatch_reply(reply_id, &body[size_of::<ReplyId>()..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A handler that records what it is told into shared sinks, so a test
    /// can keep inspecting a handler after it has been boxed and moved into
    /// the manager.
    struct RecordingHandler {
        replies: Rc<RefCell<Vec<Vec<u8>>>>,
        exceptions: Rc<RefCell<Vec<Reason>>>,
    }

    impl RecordingHandler {
        fn new(replies: &Rc<RefCell<Vec<Vec<u8>>>>) -> Self {
            Self {
                replies: Rc::clone(replies),
                exceptions: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl ReplyMessageHandler for RecordingHandler {
        fn on_reply(&mut self, payload: &[u8]) {
            self.replies.borrow_mut().push(payload.to_vec());
        }

        fn on_exception(&mut self, reason: Reason) {
            self.exceptions.borrow_mut().push(reason);
        }
    }

    #[test]
    fn the_allocator_wraps_at_reply_id_max() {
        let mut manager = RequestManager::new(REPLY_ID_MAX - 1);

        // The last two ids before the wrap are handed out...
        assert_eq!(manager.get_next_reply_id(), REPLY_ID_MAX - 1);
        assert_eq!(manager.get_next_reply_id(), REPLY_ID_MAX);
        // ...then the sequence wraps to 1 (0 is never used).
        assert_eq!(manager.get_next_reply_id(), 1);
        assert_eq!(manager.get_next_reply_id(), 2);
    }

    #[test]
    fn interleaved_requests_each_receive_their_own_reply() {
        let mut manager = RequestManager::new(10101);
        let sinks: Vec<Rc<RefCell<Vec<Vec<u8>>>>> =
            (0..3).map(|_| Rc::new(RefCell::new(Vec::new()))).collect();

        // Three requests, each with its own recording sink. They are added
        // in a row, so the reply ids are consecutive.
        let mut ids = Vec::new();
        for sink in &sinks {
            ids.push(manager.add_request(Box::new(RecordingHandler::new(sink))));
        }
        assert_eq!(manager.pending_count(), 3);
        assert_eq!(ids, [10101, 10102, 10103]);

        // Replies arrive out of order, each with its own id and payload.
        let mut reply_to = |id: ReplyId, payload: &[u8]| {
            assert_eq!(
                manager.dispatch_reply(id, payload),
                DispatchOutcome::Dispatched
            );
        };
        reply_to(10103, &[0x33, 0x33]);
        reply_to(10101, &[0x11, 0x11]);
        reply_to(10102, &[0x22, 0x22]);
        assert_eq!(manager.pending_count(), 0);

        // Each handler received exactly the reply addressed to it.
        assert_eq!(*sinks[0].borrow(), vec![vec![0x11, 0x11]]);
        assert_eq!(*sinks[1].borrow(), vec![vec![0x22, 0x22]]);
        assert_eq!(*sinks[2].borrow(), vec![vec![0x33, 0x33]]);
    }

    #[test]
    fn a_second_reply_for_the_same_id_is_unknown() {
        let mut manager = RequestManager::new(10101);
        let sink = Rc::new(RefCell::new(Vec::new()));
        let id = manager.add_request(Box::new(RecordingHandler::new(&sink)));

        assert_eq!(
            manager.dispatch_reply(id, &[1, 2, 3]),
            DispatchOutcome::Dispatched
        );
        assert_eq!(
            manager.dispatch_reply(id, &[4, 5, 6]),
            DispatchOutcome::UnknownReply
        );
        assert_eq!(sink.borrow().len(), 1);
    }

    #[test]
    fn an_unknown_reply_is_dropped_without_an_exception() {
        let mut manager = RequestManager::new(10101);
        assert_eq!(
            manager.dispatch_reply(424242, &[1, 2]),
            DispatchOutcome::UnknownReply
        );
    }

    #[test]
    fn a_reply_frame_is_routed_by_its_wire_reply_id() {
        let mut manager = RequestManager::new(10101);
        let sink = Rc::new(RefCell::new(Vec::new()));
        let id = manager.add_request(Box::new(RecordingHandler::new(&sink)));

        // Build the reply frame the way the reference's startReply does:
        // [0xFF][len][reply_id LE][payload].
        let reply_element = InterfaceElement::reply();
        let mut body = id.to_le_bytes().to_vec();
        body.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let frame = crate::message_frame::encode_frame(&reply_element, &body).unwrap();

        assert_eq!(
            manager.dispatch_reply_frame(&frame).unwrap(),
            DispatchOutcome::Dispatched
        );
        assert_eq!(*sink.borrow(), vec![vec![0xDE, 0xAD, 0xBE, 0xEF]]);
    }

    #[test]
    fn a_reply_frame_with_a_short_body_is_corrupted() {
        let mut manager = RequestManager::new(10101);
        let reply_element = InterfaceElement::reply();
        // Two bytes, short of the four-byte reply id.
        let body = [0x00, 0x00];
        let frame = crate::message_frame::encode_frame(&reply_element, &body).unwrap();
        assert_eq!(
            manager.dispatch_reply_frame(&frame).unwrap(),
            DispatchOutcome::CorruptedReply
        );
    }

    #[test]
    fn an_expired_request_is_unknown_when_its_reply_arrives() {
        let mut manager = RequestManager::new(10101);
        let sink = Rc::new(RefCell::new(Vec::new()));
        let id = manager.add_request(Box::new(RecordingHandler::new(&sink)));

        // The request is failed (e.g. timed out) and drops out of the map.
        manager.dispatch_reply(id, &[1]);
        // The late reply for the same id finds nothing.
        assert_eq!(
            manager.dispatch_reply(id, &[2]),
            DispatchOutcome::UnknownReply
        );
    }
}
