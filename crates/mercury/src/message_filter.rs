//! `Mercury::MessageFilter` (`lib/network/message_filter.hpp`).
//!
//! The first-pass handler a channel holds for the messages that arrive
//! on it. A channel with one hands every delivered message to its filter
//! instead of the message handler, and the filter delivers by calling
//! the handler itself (udp_bundle_processor.cpp:161-170,
//! tcp_bundle_processor.cpp:178-185). The reference documents
//! rate-limited dispatch as the use case, and Mercury ships no concrete
//! subclass: the trait is deliberately bare, so an app's filter is a
//! plain `impl MessageFilter`.

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interfaces::InputMessageHandler;
use crate::unpacked_message_header::UnpackedMessageHeader;

/// `Mercury::MessageFilter` (message_filter.hpp:25).
pub trait MessageFilter {
    /// `filterMessage`: first-pass handling of one delivered message. The
    /// filter is handed the destination handler so that it can deliver
    /// the message itself, possibly rewritten or held back; not calling
    /// it drops the message.
    fn filter_message(
        &self,
        src_addr: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
        handler: &dyn InputMessageHandler,
    );
}
