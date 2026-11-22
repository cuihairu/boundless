//! Signal OS primitives, the sole landing point for the signal syscalls
//! (`sigwait`/`sigwaitinfo`, `pthread_sigmask`, `raise`, `kill`,
//! `pthread_kill`, `sigaction`, `sigpending`, and the `sigset_t` family)
//! -- BW-16-02-01.
//!
//! `Signal::Set`/`Signal::Blocker`/`SignalProcessor` (the reference's
//! `signal_processor.cpp` / `signal_set.hpp` port) live in server-runtime;
//! this module wraps only the POSIX calls they and the server entry need.
//! The signal constants are re-exported too, so nothing above `platform`
//! has to name `libc` for a signal.
//!
//! Portability: the blocking wait is `sigwaitinfo` on Linux and `sigwait`
//! on macOS (the libc crate exposes only the latter for the BSD family;
//! both wrap the same POSIX wait and both can be interrupted by `EINTR`).
//! The reference `SIGNAL_NAMES` table is Linux-numbered, so macOS keeps
//! its own table (signal numbers differ: `SIGUSR1`/`SIGUSR2` are 30/31
//! and there is a `SIGEMT`/`SIGINFO` but no `SIGSTKFLT`/`SIGPWR`). The
//! quit signals the server uses -- `SIGTERM`, `SIGINT`, `SIGQUIT` --
//! carry the same numbers on both platforms.

/// The `sigset_t` handle.
pub type SigSet = libc::sigset_t;
/// The `sigaction` descriptor.
pub type SigAction = libc::sigaction;
/// The `siginfo_t` delivered with a queued signal.
pub type SigInfo = libc::siginfo_t;
/// The `pthread_t` handle.
pub type ThreadId = libc::pthread_t;

// The signal constants the server layer and its tests name.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use libc::SIGSYS;
pub use libc::{EINTR, SIGINT, SIGKILL, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2};
pub use libc::{SIG_BLOCK, SIG_DFL, SIG_IGN, SIG_SETMASK};
#[cfg(target_os = "macos")]
pub use libc::{SIGEMT, SIGINFO};

/// `SIGMIN`.
pub const SIGMIN: i32 = 1;

/// `SIGMAX`: the highest standard signal, which bounds the `AtomicU32`
/// masks in server-runtime (both platforms number their highest signal
/// 31, so the masks stay identical; only the table below differs).
#[cfg(target_os = "linux")]
pub const SIGMAX: i32 = libc::SIGSYS;
#[cfg(target_os = "macos")]
pub const SIGMAX: i32 = libc::SIGUSR2;

/// The `SIGNAL_NAMES` table, `SIGMIN..=SIGMAX` (index 0 unused).
///
/// The reference table is Linux-numbered; macOS numbers several signals
/// differently (`SIGUSR1`/`SIGUSR2` are 30/31, `SIGBUS` is 10, and
/// `SIGEMT`/`SIGINFO` replace the Linux `SIGSTKFLT`/`SIGPWR` slots), so
/// each platform carries its own. Index `n` is signal number `n`.
#[cfg(target_os = "linux")]
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

/// The macOS-numbered table: `SIGEMT` at 7, `SIGBUS` at 10, `SIGSYS` at
/// 12, `SIGURG` at 16, `SIGCHLD` at 20, `SIGIO` at 23, `SIGINFO` at 29,
/// `SIGUSR1` at 30, `SIGUSR2` at 31. The signal *names* are the same;
/// only the numbers move.
#[cfg(target_os = "macos")]
pub const SIGNAL_NAMES: [&str; 32] = [
    "",
    "SIGHUP",
    "SIGINT",
    "SIGQUIT",
    "SIGILL",
    "SIGTRAP",
    "SIGABRT",
    "SIGEMT",
    "SIGFPE",
    "SIGKILL",
    "SIGBUS",
    "SIGSEGV",
    "SIGSYS",
    "SIGPIPE",
    "SIGALRM",
    "SIGTERM",
    "SIGURG",
    "SIGSTOP",
    "SIGTSTP",
    "SIGCONT",
    "SIGCHLD",
    "SIGTTIN",
    "SIGTTOU",
    "SIGIO",
    "SIGXCPU",
    "SIGXFSZ",
    "SIGVTALRM",
    "SIGPROF",
    "SIGWINCH",
    "SIGINFO",
    "SIGUSR1",
    "SIGUSR2",
];

