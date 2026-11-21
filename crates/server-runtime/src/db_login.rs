//! BW-06-01-04 forward a login to the DBApp and handle its reply.
//!
//! After the challenge and readiness gates pass, the reference LoginApp
//! forwards the login to the DBApp and answers from a
//! `DatabaseReplyHandler` (`server/loginapp/database_reply_handler.{hpp,cpp}`).
//! The forward request is `dbBundle << source << *pParams` — the client
//! address followed by the logon params (`LoginApp::login`); the reply is the
//! DBApp's answer, a status byte followed by a status-specific payload.
//!
//! `DatabaseReplyHandler::handleMessage` maps the reply to the client-facing
//! status (`server/loginapp/loginapp.cpp` `handleFailure` writes the status
//! byte and, when a description is present, the message string):
//!
//! * `LOGGED_ON`: the `LoginReplyRecord` and an optional server message are
//!   cached and sent (`sendAndCacheSuccess`);
//! * `LOGIN_REJECTED_IP_ADDRESS_BAN`: the payload is a ban-expiry string; the
//!   remaining seconds are sent back as the ban reply. A malformed expiry
//!   produces the bare status with no description and no cache erase;
//! * any other failure: the message string, or a standard unelaborated-error
//!   text when none is present;
//! * no reply (`handleException`, the timeout): `LOGIN_REJECTED_DBAPP_OVERLOAD`
//!   with the text "No reply from DBApp.";
//! * `handleShuttingDown`: the attempt is dropped with no reply.
//!
//! The DBApp reply's overload statuses also feed `LoginApp::systemOverloaded_`
//! (a failure that the `login()` entry gate then reports for a second).
//!
//! NAT firewall redirection (`NATConfig::isExternalIP`) is not ported here; it
//! belongs with the connection-address plumbing.

use foundation::stream::BinaryStream;

use crate::login_request::{LoginReplyRecord, ServerAddress};
use crate::loginapp::{LOG_ON_PARAMS_PASS_THRU, LogOnParams, LogOnStatus};

/// Wire size of a `LoginReplyRecord`: the address (ip, port, salt) plus the
/// session key.
pub const LOGIN_REPLY_RECORD_STREAM_SIZE: usize = 12;

/// `sizeof(LoginReplyRecord) - sizeof(int)`: an address with no session key —
/// what a DBApp that resolved a non-Proxy entity type returns.
const LOGIN_REPLY_RECORD_WITHOUT_KEY_SIZE: usize = 8;

/// The standard unelaborated-failure text (`database_reply_handler.cpp`).
pub const DB_UNELABORATED_ERROR: &str = "Database returned an unelaborated error. Check DBApp log.";
/// The standard non-Proxy entity type text (`database_reply_handler.cpp`).
pub const DB_NON_PROXY_ERROR: &str = "Database returned a non-proxy entity type.";
/// The standard unknown-error text (`database_reply_handler.cpp`).
pub const DB_UNKNOWN_ERROR: &str = "Database returned an unknown error.";
/// The standard no-reply text (`database_reply_handler.cpp` `handleException`).
pub const DB_NO_REPLY: &str = "No reply from DBApp.";
/// The standard IP ban text (`loginapp.cpp` `handleBanIP`).
pub const IP_BAN_MESSAGE: &str = "Logins from this IP address currently are not permitted";

/// The decoded DBApp reply, as the reference `DatabaseReplyHandler` turns it
/// into a client-facing action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbReplyOutcome {
    /// A failure status. The description is written to the client only when
    /// `Some`; the cached request is erased only when `erase_cache` (the
    /// reference passes `pParams` to `handleFailure`, which erases the cache
    /// when a params snapshot was taken).
    Failure {
        /// `handleFailure` status.
        status: u8,
        /// The description string, if one is written.
        message: Option<String>,
        /// Whether the cached request is erased.
        erase_cache: bool,
    },
    /// `LOGIN_REJECTED_IP_ADDRESS_BAN`: the IP is banned for the remaining
    /// seconds; the reply is the standard ban text.
    Ban {
        /// Remaining ban seconds (`max(0, expiry - now)`).
        ban_seconds: u64,
    },
    /// `LOGGED_ON`: the reply record and server message to cache and re-send.
    Success {
        /// The `LoginReplyRecord`.
        record: LoginReplyRecord,
        /// The server message.
        server_msg: String,
    },
    /// The DBApp is shutting down; the attempt is dropped with no reply.
    ShuttingDown,
}

