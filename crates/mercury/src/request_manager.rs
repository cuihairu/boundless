//! `RequestManager` (`lib/network/request_manager.cpp`): the half of RPC
//! that lives on the sending side, and the half that answers the replies.
//!
//! `add_reply_order` hands out a reply id and remembers who the answer
//! belongs to; `handle_message` is installed as the handler of
//! `InterfaceElement::REPLY` and matches an incoming reply back to that
//! request. The two halves meet on the id the reference writes into the
//! request frame at send time and reads back off the head of the reply.
//!
//! A request with no channel arms a one-off timeout on the dispatcher
//! (the reference does it in the `Request` constructor with
//! `pRequestManager` as the timer's user argument). Here the expiry
//! closure holds the manager's shared state weakly and the reply id as
//! the user argument, so a fired timer fails exactly the request it
//! belongs to. An answer, a channel loss, a per-handler cancellation or
//! the manager going down all take the request out and cancel its timer
//! first, so every callback happens exactly once.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use foundation::stream::BinaryStream;
use foundation::time_queue::{TimerHandle, TimerHandlerFn};

use crate::endpoint::Address;
use crate::event_dispatcher::EventDispatcher;
use crate::interfaces::{InputMessageHandler, ReplyMessageHandler};
use crate::message_framing::FrameError;
use crate::misc::{MessageID, Reason, ReplyID, REPLY_ID_MAX};
use crate::reply_order::ReplyOrder;
use crate::request::Request;
use crate::unpacked_message_header::UnpackedMessageHeader;

/// The reply identifier this manager answers
/// (`interface_element.cpp:25`, `InterfaceElement::REPLY`).
const REPLY_MESSAGE_IDENTIFIER: MessageID = crate::interface_element::REPLY_MESSAGE_IDENTIFIER;

/// The width of the reply id a reply message starts with, in bytes.
///
/// The reference measures it as `sizeof(int)`, which is the width of a
/// `ReplyID`; naming it that way here keeps the two from drifting apart.
const IN_REPLY_TO_SIZE: usize = std::mem::size_of::<ReplyID>();

/// The profiler label the timeout timers carry. The reference's
/// `addOnceOffTimer` names no timer; the port's queue wants a label and
/// the owner is the honest one.
const REQUEST_TIMER_NAME: &str = "Request";

/// `DEFAULT_REQUEST_TIMEOUT` (`bundle.hpp:25`), in microseconds: five
/// seconds, the timeout every request that names no timeout of its own
/// gets.
pub const DEFAULT_REQUEST_TIMEOUT: i32 = 5_000_000;

/// The pending-request bookkeeping the timeout closures reach: the map
/// and the id counter, held behind one shared handle so an expiry can
/// find the request it belongs to.
struct Inner {
    /// `requestMap_`: reply id to the request awaiting that reply. Ordered
    /// like the reference's `BW::map`, which its destructor walks to
    /// report what is still pending.
    requests: RefCell<BTreeMap<ReplyID, Request>>,
    /// `nextReplyID_`.
    next_reply_id: Cell<ReplyID>,
}

impl Inner {
    /// `failRequest( request, reason )`: clean up a failed request and
    /// tell its handler. The request leaves the map before its handler
    /// runs, so a handler that starts a request of its own cannot fail in
    /// a loop, and the timer is cancelled first so the same request never
    /// reports twice.
    fn fail_request(&self, reply_id: ReplyID, reason: Reason) {
        // The reference's erase happens before the failure call and holds
        // nothing open across it; the borrow must end the same way, so a
        // handler that starts a request of its own can get back in here.
        let request = {
            let mut requests = self.requests.borrow_mut();
            requests.remove(&reply_id)
        };
        if let Some(mut request) = request {
            request.cancel_timer();
            request.handle_failure(reason);
        }
    }

    /// `getNextReplyID()`: the next id, wrapping back to one once it has
    /// passed the reference's `REPLY_ID_MAX`.
    fn next_reply_id(&self) -> ReplyID {
        let mut next = self.next_reply_id.get();
        if next > REPLY_ID_MAX {
            next = 1;
        }
        self.next_reply_id.set(next + 1);
        next
    }
}

/// The reference's `dispatcher.addOnceOffTimer( microseconds, this,
/// pRequestManager )`: one expiry, `microseconds` out, failing the
/// request named by the user argument. The closure holds the shared
/// state weakly, so a timer that outlives its manager finds nothing to
/// fail.
fn arm_timeout(
    inner: &Rc<Inner>,
    dispatcher: &EventDispatcher,
    reply_id: ReplyID,
    microseconds: i32,
) -> TimerHandle<u64> {
    let weak_inner = Rc::downgrade(inner);
    let handler = Rc::new(TimerHandlerFn::new(
        move |_, user| {
            if let Some(inner) = weak_inner.upgrade() {
                inner.fail_request(user as ReplyID, Reason::TimerExpired);
            }
        },
        REQUEST_TIMER_NAME,
    ));
    dispatcher.add_once_off_timer(
        microseconds as i64,
        handler,
        reply_id as usize,
        REQUEST_TIMER_NAME,
    )
}

