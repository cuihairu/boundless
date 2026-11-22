//! `lib/network/fragmented_bundle.hpp` / `fragmented_bundle.cpp`: a
//! partially reassembled multi-packet bundle. Fragments arrive in no
//! particular order, so the bundle keeps a chain sorted by fragment index and
//! counts down how many are still missing; the channel holds one of these
//! (off-channel fragments are keyed by address in the receiver,
//! BW-02-03-07).
//!
//! Still the seam: the stream halves (`addToStream` / `createFromStream`)
//! belong to the channel-state stream path, and the off-channel
//! `FragmentedBundles` lookup with its completion dispatch belongs to the
//! receiver item (BW-02-03-07).

use std::cell::RefCell;
use std::rc::Rc;

use crate::event_dispatcher::STAMPS_PER_SECOND;
use crate::misc::{SeqNum, seq_less_than};
use crate::packet::{Packet, flags};

/// `FragmentedBundle::MAX_AGE`: the age (in seconds) at which a fragmented
/// bundle is abandoned.
pub const MAX_AGE: u64 = 10;

/// `FragmentedBundle`: a bundle that has received some but not all of its
/// fragments.
pub struct FragmentedBundle {
    /// `lastFragment_`.
    last_fragment: SeqNum,
    /// `remaining_`: one less than the number of fragments in the bundle,
    /// counted down as they arrive.
    remaining: i32,
    /// `touched_`, in stamps.
    touched: u64,
    /// `pChain_`: the fragments so far, sorted by fragment index.
    p_chain: Rc<RefCell<Packet>>,
}

impl FragmentedBundle {
    /// `FragmentedBundle( pFirstPacket )` (fragmented_bundle.cpp:13).
    pub fn new(p_first_packet: &Rc<RefCell<Packet>>, now: u64) -> Self {
        let packet = p_first_packet.borrow();
        Self {
            last_fragment: packet.frag_end(),
            // Remaining is one less than the size
            remaining: (packet
                .frag_end()
                .wrapping_sub(packet.frag_begin())
                .wrapping_add(1)) as i32
                - 1,
            touched: now,
            p_chain: Rc::clone(p_first_packet),
        }
    }

    /// `addPacket( p, isExternal, sourceStr )`
    /// (fragmented_bundle.cpp:26): file a fragment into the chain in the
    /// order it was split, not the order it arrived. Returns `false` only
    /// when the reliable fragment's footers are mangled.
    pub fn add_packet(
        &mut self,
        p: Rc<RefCell<Packet>>,
        is_external: bool,
        source: &str,
        now: u64,
    ) -> bool {
        let (chain_begin, chain_end) = {
            let chain = self.p_chain.borrow();
            (chain.frag_begin(), chain.frag_end())
        };
        let (frag_begin, frag_end, seq, is_reliable) = {
            let packet = p.borrow();
            (
                packet.frag_begin(),
                packet.frag_end(),
                packet.seq(),
                packet.has_flags(flags::IS_RELIABLE),
            )
        };

        // If the last fragment seqnums for existing packets in this bundle
        // and this one don't match up, we can't process it.
        if chain_begin != frag_begin || chain_end != frag_end {
            // If the incoming fragment is unreliable, then we're still
            // waiting for the reliable bundle attached to this to complete.
            if !is_reliable {
                eprintln!(
                    "FragmentedBundle::addPacket( {} ): Discarding unreliable fragment #{} ({},{}) while waiting for reliable chain ({},{}) to complete",
                    source, seq, frag_begin, frag_end, chain_begin, chain_end
                );

                return true;
            }

            // If we're on an external interface, then it could be someone
            // mangling the packets on purpose.
            eprintln!(
                "FragmentedBundle::addPacket( {} ): {}fragment footers on packet #{}, got [{},{}], expecting [{},{}]",
                source,
                if is_external {
                    "Mangled "
                } else {
                    "CRITICAL mangled "
                },
                seq,
                frag_begin,
                frag_end,
                chain_begin,
                chain_end
            );

            return false;
        }

        self.touched = now;

        // Find where this goes in the chain.
        let mut walk = Some(self.p_chain.clone());
        let mut pre: Option<Rc<RefCell<Packet>>> = None;
        let mut duplicate = false;
        while let Some(current) = walk.clone() {
            let current_seq = current.borrow().seq();

            // If p is already in this chain, bail now.
            if current_seq == seq {
                duplicate = true;
                break;
            }

            // Stop when `current` is the packet in the chain after p.
            if seq_less_than(seq, current_seq) {
                break;
            }

            let next = current.borrow().next().clone();
            pre = Some(current);
            walk = next;
        }

        if duplicate {
            eprintln!(
                "FragmentedBundle::addPacket( {} ): Discarding duplicate fragment #{}",
                source, seq
            );

            return true;
        }

        // Add it to the chain.
        if let Some(next) = walk {
            // chain -> ... -> p -> walk (a freshly received packet has no
            // chain of its own; when walk is None, p is simply the new tail)
            p.borrow_mut().chain(next);
        }
        match pre {
            // chain -> p -> walk
            None => self.p_chain = p.clone(),
            // chain -> ... -> pre -> p -> walk
            Some(prev) => prev.borrow_mut().chain(p.clone()),
        }

        debug_assert!(self.remaining > 0);

        self.remaining -= 1;

        true
    }

    /// `age()` (fragmented_bundle.hpp:29): how long this bundle has been
    /// waiting, in seconds.
    pub fn age(&self, now: u64) -> f64 {
        now.saturating_sub(self.touched) as f64 / STAMPS_PER_SECOND as f64
    }

