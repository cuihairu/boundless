//! The TCP frame layer: the length-delimited framing `TCPChannel::handleInput`
//! runs and the bundle frame parse `TCPBundleProcessor::dispatchMessages`
//! (`lib/network/tcp_bundle.{hpp,cpp}`, `tcp_bundle_processor.cpp`,
//! `tcp_channel.cpp`).
//!
//! A TCP connection is a length-delimited byte stream:
//!
//! ```text
//! [ frameLength ][ flags ][ firstRequestOffset? ][ messages ]
//! ```
//!
//! `frameLength` is the bundle size -- a 16-bit value, or `0xFFFF` followed by
//! a 32-bit value when the bundle does not fit 16 bits. `flags` is one byte;
//! when it has [`FLAG_HAS_REQUESTS`], the 32-bit `firstRequestOffset` follows,
//! marking the bundle's first request. The message stream is the rest.
//!
//! Every multi-byte field in the frame -- the frame length, the request
//! offset, the message length fields, a request's reply id -- is network byte
//! order (big-endian), because the reference streams it through the
//! `bw_netshort`/`bw_netlong` wrappers. This is a deliberate divergence from
//! the port's shared [`crate::message_framing`], which writes message length
//! fields little-endian; that shared framing's alignment with the reference is
//! tracked with the BW-02-03 work it belongs to, and the TCP walk below is
//! faithful to the reference on its own.
//!
//! The reference's oversize form is also different over TCP: `expandLength(,
//! NULL, isRequest )` has no packet chain to walk, so it cannot do the UDP
//! displacement trick. It reports the length field as all-ones and the
//! processor reads the true 32-bit length that immediately follows the field,
//! with the body undispaced.

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interface_element::{
    FIXED_LENGTH_MESSAGE, InterfaceElement, REPLY_MESSAGE_IDENTIFIER, VARIABLE_LENGTH_MESSAGE,
};
use crate::interface_table::InterfaceTable;
use crate::message_framing::{REPLY_ID_SIZE, SPECIAL_LENGTH_SIZE};
use crate::misc::{REPLY_ID_NONE, Reason, ReplyID};
use crate::unpacked_message_header::UnpackedMessageHeader;

/// `TCPBundle::FLAG_HAS_REQUESTS` (tcp_bundle.hpp:29): the bundle carries at
/// least one request, so a first request offset follows the flags byte.
pub const FLAG_HAS_REQUESTS: u8 = 0x01;
/// `TCPBundle::FLAG_MASK` (tcp_bundle.hpp:30): the one flag that exists.
pub const FLAG_MASK: u8 = 0x01;

/// `readSmallFrameLength`: a 16-bit frame length header.
pub const SMALL_FRAME_HEADER_SIZE: usize = 2;
/// `readSmallFrameLength` + `readLargeFrameLength`: `0xFFFF` then a 32-bit
/// frame length.
pub const LARGE_FRAME_HEADER_SIZE: usize = SMALL_FRAME_HEADER_SIZE + 4;

/// The 16-bit sentinel that switches the frame length to its 32-bit form.
const SMALL_FRAME_MAX: u16 = 0xFFFF;
/// The width of the bundle's first request offset (`TCPBundle::Offset`, a
/// 32-bit value, unlike the UDP `Packet::Offset`).
const FIRST_REQUEST_OFFSET_SIZE: usize = 4;
/// The request preamble width: the reply id and the next request offset, both
/// 32-bit over TCP.
const TCP_REQUEST_PREAMBLE_SIZE: usize = REPLY_ID_SIZE + FIRST_REQUEST_OFFSET_SIZE;

/// One unpacked message header the walk hands the handler.
struct MessageHeader {
    /// The body length in bytes.
    length: usize,
    /// The bytes the header occupies before the body: id, length field, the
    /// oversize length, and for a request the reply id and next offset.
    header_size: usize,
    reply_id: ReplyID,
    /// The next request's offset, or [`None`] when the request link is the
    /// "no next request" value.
    next_request_offset: Option<usize>,
}

