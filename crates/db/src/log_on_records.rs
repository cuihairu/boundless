//! The log-on record cache and the repeated-authentication arbitration.
//!
//! Two requests for the same account must not corrupt each other, which
//! is what this module verifies. The reference keeps the runtime view of
//! "where each logged-on entity's base lives" in
//! `LogOnRecordsCache` (`server/dbapp/log_on_records_cache.{hpp,cpp}`) and
//! refuses the second request in `DBApp::onLogOnLoggedOnUser`
//! (`dbapp.cpp:2153-2209`):
//!
//! - a re-logon already in progress → refused,
//! - a first log-on still in progress (the mailbox is not active yet) →
//!   refused,
//! - otherwise the attempt is forwarded to the entity's existing base.
//!
//! In every refused case the record is left exactly as it stood — the
//! second request only ever reads it.

use std::collections::BTreeMap;

use foundation::ids::{DatabaseId, EntityTypeId};
use mercury::endpoint::Address;

use crate::storage::{BaseMailbox, EntityKey};

/// `LogOnRecordsCache` (`log_on_records_cache.{hpp,cpp}`): `typeID` to
/// `dbID` to the entity's base mailbox.
#[derive(Debug, Default, PartialEq)]
pub struct LogOnRecordsCache {
    cache: BTreeMap<EntityTypeId, BTreeMap<DatabaseId, BaseMailbox>>,
}

impl LogOnRecordsCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// `lookUp`: the record for `key`, or nothing.
    pub fn look_up(&self, key: &EntityKey) -> Option<BaseMailbox> {
        let mailbox = self.cache.get(&key.type_id)?.get(&key.db_id)?;
        Some(mailbox.clone())
    }

    /// `insert`: one record per key, so a repeat for the same account
    /// replaces the standing record rather than adding a second.
    pub fn insert(&mut self, key: &EntityKey, mailbox: BaseMailbox) {
        self.cache
            .entry(key.type_id)
            .or_default()
            .insert(key.db_id, mailbox);
    }

    /// `erase`: removes the record. `false` is the reference's
    /// `ERROR_MSG "Could not find entity %ld of type %u"` — either the
    /// type or the id was never cached.
    pub fn erase(&mut self, key: &EntityKey) -> bool {
        match self.cache.get_mut(&key.type_id) {
            Some(per_type) => per_type.remove(&key.db_id).is_some(),
            None => false,
        }
    }

    /// `remapMailboxes`: every record pointing at `old_addr` is
    /// re-addressed through `address_for` — the reference copies only the
    /// ip and port, leaving the salt alone.
    pub fn remap_mailboxes(
        &mut self,
        old_addr: &Address,
        mut address_for: impl FnMut(i32) -> Address,
    ) {
        for per_type in self.cache.values_mut() {
            for mailbox in per_type.values_mut() {
                if &mailbox.addr == old_addr {
                    let new_addr = address_for(mailbox.entity_id);
                    mailbox.addr.ip = new_addr.ip;
                    mailbox.addr.port = new_addr.port;
                }
            }
        }
    }
}

/// `GetEntityHandler::isActiveMailBox`
/// (`server/dbapp/get_entity_handler.cpp:33-43`): the mailbox counts as
/// active when the entity has an id and a real address — a zeroed
/// address is the "pending base creation" state, which is what makes an
/// auto-load restored entity inactive until it is checked out.
pub fn is_active_mailbox(base: Option<&BaseMailbox>) -> bool {
    matches!(base, Some(base) if base.entity_id != 0 && base.addr.ip != 0)
}

/// What to do about a log-on request for an entity that is already
/// logged on.
#[derive(Debug, Clone, PartialEq)]
pub enum RelogonVerdict {
    /// `LOGGED_ON_OFFLINE`'s cousin — the reference refuses with
    /// [`RELOGIN_IN_PROGRESS_MSG`] (`dbapp.cpp:2162-2172`).
    RelogonInProg,
    /// The first log-on is still in progress: refused with
    /// [`LOGIN_IN_PROGRESS_MSG`] (`:2174-2184`).
    LoginInProg,
    /// Forward to the existing base (`:2186-2209`): the caller sends
    /// `BaseAppIntInterface::logOnAttempt` to `addr`.
    Forward {
        /// The base to forward to.
        base: BaseMailbox,
    },
}

