//! BW-06-01 the LoginApp public login entry.
//!
//! The reference reads a login request in two steps
//! (`server/loginapp/loginapp.cpp`): first the `ClientServerProtocolVersion`,
//! then — after the re-attempt, rate-limit, readiness and challenge gates —
//! the `LogOnParams`. Both are wire types from `lib/connection/`, so this
//! module owns them, together with the `LogOnStatus` codes the accept/reject
//! reply is written with (`lib/connection/log_on_status.hpp`).
//!
//! The version is read first and compared before the params are touched, so a
//! request with a bad version is rejected without parsing its credentials.
//! The gates that sit between the two reads (re-attempt, rate limit, DB
//! readiness, overload, login challenge) are the later BW-06-01-02/04/05
//! items; this module only decides the version-and-params accept/reject.

use foundation::stream::BinaryStream;

/// `ClientServerProtocolVersion` current version, `lib/connection/
/// client_server_protocol_version.hpp`.
pub const CLIENT_SERVER_PROTOCOL_VERSION_MAJOR: u8 = 2;
pub const CLIENT_SERVER_PROTOCOL_VERSION_MINOR: u8 = 9;
pub const CLIENT_SERVER_PROTOCOL_VERSION_PATCH: u8 = 0;
pub const CLIENT_SERVER_PROTOCOL_VERSION_SUBPATCH: u8 = 0;

/// `Config::maxLoginMessageSize` default: `PACKET_MAX_SIZE`
/// (`server/loginapp/loginapp_config.cpp`).
pub const MAX_LOGIN_MESSAGE_SIZE: usize = 1472;

/// `LogOnParams::HAS_DIGEST` / `HAS_ALL` and the `PASS_THRU` sentinel
/// (`lib/connection/log_on_params.hpp`).
pub const LOG_ON_PARAMS_HAS_DIGEST: u8 = 0x1;
pub const LOG_ON_PARAMS_HAS_ALL: u8 = 0x1;
pub const LOG_ON_PARAMS_PASS_THRU: u8 = 0xff;

/// `lib/connection/client_server_protocol_version.hpp`.
///
/// The client-server protocol version is four bytes streamed in the order
/// subpatch, patch, minor, major — the reverse of the natural display order.
/// The reference accepts a client version only when it is exactly equal to
/// the server's own (`supports`), so the version is really a wire tag, not a
/// range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientServerProtocolVersion {
    /// `major_`.
    pub major: u8,
    /// `minor_`.
    pub minor: u8,
    /// `patch_`.
    pub patch: u8,
    /// `subpatch_`.
    pub subpatch: u8,
}

impl ClientServerProtocolVersion {
    /// Constructor.
    pub fn new(major: u8, minor: u8, patch: u8, subpatch: u8) -> Self {
        Self {
            major,
            minor,
            patch,
            subpatch,
        }
    }

    /// The version this server speaks.
    pub fn current() -> Self {
        Self::new(
            CLIENT_SERVER_PROTOCOL_VERSION_MAJOR,
            CLIENT_SERVER_PROTOCOL_VERSION_MINOR,
            CLIENT_SERVER_PROTOCOL_VERSION_PATCH,
            CLIENT_SERVER_PROTOCOL_VERSION_SUBPATCH,
        )
    }

    /// `streamSize`: major, minor, patch, subpatch are all `uint8`.
    pub fn stream_size() -> usize {
        4
    }

    /// `readFromStream`: `stream >> subpatch_ >> patch_ >> minor_ >> major_`.
    /// Returns false when the stream runs out.
    pub fn read_from_stream(&mut self, stream: &mut BinaryStream) -> bool {
        let (Ok(subpatch), Ok(patch), Ok(minor), Ok(major)) = (
            stream.read_u8(),
            stream.read_u8(),
            stream.read_u8(),
            stream.read_u8(),
        ) else {
            return false;
        };
        self.subpatch = subpatch;
        self.patch = patch;
        self.minor = minor;
        self.major = major;
        true
    }

    /// `writeToStream`: `stream << subpatch_ << patch_ << minor_ << major_`.
    pub fn write_to_stream(&self, stream: &mut BinaryStream) {
        stream.write_u8(self.subpatch);
        stream.write_u8(self.patch);
        stream.write_u8(self.minor);
        stream.write_u8(self.major);
    }

