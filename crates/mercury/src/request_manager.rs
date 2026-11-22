//! `Mercury::RequestManager` / `Request` / `ReplyOrder`
//! (`lib/network/request_manager.hpp/.cpp`, `request.hpp/.cpp`,
//! `reply_order.hpp`).
//!
//! The reply side of a request: every request message carries a unique
//! [`ReplyID`] in its preamble, and the sender registers a
//! [`ReplyMessageHandler`] for it. When the reply comes back (identified by
//! `REPLY_MESSAGE_IDENTIFIER`, with the reply id as the first four body
//! bytes), the manager looks the id up and hands the payload to the matching
//! handler -- that correlation is what lets interleaved requests each get
//! their own reply. The manager also owns the request lifecycle: a request
//! without a channel carries a once-off timeout timer, and the destructor
//! fails whatever is still pending.
//!
//! The reply id rides the wire in the same order the reference writes it.
//! The reference uses `BW_HTONL` (write) / `BW_NTOHL` (read) which are
//! identity on the little-endian hosts this port targets (BW-01-02-01 pins
//! the port's primitives little-endian), so this module uses the port's
//! little-endian `read_i32` / `write_i32`.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use foundation::stream::BinaryStream;
use foundation::time::stamp_now;
use foundation::time_queue::{TimerHandle, TimerHandlerFn};

use crate::endpoint::Address;
use crate::event_dispatcher::EventDispatcher;
use crate::interface_element::REPLY_MESSAGE_IDENTIFIER;
use crate::interfaces::{InputMessageHandler, ReplyMessageHandler};
use crate::misc::{REPLY_ID_MAX, Reason, ReplyID};
use crate::network_interface::ChannelRef;
use crate::nub_exception::NubException;
use crate::unpacked_message_header::UnpackedMessageHeader;

/// `Mercury::ReplyOrder` (`reply_order.hpp`): a request that needs a reply.
/// Built by the bundle's `startRequest` and handed to
/// [`RequestManager::add_reply_order`] at send time.
pub struct ReplyOrder<'a> {
    /// The user reply handler.
    pub handler: Rc<dyn ReplyMessageHandler>,
    /// User argument passed to the handler.
    pub arg: usize,
    /// Timeout in microseconds (only meaningful when there is no channel).
    pub microseconds: i32,
    /// Pointer to the reply id slot, written in `NetworkInterface::send()`;
    /// the four bytes the request preamble reserves for it.
    pub p_reply_id: &'a mut [u8],
}

/// `Mercury::Request`: the context a pending request needs to correlate its
/// reply and to time out.
///
/// The reference keeps `replyID_` alongside `requestMap_[ replyID ]`, but the
/// map key here is already the reply id and `failRequest` erases by key, so
/// the field is redundant in the port (the reference reads it only to erase).
struct Request {
    /// `pHandler_`.
    handler: Rc<dyn ReplyMessageHandler>,
    /// `arg_`.
    arg: usize,
    /// `pChannel_`: `None` for a once-off request.
    channel: Option<Rc<dyn ChannelRef>>,
    /// `timerHandle_`: the once-off timeout, set only when there is no
    /// channel (a channel does its own timeouts).
    timer_handle: Option<TimerHandle<u64>>,
}

impl Request {
    fn new(
        handler: Rc<dyn ReplyMessageHandler>,
        arg: usize,
        channel: Option<Rc<dyn ChannelRef>>,
        timer_handle: Option<TimerHandle<u64>>,
    ) -> Self {
        Self {
            handler,
            arg,
            channel,
            timer_handle,
        }
    }

    /// `matches( Channel * )`: same channel pointer.
    fn matches_channel(&self, channel: &Rc<dyn ChannelRef>) -> bool {
        self.channel
            .as_ref()
            .is_some_and(|c| Rc::ptr_eq(c, channel))
    }

    /// `matches( ReplyMessageHandler * )`: same handler pointer.
    fn matches_handler(&self, handler: &Rc<dyn ReplyMessageHandler>) -> bool {
        Rc::ptr_eq(&self.handler, handler)
    }

    /// `isValidSource`: with no channel any source is valid; with one, the
    /// reply must come from the channel's address.
    fn is_valid_source(&self, source: &Address) -> bool {
        self.channel.as_ref().is_none_or(|c| c.addr() == *source)
    }

    /// `handleMessage`: deliver the reply payload to the user handler, then
    /// release this request.
    fn handle_message(
        &mut self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        self.handler.handle_message(source, header, data, self.arg);
        self.finish();
    }

