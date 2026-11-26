//! OPT-06-01-04: re-run duplicate login, reconnect and the old client.
//!
//! The acceptance is "行为一致" — the behaviour is identical. This module
//! drives the three faces once per seed — the DBApp duplicate-login gate
//! with its re-logon attempt handler, the client reconnect through the real
//! proxy session and pending-ticket queue, and the old-client protocol
//! version gate on the replay prefix — and every decision is recorded in a
//! comparable ledger. A rerun from the same seed reproduces the ledger bit
//! for bit, and a run from a different seed produces the same decisions over
//! different addresses, which is the behavioural identity the acceptance
//! asks to verify.

use foundation::stream::BinaryStream;

use crate::baseapp_continuation::{ClientSwitch, LoggedOnReply, ProxySession, log_on_complete};
use crate::billing::EntityKey;
use crate::client_login::{LoginHandler, log_on_status};
use crate::log_on_records::LogOnRecords;
use crate::login_entity::BaseMailbox;
use crate::login_request::{
    LoginRequestCache, ResentCachedDecision, ServerAddress, handle_resent_cached_attempt,
};
use crate::pending_logins::{LoginDecision, PendingLogins};
use crate::relogon::{
    DuplicateLoginOutcome, LOG_ON_ATTEMPT_TOOK_CONTROL, LOG_ON_ATTEMPT_WAIT_FOR_DESTROY,
    RelogonAttempt, RelogonOutcome, on_duplicate_login,
};
use crate::replay_header::{
    ProtocolVersion, ReplayChecksumScheme, ReplayHeader, ReplayMetaData, read_replay_prefix,
    write_replay_prefix,
};

/// The default rerun seed.
pub const RERUN_SEED: u64 = 271_828;
/// The ticks the reconnect's pending ticket clock runs at.
pub const RERUN_HERTZ: u32 = 10;

/// One rerun's decisions across the three faces.
#[derive(Clone, Debug, PartialEq)]
pub struct RerunLedger {
    /// The first duplicate login against the live base.
    pub duplicate_first: DuplicateLoginOutcome,
    /// A second attempt while the first re-logon is still pending.
    pub duplicate_pending: DuplicateLoginOutcome,
    /// The BaseApp reply that gives the new client control.
    pub relogon_reply: RelogonOutcome,
    /// A reply arriving after the attempt finished.
    pub relogon_stale_reply: RelogonOutcome,
    /// The re-logon restarted after the first one finished.
    pub duplicate_restart: DuplicateLoginOutcome,
    /// The wait-for-destroy reply that arms the restarted attempt's timer.
    pub relogon_wait_armed: RelogonOutcome,
    /// The wait-for-destroy timeout the restarted attempt hits.
    pub relogon_timeout: RelogonOutcome,
    /// The entity logging off re-runs the login for the waiting client.
    pub relogon_logoff: RelogonOutcome,
    /// The base refusing a later attempt.
    pub relogon_rejected: RelogonOutcome,
    /// A true re-send (same credentials) answered from the cached request.
    pub resent_success: bool,
    /// The old session key the reconnecting client presents.
    pub reconnect_presented: u32,
    /// The pending-queue decision for the presented key.
    pub reconnect_decision: LoginDecision,
    /// A reconnect presenting a key the queue never issued.
    pub stale_ticket_decision: LoginDecision,
    /// The ticket whose proxy was already destroyed.
    pub destroyed_proxy_decision: LoginDecision,
    /// The ticket whose client already holds a channel.
    pub channel_collision_decision: LoginDecision,
    /// The regenerated key the switch hands the client.
    pub reconnect_new_key: u32,
    /// The reconnect's successful switch to the answering BaseApp.
    pub switch_continue: ClientSwitch,
    /// The switch for a reply whose BaseApp is not the one that answered.
    pub mismatched_switch: ClientSwitch,
    /// The switch for a reply whose client already logged off.
    pub stale_switch: ClientSwitch,
    /// The current-protocol client's replay prefix is accepted.
    pub current_client_accepted: bool,
    /// An old-protocol client's replay prefix is rejected.
    pub old_client_rejected: bool,
}

