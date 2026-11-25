//! BW-15 operating system interfaces. OS calls live here only (BW-16-02-01).

pub mod clock;

/// `ioctl`-based network-interface enumeration (`endpoint.hpp:175-192`).
/// Unix only: `ioctl` is the POSIX spelling; the Windows interface
/// enumeration (`GetAdaptersAddresses`) is a separate BW-15-03 item.
#[cfg(unix)]
pub mod interface;

#[cfg(unix)]
pub mod socket;

/// Subprocess control and exit-status decoding (BW-15-01-03,
/// `lib/server/process.hpp`). `fork`/`execvp`/`waitpid`/`kill` on unix,
/// `CreateProcessW`/`WaitForSingleObject`/`TerminateProcess` on
/// Windows, behind one API.
pub mod process;

/// Path casing, encoding and system directories (BW-15-01-06). Unix
/// only: `$HOME`/`$TMPDIR`/`realpath`; Windows `%USERPROFILE%`/
/// `%TEMP%`/`GetFullPathNameW` is a separate BW-15-03 item.
#[cfg(unix)]
pub mod path;

/// `epoll` primitives for the Mercury event poller. Linux only: the
/// reference compiles `EPoller` only when `HAS_EPOLL` is defined
/// (`__linux__ && !EMSCRIPTEN`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
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

/// Read-only memory mapping and dynamic-library lookup (BW-15-01-05).
/// `mmap(2)`/`dlopen` on unix, `CreateFileMapping`/`LoadLibrary` on
/// Windows; both spellings share one API so resource readers stay
/// platform-agnostic.
pub mod mmap;

/// Signal syscalls and the `sigset_t` family, the sole landing point for
/// them (BW-16-02-01). Unix only: `sigaction`/`pthread_sigmask` are the
/// POSIX spelling; Windows console control events are a separate
/// BW-15-03 item.
#[cfg(unix)]
pub mod signals;
