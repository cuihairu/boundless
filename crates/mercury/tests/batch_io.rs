//! OPT-02-01-03: batched UDP send/receive and copy elimination.
//!
//! A loopback pair — both endpoints owned by the test thread — pushes
//! 3495 1200-byte datagrams two ways: the Linux `sendmmsg`/`recvmmsg`
//! wrappers (`platform::socket`, retained scratch, pooled receive
//! buffers) and the one-at-a-time `sendto`/`recvfrom` control. Every
//! send and receive syscall is individually timed, so the arms report
//! throughput plus per-datagram send and drain cost percentiles (the
//! tail). The allocation leg runs one warmed-up batch cycle — a socket
//! batching to itself — under a pointer-paired counting allocator:
//! pooled buffers and retained scratch mean the steady-state cycle
//! allocates nothing per datagram.
//!
//! Wall-clock numbers are machine-dependent and recorded, not banded;
//! the structural asserts (all datagrams arrive, syscall counts, zero
//! steady-state allocations) are deterministic.
//!
//! Canonical record run:
//!
//! ```sh
//! BATCH_IO_OUT=<absolute path> cargo test -p mercury --test batch_io \
//!     the_batch_io_evaluation_runs_all_legs -- --exact
//! ```

#[cfg(target_os = "linux")]
mod linux_legs {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    use mercury::endpoint::{Address, Endpoint};
    use platform::socket;

    const LOOPBACK: u32 = 0x0100_007f;
    const TOTAL: usize = 3495; // 4 MiB of 1200-byte datagrams
    const DATAGRAM: usize = 1200;
    const BATCH: usize = 64;
    /// In-flight cap: the kernel's default receive buffer drops past
    /// roughly 88 datagrams of this size (truesize accounting), so the
    /// paced window stays at one batch.
    const IN_FLIGHT: usize = BATCH;

    // ------------------------------------------------------------------
    // Counting allocator (pointer-paired ledger; see buffer_alloc.rs)
    // ------------------------------------------------------------------

    thread_local! {
        static TRACKING: Cell<bool> = const { Cell::new(false) };
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
        static LEDGER: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
        static LEDGERING: Cell<bool> = const { Cell::new(false) };
    }

    fn ledger_insert(ptr: *mut u8, size: usize) {
        let _ = LEDGER.try_with(|m| {
            LEDGERING.with(|g| g.set(true));
            m.borrow_mut().insert(ptr as usize, size);
            LEDGERING.with(|g| g.set(false));
        });
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
    }

