//! BW-06-01-03 the cached per-address login request.
//!
//! The reference LoginApp keeps one `ClientLoginRequest` per client address
//! (`server/loginapp/client_login_request.{hpp,cpp}`) and answers duplicate
//! attempts from it. A client re-sends a login when its reply was dropped; the
//! server must either ignore the attempt while a DBApp login is still in
//! flight, or re-send the cached success once the reply is known. The two
//! paths are:
//!
//! * `LoginApp::handleResentPendingAttempt` (`loginapp.cpp`) — run before the
//!   logon params are parsed. A request with an outstanding challenge has the
//!   challenge re-sent; a request still waiting on the DBApp
//!   (`isPendingAuthentication`) is ignored.
//! * `LoginApp::handleResentCachedAttempt` (`loginapp.cpp`) — run after the
//!   params are parsed. A request that is not too old and whose params match
//!   has its cached success re-sent.
//!
//! The cached reply is the `LoginReplyRecord` wire type
//! (`lib/connection/login_reply_record.hpp`): the entity-server address the
//! client must connect to, and the session key to present.
//!
//! Time is the port's stamp convention — monotonic nanoseconds from
//! `foundation::time::stamp_now`, with `STAMPS_PER_SECOND` 1e9 — so the
//! "too old" window mirrors `TimeStamp::fromSeconds(Config::maxLoginDelay())`.
//! The decision functions take `now` explicitly so tests can drive a virtual
//! clock; `ClientLoginRequest::set_data` records the reply against a caller
//! supplied stamp the same way.

use std::collections::HashMap;

use foundation::stream::BinaryStream;

use crate::login_challenge::{LoginChallenge, LoginChallengeState};
use crate::loginapp::LogOnParams;

/// `STAMPS_PER_SECOND` in the port's stamp convention (`foundation::time`).
const STAMPS_PER_SECOND: u64 = 1_000_000_000;

/// `Config::maxLoginDelay` default (`server/loginapp/loginapp_config.cpp`):
/// the number of seconds a cached success is kept for a re-send.
const MAX_LOGIN_DELAY_SECONDS: f64 = 10.0;

/// `sendAndCacheSuccess` (`loginapp.cpp`): prune the cache once it outgrows
/// this bound.
const MAX_CACHED_REQUESTS: usize = 100;

/// `Config::maxLoginDelay()`.
pub fn max_login_delay_seconds() -> f64 {
    MAX_LOGIN_DELAY_SECONDS
}

/// `LoginAppConfig::maxLoginDelayInStamps()`:
/// `TimeStamp::fromSeconds(maxLoginDelay())`.
pub fn max_login_delay_in_stamps() -> u64 {
    (MAX_LOGIN_DELAY_SECONDS * STAMPS_PER_SECOND as f64) as u64
}

/// `Mercury::Address` (`lib/network/basictypes.hpp`) as far as the login reply
/// record needs it: the entity-server address a successful client connects to.
///
/// On the wire the reference streams `ip` and `port` raw and `salt` through the
/// stream operator; on the little-endian targets this port runs on all three
/// are the native byte order, so the port writes them all the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ServerAddress {
    /// `ip`.
    pub ip: u32,
    /// `port`.
    pub port: u16,
    /// `salt`.
    pub salt: u16,
}

impl ServerAddress {
    /// Constructor.
    pub const fn new(ip: u32, port: u16, salt: u16) -> Self {
        Self { ip, port, salt }
    }

    /// `Address::NONE`.
    pub const NONE: ServerAddress = ServerAddress::new(0, 0, 0);

    /// `isNone()`.
    pub fn is_none(&self) -> bool {
        self.ip == 0
    }

    /// `operator>>` (`basictypes.cpp`): raw ip, raw port, salt.
    pub fn read_from_stream(&mut self, stream: &mut BinaryStream) -> bool {
        let (Ok(ip), Ok(port), Ok(salt)) =
            (stream.read_u32(), stream.read_u16(), stream.read_u16())
        else {
            return false;
        };
        self.ip = ip;
        self.port = port;
        self.salt = salt;
        true
    }

