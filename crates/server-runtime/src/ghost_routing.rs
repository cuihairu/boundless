//! BW-08-04-03: tracking the real address from a ghost.
//!
//! Ported from the `pRealChannel_` / `nextRealAddr_` lifecycle in
//! `server/cellapp`:
//!
//! - `Entity::convertRealToGhost` (`entity.cpp:2015-2050`): offloading
//!   points the channel at the destination and records it as the
//!   `nextRealAddr_` until the onload confirms.
//! - `RealEntity::readOffloadData` (`real_entity.cpp:345-400`): the newly
//!   loaded real notifies every haunt with `ghostSetReal`, skipping its
//!   own address, the bad-haunt address and dead channels.
//! - `Entity::ghostSetReal` (`entity.cpp:4927-4955`): the routing update
//!   proper - a subsequence check on `numTimesRealOffloaded`, then the
//!   channel is retargeted to the new owner and the pending address
//!   cleared.
//! - `Entity::checkIfZombied` (`entity.cpp:3304-3343`): when the current
//!   real's CellApp dies mid-offload, the pending address recovers the
//!   route.
//! - `Entity::realAddr` / `isOffloadingTo` (`entity.cpp:3286-3296` /
//!   `1950-1961`) and the `DISPOSITION_FORWARD_TO_REAL` lookup
//!   (`message_handlers.cpp:197-202`), plus the population channel cache
//!   `rememberRealChannel` / `findRealChannel` (`entity.cpp:2960-2962`)
//!   that keeps forwarding working for destroyed entities.
//!
//! The reference uses the `Address::NONE` sentinel for "no pending real";
//! that is an `Option` here. `relocated()` is a caller callback - the
//! retarget outcome signals it.

use std::collections::{HashMap, HashSet};

/// A `CellAppChannel` as routing needs it: the address and whether it is
/// alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingChannel {
    pub addr: u64,
    pub good: bool,
}

/// The `CellAppChannels` table plus the per-entity real-channel cache.
#[derive(Debug, Default)]
pub struct RealAddressBook {
    channels: HashMap<u64, RoutingChannel>,
    recently_died: HashSet<u64>,
    entity_channels: HashMap<u32, u64>,
}

impl RealAddressBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a channel's health (`CellAppChannels` bookkeeping).
    pub fn add_channel(&mut self, addr: u64, good: bool) {
        self.channels.insert(addr, RoutingChannel { addr, good });
    }

    /// `CellAppChannels::get`: fetch the channel, creating a good one if
    /// absent (`entity.cpp:1778`, `3339`).
    pub fn get_or_create(&mut self, addr: u64) -> RoutingChannel {
        *self
            .channels
            .entry(addr)
            .or_insert(RoutingChannel { addr, good: true })
    }

    /// A CellApp death: the channel dies and the address is remembered as
    /// recently dead for the zombie checks.
    pub fn mark_dead(&mut self, addr: u64) {
        self.add_channel(addr, false);
        self.recently_died.insert(addr);
    }

    /// `CellAppChannels::hasRecentlyDied`.
    pub fn has_recently_died(&self, addr: u64) -> bool {
        self.recently_died.contains(&addr)
    }

    /// `EntityPopulation::rememberRealChannel` (`entity.cpp:2960-2962`).
    pub fn remember_real_channel(&mut self, entity_id: u32, addr: u64) {
        self.entity_channels.insert(entity_id, addr);
    }

    /// `EntityPopulation::findRealChannel`.
    pub fn find_real_channel(&self, entity_id: u32) -> Option<u64> {
        self.entity_channels.get(&entity_id).copied()
    }
}

/// What `ghostSetReal` did (`entity.cpp:4927-4955`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhostSetRealOutcome {
    /// The subsequence matched: the route moved to the new owner and
    /// `relocated()` follows.
    Retargeted,
    /// Invalid subsequence id: the reference buffers the message
    /// (`delaySubsequence`) for replay once the gap fills; the route is
    /// untouched.
    Delayed,
}

