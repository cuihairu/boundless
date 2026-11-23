//! The receive-side walk over a bundle: `Mercury::UDPBundleProcessor::
//! dispatchMessages` and its `iterator`
//! (`lib/network/udp_bundle_processor.cpp:47-505`).
//!
//! For every message packed in the body the walk looks the id up in the
//! socket's [`InterfaceTable`], resolves a callback message's length with
//! its handler, unpacks the frame (length field, then the reply preamble
//! of a request), and hands the body to the handler the element is served
//! with. Every way the wire can disagree with the interface -- an id no
//! handler serves, a length field that lies -- ends the walk and reports
//! the reason the reference reports, leaving the rest of the bundle
//! undelivered.
//!
//! The reference walks a chain of packets with packet-absolute offsets,
//! its flags word at offset 0 and the first body byte at offset
//! `Packet::HEADER_SIZE`. This port flattens the chain into one
//! contiguous buffer with the flags word stripped off (see
//! [`crate::message_framing`]), so the walk's offsets are body-relative.
//! The request links the wire carries stay packet-absolute -- the caller
//! hands the first request's footer value through untouched ([`None`]
//! when the packet carried no request footer) and the walk translates at
//! the two places a link meets an offset: seeding the first message and
//! comparing each message's link against its own position. The same
//! flattening folds the reference's "is there a next packet to hold the
//! payload" check into a plain bounds check, and the walk's end
//! condition into `offset >= body.len()`.
//!
//! Left with the channel layer of BW-02-03: the packet chain and its
//! `nextPacket`, early (`isEarly`) dispatch, the break-loop hook a dying
//! channel pulls (`header.pBreakLoop`), the fragment reassembly, the
//! `ProcessSocketStatsHelper`, and the table's `onBundleStarted`/
//! `onBundleFinished` hooks, whose handler takes a `Channel *`.

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interface_element::{InterfaceElement, REPLY_MESSAGE_IDENTIFIER};
use crate::interface_table::InterfaceTable;
use crate::message_filter::MessageFilter;
use crate::message_framing::{NEXT_REQUEST_OFFSET_SIZE, REPLY_ID_SIZE, SPECIAL_LENGTH_SIZE};
use crate::misc::{Reason, ReplyID};
use crate::unpacked_message_header::{NO_REPLY, UnpackedMessageHeader};

/// One unpacked message: the header the handler is handed, and where its
/// body sits in the bundle buffer.
struct Unpacked {
    header: UnpackedMessageHeader,
    data_offset: usize,
    data_length: usize,
}