    /// `operator<<` (`basictypes.cpp`).
    pub fn write_to_stream(&self, stream: &mut BinaryStream) {
        stream.write_u32(self.ip);
        stream.write_u16(self.port);
        stream.write_u16(self.salt);
    }
}

/// `LoginReplyRecord` (`lib/connection/login_reply_record.hpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoginReplyRecord {
    server_addr: ServerAddress,
    session_key: u32,
}

impl LoginReplyRecord {
    /// Constructor.
    pub fn new(server_addr: ServerAddress, session_key: u32) -> Self {
        Self {
            server_addr,
            session_key,
        }
    }

    /// `serverAddr`.
    pub fn server_addr(&self) -> ServerAddress {
        self.server_addr
    }

    /// `sessionKey`.
    pub fn session_key(&self) -> u32 {
        self.session_key
    }

    /// `operator>>`: `is >> serverAddr >> sessionKey`.
    pub fn read_from_stream(&mut self, stream: &mut BinaryStream) -> bool {
        if !self.server_addr.read_from_stream(stream) {
            return false;
        }
        let Ok(session_key) = stream.read_u32() else {
            return false;
        };
        self.session_key = session_key;
        true
    }

    /// `operator<<`: `os << serverAddr << sessionKey`.
    pub fn write_to_stream(&self, stream: &mut BinaryStream) {
        self.server_addr.write_to_stream(stream);
        stream.write_u32(self.session_key);
    }
}

/// `ClientLoginRequest` (`server/loginapp/client_login_request.hpp`): the
/// per-address login state the duplicate-request cache answers from.
///
/// `creation_time` is zero while the login is pending on the DBApp and set to
/// the stamp of the DBApp reply by `set_data`; `params` is the credential
/// snapshot compared for identity; the challenge fields are the BW-06-01-02
/// `LoginChallengeState`.
pub struct ClientLoginRequest {
    /// `creationTime_`, zero while pending.
    creation_time: u64,
    /// `pParams_`.
    params: Option<LogOnParams>,
    /// `challengeType_` / `didFailChallenge_` / `pLoginChallenge_`.
    challenge: Option<LoginChallengeState>,
    /// `replyRecord_`.
    reply_record: LoginReplyRecord,
    /// `serverMsg_`.
    server_msg: String,
}

impl ClientLoginRequest {
    /// Constructor: `creationTime_ = 0` (pending), everything else default.
    pub fn new() -> Self {
        Self {
            creation_time: 0,
            params: None,
            challenge: None,
            reply_record: LoginReplyRecord::new(ServerAddress::NONE, 0),
            server_msg: String::new(),
        }
    }

    /// `hasPendingChallenge()`.
    pub fn has_pending_challenge(&self) -> bool {
        self.challenge
            .as_ref()
            .is_some_and(|c| c.pending().is_some())
    }

    /// `didFailChallenge()`.
    pub fn did_fail_challenge(&self) -> bool {
        self.challenge
            .as_ref()
            .is_some_and(|c| c.did_fail_challenge())
    }

    /// `pParams()`.
    pub fn params(&self) -> Option<&LogOnParams> {
        self.params.as_ref()
    }

    /// `pParams(pParams)`.
    pub fn set_params(&mut self, params: LogOnParams) {
        self.params = Some(params);
    }

    /// `pLoginChallenge()` for the challenge negotiation state; `None` before
    /// any challenge is issued and after one is cleared.
    pub fn challenge_state(&self) -> &Option<LoginChallengeState> {
        &self.challenge
    }

    /// Mutable challenge state, for `process_for_login_challenge` /
    /// `challenge_response`.
    pub fn challenge_state_mut(&mut self) -> &mut Option<LoginChallengeState> {
        &mut self.challenge
    }

    /// `creationTime()`.
    pub fn creation_time(&self) -> u64 {
        self.creation_time
    }

    /// `replyRecord()`.
    pub fn reply_record(&self) -> LoginReplyRecord {
        self.reply_record
    }

    /// `serverMsg()`.
    pub fn server_msg(&self) -> &str {
        &self.server_msg
    }

