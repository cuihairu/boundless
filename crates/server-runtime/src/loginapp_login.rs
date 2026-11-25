//! BW-14-02-01 increment 3: the LoginApp external login faces.
//!
//! `LoginApp::login` and `LoginApp::challengeResponse`
//! (`server/loginapp/loginapp.cpp:706/1195`) are the two served elements of
//! the external `LoginInterface` besides the probe. This module is the
//! aggregator: the gate machinery it walks lives in the BW-06 items
//! (`login_gate`, `login_request`, `login_challenge`, `db_login`) and this
//! face drives them in the reference order, writes the client replies over
//! the bound interface, and hands accepted logins to the DBApp leg.
//!
//! The reference answers on `header.pChannel`'s bundle when a channel exists
//! and on an off-channel `UDPBundle` otherwise; the port has no channels yet
//! (BW-02-03), so every reply takes the off-channel path — which is also the
//! path a one-shot login datagram takes in the reference. Replies to failed
//! logins are deliberately unreliable (`RELIABLE_NO`) and rate-throttled
//! (`maxRepliesOnFailPerSecond`), exactly as the reference's
//! `handleFailure` does, so a client cannot DOS the replies.
//!
//! The reference keeps one live `DatabaseReplyHandler` object per attempt
//! holding the client address, channel, reply id and params snapshot; the
//! port keeps the same per-attempt facts in the `in_flight` registry
//! ([`LoginFaces::db_reply`] / [`LoginFaces::db_no_reply`] are
//! `DatabaseReplyHandler::handleMessage` / `handleException`
//! (`server/loginapp/database_reply_handler.cpp`)). The DBApp request leg
//! itself is the [`LogOnForward`] seam: the production sink
//! [`DbAppForward`] bundles the `DBAppInterface::logOn` request
//! (`dbBundle.startRequest( DBAppInterface::logOn, pDBHandler ) +
//! dbAppAlpha().send()`, `loginapp.cpp:1024-1029`) and routes the answer
//! back through [`DbReplyHandler`]; [`NoLogOnForward`] remains as the
//! dead-link stand-in for tests without a wire.
//!
//! DEBUG-severity reference logs are omitted (they are silent under the
//! default message severity threshold); `loginStats_` counters and the
//! NAT/firewall redirect (`NATConfig::isExternalIP`) are not ported.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use foundation::stream::BinaryStream;
use mercury::endpoint::Address;
use mercury::interfaces::{InputMessageHandler, ReplyMessageHandler};
use mercury::misc::ReplyID;
use mercury::network_interface::NetworkInterface;
use mercury::nub_exception::NubException;
use mercury::udp_bundle::{DEFAULT_REQUEST_TIMEOUT, ReliableType, UdpBundle};
use mercury::unpacked_message_header::UnpackedMessageHeader;

use crate::config::BwConfig;
use crate::db_login::{
    DB_NO_REPLY, DbReplyOutcome, IP_BAN_MESSAGE, SystemOverload, db_app_log_on_element,
    handle_db_reply, write_logon_request,
};
use crate::login_challenge::{
    ChallengeAction, ChallengeFactories, ChallengeResponseOutcome, LoginChallengeState,
    challenge_response, process_for_login_challenge,
};
use crate::login_gate::{
    IpBanMap, LoginGate, LoginGateConfig, RateLimiter, check_post_version_gates,
    check_pre_version_gates, write_failure, write_success_reply,
};
use crate::login_request::{
    ClientLoginRequest, LoginRequestCache, ResentCachedDecision, ResentPendingDecision,
    ServerAddress, handle_resent_cached_attempt, handle_resent_pending_attempt,
};
use crate::loginapp::{ClientServerProtocolVersion, LogOnParams, LogOnStatus};

/// `WARN_BUNDLE_PACKETS` (`loginapp.cpp:74`): the packet count past which
/// `handleFailure` / `sendRawReply` warn about the reply's size.
pub const WARN_BUNDLE_PACKETS: usize = 3;

/// `Config::maxRepliesOnFailPerSecond` default (`loginapp_config.cpp:31`).
const DEFAULT_MAX_REPLIES_ON_FAIL_PER_SECOND: u32 = 100;

/// The port's stamp rate: one second in nanosecond stamps, the convention
/// the BW-06 gate modules keep (`login_gate.rs`).
const STAMPS_PER_SECOND: u64 = 1_000_000_000;

/// The login faces' configuration: the BW-06 gate options plus the options
/// the aggregator itself reads.
#[derive(Clone, Debug)]
pub struct LoginFacesConfig {
    /// The BW-06-01-05 gate options.
    pub gate: LoginGateConfig,
    /// `Config::challengeType` (`loginApp/challengeType`, default empty:
    /// no challenge).
    pub challenge_type: String,
    /// `Config::passwordlessLoginsOnly` (`loginapp_config.cpp:44`).
    pub passwordless_logins_only: bool,
    /// `Config::maxRepliesOnFailPerSecond`.
    pub max_replies_on_fail_per_second: u32,
}

impl LoginFacesConfig {
    /// The `loginapp_config.cpp` defaults.
    pub fn new() -> Self {
        Self {
            gate: LoginGateConfig::new(),
            challenge_type: String::new(),
            passwordless_logins_only: false,
            max_replies_on_fail_per_second: DEFAULT_MAX_REPLIES_ON_FAIL_PER_SECOND,
        }
    }

    /// Read the `loginApp` section of the `bw.xml` chain.
    pub fn from_config(config: &BwConfig) -> Self {
        let mut gate = LoginGateConfig::new();
        gate.allow_login = config.get_bool("loginApp/allowLogin", false);
        gate.allow_unencrypted_logins = config.get_bool("loginApp/allowUnencryptedLogins", false);
        gate.verbose_login_failures = config.get_bool("loginApp/verboseLoginFailures", false);
        gate.login_rate_limit = config.get_uint("loginApp/loginRateLimit", 0);
        gate.rate_limit_duration_seconds = config.get_uint("loginApp/rateLimitDuration", 0) as f64;
        gate.max_username_length = config.get_uint("loginApp/maxUsernameLength", 256) as usize;
        gate.max_password_length = config.get_uint("loginApp/maxPasswordLength", 256) as usize;
        gate.max_login_message_size =
            config.get_uint("loginApp/maxLoginMessageSize", 1472) as usize;
        gate.ip_ban_cleanup_interval_seconds =
            config.get_uint("loginApp/ipBanListCleanupInterval", 10) as f64;
        Self {
            gate,
            challenge_type: config.get_string("loginApp/challengeType", ""),
            passwordless_logins_only: config.get_bool("loginApp/passwordlessLoginsOnly", false),
            max_replies_on_fail_per_second: config.get_uint(
                "loginApp/maxRepliesOnFailPerSecond",
                DEFAULT_MAX_REPLIES_ON_FAIL_PER_SECOND,
            ),
        }
    }
}

impl Default for LoginFacesConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The DBApp leg seam: `dbBundle << source << *pParams;
/// dbAppAlpha().send()` (`loginapp.cpp:1024-1029`).
///
/// The production sink [`DbAppForward`] bundles the request onto the wire;
/// [`NoLogOnForward`] drops it, the way a dead DBApp
/// link would: no reply ever reaches the client.
pub trait LogOnForward {
    /// Hand one accepted login to the DBApp leg.
    fn send_logon(&self, source: ServerAddress, params: LogOnParams);
}

/// The dead-link stand-in: a sink that drops every request.
#[derive(Default)]
pub struct NoLogOnForward;

impl LogOnForward for NoLogOnForward {
    fn send_logon(&self, _source: ServerAddress, _params: LogOnParams) {}
}

/// A recording sink for tests and inspection.
#[derive(Default)]
pub struct RecordingLogOnForward(pub RefCell<Vec<(ServerAddress, LogOnParams)>>);

impl LogOnForward for RecordingLogOnForward {
    fn send_logon(&self, source: ServerAddress, params: LogOnParams) {
        self.0.borrow_mut().push((source, params));
    }
}

/// `DatabaseReplyHandler` (`server/loginapp/database_reply_handler.hpp`)
/// as a Mercury reply handler: the object a `DBAppInterface::logOn`
/// request carries so its answer comes back. The reference constructs one
/// per attempt holding the client address, channel, reply id and params
/// snapshot; the port keeps those facts in the faces' `in_flight`
/// registry, so this handler only carries the client address and routes
/// every delivery into the faces.
pub struct DbReplyHandler {
    /// The faces the attempt was accepted by; weak so a request outliving
    /// its LoginApp cannot pin it.
    faces: Weak<LoginFaces>,
    /// `clientAddr_`: the client the attempt came from.
    client_addr: ServerAddress,
}

impl DbReplyHandler {
    /// The client address as the wire sees it.
    fn address(&self) -> Address {
        Address {
            ip: self.client_addr.ip,
            port: self.client_addr.port,
            salt: self.client_addr.salt,
        }
    }
}

impl ReplyMessageHandler for DbReplyHandler {
    /// `handleMessage`: the DBApp answered; decode it and reply to the
    /// client.
    fn handle_message(
        &self,
        _source: &Address,
        _header: &UnpackedMessageHeader,
        data: &mut BinaryStream,
        _arg: usize,
    ) {
        if let Some(faces) = self.faces.upgrade() {
            faces.db_reply(&self.address(), data);
        }
    }

    /// `handleException`: no reply came back. The reference reports the
    /// overload failure first, then the WARNING with the reason
    /// (`database_reply_handler.cpp:165-172`).
    fn handle_exception(&self, exception: &NubException, _arg: usize) {
        if let Some(faces) = self.faces.upgrade() {
            faces.db_no_reply(&self.address());
        }
        eprintln!(
            "DatabaseReplyHandler: got an exception ({})",
            exception.reason().to_string()
        );
    }

    /// `handleShuttingDown`: the request died with Mercury, not with the
    /// DBApp; the attempt is dropped silently
    /// (`database_reply_handler.cpp:175-179`).
    fn handle_shutting_down(&self, _exception: &NubException, _arg: usize) {
        if let Some(faces) = self.faces.upgrade() {
            faces.db_shutting_down(&self.address());
        }
    }
}

/// The production DBApp leg (the `dbAppAlpha_` member,
/// `loginapp.hpp:117`): the accepted login goes out as a
/// `DBAppInterface::logOn` request on the bound interface, carrying a
/// [`DbReplyHandler`] with the client address so the DBApp's answer finds
/// its way back.
///
/// `dbAppAlpha_.addr()` starts unset; [`LoginFaces::notify_db_app_alpha`]
/// records it, exactly as the reference's `notifyDBAppAlpha`
/// (`loginapp.cpp:516-521`) points the alpha's channel owner at the
/// address and its channel then establishes.
pub struct DbAppForward {
    /// The interface the request datagrams leave on; weak so the faces
    /// (which hold this sink) cannot pin the interface through it.
    interface: Weak<NetworkInterface>,
    /// The faces, for the alpha address and the reply routing; handed over
    /// by [`DbAppForward::set_faces`] once they exist.
    faces: RefCell<Weak<LoginFaces>>,
}

impl DbAppForward {
    /// Constructor over a live interface.
    pub fn new(interface: &Rc<NetworkInterface>) -> Self {
        Self {
            interface: Rc::downgrade(interface),
            faces: RefCell::new(Weak::new()),
        }
    }

    /// Hand the sink the faces it serves; called once by the interface
    /// `open`, after the faces exist.
    pub fn set_faces(&self, faces: &Rc<LoginFaces>) {
        *self.faces.borrow_mut() = Rc::downgrade(faces);
    }
}

