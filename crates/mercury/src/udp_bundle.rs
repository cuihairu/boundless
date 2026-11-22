//! `Mercury::Bundle` / `Mercury::UDPBundle` (`lib/network/bundle.cpp`,
//! `lib/network/udp_bundle.cpp`): the encode half of the BW-02 codec.
//!
//! A bundle is a chain of packets holding a sequence of messages. Message
//! bodies are written incrementally; [`qreserve`](UDPBundle::qreserve) rolls
//! to a fresh packet whenever the current one runs out, which is how a
//! message comes to span packets (`FLAG_IS_FRAGMENT`).
//!
//! Scope of BW-02-03-01: the off-channel path — no channel, so no
//! reliable-order bookkeeping (`reliableOrders_` is only fed when the
//! bundle rides an external channel) and no piggybacks. The channel
//! branches of `preparePackets` (`pChannel->writeFlags`/`writeFooter`/
//! `useNextSequenceID`/`addResendTimer`) are the BW-02-03-02/03/04 seams.

use std::cell::RefCell;
use std::rc::Rc;

use crate::interface_element::InterfaceElement;
use crate::interfaces::ReplyMessageHandler;
use crate::message_framing::{NEXT_REQUEST_OFFSET_SIZE, REPLY_ID_SIZE, SPECIAL_LENGTH_SIZE};
use crate::misc::{SEQ_NULL, SeqNum, SeqNumAllocator};
use crate::network_interface::ChannelRef;
use crate::packet::{HEADER_SIZE, Packet, flags};
use crate::request_manager::{ReplyOrder, RequestManager};

/// `ReliableTypeEnum` (bundle.hpp): the three reliability levels a message
/// can ask for. `RELIABLE_PASSENGER` messages only ride along when the
/// bundle has at least one driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReliableType {
    /// Not reliable.
    No,
    /// Reliable, and makes the bundle reliable on its own.
    Driver,
    /// Reliable, but only sent reliably when a driver is present.
    Passenger,
    /// Like a driver, but also marks the bundle critical.
    Critical,
}

impl ReliableType {
    /// `isReliable`.
    pub fn is_reliable(self) -> bool {
        self != ReliableType::No
    }

    /// `isDriver`: only a driver or a critical message drives reliability
    /// (the reference tests the enum's low bit).
    pub fn is_driver(self) -> bool {
        matches!(self, ReliableType::Driver | ReliableType::Critical)
    }
}

/// One request awaiting its reply, as `replyOrders_` holds it.
///
/// The reference's `ReplyOrder` keeps a `void*` into the packet data for
/// the reply id; the packet never moves while the bundle lives, so this
/// port keeps the same fact as the packet and the byte offset of the slot.
struct ReplyOrderRecord {
    handler: Rc<dyn ReplyMessageHandler>,
    arg: usize,
    microseconds: i32,
    /// The packet whose data holds the reply id slot.
    packet: Rc<RefCell<Packet>>,
    /// The data offset of the four reply id bytes.
    slot_offset: usize,
}

/// `UDPBundle`: messages written into a chain of packets.
pub struct UDPBundle {
    /// `pFirstPacket_`: the chain head.
    first_packet: Option<Rc<RefCell<Packet>>>,
    /// `pCurrentPacket_`: the packet being written.
    current_packet: Option<Rc<RefCell<Packet>>>,
    /// `hasEndedMsgEarly_`.
    has_ended_msg_early: bool,
    /// `reliableDriver_`: any driving reliable messages added.
    reliable_driver: bool,
    /// `extraSize_`: spare bytes each packet keeps for e.g. a filter.
    extra_size: u8,
    /// `isCritical_`.
    is_critical: bool,
    /// `ack_`: the off-channel ack (at most one per bundle).
    ack: SeqNum,
    /// `curIE_`: the element of the message being written.
    cur_ie: Option<InterfaceElement>,
    /// `msgLen_`: body bytes of the current message so far.
    msg_len: i32,
    /// `msgExtra_`: the request preamble size.
    msg_extra: usize,
    /// `msgBeg_`: where the current message's header starts.
    msg_beg: Option<(Rc<RefCell<Packet>>, usize)>,
    /// `msgChunkOffset_`: where the current message's chunk in this packet
    /// starts.
    msg_chunk_offset: usize,
    /// `msgIsReliable_`.
    msg_is_reliable: bool,
    /// `msgIsRequest_`.
    msg_is_request: bool,
    /// `numMessages_`.
    num_messages: usize,
    /// `numReliableMessages_`.
    num_reliable_messages: usize,
    /// `isFinalised_`.
    is_finalised: bool,
    /// `replyOrders_`.
    reply_orders: Vec<ReplyOrderRecord>,
}

