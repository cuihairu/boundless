//! `lib/network/packet_receiver_stats.hpp`: the per-socket receive
//! statistics the packet receiver threads onto every datagram it handles.
//!
//! BW-02-03-05 scope: only the counters `addToReceiveWindow` touches
//! (`numDuplicatePacketsReceived_`). The rate-of-change `Stat` wrappers,
//! the one-second gather timer and the socket-queue sampling belong to the
//! EventDispatcher item (BW-02-01-05), which extends this type.

/// `PacketReceiverStats` (packet_receiver_stats.hpp:18): currently the
/// duplicate counter `addToReceiveWindow` increments. The remaining
/// counters (`numPacketsReceived_`, `numBundlesReceived_`, ...) land with
/// BW-02-01-05.
#[derive(Default)]
pub struct PacketReceiverStats {
    num_duplicate_packets_received: u32,
}

impl PacketReceiverStats {
    /// `PacketReceiverStats()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `incDuplicatePackets()` (packet_receiver_stats.hpp:24): a packet
    /// already seen below `inSeqAt_`, or already sitting in a buffered
    /// slot, arrived again.
    pub fn inc_duplicate_packets(&mut self) {
        self.num_duplicate_packets_received += 1;
    }

    /// `numDuplicatePacketsReceived()`.
    pub fn num_duplicate_packets_received(&self) -> u32 {
        self.num_duplicate_packets_received
    }
}
