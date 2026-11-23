//! `lib/network/udp_channel.cpp` -- the registration-and-lifecycle slice
//! of `Mercury::UDPChannel` (BW-02-03-02) plus the send window and its acks
//! (BW-02-03-03) and the resend timing (BW-02-03-04): construction
//! registers a non-indexed channel with its interface, `condemn` hands it
//! to the condemned list and marks it irregular, `destroy` closes it for
//! good, `addResendTimer`/`handleAck` keep the unacked ring honest across
//! the `SEQ_SIZE` seam, and `checkResendTimers` gives aged or NACKed
//! packets their resend. The receive-window filtering (BW-02-03-05)
//! carries its fields from the reference constructor but stays inert
//! until its task lands.
//!
//! The reference's `Channel` base (`channel.hpp`) is folded in: its state
//! is the leading fields and its `destroy`/inactivity flow is the
//! `destroy`/`start_inactivity_detection` pair here.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};

use foundation::time_queue::{TimerHandle, TimerHandler};

use crate::channel_listener::ChannelListener;
use crate::circular_array::CircularArray;
use crate::endpoint::{Address, NONE};
use crate::fragmented_bundle::FragmentedBundle;
use crate::misc::{
    CHANNEL_ID_NULL, ChannelID, ChannelVersion, SEQ_NULL, SeqNum, seq_less_than, seq_mask,
};
use crate::network_interface::{ChannelRef, NetworkInterface};
use crate::packet::{Packet, flags};
use crate::packet_filter::PacketFilter;
use crate::reliable_order::ReliableOrder;
use crate::udp_bundle::UdpBundle;
use crate::unacked_packet::UnackedPacket;

/// `s_maxOverflowPackets_` (udp_channel.cpp:34-38): how many *unsent*
/// packets may pile up past the send window before the channel starts
/// complaining -- external, internal, indexed.
const MAX_OVERFLOW_PACKETS: [usize; 3] = [1024, 8192, 4096];

/// `DEFAULT_INACTIVITY_RESEND_DELAY` (udp_channel.hpp).
pub const DEFAULT_INACTIVITY_RESEND_DELAY: f32 = 1.0;

/// `UDPChannel::Traits` (udp_channel.hpp:67): what kind of link this is,
/// which picks the reliability method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Traits {
    /// Server-to-server: low latency, high bandwidth, low loss.
    Internal,
    /// Client-to-server: high latency, low bandwidth, high loss.
    External,
}

/// `UDPChannel::AddToReceiveWindowResult` (udp_channel.hpp:161): what the
/// receive window made of one reliable incoming packet.
pub enum AddToReceiveWindowResult {
    /// `PACKET_IS_NEXT_IN_WINDOW`: the packet was the next expected one.
    /// The reference chains whatever buffered successors now follow onto
    /// it; the port hands the chain back in the original send order.
    NextInWindow(Vec<Rc<Packet>>),
    /// `PACKET_IS_BUFFERED_IN_WINDOW`: a future packet, parked until its
    /// predecessors arrive.
    BufferedInWindow,
    /// `PACKET_IS_DUPLICATE`: already seen below `inSeqAt_`, discarded.
    Duplicate,
    /// `PACKET_IS_OUT_OF_WINDOW`: too far in the future to even buffer.
    OutOfWindow,
    /// `PACKET_IS_CORRUPT`.
    Corrupt,
}

/// `EXTERNAL_CHANNEL_SIZE` (udp_channel.cpp:27).
pub const EXTERNAL_CHANNEL_SIZE: usize = 256;

/// `INTERNAL_CHANNEL_SIZE` (udp_channel.cpp:28).
pub const INTERNAL_CHANNEL_SIZE: usize = 4096;

/// `INDEXED_CHANNEL_SIZE` (udp_channel.cpp:29).
pub const INDEXED_CHANNEL_SIZE: usize = 512;

/// The window size the constructor picks: external channels are small
/// because client links are scarce on bandwidth, indexed internal ones
/// half the internal window.
fn window_size(traits: Traits, id: ChannelID) -> usize {
    match traits {
        Traits::External => EXTERNAL_CHANNEL_SIZE,
        Traits::Internal if id == CHANNEL_ID_NULL => INTERNAL_CHANNEL_SIZE,
        Traits::Internal => INDEXED_CHANNEL_SIZE,
    }
}

/// `SmartPointer< Channel > ChannelPtr` (channel.hpp): the shared handle
/// every collection holds. The reference deletes the channel when its
/// count hits zero; the port drops it when the last strong reference
/// goes, which is why [`UdpChannel::destroy`] deregisters eagerly where
/// the reference deregisters from the destructor.
pub type ChannelPtr = Rc<RefCell<UdpChannel>>;

/// `Mercury::UDPChannel` (BW-02-03-02 slice).
pub struct UdpChannel {
    // -- Channel base (channel.hpp) ------------------------------------
    /// `addr_`: `Address::NONE` until [`set_address`](Self::set_address_fn)
    /// registers us.
    addr: Address,
    /// `pBundle_`.
    bundle: UdpBundle,
    /// `isDestroyed_`.
    is_destroyed: bool,
    /// `inactivityTimerHandle_`.
    inactivity_timer: TimerHandle<u64>,
    /// The `InactivityTimeoutChecker`'s `inactivityExceptionPeriod_`, in
    /// micros (the port's stamps are micros).
    inactivity_exception_period: u64,
    /// `lastReceivedTime_`, in micros.
    last_received_time: u64,
    /// `pListener_`.
    listener: Option<Rc<dyn ChannelListener>>,
    /// `numDataUnitsSent_`.
    num_data_units_sent: u32,
    /// `numDataUnitsReceived_`.
    num_data_units_received: u32,
    /// `numBytesSent_`.
    num_bytes_sent: u32,
    /// `numBytesReceived_`.
    num_bytes_received: u32,

    // -- UDPChannel ------------------------------------------------------
    /// `traits_`.
    traits: Traits,
    /// `id_`: the indexed-channel id, or `CHANNEL_ID_NULL`.
    id: ChannelID,
    /// `version_`.
    version: ChannelVersion,
    /// `creationVersion_`.
    creation_version: ChannelVersion,
    /// `windowSize_`.
    window_size: usize,
    /// `smallOutSeqAt_` (BW-02-03-03).
    small_out_seq_at: SeqNum,
    /// `largeOutSeqAt_` (BW-02-03-03).
    large_out_seq_at: SeqNum,
    /// `oldestUnackedSeq_` (BW-02-03-03): `SEQ_NULL` while every sent
    /// packet is acknowledged.
    oldest_unacked_seq: SeqNum,
    /// `highestAck_` = `seqMask( smallOutSeqAt_ - 1 )`.
    highest_ack: SeqNum,
    /// `inSeqAt_` (BW-02-03-05).
    in_seq_at: SeqNum,
    /// `unreliableInSeqAt_` (BW-02-03-05).
    unreliable_in_seq_at: SeqNum,
    /// `lastReliableSendTime_`.
    last_reliable_send_time: u64,
    /// `roundTripTime_`, in micros.
    round_trip_time: u64,
    /// `minInactivityResendDelay_`, in micros.
    min_inactivity_resend_delay: u64,
    /// `isCondemned_`: detached from its owner, waiting to be deleted.
    is_condemned: bool,
    /// `hasRemoteFailed_`.
    has_remote_failed: bool,
    /// `isAnonymous_`.
    is_anonymous: bool,
    /// `isLocalRegular_` (BW-02-03-04): whether this channel's sends are
    /// paced by a game tick. Irregular channels are resend-checked by the
    /// interface's shared timer.
    is_local_regular: bool,
    /// `isRemoteRegular_` (BW-02-03-04): whether the remote side sends
    /// regularly; if it does, resends go by NACK rather than by the timer.
    is_remote_regular: bool,

    /// `unackedPackets_` (udp_channel.hpp:415): the sent-but-unacked ring,
    /// addressed by sequence number.
    unacked_packets: CircularArray<Option<UnackedPacket>>,
    /// `unackedCriticalSeq_` (udp_channel.hpp:448).
    unacked_critical_seq: SeqNum,
    /// `hasSeenOverflowWarning_`.
    has_seen_overflow_warning: bool,
    /// `lastReliableResendTime_`, in micros.
    last_reliable_resend_time: u64,
    /// `numPacketsResent_` (BW-02-03-04).
    num_packets_resent: u32,
    /// `numReliablePacketsSent_`: the reliable bundles sent on this
    /// channel, read by [`write_flags`](Self::write_flags) to flag the
    /// first one `CREATE_CHANNEL`.
    num_reliable_packets_sent: u32,
    /// `bufferedReceives_` (udp_channel.hpp:444, BW-02-03-05): reliable
    /// packets that arrived ahead of their predecessors, parked by
    /// sequence number until the hole fills.
    buffered_receives: CircularArray<Option<Rc<Packet>>>,
    /// `numBufferedReceives_`.
    num_buffered_receives: u32,
    /// `acksToSend_` (udp_channel.hpp:174, BW-02-03-05): the seqs the
    /// next send must acknowledge, ordered set like the reference's
    /// `BW::set`.
    acks_to_send: BTreeSet<SeqNum>,
    /// `shouldAutoSwitchToSrcAddr_`: an indexed channel may re-address
    /// itself from incoming packets' source addresses.
    should_auto_switch_to_src_addr: bool,
    /// The duplicates the reference counts into
    /// `PacketReceiverStats::numDuplicatePackets_`; the stats struct is
    /// not ported yet, so the channel keeps the tally (BW-02-03-05).
    num_duplicate_packets: u32,
    /// `pFragments_` (udp_channel.hpp:436): the fragment chain being
    /// reassembled on this channel, wired by the receive dispatch
    /// (BW-02-03-07). `None` while no fragment bundle is in flight.
    pub(crate) fragments: Option<FragmentedBundle>,
    /// `wantsFirstPacket_`: a re-addressed established channel refuses
    /// traffic until a `FLAG_CREATE_CHANNEL` packet arrives.
    wants_first_packet: bool,
    /// `pFilter_` (udp_channel.hpp:381, BW-02-05-01): this channel's
    /// packet filter, consulted before the header walk on the way in and
    /// before the socket on the way out.
    p_filter: Option<Rc<dyn PacketFilter>>,

    /// The interface this channel registers with. Weak so the map's
    /// strong references do not keep the interface alive.
    network_interface: Weak<NetworkInterface>,
}

impl UdpChannel {
    /// `UDPChannel( networkInterface, addr, traits, minInactivityResendDelay, id )`
    /// (udp_channel.cpp:82): initialise the window fields from the traits
    /// and id, open the bundle, and register non-indexed channels through
    /// [`set_address`](Self::set_address_fn). The constructor takes no
    /// filter, so the reference's `pFilter_ && id_ != CHANNEL_ID_NULL`
    /// critical (udp_channel.cpp:129-133) stays unrepresentable here; a
    /// filter is attached later with [`set_p_filter`](Self::set_p_filter),
    /// which like the reference's setter does not check indexed-ness.
    pub fn new(
        interface: &Rc<NetworkInterface>,
        addr: Address,
        traits: Traits,
        min_inactivity_resend_delay: f32,
        id: ChannelID,
    ) -> ChannelPtr {
        // `unackedPackets_( windowSize_ )` (udp_channel.cpp:105): the ring
        // opens at the window size, which is a power of two by construction.
        let size = window_size(traits, id);
        let channel = Rc::new(RefCell::new(UdpChannel {
            addr: NONE,
            bundle: UdpBundle::new(0),
            is_destroyed: false,
            inactivity_timer: TimerHandle::default(),
            inactivity_exception_period: 0,
            last_received_time: 0,
            listener: None,
            num_data_units_sent: 0,
            num_data_units_received: 0,
            num_bytes_sent: 0,
            num_bytes_received: 0,
            traits,
            id,
            version: 0,
            creation_version: 0,
            window_size: size,
            unacked_packets: CircularArray::new(size),
            small_out_seq_at: 0,
            large_out_seq_at: 0,
            oldest_unacked_seq: SEQ_NULL,
            highest_ack: seq_mask(0u32.wrapping_sub(1)),
            in_seq_at: 0,
            unreliable_in_seq_at: SEQ_NULL,
            last_reliable_send_time: 0,
            round_trip_time: if traits == Traits::Internal {
                100_000
            } else {
                1_000_000
            },
            min_inactivity_resend_delay: (min_inactivity_resend_delay * 1_000_000.0) as u64,
            is_condemned: false,
            has_remote_failed: false,
            is_anonymous: false,
            is_local_regular: true,
            is_remote_regular: true,
            unacked_critical_seq: SEQ_NULL,
            has_seen_overflow_warning: false,
            last_reliable_resend_time: 0,
            num_packets_resent: 0,
            num_reliable_packets_sent: 0,
            // `bufferedReceives_( windowSize_ )` (udp_channel.cpp:108).
            buffered_receives: CircularArray::new(size),
            num_buffered_receives: 0,
            acks_to_send: BTreeSet::new(),
            should_auto_switch_to_src_addr: false,
            num_duplicate_packets: 0,
            fragments: None,
            wants_first_packet: false,
            p_filter: None,
            network_interface: Rc::downgrade(interface),
        }));

        // This corresponds to the decRef in Channel::destroy: the port's
        // lifetime bookkeeping is the Rc, nothing to balance here.

        // The bundle's reliable-order recording runs only on an external
        // channel (the segments tell a resend what to carry to a peer),
        // so tag the bundle with the trait up front.
        channel
            .borrow_mut()
            .bundle
            .set_external_channel(traits == Traits::External);

        // This registers non-indexed channels.
        UdpChannel::set_address_fn(&channel, addr);
        channel
    }

    /// `UDPChannel::get( networkInterface, address )` (udp_channel.cpp:171):
    /// claim the anonymous channel to the address, or build a fresh
    /// internal one.
    pub fn get(interface: &Rc<NetworkInterface>, address: Address) -> ChannelPtr {
        match interface.find_channel(address, false) {
            Some(channel) => {
                let is_condemned = channel.borrow().is_condemned;
                if !is_condemned {
                    assert!(
                        channel.borrow().is_anonymous,
                        "UDPChannel::get: MF_ASSERT( pChannel->isAnonymous() )"
                    );

                    // This brings the channel back in sync with the state
                    // it would have been in from a normal (explicit)
                    // construction.
                    UdpChannel::set_is_anonymous(&channel, false);

                    eprintln!("UDPChannel::get: Claimed anonymous channel to {address}");
                } else {
                    eprintln!("UDPChannel::get: Returned condemned channel to {address}");
                }
                channel
            }
            None => UdpChannel::new(
                interface,
                address,
                Traits::Internal,
                DEFAULT_INACTIVITY_RESEND_DELAY,
                CHANNEL_ID_NULL,
            ),
        }
    }

