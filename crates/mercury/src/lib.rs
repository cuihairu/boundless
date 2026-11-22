//! BW-02 transports, message framing and RPC.

/// The block cipher `EncryptionFilter` encrypts a channel with (BW-02-05-02).
pub mod block_cipher;
pub mod bundle_dispatch;
#[cfg(target_os = "linux")]
pub mod channel_listener;
#[cfg(target_os = "linux")]
pub mod channel_map;
pub mod circular_array;
#[cfg(target_os = "linux")]
pub mod condemned_channels;
/// The per-channel encryption filter (BW-02-05-02). Linux only, because it
/// implements `PacketFilter`, whose base methods touch the socket paths.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
pub mod irregular_channels;
pub mod message_framing;
pub mod misc;
#[cfg(target_os = "linux")]
pub mod network_interface;
pub mod network_stats;
pub mod network_utils;
pub mod nub_exception;
pub mod packet;

/// The per-channel packet filter (BW-02-05-01). Linux only, because its
/// base methods call into the socket read and write paths.
#[cfg(target_os = "linux")]
pub mod packet_filter;

#[cfg(target_os = "linux")]
pub mod packet_receiver;

/// The socket write path (BW-02-03-06). Linux only, because it writes
/// through the interface's UDP socket and dispatcher.
#[cfg(target_os = "linux")]
pub mod packet_sender;

/// The event poller (BW-02-01-02). Linux only, because the reference's
/// `EPoller` backend is what `EventPoller::create()` picks under
/// `HAS_EPOLL` and no other backend is ported yet.
#[cfg(target_os = "linux")]
pub mod poller;

pub mod reliable_order;
pub mod reply_order;
pub mod request;
pub mod request_manager;

/// The session cipher: Blowfish, byte-compatible with the original's
/// OpenSSL (BW-02-05-02).
pub mod symmetric_block_cipher;

/// An external interface's recently-dead channel addresses (BW-02-05-01).
/// Linux only, because the expiry timers arm on the EventDispatcher.
#[cfg(target_os = "linux")]
pub mod recently_dead_channels;

pub mod tcp_bundle;
pub mod tcp_channel_lifecycle;
pub mod tcp_connection_header;
pub mod tcp_send_buffer;
pub mod udp_bundle;
#[cfg(target_os = "linux")]
pub mod udp_channel;
pub mod unacked_packet;
pub mod unpacked_message_header;

/// The BW-02-04-05 mixed-run fixture: the TCP transport components running
/// together (connection header, frame parsing, send path, disconnect
/// cleanup).
#[cfg(test)]
mod tcp_transport_tests;
