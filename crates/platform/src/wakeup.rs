//! The wakeup half of the event-poller abstraction (BW-15-01-01).
//!
//! An idle event loop is blocked inside `epoll_wait` (or the platform's
//! `select`/`kqueue` equivalent) for up to the wait budget. Setting a
//! break flag from another thread does not interrupt that blocked wait:
//! the loop only notices the flag after the wait returns. This type is
//! the self-pipe that makes an idle loop *interruptible* -- registering
//! [`Wakeup::read_fd`] with the poller and calling [`Wakeup::trigger`]
//! from any thread makes the blocked wait return, so the loop can notice
//! the break and stop. That is the "三端可停止空闲循环" acceptance.
//!
//! The mechanism is the classic self-pipe: `pipe(2)`. A one-byte write
//! to the write end makes the read end readable, which the poller reports
//! as an event; [`Wakeup::consume`] drains the byte to re-arm. The write
//! end is never closed for the lifetime of the `Wakeup`, so the read end
//! sees data, never EOF. Both ends are non-blocking so `trigger` and
//! `consume` never stall the caller.
//!
//! Unix-only: `pipe(2)` is the same call on Linux and macOS. The Windows
//! poller uses `WSAPoll` over sockets, so its wakeup needs a socketpair
//! rather than an anonymous pipe; that backend lands with the Windows
//! poller under BW-15-05 (the three native CI runners).

use crate::socket;
use std::os::fd::RawFd;

/// A self-pipe whose read end can be registered with a poller and whose
/// write end is triggered from any thread to interrupt a blocked wait.
pub struct Wakeup {
    read_fd: RawFd,
    write_fd: RawFd,
}

impl Wakeup {
    /// `pipe(2)` plus `O_NONBLOCK` on both ends. `Err` when the kernel
    /// refuses to create the pipe.
    pub fn new() -> std::io::Result<Self> {
        let (read_fd, write_fd) =
            socket::create_pipe().ok_or_else(std::io::Error::last_os_error)?;
        // Non-blocking on both ends: a full pipe must not stall `trigger`,
        // and an empty pipe must not stall `consume`.
        socket::set_nonblocking(read_fd, true);
        socket::set_nonblocking(write_fd, true);
        Ok(Self { read_fd, write_fd })
    }

    /// The read end, to register with the poller for read readiness.
    pub fn read_fd(&self) -> RawFd {
        self.read_fd
    }

    /// Wake a blocked wait: one byte into the pipe. Safe from any thread.
    ///
    /// `EAGAIN`/`EWOULDBLOCK` means the pipe is already full, so a wakeup
    /// byte is already pending and the wait is (or will be) interrupted;
    /// that is not an error. Any other failure is reported to stderr and
    /// otherwise ignored -- the loop may be shutting down and the fd may
    /// be closed underneath us.
    pub fn trigger(&self) {
        let n = socket::write(self.write_fd, &[1u8]);
        if n < 0 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {}
                _ => eprintln!("Wakeup::trigger: write failed: {err}"),
            }
        }
    }

    /// Drain the pending wakeup byte(s) and re-arm. Call after the poller
    /// reports the read end, before re-entering the blocked wait.
    pub fn consume(&self) {
        let mut buf = [0u8; 64];
        loop {
            match socket::read(self.read_fd, &mut buf) {
                n if n > 0 => {
                    // A short read means the pipe is drained.
                    if (n as usize) < buf.len() {
                        break;
                    }
                }
                _ => break,
            }
        }
    }
}

impl Drop for Wakeup {
    fn drop(&mut self) {
        socket::close(self.read_fd);
        socket::close(self.write_fd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poller::{self, PollEvent};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// One epoll set over a `Wakeup`'s read end, closed on drop. The
    /// wakeup is shared so a trigger thread can hold a clone.
    struct Waker {
        wakeup: Arc<Wakeup>,
        epfd: RawFd,
    }

    impl Waker {
        fn new() -> Self {
            let wakeup = Arc::new(Wakeup::new().expect("wakeup"));
            let epfd = poller::create(10);
            assert!(epfd >= 0, "epoll_create1 refused");
            let rc = poller::ctl(epfd, poller::CTL_ADD, wakeup.read_fd(), poller::EPOLLIN);
            assert_eq!(rc, 0, "epoll_ctl ADD failed");
            Self { wakeup, epfd }
        }

        fn wait(&self, timeout_ms: i32) -> i32 {
            let mut events = [PollEvent { events: 0, fd: 0 }];
            poller::wait(self.epfd, &mut events, timeout_ms)
        }
    }

    impl Drop for Waker {
        fn drop(&mut self) {
            let _ = poller::ctl(self.epfd, poller::CTL_DEL, self.wakeup.read_fd(), 0);
            socket::close(self.epfd);
        }
    }

    /// The acceptance: an idle loop blocked in `epoll_wait` is woken by a
    /// `trigger` from another thread well inside the wait budget -- the
    /// loop can stop instead of sleeping out the idle timeout.
    #[test]
    fn trigger_wakes_a_blocked_wait_promptly() {
        let waker = Waker::new();
        let wakeup = Arc::clone(&waker.wakeup);

        let triggerer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            wakeup.trigger();
        });

        // A 5s budget the trigger must cut short.
        let start = Instant::now();
        let n = waker.wait(5000);
        let elapsed = start.elapsed();

        triggerer.join().unwrap();
        assert_eq!(n, 1, "the wakeup must be the reported event");
        assert!(
            elapsed < Duration::from_millis(1000),
            "idle wait should be woken promptly, took {elapsed:?}"
        );
    }

    /// A trigger before the wait is an already-pending wakeup: the wait
    /// returns immediately with the event.
    #[test]
    fn pending_trigger_makes_wait_return_immediately() {
        let waker = Waker::new();
        waker.wakeup.trigger();
        let start = Instant::now();
        let n = waker.wait(5000);
        let elapsed = start.elapsed();
        assert_eq!(n, 1);
        assert!(elapsed < Duration::from_millis(500), "took {elapsed:?}");
    }

    /// `consume` re-arms: after draining, the read end is not readable
    /// and a zero-timeout wait reports nothing.
    #[test]
    fn consume_rearms_the_wakeup() {
        let waker = Waker::new();
        waker.wakeup.trigger();
        assert_eq!(waker.wait(5000), 1);
        waker.wakeup.consume();
        assert_eq!(waker.wait(0), 0, "consumed wakeup must not re-report");
    }

    /// `consume` on an un-triggered wakeup is a no-op.
    #[test]
    fn consume_without_trigger_is_a_noop() {
        let waker = Waker::new();
        waker.wakeup.consume();
        assert_eq!(waker.wait(0), 0);
    }

    /// Repeated trigger/consume cycles keep working: the pipe neither
    /// fills up nor reports stale events.
    #[test]
    fn repeated_trigger_consume_cycles() {
        let waker = Waker::new();
        for _ in 0..100 {
            waker.wakeup.trigger();
            assert_eq!(waker.wait(5000), 1);
            waker.wakeup.consume();
            assert_eq!(waker.wait(0), 0);
        }
    }
}
