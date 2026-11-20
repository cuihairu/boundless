//! `lib/network/udp_channel.cpp` -- the registration-and-lifecycle slice
//! of `Mercury::UDPChannel` (BW-02-03-02): construction registers a
//! non-indexed channel with its interface, `condemn` hands it to the
//! condemned list, and `destroy` closes it for good. The sequence-window
//! state (BW-02-03-03/05) and the resend timers (BW-02-03-04) carry their
//! fields from the reference constructor but stay inert until their
//! tasks land.
//!
//! The reference's `Channel` base (`channel.hpp`) is folded in: its state
//! is the leading fields and its `destroy`/inactivity flow is the
//! `destroy`/`start_inactivity_detection` pair here.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use foundation::time_queue::{TimerHandle, TimerHandler};

use crate::channel_listener::ChannelListener;
use crate::endpoint::{Address, NONE};
use crate::misc::{CHANNEL_ID_NULL, ChannelID, ChannelVersion, SEQ_NULL, SeqNum, seq_mask};
use crate::network_interface::{ChannelRef, NetworkInterface};
use crate::udp_bundle::UdpBundle;

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
            window_size: window_size(traits, id),
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
    /// unset until BW-02-03-05 moves it.
    pub fn unreliable_in_seq_at(&self) -> SeqNum {
        self.unreliable_in_seq_at
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

    /// A test hook driving `oldestUnackedSeq_` until the send window
    /// (BW-02-03-03) manages it for real.
    #[cfg(test)]
    pub fn force_unacked(&mut self, seq: SeqNum) {
        self.oldest_unacked_seq = seq;
    }

    /// A test hook ageing `lastReceivedTime_` for the condemned sweep.
    #[cfg(test)]
    pub fn force_last_received_time(&mut self, micros: u64) {
        self.last_received_time = micros;
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
}