/// `TCPChannel::handleInput`'s framing loop: turn a byte stream into complete
/// frames, whatever the read sizes happen to be.
///
/// A short read (a frame arriving across several `feed`s) and a merged read
/// (several frames in one `feed`) both come out of the same state machine: the
/// reader buffers bytes, decodes the frame length once its header is present,
/// and yields each frame body once the full length has arrived, leaving any
/// leftover bytes for the next frame.
pub struct TcpFrameReader {
    /// The buffered, not-yet-consumed bytes.
    buf: Vec<u8>,
    /// The read cursor into `buf`: where the current frame starts.
    start: usize,
    /// The current frame's body length, once its header has been decoded.
    frame_len: Option<usize>,
    /// How many header bytes of the current frame have been consumed: 0 before
    /// the 16-bit length, 2 after it, 6 after a 32-bit length.
    header_consumed: usize,
}

impl Default for TcpFrameReader {
    fn default() -> Self {
        Self::new()
    }
}

impl TcpFrameReader {
    /// `pFrameData_( new MemoryOStream( maxSegmentSize ) )`: an empty buffer.
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
            frame_len: None,
            header_consumed: 0,
        }
    }

    /// `readInto( *pFrameData_ )`: append `data` to the buffered bytes.
    ///
    /// Any fully-consumed prefix is dropped first, so the buffer only ever
    /// holds the current (partial) frame and whatever follows it.
    pub fn feed(&mut self, data: &[u8]) {
        if self.start > 0 {
            self.buf.drain(0..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// `potentiallyMoreFrames`: whether a whole frame's worth of bytes is
    /// buffered, so the reference's loop would call `processReceivedFrame`.
    ///
    /// Decodes the length header as far as the buffered bytes allow first, so
    /// a freshly fed frame reports itself once its full length has arrived.
    pub fn has_frame(&mut self) -> bool {
        self.decode_length();
        match self.frame_len {
            Some(len) => self.buf.len() - self.start >= self.header_consumed + len,
            None => false,
        }
    }

    /// `processReceivedFrame`: the next complete frame body (the bundle data
    /// after the length header), if one is buffered.
    ///
    /// Returns the frame body and advances past it; a partial frame returns
    /// [`None`] and keeps its state for the next `feed`.
    pub fn next_frame(&mut self) -> Option<&[u8]> {
        self.decode_length();
        if let Some(len) = self.frame_len {
            let total = self.header_consumed + len;
            if self.buf.len() - self.start >= total {
                let frame_start = self.start + self.header_consumed;
                let frame = &self.buf[frame_start..frame_start + len];
                self.start = frame_start + len;
                self.frame_len = None;
                self.header_consumed = 0;
                return Some(frame);
            }
        }
        None
    }

    /// `readSmallFrameLength`/`readLargeFrameLength`: advance the length
    /// decode as far as the buffered bytes allow. A partial length header
    /// leaves the state for the next `feed`.
    fn decode_length(&mut self) {
        if self.frame_len.is_some() {
            return;
        }
        let avail = self.buf.len() - self.start;
        if self.header_consumed == 0 {
            if avail < SMALL_FRAME_HEADER_SIZE {
                return;
            }
            let small = u16::from_be_bytes([self.buf[self.start], self.buf[self.start + 1]]);
            self.header_consumed = SMALL_FRAME_HEADER_SIZE;
            if small != SMALL_FRAME_MAX {
                self.frame_len = Some(small as usize);
                return;
            }
        }
        if avail < LARGE_FRAME_HEADER_SIZE {
            return;
        }
        let s = self.start;
        let large = u32::from_be_bytes([
            self.buf[s + 2],
            self.buf[s + 3],
            self.buf[s + 4],
            self.buf[s + 5],
        ]);
        self.frame_len = Some(large as usize);
        self.header_consumed = LARGE_FRAME_HEADER_SIZE;
    }
}

/// `TCPBundleProcessor::dispatchMessages`: walk the messages packed in a frame
/// body and deliver each to the handler its id is served with.
///
/// `frame` is the bundle data the frame reader produced -- the flags byte, the
/// optional first request offset, then the messages. The reference returns the
/// reason it stopped on ([`Reason::Success`] when every message was
/// delivered); the rest of a bundle after an error is never delivered.
pub fn dispatch_messages(table: &InterfaceTable, source: &Address, frame: &mut [u8]) -> Reason {
    if frame.is_empty() {
        eprintln!(
            "TCPBundleProcessor::dispatchMessages( {} ): Empty frame",
            source.c_str()
        );
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

    let mut cursor = 1;
    let mut next_request_offset: Option<usize> = None;
    if flags & FLAG_HAS_REQUESTS != 0 {
        if frame.len() < 1 + FIRST_REQUEST_OFFSET_SIZE {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after short first request offset",
                source.c_str()
            );
            return Reason::CorruptedPacket;
        }
        let offset =
            u32::from_be_bytes(frame[1..1 + FIRST_REQUEST_OFFSET_SIZE].try_into().unwrap());
        // `if (int)nextRequestOffset > bundleLength`: the reference compares
        // signed, so a "no next request" value is not an error.
        if (offset as i32) > frame.len() as i32 {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after out-of-bounds first request offset (got offset {} > {} bundle length)",
                source.c_str(),
                offset,
                frame.len()
            );
            return Reason::CorruptedPacket;
        }
        next_request_offset = Some(offset as usize);
        cursor = 1 + FIRST_REQUEST_OFFSET_SIZE;
    }

    // `while (data_.remainingLength() && !shouldBreakLoop)`: the walk ends
    // when the messages run out or an error stops it.
    while cursor < frame.len() {
        let offset = cursor;
        // `isRequest = ( offset == nextRequestOffset )`: TCP offsets are
        // bundle-absolute from the flags byte, so there is no packet-header
        // shift the way the UDP walk applies one.
        let is_request = next_request_offset == Some(offset);
        let msg_id = frame[cursor];

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

        // `updatedIE_`: the callback sizing folds into a copy, so the served
        // entry keeps its `CALLBACK_LENGTH_MESSAGE` style for the next frame.
        let mut updated = served.clone();
        if !updated.update_length_details(handler.as_ref()) {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after failure to update length details for message ID {}",
                source.c_str(),
                msg_id
            );
            return Reason::CorruptedPacket;
        }

        let header = match read_message_header(&updated, frame, cursor, is_request) {
            Ok(header) => header,
            Err(reason) => return reason,
        };

        let body_start = cursor + header.header_size;
        if frame.len() - body_start < header.length {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding rest of bundle since chain too short for data of message ID {} length {} (have {} bytes)",
                source.c_str(),
                msg_id,
                header.length,
                frame.len() - body_start
            );
            return Reason::CorruptedPacket;
        }

        let mut unpacked = UnpackedMessageHeader::new(msg_id);
        unpacked.set_reply_id(header.reply_id);
        unpacked.set_interface_element(served.clone());
        unpacked.set_length(header.length as i32);

        let mut stream = BinaryStream::new();
        stream.write_bytes(&frame[body_start..body_start + header.length]);
        handler.handle_message(source, &mut unpacked, &mut stream);
        table.record_received(msg_id, header.length as u32);

        // A handler that did not eat its body is reported; a plain message's
        // leftovers are consumed off it, a reply's are not.
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

        if is_request {
            next_request_offset = header.next_request_offset;
        }
        cursor = body_start + header.length;
    }

    Reason::Success
}

