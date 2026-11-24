//! The standard billing system: how a `username`/`password` pair becomes
//! an entity, and how a mapping gets written back.
//!
//! The reference's "standard" billing creator
//! (`standard_billing_system.cpp:63-86`) uses the personality's
//! `connectToBillingSystem` script when it has one and otherwise falls
//! through to the database type's own billing system — `MySqlBillingSystem`
//! (`mysql_billing_system.cpp`) over the `bigworldLogOnMapping` tables,
//! `XMLBillingSystem` (`xml_billing_system.cpp`) over the
//! `_BigWorldInfo/LogOnMapping` section. Both share the `BillingSystem`
//! base's configuration (`billing_system.cpp:23-48`) and the handler
//! contract (`billing_system.hpp:15-76`): one of four outcomes comes back.
//!
//! The reference reports through callbacks on a background task; the port
//! folds them into the [`GetEntityKeyOutcome`] enum. The MD5 digest is the
//! caller's business, exactly as [`crate::account::check_password`] leaves
//! it: `isPasswordHashed` only decides how `presented` was formed.
//!
//! The acceptance for BW-09-04-01 is the outcome table: a legal account
//! logs on (or creates a new entity), an invalid account is rejected with
//! the status the reference picks.

use std::collections::BTreeMap;

use foundation::ids::{DatabaseId, EntityTypeId, INVALID_ENTITY_TYPE_ID};

use crate::account::{AccountLogOn, AccountRow, check_password};
use crate::storage::EntityKey;

/// The `BillingSystem` constructor's configuration
/// (`billing_system.cpp:23-48`), read from `bw.xml`'s `billingSystem/`
/// section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingConfig {
    /// `billingSystem/shouldAcceptUnknownUsers`, default false.
    pub should_accept_unknown_users: bool,
    /// `billingSystem/shouldRememberUnknownUsers`, default false.
    pub should_remember_unknown_users: bool,
    /// `billingSystem/authenticateViaBaseEntity`, default false.
    pub authenticate_via_base_entity: bool,
    /// `entityTypeIDForUnknownUsers_`: the resolved
    /// `billingSystem/entityTypeForUnknownUsers` name. Unresolvable names
    /// leave [`INVALID_ENTITY_TYPE_ID`] (`billing_system.cpp:48-51`).
    pub entity_type_id_for_unknown_users: EntityTypeId,
}

impl BillingConfig {
    /// The constructor: `resolve` plays `EntityDefs::getEntityType` on the
    /// configured name (`billing_system.cpp:44-48`). The reference logs an
    /// error when the type is needed but missing; [`Self::is_okay`] is
    /// where that shows up.
    pub fn from_name(
        should_accept_unknown_users: bool,
        should_remember_unknown_users: bool,
        authenticate_via_base_entity: bool,
        entity_type_for_unknown_users: &str,
        resolve: impl FnOnce(&str) -> Option<EntityTypeId>,
    ) -> Self {
        Self {
            should_accept_unknown_users,
            should_remember_unknown_users,
            authenticate_via_base_entity,
            entity_type_id_for_unknown_users: resolve(entity_type_for_unknown_users)
                .unwrap_or(INVALID_ENTITY_TYPE_ID),
        }
    }

    /// The defaults: every flag off, no known unknown-user type.
    pub fn disabled() -> Self {
        Self {
            should_accept_unknown_users: false,
            should_remember_unknown_users: false,
            authenticate_via_base_entity: false,
            entity_type_id_for_unknown_users: INVALID_ENTITY_TYPE_ID,
        }
    }

    /// `BillingSystem::isOkay` (`billing_system.hpp:110-114`): the
    /// system can start with a usable unknown-user type, or when it
    /// needs no unknown-user face at all.
    pub fn is_okay(&self) -> bool {
        (self.entity_type_id_for_unknown_users != INVALID_ENTITY_TYPE_ID)
            || (!self.should_accept_unknown_users && !self.authenticate_via_base_entity)
    }
}

