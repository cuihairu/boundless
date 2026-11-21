//! BW-06-01-05 the login overload and failure gates.
//!
//! Before a login can be forwarded to the DBApp the reference `LoginApp::login`
//! runs a sequence of readiness and load gates
//! (`server/loginapp/loginapp.cpp`). The ones not covered by the earlier
//! BW-06-01 items (challenge, duplicate cache, DBApp forward) are the
//! allow-login, IP-ban, spoofed-address, rate-limit, DB-ready and
//! system-overload checks. Each rejects with the exact status the reference
//! `handleFailure` writes.
//!
//! The gates run in the reference order:
//!
//! ```text
//! begin_login (rate-limit window reset)
//!   -> allowLogin?        -> LOGIN_REJECTED_LOGINS_NOT_ALLOWED
//!   -> IP ban?            -> LOGIN_REJECTED_IP_ADDRESS_BAN
//!   -> periodic ban sweep
//!   -> empty source IP    -> dropped silently, no reply
//!   -> [version read + cache-pending gate: BW-06-01-01/03]
//!   -> rate limit?        -> LOGIN_REJECTED_RATE_LIMITED
//!   -> DBApp ready?       -> LOGIN_REJECTED_DB_NOT_READY
//!   -> system overloaded? -> the remembered overload status
//!   -> [challenge gate: BW-06-01-02]
//!   -> [params parse + cached gate: BW-06-01-01/03]
//!   -> [DBApp forward: BW-06-01-04]
//! ```
//!
//! The overload status is the BW-06-01-04 `SystemOverload` state; the DBApp
//! reply's overload failures set it, and `login()` reports it until it
//! expires. `write_failure` is the reply writer: the status byte and, when a
//! description is present, the message string — exactly `handleFailure`.
//! `write_success_reply` is the clear-text `sendSuccess` payload (the
//! encrypted form, chosen when the client sends an encryption key, is the
//! BW-06-03 connection-encryption item).

use std::collections::HashMap;

use foundation::stream::BinaryStream;

use crate::login_request::ClientLoginRequest;
use crate::loginapp::LogOnStatus;

/// `PACKET_MAX_SIZE`, the default `Config::maxLoginMessageSize`
/// (`loginapp_config.cpp`).
const DEFAULT_MAX_LOGIN_MESSAGE_SIZE: usize = 1472;

/// `Config::maxUsernameLength` default.
const DEFAULT_MAX_USERNAME_LENGTH: usize = 256;
/// `Config::maxPasswordLength` default.
const DEFAULT_MAX_PASSWORD_LENGTH: usize = 256;
/// `Config::ipBanListCleanupInterval` default (seconds).
const DEFAULT_IP_BAN_CLEANUP_INTERVAL: f64 = 10.0;

/// The login gate configuration, with the `LoginAppConfig` defaults
/// (`server/loginapp/loginapp_config.cpp`).
#[derive(Clone, Debug)]
pub struct LoginGateConfig {
    /// `Config::allowLogin`.
    pub allow_login: bool,
    /// `Config::allowUnencryptedLogins`.
    pub allow_unencrypted_logins: bool,
    /// `Config::verboseLoginFailures`.
    pub verbose_login_failures: bool,
    /// `Config::loginRateLimit`.
    pub login_rate_limit: u32,
    /// `Config::rateLimitDuration` (seconds).
    pub rate_limit_duration_seconds: f64,
    /// `Config::maxUsernameLength`.
    pub max_username_length: usize,
    /// `Config::maxPasswordLength`.
    pub max_password_length: usize,
    /// `Config::maxLoginMessageSize`.
    pub max_login_message_size: usize,
    /// `Config::ipBanListCleanupInterval` (seconds).
    pub ip_ban_cleanup_interval_seconds: f64,
}

impl LoginGateConfig {
    /// Defaults from `loginapp_config.cpp`.
    pub fn new() -> Self {
        Self {
            allow_login: false,
            allow_unencrypted_logins: false,
            verbose_login_failures: false,
            login_rate_limit: 0,
            rate_limit_duration_seconds: 0.0,
            max_username_length: DEFAULT_MAX_USERNAME_LENGTH,
            max_password_length: DEFAULT_MAX_PASSWORD_LENGTH,
            max_login_message_size: DEFAULT_MAX_LOGIN_MESSAGE_SIZE,
            ip_ban_cleanup_interval_seconds: DEFAULT_IP_BAN_CLEANUP_INTERVAL,
        }
    }

