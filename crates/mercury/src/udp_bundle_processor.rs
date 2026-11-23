//! `Mercury::UDPBundleProcessor` (`lib/network/udp_bundle_processor.cpp/.hpp`):
//! the decode half of the BW-02-03-01 codec.
//!
//! A received bundle is a chain of packets with their footers already
//! stripped (see [`Packet::strip_receive_footers`]); this module walks the
//! chain and unpacks each message, then delivers it to the handler the
//! interface table serves for its id.
//!
//! The iterator mirrors the reference's `UDPBundleProcessor::iterator`
//! exactly: it advances across the chain, and a message that spans packets
//! (the special form, or a body longer than the packet it started in) is
//! stitched into a temporary buffer by [`data`](BundleIterator::data).
//! The framing is chain-aware because a header or a special-form tail can
//! straddle a packet boundary (BW-02-02-02's deferred cross-packet
//! requirement).

use std::cell::RefCell;
use std::rc::Rc;

use foundation::stream::BinaryStream;

use crate::endpoint::Address;
use crate::interface_element::{
    FIXED_LENGTH_MESSAGE, INVALID_MESSAGE, InterfaceElement, REPLY_MESSAGE_IDENTIFIER,
    VARIABLE_LENGTH_MESSAGE,
};
use crate::interface_table::InterfaceTable;
use crate::message_framing::{
    FrameError, NEXT_REQUEST_OFFSET_SIZE, REPLY_ID_SIZE, SPECIAL_LENGTH_SIZE,
};
use crate::misc::{MessageID, REPLY_ID_NONE, Reason};
use crate::network_interface::ChannelRef;
use crate::packet::{HEADER_SIZE, Packet};
use crate::unpacked_message_header::UnpackedMessageHeader;

/// Read `out.len()` bytes starting at `(node, offset)`, walking the chain
/// across packet boundaries (a fresh packet's body starts at `HEADER_SIZE`).
/// `None` when the chain runs out before `out.len()` bytes.
fn chain_read(node: &Rc<RefCell<Packet>>, start: usize, out: &mut [u8]) -> Option<()> {
    let mut node = Rc::clone(node);
    let mut offset = start;
    let mut done = 0;
    loop {
        // The `Ref` borrows through `node`, so the next packet is read out
        // (and the borrow dropped) before `node` is reassigned.
        let next = {
            let packet = node.borrow();
            let avail = packet.msg_end_offset().checked_sub(offset)?;
            let take = avail.min(out.len() - done);
            if take > 0 {
                out[done..done + take].copy_from_slice(packet.byte_range(offset, take));
                done += take;
            }
            if done == out.len() {
                return Some(());
            }
            packet.next()?
        };
        node = next;
        offset = HEADER_SIZE;
    }
}

/// `BundleDataPos::advance( n )`: where `n` bytes past `(node, offset)` land
/// in the chain, as `(packet, intra-packet offset)`.
///
/// The chain is a stream where each packet contributes
/// `msg_end_offset() - HEADER_SIZE` bytes, so the offset is not absolute and
/// has to be walked: each packet gives up its remaining span and the rest
/// starts at the next packet's body. `None` when the chain runs out first.
fn chain_advance(
    node: &Rc<RefCell<Packet>>,
    offset: usize,
    bytes: usize,
) -> Option<(Rc<RefCell<Packet>>, usize)> {
    let mut node = Rc::clone(node);
    let mut offset = offset;
    let mut remaining = bytes;

    loop {
        let next = {
            let packet = node.borrow();
            let avail = packet.msg_end_offset().checked_sub(offset)?;
            if remaining <= avail {
                return Some((Rc::clone(&node), offset + remaining));
            }
            remaining -= avail;
            packet.next()?
        };
        node = next;
        offset = HEADER_SIZE;
    }
}

/// Write `bytes` starting at `(node, offset)`, walking the chain. The write
/// positions are always inside reserved space (`msg_end_offset`), so this
/// never grows a packet.
fn chain_write(node: &Rc<RefCell<Packet>>, start: usize, bytes: &[u8]) -> Option<()> {
    let mut node = Rc::clone(node);
    let mut offset = start;
    let mut done = 0;
    loop {
        let next = {
            let mut packet = node.borrow_mut();
            let avail = packet.msg_end_offset().checked_sub(offset)?;
            let take = avail.min(bytes.len() - done);
            if take > 0 {
                packet
                    .byte_range_mut(offset, take)
                    .copy_from_slice(&bytes[done..done + take]);
                done += take;
            }
            if done == bytes.len() {
                return Some(());
            }
            packet.next()?
        };
        node = next;
        offset = HEADER_SIZE;
    }
}

