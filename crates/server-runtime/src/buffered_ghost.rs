//! BW-08-04-04: buffering and replaying Real→Ghost messages across a switch.
//!
//! Messages from a single CellApp are ordered, but an offload splits a
//! Ghost's message stream into subsequences — one per CellApp the Real
//! visits. A subsequence sent by the *next* Real can arrive before the
//! current Real's sign-off (`ghostSetNextReal`) has been played, so the
//! Ghost buffers it and only replays it once the preceding subsequence
//! ends. This is `BufferedGhostMessages` and friends:
//!
//! - a *subsequence start* is `ghostSetReal` or a buffered `createGhost`
//!   (`buffered_ghost_messages.hpp:56` `isSubsequenceStart`,
//!   `buffered_ghost_message_factory.cpp` both buffered types are starts);
//! - a *subsequence end* is `delGhost` or `ghostSetNextReal`
//!   (`buffered_ghost_messages.hpp:61` `isSubsequenceEnd`);
//! - a queue keyed by the source address replays messages in order up to
//!   and including the first end, and an end that is `ghostSetNextReal`
//!   chains straight into the next Real's queue
//!   (`buffered_ghost_message_queue.cpp:144` `playSubsequence`,
//!   `entity.cpp:4975` the chain call);
//! - a queue may also *delay* an out-of-order start: the new start is
//!   parked and the still-queued messages of the previous subsequence are
//!   drained in behind it until an end is reached
//!   (`buffered_ghost_message_queue.cpp:173` `delaySubsequence`);
//! - when a Ghost is destroyed, a parked `createGhost` of the *next*
//!   lifespan is promoted and played
//!   (`entity.cpp:3007` `playNewLifespanFor`,
//!   `buffered_ghost_messages_for_entity.cpp:94` `playNewLifespan`).
//!
//! The acceptance, "交付顺序与去重结果吻合", is that messages buffered
//! during a switch are delivered exactly once, in the order the
//! subsequences chain together — never dropped, never duplicated.
//!
//! Reference:
//! - `buffered_ghost_messages.hpp:13-35` — the class doc: the ghost
//!   lifespan is split into subsequences, chained by the sign-off.
//! - `buffered_ghost_messages.hpp:56-61` — start/end classification.
//! - `buffered_ghost_message_queue.cpp:38` `add` — while delaying, new
//!   messages join the parked list; otherwise the live queue, whose front
//!   is always a subsequence start.
//! - `buffered_ghost_message_queue.cpp:144` `playSubsequence` — replay up
//!   to and including the first end, promoting parked messages first.
//! - `buffered_ghost_message_queue.cpp:173` `delaySubsequence` — park a
//!   new start and drain the live queue in behind it until an end.
//! - `buffered_ghost_messages_for_entity.cpp:94` `playNewLifespan` —
//!   promote every queue, then play the first whose front is a
//!   `createGhost`.
//! - `entity.cpp:4943` — an out-of-order `ghostSetReal` is parked on the
//!   sender's queue instead of switching the routing.
//! - `entity.cpp:4975-4982` — `ghostSetNextReal` records the pending Real
//!   and replays that sender's buffered subsequence.
//! - `entity.cpp:3007` — after a Ghost is destroyed, any parked
//!   `createGhost` of the next lifespan is played.
//! - `space.cpp:236-256` — a `createGhost` for an existing entity is
//!   buffered on the sender's queue (parked when it starts a new
//!   lifespan).

use std::collections::{BTreeMap, VecDeque};

use crate::ghost_data::Address;

/// Which CellApp interface message a buffered entry carries
/// (`cellapp_interface.hpp`; the ids classify subsequences).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhostMessageKind {
    /// `createGhost`: starts a ghost's whole lifespan.
    CreateGhost,
    /// `ghostSetReal`: starts a subsequence from the new Real.
    GhostSetReal,
    /// `delGhost`: ends the lifespan.
    DelGhost,
    /// `ghostSetNextReal`: ends a subsequence and chains to the next Real.
    GhostSetNextReal,
    /// Any other real→ghost message (`ghostedDataUpdate`,
    /// `ghostVolatileInfo`, the controller messages, ...).
    Other,
}

