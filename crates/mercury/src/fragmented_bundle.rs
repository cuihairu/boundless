//! BW-02 fragment reassembly: `FragmentedBundle` (`fragmented_bundle.hpp`)
//! plus the fragment branch of `PacketReceiver::processOrderedPacket`
//! (`packet_receiver.cpp:1207-1360`).
//!
//! A bundle that spans several datagrams arrives as one fragment `Packet`
//! per datagram. Each carries the bundle's fragment range in its footers
//! (after `stripFragInfo`, `fragBegin`/`fragEnd` name the whole range and
//! `seq` is this fragment's number). The reassembler keeps the fragments it
//! has in sequence order until the last one lands, then hands the ordered
//! chain back for dispatch.
//!
//! Two stores mirror the reference: a channel keeps one pending bundle in
//! its own `pFragments` slot (cheap lookup, since only reliable packets use
//! the channel's sequence numbers), and off-channel traffic keeps a
//! `FragmentedBundles` map keyed by `(addr, fragBegin)`.
//!
//! Deviation: the reference chains fragments with intrusive `PacketPtr`
//! links (`p->chain(walk)`); this port holds an ordered `Vec<Rc<Packet>>`,
//! the documented chain convention elsewhere in this crate. The stream
//! encode/decode pair (`addToStream`/`createFromStream`) is deferred with
//! `UnackedPacket`/`Packet` stream encoding (BW-02-03-01).

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::rc::Rc;

use crate::endpoint::Address;
use crate::misc::{SeqNum, seq_less_than};
use crate::packet::{Packet, flags};

/// `FragmentedBundle::MAX_AGE`: the age, in seconds, at which a fragmented
/// bundle is abandoned.
pub const MAX_AGE: u64 = 10;

/// `stampsPerSecond()`: stamps are microseconds (`timestamp()` is micros),
/// so a second is one million stamps. The reference's `isOld` test is
/// `now - touched_ > STAMPS_PER_SECOND * MAX_AGE`.
const STAMPS_PER_SECOND: u64 = 1_000_000;

/// A partially reassembled multi-packet bundle.
pub struct FragmentedBundle {
    /// `lastFragment_`: the bundle's `fragEnd`.
    last_fragment: SeqNum,
    /// `remaining_`: fragments still missing -- one less than the count,
    /// since the first packet is counted at construction.
    remaining: u32,
    /// `touched_`: the stamp of the last fragment added, micros.
    touched: u64,
    /// `pChain_`: the fragments held so far, in sequence order.
    chain: Vec<Rc<Packet>>,
}

/// The once-off map's keys (`FragmentedBundle::Key`).
#[derive(Clone, Copy, Debug)]
pub struct FragmentedBundleKey {
    /// `firstFragment_`: the bundle's `fragBegin`.
    pub first_fragment: SeqNum,
    /// `addr_`: the sender; `Address`'s `Eq` ignores `salt`.
    pub addr: Address,
}

/// The once-off map (`FragmentedBundles`).
pub type FragmentedBundles = BTreeMap<FragmentedBundleKey, FragmentedBundle>;

impl PartialEq for FragmentedBundleKey {
    /// `operator==`: both fields, `salt` ignored by `Address`'s own `Eq`.
    fn eq(&self, other: &Self) -> bool {
        self.first_fragment == other.first_fragment && self.addr == other.addr
    }
}

impl Eq for FragmentedBundleKey {}

impl PartialOrd for FragmentedBundleKey {
    /// `operator<`: `firstFragment_` first, then `addr_`.
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FragmentedBundleKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.first_fragment
            .cmp(&other.first_fragment)
            .then(self.addr.cmp(&other.addr))
    }
}

impl FragmentedBundle {
    /// `FragmentedBundle( pFirstPacket )`.
    pub fn new(first: Rc<Packet>, now: u64) -> Self {
        Self {
            last_fragment: first.frag_end(),
            // Remaining is one less than the size.
            remaining: first.frag_end().wrapping_sub(first.frag_begin()),
            touched: now,
            chain: vec![first],
        }
    }