    /// `handleFailure`: the request failed -- deliver the exception to the
    /// user handler (or the shutdown notice), then release this request.
    fn handle_failure(&mut self, reason: Reason) {
        let exception = NubException::new(reason, crate::endpoint::NONE);
        if reason != Reason::ShuttingDown {
            self.handler.handle_exception(&exception, self.arg);
        } else {
            self.handler.handle_shutting_down(&exception, self.arg);
        }
        self.finish();
    }

    /// `finish`: release the once-off timer if one is held. In the reference
    /// this also deletes the request (via the timer's `onRelease` or a direct
    /// delete); here the request is owned by the manager's map and drops when
    /// the caller drops it, so `finish` only cancels the timer.
    fn finish(&mut self) {
        if let Some(timer) = &mut self.timer_handle {
            timer.cancel();
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        // Safety net: cancel the timer so a pending once-off cannot fire on a
        // request that has already been resolved.
        if let Some(timer) = &mut self.timer_handle {
            timer.cancel();
        }
    }
}

/// `Mercury::RequestManager`: correlates replies to the requests that asked
/// for them and dispatches each reply to its handler.
///
/// This is the handler for the internal `REPLY_MESSAGE_IDENTIFIER` message.
pub struct RequestManager {
    /// `isExternal_`: only external nubs enforce the source check.
    is_external: bool,
    /// `nextReplyID_`.
    next_reply_id: Cell<ReplyID>,
    /// `requestMap_`.
    request_map: RefCell<BTreeMap<ReplyID, Request>>,
    /// `dispatcher_`: where the once-off request timeouts are registered.
    dispatcher: Rc<EventDispatcher>,
}

impl RequestManager {
    /// `RequestManager( isExternal, dispatcher )`.
    pub fn new(is_external: bool, dispatcher: Rc<EventDispatcher>) -> Self {
        // The reference seeds from `( uint32(timestamp())%100000 ) + 10101`
        // so reply ids do not restart at 1 after a process restart.
        let seed = (stamp_now() % 100_000) as ReplyID + 10101;
        Self {
            is_external,
            next_reply_id: Cell::new(seed),
            request_map: RefCell::new(BTreeMap::new()),
            dispatcher,
        }
    }

    /// `getNextReplyID`: return the current id and advance, wrapping to 1
    /// once past [`REPLY_ID_MAX`].
    fn get_next_reply_id(&self) -> ReplyID {
        let mut next = self.next_reply_id.get();
        if next > REPLY_ID_MAX {
            next = 1;
        }
        self.next_reply_id.set(next + 1);
        next
    }

    /// `addReplyOrder`: allocate a reply id, write it into the request's
    /// preamble slot, register the handler, and (when there is no channel)
    /// arm a once-off timeout.
    ///
    /// Takes `&Rc<Self>` because the timeout handler needs a back-reference
    /// to fail the request when it fires; the reference passes the manager
    /// pointer as the timer's user argument, which the port cannot do
    /// without unsafe, so it captures a weak handle instead.
    pub fn add_reply_order(
        self: &Rc<Self>,
        reply_order: ReplyOrder,
        channel: Option<Rc<dyn ChannelRef>>,
    ) {
        let ReplyOrder {
            handler,
            arg,
            microseconds,
            p_reply_id,
        } = reply_order;

        let reply_id = self.get_next_reply_id();
        p_reply_id[..4].copy_from_slice(&(reply_id as u32).to_le_bytes());

        let timer_handle = if channel.is_none() {
            debug_assert!(
                microseconds > 0,
                "Request: a channel-less request must carry a positive timeout"
            );
            let weak = Rc::downgrade(self);
            let handler: Rc<dyn foundation::time_queue::TimerHandler<u64>> =
                Rc::new(TimerHandlerFn::new(
                    move |_handle, _user| {
                        if let Some(manager) = weak.upgrade() {
                            manager.fail_request(reply_id, Reason::TimerExpired);
                        }
                    },
                    "Request",
                ));
            Some(
                self.dispatcher
                    .add_once_off_timer(microseconds as i64, handler, 0, "Request"),
            )
        } else {
            None
        };

        self.request_map
            .borrow_mut()
            .insert(reply_id, Request::new(handler, arg, channel, timer_handle));
    }

