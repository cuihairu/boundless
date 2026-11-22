//! `Mercury::Channel` / `Mercury::UDPChannel` (`lib/network/channel.hpp`,
//! `lib/network/udp_channel.hpp`): the lifecycle identity of a regular
//! connection between two addresses, and the window it sends through.
//!
//! Scope of BW-02-03-02 through -06: registration and closing (the address
//! identity, the traits/version bookkeeping, and the condemn / destroy /
//! remote-failed lifecycle that [`ChannelMap`], `CondemnedChannels` and
//! `RecentlyDeadChannels` drive), the acknowledgement half of the data
//! plane -- the unacked-packet send window, the sequence allocators and the
//! wrap math, ACK application, unreliable-sequence validation, and the
//! channel's part of a packet's flags and footer -- the timed resend
//! scan (`checkResendTimers` / `resend` / `sendUnacked`), the receive
//! window (`addToReceiveWindow`, the buffered receives that restore the
//! original order), and the fragment chain holder (`pFragments_`).
//!
//! Still the seam: piggybacks and their `reliableOrders_` (the on-bundle
//! half of `resend`), the receiver that walks a next-in-window chain into
//! bundles plus the physical wire send that `sendUnacked` leaves to the
//! interface (-07).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};

use crate::endpoint::{Address, NONE};
use crate::event_dispatcher::STAMPS_PER_SECOND;
use crate::fragmented_bundle::FragmentedBundle;
use crate::misc::{
    CHANNEL_ID_NULL, ChannelID, ChannelVersion, Reason, SEQ_MASK, SEQ_NULL, SeqNum,
    SeqNumAllocator, seq_less_than, seq_mask,
};
use crate::network_interface::{ChannelRef, NetworkInterface};
use crate::packet::{MAX_ACKS, Packet, flags};
use crate::packet_receiver::PacketReceiverStats;
use crate::udp_bundle::UDPBundle;

/// `EXTERNAL_CHANNEL_SIZE` (udp_channel.cpp:27).
const EXTERNAL_CHANNEL_SIZE: usize = 256;
/// `INTERNAL_CHANNEL_SIZE` (udp_channel.cpp:28).
const INTERNAL_CHANNEL_SIZE: usize = 4096;
/// `INDEXED_CHANNEL_SIZE` (udp_channel.cpp:29).
const INDEXED_CHANNEL_SIZE: usize = 512;

/// `UDPChannel::s_maxOverflowPackets_` (udp_channel.cpp:34), indexed external /
/// internal / indexed.
const MAX_OVERFLOW_PACKETS: [SeqNum; 3] = [1024, 8192, 4096];
/// `UDPChannel::s_sendWindowWarnThresholds_` (udp_channel.cpp:42), indexed
/// internal / indexed.
const SEND_WINDOW_WARN_THRESHOLDS: [u32; 2] = [
    INTERNAL_CHANNEL_SIZE as u32 / 4,
    INDEXED_CHANNEL_SIZE as u32 / 4,
];

/// Stamps per millisecond, for the reference's `stampsPerSecondD()` log lines.
fn stamps_per_millisecond() -> u64 {
    STAMPS_PER_SECOND / 1000
}

/// `UDPChannel::UnackedPacket` (`lib/network/unacked_packet.hpp`): a packet
/// the channel may still have to resend. The reference pools and custom
/// allocates these; the port just owns them from the window's slots.
struct UnackedPacket {
    /// `pPacket_`.
    packet: Rc<RefCell<Packet>>,
    /// The outgoing sequence number on the channel the last time this packet
    /// was sent.
    last_sent_at_out_seq: Cell<SeqNum>,
    /// The time this packet was initially sent.
    last_sent_time: Cell<u64>,
    /// Whether this packet has been resent.
    was_resent: Cell<bool>,
    // reliableOrders_, the piggyback records the reference streams alongside,
    // are the BW-02-03-04 seam.
}

impl UnackedPacket {
    /// `UnackedPacket( pPacket )`, with the fields `addResendTimer` fills in.
    fn new(packet: &Rc<RefCell<Packet>>, out_seq: SeqNum, now: u64) -> Self {
        Self {
            packet: Rc::clone(packet),
            last_sent_at_out_seq: Cell::new(out_seq),
            last_sent_time: Cell::new(now),
            was_resent: Cell::new(false),
        }
    }
}

/// `Mercury::CircularArray<T>` (`lib/network/circular_array.hpp`): a
/// power-of-two slot array where the index is the sequence number, so
/// `array[ seq ]` is `slots[ seq & mask ]`. The channel instantiates it for
/// the send window ([`UnackedPacket`]) and the buffered-receive window
/// (packets).
struct CircularArray<T> {
    slots: Vec<Option<T>>,
    mask: usize,
}

/// The send window's instantiation: `CircularArray<UnackedPacket *>`.
type SendWindow = CircularArray<UnackedPacket>;

/// The buffered-receive window's instantiation: `CircularArray<PacketPtr>`.
type ReceiveWindow = CircularArray<Rc<RefCell<Packet>>>;

impl<T> CircularArray<T> {
    /// `CircularArray( size )`: every slot starts empty. The reference
    /// default-constructs `new T[size]`, which is `NULL` for its pointer
    /// element type.
    fn new(size: usize) -> Self {
        assert!(size.is_power_of_two(), "circular arrays are powers of two");
        Self {
            slots: (0..size).map(|_| None).collect(),
            mask: size - 1,
        }
    }

    /// `size()`.
    fn size(&self) -> usize {
        self.slots.len()
    }

    /// `operator[]( n )`.
    fn get(&self, seq: SeqNum) -> Option<&T> {
        self.slots[(seq as usize) & self.mask].as_ref()
    }

    /// `operator[]( n )`, mutable.
    fn get_mut(&mut self, seq: SeqNum) -> Option<&mut T> {
        self.slots[(seq as usize) & self.mask].as_mut()
    }

    /// `operator[]` write.
    fn set(&mut self, seq: SeqNum, value: Option<T>) {
        self.slots[(seq as usize) & self.mask] = value;
    }

    /// `doubleSize( startIndex )`: double the window. The reference copies
    /// `newWindow[ startIndex + i ] = (*this)[ startIndex + i ]`, so every
    /// slot keeps its absolute position -- the "slot == index & mask"
    /// invariant survives, which is what lets a packet be found again by
    /// its sequence number after the window grows (circular_array.hpp:63).
    fn double_size(&mut self, start_index: SeqNum) {
        let old_size = self.slots.len();
        let old_mask = old_size - 1;
        let new_mask = old_size * 2 - 1;
        let mut new_slots: Vec<Option<T>> = (0..new_mask + 1).map(|_| None).collect();

        for i in 0..old_size {
            // `startIndex + i` is uint32 arithmetic in the reference; the
            // array's `operator[]` masks it on each side.
            let index = start_index.wrapping_add(i as SeqNum);
            if let Some(value) = self.slots[(index as usize) & old_mask].take() {
                new_slots[(index as usize) & new_mask] = Some(value);
            }
        }

        self.slots = new_slots;
        self.mask = new_mask;
    }

    /// The `clearState` discard count.
    fn count_used(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// `clearState`: forget every packet still queued.
    fn clear(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
    }
}

/// `UDPChannel::Traits` (udp_channel.hpp:67): decides the reliability
/// method -- internal channels are low latency / low loss, external ones
/// high latency / high loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelTraits {
    /// Server-to-server.
    Internal,
    /// Client-to-server.
    External,
}

/// `UDPChannel::AddToReceiveWindowResult` (udp_channel.hpp:161): what
/// `addToReceiveWindow` did with a received reliable packet, so the receiver
/// knows whether to dispatch it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddToReceiveWindowResult {
    /// The packet was the next one in sequence, with any buffered
    /// successors chained onto it.
    PacketIsNextInWindow,
    /// The packet sat ahead of its sequence number and was buffered.
    PacketIsBufferedInWindow,
    /// The packet had already been received.
    PacketIsDuplicate,
    /// The packet was far enough ahead of `inSeqAt_` to be unusable.
    PacketIsOutOfRange,
    /// The packet was corrupt (out-of-range sequence number, or from the
    /// wrong source address).
    PacketIsCorrupt,
}

/// `UDPChannel`: one regular communication channel to one address.
///
/// The channel holds only a [`Weak`] to its interface: the interface owns
/// the channel via its channel map, so a strong reference would be a cycle.
/// Every method that reaches the interface upgrades the weak reference.
pub struct UDPChannel {
    /// `pNetworkInterface_`.
    network_interface: Weak<NetworkInterface>,
    /// `addr_`.
    addr: Cell<Address>,
    /// `traits_`.
    traits: ChannelTraits,
    /// `id_`.
    id: ChannelID,
    /// `version_`.
    version: Cell<ChannelVersion>,
    /// `creationVersion_`.
    creation_version: Cell<ChannelVersion>,
    /// `isCondemned_`.
    is_condemned: Cell<bool>,
    /// `isDestroyed_`.
    is_destroyed: Cell<bool>,
    /// `hasRemoteFailed_`.
    has_remote_failed: Cell<bool>,
    /// `isAnonymous_`.
    is_anonymous: Cell<bool>,
    /// `isLocalRegular_`.
    is_local_regular: Cell<bool>,
    /// `isRemoteRegular_`.
    is_remote_regular: Cell<bool>,
    /// `lastReceivedTime_`, in stamps.
    last_received_time: Cell<u64>,
    /// `lastReliableSendTime_`, in stamps.
    last_reliable_send_time: Cell<u64>,
    /// `oldestUnackedSeq_`: `SEQ_NULL` when nothing is unacked.
    oldest_unacked_seq: Cell<SeqNum>,
    /// `unackedCriticalSeq_`: `SEQ_NULL` when no critical is unacked.
    unacked_critical_seq: Cell<SeqNum>,
    /// `windowSize_`: how many packets the window holds before it overflows.
    window_size: usize,
    /// `smallOutSeqAt_`: the lowest sequence number that has not been filled
    /// in yet.
    small_out_seq_at: Cell<SeqNum>,
    /// `largeOutSeqAt_`: the highest sequence number that has been handed out.
    large_out_seq_at: RefCell<SeqNumAllocator>,
    /// `unreliableInSeqAt_`: the last sequence number that has been seen on
    /// an unreliable packet.
    unreliable_in_seq_at: Cell<SeqNum>,
    /// `unackedPackets_`: the send window.
    unacked_packets: RefCell<SendWindow>,
    /// `hasSeenOverflowWarning_`.
    has_seen_overflow_warning: Cell<bool>,
    /// `inSeqAt_`: the next sequence number that is expected to arrive.
    in_seq_at: Cell<SeqNum>,
    /// `bufferedReceives_`: packets that arrived ahead of `inSeqAt_`, held
    /// in order until the gap closes.
    buffered_receives: RefCell<ReceiveWindow>,
    /// `numBufferedReceives_`.
    num_buffered_receives: Cell<u32>,
    /// `shouldAutoSwitchToSrcAddr_`: whether a received packet with a newer
    /// version may switch the channel's address.
    should_auto_switch_to_src_addr: Cell<bool>,
    /// `pushUnsentAcksThreshold_`: how many unsent ACKs before the channel
    /// pushes them out immediately (`0` disables it).
    push_unsent_acks_threshold: Cell<u32>,
    /// `pFragments_`: the partially reassembled bundle whose fragments are
    /// arriving on this channel.
    p_fragments: RefCell<Option<Rc<RefCell<FragmentedBundle>>>>,
    /// `acksToSend_`: the ACKs that have been received but not sent yet.
    acks_to_send: RefCell<BTreeSet<SeqNum>>,
    /// `highestAck_`.
    highest_ack: Cell<SeqNum>,
    /// `roundTripTime_`, in stamps.
    round_trip_time: Cell<u64>,
    /// `lastReliableResendTime_`, in stamps.
    last_reliable_resend_time: Cell<u64>,
    /// `minInactivityResendDelay_`, in stamps.
    min_inactivity_resend_delay: Cell<u64>,
    /// `numPacketsResent_`.
    num_packets_resent: Cell<u32>,
    /// `numReliablePacketsSent_`.
    num_reliable_packets_sent: Cell<u32>,
    /// `numDataUnitsSent_` (udp_channel.cpp:785): how many bundles this
    /// channel has pushed, not packets.
    num_data_units_sent: Cell<u32>,
    /// `numBytesSent_`.
    num_bytes_sent: Cell<u64>,
    /// `shouldDropNextSend_`: arrange for the next bundle to be dropped.
    should_drop_next_send: Cell<bool>,
    /// `wantsFirstPacket_`: a reset established channel waits for
    /// `FLAG_CREATE_CHANNEL` before accepting packets.
    wants_first_packet: Cell<bool>,
    /// The previous high-water mark for the internal send window backlog
    /// (udp_channel.cpp:1756).
    send_window_warn_threshold: Cell<u32>,
}

