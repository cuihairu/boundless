//! `lib/network/udp_channel.cpp` -- the registration-and-lifecycle slice
//! of `Mercury::UDPChannel` (BW-02-03-02) plus the send window and its acks
//! (BW-02-03-03): construction registers a non-indexed channel with its
//! interface, `condemn` hands it to the condemned list, `destroy` closes it
//! for good, and `addResendTimer`/`handleAck` keep the unacked ring honest
//! across the `SEQ_SIZE` seam. The resend timers (BW-02-03-04) and the
//! receive-window filtering (BW-02-03-05) are now active; their fields
//! still mirror the reference constructor.
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
use crate::packet_receiver_stats::PacketReceiverStats;
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

/// `UDPChannel::AddToReceiveWindowResult` (udp_channel.hpp:161): how an
/// incoming reliable packet was classified by the receive window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddToReceiveWindowResult {
    /// The packet was the one expected next, and the run of buffered
    /// successors it unblocked is ready to dispatch.
    PacketIsNextInWindow,
    /// The packet arrived ahead of the hole and is parked in the buffer
    /// until the missing sequence turns up.
    PacketIsBufferedInWindow,
    /// The packet was already seen (below `inSeqAt_`, or sitting in a
    /// buffered slot).
    PacketIsDuplicate,
    /// The sequence is too far ahead of the window to buffer.
    PacketIsOutOfWindow,
    /// The sequence is invalid, or the packet came from an unexpected
    /// source address.
    PacketIsCorrupt,
}

/// `processOrderedPacket`'s fragment branch, on-channel half
/// (packet_receiver.cpp:1236-1360): the verdict after one on-channel
/// fragment was offered to [`UdpChannel::process_fragment`].
///
/// `Packet` carries no derives, so the variants are matched with
/// `matches!` rather than `assert_eq!`.
pub enum FragmentResult {
    /// The fragment was stashed in the channel's bundle, which is still
    /// waiting for the rest (the reference's `REASON_SUCCESS` with no
    /// chain handed off yet).
    Waiting,
    /// The bundle just completed: the ordered fragment chain is handed out
    /// for dispatch, the reference's `pChain`.
    Complete(Vec<Packet>),
    /// The fragment is corrupt and the caller should drop it: the footers
    /// would not strip, or the chain is a reliable fragment whose range
    /// disagrees with the bundle already held.
    Corrupt,
}

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
    /// `inSeqAt_` (BW-02-03-05): the next sequence number the receive
    /// window expects.
    in_seq_at: SeqNum,
    /// `unreliableInSeqAt_` (BW-02-03-05): the unreliable slot's ring
    /// position, still `SEQ_NULL` until the unreliable path lands.
    unreliable_in_seq_at: SeqNum,
    /// `bufferedReceives_` (BW-02-03-05): ordered packets that arrived
    /// ahead of the hole, parked at their sequence numbers.
    buffered_receives: CircularArray<Option<Packet>>,
    /// `numBufferedReceives_` (BW-02-03-05).
    num_buffered_receives: u32,
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
    /// `numPacketsResent_` (udp_channel.hpp:528): how many packets
    /// `checkResendTimers` has re-sent.
    num_packets_resent: u32,
    /// `isLocalRegular_` (BW-02-03-04): whether this channel sends
    /// regularly. The setter's `irregularChannels()` registration lands
    /// with the send path (BW-02-03-06).
    is_local_regular: bool,
    /// `isRemoteRegular_` (BW-02-03-04): whether the peer acknowledges
    /// regularly. False means the channel ages its unacked packets.
    is_remote_regular: bool,
    /// `acksToSend_` (BW-02-03-05): the reliable sequences that still owe
    /// an ACK on the next outgoing bundle. Piggybacks are absent by
    /// construction.
    acks_to_send: BTreeSet<SeqNum>,
    /// `pushUnsentAcksThreshold_` (BW-02-03-05): how many unsent ACKs
    /// force the outgoing bundle to go out early. 0 disables the push.
    push_unsent_acks_threshold: usize,
    /// `shouldAutoSwitchToSrcAddr_` (BW-02-03-05): adopt a new source
    /// address as soon as its channel version is usable.
    should_auto_switch_to_src_addr: bool,
    /// The real `send()` call that drains [`Self::acks_to_send`] lands
    /// with the send path (BW-02-03-06); until then, crossing the push
    /// threshold parks the demand here for the caller to observe.
    acks_need_push: bool,
    /// `pFragments_` (udp_channel.hpp:449, BW-02-03-06): the bundle this
    /// channel is mid-reassembly on. The reference keeps a refcounted
    /// `FragmentedBundlePtr`; here the channel owns the one bundle
    /// outright, and a `None` is the reference's null.
    p_fragments: Option<FragmentedBundle>,
    /// `numReliablePacketsSent_` (BW-02-03-07): how many reliable
    /// bundles have gone out. The first one carries `FLAG_CREATE_CHANNEL`
    /// on an internal channel.
    num_reliable_packets_sent: u32,

    /// The interface this channel registers with. Weak so the map's
    /// strong references do not keep the interface alive.
    network_interface: Weak<NetworkInterface>,
}

impl UdpChannel {
    /// `UDPChannel( networkInterface, addr, traits, minInactivityResendDelay, id )`
    /// (udp_channel.cpp:82): initialise the window fields from the traits
    /// and id, open the bundle, and register non-indexed channels through
    /// [`set_address`](Self::set_address_fn). The `pFilter && id_` critical
    /// is unrepresentable until filters land (BW-02-05).
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
            buffered_receives: CircularArray::new(size),
            num_buffered_receives: 0,
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
            num_packets_resent: 0,
            is_local_regular: true,
            is_remote_regular: true,
            acks_to_send: BTreeSet::new(),
            push_unsent_acks_threshold: 0,
            should_auto_switch_to_src_addr: false,
            acks_need_push: false,
            p_fragments: None,
            num_reliable_packets_sent: 0,
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

