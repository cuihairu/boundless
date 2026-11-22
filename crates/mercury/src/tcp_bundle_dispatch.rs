//! The receive-side walk over a complete TCP frame: `Mercury::
//! TCPBundleProcessor::dispatchMessages`
//! (`lib/network/tcp_bundle_processor.cpp:47-235`).
//!
//! The reference keeps the TCP walk apart from the UDP one because a TCP
//! frame is one contiguous bundle, not a packet chain, and its wire terms
//! differ: `TCPBundle::Flags` is a `uint8` and `TCPBundle::Offset` is a
//! `uint32` (`tcp_bundle.hpp:28-32`) -- a request's offset is four bytes
//! wide where a UDP packet's is two. The frame's flags word sits at
//! offset 0, so the walk's positions are already in the same terms as the
//! request links the footer carries (no packet-`HEADER_SIZE` shift the UDP
//! walk needs); the flags word is read here and the bundle is rejected
//! when it does not match `FLAG_MASK`.
//!
//! The walk is otherwise the UDP walk's shape: id to [`InterfaceTable`],
//! length resolution through [`InterfaceElement::expand_length`], the
//! request footer (reply id, then the next request's offset) read off a
//! request, the body handed to the served handler, and the walk stopping
//! on the first disagreement with the wire. Every error reports the
//! reference's message and leaves the rest of the bundle undelivered.
//!
//! Left with the channel layer (as the UDP walk's): the per-channel
//! `MessageFilter` (`channel.pMessageFilter()`), the break-loop hook a
//! dying channel pulls (`header.pBreakLoop`), and the profile stats.

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interface_element::REPLY_MESSAGE_IDENTIFIER;
use crate::interface_table::InterfaceTable;
use crate::misc::{Reason, ReplyID};
use crate::unpacked_message_header::{NO_REPLY, UnpackedMessageHeader};

/// `TCPBundle::FLAG_HAS_REQUESTS` (`tcp_bundle.hpp:29`).
pub const FLAG_HAS_REQUESTS: u8 = 0x01;

/// `TCPBundle::FLAG_MASK` (`tcp_bundle.hpp:30`).
const FLAG_MASK: u8 = 0x01;

/// `sizeof( TCPBundle::Flags )`: a `uint8`.
const FLAGS_SIZE: usize = 1;

/// `sizeof( TCPBundle::Offset )`: a `uint32`.
const OFFSET_SIZE: usize = 4;

/// `sizeof( ReplyID )`, the first half of a request footer.
const REPLY_ID_SIZE: usize = 4;

/// `REQUEST_FOOTER_SIZE`: `sizeof(ReplyID) + sizeof(Offset)` -- the width
/// of the reply id plus the next request's offset a request carries.
const REQUEST_FOOTER_SIZE: usize = REPLY_ID_SIZE + OFFSET_SIZE;

/// The four bytes a "special" length leaves at the tail of the body
/// (`TCPBundleProcessor` reads the 4-byte `specialLengthHeader` the same
/// way the UDP walk does).
const SPECIAL_LENGTH_SIZE: usize = 4;

