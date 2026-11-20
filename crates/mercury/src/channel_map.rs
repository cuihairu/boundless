//! `lib/network/channel_map.cpp` -- the address-keyed registry of
//! non-indexed channels (BW-02-03-02).

use std::collections::HashMap;
use std::rc::Rc;

use crate::endpoint::Address;
use crate::udp_channel::ChannelPtr;

/// `Mercury::ChannelMap` (channel_map.hpp:21): the `udp:`-address to
/// channel table. Indexed channels keep their own registry elsewhere
/// (BW-02-03-05); everything this map holds is looked up by address.
#[derive(Default)]
pub struct ChannelMap {
    map: HashMap<Address, ChannelPtr>,
}

impl ChannelMap {
    /// `ChannelMap()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `ChannelMap::size()`.
    pub fn size(&self) -> usize {
        self.map.len()
    }

    /// `ChannelMap::add( channel )` (channel_map.cpp:19): register a
    /// non-indexed channel under its address. False when the address is
    /// already taken -- the caller is expected to have looked first.
    pub fn add(&mut self, channel: &ChannelPtr) -> bool {
        let addr = channel.borrow().addr();
        assert!(
            !addr.is_none(),
            "ChannelMap::add: MF_ASSERT( channel.addr() != Address::NONE )"
        );

        if self.map.contains_key(&addr) {
            eprintln!(
                "ChannelMap::add: Should not register a channel twice ({})",
                channel.borrow().c_str()
            );
            return false;
        }

        self.map.insert(addr, channel.clone());
        true
    }

    /// `ChannelMap::remove( channel )` (channel_map.cpp:41): drop the
    /// channel's address. False when the address held somebody else --
    /// the map stays untouched so the occupant is not lost.
    pub fn remove(&mut self, channel: &ChannelPtr) -> bool {
        let addr = channel.borrow().addr();
        match self.map.get(&addr) {
            Some(occupant) if Rc::ptr_eq(occupant, channel) => {
                self.map.remove(&addr);
                true
            }
            _ => {
                eprintln!(
                    "ChannelMap::remove: Tried to remove non-existent channel {}",
                    channel.borrow().c_str()
                );
                false
            }
        }
    }

    /// `ChannelMap::find( addr )` (channel_map.hpp:76): the channel at
    /// the address, if any. The creation arm of the reference's
    /// `findChannel` lives on the interface -- it needs the interface to
    /// build and register the new channel, and this map must not hold a
    /// borrow while that re-enters it.
    pub fn find(&self, addr: Address) -> Option<ChannelPtr> {
        self.map.get(&addr).cloned()
    }

    /// Every registered channel, in no particular order -- the scan
    /// `destroyOwnedChannels` runs before its caller destroys.
    pub fn iter(&self) -> impl Iterator<Item = ChannelPtr> + '_ {
        self.map.values().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::NONE;
    use crate::event_dispatcher::EventDispatcher;
    use crate::misc::CHANNEL_ID_NULL;
    use crate::network_interface::InterfaceType;
    use crate::udp_channel::UdpChannel;
    use crate::udp_channel::{DEFAULT_INACTIVITY_RESEND_DELAY, Traits};
    use std::cell::Cell;

