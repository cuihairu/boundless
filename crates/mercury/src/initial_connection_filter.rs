//! `InitialConnectionFilter` (`server/baseapp/initial_connection_filter.{hpp,cpp}`):
//! the connection filter the reference enables by default on a BaseApp's
//! external interface (`baseapp.cpp:160`, `pExternalInterfaceFilter_`).
//!
//! Before a channel exists, the only datagram an external peer may send is
//! a single `baseAppLogin` request; anything else is noise or malicious and
//! is dropped. The rule (`initial_connection_filter.cpp:44-64`): admit a
//! packet only when its flags are exactly `FLAG_HAS_REQUESTS`, its total
//! size is the exact wire size of one `baseAppLogin` request, and its first
//! body byte is the `baseAppLogin` message id. An admitted packet goes to
//! the base `PacketFilter::recv`, i.e. the receiver's filtered path; a
//! denied one returns `REASON_GENERAL_NETWORK` after a warning when the
//! receiver is verbose.
//!
//! The wire size, from the reference's literal arithmetic
//! (`sizeof( Flags ) + sizeof( MessageID ) + sizeof( baseAppLoginArgs ) +
//!  sizeof( ReplyID ) + 2 * sizeof( Offset )`):
//!
//! | field                     | bytes |
//! | ------------------------- | ----- |
//! | `Flags`                   | 2     |
//! | `MessageID`               | 1     |
//! | `baseAppLoginArgs`        | 5     |
//! | `ReplyID`                 | 4     |
//! | two `Offset`s (link, firstRequestOffset) | 4 |
//!
//! The interface (`BaseAppExtInterface`) that owns `baseAppLogin`'s id is
//! not ported -- it is a server message table, and the port's message
//! dispatch is BW-02-02's seam -- so the login message id is a constructor
//! parameter instead of `BaseAppExtInterface::baseAppLogin.id()`.

use crate::endpoint::Address;
use crate::misc::Reason;
use crate::network_interface::NetworkInterface;
use crate::packet::{HEADER_SIZE, Packet, flags};
use crate::packet_filter::PacketFilter;

/// The total wire size of one `baseAppLogin` request: 16 bytes.
const BASE_APP_LOGIN_SIZE: usize = 16;

/// The connection filter of a BaseApp's external interface.
pub struct InitialConnectionFilter {
    /// `BaseAppExtInterface::baseAppLogin.id()`.
    login_message_id: u8,
}

impl InitialConnectionFilter {
    /// A filter admitting the `baseAppLogin` request carrying the given
    /// message id. `InitialConnectionFilter::create()`.
    pub fn new(login_message_id: u8) -> Self {
        Self { login_message_id }
    }
}

impl PacketFilter for InitialConnectionFilter {
    fn recv(&self, interface: &NetworkInterface, addr: &Address, packet: &mut Packet) -> Reason {
        // Allow a request only, of exactly the wire shape, naming
        // baseAppLogin. `bodySize() >= 1` keeps the id read in bounds on a
        // crafted empty-bodied packet, where the reference's `body()[0]`
        // would read past the message.
        if packet.flags() == flags::HAS_REQUESTS
            && packet.total_size() == BASE_APP_LOGIN_SIZE
            && packet.body_size() >= 1
            && packet.data()[HEADER_SIZE] == self.login_message_id
        {
            return interface.process_filtered_packet(addr, packet);
        }

        if interface.is_verbose() {
            eprintln!(
                "InitialConnectionFilter::recv( {} ): \
                 Dropping unexpected packet on BaseApps external interface \
                 (noise/malicious)",
                addr.c_str()
            );
        }

        Reason::GeneralNetwork
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default `max_spare_size` is zero, as the reference's base and
    /// its `InitialConnectionFilter::maxSpareSize()` both return.
    #[test]
    fn the_filter_needs_no_spare_size() {
        let filter = InitialConnectionFilter::new(0x0D);
        assert_eq!(filter.max_spare_size(), 0);
    }
}
