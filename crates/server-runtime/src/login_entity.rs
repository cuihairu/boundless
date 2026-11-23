//! BW-06-02-02 the account entity selection and BaseApp dispatch.
//!
//! After the billing system resolves the account (`billing.rs`), the DBApp's
//! `LoginHandler` picks the account entity and routes the login to a BaseApp
//! (`server/dbapp/login_handler.cpp`). The reference state machine:
//!
//! * `onGetEntityKeyForAccountSuccess` loads the entity by key;
//! * `onGetEntityKeyForAccountLoadFromUsername` loads by username and creates
//!   on failure;
//! * `onGetEntityKeyForAccountCreateNew` creates a default entity;
//! * `onGetEntityCompleted` either checks the entity out (reserving its base
//!   mailbox), creates it when a load fails and that was requested, or rejects
//!   with `LOGIN_REJECTED_NO_SUCH_USER`;
//! * `checkOutEntity` routes an entity that already has a live base (or is
//!   being checked out by another login) to the re-logon path
//!   (`DBApp::onLogOnLoggedOnUser`, the BW-06-02-04 duplicate login), and
//!   otherwise reserves the base mailbox;
//! * a remembered new entity is persisted (`putEntity`) and its logon mapping
//!   is remembered (`setEntityKeyForAccount`) before the dispatch;
//! * `sendCreateEntityMsg` sends the `createEntity` message to the BaseAppMgr,
//!   which picks the least-loaded BaseApp (`baseappmgr::create_entity`).
//!
//! The Mercury wire and the DB storage are not modelled here: the methods
//! return the next `EntityStep`, and the storage calls
//! (`getEntity`/`putEntity`/`defaultEntityToStrm`) are the caller's
//! responsibility at each `Load`/`Put`/`Create` step.

use crate::baseappmgr::Addr;
use crate::billing::{AccountResolution, EntityKey, failure_message};
use crate::loginapp::LogOnStatus;

/// `EntityTypeID` (`lib/entitydef/entity_description.hpp`).
pub type EntityTypeID = crate::billing::EntityTypeID;
/// `DatabaseID` (`lib/db_storage/entity_key.hpp`).
pub type DatabaseID = crate::billing::DatabaseID;

/// `EntityDBKey` (`lib/db_storage/entity_key.hpp`): an entity key plus the
/// optional name used for the username-based load and the new-entity name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityDBKey {
    /// `typeID`.
    pub type_id: EntityTypeID,
    /// `dbID`.
    pub db_id: DatabaseID,
    /// The entity name, or empty when loading by database id.
    pub name: String,
}

impl EntityDBKey {
    /// Constructor.
    pub fn new(type_id: EntityTypeID, db_id: DatabaseID, name: &str) -> Self {
        Self {
            type_id,
            db_id,
            name: name.to_string(),
        }
    }

    /// From an `EntityKey` (no name).
    pub fn from_key(key: EntityKey) -> Self {
        Self::new(key.type_id, key.db_id, "")
    }

    /// `EntityKey`.
    pub fn to_key(&self) -> EntityKey {
        EntityKey::new(self.type_id, self.db_id)
    }
}

/// An `EntityMailBoxRef`-like base mailbox: where a logged-on entity lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseMailbox {
    /// `type`.
    pub type_id: EntityTypeID,
    /// `id`.
    pub id: u32,
    /// `addr`.
    pub addr: Addr,
}

/// The `LoginHandler` state, mirroring `server/dbapp/login_handler.cpp`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginEntityHandler {
    entity_key: EntityDBKey,
    logon_name: String,
    data_for_client: String,
    data_for_base_entity: String,
    should_create_new_on_load_failure: bool,
    should_remember_new_on_load_failure: bool,
    base_ref: Option<BaseMailbox>,
    /// The createEntity bundle's dbID slot (`pStrmDbID_`), once prepared.
    db_id_slot: Option<DatabaseID>,
}

