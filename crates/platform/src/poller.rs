//! The event-polling primitives (`lib/network/event_poller.cpp`, `EPoller`).
//!
//! `EPoller` is `EventPoller::create()`'s choice on Linux: the reference
//! gates it on `HAS_EPOLL` (`__linux__ && !EMSCRIPTEN`). The port keeps the
//! reference's epoll-shaped `create`/`ctl`/`wait` contract on macOS too, and
//! backs it with kqueue: kqueue, like epoll, is a descriptor-persistent
//! registration set, so the same three calls map onto it one-to-one. The
//! reference's `SelectPoller` (`__APPLE__`) rebuilds its fd-sets per call,
//! which does not fit the `create`/`ctl`/`wait` shape, so the port's macOS
//! backend is kqueue rather than `select`.

#[cfg(target_os = "linux")]
mod epoll {
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

        /// A registered read side reports the byte, and the report carries
        /// the descriptor the byte was written to.
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
}

#[cfg(target_os = "macos")]
mod kqueue {
    use std::os::fd::RawFd;

    /// `EPOLL_CTL_ADD`, spelled as the epoll opcode so callers are
    /// platform-agnostic.
    pub const CTL_ADD: i32 = 1;
    /// `EPOLL_CTL_DEL`, spelled as the epoll opcode.
    pub const CTL_DEL: i32 = 2;
    /// `EPOLL_CTL_MOD`, spelled as the epoll opcode. kqueue's `EV_ADD`
    /// updates an existing filter, so `MOD` is the same call as `ADD`.
    pub const CTL_MOD: i32 = 3;

    /// Read interest, using the epoll bit values so a caller sees the same
    /// bits on both platforms.
    pub const EPOLLIN: u32 = 0x1;
    /// Write interest.
    pub const EPOLLOUT: u32 = 0x4;
    /// The `EV_ERROR` event, mapped to the epoll bit.
    pub const EPOLLERR: u32 = 0x8;
    /// The `EV_EOF` event, mapped to the epoll bit.
    pub const EPOLLHUP: u32 = 0x10;
    /// `ENOENT`, the `kevent` failure the reference reports as a warning
    /// rather than an error: nothing was registered, so removing it is not
    /// an error the caller has to act on.
    pub const ENOENT: i32 = libc::ENOENT;

