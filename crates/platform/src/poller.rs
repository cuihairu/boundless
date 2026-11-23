//! The `epoll` primitives (`lib/network/event_poller.cpp`, `EPoller`).
//!
//! `EPoller` is `EventPoller::create()`'s choice on Linux: the reference gates
//! it on `HAS_EPOLL` (`__linux__ && !EMSCRIPTEN`), so this module only builds
//! there -- the reference's `PollPoller` and `SelectPoller` backends are
//! out of scope, and the `SelectPoller` fallback is what a non-Linux build
//! gets.

use std::os::fd::RawFd;

/// `EPOLL_CTL_ADD`.
pub const CTL_ADD: i32 = libc::EPOLL_CTL_ADD;
/// `EPOLL_CTL_DEL`.
pub const CTL_DEL: i32 = libc::EPOLL_CTL_DEL;
/// `EPOLL_CTL_MOD`.
pub const CTL_MOD: i32 = libc::EPOLL_CTL_MOD;
/// `EPOLLIN`.
pub const EPOLLIN: u32 = libc::EPOLLIN as u32;
/// `EPOLLOUT`.
pub const EPOLLOUT: u32 = libc::EPOLLOUT as u32;
/// `EPOLLERR`.
pub const EPOLLERR: u32 = libc::EPOLLERR as u32;
/// `EPOLLHUP`.
pub const EPOLLHUP: u32 = libc::EPOLLHUP as u32;
/// `ENOENT`, the `epoll_ctl` failure the reference reports as a warning
/// rather than an error: nothing was registered, so removing it is not
/// an error the caller has to act on.
pub const ENOENT: i32 = libc::ENOENT;

/// `epoll_create1(0)`. `EPoller(expectedSize)` passes a size to the
/// obsolete `epoll_create`; the size is advisory on modern kernels, and
/// `epoll_create1` is the spelling that accepts no size. `-1` when the
/// kernel refuses.
pub fn create(expected_size: i32) -> RawFd {
    let _ = expected_size;
    let fd = unsafe { libc::epoll_create1(0) };
    if fd < 0 { -1 } else { fd }
}

/// `epoll_ctl(op, fd)`: the single entry point behind the four
/// `doRegisterFor{Read,Write}` / `doDeregisterFor{Read,Write}` hooks,
/// which the reference folds into one `doRegister` with flags.
pub fn ctl(epfd: RawFd, op: i32, fd: RawFd, events: u32) -> i32 {
    let mut event = libc::epoll_event {
        events,
        u64: fd as u64,
    };
    unsafe { libc::epoll_ctl(epfd, op, fd, &mut event) }
}

/// One entry `epoll_wait` reported: the event bits and the descriptor
/// epoll tagged onto the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollEvent {
    pub events: u32,
    pub fd: RawFd,
}

/// `epoll_wait(epfd, events, max, timeout_ms)`. The count, or `-1` on
/// error. Fills `events` in place and leaves the rest untouched.
pub fn wait(epfd: RawFd, events: &mut [PollEvent], timeout_ms: i32) -> i32 {
    let mut raw: [libc::epoll_event; 10] = unsafe { std::mem::zeroed() };
    let count = events.len().min(raw.len());
    let result = unsafe { libc::epoll_wait(epfd, raw.as_mut_ptr(), count as i32, timeout_ms) };
    if result <= 0 {
        return result;
    }
    for (slot, event) in events.iter_mut().zip(raw.iter().take(result as usize)) {
        *slot = PollEvent {
            events: event.events,
            fd: event.u64 as RawFd,
        };
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A usable epoll file descriptor, or a skipped test.
    fn epfd() -> RawFd {
        let fd = create(10);
        assert!(fd >= 0, "epoll_create1 failed");
        fd
    }

    /// `epoll_create1` hands back a live descriptor and closes cleanly.
    #[test]
    fn create_returns_a_usable_descriptor() {
        let fd = epfd();
        assert_eq!(crate::socket::close(fd), 0);
    }

    /// A registered read side reports the byte, and the report carries the
    /// descriptor the byte was written to.
    #[test]
    fn wait_reports_a_readable_side() {
        let fd = epfd();
        let (read_end, write_end) = crate::socket::create_pipe().expect("pipe");

        assert_eq!(ctl(fd, CTL_ADD, read_end, EPOLLIN), 0, "ADD the read side");
        let byte = b"x";
        assert_eq!(crate::socket::write(write_end, byte), 1);

        let mut events = [PollEvent { events: 0, fd: -1 }; 2];
        assert_eq!(wait(fd, &mut events, 0), 1, "one entry is ready");
        assert_eq!(events[0].fd, read_end);
        assert_ne!(events[0].events & EPOLLIN, 0, "EPOLLIN is set");

        crate::socket::close(read_end);
        crate::socket::close(write_end);
        crate::socket::close(fd);
    }

    /// With nothing to wait for, a zero timeout returns 0 rather than
    /// blocking -- this is what makes an idle poll stop normally.
    #[test]
    fn wait_is_idle_without_a_timeout() {
        let fd = epfd();
        let mut events = [PollEvent { events: 0, fd: -1 }; 2];
        assert_eq!(wait(fd, &mut events, 0), 0, "nothing registered");
        crate::socket::close(fd);
    }

    /// `DEL` removes interest, and `DEL` of a descriptor epoll never saw
    /// fails the way `EPoller::doRegister` expects (`-1`, `ENOENT`).
    #[test]
    fn del_fails_for_an_unknown_descriptor() {
        let fd = epfd();
        let (read_end, write_end) = crate::socket::create_pipe().expect("pipe");

        assert_eq!(ctl(fd, CTL_ADD, read_end, EPOLLIN), 0);
        assert_eq!(ctl(fd, CTL_DEL, read_end, 0), 0, "DEL the read side");
        assert_eq!(wait(fd, &mut [PollEvent { events: 0, fd: -1 }], 0), 0);

        let dead = crate::socket::descriptor_limit();
        assert_eq!(ctl(fd, CTL_DEL, dead, 0), -1, "ENOENT arm");
        assert_eq!(ctl(fd, CTL_ADD, -1, EPOLLIN), -1, "EBADF arm");

        crate::socket::close(read_end);
        crate::socket::close(write_end);
        crate::socket::close(fd);
    }
}