impl LoginEntityHandler {
    /// Constructor: the `LoginHandler` defaults.
    pub fn new() -> Self {
        Self {
            entity_key: EntityDBKey::new(0, 0, ""),
            logon_name: String::new(),
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
            should_create_new_on_load_failure: false,
            should_remember_new_on_load_failure: false,
            base_ref: None,
            db_id_slot: None,
        }
    }

    /// `entityKey_`.
    pub fn entity_key(&self) -> &EntityDBKey {
        &self.entity_key
    }

    /// `pParams_->username()`.
    pub fn logon_name(&self) -> &str {
        &self.logon_name
    }

    /// `dataForClient_`.
    pub fn data_for_client(&self) -> &str {
        &self.data_for_client
    }

    /// `dataForBaseEntity_`.
    pub fn data_for_base_entity(&self) -> &str {
        &self.data_for_base_entity
    }

    /// `pBaseRef_`.
    pub fn base_ref(&self) -> Option<BaseMailbox> {
        self.base_ref
    }

    /// `pStrmDbID_` (the pinned createEntity bundle dbID slot).
    pub fn db_id_slot(&self) -> Option<DatabaseID> {
        self.db_id_slot
    }

    /// `login()` completed billing: start the entity step for the resolution.
    pub fn begin(&mut self, resolution: &AccountResolution, username: &str) -> EntityStep {
        match resolution {
            AccountResolution::Success {
                entity_key,
                data_for_client,
                data_for_base_entity,
            } => {
                self.logon_name = username.to_string();
                self.data_for_client = data_for_client.clone();
                self.data_for_base_entity = data_for_base_entity.clone();
                let key = EntityDBKey::from_key(*entity_key);
                self.entity_key = key.clone();
                // `prepareCreateEntityBundle(typeID, dbID, ...)`.
                self.db_id_slot = Some(key.db_id);
                EntityStep::LoadByKey { key }
            }
            AccountResolution::LoadFromUsername {
                entity_type,
                username: load_username,
                should_create_new_on_load_failure,
                should_remember_new_on_load_failure,
                data_for_client,
                data_for_base_entity,
            } => {
                self.logon_name = load_username.clone();
                self.data_for_client = data_for_client.clone();
                self.data_for_base_entity = data_for_base_entity.clone();
                self.should_create_new_on_load_failure = *should_create_new_on_load_failure;
                self.should_remember_new_on_load_failure = *should_remember_new_on_load_failure;
                let key = EntityDBKey::new(*entity_type, 0, load_username);
                self.entity_key = key.clone();
                // `prepareCreateEntityBundle(entityType, 0, ...)`.
                self.db_id_slot = Some(0);
                EntityStep::LoadByUsername {
                    entity_type: *entity_type,
                    username: load_username.clone(),
                    should_create_new_on_load_failure: *should_create_new_on_load_failure,
                    should_remember_new_on_load_failure: *should_remember_new_on_load_failure,
                }
            }
            AccountResolution::CreateNew {
                entity_type,
                should_remember,
                data_for_client,
                data_for_base_entity,
            } => {
                self.logon_name = username.to_string();
                self.data_for_client = data_for_client.clone();
                self.data_for_base_entity = data_for_base_entity.clone();
                self.entity_key.type_id = *entity_type;
                EntityStep::CreateEntity {
                    type_id: *entity_type,
                    should_remember: *should_remember,
                    entity_name: self.new_entity_name(),
                }
            }
            AccountResolution::Failure { status, error_msg } => {
                let message = failure_message(*status, error_msg);
                EntityStep::Reject {
                    status: *status,
                    message,
                }
            }
        }
    }