impl DbReplyOutcome {
    /// `handleFailure` status byte, when this outcome carries one.
    pub fn status_byte(&self) -> Option<u8> {
        match self {
            DbReplyOutcome::Failure { status, .. } => Some(*status),
            DbReplyOutcome::Ban { .. } => Some(LogOnStatus::LoginRejectedIpAddressBan as u8),
            DbReplyOutcome::Success { .. } => Some(LogOnStatus::LoggedOn as u8),
            DbReplyOutcome::ShuttingDown => None,
        }
    }
}

/// The DBApp reply for a login that never answered (`handleException`): the
/// request timed out, reported as a DBApp overload failure.
pub fn db_reply_timeout() -> DbReplyOutcome {
    DbReplyOutcome::Failure {
        status: LogOnStatus::LoginRejectedDbAppOverload as u8,
        message: Some(DB_NO_REPLY.to_string()),
        erase_cache: true,
    }
}

/// `LoginApp::login` forward: `dbBundle << source << *pParams`.
///
/// The logon params are streamed with the `PASS_THRU` flags, exactly as the
/// reference `LogOnParams::operator<<` does.
pub fn write_logon_request(
    source: &ServerAddress,
    params: &LogOnParams,
    stream: &mut BinaryStream,
) {
    source.write_to_stream(stream);
    params.add_to_stream(stream, LOG_ON_PARAMS_PASS_THRU);
}

/// `DatabaseReplyHandler::handleMessage`: decode the DBApp reply into the
/// client-facing outcome.
///
/// `now_epoch` is the wall-clock time in seconds, used to convert the absolute
/// ban expiry the DBApp sends into a remaining duration (the reference
/// `::time(NULL)`). The IP ban reply's absolute expiry is parsed the way a C++
/// `istream >> time_t` would — optional leading whitespace and sign, then the
/// digits up to the first non-digit — so a malformed string decodes to zero
/// and takes the error path.
pub fn handle_db_reply(data: &mut BinaryStream, now_epoch: i64) -> DbReplyOutcome {
    let Ok(status) = data.read_u8() else {
        // A reply with no status byte is not decodable; treat it as a general
        // failure the same way the reference does for a short reply.
        return DbReplyOutcome::Failure {
            status: LogOnStatus::LoginRejectedDbGeneralFailure as u8,
            message: Some(DB_UNKNOWN_ERROR.to_string()),
            erase_cache: true,
        };
    };

    if status == LogOnStatus::LoginRejectedIpAddressBan as u8 {
        let expiry = data
            .read_string()
            .ok()
            .and_then(|s| parse_time_t(&s))
            .unwrap_or(0);
        if expiry == 0 {
            // The ban expiry was missing or malformed: the reference reports
            // the bare status with no description and, having no params
            // snapshot, does not erase the cached request.
            return DbReplyOutcome::Failure {
                status,
                message: None,
                erase_cache: false,
            };
        }
        let remaining = (expiry - now_epoch).max(0) as u64;
        return DbReplyOutcome::Ban {
            ban_seconds: remaining,
        };
    }

    if status != LogOnStatus::LoggedOn as u8 {
        let message = if data.remaining() > 0 {
            data.read_string().ok().unwrap_or_default()
        } else {
            DB_UNELABORATED_ERROR.to_string()
        };
        return DbReplyOutcome::Failure {
            status,
            message: Some(message),
            erase_cache: true,
        };
    }

    // `LOGGED_ON`.
    if data.remaining() < LOGIN_REPLY_RECORD_STREAM_SIZE {
        let (status, message) = if data.remaining() == LOGIN_REPLY_RECORD_WITHOUT_KEY_SIZE {
            (
                LogOnStatus::LoginCustomDefinedError as u8,
                DB_NON_PROXY_ERROR.to_string(),
            )
        } else {
            (
                LogOnStatus::LoginRejectedDbGeneralFailure as u8,
                DB_UNKNOWN_ERROR.to_string(),
            )
        };
        return DbReplyOutcome::Failure {
            status,
            message: Some(message),
            erase_cache: true,
        };
    }

    let mut record = LoginReplyRecord::new(ServerAddress::NONE, 0);
    if !record.read_from_stream(data) {
        return DbReplyOutcome::Failure {
            status: LogOnStatus::LoginRejectedDbGeneralFailure as u8,
            message: Some(DB_UNKNOWN_ERROR.to_string()),
            erase_cache: true,
        };
    }
    let server_msg = if data.remaining() > 0 {
        data.read_string().unwrap_or_default()
    } else {
        String::new()
    };
    DbReplyOutcome::Success { record, server_msg }
}

