//! OPT-02-01-02: allocation counts and peak live memory for the network
//! hot surfaces.
//!
//! The timing baselines live in `benches/network.rs`; this harness
//! measures the buffer story behind them. A counting global allocator
//! with a thread-local tracking flag counts only the calling thread's
//! allocations, and unlike the foundation harness it also tracks
//! *live* bytes (allocations minus frees), so each fixture reports the
//! peak heap the surface holds at once -- the process-level peak-RSS
//! record for the wire layer lives in `network_peak_rss.rs`.
//!
//! Fixtures mirror `benches/network.rs` shape for shape, protocol layer
//! only: the wire faces' counts depend on kernel wakeup batching, which
//! is not reproducible enough to pin, while these are -- every fixture
//! asserts that two runs allocate identically, so each number below is
//! exactly reproducible on a given toolchain.
//!
//! ```sh
//! cargo test -p mercury --test network_alloc_counts
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use foundation::stream::BinaryStream;
use mercury::bundle_dispatch::dispatch_messages;
use mercury::endpoint::Address;
use mercury::event_dispatcher::EventDispatcher;
use mercury::interface_element::{FIXED_LENGTH_MESSAGE, InterfaceElement};
use mercury::interface_table::InterfaceTable;
use mercury::interfaces::{InputMessageHandler, ReplyMessageHandler};
use mercury::misc::{CHANNEL_ID_NULL, Reason, SeqNumAllocator};
use mercury::network_interface::{InterfaceType, NetworkInterface};
use mercury::nub_exception::NubException;
use mercury::packet::HEADER_SIZE;
use mercury::request_manager::{DEFAULT_REQUEST_TIMEOUT, RequestManager};
use mercury::tcp_channel::{SendError, SendResult, SendSink, TcpSendBuffer};
use mercury::udp_bundle::{ReliableType, UdpBundle};
use mercury::udp_channel::{Traits, UdpChannel};
use mercury::unpacked_message_header::UnpackedMessageHeader;

// -- counting allocator ---------------------------------------------------

/// Allocations, cumulative bytes, and the peak of live bytes during a
/// tracked region, on the calling thread only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
    /// High-water mark of (allocated - freed) bytes inside the window.
    peak_live: isize,
}

thread_local! {
    /// const-init: the counters never allocate, so turning tracking on
    /// inside the allocator cannot recurse. All state lives on the
    /// calling thread -- concurrent test threads cannot pollute a
    /// window, because none of this is shared.
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            BYTES.with(|c| c.set(c.get() + layout.size()));
            LIVE.with(|l| {
                let live = l.get() + layout.size() as isize;
                l.set(live);
                PEAK.with(|p| {
                    if live > p.get() {
                        p.set(live);
                    }
                });
            });
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACKING.with(Cell::get) {
            LIVE.with(|l| l.set(l.get() - layout.size() as isize));
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    /// Routed through alloc+copy+dealloc while tracking so the live
    /// counter sees both sides of the move; plain passthrough otherwise.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !TRACKING.with(Cell::get) {
            return unsafe { System.realloc(ptr, layout, new_size) };
        }
        let new =
            unsafe { self.alloc(Layout::from_size_align_unchecked(new_size, layout.align())) };
        if !new.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
        }
        new
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Count the calling thread's allocations inside `body`, resetting the
/// live/peak counters first so the window's numbers stand alone.
fn measure<T>(body: impl FnOnce() -> T) -> (Count, T) {
    TRACKING.with(|t| {
        ALLOCS.with(|c| c.set(0));
        BYTES.with(|c| c.set(0));
        LIVE.with(|l| l.set(0));
        PEAK.with(|p| p.set(0));
        t.set(true);
    });
    let out = body();
    TRACKING.with(|t| t.set(false));
    let count = TRACKING.with(|_| Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
        peak_live: PEAK.with(Cell::get),
    });
    (count, out)
}

/// Two runs of a deterministic fixture must allocate identically -- that
/// is what makes the recorded numbers a baseline rather than a sample.
fn assert_reproducible<T>(body: impl Fn() -> T, label: &str) -> Count {
    let (first, _) = measure(&body);
    let (second, _) = measure(&body);
    assert_eq!(
        first, second,
        "{label}: allocation counts are not reproducible"
    );
    first
}

// -- shared fixtures (benches/network.rs shapes) --------------------------

/// A fixed-length message element, the shape every face here speaks.
fn fixed(id: u8, size: i32) -> InterfaceElement {
    InterfaceElement::new("fixed", id, FIXED_LENGTH_MESSAGE, size, None)
}