impl LogOnForward for DbAppForward {
    /// `dbBundle.startRequest( DBAppInterface::logOn, pDBHandler );
    /// dbBundle << source << *pParams; dbAppAlpha().send()`
    /// (`loginapp.cpp:1024-1029`).
    fn send_logon(&self, source: ServerAddress, params: LogOnParams) {
        let Some(interface) = self.interface.upgrade() else {
            return;
        };
        let Some(faces) = self.faces.borrow().upgrade() else {
            return;
        };
        let alpha = faces.db_app_alpha();
        if alpha.is_none() {
            // No alpha has registered, so the ready gate cannot have let
            // this attempt through; only a direct send reaches here. Drop
            // the request the way a dead DBApp link would.
            return;
        }

        let mut bundle = UdpBundle::new(0);
        bundle.start_request(
            &db_app_log_on_element(),
            Rc::new(DbReplyHandler {
                faces: Rc::downgrade(&faces),
                client_addr: source,
            }),
            0,
            DEFAULT_REQUEST_TIMEOUT,
            ReliableType::Driver,
        );

        let mut body = BinaryStream::new();
        write_logon_request(&source, &params, &mut body);
        bundle.write_bytes(body.as_bytes());

        interface.send(&alpha, &mut bundle, None);
    }
}

/// `LoginApp`'s mutable login state, interior to keep the message-handler
/// faces `&self` like the reference's registered handlers.
struct LoginFacesState {
    /// `isDBReady_`: set when a DBApp registers (the dbapp leg).
    db_ready: bool,
    /// `dbAppAlpha_`'s address (`loginapp.hpp:117`): unset until the
    /// DBAppMgr names the alpha, then the destination the DBApp request
    /// leg sends to.
    db_app_alpha: Address,
    /// `numAllowedLoginsLeft_` / `lastRateLimitCheckTime_`.
    limiter: RateLimiter,
    /// `ipAddressBanMap_` / `nextIPAddressBanMapCleanupTime_`.
    bans: IpBanMap,
    /// `systemOverloaded_` / `systemOverloadedTime_`.
    overload: SystemOverload,
    /// `loginRequests_`.
    requests: LoginRequestCache,
    /// The per-attempt facts a reference `DatabaseReplyHandler` object
    /// holds, keyed by the client address.
    in_flight: HashMap<ServerAddress, ReplyID>,
    /// `numFailRepliesLeft_` / `repliedFailsCounterResetTime_`.
    fail_replies_left: u32,
    fail_replies_reset_at: u64,
    /// `gNumLoginAttempts` (`loginapp.cpp`).
    num_login_attempts: u32,
    /// `gNumLoginFailures`.
    num_login_failures: u32,
}

/// The LoginApp external login faces: `login`, `challengeResponse` and the
/// DBApp reply side, over one bound interface.
pub struct LoginFaces {
    config: LoginFacesConfig,
    factories: ChallengeFactories,
    state: RefCell<LoginFacesState>,
    interface: Weak<NetworkInterface>,
    /// `gNumLogins`, the counter the probe reports.
    users_count: Rc<Cell<u32>>,
    forward: Rc<dyn LogOnForward>,
    clock: Rc<dyn Fn() -> u64>,
}

impl LoginFaces {
    /// Constructor with the system clock.
    pub fn new(
        config: LoginFacesConfig,
        interface: Weak<NetworkInterface>,
        users_count: Rc<Cell<u32>>,
        forward: Rc<dyn LogOnForward>,
    ) -> Self {
        Self::with_clock(
            config,
            interface,
            users_count,
            forward,
            Rc::new(system_stamps),
        )
    }

    /// Constructor with an injected clock, the nanosecond-stamp convention
    /// the BW-06 gate modules keep.
    pub fn with_clock(
        config: LoginFacesConfig,
        interface: Weak<NetworkInterface>,
        users_count: Rc<Cell<u32>>,
        forward: Rc<dyn LogOnForward>,
        clock: Rc<dyn Fn() -> u64>,
    ) -> Self {
        let max_replies = config.max_replies_on_fail_per_second;
        Self {
            factories: ChallengeFactories::new(),
            state: RefCell::new(LoginFacesState {
                db_ready: false,
                db_app_alpha: Address::default(),
                limiter: RateLimiter::new(config.gate.login_rate_limit),
                bans: IpBanMap::new(),
                overload: SystemOverload::new(),
                requests: LoginRequestCache::new(),
                in_flight: HashMap::new(),
                fail_replies_left: max_replies / 2,
                fail_replies_reset_at: 0,
                num_login_attempts: 0,
                num_login_failures: 0,
            }),
            config,
            interface,
            users_count,
            forward,
            clock,
        }
    }

    /// The faces' configuration.
    pub fn config(&self) -> &LoginFacesConfig {
        &self.config
    }

    /// `isDBReady()`.
    pub fn db_ready(&self) -> bool {
        self.state.borrow().db_ready
    }

    /// `isDBReady_`, set by the DBApp registration faces.
    pub fn set_db_ready(&self, ready: bool) {
        self.state.borrow_mut().db_ready = ready;
    }

    /// `dbAppAlpha_`'s address, the destination of the DBApp request leg.
    pub fn db_app_alpha(&self) -> Address {
        self.state.borrow().db_app_alpha
    }

    /// `LoginApp::notifyDBAppAlpha` (`loginapp.cpp:516-521`): the
    /// DBAppMgr's notice of which DBApp is the alpha. The reference
    /// records the address on the alpha's channel owner, and readiness is
    /// that channel establishing (`isDBReady()`, `loginapp.hpp:122`); the
    /// port has no channels yet (BW-02-03), so registration is the ready
    /// state itself.
    pub fn notify_db_app_alpha(&self, addr: &Address) {
        eprintln!("LoginApp::notifyDBAppAlpha: {}", addr.c_str());
        let mut st = self.state.borrow_mut();
        st.db_app_alpha = *addr;
        st.db_ready = true;
    }

    /// `gNumLoginAttempts`.
    pub fn num_login_attempts(&self) -> u32 {
        self.state.borrow().num_login_attempts
    }

    /// `gNumLoginFailures`.
    pub fn num_login_failures(&self) -> u32 {
        self.state.borrow().num_login_failures
    }

    /// `gNumLogins`, the probe's `usersCount`.
    pub fn users_count(&self) -> u32 {
        self.users_count.get()
    }

    /// Ban an IP until `end_stamp`, the map half of `handleBanIP`.
    pub fn ban_ip(&self, ip: u32, end_stamp: u64) {
        self.state.borrow_mut().bans.ban(ip, end_stamp);
    }

    /// Run a closure over the request cache (`loginRequests_`).
    pub fn with_requests<R>(&self, f: impl FnOnce(&mut LoginRequestCache) -> R) -> R {
        f(&mut self.state.borrow_mut().requests)
    }

