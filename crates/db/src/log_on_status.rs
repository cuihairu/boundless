//! The log-on status codes (`lib/connection/log_on_status.hpp`).
//!
//! One shared table so every billing face speaks the reference's numbers:
//! the DBApp's failure replies carry `uint8(status.value())`, and the
//! Python billing response validates the raw `failure(errorCode)` the
//! script passes against `LogOnStatus::okay()` — the value's meaning
//! lives in this table, nowhere else.
//!
//! The value is an `i32` because the reference's `failure(int)` casts the
//! script's integer straight into the enum — `statusAsInt != value()`
//! never fires, so any non-`okay()` integer rides through and only the
//! wire write truncates it to `uint8`. The client half of the table
//! (including its own re-numbered `LOGIN_REJECTED_*` values) is the login
//! lane's, `server-runtime/src/client_login.rs`; this module is the whole
//! enum of `log_on_status.hpp` as the server reads it.
//!
//! Layering: domain library (layer 2).

/// The `LogOnStatus::Status` values (`log_on_status.hpp:15-76`).
pub mod codes {
    // client status values
    /// `NOT_SET`.
    pub const NOT_SET: i32 = 0;
    /// `LOGGED_ON`.
    pub const LOGGED_ON: i32 = 1;
    /// `LOGGED_ON_OFFLINE`.
    pub const LOGGED_ON_OFFLINE: i32 = 2;
    /// `CONNECTION_FAILED`.
    pub const CONNECTION_FAILED: i32 = 3;
    /// `DNS_LOOKUP_FAILED`.
    pub const DNS_LOOKUP_FAILED: i32 = 4;
    /// `UNKNOWN_ERROR`.
    pub const UNKNOWN_ERROR: i32 = 5;
    /// `CANCELLED`.
    pub const CANCELLED: i32 = 6;
    /// `ALREADY_ONLINE_LOCALLY`.
    pub const ALREADY_ONLINE_LOCALLY: i32 = 7;
    /// `PUBLIC_KEY_LOOKUP_FAILED`.
    pub const PUBLIC_KEY_LOOKUP_FAILED: i32 = 8;
    /// `LAST_CLIENT_SIDE_VALUE`.
    pub const LAST_CLIENT_SIDE_VALUE: i32 = 63;

