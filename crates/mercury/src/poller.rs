//! BW-02-01-02 the event poller: registering, deregistering and waking
//! handlers on file descriptors (`lib/network/event_poller.{hpp,cpp}`).
//!
//! Linux only, matching the reference's `HAS_EPOLL` build: `create()` wires
//! the `EPoller` backend, and the platform's `epoll` calls live in
//! `platform::poller`. The reference's `PollPoller` (EMSCRIPTEN) and
//! `SelectPoller` (everywhere else) arms are not ported, so a non-Linux
//! build has no poller until the portability item lands a backend.
//!
//! The C++ shape is an abstract base whose concrete pollers each hand-roll
//! their own wait loop. Here the base is a struct that owns the handler
//! maps, the trigger dispatch and the spare-time bookkeeping, and a
//! `PollerBackend` trait object supplies the per-OS arms -- the reference's
//! three wait loops dispatch ready events identically (`EPOLLERR|EPOLLHUP`
//! to `triggerError`, `EPOLLIN` to `triggerRead`, `EPOLLOUT` to
//! `triggerWrite`), so the loop lives once in the struct and the backend
//! only waits. Deviation documented here rather than per-method.
//!
//! Handlers are shared, not owned: the reference stores a raw
//! `InputNotificationHandler *` the poller never deletes, so registrations
//! take an `Rc<dyn InputNotificationHandler>` and a handler can sit behind
//! several descriptors (and behind a parent poller, once the dispatcher
//! chains them).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::os::fd::RawFd;
use std::rc::Rc;

use platform::poller::{self as epoll, PollEvent};

/// `Mercury::InputNotificationHandler` (`network/interfaces.hpp`): the
/// surface a registered descriptor reports on.
pub trait InputNotificationHandler {
    /// `handleInputNotification`: the descriptor is ready for reading (or
    /// writing, per the registration). The return value is ignored;
    /// implementors return 0.
    fn handle_input_notification(&self, fd: RawFd) -> i32;

    /// `handleErrorNotification`: an error condition was detected. `true`
    /// means the handler cleared it; `false` falls back to input
    /// notification. The reference's default just returns false.
    fn handle_error_notification(&self, _fd: RawFd) -> bool {
        false
    }

    /// `isReadyForShutdown`: asked while the dispatcher is shutting down.
    fn is_ready_for_shutdown(&self) -> bool {
        true
    }
}

/// `InputHandlerEntry`: a registered handler with the name and the counts
/// the watchers expose. The trigger and error counters wrap, like the
/// reference's `uint32`s.
pub struct InputHandlerEntry {
    handler: Rc<dyn InputNotificationHandler>,
    name: String,
    num_triggers: Cell<u32>,
    num_errors: Cell<u32>,
}

impl InputHandlerEntry {
    /// The reference's `INFORM_COUNT`: report every this many triggers.
    pub const INFORM_COUNT: u32 = 100_000;

    /// `InputHandlerEntry(pHandler, name)`.
    pub fn new(handler: Rc<dyn InputNotificationHandler>, name: &str) -> Self {
        Self {
            handler,
            name: name.to_string(),
            num_triggers: Cell::new(0),
            num_errors: Cell::new(0),
        }
    }

    /// `handleInputNotification`: count, report on the boundary, forward.
    pub fn handle_input_notification(&self, fd: RawFd) -> i32 {
        let triggers = self.num_triggers.get().wrapping_add(1);
        self.num_triggers.set(triggers);

        if triggers.is_multiple_of(Self::INFORM_COUNT) {
            eprintln!(
                "InputHandlerEntry::handleInputNotification: \
                 The fd {} ({}) has triggered {} times",
                fd, self.name, triggers
            );
        }

        self.handler.handle_input_notification(fd)
    }

    /// `handleErrorNotification`: the error-counting twin.
    pub fn handle_error_notification(&self, fd: RawFd) -> bool {
        let errors = self.num_errors.get().wrapping_add(1);
        self.num_errors.set(errors);

        if errors.is_multiple_of(Self::INFORM_COUNT) {
            eprintln!(
                "InputHandlerEntry::handleErrorNotification: \
                 The fd {} ({}) has triggered {} times",
                fd, self.name, errors
            );
        }

        self.handler.handle_error_notification(fd)
    }

    /// Forwards to the handler, the way the watcher asks.
    pub fn is_ready_for_shutdown(&self) -> bool {
        self.handler.is_ready_for_shutdown()
    }

    /// `name`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `numTriggers`.
    pub fn num_triggers(&self) -> u32 {
        self.num_triggers.get()
    }