    /// `UDPChannel::setAddress( addr )` (udp_channel.cpp:206): a new
    /// address deregisters the old one and registers the new, unless the
    /// channel is indexed -- those stay off the map.
    pub fn set_address_fn(channel: &ChannelPtr, addr: Address) {
        let (old, is_indexed, interface) = {
            let this = channel.borrow();
            (
                this.addr,
                this.is_indexed(),
                this.network_interface.upgrade(),
            )
        };
        if old == addr {
            return;
        }

        let now = interface
            .as_ref()
            .map(|interface| interface.dispatcher().now())
            .unwrap_or(0);
        channel.borrow_mut().last_received_time = now;

        // If this channel was previously established, it waits for a
        // `FLAG_CREATE_CHANNEL` packet from the new address before
        // accepting traffic again (udp_channel.cpp:1655-1661).
        if !old.is_none() {
            channel.borrow_mut().wants_first_packet = true;
        }

        if !is_indexed {
            if let Some(interface) = interface.as_ref()
                && !old.is_none()
            {
                assert!(
                    interface.deregister_channel(channel),
                    "MF_VERIFY( pNetworkInterface_->deregisterChannel( *this ) )"
                );
            }

            channel.borrow_mut().addr = addr;

            if let Some(interface) = interface.as_ref()
                && !addr.is_none()
            {
                assert!(
                    interface.register_channel(channel),
                    "MF_VERIFY( pNetworkInterface_->registerChannel( *this ) )"
                );
            }
        } else {
            channel.borrow_mut().addr = addr;
        }
    }

    /// `UDPChannel::isAnonymous( bool )` (udp_channel.cpp:1394), minus the
    /// keepalive and regularity bookkeeping (BW-02-03-03/05): anonymity
    /// means the interface owns the channel's cleanup.
    pub fn set_is_anonymous(channel: &ChannelPtr, anonymous: bool) {
        channel.borrow_mut().is_anonymous = anonymous;
    }

    /// `UDPChannel::condemn()` (udp_channel.cpp:380): detach from the
    /// owner and schedule deletion once the unacked packets drain. With
    /// nothing unacked the channel destroys on the spot.
    pub fn condemn(channel: &ChannelPtr) {
        if channel.borrow().is_condemned {
            eprintln!(
                "UDPChannel::condemn( {} ): Already condemned.",
                channel.borrow().c_str()
            );
            return;
        }

        // The reference flushes unsent data here through `send()`; the
        // channel send path is BW-02-03-06, so until then the flush arm is
        // deferred and any unsent bundle goes with the channel.

        // Since you aren't going to be actively sending on this channel
        // anymore, it must be marked as irregular (udp_channel.cpp:403-407).
        // `set_local_regular` files the channel for the interface's shared
        // periodic resend check, which is what keeps the drain moving.
        UdpChannel::set_local_regular(channel, false);
        UdpChannel::set_remote_regular(channel, false);

        {
            let mut this = channel.borrow_mut();
            this.is_condemned = true;
        }

        // The weak upgrade through the borrow would hold the Ref across
        // the call below, which re-borrows the channel; scope it out.
        let interface = channel.borrow().network_interface.upgrade();
        if let Some(interface) = interface {
            // Note: This call may delete this channel.
            interface.add_condemned_channel(channel);
        }
    }

    /// `Channel::destroy()` (channel.cpp:319): close the channel for
    /// good. Idempotent, like the reference's `IF_NOT_MF_ASSERT_DEV`.
    pub fn destroy(channel: &ChannelPtr) {
        let (addr, listener, interface) = {
            let mut this = channel.borrow_mut();
            if this.is_destroyed {
                // IF_NOT_MF_ASSERT_DEV( !isDestroyed_ ): the dev warning is
                // the caller's to notice; the port stays quiet.
                return;
            }

            this.inactivity_timer.cancel();
            this.is_destroyed = true;
            (
                this.addr,
                this.listener.clone(),
                this.network_interface.upgrade(),
            )
        };

        if let Some(interface) = interface.as_ref() {
            // pNetworkInterface_->onChannelGone( this ): any outstanding
            // requests are told of this failure.
            interface.on_channel_gone(&addr);

            // The reference deregisters from the destructor
            // (`clearState` -> `setAddress( NONE )`); the port has no
            // delete-on-last-unref moment to hang it on, so it
            // deregisters here.
            UdpChannel::set_address_fn(channel, NONE);

            // `clearState` clears this channel from the monitoring
            // collections (udp_channel.cpp:346).
            interface.del_irregular_channel_if_necessary(channel);
        }

        if let Some(listener) = listener {
            listener.on_channel_gone(channel);
        }
    }

    /// `Channel::onChannelInactivityTimeout()` (channel.hpp): the
    /// inactivity checker's verdict is destruction.
    pub fn on_channel_inactivity_timeout(channel: &ChannelPtr) {
        UdpChannel::destroy(channel);
    }

    /// `Channel::startInactivityDetection( period, checkPeriod )`
    /// (channel.cpp:371): arm the checker. Nothing received for `period`
    /// seconds destroys the channel.
    pub fn start_inactivity_detection(channel: &ChannelPtr, period: f32, check_period: f32) {
        let interface = match channel.borrow().network_interface.upgrade() {
            Some(interface) => interface,
            None => return,
        };

        {
            let mut this = channel.borrow_mut();
            this.inactivity_timer.cancel();

            this.inactivity_exception_period = (period * 1_000_000.0) as u64;
            this.last_received_time = interface.dispatcher().now();
        }

        let handler = Rc::new(InactivityTimeoutChecker {
            channel: Rc::downgrade(channel),
            inactivity_exception_period: (period * 1_000_000.0) as u64,
        });
        let timer = interface.dispatcher().add_timer(
            (check_period * 1_000_000.0) as i64,
            handler,
            0,
            "ChannelInactivity",
        );
        channel.borrow_mut().inactivity_timer = timer;
    }

    /// `pFilter()` (udp_channel.hpp:139): this channel's packet filter,
    /// consulted before the header walk and before the socket.
    pub fn p_filter(channel: &ChannelPtr) -> Option<Rc<dyn PacketFilter>> {
        channel.borrow().p_filter.clone()
    }

    /// `pFilter( pFilter )` (udp_channel.hpp:140): install the filter and
    /// capture the spare room it wants out of every packet
    /// (`newBundle`, udp_channel.cpp:1526-1531). The reference reads the
    /// spare only when the channel's bundle is first created, which here
    /// happens at construction -- before a filter can be attached -- so
    /// the capture runs at attach instead; a channel attaches its filter
    /// before it sends, so the value matches.
    pub fn set_p_filter(channel: &ChannelPtr, filter: Option<Rc<dyn PacketFilter>>) {
        let spare = filter.as_ref().map_or(0, |filter| filter.max_spare_size());
        let mut this = channel.borrow_mut();
        this.p_filter = filter;
        this.bundle.set_spare_size(spare as u8);
    }

    /// `UDPChannel::setEncryption` (udp_channel.cpp:670): wrap a session
    /// cipher in an `EncryptionFilter` and install it, or clear the
    /// channel's filter when given `None` -- the two ends of session
    /// negotiation (BW-02-05-02).
    pub fn set_encryption(
        channel: &ChannelPtr,
        cipher: Option<Rc<dyn crate::block_cipher::BlockCipher>>,
    ) {
        let filter = cipher.map(|cipher| {
            let filter: Rc<dyn PacketFilter> =
                crate::encryption_filter::EncryptionFilter::create(cipher);
            filter
        });
        Self::set_p_filter(channel, filter);
    }

    /// `UDPChannel::setRemoteFailed()` (udp_channel.cpp:1792): the far end
    /// is gone. Internal channels cancel their outstanding requests, and
    /// anonymous ones clean themselves up -- no-one else will.
    pub fn set_remote_failed(channel: &ChannelPtr) {
        let (is_internal, is_anonymous, addr, interface) = {
            let mut this = channel.borrow_mut();
            this.has_remote_failed = true;
            (
                this.traits == Traits::Internal,
                this.is_anonymous,
                this.addr,
                this.network_interface.upgrade(),
            )
        };

        if let Some(interface) = interface.as_ref() {
            if is_internal {
                eprintln!(
                    "UDPChannel::setRemoteFailed: Cancelling requests on an internal channel {}",
                    channel.borrow().c_str()
                );
                interface.cancel_requests_for(&addr);
            }

            // If this channel is anonymous, then no-one else is going to
            // clean it up.
            if is_anonymous {
                eprintln!(
                    "UDPChannel::setRemoteFailed: Cleaning up dead anonymous channel to {addr}"
                );
                interface.del_anonymous(&addr);
            }
        }
    }

    /// `isLocalRegular()` (udp_channel.hpp:142).
    pub fn is_local_regular(channel: &ChannelPtr) -> bool {
        channel.borrow().is_local_regular
    }

    /// `isLocalRegular( bool )` (udp_channel.cpp:1702): set whether this
    /// channel sends irregularly and indicates that its resends are
    /// managed globally.
    pub fn set_local_regular(channel: &ChannelPtr, is_local_regular: bool) {
        {
            let mut this = channel.borrow_mut();
            this.is_local_regular = is_local_regular;
        }

        // Channels that do not send regularly are added to a collection to
        // do their resend checking periodically.
        if let Some(interface) = channel.borrow().network_interface.upgrade() {
            interface.add_irregular_channel_if_necessary(channel);
        }
    }

    /// `isRemoteRegular()` (udp_channel.hpp:145).
    pub fn is_remote_regular(channel: &ChannelPtr) -> bool {
        channel.borrow().is_remote_regular
    }

    /// `isRemoteRegular( bool )` (udp_channel.cpp:1717): set whether the
    /// remote side of this channel sends regularly. If the remote side
    /// sends regularly, resends can be done via NACKS instead of being
    /// timer based.
    pub fn set_remote_regular(channel: &ChannelPtr, is_remote_regular: bool) {
        let mut this = channel.borrow_mut();
        this.is_remote_regular = is_remote_regular;
    }

    /// `UDPChannel::onPacketReceived( bytes )` (udp_channel.cpp:1822).
    pub fn on_packet_received(channel: &ChannelPtr, bytes: i32) {
        // The now reads through the same channel, so it comes first: the
        // stamp is in hand before the fields are written.
        let now = UdpChannel::now(channel);

        let mut this = channel.borrow_mut();
        this.last_received_time = now;
        this.num_data_units_received += 1;
        this.num_bytes_received += bytes as u32;
    }

    /// The dispatcher's now, through the interface. A channel whose
    /// interface is gone reads zero.
    fn now(channel: &ChannelPtr) -> u64 {
        channel
            .borrow()
            .network_interface
            .upgrade()
            .map(|interface| interface.dispatcher().now())
            .unwrap_or(0)
    }

    // -- The send window and its acks (BW-02-03-03) -----------------------

    /// `useNextSequenceID()` (udp_channel.cpp:1728), minus the
    /// warn-threshold/callback bookkeeping (BW-02-03-06): hand out the
    /// next outgoing sequence number, advancing the allocator through the
    /// mask.
    pub fn use_next_sequence_id(channel: &ChannelPtr) -> SeqNum {
        let mut this = channel.borrow_mut();
        let seq_num = this.large_out_seq_at;
        this.large_out_seq_at = seq_mask(seq_num.wrapping_add(1));
        seq_num
    }

    /// `isInSentWindow( seq )` (udp_channel.ipp:29): whether `seq` falls in
    /// `[oldestUnackedSeq_, smallOutSeqAt_)`. Both ends are wrap-aware.
    pub fn is_in_sent_window(&self, seq: SeqNum) -> bool {
        self.oldest_unacked_seq != SEQ_NULL
            && !seq_less_than(seq, self.oldest_unacked_seq)
            && seq_less_than(seq, self.small_out_seq_at)
    }

    /// `unackedPackets_[ seq ] != NULL`, spelled out: whether the seq holds
    /// an unacked packet. The reference inlines this test at three sites
    /// (udp_channel.cpp:989, 1019, 1103).
    pub fn is_unacked(&self, seq: SeqNum) -> bool {
        self.unacked_packets.get(seq as usize).is_some()
    }

    /// `sendWindowUsage()` (udp_channel.hpp:237): how much of the send
    /// window is in use. "This includes the overflow packets and so can be
    /// larger than windowSize_."
    pub fn send_window_usage(&self) -> i32 {
        if self.has_unacked_packets() {
            seq_mask(self.large_out_seq_at.wrapping_sub(self.oldest_unacked_seq)) as i32
        } else {
            0
        }
    }

    /// `sendWindowSize()` (udp_channel.hpp:416): the ring's current
    /// capacity -- it doubles past the window size.
    pub fn send_window_size(&self) -> usize {
        self.unacked_packets.size()
    }

    /// `numOutstandingPackets()` (udp_channel.cpp:1692).
    pub fn num_outstanding_packets(&self) -> i32 {
        self.bundle.num_data_units() as i32 + self.send_window_usage()
    }

    /// `hasSeenOverflowWarning_`.
    pub fn has_seen_overflow_warning(&self) -> bool {
        self.has_seen_overflow_warning
    }

    /// `getMaxOverflowPackets()` (udp_channel.hpp:423).
    fn get_max_overflow_packets(&self) -> usize {
        if self.is_external() {
            MAX_OVERFLOW_PACKETS[0]
        } else {
            MAX_OVERFLOW_PACKETS[usize::from(self.is_indexed()) * 2]
        }
    }

    /// `maxWindowSize()` (udp_channel.hpp:435): the receive window's
    /// reject ceiling, overflow headroom included.
    fn max_window_size(&self) -> usize {
        self.window_size + self.get_max_overflow_packets()
    }

    /// `checkOverflowErrors()` (udp_channel.cpp:421) minus the
    /// `s_assertOnMaxOverflowPackets` assert (off by default; the port has
    /// no interactive-debugging switch): warn once when the unsent overflow
    /// passes half the ceiling, clear once it drains under a third.
    fn check_overflow_errors(&mut self) {
        let max_overflow_packets = self.get_max_overflow_packets() as SeqNum;
        let num_overflow_packets =
            seq_mask(self.large_out_seq_at.wrapping_sub(self.small_out_seq_at));

        if max_overflow_packets != 0 {
            // Warn if the overflow size has grown to 1/2 of the MAX size
            if num_overflow_packets > (max_overflow_packets / 2) {
                if !self.has_seen_overflow_warning {
                    eprintln!(
                        "UDPChannel::checkOverflowErrors({}): Overflow packet list size ({}) exceeding safety threshold ({}).",
                        self.c_str(),
                        num_overflow_packets,
                        max_overflow_packets / 2
                    );
                    self.has_seen_overflow_warning = true;
                }
            } else if self.has_seen_overflow_warning
                && num_overflow_packets < (max_overflow_packets / 3)
            {
                self.has_seen_overflow_warning = false;
            }
        }
    }

