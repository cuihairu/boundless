//! `lib/network/udp_channel.cpp` -- the registration-and-lifecycle slice
//! of `Mercury::UDPChannel` (BW-02-03-02) plus the send window and its acks
//! (BW-02-03-03), the timed resend scan (BW-02-03-04) and the receive
//! window (BW-02-03-05): construction registers a non-indexed channel with
//! its interface, `condemn` hands it to the condemned list, `destroy`
//! closes it for good, `addResendTimer`/`handleAck` keep the unacked ring
//! honest across the `SEQ_SIZE` seam, `checkResendTimers` pushes the stale
//! ones back out, and `addToReceiveWindow` filters reordered and duplicate
//! packets into an in-order chain.
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
use crate::fragmented_bundle::{FragmentedBundle, FragmentedBundlePtr};
use crate::misc::{
    CHANNEL_ID_NULL, ChannelID, ChannelVersion, Reason, SEQ_NULL, SeqNum, seq_less_than, seq_mask,
};
use crate::network_interface::{ChannelRef, NetworkInterface};
use crate::packet::{HEADER_SIZE, MAX_ACKS, Packet, flags};
use crate::packet_filter::PacketFilter;
use crate::reply_order::ReplyOrder;
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

/// `AddToReceiveWindowResult` (udp_channel.hpp:161-167): what
/// [`UdpChannel::add_to_receive_window`] did with a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddToReceiveWindowResult {
    /// The packet was the next one the channel expected: it was delivered,
    /// chaining the buffered packets it unblocked.
    NextInWindow,
    /// The packet was stored ahead of the expected sequence.
    BufferedInWindow,
    /// The packet was older than the expected sequence: a duplicate.
    Duplicate,
    /// The packet fell outside the window the channel will accept.
    OutOfWindow,
    /// The packet's sequence or source address did not match the channel.
    Corrupt,
}

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

    /// `unackedPackets_` (udp_channel.hpp:415): the sent-but-unacked ring,
    /// addressed by sequence number.
    unacked_packets: CircularArray<Option<UnackedPacket>>,
    /// `unackedCriticalSeq_` (udp_channel.hpp:448).
    unacked_critical_seq: SeqNum,
    /// `hasSeenOverflowWarning_`.
    has_seen_overflow_warning: bool,
    /// `lastReliableResendTime_`, in micros.
    last_reliable_resend_time: u64,
    /// `isRemoteRegular_` (udp_channel.cpp:115): the peer sends on a steady
    /// rhythm, so an age-based resend would be redundant; only an irregular
    /// channel's stale packets are resent by age.
    is_remote_regular: bool,
    /// `numPacketsResent_`: resends the timer scan has pushed back out.
    num_packets_resent: u32,
    /// `bufferedReceives_` (udp_channel.hpp:444): packets received ahead of
    /// `in_seq_at`, held until the missing ones arrive. Addressed by seq;
    /// the run of consecutive slots above `in_seq_at` chains into the
    /// in-order packet when it finally arrives.
    buffered_receives: CircularArray<Option<Rc<Packet>>>,
    /// `numBufferedReceives_`.
    num_buffered_receives: u32,
    /// `pFragments_` (udp_channel.hpp:449): this channel's own partially
    /// reassembled bundle, while a fragmented one is in flight. The receive
    /// dispatch drives it (BW-02-03-07).
    p_fragments: Option<FragmentedBundlePtr>,
    /// `acksToSend_` (udp_channel.hpp:452): the seqs this channel owes an
    /// ack to, carried on the next outbound bundle's footer.
    /// [`UdpChannel::write_footer`] drains it.
    acks_to_send: BTreeSet<SeqNum>,
    /// `pushUnsentAcksThreshold_`: 0 disables the push; otherwise a
    /// received packet that grows the set to this size forces the outgoing
    /// bundle out now (udp_channel.cpp:1287).
    push_unsent_acks_threshold: usize,
    /// `shouldAutoSwitchToSrcAddr_` (udp_channel.hpp:465): the channel may
    /// adopt the source address of an acceptable incoming packet.
    should_auto_switch_to_src_addr: bool,
    /// `numReliablePacketsSent_` (udp_channel.cpp:789): reliable outbound
    /// packets this channel has pushed through the interface.
    num_reliable_packets_sent: u32,
    /// `pFilter_` (udp_channel.hpp:381): the channel's packet filter, the
    /// encryption filter of BW-02-05. An `Rc` so the reference's smart
    /// pointer shares the filter the same way.
    p_filter: Option<Rc<dyn PacketFilter>>,

    /// The interface this channel registers with. Weak so the map's
    /// strong references do not keep the interface alive.
    network_interface: Weak<NetworkInterface>,
}