impl UDPChannel {
    /// `UDPChannel( networkInterface, address, traits, ..., id )`
    /// (`udp_channel.cpp:82`): the base starts at `NONE` and the
    /// derived constructor's `setAddress( address )` registers non-indexed
    /// channels with the interface.
    pub fn new(
        interface: &Rc<NetworkInterface>,
        addr: Address,
        traits: ChannelTraits,
        id: ChannelID,
    ) -> Rc<UDPChannel> {
        // (udp_channel.cpp:92): external channels are wide and slow, internal
        // ones narrow and fast, indexed ones in between.
        let window_size = if traits == ChannelTraits::External {
            EXTERNAL_CHANNEL_SIZE
        } else if id == CHANNEL_ID_NULL {
            INTERNAL_CHANNEL_SIZE
        } else {
            INDEXED_CHANNEL_SIZE
        };
        let is_internal = traits == ChannelTraits::Internal;

        let channel = Rc::new(Self {
            network_interface: Rc::downgrade(interface),
            addr: Cell::new(NONE),
            traits,
            id,
            version: Cell::new(0),
            creation_version: Cell::new(0),
            is_condemned: Cell::new(false),
            is_destroyed: Cell::new(false),
            has_remote_failed: Cell::new(false),
            is_anonymous: Cell::new(false),
            is_local_regular: Cell::new(true),
            is_remote_regular: Cell::new(true),
            last_received_time: Cell::new(0),
            last_reliable_send_time: Cell::new(0),
            oldest_unacked_seq: Cell::new(SEQ_NULL),
            unacked_critical_seq: Cell::new(SEQ_NULL),
            window_size,
            small_out_seq_at: Cell::new(0),
            large_out_seq_at: RefCell::new(SeqNumAllocator::new(0)),
            unreliable_in_seq_at: Cell::new(SEQ_NULL),
            unacked_packets: RefCell::new(SendWindow::new(window_size)),
            has_seen_overflow_warning: Cell::new(false),
            in_seq_at: Cell::new(0),
            buffered_receives: RefCell::new(ReceiveWindow::new(window_size)),
            num_buffered_receives: Cell::new(0),
            should_auto_switch_to_src_addr: Cell::new(false),
            push_unsent_acks_threshold: Cell::new(0),
            p_fragments: RefCell::new(None),
            acks_to_send: RefCell::new(BTreeSet::new()),
            // seqMask( smallOutSeqAt_ - 1 ) with smallOutSeqAt_ == 0.
            highest_ack: Cell::new(SEQ_MASK),
            round_trip_time: Cell::new(if is_internal {
                STAMPS_PER_SECOND / 10
            } else {
                STAMPS_PER_SECOND
            }),
            last_reliable_resend_time: Cell::new(0),
            min_inactivity_resend_delay: Cell::new(0),
            num_packets_resent: Cell::new(0),
            num_reliable_packets_sent: Cell::new(0),
            num_data_units_sent: Cell::new(0),
            num_bytes_sent: Cell::new(0),
            should_drop_next_send: Cell::new(false),
            wants_first_packet: Cell::new(false),
            send_window_warn_threshold: Cell::new(if is_internal {
                SEND_WINDOW_WARN_THRESHOLDS[0]
            } else if id != CHANNEL_ID_NULL {
                SEND_WINDOW_WARN_THRESHOLDS[1]
            } else {
                0
            }),
        });
        channel.set_address(addr);
        channel
    }

    /// `UDPChannel::get( interface, address )` (`udp_channel.cpp:171`):
    /// return the channel to `address`, claiming an existing anonymous one
    /// or creating a new internal one.
    pub fn get(interface: &Rc<NetworkInterface>, address: Address) -> Rc<UDPChannel> {
        match interface.find_channel_owned(&address) {
            Some(channel) => {
                if !channel.is_condemned.get() {
                    // Claim the anonymous channel: bring it back in sync
                    // with a normal (explicit) construction.
                    channel.set_anonymous(false);
                } else {
                    eprintln!(
                        "UDPChannel::get: Returned condemned channel to {}",
                        channel.c_str()
                    );
                }
                channel
            }
            None => {
                let channel =
                    UDPChannel::new(interface, address, ChannelTraits::Internal, CHANNEL_ID_NULL);
                eprintln!(
                    "UDPChannel::get: Claimed anonymous channel to {}",
                    channel.c_str()
                );
                channel
            }
        }
    }

    /// `setAddress( addr )` (`udp_channel.cpp:210`): move the channel to a
    /// new address, deregistering from / registering with the interface as
    /// needed (non-indexed channels only).
    fn set_address(self: &Rc<Self>, addr: Address) {
        if self.addr.get() == addr {
            return;
        }

        self.last_received_time.set(self.now());

        if self.is_indexed() {
            self.addr.set(addr);
            return;
        }

        if self.addr.get() != NONE {
            self.with_interface(|interface| {
                interface.deregister_channel(self);
            });
        }
        self.addr.set(addr);
        if addr != NONE {
            self.with_interface(|interface| {
                interface.register_channel(self);
            });
        }
    }

    /// `UDPChannel::condemn()` (`udp_channel.cpp:380`): schedule this
    /// channel for deletion once its packets are all acked. The interface's
    /// condemned collection owns the channel from here.
    pub fn condemn(self: &Rc<Self>) {
        if self.is_condemned.get() {
            eprintln!(
                "UDPChannel::condemn( {} ): Already condemned.",
                self.c_str()
            );
            return;
        }

        // Send any unsent traffic that may have accumulated here. The
        // on-channel bundle and ACK queue are BW-02-03-03/04.
        if self.has_unsent_data() {
            if self.is_established() {
                // self.send() -- on-channel send is BW-02-03-03.
            } else {
                eprintln!(
                    "UDPChannel::condemn( {} ): Unsent data was lost because channel not established",
                    self.c_str()
                );
            }
        }

        // Not actively sending anymore, so it must be marked irregular.
        // The irregular-channels collection is BW-02-03-05.
        self.is_local_regular.set(false);
        self.is_remote_regular.set(false);

        self.is_condemned.set(true);

        // This call may delete the channel.
        self.with_interface(|interface| interface.condemned_channels_add(Rc::clone(self)));
    }

    /// `Channel::destroy()` (`channel.cpp:217`): mark the channel destroyed,
    /// fail its pending requests, and deregister it. May be called while
    /// other references remain.
    pub fn destroy(self: &Rc<Self>) {
        if self.is_destroyed.get() {
            return;
        }

        // doDestroy() -- the reference's destructor calls clearState(), which
        // forgets the unacked sends and resets the window. The receive window
        // and fragments are BW-02-03-05/-06.
        self.clear_state(false);
        self.is_destroyed.set(true);

        // onChannelGone( this ): fail the requests made on this channel.
        self.with_interface(|interface| {
            if let Some(manager) = interface.request_manager() {
                let concrete = Rc::clone(self);
                let channel: Rc<dyn ChannelRef> = concrete;
                manager.cancel_requests_for_channel(&channel);
            }
        });

        // The reference's `~UDPChannel` -> `clearState` ->
        // `setAddress( NONE )` deregisters. The port makes that
        // explicit here because `Rc` drop is not an observable point.
        if !self.is_indexed() && self.addr.get() != NONE {
            self.with_interface(|interface| {
                interface.deregister_channel(self);
            });
        }
        self.addr.set(NONE);
    }

    /// `setRemoteFailed` (`udp_channel.cpp:1805`): the remote end is gone;
    /// an anonymous channel is cleaned up by the interface.
    pub fn set_remote_failed(&self) {
        self.has_remote_failed.set(true);

        // Internal channels cancel their requests (the request manager
        // does that when the channel is destroyed; the reference does it
        // here too, via cancelRequestsFor).
        if self.is_internal() {
            // cancelRequestsFor( this ) -- covered by destroy().
        }

        // If anonymous, no-one else is going to clean it up.
        if self.is_anonymous.get() {
            let addr = self.addr.get();
            self.with_interface(|interface| interface.del_anonymous_channel(&addr));
        }
    }

    /// `isAnonymous( v )` (`udp_channel.cpp:1407`): anonymity means
    /// keepalive checking and irregularity; the keepalive/irregular
    /// collections are BW-02-03-05.
    pub fn set_anonymous(&self, anonymous: bool) {
        self.is_anonymous.set(anonymous);
        if anonymous {
            self.is_local_regular.set(false);
            self.is_remote_regular.set(false);
        }
    }

    /// `addr()`.
    pub fn addr(&self) -> Address {
        self.addr.get()
    }

    /// `traits()`.
    pub fn traits(&self) -> ChannelTraits {
        self.traits
    }

    /// `isExternal()`.
    pub fn is_external(&self) -> bool {
        self.traits == ChannelTraits::External
    }

    /// `isInternal()`.
    pub fn is_internal(&self) -> bool {
        self.traits == ChannelTraits::Internal
    }

    /// `id()`.
    pub fn id(&self) -> ChannelID {
        self.id
    }

    /// `isIndexed()`.
    pub fn is_indexed(&self) -> bool {
        self.id != CHANNEL_ID_NULL
    }

    /// `version()`.
    pub fn version(&self) -> ChannelVersion {
        self.version.get()
    }

    /// `creationVersion()`.
    pub fn creation_version(&self) -> ChannelVersion {
        self.creation_version.get()
    }

    /// `isEstablished()`: the address has an ip.
    pub fn is_established(&self) -> bool {
        self.addr.get().ip != 0
    }

    /// `isCondemned()`.
    pub fn is_condemned(&self) -> bool {
        self.is_condemned.get()
    }

    /// `isDestroyed()`.
    pub fn is_destroyed(&self) -> bool {
        self.is_destroyed.get()
    }

    /// `isDead()`: condemned or destroyed.
    pub fn is_dead(&self) -> bool {
        self.is_condemned.get() || self.is_destroyed.get()
    }

