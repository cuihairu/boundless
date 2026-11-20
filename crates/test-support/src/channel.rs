//! Deterministic fault channel for message-level tests (BW-18-03-02+).
//!
//! The reference injects loss with `MangleFilter`
//! (`lib/network/unit_test/test_mangle.cpp`), which drops packets by wall
//! clock probability (`g_mangleProb = 0.05`) -- a run cannot name which
//! packet will vanish, so retransmission behaviour is not reproducible.
//! Here the test *scripts* each packet's fate by send index: dropping a
//! designated packet deterministically forces the expected retransmission,
//! and a seeded random mode mirrors the reference's probabilistic filter
//! without losing reproducibility.
//!
//! The channel is generic over the message type; the mercury lane wires its
//! real packets through it at BW-02+, and until then tests demonstrate the
//! send/lose/retransmit loop with plain values.

use crate::rng::SeededRng;

/// What becomes of one sent packet, identified by its send index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Deliver on the next pump.
    Deliver,
    /// Never deliver on the wire. The sender is expected to notice and
    /// retransmit -- the channel records the packet in `dropped()`.
    Drop,
    /// Deliver on the first pump at or after `until_nanos` on the virtual
    /// clock ([`crate::clock::ManualClock`] stamps).
    Delayed { until_nanos: u64 },
    /// Deliver only after packet `anchor` has delivered -- the scripted
    /// reorder primitive: `script(a, Fate::After(b))` swaps `a` behind `b`.
    /// An anchor that never delivers (dropped) holds the packet back.
    After(usize),
}

/// A scripted channel: packets are sent in order and delivered by their
/// scripted fate, deterministically.
#[derive(Debug)]
pub struct ScriptedChannel<T> {
    /// One fate per send index.
    schedule: Vec<Fate>,
    /// Packets still awaiting delivery, by send index.
    in_flight: Vec<Option<T>>,
    /// Packets the channel dropped, in drop order.
    dropped: Vec<T>,
    /// Send indices delivered so far, in delivery order.
    delivered: Vec<usize>,
}

impl<T> Default for ScriptedChannel<T> {
    fn default() -> Self {
        Self {
            schedule: Vec::new(),
            in_flight: Vec::new(),
            dropped: Vec::new(),
            delivered: Vec::new(),
        }
    }
}

impl<T> ScriptedChannel<T> {
    /// An empty channel.
    pub fn new() -> Self {
        Self::default()
    }

    /// Send a packet; its fate starts as [`Fate::Deliver`] and can be
    /// re-scripted with [`ScriptedChannel::script`] until it delivers.
    pub fn send(&mut self, packet: T) -> usize {
        let index = self.schedule.len();
        self.schedule.push(Fate::Deliver);
        self.in_flight.push(Some(packet));
        index
    }

    /// Re-script the fate of a sent packet.
    ///
    /// # Panics
    /// If `index` is not a sent packet.
    pub fn script(&mut self, index: usize, fate: Fate) {
        assert!(index < self.schedule.len(), "no packet {index}");
        self.schedule[index] = fate;
    }

    /// Script `index` to drop exactly when `rng` draws against the share
    /// `numerator`/`denominator` -- the seeded stand-in for the reference's
    /// probabilistic `MangleFilter`, reproducible because the rng is.
    pub fn script_by_chance(
        &mut self,
        index: usize,
        rng: &mut SeededRng,
        numerator: u64,
        denominator: u64,
    ) {
        if rng.chance(numerator, denominator) {
            self.script(index, Fate::Drop);
        }
    }