    /// `numErrors`.
    pub fn num_errors(&self) -> u32 {
        self.num_errors.get()
    }
}

/// The per-OS arms of the reference's abstract `EventPoller`: the
/// `doRegisterFor{Read,Write}` / `doDeregisterFor{Read,Write}` hooks folded
/// into one `doRegister`, and the raw wait.
pub trait PollerBackend {
    /// `EPoller::doRegister(fd, isRead, isRegister, name)` reduced to its
    /// `epoll_ctl` facts. `opposite_registered` is the poller's
    /// `isRegistered(fd, !is_read)` answer, which picks `EPOLL_CTL_MOD`
    /// over `ADD`/`DEL`; the reference's backends up-call into the base's
    /// maps for it, so here it arrives as a parameter.
    fn do_register(
        &self,
        fd: RawFd,
        is_read: bool,
        is_register: bool,
        name: Option<&str>,
        opposite_registered: bool,
    ) -> bool;

    /// `epoll_wait`, or the backend's equivalent: report the ready entries
    /// into `events` and return their count, `-1` on error.
    fn wait(&self, timeout_ms: i32, events: &mut [PollEvent]) -> i32;

    /// `getFileDescriptor` when the backend owns a pollable descriptor of
    /// its own (an epoll set); the base's default is no descriptor.
    fn file_descriptor(&self) -> RawFd {
        -1
    }
}

/// `EPoller`: the Linux backend, `create()`'s choice under `HAS_EPOLL`.
struct EpollBackend {
    epfd: RawFd,
}

impl EpollBackend {
    /// `EPoller(expectedSize = 10)`. The reference reports a failed
    /// `epoll_create` with `CRITICAL_MSG`, which aborts; the port
    /// panics with the same message, as every CRITICAL site is.
    fn new() -> Self {
        Self::from_fd(epoll::create(10))
    }

