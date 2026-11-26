//! OPT-15-01-03: 检验平台针对性优化 — the platform-targeted optimization
//! validated by the same functionality and protocol suite.
//!
//! The tree's platform-targeted optimization is the Linux
//! `sendmmsg`/`recvmmsg` batch pair behind `platform::socket` (the
//! OPT-02-01-03 delivery). The acceptance is that the same functionality
//! and protocol suite passes on the optimized path: the identical
//! datagram scenarios run through the generic one-datagram-per-syscall
//! arm (`sendto`/`recvfrom`) and the batched arm must produce identical
//! observable outcomes — the protocol contract the mercury endpoints sit
//! on, not a second implementation of it:
//!
//! - datagram boundary preservation (varying payload lengths, no
//!   coalescing, no splitting);
//! - byte-identical payloads, in order, under the paced window;
//! - per-datagram source address in network byte order, identical
//!   between `recvfrom` and `recvmmsg` triples;
//! - the drain-to-empty shape (`EAGAIN` on a quiet nonblocking socket,
//!   both arms, the same error);
//! - the edge inputs (empty batch, width above what is available, a
//!   single-datagram batch) answer exactly as the generic calls.
//!
//! Every structural result is asserted; syscall counts are asserted
//! where they are the optimization's own contract (one syscall per
//! batch) and printed for the rest.
//!
//! ```sh
//! cargo test -p platform --test platform_opt_parity
//! ```

#![cfg(target_os = "linux")]

use std::time::Duration;

use platform::socket::{self, EAGAIN, RecvScratch, SOCK_DGRAM, SendScratch};

const LOOPBACK: u32 = 0x0100_007f;
const DATAGRAMS: usize = 256;
const WINDOW: usize = 32;
const MAX_PAYLOAD: usize = 1200;

/// Datagram `seq`'s payload: a big-endian sequence tag plus a
/// deterministic fill, at a per-sequence length — the varying lengths
/// are what catch coalescing and splitting.
fn payload(seq: usize) -> Vec<u8> {
    let len = 16 + (seq * 37) % (MAX_PAYLOAD - 15);
    let mut data = Vec::with_capacity(len);
    data.extend_from_slice(&(seq as u32).to_be_bytes());
    let mut state = (seq as u64) | 0x9e37_79b9_7f4a_7c15;
    while data.len() < len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        data.push((state >> 33) as u8);
    }
    data
}

/// One arm's record: the datagrams in arrival order as
/// `(payload, source port, source ip)` — the protocol face both arms
/// must deliver identically — plus the syscall counts.
struct Leg {
    datagrams: Vec<(Vec<u8>, u16, u32)>,
    send_calls: usize,
    recv_calls: usize,
}

/// A loopback UDP pair: the sender blocking, the receiver nonblocking
/// (the drain's `EAGAIN` is part of the suite).
fn udp_pair() -> (std::os::fd::RawFd, std::os::fd::RawFd, u16) {
    let sender = socket::create_socket(SOCK_DGRAM);
    let receiver = socket::create_socket(SOCK_DGRAM);
    assert!(sender >= 0 && receiver >= 0, "both sockets created");
    assert_eq!(socket::bind(receiver, 0, LOOPBACK), 0, "receiver bound");
    assert_eq!(
        socket::set_nonblocking(receiver, true),
        0,
        "receiver nonblocking"
    );
    let (port, ip) = socket::local_address(receiver).expect("receiver address");
    assert_eq!(
        ip, LOOPBACK,
        "the receiver sits on the loopback interface (the in_addr u32)"
    );
    (sender, receiver, port)
}