    /// `sendUnacked( unacked )` (udp_channel.cpp:1187): push a packet in
    /// the window out again -- re-stamp an indexed packet's channel
    /// version, hand it to the interface as a resend, and refresh the
    /// window's stamps. An absent slot is a no-op where the reference
    /// would dereference null.
    fn send_unacked(channel: &ChannelPtr, seq: SeqNum) {
        let now = UdpChannel::now(channel);
        let (version, id, addr, small) = {
            let this = channel.borrow();
            (this.version, this.id, this.addr, this.small_out_seq_at)
        };
        let interface = channel.borrow().network_interface.upgrade();

        let mut sent_packet: Option<Rc<Packet>> = None;
        {
            let mut this = channel.borrow_mut();
            if let Some(unacked) = this.unacked_packets.get_mut(seq as usize).as_mut() {
                unacked.packet_mut().update_channel_version(version, id);
                unacked.set_last_sent_at_out_seq(small);
                unacked.set_was_resent(true);
                unacked.set_last_sent_time(now);
                sent_packet = Some(Rc::clone(unacked.packet()));
            }
            this.last_reliable_resend_time = now;
        }

        if let (Some(interface), Some(packet)) = (interface, sent_packet) {
            interface.send_packet(&addr, &packet, Some(channel), true);
        }
    }

    /// `writeFlags( p )` (udp_channel.cpp:1526): the flags a packet riding
    /// on this channel owes, plus the footer room each one costs: the
    /// channel tag, a cumulative ack pruning the acks already covered, as
    /// many individual acks as fit, and the first-reliable `CREATE_CHANNEL`
    /// flag.
    pub fn write_flags(&mut self, packet: &mut Packet) {
        packet.enable_flags(flags::ON_CHANNEL);

        if self.is_indexed() {
            packet.enable_flags(flags::INDEXED_CHANNEL);
            packet.set_channel_id(self.id);
            packet.set_channel_version(self.version);
            packet.reserve_footer(4 + 4);
        }

        // Add a cumulative ACK: everything before `in_seq_at_` arrived.
        if packet.free_space() >= 4 {
            packet.enable_flags(flags::HAS_CUMULATIVE_ACK);
            packet.reserve_footer(4);

            // Prune the acks the cumulative one already covers.
            let in_seq_at = self.in_seq_at;
            self.acks_to_send
                .retain(|&seq| !seq_less_than(seq, in_seq_at));
        }

        // Put on as many acks as we can.
        if !self.acks_to_send.is_empty() && packet.free_space() > 4 {
            packet.enable_flags(flags::HAS_ACKS);
            packet.reserve_footer(1);

            let min_space = packet.free_space() / 4; // in SeqNums
            let min_space_size = min_space.min(self.acks_to_send.len());
            let n_acks = min_space_size.min(crate::packet::MAX_ACKS);
            packet.set_n_acks(n_acks as u8);
            packet.reserve_footer(4 * n_acks);
        }

        // If this is the first reliable outbound packet, flag it.
        if packet.has_flags(flags::IS_RELIABLE)
            && self.is_internal()
            && self.large_out_seq_at == 0
            && self.num_reliable_packets_sent == 0
        {
            packet.enable_flags(flags::CREATE_CHANNEL);
        }
    }

    /// `writeFooter( p )` (udp_channel.cpp:1593): the footers the flags
    /// owe, packed back after the bundle's own -- the channel tag, the
    /// cumulative ack, then the individual acks, consuming them.
    pub fn write_footer(&mut self, packet: &mut Packet) {
        if packet.has_flags(flags::INDEXED_CHANNEL) {
            let channel_id = packet.channel_id() as u32;
            packet.pack_footer_u32(channel_id);
            let channel_version = packet.channel_version();
            packet.pack_footer_u32(channel_version);
        }

        if packet.has_flags(flags::HAS_CUMULATIVE_ACK) {
            let in_seq_at = self.in_seq_at;
            packet.pack_footer_u32(in_seq_at);
        }

        if packet.has_flags(flags::HAS_ACKS) {
            let n_acks = packet.n_acks();
            packet.pack_footer_u8(n_acks);
            let mut acks_added = 0u8;
            while !self.acks_to_send.is_empty() && acks_added < n_acks {
                let seq = *self.acks_to_send.iter().next().expect("non-empty set");
                packet.pack_footer_u32(seq);
                self.acks_to_send.remove(&seq);
                acks_added += 1;
            }
        }
    }

    /// `hasUnsentData()` (udp_channel.cpp:645): does the bundle hold
    /// anything a send would put on the wire? The bundle primer's
    /// unreliable messages are BW-02-05 and do not exist here. Used by
    /// [`send`](Self::send), which reads the bundle it moved out.
    pub fn has_unsent_data(&self) -> bool {
        let bundle = &self.bundle;
        bundle.num_messages() > 0
            || bundle.has_data_footers()
            || bundle.is_reliable()
            || !self.acks_to_send.is_empty()
    }

    /// `hasUnsentData()` for a bundle that has been moved out of the
    /// channel: [`do_send`](Self::do_send) holds the channel's bundle by
    /// value while the channel's own `bundle` slot is a placeholder, so
    /// the reference's gate reads the bundle being sent, not the slot.
    fn has_unsent_data_for(bundle: &UdpBundle, acks_to_send: &BTreeSet<SeqNum>) -> bool {
        bundle.num_messages() > 0
            || bundle.has_data_footers()
            || bundle.is_reliable()
            || !acks_to_send.is_empty()
    }

    /// `Channel::send` (channel.cpp) with `UDPChannel::doPreFinaliseBundle`
    /// (checkResendTimers), `finalise`, `addReplyOrdersTo` and
    /// `UDPChannel::doSend` inlined: resend-check, close the bundle, send
    /// it, update the stats, and clear. The bundle is moved out of the
    /// channel first so the resend check and the interface send can borrow
    /// the channel freely -- the reference reads both through raw pointers.
    pub fn send(channel: &ChannelPtr) {
        if !channel.borrow().is_connected() {
            eprintln!(
                "Channel::send( {} ): Channel is not connected",
                channel.borrow().c_str()
            );
            return;
        }

        // The send path cancels requests rather than sending when the
        // remote has failed.
        if channel.borrow().has_remote_failed {
            let interface = channel.borrow().network_interface.upgrade();
            if let Some(interface) = interface {
                interface.cancel_requests_for(&channel.borrow().addr);
            }
            return;
        }

        // Move the bundle out so `check_resend_timers` and the interface
        // send can borrow the channel; the placeholder is put back below.
        let mut bundle = std::mem::replace(&mut channel.borrow_mut().bundle, UdpBundle::new(0));

        UdpChannel::do_send(channel, &mut bundle, true);

        // Clear the bundle, keeping the channel's external tag.
        bundle.clear();
        channel.borrow_mut().bundle = bundle;

        if let Some(listener) = channel.borrow().listener.clone() {
            listener.on_channel_send(channel);
        }
    }

    /// `Channel::send( &emptyBundle )` (channel.cpp:149): push the channel's
    /// acknowledgements out through a throwaway bundle, the way the receive
    /// path sends an ack the moment an out-of-order packet arrives.
    pub fn send_acks(channel: &ChannelPtr) {
        if !channel.borrow().is_connected() {
            eprintln!(
                "Channel::send( {} ): Channel is not connected",
                channel.borrow().c_str()
            );
            return;
        }

        if channel.borrow().has_remote_failed {
            let interface = channel.borrow().network_interface.upgrade();
            if let Some(interface) = interface {
                interface.cancel_requests_for(&channel.borrow().addr);
            }
            return;
        }

        let mut bundle = UdpBundle::new(0);
        UdpChannel::do_send(channel, &mut bundle, false);
        bundle.clear();

        if let Some(listener) = channel.borrow().listener.clone() {
            listener.on_channel_send(channel);
        }
    }

    /// `Channel::send`'s body from `doPreFinaliseBundle` down through
    /// `UDPChannel::doSend` (udp_channel.cpp:693-768). `is_own` says whether
    /// the bundle is the channel's own: only that one may skip the wire
    /// while it carries no data.
    fn do_send(channel: &ChannelPtr, bundle: &mut UdpBundle, is_own: bool) {
        let addr = channel.borrow().addr;

        // `doSend` drops the bundle when there is nowhere to send it.
        if addr.is_none() {
            eprintln!(
                "UDPChannel::doSend( {} ): no address to send to, dropping bundle",
                channel.borrow().c_str()
            );
            return;
        }

        // `doPreFinaliseBundle`: tack on piggybacks from resends.
        UdpChannel::check_resend_timers(channel, bundle);

        bundle.finalise();

        // `addReplyOrdersTo`.
        let interface = channel.borrow().network_interface.upgrade();
        if let Some(interface) = interface.as_ref() {
            interface.add_reply_orders_to(bundle, Some(channel));
        }

        // A channel's own empty bundle sends nothing (after the resend
        // check, so channels still get their resends checked). The
        // piggybacks a resend folded in count as data. The bundle was
        // moved out of the channel's slot, so the gate reads it here.
        let has_unsent = {
            let this = channel.borrow();
            Self::has_unsent_data_for(bundle, &this.acks_to_send)
        };
        if is_own && !has_unsent {
            return;
        }

        if let Some(interface) = interface.as_ref() {
            interface.send(&addr, bundle, Some(channel));
        }

        {
            let mut this = channel.borrow_mut();
            this.num_data_units_sent += 1;
            this.num_bytes_sent += bundle.size() as u32;
            if bundle.is_reliable() {
                this.num_reliable_packets_sent += 1;
            }
        }

        // Channels that do not send regularly are checked periodically.
        if let Some(interface) = channel.borrow().network_interface.upgrade() {
            interface.add_irregular_channel_if_necessary(channel);
        }

        // A critical bundle's last packet sequence is the new critical.
        if bundle.is_critical() {
            let num_packets = bundle.num_data_units();
            let last_seq = bundle.packet(num_packets - 1).seq();
            channel.borrow_mut().unacked_critical_seq = last_seq;
        }
    }

    /// `addResendTimer( seq, p, roBeg, roEnd )` (udp_channel.cpp:817):
    /// file a sent packet in the window and say whether the caller should
    /// send now -- false means the window is full and the packet is queued
    /// as overflow (`REASON_WINDOW_OVERFLOW`).
    pub fn add_resend_timer(
        channel: &ChannelPtr,
        seq: SeqNum,
        packet: Rc<Packet>,
        reliable_orders: &[ReliableOrder],
    ) -> bool {
        let now = UdpChannel::now(channel);
        let mut this = channel.borrow_mut();
        assert!(
            this.oldest_unacked_seq == SEQ_NULL
                || this
                    .unacked_packets
                    .get(this.oldest_unacked_seq as usize)
                    .is_some(),
            "addResendTimer: the oldest unacked slot is occupied"
        );
        assert!(
            seq == packet.seq(),
            "addResendTimer: MF_ASSERT( seq == p->seq() )"
        );

        let mut unacked = UnackedPacket::new(packet);

        // If this channel has no unacked packets, record this as the oldest.
        if this.oldest_unacked_seq == SEQ_NULL {
            this.oldest_unacked_seq = seq;
        }

        // Fill it in
        unacked.set_last_sent_at_out_seq(seq);
        unacked.set_last_sent_time(now);
        this.last_reliable_send_time = now;
        unacked.set_was_resent(false);
        if !reliable_orders.is_empty() {
            unacked.set_reliable_orders(reliable_orders.to_vec());
        }

        // Grow the unackedPackets_ array, if necessary.
        let old_unacked_seq = this.oldest_unacked_seq;
        if seq_mask(seq.wrapping_sub(old_unacked_seq).wrapping_add(1)) as usize
            > this.unacked_packets.size()
        {
            this.unacked_packets.double_size(old_unacked_seq as usize);
        }

        assert!(
            this.unacked_packets.get(seq as usize).is_none(),
            "addResendTimer: the slot is free"
        );
        *this.unacked_packets.get_mut(seq as usize) = Some(unacked);

        if seq_mask(this.large_out_seq_at.wrapping_sub(this.oldest_unacked_seq)) as usize
            >= this.window_size
        {
            // Make sure that we at least send occasionally.
            let prev = seq_mask(this.small_out_seq_at.wrapping_sub(1));
            let min_inactivity_resend_delay = this.min_inactivity_resend_delay;
            let prev_old_enough = match this.unacked_packets.get(prev as usize) {
                Some(prev) => {
                    now.saturating_sub(prev.last_sent_time()) > min_inactivity_resend_delay
                }
                None => true,
            };

            if prev_old_enough {
                let small = this.small_out_seq_at;
                drop(this);
                UdpChannel::send_unacked(channel, small);
                let mut this = channel.borrow_mut();
                this.small_out_seq_at = seq_mask(small.wrapping_add(1));
                this.check_overflow_errors();
            } else {
                this.check_overflow_errors();
            }

            // We shouldn't send now
            false
        } else {
            // We should send now
            this.small_out_seq_at = this.large_out_seq_at;
            true
        }
    }

    /// `handleAck( seq )` (udp_channel.cpp:964): the single place an ack
    /// lands -- the RTT average, the critical slot, the oldest-unacked
    /// rescan and the highest-ack advance all happen here, in that order.
    /// False only for an out-of-range sequence number.
    pub fn handle_ack(channel: &ChannelPtr, seq: SeqNum) -> bool {
        let now = UdpChannel::now(channel);
        let mut this = channel.borrow_mut();
        assert!(
            this.oldest_unacked_seq == SEQ_NULL
                || this
                    .unacked_packets
                    .get(this.oldest_unacked_seq as usize)
                    .is_some(),
            "handleAck: the oldest unacked slot is occupied"
        );

        // Make sure the sequence number is valid
        if seq_mask(seq) != seq {
            eprintln!(
                "UDPChannel::handleAck( {} ): Got out-of-range seq #{} (outseq: #{})",
                this.c_str(),
                seq,
                this.small_out_seq_at
            );
            return false;
        }

        if !this.is_in_sent_window(seq) {
            return true;
        }

        // now make sure there's actually a packet there
        let (was_resent, last_sent_time) = match this.unacked_packets.get(seq as usize) {
            Some(unacked) => (unacked.was_resent(), unacked.last_sent_time()),
            None => return true,
        };

        // Update the average RTT for this channel, if this packet hadn't
        // already been resent.
        if !was_resent {
            const RTT_AVERAGE_DENOM: u64 = 10;
            this.round_trip_time = (this.round_trip_time * (RTT_AVERAGE_DENOM - 1)
                + now.saturating_sub(last_sent_time))
                / RTT_AVERAGE_DENOM;
        }

        // If this packet was the critical one, we're no longer in a
        // critical state!
        if this.unacked_critical_seq == seq {
            this.unacked_critical_seq = SEQ_NULL;
        }

        // If we released the oldest unacked packet, figure out the new one
        if seq == this.oldest_unacked_seq {
            this.oldest_unacked_seq = SEQ_NULL;
            let mut i = seq_mask(seq.wrapping_add(1));
            while i != this.large_out_seq_at {
                if this.unacked_packets.get(i as usize).is_some() {
                    this.oldest_unacked_seq = i;
                    break;
                }
                i = seq_mask(i.wrapping_add(1));
            }
        }

        // If the incoming seq is after the last ack, then it is the new
        // last ack
        if seq_less_than(this.highest_ack, seq) {
            this.highest_ack = seq;
        }

        // Now we can release the unacked packet
        *this.unacked_packets.get_mut(seq as usize) = None;
        drop(this);

        // While the window has room and something waits at smallOutSeqAt_,
        // send it and advance (udp_channel.cpp:1040).
        loop {
            let (send_it, small) = {
                let this = channel.borrow();
                let small = this.small_out_seq_at;
                let window_size = this.window_size;
                let roomy =
                    (seq_mask(small.wrapping_sub(this.oldest_unacked_seq)) as usize) < window_size;
                (
                    roomy && this.unacked_packets.get(small as usize).is_some(),
                    small,
                )
            };
            if !send_it {
                break;
            }
            UdpChannel::send_unacked(channel, small);
            channel.borrow_mut().small_out_seq_at = seq_mask(small.wrapping_add(1));
        }

        true
    }