impl RerunLedger {
    /// The decision fields that must not change between reruns: every
    /// outcome kind and message, ignoring the seed-derived addresses, keys
    /// and ids the Debug forms embed.
    pub fn decisions(&self) -> Vec<String> {
        let duplicate = |o: &DuplicateLoginOutcome| match o {
            DuplicateLoginOutcome::StartRelogon { .. } => "StartRelogon".to_string(),
            DuplicateLoginOutcome::Rejected { message } => format!("Rejected({message})"),
        };
        let relogon = |o: &RelogonOutcome| match o {
            RelogonOutcome::LoggedOn { .. } => "LoggedOn".to_string(),
            RelogonOutcome::Rejected { message } => format!("Rejected({message})"),
            RelogonOutcome::BaseAppTimeout { message } => format!("BaseAppTimeout({message})"),
            RelogonOutcome::RetryLogin => "RetryLogin".to_string(),
            RelogonOutcome::WaitForDestroy => "WaitForDestroy".to_string(),
            RelogonOutcome::Ignored => "Ignored".to_string(),
        };
        let relogon_decision = |o: &LoginDecision| match o {
            LoginDecision::AttachClient { num_attempts, .. } => {
                format!("AttachClient(attempts={num_attempts})")
            }
            LoginDecision::InvalidTicket => "InvalidTicket".to_string(),
            LoginDecision::ProxyDestroyed => "ProxyDestroyed".to_string(),
            LoginDecision::ChannelCollision => "ChannelCollision".to_string(),
        };
        let switch = |o: &ClientSwitch| match o {
            ClientSwitch::ContinueToBaseApp(_) => "ContinueToBaseApp".to_string(),
            ClientSwitch::AlreadyLoggedOff => "AlreadyLoggedOff".to_string(),
            ClientSwitch::LoginFailed { status } => format!("LoginFailed({status})"),
        };
        vec![
            duplicate(&self.duplicate_first),
            duplicate(&self.duplicate_pending),
            relogon(&self.relogon_reply),
            relogon(&self.relogon_stale_reply),
            duplicate(&self.duplicate_restart),
            relogon(&self.relogon_wait_armed),
            relogon(&self.relogon_timeout),
            relogon(&self.relogon_logoff),
            relogon(&self.relogon_rejected),
            format!("{}", self.resent_success),
            relogon_decision(&self.reconnect_decision),
            relogon_decision(&self.stale_ticket_decision),
            relogon_decision(&self.destroyed_proxy_decision),
            relogon_decision(&self.channel_collision_decision),
            switch(&self.switch_continue),
            switch(&self.mismatched_switch),
            switch(&self.stale_switch),
            format!("{}", self.current_client_accepted),
            format!("{}", self.old_client_rejected),
        ]
    }
}

