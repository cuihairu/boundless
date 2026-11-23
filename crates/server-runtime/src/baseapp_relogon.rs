//! BW-06-03-05 the BaseApp's re-logon and session recycling.
//!
//! `BaseApp::logOnAttempt` (`server/baseapp/baseapp.cpp`) is the BaseApp half
//! of a re-logon: the DBApp has decided to hand a login to an existing base
//! and asks this BaseApp to let the new client in. The reply is one of
//!
//! * `LOG_ON_ATTEMPT_WAIT_FOR_DESTROY` — there is no base, the base destroyed
//!   itself in `onLogOnAttempt`, or the script returned
//!   `LOG_ON_WAIT_FOR_DESTROY`; the DBApp re-runs the login once the old
//!   entity is gone;
//! * `LOG_ON_ATTEMPT_REJECTED` — the base is not a proxy, the script returned
//!   no result, or it refused;
//! * `LOG_ON_ATTEMPT_TOOK_CONTROL` — the existing proxy is reused for the new
//!   client (never a second proxy), after the old client is logged off, or
//!   deferred when the proxy is mid-`giveClientTo`.
//!
//! The deferred case is `Proxy::prepareForReLogOnAfterGiveClientToSuccess`:
//! the attempt is stored in `pPendingReLogOn_`, which holds at most one
//! attempt — a newer one replaces (and rejects) an older one, so a retry can
//! never accumulate duplicate proxies. `Proxy::completeReLogOnAttempt` replies
//! `TOOK_CONTROL` with the BaseApp address, the proxy's mailbox ref and a
//! fresh login key (0 for a web login), and re-arms the pending login.

use crate::baseapp_continuation::{EntityMailBoxRef, ProxySession, SessionKey};
use crate::baseappmgr::Addr;
use crate::pending_logins::PendingLogins;
use crate::shutdown::GameTime;

/// `LogOnAttemptResult::LOG_ON_REJECT` (`script_bigworld.hpp`).
pub const LOG_ON_REJECT: i32 = 0;
/// `LogOnAttemptResult::LOG_ON_ACCEPT`.
pub const LOG_ON_ACCEPT: i32 = 1;
/// `LogOnAttemptResult::LOG_ON_WAIT_FOR_DESTROY`.
pub const LOG_ON_WAIT_FOR_DESTROY: i32 = 2;

/// `BaseAppIntInterface::LOG_ON_ATTEMPT_REJECTED`.
pub const LOG_ON_ATTEMPT_REJECTED: u8 = 0;
/// `BaseAppIntInterface::LOG_ON_ATTEMPT_TOOK_CONTROL`.
pub const LOG_ON_ATTEMPT_TOOK_CONTROL: u8 = 1;
/// `BaseAppIntInterface::LOG_ON_ATTEMPT_WAIT_FOR_DESTROY`.
pub const LOG_ON_ATTEMPT_WAIT_FOR_DESTROY: u8 = 3;

/// `BaseApp::logOnAttempt`'s decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogOnAttemptDecision {
    /// Reply `LOG_ON_ATTEMPT_WAIT_FOR_DESTROY`.
    WaitForDestroy,
    /// Reply `LOG_ON_ATTEMPT_REJECTED`.
    Rejected,
    /// The existing proxy is reused; the old client is logged off (unless it
    /// is a web login) and the re-logon completes.
    TookControl {
        /// The client's address; a zero `ip` is a web login.
        client_addr: Addr,
        /// Whether it is a web login (no client channel to log off).
        web_login: bool,
    },
    /// The proxy is giving its client away: defer the re-logon in the single
    /// pending slot.
    Defer {
        /// The client's address.
        client_addr: Addr,
    },
}

