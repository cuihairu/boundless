//! BW-06-04-02 the client's login state machine: connecting and the server
//! switch.
//!
//! `LoginHandler` (`lib/connection/login_handler.cpp`) manages the stages of
//! logging into the server — first the LoginApp, then the BaseApp the login
//! reply names. Its transitions are:
//!
//! * `start(loginAppAddr)` — a handler that is already done refuses to start
//!   again; a `NONE` address fails immediately with `CONNECTION_FAILED`
//!   ("Invalid LoginApp address"); otherwise the stage becomes `ToLoginApp`
//!   and the first attempt is sent. A stage configured with no requests is
//!   exhausted on the spot.
//! * `startWithBaseAddr(baseAppAddr, sessionKey)` — the BaseApp half of a
//!   switch. `loginAppAddr_` stays `NONE`, so `isBaseAppSwitch()` is true, and
//!   the reply record is seeded with the session key the client must present.
//! * `onLoginAppLoginSuccess(replyRecord, serverMessage)` — the LoginApp
//!   answered `LOGGED_ON` naming the BaseApp; the reply record and message are
//!   stored, the stage becomes `ToBaseApp`, the attempt counter resets and the
//!   first BaseApp attempt is sent.
//! * `onBaseAppLoginSuccess(channel, sessionKey)` — the BaseApp accepted. The
//!   status becomes `LOGGED_ON`; the session key must be non-zero and different
//!   from the one the client presented, otherwise the login fails with
//!   `CONNECTION_FAILED`. The handler finishes.
//! * `onRequestFailed` — once every attempt is used and no child request is
//!   outstanding, the stage is exhausted: the LoginApp reports
//!   `LOGIN_REJECTED_NO_LOGINAPP_RESPONSE`, the BaseApp
//!   `LOGIN_REJECTED_NO_BASEAPP_RESPONSE`.
//! * `fail(status, errorMessage)` — any failure sets the status, records the
//!   message and finishes.
//!
//! The `LogOnStatus` values are the client-side enum of `log_on_status.hpp`.

use crate::baseapp_continuation::SessionKey;
use crate::baseappmgr::Addr;

/// The `LogOnStatus::Status` client values (`log_on_status.hpp`).
pub mod log_on_status {
    /// `NOT_SET`.
    pub const NOT_SET: u8 = 0;
    /// `LOGGED_ON`.
    pub const LOGGED_ON: u8 = 1;
    /// `LOGGED_ON_OFFLINE`.
    pub const LOGGED_ON_OFFLINE: u8 = 2;
    /// `CONNECTION_FAILED`.
    pub const CONNECTION_FAILED: u8 = 3;
    /// `DNS_LOOKUP_FAILED`.
    pub const DNS_LOOKUP_FAILED: u8 = 4;
    /// `UNKNOWN_ERROR`.
    pub const UNKNOWN_ERROR: u8 = 5;
    /// `CANCELLED`.
    pub const CANCELLED: u8 = 6;
    /// `ALREADY_ONLINE_LOCALLY`.
    pub const ALREADY_ONLINE_LOCALLY: u8 = 7;
    /// `PUBLIC_KEY_LOOKUP_FAILED`.
    pub const PUBLIC_KEY_LOOKUP_FAILED: u8 = 8;
    /// `LOGIN_REJECTED_NO_LOGINAPP_RESPONSE` (generated client-side).
    pub const LOGIN_REJECTED_NO_LOGINAPP_RESPONSE: u8 = 37;
    /// `LOGIN_REJECTED_NO_BASEAPP_RESPONSE` (generated client-side).
    pub const LOGIN_REJECTED_NO_BASEAPP_RESPONSE: u8 = 38;
}

/// A `LogOnStatus` value with the reference predicates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogOnStatus(u8);

impl LogOnStatus {
    /// Constructor.
    pub fn new(value: u8) -> Self {
        Self(value)
    }

    /// The raw status value.
    pub fn value(self) -> u8 {
        self.0
    }

    /// `succeeded()`: the logon succeeded.
    pub fn succeeded(self) -> bool {
        self.0 == log_on_status::LOGGED_ON
    }

    /// `fatal()`: the logon failed in a way that cannot be retried.
    pub fn fatal(self) -> bool {
        self.0 == log_on_status::CONNECTION_FAILED
            || self.0 == log_on_status::CANCELLED
            || self.0 == log_on_status::UNKNOWN_ERROR
    }