    /// `isPendingAuthentication()`: waiting on the DBApp, with no outstanding
    /// challenge.
    pub fn is_pending_authentication(&self) -> bool {
        !self.has_pending_challenge() && self.params.is_some() && self.creation_time == 0
    }

    /// `isTooOld()`: resolved, but older than `maxLoginDelayInStamps`.
    pub fn is_too_old(&self, now: u64) -> bool {
        !self.is_pending_authentication()
            && now.saturating_sub(self.creation_time) > max_login_delay_in_stamps()
    }

    /// `setData`: the DBApp replied; remember the success and mark the request
    /// resolved at the caller's stamp.
    pub fn set_data(&mut self, record: LoginReplyRecord, server_msg: &str, now: u64) {
        self.reply_record = record;
        self.server_msg = server_msg.to_string();
        self.creation_time = now;
    }

    /// `reset()`: back to pending for a fresh attempt from the same address.
    pub fn reset(&mut self) {
        self.creation_time = 0;
    }

    /// `writeSuccessResultToStream`: `stream << replyRecord_ << serverMsg_`.
    pub fn write_success_result_to_stream(&self, stream: &mut BinaryStream) {
        self.reply_record.write_to_stream(stream);
        let _ = stream.write_string(&self.server_msg);
    }

    /// `setLoginChallenge`: record the issued challenge (resetting the fail
    /// flag, as the reference `ClientLoginRequest::setLoginChallenge` does).
    pub fn set_login_challenge(
        &mut self,
        challenge_type: &str,
        challenge: Box<dyn LoginChallenge>,
    ) {
        let state = self
            .challenge
            .get_or_insert_with(LoginChallengeState::default);
        state.set_login_challenge(challenge_type.to_string(), challenge);
    }
}

impl Default for ClientLoginRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// `LoginApp::loginRequests_`: one cached request per client address.
#[derive(Default)]
pub struct LoginRequestCache {
    requests: HashMap<ServerAddress, ClientLoginRequest>,
}

impl LoginRequestCache {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `loginRequests_.count(addr) != 0` — whether a request already exists
    /// for this address.
    pub fn is_reattempt(&self, addr: ServerAddress) -> bool {
        self.requests.contains_key(&addr)
    }

    /// `loginRequests_.find(addr)`.
    pub fn find(&self, addr: ServerAddress) -> Option<&ClientLoginRequest> {
        self.requests.get(&addr)
    }

    /// Mutable `find`.
    pub fn find_mut(&mut self, addr: ServerAddress) -> Option<&mut ClientLoginRequest> {
        self.requests.get_mut(&addr)
    }

    /// `loginRequests_[addr]`: default-construct when absent.
    pub fn entry(&mut self, addr: ServerAddress) -> &mut ClientLoginRequest {
        self.requests.entry(addr).or_default()
    }

    /// `loginRequests_.erase(addr)`.
    pub fn erase(&mut self, addr: ServerAddress) {
        self.requests.remove(&addr);
    }

    /// Number of cached requests.
    pub fn len(&self) -> usize {
        self.requests.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// `sendAndCacheSuccess` prune: once the map outgrows `MAX_CACHED_REQUESTS`
    /// drop every too-old entry.
    pub fn prune_if_over_bound(&mut self, now: u64) {
        if self.requests.len() > MAX_CACHED_REQUESTS {
            self.retain_not_too_old(now);
        }
    }

    /// Drop every too-old request.
    pub fn retain_not_too_old(&mut self, now: u64) {
        self.requests.retain(|_, request| !request.is_too_old(now));
    }
}

/// The outcome of `LoginApp::handleResentPendingAttempt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResentPendingDecision {
    /// No cached request: the attempt proceeds normally.
    Continue,
    /// The outstanding challenge was re-sent; the caller verifies the response
    /// as usual (the reference returns false, so the login continues).
    ResentChallenge,
    /// A DBApp login is in flight: the duplicate is ignored.
    IgnoreInProgress,
}

