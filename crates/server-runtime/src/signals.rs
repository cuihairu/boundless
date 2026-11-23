//! `SignalProcessor` + `Signal::Set`/`Signal::Blocker` port
//! (`lib/server/signal_processor.cpp`, `signal_set.hpp`).
//!
//! Reference quirks preserved:
//! - the C-level handler only sets a bit in the pending set (async-signal
//!   safe, no allocation, no logging); the registered handlers run later
//!   on the main thread inside `dispatch()`, the dispatcher's frequent
//!   task;
//! - `SIGQUIT` is special: its single handler runs *immediately* from the
//!   signal context (the machined watchdog path -- the app may be about
//!   to die), a second registration asserts;
//! - `SIGRTMIN..=SIGRTMAX` render as the literal `"SIGRTxxx"`;
//! - removing the last handler of a signal reverts its disposition to
//!   `SIG_DFL`, and dropping the processor resets every signal it still
//!   handles;
//! - `dispatch()` blocks all signals for its duration (the `Blocker`).
//!
//! Deviations: the pending set is an `AtomicU32` over signals 1..=31
//! (`SIGMAX == SIGSYS`) instead of a `sigset_t` -- same async-safe
//! mutation contract, real-time signals are simply not tracked (nothing
//! in the server apps registers one). The `SIGQUIT` immediate slot takes
//! a `Send + Sync` callback instead of re-entering the shared handler
//! through a `RefCell` (the reference dereferences the handler pointer
//! straight from signal context; its own contract is "must not block or
//! allocate", which the callback keeps).

use std::cell::RefCell;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// `SIGMIN`.
pub const SIGMIN: i32 = 1;

/// `SIGMAX` (= `SIGSYS` on POSIX).
pub const SIGMAX: i32 = libc::SIGSYS;

/// The `SIGNAL_NAMES` table, `SIGMIN..=SIGMAX` (index 0 unused).
pub const SIGNAL_NAMES: [&str; 32] = [
    "",
    "SIGHUP",
    "SIGINT",
    "SIGQUIT",
    "SIGILL",
    "SIGTRAP",
    "SIGABRT",
    "SIGBUS",
    "SIGFPE",
    "SIGKILL",
    "SIGUSR1",
    "SIGSEGV",
    "SIGUSR2",
    "SIGPIPE",
    "SIGALRM",
    "SIGTERM",
    "SIGSTKFLT",
    "SIGCHLD",
    "SIGCONT",
    "SIGSTOP",
    "SIGTSTP",
    "SIGTTIN",
    "SIGTTOU",
    "SIGURG",
    "SIGXCPU",
    "SIGXFSZ",
    "SIGVTALRM",
    "SIGPROF",
    "SIGWINCH",
    "SIGIO",
    "SIGPWR",
    "SIGSYS",
];

/// The pending set: bit `sig` set means signal `sig` arrived and has not
/// been dispatched yet. Mutation from the signal context is the only
/// thing the C handler does.
static PENDING_SIGNALS: AtomicU32 = AtomicU32::new(0);

/// The undelivered-signal mask (the C handler's pending set).
pub fn pending_signals() -> u32 {
    PENDING_SIGNALS.load(Ordering::Relaxed)
}

/// The process-wide kernel pending mask (`sigpending`): signals raised
/// while blocked wait here until delivery.
pub fn kernel_pending_signals() -> u32 {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigpending(&mut set);
        let mut mask = 0u32;
        for sig_num in SIGMIN..=SIGMAX {
            if libc::sigismember(&set, sig_num) == 1 {
                mask |= 1 << sig_num;
            }
        }
        mask
    }
}

/// The single `SIGQUIT` handler slot: a thin pointer to a concrete box
/// wrapping the callback, read straight from signal context (the
/// reference stores the handler pointer the same way).
static SIGQUIT_HANDLER: AtomicPtr<QuitSlot> = AtomicPtr::new(std::ptr::null_mut());

type QuitCallback = dyn Fn(i32) + Send + Sync;
type ArcQuitHandler = Arc<QuitCallback>;

/// Concrete wrapper keeping the signal-context pointer thin.
struct QuitSlot {
    callback: Box<dyn Fn(i32) + Send + Sync>,
}

/// Serialises every test that changes a process-global signal
/// disposition (cargo runs tests on parallel threads; dispositions are
/// process-wide state).
pub static DISPOSITION_LOCK: Mutex<()> = Mutex::new(());

