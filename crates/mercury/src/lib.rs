//! BW-02 transports, message framing and RPC.

pub mod bundle_dispatch;
#[cfg(target_os = "linux")]
pub mod channel_listener;
#[cfg(target_os = "linux")]
pub mod channel_map;
pub mod circular_array;
#[cfg(target_os = "linux")]
pub mod condemned_channels;
pub mod endpoint;
#[cfg(target_os = "linux")]
pub mod event_dispatcher;
pub mod fragmented_bundle;
pub mod frequent_tasks;
pub mod initial_connection_filter;
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
pub mod packet_receiver_stats;

/// The event poller (BW-02-01-02). Linux only, because the reference's
/// `EPoller` backend is what `EventPoller::create()` picks under
/// `HAS_EPOLL` and no other backend is ported yet.
#[cfg(target_os = "linux")]
pub mod poller;

pub mod reply_order;
pub mod request;
pub mod request_manager;
#[cfg(target_os = "linux")]
pub mod tcp_channel;
pub mod tcp_frame;
pub mod tcp_send;
pub mod udp_bundle;
#[cfg(target_os = "linux")]
pub mod udp_channel;
pub mod unacked_packet;
pub mod unpacked_message_header;