/// `onLogOnLoggedOnUser`'s rejection text (`dbapp.cpp:2169-2170`).
pub const RELOGIN_IN_PROGRESS_MSG: &str = "A relogin of same name still in progress.";
/// `onLogOnLoggedOnUser`'s other rejection text (`:2181-2182`).
pub const LOGIN_IN_PROGRESS_MSG: &str = "Another login of same name still in progress.";

/// `DBApp::onLogOnLoggedOnUser` (`dbapp.cpp:2153-2209`): the repeated
/// authentication is refused in the two in-progress cases and forwarded
/// to the existing base otherwise.
pub fn arbitrate_repeated_auth(
    existing_base: Option<&BaseMailbox>,
    relogon_in_progress: bool,
) -> RelogonVerdict {
    if relogon_in_progress {
        return RelogonVerdict::RelogonInProg;
    }
    if !is_active_mailbox(existing_base) {
        return RelogonVerdict::LoginInProg;
    }
    RelogonVerdict::Forward {
        base: existing_base.unwrap().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::log_on_status;

    fn mailbox(entity_id: i32, ip: u32, port: u16, salt: u16) -> BaseMailbox {
        BaseMailbox {
            addr: Address { ip, port, salt },
            entity_id,
        }
    }

    fn key(type_id: EntityTypeId, db_id: DatabaseId) -> EntityKey {
        EntityKey { type_id, db_id }
    }

    #[test]
    fn the_cache_round_trips_one_record_per_key() {
        let mut cache = LogOnRecordsCache::new();
        let first = key(1, 7);
        assert_eq!(cache.look_up(&first), None);

        cache.insert(&first, mailbox(77, 0x0100_007F, 20_001, 9));
        assert_eq!(
            cache.look_up(&first),
            Some(mailbox(77, 0x0100_007F, 20_001, 9))
        );
        assert_eq!(cache.look_up(&key(1, 8)), None, "other id");
        assert_eq!(cache.look_up(&key(2, 7)), None, "other type");
    }

    /// A repeated insert for the same account replaces the record —
    /// the cache never grows a second one.
    #[test]
    fn a_repeat_insert_replaces_rather_than_duplicates() {
        let mut cache = LogOnRecordsCache::new();
        let first = key(1, 7);
        cache.insert(&first, mailbox(77, 0x0100_007F, 20_001, 9));
        cache.insert(&first, mailbox(78, 0x0200_007F, 20_002, 10));

        assert_eq!(
            cache.look_up(&first),
            Some(mailbox(78, 0x0200_007F, 20_002, 10))
        );
        assert_eq!(
            cache.look_up(&first),
            Some(mailbox(78, 0x0200_007F, 20_002, 10))
        );
    }

    /// Both missing legs of `erase` report nothing found.
    #[test]
    fn erase_reports_when_the_record_was_absent() {
        let mut cache = LogOnRecordsCache::new();
        let first = key(1, 7);
        assert!(!cache.erase(&first), "nothing cached at all");
        cache.insert(&first, mailbox(77, 0x0100_007F, 20_001, 9));
        assert!(cache.erase(&first), "the cached record is gone");
        assert!(!cache.erase(&first), "a second erase finds nothing");
        assert_eq!(cache.look_up(&first), None);
    }

    /// `remapMailboxes` moves only the matching addresses and keeps
    /// the salt, as the reference copies only ip and port.
    #[test]
    fn the_remap_moves_matching_addresses_and_keeps_the_salt() {
        let mut cache = LogOnRecordsCache::new();
        let moved = key(1, 7);
        let other = key(1, 8);
        let old = Address {
            ip: 0x0100_007F,
            port: 20_001,
            salt: 3,
        };
        cache.insert(&moved, mailbox(77, old.ip, old.port, 41));
        cache.insert(&other, mailbox(78, 0x0200_007F, 20_002, 42));

        cache.remap_mailboxes(&old, |entity_id| Address {
            ip: 0x0A00_0001,
            port: 20_100,
            salt: (entity_id as u16) % 100,
        });

        assert_eq!(
            cache.look_up(&moved),
            Some(mailbox(77, 0x0A00_0001, 20_100, 41)),
            "only ip and port move"
        );
        assert_eq!(
            cache.look_up(&other),
            Some(mailbox(78, 0x0200_007F, 20_002, 42)),
            "a different address is untouched"
        );
    }

    /// Every arm of the active-mailbox predicate: an id or a real
    /// address is missing and the mailbox is pending.
    #[test]
    fn a_mailbox_is_active_only_when_fully_checked_out() {
        assert!(!is_active_mailbox(None));
        assert!(!is_active_mailbox(Some(&mailbox(
            0,
            0x0100_007F,
            20_001,
            0
        ))));
        assert!(!is_active_mailbox(Some(&mailbox(77, 0, 20_001, 0))));
        assert!(is_active_mailbox(Some(&mailbox(
            77,
            0x0100_007F,
            20_001,
            0
        ))));
    }

    /// The arbitration: an in-progress re-logon wins first, an
    /// inactive record second, and only a live record forwards.
    #[test]
    fn the_arbitration_refuses_the_in_progress_cases() {
        let live = mailbox(77, 0x0100_007F, 20_001, 0);
        let pending = mailbox(77, 0, 0, 0);

        assert_eq!(
            arbitrate_repeated_auth(Some(&live), true),
            RelogonVerdict::RelogonInProg,
            "a re-logon in flight wins over an active base"
        );
        assert_eq!(
            arbitrate_repeated_auth(Some(&live), false),
            RelogonVerdict::Forward { base: live }
        );
        assert_eq!(
            arbitrate_repeated_auth(Some(&pending), false),
            RelogonVerdict::LoginInProg
        );
        assert_eq!(
            arbitrate_repeated_auth(None, false),
            RelogonVerdict::LoginInProg
        );
    }

    /// The acceptance: a refused second request leaves the log-on
    /// record exactly as it stood, and the rejection carries
    /// `LOGIN_REJECTED_ALREADY_LOGGED_IN`.
    #[test]
    fn a_repeated_request_cannot_pollute_the_record() {
        let record = mailbox(77, 0x0100_007F, 20_001, 9);
        let mut cache = LogOnRecordsCache::new();
        let key = key(1, 7);
        cache.insert(&key, record.clone());

        // First request: the base is live, so the attempt forwards and
        // the record is left alone.
        let verdict = arbitrate_repeated_auth(cache.look_up(&key).as_ref(), false);
        assert_eq!(
            verdict,
            RelogonVerdict::Forward {
                base: record.clone()
            }
        );

        // Second request while the first re-logon is in flight.
        assert_eq!(
            arbitrate_repeated_auth(cache.look_up(&key).as_ref(), true),
            RelogonVerdict::RelogonInProg
        );

        // Third request while the entity is still pending.
        let pending = mailbox(77, 0, 0, 0);
        assert_eq!(
            arbitrate_repeated_auth(Some(&pending), false),
            RelogonVerdict::LoginInProg
        );

        // Every refusal above only read: the cache still holds the one
        // record that stood there.
        assert_eq!(cache.look_up(&key), Some(record));
        assert_eq!(
            log_on_status::LOGIN_REJECTED_ALREADY_LOGGED_IN,
            69,
            "the status both refusals ride"
        );
        assert_eq!(
            RELOGIN_IN_PROGRESS_MSG,
            "A relogin of same name still in progress."
        );
        assert_eq!(
            LOGIN_IN_PROGRESS_MSG,
            "Another login of same name still in progress."
        );
    }
}