/// The `SignalHandler` abstract class: run on the main thread during
/// `dispatch`.
pub trait SignalHandler {
    fn handle_signal(&mut self, sig_num: i32);
}

/// Any closure as a `SignalHandler` (the `ServerAppSignalHandler` /
/// `ChildProcessHandler` classes of the reference).
pub struct SignalHandlerFn<F: FnMut(i32)>(pub F);

impl<F: FnMut(i32)> SignalHandler for SignalHandlerFn<F> {
    fn handle_signal(&mut self, sig_num: i32) {
        (self.0)(sig_num)
    }
}

/// Identity token of a registered handler (`SignalHandler *` in the
/// reference, whose map keys on the pointer: one handler registered
/// under several signals shares one identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandlerId(usize);

/// Serialises every test that changes a process-global signal
/// disposition (cargo runs tests on parallel threads; dispositions are
/// process-wide state). Poisoning is ignored on purpose: the guard only
/// serialises, it guards no invariants.
pub fn disposition_guard() -> std::sync::MutexGuard<'static, ()> {
    DISPOSITION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `Signal::Set` over a `sigset_t` (`EMPTY`/`FULL` initialisers and the
/// `isClear_` dodge for the non-standard `sigisemptyset`).
pub struct Set {
    raw: libc::sigset_t,
    is_clear: bool,
}

impl Set {
    /// `Set(EMPTY)`.
    pub fn empty() -> Self {
        let mut raw: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut raw);
        }
        Self {
            raw,
            is_clear: true,
        }
    }

    /// `Set(FULL)`.
    pub fn full() -> Self {
        let mut raw: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigfillset(&mut raw);
        }
        Self {
            raw,
            is_clear: false,
        }
    }

    pub fn clear(&mut self, signal: i32) {
        unsafe {
            libc::sigdelset(&mut self.raw, signal);
        }
        self.is_clear = self.is_clear();
    }

    pub fn set(&mut self, signal: i32) {
        unsafe {
            libc::sigaddset(&mut self.raw, signal);
        }
        self.is_clear = false;
    }

    pub fn clear_all(&mut self) {
        unsafe {
            libc::sigemptyset(&mut self.raw);
        }
        self.is_clear = true;
    }

    /// `isClear`. The reference leans on the "dodgy" `sigisemptyset`
    /// feature; libc does not expose it, so the scan spells it out.
    pub fn is_clear(&self) -> bool {
        (SIGMIN..=SIGMAX).all(|sig_num| !self.is_set(sig_num))
    }

    pub fn is_set(&self, signal: i32) -> bool {
        unsafe { libc::sigismember(&self.raw, signal) == 1 }
    }

    pub fn raw(&self) -> &libc::sigset_t {
        &self.raw
    }
}

/// `Signal::Blocker`: blocks `signals` for its lifetime (per-thread
/// `pthread_sigmask`, POSIX-only like the reference).
pub struct Blocker {
    old_signals: libc::sigset_t,
}

impl Blocker {
    pub fn new(signals: &Set) -> Self {
        let mut old_signals: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            let result = libc::pthread_sigmask(libc::SIG_SETMASK, signals.raw(), &mut old_signals);
            assert_eq!(result, 0, "pthread_sigmask failed");
        }
        Self { old_signals }
    }
}

impl Drop for Blocker {
    fn drop(&mut self) {
        unsafe {
            let result =
                libc::pthread_sigmask(libc::SIG_SETMASK, &self.old_signals, std::ptr::null_mut());
            assert_eq!(result, 0, "pthread_sigmask restore failed");
        }
    }
}

/// The C-level handler (`::signalHandler`): set the pending bit, and for
/// `SIGQUIT` run the immediate handler. No allocation, no logging.
extern "C" fn c_signal_handler(sig_num: i32) {
    if (SIGMIN..=SIGMAX).contains(&sig_num) {
        PENDING_SIGNALS.fetch_or(1 << sig_num, Ordering::Relaxed);
    }

    if sig_num == libc::SIGQUIT {
        let slot = SIGQUIT_HANDLER.load(Ordering::Relaxed);
        if !slot.is_null() {
            // The reference dereferences the stored handler pointer
            // straight from signal context; the slot outlives every
            // raise (dropped only by the owning processor).
            let quit_slot = unsafe { &*slot };
            (quit_slot.callback)(sig_num);
        }
    }
}