    /// `onGetEntityCompleted` (`login_handler.cpp`): the entity load finished.
    pub fn on_entity_loaded(
        &mut self,
        is_ok: bool,
        entity_key: EntityDBKey,
        base_location: Option<BaseMailbox>,
    ) -> EntityStep {
        if !is_ok {
            if self.should_create_new_on_load_failure {
                let type_id = entity_key.type_id;
                let should_remember = self.should_remember_new_on_load_failure;
                // entityKey_ is the load key (type, 0, username); its name is
                // the new entity's name.
                self.entity_key = entity_key;
                return EntityStep::CreateEntity {
                    type_id,
                    should_remember,
                    entity_name: self.new_entity_name(),
                };
            }
            return EntityStep::Reject {
                status: LogOnStatus::LoginRejectedNoSuchUser,
                message: "Failed to load entity.".to_string(),
            };
        }

        self.entity_key = entity_key.clone();
        self.base_ref = base_location;
        // The createEntity bundle was prepared with dbID 0; pin it now.
        if let Some(slot) = &mut self.db_id_slot {
            *slot = entity_key.db_id;
        }
        // The caller decides the checkout with `DBApp::onStartEntityCheckout`
        // and then calls `check_out_entity`.
        EntityStep::CheckOut
    }

    /// `checkOutEntity` (`login_handler.cpp`).
    ///
    /// `already_checked_out` is `DBApp::onStartEntityCheckout`'s negation: the
    /// reference calls it to learn whether the entity is not already logged on
    /// or being checked out; `true` here routes the login to the re-logon path.
    pub fn check_out_entity(&mut self, already_checked_out: bool) -> EntityStep {
        if self.base_ref.is_none() && !already_checked_out {
            // Not checked out and not being checked out: reserve the mailbox.
            EntityStep::ReserveBaseMailbox {
                key: self.entity_key.clone(),
            }
        } else {
            // Checked out (a live base) or another login in progress: the
            // BW-06-02-04 re-logon path. `existing_base` is NULL when the
            // entity is merely being checked out.
            EntityStep::AlreadyLoggedOn {
                key: self.entity_key.clone(),
                existing_base: self.base_ref,
            }
        }
    }

    /// `createNewEntity` (`login_handler.cpp`): create a default entity.
    ///
    /// `default_entity_ok` is `DBApp::defaultEntityToStrm`: whether the entity
    /// type's default property set could be produced.
    pub fn create_new_entity(
        &mut self,
        type_id: EntityTypeID,
        should_remember: bool,
        default_entity_ok: bool,
    ) -> EntityStep {
        self.entity_key.type_id = type_id;

        // `prepareCreateEntityBundle(typeID, 0, ...)`: the bundle's dbID slot
        // is prepared with 0 once.
        if self.db_id_slot.is_none() {
            self.db_id_slot = Some(0);
        }

        if !default_entity_ok {
            return EntityStep::Reject {
                status: LogOnStatus::LoginCustomDefinedError,
                message: "Failed to create default entity".to_string(),
            };
        }

        if should_remember {
            EntityStep::PutEntity {
                key: self.entity_key.clone(),
                entity_name: self.new_entity_name(),
            }
        } else {
            // *pStrmDbID_ = 0; the default entity streams straight into the
            // createEntity bundle.
            if let Some(slot) = &mut self.db_id_slot {
                *slot = 0;
            }
            self.dispatch_create_entity()
        }
    }

    /// `onPutEntityComplete` (`login_handler.cpp`): the remembered default
    /// entity was persisted.
    pub fn on_entity_put(&mut self, is_ok: bool, db_id: DatabaseID) -> EntityStep {
        self.entity_key.db_id = db_id;
        if let Some(slot) = &mut self.db_id_slot {
            *slot = db_id;
        }

        if !is_ok {
            // "Failed to write default entity": the reserve fails with a
            // database failure (the reference lets the login fail here).
            return EntityStep::Reject {
                status: LogOnStatus::LoginRejectedDbGeneralFailure,
                message: "Unexpected database failure.".to_string(),
            };
        }

        // Remember the account's entity key; the logon record is then cached.
        EntityStep::SetEntityKeyForAccount {
            username: self.logon_name.clone(),
            key: self.entity_key.to_key(),
        }
    }