    /// `okay()`: the logon succeeded or is still pending.
    pub fn okay(self) -> bool {
        self.0 == log_on_status::NOT_SET || self.0 == log_on_status::LOGGED_ON
    }
}

/// The number of login attempts per protocol (`LoginRequestProtocol::numRequests`).
pub const NUM_REQUESTS: u32 = 10;

/// The stage the login is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginStage {
    /// No login has been started.
    NotStarted,
    /// Logging into the LoginApp.
    ToLoginApp,
    /// Logging into the BaseApp the LoginApp named.
    ToBaseApp,
    /// The login finished (successfully or not).
    Done,
}

/// `LoginReplyRecord`: the server address and session key the server chain
/// returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoginReplyRecord {
    /// The BaseApp the client should switch to.
    pub server_addr: Addr,
    /// The session key the client presents.
    pub session_key: SessionKey,
}

/// The outcome of `start`/`startWithBaseAddr`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartOutcome {
    /// The handler already completed and refused to start.
    AlreadyDone,
    /// A `NONE` address: the login failed with `CONNECTION_FAILED`.
    InvalidAddr,
    /// The stage has no requests: it was exhausted immediately.
    NoRequests,
    /// The stage started and the first attempt was sent.
    Started,
}

/// The outcome of `on_login_app_login_success`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginAppSuccessOutcome {
    /// The handler was already done (e.g. a `SHUTDOWN`); ignored.
    Ignored,
    /// The stage advanced to the BaseApp and the first attempt was sent.
    Advanced,
    /// The BaseApp stage has no requests: it was exhausted immediately.
    NoRequests,
}

/// The outcome of `on_base_app_login_success`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BaseAppSuccessOutcome {
    /// The handler was already done; ignored.
    Ignored,
    /// The BaseApp accepted: the login is `LOGGED_ON` and the handler is done.
    LoggedOn {
        /// The adopted session key.
        session_key: SessionKey,
    },
    /// The login failed with the status and message.
    Failed {
        /// The `CONNECTION_FAILED` status.
        status: u8,
        /// The failure message.
        message: String,
    },
}

/// The outcome of `on_request_failed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestFailedOutcome {
    /// The attempt counter allows another attempt, or a child is still in
    /// flight.
    Retry,
    /// Every attempt is used and no child remains: the stage was exhausted.
    Exhausted,
}

/// The client's login state machine (`LoginHandler`).
///
/// The network transport and the per-attempt child `LoginRequest`s are not
/// modelled; what is modelled is the *stage* the login is in and the
/// transitions the reference makes as replies and failures arrive. A single
/// `in_flight` counter stands in for the set of outstanding child requests:
/// `send_next_request` adds one, `on_request_failed` (called after the failed
/// child has removed itself) removes one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginHandler {
    stage: LoginStage,
    status: LogOnStatus,
    server_msg: String,
    login_app_addr: Addr,
    base_app_addr: Addr,
    reply_record: LoginReplyRecord,
    num_attempts_made: u32,
    num_requests: u32,
    in_flight: u32,
    has_received_no_such_port: bool,
    done: bool,
}

impl LoginHandler {
    /// Constructor. `login_not_sent` seeds the status; a value other than
    /// `NOT_SET` means the handler starts done (the login already happened).
    pub fn new(login_not_sent: LogOnStatus) -> Self {
        Self {
            stage: if login_not_sent.value() == log_on_status::NOT_SET {
                LoginStage::NotStarted
            } else {
                LoginStage::Done
            },
            status: login_not_sent,
            server_msg: String::new(),
            login_app_addr: (0, 0),
            base_app_addr: (0, 0),
            reply_record: LoginReplyRecord {
                server_addr: (0, 0),
                session_key: 0,
            },
            num_attempts_made: 0,
            num_requests: NUM_REQUESTS,
            in_flight: 0,
            has_received_no_such_port: false,
            done: login_not_sent.value() != log_on_status::NOT_SET,
        }
    }

    /// `start`: begin the LoginApp stage.
    pub fn start(&mut self, login_app_addr: Addr) -> StartOutcome {
        if self.done {
            return StartOutcome::AlreadyDone;
        }
        if login_app_addr == (0, 0) {
            self.fail(
                log_on_status::CONNECTION_FAILED,
                "Invalid LoginApp address".to_string(),
            );
            return StartOutcome::InvalidAddr;
        }
        self.stage = LoginStage::ToLoginApp;
        self.login_app_addr = login_app_addr;
        self.base_app_addr = (0, 0);
        self.num_attempts_made = 0;
        self.in_flight = 0;
        self.begin_stage()
    }

