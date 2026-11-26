//! BW-06-02-04 the duplicate login and re-logon handling.
//!
//! When a login arrives for an entity that is already logged on or logging
//! in, the DBApp's `onLogOnLoggedOnUser` (`dbapp.cpp`) either rejects it or
//! hands it to the existing base entity: a `RelogonAttemptHandler`
//! (`relogon_attempt_handler.cpp`) sends the BaseApp a `logOnAttempt` request
//! and waits for one of
//!
//! * `LOG_ON_ATTEMPT_TOOK_CONTROL` — the new client takes over the existing
//!   base; the login replies `LOGGED_ON`;
//! * `LOG_ON_ATTEMPT_REJECTED` — the base refused; the login replies
//!   `LOGIN_REJECTED_ALREADY_LOGGED_IN` ("Relogin denied.");
//! * `LOG_ON_ATTEMPT_WAIT_FOR_DESTROY` — the base is waiting for the old
//!   session to go away; the handler waits five seconds for the entity to be
//!   destroyed (the DBApp's online records then wake it via `onEntityLogOff`
//!   and the login re-runs), or gives up with
//!   `LOGIN_REJECTED_BASEAPP_TIMEOUT`.
//!
//! A second re-logon for the same entity is refused outright
//! ("A relogin of same name still in progress."), as is a login whose entity
//! is merely being checked out (`isActiveMailBox` fails: "Another login of
//! same name still in progress.").

use crate::baseappmgr::Addr;
use crate::billing::EntityKey;
use crate::log_on_records::LogOnRecords;
use crate::login_entity::BaseMailbox;

/// `BaseAppIntInterface::LOG_ON_ATTEMPT_REJECTED`.
pub const LOG_ON_ATTEMPT_REJECTED: u8 = 0;
/// `BaseAppIntInterface::LOG_ON_ATTEMPT_TOOK_CONTROL`.
pub const LOG_ON_ATTEMPT_TOOK_CONTROL: u8 = 1;
/// `BaseAppIntInterface::LOG_ON_ATTEMPT_WAIT_FOR_DESTROY`.
pub const LOG_ON_ATTEMPT_WAIT_FOR_DESTROY: u8 = 3;

/// The wait-for-destroy timeout (`waitForDestroyTimeout`), in stamps.
pub const WAIT_FOR_DESTROY_TIMEOUT: u64 = 5_000_000_000;

/// `GetEntityHandler::isActiveMailBox` (`get_entity_handler.cpp`): a mailbox
/// is active when it names an entity on a non-empty address.
pub fn is_active_mailbox(base_ref: Option<BaseMailbox>) -> bool {
    match base_ref {
        Some(mb) => mb.id != 0 && mb.addr.0 != 0,
        None => false,
    }
}

/// The `DBApp::onLogOnLoggedOnUser` gates: reject or start a re-logon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicateLoginOutcome {
    /// The `logOnAttempt` request goes to the existing base; the attempt is
    /// registered in the online records.
    StartRelogon {
        /// The entity being re-logged on.
        key: EntityKey,
        /// The existing base the request is sent to.
        existing_base: BaseMailbox,
    },
    /// `LOGIN_REJECTED_ALREADY_LOGGED_IN` with the reference message.
    Rejected { message: &'static str },
}

/// `onLogOnLoggedOnUser` (`dbapp.cpp`).
pub fn on_duplicate_login(
    records: &mut LogOnRecords,
    key: EntityKey,
    existing_base: Option<BaseMailbox>,
) -> DuplicateLoginOutcome {
    if records.has_pending_relogon(key) {
        // Another re-logon already in progress.
        return DuplicateLoginOutcome::Rejected {
            message: "A relogin of same name still in progress.",
        };
    }
    if !is_active_mailbox(existing_base) {
        // Another logon still in progress (or no live base).
        return DuplicateLoginOutcome::Rejected {
            message: "Another login of same name still in progress.",
        };
    }
    // The RelogonAttemptHandler constructor registers the pending attempt.
    records.start_relogon(key);
    DuplicateLoginOutcome::StartRelogon {
        key,
        existing_base: existing_base.expect("checked active above"),
    }
}

/// The state of one re-logon attempt (`RelogonAttemptHandler`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelogonState {
    /// Waiting for the BaseApp's `logOnAttempt` reply.
    AwaitingReply,
    /// The BaseApp waits for the old session's destruction; the timeout is
    /// armed.
    WaitingForDestroy,
    /// The attempt is over; later events are ignored.
    Finished,
}