    /// `handleMessage`: correlate a reply message to its pending request and
    /// dispatch the payload. Mirrors `RequestManager::handleMessage`.
    pub fn handle_message(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        if header.identifier != REPLY_MESSAGE_IDENTIFIER {
            eprintln!(
                "RequestManager::handleMessage( {} ): received the wrong kind of message!",
                source.c_str()
            );
            return;
        }

        if header.length < std::mem::size_of::<i32>() as i32 {
            eprintln!(
                "RequestManager::handleMessage( {} ): received a corrupted reply message (length {})!",
                source.c_str(),
                header.length
            );
            return;
        }

        let in_reply_to = data
            .read_i32()
            .expect("reply id present: guarded by length >= 4");
        header.length -= std::mem::size_of::<i32>() as i32;

        if !self.request_map.borrow().contains_key(&in_reply_to) {
            eprintln!(
                "RequestManager::handleMessage( {} ): Couldn't find handler for reply id 0x{:08x} (maybe it timed out?)",
                source.c_str(),
                in_reply_to
            );
            data.finish();
            return;
        }

        // Only external nubs enforce the source check: replies can come from
        // a different address on an internal nub if a channel was offloaded.
        if self.is_external {
            let valid = self
                .request_map
                .borrow()
                .get(&in_reply_to)
                .is_some_and(|r| r.is_valid_source(source));
            if !valid {
                eprintln!(
                    "RequestManager::handleMessage: Got reply to request {} from unexpected source {}",
                    in_reply_to,
                    source.c_str()
                );
                return;
            }
        }

        let mut request = self
            .request_map
            .borrow_mut()
            .remove(&in_reply_to)
            .expect("request present: checked above");
        request.handle_message(source, header, data);
    }

    /// `failRequest`: clean up a failed request and inform its handler.
    fn fail_request(&self, reply_id: ReplyID, reason: Reason) {
        if let Some(mut request) = self.request_map.borrow_mut().remove(&reply_id) {
            request.handle_failure(reason);
        }
    }

    /// `cancelRequestsFor( Channel * )`: fail every request on the channel
    /// with `REASON_CHANNEL_LOST`.
    pub fn cancel_requests_for_channel(&self, channel: &Rc<dyn ChannelRef>) {
        let ids: Vec<ReplyID> = self
            .request_map
            .borrow()
            .iter()
            .filter(|(_, request)| request.matches_channel(channel))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.fail_request(id, Reason::ChannelLost);
        }
    }

    /// `cancelRequestsFor( ReplyMessageHandler *, reason )`: fail every
    /// request whose handler is `handler` with `reason`.
    pub fn cancel_requests_for_handler(
        &self,
        handler: &Rc<dyn ReplyMessageHandler>,
        reason: Reason,
    ) {
        let ids: Vec<ReplyID> = self
            .request_map
            .borrow()
            .iter()
            .filter(|(_, request)| request.matches_handler(handler))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.fail_request(id, reason);
        }
    }
}

/// The request manager is the handler for the internal
/// `REPLY_MESSAGE_IDENTIFIER` message: an interface serves it as the reply
/// element (`network_interface.cpp`: `interfaceTable().serve(
/// InterfaceElement::REPLY, pRequestManager_ )`), and the interface table
/// delivers every reply message to [`handle_message`](Self::handle_message).
///
/// The sizing half is never consulted: the reply element is
/// `VARIABLE_LENGTH_MESSAGE` with a 4-byte length header, so
/// `updateLengthDetails` passes a non-callback element straight through.
impl InputMessageHandler for RequestManager {
    fn handle_message(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        RequestManager::handle_message(self, source, header, data);
    }
}