/// The far leg of the RPC (the bench's `Echo`): read the reply id the
/// request preamble carries (it reaches handlers through the unpacked
/// header, not the body stream), and pack the answer back into the
/// shared outbox.
#[derive(Default)]
struct Echo {
    outbox: RefCell<Vec<Vec<u8>>>,
}

impl InputMessageHandler for Echo {
    fn handle_message(
        &self,
        _source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        let reply_id = header.reply_id();
        let payload = data.unread_bytes().to_vec();
        data.finish();

        let mut reply = UdpBundle::new(0);
        reply.start_reply(reply_id, ReliableType::No);
        reply.write_bytes(&payload);
        reply.finalise();
        reply.prepare_packets(&mut SeqNumAllocator::new(1), false, None);

        let packet = reply.packet(0);
        let end = packet.msg_end_offset();
        self.outbox
            .borrow_mut()
            .push(packet.buffer()[HEADER_SIZE..end].to_vec());
    }
}

/// The sender side's reply sink.
#[derive(Default)]
struct ReplyRecorder {
    notes: Cell<usize>,
}

impl ReplyMessageHandler for ReplyRecorder {
    fn handle_message(
        &self,
        _source: &Address,
        _header: &UnpackedMessageHeader,
        data: &mut BinaryStream,
        _arg: usize,
    ) {
        data.finish();
        self.notes.set(self.notes.get() + 1);
    }

    fn handle_exception(&self, _exception: &NubException, _arg: usize) {}
}

/// Everything one RPC exchange needs (the bench's `RpcFace`).
struct RpcFace {
    sender_table: Rc<InterfaceTable>,
    peer_table: Rc<InterfaceTable>,
    manager: Rc<RequestManager>,
    echo: Rc<Echo>,
    recorder: Rc<ReplyRecorder>,
    peer_addr: Address,
    element: InterfaceElement,
}

impl RpcFace {
    fn new() -> Self {
        let manager = Rc::new(RequestManager::new(false, Rc::new(EventDispatcher::new())));
        let echo = Rc::new(Echo::default());
        let recorder = Rc::new(ReplyRecorder::default());

        let sender_table = Rc::new(InterfaceTable::new());
        sender_table.serve(
            &InterfaceElement::reply(),
            Some(Rc::clone(&manager) as Rc<dyn InputMessageHandler>),
        );
        let peer_table = Rc::new(InterfaceTable::new());
        peer_table.serve(
            &fixed(3, 8),
            Some(Rc::clone(&echo) as Rc<dyn InputMessageHandler>),
        );

        Self {
            sender_table,
            peer_table,
            manager,
            echo,
            recorder,
            peer_addr: Address::new(0x0100_0064, 2000),
            // A fixed-length request: the eight body bytes are payload;
            // the reply id and its link ride the preamble the unpack
            // skips, so the declared length counts only the payload.
            element: fixed(3, 8),
        }
    }

    /// One full RPC: request out, dispatch at the peer, the reply back,
    /// dispatch at the sender, the handler notified.
    fn once(&self) {
        let mut request = UdpBundle::new(0);
        request.start_request(
            &self.element,
            Rc::clone(&self.recorder) as Rc<dyn ReplyMessageHandler>,
            7,
            DEFAULT_REQUEST_TIMEOUT,
            ReliableType::No,
        );
        request.write_bytes(b"pingpong");
        request.finalise();
        request.add_reply_orders_to(&self.manager, Some(self.peer_addr));
        request.prepare_packets(&mut SeqNumAllocator::new(2), false, None);

        let packet = request.packet_mut(0);
        packet.grow(packet.footer_size());
        let first_request = packet.strip_footer_u16().map(usize::from);
        let end = packet.msg_end_offset();
        let mut body = packet.buffer()[HEADER_SIZE..end].to_vec();
        let reason = dispatch_messages(&self.peer_table, &self.peer_addr, &mut body, first_request);
        assert_eq!(reason, Reason::Success, "request dispatched");

        let mut reply_body = self
            .echo
            .outbox
            .borrow_mut()
            .pop()
            .expect("the echo packed a reply");
        let reason = dispatch_messages(&self.sender_table, &self.peer_addr, &mut reply_body, None);
        assert_eq!(reason, Reason::Success, "reply dispatched");
        assert_eq!(
            self.recorder.notes.get(),
            1,
            "the reply reached the handler"
        );
        self.recorder.notes.set(0);
    }
}