/// One folded `IGetEntityKeyForAccountHandler` outcome
/// (`billing_system.hpp:15-63`). The `dataForClient`/`dataForBaseEntity`
/// strings ride on every callback in the reference; the standard system
/// only ever fills the base-entity one, with the password, on the
/// base-entity authentication leg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetEntityKeyOutcome {
    /// `onGetEntityKeyForAccountSuccess`: the account maps to this entity.
    Success {
        /// The mapped entity.
        key: EntityKey,
        /// `dataForClient`.
        data_for_client: String,
        /// `dataForBaseEntity`.
        data_for_base_entity: String,
    },
    /// `onGetEntityKeyForAccountLoadFromUsername`: load (or create) by
    /// name instead of by mapping — the base-entity authentication leg.
    LoadFromUsername {
        /// The type to load.
        type_id: EntityTypeId,
        /// The name to load.
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
    /// `onGetEntityKeyForAccountCreateNew`: the account has no entity
    /// yet; make one.
    CreateNew {
        /// The type to create.
        type_id: EntityTypeId,
        /// `shouldRemember`: whether the new entity is mapped to the
        /// account.
        should_remember: bool,
        /// `dataForClient`.
        data_for_client: String,
        /// `dataForBaseEntity`.
        data_for_base_entity: String,
    },
    /// `onGetEntityKeyForAccountFailure`: rejected.
    Failure {
        /// The `LogOnStatus` the reference reports.
        status: AccountLogOn,
        /// `errorMsg` — always empty for the standard systems.
        error_msg: String,
    },
}

/// One folded `ISetEntityKeyForAccountHandler` outcome
/// (`billing_system.hpp:66-80`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetEntityKeyOutcome {
    /// `onSetEntityKeyForAccountSuccess`.
    Success(EntityKey),
    /// `onSetEntityKeyForAccountFailure` — the SQL exception path.
    Failure(EntityKey),
}

/// One looked-up account row: the `bigworldLogOnMapping` row joined to
/// its BigWorld type id and entity id
/// (`get_entity_key_for_account_task.cpp:38-48`). `None` legs mirror SQL
/// NULL: the `LEFT JOIN` misses leave [`Self::bigworld_type_id`] unset,
/// and a NULL `entityID` leaves [`Self::entity_id`] unset — only SQL
/// NULL unsets them, a stored 0 arrives as `Some(0)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLookup {
    /// The `bigworldLogOnMapping` row.
    pub row: AccountRow,
    /// `t.bigworldID`, `NULL` when the type has no BigWorld mapping.
    pub bigworld_type_id: Option<EntityTypeId>,
    /// `m.entityID`.
    pub entity_id: Option<DatabaseId>,
}

/// `GetEntityKeyForAccountTask::performBackgroundTask`
/// (`get_entity_key_for_account_task.cpp:50-77`): no row is
/// `NO_SUCH_USER`, a row goes through the byte-exact password compare.
/// The reference maps the query's exception to
/// `LOGIN_REJECTED_DB_GENERAL_FAILURE` (`:80-86`) — that stays the
/// caller's, which keeps this function total.
pub fn resolve_status(
    lookup: Option<&AccountLookup>,
    is_password_hashed: bool,
    presented: &str,
) -> AccountLogOn {
    let Some(lookup) = lookup else {
        return AccountLogOn::RejectedNoSuchUser;
    };
    check_password(&lookup.row, is_password_hashed, presented)
}