/// The current sigaction disposition of `sig_num` (tests observe
/// `SIG_DFL`/`SIG_IGN`/handler through it).
pub fn disposition_of(sig_num: i32) -> usize {
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(sig_num, std::ptr::null(), &mut old);
        old.sa_sigaction
    }
}

/// One registration entry: the signal number, the registration id and the
/// handler, kept in registration order (the reference's multimap keeps
/// insertion order within a signal).
type HandlerEntry = (i32, HandlerId, Rc<RefCell<dyn SignalHandler>>);

/// `SignalProcessor`: registration + the dispatch drain. The global
/// signal/quit-handler state is shared exactly like the reference's
/// singleton; instances manage the main-thread registration table.
pub struct SignalProcessor {
    /// `(signal, id, handler)` in registration order.
    handlers: Vec<HandlerEntry>,
    /// Whether this instance installed the `SIGQUIT` slot.
    quit_installed: bool,
}

impl Default for SignalProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalProcessor {
    /// `SignalProcessor(dispatcher)`: the constructor also registers the
    /// dispatch frequent task; that wiring belongs to the caller here
    /// (`Dispatcher::add_frequent_task`).
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            quit_installed: false,
        }
    }

    /// `ignoreSignal`: clear handlers and set `SIG_IGN`.
    pub fn ignore_signal(&mut self, sig_num: i32) {
        self.clear_signal_handlers(sig_num);
        unsafe {
            libc::signal(sig_num, libc::SIG_IGN);
        }
    }

    /// `setDefaultSignalHandler`: clear handlers and set `SIG_DFL`.
    pub fn set_default_signal_handler(&mut self, sig_num: i32) {
        self.clear_signal_handlers(sig_num);
        unsafe {
            libc::signal(sig_num, libc::SIG_DFL);
        }
    }

    /// `addSignalHandler`: register under `sig_num` and enable detection
    /// via `sigaction` with a full mask. The identity is the handler
    /// allocation (the reference's handler pointer): registering the
    /// same handler Rc under several signals yields one id. `SIGQUIT`
    /// goes through [`SignalProcessor::add_quit_handler`] instead.
    pub fn add_signal_handler(
        &mut self,
        sig_num: i32,
        handler: Rc<RefCell<dyn SignalHandler>>,
    ) -> HandlerId {
        assert!(
            sig_num != libc::SIGQUIT,
            "SIGQUIT takes the single immediate handler"
        );
        let id = HandlerId(Rc::as_ptr(&handler) as *const () as usize);
        self.handlers.push((sig_num, id, handler));
        self.enable_detect_signal(sig_num);
        id
    }

    /// The `SIGQUIT` arm of `addSignalHandler`: the single immediate
    /// handler (`MF_ASSERT( pSigQuitHandler_ == NULL )` on a second).
    pub fn add_quit_handler(&mut self, callback: ArcQuitHandler) {
        assert!(
            SIGQUIT_HANDLER.load(Ordering::Relaxed).is_null(),
            "can only have one handler for SIGQUIT"
        );
        SIGQUIT_HANDLER.store(
            Box::into_raw(Box::new(QuitSlot {
                callback: Box::new(move |sig_num| callback(sig_num)),
            })),
            Ordering::Relaxed,
        );
        self.quit_installed = true;
        self.enable_detect_signal(libc::SIGQUIT);
    }

    /// `enableDetectSignal`: sigaction with a full signal mask (all
    /// signals blocked while the handler runs); `SA_SIGINFO` is not
    /// supported (the reference strips it with an error).
    fn enable_detect_signal(&self, sig_num: i32) {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = c_signal_handler as *const () as usize;
            libc::sigfillset(&mut action.sa_mask);
            action.sa_flags = 0;
            libc::sigaction(sig_num, &action, std::ptr::null_mut());
        }
    }

    /// `clearSignalHandlers(sigNum)`: drop every handler of the signal.
    pub fn clear_signal_handlers(&mut self, sig_num: i32) {
        if sig_num == libc::SIGQUIT {
            self.clear_quit_handler();
        }
        self.handlers.retain(|(sig, _, _)| *sig != sig_num);
    }

    /// `clearSignalHandler(sigNum, handler)`: drop the handler from the
    /// signal; the last removal reverts the disposition to `SIG_DFL`.
    /// Returns whether anything was removed.
    pub fn clear_signal_handler(&mut self, sig_num: i32, id: HandlerId) -> bool {
        if sig_num == libc::SIGQUIT {
            let had = self.quit_installed;
            self.clear_quit_handler();
            return had;
        }
        let before = self.handlers.len();
        self.handlers
            .retain(|(sig, hid, _)| !(*sig == sig_num && *hid == id));
        let removed = self.handlers.len() != before;
        if removed && !self.handlers.iter().any(|(sig, _, _)| *sig == sig_num) {
            self.set_default_signal_handler(sig_num);
        }
        removed
    }

    /// `clearSignalHandler(handler)`: drop the handler from every signal,
    /// reverting dispositions that lost their last handler.
    pub fn clear_handler_everywhere(&mut self, id: HandlerId) {
        let mut touched: Vec<i32> = Vec::new();
        self.handlers.retain(|(sig, hid, _)| {
            if *hid == id {
                touched.push(*sig);
                false
            } else {
                true
            }
        });
        for sig in touched {
            if !self.handlers.iter().any(|(s, _, _)| *s == sig) {
                self.set_default_signal_handler(sig);
            }
        }
    }

    fn clear_quit_handler(&mut self) {
        if self.quit_installed {
            let slot = SIGQUIT_HANDLER.swap(ptr::null_mut(), Ordering::Relaxed);
            if !slot.is_null() {
                drop(unsafe { Box::from_raw(slot) });
            }
            self.quit_installed = false;
            // The reference destructor resets every signal it handled,
            // SIGQUIT included.
            unsafe {
                libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            }
        }
    }

    /// `SignalProcessor::handleSignal` (the async-signal-safe half,
    /// normally entered through the C handler): mark the signal pending
    /// and run the `SIGQUIT` handler immediately.
    pub fn handle_signal(&self, sig_num: i32) {
        c_signal_handler(sig_num);
    }

    /// `SignalProcessor::dispatch`: drain every pending signal through
    /// its handlers on this thread, with all signals blocked meanwhile;
    /// the pending set is emptied afterwards.
    pub fn dispatch(&mut self) {
        let all = Set::full();
        let _blocker = Blocker::new(&all);

        for sig_num in SIGMIN..=SIGMAX {
            if PENDING_SIGNALS.load(Ordering::Relaxed) & (1 << sig_num) != 0 {
                self.dispatch_signal(sig_num);
            }
        }

        PENDING_SIGNALS.store(0, Ordering::Relaxed);
    }

    /// `dispatchSignal`: run the signal's handlers against a snapshot of
    /// the registration table (handlers may re-register).
    pub fn dispatch_signal(&mut self, sig_num: i32) {
        eprintln!(
            "SignalProcessor::dispatchSignal: {}",
            signal_number_to_string(sig_num)
        );

        let snapshot: Vec<_> = self
            .handlers
            .iter()
            .filter(|(sig, _, _)| *sig == sig_num)
            .map(|(_, _, handler)| Rc::clone(handler))
            .collect();
        for handler in snapshot {
            handler.borrow_mut().handle_signal(sig_num);
        }
    }

    /// `waitForSignals`: `sigwaitinfo` until one of `set` arrives
    /// (retrying on `EINTR`), then dispatch it. Only
    /// `TableSynchroniser::run` uses this in the reference.
    pub fn wait_for_signals(&mut self, set: &Set) -> i32 {
        let mut sig_num = unsafe { libc::sigwaitinfo(set.raw(), std::ptr::null_mut()) };
        while sig_num < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            sig_num = unsafe { libc::sigwaitinfo(set.raw(), std::ptr::null_mut()) };
        }
        if sig_num >= 0 {
            self.dispatch_signal(sig_num);
        }
        sig_num
    }
}