/// `iterator::unpack` (TCPBundleProcessor): read one message's header out of
/// the frame at `start`, in the reference's TCP wire form.
///
/// The length field decodes big-endian; when a variable message's field is all
/// ones, the true 32-bit length follows the field and the body is undispaced.
/// Every failure returns [`Reason::CorruptedPacket`] the way the reference
/// answers `REASON_CORRUPTED_PACKET`.
fn read_message_header(
    element: &InterfaceElement,
    frame: &[u8],
    start: usize,
    is_request: bool,
) -> Result<MessageHeader, Reason> {
    let mut cursor = start;
    if cursor >= frame.len() {
        return Err(Reason::CorruptedPacket);
    }
    cursor += 1; // the message id byte

    let length = match element.length_style() {
        FIXED_LENGTH_MESSAGE => element.length_param().max(0) as usize,

        VARIABLE_LENGTH_MESSAGE => {
            let width = element.length_param().max(0) as usize;
            if frame.len() - cursor < width {
                return Err(Reason::CorruptedPacket);
            }
            let len = decode_be_length(&frame[cursor..cursor + width], width);
            cursor += width;

            if width == 4 {
                // A 32-bit field cannot overflow as an unsigned value, but the
                // reference reports the top-bit case as an overflow and reads
                // a length that is not there; either way the frame is corrupt.
                if len > i32::MAX as u32 {
                    return Err(Reason::CorruptedPacket);
                }
                len as usize
            } else if !element.can_handle_length_for(len as i32) {
                // The field is all ones: the true length follows it, in the
                // undispaced TCP form.
                if frame.len() - cursor < SPECIAL_LENGTH_SIZE {
                    return Err(Reason::CorruptedPacket);
                }
                let slen = u32::from_be_bytes(
                    frame[cursor..cursor + SPECIAL_LENGTH_SIZE]
                        .try_into()
                        .unwrap(),
                );
                if slen > i32::MAX as u32 {
                    return Err(Reason::CorruptedPacket);
                }
                cursor += SPECIAL_LENGTH_SIZE;
                slen as usize
            } else {
                len as usize
            }
        }

        // A callback that did not resolve, or an invalid style: the reference's
        // expandLength default arm reports the format and returns -1.
        _ => return Err(Reason::CorruptedPacket),
    };

    let mut reply_id = REPLY_ID_NONE;
    let mut next_request_offset = None;
    if is_request {
        if frame.len() - cursor < TCP_REQUEST_PREAMBLE_SIZE {
            return Err(Reason::CorruptedPacket);
        }
        reply_id =
            ReplyID::from_be_bytes(frame[cursor..cursor + REPLY_ID_SIZE].try_into().unwrap());
        cursor += REPLY_ID_SIZE;
        let nro = u32::from_be_bytes(
            frame[cursor..cursor + FIRST_REQUEST_OFFSET_SIZE]
                .try_into()
                .unwrap(),
        );
        // Signed compare, so the "no next request" value is not an error.
        if (nro as i32) > frame.len() as i32 {
            return Err(Reason::CorruptedPacket);
        }
        next_request_offset = Some(nro as usize);
        cursor += FIRST_REQUEST_OFFSET_SIZE;
    }

    Ok(MessageHeader {
        length,
        header_size: cursor - start,
        reply_id,
        next_request_offset,
    })
}

