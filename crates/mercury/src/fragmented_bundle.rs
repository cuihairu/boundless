//! `lib/network/fragmented_bundle.cpp` -- `Mercury::FragmentedBundle`
//! (BW-02-03-06): a bundle too big for one packet arrives as fragments, and
//! this is the partially reassembled chain that holds them until the last
//! one lands. Fragments file into the chain in sequence order regardless of
//! arrival order; once nothing is missing the chain hands back as the
//! complete bundle. An untouched bundle past [`MAX_AGE`] is abandoned -- the
//! receive dispatch drops stale overlapping bundles rather than let a lost
//! fragment pin memory forever.
//!
//! The reference times the bundle against the global `timestamp()`; the
//! port injects the dispatcher's stamp into the constructor, `add_packet`
//! and the age probes, so a bundle carries no clock of its own. The
//! `addToStream`/`createFromStream` serialization half arrives with the
//! channel stream work (BW-02-03-07), and the receive dispatch that drives
//! this type (`processOrderedPacket`'s fragment arm) with it too.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::endpoint::Address;
use crate::event_dispatcher::STAMPS_PER_SECOND;
use crate::misc::seq_less_than;
use crate::packet::{Packet, flags};

/// `FragmentedBundle::MAX_AGE` (fragmented_bundle.hpp): the age in seconds
/// at which a fragmented bundle is abandoned.
pub const MAX_AGE: u64 = 10;

/// `MAX_AGE` in this dispatcher's microsecond stamps.
pub const MAX_AGE_STAMPS: u64 = MAX_AGE * STAMPS_PER_SECOND;

/// `FragmentedBundlePtr`: a bundle the receive dispatch and its channel can
/// both hold.
pub type FragmentedBundlePtr = Rc<RefCell<FragmentedBundle>>;

/// A partially reassembled multi-packet bundle.
pub struct FragmentedBundle {
    /// The bundle's `fragBegin`, fixed by the first fragment seen.
    first_fragment: u32,
    /// `lastFragment_`: the bundle's `fragEnd`.
    last_fragment: u32,
    /// `remaining_`: fragments still missing ("remaining is one less than
    /// the size" when the first fragment lands, which is itself one of them).
    remaining: u32,
    /// `touched_`: the last stamp a fragment arrived.
    touched: u64,
    /// `pChain_`: the fragments seen so far, ordered by sequence.
    chain: Option<Rc<Packet>>,
}

impl FragmentedBundle {
    /// `FragmentedBundle( pFirstPacket )`: the first fragment fixes the
    /// bundle's span and counts itself as arrived.
    pub fn new(first: Rc<Packet>, now: u64) -> Self {
        let first_fragment = first.frag_begin();
        let last_fragment = first.frag_end();
        Self {
            first_fragment,
            last_fragment,
            // Remaining is one less than the size.
            remaining: last_fragment.wrapping_sub(first_fragment),
            touched: now,
            chain: Some(first),
        }
    }

