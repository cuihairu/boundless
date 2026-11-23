//! The account tables: `bigworldLogOnMapping`, `bigworldInfo`,
//! `bigworldEntityTypes` and `bigworldLogOns`.
//!
//! Accounts are not entity rows. `createSpecialBigWorldTables`
//! (`sync_db/mysql_synchronise.cpp:266-320`) creates them alongside the
//! `tbl_<Entity>` tables: `bigworldLogOnMapping` maps a log-on name (the
//! primary key) to a password and an entity key, `bigworldInfo` carries
//! the database version and the `isPasswordHashed` flag, and
//! `bigworldEntityTypes` assigns each entity type its database-internal
//! `typeID`. The log-on/checkout rows live in `bigworldLogOns`
//! (`entity_type_mapping.cpp:749-818`).
//!
//! The passwords follow one rule on both write and read
//! (`set_entity_key_for_account_task.cpp:39-57`,
//! `get_entity_key_for_account_task.cpp:40-60`): when `bigworldInfo`
//! says passwords are hashed, the stored value is
//! `MD5(CONCAT(password, logOnName))`, otherwise the raw bytes. The
//! get is the task's three-way outcome — no such user, invalid
//! password, or logged on with the entity key (a missing type or
//! entity id hands back no key, the reference's create-new branch,
//! `get_entity_key_for_account_task.cpp:74-90`).

use crate::storage::EntityKey;
use foundation::ids::{DatabaseId, EntityTypeId};
use md5::{Digest as _, Md5 as Md5Core};

/// `bigworldLogOnMapping` (`sync_db/mysql_synchronise.cpp:286-292`).
pub const LOG_ON_MAPPING_TABLE: &str = "bigworldLogOnMapping";
/// `bigworldInfo` (`sync_db/mysql_synchronise.cpp:270-274`).
pub const INFO_TABLE: &str = "bigworldInfo";
/// `bigworldEntityTypes` (`sync_db/mysql_synchronise.cpp:276-280`).
pub const ENTITY_TYPES_TABLE: &str = "bigworldEntityTypes";
/// `bigworldLogOns` (`sync_db/mysql_synchronise.cpp:282-285`).
pub const LOG_ONS_TABLE: &str = "bigworldLogOns";

/// `BWMySQLMaxLogOnNameLen` (`db_storage_mysql/constants.hpp:30`).
pub const MAX_LOG_ON_NAME_LEN: usize = 255;
/// `BWMySQLMaxLogOnPasswordLen` (`db_storage_mysql/constants.hpp:31`).
pub const MAX_LOG_ON_PASSWORD_LEN: usize = 255;

/// `CREATE TABLE bigworldInfo`
/// (`sync_db/mysql_synchronise.cpp:270-274`).
pub fn info_ddl() -> String {
    "CREATE TABLE IF NOT EXISTS bigworldInfo \
     (version INT UNSIGNED NOT NULL, \
     snapshotTime TIMESTAMP NULL, \
     isPasswordHashed BOOL NOT NULL DEFAULT FALSE) \
     ENGINE=InnoDB"
        .to_string()
}

/// `CREATE TABLE bigworldEntityTypes`
/// (`sync_db/mysql_synchronise.cpp:276-280`).
pub fn entity_types_ddl() -> String {
    "CREATE TABLE IF NOT EXISTS bigworldEntityTypes \
     (typeID INT NOT NULL AUTO_INCREMENT, bigworldID INT, \
     name CHAR(255) CHARACTER SET latin1 NOT NULL UNIQUE, \
     PRIMARY KEY(typeID), \
     KEY(bigworldID)) \
     ENGINE=InnoDB"
        .to_string()
}

/// `CREATE TABLE bigworldLogOns`
/// (`sync_db/mysql_synchronise.cpp:282-285`).
pub fn log_ons_ddl() -> String {
    "CREATE TABLE IF NOT EXISTS bigworldLogOns \
     (databaseID BIGINT NOT NULL, typeID INT NOT NULL, \
     objectID INT, ip INT UNSIGNED, port SMALLINT UNSIGNED, \
     salt SMALLINT UNSIGNED, \
     shouldAutoLoad BOOL NOT NULL DEFAULT FALSE, \
     PRIMARY KEY(typeID, databaseID)) \
     ENGINE=InnoDB"
        .to_string()
}

