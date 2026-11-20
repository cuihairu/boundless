//! `Request` (`lib/network/request.hpp/.cpp`): the context of one
//! outstanding request, the thing a reply is matched back to.
//!
//! The reference is a `TimerHandler` because a request with no channel
//! owns a one-off timeout timer. Here the timer handle rides on the
//! request too, but arming it stays with the manager: the expiry callback
//! holds the manager's shared state (the reference passes
//! `pRequestManager` as the timer's user argument), which only exists at
//! the manager. The request carries the handle so an answer or a failure
//! can cancel the timeout before it fires -- a callback must happen
//! exactly once.

use std::rc::Rc;

use foundation::stream::BinaryStream;
use foundation::time_queue::TimerHandle;

use crate::endpoint::Address;
use crate::interfaces::ReplyMessageHandler;
use crate::misc::{Reason, ReplyID};
use crate::nub_exception::NubException;
use crate::reply_order::ReplyOrder;
use crate::unpacked_message_header::UnpackedMessageHeader;

/// `Request`: one request awaiting its reply.
pub struct Request {
    /// `replyID_`: the id the reply answers to.
    reply_id: ReplyID,
    /// `pHandler_`: the user reply handler.
    handler: Rc<dyn ReplyMessageHandler>,
    /// `arg_`: the user argument handed to the handler.
    arg: usize,
    /// The channel address a reply must come from, when there is a
    /// channel. `None` means no channel, so any source answers.
    ///
    /// The reference holds a `Channel *` and reads `addr()` off it; the
    /// channel object does not exist until BW-02-03, so only the address
    /// it would be asked for is kept.
    channel: Option<Address>,
    /// `timerHandle_`: the one-off timeout, armed only for a request with
    /// no channel (the reference arms it in its constructor).
    timer: TimerHandle<u64>,
}

impl Request {
    /// `Request( replyID, replyOrder, pChannel, ... )`: fill out the
    /// request from the reply order and the channel it rides. The timeout
    /// timer, if there is one, is armed afterwards by the manager.
    ///
    /// `pChannel` is the channel's address, or `None` for a request with
    /// no channel.
    pub fn new(reply_id: ReplyID, order: &ReplyOrder, channel: Option<Address>) -> Self {
        Request {
            reply_id,
            handler: order.handler(),
            arg: order.arg(),
            channel,
            timer: TimerHandle::default(),
        }
    }

    /// `replyID()`.
    pub fn reply_id(&self) -> ReplyID {
        self.reply_id
    }

    /// The reply handler, shared so the manager can drop its own copy.
    pub fn handler(&self) -> Rc<dyn ReplyMessageHandler> {
        self.handler.clone()
    }

    /// `arg_`.
    pub fn arg(&self) -> usize {
        self.arg
    }

    /// Arm the timeout; the manager calls this for a request with no
    /// channel, standing in for the reference doing it in the
    /// constructor.
    pub(crate) fn set_timer(&mut self, handle: TimerHandle<u64>) {
        self.timer = handle;
    }

    /// `finish()`'s `timerHandle_.cancel()`: take the timeout back before
    /// the request goes away, so it can never fire on a request that is
    /// already gone.
    pub(crate) fn cancel_timer(&mut self) {
        self.timer.cancel();
    }

    /// `matches( pChannel )`: pointer identity on the channel. Channels
    /// do not exist until BW-02-03, and Mercury keys a channel by its
    /// remote address (`findChannel`), so the address is the identity
    /// here.
    pub fn matches_channel(&self, channel: &Address) -> bool {
        self.channel.as_ref() == Some(channel)
    }

    /// `matches( pHandler )`: pointer identity on the reply handler.
    pub fn matches_handler(&self, handler: &Rc<dyn ReplyMessageHandler>) -> bool {
        Rc::ptr_eq(&self.handler, handler)
    }

    /// `isValidSource( source )`: `( pChannel_ == NULL ) || ( source ==
    /// pChannel_->addr() )`. With no channel any source answers.
    pub fn is_valid_source(&self, source: &Address) -> bool {
        match self.channel {
            None => true,
            Some(channel) => source == &channel,
        }
    }

    /// `handleMessage`: deliver the reply to the handler. The reference
    /// then calls `finish()`, which drops the request; the manager does
    /// that by removing it from its map and cancelling the timeout, so
    /// this only makes the call.
    pub fn handle_message(
        &self,
        source: &Address,
        header: &UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        self.handler.handle_message(source, header, data, self.arg);
    }

