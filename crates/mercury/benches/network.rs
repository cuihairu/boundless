//! Network transport baselines (OPT-02-01-01).
//!
//! The row's three records -- RPC RTT, throughput, and the wire shapes
//! behind them -- measured at two layers:
//!
//! - **Protocol layer** (in-process, no kernel): the Mercury machinery
//!   alone. The request/reply round trip and bundle fragmentation.
//!   These are CPU-cost records; they are stable on a shared box.
//! - **Wire layer** (real loopback sockets, kernel and scheduler
//!   included): one reliable round trip, a 64 KiB reliable transfer,
//!   and a 1 MiB TCP stream. These carry syscall and scheduling cost,
//!   so on a loaded box they are floor estimates -- see the load note
//!   on the baseline page.
//!
//! The TCP faces use the declared send-buffer face
//! (`tcp_channel::TcpSendBuffer`) and split the length-prefixed frames
//! inline: the dedicated frame-parser module is not declared in
//! `lib.rs` yet (it rides the tcp restore lane), so claiming it here
//! would bench an orphan.
//!
//! Fixtures are deterministic (index arithmetic, no RNG). Loss recovery
//! is deliberately *not* benched: its timings run off the channel's RTT
//! average and resend period, which are simulated-time machinery, so
//! `crates/mercury/tests/loss_recovery_record.rs` pins the exact stamps
//! instead of sampling a noisy wall clock.

use std::cell::{Cell, RefCell};
use std::hint::black_box;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::mpsc;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
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
use mercury::packet::{HEADER_SIZE, Packet, flags};
use mercury::packet_receiver::PacketSink;
use mercury::request_manager::{DEFAULT_REQUEST_TIMEOUT, RequestManager};
use mercury::tcp_channel::{SendError, SendResult, SendSink, TcpSendBuffer};
use mercury::udp_bundle::{ReliableType, UdpBundle};
use mercury::udp_channel::{Traits, UdpChannel};
use mercury::unpacked_message_header::UnpackedMessageHeader;

// -- shared fixtures -----------------------------------------------------

/// A fixed-length message element, the shape every face here speaks.
fn fixed(id: u8, size: i32) -> InterfaceElement {
    InterfaceElement::new("fixed", id, FIXED_LENGTH_MESSAGE, size, None)
}

// -- protocol layer: the RPC round trip ----------------------------------

/// The far leg of the RPC: read the reply id every Mercury request
/// carries in its body, and pack the answer back into a shared outbox
/// the bench dispatches at the sender's table.
#[derive(Default)]
struct Echo {
    outbox: RefCell<Vec<Vec<u8>>>,
}

impl InputMessageHandler for Echo {
    fn handle_message(
        &self,
        _source: &Address,
        _header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        let reply_id = data.read_i32().expect("request reply id");
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

/// Everything one RPC exchange needs. Built once; the request bundle is
/// rebuilt every iteration like a real caller's would be.
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

        // The sender's nub serves the reply element with its request
        // manager; the peer's nub serves the request message with the
        // echo handler.
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

        let peer_addr = Address::new(0x0100_0064, 2000);
        Self {
            sender_table,
            peer_table,
            manager,
            echo,
            recorder,
            peer_addr,
            // 8 fixed bytes: the four reply id bytes the request carries
            // plus the four payload bytes.
            element: fixed(3, 8),
        }
    }