    // server status values
    /// `LOGIN_MALFORMED_REQUEST`.
    pub const LOGIN_MALFORMED_REQUEST: i32 = 64;
    /// `LOGIN_BAD_PROTOCOL_VERSION`.
    pub const LOGIN_BAD_PROTOCOL_VERSION: i32 = 65;
    /// `LOGIN_CHALLENGE_ISSUED`.
    pub const LOGIN_CHALLENGE_ISSUED: i32 = 66;
    /// `LOGIN_REJECTED_NO_SUCH_USER`.
    pub const LOGIN_REJECTED_NO_SUCH_USER: i32 = 67;
    /// `LOGIN_REJECTED_INVALID_PASSWORD`.
    pub const LOGIN_REJECTED_INVALID_PASSWORD: i32 = 68;
    /// `LOGIN_REJECTED_ALREADY_LOGGED_IN`.
    pub const LOGIN_REJECTED_ALREADY_LOGGED_IN: i32 = 69;
    /// `LOGIN_REJECTED_BAD_DIGEST`.
    pub const LOGIN_REJECTED_BAD_DIGEST: i32 = 70;
    /// `LOGIN_REJECTED_DB_GENERAL_FAILURE`.
    pub const LOGIN_REJECTED_DB_GENERAL_FAILURE: i32 = 71;
    /// `LOGIN_REJECTED_DB_NOT_READY`.
    pub const LOGIN_REJECTED_DB_NOT_READY: i32 = 72;
    /// `LOGIN_REJECTED_ILLEGAL_CHARACTERS`.
    pub const LOGIN_REJECTED_ILLEGAL_CHARACTERS: i32 = 73;
    /// `LOGIN_REJECTED_SERVER_NOT_READY`.
    pub const LOGIN_REJECTED_SERVER_NOT_READY: i32 = 74;
    /// `LOGIN_REJECTED_UPDATER_NOT_READY` (no longer used).
    pub const LOGIN_REJECTED_UPDATER_NOT_READY: i32 = 75;
    /// `LOGIN_REJECTED_NO_BASEAPPS`.
    pub const LOGIN_REJECTED_NO_BASEAPPS: i32 = 76;
    /// `LOGIN_REJECTED_BASEAPP_OVERLOAD`.
    pub const LOGIN_REJECTED_BASEAPP_OVERLOAD: i32 = 77;
    /// `LOGIN_REJECTED_CELLAPP_OVERLOAD`.
    pub const LOGIN_REJECTED_CELLAPP_OVERLOAD: i32 = 78;
    /// `LOGIN_REJECTED_BASEAPP_TIMEOUT`.
    pub const LOGIN_REJECTED_BASEAPP_TIMEOUT: i32 = 79;
    /// `LOGIN_REJECTED_BASEAPPMGR_TIMEOUT`.
    pub const LOGIN_REJECTED_BASEAPPMGR_TIMEOUT: i32 = 80;
    /// `LOGIN_REJECTED_DBAPP_OVERLOAD`.
    pub const LOGIN_REJECTED_DBAPP_OVERLOAD: i32 = 81;
    /// `LOGIN_REJECTED_LOGINS_NOT_ALLOWED`.
    pub const LOGIN_REJECTED_LOGINS_NOT_ALLOWED: i32 = 82;
    /// `LOGIN_REJECTED_RATE_LIMITED`.
    pub const LOGIN_REJECTED_RATE_LIMITED: i32 = 83;
    /// `LOGIN_REJECTED_BAN`.
    pub const LOGIN_REJECTED_BAN: i32 = 84;
    /// `LOGIN_REJECTED_CHALLENGE_ERROR`.
    pub const LOGIN_REJECTED_CHALLENGE_ERROR: i32 = 85;
    /// `LOGIN_REJECTED_AUTH_SERVICE_NO_SUCH_ACCOUNT`.
    pub const LOGIN_REJECTED_AUTH_SERVICE_NO_SUCH_ACCOUNT: i32 = 86;
    /// `LOGIN_REJECTED_AUTH_SERVICE_LOGIN_DISALLOWED`.
    pub const LOGIN_REJECTED_AUTH_SERVICE_LOGIN_DISALLOWED: i32 = 87;
    /// `LOGIN_REJECTED_AUTH_SERVICE_UNREACHABLE`.
    pub const LOGIN_REJECTED_AUTH_SERVICE_UNREACHABLE: i32 = 88;
    /// `LOGIN_REJECTED_AUTH_SERVICE_INVALID_RESPONSE`.
    pub const LOGIN_REJECTED_AUTH_SERVICE_INVALID_RESPONSE: i32 = 89;
    /// `LOGIN_REJECTED_AUTH_SERVICE_GENERAL_FAILURE`.
    pub const LOGIN_REJECTED_AUTH_SERVICE_GENERAL_FAILURE: i32 = 90;
    /// `LOGIN_REJECTED_NO_LOGINAPP`.
    pub const LOGIN_REJECTED_NO_LOGINAPP: i32 = 91;
    /// `LOGIN_REJECTED_NO_LOGINAPP_RESPONSE`.
    pub const LOGIN_REJECTED_NO_LOGINAPP_RESPONSE: i32 = 92;
    /// `LOGIN_REJECTED_NO_BASEAPP_RESPONSE`.
    pub const LOGIN_REJECTED_NO_BASEAPP_RESPONSE: i32 = 93;
    /// `LOGIN_REJECTED_IP_ADDRESS_BAN`.
    pub const LOGIN_REJECTED_IP_ADDRESS_BAN: i32 = 244;
    /// `LOGIN_REJECTED_INACCESSIBLE_REALM`.
    pub const LOGIN_REJECTED_INACCESSIBLE_REALM: i32 = 245;
    /// `LOGIN_REJECTED_REGISTRATION_NOT_ALLOWED`.
    pub const LOGIN_REJECTED_REGISTRATION_NOT_ALLOWED: i32 = 246;
    /// `LOGIN_REJECTED_REGISTRATION_NOT_CONFIRMED`.
    pub const LOGIN_REJECTED_REGISTRATION_NOT_CONFIRMED: i32 = 247;
    /// `LOGIN_REJECTED_NOT_REGISTERED`.
    pub const LOGIN_REJECTED_NOT_REGISTERED: i32 = 248;
    /// `LOGIN_REJECTED_ACTIVATING`.
    pub const LOGIN_REJECTED_ACTIVATING: i32 = 249;
    /// `LOGIN_REJECTED_UNABLE_TO_PARSE_JSON`.
    pub const LOGIN_REJECTED_UNABLE_TO_PARSE_JSON: i32 = 250;
    /// `LOGIN_REJECTED_USERS_LIMIT`.
    pub const LOGIN_REJECTED_USERS_LIMIT: i32 = 251;
    /// `LOGIN_REJECTED_LOGIN_QUEUE`.
    pub const LOGIN_REJECTED_LOGIN_QUEUE: i32 = 252;
    /// `LOGIN_CUSTOM_DEFINED_ERROR`.
    pub const LOGIN_CUSTOM_DEFINED_ERROR: i32 = 254;
    /// `LAST_SERVER_SIDE_VALUE`.
    pub const LAST_SERVER_SIDE_VALUE: i32 = 255;
}