impl InterfaceElement {
    /// `expandLength( data, pPacket, isRequest )` over a packet chain: read
    /// the length field, and for the special form move the last four body
    /// bytes back to the head.
    ///
    /// The header (id and length field) always sits inside the current
    /// packet -- [`BundleIterator::unpack`] checks that -- but the special
    /// form's true-length read and its displaced-tail walk may cross a
    /// packet boundary, so this walks the chain where the contiguous
    /// [`super::message_framing::InterfaceElement::expand_length`] cannot.
    pub(crate) fn chain_expand_length(
        &self,
        cursor: &Rc<RefCell<Packet>>,
        msg_beg: usize,
        is_request: bool,
    ) -> Result<i32, FrameError> {
        match self.length_style() {
            FIXED_LENGTH_MESSAGE => Ok(self.length_param()),

            VARIABLE_LENGTH_MESSAGE => {
                let width = self.length_param().max(0) as usize;
                let mut field = [0u8; 4];
                {
                    let packet = cursor.borrow();
                    if msg_beg + 1 + width > packet.msg_end_offset() {
                        return Err(FrameError::BufferTooSmall);
                    }
                    field[..width].copy_from_slice(packet.byte_range(msg_beg + 1, width));
                }

                let raw: u32 = match width {
                    1 => u32::from(field[0]),
                    2 => u32::from(u16::from_le_bytes([field[0], field[1]])),
                    3 => u32::from_le_bytes([field[0], field[1], field[2], 0]),
                    4 => u32::from_le_bytes(field),
                    // The reference's default arm answers zero.
                    _ => 0,
                };

                let len = raw as i32;
                // A width-4 field with the top bit set overflows the int.
                if len < 0 {
                    return Err(FrameError::Overflow);
                }

                if self.can_handle_length_for(len) {
                    return Ok(len);
                }

                // Special form: the four bytes at the head of the body hold
                // the true length; the last four body bytes are the
                // displaced originals, now moved back to the head. The head
                // is inside the current packet, but the tail is a chain
                // walk away.
                let body = msg_beg + self.body_offset(is_request);
                let mut head = [0u8; SPECIAL_LENGTH_SIZE];
                chain_read(cursor, body, &mut head).ok_or(FrameError::BufferTooSmall)?;
                let true_len = i32::from_le_bytes(head);
                if true_len < 0 {
                    return Err(FrameError::Overflow);
                }
                let (tail_node, tail_offset) = chain_advance(cursor, body, true_len as usize)
                    .ok_or(FrameError::BufferTooSmall)?;
                let mut displaced = [0u8; SPECIAL_LENGTH_SIZE];
                chain_read(&tail_node, tail_offset, &mut displaced)
                    .ok_or(FrameError::BufferTooSmall)?;
                chain_write(cursor, body, &displaced).ok_or(FrameError::BufferTooSmall)?;
                Ok(true_len)
            }

            _ => Err(FrameError::UnknownLengthStyle),
        }
    }

    /// `unexpandLength( data, length, pPacket, isRequest )` over a packet
    /// chain: the inverse of [`chain_expand_length`](Self::chain_expand_length),
    /// used by [`BundleIterator::advance`] to restore the special form.
    pub(crate) fn chain_unexpand_length(
        &self,
        cursor: &Rc<RefCell<Packet>>,
        msg_beg: usize,
        length: i32,
        is_request: bool,
    ) -> Result<(), FrameError> {
        if self.length_style() != VARIABLE_LENGTH_MESSAGE {
            return Ok(());
        }

        let special = match self.length_param() {
            1 => length >= 0xff,
            2 => length >= 0xffff,
            3 => length >= 0xffffff,
            _ => false,
        };

        if !special {
            return Ok(());
        }

        // Re-create the special form: the first four body bytes (now the
        // restored originals) go back to the tail, wherever the chain puts it.
        let body = msg_beg + self.body_offset(is_request);
        let mut head = [0u8; SPECIAL_LENGTH_SIZE];
        chain_read(cursor, body, &mut head).ok_or(FrameError::BufferTooSmall)?;
        let (tail_node, tail_offset) =
            chain_advance(cursor, body, length as usize).ok_or(FrameError::BufferTooSmall)?;
        chain_write(&tail_node, tail_offset, &head).ok_or(FrameError::BufferTooSmall)?;
        Ok(())
    }
}