/// `LoginApp::handleResentPendingAttempt` (`loginapp.cpp`): decide what a
/// re-sent attempt needs before the params are parsed.
///
/// With an outstanding challenge the challenge reply is re-sent and the login
/// continues (so the client can answer it); a request still waiting on the
/// DBApp is ignored; anything else continues normally.
pub fn handle_resent_pending_attempt(
    cache: &LoginRequestCache,
    addr: ServerAddress,
) -> ResentPendingDecision {
    let Some(request) = cache.find(addr) else {
        return ResentPendingDecision::Continue;
    };
    if request.has_pending_challenge() {
        return ResentPendingDecision::ResentChallenge;
    }
    if request.is_pending_authentication() {
        return ResentPendingDecision::IgnoreInProgress;
    }
    ResentPendingDecision::Continue
}

/// The outcome of `LoginApp::handleResentCachedAttempt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResentCachedDecision {
    /// No usable cached request (absent, too old, or different credentials):
    /// the attempt proceeds to a fresh DBApp login.
    Continue,
    /// The cached success was re-sent: the duplicate is answered and the
    /// attempt stops here.
    ResentSuccess,
}

/// `LoginApp::handleResentCachedAttempt` (`loginapp.cpp`): decide whether a
/// re-sent attempt, whose params were just parsed, is answered from the cache.
///
/// The cached success is re-sent only when the request is present, not too old
/// at `now`, and its params are security-identical to the re-sent ones.
pub fn handle_resent_cached_attempt(
    cache: &LoginRequestCache,
    addr: ServerAddress,
    params: &LogOnParams,
    now: u64,
) -> ResentCachedDecision {
    let Some(request) = cache.find(addr) else {
        return ResentCachedDecision::Continue;
    };
    if !request.is_too_old(now)
        && request
            .params()
            .is_some_and(|cached| cached.same_security(params))
    {
        return ResentCachedDecision::ResentSuccess;
    }
    ResentCachedDecision::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000_000; // 1s into the port clock
    const ADDR: ServerAddress = ServerAddress::new(0x0100_0007, 40000, 0x1234);
    const KEY: u32 = 0xdead_beef;

    fn params(user: &str) -> LogOnParams {
        let mut p = LogOnParams::with_credentials(user, "pw", "key");
        p.set_nonce(7);
        p
    }

    fn record() -> LoginReplyRecord {
        LoginReplyRecord::new(ServerAddress::new(0x0a00_0001, 20000, 0x0001), KEY)
    }

    fn resolved_request(user: &str) -> ClientLoginRequest {
        let mut r = ClientLoginRequest::new();
        r.set_params(params(user));
        r.set_data(record(), "welcome", NOW);
        r
    }

    #[test]
    fn reply_record_roundtrips_the_wire_format() {
        let mut stream = BinaryStream::new();
        record().write_to_stream(&mut stream);
        // ip, port, salt raw; session key after.
        assert_eq!(stream.as_bytes().len(), 4 + 2 + 2 + 4);
        let mut back = LoginReplyRecord::new(ServerAddress::NONE, 0);
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        assert!(back.read_from_stream(&mut read));
        assert_eq!(back, record());
        assert!(read.at_end());
    }

    #[test]
    fn a_fresh_request_is_pending_authentication() {
        let r = ClientLoginRequest::new();
        assert!(!r.has_pending_challenge());
        assert!(!r.did_fail_challenge());
        assert!(!r.is_pending_authentication()); // no params yet
        assert_eq!(r.creation_time(), 0);
    }

    #[test]
    fn params_set_without_reply_keeps_the_request_pending() {
        let mut r = ClientLoginRequest::new();
        r.set_params(params("alice"));
        assert!(r.is_pending_authentication());
        assert!(!r.is_too_old(NOW)); // pending is never too old
    }

    #[test]
    fn set_data_resolves_the_request_and_caches_the_result() {
        let mut r = ClientLoginRequest::new();
        r.set_params(params("alice"));
        assert!(r.is_pending_authentication());
        r.set_data(record(), "welcome", NOW);
        assert!(!r.is_pending_authentication());
        assert_eq!(r.creation_time(), NOW);
        assert_eq!(r.reply_record(), record());
        assert_eq!(r.server_msg(), "welcome");
    }

    #[test]
    fn a_resolved_request_becomes_too_old_after_the_window() {
        let mut r = ClientLoginRequest::new();
        r.set_params(params("alice"));
        r.set_data(record(), "welcome", NOW);
        assert!(!r.is_too_old(NOW));
        // Just inside the 10s window.
        assert!(!r.is_too_old(NOW + max_login_delay_in_stamps()));
        // One stamp past the window.
        assert!(r.is_too_old(NOW + max_login_delay_in_stamps() + 1));
    }

    #[test]
    fn write_success_result_is_the_reply_record_then_the_server_message() {
        let r = resolved_request("alice");
        let mut stream = BinaryStream::new();
        r.write_success_result_to_stream(&mut stream);
        let mut read = BinaryStream::from_slice(stream.as_bytes());
        let mut decoded = LoginReplyRecord::new(ServerAddress::NONE, 0);
        assert!(decoded.read_from_stream(&mut read));
        assert_eq!(decoded, record());
        assert_eq!(read.read_string().unwrap(), "welcome");
        assert!(read.at_end());
    }

    #[test]
    fn reset_returns_a_resolved_request_to_pending() {
        let mut r = resolved_request("alice");
        assert!(!r.is_pending_authentication());
        r.reset();
        assert!(r.is_pending_authentication());
    }

    #[test]
    fn resent_pending_ignores_a_dbapp_in_flight_login() {
        let mut cache = LoginRequestCache::new();
        cache.entry(ADDR).set_params(params("alice"));
        // Pending auth (no reply yet) and no challenge.
        assert_eq!(
            handle_resent_pending_attempt(&cache, ADDR),
            ResentPendingDecision::IgnoreInProgress
        );
    }

    #[test]
    fn resent_pending_continues_without_a_cached_request() {
        let cache = LoginRequestCache::new();
        assert_eq!(
            handle_resent_pending_attempt(&cache, ADDR),
            ResentPendingDecision::Continue
        );
    }

    #[test]
    fn resent_pending_resends_an_outstanding_challenge() {
        let mut cache = LoginRequestCache::new();
        let request = cache.entry(ADDR);
        request.set_params(params("alice"));
        request.set_login_challenge(
            crate::login_challenge::CHALLENGE_DELAY,
            Box::new(crate::login_challenge::DelayChallenge::new(1.5)),
        );
        assert_eq!(
            handle_resent_pending_attempt(&cache, ADDR),
            ResentPendingDecision::ResentChallenge
        );
    }

    #[test]
    fn resent_pending_continues_after_the_challenge_is_verified() {
        use crate::login_challenge::{
            CHALLENGE_DELAY, ChallengeAction, ChallengeFactories, ChallengeResponseOutcome,
            challenge_response, process_for_login_challenge,
        };

        let factories = ChallengeFactories::new();
        let mut cache = LoginRequestCache::new();
        let request = cache.entry(ADDR);
        assert_eq!(
            process_for_login_challenge(request.challenge_state_mut(), CHALLENGE_DELAY, &factories),
            ChallengeAction::SendChallenge
        );
        // The client answers the delay challenge: duration, then the echo.
        let mut reply = BinaryStream::new();
        reply.write_f32(0.5);
        reply.write_f32(1.0); // DelayLoginChallengeFactory::DEFAULT_DURATION
        assert_eq!(
            challenge_response(request.challenge_state_mut(), &mut reply),
            ChallengeResponseOutcome::Verified
        );
        request.set_params(params("alice"));
        // Challenge cleared and verified, DBApp login in flight: ignored.
        assert_eq!(
            handle_resent_pending_attempt(&cache, ADDR),
            ResentPendingDecision::IgnoreInProgress
        );
    }

    #[test]
    fn resent_cached_resends_an_identical_not_too_old_request() {
        let mut cache = LoginRequestCache::new();
        let request = cache.entry(ADDR);
        request.set_params(params("alice"));
        request.set_data(record(), "welcome", NOW);
        assert_eq!(
            handle_resent_cached_attempt(&cache, ADDR, &params("alice"), NOW),
            ResentCachedDecision::ResentSuccess
        );
    }

    #[test]
    fn resent_cached_continues_for_different_credentials() {
        let mut cache = LoginRequestCache::new();
        let request = cache.entry(ADDR);
        request.set_params(params("alice"));
        request.set_data(record(), "welcome", NOW);
        assert_eq!(
            handle_resent_cached_attempt(&cache, ADDR, &params("bob"), NOW),
            ResentCachedDecision::Continue
        );
    }

    #[test]
    fn resent_cached_continues_when_too_old() {
        let mut cache = LoginRequestCache::new();
        let request = cache.entry(ADDR);
        request.set_params(params("alice"));
        request.set_data(record(), "welcome", NOW);
        let late = NOW + max_login_delay_in_stamps() + 1;
        assert_eq!(
            handle_resent_cached_attempt(&cache, ADDR, &params("alice"), late),
            ResentCachedDecision::Continue
        );
    }

    #[test]
    fn resent_cached_continues_without_a_cached_request() {
        let cache = LoginRequestCache::new();
        assert_eq!(
            handle_resent_cached_attempt(&cache, ADDR, &params("alice"), NOW),
            ResentCachedDecision::Continue
        );
    }

    #[test]
    fn the_cache_prunes_only_when_it_outgrows_the_bound() {
        let mut cache = LoginRequestCache::new();
        for i in 0..MAX_CACHED_REQUESTS {
            let request = cache.entry(ServerAddress::new(i as u32, 1, 0));
            request.set_params(params("alice"));
            request.set_data(record(), "welcome", NOW);
        }
        cache.prune_if_over_bound(NOW);
        assert_eq!(cache.len(), MAX_CACHED_REQUESTS); // not over bound, no prune

        // Add one more so the bound is exceeded, then make it too old and prune.
        let request = cache.entry(ServerAddress::new(MAX_CACHED_REQUESTS as u32, 1, 0));
        request.set_params(params("alice"));
        request.set_data(record(), "welcome", NOW);
        assert_eq!(cache.len(), MAX_CACHED_REQUESTS + 1);
        let late = NOW + max_login_delay_in_stamps() + 1;
        cache.prune_if_over_bound(late);
        assert!(cache.is_empty());
    }

    #[test]
    fn is_reattempt_reflects_cache_presence() {
        let mut cache = LoginRequestCache::new();
        assert!(!cache.is_reattempt(ADDR));
        cache.entry(ADDR);
        assert!(cache.is_reattempt(ADDR));
        cache.erase(ADDR);
        assert!(!cache.is_reattempt(ADDR));
    }

    #[test]
    fn a_resent_identical_login_replays_the_exact_cached_result() {
        // The acceptance for BW-06-01-03: a client that re-sends because its
        // reply was dropped gets the very same bytes, not a second DBApp login.
        let mut cache = LoginRequestCache::new();
        let request = cache.entry(ADDR);
        request.set_params(params("alice"));
        request.set_data(record(), "welcome", NOW);

        let mut first = BinaryStream::new();
        request.write_success_result_to_stream(&mut first);

        // The re-sent attempt is answered from the cache while the cached
        // result is still fresh...
        assert_eq!(
            handle_resent_cached_attempt(&cache, ADDR, &params("alice"), NOW),
            ResentCachedDecision::ResentSuccess
        );
        // ...and writes byte-for-byte the same result.
        let mut second = BinaryStream::new();
        cache
            .find(ADDR)
            .expect("cached request")
            .write_success_result_to_stream(&mut second);
        assert_eq!(first.as_bytes(), second.as_bytes());

        // Past the window the same re-send is treated as fresh, not replayed.
        let late = NOW + max_login_delay_in_stamps() + 1;
        assert_eq!(
            handle_resent_cached_attempt(&cache, ADDR, &params("alice"), late),
            ResentCachedDecision::Continue
        );
    }
}