    /// `LoginApp::login` (`loginapp.cpp:706`): the gate sequence, the
    /// challenge gate, the logon-parameter gates, and the DBApp forward.
    pub fn login(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        let addr = server_address_of(source);
        let now = (self.clock)();
        let verbose = self.config.gate.verbose_login_failures;
        let mut st = self.state.borrow_mut();

        // Reset the rate-limit window when it has elapsed (loginapp.cpp:712),
        // then the pre-version gates: the allow-login flag, the IP-ban check
        // and sweep, and the spoofed-empty-address drop
        // (loginapp.cpp:721-789).
        st.limiter.begin_window(now, &self.config.gate);
        match check_pre_version_gates(&self.config.gate, &mut st.bans, addr.ip, now) {
            LoginGate::Reject { status } => {
                if verbose {
                    if !self.config.gate.allow_login {
                        eprintln!(
                            "LoginApp::login: Dropping login attempt from {} because config/allowLogin is false.",
                            source.c_str()
                        );
                    } else {
                        eprintln!(
                            "LoginApp::login: Dropping login attempt from {} because this IP address is currently banned.",
                            source.c_str()
                        );
                    }
                }
                data.finish();
                let reply_id = header.reply_id();
                drop(st);
                self.handle_failure(source, reply_id, status, None, false);
                return;
            }
            // Spoofed address trying to login as web client; dropped
            // without a reply.
            LoginGate::DropSilently => {
                if verbose {
                    eprintln!("LoginApp::login: Spoofed empty address");
                }
                data.finish();
                return;
            }
            LoginGate::Proceed => {}
        }

        let is_reattempt = st.requests.is_reattempt(addr);
        eprintln!(
            "LoginApp::login: {} from {}",
            if is_reattempt {
                "Re-attempt"
            } else {
                "Attempt"
            },
            source.c_str()
        );
        st.num_login_attempts += 1;

        let mut version = ClientServerProtocolVersion::default();
        if !version.read_from_stream(data) {
            if verbose {
                eprintln!(
                    "LoginApp::login: Not enough data on stream ({})",
                    header.length()
                );
            }
            let reply_id = header.reply_id();
            drop(st);
            self.handle_failure(
                source,
                reply_id,
                LogOnStatus::LoginMalformedRequest as u8,
                None,
                false,
            );
            return;
        }

        if !ClientServerProtocolVersion::current().supports(&version) {
            if verbose {
                eprintln!(
                    "LoginApp::login: User at {} tried to log on with protocol version {}. Expected {}.",
                    source.c_str(),
                    version.c_str(),
                    ClientServerProtocolVersion::current().c_str()
                );
            }
            data.finish();
            let reply_id = header.reply_id();
            drop(st);
            self.handle_failure(
                source,
                reply_id,
                LogOnStatus::LoginBadProtocolVersion as u8,
                None,
                false,
            );
            return;
        }

        // First check whether this is a repeat attempt from a recent pending
        // login before attempting to decrypt and log in
        // (loginapp.cpp:836-844). A pending challenge is re-sent here and
        // -- because the login continues -- once more at the challenge gate
        // below, so a duplicate during a pending challenge draws two
        // identical replies; the reference does the same.
        match handle_resent_pending_attempt(&st.requests, addr) {
            ResentPendingDecision::IgnoreInProgress => {
                data.finish();
                return;
            }
            ResentPendingDecision::ResentChallenge => {
                match self.challenge_payload(&st.requests, addr) {
                    Some(payload) => {
                        let reply_id = header.reply_id();
                        drop(st);
                        self.send_raw_reply(source, reply_id, &payload);
                        st = self.state.borrow_mut();
                    }
                    None => {
                        let reply_id = header.reply_id();
                        drop(st);
                        self.handle_failure(
                            source,
                            reply_id,
                            LogOnStatus::LoginRejectedChallengeError as u8,
                            Some("Failed to send challenge data."),
                            false,
                        );
                        return;
                    }
                }
            }
            ResentPendingDecision::Continue => {}
        }

        // The post-version gates (loginapp.cpp:846-902): the rate limit,
        // the DBApp-ready check, and the remembered overload status.
        let state = &mut *st;
        let post = check_post_version_gates(
            &self.config.gate,
            &mut state.limiter,
            state.db_ready,
            &mut state.overload,
            now,
        );
        match post {
            LoginGate::Reject { status } => {
                if verbose {
                    if st.limiter.is_rate_limited(&self.config.gate) {
                        eprintln!(
                            "LoginApp::login: Login from {} not allowed due to rate limiting",
                            source.c_str()
                        );
                    } else if !st.db_ready {
                        eprintln!("LoginApp::login: Attempted login when database not yet ready.");
                    } else {
                        eprintln!(
                            "LoginApp::login: Attempted login when system overloaded or not yet ready."
                        );
                    }
                }
                data.finish();
                let reply_id = header.reply_id();
                drop(st);
                self.handle_failure(source, reply_id, status, None, false);
                return;
            }
            LoginGate::DropSilently | LoginGate::Proceed => {}
        }

        // The challenge gate (loginapp.cpp:1048-1127).
        let failed_before = st
            .requests
            .find(addr)
            .is_some_and(|request| request.did_fail_challenge());
        let action = match st.requests.find_mut(addr) {
            Some(request) => process_for_login_challenge(
                request.challenge_state_mut(),
                &self.config.challenge_type,
                &self.factories,
            ),
            None if self.config.challenge_type.is_empty() => ChallengeAction::Proceed,
            None => {
                // The reference only creates the loginRequests_ entry once a
                // challenge is actually issued (loginapp.cpp:1108-1112), so
                // the fresh state is built aside and installed on success.
                let mut fresh: Option<LoginChallengeState> = None;
                let action = process_for_login_challenge(
                    &mut fresh,
                    &self.config.challenge_type,
                    &self.factories,
                );
                if let Some(state) = fresh {
                    *st.requests.entry(addr).challenge_state_mut() = Some(state);
                }
                action
            }
        };
        match action {
            ChallengeAction::RejectChallengeError => {
                if !failed_before {
                    eprintln!(
                        "LoginApp::login: Configured login challenge factory (\"{}\") failed to create a challenge instance",
                        self.config.challenge_type
                    );
                }
                let message = if failed_before {
                    "Failed login challenge"
                } else {
                    "Failed to instantiate login challenge."
                };
                data.finish();
                let reply_id = header.reply_id();
                drop(st);
                self.handle_failure(
                    source,
                    reply_id,
                    LogOnStatus::LoginRejectedChallengeError as u8,
                    Some(message),
                    false,
                );
                return;
            }
            ChallengeAction::SendChallenge => {
                let payload = self.challenge_payload(&st.requests, addr);
                data.finish();
                let reply_id = header.reply_id();
                match payload {
                    Some(payload) => {
                        drop(st);
                        self.send_raw_reply(source, reply_id, &payload);
                    }
                    None => {
                        // Unreachable in this port: the factory map is
                        // closed over challenges whose `write` succeeds,
                        // so only the resent-pending path above can see
                        // a declining challenge. The reference's open C++
                        // factories could hand `sendChallengeReply` one
                        // that refuses to stream; keep the arm verbatim.
                        drop(st);
                        self.handle_failure(
                            source,
                            reply_id,
                            LogOnStatus::LoginRejectedChallengeError as u8,
                            Some("Failed to send challenge data."),
                            false,
                        );
                    }
                }
                return;
            }
            ChallengeAction::Proceed => {}
        }

        // Read off login parameters (loginapp.cpp:904-965). The size bound
        // applies to what remains after the version. The reference reads
        // them through the StreamEncoder with an unencrypted retry
        // (BW-06-03); this port reads the clear form only.
        if data.remaining() > self.config.gate.max_login_message_size {
            if verbose {
                eprintln!("LoginApp::login: Attempted login with too long credentials.");
            }
            data.finish();
            let reply_id = header.reply_id();
            drop(st);
            self.handle_failure(
                source,
                reply_id,
                LogOnStatus::LoginMalformedRequest as u8,
                None,
                false,
            );
            return;
        }

        let mut params = LogOnParams::new();
        if !params.read_from_stream(data) {
            if verbose {
                eprintln!(
                    "LoginApp::login: Could not destream login parameters. Possibly caused by mis-matching LoginApp keypair, or different client/server res trees."
                );
            }
            let reply_id = header.reply_id();
            drop(st);
            self.handle_failure(
                source,
                reply_id,
                LogOnStatus::LoginMalformedRequest as u8,
                None,
                false,
            );
            return;
        }

        if params.username().len() > self.config.gate.max_username_length
            || params.password().len() > self.config.gate.max_password_length
        {
            if verbose {
                eprintln!("LoginApp::login: Attempted login with too long credentials.");
            }
            data.finish();
            let reply_id = header.reply_id();
            drop(st);
            self.handle_failure(
                source,
                reply_id,
                LogOnStatus::LoginMalformedRequest as u8,
                None,
                false,
            );
            return;
        }

        // First check whether this is a repeat attempt from a recently
        // resolved login before attempting to log in (loginapp.cpp:968-975).
        if let ResentCachedDecision::ResentSuccess =
            handle_resent_cached_attempt(&st.requests, addr, &params, now)
        {
            if let Some(request) = st.requests.find(addr) {
                self.send_success(source, header.reply_id(), request);
            }
            return;
        }

        // We've done the hard work of decrypting the logon parameters now,
        // so we count this as a login with regards to rate-limiting
        // (loginapp.cpp:977-981).
        st.limiter.consume(&self.config.gate);

        // Check that it has encryption key if we disallow unencrypted logins
        // (loginapp.cpp:984-994).
        if params.encryption_key().is_empty() && !self.config.gate.allow_unencrypted_logins {
            if verbose {
                eprintln!(
                    "LoginApp::login: No encryption key supplied, and server is not allowing unencrypted logins."
                );
            }
            let reply_id = header.reply_id();
            drop(st);
            self.handle_failure(
                source,
                reply_id,
                LogOnStatus::LoginMalformedRequest as u8,
                None,
                false,
            );
            return;
        }

        if self.config.passwordless_logins_only && !params.password().is_empty() {
            if verbose {
                eprintln!(
                    "LoginApp::login: Login attempt from {} is using password while passwordlessLoginsOnly option is active",
                    source.c_str()
                );
            }
            let reply_id = header.reply_id();
            drop(st);
            self.handle_failure(
                source,
                reply_id,
                LogOnStatus::LoginRejectedInvalidPassword as u8,
                None,
                false,
            );
            return;
        }

        eprintln!(
            "LoginApp::login: Logging in user '{}' ({})",
            params.username(),
            source.c_str()
        );

        // Remember that this attempt is now in progress and discard further
        // attempts from that address for some time after it completes
        // (loginapp.cpp:1018-1023).
        let entry = st.requests.entry(addr);
        entry.reset();
        entry.set_params(params.clone());
        st.in_flight.insert(addr, header.reply_id());
        drop(st);
        self.forward.send_logon(addr, params);
    }

    /// `LoginApp::challengeResponse` (`loginapp.cpp:1195`): read and verify
    /// the client's answer to the outstanding challenge.
    pub fn challenge_response(&self, source: &Address, data: &mut BinaryStream) {
        let addr = server_address_of(source);
        let mut st = self.state.borrow_mut();
        let Some(request) = st.requests.find_mut(addr) else {
            // Probably timed out. The reference sets
            // `*(header.pBreakLoop) = true` here (loginapp.cpp:1202); the
            // break-loop field is not ported yet (BW-02-03), so this port
            // just drains.
            data.finish();
            return;
        };
        let start = (self.clock)();
        let outcome = challenge_response(request.challenge_state_mut(), data);
        drop(st);
        match outcome {
            ChallengeResponseOutcome::TimedOut => {}
            // Challenge response already verified, this is a re-send; let
            // the login message handler re-send cached result
            // (loginapp.cpp:1212-1218).
            ChallengeResponseOutcome::AlreadyVerified => data.finish(),
            ChallengeResponseOutcome::Failed => {
                eprintln!(
                    "LoginApp::login: Client {} failed login challenge (took {:.3}ms to verify)",
                    source.c_str(),
                    ((self.clock)() - start) as f64 / 1_000_000.0
                );
                data.finish();
            }
            // The TRACE pass line and the challenge-time stat samples are
            // not ported.
            ChallengeResponseOutcome::Verified => {}
        }
    }

    /// `DatabaseReplyHandler::handleMessage`
    /// (`database_reply_handler.cpp:45`): decode the DBApp's answer and
    /// reply to the client. `source` is the client address the in-flight
    /// attempt was registered under, not the DBApp's.
    pub fn db_reply(&self, source: &Address, data: &mut BinaryStream) {
        let addr = server_address_of(source);
        let now = (self.clock)();
        let now_epoch = (now / STAMPS_PER_SECOND) as i64;
        let mut st = self.state.borrow_mut();
        let Some(reply_id) = st.in_flight.remove(&addr) else {
            // No handler in flight for this address: a stray reply.
            return;
        };
        match handle_db_reply(data, now_epoch) {
            DbReplyOutcome::Failure {
                status,
                message,
                erase_cache,
            } => {
                // The overload statuses the reply leaves behind
                // (database_reply_handler.cpp:103-111): BASEAPP only when
                // none is remembered yet, CELL and DB unconditionally.
                if (st.overload.current(now) == 0
                    && status == LogOnStatus::LoginRejectedBaseAppOverload as u8)
                    || status == LogOnStatus::LoginRejectedCellAppOverload as u8
                    || status == LogOnStatus::LoginRejectedDbAppOverload as u8
                {
                    st.overload.note_db_failure(status, now);
                }
                drop(st);
                self.handle_failure(source, reply_id, status, message.as_deref(), erase_cache);
            }
            DbReplyOutcome::Ban { ban_seconds } => {
                if ban_seconds > 0 {
                    eprintln!(
                        "DatabaseReplyHandler::handleMessage(): banning IP address {} for {} seconds",
                        source.c_str(),
                        ban_seconds
                    );
                }
                // handleBanIP (loginapp.cpp:688-695).
                st.bans.ban(addr.ip, now + ban_seconds * STAMPS_PER_SECOND);
                drop(st);
                self.handle_failure(
                    source,
                    reply_id,
                    LogOnStatus::LoginRejectedIpAddressBan as u8,
                    Some(IP_BAN_MESSAGE),
                    true,
                );
            }
            DbReplyOutcome::Success { record, server_msg } => {
                // sendAndCacheSuccess (loginapp.cpp:1280-1310). The
                // reference's MF_ASSERT_DEV on the params identity is a
                // release no-op and is not ported.
                let entry = st.requests.entry(addr);
                entry.set_data(record, &server_msg, now);
                self.send_success(source, reply_id, entry);
                st.requests.prune_if_over_bound(now);
            }
            // `handle_db_reply` never decodes to `ShuttingDown`: the
            // reference reaches this from a separate DBApp shutdown
            // message, which this port answers through
            // `db_shutting_down` below. The arm stays for exhaustiveness.
            DbReplyOutcome::ShuttingDown => {
                eprintln!(
                    "DatabaseReplyHandler::handleShuttingDown: Ignoring {}",
                    source.c_str()
                );
            }
        }
    }

    /// `DatabaseReplyHandler::handleException`
    /// (`database_reply_handler.cpp:176`): the DBApp never answered. The
    /// reference's WARNING carries the `NubException` reason string; the
    /// port has no nub, so that line is not printed.
    pub fn db_no_reply(&self, source: &Address) {
        let addr = server_address_of(source);
        let mut st = self.state.borrow_mut();
        let Some(reply_id) = st.in_flight.remove(&addr) else {
            return;
        };
        drop(st);
        self.handle_failure(
            source,
            reply_id,
            LogOnStatus::LoginRejectedDbAppOverload as u8,
            Some(DB_NO_REPLY),
            true,
        );
    }

    /// `DatabaseReplyHandler::handleShuttingDown`
    /// (`database_reply_handler.cpp:190`): the DBApp is going away while an
    /// attempt is outstanding. The attempt is dropped with no reply -- the
    /// in-flight slot stays, so a retry from the same address is still
    /// recognised as a duplicate.
    pub fn db_shutting_down(&self, source: &Address) {
        eprintln!(
            "DatabaseReplyHandler::handleShuttingDown: Ignoring {}",
            source.c_str()
        );
    }

    /// `sendChallengeReply`'s payload (`loginapp.cpp:1349`): the challenge
    /// status, the type and the challenge data, or `None` when the challenge
    /// cannot stream itself (which the caller reports as a failure).
    fn challenge_payload(
        &self,
        requests: &LoginRequestCache,
        addr: ServerAddress,
    ) -> Option<Vec<u8>> {
        let pending = requests
            .find(addr)
            .and_then(|request| request.challenge_state().as_ref())
            .and_then(|state| state.pending())?;
        let mut payload = BinaryStream::new();
        payload.write_u8(LogOnStatus::LoginChallengeIssued as u8);
        let _ = payload.write_string(pending.challenge_type());
        if !pending.challenge().write_challenge_to_stream(&mut payload) {
            eprintln!(
                "LoginApp::sendChallengeReply: Failed to stream challenge data (\"{}\") to bundle.",
                pending.challenge_type()
            );
            return None;
        }
        Some(payload.as_bytes().to_vec())
    }