    /// `FragmentedBundle::addPacket( p, isExternal, sourceStr )`: file a
    /// fragment into the chain, keeping it sorted by sequence; say whether
    /// the bundle is still being assembled. `false` means the packet's
    /// fragment footers are mangled and the receive dispatch should treat
    /// the packet as corrupted.
    pub fn add_packet(&mut self, p: Rc<Packet>, is_external: bool, source: &str, now: u64) -> bool {
        // If the last fragment seqnums for existing packets in this bundle
        // and this one don't match up, we can't process it.
        if self.first_fragment != p.frag_begin() || self.last_fragment != p.frag_end() {
            // If the incoming fragment is unreliable, then we're still
            // waiting for the reliable bundle attached to this to complete.
            if !p.has_flags(flags::IS_RELIABLE) {
                eprintln!(
                    "FragmentedBundle::addPacket( {source} ): Discarding unreliable fragment \
                     #{} (#{},{}) while waiting for reliable chain (#{},{}) to complete",
                    p.seq(),
                    p.frag_begin(),
                    p.frag_end(),
                    self.first_fragment,
                    self.last_fragment
                );
                return true;
            }

            // On an external interface it could be someone mangling the
            // packets on purpose; internal traffic should never do this.
            eprintln!(
                "FragmentedBundle::addPacket( {source} ): {}Mangled fragment footers on packet \
                 #{}, got [{},{}], expecting [{},{}]",
                if is_external { "" } else { "CRITICAL: " },
                p.seq(),
                p.frag_begin(),
                p.frag_end(),
                self.first_fragment,
                self.last_fragment
            );
            return false;
        }

        self.touched = now;

        // Find where this goes in the chain, by sequence. The reference
        // splices raw pointers through `next_`; an `Rc` graph cannot be
        // walked that way -- a cursor into it would borrow the very packet
        // it reassigns itself through. So the chain is dismantled into
        // sequence order and rebuilt, which is what `addPacket` wants the
        // result to look like anyway.
        let mut head = self.chain.take();
        let mut ordered: Vec<Rc<Packet>> = Vec::new();
        while let Some(mut link) = head.take() {
            // If p is already in this chain, bail now: the bundle keeps the
            // fragment it has and the retransmit is dropped whole.
            if link.seq() == p.seq() {
                take_chain(&mut link, &mut head);
                ordered.push(link);
                take_rest(&mut head, &mut ordered);
                self.relink(ordered);
                eprintln!(
                    "FragmentedBundle::addPacket( {source} ): Discarding duplicate fragment #{}",
                    p.seq()
                );
                return true;
            }

            // Stop when this link is the one in the chain after p: p takes
            // its place in the run, and relink rebuilds the chain.
            if seq_less_than(p.seq(), link.seq()) {
                take_chain(&mut link, &mut head);
                ordered.push(p);
                ordered.push(link);
                take_rest(&mut head, &mut ordered);
                self.relink(ordered);
                self.remaining -= 1;
                return true;
            }

            // This link stays ahead of p: carry it over and descend.
            take_chain(&mut link, &mut head);
            ordered.push(link);
        }

        // The chain ran out ahead of p: p is the new tail.
        ordered.push(p);
        self.relink(ordered);
        self.remaining -= 1;
        true
    }

    /// Rebuild the chain from the fragments collected in sequence order.
    fn relink(&mut self, ordered: Vec<Rc<Packet>>) {
        let mut chain: Option<Rc<Packet>> = None;
        for mut link in ordered.into_iter().rev() {
            *Rc::get_mut(&mut link)
                .expect("chain: every link is the sole owner of its packet")
                .chain_next_slot_mut() = chain;
            chain = Some(link);
        }
        self.chain = chain;
    }

    /// `remaining_`.
    pub fn remaining(&self) -> u32 {
        self.remaining
    }

    /// `age()`: seconds since the last fragment arrived.
    pub fn age(&self, now: u64) -> f64 {
        now.saturating_sub(self.touched) as f64 / STAMPS_PER_SECOND as f64
    }

    /// `isOld()`: untouched for longer than [`MAX_AGE`] seconds.
    pub fn is_old(&self, now: u64) -> bool {
        now.saturating_sub(self.touched) > MAX_AGE_STAMPS
    }

    /// `isReliable()`: whether the chain's first fragment carried the
    /// reliable flag.
    pub fn is_reliable(&self) -> bool {
        self.chain
            .as_ref()
            .expect("the bundle holds its first fragment")
            .has_flags(flags::IS_RELIABLE)
    }

    /// `chainLength()`: the fragments seen so far.
    pub fn chain_length(&self) -> usize {
        let mut length = 0;
        let mut cursor = self.chain.clone();
        while let Some(link) = cursor {
            length += 1;
            cursor = link.chain_next();
        }
        length
    }

    /// `pChain()`: the head of the chain, for dispatching a completed
    /// bundle. `None` only before the first fragment lands, which the
    /// constructor makes impossible.
    pub fn chain(&self) -> Option<Rc<Packet>> {
        self.chain.clone()
    }

    /// `lastFragment()`.
    pub fn last_fragment(&self) -> u32 {
        self.last_fragment
    }

    /// `isComplete()`.
    pub fn is_complete(&self) -> bool {
        self.remaining == 0
    }
}

/// Move `link`'s successor out of its slot into `head`, leaving the link's
/// chain link empty. This is how the chain is dismantled: taking each link
/// out of its predecessor keeps every `Rc` at refcount one.
fn take_chain(link: &mut Rc<Packet>, head: &mut Option<Rc<Packet>>) {
    *head = Rc::get_mut(link)
        .expect("chain: every link is the sole owner of its packet")
        .chain_next_slot_mut()
        .take();
}