/// The result of one re-logon attempt event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelogonOutcome {
    /// `LOG_ON_ATTEMPT_TOOK_CONTROL`: the new client took over; the login
    /// replies `LOGGED_ON` with the proxy's address.
    LoggedOn { proxy_addr: Addr },
    /// `LOGIN_REJECTED_ALREADY_LOGGED_IN`.
    Rejected { message: &'static str },
    /// `LOGIN_REJECTED_BASEAPP_TIMEOUT`.
    BaseAppTimeout { message: String },
    /// The old entity logged off: abort and re-run `logOn` for this client.
    RetryLogin,
    /// `LOG_ON_ATTEMPT_WAIT_FOR_DESTROY`: wait for the old entity to be
    /// destroyed; the attempt stays pending.
    WaitForDestroy,
    /// The attempt already finished (or the event does not apply); nothing to
    /// do.
    Ignored,
}

/// One pending re-logon attempt: the `RelogonAttemptHandler` state.
#[derive(Clone, Debug, PartialEq)]
pub struct RelogonAttempt {
    key: EntityKey,
    state: RelogonState,
}

impl RelogonAttempt {
    /// Constructor; the pending attempt must already be registered in the
    /// online records (`on_duplicate_login` does this).
    pub fn new(key: EntityKey) -> Self {
        Self {
            key,
            state: RelogonState::AwaitingReply,
        }
    }

    /// `ekey_`.
    pub fn key(&self) -> EntityKey {
        self.key
    }

    /// The current state.
    pub fn state(&self) -> RelogonState {
        self.state
    }

    /// `RelogonAttemptHandler::handleMessage`: the `logOnAttempt` reply.
    ///
    /// `proxy_addr` is the address streamed with a `TOOK_CONTROL` reply.
    pub fn on_reply(
        &mut self,
        records: &mut LogOnRecords,
        result: u8,
        proxy_addr: Addr,
    ) -> RelogonOutcome {
        if self.state == RelogonState::Finished {
            // Ignoring BaseApp reply, re-logon attempt has been aborted.
            return RelogonOutcome::Ignored;
        }
        match result {
            LOG_ON_ATTEMPT_TOOK_CONTROL => {
                self.finish(records);
                RelogonOutcome::LoggedOn { proxy_addr }
            }
            LOG_ON_ATTEMPT_REJECTED => {
                self.finish(records);
                RelogonOutcome::Rejected {
                    message: "Relogin denied.",
                }
            }
            LOG_ON_ATTEMPT_WAIT_FOR_DESTROY => {
                self.state = RelogonState::WaitingForDestroy;
                RelogonOutcome::WaitForDestroy
            }
            _ => {
                // Invalid result: the reference treats this as fatal and
                // drops the handler without a reply.
                self.finish(records);
                RelogonOutcome::Ignored
            }
        }
    }

    /// `handleTimeout`: the wait-for-destroy timer fired.
    pub fn on_timeout(&mut self, records: &mut LogOnRecords) -> RelogonOutcome {
        if self.state != RelogonState::WaitingForDestroy {
            return RelogonOutcome::Ignored;
        }
        self.finish(records);
        RelogonOutcome::BaseAppTimeout {
            message: "Timeout waiting for previous login to disconnect.".to_string(),
        }
    }

    /// `handleException`: the BaseApp channel died.
    pub fn on_exception(&mut self, records: &mut LogOnRecords, reason: &str) -> RelogonOutcome {
        if self.state == RelogonState::Finished {
            return RelogonOutcome::Ignored;
        }
        self.finish(records);
        RelogonOutcome::BaseAppTimeout {
            message: reason.to_string(),
        }
    }

    /// `onEntityLogOff`: the entity the attempt waits for was destroyed;
    /// abort and re-run `logOn`.
    pub fn on_entity_logoff(&mut self, records: &mut LogOnRecords) -> RelogonOutcome {
        if self.state == RelogonState::Finished {
            return RelogonOutcome::Ignored;
        }
        self.finish(records);
        RelogonOutcome::RetryLogin
    }

