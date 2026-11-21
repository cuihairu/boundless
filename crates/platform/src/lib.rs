//! BW-15 operating system interfaces. OS calls live here only (BW-16-02-01).

pub mod clock;

/// `ioctl`-based network-interface enumeration (`endpoint.hpp:175-192`).
pub mod interface;

#[cfg(unix)]
pub mod socket;

/// `epoll` primitives for the Mercury event poller. Linux only: the
/// reference compiles `EPoller` only when `HAS_EPOLL` is defined
/// (`__linux__ && !EMSCRIPTEN`).
#[cfg(target_os = "linux")]
pub mod poller;

/// The self-pipe wakeup that makes an idle event loop interruptible
/// (BW-15-01-01). Unix only: `pipe(2)` is the same on Linux and macOS;
/// the Windows `WSAPoll` backend needs a socketpair and lands with the
/// Windows poller under BW-15-05.
#[cfg(unix)]
pub mod wakeup;

/// Advisory file locking and atomic replacement (BW-15-01-04). Unix
/// only: `flock(2)`/`rename(2)`; the Windows `LockFileEx`/`ReplaceFile`
/// backend lands with the Windows work under BW-15-05.
#[cfg(unix)]
pub mod file;
