//! `lib/network/fragmented_bundle.{hpp,cpp}` -- partially reassembled
//! multi-packet bundles (BW-02-03-06).
//!
//! A fragmented bundle collects the packets of one bundle that arrived as
//! fragments, in sequence order, until `isComplete()`. It ages from its
//! last activity and any bundle older than [`MAX_AGE_MICROS`] is
//! abandoned. The port models the reference's `Packet::chain` linked list
//! as an ordered `Vec<Packet>` (the same choice the receive window makes
//! in BW-02-03-05).
//!
//! `addToStream`/`createFromStream` belong to channel-state sync, which
//! needs `Packet` serialization and lands with it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::endpoint::Address;
use crate::misc::{SeqNum, seq_less_than};
use crate::packet::{Packet, flags};

/// `FragmentedBundle::MAX_AGE` (fragmented_bundle.hpp:26): the age, in
/// micros, at which a fragmented bundle is abandoned. 10 seconds.
pub const MAX_AGE_MICROS: u64 = 10_000_000;

/// Partially reassembled multi-packet bundles.
///
/// The chain is kept in sequence order (the reference links each packet
/// into place by `seqLessThan`), so [`Self::p_chain`] always walks the
/// fragments in the order their bundle was sent.
pub struct FragmentedBundle {
    /// `lastFragment_`: the bundle's last sequence number (`fragEnd_`).
    last_fragment: SeqNum,
    /// `remaining_`: fragments still missing; 0 means complete.
    remaining: u32,
    /// `touched_`, in micros: the last activity, driving [`Self::is_old`].
    touched: u64,
    /// `pChain_`: the ordered chain of fragments, `Packet::chain` as a
    /// list.
    chain: Vec<Packet>,
}

impl FragmentedBundle {
    /// `FragmentedBundle( pFirstPacket )` (fragmented_bundle.cpp:24): the
    /// first fragment anchors the bundle and carries the range the rest
    /// must match.
    pub fn new(first: Packet, now: u64) -> Self {
        let last_fragment = first.frag_end();
        let remaining = last_fragment.wrapping_sub(first.frag_begin());
        Self {
            last_fragment,
            remaining,
            touched: now,
            chain: vec![first],
        }
    }

    /// `addPacket( p, isExternal, sourceStr )` (fragmented_bundle.cpp:52):
    /// slot one more fragment into the chain.
    ///
    /// Returns `true` when the fragment was handled (joined, or a benign
    /// duplicate/mismatch the bundle survives) and `false` when the
    /// packet is corrupt and the caller should reject it. The fragment
    /// bounds must match the bundle's, an unreliable mismatched fragment
    /// is quietly dropped, and a reliable one is reported mangled.
    pub fn add_packet(&mut self, p: Packet, is_external: bool, source_str: &str, now: u64) -> bool {
        let head = &self.chain[0];
        if head.frag_begin() != p.frag_begin() || head.frag_end() != p.frag_end() {
            if !p.has_flags(flags::IS_RELIABLE) {
                // We're still waiting for the reliable bundle this
                // unreliable fragment was part of.
                assert!(
                    self.is_reliable() || is_external,
                    "MF_ASSERT( this->isReliable() || isExternal )"
                );
                eprintln!(
                    "FragmentedBundle::addPacket( {source_str} ): \
                     Discarding unreliable fragment #{} (#{},{}) while \
                     waiting for reliable chain (#{},{}) to complete",
                    p.seq(),
                    p.frag_begin(),
                    p.frag_end(),
                    head.frag_begin(),
                    head.frag_end()
                );
                return true;
            }

            // An external interface could be mangled on purpose; on an
            // internal one this is a bug.
            eprintln!(
                "FragmentedBundle::addPacket( {source_str} ): \
                 Mangled fragment footers on packet #{}, got [{},{}], \
                 expecting [{},{}]",
                p.seq(),
                p.frag_begin(),
                p.frag_end(),
                head.frag_begin(),
                head.frag_end()
            );
            return false;
        }

        self.touched = now;

        // Find where this goes in the chain, dropping duplicates.
        let mut insert_at = self.chain.len();
        for (i, walk) in self.chain.iter().enumerate() {
            if walk.seq() == p.seq() {
                eprintln!(
                    "FragmentedBundle::addPacket( {source_str} ): \
                     Discarding duplicate fragment #{}",
                    p.seq()
                );
                return true;
            }

            // Stop when `walk` is the packet after `p`.
            if seq_less_than(p.seq(), walk.seq()) {
                insert_at = i;
                break;
            }
        }

        self.chain.insert(insert_at, p);

        assert!(self.remaining > 0, "MF_ASSERT( remaining_ > 0 )");
        self.remaining -= 1;
        true
    }