/// The body of `addReplyOrder`, as a function of the shared state and the
/// dispatcher rather than of the manager, so the manager's destructor --
/// which still holds both -- can serve a handler that starts one more
/// request while it is being told its requests are gone.
fn add_reply_order_to(
    inner: &Rc<Inner>,
    dispatcher: &EventDispatcher,
    order: &ReplyOrder,
    channel: Option<Address>,
    frame: &mut [u8],
) -> Result<ReplyID, FrameError> {
    let offset = order.reply_id_offset();
    if offset + IN_REPLY_TO_SIZE > frame.len() {
        return Err(FrameError::BufferTooSmall);
    }

    let reply_id = inner.next_reply_id();

    let mut request = Request::new(reply_id, order, channel);
    if channel.is_none() {
        // MF_ASSERT( replyOrder.microseconds > 0 ) in the reference's
        // Request constructor.
        assert!(
            order.microseconds() > 0,
            "Request: a request with no channel needs a positive timeout"
        );
        request.set_timer(arm_timeout(
            inner,
            dispatcher,
            reply_id,
            order.microseconds(),
        ));
    }

    inner.requests.borrow_mut().insert(reply_id, request);

    // `*pReplyID = BW_HTONL( replyID )`: identity on this host, so
    // little-endian bytes.
    frame[offset..offset + IN_REPLY_TO_SIZE].copy_from_slice(&reply_id.to_le_bytes());

    Ok(reply_id)
}

/// `RequestManager`: the pending requests of one socket.
///
/// The reference derives from `InputMessageHandler` because it is
/// registered as the handler of `InterfaceElement::REPLY`; that derive is
/// the point of this port, so it is kept.
pub struct RequestManager {
    /// The bookkeeping shared with the timeout closures.
    inner: Rc<Inner>,
    /// The dispatcher the request timeouts are scheduled on
    /// (`dispatcher_`).
    dispatcher: Rc<EventDispatcher>,
    /// `isExternal_`: external nubs check a reply's source, internal ones
    /// do not, because an offloaded channel answers from another address.
    is_external: bool,
}

impl RequestManager {
    /// `RequestManager( isExternal, dispatcher )`: the reference seeds the
    /// id counter from the clock so that two processes started together do
    /// not hand out the same ids in the same window.
    pub fn new(is_external: bool, dispatcher: Rc<EventDispatcher>) -> Self {
        Self::with_next_reply_id(
            is_external,
            seed_next_reply_id(real_clock_micros()),
            dispatcher,
        )
    }

    /// A manager whose next reply id is a fixed value, so a test can name
    /// the ids it will get.
    pub fn with_next_reply_id(
        is_external: bool,
        next_reply_id: ReplyID,
        dispatcher: Rc<EventDispatcher>,
    ) -> Self {
        RequestManager {
            inner: Rc::new(Inner {
                requests: RefCell::new(BTreeMap::new()),
                next_reply_id: Cell::new(next_reply_id),
            }),
            dispatcher,
            is_external,
        }
    }

    /// `addReplyOrder( replyOrder, pChannel )`: allocate a reply id for
    /// this request, remember the request, and write the id into the
    /// frame.
    ///
    /// The reference writes through the `pReplyID` pointer it was handed;
    /// here the offset into the frame carries the same information, and
    /// [`FrameError::BufferTooSmall`] stands for the reference's
    /// out-of-range write.
    ///
    /// Returns the id, so the caller can tell its replies apart.
    pub fn add_reply_order(
        &self,
        order: &ReplyOrder,
        channel: Option<Address>,
        frame: &mut [u8],
    ) -> Result<ReplyID, FrameError> {
        add_reply_order_to(&self.inner, &self.dispatcher, order, channel, frame)
    }

    /// `handleMessage`: deliver a reply to the request it answers.
    ///
    /// The reference returns void and reports trouble by logging; this
    /// does the same.
    fn deliver_reply(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        if header.identifier() != REPLY_MESSAGE_IDENTIFIER {
            eprintln!(
                "RequestManager::handleMessage( {} ): received the wrong kind of message!",
                source.c_str()
            );
            return;
        }

        if header.length() < IN_REPLY_TO_SIZE as i32 {
            eprintln!(
                "RequestManager::handleMessage( {} ): received a corrupted reply message (length {})!",
                source.c_str(),
                header.length()
            );
            return;
        }

        // The reference leaves the id zero when the stream is too short,
        // so a reply with no readable id answers nobody.
        let in_reply_to = data.read_i32().unwrap_or(0);
        header.set_length(header.length() - IN_REPLY_TO_SIZE as i32);

        if !self.inner.requests.borrow().contains_key(&in_reply_to) {
            eprintln!(
                "RequestManager::handleMessage( {} ): Couldn't find handler for reply id 0x{:08x} (maybe it timed out?)",
                source.c_str(),
                in_reply_to as u32
            );
            // The reference runs the stream to the end, so the bundle
            // does not hand this reply to another handler.
            data.finish();
            return;
        }

        // Replies can come from any address on an internal nub, because an
        // offloaded channel answers from somewhere else.
        if self.is_external
            && !self
                .inner
                .requests
                .borrow()
                .get(&in_reply_to)
                .unwrap()
                .is_valid_source(source)
        {
            eprintln!(
                "RequestManager::handleMessage: Got reply to request {} from unexpected source {}",
                in_reply_to,
                source.c_str()
            );
            return;
        }

        // The request leaves the pending map before its handler runs, so a
        // handler that starts a request of its own cannot answer itself.
        // The borrow ends with the removal: nothing is held open across
        // the delivery.
        let mut request = {
            let mut requests = self.inner.requests.borrow_mut();
            requests
                .remove(&in_reply_to)
                .expect("the request was present on the lookup above")
        };
        // The answer arrived, so the timeout must not: one callback per
        // request, never two.
        request.cancel_timer();
        request.handle_message(source, header, data);
    }

    /// `cancelRequestsFor( pChannel )`: fail every request riding the
    /// channel, with `REASON_CHANNEL_LOST`.
    ///
    /// The matching ids are collected before anything fails, for the
    /// reference's own reason: `failRequest` can modify the map, so its
    /// iterator needs to be moved early.
    pub fn cancel_requests_for_channel(&self, channel: &Address) {
        let matched: Vec<ReplyID> = {
            let requests = self.inner.requests.borrow();
            requests
                .iter()
                .filter(|(_, request)| request.matches_channel(channel))
                .map(|(id, _)| *id)
                .collect()
        };
        for reply_id in matched {
            self.inner.fail_request(reply_id, Reason::ChannelLost);
        }
    }