/// `UDPBundleProcessor`: walks a received packet chain and dispatches its
/// messages.
pub struct UDPBundleProcessor {
    /// `pFirstPacket_`.
    first_packet: Option<Rc<RefCell<Packet>>>,
    /// `isEarly_`.
    is_early: bool,
}

impl UDPBundleProcessor {
    /// `UDPBundleProcessor( p, isEarly )`.
    pub fn new(first_packet: Option<Rc<RefCell<Packet>>>, is_early: bool) -> Self {
        Self {
            first_packet,
            is_early,
        }
    }

    /// `begin()`: an iterator at the first message (or the end).
    pub fn begin(&self) -> BundleIterator {
        BundleIterator::new(self.first_packet.clone())
    }

    /// `end()`: the past-the-end iterator.
    pub fn end() -> BundleIterator {
        BundleIterator::end()
    }

    /// `dispatchMessages`: iterate the bundle and deliver each message to
    /// the handler the table serves for its id.
    ///
    /// The channel arm (`pChannel->isDead` break-loop, message filters) is
    /// the BW-02-03-02 seam; off-channel it stays quiet.
    pub fn dispatch_messages(
        &self,
        interface_table: &InterfaceTable,
        addr: &Address,
        _channel: Option<&Rc<dyn ChannelRef>>,
    ) -> Reason {
        let mut ret = Reason::Success;
        let mut iter = self.begin();

        while !iter.is_end() {
            let msg_id = iter.msg_id();

            // Look up the element and handler for this id, validating that
            // it can be delivered.
            let (updated_ie, handler) = match interface_table.resolve_message(msg_id) {
                Ok(pair) => pair,
                Err(reason) => {
                    ret = reason;
                    break;
                }
            };

            if self.is_early && !updated_ie.should_process_early() {
                iter.advance();
                continue;
            }

            // Unpack the header (may rewrite the frame for the special
            // form).
            let header = match iter.unpack(&updated_ie) {
                Some(header) => *header,
                None => {
                    ret = Reason::CorruptedPacket;
                    break;
                }
            };

            let Some(msg_data) = iter.data() else {
                ret = Reason::CorruptedPacket;
                break;
            };

            // Hand the handler only the declared body bytes; the four stale
            // tail bytes of the special form are not part of it.
            let mut data = BinaryStream::new();
            data.write_bytes(&msg_data[..header.length as usize]);

            handler.handle_message(addr, &mut UnpackedMessageHeader { ..header }, &mut data);

            iter.advance();

            if data.remaining() != 0 {
                // The reference warns and calls `mis.finish()`. Both the
                // port and the reference keep going; only the label differs
                // by whether this was a reply.
                if header.identifier == REPLY_MESSAGE_IDENTIFIER {
                    eprintln!(
                        "UDPBundleProcessor::dispatchMessages( {} ): Handler for reply left {} bytes",
                        addr.c_str(),
                        data.remaining()
                    );
                } else {
                    eprintln!(
                        "UDPBundleProcessor::dispatchMessages( {} ): Handler for message {} (ID {}) left {} bytes",
                        addr.c_str(),
                        updated_ie.name(),
                        header.identifier,
                        data.remaining()
                    );
                    data.finish();
                }
            }
        }

        ret
    }
}

/// `UDPBundleProcessor::iterator`: one position in the message walk of a
/// bundle.
pub struct BundleIterator {
    /// `cursor_`.
    cursor: Option<Rc<RefCell<Packet>>>,
    /// `isUnpacked_`.
    is_unpacked: bool,
    /// `bodyEndOffset_`.
    body_end_offset: usize,
    /// `offset_`: the current message's start (`msgBeg`).
    offset: usize,
    /// `dataOffset_`.
    data_offset: usize,
    /// `dataLength_`.
    data_length: usize,
    /// `dataBuffer_`: the stitched temp buffer for a spanning message.
    data_buffer: Option<Vec<u8>>,
    /// `nextRequestOffset_`.
    next_request_offset: usize,
    /// `curHeader_`.
    cur_header: UnpackedMessageHeader,
    /// `updatedIE_`.
    updated_ie: InterfaceElement,
}