/// Run the three faces once at the given seed. The seed derives the entity
/// id, the addresses and the session clocks; the decisions are structural.
pub fn run_login_rerun(seed: u64) -> RerunLedger {
    let base_ip = 0x7f00_0001u32.wrapping_add(seed as u32 % 16);
    let mailbox = BaseMailbox {
        type_id: 1,
        id: seed as u32 + 1,
        addr: (base_ip, 21_000),
    };
    let key = EntityKey::new(1, (seed % 0xFFFF_FFFF) as u32 + 1);

    // Duplicate login: the live base starts a re-logon; a second attempt
    // while one is pending is rejected with the reference message. The
    // reply that takes control finishes the attempt; a later reply is
    // stale and ignored.
    let mut records = LogOnRecords::new();
    records.insert_record(key, mailbox);
    let duplicate_first = on_duplicate_login(&mut records, key, Some(mailbox));
    let duplicate_pending = on_duplicate_login(&mut records, key, Some(mailbox));
    let mut attempt = RelogonAttempt::new(key);
    let relogon_reply = attempt.on_reply(&mut records, LOG_ON_ATTEMPT_TOOK_CONTROL, mailbox.addr);
    let relogon_stale_reply =
        attempt.on_reply(&mut records, LOG_ON_ATTEMPT_TOOK_CONTROL, mailbox.addr);

    // The restarted attempt arms the wait-for-destroy timeout, which fires
    // with the reference message; the restarted-after-that attempt is
    // aborted by the old entity logging off, which re-runs the login.
    let duplicate_restart = on_duplicate_login(&mut records, key, Some(mailbox));
    let mut wait = RelogonAttempt::new(key);
    let relogon_wait_armed =
        wait.on_reply(&mut records, LOG_ON_ATTEMPT_WAIT_FOR_DESTROY, mailbox.addr);
    let relogon_timeout = wait.on_timeout(&mut records);
    let duplicate_again = on_duplicate_login(&mut records, key, Some(mailbox));
    let mut waited = RelogonAttempt::new(key);
    let relogon_logoff = waited.on_entity_logoff(&mut records);
    let _ = duplicate_again;

    // A later attempt the base refuses outright.
    let duplicate_refused = on_duplicate_login(&mut records, key, Some(mailbox));
    let mut refused = RelogonAttempt::new(key);
    let relogon_rejected = refused.on_reply(
        &mut records,
        crate::relogon::LOG_ON_ATTEMPT_REJECTED,
        mailbox.addr,
    );
    let _ = duplicate_refused;

    // A true re-send: the same credentials hitting the cached success are
    // answered from the cache, not re-driven through the DBApp.
    let mut cache = LoginRequestCache::new();
    let addr = ServerAddress::new(base_ip, 20_000, seed as u16);
    let params = crate::loginapp::LogOnParams::with_credentials("rerun", "password", "");
    cache.entry(addr).set_params(params.clone());
    let now = seed * 1_000_000_000;
    cache.entry(addr).set_data(
        crate::login_request::LoginReplyRecord::new(addr, seed as u32),
        "welcome",
        now,
    );
    let resent_success = matches!(
        handle_resent_cached_attempt(&cache, addr, &params, now),
        ResentCachedDecision::ResentSuccess
    );

    // Reconnect: the client comes back presenting the OLD session key; the
    // pending queue hands it the ticket and the switch completes with the
    // regenerated key. A reply for a client that already logged off stays
    // a no-op switch.
    let mut session = ProxySession::new(now);
    let presented = session.prepare_for_login((base_ip, 20_000), now + 1);
    let mut pending = PendingLogins::new_default();
    pending.add(
        seed as u32 + 7,
        presented,
        (base_ip, 20_000),
        0,
        RERUN_HERTZ,
    );
    let reconnect_decision = pending.login(presented, false, false, 2);
    // The other pending-queue faces a reconnect can hit: a key the queue
    // never issued, a ticket whose proxy already died, and a client that
    // already holds a channel.
    let stale_ticket_decision = pending.login(presented.wrapping_add(1_000), false, false, 1);
    pending.add(
        seed as u32 + 8,
        presented.wrapping_add(2_000),
        (base_ip, 20_000),
        0,
        RERUN_HERTZ,
    );
    let destroyed_proxy_decision = pending.login(presented.wrapping_add(2_000), true, false, 1);
    pending.add(
        seed as u32 + 9,
        presented.wrapping_add(3_000),
        (base_ip, 20_000),
        0,
        RERUN_HERTZ,
    );
    let channel_collision_decision = pending.login(presented.wrapping_add(3_000), false, true, 1);
    let reconnect_new_key = session.session_key();
    let switch_continue = log_on_complete(
        crate::loginapp::LogOnStatus::LoggedOn.as_u8(),
        true,
        (base_ip, 21_000),
        Some(LoggedOnReply {
            server_addr: (base_ip, 21_000),
            session_key: reconnect_new_key,
        }),
    );
    let mismatched_switch = log_on_complete(
        crate::loginapp::LogOnStatus::LoggedOn.as_u8(),
        true,
        (base_ip, 21_000),
        Some(LoggedOnReply {
            server_addr: (base_ip, 21_001),
            session_key: reconnect_new_key,
        }),
    );
    let stale_switch = log_on_complete(
        crate::loginapp::LogOnStatus::LoggedOn.as_u8(),
        false,
        (base_ip, 21_000),
        Some(LoggedOnReply {
            server_addr: (base_ip, 21_000),
            session_key: reconnect_new_key,
        }),
    );

    // Old client: the replay prefix protocol-version gate. The current
    // client's prefix verifies; a copy stamped with a different protocol
    // version is rejected at the same boundary.
    let scheme = ReplayChecksumScheme::new(8);
    let header = ReplayHeader::new(&scheme, [0u8; 16], 20, 0, [0u8; 8]);
    let mut prefix = BinaryStream::new();
    write_replay_prefix(&scheme, &header, &ReplayMetaData::new(), &mut prefix);
    let bytes = prefix.as_bytes().to_vec();
    let current_client_accepted =
        read_replay_prefix(&mut BinaryStream::from_slice(&bytes), &scheme, false).is_ok();
    let old_client_rejected = {
        let mut foreign = bytes.clone();
        let version = ProtocolVersion {
            subpatch: 0,
            patch: 1,
            ..ProtocolVersion::current()
        };
        foreign[0..4].copy_from_slice(&[
            version.subpatch,
            version.patch,
            version.minor,
            version.major,
        ]);
        read_replay_prefix(&mut BinaryStream::from_slice(&foreign), &scheme, false)
            .map(|_| ())
            .is_err()
    };

    RerunLedger {
        duplicate_first,
        duplicate_pending,
        relogon_reply,
        relogon_stale_reply,
        duplicate_restart,
        relogon_wait_armed,
        relogon_timeout,
        relogon_logoff,
        relogon_rejected,
        resent_success,
        reconnect_presented: presented,
        reconnect_decision,
        stale_ticket_decision,
        destroyed_proxy_decision,
        channel_collision_decision,
        reconnect_new_key,
        switch_continue,
        mismatched_switch,
        stale_switch,
        current_client_accepted,
        old_client_rejected,
    }
}