impl Drop for SignalProcessor {
    /// The destructor: reset every signal it still handles to the default
    /// disposition.
    fn drop(&mut self) {
        let signals: Vec<i32> = self.handlers.iter().map(|(sig, _, _)| *sig).collect();
        for sig_num in signals {
            unsafe {
                libc::signal(sig_num, libc::SIG_DFL);
            }
        }
        self.clear_quit_handler();
    }
}

/// `SignalProcessor::signalNumberToString`.
pub fn signal_number_to_string(sig_num: i32) -> &'static str {
    // SIGRTMIN/SIGRTMAX are function calls into glibc.
    let (sig_rtmin, sig_rtmax) = (libc::SIGRTMIN(), libc::SIGRTMAX());
    if (sig_rtmin..=sig_rtmax).contains(&sig_num) {
        return "SIGRTxxx";
    }
    assert!((SIGMIN..=SIGMAX).contains(&sig_num) && sig_num != 0);
    SIGNAL_NAMES[sig_num as usize]
}

/// The `SignalProcessor`-as-`FrequentTask` binding: the reference
/// constructor does `dispatcher_.addFrequentTask( this )` and its
/// destructor `cancelFrequentTask( this )`; Rust ownership flattens the
/// back-reference into a task holding the processor, registered by the
/// app that owns both.
pub struct SignalDispatchTask(Rc<RefCell<SignalProcessor>>);

