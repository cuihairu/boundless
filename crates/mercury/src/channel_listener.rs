//! `lib/network/channel_listener.hpp`: the observer a channel reports its
//! sends and its death to.

use crate::udp_channel::ChannelPtr;

/// `Mercury::ChannelListener`: informs interested parties about events on
/// a channel.
pub trait ChannelListener {
    /// `onChannelSend`: the channel finished a send.
    fn on_channel_send(&self, _channel: &ChannelPtr) {}

    /// `onChannelGone`: the channel has become unusable, and any owned
    /// references should be removed.
    fn on_channel_gone(&self, _channel: &ChannelPtr) {}
}