    /// `rateLimitEnabled()`: the reference enables rate limiting only when the
    /// duration is positive.
    pub fn rate_limit_enabled(&self) -> bool {
        self.rate_limit_duration_seconds > 0.0
    }

    /// `rateLimitDurationInStamps()`.
    pub fn rate_limit_duration_in_stamps(&self) -> u64 {
        (self.rate_limit_duration_seconds * 1_000_000_000.0) as u64
    }

    /// `ipBanListCleanupInterval()` in stamps.
    pub fn ip_ban_cleanup_interval_in_stamps(&self) -> u64 {
        (self.ip_ban_cleanup_interval_seconds * 1_000_000_000.0) as u64
    }
}

impl Default for LoginGateConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The outcome of a login gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginGate {
    /// The gate passes; the login continues.
    Proceed,
    /// The gate fails; the login is rejected with the status.
    Reject {
        /// The `LogOnStatus` value to write.
        status: u8,
    },
    /// The attempt is dropped without a reply (a spoofed empty address).
    DropSilently,
}

/// `LoginApp::numAllowedLoginsLeft_` / `lastRateLimitCheckTime_`: the login
/// rate-limit window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimiter {
    allowed_left: u32,
    last_check: u64,
}

impl RateLimiter {
    /// Constructor: a fresh window with the configured quota.
    pub fn new(login_rate_limit: u32) -> Self {
        Self {
            allowed_left: login_rate_limit,
            last_check: 0,
        }
    }

    /// `numAllowedLoginsLeft_`.
    pub fn allowed_left(&self) -> u32 {
        self.allowed_left
    }

    /// The top of `login()`: reset the window when it has elapsed.
    pub fn begin_window(&mut self, now: u64, config: &LoginGateConfig) {
        if config.rate_limit_enabled()
            && now
                > self
                    .last_check
                    .saturating_add(config.rate_limit_duration_in_stamps())
        {
            self.allowed_left = config.login_rate_limit;
            self.last_check = now;
        }
    }

    /// `isRateLimited`.
    pub fn is_rate_limited(&self, config: &LoginGateConfig) -> bool {
        config.rate_limit_enabled() && self.allowed_left == 0
    }

    /// `--numAllowedLoginsLeft_` after an attempt is forwarded to the DBApp.
    pub fn consume(&mut self, config: &LoginGateConfig) {
        if config.rate_limit_enabled() && self.allowed_left > 0 {
            self.allowed_left -= 1;
        }
    }
}

/// `LoginApp::ipAddressBanMap_`: IP addresses banned until a stamp, as the
/// DBApp ban reply and `handleBanIP` leave them.
#[derive(Default)]
pub struct IpBanMap {
    bans: HashMap<u32, u64>,
    next_cleanup: u64,
}

impl IpBanMap {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `handleBanIP`: ban the IP until `end_stamp`.
    pub fn ban(&mut self, ip: u32, end_stamp: u64) {
        self.bans.insert(ip, end_stamp);
    }

    /// `ipAddressBanMap_.find` in `login()`: a current ban rejects; an expired
    /// ban is dropped and passes.
    pub fn is_banned(&mut self, ip: u32, now: u64) -> bool {
        match self.bans.get(&ip) {
            Some(&end) if now <= end => true,
            Some(_) => {
                self.bans.remove(&ip);
                false
            }
            None => false,
        }
    }

    /// The periodic sweep in `login()`: erase every expired ban once the
    /// cleanup interval has elapsed since the last sweep.
    pub fn sweep_expired(&mut self, now: u64, interval_stamps: u64) {
        if now > self.next_cleanup {
            self.next_cleanup = now.saturating_add(interval_stamps);
            self.bans.retain(|_, end| *end >= now);
        }
    }

    /// Number of current bans.
    pub fn len(&self) -> usize {
        self.bans.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.bans.is_empty()
    }
}

/// `LoginApp::login` pre-version gates: `begin_window` (rate-limit reset), the
/// allow-login flag, the IP-ban check and sweep, and the spoofed-empty-address
/// drop. Runs before the client protocol version is read.
pub fn check_pre_version_gates(
    config: &LoginGateConfig,
    bans: &mut IpBanMap,
    source_ip: u32,
    now: u64,
) -> LoginGate {
    if !config.allow_login {
        return LoginGate::Reject {
            status: LogOnStatus::LoginRejectedLoginsNotAllowed as u8,
        };
    }
    if bans.is_banned(source_ip, now) {
        return LoginGate::Reject {
            status: LogOnStatus::LoginRejectedIpAddressBan as u8,
        };
    }
    bans.sweep_expired(now, config.ip_ban_cleanup_interval_in_stamps());
    if source_ip == 0 {
        // A spoofed empty address: dropped without a reply.
        return LoginGate::DropSilently;
    }
    LoginGate::Proceed
}