impl UdpChannel {
    /// `UDPChannel( networkInterface, addr, traits, minInactivityResendDelay, id )`
    /// (udp_channel.cpp:82): initialise the window fields from the traits
    /// and id, open the bundle, and register non-indexed channels through
    /// [`set_address`](Self::set_address_fn). A filter is fitted later,
    /// through [`set_p_filter`](Self::set_p_filter), which carries the
    /// `pFilter && id_` critical (udp_channel.cpp:129).
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
            buffered_receives: CircularArray::new(size),
            num_buffered_receives: 0,
            p_fragments: None,
            acks_to_send: BTreeSet::new(),
            push_unsent_acks_threshold: 0,
            should_auto_switch_to_src_addr: false,
            num_reliable_packets_sent: 0,
            p_filter: None,
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
            unacked_critical_seq: SEQ_NULL,
            has_seen_overflow_warning: false,
            last_reliable_resend_time: 0,
            is_remote_regular: true,
            num_packets_resent: 0,
            network_interface: Rc::downgrade(interface),
        }));

        // This corresponds to the decRef in Channel::destroy: the port's
        // lifetime bookkeeping is the Rc, nothing to balance here.

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
    /// keepalive and regularity bookkeeping (BW-02-03-04/05): anonymity
    /// means the interface owns the channel's cleanup.
    pub fn set_is_anonymous(channel: &ChannelPtr, anonymous: bool) {
        channel.borrow_mut().is_anonymous = anonymous;
    }

    /// `UDPChannel::isRemoteRegular( bool )` (udp_channel.cpp:115): the
    /// peer sends on a steady rhythm. Irregular channels are the ones the
    /// resend timer resends by age (BW-02-03-04); a regular channel's stale
    /// packets wait on a newer ack instead.
    pub fn set_is_remote_regular(channel: &ChannelPtr, regular: bool) {
        channel.borrow_mut().is_remote_regular = regular;
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

        {
            // Since you aren't going to be actively sending on this channel
            // anymore, it must be marked as irregular (udp_channel.cpp:406):
            // the resend timer resends by age only on irregular channels.
            // The isLocalRegular half of the pair arrives with the local
            // regularity bookkeeping (BW-02-05 keepalive).
            let mut this = channel.borrow_mut();
            this.is_remote_regular = false;
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

    /// `writeFlags( p )` (udp_channel.cpp:1527): write the flags on a
    /// packet fitting for one that will ride on this channel, reserving
    /// the footer room each flag owes. Called before
    /// [`write_footer`](Self::write_footer).
    pub(crate) fn write_flags(this: &mut UdpChannel, p: &mut Packet) {
        p.enable_flags(flags::ON_CHANNEL);

        if this.is_indexed() {
            p.enable_flags(flags::INDEXED_CHANNEL);
            p.set_channel_id(this.id);
            p.set_channel_version(this.version);
            p.reserve_footer(
                std::mem::size_of::<ChannelID>() + std::mem::size_of::<ChannelVersion>(),
            );
        }

        // Add a cumulative ACK. This indicates that all packets BEFORE a
        // given seq have been received (udp_channel.cpp:1550).
        if p.free_space() >= std::mem::size_of::<SeqNum>() {
            p.enable_flags(flags::HAS_CUMULATIVE_ACK);
            p.reserve_footer(std::mem::size_of::<SeqNum>());

            // Need to go through all due to wrap-around case.
            let in_seq_at = this.in_seq_at;
            this.acks_to_send
                .retain(|&ack| !seq_less_than(ack, in_seq_at));
        }

        // Put on as many acks as we can (udp_channel.cpp:1574): the ack
        // block is the one-byte count plus at least one sequence number,
        // so `>= 1 + sizeof(SeqNum)` reads as the same bound clippy
        // prefers written `>`.
        if !this.acks_to_send.is_empty() && p.free_space() > std::mem::size_of::<SeqNum>() {
            p.enable_flags(flags::HAS_ACKS);
            p.reserve_footer(1);

            let min_space = p.free_space() / std::mem::size_of::<SeqNum>();
            let n_acks = min_space
                .min(this.acks_to_send.len())
                .min(usize::from(MAX_ACKS));
            p.set_n_acks(n_acks as u8);
            p.reserve_footer(std::mem::size_of::<SeqNum>() * n_acks);
        }

        // If this is the first reliable outbound packet, flag it
        // (udp_channel.cpp:1592).
        if p.has_flags(flags::IS_RELIABLE)
            && this.is_internal()
            && this.large_out_seq_at == 0
            && this.num_reliable_packets_sent == 0
        {
            p.enable_flags(flags::CREATE_CHANNEL);
        }
    }

    /// `writeFooter( p )` (udp_channel.cpp:1596): pack the footers the
    /// flags reserved, draining the acks this channel owes. Called after
    /// [`write_flags`](Self::write_flags) while the footers are written
    /// backwards towards the message data.
    pub(crate) fn write_footer(this: &mut UdpChannel, p: &mut Packet) {
        if p.has_flags(flags::INDEXED_CHANNEL) {
            p.pack_footer_i32(p.channel_id());
            p.pack_footer_u32(p.channel_version());
        }

        if p.has_flags(flags::HAS_CUMULATIVE_ACK) {
            p.pack_footer_u32(this.in_seq_at);
        }

        if p.has_flags(flags::HAS_ACKS) {
            // Note: Technically we should start at inSeqAt_ since sequence
            // numbers wrap around but this is rare enough not to worry
            // about (since it still works but is less efficient).
            p.pack_footer_u8(p.n_acks());
            for _ in 0..p.n_acks() {
                let ack = *this
                    .acks_to_send
                    .iter()
                    .next()
                    .expect("writeFooter: the acks count was reserved");
                p.pack_footer_u32(ack);
                this.acks_to_send.remove(&ack);
            }
        }
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

    /// `getMaxOverflowPackets()` (udp_channel.hpp:423): external 1024,
    /// internal 8192, indexed 4096. Index 1 (internal) is not reachable
    /// through a `* 2` of the indexed flag, so the non-external branch
    /// advances one from the external entry.
    fn get_max_overflow_packets(&self) -> usize {
        if self.is_external() {
            MAX_OVERFLOW_PACKETS[0]
        } else {
            MAX_OVERFLOW_PACKETS[usize::from(self.is_indexed()) + 1]
        }
    }

    /// `maxWindowSize()` (udp_channel.hpp:433): how far ahead of `inSeqAt_`
    /// the receive window may accept a packet.
    pub fn max_window_size(&self) -> usize {
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

    /// `checkResendTimers( bundle )` (udp_channel.cpp:1080): resend the
    /// unacked packets the timing says are stale. A packet goes back out
    /// when a newer ack has passed its send slot, or when it has aged past
    /// twice the round trip on a channel whose peer sends irregularly --
    /// "anything that has taken more than twice the RTT on the channel to
    /// come back is considered to be too old". Throttled to `windowSize/8`
    /// resends per scan. The bundle parameter only feeds the piggyback
    /// attempt inside `resend`, which the send path owns (BW-02-03-07), so
    /// the port takes no bundle here.
    pub fn check_resend_timers(channel: &ChannelPtr) {
        // There are no unacked packets, or the remote process has failed:
        // nothing to time and nowhere to send it.
        let this = channel.borrow();
        if this.oldest_unacked_seq == SEQ_NULL || this.has_remote_failed {
            return;
        }

        // The scan runs from the oldest unacked packet up to the newest
        // send on an irregular channel, or the newest ack on a regular one.
        let is_irregular = !this.is_remote_regular;
        let end_seq = if is_irregular {
            this.small_out_seq_at
        } else {
            this.highest_ack
        };
        let oldest = this.oldest_unacked_seq;
        let highest_ack = this.highest_ack;
        let resend_period = this
            .round_trip_time
            .checked_mul(2)
            .unwrap_or(this.round_trip_time)
            .max(this.min_inactivity_resend_delay);
        // TODO(udp_channel.cpp:1113): 8 is a magic number; the idea is to
        // throttle resends in extreme situations.
        let max_resends = this.window_size / 8;
        drop(this);

        let now = UdpChannel::now(channel);
        let mut num_resends = 0;
        let mut seq = oldest;
        while seq_less_than(seq, end_seq) && num_resends < max_resends {
            let stale = match channel.borrow().unacked_packets.get(seq as usize) {
                Some(unacked) => {
                    let has_newer_ack = seq_less_than(unacked.last_sent_at_out_seq(), highest_ack);
                    has_newer_ack
                        || (is_irregular
                            && now.saturating_sub(unacked.last_sent_time()) > resend_period)
                }
                None => false,
            };

            if stale {
                UdpChannel::resend(channel, seq);
                num_resends += 1;
            }
            seq = seq_mask(seq.wrapping_add(1));
        }
    }

    /// `resend( seq, bundle )` (udp_channel.cpp:1181): push a packet in
    /// the window back out and count the resend; returns whether the packet
    /// left the window (the reference returns true only when a piggyback
    /// ride acked it straight away). The piggyback half -- an external
    /// channel, an unfragmented packet and a free head slot -- arrives with
    /// the send path (BW-02-03-07); until then a resend goes back out on
    /// its own, which is `sendUnacked`'s own path.
    fn resend(channel: &ChannelPtr, seq: SeqNum) -> bool {
        channel.borrow_mut().num_packets_resent += 1;
        UdpChannel::send_unacked(channel, seq);
        false
    }

    /// `sendUnacked( unacked )` (udp_channel.cpp:1187): push a packet in
    /// the window out again, stamping the bookkeeping the ack and resend
    /// paths read. The channel-version refresh
    /// (`pPacket_->updateChannelVersion`) and the hand to the interface
    /// (`pNetworkInterface_->sendPacket`) arrive with the send path
    /// (BW-02-03-06); an absent slot is a no-op where the reference would
    /// dereference null.
    fn send_unacked(channel: &ChannelPtr, seq: SeqNum) {
        let now = UdpChannel::now(channel);

        // `unacked.pPacket_->updateChannelVersion( version_, id_ )` then
        // `pNetworkInterface_->sendPacket( addr_, pPacket_, this, true )`
        // (udp_channel.cpp:1214): the packet snapshot rides back out on
        // the interface's socket.
        let (packet, addr, interface) = {
            let mut this = channel.borrow_mut();
            // The channel version and id ride into the packet snapshot;
            // reading them first keeps the slot's mutable borrow local.
            let (version, id, addr) = (this.version, this.id, this.addr);
            let interface = this.network_interface.upgrade();
            let Some(unacked) = this.unacked_packets.get_mut(seq as usize).as_mut() else {
                // An absent slot is a no-op where the reference would
                // dereference null.
                return;
            };
            if let Some(packet_mut) = Rc::get_mut(unacked.packet_mut()) {
                packet_mut.update_channel_version(version, id);
            }
            let packet = unacked.packet().as_ref().clone();
            (packet, addr, interface)
        };
        if let Some(interface) = interface.as_ref() {
            // `sendRescheduledPacket` (packet_sender.cpp:241-258): a
            // resend rides the channel's filter before the socket, same as
            // a fresh send.
            match channel.borrow().p_filter() {
                Some(filter) => {
                    let mut packet = packet;
                    filter.send(&addr, &mut packet, &|addr, packet| {
                        interface.send_packet(addr, packet)
                    });
                }
                None => interface.send_packet(&addr, &packet),
            }
        }

        let mut this = channel.borrow_mut();
        let small = this.small_out_seq_at;
        if let Some(unacked) = this.unacked_packets.get_mut(seq as usize).as_mut() {
            unacked.set_last_sent_at_out_seq(small);
            unacked.set_was_resent(true);
            unacked.set_last_sent_time(now);
        }
        this.last_reliable_resend_time = now;
    }

    /// `Channel::send( pBundle )` + `UDPChannel::doSend( bundle )`
    /// (channel.cpp:149, udp_channel.cpp:705), external-bundle form:
    /// finalise the bundle, hand it to the interface as UDP packets, and
    /// keep the channel's send stats. The channel's own-bundle handling
    /// (`clearBundle`, the empty own-bundle bail) and the irregular-channel
    /// registration ride with the channel's owning code; the `hasUnsentData`
    /// case is the caller's. The `cancelRequestsFor` arms report through the
    /// interface's request manager.
    pub fn send_bundle(channel: &ChannelPtr, bundle: &mut UdpBundle) {
        {
            let this = channel.borrow();
            if !this.is_connected() {
                eprintln!(
                    "Channel::send( {} ): Channel is not connected",
                    this.c_str()
                );
                return;
            }
        }

        // Don't do anything if the remote process has failed.
        if channel.borrow().has_remote_failed {
            if let Some(interface) = channel.borrow().network_interface.upgrade() {
                interface.cancel_requests_for(&channel.borrow().addr);
            }
            return;
        }

        bundle.finalise();

        // `doPreFinaliseBundle`/`addReplyOrdersTo` (channel.cpp:157): the
        // reply id bookkeeping happens here, after finalise.
        if let Some(interface) = channel.borrow().network_interface.upgrade() {
            let addr = channel.borrow().addr;
            if let Some(manager) = interface.request_manager() {
                bundle.add_reply_orders_to(&manager, Some(addr));
            }
        }

        let addr = channel.borrow().addr;
        let size = bundle.size();
        let num_units = bundle.num_data_units();
        let reliable = bundle.is_reliable();
        let critical = bundle.is_critical();

        // The upgrade is scoped out before `send`: the send path borrows
        // the channel again (`prepare_packets`'s `write_flags`), so the
        // `if let` scrutinee's temporary `Ref` must be gone by then.
        let interface = channel.borrow().network_interface.upgrade();
        if let Some(interface) = interface {
            interface.send(&addr, bundle, Some(channel));
        }

        // Update our stats (udp_channel.cpp:787-801).
        let mut this = channel.borrow_mut();
        this.num_data_units_sent += 1;
        this.num_bytes_sent += size as u32;
        if reliable {
            this.num_reliable_packets_sent += 1;
        }

        // If the bundle that was just sent was critical, the sequence
        // number of its last packet is the new unackedCriticalSeq_.
        if critical && num_units > 0 {
            let first_seq = bundle.packet(0).seq();
            this.unacked_critical_seq =
                seq_mask(first_seq.wrapping_add(num_units as u32).wrapping_sub(1));
        }
    }

    /// The `UDPBundle emptyBundle; pChannel->send( &emptyBundle )` of the
    /// receive paths (udp_channel.cpp:1290, packet_receiver.cpp:927): push
    /// the pending acks out on an empty bundle.
    pub fn push_unsent_acks(channel: &ChannelPtr) {
        let mut empty = UdpBundle::new(0);
        UdpChannel::send_bundle(channel, &mut empty);
    }

    /// `processPacket`'s on-channel dispatch (packet_receiver.cpp:518+
    /// `processFilteredPacket`): strip the footers the channel owns -- the
    /// cumulative ack, the ack list, the sequence number -- feed the
    /// packet through [`add_to_receive_window`](Self::add_to_receive_window),
    /// and walk the resulting in-order chain into
    /// [`process_ordered_packet`](Self::process_ordered_packet), which
    /// peels the request and fragment footers and hands the messages to
    /// the sink.
    ///
    /// Scope of BW-02-03-07: the reliable and unreliable arms, the ack
    /// bookkeeping, and the on-channel fragment reassembly. Deferred, as
    /// the reference has them: the piggyback pass, the indexed-channel
    /// arms, the once-off-reliable arm and the `onceOffReceiver_` map,
    /// the `isLocalRegular_`/`isEstablished_` tail-ack push (the
    /// threshold-driven push inside `add_to_receive_window` carries the
    /// flow instead), and the `wantsFirstPacket_` arm (the field is not
    /// modelled yet). Every early-out matches the reference's
    /// `RETURN_FOR_CORRUPTED_PACKET`.
    pub fn process_packet(channel: &ChannelPtr, addr: &Address, p: &mut Packet) -> Reason {
        // The undersized / bad-flags / empty-body checks (packet_receiver.cpp:519-575).
        if p.total_size() < 2 || p.total_size() <= HEADER_SIZE {
            return Reason::CorruptedPacket;
        }
        if p.flags() & !flags::KNOWN_FLAGS != 0 {
            return Reason::CorruptedPacket;
        }

        // `validateChecksum` (packet_receiver.cpp:585-599): a packet that
        // fails its footer is "Invalid checksum" and corrupted. A packet
        // with no checksum flag validates trivially.
        if !p.validate_checksum() {
            return Reason::CorruptedPacket;
        }

        let interface = channel.borrow().network_interface.upgrade();

        // A failed remote, or a packet older than the channel's creation
        // version, is dropped (packet_receiver.cpp:686-706).
        {
            let this = channel.borrow();
            if this.has_remote_failed {
                return Reason::GeneralNetwork;
            }
            if seq_less_than(p.channel_version(), this.creation_version) {
                return Reason::Success;
            }
        }

        // Strip the cumulative ack footer and feed it to the window
        // (packet_receiver.cpp:760-780).
        if p.has_flags(flags::HAS_CUMULATIVE_ACK) {
            let Some(end_seq) = p.strip_footer_u32() else {
                return Reason::CorruptedPacket;
            };
            if !UdpChannel::handle_cumulative_ack(channel, end_seq) {
                return Reason::CorruptedPacket;
            }
        }

        // Strip and handle the ack list (packet_receiver.cpp:782-855).
        if p.has_flags(flags::HAS_ACKS) {
            let Some(n_acks) = p.strip_footer_u8() else {
                return Reason::CorruptedPacket;
            };
            if n_acks == 0 {
                return Reason::CorruptedPacket;
            }
            let ack_size = n_acks as usize * 4;
            if p.body_size() < ack_size {
                return Reason::CorruptedPacket;
            }
            for _ in 0..n_acks {
                let Some(seq) = p.strip_footer_u32() else {
                    return Reason::CorruptedPacket;
                };
                if !UdpChannel::handle_ack(channel, seq) {
                    return Reason::CorruptedPacket;
                }
            }
        }

        // Strip the sequence number footer (packet_receiver.cpp:857-870).
        if p.has_flags(flags::HAS_SEQUENCE_NUMBER) {
            let Some(seq) = p.strip_footer_u32() else {
                return Reason::CorruptedPacket;
            };
            p.set_seq(seq);
        }

        let Some(interface) = interface else {
            return Reason::GeneralNetwork;
        };

        // now do something if it's reliable (packet_receiver.cpp:872-950).
        if p.has_flags(flags::IS_RELIABLE) {
            // first make sure it has a sequence number, so we can address it
            if p.seq() == SEQ_NULL {
                return Reason::CorruptedPacket;
            }

            let (result, chain) =
                UdpChannel::add_to_receive_window(channel, Rc::new(p.clone()), *addr);
            match result {
                AddToReceiveWindowResult::NextInWindow => {
                    // The head is the received packet chained through every
                    // buffered packet it unblocked, in sequence order; walk
                    // it and break the chain link by link.
                    let mut cursor = chain;
                    while let Some(rc) = cursor {
                        cursor = rc.chain_next();
                        let Ok(mut owned) = Rc::try_unwrap(rc) else {
                            continue;
                        };
                        let reason = UdpChannel::process_ordered_packet(
                            channel, addr, &interface, true, &mut owned,
                        );
                        if reason == Reason::CorruptedPacket {
                            return reason;
                        }
                    }
                    Reason::Success
                }
                AddToReceiveWindowResult::Corrupt => Reason::CorruptedPacket,
                // Duplicate or buffered: the reference returns success,
                // having already queued the ack inside `add_to_receive_window`.
                _ => Reason::Success,
            }
        } else {
            // If the packet is unreliable, confirm the sequence number is
            // incrementing as a sanity check (packet_receiver.cpp:958-968).
            let mut this = channel.borrow_mut();
            if this.is_external() && !this.validate_unreliable_seq_num(p.seq()) {
                return Reason::CorruptedPacket;
            }

            UdpChannel::process_ordered_packet(channel, addr, &interface, false, p)
        }
    }

    /// `processOrderedPacket` (packet_receiver.cpp:1170): strip the
    /// first-request footer, then the fragment footer, and hand the
    /// packet's messages to the sink. A complete fragment chain is
    /// walked the same way, one packet at a time. `is_on_channel` is the
    /// reference's `pChannel && p->hasFlags( FLAG_IS_RELIABLE )`.
    fn process_ordered_packet(
        channel: &ChannelPtr,
        addr: &Address,
        interface: &NetworkInterface,
        is_on_channel: bool,
        p: &mut Packet,
    ) -> Reason {
        // Strip first request offset (packet_receiver.cpp:1176-1194).
        if p.has_flags(flags::HAS_REQUESTS) {
            match p.strip_footer_u16() {
                Some(offset) if (offset as usize) <= p.msg_end_offset() => {
                    p.set_first_request_offset(offset as usize);
                }
                _ => return Reason::CorruptedPacket,
            }
        }

        // Strip fragment footers (packet_receiver.cpp:1204-1310). Only the
        // on-channel arm is modelled: the channel keeps its own partially
        // reassembled bundle in [`p_fragments`](Self::p_fragments). The
        // off-channel map (`onceOffReceiver_`) is deferred, so an off-channel
        // fragment is dropped here.
        if p.has_flags(flags::IS_FRAGMENT) {
            if !p.strip_frag_info() {
                return Reason::CorruptedPacket;
            }

            if !is_on_channel {
                return Reason::Success;
            }

            let now = interface.dispatcher().now();
            let source = channel.borrow().c_str();
            let external = interface.is_external();

            let existing = channel.borrow_mut().p_fragments.take();
            let chain = match existing {
                None => {
                    // The first fragment must be the bundle's first.
                    if p.seq() != p.frag_begin() {
                        return Reason::CorruptedPacket;
                    }
                    let bundle = FragmentedBundle::new(Rc::new(p.clone()), now);
                    channel.borrow_mut().p_fragments = Some(Rc::new(RefCell::new(bundle)));
                    // A first fragment delivers nothing yet.
                    return Reason::Success;
                }
                Some(fb_cell) => {
                    let mut bundle = fb_cell.borrow_mut();
                    if !bundle.add_packet(Rc::new(p.clone()), external, &source, now) {
                        return Reason::CorruptedPacket;
                    }
                    if bundle.is_complete() {
                        channel.borrow_mut().p_fragments = None;
                        bundle.chain()
                    } else {
                        // Still incomplete: put it back and stop.
                        drop(bundle);
                        channel.borrow_mut().p_fragments = Some(fb_cell);
                        return Reason::Success;
                    }
                }
            };

            // The bundle is complete: walk the assembled chain and deliver
            // every packet's messages.
            let mut cursor = chain;
            let mut reason = Reason::Success;
            while let Some(rc) = cursor {
                cursor = rc.chain_next();
                let Ok(mut owned) = Rc::try_unwrap(rc) else {
                    continue;
                };
                reason = interface.deliver_packet(addr, &mut owned);
                if reason == Reason::CorruptedPacket {
                    break;
                }
            }
            return reason;
        }

        // A non-fragment packet with a sequence number while the channel
        // expects fragments is corrupt (packet_receiver.cpp:1196-1202).
        if is_on_channel
            && p.has_flags(flags::HAS_SEQUENCE_NUMBER)
            && channel.borrow().p_fragments().is_some()
        {
            return Reason::CorruptedPacket;
        }

        interface.deliver_packet(addr, p)
    }

    /// `addToReceiveWindow( p, srcAddr, stats )` (udp_channel.cpp:1232):
    /// filter a received packet against the receive window and say what
    /// happened to it. The packet moves in; when it is the next one the
    /// channel expected it comes back out as the head of a chain reaching
    /// through every buffered packet it unblocked, in sequence order -- the
    /// caller dispatches by walking [`Packet::chain_next`]. Everything else
    /// consumes the packet: buffered packets are filed in
    /// `buffered_receives`, duplicates and rejects are dropped.
    ///
    /// The reference's `shouldAutoSwitchToSrcAddr_` arm switches the
    /// channel's address on an acceptable version; without it a packet
    /// from any address but the channel's own is corrupt. The
    /// `acksToSend_.insert` and the unsent-ack push are wired: a received
    /// packet owes an ack, and a set grown past
    /// [`UdpChannel::set_push_unsent_acks_threshold`] pushes the outgoing
    /// bundle out at once. The `stats` counters arrive with the
    /// interface's receive path (BW-02-05-04).
    pub fn add_to_receive_window(
        channel: &ChannelPtr,
        packet: Rc<Packet>,
        src_addr: Address,
    ) -> (AddToReceiveWindowResult, Option<Rc<Packet>>) {
        let seq = packet.seq();

        // Make sure the sequence number is valid (udp_channel.cpp:1239).
        if seq_mask(seq) != seq {
            return (AddToReceiveWindowResult::Corrupt, None);
        }

        // Address validation. The auto-switch arm (udp_channel.cpp:1251-1267)
        // adopts the source when the channel version is acceptable -- the
        // first packet from a cell entity sets the address and is version 0.
        // Otherwise a packet from any address but the channel's own is
        // corrupt.
        let adopted = {
            let mut this = channel.borrow_mut();
            if this.should_auto_switch_to_src_addr {
                if seq_less_than(packet.channel_version(), this.version) {
                    None
                } else {
                    this.version = packet.channel_version();
                    Some(src_addr)
                }
            } else {
                if this.addr != src_addr {
                    // Got packet from wrong source address (udp_channel.cpp:1260).
                    return (AddToReceiveWindowResult::Corrupt, None);
                }
                None
            }
        };
        if let Some(new_addr) = adopted {
            UdpChannel::set_address_fn(channel, new_addr);
        }

        // Always add the ACKs for others even though a cumulative ack may
        // mean it is not sent. This still shows that the channel is not
        // empty (udp_channel.cpp:1275). No need to ack piggybacks as they
        // are implicitly acked by the containing packet.
        let should_push = {
            let mut this = channel.borrow_mut();
            if !packet.is_piggyback() {
                this.acks_to_send.insert(seq);
            }
            let threshold = this.push_unsent_acks_threshold;
            threshold != 0 && this.acks_to_send.len() >= threshold
        };

        // Push the outgoing bundle immediately if required (udp_channel.cpp:1287).
        if should_push {
            UdpChannel::push_unsent_acks(channel);
        }

        let mut this = channel.borrow_mut();

        // check the good case first (udp_channel.cpp:1302)
        if seq == this.in_seq_at {
            this.in_seq_at = seq_mask(this.in_seq_at.wrapping_add(1));

            // Attach as many buffered packets as possible to this one. The
            // reference keeps `pPrev` on the just-chained packet and links
            // the next buffered packet behind it; the port's cursor is the
            // same pointer, threaded through each link's `chain_next` slot.
            let mut head = packet;
            let mut tail: &mut Rc<Packet> = &mut head;
            loop {
                let idx = this.in_seq_at as usize;
                let next = this.buffered_receives.get_mut(idx).take();
                let Some(next) = next else {
                    break;
                };
                this.num_buffered_receives -= 1;
                this.in_seq_at = seq_mask(this.in_seq_at.wrapping_add(1));
                let last = Rc::get_mut(tail).expect("chain: sole owner");
                last.chain(next);
                tail = last.chain_next_mut().unwrap();
            }

            return (AddToReceiveWindowResult::NextInWindow, Some(head));
        }

        // see if we've got this one before. We have if seq < inSeqAt_
        // (udp_channel.cpp:1325).
        if seq_less_than(seq, this.in_seq_at) {
            return (AddToReceiveWindowResult::Duplicate, None);
        }

        // make sure it's in range (udp_channel.cpp:1341). Note that we
        // store packets between [inSeqAt_ + 1, inSeqAt_ + size) -- this is
        // why the bounds use > rather than >=.
        let required_window_size = seq_mask(seq.wrapping_sub(this.in_seq_at));
        if required_window_size as usize > 2 * this.buffered_receives.size()
            || required_window_size as usize > this.max_window_size()
        {
            return (AddToReceiveWindowResult::OutOfWindow, None);
        } else if required_window_size as usize > this.buffered_receives.size() {
            // Copy buffered receives from the start of our buffered
            // receives, the first starting at inSeqAt_ + 1.
            let start = this.in_seq_at.wrapping_add(1) as usize;
            this.buffered_receives.double_size(start);
        }

        // ok - we'll buffer this packet then.... (udp_channel.cpp:1381)
        let slot = this.buffered_receives.get_mut(seq as usize);
        if slot.is_some() {
            // ... but only if we don't already have it. A different
            // sequence in the slot is the reference's CRITICAL_MSG: logged,
            // and the resident packet keeps the slot.
            if slot.as_ref().unwrap().seq() == seq {
                return (AddToReceiveWindowResult::Duplicate, None);
            }
            eprintln!(
                "UDPChannel::addToReceiveWindow: Packet {} in {}'s buffered receive slot",
                slot.as_ref().unwrap().seq(),
                seq
            );
            return (AddToReceiveWindowResult::BufferedInWindow, None);
        }

        *slot = Some(packet);
        this.num_buffered_receives += 1;

        (AddToReceiveWindowResult::BufferedInWindow, None)
    }

    /// `numBufferedReceives_`.
    pub fn num_buffered_receives(&self) -> u32 {
        self.num_buffered_receives
    }

    /// `pFragments()` (udp_channel.hpp:225): this channel's in-flight
    /// fragment bundle, if one is being reassembled.
    pub fn p_fragments(&self) -> Option<FragmentedBundlePtr> {
        self.p_fragments.clone()
    }

    /// `pFragments( pFragments )` (udp_channel.hpp:226): file or clear the
    /// channel's fragment bundle.
    pub fn set_p_fragments(channel: &ChannelPtr, p_fragments: Option<FragmentedBundlePtr>) {
        channel.borrow_mut().p_fragments = p_fragments;
    }

    /// `pushUnsentAcksThreshold()`: the size that makes a received packet
    /// push the pending acks out immediately; 0 disables the push.
    pub fn push_unsent_acks_threshold(&self) -> usize {
        self.push_unsent_acks_threshold
    }

    /// `pushUnsentAcksThreshold( n )` (udp_channel.cpp:1287): set the
    /// threshold. The reference asserts it is not set on an indexed
    /// channel; that is unrepresentable until filters land (BW-02-05).
    pub fn set_push_unsent_acks_threshold(&mut self, threshold: usize) {
        self.push_unsent_acks_threshold = threshold;
    }

    /// `shouldAutoSwitchToSrcAddr()`.
    pub fn should_auto_switch_to_src_addr(&self) -> bool {
        self.should_auto_switch_to_src_addr
    }

    /// `shouldAutoSwitchToSrcAddr( b )` (udp_channel.cpp:1476): allow the
    /// channel to adopt the source address of an acceptable incoming
    /// packet. Indexed-only, as the reference asserts.
    pub fn set_should_auto_switch_to_src_addr(&mut self, b: bool) {
        self.should_auto_switch_to_src_addr = b;
    }

    /// `numReliablePacketsSent_` (udp_channel.cpp:789).
    pub fn num_reliable_packets_sent(&self) -> u32 {
        self.num_reliable_packets_sent
    }

    /// `acksToSend_.size()`: how many pending acks this channel owes.
    pub fn num_acks_to_send(&self) -> usize {
        self.acks_to_send.len()
    }

    /// `addResendTimer( seq, p, roBeg, roEnd )` (udp_channel.cpp:817):
    /// file a sent packet in the window and say whether the caller should
    /// send now -- false means the window is full and the packet is queued
    /// as overflow (`REASON_WINDOW_OVERFLOW`).
    pub fn add_resend_timer(
        channel: &ChannelPtr,
        seq: SeqNum,
        packet: Rc<Packet>,
        reply_orders: &[Rc<ReplyOrder>],
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
        if !reply_orders.is_empty() {
            unacked.set_reliable_orders(reply_orders.to_vec());
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

    // -- Channel base accessors (channel.hpp) ----------------------------

    /// `addr()`.
    pub fn addr(&self) -> Address {
        self.addr
    }

    /// `pFilter()` (udp_channel.hpp:139): the channel's packet filter, if
    /// one is fitted.
    pub fn p_filter(&self) -> Option<Rc<dyn PacketFilter>> {
        self.p_filter.clone()
    }

    /// `pFilter( PacketFilterPtr )` (udp_channel.hpp:140). The constructor
    /// critical (udp_channel.cpp:129) -- filters are not supported on
    /// indexed channels -- lands here, where the port fits a filter.
    pub fn set_p_filter(&mut self, filter: Option<Rc<dyn PacketFilter>>) {
        if filter.is_some() && self.id != CHANNEL_ID_NULL {
            panic!(
                "UDPChannel::pFilter: PacketFilters are not supported on indexed channels (id:{})",
                self.id
            );
        }
        self.p_filter = filter;
    }

    /// `bundle()`: the channel's own bundle.
    pub fn bundle(&self) -> &UdpBundle {
        &self.bundle
    }

    /// `bundle()` mutable: the sender writes into it.
    pub fn bundle_mut(&mut self) -> &mut UdpBundle {
        &mut self.bundle
    }

    /// `clearBundle()` (channel.cpp:299): the reference rebuilds the
    /// bundle through `newBundle`, which sizes the spare room off the
    /// current filter (udp_channel.cpp:1523-1529). A filter that claims
    /// padding gets a fresh bundle; one that claims nothing keeps the
    /// cheap reuse.
    pub fn clear_bundle(&mut self) {
        let spare = self
            .p_filter
            .as_deref()
            .map_or(0, |f| f.max_spare_size().min(u8::MAX as usize));
        if spare != 0 {
            self.bundle = UdpBundle::new(spare as u8);
        } else {
            self.bundle.clear();
        }
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

    /// `unreliableInSeqAt()`: the unreliable slot's ring position, still
    /// unset until BW-02-03-05 moves it.
    pub fn unreliable_in_seq_at(&self) -> SeqNum {
        self.unreliable_in_seq_at
    }

    /// `validateUnreliableSeqNum` (udp_channel.cpp:522): the sanity check
    /// on unreliable external traffic -- the sequence must be a bare ring
    /// position and not one the channel has already moved past. Only a
    /// completely validated number is stored.
    pub fn validate_unreliable_seq_num(&mut self, seq: SeqNum) -> bool {
        if seq_mask(seq) != seq {
            return false;
        }

        if seq_less_than(seq, self.unreliable_in_seq_at) && self.unreliable_in_seq_at != SEQ_NULL {
            return false;
        }

        self.unreliable_in_seq_at = seq;
        true
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

    /// `lastReliableSendOrResendTime()` (udp_channel.ipp): the later of the
    /// last reliable send and the last resend, in micros.
    pub fn last_reliable_send_or_resend_time(&self) -> u64 {
        self.last_reliable_send_time
            .max(self.last_reliable_resend_time)
    }

    /// `isRemoteRegular()`.
    pub fn is_remote_regular(&self) -> bool {
        self.is_remote_regular
    }

    /// `numPacketsResent()`: resends the timer scan has pushed back out.
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
    use crate::misc::SEQ_MASK;
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
        assert_eq!(
            internal.borrow().max_window_size(),
            INTERNAL_CHANNEL_SIZE + 8192,
            "internal channels overflow against 8192 (getMaxOverflowPackets)"
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
        assert_eq!(
            external.borrow().max_window_size(),
            EXTERNAL_CHANNEL_SIZE + 1024,
            "external channels overflow against 1024"
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
        assert_eq!(
            indexed.borrow().max_window_size(),
            INDEXED_CHANNEL_SIZE + 4096,
            "indexed channels overflow against 4096"
        );
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
        let order = Rc::new(ReplyOrder::new(Rc::clone(&handler), 1, 500_000, 0));
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
        assert_eq!(orders.len(), 1);
        assert!(Rc::ptr_eq(&orders[0], &order));

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

    /// The BW-02-03-04 acceptance: one controlled loss lands and the resend
    /// timer pushes the packet back out exactly once. The packet's ack was
    /// lost, no newer ack has passed it, and the channel is irregular --
    /// the only kind whose stale packets are resent by age -- so past
    /// `max(2 * roundTripTime, minInactivityResendDelay)` it goes back out.
    #[test]
    fn one_controlled_loss_is_resent_exactly_once() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00ce),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        channel.borrow_mut().force_out_seq_at(0);
        assert!(send(&channel)); // seq 0, whose ack never comes back

        // The peer sends irregularly: only such a channel resends by age.
        UdpChannel::set_is_remote_regular(&channel, false);

        // resendPeriod = max(roundTripTime * 2, minInactivityResendDelay)
        // = max(0.2s, 1.0s) = 1.0s. At 0.5s the packet is still young.
        clock.set(1_500_000);
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 0, "under the period");

        // At 1.5s it is past the period: it goes back out once.
        clock.set(2_500_000);
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 1);
        assert_eq!(channel.borrow().last_reliable_resend_time(), 2_500_000);
        assert_eq!(
            channel.borrow().last_reliable_send_or_resend_time(),
            2_500_000
        );

        // The resend stamped itself as the newest send, so a second scan at
        // the same instant finds nothing stale: one loss, one delivery.
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 1);

        // The lost ack lands late, drains the window, and nothing more is
        // left to time.
        assert!(UdpChannel::handle_ack(&channel, 0));
        assert!(!channel.borrow().has_unacked_packets());
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 1);
    }

    /// The newer-ack arm of the same scan: a packet whose send slot a later
    /// ack has passed is resent even on a regular channel, because the ack
    /// that would have carried it went missing and a newer one superseded
    /// it. No age has to accumulate at all.
    #[test]
    fn a_later_ack_resends_the_packet_it_passed() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00cf),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        channel.borrow_mut().force_out_seq_at(0);
        assert!(send(&channel)); // seq 0
        assert!(send(&channel)); // seq 1

        assert!(channel.borrow().is_remote_regular());
        clock.set(1_200_000);
        assert!(UdpChannel::handle_ack(&channel, 1)); // seq 0's ack was lost

        UdpChannel::check_resend_timers(&channel);
        assert_eq!(
            channel.borrow().num_packets_resent(),
            1,
            "seq 0 sits behind a newer ack"
        );
        assert_eq!(channel.borrow().last_reliable_resend_time(), 1_200_000);
    }

    /// A fresh packet carrying `seq`, owned by a single `Rc` -- the way the
    /// receive dispatch hands one to the filter.
    fn packet(seq: SeqNum) -> Rc<Packet> {
        let mut packet = Packet::new();
        packet.set_seq(seq);
        Rc::new(packet)
    }

    /// Filter `seq` onto the channel the way the reference's receive dispatch
    /// does: a fresh packet with that seq, from the channel's own address.
    fn receive(
        channel: &ChannelPtr,
        seq: SeqNum,
    ) -> (AddToReceiveWindowResult, Option<Rc<Packet>>) {
        let own = channel.borrow().addr();
        UdpChannel::add_to_receive_window(channel, packet(seq), own)
    }

    /// Walk a receive chain and collect the sequence numbers in dispatch
    /// order. This is the acceptance check for "按原版顺序派发": whatever the
    /// caller dispatches is the head, then each link in sequence order.
    fn chain_seq(head: &Rc<Packet>) -> Vec<u32> {
        let mut seen = vec![head.seq()];
        let mut cursor = Rc::clone(head);
        while let Some(next) = cursor.chain_next() {
            seen.push(next.seq());
            cursor = next;
        }
        seen
    }

    /// BW-02-03-05 acceptance: three packets arrive out of order ahead of
    /// the one the channel expects, and the late arrival unblocks all three.
    /// The caller gets back the missing packet as the head of a chain
    /// reaching through every buffered successor, in sequence order.
    #[test]
    fn a_late_packet_unblocks_the_buffered_run_in_sequence_order() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00d0),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        // Two packets land ahead of the expected seq 0: both are buffered,
        // nothing is dispatched, and the window remembers them.
        assert_eq!(
            receive(&channel, 2).0,
            AddToReceiveWindowResult::BufferedInWindow,
            "seq 2 arrives ahead of inSeqAt 0"
        );
        assert_eq!(
            receive(&channel, 3).0,
            AddToReceiveWindowResult::BufferedInWindow
        );
        assert_eq!(channel.borrow().num_buffered_receives(), 2);
        assert_eq!(
            channel.borrow().in_seq_at,
            0,
            "a gap is not closed by a successor"
        );

        // Seq 0 is the packet the channel expects: it is dispatched, but the
        // run stops at the still-missing seq 1.
        let (result, head) = receive(&channel, 0);
        assert_eq!(result, AddToReceiveWindowResult::NextInWindow);
        let Some(head) = head else {
            panic!("an in-order packet is the dispatch head");
        };
        assert_eq!(chain_seq(&head), vec![0], "seq 1 is still missing");
        assert_eq!(channel.borrow().in_seq_at, 1);
        assert_eq!(
            channel.borrow().num_buffered_receives(),
            2,
            "buffering survives"
        );

        // Seq 1 closes the gap: it dispatches and the whole buffered run
        // follows it in sequence order.
        let (result, head) = receive(&channel, 1);
        assert_eq!(result, AddToReceiveWindowResult::NextInWindow);
        let Some(head) = head else {
            panic!("an in-order packet is the dispatch head");
        };
        assert_eq!(
            chain_seq(&head),
            vec![1, 2, 3],
            "the run dispatches in order"
        );
        assert_eq!(channel.borrow().in_seq_at, 4, "the run advanced the window");
        assert_eq!(
            channel.borrow().num_buffered_receives(),
            0,
            "the run drained the buffer"
        );
    }

    /// A retransmit older than the window has moved past is a duplicate,
    /// whether it has already been dispatched or is still buffered.
    #[test]
    fn a_retransmit_is_reported_as_a_duplicate() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00d1),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // Already-dispatched packets are duplicates once the window has
        // passed them.
        assert_eq!(
            receive(&channel, 0).0,
            AddToReceiveWindowResult::NextInWindow
        );
        assert_eq!(receive(&channel, 0).0, AddToReceiveWindowResult::Duplicate);

        // A buffered packet that arrives twice is a duplicate on the second
        // go, and the window keeps only the first.
        assert_eq!(
            receive(&channel, 2).0,
            AddToReceiveWindowResult::BufferedInWindow
        );
        assert_eq!(receive(&channel, 2).0, AddToReceiveWindowResult::Duplicate);
        assert_eq!(channel.borrow().num_buffered_receives(), 1);

        // The resident packet is the one dispatched when the gap closes.
        let (_, head) = receive(&channel, 1);
        let Some(head) = head else {
            panic!("seq 1 is the expected packet");
        };
        assert_eq!(chain_seq(&head), vec![1, 2]);
    }

    /// A sequence outside the masked range and a foreign source address are
    /// corrupt: the reference logs an error and drops the packet.
    #[test]
    fn a_corrupt_packet_is_reported_corrupt() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00d2),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let own = channel.borrow().addr();

        // SEQ_SIZE itself is outside the 28-bit range.
        let mut wide = Packet::new();
        wide.set_seq(0x1000_0000);
        assert_eq!(
            UdpChannel::add_to_receive_window(&channel, Rc::new(wide), own).0,
            AddToReceiveWindowResult::Corrupt,
            "a seq outside seqMask() cannot index the ring"
        );

        // A packet from another address goes to the channel that owns it.
        assert_eq!(
            UdpChannel::add_to_receive_window(&channel, packet(2), addr(0x0200_00d2)).0,
            AddToReceiveWindowResult::Corrupt,
            "the source address is part of the channel identity"
        );

        // Neither rejected packet touched the window.
        assert_eq!(channel.borrow().in_seq_at, 0);
        assert_eq!(channel.borrow().num_buffered_receives(), 0);
    }

    /// A packet further ahead than the window will grow to is out of window:
    /// the receive buffer cannot reach it, and the reference drops it rather
    /// than chase it.
    #[test]
    fn a_packet_beyond_the_window_is_out_of_window() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00d3),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        let max = channel.borrow().max_window_size();
        assert_eq!(max, EXTERNAL_CHANNEL_SIZE + 1024, "external ceiling");

        assert_eq!(
            receive(&channel, max as SeqNum).0,
            AddToReceiveWindowResult::OutOfWindow,
            "one past the ceiling is unreachable"
        );
        assert_eq!(
            receive(&channel, 0).0,
            AddToReceiveWindowResult::NextInWindow,
            "the expected packet still lands"
        );
    }

    /// The receive window grows to take a packet that is in range but past
    /// the current ring size, carrying the buffered run forward.
    #[test]
    fn the_receive_window_grows_and_keeps_the_run() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00d4),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        // 300 ahead is past the 256-slot ring but inside the ceiling, so the
        // ring doubles rather than rejecting the packet.
        assert_eq!(
            receive(&channel, 300).0,
            AddToReceiveWindowResult::BufferedInWindow
        );
        assert_eq!(channel.borrow().num_buffered_receives(), 1);
        assert_eq!(
            channel.borrow().buffered_receives.size(),
            EXTERNAL_CHANNEL_SIZE * 2,
            "the ring doubled to hold the packet"
        );

        // The doubled ring kept the slot: the resident packet is still
        // there, so a retransmit of it is still a duplicate.
        assert_eq!(
            receive(&channel, 300).0,
            AddToReceiveWindowResult::Duplicate
        );

        // Fill the gaps and the whole run comes back out in order.
        for seq in 1..300 {
            assert_eq!(
                receive(&channel, seq).0,
                AddToReceiveWindowResult::BufferedInWindow
            );
        }
        let (_, head) = receive(&channel, 0);
        let Some(head) = head else {
            panic!("seq 0 is the expected packet");
        };
        let seen = chain_seq(&head);
        assert_eq!(seen.len(), 301, "the run reached through the grown ring");
        assert_eq!(seen[0], 0);
        assert_eq!(seen[300], 300);
        assert!(
            seen.windows(2).all(|pair| pair[0] + 1 == pair[1]),
            "the run dispatches in sequence order"
        );
        assert_eq!(channel.borrow().num_buffered_receives(), 0);
    }

    // ------------------------------------------------------------------
    // The bidirectional acceptance test (BW-02-03-07)
    // ------------------------------------------------------------------

    use crate::bundle_dispatch::dispatch_messages;
    use crate::interface_element::{FIXED_LENGTH_MESSAGE, InterfaceElement};
    use crate::interface_table::InterfaceTable;
    use crate::interfaces::InputMessageHandler;
    use crate::network_interface::InterfaceType;
    use crate::packet_receiver::PacketSink;
    use crate::udp_bundle::ReliableType;
    use crate::unpacked_message_header::UnpackedMessageHeader;
    use foundation::stream::BinaryStream;

    /// Records every message delivered through a [`DispatchSink`].
    #[derive(Default)]
    struct Recorder {
        notes: RefCell<Vec<(u8, Vec<u8>)>>,
    }

    impl Recorder {
        fn notes(&self) -> Vec<(u8, Vec<u8>)> {
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
            self.notes.borrow_mut().push((header.identifier(), body));
        }
    }

    /// The `PacketSink` that stands in for the interface table: strip the
    /// flags word and walk the messages exactly as the receiver's
    /// `dispatchMessages` does.
    struct DispatchSink {
        table: Rc<InterfaceTable>,
    }

    impl PacketSink for DispatchSink {
        fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
            let first_request = if packet.has_flags(flags::HAS_REQUESTS) {
                Some(packet.first_request_offset())
            } else {
                None
            };
            let body = &mut packet.data_mut()[HEADER_SIZE..];
            dispatch_messages(&self.table, addr, body, first_request)
        }
    }

    fn fixed(id: u8, size: i32) -> InterfaceElement {
        InterfaceElement::new("fixed", id, FIXED_LENGTH_MESSAGE, size, None)
    }

    /// One leg of the loopback pair: an interface bound to the kernel
    /// pick, a channel to the far end, and a served fixed message.
    struct Leg {
        interface: Rc<NetworkInterface>,
        channel: ChannelPtr,
        recorder: Rc<Recorder>,
    }

    /// Build the two legs and the channels pointing at each other. The
    /// `min_inactivity_resend_delay` keeps the resend period short for the
    /// loss arm.
    fn loopback_pair() -> (Leg, Leg) {
        let (iface_a, _recv_a) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let (iface_b, _recv_b) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");

        let addr_a = iface_a.address();
        let addr_b = iface_b.address();

        let chan_a = UdpChannel::new(&iface_a, addr_b, Traits::Internal, 0.05, CHANNEL_ID_NULL);
        let chan_b = UdpChannel::new(&iface_b, addr_a, Traits::Internal, 0.05, CHANNEL_ID_NULL);

        // A received packet owes an ack; pushing at one forces the ack
        // bundle out immediately, which is what keeps the window draining.
        chan_a.borrow_mut().set_push_unsent_acks_threshold(1);
        chan_b.borrow_mut().set_push_unsent_acks_threshold(1);

        let recorder_a = Rc::new(Recorder::default());
        let table_a = Rc::new(InterfaceTable::new());
        table_a.serve(
            &fixed(3, 4),
            Some(Rc::clone(&recorder_a) as Rc<dyn InputMessageHandler>),
        );
        iface_a.set_packet_sink(Some(Rc::new(DispatchSink {
            table: Rc::clone(&table_a),
        })));

        let recorder_b = Rc::new(Recorder::default());
        let table_b = Rc::new(InterfaceTable::new());
        table_b.serve(
            &fixed(3, 4),
            Some(Rc::clone(&recorder_b) as Rc<dyn InputMessageHandler>),
        );
        iface_b.set_packet_sink(Some(Rc::new(DispatchSink {
            table: Rc::clone(&table_b),
        })));

        (
            Leg {
                interface: iface_a,
                channel: chan_a,
                recorder: recorder_a,
            },
            Leg {
                interface: iface_b,
                channel: chan_b,
                recorder: recorder_b,
            },
        )
    }

    /// A reliable fixed message, sent from `from` to `to`.
    fn send_reliable(from: &Leg, to: &Leg, payload: &[u8; 4]) {
        let mut bundle = UdpBundle::new(0);
        bundle.start_message(&fixed(3, 4), ReliableType::Driver);
        bundle.write_bytes(payload);
        UdpChannel::send_bundle(&from.channel, &mut bundle);
        assert!(to.interface.poller().process_pending_events(0.1) >= 1);
        // The receiving channel pushed an ack bundle straight back.
        assert!(from.interface.poller().process_pending_events(0.1) >= 1);
    }

    /// Two directions exchange a reliable message and count a resend.
    #[test]
    fn both_directions_exchange_and_count_resends() {
        let (a, b) = loopback_pair();

        // A -> B.
        send_reliable(&a, &b, b"AA1X");
        assert_eq!(b.recorder.notes(), vec![(3, b"AA1X".to_vec())]);
        assert_eq!(a.channel.borrow().num_reliable_packets_sent(), 1);
        assert_eq!(a.channel.borrow().num_packets_resent(), 0);

        // B -> A.
        send_reliable(&b, &a, b"BB2Y");
        assert_eq!(a.recorder.notes(), vec![(3, b"BB2Y".to_vec())]);
        assert_eq!(b.channel.borrow().num_reliable_packets_sent(), 1);
        assert_eq!(b.channel.borrow().num_packets_resent(), 0);

        // The exchange cleared both send windows.
        assert!(!a.channel.borrow().has_unacked_packets());
        assert!(!b.channel.borrow().has_unacked_packets());
    }

    /// A lost reliable packet is resent and delivered exactly once.
    #[test]
    fn a_lost_packet_is_resent_and_delivered_once() {
        let (a, b) = loopback_pair();
        UdpChannel::set_is_remote_regular(&a.channel, false);

        // Send, but never drain b's socket: the datagram sits in the
        // kernel buffer, so no ack returns and the window stays full.
        let mut bundle = UdpBundle::new(0);
        bundle.start_message(&fixed(3, 4), ReliableType::Driver);
        bundle.write_bytes(b"LSTX");
        UdpChannel::send_bundle(&a.channel, &mut bundle);
        assert!(a.channel.borrow().has_unacked_packets());

        // The resend period is max(2*RTT, minInactivityResendDelay) =
        // 200ms; wait past it and scan.
        std::thread::sleep(std::time::Duration::from_millis(250));
        UdpChannel::check_resend_timers(&a.channel);
        assert_eq!(
            a.channel.borrow().num_packets_resent(),
            1,
            "the lost packet went back out once"
        );

        // Now drain b: both the original and the resend are queued. The
        // first delivers and acks; the duplicate is ack-only.
        assert!(b.interface.poller().process_pending_events(0.1) >= 1);
        assert_eq!(
            b.recorder.notes(),
            vec![(3, b"LSTX".to_vec())],
            "the resend did not double-deliver"
        );
        assert!(a.interface.poller().process_pending_events(0.1) >= 1);
        assert!(!a.channel.borrow().has_unacked_packets());
    }

    // -- The channel filter (BW-02-05-01) ---------------------------------

    /// A no-op filter, so the accessors and the guard are drivable.
    struct TestFilter;

    impl PacketFilter for TestFilter {
        fn recv(
            &self,
            _addr: &Address,
            _packet: &mut Packet,
            _next: &dyn Fn(&Address, &mut Packet) -> Reason,
        ) -> Reason {
            Reason::Success
        }
    }

    /// A filter that claims spare room.
    struct SpareFilter {
        spare: usize,
    }

    impl PacketFilter for SpareFilter {
        fn max_spare_size(&self) -> usize {
            self.spare
        }
        fn recv(
            &self,
            _addr: &Address,
            _packet: &mut Packet,
            _next: &dyn Fn(&Address, &mut Packet) -> Reason,
        ) -> Reason {
            Reason::Success
        }
    }

    /// `pFilter` (udp_channel.hpp:139-140): a filter fits on a channel and
    /// reads back; the constructor critical (udp_channel.cpp:129) -- no
    /// filters on indexed channels -- lands in the setter.
    #[test]
    fn a_filter_fits_on_a_channel_but_not_an_indexed_one() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);

        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00d1),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert!(channel.borrow().p_filter().is_none());
        channel.borrow_mut().set_p_filter(Some(Rc::new(TestFilter)));
        assert!(channel.borrow().p_filter().is_some());

        let indexed = UdpChannel::new(
            &interface,
            addr(0x0100_00d2),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            1,
        );
        let fitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            indexed.borrow_mut().set_p_filter(Some(Rc::new(TestFilter)));
        }));
        assert!(fitted.is_err(), "filters are refused on indexed channels");
    }

    /// `clearBundle` (channel.cpp:299) rebuilds through `newBundle`, which
    /// sizes the spare room off the current filter's `maxSpareSize`
    /// (udp_channel.cpp:1523-1529): a filter that claims room puts its
    /// padding on the bundle's packets, one that claims none reuses.
    #[test]
    fn a_claiming_filter_sizes_the_own_bundle() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00d3),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        channel.borrow_mut().clear_bundle();
        assert_eq!(
            channel.borrow().bundle().packet(0).extra_filter_size(),
            0,
            "no filter: no spare room"
        );

        channel
            .borrow_mut()
            .set_p_filter(Some(Rc::new(SpareFilter { spare: 8 })));
        channel.borrow_mut().clear_bundle();
        assert_eq!(
            channel.borrow().bundle().packet(0).extra_filter_size(),
            8,
            "the claiming filter's room rides the bundle"
        );
    }
}
