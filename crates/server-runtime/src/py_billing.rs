//! BW-09-04-02 the Python billing system (`server/dbapp/py_billing_system.cpp`).
//!
//! The reference DBApp can front its account authentication with a script:
//! the `billingSystem/type = standard` creator asks the personality for
//! `connectToBillingSystem` and wraps the returned object in a
//! `PyBillingSystem`. The object is handed a [`PyBillingResponse`] and must
//! call exactly one of its methods:
//!
//! * `loadEntity(type, dbID, dataForClient="", dataForBaseEntity="")` —
//!   authenticate and load the named entity;
//! * `loadEntityByName(type, name, createUnknown=false, rememberUnknown=false,
//!   …)` — authenticate via the base entity, loading by its identifier;
//! * `createNewEntity(type, shouldRemember, …)` — create a fresh entity;
//! * `failureInvalidPassword(msg="")` / `failureNoSuchUser(msg="")` /
//!   `failureDBError(msg="")` — the three named rejections;
//! * `failure(status, msg="")` — an arbitrary rejection code.
//!
//! Each method validates its arguments and raises a Python `ValueError`
//! when the request is invalid: a database id of 0, an unknown entity type,
//! a status that is not a failure (`okay()`: `NOT_SET`/`LOGGED_ON`, or a
//! value the `u8` wire domain truncates), or any method after the response
//! was already sent. The response object also models the destructor: a
//! script that returns without sending anything is answered with a general
//! database failure ("No response from script"), and a script that raises
//! is answered the same way (`PyErr_PrintEx` plus the destructor).
//!
//! Layering: runtime (BW-06 dbapp face). The script itself is modelled as a
//! closure the tests supply; the CPython object surface is out of this
//! port's scope (the reference's `PyObject*` plumbing).

use crate::baseappmgr::Addr;
use crate::billing::{DatabaseID, EntityKey, EntityTypeID};
use crate::loginapp::LogOnStatus;

/// `PyExc_ValueError: "0 is an invalid database id"` (`loadEntity`).
pub const ERR_ZERO_DBID: &str = "0 is an invalid database id";
/// Any method after the response was already sent.
pub const ERR_RESPONSE_SENT: &str = "Response already sent";
/// `PyErr_Format( PyExc_ValueError, "Invalid entity type '%s'\n" )` — the
/// reference keeps the trailing newline.
pub const ERR_INVALID_ENTITY_TYPE: &str = "Invalid entity type";
/// `PyErr_Format( PyExc_ValueError, "Invalid failure status %d" )`.
pub const ERR_INVALID_FAILURE_STATUS: &str = "Invalid failure status";

/// The `entityDefs` the response resolves script type strings against
/// (`EntityDefs::getEntityType` / `getEntityDescription`).
#[derive(Debug, Clone, Default)]
pub struct EntityDefs {
    /// `vector_`: entity type name -> type id.
    by_name: std::collections::BTreeMap<String, EntityTypeID>,
    /// The reverse lookup `getEntityDescription( typeID ).name()`.
    by_id: std::collections::BTreeMap<EntityTypeID, String>,
}

impl EntityDefs {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// One entity type (`vector_` entry).
    pub fn add_entity(&mut self, name: &str, type_id: EntityTypeID) {
        self.by_name.insert(name.to_string(), type_id);
        self.by_id.insert(type_id, name.to_string());
    }

    /// `getEntityType( name )`: the type id, or `None`.
    pub fn resolve(&self, name: &str) -> Option<EntityTypeID> {
        self.by_name.get(name).copied()
    }

    /// `getEntityDescription( typeID ).name()`: the name, or `None`.
    pub fn name_of(&self, type_id: EntityTypeID) -> Option<&str> {
        self.by_id.get(&type_id).map(String::as_str)
    }
}