    /// `handleCumulativeAck( endSeq )` (udp_channel.cpp:899): decompose a
    /// cumulative ack into one `handleAck` per seq, up to but not including
    /// `endSeq`. False for an out-of-range `endSeq` or one that acks
    /// packets not yet sent.
    pub fn handle_cumulative_ack(channel: &ChannelPtr, end_seq: SeqNum) -> bool {
        {
            let this = channel.borrow();

            // Make sure the sequence number is valid
            if seq_mask(end_seq) != end_seq {
                eprintln!(
                    "UDPChannel::handleCumulativeAck( {} ): Got out-of-range endSeq #{} (outseq: #{})",
                    this.c_str(),
                    end_seq,
                    this.small_out_seq_at
                );
                return false;
            }

            if !this.has_unacked_packets() {
                return true;
            }

            // Check that the ACK is not in the future.
            // Note: endSeq is first seqNum after what's been received.
            if seq_less_than(this.small_out_seq_at, end_seq) {
                // ERROR on external, CRITICAL otherwise (same text).
                eprintln!(
                    "UDPChannel::handleCumulativeAck( {} ): Received ACK for packets not yet sent. endSeq: #{} outSeq: #{}",
                    this.c_str(),
                    end_seq,
                    this.small_out_seq_at
                );
                return false;
            }
        }

        let mut seq = channel.borrow().oldest_unacked_seq;

        // Note: Up to but not including endSeq
        while seq_less_than(seq, end_seq) {
            UdpChannel::handle_ack(channel, seq);
            seq = seq_mask(seq.wrapping_add(1));
        }

        true
    }

    // -- The resend timing (BW-02-03-04) ----------------------------------

    /// `checkResendTimers( bundle )` (udp_channel.cpp:1051): resend any
    /// unacked packets as appropriate. This can be because of time since
    /// last sent, receiving later acks before earlier ones. The bundle is
    /// the piggyback target of a resend that folds into the next send.
    pub fn check_resend_timers(channel: &ChannelPtr, bundle: &mut UdpBundle) {
        let now = UdpChannel::now(channel);

        // There are no un-acked packets
        {
            let this = channel.borrow();
            if this.oldest_unacked_seq == SEQ_NULL {
                return;
            }

            // Don't do anything if the remote process has failed
            if this.has_remote_failed {
                if this
                    .network_interface
                    .upgrade()
                    .is_some_and(|interface| interface.is_verbose())
                {
                    eprintln!(
                        "UDPChannel::checkResendTimers( {} ): Not doing anything due to remote process failure",
                        this.c_str()
                    );
                }
                return;
            }
        }

        // If we have unacked packets that are getting a bit old, then
        // resend the ones that are older than we'd like.  Anything that
        // has taken more than twice the RTT on the channel to come back is
        // considered to be too old.
        let (resend_period, is_irregular, end_seq, max_resends) = {
            let this = channel.borrow();
            let resend_period =
                std::cmp::max(this.round_trip_time * 2, this.min_inactivity_resend_delay);
            let is_irregular = !this.is_remote_regular;
            let end_seq = if is_irregular {
                this.small_out_seq_at
            } else {
                this.highest_ack
            };

            // TODO: 8 is a magic number and would be nice to be more
            // scientific. The idea is to throttle the resends a little in
            // extreme situations. We want to send enough so that no (or
            // not too many) packets are lost but still be able to send
            // more when the RTT is large.
            let max_resends = this.window_size / 8;

            (resend_period, is_irregular, end_seq, max_resends)
        };

        let mut num_resends: usize = 0;
        let mut seq = channel.borrow().oldest_unacked_seq;

        while seq_less_than(seq, end_seq) && num_resends < max_resends {
            // Send if the packet is old, or we have a later ack. The
            // decision reads the slot as it stands: an earlier resend in
            // this same walk has already refreshed its stamps.
            let should_resend = {
                let this = channel.borrow();
                match this.unacked_packets.get(seq as usize).as_ref() {
                    Some(unacked) => {
                        let has_newer_ack =
                            seq_less_than(unacked.last_sent_at_out_seq(), this.highest_ack);
                        has_newer_ack
                            || (is_irregular
                                && now.saturating_sub(unacked.last_sent_time()) > resend_period)
                    }
                    None => false,
                }
            };

            if should_resend {
                UdpChannel::resend(channel, seq, bundle);
                num_resends += 1;
            }

            seq = seq_mask(seq.wrapping_add(1));
        }
    }

    /// `resend( seq, bundle )` (udp_channel.cpp:1151): resends an un-acked
    /// packet by the most sensible method available. True when the packet
    /// is no longer un-acked -- the piggyback arm folded it into the
    /// outgoing bundle and acked it.
    fn resend(channel: &ChannelPtr, seq: SeqNum, bundle: &mut UdpBundle) -> bool {
        {
            let mut this = channel.borrow_mut();
            this.num_packets_resent += 1;
        }

        // If possible, piggyback this packet onto the next outgoing
        // bundle: external channels, non-fragment packets, and a free slot
        // at `small_out_seq_at_` (so the resend does not overflow).
        let is_external = channel.borrow().is_external();
        let slot_free = {
            let this = channel.borrow();
            this.unacked_packets
                .get(this.small_out_seq_at as usize)
                .is_none()
        };
        let (is_fragment, orders, packet) = {
            let this = channel.borrow();
            match this.unacked_packets.get(seq as usize) {
                Some(unacked) => (
                    unacked.packet().has_flags(flags::IS_FRAGMENT),
                    unacked.reliable_orders().to_vec(),
                    Rc::clone(unacked.packet()),
                ),
                None => return false,
            }
        };

        if is_external && !is_fragment && slot_free && bundle.piggyback(seq, &orders, packet) {
            // Don't count this for RTT calculations.
            if let Some(unacked) = channel
                .borrow_mut()
                .unacked_packets
                .get_mut(seq as usize)
                .as_mut()
            {
                unacked.set_was_resent(true);
            }
            UdpChannel::handle_ack(channel, seq);
            return true;
        }

        // If there are any acks on this packet, then they will be resent
        // too, but it does no harm.
        UdpChannel::send_unacked(channel, seq);

        false
    }

    /// `addToReceiveWindow( p, srcAddr, stats )` (udp_channel.cpp:1208):
    /// file a reliable incoming packet -- delivering the next-in-order
    /// chain, buffering a future one, discarding a duplicate, or
    /// rejecting it. The port returns the delivered chain in the
    /// `NextInWindow` variant (the reference chains onto the packet); the
    /// caller walks it in the original send order.
    pub fn add_to_receive_window(
        channel: &ChannelPtr,
        packet: Rc<Packet>,
        src_addr: &Address,
    ) -> AddToReceiveWindowResult {
        let seq = packet.seq();

        // Make sure the sequence number is valid.
        if seq_mask(seq) != seq {
            {
                let this = channel.borrow();
                if this
                    .network_interface
                    .upgrade()
                    .is_some_and(|interface| interface.is_verbose())
                {
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): \
                         Got out-of-range incoming seq #{} (inSeqAt: #{})",
                        this.c_str(),
                        seq,
                        this.in_seq_at
                    );
                }
            }