    /// `isAnonymous()`.
    pub fn is_anonymous(&self) -> bool {
        self.is_anonymous.get()
    }

    /// `isOwnedByInterface()` (`udp_channel.hpp:181`): the interface owns
    /// anonymous and condemned channels.
    pub fn is_owned_by_interface(&self) -> bool {
        !self.is_destroyed.get() && (self.is_anonymous.get() || self.is_condemned.get())
    }

    /// `hasRemoteFailed()`.
    pub fn has_remote_failed(&self) -> bool {
        self.has_remote_failed.get()
    }

    /// `hasUnackedPackets()` (`udp_channel.hpp:233`): the send window has
    /// something outstanding. The window itself is BW-02-03-03/04.
    pub fn has_unacked_packets(&self) -> bool {
        self.oldest_unacked_seq.get() != SEQ_NULL
    }

    /// Test-only: force the send window to look unacked or clean, so the
    /// condemned-recycling logic can be driven before the send window lands
    /// (BW-02-03-03/04).
    #[cfg(test)]
    pub fn debug_set_unacked(&self, unacked: bool) {
        self.oldest_unacked_seq
            .set(if unacked { 1 } else { SEQ_NULL });
    }

    /// Test-only: move the send window's sequence numbers to `first`, so a
    /// fixture can exercise the wrap without allocating through it.
    #[cfg(test)]
    pub fn debug_start_out_seqs_at(&self, first: SeqNum) {
        self.small_out_seq_at.set(first);
        *self.large_out_seq_at.borrow_mut() = SeqNumAllocator::new(first);
        self.highest_ack.set(seq_mask(first.wrapping_sub(1)));
    }

    /// Test-only: set the next sequence number expected on the way in, so a
    /// fixture can drive the cumulative-ACK footer before the receive window
    /// lands (BW-02-03-05).
    #[cfg(test)]
    pub fn debug_set_in_seq(&self, seq: SeqNum) {
        self.in_seq_at.set(seq);
    }

    /// Test-only: how many ACKs are queued for the next outgoing packet.
    #[cfg(test)]
    pub fn pending_acks(&self) -> usize {
        self.acks_to_send.borrow().len()
    }

    /// Test-only: mark the remote as irregular so `checkResendTimers` uses the
    /// inactivity resend (the real detection lands with the receive window).
    #[cfg(test)]
    pub fn debug_set_remote_regular(&self, regular: bool) {
        self.is_remote_regular.set(regular);
    }

    /// Test-only: mark the local side as irregular so the receiver pushes an
    /// ACK as soon as it queues one (the real detection is BW-02-01-04).
    #[cfg(test)]
    pub fn debug_set_local_regular(&self, regular: bool) {
        self.is_local_regular.set(regular);
    }

    /// Test-only: rewind every unacked packet to the start of the clock and
    /// pin the round-trip time just above it, so the inactivity resend
    /// period is half the channel's current stamp and a scan fires
    /// deterministically without a fake clock: the age is `now`, which beats
    /// `now/2`, while the refresh from a resend leaves only the tiny
    /// inter-scan drift, which does not.
    #[cfg(test)]
    pub fn debug_make_packets_old(&self) {
        let now = self.now();
        // resendPeriod = max(roundTripTime*2, minInactivity) = now/2.
        self.round_trip_time.set(now / 4);
        for slot in self.unacked_packets.borrow_mut().slots.iter_mut().flatten() {
            slot.last_sent_time.set(0);
        }
    }

    /// `hasUnackedCriticals()` (`udp_channel.hpp:184`).
    pub fn has_unacked_criticals(&self) -> bool {
        self.unacked_critical_seq.get() != SEQ_NULL
    }

    /// `lastReceivedTime()`.
    pub fn last_received_time(&self) -> u64 {
        self.last_received_time.get()
    }

    /// `lastReliableSendTime()`.
    pub fn last_reliable_send_time(&self) -> u64 {
        self.last_reliable_send_time.get()
    }

    /// `lastReliableResendTime()`, in stamps.
    pub fn last_reliable_resend_time(&self) -> u64 {
        self.last_reliable_resend_time.get()
    }

    /// `numPacketsResent()`. The resend scan that advances it is
    /// BW-02-03-04.
    pub fn num_packets_resent(&self) -> u32 {
        self.num_packets_resent.get()
    }

    /// `numReliablePacketsSent()`. The send path that advances it is
    /// BW-02-03-07.
    pub fn num_reliable_packets_sent(&self) -> u32 {
        self.num_reliable_packets_sent.get()
    }

    /// `isLocalRegular()`.
    pub fn is_local_regular(&self) -> bool {
        self.is_local_regular.get()
    }

    /// `isRemoteRegular()`.
    pub fn is_remote_regular(&self) -> bool {
        self.is_remote_regular.get()
    }

    /// `networkInterface()`.
    pub fn network_interface(&self) -> Rc<NetworkInterface> {
        self.with_interface(Rc::clone)
    }

    /// `hasUnsentData()` (`udp_channel.cpp:645`): the on-channel bundle holds
    /// messages, data footers, or reliability. Only the queued ACKs are this
    /// slice's own state; the bundle terms are counted once the channel owns
    /// its bundle (BW-02-03-07).
    pub fn has_unsent_data(&self) -> bool {
        !self.acks_to_send.borrow().is_empty()
    }

    /// `hasAcks()` (udp_channel.hpp:176): the channel has queued ACKs for the
    /// next outgoing packet.
    pub fn has_acks(&self) -> bool {
        !self.acks_to_send.borrow().is_empty()
    }

    /// `numDataUnitsSent_` (udp_channel.cpp:785): the bundles this channel
    /// has pushed, packets apart from.
    pub fn num_data_units_sent(&self) -> u32 {
        self.num_data_units_sent.get()
    }

    /// `numBytesSent_`.
    pub fn num_bytes_sent(&self) -> u64 {
        self.num_bytes_sent.get()
    }

    /// `shouldDropNextSend( b )` (udp_channel.hpp:231): arrange for the next
    /// bundle to be dropped, which the sender turns into artificial loss.
    pub fn set_should_drop_next_send(&self, b: bool) {
        self.should_drop_next_send.set(b);
    }

    /// `wantsFirstPacket()` (udp_channel.hpp:191): a reset established channel
    /// accepts nothing but a `FLAG_CREATE_CHANNEL` packet.
    pub fn wants_first_packet(&self) -> bool {
        self.wants_first_packet.get()
    }

    /// `gotFirstPacket()` (udp_channel.hpp:192).
    pub fn got_first_packet(&self) {
        self.wants_first_packet.set(false);
    }

    /// `onPacketReceived( bytes )` (udp_channel.cpp:1844): what the channel
    /// learns about itself from a packet that reached it. The data-unit and
    /// byte receive counters belong to the interface statistics item
    /// (BW-02-01-04).
    pub fn on_packet_received(&self, _bytes: i32) {
        self.last_received_time.set(self.now());
    }

    /// `doSend( bundle )` (udp_channel.cpp:700): push a bundle through the
    /// interface's sender and count what the channel sent. The channel's own
    /// bundle is not ported yet (see [`has_unsent_data`](Self::has_unsent_data)),
    /// so every bundle here is an external one.
    pub fn send(&self, bundle: &mut UDPBundle) {
        // Don't do anything if the remote process has failed.
        if self.has_remote_failed.get() {
            if self.network_interface().is_verbose() {
                eprintln!(
                    "UDPChannel::doSend( {} ): Remote process failure. Cancelling requests.\n",
                    self.c_str()
                );
            }
            self.network_interface()
                .cancel_requests_for(bundle, Reason::ChannelLost);
            return;
        }

        // Not the channel's own bundle: no interleaved messages, and no
        // bundle primer (both seams while the channel owns no bundle).
        debug_assert!(!self.is_indexed() || bundle.is_empty());

        if self.addr().ip == 0 {
            eprintln!(
                "UDPChannel::doSend({}): no address to send to, dropping bundle\n",
                self.c_str()
            );
            self.network_interface()
                .cancel_requests_for(bundle, Reason::GeneralNetwork);
            return;
        }

        // All internal traffic must be marked as reliable by the
        // startMessage calls.
        debug_assert!(
            self.is_external() || bundle.is_empty() || bundle.is_reliable(),
            "doSend: internal bundle {} is not reliable",
            self.c_str()
        );

        // Enable artificial loss if required.
        if self.should_drop_next_send.get() {
            self.network_interface().drop_next_send();
            self.should_drop_next_send.set(false);
        }

        let interface = self.network_interface();
        interface.send(&self.addr(), bundle, Some(self));

        // Update our stats.
        self.num_data_units_sent
            .set(self.num_data_units_sent.get() + 1);
        self.num_bytes_sent
            .set(self.num_bytes_sent.get() + bundle.size() as u64);

        if bundle.is_reliable() {
            self.num_reliable_packets_sent
                .set(self.num_reliable_packets_sent.get() + 1);
        }

        // Channels that do not send regularly are added to a collection to do
        // their resend checking periodically (BW-02-01-04).

        // If the bundle that was just sent was critical, the sequence number
        // of its last packet is the new unackedCriticalSeq_.
        if bundle.is_critical()
            && let Some(first) = bundle.first_packet()
        {
            self.unacked_critical_seq.set(seq_mask(
                first
                    .borrow()
                    .seq()
                    .wrapping_add(bundle.num_data_units() as SeqNum)
                    .wrapping_sub(1),
            ));
        }
    }

    // ------------------------------------------------------------------
    // BW-02-03-03: the send window
    // ------------------------------------------------------------------

    /// `windowSize()` (udp_channel.cpp:92): the window the channel keeps, for
    /// outgoing resends and incoming reordering.
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// `getMaxOverflowPackets()` (udp_channel.hpp:423): how many packets the
    /// window may overflow by before the channel is considered lost.
    fn max_overflow_packets(&self) -> SeqNum {
        if self.is_external() {
            MAX_OVERFLOW_PACKETS[0]
        } else {
            MAX_OVERFLOW_PACKETS[if self.is_indexed() { 2 } else { 1 }]
        }
    }

    /// `isInSentWindow( seq )` (udp_channel.ipp:29): is `seq` inside the window
    /// of packets that have been sent and not acknowledged?
    pub fn is_in_sent_window(&self, seq: SeqNum) -> bool {
        self.oldest_unacked_seq.get() != SEQ_NULL
            && !seq_less_than(seq, self.oldest_unacked_seq.get())
            && seq_less_than(seq, self.small_out_seq_at.get())
    }

    /// `sendWindowUsage()` (udp_channel.hpp:237): how much of the send window
    /// is in use, overflow packets included.
    pub fn send_window_usage(&self) -> u32 {
        if self.has_unacked_packets() {
            seq_mask(
                self.large_out_seq_at
                    .borrow()
                    .next()
                    .wrapping_sub(self.oldest_unacked_seq.get()),
            )
        } else {
            0
        }
    }

    /// `numOutstandingPackets()` (udp_channel.cpp:1705). The on-channel
    /// bundle's data units are counted when the channel owns its bundle
    /// (BW-02-03-07).
    pub fn num_outstanding_packets(&self) -> u32 {
        self.send_window_usage()
    }

    /// `roundTripTime()`, in stamps.
    pub fn round_trip_time(&self) -> u64 {
        self.round_trip_time.get()
    }