impl UDPBundle {
    /// `UDPBundle( spareSize )`: an empty bundle for writing.
    pub fn new(spare_size: u8) -> Self {
        let mut bundle = Self {
            first_packet: None,
            current_packet: None,
            has_ended_msg_early: false,
            reliable_driver: false,
            extra_size: spare_size,
            is_critical: false,
            ack: SEQ_NULL,
            cur_ie: None,
            msg_len: 0,
            msg_extra: 0,
            msg_beg: None,
            msg_chunk_offset: 0,
            msg_is_reliable: false,
            msg_is_request: false,
            num_messages: 0,
            num_reliable_messages: 0,
            is_finalised: false,
            reply_orders: Vec::new(),
        };
        bundle.clear(true);
        bundle
    }

    /// `clear( newBundle )`: flush everything and ready the bundle for new
    /// messages.
    pub fn clear(&mut self, new_bundle: bool) {
        // Bundle::clear
        self.is_finalised = false;
        self.num_messages = 0;
        self.reply_orders.clear();

        // If this isn't the first time, flush everything.
        if !new_bundle {
            self.dispose();
            self.has_ended_msg_early = false;
        }

        self.reliable_driver = false;
        self.is_critical = false;
        self.cur_ie = None;
        self.msg_len = 0;
        self.msg_beg = None;
        self.msg_chunk_offset = 0;
        self.msg_is_reliable = false;
        self.msg_is_request = false;
        self.num_reliable_messages = 0;
        self.ack = SEQ_NULL;

        // A fresh bundle starts with one empty packet.
        if self.first_packet.is_none() {
            let packet = Rc::new(RefCell::new(Packet::new()));
            self.first_packet = Some(Rc::clone(&packet));
            self.start_packet(packet);
        }
    }

    /// `dispose`: release the packets.
    fn dispose(&mut self) {
        self.first_packet = None;
        self.current_packet = None;
    }

    // ------------------------------------------------------------------
    // Chain walking
    // ------------------------------------------------------------------

    /// `pFirstPacket()`.
    pub fn first_packet(&self) -> Option<Rc<RefCell<Packet>>> {
        self.first_packet.clone()
    }

    /// The packet being written.
    fn current(&self) -> Rc<RefCell<Packet>> {
        Rc::clone(self.current_packet.as_ref().expect("bundle has no packet"))
    }

    /// `numDataUnits()`: how many packets the bundle holds.
    pub fn num_data_units(&self) -> usize {
        self.first_packet
            .as_ref()
            .expect("bundle has no packet")
            .borrow()
            .chain_length()
    }

    /// `hasMultipleDataUnits()`.
    pub fn has_multiple_data_units(&self) -> bool {
        self.num_data_units() > 1
    }

    /// `size()`: the accumulated size of the bundle, headers and footers
    /// included.
    pub fn size(&self) -> usize {
        let mut total = 0;
        let mut node = self.first_packet.clone();
        while let Some(packet) = node {
            total += packet.borrow().total_size();
            node = packet.borrow().next();
        }
        total
    }

    /// `freeBytesInLastDataUnit()`.
    pub fn free_bytes_in_last_data_unit(&self) -> isize {
        self.current().borrow().free_space()
    }

    /// `hasDataFooters()`.
    pub fn has_data_footers(&self) -> bool {
        let mut node = self.first_packet.clone();
        while let Some(packet) = node {
            let has = packet
                .borrow()
                .has_flags(flags::HAS_ACKS | flags::HAS_PIGGYBACKS);
            if has {
                return true;
            }
            node = packet.borrow().next();
        }
        false
    }

    /// `isEmpty()`.
    pub fn is_empty(&self) -> bool {
        // isReliable() says whether a sequence number will land on this
        // bundle during send.
        let has_data = self.num_messages > 0
            || self.has_multiple_data_units()
            || self.is_reliable()
            || self.has_data_footers()
            || self.ack != SEQ_NULL;
        !has_data
    }

    // ------------------------------------------------------------------
    // Reliability and acks
    // ------------------------------------------------------------------

