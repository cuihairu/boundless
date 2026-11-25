//! OPT-02-01-01: the network baseline measurements.
//!
//! The acceptance saves three record families: RPC round-trip latency,
//! bulk throughput, and loss recovery. The first two run against the
//! real loopback -- a pair of [`mercury::endpoint::Endpoint`]s talking
//! UDP, and a TCP listener/accept pair -- so the numbers are the
//! transport's own; the third is driven deterministically on the pinned
//! clock through the [`mercury::udp_channel::UdpChannel`] resend
//! machinery (the same shape as its unit tests), so the recovery
//! record -- one controlled loss, exactly one redelivery, one ack
//! draining the window -- is identical on every run.
//!
//! Structural invariants are asserted on every run. The wall-clock
//! numbers are only recorded (never asserted tight), and pass through
//! `NET_BASELINE_OUT` as a `metric<TAB>value` TSV when set, matching the
//! load-harness fixture format. The canonical record is produced by the
//! runner leg alone, in fixed leg order:
//!
//! ```sh
//! NET_BASELINE_OUT=<abs path> cargo test -p mercury --test network_baseline \
//!     the_network_baseline_runs_all_legs -- --exact
//! ```
//!
//! (cargo starts the test binary in the crate root, so the path must be
//! absolute; the flushes append, so parallel runs interleave instead of
//! clobbering.)

use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mercury::endpoint::{Endpoint, INADDR_ANY, NO_SOCKET};
use mercury::event_dispatcher::EventDispatcher;
use mercury::misc::CHANNEL_ID_NULL;
use mercury::network_interface::{InterfaceType, NetworkInterface};
use mercury::packet::Packet;
use mercury::udp_channel::{DEFAULT_INACTIVITY_RESEND_DELAY, Traits, UdpChannel};
use platform::socket;

/// `127.0.0.1` in the codebase's wire (little-endian word) order, as in
/// the endpoint unit tests.
const LOOPBACK: u32 = 0x0100_007f;

/// A bound UDP endpoint on an ephemeral port.
fn udp_endpoint(ip: u32) -> Endpoint {
    let mut ep = Endpoint::new();
    ep.socket(socket::SOCK_DGRAM);
    assert_eq!(ep.bind(0, ip), 0, "udp bind failed");
    ep.set_nonblocking(true);
    ep
}