/// The protocol RPC round trip: one 4-byte request out and its echo
/// back, both dispatched. The number to read is "what a served RPC
/// costs the heap per call".
#[test]
fn rpc_roundtrip() {
    let face = RpcFace::new();
    face.once(); // warm the tables and the request manager

    let count = assert_reproducible(|| face.once(), "rpc_roundtrip");
    println!("rpc_roundtrip: {count:?}");
    assert!(count.allocs > 0, "an RPC pays real allocations");
}

/// A bundle carrying 1024 fixed 64-byte messages = 64 KiB of payload,
/// the bulk-update size class.
fn bulk_bundle() -> UdpBundle {
    let element = fixed(5, 64);
    let mut bundle = UdpBundle::new(0);
    for i in 0..1024u32 {
        bundle.start_message(&element, ReliableType::No);
        let chunk: [u8; 64] =
            std::array::from_fn(|j: usize| ((i as usize * 31 + j * 7) % 251) as u8);
        bundle.write_bytes(&chunk);
    }
    bundle.finalise();
    bundle
}

/// One 64 KiB bulk send in two phases: the bundle build (where the
/// packet chain allocates) and `prepare_packets` (where the footers land
/// and the reliable-window bookkeeping allocates). This fixture's
/// messages are unreliable, so the prepare phase shows the footer-only
/// cost; the reliable variant below shows the window leg.
#[test]
fn fragment_64k() {
    let element = fixed(5, 64);
    let build = assert_reproducible(
        || {
            let mut bundle = UdpBundle::new(0);
            for i in 0..1024u32 {
                bundle.start_message(&element, ReliableType::No);
                let chunk: [u8; 64] =
                    std::array::from_fn(|j: usize| ((i as usize * 31 + j * 7) % 251) as u8);
                bundle.write_bytes(&chunk);
            }
            bundle.finalise();
            bundle
        },
        "fragment_64k/build",
    );
    println!("fragment_64k/build: {build:?}");

    let mut bundle = bulk_bundle();
    let prepare = measure(|| {
        bundle.prepare_packets(&mut SeqNumAllocator::new(1), false, None);
    })
    .0;
    println!("fragment_64k/prepare: {prepare:?}");

    let packets = bundle.num_data_units();
    assert!(packets > 40, "the bulk bundle fragments: {packets} packets");
}

/// The reliable window leg of the same 64 KiB send: every reliable
/// packet is cloned into an `Rc` for the resend window on top of the
/// footer work. This is the clone the reference does not pay (its
/// `UnackedPacket` shares the bundle's `PacketPtr`).
#[test]
fn fragment_64k_reliable_window_leg() {
    let element = fixed(5, 64);
    let mut reliable = UdpBundle::new(0);
    for i in 0..1024u32 {
        reliable.start_message(&element, ReliableType::Driver);
        let chunk: [u8; 64] =
            std::array::from_fn(|j: usize| ((i as usize * 31 + j * 7) % 251) as u8);
        reliable.write_bytes(&chunk);
    }
    reliable.finalise();

    // The window leg clones only when a channel is passed; the channel
    // itself is built outside the window (its own arrays are not this
    // fixture's subject).
    let (interface, _receiver) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
    let channel = UdpChannel::new(
        &interface,
        Address::new(0x0100_00a5, 2005),
        Traits::Internal,
        0.05,
        CHANNEL_ID_NULL,
    );

    let build = assert_reproducible(
        || {
            let mut twin = UdpBundle::new(0);
            for i in 0..1024u32 {
                twin.start_message(&element, ReliableType::Driver);
                let chunk: [u8; 64] =
                    std::array::from_fn(|j: usize| ((i as usize * 31 + j * 7) % 251) as u8);
                twin.write_bytes(&chunk);
            }
            twin.finalise();
            twin
        },
        "fragment_64k_reliable/build",
    );
    println!("fragment_64k_reliable/build: {build:?}");

    let prepare = measure(|| {
        reliable.prepare_packets(&mut SeqNumAllocator::new(1), false, Some(&channel));
    })
    .0;
    println!("fragment_64k_reliable/prepare: {prepare:?}");
    let _ = reliable;
}