        {
            // isLocalRegular( false ) / isRemoteRegular( false ) land with
            // the regularity bookkeeping (BW-02-03-03+).
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

    /// `maxWindowSize()` (udp_channel.hpp:433): the widest the receive
    /// window may stretch, window plus the tolerance band the reference
    /// allows before it starts complaining.
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
    /// the window out again -- refresh the channel version footer it
    /// carries and hand it back to the interface -- stamping the
    /// bookkeeping the ack and resend paths read. An absent slot is a
    /// no-op where the reference would dereference null.
    fn send_unacked(channel: &ChannelPtr, seq: SeqNum) {
        let now = UdpChannel::now(channel);
        let (interface, addr, version, id, packet) = {
            let mut this = channel.borrow_mut();
            let small = this.small_out_seq_at;
            let packet = match this.unacked_packets.get_mut(seq as usize).as_mut() {
                Some(unacked) => {
                    unacked.set_last_sent_at_out_seq(small);
                    unacked.set_was_resent(true);
                    unacked.set_last_sent_time(now);
                    (**unacked.packet()).clone()
                }
                None => return,
            };
            this.last_reliable_resend_time = now;
            (
                this.network_interface.upgrade(),
                this.addr,
                this.version,
                this.id,
                packet,
            )
        };

        if let Some(interface) = interface {
            let mut packet = packet;
            // `pPacket_->updateChannelVersion( version_, id_ )`.
            packet.update_channel_version(version, id);
            interface.send_packet(addr, packet.wire());
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

    // -- Retransmit timing (BW-02-03-04) ---------------------------------

    /// `checkResendTimers( bundle )` (udp_channel.cpp:1068): resend any
    /// unacked packet that has taken too long to come back, or that a newer
    /// ack has moved past. Called when the channel's bundle is finalised
    /// (`doPreFinaliseBundle`, udp_channel.cpp:686). The piggyback re-send
    /// path the reference uses for external channels is deferred with the
    /// send path (BW-02-03-06), so every resend currently takes the
    /// `sendUnacked` fallback and the packet stays unacked.
    pub fn check_resend_timers(channel: &ChannelPtr) {
        let now = UdpChannel::now(channel);
        let mut this = channel.borrow_mut();

        // There are no un-acked packets.
        if this.oldest_unacked_seq == SEQ_NULL {
            return;
        }

        // Don't do anything if the remote process has failed.
        if this.has_remote_failed {
            return;
        }

        // Anything that has taken more than twice the RTT on the channel to
        // come back is considered too old.
        let resend_period =
            std::cmp::max(this.round_trip_time * 2, this.min_inactivity_resend_delay);
        let is_irregular = !this.is_remote_regular;
        let end_seq = if is_irregular {
            this.small_out_seq_at
        } else {
            this.highest_ack
        };

        // Throttle resends a little in extreme situations (udp_channel.cpp:
        // 1104): no more than a windowSize_/8 per pass.
        let max_resends = this.window_size / 8;
        let mut num_resends = 0;
        let mut seq = this.oldest_unacked_seq;
        while seq_less_than(seq, end_seq) && num_resends < max_resends {
            let should_resend = match this.unacked_packets.get(seq as usize) {
                Some(unacked) => {
                    let has_newer_ack =
                        seq_less_than(unacked.last_sent_at_out_seq(), this.highest_ack);
                    has_newer_ack
                        || (is_irregular
                            && (now.saturating_sub(unacked.last_sent_time()) > resend_period))
                }
                None => false,
            };
            if should_resend {
                drop(this);
                UdpChannel::resend(channel, seq);
                num_resends += 1;
                this = channel.borrow_mut();
            }
            seq = seq_mask(seq.wrapping_add(1));
        }
    }

    /// `resend( seq, bundle )` (udp_channel.cpp:1168): re-send an unacked
    /// packet. Returns whether the packet is no longer unacked; without the
    /// piggyback path (BW-02-03-06) every resend goes through
    /// `sendUnacked` and the packet stays unacked, so this is always false
    /// here.
    fn resend(channel: &ChannelPtr, seq: SeqNum) -> bool {
        {
            let mut this = channel.borrow_mut();
            this.num_packets_resent += 1;
        }
        UdpChannel::send_unacked(channel, seq);
        false
    }

    /// `UDPChannel::addToReceiveWindow( p, srcAddr, stats )`
    /// (udp_channel.cpp:1220): classify one incoming reliable packet
    /// against the receive window and, when it closes a hole, hand back the
    /// ordered run to dispatch.
    ///
    /// The returned run is the incoming packet followed by the buffered
    /// successors its arrival unblocked. The reference links those
    /// successors onto the packet through `Packet::chain` and the caller
    /// walks `next()`; the port has no packet links, so the run travels as
    /// a list instead. It is empty for every result but
    /// [`AddToReceiveWindowResult::PacketIsNextInWindow`].
    pub fn add_to_receive_window(
        channel: &ChannelPtr,
        packet: Packet,
        src_addr: Address,
        stats: &mut PacketReceiverStats,
    ) -> (AddToReceiveWindowResult, Vec<Packet>) {
        let seq = packet.seq();

        // Make sure the sequence number is valid.
        if seq_mask(seq) != seq {
            return (AddToReceiveWindowResult::PacketIsCorrupt, Vec::new());
        }

        if channel.borrow().should_auto_switch_to_src_addr {
            // We switch address if the version number is acceptable. We
            // switch on equal version numbers because the first packet
            // from a cell entity sets the address and is version 0.
            let switch = {
                let this = channel.borrow();
                !seq_less_than(packet.channel_version(), this.version)
            };
            if switch {
                channel.borrow_mut().version = packet.channel_version();
                UdpChannel::set_address_fn(channel, src_addr);
            }
        } else if channel.borrow().addr != src_addr {
            return (AddToReceiveWindowResult::PacketIsCorrupt, Vec::new());
        }

        let mut ready: Vec<Packet> = Vec::new();
        {
            let mut this = channel.borrow_mut();

            if !packet.is_piggyback() {
                // No need to ACK piggybacks as they are implicitly ACKed by
                // the containing packet's ACK. ACKs are always added for
                // the others even though a cumulative ack may mean they
                // are never sent: it still shows the channel is not empty.
                this.acks_to_send.insert(seq);
            }

            // Push the outgoing bundle immediately if required.
            if this.push_unsent_acks_threshold != 0
                && this.acks_to_send.len() >= this.push_unsent_acks_threshold
            {
                // The real `send()` call lands with the send path
                // (BW-02-03-06); until then the threshold parks the demand
                // on the channel for the caller to drain.
                this.acks_need_push = true;
            }

            // The good case first: this is the packet we expected.
            if seq == this.in_seq_at {
                this.in_seq_at = seq_mask(this.in_seq_at.wrapping_add(1));
                ready.push(packet);

                // Attach as many buffered packets as possible to this one,
                // linking each to its predecessor and clearing the slot.
                loop {
                    let at = this.in_seq_at;
                    let buffered = this.buffered_receives.get_mut(at as usize).take();
                    match buffered {
                        Some(next) => {
                            ready.push(next);
                            this.num_buffered_receives -= 1;
                            this.in_seq_at = seq_mask(this.in_seq_at.wrapping_add(1));
                        }
                        None => break,
                    }
                }

                return (AddToReceiveWindowResult::PacketIsNextInWindow, ready);
            }

            // See if we've got this one before: we have if seq < inSeqAt_.
            if seq_less_than(seq, this.in_seq_at) {
                stats.inc_duplicate_packets();
                return (AddToReceiveWindowResult::PacketIsDuplicate, Vec::new());
            }

            // Make sure it's in range. Packets are stored between
            // [inSeqAt_ + 1, inSeqAt_ + bufferedReceives_.size()], which is
            // why the bounds below compare strictly greater.
            let required_window_size = seq_mask(seq.wrapping_sub(this.in_seq_at));
            if (required_window_size as usize > 2 * this.buffered_receives.size())
                || (required_window_size as usize > this.max_window_size())
            {
                return (AddToReceiveWindowResult::PacketIsOutOfWindow, Vec::new());
            } else if required_window_size as usize > this.buffered_receives.size() {
                // Copy the buffered receives from the start of the ring,
                // the first parked at inSeqAt_ + 1.
                let start = this.in_seq_at.wrapping_add(1) as usize;
                this.buffered_receives.double_size(start);
            }

            // ... buffer the packet, but only if the slot is free.
            let slot = this.buffered_receives.get_mut(seq as usize);
            if slot.is_some() {
                if slot.as_ref().unwrap().seq() == seq {
                    stats.inc_duplicate_packets();
                } else {
                    // The reference logs this as critical: two sequences
                    // can never share a slot, since a slot's index is the
                    // sequence.
                    assert_eq!(
                        slot.as_ref().unwrap().seq(),
                        seq,
                        "two sequences share a buffered receive slot"
                    );
                }
            } else {
                *slot = Some(packet);
                this.num_buffered_receives += 1;
            }

            (
                AddToReceiveWindowResult::PacketIsBufferedInWindow,
                Vec::new(),
            )
        }
    }

    // -- Fragment reassembly (BW-02-03-06) -------------------------------

    /// `pFragments()` (udp_channel.hpp:225): the bundle this channel is
    /// mid-reassembly on, if any. `None` is the reference's null.
    pub fn p_fragments(&self) -> Option<&FragmentedBundle> {
        self.p_fragments.as_ref()
    }

    /// `processOrderedPacket`'s fragment branch, on-channel half
    /// (packet_receiver.cpp:1236-1360): strip the footers off one
    /// on-channel fragment and join it into the channel's bundle.
    ///
    /// Call this for a reliable, on-channel packet carrying
    /// `FLAG_IS_FRAGMENT`. The reference walks the same branch for both
    /// on- and off-channel packets; here the channel's own [`Self::p_fragments`]
    /// slot is the seam, and the off-channel map lives with the once-off
    /// receiver (BW-02-03-06). The "expecting a fragment" guard for a
    /// non-fragment packet (packet_receiver.cpp:1209-1232) and the
    /// `clearState` reclaim (udp_channel.cpp:301) belong to the wired
    /// receive path, still to come.
    ///
    /// The fragment is corrupt when its footers will not strip, when it
    /// would start the channel's bundle off its head (channel traffic is
    /// ordered, so the first fragment seen must be the bundle's first),
    /// or when `FragmentedBundle::add_packet` rejects it as mangled. A
    /// completed bundle hands its ordered chain back through
    /// [`FragmentResult::Complete`] and clears the slot; anything else
    /// leaves the bundle parked.
    pub fn process_fragment(
        &mut self,
        packet: Packet,
        is_external: bool,
        source_str: &str,
        now: u64,
    ) -> FragmentResult {
        // The footers are already off: `processOrderedPacket` strips them
        // (`p->stripFragInfo()`) once, filling `fragBegin_`/`fragEnd_`
        // before the chain logic here reads them (packet_receiver.cpp:1238).
        // This half is the reference's on-channel branch, lifted out so it
        // is drivable in isolation.

        if self.p_fragments.is_none() {
            // Channel traffic is ordered, so the first fragment of a
            // bundle on a channel must be the bundle's first.
            if packet.seq() != packet.frag_begin() {
                return FragmentResult::Corrupt;
            }

            self.p_fragments = Some(FragmentedBundle::new(packet, now));
            return FragmentResult::Waiting;
        }

        let fragments = self.p_fragments.as_mut().expect("checked above");
        if !fragments.add_packet(packet, is_external, source_str, now) {
            return FragmentResult::Corrupt;
        }
        if !fragments.is_complete() {
            return FragmentResult::Waiting;
        }

        // The bundle is complete: hand the ordered chain out and clear
        // the slot for the next one.
        let chain = fragments.take_chain();
        self.p_fragments = None;
        FragmentResult::Complete(chain)
    }

    // -- The send path (BW-02-03-07) ------------------------------------

    /// `hasUnsentData()` (bundle.hpp): the channel's bundle still owes
    /// bytes or is reliable. The port's `isComplete` (all data units
    /// written) is read as "empty" -- a bundle with messages in it is
    /// not complete; the reliable half of the disjunction is exact.
    pub fn has_unsent_data(&self) -> bool {
        !self.bundle.is_empty() || self.bundle.is_reliable()
    }

    /// `hasAcks()` (udp_channel.hpp): the channel owes the peer an ack on
    /// the next outgoing bundle.
    pub fn has_acks(&self) -> bool {
        !self.acks_to_send.is_empty()
    }

    /// `numReliablePacketsSent()` (udp_channel.hpp:537).
    pub fn num_reliable_packets_sent(&self) -> u32 {
        self.num_reliable_packets_sent
    }

    /// `writeFlags( p )` (udp_channel.cpp:1526): mark a packet as coming
    /// from this channel and reserve the footers the flags promise. The
    /// cumulative ack and the ack list ride out on the next bundle, and
    /// the first reliable packet on an internal channel asks the peer to
    /// create one.
    pub fn write_flags(&mut self, packet: &mut Packet) {
        packet.enable_flags(flags::ON_CHANNEL);

        if self.is_indexed() {
            packet.enable_flags(flags::INDEXED_CHANNEL);
            packet.set_channel_id(self.id);
            packet.set_channel_version(self.version);
            packet.reserve_footer(4 + 4);
        }

        // Add a cumulative ACK. This indicates that all packets BEFORE a
        // given seq have been received.
        if packet.free_space() >= 4 {
            packet.enable_flags(flags::HAS_CUMULATIVE_ACK);
            packet.reserve_footer(4);

            // Need to go through all due to wrap-around case.
            let in_seq_at = self.in_seq_at;
            self.acks_to_send
                .retain(|seq| !seq_less_than(*seq, in_seq_at));
        }

        // Put on as many acks as we can. The reference's
        // `freeSpace >= 1 + 4` is a header byte plus one sequence.
        if !self.acks_to_send.is_empty() && packet.free_space() >= 5 {
            // `Packet::MAX_ACKS` (packet.hpp:97).
            const MAX_ACKS: usize = 255;
            packet.enable_flags(flags::HAS_ACKS);
            packet.reserve_footer(1);

            let min_space = packet.free_space() / 4;
            let n_acks = min_space.min(self.acks_to_send.len()).min(MAX_ACKS);
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

    /// `writeFooter( p )` (udp_channel.cpp:1593): write the footers the
    /// channel reserved in [`Self::write_flags`]. Must be called after
    /// `write_flags`.
    pub fn write_footer(&mut self, packet: &mut Packet) {
        if packet.has_flags(flags::INDEXED_CHANNEL) {
            packet.pack_footer_u32(self.id as u32);
            packet.pack_footer_u32(self.version);
        }

        if packet.has_flags(flags::HAS_CUMULATIVE_ACK) {
            packet.pack_footer_u32(self.in_seq_at);
        }

        if packet.has_flags(flags::HAS_ACKS) {
            // Note: Technically we should start at inSeqAt_ since sequence
            // numbers wrap around but this is rare enough not to worry
            // about.
            packet.pack_footer_u8(packet.n_acks());
            let mut acks_added = 0u8;
            while !self.acks_to_send.is_empty() && acks_added < packet.n_acks() {
                let seq = *self.acks_to_send.iter().next().expect("checked above");
                packet.pack_footer_u32(seq);
                self.acks_to_send.remove(&seq);
                acks_added += 1;
            }
        }
    }

    /// `Channel::send( pBundle )` (channel.cpp:149), the way UDPChannel
    /// drives it: pre-finalise, finalise, hand to the interface, then
    /// clear the bundle.
    ///
    /// `external` is the reference's `pBundle` argument. `None` sends the
    /// channel's own bundle; `Some(bundle)` sends a caller-owned one, the
    /// empty bundle the ack-push path uses. The bundle is moved out of
    /// the channel first so it can be walked while the channel is still
    /// borrowed for sequence numbers and acks.
    pub fn send(channel: &ChannelPtr, external: Option<&mut UdpBundle>) {
        if !channel.borrow().is_connected() {
            eprintln!(
                "Channel::send( {} ): Channel is not connected",
                channel.borrow().c_str()
            );
            return;
        }

        match external {
            Some(bundle) => {
                UdpChannel::do_pre_finalise_bundle(channel, bundle);
                bundle.finalise();
                UdpChannel::do_send(channel, bundle, false);
                bundle.clear();
            }
            None => {
                let mut own = std::mem::take(&mut channel.borrow_mut().bundle);
                UdpChannel::do_pre_finalise_bundle(channel, &mut own);
                own.finalise();
                UdpChannel::do_send(channel, &mut own, true);
                // `clearBundle()` (channel.cpp:299): reuse the bundle.
                own.clear();
                channel.borrow_mut().bundle = own;
            }
        }
    }

    /// `doPreFinaliseBundle( bundle )` (udp_channel.cpp:681): the resend
    /// check that runs before the bundle is finalised. The piggyback
    /// tack-on the reference also does here arrives with the piggyback
    /// send path (BW-02-03-08).
    fn do_pre_finalise_bundle(_channel: &ChannelPtr, _bundle: &mut UdpBundle) {
        UdpChannel::check_resend_timers(_channel);
    }

    /// `doSend( bundle )` (udp_channel.cpp:717): the guards, the hand to
    /// the interface, and the send-side stats. `is_own` is the
    /// reference's `isSendingOwnBundle`.
    fn do_send(channel: &ChannelPtr, bundle: &mut UdpBundle, is_own: bool) {
        let interface = match channel.borrow().network_interface.upgrade() {
            Some(interface) => interface,
            None => return,
        };

        {
            let this = channel.borrow();
            // Don't do anything if the remote process has failed.
            if this.has_remote_failed {
                // `cancelRequestsFor( bundle, REASON_CHANNEL_LOST )`: the
                // port cancels by address, the channel's one.
                interface.cancel_requests_for(&this.addr);
                return;
            }
            if this.addr.ip == 0 {
                return;
            }
            // If we're sending the channel's bundle and it's empty, don't.
            if is_own && !this.has_unsent_data() {
                return;
            }
        }

        let addr = channel.borrow().addr;

        // The bundle is handed to the interface as UDP packets.
        interface.send_bundle(addr, bundle, Some(channel));

        let size = bundle.size();
        let is_reliable = bundle.is_reliable();
        let mut this = channel.borrow_mut();
        this.num_data_units_sent += 1;
        this.num_bytes_sent += size as u32;
        if is_reliable {
            this.num_reliable_packets_sent += 1;
        }
        // `irregularChannels().addIfNecessary` (BW-02-03-08) and the
        // critical-bundle `unackedCriticalSeq_` assignment ride with the
        // piggyback/regularity work.
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

    /// `clearBundle()` (channel.cpp:299): reuse the bundle or make one,
    /// primers being BW-02-05.
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
    /// unset until the unreliable path lands.
    pub fn unreliable_in_seq_at(&self) -> SeqNum {
        self.unreliable_in_seq_at
    }

    /// `numBufferedReceives()`: how many packets are parked ahead of the
    /// hole right now.
    pub fn num_buffered_receives(&self) -> u32 {
        self.num_buffered_receives
    }

    /// `bufferedReceives()`: the out-of-order packet ring, addressed by
    /// sequence number.
    pub fn buffered_receives(&self) -> &CircularArray<Option<Packet>> {
        &self.buffered_receives
    }

    /// `acksToSend()`: the sequences that still owe an ACK.
    pub fn acks_to_send(&self) -> &BTreeSet<SeqNum> {
        &self.acks_to_send
    }

    /// `pushUnsentAcksThreshold()` (udp_channel.hpp:249): how many unsent
    /// ACKs force the outgoing bundle out early, 0 to disable.
    pub fn push_unsent_acks_threshold(&self) -> usize {
        self.push_unsent_acks_threshold
    }

    /// `pushUnsentAcksThreshold( i )` (udp_channel.hpp:252).
    pub fn set_push_unsent_acks_threshold(channel: &ChannelPtr, threshold: usize) {
        channel.borrow_mut().push_unsent_acks_threshold = threshold;
    }

    /// `shouldAutoSwitchToSrcAddr()` (udp_channel.hpp:202).
    pub fn should_auto_switch_to_src_addr(&self) -> bool {
        self.should_auto_switch_to_src_addr
    }

    /// `shouldAutoSwitchToSrcAddr( bool )`.
    pub fn set_should_auto_switch_to_src_addr(channel: &ChannelPtr, switch: bool) {
        channel.borrow_mut().should_auto_switch_to_src_addr = switch;
    }

    /// Take the pending ACK-push request, clearing it. The send path
    /// (BW-02-03-06) drains this instead of the real `send()` call.
    pub fn take_acks_need_push(&mut self) -> bool {
        std::mem::take(&mut self.acks_need_push)
    }

    /// Take the queued ACKs, clearing the set, for the next outgoing
    /// bundle.
    pub fn take_acks_to_send(&mut self) -> BTreeSet<SeqNum> {
        std::mem::take(&mut self.acks_to_send)
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

    /// `lastReliableSendOrResendTime()` (udp_channel.hpp:275): the last
    /// time a reliable packet was sent for the first time or re-sent.
    pub fn last_reliable_send_or_resend_time(&self) -> u64 {
        std::cmp::max(self.last_reliable_send_time, self.last_reliable_resend_time)
    }

    /// `numPacketsResent()` (udp_channel.hpp:257).
    pub fn num_packets_resent(&self) -> u32 {
        self.num_packets_resent
    }

    /// `isLocalRegular()` (udp_channel.hpp:142).
    pub fn is_local_regular(&self) -> bool {
        self.is_local_regular
    }

    /// `isRemoteRegular()` (udp_channel.hpp:145).
    pub fn is_remote_regular(&self) -> bool {
        self.is_remote_regular
    }

    /// `isRemoteRegular( bool )` (udp_channel.cpp:1730). The reference
    /// setter is a plain flag store; irregular channels reach the periodic
    /// resend check through `irregularChannels()`, which lands with the
    /// send path (BW-02-03-06).
    pub fn set_is_remote_regular(channel: &ChannelPtr, regular: bool) {
        channel.borrow_mut().is_remote_regular = regular;
    }

    /// `isLocalRegular( bool )` (udp_channel.cpp:1715), minus the
    /// `irregularChannels()` registration (BW-02-03-06).
    pub fn set_is_local_regular(channel: &ChannelPtr, regular: bool) {
        channel.borrow_mut().is_local_regular = regular;
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
    use crate::packet::flags;
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
        assert_eq!(this.num_buffered_receives(), 0);
        assert_eq!(
            this.buffered_receives().size(),
            EXTERNAL_CHANNEL_SIZE,
            "the receive ring opens at the window size"
        );
        assert!(this.acks_to_send().is_empty());
        assert_eq!(this.push_unsent_acks_threshold(), 0);
        assert!(!this.should_auto_switch_to_src_addr());
        assert!(this.is_local_regular());
        assert!(this.is_remote_regular());
        // The bundle starts on one empty packet, and `clearBundle`
        // resets it back to that.
        assert_eq!(this.bundle().num_data_units(), 1);
        drop(this);

        channel.borrow_mut().clear_bundle();
        channel.borrow_mut().set_channel_listener(None);
        // The regular flags and the ACK hand-off all write through plain
        // setters.
        UdpChannel::set_is_local_regular(&channel, false);
        assert!(!channel.borrow().is_local_regular());
        UdpChannel::set_is_local_regular(&channel, true);
        {
            let mut this = channel.borrow_mut();
            assert!(this.take_acks_to_send().is_empty());
            assert!(!this.take_acks_need_push());
        }
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

    // -- BW-02-03-04: retransmit timing (checkResendTimers) ---------------

    /// The acceptance for the item: a packet whose ack is lost is resent
    /// exactly once per resend period — re-checking immediately after does
    /// not resend it again, so a single controlled loss yields a single
    /// delivery.
    #[test]
    fn an_old_packet_is_resent_once_per_resend_period() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e0),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        // Irregular channels age their unacked packets and resend them.
        UdpChannel::set_is_remote_regular(&channel, false);

        send(&channel); // seq 0, at now = 1_000_000
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // A fresh packet is not resent within the resend period.
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 0);

        // resendPeriod = max(RTT*2, minInactivityResendDelay). The internal
        // RTT opens at 100ms (200ms doubled); the default delay is 1s.
        clock.set(2_000_001); // just past one second of inactivity
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 1);
        {
            let this = channel.borrow();
            assert!(this.is_unacked(0), "no piggyback yet: the slot stays");
            let unacked = this.unacked_packets().get(0).as_ref().unwrap();
            assert!(unacked.was_resent(), "the resend is flagged");
            assert_eq!(unacked.last_sent_time(), 2_000_001);
            assert_eq!(this.last_reliable_resend_time(), 2_000_001);
            assert_eq!(this.last_reliable_send_or_resend_time(), 2_000_001);
        }

        // Re-checking at the same instant does not re-resend: one delivery
        // per resend period.
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 1);
    }

    /// A regular channel only resends a packet when a newer ack has moved
    /// past it — the age-based resend is an irregular-channel behaviour.
    #[test]
    fn a_regular_channel_resends_only_for_a_newer_ack() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e1),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        assert!(channel.borrow().is_remote_regular());

        send(&channel); // seq 0
        clock.set(2_000_001); // old, but regular: no age resend
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(
            channel.borrow().num_packets_resent(),
            0,
            "a regular channel's endSeq (highestAck) excludes the unacked packet"
        );

        // An out-of-order ack for a later packet makes the low packet
        // "stale": its last sent position is behind the highest ack.
        send(&channel); // seq 1
        assert!(UdpChannel::handle_ack(&channel, 1));
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(
            channel.borrow().num_packets_resent(),
            1,
            "a newer ack forces a resend of the stale packet"
        );
    }

    /// The remote-failure gate: a channel whose peer failed does not resend.
    #[test]
    fn a_failed_remote_never_triggers_a_resend() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e2),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_is_remote_regular(&channel, false);
        send(&channel);
        UdpChannel::set_remote_failed(&channel);
        clock.set(2_000_001);
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 0);
    }

    /// With nothing unacked, the check is a no-op.
    #[test]
    fn no_unacked_packets_resends_nothing() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e3),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_is_remote_regular(&channel, false);
        clock.set(2_000_001);
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), 0);
    }

    /// One pass resends at most `windowSize_/8` packets (the throttle so a
    /// pathological burst does not re-send the whole window at once).
    #[test]
    fn a_pass_resends_at_most_a_quarter_of_the_window() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00e4),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_is_remote_regular(&channel, false);
        for _ in 0..40 {
            send(&channel);
        }
        // The external RTT opens at 1s, so the resend period is 2s: the
        // packets must be older than that before the age path fires.
        clock.set(3_000_001);
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(
            channel.borrow().num_packets_resent(),
            256 / 8,
            "the pass is capped at windowSize_/8"
        );
        {
            let this = channel.borrow();
            let unacked = |seq| this.unacked_packets().get(seq).as_ref().unwrap();
            assert!(unacked(31).was_resent(), "the 32nd oldest was resent");
            assert!(
                !unacked(32).was_resent(),
                "the pass stopped at the cap, leaving the tail"
            );
        }
    }

    // -- BW-02-03-05: out-of-order and duplicate filtering ---------------

    /// A reliable packet with `seq`, ready to be offered to the receive
    /// window.
    fn packet_with_seq(seq: SeqNum) -> Packet {
        let mut packet = Packet::new();
        packet.set_seq(seq);
        packet
    }

    /// The acceptance for the item: an out-of-order run is buffered, and
    /// the packet that closes the hole delivers the whole run in sequence
    /// -- the reference chains the buffered successors onto the arriving
    /// packet (`Packet::chain`), so the dispatch order is exactly
    /// `inSeqAt_`'s ascending order.
    #[test]
    fn out_of_order_packets_are_delivered_in_order() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f0),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let src = addr(0x0100_00f0);
        let mut stats = PacketReceiverStats::new();

        // Deliver 1, 2, 3 first: each is buffered, nothing dispatches.
        for seq in [1, 2, 3] {
            let (result, ready) =
                UdpChannel::add_to_receive_window(&channel, packet_with_seq(seq), src, &mut stats);
            assert_eq!(result, AddToReceiveWindowResult::PacketIsBufferedInWindow);
            assert!(ready.is_empty(), "a buffered packet is not dispatched");
        }
        assert_eq!(channel.borrow().num_buffered_receives(), 3);

        // The packet closing the hole delivers the whole run, in order.
        let (result, ready) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(0), src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsNextInWindow);
        let seqs: Vec<_> = ready.iter().map(|p| p.seq()).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3], "按原版顺序派发");
        assert_eq!(channel.borrow().in_seq_at(), 4);
        assert_eq!(channel.borrow().num_buffered_receives(), 0);
    }

    /// A packet already below `inSeqAt_` is a duplicate: it is dropped and
    /// counted, and the window does not move.
    #[test]
    fn a_duplicate_below_in_seq_at_is_counted_and_dropped() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f1),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let src = addr(0x0100_00f1);
        let mut stats = PacketReceiverStats::new();

        let (result, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(0), src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsNextInWindow);

        let (result, ready) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(0), src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsDuplicate);
        assert!(ready.is_empty());
        assert_eq!(stats.num_duplicate_packets_received(), 1);
        assert_eq!(channel.borrow().in_seq_at(), 1);
    }

    /// A sequence number with bits above `SEQ_MASK` is corrupt, whatever
    /// else it says.
    #[test]
    fn a_corrupt_sequence_number_is_rejected() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f2),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let src = addr(0x0100_00f2);
        let mut stats = PacketReceiverStats::new();

        // SEQ_MASK is 0x0FFF_FFFF; a seq with a higher bit set fails the
        // seqMask() == seq check.
        let (result, ready) = UdpChannel::add_to_receive_window(
            &channel,
            packet_with_seq(0x1000_0000),
            src,
            &mut stats,
        );
        assert_eq!(result, AddToReceiveWindowResult::PacketIsCorrupt);
        assert!(ready.is_empty());
    }

    /// A sequence number beyond `2 * bufferedReceives_.size()` (or past
    /// `maxWindowSize()`) is dropped without buffering.
    #[test]
    fn a_packet_way_out_of_window_is_dropped() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f3),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let src = addr(0x0100_00f3);
        let mut stats = PacketReceiverStats::new();

        // External: window 256, ring 256, maxWindowSize 256 + 1024 = 1280.
        // Distance 600 is within maxWindowSize but beyond 2 * 256, so the
        // reference drops it -- you can only buffer within twice the
        // current ring.
        let (result, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(600), src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsOutOfWindow);

        // Distance 2000 is beyond maxWindowSize as well.
        let (result, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(2000), src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsOutOfWindow);
        assert_eq!(channel.borrow().num_buffered_receives(), 0);
    }

    /// A packet within twice the ring but past its size doubles the
    /// receive buffer instead of overflowing it.
    #[test]
    fn a_near_packet_doubles_the_receive_buffer() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f4),
            Traits::External,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let src = addr(0x0100_00f4);
        let mut stats = PacketReceiverStats::new();

        // Distance 300 is in (256, 512]: it buffers and doubles the ring.
        let (result, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(300), src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsBufferedInWindow);
        assert_eq!(channel.borrow().buffered_receives().size(), 512);
        assert_eq!(channel.borrow().num_buffered_receives(), 1);
    }

    /// Reliable packets queue an ACK for the next outgoing bundle;
    /// piggyback packets are implicitly ACKed by the containing packet and
    /// must not.
    #[test]
    fn reliable_packets_queue_acks_but_piggybacks_do_not() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f5),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let src = addr(0x0100_00f5);
        let mut stats = PacketReceiverStats::new();

        let (result, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(0), src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsNextInWindow);
        assert!(channel.borrow().acks_to_send().contains(&0));

        let mut piggyback = packet_with_seq(1);
        piggyback.force_piggyback();
        let (result, _) = UdpChannel::add_to_receive_window(&channel, piggyback, src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsNextInWindow);
        assert!(
            !channel.borrow().acks_to_send().contains(&1),
            "a piggyback is implicitly ACKed, never queued"
        );
    }

    /// Without auto-switch, a packet from a different source address is
    /// corrupt.
    #[test]
    fn a_wrong_source_address_is_corrupt_without_auto_switch() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f6),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let wrong_src = addr(0x0200_00f6);
        let mut stats = PacketReceiverStats::new();

        let (result, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(0), wrong_src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsCorrupt);
        assert_eq!(channel.borrow().in_seq_at(), 0);
    }

    /// Auto-switch adopts a new source once its version is usable -- equal
    /// versions count, since the first packet from a cell entity sets the
    /// address and is version 0.
    #[test]
    fn auto_switch_adopts_a_new_source_with_a_usable_version() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f7),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_should_auto_switch_to_src_addr(&channel, true);
        let new_src = addr(0x0200_00f7);
        let mut stats = PacketReceiverStats::new();

        let mut packet = packet_with_seq(0);
        packet.set_channel_version(5);
        let (result, _) = UdpChannel::add_to_receive_window(&channel, packet, new_src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsNextInWindow);
        assert_eq!(channel.borrow().addr(), new_src, "the channel moved");
        assert_eq!(channel.borrow().version(), 5);
    }

    /// Crossing the push threshold parks an ACK-send request on the
    /// channel (the real `send()` call lands with the send path,
    /// BW-02-03-06).
    #[test]
    fn the_push_threshold_requests_an_ack_send() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f8),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        UdpChannel::set_push_unsent_acks_threshold(&channel, 2);
        let src = addr(0x0100_00f8);
        let mut stats = PacketReceiverStats::new();

        let (_, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(0), src, &mut stats);
        assert!(
            !channel.borrow_mut().take_acks_need_push(),
            "one ack is below the threshold"
        );

        let (_, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(1), src, &mut stats);
        assert!(
            channel.borrow_mut().take_acks_need_push(),
            "two acks reach the threshold"
        );
        assert!(
            !channel.borrow_mut().take_acks_need_push(),
            "the request is consumed"
        );
    }

    /// A buffered slot already holding the same sequence is a duplicate:
    /// the reference still reports it as buffered (not duplicate), but the
    /// counter increments and the ring keeps the first copy.
    #[test]
    fn a_repeated_buffered_packet_counts_a_duplicate() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00f9),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        let src = addr(0x0100_00f9);
        let mut stats = PacketReceiverStats::new();

        let (result, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(2), src, &mut stats);
        assert_eq!(result, AddToReceiveWindowResult::PacketIsBufferedInWindow);

        let (result, _) =
            UdpChannel::add_to_receive_window(&channel, packet_with_seq(2), src, &mut stats);
        assert_eq!(
            result,
            AddToReceiveWindowResult::PacketIsBufferedInWindow,
            "the reference returns BUFFERED for a repeated slot"
        );
        assert_eq!(stats.num_duplicate_packets_received(), 1);
        assert_eq!(channel.borrow().num_buffered_receives(), 1);
    }

    // -- BW-02-03-06: fragment reassembly -------------------------------

    /// A reliable fragment of the bundle spanning `[frag_begin, frag_end]`,
    /// with the footers still on: `process_fragment` strips them itself,
    /// like the reference's fragment branch.
    fn fragment_packet(seq: SeqNum, frag_begin: SeqNum, frag_end: SeqNum) -> Packet {
        let mut packet = packet_with_seq(seq);
        packet.enable_flags(flags::IS_FRAGMENT | flags::IS_RELIABLE);
        packet.grow(crate::packet::HEADER_SIZE);
        packet.grow(4); // a body the footers trail
        let buf = packet.buffer_mut();
        buf[crate::packet::HEADER_SIZE + 4..crate::packet::HEADER_SIZE + 8]
            .copy_from_slice(&frag_begin.to_le_bytes());
        buf[crate::packet::HEADER_SIZE + 8..crate::packet::HEADER_SIZE + 12]
            .copy_from_slice(&frag_end.to_le_bytes());
        packet.grow(8);
        // The wired path's `processOrderedPacket` strips the fragment
        // footers (`p->stripFragInfo()`) before the channel's chain logic
        // sees the packet; do the same here so the unit boundary matches.
        assert!(
            packet.strip_frag_info(),
            "fragment_packet: the footers must strip cleanly"
        );
        packet
    }

    /// The acceptance for the item: fragments that arrive out of order are
    /// reassembled in sequence order on the channel, and the channel's
    /// fragment slot is empty again once the bundle hands off.
    #[test]
    fn fragments_arriving_out_of_order_reassemble_on_the_channel() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fa),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // The head anchors the bundle and waits for the rest.
        let result =
            channel
                .borrow_mut()
                .process_fragment(fragment_packet(0, 0, 2), true, "TEST", 0);
        assert!(matches!(result, FragmentResult::Waiting));
        assert!(
            channel
                .borrow()
                .p_fragments()
                .is_some_and(|f| f.chain_length() == 1),
            "the first fragment anchors the channel's bundle"
        );

        // The last fragment arrives next and leaves a hole.
        let result =
            channel
                .borrow_mut()
                .process_fragment(fragment_packet(2, 0, 2), true, "TEST", 1);
        assert!(matches!(result, FragmentResult::Waiting));

        // The hole-filling fragment completes the bundle; the chain hands
        // out in sequence order and the slot is cleared for the next one.
        let result =
            channel
                .borrow_mut()
                .process_fragment(fragment_packet(1, 0, 2), true, "TEST", 2);
        let chain = match result {
            FragmentResult::Complete(chain) => chain,
            FragmentResult::Waiting => panic!("the bundle still waits for fragments"),
            FragmentResult::Corrupt => panic!("a valid fragment was reported corrupt"),
        };
        let seqs: Vec<_> = chain.iter().map(|p| p.seq()).collect();
        assert_eq!(seqs, vec![0, 1, 2], "按原版顺序恢复");
        assert!(
            channel.borrow().p_fragments().is_none(),
            "the completed bundle clears the channel's slot"
        );
    }

    /// A fragment with unusable footers is corrupt before any bundle is
    /// touched (the reference's `Invalid fragment footers` path).
    #[test]
    fn a_fragment_with_bad_footers_is_corrupt() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fb),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // Too short to hold the two footers: the caller's `stripFragInfo`
        // (`processOrderedPacket`, packet_receiver.cpp:1238) is the guard
        // that rejects it before the channel's chain logic ever runs.
        let mut short = packet_with_seq(0);
        short.enable_flags(flags::IS_FRAGMENT | flags::IS_RELIABLE);
        assert!(!short.strip_frag_info(), "the footers do not fit");
        assert!(channel.borrow().p_fragments().is_none());
    }

    /// Channel traffic is ordered, so a bundle that starts mid-way -- the
    /// first fragment the channel ever sees is not the bundle's head -- is
    /// corrupt (the reference's `missing packets before` path).
    #[test]
    fn an_on_channel_bundle_must_start_at_its_head() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fc),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        // Fragment 1 of [0,2]: the head (0) never showed up.
        let result =
            channel
                .borrow_mut()
                .process_fragment(fragment_packet(1, 0, 2), true, "TEST", 0);
        assert!(matches!(result, FragmentResult::Corrupt));
        assert!(
            channel.borrow().p_fragments().is_none(),
            "no bundle is created for a mid-stream start"
        );
    }

    /// A fragment whose footers disagree with the bundle the channel is
    /// already holding is mangled: `add_packet` rejects it, the chain
    /// keeps what it had, and the caller is told the packet is corrupt.
    #[test]
    fn a_mismatched_fragment_is_corrupt_on_the_channel() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = interface(&clock);
        let channel = UdpChannel::new(
            &interface,
            addr(0x0100_00fd),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );

        assert!(matches!(
            channel
                .borrow_mut()
                .process_fragment(fragment_packet(0, 0, 2), true, "TEST", 0),
            FragmentResult::Waiting
        ));

        // A reliable fragment of a different bundle ([1,3]) cannot join.
        assert!(matches!(
            channel
                .borrow_mut()
                .process_fragment(fragment_packet(1, 1, 3), false, "TEST", 1),
            FragmentResult::Corrupt
        ));
        assert!(
            channel
                .borrow()
                .p_fragments()
                .is_some_and(|f| f.chain_length() == 1),
            "the bundle keeps the chain it had"
        );
    }

    /// The off-channel half of the timeout acceptance: bundles too old to
    /// be worth finishing are swept, fresh ones are kept, and the map
    /// ends up holding only the live reassembly.
    #[test]
    fn the_fragment_timeout_sweep_reclaims_stale_bundles() {
        use crate::fragmented_bundle::{
            FragmentedBundle, FragmentedBundles, discard_old_fragmented_bundles,
        };

        let stale = (0, addr(0x0100_00fe));
        let fresh = (5, addr(0x0100_00ff));
        let mut bundles = FragmentedBundles::new();
        bundles.insert(
            stale,
            Rc::new(RefCell::new(FragmentedBundle::new(
                fragment_packet(0, 0, 2),
                0,
            ))),
        );
        bundles.insert(
            fresh,
            Rc::new(RefCell::new(FragmentedBundle::new(
                fragment_packet(5, 5, 7),
                9_000_000,
            ))),
        );

        discard_old_fragmented_bundles(&mut bundles, 11_000_001);
        assert!(!bundles.contains_key(&stale), "the stale one was reclaimed");
        assert!(bundles.contains_key(&fresh), "the fresh one survives");
        assert_eq!(bundles.len(), 1);
    }
}
