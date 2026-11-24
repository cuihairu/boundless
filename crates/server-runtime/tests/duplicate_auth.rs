//! BW-09-04-04: repeated authentication must not pollute the login record.
//!
//! The acceptance (并发请求不污染登录记录) drives the reference's duplicate
//! path end to end. A login for an entity that is already logged on is
//! refused or handed to the existing base by
//! `DBApp::onLogOnLoggedOnUser` (`dbapp.cpp:2146-2201`), reached from
//! `LoginHandler::checkOutEntity` when the base is already checked out
//! (`login_handler.cpp:284-306`):
//!
//! * a re-logon already in progress — `LOGIN_REJECTED_ALREADY_LOGGED_IN`
//!   "A relogin of same name still in progress.";
//! * the base merely in the process of being checked out
//!   (`GetEntityHandler::isActiveMailBox` false) — "Another login of same
//!   name still in progress.";
//! * otherwise the `logOnAttempt` request goes to the existing base.
//!
//! The DBApp is the authority for the records (`LogOnRecordsCache`,
//! `inProgCheckouts_`, `pendingAttempts_`), and the billing system is the
//! authority for the logon mapping. Neither may gain a second entry for one
//! account: the cache is keyed by `(typeID, dbID)` and updated in place, the
//! pending-attempt set holds at most one attempt per key, and the mapping
//! write overwrites the entry found for the username
//! (`xml_billing_system.cpp:122-159`) instead of appending one.
//!
//! The requests below interleave rather than race: the DBApp's event loop
//! serialises them, so the port models concurrency as interleaved arrival
//! (the second request landing while the first re-logon is still pending).

use server_runtime::billing::{
    AccountResolution, BillingConfig, DatabaseID, EntityKey, EntityTypeID,
};
use server_runtime::billing::{LogonMapBillingSystem, INVALID_ENTITY_TYPE_ID};
use server_runtime::billing_creator::BillingSystem;
use server_runtime::log_on_records::{LogOffOutcome, LogOnRecords};
use server_runtime::login_entity::BaseMailbox;
use server_runtime::loginapp::LogOnStatus;
use server_runtime::relogon::{on_duplicate_login, DuplicateLoginOutcome};

const ACCOUNT_TYPE: EntityTypeID = 3;
const ACCOUNT_DBID: DatabaseID = 42;
const ENTITY_ID: u32 = 7;

fn account_key() -> EntityKey {
    EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID)
}

fn mailbox() -> BaseMailbox {
    BaseMailbox {
        type_id: ACCOUNT_TYPE,
        id: ENTITY_ID,
        addr: (0x7f00_0001, 20000),
    }
}

/// A logon mapping written once, as the first login's `setEntityKeyForAccount`
/// leaves it.
fn billing_with_alice() -> LogonMapBillingSystem {
    let mut billing = LogonMapBillingSystem::new(BillingConfig::new());
    billing.add_mapping("alice", account_key(), "pw");
    billing
}