/// `dispatchMessages( interfaceTable, addr, ... )`: walk the messages
/// packed in `body` and deliver each to the handler its id is served
/// with.
///
/// `first_request_offset` is the packet's first-request footer value --
/// the packet-absolute offset of the bundle's first request -- or
/// [`None`] when the packet carried no request footer; the request links
/// each message carries chain the rest, in the same wire terms.
///
/// `p_message_filter` is the dispatching channel's message filter, the
/// `pMessageFilter` the reference fetches before its loop
/// (udp_bundle_processor.cpp:59-60): a channel with one takes the
/// delivery through it instead of the handler, and the filter delivers
/// by calling the handler itself (:161-170).
///
/// The reference returns the reason it stopped on (`REASON_SUCCESS` when
/// every message was delivered); the rest of a bundle after an error is
/// never delivered.
pub fn dispatch_messages(
    table: &InterfaceTable,
    source: &Address,
    body: &mut [u8],
    first_request_offset: Option<usize>,
    p_message_filter: Option<&dyn MessageFilter>,
) -> Reason {
    let msg_end = body.len();
    let mut offset = 0usize;
    // `iterator::nextPacket`: the packet's first request offset, zero when
    // it has none. `None` is that zero here, and it can never equal a
    // wire offset, which starts at the flags word the body lacks.
    let mut next_request_offset = first_request_offset;

    // `while (iter != end && !breakLoop)`: with one buffer, the iterator
    // runs out of packets exactly when the next message would start at or
    // past the end of the body.
    while offset < msg_end {
        // `interfaceTable[ iter.msgID() ]`
        let msg_id = body[offset];
        let served = table.element(msg_id);
        let handler = match served.handler() {
            Some(handler) => handler,
            None => {
                // If there aren't any interfaces served on this nub then
                // don't print the warning (slightly dodgy I know)
                eprintln!(
                    "UDPBundleProcessor::dispatchMessages( {} ): Discarding bundle after hitting unhandled message ID {}",
                    source.c_str(),
                    msg_id
                );
                return Reason::NonexistentEntry;
            }
        };

        // `updatedIE_`: the callback sizing folds into a copy, so the
        // served entry keeps its `CALLBACK_LENGTH_MESSAGE` style for the
        // next bundle.
        let mut updated = served.clone();
        if !updated.update_length_details(handler.as_ref()) {
            eprintln!(
                "UDPBundleProcessor::dispatchMessages( {} ): Discarding bundle after failure to update length details for message ID {}",
                source.c_str(),
                msg_id
            );
            return Reason::CorruptedPacket;
        }

        // `isRequest = ( nextRequestOffset_ == offset_ )`: the wire value
        // counts from the flags word, the walk from the body's first
        // byte, so the constant header shifts the walk's position up.
        let is_request = next_request_offset == Some(offset + crate::packet::HEADER_SIZE);
        let mut unpacked =
            match unpack_message(&updated, body, offset, is_request, &mut next_request_offset) {
                Ok(unpacked) => unpacked,
                Err(()) => return Reason::CorruptedPacket,
            };
        // `header.pInterfaceElement = &ie`: the served entry, callback
        // style and all, not the resolved copy.
        unpacked.header.set_interface_element(served.clone());

        // `iter.data()`: with the payload already bounds-checked against
        // this one buffer, the data is always where unpack left it -- the
        // reference's "chain too short" (`data() == NULL`) cannot happen
        // here.
        let mut stream = BinaryStream::new();
        // `MemoryIStream mis( msgData, header.length )`: the stream is the
        // declared length; `data_length` only carries the walk, its extra
        // four bytes being the stale tail the special form leaves.
        stream.write_bytes(
            &body[unpacked.data_offset..unpacked.data_offset + unpacked.header.length() as usize],
        );

        // The channel's message filter first-passes the delivery when it
        // has one, and delivers by calling the handler itself
        // (udp_bundle_processor.cpp:161-170).
        match p_message_filter {
            Some(filter) => {
                filter.filter_message(source, &mut unpacked.header, &mut stream, handler.as_ref())
            }
            None => handler.handle_message(source, &mut unpacked.header, &mut stream),
        }

        // `ie.stopProfile( header.length )` records the delivery against
        // the element (the reference does it under `ENABLE_WATCHERS`).
        table.record_received(msg_id, unpacked.header.length() as u32);

        // A handler that did not eat its body is reported; a plain
        // message's leftovers are consumed off it, a reply's are not.
        if stream.remaining() != 0 {
            if msg_id == REPLY_MESSAGE_IDENTIFIER {
                eprintln!(
                    "UDPBundleProcessor::dispatchMessages( {} ): Handler for reply left {} bytes",
                    source.c_str(),
                    stream.remaining()
                );
            } else {
                eprintln!(
                    "UDPBundleProcessor::dispatchMessages( {} ): Handler for message {} (ID {}) left {} bytes",
                    source.c_str(),
                    served.name(),
                    msg_id,
                    stream.remaining()
                );
                stream.finish();
            }
        }

        // `iterator::operator++`: the next message starts after this one's
        // body, the four spare bytes of the special form included
        // (`dataLength_` already counts them).
        offset = unpacked.data_offset + unpacked.data_length;
    }

    Reason::Success
}

