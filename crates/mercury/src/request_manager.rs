//! `RequestManager` (`lib/network/request_manager.cpp`): the half of RPC
//! that lives on the sending side, and the half that answers the replies.
//!
//! `add_reply_order` hands out a reply id and remembers who the answer
//! belongs to; `handle_message` is installed as the handler of
//! `InterfaceElement::REPLY` and matches an incoming reply back to that
//! request. The two halves meet on the id the reference writes into the
//! request frame at send time and reads back off the head of the reply.
//!
//! The request timeout and its cancellation are not here; they are
//! BW-02-02-04, which builds the timer onto the same `Request`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interfaces::InputMessageHandler;
use crate::message_framing::FrameError;
use crate::misc::{MessageID, ReplyID, REPLY_ID_MAX};
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

/// `DEFAULT_REQUEST_TIMEOUT` (`bundle.hpp:25`), in microseconds: five
/// seconds, the timeout every request that names no timeout of its own
/// gets.
pub const DEFAULT_REQUEST_TIMEOUT: i32 = 5_000_000;

/// `RequestManager`: the pending requests of one socket.
///
/// The reference derives from `InputMessageHandler` because it is
/// registered as the handler of `InterfaceElement::REPLY`; that derive is
/// the point of this port, so it is kept.
pub struct RequestManager {
    /// `requestMap_`: reply id to the request awaiting that reply. Ordered
    /// like the reference's `BW::map`, which its destructor walks to
    /// report what is still pending.
    requests: RefCell<BTreeMap<ReplyID, Request>>,
    /// `nextReplyID_`.
    next_reply_id: Cell<ReplyID>,
    /// `isExternal_`: external nubs check a reply's source, internal ones
    /// do not, because an offloaded channel answers from another address.
    is_external: bool,
}

impl RequestManager {
    /// `RequestManager( isExternal, dispatcher )`: the reference seeds the
    /// id counter from the clock so that two processes started together do
    /// not hand out the same ids in the same window.
    ///
    /// The dispatcher argument goes with the timeout item; this port has
    /// nothing to schedule yet.
    pub fn new(is_external: bool) -> Self {
        Self::with_next_reply_id(is_external, seed_next_reply_id(real_clock_micros()))
    }

    /// A manager whose next reply id is a fixed value, so a test can name
    /// the ids it will get.
    pub fn with_next_reply_id(is_external: bool, next_reply_id: ReplyID) -> Self {
        RequestManager {
            requests: RefCell::new(BTreeMap::new()),
            next_reply_id: Cell::new(next_reply_id),
            is_external,
        }
    }

    /// `getNextReplyID()`: the next id, wrapping back to one once it has
    /// passed the reference's `REPLY_ID_MAX`.
    fn get_next_reply_id(&self) -> ReplyID {
        let mut next = self.next_reply_id.get();
        if next > REPLY_ID_MAX {
            next = 1;
        }
        self.next_reply_id.set(next + 1);
        next
    }

    /// `addReplyOrder( replyOrder, pChannel )`: allocate a reply id for
    /// this request, remember the request, and write the id into the frame.
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
        let offset = order.reply_id_offset();
        if offset + IN_REPLY_TO_SIZE > frame.len() {
            return Err(FrameError::BufferTooSmall);
        }

        let reply_id = self.get_next_reply_id();
        self.requests
            .borrow_mut()
            .insert(reply_id, Request::new(reply_id, order, channel));

        // `*pReplyID = BW_HTONL( replyID )`: identity on this host, so
        // little-endian bytes.
        frame[offset..offset + IN_REPLY_TO_SIZE].copy_from_slice(&reply_id.to_le_bytes());

        Ok(reply_id)
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

        if !self.requests.borrow().contains_key(&in_reply_to) {
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
        let request = self
            .requests
            .borrow_mut()
            .remove(&in_reply_to)
            .expect("the request was present on the lookup above");
        request.handle_message(source, header, data);
    }

    /// The number of requests still awaiting a reply.
    pub fn num_pending(&self) -> usize {
        self.requests.borrow().len()
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

        fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
    }

    impl Recorder {
        fn answers(&self) -> Vec<(usize, Vec<u8>)> {
            self.delivered.borrow().clone()
        }
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
        let manager = RequestManager::with_next_reply_id(false, REPLY_ID_MAX - 1);
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
        let manager = RequestManager::with_next_reply_id(false, 10101);
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
        let manager = RequestManager::with_next_reply_id(false, 77);
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
        let manager = RequestManager::with_next_reply_id(false, 9);
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
        let manager = RequestManager::with_next_reply_id(false, 3);
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
        let manager = RequestManager::with_next_reply_id(false, 4);
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
        let manager = RequestManager::with_next_reply_id(false, 5);

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
            let manager = RequestManager::with_next_reply_id(external, 60);
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
        let manager = RequestManager::with_next_reply_id(true, 80);
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
        let manager = RequestManager::new(true);
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
        let manager = Rc::new(RequestManager::with_next_reply_id(true, 400));
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

        // The failure half of the same handler stays silent; requests fail
        // through it only from BW-02-02-04 on.
        handler.handle_exception(
            &NubException::new(crate::misc::Reason::TimerExpired, None),
            0,
        );
        assert_eq!(handler.answers(), vec![(0, vec![7])]);
    }
}