    /// `inSeqAt()`: the next sequence number expected on the way in.
    pub fn in_seq_at(&self) -> SeqNum {
        self.in_seq_at.get()
    }

    /// `highestAck()`: the highest sequence number acknowledged.
    pub fn highest_ack(&self) -> SeqNum {
        self.highest_ack.get()
    }

    /// `unreliableInSeqAt()`: the last sequence number seen on an unreliable
    /// packet, `SEQ_NULL` before the first.
    pub fn unreliable_in_seq_at(&self) -> SeqNum {
        self.unreliable_in_seq_at.get()
    }

    /// `useNextSequenceID()` (udp_channel.cpp:1741): hand out the channel's
    /// next sequence number, logging when an internal channel's backlog grows
    /// past the previous high-water mark.
    pub fn use_next_sequence_id(&self) -> SeqNum {
        let seq = self.large_out_seq_at.borrow_mut().get_next();

        if self.is_internal() {
            let usage = self.send_window_usage();
            if usage > self.send_window_warn_threshold.get() {
                eprintln!(
                    "UDPChannel::useNextSequenceID( {} ): Send window backlog is now {} packets, exceeded previous max of {}",
                    self.c_str(),
                    usage,
                    self.send_window_warn_threshold.get()
                );
                self.send_window_warn_threshold.set(usage);
            }
            // The indexed send-window callback is BW-02-03-04.
        }

        seq
    }

    /// `addResendTimer( seq, p )` (udp_channel.cpp:830): record a packet that
    /// may need to be resent, growing the window as it fills.
    ///
    /// Returns `false` when the send window overflowed: the packet is queued
    /// and the oldest unacked one is pushed instead.
    ///
    /// The reliable-order range the reference records here (for external
    /// piggybacks) is the BW-02-03-04 seam.
    pub fn add_resend_timer(&self, seq: SeqNum, packet: &Rc<RefCell<Packet>>) -> bool {
        debug_assert!(
            self.oldest_unacked_seq.get() == SEQ_NULL
                || self
                    .unacked_packets
                    .borrow()
                    .get(self.oldest_unacked_seq.get())
                    .is_some()
        );
        debug_assert!(seq == packet.borrow().seq());

        let now = self.now();
        self.last_reliable_send_time.set(now);

        // If this channel has no unacked packets, record this as the oldest.
        if self.oldest_unacked_seq.get() == SEQ_NULL {
            self.oldest_unacked_seq.set(seq);
        }

        {
            let mut window = self.unacked_packets.borrow_mut();
            let oldest = self.oldest_unacked_seq.get();

            // Grow the window, if necessary.
            if seq_mask(seq.wrapping_sub(oldest).wrapping_add(1)) > window.size() as SeqNum {
                window.double_size(oldest);
                if self.verbose() {
                    eprintln!(
                        "UDPChannel::addResendTimer( {} ): Doubled send buffer size to {}",
                        self.c_str(),
                        window.size()
                    );
                }
            }

            let prev = window.get(seq);
            debug_assert!(prev.is_none());
            window.set(seq, Some(UnackedPacket::new(packet, seq, now)));
        }

        let window_full = seq_mask(
            self.large_out_seq_at
                .borrow()
                .next()
                .wrapping_sub(self.oldest_unacked_seq.get()),
        ) >= self.window_size as SeqNum;

        if window_full {
            // Make sure that we at least send occasionally.
            let small = self.small_out_seq_at.get();
            let resend_oldest = {
                let window = self.unacked_packets.borrow();
                match window.get(seq_mask(small.wrapping_sub(1))) {
                    None => true,
                    Some(previous) => {
                        now.saturating_sub(previous.last_sent_time.get())
                            > self.min_inactivity_resend_delay.get()
                    }
                }
            };
            if resend_oldest {
                self.send_unacked(small);
                self.small_out_seq_at.set(seq_mask(small.wrapping_add(1)));
            }

            self.check_overflow_errors();
            // We shouldn't send now.
            false
        } else {
            // We should send now.
            self.small_out_seq_at
                .set(self.large_out_seq_at.borrow().next());
            true
        }
    }

    /// `sendUnacked( unacked )` (udp_channel.cpp:1200): push a queued packet
    /// over the wire again and record what the channel learns from it.
    fn send_unacked(&self, out_seq: SeqNum) {
        let now = self.now();

        {
            let mut window = self.unacked_packets.borrow_mut();
            let Some(unacked) = window.get_mut(out_seq) else {
                return;
            };

            // updateChannelVersion( version_, id_ ).
            {
                let mut packet = unacked.packet.borrow_mut();
                packet.set_channel_version(self.version());
                if self.is_indexed() {
                    packet.set_channel_id(self.id());
                }
            }

            // sendPacket( addr_, pPacket_, this, isResend = true ).
            self.network_interface()
                .send_packet(&self.addr.get(), &unacked.packet, true);

            unacked
                .last_sent_at_out_seq
                .set(self.small_out_seq_at.get());
            unacked.was_resent.set(true);
            unacked.last_sent_time.set(now);
        }

        self.last_reliable_resend_time.set(now);
    }

    /// `lastReliableSendOrResendTime()` (udp_channel.hpp:275): the later of the
    /// last reliable send and the last resend, in stamps.
    pub fn last_reliable_send_or_resend_time(&self) -> u64 {
        self.last_reliable_send_time
            .get()
            .max(self.last_reliable_resend_time.get())
    }

    /// `minInactivityResendDelay( seconds )` (udp_channel.cpp:102): the
    /// minimum time, in seconds, before an irregular channel resends an
    /// unacked packet.
    pub fn set_min_inactivity_resend_delay(&self, seconds: f64) {
        self.min_inactivity_resend_delay
            .set((seconds * STAMPS_PER_SECOND as f64) as u64);
    }

    /// `checkResendTimers( bundle )` (udp_channel.cpp:1080): resend the unacked
    /// packets that have aged past the resend period, or that a newer ACK has
    /// overtaken. The piggyback half (external channels riding on the next
    /// outgoing bundle) and its `reliableOrders_` are the on-channel-bundle
    /// seam of BW-02-03-07.
    pub fn check_resend_timers(&self) {
        // There are no un-acked packets.
        if self.oldest_unacked_seq.get() == SEQ_NULL {
            return;
        }

        // Don't do anything if the remote process has failed.
        if self.has_remote_failed.get() {
            return;
        }

        // Anything that has taken more than twice the RTT to come back is
        // considered too old.
        let now = self.now();
        let resend_period =
            (self.round_trip_time.get() * 2).max(self.min_inactivity_resend_delay.get());

        let is_irregular = !self.is_remote_regular.get();
        let end_seq = if is_irregular {
            self.small_out_seq_at.get()
        } else {
            self.highest_ack.get()
        };
        // Throttle the resends a little in extreme situations.
        let max_resends = self.window_size / 8;

        let mut num_resends = 0;
        let mut seq = self.oldest_unacked_seq.get();
        while seq_less_than(seq, end_seq) && num_resends < max_resends {
            // ( lastSentAtOutSeq, lastSentTime, hasNewerAck, shouldResend )
            let (last_sent_at_out_seq, last_sent_time, has_newer_ack, should_resend) = {
                let window = self.unacked_packets.borrow();
                match window.get(seq) {
                    None => continue,
                    Some(unacked) => {
                        let last_sent_at_out_seq = unacked.last_sent_at_out_seq.get();
                        let has_newer_ack =
                            seq_less_than(last_sent_at_out_seq, self.highest_ack.get());
                        let inactivity = is_irregular
                            && now.saturating_sub(unacked.last_sent_time.get()) > resend_period;
                        (
                            last_sent_at_out_seq,
                            unacked.last_sent_time.get(),
                            has_newer_ack,
                            has_newer_ack || inactivity,
                        )
                    }
                }
            };

            if should_resend {
                self.resend(seq);
                num_resends += 1;

                if self.verbose() {
                    if has_newer_ack {
                        eprintln!(
                            "UDPChannel::checkResendTimers( {} ): Resent unacked packet due to newer ack (last sent #{}, latest ack #{})",
                            self.c_str(),
                            last_sent_at_out_seq,
                            self.highest_ack.get()
                        );
                    } else {
                        eprintln!(
                            "UDPChannel::checkResendTimers( {} ): Resent unacked packet due to inactivity ({}ms sent, RTT {}ms)",
                            self.c_str(),
                            now.saturating_sub(last_sent_time) / stamps_per_millisecond(),
                            self.round_trip_time.get() / stamps_per_millisecond()
                        );
                    }
                }
            }

            seq = seq_mask(seq.wrapping_add(1));
        }
    }

    /// `resend( seq, bundle )` (udp_channel.cpp:1181): push a packet again and
    /// count the resend. The piggyback half (which, for an external channel,
    /// rides the packet on the next outgoing bundle and releases it there) is
    /// the BW-02-03-07 seam; the port resends it directly.
    ///
    /// Returns `true` if the packet is no longer unacked -- only the piggyback
    /// path does that, so the port returns `false`.
    fn resend(&self, seq: SeqNum) -> bool {
        self.num_packets_resent
            .set(self.num_packets_resent.get() + 1);

        self.send_unacked(seq);

        false
    }

    // ------------------------------------------------------------------
    // BW-02-03-05: the receive window
    // ------------------------------------------------------------------

    /// `shouldAutoSwitchToSrcAddr( b )` (udp_channel.cpp:1476): indexed
    /// channels may follow the source address of what they receive.
    pub fn set_should_auto_switch_to_src_addr(&self, b: bool) {
        self.should_auto_switch_to_src_addr.set(b);
        assert!(!b || self.is_indexed());
    }

    /// `pushUnsentAcksThreshold( i )` (udp_channel.hpp:252).
    pub fn set_push_unsent_acks_threshold(&self, i: u32) {
        self.push_unsent_acks_threshold.set(i);
    }

    /// `maxWindowSize()` (udp_channel.hpp:433): the receive window the
    /// channel buffers, overflow packets included.
    pub fn max_window_size(&self) -> usize {
        self.window_size + self.max_overflow_packets() as usize
    }

    /// `numBufferedReceives_`, for the receiver's "still has state" checks
    /// and `clearState`.
    pub fn num_buffered_receives(&self) -> u32 {
        self.num_buffered_receives.get()
    }

    /// `pFragments()` (udp_channel.hpp:225): the partially reassembled bundle
    /// whose fragments are arriving on this channel, if any.
    pub fn p_fragments(&self) -> Option<Rc<RefCell<FragmentedBundle>>> {
        self.p_fragments.borrow().clone()
    }

    /// `pFragments( pFragments )` (udp_channel.hpp:226).
    pub fn set_p_fragments(&self, p_fragments: Option<Rc<RefCell<FragmentedBundle>>>) {
        *self.p_fragments.borrow_mut() = p_fragments;
    }