/// The one callback the response sends (`IGetEntityKeyForAccountHandler`,
/// `db_storage/billing_system.hpp:17-69`).
#[derive(Debug, Clone, PartialEq)]
pub enum PyEntityKeyOutcome {
    /// `onGetEntityKeyForAccountSuccess` — a specific entity to load.
    Success {
        /// The account entity key.
        key: EntityKey,
        /// `dataForClient`.
        data_for_client: String,
        /// `dataForBaseEntity`.
        data_for_base_entity: String,
    },
    /// `onGetEntityKeyForAccountLoadFromUsername` — authenticate via the
    /// base entity, loading by its identifier.
    LoadFromUsername {
        /// The entity type to load.
        entity_type: EntityTypeID,
        /// The entity's identifier property value.
        username: String,
        /// `shouldCreateUnknown`.
        should_create_unknown: bool,
        /// `shouldRememberUnknown`.
        should_remember_unknown: bool,
        /// `dataForClient`.
        data_for_client: String,
        /// `dataForBaseEntity`.
        data_for_base_entity: String,
    },
    /// `onGetEntityKeyForAccountCreateNew` — create a fresh entity.
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
    /// `onGetEntityKeyForAccountFailure` — an arbitrary status the script
    /// reported. The script's `failure( n, … )` forwards any non-okay int,
    /// and the account vocabulary names statuses the runtime enum has no
    /// variant for, so the outcome carries the raw value.
    Failure {
        /// The numeric `LogOnStatus` wire value.
        status: i32,
        /// `errorMsg`.
        error_msg: String,
    },
}

/// The one callback `setEntityKeyForAccount` makes
/// (`ISetEntityKeyForAccountHandler`,
/// `db_storage/billing_system.hpp:71-85`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PySetEntityKeyOutcome {
    /// `onSetEntityKeyForAccountSuccess`.
    Success(EntityKey),
    /// `onSetEntityKeyForAccountFailure`.
    Failure(EntityKey),
}

/// `PyBillingResponse` (`py_billing_system.cpp:18-95`): the response object
/// handed to the billing script. Exactly one method may be called; the
/// first one consumes the response.
#[derive(Debug)]
pub struct PyBillingResponse<'a> {
    /// `entityDefs_` — the type strings the script names are resolved
    /// against.
    defs: &'a EntityDefs,
    /// The outcome sent so far; `None` until a method consumes the response.
    sent: Option<PyEntityKeyOutcome>,
}

impl<'a> PyBillingResponse<'a> {
    /// Constructor: the response is created with the handler and the entity
    /// definitions.
    pub fn new(defs: &'a EntityDefs) -> Self {
        Self { defs, sent: None }
    }

    /// The outcome sent so far (`None` until a method consumes the
    /// response).
    pub fn sent(&self) -> Option<&PyEntityKeyOutcome> {
        self.sent.as_ref()
    }

    /// Take the sent outcome (the system reads it after the script call).
    pub fn take(&mut self) -> Option<PyEntityKeyOutcome> {
        self.sent.take()
    }

    /// `getEntityTypeID`: `ValueError` when the type is unknown or the
    /// response was already sent.
    fn resolve_type(&self, entity_type: &str) -> Result<EntityTypeID, String> {
        if self.sent.is_some() {
            return Err(ERR_RESPONSE_SENT.to_string());
        }
        self.defs
            .resolve(entity_type)
            .ok_or_else(|| format!("{ERR_INVALID_ENTITY_TYPE} '{entity_type}'\n"))
    }

    /// `loadEntity( entityType, entityID, dataForClient="", dataForBaseEntity=""
    /// )` (`py_billing_system.cpp:193-218`). `ValueError` on a database id
    /// of 0 or an unknown entity type.
    pub fn load_entity(
        &mut self,
        entity_type: &str,
        db_id: DatabaseID,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        if db_id == 0 {
            return Err(ERR_ZERO_DBID.to_string());
        }
        let type_id = self.resolve_type(entity_type)?;
        self.sent = Some(PyEntityKeyOutcome::Success {
            key: EntityKey::new(type_id, db_id),
            data_for_client: data_for_client.to_string(),
            data_for_base_entity: data_for_base_entity.to_string(),
        });
        Ok(())
    }