impl BundleIterator {
    /// `iterator( first )`: find the first packet with body data.
    fn new(first: Option<Rc<RefCell<Packet>>>) -> Self {
        let mut iter = Self {
            cursor: None,
            is_unpacked: false,
            body_end_offset: 0,
            offset: 0,
            data_offset: 0,
            data_length: 0,
            data_buffer: None,
            next_request_offset: 0,
            cur_header: UnpackedMessageHeader::default(),
            updated_ie: InterfaceElement::new("", 0, INVALID_MESSAGE, 0, None),
        };

        // A packet can have no body (only footers); skip to the first one
        // with a message. Like the reference's `for (cursor_ = first; ...)`:
        // when the chain runs out, the cursor stays null, so the iterator
        // is the end.
        let mut node = first;
        while let Some(packet) = node {
            iter.cursor = Some(Rc::clone(&packet));
            iter.next_packet();
            if iter.offset < iter.body_end_offset {
                break;
            }
            iter.cursor = None;
            node = packet.borrow().next();
        }

        iter
    }

    /// `iterator( NULL )`: the end.
    fn end() -> Self {
        Self::new(None)
    }

    /// `nextPacket`: set up the iterator for the packet now at the cursor.
    fn next_packet(&mut self) {
        let cursor = self.cursor.as_ref().expect("nextPacket with no cursor");
        let packet = cursor.borrow();
        self.next_request_offset = packet.first_request_offset() as usize;
        self.body_end_offset = packet.msg_end_offset();
        self.offset = HEADER_SIZE;
    }

    /// `isUnpacked`.
    pub fn is_unpacked(&self) -> bool {
        self.is_unpacked
    }

    /// Whether this iterator is past the last message.
    pub fn is_end(&self) -> bool {
        self.cursor.is_none()
    }

    /// `msgID`.
    pub fn msg_id(&self) -> MessageID {
        let cursor = self.cursor.as_ref().expect("msgID at the end");
        let packet = cursor.borrow();
        packet.byte_range(self.offset, 1)[0]
    }

    /// `unpack( ie )`: read the header of the message at the cursor. On
    /// success returns the header; `None` is the reference's
    /// "corrupted header" error. Must be called once per message before
    /// [`data`](Self::data) and [`advance`](Self::advance).
    pub fn unpack(&mut self, ie: &InterfaceElement) -> Option<&UnpackedMessageHeader> {
        debug_assert!(!self.is_unpacked, "unpack twice without advance");
        let cursor = self.cursor.clone()?;
        let msg_beg = self.offset;
        let is_request = self.next_request_offset == self.offset;

        self.updated_ie = ie.clone();
        let header_len = self.updated_ie.header_size().max(0) as usize;

        // The header must fit in the current packet.
        if msg_beg + header_len > self.body_end_offset {
            return None;
        }

        self.cur_header.identifier = self.msg_id();
        let length = self
            .updated_ie
            .chain_expand_length(&cursor, msg_beg, is_request)
            .ok()?;
        if length < 0 {
            return None;
        }
        self.cur_header.length = length;

        let mut msg_beg = msg_beg + header_len;

        if !is_request {
            self.cur_header.reply_id = REPLY_ID_NONE;
        } else {
            // Request preamble: the reply id and the next request offset.
            if msg_beg + REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE > self.body_end_offset {
                return None;
            }
            let mut preamble = [0u8; REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE];
            chain_read(&cursor, msg_beg, &mut preamble)?;
            self.cur_header.reply_id =
                i32::from_le_bytes(preamble[..REPLY_ID_SIZE].try_into().unwrap());
            self.next_request_offset = u16::from_le_bytes(
                preamble[REPLY_ID_SIZE..REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE]
                    .try_into()
                    .unwrap(),
            ) as usize;
            if self.next_request_offset > self.body_end_offset {
                return None;
            }
            msg_beg += REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE;
        }

        // The payload must fit the chain: if it is not all in this packet,
        // the next one must exist.
        if msg_beg + length as usize > self.body_end_offset && cursor.borrow().next().is_none() {
            return None;
        }

        self.data_offset = msg_beg;
        self.data_length = length as usize;

        // The special form leaves four extra bytes at the end of the body,
        // which the caller's handler must not see but `operator++` needs to
        // restore.
        if !self.updated_ie.can_handle_length_for(length) {
            self.data_length += SPECIAL_LENGTH_SIZE;
        }

        self.is_unpacked = true;
        Some(&self.cur_header)
    }