/// `BW_UNPACK3`/`BW_NTOHS`/`BW_NTOHL`: the length field, big-endian, in the
/// width the element declared.
fn decode_be_length(bytes: &[u8], width: usize) -> u32 {
    match width {
        1 => bytes[0] as u32,
        2 => u16::from_be_bytes([bytes[0], bytes[1]]) as u32,
        3 => ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | (bytes[2] as u32),
        4 => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_element::FIXED_LENGTH_MESSAGE;
    use crate::interfaces::InputMessageHandler;
    use crate::misc::MessageID;
    use std::cell::RefCell;
    use std::rc::Rc;

    // ------------------------------------------------------------------
    // Test doubles
    // ------------------------------------------------------------------

    /// A handler that records `( id, reply id, body bytes )` per delivery and
    /// consumes its whole body.
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

    fn variable(id: MessageID, width: i32) -> InterfaceElement {
        InterfaceElement::new("variable", id, VARIABLE_LENGTH_MESSAGE, width, None)
    }

    fn fixed(id: MessageID, size: i32) -> InterfaceElement {
        InterfaceElement::new("fixed", id, FIXED_LENGTH_MESSAGE, size, None)
    }

    fn source() -> Address {
        Address::new(0x7f000001, 2000)
    }

    fn handler_of(recorder: &Rc<Recorder>) -> Rc<dyn InputMessageHandler> {
        Rc::clone(recorder) as Rc<dyn InputMessageHandler>
    }

    /// A frame body ready for requests: the flags byte and a zeroed first
    /// request offset field, so every message offset from here is
    /// bundle-absolute (measured from the flags byte).
    fn request_frame() -> Vec<u8> {
        let mut body = vec![0u8];
        body.extend_from_slice(&[0u8; FIRST_REQUEST_OFFSET_SIZE]);
        body
    }

    // ------------------------------------------------------------------
    // Frame builder: the TCP bundle the reference's doSend frames, one
    // message at a time, big-endian, into a body the reader and walker
    // read back.
    // ------------------------------------------------------------------

    /// Prepend the frame length header. `SMALL_FRAME_MAX` selects the 32-bit
    /// form, the way `doSend` does when the bundle is large.
    fn frame_around(body: &[u8], large: bool) -> Vec<u8> {
        let mut frame = Vec::new();
        if large || body.len() >= SMALL_FRAME_MAX as usize {
            frame.extend_from_slice(&SMALL_FRAME_MAX.to_be_bytes());
            frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        } else {
            frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(body);
        frame
    }

    /// A variable message: id, big-endian length field (or the oversize form),
    /// body.
    fn fill_variable(body: &mut Vec<u8>, element: &InterfaceElement, payload: &[u8]) {
        let width = element.length_param().max(0) as usize;
        body.push(element.id());
        let field_start = body.len();
        body.resize(body.len() + width, 0);
        body.extend_from_slice(payload);

        let len = payload.len() as u32;
        if element.can_handle_length_for(len as i32) {
            body[field_start..field_start + width].copy_from_slice(&len.to_be_bytes()[4 - width..]);
        } else {
            // Oversize: all-ones field, then the 32-bit length, body undispaced.
            body[field_start..field_start + width].fill(0xff);
            let mut rest = Vec::new();
            rest.extend_from_slice(&len.to_be_bytes());
            rest.extend_from_slice(payload);
            body.splice(body.len() - payload.len().., rest);
        }
    }

    /// A fixed message: id byte and body, no length field.
    fn fill_fixed(body: &mut Vec<u8>, element: &InterfaceElement, payload: &[u8]) {
        body.push(element.id());
        body.extend_from_slice(payload);
    }

    /// A request-shaped message: id, length field (or oversize), reply id,
    /// next request link (left zero), body. Answers `( message offset,
    /// link slot )` so the chain can be patched once every offset is known.
    fn fill_request(
        body: &mut Vec<u8>,
        element: &InterfaceElement,
        reply_id: ReplyID,
        payload: &[u8],
    ) -> (usize, usize) {
        let offset = body.len();
        fill_variable(body, element, payload);
        // The request preamble sits between the length field and the body.
        let width = element.length_param().max(0) as usize;
        let preamble_at = offset + 1 + width;
        body.splice(preamble_at..preamble_at, {
            let mut pre = Vec::new();
            pre.extend_from_slice(&reply_id.to_be_bytes());
            pre.extend_from_slice(&0u32.to_be_bytes());
            pre
        });
        let link_slot = preamble_at + REPLY_ID_SIZE;
        (offset, link_slot)
    }

    /// Patch the request links: each request points at the next one, the last
    /// at "no next", and the frame header carries the first. Answers the first
    /// request's offset so the caller can mark the bundle's `FLAG_HAS_REQUESTS`.
    fn chain(links: &[(usize, usize)], body: &mut [u8]) -> usize {
        for (i, &(_, slot)) in links.iter().enumerate() {
            let next = links
                .get(i + 1)
                .map_or(u32::MAX, |&(offset, _)| offset as u32);
            body[slot..slot + FIRST_REQUEST_OFFSET_SIZE].copy_from_slice(&next.to_be_bytes());
        }
        links.first().map_or(0, |&(offset, _)| offset)
    }

    // ------------------------------------------------------------------
    // The frame reader: short reads and merged reads.
    // ------------------------------------------------------------------

    /// The acceptance fixture for a short read: a frame fed one byte at a time
    /// yields nothing until the last byte, then exactly the frame body.
    #[test]
    fn a_frame_read_in_short_reads_is_assembled() {
        let body = b"\x00\x01hello";
        let wire = frame_around(body, false);

        let mut reader = TcpFrameReader::new();
        assert!(!reader.has_frame());

        for (i, &byte) in wire.iter().enumerate() {
            reader.feed(&[byte]);
            if i + 1 < wire.len() {
                assert!(!reader.has_frame(), "no frame until the last byte");
                assert!(reader.next_frame().is_none());
            }
        }

        assert!(reader.has_frame());
        assert_eq!(reader.next_frame(), Some(&body[..]));
        assert!(!reader.has_frame());
        assert!(reader.next_frame().is_none());
    }

    /// The acceptance fixture for a merged read: two frames in one feed come
    /// out one at a time, the second following the first.
    #[test]
    fn two_frames_in_one_read_are_split() {
        let first = b"\x00\x03abc";
        let second = b"\x01\x02xy";
        let mut wire = frame_around(first, false);
        wire.extend_from_slice(&frame_around(second, false));

        let mut reader = TcpFrameReader::new();
        reader.feed(&wire);

        assert!(reader.has_frame());
        assert_eq!(reader.next_frame(), Some(&first[..]));
        assert_eq!(reader.next_frame(), Some(&second[..]));
        assert!(!reader.has_frame());
    }

    /// The 32-bit frame length form: a bundle too large for 16 bits decodes
    /// from `0xFFFF` plus a 32-bit length.
    #[test]
    fn a_large_frame_uses_the_32_bit_length_form() {
        let body = vec![0x42u8; 70_000];
        let wire = frame_around(&body, true);

        let mut reader = TcpFrameReader::new();
        reader.feed(&wire);

        assert_eq!(reader.next_frame(), Some(&body[..]));
    }

    /// A frame split across a boundary that lands inside the length header
    /// still assembles: the reader waits for the whole header, then the body.
    #[test]
    fn a_short_read_that_splits_the_length_header_assembles() {
        let body = b"\x00\x07abcdefg";
        let wire = frame_around(body, false);

        let mut reader = TcpFrameReader::new();
        // One byte of the length header, then the rest.
        reader.feed(&wire[..1]);
        assert!(reader.next_frame().is_none());
        reader.feed(&wire[1..]);
        assert_eq!(reader.next_frame(), Some(&body[..]));
    }

    // ------------------------------------------------------------------
    // The frame parse: flags, first request offset, and the message walk.
    // ------------------------------------------------------------------

    /// A frame with no requests: the flags byte alone prefixes the messages.
    #[test]
    fn a_plain_frame_dispatches_its_messages() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&known)));

        let mut body = vec![0u8]; // flags, no requests
        fill_variable(&mut body, &variable(5, 2), b"hello");

        let reason = dispatch_messages(&table, &source(), &mut body);
        assert_eq!(reason, Reason::Success);
        assert_eq!(known.notes(), vec![(5, REPLY_ID_NONE, b"hello".to_vec())]);
    }

    /// A request frame: the flags mark the first request, and the walker
    /// hands the request's reply id through.
    #[test]
    fn a_request_frame_dispatches_with_its_reply_id() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&known)));

        let mut body = request_frame();
        let links = vec![fill_request(
            &mut body,
            &variable(5, 2),
            0x12345678,
            b"body",
        )];
        let first_offset = chain(&links, &mut body);
        body[0] |= FLAG_HAS_REQUESTS;
        body[1..1 + FIRST_REQUEST_OFFSET_SIZE]
            .copy_from_slice(&(first_offset as u32).to_be_bytes());

        let reason = dispatch_messages(&table, &source(), &mut body);
        assert_eq!(reason, Reason::Success);
        assert_eq!(known.notes(), vec![(5, 0x12345678, b"body".to_vec())]);
    }

    /// Two requests chain: the first's link points at the second, the
    /// second's at "no next", and both are recognised as requests.
    #[test]
    fn chained_requests_are_each_delivered_as_requests() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&known)));

        let mut body = request_frame();
        let links = vec![
            fill_request(&mut body, &variable(5, 2), 1, b"first"),
            fill_request(&mut body, &variable(5, 2), 2, b"second"),
        ];
        let first_offset = chain(&links, &mut body);
        body[0] |= FLAG_HAS_REQUESTS;
        body[1..1 + FIRST_REQUEST_OFFSET_SIZE]
            .copy_from_slice(&(first_offset as u32).to_be_bytes());

        let reason = dispatch_messages(&table, &source(), &mut body);
        assert_eq!(reason, Reason::Success);
        assert_eq!(
            known.notes(),
            vec![(5, 1, b"first".to_vec()), (5, 2, b"second".to_vec()),]
        );
    }

    /// A fixed-length message carries no length field.
    #[test]
    fn a_fixed_message_dispatches_without_a_length_field() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        table.serve(&fixed(6, 4), Some(handler_of(&known)));

        let mut body = vec![0u8];
        fill_fixed(&mut body, &fixed(6, 4), b"data");

        let reason = dispatch_messages(&table, &source(), &mut body);
        assert_eq!(reason, Reason::Success);
        assert_eq!(known.notes(), vec![(6, REPLY_ID_NONE, b"data".to_vec())]);
    }

    /// The oversize form: a length field set to all ones, the 32-bit length
    /// following, body undispaced.
    #[test]
    fn an_oversize_variable_message_uses_the_undispaced_32_bit_length() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&known)));

        let mut body = vec![0u8];
        fill_variable(&mut body, &variable(5, 2), &[0xAB; 700]);

        let reason = dispatch_messages(&table, &source(), &mut body);
        assert_eq!(reason, Reason::Success);
        assert_eq!(known.notes(), vec![(5, REPLY_ID_NONE, vec![0xAB; 700])]);
    }

    /// An id no handler serves ends the walk with
    /// `REASON_NONEXISTENT_ENTRY`.
    #[test]
    fn an_unhandled_message_id_discards_the_rest_of_the_frame() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        let element = variable(5, 2);
        table.serve(&element, Some(handler_of(&known)));

        let mut body = vec![0u8];
        fill_variable(&mut body, &variable(9, 2), b"rogue");
        fill_variable(&mut body, &element, b"good");

        let reason = dispatch_messages(&table, &source(), &mut body);
        assert_eq!(reason, Reason::NonexistentEntry);
        assert!(known.notes().is_empty());
    }

    /// An invalid flags byte discards the whole frame.
    #[test]
    fn an_invalid_flags_byte_discards_the_frame() {
        let table = InterfaceTable::new();
        let mut body = vec![0xFFu8];

        let reason = dispatch_messages(&table, &source(), &mut body);
        assert_eq!(reason, Reason::CorruptedPacket);
    }

    /// The reader and the walker compose: a merged wire stream decodes into
    /// frames and each frame's messages reach their handlers.
    #[test]
    fn the_reader_and_walker_compose_end_to_end() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        table.serve(&variable(5, 2), Some(handler_of(&known)));

        let mut first_body = vec![0u8];
        fill_variable(&mut first_body, &variable(5, 2), b"one");
        let mut second_body = vec![0u8];
        fill_variable(&mut second_body, &variable(5, 2), b"two");

        let mut wire = frame_around(&first_body, false);
        wire.extend_from_slice(&frame_around(&second_body, false));

        let mut reader = TcpFrameReader::new();
        reader.feed(&wire);

        let mut first = reader.next_frame().unwrap().to_vec();
        assert_eq!(
            dispatch_messages(&table, &source(), &mut first),
            Reason::Success
        );
        let mut second = reader.next_frame().unwrap().to_vec();
        assert_eq!(
            dispatch_messages(&table, &source(), &mut second),
            Reason::Success
        );

        assert_eq!(
            known.notes(),
            vec![
                (5, REPLY_ID_NONE, b"one".to_vec()),
                (5, REPLY_ID_NONE, b"two".to_vec()),
            ]
        );
    }
}
