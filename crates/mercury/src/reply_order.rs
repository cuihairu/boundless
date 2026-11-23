//! `ReplyOrder` (`lib/network/reply_order.hpp`): one request that needs a
//! reply, as the sending side of a bundle records it.
//!
//! `startRequest` appends one of these to the bundle for every request it
//! starts, and `addReplyOrdersTo` hands them all to a `RequestManager`
//! once the bundle is finalised. That is when the reply id is allocated
//! and written into the message, which is why this holds where it goes
//! rather than the id itself.

use std::rc::Rc;

use crate::interfaces::ReplyMessageHandler;
use crate::message_framing::REPLY_ID_SIZE;

/// `ReplyOrder`.
#[derive(Clone)]
pub struct ReplyOrder {
    /// `handler`: the user reply handler.
    handler: Rc<dyn ReplyMessageHandler>,
    /// `arg`: user argument passed to the handler.
    arg: usize,
    /// `microseconds`: timeout in microseconds.
    microseconds: i32,
    /// `pReplyID`: where in the frame the allocated reply id is written.
    ///
    /// The reference keeps a pointer into the bundle data
    /// (`void *pReplyID`); a shared buffer and an offset into it is the
    /// same information without the raw pointer.
    reply_id_offset: usize,
}

impl ReplyOrder {
    /// `ReplyOrder( handler, arg, timeout, pReplyID )`
    /// (`udp_bundle.cpp`, `startRequest`).
    pub fn new(
        handler: Rc<dyn ReplyMessageHandler>,
        arg: usize,
        microseconds: i32,
        reply_id_offset: usize,
    ) -> Self {
        ReplyOrder {
            handler,
            arg,
            microseconds,
            reply_id_offset,
        }
    }

    /// `handler`.
    pub fn handler(&self) -> Rc<dyn ReplyMessageHandler> {
        self.handler.clone()
    }

    /// `arg`.
    pub fn arg(&self) -> usize {
        self.arg
    }

    /// `microseconds`: the timeout, in microseconds.
    pub fn microseconds(&self) -> i32 {
        self.microseconds
    }

    /// `pReplyID`, as the offset the reply id is written at.
    pub fn reply_id_offset(&self) -> usize {
        self.reply_id_offset
    }

    /// How far the reply id field runs from its offset.
    pub fn reply_id_span(&self) -> usize {
        REPLY_ID_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::ReplyMessageHandler;
    use crate::misc::REPLY_ID_MAX;
    use crate::nub_exception::NubException;

    struct NoReply;

    impl ReplyMessageHandler for NoReply {
        fn handle_message(
            &self,
            _source: &crate::endpoint::Address,
            _header: &crate::unpacked_message_header::UnpackedMessageHeader,
            _data: &mut foundation::stream::BinaryStream,
            _arg: usize,
        ) {
        }

        fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
    }

    #[test]
    fn an_order_carries_what_the_request_promised() {
        let handler: Rc<dyn ReplyMessageHandler> = Rc::new(NoReply);
        let order = ReplyOrder::new(handler, 7, REPLY_ID_MAX, 11);

        assert_eq!(order.arg(), 7);
        assert_eq!(order.microseconds(), REPLY_ID_MAX);
        assert_eq!(order.reply_id_offset(), 11);
        assert_eq!(order.reply_id_span(), 4, "a ReplyID is four bytes");

        // The order hands out the very handler it was given; a handler
        // that wants neither reply nor failure takes both silently.
        let handler = order.handler();
        handler.handle_message(
            &crate::endpoint::Address::new(0, 0),
            &crate::unpacked_message_header::UnpackedMessageHeader::new(0xFF),
            &mut foundation::stream::BinaryStream::new(),
            order.arg(),
        );
        handler.handle_exception(
            &NubException::new(crate::misc::Reason::TimerExpired, None),
            order.arg(),
        );
    }
}