    /// `onSetEntityKeyForAccountSuccess` (`login_handler.cpp`).
    pub fn on_set_entity_key_for_account_success(&mut self) -> EntityStep {
        self.dispatch_create_entity()
    }

    /// `onSetEntityKeyForAccountFailure` (`login_handler.cpp`): the billing
    /// system could not remember the new mapping.
    pub fn on_set_entity_key_for_account_failure(&mut self) -> EntityStep {
        EntityStep::Reject {
            status: LogOnStatus::LoginRejectedAuthServiceGeneralFailure,
            message: "External billing system failed to write new entity.".to_string(),
        }
    }

    /// `onReservedBaseMailbox` (`login_handler.cpp`): the base mailbox was
    /// reserved (the `bigworldLogOns` record exists).
    pub fn on_base_reserved(&mut self, is_ok: bool) -> EntityStep {
        if is_ok {
            self.dispatch_create_entity()
        } else {
            EntityStep::Reject {
                status: LogOnStatus::LoginRejectedDbGeneralFailure,
                message: "Unexpected database failure.".to_string(),
            }
        }
    }

    /// `sendCreateEntityMsg`: the createEntity message goes to the BaseAppMgr.
    fn dispatch_create_entity(&self) -> EntityStep {
        EntityStep::DispatchCreateEntity {
            key: self.entity_key.clone(),
            base_ref: self.base_ref,
            db_id_slot: self.db_id_slot.unwrap_or(0),
        }
    }

    /// `newEntityName` (`login_handler.cpp`): the entity key's name when set,
    /// otherwise the logon name.
    fn new_entity_name(&self) -> String {
        if self.entity_key.name.is_empty() {
            self.logon_name.clone()
        } else {
            self.entity_key.name.clone()
        }
    }
}

impl Default for LoginEntityHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// The next step of the DBApp's entity handling for a login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntityStep {
    /// `loadEntity(EntityDBKey(typeID, dbID, ""))`.
    LoadByKey { key: EntityDBKey },
    /// `loadEntity(EntityDBKey(entityType, 0, username))` after
    /// `onGetEntityKeyForAccountLoadFromUsername`.
    LoadByUsername {
        entity_type: EntityTypeID,
        username: String,
        should_create_new_on_load_failure: bool,
        should_remember_new_on_load_failure: bool,
    },
    /// `createNewEntity` entry: the caller resolves the entity type's default
    /// property set (`defaultEntityToStrm`) and calls `create_new_entity`.
    CreateEntity {
        type_id: EntityTypeID,
        should_remember: bool,
        entity_name: String,
    },
    /// The entity loaded and is ready to be checked out; the caller learns
    /// whether it is already checked out (`DBApp::onStartEntityCheckout`) and
    /// calls `check_out_entity`.
    CheckOut,
    /// The account entity is loaded and checked out: reserve its base mailbox
    /// (`setBaseEntityLocation`).
    ReserveBaseMailbox { key: EntityDBKey },
    /// The account entity is already logged on or logging in: the BW-06-02-04
    /// re-logon path. `existing_base` is `None` when another login is merely
    /// checking the entity out.
    AlreadyLoggedOn {
        key: EntityDBKey,
        existing_base: Option<BaseMailbox>,
    },
    /// A remembered default entity is persisted (`putEntity`); its logon
    /// mapping is then remembered (`setEntityKeyForAccount`).
    PutEntity {
        key: EntityDBKey,
        entity_name: String,
    },
    /// A remembered default entity was persisted; the logon mapping must be
    /// remembered (`setEntityKeyForAccount`).
    SetEntityKeyForAccount { username: String, key: EntityKey },
    /// The logon record is cached after a successful putEntity
    /// (`logOnRecordsCache().insert`).
    CacheLogOnRecord {
        key: EntityDBKey,
        base_ref: BaseMailbox,
    },
    /// `sendCreateEntityMsg`: dispatch the createEntity message to the
    /// BaseAppMgr, which picks the least-loaded BaseApp.
    DispatchCreateEntity {
        key: EntityDBKey,
        base_ref: Option<BaseMailbox>,
        db_id_slot: DatabaseID,
    },
    /// The login is rejected.
    Reject {
        status: LogOnStatus,
        message: String,
    },
}