/// A `LogOnStatus` value with the reference's predicates
/// (`log_on_status.hpp:8-116`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogOnStatus(i32);

impl LogOnStatus {
    /// Constructor (`LogOnStatus(Status)`; the `failure(int)` path casts
    /// the script's integer straight in).
    pub fn new(value: i32) -> Self {
        Self(value)
    }

    /// `value()`: the raw status value.
    pub fn value(self) -> i32 {
        self.0
    }

    /// `succeeded()`: the logon succeeded.
    pub fn succeeded(self) -> bool {
        self.0 == codes::LOGGED_ON
    }

    /// `fatal()`: the logon failed in a way that cannot be retried.
    pub fn fatal(self) -> bool {
        self.0 == codes::CONNECTION_FAILED
            || self.0 == codes::CANCELLED
            || self.0 == codes::UNKNOWN_ERROR
    }

    /// `okay()`: the logon succeeded or is still pending.
    pub fn okay(self) -> bool {
        self.0 == codes::NOT_SET || self.0 == codes::LOGGED_ON
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The enumerators keep `log_on_status.hpp`'s numbers: the client
    /// block 0..=8, `LAST_CLIENT_SIDE_VALUE`'s 63, the server block
    /// 64..=93, then the explicit 244..=252 / 254 / 255 tail.
    #[test]
    fn the_status_values_follow_the_reference_numbering() {
        assert_eq!(codes::NOT_SET, 0);
        assert_eq!(codes::LOGGED_ON, 1);
        assert_eq!(codes::PUBLIC_KEY_LOOKUP_FAILED, 8);
        assert_eq!(codes::LAST_CLIENT_SIDE_VALUE, 63);
        assert_eq!(codes::LOGIN_MALFORMED_REQUEST, 64);
        assert_eq!(codes::LOGIN_BAD_PROTOCOL_VERSION, 65);
        assert_eq!(codes::LOGIN_CHALLENGE_ISSUED, 66);
        assert_eq!(codes::LOGIN_REJECTED_NO_SUCH_USER, 67);
        assert_eq!(codes::LOGIN_REJECTED_INVALID_PASSWORD, 68);
        assert_eq!(codes::LOGIN_REJECTED_ALREADY_LOGGED_IN, 69);
        assert_eq!(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE, 71);
        assert_eq!(codes::LOGIN_REJECTED_BAN, 84);
        assert_eq!(codes::LOGIN_REJECTED_CHALLENGE_ERROR, 85);
        assert_eq!(codes::LOGIN_REJECTED_AUTH_SERVICE_GENERAL_FAILURE, 90);
        assert_eq!(codes::LOGIN_REJECTED_NO_LOGINAPP, 91);
        assert_eq!(codes::LOGIN_REJECTED_NO_LOGINAPP_RESPONSE, 92);
        assert_eq!(codes::LOGIN_REJECTED_NO_BASEAPP_RESPONSE, 93);
        assert_eq!(codes::LOGIN_REJECTED_USERS_LIMIT, 251);
        assert_eq!(codes::LOGIN_CUSTOM_DEFINED_ERROR, 254);
        assert_eq!(codes::LAST_SERVER_SIDE_VALUE, 255);
    }

    #[test]
    fn the_predicates_match_the_reference() {
        assert!(LogOnStatus::new(codes::NOT_SET).okay());
        assert!(LogOnStatus::new(codes::LOGGED_ON).okay());
        assert!(!LogOnStatus::new(codes::LOGGED_ON_OFFLINE).okay());

        assert!(LogOnStatus::new(codes::LOGGED_ON).succeeded());
        assert!(!LogOnStatus::new(codes::LOGGED_ON_OFFLINE).succeeded());

        assert!(LogOnStatus::new(codes::CONNECTION_FAILED).fatal());
        assert!(LogOnStatus::new(codes::CANCELLED).fatal());
        assert!(LogOnStatus::new(codes::UNKNOWN_ERROR).fatal());
        assert!(!LogOnStatus::new(codes::DNS_LOOKUP_FAILED).fatal());

        assert_eq!(
            LogOnStatus::new(codes::LOGIN_REJECTED_BAN).value(),
            codes::LOGIN_REJECTED_BAN
        );
    }
}