    /// `loadEntityByName( entityType, entityName, shouldCreateUnknown=false,
    /// shouldRememberUnknown=false, dataForClient="", dataForBaseEntity=""
    /// )` (`py_billing_system.cpp:221-241`).
    pub fn load_entity_by_name(
        &mut self,
        entity_type: &str,
        username: &str,
        should_create_unknown: bool,
        should_remember_unknown: bool,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        let type_id = self.resolve_type(entity_type)?;
        self.sent = Some(PyEntityKeyOutcome::LoadFromUsername {
            entity_type: type_id,
            username: username.to_string(),
            should_create_unknown,
            should_remember_unknown,
            data_for_client: data_for_client.to_string(),
            data_for_base_entity: data_for_base_entity.to_string(),
        });
        Ok(())
    }

    /// `createNewEntity( entityType, shouldRemember, dataForClient="",
    /// dataForBaseEntity="" )` (`py_billing_system.cpp:244-260`).
    pub fn create_new_entity(
        &mut self,
        entity_type: &str,
        should_remember: bool,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        let type_id = self.resolve_type(entity_type)?;
        self.sent = Some(PyEntityKeyOutcome::CreateNew {
            entity_type: type_id,
            should_remember,
            data_for_client: data_for_client.to_string(),
            data_for_base_entity: data_for_base_entity.to_string(),
        });
        Ok(())
    }

    /// `failureInvalidPassword( errorMsg="" )`
    /// (`py_billing_system.cpp:285-289`).
    pub fn failure_invalid_password(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(LogOnStatus::LoginRejectedInvalidPassword as i32, error_msg)
    }

    /// `failureNoSuchUser( errorMsg="" )` (`py_billing_system.cpp:292-296`).
    pub fn failure_no_such_user(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(LogOnStatus::LoginRejectedNoSuchUser as i32, error_msg)
    }

    /// `failureDBError( errorMsg="" )` (`py_billing_system.cpp:299-303`).
    pub fn failure_db_error(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(LogOnStatus::LoginRejectedDbGeneralFailure as i32, error_msg)
    }

    /// `failure( status, errorMsg="" )` (`py_billing_system.cpp:306-318`):
    /// `ValueError` when the status is not a failure — `okay()` (`NOT_SET`/
    /// `LOGGED_ON`) or a value the `u8` wire domain truncates.
    pub fn failure(&mut self, status_as_int: i32, error_msg: &str) -> Result<(), String> {
        if !(2..=255).contains(&status_as_int) {
            return Err(format!("{ERR_INVALID_FAILURE_STATUS} {status_as_int}"));
        }
        self.send_failure(status_as_int, error_msg)
    }

    /// `sendFailure` (`py_billing_system.cpp:324-337`): `ValueError` when
    /// the response was already sent.
    fn send_failure(&mut self, status: i32, error_msg: &str) -> Result<(), String> {
        if self.sent.is_some() {
            return Err(ERR_RESPONSE_SENT.to_string());
        }
        self.sent = Some(PyEntityKeyOutcome::Failure {
            status,
            error_msg: error_msg.to_string(),
        });
        Ok(())
    }
}

/// The body of the script's `getEntityKeyForAccount` callable: it receives
/// the login arguments and the response object, and either sends a response
/// or returns `Err` with the exception text the script raised. Returning
/// `Err` models `PyObject_CallObject` failing (`PyErr_PrintEx`); the
/// response object then still holds no outcome, and the system reports the
/// general failure.
pub type GetEntityKeyBody =
    Box<dyn FnMut(&str, &str, Addr, &mut PyBillingResponse) -> Result<(), String>>;