/// `CREATE TABLE bigworldLogOnMapping`
/// (`sync_db/mysql_synchronise.cpp:286-292`).
pub fn log_on_mapping_ddl() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS bigworldLogOnMapping \
         (logOnName VARBINARY({MAX_LOG_ON_NAME_LEN}) NOT NULL, \
         password VARBINARY({MAX_LOG_ON_PASSWORD_LEN}) NOT NULL, \
         entityType INT, entityID BIGINT, \
         PRIMARY KEY(logOnName)) \
         ENGINE=InnoDB"
    )
}

/// `MD5(CONCAT(password, logOnName))` as MySQL's `MD5()` renders it —
/// lowercase hex (`get_entity_key_for_account_task.cpp:44-46`).
pub fn hash_password(password: &[u8], log_on_name: &[u8]) -> String {
    let mut digest = Md5Core::new();
    digest.update(password);
    digest.update(log_on_name);
    format!("{:x}", digest.finalize())
}

/// The outcome of the log-on mapping lookup
/// (`LogOnStatus`, `get_entity_key_for_account_task.cpp:53-66,95-101`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountLogOnStatus {
    /// `LogOnStatus::LOGGED_ON`.
    LoggedOn,
    /// `LogOnStatus::LOGIN_REJECTED_NO_SUCH_USER`.
    RejectedNoSuchUser,
    /// `LogOnStatus::LOGIN_REJECTED_INVALID_PASSWORD`.
    RejectedInvalidPassword,
    /// `LogOnStatus::LOGIN_REJECTED_DB_GENERAL_FAILURE` — the task's
    /// `onException` outcome.
    RejectedDbGeneralFailure,
}

impl core::fmt::Display for AccountLogOnStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self {
            AccountLogOnStatus::LoggedOn => "LOGGED_ON",
            AccountLogOnStatus::RejectedNoSuchUser => "LOGIN_REJECTED_NO_SUCH_USER",
            AccountLogOnStatus::RejectedInvalidPassword => "LOGIN_REJECTED_INVALID_PASSWORD",
            AccountLogOnStatus::RejectedDbGeneralFailure => "LOGIN_REJECTED_DB_GENERAL_FAILURE",
        };
        f.write_str(name)
    }
}

/// One `bigworldLogOnMapping` row
/// (`set_entity_key_for_account_task.cpp:41-49`): the log-on name is the
/// primary key, the password follows the [`INFO_TABLE`] flag, and the
/// entity type is stored as the database-internal `typeID` (the
/// `bigworldEntityTypes` subselect, `set_entity_key_for_account_task.cpp:46-48`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    /// `logOnName`, the `VARBINARY(255)` primary key.
    pub log_on_name: Vec<u8>,
    /// `password`, plain bytes or [`hash_password`] output.
    pub password: Vec<u8>,
    /// `entityType`, the internal `typeID`; `None` when the bigworld id
    /// has no `bigworldEntityTypes` row (`entityType INT` is nullable).
    pub entity_type: Option<EntityTypeId>,
    /// `entityID`; `None` when unset (`entityID BIGINT` is nullable).
    pub entity_id: Option<DatabaseId>,
}

/// The account tables' in-memory backend: `bigworldInfo`'s flag,
/// `bigworldEntityTypes`' id assignment, and the `bigworldLogOnMapping`
/// rows. Real backends reimplement the same operations over their own
/// tables.
#[derive(Default)]
pub struct AccountStore {
    /// The `bigworldInfo.isPasswordHashed` flag
    /// (`FALSE` until updated, `mysql_synchronise.cpp:272`).
    is_password_hashed: bool,
    /// `bigworldEntityTypes`: bigworldID → typeID; the typeID is the
    /// table's `AUTO_INCREMENT`.
    types: std::collections::HashMap<i32, EntityTypeId>,
    /// The `AUTO_INCREMENT` counter of `bigworldEntityTypes`.
    next_type_id: EntityTypeId,
    /// The `bigworldLogOnMapping` rows, keyed by `logOnName`
    /// (`PRIMARY KEY(logOnName)`).
    rows: std::collections::HashMap<Vec<u8>, AccountRow>,
}