    /// `data`: the bytes of the current message's body, stitched across
    /// packets when it spans. `None` when the chain is too short.
    pub fn data(&mut self) -> Option<Vec<u8>> {
        debug_assert!(self.is_unpacked, "data before unpack");
        let cursor = self.cursor.clone()?;
        let data_offset = self.data_offset;
        let data_length = self.data_length;
        let body_end_offset = self.body_end_offset;

        if data_offset + data_length <= body_end_offset {
            // All in the current packet.
            let packet = cursor.borrow();
            return Some(packet.byte_range(data_offset, data_length).to_vec());
        }

        // Spanning: bound the allocation by what the chain actually holds
        // (the declared length is peer-influenced), then stitch.
        let mut available = 0usize;
        {
            let mut node = Some(Rc::clone(&cursor));
            let mut offset = data_offset;
            while let Some(packet) = node {
                let packet = packet.borrow();
                if packet.msg_end_offset() < offset {
                    return None;
                }
                available += packet.msg_end_offset() - offset;
                offset = HEADER_SIZE;
                node = packet.next();
            }
        }
        if data_length > available {
            return None;
        }

        let mut buffer = vec![0u8; data_length];
        {
            let mut node = Some(cursor);
            let mut offset = data_offset;
            let mut done = 0;
            while done < data_length {
                let packet = node.clone()?;
                let packet_ref = packet.borrow();
                if packet_ref.msg_end_offset() < offset {
                    return None;
                }
                let take = (packet_ref.msg_end_offset() - offset).min(data_length - done);
                if take > 0 {
                    buffer[done..done + take].copy_from_slice(packet_ref.byte_range(offset, take));
                    done += take;
                }
                if done < data_length {
                    offset = HEADER_SIZE;
                    node = packet_ref.next();
                }
            }
        }
        Some(buffer)
    }

    /// `operator++`: advance to the next message.
    pub fn advance(&mut self) {
        debug_assert!(self.is_unpacked, "operator++ before unpack");
        self.is_unpacked = false;

        // Restore the special form before moving on.
        if let Some(cursor) = self.cursor.clone() {
            let is_request = self.next_request_offset == self.offset;
            let _ = self.updated_ie.chain_unexpand_length(
                &cursor,
                self.offset,
                self.cur_header.length,
                is_request,
            );
        }
        self.data_buffer = None;

        let mut bigger_offset = self.data_offset + self.data_length;
        while bigger_offset >= self.body_end_offset {
            bigger_offset -= self.body_end_offset;

            let cursor = self.cursor.clone().expect("advance with no cursor");
            match cursor.borrow().next() {
                Some(next) => {
                    self.cursor = Some(next);
                    self.next_packet();
                    bigger_offset += self.offset;
                }
                None => {
                    // Run out of packets: the iterator is now the end.
                    self.cursor = None;
                    break;
                }
            }
        }
        self.offset = bigger_offset;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use foundation::stream::BinaryStream;
    use platform::socket;

    use super::UDPBundleProcessor;
    use crate::endpoint::{Address, Endpoint, INADDR_ANY};
    use crate::event_dispatcher::EventDispatcher;
    use crate::interface_element::{
        FIXED_LENGTH_MESSAGE, InterfaceElement, REPLY_MESSAGE_IDENTIFIER, VARIABLE_LENGTH_MESSAGE,
    };
    use crate::interface_table::InterfaceTable;
    use crate::interfaces::{InputMessageHandler, ReplyMessageHandler};
    use crate::misc::{MessageID, REPLY_ID_NONE, Reason, ReplyID, SeqNumAllocator};
    use crate::nub_exception::NubException;
    use crate::packet::{Packet, flags as f};
    use crate::request_manager::RequestManager;
    use crate::udp_bundle::{ReliableType, UDPBundle};
    use crate::unpacked_message_header::UnpackedMessageHeader;

    /// One delivery: `(identifier, length, reply_id, body bytes)`.
    type Delivery = (MessageID, i32, ReplyID, Vec<u8>);

    /// A delivery that records what it was handed.
    struct Recorder {
        records: Rc<RefCell<Vec<Delivery>>>,
    }

    impl Recorder {
        fn shared() -> (Rc<Self>, Rc<RefCell<Vec<Delivery>>>) {
            let records = Rc::new(RefCell::new(Vec::new()));
            (
                Rc::new(Self {
                    records: Rc::clone(&records),
                }),
                records,
            )
        }
    }

    impl InputMessageHandler for Recorder {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            self.records.borrow_mut().push((
                header.identifier,
                header.length,
                header.reply_id,
                data.as_bytes().to_vec(),
            ));
        }
    }