impl Drop for RequestManager {
    fn drop(&mut self) {
        if !self.request_map.borrow().is_empty() {
            eprintln!(
                "RequestManager::~RequestManager: Num pending reply handlers = {}",
                self.request_map.borrow().len()
            );
        }
        let pending = std::mem::take(&mut *self.request_map.borrow_mut());
        for (_, mut request) in pending {
            request.handle_failure(Reason::ShuttingDown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use crate::endpoint::Address;
    use crate::interface_element::InterfaceElement;
    use crate::interface_table::InterfaceTable;

    /// A pinned clock: once-off request timeouts only fire when a test
    /// advances it and calls `process_timers`.
    fn pinned_dispatcher(at: u64) -> (Rc<EventDispatcher>, Rc<Cell<u64>>) {
        let clock = Rc::new(Cell::new(at));
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new({
            let clock = clock.clone();
            move || clock.get()
        })));
        (dispatcher, clock)
    }

    /// A request manager on a pinned clock, so the timeout behaviour is
    /// deterministic.
    fn manager_at(is_external: bool, at: u64) -> (Rc<RequestManager>, Rc<Cell<u64>>) {
        let (dispatcher, clock) = pinned_dispatcher(at);
        (Rc::new(RequestManager::new(is_external, dispatcher)), clock)
    }

    /// Records everything the reply handler is given.
    #[derive(Default)]
    struct RecordingHandler {
        /// `(arg, payload)` for each delivered reply.
        payloads: RefCell<Vec<(usize, Vec<u8>)>>,
        /// The source of each delivered reply.
        sources: RefCell<Vec<Address>>,
        /// `(reason, arg)` for each exception.
        exceptions: RefCell<Vec<(Reason, usize)>>,
        /// `(reason, arg)` for each shutdown notice.
        shutdowns: RefCell<Vec<(Reason, usize)>>,
    }

    impl RecordingHandler {
        fn new() -> Rc<Self> {
            Rc::new(Self::default())
        }
    }

    /// The handler as a reply-handler trait object. Unsizing the same `Rc`
    /// keeps the allocation, so every call yields an `Rc` that `Rc::ptr_eq`
    /// matches the one `add_reply_order` stored.
    fn trait_ref(h: &Rc<RecordingHandler>) -> Rc<dyn ReplyMessageHandler> {
        h.clone()
    }

    impl ReplyMessageHandler for RecordingHandler {
        fn handle_message(
            &self,
            source: &Address,
            _header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
            arg: usize,
        ) {
            self.payloads
                .borrow_mut()
                .push((arg, data.unread_bytes().to_vec()));
            self.sources.borrow_mut().push(*source);
        }
        fn handle_exception(&self, exception: &NubException, arg: usize) {
            self.exceptions.borrow_mut().push((exception.reason(), arg));
        }
        fn handle_shutting_down(&self, exception: &NubException, arg: usize) {
            self.shutdowns.borrow_mut().push((exception.reason(), arg));
        }
    }

    /// A channel whose address is settable, so the external source check can
    /// be driven.
    struct FakeChannel {
        addr: Address,
        failed: Cell<bool>,
    }

    impl ChannelRef for FakeChannel {
        fn c_str(&self) -> String {
            "fake-channel".to_string()
        }
        fn is_internal(&self) -> bool {
            true
        }
        fn has_remote_failed(&self) -> bool {
            self.failed.get()
        }
        fn set_remote_failed(&self) {
            self.failed.set(true);
        }
        fn addr(&self) -> Address {
            self.addr
        }
    }

    fn source_a() -> Address {
        Address::new(0x7f00_0001, 40000)
    }

    fn source_b() -> Address {
        Address::new(0x7f00_0001, 40001)
    }

    /// Build a reply body: the reply id, then `payload`.
    fn reply_body(reply_id: i32, payload: &[u8]) -> BinaryStream {
        let mut data = BinaryStream::new();
        data.write_i32(reply_id);
        data.write_bytes(payload);
        data.rewind();
        data
    }

    /// A reply header for `body`: the whole body length (reply id included).
    fn reply_header(body: &BinaryStream) -> UnpackedMessageHeader {
        UnpackedMessageHeader {
            identifier: REPLY_MESSAGE_IDENTIFIER,
            reply_id: UnpackedMessageHeader::NO_REPLY,
            length: body.length() as i32,
        }
    }

    /// Deliver `payload` as the reply to `reply_id` to the manager.
    fn deliver(manager: &Rc<RequestManager>, reply_id: i32, payload: &[u8]) {
        let mut body = reply_body(reply_id, payload);
        let mut header = reply_header(&body);
        manager.handle_message(&source_a(), &mut header, &mut body);
    }

    /// Decode the reply id the manager wrote into a preamble slot.
    fn slot_reply_id(slot: &[u8]) -> i32 {
        i32::from_le_bytes([slot[0], slot[1], slot[2], slot[3]])
    }

    fn request_order(
        handler: Rc<dyn ReplyMessageHandler>,
        arg: usize,
        slot: &mut [u8],
    ) -> ReplyOrder<'_> {
        ReplyOrder {
            handler,
            arg,
            microseconds: 1_000_000,
            p_reply_id: slot,
        }
    }

    /// The acceptance test for BW-02-02-05: the request manager is served
    /// on the interface table as the reply element's handler, and the
    /// table's dispatch arm validates and delivers a reply to it -- the
    /// registration the reference wires in the `NetworkInterface`
    /// constructor (`interfaceTable().serve( InterfaceElement::REPLY,
    /// pRequestManager_ )`).
    #[test]
    fn serving_the_request_manager_on_the_table_delivers_replies() {
        let (manager, _clock) = manager_at(false, 0);
        let table = InterfaceTable::new();

        table.serve(
            &InterfaceElement::reply(),
            Some(Rc::clone(&manager) as Rc<dyn InputMessageHandler>),
        );

        // The reply element is served, so its id resolves to a handler.
        let (_, input_handler) = table
            .resolve_message(REPLY_MESSAGE_IDENTIFIER)
            .expect("reply served");

        let handler = RecordingHandler::new();
        let mut slot = [0u8; 4];
        manager.add_reply_order(request_order(trait_ref(&handler), 42, &mut slot), None);
        let reply_id = slot_reply_id(&slot);

        let mut body = reply_body(reply_id, b"payload");
        let mut header = reply_header(&body);
        input_handler.handle_message(&source_a(), &mut header, &mut body);

        assert_eq!(
            handler.payloads.borrow().as_slice(),
            &[(42, b"payload".to_vec())]
        );
        assert_eq!(
            *handler.sources.borrow().first().expect("delivered"),
            source_a()
        );
    }

