//! `InputMessageHandler` and `ReplyMessageHandler`
//! (`lib/network/interfaces.hpp`).
//!
//! The two halves of receiving a message: the interface a message handler
//! implements (its size, and the delivery call), and the interface a
//! request's answer is delivered through. The socket
//! `InputNotificationHandler` stays with the poller item, which owns file
//! descriptors.

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::misc::MessageID;
use crate::nub_exception::NubException;
use crate::unpacked_message_header::UnpackedMessageHeader;

/// `Mercury::InputMessageHandler`: the receiver side of a message, and the
/// callback the registry asks when a `CALLBACK_LENGTH_MESSAGE` needs to
/// size itself.
///
/// The reference declares `getMessageStreamSize( networkInterface,
/// srcAddr, msgID )`; this port drops the first two. No handler in the
/// reference uses them to size a callback message, and the network
/// interface does not exist at this layer yet.
pub trait InputMessageHandler {
    /// `handleMessage`: deliver the message. Pure virtual in the
    /// reference, so there is no default here.
    ///
    /// `header` is handed over mutably because the delivery path shrinks
    /// `length` as it consumes the body -- the reference notes this is why
    /// the parameter is not `const`.
    fn handle_message(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    );

    /// The number of body bytes, positive for a fixed size and negative
    /// for the number of length bytes of a variable message;
    /// `INVALID_STREAM_SIZE` says the bundle is invalid.
    ///
    /// The reference's default implementation trips `MF_ASSERT( 0 )` and
    /// then returns -1: a callback message whose handler never overrides
    /// this is a programmer error. This port drops the abort and keeps
    /// the -1, so a misdeclared callback surfaces as an invalid bundle
    /// instead of killing the process.
    fn get_message_stream_size(&self, _msg_id: MessageID) -> i32 {
        -1
    }
}

/// `Mercury::ReplyMessageHandler`: the interface a request gives Mercury
/// so the answer to it can be delivered, or its failure reported.
///
/// A user argument travels with the request rather than with the handler,
/// which is how one handler serves several outstanding requests. The
/// reference passes it as a `void *`; here it is a `usize`, the same
/// reduced form the timer layer uses for its user argument.
pub trait ReplyMessageHandler {
    /// `handleMessage`: deliver the reply. `data` holds the body with the
    /// four reply id bytes already read off it by the dispatch.
    fn handle_message(
        &self,
        source: &Address,
        header: &UnpackedMessageHeader,
        data: &mut BinaryStream,
        arg: usize,
    );

    /// `handleException`: report that the request failed rather than
    /// answered -- normally a timeout.
    fn handle_exception(&self, exception: &NubException, arg: usize);

    /// `handleShuttingDown`: the request was dropped because Mercury is
    /// going down, not because it failed.
    ///
    /// The reference's default logs a warning about a possible memory
    /// leak, because a handler with no request left pending will never be
    /// told again.
    fn handle_shutting_down(&self, _exception: &NubException, _arg: usize) {
        eprintln!("ReplyMessageHandler::handleShuttingDown: Not handled. Possible memory leak.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::{Reason, REPLY_ID_NONE};

    struct NeverOverrides;

    impl InputMessageHandler for NeverOverrides {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &mut UnpackedMessageHeader,
            _data: &mut BinaryStream,
        ) {
        }
    }

    /// A handler that never overrides the sizing answers -1, the
    /// reference's post-assert return value; its delivery is a plain
    /// no-op.
    #[test]
    fn a_handler_that_never_overrides_says_negative_one() {
        let never = NeverOverrides;
        assert_eq!(never.get_message_stream_size(7), -1);
        never.handle_message(
            &Address::new(0, 0),
            &mut UnpackedMessageHeader::new(1),
            &mut BinaryStream::new(),
        );
    }

    /// A reply handler that records what it was given, and never overrides
    /// the shutdown hook.
    struct Recording {
        delivered: std::cell::Cell<Option<(usize, usize)>>,
    }

    impl ReplyMessageHandler for Recording {
        fn handle_message(
            &self,
            source: &Address,
            header: &UnpackedMessageHeader,
            _data: &mut BinaryStream,
            arg: usize,
        ) {
            assert_eq!(source.c_str(), "0.0.0.0:0");
            assert_eq!(header.reply_id(), REPLY_ID_NONE);
            self.delivered.set(Some((header.length() as usize, arg)));
        }

        fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
    }

    /// The default shutdown hook prints the reference's warning and
    /// does nothing else, so a handler that never overrides it is safe to
    /// drop.
    #[test]
    fn the_default_shutdown_hook_is_a_warning_only() {
        let handler = Recording {
            delivered: std::cell::Cell::new(None),
        };

        let exception = NubException::new(Reason::ShuttingDown, None);
        handler.handle_shutting_down(&exception, 9);
        handler.handle_exception(&exception, 9);
        assert_eq!(handler.delivered.get(), None, "the hook delivers nothing");
    }

    /// `handleMessage` is the seam the dispatch hands the answer to, with
    /// the user argument that rode along on the request.
    #[test]
    fn the_message_handler_receives_the_argument_that_came_with_the_request() {
        let handler = Recording {
            delivered: std::cell::Cell::new(None),
        };

        let source = Address::new(0, 0);
        let header = UnpackedMessageHeader::new(0xFF);
        let mut data = BinaryStream::new();
        handler.handle_message(&source, &header, &mut data, 42);
        assert_eq!(handler.delivered.get(), Some((0, 42)));
    }
}