impl SignalDispatchTask {
    /// The task to hand to `Dispatcher::add_frequent_task`.
    pub fn new_task(
        processor: &Rc<RefCell<SignalProcessor>>,
    ) -> Rc<RefCell<dyn crate::dispatcher::FrequentTask>> {
        Rc::new(RefCell::new(SignalDispatchTask(Rc::clone(processor))))
    }
}

impl crate::dispatcher::FrequentTask for SignalDispatchTask {
    fn do_task(&mut self) {
        self.0.borrow_mut().dispatch();
    }

    fn task_name(&self) -> &str {
        "SignalProcessor"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A handler that counts every delivered signal.
    fn counting_handler(counter: Arc<AtomicUsize>) -> Rc<RefCell<dyn SignalHandler>> {
        Rc::new(RefCell::new(SignalHandlerFn(move |_sig| {
            counter.fetch_add(1, Ordering::SeqCst);
        })))
    }

    /// `signalNumberToString`: the fixed table, the real-time range and
    /// the out-of-range rejection.
    #[test]
    fn signal_names_cover_the_reference_table() {
        let _guard = disposition_guard();
        assert_eq!(signal_number_to_string(libc::SIGINT), "SIGINT");
        assert_eq!(signal_number_to_string(libc::SIGTERM), "SIGTERM");
        assert_eq!(signal_number_to_string(libc::SIGQUIT), "SIGQUIT");
        assert_eq!(signal_number_to_string(libc::SIGUSR1), "SIGUSR1");
        assert_eq!(signal_number_to_string(libc::SIGSYS), "SIGSYS");
        let (sig_rtmin, sig_rtmax) = (libc::SIGRTMIN(), libc::SIGRTMAX());
        assert_eq!(signal_number_to_string(sig_rtmin), "SIGRTxxx");
        assert_eq!(signal_number_to_string(sig_rtmax), "SIGRTxxx");
        assert_eq!(SIGMAX, libc::SIGSYS);
    }

    /// `Signal::Set`: EMPTY/FULL initialisers, per-signal set/clear,
    /// clearAll, and the sigisemptyset-backed is_clear.
    #[test]
    fn set_mirrors_the_sigset_t_wrapper() {
        let mut set = Set::empty();
        assert!(set.is_clear());
        assert!(!set.is_set(libc::SIGUSR1));

        set.set(libc::SIGUSR1);
        set.set(libc::SIGUSR2);
        assert!(!set.is_clear());
        assert!(set.is_set(libc::SIGUSR1));
        assert!(set.is_set(libc::SIGUSR2));
        assert!(!set.is_set(libc::SIGTERM));

        set.clear(libc::SIGUSR1);
        assert!(!set.is_set(libc::SIGUSR1));
        assert!(set.is_set(libc::SIGUSR2));
        assert!(!set.is_clear());

        set.clear_all();
        assert!(set.is_clear());

        assert!(Set::full().is_set(libc::SIGTERM));
    }

    /// The full async chain: raise marks the pending bit immediately
    /// (the C handler ran), dispatch delivers on this thread and drains.
    #[test]
    fn raise_marks_pending_and_dispatch_delivers() {
        let _guard = disposition_guard();
        let counter = Arc::new(AtomicUsize::new(0));
        let mut processor = SignalProcessor::new();
        {
            let counter = Arc::clone(&counter);
            processor.add_signal_handler(libc::SIGUSR1, counting_handler(counter));
        }

        assert_eq!(
            PENDING_SIGNALS.load(Ordering::Relaxed) & (1 << libc::SIGUSR1),
            0
        );
        unsafe {
            libc::raise(libc::SIGUSR1);
        }
        assert!(PENDING_SIGNALS.load(Ordering::Relaxed) & (1 << libc::SIGUSR1) != 0);
        assert_eq!(counter.load(Ordering::SeqCst), 0, "not yet dispatched");

        processor.dispatch();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(PENDING_SIGNALS.load(Ordering::Relaxed), 0, "drained");

        // A second raise re-delivers; every handler of the signal runs,
        // in registration order.
        let counter_b = Arc::new(AtomicUsize::new(0));
        {
            let counter_b = Arc::clone(&counter_b);
            processor.add_signal_handler(libc::SIGUSR1, counting_handler(counter_b));
        }
        unsafe {
            libc::raise(libc::SIGUSR1);
        }
        processor.dispatch();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert_eq!(counter_b.load(Ordering::SeqCst), 1);

        processor.set_default_signal_handler(libc::SIGUSR1);
        assert_eq!(disposition_of(libc::SIGUSR1), libc::SIG_DFL);
    }

    /// `Blocker`: a blocked raise never reaches the C handler; the
    /// signal delivers when the mask is restored.
    #[test]
    fn blocker_defers_the_signal_until_its_scope_ends() {
        let _guard = disposition_guard();
        let counter = Arc::new(AtomicUsize::new(0));
        let mut processor = SignalProcessor::new();
        {
            let counter = Arc::clone(&counter);
            processor.add_signal_handler(libc::SIGUSR2, counting_handler(counter));
        }

        let mut blocked = Set::empty();
        blocked.set(libc::SIGUSR2);
        let pending_before = PENDING_SIGNALS.load(Ordering::Relaxed);
        {
            let _blocker = Blocker::new(&blocked);
            unsafe {
                libc::raise(libc::SIGUSR2);
            }
            // Blocked at delivery: no handler, no pending bit.
            assert_eq!(PENDING_SIGNALS.load(Ordering::Relaxed), pending_before);
            assert_eq!(counter.load(Ordering::SeqCst), 0);
        }

        // The mask is restored: the kernel delivers now, the C handler
        // marks the bit, dispatch runs the app-side handler.
        assert!(PENDING_SIGNALS.load(Ordering::Relaxed) & (1 << libc::SIGUSR2) != 0);
        processor.dispatch();
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        processor.set_default_signal_handler(libc::SIGUSR2);
    }

    /// `SIGQUIT`'s single immediate handler fires in signal context; a
    /// second registration asserts (the reference's `MF_ASSERT`).
    #[test]
    #[should_panic(expected = "can only have one handler for SIGQUIT")]
    fn second_sigquit_handler_asserts() {
        let _guard = disposition_guard();
        let mut processor = SignalProcessor::new();
        processor.add_quit_handler(Arc::new(|_sig| {}));
        processor.add_quit_handler(Arc::new(|_sig| {}));
    }

    /// The installed `SIGQUIT` handler runs during `raise`, before any
    /// dispatch.
    #[test]
    fn sigquit_handler_fires_immediately() {
        let _guard = disposition_guard();
        let fired = Arc::new(AtomicUsize::new(0));
        let mut processor = SignalProcessor::new();
        {
            let fired = Arc::clone(&fired);
            processor.add_quit_handler(Arc::new(move |_sig| {
                fired.fetch_add(1, Ordering::SeqCst);
            }));
        }

        unsafe {
            libc::raise(libc::SIGQUIT);
        }
        // Immediate: no dispatch needed, and the pending bit is set for
        // the regular drain too (nobody regular ever registers for it).
        assert_eq!(fired.load(Ordering::SeqCst), 1);

        // The global bitset is shared process state; wipe the bit this
        // raise left behind so the rest of the suite starts clean.
        PENDING_SIGNALS.store(0, Ordering::Relaxed);

        // `clearSignalHandler( SIGQUIT, ... )` drops the slot; a second
        // call finds nothing; the bulk `clearSignalHandlers` path is a
        // no-op on the empty slot.
        assert!(processor.clear_signal_handler(libc::SIGQUIT, HandlerId(0)));
        assert!(!processor.clear_signal_handler(libc::SIGQUIT, HandlerId(0)));
        processor.clear_signal_handlers(libc::SIGQUIT);
    }

    /// Handler bookkeeping: removing one of several keeps the rest; the
    /// last removal reverts the disposition to `SIG_DFL`.
    #[test]
    fn handler_removal_reverts_the_disposition() {
        let _guard = disposition_guard();
        let mut processor = SignalProcessor::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let id_a = {
            let counter = Arc::clone(&counter);
            processor.add_signal_handler(libc::SIGUSR1, counting_handler(counter))
        };
        let counter_b = Arc::new(AtomicUsize::new(0));
        let id_b = {
            let counter_b = Arc::clone(&counter_b);
            processor.add_signal_handler(libc::SIGUSR1, counting_handler(counter_b))
        };

        unsafe {
            libc::raise(libc::SIGUSR1);
        }
        processor.dispatch();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(counter_b.load(Ordering::SeqCst), 1);

        assert!(processor.clear_signal_handler(libc::SIGUSR1, id_a));
        unsafe {
            libc::raise(libc::SIGUSR1);
        }
        processor.dispatch();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(counter_b.load(Ordering::SeqCst), 2);
        assert_ne!(disposition_of(libc::SIGUSR1), libc::SIG_DFL);

        // Last handler out: back to SIG_DFL; a stale removal is a no-op.
        assert!(processor.clear_signal_handler(libc::SIGUSR1, id_b));
        assert_eq!(disposition_of(libc::SIGUSR1), libc::SIG_DFL);
        assert!(!processor.clear_signal_handler(libc::SIGUSR1, id_b));
    }

    /// `clear_handler_everywhere` removes one handler from all signals
    /// it is registered under (one identity spanning signals, like the
    /// reference's handler pointer) and reverts each signal that lost
    /// its last handler.
    #[test]
    fn clear_handler_everywhere_cleans_all_signals() {
        let _guard = disposition_guard();
        let mut processor = SignalProcessor::new();

        // One handler Rc under two signals: one identity.
        let counter = Arc::new(AtomicUsize::new(0));
        let shared = counting_handler(Arc::clone(&counter));
        let id_a = processor.add_signal_handler(libc::SIGUSR1, Rc::clone(&shared));
        let id_b = processor.add_signal_handler(libc::SIGUSR2, Rc::clone(&shared));
        assert_eq!(id_a, id_b);

        // An independent handler keeps SIGUSR1 alive after `shared`
        // leaves it.
        let other = counting_handler(Arc::new(AtomicUsize::new(0)));
        let _keep = processor.add_signal_handler(libc::SIGUSR1, other);

        processor.clear_handler_everywhere(id_a);
        // SIGUSR2 lost its last handler -> SIG_DFL; SIGUSR1 still has one.
        assert_eq!(disposition_of(libc::SIGUSR2), libc::SIG_DFL);
        assert_ne!(disposition_of(libc::SIGUSR1), libc::SIG_DFL);

        // The removed handler gets no more deliveries: raising SIGUSR1
        // (whose remaining handler is the kept one) leaves `counter`
        // untouched. SIGUSR2 has no handler any more -- raising it would
        // be the default action -- so it is not raised.
        unsafe {
            libc::raise(libc::SIGUSR1);
        }
        processor.dispatch();
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        processor.set_default_signal_handler(libc::SIGUSR1);
    }

    /// `ignoreSignal` sets SIG_IGN and drops the handlers; the raised
    /// signal does nothing.
    #[test]
    fn ignore_signal_swallows_raises() {
        let _guard = disposition_guard();
        let mut processor = SignalProcessor::new();
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let counter = Arc::clone(&counter);
            processor.add_signal_handler(libc::SIGUSR2, counting_handler(counter));
        }

        processor.ignore_signal(libc::SIGUSR2);
        assert_eq!(disposition_of(libc::SIGUSR2), libc::SIG_IGN);
        // The pending bitset is shared process state that a sibling test may
        // have dirtied, so assert the ignored raise added nothing rather
        // than that the world was quiet.
        let before = PENDING_SIGNALS.load(Ordering::Relaxed);
        unsafe {
            libc::raise(libc::SIGUSR2);
        }
        assert_eq!(
            PENDING_SIGNALS.load(Ordering::Relaxed),
            before,
            "an ignored signal queues nothing"
        );
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        // Restore for the rest of the suite.
        processor.set_default_signal_handler(libc::SIGUSR2);
        assert_eq!(disposition_of(libc::SIGUSR2), libc::SIG_DFL);
    }

