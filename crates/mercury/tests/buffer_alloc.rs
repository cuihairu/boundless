//! OPT-02-01-02: buffer-reuse evaluation — allocations and peak memory
//! on the transport hot paths.
//!
//! A counting global allocator with a thread-local tracking flag counts
//! the calling thread's allocations and tracks live bytes, so each
//! fixture reports an allocation count, a cumulative byte count and a
//! live-bytes high-water mark relative to its window. The fixtures
//! measure the places a datagram-sized buffer is born or held:
//!
//! 1. `Packet::new` — the 1472-byte box every datagram buffer starts as.
//! 2. The receive loop — `PacketReceiver` recycles via `mem::take`, so
//!    each drained datagram pays one fresh `Packet` allocation.
//! 3. The channel loopback — a reliable/unreliable 4-byte message from
//!    `send_bundle` through the wire to a served message handler, the
//!    reference's whole send→recv→dispatch path.
//! 4. The reliable send window — unacked packets retain their `Rc<Packet>`
//!    until acked, so live bytes scale with window occupancy; the peak
//!    stays after the acks drain, which is the memory the window costs.
//! 5. The TCP send buffer — the reuse datapoint: one backing `Vec`, and
//!    steady-state writes should not allocate at all.
//!
//! Numbers are exactly reproducible on a given toolchain and recorded
//! through `BUFFER_ALLOC_OUT` as `metric<TAB>value` TSV rows (append;
//! pass an absolute path — cargo starts the test binary in the crate
//! root):
//!
//! ```sh
//! BUFFER_ALLOC_OUT=<abs path> cargo test -p mercury --test buffer_alloc
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use foundation::stream::BinaryStream;
use mercury::bundle_dispatch::dispatch_messages;
use mercury::endpoint::Endpoint;
use mercury::interface_element::{FIXED_LENGTH_MESSAGE, InterfaceElement};
use mercury::interface_table::InterfaceTable;
use mercury::interfaces::InputMessageHandler;
use mercury::misc::{CHANNEL_ID_NULL, Reason};
use mercury::network_interface::{InterfaceType, NetworkInterface};
use mercury::packet::{HEADER_SIZE, PACKET_MAX_SIZE, Packet, flags};
use mercury::packet_receiver::PacketSink;
use mercury::poller::InputNotificationHandler;
use mercury::tcp_channel::{SendError, SendSink, TcpSendBuffer};
use mercury::udp_bundle::{ReliableType, UdpBundle};
use mercury::udp_channel::{ChannelPtr, DEFAULT_INACTIVITY_RESEND_DELAY, Traits, UdpChannel};
use mercury::unpacked_message_header::UnpackedMessageHeader;
use platform::socket;

/// `127.0.0.1` in wire (little-endian word) order, as in the endpoint
/// tests.
const LOOPBACK: u32 = 0x0100_007f;

// ---------------------------------------------------------------------------
// The counting allocator
// ---------------------------------------------------------------------------

/// Allocations seen during a tracked region, on the calling thread only.
/// `peak` is the high-water mark of live bytes inside the window, and
/// `base` the live bytes when the window opened — the deltas are the
/// figures the fixtures report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
    base: usize,
    peak: usize,
}

impl Count {
    /// Live-bytes high water over the window's opening level.
    fn peak_delta(&self) -> usize {
        self.peak - self.base
    }
}

thread_local! {
    /// const-init: flag and counters never allocate, so turning
    /// tracking on inside the allocator cannot recurse. All live on the
    /// calling thread — concurrent test threads cannot pollute a
    /// measurement window.
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    /// The live-set ledger: every allocation this thread made, paired
    /// by pointer, so a free can only ever subtract what an alloc
    /// added. Frees of foreign blocks (the test harness's, made on
    /// another pooled thread) are ignored, which keeps the ledger exact
    /// where a bare running sum would underflow.
    static LEDGER: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
    /// Reentrancy guard: the ledger map's own allocations must not hit
    /// the hooks.
    static LEDGERING: Cell<bool> = const { Cell::new(false) };
    /// The ledger's byte sum and its high water (the latter only
    /// advances inside a tracking window).
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static IN_WINDOW: Cell<bool> = const { Cell::new(false) };
}

