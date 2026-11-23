//! `Mercury::MessageFilter` (`lib/network/message_filter.hpp`).
//!
//! A message filter has first-pass handling of messages before their
//! destination message handlers -- the seam a channel is handed so
//! application code can decide whether, and when, a received message
//! reaches its handler. The reference's one implementation is the baseapp's
//! `RateLimitMessageFilter`, which defers messages that arrive faster than
//! the configured limits allow ([`crate::rate_limit_message_filter`]).
//!
//! The dispatch walk consults the channel's filter instead of the handler
//! when one is fitted (`UDPBundleProcessor::dispatchMessages`,
//! udp_bundle_processor.cpp:171-179); the port's
//! [`crate::bundle_dispatch::dispatch_messages_filtered`] is that arm.

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interfaces::InputMessageHandler;
use crate::unpacked_message_header::UnpackedMessageHeader;
use std::rc::Rc;

/// `Mercury::MessageFilter`: first-pass handling of a message before its
/// destination handler.
///
/// The reference hands the handler over as a raw
/// `Mercury::InputMessageHandler *`; the port hands an `&Rc` of it, because
/// a filter that defers a message (the rate limiter) has to keep the
/// handler alive to replay the message later -- the raw pointer the
/// reference stores in its `BufferedMessage` works only because the
/// interface table outlives every buffered message.
///
/// Filters are reference-counted in the reference (`MessageFilterPtr`);
/// here they are shared as `Rc<dyn MessageFilter>`, the same shape the
/// channel's `pMessageFilter` accessor holds.
pub trait MessageFilter {
    /// `filterMessage`: decide what happens to this message -- deliver it
    /// now, later, or never.
    ///
    /// `header` is handed over mutably and `data` consumed at the filter's
    /// discretion, exactly as a handler would receive them, so a filter
    /// that passes the message through unchanged simply calls
    /// [`InputMessageHandler::handle_message`] itself.
    fn filter_message(
        &self,
        src_addr: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
        handler: &Rc<dyn InputMessageHandler>,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A pass-through filter: the reference's "may decide to pass the
    /// message on" arm, which just calls the handler it was handed.
    struct PassThrough {
        seen: RefCell<usize>,
    }

    impl MessageFilter for PassThrough {
        fn filter_message(
            &self,
            src_addr: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
            handler: &Rc<dyn InputMessageHandler>,
        ) {
            *self.seen.borrow_mut() += 1;
            handler.handle_message(src_addr, header, data);
        }
    }

    /// A sink handler that records its deliveries.
    struct Sink {
        bodies: RefCell<Vec<Vec<u8>>>,
    }

    impl InputMessageHandler for Sink {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            self.bodies.borrow_mut().push(data.unread_bytes().to_vec());
            data.finish();
        }
    }

    /// A filter may simply pass the message on: the handler is called with
    /// the header and stream the filter was handed, which is the contract
    /// every implementing filter is written against.
    #[test]
    fn a_pass_through_filter_delivers_the_handler_its_message() {
        let sink = Rc::new(Sink {
            bodies: RefCell::new(Vec::new()),
        });
        let filter = PassThrough {
            seen: RefCell::new(0),
        };

        let mut stream = BinaryStream::new();
        stream.write_bytes(b"body");
        filter.filter_message(
            &Address::new(0x7f000001, 7001),
            &mut UnpackedMessageHeader::new(3),
            &mut stream,
            &(Rc::clone(&sink) as Rc<dyn InputMessageHandler>),
        );

        assert_eq!(*filter.seen.borrow(), 1);
        assert_eq!(sink.bodies.borrow()[0], b"body");
    }
}