    /// `startWithBaseAddr`: begin the BaseApp stage of a switch.
    pub fn start_with_base_addr(
        &mut self,
        base_app_addr: Addr,
        session_key: SessionKey,
    ) -> StartOutcome {
        if self.done {
            return StartOutcome::AlreadyDone;
        }
        self.stage = LoginStage::ToBaseApp;
        self.login_app_addr = (0, 0);
        self.base_app_addr = base_app_addr;
        self.reply_record = LoginReplyRecord {
            server_addr: base_app_addr,
            session_key,
        };
        self.num_attempts_made = 0;
        self.in_flight = 0;
        self.begin_stage()
    }

    /// The stage setup shared by `start` and `start_with_base_addr`: send the
    /// first attempt, or exhaust a stage with no requests.
    fn begin_stage(&mut self) -> StartOutcome {
        if self.num_requests == 0 {
            self.exhaust_current_stage();
            return StartOutcome::NoRequests;
        }
        self.send_next_request();
        StartOutcome::Started
    }

    /// `onLoginAppLoginSuccess`: the LoginApp answered `LOGGED_ON`.
    pub fn on_login_app_login_success(
        &mut self,
        reply: LoginReplyRecord,
        server_message: &str,
    ) -> LoginAppSuccessOutcome {
        if self.done {
            return LoginAppSuccessOutcome::Ignored;
        }
        self.reply_record = reply;
        self.server_msg = server_message.to_string();
        self.base_app_addr = reply.server_addr;
        self.stage = LoginStage::ToBaseApp;
        self.num_attempts_made = 0;
        self.in_flight = 0;
        self.has_received_no_such_port = false;
        if self.num_requests == 0 {
            self.exhaust_current_stage();
            return LoginAppSuccessOutcome::NoRequests;
        }
        self.send_next_request();
        LoginAppSuccessOutcome::Advanced
    }

    /// `onBaseAppLoginSuccess`: the BaseApp accepted.
    pub fn on_base_app_login_success(&mut self, session_key: SessionKey) -> BaseAppSuccessOutcome {
        if self.done {
            return BaseAppSuccessOutcome::Ignored;
        }
        self.status = LogOnStatus::new(log_on_status::LOGGED_ON);
        if session_key == 0 {
            return self.fail_base_app("Got zero session key");
        }
        if session_key == self.reply_record.session_key {
            return self.fail_base_app("Got unchanged session key");
        }
        self.reply_record.session_key = session_key;
        self.finish();
        BaseAppSuccessOutcome::LoggedOn { session_key }
    }

    /// `onRequestFailed`: one attempt's child request has finished and failed.
    /// When every attempt is used and no child is still outstanding, the stage
    /// is exhausted.
    pub fn on_request_failed(&mut self) -> RequestFailedOutcome {
        if self.done {
            return RequestFailedOutcome::Retry;
        }
        self.in_flight = self.in_flight.saturating_sub(1);
        if self.num_attempts_made >= self.num_requests && self.in_flight == 0 {
            self.exhaust_current_stage();
            RequestFailedOutcome::Exhausted
        } else {
            RequestFailedOutcome::Retry
        }
    }

    /// `sendNextRequest`: send the next attempt to the current stage's app.
    pub fn send_next_request(&mut self) {
        self.num_attempts_made += 1;
        self.in_flight += 1;
    }

    /// `onNoSuchPort`: an ICMP no-such-port for the current app. This only
    /// affects the message a later exhaustion produces.
    pub fn on_no_such_port(&mut self, addr: Addr) {
        if self.done || self.current_addr() != Some(addr) {
            return;
        }
        self.has_received_no_such_port = true;
    }

    /// `fail`: set the status and message, and finish.
    pub fn fail(&mut self, status: u8, message: String) {
        self.status = LogOnStatus::new(status);
        self.server_msg = message;
        self.finish();
    }

    /// `finish`: mark the handler done (a second call is a no-op).
    pub fn finish(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        self.stage = LoginStage::Done;
        self.in_flight = 0;
    }