            return AddToReceiveWindowResult::Corrupt;
        }

        // An auto-switching indexed channel re-addresses itself from the
        // packet when the version is acceptable. We switch on equal
        // version numbers because the first packet from a cell entity
        // sets the address and is version 0. The re-addressing runs
        // outside the borrow: `set_address_fn` borrows the channel too.
        let should_switch = {
            let mut this = channel.borrow_mut();
            let should_switch = this.should_auto_switch_to_src_addr
                && !seq_less_than(packet.channel_version(), this.version);

            if !this.should_auto_switch_to_src_addr && this.addr != *src_addr {
                if this
                    .network_interface
                    .upgrade()
                    .is_some_and(|interface| interface.is_verbose())
                {
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): \
                         Got packet #{} from wrong source address: {}",
                        this.c_str(),
                        seq,
                        src_addr
                    );
                }

                return AddToReceiveWindowResult::Corrupt;
            }

            if should_switch {
                this.version = packet.channel_version();
            }

            should_switch
        };
        if should_switch {
            UdpChannel::set_address_fn(channel, *src_addr);
        }

        {
            let mut this = channel.borrow_mut();

            // No need to ACK piggybacks as they are implicitly ACKed by
            // the containing packet's ACK.
            if !packet.is_piggyback() {
                // Always add the ACK for others even though a cumulative
                // ack may mean it is not sent. This still shows that the
                // channel is not empty.
                this.acks_to_send.insert(seq);
            }

            // check the good case first
            if seq == this.in_seq_at {
                this.in_seq_at = seq_mask(this.in_seq_at.wrapping_add(1));

                let mut chain = vec![packet];

                // Attach as many buffered packets as possible to this one.
                loop {
                    let slot = this.in_seq_at as usize;
                    match this.buffered_receives.get_mut(slot).take() {
                        Some(buffered) => {
                            this.num_buffered_receives -= 1;
                            chain.push(buffered);
                            this.in_seq_at = seq_mask(this.in_seq_at.wrapping_add(1));
                        }
                        None => break,
                    }
                }

                return AddToReceiveWindowResult::NextInWindow(chain);
            }

            // see if we've got this one before. We have if seq < inSeqAt_.
            if seq_less_than(seq, this.in_seq_at) {
                if this
                    .network_interface
                    .upgrade()
                    .is_some_and(|interface| interface.is_verbose())
                {
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): \
                         Discarding already-seen packet #{} below inSeqAt #{}",
                        this.c_str(),
                        seq,
                        this.in_seq_at
                    );
                }
                this.num_duplicate_packets += 1;

                return AddToReceiveWindowResult::Duplicate;
            }

            // make sure it's in range
            let required_window_size = seq_mask(seq.wrapping_sub(this.in_seq_at)) as usize;

            // Note that we store packets between
            //  [inSeqAt_ + 1, inSeqAt_ + bufferedReceives_.size()]
            // in bufferedReceives. This is why we have > instead of >= in
            // the below conditions.
            if required_window_size > 2 * this.buffered_receives.size()
                || required_window_size > this.max_window_size()
            {
                if this
                    .network_interface
                    .upgrade()
                    .is_some_and(|interface| interface.is_verbose())
                {
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): \
                         Sequence number #{} is way out of window #{}",
                        this.c_str(),
                        seq,
                        this.in_seq_at
                    );
                }

                return AddToReceiveWindowResult::OutOfWindow;
            } else if required_window_size > this.buffered_receives.size() {
                // Copy buffered receives from the start of our buffered
                // receives, the first starting at inSeqAt_ + 1.
                let start_index = this.in_seq_at.wrapping_add(1) as usize;
                this.buffered_receives.double_size(start_index);

                if this
                    .network_interface
                    .upgrade()
                    .is_some_and(|interface| interface.is_verbose())
                {
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): \
                         Doubled receive buffer size to {}",
                        this.c_str(),
                        this.buffered_receives.size()
                    );
                }
            }

            // ok - we'll buffer this packet then....
            let slot = this.buffered_receives.get_mut(seq as usize);

            if slot.is_some() {
                // ... but only if we don't already have it. The reference
                // CRITICALs when the slot holds a different seq; the
                // port's slots are addressed by the very seq that keys
                // them, so an occupied slot always holds this packet and
                // the mismatch arm is unrepresentable.
                if this
                    .network_interface
                    .upgrade()
                    .is_some_and(|interface| interface.is_verbose())
                {
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): \
                         Discarding already-buffered packet #{}",
                        this.c_str(),
                        seq
                    );
                }
                this.num_duplicate_packets += 1;
            } else {
                *slot = Some(packet);
                this.num_buffered_receives += 1;

                if this
                    .network_interface
                    .upgrade()
                    .is_some_and(|interface| interface.is_verbose())
                {
                    eprintln!(
                        "UDPChannel::addToReceiveWindow( {} ): \
                         Buffering packet #{} above #{}",
                        this.c_str(),
                        seq,
                        this.in_seq_at
                    );
                }
            }

            AddToReceiveWindowResult::BufferedInWindow
        }
    }

    /// `validateUnreliableSeqNum( seqNum )` (udp_channel.cpp:1765): the
    /// unreliable sanity check -- reject an out-of-range seq, or one older
    /// than the last accepted one. Returns true when the slot moves.
    pub fn validate_unreliable_seq_num(channel: &ChannelPtr, seq_num: SeqNum) -> bool {
        let mut this = channel.borrow_mut();

        if seq_mask(seq_num) != seq_num {
            if this
                .network_interface
                .upgrade()
                .is_some_and(|interface| interface.is_verbose())
            {
                eprintln!(
                    "UDPChannel:validateUnreliableSeqNum: Invalid sequence number ({}).",
                    seq_num
                );
            }
            return false;
        }

        if seq_less_than(seq_num, this.unreliable_in_seq_at)
            && this.unreliable_in_seq_at != SEQ_NULL
        {
            return false;
        }

        // Only store the new seqNum if it has been completely validated.
        this.unreliable_in_seq_at = seq_num;
        true
    }

    // -- Channel base accessors (channel.hpp) ----------------------------

    /// `addr()`.
    pub fn addr(&self) -> Address {
        self.addr
    }

    /// `bundle()`: the channel's own bundle.
    pub fn bundle(&self) -> &UdpBundle {
        &self.bundle
    }

    /// `bundle()` mutable: the sender writes into it.
    pub fn bundle_mut(&mut self) -> &mut UdpBundle {
        &mut self.bundle
    }

    /// `clearBundle()` (channel.cpp:299): reuse the bundle or make one.
    /// The bundle keeps the spare room its filter claimed at attach
    /// ([`set_p_filter`](Self::set_p_filter)); primers are BW-02-05.
    pub fn clear_bundle(&mut self) {
        self.bundle.clear();
    }

    /// `isDestroyed()`.
    pub fn is_destroyed(&self) -> bool {
        self.is_destroyed
    }

    /// `isConnected()` (channel.hpp): usable for sending.
    pub fn is_connected(&self) -> bool {
        !self.is_destroyed
    }

    /// `lastReceivedTime()`, in micros.
    pub fn last_received_time(&self) -> u64 {
        self.last_received_time
    }

    /// `pChannelListener` setter.
    pub fn set_channel_listener(&mut self, listener: Option<Rc<dyn ChannelListener>>) {
        self.listener = listener;
    }

    /// `numDataUnitsSent()`.
    pub fn num_data_units_sent(&self) -> u32 {
        self.num_data_units_sent
    }

    /// `numDataUnitsReceived()`.
    pub fn num_data_units_received(&self) -> u32 {
        self.num_data_units_received
    }

    /// `numBytesSent()`.
    pub fn num_bytes_sent(&self) -> u32 {
        self.num_bytes_sent
    }

    /// `numBytesReceived()`.
    pub fn num_bytes_received(&self) -> u32 {
        self.num_bytes_received
    }

    // -- UDPChannel accessors --------------------------------------------

    /// `traits()`.
    pub fn traits(&self) -> Traits {
        self.traits
    }

    /// `isExternal()`.
    pub fn is_external(&self) -> bool {
        self.traits == Traits::External
    }

    /// `isInternal()`.
    pub fn is_internal(&self) -> bool {
        self.traits == Traits::Internal
    }

    /// `id()`.
    pub fn id(&self) -> ChannelID {
        self.id
    }

    /// `isIndexed()`: the id is not the null one.
    pub fn is_indexed(&self) -> bool {
        self.id != CHANNEL_ID_NULL
    }

    /// `version()`.
    pub fn version(&self) -> ChannelVersion {
        self.version
    }

    /// `creationVersion()`.
    pub fn creation_version(&self) -> ChannelVersion {
        self.creation_version
    }

    /// `isEstablished()`: the address is real.
    pub fn is_established(&self) -> bool {
        !self.addr.is_none()
    }

    /// `wantsFirstPacket()` (udp_channel.hpp:191): a re-addressed
    /// established channel waits for `FLAG_CREATE_CHANNEL` before it
    /// accepts traffic again.
    pub fn wants_first_packet(&self) -> bool {
        self.wants_first_packet
    }

    /// `gotFirstPacket()` (udp_channel.hpp:192): the create-channel packet
    /// arrived.
    pub fn got_first_packet(channel: &ChannelPtr) {
        channel.borrow_mut().wants_first_packet = false;
    }

    /// `windowSize()`.
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// `smallOutSeqAt()`.
    pub fn small_out_seq_at(&self) -> SeqNum {
        self.small_out_seq_at
    }

    /// `largeOutSeqAt()`.
    pub fn large_out_seq_at(&self) -> SeqNum {
        self.large_out_seq_at
    }

    /// `oldestUnackedSeq()`.
    pub fn oldest_unacked_seq(&self) -> SeqNum {
        self.oldest_unacked_seq
    }

    /// `highestAck()`.
    pub fn highest_ack(&self) -> SeqNum {
        self.highest_ack
    }

    /// `inSeqAt()`.
    pub fn in_seq_at(&self) -> SeqNum {
        self.in_seq_at
    }

    /// `unreliableInSeqAt()`: the newest unreliable sequence the channel
    /// has accepted, or `SEQ_NULL` before the first.
    pub fn unreliable_in_seq_at(&self) -> SeqNum {
        self.unreliable_in_seq_at
    }

    /// `hasAcks()` (udp_channel.hpp:176): the next send must carry
    /// acknowledgements (BW-02-03-05).
    pub fn has_acks(&self) -> bool {
        !self.acks_to_send.is_empty()
    }

    /// `numBufferedReceives_`: reliable packets parked ahead of
    /// `inSeqAt_` (BW-02-03-05).
    pub fn num_buffered_receives(&self) -> u32 {
        self.num_buffered_receives
    }

    /// `shouldAutoSwitchToSrcAddr()` (udp_channel.hpp:202).
    pub fn should_auto_switch_to_src_addr(&self) -> bool {
        self.should_auto_switch_to_src_addr
    }

    /// `shouldAutoSwitchToSrcAddr( b )` (udp_channel.cpp:1450): enabling
    /// is only allowed for indexed channels.
    pub fn set_should_auto_switch_to_src_addr(channel: &ChannelPtr, b: bool) {
        let mut this = channel.borrow_mut();
        this.should_auto_switch_to_src_addr = b;
        assert!(
            !this.should_auto_switch_to_src_addr || this.is_indexed(),
            "MF_ASSERT( !shouldAutoSwitchToSrcAddr_ || this->isIndexed() )"
        );
    }

    /// `numDuplicatePackets()`: the receive-side duplicate tally the
    /// reference keeps in `PacketReceiverStats` (BW-02-03-05).
    pub fn num_duplicate_packets(&self) -> u32 {
        self.num_duplicate_packets
    }

    /// `minInactivityResendDelay()`, in micros: the first check may not
    /// come before this, whatever the detection period is (BW-02-03-04).
    pub fn min_inactivity_resend_delay(&self) -> u64 {
        self.min_inactivity_resend_delay
    }

    /// `roundTripTimeInSeconds()` (channel.hpp), in seconds.
    pub fn round_trip_time_in_seconds(&self) -> f64 {
        self.round_trip_time as f64 / 1_000_000.0
    }

    /// `hasUnackedPackets()`: a send window exists to drain.
    pub fn has_unacked_packets(&self) -> bool {
        self.oldest_unacked_seq != SEQ_NULL
    }

    /// `hasRemoteFailed()`.
    pub fn has_remote_failed(&self) -> bool {
        self.has_remote_failed
    }

    /// `isAnonymous()`.
    pub fn is_anonymous(&self) -> bool {
        self.is_anonymous
    }

    /// `isCondemned()`.
    pub fn is_condemned(&self) -> bool {
        self.is_condemned
    }

    /// `isOwnedByInterface()` (udp_channel.hpp:181): the interface is the
    /// only one left to clean this channel up.
    pub fn is_owned_by_interface(&self) -> bool {
        !self.is_destroyed && (self.is_anonymous || self.is_condemned)
    }

    /// `lastReliableSendTime()`, in micros.
    pub fn last_reliable_send_time(&self) -> u64 {
        self.last_reliable_send_time
    }

    /// `lastReliableResendTime_`, in micros: the last time a windowed
    /// packet went back out. The resend timing reads this (BW-02-03-04).
    pub fn last_reliable_resend_time(&self) -> u64 {
        self.last_reliable_resend_time
    }

    /// `numPacketsResent()` (udp_channel.hpp:257).
    pub fn num_packets_resent(&self) -> u32 {
        self.num_packets_resent
    }

    /// `unackedCriticalSeq()`: the seq holding the critical unacked
    /// message, or `SEQ_NULL`. Cleared by [`UdpChannel::handle_ack`].
    pub fn unacked_critical_seq(&self) -> SeqNum {
        self.unacked_critical_seq
    }

    /// `c_str()` (udp_channel.cpp:1465): `udp:` plus the address, the id
    /// for indexed channels, and an exclamation mark for condemned ones.
    pub fn c_str(&self) -> String {
        let mut string = format!("udp:{}", self.addr);
        if self.is_indexed() {
            string.push_str(&format!("/{}", self.id));
        }
        if self.is_condemned {
            string.push('!');
        }
        string
    }

    /// A test hook driving `oldestUnackedSeq_` past the seq ring seam.
    #[cfg(test)]
    pub fn force_unacked(&mut self, seq: SeqNum) {
        self.oldest_unacked_seq = seq;
    }

    /// A test hook parking the outgoing sequence at `seq` (the reference
    /// reaches this state through `initFromStream`, udp_channel.cpp:107),
    /// so the sends that follow land where the test wants them.
    #[cfg(test)]
    pub fn force_out_seq_at(&mut self, seq: SeqNum) {
        self.small_out_seq_at = seq;
        self.large_out_seq_at = seq;
        self.highest_ack = seq_mask(seq.wrapping_sub(1));
    }

    /// A test hook parking the incoming sequence at `seq`, so a receive
    /// chain can straddle the seq ring seam.
    #[cfg(test)]
    pub fn force_in_seq_at(&mut self, seq: SeqNum) {
        self.in_seq_at = seq;
    }

    /// A test hook raising the channel version, the state the reference
    /// reaches through `clearState`'s indexed bump (udp_channel.cpp:341).
    #[cfg(test)]
    pub fn force_version(&mut self, version: ChannelVersion) {
        self.version = version;
    }

    /// A test hook ageing `lastReceivedTime_` for the condemned sweep.
    #[cfg(test)]
    pub fn force_last_received_time(&mut self, micros: u64) {
        self.last_received_time = micros;
    }

    /// A test hook exposing the unacked ring's slots.
    #[cfg(test)]
    pub fn unacked_packets(&self) -> &CircularArray<Option<UnackedPacket>> {
        &self.unacked_packets
    }

    /// A test hook marking `seq` critical until an ack clears it. The real
    /// setter arrives with the reliable-order machinery (BW-02-03-06):
    /// `unackedCriticalSeq_` is set when a packet carrying an unacked
    /// critical message is filed.
    #[cfg(test)]
    pub fn force_unacked_critical_seq(&mut self, seq: SeqNum) {
        self.unacked_critical_seq = seq;
    }

    /// A test hook ageing `lastReliableSendTime_`.
    #[cfg(test)]
    pub fn force_last_reliable_send_time(&mut self, micros: u64) {
        self.last_reliable_send_time = micros;
    }
}

/// `InactivityTimeoutChecker` (channel.cpp:34): fire the channel's
/// inactivity timeout when nothing arrived for the exception period.
struct InactivityTimeoutChecker {
    channel: Weak<RefCell<UdpChannel>>,
    inactivity_exception_period: u64,
}

impl TimerHandler<u64> for InactivityTimeoutChecker {
    fn handle_timeout(&self, _handle: TimerHandle<u64>, _user: usize) {
        let channel = match self.channel.upgrade() {
            Some(channel) => channel,
            None => return,
        };

        let now = UdpChannel::now(&channel);
        let expired = {
            let this = channel.borrow();
            now.saturating_sub(this.last_received_time) > self.inactivity_exception_period
        };

        if expired {
            UdpChannel::on_channel_inactivity_timeout(&channel);
        }
    }
}

/// The bridge from a channel handle to the [`ChannelRef`] seam: `Rc<T>`
/// coerces to `Rc<dyn ChannelRef>` only through a `T: ChannelRef`, so
/// the error paths' lookups wrap the handle in this thin carrier.
/// (`ChannelRef` lives on the interface; the reference needs no bridge
/// because `UDPChannel *` already is the slice.)
pub struct ChannelRefHandle(pub(crate) ChannelPtr);

impl ChannelRef for ChannelRefHandle {
    fn c_str(&self) -> String {
        self.0.borrow().c_str()
    }

    fn is_internal(&self) -> bool {
        self.0.borrow().is_internal()
    }

    fn has_remote_failed(&self) -> bool {
        self.0.borrow().has_remote_failed
    }