    /// `supports`: true only when the given version is exactly this version.
    pub fn supports(&self, other: &ClientServerProtocolVersion) -> bool {
        self.major == other.major
            && self.minor == other.minor
            && self.patch == other.patch
            && self.subpatch == other.subpatch
    }

    /// `c_str`: `"major.minor.patch"`, or `"major.minor.patch (subpatch)"`
    /// when the subpatch is non-zero.
    pub fn c_str(&self) -> String {
        if self.subpatch == 0 {
            format!("{}.{}.{}", self.major, self.minor, self.patch)
        } else {
            format!(
                "{}.{}.{} ({})",
                self.major, self.minor, self.patch, self.subpatch
            )
        }
    }
}

impl Default for ClientServerProtocolVersion {
    /// Empty constructor: all fields zero.
    fn default() -> Self {
        Self::new(0, 0, 0, 0)
    }
}

/// `lib/connection/log_on_params.hpp`: the parameters the client sends at
/// login, forwarded loginapp -> dbapp -> baseapp.
///
/// The wire order is `flags`, username, password, encryptionKey, the digest
/// when `flags & HAS_DIGEST`, then the nonce. Strings use the packed-int
/// length prefix of `BW::string`; the digest is a raw 16 bytes. The reference
/// keeps the digest as a cleared (zeroed) field, so a default `HAS_ALL`
/// params streams a zeroed digest: `HAS_ALL` *is* `HAS_DIGEST`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogOnParams {
    /// `flags_`.
    flags: u8,
    /// `username_`.
    username: String,
    /// `password_`.
    password: String,
    /// `encryptionKey_`.
    encryption_key: String,
    /// `digest_`, cleared to zero by the constructors.
    digest: [u8; 16],
    /// `nonce_`.
    nonce: u32,
}

impl LogOnParams {
    /// Default constructor: `flags_ = HAS_ALL`, `digest_.clear()`. The
    /// reference seeds the nonce from `std::rand()`; the port uses a
    /// deterministic zero and lets the caller set it, so tests can reproduce
    /// a wire sample.
    pub fn new() -> Self {
        Self {
            flags: LOG_ON_PARAMS_HAS_ALL,
            username: String::new(),
            password: String::new(),
            encryption_key: String::new(),
            digest: [0; 16],
            nonce: 0,
        }
    }

    /// Constructor with credentials.
    pub fn with_credentials(username: &str, password: &str, encryption_key: &str) -> Self {
        Self {
            flags: LOG_ON_PARAMS_HAS_ALL,
            username: username.to_string(),
            password: password.to_string(),
            encryption_key: encryption_key.to_string(),
            digest: [0; 16],
            nonce: 0,
        }
    }

    /// `flags()`.
    pub fn flags(&self) -> u8 {
        self.flags
    }

    /// `username()`.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// `password()`.
    pub fn password(&self) -> &str {
        &self.password
    }

    /// `encryptionKey()`.
    pub fn encryption_key(&self) -> &str {
        &self.encryption_key
    }

    /// `digest()`.
    pub fn digest(&self) -> &[u8; 16] {
        &self.digest
    }

    /// `nonce()`.
    pub fn nonce(&self) -> u32 {
        self.nonce
    }

    /// Set the `nonce_`.
    pub fn set_nonce(&mut self, nonce: u32) {
        self.nonce = nonce;
    }

    /// Set the `digest_`. The reference setter does not touch `flags_`; the
    /// caller decides via the streaming flags whether it goes on the wire.
    pub fn set_digest(&mut self, digest: [u8; 16]) {
        self.digest = digest;
    }

    /// `readFromStreamInternal`: read the fields back, returning false when
    /// the stream runs out (which the reference reports as `!data.error()`).
    /// When the wire flags lack `HAS_DIGEST` the stored digest is untouched.
    pub fn read_from_stream(&mut self, stream: &mut BinaryStream) -> bool {
        let Ok(flags) = stream.read_u8() else {
            return false;
        };
        let Ok(username) = stream.read_string() else {
            return false;
        };
        let Ok(password) = stream.read_string() else {
            return false;
        };
        let Ok(encryption_key) = stream.read_string() else {
            return false;
        };
        self.flags = flags;
        self.username = username;
        self.password = password;
        self.encryption_key = encryption_key;
        if flags & LOG_ON_PARAMS_HAS_DIGEST != 0 {
            let Ok(bytes) = stream.read_blob_raw(16) else {
                return false;
            };
            self.digest.copy_from_slice(&bytes);
        }
        let Ok(nonce) = stream.read_u32() else {
            return false;
        };
        self.nonce = nonce;
        true
    }