impl GhostMessageKind {
    /// `BufferedGhostMessages::isSubsequenceStart`
    /// (`buffered_ghost_messages.hpp:69`).
    pub fn is_subsequence_start(self) -> bool {
        matches!(
            self,
            GhostMessageKind::CreateGhost | GhostMessageKind::GhostSetReal
        )
    }

    /// `BufferedGhostMessages::isSubsequenceEnd`
    /// (`buffered_ghost_messages.hpp:74`).
    pub fn is_subsequence_end(self) -> bool {
        matches!(
            self,
            GhostMessageKind::DelGhost | GhostMessageKind::GhostSetNextReal
        )
    }

    /// `BufferedGhostMessage::isCreateGhostMessage` — only a `createGhost`
    /// starts a fresh lifespan (`buffered_ghost_message_factory.cpp`).
    pub fn is_create_ghost(self) -> bool {
        matches!(self, GhostMessageKind::CreateGhost)
    }
}

/// One buffered real→ghost message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostMessage {
    /// The interface message this entry replays.
    pub kind: GhostMessageKind,
    /// A send-order label, so tests can assert delivery order and
    /// de-duplication (the payload stands in for the serialised stream).
    pub seq: u32,
    /// `args.nextRealAddr` — only set on `GhostSetNextReal`.
    pub next_addr: Option<Address>,
}

impl GhostMessage {
    /// A message of `kind` with no chain target.
    pub fn new(kind: GhostMessageKind, seq: u32) -> Self {
        GhostMessage {
            kind,
            seq,
            next_addr: None,
        }
    }

    /// A buffered `createGhost` (`BufferedCreateGhostMessage`).
    pub fn create_ghost(seq: u32) -> Self {
        GhostMessage::new(GhostMessageKind::CreateGhost, seq)
    }

    /// A buffered `ghostSetReal` (`BufferedGhostSetRealMessage`).
    pub fn ghost_set_real(seq: u32) -> Self {
        GhostMessage::new(GhostMessageKind::GhostSetReal, seq)
    }

    /// A buffered `delGhost`.
    pub fn del_ghost(seq: u32) -> Self {
        GhostMessage::new(GhostMessageKind::DelGhost, seq)
    }

    /// A buffered `ghostSetNextReal` pointing at the next Real.
    pub fn ghost_set_next_real(seq: u32, next: Address) -> Self {
        GhostMessage {
            kind: GhostMessageKind::GhostSetNextReal,
            seq,
            next_addr: Some(next),
        }
    }

    /// Any other real→ghost message.
    pub fn other(seq: u32) -> Self {
        GhostMessage::new(GhostMessageKind::Other, seq)
    }
}

/// The buffered messages from one CellApp for one entity
/// (`BufferedGhostMessageQueue`): a live subsequence plus an optional
/// parked one.
#[derive(Debug, Clone, Default, PartialEq)]
struct Queue {
    /// `messages_`: the live messages, front is a subsequence start while
    /// not delaying.
    messages: VecDeque<GhostMessage>,
    /// `pDelayedMessages_`: the parked out-of-order subsequence, if any.
    delayed: Option<VecDeque<GhostMessage>>,
}

impl Queue {
    /// `BufferedGhostMessageQueue::isEmpty` — only the live list counts;
    /// parked messages are invisible to `hasMessagesFor`
    /// (`buffered_ghost_message_queue.hpp:31`).
    fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// `isDelaying` (`buffered_ghost_message_queue.cpp:197`): parked
    /// messages exist and the last one is not yet a subsequence end.
    fn is_delaying(&self) -> bool {
        self.delayed
            .as_ref()
            .is_some_and(|d| !d.back().is_some_and(|m| m.kind.is_subsequence_end()))
    }

    /// `isFrontSubsequenceStart`.
    fn is_front_subsequence_start(&self) -> bool {
        self.messages
            .front()
            .is_some_and(|m| m.kind.is_subsequence_start())
    }

    /// `isFrontSubsequenceEnd`.
    fn is_front_subsequence_end(&self) -> bool {
        self.messages
            .front()
            .is_some_and(|m| m.kind.is_subsequence_end())
    }

    /// `isFrontCreateGhost`.
    fn is_front_create_ghost(&self) -> bool {
        self.messages
            .front()
            .is_some_and(|m| m.kind.is_create_ghost())
    }

