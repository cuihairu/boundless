//! `NubException` (`lib/network/nub_exception.hpp`).
//!
//! The reason a request failed, plus the address it failed on. The
//! reference keeps the two together because the reason alone does not
//! say who to blame; here the address is optional because the request
//! layer has no channel to read it from until BW-02-03.

use crate::endpoint::Address;
use crate::misc::Reason;

/// `NubException`: the exception a `ReplyMessageHandler` is handed when a
/// request fails rather than gets answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NubException {
    reason: Reason,
    address: Option<Address>,
}

impl NubException {
    /// `NubException( reason, addr )` (`nub_exception.ipp`): a reason and,
    /// optionally, the address it came from.
    pub fn new(reason: Reason, address: Option<Address>) -> Self {
        NubException { reason, address }
    }

    /// `reason()`.
    pub fn reason(&self) -> Reason {
        self.reason
    }

    /// `getAddress()`: whether the address was supplied, and what it was.
    /// The reference returns false and leaves the argument untouched when
    /// there is none; the port returns the `Option` so the caller decides.
    pub fn get_address(&self) -> Option<Address> {
        self.address
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_and_an_address_are_kept_together() {
        let address = Address::new(0x7f000001, 1234);
        let exception = NubException::new(Reason::ChannelLost, Some(address));
        assert_eq!(exception.reason(), Reason::ChannelLost);
        assert_eq!(exception.get_address(), Some(address));
    }

    #[test]
    fn an_exception_may_have_no_address() {
        let exception = NubException::new(Reason::TimerExpired, None);
        assert_eq!(exception.reason(), Reason::TimerExpired);
        assert_eq!(exception.get_address(), None);
    }
}
