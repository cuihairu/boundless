//! The TCP side of Mercury: `Mercury::TCPBundle`'s frame encoding, the
//! frame-length framing the channel wraps a frame in before it reaches
//! the socket, and `Mercury::TCPBundleProcessor::dispatchMessages`, the
//! walk that pulls messages back out of one frame.
//!
//! Reference sources (`lib/network/`): `tcp_bundle.cpp/.hpp` (the writer),
//! `tcp_bundle_processor.cpp/.hpp` (the walk), `tcp_channel.cpp` (the
//! frame-length framing, the `handleInput` loop the [`TcpFrameAssembler`]
//! ports). The write and read sides of the length field meet in
//! [`TcpBundle`]'s encoding and [`tcp_expand_length`]'s decode, and the
//! acceptance fixtures `unit_test/test_tcp_bundle.cpp` port to the tests
//! below.
//!
//! A TCP frame is one bundle, framed by the channel as
//! `[u16 frameLength | 0xffff + u32 frameLength][frame]`; the frame itself
//! is `[flags][first request offset when FLAG_HAS_REQUESTS][messages]`.
//! A message is `[id][length field | 0xff escape + u32][payload]`, with a
//! request carrying its reply id and the next request's offset between the
//! length field and the payload.
//!
//! Deliberate departures from the reference:
//!
//! * [`TcpBundle`] takes the maximum segment size as a number instead of
//!   a live `TCPChannel &`: the channel and its MSS arithmetic arrive
//!   with BW-02-04-02. The arithmetic is the same
//!   (`numDataUnits`/`freeBytesInLastDataUnit`).
//! * [`TcpBundle`] keeps reply-order offsets rather than resolving them
//!   onto raw pointers at `doFinalise` (the reference's `ReplyIDOrOffset`
//!   hack); an offset into a `Vec<u8>` answers every use a pointer did.
//! * The escape form of a length field is written as the reference's
//!   standalone bundle writes it -- the field all ones with the real
//!   length appended after it -- because that is the wire
//!   `TCPBundleProcessor` reads (the reference's `expandLength` returns -1
//!   for a standalone frame and the processor then reads the appended
//!   `u32`). The in-place displace form is the packet-chained UDP path.
//! * The request preamble is eight bytes (a `u32` next-request offset),
//!   not the six the UDP framing uses, and the special form's body
//!   arithmetic follows it -- the reference's `specialCompressLength`
//!   advances by `sizeof( Packet::Offset )`, which is the packet's `u16`
//!   even under TCP; this port uses the width TCP actually writes.
//! * The connection-header sniffing that guards the first frame (the
//!   `GET ` WebSocket rejection and the `CLIENT_HEADER` handshake) is
//!   part of session negotiation and lands in BW-02-05-02; the assembler
//!   here frames raw TCP only.

use std::rc::Rc;

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interface_element::{
    FIXED_LENGTH_MESSAGE, InterfaceElement, REPLY_MESSAGE_IDENTIFIER, VARIABLE_LENGTH_MESSAGE,
};
use crate::interface_table::InterfaceTable;
use crate::message_framing::REPLY_ID_SIZE;
use crate::misc::{REPLY_ID_NONE, Reason, ReplyID};
use crate::reply_order::ReplyOrder;
use crate::unpacked_message_header::UnpackedMessageHeader;

/// `TCPBundle::FLAG_HAS_REQUESTS` (`tcp_bundle.hpp:28`): the frame's
/// first byte carries a four-byte first request offset when set.
pub const FLAG_HAS_REQUESTS: u8 = 0x01;
/// `TCPBundle::FLAG_MASK` (`tcp_bundle.hpp:29`): every flag bit this
/// protocol defines. Anything else in the flags byte is a lie.
pub const FLAG_MASK: u8 = 0x01;

/// The flags byte is one byte at the head of every frame.
pub const TCP_FLAGS_SIZE: usize = 1;
/// `TCPBundle::Offset` (`tcp_bundle.hpp:32`): the width of a request
/// offset inside a frame.
pub const TCP_OFFSET_SIZE: usize = 4;
/// The bytes a request message carries before its payload: the reply id
/// and the next request's offset.
pub const TCP_REQUEST_PREAMBLE_SIZE: usize = REPLY_ID_SIZE + TCP_OFFSET_SIZE;

/// `ReplyIDOrOffset`'s placeholder (`tcp_bundle.cpp:93`): the reply id a
/// request writes while it waits for the request manager to allocate the
/// real one.
const REPLY_ID_PLACEHOLDER: u32 = 0x1234_5678;
/// `TCPBundle::Offset( -1 )` (`tcp_bundle.cpp:93`): the last request's
/// next offset, the sentinel that ends the chain.
pub const END_REQUEST_OFFSET: u32 = u32::MAX;

/// The frame-length header escapes frames at or above this length: the
/// `u16` reads `0xffff` and the real length follows as a `u32`.
pub const LARGE_FRAME_ESCAPE: u16 = 0xffff;
/// The frame header when it is not escaped.
pub const SMALL_FRAME_HEADER_SIZE: usize = 2;
/// The frame header when it is escaped: the `u16` and the `u32`.
pub const LARGE_FRAME_HEADER_SIZE: usize = 6;

/// The width of a message id on the wire; `MessageID` is a byte.
const IDENTIFIER_SIZE: usize = 1;

/// `TCPBundle` (`tcp_bundle.hpp:25`): a bundle sent on a TCP channel.
///
/// The reference writes through two `MemoryOStream`s -- the frame's header
/// region and the message under construction -- and splices each message
/// into the frame when it is finished. The port keeps the same two
/// regions as two `Vec<u8>`s: `frame_data` holds the flags byte, the
/// optional first request offset and every finished message, and
/// `message_data` holds the message being built.
pub struct TcpBundle {
    /// `tcpChannel().maxSegmentSize()`: the MSS the segment arithmetic
    /// divides by, held as a number until the channel exists.
    max_segment_size: usize,
    /// `frameStartOffset_`: the bytes of `frame_data` hidden in front of
    /// the wire view. Four until the first request moves them into play.
    frame_start_offset: usize,
    /// `pFrameData_`: the frame's header region and every finished
    /// message.
    frame_data: Vec<u8>,
    /// `pMessageData_`: the message under construction.
    message_data: Vec<u8>,
    /// `pCurrentMessage_`: the element whose message is being written.
    current_message: Option<InterfaceElement>,
    /// `isCurrentMessageRequest_`: whether the message under construction
    /// carries a reply id and a next offset.
    is_current_message_request: bool,
    /// `numMessages_`: how many messages the bundle has started.
    num_messages: u32,
    /// `replyOrders_`: the requests a reply will answer, in the order
    /// they were started.
    reply_orders: Vec<ReplyOrder>,
}

impl TcpBundle {
    /// `TCPBundle( channel )` (`tcp_bundle.cpp:26-41`): reserve the flags
    /// byte and the four-byte first request offset, and start with no
    /// flags set.
    pub fn new(max_segment_size: usize) -> Self {
        TcpBundle {
            max_segment_size,
            frame_start_offset: TCP_OFFSET_SIZE,
            // The reserved four bytes are a placeholder; `data()` starts
            // past them until a request pulls them into the wire.
            frame_data: vec![0; TCP_FLAGS_SIZE + TCP_OFFSET_SIZE],
            message_data: Vec::new(),
            current_message: None,
            is_current_message_request: false,
            num_messages: 0,
            reply_orders: Vec::new(),
        }
    }