/// Collect whatever `head` still points at into `ordered`, in chain order.
fn take_rest(head: &mut Option<Rc<Packet>>, ordered: &mut Vec<Rc<Packet>>) {
    while let Some(mut link) = head.take() {
        let next = Rc::get_mut(&mut link)
            .expect("chain: every link is the sole owner of its packet")
            .chain_next_slot_mut()
            .take();
        ordered.push(link);
        *head = next;
    }
}

/// `FragmentedBundle::Key` (fragmented_bundle.hpp): the off-channel bundle
/// key -- the sender's address and the bundle's first fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    /// `addr_`.
    pub addr: Address,
    /// `firstFragment_`.
    pub first_fragment: u32,
}

/// `FragmentedBundles` (fragmented_bundle.hpp): the partially reassembled
/// packets received off-channel. Channels maintain their own
/// [`FragmentedBundle`] instead. Ordered by first fragment, then address,
/// as the reference's `operator<` does.
pub type FragmentedBundles = BTreeMap<Key, FragmentedBundlePtr>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A fragment packet with a sequence and a `[begin,end]` span.
    fn fragment(seq: u32, begin: u32, end: u32, reliable: bool) -> Rc<Packet> {
        let mut packet = Packet::new();
        packet.set_seq(seq);
        packet.set_frag_info(begin, end);
        if reliable {
            packet.enable_flags(flags::IS_RELIABLE);
        }
        Rc::new(packet)
    }

    /// The bundle under test: fragments [0,5], the first one reliable, at
    /// stamp zero.
    fn bundle() -> FragmentedBundle {
        FragmentedBundle::new(fragment(0, 0, 5, true), 0)
    }

    /// Walk the chain and collect the sequences in dispatch order.
    fn chain_seq(bundle: &FragmentedBundle) -> Vec<u32> {
        let mut seen = Vec::new();
        let mut cursor = bundle.chain();
        while let Some(link) = cursor {
            seen.push(link.seq());
            cursor = link.chain_next();
        }
        seen
    }

    /// A bundle spanning [0,5] starts with one fragment in and five to go:
    /// "remaining is one less than the size", the size being all six.
    #[test]
    fn the_first_fragment_fixes_the_span() {
        let bundle = bundle();
        assert_eq!(bundle.last_fragment(), 5);
        assert_eq!(bundle.chain_length(), 1);
        assert_eq!(bundle.remaining(), 5);
        assert!(!bundle.is_complete());
        assert!(bundle.is_reliable(), "the first fragment was reliable");
        assert_eq!(bundle.age(1_000_000), 1.0, "one second of silence");
    }

    /// BW-02-03-06 acceptance, first half: fragments arriving out of order
    /// still come back out in sequence order, and the bundle only completes
    /// when the missing one lands.
    #[test]
    fn out_of_order_fragments_reassemble_in_sequence_order() {
        let mut bundle = bundle();

        // 2 then 4 land out of order: both file in, neither completes.
        assert!(bundle.add_packet(fragment(2, 0, 5, true), false, "TEST", 1));
        assert!(!bundle.is_complete());
        assert_eq!(chain_seq(&bundle), vec![0, 2]);
        assert_eq!(bundle.remaining(), 4);

        assert!(bundle.add_packet(fragment(4, 0, 5, true), false, "TEST", 2));
        assert!(!bundle.is_complete());
        assert_eq!(chain_seq(&bundle), vec![0, 2, 4], "the chain stays sorted");
        assert_eq!(bundle.remaining(), 3);

        // 1 slots between 0 and 2.
        assert!(bundle.add_packet(fragment(1, 0, 5, true), false, "TEST", 3));
        assert_eq!(chain_seq(&bundle), vec![0, 1, 2, 4]);
        assert_eq!(bundle.remaining(), 2);

        // 3 slots between 2 and 4, 5 closes the span: complete, and the
        // whole chain dispatches in order.
        assert!(bundle.add_packet(fragment(3, 0, 5, true), false, "TEST", 4));
        assert_eq!(chain_seq(&bundle), vec![0, 1, 2, 3, 4]);
        assert_eq!(bundle.remaining(), 1);

        assert!(bundle.add_packet(fragment(5, 0, 5, true), false, "TEST", 5));
        assert!(bundle.is_complete());
        assert_eq!(chain_seq(&bundle), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(bundle.remaining(), 0);
    }

    /// A retransmit of a fragment already held is dropped whole: the chain
    /// and the missing count do not move.
    #[test]
    fn a_duplicate_fragment_changes_nothing() {
        let mut bundle = bundle();
        assert!(bundle.add_packet(fragment(3, 0, 5, true), false, "TEST", 1));
        assert_eq!(bundle.remaining(), 4);

        assert!(bundle.add_packet(fragment(3, 0, 5, true), false, "TEST", 2));
        assert_eq!(bundle.remaining(), 4, "the duplicate was not counted");
        assert_eq!(chain_seq(&bundle), vec![0, 3]);
        assert_eq!(
            bundle.touched, 2,
            "a duplicate still touches the bundle, so it stays alive"
        );
    }

    /// Fragments whose footers name a different bundle are refused, and an
    /// unreliable one is quietly discarded while a reliable one is a
    /// corrupted packet.
    #[test]
    fn mangled_footers_are_refused() {
        let mut bundle = bundle();

        // Unreliable fragment of some other bundle: discarded, bundle stays.
        assert!(bundle.add_packet(fragment(9, 20, 25, false), false, "TEST", 1));
        assert_eq!(bundle.remaining(), 5, "the stray fragment changed nothing");

        // Reliable fragment of another bundle on an external interface:
        // rejected, the packet counts as corrupted.
        assert!(!bundle.add_packet(fragment(9, 20, 25, true), true, "TEST", 2));
        assert_eq!(bundle.remaining(), 5);

        // The same on an internal interface is a critical -- same rejection.
        assert!(!bundle.add_packet(fragment(9, 20, 25, true), false, "TEST", 3));
        assert_eq!(bundle.remaining(), 5);
    }

    /// BW-02-03-06 acceptance, second half: a bundle untouched for longer
    /// than MAX_AGE is old, and the receive dispatch reclaims it rather
    /// than let a lost fragment pin it forever.
    #[test]
    fn an_untouched_bundle_goes_stale_after_max_age() {
        let mut bundle = bundle();
        assert!(!bundle.is_old(0));

        // A touch inside the age keeps it alive.
        assert!(bundle.add_packet(fragment(2, 0, 5, true), false, "TEST", 5_000_000));
        assert!(!bundle.is_old(5_000_000 + MAX_AGE_STAMPS), "just inside");
        assert!(!bundle.is_old(14_999_999));

        // One stamp past the age, it is abandoned.
        assert!(bundle.is_old(15_000_001));
        assert_eq!(bundle.age(15_000_001), 10.000001);

        // The receive dispatch's reclaim arm: an old bundle is dropped and
        // the late fragment starts a fresh one at the same key.
        let stale = bundle;
        let mut fresh = FragmentedBundle::new(fragment(0, 0, 5, true), 20_000_000);
        assert!(stale.is_old(20_000_000), "the stale bundle is reclaimed");
        assert!(!fresh.is_old(20_000_000), "the fresh bundle replaces it");
        assert!(fresh.add_packet(fragment(2, 0, 5, true), false, "TEST", 20_000_000));
        assert_eq!(chain_seq(&fresh), vec![0, 2]);
    }

    /// The off-channel key sorts by first fragment, then address, as the
    /// reference's `operator<` does -- so a `BTreeMap` lookup finds the
    /// bundle a fragment belongs to.
    #[test]
    fn the_key_sorts_by_fragment_then_address() {
        let early = Key {
            addr: addr(0x0100_0001),
            first_fragment: 100,
        };
        let late = Key {
            addr: addr(0x0100_0001),
            first_fragment: 200,
        };
        let other = Key {
            addr: addr(0x0100_0002),
            first_fragment: 100,
        };
        assert!(early < late, "first fragment orders first");
        assert!(early < other, "then the address");

        let mut bundles: FragmentedBundles = FragmentedBundles::new();
        bundles.insert(early, Rc::new(RefCell::new(bundle())));
        assert!(bundles.contains_key(&early));
        assert!(!bundles.contains_key(&late));
    }

    fn addr(ip: u32) -> Address {
        Address::new(ip, 20)
    }
}