    /// `addToStreamInternal`: write the fields back. `PASS_THRU` means "use
    /// the stored flags"; any other value streams that subset of fields, as
    /// the reference's flags argument does.
    pub fn add_to_stream(&self, stream: &mut BinaryStream, flags: u8) {
        let flags = if flags == LOG_ON_PARAMS_PASS_THRU {
            self.flags
        } else {
            flags
        };
        stream.write_u8(flags);
        let _ = stream.write_string(&self.username);
        let _ = stream.write_string(&self.password);
        let _ = stream.write_string(&self.encryption_key);
        if flags & LOG_ON_PARAMS_HAS_DIGEST != 0 {
            stream.write_blob_raw(&self.digest);
        }
        stream.write_u32(self.nonce);
    }

    /// `operator==`: the reference compares the security-relevant fields.
    pub fn same_security(&self, other: &LogOnParams) -> bool {
        self.username == other.username
            && self.password == other.password
            && self.encryption_key == other.encryption_key
            && self.nonce == other.nonce
    }
}

impl Default for LogOnParams {
    fn default() -> Self {
        Self::new()
    }
}

/// `lib/connection/log_on_status.hpp`: the wire status a login attempt is
/// answered with. The client decodes these numbers directly, so the
/// discriminants are part of the protocol and are pinned to the reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LogOnStatus {
    /// `NOT_SET`.
    NotSet = 0,
    /// `LOGGED_ON`.
    LoggedOn = 1,
    /// `LOGGED_ON_OFFLINE`.
    LoggedOnOffline = 2,
    /// `CONNECTION_FAILED`.
    ConnectionFailed = 3,
    /// `DNS_LOOKUP_FAILED`.
    DnsLookupFailed = 4,
    /// `UNKNOWN_ERROR`.
    UnknownError = 5,
    /// `CANCELLED`.
    Cancelled = 6,
    /// `ALREADY_ONLINE_LOCALLY`.
    AlreadyOnlineLocally = 7,
    /// `PUBLIC_KEY_LOOKUP_FAILED`.
    PublicKeyLookupFailed = 8,
    /// `LAST_CLIENT_SIDE_VALUE`.
    LastClientSideValue = 63,
    /// `LOGIN_MALFORMED_REQUEST`.
    LoginMalformedRequest = 64,
    /// `LOGIN_BAD_PROTOCOL_VERSION`.
    LoginBadProtocolVersion = 65,
    /// `LOGIN_CHALLENGE_ISSUED`.
    LoginChallengeIssued = 66,
    /// `LOGIN_REJECTED_NO_SUCH_USER`.
    LoginRejectedNoSuchUser = 67,
    /// `LOGIN_REJECTED_INVALID_PASSWORD`.
    LoginRejectedInvalidPassword = 68,
    /// `LOGIN_REJECTED_ALREADY_LOGGED_IN`.
    LoginRejectedAlreadyLoggedIn = 69,
    /// `LOGIN_REJECTED_BAD_DIGEST`.
    LoginRejectedBadDigest = 70,
    /// `LOGIN_REJECTED_DB_GENERAL_FAILURE`.
    LoginRejectedDbGeneralFailure = 71,
    /// `LOGIN_REJECTED_DB_NOT_READY`.
    LoginRejectedDbNotReady = 72,
    /// `LOGIN_REJECTED_ILLEGAL_CHARACTERS`.
    LoginRejectedIllegalCharacters = 73,
    /// `LOGIN_REJECTED_SERVER_NOT_READY`.
    LoginRejectedServerNotReady = 74,
    /// `LOGIN_REJECTED_UPDATER_NOT_READY` (no longer used).
    LoginRejectedUpdaterNotReady = 75,
    /// `LOGIN_REJECTED_NO_BASEAPPS`.
    LoginRejectedNoBaseApps = 76,
    /// `LOGIN_REJECTED_BASEAPP_OVERLOAD`.
    LoginRejectedBaseAppOverload = 77,
    /// `LOGIN_REJECTED_CELLAPP_OVERLOAD`.
    LoginRejectedCellAppOverload = 78,
    /// `LOGIN_REJECTED_BASEAPP_TIMEOUT`.
    LoginRejectedBaseAppTimeout = 79,
    /// `LOGIN_REJECTED_BASEAPPMGR_TIMEOUT`.
    LoginRejectedBaseAppMgrTimeout = 80,
    /// `LOGIN_REJECTED_DBAPP_OVERLOAD`.
    LoginRejectedDbAppOverload = 81,
    /// `LOGIN_REJECTED_LOGINS_NOT_ALLOWED`.
    LoginRejectedLoginsNotAllowed = 82,
    /// `LOGIN_REJECTED_RATE_LIMITED`.
    LoginRejectedRateLimited = 83,
    /// `LOGIN_REJECTED_BAN`.
    LoginRejectedBan = 84,
    /// `LOGIN_REJECTED_CHALLENGE_ERROR`.
    LoginRejectedChallengeError = 85,
    /// `LOGIN_REJECTED_AUTH_SERVICE_NO_SUCH_ACCOUNT`.
    LoginRejectedAuthServiceNoSuchAccount = 86,
    /// `LOGIN_REJECTED_AUTH_SERVICE_LOGIN_DISALLOWED`.
    LoginRejectedAuthServiceLoginDisallowed = 87,
    /// `LOGIN_REJECTED_AUTH_SERVICE_UNREACHABLE`.
    LoginRejectedAuthServiceUnreachable = 88,
    /// `LOGIN_REJECTED_AUTH_SERVICE_INVALID_RESPONSE`.
    LoginRejectedAuthServiceInvalidResponse = 89,
    /// `LOGIN_REJECTED_AUTH_SERVICE_GENERAL_FAILURE`.
    LoginRejectedAuthServiceGeneralFailure = 90,
    /// `LOGIN_REJECTED_NO_LOGINAPP`.
    LoginRejectedNoLoginApp = 91,
    /// `LOGIN_REJECTED_NO_LOGINAPP_RESPONSE`.
    LoginRejectedNoLoginAppResponse = 92,
    /// `LOGIN_REJECTED_NO_BASEAPP_RESPONSE`.
    LoginRejectedNoBaseAppResponse = 93,
    /// `LOGIN_REJECTED_IP_ADDRESS_BAN`.
    LoginRejectedIpAddressBan = 244,
    /// `LOGIN_REJECTED_INACCESSIBLE_REALM`.
    LoginRejectedInaccessibleRealm = 245,
    /// `LOGIN_REJECTED_REGISTRATION_NOT_ALLOWED`.
    LoginRejectedRegistrationNotAllowed = 246,
    /// `LOGIN_REJECTED_REGISTRATION_NOT_CONFIRMED`.
    LoginRejectedRegistrationNotConfirmed = 247,
    /// `LOGIN_REJECTED_NOT_REGISTERED`.
    LoginRejectedNotRegistered = 248,
    /// `LOGIN_REJECTED_ACTIVATING`.
    LoginRejectedActivating = 249,
    /// `LOGIN_REJECTED_UNABLE_TO_PARSE_JSON`.
    LoginRejectedUnableToParseJson = 250,
    /// `LOGIN_REJECTED_USERS_LIMIT`.
    LoginRejectedUsersLimit = 251,
    /// `LOGIN_REJECTED_LOGIN_QUEUE`.
    LoginRejectedLoginQueue = 252,
    /// `LOGIN_CUSTOM_DEFINED_ERROR`.
    LoginCustomDefinedError = 254,
    /// `LAST_SERVER_SIDE_VALUE`.
    LastServerSideValue = 255,
}