/// Drain the receiver until `want` datagrams have arrived. Loopback UDP
/// delivery is deterministic once `send` returns (the datagram sits in
/// the receive queue); the bounded retry is armour, not a timing band.
fn drain_until(
    want: usize,
    recv_one: impl Fn(&mut [u8]) -> (i32, u16, u32),
    mut recv_calls: usize,
) -> (Vec<(Vec<u8>, u16, u32)>, usize) {
    let mut got = Vec::with_capacity(want);
    let mut idle_rounds = 0usize;
    let mut buf = vec![0u8; MAX_PAYLOAD];
    while got.len() < want {
        let (n, sport, sip) = recv_one(&mut buf);
        recv_calls += 1;
        if n < 0 {
            idle_rounds += 1;
            assert!(
                idle_rounds <= 100,
                "the window never completed: {} of {want}",
                got.len()
            );
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        idle_rounds = 0;
        assert!(n > 0, "a delivered datagram carries bytes");
        got.push((buf[..n as usize].to_vec(), sport, sip));
    }
    (got, recv_calls)
}

/// The generic arm: `sendto` one datagram per syscall, `recvfrom` one
/// datagram per syscall, window-paced.
fn generic_arm(sender: std::os::fd::RawFd, receiver: std::os::fd::RawFd, rport: u16) -> Leg {
    let mut send_calls = 0usize;
    let mut datagrams = Vec::with_capacity(DATAGRAMS);
    let mut recv_calls = 0usize;
    for window in 0..DATAGRAMS / WINDOW {
        let start = window * WINDOW;
        for seq in start..start + WINDOW {
            let data = payload(seq);
            let sent = socket::sendto(sender, &data, rport, LOOPBACK);
            send_calls += 1;
            assert_eq!(sent, data.len() as i32, "sendto accepted datagram {seq}");
        }
        let (got, calls) = drain_until(WINDOW, |buf| socket::recvfrom(receiver, buf), recv_calls);
        recv_calls = calls;
        datagrams.extend(got);
    }
    Leg {
        datagrams,
        send_calls,
        recv_calls,
    }
}

/// The optimized arm: `sendmmsg` one window per syscall, `recvmmsg`
/// draining in batches of `WINDOW`, the same pacing.
fn batch_arm(sender: std::os::fd::RawFd, receiver: std::os::fd::RawFd, rport: u16) -> Leg {
    let mut send_scratch = SendScratch::new(WINDOW);
    let mut recv_scratch = RecvScratch::new(WINDOW);
    let mut pool: Vec<Vec<u8>> = (0..WINDOW).map(|_| vec![0u8; MAX_PAYLOAD]).collect();
    let mut send_calls = 0usize;
    let mut datagrams = Vec::with_capacity(DATAGRAMS);
    let mut recv_calls = 0usize;
    for window in 0..DATAGRAMS / WINDOW {
        let start = window * WINDOW;
        let owned: Vec<Vec<u8>> = (start..start + WINDOW).map(payload).collect();
        let batch: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
        let accepted = socket::sendmmsg(sender, &batch, rport, LOOPBACK, &mut send_scratch);
        send_calls += 1;
        assert_eq!(
            accepted, WINDOW as i32,
            "sendmmsg accepted the whole window"
        );
        let mut got: Vec<(Vec<u8>, u16, u32)> = Vec::with_capacity(WINDOW);
        while got.len() < WINDOW {
            let mut bufs: Vec<&mut [u8]> = pool.iter_mut().map(|b| &mut b[..]).collect();
            let mut out = Vec::with_capacity(WINDOW);
            let n = socket::recvmmsg(receiver, &mut bufs, &mut out, &mut recv_scratch);
            recv_calls += 1;
            if n < 0 {
                let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                assert_eq!(errno, EAGAIN, "the only quiet-socket answer is EAGAIN");
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            // the pool buffers and the triples are both in batch order
            for (slot, (len, sport, sip)) in out.iter().enumerate() {
                got.push((pool[slot][..*len].to_vec(), *sport, *sip));
            }
            assert_eq!(
                got.len(),
                n as usize,
                "recvmmsg reported every datagram it filled"
            );
        }
        datagrams.extend(got);
    }
    Leg {
        datagrams,
        send_calls,
        recv_calls,
    }
}

#[test]
fn the_batch_optimization_passes_the_generic_arms_protocol_suite() {
    // one pair for both arms: the source triples the two arms report
    // must be identical, so both arms ride the same sender socket
    let (sender, receiver, rport) = udp_pair();
    let generic = generic_arm(sender, receiver, rport);
    let batch = batch_arm(sender, receiver, rport);
    unsafe {
        libc::close(sender);
        libc::close(receiver);
    }

    // the same suite, the same outcomes: datagram for datagram, the
    // optimized arm hands over what the generic arm hands over — same
    // count, same order, byte-identical payloads, identical source
    // addresses in network byte order
    assert_eq!(
        generic.datagrams.len(),
        DATAGRAMS,
        "the generic arm delivered"
    );
    assert_eq!(batch.datagrams.len(), DATAGRAMS, "the batch arm delivered");
    for (index, (a, b)) in generic.datagrams.iter().zip(&batch.datagrams).enumerate() {
        assert_eq!(a.0, b.0, "datagram {index}: byte-identical payload");
        assert_eq!(
            a.0.len(),
            payload(index).len(),
            "datagram {index}: boundary kept"
        );
        assert_eq!(a.1, b.1, "datagram {index}: identical source port");
        assert_eq!(a.2, b.2, "datagram {index}: identical source address");
    }
    // in order: the sequence tag inside each payload is its own index
    for (index, (data, _, _)) in batch.datagrams.iter().enumerate() {
        assert_eq!(
            &data[..4],
            &(index as u32).to_be_bytes(),
            "datagram {index}: arrived in send order"
        );
    }

    // the optimization's own syscall contract: one send syscall per
    // window against one per datagram, and strictly fewer receive
    // syscalls than datagrams
    assert_eq!(
        generic.send_calls, DATAGRAMS,
        "the generic arm: one per datagram"
    );
    assert_eq!(
        batch.send_calls,
        DATAGRAMS / WINDOW,
        "the batch arm: one per window"
    );
    assert!(
        batch.recv_calls < DATAGRAMS,
        "the batch arm drained in {} recv syscalls for {DATAGRAMS} datagrams",
        batch.recv_calls
    );

    eprintln!(
        "PARITY generic datagrams={} send_calls={} recv_calls={}",
        generic.datagrams.len(),
        generic.send_calls,
        generic.recv_calls
    );
    eprintln!(
        "PARITY batch datagrams={} send_calls={} recv_calls={}",
        batch.datagrams.len(),
        batch.send_calls,
        batch.recv_calls
    );
}

#[test]
fn the_batch_api_edges_answer_as_the_generic_api() {
    // empty batch: zero datagrams requested, zero sent, and an empty
    // receive request reads nothing without touching a buffer
    let (sender, receiver, rport) = udp_pair();
    let mut send_scratch = SendScratch::new(8);
    let mut recv_scratch = RecvScratch::new(8);
    assert_eq!(
        socket::sendmmsg(sender, &[], rport, LOOPBACK, &mut send_scratch),
        0,
        "an empty batch sends nothing"
    );
    // eight receive slots: wider than the five-datagram leg below, so
    // "width above what is available" really is wider
    let mut pool: Vec<Vec<u8>> = (0..8).map(|_| vec![0u8; MAX_PAYLOAD]).collect();
    let mut bufs: Vec<&mut [u8]> = pool.iter_mut().map(|b| &mut b[..]).collect();
    let mut out = Vec::new();
    assert_eq!(
        socket::recvmmsg(receiver, &mut [], &mut out, &mut recv_scratch),
        0,
        "an empty receive request reads nothing"
    );

    // a quiet nonblocking socket: both arms answer with the same error,
    // the drain-to-empty shape (the wrappers surface -1 with errno set)
    let quiet_generic = socket::recvfrom(receiver, &mut [0u8; 64]);
    assert!(quiet_generic.0 < 0, "recvfrom: drained");
    let quiet_generic_errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let quiet_batch = socket::recvmmsg(receiver, &mut bufs, &mut out, &mut recv_scratch);
    assert!(quiet_batch < 0, "recvmmsg: drained");
    let quiet_batch_errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(
        quiet_generic_errno, quiet_batch_errno,
        "both arms name the same drained error"
    );
    assert_eq!(quiet_generic_errno, EAGAIN, "the drained error is EAGAIN");

    // width above what is available: exactly what was sent comes back
    let five_owned: Vec<Vec<u8>> = (0..5).map(payload).collect();
    let five: Vec<&[u8]> = five_owned.iter().map(Vec::as_slice).collect();
    assert_eq!(
        socket::sendmmsg(sender, &five, rport, LOOPBACK, &mut send_scratch),
        5,
        "the five-datagram batch accepted"
    );
    let n = socket::recvmmsg(receiver, &mut bufs, &mut out, &mut recv_scratch);
    assert_eq!(n, 5, "an 8-slot request returns the 5 available datagrams");
    assert_eq!(out.len(), 5);
    for (slot, (len, _, _)) in out.iter().enumerate() {
        assert_eq!(
            &pool[slot][..*len],
            &payload(slot),
            "slot {slot}: byte-identical"
        );
    }

    // a single-datagram batch rides the same contract as sendto:
    // sendmmsg one, recvfrom one
    let one_owned = [payload(99)];
    let one: Vec<&[u8]> = one_owned.iter().map(Vec::as_slice).collect();
    assert_eq!(
        socket::sendmmsg(sender, &one, rport, LOOPBACK, &mut send_scratch),
        1
    );
    let mut buf = [0u8; MAX_PAYLOAD];
    let (n, sport, sip) = socket::recvfrom(receiver, &mut buf);
    assert_eq!(
        n,
        payload(99).len() as i32,
        "recvfrom: the batched datagram"
    );
    assert_eq!(&buf[..n as usize], &payload(99), "byte-identical");
    let (sender_port, _) = socket::local_address(sender).expect("sender address");
    // the source triple recvfrom reports: the sender's own ephemeral
    // port and the loopback interface the packet rode (an implicitly
    // bound sender reports 0.0.0.0 to getsockname, so the observable
    // source is what the receiver saw)
    assert_eq!(sport, sender_port, "source port is the sender's");
    assert_eq!(sip, LOOPBACK, "source address is the loopback");

    unsafe {
        libc::close(sender);
        libc::close(receiver);
    }
}
