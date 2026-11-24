//! `Mercury::MessageFilter` (`lib/network/message_filter.hpp`).
//!
//! The first-pass handler a channel holds for the messages that arrive
//! on it. A channel with one hands every delivered message to its filter
//! instead of the message handler, and the filter delivers by calling
//! the handler itself (udp_bundle_processor.cpp:161-170,
//! tcp_bundle_processor.cpp:178-185). The reference documents
//! rate-limited dispatch as the use case, and the baseapp's
//! `RateLimitMessageFilter` is that concrete filter
//! ([`crate::rate_limit_message_filter`]).

use std::rc::Rc;

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interfaces::InputMessageHandler;
use crate::unpacked_message_header::UnpackedMessageHeader;

/// `Mercury::MessageFilter` (message_filter.hpp:25).
///
/// The reference hands the destination handler over as a raw
/// `Mercury::InputMessageHandler *`; the port hands an `&Rc` of it,
/// because a filter that defers a message (the rate limiter) has to
/// keep the handler alive to replay the message later -- the raw
/// pointer the reference stores in its `BufferedMessage` works only
/// because the interface table outlives every buffered message.
pub trait MessageFilter {
    /// `filterMessage`: first-pass handling of one delivered message.
    /// The filter is handed the destination handler so that it can
    /// deliver the message itself, possibly rewritten, held back or
    /// dropped; not calling it drops the message.
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
