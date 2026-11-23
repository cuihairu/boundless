//! BW-06-02-01 the billing account authentication.
//!
//! The reference DBApp turns a login's credentials into the account's entity
//! key through a `BillingSystem` (`lib/db_storage/billing_system.hpp`). The
//! `LoginHandler::login` calls `getEntityKeyForAccount`; one of the four
//! `IGetEntityKeyForAccountHandler` callbacks is invoked with the outcome
//! (`server/dbapp/login_handler.cpp`). The default storage-backed
//! implementations are the MySQL task
//! (`lib/db_storage_mysql/tasks/get_entity_key_for_account_task.cpp`) and the
//! XML logon map (`lib/db_storage_xml/xml_billing_system.cpp`); both share the
//! same decision tree:
//!
//! * `authenticateViaBaseEntity` — the credentials are delegated to the base
//!   entity (the password is passed as `dataForBaseEntity`), so the account is
//!   loaded from the username and created if missing;
//! * a logon mapping found with a matching password — the account's entity
//!   key;
//! * a mapping with a wrong password — `LOGIN_REJECTED_INVALID_PASSWORD`;
//! * no mapping — `LOGIN_REJECTED_NO_SUCH_USER`, unless unknown users are
//!   accepted, in which case a new account entity is created.
//!
//! The configuration is `billingSystem/*` in `bw.xml`; the defaults are
//! `shouldAcceptUnknownUsers`/`shouldRememberUnknownUsers`/
//! `authenticateViaBaseEntity` false and `entityTypeForUnknownUsers` empty.
//!
//! The DBApp's storage backend is not wired into this port yet, so the
//! `LogonMapBillingSystem` mirrors the XML implementation: an in-memory
//! logon-name -> (entity key, password) map. The decision logic is exposed
//! separately (`resolve_account`) so a storage-backed backend can reuse it.

use std::collections::HashMap;

use crate::loginapp::LogOnStatus;

/// `EntityTypeID` (`lib/entitydef/entity_description.hpp`).
pub type EntityTypeID = u16;
/// `INVALID_ENTITY_TYPE_ID`.
pub const INVALID_ENTITY_TYPE_ID: EntityTypeID = EntityTypeID::MAX;
/// `DatabaseID` (`lib/db_storage/entity_key.hpp`).
pub type DatabaseID = u32;

/// `EntityKey` (`lib/db_storage/entity_key.hpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntityKey {
    /// `typeID`.
    pub type_id: EntityTypeID,
    /// `dbID`.
    pub db_id: DatabaseID,
}

impl EntityKey {
    /// Constructor.
    pub fn new(type_id: EntityTypeID, db_id: DatabaseID) -> Self {
        Self { type_id, db_id }
    }
}

/// The `billingSystem/*` configuration (`lib/db_storage/billing_system.cpp`).
#[derive(Clone, Debug)]
pub struct BillingConfig {
    /// `billingSystem/shouldAcceptUnknownUsers`.
    pub should_accept_unknown_users: bool,
    /// `billingSystem/shouldRememberUnknownUsers`.
    pub should_remember_unknown_users: bool,
    /// `billingSystem/authenticateViaBaseEntity`.
    pub authenticate_via_base_entity: bool,
    /// `billingSystem/entityTypeForUnknownUsers`, resolved to an entity type
    /// id by the constructor.
    pub entity_type_for_unknown_users: EntityTypeID,
}

impl BillingConfig {
    /// Defaults from `billing_system.cpp`: all flags false, no unknown-user
    /// entity type.
    pub fn new() -> Self {
        Self {
            should_accept_unknown_users: false,
            should_remember_unknown_users: false,
            authenticate_via_base_entity: false,
            entity_type_for_unknown_users: INVALID_ENTITY_TYPE_ID,
        }
    }

    /// Resolve `entityTypeForUnknownUsers` with the entity-defs name lookup
    /// (the reference `entityDefs.getEntityType`).
    pub fn with_unknown_entity_type(
        mut self,
        entity_type_name: &str,
        resolve: impl Fn(&str) -> EntityTypeID,
    ) -> Self {
        self.entity_type_for_unknown_users = resolve(entity_type_name);
        self
    }