    /// `add` (`buffered_ghost_message_queue.cpp:45`): while delaying, new
    /// messages join the parked list; otherwise they join the live queue,
    /// whose front stays a subsequence start.
    fn add(&mut self, msg: GhostMessage) {
        if self.is_delaying() {
            self.delayed
                .as_mut()
                .expect("delaying implies parked messages")
                .push_back(msg);
        } else {
            self.messages.push_back(msg);
            debug_assert!(
                self.is_front_subsequence_start(),
                "a live queue's front must be a subsequence start"
            );
        }
    }

    /// `popFront`.
    fn pop_front(&mut self) -> Option<GhostMessage> {
        self.messages.pop_front()
    }

    /// `promoteDelayedSubsequences` (`buffered_ghost_message_queue.cpp:127`):
    /// move the parked subsequence to the front of the live queue.
    fn promote_delayed_subsequences(&mut self) {
        if let Some(delayed) = self.delayed.take() {
            for msg in delayed.into_iter().rev() {
                self.messages.push_front(msg);
            }
        }
    }

    /// `delaySubsequence` (`buffered_ghost_message_queue.cpp:173`): park
    /// `first` (a subsequence start) and drain the live queue in behind it
    /// until a subsequence end is reached.
    fn delay_subsequence(&mut self, first: GhostMessage) {
        debug_assert!(
            first.kind.is_subsequence_start(),
            "only a subsequence start can be delayed"
        );

        let delayed = self.delayed.get_or_insert_with(VecDeque::new);
        delayed.push_back(first);

        // Delay this old (unexpected) subsequence: drain until the parked
        // list's back is an end, which stops `is_delaying`.
        while !self.messages.is_empty() && self.is_delaying() {
            if let Some(msg) = self.messages.pop_front() {
                self.delayed
                    .as_mut()
                    .expect("delaying implies parked messages")
                    .push_back(msg);
            }
        }
    }

    /// Whether the queue is fully drained: no live messages and nothing
    /// parked (`playFront`'s "became empty" test,
    /// `buffered_ghost_message_queue.cpp:99`).
    fn is_drained(&self) -> bool {
        self.messages.is_empty() && self.delayed.is_none()
    }
}

/// The per-source-address queues of one entity
/// (`BufferedGhostMessagesForEntity`).
#[derive(Debug, Clone, Default)]
struct EntityBuffers {
    /// `queues_`: keyed by the source address.
    queues: BTreeMap<Address, Queue>,
}

impl EntityBuffers {
    /// `findOrCreate` + `add` (`buffered_ghost_messages_for_entity.cpp:47`).
    fn add(&mut self, src: Address, msg: GhostMessage) {
        self.queues.entry(src).or_default().add(msg);
    }

    /// `delaySubsequence` (`buffered_ghost_messages_for_entity.cpp:155`).
    fn delay_subsequence(&mut self, src: Address, first: GhostMessage) {
        self.queues.entry(src).or_default().delay_subsequence(first);
    }

    /// `hasMessagesFrom` (`buffered_ghost_messages_for_entity.cpp:131`).
    fn has_messages_from(&self, src: Address) -> bool {
        self.queues.get(&src).is_some_and(|q| !q.is_empty())
    }

    /// `isDelayingMessagesFor` (`buffered_ghost_messages_for_entity.cpp:143`).
    fn is_delaying_from(&self, src: Address) -> bool {
        self.queues.get(&src).is_some_and(Queue::is_delaying)
    }
}

/// All buffered real→ghost messages on one CellApp
/// (`BufferedGhostMessages`), keyed by entity id.
#[derive(Debug, Clone, Default)]
pub struct BufferedGhostMessages {
    /// `map_`: entity id → the entity's per-sender queues.
    entities: BTreeMap<u32, EntityBuffers>,
}

impl BufferedGhostMessages {
    /// An empty buffer set.
    pub fn new() -> Self {
        Self::default()
    }

    /// `isEmpty` (`buffered_ghost_messages.hpp:83`).
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// `add` (`buffered_ghost_messages.cpp:11`): a GHOST_ONLY message that
    /// arrived out of sender order joins the sender's queue
    /// (`message_handlers.cpp:140-146`).
    pub fn add(&mut self, entity_id: u32, src: Address, msg: GhostMessage) {
        self.entities.entry(entity_id).or_default().add(src, msg);
    }