    /// `cancelRequestsFor( pHandler, reason )`: stop a reply handler from
    /// hearing anything more -- no message and no timeout -- by failing
    /// each of its outstanding requests with the given reason.
    ///
    /// The matching ids are collected first, for the same
    /// modify-while-iterating reason as the channel variant.
    pub fn cancel_requests_for_handler(
        &self,
        handler: &Rc<dyn ReplyMessageHandler>,
        reason: Reason,
    ) {
        let matched: Vec<ReplyID> = {
            let requests = self.inner.requests.borrow();
            requests
                .iter()
                .filter(|(_, request)| request.matches_handler(handler))
                .map(|(id, _)| *id)
                .collect()
        };
        for reply_id in matched {
            self.inner.fail_request(reply_id, reason);
        }
    }

    /// `failRequest( request, reason )`: the shared failure path -- take
    /// the request out, cancel its timeout, tell the handler.
    pub fn fail_request(&self, reply_id: ReplyID, reason: Reason) {
        self.inner.fail_request(reply_id, reason);
    }

    /// The number of requests still awaiting a reply.
    pub fn num_pending(&self) -> usize {
        self.inner.requests.borrow().len()
    }
}

impl InputMessageHandler for RequestManager {
    fn handle_message(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        self.deliver_reply(source, header, data);
    }
}

/// `~RequestManager`: fail every request still pending, through the
/// shutdown hook -- Mercury going down is not any request's fault.
impl Drop for RequestManager {
    fn drop(&mut self) {
        let pending: Vec<Request> = {
            let mut map = self.inner.requests.borrow_mut();
            std::mem::take(&mut *map).into_values().collect()
        };
        if !pending.is_empty() {
            eprintln!(
                "RequestManager::~RequestManager: Num pending reply handlers = {}",
                pending.len()
            );
        }
        for mut request in pending {
            request.cancel_timer();
            request.handle_failure(Reason::ShuttingDown);
        }
    }
}

/// The first id the reference's constructor hands out
/// (`request_manager.cpp:25`): `( timestamp() % 100000 ) + 10101`.
///
/// The offset keeps two processes started at different times from handing
/// out the same ids in the same window.
fn seed_next_reply_id(clock_micros: i64) -> ReplyID {
    (clock_micros % 100_000) as ReplyID + 10_101
}

