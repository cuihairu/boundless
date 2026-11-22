//! `Mercury::NubException` (`lib/network/nub_exception.hpp/.ipp`).
//!
//! The failure a reply handler receives in `ReplyMessageHandler::handleException`
//! / `handleShuttingDown`: a reason and, when one is known, the address it
//! happened at.

use crate::endpoint::Address;
use crate::misc::Reason;

/// `Mercury::NubException`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NubException {
    /// `reason_`.
    reason: Reason,
    /// `address_`.
    address: Address,
}

impl NubException {
    /// `NubException( reason, addr = Address::NONE )`.
    pub fn new(reason: Reason, address: Address) -> Self {
        Self { reason, address }
    }

    /// `reason()`.
    pub fn reason(&self) -> Reason {
        self.reason
    }

    /// `getAddress( addr )`: the address this exception occurred at, or
    /// `None` when it was not set (`Address::NONE`). The reference writes the
    /// address into an out-parameter and returns whether it was set; an
    /// `Option` carries the same information.
    pub fn get_address(&self) -> Option<Address> {
        if self.address.is_none() {
            None
        } else {
            Some(self.address)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::Address;

    #[test]
    fn a_none_address_reports_no_address() {
        let exception = NubException::new(Reason::TimerExpired, crate::endpoint::NONE);
        assert_eq!(exception.reason(), Reason::TimerExpired);
        assert_eq!(exception.get_address(), None);
    }

    #[test]
    fn a_real_address_is_reported() {
        let addr = Address::new(0x7f00_0001, 40001);
        let exception = NubException::new(Reason::ChannelLost, addr);
        assert_eq!(exception.get_address(), Some(addr));
    }
}