/// `GetEntityKeyForAccountTask::performMainThreadTask`
/// (`get_entity_key_for_account_task.cpp:88-116`): the status picks the
/// handler callback. The unknown-user type id backs both a NULL join and
/// the create path; a mapped entity id means success, a missing one
/// means a **remembered** create.
pub fn dispatch_get(
    status: AccountLogOn,
    lookup: Option<&AccountLookup>,
    config: &BillingConfig,
) -> GetEntityKeyOutcome {
    // `pEntityTypeID == NULL` falls back to `defaultEntityTypeID_`
    // (`:91-97`).
    let type_id = lookup
        .and_then(|lookup| lookup.bigworld_type_id)
        .unwrap_or(config.entity_type_id_for_unknown_users);
    match status {
        AccountLogOn::LoggedOn => {
            // `pDBID` set → success; unset → create and remember.
            match lookup.and_then(|lookup| lookup.entity_id) {
                Some(db_id) => GetEntityKeyOutcome::Success {
                    key: EntityKey { type_id, db_id },
                    data_for_client: String::new(),
                    data_for_base_entity: String::new(),
                },
                None => GetEntityKeyOutcome::CreateNew {
                    type_id,
                    should_remember: true,
                    data_for_client: String::new(),
                    data_for_base_entity: String::new(),
                },
            }
        }
        AccountLogOn::RejectedNoSuchUser if config.should_accept_unknown_users => {
            GetEntityKeyOutcome::CreateNew {
                type_id,
                should_remember: config.should_remember_unknown_users,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        }
        other => GetEntityKeyOutcome::Failure {
            status: other,
            error_msg: String::new(),
        },
    }
}

/// The standard front: `authenticateViaBaseEntity` takes the load-by-name
/// leg with the password handed to the base entity
/// (`mysql_billing_system.cpp:37-45`, `xml_billing_system.cpp:60-73`);
/// everything else resolves the row and dispatches its status.
pub fn get_entity_key_for_account(
    config: &BillingConfig,
    lookup: Option<&AccountLookup>,
    log_on_name: &str,
    is_password_hashed: bool,
    presented: &str,
) -> GetEntityKeyOutcome {
    if config.authenticate_via_base_entity {
        return GetEntityKeyOutcome::LoadFromUsername {
            type_id: config.entity_type_id_for_unknown_users,
            username: log_on_name.to_owned(),
            should_create_new_on_load_failure: true,
            should_remember_new_on_load_failure: false,
            data_for_client: String::new(),
            data_for_base_entity: presented.to_owned(),
        };
    }
    let status = resolve_status(lookup, is_password_hashed, presented);
    dispatch_get(status, lookup, config)
}

/// `SetEntityKeyForAccountTask`'s outcome: the reference's background
/// REPLACE either lands (`performMainThreadTask` always reports success,
/// `set_entity_key_for_account_task.cpp:63-68`) or the connection
/// failed (`onException`, `:70-77`).
pub fn dispatch_set(key: EntityKey, succeeded: bool) -> SetEntityKeyOutcome {
    if succeeded {
        SetEntityKeyOutcome::Success(key)
    } else {
        SetEntityKeyOutcome::Failure(key)
    }
}

/// `LogOnMapping` (`xml_billing_system.hpp:70-90`): the entity a log-on
/// name maps to and its byte-compared password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlLogOnMapping {
    /// The mapped entity.
    pub entity_key: EntityKey,
    /// The stored password, compared byte-exact.
    pub password: String,
}

/// One entry of the `LogOnMapping` section as it reads off disk
/// (`xml_billing_system.cpp:31-58`): `logOnName`, `type`, `entityID`,
/// `entityName`, `password`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlLogOnEntry {
    /// `logOnName`.
    pub log_on_name: String,
    /// `type`: the entity type's name.
    pub type_name: String,
    /// `entityID` — `0` means "resolve [`Self::entity_name`] instead".
    pub entity_id: DatabaseId,
    /// `entityName`.
    pub entity_name: String,
    /// `password`.
    pub password: String,
}

/// The written tags of one `LogOnMapping/item` section
/// (`xml_billing_system.cpp:128-153`) — what a set hands the caller to
/// persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlLogOnEntryWrite {
    /// `logOnName`.
    pub log_on_name: String,
    /// `password`.
    pub password: String,
    /// `type`: the entity type's name.
    pub type_name: String,
    /// `entityID`.
    pub entity_id: DatabaseId,
}