    /// `abort` + destruction: complete the pending attempt in the records.
    fn finish(&mut self, records: &mut LogOnRecords) {
        self.state = RelogonState::Finished;
        records.complete_relogon(self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::EntityTypeID;

    const ACCOUNT_TYPE: EntityTypeID = 3;
    const ACCOUNT_DBID: crate::billing::DatabaseID = 42;

    fn key() -> EntityKey {
        EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID)
    }

    fn active_base() -> BaseMailbox {
        BaseMailbox {
            type_id: ACCOUNT_TYPE,
            id: 7,
            addr: (0x7f00_0001, 20000),
        }
    }

    #[test]
    fn an_active_base_starts_a_relogon_attempt() {
        let mut records = LogOnRecords::new();
        let outcome = on_duplicate_login(&mut records, key(), Some(active_base()));
        assert_eq!(
            outcome,
            DuplicateLoginOutcome::StartRelogon {
                key: key(),
                existing_base: active_base(),
            }
        );
        assert!(records.has_pending_relogon(key()));
    }

    #[test]
    fn a_second_relogon_for_the_same_entity_is_refused() {
        let mut records = LogOnRecords::new();
        on_duplicate_login(&mut records, key(), Some(active_base()));
        let outcome = on_duplicate_login(&mut records, key(), Some(active_base()));
        assert_eq!(
            outcome,
            DuplicateLoginOutcome::Rejected {
                message: "A relogin of same name still in progress.",
            }
        );
    }

    #[test]
    fn a_being_checked_out_entity_is_refused() {
        let mut records = LogOnRecords::new();
        // No base mailbox yet: the entity is only being checked out.
        let outcome = on_duplicate_login(&mut records, key(), None);
        assert_eq!(
            outcome,
            DuplicateLoginOutcome::Rejected {
                message: "Another login of same name still in progress.",
            }
        );
        // A zero-id or zero-address mailbox is not active either.
        let mut dead = active_base();
        dead.id = 0;
        assert!(!is_active_mailbox(Some(dead)));
        let mut no_addr = active_base();
        no_addr.addr = (0, 20000);
        assert!(!is_active_mailbox(Some(no_addr)));
        assert!(!is_active_mailbox(None));
    }

    /// BW-09-04-04: two clients authenticating the same account at once,
    /// interleaved — the records must end with exactly one session and no
    /// stale checkout or pending attempt.
    #[test]
    fn concurrent_logins_for_one_account_leave_one_record() {
        let mut records = LogOnRecords::new();

        // Client A starts its checkout; client B's checkout of the same
        // account at the same time is refused.
        assert!(records.start_checkout(key()));
        assert!(!records.start_checkout(key()));

        // A completes the checkout and its base mailbox is reserved and
        // recorded.
        assert!(records.complete_checkout(key()));
        records.insert_record(key(), active_base());

        // B authenticates while A's session is live: the duplicate login is
        // handed to A's base, and the attempt is pending.
        let existing = records.look_up_record(key());
        let mut attempt = match on_duplicate_login(&mut records, key(), existing) {
            DuplicateLoginOutcome::StartRelogon {
                key: k,
                existing_base,
            } => {
                assert_eq!(k, key());
                assert_eq!(existing_base, active_base());
                RelogonAttempt::new(k)
            }
            other => panic!("unexpected outcome: {other:?}"),
        };
        assert!(records.has_pending_relogon(key()));

        // A third client piling on during the wait is refused outright, and
        // the refusal leaves the pending attempt untouched.
        let existing = records.look_up_record(key());
        assert_eq!(
            on_duplicate_login(&mut records, key(), existing),
            DuplicateLoginOutcome::Rejected {
                message: "A relogin of same name still in progress.",
            }
        );
        assert!(records.has_pending_relogon(key()));

        // The base refuses B: A's session and record stay exactly as they
        // were.
        assert_eq!(
            attempt.on_reply(&mut records, LOG_ON_ATTEMPT_REJECTED, (0, 0)),
            RelogonOutcome::Rejected {
                message: "Relogin denied.",
            }
        );
        assert_eq!(records.look_up_record(key()), Some(active_base()));
        assert!(!records.has_pending_relogon(key()));

        // B retries and the base waits for the old session to go away.
        let existing = records.look_up_record(key());
        let mut attempt = match on_duplicate_login(&mut records, key(), existing) {
            DuplicateLoginOutcome::StartRelogon { key: k, .. } => RelogonAttempt::new(k),
            other => panic!("unexpected outcome: {other:?}"),
        };
        assert_eq!(
            attempt.on_reply(&mut records, LOG_ON_ATTEMPT_WAIT_FOR_DESTROY, (0, 0)),
            RelogonOutcome::WaitForDestroy
        );

        // The old entity logs off during the wait: the record is erased and
        // B's login is re-run.
        assert_eq!(
            records.apply_logoff(key()),
            crate::log_on_records::LogOffOutcome::RelogonWaiting
        );
        assert_eq!(
            attempt.on_entity_logoff(&mut records),
            RelogonOutcome::RetryLogin
        );
        assert!(records.look_up_record(key()).is_none());
        assert!(!records.has_pending_relogon(key()));
        assert!(!records.is_checking_out(key()));

        // B's retried login records its own base — one record, B's proxy.
        let b_base = BaseMailbox {
            type_id: ACCOUNT_TYPE,
            id: 9,
            addr: (0x7f00_0002, 30000),
        };
        records.insert_record(key(), b_base);
        assert_eq!(records.look_up_record(key()), Some(b_base));

        // B logs off: the record set is empty again.
        assert_eq!(
            records.apply_logoff(key()),
            crate::log_on_records::LogOffOutcome::None
        );
        assert!(records.look_up_record(key()).is_none());
    }

    #[test]
    fn a_take_control_reply_logs_the_new_session_on() {
        let mut records = LogOnRecords::new();
        on_duplicate_login(&mut records, key(), Some(active_base()));
        let mut attempt = RelogonAttempt::new(key());
        let outcome = attempt.on_reply(
            &mut records,
            LOG_ON_ATTEMPT_TOOK_CONTROL,
            (0x7f00_0002, 30000),
        );
        assert_eq!(
            outcome,
            RelogonOutcome::LoggedOn {
                proxy_addr: (0x7f00_0002, 30000)
            }
        );
        // The attempt is finished and no longer pending.
        assert_eq!(attempt.state(), RelogonState::Finished);
        assert!(!records.has_pending_relogon(key()));
        // A late reply is ignored.
        assert_eq!(
            attempt.on_reply(&mut records, LOG_ON_ATTEMPT_TOOK_CONTROL, (0, 0)),
            RelogonOutcome::Ignored
        );
    }

    #[test]
    fn a_rejected_reply_denies_the_relogin() {
        let mut records = LogOnRecords::new();
        on_duplicate_login(&mut records, key(), Some(active_base()));
        let mut attempt = RelogonAttempt::new(key());
        assert_eq!(
            attempt.on_reply(&mut records, LOG_ON_ATTEMPT_REJECTED, (0, 0)),
            RelogonOutcome::Rejected {
                message: "Relogin denied.",
            }
        );
        assert!(!records.has_pending_relogon(key()));
    }

    #[test]
    fn a_wait_for_destroy_reply_waits_then_times_out() {
        let mut records = LogOnRecords::new();
        on_duplicate_login(&mut records, key(), Some(active_base()));
        let mut attempt = RelogonAttempt::new(key());
        assert_eq!(
            attempt.on_reply(&mut records, LOG_ON_ATTEMPT_WAIT_FOR_DESTROY, (0, 0)),
            RelogonOutcome::WaitForDestroy
        );
        assert_eq!(attempt.state(), RelogonState::WaitingForDestroy);
        // Still pending while waiting.
        assert!(records.has_pending_relogon(key()));
        // The timer fires: the new session is told the old one never left.
        assert_eq!(
            attempt.on_timeout(&mut records),
            RelogonOutcome::BaseAppTimeout {
                message: "Timeout waiting for previous login to disconnect.".to_string(),
            }
        );
        assert!(!records.has_pending_relogon(key()));
    }

    #[test]
    fn an_entity_logoff_during_the_wait_retries_the_login() {
        let mut records = LogOnRecords::new();
        on_duplicate_login(&mut records, key(), Some(active_base()));
        let mut attempt = RelogonAttempt::new(key());
        attempt.on_reply(&mut records, LOG_ON_ATTEMPT_WAIT_FOR_DESTROY, (0, 0));
        // The old session's entity is destroyed: the DBApp wakes the attempt.
        assert_eq!(
            attempt.on_entity_logoff(&mut records),
            RelogonOutcome::RetryLogin
        );
        assert!(!records.has_pending_relogon(key()));
        // A timeout after the retry is ignored.
        assert_eq!(attempt.on_timeout(&mut records), RelogonOutcome::Ignored);
    }

    #[test]
    fn a_baseapp_channel_exception_times_the_attempt_out() {
        let mut records = LogOnRecords::new();
        on_duplicate_login(&mut records, key(), Some(active_base()));
        let mut attempt = RelogonAttempt::new(key());
        assert_eq!(
            attempt.on_exception(&mut records, "channel died"),
            RelogonOutcome::BaseAppTimeout {
                message: "channel died".to_string(),
            }
        );
        assert!(!records.has_pending_relogon(key()));
    }

    #[test]
    fn an_invalid_reply_code_finishes_without_a_reply() {
        let mut records = LogOnRecords::new();
        on_duplicate_login(&mut records, key(), Some(active_base()));
        let mut attempt = RelogonAttempt::new(key());
        assert_eq!(
            attempt.on_reply(&mut records, 99, (0, 0)),
            RelogonOutcome::Ignored
        );
        assert_eq!(attempt.state(), RelogonState::Finished);
        assert!(!records.has_pending_relogon(key()));
    }
}