/// `BW::istringstream >> time_t`: skip leading whitespace and an optional sign,
/// then take the run of digits, stopping at the first non-digit. Returns `None`
/// when no digit is seen.
fn parse_time_t(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let negative = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let start = i;
    let mut value: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value
            .checked_mul(10)?
            .checked_add((bytes[i] - b'0') as i64)?;
        i += 1;
    }
    if i == start {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// `LoginApp::systemOverloaded_` / `systemOverloadedTime_` (`loginapp.hpp`):
/// the overload status a DBApp failure leaves behind, which the login entry
/// gate reports until it expires one second later.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemOverload {
    status: u8,
    time: u64,
}

impl SystemOverload {
    /// Constructor: not overloaded.
    pub fn new() -> Self {
        Self::default()
    }

    /// `systemOverloaded()`.
    pub fn status(&self) -> u8 {
        self.status
    }

    /// `systemOverloaded(status)` (`loginapp.hpp`): record the status at
    /// `now`.
    pub fn set(&mut self, status: u8, now: u64) {
        self.status = status;
        self.time = now;
    }

    /// `DatabaseReplyHandler::handleMessage` overload accounting: a DBApp or
    /// CellApp overload always records; a BaseApp overload records only when
    /// nothing is recorded yet.
    pub fn note_db_failure(&mut self, status: u8, now: u64) {
        let baseapp = status == LogOnStatus::LoginRejectedBaseAppOverload as u8;
        let cellapp = status == LogOnStatus::LoginRejectedCellAppOverload as u8;
        let dbapp = status == LogOnStatus::LoginRejectedDbAppOverload as u8;
        if (self.status == 0 && baseapp) || cellapp || dbapp {
            self.set(status, now);
        }
    }

    /// The `login()` expiry gate (`loginapp.cpp`): clear the remembered status
    /// once a second has passed since it was recorded. Returns the status the
    /// entry gate should reject with, or 0 when not overloaded.
    pub fn current(&mut self, now: u64) -> u8 {
        if self.status == 0 {
            return 0;
        }
        if self.time + 1_000_000_000 < now {
            self.status = 0;
        }
        self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success_reply() -> Vec<u8> {
        let mut stream = BinaryStream::new();
        stream.write_u8(LogOnStatus::LoggedOn as u8);
        LoginReplyRecord::new(ServerAddress::new(0x0a00_0001, 20000, 1), 0xdead_beef)
            .write_to_stream(&mut stream);
        stream.write_string("welcome").unwrap();
        stream.as_bytes().to_vec()
    }

    #[test]
    fn logon_request_streams_address_then_params() {
        let source = ServerAddress::new(0x0100_0007, 40000, 0x1234);
        let mut p = LogOnParams::with_credentials("alice", "pw", "key");
        p.set_nonce(7);
        let mut stream = BinaryStream::new();
        write_logon_request(&source, &p, &mut stream);
        // Address (8 bytes) then the params with PASS_THRU flags.
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        let mut addr = ServerAddress::NONE;
        assert!(addr.read_from_stream(&mut read));
        assert_eq!(addr, source);
        let mut back = LogOnParams::new();
        assert!(back.read_from_stream(&mut read));
        assert_eq!(back.username(), "alice");
        assert_eq!(back.password(), "pw");
        assert_eq!(back.encryption_key(), "key");
        assert_eq!(back.nonce(), 7);
        assert!(read.at_end());
    }

    #[test]
    fn a_success_reply_decodes_to_the_record_and_message() {
        let bytes = success_reply();
        let mut read = BinaryStream::from_slice(&bytes);
        let outcome = handle_db_reply(&mut read, 1000);
        assert_eq!(
            outcome,
            DbReplyOutcome::Success {
                record: LoginReplyRecord::new(
                    ServerAddress::new(0x0a00_0001, 20000, 1),
                    0xdead_beef
                ),
                server_msg: "welcome".to_string(),
            }
        );
    }

    #[test]
    fn a_success_reply_without_a_message_decodes_with_an_empty_message() {
        let mut stream = BinaryStream::new();
        stream.write_u8(LogOnStatus::LoggedOn as u8);
        LoginReplyRecord::new(ServerAddress::new(0x0a00_0001, 20000, 1), 0xdead_beef)
            .write_to_stream(&mut stream);
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        let outcome = handle_db_reply(&mut read, 1000);
        assert_eq!(
            outcome,
            DbReplyOutcome::Success {
                record: LoginReplyRecord::new(
                    ServerAddress::new(0x0a00_0001, 20000, 1),
                    0xdead_beef
                ),
                server_msg: String::new(),
            }
        );
    }

    #[test]
    fn a_failure_reply_carries_the_status_and_message() {
        let mut stream = BinaryStream::new();
        stream.write_u8(LogOnStatus::LoginRejectedInvalidPassword as u8);
        stream.write_string("wrong password").unwrap();
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        assert_eq!(
            handle_db_reply(&mut read, 1000),
            DbReplyOutcome::Failure {
                status: LogOnStatus::LoginRejectedInvalidPassword as u8,
                message: Some("wrong password".to_string()),
                erase_cache: true,
            }
        );
    }

    #[test]
    fn a_failure_reply_without_a_description_uses_the_standard_text() {
        let mut stream = BinaryStream::new();
        stream.write_u8(LogOnStatus::LoginRejectedDbGeneralFailure as u8);
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        assert_eq!(
            handle_db_reply(&mut read, 1000),
            DbReplyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as u8,
                message: Some(DB_UNELABORATED_ERROR.to_string()),
                erase_cache: true,
            }
        );
    }