    /// The acceptance test for BW-02-02-03: interleaved requests each receive
    /// their own reply, even when the replies arrive out of order.
    #[test]
    fn interleaved_requests_each_receive_their_own_reply() {
        let (manager, _clock) = manager_at(false, 0);

        let h0 = RecordingHandler::new();
        let h1 = RecordingHandler::new();
        let h2 = RecordingHandler::new();
        let mut slot0 = [0u8; 4];
        let mut slot1 = [0u8; 4];
        let mut slot2 = [0u8; 4];

        // Issue the requests in order, then reply in reverse order.
        manager.add_reply_order(request_order(trait_ref(&h0), 10, &mut slot0), None);
        manager.add_reply_order(request_order(trait_ref(&h1), 20, &mut slot1), None);
        manager.add_reply_order(request_order(trait_ref(&h2), 30, &mut slot2), None);

        let id0 = slot_reply_id(&slot0);
        let id1 = slot_reply_id(&slot1);
        let id2 = slot_reply_id(&slot2);

        // The reply ids are unique.
        assert_ne!(id0, id1);
        assert_ne!(id1, id2);
        assert_ne!(id0, id2);

        // Replies arrive in reverse order, each with a distinct payload.
        deliver(&manager, id2, &[0xCC]);
        deliver(&manager, id1, &[0xBB]);
        deliver(&manager, id0, &[0xAA]);

        // Each request got exactly its own reply and its own arg.
        assert_eq!(h2.payloads.borrow().as_slice(), &[(30, vec![0xCC])]);
        assert_eq!(h1.payloads.borrow().as_slice(), &[(20, vec![0xBB])]);
        assert_eq!(h0.payloads.borrow().as_slice(), &[(10, vec![0xAA])]);
        assert_eq!(h0.sources.borrow().as_slice(), &[source_a()]);

        // Every request is resolved: replaying an id finds no handler.
        deliver(&manager, id0, &[0x00]);
        assert_eq!(h0.payloads.borrow().as_slice(), &[(10, vec![0xAA])]);
    }

    /// A reply for an id nobody is waiting on is dropped, not dispatched.
    #[test]
    fn a_reply_to_an_unknown_id_is_not_dispatched() {
        let (manager, _clock) = manager_at(false, 0);
        let h = RecordingHandler::new();

        manager.add_reply_order(request_order(trait_ref(&h), 7, &mut [0u8; 4]), None);
        deliver(&manager, 999_999, &[0x01]);

        assert!(h.payloads.borrow().is_empty());
        assert!(h.exceptions.borrow().is_empty());
    }