    /// `startMessage( ie )` (`tcp_bundle.cpp:61-65`): begin a message
    /// with this element's id. Starting a message finishes the previous
    /// one.
    pub fn start_message(&mut self, ie: &InterfaceElement) {
        if self.current_message.is_some() {
            self.finalise_current_message();
        }
        self.num_messages += 1;
        self.current_message = Some(ie.clone());
        self.message_data.clear();
        self.message_data.push(ie.id());
        if ie.length_style() == VARIABLE_LENGTH_MESSAGE {
            // The reserved length field, filled when the message ends.
            self.message_data
                .extend(std::iter::repeat_n(0, ie.length_param().max(0) as usize));
        }
    }

    /// `startRequest( ie, pHandler, arg, timeout )` (`tcp_bundle.cpp:71-115`):
    /// begin a request message: pull the frame's header down into the
    /// wire, link this request onto the chain, and leave a reply id
    /// placeholder behind.
    pub fn start_request(
        &mut self,
        ie: &InterfaceElement,
        handler: Rc<dyn crate::interfaces::ReplyMessageHandler>,
        arg: usize,
        microseconds: i32,
    ) {
        // `if (frameStartOffset_ > 0)`: the first request brings the
        // reserved offset header onto the wire and flags the frame.
        if self.frame_start_offset > 0 {
            self.frame_start_offset = 0;
            let flags = self.flags();
            self.set_flags(flags | FLAG_HAS_REQUESTS);
        }

        // The offset this request's message starts at, before it starts.
        let offset = self.size() as u32;

        self.start_message(ie);

        self.set_next_request_offset(offset);

        let reply_id_offset = self.size();
        self.message_data
            .extend_from_slice(&REPLY_ID_PLACEHOLDER.to_le_bytes());
        self.message_data
            .extend_from_slice(&END_REQUEST_OFFSET.to_le_bytes());

        self.reply_orders
            .push(ReplyOrder::new(handler, arg, microseconds, reply_id_offset));
        self.is_current_message_request = true;
    }

    /// `setNextRequestOffset( offset )` (`tcp_bundle.cpp:123-152`): write
    /// where this request sits into the slot the previous one left open.
    ///
    /// The first request writes into the header's own four bytes; each
    /// later one writes over the previous request's next-offset
    /// placeholder, which sits four bytes after its reply id.
    fn set_next_request_offset(&mut self, offset: u32) {
        let slot = match self.reply_orders.last() {
            None => TCP_FLAGS_SIZE,
            Some(previous) => previous.reply_id_offset() + REPLY_ID_SIZE,
        };
        self.frame_data[slot..slot + TCP_OFFSET_SIZE].copy_from_slice(&offset.to_le_bytes());
    }

    /// `startReply( id )` (`tcp_bundle.cpp:158-163`): begin the reply
    /// message, whose reply id is its payload.
    pub fn start_reply(&mut self, id: ReplyID) {
        self.start_message(&InterfaceElement::reply());
        self.message_data.extend_from_slice(&id.to_le_bytes());
    }

    /// `writeBytes`/`reserve`: the current message's payload grows here.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.message_data.extend_from_slice(bytes);
    }

    /// `doFinalise` (`tcp_bundle.cpp:281-302`): finish the message under
    /// construction, if any.
    pub fn finalise(&mut self) {
        if self.current_message.is_some() {
            self.finalise_current_message();
        }
    }

    /// `clear( newBundle )` (`tcp_bundle.cpp:169-185`): back to the fresh
    /// state: the reserved header hidden again, no messages, no flags.
    pub fn clear(&mut self) {
        self.frame_start_offset = TCP_OFFSET_SIZE;
        self.frame_data = vec![0; TCP_FLAGS_SIZE + TCP_OFFSET_SIZE];
        self.message_data.clear();
        self.current_message = None;
        self.is_current_message_request = false;
        self.num_messages = 0;
        self.reply_orders.clear();
    }

    /// `freeBytesInLastDataUnit` (`tcp_bundle.cpp:191-196`).
    pub fn free_bytes_in_last_data_unit(&self) -> usize {
        self.max_segment_size - (self.size() % self.max_segment_size)
    }

    /// `numDataUnits` (`tcp_bundle.cpp:201-207`): how many segments the
    /// bundle spans.
    pub fn num_data_units(&self) -> usize {
        self.size().div_ceil(self.max_segment_size)
    }

    /// `size` (`tcp_bundle.cpp:220-225`): the frame's bytes on the wire,
    /// the reserved offset header not counting until a request makes it
    /// visible.
    pub fn size(&self) -> usize {
        self.frame_data.len() + self.message_data.len() - self.frame_start_offset
    }

    /// `data` (`tcp_bundle.cpp:339-342`): the frame as it will leave the
    /// channel.
    pub fn data(&self) -> &[u8] {
        &self.frame_data[self.frame_start_offset..]
    }

    /// `numMessages`.
    pub fn num_messages(&self) -> u32 {
        self.num_messages
    }

    /// `Bundle::replyOrders`: the requests a reply will answer, in start
    /// order. The reference hands these to the request manager so the
    /// reply ids can be allocated and written in.
    pub fn reply_orders(&self) -> &[ReplyOrder] {
        &self.reply_orders
    }

    /// The frame as it goes on the wire: the frame-length header followed
    /// by the frame itself. `TCPChannel::doSend` (`tcp_channel.cpp:429-438`)
    /// escapes at `UINT16_MAX` and above, which the fixture calls
    /// `bundle.size() >= 0xffff`.
    pub fn frame_encode(&self) -> Vec<u8> {
        let frame = self.data();
        let mut wire = Vec::with_capacity(frame.len() + LARGE_FRAME_HEADER_SIZE);
        if frame.len() >= LARGE_FRAME_ESCAPE as usize {
            wire.extend_from_slice(&LARGE_FRAME_ESCAPE.to_le_bytes());
            wire.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        } else {
            wire.extend_from_slice(&(frame.len() as u16).to_le_bytes());
        }
        wire.extend_from_slice(frame);
        wire
    }

    /// `flags` (`tcp_bundle.cpp:348-351`): the frame's first byte.
    fn flags(&self) -> u8 {
        self.frame_data[0]
    }

    /// Write the flags byte.
    fn set_flags(&mut self, flags: u8) {
        self.frame_data[0] = flags;
    }

    /// `currentMessagePayloadLength` (`tcp_bundle.cpp:308-333`): the body
    /// the message owes -- everything the message has written that is not
    /// its id, its length field or, for a request, its reply preamble.
    fn current_message_payload_length(&self, ie: &InterfaceElement, is_request: bool) -> usize {
        let length_field = if ie.length_style() == VARIABLE_LENGTH_MESSAGE {
            ie.length_param().max(0) as usize
        } else {
            0
        };
        let preamble = usize::from(is_request) * TCP_REQUEST_PREAMBLE_SIZE;
        self.message_data
            .len()
            .saturating_sub(IDENTIFIER_SIZE + length_field + preamble)
    }

    /// `finaliseCurrentMessage` (`tcp_bundle.cpp:255-275`): write the
    /// message's length into its field and splice the message into the
    /// frame.
    fn finalise_current_message(&mut self) {
        let Some(ie) = self.current_message.take() else {
            return;
        };
        let is_request = self.is_current_message_request;
        self.is_current_message_request = false;

        let length = self.current_message_payload_length(&ie, is_request) as i32;

        // `int payloadStart = sizeof( MessageID ) + ie.lengthParam()`
        // (`tcp_bundle.cpp:262`).
        let payload_start = IDENTIFIER_SIZE + ie.length_param().max(0) as usize;
        let escaped = self.write_length_field(&ie, length);

        if escaped {
            // The reference splices up to `payloadStart` -- the id and
            // the length field filled with the escape marker -- then the
            // real length, then the rest.
            self.frame_data
                .extend_from_slice(&self.message_data[..payload_start]);
            self.frame_data
                .extend_from_slice(&(length as u32).to_le_bytes());
            self.frame_data
                .extend_from_slice(&self.message_data[payload_start..]);
        } else {
            // The reference's `addBlob( pMessageData_->data(), size() )`:
            // the whole message -- id, length field, payload.
            self.frame_data.extend_from_slice(&self.message_data);
        }
        self.message_data.clear();
    }

    /// `compressLength( pMessageData_->data(), ... )`
    /// (`tcp_bundle.cpp:258-260`): fill the reserved length field, and
    /// report that the length escaped to its appended form.
    ///
    /// The reference's `compressLength` fills the field with the escape
    /// marker and returns -1 when the length cannot fit, because a
    /// standalone message has no packet chain to carry the in-place
    /// displace. This port writes the field the same way and answers the
    /// same `-1`, and [`finalise_current_message`] turns it into the
    /// appended `u32` the processor reads.
    fn write_length_field(&mut self, ie: &InterfaceElement, length: i32) -> bool {
        if ie.length_style() != VARIABLE_LENGTH_MESSAGE {
            // Fixed messages write nothing.
            return false;
        }
        if length < 0 {
            return true;
        }
        let width = ie.length_param().max(0) as usize;
        if matches!(width, 1..=3) && !ie.can_handle_length_for(length) {
            // Fill the original length field with ones to mark the special
            // situation (`tcp_bundle.cpp` / `interface_element.cpp:384-391`).
            for byte in &mut self.message_data[IDENTIFIER_SIZE..=width] {
                *byte = 0xff;
            }
            return true;
        }
        let bytes = &length.to_le_bytes()[..width];
        self.message_data[IDENTIFIER_SIZE..IDENTIFIER_SIZE + width].copy_from_slice(bytes);
        false
    }
}