    /// An interface on a pinned clock with no socket: the channel
    /// machinery needs only the dispatcher and the maps.
    fn pinned_interface(clock: &Rc<Cell<u64>>) -> Rc<crate::network_interface::NetworkInterface> {
        let clock_cell = Rc::clone(clock);
        let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
            clock_cell.get()
        })));
        crate::network_interface::NetworkInterface::new_with_dispatcher(
            InterfaceType::Internal,
            dispatcher,
        )
    }

    fn addr(ip: u32) -> Address {
        Address::new(ip, platform::socket::htons(20))
    }

    fn make_channel(
        interface: &Rc<crate::network_interface::NetworkInterface>,
        ip: u32,
    ) -> ChannelPtr {
        UdpChannel::new(
            interface,
            addr(ip),
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        )
    }

    /// THE acceptance pair: a duplicate registration refuses, and the
    /// slot a destroyed channel frees serves a fresh channel -- 重复创建
    /// 和旧 Channel 回收吻合.
    #[test]
    fn duplicates_refuse_and_reclaimed_slots_serve_fresh_channels() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = pinned_interface(&clock);
        let address = addr(0x0100_0001);

        let first = make_channel(&interface, 0x0100_0001);
        let mut map = ChannelMap::new();

        assert!(map.add(&first), "the first tenant registers");
        assert_eq!(map.size(), 1);

        // A second channel to the same address cannot take the slot. It
        // is built on a second interface because the first interface's
        // own map would refuse the double registration through
        // `MF_VERIFY` before the standalone map ever saw it.
        let other_interface = pinned_interface(&clock);
        let second = make_channel(&other_interface, 0x0100_0001);
        assert!(!map.add(&second), "a duplicate registration refuses");
        assert_eq!(map.size(), 1, "the tenant is kept");
        assert!(Rc::ptr_eq(&map.find(address).unwrap(), &first));

        // The first tenant leaves; the slot serves a fresh channel. The
        // freeing happens in the live registry -- the interface's own
        // map, not this test's copy of one.
        UdpChannel::destroy(&first);
        assert!(
            interface.find_channel(address, false).is_none(),
            "destroy freed the slot"
        );

        let third = make_channel(&interface, 0x0100_0001);
        assert!(Rc::ptr_eq(
            &interface.find_channel(address, false).unwrap(),
            &third
        ));
    }

    /// `Address::NONE` is a programmer error as a key, like the
    /// reference's `MF_ASSERT( channel.addr() != Address::NONE )`.
    #[test]
    #[should_panic]
    fn adding_an_unaddressed_channel_asserts() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = pinned_interface(&clock);
        let mut map = ChannelMap::new();

        let unaddressed = UdpChannel::new(
            &interface,
            NONE,
            Traits::Internal,
            DEFAULT_INACTIVITY_RESEND_DELAY,
            CHANNEL_ID_NULL,
        );
        map.add(&unaddressed);
    }

    /// Removing a channel that is not the occupant -- or at an address
    /// nobody holds -- refuses and leaves the map alone.
    #[test]
    fn remove_refuses_strangers_and_unknowns() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = pinned_interface(&clock);
        let mut map = ChannelMap::new();

        let tenant = make_channel(&interface, 0x0100_0002);
        assert!(map.add(&tenant));

        // A stranger at the same address: refused, tenant kept. Built on
        // a second interface so the first's own map never has to judge
        // the double registration.
        let other_interface = pinned_interface(&clock);
        let stranger = make_channel(&other_interface, 0x0100_0002);
        assert!(!map.remove(&stranger));
        assert_eq!(map.size(), 1);

        // An address nobody holds.
        let hermit = make_channel(&interface, 0x0100_0003);
        assert!(!map.remove(&hermit));
        assert_eq!(map.size(), 1);

        // The tenant itself: dropped.
        assert!(map.remove(&tenant));
        assert_eq!(map.size(), 0);
    }

    /// `find` answers the occupant and nobody else.
    #[test]
    fn find_answers_the_occupant_only() {
        let clock = Rc::new(Cell::new(1_000_000u64));
        let interface = pinned_interface(&clock);
        let mut map = ChannelMap::new();

        assert!(map.find(addr(0x0100_0004)).is_none());

        let channel = make_channel(&interface, 0x0100_0004);
        assert!(map.add(&channel));
        assert!(Rc::ptr_eq(&map.find(addr(0x0100_0004)).unwrap(), &channel));
        assert!(map.find(addr(0x0100_0005)).is_none());

        // The salt never counts: the same ip/port under a different salt
        // is the same channel.
        let same_ip_port = addr(0x0100_0004);
        let salted = Address {
            ip: same_ip_port.ip,
            port: same_ip_port.port,
            salt: 0xbeef,
        };
        assert!(Rc::ptr_eq(&map.find(salted).unwrap(), &channel));
    }
}