    /// `isReliable()`: the bundle has a driving reliable message.
    pub fn is_reliable(&self) -> bool {
        self.reliable_driver
    }

    /// `isCritical()`.
    pub fn is_critical(&self) -> bool {
        self.is_critical
    }

    /// `reliable( currentMsgReliable )`: set the current message's policy
    /// without starting a new message.
    pub fn reliable(&mut self, current_msg_reliable: ReliableType) {
        self.msg_is_reliable = current_msg_reliable.is_reliable();
        self.reliable_driver |= current_msg_reliable.is_driver();
    }

    /// `addAck( seq )`: attach an off-channel ack.
    pub fn add_ack(&mut self, seq: SeqNum) {
        debug_assert_eq!(self.ack, SEQ_NULL, "a bundle carries at most one ack");
        self.ack = seq;
    }

    /// `getAck()`.
    pub fn get_ack(&self) -> SeqNum {
        debug_assert_ne!(self.ack, SEQ_NULL, "getAck without addAck");
        self.ack
    }

    /// `numReliableMessages()`.
    pub fn num_reliable_messages(&self) -> usize {
        self.num_reliable_messages
    }

    // ------------------------------------------------------------------
    // Writing messages
    // ------------------------------------------------------------------

    /// `startMessage( ie, reliable )`.
    pub fn start_message(&mut self, ie: &InterfaceElement, reliable: ReliableType) {
        // Piggybacks should only be added immediately before sending.
        debug_assert!(
            !self.current().borrow().has_flags(flags::HAS_PIGGYBACKS),
            "piggybacks are only added immediately before sending"
        );

        self.end_message();
        self.cur_ie = Some(ie.clone());
        self.msg_is_reliable = reliable.is_reliable();
        self.msg_is_request = false;
        self.is_critical = reliable == ReliableType::Critical;
        self.new_message(0);

        self.reliable_driver |= reliable.is_driver();
    }

    /// `startRequest( ie, handler, arg, timeout, reliable )`.
    pub fn start_request(
        &mut self,
        ie: &InterfaceElement,
        handler: Rc<dyn ReplyMessageHandler>,
        arg: usize,
        timeout: i32,
        reliable: ReliableType,
    ) {
        self.end_message();
        self.cur_ie = Some(ie.clone());
        self.msg_is_reliable = reliable.is_reliable();
        self.msg_is_request = true;
        self.is_critical = reliable == ReliableType::Critical;

        // Start a new message, and reserve extra space for the reply ID and
        // the next request offset.  The reply ID is actually written in
        // NetworkInterface::send().
        let (slot_packet, slot_offset) = self.new_message(REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE);

        let packet = self.current();
        let header_len = ie.header_size().max(0) as usize;
        let message_start = (packet.borrow().msg_end_offset()
            - (header_len + REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE))
            as u16;
        let next_request_link = packet.borrow().msg_end_offset() - NEXT_REQUEST_OFFSET_SIZE;

        // Update the request tracking stuff on the current packet.
        packet
            .borrow_mut()
            .add_request(message_start, next_request_link);

        // now make and add a reply order
        self.reply_orders.push(ReplyOrderRecord {
            handler,
            arg,
            microseconds: timeout,
            packet: slot_packet,
            slot_offset,
        });

        // this packet has requests
        packet.borrow_mut().enable_flags(flags::HAS_REQUESTS);

        self.reliable_driver |= reliable.is_driver();
    }

    /// `startReply( id, reliable )`. All replies are 4-byte variable size.
    pub fn start_reply(&mut self, id: crate::misc::ReplyID, reliable: ReliableType) {
        self.end_message();
        self.cur_ie = Some(InterfaceElement::reply());
        self.msg_is_reliable = reliable.is_reliable();
        self.msg_is_request = false;
        self.is_critical = reliable == ReliableType::Critical;
        self.new_message(0);

        self.reliable_driver |= reliable.is_driver();

        // stream on the id (counts as part of the length)
        self.add_blob(&id.to_le_bytes());
    }