    /// `isOkay()`: either an unknown-user entity type is configured, or unknown
    /// users are neither accepted nor authenticated via the base entity.
    pub fn is_okay(&self) -> bool {
        self.entity_type_for_unknown_users != INVALID_ENTITY_TYPE_ID
            || (!self.should_accept_unknown_users && !self.authenticate_via_base_entity)
    }
}

impl Default for BillingConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The result of looking up a logon name, as a storage backend reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountLookup {
    /// No mapping for the logon name.
    NoSuchUser,
    /// A mapping with a matching password. The entity key is `Some` when the
    /// account has a database id, `None` when it must be created.
    Found {
        /// The account entity key, if it already has a database id.
        entity_key: Option<EntityKey>,
    },
    /// A mapping whose password does not match.
    InvalidPassword,
}

/// The account-resolution outcome, as `LoginHandler` receives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountResolution {
    /// `onGetEntityKeyForAccountSuccess`.
    Success {
        /// The account entity key.
        entity_key: EntityKey,
        /// `dataForClient`.
        data_for_client: String,
        /// `dataForBaseEntity`.
        data_for_base_entity: String,
    },
    /// `onGetEntityKeyForAccountLoadFromUsername`: authenticate via the base
    /// entity, loading by username and creating on failure.
    LoadFromUsername {
        /// The entity type to load.
        entity_type: EntityTypeID,
        /// The username to load.
        username: String,
        /// `shouldCreateNewOnLoadFailure`.
        should_create_new_on_load_failure: bool,
        /// `shouldRememberNewOnLoadFailure`.
        should_remember_new_on_load_failure: bool,
        /// `dataForClient`.
        data_for_client: String,
        /// `dataForBaseEntity`.
        data_for_base_entity: String,
    },
    /// `onGetEntityKeyForAccountCreateNew`.
    CreateNew {
        /// The entity type to create.
        entity_type: EntityTypeID,
        /// `shouldRemember`.
        should_remember: bool,
        /// `dataForClient`.
        data_for_client: String,
        /// `dataForBaseEntity`.
        data_for_base_entity: String,
    },
    /// `onGetEntityKeyForAccountFailure`.
    Failure {
        /// The `LogOnStatus`.
        status: LogOnStatus,
        /// `errorMsg`.
        error_msg: String,
    },
}

/// `BillingSystem::getEntityKeyForAccount` outcome dispatch: turn an account
/// lookup and the configuration into the callback the reference would invoke.
///
/// This is the decision tree shared by the MySQL task and the XML logon map:
/// the MySQL task's `performMainThreadTask` differs only in that a logged-on
/// account with no database id creates a *remembered* entity (`true`) rather
/// than `shouldRememberUnknownUsers` — the MySQL mapping's `databaseID` is
/// never null on the logged-on path, so the two coincide in practice.
pub fn resolve_account(lookup: AccountLookup, config: &BillingConfig) -> AccountResolution {
    if config.authenticate_via_base_entity {
        return AccountResolution::LoadFromUsername {
            entity_type: config.entity_type_for_unknown_users,
            username: String::new(), // the caller fills the logon name
            should_create_new_on_load_failure: true,
            should_remember_new_on_load_failure: false,
            data_for_client: String::new(),
            data_for_base_entity: String::new(), // the caller fills the password
        };
    }
    match lookup {
        AccountLookup::Found {
            entity_key: Some(entity_key),
        } => AccountResolution::Success {
            entity_key,
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        },
        AccountLookup::Found { entity_key: None } => AccountResolution::CreateNew {
            entity_type: config.entity_type_for_unknown_users,
            should_remember: true,
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        },
        AccountLookup::InvalidPassword => AccountResolution::Failure {
            status: LogOnStatus::LoginRejectedInvalidPassword,
            error_msg: String::new(),
        },
        AccountLookup::NoSuchUser => {
            if config.should_accept_unknown_users {
                AccountResolution::CreateNew {
                    entity_type: config.entity_type_for_unknown_users,
                    should_remember: config.should_remember_unknown_users,
                    data_for_client: String::new(),
                    data_for_base_entity: String::new(),
                }
            } else {
                AccountResolution::Failure {
                    status: LogOnStatus::LoginRejectedNoSuchUser,
                    error_msg: String::new(),
                }
            }
        }
    }
}

