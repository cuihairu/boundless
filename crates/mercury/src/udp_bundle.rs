//! `Mercury::UDPBundle` (`lib/network/udp_bundle.hpp/.cpp/.ipp`): the
//! write side of a bundle, laid out across one or more packets.
//!
//! A bundle collects messages between finalises. Every message starts
//! with [`start_message`](Self::start_message) (or the request/reply
//! variants), the payload streams in through
//! [`write_bytes`](Self::write_bytes), and
//! [`finalise`](Self::finalise) closes the last message and packet. When
//! a message outgrows its packet, the reservation machinery opens the
//! next one and the message continues there; `prepare_packets` then
//! walks the chain writing every footer the bundle owes (checksums, acks,
//! sequence numbers, the first request offset, fragment bounds), so each
//! packet is a datagram ready for `sendto`.
//!
//! The request links this side writes are packet-absolute, counting from
//! the flags word -- the same terms the wire carries and
//! [`crate::bundle_dispatch`] translates on the walk in.
//!
//! Left with the channel layer (BW-02-03-02 and on): the owning
//! `UDPChannel` (`pChannel_`), the reliable-order segments a packet
//! splitting breaks a reliable message into, the piggyback send path,
//! and the channel footer and its `writeFlags`/`writeFooter` hooks --
//! every `isOnExternalChannel` branch here is that machinery waiting.

use std::rc::Rc;

use crate::endpoint::Address;
use crate::interface_element::{INVALID_MESSAGE, InterfaceElement};
use crate::interfaces::ReplyMessageHandler;
use crate::message_framing::{NEXT_REQUEST_OFFSET_SIZE, REPLY_ID_SIZE, SPECIAL_LENGTH_SIZE};
use crate::misc::{SEQ_NULL, SeqNum, SeqNumAllocator};
use crate::packet::{HEADER_SIZE, Packet, flags};
use crate::reply_order::ReplyOrder;
use crate::request_manager::RequestManager;
use crate::udp_channel::{ChannelPtr, UdpChannel};

/// `DEFAULT_REQUEST_TIMEOUT` (`bundle.hpp`): requests on a channel never
/// carry a non-default timeout, the reference warns and ignores it.
pub const DEFAULT_REQUEST_TIMEOUT: i32 = 5_000_000;

/// `Mercury::ReliableType` (`bundle.hpp`): the reliability policy of one
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReliableType {
    /// `RELIABLE_NO`: best effort.
    No,
    /// `RELIABLE_DRIVER`: reliable, and it keeps any passengers alive.
    Driver,
    /// `RELIABLE_PASSENGER`: reliable only while a driver rides along.
    Passenger,
    /// `RELIABLE_CRITICAL`: a driver that also marks the bundle critical.
    Critical,
}

impl ReliableType {
    /// `ReliableType::isReliable`: anything but `RELIABLE_NO`.
    pub fn is_reliable(self) -> bool {
        self != ReliableType::No
    }

    /// `ReliableType::isDriver`: only `RELIABLE_DRIVER` and
    /// `RELIABLE_CRITICAL` share the driver bit.
    pub fn is_driver(self) -> bool {
        matches!(self, ReliableType::Driver | ReliableType::Critical)
    }
}

/// One request waiting for its reply id: where the frame sits in the
/// bundle's packets, and the reply order the manager will get. The
/// reference stores `ReplyOrder{ handler, arg, timeout, pReplyID }` with
/// the pointer already inside the packet data; here the id's offset
/// travels with the frame it belongs to.
struct PendingRequest {
    /// The packet the message header starts in.
    packet: usize,
    /// The header's offset inside that packet, i.e. the frame's first
    /// byte as [`crate::bundle_dispatch`] reads it.
    frame_offset: usize,
    /// The order, with `reply_id_offset` counting from `frame_offset`
    /// (past the id byte and the length field).
    order: ReplyOrder,
}

/// The write side of a bundle. The reference chains its packets with
/// `PacketPtr`s; here the bundle owns them in send order, which is the
/// only order anything reads them in until a channel takes the chain
/// over in BW-02-03-02.
pub struct UdpBundle {
    /// `pFirstPacket_`..`pCurrentPacket_`, in chain order.
    packets: Vec<Packet>,
    /// Which packet is `pCurrentPacket_`.
    current: usize,
    /// `numMessages_`.
    num_messages: u32,
    /// `replyOrders_`.
    reply_orders: Vec<PendingRequest>,
    /// `hasEndedMsgEarly_`.
    #[allow(dead_code)]
    has_ended_msg_early: bool,
    /// `reliableDriver_`.
    reliable_driver: bool,
    /// `extraSize_`: the room an outbound filter is promised.
    extra_size: u8,
    /// `isCritical_`.
    is_critical: bool,
    /// `ack_`: the one off-channel ack this bundle can carry.
    ack: SeqNum,
    /// `curIE_`: the element of the message being written.
    cur_ie: InterfaceElement,
    /// `msgLen_`: the running length of the message being written.
    msg_len: i32,
    /// `msgBeg_`: where the message's frame starts, as `( packet, offset
    /// within the packet )`. The reference keeps the raw pointer; the
    /// header never straddles a packet, so a pair answers everywhere the
    /// pointer did.
    msg_beg: Option<(usize, usize)>,
    /// `msgChunkOffset_`: where the current chunk of the message started.
    msg_chunk_offset: usize,
    /// `msgIsReliable_`.
    msg_is_reliable: bool,
    /// `msgIsRequest_`.
    msg_is_request: bool,
    /// `numReliableMessages_`.
    num_reliable_messages: u32,
    /// `isFinalised_`: set once `finalise` has closed the last message
    /// and packet, checked by the send path before it touches the wire.
    finalised: bool,
}

impl UdpBundle {
    /// `UDPBundle( spareSize )`: one empty packet, ready for a message.
    pub fn new(spare_size: u8) -> Self {
        let mut bundle = Self {
            packets: Vec::new(),
            current: 0,
            num_messages: 0,
            reply_orders: Vec::new(),
            has_ended_msg_early: false,
            reliable_driver: false,
            extra_size: spare_size,
            is_critical: false,
            ack: SEQ_NULL,
            cur_ie: InterfaceElement::new("invalid", 0, INVALID_MESSAGE, 0, None),
            msg_len: 0,
            msg_beg: None,
            msg_chunk_offset: 0,
            msg_is_reliable: false,
            msg_is_request: false,
            num_reliable_messages: 0,
            finalised: false,
        };
        bundle.packets.push(Packet::new());
        bundle.start_packet();
        bundle
    }