    /// `sendSuccess` (`loginapp.cpp:1320`): the logged-on payload. The
    /// encrypted form the reference picks when the client supplied an
    /// encryption key is the BW-06-03 connection-encryption item; this is
    /// the clear-text path.
    fn send_success(&self, source: &Address, reply_id: ReplyID, request: &ClientLoginRequest) {
        let mut payload = BinaryStream::new();
        write_success_reply(&mut payload, request);
        self.users_count.set(self.users_count.get() + 1);
        self.send_raw_reply(source, reply_id, payload.as_bytes());
    }

    /// `handleFailure` (`loginapp.cpp:604`): count the failure, respect the
    /// reply throttle, and send the status payload.
    fn handle_failure(
        &self,
        source: &Address,
        reply_id: ReplyID,
        status: u8,
        message: Option<&str>,
        erase: bool,
    ) {
        let now = (self.clock)();
        let mut st = self.state.borrow_mut();
        st.num_login_failures += 1;

        // Reset replies counter each interval (0.5 sec) (loginapp.cpp:619-624).
        if st.fail_replies_reset_at <= now {
            st.fail_replies_reset_at = now + STAMPS_PER_SECOND / 2;
            st.fail_replies_left = self.config.max_replies_on_fail_per_second / 2;
        }

        if st.fail_replies_left > 0 {
            st.fail_replies_left -= 1;
            eprintln!(
                "LoginApp::handleFailure: LogOn for {} failed, LogOnStatus: {}, description '{}'.",
                source.c_str(),
                status,
                message.unwrap_or("none")
            );
            drop(st);
            let mut payload = BinaryStream::new();
            write_failure(&mut payload, status, message);
            let units = self.send_bundle(source, reply_id, payload.as_bytes());
            if units > WARN_BUNDLE_PACKETS {
                eprintln!(
                    "LoginApp::handleFailure: Sent LogOnStatus {} with size larger than {} packets.",
                    status, WARN_BUNDLE_PACKETS
                );
            }
            // Erase the mapping for this attempt if appropriate
            // (loginapp.cpp:666-669).
            if erase {
                self.state
                    .borrow_mut()
                    .requests
                    .erase(server_address_of(source));
            }
            return;
        }

        if erase {
            st.requests.erase(server_address_of(source));
        }
    }

    /// `sendRawReply` (`loginapp.cpp:1367`): an unreliable reply carrying
    /// the payload.
    fn send_raw_reply(&self, source: &Address, reply_id: ReplyID, payload: &[u8]) {
        let units = self.send_bundle(source, reply_id, payload);
        if units > WARN_BUNDLE_PACKETS {
            eprintln!(
                "LoginApp::sendRawReply: Sent reply with size larger than {} > {} packets.",
                units, WARN_BUNDLE_PACKETS
            );
        }
    }

    /// The off-channel half of `handleFailure`'s and `sendRawReply`'s sends:
    /// the unreliable reply bundle. Answers the number of data units the
    /// bundle took, for the callers' size warnings.
    fn send_bundle(&self, source: &Address, reply_id: ReplyID, payload: &[u8]) -> usize {
        let Some(interface) = self.interface.upgrade() else {
            return 0;
        };
        let mut bundle = UdpBundle::new(0);
        bundle.start_reply(reply_id, ReliableType::No);
        bundle.write_bytes(payload);
        interface.send(source, &mut bundle, None);
        bundle.num_data_units()
    }
}

/// The client address in `loginRequests_` terms: the wire address with a
/// zero salt (the salt only the DBApp's reply record carries).
fn server_address_of(source: &Address) -> ServerAddress {
    ServerAddress {
        ip: source.ip,
        port: source.port,
        salt: 0,
    }
}

/// The system clock in nanosecond stamps.
fn system_stamps() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

/// The `login` element's handler: `LoginApp::login`.
pub struct LoginRequestHandler(pub Rc<LoginFaces>);

impl InputMessageHandler for LoginRequestHandler {
    fn handle_message(
        &self,
        source: &Address,
        header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        self.0.login(source, header, data);
    }
}

/// The `challengeResponse` element's handler.
pub struct ChallengeResponseHandler(pub Rc<LoginFaces>);

impl InputMessageHandler for ChallengeResponseHandler {
    fn handle_message(
        &self,
        source: &Address,
        _header: &mut UnpackedMessageHeader,
        data: &mut BinaryStream,
    ) {
        self.0.challenge_response(source, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db_login::{
        DB_APP_LOG_ON_ELEMENT_ID, DB_NON_PROXY_ERROR, DB_UNELABORATED_ERROR,
        LOGIN_REPLY_RECORD_STREAM_SIZE, write_logon_request,
    };
    use crate::login_challenge::{CHALLENGE_DELAY, LoginChallenge};
    use crate::login_request::LoginReplyRecord;
    use crate::loginapp::{ClientServerProtocolVersion, LOG_ON_PARAMS_PASS_THRU};
    use mercury::event_dispatcher::EventDispatcher;
    use mercury::interface_element::REPLY_MESSAGE_IDENTIFIER;
    use mercury::misc::Reason;
    use mercury::network_interface::InterfaceType;
    use mercury::packet::{HEADER_SIZE, flags};
    use mercury::request_manager::RequestManager;

    const LOOPBACK: u32 = 0x0100_007f;
    /// One second into the port clock; nanosecond stamps.
    const NOW: u64 = 1_000_000_000;

    /// A client socket bound on the loopback, as the login client stands
    /// on the wire.
    fn client_socket() -> std::net::UdpSocket {
        let client = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind the client");
        client
            .set_read_timeout(Some(std::time::Duration::from_millis(300)))
            .expect("arm the client timeout");
        client
    }

    /// The client socket's address in Mercury terms.
    fn client_address(client: &std::net::UdpSocket) -> Address {
        let addr = client.local_addr().expect("client address");
        Address::new(
            platform::socket::inet_aton("127.0.0.1").expect("loopback parses"),
            platform::socket::htons(addr.port()),
        )
    }

    /// Try to read one datagram; `None` after the timeout.
    fn recv(client: &std::net::UdpSocket) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 16384];
        match client.recv_from(&mut buf) {
            Ok((n, _)) => Some(buf[..n].to_vec()),
            Err(_) => None,
        }
    }

    /// The reply frame's body: `[flags 2][reply id byte][4-byte length]
    /// [4-byte reply id][body]`, the length counting the reply id with the
    /// body. Answers `(reply id, body)`, the body copied out.
    fn reply_frame(reply: &[u8]) -> (i32, Vec<u8>) {
        assert_eq!(
            u16::from_le_bytes(reply[0..2].try_into().unwrap()),
            0,
            "an off-channel reply carries no flags"
        );
        assert_eq!(reply[2], REPLY_MESSAGE_IDENTIFIER, "the reply id byte");
        let length = u32::from_le_bytes(reply[3..7].try_into().unwrap()) as usize;
        let reply_id = i32::from_le_bytes(reply[7..11].try_into().unwrap());
        let body = length - 4;
        (reply_id, reply[11..11 + body].to_vec())
    }

    /// The first payload byte: the `LogOnStatus`.
    fn reply_status(body: &[u8]) -> u8 {
        body[0]
    }

    /// The description string after the status byte, when one is there.
    fn reply_text(body: &[u8]) -> Option<String> {
        let mut rest = BinaryStream::from_slice(&body[1..]);
        rest.read_string().ok()
    }

    /// `LogOnParams::with_credentials` with a nonce, as a client builds it.
    fn params(user: &str, password: &str, key: &str) -> LogOnParams {
        let mut p = LogOnParams::with_credentials(user, password, key);
        p.set_nonce(7);
        p
    }

    /// The login body after the version: version bytes then the params
    /// streamed the way the PASS_THRU encoder does.
    fn login_payload(p: &LogOnParams) -> Vec<u8> {
        let mut s = BinaryStream::new();
        ClientServerProtocolVersion::current().write_to_stream(&mut s);
        p.add_to_stream(&mut s, LOG_ON_PARAMS_PASS_THRU);
        s.as_bytes().to_vec()
    }

    /// Hand one login body to the `login` face under `reply_id`.
    fn drive_login(faces: &LoginFaces, addr: &Address, reply_id: i32, payload: &[u8]) {
        let mut header = UnpackedMessageHeader::new(0);
        header.set_length(payload.len() as i32);
        header.set_reply_id(reply_id);
        let mut data = BinaryStream::from_slice(payload);
        faces.login(addr, &mut header, &mut data);
    }

    /// Hand one challenge answer to the `challengeResponse` face.
    fn drive_challenge_response(faces: &LoginFaces, addr: &Address, payload: &[u8]) {
        let mut data = BinaryStream::from_slice(payload);
        faces.challenge_response(addr, &mut data);
    }

    /// The client's challenge answer: the calculation duration the
    /// challengeResponse face reads first, then the echoed duration the
    /// delay challenge verifies.
    fn challenge_answer(echo: f32) -> Vec<u8> {
        let mut s = BinaryStream::new();
        s.write_f32(0.001);
        s.write_f32(echo);
        s.as_bytes().to_vec()
    }

    /// A DBApp reply carrying a failure status and, when `Some`, the
    /// description string.
    fn db_failure(status: u8, message: Option<&str>) -> Vec<u8> {
        let mut s = BinaryStream::new();
        s.write_u8(status);
        if let Some(message) = message {
            let _ = s.write_string(message);
        }
        s.as_bytes().to_vec()
    }

    /// A DBApp ban reply: the status and the absolute expiry as a string,
    /// which `istream >> time_t` parses.
    fn db_ban(expiry_epoch: i64) -> Vec<u8> {
        let mut s = BinaryStream::new();
        s.write_u8(LogOnStatus::LoginRejectedIpAddressBan as u8);
        let _ = s.write_string(&expiry_epoch.to_string());
        s.as_bytes().to_vec()
    }

    /// A DBApp success reply: the status, the record and the server message.
    fn db_success_bytes(record: &LoginReplyRecord, server_msg: &str) -> Vec<u8> {
        let mut s = BinaryStream::new();
        s.write_u8(LogOnStatus::LoggedOn as u8);
        record.write_to_stream(&mut s);
        let _ = s.write_string(server_msg);
        s.as_bytes().to_vec()
    }

    /// A DBApp success reply whose record is short by `missing` bytes.
    fn db_success_short(missing: usize) -> Vec<u8> {
        let mut s = BinaryStream::new();
        s.write_u8(LogOnStatus::LoggedOn as u8);
        let full = LOGIN_REPLY_RECORD_STREAM_SIZE;
        let stub = vec![0u8; full - missing];
        s.write_bytes(&stub);
        s.as_bytes().to_vec()
    }

    /// A login challenge that refuses to stream itself, to drive
    /// `sendChallengeReply`'s failure arm.
    struct DecliningChallenge;

    impl LoginChallenge for DecliningChallenge {
        fn write_challenge_to_stream(&self, _stream: &mut BinaryStream) -> bool {
            false
        }
        fn read_challenge_from_stream(&mut self, _stream: &mut BinaryStream) -> bool {
            false
        }
        fn write_response_to_stream(&self, _stream: &mut BinaryStream) -> bool {
            false
        }
        fn read_response_from_stream(&mut self, _stream: &mut BinaryStream) -> bool {
            false
        }
    }

    /// The faces over a live interface, a frozen clock, and a recording
    /// DBApp leg. The interface answers who the replies reach.
    struct Harness {
        faces: Rc<LoginFaces>,
        clock: Rc<Cell<u64>>,
        forward: Rc<RecordingLogOnForward>,
        users: Rc<Cell<u32>>,
    }