    /// `handleFailure( reason )`: the request failed rather than
    /// answered. A normal failure reaches the handler's exception hook; a
    /// `ShuttingDown` failure reaches the shutdown hook instead, because
    /// Mercury going down is not the request's fault.
    pub fn handle_failure(&self, reason: Reason) {
        let exception = NubException::new(reason, None);
        if reason == Reason::ShuttingDown {
            self.handler.handle_shutting_down(&exception, self.arg);
        } else {
            self.handler.handle_exception(&exception, self.arg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::ReplyMessageHandler;
    use std::cell::RefCell;

    /// A reply handler that records which hook it was called through.
    #[derive(Default)]
    struct Noting {
        /// `( hook, arg )` pairs, in call order. `0` is the reply hook,
        /// `1` the exception hook, `2` the shutdown hook.
        notes: RefCell<Vec<(&'static str, usize)>>,
    }

    impl Noting {
        fn notes(&self) -> Vec<(&'static str, usize)> {
            self.notes.borrow().clone()
        }
    }

    impl ReplyMessageHandler for Noting {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &UnpackedMessageHeader,
            _data: &mut BinaryStream,
            arg: usize,
        ) {
            self.notes.borrow_mut().push(("message", arg));
        }

        fn handle_exception(&self, _exception: &NubException, arg: usize) {
            self.notes.borrow_mut().push(("exception", arg));
        }

        fn handle_shutting_down(&self, _exception: &NubException, arg: usize) {
            self.notes.borrow_mut().push(("shutting down", arg));
        }
    }

    struct Silent;

    impl ReplyMessageHandler for Silent {
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

    fn order() -> ReplyOrder {
        let handler: Rc<dyn ReplyMessageHandler> = Rc::new(Silent);
        ReplyOrder::new(handler, 3, 5000000, 5)
    }

    fn noting_order(handler: &Rc<Noting>) -> ReplyOrder {
        let handler: Rc<dyn ReplyMessageHandler> = handler.clone();
        ReplyOrder::new(handler, 6, 5000000, 5)
    }

    /// The request remembers the reply id and argument the reply order
    /// carried.
    #[test]
    fn a_request_takes_the_id_argument_and_handler_of_its_order() {
        let request = Request::new(42, &order(), None);
        assert_eq!(request.reply_id(), 42);
        assert_eq!(request.arg(), 3);
    }

    /// `isValidSource`: with no channel any source answers, with a channel
    /// only the matching address does.
    #[test]
    fn the_source_is_validated_against_the_channel_address() {
        let channel = Address::new(0x7f000001, 2000);

        let no_channel = Request::new(1, &order(), None);
        assert!(no_channel.is_valid_source(&Address::new(0x08080808, 1)));

        let on_channel = Request::new(2, &order(), Some(channel));
        assert!(on_channel.is_valid_source(&channel));
        assert!(!on_channel.is_valid_source(&Address::new(0x7f000001, 2001)));
    }

    /// The request shares the handler its order named, ready to take the
    /// reply or its failure.
    #[test]
    fn the_request_hands_out_its_handler() {
        let request = Request::new(9, &order(), None);
        let handler = request.handler();
        handler.handle_message(
            &Address::new(0, 0),
            &UnpackedMessageHeader::new(0xFF),
            &mut BinaryStream::new(),
            request.arg(),
        );
        handler.handle_exception(
            &NubException::new(crate::misc::Reason::TimerExpired, None),
            request.arg(),
        );
    }

    /// `handleFailure` sends a normal failure to the exception hook, with
    /// the reason carried on the exception.
    #[test]
    fn a_normal_failure_reaches_the_exception_hook() {
        let handler = Rc::new(Noting::default());
        let order = ReplyOrder::new(
            Rc::clone(&handler) as Rc<dyn ReplyMessageHandler>,
            6,
            5000000,
            5,
        );
        let request = Request::new(1, &order, None);
        request.handle_failure(Reason::TimerExpired);
        assert_eq!(handler.notes(), vec![("exception", 6)]);
    }

    /// A `ShuttingDown` failure is not the request's fault, so it reaches
    /// the shutdown hook and only that.
    #[test]
    fn a_shutting_down_failure_reaches_the_shutdown_hook() {
        let handler = Rc::new(Noting::default());
        let order = ReplyOrder::new(
            Rc::clone(&handler) as Rc<dyn ReplyMessageHandler>,
            7,
            5000000,
            5,
        );
        let request = Request::new(2, &order, None);
        request.handle_failure(Reason::ShuttingDown);
        assert_eq!(handler.notes(), vec![("shutting down", 7)]);
    }

    /// `matches( pHandler )`: the same shared handler matches; a
    /// different handler of the same kind does not, because identity and
    /// not content is what the reference compares.
    #[test]
    fn the_handler_match_is_by_identity() {
        let first = Rc::new(Noting::default());
        let second = Rc::new(Noting::default());
        let order = ReplyOrder::new(
            Rc::clone(&first) as Rc<dyn ReplyMessageHandler>,
            0,
            5000000,
            5,
        );
        let request = Request::new(3, &order, None);

        assert!(request.matches_handler(&(Rc::clone(&first) as Rc<dyn ReplyMessageHandler>)));
        assert!(!request.matches_handler(&(Rc::clone(&second) as Rc<dyn ReplyMessageHandler>)));
    }

    /// `matches( pChannel )`: the channel's address decides, and a
    /// channelless request matches no channel.
    #[test]
    fn the_channel_match_is_by_address() {
        let here = Address::new(0x7f000001, 2000);
        let elsewhere = Address::new(0x7f000001, 2001);

        let on_channel = Request::new(4, &order(), Some(here));
        assert!(on_channel.matches_channel(&here));
        assert!(!on_channel.matches_channel(&elsewhere));

        let no_channel = Request::new(5, &order(), None);
        assert!(!no_channel.matches_channel(&here));
    }

    /// A fresh request carries no timeout, whatever its channel: arming
    /// is the manager's move, not the constructor's. Cancelling the
    /// absent timer is inert, so a request with no timeout still
    /// finishes safely.
    #[test]
    fn a_fresh_request_carries_no_timeout() {
        let handler = Rc::new(Noting::default());
        let order = noting_order(&handler);
        let mut request = Request::new(6, &order, None);

        request.cancel_timer();
        assert_eq!(
            handler.notes(),
            Vec::<(&'static str, usize)>::new(),
            "cancelling a timer that was never armed changes nothing"
        );

        request.handle_message(
            &Address::new(0, 0),
            &UnpackedMessageHeader::new(0xFF),
            &mut BinaryStream::new(),
        );
        assert_eq!(handler.notes(), vec![("message", 6)]);
        assert_eq!(request.reply_id(), 6);
        assert_eq!(request.arg(), 6);
    }
}