/// `TCPBundleProcessor::dispatchMessages` (`tcp_bundle_processor.cpp:44-220`):
/// walk the messages of one frame and hand each to the handler its id is
/// served with.
///
/// `frame` is the frame alone (the flags byte first), the way
/// `TCPChannel::processReceivedFrame` (`tcp_channel.cpp:360-378`) hands
/// the one frame it has just pulled off the socket to a processor. The
/// walk stops at the first message the wire disagrees with, reports the
/// reason the reference reports, and leaves the rest of the frame
/// undelivered.
///
/// The reference carries the `Channel`, its network interface and its
/// message filter along; here the handler receives the source address and
/// the table carries the interface, which is what the delivery path needs
/// once the channel exists (BW-02-04-02).
pub fn dispatch_messages(table: &InterfaceTable, source: &Address, frame: &[u8]) -> Reason {
    let bundle_length = frame.len();

    // `data_ >> flags`
    let Some(&flags) = frame.first() else {
        return Reason::CorruptedPacket;
    };

    // `if (flags & ~FLAG_MASK)`
    if flags & !FLAG_MASK != 0 {
        eprintln!(
            "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after invalid flags: {:02x}",
            source.c_str(),
            flags
        );
        return Reason::CorruptedPacket;
    }

    let mut pos = TCP_FLAGS_SIZE;
    let mut next_request_offset = 0u32;

    // `if (flags & FLAG_HAS_REQUESTS) data_ >> nextRequestOffset`
    if flags & FLAG_HAS_REQUESTS != 0 {
        if frame.len() < TCP_FLAGS_SIZE + TCP_OFFSET_SIZE {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after a truncated first request offset",
                source.c_str()
            );
            return Reason::CorruptedPacket;
        }
        next_request_offset = u32::from_le_bytes(
            frame[TCP_FLAGS_SIZE..TCP_FLAGS_SIZE + TCP_OFFSET_SIZE]
                .try_into()
                .unwrap(),
        );

        // The reference compares `static_cast<int>( nextRequestOffset ) >
        // bundleLength`, so the end-of-chain sentinel `0xffffffff` casts
        // to `-1` and passes; only a real positive offset can trip this.
        if next_request_offset as i32 > bundle_length as i32 {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after out-of-bounds first request offset (got offset {} > {} bundle length)",
                source.c_str(),
                next_request_offset,
                bundle_length
            );
            return Reason::CorruptedPacket;
        }
        pos = TCP_FLAGS_SIZE + TCP_OFFSET_SIZE;
    }

    while pos < bundle_length {
        let offset = pos;

        // `data_ >> messageID`
        let msg_id = frame[offset];

        let served = table.element(msg_id);

        // `if (ie.pHandler() == NULL)`
        let Some(handler) = served.handler() else {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after hitting unhandled message ID {}",
                source.c_str(),
                msg_id
            );
            return Reason::NonexistentEntry;
        };

        // `updatedIE.updateLengthDetails(...)` folds a callback's length
        // into a copy, so the served entry keeps its style for the next
        // frame.
        let mut updated = served.clone();
        if !updated.update_length_details(handler.as_ref()) {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after failure to update length details for message ID {}",
                source.c_str(),
                msg_id
            );
            return Reason::CorruptedPacket;
        }

        // `bool isRequest = (offset == nextRequestOffset)`
        let is_request = offset == next_request_offset as usize;

        let (length, header_end) = match tcp_expand_length(&updated, &frame[offset..]) {
            Ok(expanded) => expanded,
            Err(()) => {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Discarding bundle after a bad length for message ID {}",
                    source.c_str(),
                    msg_id
                );
                return Reason::CorruptedPacket;
            }
        };

        let mut reply_id = REPLY_ID_NONE;
        // The message's header ends at `offset + header_end`; a request's
        // eight-byte reply preamble sits between that and the payload, the
        // way the reference reads it off the stream before `retrieve(
        // header.length )`.
        let preamble_start = offset + header_end;
        let payload_start = preamble_start
            + if is_request {
                TCP_REQUEST_PREAMBLE_SIZE
            } else {
                0
            };
        if is_request {
            // `data_ >> header.replyID; data_ >> nextRequestOffset`
            if bundle_length < preamble_start + TCP_REQUEST_PREAMBLE_SIZE {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Discarding rest of bundle since chain too short for the request preamble of message ID {}",
                    source.c_str(),
                    msg_id
                );
                return Reason::CorruptedPacket;
            }
            reply_id = i32::from_le_bytes(
                frame[preamble_start..preamble_start + REPLY_ID_SIZE]
                    .try_into()
                    .unwrap(),
            );
            next_request_offset = u32::from_le_bytes(
                frame[preamble_start + REPLY_ID_SIZE..preamble_start + TCP_REQUEST_PREAMBLE_SIZE]
                    .try_into()
                    .unwrap(),
            );
            // The sentinel keeps passing here too: a request's own next
            // offset is `-1` when it is the last of the chain.
            if next_request_offset as i32 > bundle_length as i32 {
                eprintln!(
                    "TCPBundleProcessor::dispatchMessages( {} ): Discarding rest of bundle due to out-of-bounds next request offset (got offset {} > {} byte bundle length)",
                    source.c_str(),
                    next_request_offset,
                    bundle_length
                );
                return Reason::CorruptedPacket;
            }
        }

        // `if (data_.remainingLength() < header.length)`: the payload is
        // what the length declares, the preamble included in the bytes
        // that were just read.
        if bundle_length < payload_start + length.max(0) as usize {
            eprintln!(
                "TCPBundleProcessor::dispatchMessages( {} ): Discarding rest of bundle since chain too short for data of message ID {} length {} (have {} bytes)",
                source.c_str(),
                msg_id,
                length,
                bundle_length.saturating_sub(payload_start)
            );
            return Reason::CorruptedPacket;
        }

        // `header.pInterfaceElement = &ie`
        let mut header = UnpackedMessageHeader::new(msg_id);
        header.set_length(length);
        header.set_reply_id(reply_id);
        header.set_interface_element(served.clone());

        // `MemoryIStream mis( data_.retrieve( header.length ), ... )`
        let mut stream = BinaryStream::new();
        stream.write_bytes(&frame[payload_start..payload_start + length.max(0) as usize]);

        handler.handle_message(source, &mut header, &mut stream);

        table.record_received(msg_id, length.max(0) as u32);

        // A handler that did not eat its body is reported; a reply's
        // leftovers are not consumed.
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

        pos = payload_start + length.max(0) as usize;
    }

    Reason::Success
}

