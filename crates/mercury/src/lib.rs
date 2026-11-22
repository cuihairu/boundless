//! BW-02 transports, message framing and RPC.

/// The channel registry (BW-02-03-02). Linux only: the registered channel
/// it holds is the Linux `UDPChannel`.
#[cfg(target_os = "linux")]
pub mod channel_map;
#[cfg(target_os = "linux")]
pub mod condemned_channels;
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
pub mod nub_exception;
pub mod packet;
#[cfg(target_os = "linux")]
pub mod packet_receiver;
#[cfg(target_os = "linux")]
pub mod recently_dead_channels;

/// The reply-correlation layer (BW-02-02-03). Linux only: it registers its
/// request timeouts on the Linux `EventDispatcher`.
#[cfg(target_os = "linux")]
pub mod request_manager;

/// The bundle encode/decode codec (BW-02-03-01). Linux only: it registers
/// request orders with the reply-correlation layer and carries the channel
/// reference the off-channel codec already names.
#[cfg(target_os = "linux")]
pub mod udp_bundle;
#[cfg(target_os = "linux")]
pub mod udp_bundle_processor;
#[cfg(target_os = "linux")]
pub mod udp_channel;

/// The event poller (BW-02-01-02). Linux only, because the reference's
/// `EPoller` backend is what `EventPoller::create()` picks under
/// `HAS_EPOLL` and no other backend is ported yet.
#[cfg(target_os = "linux")]
pub mod poller;

pub mod unpacked_message_header;