/// `iterator::unpack( ie )`: read one message's header out of the body at
/// `offset`, advancing `next_request_offset` along the request links.
///
/// Every failure logs the reference's message and its
/// "Got corrupted message header" tail; the caller answers with
/// `REASON_CORRUPTED_PACKET`. The errors after the length is read undo the
/// unpack first (`goto error`), the way the reference restores the buffer
/// before giving up.
fn unpack_message(
    element: &InterfaceElement,
    body: &mut [u8],
    offset: usize,
    is_request: bool,
    next_request_offset: &mut Option<usize>,
) -> Result<Unpacked, ()> {
    let msg_end = body.len();

    // read the standard header
    let header_size = element.header_size();
    if offset + header_size.max(0) as usize > msg_end {
        eprintln!(
            "UDPBundleProcessor::iterator::unpack( {} ): Not enough data on stream at {} for header ({} bytes, needed {})",
            element.name(),
            offset,
            msg_end - offset,
            header_size
        );
        return corrupt_header();
    }

    let length = match element.expand_length(&mut body[offset..], is_request) {
        Ok(length) => length,
        Err(_) => {
            // If length is -1, then chances are we've had an overflow
            eprintln!(
                "UDPBundleProcessor::iterator::unpack( {} ): Error unpacking header length at {}",
                element.name(),
                offset
            );
            return corrupt_header();
        }
    };

    let mut msg_beg = offset + header_size as usize;
    let reply_id: ReplyID;

    // let's figure out some flags
    if !is_request {
        reply_id = NO_REPLY;
    } else {
        if msg_beg + REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE > msg_end {
            eprintln!(
                "UDPBundleProcessor::iterator::unpack( {} ): Not enough data on stream at {} for request ID and NRO ({} left, needed {})",
                element.name(),
                offset,
                msg_end - msg_beg,
                REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE
            );
            revert(element, body, offset, length, is_request);
            return corrupt_header();
        }

        let mut cursor = msg_beg;
        reply_id = ReplyID::from_le_bytes(body[cursor..cursor + REPLY_ID_SIZE].try_into().unwrap());
        cursor += REPLY_ID_SIZE;

        let link = u16::from_le_bytes(
            body[cursor..cursor + NEXT_REQUEST_OFFSET_SIZE]
                .try_into()
                .unwrap(),
        );
        // The wire link counts from the flags word the body slice lacks,
        // so it may reach `HEADER_SIZE` past the body's end.
        let wire_msg_end = msg_end + crate::packet::HEADER_SIZE;
        if link as usize > wire_msg_end {
            eprintln!(
                "UDPBundleProcessor::iterator::unpack( {} ): Next request offset out of bounds (got offset {}, msg end offset {})",
                element.name(),
                link,
                wire_msg_end
            );
            revert(element, body, offset, length, is_request);
            return corrupt_header();
        }

        // The link stays in wire terms; the walk translates when it
        // compares. Zero is "no next request", which can never equal an
        // offset a later message stands on.
        *next_request_offset = Some(link as usize);
        msg_beg += REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE;
    }

    // and set up the fields about the message data. With one buffer there
    // is no next packet to lean on, so the payload must be entirely inside
    // this one.
    if msg_beg + length.unsigned_abs() as usize > msg_end {
        eprintln!(
            "UDPBundleProcessor::iterator::unpack( {} ): Not enough data on stream at {} for payload ({} left, needed {})",
            element.name(),
            offset,
            msg_end - msg_beg,
            length
        );
        revert(element, body, offset, length, is_request);
        return corrupt_header();
    }

    let mut data_length = length as usize;
    // If this is a special case of data length (where we put a four byte
    // size on the end), we need to now disregard these bytes.
    if !element.can_handle_length_for(length) {
        data_length += SPECIAL_LENGTH_SIZE;
    }

    let mut header = UnpackedMessageHeader::new(body[offset]);
    header.set_reply_id(reply_id);
    header.set_length(length);

    Ok(Unpacked {
        header,
        data_offset: msg_beg,
        data_length,
    })
}

/// The `errorNoRevert` label's closing
/// `ERROR_MSG( "... Got corrupted message header\n" )`.
fn corrupt_header() -> Result<Unpacked, ()> {
    eprintln!("UDPBundleProcessor::iterator::unpack: Got corrupted message header");
    Err(())
}