/// The real-address tracking state of one entity: `pRealChannel_`,
/// `nextRealAddr_` and `numTimesRealOffloaded_`. A real entity carries no
/// channel (`entity.ipp:113`: `pReal_` and `pRealChannel_` are mutually
/// exclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GhostRouting {
    is_real: bool,
    real_channel: Option<RoutingChannel>,
    next_real_addr: Option<u64>,
    num_times_real_offloaded: u32,
}

/// `readOffloadData`'s notification sweep (`real_entity.cpp:345-397`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyOutcome {
    /// The addresses that received `ghostSetReal`.
    pub notified: Vec<u64>,
    /// True when the haunt list contained our own address (the local
    /// ghost is handled in place).
    pub are_we_haunted: bool,
}

impl GhostRouting {
    /// A freshly created real: it is its own real.
    pub fn real() -> Self {
        GhostRouting {
            is_real: true,
            real_channel: None,
            next_real_addr: None,
            num_times_real_offloaded: 0,
        }
    }

    /// A ghost created from a snapshot: the channel comes from the
    /// snapshot's real address (`entity.cpp:1778`).
    pub fn new_ghost(
        book: &mut RealAddressBook,
        real_addr: u64,
        num_times_real_offloaded: u32,
    ) -> Self {
        GhostRouting {
            is_real: false,
            real_channel: Some(book.get_or_create(real_addr)),
            next_real_addr: None,
            num_times_real_offloaded,
        }
    }

    /// `Entity::realAddr` (`entity.cpp:3286-3296`): the channel's address,
    /// or our own for a real.
    pub fn real_addr(&self, own_addr: u64) -> u64 {
        match self.real_channel {
            Some(channel) => channel.addr,
            None => own_addr,
        }
    }

    /// The `DISPOSITION_FORWARD_TO_REAL` target
    /// (`message_handlers.cpp:197-202`): where a message for the real
    /// should be sent. `None` on a real - the message processes locally.
    pub fn forward_to_real(&self) -> Option<u64> {
        self.real_channel.filter(|_| !self.is_real).map(|c| c.addr)
    }

    /// `Entity::isOffloadingTo` (`entity.cpp:1950-1961`): true when we
    /// offloaded to `addr` and are awaiting its confirmation.
    pub fn is_offloading_to(&self, addr: u64, own_addr: u64) -> bool {
        self.next_real_addr == Some(addr) && self.real_addr(own_addr) == addr
    }

    /// `Entity::convertRealToGhost` routing half (`entity.cpp:2036-2040`):
    /// point the channel at the destination and record it as pending.
    pub fn convert_real_to_ghost(&mut self, book: &mut RealAddressBook, dest_addr: u64) {
        debug_assert!(self.is_real);
        debug_assert!(self.real_channel.is_none());
        let channel = book.get_or_create(dest_addr);
        self.real_channel = Some(channel);
        // Once the real is created on the other CellApp, it will send a
        // ghostSetReal back, so we better be ready for it.
        self.next_real_addr = Some(channel.addr);
        self.is_real = false;
    }

    /// `Entity::ghostSetNextReal` (`entity.cpp:4960-4977`): the current
    /// real's sign-off before offloading.
    pub fn ghost_set_next_real(&mut self, next_addr: u64) {
        debug_assert!(self.next_real_addr.is_none());
        self.next_real_addr = Some(next_addr);
    }

    /// `Entity::ghostSetReal` (`entity.cpp:4927-4955`): the routing
    /// update. The subsequence must be exactly one past ours, else the
    /// message is delayed.
    pub fn ghost_set_real(
        &mut self,
        book: &mut RealAddressBook,
        owner: u64,
        num_times_real_offloaded: u32,
    ) -> GhostSetRealOutcome {
        let expected = self.num_times_real_offloaded + 1;
        if num_times_real_offloaded != expected {
            return GhostSetRealOutcome::Delayed;
        }

        self.num_times_real_offloaded = num_times_real_offloaded;
        debug_assert!(!self.is_real);
        debug_assert!(self.next_real_addr == Some(owner));

        self.real_channel = Some(book.get_or_create(owner));
        self.next_real_addr = None;
        GhostSetRealOutcome::Retargeted
    }

