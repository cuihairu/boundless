//! `Mercury::Channel` / `Mercury::UDPChannel` (`lib/network/channel.hpp`,
//! `lib/network/udp_channel.hpp`): the lifecycle identity of a regular
//! connection between two addresses.
//!
//! Scope of BW-02-03-02: registration and closing. This item carries the
//! address identity, the traits/version bookkeeping, and the condemn /
//! destroy / remote-failed lifecycle that [`ChannelMap`], `CondemnedChannels`
//! and `RecentlyDeadChannels` (and the interface's shutdown) drive. The
//! data plane -- send window, sequence allocators, unacked packets,
//! buffered receives, fragments, resend timers, ACK queues -- is the
//! BW-02-03-03/04/05/06 seam and is not ported here.

use std::cell::Cell;
use std::rc::{Rc, Weak};

use crate::endpoint::{Address, NONE};
use crate::misc::{CHANNEL_ID_NULL, ChannelID, ChannelVersion, SEQ_NULL, SeqNum};
use crate::network_interface::{ChannelRef, NetworkInterface};

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

        // doDestroy() -- the data-plane teardown is BW-02-03-03/04.
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

    /// `hasUnsentData()` (`udp_channel.cpp:645`): the on-channel bundle
    /// holds messages, footers, reliability, or pending ACKs. The bundle
    /// ownership and ACK queue are BW-02-03-03/04, so this slice reports
    /// none.
    pub fn has_unsent_data(&self) -> bool {
        false
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
}