/// `BaseApp::logOnAttempt`: decide the reply for a re-logon attempt.
///
/// `base_exists` is whether the entity is on this BaseApp, `is_proxy` whether
/// it is a `Proxy`, `is_destroyed` whether the script destroyed it during the
/// callback, `script_result` the `onLogOnAttempt` return (`None` when the
/// script produced no result or raised), and `is_giving_client_away` whether
/// a `giveClientTo` transfer is in flight.
pub fn log_on_attempt(
    base_exists: bool,
    is_proxy: bool,
    is_destroyed: bool,
    script_result: Option<i32>,
    client_addr: Addr,
    is_giving_client_away: bool,
) -> LogOnAttemptDecision {
    if !base_exists {
        return LogOnAttemptDecision::WaitForDestroy;
    }
    if !is_proxy {
        return LogOnAttemptDecision::Rejected;
    }
    match script_result {
        // "failed to return an authorisation status": reject.
        None => LogOnAttemptDecision::Rejected,
        Some(result) => {
            // The base destroyed itself or the script asked to wait for the
            // old entity to be destroyed.
            if is_destroyed || result == LOG_ON_WAIT_FOR_DESTROY {
                return LogOnAttemptDecision::WaitForDestroy;
            }
            // tookControl = !!result: a falsy result refuses the attempt.
            if result == LOG_ON_REJECT {
                return LogOnAttemptDecision::Rejected;
            }
            if is_giving_client_away {
                LogOnAttemptDecision::Defer { client_addr }
            } else {
                LogOnAttemptDecision::TookControl {
                    client_addr,
                    web_login: client_addr.0 == 0,
                }
            }
        }
    }
}

/// One deferred re-logon attempt (`PendingReLogOn`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingReLogOn {
    /// The client's address.
    pub client_addr: Addr,
    /// The login request's reply id.
    pub reply_id: u32,
    /// The session encryption key.
    pub encryption_key: Vec<u8>,
}

/// The `pPendingReLogOn_` slot: at most one deferred attempt.
#[derive(Clone, Debug, Default)]
pub struct DeferredReLogOn {
    pending: Option<PendingReLogOn>,
}

impl DeferredReLogOn {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// The deferred attempt, if any.
    pub fn pending(&self) -> Option<&PendingReLogOn> {
        self.pending.as_ref()
    }

    /// Whether an attempt is deferred.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// `prepareForReLogOnAfterGiveClientToSuccess`: keep the newest attempt;
    /// an older one is discarded and its `REJECTED` reply goes out.
    pub fn defer(&mut self, attempt: PendingReLogOn) -> DeferOutcome {
        match self.pending.replace(attempt) {
            Some(old) => DeferOutcome::Replaced { rejected: old },
            None => DeferOutcome::Deferred,
        }
    }

    /// `discardReLogOnAttempt`: reply `REJECTED` and clear the slot.
    pub fn discard(&mut self) -> Option<PendingReLogOn> {
        self.pending.take()
    }

    /// `completeReLogOnAttempt` clears the slot.
    pub fn clear(&mut self) -> Option<PendingReLogOn> {
        self.pending.take()
    }
}

/// The outcome of `defer`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeferOutcome {
    /// A previous deferred attempt was replaced; its `REJECTED` reply goes
    /// out.
    Replaced {
        /// The discarded attempt.
        rejected: PendingReLogOn,
    },
    /// No attempt was deferred before.
    Deferred,
}

/// The `LOG_ON_ATTEMPT_TOOK_CONTROL` reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReLogOnReply {
    /// `LOG_ON_ATTEMPT_TOOK_CONTROL`.
    pub code: u8,
    /// The BaseApp external address.
    pub base_app_addr: Addr,
    /// The proxy's mailbox ref (the same proxy — never a second one).
    pub entity_mailbox: EntityMailBoxRef,
    /// The login key the client presents; 0 for a web login.
    pub login_key: SessionKey,
}