    fn set_remote_failed(&self) {
        UdpChannel::set_remote_failed(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_dispatcher::EventDispatcher;
    use crate::misc::{SEQ_MASK, SEQ_SIZE};
    use std::cell::Cell;

    /// An interface on a pinned clock with no socket: the channel
    /// machinery needs only the dispatcher and the maps.
    fn interface(clock: &Rc<Cell<u64>>) -> Rc<NetworkInterface> {
        let clock_cell = Rc::clone(clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_cell.get()
        })));
        NetworkInterface::new_with_dispatcher(
            crate::network_interface::InterfaceType::Internal,
            dispatcher,
        )
    }

    fn addr(ip: u32) -> Address {
        Address::new(ip, platform::socket::htons(20))
    }

    /// The window sizes the traits and id pick (udp_channel.cpp:27-29).
    #[test]
    fn window_sizes_follow_the_traits_and_id() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);

        let internal = UdpChannel::new(
            &interface,
            addr(0x0100_0001),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert_eq!(internal.borrow().window_size(), INTERNAL_CHANNEL_SIZE);
        assert_eq!(internal.borrow().window_size(), 4096);
        assert_eq!(
            internal.borrow().round_trip_time_in_seconds(),
            0.1,
            "internal channels assume a fast link"
        );

        let external = UdpChannel::new(
            &interface,
            addr(0x0100_0002),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert_eq!(external.borrow().window_size(), EXTERNAL_CHANNEL_SIZE);
        assert_eq!(external.borrow().window_size(), 256);
        assert_eq!(
            external.borrow().round_trip_time_in_seconds(),
            1.0,
            "external channels assume a second"
        );

        let indexed = UdpChannel::new(
            &interface,
            addr(0x0100_0003),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            7,
        );
        assert_eq!(indexed.borrow().window_size(), INDEXED_CHANNEL_SIZE);
        assert_eq!(indexed.borrow().window_size(), 512);
        assert!(indexed.borrow().is_indexed());
        assert_eq!(indexed.borrow().c_str(), "udp:3.0.0.1:20/7");

        // The constructor's remaining bookkeeping: the resend delay is the
        // default in stamps and the unreliable ring starts unset.
        assert_eq!(
            indexed.borrow().min_inactivity_resend_delay(),
            1_000_000,
            "the default delay is a second"
        );
        assert_eq!(indexed.borrow().unreliable_in_seq_at(), SEQ_NULL);
    }

    /// Construction registers a non-indexed channel; `Address::NONE`
    /// deregisters it; indexed channels never touch the map.
    #[test]
    fn registration_follows_the_address() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_0064);

        let channel = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert!(channel.borrow().is_established());
        let found = interface
            .find_channel(address, false)
            .expect("registered on construction");
        assert!(Rc::ptr_eq(&found, &channel));

        UdpChannel::set_address_fn(&channel, NONE);
        assert!(interface.find_channel(address, false).is_none());
        assert!(!channel.borrow().is_established());

        UdpChannel::set_address_fn(&channel, address);
        let again = interface
            .find_channel(address, false)
            .expect("reregistered");
        assert!(Rc::ptr_eq(&again, &channel));

        let indexed = UdpChannel::new(
            &interface,
            addr(0x0100_0065),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            3,
        );
        assert!(
            interface.find_channel(addr(0x0100_0065), false).is_none(),
            "indexed channels are not registered with the nub"
        );
        assert!(!indexed.borrow().is_destroyed());
    }

    /// `UDPChannel::get`: claims the anonymous channel (clearing the
    /// flag), hands back a condemned one with a warning, and builds a
    /// fresh internal channel when there is none.
    #[test]
    fn get_claims_anonymous_or_builds_fresh() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_00c8);

        let built = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert!(
            !built.borrow().is_anonymous(),
            "an explicit channel is never anonymous"
        );

        // An anonymous one found by `get` is claimed outright.
        UdpChannel::set_is_anonymous(&built, true);
        let claimed = UdpChannel::get(&interface, address);
        assert!(Rc::ptr_eq(&claimed, &built));
        assert!(!claimed.borrow().is_anonymous());

        // A condemned one comes back as-is, with the warning.
        {
            let mut this = claimed.borrow_mut();
            this.force_unacked(5);
        }
        UdpChannel::condemn(&claimed);
        assert!(claimed.borrow().is_condemned());
        assert!(
            interface.find_channel(address, false).is_some(),
            "the condemned channel is still registered while it drains"
        );
        let condemned = UdpChannel::get(&interface, address);
        assert!(Rc::ptr_eq(&condemned, &claimed));
        assert!(condemned.borrow().is_condemned());

        // A fresh address builds a new internal channel.
        let fresh = UdpChannel::get(&interface, addr(0x0100_00c9));
        assert!(fresh.borrow().is_internal());
        assert!(!fresh.borrow().is_anonymous());
        assert!(Rc::ptr_eq(
            &interface.find_channel(addr(0x0100_00c9), false).unwrap(),
            &fresh
        ));
    }

    /// The inactivity checker destroys a channel that has stopped
    /// receiving; receiving again pushes the verdict out.
    #[test]
    fn inactivity_destroys_a_quiet_channel() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_012c);

        let channel = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::start_inactivity_detection(&channel, 0.5, 0.1);
        assert!(!channel.borrow().is_destroyed());

        // Half a second of quiet passes the period; the check fires.
        clock.set(1_000_000 + 600_000);
        interface.dispatcher().process_timers();
        assert!(
            channel.borrow().is_destroyed(),
            "a channel quiet past its period is destroyed"
        );
        assert!(interface.find_channel(address, false).is_none());

        // Traffic inside the period keeps the channel alive.
        let lively = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::start_inactivity_detection(&lively, 0.5, 0.1);
        clock.set(1_000_000 + 600_000 + 100_000);
        UdpChannel::on_packet_received(&lively, 100);
        assert_eq!(lively.borrow().num_data_units_received(), 1);
        assert_eq!(lively.borrow().num_bytes_received(), 100);
        interface.dispatcher().process_timers();
        assert!(
            !lively.borrow().is_destroyed(),
            "fresh traffic holds the line"
        );
        UdpChannel::destroy(&lively);
    }

    /// `destroy` closes the channel exactly once, fires the listener, and
    /// frees the map slot; `condemn` twice warns and does nothing new.
    #[test]
    fn destroy_is_idempotent_and_reports_to_the_listener() {
        struct Recorder {
            gone: Cell<bool>,
        }
        impl ChannelListener for Recorder {
            fn on_channel_gone(&self, _channel: &ChannelPtr) {
                self.gone.set(true);
            }
        }

        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_0190);

        let channel = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let recorder = Rc::new(Recorder {
            gone: Cell::new(false),
        });
        channel
            .borrow_mut()
            .set_channel_listener(Some(Rc::clone(&recorder) as Rc<dyn ChannelListener>));
        assert!(channel.borrow().is_connected());

        UdpChannel::destroy(&channel);
        assert!(channel.borrow().is_destroyed());
        assert!(!channel.borrow().is_connected());
        assert!(recorder.gone.get(), "the listener heard of the death");
        assert!(interface.find_channel(address, false).is_none());

        // A second destroy is a no-op, and the listener hears nothing.
        recorder.gone.set(false);
        UdpChannel::destroy(&channel);
        assert!(!recorder.gone.get(), "no second report");
    }

    /// The constructor's bookkeeping as the accessors report it: the ring
    /// fields start at their reference values, the counters at zero, and
    /// ownership follows anonymity and condemnation.
    #[test]
    fn the_accessors_report_the_constructor_bookkeeping() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_0258);

        let channel = UdpChannel::new(&interface, address, Traits::External, 1.0, CHANNEL_ID_NULL);
        let this = channel.borrow();
        assert_eq!(this.addr(), address);
        assert!(!this.is_destroyed());
        assert!(this.is_connected());
        assert_eq!(
            this.last_received_time(),
            1_000_000,
            "set from the dispatcher's now"
        );
        assert_eq!(this.num_data_units_sent(), 0);
        assert_eq!(this.num_data_units_received(), 0);
        assert_eq!(this.num_bytes_sent(), 0);
        assert_eq!(this.num_bytes_received(), 0);
        assert_eq!(this.traits(), Traits::External);
        assert!(this.is_external());
        assert!(!this.is_internal());
        assert_eq!(this.id(), CHANNEL_ID_NULL);
        assert!(!this.is_indexed());
        assert_eq!(this.version(), 0);
        assert_eq!(this.creation_version(), 0);
        assert!(this.is_established());
        assert_eq!(this.window_size(), EXTERNAL_CHANNEL_SIZE);
        assert_eq!(this.small_out_seq_at(), 0);
        assert_eq!(this.large_out_seq_at(), 0);
        assert_eq!(this.oldest_unacked_seq(), SEQ_NULL);
        assert_eq!(
            this.highest_ack(),
            SEQ_MASK,
            "the ack is the seq before the first"
        );
        assert_eq!(this.in_seq_at(), 0);
        assert_eq!(this.round_trip_time_in_seconds(), 1.0);
        assert!(!this.has_unacked_packets());
        assert!(!this.has_remote_failed());
        assert!(!this.is_anonymous());
        assert!(!this.is_condemned());
        assert!(
            !this.is_owned_by_interface(),
            "explicit channels outlive the interface"
        );
        assert_eq!(this.last_reliable_send_time(), 0);
        assert_eq!(this.min_inactivity_resend_delay(), 1_000_000);
        assert_eq!(this.c_str(), format!("udp:{address}"));
        // The bundle starts on one empty packet, and `clearBundle`
        // resets it back to that.
        assert_eq!(this.bundle().num_data_units(), 1);
        drop(this);

        channel.borrow_mut().clear_bundle();
        channel.borrow_mut().set_channel_listener(None);
        // The mutable accessor is the sender's handle to that bundle, and
        // a cleared one still has room to write into. The RefMut has to
        // outlive the borrow it hands out.
        let mut this = channel.borrow_mut();
        let bundle = this.bundle_mut();
        assert_eq!(bundle.num_data_units(), 1);
        assert!(bundle.free_bytes_in_last_data_unit() > 0);
        drop(this);
    }

    /// `condemn` twice: the second one warns and does nothing new.
    #[test]
    fn condemn_twice_warns_and_does_nothing_new() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_02bc);

        let channel = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        channel.borrow_mut().force_unacked(6);
        UdpChannel::condemn(&channel);
        UdpChannel::condemn(&channel);
        assert!(
            !channel.borrow().is_destroyed(),
            "a warned condemn is a no-op"
        );
        assert_eq!(interface.condemned_size(), 1, "still one condemned slot");
        interface.condemned_clear_for_test();
    }

    /// A channel whose interface has gone: nothing to report to and
    /// nothing to deregister from, and no detector to arm.
    #[test]
    fn a_channel_without_an_interface_reports_nowhere() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_0320);

        let channel = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert!(channel.borrow().is_established());
        drop(interface);

        // No interface to arm the checker on: the call gives up.
        UdpChannel::start_inactivity_detection(&channel, 0.5, 0.1);
        // And a destroy has nothing to tell of itself.
        UdpChannel::destroy(&channel);
        assert!(channel.borrow().is_destroyed());
    }

    /// The inactivity checker's own death: it holds the channel only
    /// weakly, so a dropped channel ends the timer quietly rather than
    /// destroying anything.
    #[test]
    fn the_inactivity_checker_survives_its_channel() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_0384);

        let channel = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::start_inactivity_detection(&channel, 0.5, 0.1);

        // The map holds the channel's other strong reference, so the
        // local drop alone leaves it alive: deregister first, so the
        // checker's weak handle is really the last one.
        assert!(interface.deregister_channel(&channel));
        drop(channel);
        clock.set(1_000_000 + 600_000);
        interface.dispatcher().process_timers();
        assert_eq!(
            interface.condemned_size(),
            0,
            "a channel that is already gone is not destroyed twice"
        );
    }

    /// A listener that overrides nothing still hears both events through
    /// the trait's defaults -- the reference's no-op implementations.
    #[test]
    fn a_listener_that_overrides_nothing_still_hears_both_events() {
        struct Silent;
        impl ChannelListener for Silent {}

        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_03e8),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        let silent = Rc::new(Silent);
        channel
            .borrow_mut()
            .set_channel_listener(Some(Rc::clone(&silent) as Rc<dyn ChannelListener>));
        silent.on_channel_send(&channel);
        // The default hears the death as a no-op too.
        UdpChannel::destroy(&channel);
        assert!(channel.borrow().is_destroyed());
    }

    /// `setRemoteFailed` cancels an internal channel's outstanding
    /// requests with `ChannelLost`, and an anonymous channel cleans
    /// itself up because nobody else will.
    #[test]
    fn remote_failure_cancels_requests_and_cleans_anonymous() {
        use crate::interfaces::ReplyMessageHandler;
        use crate::nub_exception::NubException;
        use crate::reply_order::ReplyOrder;
        use crate::request_manager::RequestManager;
        use crate::unpacked_message_header::UnpackedMessageHeader;
        use foundation::stream::BinaryStream;

        /// A reply slot that records if it is ever delivered: it must not
        /// be -- a lost channel reports `ChannelLost`, never a reply.
        struct Empty {
            delivered: Rc<Cell<bool>>,
        }
        impl ReplyMessageHandler for Empty {
            fn handle_message(
                &self,
                _source: &Address,
                _header: &UnpackedMessageHeader,
                _data: &mut BinaryStream,
                _arg: usize,
            ) {
                self.delivered.set(true);
            }

            fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
        }

        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let address = addr(0x0100_01f4);

        // A request aimed at the channel dies with it.
        let manager = Rc::new(RequestManager::new(
            false,
            Rc::clone(&interface.dispatcher()),
        ));
        interface.set_request_manager(Some(Rc::clone(&manager)));
        let delivered = Rc::new(Cell::new(false));
        let order = ReplyOrder::new(
            Rc::new(Empty {
                delivered: delivered.clone(),
            }),
            9,
            1_000_000,
            0,
        );
        let mut frame = [0u8; 16];
        let _ = manager.add_reply_order(&order, Some(address), &mut frame);
        assert_eq!(manager.num_pending(), 1);

        let channel = UdpChannel::new(
            &interface,
            address,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_remote_failed(&channel);
        assert!(channel.borrow().has_remote_failed());
        assert_eq!(
            manager.num_pending(),
            0,
            "the remote failure took the request off the table"
        );
        assert!(
            !delivered.get(),
            "a lost channel reports ChannelLost, it never delivers a reply"
        );

        // An anonymous channel at another address cleans itself up.
        let anonymous = interface
            .find_channel(addr(0x0100_01f5), true)
            .expect("created");
        assert!(anonymous.borrow().is_anonymous());
        assert!(anonymous.borrow().is_owned_by_interface());
        UdpChannel::set_remote_failed(&anonymous);
        assert!(anonymous.borrow().is_destroyed());
        assert!(
            interface.find_channel(addr(0x0100_01f5), false).is_none(),
            "the anonymous channel deregistered itself"
        );
    }

    /// `useNextSequenceID` plus `addResendTimer`, the way the reference's
    /// send path drives them: one packet per seq, no reliable orders yet.
    fn send(channel: &ChannelPtr) -> bool {
        let seq = UdpChannel::use_next_sequence_id(channel);
        let mut packet = Packet::new();
        packet.set_seq(seq);
        UdpChannel::add_resend_timer(channel, seq, Rc::new(packet), &[])
    }

    /// `send`, answering the sequence number it handed out.
    fn send_seq(channel: &ChannelPtr) -> SeqNum {
        let seq = UdpChannel::use_next_sequence_id(channel);
        let mut packet = Packet::new();
        packet.set_seq(seq);
        assert!(
            UdpChannel::add_resend_timer(channel, seq, Rc::new(packet), &[]),
            "the send window has room"
        );
        seq
    }

    /// The acceptance fixture: the send window works across the `SEQ_SIZE`
    /// seam. Three sends straddle the wrap, an out-of-order ack crosses
    /// back over it, and a cumulative ack drains the ring -- every step is
    /// plain masked arithmetic; nothing special-cases the seam.
    #[test]
    fn the_window_and_acks_wrap_through_the_seq_ring() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00ca),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // Park the outgoing seq just before the seam: the next three
        // sends land on SEQ_MASK-1, SEQ_MASK and 0.
        channel.borrow_mut().force_out_seq_at(SEQ_MASK - 1);
        assert!(send(&channel));
        assert!(send(&channel));
        assert!(send(&channel));

        {
            let this = channel.borrow();
            assert_eq!(this.small_out_seq_at(), 1, "the seq wrapped around to 1");
            assert_eq!(this.large_out_seq_at(), 1);
            assert_eq!(this.oldest_unacked_seq(), SEQ_MASK - 1);
            assert_eq!(this.send_window_usage(), 3);
            assert!(this.is_in_sent_window(SEQ_MASK - 1));
            assert!(this.is_in_sent_window(SEQ_MASK));
            assert!(this.is_in_sent_window(0), "the window spans the seam");
            assert!(!this.is_in_sent_window(1), "one past the sent head");
            assert!(!this.is_in_sent_window(SEQ_MASK - 2), "before the oldest");
            assert!(this.is_unacked(0));
        }

        // A critical seq parked on the wrapped packet: acking it clears it.
        channel.borrow_mut().force_unacked_critical_seq(0);

        // The out-of-order ack of the wrapped seq: the highest ack crosses
        // the seam backwards, the oldest stays put.
        assert!(UdpChannel::handle_ack(&channel, 0));
        {
            let this = channel.borrow();
            assert_eq!(this.highest_ack(), 0, "the highest ack crossed the seam");
            assert_eq!(
                this.oldest_unacked_seq(),
                SEQ_MASK - 1,
                "an out-of-order ack does not move the oldest"
            );
            assert!(!this.is_unacked(0));
            assert_eq!(this.unacked_critical_seq(), SEQ_NULL);
            assert_eq!(
                this.send_window_usage(),
                3,
                "usage spans [oldest, large): the hole is not the oldest yet"
            );
        }

        // A stale re-ack finds an empty slot and does nothing.
        assert!(UdpChannel::handle_ack(&channel, 0));
        assert_eq!(channel.borrow().send_window_usage(), 3);

        // An out-of-range seq is refused: seqMask(SEQ_NULL) != SEQ_NULL.
        assert!(!UdpChannel::handle_ack(&channel, SEQ_NULL));

        // A cumulative ack over the seam drains what is left, up to but
        // not including endSeq.
        assert!(UdpChannel::handle_cumulative_ack(&channel, 1));
        {
            let this = channel.borrow();
            assert_eq!(this.oldest_unacked_seq(), SEQ_NULL);
            assert_eq!(this.send_window_usage(), 0, "the ring drained");
            assert!(!this.has_unacked_packets());
            assert_eq!(this.highest_ack(), 0, "0 is still the newest ack");
        }

        // With nothing unacked, further acks are no-ops.
        assert!(UdpChannel::handle_cumulative_ack(&channel, 1));
        assert!(UdpChannel::handle_ack(&channel, SEQ_MASK - 1));
    }

    /// `handleCumulativeAck` refuses an endSeq past what has been sent, and
    /// an out-of-range endSeq outright.
    #[test]
    fn a_future_cumulative_ack_refuses() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00cb),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        channel.borrow_mut().force_out_seq_at(0);
        assert!(send(&channel)); // seq 0 sent; the head is now 1
        assert_eq!(channel.borrow().small_out_seq_at(), 1);

        assert!(
            !UdpChannel::handle_cumulative_ack(&channel, 2),
            "acks for packets not yet sent"
        );
        assert!(channel.borrow().has_unacked_packets(), "nothing was acked");
        assert!(
            !UdpChannel::handle_cumulative_ack(&channel, SEQ_NULL),
            "seqMask(SEQ_NULL) != SEQ_NULL"
        );
        assert_eq!(channel.borrow().send_window_usage(), 1);
    }

    /// One clean round trip folds into the average exactly once
    /// (udp_channel.cpp:989): nine tenths of the old value plus a tenth of
    /// the sample. Stale and never-sent seqs leave it untouched.
    #[test]
    fn an_ack_updates_the_rtt_average_once() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00cc),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert_eq!(channel.borrow().round_trip_time_in_seconds(), 0.1);

        channel.borrow_mut().force_out_seq_at(0);
        assert!(send(&channel));

        clock.set(1_300_000); // a 0.3s round trip
        assert!(UdpChannel::handle_ack(&channel, 0));
        assert_eq!(
            channel.borrow().round_trip_time_in_seconds(),
            0.12,
            "(0.1 * 9 + 0.3) / 10"
        );

        // A stale re-ack and an unsent seq both leave the average alone.
        assert!(UdpChannel::handle_ack(&channel, 0));
        assert!(UdpChannel::handle_ack(&channel, 5));
        assert_eq!(channel.borrow().round_trip_time_in_seconds(), 0.12);
    }

    /// A resent packet's ack does not fold into the average
    /// (udp_channel.cpp:987): its round trip was not a round trip.
    #[test]
    fn an_ack_for_a_resent_packet_leaves_the_average_alone() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00cd),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        channel.borrow_mut().force_out_seq_at(0);
        for _ in 0..257 {
            send(&channel);
        }
        assert_eq!(
            channel.borrow().small_out_seq_at(),
            255,
            "the window is full"
        );
        assert!(!channel.borrow().has_seen_overflow_warning());

        // The packet queued at the head is old enough to go back out.
        clock.set(3_000_000);
        assert!(!send(&channel)); // seq 257 resends the packet at seq 255
        assert_eq!(channel.borrow().last_reliable_resend_time(), 3_000_000);

        // Its ack comes back: no round-trip sample for a resent packet.
        assert!(UdpChannel::handle_ack(&channel, 255));
        assert_eq!(
            channel.borrow().round_trip_time_in_seconds(),
            1.0,
            "a resent packet does not sample the RTT"
        );
        assert_eq!(channel.borrow().oldest_unacked_seq(), 0);

        // The slot ahead of the head is now empty: the next queued send
        // goes back out on its own (the missing predecessor is no
        // obstacle).
        assert!(!send(&channel));
        assert_eq!(channel.borrow().small_out_seq_at(), 257);
    }

    /// A packet carrying reliable orders files them with its window slot,
    /// the way the send path records what a resend may piggyback.
    #[test]
    fn a_reliable_order_rides_with_its_packet() {
        use crate::interfaces::ReplyMessageHandler;
        use crate::misc::Reason;
        use crate::nub_exception::NubException;
        use crate::unpacked_message_header::UnpackedMessageHeader;
        use foundation::stream::BinaryStream;

        /// A placeholder handler that accepts anything and says nothing.
        struct NoOp;
        impl ReplyMessageHandler for NoOp {
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

        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00ce),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        channel.borrow_mut().force_out_seq_at(0);

        let handler: Rc<dyn ReplyMessageHandler> = Rc::new(NoOp);
        let order = crate::reliable_order::ReliableOrder {
            packet: 0,
            offset: 1,
            length: 5,
            part_of_request: false,
        };
        let seq = UdpChannel::use_next_sequence_id(&channel);
        let mut packet = Packet::new();
        packet.set_seq(seq);
        assert!(UdpChannel::add_resend_timer(
            &channel,
            seq,
            Rc::new(packet),
            std::slice::from_ref(&order)
        ));

        // The orders ride with the slot.
        let this = channel.borrow();
        let slot = this.unacked_packets().get(seq as usize).as_ref();
        assert!(slot.is_some(), "the slot is filed");
        let orders = slot.unwrap().reliable_orders();
        assert_eq!(orders, [order]);

        // The placeholder's whole contract: say nothing.
        handler.handle_message(
            &Address::new(0, 0),
            &UnpackedMessageHeader::new(1),
            &mut BinaryStream::new(),
            0,
        );
        handler.handle_exception(&NubException::new(Reason::TimerExpired, None), 0);
    }

    /// The full cycle of a saturated window (udp_channel.cpp:421): packets
    /// queue past the window, the queue passes half the safety ceiling and
    /// the channel warns once, cumulative acks drain the queue, and once
    /// the queue is under a third of the ceiling on the next fill, the
    /// warning clears.
    #[test]
    fn a_full_window_overflows_until_it_warns_and_then_drains() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00ce),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        channel.borrow_mut().force_out_seq_at(0);

        // 768 sends: the first 255 fit the window, the 256th is the first
        // refusal, and the overflow grows past half the 1024 ceiling.
        for i in 0..768 {
            let sent = send(&channel);
            if i < 255 {
                assert!(sent);
            } else {
                assert!(!sent, "send #{} is queued as overflow", i + 1);
            }
        }
        {
            let this = channel.borrow();
            assert!(this.has_seen_overflow_warning(), "513 queued > 1024 / 2");
            assert_eq!(this.send_window_usage(), 768, "usage counts the overflow");
            assert_eq!(this.num_outstanding_packets(), 769, "plus the bundle");
            assert_eq!(this.send_window_size(), 1024, "the ring doubled twice");
            assert_eq!(this.oldest_unacked_seq(), 0);
        }

        // The packet queued at the head is now old enough: it goes back
        // out and the head advances by one.
        clock.set(3_000_000);
        assert!(!send(&channel)); // seq 768
        assert_eq!(channel.borrow().small_out_seq_at(), 256);
        assert_eq!(channel.borrow().last_reliable_resend_time(), 3_000_000);

        // Cumulative acks drain the window: every ack makes room and the
        // queued packets flow out behind it.
        assert!(UdpChannel::handle_cumulative_ack(&channel, 256));
        {
            let this = channel.borrow();
            assert_eq!(this.oldest_unacked_seq(), 256);
            assert_eq!(this.small_out_seq_at(), 512);
        }
        assert!(UdpChannel::handle_cumulative_ack(&channel, 512));
        {
            let this = channel.borrow();
            assert_eq!(this.oldest_unacked_seq(), 512);
            assert_eq!(this.small_out_seq_at(), 768);
        }
        assert!(UdpChannel::handle_cumulative_ack(&channel, 768));
        {
            let this = channel.borrow();
            assert_eq!(this.oldest_unacked_seq(), 768);
            assert_eq!(this.small_out_seq_at(), 769);
            assert_eq!(this.send_window_usage(), 1);
            assert!(
                this.has_seen_overflow_warning(),
                "only addResendTimer checks"
            );
        }

        // The drain left the window roomy again: the next send goes
        // straight out instead of queueing.
        assert!(send(&channel)); // seq 769
        assert_eq!(channel.borrow().small_out_seq_at(), 770);

        // The window fills once more, and this time the overflow check
        // finds the queue drained and clears the warning.
        let mut last = true;
        for _ in 0..254 {
            last = send(&channel); // seqs 770..1023
        }
        assert!(!last, "the window filled again");
        {
            let this = channel.borrow();
            assert!(!this.has_seen_overflow_warning(), "1 < 1024 / 3");
            assert_eq!(this.send_window_usage(), 256);
            assert_eq!(this.send_window_size(), 1024);
            assert_eq!(this.window_size(), 256, "the ring outgrew the window");
        }
    }

    /// `doubleSize` keeps the ring ahead of the span (udp_channel.cpp:856):
    /// a full window's worth plus one doubles it, and the carried slots
    /// survive the doubling at their absolute indexes.
    #[test]
    fn the_ring_doubles_when_the_span_outgrows_it() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00cf),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        channel.borrow_mut().force_out_seq_at(0);

        for _ in 0..256 {
            send(&channel);
        }
        assert_eq!(channel.borrow().send_window_size(), 256);
        assert!(channel.borrow().is_unacked(255));

        send(&channel); // seq 256: the span is now 257
        assert_eq!(channel.borrow().send_window_size(), 512);
        assert!(channel.borrow().is_unacked(5), "slots survive the doubling");

        for _ in 0..255 {
            send(&channel); // seqs 257..511
        }
        assert_eq!(channel.borrow().send_window_size(), 512);

        send(&channel); // seq 512: the span is now 513
        assert_eq!(channel.borrow().send_window_size(), 1024);
        assert!(channel.borrow().is_unacked(300));
        assert!(channel.borrow().is_unacked(512));
        assert_eq!(channel.borrow().window_size(), 256);
    }

    /// The internal and indexed windows overflow against their own
    /// ceilings (udp_channel.cpp:34): 8192 and 4096 queued packets, so a
    /// freshly full window is nowhere near warning.
    #[test]
    fn internal_and_indexed_channels_overflow_against_their_own_ceilings() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);

        let internal = UdpChannel::new(
            &interface,
            addr(0x0100_00d0),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        internal.borrow_mut().force_out_seq_at(0);
        for _ in 0..4096 {
            send(&internal);
        }
        assert!(!internal.borrow().has_seen_overflow_warning());
        send(&internal); // seq 4096 doubles the ring
        {
            let this = internal.borrow();
            assert_eq!(this.send_window_size(), 8192);
            assert!(!this.has_seen_overflow_warning());
        }

        let indexed = UdpChannel::new(
            &interface,
            addr(0x0100_00d1),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            7,
        );
        indexed.borrow_mut().force_out_seq_at(0);
        for _ in 0..512 {
            send(&indexed);
        }
        assert!(!indexed.borrow().has_seen_overflow_warning());
        send(&indexed); // seq 512 doubles the ring
        {
            let this = indexed.borrow();
            assert_eq!(this.send_window_size(), 1024);
            assert!(!this.has_seen_overflow_warning());
        }
    }

    // -- The resend timing (BW-02-03-04) ----------------------------------

    /// The acceptance fixture: 单次可控丢包后仅交付一次. One packet goes
    /// out and its ack is "lost"; the irregular-channel clock gives the
    /// packet its resend check every period, and the resend period is
    /// `max( RTT*2, minInactivityResendDelay )` = 2s here (the external
    /// constructor RTT). The packet goes back out exactly once -- the
    /// resend re-stamps its clock -- then the resend's ack clears the slot
    /// and the channel leaves the irregular collection: no second copy,
    /// no duplicate delivery.
    #[test]
    fn a_single_controlled_loss_is_delivered_exactly_once() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);
        // The reference's own lose-one-packet fixture scans every 0.1s
        // (test_reliable.cpp:217).
        interface.set_irregular_channels_resend_period(0.1);

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e1),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        // Both flags off, like `ExternalLink( false )` in the reference
        // fixture (test_reliable.cpp:221-222): the local flag files the
        // channel for the shared timer, the remote one makes the resend
        // decision time-based.
        UdpChannel::set_local_regular(&channel, false);
        UdpChannel::set_remote_regular(&channel, false);

        let seq = send_seq(&channel); // the loss: no ack will come for this
        assert_eq!(seq, 0);
        interface.add_irregular_channel_if_necessary(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // Nineteen ticks inside the resend period: quiet.
        for _ in 0..19 {
            clock.set(clock.get() + 100_000);
            interface.check_irregular_channels();
        }
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // Exactly two seconds old is still not old enough: the reference
        // test is strict (`>`).
        clock.set(clock.get() + 100_000);
        interface.check_irregular_channels();
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // One tick past: exactly one resend.
        clock.set(clock.get() + 100_000);
        interface.check_irregular_channels();
        assert_eq!(channel.borrow().num_packets_resent(), 1);
        {
            let this = channel.borrow();
            let slot = this.unacked_packets().get(seq as usize).as_ref().unwrap();
            assert!(slot.was_resent(), "the resent slot is marked");
            assert_eq!(
                slot.last_sent_time(),
                3_100_000,
                "the resend re-stamps the packet's clock"
            );
        }

        // However many more periods pass, the re-stamped clock holds the
        // line: still exactly one copy on the wire.
        for _ in 0..20 {
            clock.set(clock.get() + 100_000);
            interface.check_irregular_channels();
        }
        assert_eq!(channel.borrow().num_packets_resent(), 1);

        // The resend got through; its ack lands and the delivery is done.
        assert!(UdpChannel::handle_ack(&channel, seq));
        {
            let this = channel.borrow();
            assert_eq!(this.oldest_unacked_seq(), SEQ_NULL);
            assert!(!this.has_unacked_packets());
            assert_eq!(this.num_packets_resent(), 1);
        }

        // The next scan retires the served channel: nothing left to check.
        clock.set(clock.get() + 100_000);
        interface.check_irregular_channels();
        assert_eq!(interface.irregular_channels().borrow().size(), 0);
        assert_eq!(channel.borrow().num_packets_resent(), 1);
        interface.irregular_clear_for_test();
    }

    /// A later ack NACKs the earlier packets: on a channel whose remote
    /// sends regularly, `checkResendTimers` scans only up to the highest
    /// ack and resends anything whose last send predates it -- fresh
    /// packets, no timer involved. This is the NACK arm the regular
    /// channels live on.
    #[test]
    fn a_newer_ack_nacks_the_older_packet() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e2),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        // The remote stays regular (the constructor default): the scan is
        // NACK-driven, and the age arm never fires.

        let first = send_seq(&channel);
        let second = send_seq(&channel);
        assert_eq!(first, 0);
        assert_eq!(second, 1);

        // Only the second packet's ack arrives.
        assert!(UdpChannel::handle_ack(&channel, second));
        assert_eq!(channel.borrow().oldest_unacked_seq(), first);

        UdpChannel::check_resend_timers(&channel, &mut UdpBundle::new(0));
        {
            let this = channel.borrow();
            assert_eq!(
                this.num_packets_resent(),
                1,
                "the NACKed packet went back out"
            );
            let slot = this.unacked_packets().get(first as usize).as_ref().unwrap();
            assert!(slot.was_resent());
            // The acked packet keeps its seat: the scan stops at the
            // highest ack, so nothing past it is even looked at.
            assert!(!this.is_unacked(second));
            assert!(this.is_unacked(first));
        }

        // The channel is regular, so the shared collection never takes it.
        assert_eq!(interface.irregular_channels().borrow().size(), 0);
        interface.irregular_clear_for_test();
    }

    /// The resend period is `max( RTT*2, minInactivityResendDelay )`: an
    /// internal channel's RTT of 0.1s would ask for a 0.2s check, but the
    /// one-second inactivity floor holds it there.
    #[test]
    fn the_inactivity_floor_holds_the_resend_period() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e3),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert_eq!(channel.borrow().round_trip_time_in_seconds(), 0.1);
        UdpChannel::set_local_regular(&channel, false);
        UdpChannel::set_remote_regular(&channel, false);
        send(&channel);
        interface.add_irregular_channel_if_necessary(&channel);

        // 0.9s old: under the floor, nothing yet -- even though twice the
        // RTT has long passed.
        clock.set(clock.get() + 900_000);
        interface.check_irregular_channels();
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // Exactly 1s: the floor test is strict too.
        clock.set(clock.get() + 100_000);
        interface.check_irregular_channels();
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // One micro past the floor: the resend goes.
        clock.set(clock.get() + 1);
        interface.check_irregular_channels();
        assert_eq!(channel.borrow().num_packets_resent(), 1);
        interface.irregular_clear_for_test();
    }

    /// The throttle: a scan resends at most `windowSize_/8` packets. An
    /// external window of 256 caps one pass at 32, however much is old
    /// and NACKed at once.
    #[test]
    fn the_throttle_caps_a_scan_at_window_size_over_eight() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e4),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // Forty packets, every one of them NACKed by the ack of the last.
        let mut last = 0;
        for _ in 0..40 {
            last = send_seq(&channel);
        }
        assert!(UdpChannel::handle_ack(&channel, last));
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        UdpChannel::check_resend_timers(&channel, &mut UdpBundle::new(0));
        assert_eq!(
            channel.borrow().num_packets_resent(),
            256 / 8,
            "one scan, capped at windowSize_/8"
        );
    }

    /// A channel whose remote has failed is done: the scan returns before
    /// looking at anything, however old the packets are.
    #[test]
    fn a_remote_failed_channel_stops_resending() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        // Verbosity stays at its default: the reference logs the refusal
        // under debug verbosity, and the port does too.

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e5),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_remote_regular(&channel, false);
        send(&channel);

        clock.set(clock.get() + 10_000_000); // ten seconds old
        UdpChannel::set_remote_failed(&channel);
        UdpChannel::check_resend_timers(&channel, &mut UdpBundle::new(0));
        assert_eq!(channel.borrow().num_packets_resent(), 0);
    }

    /// An empty window scans to nothing, and holes in the window are
    /// skipped, not resurrected: the walk crosses emptied `NULL` slots
    /// without touching them.
    #[test]
    fn an_empty_window_and_holes_scans_to_nothing() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e7),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_remote_regular(&channel, false);

        // Nothing outstanding: the scan refuses at the gate.
        UdpChannel::check_resend_timers(&channel, &mut UdpBundle::new(0));
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // Three packets, the middle one acked: an irregular channel's scan
        // runs to `smallOutSeqAt_`, so it crosses the emptied slot.
        let first = send_seq(&channel);
        let middle = send_seq(&channel);
        let last = send_seq(&channel);
        assert!(UdpChannel::handle_ack(&channel, middle));

        // The middle ack NACKs the older packet -- the reference resends
        // on a later ack regardless of regularity -- and the walk steps
        // over the emptied hole to the fresh tail without touching it.
        UdpChannel::check_resend_timers(&channel, &mut UdpBundle::new(0));
        {
            let this = channel.borrow();
            assert_eq!(
                this.num_packets_resent(),
                1,
                "the NACKed packet went back out"
            );
            assert!(this.is_unacked(first), "a resend does not ack");
            assert!(!this.is_unacked(middle), "the hole stays empty");
            let tail = this.unacked_packets().get(last as usize).as_ref().unwrap();
            assert!(!tail.was_resent(), "the fresh tail was left alone");
        }
    }

    /// `condemn` marks the channel irregular so the shared timer keeps
    /// its drain moving, and `destroy` clears it from the collection
    /// (`clearState`'s `delIfNecessary`).
    #[test]
    fn condemn_marks_irregular_and_destroy_unfiles() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e6),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        send(&channel);
        UdpChannel::condemn(&channel);
        assert!(
            !UdpChannel::is_local_regular(&channel),
            "condemn marks the channel irregular"
        );
        assert!(
            !UdpChannel::is_remote_regular(&channel),
            "condemn marks the remote irregular too"
        );
        assert_eq!(
            interface.irregular_channels().borrow().size(),
            1,
            "the condemned channel files for the shared resend check"
        );

        // And the destructor's clearState takes it back out again.
        UdpChannel::destroy(&channel);
        assert_eq!(interface.irregular_channels().borrow().size(), 0);
    }

    // -- BW-02-03-05: the receive window ----------------------------------

    /// Deliver one reliable packet to the channel from its own address:
    /// the bookkeeping a real receive's footer walk does, minus the wire.
    fn receive(channel: &ChannelPtr, seq: SeqNum) -> AddToReceiveWindowResult {
        let src = channel.borrow().addr();
        let mut packet = Packet::new();
        packet.set_seq(seq);
        UdpChannel::add_to_receive_window(channel, Rc::new(packet), &src)
    }

    /// Unwrap the delivered chain, as the sequence numbers the caller
    /// gets to dispatch, in the order it gets to dispatch them.
    fn delivered(result: AddToReceiveWindowResult) -> Vec<SeqNum> {
        match result {
            AddToReceiveWindowResult::NextInWindow(chain) => {
                chain.iter().map(|packet| packet.seq()).collect()
            }
            _ => panic!("expected NextInWindow, got a different outcome"),
        }
    }

    /// THE acceptance fixture (验收: 按原版顺序派发): one packet of many is
    /// lost on the way in, the rest arrive buffered, and the hole's
    /// arrival hands the whole chain back in the original send order --
    /// the reference's lose-one-packet walk, at the window level.
    #[test]
    fn the_lost_packet_releases_the_buffer_in_the_original_order() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f0),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // Five packets go out (seqs 0..4); #1 is lost. 2..4 arrive
        // buffered, 3 last, so the buffering order is shuffled too.
        assert!(matches!(
            receive(&channel, 2),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert!(matches!(
            receive(&channel, 4),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert!(matches!(
            receive(&channel, 3),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert_eq!(channel.borrow().num_buffered_receives(), 3);
        assert_eq!(channel.borrow().in_seq_at(), 0);

        // #0 arrives, but the hole is still #1: it delivers alone.
        assert_eq!(delivered(receive(&channel, 0)), vec![0]);

        // The hole fills: the buffered tail drains with it, in send
        // order, not in arrival order.
        assert_eq!(
            delivered(receive(&channel, 1)),
            vec![1, 2, 3, 4],
            "the chain follows the original order, not the arrival order"
        );
        assert_eq!(channel.borrow().in_seq_at(), 5);
        assert_eq!(channel.borrow().num_buffered_receives(), 0);

        // A later packet hands back just itself.
        assert_eq!(delivered(receive(&channel, 5)), vec![5]);

        // Every reliable receive queued its acknowledgement -- the set
        // collapses the duplicates to come.
        assert!(channel.borrow().has_acks());
    }

    /// The buffered chain crosses the seq ring seam like everything else
    /// in the window arithmetic: no wrap special case anywhere.
    #[test]
    fn the_buffer_chain_crosses_the_seq_seam() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f1),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        channel.borrow_mut().force_in_seq_at(SEQ_MASK - 1);

        assert!(matches!(
            receive(&channel, SEQ_MASK),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert!(matches!(
            receive(&channel, 0),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert_eq!(
            delivered(receive(&channel, SEQ_MASK - 1)),
            vec![SEQ_MASK - 1, SEQ_MASK, 0]
        );
        assert_eq!(channel.borrow().in_seq_at(), 1);
    }

    /// `PACKET_IS_DUPLICATE`: a packet at or below `inSeqAt_` was seen;
    /// it counts as a duplicate and never re-buffers.
    #[test]
    fn duplicates_below_in_seq_at_are_discarded() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f2),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        receive(&channel, 0);
        assert!(matches!(
            receive(&channel, 0),
            AddToReceiveWindowResult::Duplicate
        ));
        assert!(matches!(
            receive(&channel, 2),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert_eq!(delivered(receive(&channel, 1)), vec![1, 2]);
        // 1 went out with the 2-chain: re-delivering it is a dup.
        assert!(matches!(
            receive(&channel, 1),
            AddToReceiveWindowResult::Duplicate
        ));
        assert_eq!(channel.borrow().num_duplicate_packets(), 2);
        assert_eq!(channel.borrow().num_buffered_receives(), 0);
    }

    /// An occupied slot holds the same seq: the re-delivery counts as a
    /// duplicate and does not double-book the slot.
    #[test]
    fn an_already_buffered_packet_is_counted_not_rebuffered() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f3),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        assert!(matches!(
            receive(&channel, 5),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert!(matches!(
            receive(&channel, 5),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert_eq!(channel.borrow().num_buffered_receives(), 1);
        assert_eq!(channel.borrow().num_duplicate_packets(), 1);
    }

    /// An out-of-range sequence number is corrupt (the `seqMask` gate,
    /// udp_channel.cpp:1212).
    #[test]
    fn an_out_of_range_sequence_number_is_corrupt() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f4),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        assert!(matches!(
            receive(&channel, SEQ_SIZE),
            AddToReceiveWindowResult::Corrupt
        ));
    }

    /// A packet from a source the channel is not bound to is corrupt,
    /// unless the channel is an auto-switching indexed one.
    #[test]
    fn a_packet_from_the_wrong_source_address_is_corrupt() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f5),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        let stranger = addr(0x0100_00f6);
        let mut packet = Packet::new();
        packet.set_seq(0);
        assert!(matches!(
            UdpChannel::add_to_receive_window(&channel, Rc::new(packet), &stranger),
            AddToReceiveWindowResult::Corrupt
        ));
        assert!(!channel.borrow().has_acks(), "nothing was accepted");
    }

    /// `shouldAutoSwitchToSrcAddr`: the indexed channel follows the
    /// source address on an acceptable (not older) channel version -- on
    /// equal versions too, since the first cell-entity packet is v0.
    #[test]
    fn an_auto_switching_indexed_channel_follows_the_source() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        interface.set_verbose(false);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f7),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            7,
        );
        UdpChannel::set_should_auto_switch_to_src_addr(&channel, true);
        assert!(channel.borrow().should_auto_switch_to_src_addr());
        channel.borrow_mut().force_version(10);

        // An older version does not move the channel.
        let mut older = Packet::new();
        older.set_seq(0);
        older.set_channel_version(9);
        let first_src = addr(0x0100_00f8);
        assert!(matches!(
            UdpChannel::add_to_receive_window(&channel, Rc::new(older), &first_src),
            AddToReceiveWindowResult::NextInWindow(_)
        ));
        assert_eq!(channel.borrow().addr(), addr(0x0100_00f7));
        assert_eq!(channel.borrow().version(), 10);

        // An equal-or-newer version from a new source re-addresses it.
        let mut newer = Packet::new();
        newer.set_seq(1);
        newer.set_channel_version(10);
        let second_src = addr(0x0100_00f9);
        assert!(matches!(
            UdpChannel::add_to_receive_window(&channel, Rc::new(newer), &second_src),
            AddToReceiveWindowResult::NextInWindow(_)
        ));
        assert_eq!(channel.borrow().addr(), second_src);
        assert_eq!(channel.borrow().version(), 10);
    }

    /// Enabling the auto-switch on a non-indexed channel trips the
    /// reference's assert.
    #[test]
    #[should_panic(expected = "MF_ASSERT( !shouldAutoSwitchToSrcAddr_ || this->isIndexed() )")]
    fn enabling_the_auto_switch_requires_an_indexed_channel() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fa),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_should_auto_switch_to_src_addr(&channel, true);
    }

    /// `PACKET_IS_OUT_OF_WINDOW`, both arms: past twice the buffered
    /// array, and past the overflow-padded window ceiling.
    #[test]
    fn a_far_future_sequence_is_out_of_window() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fb),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        // Twice the 256-slot array, but inside the 256+8192 ceiling.
        assert!(matches!(
            receive(&channel, 600),
            AddToReceiveWindowResult::OutOfWindow
        ));
        assert_eq!(channel.borrow().num_buffered_receives(), 0);

        // Past the ceiling as well.
        assert!(matches!(
            receive(&channel, 10_000),
            AddToReceiveWindowResult::OutOfWindow
        ));
    }

    /// A future packet inside the range but beyond the current array
    /// doubles the receive buffer (NOTICE branch) and then buffers.
    #[test]
    fn a_far_packet_doubles_the_receive_buffer() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fc),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // 400 is beyond the 256 slots but within 2*256: double, buffer.
        assert!(matches!(
            receive(&channel, 400),
            AddToReceiveWindowResult::BufferedInWindow
        ));
        assert_eq!(channel.borrow().buffered_receives.size(), 512);
        assert_eq!(channel.borrow().num_buffered_receives(), 1);

        // The doubling carried nothing and the chain still drains right.
        let mut chain = Vec::new();
        for seq in 0..400 {
            match receive(&channel, seq) {
                AddToReceiveWindowResult::NextInWindow(packets) => {
                    chain.extend(packets.iter().map(|p| p.seq()));
                }
                _ => panic!("the arrivals before 400 are all next-in-window"),
            }
        }
        assert_eq!(chain.last(), Some(&400));
        assert_eq!(channel.borrow().num_buffered_receives(), 0);
        assert_eq!(channel.borrow().in_seq_at(), 401);
    }

    /// Piggybacks are not acknowledged: the containing packet's ACK
    /// covers them (udp_channel.cpp:1253).
    #[test]
    fn piggybacks_do_not_queue_an_ack() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fd),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        let src = channel.borrow().addr();
        let mut packet = Packet::new();
        packet.set_seq(0);
        packet.set_is_piggyback(true);
        assert!(matches!(
            UdpChannel::add_to_receive_window(&channel, Rc::new(packet), &src),
            AddToReceiveWindowResult::NextInWindow(_)
        ));
        assert!(
            !channel.borrow().has_acks(),
            "the containing packet's ack covers the piggyback"
        );
    }

    /// `validateUnreliableSeqNum`: the unreliable arm accepts and moves
    /// on equal-or-newer, rejects old and out-of-range ones without
    /// moving.
    #[test]
    fn the_unreliable_sequence_check_moves_only_forward() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fe),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // The first packet can be anything in range.
        assert!(UdpChannel::validate_unreliable_seq_num(&channel, 5));
        assert_eq!(channel.borrow().unreliable_in_seq_at(), 5);

        // An equal seq is fine (not seqLessThan itself).
        assert!(UdpChannel::validate_unreliable_seq_num(&channel, 5));

        // An older one is refused and does not move the slot.
        assert!(!UdpChannel::validate_unreliable_seq_num(&channel, 4));
        assert_eq!(channel.borrow().unreliable_in_seq_at(), 5);

        // An out-of-range one is refused too.
        assert!(!UdpChannel::validate_unreliable_seq_num(&channel, SEQ_SIZE));
        assert_eq!(channel.borrow().unreliable_in_seq_at(), 5);
    }
}