/// The `error:` label's `updatedIE_.unexpandLength( ... )`: put the frame
/// back the way the expand found it before giving up on the message. The
/// bundle is being discarded either way, so a frame that cannot be
/// restored is not reported further.
fn revert(
    element: &InterfaceElement,
    body: &mut [u8],
    offset: usize,
    length: i32,
    is_request: bool,
) {
    let _ = element.unexpand_length(&mut body[offset..], length, is_request);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_element::{
        CALLBACK_LENGTH_MESSAGE, FIXED_LENGTH_MESSAGE, INVALID_STREAM_SIZE, VARIABLE_LENGTH_MESSAGE,
    };
    use crate::interfaces::{InputMessageHandler, ReplyMessageHandler};
    use crate::message_filter::MessageFilter;
    use crate::misc::{MessageID, REPLY_ID_NONE};
    use crate::nub_exception::NubException;
    use crate::reply_order::ReplyOrder;
    use crate::request_manager::RequestManager;
    use std::cell::RefCell;
    use std::rc::Rc;

    // ------------------------------------------------------------------
    // Test doubles
    // ------------------------------------------------------------------

    /// A handler that records `( id, reply id, body bytes )` per delivery
    /// and consumes its whole body.
    #[derive(Default)]
    struct Recorder {
        notes: RefCell<Vec<(MessageID, ReplyID, Vec<u8>)>>,
    }

    impl Recorder {
        fn notes(&self) -> Vec<(MessageID, ReplyID, Vec<u8>)> {
            self.notes.borrow().clone()
        }
    }

    impl InputMessageHandler for Recorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            let body = data.unread_bytes().to_vec();
            data.finish();
            self.notes
                .borrow_mut()
                .push((header.identifier(), header.reply_id(), body));
        }
    }

    /// A handler for `CALLBACK_LENGTH_MESSAGE` elements: answers every
    /// size question with the one number it was built with, and records
    /// what it is handed like a [`Recorder`].
    struct SizingRecorder {
        answer: i32,
        notes: RefCell<Vec<(MessageID, ReplyID, Vec<u8>)>>,
    }

    impl SizingRecorder {
        fn notes(&self) -> Vec<(MessageID, ReplyID, Vec<u8>)> {
            self.notes.borrow().clone()
        }
    }

    impl InputMessageHandler for SizingRecorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            let body = data.unread_bytes().to_vec();
            data.finish();
            self.notes
                .borrow_mut()
                .push((header.identifier(), header.reply_id(), body));
        }

        fn get_message_stream_size(&self, _msg_id: MessageID) -> i32 {
            self.answer
        }
    }

    /// A handler that never consumes its body, the way a handler that
    /// reads nothing at all leaves the stream.
    struct Glutton;

    impl InputMessageHandler for Glutton {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &mut UnpackedMessageHeader,
            _data: &mut BinaryStream,
        ) {
        }
    }

    /// A reply handler that records `( arg, body bytes )`.
    #[derive(Default)]
    struct ReplyRecorder {
        notes: RefCell<Vec<(usize, Vec<u8>)>>,
    }

    impl ReplyRecorder {
        fn notes(&self) -> Vec<(usize, Vec<u8>)> {
            self.notes.borrow().clone()
        }
    }

    impl ReplyMessageHandler for ReplyRecorder {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &UnpackedMessageHeader,
            data: &mut BinaryStream,
            arg: usize,
        ) {
            self.notes
                .borrow_mut()
                .push((arg, data.unread_bytes().to_vec()));
        }

        fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
    }

    // ------------------------------------------------------------------
    // Frame builders: the sending side the reference's Bundle does, one
    // message at a time, into a body the walker will read back.
    // ------------------------------------------------------------------

    /// A fixed message: id byte and body, no length field.
    fn fill_fixed(body: &mut Vec<u8>, element: &InterfaceElement, payload: &[u8]) {
        body.push(element.id());
        body.extend_from_slice(payload);
    }

    /// A variable message: id, length field, body.
    fn fill_variable(body: &mut Vec<u8>, element: &InterfaceElement, payload: &[u8]) {
        let offset = body.len();
        body.push(element.id());
        body.resize(body.len() + element.length_param() as usize, 0);
        body.extend_from_slice(payload);
        element
            .compress_length(&mut body[offset..], payload.len() as i32, false)
            .unwrap();
    }

    /// A request-shaped message: id, length field, reply id, next-request
    /// link (left zero), body. Answers `( message offset, link slot )` so
    /// the chain can be patched once every offset is known.
    fn fill_request(
        body: &mut Vec<u8>,
        element: &InterfaceElement,
        reply_id: ReplyID,
        payload: &[u8],
    ) -> (usize, usize) {
        let offset = body.len();
        body.push(element.id());
        body.resize(body.len() + element.length_param() as usize, 0);
        body.extend_from_slice(&reply_id.to_le_bytes());
        let link_slot = body.len();
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(payload);
        element
            .compress_length(&mut body[offset..], payload.len() as i32, true)
            .unwrap();
        (offset, link_slot)
    }

    /// Patch the request links: each request points at the next one, the
    /// last at nothing, and the walk starts at the first. The values are
    /// wire offsets, packet-absolute like [`crate::udp_bundle`] writes
    /// them, so every body offset gains the flags word's two bytes.
    fn chain(links: &[(usize, usize)], body: &mut [u8]) -> Option<usize> {
        for (i, &(_, slot)) in links.iter().enumerate() {
            let next = links
                .get(i + 1)
                .map_or(0usize, |&(offset, _)| offset + crate::packet::HEADER_SIZE);
            body[slot..slot + NEXT_REQUEST_OFFSET_SIZE]
                .copy_from_slice(&(next as u16).to_le_bytes());
        }
        links
            .first()
            .map(|&(offset, _)| offset + crate::packet::HEADER_SIZE)
    }

    fn variable(id: MessageID, width: i32) -> InterfaceElement {
        InterfaceElement::new("variable", id, VARIABLE_LENGTH_MESSAGE, width, None)
    }

    fn fixed(id: MessageID, size: i32) -> InterfaceElement {
        InterfaceElement::new("fixed", id, FIXED_LENGTH_MESSAGE, size, None)
    }

    fn handler_of(recorder: &Rc<Recorder>) -> Rc<dyn InputMessageHandler> {
        Rc::clone(recorder) as Rc<dyn InputMessageHandler>
    }

    fn source() -> Address {
        Address::new(0x7f000001, 2000)
    }

    /// A `MessageFilter` that records each message it intercepts and then
    /// delivers it through the handler itself, the reference's
    /// `filterMessage( ..., pHandler )` idiom.
    #[derive(Default)]
    struct RecordingFilter {
        seen: RefCell<Vec<(MessageID, Vec<u8>)>>,
    }

    impl RecordingFilter {
        fn notes(&self) -> Vec<(MessageID, Vec<u8>)> {
            self.seen.borrow().clone()
        }
    }

    impl MessageFilter for RecordingFilter {
        fn filter_message(
            &self,
            src_addr: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
            handler: &dyn InputMessageHandler,
        ) {
            self.seen
                .borrow_mut()
                .push((header.identifier(), data.unread_bytes().to_vec()));
            handler.handle_message(src_addr, header, data);
        }
    }

    /// A filter that swallows its message: no forward, no delivery.
    struct SwallowingFilter;

    impl MessageFilter for SwallowingFilter {
        fn filter_message(
            &self,
            _src_addr: &Address,
            _header: &mut UnpackedMessageHeader,
            _data: &mut BinaryStream,
            _handler: &dyn InputMessageHandler,
        ) {
        }
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    /// An id no handler serves ends the walk with
    /// `REASON_NONEXISTENT_ENTRY`, and a good message sitting after it is
    /// never delivered.
    #[test]
    fn an_unhandled_message_id_discards_the_rest_of_the_bundle() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        let element = variable(5, 2);
        table.serve(&element, Some(handler_of(&known)));

        let mut body = Vec::new();
        fill_variable(&mut body, &variable(9, 2), b"rogue");
        fill_variable(&mut body, &element, b"good");

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::NonexistentEntry);
        assert!(
            known.notes().is_empty(),
            "the bundle is discarded at the unknown id"
        );
    }

    /// An element that was served without a handler is as unknown as an
    /// id nothing was served for: `pHandler() == NULL` is the test.
    #[test]
    fn a_registered_element_without_a_handler_is_unhandled() {
        let table = InterfaceTable::new();
        table.serve(&variable(5, 2), None);

        let mut body = Vec::new();
        fill_variable(&mut body, &variable(5, 2), b"body");

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::NonexistentEntry);
    }

    /// A channel's message filter takes the delivery instead of the
    /// handler, and it delivers by calling the handler itself
    /// (udp_bundle_processor.cpp:161-170).
    #[test]
    fn a_message_filter_takes_the_delivery() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        let element = variable(5, 2);
        table.serve(&element, Some(handler_of(&recorder)));
        let filter = RecordingFilter::default();

        let mut body = Vec::new();
        fill_variable(&mut body, &element, b"body");

        let reason = dispatch_messages(&table, &source(), &mut body, None, Some(&filter));

        assert_eq!(reason, Reason::Success);
        assert_eq!(filter.notes().len(), 1, "the filter saw the message");
        assert_eq!(recorder.notes().len(), 1, "the filter forwarded it");
        assert_eq!(recorder.notes()[0].2, b"body");
    }

    /// A filter that does not forward its message swallows it: the
    /// handler is never called, and the untouched body is reported and
    /// consumed like any other handler that leaves bytes.
    #[test]
    fn a_filter_that_does_not_forward_swallows_the_message() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&recorder)));

        let mut body = Vec::new();
        fill_variable(&mut body, &variable(5, 2), b"body");

        let reason = dispatch_messages(&table, &source(), &mut body, None, Some(&SwallowingFilter));

        assert_eq!(reason, Reason::Success);
        assert!(
            recorder.notes().is_empty(),
            "a filter that does not forward drops the message"
        );
    }

    /// A callback message whose handler declares the bundle invalid stops
    /// the walk with `REASON_CORRUPTED_PACKET` and delivers nothing after
    /// it.
    #[test]
    fn a_callback_message_that_refuses_its_size_is_corrupted() {
        let table = InterfaceTable::new();
        let refusing = Rc::new(SizingRecorder {
            answer: INVALID_STREAM_SIZE,
            notes: RefCell::new(Vec::new()),
        });
        table.serve(
            &InterfaceElement::new("callback", 6, CALLBACK_LENGTH_MESSAGE, 0, None),
            Some(Rc::clone(&refusing) as Rc<dyn InputMessageHandler>),
        );
        let later = Rc::new(Recorder::default());
        let plain = variable(7, 2);
        table.serve(&plain, Some(handler_of(&later)));

        let mut body = Vec::new();
        body.extend_from_slice(&[6, 1, 2, 3]);
        fill_variable(&mut body, &plain, b"tail");

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::CorruptedPacket);
        assert!(later.notes().is_empty(), "nothing after the failure");
    }

    /// A callback message is sized by its handler before the frame is
    /// read: a negative answer means a variable message of that width, a
    /// positive one a fixed message of that size, and both then unpack
    /// and deliver like any other message.
    #[test]
    fn a_callback_message_is_sized_by_its_handler() {
        // Negative answer: the magnitude is the width of the length field.
        let table = InterfaceTable::new();
        let sized = Rc::new(SizingRecorder {
            answer: -3,
            notes: RefCell::new(Vec::new()),
        });
        table.serve(
            &InterfaceElement::new("callback", 6, CALLBACK_LENGTH_MESSAGE, 0, None),
            Some(Rc::clone(&sized) as Rc<dyn InputMessageHandler>),
        );

        let mut body = Vec::new();
        body.push(6);
        body.extend_from_slice(&[3, 0, 0]); // three-byte length field
        body.extend_from_slice(b"abc");

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);
        assert_eq!(reason, Reason::Success);
        assert_eq!(sized.notes(), vec![(6, REPLY_ID_NONE, b"abc".to_vec())]);

        // Positive answer: the value is the fixed body size.
        let table = InterfaceTable::new();
        let sized = Rc::new(SizingRecorder {
            answer: 4,
            notes: RefCell::new(Vec::new()),
        });
        table.serve(
            &InterfaceElement::new("callback", 6, CALLBACK_LENGTH_MESSAGE, 0, None),
            Some(Rc::clone(&sized) as Rc<dyn InputMessageHandler>),
        );

        let mut body = Vec::new();
        body.push(6);
        body.extend_from_slice(b"abcd"); // a fixed message writes no field

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);
        assert_eq!(reason, Reason::Success);
        assert_eq!(sized.notes(), vec![(6, REPLY_ID_NONE, b"abcd".to_vec())]);
    }

    /// A message whose header runs past the end of the body is a
    /// corrupted bundle.
    #[test]
    fn a_truncated_header_is_corrupted() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&got)));

        let mut body = Vec::new();
        body.push(5);
        body.push(3); // one byte of a two-byte length field, then nothing

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::CorruptedPacket);
        assert!(got.notes().is_empty());
    }

    /// A four-byte length field with the top bit set overflows the int
    /// the length is read into, which is a corrupted bundle.
    #[test]
    fn an_overflowing_length_field_is_corrupted() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        table.serve(&variable(5, 4), Some(handler_of(&got)));

        let mut body = Vec::new();
        body.push(5);
        body.extend_from_slice(&0x8000_0000u32.to_le_bytes());

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::CorruptedPacket);
        assert!(got.notes().is_empty());
    }

    /// A request whose body ends before the reply id and next-request
    /// link are both in is a corrupted bundle.
    #[test]
    fn a_request_without_room_for_its_preamble_is_corrupted() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&got)));

        let mut body = Vec::new();
        body.push(5);
        body.extend_from_slice(&4u16.to_le_bytes());
        body.extend_from_slice(&77i32.to_le_bytes()); // reply id, no link

        let reason = dispatch_messages(
            &table,
            &source(),
            &mut body,
            Some(crate::packet::HEADER_SIZE),
            None,
        );

        assert_eq!(reason, Reason::CorruptedPacket);
        assert!(got.notes().is_empty());
    }

    /// A request link pointing outside the body is a corrupted bundle,
    /// and the message the link belongs to is not delivered either.
    #[test]
    fn a_request_link_out_of_bounds_is_corrupted() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        let element = variable(5, 2);
        table.serve(&element, Some(handler_of(&got)));

        let mut body = Vec::new();
        let (_, slot) = fill_request(&mut body, &element, 77, b"abc");
        body[slot..slot + NEXT_REQUEST_OFFSET_SIZE].copy_from_slice(&5000u16.to_le_bytes());

        let reason = dispatch_messages(
            &table,
            &source(),
            &mut body,
            Some(crate::packet::HEADER_SIZE),
            None,
        );

        assert_eq!(reason, Reason::CorruptedPacket);
        assert!(got.notes().is_empty());
    }

    /// A length field claiming more body than the bundle holds is a
    /// corrupted bundle.
    #[test]
    fn a_short_payload_is_corrupted() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&got)));

        let mut body = Vec::new();
        body.push(5);
        body.extend_from_slice(&10u16.to_le_bytes());
        body.extend_from_slice(b"ab"); // claims ten, carries two

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::CorruptedPacket);
        assert!(got.notes().is_empty());
    }

    /// A good bundle delivers every message, in wire order, each with its
    /// own body and no reply id, and reports success.
    #[test]
    fn a_good_bundle_delivers_every_message_in_order() {
        let table = InterfaceTable::new();
        let first = Rc::new(Recorder::default());
        let second = Rc::new(Recorder::default());
        let first_element = fixed(3, 4);
        let second_element = variable(4, 2);
        table.serve(&first_element, Some(handler_of(&first)));
        table.serve(&second_element, Some(handler_of(&second)));

        let mut body = Vec::new();
        fill_fixed(&mut body, &first_element, b"ABCD");
        fill_variable(&mut body, &second_element, b"hello");

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::Success);
        assert_eq!(first.notes(), vec![(3, REPLY_ID_NONE, b"ABCD".to_vec())]);
        assert_eq!(second.notes(), vec![(4, REPLY_ID_NONE, b"hello".to_vec())]);
    }

    /// A request in a bundle is delivered with the reply id its sender
    /// wrote in the preamble.
    #[test]
    fn a_request_in_a_bundle_carries_its_reply_id() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        let element = variable(5, 2);
        table.serve(&element, Some(handler_of(&got)));

        let mut body = Vec::new();
        fill_request(&mut body, &element, 77, b"pay");

        let reason = dispatch_messages(
            &table,
            &source(),
            &mut body,
            Some(crate::packet::HEADER_SIZE),
            None,
        );

        assert_eq!(reason, Reason::Success);
        assert_eq!(got.notes(), vec![(5, 77, b"pay".to_vec())]);
    }

    /// The request links chain the walk: two requests with a plain
    /// message sitting between them are all delivered, each request
    /// carrying its own reply id and the plain one none.
    #[test]
    fn two_requests_chain_and_a_plain_message_sits_between() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        let element = variable(5, 2);
        table.serve(&element, Some(handler_of(&got)));

        let mut body = Vec::new();
        let first = fill_request(&mut body, &element, 101, b"one");
        fill_variable(&mut body, &element, b"mid");
        let second = fill_request(&mut body, &element, 202, b"two");
        let start = chain(&[first, second], &mut body);

        let reason = dispatch_messages(&table, &source(), &mut body, start, None);

        assert_eq!(reason, Reason::Success);
        assert_eq!(
            got.notes(),
            vec![
                (5, 101, b"one".to_vec()),
                (5, REPLY_ID_NONE, b"mid".to_vec()),
                (5, 202, b"two".to_vec()),
            ]
        );
    }

    /// A reply message on the wire is delivered through the table to the
    /// request manager serving it, and the pending request it answers is
    /// gone: the walk, the table and the manager close the loop.
    #[test]
    fn a_reply_in_a_bundle_completes_its_pending_request() {
        use std::cell::Cell;

        let now = Rc::new(Cell::new(1_000_000u64));
        let clock = Rc::clone(&now);
        let dispatcher =
            crate::event_dispatcher::EventDispatcher::with_clock(Box::new(move || clock.get()));

        let manager = Rc::new(RequestManager::new(false, Rc::new(dispatcher)));
        let replies = Rc::new(ReplyRecorder::default());
        let order = ReplyOrder::new(
            Rc::clone(&replies) as Rc<dyn ReplyMessageHandler>,
            99,
            5_000_000,
            0,
        );
        let pending_id = manager
            .add_reply_order(&order, None, &mut [0u8; 4])
            .unwrap();

        // The failure hook rides along on the double; nothing in a
        // delivered reply reaches it, so it is exercised by hand.
        replies.handle_exception(&NubException::new(Reason::TimerExpired, None), 99);
        assert!(replies.notes().is_empty());

        let table = InterfaceTable::new();
        table.serve(
            &InterfaceElement::reply(),
            Some(Rc::clone(&manager) as Rc<dyn InputMessageHandler>),
        );

        // The wire frame: a request-shaped reply whose body answers
        // `pending_id` and carries the answer payload after it.
        let mut body = Vec::new();
        let mut answer = pending_id.to_le_bytes().to_vec();
        answer.extend_from_slice(b"done");
        fill_request(&mut body, &InterfaceElement::reply(), 7, &answer);

        let reason = dispatch_messages(
            &table,
            &source(),
            &mut body,
            Some(crate::packet::HEADER_SIZE),
            None,
        );

        assert_eq!(reason, Reason::Success);
        assert_eq!(replies.notes(), vec![(99, b"done".to_vec())]);
        assert_eq!(manager.num_pending(), 0, "the request is answered");
    }

    /// Every delivery is counted against the element that served it: one
    /// message and its body length each, and the ids that saw nothing
    /// stay at nothing.
    #[test]
    fn delivered_messages_are_counted_against_their_elements() {
        let table = InterfaceTable::new();
        let first = Rc::new(Recorder::default());
        let second = Rc::new(Recorder::default());
        let first_element = fixed(3, 4);
        let second_element = variable(4, 1);
        table.serve(&first_element, Some(handler_of(&first)));
        table.serve(&second_element, Some(handler_of(&second)));

        let mut body = Vec::new();
        fill_fixed(&mut body, &first_element, b"ABCD");
        fill_variable(&mut body, &second_element, b"xyz");

        dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(table.element_stats(3).num_messages_received(), 1);
        assert_eq!(table.element_stats(3).num_bytes_received(), 4);
        assert_eq!(table.element_stats(4).num_messages_received(), 1);
        assert_eq!(table.element_stats(4).num_bytes_received(), 3);
        assert_eq!(table.element_stats(5).num_messages_received(), 0);
    }

    /// A handler that leaves its body uneaten does not stop the walk: the
    /// leftover is reported and finished off, and the next message is
    /// still delivered.
    #[test]
    fn a_handler_that_leaves_bytes_does_not_stop_the_walk() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        let element = variable(5, 2);
        table.serve(&variable(4, 2), Some(Rc::new(Glutton)));
        table.serve(&element, Some(handler_of(&got)));

        let mut body = Vec::new();
        fill_variable(&mut body, &variable(4, 2), b"waste");
        fill_variable(&mut body, &element, b"next");

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::Success);
        assert_eq!(got.notes(), vec![(5, REPLY_ID_NONE, b"next".to_vec())]);
    }

    /// An empty bundle has no messages to walk and is a success.
    #[test]
    fn an_empty_bundle_is_a_success() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&got)));

        let reason = dispatch_messages(&table, &source(), &mut Vec::new(), None, None);

        assert_eq!(reason, Reason::Success);
        assert!(got.notes().is_empty());
    }

    /// A body too big for its length field arrives in the special form:
    /// the four-byte length at the head of the body is undone, the
    /// handler sees the restored body at its declared length, and the
    /// four stale tail bytes the form leaves are stepped over.
    #[test]
    fn an_oversize_body_walks_its_special_form() {
        let table = InterfaceTable::new();
        let got = Rc::new(Recorder::default());
        let element = variable(5, 1);
        table.serve(&element, Some(handler_of(&got)));

        // The reference's Bundle reserves four spare bytes before
        // compressing an oversize body; the test does the same.
        let payload: Vec<u8> = (1..=255u8).collect();
        let mut body = Vec::new();
        body.push(element.id());
        body.push(0); // the one-byte length field, to be marked 0xff
        body.extend_from_slice(&payload);
        body.extend_from_slice(&[0; SPECIAL_LENGTH_SIZE]);
        element
            .compress_length(&mut body, payload.len() as i32, false)
            .unwrap();
        assert_eq!(body[1], 0xff, "the length field is marked for the form");

        let reason = dispatch_messages(&table, &source(), &mut body, None, None);

        assert_eq!(reason, Reason::Success);
        let (id, reply, bytes) = got.notes().remove(0);
        assert_eq!((id, reply), (5, REPLY_ID_NONE));
        assert_eq!(bytes.len(), 255, "the handler sees the declared length");
        assert_eq!(bytes, payload, "the displaced head is put back");
    }
}