    /// `addToReceiveWindow( p, srcAddr, stats )` (udp_channel.cpp:1233): file
    /// a reliable packet in the receive window. The next-in-window packet is
    /// returned as-is with every buffered successor chained onto it; the
    /// receiver walks that chain into its bundles (the -07 seam). Returns
    /// what happened, so the receiver can ACK the early/duplicate arrivals
    /// of an irregular channel instead of dispatching them.
    pub fn add_to_receive_window(
        self: &Rc<Self>,
        p: Rc<RefCell<Packet>>,
        src_addr: Address,
        stats: &PacketReceiverStats,
    ) -> AddToReceiveWindowResult {
        let seq = p.borrow().seq();

        // Make sure the sequence number is valid.
        if seq_mask(seq) != seq {
            if self.verbose() {
                eprintln!(
                    "UDPChannel::addToReceiveWindow( {} ): Got out-of-range incoming seq #{} (inSeqAt: #{})",
                    self.c_str(),
                    seq,
                    self.in_seq_at.get()
                );
            }

            return AddToReceiveWindowResult::PacketIsCorrupt;
        }

        if self.should_auto_switch_to_src_addr.get() {
            // We switch address if the version number is acceptable. We
            // switch on equal version numbers because the first packet from a
            // cell entity sets the address and is version 0.
            let channel_version = p.borrow().channel_version();
            if !seq_less_than(channel_version, self.version()) {
                self.version.set(channel_version);
                self.set_address(src_addr);
            }
        } else if self.addr.get() != src_addr {
            if self.verbose() {
                eprintln!(
                    "UDPChannel::addToReceiveWindow( {} ): Got packet #{} from wrong source address: {}",
                    self.c_str(),
                    seq,
                    src_addr.c_str()
                );
            }

            return AddToReceiveWindowResult::PacketIsCorrupt;
        }

        if !p.borrow().is_piggyback() {
            // No need to ACK piggybacks as they are implicitly ACKed by the
            // containing packet's ACK.

            // Always add the ACKs for others even though a cumulative ack may
            // mean it is not sent. This still shows that the channel is not
            // empty.
            self.acks_to_send.borrow_mut().insert(seq);
        }

        // Push the outgoing bundle immediately if required. The `send()`
        // itself is the BW-02-03-07 wire seam.
        if self.push_unsent_acks_threshold.get() != 0
            && self.acks_to_send.borrow().len() as u32 >= self.push_unsent_acks_threshold.get()
            && self.verbose()
        {
            eprintln!(
                "UDPChannel::addToReceiveWindow( {} ): Pushing {} unsent ACKs due to inactivity",
                self.c_str(),
                self.acks_to_send.borrow().len()
            );
        }

        // Check the good case first.
        if seq == self.in_seq_at.get() {
            self.in_seq_at
                .set(seq_mask(self.in_seq_at.get().wrapping_add(1)));

            // Attach as many buffered packets as possible to this one.
            {
                let mut window = self.buffered_receives.borrow_mut();
                let mut p_prev = p.clone();
                let mut buffered_packet = window.get(self.in_seq_at.get()).cloned();

                while let Some(next) = buffered_packet {
                    // Link it to the prev packet then remove it from the
                    // buffer.
                    p_prev.borrow_mut().chain(next.clone());
                    window.set(self.in_seq_at.get(), None);
                    self.num_buffered_receives
                        .set(self.num_buffered_receives.get() - 1);

                    // Advance to the next buffered packet.
                    p_prev = next;
                    self.in_seq_at
                        .set(seq_mask(self.in_seq_at.get().wrapping_add(1)));
                    buffered_packet = window.get(self.in_seq_at.get()).cloned();
                }
            }

            return AddToReceiveWindowResult::PacketIsNextInWindow;
        }

        // See if we've got this one before. We have if seq < inSeqAt_.
        if seq_less_than(seq, self.in_seq_at.get()) {
            if self.verbose() {
                eprintln!(
                    "UDPChannel::addToReceiveWindow( {} ): Discarding already-seen packet #{} below inSeqAt #{}",
                    self.c_str(),
                    seq,
                    self.in_seq_at.get()
                );
            }

            stats.inc_duplicate_packets();

            return AddToReceiveWindowResult::PacketIsDuplicate;
        }

        // Make sure it's in range.
        let required_window_size = seq_mask(seq.wrapping_sub(self.in_seq_at.get())) as usize;

        // Note that we store packets between
        // [inSeqAt_ + 1, inSeqAt_ + bufferedReceives_.size()] in
        // bufferedReceives. This is why we have > instead of >= in the below
        // conditions.
        if required_window_size > 2 * self.buffered_receives.borrow().size()
            || required_window_size > self.max_window_size()
        {
            if self.verbose() {
                eprintln!(
                    "UDPChannel::addToReceiveWindow( {} ): Sequence number #{} is way out of window #{}!",
                    self.c_str(),
                    seq,
                    self.in_seq_at.get()
                );
            }

            return AddToReceiveWindowResult::PacketIsOutOfRange;
        } else if required_window_size > self.buffered_receives.borrow().size() {
            // Copy buffered receives from the start of our buffered receives,
            // the first starting at inSeqAt_ + 1.
            self.buffered_receives
                .borrow_mut()
                .double_size(self.in_seq_at.get().wrapping_add(1));

            if self.verbose() {
                eprintln!(
                    "UDPChannel::addToReceiveWindow( {} ): Doubled receive buffer size to {}",
                    self.c_str(),
                    self.buffered_receives.borrow().size()
                );
            }
        }

        // Ok - we'll buffer this packet then.... but only if we don't already
        // have it.
        let existing = self.buffered_receives.borrow().get(seq).cloned();
        match existing {
            Some(buffered) => {
                if buffered.borrow().seq() == seq {
                    if self.verbose() {
                        eprintln!(
                            "UDPChannel::addToReceiveWindow( {} ): Discarding already-buffered packet #{}",
                            self.c_str(),
                            seq
                        );
                    }
                    stats.inc_duplicate_packets();
                } else {
                    // CRITICAL_MSG in the reference: a slot can only hold the
                    // packet whose sequence number indexes it.
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): Packet {} in {}'s buffered receive slot",
                        self.c_str(),
                        buffered.borrow().seq(),
                        seq
                    );
                }
            }
            None => {
                self.buffered_receives.borrow_mut().set(seq, Some(p));
                self.num_buffered_receives
                    .set(self.num_buffered_receives.get() + 1);

                if self.verbose() {
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): Buffering packet #{} above #{}",
                        self.c_str(),
                        seq,
                        self.in_seq_at.get()
                    );
                }
            }
        }

        AddToReceiveWindowResult::PacketIsBufferedInWindow
    }

    /// `handleCumulativeAck( endSeq )` (udp_channel.cpp:912): everything before
    /// `endSeq` has been received by the remote end.
    pub fn handle_cumulative_ack(&self, end_seq: SeqNum) -> bool {
        if seq_mask(end_seq) != end_seq {
            if self.verbose() {
                eprintln!(
                    "UDPChannel::handleCumulativeAck( {} ): Got out-of-range endSeq #{} (outseq: #{})",
                    self.c_str(),
                    end_seq,
                    self.small_out_seq_at.get()
                );
            }
            return false;
        }

        if !self.has_unacked_packets() {
            return true;
        }

        // Check that the ACK is not in the future: endSeq is the first
        // sequence number after everything that was received.
        if seq_less_than(self.small_out_seq_at.get(), end_seq) {
            if self.is_external() {
                if self.verbose() {
                    eprintln!(
                        "UDPChannel::handleCumulativeAck( {} ): Received ACK for packets not yet sent. endSeq: #{} outSeq: #{}",
                        self.c_str(),
                        end_seq,
                        self.small_out_seq_at.get()
                    );
                }
            } else {
                eprintln!(
                    "UDPChannel::handleCumulativeAck( {} ): Received ACK for packets not yet sent. endSeq: #{} outSeq: #{}",
                    self.c_str(),
                    end_seq,
                    self.small_out_seq_at.get()
                );
            }
            return false;
        }

        let mut seq = self.oldest_unacked_seq.get();

        // Up to but not including endSeq.
        while seq_less_than(seq, end_seq) {
            self.handle_ack(seq);
            seq = seq_mask(seq.wrapping_add(1));
        }

        true
    }

    /// `handleAck( seq )` (udp_channel.cpp:977): drop one acknowledged packet
    /// from the send window.
    pub fn handle_ack(&self, seq: SeqNum) -> bool {
        debug_assert!(
            self.oldest_unacked_seq.get() == SEQ_NULL
                || self
                    .unacked_packets
                    .borrow()
                    .get(self.oldest_unacked_seq.get())
                    .is_some()
        );

        if seq_mask(seq) != seq {
            if self.verbose() {
                eprintln!(
                    "UDPChannel::handleAck( {} ): Got out-of-range seq #{} (outseq: #{})",
                    self.c_str(),
                    seq,
                    self.small_out_seq_at.get()
                );
            }
            return false;
        }

        if !self.is_in_sent_window(seq) {
            return true;
        }

        // Now make sure there's actually a packet there.
        let (was_resent, last_sent_time) = {
            let window = self.unacked_packets.borrow();
            let Some(unacked) = window.get(seq) else {
                return true;
            };
            (unacked.was_resent.get(), unacked.last_sent_time.get())
        };

        // Update the average RTT, if this packet hadn't already been resent.
        if !was_resent {
            const RTT_AVERAGE_DENOM: u64 = 10;
            self.round_trip_time.set(
                (self.round_trip_time.get() * (RTT_AVERAGE_DENOM - 1)
                    + self.now().saturating_sub(last_sent_time))
                    / RTT_AVERAGE_DENOM,
            );
        }

        // If this packet was the critical one, we're no longer in a critical
        // state.
        if self.unacked_critical_seq.get() == seq {
            self.unacked_critical_seq.set(SEQ_NULL);
        }

        // If we released the oldest unacked packet, figure out the new one.
        if seq == self.oldest_unacked_seq.get() {
            let large = self.large_out_seq_at.borrow().next();
            let mut new_oldest = SEQ_NULL;
            let mut i = seq_mask(seq.wrapping_add(1));
            while i != large {
                if self.unacked_packets.borrow().get(i).is_some() {
                    new_oldest = i;
                    break;
                }
                i = seq_mask(i.wrapping_add(1));
            }
            self.oldest_unacked_seq.set(new_oldest);
        }

        // If the incoming seq is after the last ack, then it is the new last ack.
        if seq_less_than(self.highest_ack.get(), seq) {
            self.highest_ack.set(seq);
        }

        // Now we can release the unacked packet.
        self.unacked_packets.borrow_mut().set(seq, None);

        let oldest = self.oldest_unacked_seq.get();
        while seq_mask(self.small_out_seq_at.get().wrapping_sub(oldest))
            < self.window_size as SeqNum
            && self
                .unacked_packets
                .borrow()
                .get(self.small_out_seq_at.get())
                .is_some()
        {
            let small = self.small_out_seq_at.get();
            self.send_unacked(small);
            self.small_out_seq_at.set(seq_mask(small.wrapping_add(1)));
        }

        true
    }

    /// `validateUnreliableSeqNum( seqNum )` (udp_channel.cpp:1778): does an
    /// unreliable packet's sequence number look to be newer than the last
    /// seen one, within the range that is valid?
    pub fn validate_unreliable_seq_num(&self, seq_num: SeqNum) -> bool {
        if seq_num != seq_mask(seq_num) {
            if self.verbose() {
                eprintln!(
                    "UDPChannel:validateUnreliableSeqNum: Invalid sequence number ({}).",
                    seq_num
                );
            }
            return false;
        }

        if seq_less_than(seq_num, self.unreliable_in_seq_at.get())
            && self.unreliable_in_seq_at.get() != SEQ_NULL
        {
            return false;
        }

        // Only store the new seqNum if it has been completely validated.
        self.unreliable_in_seq_at.set(seq_num);
        true
    }

    /// The `acksToSend_.insert( seq )` half of `addToReceiveWindow`
    /// (udp_channel.cpp:1271): queue an ACK for the next outgoing packet.
    pub fn add_ack(&self, seq: SeqNum) {
        self.acks_to_send.borrow_mut().insert(seq);
    }

    /// `writeFlags( p )` (udp_channel.cpp:1526): set this channel's flags on a
    /// packet that will ride on it, reserving the footers those flags need.
    pub fn write_flags(&self, p: &Rc<RefCell<Packet>>) {
        let mut packet = p.borrow_mut();
        packet.enable_flags(flags::ON_CHANNEL);

        if self.is_indexed() {
            packet.enable_flags(flags::INDEXED_CHANNEL);
            packet.set_channel_id(self.id());
            packet.set_channel_version(self.version());
            packet.reserve_footer(8); // ChannelID + ChannelVersion
        }

        // Add a cumulative ACK: all packets before a given seq have been
        // received.
        if packet.free_space() >= 4 {
            packet.enable_flags(flags::HAS_CUMULATIVE_ACK);
            packet.reserve_footer(4);

            // Go through all of them due to the wrap-around case.
            let mut acks = self.acks_to_send.borrow_mut();
            acks.retain(|ack| !seq_less_than(*ack, self.in_seq_at.get()));
        }

        // Put on as many acks as we can.
        let n_acks =
            if !self.acks_to_send.borrow().is_empty() && packet.free_space() >= (1 + 4) as isize {
                packet.enable_flags(flags::HAS_ACKS);
                packet.reserve_footer(1); // AckCount

                let min_space = (packet.free_space() / 4) as usize;
                let n_acks = min_space
                    .min(self.acks_to_send.borrow().len())
                    .min(MAX_ACKS as usize);
                packet.set_n_acks(n_acks as u8);
                packet.reserve_footer(4 * n_acks);
                Some(n_acks)
            } else {
                None
            };
        let _ = &n_acks;

        // If this is the first reliable outbound packet, flag it.
        if packet.has_flags(flags::IS_RELIABLE)
            && self.is_internal()
            && self.large_out_seq_at.borrow().next() == 0
            && self.num_reliable_packets_sent.get() == 0
        {
            packet.enable_flags(flags::CREATE_CHANNEL);
        }
    }

    /// `writeFooter( p )` (udp_channel.cpp:1593): pack the footers
    /// [`write_flags`](Self::write_flags) reserved. Call it after that.
    pub fn write_footer(&self, p: &Rc<RefCell<Packet>>) {
        let mut packet = p.borrow_mut();

        if packet.has_flags(flags::INDEXED_CHANNEL) {
            packet.pack_footer_u32(self.id() as u32);
            packet.pack_footer_u32(self.version());
        }

        if packet.has_flags(flags::HAS_CUMULATIVE_ACK) {
            packet.pack_footer_u32(self.in_seq_at.get());
        }

        if packet.has_flags(flags::HAS_ACKS) {
            // Technically we should start at inSeqAt_ since sequence numbers
            // wrap around, but this is rare enough not to worry about.
            let n_acks = packet.n_acks();
            packet.pack_footer_u8(n_acks);
            let mut acks = self.acks_to_send.borrow_mut();
            let mut acks_added = 0;
            while !acks.is_empty() && acks_added < n_acks as usize {
                let first = *acks.iter().next().unwrap();
                packet.pack_footer_u32(first);
                acks.remove(&first);
                acks_added += 1;
            }
        }
    }

    /// `clearState()` (udp_channel.cpp:250): forget the unacked sends, the
    /// buffered receives, and reset the window. The fragment chain is
    /// BW-02-03-06.
    pub fn clear_state(&self, warn_on_discard: bool) {
        if self.has_unacked_packets() {
            let num_unacked = {
                let window = self.unacked_packets.borrow();
                window.count_used()
            };
            self.unacked_packets.borrow_mut().clear();
            if warn_on_discard && num_unacked > 0 {
                eprintln!(
                    "UDPChannel::clearState( {} ): Forgetting {} unacked packet(s)",
                    self.c_str(),
                    num_unacked
                );
            }
        }

        // Clear buffered receives (udp_channel.cpp:277-297).
        if self.num_buffered_receives.get() > 0 {
            if warn_on_discard {
                eprintln!(
                    "UDPChannel::clearState( {} ): Discarding {} buffered packet(s)",
                    self.c_str(),
                    self.num_buffered_receives.get()
                );
            }

            self.buffered_receives.borrow_mut().clear();
            self.num_buffered_receives.set(0);
        }

        // Clear any chained fragments (udp_channel.cpp:301-310).
        if let Some(fragments) = self.p_fragments.take()
            && warn_on_discard
        {
            eprintln!(
                "UDPChannel::clearState( {} ): Forgetting {} unprocessed packet(s) in the fragment chain",
                self.c_str(),
                fragments.borrow().chain_length()
            );
        }

        // Reset fields.
        self.in_seq_at.set(0);
        self.small_out_seq_at.set(0);
        *self.large_out_seq_at.borrow_mut() = SeqNumAllocator::new(0);
        self.last_received_time.set(self.now());
        self.highest_ack.set(seq_mask(0u32.wrapping_sub(1)));
        self.oldest_unacked_seq.set(SEQ_NULL);
        self.last_reliable_send_time.set(0);
        self.last_reliable_resend_time.set(0);
        self.round_trip_time.set(if self.is_internal() {
            STAMPS_PER_SECOND / 10
        } else {
            STAMPS_PER_SECOND
        });
        self.unacked_critical_seq.set(SEQ_NULL);
        self.num_reliable_packets_sent.set(0);
        self.acks_to_send.borrow_mut().clear();
    }

    /// `checkOverflowErrors()` (udp_channel.cpp:421): warn when the overflow
    /// has grown to half the safety threshold, and stop warning once it
    /// shrinks below a third of it.
    fn check_overflow_errors(&self) {
        let max_overflow_packets = self.max_overflow_packets();
        let num_overflow_packets = seq_mask(
            self.large_out_seq_at
                .borrow()
                .next()
                .wrapping_sub(self.small_out_seq_at.get()),
        );

        if max_overflow_packets == 0 {
            return;
        }

        if num_overflow_packets > max_overflow_packets / 2 {
            if !self.has_seen_overflow_warning.get() {
                eprintln!(
                    "UDPChannel::checkOverflowErrors({}): Overflow packet list size ({}) exceeding safety threshold ({}).",
                    self.c_str(),
                    num_overflow_packets,
                    max_overflow_packets / 2
                );
                self.has_seen_overflow_warning.set(true);
            }
        } else if self.has_seen_overflow_warning.get()
            && num_overflow_packets < max_overflow_packets / 3
        {
            self.has_seen_overflow_warning.set(false);
        }
    }

    /// The interface's verbosity switch, as the reference's
    /// `networkInterface().isVerbose()` reads it.
    fn verbose(&self) -> bool {
        self.with_interface(|interface| interface.is_verbose())
    }

    /// Run `f` against the live interface.
    fn with_interface<R>(&self, f: impl FnOnce(&Rc<NetworkInterface>) -> R) -> R {
        let interface = self
            .network_interface
            .upgrade()
            .expect("channel used after its interface was dropped");
        f(&interface)
    }

    /// The interface's dispatcher clock, in stamps.
    fn now(&self) -> u64 {
        self.with_interface(|interface| interface.dispatcher().now())
    }
}

