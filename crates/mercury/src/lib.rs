//! BW-02 transports, message framing and RPC.

pub mod block_cipher;
pub mod bundle_dispatch;
#[cfg(target_os = "linux")]
pub mod channel_listener;
#[cfg(target_os = "linux")]
pub mod channel_map;
pub mod circular_array;
#[cfg(target_os = "linux")]
pub mod condemned_channels;
pub mod encryption_filter;
pub mod endpoint;
#[cfg(target_os = "linux")]
pub mod event_dispatcher;
pub mod fragmented_bundle;
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
pub mod packet_filter;
#[cfg(target_os = "linux")]
pub mod packet_receiver;
#[cfg(target_os = "linux")]
pub mod packet_sender;

/// The event poller (BW-02-01-02). Linux only, because the reference's
/// `EPoller` backend is what `EventPoller::create()` picks under
/// `HAS_EPOLL` and no other backend is ported yet.
#[cfg(target_os = "linux")]
pub mod poller;

pub mod rate_limiter;
pub mod reply_order;
pub mod request;
pub mod request_manager;
pub mod tcp_bundle;
pub mod tcp_channel;
pub mod udp_bundle;
#[cfg(target_os = "linux")]
pub mod udp_channel;
pub mod unacked_packet;
pub mod unpacked_message_header;
