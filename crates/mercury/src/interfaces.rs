//! `InputMessageHandler` and `ReplyMessageHandler` (`lib/network/interfaces.hpp`).
//!
//! `InputMessageHandler` currently only carries the half the interface
//! registry needs: the callback message length. Its delivery half
//! (`handleMessage`) arrives with the interface-table dispatch in
//! BW-02-02-05. `ReplyMessageHandler` is fully ported here because the
//! request manager dispatches replies to it (BW-02-02-03).

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

/// `Mercury::InputMessageHandler`: the receiver side of a message, and the
/// callback the registry asks when a `CALLBACK_LENGTH_MESSAGE` needs to
/// size itself.
///
/// The reference declares `getMessageStreamSize( networkInterface,
/// srcAddr, msgID )`; this port drops the first two. No handler in the
/// reference uses them to size a callback message, and the address is not
/// available at this layer yet -- it arrives with the delivery seam in
/// BW-02-02-03.
pub trait InputMessageHandler {
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

    struct NeverOverrides;

    impl InputMessageHandler for NeverOverrides {}

    /// A handler that never overrides the sizing answers -1, the
    /// reference's post-assert return value.
    #[test]
    fn a_handler_that_never_overrides_says_negative_one() {
        assert_eq!(NeverOverrides.get_message_stream_size(7), -1);
    }
}