/// A blocking receive with a deadline: loops the non-blocking socket
/// with short sleeps so a lost datagram fails the leg instead of
/// hanging it.
fn recv_deadline(
    ep: &Endpoint,
    buf: &mut [u8],
    addr: &mut mercury::endpoint::Address,
) -> Option<usize> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let n = ep.recvfrom_address(buf, addr);
        if n >= 0 {
            return Some(n as usize);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------------------
// The record sink
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows. Printed to stdout on drop-flush and
/// written to `$NET_BASELINE_OUT` when set (the load-harness TSV format).
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
        if let Ok(path) = std::env::var("NET_BASELINE_OUT") {
            // Append, not truncate: the legs flush independently (and
            // cargo starts the test binary in the crate root, so pass
            // NET_BASELINE_OUT as an absolute path).
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write NET_BASELINE_OUT");
            println!("[{tag}] record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// The p50/p90/p99/max/min summary of a sorted sample set.
fn summarize(samples: &mut [f64]) -> [f64; 5] {
    samples.sort_by(|a, b| a.total_cmp(b));
    let pick = |p: f64| {
        let idx = ((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len()) - 1;
        samples[idx]
    };
    [
        pick(0.50),
        pick(0.90),
        pick(0.99),
        samples[samples.len() - 1],
        samples[0],
    ]
}

// ---------------------------------------------------------------------------
// Leg 1+2: UDP RTT and throughput over the real loopback
// ---------------------------------------------------------------------------

/// A UDP echo endpoint bound inside its own thread; the bound address
/// comes back through the channel so the caller sends at it.
fn spawn_udp_echo() -> (
    std::sync::mpsc::Receiver<mercury::endpoint::Address>,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let ep = udp_endpoint(LOOPBACK);
        let local = ep.local();
        tx.send(local).expect("echo reports its address");

        let mut buf = [0u8; 65536];
        let mut peer = mercury::endpoint::Address::default();
        // 5s idle ends the leg.
        while let Some(n) = recv_deadline(&ep, &mut buf, &mut peer) {
            // Echo the payload back to whoever sent it.
            assert_eq!(ep.sendto_address(&buf[..n], peer), n as i32);
        }
    });
    (rx, handle)
}

/// `NET_BASELINE_UDP_RTT_*`: N pings through the loopback echo, each
/// timed with the monotonic clock. Asserts every reply came back whole
/// and promptly (loose bounds: loopback is microseconds; the bound only
/// catches a broken transport or a swamped runner).
#[test]
fn udp_loopback_rtt_is_recorded() {
    const SAMPLES: usize = 200;
    const PAYLOAD: usize = 64;

    let (rx, echo) = spawn_udp_echo();
    let peer_addr = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("echo address");

    let client = udp_endpoint(LOOPBACK);
    let mut samples = Vec::with_capacity(SAMPLES);
    let mut buf = [0u8; 65536];
    let mut from = mercury::endpoint::Address::default();
    for i in 0..SAMPLES {
        let ping: Vec<u8> = (0..PAYLOAD).map(|j| (i + j) as u8).collect();
        let start = Instant::now();
        assert_eq!(client.sendto_address(&ping, peer_addr), PAYLOAD as i32);
        let Some(n) = recv_deadline(&client, &mut buf, &mut from) else {
            panic!("ping {i} got no reply within the deadline");
        };
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        assert_eq!(n, PAYLOAD, "the echo came back whole");
        assert_eq!(&buf[..n], &ping, "the echo returned the payload");
    }
    echo.join().expect("echo thread");

    let [p50, p90, p99, max, min] = summarize(&mut samples);
    let mut rec = Record::default();
    rec.put("udp_rtt_samples", SAMPLES);
    rec.put("udp_rtt_payload_bytes", PAYLOAD);
    rec.put("udp_rtt_p50_us", format!("{p50:.3}"));
    rec.put("udp_rtt_p90_us", format!("{p90:.3}"));
    rec.put("udp_rtt_p99_us", format!("{p99:.3}"));
    rec.put("udp_rtt_max_us", format!("{max:.3}"));
    rec.put("udp_rtt_min_us", format!("{min:.3}"));
    assert!(max < 500_000.0, "loopback RTT blew past 500ms: {max}us");
    rec.flush("udp-rtt");
}

/// `NET_BASELINE_UDP_TPUT_*`: 4 MiB pushed as 1200-byte datagrams, all
/// of which must arrive (loopback neither drops nor reorders). The push
/// is paced -- at most [`IN_FLIGHT`] unechoed datagrams are in the air --
/// because blasting all 3495 before draining overflows the client's
/// kernel receive buffer and the kernel silently drops the overflow.
/// Records the elapsed time, the bytes/s and the datagram rate.
#[test]
fn udp_loopback_throughput_is_recorded() {
    const MIB: usize = 4 * 1024 * 1024;
    const DATAGRAM: usize = 1200;
    const IN_FLIGHT: usize = 64;
    let total = MIB / DATAGRAM;

    let (rx, echo) = spawn_udp_echo();
    let peer_addr = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("echo address");

    let client = udp_endpoint(LOOPBACK);
    let payload: Vec<u8> = (0..DATAGRAM).map(|j| j as u8).collect();
    let mut buf = [0u8; 65536];
    let mut from = mercury::endpoint::Address::default();
    let start = Instant::now();
    let mut sent = 0usize;
    let mut got = 0usize;
    while got < total {
        // Keep a bounded window in the air; drain as it fills.
        while sent < total && sent - got < IN_FLIGHT {
            assert_eq!(client.sendto_address(&payload, peer_addr), DATAGRAM as i32);
            sent += 1;
        }
        let Some(n) = recv_deadline(&client, &mut buf, &mut from) else {
            panic!("echo stream stopped at {got}/{total} datagrams");
        };
        assert_eq!(n, DATAGRAM);
        assert_eq!(&buf[..n], &payload, "echo payload mismatch");
        got += 1;
    }
    let secs = start.elapsed().as_secs_f64();
    echo.join().expect("echo thread");

    let mut rec = Record::default();
    rec.put("udp_tput_bytes", got * DATAGRAM);
    rec.put("udp_tput_datagrams", got);
    rec.put("udp_tput_datagram_size", DATAGRAM);
    rec.put("udp_tput_seconds", format!("{secs:.6}"));
    rec.put(
        "udp_tput_mib_per_s",
        format!("{:.3}", MIB as f64 / secs / (1 << 20) as f64),
    );
    rec.put(
        "udp_tput_datagrams_per_s",
        format!("{:.1}", got as f64 / secs),
    );
    assert_eq!(got, total, "every datagram must come back");
    rec.flush("udp-tput");
}

// ---------------------------------------------------------------------------
// Leg 3: TCP RTT and throughput over the real loopback
// ---------------------------------------------------------------------------

/// Push every byte out the non-blocking stream: a stream send may take
/// only part of the buffer, so the cursor advances by whatever each
/// call accepted and `WouldBlock` waits 1ms for space. Panics past a
/// 30s deadline instead of hanging.
fn send_all(ep: &Endpoint, mut data: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut total = 0usize;
    while !data.is_empty() {
        let n = ep.send(data);
        if n > 0 {
            data = &data[n as usize..];
            total += n as usize;
        } else {
            assert!(Instant::now() < deadline, "tcp send stalled at {total}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// A TCP echo thread: listener on an ephemeral port, one accept, echo
/// everything until a 5s idle.
fn spawn_tcp_echo() -> (std::sync::mpsc::Receiver<u16>, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_STREAM);
        assert_eq!(listener.set_reuseaddr(true), 0);
        assert_eq!(listener.bind(0, LOOPBACK), 0, "tcp bind failed");
        assert_eq!(listener.listen(1), 0);
        // The port stays in wire order: `Endpoint::connect` takes it as
        // stored (see the endpoint tests), host-ordering it here makes
        // every connect refuse.
        tx.send(listener.local().port)
            .expect("echo reports its port");

        let Some(conn) = listener.accept(None, None) else {
            panic!("tcp accept failed");
        };
        conn.set_nonblocking(true);
        let mut buf = [0u8; 65536];
        let mut last_rx = Instant::now();
        loop {
            match conn.recv(&mut buf) {
                n if n > 0 => {
                    // A stream send may take only part of the buffer;
                    // push the rest out as buffer space opens, never
                    // blocking the echo loop (bounded so a dead peer
                    // cannot spin us forever).
                    let mut data = &buf[..n as usize];
                    let stall = Instant::now() + Duration::from_secs(30);
                    while !data.is_empty() {
                        let sent = conn.send(data);
                        if sent > 0 {
                            data = &data[sent as usize..];
                        } else {
                            assert!(Instant::now() < stall, "echo send stalled");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }
                    last_rx = Instant::now();
                }
                _ => {
                    if last_rx.elapsed() > Duration::from_secs(5) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    });
    (rx, handle)
}

/// `NET_BASELINE_TCP_RTT_*` and `NET_BASELINE_TCP_TPUT_*`: the same two
/// shapes over a connected stream, for the UDP/TCP comparison the OPT-02
/// cluster wants (the version-mixed leg in 02-01-04 replays both).
#[test]
fn tcp_loopback_rtt_and_throughput_are_recorded() {
    const RTT_SAMPLES: usize = 100;
    const RTT_PAYLOAD: usize = 64;
    const MIB: usize = 4 * 1024 * 1024;

    let (rx, echo) = spawn_tcp_echo();
    let port = rx.recv_timeout(Duration::from_secs(5)).expect("echo port");

    let mut conn = Endpoint::new();
    conn.socket(socket::SOCK_STREAM);
    assert_eq!(conn.connect(port, LOOPBACK), 0, "tcp connect failed");
    conn.set_nonblocking(true);

    // RTT: one ping in flight at a time; the echo replies byte-exact.
    let mut samples = Vec::with_capacity(RTT_SAMPLES);
    let mut buf = [0u8; 65536];
    for i in 0..RTT_SAMPLES {
        let ping: Vec<u8> = (0..RTT_PAYLOAD).map(|j| (i * 7 + j) as u8).collect();
        let start = Instant::now();
        send_all(&conn, &ping);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = 0usize;
        while got < RTT_PAYLOAD {
            let n = conn.recv(&mut buf);
            if n > 0 {
                assert_eq!(&buf[..n as usize], &ping[got..got + n as usize]);
                got += n as usize;
            } else {
                assert!(Instant::now() < deadline, "tcp reply stalled");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6);
    }

    let [p50, p90, p99, max, min] = summarize(&mut samples);
    let mut rec = Record::default();
    rec.put("tcp_rtt_samples", RTT_SAMPLES);
    rec.put("tcp_rtt_payload_bytes", RTT_PAYLOAD);
    rec.put("tcp_rtt_p50_us", format!("{p50:.3}"));
    rec.put("tcp_rtt_p90_us", format!("{p90:.3}"));
    rec.put("tcp_rtt_p99_us", format!("{p99:.3}"));
    rec.put("tcp_rtt_max_us", format!("{max:.3}"));
    rec.put("tcp_rtt_min_us", format!("{min:.3}"));
    assert!(max < 500_000.0, "loopback RTT blew past 500ms: {max}us");

    // Throughput: 4 MiB out, all of it echoed back. Send and drain are
    // interleaved -- a bulk push first would fill the pipe in both
    // directions and deadlock two senders neither of which is reading.
    // Bytes only are counted here; the RTT leg already proved the echo
    // byte-exact.
    let payload: Vec<u8> = (0..65536).map(|j| j as u8).collect();
    let start = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut sent = 0usize;
    let mut back = 0usize;
    while back < MIB {
        if sent < MIB {
            let chunk = payload.len().min(MIB - sent);
            let n = conn.send(&payload[..chunk]);
            if n > 0 {
                sent += n as usize;
            }
        }
        let n = conn.recv(&mut buf);
        if n > 0 {
            back += n as usize;
        } else if sent < MIB {
            assert!(
                Instant::now() < deadline,
                "tcp echo stalled: sent {sent}, back {back}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let secs = start.elapsed().as_secs_f64();

    rec.put("tcp_tput_bytes", MIB);
    rec.put("tcp_tput_seconds", format!("{secs:.6}"));
    rec.put(
        "tcp_tput_mib_per_s",
        format!("{:.3}", MIB as f64 / secs / (1 << 20) as f64),
    );
    rec.flush("tcp");
    drop(conn);
    echo.join().expect("echo thread");
}

// ---------------------------------------------------------------------------
// Leg 4: loss recovery on the pinned clock (deterministic)
// ---------------------------------------------------------------------------

/// The pinned-clock interface the udp_channel unit tests use: no
/// socket, just the dispatcher and the channel maps, so the resend
/// scan is driven purely by the clock values this test sets.
fn interface(clock: &Rc<Cell<u64>>) -> Rc<NetworkInterface> {
    let clock_cell = Rc::clone(clock);
    let dispatcher = Rc::new(EventDispatcher::with_clock(Box::new(move || {
        clock_cell.get()
    })));
    NetworkInterface::new_with_dispatcher(InterfaceType::Internal, dispatcher)
}

fn addr(ip: u32) -> mercury::endpoint::Address {
    mercury::endpoint::Address::new(ip, socket::htons(20))
}

/// One reliable packet out on `seq`, the way the reference's send path
/// drives `useNextSequenceID` + `addResendTimer`.
fn send_reliable(channel: &mercury::udp_channel::ChannelPtr) -> bool {
    let seq = UdpChannel::use_next_sequence_id(channel);
    let mut packet = Packet::new();
    packet.set_seq(seq);
    UdpChannel::add_resend_timer(channel, seq, Rc::new(packet), &[])
}

/// `NET_BASELINE_LOSS_*`: sixteen rounds of one controlled loss each.
/// The packet's ack never comes, the peer is irregular (resends by age),
/// and past `max(2*rtt, minResendDelay)` = 1.0s the scan pushes it back
/// out -- exactly once per round, the second scan at the same instant
/// finding nothing stale. The late ack then drains the window. Every
/// number here is clock arithmetic, so the record is identical on
/// every run.
#[test]
fn controlled_losses_are_recovered_exactly_once() {
    const ROUNDS: u32 = 16;
    let clock = Rc::new(Cell::new(1_000_000u64));
    let interface = interface(&clock);
    let channel = UdpChannel::new(
        &interface,
        addr(0x0100_0ba5),
        Traits::Internal,
        DEFAULT_INACTIVITY_RESEND_DELAY,
        CHANNEL_ID_NULL,
    );
    // A fresh channel already starts at seq 0 with highest_ack = SEQ_MASK(0-1),
    // the exact state the crate's `force_out_seq_at(0)` test hook produces,
    // so the sends below land on seq 0, 1, 2, ...
    UdpChannel::set_is_remote_regular(&channel, false);

    let mut rec = Record::default();
    let mut redeliveries = 0;
    for round in 0..ROUNDS {
        assert!(send_reliable(&channel), "round {round} send");
        // Half the period: young, nothing goes out.
        clock.set(clock.get() + 500_000);
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), redeliveries);

        // Past the 1.0s period: exactly one redelivery.
        clock.set(clock.get() + 1_000_000);
        UdpChannel::check_resend_timers(&channel);
        redeliveries += 1;
        assert_eq!(channel.borrow().num_packets_resent(), redeliveries);
        // The resend stamped itself newest; a same-instant re-scan is inert.
        UdpChannel::check_resend_timers(&channel);
        assert_eq!(channel.borrow().num_packets_resent(), redeliveries);

        // The ack lands late and drains the window.
        let seq = round & mercury::misc::SEQ_MASK;
        assert!(UdpChannel::handle_ack(&channel, seq));
        assert!(!channel.borrow().has_unacked_packets());
    }

    rec.put("loss_rounds", ROUNDS);
    rec.put("loss_redeliveries", redeliveries);
    rec.put("loss_resend_period_s", DEFAULT_INACTIVITY_RESEND_DELAY);
    rec.put("loss_exactly_once", redeliveries == ROUNDS);
    rec.put("loss_recovery_clock_us_per_round", 1_500_000u64);
    rec.put("loss_double_scan_resend", 0);
    rec.put("loss_deterministic", true);
    assert_eq!(redeliveries, ROUNDS, "every loss recovered exactly once");
    rec.flush("loss");
}

/// The other recovery arm, also deterministic: eight packets out, the
/// ack of seq 3 lost, the newer ack for seq 4 passes it -- the scan
/// resends the passed packet even on a regular channel, with no age
/// accumulated at all.
#[test]
fn a_passed_ack_recovers_the_packet_it_passed() {
    let clock = Rc::new(Cell::new(1_000_000u64));
    let interface = interface(&clock);
    let channel = UdpChannel::new(
        &interface,
        addr(0x0100_0ba6),
        Traits::Internal,
        DEFAULT_INACTIVITY_RESEND_DELAY,
        CHANNEL_ID_NULL,
    );
    // Fresh channel: sends land on seq 0..7 (see the note in the other leg).
    for _ in 0..8 {
        assert!(send_reliable(&channel));
    }
    // Ack everything but seq 3: its ack was the lost one. Only seq 3
    // stays unreleased (the outstanding-packet counter mixes bundle
    // units and send-window usage, so the slot view is the honest one).
    for seq in [0u32, 1, 2, 4, 5, 6, 7] {
        assert!(UdpChannel::handle_ack(&channel, seq), "ack {seq}");
    }
    assert!(
        channel.borrow().is_unacked(3),
        "seq 3 is the only live slot"
    );
    for seq in [0u32, 1, 2, 4, 5, 6, 7] {
        assert!(!channel.borrow().is_unacked(seq), "seq {seq} released");
    }

    clock.set(1_200_000);
    UdpChannel::check_resend_timers(&channel);
    assert_eq!(channel.borrow().num_packets_resent(), 1, "seq 3 alone");
    // Draining it closes the incident.
    assert!(UdpChannel::handle_ack(&channel, 3));
    assert!(!channel.borrow().has_unacked_packets());

    let mut rec = Record::default();
    rec.put("loss_passed_ack_window", 8);
    rec.put("loss_passed_ack_redeliveries", 1);
    rec.put("loss_passed_ack_outstanding_before_scan", 1);
    rec.put("loss_passed_ack_age_needed", false);
    rec.flush("loss-passed");
}

/// The baseline runner: the legs land in this one target so the whole
/// record family is produced by this single test (see the module doc
/// for the exact command). The leg tests above are each independent;
/// this one chains them in leg order for the saved record.
#[test]
fn the_network_baseline_runs_all_legs() {
    udp_loopback_rtt_is_recorded();
    udp_loopback_throughput_is_recorded();
    tcp_loopback_rtt_and_throughput_are_recorded();
    controlled_losses_are_recovered_exactly_once();
    a_passed_ack_recovers_the_packet_it_passed();
}

/// Guard on the surface the measurement depends on: a fresh endpoint
/// holds no socket and a closed one stops being good -- the same
/// precondition the loopback legs assume.
#[test]
fn a_closed_endpoint_stops_being_good() {
    let mut ep = udp_endpoint(INADDR_ANY);
    assert!(ep.good());
    assert_ne!(ep.fileno(), NO_SOCKET);
    assert_eq!(ep.close(), 0);
    assert!(!ep.good());
    assert_eq!(ep.fileno(), NO_SOCKET);
}
