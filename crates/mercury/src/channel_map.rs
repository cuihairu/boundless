//! `Mercury::ChannelMap` (`lib/network/channel_map.hpp/.cpp`): the map of
//! registered channels, keyed by their destination address.
//!
//! The reference keys by `Address` (a `BW::map`, ordered by
//! `Address::operator<`); the port keys a `BTreeMap` by the same ordering.
//! The interface owns this map and every channel in it.
//!
//! `delAnonymous` and `destroyOwnedChannels` are composed at the interface
//! (`network_interface.rs`) rather than living here: they condemn or
//! destroy channels, which deregisters them through this same map, and the
//! `RefCell` cannot be held across that call.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::endpoint::{Address, NONE};
use crate::udp_channel::UDPChannel;

/// `Mercury::ChannelMap`.
#[derive(Default)]
pub struct ChannelMap {
    /// `map_`: address -> channel.
    map: BTreeMap<Address, Rc<UDPChannel>>,
}

impl ChannelMap {
    /// `ChannelMap()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `add( channel )` (`channel_map.cpp:47`): register a channel, keyed by
    /// its address. A channel already registered at that address is
    /// rejected -- the reference's `IF_NOT_MF_ASSERT_DEV( result.second )`.
    pub fn add(&mut self, channel: &Rc<UDPChannel>) -> bool {
        let addr = channel.addr();
        debug_assert!(addr != NONE, "channel registered with no address");
        match self.map.entry(addr) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Rc::clone(channel));
                true
            }
            std::collections::btree_map::Entry::Occupied(_) => false,
        }
    }

    /// `remove( channel )` (`channel_map.cpp:71`): deregister a channel by
    /// its address. The reference `CRITICAL_MSG`s when it is absent; the
    /// port warns and reports `false` instead of aborting.
    pub fn remove(&mut self, channel: &Rc<UDPChannel>) -> bool {
        let addr = channel.addr();
        debug_assert!(addr != NONE, "channel removed with no address");
        let was_removed = self.map.remove(&addr).is_some();
        if !was_removed {
            eprintln!("ChannelMap::remove: Channel not found {}", addr.c_str());
        }
        was_removed
    }

    /// `find( addr )` (`channel_map.cpp:105`, the lookup half): the channel
    /// registered at `addr`, if any. The anonymous-creation half lives on
    /// the interface (see
    /// [`NetworkInterface::find_or_create_channel`](crate::network_interface::NetworkInterface::find_or_create_channel)),
    /// because creating one registers through this same map.
    pub fn find(&self, addr: &Address) -> Option<Rc<UDPChannel>> {
        if addr.ip == 0 {
            return None;
        }
        self.map.get(addr).cloned()
    }

    /// The channels the interface owns (anonymous or condemned), as
    /// [`UDPChannel::is_owned_by_interface`] says. `destroyOwnedChannels`
    /// drives these.
    pub fn owned_channels(&self) -> Vec<Rc<UDPChannel>> {
        self.map
            .values()
            .filter(|channel| channel.is_owned_by_interface())
            .cloned()
            .collect()
    }

    /// The `destroyOwnedChannels` warning half: the channels still
    /// registered that the interface does not own.
    pub fn unowned_channels(&self) -> Vec<(Address, Rc<UDPChannel>)> {
        self.map
            .iter()
            .filter(|(_addr, channel)| !channel.is_owned_by_interface())
            .map(|(addr, channel)| (*addr, Rc::clone(channel)))
            .collect()
    }

    /// `hasUnackedPackets()` (`channel_map.cpp:179`): any registered channel
    /// has outstanding packets.
    pub fn has_unacked_packets(&self) -> bool {
        self.map
            .values()
            .any(|channel| channel.has_unacked_packets())
    }

    /// How many channels are registered (the tests' window on the map).
    pub fn count(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::CHANNEL_ID_NULL;
    use crate::network_interface::{InterfaceType, NetworkInterface};
    use crate::udp_channel::{ChannelTraits, UDPChannel};

    const LOOPBACK: u32 = 0x0100_007f;

    fn address(port: u16) -> Address {
        Address::new(LOOPBACK, port)
    }

    /// A loopback interface and a channel registered at the given port.
    fn channel_at(port: u16) -> (Rc<NetworkInterface>, Rc<UDPChannel>) {
        let (interface, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
        let channel = UDPChannel::new(
            &interface,
            address(port),
            ChannelTraits::Internal,
            CHANNEL_ID_NULL,
        );
        (interface, channel)
    }

    /// BW-02-03-02: registering a channel twice for the same address is
    /// rejected, and the first registration is the one the map returns.
    #[test]
    fn a_duplicate_registration_is_rejected() {
        let mut map = ChannelMap::new();
        let (_interface, channel) = channel_at(1000);
        let (_interface2, duplicate) = channel_at(1000);

        assert!(map.add(&channel), "the first registration succeeds");
        assert!(
            !map.add(&duplicate),
            "a second channel at the same address is rejected"
        );
        assert_eq!(map.find(&address(1000)).unwrap().addr(), address(1000));
        assert_eq!(map.count(), 1, "the duplicate did not displace the first");
    }

    /// Removing a channel drops it from the map; removing one that is not
    /// there reports `false` and warns.
    #[test]
    fn removing_a_channel_drops_it() {
        let mut map = ChannelMap::new();
        let (_interface, channel) = channel_at(1001);
        assert!(map.add(&channel));

        assert!(map.remove(&channel));
        assert!(map.find(&address(1001)).is_none());

        let (_interface2, stranger) = channel_at(9999);
        assert!(!map.remove(&stranger), "removing an absent channel fails");
    }

    /// An address with no ip is never found (`channel_map.cpp:110`).
    #[test]
    fn an_address_with_no_ip_is_never_found() {
        let mut map = ChannelMap::new();
        let (_interface, channel) = channel_at(1003);
        assert!(map.add(&channel));

        assert!(map.find(&NONE).is_none());
        assert!(map.find(&address(1003)).is_some());
    }
}
