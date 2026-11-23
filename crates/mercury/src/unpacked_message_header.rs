//! `UnpackedMessageHeader` (`lib/network/unpacked_message_header.hpp`).
//!
//! The header Mercury hands to a handler once a message has been unpacked
//! out of a bundle: what it is, which request it answers, and how long it
//! is. The `ChannelPtr`, `NetworkInterface` and break-loop fields the
//! reference carries are not ported yet; they arrive with the channel and
//! delivery items of BW-02-03.

use crate::interface_element::InterfaceElement;
use crate::misc::MessageID;
use crate::misc::ReplyID;

/// `UnpackedMessageHeader::NO_REPLY`: the reply id of a message that
/// answers no request, which is the reference's `REPLY_ID_NONE`.
pub const NO_REPLY: ReplyID = crate::misc::REPLY_ID_NONE;

/// `UnpackedMessageHeader`.
///
/// `Clone` only: the element this header carries a shared handler in, so
/// it cannot be compared, and the reference has no comparison for it
/// either.
#[derive(Debug, Clone)]
pub struct UnpackedMessageHeader {
    /// `identifier`: the message identifier.
    identifier: MessageID,
    /// `replyID`: a unique ID, used for replying.
    reply_id: ReplyID,
    /// `length`: the number of body bytes in this message.
    length: i32,
    /// `pInterfaceElement`: the element that declared this message, which
    /// is where `msgName()` reads from.
    element: Option<InterfaceElement>,
}

impl Default for UnpackedMessageHeader {
    fn default() -> Self {
        UnpackedMessageHeader::new(0)
    }
}

impl UnpackedMessageHeader {
    /// `UnpackedMessageHeader()`: an empty header. The reply id starts at
    /// `NO_REPLY`, and no element is known yet.
    pub fn new(identifier: MessageID) -> Self {
        UnpackedMessageHeader {
            identifier,
            reply_id: NO_REPLY,
            length: 0,
            element: None,
        }
    }

    /// `identifier`.
    pub fn identifier(&self) -> MessageID {
        self.identifier
    }

    /// `replyID`.
    pub fn reply_id(&self) -> ReplyID {
        self.reply_id
    }

    /// Set `replyID`.
    pub fn set_reply_id(&mut self, reply_id: ReplyID) {
        self.reply_id = reply_id;
    }

    /// `length`.
    pub fn length(&self) -> i32 {
        self.length
    }

    /// Set `length`; the reply dispatch shrinks it as it consumes the body.
    pub fn set_length(&mut self, length: i32) {
        self.length = length;
    }

    /// `pInterfaceElement`: the element that declared the message.
    pub fn interface_element(&self) -> Option<&InterfaceElement> {
        self.element.as_ref()
    }

    /// Set `pInterfaceElement`.
    pub fn set_interface_element(&mut self, element: InterfaceElement) {
        self.element = Some(element);
    }

    /// `msgName()`: the name of the element, or the empty string when no
    /// element is known (`unpacked_message_header.cpp`). The reference
    /// returns a pointer to a `const char` that the element owns; here the
    /// element's name is a static string, so the result is too.
    pub fn msg_name(&self) -> &'static str {
        self.element.as_ref().map_or("", |element| element.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_element::VARIABLE_LENGTH_MESSAGE;
    use crate::misc::{REPLY_ID_MAX, REPLY_ID_NONE};

    #[test]
    fn a_default_header_has_no_reply_and_no_length() {
        let header = UnpackedMessageHeader::default();
        assert_eq!(
            header.identifier(),
            UnpackedMessageHeader::new(0).identifier()
        );
        assert_eq!(header.identifier(), 0);
        assert_eq!(header.reply_id(), NO_REPLY);
        assert_eq!(header.reply_id(), REPLY_ID_NONE);
        assert_eq!(header.length(), 0);
        assert_eq!(header.msg_name(), "", "no element, no name");
        assert!(header.interface_element().is_none());
    }

    #[test]
    fn the_header_carries_its_reply_id_and_length() {
        let mut header = UnpackedMessageHeader::new(0xFF);
        header.set_reply_id(REPLY_ID_MAX);
        header.set_length(12);
        assert_eq!(header.identifier(), 0xFF);
        assert_eq!(header.reply_id(), REPLY_ID_MAX);
        assert_eq!(header.length(), 12);
    }

    /// `msgName()` names the declaring element, unlike a name copied off
    /// it: replacing the element replaces the name.
    #[test]
    fn the_name_comes_from_the_declaring_element() {
        let mut header = UnpackedMessageHeader::new(3);
        header.set_interface_element(InterfaceElement::new(
            "login",
            3,
            VARIABLE_LENGTH_MESSAGE,
            2,
            None,
        ));
        assert_eq!(header.msg_name(), "login");
        assert_eq!(
            header.interface_element().map(InterfaceElement::name),
            Some("login")
        );

        header.set_interface_element(InterfaceElement::new(
            "probe",
            4,
            VARIABLE_LENGTH_MESSAGE,
            1,
            None,
        ));
        assert_eq!(header.msg_name(), "probe");
    }
}