    /// `delaySubsequence` (`buffered_ghost_messages.cpp:101`): an
    /// out-of-order subsequence start parks on the sender's queue
    /// (`entity.cpp:4943`, `space.cpp:255`).
    pub fn delay_subsequence(&mut self, entity_id: u32, src: Address, first: GhostMessage) {
        self.entities
            .entry(entity_id)
            .or_default()
            .delay_subsequence(src, first);
    }

    /// `hasMessagesFor` (`buffered_ghost_messages.cpp:64`).
    pub fn has_messages_for(&self, entity_id: u32, src: Address) -> bool {
        self.entities
            .get(&entity_id)
            .is_some_and(|e| e.has_messages_from(src))
    }

    /// `isDelayingMessagesFor` (`buffered_ghost_messages.cpp:82`).
    pub fn is_delaying_for(&self, entity_id: u32, src: Address) -> bool {
        self.entities
            .get(&entity_id)
            .is_some_and(|e| e.is_delaying_from(src))
    }

    /// `playSubsequenceFor` (`buffered_ghost_messages.cpp:22`): replay the
    /// subsequence buffered from `src`, following any `ghostSetNextReal`
    /// chain into the next Real's queue. Returns whether the entity's
    /// entry was fully drained (and removed).
    ///
    /// Every replayed message is handed to `deliver` exactly once, in
    /// order.
    pub fn play_subsequence_for(
        &mut self,
        entity_id: u32,
        src: Address,
        deliver: &mut dyn FnMut(&GhostMessage),
    ) -> bool {
        let drained = self.play_entity_queue(entity_id, src, deliver);
        if drained {
            self.entities.remove(&entity_id);
        }
        drained
    }

    /// `playNewLifespanFor` (`buffered_ghost_messages.cpp:42`, called from
    /// `entity.cpp:3007` once a Ghost is destroyed): promote every queue's
    /// parked messages, then play the first queue whose front is a
    /// `createGhost` — the next lifespan. Returns whether the entity's
    /// entry was fully drained (and removed).
    pub fn play_new_lifespan(
        &mut self,
        entity_id: u32,
        deliver: &mut dyn FnMut(&GhostMessage),
    ) -> bool {
        // The queues are visited in address order, promoting each until a
        // `createGhost` front is found
        // (`buffered_ghost_messages_for_entity.cpp:94-127`).
        let addrs: Vec<Address> = self
            .entities
            .get(&entity_id)
            .map(|e| e.queues.keys().copied().collect())
            .unwrap_or_default();

        for addr in addrs {
            let front_is_create = self
                .entities
                .get_mut(&entity_id)
                .and_then(|e| e.queues.get_mut(&addr))
                .map(|q| {
                    q.promote_delayed_subsequences();
                    q.is_front_create_ghost()
                });

            if front_is_create == Some(true) {
                let drained = self.play_entity_queue(entity_id, addr, deliver);
                if drained {
                    self.entities.remove(&entity_id);
                }
                return drained;
            }
        }

        false
    }