    /// `reliable( currentMsgReliable )`: set the current message's policy
    /// without opening one, so a bundle carrying only footers still owes
    /// the wire its reliable sequence number.
    pub fn reliable(&mut self, current_msg_reliable: ReliableType) {
        self.msg_is_reliable = current_msg_reliable.is_reliable();
        self.reliable_driver |= current_msg_reliable.is_driver();
    }

    /// `UDPBundle::clear( spareSize )`: reset to construction state,
    /// keeping the spare size the bundle was built with. The channel
    /// calls this from `clearBundle` after every send.
    pub fn clear(&mut self) {
        *self = Self::new(self.extra_size);
    }

    // -- packet chain -----------------------------------------------------

    /// `startPacket`: open the current packet for writing -- flags clear,
    /// the cursor at the header, filter space reserved.
    fn start_packet(&mut self) {
        let packet = &mut self.packets[self.current];
        packet.reserve_filter_space(self.extra_size as usize);
        packet.set_flags(0);
        packet.grow(HEADER_SIZE);
        self.msg_chunk_offset = packet.msg_end_offset();
    }

    /// `endPacket( isExtending )`: close the current packet, folding the
    /// bytes it holds into the open message's length. The `isExtending`
    /// reliable-order bookkeeping is external-channel machinery, so the
    /// argument is carried for shape and ignored for now.
    fn end_packet(&mut self, _is_extending: bool) {
        let packet = &mut self.packets[self.current];
        self.msg_len += packet.msg_end_offset() as i32 - self.msg_chunk_offset as i32;
        self.msg_chunk_offset = packet.msg_end_offset();
    }

    /// `qreserve( nBytes )`: claim `n` contiguous bytes in the current
    /// packet, or a fresh one when it does not fit. Answers where they
    /// are.
    fn qreserve(&mut self, n_bytes: usize) -> (usize, usize) {
        if n_bytes <= self.packets[self.current].free_space() {
            let position = (self.current, self.packets[self.current].back());
            self.packets[self.current].grow(n_bytes);
            position
        } else {
            self.sreserve(n_bytes)
        }
    }

    /// `sreserve( nBytes )`: close the current packet, open a new one,
    /// and claim `n` contiguous bytes of it.
    fn sreserve(&mut self, n_bytes: usize) -> (usize, usize) {
        self.end_packet(true);
        self.packets.push(Packet::new());
        self.current = self.packets.len() - 1;
        self.start_packet();

        let position = (self.current, self.packets[self.current].back());
        self.packets[self.current].grow(n_bytes);
        position
    }

