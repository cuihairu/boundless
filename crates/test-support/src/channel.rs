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
    /// clock stamp for the delayed fates added at BW-18-03-03.
    pub fn pump(&mut self, now: u64) -> Vec<T> {
        let _ = now;
        let mut out = Vec::new();
        for index in 0..self.schedule.len() {
            match self.schedule[index] {
                Fate::Deliver => {
                    if let Some(packet) = self.in_flight[index].take() {
                        self.delivered.push(index);
                        out.push(packet);
                    }
                }
                Fate::Drop => {
                    if let Some(packet) = self.in_flight[index].take() {
                        self.dropped.push(packet);
                    }
                }
            }
        }
        out
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
}