/// `LoginApp::login` post-version gates: the rate limit, the DBApp-ready
/// check, and the remembered overload status. Runs after the version read and
/// the cached-pending gate, before the challenge gate.
pub fn check_post_version_gates(
    config: &LoginGateConfig,
    limiter: &mut RateLimiter,
    db_ready: bool,
    overload: &mut crate::db_login::SystemOverload,
    now: u64,
) -> LoginGate {
    if limiter.is_rate_limited(config) {
        return LoginGate::Reject {
            status: LogOnStatus::LoginRejectedRateLimited as u8,
        };
    }
    if !db_ready {
        return LoginGate::Reject {
            status: LogOnStatus::LoginRejectedDbNotReady as u8,
        };
    }
    let status = overload.current(now);
    if status != 0 {
        return LoginGate::Reject { status };
    }
    LoginGate::Proceed
}

/// `handleFailure` reply (`loginapp.cpp`): the status byte and, when a
/// description is present, the message string. Replies to failed logins are
/// sent unreliable (`RELIABLE_NO`) — the reliability is a Mercury concern.
pub fn write_failure(stream: &mut BinaryStream, status: u8, message: Option<&str>) {
    stream.write_u8(status);
    if let Some(message) = message {
        let _ = stream.write_string(message);
    }
}