    /// `addPacket( p, isExternal, sourceStr )`: merge `p` into the chain.
    /// False when the fragment's footers mismatch the bundle's range and
    /// the fragment is reliable -- the bundle cannot be completed and the
    /// caller must treat the packet as corrupt.
    pub fn add_packet(
        &mut self,
        packet: Rc<Packet>,
        is_external: bool,
        source: &str,
        now: u64,
    ) -> bool {
        let head = &self.chain[0];

        // If the last fragment seqnums for existing packets in this bundle
        // and this one don't match up, we can't process it.
        if head.frag_begin() != packet.frag_begin() || head.frag_end() != packet.frag_end() {
            // If the incoming fragment is unreliable, then we're still
            // waiting for the reliable bundle attached to this to complete.
            if !packet.has_flags(flags::IS_RELIABLE) {
                assert!(
                    self.is_reliable() || is_external,
                    "FragmentedBundle::addPacket: MF_ASSERT( this->isReliable() || isExternal )"
                );
                eprintln!(
                    "FragmentedBundle::addPacket( {source} ): Discarding unreliable fragment \
                     #{} (#{},#{}) while waiting for reliable chain (#{},{}) to complete",
                    packet.seq(),
                    packet.frag_begin(),
                    packet.frag_end(),
                    head.frag_begin(),
                    head.frag_end()
                );
                return true;
            }

            // If we're on an external interface, then it could be someone
            // mangling the packets on purpose. We should never get this for
            // reliable traffic on internal interfaces (CRITICAL otherwise).
            eprintln!(
                "FragmentedBundle::addPacket( {source} ): Mangled fragment footers on \
                 packet #{}, got [{},{}], expecting [{},{}]",
                packet.seq(),
                packet.frag_begin(),
                packet.frag_end(),
                head.frag_begin(),
                head.frag_end()
            );
            return false;
        }

        self.touched = now;

        // find where this goes in the chain
        let mut insert_at = self.chain.len();
        for (i, walk) in self.chain.iter().enumerate() {
            // If p is already in this chain, bail now
            if walk.seq() == packet.seq() {
                eprintln!(
                    "FragmentedBundle::addPacket( {source} ): Discarding duplicate fragment #{}",
                    packet.seq()
                );
                return true;
            }

            // Stop when 'walk' is the packet in the chain after p.
            if seq_less_than(packet.seq(), walk.seq()) {
                insert_at = i;
                break;
            }
        }

        // add it to the chain
        self.chain.insert(insert_at, packet);

        assert!(
            self.remaining > 0,
            "FragmentedBundle::addPacket: MF_ASSERT( remaining_ > 0 )"
        );

        self.remaining -= 1;

        true
    }

    /// `age()`: seconds since the last fragment was added.
    pub fn age(&self, now: u64) -> f64 {
        (now.saturating_sub(self.touched)) as f64 / STAMPS_PER_SECOND as f64
    }

    /// `isOld()`: `now - touched_ > stampsPerSecond() * MAX_AGE`.
    pub fn is_old(&self, now: u64) -> bool {
        now.saturating_sub(self.touched) > STAMPS_PER_SECOND * MAX_AGE
    }

    /// `isReliable()`: the chain head carries `FLAG_IS_RELIABLE`.
    pub fn is_reliable(&self) -> bool {
        self.chain[0].has_flags(flags::IS_RELIABLE)
    }

    /// `chainLength()`.
    pub fn chain_length(&self) -> usize {
        self.chain.len()
    }

    /// `pChain()`.
    pub fn chain(&self) -> &[Rc<Packet>] {
        &self.chain
    }

    /// `lastFragment()`.
    pub fn last_fragment(&self) -> SeqNum {
        self.last_fragment
    }

    /// `isComplete()`: `remaining_ == 0`.
    pub fn is_complete(&self) -> bool {
        self.remaining == 0
    }