    #[test]
    fn a_ban_reply_parses_the_expiry_into_remaining_seconds() {
        let mut stream = BinaryStream::new();
        stream.write_u8(LogOnStatus::LoginRejectedIpAddressBan as u8);
        stream.write_string("3000").unwrap();
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        assert_eq!(
            handle_db_reply(&mut read, 1000),
            DbReplyOutcome::Ban { ban_seconds: 2000 }
        );
    }

    #[test]
    fn a_ban_reply_with_an_expired_ban_clamps_to_zero() {
        let mut stream = BinaryStream::new();
        stream.write_u8(LogOnStatus::LoginRejectedIpAddressBan as u8);
        stream.write_string("500").unwrap();
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        assert_eq!(
            handle_db_reply(&mut read, 1000),
            DbReplyOutcome::Ban { ban_seconds: 0 }
        );
    }

    #[test]
    fn a_malformed_ban_expiry_reports_the_bare_status_without_erasing() {
        for expiry in ["", "abc", "x9"] {
            let mut stream = BinaryStream::new();
            stream.write_u8(LogOnStatus::LoginRejectedIpAddressBan as u8);
            stream.write_string(expiry).unwrap();
            let mut read = BinaryStream::from_slice(stream.as_bytes());
            assert_eq!(
                handle_db_reply(&mut read, 1000),
                DbReplyOutcome::Failure {
                    status: LogOnStatus::LoginRejectedIpAddressBan as u8,
                    message: None,
                    erase_cache: false,
                },
                "expiry {expiry:?}"
            );
        }
    }

    #[test]
    fn a_short_success_reply_is_a_non_proxy_entity_error() {
        // An address (8 bytes) with no session key: the DBApp resolved an
        // entity type that is not a Proxy.
        let mut stream = BinaryStream::new();
        stream.write_u8(LogOnStatus::LoggedOn as u8);
        ServerAddress::new(0x0a00_0001, 20000, 1).write_to_stream(&mut stream);
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        assert_eq!(
            handle_db_reply(&mut read, 1000),
            DbReplyOutcome::Failure {
                status: LogOnStatus::LoginCustomDefinedError as u8,
                message: Some(DB_NON_PROXY_ERROR.to_string()),
                erase_cache: true,
            }
        );
    }