/// `dispatchMessages( channel, interfaceTable )`: walk the messages
/// packed in the complete TCP frame and deliver each to the handler its
/// id is served with.
///
/// The reference returns the reason it stopped on (`REASON_SUCCESS` when
/// every message was delivered); the rest of a frame after an error is
/// never delivered.
pub fn dispatch_messages(table: &InterfaceTable, source: &Address, frame: &mut [u8]) -> Reason {
    if frame.len() < FLAGS_SIZE {
        return Reason::CorruptedPacket;
    }
    let flags = frame[0];
    if flags & !FLAG_MASK != 0 {
        eprintln!(
            "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after invalid flags: {:02x}",
            source.c_str(),
            flags
        );
        return Reason::CorruptedPacket;
    }

    // `offset` is the walk position relative to the frame's first byte,
    // which is the flags word. `nextRequestOffset` is the bundle-relative
    // offset of the next request, seeded from the frame's first-request
    // slot when the flags say there is one.
    let mut next_request_offset: Option<usize> = None;
    let mut offset = FLAGS_SIZE;
    if flags & FLAG_HAS_REQUESTS != 0 {
        if frame.len() < FLAGS_SIZE + OFFSET_SIZE {
            return Reason::CorruptedPacket;
        }
        let first = read_offset(frame, FLAGS_SIZE);
        if first > frame.len() {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after out-of-bounds first request offset (got offset {} > {} bundle length)",
                source.c_str(),
                first,
                frame.len()
            );
            return Reason::CorruptedPacket;
        }
        next_request_offset = Some(first);
        offset = FLAGS_SIZE + OFFSET_SIZE;
    }

    let bundle_length = frame.len();

    // `while ( data_.remainingLength() && !shouldBreakLoop )`.
    while offset < bundle_length {
        let msg_id = frame[offset];
        let served = table.element(msg_id);
        let handler = match served.handler() {
            Some(handler) => handler,
            None => {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after hitting unhandled message ID {}",
                    source.c_str(),
                    msg_id
                );
                return Reason::NonexistentEntry;
            }
        };

        let mut updated = served.clone();
        if !updated.update_length_details(handler.as_ref()) {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after failure to update length details for message ID {}",
                source.c_str(),
                msg_id
            );
            return Reason::CorruptedPacket;
        }

        // `isRequest = ( offset == nextRequestOffset )`: both sides count
        // from the frame's first byte, so no shift is needed.
        let is_request = next_request_offset == Some(offset);

        let length = match updated.expand_length(&mut frame[offset..], is_request) {
            Ok(length) => length,
            Err(_) => {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Error unpacking header length at {} for message ID {}",
                    source.c_str(),
                    offset,
                    msg_id
                );
                return Reason::CorruptedPacket;
            }
        };

        let header_size = updated.header_size().max(0) as usize;
        let mut msg_beg = offset + header_size;

        // A variable message's special-length form leaves a four-byte
        // count at the body's tail; the length already accounts for the
        // head, the advance accounts for the tail.
        let mut data_length = length.max(0) as usize;
        if !updated.can_handle_length_for(length) {
            data_length += SPECIAL_LENGTH_SIZE;
        }

        let reply_id: ReplyID;
        if is_request {
            if msg_beg + REQUEST_FOOTER_SIZE > bundle_length {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Not enough data on stream at {} for request ID and NRO ({} left, needed {})",
                    source.c_str(),
                    offset,
                    bundle_length - msg_beg,
                    REQUEST_FOOTER_SIZE
                );
                return Reason::CorruptedPacket;
            }
            reply_id = read_reply_id(frame, msg_beg);
            let link = read_offset(frame, msg_beg + REPLY_ID_SIZE);
            if link > bundle_length {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Discarding rest of bundle due to out-of-bounds next request offset (got offset {} > {} byte bundle length)",
                    source.c_str(),
                    link,
                    bundle_length
                );
                return Reason::CorruptedPacket;
            }
            next_request_offset = Some(link);
            msg_beg += REQUEST_FOOTER_SIZE;
        } else {
            reply_id = NO_REPLY;
        }

        if msg_beg + length.max(0) as usize > bundle_length {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding rest of bundle since chain too short for data of message ID {} length {} (have {} bytes)",
                source.c_str(),
                msg_id,
                length,
                bundle_length - msg_beg
            );
            return Reason::CorruptedPacket;
        }

        let mut header = UnpackedMessageHeader::new(msg_id);
        header.set_reply_id(reply_id);
        header.set_length(length);
        header.set_interface_element(served.clone());

        let mut stream = BinaryStream::new();
        stream.write_bytes(&frame[msg_beg..msg_beg + length.max(0) as usize]);
        handler.handle_message(source, &mut header, &mut stream);

        table.record_received(msg_id, length.max(0) as u32);

        if stream.remaining() != 0 {
            if msg_id == REPLY_MESSAGE_IDENTIFIER {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Handler for reply left {} bytes",
                    source.c_str(),
                    stream.remaining()
                );
            } else {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Handler for message {} (ID {}) left {} bytes",
                    source.c_str(),
                    served.name(),
                    msg_id,
                    stream.remaining()
                );
                stream.finish();
            }
        }

        offset = msg_beg + data_length;
    }

    Reason::Success
}