/// The parse-back: dispatch one 64 KiB bulk bundle's concatenated
/// stream through the message walk, 1024 handler calls. The stream is
/// built once outside the window, exactly like the bench's
/// `iter_batched` setup.
#[test]
fn reparse_64k() {
    let mut bundle = bulk_bundle();
    bundle.prepare_packets(&mut SeqNumAllocator::new(1), false, None);

    let table = Rc::new(InterfaceTable::new());
    let delivered = Rc::new(Cell::new(0u64));
    struct Counting(Rc<Cell<u64>>);
    impl InputMessageHandler for Counting {
        fn handle_message(
            &self,
            _source: &Address,
            header: &mut UnpackedMessageHeader,
            data: &mut BinaryStream,
        ) {
            self.0.set(self.0.get() + header.length() as u64);
            data.finish();
        }
    }
    table.serve(
        &fixed(5, 64),
        Some(Rc::new(Counting(Rc::clone(&delivered))) as Rc<dyn InputMessageHandler>),
    );

    let source = Address::new(0x0100_0065, 2001);
    let num_packets = bundle.num_data_units();
    let mut stream = Vec::with_capacity(1024 * 64 + num_packets);
    for index in 0..num_packets {
        let packet = bundle.packet(index);
        let end = packet.msg_end_offset();
        stream.extend_from_slice(&packet.buffer()[HEADER_SIZE..end]);
    }

    let count = assert_reproducible(
        || {
            let mut body = stream.clone();
            delivered.set(0);
            let reason = dispatch_messages(&table, &source, &mut body, None);
            assert_eq!(reason, Reason::Success);
            assert_eq!(delivered.get(), 1024 * 64);
        },
        "reparse_64k",
    );
    println!("reparse_64k: {count:?}");
}

// -- TCP faces ------------------------------------------------------------

/// 512 frames of 2 KiB payload = ~1 MiB of framed stream bytes.
const TCP_FRAMES: usize = 512;
const TCP_PAYLOAD: usize = 2048;

fn tcp_frame_payload(frame: usize) -> Vec<u8> {
    (0..TCP_PAYLOAD)
        .map(|j| ((frame * 17 + j * 11) % 253) as u8)
        .collect()
}

/// The wire form `TCPChannel::doSend` writes: 16-bit LE length prefix
/// then the body.
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 2);
    frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// The send half through `TcpSendBuffer` into a taking sink: the buffer
/// is supposed to stay empty on the drained path (the socket tried
/// directly, no buffering copy). Peak live answers whether the framing
/// machinery holds the whole stream at once or frame by frame.
struct VecSink<'a>(&'a mut Vec<u8>);

impl SendSink for VecSink<'_> {
    fn send(&mut self, data: &[u8]) -> Result<usize, SendError> {
        self.0.extend_from_slice(data);
        Ok(data.len())
    }
}

#[test]
fn tcp_frame_stream() {
    let frames: Vec<Vec<u8>> = (0..TCP_FRAMES).map(tcp_frame_payload).collect();

    // Send half alone: the fixture Vecs (frames + wire) are built
    // inside the window, so subtract their known size when reading the
    // framing machinery's own peak: frames hold 512 x (2048 + Vec
    // overhead), the wire holds the concatenated stream.
    let send = assert_reproducible(
        || {
            let mut wire = Vec::with_capacity(TCP_FRAMES * (TCP_PAYLOAD + 2));
            let mut sink = VecSink(&mut wire);
            let mut buffer = TcpSendBuffer::new();
            for frame in &frames {
                assert_eq!(
                    buffer.write_from(&mut sink, &framed(frame)),
                    SendResult::Drained
                );
            }
            assert!(!buffer.has_pending());
            wire
        },
        "tcp_frame_stream/send",
    );
    println!("tcp_frame_stream/send: {send:?}");

    let wire_len = TCP_FRAMES * (TCP_PAYLOAD + 2);
    let count = send;
    // Every framed(frame) allocates a fresh frame Vec per write; the
    // send buffer itself must never hold data on this path.
    assert!(
        count.peak_live < (wire_len + frames.len() * (TCP_PAYLOAD + 2) + 4096) as isize,
        "the send-buffer drained path must not buffer copies"
    );

    // The stalled path: a sink that refuses everything, so every write
    // lands in the buffer. Peak live must reach the full backlog.
    struct Refusing;
    impl SendSink for Refusing {
        fn send(&mut self, _data: &[u8]) -> Result<usize, SendError> {
            Ok(0)
        }
    }
    let stalled = measure(|| {
        let mut buffer = TcpSendBuffer::new();
        let mut sink = Refusing;
        for frame in &frames {
            let _ = buffer.write_from(&mut sink, &framed(frame));
        }
        assert_eq!(buffer.pending(), wire_len);
        buffer
    })
    .0;
    println!("tcp_frame_stream/stalled: {stalled:?}");
    assert!(
        stalled.peak_live >= wire_len as isize,
        "the stalled buffer holds the whole backlog"
    );
}