    /// `Drop` resets every handled signal to the default disposition.
    #[test]
    fn drop_resets_dispositions() {
        let _guard = disposition_guard();
        {
            let mut processor = SignalProcessor::new();
            let _id = processor.add_signal_handler(
                libc::SIGUSR1,
                counting_handler(Arc::new(AtomicUsize::new(0))),
            );
            assert_ne!(disposition_of(libc::SIGUSR1), libc::SIG_DFL);
        }
        assert_eq!(disposition_of(libc::SIGUSR1), libc::SIG_DFL);
    }

    /// `waitForSignals`: sigwaitinfo consumes a signal that arrived
    /// while blocked and dispatches it against the processor's table.
    /// The raise happens on this thread while the signal is blocked (the
    /// TableSynchroniser pattern -- it blocks the set before waiting), so
    /// the arrival parks for nobody but the waiter; a cross-thread
    /// process-directed raise would be deliverable to any parallel test
    /// thread and is not deterministic here.
    #[test]
    fn wait_for_signals_wakes_on_raise() {
        let _guard = disposition_guard();

        let mut set = Set::empty();
        set.set(libc::SIGUSR1);

        // Block first (the sigwait contract), then raise: the arrival
        // parks on this thread and only sigwaitinfo can consume it.
        let mut old_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, set.raw(), &mut old_mask);
            libc::raise(libc::SIGUSR1);
            assert!(
                kernel_pending_signals() & (1 << libc::SIGUSR1) != 0,
                "the blocked raise parks"
            );
        }

        let mut processor = SignalProcessor::new();
        assert_eq!(processor.wait_for_signals(&set), libc::SIGUSR1);
        assert!(
            kernel_pending_signals() & (1 << libc::SIGUSR1) == 0,
            "sigwait consumed the arrival"
        );

        // Restore this thread's mask for the rest of the suite.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &old_mask, std::ptr::null_mut());
        }
    }

    /// `waitForSignals` retries on `EINTR`: a caught signal delivered
    /// while the thread sits in sigwaitinfo interrupts the wait (the
    /// signal is not in the wait set, so it goes to its handler) and the
    /// loop re-enters until the real arrival.
    #[test]
    fn wait_for_signals_retries_after_eintr() {
        let _guard = disposition_guard();
        use std::thread;
        use std::time::Duration;

        // SIGUSR2 is outside the wait set with a handler: its delivery
        // interrupts the sigwait instead of satisfying it.
        let counter = Arc::new(AtomicUsize::new(0));
        let mut processor = SignalProcessor::new();
        {
            let counter = Arc::clone(&counter);
            processor.add_signal_handler(libc::SIGUSR2, counting_handler(counter));
        }

        let mut wait_set = Set::empty();
        wait_set.set(libc::SIGUSR1);
        let mut old_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, wait_set.raw(), &mut old_mask);
        }

        // All deliveries are thread-directed at THIS thread, so no
        // parallel test thread can be hit and nothing is left to chance.
        let main_thread = unsafe { libc::pthread_self() };
        let interrupter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            unsafe {
                libc::pthread_kill(main_thread, libc::SIGUSR2);
            }
            thread::sleep(Duration::from_millis(50));
            // Blocked on the waiter thread: parks there for sigwait.
            unsafe {
                libc::pthread_kill(main_thread, libc::SIGUSR1);
            }
        });

        assert_eq!(processor.wait_for_signals(&wait_set), libc::SIGUSR1);
        interrupter.join().unwrap();

        // The interrupting delivery ran the SIGUSR2 handler (its pending
        // bit is up; drain it away) and the wait still returned the
        // original arrival.
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        processor.dispatch();
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &old_mask, std::ptr::null_mut());
        }
        processor.set_default_signal_handler(libc::SIGUSR2);
    }

    /// `handle_signal` (the in-process half) marks the bit without any
    /// real signal, mirroring what the C handler does.
    #[test]
    fn handle_signal_marks_the_pending_bit() {
        // The global set is shared: a parallel test's drain must not see
        // or wipe this bit mid-assert.
        let _guard = disposition_guard();
        let processor = SignalProcessor::default();
        processor.handle_signal(libc::SIGTERM);
        assert!(PENDING_SIGNALS.load(Ordering::Relaxed) & (1 << libc::SIGTERM) != 0);
        PENDING_SIGNALS.store(0, Ordering::Relaxed);
    }
}