    /// `age()` (fragmented_bundle.hpp:41), in seconds.
    pub fn age(&self, now: u64) -> f64 {
        now.saturating_sub(self.touched) as f64 / 1_000_000.0
    }

    /// `isOld()` (fragmented_bundle.hpp:46): no activity within
    /// [`MAX_AGE_MICROS`].
    pub fn is_old(&self, now: u64) -> bool {
        now.saturating_sub(self.touched) > MAX_AGE_MICROS
    }

    /// `isReliable()` (fragmented_bundle.hpp:48): the chain's head packet
    /// carries `FLAG_IS_RELIABLE`.
    pub fn is_reliable(&self) -> bool {
        self.chain[0].has_flags(flags::IS_RELIABLE)
    }

    /// `chainLength()` (fragmented_bundle.hpp:52): how many fragments are
    /// in so far.
    pub fn chain_length(&self) -> usize {
        self.chain.len()
    }

    /// `pChain()` (fragmented_bundle.hpp:53): the ordered fragment chain.
    pub fn p_chain(&self) -> &[Packet] {
        &self.chain
    }

    /// Take the ordered chain out (the hand-off when the bundle
    /// completes), leaving the bundle empty.
    pub fn take_chain(&mut self) -> Vec<Packet> {
        std::mem::take(&mut self.chain)
    }

    /// `lastFragment()` (fragmented_bundle.hpp:54).
    pub fn last_fragment(&self) -> SeqNum {
        self.last_fragment
    }

    /// `isComplete()` (fragmented_bundle.hpp:57).
    pub fn is_complete(&self) -> bool {
        self.remaining == 0
    }
}

/// `FragmentedBundles` (fragmented_bundle.hpp:112): the off-channel map of
/// partially reassembled bundles, keyed by first fragment then address.
pub type FragmentedBundles = BTreeMap<(SeqNum, Address), Rc<RefCell<FragmentedBundle>>>;