    #[test]
    fn a_truncated_success_reply_is_a_general_failure() {
        let mut stream = BinaryStream::new();
        stream.write_u8(LogOnStatus::LoggedOn as u8);
        stream.write_u32(0x0a00_0001);
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        assert_eq!(
            handle_db_reply(&mut read, 1000),
            DbReplyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as u8,
                message: Some(DB_UNKNOWN_ERROR.to_string()),
                erase_cache: true,
            }
        );
    }

    #[test]
    fn an_empty_reply_is_a_general_failure() {
        let mut read = BinaryStream::from_slice(&[]);
        assert_eq!(
            handle_db_reply(&mut read, 1000),
            DbReplyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as u8,
                message: Some(DB_UNKNOWN_ERROR.to_string()),
                erase_cache: true,
            }
        );
    }

    #[test]
    fn the_timeout_is_a_dbapp_overload_failure() {
        assert_eq!(
            db_reply_timeout(),
            DbReplyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbAppOverload as u8,
                message: Some(DB_NO_REPLY.to_string()),
                erase_cache: true,
            }
        );
    }

    #[test]
    fn parse_time_t_mirrors_the_cxx_stream_operator() {
        assert_eq!(parse_time_t("3000"), Some(3000));
        assert_eq!(parse_time_t("  -42xyz"), Some(-42));
        assert_eq!(parse_time_t("12abc"), Some(12));
        assert_eq!(parse_time_t(""), None);
        assert_eq!(parse_time_t("abc"), None);
        assert_eq!(parse_time_t("+7"), Some(7));
    }

    #[test]
    fn overload_accounting_records_cell_and_dbapp_always() {
        let mut o = SystemOverload::new();
        o.note_db_failure(LogOnStatus::LoginRejectedCellAppOverload as u8, 10);
        assert_eq!(o.status(), LogOnStatus::LoginRejectedCellAppOverload as u8);
        o.note_db_failure(LogOnStatus::LoginRejectedDbAppOverload as u8, 20);
        assert_eq!(o.status(), LogOnStatus::LoginRejectedDbAppOverload as u8);
    }

    #[test]
    fn overload_accounting_records_baseapp_only_when_clear() {
        let mut o = SystemOverload::new();
        // Already overloaded: a BaseApp overload does not override.
        o.note_db_failure(LogOnStatus::LoginRejectedDbAppOverload as u8, 10);
        o.note_db_failure(LogOnStatus::LoginRejectedBaseAppOverload as u8, 20);
        assert_eq!(o.status(), LogOnStatus::LoginRejectedDbAppOverload as u8);

        // Clear: a BaseApp overload now records.
        let mut o = SystemOverload::new();
        o.note_db_failure(LogOnStatus::LoginRejectedBaseAppOverload as u8, 10);
        assert_eq!(o.status(), LogOnStatus::LoginRejectedBaseAppOverload as u8);
    }

    #[test]
    fn the_overload_status_expires_one_second_after_it_was_recorded() {
        let now = 1_000_000_000u64; // 1s on the nanosecond stamp clock
        let mut o = SystemOverload::new();
        o.note_db_failure(LogOnStatus::LoginRejectedDbAppOverload as u8, now);
        assert_eq!(
            o.current(now),
            LogOnStatus::LoginRejectedDbAppOverload as u8
        );
        // Exactly one second later: still rejecting (strict `>` expiry).
        assert_eq!(
            o.current(now + 1_000_000_000),
            LogOnStatus::LoginRejectedDbAppOverload as u8
        );
        // One stamp past: cleared.
        assert_eq!(o.current(now + 1_000_000_001), 0);
        assert_eq!(o.status(), 0);
    }

    #[test]
    fn status_byte_reflects_each_outcome() {
        assert_eq!(
            db_reply_timeout().status_byte(),
            Some(LogOnStatus::LoginRejectedDbAppOverload as u8)
        );
        assert_eq!(
            DbReplyOutcome::Ban { ban_seconds: 1 }.status_byte(),
            Some(LogOnStatus::LoginRejectedIpAddressBan as u8)
        );
        assert_eq!(
            DbReplyOutcome::Success {
                record: LoginReplyRecord::new(ServerAddress::NONE, 0),
                server_msg: String::new(),
            }
            .status_byte(),
            Some(LogOnStatus::LoggedOn as u8)
        );
        assert_eq!(DbReplyOutcome::ShuttingDown.status_byte(), None);
    }
}