    fn ledger_remove(ptr: *mut u8) {
        let _ = LEDGER.try_with(|m| {
            LEDGERING.with(|g| g.set(true));
            m.borrow_mut().remove(&(ptr as usize));
            LEDGERING.with(|g| g.set(false));
        });
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
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            if !LEDGERING.with(Cell::get) {
                ledger_remove(ptr);
            }
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    /// Runs `body` with allocation counting on for this thread.
    fn counted<T>(body: impl FnOnce() -> T) -> usize {
        ALLOCS.with(|c| c.set(0));
        TRACKING.with(|t| t.set(true));
        let _ = body();
        TRACKING.with(|t| t.set(false));
        ALLOCS.with(Cell::get)
    }

    // ------------------------------------------------------------------
    // Fixture record
    // ------------------------------------------------------------------

    #[derive(Default)]
    struct Record(Vec<(String, String)>);

    impl Record {
        fn put(&mut self, key: impl AsRef<str>, value: impl ToString) {
            self.0.push((key.as_ref().to_string(), value.to_string()));
        }

        fn flush(&self, tag: &str) {
            let mut text = String::new();
            for (key, value) in &self.0 {
                text.push_str(key);
                text.push('\t');
                text.push_str(value);
                text.push('\n');
            }
            if let Ok(path) = std::env::var("BATCH_IO_OUT") {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .and_then(|mut f| f.write_all(text.as_bytes()))
                    .expect("write BATCH_IO_OUT");
                println!("[{tag}] record: {path}");
            }
            let _ = std::io::stdout().flush();
        }
    }

    /// The p50/p90/p99/max of a sample set (microseconds).
    fn summarize(mut samples: Vec<f64>) -> [f64; 4] {
        assert!(!samples.is_empty(), "no samples");
        samples.sort_by(|a, b| a.total_cmp(b));
        let pick = |p: f64| {
            let idx = ((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len()) - 1;
            samples[idx]
        };
        [pick(0.5), pick(0.9), pick(0.99), samples[samples.len() - 1]]
    }

    fn put_summary(rec: &mut Record, stem: &str, summary: [f64; 4]) {
        for (name, value) in ["p50", "p90", "p99", "max"].iter().zip(summary) {
            rec.put(format!("{stem}_{name}"), format!("{value:.3}"));
        }
    }

    fn udp_bound() -> Endpoint {
        let mut ep = Endpoint::new();
        ep.socket(socket::SOCK_DGRAM);
        assert_eq!(ep.bind(0, LOOPBACK), 0, "udp bind failed");
        ep.set_nonblocking(true);
        ep
    }

    /// The pooled receive buffers. Each leg takes a fresh mutable view
    /// of the same boxes (the view vec is the only per-leg cost, and it
    /// is built outside the allocation window).
    fn pooled_bufs() -> Vec<Box<[u8]>> {
        (0..BATCH)
            .map(|_| vec![0u8; DATAGRAM].into_boxed_slice())
            .collect()
    }

    fn bufs_view(bufs: &mut [Box<[u8]>]) -> Vec<&mut [u8]> {
        bufs.iter_mut().map(|b| &mut b[..]).collect()
    }

    /// Per-arm measurements: wall clock, syscall counts, and the
    /// per-datagram send/receive cost samples in microseconds.
    #[derive(Default)]
    struct LegStats {
        secs: f64,
        got: usize,
        send_calls: usize,
        recv_calls: usize,
        send_per_dg: Vec<f64>,
        recv_per_dg: Vec<f64>,
    }

    /// One paced pump-drain leg. `send` pushes one datagram (control)
    /// or a batch (batch arm); `recv` drains whatever is readable.
    /// The retained scratch plus the receive metadata for one arm.
    struct BatchHandles {
        meta: Vec<(usize, u16, u32)>,
        send_scratch: socket::SendScratch,
        recv_scratch: socket::RecvScratch,
    }

    impl BatchHandles {
        fn new() -> Self {
            Self {
                meta: Vec::with_capacity(BATCH),
                send_scratch: socket::SendScratch::new(BATCH),
                recv_scratch: socket::RecvScratch::new(BATCH),
            }
        }
    }

    fn run_leg(
        client_fd: i32,
        peer: &Address,
        payload: &[u8],
        payrefs: &[&[u8]],
        bufrefs: &mut [&mut [u8]],
        handles: &mut BatchHandles,
        batched: bool,
    ) -> LegStats {
        let mut buf = vec![0u8; DATAGRAM];
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut stats = LegStats::default();

        let start = Instant::now();
        let (mut got, mut seq, mut in_flight) = (0usize, 0usize, 0usize);
        while got < TOTAL {
            if seq < TOTAL && in_flight < IN_FLIGHT {
                let t = Instant::now();
                if batched {
                    let n = std::cmp::min(BATCH, TOTAL - seq);
                    let sent = socket::sendmmsg(
                        client_fd,
                        &payrefs[..n],
                        peer.port,
                        peer.ip,
                        &mut handles.send_scratch,
                    );
                    assert!(sent > 0, "batch send failed: {sent}");
                    let us = t.elapsed().as_secs_f64() * 1e6 / sent as f64;
                    stats
                        .send_per_dg
                        .extend(std::iter::repeat_n(us, sent as usize));
                    seq += sent as usize;
                    in_flight += sent as usize;
                } else {
                    let n = socket::sendto(client_fd, payload, peer.port, peer.ip);
                    assert_eq!(n, DATAGRAM as i32, "single send failed: {n}");
                    stats.send_per_dg.push(t.elapsed().as_secs_f64() * 1e6);
                    seq += 1;
                    in_flight += 1;
                }
                stats.send_calls += 1;
            }
            let t = Instant::now();
            if batched {
                let n = socket::recvmmsg(
                    client_fd,
                    bufrefs,
                    &mut handles.meta,
                    &mut handles.recv_scratch,
                );
                if n > 0 {
                    for j in 0..n as usize {
                        assert_eq!(handles.meta[j].0, DATAGRAM, "short datagram");
                        assert_eq!(&bufrefs[j][..DATAGRAM], payload, "corrupted");
                    }
                    let us = t.elapsed().as_secs_f64() * 1e6 / n as f64;
                    stats
                        .recv_per_dg
                        .extend(std::iter::repeat_n(us, n as usize));
                    got += n as usize;
                    in_flight -= n as usize;
                } else {
                    assert!(
                        Instant::now() < deadline,
                        "batch drain stalled at {got}/{TOTAL}"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            } else {
                let (n, _, _) = socket::recvfrom(client_fd, &mut buf);
                if n > 0 {
                    assert_eq!(n as usize, DATAGRAM);
                    assert_eq!(&buf[..DATAGRAM], payload, "corrupted");
                    stats.recv_per_dg.push(t.elapsed().as_secs_f64() * 1e6);
                    got += 1;
                    in_flight -= 1;
                } else {
                    assert!(
                        Instant::now() < deadline,
                        "single drain stalled at {got}/{TOTAL}"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            stats.recv_calls += 1;
        }
        stats.secs = start.elapsed().as_secs_f64();
        stats.got = got;
        stats
    }

    #[test]
    fn the_batch_io_evaluation_runs_all_legs() {
        let client = udp_bound();
        let peer = client.local(); // the socket talks to itself on loopback
        let client_fd = client.fileno();
        let payload: Vec<u8> = (0..DATAGRAM).map(|j| (j * 7) as u8).collect();
        let payrefs: Vec<&[u8]> = vec![&payload; BATCH];
        let mut handles = BatchHandles::new();
        let mut bufs = pooled_bufs();

        // ---- batch arm ----
        let mut bufrefs = bufs_view(&mut bufs);
        let batch = run_leg(
            client_fd,
            &peer,
            &payload,
            &payrefs,
            &mut bufrefs,
            &mut handles,
            true,
        );
        assert_eq!(batch.got, TOTAL, "batch arm lost datagrams");
        assert!(
            batch.send_calls <= TOTAL / 8 + 2 && batch.recv_calls <= TOTAL / 8 + 2,
            "batch syscalls should amortize: sends {} recvs {} for {TOTAL}",
            batch.send_calls,
            batch.recv_calls
        );

        // ---- control arm: same workload, one datagram per syscall ----
        let mut bufrefs = bufs_view(&mut bufs);
        let single = run_leg(
            client_fd,
            &peer,
            &payload,
            &payrefs,
            &mut bufrefs,
            &mut handles,
            false,
        );
        assert_eq!(single.got, TOTAL, "control arm lost datagrams");
        assert_eq!(
            single.send_calls, TOTAL,
            "control sends are one syscall each"
        );
        assert!(single.recv_calls >= TOTAL);

        let mut rec = Record::default();
        rec.put("payload_bytes", DATAGRAM);
        rec.put("total_datagrams", TOTAL);
        rec.put("batch_width", BATCH);
        let mib = |s: &LegStats| s.got as f64 * DATAGRAM as f64 / s.secs / (1 << 20) as f64;
        rec.put("batch_seconds", format!("{:.6}", batch.secs));
        rec.put("batch_mib_per_s", format!("{:.3}", mib(&batch)));
        rec.put("batch_send_syscalls", batch.send_calls);
        rec.put("batch_recv_syscalls", batch.recv_calls);
        put_summary(
            &mut rec,
            "batch_send_per_dg_us",
            summarize(batch.send_per_dg),
        );
        put_summary(
            &mut rec,
            "batch_recv_per_dg_us",
            summarize(batch.recv_per_dg),
        );
        rec.put("single_seconds", format!("{:.6}", single.secs));
        rec.put("single_mib_per_s", format!("{:.3}", mib(&single)));
        rec.put("single_send_syscalls", single.send_calls);
        rec.put("single_recv_syscalls", single.recv_calls);
        put_summary(
            &mut rec,
            "single_send_per_dg_us",
            summarize(single.send_per_dg),
        );
        put_summary(
            &mut rec,
            "single_recv_per_dg_us",
            summarize(single.recv_per_dg),
        );

        // ---- allocation leg: one warmed-up self-batching cycle ----
        // The arms above already exercised every scratch capacity; this
        // cycle reuses them. `client_fd` batches to its own port, so
        // the datagrams come right back into the same socket.
        let mut bufrefs = bufs_view(&mut bufs);
        let cycle = |bufrefs: &mut [&mut [u8]], handles: &mut BatchHandles| {
            let sent = socket::sendmmsg(
                client_fd,
                &payrefs,
                peer.port,
                peer.ip,
                &mut handles.send_scratch,
            );
            assert_eq!(sent, BATCH as i32);
            let mut got = 0usize;
            let deadline = Instant::now() + Duration::from_secs(10);
            while got < BATCH {
                let n = socket::recvmmsg(
                    client_fd,
                    bufrefs,
                    &mut handles.meta,
                    &mut handles.recv_scratch,
                );
                if n > 0 {
                    got += n as usize;
                } else {
                    assert!(Instant::now() < deadline, "cycle stalled");
                }
            }
        };
        // Warm-up cycle outside the window.
        cycle(&mut bufrefs, &mut handles);
        let allocs = counted(|| cycle(&mut bufrefs, &mut handles));
        rec.put("batch_cycle_datagrams", BATCH);
        rec.put("batch_cycle_allocs", allocs);
        assert_eq!(
            allocs, 0,
            "pooled buffers and retained scratch allocate nothing per datagram"
        );

        rec.flush("batch-io");
    }
}

/// Non-Linux platforms: the batch syscalls are Linux-only; the record
/// states that instead of failing.
#[cfg(not(target_os = "linux"))]
#[test]
fn batch_syscalls_are_linux_only() {
    println!(
        "batch syscalls are linux-only; platform is {}",
        std::env::consts::OS
    );
}