/// `sendSuccess` clear-text payload (`loginapp.cpp`): the status byte, then
/// the cached reply record and server message. The reference writes the
/// `LoginReplyRecord` through the client's encryption filter when the logon
/// params carry an encryption key; that encrypted form is the BW-06-03
/// connection-encryption item, so this is the clear-text path.
pub fn write_success_reply(stream: &mut BinaryStream, request: &ClientLoginRequest) {
    stream.write_u8(LogOnStatus::LoggedOn as u8);
    request.write_success_result_to_stream(stream);
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000_000; // 1s on the nanosecond clock

    fn default_config() -> LoginGateConfig {
        LoginGateConfig::new()
    }

    #[test]
    fn config_defaults_match_the_reference() {
        let c = LoginGateConfig::new();
        assert!(!c.allow_login);
        assert!(!c.allow_unencrypted_logins);
        assert_eq!(c.login_rate_limit, 0);
        assert!(!c.rate_limit_enabled());
        assert_eq!(c.max_username_length, 256);
        assert_eq!(c.max_password_length, 256);
        assert_eq!(c.max_login_message_size, 1472);
    }

    #[test]
    fn allow_login_disabled_rejects_every_attempt() {
        let mut bans = IpBanMap::new();
        assert_eq!(
            check_pre_version_gates(&default_config(), &mut bans, 0x0100_0007, NOW),
            LoginGate::Reject {
                status: LogOnStatus::LoginRejectedLoginsNotAllowed as u8,
            }
        );
    }

    #[test]
    fn an_unbanned_ip_passes_when_logins_are_allowed() {
        let mut config = default_config();
        config.allow_login = true;
        let mut bans = IpBanMap::new();
        assert_eq!(
            check_pre_version_gates(&config, &mut bans, 0x0100_0007, NOW),
            LoginGate::Proceed
        );
    }

    #[test]
    fn a_current_ban_rejects_with_the_ip_ban_status() {
        let mut config = default_config();
        config.allow_login = true;
        let mut bans = IpBanMap::new();
        bans.ban(0x0100_0007, NOW + 5_000_000_000);
        assert_eq!(
            check_pre_version_gates(&config, &mut bans, 0x0100_0007, NOW),
            LoginGate::Reject {
                status: LogOnStatus::LoginRejectedIpAddressBan as u8,
            }
        );
    }

    #[test]
    fn an_expired_ban_passes_and_is_dropped() {
        let mut config = default_config();
        config.allow_login = true;
        let mut bans = IpBanMap::new();
        bans.ban(0x0100_0007, NOW); // expired at `now`
        assert_eq!(
            check_pre_version_gates(&config, &mut bans, 0x0100_0007, NOW + 1),
            LoginGate::Proceed
        );
        assert!(!bans.is_banned(0x0100_0007, NOW + 1));
    }

    #[test]
    fn a_spoofed_empty_address_is_dropped_silently() {
        let mut config = default_config();
        config.allow_login = true;
        let mut bans = IpBanMap::new();
        assert_eq!(
            check_pre_version_gates(&config, &mut bans, 0, NOW),
            LoginGate::DropSilently
        );
    }

    #[test]
    fn the_ban_sweep_erases_expired_entries_on_the_interval() {
        let mut config = default_config();
        config.allow_login = true;
        let mut bans = IpBanMap::new();
        bans.ban(0x0a00_0001, NOW + 100_000_000_000); // still current
        bans.ban(0x0a00_0002, NOW - 100); // expired long ago
        // First sweep: only expired entries go.
        bans.sweep_expired(
            NOW + config.ip_ban_cleanup_interval_in_stamps() + 1,
            config.ip_ban_cleanup_interval_in_stamps(),
        );
        assert_eq!(bans.len(), 1);
        // A second sweep before the interval elapses does nothing.
        bans.sweep_expired(
            NOW + config.ip_ban_cleanup_interval_in_stamps() + 2,
            config.ip_ban_cleanup_interval_in_stamps(),
        );
        assert_eq!(bans.len(), 1);
    }

    #[test]
    fn rate_limit_is_disabled_by_default() {
        let mut limiter = RateLimiter::new(0);
        let config = default_config();
        limiter.begin_window(NOW, &config);
        assert!(!limiter.is_rate_limited(&config));
        limiter.consume(&config);
        assert_eq!(limiter.allowed_left(), 0);
    }

    #[test]
    fn rate_limit_resets_the_window_and_rejects_when_exhausted() {
        let mut config = default_config();
        config.login_rate_limit = 2;
        config.rate_limit_duration_seconds = 1.0;
        assert!(config.rate_limit_enabled());

        let mut limiter = RateLimiter::new(config.login_rate_limit);
        limiter.begin_window(NOW, &config);
        assert!(!limiter.is_rate_limited(&config));
        limiter.consume(&config);
        limiter.consume(&config);
        assert_eq!(limiter.allowed_left(), 0);
        assert!(limiter.is_rate_limited(&config));

        // The window resets after the duration.
        limiter.begin_window(NOW + 2_000_000_000, &config);
        assert!(!limiter.is_rate_limited(&config));
        assert_eq!(limiter.allowed_left(), 2);
    }

    #[test]
    fn db_not_ready_rejects_with_the_db_not_ready_status() {
        let mut limiter = RateLimiter::new(0);
        let mut overload = crate::db_login::SystemOverload::new();
        assert_eq!(
            check_post_version_gates(&default_config(), &mut limiter, false, &mut overload, NOW),
            LoginGate::Reject {
                status: LogOnStatus::LoginRejectedDbNotReady as u8,
            }
        );
    }

    #[test]
    fn an_active_overload_rejects_with_the_remembered_status() {
        let mut limiter = RateLimiter::new(0);
        let mut overload = crate::db_login::SystemOverload::new();
        overload.note_db_failure(LogOnStatus::LoginRejectedDbAppOverload as u8, NOW);
        assert_eq!(
            check_post_version_gates(&default_config(), &mut limiter, true, &mut overload, NOW),
            LoginGate::Reject {
                status: LogOnStatus::LoginRejectedDbAppOverload as u8,
            }
        );
    }

    #[test]
    fn ready_and_not_overloaded_passes() {
        let mut limiter = RateLimiter::new(0);
        let mut overload = crate::db_login::SystemOverload::new();
        assert_eq!(
            check_post_version_gates(&default_config(), &mut limiter, true, &mut overload, NOW),
            LoginGate::Proceed
        );
    }

    #[test]
    fn write_failure_streams_the_status_then_the_message() {
        let mut stream = BinaryStream::new();
        write_failure(
            &mut stream,
            LogOnStatus::LoginRejectedInvalidPassword as u8,
            Some("bad"),
        );
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        assert_eq!(
            read.read_u8().unwrap(),
            LogOnStatus::LoginRejectedInvalidPassword as u8
        );
        assert_eq!(read.read_string().unwrap(), "bad");
        assert!(read.at_end());
    }

    #[test]
    fn write_failure_without_a_message_writes_only_the_status() {
        let mut stream = BinaryStream::new();
        write_failure(
            &mut stream,
            LogOnStatus::LoginRejectedRateLimited as u8,
            None,
        );
        assert_eq!(
            stream.as_bytes(),
            &[LogOnStatus::LoginRejectedRateLimited as u8]
        );
    }
}