    /// The stage's app address, when a stage is active.
    fn current_addr(&self) -> Option<Addr> {
        match self.stage {
            LoginStage::ToLoginApp => Some(self.login_app_addr),
            LoginStage::ToBaseApp => Some(self.base_app_addr),
            _ => None,
        }
    }

    /// `onAttemptsExhausted` for the current stage.
    fn exhaust_current_stage(&mut self) {
        match self.stage {
            LoginStage::ToLoginApp => {
                let mut message = format!(
                    "No response from LoginApp {}",
                    fmt_addr(self.login_app_addr)
                );
                if self.has_received_no_such_port {
                    message.push_str(" (connection refused)");
                }
                self.fail(log_on_status::LOGIN_REJECTED_NO_LOGINAPP_RESPONSE, message);
            }
            LoginStage::ToBaseApp => {
                let message = format!(
                    "Exhausted available attempts ({}) to login to BaseApp {}",
                    self.num_requests,
                    fmt_addr(self.base_app_addr),
                );
                self.fail(log_on_status::LOGIN_REJECTED_NO_BASEAPP_RESPONSE, message);
            }
            _ => {}
        }
    }

    /// A failed `on_base_app_login_success` sets `CONNECTION_FAILED` and
    /// returns the matching outcome.
    fn fail_base_app(&mut self, message: &str) -> BaseAppSuccessOutcome {
        let status = log_on_status::CONNECTION_FAILED;
        let message = message.to_string();
        self.fail(status, message.clone());
        BaseAppSuccessOutcome::Failed { status, message }
    }

    /// Whether the handler is done.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The current stage.
    pub fn stage(&self) -> LoginStage {
        self.stage
    }

    /// `status()`.
    pub fn status(&self) -> LogOnStatus {
        self.status
    }

    /// `serverMsg()`.
    pub fn server_msg(&self) -> &str {
        &self.server_msg
    }

    /// `loginAppAddr()`.
    pub fn login_app_addr(&self) -> Addr {
        self.login_app_addr
    }

    /// `baseAppAddr()`.
    pub fn base_app_addr(&self) -> Addr {
        self.base_app_addr
    }

    /// `isBaseAppSwitch()`.
    pub fn is_base_app_switch(&self) -> bool {
        self.login_app_addr == (0, 0) && self.base_app_addr != (0, 0)
    }

    /// `numAttemptsMade_`.
    pub fn num_attempts_made(&self) -> u32 {
        self.num_attempts_made
    }

    /// `replyRecord()`.
    pub fn reply_record(&self) -> LoginReplyRecord {
        self.reply_record
    }

    /// `hasReceivedNoSuchPort_`.
    pub fn has_received_no_such_port(&self) -> bool {
        self.has_received_no_such_port
    }
}

/// A dotted-quad address for the reference-style messages.
fn fmt_addr(addr: Addr) -> String {
    let ip = addr.0;
    format!(
        "{}.{}.{}.{}:{}",
        (ip >> 24) & 0xff,
        (ip >> 16) & 0xff,
        (ip >> 8) & 0xff,
        ip & 0xff,
        addr.1,
    )
}

#[cfg(test)]
mod tests {
    use super::log_on_status::*;
    use super::*;

    const LOGIN_APP: Addr = (0x0a00_0001, 20013);
    const BASE_APP: Addr = (0x0b00_0001, 20000);
    const SEEDED_KEY: SessionKey = 0x1111_1111;
    const NEW_KEY: SessionKey = 0x2222_2222;

    fn new_handler() -> LoginHandler {
        LoginHandler::new(LogOnStatus::new(NOT_SET))
    }

    #[test]
    fn a_not_set_handler_is_not_done() {
        let handler = new_handler();
        assert_eq!(handler.status(), LogOnStatus::new(NOT_SET));
        assert!(!handler.is_done());
        assert_eq!(handler.stage(), LoginStage::NotStarted);
        assert!(handler.status().okay());
        assert!(!handler.status().succeeded());
        assert!(!handler.status().fatal());
    }

    #[test]
    fn a_preseeded_status_starts_done() {
        let handler = LoginHandler::new(LogOnStatus::new(LOGGED_ON));
        assert!(handler.is_done());
        assert_eq!(handler.stage(), LoginStage::Done);
        assert!(handler.status().succeeded());
    }