    /// A reply handler stub: the request path only needs the id written.
    struct NoReply;

    impl ReplyMessageHandler for NoReply {
        fn handle_message(
            &self,
            _source: &Address,
            _header: &mut UnpackedMessageHeader,
            _data: &mut BinaryStream,
            _arg: usize,
        ) {
        }
        fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
    }

    /// A pair of real loopback UDP sockets and the bound destination.
    fn loopback() -> (Endpoint, Endpoint, Address) {
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_DGRAM);
        assert_eq!(listener.bind(0, INADDR_ANY), 0);
        let dest = listener.local();

        let mut sender = Endpoint::new();
        sender.socket(socket::SOCK_DGRAM);
        // Bind so `local()` names a precise address: an auto-bound socket
        // reports INADDR_ANY, not the interface the datagram left by.
        assert_eq!(sender.bind(0, 0x0100_007f), 0);

        (sender, listener, dest)
    }

    /// Send every datagram of the bundle over the real socket and receive
    /// them back, chaining the received packets in order. Each frame must
    /// arrive byte for byte, and each must strip its footers cleanly.
    ///
    /// Returns the head of the received chain.
    fn send_and_receive(
        sender: &Endpoint,
        listener: &Endpoint,
        dest: Address,
        first: Rc<RefCell<Packet>>,
    ) -> Rc<RefCell<Packet>> {
        let mut received: Vec<Rc<RefCell<Packet>>> = Vec::new();

        let mut node = Some(Rc::clone(&first));
        while let Some(packet) = node {
            let wire = packet.borrow().datagram().to_vec();
            node = packet.borrow().next();

            assert_eq!(
                sender.sendto(&wire, dest.port, dest.ip),
                wire.len() as i32,
                "the whole datagram goes out"
            );

            let mut frame = Packet::new();
            let mut source = Address::default();
            assert_eq!(
                frame.recv_from_endpoint(listener, &mut source),
                wire.len() as i32,
                "real UDP hands back the whole datagram"
            );
            assert_eq!(
                frame.datagram(),
                &wire[..],
                "the real UDP frame is byte for byte what was sent"
            );
            assert!(
                frame.strip_receive_footers().is_none(),
                "the receive footer strip accepts the frame"
            );
            received.push(Rc::new(RefCell::new(frame)));
        }

        for pair in received.windows(2) {
            pair[0].borrow_mut().chain(Rc::clone(&pair[1]));
        }
        received
            .into_iter()
            .next()
            .expect("a bundle has a first packet")
    }

    /// One packet, three messages, a reliable driver, an ack, checksums on.
    /// The frame goes over a real loopback socket and comes back as the same
    /// messages, in order, with the sequence number and ack stripped.
    #[test]
    fn a_real_udp_frame_round_trips_a_single_packet_bundle() {
        let (recorder, records) = Recorder::shared();
        let recorder: Rc<dyn InputMessageHandler> = recorder;
        let table = InterfaceTable::new();
        let wide = InterfaceElement::new("wide", 3, VARIABLE_LENGTH_MESSAGE, 1, None);
        let echo = InterfaceElement::new("echo", 2, VARIABLE_LENGTH_MESSAGE, 4, None);
        let ping = InterfaceElement::new("ping", 1, FIXED_LENGTH_MESSAGE, 0, None);
        table.serve(&wide, Some(Rc::clone(&recorder)));
        table.serve(&echo, Some(Rc::clone(&recorder)));
        table.serve(&ping, Some(Rc::clone(&recorder)));

        let mut bundle = UDPBundle::new(0);
        bundle.start_message(&wide, ReliableType::Driver);
        bundle.add_blob(&[0x01, 0x02, 0x03, 0x04, 0x05]);
        bundle.start_message(&echo, ReliableType::No);
        bundle.add_blob(&[0xaa, 0xbb, 0xcc, 0xdd]);
        bundle.start_message(&ping, ReliableType::No);
        bundle.add_ack(19);
        bundle.finalise();

        let mut allocator = SeqNumAllocator::new(0);
        assert!(
            bundle.prepare_packets(None, &mut allocator, true).is_none(),
            "off-channel there is no resend window to overflow"
        );
        assert_eq!(bundle.num_data_units(), 1, "it all fits one packet");

        let (sender, listener, dest) = loopback();
        let head = send_and_receive(&sender, &listener, dest, bundle.first_packet().unwrap());

        {
            let received = head.borrow();
            assert!(received.has_flags(f::HAS_CHECKSUM));
            assert!(received.has_flags(f::IS_RELIABLE));
            assert!(received.has_flags(f::HAS_SEQUENCE_NUMBER));
            assert!(received.has_flags(f::HAS_ACKS));
            assert!(!received.has_flags(f::IS_FRAGMENT));
            assert_eq!(received.n_acks(), 1);
            assert_eq!(received.seq(), 0, "the allocator's first sequence number");
        }

        let processor = UDPBundleProcessor::new(Some(head), false);
        let ret = processor.dispatch_messages(&table, &dest, None);
        assert_eq!(ret, Reason::Success);

        let records = records.borrow();
        assert_eq!(records.len(), 3, "all three messages are delivered");
        assert_eq!(
            records[0],
            (3u8, 5, REPLY_ID_NONE, vec![0x01, 0x02, 0x03, 0x04, 0x05])
        );
        assert_eq!(
            records[1],
            (2u8, 4, REPLY_ID_NONE, vec![0xaa, 0xbb, 0xcc, 0xdd])
        );
        assert_eq!(records[2], (1u8, 0, REPLY_ID_NONE, Vec::new()));
    }

    /// A width-1 variable message whose body overflows both the packet and
    /// the one-byte length field. The body takes the special form and rolls
    /// across several packets, so the encode writes the frame across
    /// datagrams, the receive side stitches it back, and a message after it
    /// is still found and delivered.
    #[test]
    fn a_real_udp_frame_round_trips_a_spanning_special_form_bundle() {
        let (recorder, records) = Recorder::shared();
        let recorder: Rc<dyn InputMessageHandler> = recorder;
        let table = InterfaceTable::new();
        let wide = InterfaceElement::new("wide", 3, VARIABLE_LENGTH_MESSAGE, 1, None);
        let ping = InterfaceElement::new("ping", 1, FIXED_LENGTH_MESSAGE, 0, None);
        table.serve(&wide, Some(Rc::clone(&recorder)));
        table.serve(&ping, Some(Rc::clone(&recorder)));

        let body: Vec<u8> = (0..4000u32).map(|i| (i % 251) as u8).collect();

        let mut bundle = UDPBundle::new(0);
        bundle.start_message(&wide, ReliableType::Driver);
        bundle.add_blob(&body);
        bundle.start_message(&ping, ReliableType::No);
        bundle.finalise();

        let mut allocator = SeqNumAllocator::new(0);
        assert!(bundle.prepare_packets(None, &mut allocator, true).is_none());
        let num_packets = bundle.num_data_units();
        assert!(
            num_packets > 1,
            "4000 body bytes must roll across packets, got {num_packets}"
        );

        let (sender, listener, dest) = loopback();
        let head = send_and_receive(&sender, &listener, dest, bundle.first_packet().unwrap());

        {
            let received = head.borrow();
            assert!(received.has_flags(f::IS_FRAGMENT));
            assert!(received.has_flags(f::HAS_SEQUENCE_NUMBER));
            assert!(received.has_flags(f::HAS_CHECKSUM));
            assert_eq!(received.frag_begin(), 0);
            assert_eq!(
                received.frag_end(),
                num_packets as u32 - 1,
                "the fragment range covers every packet"
            );
            assert_eq!(received.chain_length(), num_packets);
        }

        let processor = UDPBundleProcessor::new(Some(head), false);
        let ret = processor.dispatch_messages(&table, &dest, None);
        assert_eq!(ret, Reason::Success);

        let records = records.borrow();
        assert_eq!(records.len(), 2, "the spanning message and the one after");
        assert_eq!(records[0].0, 3);
        assert_eq!(
            records[0].1, 4000,
            "the true length survives the special form"
        );
        assert_eq!(records[0].3, body, "the body is stitched byte for byte");
        assert_eq!(records[1], (1u8, 0, REPLY_ID_NONE, Vec::new()));
    }

    /// A request's preamble carries the reply id the request manager hands
    /// out, and the receive side reads it back. The request itself rides a
    /// real UDP frame like any other message.
    #[test]
    fn a_request_round_trips_with_its_reply_id() {
        let (recorder, records) = Recorder::shared();
        let recorder: Rc<dyn InputMessageHandler> = recorder;
        let table = InterfaceTable::new();
        let login = InterfaceElement::new("login", 5, VARIABLE_LENGTH_MESSAGE, 2, None);
        table.serve(&login, Some(Rc::clone(&recorder)));

        let dispatcher = Rc::new(EventDispatcher::new());
        let manager: Rc<RequestManager> = Rc::new(RequestManager::new(false, dispatcher));

        let mut bundle = UDPBundle::new(0);
        bundle.start_request(
            &login,
            Rc::new(NoReply),
            0xdead,
            1_000_000,
            ReliableType::Driver,
        );
        bundle.add_blob(&[0x51, 0x52, 0x53, 0x54]);
        bundle.finalise();
        bundle.add_reply_orders_to(&manager, None);

        let mut allocator = SeqNumAllocator::new(0);
        assert!(bundle.prepare_packets(None, &mut allocator, true).is_none());
        assert_eq!(bundle.num_data_units(), 1);

        let (sender, listener, dest) = loopback();
        let head = send_and_receive(&sender, &listener, dest, bundle.first_packet().unwrap());

        let processor = UDPBundleProcessor::new(Some(head), false);
        let ret = processor.dispatch_messages(&table, &dest, None);
        assert_eq!(ret, Reason::Success);

        let records = records.borrow();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, 5);
        assert_eq!(records[0].1, 4, "the body length, preamble excluded");
        assert_eq!(records[0].3, vec![0x51, 0x52, 0x53, 0x54]);
        assert!(
            records[0].2 != REPLY_ID_NONE,
            "the reply id is a real allocated id, not the sentinel"
        );
    }

    /// A reply carries the reply id of the request it answers. The receive
    /// side serves the reserved reply id like any other message.
    #[test]
    fn a_reply_round_trips_with_its_reply_id() {
        let (recorder, records) = Recorder::shared();
        let recorder: Rc<dyn InputMessageHandler> = recorder;
        let table = InterfaceTable::new();
        let reply = InterfaceElement::reply();
        table.serve(&reply, Some(Rc::clone(&recorder)));

        let mut bundle = UDPBundle::new(0);
        bundle.start_reply(77, ReliableType::No);
        bundle.add_blob(&[0x01, 0x02, 0x03, 0x04]);
        bundle.finalise();

        let mut allocator = SeqNumAllocator::new(0);
        assert!(bundle.prepare_packets(None, &mut allocator, true).is_none());

        let (sender, listener, dest) = loopback();
        let head = send_and_receive(&sender, &listener, dest, bundle.first_packet().unwrap());

        let processor = UDPBundleProcessor::new(Some(head), false);
        let ret = processor.dispatch_messages(&table, &dest, None);
        assert_eq!(ret, Reason::Success);

        let records = records.borrow();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, REPLY_MESSAGE_IDENTIFIER);
        assert_eq!(records[0].1, 8, "4 id bytes plus the 4-byte body");
        assert_eq!(
            records[0].3,
            vec![77, 0, 0, 0, 0x01, 0x02, 0x03, 0x04],
            "the reply id leads the body, as startReply streams it"
        );
    }
}