impl AccountStore {
    /// A freshly created database: `isPasswordHashed` defaults to
    /// `FALSE` (`mysql_synchronise.cpp:272`), and the
    /// `bigworldEntityTypes` `AUTO_INCREMENT` starts at 1.
    pub fn new() -> Self {
        Self {
            next_type_id: 1,
            ..Default::default()
        }
    }

    /// `UPDATE bigworldInfo SET isPasswordHashed = ?` — the sync tool's
    /// password-hash switch (`mysql_synchronise.cpp`, `updatePasswordHash`).
    pub fn set_password_hashed(&mut self, is_password_hashed: bool) {
        self.is_password_hashed = is_password_hashed;
    }

    /// The `bigworldInfo.isPasswordHashed` flag.
    pub fn is_password_hashed(&self) -> bool {
        self.is_password_hashed
    }

    /// Insert a `bigworldEntityTypes` row for `bigworld_id` and return
    /// its assigned `typeID` (`entity_types_ddl`: `typeID INT NOT NULL
    /// AUTO_INCREMENT`).
    pub fn add_entity_type(&mut self, bigworld_id: i32) -> EntityTypeId {
        let type_id = self.next_type_id;
        self.next_type_id += 1;
        self.types.insert(bigworld_id, type_id);
        type_id
    }

    /// The internal `typeID` of a bigworld id — the
    /// `bigworldEntityTypes` subselect
    /// (`set_entity_key_for_account_task.cpp:46-48`).
    pub fn type_id_for(&self, bigworld_id: i32) -> Option<EntityTypeId> {
        self.types.get(&bigworld_id).copied()
    }