/// `expandLength( pHeaderData, NULL, isRequest )`
/// (`tcp_bundle_processor.cpp:128`): read the length a message declares,
/// undoing the appended-`u32` escape when it was used.
///
/// `data` starts at the message's id byte, the way the reference hands
/// `pHeaderData` to `expandLength`. Returns the length and how many bytes
/// the message's header occupies from that id byte -- the id, the length
/// field and the appended `u32` of an escaped length. The request
/// preamble the reference's `isRequest` argument carries is not part of
/// this count: the processor reads it after the field, so
/// [`dispatch_messages`] adds it on.
///
/// The reference's `expandLength` returns -1 for the special form when
/// it has no packet to walk, and the processor then reads the appended
/// `u32` (`tcp_bundle_processor.cpp:134-140`); this is that decode.
fn tcp_expand_length(ie: &InterfaceElement, data: &[u8]) -> Result<(i32, usize), ()> {
    match ie.length_style() {
        FIXED_LENGTH_MESSAGE => Ok((ie.length_param(), IDENTIFIER_SIZE)),
        VARIABLE_LENGTH_MESSAGE => {
            let width = ie.length_param().max(0) as usize;
            if IDENTIFIER_SIZE + width > data.len() {
                return Err(());
            }
            let len: u32 = match width {
                1 => u8::from_le_bytes([data[IDENTIFIER_SIZE]]).into(),
                2 => u16::from_le_bytes([data[IDENTIFIER_SIZE], data[IDENTIFIER_SIZE + 1]]).into(),
                3 => u32::from_le_bytes([
                    data[IDENTIFIER_SIZE],
                    data[IDENTIFIER_SIZE + 1],
                    data[IDENTIFIER_SIZE + 2],
                    0,
                ]),
                4 => u32::from_le_bytes(
                    data[IDENTIFIER_SIZE..IDENTIFIER_SIZE + 4]
                        .try_into()
                        .unwrap(),
                ),
                // The reference's default arm leaves the length zero.
                _ => 0,
            };

            // A field of all ones is the escape marker, the value that
            // does not fit; the real length follows as a `u32`.
            let escaped = matches!(width, 1..=3)
                && data[IDENTIFIER_SIZE..IDENTIFIER_SIZE + width]
                    .iter()
                    .all(|byte| *byte == 0xff);
            if escaped {
                if IDENTIFIER_SIZE + width + 4 > data.len() {
                    return Err(());
                }
                let appended = i32::from_le_bytes(
                    data[IDENTIFIER_SIZE + width..IDENTIFIER_SIZE + width + 4]
                        .try_into()
                        .unwrap(),
                );
                return Ok((appended, IDENTIFIER_SIZE + width + 4));
            }

            Ok((len as i32, IDENTIFIER_SIZE + width))
        }
        _ => Err(()),
    }
}

/// `TCPChannel::handleInput`'s frame-length framing
/// (`tcp_channel.cpp:184-264`), minus the connection-header sniffing the
/// session item owns.
///
/// The channel pulls whatever bytes the socket hands it and feeds them
/// here; this keeps the frame header and the frame body until a frame is
/// whole. A frame arrives short -- a length split across reads, a body
/// that follows its header later -- and the assembler simply holds the
/// bytes until the next push completes it. Several frames arriving at
/// once are each returned in order, the bytes after the last one staying
/// for the next.
pub struct TcpFrameAssembler {
    /// `pFrameData_`: the bytes that have arrived, header and body.
    pending: Vec<u8>,
    /// `frameLength_`: the body length the header declared, zero until a
    /// header has been read.
    frame_length: u32,
    /// `frameHeaderLength_`: the header bytes this frame has read, zero
    /// until a header has been read.
    frame_header_length: usize,
    /// `numDataUnitsReceived_`
    num_data_units_received: u64,
    /// `numBytesReceived_`
    num_bytes_received: u64,
}

impl TcpFrameAssembler {
    /// The empty assembler.
    pub fn new() -> Self {
        TcpFrameAssembler {
            pending: Vec::new(),
            frame_length: 0,
            frame_header_length: 0,
            num_data_units_received: 0,
            num_bytes_received: 0,
        }
    }

    /// Feed the bytes a socket read returned, and take back every frame
    /// that became complete, in order. The bytes after the last frame
    /// stay for the next push.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);

        let mut frames = Vec::new();
        loop {
            // `if ((frameLength_ == 0) && (frameHeaderLength_ == 0))`
            if self.frame_length == 0 && self.frame_header_length == 0 {
                self.read_small_frame_length();
            }
            // `if ((frameLength_ == 0) && (frameHeaderLength_ != 0))`
            if self.frame_length == 0 && self.frame_header_length != 0 {
                self.read_large_frame_length();
            }

            // `if ((frameLength_ != 0) && (remaining >= frameLength))`
            if self.frame_length != 0 && self.pending.len() >= self.frame_length as usize {
                let end = self.frame_length as usize;
                frames.push(self.pending.drain(..end).collect());
                self.clean_up_after_frame_receive();
            } else {
                break;
            }
        }

        frames
    }

    /// `numDataUnitsReceived_`
    pub fn num_data_units_received(&self) -> u64 {
        self.num_data_units_received
    }

    /// `numBytesReceived_`
    pub fn num_bytes_received(&self) -> u64 {
        self.num_bytes_received
    }

    /// `readSmallFrameLength` (`tcp_channel.cpp:321-339`): the two-byte
    /// frame length, or the escape that means it is bigger.
    fn read_small_frame_length(&mut self) {
        if self.pending.len() >= SMALL_FRAME_HEADER_SIZE {
            let small = u16::from_le_bytes(self.pending[..2].try_into().unwrap());
            self.pending.drain(..SMALL_FRAME_HEADER_SIZE);
            if small != LARGE_FRAME_ESCAPE {
                self.frame_length = small.into();
                self.frame_header_length = SMALL_FRAME_HEADER_SIZE;
            } else {
                // The large length follows; the header is not done.
                self.frame_header_length = LARGE_FRAME_HEADER_SIZE;
            }
        }
    }

    /// `readLargeFrameLength` (`tcp_channel.cpp:345-354`).
    fn read_large_frame_length(&mut self) {
        if self.pending.len() >= TCP_OFFSET_SIZE {
            self.frame_length =
                u32::from_le_bytes(self.pending[..TCP_OFFSET_SIZE].try_into().unwrap());
            self.pending.drain(..TCP_OFFSET_SIZE);
        }
    }

    /// `cleanUpAfterFrameReceive` (`tcp_channel.cpp:384-399`): record the
    /// frame that was just processed and start the next one from what is
    /// left.
    fn clean_up_after_frame_receive(&mut self) {
        self.num_data_units_received += 1;
        self.num_bytes_received += (self.frame_header_length + self.frame_length as usize) as u64;

        self.frame_length = 0;
        self.frame_header_length = 0;
    }
}

