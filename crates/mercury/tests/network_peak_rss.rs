//! OPT-02-01-02: the process-level peak-memory record for the wire
//! layer.
//!
//! The allocation-count harness (`network_alloc_counts.rs`) pins heap
//! counts and peak *live* bytes for the protocol faces; this record is
//! the other half of the acceptance -- peak resident memory of a real
//! sustained wire transfer, kernel buffers and allocator slack
//! included. It is its own test binary so no sibling test's footprint
//! can raise the high-water mark before the transfer runs.
//!
//! The workload: 256 bursts of 1024 reliable 64-byte messages over one
//! loopback channel pair (16 MiB of payload), drained to the last ack.
//! 1024 packets in flight per burst is a quarter of the internal
//! channel's 4096 window, which is where the send window, the unacked
//! clones and the receive window all sit at their working maximum.
//!
//! ```sh
//! cargo test -p mercury --test network_peak_rss -- --nocapture
//! ```

use std::cell::Cell;
use std::rc::Rc;

use foundation::stream::BinaryStream;
use mercury::bundle_dispatch::dispatch_messages;
use mercury::endpoint::Address;
use mercury::interface_element::{FIXED_LENGTH_MESSAGE, InterfaceElement};
use mercury::interface_table::InterfaceTable;
use mercury::interfaces::InputMessageHandler;
use mercury::misc::{CHANNEL_ID_NULL, Reason};
use mercury::network_interface::{InterfaceType, NetworkInterface};
use mercury::packet::HEADER_SIZE;
use mercury::packet::flags;
use mercury::packet_receiver::PacketSink;
use mercury::udp_bundle::{ReliableType, UdpBundle};
use mercury::udp_channel::{Traits, UdpChannel};
use mercury::unpacked_message_header::UnpackedMessageHeader;

/// A sink that routes every packet the interface accepts into the
/// dispatch walk, the way `processPacket` does.
struct DispatchSink {
    table: Rc<InterfaceTable>,
}

impl PacketSink for DispatchSink {
    fn process_packet(&self, addr: &Address, packet: &mut Packet) -> Reason {
        let first_request = if packet.has_flags(flags::HAS_REQUESTS) {
            Some(packet.first_request_offset())
        } else {
            None
        };
        let body = &mut packet.data_mut()[HEADER_SIZE..];
        dispatch_messages(&self.table, addr, body, first_request)
    }
}

use mercury::packet::Packet;

#[derive(Default)]
struct CountingDelivery(Rc<Cell<u64>>);

impl InputMessageHandler for CountingDelivery {
    fn handle_message(
        &self,
        _source: &Address,
        _header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        data.finish();
        self.0.set(self.0.get() + 1);
    }
}

/// One leg of the loopback pair, mirroring the bench's `Leg`.
struct Leg {
    interface: Rc<NetworkInterface>,
    channel: mercury::udp_channel::ChannelPtr,
    delivered: Rc<Cell<u64>>,
}

/// Two loopback legs with reliable channels pointing at each other; the
/// 1-threshold pushes every received ack straight back, which is what
/// drains the windows.
fn loopback_pair() -> (Leg, Leg) {
    let (iface_a, _recv_a) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
    let (iface_b, _recv_b) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
    let addr_a = iface_a.address();
    let addr_b = iface_b.address();

    let chan_a = UdpChannel::new(&iface_a, addr_b, Traits::Internal, 0.05, CHANNEL_ID_NULL);
    let chan_b = UdpChannel::new(&iface_b, addr_a, Traits::Internal, 0.05, CHANNEL_ID_NULL);
    chan_a.borrow_mut().set_push_unsent_acks_threshold(1);
    chan_b.borrow_mut().set_push_unsent_acks_threshold(1);

    fn leg(interface: Rc<NetworkInterface>, channel: mercury::udp_channel::ChannelPtr) -> Leg {
        let delivered = Rc::new(Cell::new(0u64));
        let table = Rc::new(InterfaceTable::new());
        table.serve(
            &InterfaceElement::new("fixed", 3, FIXED_LENGTH_MESSAGE, 64, None),
            Some(Rc::new(CountingDelivery(Rc::clone(&delivered))) as Rc<dyn InputMessageHandler>),
        );
        interface.set_packet_sink(Some(Rc::new(DispatchSink { table })));
        Leg {
            interface,
            channel,
            delivered,
        }
    }

    (
        leg(Rc::clone(&iface_a), chan_a),
        leg(Rc::clone(&iface_b), chan_b),
    )
}

/// `/proc/self/status`'s `VmHWM` line: the process's peak resident set
/// so far, in KiB.
fn peak_rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let value = rest
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse::<u64>()
                .expect("VmHWM value");
            return value;
        }
    }
    panic!("VmHWM missing from /proc/self/status");
}

#[test]
fn sustained_wire_transfer_peak_rss() {
    let (a, b) = loopback_pair();
    let element = InterfaceElement::new("fixed", 3, FIXED_LENGTH_MESSAGE, 64, None);

    const BURSTS: u32 = 256;
    const MESSAGES_PER_BURST: u32 = 1024;
    const PAYLOAD_BYTES: u64 = 64;
    let total_messages = BURSTS as u64 * MESSAGES_PER_BURST as u64;
    let total_payload = total_messages * PAYLOAD_BYTES;

    for _ in 0..BURSTS {
        for i in 0..MESSAGES_PER_BURST {
            let mut bundle = UdpBundle::new(0);
            bundle.start_message(&element, ReliableType::Driver);
            let chunk: [u8; 64] =
                std::array::from_fn(|j: usize| ((i as usize * 13 + j * 5) % 241) as u8);
            bundle.write_bytes(&chunk);
            UdpChannel::send_bundle(&a.channel, &mut bundle);
        }
        // Drain each burst before the next so the record's peak is the
        // working set of one burst's window, not 256 of them queued.
        let mut steps = 0;
        while a.channel.borrow().has_unacked_packets() {
            let _ = b.interface.poller().process_pending_events(0.1);
            let _ = a.interface.poller().process_pending_events(0.1);
            steps += 1;
            assert!(steps < 500, "burst failed to drain");
        }
    }

    assert_eq!(b.delivered.get(), total_messages, "every message arrived");
    let peak = peak_rss_kib();
    println!(
        "sustained wire transfer: {total_payload} bytes payload, {total_messages} messages, peak RSS {peak} KiB"
    );
    // The payload streams through in per-burst windows, so the peak is
    // the working set of one burst (window arrays, unacked clones,
    // socket buffers), not the transfer's total size -- 6 MiB-class for
    // a 16 MiB transfer. The floor just proves the process is the one
    // that ran the workload.
    assert!(peak > 1024, "peak RSS above a bare process floor");
}