/// The `createEntity` reply from the BaseAppMgr (`LoginHandler::handleMessage`,
/// `login_handler.cpp`).
///
/// A `proxy_addr` with `ip == 0` is the error path; the reference encodes the
/// error in `port` (`CREATE_ENTITY_ERROR_NO_BASEAPPS` /
/// `CREATE_ENTITY_ERROR_BASEAPPS_OVERLOADED`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyReply {
    /// The proxy was created at the BaseApp.
    Success {
        /// The proxy's client address.
        proxy_addr: Addr,
    },
    /// The BaseAppMgr reported an error code in the reply port.
    Error { port: u16 },
}

impl ProxyReply {
    /// `handleMessage`'s `LOGIN_REJECTED_*` status for an error reply.
    pub fn reject_status(self) -> LogOnStatus {
        match self {
            ProxyReply::Success { .. } => LogOnStatus::LoggedOn,
            ProxyReply::Error { port } => match port {
                1 => LogOnStatus::LoginRejectedNoBaseApps,
                2 => LogOnStatus::LoginRejectedBaseAppOverload,
                _ => LogOnStatus::LoginCustomDefinedError,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT_TYPE: EntityTypeID = 3;
    const ACCOUNT_DBID: DatabaseID = 42;

    fn success_resolution() -> AccountResolution {
        AccountResolution::Success {
            entity_key: EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID),
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    }

    fn mailbox() -> BaseMailbox {
        BaseMailbox {
            type_id: ACCOUNT_TYPE,
            id: 7,
            addr: (0x7f00_0001, 20000),
        }
    }

    #[test]
    fn a_success_resolution_loads_the_entity_by_key() {
        let mut handler = LoginEntityHandler::new();
        let step = handler.begin(&success_resolution(), "alice");
        assert_eq!(
            step,
            EntityStep::LoadByKey {
                key: EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, "")
            }
        );
        assert_eq!(
            handler.entity_key(),
            &EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, "")
        );
        assert_eq!(handler.logon_name(), "alice");
    }

    #[test]
    fn a_failure_resolution_rejects_with_the_mapped_message() {
        let mut handler = LoginEntityHandler::new();
        let step = handler.begin(
            &AccountResolution::Failure {
                status: LogOnStatus::LoginRejectedInvalidPassword,
                error_msg: String::new(),
            },
            "alice",
        );
        assert_eq!(
            step,
            EntityStep::Reject {
                status: LogOnStatus::LoginRejectedInvalidPassword,
                message: "Invalid password.".to_string(),
            }
        );
    }

    #[test]
    fn a_load_from_username_resolution_loads_by_username() {
        let mut handler = LoginEntityHandler::new();
        let step = handler.begin(
            &AccountResolution::LoadFromUsername {
                entity_type: ACCOUNT_TYPE,
                username: "bob".to_string(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_string(),
            },
            "ignored",
        );
        assert_eq!(
            step,
            EntityStep::LoadByUsername {
                entity_type: ACCOUNT_TYPE,
                username: "bob".to_string(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
            }
        );
        // The handler adopts the loaded username and keeps the password.
        assert_eq!(handler.logon_name(), "bob");
        assert_eq!(handler.data_for_base_entity(), "pw");
    }

    fn create_new_resolution(should_remember: bool) -> AccountResolution {
        AccountResolution::CreateNew {
            entity_type: ACCOUNT_TYPE,
            should_remember,
            data_for_client: String::new(),
            data_for_base_entity: String::new(),
        }
    }

    #[test]
    fn a_create_new_resolution_puts_a_remembered_entity_under_the_logon_name() {
        let mut handler = LoginEntityHandler::new();
        let step = handler.begin(&create_new_resolution(true), "carol");
        assert_eq!(
            step,
            EntityStep::CreateEntity {
                type_id: ACCOUNT_TYPE,
                should_remember: true,
                entity_name: "carol".to_string(),
            }
        );
        assert_eq!(handler.entity_key().type_id, ACCOUNT_TYPE);
        // The default entity streams OK; a remembered entity is persisted.
        let step = handler.create_new_entity(ACCOUNT_TYPE, true, true);
        assert_eq!(
            step,
            EntityStep::PutEntity {
                key: EntityDBKey::new(ACCOUNT_TYPE, 0, ""),
                entity_name: "carol".to_string(),
            }
        );
        assert_eq!(handler.db_id_slot(), Some(0));
    }

    #[test]
    fn a_create_new_resolution_dispatches_directly_when_not_remembered() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&create_new_resolution(false), "frank");
        let step = handler.create_new_entity(ACCOUNT_TYPE, false, true);
        assert_eq!(
            step,
            EntityStep::DispatchCreateEntity {
                key: EntityDBKey::new(ACCOUNT_TYPE, 0, ""),
                base_ref: None,
                db_id_slot: 0,
            }
        );
    }

    #[test]
    fn a_failed_default_entity_rejects_with_a_custom_error() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&create_new_resolution(true), "carol");
        let step = handler.create_new_entity(ACCOUNT_TYPE, true, false);
        assert_eq!(
            step,
            EntityStep::Reject {
                status: LogOnStatus::LoginCustomDefinedError,
                message: "Failed to create default entity".to_string(),
            }
        );
    }

    #[test]
    fn a_loaded_entity_with_no_base_is_reserved() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&success_resolution(), "alice");
        let step =
            handler.on_entity_loaded(true, EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""), None);
        assert_eq!(step, EntityStep::CheckOut);
        // The dbID slot is pinned to the loaded entity's id.
        assert_eq!(handler.db_id_slot(), Some(ACCOUNT_DBID));
        // Not checked out elsewhere: the caller reserves the mailbox.
        let step = handler.check_out_entity(false);
        assert_eq!(
            step,
            EntityStep::ReserveBaseMailbox {
                key: EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, "")
            }
        );
    }

    #[test]
    fn a_loaded_entity_with_a_live_base_routes_to_relogon() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&success_resolution(), "alice");
        let step = handler.on_entity_loaded(
            true,
            EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""),
            Some(mailbox()),
        );
        assert_eq!(step, EntityStep::CheckOut);
        // A live base always routes to the re-logon path.
        let step = handler.check_out_entity(false);
        assert_eq!(
            step,
            EntityStep::AlreadyLoggedOn {
                key: EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""),
                existing_base: Some(mailbox()),
            }
        );
    }

    #[test]
    fn a_being_checked_out_entity_routes_to_relogon_without_a_base() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&success_resolution(), "alice");
        handler.on_entity_loaded(true, EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""), None);
        // Another login is checking the entity out.
        let step = handler.check_out_entity(true);
        assert_eq!(
            step,
            EntityStep::AlreadyLoggedOn {
                key: EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""),
                existing_base: None,
            }
        );
    }

    #[test]
    fn a_failed_load_creates_when_requested() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(
            &AccountResolution::LoadFromUsername {
                entity_type: ACCOUNT_TYPE,
                username: "dave".to_string(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            },
            "ignored",
        );
        let step = handler.on_entity_loaded(false, EntityDBKey::new(ACCOUNT_TYPE, 0, "dave"), None);
        assert_eq!(
            step,
            EntityStep::CreateEntity {
                type_id: ACCOUNT_TYPE,
                should_remember: true,
                entity_name: "dave".to_string(),
            }
        );
        // The caller streams the default entity, then the handler persists it.
        let step = handler.create_new_entity(ACCOUNT_TYPE, true, true);
        assert_eq!(
            step,
            EntityStep::PutEntity {
                key: EntityDBKey::new(ACCOUNT_TYPE, 0, "dave"),
                entity_name: "dave".to_string(),
            }
        );
    }

    #[test]
    fn a_failed_load_without_create_rejects() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&success_resolution(), "alice");
        let step = handler.on_entity_loaded(false, EntityDBKey::new(ACCOUNT_TYPE, 0, ""), None);
        assert_eq!(
            step,
            EntityStep::Reject {
                status: LogOnStatus::LoginRejectedNoSuchUser,
                message: "Failed to load entity.".to_string(),
            }
        );
    }

    #[test]
    fn a_remembered_put_remembers_the_mapping_then_dispatches() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&create_new_resolution(true), "eve");
        handler.create_new_entity(ACCOUNT_TYPE, true, true);
        // PutEntity → SetEntityKeyForAccount.
        let step = handler.on_entity_put(true, ACCOUNT_DBID);
        assert_eq!(
            step,
            EntityStep::SetEntityKeyForAccount {
                username: "eve".to_string(),
                key: EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID),
            }
        );
        // Then dispatch on the billing success.
        let step = handler.on_set_entity_key_for_account_success();
        assert_eq!(
            step,
            EntityStep::DispatchCreateEntity {
                key: EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""),
                base_ref: None,
                db_id_slot: ACCOUNT_DBID,
            }
        );
    }

    #[test]
    fn a_failed_put_rejects_with_a_database_failure() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(
            &AccountResolution::CreateNew {
                entity_type: ACCOUNT_TYPE,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            },
            "eve",
        );
        let step = handler.on_entity_put(false, 0);
        assert_eq!(
            step,
            EntityStep::Reject {
                status: LogOnStatus::LoginRejectedDbGeneralFailure,
                message: "Unexpected database failure.".to_string(),
            }
        );
    }

    #[test]
    fn a_reserved_mailbox_dispatches_to_the_base_app_mgr() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&success_resolution(), "alice");
        handler.on_entity_loaded(true, EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""), None);
        // The caller reserves the mailbox; the handler then dispatches.
        let step = handler.on_base_reserved(true);
        assert_eq!(
            step,
            EntityStep::DispatchCreateEntity {
                key: EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""),
                base_ref: None,
                db_id_slot: ACCOUNT_DBID,
            }
        );
    }

    #[test]
    fn a_failed_reserve_rejects_with_a_database_failure() {
        let mut handler = LoginEntityHandler::new();
        handler.begin(&success_resolution(), "alice");
        handler.on_entity_loaded(true, EntityDBKey::new(ACCOUNT_TYPE, ACCOUNT_DBID, ""), None);
        let step = handler.on_base_reserved(false);
        assert_eq!(
            step,
            EntityStep::Reject {
                status: LogOnStatus::LoginRejectedDbGeneralFailure,
                message: "Unexpected database failure.".to_string(),
            }
        );
    }

    #[test]
    fn a_billing_set_failure_rejects_with_the_auth_service_failure() {
        let mut handler = LoginEntityHandler::new();
        let step = handler.on_set_entity_key_for_account_failure();
        assert_eq!(
            step,
            EntityStep::Reject {
                status: LogOnStatus::LoginRejectedAuthServiceGeneralFailure,
                message: "External billing system failed to write new entity.".to_string(),
            }
        );
    }

    #[test]
    fn proxy_reply_error_codes_map_to_login_statuses() {
        assert_eq!(
            ProxyReply::Error { port: 1 }.reject_status(),
            LogOnStatus::LoginRejectedNoBaseApps
        );
        assert_eq!(
            ProxyReply::Error { port: 2 }.reject_status(),
            LogOnStatus::LoginRejectedBaseAppOverload
        );
        assert_eq!(
            ProxyReply::Error { port: 99 }.reject_status(),
            LogOnStatus::LoginCustomDefinedError
        );
        assert_eq!(
            ProxyReply::Success { proxy_addr: (0, 0) }.reject_status(),
            LogOnStatus::LoggedOn
        );
    }
}