/// The little-endian read of a `TCPBundle::Offset` (`uint32`).
fn read_offset(frame: &[u8], at: usize) -> usize {
    u32::from_le_bytes(frame[at..at + OFFSET_SIZE].try_into().unwrap()) as usize
}

/// The little-endian read of a request's `ReplyID`.
fn read_reply_id(frame: &[u8], at: usize) -> ReplyID {
    ReplyID::from_le_bytes(frame[at..at + REPLY_ID_SIZE].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_element::{
        CALLBACK_LENGTH_MESSAGE, FIXED_LENGTH_MESSAGE, INVALID_STREAM_SIZE, InterfaceElement,
        VARIABLE_LENGTH_MESSAGE,
    };
    use crate::interfaces::InputMessageHandler;
    use crate::misc::MessageID;
    use crate::unpacked_message_header::UnpackedMessageHeader;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A handler that records each `( id, body )` it was handed.
    #[derive(Clone, Default)]
    struct Recorder {
        delivered: RefCell<Vec<(u8, Vec<u8>)>>,
    }

    impl InputMessageHandler for Recorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            self.delivered
                .borrow_mut()
                .push((header.identifier(), data.unread_bytes().to_vec()));
        }
    }

    /// A table serving `id` with a fixed-length handler.
    fn table_with(id: MessageID, length: i32, recorder: &Rc<Recorder>) -> InterfaceTable {
        let table = InterfaceTable::new();
        let ie = InterfaceElement::new("test", id, FIXED_LENGTH_MESSAGE, length, None);
        table.serve(
            &ie,
            Some(Rc::clone(recorder) as Rc<dyn InputMessageHandler>),
        );
        table
    }

    /// A plain message frame: `[flags][id][body]`.
    fn plain_frame(id: MessageID, body: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8];
        f.push(id);
        f.extend_from_slice(body);
        f
    }

    /// A request frame: `[flags|HAS_REQUESTS][firstReqOffset:4][id][replyID:4][link:4][body]`.
    fn request_frame(id: MessageID, body: &[u8], first: usize) -> Vec<u8> {
        let mut f = vec![FLAG_HAS_REQUESTS];
        f.extend_from_slice(&(first as u32).to_le_bytes());
        f.push(id);
        f.extend_from_slice(&0x12345678u32.to_le_bytes()); // reply id
        f.extend_from_slice(&(first as u32).to_le_bytes()); // next link
        f.extend_from_slice(body);
        f
    }

    /// A frame with no flags word at all is corrupted.
    #[test]
    fn a_frame_with_no_flags_word_is_corrupted() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x01, 4, &recorder);
        let mut frame = vec![];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
    }

    /// A flags word outside `FLAG_MASK` is discarded.
    #[test]
    fn an_invalid_flags_word_is_discarded() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x01, 4, &recorder);
        let mut frame = vec![0x02];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
    }

    /// A plain message is delivered to the handler its id is served with.
    #[test]
    fn a_plain_message_is_delivered() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x07, 4, &recorder);
        let mut frame = plain_frame(0x07, &[0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::Success
        );
        assert_eq!(
            recorder.delivered.borrow().clone(),
            vec![(0x07, vec![0xAA, 0xBB, 0xCC, 0xDD])]
        );
    }

    /// A request's footer is read off the frame and its reply id is
    /// handed to the handler, the link chaining the next request.
    #[test]
    fn a_request_is_delivered_with_its_reply_id_and_link() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x09, 2, &recorder);
        // first request at offset 5 (flags + slot), body 2 bytes.
        let mut frame = request_frame(0x09, &[0x01, 0x02], 5);
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::Success
        );
        let (id, body) = &recorder.delivered.borrow().clone()[0];
        assert_eq!(*id, 0x09);
        assert_eq!(*body, vec![0x01, 0x02]);
    }

    /// Two messages packed in one frame are delivered in order.
    #[test]
    fn two_messages_in_one_frame_come_back_in_order() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x03, 1, &recorder);
        let mut frame = vec![0u8, 0x03, 0x01, 0x03, 0x02];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::Success
        );
        assert_eq!(
            recorder.delivered.borrow().clone(),
            vec![(0x03, vec![0x01]), (0x03, vec![0x02])]
        );
    }

    /// An id no handler serves stops the walk, and the rest of the frame
    /// is never delivered.
    #[test]
    fn an_unhandled_id_stops_the_walk() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x03, 1, &recorder);
        let mut frame = vec![0u8, 0x03, 0x01, 0x2F, 0x02];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::NonexistentEntry
        );
        assert_eq!(
            recorder.delivered.borrow().clone(),
            vec![(0x03, vec![0x01])],
            "the message before the bad id is delivered, the rest is not"
        );
    }

    /// A request whose next-request offset points outside the frame is
    /// corrupted.
    #[test]
    fn an_out_of_bounds_next_request_offset_is_corrupted() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x09, 2, &recorder);
        let mut frame = request_frame(0x09, &[0x01, 0x02], 5);
        // Patch the link to a bogus value far past the frame.
        let link_at = 1 + 4 + 1 + 4; // flags + slot + id + reply id
        frame[link_at..link_at + 4].copy_from_slice(&999u32.to_le_bytes());
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
        assert_eq!(recorder.delivered.borrow().len(), 0);
    }

    /// A variable-length message is resolved through its length field.
    #[test]
    fn a_variable_length_message_resolves_its_length() {
        let recorder = Rc::new(Recorder::default());
        let table = InterfaceTable::new();
        let ie = InterfaceElement::new("var", 0x05, VARIABLE_LENGTH_MESSAGE, 1, None);
        table.serve(
            &ie,
            Some(Rc::clone(&recorder) as Rc<dyn InputMessageHandler>),
        );

        // [flags][id][len:1][body]
        let mut frame = vec![0u8, 0x05, 0x02, 0x11, 0x22];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::Success
        );
        assert_eq!(
            recorder.delivered.borrow().clone(),
            vec![(0x05, vec![0x11, 0x22])]
        );
    }

    // -- the walk's error arms and the special form -----------------------

    /// A handler whose `get_message_stream_size` answer is fixed, for the
    /// callback-length messages a [`SizingRecorder`]-style table serves.
    struct SizingRecorder {
        answer: i32,
        delivered: RefCell<Vec<(u8, Vec<u8>)>>,
    }

    impl InputMessageHandler for SizingRecorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            self.delivered
                .borrow_mut()
                .push((header.identifier(), data.unread_bytes().to_vec()));
        }

        fn get_message_stream_size(&self, _msg_id: MessageID) -> i32 {
            self.answer
        }
    }

    /// A flags word that says requests are present but the frame ends
    /// before the first-request slot does is corrupted.
    #[test]
    fn a_request_flag_with_no_room_for_its_offset_is_corrupted() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x07, 4, &recorder);
        // Only the flags byte, no slot.
        let mut frame = vec![FLAG_HAS_REQUESTS];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
    }

    /// A first-request offset past the end of the frame is discarded.
    #[test]
    fn an_out_of_bounds_first_request_offset_is_corrupted() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x07, 4, &recorder);
        let mut frame = vec![FLAG_HAS_REQUESTS];
        frame.extend_from_slice(&999u32.to_le_bytes());
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
    }

    /// A callback message whose handler refuses to size it (`INVALID_-
    /// STREAM_SIZE`) stops the walk.
    #[test]
    fn a_callback_message_that_refuses_its_size_is_corrupted() {
        let table = InterfaceTable::new();
        let refusing = Rc::new(SizingRecorder {
            answer: INVALID_STREAM_SIZE,
            delivered: RefCell::new(Vec::new()),
        });
        table.serve(
            &InterfaceElement::new("cb", 0x06, CALLBACK_LENGTH_MESSAGE, 0, None),
            Some(Rc::clone(&refusing) as Rc<dyn InputMessageHandler>),
        );
        let mut frame = vec![0u8, 0x06];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
    }

    /// A variable message whose length field is truncated is corrupted.
    #[test]
    fn a_truncated_length_field_is_corrupted() {
        let recorder = Rc::new(Recorder::default());
        let table = InterfaceTable::new();
        let ie = InterfaceElement::new("var", 0x05, VARIABLE_LENGTH_MESSAGE, 2, None);
        table.serve(
            &ie,
            Some(Rc::clone(&recorder) as Rc<dyn InputMessageHandler>),
        );
        // [flags][id][one byte of a two-byte length field]
        let mut frame = vec![0u8, 0x05, 0x02];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
    }

    /// A body too big for its length field arrives in the special form:
    /// the four-byte length at the head is undone, the handler sees the
    /// restored body, and the four stale tail bytes are stepped over.
    #[test]
    fn an_oversize_body_walks_its_special_form() {
        let recorder = Rc::new(Recorder::default());
        let table = InterfaceTable::new();
        let ie = InterfaceElement::new("var", 0x05, VARIABLE_LENGTH_MESSAGE, 1, None);
        table.serve(
            &ie,
            Some(Rc::clone(&recorder) as Rc<dyn InputMessageHandler>),
        );

        let payload: Vec<u8> = (1..=255u8).collect();
        let mut frame = vec![0u8, 0x05, 0];
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&[0; SPECIAL_LENGTH_SIZE]);
        // The length field is compressed from the id byte, which is where
        // `body_offset` counts from.
        ie.compress_length(&mut frame[1..], payload.len() as i32, false)
            .unwrap();
        assert_eq!(frame[2], 0xff, "the length field is marked for the form");

        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::Success
        );
        assert_eq!(
            recorder.delivered.borrow().clone(),
            vec![(0x05, payload)],
            "the displaced head is put back"
        );
    }

    /// A request whose footer runs past the end of the frame is corrupted.
    #[test]
    fn a_request_without_room_for_its_footer_is_corrupted() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(0x09, 2, &recorder);
        // [flags|HAS_REQUESTS][first:4][id][reply id, no link]
        let mut frame = vec![FLAG_HAS_REQUESTS];
        frame.extend_from_slice(&5u32.to_le_bytes());
        frame.push(0x09);
        frame.extend_from_slice(&0x12345678u32.to_le_bytes());
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
    }

    /// A length field claiming more body than the frame holds is corrupted.
    #[test]
    fn a_short_payload_is_corrupted() {
        let recorder = Rc::new(Recorder::default());
        let table = InterfaceTable::new();
        let ie = InterfaceElement::new("var", 0x05, VARIABLE_LENGTH_MESSAGE, 2, None);
        table.serve(
            &ie,
            Some(Rc::clone(&recorder) as Rc<dyn InputMessageHandler>),
        );
        // [flags][id][len:10][two bytes] -- claims ten, carries two.
        let mut frame = vec![0u8, 0x05];
        frame.extend_from_slice(&10u16.to_le_bytes());
        frame.extend_from_slice(b"ab");
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::CorruptedPacket
        );
    }

    /// A reply whose handler leaves bytes is reported but does not stop
    /// the walk -- the reply arm's trace is the only side effect.
    #[test]
    fn a_reply_handler_that_leaves_bytes_is_reported() {
        let recorder = Rc::new(Recorder::default());
        let table = table_with(REPLY_MESSAGE_IDENTIFIER, 2, &recorder);
        // A reply body the handler does not consume.
        let mut frame = vec![0u8, REPLY_MESSAGE_IDENTIFIER, 0xAA, 0xBB];
        assert_eq!(
            dispatch_messages(&table, &Address::new(1, 2), &mut frame),
            Reason::Success,
            "the leftover bytes are reported, not fatal"
        );
    }
}