    /// Deliver every in-flight packet whose fate allows it *now*; returns
    /// the packets in delivery order. The `now` parameter is the virtual
    /// clock stamp consumed by [`Fate::Delayed`]. Constraints resolve to a
    /// fixpoint within one pump, so a reorder chain settles in the scripted
    /// order.
    pub fn pump(&mut self, now: u64) -> Vec<T> {
        let mut out = Vec::new();
        let mut delivered_this_pump: Vec<usize> = Vec::new();
        loop {
            let mut progressed = false;
            for index in 0..self.schedule.len() {
                if self.in_flight[index].is_none() {
                    continue;
                }
                let ready = match self.schedule[index] {
                    Fate::Deliver | Fate::Drop => true,
                    Fate::Delayed { until_nanos } => now >= until_nanos,
                    Fate::After(anchor) => {
                        anchor != index
                            && (self.delivered.contains(&anchor)
                                || delivered_this_pump.contains(&anchor))
                    }
                };
                if !ready {
                    continue;
                }
                if let Some(packet) = self.in_flight[index].take() {
                    match self.schedule[index] {
                        Fate::Drop => self.dropped.push(packet),
                        _ => {
                            self.delivered.push(index);
                            delivered_this_pump.push(index);
                            out.push(packet);
                        }
                    }
                    progressed = true;
                }
            }
            if !progressed {
                return out;
            }
        }
    }

    /// Packets the channel dropped, in drop order.
    pub fn dropped(&self) -> &[T] {
        &self.dropped
    }

    /// Send indices delivered so far, in delivery order.
    pub fn delivered(&self) -> &[usize] {
        &self.delivered
    }