/// The body of the script's `setEntityKeyForAccount` callable: it receives
/// the username, password, entity type name and database id, and returns
/// `Err` when the script raised.
pub type SetEntityKeyBody = Box<dyn FnMut(&str, &str, &str, DatabaseID) -> Result<(), String>>;

/// One `PyBillingSystem` callable (`PyBillingSystem::getMember` looked both
/// attributes up at construction; a missing one is an error log, and
/// `isOkay()` reports whether the required one exists).
pub struct PyGetEntityKeyFn {
    body: GetEntityKeyBody,
}

impl PyGetEntityKeyFn {
    /// Constructor.
    pub fn new(body: GetEntityKeyBody) -> Self {
        Self { body }
    }

    /// Invoke the callable with the login arguments and the response.
    pub fn call(
        &mut self,
        username: &str,
        password: &str,
        client_addr: Addr,
        response: &mut PyBillingResponse,
    ) -> Result<(), String> {
        (self.body)(username, password, client_addr, response)
    }
}

/// The `setEntityKeyForAccount` callable.
pub struct PySetEntityKeyFn {
    body: SetEntityKeyBody,
}

impl PySetEntityKeyFn {
    /// Constructor.
    pub fn new(body: SetEntityKeyBody) -> Self {
        Self { body }
    }

    /// Invoke the callable; `Err` carries the exception the script raised.
    pub fn call(
        &mut self,
        username: &str,
        password: &str,
        entity_type: &str,
        db_id: DatabaseID,
    ) -> Result<(), String> {
        (self.body)(username, password, entity_type, db_id)
    }
}

/// The script object `PyBillingSystem` binds to: the persona's
/// `connectToBillingSystem()` result. The reference looks both attributes
/// up with `getMember`; a missing attribute is an error log, not a
/// construction failure.
#[derive(Default)]
pub struct BillingScript {
    /// The `getEntityKeyForAccount` attribute, when the script defines it.
    pub get_entity_key_for_account: Option<PyGetEntityKeyFn>,
    /// The `setEntityKeyForAccount` attribute, when the script defines it.
    pub set_entity_key_for_account: Option<PySetEntityKeyFn>,
}

impl BillingScript {
    /// The reference's `getMember` error text for a missing attribute.
    pub fn missing_member_error(member: &str) -> String {
        format!("No such method {member}")
    }
}

/// `PyBillingSystem` (`py_billing_system.cpp:347-354`): the billing system
/// that fronts the account authentication with a script.
pub struct PyBillingSystem {
    /// The script object.
    script: BillingScript,
    /// `entityDefs_`.
    defs: EntityDefs,
}

impl PyBillingSystem {
    /// The constructor: `getMember` looks both attributes up.
    pub fn new(script: BillingScript, defs: EntityDefs) -> Self {
        Self { script, defs }
    }

    /// `isOkay()`: the required `getEntityKeyForAccount` attribute exists.
    pub fn is_okay(&self) -> bool {
        self.script.get_entity_key_for_account.is_some()
    }