    /// `OnceOffReceiver::handleTimeout`'s `clearFragmentedBundlesTimer`
    /// arm (`once_off_packet.cpp:133-154`): drop every stale bundle,
    /// warning with its age and packet count. The reference logs the
    /// address's string; the map's key carries it here.
    pub fn sweep_stale(bundles: &mut FragmentedBundles, now: u64) {
        bundles.retain(|key, bundle| {
            if bundle.is_old(now) {
                eprintln!(
                    "OnceOffReceiver::handleTimeout: Discarded stale fragmented bundle from \
                     {} ({:.1}s old, {} packets)",
                    key.addr,
                    bundle.age(now),
                    bundle.chain_length()
                );
                false
            } else {
                true
            }
        });
    }
}

/// What `assemble_fragment` did with the packet.
pub enum FragmentOutcome {
    /// Not a fragment: the caller dispatches the packet as-is (the
    /// reference falls through to `UDPBundleProcessor`). The packet is
    /// handed back untouched.
    NotFragment(Packet),
    /// The fragment was accepted; the bundle is not complete yet.
    Incomplete,
    /// The bundle completed: the ordered chain is ready to dispatch.
    Complete(Vec<Rc<Packet>>),
    /// The packet was corrupted.
    Corrupt,
}

/// The fragment branch of `processOrderedPacket`
/// (`packet_receiver.cpp:1207-1360`): strip the fragment footers, find or
/// start the bundle, merge the fragment, and hand back the ordered chain
/// when the last fragment lands.
///
/// `has_channel` is `pChannel != NULL`. Only reliable packets use the
/// channel's sequence numbers, so `isOnChannel` is computed here from the
/// packet's `FLAG_IS_RELIABLE` as the reference does. `source` is the
/// channel's `c_str()` or the address's string, for the warnings. `now` is
/// injectable for tests (micros).
///
/// The reference branches this out of one big method, so the port keeps the
/// same many-parameter shape rather than inventing a context struct the
/// C++ does not have.
#[allow(clippy::too_many_arguments)]
pub fn assemble_fragment(
    addr: &Address,
    packet: Packet,
    has_channel: bool,
    is_external: bool,
    source: &str,
    now: u64,
    channel_fragments: &mut Option<FragmentedBundle>,
    once_off: &mut FragmentedBundles,
) -> FragmentOutcome {
    let is_on_channel = has_channel && packet.has_flags(flags::IS_RELIABLE);
    let mut packet = packet;

    // Make sure that for incoming non-fragment packets with sequence
    // numbers, we are not expecting them to be fragments.
    if !packet.has_flags(flags::IS_FRAGMENT)
        && packet.has_flags(flags::HAS_SEQUENCE_NUMBER)
        && is_on_channel
        && channel_fragments.is_some()
    {
        eprintln!(
            "Nub::processOrderedPacket( {source} ): got non-fragment packet, when expecting \
             fragment (#{})",
            packet.seq()
        );
        return FragmentOutcome::Corrupt;
    }

    if !packet.has_flags(flags::IS_FRAGMENT) {
        return FragmentOutcome::NotFragment(packet);
    }

    if !packet.strip_frag_info() {
        eprintln!("PacketReceiver::processOrderedPacket( {source} ): Invalid fragment footers");
        return FragmentOutcome::Corrupt;
    }

    let key = FragmentedBundleKey {
        first_fragment: packet.frag_begin(),
        addr: *addr,
    };
    let seq = packet.seq();
    let frag_begin = packet.frag_begin();
    let frag_end = packet.frag_end();
    let packet = Rc::new(packet);

    if is_on_channel {
        // Channels are easy, they just maintain their own fragment chain,
        // which makes lookup really cheap. Note that only reliable packets
        // use the channel's sequence numbers. On-channel bundles are never
        // reclaimed here: `isOld() && !isOnChannel` gates the erase.
        let complete = match channel_fragments.as_mut() {
            None => {
                // If this is on a channel, it must be the first packet in
                // the bundle, since channel traffic is ordered.
                if seq != frag_begin {
                    eprintln!(
                        "PacketReceiver::processOrderedPacket( {source} ): Bundle \
                         (#{frag_begin},#{frag_end}) is missing packets before #{seq}"
                    );
                    return FragmentOutcome::Corrupt;
                }
                *channel_fragments = Some(FragmentedBundle::new(packet, now));
                return FragmentOutcome::Incomplete;
            }
            Some(bundle) => {
                if !bundle.add_packet(packet, is_external, source, now) {
                    return FragmentOutcome::Corrupt;
                }
                bundle.is_complete()
            }
        };

        if !complete {
            return FragmentOutcome::Incomplete;
        }

        let chain = channel_fragments
            .take()
            .expect("a complete bundle sat in the channel slot")
            .chain()
            .to_vec();
        FragmentOutcome::Complete(chain)
    } else {
        // We need to look up off-channel fragments in fragmentedBundles.
        match once_off.entry(key) {
            Entry::Occupied(mut entry) => {
                // If the previous fragment is really old, then this must be
                // some failed bundle that has not been resent and has been
                // given up on, so we get rid of it now.
                if entry.get().is_old(now) {
                    eprintln!(
                        "PacketReceiver::processOrderedPacket( {source} ): Discarding abandoned \
                         stale overlapping fragmented bundle from seq {frag_begin} to {}",
                        entry.get().last_fragment()
                    );
                    entry.remove();
                    if has_channel && seq != frag_begin {
                        eprintln!(
                            "PacketReceiver::processOrderedPacket( {source} ): Bundle \
                             (#{frag_begin},#{frag_end}) is missing packets before #{seq}"
                        );
                        return FragmentOutcome::Corrupt;
                    }
                    once_off.insert(key, FragmentedBundle::new(packet, now));
                    return FragmentOutcome::Incomplete;
                }

                let bundle = entry.get_mut();
                if !bundle.add_packet(packet, is_external, source, now) {
                    return FragmentOutcome::Corrupt;
                }
                if !bundle.is_complete() {
                    return FragmentOutcome::Incomplete;
                }
                let chain = entry.remove().chain().to_vec();
                FragmentOutcome::Complete(chain)
            }
            Entry::Vacant(entry) => {
                if has_channel && seq != frag_begin {
                    eprintln!(
                        "PacketReceiver::processOrderedPacket( {source} ): Bundle \
                         (#{frag_begin},#{frag_end}) is missing packets before #{seq}"
                    );
                    return FragmentOutcome::Corrupt;
                }
                entry.insert(FragmentedBundle::new(packet, now));
                FragmentOutcome::Incomplete
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::HEADER_SIZE;

    /// Build a fragment the way it arrives on the wire: a body the two frag
    /// footers trail, fragBegin below fragEnd, `seq` inside the range. The
    /// frag fields are still unset -- `assemble_fragment` strips them.
    fn as_arrived(seq: u32, frag_begin: u32, frag_end: u32, reliable: bool) -> Packet {
        let mut packet = Packet::new();
        packet.set_seq(seq);
        let mut word = flags::IS_FRAGMENT | flags::HAS_SEQUENCE_NUMBER;
        if reliable {
            word |= flags::IS_RELIABLE;
        }
        packet.enable_flags(word);
        packet.grow(HEADER_SIZE);
        packet.grow(4); // a body the footers trail
        packet.buffer_mut()[HEADER_SIZE + 4..HEADER_SIZE + 8]
            .copy_from_slice(&frag_begin.to_le_bytes());
        packet.buffer_mut()[HEADER_SIZE + 8..HEADER_SIZE + 12]
            .copy_from_slice(&frag_end.to_le_bytes());
        packet.grow(8);
        packet
    }

    /// A fragment after the receive path stripped its footers: the range
    /// filled in, the body back down. The bundle-level tests hand
    /// `addPacket` already-stripped packets, the way
    /// `processOrderedPacket` does.
    fn fragment(seq: u32, frag_begin: u32, frag_end: u32, reliable: bool) -> Packet {
        let mut packet = as_arrived(seq, frag_begin, frag_end, reliable);
        assert!(
            packet.strip_frag_info(),
            "the helper built a valid fragment"
        );
        packet
    }

    fn addr() -> Address {
        Address::new(0x0100007f, 40000)
    }

    /// A helper running the fragment branch over a fresh pair of stores.
    fn run(
        packet: Packet,
        has_channel: bool,
        is_external: bool,
        now: u64,
        slot: &mut Option<FragmentedBundle>,
        map: &mut FragmentedBundles,
    ) -> FragmentOutcome {
        assemble_fragment(
            &addr(),
            packet,
            has_channel,
            is_external,
            "channel",
            now,
            slot,
            map,
        )
    }

    // ------------------------------------------------------------------
    // FragmentedBundle
    // ------------------------------------------------------------------

    /// The constructor: one less than the count remaining, the last
    /// fragment's number, and reliability from the head packet.
    #[test]
    fn constructor_counts_the_fragments_still_missing() {
        let first = Rc::new(fragment(0, 0, 2, true));
        let bundle = FragmentedBundle::new(first, 1_000_000);

        assert_eq!(bundle.last_fragment(), 2);
        assert_eq!(bundle.chain_length(), 1);
        assert!(!bundle.is_complete(), "two fragments still missing");
        assert!(bundle.is_reliable(), "the head carries the reliable flag");
    }

    /// `addPacket` inserts by sequence order, so out-of-order fragments
    /// reassemble into the send-order chain and complete.
    #[test]
    fn out_of_order_fragments_reassemble_in_sequence_order() {
        let mut bundle = FragmentedBundle::new(Rc::new(fragment(1, 1, 3, true)), 1_000_000);

        assert!(bundle.add_packet(Rc::new(fragment(3, 1, 3, true)), false, "c", 1_000_000));
        assert!(!bundle.is_complete());
        assert_eq!(bundle.chain_length(), 2);

        assert!(bundle.add_packet(Rc::new(fragment(2, 1, 3, true)), false, "c", 1_000_000));
        assert!(bundle.is_complete());
        assert_eq!(bundle.chain_length(), 3);

        let seqs: Vec<u32> = bundle.chain().iter().map(|p| p.seq()).collect();
        assert_eq!(seqs, vec![1, 2, 3], "the chain holds the send order");
    }

    /// A fragment already in the chain is discarded, not inserted twice.
    #[test]
    fn duplicate_fragment_is_discarded() {
        let mut bundle = FragmentedBundle::new(Rc::new(fragment(1, 1, 3, true)), 0);
        assert!(bundle.add_packet(Rc::new(fragment(2, 1, 3, true)), false, "c", 0));
        assert!(bundle.add_packet(Rc::new(fragment(2, 1, 3, true)), false, "c", 0));
        assert_eq!(bundle.chain_length(), 2, "the duplicate was not inserted");
        assert!(!bundle.is_complete());
    }

    /// Mismatched footers on a reliable fragment cannot be completed, so
    /// the bundle is poisoned (false) -- external interfaces log a warning,
    /// internal ones would CRITICAL. Both refuse.
    #[test]
    fn mismatched_footers_on_a_reliable_fragment_are_rejected() {
        let mut bundle = FragmentedBundle::new(Rc::new(fragment(1, 1, 3, true)), 0);

        assert!(!bundle.add_packet(Rc::new(fragment(3, 2, 4, true)), true, "c", 0));
        assert_eq!(bundle.chain_length(), 1);

        assert!(!bundle.add_packet(Rc::new(fragment(3, 2, 4, true)), false, "c", 0));
        assert_eq!(bundle.chain_length(), 1);
    }

    /// An unreliable fragment with mismatched footers is discarded while
    /// waiting for the reliable chain, not treated as corruption.
    #[test]
    fn unreliable_fragment_is_discarded_while_waiting_for_the_reliable_chain() {
        let mut bundle = FragmentedBundle::new(Rc::new(fragment(1, 1, 3, true)), 0);

        assert!(bundle.add_packet(Rc::new(fragment(3, 2, 4, false)), true, "c", 0));
        assert!(bundle.add_packet(Rc::new(fragment(3, 2, 4, false)), false, "c", 0));
        assert_eq!(
            bundle.chain_length(),
            1,
            "the unreliable fragment was dropped"
        );
    }

    /// `MF_ASSERT( this->isReliable() || isExternal )`: an unreliable
    /// fragment with mismatched footers on an internal interface, against a
    /// non-reliable chain, is a programming error.
    #[test]
    #[should_panic(expected = "MF_ASSERT( this->isReliable() || isExternal )")]
    fn unreliable_mismatch_against_a_non_reliable_chain_asserts() {
        let mut bundle = FragmentedBundle::new(Rc::new(fragment(1, 1, 3, false)), 0);
        bundle.add_packet(Rc::new(fragment(3, 2, 4, false)), false, "c", 0);
    }

    /// `MF_ASSERT( remaining_ > 0 )`: adding a fragment past the counted
    /// range overflows the count.
    #[test]
    #[should_panic(expected = "MF_ASSERT( remaining_ > 0 )")]
    fn adding_a_fragment_past_the_count_asserts() {
        let mut bundle = FragmentedBundle::new(Rc::new(fragment(0, 0, 2, true)), 0);
        assert!(bundle.add_packet(Rc::new(fragment(1, 0, 2, true)), false, "c", 0));
        assert!(bundle.add_packet(Rc::new(fragment(2, 0, 2, true)), false, "c", 0));
        assert!(bundle.is_complete());

        // The count is exhausted and no in-range sequence number is left to
        // add, so hand `addPacket` a fragment whose number sits outside the
        // bundle's range.
        let mut overflowing = Packet::new();
        overflowing.set_seq(9);
        overflowing.set_frag_range_for_test(0, 2);
        bundle.add_packet(Rc::new(overflowing), false, "c", 0);
    }

    /// `isOld()` is strict: exactly `MAX_AGE` seconds is not yet old.
    #[test]
    fn age_and_old_boundaries() {
        let now = 1_000_000;
        let bundle = FragmentedBundle::new(Rc::new(fragment(1, 1, 3, true)), now);

        assert_eq!(bundle.age(now), 0.0);
        assert!(!bundle.is_old(now));

        let limit = STAMPS_PER_SECOND * MAX_AGE;
        assert!(
            !bundle.is_old(now + limit),
            "exactly MAX_AGE is not yet old"
        );
        assert!(bundle.is_old(now + limit + 1));
        assert!((bundle.age(now + limit) - MAX_AGE as f64).abs() < 1e-9);
    }

    // ------------------------------------------------------------------
    // FragmentedBundleKey / FragmentedBundles
    // ------------------------------------------------------------------

    /// `operator==` and `operator<`: firstFragment first, then address.
    #[test]
    fn bundle_keys_order_by_fragment_then_address() {
        let a = FragmentedBundleKey {
            first_fragment: 5,
            addr: addr(),
        };
        let same = FragmentedBundleKey {
            first_fragment: 5,
            addr: addr(),
        };
        let bigger_frag = FragmentedBundleKey {
            first_fragment: 6,
            addr: addr(),
        };
        let bigger_addr = FragmentedBundleKey {
            first_fragment: 5,
            addr: Address::new(0x0100007f, 40001),
        };

        assert_eq!(a, same);
        assert!(a < bigger_frag, "firstFragment is the primary key");
        assert!(a < bigger_addr, "the address breaks the tie");
        assert!(bigger_frag > bigger_addr, "fragment wins over address");
    }

    /// `sweep_stale` drops the old bundles and keeps the fresh ones.
    #[test]
    fn sweep_stale_drops_only_the_old_bundles() {
        let mut map = FragmentedBundles::new();
        let sweep_time = STAMPS_PER_SECOND * MAX_AGE + 1_000_000;
        map.insert(
            FragmentedBundleKey {
                first_fragment: 1,
                addr: addr(),
            },
            FragmentedBundle::new(Rc::new(fragment(1, 1, 3, true)), 0),
        );
        map.insert(
            FragmentedBundleKey {
                first_fragment: 10,
                addr: addr(),
            },
            FragmentedBundle::new(Rc::new(fragment(10, 10, 12, true)), sweep_time - 100),
        );

        FragmentedBundle::sweep_stale(&mut map, sweep_time);

        assert_eq!(map.len(), 1, "only the fresh bundle survives");
        assert!(map.contains_key(&FragmentedBundleKey {
            first_fragment: 10,
            addr: addr(),
        }));
    }

    // ------------------------------------------------------------------
    // assemble_fragment -- the processOrderedPacket fragment branch
    // ------------------------------------------------------------------

    /// A non-fragment packet is handed back untouched, no channel needed.
    #[test]
    fn a_non_fragment_packet_is_handed_back() {
        let mut packet = Packet::new();
        packet.grow(HEADER_SIZE);
        packet.grow(4);
        let mut slot = None;
        let mut map = FragmentedBundles::new();

        let back = match run(packet, false, false, 0, &mut slot, &mut map) {
            FragmentOutcome::NotFragment(back) => back,
            _ => panic!("expected NotFragment"),
        };
        assert_eq!(back.body_size(), 4, "the packet came back untouched");
    }

    /// A non-fragment packet arriving while a channel is mid-bundle is
    /// corruption.
    #[test]
    fn a_non_fragment_packet_while_expecting_fragments_is_corrupt() {
        let mut packet = Packet::new();
        packet.set_seq(7);
        packet.enable_flags(flags::HAS_SEQUENCE_NUMBER | flags::IS_RELIABLE);
        packet.grow(HEADER_SIZE);
        packet.grow(4);
        let mut slot = Some(FragmentedBundle::new(Rc::new(fragment(1, 1, 3, true)), 0));
        let mut map = FragmentedBundles::new();

        assert!(matches!(
            run(packet, true, false, 0, &mut slot, &mut map),
            FragmentOutcome::Corrupt
        ));
    }

    /// Invalid fragment footers (a body too short to hold them) are
    /// corruption.
    #[test]
    fn invalid_fragment_footers_are_corrupt() {
        let mut packet = Packet::new();
        packet.set_seq(1);
        packet.enable_flags(flags::IS_FRAGMENT | flags::HAS_SEQUENCE_NUMBER);
        packet.grow(HEADER_SIZE);
        packet.grow(4);
        let mut slot = None;
        let mut map = FragmentedBundles::new();

        assert!(matches!(
            run(packet, false, false, 0, &mut slot, &mut map),
            FragmentOutcome::Corrupt
        ));
    }

    /// The first fragment of a channel bundle must be `fragBegin`; anything
    /// else means earlier packets were lost, which is corruption.
    #[test]
    fn a_channel_bundle_missing_packets_before_it_is_corrupt() {
        let mut slot = None;
        let mut map = FragmentedBundles::new();

        assert!(matches!(
            run(
                as_arrived(2, 1, 3, true),
                true,
                false,
                0,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Corrupt
        ));
        assert!(slot.is_none(), "the corrupt first packet left no bundle");
    }

    /// A new off-channel fragment starts a bundle in the map.
    #[test]
    fn a_new_fragment_starts_a_bundle_in_the_once_off_map() {
        let mut slot = None;
        let mut map = FragmentedBundles::new();

        assert!(matches!(
            run(
                as_arrived(1, 1, 3, true),
                false,
                false,
                0,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Incomplete
        ));
        assert!(slot.is_none());
        assert_eq!(map.len(), 1);
        let bundle = &map[&FragmentedBundleKey {
            first_fragment: 1,
            addr: addr(),
        }];
        assert_eq!(bundle.chain_length(), 1);
    }

    /// Out-of-order fragments reassemble off-channel into the ordered chain,
    /// which is handed back on completion and the map slot cleared.
    #[test]
    fn off_channel_out_of_order_fragments_complete_the_bundle() {
        let mut slot = None;
        let mut map = FragmentedBundles::new();

        assert!(matches!(
            run(
                as_arrived(1, 1, 3, true),
                false,
                false,
                0,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Incomplete
        ));
        assert!(matches!(
            run(
                as_arrived(3, 1, 3, true),
                false,
                false,
                0,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Incomplete
        ));

        let chain = match run(
            as_arrived(2, 1, 3, true),
            false,
            false,
            0,
            &mut slot,
            &mut map,
        ) {
            FragmentOutcome::Complete(chain) => chain,
            _ => panic!("expected Complete"),
        };
        let seqs: Vec<u32> = chain.iter().map(|p| p.seq()).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert!(map.is_empty(), "the completed bundle left the map");
    }

    /// On-channel fragments live in the channel slot, and completing one
    /// clears it.
    #[test]
    fn on_channel_fragments_use_the_channel_slot() {
        let mut slot = None;
        let mut map = FragmentedBundles::new();

        assert!(matches!(
            run(
                as_arrived(1, 1, 2, true),
                true,
                false,
                0,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Incomplete
        ));
        assert!(slot.is_some(), "the channel slot holds the pending bundle");
        assert!(map.is_empty(), "no once-off bundle was started");

        assert!(matches!(
            run(
                as_arrived(2, 1, 2, true),
                true,
                false,
                0,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Complete(_)
        ));
        assert!(slot.is_none(), "completion cleared the channel slot");
    }

    /// A stale overlapping off-channel bundle is abandoned and replaced by
    /// the incoming fragment's own new bundle.
    #[test]
    fn a_stale_overlapping_bundle_is_reclaimed() {
        let mut slot = None;
        let mut map = FragmentedBundles::new();
        let now = 1_000_000;

        assert!(matches!(
            run(
                as_arrived(1, 1, 3, true),
                false,
                false,
                now,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Incomplete
        ));

        // Ten seconds later a new fragment for the same key arrives; the old
        // bundle is abandoned and a fresh one starts with only this packet.
        let later = now + STAMPS_PER_SECOND * MAX_AGE + 1;
        assert!(matches!(
            run(
                as_arrived(1, 1, 3, true),
                false,
                false,
                later,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Incomplete
        ));
        let bundle = &map[&FragmentedBundleKey {
            first_fragment: 1,
            addr: addr(),
        }];
        assert_eq!(bundle.chain_length(), 1, "the stale bundle was replaced");
    }

    /// A stale off-channel bundle that fails the missing-prefix check (it is
    /// on a channel) is corruption.
    #[test]
    fn a_reclaimed_bundle_missing_its_prefix_is_corrupt() {
        let mut slot = None;
        let mut map = FragmentedBundles::new();
        let now = 1_000_000;

        assert!(matches!(
            run(
                as_arrived(1, 1, 3, true),
                false,
                false,
                now,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Incomplete
        ));

        // A later fragment for the same key, but not `fragBegin`. It is
        // unreliable, so it walks the once-off map (the channel prefix rule
        // only applies to the channel's sequence numbers); the stale bundle
        // is reclaimed and the `hasChannel` prefix check then fires.
        let later = now + STAMPS_PER_SECOND * MAX_AGE + 1;
        assert!(matches!(
            run(
                as_arrived(2, 1, 3, false),
                true,
                false,
                later,
                &mut slot,
                &mut map
            ),
            FragmentOutcome::Corrupt
        ));
    }
}