    /// True while packet `index` has neither delivered nor dropped.
    pub fn is_in_flight(&self, index: usize) -> bool {
        self.in_flight.get(index).is_some_and(|slot| slot.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three packets flow; the scripted one vanishes and the rest deliver
    /// in order.
    #[test]
    fn designated_packets_drop_and_the_rest_deliver() {
        let mut channel: ScriptedChannel<String> = ScriptedChannel::new();
        let first = channel.send("packet-0".to_string());
        let second = channel.send("packet-1".to_string());
        let third = channel.send("packet-2".to_string());
        channel.script(second, Fate::Drop);

        let delivered = channel.pump(0);
        assert_eq!(delivered, ["packet-0", "packet-2"]);
        assert_eq!(channel.delivered(), [first, third]);
        assert_eq!(channel.dropped(), ["packet-1"]);
        assert!(!channel.is_in_flight(second));
    }

    /// The acceptance loop: a scripted drop forces exactly one
    /// retransmission, and the retransmitted copy arrives.
    #[test]
    fn a_designated_drop_triggers_the_expected_retransmission() {
        let mut channel: ScriptedChannel<u32> = ScriptedChannel::new();
        channel.send(0);
        let doomed = channel.send(1);
        channel.send(2);
        channel.script(doomed, Fate::Drop);

        // Receiver sees 0, 2 -- 1 is missing.
        let received = channel.pump(0);
        assert_eq!(received, [0, 2]);
        assert_eq!(channel.dropped(), [1]);

        // The sender's retransmit logic notices the gap and re-sends 1;
        // the fresh copy goes through untouched.
        let retransmit = channel.send(1);
        let received = channel.pump(0);
        assert_eq!(received, [1]);
        assert_eq!(channel.delivered(), [0, 2, retransmit]);
    }

    /// Only scripted fates drop: an untouched channel delivers everything.
    #[test]
    fn an_unscripted_channel_delivers_everything() {
        let mut channel: ScriptedChannel<u8> = ScriptedChannel::new();
        for value in 0..5 {
            channel.send(value);
        }
        assert_eq!(channel.pump(0).len(), 5);
        assert!(channel.dropped().is_empty());
    }

    /// The seeded chance mode reproduces the reference's probabilistic
    /// filter deterministically: the same seed drops the same packets,
    /// another seed drops a different set.
    #[test]
    fn seeded_chance_scripting_is_reproducible() {
        let dropped = |seed: u64| -> Vec<u8> {
            let mut channel: ScriptedChannel<u8> = ScriptedChannel::new();
            let mut rng = SeededRng::new(seed);
            for value in 0..8 {
                let index = channel.send(value);
                channel.script_by_chance(index, &mut rng, 1, 2);
            }
            channel.pump(0);
            channel.dropped().to_vec()
        };

        let seven = dropped(7);
        assert!(
            !seven.is_empty() && seven.len() < 8,
            "a 1/2 share dropped: {seven:?}"
        );
        assert_eq!(seven, dropped(7), "the same seed drops the same packets");
        assert_ne!(seven, dropped(8), "another seed drops another set");
    }

    /// Delayed packets hold back until the virtual clock reaches their
    /// stamp, then deliver in send order with everything else.
    #[test]
    fn delayed_packets_deliver_when_the_clock_advances() {
        let mut channel: ScriptedChannel<char> = ScriptedChannel::new();
        let late = channel.send('b');
        channel.send('a');
        channel.send('c');
        channel.script(
            late,
            Fate::Delayed {
                until_nanos: 50 * crate::clock::NANOS_PER_MS,
            },
        );

        let early = channel.pump(0);
        assert_eq!(early, ['a', 'c']);
        assert!(channel.is_in_flight(late), "not yet due");

        let still_early = channel.pump(49 * crate::clock::NANOS_PER_MS);
        assert!(still_early.is_empty(), "one millisecond short");

        let on_time = channel.pump(50 * crate::clock::NANOS_PER_MS);
        assert_eq!(on_time, ['b']);
        assert_eq!(channel.delivered(), [1, 2, 0]);
    }

    /// Anchored delivery swaps the scripted packet behind its anchor:
    /// the specified order, not the send order.
    #[test]
    fn anchored_packets_deliver_after_their_anchor() {
        let mut channel: ScriptedChannel<u8> = ScriptedChannel::new();
        for value in 0..3 {
            channel.send(value);
        }
        channel.script(1, Fate::After(2));

        let delivered = channel.pump(0);
        assert_eq!(delivered, [0, 2, 1]);
        assert_eq!(channel.delivered(), [0, 2, 1]);
    }

    /// A chain of anchors settles within one pump in the fully scripted
    /// order.
    #[test]
    fn an_anchor_chain_delivers_in_the_scripted_order() {
        let mut channel: ScriptedChannel<u8> = ScriptedChannel::new();
        for value in 0..3 {
            channel.send(value);
        }
        // 0 waits for 1, 1 waits for 2: delivery runs 2, 1, 0.
        channel.script(0, Fate::After(1));
        channel.script(1, Fate::After(2));

        let delivered = channel.pump(0);
        assert_eq!(delivered, [2, 1, 0]);
    }

    /// Delay plus reorder resolve together deterministically: the same
    /// script and clock steps give the same delivery order every time.
    #[test]
    fn delay_and_reorder_stay_deterministic_together() {
        let scenario = || {
            let mut channel: ScriptedChannel<u8> = ScriptedChannel::new();
            for value in 0..4 {
                channel.send(value);
            }
            channel.script(
                1,
                Fate::Delayed {
                    until_nanos: 10 * crate::clock::NANOS_PER_MS,
                },
            );
            channel.script(3, Fate::After(2));
            let mut order = Vec::new();
            order.extend(channel.pump(0));
            order.extend(channel.pump(5 * crate::clock::NANOS_PER_MS));
            order.extend(channel.pump(10 * crate::clock::NANOS_PER_MS));
            (order, channel.delivered().to_vec())
        };
        assert_eq!(scenario(), scenario());
        let (order, indices) = scenario();
        // Pump one: 0 then 2 (3 anchored behind it). Pump three: 1's delay
        // expires. 3 lands with pump one's second pass.
        assert_eq!(order, [0, 2, 3, 1]);
        assert_eq!(indices, [0, 2, 3, 1]);
    }

    /// A packet anchored to a dropped packet never delivers: the hold is
    /// relative to actual delivery.
    #[test]
    fn an_anchor_behind_a_dropped_packet_stays_back() {
        let mut channel: ScriptedChannel<u8> = ScriptedChannel::new();
        channel.send(0);
        let follower = channel.send(1);
        channel.send(2);
        channel.script(1, Fate::After(2));
        channel.script(2, Fate::Drop);

        let delivered = channel.pump(0);
        assert_eq!(delivered, [0]);
        assert!(channel.is_in_flight(follower), "anchor was dropped");
    }
}