    /// The queue-level replay loop (`BufferedGhostMessageQueue::
    /// playSubsequence`, `buffered_ghost_message_queue.cpp:139`), with the
    /// `ghostSetNextReal` chaining (`entity.cpp:4981`) done at this level
    /// so the recursion can retake borrows between messages.
    ///
    /// Returns whether the entity's entry ended up fully drained.
    fn play_entity_queue(
        &mut self,
        entity_id: u32,
        src: Address,
        deliver: &mut dyn FnMut(&GhostMessage),
    ) -> bool {
        loop {
            // A missing queue plays nothing
            // (`buffered_ghost_messages_for_entity.cpp:59-66`).
            let exists = self
                .entities
                .get(&entity_id)
                .is_some_and(|e| e.queues.contains_key(&src));
            if !exists {
                return false;
            }

            // Promote parked messages, then pop the front one.
            let step = self.entities.get_mut(&entity_id).and_then(|e| {
                let q = e.queues.get_mut(&src)?;
                q.promote_delayed_subsequences();
                if q.is_empty() {
                    return None;
                }
                let finished = q.is_front_subsequence_end();
                let msg = q.pop_front().expect("front checked non-empty");
                Some((msg, finished))
            });

            let (msg, finished) = match step {
                Some(step) => step,
                // Empty after promoting: nothing to play, the queue stays.
                None => return false,
            };

            deliver(&msg);

            // An end that is `ghostSetNextReal` chains straight into the
            // next Real's subsequence (`entity.cpp:4975`).
            if msg.kind == GhostMessageKind::GhostSetNextReal
                && let Some(next) = msg.next_addr
            {
                // The chain may drain every queue of this entity.
                let _ = self.play_entity_queue(entity_id, next, deliver);
            }

            // `playFront` re-checks emptiness *after* play, because playing
            // may have modified the queue
            // (`buffered_ghost_message_queue.cpp:99-110`).
            if !self.entities.contains_key(&entity_id) {
                // The chain removed the whole entry.
                return true;
            }

            let drained = self
                .entities
                .get(&entity_id)
                .and_then(|e| e.queues.get(&src))
                .is_some_and(Queue::is_drained);

            if drained {
                let now_empty = {
                    let e = self
                        .entities
                        .get_mut(&entity_id)
                        .expect("entry checked above");
                    e.queues.remove(&src);
                    e.queues.is_empty()
                };
                if now_empty {
                    self.entities.remove(&entity_id);
                }
                // The caller deletes the entry only when *every* queue of the
                // entity is empty
                // (`buffered_ghost_messages_for_entity.cpp:77-80`).
                return now_empty;
            }

            // A subsequence end stops the replay unless the queue emptied
            // first (`buffered_ghost_message_queue.cpp:146-166`).
            if finished {
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects the sequence labels of the delivered messages, so a test
    /// can assert both the order and that each arrives once.
    #[derive(Default)]
    struct DeliveryLog(Vec<u32>);

    impl DeliveryLog {
        fn record(&mut self, msg: &GhostMessage) {
            self.0.push(msg.seq);
        }
    }

    #[test]
    fn messages_from_one_sender_play_in_order_and_each_once() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);

        // One full lifespan: create, updates, delete.
        buffers.add(42, a, GhostMessage::create_ghost(1));
        buffers.add(42, a, GhostMessage::other(2));
        buffers.add(42, a, GhostMessage::other(3));
        buffers.add(42, a, GhostMessage::del_ghost(4));

        assert!(buffers.has_messages_for(42, a));
        assert!(!buffers.is_delaying_for(42, a));

        let mut log = DeliveryLog::default();
        assert!(buffers.play_subsequence_for(42, a, &mut |m| log.record(m)));

        assert_eq!(
            log.0,
            vec![1, 2, 3, 4],
            "each buffered message delivered once, in send order"
        );
        assert!(buffers.is_empty(), "the drained entity entry is removed");
    }

    /// The acceptance: messages buffered from the *next* Real during an
    /// offload are delivered in chain order — after the current Real's
    /// sign-off, before anything else — and exactly once.
    #[test]
    fn a_switch_delivers_the_new_reals_subsequence_in_chain_order() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);
        let b = Address(200);

        // Real B's messages arrive while real A still owns the ghost: they
        // are buffered on B's queue (message_handlers.cpp:140-146).
        buffers.add(42, b, GhostMessage::ghost_set_real(3));
        buffers.add(42, b, GhostMessage::other(4));

        // Real A's in-flight subsequence, ending in the sign-off.
        buffers.add(42, a, GhostMessage::ghost_set_real(1));
        buffers.add(42, a, GhostMessage::ghost_set_next_real(2, b));

        let mut log = DeliveryLog::default();
        assert!(buffers.play_subsequence_for(42, a, &mut |m| log.record(m)));