    #[test]
    fn start_sets_the_login_app_stage_and_sends_the_first_attempt() {
        let mut handler = new_handler();
        assert_eq!(handler.start(LOGIN_APP), StartOutcome::Started);
        assert_eq!(handler.stage(), LoginStage::ToLoginApp);
        assert_eq!(handler.login_app_addr(), LOGIN_APP);
        assert_eq!(handler.base_app_addr(), (0, 0));
        assert!(!handler.is_base_app_switch());
        assert_eq!(handler.num_attempts_made(), 1);
    }

    #[test]
    fn start_after_done_refuses() {
        let mut handler = LoginHandler::new(LogOnStatus::new(LOGGED_ON));
        assert_eq!(handler.start(LOGIN_APP), StartOutcome::AlreadyDone);
    }

    #[test]
    fn a_none_address_fails_immediately() {
        let mut handler = new_handler();
        assert_eq!(handler.start((0, 0)), StartOutcome::InvalidAddr);
        assert!(handler.is_done());
        assert_eq!(handler.status(), LogOnStatus::new(CONNECTION_FAILED));
        assert_eq!(handler.server_msg(), "Invalid LoginApp address");
    }

    #[test]
    fn a_stage_with_no_requests_is_exhausted_on_start() {
        let mut handler = new_handler();
        handler.num_requests = 0;
        assert_eq!(handler.start(LOGIN_APP), StartOutcome::NoRequests);
        assert!(handler.is_done());
        assert_eq!(
            handler.status(),
            LogOnStatus::new(LOGIN_REJECTED_NO_LOGINAPP_RESPONSE)
        );
    }

    #[test]
    fn start_with_base_addr_is_a_base_app_switch() {
        let mut handler = new_handler();
        assert_eq!(
            handler.start_with_base_addr(BASE_APP, SEEDED_KEY),
            StartOutcome::Started
        );
        assert_eq!(handler.stage(), LoginStage::ToBaseApp);
        assert_eq!(handler.base_app_addr(), BASE_APP);
        assert_eq!(handler.login_app_addr(), (0, 0));
        assert!(handler.is_base_app_switch());
        assert_eq!(handler.reply_record().session_key, SEEDED_KEY);
        assert_eq!(handler.reply_record().server_addr, BASE_APP);
        assert_eq!(handler.num_attempts_made(), 1);
    }

    #[test]
    fn a_login_app_success_switches_to_the_base_app() {
        let mut handler = new_handler();
        handler.start(LOGIN_APP);
        let outcome = handler.on_login_app_login_success(
            LoginReplyRecord {
                server_addr: BASE_APP,
                session_key: SEEDED_KEY,
            },
            "welcome",
        );
        assert_eq!(outcome, LoginAppSuccessOutcome::Advanced);
        assert_eq!(handler.stage(), LoginStage::ToBaseApp);
        assert_eq!(handler.base_app_addr(), BASE_APP);
        assert_eq!(handler.reply_record().session_key, SEEDED_KEY);
        assert_eq!(handler.server_msg(), "welcome");
        // The attempt counter reset for the new stage.
        assert_eq!(handler.num_attempts_made(), 1);
    }

    #[test]
    fn a_login_app_success_after_done_is_ignored() {
        let mut handler = new_handler();
        handler.fail(CONNECTION_FAILED, "done".to_string());
        assert_eq!(
            handler.on_login_app_login_success(
                LoginReplyRecord {
                    server_addr: BASE_APP,
                    session_key: SEEDED_KEY,
                },
                "ignored",
            ),
            LoginAppSuccessOutcome::Ignored
        );
        assert_eq!(handler.server_msg(), "done");
    }

    #[test]
    fn a_base_app_success_logs_on() {
        let mut handler = new_handler();
        handler.start_with_base_addr(BASE_APP, SEEDED_KEY);
        let outcome = handler.on_base_app_login_success(NEW_KEY);
        assert_eq!(
            outcome,
            BaseAppSuccessOutcome::LoggedOn {
                session_key: NEW_KEY
            }
        );
        assert!(handler.is_done());
        assert_eq!(handler.status(), LogOnStatus::new(LOGGED_ON));
        assert!(handler.status().succeeded());
        assert_eq!(handler.reply_record().session_key, NEW_KEY);
    }

    #[test]
    fn a_zero_session_key_fails() {
        let mut handler = new_handler();
        handler.start_with_base_addr(BASE_APP, SEEDED_KEY);
        let outcome = handler.on_base_app_login_success(0);
        assert_eq!(
            outcome,
            BaseAppSuccessOutcome::Failed {
                status: CONNECTION_FAILED,
                message: "Got zero session key".to_string(),
            }
        );
        assert!(handler.is_done());
        assert_eq!(handler.status(), LogOnStatus::new(CONNECTION_FAILED));
        assert!(handler.status().fatal());
    }