/// `LoginHandler::onGetEntityKeyForAccountFailure` client message
/// (`server/dbapp/login_handler.cpp`).
pub fn failure_message(status: LogOnStatus, error_msg: &str) -> String {
    match status {
        LogOnStatus::LoginRejectedNoSuchUser => "Unknown user.".to_string(),
        LogOnStatus::LoginRejectedInvalidPassword => "Invalid password.".to_string(),
        LogOnStatus::LoginRejectedBan => {
            if error_msg.is_empty() {
                "User is banned.".to_string()
            } else {
                error_msg.to_string()
            }
        }
        LogOnStatus::LoginRejectedDbGeneralFailure => "Unexpected database failure.".to_string(),
        _ => {
            if error_msg.is_empty() {
                "Unspecified error".to_string()
            } else {
                error_msg.to_string()
            }
        }
    }
}

/// One logon mapping: the account entity key and its password.
#[derive(Clone, Debug)]
struct LogOnMapping {
    entity_key: EntityKey,
    password: String,
}

/// The `XMLBillingSystem` port: an in-memory logon-name -> account map.
pub struct LogonMapBillingSystem {
    config: BillingConfig,
    logon_map: HashMap<String, LogOnMapping>,
}

impl LogonMapBillingSystem {
    /// Constructor with the given configuration.
    pub fn new(config: BillingConfig) -> Self {
        Self {
            config,
            logon_map: HashMap::new(),
        }
    }

    /// `initLogOnMapping`: register a logon name -> (entity key, password).
    pub fn add_mapping(&mut self, log_on_name: &str, entity_key: EntityKey, password: &str) {
        self.logon_map.insert(
            log_on_name.to_string(),
            LogOnMapping {
                entity_key,
                password: password.to_string(),
            },
        );
    }

    /// `config()`.
    pub fn config(&self) -> &BillingConfig {
        &self.config
    }

    /// `isOkay()`.
    pub fn is_okay(&self) -> bool {
        self.config.is_okay()
    }

    /// `getEntityKeyForAccount`: look up the credentials and resolve the
    /// outcome the reference billing system reports.
    pub fn get_entity_key_for_account(
        &self,
        log_on_name: &str,
        password: &str,
    ) -> AccountResolution {
        let lookup = match self.logon_map.get(log_on_name) {
            Some(mapping) => {
                if mapping.password == password {
                    AccountLookup::Found {
                        entity_key: Some(mapping.entity_key),
                    }
                } else {
                    AccountLookup::InvalidPassword
                }
            }
            None => AccountLookup::NoSuchUser,
        };
        let mut resolution = resolve_account(lookup, &self.config);
        // `LoadFromUsername` carries the logon name and password on the wire
        // (the reference `onGetEntityKeyForAccountLoadFromUsername`).
        if let AccountResolution::LoadFromUsername {
            username,
            data_for_base_entity,
            ..
        } = &mut resolution
        {
            *username = log_on_name.to_string();
            *data_for_base_entity = password.to_string();
        }
        resolution
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT_TYPE: EntityTypeID = 3;
    const ACCOUNT_DBID: DatabaseID = 42;

    fn config() -> BillingConfig {
        BillingConfig::new()
    }

    fn key() -> EntityKey {
        EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID)
    }

    #[test]
    fn config_defaults_are_all_disabled() {
        let c = BillingConfig::new();
        assert!(!c.should_accept_unknown_users);
        assert!(!c.should_remember_unknown_users);
        assert!(!c.authenticate_via_base_entity);
        assert_eq!(c.entity_type_for_unknown_users, INVALID_ENTITY_TYPE_ID);
        assert!(c.is_okay()); // nothing to accept, nothing to resolve
    }