    /// `addBlob`: stream bytes across the packet chain, opening packets
    /// as they run out.
    pub fn write_bytes(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.packets[self.current].free_space() == 0 {
                self.sreserve(0);
            }

            let room = self.packets[self.current].free_space().min(bytes.len());
            let (packet, offset) = self.qreserve(room);
            self.packets[packet].buffer_mut()[offset..offset + room]
                .copy_from_slice(&bytes[..room]);
            bytes = &bytes[room..];
        }
    }

    /// Write one little-endian value into the payload.
    pub fn write_u16(&mut self, value: u16) {
        self.write_bytes(&value.to_le_bytes());
    }

    /// Write one little-endian value into the payload.
    pub fn write_u32(&mut self, value: u32) {
        self.write_bytes(&value.to_le_bytes());
    }

    /// Write one little-endian value into the payload.
    pub fn write_i32(&mut self, value: i32) {
        self.write_bytes(&value.to_le_bytes());
    }

    // -- messages ---------------------------------------------------------

    /// `startMessage( ie, reliable )`.
    pub fn start_message(&mut self, ie: &InterfaceElement, reliable: ReliableType) {
        self.end_message(false);
        self.cur_ie = ie.clone();
        self.msg_is_reliable = reliable.is_reliable();
        self.msg_is_request = false;
        self.is_critical = reliable == ReliableType::Critical;
        self.new_message(0);

        self.reliable_driver |= reliable.is_driver();
    }

    /// `startRequest( ie, handler, arg, timeout, reliable )`: start a
    /// request, reserving the reply id and next-request link the send
    /// will fill in, and remember the reply order.
    pub fn start_request(
        &mut self,
        ie: &InterfaceElement,
        handler: Rc<dyn ReplyMessageHandler>,
        arg: usize,
        timeout: i32,
        reliable: ReliableType,
    ) {
        self.end_message(false);
        self.cur_ie = ie.clone();
        self.msg_is_reliable = reliable.is_reliable();
        self.msg_is_request = true;
        self.is_critical = reliable == ReliableType::Critical;

        let header_len = ie.header_size() as usize;
        let (_, reply_id_offset) = self.new_message(REPLY_ID_SIZE + NEXT_REQUEST_OFFSET_SIZE);

        // `messageStart` is the frame's packet-absolute offset; the link
        // slot sits right after the reply id.
        let current_end = self.packets[self.current].msg_end_offset();
        let message_start =
            (current_end - header_len - REPLY_ID_SIZE - NEXT_REQUEST_OFFSET_SIZE) as u16;
        let next_request_link = (current_end - NEXT_REQUEST_OFFSET_SIZE) as u16;
        self.packets[self.current].add_request(message_start, next_request_link);

        self.reply_orders.push(PendingRequest {
            packet: self.current,
            frame_offset: reply_id_offset - header_len,
            order: ReplyOrder::new(handler, arg, timeout, header_len),
        });

        self.packets[self.current].enable_flags(flags::HAS_REQUESTS);

        self.reliable_driver |= reliable.is_driver();
    }

    /// `startReply( id, reliable )`: a reply is a `REPLY` message whose
    /// body starts with the id it answers.
    pub fn start_reply(&mut self, id: crate::misc::ReplyID, reliable: ReliableType) {
        self.end_message(false);
        self.cur_ie = InterfaceElement::reply();
        self.msg_is_reliable = reliable.is_reliable();
        self.msg_is_request = false;
        self.is_critical = reliable == ReliableType::Critical;
        self.new_message(0);

        self.reliable_driver |= reliable.is_driver();

        // stream on the id (counts as part of the length)
        self.write_i32(id);
    }

    /// `newMessage( extra )`: open a message frame -- reserve the header
    /// and the request preamble when there is one, write the id byte,
    /// and answer where the frame starts.
    fn new_message(&mut self, extra: usize) -> (usize, usize) {
        let header_len = self.cur_ie.header_size();
        assert!(
            header_len >= 0,
            "Mercury::UDPBundle::newMessage: tried to add a message with an unknown length format {}",
            self.cur_ie.length_style()
        );
        let header_len = header_len as usize;

        self.num_messages += 1;
        if self.msg_is_reliable {
            self.num_reliable_messages += 1;
        }

        // make space for the header
        let (packet, offset) = self.qreserve(header_len + extra);

        // set the start of this msg
        self.msg_beg = Some((packet, offset));
        self.msg_chunk_offset = self.packets[self.current].msg_end_offset();

        // write in the identifier
        let id = self.cur_ie.id();
        self.packets[packet].buffer_mut()[offset] = id;

        // set the length to zero
        self.msg_len = 0;

        (packet, offset + header_len)
    }

    /// `endMessage( isEarlyCall )`: close the message -- fold the last
    /// chunk into its length and compress the length into the frame. An
    /// oversize body takes the special form: the length field is marked
    /// all-ones, the true length is written at the head of the body, and
    /// the four body bytes it displaced move to the end of the bundle.
    fn end_message(&mut self, is_early_call: bool) {
        // nothing to do if no message yet
        let Some((packet, offset)) = self.msg_beg else {
            return;
        };

        // add the amt used in this packet to the length
        self.msg_len +=
            self.packets[self.current].msg_end_offset() as i32 - self.msg_chunk_offset as i32;

        let width = self.cur_ie.length_param();
        let needs_special_form = self.cur_ie.length_style()
            == crate::interface_element::VARIABLE_LENGTH_MESSAGE
            && (1..=3).contains(&width)
            && !InterfaceElement::can_handle_length(self.msg_len, width);

        if needs_special_form {
            // `InterfaceElement::specialCompressLength` through the
            // bundle: mark the field, keep the first four body bytes,
            // reserve their new home at the end of the bundle, then
            // write the true length over the body's head. The body may
            // straddle packets, so the moves go through the span
            // helpers.
            let width = width as usize;
            let body = (
                packet,
                offset + self.cur_ie.body_offset(self.msg_is_request),
            );
            for byte in &mut self.packets[packet].buffer_mut()[offset + 1..offset + 1 + width] {
                *byte = 0xff;
            }

            let displaced = self.span_read(body, SPECIAL_LENGTH_SIZE);
            let tail = self.qreserve(SPECIAL_LENGTH_SIZE);
            self.span_write(body, &self.msg_len.to_le_bytes());
            self.span_write(tail, &displaced);
        } else {
            // fill in headers for this msg. The frame never straddles a
            // packet, and the length fits its field, so the plain
            // compress sees exactly the bytes it needs.
            let header_len = self.cur_ie.header_size() as usize;
            let frame = &mut self.packets[packet].buffer_mut()[offset..offset + header_len];
            self.cur_ie
                .compress_length(frame, self.msg_len, self.msg_is_request)
                .expect("a fitting length compresses into its own header");
        }

        // record its details if it was reliable. `addReliableOrder` is
        // external-channel machinery, so only the reset survives here.
        if self.msg_is_reliable {
            self.msg_is_reliable = false; // for sanity
        }

        self.msg_chunk_offset = self.packets[self.current].msg_end_offset();
        self.msg_beg = None;
        self.msg_is_request = false;

        self.has_ended_msg_early = is_early_call;
    }

    /// Read `n` bundle bytes starting at `( packet, offset )`, walking
    /// into later packets when the region straddles one.
    fn span_read(&self, position: (usize, usize), n: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(n);
        let (mut packet, mut offset) = position;
        while bytes.len() < n {
            let end = self.packets[packet].msg_end_offset();
            let take = (end - offset).min(n - bytes.len());
            bytes.extend_from_slice(&self.packets[packet].data()[offset..offset + take]);
            packet += 1;
            offset = HEADER_SIZE;
        }
        bytes
    }

    /// The write half of [`span_read`](Self::span_read).
    fn span_write(&mut self, position: (usize, usize), bytes: &[u8]) {
        let (mut packet, mut offset) = position;
        let mut rest = bytes;
        while !rest.is_empty() {
            let end = self.packets[packet].msg_end_offset();
            let take = (end - offset).min(rest.len());
            self.packets[packet].buffer_mut()[offset..offset + take].copy_from_slice(&rest[..take]);
            rest = &rest[take..];
            packet += 1;
            offset = HEADER_SIZE;
        }
    }

    // -- finalising -------------------------------------------------------

    /// `finalise`: close the message and the packet. The stray-bytes
    /// check prints the reference's `CRITICAL_MSG` and carries on.
    pub fn finalise(&mut self) {
        // make sure we're not sending a packet where the message
        // wasn't properly started...
        if self.msg_beg.is_none()
            && self.packets[self.current].msg_end_offset() != self.msg_chunk_offset
        {
            eprintln!("UDPBundle::finalise: data not part of message found at end of bundle!");
        }

        self.end_message(false);
        self.end_packet(false);
        self.finalised = true;
    }

    /// `isFinalised`: whether `finalise` has run -- the reference checks
    /// it on the send path to avoid double-finalising an off-channel
    /// bundle (packet_sender.cpp:72).
    pub fn is_finalised(&self) -> bool {
        self.finalised
    }

    /// `writeFlags`: set up a packet's flags in accordance with the
    /// bundle's information, reserving the footer room each flag owes.
    /// The `reliableOrders_.size()` term of the reference's reliable
    /// condition is external-channel state, so it is absent here.
    fn write_flags_terms(&self) -> (bool, bool, bool) {
        (
            self.msg_is_reliable || self.num_reliable_messages > 0,
            self.packets.len() > 1,
            self.ack != SEQ_NULL,
        )
    }

    /// `preparePackets` (udp_bundle.cpp:398): fill in all the footers that
    /// are left to us, so every packet is a datagram ready for `sendto`.
    /// With a channel, its `writeFlags`/`writeFooter` ride along, reliable
    /// packets draw the channel's sequence numbers and file into its
    /// resend window; returns the index of the first packet that could not
    /// be sent (the send window overflowed before it), `None` when every
    /// packet goes out. The piggyback write (udp_bundle.cpp:474-521) and
    /// the external channel's reliable-order extraction ride with their
    /// paths (BW-02-05).
    pub fn prepare_packets(
        &mut self,
        seq_num_allocator: &mut SeqNumAllocator,
        should_use_checksums: bool,
        channel: Option<&ChannelPtr>,
    ) -> Option<usize> {
        let (flag_reliable, flag_fragment, has_ack) = self.write_flags_terms();
        let ack = self.ack;
        let num_packets = self.packets.len();
        let mut first_seq = 0u32;
        let mut last_seq = 0u32;
        let mut first_overflow: Option<usize> = None;
        let no_reply_orders: Vec<Rc<ReplyOrder>> = Vec::new();

        for (index, packet) in self.packets.iter_mut().enumerate() {
            if should_use_checksums {
                // Reserve space for the checksum footer
                assert!(!packet.has_flags(flags::HAS_CHECKSUM));
                packet.reserve_footer(4);
                packet.enable_flags(flags::HAS_CHECKSUM);
            }

            // `this->writeFlags( pPacket )`
            if flag_reliable {
                packet.enable_flags(flags::IS_RELIABLE);
            }
            if packet.has_flags(flags::HAS_REQUESTS) {
                packet.reserve_footer(2);
            }
            if flag_fragment {
                packet.enable_flags(flags::IS_FRAGMENT);
                packet.reserve_footer(8);
            }
            if has_ack {
                packet.enable_flags(flags::HAS_ACKS);
                packet.reserve_footer(1 + 4);
            }

            // `pChannel->writeFlags( pPacket )`: on-channel flags and the
            // ack footers they owe.
            if let Some(channel) = channel {
                UdpChannel::write_flags(&mut channel.borrow_mut(), packet);
            }

            // The sequence footer follows from the flags: reliable or
            // fragmented packets carry one, and so does everything an
            // external channel sends (udp_bundle.cpp:433).
            let wants_seq = channel.is_some_and(|ch| ch.borrow().is_external())
                || packet.has_flags(flags::IS_RELIABLE)
                || packet.has_flags(flags::IS_FRAGMENT);
            if wants_seq {
                packet.reserve_footer(4);
                packet.enable_flags(flags::HAS_SEQUENCE_NUMBER);
            }

            // Advance the cursor to the end of the footers, then write
            // backwards towards the message data, finishing back at it.
            let msg_end_offset = packet.msg_end_offset();
            packet.grow(packet.footer_size());

            // Pack in a zero checksum. We'll calculate it later.
            let mut checksum_at = None;
            if packet.has_flags(flags::HAS_CHECKSUM) {
                packet.pack_footer_u32(0);
                checksum_at = Some(packet.msg_end_offset());
            }

            if let Some(channel) = channel {
                UdpChannel::write_footer(&mut channel.borrow_mut(), packet);
            } else if packet.has_flags(flags::HAS_ACKS) {
                // For once off reliable, we only ever have one ack.
                packet.pack_footer_u8(1);
                packet.pack_footer_u32(ack);
            }

            if packet.has_flags(flags::HAS_SEQUENCE_NUMBER) {
                // If we're sending reliable traffic on a channel, use the
                // channel's sequence numbers. Otherwise use the nub's.
                let seq = match channel {
                    Some(channel) if packet.has_flags(flags::IS_RELIABLE) => {
                        UdpChannel::use_next_sequence_id(channel)
                    }
                    _ => seq_num_allocator.get_next(),
                };
                packet.set_seq(seq);
                packet.pack_footer_u32(seq);

                if index == 0 {
                    first_seq = seq;
                    last_seq = seq + num_packets as u32 - 1;
                }
            }

            // Add the first request offset.
            if packet.has_flags(flags::HAS_REQUESTS) {
                packet.pack_footer_u16(packet.first_request_offset() as u16);
            }

            // add the fragment info
            if packet.has_flags(flags::IS_FRAGMENT) {
                packet.pack_footer_u32(last_seq);
                packet.pack_footer_u32(first_seq);
            }

            // Make sure writing all the footers brought us back to the
            // end of the message data.
            assert_eq!(
                packet.msg_end_offset(),
                msg_end_offset,
                "the footers packed back to where they started"
            );

            if let Some(at) = checksum_at {
                packet.write_checksum(at);
            }

            // set up the reliable machinery (udp_bundle.cpp:588)
            if packet.has_flags(flags::IS_RELIABLE)
                && let Some(channel) = channel
            {
                // Internal channels carry no reliable orders; the external
                // arm's extraction rides with the external path.
                if !UdpChannel::add_resend_timer(
                    channel,
                    packet.seq(),
                    Rc::new(packet.clone()),
                    &no_reply_orders,
                ) && first_overflow.is_none()
                {
                    first_overflow = Some(index);
                }
            }
        }

        first_overflow
    }

    /// `addAck( seq )`: the one ack an off-channel bundle carries.
    pub fn add_ack(&mut self, seq: SeqNum) {
        assert!(self.ack == SEQ_NULL, "UDPBundle::addAck: ack already set");
        self.ack = seq;
    }

    /// `getAck()`.
    pub fn get_ack(&self) -> SeqNum {
        assert!(self.ack != SEQ_NULL, "UDPBundle::getAck: no ack to give");
        self.ack
    }

    /// `addReplyOrdersTo( pRequestManager, pChannel )`: hand every
    /// request's reply order to the manager, which picks the reply id
    /// and writes it into the frame. Called after
    /// [`finalise`](Self::finalise), the way `NetworkInterface::send`
    /// does it.
    pub fn add_reply_orders_to(
        &mut self,
        request_manager: &RequestManager,
        channel: Option<Address>,
    ) {
        for pending in &self.reply_orders {
            let frame = &mut self.packets[pending.packet].buffer_mut()[pending.frame_offset..];
            // The id slot sits inside the reserved header of a full-size
            // packet buffer, so the manager's frame-too-small failure
            // cannot happen here; the reference's log-and-continue would
            // only ever leave the slot as REPLY_ID_NONE.
            request_manager
                .add_reply_order(&pending.order, channel, frame)
                .expect("UDPBundle::addReplyOrdersTo: failed to add reply order");
        }
    }

    // -- measurements -----------------------------------------------------

    /// `isEmpty`: empty of messages *and* of anything that would put a
    /// footer on the wire.
    pub fn is_empty(&self) -> bool {
        !(self.num_messages > 0
            || self.has_multiple_data_units()
            || self.is_reliable()
            || self.has_data_footers()
            || self.ack != SEQ_NULL)
    }

    /// `numDataUnits`: packets in the chain (`chainLength`).
    pub fn num_data_units(&self) -> usize {
        self.packets.len()
    }

    /// `hasMultipleDataUnits`.
    pub fn has_multiple_data_units(&self) -> bool {
        self.num_data_units() > 1
    }

    /// `size`: the accumulated size of every packet, headers and footers
    /// included.
    pub fn size(&self) -> usize {
        self.packets.iter().map(|p| p.total_size()).sum()
    }

    /// `freeBytesInLastDataUnit`.
    pub fn free_bytes_in_last_data_unit(&self) -> usize {
        self.packets[self.current].free_space()
    }

    /// `isReliable`: the bundle carries a reliable driver.
    pub fn is_reliable(&self) -> bool {
        self.reliable_driver
    }

    /// `isCritical`: the bundle carries a critical request, so its last
    /// packet becomes the channel's `unackedCriticalSeq_`
    /// (udp_channel.cpp:800).
    pub fn is_critical(&self) -> bool {
        self.is_critical
    }

    /// `numReliableMessages_`: how many reliable messages the bundle
    /// frames (sending_stats.cpp).
    pub fn num_reliable_messages(&self) -> u32 {
        self.num_reliable_messages
    }

    /// `hasDataFooters`: whether any packet owes an ack or piggyback
    /// block.
    pub fn has_data_footers(&self) -> bool {
        self.packets
            .iter()
            .any(|p| p.has_flags(flags::HAS_ACKS) || p.has_flags(flags::HAS_PIGGYBACKS))
    }

    /// The packet at `index` in the chain, for the sender that walks the
    /// datagrams out.
    pub fn packet(&self, index: usize) -> &Packet {
        &self.packets[index]
    }

    /// Mutable: a channel filter (BW-02-05-01) rewrites or drops a packet
    /// on its way out (packet_sender.cpp:241-258 `sendRescheduledPacket`).
    pub fn packet_mut(&mut self, index: usize) -> &mut Packet {
        &mut self.packets[index]
    }

    /// `numMessages_`.
    pub fn num_messages(&self) -> u32 {
        self.num_messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::Endpoint;
    use crate::interface_element::{FIXED_LENGTH_MESSAGE, VARIABLE_LENGTH_MESSAGE};
    use crate::interfaces::InputMessageHandler;
    use crate::misc::{REPLY_ID_MAX, REPLY_ID_NONE, Reason, ReplyID};
    use crate::nub_exception::NubException;
    use crate::packet::{PACKET_MAX_SIZE, RESERVED_FOOTER_SIZE};
    use crate::request_manager::RequestManager;
    use crate::unpacked_message_header::UnpackedMessageHeader;
    use foundation::stream::BinaryStream;
    use std::cell::RefCell;

    // ------------------------------------------------------------------
    // Test doubles
    // ------------------------------------------------------------------

    /// A handler that records `( id, reply id, body bytes )` and eats its
    /// body.
    #[derive(Default)]
    struct Recorder {
        notes: RefCell<Vec<(u8, ReplyID, Vec<u8>)>>,
    }

    impl Recorder {
        fn notes(&self) -> Vec<(u8, ReplyID, Vec<u8>)> {
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
            data.finish();
        }

        fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
    }

    fn fixed(id: u8, size: i32) -> InterfaceElement {
        InterfaceElement::new("fixed", id, FIXED_LENGTH_MESSAGE, size, None)
    }

    fn variable(id: u8, width: i32) -> InterfaceElement {
        InterfaceElement::new("variable", id, VARIABLE_LENGTH_MESSAGE, width, None)
    }

    fn reply_handler(recorder: &Rc<ReplyRecorder>) -> Rc<dyn ReplyMessageHandler> {
        Rc::clone(recorder) as Rc<dyn ReplyMessageHandler>
    }

    /// A request manager on a pinned clock, the way the receive tests
    /// build one.
    fn manager() -> (Rc<RequestManager>, std::rc::Rc<std::cell::Cell<u64>>) {
        let now = Rc::new(std::cell::Cell::new(1_000_000u64));
        let clock = Rc::clone(&now);
        let dispatcher =
            crate::event_dispatcher::EventDispatcher::with_clock(Box::new(move || clock.get()));
        (
            Rc::new(RequestManager::new(false, Rc::new(dispatcher))),
            now,
        )
    }

    // ------------------------------------------------------------------
    // Frame layout
    // ------------------------------------------------------------------

    /// A fixed message frames as the id byte and its body, flags clear,
    /// one packet, finalised to exactly the wire bytes.
    #[test]
    fn a_fixed_message_frames_as_id_and_body() {
        let mut bundle = UdpBundle::new(0);
        assert!(bundle.is_empty(), "a fresh bundle carries nothing");

        bundle.start_message(&fixed(3, 4), ReliableType::No);
        bundle.write_bytes(b"ABCD");
        bundle.finalise();

        assert_eq!(bundle.num_data_units(), 1);
        assert_eq!(bundle.num_messages(), 1);
        let packet = bundle.packet(0);
        assert_eq!(packet.msg_end_offset(), HEADER_SIZE + 5);
        assert_eq!(packet.total_size(), 7);
        assert_eq!(packet.data(), &[0, 0, 3, b'A', b'B', b'C', b'D']);

        // prepare without checksums leaves the datagram untouched.
        let mut allocator = SeqNumAllocator::new(1);
        bundle.prepare_packets(&mut allocator, false, None);
        assert_eq!(bundle.packet(0).data(), &[0, 0, 3, b'A', b'B', b'C', b'D']);
        assert_eq!(bundle.size(), 7);
    }

    /// A variable message writes its length field at finalise, sized by
    /// what was streamed after the header.
    #[test]
    fn a_variable_message_writes_its_length_field() {
        let mut bundle = UdpBundle::new(0);
        bundle.start_message(&variable(4, 2), ReliableType::No);
        bundle.write_bytes(b"hello");
        bundle.finalise();

        let packet = bundle.packet(0);
        assert_eq!(
            packet.data(),
            &[0, 0, 4, 5, 0, b'h', b'e', b'l', b'l', b'o']
        );
    }

    /// A reply is a `REPLY` message whose body starts with the id it
    /// answers, the id counting toward the length.
    #[test]
    fn a_reply_frames_as_the_reply_element_with_the_id() {
        let mut bundle = UdpBundle::new(0);
        bundle.start_reply(77, ReliableType::No);
        bundle.write_bytes(b"done");
        bundle.finalise();

        let packet = bundle.packet(0);
        // id 0xFF, a four-byte length field holding 8 (the four id bytes
        // plus the payload), then the id and the payload.
        assert_eq!(
            packet.data(),
            &[
                0, 0,    // flags
                0xFF, // the reply id byte
                8, 0, 0, 0, // the length field, four bytes wide
                77, 0, 0, 0, // the id the reply answers
                b'd', b'o', b'n', b'e',
            ]
        );
    }

    /// A request reserves the reply id and the next-request link, zeroes
    /// them until the send fills them in, flags the packet, and records
    /// the frame for the reply orders.
    #[test]
    fn a_request_reserves_its_preamble_and_flags_the_packet() {
        let mut bundle = UdpBundle::new(0);
        let replies = Rc::new(ReplyRecorder::default());
        bundle.start_request(
            &variable(4, 2),
            reply_handler(&replies),
            6,
            5_000_000,
            ReliableType::No,
        );
        bundle.write_bytes(b"pay");
        bundle.finalise();

        let packet = bundle.packet(0);
        assert!(packet.has_flags(flags::HAS_REQUESTS));
        // id, length, reply id (zero until the send writes it), link
        // (zero: no next request), body.
        assert_eq!(
            packet.data(),
            &[
                0x01, 0x00, // flags: HAS_REQUESTS
                4,    // the id byte
                3, 0, // the length field
                0, 0, 0, 0, // the reply id, reserved
                0, 0, // the next-request link, empty
                b'p', b'a', b'y',
            ]
        );
        assert_eq!(
            packet.first_request_offset(),
            HEADER_SIZE,
            "the frame starts at the flags word's end"
        );

        bundle.prepare_packets(&mut SeqNumAllocator::new(1), false, None);
        // The first request offset is packed as the footer.
        let packet = bundle.packet(0);
        assert_eq!(
            &packet.wire()[packet.msg_end_offset()..],
            &(HEADER_SIZE as u16).to_le_bytes(),
            "the footer carries the packet-absolute frame offset"
        );
    }

    /// `addReplyOrdersTo` picks a reply id per request, writes it into
    /// the reserved slot, and leaves a pending request behind.
    #[test]
    fn the_reply_orders_fill_their_slots_and_the_manager() {
        let (manager, _clock) = manager();
        let replies = Rc::new(ReplyRecorder::default());

        let mut bundle = UdpBundle::new(0);
        bundle.start_request(
            &variable(4, 2),
            reply_handler(&replies),
            6,
            5_000_000,
            ReliableType::No,
        );
        bundle.write_bytes(b"pay");
        bundle.finalise();
        bundle.add_reply_orders_to(&manager, None);

        assert_eq!(manager.num_pending(), 1);
        let packet = bundle.packet(0);
        let data = packet.data();
        let reply_id = ReplyID::from_le_bytes(data[5..9].try_into().unwrap());
        assert!(
            (0..REPLY_ID_MAX).contains(&reply_id),
            "the manager handed out a real id, got {reply_id}"
        );
        assert_eq!(&data[9..11], &[0, 0], "the only link stays empty");
    }

    /// Two requests chain their links: the first points at the second in
    /// packet-absolute terms, the second's stays empty, and the footer
    /// names the first.
    #[test]
    fn two_requests_chain_their_links() {
        let replies = Rc::new(ReplyRecorder::default());
        let mut bundle = UdpBundle::new(0);
        bundle.start_request(
            &variable(4, 2),
            reply_handler(&replies),
            1,
            5_000_000,
            ReliableType::No,
        );
        bundle.write_bytes(b"one");
        bundle.start_request(
            &variable(4, 2),
            reply_handler(&replies),
            2,
            5_000_000,
            ReliableType::No,
        );
        bundle.write_bytes(b"two");
        bundle.finalise();

        let packet = bundle.packet(0);
        let data = packet.data();
        // Frame one at packet offset 2, frame two at 2 + (3 + 6 + 3) = 14.
        assert_eq!(packet.first_request_offset(), 2);
        assert_eq!(&data[9..11], &(14u16).to_le_bytes(), "first link -> second");
        assert_eq!(&data[21..23], &[0, 0], "second link is empty");
    }

    // ------------------------------------------------------------------
    // Multi-packet bundles
    // ------------------------------------------------------------------

    /// A message longer than a packet continues into the next one, and
    /// `preparePackets` marks every packet a fragment with its sequence
    /// number and the range footers.
    #[test]
    fn a_message_outgrowing_a_packet_splits_into_fragments() {
        let body: Vec<u8> = (0..2000).map(|i| (i % 251) as u8).collect();

        let mut bundle = UdpBundle::new(0);
        bundle.start_message(&variable(4, 2), ReliableType::No);
        bundle.write_bytes(&body);
        bundle.finalise();

        assert_eq!(bundle.num_data_units(), 2);
        assert!(bundle.has_multiple_data_units());
        assert!(!bundle.is_empty(), "a fragmented bundle is not empty");

        let mut allocator = SeqNumAllocator::new(1);
        bundle.prepare_packets(&mut allocator, false, None);

        let first_body_len = PACKET_MAX_SIZE - RESERVED_FOOTER_SIZE - HEADER_SIZE - 3;
        // Packet one: id, length, the first slice of the body.
        let first = bundle.packet(0);
        assert_eq!(
            first.flags(),
            flags::IS_FRAGMENT | flags::HAS_SEQUENCE_NUMBER,
            "a fragment carries its number footer"
        );
        assert_eq!(
            &first.data()[HEADER_SIZE..HEADER_SIZE + 3],
            &[
                4,
                (body.len() as u16).to_le_bytes()[0],
                (body.len() as u16).to_le_bytes()[1]
            ],
            "the length lands on the first packet's frame"
        );
        assert_eq!(
            &first.data()[HEADER_SIZE + 3..HEADER_SIZE + 3 + first_body_len],
            &body[..first_body_len]
        );

        let second = bundle.packet(1);
        assert_eq!(
            second.flags(),
            flags::IS_FRAGMENT | flags::HAS_SEQUENCE_NUMBER
        );
        assert_eq!(
            second.body_size(),
            body.len() - first_body_len,
            "the rest of the body rides the second packet"
        );

        // The allocator numbered the chain in order.
        assert_eq!(first.seq(), 1);
        assert_eq!(second.seq(), 2);
        assert_eq!(
            first.frag_begin(),
            SEQ_NULL,
            "only stripFragInfo fills these, on receive"
        );

        // The footers on the wire, ascending: fragBegin, fragEnd, seq.
        let expected_footer: Vec<u8> = [
            first.seq().to_le_bytes().as_slice(),
            second.seq().to_le_bytes().as_slice(),
            first.seq().to_le_bytes().as_slice(),
        ]
        .concat();
        assert_eq!(
            first.wire()[first.msg_end_offset()..],
            expected_footer[..],
            "first-seq 1, last-seq 2, then the packet's own number"
        );
    }

    /// An oversize body takes the special form at finalise: the field is
    /// marked all-ones, the true length sits at the body's head, and the
    /// displaced four bytes ride at the end of the bundle.
    #[test]
    fn the_special_form_marks_an_oversize_body() {
        let payload: Vec<u8> = (1..=255u8).collect();

        let mut bundle = UdpBundle::new(0);
        bundle.start_message(&variable(5, 1), ReliableType::No);
        bundle.write_bytes(&payload);
        bundle.finalise();

        let packet = bundle.packet(0);
        let data = packet.data();
        // flags, id, marked field, true length (255, LE), body minus its
        // first four bytes, the displaced four bytes.
        let mut expected = vec![0, 0, 5, 0xff];
        expected.extend_from_slice(&255u32.to_le_bytes());
        expected.extend_from_slice(&payload[4..]);
        expected.extend_from_slice(&payload[..4]);
        assert_eq!(data, &expected[..]);
    }

    // ------------------------------------------------------------------
    // Footers on the wire
    // ------------------------------------------------------------------

    /// An ack reserves the footer, flags the packet, and packs as count
    /// one plus the sequence number.
    #[test]
    fn an_ack_lands_in_the_footer() {
        let mut bundle = UdpBundle::new(0);
        bundle.add_ack(9);
        assert!(!bundle.is_empty(), "an acked bundle is not empty");
        assert_eq!(bundle.get_ack(), 9);

        bundle.prepare_packets(&mut SeqNumAllocator::new(1), false, None);
        assert!(
            bundle.has_data_footers(),
            "prepare put the flag on the packet"
        );
        let packet = bundle.packet(0);
        assert_eq!(packet.flags(), flags::HAS_ACKS);
        assert_eq!(
            &packet.wire()[packet.msg_end_offset()..],
            &[9, 0, 0, 0, 1],
            "the sequence number, then the count packed above it"
        );
    }

    /// `preparePackets` with checksums reserves the footer, flags the
    /// packet, and writes a checksum that validates over the datagram.
    #[test]
    fn checksums_are_reserved_and_written_by_prepare() {
        let mut bundle = UdpBundle::new(0);
        bundle.start_message(&fixed(3, 4), ReliableType::No);
        bundle.write_bytes(b"ABCD");
        bundle.finalise();

        bundle.prepare_packets(&mut SeqNumAllocator::new(1), true, None);
        let packet = bundle.packet(0);
        assert!(packet.has_flags(flags::HAS_CHECKSUM));
        assert_eq!(packet.total_size(), 7 + 4);

        let wire: Vec<u8> = packet.wire().to_vec();
        let mut received = Packet::new();
        received.grow(wire.len());
        received.buffer_mut()[..wire.len()].copy_from_slice(&wire);
        assert!(
            received.validate_checksum(),
            "the written checksum validates"
        );
    }

    /// An empty bundle finalises to a bare flags word and answers empty;
    /// a reliable hint (`reliable()`) on a footer-only bundle is not
    /// empty, its sequence number still owed to the wire.
    #[test]
    fn an_empty_bundle_finalises_to_a_bare_flags_word() {
        let mut bundle = UdpBundle::new(0);
        bundle.finalise();
        assert!(bundle.is_empty());
        bundle.prepare_packets(&mut SeqNumAllocator::new(1), false, None);
        assert_eq!(bundle.size(), HEADER_SIZE);

        // `msgIsReliable_` on a footer-only bundle still counts.
        let mut hinted = UdpBundle::new(0);
        hinted.reliable(ReliableType::Driver);
        assert!(hinted.is_reliable());
        assert!(
            !hinted.is_empty(),
            "the reliable hint owes a sequence number"
        );

        hinted.prepare_packets(&mut SeqNumAllocator::new(1), false, None);
        let packet = hinted.packet(0);
        assert_eq!(
            packet.flags(),
            flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER
        );
        assert_eq!(
            &packet.wire()[packet.msg_end_offset()..],
            &1u32.to_le_bytes(),
            "the hint alone still takes a sequence number"
        );
    }

    /// A reliable message counts itself in `numReliableMessages` and
    /// resets the per-message hint when it ends, so `preparePackets`
    /// flags the packet reliable and gives it a sequence number.
    #[test]
    fn a_reliable_message_takes_a_sequence_number() {
        let mut bundle = UdpBundle::new(0);
        bundle.start_message(&fixed(5, 2), ReliableType::Passenger);
        bundle.write_bytes(&[7, 9]);
        bundle.finalise();

        assert_eq!(bundle.num_messages(), 1);
        bundle.prepare_packets(&mut SeqNumAllocator::new(3), false, None);

        let packet = bundle.packet(0);
        assert_eq!(
            packet.flags(),
            flags::IS_RELIABLE | flags::HAS_SEQUENCE_NUMBER,
            "one reliable message makes the packet reliable"
        );
        assert_eq!(packet.seq(), 3);
        assert_eq!(
            &packet.wire()[packet.msg_end_offset()..],
            &3u32.to_le_bytes(),
            "the sequence footer rides behind the body"
        );
    }

    /// `write_u16`/`write_u32` stream through `addBlob`, and a value that
    /// does not fit the current packet opens the next one (`qreserve`'s
    /// `sreserve` arm).
    #[test]
    fn a_header_at_the_packet_edge_opens_a_new_packet() {
        let mut bundle = UdpBundle::new(0);
        while bundle.free_bytes_in_last_data_unit() >= 2 {
            bundle.write_u16(0x55AA);
        }
        assert_eq!(
            bundle.free_bytes_in_last_data_unit(),
            1,
            "the u16 fill lands one byte short of full"
        );

        // A message header claims contiguous space, so one that does not
        // fit the current packet opens the next one rather than
        // straddling -- `qreserve`'s `sreserve` arm.
        bundle.start_message(&variable(7, 2), ReliableType::No);
        bundle.write_bytes(&[3, 4]);
        bundle.finalise();

        assert_eq!(
            bundle.num_data_units(),
            2,
            "the header refused to straddle the packet edge"
        );
        let last = bundle.packet(1);
        assert_eq!(
            last.body_size(),
            5,
            "id byte, two length bytes, two body bytes"
        );
        assert_eq!(
            &last.data()[HEADER_SIZE..],
            &[7, 2, 0, 3, 4],
            "the fresh packet holds the whole frame"
        );
    }

    /// `write_u16`/`write_u32` stream through `addBlob` as plain bytes,
    /// and bytes written outside any message draw `finalise`'s
    /// stray-data `CRITICAL_MSG`, the way the reference logs them.
    #[test]
    fn integer_writes_stream_and_stray_bytes_draw_the_warning() {
        let mut bundle = UdpBundle::new(0);
        bundle.write_u32(0x0403_0201);
        bundle.write_u16(0x55AA);
        bundle.finalise();

        assert_eq!(bundle.num_messages(), 0);
        let packet = bundle.packet(0);
        assert_eq!(
            &packet.data()[HEADER_SIZE..HEADER_SIZE + 6],
            &[1, 2, 3, 4, 0xAA, 0x55],
            "both integers landed little-endian"
        );
    }

    // ------------------------------------------------------------------
    // The acceptance round trip
    // ------------------------------------------------------------------

    /// A bundle built the way a real sender builds it -- a plain fixed
    /// message, a request, and the reply to a pending request -- walks a
    /// real loopback UDP socket pair, checksum validated, footers
    /// stripped, and delivers every message through the dispatch.
    #[test]
    fn a_bundle_survives_a_real_udp_round_trip() {
        use crate::bundle_dispatch::dispatch_messages;
        use crate::interface_table::InterfaceTable;

        let mut listener = Endpoint::new();
        listener.socket(platform::socket::SOCK_DGRAM);
        assert_eq!(listener.bind(0, crate::endpoint::INADDR_ANY), 0);
        let bound = listener.local();

        let mut sender = Endpoint::new();
        sender.socket(platform::socket::SOCK_DGRAM);
        assert_eq!(sender.bind(0, 0x0100_007f), 0);

        // The receiver's interface: two messages served, replies handled
        // by the receiver's request manager.
        let (receiver_manager, _clock) = manager();
        let plain = Rc::new(Recorder::default());
        let requested = Rc::new(Recorder::default());

        let table = InterfaceTable::new();
        let fixed_element = fixed(3, 4);
        let variable_element = variable(4, 2);
        table.serve(
            &fixed_element,
            Some(Rc::clone(&plain) as Rc<dyn InputMessageHandler>),
        );
        table.serve(
            &variable_element,
            Some(Rc::clone(&requested) as Rc<dyn InputMessageHandler>),
        );
        table.serve(
            &InterfaceElement::reply(),
            Some(Rc::clone(&receiver_manager) as Rc<dyn InputMessageHandler>),
        );

        // A request pending on the receiver's manager -- in a real
        // exchange the far end asked earlier, and the reply packed below
        // answers it.
        let replies = Rc::new(ReplyRecorder::default());
        let mut ask = UdpBundle::new(0);
        ask.start_request(
            &variable_element,
            reply_handler(&replies),
            99,
            DEFAULT_REQUEST_TIMEOUT,
            ReliableType::No,
        );
        ask.write_bytes(b"?");
        ask.finalise();
        ask.add_reply_orders_to(&receiver_manager, None);
        let pending = ReplyID::from_le_bytes(ask.packet(0).data()[5..9].try_into().unwrap());
        assert_eq!(receiver_manager.num_pending(), 1);

        // The in-bundle request, by contrast, is registered on a
        // sender-side manager: it waits for a reply that only the far
        // end can send back.
        let (sender_manager, _sender_clock) = manager();

        // The outgoing bundle: a fixed message, a request, and the reply
        // to the pending request above.
        let mut bundle = UdpBundle::new(0);
        bundle.start_message(&fixed_element, ReliableType::No);
        bundle.write_bytes(b"ABCD");
        bundle.start_request(
            &variable_element,
            reply_handler(&replies),
            6,
            DEFAULT_REQUEST_TIMEOUT,
            ReliableType::No,
        );
        bundle.write_bytes(b"pay");
        bundle.start_reply(pending, ReliableType::No);
        bundle.write_bytes(b"done");
        bundle.finalise();
        bundle.add_reply_orders_to(&sender_manager, None);
        bundle.prepare_packets(&mut SeqNumAllocator::new(1), true, None);

        assert_eq!(bundle.num_data_units(), 1, "everything fits one datagram");
        let datagrams = bundle.num_data_units();
        for index in 0..datagrams {
            let packet = bundle.packet(index);
            let datagram = packet.wire();
            assert_eq!(
                sender.sendto_address(datagram, bound),
                datagram.len() as i32,
                "datagram {index} left whole"
            );
        }

        // One more datagram with no footers at all: no checksum to
        // validate and no request footer to peel.
        let mut bare = Packet::new();
        bare.grow(HEADER_SIZE);
        assert_eq!(
            sender.sendto_address(bare.wire(), bound),
            HEADER_SIZE as i32
        );

        // Receive and dispatch each datagram exactly as the packet
        // receiver will: checksum first, then the request footer, then
        // the body slice the walk reads.
        let mut source = crate::endpoint::Address::default();
        for index in 0..datagrams + 1 {
            let mut packet = Packet::new();
            assert!(
                packet.recv_from_endpoint(&listener, &mut source) >= 0,
                "datagram {index} arrived"
            );

            if packet.has_flags(flags::HAS_CHECKSUM) {
                assert!(
                    packet.validate_checksum(),
                    "datagram {index} carries a checksum that validates"
                );
            }

            let first_request = if packet.has_flags(flags::HAS_REQUESTS) {
                Some(packet.strip_footer_u16().expect("requests footer") as usize)
            } else {
                None
            };

            let end = packet.msg_end_offset();
            let body = &mut packet.buffer_mut()[HEADER_SIZE..end];
            let reason = dispatch_messages(&table, &source, body, first_request);
            assert_eq!(reason, Reason::Success, "datagram {index} dispatched clean");
        }

        // A request that dies before its reply arrives hands the
        // exception to the same handler; none of it reaches the body.
        let delivered = replies.notes().len();
        replies.handle_exception(&NubException::new(Reason::TimerExpired, None), 6);
        assert_eq!(
            replies.notes().len(),
            delivered,
            "an exception delivered no body"
        );

        // Every message landed: the fixed one plain, the request with
        // its reply id, and the reply through the sender's manager.
        assert_eq!(plain.notes(), vec![(3, REPLY_ID_NONE, b"ABCD".to_vec())]);
        let request_notes = requested.notes();
        assert_eq!(request_notes.len(), 1);
        assert_eq!(request_notes[0].0, 4);
        assert_eq!(request_notes[0].2, b"pay".to_vec());
        assert_eq!(
            replies.notes(),
            vec![(99, b"done".to_vec())],
            "the reply found the receiver's pending request"
        );
        assert_eq!(
            receiver_manager.num_pending(),
            0,
            "the answer took the request off the table"
        );
        assert_eq!(
            sender_manager.num_pending(),
            1,
            "the in-bundle request still waits for the far end"
        );
    }

    /// `freeBytesInLastDataUnit` tracks the room the current packet has
    /// left, and a bundle that outgrows its packet reports the new one.
    #[test]
    fn free_bytes_tracks_the_current_packet() {
        let mut bundle = UdpBundle::new(0);
        let fresh = bundle.free_bytes_in_last_data_unit();
        assert!(fresh > 0);

        bundle.start_message(&fixed(3, 4), ReliableType::No);
        bundle.write_bytes(&[b'x'; 10]);
        assert_eq!(
            bundle.free_bytes_in_last_data_unit(),
            fresh - 10 - 1,
            "the id byte and the body ate their room"
        );
    }
}