impl Default for TcpFrameAssembler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use foundation::stream::BinaryStream;

    use super::*;
    use crate::interface_element::CALLBACK_LENGTH_MESSAGE;
    use crate::interfaces::{InputMessageHandler, ReplyMessageHandler};
    use crate::misc::{MessageID, REPLY_ID_NONE, ReplyID};
    use crate::nub_exception::NubException;
    use crate::unpacked_message_header::UnpackedMessageHeader;

    /// The segment size the fixtures divide by; the reference reads it
    /// live off the accepted socket, the port takes the number directly.
    const MAX_SEGMENT: usize = 1460;

    /// The fixtures' `0xdeadbeef`, spelled by its little-endian bytes
    /// because it does not fit a `ReplyID` (`i32`) as a literal.
    const REPLY_ID: ReplyID = i32::from_le_bytes([0xef, 0xbe, 0xad, 0xde]);

    // The fixture's test elements (`test_tcp_bundle.cpp:152-187`): one
    // fixed-length, three variable widths, one request.
    const FIXED_MSG_ID: u8 = 0x11;
    const VAR1_MSG_ID: u8 = 0x22;
    const VAR2_MSG_ID: u8 = 0x33;
    const VAR3_MSG_ID: u8 = 0x55;
    const REQ_MSG_ID: u8 = 0x44;

    fn fixed(id: u8) -> InterfaceElement {
        InterfaceElement::new("TestFixed", id, FIXED_LENGTH_MESSAGE, 4, None)
    }

    fn var(id: u8, width: i32) -> InterfaceElement {
        InterfaceElement::new("TestVar", id, VARIABLE_LENGTH_MESSAGE, width, None)
    }

    fn request_element() -> InterfaceElement {
        var(REQ_MSG_ID, 1)
    }

    fn source() -> Address {
        Address::new(0x7f000001, 2000)
    }

    fn read_le32(bytes: &[u8]) -> u32 {
        u32::from_le_bytes(bytes.try_into().unwrap())
    }

    /// A delivery recorder: every message a dispatch hands it, its reply
    /// id and its body, in order.
    struct Recorder {
        notes: RefCell<Vec<(MessageID, ReplyID, Vec<u8>)>>,
    }

    impl Default for Recorder {
        fn default() -> Self {
            Recorder {
                notes: RefCell::new(Vec::new()),
            }
        }
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

    /// A delivery recorder that eats only its first `keep` body bytes, for
    /// the leftover-bytes warnings.
    struct PartialRecorder {
        keep: usize,
        notes: RefCell<Vec<(MessageID, ReplyID, Vec<u8>)>>,
    }

    impl InputMessageHandler for PartialRecorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            let mut body = Vec::with_capacity(self.keep);
            while body.len() < self.keep {
                match data.read_u8() {
                    Ok(byte) => body.push(byte),
                    Err(_) => break,
                }
            }
            self.notes
                .borrow_mut()
                .push((header.identifier(), header.reply_id(), body));
        }
    }

    /// A reply handler stub: the request fixtures never deliver a reply.
    struct StubReplyHandler;

    impl ReplyMessageHandler for StubReplyHandler {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &UnpackedMessageHeader,
            _data: &mut BinaryStream,
            _arg: usize,
        ) {
        }

        fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
    }

    fn reply_handler() -> Rc<dyn ReplyMessageHandler> {
        Rc::new(StubReplyHandler)
    }

    fn serve(table: &InterfaceTable, element: &InterfaceElement, recorder: &Rc<Recorder>) {
        table.serve(
            element,
            Some(Rc::clone(recorder) as Rc<dyn InputMessageHandler>),
        );
    }

    // ------------------------------------------------------------------
    // Frame layout (test_tcp_bundle.cpp)
    // ------------------------------------------------------------------

    /// `TCPBundle_freshState`: one byte on the wire, the flags byte at
    /// zero, and the reserved four-byte offset header hidden.
    #[test]
    fn a_fresh_bundle_is_one_flags_byte() {
        let bundle = TcpBundle::new(MAX_SEGMENT);
        assert_eq!(bundle.size(), 1);
        assert_eq!(bundle.num_messages(), 0);
        assert_eq!(bundle.data()[0], 0, "the flags byte starts at zero");
        assert!(!bundle.data().is_empty());
    }

    /// `TCPBundle_fixedLengthMessageFrame`: a fixed message gains no
    /// length field; the frame is `[flags][id][payload]`.
    #[test]
    fn a_fixed_length_message_frame_has_no_length_field() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&fixed(FIXED_MSG_ID));
        bundle.write_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[0], 0, "the flags byte leads every frame");
        assert_eq!(wire[1], FIXED_MSG_ID);
        assert_eq!(&wire[2..6], &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(bundle.size(), 6, "flags + id + payload");
        assert_eq!(bundle.num_messages(), 1);
    }

    /// `TCPBundle_variableLengthWidth1`: the length is the byte after the
    /// id.
    #[test]
    fn a_width_one_length_sits_after_the_id() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(&[0xa1, 0xb2, 0xc3]);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[1], VAR1_MSG_ID);
        assert_eq!(wire[2], 3, "the length is the byte after the id");
        assert_eq!(&wire[3..6], &[0xa1, 0xb2, 0xc3]);
        assert_eq!(bundle.size(), 6);
    }

    /// `TCPBundle_variableLengthWidth1Boundary`: 254 is the largest
    /// inline width-1 length; the field reads 0xff for anything past it.
    #[test]
    fn a_width_one_length_runs_to_254_before_it_escapes() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(&[0x5a; 254]);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[2], 254);
        assert_eq!(bundle.size(), 257, "flags + id + length + 254");
    }

    /// `TCPBundle_variableLengthWidth2WireOrder`: a width-2 length is
    /// little-endian on the wire.
    #[test]
    fn a_width_two_length_is_little_endian() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR2_MSG_ID, 2));
        bundle.write_bytes(&[0x77; 258]);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[1], VAR2_MSG_ID);
        assert_eq!(wire[2], 0x02, "258 = 0x0102, low byte first");
        assert_eq!(wire[3], 0x01);
        assert_eq!(&wire[4..], &[0x77; 258][..]);
        assert_eq!(bundle.size(), 262, "flags + id + 2 + 258");
    }

    /// `TCPBundle_oversizeEscape`: a payload that does not fit the inline
    /// width escapes -- the field fills with 0xff and the real length
    /// follows as a `u32`.
    #[test]
    fn an_oversized_length_escapes_after_the_field() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(&[0x99; 300]);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[1], VAR1_MSG_ID);
        assert_eq!(wire[2], 0xff, "the escape marker");
        assert_eq!(read_le32(&wire[3..7]), 300, "the real length follows");
        assert_eq!(&wire[7..], &[0x99; 300][..]);
        assert_eq!(bundle.size(), 307);
    }

    /// `TCPBundle_variableLengthWidth3Pack3`: a width-3 length is three
    /// little-endian bytes, the one style that never escapes.
    #[test]
    fn a_width_three_length_is_packed_in_three_bytes() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR3_MSG_ID, 3));
        bundle.write_bytes(&[0x66; 300]);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[1], VAR3_MSG_ID);
        assert_eq!(wire[2], 0x2c, "300 = 0x012C, three bytes");
        assert_eq!(wire[3], 0x01);
        assert_eq!(wire[4], 0x00);
        assert_eq!(&wire[5..], &[0x66; 300][..]);
        assert_eq!(bundle.size(), 305);
    }

    /// `TCPBundle_startReplyFrame`: the reply message is the reply element
    /// with the reply id as its payload, no request flag.
    #[test]
    fn a_reply_frame_carries_the_reply_id_as_its_payload() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_reply(REPLY_ID);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[0] & FLAG_HAS_REQUESTS, 0, "a reply is not a request");
        assert_eq!(wire[1], REPLY_MESSAGE_IDENTIFIER);
        assert_eq!(read_le32(&wire[2..6]), 4);
        assert_eq!(read_le32(&wire[6..10]), 0xdead_beef);
        assert_eq!(bundle.size(), 10);
    }

    /// `TCPBundle_firstRequestFrame`: the first request pulls the reserved
    /// offset header down into the wire and flags the frame; its length
    /// excludes the eight-byte request preamble.
    #[test]
    fn a_first_request_frame_pulls_the_reserved_header_down() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_request(&request_element(), reply_handler(), 0, 0);
        bundle.write_bytes(&[0x0f, 0xf0]);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[0], FLAG_HAS_REQUESTS);
        assert_eq!(
            read_le32(&wire[1..5]),
            5,
            "the first request starts after the header"
        );
        assert_eq!(wire[5], REQ_MSG_ID);
        assert_eq!(wire[6], 2, "the length counts only the payload");
        assert_eq!(
            read_le32(&wire[7..11]),
            0x1234_5678,
            "the reply id placeholder"
        );
        assert_eq!(
            read_le32(&wire[11..15]),
            END_REQUEST_OFFSET,
            "the last request keeps the sentinel"
        );
        assert_eq!(&wire[15..17], &[0x0f, 0xf0]);
        assert_eq!(bundle.size(), 17);
        assert_eq!(bundle.num_messages(), 1);
        assert_eq!(bundle.reply_orders().len(), 1);
        assert_eq!(bundle.reply_orders()[0].reply_id_offset(), 7);
    }

    /// `TCPBundle_requestChain`: the second request's offset is written
    /// over the first's next-offset placeholder, chaining them; the last
    /// keeps the sentinel.
    #[test]
    fn a_request_chain_links_the_second_offset_over_the_first() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_request(&request_element(), reply_handler(), 0, 0);
        bundle.write_bytes(&[0x01, 0x02]);
        bundle.start_request(&request_element(), reply_handler(), 0, 0);
        bundle.write_bytes(&[0x03, 0x04, 0x05]);
        bundle.finalise();

        let wire = bundle.data();
        assert_eq!(wire[0], FLAG_HAS_REQUESTS);
        assert_eq!(read_le32(&wire[1..5]), 5);
        assert_eq!(read_le32(&wire[11..15]), 17, "the second request's offset");
        assert_eq!(
            read_le32(&wire[23..27]),
            END_REQUEST_OFFSET,
            "the second request keeps the sentinel"
        );
        assert_eq!(wire[18], 3, "the second length counts only its payload");
        assert_eq!(read_le32(&wire[7..11]), 0x1234_5678);
        assert_eq!(read_le32(&wire[19..23]), 0x1234_5678);

        assert_eq!(bundle.reply_orders().len(), 2);
        assert_eq!(bundle.reply_orders()[0].reply_id_offset(), 7);
        assert_eq!(bundle.reply_orders()[1].reply_id_offset(), 19);
    }

    /// `TCPBundle_clearResets`: `clear` restores the fresh state and the
    /// bundle builds fresh frames.
    #[test]
    fn clear_restores_the_fresh_state() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(&[1, 2, 3]);
        bundle.finalise();
        assert!(bundle.size() > 1);

        bundle.clear();

        assert_eq!(bundle.size(), 1);
        assert_eq!(bundle.num_messages(), 0);
        assert_eq!(bundle.data()[0], 0);

        // And it is reusable.
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(&[1, 2, 3]);
        bundle.finalise();
        let wire = bundle.data();
        assert_eq!(wire[1], VAR1_MSG_ID);
        assert_eq!(wire[2], 3);
        assert_eq!(bundle.size(), 6);
    }

    /// `TCPBundle_numDataUnitsArithmetic`: `ceil(size / mss)` segments and
    /// `mss - (size % mss)` free bytes in the last, with the payload
    /// counts chosen to land exactly on and one past the boundary.
    #[test]
    fn num_data_units_round_up_and_the_last_segment_keeps_the_remainder() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);

        assert_eq!(bundle.size(), 1);
        assert_eq!(bundle.num_data_units(), 1);
        assert_eq!(bundle.free_bytes_in_last_data_unit(), MAX_SEGMENT - 1);

        // Exactly one full segment: size = 4 + payload = mss.
        bundle.start_message(&var(VAR2_MSG_ID, 2));
        bundle.write_bytes(&[0x42; MAX_SEGMENT - 4]);
        assert_eq!(bundle.size(), MAX_SEGMENT);
        assert_eq!(bundle.num_data_units(), 1);
        assert_eq!(bundle.free_bytes_in_last_data_unit(), MAX_SEGMENT);

        // One byte over rolls onto a second unit with one byte free.
        bundle.write_bytes(&[0x43]);
        assert_eq!(bundle.size(), MAX_SEGMENT + 1);
        assert_eq!(bundle.num_data_units(), 2);
        assert_eq!(bundle.free_bytes_in_last_data_unit(), MAX_SEGMENT - 1);
    }

    // ------------------------------------------------------------------
    // The frame-length header (`TCPChannel::doSend`)
    // ------------------------------------------------------------------

    /// `TCPChannel::doSend` (`tcp_channel.cpp:429-438`): a frame leaves
    /// as `[u16 length][frame]`, escaping at `UINT16_MAX`.
    #[test]
    fn frame_encode_writes_the_length_header() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(&[1, 2, 3]);
        bundle.finalise();

        let wire = bundle.frame_encode();
        assert_eq!(&wire[..2], &[6, 0], "six-byte frame, little-endian");
        assert_eq!(&wire[2..], bundle.data());
    }

    /// The escape: a frame of exactly `UINT16_MAX` uses the `0xffff` marker
    /// and the `u32` length; one byte under does not.
    ///
    /// Both bundles use an escaped message length, so the frame is
    /// `[flags][id][0xff field][u32 length][payload]` -- seven bytes of
    /// header before the body, and the payload that lands the frame on a
    /// given size follows from that.
    #[test]
    fn frame_encode_escapes_at_the_u16_boundary() {
        // 65534 = 0xfffe fits, 65535 = 0xffff escapes.
        let mut small = TcpBundle::new(MAX_SEGMENT);
        small.start_message(&var(VAR1_MSG_ID, 1));
        small.write_bytes(&[0xab; LARGE_FRAME_ESCAPE as usize - 8]);
        small.finalise();
        assert_eq!(small.size(), LARGE_FRAME_ESCAPE as usize - 1);
        assert_eq!(&small.frame_encode()[..2], &[0xfe, 0xff]);

        let mut big = TcpBundle::new(MAX_SEGMENT);
        big.start_message(&var(VAR1_MSG_ID, 1));
        big.write_bytes(&[0xab; LARGE_FRAME_ESCAPE as usize - 7]);
        big.finalise();
        assert_eq!(big.size(), LARGE_FRAME_ESCAPE as usize);
        let wire = big.frame_encode();
        assert_eq!(&wire[..2], &[0xff, 0xff]);
        assert_eq!(read_le32(&wire[2..6]), LARGE_FRAME_ESCAPE as u32);
        assert_eq!(
            wire.len(),
            LARGE_FRAME_HEADER_SIZE + LARGE_FRAME_ESCAPE as usize
        );
    }

    // ------------------------------------------------------------------
    // The frame framing (`TCPChannel::handleInput`): the acceptance for
    // BW-02-04-01 -- a frame arriving in short reads and several frames in
    // one read.
    // ------------------------------------------------------------------

    /// A frame whose bytes arrive one at a time is only a frame once the
    /// last body byte has arrived.
    #[test]
    fn a_short_read_holds_a_frame_until_the_last_byte_arrives() {
        let mut assembler = TcpFrameAssembler::new();
        let bytes: &[u8] = &[6, 0, 1, 2, 3, 4, 5, 6];

        for byte in &bytes[..bytes.len() - 1] {
            assert!(
                assembler.push(std::slice::from_ref(byte)).is_empty(),
                "a partial frame is not a frame"
            );
        }

        let frames = assembler.push(&bytes[bytes.len() - 1..]);
        assert_eq!(frames.len(), 1, "the last byte completes the frame");
        assert_eq!(frames[0], vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(assembler.num_data_units_received(), 1);
        assert_eq!(assembler.num_bytes_received(), 8, "the header counts too");
    }

    /// A length header split across reads waits for the second byte.
    #[test]
    fn a_split_length_header_waits_for_the_rest() {
        let mut assembler = TcpFrameAssembler::new();
        assert!(
            assembler.push(&[0x05]).is_empty(),
            "one header byte is not a length"
        );
        assert!(
            assembler.push(&[0x00]).is_empty(),
            "the length is whole, the body is not"
        );
        assert!(
            assembler.push(&[1, 2, 3]).is_empty(),
            "three of the five body bytes"
        );
        let frames = assembler.push(&[4, 5]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![1, 2, 3, 4, 5]);
    }

    /// Several frames in one read are each returned in order, and the
    /// bytes after the last frame stay for the next read.
    #[test]
    fn a_coalesced_read_dispatches_every_frame_in_order() {
        let mut assembler = TcpFrameAssembler::new();

        // Two frames in one read: [3,0,1,2,3] and [2,0,4,5].
        let frames = assembler.push(&[3, 0, 1, 2, 3, 2, 0, 4, 5]);
        assert_eq!(frames.len(), 2, "both frames complete");
        assert_eq!(frames[0], vec![1, 2, 3]);
        assert_eq!(frames[1], vec![4, 5]);
        assert_eq!(assembler.num_data_units_received(), 2);
        assert_eq!(assembler.num_bytes_received(), 9);

        // The assembler carries what it did not consume: a frame split
        // across the read boundary completes on the next push.
        assert!(assembler.push(&[2]).is_empty(), "the first header byte");
        assert!(
            assembler.push(&[0]).is_empty(),
            "the header is whole, the body is not"
        );
        assert!(assembler.push(&[9]).is_empty(), "one of the two body bytes");
        let frames = assembler.push(&[8]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![9, 8]);
    }

    /// An escaped frame length arrives as the `u16` marker, then the `u32`
    /// and the body.
    #[test]
    fn an_escaped_frame_length_comes_in_two_reads() {
        let mut assembler = TcpFrameAssembler::new();
        assert!(
            assembler.push(&[0xff, 0xff, 0x08, 0x00]).is_empty(),
            "the marker and the low half of the length"
        );
        assert!(
            assembler.push(&[0x00, 0x00]).is_empty(),
            "the length is whole, the body is not"
        );
        let frames = assembler.push(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(assembler.num_data_units_received(), 1);
        assert_eq!(assembler.num_bytes_received(), 6 + 8);
    }

    // ------------------------------------------------------------------
    // The walk (`TCPBundleProcessor::dispatchMessages`)
    // ------------------------------------------------------------------

    /// `flags & ~FLAG_MASK`: a flag bit the protocol does not define is a
    /// corrupted bundle.
    #[test]
    fn flags_bits_the_mask_refuses_are_corrupted() {
        let table = InterfaceTable::new();
        let frame = [0x02, FIXED_MSG_ID, 0, 0, 0, 0];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
    }

    /// A first request offset past the frame end is corrupted
    /// (`tcp_bundle_processor.cpp:76-85`). It has to be a real positive
    /// offset: the `0xffffffff` sentinel casts to `-1` in the reference's
    /// `static_cast<int>` check and is allowed through.
    #[test]
    fn a_first_request_offset_past_the_frame_is_corrupted() {
        let table = InterfaceTable::new();
        let frame = [FLAG_HAS_REQUESTS, 0xff, 0xff, 0x00, 0x00];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
    }

    /// A frame too short for the request offset header is corrupted.
    #[test]
    fn a_truncated_request_header_is_corrupted() {
        let table = InterfaceTable::new();
        let frame = [FLAG_HAS_REQUESTS, 0xff, 0xff];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
    }

    /// An id no handler serves ends the walk with
    /// `REASON_NONEXISTENT_ENTRY`, and a good message after it is never
    /// delivered.
    #[test]
    fn an_unhandled_message_id_ends_the_walk() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        serve(&table, &var(VAR1_MSG_ID, 1), &known);

        let mut frame = vec![0x00, 0x09, 0x03, b'r', b'o', b'g'];
        frame.extend_from_slice(&[VAR1_MSG_ID, 2, 1, 2]);

        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::NonexistentEntry
        );
        assert!(
            known.notes().is_empty(),
            "nothing after the unknown id is delivered"
        );
    }

    /// A body shorter than the length declares is corrupted.
    #[test]
    fn a_body_shorter_than_the_length_is_corrupted() {
        let table = InterfaceTable::new();
        let known = Rc::new(Recorder::default());
        serve(&table, &var(VAR1_MSG_ID, 1), &known);

        let frame = vec![0x00, VAR1_MSG_ID, 10, b'x', b'y', b'z'];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
        assert!(known.notes().is_empty());
    }

    /// A frame written by the bundle delivers every message in order, and
    /// a plain message answers no request.
    #[test]
    fn a_frame_written_and_read_back_delivers_every_message_in_order() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(b"first");
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(b"second");
        bundle.finalise();

        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &var(VAR1_MSG_ID, 1), &recorder);

        assert_eq!(
            dispatch_messages(&table, &source(), bundle.data()),
            Reason::Success
        );
        let notes = recorder.notes();
        assert_eq!(notes.len(), 2, "both messages are delivered, in order");
        assert_eq!(notes[0].2, b"first");
        assert_eq!(notes[1].2, b"second");
        assert_eq!(
            notes[0].1, REPLY_ID_NONE,
            "a plain message answers no request"
        );
    }

    /// A request chain walks to the end, each request carrying its reply
    /// id placeholder and its own payload.
    #[test]
    fn a_request_chain_walks_and_carries_the_reply_ids() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_request(&request_element(), reply_handler(), 0, 0);
        bundle.write_bytes(&[0x01, 0x02]);
        bundle.start_request(&request_element(), reply_handler(), 0, 0);
        bundle.write_bytes(&[0x03, 0x04, 0x05]);
        bundle.finalise();

        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &request_element(), &recorder);

        assert_eq!(
            dispatch_messages(&table, &source(), bundle.data()),
            Reason::Success
        );
        let notes = recorder.notes();
        assert_eq!(notes.len(), 2);
        assert_eq!(
            notes[0].1, 0x1234_5678,
            "the placeholder, until the request manager writes the real id"
        );
        assert_eq!(notes[1].1, 0x1234_5678);
        assert_eq!(notes[0].2, vec![0x01, 0x02]);
        assert_eq!(notes[1].2, vec![0x03, 0x04, 0x05]);
    }

    /// A reply message delivers its reply id as the body.
    #[test]
    fn a_reply_message_carries_its_reply_id() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_reply(REPLY_ID);
        bundle.finalise();

        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &InterfaceElement::reply(), &recorder);

        assert_eq!(
            dispatch_messages(&table, &source(), bundle.data()),
            Reason::Success
        );
        let notes = recorder.notes();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, REPLY_MESSAGE_IDENTIFIER);
        // The reference sets `REPLY_ID_NONE` on the header for a
        // non-request; the reply id is the message's body, and the
        // request manager reads it there.
        assert_eq!(notes[0].1, REPLY_ID_NONE);
        assert_eq!(notes[0].2, [0xef, 0xbe, 0xad, 0xde].to_vec());
    }

    /// The acceptance end to end: a framed bundle arrives in both read
    /// styles and delivers the same messages either way.
    #[test]
    fn a_short_and_a_coalesced_read_both_deliver_the_frame() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(b"one");
        bundle.start_message(&var(VAR1_MSG_ID, 1));
        bundle.write_bytes(b"two");
        bundle.finalise();
        let wire = bundle.frame_encode();

        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &var(VAR1_MSG_ID, 1), &recorder);

        // One byte per read.
        let mut short = TcpFrameAssembler::new();
        let mut frames = Vec::new();
        for byte in &wire {
            frames.extend(short.push(std::slice::from_ref(byte)));
        }
        assert_eq!(frames.len(), 1, "the short read completes the frame");
        assert_eq!(
            dispatch_messages(&table, &source(), &frames[0]),
            Reason::Success
        );
        assert_eq!(recorder.notes().len(), 2);

        // Everything at once.
        let mut coalesced = TcpFrameAssembler::new();
        let frames = coalesced.push(&wire);
        assert_eq!(frames.len(), 1, "the coalesced read completes the frame");
        assert_eq!(frames[0], bundle.data().to_vec());
    }

    /// `updateLengthDetails` failing: a callback message whose handler never
    /// overrides the sizing answers -1, so the walk stops without a reason
    /// for the message itself.
    #[test]
    fn a_callback_message_that_cannot_be_sized_is_corrupted() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(
            &table,
            &InterfaceElement::new("TestCallback", 0x99, CALLBACK_LENGTH_MESSAGE, 0, None),
            &recorder,
        );

        let frame = [0x00, 0x99, 3, b'h', b'i'];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
        assert!(recorder.notes().is_empty());
    }

    /// A frame that ends inside the message's length field cannot be
    /// expanded.
    #[test]
    fn a_length_field_the_frame_cuts_off_is_corrupted() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &var(VAR1_MSG_ID, 1), &recorder);

        let frame = [0x00, VAR1_MSG_ID];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
    }

    /// A request whose frame ends before its eight-byte reply preamble is
    /// corrupted, the frame's first offset being legal.
    #[test]
    fn a_request_cut_off_its_preamble_is_corrupted() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &request_element(), &recorder);

        // flags, first offset 5, id, a zero length field, two of the
        // eight preamble bytes.
        let frame = [FLAG_HAS_REQUESTS, 5, 0, 0, 0, REQ_MSG_ID, 0x00, 0x78, 0x56];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
        assert!(recorder.notes().is_empty());
    }

    /// A request chain whose next offset is a real offset past the frame
    /// end is corrupted -- not the `0xffffffff` sentinel, which ends the
    /// chain.
    #[test]
    fn a_chain_next_offset_past_the_frame_is_corrupted() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &request_element(), &recorder);

        // flags, first offset 5, id, a zero length field, the reply id
        // placeholder, and a next offset of 256.
        let frame = [
            FLAG_HAS_REQUESTS,
            5,
            0,
            0,
            0,
            REQ_MSG_ID,
            0x00,
            0x78,
            0x56,
            0x34,
            0x12,
            0x00,
            0x01,
            0x00,
            0x00,
        ];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
        assert!(recorder.notes().is_empty());
    }

    /// A handler that leaves body bytes is reported, and the walk still
    /// delivers what it did eat.
    #[test]
    fn a_message_that_leaves_body_bytes_is_reported() {
        let table = InterfaceTable::new();
        let partial = Rc::new(PartialRecorder {
            keep: 1,
            notes: RefCell::new(Vec::new()),
        });
        let handler = partial.clone() as Rc<dyn InputMessageHandler>;
        table.serve(&var(VAR1_MSG_ID, 1), Some(handler));

        let frame = [0x00, VAR1_MSG_ID, 0x02, 0xaa, 0xbb];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::Success
        );
        let notes = partial.notes.borrow();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].2, vec![0xaa], "the one byte it did eat");
    }

    /// A reply's leftovers are reported by another name, and not consumed.
    #[test]
    fn a_reply_that_leaves_body_bytes_is_reported() {
        let mut bundle = TcpBundle::new(MAX_SEGMENT);
        bundle.start_reply(REPLY_ID);
        bundle.finalise();

        let table = InterfaceTable::new();
        let partial = Rc::new(PartialRecorder {
            keep: 0,
            notes: RefCell::new(Vec::new()),
        });
        let handler = partial.clone() as Rc<dyn InputMessageHandler>;
        table.serve(&InterfaceElement::reply(), Some(handler));

        assert_eq!(
            dispatch_messages(&table, &source(), bundle.data()),
            Reason::Success
        );
        assert_eq!(partial.notes.borrow().len(), 1);
    }

    // ------------------------------------------------------------------
    // Every length width, and the escape they share
    // ------------------------------------------------------------------

    /// Widths 1 to 4 each read their own length off the wire, and a
    /// variable message with no length field carries no body. One frame
    /// holds all of them, the walk walking through each.
    #[test]
    fn every_length_width_reads_its_own_length_off_the_wire() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &var(0x77, 0), &recorder);
        serve(&table, &var(0x33, 2), &recorder);
        serve(&table, &var(0x55, 3), &recorder);
        serve(&table, &var(0x66, 4), &recorder);

        let frame = [
            0x00, // flags
            0x77, // no length field, no body
            0x33, 0x02, 0x00, 0xaa, 0xbb, 0x55, 0x03, 0x00, 0x00, 0xaa, 0xbb, 0xcc, 0x66, 0x04,
            0x00, 0x00, 0x00, 0xaa, 0xbb, 0xcc, 0xdd,
        ];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::Success
        );
        let notes = recorder.notes();
        assert_eq!(notes.len(), 4, "every width's message is delivered");
        assert!(notes[0].2.is_empty(), "a width-zero message has no body");
        assert_eq!(notes[1].2, vec![0xaa, 0xbb], "the width-two length reads 2");
        assert_eq!(
            notes[2].2,
            vec![0xaa, 0xbb, 0xcc],
            "the width-three length reads 3"
        );
        assert_eq!(
            notes[3].2,
            vec![0xaa, 0xbb, 0xcc, 0xdd],
            "the width-four length reads 4"
        );
    }

    /// The escape form: a length field of all ones, the real length
    /// following as a `u32`.
    #[test]
    fn a_walk_reads_an_escaped_length_off_the_wire() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &var(VAR1_MSG_ID, 1), &recorder);

        let frame = [0x00, VAR1_MSG_ID, 0xff, 3, 0, 0, 0, 0xaa, 0xbb, 0xcc];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::Success
        );
        let notes = recorder.notes();
        assert_eq!(notes.len(), 1);
        assert_eq!(
            notes[0].2,
            vec![0xaa, 0xbb, 0xcc],
            "the appended length wins"
        );
    }

    /// An escape marker whose appended length is cut off is corrupted.
    #[test]
    fn an_escaped_length_cut_off_its_appendage_is_corrupted() {
        let table = InterfaceTable::new();
        let recorder = Rc::new(Recorder::default());
        serve(&table, &var(VAR1_MSG_ID, 1), &recorder);

        let frame = [0x00, VAR1_MSG_ID, 0xff, 0x03];
        assert_eq!(
            dispatch_messages(&table, &source(), &frame),
            Reason::CorruptedPacket
        );
        assert!(recorder.notes().is_empty());
    }

    /// `Default` gives the empty assembler.
    #[test]
    fn the_assemblers_default_is_the_empty_assembler() {
        let mut assembler = TcpFrameAssembler::default();
        assert!(assembler.push(&[]).is_empty());
        assert_eq!(assembler.num_data_units_received(), 0);
        assert_eq!(assembler.num_bytes_received(), 0);
    }
}