/// The billing record: one repeated authentication, then a rewritten
/// credential, must hold one mapping for the account — the reference
/// `setEntityKeyForAccount` looks the section up by `logOnName`, writes it
/// in place, and reports success
/// (`xml_billing_system.cpp:122-159`).
#[test]
fn repeated_authentication_rewrites_one_mapping_in_place() {
    let mut billing = billing_with_alice();

    // The second login of the same account: the mapping is looked up and the
    // same entity key comes back.
    assert_eq!(
        billing.get_entity_key_for_account("alice", "pw"),
        AccountResolution::Success {
            entity_key: account_key(),
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );

    // The credential is rewritten: success again, and one mapping left — the
    // old password no longer answers, so nothing was appended beside it.
    assert_eq!(
        billing.set_entity_key_for_account("alice", "newpw", account_key()),
        server_runtime::billing_creator::SetEntityKeyOutcome::Success(account_key())
    );
    assert_eq!(
        billing.get_entity_key_for_account("alice", "pw"),
        AccountResolution::Failure {
            status: LogOnStatus::LoginRejectedInvalidPassword,
            error_msg: String::new(),
        }
    );
    assert_eq!(
        billing.get_entity_key_for_account("alice", "newpw"),
        AccountResolution::Success {
            entity_key: account_key(),
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );

    // A third account written the same way answers for itself: no crosstalk.
    let other_key = EntityKey::new(ACCOUNT_TYPE, 43);
    billing.add_mapping("bob", other_key, "secret");
    assert_eq!(
        billing.get_entity_key_for_account("bob", "secret"),
        AccountResolution::Success {
            entity_key: other_key,
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    );
    assert_eq!(
        billing.get_entity_key_for_account("bob", "pw"),
        AccountResolution::Failure {
            status: LogOnStatus::LoginRejectedInvalidPassword,
            error_msg: String::new(),
        }
    );
}

/// The duplicate-login sequence: the first re-logon is admitted and its
/// attempt registered, every request landing while it is pending is refused
/// without touching the record, and the refusal leaves exactly one pending
/// attempt to complete.
#[test]
fn a_concurrent_request_is_refused_without_polluting_the_record() {
    let mailbox = mailbox();
    let mut records = LogOnRecords::new();
    records.insert_record(account_key(), mailbox);
    assert!(!records.has_pending_relogon(account_key()));

    // The entity is checked out, so the second login reaches
    // `onLogOnLoggedOnUser` (`login_handler.cpp:284-306`).
    assert!(records.start_checkout(account_key()));
    // A checkout is admitted once: the second request for the same entity is
    // already in progress.
    assert!(!records.start_checkout(account_key()));
    assert!(records.is_checking_out(account_key()));

    let first = on_duplicate_login(&mut records, account_key(), Some(mailbox));
    assert_eq!(
        first,
        DuplicateLoginOutcome::StartRelogon {
            key: account_key(),
            existing_base: mailbox,
        }
    );

    // A second request for the same account, arriving while the first is
    // still pending: refused, the record left alone
    // (dbapp.cpp:2159-2167).
    let duplicate = on_duplicate_login(&mut records, account_key(), Some(mailbox));
    assert_eq!(
        duplicate,
        DuplicateLoginOutcome::Rejected {
            message: "A relogin of same name still in progress.",
        }
    );

    // The cache still holds the original base, not the duplicate's
    // interpretation of it.
    assert_eq!(records.look_up_record(account_key()), Some(mailbox));
    // The checkout of the entity is still in progress.
    assert!(records.is_checking_out(account_key()));

    // One attempt was registered: it completes, and a second completion finds
    // nothing, so the refusal added no attempt of its own.
    assert!(records.has_pending_relogon(account_key()));
    assert!(records.complete_relogon(account_key()));
    assert!(!records.has_pending_relogon(account_key()));
    assert!(!records.complete_relogon(account_key()));

    // The checkout completes with the entity, once.
    assert!(records.complete_checkout(account_key()));
    assert!(!records.is_checking_out(account_key()));
}

/// An entity whose base is only being reserved — `isActiveMailBox` false — is
/// refused without a re-logon attempt and without touching the record.
#[test]
fn an_in_progress_checkout_refuses_without_an_attempt() {
    let mut records = LogOnRecords::new();
    records.start_checkout(account_key());

    let outcome = on_duplicate_login(&mut records, account_key(), None);
    assert_eq!(
        outcome,
        DuplicateLoginOutcome::Rejected {
            message: "Another login of same name still in progress.",
        }
    );

    // No attempt registered, the record never written, the checkout still held.
    assert!(!records.has_pending_relogon(account_key()));
    assert_eq!(records.look_up_record(account_key()), None);
    assert!(records.is_checking_out(account_key()));
}

/// The admitted re-logon ends with the old session logging off: the cache
/// record is erased and the waiting attempt is woken, and the retried login
/// writes exactly one record again.
#[test]
fn the_logoff_of_a_relogoned_entity_leaves_one_record() {
    let mailbox = mailbox();
    let mut records = LogOnRecords::new();
    records.insert_record(account_key(), mailbox);

    assert_eq!(
        on_duplicate_login(&mut records, account_key(), Some(mailbox)),
        DuplicateLoginOutcome::StartRelogon {
            key: account_key(),
            existing_base: mailbox,
        }
    );

    // The old base is destroyed: `onEntityLogOff` wakes the attempt and the
    // log-off write erases the record.
    assert_eq!(
        records.apply_logoff(account_key()),
        LogOffOutcome::RelogonWaiting
    );
    assert_eq!(records.look_up_record(account_key()), None);
    assert!(!records.has_pending_relogon(account_key()));

    // The retried login reserves the base and records it once.
    let taken_over = BaseMailbox {
        type_id: ACCOUNT_TYPE,
        id: ENTITY_ID,
        addr: (0x7f00_0002, 20001),
    };
    records.insert_record(account_key(), taken_over);
    assert_eq!(records.look_up_record(account_key()), Some(taken_over));
    // The previous record is not a second entry beside the new one: the same
    // key looks the same mailbox up, and erasing it leaves nothing behind.
    assert!(records.erase_record(account_key()));
    assert_eq!(records.look_up_record(account_key()), None);
    assert!(!records.erase_record(account_key()));
}

/// The custom extension, asked for the same account twice, answers the same
/// `CreateNew` both times and never invents a record of its own
/// (`custom_billing_system.cpp:70-102`).
#[test]
fn the_custom_extension_is_repeatable_for_one_account() {
    let mut extension = server_runtime::billing_creator::CustomBillingSystem::new(|name| {
        if name == "Account" {
            ACCOUNT_TYPE
        } else {
            INVALID_ENTITY_TYPE_ID
        }
    });
    assert!(extension.is_okay());

    let answer = AccountResolution::CreateNew {
        entity_type: ACCOUNT_TYPE,
        should_remember: false,
        data_for_client: String::new(),
        data_for_base_entity: "pw".to_string(),
    };
    assert_eq!(extension.get_entity_key_for_account("alice", "pw"), answer);
    // Repeated for the same account: the same answer, nothing remembered.
    assert_eq!(extension.get_entity_key_for_account("alice", "pw"), answer);
    assert_eq!(
        extension.set_entity_key_for_account("alice", "pw", account_key()),
        server_runtime::billing_creator::SetEntityKeyOutcome::Success(account_key())
    );
}
