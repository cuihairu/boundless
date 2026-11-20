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