    /// `newMessage( extra )`: begin a message, write its id, and return
    /// where the `extra` request bytes were reserved.
    fn new_message(&mut self, extra: usize) -> (Rc<RefCell<Packet>>, usize) {
        let ie = self
            .cur_ie
            .clone()
            .expect("newMessage before startMessage/startRequest/startReply");

        // figure the length of the header. The reference CRITICAL_MSGs on
        // an unknown length format; the port asserts and carries on with a
        // one-byte header, matching how the other CRITICAL paths here stay
        // standing.
        let header_len = ie.header_size();
        debug_assert!(header_len >= 0, "unknown length format for {}", ie.name());
        let header_len = header_len.max(0) as usize;

        self.num_messages += 1;

        if self.msg_is_reliable {
            self.num_reliable_messages += 1;
        }

        // make space for the header
        let (packet, pos) = self.qreserve(header_len + extra);

        // set the start of this msg
        self.msg_beg = Some((Rc::clone(&packet), pos));
        self.msg_chunk_offset = packet.borrow().msg_end_offset();

        // write in the identifier
        packet.borrow_mut().byte_range_mut(pos, 1)[0] = ie.id();

        // set the length to zero
        self.msg_len = 0;
        self.msg_extra = extra;

        // and return where the extra data went
        (packet, pos + header_len)
    }

    /// `endMessage( isEarlyCall )`: finalise the message being written.
    pub fn end_message(&mut self) {
        self.end_message_inner(false);
    }

    fn end_message_inner(&mut self, is_early_call: bool) {
        // nothing to do if no message yet
        let Some((beg_packet, beg_offset)) = self.msg_beg.clone() else {
            return;
        };

        // add the amt used in this packet to the length
        let packet = self.current();
        self.msg_len += (packet.borrow().msg_end_offset() - self.msg_chunk_offset) as i32;

        // fill in headers for this msg
        self.compress_length_into_bundle(&beg_packet, beg_offset);

        // record its details if it was reliable
        if self.msg_is_reliable {
            // Reliable orders only exist on an external channel; the
            // off-channel port records nothing here.
            self.msg_is_reliable = false; // for sanity
        }

        self.msg_chunk_offset = packet.borrow().msg_end_offset();

        self.msg_beg = None;
        self.msg_is_request = false;

        self.has_ended_msg_early = is_early_call;
    }

    /// `curIE_.compressLength( msgBeg_, msgLen_, this, msgIsRequest_ )`,
    /// spelled out over the packet chain: the normal form writes the length
    /// field inside the message header (always within the first packet of
    /// the message), the special form marks the field all ones, moves the
    /// first four body bytes to the end of the message, and writes the true
    /// length where they were.
    fn compress_length_into_bundle(&mut self, beg_packet: &Rc<RefCell<Packet>>, beg_offset: usize) {
        let ie = self.cur_ie.clone().expect("compressLength with no element");
        let is_request = self.msg_is_request;
        let msg_len = self.msg_len;

        if ie.can_handle_length_for(msg_len) {
            // Normal form. Only the length field is touched.
            let header_len = ie.header_size().max(0) as usize;
            let mut first = beg_packet.borrow_mut();
            let window = first.byte_range_mut(beg_offset, header_len);
            // BufferTooSmall cannot fire: the field is inside the packet.
            let _ = ie.compress_length(window, msg_len, is_request);
            return;
        }

        // Special form (a width of one to three, wide body). The reference
        // reserves four bytes on the bundle first, then walks head and
        // tail byte by byte; head and tail never overlap, so whole-word
        // moves are the same result (BW-02-02-02).
        let width = ie.length_param().max(0) as usize;
        let body_offset = ie.body_offset(is_request);

        // Capture the body length before the reserve below; a roll here
        // re-adds this packet's chunk to msg_len (the reference does the
        // same and the next newMessage resets it), but the head below is
        // written from this copy.
        let length = msg_len;

        // Reserve space for the four displaced bytes at the end of the
        // bundle.
        let (tail_packet, tail_offset) = self.qreserve(SPECIAL_LENGTH_SIZE);

        // Mark the length field all ones, starting after the id byte.
        {
            let mut first = beg_packet.borrow_mut();
            for byte in first.byte_range_mut(beg_offset + 1, width) {
                *byte = 0xff;
            }
        }

        // Move the first four body bytes to the tail, and write the true
        // length where they were.
        let mut displaced = [0u8; SPECIAL_LENGTH_SIZE];
        Self::chain_read(beg_packet, beg_offset + body_offset, &mut displaced);
        Self::chain_write(beg_packet, beg_offset + body_offset, &length.to_le_bytes());
        Self::chain_write(&tail_packet, tail_offset, &displaced);
    }