/// The XML billing system (`xml_billing_system.cpp`): the
/// `_BigWorldInfo/LogOnMapping` section as a live map. Section writes
/// stay the caller's — this face hands back the entry to write, the way
/// the database crate keeps the document face in `xml.rs`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct XmlBillingSystem {
    /// `logonMap_`, keyed by `logOnName`.
    logon_map: BTreeMap<String, XmlLogOnMapping>,
}

impl XmlBillingSystem {
    /// `initLogOnMapping` (`xml_billing_system.cpp:31-58`): resolve each
    /// entry's type name through `resolve`; an invalid type is ignored
    /// with the reference's warning. An `entityID` of 0 falls to the
    /// name lookup.
    pub fn from_entries(
        entries: impl IntoIterator<Item = XmlLogOnEntry>,
        mut resolve: impl FnMut(&str) -> Option<EntityTypeId>,
        mut find_by_name: impl FnMut(EntityTypeId, &str) -> Option<DatabaseId>,
    ) -> Self {
        let mut logon_map = BTreeMap::new();
        for entry in entries {
            let Some(type_id) = resolve(&entry.type_name) else {
                // WARNING_MSG "Logon mapping ignored because '%s' is not
                // a valid entity type".
                continue;
            };
            let mut entity_id = entry.entity_id;
            if entity_id == 0 {
                entity_id = find_by_name(type_id, &entry.entity_name).unwrap_or(0);
            }
            logon_map.insert(
                entry.log_on_name,
                XmlLogOnMapping {
                    entity_key: EntityKey {
                        type_id,
                        db_id: entity_id,
                    },
                    password: entry.password,
                },
            );
        }
        Self { logon_map }
    }