/// The `LoginHandler` guard the reconnect's accepted ticket walks; kept as a
/// helper so the scenario exercises the same start face the flood does.
pub fn reconnect_start(base_addr: (u32, u16), key: u32) -> crate::client_login::StartOutcome {
    let mut handler = LoginHandler::new(crate::client_login::LogOnStatus::new(
        log_on_status::NOT_SET,
    ));
    handler.start_with_base_addr(base_addr, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rerun_reproduces_from_its_seed() {
        assert_eq!(run_login_rerun(RERUN_SEED), run_login_rerun(RERUN_SEED));
    }

    #[test]
    fn the_duplicate_login_faces_are_stable() {
        let run = run_login_rerun(RERUN_SEED);
        assert!(matches!(
            run.duplicate_first,
            DuplicateLoginOutcome::StartRelogon { .. }
        ));
        assert_eq!(
            run.duplicate_pending,
            DuplicateLoginOutcome::Rejected {
                message: "A relogin of same name still in progress."
            }
        );
        assert!(matches!(run.relogon_reply, RelogonOutcome::LoggedOn { .. }));
        assert_eq!(run.relogon_stale_reply, RelogonOutcome::Ignored);
        assert!(matches!(
            run.duplicate_restart,
            DuplicateLoginOutcome::StartRelogon { .. }
        ));
        assert_eq!(
            run.relogon_timeout,
            RelogonOutcome::BaseAppTimeout {
                message: "Timeout waiting for previous login to disconnect.".to_string()
            }
        );
        assert_eq!(run.relogon_logoff, RelogonOutcome::RetryLogin);
        assert_eq!(run.relogon_wait_armed, RelogonOutcome::WaitForDestroy);
        assert_eq!(
            run.relogon_rejected,
            RelogonOutcome::Rejected {
                message: "Relogin denied."
            }
        );
    }

    #[test]
    fn the_reconnect_faces_are_stable() {
        let run = run_login_rerun(RERUN_SEED);
        assert!(run.resent_success);
        assert!(matches!(
            run.reconnect_decision,
            LoginDecision::AttachClient {
                proxy_id,
                num_attempts: 2,
                ..
            } if proxy_id == RERUN_SEED as u32 + 7
        ));
        assert_eq!(run.stale_ticket_decision, LoginDecision::InvalidTicket);
        assert_eq!(run.destroyed_proxy_decision, LoginDecision::ProxyDestroyed);
        assert_eq!(
            run.channel_collision_decision,
            LoginDecision::ChannelCollision
        );
        assert_ne!(run.reconnect_new_key, run.reconnect_presented);
        assert!(matches!(
            run.switch_continue,
            ClientSwitch::ContinueToBaseApp(_)
        ));
        assert_eq!(
            run.mismatched_switch,
            ClientSwitch::LoginFailed { status: 1 }
        );
        assert_eq!(run.stale_switch, ClientSwitch::AlreadyLoggedOff);
        assert_eq!(
            reconnect_start((0x7f00_0001, 20_001), 9),
            crate::client_login::StartOutcome::Started
        );
    }

    #[test]
    fn the_old_client_gate_is_stable() {
        let run = run_login_rerun(RERUN_SEED);
        assert!(run.current_client_accepted);
        assert!(run.old_client_rejected);
    }

    #[test]
    fn the_shape_holds_across_seeds() {
        let a = run_login_rerun(RERUN_SEED);
        let b = run_login_rerun(RERUN_SEED + 3);
        assert_eq!(a.decisions(), b.decisions());
        assert_ne!(a.reconnect_presented, b.reconnect_presented);
        assert_ne!(a.reconnect_new_key, b.reconnect_new_key);
    }
}
