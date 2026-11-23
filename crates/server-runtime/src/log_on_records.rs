//! BW-06-02-03 the DBApp's online logon records.
//!
//! The DBApp is the authority for which entity is logged on where. It keeps
//! the in-memory `LogOnRecordsCache` (`server/dbapp/log_on_records_cache.*`),
//! the set of entities in the middle of being checked out
//! (`inProgCheckouts_`), and the pending re-logon attempts
//! (`pendingAttempts_`, `dbapp.cpp`):
//!
//! * a login inserts a record once its base mailbox is reserved
//!   (`LoginHandler::onPutEntityComplete` → `logOnRecordsCache().insert`);
//! * an entity is in the `inProgCheckouts_` set from `onStartEntityCheckout`
//!   until `onCompleteEntityCheckout` (on a successful or failed logon);
//! * a `WRITE_LOG_OFF` write (`base.cpp` destroying a base entity) erases the
//!   cache record and, via `DBApp::onEntityLogOff`, tells a waiting re-logon
//!   handler that the old session is gone so it can re-run `logOn`
//!   (`relogon_attempt_handler.cpp`);
//! * a rejected login with a reserved base location clears that location
//!   (`LoginHandler::handleFailure` → `clearBaseEntityLocation`) and completes
//!   the checkout with no base.
//!
//! The DB writes (the `bigworldLogOns` table) are the caller's responsibility;
//! this module records the DBApp-side state the writes keep in sync.

use std::collections::{HashMap, HashSet};

use crate::billing::{DatabaseID, EntityKey, EntityTypeID};
use crate::login_entity::BaseMailbox;

/// The `LogOnRecordsCache` (`log_on_records_cache.hpp`): a nested
/// typeID -> dbID -> mailbox map.
#[derive(Debug, Default, Clone)]
pub struct LogOnRecordsCache {
    cache: HashMap<EntityTypeID, HashMap<DatabaseID, BaseMailbox>>,
}

impl LogOnRecordsCache {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `insert`.
    pub fn insert(&mut self, key: EntityKey, mailbox: BaseMailbox) {
        self.cache
            .entry(key.type_id)
            .or_default()
            .insert(key.db_id, mailbox);
    }

    /// `erase`: whether the record existed (the reference logs an error when
    /// it is missing).
    pub fn erase(&mut self, key: EntityKey) -> bool {
        let mut erased = false;
        if let Some(by_db_id) = self.cache.get_mut(&key.type_id) {
            erased = by_db_id.remove(&key.db_id).is_some();
            if by_db_id.is_empty() {
                self.cache.remove(&key.type_id);
            }
        }
        erased
    }

    /// `lookUp`.
    pub fn look_up(&self, key: EntityKey) -> Option<BaseMailbox> {
        self.cache
            .get(&key.type_id)
            .and_then(|by_db_id| by_db_id.get(&key.db_id))
            .copied()
    }

    /// `remapMailboxes`: re-point every record on `old_addr` to the destination
    /// the backup hash chose for that entity id.
    pub fn remap_mailboxes(
        &mut self,
        old_addr: crate::baseappmgr::Addr,
        address_for: impl Fn(u32) -> crate::baseappmgr::Addr,
    ) {
        for by_db_id in self.cache.values_mut() {
            for mailbox in by_db_id.values_mut() {
                if mailbox.addr == old_addr {
                    mailbox.addr = address_for(mailbox.id);
                }
            }
        }
    }
}

/// The DBApp-side online logon record state.
#[derive(Debug, Default)]
pub struct LogOnRecords {
    cache: LogOnRecordsCache,
    in_prog_checkouts: HashSet<EntityKey>,
    pending_relogon: HashSet<EntityKey>,
}

impl LogOnRecords {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// The `LogOnRecordsCache`.
    pub fn cache(&self) -> &LogOnRecordsCache {
        &self.cache
    }

    /// `logOnRecordsCache().insert`.
    pub fn insert_record(&mut self, key: EntityKey, mailbox: BaseMailbox) {
        self.cache.insert(key, mailbox);
    }

    /// `logOnRecordsCache().lookUp`.
    pub fn look_up_record(&self, key: EntityKey) -> Option<BaseMailbox> {
        self.cache.look_up(key)
    }

    /// `logOnRecordsCache().erase`.
    pub fn erase_record(&mut self, key: EntityKey) -> bool {
        self.cache.erase(key)
    }

    /// `onStartEntityCheckout`: whether the entity is not already in the
    /// process of being checked out.
    pub fn start_checkout(&mut self, key: EntityKey) -> bool {
        self.in_prog_checkouts.insert(key)
    }

    /// `onCompleteEntityCheckout`: whether the entity was in the process of
    /// being checked out. The caller notifies any checkout-completion
    /// listeners registered for it.
    pub fn complete_checkout(&mut self, key: EntityKey) -> bool {
        self.in_prog_checkouts.remove(&key)
    }

    /// Whether the entity is currently in the process of being checked out.
    pub fn is_checking_out(&self, key: EntityKey) -> bool {
        self.in_prog_checkouts.contains(&key)
    }