impl ChannelRef for UDPChannel {
    /// `c_str()` (`udp_channel.cpp`): `udp:` prefix, the address, `/id` for
    /// indexed channels, `!` when condemned.
    fn c_str(&self) -> String {
        let mut s = format!("udp:{}", self.addr.get().c_str());
        if self.is_indexed() {
            s.push_str(&format!("/{}", self.id));
        }
        if self.is_condemned.get() {
            s.push('!');
        }
        s
    }

    fn is_internal(&self) -> bool {
        UDPChannel::is_internal(self)
    }

    fn has_remote_failed(&self) -> bool {
        UDPChannel::has_remote_failed(self)
    }

    fn set_remote_failed(&self) {
        UDPChannel::set_remote_failed(self);
    }

    fn addr(&self) -> Address {
        UDPChannel::addr(self)
    }
}

impl std::fmt::Debug for UDPChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UDPChannel")
            .field("addr", &self.addr())
            .field("traits", &self.traits)
            .field("id", &self.id)
            .field("condemned", &self.is_condemned())
            .field("destroyed", &self.is_destroyed())
            .field("anonymous", &self.is_anonymous())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_interface::{InterfaceType, NetworkInterface};

    /// `127.0.0.1` as the repo stores it.
    const LOOPBACK: u32 = 0x0100_007f;

    fn address(port: u16) -> Address {
        Address::new(LOOPBACK, port)
    }

    fn interface() -> Rc<NetworkInterface> {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        interface
    }

    /// BW-02-03-02: `UDPChannel::get` returns the channel that is already
    /// registered for the address and claims an anonymous one, instead of
    /// creating a duplicate (udp_channel.cpp:171-205).
    #[test]
    fn get_claims_the_channel_registered_for_the_address() {
        let interface = interface();
        let addr = address(5000);

        let created = interface
            .find_or_create_channel(&addr, true)
            .expect("an anonymous channel is created for the address");
        assert!(created.is_anonymous());

        let claimed = UDPChannel::get(&interface, addr);
        assert!(
            Rc::ptr_eq(&claimed, &created),
            "no duplicate channel is created"
        );
        assert!(
            !claimed.is_anonymous(),
            "claiming the anonymous channel clears its anonymity"
        );
        assert!(interface.find_channel_owned(&addr).is_some());
    }

    /// With no channel registered, `get` creates an internal one and
    /// registers it (udp_channel.cpp:187-200).
    #[test]
    fn get_creates_a_channel_when_none_is_registered() {
        let interface = interface();
        let addr = address(5001);

        assert!(interface.find_channel_owned(&addr).is_none());

        let channel = UDPChannel::get(&interface, addr);
        assert_eq!(channel.addr(), addr);
        assert!(channel.is_internal());
        assert!(!channel.is_anonymous());
        assert!(
            interface
                .find_channel_owned(&addr)
                .is_some_and(|found| Rc::ptr_eq(&found, &channel)),
            "the new channel is registered"
        );
    }

    /// Registering two channels for one address is rejected, and the first
    /// registration is what the map keeps (channel_map.cpp:52-68).
    #[test]
    fn a_duplicate_registration_for_the_address_is_rejected() {
        let interface = interface();
        let addr = address(5002);

        let first = UDPChannel::new(&interface, addr, ChannelTraits::Internal, CHANNEL_ID_NULL);
        assert!(interface.find_channel_owned(&addr).is_some());

        let duplicate = UDPChannel::new(&interface, addr, ChannelTraits::Internal, CHANNEL_ID_NULL);
        assert!(
            !interface.register_channel(&duplicate),
            "the duplicate refuses"
        );

        assert!(
            interface
                .find_channel_owned(&addr)
                .is_some_and(|found| Rc::ptr_eq(&found, &first)),
            "the first channel is still the registered one"
        );
        assert!(!interface.has_unacked_packets());
    }

    /// Destroying a channel marks it destroyed, deregisters it, and leaves
    /// it with no address (channel.cpp:217, udp_channel.cpp:234-237).
    #[test]
    fn destroying_a_channel_deregisters_it() {
        let interface = interface();
        let addr = address(5003);
        let channel = UDPChannel::new(&interface, addr, ChannelTraits::Internal, CHANNEL_ID_NULL);

        channel.destroy();

        assert!(channel.is_destroyed());
        assert!(channel.is_dead());
        assert!(!channel.is_established());
        assert!(channel.addr().is_none());
        assert!(interface.find_channel_owned(&addr).is_none());

        // A second destroy has nothing left to tear down.
        channel.destroy();
    }

    /// An anonymous channel the interface owns is still listed as owned
    /// after condemnation, so the shutdown sweep destroys it
    /// (udp_channel.hpp:181, channel_map.cpp:12-34).
    #[test]
    fn condemned_and_anonymous_channels_are_owned_by_the_interface() {
        let interface = interface();
        let addr = address(5004);
        let channel = interface
            .find_or_create_channel(&addr, true)
            .expect("an anonymous channel is created for the address");
        assert!(channel.is_owned_by_interface());

        interface.destroy_owned_channels();

        assert!(channel.is_destroyed());
        assert!(interface.find_channel_owned(&addr).is_none());
    }

    /// `c_str` labels the address, the id of indexed channels, and a
    /// condemned channel.
    #[test]
    fn the_channel_label_carries_the_address_id_and_condemnation() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(5005),
            ChannelTraits::External,
            CHANNEL_ID_NULL,
        );
        let label = channel.c_str();
        assert!(label.starts_with("udp:"), "{label}");

        let indexed = UDPChannel::new(&interface, address(5006), ChannelTraits::External, 7);
        assert!(indexed.c_str().ends_with("/7"), "{}", indexed.c_str());
        assert!(!indexed.is_owned_by_interface());
    }

    /// A packet stamped with the sequence number the window remembers it
    /// under.
    fn unacked_packet(seq: SeqNum) -> Rc<RefCell<Packet>> {
        let packet = Rc::new(RefCell::new(Packet::new()));
        packet.borrow_mut().set_seq(seq);
        packet
    }

    /// Hand the window a reliable packet the way `preparePackets` does.
    fn send_reliable(channel: &UDPChannel, seq: SeqNum) -> Rc<RefCell<Packet>> {
        let packet = unacked_packet(seq);
        assert!(
            channel.add_resend_timer(seq, &packet),
            "the window takes the packet {seq}"
        );
        packet
    }

    /// BW-02-03-03: the sequence numbers the window allocates, and the ACKs
    /// that release them, are the same on both sides of the wrap to zero.
    #[test]
    fn the_send_window_holds_packets_across_the_sequence_wrap() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6001),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );
        assert_eq!(channel.window_size(), INTERNAL_CHANNEL_SIZE);

        // Start one sequence number before the wrap point, so the packets
        // straddle it.
        channel.debug_start_out_seqs_at(SEQ_MASK - 1);

        let seqs: Vec<SeqNum> = (0..4).map(|_| channel.use_next_sequence_id()).collect();
        for seq in &seqs {
            send_reliable(&channel, *seq);
        }

        // Two sequence numbers from before the wrap, two after it.
        assert_eq!(seqs, [SEQ_MASK - 1, SEQ_MASK, 0, 1]);
        assert_eq!(seqs[2], 0, "the sequence number wrapped through zero");

        // Every packet sits in the window, before and after the wrap.
        for seq in &seqs {
            assert!(channel.is_in_sent_window(*seq), "{seq} is in the window");
        }
        assert!(!channel.is_in_sent_window(2), "not handed out yet");
        assert_eq!(channel.send_window_usage(), 4);
        assert_eq!(channel.num_outstanding_packets(), 4);

        // A cumulative ACK up to 2 releases all of them, across the wrap.
        assert!(channel.handle_cumulative_ack(2));
        assert!(
            !channel.has_unacked_packets(),
            "the window is empty after the wrap"
        );
        assert_eq!(channel.send_window_usage(), 0);
        assert_eq!(channel.highest_ack(), 1, "the highest ack is the last one");
    }

    /// Acks released one at a time pull the window in from its oldest
    /// packet, and the empty-window drain stops instead of running off the
    /// sequence space.
    #[test]
    fn individual_acks_release_the_oldest_packet_across_the_wrap() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6002),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );
        channel.debug_start_out_seqs_at(SEQ_MASK - 1);

        let seqs: Vec<SeqNum> = (0..4).map(|_| channel.use_next_sequence_id()).collect();
        for seq in &seqs {
            send_reliable(&channel, *seq);
        }

        for (index, seq) in seqs.iter().enumerate() {
            assert!(channel.handle_ack(*seq), "ack #{index} releases {seq}");
            if index + 1 < seqs.len() {
                assert_eq!(
                    channel.send_window_usage(),
                    (seqs.len() - index - 1) as u32,
                    "one packet left the window"
                );
            }
        }
        assert!(!channel.has_unacked_packets());
    }

    /// The window is full once `window_size` packets are outstanding: the
    /// one that would make `largeOutSeqAt_ - oldest` reach `window_size` is
    /// refused (the reference keeps it queued and pushes the oldest instead).
    #[test]
    fn an_overflowing_send_window_turns_the_next_packet_away() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6003),
            ChannelTraits::External,
            CHANNEL_ID_NULL,
        );
        assert_eq!(channel.window_size(), EXTERNAL_CHANNEL_SIZE);

        for index in 0..EXTERNAL_CHANNEL_SIZE {
            let seq = channel.use_next_sequence_id();
            assert_eq!(seq, index as SeqNum, "the window hands out in order");
            let packet = unacked_packet(seq);
            let accepted = channel.add_resend_timer(seq, &packet);
            if index + 1 < EXTERNAL_CHANNEL_SIZE {
                assert!(accepted, "packet {index} fits");
            } else {
                assert!(
                    !accepted,
                    "the window-size-th packet is the one that overflows"
                );
            }
        }

        // The window holds every packet, the overflowing one counted too.
        assert_eq!(channel.send_window_usage(), EXTERNAL_CHANNEL_SIZE as u32);
        assert!(channel.has_unacked_packets());
    }

    /// The window sizes are pinned by channel type, and are powers of two so
    /// the window can be indexed by sequence number.
    #[test]
    fn the_window_size_is_pinned_by_channel_type() {
        let interface = interface();
        let external = UDPChannel::new(
            &interface,
            address(6004),
            ChannelTraits::External,
            CHANNEL_ID_NULL,
        );
        let internal = UDPChannel::new(
            &interface,
            address(6005),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );
        // Indexed channels are internal in practice, so they get the indexed
        // size; an indexed channel that is external stays at the external size
        // (udp_channel.cpp:92).
        let indexed = UDPChannel::new(&interface, address(6006), ChannelTraits::Internal, 42);

        assert_eq!(external.window_size(), EXTERNAL_CHANNEL_SIZE);
        assert_eq!(internal.window_size(), INTERNAL_CHANNEL_SIZE);
        assert_eq!(indexed.window_size(), INDEXED_CHANNEL_SIZE);
        assert!(external.window_size().is_power_of_two());
        assert!(internal.window_size().is_power_of_two());
        assert!(indexed.window_size().is_power_of_two());
    }

    /// `writeFlags`/`writeFooter` pack the cumulative ack and the individual
    /// acks that were queued, pruning anything already behind the next
    /// sequence number expected.
    #[test]
    fn the_ack_footer_carries_the_cumulative_ack_and_the_ack_list() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6007),
            ChannelTraits::External,
            CHANNEL_ID_NULL,
        );

        // The receive window that advances this is BW-02-03-05.
        channel.debug_set_in_seq(7);
        channel.add_ack(5); // behind the cumulative ack: pruned.
        channel.add_ack(7);
        channel.add_ack(9);

        // Eight bytes of message data, so the footer has something to sit
        // above and the read-back has a body to check against.
        let packet = Rc::new(RefCell::new(Packet::new()));
        packet.borrow_mut().grow(8);

        channel.write_flags(&packet);

        // Cumulative ack, ack count, and the two acks that survived.
        let footer_size = packet.borrow().footer_size();
        assert_eq!(footer_size, 4 + 1 + 2 * 4, "the footers are reserved");
        assert!(packet.borrow().has_flags(flags::ON_CHANNEL));
        assert!(packet.borrow().has_flags(flags::HAS_CUMULATIVE_ACK));
        assert!(packet.borrow().has_flags(flags::HAS_ACKS));
        assert_eq!(packet.borrow().n_acks(), 2);

        // The reference grows to the reserved footers and packs into them.
        packet.borrow_mut().grow(footer_size);
        channel.write_footer(&packet);

        // The footer reads back the way it was packed. Packing moved the
        // write position back to the message end, so step to the top of the
        // footer again to read it.
        packet.borrow_mut().grow(footer_size);
        let mut packet = packet.borrow_mut();
        assert_eq!(packet.strip_footer_u32(), Some(7), "the cumulative ack");
        assert_eq!(packet.strip_footer_u8(), Some(2), "the ack count");
        assert_eq!(packet.strip_footer_u32(), Some(7), "the first ack");
        assert_eq!(packet.strip_footer_u32(), Some(9), "the second ack");
        drop(packet);
        assert_eq!(channel.pending_acks(), 0, "the ACKs were sent");
    }

    /// An unreliable packet's sequence number has to be in range and newer
    /// than the last one seen; anything older, or out of range, is refused
    /// without moving the window.
    #[test]
    fn validate_unreliable_seq_num_accepts_in_order_and_rejects_stale() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6008),
            ChannelTraits::External,
            CHANNEL_ID_NULL,
        );
        assert_eq!(channel.unreliable_in_seq_at(), SEQ_NULL);

        // The first unreliable packet is always new.
        assert!(channel.validate_unreliable_seq_num(10));
        assert!(channel.validate_unreliable_seq_num(11));
        assert_eq!(channel.unreliable_in_seq_at(), 11);

        // A packet from before the last one seen is not new.
        assert!(!channel.validate_unreliable_seq_num(9));
        assert_eq!(
            channel.unreliable_in_seq_at(),
            11,
            "the stale one is refused"
        );

        // An out-of-range number is refused and does not move the window.
        assert!(!channel.validate_unreliable_seq_num(SEQ_NULL));
        assert_eq!(channel.unreliable_in_seq_at(), 11);
    }

    /// `clearState` forgets the unacked sends and resets the window back to
    /// how the constructor left it.
    #[test]
    fn clear_state_forgets_the_unacked_sends() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6009),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );
        send_reliable(&channel, channel.use_next_sequence_id());
        channel.add_ack(4);

        channel.clear_state(false);

        assert!(!channel.has_unacked_packets());
        assert_eq!(channel.send_window_usage(), 0);
        assert_eq!(channel.pending_acks(), 0);
        assert_eq!(
            channel.highest_ack(),
            SEQ_MASK,
            "the window is back at the top"
        );
        assert_eq!(channel.round_trip_time(), STAMPS_PER_SECOND / 10);
    }

    /// `pFragments_` holds the channel's half-reassembled bundle, and
    /// `clearState` lets it go when the remote side restarts.
    #[test]
    fn clear_state_forgets_the_fragment_chain() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6009),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );
        assert!(channel.p_fragments().is_none());

        let first = Packet::fragment_for_test(10, 12, 11, true);
        let fragments = Rc::new(RefCell::new(FragmentedBundle::new(&first, 0)));
        fragments.borrow_mut().add_packet(
            Packet::fragment_for_test(10, 12, 12, true),
            false,
            "test",
            0,
        );
        channel.set_p_fragments(Some(Rc::clone(&fragments)));

        let held = channel.p_fragments().expect("the channel holds the bundle");
        assert_eq!(held.borrow().chain_length(), 2);
        assert!(!held.borrow().is_complete());

        channel.clear_state(true);

        assert!(channel.p_fragments().is_none());
    }

    /// The window doubles as packets fill it, carrying the packets it holds
    /// to their new slots.
    #[test]
    fn the_send_window_doubles_as_it_fills() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6010),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );
        // Start near the end of the space so the growth has to carry the
        // packets over the wrap too.
        channel.debug_start_out_seqs_at(SEQ_MASK - 1);

        let mut seqs = Vec::new();
        for _ in 0..500 {
            let seq = channel.use_next_sequence_id();
            send_reliable(&channel, seq);
            seqs.push(seq);
        }

        // Every packet is still in the window, across the wrap and the
        // doubling.
        for seq in &seqs {
            assert!(channel.is_in_sent_window(*seq), "{seq} survived");
        }
        assert_eq!(channel.send_window_usage(), 500);
    }

    /// BW-02-03-04: when a newer ACK overtakes an older unacked packet, the
    /// resend scan pushes the older one again -- but only once, because the
    /// resend records the newer out-sequence it went out under.
    #[test]
    fn a_packet_overtaken_by_a_newer_ack_is_resent_once() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6101),
            ChannelTraits::External,
            CHANNEL_ID_NULL,
        );

        let seq0 = channel.use_next_sequence_id();
        let seq1 = channel.use_next_sequence_id();
        send_reliable(&channel, seq0);
        send_reliable(&channel, seq1);

        // Ack the newer packet only; the older one is left behind, but the
        // highest ack is now past it.
        assert!(channel.handle_ack(seq1), "acking the newer packet succeeds");
        assert!(
            channel.has_unacked_packets(),
            "the older packet is still outstanding"
        );
        assert_eq!(channel.highest_ack(), seq1);

        channel.check_resend_timers();
        assert_eq!(
            channel.num_packets_resent(),
            1,
            "the overtaken packet is resent once"
        );

        // A second scan has nothing new to resend: the resend went out under
        // the current out-sequence, which is not behind the highest ack.
        channel.check_resend_timers();
        assert_eq!(
            channel.num_packets_resent(),
            1,
            "no double resend on a fresh scan"
        );
    }

    /// BW-02-03-04: an irregular channel resends packets that have sat for
    /// longer than the resend period, which is at least twice the RTT. A
    /// regular channel does not resend on age alone.
    #[test]
    fn an_irregular_channel_resends_after_the_inactivity_period() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6102),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );
        assert_eq!(channel.round_trip_time(), STAMPS_PER_SECOND / 10);

        let seq0 = channel.use_next_sequence_id();
        let seq1 = channel.use_next_sequence_id();
        send_reliable(&channel, seq0);
        send_reliable(&channel, seq1);

        // Rewind both packets to the start of the clock, with the RTT pinned
        // so the resend period is half the current stamp (see the helper).
        channel.debug_make_packets_old();

        channel.debug_set_remote_regular(false);
        channel.check_resend_timers();
        assert_eq!(
            channel.num_packets_resent(),
            2,
            "both aged packets are resent"
        );

        // The resends refreshed the send times, so a fresh scan is quiet.
        channel.check_resend_timers();
        assert_eq!(
            channel.num_packets_resent(),
            2,
            "no inactivity resend on a fresh scan"
        );
    }

    /// BW-02-03-04: age alone does not trigger a resend on a regular channel;
    /// only the newer-ack rule applies there.
    #[test]
    fn a_regular_channel_ignores_age() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6103),
            ChannelTraits::External,
            CHANNEL_ID_NULL,
        );

        let seq0 = channel.use_next_sequence_id();
        send_reliable(&channel, seq0);
        channel.debug_make_packets_old();

        channel.check_resend_timers();
        assert_eq!(
            channel.num_packets_resent(),
            0,
            "a regular channel does not resend on age alone"
        );
    }

    /// BW-02-03-04: the resend scan gives up once a channel has failed.
    #[test]
    fn a_failed_remote_is_not_resent() {
        let interface = interface();
        let channel = UDPChannel::new(
            &interface,
            address(6104),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );

        let seq0 = channel.use_next_sequence_id();
        send_reliable(&channel, seq0);
        channel.debug_make_packets_old();
        channel.debug_set_remote_regular(false);
        channel.set_remote_failed();

        channel.check_resend_timers();
        assert_eq!(channel.num_packets_resent(), 0);
    }

    /// A received reliable packet, as `processFilteredPacket` hands it to the
    /// receive window.
    fn received_packet(seq: SeqNum) -> Rc<RefCell<Packet>> {
        unacked_packet(seq)
    }

    /// BW-02-03-05: a packet that arrives ahead of its sequence number is
    /// buffered, and when the gap closes the next-in-window packet carries
    /// it -- chained on in the original order -- so the receiver dispatches
    /// the sequence as it was sent (udp_channel.cpp:1300-1322). A far-early
    /// packet waits out its own gap instead of jumping the queue.
    #[test]
    fn an_early_packet_is_chained_on_in_the_original_order() {
        let interface = interface();
        let addr = address(6201);
        // External: window 256, so a packet 400 slots ahead forces a doubling.
        let channel = UDPChannel::new(&interface, addr, ChannelTraits::External, CHANNEL_ID_NULL);
        let stats = PacketReceiverStats::new();

        // Deliver seq 2 and seq 400 while 0 and 1 are still missing.
        let p2 = received_packet(2);
        assert_eq!(
            channel.add_to_receive_window(p2.clone(), addr, &stats),
            AddToReceiveWindowResult::PacketIsBufferedInWindow
        );
        assert_eq!(channel.num_buffered_receives(), 1);

        let p400 = received_packet(400);
        assert_eq!(
            channel.add_to_receive_window(p400.clone(), addr, &stats),
            AddToReceiveWindowResult::PacketIsBufferedInWindow
        );
        assert_eq!(channel.num_buffered_receives(), 2);
        // requiredWindowSize(400) > 256 forced a doubling of the window.
        assert_eq!(channel.buffered_receives.borrow().size(), 512);

        // The gap closes with seq 0, then 1.
        let p0 = received_packet(0);
        assert_eq!(
            channel.add_to_receive_window(p0.clone(), addr, &stats),
            AddToReceiveWindowResult::PacketIsNextInWindow
        );
        assert_eq!(
            channel.num_buffered_receives(),
            2,
            "the early packets stay queued"
        );
        assert_eq!(channel.in_seq_at(), 1);

        // seq 1 is next-in-window and picks up the consecutively buffered 2.
        let p1 = received_packet(1);
        assert_eq!(
            channel.add_to_receive_window(p1.clone(), addr, &stats),
            AddToReceiveWindowResult::PacketIsNextInWindow
        );
        assert_eq!(channel.in_seq_at(), 3);
        // The original order: 1 goes out, then 2 chained onto it.
        assert_eq!(
            p1.borrow().chain_length(),
            2,
            "1 followed by its successor 2"
        );
        assert_eq!(channel.num_buffered_receives(), 1, "400 is still early");
        assert_eq!(p0.borrow().chain_length(), 1, "0 went out on its own");

        // The early arrivals queued an ACK for the next outgoing bundle.
        assert!(channel.pending_acks() >= 1);
    }

    /// BW-02-03-05: a packet at or below `inSeqAt_` has already been seen and
    /// is filtered out as a duplicate instead of being dispatched again
    /// (udp_channel.cpp:1325-1338).
    #[test]
    fn an_already_seen_packet_is_filtered_as_a_duplicate() {
        let interface = interface();
        let addr = address(6202);
        let channel = UDPChannel::new(&interface, addr, ChannelTraits::Internal, CHANNEL_ID_NULL);
        let stats = PacketReceiverStats::new();

        let p0 = received_packet(0);
        assert_eq!(
            channel.add_to_receive_window(p0.clone(), addr, &stats),
            AddToReceiveWindowResult::PacketIsNextInWindow
        );

        // The same packet again (e.g. the sender resent it after losing our
        // ACK): below inSeqAt_, already seen.
        let p0_again = received_packet(0);
        assert_eq!(
            channel.add_to_receive_window(p0_again, addr, &stats),
            AddToReceiveWindowResult::PacketIsDuplicate
        );
        assert_eq!(stats.num_duplicate_packets_received(), 1);
        assert_eq!(channel.num_buffered_receives(), 0);

        // The resent packet does not go out a second time (chain_length
        // counts the packet itself).
        assert_eq!(p0.borrow().chain_length(), 1);

        // A repeat of a buffered (early) packet is a duplicate too.
        let p2 = received_packet(2);
        assert_eq!(
            channel.add_to_receive_window(p2.clone(), addr, &stats),
            AddToReceiveWindowResult::PacketIsBufferedInWindow
        );
        let p2_again = received_packet(2);
        assert_eq!(
            channel.add_to_receive_window(p2_again, addr, &stats),
            AddToReceiveWindowResult::PacketIsBufferedInWindow
        );
        assert_eq!(stats.num_duplicate_packets_received(), 2);
        assert_eq!(channel.num_buffered_receives(), 1, "still just one slot");
    }

    /// BW-02-03-05: a sequence number far ahead of `inSeqAt_` is unusable and
    /// refused outright, buffered nowhere (udp_channel.cpp:1340-1359).
    #[test]
    fn a_far_early_packet_is_out_of_the_receive_window() {
        let interface = interface();
        let addr = address(6203);
        // External: window 256, overflow 1024, so anything past 1280 slots
        // ahead is way out of window.
        let channel = UDPChannel::new(&interface, addr, ChannelTraits::External, CHANNEL_ID_NULL);
        assert_eq!(channel.max_window_size(), 256 + 1024);
        let stats = PacketReceiverStats::new();

        let p = received_packet(2000);
        assert_eq!(
            channel.add_to_receive_window(p, addr, &stats),
            AddToReceiveWindowResult::PacketIsOutOfRange
        );
        assert_eq!(channel.num_buffered_receives(), 0);
    }

    /// BW-02-03-05: a packet from the wrong source address is corrupt, and
    /// `clearState` discards whatever the window still holds
    /// (udp_channel.cpp:1263-1274, 277-297).
    #[test]
    fn a_foreign_source_is_rejected_and_clear_state_empties_the_window() {
        let interface = interface();
        let addr = address(6204);
        let channel = UDPChannel::new(&interface, addr, ChannelTraits::Internal, CHANNEL_ID_NULL);
        let stats = PacketReceiverStats::new();

        let p = received_packet(5);
        assert_eq!(
            channel.add_to_receive_window(p, address(6299), &stats),
            AddToReceiveWindowResult::PacketIsCorrupt
        );

        let early = received_packet(7);
        assert_eq!(
            channel.add_to_receive_window(early, addr, &stats),
            AddToReceiveWindowResult::PacketIsBufferedInWindow
        );
        assert_eq!(channel.num_buffered_receives(), 1);

        channel.clear_state(false);
        assert_eq!(channel.num_buffered_receives(), 0);
        assert_eq!(channel.in_seq_at(), 0);
    }
}