    /// One entry `kevent` reported: the event bits and the descriptor the
    /// filter fired on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PollEvent {
        pub events: u32,
        pub fd: RawFd,
    }

    /// `kqueue()`. `EPoller(expectedSize)` passes a size to `epoll_create`;
    /// kqueue takes no size, so the argument is accepted and ignored, the
    /// same way `epoll_create1` treats it as advisory. `-1` when the kernel
    /// refuses.
    pub fn create(_expected_size: i32) -> RawFd {
        let fd = unsafe { libc::kqueue() };
        if fd < 0 { -1 } else { fd }
    }

    /// A single `kevent` change with no eventlist, so an error comes back as
    /// `-1` plus errno -- the spelling `epoll_ctl` uses.
    fn one_change(kqfd: RawFd, change: &libc::kevent) -> i32 {
        unsafe { libc::kevent(kqfd, change, 1, std::ptr::null_mut(), 0, std::ptr::null()) }
    }

    /// A zeroed `kevent` change tagged for `filter`/`flags`.
    fn change(fd: RawFd, filter: i16, flags: u16) -> libc::kevent {
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        event.ident = fd as libc::uintptr_t;
        event.filter = filter;
        event.flags = flags;
        event
    }

    /// `kevent(KQ_DELETE...)` on one filter. `EV_DELETE` of a filter that is
    /// not registered fails with `ENOENT`.
    fn delete_one(kqfd: RawFd, fd: RawFd, filter: i16) -> i32 {
        one_change(kqfd, &change(fd, filter, libc::EV_DELETE))
    }

    /// Register (`CTL_ADD`/`CTL_MOD`) or deregister (`CTL_DEL`) a
    /// descriptor's read/write interest: the single entry point behind the
    /// four `doRegisterFor{Read,Write}` / `doDeregisterFor{Read,Write}`
    /// hooks, which the reference folds into one `doRegister` with flags.
    ///
    /// kqueue registers one filter at a time, so a combined interest
    /// registers each filter it names. `CTL_DEL` removes the descriptor
    /// wholesale, matching epoll's `EPOLL_CTL_DEL`; it deletes the read
    /// filter, then the write one, and succeeds if either was registered.
    /// A descriptor no filter knows fails with `ENOENT`, the error the
    /// reference reports as a warning.
    pub fn ctl(kqfd: RawFd, op: i32, fd: RawFd, events: u32) -> i32 {
        if op == CTL_DEL {
            if delete_one(kqfd, fd, libc::EVFILT_READ) == 0 {
                let _ = delete_one(kqfd, fd, libc::EVFILT_WRITE);
                return 0;
            }
            return if delete_one(kqfd, fd, libc::EVFILT_WRITE) == 0 {
                0
            } else {
                -1
            };
        }

        if events & EPOLLIN != 0
            && one_change(kqfd, &change(fd, libc::EVFILT_READ, libc::EV_ADD)) < 0
        {
            return -1;
        }
        if events & EPOLLOUT != 0
            && one_change(kqfd, &change(fd, libc::EVFILT_WRITE, libc::EV_ADD)) < 0
        {
            return -1;
        }
        0
    }

    /// `kevent` wait: the count, or `-1` on error. Fills `events` in place
    /// and leaves the rest untouched. `timeout_ms` follows `epoll_wait`'s
    /// convention: negative blocks, zero polls, positive waits that long.
    pub fn wait(kqfd: RawFd, events: &mut [PollEvent], timeout_ms: i32) -> i32 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let ts_ptr: *const libc::timespec = if timeout_ms < 0 {
            std::ptr::null()
        } else {
            ts.tv_sec = (timeout_ms / 1000) as libc::time_t;
            ts.tv_nsec = ((timeout_ms % 1000) * 1_000_000) as libc::c_long;
            &ts
        };

        let mut raw: [libc::kevent; 10] = unsafe { std::mem::zeroed() };
        let count = events.len().min(raw.len());
        let result = unsafe {
            libc::kevent(
                kqfd,
                std::ptr::null(),
                0,
                raw.as_mut_ptr(),
                count as i32,
                ts_ptr,
            )
        };
        if result <= 0 {
            return result;
        }
        for (slot, event) in events.iter_mut().zip(raw.iter().take(result as usize)) {
            let mut bits = 0u32;
            if event.filter == libc::EVFILT_READ {
                bits |= EPOLLIN;
            } else if event.filter == libc::EVFILT_WRITE {
                bits |= EPOLLOUT;
            }
            if event.flags & libc::EV_EOF != 0 {
                bits |= EPOLLHUP;
            }
            if event.flags & libc::EV_ERROR != 0 {
                bits |= EPOLLERR;
            }
            *slot = PollEvent {
                events: bits,
                fd: event.ident as RawFd,
            };
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A usable kqueue file descriptor, or a skipped test.
        fn kqfd() -> RawFd {
            let fd = create(10);
            assert!(fd >= 0, "kqueue failed");
            fd
        }

        /// `kqueue` hands back a live descriptor and closes cleanly.
        #[test]
        fn create_returns_a_usable_descriptor() {
            let fd = kqfd();
            assert_eq!(crate::socket::close(fd), 0);
        }

        /// A registered read side reports the byte, and the report carries
        /// the descriptor the byte was written to.
        #[test]
        fn wait_reports_a_readable_side() {
            let fd = kqfd();
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
            let fd = kqfd();
            let mut events = [PollEvent { events: 0, fd: -1 }; 2];
            assert_eq!(wait(fd, &mut events, 0), 0, "nothing registered");
            crate::socket::close(fd);
        }

        /// `DEL` removes interest, and `DEL` of a descriptor kqueue never saw
        /// fails the way `EPoller::doRegister` expects (`-1`, `ENOENT`).
        #[test]
        fn del_fails_for_an_unknown_descriptor() {
            let fd = kqfd();
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
}

#[cfg(target_os = "linux")]
pub use epoll::*;
#[cfg(target_os = "macos")]
pub use kqueue::*;