    fn harness_with(config: LoginFacesConfig, interface: Weak<NetworkInterface>) -> Harness {
        let clock = Rc::new(Cell::new(NOW));
        let clock_fn: Rc<dyn Fn() -> u64> = {
            let cell = Rc::clone(&clock);
            Rc::new(move || cell.get())
        };
        let forward = Rc::new(RecordingLogOnForward::default());
        let sink: Rc<dyn LogOnForward> = forward.clone();
        let users = Rc::new(Cell::new(0));
        let faces = Rc::new(LoginFaces::with_clock(
            config,
            interface,
            Rc::clone(&users),
            sink,
            clock_fn,
        ));
        Harness {
            faces,
            clock,
            forward,
            users,
        }
    }

    /// A harness whose replies reach a live interface.
    fn harness(config: LoginFacesConfig) -> (Harness, Rc<NetworkInterface>) {
        let interface = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1").0;
        (harness_with(config, Rc::downgrade(&interface)), interface)
    }

    /// A harness without an interface: `send_bundle`'s upgrade fails.
    fn detached(config: LoginFacesConfig) -> Harness {
        harness_with(config, Weak::new())
    }

    /// The defaults with `allowLogin` and `verboseLoginFailures` on, what
    /// a dev-configured loginapp runs with.
    fn open_config() -> LoginFacesConfig {
        let mut config = LoginFacesConfig::new();
        config.gate.allow_login = true;
        config.gate.verbose_login_failures = true;
        config
    }

    fn config_with_challenge(challenge_type: &str) -> LoginFacesConfig {
        let mut config = open_config();
        config.challenge_type = challenge_type.to_string();
        config
    }

    // -- configuration -------------------------------------------------

    /// The `loginapp_config.cpp` defaults, as `LoginFacesConfig::new`
    /// keeps them.
    #[test]
    fn defaults_match_the_reference() {
        let config = LoginFacesConfig::default();
        assert_eq!(config.challenge_type, "");
        assert!(!config.passwordless_logins_only);
        assert_eq!(config.max_replies_on_fail_per_second, 100);
        assert!(!config.gate.allow_login, "allowLogin defaults false");
        assert!(!config.gate.allow_unencrypted_logins);
        assert_eq!(config.gate.max_username_length, 256);
        assert_eq!(config.gate.max_password_length, 256);
        assert_eq!(config.gate.max_login_message_size, 1472);
        assert_eq!(config.gate.ip_ban_cleanup_interval_seconds, 10.0);
        assert_eq!(config.gate.login_rate_limit, 0);
        // The faces hand the config back unchanged.
        let harness = detached(LoginFacesConfig::new());
        assert_eq!(harness.faces.config().challenge_type, "");
    }

    /// A `loginApp` section overrides every default the faces read.
    #[test]
    fn a_configured_login_app_section_overrides_the_defaults() {
        static TEMP_COUNTER: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let tag = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("login-faces-config-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("server")).expect("mkdir the config root");
        std::fs::write(
            root.join("server/bw.xml"),
            "<root>\n\
             \t<loginApp>\n\
             \t\t<allowLogin>true</allowLogin>\n\
             \t\t<allowUnencryptedLogins>true</allowUnencryptedLogins>\n\
             \t\t<verboseLoginFailures>true</verboseLoginFailures>\n\
             \t\t<loginRateLimit>7</loginRateLimit>\n\
             \t\t<rateLimitDuration>10</rateLimitDuration>\n\
             \t\t<maxUsernameLength>32</maxUsernameLength>\n\
             \t\t<maxPasswordLength>33</maxPasswordLength>\n\
             \t\t<maxLoginMessageSize>1400</maxLoginMessageSize>\n\
             \t\t<ipBanListCleanupInterval>20</ipBanListCleanupInterval>\n\
             \t\t<maxRepliesOnFailPerSecond>50</maxRepliesOnFailPerSecond>\n\
             \t\t<passwordlessLoginsOnly>true</passwordlessLoginsOnly>\n\
             \t\t<challengeType>delay</challengeType>\n\
             \t</loginApp>\n\
             </root>\n",
        )
        .expect("write the fixture bw.xml");
        let resource = resources::bwresource::BwResource::init_paths(&[&root.to_string_lossy()]);
        let mut config = crate::config::BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        let faces = LoginFacesConfig::from_config(&config);
        assert!(faces.gate.allow_login);
        assert!(faces.gate.allow_unencrypted_logins);
        assert!(faces.gate.verbose_login_failures);
        assert_eq!(faces.gate.login_rate_limit, 7);
        assert_eq!(faces.gate.rate_limit_duration_seconds, 10.0);
        assert_eq!(faces.gate.max_username_length, 32);
        assert_eq!(faces.gate.max_password_length, 33);
        assert_eq!(faces.gate.max_login_message_size, 1400);
        assert_eq!(faces.gate.ip_ban_cleanup_interval_seconds, 20.0);
        assert_eq!(faces.max_replies_on_fail_per_second, 50);
        assert!(faces.passwordless_logins_only);
        assert_eq!(faces.challenge_type, "delay");
    }

    // -- handler wrappers ----------------------------------------------

    /// The registered handlers are thin delegations to their faces
    /// (`LoginApp::login` / `challengeResponse`'s message entries).
    #[test]
    fn the_handlers_delegate_to_their_faces() {
        let harness = detached(LoginFacesConfig::new());
        let login: Rc<dyn InputMessageHandler> =
            Rc::new(LoginRequestHandler(Rc::clone(&harness.faces)));
        let challenge: Rc<dyn InputMessageHandler> =
            Rc::new(ChallengeResponseHandler(Rc::clone(&harness.faces)));

        let addr = Address::new(LOOPBACK, 40000);
        let mut header = UnpackedMessageHeader::new(0);
        header.set_length(0);
        header.set_reply_id(5);
        login.handle_message(&addr, &mut header, &mut BinaryStream::new());
        assert_eq!(
            harness.faces.num_login_failures(),
            1,
            "the login handler drove the face"
        );

        let mut header = UnpackedMessageHeader::new(2);
        header.set_length(0);
        challenge.handle_message(&addr, &mut header, &mut BinaryStream::new());
        assert_eq!(harness.faces.num_login_failures(), 1, "no reply, no count");
    }

    // -- pre-version gates ----------------------------------------------

    /// `allowLogin` false rejects with `LOGIN_REJECTED_LOGINS_NOT_ALLOWED`
    /// and no description (loginapp.cpp:721-736).
    #[test]
    fn logins_not_allowed_rejects_with_a_reply() {
        let client = client_socket();
        let (harness, _interface) = harness(LoginFacesConfig::new());
        let addr = client_address(&client);

        drive_login(
            &harness.faces,
            &addr,
            4242,
            &login_payload(&params("u", "p", "k")),
        );
        let (reply_id, body) = reply_frame(&recv(&client).expect("the refusal replies"));
        assert_eq!(reply_id, 4242, "the request's reply id");
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedLoginsNotAllowed as u8
        );
        assert!(reply_text(&body).is_none(), "no description");
        assert_eq!(harness.faces.num_login_failures(), 1);
        assert_eq!(harness.faces.num_login_attempts(), 0, "refused at the gate");
        assert!(harness.forward.0.borrow().is_empty());
    }

