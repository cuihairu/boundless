//! `lib/network/packet_filter.{hpp,cpp}` (BW-02-05-01): the per-channel
//! packet filter, and the order the receive chain consults the filters in.
//!
//! A filter is handed every packet that leaves a channel and every packet
//! that arrives on one, and its verdict decides what Mercury does next:
//! the base class forwards both to the transport -- `basicSendWithRetries`
//! on the way out, `processFilteredPacket`'s header walk on the way in --
//! so an override is free to inspect, rewrite or drop a packet and still
//! hand it onward by calling the base.
//!
//! The order matters as much as the filters themselves, and it is fixed by
//! `PacketReceiver::processPacket` (packet_receiver.cpp:455-500): a
//! channel's own filter comes first, then the recently-dead check, then
//! the interface's off-channel filter, and the header walk last.

use std::rc::Rc;

use crate::endpoint::Address;
use crate::misc::Reason;
use crate::network_interface::NetworkInterface;
use crate::packet::Packet;
use crate::packet_receiver::PacketReceiver;
use crate::packet_sender::PacketSender;
use crate::udp_channel::ChannelPtr;

/// `Mercury::PacketFilter` (packet_filter.hpp:29): the interface a channel
/// installs to filter its packets.
pub trait PacketFilter {
    /// `send` (packet_filter.cpp:34): an outbound packet. The base sends
    /// it over the interface's socket with a few retries; calling it from
    /// an override is the way to pass the packet onward.
    fn send(&self, sender: &mut PacketSender, addr: &Address, packet: &Rc<Packet>) -> Reason {
        sender.basic_send_with_retries(addr, packet)
    }

    /// `recv` (packet_filter.cpp:44): an inbound packet. The base hands it
    /// to the header walk; calling it from an override is how the packet
    /// gets processed after the filter has had its look.
    ///
    /// Deviation from the reference's three-argument `recv`: the port's
    /// header walk needs the `interface` and the `channel` the reference
    /// keeps on the receiver (packet_receiver.cpp:526, :584), so they are
    /// threaded through here.
    fn recv(
        &self,
        receiver: &PacketReceiver,
        interface: &Rc<NetworkInterface>,
        addr: &Address,
        packet: &mut Packet,
        channel: Option<ChannelPtr>,
    ) -> Reason {
        receiver.process_filtered_packet(interface, addr, packet, channel)
    }

    /// `maxSpareSize` (packet_filter.hpp:61): the room an outbound filter
    /// may claim out of every packet, reserved when a packet opens
    /// (`newBundle`, udp_channel.cpp:1526-1531).
    fn max_spare_size(&self) -> usize {
        0
    }
}
