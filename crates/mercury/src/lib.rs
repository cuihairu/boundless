//! BW-02 transports, message framing and RPC.

pub mod endpoint;
#[cfg(target_os = "linux")]
pub mod event_dispatcher;
pub mod frequent_tasks;
pub mod interface_element;
pub mod interface_minder;
pub mod interface_table;
pub mod interfaces;
pub mod message_framing;
pub mod misc;
#[cfg(target_os = "linux")]
pub mod network_interface;
pub mod network_stats;
pub mod network_utils;
pub mod packet;
#[cfg(target_os = "linux")]
pub mod packet_receiver;

/// The event poller (BW-02-01-02). Linux only, because the reference's
/// `EPoller` backend is what `EventPoller::create()` picks under
/// `HAS_EPOLL` and no other backend is ported yet.
#[cfg(target_os = "linux")]
pub mod poller;