    #[test]
    fn an_unchanged_session_key_fails() {
        let mut handler = new_handler();
        handler.start_with_base_addr(BASE_APP, SEEDED_KEY);
        let outcome = handler.on_base_app_login_success(SEEDED_KEY);
        assert_eq!(
            outcome,
            BaseAppSuccessOutcome::Failed {
                status: CONNECTION_FAILED,
                message: "Got unchanged session key".to_string(),
            }
        );
    }

    #[test]
    fn a_base_app_success_after_done_is_ignored() {
        let mut handler = new_handler();
        handler.fail(CONNECTION_FAILED, "done".to_string());
        assert_eq!(
            handler.on_base_app_login_success(NEW_KEY),
            BaseAppSuccessOutcome::Ignored
        );
    }

    #[test]
    fn exhausting_every_attempt_reports_no_login_app_response() {
        let mut handler = new_handler();
        // `start` sends attempt 1; the retry timer sends the rest as each
        // attempt times out with no reply.
        handler.start(LOGIN_APP);
        for _ in 0..(NUM_REQUESTS - 1) {
            assert_eq!(handler.on_request_failed(), RequestFailedOutcome::Retry);
            handler.send_next_request();
        }
        assert_eq!(handler.on_request_failed(), RequestFailedOutcome::Exhausted);
        assert!(handler.is_done());
        assert_eq!(
            handler.status(),
            LogOnStatus::new(LOGIN_REJECTED_NO_LOGINAPP_RESPONSE)
        );
        assert!(handler.server_msg().contains("No response from LoginApp"));
    }

    #[test]
    fn exhausting_the_base_app_stage_reports_no_base_app_response() {
        let mut handler = new_handler();
        handler.start_with_base_addr(BASE_APP, SEEDED_KEY);
        for _ in 0..(NUM_REQUESTS - 1) {
            assert_eq!(handler.on_request_failed(), RequestFailedOutcome::Retry);
            handler.send_next_request();
        }
        assert_eq!(handler.on_request_failed(), RequestFailedOutcome::Exhausted);
        assert_eq!(
            handler.status(),
            LogOnStatus::new(LOGIN_REJECTED_NO_BASEAPP_RESPONSE)
        );
        assert!(
            handler
                .server_msg()
                .contains("Exhausted available attempts")
        );
    }

    #[test]
    fn not_every_attempt_used_retries() {
        let mut handler = new_handler();
        handler.start(LOGIN_APP);
        // The first attempt fails while attempts remain: retry, not exhaust.
        assert_eq!(handler.on_request_failed(), RequestFailedOutcome::Retry);
        assert!(!handler.is_done());
        assert_eq!(handler.status(), LogOnStatus::new(NOT_SET));
    }

    #[test]
    fn a_child_still_in_flight_prevents_exhaustion() {
        let mut handler = new_handler();
        handler.start(LOGIN_APP);
        // Send the remaining attempts; every child is still outstanding.
        for _ in 1..NUM_REQUESTS {
            handler.send_next_request();
        }
        for _ in 0..(NUM_REQUESTS - 1) {
            assert_eq!(handler.on_request_failed(), RequestFailedOutcome::Retry);
        }
        assert_eq!(handler.on_request_failed(), RequestFailedOutcome::Exhausted);
    }

    #[test]
    fn a_no_such_port_notification_is_recorded_for_the_current_app() {
        let mut handler = new_handler();
        handler.start(LOGIN_APP);
        handler.on_no_such_port(LOGIN_APP);
        assert!(handler.has_received_no_such_port());
        // An unrelated (possibly old) address is ignored.
        handler.on_no_such_port(BASE_APP);
        assert!(handler.has_received_no_such_port());
    }

    #[test]
    fn an_exhaustion_after_connection_refused_mentions_it() {
        let mut handler = new_handler();
        handler.start(LOGIN_APP);
        handler.on_no_such_port(LOGIN_APP);
        for _ in 0..(NUM_REQUESTS - 1) {
            handler.on_request_failed();
            handler.send_next_request();
        }
        handler.on_request_failed();
        assert!(handler.server_msg().contains("(connection refused)"));
    }
}