/// The BaseApp context one re-logon completes against: the proxy being reused
/// and the containers its reply touches.
pub struct ReLogOnApp<'a> {
    /// The proxy's session side.
    pub session: &'a mut ProxySession,
    /// The BaseApp's pending logins.
    pub pending: &'a mut PendingLogins,
    /// The proxy being reused (never a second one).
    pub proxy_id: u32,
    /// The BaseApp's external address.
    pub base_app_addr: Addr,
    /// The proxy's mailbox ref.
    pub entity_mailbox: EntityMailBoxRef,
    /// The timestamp for the key regeneration.
    pub now: u64,
    /// The game time that arms the pending login's timeout.
    pub game_time: GameTime,
    /// The BaseApp tick rate.
    pub update_hertz: u32,
}

impl ReLogOnApp<'_> {
    /// `Proxy::completeReLogOnAttempt`: reuse the existing proxy for the new
    /// client, re-arm the pending login with a fresh key and reply
    /// `TOOK_CONTROL`.
    pub fn complete_re_log_on_attempt(&mut self, client_addr: Addr) -> ReLogOnReply {
        let web_login = client_addr.0 == 0;
        let login_key = if web_login {
            // "only clear base's client channel if this is not a web login"
            0
        } else {
            let key = self.session.prepare_for_login(client_addr, self.now);
            self.pending.add(
                self.proxy_id,
                key,
                client_addr,
                self.game_time,
                self.update_hertz,
            );
            key
        };
        ReLogOnReply {
            code: LOG_ON_ATTEMPT_TOOK_CONTROL,
            base_app_addr: self.base_app_addr,
            entity_mailbox: self.entity_mailbox,
            login_key,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending_logins::LoginDecision;

    const PROXY_ID: u32 = 7;
    const CLIENT: Addr = (0x0a00_0001, 40000);
    const BASE_APP: Addr = (0x0b00_0001, 20000);
    const NOW: u64 = 1_000_000_000;
    const GAME_TIME: GameTime = 500;
    const HERTZ: u32 = 20;

    fn mailbox() -> EntityMailBoxRef {
        EntityMailBoxRef::new(PROXY_ID, BASE_APP)
    }

    #[test]
    fn a_missing_base_waits_for_the_old_entity() {
        assert_eq!(
            log_on_attempt(false, true, false, Some(LOG_ON_ACCEPT), CLIENT, false),
            LogOnAttemptDecision::WaitForDestroy
        );
    }

    #[test]
    fn a_non_proxy_base_is_rejected() {
        assert_eq!(
            log_on_attempt(true, false, false, Some(LOG_ON_ACCEPT), CLIENT, false),
            LogOnAttemptDecision::Rejected
        );
    }

    #[test]
    fn a_destroyed_base_waits_for_the_old_entity() {
        assert_eq!(
            log_on_attempt(true, true, true, Some(LOG_ON_ACCEPT), CLIENT, false),
            LogOnAttemptDecision::WaitForDestroy
        );
    }

    #[test]
    fn a_wait_for_destroy_result_waits() {
        assert_eq!(
            log_on_attempt(
                true,
                true,
                false,
                Some(LOG_ON_WAIT_FOR_DESTROY),
                CLIENT,
                false,
            ),
            LogOnAttemptDecision::WaitForDestroy
        );
    }

    #[test]
    fn a_missing_script_result_is_rejected() {
        assert_eq!(
            log_on_attempt(true, true, false, None, CLIENT, false),
            LogOnAttemptDecision::Rejected
        );
    }

    #[test]
    fn a_refused_attempt_is_rejected() {
        assert_eq!(
            log_on_attempt(true, true, false, Some(LOG_ON_REJECT), CLIENT, false),
            LogOnAttemptDecision::Rejected
        );
    }

    #[test]
    fn an_accepted_attempt_reuses_the_proxy() {
        assert_eq!(
            log_on_attempt(true, true, false, Some(LOG_ON_ACCEPT), CLIENT, false),
            LogOnAttemptDecision::TookControl {
                client_addr: CLIENT,
                web_login: false,
            }
        );
    }

    #[test]
    fn a_web_login_reuses_the_proxy_without_logging_off() {
        assert_eq!(
            log_on_attempt(true, true, false, Some(LOG_ON_ACCEPT), (0, 40000), false),
            LogOnAttemptDecision::TookControl {
                client_addr: (0, 40000),
                web_login: true,
            }
        );
    }

    #[test]
    fn a_giving_away_proxy_defers_the_attempt() {
        assert_eq!(
            log_on_attempt(true, true, false, Some(LOG_ON_ACCEPT), CLIENT, true),
            LogOnAttemptDecision::Defer {
                client_addr: CLIENT
            }
        );
    }

    #[test]
    fn the_pending_slot_holds_at_most_one_attempt() {
        let mut deferred = DeferredReLogOn::new();
        assert!(!deferred.is_pending());
        let first = PendingReLogOn {
            client_addr: CLIENT,
            reply_id: 5,
            encryption_key: vec![1],
        };
        let second = PendingReLogOn {
            client_addr: (0x0a00_0002, 40001),
            reply_id: 6,
            encryption_key: vec![2],
        };
        assert_eq!(deferred.defer(first.clone()), DeferOutcome::Deferred);
        assert_eq!(deferred.pending(), Some(&first));
        // A retry replaces rather than accumulates a second attempt.
        assert_eq!(
            deferred.defer(second.clone()),
            DeferOutcome::Replaced { rejected: first }
        );
        assert_eq!(deferred.pending(), Some(&second));
        assert_eq!(deferred.discard(), Some(second));
        assert!(!deferred.is_pending());
        assert_eq!(deferred.discard(), None);
    }

    /// A `ReLogOnApp` against fresh session and pending-login containers.
    fn app<'a>(session: &'a mut ProxySession, pending: &'a mut PendingLogins) -> ReLogOnApp<'a> {
        ReLogOnApp {
            session,
            pending,
            proxy_id: PROXY_ID,
            base_app_addr: BASE_APP,
            entity_mailbox: mailbox(),
            now: NOW + 1,
            game_time: GAME_TIME,
            update_hertz: HERTZ,
        }
    }

    #[test]
    fn completing_a_relogon_reuses_the_proxy_and_rearms_the_pending_login() {
        let mut session = ProxySession::new(NOW);
        let mut pending = PendingLogins::new_default();
        // The proxy's current key is the one the client presents.
        let login_key_before = session.session_key();

        let reply = app(&mut session, &mut pending).complete_re_log_on_attempt(CLIENT);

        assert_eq!(reply.code, LOG_ON_ATTEMPT_TOOK_CONTROL);
        assert_eq!(reply.base_app_addr, BASE_APP);
        assert_eq!(reply.entity_mailbox, mailbox());
        // The login key is the proxy's previous key and is pending again; the
        // proxy keeps a new key for the attach reply.
        assert_eq!(reply.login_key, login_key_before);
        assert_ne!(reply.login_key, 0);
        assert_ne!(session.session_key(), login_key_before);
        assert_eq!(pending.find(reply.login_key).unwrap().proxy_id, PROXY_ID);
    }

    #[test]
    fn a_web_login_gets_a_zero_login_key_and_no_pending_entry() {
        let mut session = ProxySession::new(NOW);
        let mut pending = PendingLogins::new_default();

        let reply = app(&mut session, &mut pending).complete_re_log_on_attempt((0, 40000));

        assert_eq!(reply.login_key, 0);
        // No pending login was armed for a web login.
        assert_eq!(
            pending.login(reply.login_key, false, false, 1),
            LoginDecision::InvalidTicket
        );
    }

    #[test]
    fn clearing_the_slot_completes_the_relogon() {
        let mut deferred = DeferredReLogOn::new();
        let attempt = PendingReLogOn {
            client_addr: CLIENT,
            reply_id: 9,
            encryption_key: vec![],
        };
        deferred.defer(attempt.clone());
        assert_eq!(deferred.clear(), Some(attempt));
        assert!(!deferred.is_pending());
    }
}