    #[test]
    fn a_matching_logon_mapping_resolves_to_the_entity_key() {
        let mut billing = LogonMapBillingSystem::new(config());
        billing.add_mapping("alice", key(), "s3cret");
        assert_eq!(
            billing.get_entity_key_for_account("alice", "s3cret"),
            AccountResolution::Success {
                entity_key: key(),
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    #[test]
    fn a_wrong_password_is_rejected() {
        let mut billing = LogonMapBillingSystem::new(config());
        billing.add_mapping("alice", key(), "s3cret");
        assert_eq!(
            billing.get_entity_key_for_account("alice", "wrong"),
            AccountResolution::Failure {
                status: LogOnStatus::LoginRejectedInvalidPassword,
                error_msg: String::new(),
            }
        );
    }

    #[test]
    fn an_unknown_user_is_rejected_when_unknown_users_are_not_accepted() {
        let billing = LogonMapBillingSystem::new(config());
        assert_eq!(
            billing.get_entity_key_for_account("nobody", "x"),
            AccountResolution::Failure {
                status: LogOnStatus::LoginRejectedNoSuchUser,
                error_msg: String::new(),
            }
        );
    }

    #[test]
    fn an_unknown_user_creates_a_new_account_when_accepted() {
        let mut config = config();
        config.should_accept_unknown_users = true;
        config.should_remember_unknown_users = true;
        config.entity_type_for_unknown_users = ACCOUNT_TYPE;
        let billing = LogonMapBillingSystem::new(config);
        assert_eq!(
            billing.get_entity_key_for_account("nobody", "x"),
            AccountResolution::CreateNew {
                entity_type: ACCOUNT_TYPE,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    #[test]
    fn authenticate_via_base_entity_loads_by_username_with_the_password() {
        let mut config = config();
        config.authenticate_via_base_entity = true;
        config.entity_type_for_unknown_users = ACCOUNT_TYPE;
        let billing = LogonMapBillingSystem::new(config);
        assert_eq!(
            billing.get_entity_key_for_account("alice", "s3cret"),
            AccountResolution::LoadFromUsername {
                entity_type: ACCOUNT_TYPE,
                username: "alice".to_string(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
                data_for_client: String::new(),
                data_for_base_entity: "s3cret".to_string(),
            }
        );
    }

    #[test]
    fn the_config_is_not_okay_when_it_accepts_unknowns_without_an_entity_type() {
        let mut config = config();
        config.should_accept_unknown_users = true;
        assert!(!config.is_okay()); // accepting unknown users, no entity type
        config.entity_type_for_unknown_users = ACCOUNT_TYPE;
        assert!(config.is_okay());
    }

    #[test]
    fn failure_messages_match_the_reference() {
        assert_eq!(
            failure_message(LogOnStatus::LoginRejectedNoSuchUser, ""),
            "Unknown user."
        );
        assert_eq!(
            failure_message(LogOnStatus::LoginRejectedInvalidPassword, ""),
            "Invalid password."
        );
        assert_eq!(
            failure_message(LogOnStatus::LoginRejectedBan, ""),
            "User is banned."
        );
        assert_eq!(
            failure_message(LogOnStatus::LoginRejectedBan, "custom"),
            "custom"
        );
        assert_eq!(
            failure_message(LogOnStatus::LoginRejectedDbGeneralFailure, ""),
            "Unexpected database failure."
        );
        assert_eq!(
            failure_message(LogOnStatus::LoginRejectedAlreadyLoggedIn, ""),
            "Unspecified error"
        );
        assert_eq!(
            failure_message(LogOnStatus::LoginRejectedAlreadyLoggedIn, "e"),
            "e"
        );
    }

    #[test]
    fn resolve_account_creates_a_new_entity_for_a_logged_on_no_dbid() {
        // The MySQL task: a logged-on mapping with no database id creates a
        // remembered entity.
        let lookup = AccountLookup::Found { entity_key: None };
        assert_eq!(
            resolve_account(lookup, &config()),
            AccountResolution::CreateNew {
                entity_type: INVALID_ENTITY_TYPE_ID,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }
}