/// `sigemptyset`.
pub fn set_clear(set: &mut SigSet) {
    unsafe { libc::sigemptyset(set) };
}

/// `sigfillset`.
pub fn set_fill(set: &mut SigSet) {
    unsafe { libc::sigfillset(set) };
}

/// `sigaddset`.
pub fn set_add(set: &mut SigSet, signal: i32) {
    unsafe { libc::sigaddset(set, signal) };
}

/// `sigdelset`.
pub fn set_del(set: &mut SigSet, signal: i32) {
    unsafe { libc::sigdelset(set, signal) };
}

/// `sigismember`: is `signal` in `set`?
pub fn set_member(set: &SigSet, signal: i32) -> bool {
    unsafe { libc::sigismember(set, signal) == 1 }
}

/// A fresh empty set (`Set(EMPTY)`).
pub fn empty_set() -> SigSet {
    let mut set: SigSet = unsafe { std::mem::zeroed() };
    set_clear(&mut set);
    set
}

/// A fresh full set (`Set(FULL)`).
pub fn full_set() -> SigSet {
    let mut set: SigSet = unsafe { std::mem::zeroed() };
    set_fill(&mut set);
    set
}

/// `sigpending`: the kernel's pending mask.
pub fn pending_set() -> SigSet {
    let mut set: SigSet = unsafe { std::mem::zeroed() };
    unsafe { libc::sigpending(&mut set) };
    set
}

/// `pthread_sigmask` (the POSIX per-thread mask): `old` receives the
/// previous mask. Returns the error, or 0 on success.
pub fn block_mask(how: i32, set: &SigSet, old: &mut SigSet) -> i32 {
    unsafe { libc::pthread_sigmask(how, set, old) }
}

/// `pthread_sigmask` with no out-parameter (restore a saved mask).
pub fn restore_mask(how: i32, set: &SigSet) -> i32 {
    unsafe { libc::pthread_sigmask(how, set, std::ptr::null_mut()) }
}

/// `raise`: send a signal to the calling thread.
pub fn raise(signal: i32) -> i32 {
    unsafe { libc::raise(signal) }
}

/// `kill`: send a signal to a process (`pid < 0` targets the process
/// group `-pid`).
pub fn kill(pid: i32, signal: i32) -> i32 {
    unsafe { libc::kill(pid, signal) }
}

/// The calling thread's id.
pub fn thread_self() -> ThreadId {
    unsafe { libc::pthread_self() }
}

/// `pthread_kill`: send a signal to a thread.
pub fn thread_kill(thread: ThreadId, signal: i32) -> i32 {
    unsafe { libc::pthread_kill(thread, signal) }
}

/// `signal(sig, SIG_IGN)`: ignore a signal; returns the previous
/// disposition.
pub fn ignore(signal: i32) -> usize {
    unsafe { libc::signal(signal, SIG_IGN) }
}

/// `signal(sig, SIG_DFL)`: restore the default disposition; returns the
/// previous disposition.
pub fn default(signal: i32) -> usize {
    unsafe { libc::signal(signal, SIG_DFL) }
}

/// `sigaction` query: the current `sa_sigaction` disposition (tests
/// observe `SIG_DFL`/`SIG_IGN`/handler through it).
pub fn current_disposition(signal: i32) -> usize {
    let mut old: SigAction = unsafe { std::mem::zeroed() };
    unsafe { libc::sigaction(signal, std::ptr::null(), &mut old) };
    old.sa_sigaction
}

/// `sigaction` install: a handler with a full signal mask (all signals
/// blocked while it runs) and no flags (`enableDetectSignal`).
pub fn install_action(signal: i32, handler: usize) {
    let mut action: SigAction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler;
    set_fill(&mut action.sa_mask);
    action.sa_flags = 0;
    unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
}

/// The glibc real-time signal range, when there is one (Linux only;
/// `SIGRTMIN`/`SIGRTMAX` are glibc calls and macOS has no real-time
/// range to render).
#[cfg(target_os = "linux")]
pub fn realtime_range() -> Option<(i32, i32)> {
    Some((libc::SIGRTMIN(), libc::SIGRTMAX()))
}

/// macOS has no real-time signal range.
#[cfg(not(target_os = "linux"))]
pub fn realtime_range() -> Option<(i32, i32)> {
    None
}