    /// One full RPC: request out, dispatch at the peer, the echo's reply
    /// back, dispatch at the sender, the handler notified.
    fn once(&self) {
        let mut request = UdpBundle::new(0);
        request.start_request(
            &self.element,
            Rc::clone(&self.recorder) as Rc<dyn ReplyMessageHandler>,
            7,
            DEFAULT_REQUEST_TIMEOUT,
            ReliableType::No,
        );
        request.write_bytes(b"ping");
        request.finalise();
        request.add_reply_orders_to(&self.manager, Some(self.peer_addr));
        request.prepare_packets(&mut SeqNumAllocator::new(2), false, None);

        // The peer's receive recipe: peel the request footer, then walk
        // the body the way the packet receiver does.
        let first_request = request.packet_mut(0).strip_footer_u16().map(usize::from);
        let packet = request.packet(0);
        let end = packet.msg_end_offset();
        let mut body = packet.buffer()[HEADER_SIZE..end].to_vec();
        let reason = dispatch_messages(&self.peer_table, &self.peer_addr, &mut body, first_request);
        assert_eq!(reason, Reason::Success, "request dispatched");

        // The reply travels back and lands on the sender's manager.
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

fn bench_rpc_roundtrip(c: &mut Criterion) {
    let face = RpcFace::new();
    face.once(); // warm the tables before timing

    let mut group = c.benchmark_group("udp");
    group.bench_function("protocol_rpc_roundtrip", |bench| bench.iter(|| face.once()));
    group.finish();
}

// -- protocol layer: fragmentation ---------------------------------------

/// A bundle carrying 1024 fixed 64-byte messages = 64 KiB of payload
/// (plus one message id each), the size class a bulk update rides in.
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

/// The payload bytes one bulk bundle carries, for the throughput figure.
fn bulk_payload_bytes() -> u64 {
    1024 * 64
}

fn bench_fragment(c: &mut Criterion) {
    let mut group = c.benchmark_group("udp");
    group.throughput(Throughput::Bytes(bulk_payload_bytes()));
    // prepare_packets packs the footers into the packets, so each
    // iteration gets a fresh bundle (the setup is outside the window,
    // same split the timing table's fixtures use).
    group.bench_function("fragment_64k_stream", |bench| {
        bench.iter_batched(
            bulk_bundle,
            |mut bundle| {
                let packets = bundle.prepare_packets(&mut SeqNumAllocator::new(1), false, None);
                black_box(packets);
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_reparse(c: &mut Criterion) {
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
    assert!(
        num_packets > 40,
        "the bulk bundle fragments: {} packets",
        num_packets
    );

    // A bundle is one contiguous message stream across its packet chain
    // -- messages straddle packet boundaries, so the receive side walks
    // the concatenated bodies, which is what the bundle iterator's
    // nextPacket chaining does. Rebuild that stream once here; the
    // iteration dispatches a fresh copy.
    let mut stream = Vec::with_capacity(bulk_payload_bytes() as usize + num_packets);
    for index in 0..num_packets {
        let packet = bundle.packet(index);
        let end = packet.msg_end_offset();
        stream.extend_from_slice(&packet.buffer()[HEADER_SIZE..end]);
    }

    let mut group = c.benchmark_group("udp");
    group.throughput(Throughput::Bytes(bulk_payload_bytes()));
    group.bench_function("reparse_64k_stream", |bench| {
        bench.iter_batched(
            || stream.clone(),
            |mut body| {
                delivered.set(0);
                let reason = dispatch_messages(&table, &source, &mut body, None);
                assert_eq!(reason, Reason::Success);
                assert_eq!(delivered.get(), bulk_payload_bytes());
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

// -- wire layer: loopback sockets ----------------------------------------

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

/// One leg of the loopback pair: an interface bound to a kernel-picked
/// port, a reliable channel to the far end, and the delivered-message
/// count.
struct Leg {
    interface: Rc<NetworkInterface>,
    channel: mercury::udp_channel::ChannelPtr,
    delivered: Rc<Cell<u64>>,
}

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

/// Build two loopback legs with channels pointing at each other. The
/// 1-threshold makes every received packet push its ack straight back,
/// which is what drains the windows on the wire.
fn loopback_pair(body_size: i32) -> (Leg, Leg) {
    let (iface_a, _recv_a) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");
    let (iface_b, _recv_b) = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1");

    let addr_a = iface_a.address();
    let addr_b = iface_b.address();

    let chan_a = UdpChannel::new(&iface_a, addr_b, Traits::Internal, 0.05, CHANNEL_ID_NULL);
    let chan_b = UdpChannel::new(&iface_b, addr_a, Traits::Internal, 0.05, CHANNEL_ID_NULL);
    chan_a.borrow_mut().set_push_unsent_acks_threshold(1);
    chan_b.borrow_mut().set_push_unsent_acks_threshold(1);

    fn leg(
        interface: Rc<NetworkInterface>,
        channel: mercury::udp_channel::ChannelPtr,
        body_size: i32,
    ) -> Leg {
        let delivered = Rc::new(Cell::new(0u64));
        let table = Rc::new(InterfaceTable::new());
        table.serve(
            &fixed(3, body_size),
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
        leg(iface_a, chan_a, body_size),
        leg(iface_b, chan_b, body_size),
    )
}

/// One reliable 4-byte message from `from` towards `to`, over the real
/// loopback socket. The caller pumps the pollers.
fn wire_send_reliable(from: &Leg, payload: &[u8; 4]) {
    let mut bundle = UdpBundle::new(0);
    bundle.start_message(&fixed(3, 4), ReliableType::Driver);
    bundle.write_bytes(payload);
    UdpChannel::send_bundle(&from.channel, &mut bundle);
}

fn bench_wire_roundtrip(c: &mut Criterion) {
    let (a, b) = loopback_pair(4);

    let mut group = c.benchmark_group("udp");
    group.bench_function("wire_reliable_roundtrip_loopback", |bench| {
        bench.iter(|| {
            wire_send_reliable(&a, b"RT1X");
            // One pump drains the far socket and pushes the ack out;
            // the second reads it and clears the window.
            let _ = b.interface.poller().process_pending_events(0.1);
            let _ = a.interface.poller().process_pending_events(0.1);
            assert_eq!(b.delivered.get(), 1);
            b.delivered.set(0);
        })
    });
    group.finish();
}

fn bench_wire_transfer(c: &mut Criterion) {
    let (a, b) = loopback_pair(64);

    let mut group = c.benchmark_group("udp");
    group.throughput(Throughput::Bytes(bulk_payload_bytes()));
    group.bench_function("wire_64k_transfer_loopback", |bench| {
        bench.iter(|| {
            // One reliable 64-byte message per datagram: a bundle whose
            // messages straddle packets needs the bundle-processor
            // reassembly face, which is not declared in lib.rs yet, so
            // the per-datagram shape is what the declared receive path
            // carries whole. The drain loop pumps until every ack is
            // back.
            let element = fixed(3, 64);
            for i in 0..1024u32 {
                let mut bundle = UdpBundle::new(0);
                bundle.start_message(&element, ReliableType::Driver);
                let chunk: [u8; 64] =
                    std::array::from_fn(|j: usize| ((i as usize * 13 + j * 5) % 241) as u8);
                bundle.write_bytes(&chunk);
                UdpChannel::send_bundle(&a.channel, &mut bundle);
            }

            let mut steps = 0;
            while a.channel.borrow().has_unacked_packets() {
                let _ = b.interface.poller().process_pending_events(0.1);
                let _ = a.interface.poller().process_pending_events(0.1);
                steps += 1;
                assert!(steps < 500, "the transfer must drain");
            }
            assert_eq!(b.delivered.get(), 1024, "every message arrived");
            b.delivered.set(0);
        })
    });
    group.finish();
}

// -- TCP faces -----------------------------------------------------------

/// 512 frames of 2 KiB payload = ~1 MiB of framed stream bytes.
const TCP_FRAMES: usize = 512;
const TCP_PAYLOAD: usize = 2048;
const TCP_STREAM_BYTES: u64 = (TCP_FRAMES * (TCP_PAYLOAD + 2)) as u64;

fn tcp_frame_payload(frame: usize) -> Vec<u8> {
    (0..TCP_PAYLOAD)
        .map(|j| ((frame * 17 + j * 11) % 253) as u8)
        .collect()
}

/// One framed bundle: the 16-bit little-endian length prefix then the
/// body, the wire form `TCPChannel::doSend` writes.
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 2);
    frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// The frame split the receive side does on the byte stream: how many
/// complete length-prefixed frames the buffer holds.
fn count_frames(buf: &[u8]) -> usize {
    let mut pos = 0usize;
    let mut frames = 0usize;
    while pos + 2 <= buf.len() {
        let len = u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap()) as usize;
        if pos + 2 + len > buf.len() {
            break;
        }
        pos += 2 + len;
        frames += 1;
    }
    frames
}

/// A stream state across read segments: one frame can straddle a segment
/// boundary, so the leftover head is carried to the next chunk.
#[derive(Default)]
struct StreamSplit {
    buffer: Vec<u8>,
    frames: usize,
}

impl StreamSplit {
    fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        let mut pos = 0usize;
        loop {
            if pos + 2 > self.buffer.len() {
                break;
            }
            let len = u16::from_le_bytes(self.buffer[pos..pos + 2].try_into().unwrap()) as usize;
            if pos + 2 + len > self.buffer.len() {
                break;
            }
            pos += 2 + len;
            self.frames += 1;
        }
        self.buffer.drain(..pos);
    }
}

fn bench_tcp_in_memory(c: &mut Criterion) {
    let frames: Vec<Vec<u8>> = (0..TCP_FRAMES).map(tcp_frame_payload).collect();

    let mut group = c.benchmark_group("tcp");
    group.throughput(Throughput::Bytes(TCP_STREAM_BYTES));
    group.bench_function("frame_stream_in_memory", |bench| {
        bench.iter(|| {
            // Send half: every frame through the send buffer into a
            // taking sink, the way a channel hands the socket bytes.
            let mut wire = Vec::with_capacity(TCP_STREAM_BYTES as usize);
            let mut sink = VecSink(&mut wire);
            let mut buffer = TcpSendBuffer::new();
            for frame in &frames {
                assert_eq!(
                    buffer.write_from(&mut sink, &framed(frame)),
                    mercury::tcp_channel::SendResult::Drained
                );
            }
            assert!(!buffer.has_pending());

            // Receive half: the stream comes back in 16 KiB segments,
            // which is what a segment burst looks like; frames straddle
            // the boundaries and the head carries over.
            let mut split = StreamSplit::default();
            for chunk in wire.chunks(16 * 1024) {
                split.feed(chunk);
            }
            assert_eq!(split.frames, TCP_FRAMES);
        })
    });
    group.finish();
}

/// A sink that takes everything it is handed, accumulating the stream.
struct VecSink<'a>(&'a mut Vec<u8>);

impl SendSink for VecSink<'_> {
    fn send(&mut self, data: &[u8]) -> Result<usize, SendError> {
        self.0.extend_from_slice(data);
        Ok(data.len())
    }
}

/// The loopback socket as a send sink.
struct SocketSink<'a>(&'a TcpStream);

impl SendSink for SocketSink<'_> {
    fn send(&mut self, data: &[u8]) -> Result<usize, SendError> {
        self.0.write(data).map_err(|_| SendError)
    }
}

fn bench_tcp_wire_stream(c: &mut Criterion) {
    let frames: Vec<Vec<u8>> = (0..TCP_FRAMES).map(tcp_frame_payload).collect();

    // A real loopback pair; the reader thread lives across iterations
    // and signals done with a one-byte sentinel frame, so no iteration
    // pays a thread spawn.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let reader = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut backlog: Vec<u8> = Vec::with_capacity(TCP_STREAM_BYTES as usize + 8);
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let read = match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            backlog.extend_from_slice(&chunk[..read]);
            if count_frames(&backlog) > TCP_FRAMES {
                backlog.clear();
                if done_tx.send(()).is_err() {
                    break;
                }
            }
        }
    });
    let stream = TcpStream::connect(addr).expect("connect loopback");

    let mut group = c.benchmark_group("tcp");
    group.throughput(Throughput::Bytes(TCP_STREAM_BYTES));
    group.bench_function("wire_stream_loopback", |bench| {
        bench.iter(|| {
            let mut buffer = TcpSendBuffer::new();
            let mut sink = SocketSink(&stream);
            for frame in &frames {
                assert_eq!(
                    buffer.write_from(&mut sink, &framed(frame)),
                    SendResult::Drained
                );
            }

            // The sentinel frame tells the reader this iteration's
            // stream is complete; waiting for it keeps the timed region
            // honest (the reader has parsed the whole megabyte).
            let sentinel = framed(&[0u8]);
            assert_eq!(buffer.write_from(&mut sink, &sentinel), SendResult::Drained);

            done_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the reader kept up");
        })
    });
    group.finish();

    // Unwind: closing the stream ends the reader's read loop.
    drop(stream);
    let _ = reader.join();
}

criterion_group!(
    benches,
    bench_rpc_roundtrip,
    bench_fragment,
    bench_reparse,
    bench_wire_roundtrip,
    bench_wire_transfer,
    bench_tcp_in_memory,
    bench_tcp_wire_stream
);
criterion_main!(benches);