        assert_eq!(
            log.0,
            vec![1, 2, 3, 4],
            "A's subsequence plays first, the sign-off chains into B's"
        );
        assert!(buffers.is_empty(), "both queues drained, entry removed");
    }

    /// The acceptance's dedup half: a message buffered during the switch is
    /// neither dropped nor replayed twice, and once drained it no longer
    /// counts as buffered.
    #[test]
    fn a_message_buffered_during_the_switch_is_delivered_exactly_once() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);
        let b = Address(200);

        // B's update arrives early and is buffered.
        buffers.add(7, b, GhostMessage::ghost_set_real(1));
        buffers.add(7, b, GhostMessage::other(2));
        assert!(buffers.has_messages_for(7, b));

        // A hands over.
        buffers.add(7, a, GhostMessage::ghost_set_real(3));
        buffers.add(7, a, GhostMessage::ghost_set_next_real(4, b));

        let mut log = DeliveryLog::default();
        buffers.play_subsequence_for(7, a, &mut |m| log.record(m));

        let b_deliveries = log.0.iter().filter(|seq| **seq == 2).count();
        assert_eq!(b_deliveries, 1, "the buffered update is delivered once");
        assert_eq!(
            log.0,
            vec![3, 4, 1, 2],
            "A's subsequence first, then B's, each message once"
        );
        assert!(
            !buffers.has_messages_for(7, b),
            "the drained queue no longer counts as buffered"
        );
        assert!(buffers.is_empty());
    }

    /// `delaySubsequence` (`buffered_ghost_message_queue.cpp:173`): the new
    /// start is parked and the live queue's messages are drained in behind
    /// it; the parked list stays open until a subsequence end joins it.
    #[test]
    fn delaying_parks_a_new_start_behind_the_current_subsequence() {
        let mut buffers = BufferedGhostMessages::new();
        let b = Address(200);
        let c = Address(300);

        // Queue B holds an in-flight subsequence whose end has not arrived.
        buffers.add(9, b, GhostMessage::ghost_set_real(1));
        buffers.add(9, b, GhostMessage::other(2));

        // An out-of-order start arrives (entity.cpp:4943) and parks; the
        // live messages drain in behind it.
        buffers.delay_subsequence(9, b, GhostMessage::ghost_set_real(3));
        assert!(
            buffers.is_delaying_for(9, b),
            "the parked list is open while its back is not an end"
        );
        assert!(
            !buffers.has_messages_for(9, b),
            "parked messages do not count as buffered"
        );

        // Further messages join the parked list while it stays open.
        buffers.add(9, b, GhostMessage::other(4));

        // The subsequence end arrives and closes the parked list.
        buffers.add(9, b, GhostMessage::ghost_set_next_real(5, c));
        assert!(!buffers.is_delaying_for(9, b), "an end closes the list");

        let mut log = DeliveryLog::default();
        assert!(buffers.play_subsequence_for(9, b, &mut |m| log.record(m)));
        assert_eq!(
            log.0,
            vec![3, 1, 2, 4, 5],
            "the parked start leads, then the drained messages in order, \
             then the end"
        );
        assert!(buffers.is_empty());
    }

    /// The out-of-order `ghostSetReal` flow (`entity.cpp:4943`): the
    /// message parks until the preceding subsequence's sign-off chains into
    /// the sender's queue and replays it.
    #[test]
    fn an_out_of_order_real_is_parked_until_the_sign_off_chains_to_it() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);
        let b = Address(200);

        // B's ghostSetReal arrives out of order and parks on B's queue
        // instead of switching the routing (entity.cpp:4943).
        buffers.delay_subsequence(42, b, GhostMessage::ghost_set_real(3));
        // Later messages from B join the parked list.
        buffers.add(42, b, GhostMessage::other(4));

        // A's subsequence ends with the sign-off to B.
        buffers.add(42, a, GhostMessage::ghost_set_real(1));
        buffers.add(42, a, GhostMessage::ghost_set_next_real(2, b));

        let mut log = DeliveryLog::default();
        assert!(buffers.play_subsequence_for(42, a, &mut |m| log.record(m)));

        assert_eq!(
            log.0,
            vec![1, 2, 3, 4],
            "the sign-off chains into B and replays the parked subsequence"
        );
        assert!(buffers.is_empty());
    }

    /// After a Ghost is destroyed, a parked `createGhost` of the next
    /// lifespan is promoted and played (`entity.cpp:3007`,
    /// `space.cpp:249-256`).
    #[test]
    fn destroying_a_ghost_plays_the_parked_next_lifespan() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);
        let b = Address(200);

        // The old lifespan ends: real A's subsequence ends in delGhost.
        buffers.add(11, a, GhostMessage::ghost_set_real(1));
        buffers.add(11, a, GhostMessage::del_ghost(2));

        // A createGhost for the next lifespan arrives while the old ghost
        // still exists and parks on the new real's queue.
        buffers.delay_subsequence(11, b, GhostMessage::create_ghost(3));
        // Later messages from the new real join the parked list.
        buffers.add(11, b, GhostMessage::other(4));
        assert!(buffers.is_delaying_for(11, b));

        let mut log = DeliveryLog::default();

        // The old subsequence plays to its delGhost end; the parked next
        // lifespan keeps the entity entry alive.
        assert!(!buffers.play_subsequence_for(11, a, &mut |m| log.record(m)));
        assert_eq!(log.0, vec![1, 2]);

        // Entity::destroy then plays the parked next lifespan.
        assert!(buffers.play_new_lifespan(11, &mut |m| log.record(m)));
        assert_eq!(
            log.0,
            vec![1, 2, 3, 4],
            "the parked createGhost leads the replay"
        );
        assert!(buffers.is_empty());
    }

    /// `playNewLifespan` does nothing when no queue starts with a
    /// `createGhost` (`buffered_ghost_messages_for_entity.cpp:111-128`).
    #[test]
    fn a_new_lifespan_needs_a_create_ghost_front() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);

        buffers.add(11, a, GhostMessage::ghost_set_real(1));
        buffers.add(11, a, GhostMessage::del_ghost(2));

        let mut log = DeliveryLog::default();
        assert!(!buffers.play_new_lifespan(11, &mut |m| log.record(m)));
        assert!(log.0.is_empty(), "no createGhost front, no replay");
        assert!(
            !buffers.is_empty(),
            "the queues are untouched for a later lifespan"
        );
    }

    /// Playing only up to the first subsequence end leaves the rest of the
    /// queue for the next hand-over.
    #[test]
    fn a_subsequence_end_stops_the_replay_within_one_queue() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);
        let b = Address(200);

        // Two complete subsequences queued from A (two visits).
        buffers.add(5, a, GhostMessage::ghost_set_real(1));
        buffers.add(5, a, GhostMessage::ghost_set_next_real(2, b));
        buffers.add(5, a, GhostMessage::ghost_set_real(3));
        buffers.add(5, a, GhostMessage::del_ghost(4));

        let mut log = DeliveryLog::default();
        // B's queue is empty: the chain from the sign-off plays nothing.
        assert!(
            !buffers.play_subsequence_for(5, a, &mut |m| log.record(m)),
            "the queue still holds the second subsequence"
        );
        assert_eq!(log.0, vec![1, 2], "the replay stops after the first end");
        assert!(
            buffers.has_messages_for(5, a),
            "the remaining subsequence stays buffered"
        );

        // The next hand-over replays the rest.
        assert!(buffers.play_subsequence_for(5, a, &mut |m| log.record(m)));
        assert_eq!(log.0, vec![1, 2, 3, 4]);
        assert!(buffers.is_empty());
    }

    /// A `delGhost` end also stops a replay, and ends the entity's entry
    /// when it is the last message.
    #[test]
    fn a_del_ghost_end_stops_the_replay() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);

        buffers.add(5, a, GhostMessage::ghost_set_real(1));
        buffers.add(5, a, GhostMessage::del_ghost(2));

        let mut log = DeliveryLog::default();
        assert!(buffers.play_subsequence_for(5, a, &mut |m| log.record(m)));
        assert_eq!(log.0, vec![1, 2]);
        assert!(buffers.is_empty());
    }

    /// When the sign-off chains into a queue that drains *every* remaining
    /// message, the recursive play removes the entity entry and the outer
    /// replay sees it is gone and stops (`playFront`'s ref-counted guard,
    /// `buffered_ghost_message_queue.cpp:99-110`).
    #[test]
    fn a_chain_that_drains_everything_removes_the_entry_mid_replay() {
        let mut buffers = BufferedGhostMessages::new();
        let a = Address(100);

        // One queue whose sign-off chains back to the same address, with a
        // trailing message for the recursive play to drain.
        buffers.add(7, a, GhostMessage::ghost_set_real(1));
        buffers.add(7, a, GhostMessage::ghost_set_next_real(2, a));
        buffers.add(7, a, GhostMessage::other(3));

        let mut log = DeliveryLog::default();
        assert!(buffers.play_subsequence_for(7, a, &mut |m| log.record(m)));
        assert_eq!(log.0, vec![1, 2, 3], "the chain delivered every message");
        assert!(buffers.is_empty());
    }
}