    /// `onStartRelogonAttempt`: whether the attempt was newly registered (a
    /// second attempt for the same key is rejected).
    pub fn start_relogon(&mut self, key: EntityKey) -> bool {
        self.pending_relogon.insert(key)
    }

    /// `onCompleteRelogonAttempt`: whether a pending attempt was removed.
    pub fn complete_relogon(&mut self, key: EntityKey) -> bool {
        self.pending_relogon.remove(&key)
    }

    /// `getInProgRelogonAttempt`.
    pub fn has_pending_relogon(&self, key: EntityKey) -> bool {
        self.pending_relogon.contains(&key)
    }

    /// `DBApp::onEntityLogOff` plus the cache erase the log-off write performs
    /// (`write_entity_handler.cpp` `finalise`): the entity is gone.
    pub fn apply_logoff(&mut self, key: EntityKey) -> LogOffOutcome {
        let relogon_was_waiting = self.pending_relogon.remove(&key);
        self.cache.erase(key);
        if relogon_was_waiting {
            LogOffOutcome::RelogonWaiting
        } else {
            LogOffOutcome::None
        }
    }
}

/// The result of an entity logging off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogOffOutcome {
    /// No re-logon handler was waiting.
    None,
    /// A re-logon handler was waiting for the old session; the caller re-runs
    /// `logOn` for it.
    RelogonWaiting,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT_TYPE: EntityTypeID = 3;
    const ACCOUNT_DBID: DatabaseID = 42;

    fn key() -> EntityKey {
        EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID)
    }

    fn mailbox() -> BaseMailbox {
        BaseMailbox {
            type_id: ACCOUNT_TYPE,
            id: 7,
            addr: (0x7f00_0001, 20000),
        }
    }

    #[test]
    fn insert_lookup_and_erase_round_trip() {
        let mut cache = LogOnRecordsCache::new();
        assert_eq!(cache.look_up(key()), None);
        cache.insert(key(), mailbox());
        assert_eq!(cache.look_up(key()), Some(mailbox()));
        assert!(cache.erase(key()));
        assert_eq!(cache.look_up(key()), None);
        // Erasing a missing record reports it.
        assert!(!cache.erase(key()));
    }

    #[test]
    fn the_cache_is_keyed_by_type_then_dbid() {
        let mut cache = LogOnRecordsCache::new();
        let other_type = EntityKey::new(ACCOUNT_TYPE + 1, ACCOUNT_DBID);
        cache.insert(key(), mailbox());
        assert_eq!(cache.look_up(other_type), None);
        assert_eq!(cache.look_up(EntityKey::new(ACCOUNT_TYPE, 99)), None);
    }

    #[test]
    fn remap_mailboxes_repoints_matching_addresses() {
        let mut cache = LogOnRecordsCache::new();
        cache.insert(key(), mailbox());
        cache.remap_mailboxes((0x7f00_0001, 20000), |_id| (0x7f00_0002, 30000));
        assert_eq!(cache.look_up(key()).unwrap().addr, (0x7f00_0002, 30000));
        // An address on another record is untouched.
        cache.remap_mailboxes((0x0a00_0001, 1), |_id| (0, 0));
        assert_eq!(cache.look_up(key()).unwrap().addr, (0x7f00_0002, 30000));
    }

    #[test]
    fn a_checkout_is_marked_in_progress_until_completed() {
        let mut records = LogOnRecords::new();
        assert!(records.start_checkout(key()));
        assert!(records.is_checking_out(key()));
        // A second checkout for the same entity is refused.
        assert!(!records.start_checkout(key()));
        assert!(records.complete_checkout(key()));
        assert!(!records.is_checking_out(key()));
        // Completing again reports nothing was in progress.
        assert!(!records.complete_checkout(key()));
    }

    #[test]
    fn a_relogon_attempt_is_tracked_and_completed() {
        let mut records = LogOnRecords::new();
        assert!(records.start_relogon(key()));
        assert!(records.has_pending_relogon(key()));
        // A second attempt is refused.
        assert!(!records.start_relogon(key()));
        assert!(records.complete_relogon(key()));
        assert!(!records.has_pending_relogon(key()));
        assert!(!records.complete_relogon(key()));
    }

    #[test]
    fn a_logoff_erases_the_record_and_flags_a_waiting_relogon() {
        let mut records = LogOnRecords::new();
        records.insert_record(key(), mailbox());
        assert_eq!(records.look_up_record(key()), Some(mailbox()));
        // No pending re-logon: a plain logoff.
        assert_eq!(records.apply_logoff(key()), LogOffOutcome::None);
        assert_eq!(records.look_up_record(key()), None);

        // A re-logon handler waiting for the old session is told to re-run.
        records.insert_record(key(), mailbox());
        records.start_relogon(key());
        assert_eq!(records.apply_logoff(key()), LogOffOutcome::RelogonWaiting);
        assert!(!records.has_pending_relogon(key()));
        assert_eq!(records.look_up_record(key()), None);
    }

    #[test]
    fn a_rejection_clears_the_checkout_with_no_base() {
        let mut records = LogOnRecords::new();
        records.start_checkout(key());
        // The rejected login completes its checkout with no base mailbox.
        assert!(records.complete_checkout(key()));
        assert!(!records.is_checking_out(key()));
    }
}