    /// A current ban rejects with the bare status
    /// (loginapp.cpp:738-758).
    #[test]
    fn a_banned_address_rejects_with_a_reply() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness
            .faces
            .ban_ip(LOOPBACK, NOW + 1000 * STAMPS_PER_SECOND);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("u", "p", "k")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the ban refusal replies"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedIpAddressBan as u8
        );
        assert!(reply_text(&body).is_none(), "no description");
        assert_eq!(harness.faces.num_login_failures(), 1);
    }

    /// An expired ban is dropped at the check and the login lands on the
    /// next gate instead (loginapp.cpp:741-744).
    #[test]
    fn an_expired_ban_passes_and_lands_on_the_next_gate() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.ban_ip(LOOPBACK, NOW - 1);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("u", "p", "k")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the next gate replies"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedDbNotReady as u8,
            "the ban expired; the DB-not-ready gate answers"
        );
        assert!(reply_text(&body).is_none());
    }

    /// A spoofed empty address is dropped without a reply
    /// (loginapp.cpp:779-789).
    #[test]
    fn a_spoofed_empty_address_is_dropped_without_a_reply() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = Address::new(0, 40000);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("u", "p", "k")),
        );
        assert!(recv(&client).is_none(), "no reply for a spoofed address");
        assert_eq!(harness.faces.num_login_failures(), 0);
        assert_eq!(harness.faces.num_login_attempts(), 0);
    }

    // -- version and post-version gates ---------------------------------

    /// A body without a version is `LOGIN_MALFORMED_REQUEST`
    /// (loginapp.cpp:819-833).
    #[test]
    fn a_body_without_a_version_is_malformed() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);

        drive_login(&harness.faces, &addr, 0, &[]);
        let (_, body) = reply_frame(&recv(&client).expect("the malformed reply"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginMalformedRequest as u8
        );
        assert_eq!(harness.faces.num_login_attempts(), 1, "counted first");
    }

    /// A version the server does not support is refused
    /// (loginapp.cpp:835-847).
    #[test]
    fn an_unsupported_protocol_version_is_refused() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        let mut payload = BinaryStream::new();
        ClientServerProtocolVersion::new(1, 9, 0, 0).write_to_stream(&mut payload);

        drive_login(&harness.faces, &addr, 0, payload.as_bytes());
        let (_, body) = reply_frame(&recv(&client).expect("the version refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginBadProtocolVersion as u8
        );
    }

    /// A login before the DBApp registers is refused with
    /// `LOGIN_REJECTED_DB_NOT_READY` (loginapp.cpp:863-874).
    #[test]
    fn a_login_before_the_dbapp_registers_is_refused() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        assert!(!harness.faces.db_ready(), "the default state");

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("user", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the not-ready refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedDbNotReady as u8
        );
        assert!(harness.forward.0.borrow().is_empty());
    }

    /// A rate-limited login is refused after the quota is spent
    /// (loginapp.cpp:846-861): the first login consumes the one allowed,
    /// the second is rejected before the DBApp-ready gate.
    #[test]
    fn a_rate_limited_login_is_refused() {
        let client = client_socket();
        let mut config = open_config();
        config.gate.login_rate_limit = 1;
        config.gate.rate_limit_duration_seconds = 10.0;
        let limited = client_socket();
        let (harness, _interface) = harness(config);
        let addr = client_address(&client);
        let other = client_address(&limited);
        harness.faces.set_db_ready(true);
        let payload = login_payload(&params("user", "pw", "key"));

        drive_login(&harness.faces, &addr, 1, &payload);
        assert_eq!(harness.forward.0.borrow().len(), 1, "the first passes");
        drive_login(&harness.faces, &other, 2, &payload);
        let (_, body) = reply_frame(&recv(&limited).expect("the rate-limit refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedRateLimited as u8
        );
        assert_eq!(harness.forward.0.borrow().len(), 1, "the second stops");
        assert!(recv(&client).is_none(), "the first stays silent");
    }

    /// An overload a DBApp failure leaves behind rejects further logins
    /// until one second after it was noted (loginapp.cpp:876-902,
    /// database_reply_handler.cpp:103-111).
    #[test]
    fn an_overload_from_the_dbapp_rejects_until_it_expires() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let payload = login_payload(&params("user", "pw", "key"));

        drive_login(&harness.faces, &addr, 1, &payload);
        assert_eq!(harness.forward.0.borrow().len(), 1);
        let failure = db_failure(LogOnStatus::LoginRejectedCellAppOverload as u8, None);
        harness
            .faces
            .db_reply(&addr, &mut BinaryStream::from_slice(&failure));
        let (_, body) = reply_frame(&recv(&client).expect("the failure reply"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedCellAppOverload as u8
        );

        drive_login(&harness.faces, &addr, 2, &payload);
        let (_, body) = reply_frame(&recv(&client).expect("the overload refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedCellAppOverload as u8,
            "the remembered overload answers"
        );
        assert_eq!(harness.forward.0.borrow().len(), 1);

        harness.clock.set(NOW + STAMPS_PER_SECOND + 1);
        drive_login(&harness.faces, &addr, 3, &payload);
        assert_eq!(harness.forward.0.borrow().len(), 2, "the overload expired");
    }

    /// The failure-reply throttle: half of `maxRepliesOnFailPerSecond`
    /// replies per half-second window (loginapp.cpp:619-637), refilled by
    /// the clock.
    #[test]
    fn the_throttle_holds_back_failure_replies_half_a_second() {
        let client = client_socket();
        let mut config = open_config();
        config.max_replies_on_fail_per_second = 2;
        let (harness, _interface) = harness(config);
        let addr = client_address(&client);
        let payload = login_payload(&params("user", "pw", "key"));

        drive_login(&harness.faces, &addr, 1, &payload);
        assert!(recv(&client).is_some(), "the first failure replies");
        drive_login(&harness.faces, &addr, 2, &payload);
        assert!(
            recv(&client).is_none(),
            "the throttled window sends no reply"
        );
        harness.clock.set(NOW + STAMPS_PER_SECOND / 2);
        drive_login(&harness.faces, &addr, 3, &payload);
        assert!(recv(&client).is_some(), "the refilled window replies");
        assert_eq!(harness.faces.num_login_failures(), 3, "all three counted");
    }

    // -- the challenge gate ----------------------------------------------

    /// The delay challenge is issued, answered, verified, and the retried
    /// login proceeds to the DBApp leg (loginapp.cpp:1048-1127).
    #[test]
    fn a_challenge_is_issued_then_verified_then_the_login_proceeds() {
        let client = client_socket();
        let (harness, _interface) = harness(config_with_challenge(CHALLENGE_DELAY));
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the challenge reply"));
        assert_eq!(reply_status(&body), LogOnStatus::LoginChallengeIssued as u8);
        let mut rest = BinaryStream::from_slice(&body[1..]);
        assert_eq!(
            rest.read_string().expect("the challenge type"),
            CHALLENGE_DELAY
        );
        assert_eq!(
            rest.read_f32().expect("the delay"),
            1.0,
            "the default delay duration"
        );
        assert!(
            harness.faces.with_requests(|r| r
                .find(server_address_of(&addr))
                .unwrap()
                .has_pending_challenge()),
            "the issued challenge is pending"
        );

        drive_challenge_response(&harness.faces, &addr, &challenge_answer(1.0));
        assert!(
            harness.faces.with_requests(|r| !r
                .find(server_address_of(&addr))
                .unwrap()
                .has_pending_challenge()),
            "the verified challenge cleared"
        );

        drive_login(
            &harness.faces,
            &addr,
            2,
            &login_payload(&params("user", "pw", "key")),
        );
        assert_eq!(
            harness.forward.0.borrow().len(),
            1,
            "the login proceeds to the DBApp leg"
        );
        assert!(recv(&client).is_none(), "no challenge again");
    }

    /// A duplicate during a pending challenge is answered twice: the
    /// re-attempt path re-sends it and, because the login continues, the
    /// challenge gate sends it again (loginapp.cpp:836-844 returns false
    /// into processForLoginChallenge's resend arm).
    #[test]
    fn a_dupe_during_a_pending_challenge_gets_the_challenge_twice() {
        let client = client_socket();
        let (harness, _interface) = harness(config_with_challenge(CHALLENGE_DELAY));
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let payload = login_payload(&params("user", "pw", "key"));

        drive_login(&harness.faces, &addr, 1, &payload);
        assert!(recv(&client).is_some(), "the first challenge");
        drive_login(&harness.faces, &addr, 2, &payload);
        let first = recv(&client).expect("the resent challenge");
        let second = recv(&client).expect("the challenge gate's resend");
        assert_eq!(
            reply_frame(&first).1,
            reply_frame(&second).1,
            "two identical challenge replies"
        );
        assert_eq!(harness.faces.num_login_attempts(), 2);
        assert!(harness.forward.0.borrow().is_empty());
    }

    /// A failed challenge marks the attempt, and the retry is refused
    /// with `LOGIN_REJECTED_CHALLENGE_ERROR` and the cached-failure text
    /// (loginapp.cpp:1057-1065).
    #[test]
    fn a_failed_challenge_marks_the_attempt_and_the_retry_is_refused() {
        let client = client_socket();
        let (harness, _interface) = harness(config_with_challenge(CHALLENGE_DELAY));
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        assert!(recv(&client).is_some());
        drive_challenge_response(&harness.faces, &addr, &challenge_answer(0.5));
        assert!(
            harness.faces.with_requests(|r| r
                .find(server_address_of(&addr))
                .unwrap()
                .did_fail_challenge()),
            "the failure is marked on the request"
        );
        assert!(
            recv(&client).is_none(),
            "the challengeResponse face itself sends nothing"
        );

        drive_login(
            &harness.faces,
            &addr,
            2,
            &login_payload(&params("user", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the challenge-error refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedChallengeError as u8
        );
        assert_eq!(reply_text(&body).as_deref(), Some("Failed login challenge"));
    }

    /// A challenge type no factory knows refuses without creating a
    /// loginRequests_ entry (loginapp.cpp:1088-1101).
    #[test]
    fn an_unknown_challenge_type_is_refused_without_a_cache_entry() {
        let client = client_socket();
        let (harness, _interface) = harness(config_with_challenge("no_such_challenge"));
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("user", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the factory refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedChallengeError as u8
        );
        assert_eq!(
            reply_text(&body).as_deref(),
            Some("Failed to instantiate login challenge.")
        );
        assert!(
            harness.faces.with_requests(|r| r.is_empty()),
            "no entry for a failed factory"
        );
    }

    /// A challenge that cannot stream itself is reported as a send failure
    /// (loginapp.cpp:1358-1367), and the request stays cached.
    #[test]
    fn a_challenge_that_cannot_stream_reports_send_failure() {
        let client = client_socket();
        let (harness, _interface) = harness(config_with_challenge(CHALLENGE_DELAY));
        let addr = client_address(&client);
        harness.faces.with_requests(|r| {
            r.entry(server_address_of(&addr))
                .set_login_challenge(CHALLENGE_DELAY, Box::new(DecliningChallenge));
        });

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("user", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the send-failure reply"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedChallengeError as u8
        );
        assert_eq!(
            reply_text(&body).as_deref(),
            Some("Failed to send challenge data.")
        );
        assert!(
            harness.faces.with_requests(|r| r.len() == 1),
            "no erase without a params snapshot"
        );
        // The challenge never got far enough for the server to read a
        // challenge from a stream or to judge a response; the remaining
        // trait faces are stubbed to refuse, pinned here.
        let mut challenge = DecliningChallenge;
        let mut stream = BinaryStream::new();
        assert!(!challenge.read_challenge_from_stream(&mut stream));
        assert!(!challenge.write_response_to_stream(&mut stream));
        assert!(!challenge.read_response_from_stream(&mut stream));
    }

    /// A challenge answer with no request behind it is a timeout: drained,
    /// no reply (loginapp.cpp:1199-1206).
    #[test]
    fn a_challenge_response_without_a_request_is_a_timeout() {
        let client = client_socket();
        let (harness, _interface) = harness(config_with_challenge(CHALLENGE_DELAY));
        let addr = client_address(&client);

        drive_challenge_response(&harness.faces, &addr, &challenge_answer(1.0));
        assert!(recv(&client).is_none());
    }

    /// A second challenge answer after the verify is a re-send: drained,
    /// no reply (loginapp.cpp:1208-1217).
    #[test]
    fn an_already_verified_challenge_response_is_drained() {
        let client = client_socket();
        let (harness, _interface) = harness(config_with_challenge(CHALLENGE_DELAY));
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the challenge"));
        assert_eq!(reply_status(&body), LogOnStatus::LoginChallengeIssued as u8);
        // The first answer verifies (the echo matches the duration), the
        // second is the re-send.
        drive_challenge_response(&harness.faces, &addr, &challenge_answer(1.0));
        drive_challenge_response(&harness.faces, &addr, &challenge_answer(1.0));
        assert!(
            recv(&client).is_none(),
            "a verified-again answer draws no reply"
        );
        harness.faces.with_requests(|r| {
            let request = r
                .find_mut(server_address_of(&addr))
                .expect("the request stays cached");
            let state = request.challenge_state().as_ref().expect("the state stays");
            assert!(state.pending().is_none(), "the challenge was taken");
        });
    }

    /// An answer arriving for a request that never issued a challenge --
    /// the entry exists but its challenge state does not -- is the free
    /// function's `TimedOut`: drained, no reply
    /// (`login_challenge.rs` `challenge_response`, loginapp.cpp:1199-1206).
    #[test]
    fn a_challenge_answer_for_a_challengeless_entry_is_timed_out() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        assert!(recv(&client).is_none(), "the accepted login stays silent");

        drive_challenge_response(&harness.faces, &addr, &challenge_answer(1.0));
        assert!(recv(&client).is_none(), "the answer draws no reply");
        assert_eq!(harness.faces.num_login_failures(), 0);
    }

    /// A second attempt while the first is still in flight is ignored
    /// outright (loginapp.cpp:836-844, `handleResentPendingAttempt`): no
    /// reply, no second DBApp forward, and the rate gate is never asked.
    #[test]
    fn a_duplicate_while_in_flight_is_ignored() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let payload = login_payload(&params("user", "pw", "key"));

        drive_login(&harness.faces, &addr, 1, &payload);
        drive_login(&harness.faces, &addr, 2, &payload);
        assert!(recv(&client).is_none(), "the duplicate draws no reply");
        assert_eq!(harness.faces.num_login_attempts(), 2, "both count");
        assert_eq!(harness.faces.num_login_failures(), 0);
        assert_eq!(
            harness.forward.0.borrow().len(),
            1,
            "only the first attempt is forwarded"
        );
    }

    /// A DBApp ban whose expiry already passed carries zero remaining
    /// seconds: the banning INFO stays silent but the failure still goes
    /// out with the standard IP-ban text, and the attempt is erased
    /// (loginapp.cpp:688-695, `database_reply_handler.cpp:120`).
    #[test]
    fn an_expired_dbapp_ban_still_answers_with_the_ban_text() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        // Expiry one second before the epoch second the frozen clock
        // reports: remaining clamps to zero.
        harness.faces.db_reply(
            &addr,
            &mut BinaryStream::from_slice(&db_ban((NOW / STAMPS_PER_SECOND) as i64 - 5)),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the ban refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedIpAddressBan as u8
        );
        assert_eq!(reply_text(&body), Some(IP_BAN_MESSAGE.to_string()));
        assert!(
            harness.faces.with_requests(|r| r.is_empty()),
            "the attempt is erased"
        );
        assert_eq!(harness.faces.num_login_failures(), 1);
    }

    /// A DBApp shutdown while an attempt is outstanding drops it with no
    /// reply: the in-flight slot stays, so the address is still recognised
    /// as a duplicate (database_reply_handler.cpp:190).
    #[test]
    fn a_dbapp_shutdown_drops_the_attempt_without_a_reply() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        harness.faces.db_shutting_down(&addr);
        assert!(recv(&client).is_none(), "the shutdown answers nothing");
        assert_eq!(harness.faces.num_login_failures(), 0);
        harness.faces.with_requests(|r| {
            assert_eq!(r.len(), 1, "the attempt stays cached");
        });
    }

    /// A no-reply notice for an address with nothing in flight is a
    /// stray: dropped without a failure count
    /// (`database_reply_handler.cpp:176`).
    #[test]
    fn a_stray_no_reply_notice_is_dropped() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);

        harness.faces.db_no_reply(&addr);
        assert!(recv(&client).is_none());
        assert_eq!(harness.faces.num_login_failures(), 0);
    }

    /// Once the failure-reply budget for the interval is spent, a failure
    /// still erases the attempt but sends nothing
    /// (loginapp.cpp:656-669): the erase sits outside the reply branch.
    #[test]
    fn an_erasing_failure_with_a_spent_budget_still_erases() {
        let client = client_socket();
        let mut config = open_config();
        config.max_replies_on_fail_per_second = 2; // a budget of one reply
        let (harness, _interface) = harness(config);
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        harness.faces.db_no_reply(&addr);
        assert!(recv(&client).is_some(), "the first failure replies");

        drive_login(
            &harness.faces,
            &addr,
            2,
            &login_payload(&params("user", "pw", "key")),
        );
        harness.faces.db_no_reply(&addr);
        assert!(
            recv(&client).is_none(),
            "the budget is spent, the second failure stays silent"
        );
        assert_eq!(harness.faces.num_login_failures(), 2);
        assert!(
            harness.faces.with_requests(|r| r.is_empty()),
            "the exhausted path erases too"
        );
    }

    /// A success reply whose server message is huge spans several
    /// datagrams; the reply still leaves and the login still counts
    /// (`loginapp.cpp:1367-1372`, `sendRawReply`'s size warning).
    #[test]
    fn an_oversized_success_reply_still_leaves() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let record = LoginReplyRecord::new(crate::login_request::ServerAddress::NONE, 7);
        let big_message = "x".repeat(6000);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        harness.faces.db_reply(
            &addr,
            &mut BinaryStream::from_slice(&db_success_bytes(&record, &big_message)),
        );
        assert!(
            recv(&client).is_some(),
            "the oversized reply leaves, one datagram per packet"
        );
        assert_eq!(harness.users.get(), 1, "gNumLogins");
        assert_eq!(harness.faces.num_login_failures(), 0);
    }

    // -- the logon-parameter gates ----------------------------------------

    /// Credentials longer than `maxLoginMessageSize` are malformed
    /// (loginapp.cpp:908-919).
    #[test]
    fn too_long_credentials_blob_is_malformed() {
        let client = client_socket();
        let mut config = open_config();
        config.gate.max_login_message_size = 4;
        let (harness, _interface) = harness(config);
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("user", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the bound refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginMalformedRequest as u8
        );
    }

    /// Parameters that do not destream are malformed
    /// (loginapp.cpp:940-953).
    #[test]
    fn unparsable_params_are_malformed() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let mut payload = BinaryStream::new();
        ClientServerProtocolVersion::current().write_to_stream(&mut payload);
        // The flags claim every field; nothing follows to read them from.
        payload.write_bytes(&[0xff, 0xff, 0xff, 0xff, 1, 2, 3]);

        drive_login(&harness.faces, &addr, 0, payload.as_bytes());
        let (_, body) = reply_frame(&recv(&client).expect("the malformed reply"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginMalformedRequest as u8
        );
    }

    /// A username over `maxUsernameLength` is malformed
    /// (loginapp.cpp:921-939).
    #[test]
    fn a_too_long_username_is_malformed() {
        let client = client_socket();
        let mut config = open_config();
        config.gate.max_username_length = 4;
        let (harness, _interface) = harness(config);
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("toolong", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the length refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginMalformedRequest as u8
        );
    }

    /// A login without an encryption key is malformed while
    /// `allowUnencryptedLogins` is false (loginapp.cpp:984-994).
    #[test]
    fn a_missing_encryption_key_is_malformed_when_unencrypted_is_disallowed() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("user", "pw", "")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the key refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginMalformedRequest as u8
        );
    }

    /// With `allowUnencryptedLogins` the keyless login proceeds
    /// (loginapp.cpp:984-994).
    #[test]
    fn an_unencrypted_login_passes_when_allowed() {
        let client = client_socket();
        let mut config = open_config();
        config.gate.allow_unencrypted_logins = true;
        let (harness, _interface) = harness(config);
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("user", "pw", "")),
        );
        assert_eq!(harness.forward.0.borrow().len(), 1);
    }

    /// `passwordlessLoginsOnly` rejects a passworded login with
    /// `LOGIN_REJECTED_INVALID_PASSWORD` (loginapp.cpp:996-1010).
    #[test]
    fn passwordless_only_rejects_a_passworded_login() {
        let client = client_socket();
        let mut config = open_config();
        config.passwordless_logins_only = true;
        let (harness, _interface) = harness(config);
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("user", "pw", "key")),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the passwordless refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedInvalidPassword as u8
        );
    }

    // -- the DBApp leg -----------------------------------------------------

    /// An accepted login is forwarded and stays silent until the DBApp
    /// answers (loginapp.cpp:1018-1029).
    #[test]
    fn an_accepted_login_is_forwarded_without_a_reply() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        assert!(recv(&client).is_none(), "no reply before the DBApp");
        assert_eq!(harness.faces.num_login_attempts(), 1);
        let forward = harness.forward.0.borrow();
        let (sent_addr, sent_params) = &forward[0];
        let expected = server_address_of(&addr);
        assert_eq!(sent_addr.ip, expected.ip);
        assert_eq!(sent_addr.port, expected.port);
        assert_eq!(sent_addr.salt, 0, "the salt the loginRequests_ key uses");
        assert_eq!(sent_params.username(), "user");
        assert_eq!(sent_params.password(), "pw");
        assert_eq!(sent_params.encryption_key(), "key");
    }

    /// The DBApp success is cached and the client learns its seat
    /// (sendAndCacheSuccess, loginapp.cpp:1280-1310).
    #[test]
    fn the_dbapp_success_is_cached_and_the_client_learns_the_seat() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let record = LoginReplyRecord::new(
            crate::login_request::ServerAddress::new(LOOPBACK, 41000, 9),
            0xdead_beef,
        );

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        harness.faces.db_reply(
            &addr,
            &mut BinaryStream::from_slice(&db_success_bytes(&record, "Welcome!")),
        );
        let (reply_id, body) = reply_frame(&recv(&client).expect("the success reply"));
        assert_eq!(reply_id, 1);
        assert_eq!(reply_status(&body), LogOnStatus::LoggedOn as u8);
        let mut rest = BinaryStream::from_slice(&body[1..]);
        let mut wire_record = BinaryStream::new();
        record.write_to_stream(&mut wire_record);
        assert_eq!(
            rest.read_blob_raw(LOGIN_REPLY_RECORD_STREAM_SIZE)
                .expect("the record")
                .as_slice(),
            wire_record.as_bytes(),
            "the cached record reaches the client"
        );
        assert_eq!(rest.read_string().expect("the server message"), "Welcome!");

        assert_eq!(harness.users.get(), 1, "gNumLogins");
        harness.faces.with_requests(|r| {
            let request = r.find(server_address_of(&addr)).unwrap();
            assert_eq!(request.reply_record(), record);
            assert_eq!(request.server_msg(), "Welcome!");
            assert_eq!(request.creation_time(), NOW, "the reply stamps the seat");
        });
    }

    /// A re-sent login whose success is cached is answered from the cache,
    /// with no second DBApp forward (loginapp.cpp:968-975,
    /// handleResentCachedAttempt).
    #[test]
    fn a_resent_login_after_success_is_answered_from_the_cache() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let record = LoginReplyRecord::new(crate::login_request::ServerAddress::NONE, 7);
        let payload = login_payload(&params("user", "pw", "key"));

        drive_login(&harness.faces, &addr, 1, &payload);
        harness.faces.db_reply(
            &addr,
            &mut BinaryStream::from_slice(&db_success_bytes(&record, "")),
        );
        assert!(recv(&client).is_some());
        drive_login(&harness.faces, &addr, 2, &payload);
        let (_, body) = reply_frame(&recv(&client).expect("the cached success"));
        assert_eq!(reply_status(&body), LogOnStatus::LoggedOn as u8);
        assert_eq!(harness.users.get(), 2, "both successes count a login");
        assert_eq!(
            harness.forward.0.borrow().len(),
            1,
            "no second DBApp forward"
        );
    }

    /// A DBApp failure with a message is relayed and the attempt erased
    /// (database_reply_handler.cpp:84-95).
    #[test]
    fn a_dbapp_failure_message_is_relayed_and_the_attempt_erased() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        harness.faces.db_reply(
            &addr,
            &mut BinaryStream::from_slice(&db_failure(
                LogOnStatus::LoginRejectedNoSuchUser as u8,
                Some("No such user"),
            )),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the failure reply"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedNoSuchUser as u8
        );
        assert_eq!(reply_text(&body).as_deref(), Some("No such user"));
        assert!(
            harness.faces.with_requests(|r| r.is_empty()),
            "the params snapshot erases the attempt"
        );
    }

    /// A DBApp failure with no message gets the unelaborated text
    /// (database_reply_handler.cpp:95-101).
    #[test]
    fn a_dbapp_failure_without_a_message_gets_the_unelaborated_text() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        harness.faces.db_reply(
            &addr,
            &mut BinaryStream::from_slice(&db_failure(
                LogOnStatus::LoginRejectedNoSuchUser as u8,
                None,
            )),
        );
        let (_, body) = reply_frame(&recv(&client).expect("the failure reply"));
        assert_eq!(reply_text(&body).as_deref(), Some(DB_UNELABORATED_ERROR));
    }

    /// A success reply whose record lacks the session key reports the
    /// non-proxy error (database_reply_handler.cpp:125-139).
    #[test]
    fn a_short_success_record_is_the_non_proxy_error() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        harness
            .faces
            .db_reply(&addr, &mut BinaryStream::from_slice(&db_success_short(4)));
        let (_, body) = reply_frame(&recv(&client).expect("the record refusal"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginCustomDefinedError as u8
        );
        assert_eq!(reply_text(&body).as_deref(), Some(DB_NON_PROXY_ERROR));
    }

    /// A DBApp ban reply bans the address, erases the attempt, and the
    /// next login is refused at the ban gate
    /// (database_reply_handler.cpp:57-81, handleBanIP loginapp.cpp:688-695).
    #[test]
    fn a_dbapp_ban_bans_the_address_and_erases_the_attempt() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let payload = login_payload(&params("user", "pw", "key"));

        drive_login(&harness.faces, &addr, 1, &payload);
        harness
            .faces
            .db_reply(&addr, &mut BinaryStream::from_slice(&db_ban(31)));
        let (_, body) = reply_frame(&recv(&client).expect("the ban reply"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedIpAddressBan as u8
        );
        assert_eq!(
            reply_text(&body).as_deref(),
            Some(crate::db_login::IP_BAN_MESSAGE)
        );
        assert!(
            harness.faces.with_requests(|r| r.is_empty()),
            "the ban erases the attempt"
        );

        drive_login(&harness.faces, &addr, 2, &payload);
        let (_, body) = reply_frame(&recv(&client).expect("the ban gate"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedIpAddressBan as u8,
            "the address is now banned"
        );
        assert!(reply_text(&body).is_none(), "the gate replies bare");
    }

    /// A ban reply whose expiry string is not a number is a bare failure
    /// that keeps the attempt (database_reply_handler.cpp:62-75).
    #[test]
    fn a_malformed_dbapp_ban_is_a_bare_failure() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        let mut bad = BinaryStream::new();
        bad.write_u8(LogOnStatus::LoginRejectedIpAddressBan as u8);
        let _ = bad.write_string("soon");
        harness.faces.db_reply(&addr, &mut bad);
        let (_, body) = reply_frame(&recv(&client).expect("the bare failure"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedIpAddressBan as u8
        );
        assert!(reply_text(&body).is_none(), "no description");
        assert!(
            harness.faces.with_requests(|r| r.len() == 1),
            "no erase without a params snapshot"
        );
    }

    /// A DBApp reply for an address with nothing in flight is ignored.
    #[test]
    fn a_stray_dbapp_reply_is_ignored() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let other = Address::new(LOOPBACK, 41234);

        harness.faces.db_reply(
            &other,
            &mut BinaryStream::from_slice(&db_failure(
                LogOnStatus::LoginRejectedNoSuchUser as u8,
                None,
            )),
        );
        assert_eq!(harness.faces.num_login_failures(), 0);
        assert!(recv(&client).is_none());
    }

    /// `handleException` reports the DBApp timeout, erases the attempt,
    /// and leaves no overload behind
    /// (database_reply_handler.cpp:176-184).
    #[test]
    fn a_missing_dbapp_reply_reports_no_reply_and_sets_no_overload() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let payload = login_payload(&params("user", "pw", "key"));

        drive_login(&harness.faces, &addr, 1, &payload);
        harness.faces.db_no_reply(&addr);
        let (_, body) = reply_frame(&recv(&client).expect("the timeout reply"));
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedDbAppOverload as u8
        );
        assert_eq!(
            reply_text(&body).as_deref(),
            Some(crate::db_login::DB_NO_REPLY)
        );
        assert!(harness.faces.with_requests(|r| r.is_empty()));

        drive_login(&harness.faces, &addr, 2, &payload);
        assert_eq!(
            harness.forward.0.borrow().len(),
            2,
            "no overload: the retry forwards"
        );
    }

    /// A failure description too large for one packet still replies, and
    /// the size warning arm fires (loginapp.cpp:651-655).
    #[test]
    fn an_oversized_failure_description_still_replies() {
        let client = client_socket();
        let (harness, _interface) = harness(open_config());
        let addr = client_address(&client);
        harness.faces.set_db_ready(true);
        let long_message = "x".repeat(6000);

        drive_login(
            &harness.faces,
            &addr,
            1,
            &login_payload(&params("user", "pw", "key")),
        );
        harness.faces.db_reply(
            &addr,
            &mut BinaryStream::from_slice(&db_failure(
                LogOnStatus::LoginRejectedNoSuchUser as u8,
                Some(&long_message),
            )),
        );
        assert!(
            recv(&client).is_some(),
            "the oversized reply still leaves, one datagram per packet"
        );
        assert_eq!(harness.faces.num_login_failures(), 1);
    }

    /// A reply sent after the interface is gone counts the failure but
    /// sends nothing (send_bundle's upgrade arm).
    #[test]
    fn a_reply_without_an_interface_sends_nothing() {
        let harness = detached(open_config());
        let addr = Address::new(LOOPBACK, 40000);

        drive_login(
            &harness.faces,
            &addr,
            0,
            &login_payload(&params("user", "pw", "key")),
        );
        assert_eq!(harness.faces.num_login_failures(), 1);
    }

    // -- the port clock ----------------------------------------------------

    /// The client address maps into the `loginRequests_` key with a zero
    /// salt, both halves copied raw off the wire.
    #[test]
    fn the_client_address_maps_with_a_zero_salt() {
        let addr = Address::new(LOOPBACK, 40000);
        let mapped = server_address_of(&addr);
        assert_eq!(mapped.ip, LOOPBACK);
        assert_eq!(mapped.port, 40000);
        assert_eq!(mapped.salt, 0);
    }

    /// The `NoLogOnForward` production sink drops the attempt, the way a
    /// dead DBApp link does; the recording sink remembers it.
    #[test]
    fn the_logon_forward_sinks_behave() {
        let production = NoLogOnForward;
        production.send_logon(
            crate::login_request::ServerAddress::NONE,
            params("u", "p", "k"),
        );

        let recording = RecordingLogOnForward::default();
        recording.send_logon(
            crate::login_request::ServerAddress::new(1, 2, 3),
            params("u", "p", "k"),
        );
        assert_eq!(recording.0.borrow().len(), 1);
        assert_eq!(recording.0.borrow()[0].1.username(), "u");
    }

    /// `notifyDBAppAlpha` (loginapp.cpp:516-521): the DBAppMgr's notice
    /// names the alpha and opens the ready gate -- the port has no
    /// channels yet (BW-02-03), so the registration is `isDBReady()`
    /// itself.
    #[test]
    fn notify_db_app_alpha_names_the_alpha_and_opens_the_gate() {
        let harness = detached(open_config());
        assert!(!harness.faces.db_ready(), "the default state");
        assert!(harness.faces.db_app_alpha().is_none(), "no alpha named");

        let alpha = Address::new(LOOPBACK, 3100);
        harness.faces.notify_db_app_alpha(&alpha);
        assert!(harness.faces.db_ready(), "registration is readiness");
        assert_eq!(harness.faces.db_app_alpha(), alpha);
    }

    /// The production DBApp leg: an accepted login leaves as a
    /// `DBAppInterface::logOn` request -- the element eighth in the
    /// declaration order, a reserved reply id in the preamble, the
    /// `source << params` pair as the body, the footer last, addressed to
    /// the alpha the notice named.
    #[test]
    fn the_dbapp_forward_bundles_the_log_on_request() {
        let dispatcher = Rc::new(EventDispatcher::new());
        let interface = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1").0;
        // The manager `open` arms the interface with: the reply id the
        // request reserves is written by the manager, not the bundle.
        interface.set_request_manager(Some(Rc::new(RequestManager::new(
            true,
            Rc::clone(&dispatcher),
        ))));

        let forward = Rc::new(DbAppForward::new(&interface));
        let users = Rc::new(Cell::new(0));
        let faces = Rc::new(LoginFaces::new(
            open_config(),
            Rc::downgrade(&interface),
            Rc::clone(&users),
            Rc::clone(&forward) as Rc<dyn LogOnForward>,
        ));
        forward.set_faces(&faces);

        // The alpha stands on a plain socket.
        let dbapp = client_socket();
        let dbapp_addr = Address::new(
            platform::socket::inet_aton("127.0.0.1").expect("loopback parses"),
            platform::socket::htons(dbapp.local_addr().expect("dbapp port").port()),
        );
        faces.notify_db_app_alpha(&dbapp_addr);

        let client = client_socket();
        let p = params("user", "pw", "key");
        drive_login(&faces, &client_address(&client), 42, &login_payload(&p));

        let request = recv(&dbapp).expect("the request leaves on the wire");
        let packet_flags = u16::from_le_bytes(request[0..2].try_into().unwrap());
        assert!(
            packet_flags & flags::HAS_REQUESTS != 0,
            "the request flag rides with the reliable driver's own bits"
        );
        assert_ne!(
            packet_flags & flags::IS_RELIABLE,
            0,
            "`RELIABLE_DRIVER` makes the request reliable"
        );
        assert_eq!(
            request[2], DB_APP_LOG_ON_ELEMENT_ID,
            "the logOn element, eighth in declaration order"
        );
        let length = u16::from_le_bytes(request[3..5].try_into().unwrap()) as usize;
        let reply_id = i32::from_le_bytes(request[5..9].try_into().unwrap());
        assert_ne!(reply_id, 0, "startRequest reserved a reply id");
        assert_eq!(
            u16::from_le_bytes(request[9..11].try_into().unwrap()),
            0,
            "no next request"
        );
        // The first-request offset footer sits just before the reliable
        // seq footer, and points at the frame behind the packet header.
        assert_eq!(
            u16::from_le_bytes(
                request[request.len() - 6..request.len() - 4]
                    .try_into()
                    .unwrap()
            ),
            HEADER_SIZE as u16,
            "the first-request footer points at the frame"
        );

        // The body is the `source << params` pair the reference streams,
        // byte for byte.
        let body = &request[11..11 + length];
        let mut expected = BinaryStream::new();
        write_logon_request(
            &server_address_of(&client_address(&client)),
            &p,
            &mut expected,
        );
        assert_eq!(body, expected.as_bytes(), "the request body");
    }

    /// The dead arms of the production leg: no faces handed over, no
    /// alpha named, no interface -- each drops the request silently, the
    /// way a dead DBApp link would.
    #[test]
    fn the_dbapp_forward_drops_without_its_wiring() {
        let interface = NetworkInterface::new(InterfaceType::Internal, 0, "127.0.0.1").0;
        let forward = Rc::new(DbAppForward::new(&interface));

        // No faces handed over yet.
        forward.send_logon(
            crate::login_request::ServerAddress::NONE,
            params("u", "p", "k"),
        );

        // Faces without an alpha: the ready gate cannot have passed.
        let users = Rc::new(Cell::new(0));
        let faces = Rc::new(LoginFaces::new(
            open_config(),
            Weak::new(),
            users,
            Rc::new(NoLogOnForward),
        ));
        forward.set_faces(&faces);
        forward.send_logon(
            crate::login_request::ServerAddress::NONE,
            params("u", "p", "k"),
        );

        // No interface to leave on.
        drop(interface);
        faces.notify_db_app_alpha(&Address::new(LOOPBACK, 3100));
        forward.send_logon(
            crate::login_request::ServerAddress::NONE,
            params("u", "p", "k"),
        );
    }

    /// `DatabaseReplyHandler` as a reply handler: a delivery routes into
    /// the faces as the DBApp reply does, an exception lands as the
    /// no-reply failure after the reference's WARNING, and a shutdown is
    /// dropped silently (`database_reply_handler.cpp:165-179`).
    #[test]
    fn the_db_reply_handler_routes_its_deliveries() {
        let (harness, _interface) = harness(open_config());
        harness
            .faces
            .notify_db_app_alpha(&Address::new(LOOPBACK, 3100));

        let client = client_socket();
        let addr = client_address(&client);
        drive_login(
            &harness.faces,
            &addr,
            42,
            &login_payload(&params("u", "p", "k")),
        );
        assert_eq!(harness.forward.0.borrow().len(), 1, "accepted");

        let handler = DbReplyHandler {
            faces: Rc::downgrade(&harness.faces),
            client_addr: server_address_of(&addr),
        };
        let dbapp = Address::new(LOOPBACK, 3100);

        // The answer: the failure status and its text reach the client
        // under the client's reply id, and the attempt leaves the
        // registry.
        let header = UnpackedMessageHeader::new(0);
        let mut data = BinaryStream::from_slice(&db_failure(
            LogOnStatus::LoginRejectedInvalidPassword as u8,
            Some("db says no"),
        ));
        handler.handle_message(&dbapp, &header, &mut data, 0);
        let reply = recv(&client).expect("the DBApp answer routes back");
        let (reply_id, body) = reply_frame(&reply);
        assert_eq!(reply_id, 42, "the client's original reply id");
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedInvalidPassword as u8
        );
        assert_eq!(reply_text(&body).as_deref(), Some("db says no"));
        assert!(harness.faces.with_requests(|requests| requests.is_empty()));
        assert_eq!(harness.faces.num_login_failures(), 1);

        // The exception: the overload failure replies, then the WARNING.
        drive_login(
            &harness.faces,
            &addr,
            43,
            &login_payload(&params("u", "p", "k")),
        );
        handler.handle_exception(&NubException::new(Reason::TimerExpired, None), 0);
        let reply = recv(&client).expect("the no-reply failure replies");
        let (_, body) = reply_frame(&reply);
        assert_eq!(
            reply_status(&body),
            LogOnStatus::LoginRejectedDbAppOverload as u8
        );
        assert_eq!(reply_text(&body).as_deref(), Some(DB_NO_REPLY));

        // The shutdown: nothing reaches the client.
        drive_login(
            &harness.faces,
            &addr,
            44,
            &login_payload(&params("u", "p", "k")),
        );
        handler.handle_shutting_down(&NubException::new(Reason::ShuttingDown, None), 0);
        assert!(recv(&client).is_none(), "a shutdown drops the attempt");
        assert_eq!(
            harness.faces.num_login_failures(),
            2,
            "only the exception failed"
        );
    }
}