    /// Read `out.len()` bytes starting at `(node, offset)`, walking the
    /// chain across packet boundaries (a fresh packet's body starts at
    /// `HEADER_SIZE`).
    fn chain_read(node: &Rc<RefCell<Packet>>, start: usize, out: &mut [u8]) {
        let mut node = Rc::clone(node);
        let mut offset = start;
        let mut done = 0;
        while done < out.len() {
            let take = {
                let packet = node.borrow();
                (packet.msg_end_offset() - offset).min(out.len() - done)
            };
            {
                let packet = node.borrow();
                out[done..done + take].copy_from_slice(packet.byte_range(offset, take));
            }
            done += take;
            if done == out.len() {
                break;
            }
            let next = node
                .borrow()
                .next()
                .expect("message runs off the packet chain");
            node = next;
            offset = HEADER_SIZE;
        }
    }

    /// Write `bytes` starting at `(node, offset)`, walking the chain.
    fn chain_write(node: &Rc<RefCell<Packet>>, start: usize, bytes: &[u8]) {
        let mut node = Rc::clone(node);
        let mut offset = start;
        let mut done = 0;
        while done < bytes.len() {
            let take = {
                let packet = node.borrow();
                (packet.msg_end_offset() - offset).min(bytes.len() - done)
            };
            {
                let mut packet = node.borrow_mut();
                packet
                    .byte_range_mut(offset, take)
                    .copy_from_slice(&bytes[done..done + take]);
            }
            done += take;
            if done == bytes.len() {
                break;
            }
            let next = node
                .borrow()
                .next()
                .expect("message runs off the packet chain");
            node = next;
            offset = HEADER_SIZE;
        }
    }

    // ------------------------------------------------------------------
    // Reserving space
    // ------------------------------------------------------------------

    /// `qreserve( nBytes )`: reserve bytes on the current packet, rolling
    /// to a new one when it is full. Returns where the bytes went.
    pub fn qreserve(&mut self, n_bytes: usize) -> (Rc<RefCell<Packet>>, usize) {
        let packet = self.current();
        if (n_bytes as isize) <= packet.borrow().free_space() {
            let pos = packet.borrow().back();
            packet.borrow_mut().grow(n_bytes);
            (packet, pos)
        } else {
            self.sreserve(n_bytes)
        }
    }

    /// `sreserve( nBytes )`: the bytes will not fit, so add a new packet.
    fn sreserve(&mut self, n_bytes: usize) -> (Rc<RefCell<Packet>>, usize) {
        self.end_packet(true);
        let fresh = Rc::new(RefCell::new(Packet::new()));
        self.start_packet(Rc::clone(&fresh));

        let pos = fresh.borrow().back();
        fresh.borrow_mut().grow(n_bytes);

        debug_assert!(fresh.borrow().free_space() >= 0);
        (fresh, pos)
    }

    /// `addBlob( pBlob, size )`: append a block, splitting over packets as
    /// needed. The port's stand-in for the reference's `reserve() ->
    /// void*` write cursor, which has no safe borrow here.
    pub fn add_blob(&mut self, bytes: &[u8]) {
        let mut cursor = 0;
        while cursor < bytes.len() {
            // If there isn't any space on this packet, force a new one.
            if self.current().borrow().free_space() == 0 {
                self.sreserve(0);
            }

            let room = self.current().borrow().free_space().max(0) as usize;
            let chunk = room.min(bytes.len() - cursor);
            debug_assert!(chunk > 0);

            let (packet, pos) = self.qreserve(chunk);
            packet
                .borrow_mut()
                .byte_range_mut(pos, chunk)
                .copy_from_slice(&bytes[cursor..cursor + chunk]);
            cursor += chunk;
        }
    }

    // ------------------------------------------------------------------
    // Packets
    // ------------------------------------------------------------------

    /// `startPacket( p )`: begin a new packet in the chain.
    fn start_packet(&mut self, p: Rc<RefCell<Packet>>) {
        // Link the new packet into the chain if necessary.
        if let Some(prev) = self.current_packet.take() {
            prev.borrow_mut().chain(Rc::clone(&p));
        }
        self.current_packet = Some(Rc::clone(&p));

        p.borrow_mut()
            .reserve_filter_space(self.extra_size as usize);
        p.borrow_mut().set_flags(0);
        p.borrow_mut().set_msg_end_offset(HEADER_SIZE);

        // if we're in the middle of a message start the next chunk here
        self.msg_chunk_offset = p.borrow().msg_end_offset();
    }