    /// A message that is not the reply identifier is not dispatched.
    #[test]
    fn a_non_reply_message_is_not_dispatched() {
        let (manager, _clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let mut slot = [0u8; 4];

        manager.add_reply_order(request_order(trait_ref(&h), 7, &mut slot), None);
        let id = slot_reply_id(&slot);

        let mut body = reply_body(id, &[0x01]);
        let mut header = UnpackedMessageHeader {
            identifier: 0x01,
            ..reply_header(&body)
        };
        manager.handle_message(&source_a(), &mut header, &mut body);

        assert!(h.payloads.borrow().is_empty());
    }

    /// A reply body shorter than the reply id is rejected, not dispatched.
    #[test]
    fn a_short_reply_body_is_not_dispatched() {
        let (manager, _clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let mut slot = [0u8; 4];

        manager.add_reply_order(request_order(trait_ref(&h), 7, &mut slot), None);
        let id = slot_reply_id(&slot);

        let mut body = BinaryStream::new();
        body.write_i32(id); // only the reply id, nothing else
        body.rewind();
        let mut header = UnpackedMessageHeader {
            identifier: REPLY_MESSAGE_IDENTIFIER,
            reply_id: UnpackedMessageHeader::NO_REPLY,
            length: 0, // a claimed length of 0 is corrupted
        };
        manager.handle_message(&source_a(), &mut header, &mut body);

        assert!(h.payloads.borrow().is_empty());
    }

    /// An external nub only accepts a reply from the channel's address; a
    /// wrong-source reply leaves the request pending for the right source.
    #[test]
    fn an_external_nub_rejects_a_reply_from_the_wrong_source() {
        let (manager, _clock) = manager_at(true, 0);
        let h = RecordingHandler::new();
        let mut slot = [0u8; 4];
        let channel = Rc::new(FakeChannel {
            addr: source_a(),
            failed: Cell::new(false),
        });

        manager.add_reply_order(request_order(trait_ref(&h), 7, &mut slot), Some(channel));
        let id = slot_reply_id(&slot);

        // The wrong source is warned and the request stays pending.
        let mut body = reply_body(id, &[0x01]);
        let mut header = reply_header(&body);
        manager.handle_message(&source_b(), &mut header, &mut body);
        assert!(h.payloads.borrow().is_empty());

        // The right source dispatches it.
        deliver(&manager, id, &[0x01]);
        assert_eq!(h.payloads.borrow().as_slice(), &[(7, vec![0x01])]);
    }

    /// An internal nub does not enforce the source check even on a channel.
    #[test]
    fn an_internal_nub_accepts_a_reply_from_any_source() {
        let (manager, _clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let mut slot = [0u8; 4];
        let channel = Rc::new(FakeChannel {
            addr: source_a(),
            failed: Cell::new(false),
        });

        manager.add_reply_order(request_order(trait_ref(&h), 7, &mut slot), Some(channel));
        let id = slot_reply_id(&slot);

        let mut body = reply_body(id, &[0x01]);
        let mut header = reply_header(&body);
        manager.handle_message(&source_b(), &mut header, &mut body);

        assert_eq!(h.payloads.borrow().as_slice(), &[(7, vec![0x01])]);
    }

    /// A channel-less request times out: the once-off timer fires the
    /// exception exactly once and the request is released.
    #[test]
    fn a_channelless_request_times_out_once() {
        let (manager, clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let mut slot = [0u8; 4];

        manager.add_reply_order(
            ReplyOrder {
                handler: trait_ref(&h),
                arg: 42,
                microseconds: 1000,
                p_reply_id: &mut slot,
            },
            None,
        );
        let id = slot_reply_id(&slot);

        clock.set(1000);
        let fired = manager.dispatcher.process_timers();
        assert_eq!(fired, 1);
        assert_eq!(
            h.exceptions.borrow().as_slice(),
            &[(Reason::TimerExpired, 42)]
        );

        // The request is gone: a late reply finds no handler.
        deliver(&manager, id, &[0x01]);
        assert!(h.payloads.borrow().is_empty());

        // It fires only once.
        clock.set(2000);
        assert_eq!(manager.dispatcher.process_timers(), 0);
        assert_eq!(h.exceptions.borrow().len(), 1);
    }

    /// A request on a channel carries no once-off timer: the channel is
    /// responsible for timeouts, so nothing fires on its own.
    #[test]
    fn a_channel_request_carries_no_timer() {
        let (manager, clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let mut slot = [0u8; 4];
        let channel = Rc::new(FakeChannel {
            addr: source_a(),
            failed: Cell::new(false),
        });

        manager.add_reply_order(
            ReplyOrder {
                handler: trait_ref(&h),
                arg: 7,
                microseconds: 0, // not meaningful on a channel
                p_reply_id: &mut slot,
            },
            Some(channel),
        );
        let id = slot_reply_id(&slot);

        clock.set(10_000_000);
        assert_eq!(manager.dispatcher.process_timers(), 0);
        assert!(h.exceptions.borrow().is_empty());

        // The request is still pending and dispatches normally.
        deliver(&manager, id, &[0x01]);
        assert_eq!(h.payloads.borrow().as_slice(), &[(7, vec![0x01])]);
    }

    /// Cancelling a channel fails only the requests on that channel.
    #[test]
    fn cancelling_a_channel_fails_only_its_requests() {
        let (manager, _clock) = manager_at(false, 0);
        let h_a = RecordingHandler::new();
        let h_b = RecordingHandler::new();
        let chan_a: Rc<dyn ChannelRef> = Rc::new(FakeChannel {
            addr: source_a(),
            failed: Cell::new(false),
        });
        let chan_b: Rc<dyn ChannelRef> = Rc::new(FakeChannel {
            addr: source_b(),
            failed: Cell::new(false),
        });

        manager.add_reply_order(
            request_order(trait_ref(&h_a), 1, &mut [0u8; 4]),
            Some(chan_a.clone()),
        );
        manager.add_reply_order(
            request_order(trait_ref(&h_a), 2, &mut [0u8; 4]),
            Some(chan_a.clone()),
        );
        manager.add_reply_order(
            request_order(trait_ref(&h_b), 3, &mut [0u8; 4]),
            Some(chan_b.clone()),
        );

        manager.cancel_requests_for_channel(&chan_a);

        assert_eq!(
            h_a.exceptions.borrow().as_slice(),
            &[(Reason::ChannelLost, 1), (Reason::ChannelLost, 2)]
        );
        assert!(h_b.exceptions.borrow().is_empty());
    }

    /// Cancelling a handler fails all of that handler's requests with the
    /// given reason.
    #[test]
    fn cancelling_a_handler_fails_all_its_requests() {
        let (manager, _clock) = manager_at(false, 0);
        let h_a = RecordingHandler::new();
        let h_b = RecordingHandler::new();

        manager.add_reply_order(request_order(trait_ref(&h_a), 1, &mut [0u8; 4]), None);
        manager.add_reply_order(request_order(trait_ref(&h_a), 2, &mut [0u8; 4]), None);
        manager.add_reply_order(request_order(trait_ref(&h_b), 3, &mut [0u8; 4]), None);

        manager.cancel_requests_for_handler(&trait_ref(&h_a), Reason::NoSuchPort);

        assert_eq!(
            h_a.exceptions.borrow().as_slice(),
            &[(Reason::NoSuchPort, 1), (Reason::NoSuchPort, 2)]
        );
        assert!(h_b.exceptions.borrow().is_empty());
    }

    /// Dropping the manager fails still-pending requests with a shutdown
    /// notice.
    #[test]
    fn dropping_the_manager_fails_pending_requests_with_shutting_down() {
        let h = RecordingHandler::new();
        {
            let (manager, _clock) = manager_at(false, 0);
            manager.add_reply_order(request_order(trait_ref(&h), 9, &mut [0u8; 4]), None);
            manager.add_reply_order(request_order(trait_ref(&h), 8, &mut [0u8; 4]), None);
            // `manager` drops here, failing both requests.
        }

        assert_eq!(
            h.shutdowns.borrow().as_slice(),
            &[(Reason::ShuttingDown, 9), (Reason::ShuttingDown, 8)]
        );
        assert!(h.exceptions.borrow().is_empty());
    }

    /// The reply id counter wraps to 1 once past `REPLY_ID_MAX`. The
    /// reference hands out `REPLY_ID_MAX` itself and wraps only when the
    /// counter sits *above* it, so the sequence from `REPLY_ID_MAX` is
    /// `REPLY_ID_MAX`, `1`, `2`.
    #[test]
    fn reply_ids_wrap_after_the_max() {
        let (manager, _clock) = manager_at(false, 0);
        manager.next_reply_id.set(REPLY_ID_MAX);

        let next_id = || -> i32 {
            let mut slot = [0u8; 4];
            manager.add_reply_order(
                request_order(trait_ref(&RecordingHandler::new()), 7, &mut slot),
                None,
            );
            slot_reply_id(&slot)
        };

        assert_eq!(next_id(), REPLY_ID_MAX);
        assert_eq!(next_id(), 1);
        assert_eq!(next_id(), 2);
    }

    /// The fake channel's identity methods answer as the test channel they
    /// stand in for.
    #[test]
    fn a_fake_channel_reports_its_identity() {
        let ch = FakeChannel {
            addr: source_a(),
            failed: Cell::new(false),
        };
        assert_eq!(ch.c_str(), "fake-channel");
        assert!(ch.is_internal());
        assert!(!ch.has_remote_failed());
        ch.set_remote_failed();
        assert!(ch.has_remote_failed());
        assert_eq!(ch.addr(), source_a());
    }

    // BW-02-02-04: exactly-once and pending-state release. A request must
    // deliver exactly one callback (message, exception, or shutdown) no
    // matter which of the timeout / reply / cancel / drop paths wins, and
    // once resolved its pending state (map entry and once-off timer) is gone.

    /// A reply resolves the request *and* releases its pending once-off timer:
    /// advancing past the timeout afterwards fires nothing.
    #[test]
    fn a_reply_releases_the_pending_timer() {
        let (manager, clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let mut slot = [0u8; 4];

        manager.add_reply_order(
            ReplyOrder {
                handler: trait_ref(&h),
                arg: 5,
                microseconds: 1000,
                p_reply_id: &mut slot,
            },
            None,
        );
        let id = slot_reply_id(&slot);

        deliver(&manager, id, &[0x77]);
        assert_eq!(h.payloads.borrow().as_slice(), &[(5, vec![0x77])]);

        // The timer that was armed at add-time is now cancelled.
        clock.set(5000);
        assert_eq!(manager.dispatcher.process_timers(), 0);
        assert!(h.exceptions.borrow().is_empty());
        assert_eq!(h.payloads.borrow().len(), 1);
    }

    /// A reply after the timeout does not call the handler a second time: the
    /// timeout already released the request, so the late reply is dropped.
    #[test]
    fn a_reply_after_a_timeout_does_not_call_back_twice() {
        let (manager, clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let mut slot = [0u8; 4];

        manager.add_reply_order(
            ReplyOrder {
                handler: trait_ref(&h),
                arg: 5,
                microseconds: 1000,
                p_reply_id: &mut slot,
            },
            None,
        );
        let id = slot_reply_id(&slot);

        clock.set(1000);
        manager.dispatcher.process_timers();
        assert_eq!(
            h.exceptions.borrow().as_slice(),
            &[(Reason::TimerExpired, 5)]
        );

        deliver(&manager, id, &[0x77]);
        assert_eq!(
            h.exceptions.borrow().len(),
            1,
            "timeout callback stays once"
        );
        assert!(h.payloads.borrow().is_empty());
    }

    /// Cancelling a channel after the request already resolved does not call
    /// the handler again.
    #[test]
    fn a_cancel_after_a_reply_does_not_call_back() {
        let (manager, _clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let channel: Rc<dyn ChannelRef> = Rc::new(FakeChannel {
            addr: source_a(),
            failed: Cell::new(false),
        });
        let mut slot = [0u8; 4];

        manager.add_reply_order(
            request_order(trait_ref(&h), 5, &mut slot),
            Some(channel.clone()),
        );
        let id = slot_reply_id(&slot);

        // Resolve by reply, then try to cancel the same channel.
        deliver(&manager, id, &[0x77]);
        assert_eq!(h.payloads.borrow().as_slice(), &[(5, vec![0x77])]);

        manager.cancel_requests_for_channel(&channel);
        assert!(h.exceptions.borrow().is_empty());
        assert_eq!(h.payloads.borrow().len(), 1);
    }

    /// Cancelling a channel twice fails each of its requests once.
    #[test]
    fn cancelling_a_channel_twice_fails_each_request_once() {
        let (manager, _clock) = manager_at(false, 0);
        let h = RecordingHandler::new();
        let channel: Rc<dyn ChannelRef> = Rc::new(FakeChannel {
            addr: source_a(),
            failed: Cell::new(false),
        });

        manager.add_reply_order(
            request_order(trait_ref(&h), 5, &mut [0u8; 4]),
            Some(channel.clone()),
        );

        manager.cancel_requests_for_channel(&channel);
        assert_eq!(
            h.exceptions.borrow().as_slice(),
            &[(Reason::ChannelLost, 5)]
        );

        // The second cancel finds nothing left to fail.
        manager.cancel_requests_for_channel(&channel);
        assert_eq!(
            h.exceptions.borrow().as_slice(),
            &[(Reason::ChannelLost, 5)]
        );
    }

    /// A timeout then a cancel does not call back again: the timeout already
    /// released the request, so the cancel finds nothing left to fail.
    #[test]
    fn a_cancel_after_a_timeout_does_not_call_back() {
        let (manager, clock) = manager_at(false, 0);
        let h = RecordingHandler::new();

        manager.add_reply_order(
            ReplyOrder {
                handler: trait_ref(&h),
                arg: 5,
                microseconds: 1000,
                p_reply_id: &mut [0u8; 4],
            },
            None,
        );

        clock.set(1000);
        manager.dispatcher.process_timers();
        assert_eq!(
            h.exceptions.borrow().as_slice(),
            &[(Reason::TimerExpired, 5)]
        );

        manager.cancel_requests_for_handler(&trait_ref(&h), Reason::NoSuchPort);
        assert_eq!(
            h.exceptions.borrow().as_slice(),
            &[(Reason::TimerExpired, 5)],
            "the timeout callback stays once"
        );
    }

    /// Dropping the manager only fails still-pending requests: one that already
    /// timed out gets no shutdown notice, and one already answered gets no
    /// callback at all.
    #[test]
    fn dropping_the_manager_after_resolution_sends_no_notice() {
        let h = RecordingHandler::new();
        let (manager, clock) = manager_at(false, 0);

        manager.add_reply_order(
            ReplyOrder {
                handler: trait_ref(&h),
                arg: 5,
                microseconds: 1000,
                p_reply_id: &mut [0u8; 4],
            },
            None,
        );

        clock.set(1000);
        manager.dispatcher.process_timers();
        assert_eq!(
            h.exceptions.borrow().as_slice(),
            &[(Reason::TimerExpired, 5)]
        );

        // `manager` drops here with nothing pending.
        drop(manager);

        assert_eq!(
            h.exceptions.borrow().as_slice(),
            &[(Reason::TimerExpired, 5)],
            "the timeout callback stays once"
        );
        assert!(
            h.shutdowns.borrow().is_empty(),
            "a resolved request takes no shutdown notice"
        );
    }
}