    /// The constructor body, split out so a failed create can be handed
    /// in directly: the kernel never refuses `epoll_create1` in tests.
    fn from_fd(epfd: RawFd) -> Self {
        if epfd < 0 {
            panic!(
                "EPoller::EPoller: epoll_create failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Self { epfd }
    }
}

impl PollerBackend for EpollBackend {
    fn do_register(
        &self,
        fd: RawFd,
        is_read: bool,
        is_register: bool,
        name: Option<&str>,
        opposite_registered: bool,
    ) -> bool {
        let (op, events) = if opposite_registered {
            // Already registered for the opposite direction: this changes
            // the interest set rather than adding or removing it.
            let both = epoll::EPOLLIN | epoll::EPOLLOUT;
            (
                epoll::CTL_MOD,
                if is_register {
                    both
                } else if is_read {
                    epoll::EPOLLOUT
                } else {
                    epoll::EPOLLIN
                },
            )
        } else {
            (
                if is_register {
                    epoll::CTL_ADD
                } else {
                    epoll::CTL_DEL
                },
                if is_read {
                    epoll::EPOLLIN
                } else {
                    epoll::EPOLLOUT
                },
            )
        };

        if epoll::ctl(self.epfd, op, fd, events) < 0 {
            let error = std::io::Error::last_os_error();
            // The reference's WARNING/ERROR split, named literally: ENOENT
            // (the fd was never registered, so removing it is not an error)
            // versus everything else.
            let level = if error.raw_os_error() == Some(epoll::ENOENT) {
                "WARNING"
            } else {
                "ERROR"
            };
            eprintln!(
                "{} EPoller::doRegister: Failed to {} {} file descriptor {}{} ({})",
                level,
                if is_register { "add" } else { "remove" },
                if is_read { "read" } else { "write" },
                name.map(|n| format!("{n} ")).unwrap_or_default(),
                fd,
                error
            );
            return false;
        }

        true
    }

    fn wait(&self, timeout_ms: i32, events: &mut [PollEvent]) -> i32 {
        epoll::wait(self.epfd, events, timeout_ms)
    }

    fn file_descriptor(&self) -> RawFd {
        self.epfd
    }
}

/// `Mercury::EventPoller`: the fd-to-handler maps plus a backend.
///
/// Not `Clone`: the maps hand out handler references, and a clone's
/// registrations would not be reflected in the original's backend.
pub struct EventPoller {
    /// `fdReadHandlers_`.
    fd_read_handlers: BTreeMap<RawFd, InputHandlerEntry>,
    /// `fdWriteHandlers_`.
    fd_write_handlers: BTreeMap<RawFd, InputHandlerEntry>,
    /// `spareTime_`, microseconds spent waiting; a `Cell` so the dispatch
    /// path can stay `&self` (the poller registers itself as a handler
    /// with parent dispatchers, whose trait methods take `&self`).
    spare_time: Cell<u64>,
    /// The concrete poller's OS arms.
    backend: Box<dyn PollerBackend>,
}

/// The reference's `MAX_EVENTS`: one `epoll_wait` reports at most this
/// many entries per pass.
const MAX_EVENTS: usize = 10;

impl EventPoller {
    /// `EventPoller::create()`: the platform poller, `EPoller` under
    /// `HAS_EPOLL`.
    pub fn create() -> Self {
        Self::new(Box::new(EpollBackend::new()))
    }

    /// Build over an injected backend; the reference reaches the same
    /// shape through subclassing.
    pub fn new(backend: Box<dyn PollerBackend>) -> Self {
        Self {
            fd_read_handlers: BTreeMap::new(),
            fd_write_handlers: BTreeMap::new(),
            spare_time: Cell::new(0),
            backend,
        }
    }

    /// `registerForRead`: the backend must accept the descriptor before
    /// the map claims it.
    pub fn register_for_read(
        &mut self,
        fd: RawFd,
        handler: Rc<dyn InputNotificationHandler>,
        name: &str,
    ) -> bool {
        let opposite = self.is_registered(fd, false);
        if !self
            .backend
            .do_register(fd, true, true, Some(name), opposite)
        {
            return false;
        }
        self.fd_read_handlers
            .insert(fd, InputHandlerEntry::new(handler, name));
        true
    }

    /// `registerForWrite`.
    pub fn register_for_write(
        &mut self,
        fd: RawFd,
        handler: Rc<dyn InputNotificationHandler>,
        name: &str,
    ) -> bool {
        let opposite = self.is_registered(fd, true);
        if !self
            .backend
            .do_register(fd, false, true, Some(name), opposite)
        {
            return false;
        }
        self.fd_write_handlers
            .insert(fd, InputHandlerEntry::new(handler, name));
        true
    }

    /// `deregisterForRead`: the map entry goes first, so the backend's
    /// MOD-or-DEL decision sees the post-erase registration state.
    pub fn deregister_for_read(&mut self, fd: RawFd) -> bool {
        self.fd_read_handlers.remove(&fd);
        let opposite = self.is_registered(fd, false);
        self.backend.do_register(fd, true, false, None, opposite)
    }

    /// `deregisterForWrite`.
    pub fn deregister_for_write(&mut self, fd: RawFd) -> bool {
        self.fd_write_handlers.remove(&fd);
        let opposite = self.is_registered(fd, true);
        self.backend.do_register(fd, false, false, None, opposite)
    }

    /// `triggerRead`/`triggerWrite`: dispatch to a registered handler.
    /// Both share `trigger(fd, handlers)`.
    pub fn trigger_read(&self, fd: RawFd) -> bool {
        self.trigger(fd, &self.fd_read_handlers)
    }

    /// `triggerWrite`.
    pub fn trigger_write(&self, fd: RawFd) -> bool {
        self.trigger(fd, &self.fd_write_handlers)
    }

    /// `trigger`: report the descriptor to its entry, if it has one.
    fn trigger(&self, fd: RawFd, handlers: &BTreeMap<RawFd, InputHandlerEntry>) -> bool {
        match handlers.get(&fd) {
            None => false,
            Some(entry) => {
                entry.handle_input_notification(fd);
                true
            }
        }
    }

    /// `triggerError`: the read handler claims the error first, the write
    /// handler only when no read handler exists; an unclaimed error falls
    /// back to input notification, and an unknown descriptor reports
    /// false.
    pub fn trigger_error(&self, fd: RawFd) -> bool {
        for handlers in [&self.fd_read_handlers, &self.fd_write_handlers] {
            if let Some(entry) = handlers.get(&fd) {
                if !entry.handle_error_notification(fd) {
                    // Error notification did not handle the error.
                    // Fallback to input notification.
                    entry.handle_input_notification(fd);
                }
                return true;
            }
        }
        false
    }

    /// `isRegistered(fd, isForRead)`.
    pub fn is_registered(&self, fd: RawFd, is_for_read: bool) -> bool {
        let handlers = if is_for_read {
            &self.fd_read_handlers
        } else {
            &self.fd_write_handlers
        };
        handlers.contains_key(&fd)
    }

    /// `isReadyForShutdown`: the conjunction over every registered
    /// handler's readiness. An empty poller is ready.
    pub fn is_ready_for_shutdown(&self) -> bool {
        [&self.fd_read_handlers, &self.fd_write_handlers]
            .iter()
            .all(|handlers| handlers.values().all(|e| e.is_ready_for_shutdown()))
    }

    /// `maxFD()`: the largest descriptor across both maps, `-1` when both
    /// are empty.
    pub fn max_fd(&self) -> RawFd {
        let read_max = self.fd_read_handlers.keys().copied().max().unwrap_or(-1);
        let write_max = self.fd_write_handlers.keys().copied().max().unwrap_or(-1);
        read_max.max(write_max)
    }

    /// `processPendingEvents(maxWait)`: wait, then dispatch. An error or
    /// hangup routes to `triggerError`; readable and writable report
    /// through their own triggers.
    pub fn process_pending_events(&self, max_wait: f64) -> i32 {
        // `int( ceil( maxWait * 1000 ) )`: fractional waits round up to
        // the whole millisecond epoll waits.
        let timeout_ms = (max_wait * 1000.0).ceil() as i32;

        let start = platform::clock::monotonic_now_micros();
        let mut events = [PollEvent { events: 0, fd: -1 }; MAX_EVENTS];
        let count = self.backend.wait(timeout_ms, &mut events);
        let elapsed = platform::clock::monotonic_now_micros()
            .saturating_duration_since(start)
            .micros();
        self.spare_time.set(self.spare_time.get() + elapsed);

        for event in events.iter().take(count.max(0) as usize) {
            if event.events & (epoll::EPOLLERR | epoll::EPOLLHUP) != 0 {
                self.trigger_error(event.fd);
            } else {
                if event.events & epoll::EPOLLIN != 0 {
                    self.trigger_read(event.fd);
                }
                if event.events & epoll::EPOLLOUT != 0 {
                    self.trigger_write(event.fd);
                }
            }
        }

        count
    }

    /// `getFileDescriptor`: the backend's epoll set, or `-1` when it has
    /// none -- used to register this poller with a parent dispatcher's
    /// poller.
    pub fn file_descriptor(&self) -> RawFd {
        self.backend.file_descriptor()
    }

    /// `spareTime`.
    pub fn spare_time(&self) -> u64 {
        self.spare_time.get()
    }

    /// `clearSpareTime`.
    pub fn clear_spare_time(&self) {
        self.spare_time.set(0);
    }
}

impl InputNotificationHandler for EventPoller {
    /// `handleInputNotification`: run one zero-timeout poll pass. This is
    /// the wake path -- a parent dispatcher registers this poller's own
    /// descriptor, and activity there makes the child process its events.
    fn handle_input_notification(&self, _fd: RawFd) -> i32 {
        self.process_pending_events(0.0);
        0
    }

    /// Shutdown readiness is the conjunction over the registered
    /// handlers, the same non-virtual method the reference's override
    /// reaches.
    fn is_ready_for_shutdown(&self) -> bool {
        EventPoller::is_ready_for_shutdown(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor the kernel will never hand out, for the failure
    /// paths. A *closed* descriptor would not do: closing frees its
    /// number, and a sibling test thread reopens it straight away, so the
    /// poller would be naming someone else's pipe or socket. The fd
    /// limit is refused forever, which keeps the failures deterministic.
    fn dead_fd() -> RawFd {
        platform::socket::descriptor_limit()
    }

    /// The reference's `FakeHandler`: counts its calls, with switchable
    /// error handling and shutdown readiness.
    struct FakeHandler {
        input_calls: Cell<u32>,
        error_calls: Cell<u32>,
        error_result: Cell<bool>,
        ready: Cell<bool>,
    }

    impl FakeHandler {
        fn new() -> Rc<Self> {
            Rc::new(Self {
                input_calls: Cell::new(0),
                error_calls: Cell::new(0),
                error_result: Cell::new(false),
                ready: Cell::new(true),
            })
        }
    }

    impl InputNotificationHandler for FakeHandler {
        fn handle_input_notification(&self, _fd: RawFd) -> i32 {
            self.input_calls.set(self.input_calls.get() + 1);
            0
        }

        fn handle_error_notification(&self, _fd: RawFd) -> bool {
            self.error_calls.set(self.error_calls.get() + 1);
            self.error_result.get()
        }

        fn is_ready_for_shutdown(&self) -> bool {
            self.ready.get()
        }
    }

    /// The stub's controls, alive after the poller took the backend.
    #[derive(Clone, Default)]
    struct StubControls {
        register_calls: Rc<Cell<u32>>,
        register_result: Rc<Cell<bool>>,
        wait_calls: Rc<Cell<u32>>,
    }

    impl StubControls {
        fn fail_registers(&self) {
            self.register_result.set(false);
        }
    }

    /// The reference's `StubPoller`: a backend with no OS behind it, so
    /// the base machinery runs without a real poll set.
    struct StubBackend {
        controls: StubControls,
    }

    impl StubBackend {
        fn pair() -> (Self, StubControls) {
            // `StubControls::default` leaves `register_result` false; the
            // reference's `StubPoller` defaults its hook to succeed.
            let controls = StubControls::default();
            controls.register_result.set(true);
            (Self { controls: controls.clone() }, controls)
        }
    }

    impl PollerBackend for StubBackend {
        fn do_register(
            &self,
            _fd: RawFd,
            _is_read: bool,
            is_register: bool,
            _name: Option<&str>,
            _opposite_registered: bool,
        ) -> bool {
            // The reference's stub counts the register hooks only.
            if is_register {
                self.controls
                    .register_calls
                    .set(self.controls.register_calls.get() + 1);
            }
            self.controls.register_result.get()
        }

        fn wait(&self, _timeout_ms: i32, _events: &mut [PollEvent]) -> i32 {
            self.controls
                .wait_calls
                .set(self.controls.wait_calls.get() + 1);
            0
        }
    }

    /// The reference's `PipePair`: a connected pair standing in for
    /// sockets, closed on drop.
    struct PipePair {
        fds: [RawFd; 2],
    }

    impl PipePair {
        fn new() -> Self {
            let (read_end, write_end) = platform::socket::create_pipe().expect("pipe");
            Self {
                fds: [read_end, write_end],
            }
        }

        fn read_end(&self) -> RawFd {
            self.fds[0]
        }

        fn write_end(&self) -> RawFd {
            self.fds[1]
        }

        /// Close the write end, hanging up the read end, and disarm the
        /// destructor entry so the descriptor is not closed twice.
        fn close_write_end(&mut self) {
            platform::socket::close(self.fds[1]);
            self.fds[1] = -1;
        }
    }

    impl Drop for PipePair {
        fn drop(&mut self) {
            for fd in self.fds {
                if fd != -1 {
                    platform::socket::close(fd);
                }
            }
        }
    }

    /// The registration maps: backend failure must leave no map entry,
    /// and successful register/deregister round-trips update
    /// `isRegistered`.
    #[test]
    fn register_maps_track_backend_result() {
        let (backend, controls) = StubBackend::pair();
        let mut poller = EventPoller::new(Box::new(backend));
        let handler = FakeHandler::new();

        // Failure from the backend must not register anything.
        controls.fail_registers();
        assert!(!poller.register_for_read(7, Rc::clone(&handler) as _, "bad"));
        assert!(!poller.is_registered(7, true));
        assert!(!poller.register_for_write(7, Rc::clone(&handler) as _, "bad"));
        assert!(!poller.is_registered(7, false));

        // Success path.
        controls.register_result.set(true);
        assert!(poller.register_for_read(7, Rc::clone(&handler) as _, "reader"));
        assert!(poller.is_registered(7, true));
        assert!(!poller.is_registered(7, false));
        assert!(poller.register_for_write(8, Rc::clone(&handler) as _, "writer"));
        assert!(poller.is_registered(8, false));

        assert!(poller.deregister_for_read(7));
        assert!(!poller.is_registered(7, true));
        assert!(poller.deregister_for_write(8));
        assert!(!poller.is_registered(8, false));

        // Two failed and two successful register hooks ran.
        assert_eq!(controls.register_calls.get(), 4);
    }

    /// `maxFD()` spans both handler maps and returns -1 when both are
    /// empty.
    #[test]
    fn max_fd_spans_both_maps() {
        let (backend, _) = StubBackend::pair();
        let mut poller = EventPoller::new(Box::new(backend));
        let handler = FakeHandler::new();

        assert_eq!(poller.max_fd(), -1);

        assert!(poller.register_for_read(5, Rc::clone(&handler) as _, "r"));
        assert_eq!(poller.max_fd(), 5);

        assert!(poller.register_for_write(9, Rc::clone(&handler) as _, "w"));
        assert_eq!(poller.max_fd(), 9);

        assert!(poller.deregister_for_write(9));
        assert_eq!(poller.max_fd(), 5);

        assert!(poller.deregister_for_read(5));
        assert_eq!(poller.max_fd(), -1);
    }

    /// `triggerRead`/`triggerWrite` dispatch only to registered fds,
    /// forwarding to the recorded handler.
    #[test]
    fn triggers_dispatch_to_registered_handlers() {
        let (backend, _) = StubBackend::pair();
        let mut poller = EventPoller::new(Box::new(backend));
        let handler = FakeHandler::new();

        // Nothing registered: no dispatch.
        assert!(!poller.trigger_read(3));
        assert!(!poller.trigger_write(3));

        assert!(poller.register_for_read(3, Rc::clone(&handler) as _, "r"));
        assert!(poller.trigger_read(3));
        assert!(!poller.trigger_write(3));
        assert_eq!(handler.input_calls.get(), 1);

        assert!(poller.register_for_write(4, Rc::clone(&handler) as _, "w"));
        assert!(poller.trigger_write(4));
        assert_eq!(handler.input_calls.get(), 2);

        // Deregistering stops the dispatch.
        assert!(poller.deregister_for_read(3));
        assert!(!poller.trigger_read(3));
        assert_eq!(handler.input_calls.get(), 2);
    }

    /// `triggerError` prefers the read handler, consults
    /// `handleErrorNotification` and falls back to input notification
    /// when the error is unclaimed; a write handler claims
    /// responsibility when no read handler exists; unknown fds report
    /// false.
    #[test]
    fn trigger_error_arms() {
        let (backend, _) = StubBackend::pair();
        let mut poller = EventPoller::new(Box::new(backend));
        let handler = FakeHandler::new();

        // No handler at all.
        assert!(!poller.trigger_error(3));

        // Read handler that claims the error: no input fallback.
        assert!(poller.register_for_read(3, Rc::clone(&handler) as _, "r"));
        handler.error_result.set(true);
        assert!(poller.trigger_error(3));
        assert_eq!(handler.error_calls.get(), 1);
        assert_eq!(handler.input_calls.get(), 0);

        // Unclaimed error falls back to input notification.
        handler.error_result.set(false);
        assert!(poller.trigger_error(3));
        assert_eq!(handler.error_calls.get(), 2);
        assert_eq!(handler.input_calls.get(), 1);

        // Write-handler-only fd: errors route there when no read
        // handler exists.
        assert!(poller.register_for_write(4, Rc::clone(&handler) as _, "w"));
        handler.error_result.set(true);
        assert!(poller.trigger_error(4));
        assert_eq!(handler.error_calls.get(), 3);
    }

    /// `isReadyForShutdown` is the conjunction over every registered
    /// handler's readiness, and an empty poller is always ready.
    #[test]
    fn shutdown_readiness_is_a_conjunction() {
        let (backend, _) = StubBackend::pair();
        let mut poller = EventPoller::new(Box::new(backend));
        let reader = FakeHandler::new();
        let writer = FakeHandler::new();

        assert!(poller.is_ready_for_shutdown());

        assert!(poller.register_for_read(3, Rc::clone(&reader) as _, "r"));
        assert!(poller.is_ready_for_shutdown());

        assert!(poller.register_for_write(4, Rc::clone(&writer) as _, "w"));
        writer.ready.set(false);
        assert!(!poller.is_ready_for_shutdown());

        reader.ready.set(false);
        assert!(!poller.is_ready_for_shutdown());

        assert!(poller.deregister_for_write(4));
        // Still blocked by the read handler.
        assert!(!poller.is_ready_for_shutdown());
        reader.ready.set(true);
        assert!(poller.is_ready_for_shutdown());
    }

    /// An `EventPoller` is itself an `InputNotificationHandler`: its
    /// `handleInputNotification` just runs a zero-timeout poll pass, and
    /// a backend without a poll descriptor reports -1.
    #[test]
    fn the_poller_is_its_own_handler() {
        let (backend, controls) = StubBackend::pair();
        let poller = EventPoller::new(Box::new(backend));

        let handler = &poller as &dyn InputNotificationHandler;
        assert_eq!(handler.handle_input_notification(99), 0);
        assert_eq!(controls.wait_calls.get(), 1);
        assert_eq!(poller.file_descriptor(), -1);
        // Nothing registered, so the poller reports itself as ready.
        assert!(handler.is_ready_for_shutdown());

        // Spare-time bookkeeping round-trips.
        poller.clear_spare_time();
        assert_eq!(poller.spare_time(), 0);
    }

    /// The `INFORM_COUNT` reporting arm fires every 100000 triggers, for
    /// both input and error counting.
    #[test]
    fn entries_count_their_triggers() {
        let handler = FakeHandler::new();
        let entry = InputHandlerEntry::new(Rc::clone(&handler) as _, "counted");

        for _ in 0..InputHandlerEntry::INFORM_COUNT {
            entry.handle_input_notification(1);
        }

        assert_eq!(handler.input_calls.get(), InputHandlerEntry::INFORM_COUNT);
        assert_eq!(entry.num_triggers(), InputHandlerEntry::INFORM_COUNT);
        assert_eq!(entry.name(), "counted");

        // Error counting is symmetric (and crosses the boundary).
        for _ in 0..InputHandlerEntry::INFORM_COUNT {
            entry.handle_error_notification(1);
        }

        assert_eq!(handler.error_calls.get(), InputHandlerEntry::INFORM_COUNT);
        assert_eq!(entry.num_errors(), InputHandlerEntry::INFORM_COUNT);
        assert_eq!(entry.num_triggers(), InputHandlerEntry::INFORM_COUNT);
    }

    /// `create()` hands back a live poller whose epoll descriptor is
    /// valid.
    #[test]
    fn create_returns_a_usable_poller() {
        let poller = EventPoller::create();
        assert!(poller.file_descriptor() >= 0, "the epoll set is live");
    }

    /// A registered read fd wakes the poll: the handler sees the event
    /// and the next pass is idle.
    #[test]
    fn polls_dispatch_read_events() {
        let pipes = PipePair::new();
        let mut poller = EventPoller::create();
        let handler = FakeHandler::new();

        assert!(poller.register_for_read(pipes.read_end(), Rc::clone(&handler) as _, "pipe-r"));

        assert_eq!(platform::socket::write(pipes.write_end(), b"x"), 1);

        assert_eq!(poller.process_pending_events(0.0), 1);
        assert_eq!(handler.input_calls.get(), 1);

        // Drain the byte: level-triggered epoll would report it again.
        let mut drain = [0u8; 1];
        assert_eq!(platform::socket::read(pipes.read_end(), &mut drain), 1);
        assert_eq!(drain[0], b'x');

        // Drained: nothing pending any more.
        assert_eq!(poller.process_pending_events(0.0), 0);
        assert_eq!(handler.input_calls.get(), 1);
    }

    /// A registered write fd is reported immediately (the pipe is
    /// writable).
    #[test]
    fn polls_dispatch_write_events() {
        let pipes = PipePair::new();
        let mut poller = EventPoller::create();
        let handler = FakeHandler::new();

        assert!(poller.register_for_write(pipes.write_end(), Rc::clone(&handler) as _, "pipe-w"));

        assert_eq!(poller.process_pending_events(0.0), 1);
        assert_eq!(handler.input_calls.get(), 1);
    }

    /// Registering both directions on one fd keeps both dispatching, and
    /// deregistering one direction leaves the other live (the
    /// `EPOLL_CTL_MOD` path).
    #[test]
    fn both_directions_and_selective_removal() {
        let pipes = PipePair::new();
        let mut poller = EventPoller::create();
        let read_handler = FakeHandler::new();
        let write_handler = FakeHandler::new();

        assert!(poller.register_for_read(pipes.read_end(), Rc::clone(&read_handler) as _, "r"));
        assert!(poller.register_for_write(pipes.read_end(), Rc::clone(&write_handler) as _, "w"));

        // The read end of a pipe is never writable; only the read event
        // lands.
        assert_eq!(platform::socket::write(pipes.write_end(), b"y"), 1);
        assert_eq!(poller.process_pending_events(0.0), 1);
        assert_eq!(read_handler.input_calls.get(), 1);
        assert_eq!(write_handler.input_calls.get(), 0);

        // Remove the read interest: further data raises nothing.
        assert!(poller.deregister_for_read(pipes.read_end()));
        let mut drain = [0u8; 1];
        // Exactly one byte was written above; a single read drains it
        // (a drain-to-empty loop would block forever: pipe fds are
        // blocking and the write end stays open).
        assert_eq!(platform::socket::read(pipes.read_end(), &mut drain), 1);
        assert_eq!(drain[0], b'y');
        assert_eq!(platform::socket::write(pipes.write_end(), b"y"), 1);
        assert_eq!(poller.process_pending_events(0.0), 0);
        assert_eq!(read_handler.input_calls.get(), 1);

        // The mirror image: bring the read interest back (the write
        // interest is still live, so this is a `MOD` to both), then take
        // the write interest away instead -- the `MOD` arm that keeps only
        // `EPOLLIN`. The retained read interest still delivers data.
        assert!(poller.register_for_read(pipes.read_end(), Rc::clone(&read_handler) as _, "r"));
        assert!(poller.deregister_for_write(pipes.read_end()));

        assert_eq!(platform::socket::write(pipes.write_end(), b"z"), 1);
        assert_eq!(poller.process_pending_events(0.0), 1);
        assert_eq!(read_handler.input_calls.get(), 2);
        assert_eq!(write_handler.input_calls.get(), 0);

        // Two bytes are queued: the one written above, which the removal
        // hid from the poller, then this one.
        let mut drained = [0u8; 2];
        assert_eq!(platform::socket::read(pipes.read_end(), &mut drained), 2);
        assert_eq!(drained, [b'y', b'z']);
    }

    /// Deregistering an fd the poller never saw fails (WARNING + false)
    /// on both directions, and registering an invalid fd fails (ERROR +
    /// false), leaving nothing registered.
    #[test]
    fn unknown_and_invalid_descriptors_fail() {
        let mut poller = EventPoller::create();
        let handler = FakeHandler::new();
        let dead = dead_fd();

        assert!(!poller.deregister_for_read(dead));
        assert!(!poller.deregister_for_write(dead));
        assert!(!poller.is_registered(dead, true));
        assert!(!poller.is_registered(dead, false));

        assert!(!poller.register_for_read(-1, Rc::clone(&handler) as _, "bad"));
        assert!(!poller.register_for_write(-1, Rc::clone(&handler) as _, "bad"));
        assert!(!poller.is_registered(-1, true));
        assert!(!poller.is_registered(-1, false));

        // `dead_fd` is a number the kernel refuses, so `epoll_ctl` fails
        // with EBADF: the ERROR arm. The WARNING arm is `ENOENT`, which
        // needs a *live* descriptor that simply is not in the poll set.
        let pipes = PipePair::new();
        assert!(!poller.deregister_for_read(pipes.read_end()));
        assert!(!poller.deregister_for_write(pipes.write_end()));
    }

    /// A hangup surfaces through `triggerError`: an unclaimed error
    /// falls back to input notification (the handler sees the EOF
    /// itself).
    #[test]
    fn hangups_route_to_the_error_handler() {
        let mut pipes = PipePair::new();
        let mut poller = EventPoller::create();
        let handler = FakeHandler::new();

        assert!(poller.register_for_read(pipes.read_end(), Rc::clone(&handler) as _, "r"));

        // Closing the write end hangs up the read end.
        pipes.close_write_end();

        assert_eq!(poller.process_pending_events(0.0), 1);
        // The EPOLLERR|EPOLLHUP arm routes to the error handler first.
        assert_eq!(handler.error_calls.get(), 1);
        assert_eq!(handler.input_calls.get(), 1);
    }

    /// An idle poll stops: a zero-timeout pass returns at once, and a
    /// timed pass waits out its timeout instead of blocking forever.
    #[test]
    fn an_idle_poll_stops() {
        let poller = EventPoller::create();

        // No descriptors, no timeout: report nothing and return.
        assert_eq!(poller.process_pending_events(0.0), 0);

        // The timed pass waits the timeout out and reports spare time,
        // which is the bookkeeping the dispatcher reads for idling.
        poller.clear_spare_time();
        let start = platform::clock::monotonic_now_micros();
        assert_eq!(poller.process_pending_events(0.05), 0, "nothing is pending");
        let waited = platform::clock::monotonic_now_micros()
            .saturating_duration_since(start)
            .micros();
        assert!(
            waited >= 40_000,
            "the timeout elapsed ({waited}us), not a busy loop"
        );
        assert!(poller.spare_time() >= 40_000, "the wait is spare time");
    }

    /// A handler that implements only `handleInputNotification` gets the
    /// reference's defaults from `interfaces.hpp`: it does not claim
    /// errors, so they fall back to the input notification, and it is
    /// always ready for shutdown.
    struct BareHandler {
        input_calls: Cell<u32>,
    }

    impl InputNotificationHandler for BareHandler {
        fn handle_input_notification(&self, _fd: RawFd) -> i32 {
            self.input_calls.set(self.input_calls.get() + 1);
            0
        }
    }

    #[test]
    fn the_handler_defaults_are_the_reference_defaults() {
        let (backend, _) = StubBackend::pair();
        let mut poller = EventPoller::new(Box::new(backend));
        let handler = Rc::new(BareHandler {
            input_calls: Cell::new(0),
        });

        assert!(poller.register_for_read(3, handler.clone() as _, "bare"));
        assert!(poller.is_ready_for_shutdown(), "the default reports ready");

        // The default error handler declines to claim the error, so the
        // input notification runs in its place.
        assert!(poller.trigger_error(3));
        assert_eq!(handler.input_calls.get(), 1);

        assert!(poller.trigger_read(3));
        assert_eq!(handler.input_calls.get(), 2);
    }

    /// A failed `epoll_create` is reported with `CRITICAL_MSG`, which
    /// aborts: the port panics with the same message, as every other
    /// CRITICAL site is ported.
    #[test]
    #[should_panic(expected = "EPoller::EPoller: epoll_create failed")]
    fn a_failed_epoll_create_is_fatal() {
        let _ = EpollBackend::from_fd(-1);
    }
}