impl LogOnStatus {
    /// The `uint8` wire value.
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// `LoginApp::login` result for the version-and-params parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginParseOutcome {
    /// The request parsed and the protocol version is supported.
    Accepted {
        /// The logon parameters, ready for the later billing and dispatch.
        params: LogOnParams,
    },
    /// `LOGIN_MALFORMED_REQUEST`.
    MalformedRequest,
    /// `LOGIN_BAD_PROTOCOL_VERSION`.
    BadProtocolVersion,
}

/// `LoginApp::login` first steps: read the version, reject a mismatched one,
/// then read the logon params. `max_login_message_size` is the bound the
/// reference applies to the credentials blob (`Config::maxLoginMessageSize`).
pub fn parse_login_request(
    stream: &mut BinaryStream,
    max_login_message_size: usize,
) -> LoginParseOutcome {
    let mut version = ClientServerProtocolVersion::default();
    if !version.read_from_stream(stream) {
        return LoginParseOutcome::MalformedRequest;
    }
    if !ClientServerProtocolVersion::current().supports(&version) {
        return LoginParseOutcome::BadProtocolVersion;
    }
    if stream.remaining() > max_login_message_size {
        return LoginParseOutcome::MalformedRequest;
    }
    let mut params = LogOnParams::new();
    if !params.read_from_stream(stream) {
        return LoginParseOutcome::MalformedRequest;
    }
    LoginParseOutcome::Accepted { params }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_bytes(params: &LogOnParams) -> Vec<u8> {
        let mut s = BinaryStream::new();
        params.add_to_stream(&mut s, LOG_ON_PARAMS_PASS_THRU);
        s.as_bytes().to_vec()
    }

    fn request_bytes(version: &ClientServerProtocolVersion, params: &LogOnParams) -> Vec<u8> {
        let mut s = BinaryStream::new();
        version.write_to_stream(&mut s);
        s.write_bytes(&params_bytes(params));
        s.as_bytes().to_vec()
    }

    fn parse(bytes: &[u8]) -> LoginParseOutcome {
        parse_login_request(&mut BinaryStream::from_slice(bytes), MAX_LOGIN_MESSAGE_SIZE)
    }

    #[test]
    fn current_version_roundtrips_in_wire_order() {
        let v = ClientServerProtocolVersion::current();
        let mut s = BinaryStream::new();
        v.write_to_stream(&mut s);
        // Wire order is subpatch, patch, minor, major.
        assert_eq!(s.as_bytes(), &[0, 0, 9, 2]);

        let mut back = ClientServerProtocolVersion::default();
        assert!(back.read_from_stream(&mut s));
        assert_eq!(back, v);
    }

    #[test]
    fn current_version_c_str_drops_zero_subpatch() {
        assert_eq!(ClientServerProtocolVersion::current().c_str(), "2.9.0");
        assert_eq!(
            ClientServerProtocolVersion::new(2, 2, 255, 5).c_str(),
            "2.2.255 (5)"
        );
    }

    #[test]
    fn supports_is_exact_equality() {
        let cur = ClientServerProtocolVersion::current();
        assert!(cur.supports(&cur));
        assert!(!cur.supports(&ClientServerProtocolVersion::new(2, 9, 0, 1)));
        assert!(!cur.supports(&ClientServerProtocolVersion::new(2, 8, 0, 0)));
        assert!(!cur.supports(&ClientServerProtocolVersion::new(0, 0, 0, 0)));
    }

    #[test]
    fn a_matching_version_and_params_are_accepted() {
        let params = LogOnParams::with_credentials("alice", "s3cret", "key");
        let bytes = request_bytes(&ClientServerProtocolVersion::current(), &params);
        match parse(&bytes) {
            LoginParseOutcome::Accepted { params: got } => {
                assert_eq!(got, params);
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn a_different_version_is_rejected_before_the_params() {
        let params = LogOnParams::with_credentials("alice", "s3cret", "key");
        for version in [
            ClientServerProtocolVersion::new(2, 9, 0, 1),
            ClientServerProtocolVersion::new(2, 8, 0, 0),
            ClientServerProtocolVersion::new(0, 0, 0, 0),
        ] {
            let bytes = request_bytes(&version, &params);
            assert_eq!(parse(&bytes), LoginParseOutcome::BadProtocolVersion);
        }
    }

    #[test]
    fn a_truncated_version_is_malformed() {
        assert_eq!(parse(&[0, 0]), LoginParseOutcome::MalformedRequest);
        assert_eq!(parse(&[]), LoginParseOutcome::MalformedRequest);
    }

    #[test]
    fn the_digest_is_present_only_with_has_digest() {
        // Default HAS_ALL flags stream a cleared (zeroed) digest and read it
        // back.
        let params = LogOnParams::with_credentials("alice", "s3cret", "key");
        let bytes = request_bytes(&ClientServerProtocolVersion::current(), &params);
        match parse(&bytes) {
            LoginParseOutcome::Accepted { params: got } => {
                assert_eq!(got.digest(), &[0; 16]);
                assert_eq!(
                    got.flags() & LOG_ON_PARAMS_HAS_DIGEST,
                    LOG_ON_PARAMS_HAS_DIGEST
                );
            }
            other => panic!("expected accept, got {other:?}"),
        }

        // A non-default digest and nonce round-trip.
        let mut with_digest = LogOnParams::with_credentials("alice", "s3cret", "key");
        with_digest.set_digest([0xAB; 16]);
        with_digest.set_nonce(7);
        let bytes = request_bytes(&ClientServerProtocolVersion::current(), &with_digest);
        match parse(&bytes) {
            LoginParseOutcome::Accepted { params: got } => {
                assert_eq!(got.digest(), &[0xAB; 16]);
                assert_eq!(got.nonce(), 7);
            }
            other => panic!("expected accept, got {other:?}"),
        }

        // Streaming with flags that clear HAS_DIGEST leaves the digest off
        // the wire; the read side keeps its cleared stored digest.
        let mut s = BinaryStream::new();
        ClientServerProtocolVersion::current().write_to_stream(&mut s);
        with_digest.add_to_stream(&mut s, 0);
        match parse_login_request(&mut s, MAX_LOGIN_MESSAGE_SIZE) {
            LoginParseOutcome::Accepted { params: got } => {
                assert_eq!(got.flags() & LOG_ON_PARAMS_HAS_DIGEST, 0);
                assert_eq!(got.digest(), &[0; 16]);
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn a_truncated_params_blob_is_malformed() {
        let mut bytes = request_bytes(
            &ClientServerProtocolVersion::current(),
            &LogOnParams::with_credentials("alice", "s3cret", "key"),
        );
        bytes.truncate(bytes.len() - 1);
        assert_eq!(parse(&bytes), LoginParseOutcome::MalformedRequest);
    }

    #[test]
    fn an_overlong_params_blob_is_malformed() {
        // The version, then a username blob larger than the bound.
        let mut s = BinaryStream::new();
        ClientServerProtocolVersion::current().write_to_stream(&mut s);
        let _ = s.write_string(&"x".repeat(MAX_LOGIN_MESSAGE_SIZE + 1));
        assert_eq!(
            parse_login_request(&mut s, MAX_LOGIN_MESSAGE_SIZE),
            LoginParseOutcome::MalformedRequest
        );
    }

    #[test]
    fn same_security_ignores_the_digest() {
        let mut a = LogOnParams::with_credentials("alice", "s3cret", "key");
        let mut b = LogOnParams::with_credentials("alice", "s3cret", "key");
        a.set_nonce(9);
        b.set_nonce(9);
        a.set_digest([1; 16]);
        assert!(a.same_security(&b));

        let mut c = LogOnParams::with_credentials("alice", "wrong", "key");
        c.set_nonce(9);
        assert!(!a.same_security(&c));
    }

    #[test]
    fn status_wire_values_match_the_reference() {
        assert_eq!(LogOnStatus::LoginMalformedRequest.as_u8(), 64);
        assert_eq!(LogOnStatus::LoginBadProtocolVersion.as_u8(), 65);
        assert_eq!(LogOnStatus::LoginChallengeIssued.as_u8(), 66);
        assert_eq!(LogOnStatus::LoginRejectedLoginsNotAllowed.as_u8(), 82);
        assert_eq!(LogOnStatus::LoginRejectedRateLimited.as_u8(), 83);
        assert_eq!(LogOnStatus::LoginRejectedDbNotReady.as_u8(), 72);
        assert_eq!(LogOnStatus::LoginRejectedIpAddressBan.as_u8(), 244);
        assert_eq!(LogOnStatus::LastServerSideValue.as_u8(), 255);
    }
}