    /// `Entity::checkIfZombied` (`entity.cpp:3304-3343`). True when the
    /// entity is (or will be) zombied; a mid-offload death recovers the
    /// route here.
    pub fn check_if_zombied(
        &mut self,
        book: &mut RealAddressBook,
        dying_addr: u64,
        destroyed: bool,
    ) -> bool {
        if destroyed || self.is_real {
            return false;
        }

        // Zombied if the next real is dying or already dead.
        if self
            .next_real_addr
            .is_some_and(|next| next == dying_addr || book.has_recently_died(next))
        {
            return true;
        }

        // Check if the current real is healthy.
        if self
            .real_channel
            .is_some_and(|channel| channel.addr != dying_addr)
        {
            return false;
        }

        // Recovering a zombie by updating the current real to the next
        // real.
        if let Some(next) = self.next_real_addr {
            self.real_channel = Some(book.get_or_create(next));
            return false;
        }

        true
    }
}

/// `RealEntity::readOffloadData` notification sweep
/// (`real_entity.cpp:345-397`): the freshly loaded real tells every haunt
/// where it now lives. Own address, the bad-haunt address (a failed
/// teleport) and dead channels get no message. The space-change and
/// teleport variants send different messages and are not modelled.
pub fn notify_haunts_of_real(
    book: &mut RealAddressBook,
    haunts: &[u64],
    own_addr: u64,
    bad_haunt: Option<u64>,
) -> NotifyOutcome {
    let mut outcome = NotifyOutcome {
        notified: Vec::new(),
        are_we_haunted: false,
    };
    for addr in haunts {
        // We don't care if it's our own address - it could be from a
        // local teleport or some such.
        if *addr == own_addr {
            outcome.are_we_haunted = true;
            continue;
        }
        // Skip the bad haunt address - onload from a failed teleport back
        // to the originating cellapp, no ghost was created there.
        if bad_haunt == Some(*addr) {
            continue;
        }
        let channel = book.get_or_create(*addr);
        if !channel.good {
            // Not notifying ghost on a dead app.
            continue;
        }
        outcome.notified.push(*addr);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u64 = 0x11;
    const B: u64 = 0x22;
    const C: u64 = 0x33;

    fn book_with(a: u64) -> RealAddressBook {
        let mut book = RealAddressBook::new();
        book.add_channel(a, true);
        book
    }

    #[test]
    fn a_real_is_its_own_real() {
        let mut book = book_with(A);
        let mut real = GhostRouting::real();
        assert_eq!(real.real_addr(A), A);
        assert_eq!(real.forward_to_real(), None, "reals process locally");
        assert!(!real.check_if_zombied(&mut book, A, false));
    }

    #[test]
    fn offload_points_the_channel_at_the_destination() {
        let mut book = book_with(A);
        let mut entity = GhostRouting::real();
        entity.convert_real_to_ghost(&mut book, B);
        assert_eq!(entity.forward_to_real(), Some(B));
        assert!(entity.is_offloading_to(B, A));
        assert!(!entity.is_offloading_to(A, A));
        assert!(!entity.is_offloading_to(C, A));
    }

    #[test]
    fn an_in_order_ghost_set_real_moves_the_route() {
        let mut book = book_with(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 2);
        ghost.ghost_set_next_real(B);
        assert_eq!(ghost.forward_to_real(), Some(A), "still the old real");

        assert_eq!(
            ghost.ghost_set_real(&mut book, B, 3),
            GhostSetRealOutcome::Retargeted
        );
        assert_eq!(ghost.forward_to_real(), Some(B), "the route moved");
        assert_eq!(ghost.real_addr(C), B);
        assert!(!ghost.is_offloading_to(B, C));
    }

    #[test]
    fn an_out_of_order_ghost_set_real_is_delayed_not_applied() {
        let mut book = book_with(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 2);
        ghost.ghost_set_next_real(B);

        assert_eq!(
            ghost.ghost_set_real(&mut book, B, 5),
            GhostSetRealOutcome::Delayed
        );
        assert_eq!(ghost.forward_to_real(), Some(A), "the route is untouched");
        assert_eq!(
            ghost.ghost_set_real(&mut book, B, 2),
            GhostSetRealOutcome::Delayed
        );
        assert_eq!(ghost.forward_to_real(), Some(A));

        // The gap fills; the buffered message replays cleanly.
        assert_eq!(
            ghost.ghost_set_real(&mut book, B, 3),
            GhostSetRealOutcome::Retargeted
        );
        // A second hop signs off with its own ghostSetNextReal first.
        ghost.ghost_set_next_real(C);
        assert_eq!(
            ghost.ghost_set_real(&mut book, C, 4),
            GhostSetRealOutcome::Retargeted
        );
        assert_eq!(ghost.forward_to_real(), Some(C));
    }

    #[test]
    fn a_mid_offload_death_recovers_the_route_to_the_next_real() {
        let mut book = book_with(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 1);
        ghost.ghost_set_next_real(B);
        // The old real's CellApp dies before the confirmation arrives.
        book.mark_dead(A);
        assert!(!ghost.check_if_zombied(&mut book, A, false));
        assert_eq!(
            ghost.forward_to_real(),
            Some(B),
            "the route recovered to the next real"
        );
    }

    #[test]
    fn a_next_real_that_is_dying_is_a_zombie() {
        let mut book = book_with(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 1);
        ghost.ghost_set_next_real(B);
        assert!(ghost.check_if_zombied(&mut book, B, false));
    }

    #[test]
    fn a_recently_dead_next_real_is_a_zombie() {
        let mut book = book_with(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 1);
        ghost.ghost_set_next_real(B);
        book.mark_dead(B);
        // The CellApp that just died is not B, but B died recently too.
        assert!(ghost.check_if_zombied(&mut book, C, false));
    }

    #[test]
    fn a_healthy_real_is_not_a_zombie() {
        let mut book = book_with(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 1);
        book.add_channel(C, true);
        assert!(!ghost.check_if_zombied(&mut book, C, false));
    }

    #[test]
    fn a_bare_ghost_of_a_dying_real_is_a_zombie() {
        let mut book = book_with(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 1);
        assert!(ghost.check_if_zombied(&mut book, A, false));
    }

    #[test]
    fn a_destroyed_entity_is_never_a_zombie() {
        let mut book = book_with(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 1);
        assert!(!ghost.check_if_zombied(&mut book, A, true));
    }

    #[test]
    fn a_ghost_of_a_dead_snapshot_address_keeps_the_dead_channel() {
        let mut book = book_with(A);
        book.mark_dead(A);
        let mut ghost = GhostRouting::new_ghost(&mut book, A, 0);
        assert_eq!(ghost.forward_to_real(), Some(A));
        // The snapshot address is the dying one and there is no pending
        // real: a zombie.
        assert!(ghost.check_if_zombied(&mut book, A, false));
    }

    #[test]
    fn the_real_notifies_every_live_haunt_but_three() {
        let mut book = book_with(A);
        book.add_channel(B, true);
        book.add_channel(C, true);
        book.mark_dead(0x44);
        let outcome = notify_haunts_of_real(&mut book, &[A, 0x44, 0x55, B, C], A, Some(0x55));
        assert_eq!(outcome.notified, vec![B, C]);
        assert!(outcome.are_we_haunted);
        // Without the failed teleport the bad haunt is notified too.
        let outcome = notify_haunts_of_real(&mut book, &[0x55, B], A, None);
        assert_eq!(outcome.notified, vec![0x55, B]);
        assert!(!outcome.are_we_haunted);
    }

    #[test]
    fn the_channel_cache_survives_the_entity() {
        let mut book = book_with(A);
        // Destroyed entities keep forwarding through the population cache
        // (entity.cpp:2960).
        book.remember_real_channel(7, A);
        assert_eq!(book.find_real_channel(7), Some(A));
        assert_eq!(book.find_real_channel(8), None);
    }
}