    /// `XMLBillingSystem::getEntityKeyForAccount`
    /// (`xml_billing_system.cpp:75-99`).
    pub fn get_entity_key_for_account(
        &self,
        config: &BillingConfig,
        log_on_name: &str,
        password: &str,
    ) -> GetEntityKeyOutcome {
        if config.authenticate_via_base_entity {
            return GetEntityKeyOutcome::LoadFromUsername {
                type_id: config.entity_type_id_for_unknown_users,
                username: log_on_name.to_owned(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
                data_for_client: String::new(),
                data_for_base_entity: password.to_owned(),
            };
        }
        match self.logon_map.get(log_on_name) {
            Some(mapping) if mapping.password == password => GetEntityKeyOutcome::Success {
                key: mapping.entity_key,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            },
            Some(_) => GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedInvalidPassword,
                error_msg: String::new(),
            },
            None if config.should_accept_unknown_users => GetEntityKeyOutcome::CreateNew {
                type_id: config.entity_type_id_for_unknown_users,
                should_remember: config.should_remember_unknown_users,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            },
            None => GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedNoSuchUser,
                error_msg: String::new(),
            },
        }
    }

    /// `XMLBillingSystem::setEntityKeyForAccount`
    /// (`xml_billing_system.cpp:102-157`): upsert the mapping and hand
    /// back the section entry to persist. The reference always reports
    /// success — a section write cannot fail.
    pub fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
        type_name_of: impl FnOnce(EntityTypeId) -> String,
    ) -> (SetEntityKeyOutcome, XmlLogOnEntryWrite) {
        self.logon_map.insert(
            username.to_owned(),
            XmlLogOnMapping {
                entity_key: key,
                password: password.to_owned(),
            },
        );
        let write = XmlLogOnEntryWrite {
            log_on_name: username.to_owned(),
            password: password.to_owned(),
            type_name: type_name_of(key.type_id),
            entity_id: key.db_id,
        };
        (SetEntityKeyOutcome::Success(key), write)
    }

    /// The mapped entity for `log_on_name`, if any.
    pub fn mapping(&self, log_on_name: &str) -> Option<&XmlLogOnMapping> {
        self.logon_map.get(log_on_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, password: &str, entity_id: i32) -> AccountRow {
        AccountRow {
            log_on_name: name.to_owned(),
            password: password.to_owned(),
            entity_type: 1,
            entity_id,
        }
    }

    fn lookup(name: &str, password: &str, entity_id: i32) -> AccountLookup {
        AccountLookup {
            row: row(name, password, entity_id),
            bigworld_type_id: Some(1),
            entity_id: Some(entity_id as DatabaseId),
        }
    }

    #[test]
    fn the_config_resolves_the_unknown_user_type_or_leaves_it_invalid() {
        let known = BillingConfig::from_name(true, true, false, "Avatar", |name| {
            (name == "Avatar").then_some(1)
        });
        assert_eq!(known.entity_type_id_for_unknown_users, 1);
        assert!(known.is_okay());

        let unknown = BillingConfig::from_name(true, true, false, "Nope", |_| None);
        assert_eq!(
            unknown.entity_type_id_for_unknown_users,
            INVALID_ENTITY_TYPE_ID
        );
        assert!(!unknown.is_okay(), "unknown users need a type");

        let plain = BillingConfig::disabled();
        assert!(plain.is_okay(), "no unknown face needs no type");
    }

    /// The outcome table: legal accounts log on, wrong passwords and
    /// unknown names are rejected with the reference's statuses.
    #[test]
    fn the_standard_arbitration_matches_the_reference_statuses() {
        let config = BillingConfig::disabled();
        let found = lookup("hero", "secret", 7);

        assert_eq!(
            get_entity_key_for_account(&config, Some(&found), "hero", false, "secret"),
            GetEntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: 1,
                    db_id: 7
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
        assert_eq!(
            get_entity_key_for_account(&config, Some(&found), "hero", false, "wrong"),
            GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedInvalidPassword,
                error_msg: String::new(),
            }
        );
        assert_eq!(
            get_entity_key_for_account(&config, None, "ghost", false, "anything"),
            GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedNoSuchUser,
                error_msg: String::new(),
            }
        );
    }

    /// The hashed-password leg: the flag only decides how `presented`
    /// was formed — the compare stays byte-exact both ways.
    #[test]
    fn hashed_passwords_compare_bytes_like_the_query() {
        let config = BillingConfig::disabled();
        let digest = "5f4dcc3b5aa765d61d8327deb882cf99";
        let hashed = AccountLookup {
            row: row("hero", digest, 7),
            bigworld_type_id: Some(1),
            entity_id: Some(7),
        };
        // MD5("password" + "hero") presented for a hashed store.
        assert_eq!(
            get_entity_key_for_account(&config, Some(&hashed), "hero", true, digest),
            GetEntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: 1,
                    db_id: 7
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
        // The plain password never matches a digest store.
        assert_eq!(
            get_entity_key_for_account(&config, Some(&hashed), "hero", true, "password"),
            GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedInvalidPassword,
                error_msg: String::new(),
            }
        );
    }

    /// A mapped account with no stored entity id creates — and the
    /// create is remembered, unlike an unknown user's.
    #[test]
    fn a_mapped_account_without_an_entity_creates_and_remembers() {
        let config = BillingConfig::from_name(true, false, false, "Avatar", |_| Some(1));
        let unmapped = AccountLookup {
            row: row("hero", "secret", 0),
            bigworld_type_id: Some(1),
            entity_id: None,
        };
        assert_eq!(
            get_entity_key_for_account(&config, Some(&unmapped), "hero", false, "secret"),
            GetEntityKeyOutcome::CreateNew {
                type_id: 1,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    /// Unknown users only create when the config accepts them, with the
    /// configured remember flag and the unknown-user type.
    #[test]
    fn unknown_users_create_only_when_accepted() {
        let accepting = BillingConfig::from_name(true, true, false, "Avatar", |_| Some(1));
        assert_eq!(
            get_entity_key_for_account(&accepting, None, "stranger", false, "x"),
            GetEntityKeyOutcome::CreateNew {
                type_id: 1,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
        let strict = BillingConfig::disabled();
        assert_eq!(
            get_entity_key_for_account(&strict, None, "stranger", false, "x"),
            GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedNoSuchUser,
                error_msg: String::new(),
            }
        );
    }

    /// A type whose BigWorld mapping is missing falls back to the
    /// unknown-user type id (`pEntityTypeID == NULL`).
    #[test]
    fn a_missing_type_mapping_falls_back_to_the_unknown_user_type() {
        let config = BillingConfig::from_name(false, false, false, "", |_| Some(9));
        let orphan = AccountLookup {
            row: row("hero", "secret", 7),
            bigworld_type_id: None,
            entity_id: Some(7),
        };
        assert_eq!(
            get_entity_key_for_account(&config, Some(&orphan), "hero", false, "secret"),
            GetEntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: 9,
                    db_id: 7
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    /// The base-entity leg bypasses the row entirely and hands the
    /// password to the entity as its data.
    #[test]
    fn base_entity_authentication_loads_by_username() {
        let config = BillingConfig::from_name(false, false, true, "Avatar", |_| Some(1));
        assert_eq!(
            get_entity_key_for_account(&config, None, "stranger", true, "pw"),
            GetEntityKeyOutcome::LoadFromUsername {
                type_id: 1,
                username: "stranger".to_owned(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_owned(),
            }
        );
    }

    /// The query's exception maps to the general-failure status, and no
    /// unknown-user accept reroutes it (`:100-105` only routes
    /// `NO_SUCH_USER`).
    #[test]
    fn a_general_failure_stays_a_failure() {
        let config = BillingConfig::from_name(true, true, false, "Avatar", |_| Some(1));
        assert_eq!(
            dispatch_get(AccountLogOn::RejectedDbGeneralFailure, None, &config),
            GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedDbGeneralFailure,
                error_msg: String::new(),
            }
        );
    }

    #[test]
    fn the_set_dispatch_reports_the_key_both_ways() {
        let key = EntityKey {
            type_id: 1,
            db_id: 7,
        };
        assert_eq!(dispatch_set(key, true), SetEntityKeyOutcome::Success(key));
        assert_eq!(dispatch_set(key, false), SetEntityKeyOutcome::Failure(key));
    }

    /// The XML face: entries resolve through the type names, invalid
    /// types are dropped, and entityID 0 falls to the name lookup.
    #[test]
    fn the_xml_map_harvests_entries_like_init_log_on_mapping() {
        let entries = vec![
            XmlLogOnEntry {
                log_on_name: "hero".to_owned(),
                type_name: "Avatar".to_owned(),
                entity_id: 7,
                entity_name: String::new(),
                password: "secret".to_owned(),
            },
            XmlLogOnEntry {
                log_on_name: "named".to_owned(),
                type_name: "Avatar".to_owned(),
                entity_id: 0,
                entity_name: "Kept".to_owned(),
                password: "pw2".to_owned(),
            },
            XmlLogOnEntry {
                log_on_name: "broken".to_owned(),
                type_name: "Nope".to_owned(),
                entity_id: 9,
                entity_name: String::new(),
                password: "pw3".to_owned(),
            },
        ];
        let system = XmlBillingSystem::from_entries(
            entries,
            |name| (name == "Avatar").then_some(1),
            |type_id, name| {
                assert_eq!((type_id, name), (1, "Kept"));
                Some(3)
            },
        );
        assert_eq!(system.mapping("broken"), None);
        assert_eq!(
            system.mapping("named").map(|m| m.entity_key),
            Some(EntityKey {
                type_id: 1,
                db_id: 3
            })
        );
    }

    /// The XML outcome table mirrors the reference's map lookup.
    #[test]
    fn the_xml_face_rejects_and_accepts_like_the_reference() {
        let entries = vec![XmlLogOnEntry {
            log_on_name: "hero".to_owned(),
            type_name: "Avatar".to_owned(),
            entity_id: 7,
            entity_name: String::new(),
            password: "secret".to_owned(),
        }];
        let system = XmlBillingSystem::from_entries(
            entries,
            |name| (name == "Avatar").then_some(1),
            |_, _| None,
        );
        let strict = BillingConfig::disabled();
        let accepting = BillingConfig::from_name(true, true, false, "Avatar", |_| Some(1));

        assert_eq!(
            system.get_entity_key_for_account(&strict, "hero", "secret"),
            GetEntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: 1,
                    db_id: 7
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
        assert_eq!(
            system.get_entity_key_for_account(&strict, "hero", "bad"),
            GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedInvalidPassword,
                error_msg: String::new(),
            }
        );
        assert_eq!(
            system.get_entity_key_for_account(&strict, "ghost", "x"),
            GetEntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedNoSuchUser,
                error_msg: String::new(),
            }
        );
        assert_eq!(
            system.get_entity_key_for_account(&accepting, "ghost", "x"),
            GetEntityKeyOutcome::CreateNew {
                type_id: 1,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    /// The XML face's base-entity leg: the map is skipped and the
    /// password rides as the base entity's data.
    #[test]
    fn the_xml_base_entity_leg_loads_by_username() {
        let entries = vec![XmlLogOnEntry {
            log_on_name: "hero".to_owned(),
            type_name: "Avatar".to_owned(),
            entity_id: 7,
            entity_name: String::new(),
            password: "secret".to_owned(),
        }];
        let system = XmlBillingSystem::from_entries(
            entries,
            |name| (name == "Avatar").then_some(1),
            |_, _| None,
        );
        let config = BillingConfig::from_name(false, false, true, "Avatar", |_| Some(1));
        assert_eq!(
            system.get_entity_key_for_account(&config, "hero", "pw"),
            GetEntityKeyOutcome::LoadFromUsername {
                type_id: 1,
                username: "hero".to_owned(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_owned(),
            }
        );
    }

    /// The set face upserts the map and hands back the section write
    /// with the type's name.
    #[test]
    fn the_xml_set_upserts_and_writes_the_section_entry() {
        let mut system = XmlBillingSystem::default();
        let (outcome, write) = system.set_entity_key_for_account(
            "hero",
            "secret",
            EntityKey {
                type_id: 1,
                db_id: 7,
            },
            |type_id| (type_id == 1).then(|| "Avatar".to_owned()).unwrap(),
        );
        assert_eq!(
            outcome,
            SetEntityKeyOutcome::Success(EntityKey {
                type_id: 1,
                db_id: 7
            })
        );
        assert_eq!(
            write,
            XmlLogOnEntryWrite {
                log_on_name: "hero".to_owned(),
                password: "secret".to_owned(),
                type_name: "Avatar".to_owned(),
                entity_id: 7,
            }
        );
        assert_eq!(
            system.mapping("hero").map(|m| m.password.as_str()),
            Some("secret")
        );

        // An overwrite keeps the map single-valued per name.
        let (outcome, _) = system.set_entity_key_for_account(
            "hero",
            "new",
            EntityKey {
                type_id: 2,
                db_id: 8,
            },
            |_| "Goblin".to_owned(),
        );
        assert!(matches!(outcome, SetEntityKeyOutcome::Success(_)));
        assert_eq!(
            system.mapping("hero").map(|m| m.entity_key),
            Some(EntityKey {
                type_id: 2,
                db_id: 8
            })
        );
        assert_eq!(
            system.mapping("hero").map(|m| m.password.as_str()),
            Some("new")
        );
    }
}