/// `OnceOffReceiver::handleTimeout`'s `clearFragmentedBundles` branch
/// (once_off_packet.cpp:133-153): drop the bundles that went stale while
/// the sweep timer ran.
pub fn discard_old_fragmented_bundles(bundles: &mut FragmentedBundles, now: u64) {
    bundles.retain(|key, bundle| {
        if bundle.borrow().is_old(now) {
            eprintln!(
                "OnceOffReceiver::handleTimeout: Discarded stale fragmented \
                 bundle from {} ({:.1}s old, {} packets)",
                key.1.c_str(),
                bundle.borrow().age(now),
                bundle.borrow().chain_length()
            );
            false
        } else {
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::flags;

    /// A reliable fragment of the bundle spanning `[frag_begin, frag_end]`,
    /// footers written in the order `strip_frag_info` reads them.
    fn fragment_packet(seq: SeqNum, frag_begin: SeqNum, frag_end: SeqNum) -> Packet {
        let mut packet = Packet::new();
        packet.set_seq(seq);
        packet.enable_flags(flags::IS_FRAGMENT | flags::IS_RELIABLE);
        packet.grow(crate::packet::HEADER_SIZE);
        packet.grow(4); // a body the footers trail
        let buf = packet.buffer_mut();
        buf[crate::packet::HEADER_SIZE + 4..crate::packet::HEADER_SIZE + 8]
            .copy_from_slice(&frag_begin.to_le_bytes());
        buf[crate::packet::HEADER_SIZE + 8..crate::packet::HEADER_SIZE + 12]
            .copy_from_slice(&frag_end.to_le_bytes());
        packet.grow(8);
        assert!(packet.strip_frag_info());
        packet
    }

    /// The acceptance for the item: fragments that arrive out of order are
    /// reassembled in sequence order -- the chain always walks 0, 1, 2 no
    /// matter which order the fragments showed up in.
    #[test]
    fn out_of_order_fragments_reassemble_in_sequence_order() {
        let mut bundle = FragmentedBundle::new(fragment_packet(0, 0, 2), 0);

        // The first two fragments leave one hole.
        assert!(bundle.add_packet(fragment_packet(2, 0, 2), true, "TEST", 1));
        assert!(!bundle.is_complete(), "fragment 1 is still missing");
        assert_eq!(bundle.chain_length(), 2);
        assert_eq!(bundle.last_fragment(), 2);

        // The hole-filling fragment completes it, and the chain comes out
        // in sequence order.
        assert!(bundle.add_packet(fragment_packet(1, 0, 2), true, "TEST", 2));
        assert!(bundle.is_complete());
        let seqs: Vec<_> = bundle.p_chain().iter().map(|p| p.seq()).collect();
        assert_eq!(seqs, vec![0, 1, 2], "按原版顺序恢复");
    }

    /// A duplicate fragment is discarded, not counted again.
    #[test]
    fn a_duplicate_fragment_is_discarded_not_readded() {
        let mut bundle = FragmentedBundle::new(fragment_packet(0, 0, 1), 0);
        assert!(bundle.add_packet(fragment_packet(1, 0, 1), true, "TEST", 1));
        assert!(bundle.is_complete());

        // A repeat of the first fragment is a benign duplicate: the chain
        // does not grow.
        assert!(bundle.add_packet(fragment_packet(0, 0, 1), true, "TEST", 2));
        assert_eq!(bundle.chain_length(), 2, "the duplicate was dropped");
    }

    /// A reliable fragment whose footers do not match the bundle is
    /// rejected as mangled on an internal interface, and the bundle keeps
    /// the chain it had.
    #[test]
    fn a_mismatched_fragment_is_rejected_on_an_internal_interface() {
        let mut bundle = FragmentedBundle::new(fragment_packet(0, 0, 2), 0);
        let before = bundle.chain_length();

        // Wrong fragment range: the bundle spans [0,2], this one [1,3].
        assert!(
            !bundle.add_packet(fragment_packet(1, 1, 3), false, "TEST", 1),
            "an internal interface reports the mangled footers as fatal"
        );
        assert_eq!(bundle.chain_length(), before);
        assert!(!bundle.is_complete());
    }

    /// An unreliable mismatched fragment is dropped silently: the reliable
    /// bundle it was a part of is still waiting for its own copies.
    #[test]
    fn an_unreliable_fragment_mismatch_is_discarded_not_fatal() {
        let mut bundle = FragmentedBundle::new(fragment_packet(0, 0, 2), 0);
        assert!(
            bundle.is_reliable(),
            "the head sets the chain's reliability"
        );

        let mut unreliable = fragment_packet(1, 0, 1);
        unreliable.disable_flags(flags::IS_RELIABLE);
        assert!(
            bundle.add_packet(unreliable, true, "TEST", 1),
            "a reliable bundle waiting on an unreliable fragment just drops it"
        );
        assert_eq!(bundle.chain_length(), 1, "nothing was added");
        assert!(!bundle.is_complete());
    }

    /// A bundle with no activity for `MAX_AGE` is old; one that is still
    /// being touched is not, and the age is measured in seconds.
    #[test]
    fn a_bundle_older_than_max_age_is_discarded() {
        let mut bundle = FragmentedBundle::new(fragment_packet(0, 0, 2), 1_000_000);
        assert!(!bundle.is_old(11_000_000), "ten seconds exactly is not old");
        assert!(bundle.is_old(11_000_001), "just past MAX_AGE is old");

        // A later fragment refreshes the clock, so the bundle lives on.
        assert!(bundle.add_packet(fragment_packet(1, 0, 2), true, "TEST", 9_000_000));
        assert!(!bundle.is_old(19_000_000), "refreshed by the new fragment");
        assert_eq!(bundle.age(11_000_000), 2.0, "age is in seconds");
    }

    /// The stale sweep drops the bundles that aged out and keeps the
    /// fresh ones.
    #[test]
    fn a_sweep_discards_only_the_stale_bundles() {
        let stale = (0, Address::new(0x0100_0001, 20));
        let fresh = (5, Address::new(0x0100_0002, 20));
        let mut bundles: FragmentedBundles = BTreeMap::new();
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
        assert!(!bundles.contains_key(&stale), "the stale one was swept");
        assert!(bundles.contains_key(&fresh), "the fresh one survives");
        assert_eq!(bundles.len(), 1);
    }
}