/// Block until one signal in `set` arrives and return it; a negative
/// result means the wait failed (the negation of the error, so `EINTR`
/// reads as `-EINTR`).
///
/// `sigwaitinfo` (Linux) hands back the signal number and reports
/// failures through `errno`; `sigwait` (macOS, and the libc crate's
/// spelling for the rest of the BSD family) hands it back through an
/// out-parameter and *returns* the error itself, so nothing is in
/// `errno` to read. Both block until a member of `set` arrives, which is
/// the POSIX contract the reference relies on.
pub fn wait(set: &SigSet) -> i32 {
    #[cfg(target_os = "linux")]
    {
        let signal = unsafe { libc::sigwaitinfo(set, std::ptr::null_mut()) };
        if signal >= 0 {
            signal
        } else {
            -std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(EINTR)
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut signal: libc::c_int = 0;
        let rc = unsafe { libc::sigwait(set, &mut signal) };
        if rc == 0 { signal } else { -rc }
    }
}

/// The name of a signal, `"SIGRTxxx"` for a real-time signal (Linux
/// only), or `None` outside the standard range (index 0 is unused).
pub fn signal_name(signal: i32) -> Option<&'static str> {
    if let Some((rt_min, rt_max)) = realtime_range()
        && (rt_min..=rt_max).contains(&signal)
    {
        return Some("SIGRTxxx");
    }
    if signal != 0 && (SIGMIN..=SIGMAX).contains(&signal) {
        Some(SIGNAL_NAMES[signal as usize])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises every test that raises or blocks a signal: dispositions
    /// and masks are process-wide, and cargo runs tests on parallel
    /// threads. The guard only serialises, it guards no invariants.
    static SIGNAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The standard range renders its names, index 0 and out-of-range do
    /// not.
    #[test]
    fn signal_name_covers_the_standard_range() {
        assert_eq!(signal_name(SIGTERM), Some("SIGTERM"));
        assert_eq!(signal_name(SIGINT), Some("SIGINT"));
        assert_eq!(signal_name(SIGQUIT), Some("SIGQUIT"));
        assert_eq!(signal_name(SIGUSR1), Some("SIGUSR1"));
        assert_eq!(signal_name(0), None, "index 0 is unused");
        assert_eq!(signal_name(-1), None, "below the range");
        assert_eq!(signal_name(SIGMAX + 1), None, "above the range");
    }

    /// The Linux real-time range renders as one literal.
    #[cfg(target_os = "linux")]
    #[test]
    fn signal_name_renders_the_realtime_range() {
        let (rt_min, _) = realtime_range().expect("Linux has a real-time range");
        assert_eq!(signal_name(rt_min), Some("SIGRTxxx"));
    }

    /// The set helpers track membership and the empty/full constructors
    /// disagree.
    #[test]
    fn set_helpers_track_membership() {
        let mut set = empty_set();
        assert!(!set_member(&set, SIGTERM));
        set_add(&mut set, SIGTERM);
        assert!(set_member(&set, SIGTERM));
        set_del(&mut set, SIGTERM);
        assert!(!set_member(&set, SIGTERM));
        assert!(
            set_member(&full_set(), SIGTERM),
            "FULL contains every signal"
        );
    }

    /// A blocked `raise` becomes pending and the wait consumes it.
    #[test]
    fn wait_reports_a_raised_member() {
        let _guard = SIGNAL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut set = empty_set();
        set_add(&mut set, SIGUSR1);
        let mut old: SigSet = unsafe { std::mem::zeroed() };
        assert_eq!(block_mask(SIG_BLOCK, &set, &mut old), 0, "block first");
        assert_eq!(raise(SIGUSR1), 0, "raise while blocked");
        assert_eq!(wait(&set), SIGUSR1, "the wait reports the raised member");
        assert_eq!(restore_mask(SIG_SETMASK, &old), 0, "restore the mask");
    }

    /// A blocked `raise` shows up in `sigpending` until consumed.
    #[test]
    fn pending_set_sees_a_blocked_signal() {
        let _guard = SIGNAL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut set = empty_set();
        set_add(&mut set, SIGUSR1);
        let mut old: SigSet = unsafe { std::mem::zeroed() };
        assert_eq!(block_mask(SIG_BLOCK, &set, &mut old), 0);
        assert_eq!(raise(SIGUSR1), 0);
        assert!(set_member(&pending_set(), SIGUSR1), "pending while blocked");
        assert_eq!(wait(&set), SIGUSR1, "consume it");
        assert_eq!(restore_mask(SIG_SETMASK, &old), 0);
    }
}