/// `timestamp()`, the microseconds the reference's constructor seeds the
/// id counter with.
fn real_clock_micros() -> i64 {
    platform::clock::monotonic_now_micros().micros() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    use crate::interfaces::ReplyMessageHandler;
    use crate::misc::REPLY_ID_NONE;
    use crate::nub_exception::NubException;

    /// A reply handler that records what it got, so a test can tell three
    /// requests' answers apart.
    #[derive(Clone, Default)]
    struct Recorder {
        /// `( arg, payload )` of each reply delivered.
        delivered: std::cell::RefCell<Vec<(usize, Vec<u8>)>>,
        /// `( reason, arg )` of each failure reported.
        failures: std::cell::RefCell<Vec<(Reason, usize)>>,
        /// `( reason, arg )` of each shutdown reported.
        shutdowns: std::cell::RefCell<Vec<(Reason, usize)>>,
    }

    impl ReplyMessageHandler for Recorder {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &UnpackedMessageHeader,
            data: &mut BinaryStream,
            arg: usize,
        ) {
            // The body starts after the reply id, which the dispatch has
            // already read off.
            let payload = data.unread_bytes().to_vec();
            self.delivered.borrow_mut().push((arg, payload));
        }

        fn handle_exception(&self, exception: &NubException, arg: usize) {
            self.failures.borrow_mut().push((exception.reason(), arg));
        }

        fn handle_shutting_down(&self, exception: &NubException, arg: usize) {
            self.shutdowns.borrow_mut().push((exception.reason(), arg));
        }
    }

    impl Recorder {
        fn answers(&self) -> Vec<(usize, Vec<u8>)> {
            self.delivered.borrow().clone()
        }

        fn failures(&self) -> Vec<(Reason, usize)> {
            self.failures.borrow().clone()
        }

        fn shutdowns(&self) -> Vec<(Reason, usize)> {
            self.shutdowns.borrow().clone()
        }
    }

    /// A dispatcher on a pinned clock, so a test can drag time forward
    /// past the timeouts it armed.
    fn pinned_dispatcher() -> (Rc<Cell<u64>>, Rc<EventDispatcher>) {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let clock_for_dispatcher = Rc::clone(&clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_for_dispatcher.get()
        })));
        (clock, dispatcher)
    }

    /// One pending request with the given argument. The frame is long
    /// enough for the id, at the offset the framing layer would put it.
    ///
    /// The handler is shared, not copied, so what the manager delivers is
    /// what the test reads back.
    fn pending(
        manager: &RequestManager,
        handler: &Rc<Recorder>,
        arg: usize,
        channel: Option<Address>,
    ) -> (ReplyID, Vec<u8>) {
        let order = ReplyOrder::new(
            Rc::clone(handler) as Rc<dyn ReplyMessageHandler>,
            arg,
            DEFAULT_REQUEST_TIMEOUT,
            2,
        );
        let mut frame = vec![0u8; 12];
        let id = manager
            .add_reply_order(&order, channel, &mut frame)
            .unwrap();
        (id, frame)
    }

    /// One pending request with a timeout of the test's own choosing.
    fn pending_with_timeout(
        manager: &RequestManager,
        handler: &Rc<Recorder>,
        arg: usize,
        timeout_micros: i32,
    ) -> (ReplyID, Vec<u8>) {
        let order = ReplyOrder::new(
            Rc::clone(handler) as Rc<dyn ReplyMessageHandler>,
            arg,
            timeout_micros,
            2,
        );
        let mut frame = vec![0u8; 12];
        let id = manager.add_reply_order(&order, None, &mut frame).unwrap();
        (id, frame)
    }

    /// Feed a reply for `id` back to `manager` from `source`.
    fn reply(manager: &RequestManager, id: ReplyID, payload: &[u8], source: &Address) {
        let mut data = BinaryStream::new();
        data.write_i32(id);
        data.write_bytes(payload);

        let mut header = UnpackedMessageHeader::new(REPLY_MESSAGE_IDENTIFIER);
        header.set_length(IN_REPLY_TO_SIZE as i32 + payload.len() as i32);
        InputMessageHandler::handle_message(manager, source, &mut header, &mut data);
    }

    /// `REPLY_ID_MAX` to the id after it: the reference wraps to one
    /// rather than climbing past its ceiling.
    #[test]
    fn the_reply_ids_climb_and_wrap_at_the_ceiling() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, REPLY_ID_MAX - 1, dispatcher);
        let handler = Rc::new(Recorder::default());

        let first = pending(&manager, &handler, 0, None).0;
        let second = pending(&manager, &handler, 0, None).0;
        let third = pending(&manager, &handler, 0, None).0;

        assert_eq!(first, REPLY_ID_MAX - 1);
        assert_eq!(second, REPLY_ID_MAX);
        assert_eq!(third, 1, "the id after the ceiling is one");
    }

    /// Three requests asked in one order and answered in another: every
    /// reply reaches the handler that asked for it, with the argument the
    /// request was sent with. This is the acceptance: interleaved
    /// requests each receive their own response.
    #[test]
    fn interleaved_replies_each_reach_their_own_request() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 10101, dispatcher);
        let first = Rc::new(Recorder::default());
        let second = Rc::new(Recorder::default());
        let third = Rc::new(Recorder::default());

        let ids = [
            pending(&manager, &first, 20, None),
            pending(&manager, &second, 21, None),
            pending(&manager, &third, 22, None),
        ];
        let just_ids: [ReplyID; 3] = ids.clone().map(|(id, _)| id);
        assert_eq!(
            just_ids,
            [10101, 10102, 10103],
            "the ids climb one per request"
        );
        assert_eq!(manager.num_pending(), 3);

        // Answer the third, the first, the second.
        reply(&manager, ids[2].0, &[3], &Address::new(0x7f000001, 4321));
        reply(&manager, ids[0].0, &[1], &Address::new(0x7f000001, 4321));
        reply(&manager, ids[1].0, &[2], &Address::new(0x7f000001, 4321));

        assert_eq!(third.answers(), vec![(22, vec![3])]);
        assert_eq!(first.answers(), vec![(20, vec![1])]);
        assert_eq!(second.answers(), vec![(21, vec![2])]);
        assert_eq!(manager.num_pending(), 0, "every reply is consumed");
    }

    /// The id the reference writes into the request frame is the one the
    /// manager gave out, little-endian, at the offset the order names, and
    /// nothing else in the frame is touched.
    #[test]
    fn the_id_lands_in_the_request_frame_little_endian() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 77, dispatcher);
        let offset = 6;
        let order = ReplyOrder::new(
            Rc::new(Recorder::default()) as Rc<dyn ReplyMessageHandler>,
            5,
            DEFAULT_REQUEST_TIMEOUT,
            offset,
        );

        let mut frame = [0xAAu8; 16];
        assert_eq!(manager.add_reply_order(&order, None, &mut frame), Ok(77));

        assert_eq!(&frame[..offset], &[0xAA; 6], "nothing before the id");
        assert_eq!(
            &frame[offset..offset + IN_REPLY_TO_SIZE],
            &77i32.to_le_bytes()
        );
        assert_eq!(
            &frame[offset + IN_REPLY_TO_SIZE..],
            &[0xAA; 6],
            "nothing after the id"
        );
    }

    /// A frame too short for the id is refused, and no request is
    /// remembered: a refused send leaks no pending entry.
    #[test]
    fn a_short_frame_is_refused_without_leaking_a_request() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 9, dispatcher);
        let order = ReplyOrder::new(
            Rc::new(Recorder::default()) as Rc<dyn ReplyMessageHandler>,
            1,
            DEFAULT_REQUEST_TIMEOUT,
            2,
        );

        let mut frame = [0u8; 4];
        assert_eq!(
            manager.add_reply_order(&order, None, &mut frame),
            Err(FrameError::BufferTooSmall)
        );
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(frame, [0u8; 4], "the refused write leaves the frame alone");
    }

    /// A message that is not a reply is dropped, and costs nothing: the
    /// pending request is untouched.
    #[test]
    fn a_non_reply_message_is_dropped() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 3, dispatcher);
        let handler = Rc::new(Recorder::default());
        pending(&manager, &handler, 0, None);

        let mut data = BinaryStream::new();
        data.write_i32(3);
        let mut header = UnpackedMessageHeader::new(7);
        header.set_length(4);
        InputMessageHandler::handle_message(&manager, &Address::new(0, 1), &mut header, &mut data);

        assert_eq!(manager.num_pending(), 1, "the wrong message costs nothing");
        assert_eq!(handler.answers(), Vec::<(usize, Vec<u8>)>::new());
    }

    /// A reply shorter than its own id is corrupted: reported, not
    /// delivered, and the request it might have answered stays pending.
    #[test]
    fn a_short_reply_is_corrupted_and_the_request_stays_pending() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 4, dispatcher);
        let handler = Rc::new(Recorder::default());
        pending(&manager, &handler, 0, None);

        let mut header = UnpackedMessageHeader::new(REPLY_MESSAGE_IDENTIFIER);
        header.set_length(3);
        InputMessageHandler::handle_message(
            &manager,
            &Address::new(0, 1),
            &mut header,
            &mut BinaryStream::new(),
        );

        assert_eq!(
            manager.num_pending(),
            1,
            "a corrupted reply is not a failure"
        );
        assert_eq!(handler.answers(), Vec::<(usize, Vec<u8>)>::new());
    }

    /// A reply nobody asked for is dropped, and the stream is run to the
    /// end so the bundle does not hand it to another handler.
    #[test]
    fn an_unexpected_reply_is_dropped_and_consumed() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 5, dispatcher);

        let mut data = BinaryStream::new();
        data.write_i32(500);
        data.write_u8(1);
        let mut header = UnpackedMessageHeader::new(REPLY_MESSAGE_IDENTIFIER);
        header.set_length(5);
        InputMessageHandler::handle_message(&manager, &Address::new(0, 1), &mut header, &mut data);

        assert!(data.at_end(), "the leftover reply is consumed");
        assert_eq!(header.length(), 1, "the id is read off the body");
        assert_eq!(manager.num_pending(), 0);
    }

    /// An external manager rejects a reply from anywhere but the channel's
    /// address, and keeps the request pending so the right reply can still
    /// arrive. An internal one accepts that same reply.
    #[test]
    fn the_source_is_checked_only_on_external_nubs() {
        let channel = Address::new(0x7f000002, 5000);
        let elsewhere = Address::new(0x7f000002, 5001);

        for external in [true, false] {
            let (_clock, dispatcher) = pinned_dispatcher();
            let manager = RequestManager::with_next_reply_id(external, 60, dispatcher);
            let handler = Rc::new(Recorder::default());
            let (id, _) = pending(&manager, &handler, 0, Some(channel));

            reply(&manager, id, &[], &elsewhere);
            if external {
                assert_eq!(
                    handler.answers(),
                    Vec::<(usize, Vec<u8>)>::new(),
                    "an external nub refuses a foreign reply"
                );
                assert_eq!(manager.num_pending(), 1, "the request is still pending");
            } else {
                assert_eq!(
                    handler.answers().len(),
                    1,
                    "an internal nub takes the reply from anywhere"
                );
                assert_eq!(manager.num_pending(), 0);
            }

            reply(&manager, id, &[], &channel);
            assert_eq!(
                manager.num_pending(),
                0,
                "the reply from the right address is still honoured"
            );
        }
    }

    /// A request with no channel answers from any address, which is what
    /// lets an offloaded channel's reply through on an external nub.
    #[test]
    fn a_channelless_request_answers_from_any_address() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(true, 80, dispatcher);
        let handler = Rc::new(Recorder::default());
        let (id, _) = pending(&manager, &handler, 1, None);

        reply(&manager, id, &[9], &Address::new(0x0a0a0a0a, 1));
        assert_eq!(handler.answers(), vec![(1, vec![9])]);
        assert_eq!(manager.num_pending(), 0);
    }

    /// The constructor's clock seed is in the reference's window: above
    /// ten thousand and below the ceiling, so ids never start at zero.
    #[test]
    fn a_default_manager_starts_its_ids_in_the_reference_window() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::new(true, dispatcher);
        let handler = Rc::new(Recorder::default());
        let (id, _) = pending(&manager, &handler, 0, None);
        assert!(id >= 10_101, "the seed starts above the round ten thousand");
        assert!(id < REPLY_ID_MAX, "the seed is below the ceiling");
        assert_ne!(id, REPLY_ID_NONE, "a handed-out id is never the absent id");
    }

    /// The seed is the reference's own arithmetic, so it can be pinned
    /// without a clock.
    #[test]
    fn the_seed_folds_the_clock_into_the_reference_window() {
        assert_eq!(seed_next_reply_id(0), 10_101);
        assert_eq!(seed_next_reply_id(1), 10_102);
        assert_eq!(seed_next_reply_id(99_999), 110_100);
        assert_eq!(
            seed_next_reply_id(100_000),
            10_101,
            "the fold wraps back down"
        );
    }

    /// The reference puts the manager in the interface table as the handler
    /// of `InterfaceElement::REPLY`, so a reply message reaches the
    /// manager through the table like any other message.
    #[test]
    fn the_manager_serves_the_reply_element_through_the_table() {
        use crate::interface_element::{InterfaceElement, REPLY_MESSAGE_IDENTIFIER};
        use crate::interface_table::InterfaceTable;

        let table = InterfaceTable::new();
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = Rc::new(RequestManager::with_next_reply_id(true, 400, dispatcher));
        table.serve(
            &InterfaceElement::reply(),
            Some(Rc::clone(&manager) as Rc<dyn InputMessageHandler>),
        );
        assert_eq!(table.msg_name(REPLY_MESSAGE_IDENTIFIER), "Reply");

        let handler = Rc::new(Recorder::default());
        let (id, _) = pending(&manager, &handler, 0, None);
        reply(&manager, id, &[7], &Address::new(0x7f000001, 9));

        assert_eq!(handler.answers(), vec![(0, vec![7])]);
        assert_eq!(manager.num_pending(), 0);

        // The failure half of the same handler is still silent: nothing
        // failed, the handler was merely called through the manager's
        // bookkeeping to prove the hook is wired.
        handler.handle_exception(
            &NubException::new(crate::misc::Reason::TimerExpired, None),
            0,
        );
        assert_eq!(handler.answers(), vec![(0, vec![7])]);
        assert_eq!(handler.failures(), vec![(Reason::TimerExpired, 0)]);
    }

    /// The acceptance: a request with no channel times out once, reports
    /// the failure once with `REASON_TIMER_EXPIRED`, and releases the
    /// pending state -- the same reply id afterwards answers nobody.
    #[test]
    fn a_channelless_request_times_out_and_reports_exactly_once() {
        let (clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 10101, Rc::clone(&dispatcher));
        let handler = Rc::new(Recorder::default());
        let (id, _) = pending(&manager, &handler, 42, None);
        assert_eq!(manager.num_pending(), 1);

        clock.set(clock.get() + DEFAULT_REQUEST_TIMEOUT as u64 + 1);
        assert_eq!(
            dispatcher.process_timers(),
            1,
            "the request's one timeout fired"
        );

        assert_eq!(
            handler.failures(),
            vec![(Reason::TimerExpired, 42)],
            "the timeout reports exactly once"
        );
        assert_eq!(handler.answers(), Vec::<(usize, Vec<u8>)>::new());
        assert_eq!(manager.num_pending(), 0, "the pending state is released");

        // The released id answers nobody now: the late reply is the
        // reference's "maybe it timed out?" path.
        reply(&manager, id, &[1], &Address::new(1, 1));
        assert_eq!(handler.failures(), vec![(Reason::TimerExpired, 42)]);
        assert_eq!(handler.answers(), Vec::<(usize, Vec<u8>)>::new());
    }

    /// A reply that arrives in time cancels the timeout: the handler gets
    /// the answer and only the answer, however long the clock runs on.
    #[test]
    fn a_replied_request_cancels_its_timeout() {
        let (clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 200, Rc::clone(&dispatcher));
        let handler = Rc::new(Recorder::default());
        let (id, _) = pending(&manager, &handler, 9, None);

        reply(&manager, id, &[5], &Address::new(1, 1));
        clock.set(clock.get() + DEFAULT_REQUEST_TIMEOUT as u64 * 10);
        dispatcher.process_timers();

        assert_eq!(handler.answers(), vec![(9, vec![5])]);
        assert_eq!(
            handler.failures(),
            Vec::<(Reason, usize)>::new(),
            "an answered request never reports a timeout"
        );
    }

    /// A request riding a channel arms no timer of its own: only the
    /// channel's loss can fail it.
    #[test]
    fn a_channel_request_carries_no_timeout_of_its_own() {
        let (clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 210, Rc::clone(&dispatcher));
        let handler = Rc::new(Recorder::default());
        let channel = Address::new(0x7f000001, 4000);
        let (id, _) = pending(&manager, &handler, 1, Some(channel));

        clock.set(clock.get() + DEFAULT_REQUEST_TIMEOUT as u64 * 10);
        assert_eq!(
            dispatcher.process_timers(),
            0,
            "no timer was armed for a channel request"
        );
        assert_eq!(handler.failures(), Vec::<(Reason, usize)>::new());
        assert_eq!(manager.num_pending(), 1);

        // And the request still works: the answer arrives, the pending
        // state goes away.
        reply(&manager, id, &[2], &channel);
        assert_eq!(handler.answers(), vec![(1, vec![2])]);
        assert_eq!(manager.num_pending(), 0);
    }

    /// `cancelRequestsFor( pChannel )`: losing the channel fails exactly
    /// its requests with `REASON_CHANNEL_LOST`; other channels' requests
    /// and channelless ones are untouched.
    #[test]
    fn losing_a_channel_fails_only_its_own_requests() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 220, Rc::clone(&dispatcher));
        let handler = Rc::new(Recorder::default());
        let gone = Address::new(0x7f000001, 5000);
        let other = Address::new(0x7f000001, 5001);

        let first = pending(&manager, &handler, 1, Some(gone));
        let second = pending(&manager, &handler, 2, Some(gone));
        let _elsewhere = pending(&manager, &handler, 3, Some(other));
        let _channelless = pending(&manager, &handler, 4, None);
        assert_eq!(manager.num_pending(), 4);

        manager.cancel_requests_for_channel(&gone);

        assert_eq!(
            handler.failures(),
            vec![(Reason::ChannelLost, 1), (Reason::ChannelLost, 2)],
            "only the lost channel's requests fail, with CHANNEL_LOST"
        );
        assert_eq!(handler.answers(), Vec::<(usize, Vec<u8>)>::new());
        assert_eq!(manager.num_pending(), 2, "the others are still pending");
        assert_eq!(first.0, 220);
        assert_eq!(second.0, 221);
    }

    /// `cancelRequestsFor( pHandler, reason )`: a handler taken away
    /// fails every request it owns, with the reason its canceller chose,
    /// and nothing of anyone else's.
    #[test]
    fn cancelling_a_handler_fails_only_its_requests() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 230, Rc::clone(&dispatcher));
        let leaving = Rc::new(Recorder::default());
        let staying = Rc::new(Recorder::default());

        let _first = pending(&manager, &leaving, 1, None);
        let _second = pending(&manager, &leaving, 2, None);
        let kept = pending(&manager, &staying, 3, None);

        let leaving_handler = Rc::clone(&leaving) as Rc<dyn ReplyMessageHandler>;
        manager.cancel_requests_for_handler(&leaving_handler, Reason::GeneralNetwork);

        assert_eq!(
            leaving.failures(),
            vec![(Reason::GeneralNetwork, 1), (Reason::GeneralNetwork, 2)],
        );
        assert_eq!(staying.failures(), Vec::<(Reason, usize)>::new());
        assert_eq!(manager.num_pending(), 1);

        // The surviving request still answers normally.
        reply(&manager, kept.0, &[8], &Address::new(1, 1));
        assert_eq!(staying.answers(), vec![(3, vec![8])]);
    }

    /// A channelless request with no timeout is a programming error, the
    /// reference's `MF_ASSERT( replyOrder.microseconds > 0 )`.
    #[test]
    #[should_panic(expected = "a request with no channel needs a positive timeout")]
    fn a_zero_timeout_without_a_channel_is_a_programming_error() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 240, dispatcher);
        let handler = Rc::new(Recorder::default());
        let order = ReplyOrder::new(Rc::clone(&handler) as Rc<dyn ReplyMessageHandler>, 1, 0, 2);
        let mut frame = [0u8; 12];
        let _ = manager.add_reply_order(&order, None, &mut frame);
    }

    /// A channel request needs no timeout, so a zero one is fine: the
    /// reference asserts only when the timer would be armed.
    #[test]
    fn a_zero_timeout_on_a_channel_is_fine() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 250, dispatcher);
        let handler = Rc::new(Recorder::default());
        let channel = Address::new(0x7f000001, 6000);
        let order = ReplyOrder::new(Rc::clone(&handler) as Rc<dyn ReplyMessageHandler>, 1, 0, 2);
        let mut frame = [0u8; 12];
        assert!(manager
            .add_reply_order(&order, Some(channel), &mut frame)
            .is_ok());
        assert_eq!(manager.num_pending(), 1);
    }

    /// `~RequestManager`: dropping the manager fails every pending
    /// request through the shutdown hook, exactly once each, and the
    /// armed timeouts never fire afterwards.
    #[test]
    fn dropping_the_manager_fails_every_request_with_shutting_down() {
        let (clock, dispatcher) = pinned_dispatcher();
        let manager = Rc::new(RequestManager::with_next_reply_id(
            false,
            300,
            Rc::clone(&dispatcher),
        ));
        let first = Rc::new(Recorder::default());
        let second = Rc::new(Recorder::default());
        let _ = pending(&manager, &first, 1, None);
        let _ = pending(&manager, &second, 2, None);
        let _ = pending(&manager, &first, 3, None);

        drop(manager);

        assert_eq!(
            first.shutdowns(),
            vec![(Reason::ShuttingDown, 1), (Reason::ShuttingDown, 3)],
        );
        assert_eq!(second.shutdowns(), vec![(Reason::ShuttingDown, 2)]);
        assert_eq!(first.failures(), Vec::<(Reason, usize)>::new());
        assert_eq!(
            dispatcher.process_timers(),
            0,
            "the cancelled timeouts fire nothing"
        );

        clock.set(clock.get() + DEFAULT_REQUEST_TIMEOUT as u64 * 10);
        assert_eq!(
            dispatcher.process_timers(),
            0,
            "however long the clock runs, nothing fires"
        );
    }

    /// A handler may start a request of its own while its failure is
    /// being reported -- the reference moves its iterator early because
    /// `failRequest` can modify the map, so the re-entered request
    /// survives the cancellation that triggered it. The delivery path
    /// gives the same freedom: a reply handler may ask again.
    #[test]
    fn a_handler_may_start_its_own_request_from_any_hook() {
        struct Reentrant {
            manager: std::rc::Weak<RequestManager>,
            /// The watcher's own handler handle, filled in once the
            /// watcher exists: each request it starts is its own again.
            self_handler: std::cell::RefCell<Option<std::rc::Weak<dyn ReplyMessageHandler>>>,
            notes: std::cell::RefCell<Vec<&'static str>>,
        }

        impl Reentrant {
            fn ask_again(&self) {
                let manager = self
                    .manager
                    .upgrade()
                    .expect("the manager outlives this handler");
                let handler = self
                    .self_handler
                    .borrow()
                    .as_ref()
                    .expect("the handler outlives this callback")
                    .upgrade()
                    .expect("the handler outlives this callback");
                let order = ReplyOrder::new(handler, 9, DEFAULT_REQUEST_TIMEOUT, 2);
                let mut frame = [0u8; 12];
                let _ = manager.add_reply_order(&order, None, &mut frame);
            }
        }

        impl ReplyMessageHandler for Reentrant {
            fn handle_message(
                &self,
                _source: &Address,
                _header: &UnpackedMessageHeader,
                _data: &mut BinaryStream,
                _arg: usize,
            ) {
                self.notes.borrow_mut().push("message");
                self.ask_again();
            }

            fn handle_exception(&self, _exception: &NubException, _arg: usize) {
                self.notes.borrow_mut().push("exception");
                self.ask_again();
            }
        }

        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = Rc::new(RequestManager::with_next_reply_id(
            false,
            310,
            Rc::clone(&dispatcher),
        ));
        let watcher = Rc::new(Reentrant {
            manager: Rc::downgrade(&manager),
            self_handler: std::cell::RefCell::new(None),
            notes: std::cell::RefCell::new(Vec::new()),
        });
        let watcher_handler = Rc::clone(&watcher) as Rc<dyn ReplyMessageHandler>;
        *watcher.self_handler.borrow_mut() = Some(Rc::downgrade(&watcher_handler));

        // A reply to the watcher's first request: the handler asks again
        // from the delivery hook, and the new request (its own, so the
        // cancellation below will find it) is simply pending.
        let order = ReplyOrder::new(Rc::clone(&watcher_handler), 0, DEFAULT_REQUEST_TIMEOUT, 2);
        let mut frame = [0u8; 12];
        assert_eq!(manager.add_reply_order(&order, None, &mut frame), Ok(310));
        reply(&manager, 310, &[1], &Address::new(1, 1));
        assert_eq!(*watcher.notes.borrow(), vec!["message"]);
        assert_eq!(manager.num_pending(), 1, "the second request (311)");

        // Now cancel the watcher outright: the failure hook asks again,
        // and the request it started (312) survives the cancellation that
        // triggered it.
        manager.cancel_requests_for_handler(&watcher_handler, Reason::TimerExpired);
        assert_eq!(*watcher.notes.borrow(), vec!["message", "exception"]);
        assert_eq!(manager.num_pending(), 1, "the third request (312)");
    }

    /// `failRequest` is public in the reference -- the request's timeout
    /// calls back into it -- so the port keeps it callable, and a request
    /// id that is already gone is no-op, not an error.
    #[test]
    fn fail_request_reports_once_and_tolerates_a_missing_id() {
        let (_clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 340, Rc::clone(&dispatcher));
        let handler = Rc::new(Recorder::default());
        let (id, _) = pending(&manager, &handler, 6, None);

        manager.fail_request(id, Reason::ClientDisconnected);
        assert_eq!(handler.failures(), vec![(Reason::ClientDisconnected, 6)]);
        assert_eq!(handler.answers(), Vec::<(usize, Vec<u8>)>::new());
        assert_eq!(manager.num_pending(), 0);

        // The same id again -- already gone -- costs nothing.
        manager.fail_request(id, Reason::ClientDisconnected);
        assert_eq!(handler.failures(), vec![(Reason::ClientDisconnected, 6)]);
    }

    /// A request started while the manager is being torn down hears
    /// nothing when its own timer fires: the teardown has already taken
    /// the shared state away, and the expiry closure finds nothing to
    /// fail. (The reference can re-enter through a raw pointer, where
    /// this port's weak handle fails first -- the silent orphan is the
    /// same observable outcome as its `requestMap_.clear()`.)
    #[test]
    fn a_request_added_while_dropping_hears_nothing() {
        struct DyingHook {
            /// The shared state and the dispatcher, so the hook can still
            /// reach the machinery mid-teardown -- a
            /// `Weak<RequestManager>` cannot, because its strong count is
            /// already zero inside the destructor. The state is handed
            /// back in the callback: holding onto it here would keep the
            /// machinery alive past the destructor it is dying with.
            inner: std::cell::RefCell<Option<Rc<Inner>>>,
            dispatcher: Rc<EventDispatcher>,
        }

        impl ReplyMessageHandler for DyingHook {
            fn handle_message(
                &self,
                _source: &Address,
                _header: &UnpackedMessageHeader,
                _data: &mut BinaryStream,
                _arg: usize,
            ) {
            }

            fn handle_exception(&self, _exception: &NubException, _arg: usize) {}

            fn handle_shutting_down(&self, _exception: &NubException, _arg: usize) {
                // The teardown callback starts one more request, the way
                // a careless handler might. It takes the state out of
                // itself rather than holding it, so when this hook ends
                // nothing but the dying manager still holds it.
                let inner = self.inner.borrow_mut().take();
                if let Some(inner) = inner.as_ref() {
                    let handler: Rc<dyn ReplyMessageHandler> = Rc::new(Recorder::default());
                    let order = ReplyOrder::new(handler, 7, 1, 2);
                    let mut frame = [0u8; 12];
                    let _ = add_reply_order_to(inner, &self.dispatcher, &order, None, &mut frame);
                }
            }
        }

        let (clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 350, Rc::clone(&dispatcher));
        let watcher = Rc::new(DyingHook {
            inner: std::cell::RefCell::new(Some(Rc::clone(&manager.inner))),
            dispatcher: Rc::clone(&dispatcher),
        });
        let order = ReplyOrder::new(
            Rc::clone(&watcher) as Rc<dyn ReplyMessageHandler>,
            7,
            DEFAULT_REQUEST_TIMEOUT,
            2,
        );
        let mut frame = [0u8; 12];
        assert_eq!(manager.add_reply_order(&order, None, &mut frame), Ok(350));

        // A second request for the same watcher: its shutdown hook runs
        // twice in the teardown, and the second run finds the state
        // already handed back.
        let order2 = ReplyOrder::new(
            Rc::clone(&watcher) as Rc<dyn ReplyMessageHandler>,
            7,
            DEFAULT_REQUEST_TIMEOUT,
            2,
        );
        let mut frame2 = [0u8; 12];
        assert_eq!(manager.add_reply_order(&order2, None, &mut frame2), Ok(351));

        // The unused hooks stay quiet, reachable but never called by the
        // teardown.
        watcher.handle_message(
            &Address::new(0, 0),
            &UnpackedMessageHeader::new(0xFF),
            &mut BinaryStream::new(),
            0,
        );
        watcher.handle_exception(&NubException::new(Reason::Success, None), 0);

        drop(manager);

        // The leftover request went away with the manager's state; its
        // one-microsecond timer still sits in the queue and fires into
        // the void -- the closure's weak handle upgrades to nothing.
        clock.set(clock.get() + 10);
        assert_eq!(
            dispatcher.process_timers(),
            1,
            "the orphaned timer fires, and finds nothing to fail"
        );
    }

    /// A timer that outlives its manager finds nothing to fail: the
    /// expiry closure holds the shared state only weakly.
    #[test]
    fn a_timer_outliving_its_manager_finds_nothing_to_fail() {
        let (clock, dispatcher) = pinned_dispatcher();
        let handler = Rc::new(Recorder::default());

        {
            let manager = RequestManager::with_next_reply_id(false, 320, Rc::clone(&dispatcher));
            pending_with_timeout(&manager, &handler, 1, 1_000);
            drop(manager);
        }

        clock.set(clock.get() + 2_000);
        assert_eq!(
            dispatcher.process_timers(),
            0,
            "the armed timeout was cancelled by the manager's destructor"
        );
        assert_eq!(handler.failures(), Vec::<(Reason, usize)>::new());
    }

    /// A short timeout fires at its own deadline, not at the default's:
    /// the microseconds the order names are the microseconds it waits.
    #[test]
    fn a_short_timeout_fires_at_its_own_deadline() {
        let (clock, dispatcher) = pinned_dispatcher();
        let manager = RequestManager::with_next_reply_id(false, 330, Rc::clone(&dispatcher));
        let handler = Rc::new(Recorder::default());
        let _ = pending_with_timeout(&manager, &handler, 5, 100);

        clock.set(clock.get() + 99);
        assert_eq!(dispatcher.process_timers(), 0, "not yet");

        clock.set(clock.get() + 1);
        assert_eq!(dispatcher.process_timers(), 1, "exactly at the deadline");
        assert_eq!(handler.failures(), vec![(Reason::TimerExpired, 5)]);
    }
}