    /// `REPLACE INTO bigworldLogOnMapping` — the write
    /// (`set_entity_key_for_account_task.cpp:39-57`): the password is
    /// stored per the [`INFO_TABLE`] flag, and the entity type is the
    /// internal `typeID` (`None` when the bigworld id is unmapped — the
    /// subselect's `NULL`). An existing row under the same log-on name
    /// is replaced, the `PRIMARY KEY(logOnName)` semantics.
    pub fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &[u8],
        bigworld_id: i32,
        entity_id: DatabaseId,
    ) {
        let password = if self.is_password_hashed {
            hash_password(password, username.as_bytes()).into_bytes()
        } else {
            password.to_vec()
        };
        self.rows.insert(
            username.as_bytes().to_vec(),
            AccountRow {
                log_on_name: username.as_bytes().to_vec(),
                password,
                entity_type: self.type_id_for(bigworld_id),
                entity_id: Some(entity_id),
            },
        );
    }

    /// The log-on mapping lookup (`get_entity_key_for_account_task.cpp:40-60`
    /// for the row outcome, `:66-101` for the handler branches): no row is
    /// [`AccountLogOnStatus::RejectedNoSuchUser`], a password mismatch is
    /// [`AccountLogOnStatus::RejectedInvalidPassword`] (both under the
    /// hashed and the plain rule), and a match is
    /// [`AccountLogOnStatus::LoggedOn`] with the entity key — `None` when
    /// the type or the entity id is missing, the reference's
    /// create-new branch.
    pub fn get_entity_key_for_account(
        &self,
        log_on_name: &str,
        password: &[u8],
    ) -> (AccountLogOnStatus, Option<EntityKey>) {
        let Some(row) = self.rows.get(log_on_name.as_bytes()) else {
            return (AccountLogOnStatus::RejectedNoSuchUser, None);
        };

        let password_ok = if self.is_password_hashed {
            row.password == hash_password(password, log_on_name.as_bytes()).into_bytes()
        } else {
            row.password == password
        };
        if !password_ok {
            return (AccountLogOnStatus::RejectedInvalidPassword, None);
        }

        let key = match (row.entity_type, row.entity_id) {
            (Some(type_id), Some(db_id)) => Some(EntityKey { type_id, db_id }),
            _ => None,
        };
        (AccountLogOnStatus::LoggedOn, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DDL of the account tables matches the reference's
    /// `createSpecialBigWorldTables` statements
    /// (`sync_db/mysql_synchronise.cpp:270-292`).
    #[test]
    fn the_account_tables_match_the_reference_ddl() {
        assert_eq!(
            info_ddl(),
            "CREATE TABLE IF NOT EXISTS bigworldInfo \
             (version INT UNSIGNED NOT NULL, snapshotTime TIMESTAMP NULL, \
             isPasswordHashed BOOL NOT NULL DEFAULT FALSE) \
             ENGINE=InnoDB"
        );
        assert_eq!(
            entity_types_ddl(),
            "CREATE TABLE IF NOT EXISTS bigworldEntityTypes \
             (typeID INT NOT NULL AUTO_INCREMENT, bigworldID INT, \
             name CHAR(255) CHARACTER SET latin1 NOT NULL UNIQUE, \
             PRIMARY KEY(typeID), KEY(bigworldID)) \
             ENGINE=InnoDB"
        );
        assert_eq!(
            log_on_mapping_ddl(),
            "CREATE TABLE IF NOT EXISTS bigworldLogOnMapping \
             (logOnName VARBINARY(255) NOT NULL, \
             password VARBINARY(255) NOT NULL, \
             entityType INT, entityID BIGINT, \
             PRIMARY KEY(logOnName)) \
             ENGINE=InnoDB"
        );
        assert_eq!(
            log_ons_ddl(),
            "CREATE TABLE IF NOT EXISTS bigworldLogOns \
             (databaseID BIGINT NOT NULL, typeID INT NOT NULL, \
             objectID INT, ip INT UNSIGNED, port SMALLINT UNSIGNED, \
             salt SMALLINT UNSIGNED, \
             shouldAutoLoad BOOL NOT NULL DEFAULT FALSE, \
             PRIMARY KEY(typeID, databaseID)) \
             ENGINE=InnoDB"
        );
        assert_eq!(MAX_LOG_ON_NAME_LEN, 255);
        assert_eq!(MAX_LOG_ON_PASSWORD_LEN, 255);
    }

    /// `MD5(CONCAT(password, logOnName))` in MySQL's lowercase hex —
    /// pinned against published MD5 vectors (`get_entity_key_for_account_task.cpp:44-46`).
    #[test]
    fn the_password_hash_is_the_reference_concatenation() {
        assert_eq!(
            hash_password(b"abc", b""),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hash_password(b"message digest", b""),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        // The log-on name is the second operand of the CONCAT.
        assert_ne!(hash_password(b"abc", b""), hash_password(b"abc", b"x"));
    }

    /// The reference status names round-trip
    /// (`LogOnStatus` enum, `get_entity_key_for_account_task.cpp:53-66`).
    #[test]
    fn the_statuses_carry_the_reference_names() {
        assert_eq!(AccountLogOnStatus::LoggedOn.to_string(), "LOGGED_ON");
        assert_eq!(
            AccountLogOnStatus::RejectedNoSuchUser.to_string(),
            "LOGIN_REJECTED_NO_SUCH_USER"
        );
        assert_eq!(
            AccountLogOnStatus::RejectedInvalidPassword.to_string(),
            "LOGIN_REJECTED_INVALID_PASSWORD"
        );
        assert_eq!(
            AccountLogOnStatus::RejectedDbGeneralFailure.to_string(),
            "LOGIN_REJECTED_DB_GENERAL_FAILURE"
        );
    }

    /// The acceptance, 原版数据库行可读写: a plain-password account round
    /// —trips through the same row the reference writes, with the entity
    /// type stored as the internal `typeID`, and the wrong password, an
    /// unknown user, and a replace behave as the reference's tasks do.
    #[test]
    fn a_plain_password_account_round_trips_like_the_reference_rows() {
        let mut store = AccountStore::new();
        assert!(
            !store.is_password_hashed(),
            "bigworldInfo defaults to FALSE"
        );
        let type_id = store.add_entity_type(42);
        assert_eq!(type_id, 1, "typeID is the AUTO_INCREMENT");
        assert_eq!(store.type_id_for(42), Some(1));
        assert_eq!(store.type_id_for(7), None);

        store.set_entity_key_for_account("wenchuan", b"secret", 42, 5);

        // The row: plain password bytes, the internal typeID (1), the
        // entity id.
        let row = store
            .rows
            .get(b"wenchuan".to_vec().as_slice())
            .expect("the mapping row");
        assert_eq!(row.password, b"secret".to_vec());
        assert_eq!(row.entity_type, Some(1));
        assert_eq!(row.entity_id, Some(5));

        // The right password logs on with the entity key.
        assert_eq!(
            store.get_entity_key_for_account("wenchuan", b"secret"),
            (
                AccountLogOnStatus::LoggedOn,
                Some(EntityKey {
                    type_id: 1,
                    db_id: 5
                })
            )
        );
        // The wrong password is the invalid-password rejection.
        assert_eq!(
            store.get_entity_key_for_account("wenchuan", b"guess"),
            (AccountLogOnStatus::RejectedInvalidPassword, None)
        );
        // An unknown user is the no-such-user rejection.
        assert_eq!(
            store.get_entity_key_for_account("nobody", b"secret"),
            (AccountLogOnStatus::RejectedNoSuchUser, None)
        );

        // REPLACE INTO: the same log-on name rewrites the row, it does
        // not duplicate (PRIMARY KEY(logOnName)).
        store.set_entity_key_for_account("wenchuan", b"new", 42, 9);
        assert_eq!(store.rows.len(), 1);
        assert_eq!(
            store.get_entity_key_for_account("wenchuan", b"new"),
            (
                AccountLogOnStatus::LoggedOn,
                Some(EntityKey {
                    type_id: 1,
                    db_id: 9
                })
            )
        );

        // An unmapped bigworld id stores no type — the reference's NULL
        // entityType and its create-new branch.
        store.set_entity_key_for_account("ghost", b"secret", 77, 3);
        assert_eq!(
            store.get_entity_key_for_account("ghost", b"secret"),
            (AccountLogOnStatus::LoggedOn, None)
        );
    }

    /// The hashed mode stores `MD5(CONCAT(password, logOnName))` and the
    /// lookup compares the same way
    /// (`set_entity_key_for_account_task.cpp:43-45`,
    /// `get_entity_key_for_account_task.cpp:44-46`).
    #[test]
    fn a_hashed_password_account_follows_the_md5_rule() {
        let mut store = AccountStore::new();
        store.set_password_hashed(true);
        assert!(store.is_password_hashed());

        store.add_entity_type(1);
        store.set_entity_key_for_account("wenchuan", b"secret", 1, 5);
        let row = store
            .rows
            .get(b"wenchuan".as_slice())
            .expect("the mapping row");
        assert_eq!(
            row.password,
            hash_password(b"secret", b"wenchuan").into_bytes()
        );

        assert_eq!(
            store.get_entity_key_for_account("wenchuan", b"secret").0,
            AccountLogOnStatus::LoggedOn
        );
        // The plain password does not match the stored hash.
        assert_eq!(
            store.get_entity_key_for_account("wenchuan", b"secretX").0,
            AccountLogOnStatus::RejectedInvalidPassword
        );
    }

    /// A fresh store behaves like an empty database: no rows, no types.
    #[test]
    fn a_fresh_store_is_an_empty_database() {
        let store = AccountStore::new();
        assert_eq!(
            store.get_entity_key_for_account("wenchuan", b"secret"),
            (AccountLogOnStatus::RejectedNoSuchUser, None)
        );
    }
}