/// Ledger bookkeeping for one allocation. The guard is not set on
/// entry, so the map's internal allocations take the `LEDGERING` early
/// path in the hooks.
fn ledger_insert(ptr: *mut u8, size: usize) {
    // The guard holds across the insert so the map's own growth does
    // not re-enter the hooks. A teardown-time insertion is discarded.
    let recorded = LEDGER.try_with(|m| {
        LEDGERING.with(|g| g.set(true));
        m.borrow_mut().insert(ptr as usize, size);
        LEDGERING.with(|g| g.set(false));
    });
    if recorded.is_err() {
        return;
    }
    LIVE.with(|c| {
        let live = c.get() + size;
        c.set(live);
        if TRACKING.with(Cell::get) {
            PEAK.with(|p| p.set(p.get().max(live)));
        } else if IN_WINDOW.with(Cell::get) {
            eprintln!("DBG untracked insert: {size}");
        }
    });
}

/// Ledger bookkeeping for one free: nothing to subtract unless this
/// thread allocated the block.
fn ledger_remove(ptr: *mut u8) {
    let freed = LEDGER.try_with(|m| {
        LEDGERING.with(|g| g.set(true));
        let freed = m.borrow_mut().remove(&(ptr as usize));
        LEDGERING.with(|g| g.set(false));
        freed
    });
    if let Ok(Some(size)) = freed {
        LIVE.with(|c| c.set(c.get() - size));
    }
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if LEDGERING.with(Cell::get) {
            return unsafe { System.alloc(layout) };
        }
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ledger_insert(ptr, layout.size());
            if TRACKING.with(Cell::get) {
                ALLOCS.with(|c| c.set(c.get() + 1));
                BYTES.with(|c| c.set(c.get() + layout.size()));
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if !LEDGERING.with(Cell::get) {
            ledger_remove(ptr);
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    // `realloc` keeps its default implementation: it is alloc + copy +
    // dealloc, so the ledger sees the new block and the old one and the
    // sum moves by the delta exactly.
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Count the calling thread's allocations inside `body`, handing the
/// result and the body's value back. Live bytes carry across windows;
/// everything reported is a delta against the window's opening level.
fn measure<T>(body: impl FnOnce() -> T) -> (Count, T) {
    let base = LIVE.with(Cell::get);
    TRACKING.with(|t| {
        ALLOCS.with(|c| c.set(0));
        BYTES.with(|c| c.set(0));
        PEAK.with(|p| p.set(base));
        IN_WINDOW.with(|w| w.set(true));
        t.set(true);
    });
    let value = body();
    TRACKING.with(|t| t.set(false));
    IN_WINDOW.with(|w| w.set(false));
    let count = TRACKING.with(|_| Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
        base,
        peak: PEAK.with(Cell::get),
    });
    (count, value)
}

/// Live bytes at this instant.
fn live_bytes() -> usize {
    LIVE.with(Cell::get)
}

// ---------------------------------------------------------------------------
// The record sink
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows. Printed to stdout on flush and
/// appended to `$BUFFER_ALLOC_OUT` when set.
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: &str, value: impl std::fmt::Display) {
        self.0.push((metric.to_string(), value.to_string()));
    }

    fn flush(&self, tag: &str) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("BUFFER_ALLOC_OUT") {
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write BUFFER_ALLOC_OUT");
            println!("[{tag}] record: {path}");
        }
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------------------
// The channel-loopback fixture (the crate's own bidirectional shape)
// ---------------------------------------------------------------------------

/// Records every message delivered through a [`DispatchSink`].
#[derive(Default)]
struct Recorder {
    notes: RefCell<Vec<(u8, Vec<u8>)>>,
}

impl InputMessageHandler for Recorder {
    fn handle_message(
        &self,
        _source: &mercury::endpoint::Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        let body = data.unread_bytes().to_vec();
        data.finish();
        self.notes.borrow_mut().push((header.identifier(), body));
    }
}

/// The `PacketSink` that dispatches messages the way the receiver's
/// table walk does.
struct DispatchSink {
    table: Rc<InterfaceTable>,
}

impl PacketSink for DispatchSink {
    fn process_packet(&self, addr: &mercury::endpoint::Address, packet: &mut Packet) -> Reason {
        let first_request = if packet.has_flags(flags::HAS_REQUESTS) {
            Some(packet.first_request_offset())
        } else {
            None
        };
        let body = &mut packet.data_mut()[HEADER_SIZE..];
        dispatch_messages(&self.table, addr, body, first_request)
    }
}

fn fixed(id: u8, size: i32) -> InterfaceElement {
    InterfaceElement::new("fixed", id, FIXED_LENGTH_MESSAGE, size, None)
}

/// One leg: an interface on a kernel-picked port, a channel to the far
/// end, a served fixed message.
struct Leg {
    interface: Rc<NetworkInterface>,
    channel: ChannelPtr,
    recorder: Rc<Recorder>,
}

fn loopback_pair() -> (Leg, Leg) {
    let (iface_a, _recv_a) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
    let (iface_b, _recv_b) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
    iface_a.set_verbose(false);
    iface_b.set_verbose(false);

    let addr_a = iface_a.address();
    let addr_b = iface_b.address();

    let chan_a = UdpChannel::new(
        &iface_a,
        addr_b,
        Traits::Internal,
        DEFAULT_INACTIVITY_RESEND_DELAY,
        CHANNEL_ID_NULL,
    );
    let chan_b = UdpChannel::new(
        &iface_b,
        addr_a,
        Traits::Internal,
        DEFAULT_INACTIVITY_RESEND_DELAY,
        CHANNEL_ID_NULL,
    );
    // A received packet owes an ack; pushing at one forces the ack
    // bundle out immediately, which is what keeps the window draining.
    chan_a.borrow_mut().set_push_unsent_acks_threshold(1);
    chan_b.borrow_mut().set_push_unsent_acks_threshold(1);

    let recorder_a = Rc::new(Recorder::default());
    let table_a = Rc::new(InterfaceTable::new());
    table_a.serve(
        &fixed(3, 4),
        Some(Rc::clone(&recorder_a) as Rc<dyn InputMessageHandler>),
    );
    iface_a.set_packet_sink(Some(Rc::new(DispatchSink { table: table_a })));

    let recorder_b = Rc::new(Recorder::default());
    let table_b = Rc::new(InterfaceTable::new());
    table_b.serve(
        &fixed(3, 4),
        Some(Rc::clone(&recorder_b) as Rc<dyn InputMessageHandler>),
    );
    iface_b.set_packet_sink(Some(Rc::new(DispatchSink { table: table_b })));

    (
        Leg {
            interface: iface_a,
            channel: chan_a,
            recorder: recorder_a,
        },
        Leg {
            interface: iface_b,
            channel: chan_b,
            recorder: recorder_b,
        },
    )
}

/// One reliable message A→B over the real loopback: send_bundle, the
/// drain of B's socket, and B's ack back.
fn send_reliable(from: &Leg, to: &Leg, payload: &[u8; 4]) {
    let mut bundle = UdpBundle::new(0);
    bundle.start_message(&fixed(3, 4), ReliableType::Driver);
    bundle.write_bytes(payload);
    UdpChannel::send_bundle(&from.channel, &mut bundle);
    assert!(to.interface.poller().process_pending_events(0.1) >= 1);
    assert!(from.interface.poller().process_pending_events(0.1) >= 1);
}

/// The same shape, unreliable.
fn send_unreliable(from: &Leg, to: &Leg, payload: &[u8; 4]) {
    let mut bundle = UdpBundle::new(0);
    bundle.start_message(&fixed(3, 4), ReliableType::No);
    bundle.write_bytes(payload);
    UdpChannel::send_bundle(&from.channel, &mut bundle);
    assert!(to.interface.poller().process_pending_events(0.1) >= 1);
}

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

/// The unit cost: a datagram buffer is one 1472-byte heap allocation,
/// and nothing else. Every per-datagram figure below is a multiple of
/// this.
#[test]
fn a_packet_is_one_heap_buffer() {
    let (count, ()) = measure(|| {
        let packet = Packet::new();
        assert_eq!(packet.buffer().len(), PACKET_MAX_SIZE);
    });
    assert_eq!(count.allocs, 1, "a Packet is exactly one allocation");
    assert_eq!(count.bytes, PACKET_MAX_SIZE);
    assert_eq!(count.peak_delta(), PACKET_MAX_SIZE);

    let mut rec = Record::default();
    rec.put("packet_allocs", count.allocs);
    rec.put("packet_bytes", count.bytes);
    rec.put("packet_max_size", PACKET_MAX_SIZE);
    rec.flush("packet-unit");
}

/// The receive loop: `next_packet` hands itself over per datagram
/// (`mem::take`), so every drained datagram pays one fresh 1472-byte
/// allocation before anyone looks at the bytes. N raw datagrams from a
/// channel-registered source through a real socket, drained by the
/// receiver's input handler; the datagrams are corrupt by design (no
/// valid flags word) so the figure isolates the buffer churn from any
/// dispatch work.
#[test]
fn the_receive_loop_costs_one_buffer_per_datagram() {
    const N: usize = 256;
    const PAYLOAD: usize = 300;

    let mut client = Endpoint::new();
    client.socket(socket::SOCK_DGRAM);
    assert_eq!(client.bind(0, LOOPBACK), 0, "client bind");
    client.set_nonblocking(true);
    let client_addr = client.local();

    let (interface, receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
    interface.set_verbose(false);
    let listen_addr = interface.address();
    let fd = interface.udp_socket().fileno();

    // The client has a channel, so its datagrams route to the channel
    // instead of spawning anonymous-channel machinery per datagram.
    // The channel must stay alive for the whole drain; its address is
    // the routing key, so it is bound to the client's, not read after.
    let _channel = UdpChannel::new(
        &interface,
        client_addr,
        Traits::Internal,
        DEFAULT_INACTIVITY_RESEND_DELAY,
        CHANNEL_ID_NULL,
    );

    let payload: Vec<u8> = (0..PAYLOAD).map(|j| (j * 7) as u8).collect();
    // Warm-up datagram outside the window.
    assert_eq!(client.sendto_address(&payload, listen_addr), PAYLOAD as i32);
    while receiver.handle_input_notification(fd) > 0 {}

    // All N out first (the sends themselves allocate nothing user-side);
    // the window is the drain.
    for _ in 0..N {
        assert_eq!(client.sendto_address(&payload, listen_addr), PAYLOAD as i32);
    }
    let (count, ()) = measure(|| while receiver.handle_input_notification(fd) > 0 {});

    let mut rec = Record::default();
    rec.put("recv_loop_datagrams", N);
    rec.put("recv_loop_allocs_total", count.allocs);
    rec.put("recv_loop_bytes_total", count.bytes);
    rec.put(
        "recv_loop_allocs_per_datagram",
        format!("{:.2}", count.allocs as f64 / N as f64),
    );
    rec.put(
        "recv_loop_bytes_per_datagram",
        format!("{:.1}", count.bytes as f64 / N as f64),
    );
    rec.put("recv_loop_peak_bytes", count.peak_delta());
    assert!(
        count.allocs >= N,
        "every datagram owes at least its fresh Packet: {} allocs for {N}",
        count.allocs
    );
    rec.flush("recv-loop");
}

/// The whole send→wire→recv→dispatch path: a 4-byte message costs this
/// much per round trip through real sockets, both directions of the
/// channel machinery included.
#[test]
fn a_reliable_round_trip_is_recorded() {
    const N: usize = 32;
    let (a, b) = loopback_pair();

    // Warm both directions outside the window.
    send_reliable(&a, &b, b"WARM");
    a.recorder.notes.borrow_mut().clear();
    b.recorder.notes.borrow_mut().clear();

    let (count, ()) = measure(|| {
        for i in 0..N {
            let payload = [b'0' + (i % 10) as u8, b'X', b'Y', b'Z'];
            send_reliable(&a, &b, &payload);
        }
    });
    assert_eq!(
        b.recorder.notes.borrow().len(),
        N,
        "every message was dispatched"
    );

    let mut rec = Record::default();
    rec.put("reliable_round_trips", N);
    rec.put("reliable_rt_allocs_total", count.allocs);
    rec.put("reliable_rt_bytes_total", count.bytes);
    rec.put(
        "reliable_rt_allocs_per_message",
        format!("{:.2}", count.allocs as f64 / N as f64),
    );
    rec.put(
        "reliable_rt_bytes_per_message",
        format!("{:.1}", count.bytes as f64 / N as f64),
    );
    rec.put("reliable_rt_peak_bytes", count.peak_delta());
    rec.flush("reliable-rt");

    // The unreliable arm, same measurement.
    let (count, ()) = measure(|| {
        for i in 0..N {
            let payload = [b'a' + (i % 10) as u8, b'X', b'Y', b'Z'];
            send_unreliable(&a, &b, &payload);
        }
    });
    assert_eq!(
        b.recorder.notes.borrow().len(),
        2 * N,
        "unreliable also dispatched"
    );

    let mut rec = Record::default();
    rec.put("unreliable_messages", N);
    rec.put("unreliable_allocs_total", count.allocs);
    rec.put("unreliable_bytes_total", count.bytes);
    rec.put(
        "unreliable_allocs_per_message",
        format!("{:.2}", count.allocs as f64 / N as f64),
    );
    rec.put("unreliable_peak_bytes", count.peak_delta());
    rec.flush("unreliable");
}

/// The peak-memory datapoint: unacked reliable packets retain their
/// full `Rc<Packet>` until the ack lands, so live bytes grow by this
/// much per window slot, and draining the window drops live back but
/// leaves the peak — the window's worst-case memory.
#[test]
fn unacked_packets_set_the_window_peak() {
    const N: usize = 256;
    let clock = Rc::new(Cell::new(1_000_000u64));
    let dispatcher = {
        let clock_cell = Rc::clone(&clock);
        Rc::new(mercury::event_dispatcher::EventDispatcher::with_clock(
            Box::new(move || clock_cell.get()),
        ))
    };
    let interface = NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher);
    let channel = UdpChannel::new(
        &interface,
        mercury::endpoint::Address::new(0x0100_0ba5, socket::htons(20)),
        Traits::Internal,
        DEFAULT_INACTIVITY_RESEND_DELAY,
        CHANNEL_ID_NULL,
    );

    // N reliable sends out: each slot holds a full Packet inside an Rc.
    // The seq vector is preallocated before the window opens so its
    // capacity is not in the window; every delta hangs off count.base
    // (captured with the window) so the vector never leaks into one.
    let mut seqs = Vec::with_capacity(N);
    let (count, ()) = measure(|| {
        for _ in 0..N {
            let seq = UdpChannel::use_next_sequence_id(&channel);
            let mut packet = Packet::new();
            packet.set_seq(seq);
            assert!(UdpChannel::add_resend_timer(
                &channel,
                seq,
                Rc::new(packet),
                &[]
            ));
            seqs.push(seq);
        }
    });

    let windowed_live = live_bytes();
    let per_unacked = windowed_live - count.base;

    // Acking everything drops live back; the peak stays.
    for seq in &seqs {
        assert!(UdpChannel::handle_ack(&channel, *seq));
    }
    let drained_live = live_bytes();

    let mut rec = Record::default();
    rec.put("window_unacked", N);
    rec.put("window_allocs_total", count.allocs);
    rec.put("window_bytes_total", count.bytes);
    rec.put("window_live_bytes", per_unacked);
    rec.put(
        "window_live_bytes_per_unacked",
        format!("{:.1}", per_unacked as f64 / N as f64),
    );
    rec.put("window_peak_bytes", count.peak_delta());
    rec.put(
        "window_peak_per_unacked",
        format!("{:.1}", count.peak_delta() as f64 / N as f64),
    );
    rec.put("window_drained_live_bytes", drained_live - count.base);
    rec.put(
        "window_drained_per_unacked",
        format!("{:.1}", (drained_live - count.base) as f64 / N as f64),
    );
    assert!(
        count.allocs >= 2 * N,
        "each slot is a Packet box plus its Rc: {} allocs for {N}",
        count.allocs
    );
    assert!(
        per_unacked >= N * PACKET_MAX_SIZE,
        "each unacked slot retains its full datagram buffer"
    );
    assert!(
        drained_live < windowed_live,
        "acking frees the retained packets"
    );
    assert_eq!(
        count.peak_delta(),
        windowed_live - count.base,
        "the peak is the fully-windowed instant"
    );
    rec.flush("window-peak");
}

/// The reuse datapoint: the TCP send buffer's backing `Vec` grows once
/// and then reuses — steady-state writes allocate nothing. A
/// short-writing socket queues the tail, but the second chunk finds the
/// capacity already there.
#[test]
fn the_tcp_send_buffer_reuses_its_backing() {
    const CHUNK: usize = 64 * 1024;
    const CHUNKS: usize = 64;

    let mut buffer = TcpSendBuffer::new();
    struct AcceptAll;
    impl SendSink for AcceptAll {
        fn send(&mut self, data: &[u8]) -> Result<usize, SendError> {
            Ok(data.len())
        }
    }

    // Fill to steady state (any first-chunk growth) outside the window.
    assert_eq!(
        buffer.write_from(&mut AcceptAll, &[0u8; CHUNK]),
        mercury::tcp_channel::SendResult::Drained
    );

    let payload = vec![7u8; CHUNK];
    let (count, ()) = measure(|| {
        for _ in 0..CHUNKS {
            assert_eq!(
                buffer.write_from(&mut AcceptAll, &payload),
                mercury::tcp_channel::SendResult::Drained,
                "an accepting socket takes it all"
            );
        }
    });

    // The buffer only grows when a socket refuses bytes: the draining
    // arm above never queues. This first requeue cycle — a half-accepted
    // chunk, then a full chunk after a WouldBlock refusal — is what
    // grows the backing to its working capacity (outside the window).
    struct AcceptHalf;
    impl SendSink for AcceptHalf {
        fn send(&mut self, data: &[u8]) -> Result<usize, SendError> {
            Ok(data.len() / 2)
        }
    }
    struct AcceptNone;
    impl SendSink for AcceptNone {
        fn send(&mut self, _: &[u8]) -> Result<usize, SendError> {
            Ok(0)
        }
    }
    assert_eq!(
        buffer.write_from(&mut AcceptHalf, &payload),
        mercury::tcp_channel::SendResult::Partial
    );
    assert_eq!(buffer.pending(), CHUNK / 2);
    assert_eq!(
        buffer.flush(&mut AcceptAll),
        mercury::tcp_channel::SendResult::Drained
    );
    assert_eq!(buffer.pending(), 0);
    assert_eq!(
        buffer.write_from(&mut AcceptNone, &payload),
        mercury::tcp_channel::SendResult::WouldBlock
    );
    assert_eq!(buffer.pending(), CHUNK);
    assert_eq!(
        buffer.flush(&mut AcceptAll),
        mercury::tcp_channel::SendResult::Drained
    );
    assert_eq!(buffer.pending(), 0);

    // The reuse datapoint, second cycle: the drained buffer keeps its
    // capacity, so the identical requeue sequence allocates nothing.
    let (count2, ()) = measure(|| {
        assert_eq!(
            buffer.write_from(&mut AcceptHalf, &payload),
            mercury::tcp_channel::SendResult::Partial
        );
        assert_eq!(buffer.pending(), CHUNK / 2, "the tail queued");
        assert_eq!(
            buffer.flush(&mut AcceptAll),
            mercury::tcp_channel::SendResult::Drained
        );
        assert_eq!(buffer.pending(), 0);
        assert_eq!(
            buffer.write_from(&mut AcceptNone, &payload),
            mercury::tcp_channel::SendResult::WouldBlock
        );
        assert_eq!(buffer.pending(), CHUNK);
        assert_eq!(
            buffer.flush(&mut AcceptAll),
            mercury::tcp_channel::SendResult::Drained
        );
        assert_eq!(buffer.pending(), 0);
    });

    let mut rec = Record::default();
    rec.put("tcp_buf_steady_chunks", CHUNKS);
    rec.put("tcp_buf_steady_chunk_bytes", CHUNK);
    rec.put("tcp_buf_steady_allocs", count.allocs);
    rec.put(
        "tcp_buf_steady_allocs_per_chunk",
        format!("{:.3}", count.allocs as f64 / CHUNKS as f64),
    );
    rec.put("tcp_buf_steady_bytes", count.bytes);
    rec.put("tcp_buf_requeue_allocs", count2.allocs);
    rec.put("tcp_buf_requeue_peak_bytes", count2.peak_delta());
    assert_eq!(
        count.allocs, 0,
        "steady-state writes reuse the backing: no allocations"
    );
    assert_eq!(
        count2.allocs, 0,
        "the queue's capacity is reused across the drain"
    );
    rec.flush("tcp-buffer");
}