    /// `endPacket( isExtending )`.
    fn end_packet(&mut self, is_extending: bool) {
        // If this won't be the last packet, add a reliable order marker.
        // Reliable orders only exist on an external channel, which the
        // port does not ride yet (BW-02-03-02).

        // if we're in the middle of a message add this chunk
        let packet = self.current();
        self.msg_len += (packet.borrow().msg_end_offset() - self.msg_chunk_offset) as i32;
        self.msg_chunk_offset = packet.borrow().msg_end_offset();
        let _ = is_extending;
    }

    // ------------------------------------------------------------------
    // Finalising
    // ------------------------------------------------------------------

    /// `doFinalise()`.
    pub fn do_finalise(&mut self) {
        // make sure we're not sending a packet where the message wasn't
        // properly started...
        let packet = self.current();
        debug_assert!(
            self.msg_beg.is_some() || packet.borrow().msg_end_offset() == self.msg_chunk_offset,
            "data not part of message found at end of bundle!"
        );

        self.end_message();
        self.end_packet(false);

        // If we don't have a reliable driver then any reliable orders
        // present are all passengers (hangers on), so get rid of them.
        // Reliable orders only exist on an external channel; nothing to
        // clear here yet.
    }

    /// `Bundle::finalise()`.
    pub fn finalise(&mut self) {
        if self.is_finalised {
            return;
        }
        self.do_finalise();
        self.is_finalised = true;
    }

    /// `isFinalised()`.
    pub fn is_finalised(&self) -> bool {
        self.is_finalised
    }

    // ------------------------------------------------------------------
    // Requests
    // ------------------------------------------------------------------

    /// `Bundle::addReplyOrdersTo`: register every request with the request
    /// manager, writing each reply id into its packet slot.
    pub fn add_reply_orders_to(
        &mut self,
        request_manager: &Rc<RequestManager>,
        channel: Option<Rc<dyn ChannelRef>>,
    ) {
        debug_assert!(self.is_finalised, "finalise before addReplyOrdersTo");

        for order in &self.reply_orders {
            let mut packet = order.packet.borrow_mut();
            let slot = packet.byte_range_mut(order.slot_offset, REPLY_ID_SIZE);
            let reply_order = ReplyOrder {
                handler: Rc::clone(&order.handler),
                arg: order.arg,
                microseconds: order.microseconds,
                p_reply_id: slot,
            };
            request_manager.add_reply_order(reply_order, channel.clone());
        }
    }

    /// `Bundle::cancelRequests`: fail every request made on this bundle.
    pub fn cancel_requests(
        &mut self,
        request_manager: Option<&Rc<RequestManager>>,
        reason: crate::misc::Reason,
    ) {
        let Some(request_manager) = request_manager else {
            return;
        };

        for order in &self.reply_orders {
            request_manager.cancel_requests_for_handler(&order.handler, reason);
        }

        self.reply_orders.clear();
    }

    // ------------------------------------------------------------------
    // Preparing for the wire
    // ------------------------------------------------------------------

    /// `writeFlags( p )`: set up a packet's flags from the bundle's
    /// information, reserving the footers those flags need.
    fn write_flags(&self, p: &Rc<RefCell<Packet>>) {
        // msgIsReliable_ is only set here if there are no msgs (only
        // footers) on the bundle, but the setter wants to indicate that it
        // should still be reliable. reliableOrders_ is the external-channel
        // store (BW-02-03-02).
        if self.msg_is_reliable || self.num_reliable_messages > 0 {
            p.borrow_mut().enable_flags(flags::IS_RELIABLE);
        }

        if p.borrow().has_flags(flags::HAS_REQUESTS) {
            p.borrow_mut().reserve_footer(NEXT_REQUEST_OFFSET_SIZE);
        }

        if self.has_multiple_data_units() {
            p.borrow_mut().enable_flags(flags::IS_FRAGMENT);
            p.borrow_mut().reserve_footer(8); // SeqNum * 2
        }

        if self.ack != SEQ_NULL {
            p.borrow_mut().enable_flags(flags::HAS_ACKS);
            p.borrow_mut().reserve_footer(1 + 4); // AckCount + SeqNum
        }
    }

