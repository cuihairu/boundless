//! `InputMessageHandler` (`lib/network/interfaces.hpp`).
//!
//! Only the half the interface registry needs is ported here: the
//! callback message length. Delivery (`handleMessage`) and the reply and
//! notification handlers belong to the request/reply items of BW-02-02.

use crate::misc::MessageID;

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
