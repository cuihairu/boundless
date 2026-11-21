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
//!
//! BW-02-02-04: a pending request also carries a deadline (`now + timeout`,
//! the reference's once-off timer register in `Request::Request`). A request
//! is failed exactly once -- timeout, caller cancel, or teardown -- which
//! invokes `on_exception` and releases the pending state; a reply arriving
//! after that is dropped as unknown.

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

/// `DEFAULT_REQUEST_TIMEOUT` (`lib/network/bundle.hpp:25`), in microseconds.
pub const DEFAULT_REQUEST_TIMEOUT_MICROS: u64 = 5_000_000;

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
    /// When the request expires (`now + timeout` at registration); the
    /// reference hands the timeout to the dispatcher's once-off timer
    /// instead (`Request::Request`), which needs no absolute clock here.
    deadline_micros: u64,
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
    ///
    /// `timeout_micros` is the `ReplyOrder::microseconds`; the deadline is
    /// `now_micros + timeout_micros`. The reference registers the timeout
    /// with the dispatcher's once-off timer only for requests without a
    /// channel (`Request::Request`); the channel case arrives with BW-02-03,
    /// so every ported request times out.
    pub fn add_request(
        &mut self,
        handler: Box<dyn ReplyMessageHandler>,
        timeout_micros: u64,
        now_micros: u64,
    ) -> ReplyId {
        let reply_id = self.get_next_reply_id();
        self.requests.insert(
            reply_id,
            PendingRequest {
                handler,
                deadline_micros: now_micros.saturating_add(timeout_micros),
            },
        );
        reply_id
    }

    /// How many requests are pending.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.requests.len()
    }

    /// `Request::handleTimeout` path: fail every pending request whose
    /// deadline has passed, invoking `on_exception(REASON_TIMER_EXPIRED)`
    /// exactly once each and dropping it from the map. Returns the number
    /// failed.
    pub fn check_timeouts(&mut self, now_micros: u64) -> usize {
        let expired: Vec<ReplyId> = self
            .requests
            .iter()
            .filter(|(_, request)| request.deadline_micros <= now_micros)
            .map(|(id, _)| *id)
            .collect();

        let expired_count = expired.len();
        for id in expired {
            self.fail_request(id, Reason::TimerExpired);
        }
        expired_count
    }

    /// `failRequest`: remove the pending request (if any) and tell its
    /// handler the request failed. Returns whether a request was cancelled;
    /// the callback fires exactly once, and a later reply or timeout finds
    /// nothing.
    pub fn fail_request(&mut self, reply_id: ReplyId, reason: Reason) -> bool {
        let Some(request) = self.requests.remove(&reply_id) else {
            return false;
        };
        let mut handler = request.handler;
        handler.on_exception(reason);
        true
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

        /// A handler that records into two caller-owned sinks, so a test can
        /// observe both the replies and the exceptions of a boxed handler.
        fn with_exceptions(
            replies: &Rc<RefCell<Vec<Vec<u8>>>>,
            exceptions: &Rc<RefCell<Vec<Reason>>>,
        ) -> Self {
            Self {
                replies: Rc::clone(replies),
                exceptions: Rc::clone(exceptions),
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
            ids.push(manager.add_request(
                Box::new(RecordingHandler::new(sink)),
                DEFAULT_REQUEST_TIMEOUT_MICROS,
                0,
            ));
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
        let id = manager.add_request(
            Box::new(RecordingHandler::new(&sink)),
            DEFAULT_REQUEST_TIMEOUT_MICROS,
            0,
        );

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
        let id = manager.add_request(
            Box::new(RecordingHandler::new(&sink)),
            DEFAULT_REQUEST_TIMEOUT_MICROS,
            0,
        );

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
        let id = manager.add_request(
            Box::new(RecordingHandler::new(&sink)),
            DEFAULT_REQUEST_TIMEOUT_MICROS,
            0,
        );

        // The request is failed (e.g. timed out) and drops out of the map.
        manager.dispatch_reply(id, &[1]);
        // The late reply for the same id finds nothing.
        assert_eq!(
            manager.dispatch_reply(id, &[2]),
            DispatchOutcome::UnknownReply
        );
    }

    /// `Request::handleTimeout`: a pending request whose deadline has passed
    /// receives `on_exception(REASON_TIMER_EXPIRED)` exactly once and leaves
    /// the map, so a second pass (or a late reply) has nothing to fail.
    #[test]
    fn a_timed_out_request_fires_its_exception_once() {
        let mut manager = RequestManager::new(10101);
        let replies = Rc::new(RefCell::new(Vec::new()));
        let exceptions = Rc::new(RefCell::new(Vec::new()));

        // Registered at now=0 with the default 5s timeout.
        let id = manager.add_request(
            Box::new(RecordingHandler::with_exceptions(&replies, &exceptions)),
            DEFAULT_REQUEST_TIMEOUT_MICROS,
            0,
        );

        // One tick short of the deadline nothing fires (the reference's
        // `checkTimeouts` is driven by the dispatcher's timer tick).
        assert_eq!(
            manager.check_timeouts(DEFAULT_REQUEST_TIMEOUT_MICROS - 1),
            0
        );
        assert!(exceptions.borrow().is_empty());

        // At the deadline the request fails with TIMER_EXPIRED...
        assert_eq!(manager.check_timeouts(DEFAULT_REQUEST_TIMEOUT_MICROS), 1);
        assert_eq!(*exceptions.borrow(), vec![Reason::TimerExpired]);
        // ...exactly once, and the pending state is released.
        assert_eq!(manager.pending_count(), 0);
        assert_eq!(
            manager.check_timeouts(DEFAULT_REQUEST_TIMEOUT_MICROS + 1),
            0
        );
        assert_eq!(exceptions.borrow().len(), 1);
        // A late reply for the released id is dropped, not delivered.
        assert_eq!(
            manager.dispatch_reply(id, &[1]),
            DispatchOutcome::UnknownReply
        );
        assert!(replies.borrow().is_empty());
    }

    /// A replied request is out of the map, so a later timeout pass cannot
    /// fail it a second time.
    #[test]
    fn a_replied_request_never_times_out() {
        let mut manager = RequestManager::new(10101);
        let replies = Rc::new(RefCell::new(Vec::new()));
        let exceptions = Rc::new(RefCell::new(Vec::new()));

        let id = manager.add_request(
            Box::new(RecordingHandler::with_exceptions(&replies, &exceptions)),
            DEFAULT_REQUEST_TIMEOUT_MICROS,
            0,
        );

        // The reply lands before the deadline...
        assert_eq!(
            manager.dispatch_reply(id, &[0x42]),
            DispatchOutcome::Dispatched
        );
        // ...so the past-deadline pass finds nothing to fail.
        assert_eq!(manager.check_timeouts(DEFAULT_REQUEST_TIMEOUT_MICROS), 0);
        assert!(exceptions.borrow().is_empty());
        assert_eq!(*replies.borrow(), vec![vec![0x42]]);
    }

    /// `Request::cancel` (via `failRequest`): a request cancelled by its
    /// caller fails with the caller's reason exactly once, and a reply that
    /// arrives afterwards is dropped without a second callback.
    #[test]
    fn a_cancelled_request_fails_once_and_ignores_late_replies() {
        let mut manager = RequestManager::new(10101);
        let replies = Rc::new(RefCell::new(Vec::new()));
        let exceptions = Rc::new(RefCell::new(Vec::new()));

        let id = manager.add_request(
            Box::new(RecordingHandler::with_exceptions(&replies, &exceptions)),
            DEFAULT_REQUEST_TIMEOUT_MICROS,
            0,
        );

        // Cancelling an unknown id does nothing.
        assert!(!manager.fail_request(424242, Reason::ChannelLost));

        // Cancelling the live request fails it with the caller's reason...
        assert!(manager.fail_request(id, Reason::ChannelLost));
        assert_eq!(*exceptions.borrow(), vec![Reason::ChannelLost]);
        assert_eq!(manager.pending_count(), 0);
        // ...and it stays failed: a late reply and a late timeout pass are
        // both no-ops.
        assert_eq!(
            manager.dispatch_reply(id, &[1, 2]),
            DispatchOutcome::UnknownReply
        );
        assert_eq!(manager.check_timeouts(DEFAULT_REQUEST_TIMEOUT_MICROS), 0);
        assert_eq!(exceptions.borrow().len(), 1);
        assert!(replies.borrow().is_empty());
    }

    /// `cancelRequestsFor`-shaped teardown: failing every pending request
    /// releases them all, each with exactly one exception.
    #[test]
    fn every_pending_request_fails_on_teardown() {
        let mut manager = RequestManager::new(10101);
        let replies = Rc::new(RefCell::new(Vec::new()));
        let exceptions = Rc::new(RefCell::new(Vec::new()));

        for _ in 0..4 {
            manager.add_request(
                Box::new(RecordingHandler::with_exceptions(&replies, &exceptions)),
                DEFAULT_REQUEST_TIMEOUT_MICROS,
                0,
            );
        }
        assert_eq!(manager.pending_count(), 4);

        let ids: Vec<ReplyId> = (10101..=10104).collect();
        let mut failed = 0;
        for id in ids {
            failed += usize::from(manager.fail_request(id, Reason::ShuttingDown));
        }
        assert_eq!(failed, 4);
        assert_eq!(*exceptions.borrow(), vec![Reason::ShuttingDown; 4]);
        assert_eq!(manager.pending_count(), 0);
    }
}
