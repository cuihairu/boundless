//! `Request` (`lib/network/request.hpp/.cpp`): the context of one
//! outstanding request, the thing a reply is matched back to.
//!
//! The reference is a `TimerHandler` because a request also owns its
//! timeout timer; the timer, and the cancellation built on it, belong to
//! BW-02-02-04. What lives here is the association: which handler, which
//! user argument, and which channel address a reply must come from.

use std::rc::Rc;

use crate::endpoint::Address;
use crate::interfaces::ReplyMessageHandler;
use crate::misc::ReplyID;
use crate::reply_order::ReplyOrder;
use crate::unpacked_message_header::UnpackedMessageHeader;

use foundation::stream::BinaryStream;

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
}

impl Request {
    /// `Request( replyID, replyOrder, pChannel, ... )`: fill out the
    /// request from the reply order and the channel it rides.
    ///
    /// `pChannel` is the channel's address, or `None` for a request with
    /// no channel.
    pub fn new(reply_id: ReplyID, order: &ReplyOrder, channel: Option<Address>) -> Self {
        Request {
            reply_id,
            handler: order.handler(),
            arg: order.arg(),
            channel,
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
    /// that by removing it from its map, so this only makes the call.
    pub fn handle_message(
        &self,
        source: &Address,
        header: &UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        self.handler.handle_message(source, header, data, self.arg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::ReplyMessageHandler;
    use crate::nub_exception::NubException;

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
}