    /// `isOld()` (fragmented_bundle.hpp:36): has the bundle waited longer
    /// than `MAX_AGE` seconds since its last fragment?
    pub fn is_old(&self, now: u64) -> bool {
        now.saturating_sub(self.touched) > STAMPS_PER_SECOND * MAX_AGE
    }

    /// `isReliable()` (fragmented_bundle.hpp:43).
    pub fn is_reliable(&self) -> bool {
        self.p_chain.borrow().has_flags(flags::IS_RELIABLE)
    }

    /// `chainLength()` (fragmented_bundle.hpp:46): how many fragments have
    /// arrived.
    pub fn chain_length(&self) -> usize {
        self.p_chain.borrow().chain_length()
    }

    /// `pChain()` (fragmented_bundle.hpp:49).
    pub fn p_chain(&self) -> Rc<RefCell<Packet>> {
        Rc::clone(&self.p_chain)
    }

    /// `lastFragment()` (fragmented_bundle.hpp:51).
    pub fn last_fragment(&self) -> SeqNum {
        self.last_fragment
    }

    /// `isComplete()` (fragmented_bundle.hpp:54).
    pub fn is_complete(&self) -> bool {
        self.remaining == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn received_fragment_packet(
        begin: SeqNum,
        end: SeqNum,
        seq: SeqNum,
        reliable: bool,
    ) -> Rc<RefCell<Packet>> {
        Packet::fragment_for_test(begin, end, seq, reliable)
    }

    /// A three-fragment reliable bundle whose packets carry sequences
    /// 10..=12. The footers name the packet sequence range of the whole
    /// bundle, so every fragment reads [10, 12]. The middle fragment
    /// arrived first and started the reassembly.
    fn bundle() -> FragmentedBundle {
        FragmentedBundle::new(&received_fragment_packet(10, 12, 11, true), 0)
    }

    /// The packet sequences as the chain files them, first to last.
    fn chain_seqs(p_chain: Rc<RefCell<Packet>>) -> Vec<SeqNum> {
        let mut seqs = Vec::new();
        let mut walk = Some(p_chain);
        while let Some(current) = walk {
            let (seq, next) = {
                let packet = current.borrow();
                (packet.seq(), packet.next())
            };
            seqs.push(seq);
            walk = next;
        }
        seqs
    }

    #[test]
    fn fragments_are_chained_in_the_order_they_were_split() {
        let mut bundle = bundle();
        assert_eq!(bundle.chain_length(), 1);

        // The last fragment arrives second.
        assert!(bundle.add_packet(
            received_fragment_packet(10, 12, 12, true),
            false,
            "test",
            2_000
        ));

        // So far it is the first fragment that is missing.
        assert!(!bundle.is_complete());
        assert_eq!(chain_seqs(bundle.p_chain()), vec![11, 12]);
        assert_eq!(bundle.last_fragment(), 12);
        assert!(bundle.is_reliable());

        // Then the gap closes with the first fragment.
        assert!(bundle.add_packet(
            received_fragment_packet(10, 12, 10, true),
            false,
            "test",
            3_000
        ));
        assert_eq!(chain_seqs(bundle.p_chain()), vec![10, 11, 12]);
        assert!(bundle.is_complete());
    }

    #[test]
    fn a_repeated_fragment_is_discarded_not_chained_twice() {
        let mut bundle = bundle();
        assert!(bundle.add_packet(
            received_fragment_packet(10, 12, 12, true),
            false,
            "test",
            2_000
        ));
        // The last fragment arrives twice; the repeat is dropped, not filed.
        assert!(bundle.add_packet(
            received_fragment_packet(10, 12, 12, true),
            false,
            "test",
            3_000
        ));

        assert_eq!(chain_seqs(bundle.p_chain()), vec![11, 12]);
        assert_eq!(bundle.chain_length(), 2);
        assert!(!bundle.is_complete());
    }

    #[test]
    fn a_reliable_fragment_from_another_bundle_is_refused() {
        let mut bundle = bundle();

        // An unreliable one from the bundle next door is merely dropped;
        // the channel can still finish its own.
        assert!(bundle.add_packet(
            received_fragment_packet(20, 22, 21, false),
            false,
            "test",
            2_000
        ));
        assert_eq!(chain_seqs(bundle.p_chain()), vec![11]);
        assert_eq!(bundle.chain_length(), 1);

        // But a reliable one with the wrong footers says the bundle is
        // mangled and nothing more of it can be used.
        assert!(!bundle.add_packet(
            received_fragment_packet(20, 22, 22, true),
            false,
            "test",
            3_000
        ));
        assert_eq!(chain_seqs(bundle.p_chain()), vec![11]);
        assert_eq!(bundle.chain_length(), 1);
    }

    #[test]
    fn a_bundle_abandoned_after_max_age_is_recycled() {
        let mut bundle = bundle();
        let second = STAMPS_PER_SECOND;

        // Exactly MAX_AGE old is not yet old enough to throw away.
        assert!(!bundle.is_old(second * MAX_AGE));
        assert!((bundle.age(second * MAX_AGE) - MAX_AGE as f64).abs() < f64::EPSILON);

        // Touched by a late fragment, so the clock restarts.
        assert!(bundle.add_packet(
            received_fragment_packet(10, 12, 12, true),
            false,
            "test",
            second * 5
        ));
        assert!(!bundle.is_old(second * (5 + MAX_AGE)));
        assert!((bundle.age(second * (5 + MAX_AGE)) - MAX_AGE as f64).abs() < f64::EPSILON);

        // One more second and the half-finished bundle is abandoned.
        let expired = second * (5 + MAX_AGE + 1);
        assert!(bundle.is_old(expired));
        assert!(bundle.age(expired) > MAX_AGE as f64);
    }
}
