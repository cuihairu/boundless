//! `InputMessageHandler` and `ReplyMessageHandler` (`lib/network/interfaces.hpp`).
//!
//! `InputMessageHandler` carries both halves of the registry contract: the
//! delivery method the interface table calls with a received message, and
//! the callback message sizing the registry asks when a
//! `CALLBACK_LENGTH_MESSAGE` needs to size itself. `ReplyMessageHandler`
//! is fully ported here because the request manager dispatches replies to
//! it (BW-02-02-03).

use crate::endpoint::Address;
use crate::misc::MessageID;
use crate::nub_exception::NubException;
use crate::unpacked_message_header::UnpackedMessageHeader;
use foundation::stream::BinaryStream;

/// `Mercury::ReplyMessageHandler`: the receiver side of a reply. A client
/// that issues a request supplies one to handle the reply.
///
/// See `Bundle::startRequest` / `Bundle::startReply`.
pub trait ReplyMessageHandler {
    /// `handleMessage`: deliver a reply. `arg` is the user data passed with
    /// the request that generated it.
    fn handle_message(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
        arg: usize,
    );

    /// `handleException`: the request failed. The usual cause is a timeout.
    fn handle_exception(&self, exception: &NubException, arg: usize);

    /// `handleShuttingDown`: the application is shutting down and the
    /// request could not be delivered. The reference warns "Not handled.
    /// Possible memory leak." and returns; subclasses that are safe to
    /// call while shutting down override this to forward to
    /// `handle_exception` (see `ShutdownSafeReplyMessageHandler`).
    fn handle_shutting_down(&self, exception: &NubException, arg: usize) {
        eprintln!(
            "ReplyMessageHandler::handleShuttingDown: Not handled. Possible memory leak.\n  reason={:?}",
            exception.reason()
        );
        let _ = arg;
    }
}

// The reference `ShutdownSafeReplyMessageHandler` is a subclass whose
// `handleShuttingDown` forwards to `handleException`. Rust trait defaults
// cannot be overridden per-impl without specialization, so this port keeps
// the forwarding as a documented idiom: a shutdown-safe handler overrides
// `handle_shutting_down` to call its own `handle_exception`. The default
// `handle_shutting_down` above still warns, matching the base class.

/// `Mercury::InputMessageHandler`: the receiver side of a message. The
/// interface table calls [`handle_message`](Self::handle_message) when a
/// message arrives, and [`get_message_stream_size`](Self::get_message_stream_size)
/// when a `CALLBACK_LENGTH_MESSAGE` needs to size itself.
///
/// The reference declares `getMessageStreamSize( networkInterface,
/// srcAddr, msgID )`; this port drops the first two. No handler in the
/// reference uses them to size a callback message, and the interface table
/// already knows the source it is delivering from.
pub trait InputMessageHandler {
    /// `handleMessage`: deliver a received message. `header` carries the
    /// type, size and flags; `data` is the message body.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::NONE;
    use crate::misc::Reason;

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
    /// reference's post-assert return value.
    #[test]
    fn a_handler_that_never_overrides_says_negative_one() {
        let handler = NeverOverrides;
        assert_eq!(handler.get_message_stream_size(7), -1);

        // The delivery half accepts and discards a message.
        let source = NONE;
        let mut header = UnpackedMessageHeader::default();
        let mut data = BinaryStream::new();
        handler.handle_message(&source, &mut header, &mut data);
    }

    /// A bare reply handler gets only the warning from the base class's
    /// `handleShuttingDown`: "Not handled. Possible memory leak."
    #[test]
    fn the_default_shutdown_notice_only_warns() {
        struct BareHandler;

        impl ReplyMessageHandler for BareHandler {
            fn handle_message(
                &self,
                _source: &Address,
                _header: &mut UnpackedMessageHeader,
                _data: &mut BinaryStream,
                _arg: usize,
            ) {
            }
            fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
        }

        let handler = BareHandler;
        let exception = NubException::new(Reason::ShuttingDown, NONE);

        // The bare handler does nothing for a delivery or a failure; only
        // the base-class shutdown notice is exercised above.
        let mut header = UnpackedMessageHeader::default();
        let mut data = BinaryStream::new();
        ReplyMessageHandler::handle_message(&handler, &NONE, &mut header, &mut data, 0);
        ReplyMessageHandler::handle_exception(&handler, &exception, 0);

        handler.handle_shutting_down(&exception, 0);
    }
}