    /// `preparePackets`: fill in all the footers that are left to the
    /// bundle and stamp sequence numbers. Off-channel form: the channel
    /// branches (`pChannel->writeFlags`/`writeFooter`, channel sequence
    /// numbers and resend timers) are BW-02-03-02/03/04 seams.
    ///
    /// Returns the first packet that could not get a resend timer — always
    /// `None` off-channel, where there is no send window to overflow.
    pub fn prepare_packets(
        &mut self,
        seq_num_allocator: &mut SeqNumAllocator,
        should_use_checksums: bool,
    ) -> Option<Rc<RefCell<Packet>>> {
        let mut first_overflow_packet = None;

        let num_packets = self.num_data_units();
        let mut first_seq = 0;
        let mut last_seq = 0;

        // Write footers for each packet.
        let mut node = self.first_packet.clone();
        while let Some(p) = node {
            debug_assert!(p.borrow().msg_end_offset() >= HEADER_SIZE);

            if should_use_checksums {
                // Reserve space for the checksum footer
                debug_assert!(!p.borrow().has_flags(flags::HAS_CHECKSUM));
                p.borrow_mut().reserve_footer(4);
                p.borrow_mut().enable_flags(flags::HAS_CHECKSUM);
            }

            self.write_flags(&p);

            if p.borrow().has_flags(flags::IS_RELIABLE) || p.borrow().has_flags(flags::IS_FRAGMENT)
            {
                // (the reference also does this for an external channel)
                p.borrow_mut().reserve_footer(4);
                p.borrow_mut().enable_flags(flags::HAS_SEQUENCE_NUMBER);
            }

            // At this point, pPacket->back() is positioned just after the
            // message data, so we advance it to the end of where the
            // footers end, then write backwards towards the message data.
            // We check that we finish up back at the message data as a
            // sanity check.
            let msg_end_offset = p.borrow().msg_end_offset();
            let footer_size = p.borrow().footer_size();
            p.borrow_mut().grow(footer_size);

            // Pack in a zero checksum.  We'll calculate it later.
            let mut checksum_at = None;
            if p.borrow().has_flags(flags::HAS_CHECKSUM) {
                p.borrow_mut().pack_footer_u32(0);
                checksum_at = Some(p.borrow().msg_end_offset());
            }

            // Piggybacks go first so that they can be stripped and the
            // rest of the packet can be dealt with as normal (BW-02-03-04).

            // Off-channel acking: we only ever have one ack.
            if p.borrow().has_flags(flags::HAS_ACKS) {
                p.borrow_mut().pack_footer_u8(1);
                let ack = self.get_ack();
                p.borrow_mut().pack_footer_u32(ack);
            }

            // Add the sequence number. Off-channel, the nub's allocator
            // numbers every packet that asks for one.
            if p.borrow().has_flags(flags::HAS_SEQUENCE_NUMBER) {
                let seq = seq_num_allocator.get_next();
                p.borrow_mut().set_seq(seq);
                p.borrow_mut().pack_footer_u32(seq);

                let is_first = self
                    .first_packet
                    .as_ref()
                    .is_some_and(|first| Rc::ptr_eq(first, &p));
                if is_first {
                    first_seq = seq;
                    last_seq = seq + num_packets as u32 - 1;
                }
            }

            // Add the first request offset.
            if p.borrow().has_flags(flags::HAS_REQUESTS) {
                let offset = p.borrow().first_request_offset();
                p.borrow_mut().pack_footer_u16(offset);
            }

            // add the fragment info
            if p.borrow().has_flags(flags::IS_FRAGMENT) {
                p.borrow_mut().pack_footer_u32(last_seq);
                p.borrow_mut().pack_footer_u32(first_seq);
            }

            // Make sure writing all the footers brought us back to the end
            // of the message data.
            assert_eq!(
                p.borrow().msg_end_offset(),
                msg_end_offset,
                "footers did not come back to the message data"
            );

            if let Some(at) = checksum_at {
                p.borrow_mut().write_checksum(at);
            }

            // set up the reliable machinery — the channel's resend window
            // (BW-02-03-02/03/04); off-channel there is none.

            node = p.borrow().next();
        }

        let _ = &mut first_overflow_packet;
        first_overflow_packet
    }
}

impl Default for UDPBundle {
    fn default() -> Self {
        Self::new(0)
    }
}