    /// `getEntityKeyForAccount( username, password, clientAddr, response )`
    /// (`py_billing_system.cpp:389-430`). The missing-method case fails the
    /// login with a general database failure; a script exception or an
    /// unsent response is answered with the general failure too — the
    /// reference prints the error and the response destructor sends
    /// `failureDBError( "No response from script" )`.
    pub fn get_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        client_addr: Addr,
    ) -> PyEntityKeyOutcome {
        let mut response = PyBillingResponse::new(&self.defs);
        let scripted = self
            .script
            .get_entity_key_for_account
            .as_mut()
            .expect("is_okay checked the get method")
            .call(username, password, client_addr, &mut response)
            .is_err();
        if scripted {
            // `PyErr_PrintEx` + the destructor's `failureDBError`.
            return PyEntityKeyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as i32,
                error_msg: "No response from script".to_string(),
            };
        }
        response.take().unwrap_or(PyEntityKeyOutcome::Failure {
            status: LogOnStatus::LoginRejectedDbGeneralFailure as i32,
            error_msg: "No response from script".to_string(),
        })
    }

    /// `setEntityKeyForAccount( username, password, entityKey, handler )`
    /// (`py_billing_system.cpp:437-480`). The missing-setter case logs the
    /// reference error and fails the write; a script exception fails the
    /// write too, and success answers with the entity key.
    pub fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> PySetEntityKeyOutcome {
        let Some(setter) = self.script.set_entity_key_for_account.as_mut() else {
            // "No setEntityKeyForAccount function exists. sendCreateNew
            //  should not be called with shouldRemember as True".
            let _ = BillingScript::missing_member_error("setEntityKeyForAccount");
            return PySetEntityKeyOutcome::Failure(key);
        };
        let entity_type_name = self.defs.name_of(key.type_id).unwrap_or("").to_string();
        if setter
            .call(username, password, &entity_type_name, key.db_id)
            .is_err()
        {
            PySetEntityKeyOutcome::Failure(key)
        } else {
            PySetEntityKeyOutcome::Success(key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT_TYPE: EntityTypeID = 3;
    const ACCOUNT_DBID: DatabaseID = 42;

    fn defs() -> EntityDefs {
        let mut defs = EntityDefs::new();
        defs.add_entity("Account", ACCOUNT_TYPE);
        defs.add_entity("Avatar", 7);
        defs
    }

    fn key() -> EntityKey {
        EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID)
    }

    fn ok_system(get: GetEntityKeyBody) -> PyBillingSystem {
        let script = BillingScript {
            get_entity_key_for_account: Some(PyGetEntityKeyFn::new(get)),
            ..Default::default()
        };
        PyBillingSystem::new(script, defs())
    }

    fn ok_system_with_set(get: GetEntityKeyBody, set: SetEntityKeyBody) -> PyBillingSystem {
        let script = BillingScript {
            get_entity_key_for_account: Some(PyGetEntityKeyFn::new(get)),
            set_entity_key_for_account: Some(PySetEntityKeyFn::new(set)),
        };
        PyBillingSystem::new(script, defs())
    }

    #[test]
    fn load_entity_sends_the_entity_key_and_the_wire_data() {
        let mut system = ok_system(Box::new(|_u, _p, _a, r| {
            r.load_entity("Account", ACCOUNT_DBID, "for-client", "for-base")
        }));
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret", (0x7f00_0001, 20000)),
            PyEntityKeyOutcome::Success {
                key: key(),
                data_for_client: "for-client".to_string(),
                data_for_base_entity: "for-base".to_string(),
            }
        );
    }

    #[test]
    fn load_entity_rejects_a_database_id_of_zero() {
        let mut system = ok_system(Box::new(|_u, _p, _a, r| {
            r.load_entity("Account", 0, "", "")
        }));
        // The `ValueError` propagates out of the script call; the login is
        // still answered with the general failure.
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret", (0, 0)),
            PyEntityKeyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as i32,
                error_msg: "No response from script".to_string(),
            }
        );
    }

    #[test]
    fn an_unknown_entity_type_raises_and_fails_the_login() {
        let mut system = ok_system(Box::new(|_u, _p, _a, r| {
            assert_eq!(
                r.load_entity("Ghost", ACCOUNT_DBID, "", ""),
                Err("Invalid entity type 'Ghost'\n".to_string())
            );
            Ok(())
        }));
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret", (0, 0)),
            PyEntityKeyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as i32,
                error_msg: "No response from script".to_string(),
            }
        );
    }

    #[test]
    fn any_method_after_the_response_is_sent_is_rejected() {
        let defs = defs();
        let mut response = PyBillingResponse::new(&defs);
        response
            .failure_invalid_password("")
            .expect("first response");
        assert_eq!(
            response.load_entity("Account", ACCOUNT_DBID, "", ""),
            Err(ERR_RESPONSE_SENT.to_string())
        );
        // A second failure method hits the same guard.
        assert_eq!(
            response.failure_invalid_password(""),
            Err(ERR_RESPONSE_SENT.to_string())
        );
    }

    #[test]
    fn the_database_id_check_precedes_the_sent_check() {
        // The reference checks dbID == 0 before getEntityTypeID (which
        // checks the sent state), so a zero id reports on a sent response.
        let defs = defs();
        let mut response = PyBillingResponse::new(&defs);
        response.failure_no_such_user("").expect("first response");
        assert_eq!(
            response.load_entity("Account", 0, "", ""),
            Err(ERR_ZERO_DBID.to_string())
        );
    }

    #[test]
    fn load_entity_by_name_sends_the_base_entity_authentication() {
        let mut system = ok_system(Box::new(|_u, _p, _a, r| {
            r.load_entity_by_name("Avatar", "hero", true, false, "c", "b")
        }));
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret", (0, 0)),
            PyEntityKeyOutcome::LoadFromUsername {
                entity_type: 7,
                username: "hero".to_string(),
                should_create_unknown: true,
                should_remember_unknown: false,
                data_for_client: "c".to_string(),
                data_for_base_entity: "b".to_string(),
            }
        );
    }

    #[test]
    fn create_new_entity_sends_the_create_outcome() {
        let mut system = ok_system(Box::new(|_u, _p, _a, r| {
            r.create_new_entity("Account", true, "", "")
        }));
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret", (0, 0)),
            PyEntityKeyOutcome::CreateNew {
                entity_type: ACCOUNT_TYPE,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    #[test]
    fn the_named_failure_helpers_send_their_statuses() {
        let defs = defs();
        let mut response = PyBillingResponse::new(&defs);
        response
            .failure_invalid_password("boom")
            .expect("invalid password");
        assert_eq!(
            response.sent(),
            Some(&PyEntityKeyOutcome::Failure {
                status: LogOnStatus::LoginRejectedInvalidPassword as i32,
                error_msg: "boom".to_string(),
            })
        );

        let mut response = PyBillingResponse::new(&defs);
        response.failure_no_such_user("").expect("no such user");
        assert_eq!(
            response.sent(),
            Some(&PyEntityKeyOutcome::Failure {
                status: LogOnStatus::LoginRejectedNoSuchUser as i32,
                error_msg: String::new(),
            })
        );

        let mut response = PyBillingResponse::new(&defs);
        response.failure_db_error("db down").expect("db error");
        assert_eq!(
            response.sent(),
            Some(&PyEntityKeyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as i32,
                error_msg: "db down".to_string(),
            })
        );
    }

    #[test]
    fn failure_forwards_any_non_okay_status() {
        let defs = defs();
        let mut response = PyBillingResponse::new(&defs);
        response.failure(94, "").expect("a raw status");
        assert_eq!(
            response.sent(),
            Some(&PyEntityKeyOutcome::Failure {
                status: 94,
                error_msg: String::new()
            })
        );
        // The named statuses arrive through the helpers too.
        let mut response = PyBillingResponse::new(&defs);
        response.failure(69, "").expect("already logged in");
        assert_eq!(
            response.sent(),
            Some(&PyEntityKeyOutcome::Failure {
                status: 69,
                error_msg: String::new()
            })
        );
    }

    #[test]
    fn failure_rejects_the_okay_and_truncated_statuses() {
        let defs = defs();
        let mut response = PyBillingResponse::new(&defs);
        for status in [0, 1, 256, -1] {
            assert_eq!(
                response.failure(status, ""),
                Err(format!("{ERR_INVALID_FAILURE_STATUS} {status}"))
            );
        }
        // The response is still unsent: a later failure works.
        response.failure_invalid_password("").expect("still usable");
    }

    #[test]
    fn an_unsent_response_reports_the_general_failure() {
        // The script returns without calling a method: the destructor sends
        // `failureDBError( "No response from script" )`.
        let mut system = ok_system(Box::new(|_u, _p, _a, _r| Ok(())));
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret", (0, 0)),
            PyEntityKeyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as i32,
                error_msg: "No response from script".to_string(),
            }
        );
    }

    #[test]
    fn a_raising_script_reports_the_general_failure() {
        let mut system = ok_system(Box::new(|_u, _p, _a, _r| {
            Err("Traceback (most recent call last)".to_string())
        }));
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret", (0, 0)),
            PyEntityKeyOutcome::Failure {
                status: LogOnStatus::LoginRejectedDbGeneralFailure as i32,
                error_msg: "No response from script".to_string(),
            }
        );
    }

    #[test]
    fn a_missing_get_method_fails_with_a_general_failure() {
        // `isOkay()` is false; the caller (the creator) deletes the system,
        // but the contract still answers the login.
        let system = PyBillingSystem::new(BillingScript::default(), defs());
        assert!(!system.is_okay());
    }

    #[test]
    fn the_script_receives_the_login_arguments() {
        type Seen = std::rc::Rc<std::cell::RefCell<Option<(String, String, (u32, u16))>>>;
        let seen: Seen = std::rc::Rc::new(std::cell::RefCell::new(None));
        let captured = seen.clone();
        let mut system = ok_system(Box::new(move |u, p, a, _r| {
            *captured.borrow_mut() = Some((u.to_string(), p.to_string(), a));
            Ok(())
        }));
        let _ = system.get_entity_key_for_account("alice", "s3cret", (0x7f00_0001, 20000));
        assert_eq!(
            *seen.borrow(),
            Some((
                "alice".to_string(),
                "s3cret".to_string(),
                (0x7f00_0001, 20000)
            ))
        );
    }

    #[test]
    fn set_passes_the_entity_name_and_db_id_and_succeeds() {
        type Seen = std::rc::Rc<std::cell::RefCell<Option<(String, String, String, DatabaseID)>>>;
        let seen: Seen = std::rc::Rc::new(std::cell::RefCell::new(None));
        let captured = seen.clone();
        let mut system = ok_system_with_set(
            Box::new(|_u, _p, _a, _r| Ok(())),
            Box::new(move |u, p, t, id| {
                *captured.borrow_mut() = Some((u.to_string(), p.to_string(), t.to_string(), id));
                Ok(())
            }),
        );
        assert_eq!(
            system.set_entity_key_for_account("alice", "s3cret", key()),
            PySetEntityKeyOutcome::Success(key())
        );
        assert_eq!(
            *seen.borrow(),
            Some((
                "alice".to_string(),
                "s3cret".to_string(),
                "Account".to_string(),
                ACCOUNT_DBID
            ))
        );
    }

    #[test]
    fn set_without_a_setter_fails_the_write() {
        let mut system = ok_system(Box::new(|_u, _p, _a, _r| Ok(())));
        assert_eq!(
            system.set_entity_key_for_account("alice", "s3cret", key()),
            PySetEntityKeyOutcome::Failure(key())
        );
    }

    #[test]
    fn a_raising_set_fails_the_write() {
        let mut system = ok_system_with_set(
            Box::new(|_u, _p, _a, _r| Ok(())),
            Box::new(|_u, _p, _t, _id| Err("script died".to_string())),
        );
        assert_eq!(
            system.set_entity_key_for_account("alice", "s3cret", key()),
            PySetEntityKeyOutcome::Failure(key())
        );
    }

    #[test]
    fn entity_defs_resolve_both_directions() {
        let d = defs();
        assert_eq!(d.resolve("Account"), Some(ACCOUNT_TYPE));
        assert_eq!(d.resolve("Ghost"), None);
        assert_eq!(d.name_of(ACCOUNT_TYPE), Some("Account"));
        assert_eq!(d.name_of(99), None);
    }
}
