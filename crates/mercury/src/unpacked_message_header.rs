//! `Mercury::UnpackedMessageHeader` (`lib/network/unpacked_message_header.hpp`).
//!
//! The header Mercury delivers beside every message body. Only the fields
//! the request/reply layer touches are ported here; the delivery-side
//! fields (`pBreakLoop`, `pInterfaceElement`, `pChannel`, `pInterface`) and
//! `msgName()` arrive with the interface-table dispatch in BW-02-02-05.

use crate::misc::{MessageID, REPLY_ID_NONE, ReplyID};

/// `Mercury::UnpackedMessageHeader`.
#[derive(Debug, Clone, Copy)]
pub struct UnpackedMessageHeader {
    /// `identifier`: the message id.
    pub identifier: MessageID,
    /// `replyID`: the id of the request this message replies to, or
    /// `NO_REPLY` for a message that is not a reply.
    pub reply_id: ReplyID,
    /// `length`: the number of body bytes this message carries.
    pub length: i32,
}

impl UnpackedMessageHeader {
    /// `NO_REPLY = REPLY_ID_NONE`.
    pub const NO_REPLY: ReplyID = REPLY_ID_NONE;
}

impl Default for UnpackedMessageHeader {
    fn default() -> Self {
        Self {
            identifier: 0,
            reply_id: Self::NO_REPLY,
            length: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_header_carries_no_reply() {
        let header = UnpackedMessageHeader::default();
        assert_eq!(header.identifier, 0);
        assert_eq!(header.reply_id, UnpackedMessageHeader::NO_REPLY);
        assert_eq!(header.reply_id, REPLY_ID_NONE);
        assert_eq!(header.length, 0);
    }
}
