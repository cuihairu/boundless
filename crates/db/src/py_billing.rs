//! The Python billing system: the script contract for account
//! authentication (`dbapp/py_billing_system.{hpp,cpp}`).
//!
//! When the personality's `connectToBillingSystem` returns an object, the
//! standard creator wraps it in a `PyBillingSystem`
//! (`standard_billing_system.cpp:60-73`). The wrapper hands the script a
//! `PyBillingResponse` and lets the script drive one of seven methods;
//! each folds to one of the standard outcomes in
//! [`crate::billing::GetEntityKeyOutcome`].
//!
//! The contract the script sees is the interesting part, and it is what
//! this module ports:
//!
//! - `loadEntity` refuses `dbID == 0` with
//!   `"0 is an invalid database id"` before it even looks at the type
//!   (`py_billing_system.cpp:198-202`).
//! - Every entity-typed response resolves the type name through the
//!   definitions; an unknown name raises
//!   `"Invalid entity type '<name>'\n"` — the reference's format string
//!   carries a trailing newline (`:276-277`).
//! - A second response raises `"Response already sent"` (`:268-270`,
//!   `:327-330`) — the handler is nulled once it fired.
//! - `failure(int)` refuses the "okay" statuses with
//!   `"Invalid failure status <n>"` (`:306-318`). The reference's second
//!   check (`statusAsInt != status.value()`) is vacuous — the enum cast
//!   is total and `value()` hands the int straight back — so only the
//!   okay check survives the port, with the reasoning recorded.
//! - A response object destroyed with nothing sent reports a general
//!   failure with `"No response from script"` through the destructor
//!   (`:31-43`): this is what a script exception that never answered
//!   produces, because the wrapper prints and clears the traceback and
//!   moves on (`:423-429`).
//! - A script object without `getEntityKeyForAccount` fails every lookup
//!   with a bare general failure (`:394-401`); without
//!   `setEntityKeyForAccount`, every remembered create fails
//!   (`:441-448`).
//!
//! The script itself is a closure here — the embedder (the Python host)
//! plays `PyObject_CallObject`, returning `Err` where the call raised.
//! The wrapper prints and clears such errors, so the port discards the
//! error value the same way.

use foundation::ids::{DatabaseId, EntityTypeId, INVALID_ENTITY_TYPE_ID};
use mercury::endpoint::Address;

use crate::account::log_on_status;
use crate::billing::{GetEntityKeyOutcome, SetEntityKeyOutcome};
use crate::storage::EntityKey;

/// The response object the script drives (`PyBillingResponse`,
/// `py_billing_system.cpp:18-95`): exactly one of its methods may fire.
///
/// The defaults the reference exposes (`OPTARG`) are spelled out in each
/// method; the port takes them explicitly.
pub struct PyBillingResponse<'d> {
    /// `entityDefs_.getEntityType` — the definitions the type names
    /// resolve through.
    resolve: &'d dyn Fn(&str) -> Option<EntityTypeId>,
    /// The outcome once a method fired; `pHandler_ = nullptr` in the
    /// reference.
    outcome: Option<GetEntityKeyOutcome>,
}

impl<'d> PyBillingResponse<'d> {
    fn new(resolve: &'d dyn Fn(&str) -> Option<EntityTypeId>) -> Self {
        Self {
            resolve,
            outcome: None,
        }
    }

    /// Whether a response has already been sent.
    pub fn sent(&self) -> bool {
        self.outcome.is_some()
    }

    /// `getEntityTypeID` (`py_billing_system.cpp:263-282`): the
    /// already-sent check rides in here for the entity-typed methods,
    /// because that is where the reference puts it.
    fn resolve_type(&self, entity_type: &str) -> Result<EntityTypeId, String> {
        if self.sent() {
            return Err("Response already sent".to_owned());
        }
        match (self.resolve)(entity_type) {
            // The reference checks the resolved id
            // (`resultID == INVALID_ENTITY_TYPE_ID`), so an invalid id
            // from the definitions reports exactly like a missing name.
            Some(type_id) if type_id != INVALID_ENTITY_TYPE_ID => Ok(type_id),
            _ => Err(format!("Invalid entity type '{entity_type}'\n")),
        }
    }

    /// `loadEntity` (`py_billing_system.cpp:193-218`): a specific entity,
    /// by database id. The zero-id refusal happens before the sent check,
    /// exactly as the reference orders it.
    pub fn load_entity(
        &mut self,
        entity_type: &str,
        db_id: DatabaseId,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        if db_id == 0 {
            return Err("0 is an invalid database id".to_owned());
        }
        let type_id = self.resolve_type(entity_type)?;
        self.outcome = Some(GetEntityKeyOutcome::Success {
            key: EntityKey { type_id, db_id },
            data_for_client: data_for_client.to_owned(),
            data_for_base_entity: data_for_base_entity.to_owned(),
        });
        Ok(())
    }

    /// `loadEntityByName` (`py_billing_system.cpp:221-241`): load (or
    /// create) by identifier instead of by id — the base-entity
    /// authentication shape. Defaults: both `should*` flags false, both
    /// data strings empty.
    pub fn load_entity_by_name(
        &mut self,
        entity_type: &str,
        entity_name: &str,
        should_create_unknown: bool,
        should_remember_unknown: bool,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        let type_id = self.resolve_type(entity_type)?;
        self.outcome = Some(GetEntityKeyOutcome::LoadFromUsername {
            type_id,
            username: entity_name.to_owned(),
            should_create_new_on_load_failure: should_create_unknown,
            should_remember_new_on_load_failure: should_remember_unknown,
            data_for_client: data_for_client.to_owned(),
            data_for_base_entity: data_for_base_entity.to_owned(),
        });
        Ok(())
    }

    /// `createNewEntity` (`py_billing_system.cpp:244-260`): a brand new
    /// entity instead of a loaded one.
    pub fn create_new_entity(
        &mut self,
        entity_type: &str,
        should_remember: bool,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        let type_id = self.resolve_type(entity_type)?;
        self.outcome = Some(GetEntityKeyOutcome::CreateNew {
            type_id,
            should_remember,
            data_for_client: data_for_client.to_owned(),
            data_for_base_entity: data_for_base_entity.to_owned(),
        });
        Ok(())
    }

    /// `failureInvalidPassword` (`:285-289`).
    pub fn failure_invalid_password(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(log_on_status::LOGIN_REJECTED_INVALID_PASSWORD, error_msg)
    }

    /// `failureNoSuchUser` (`:292-296`).
    pub fn failure_no_such_user(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(log_on_status::LOGIN_REJECTED_NO_SUCH_USER, error_msg)
    }

    /// `failureDBError` (`:299-303`).
    pub fn failure_db_error(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(log_on_status::LOGIN_REJECTED_DB_GENERAL_FAILURE, error_msg)
    }

    /// `failure` (`:306-318`): an arbitrary rejection status as its raw
    /// `LogOnStatus` int. The reference's `statusAsInt != status.value()`
    /// arm is vacuous — the enum cast is total and `value()` hands the
    /// int straight back — so the effective contract is the okay check,
    /// and this port keeps only that, refusing [`log_on_status::NOT_SET`]
    /// and [`log_on_status::LOGGED_ON`].
    pub fn failure(&mut self, status_as_int: i32, error_msg: &str) -> Result<(), String> {
        if log_on_status::okay(status_as_int) {
            return Err(format!("Invalid failure status {status_as_int}"));
        }
        self.send_failure(status_as_int, error_msg)
    }

    /// `sendFailure` (`:324-337`).
    fn send_failure(&mut self, status: i32, error_msg: &str) -> Result<(), String> {
        if self.sent() {
            return Err("Response already sent".to_owned());
        }
        self.outcome = Some(GetEntityKeyOutcome::Failure {
            status,
            error_msg: error_msg.to_owned(),
        });
        Ok(())
    }

    /// `~PyBillingResponse` (`py_billing_system.cpp:31-43`): a response
    /// object destroyed with nothing sent reports the general failure
    /// with the reference's message. This is the outcome a raising script
    /// leaves behind, because the wrapper prints and clears the traceback
    /// without answering (`:423-429`).
    fn finish(self) -> GetEntityKeyOutcome {
        self.outcome.unwrap_or(GetEntityKeyOutcome::Failure {
            status: log_on_status::LOGIN_REJECTED_DB_GENERAL_FAILURE,
            error_msg: "No response from script".to_owned(),
        })
    }
}

/// The script billing system (`PyBillingSystem`,
/// `py_billing_system.cpp:347-480`): the personality's billing object as
/// the port sees it — two script methods, present or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyBillingSystem {
    /// `getEntityKeyFunc_`: the object had a `getEntityKeyForAccount`.
    get_entity_key_func: bool,
    /// `setEntityKeyFunc_`: the object had a `setEntityKeyForAccount`.
    set_entity_key_func: bool,
}

impl PyBillingSystem {
    /// The constructor's `getMember` face (`:347-382`): a missing member
    /// is reported and cleared, leaving the system without that face.
    pub fn new(get_entity_key_func: bool, set_entity_key_func: bool) -> Self {
        Self {
            get_entity_key_func,
            set_entity_key_func,
        }
    }

    /// `isOkay` (`py_billing_system.hpp:33-36`): without the get face the
    /// system cannot start.
    pub fn is_okay(&self) -> bool {
        self.get_entity_key_func
    }

    /// `getEntityKeyForAccount` (`py_billing_system.cpp:389-430`): the
    /// script is called as `(username, password, clientAddr, response)`;
    /// `script` returning `Err` plays the call raising — the wrapper
    /// prints and clears it and lets the destructor speak.
    pub fn get_entity_key_for_account(
        &self,
        username: &str,
        password: &str,
        client_addr: &Address,
        resolve: &dyn Fn(&str) -> Option<EntityTypeId>,
        script: impl FnOnce(&str, &str, &Address, &mut PyBillingResponse) -> Result<(), String>,
    ) -> GetEntityKeyOutcome {
        if !self.get_entity_key_func {
            return GetEntityKeyOutcome::Failure {
                status: log_on_status::LOGIN_REJECTED_DB_GENERAL_FAILURE,
                error_msg: String::new(),
            };
        }
        let mut response = PyBillingResponse::new(resolve);
        let _ = script(username, password, client_addr, &mut response);
        response.finish()
    }

    /// `setEntityKeyForAccount` (`py_billing_system.cpp:437-480`): the
    /// script is called as `(username, password, typeName, dbID)`. A
    /// raising call fails the set; anything else succeeds it.
    pub fn set_entity_key_for_account(
        &self,
        username: &str,
        password: &str,
        key: EntityKey,
        type_name_of: impl FnOnce(EntityTypeId) -> String,
        script: impl FnOnce(&str, &str, &str, DatabaseId) -> Result<(), String>,
    ) -> SetEntityKeyOutcome {
        if !self.set_entity_key_func {
            // "sendCreateNew should not be called with shouldRemember as
            // True" — the reference's message for exactly this shape.
            return SetEntityKeyOutcome::Failure(key);
        }
        match script(username, password, &type_name_of(key.type_id), key.db_id) {
            Ok(()) => SetEntityKeyOutcome::Success(key),
            Err(_) => SetEntityKeyOutcome::Failure(key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Avatar → 1`, everything else unknown — the definitions face.
    fn resolve(name: &str) -> Option<EntityTypeId> {
        (name == "Avatar").then_some(1)
    }

    #[test]
    fn load_entity_delivers_the_key_and_data() {
        let mut response = PyBillingResponse::new(&resolve);
        response
            .load_entity("Avatar", 7, "for-client", "for-base")
            .expect("the response fires");
        assert!(response.sent());
        assert_eq!(
            response.finish(),
            GetEntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: 1,
                    db_id: 7
                },
                data_for_client: "for-client".to_owned(),
                data_for_base_entity: "for-base".to_owned(),
            }
        );
    }

    /// The zero-id refusal fires before the sent check — a second call
    /// with id 0 reports the id, not the double response.
    #[test]
    fn load_entity_refuses_zero_before_anything_else() {
        let mut response = PyBillingResponse::new(&resolve);
        assert_eq!(
            response.load_entity("Avatar", 0, "", ""),
            Err("0 is an invalid database id".to_owned())
        );
        assert!(!response.sent());
        response.load_entity("Avatar", 7, "", "").unwrap();
        assert_eq!(
            response.load_entity("Avatar", 0, "", ""),
            Err("0 is an invalid database id".to_owned()),
            "the id check precedes the sent check"
        );
    }

    #[test]
    fn unknown_entity_types_raise_the_reference_text() {
        let mut response = PyBillingResponse::new(&resolve);
        assert_eq!(
            response.load_entity("Nope", 7, "", ""),
            Err("Invalid entity type 'Nope'\n".to_owned())
        );
        assert!(!response.sent());
        assert_eq!(
            response.create_new_entity("Also-nope", true, "", ""),
            Err("Invalid entity type 'Also-nope'\n".to_owned())
        );
    }

    #[test]
    fn a_second_response_raises_already_sent() {
        let mut response = PyBillingResponse::new(&resolve);
        response.load_entity("Avatar", 7, "", "").unwrap();
        assert_eq!(
            response.load_entity("Avatar", 8, "", ""),
            Err("Response already sent".to_owned())
        );
        assert_eq!(
            response.create_new_entity("Avatar", true, "", ""),
            Err("Response already sent".to_owned())
        );
        assert_eq!(
            response.load_entity_by_name("Avatar", "hero", false, false, "", ""),
            Err("Response already sent".to_owned())
        );
        assert_eq!(
            response.failure_invalid_password(""),
            Err("Response already sent".to_owned())
        );
        assert_eq!(
            response.failure(log_on_status::LOGIN_REJECTED_NO_SUCH_USER, ""),
            Err("Response already sent".to_owned())
        );
    }

    #[test]
    fn load_entity_by_name_carries_the_flags_and_data() {
        let mut response = PyBillingResponse::new(&resolve);
        response
            .load_entity_by_name("Avatar", "hero", true, true, "c", "b")
            .unwrap();
        assert_eq!(
            response.finish(),
            GetEntityKeyOutcome::LoadFromUsername {
                type_id: 1,
                username: "hero".to_owned(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: true,
                data_for_client: "c".to_owned(),
                data_for_base_entity: "b".to_owned(),
            }
        );
    }

    #[test]
    fn create_new_entity_carries_the_remember_flag() {
        let mut response = PyBillingResponse::new(&resolve);
        response.create_new_entity("Avatar", false, "", "").unwrap();
        assert_eq!(
            response.finish(),
            GetEntityKeyOutcome::CreateNew {
                type_id: 1,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    /// The three named failures carry the reference's statuses and the
    /// message the script passed.
    #[test]
    fn named_failures_carry_the_reference_statuses() {
        let mut response = PyBillingResponse::new(&resolve);
        response.failure_invalid_password("bad pw").unwrap();
        assert_eq!(
            response.finish(),
            GetEntityKeyOutcome::Failure {
                status: log_on_status::LOGIN_REJECTED_INVALID_PASSWORD,
                error_msg: "bad pw".to_owned(),
            }
        );

        let mut response = PyBillingResponse::new(&resolve);
        response.failure_no_such_user("no hero").unwrap();
        assert_eq!(
            response.finish(),
            GetEntityKeyOutcome::Failure {
                status: log_on_status::LOGIN_REJECTED_NO_SUCH_USER,
                error_msg: "no hero".to_owned(),
            }
        );

        let mut response = PyBillingResponse::new(&resolve);
        response.failure_db_error("db down").unwrap();
        assert_eq!(
            response.finish(),
            GetEntityKeyOutcome::Failure {
                status: log_on_status::LOGIN_REJECTED_DB_GENERAL_FAILURE,
                error_msg: "db down".to_owned(),
            }
        );
    }

    /// `failure(int)`: okay statuses are refused with the reference's
    /// text; any rejection status — named or not — rides through raw.
    #[test]
    fn failure_takes_any_non_okay_status_and_refuses_okay_ones() {
        let mut response = PyBillingResponse::new(&resolve);
        assert_eq!(
            response.failure(log_on_status::NOT_SET, ""),
            Err("Invalid failure status 0".to_owned())
        );
        assert_eq!(
            response.failure(log_on_status::LOGGED_ON, ""),
            Err("Invalid failure status 1".to_owned())
        );
        assert!(!response.sent());

        // A status this crate has no name for: LOGIN_REJECTED_RATE_LIMITED.
        response.failure(83, "slow down").unwrap();
        assert_eq!(
            response.finish(),
            GetEntityKeyOutcome::Failure {
                status: 83,
                error_msg: "slow down".to_owned(),
            }
        );
    }

    /// The driver: the get face present, the script answers — the
    /// response rides.
    #[test]
    fn the_driver_delivers_the_scripts_response() {
        let system = PyBillingSystem::new(true, true);
        let outcome = system.get_entity_key_for_account(
            "hero",
            "secret",
            &Address {
                ip: 0x0102_0304,
                port: 20_001,
                salt: 0,
            },
            &resolve,
            |_username, _password, _addr, response| response.load_entity("Avatar", 7, "", ""),
        );
        assert_eq!(
            outcome,
            GetEntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: 1,
                    db_id: 7
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    /// A raising script that never answers leaves the destructor's
    /// no-response failure; the wrapper prints and clears the traceback.
    #[test]
    fn a_raising_script_leaves_the_no_response_failure() {
        let system = PyBillingSystem::new(true, true);
        let outcome = system.get_entity_key_for_account(
            "hero",
            "secret",
            &Address {
                ip: 0,
                port: 0,
                salt: 0,
            },
            &resolve,
            |_username, _password, _addr, _response| Err("boom".to_owned()),
        );
        assert_eq!(
            outcome,
            GetEntityKeyOutcome::Failure {
                status: log_on_status::LOGIN_REJECTED_DB_GENERAL_FAILURE,
                error_msg: "No response from script".to_owned(),
            }
        );
    }

    /// A script that answered and *then* raised keeps its answer — the
    /// destructor only speaks when the handler is still held.
    #[test]
    fn an_answered_script_beats_a_late_raise() {
        let system = PyBillingSystem::new(true, true);
        let outcome = system.get_entity_key_for_account(
            "hero",
            "secret",
            &Address {
                ip: 0,
                port: 0,
                salt: 0,
            },
            &resolve,
            |_username, _password, _addr, response| {
                response.failure_no_such_user("gone")?;
                Err("late boom".to_owned())
            },
        );
        assert_eq!(
            outcome,
            GetEntityKeyOutcome::Failure {
                status: log_on_status::LOGIN_REJECTED_NO_SUCH_USER,
                error_msg: "gone".to_owned(),
            }
        );
    }

    /// The script bodies the faceless drivers must never reach. Named
    /// functions rather than closures: a never-called closure body is an
    /// uncovered line, while a function-item reference is not, and the
    /// guard tests below can call them directly. If a driver ever calls
    /// through, the test panics and fails.
    fn must_not_run() -> Result<(), String> {
        panic!("the script ran without its face")
    }

    fn never_get<'d>(
        _username: &str,
        _password: &str,
        _addr: &Address,
        _response: &mut PyBillingResponse<'d>,
    ) -> Result<(), String> {
        must_not_run()
    }

    fn never_set(
        _username: &str,
        _password: &str,
        _type_name: &str,
        _db_id: DatabaseId,
    ) -> Result<(), String> {
        must_not_run()
    }

    /// The type-name lookup the set face resolves with; shared so its
    /// body is covered by the leg that actually runs.
    fn avatar_name(type_id: EntityTypeId) -> String {
        (type_id == 1).then(|| "Avatar".to_owned()).unwrap()
    }

    #[test]
    #[should_panic(expected = "the script ran without its face")]
    fn the_get_guard_panics_when_reached() {
        let mut response = PyBillingResponse::new(&resolve);
        let _ = never_get(
            "u",
            "p",
            &Address {
                ip: 0,
                port: 0,
                salt: 0,
            },
            &mut response,
        );
    }

    #[test]
    #[should_panic(expected = "the script ran without its face")]
    fn the_set_guard_panics_when_reached() {
        let _ = never_set("u", "p", "Avatar", 7);
    }

    /// Without the get face the driver fails bare — the script never
    /// runs at all.
    #[test]
    fn a_missing_get_face_fails_without_calling_the_script() {
        let system = PyBillingSystem::new(false, true);
        assert!(!system.is_okay());
        let outcome = system.get_entity_key_for_account(
            "hero",
            "secret",
            &Address {
                ip: 0,
                port: 0,
                salt: 0,
            },
            &resolve,
            never_get,
        );
        assert_eq!(
            outcome,
            GetEntityKeyOutcome::Failure {
                status: log_on_status::LOGIN_REJECTED_DB_GENERAL_FAILURE,
                error_msg: String::new(),
            }
        );
    }

    /// The set face: present and raised → failure, present and clean →
    /// success, missing → failure without a call.
    #[test]
    fn the_set_face_rides_the_scripts_outcome() {
        let key = EntityKey {
            type_id: 1,
            db_id: 7,
        };

        let system = PyBillingSystem::new(true, true);
        assert!(system.is_okay());
        assert_eq!(
            system.set_entity_key_for_account(
                "hero",
                "secret",
                key,
                avatar_name,
                |username, password, type_name, db_id| {
                    assert_eq!(
                        (username, password, type_name, db_id),
                        ("hero", "secret", "Avatar", 7)
                    );
                    Ok(())
                },
            ),
            SetEntityKeyOutcome::Success(key)
        );
        assert_eq!(
            system.set_entity_key_for_account(
                "hero",
                "secret",
                key,
                |_| "Avatar".to_owned(),
                |_u, _p, _t, _d| Err("boom".to_owned()),
            ),
            SetEntityKeyOutcome::Failure(key)
        );

        let mute = PyBillingSystem::new(true, false);
        assert!(mute.is_okay(), "only the get face gates isOkay");
        assert_eq!(
            mute.set_entity_key_for_account("hero", "secret", key, avatar_name, never_set,),
            SetEntityKeyOutcome::Failure(key)
        );
    }

    /// The unresolved-name leg of `resolve_type` mirrors what the
    /// definitions hand back: the invalid type id never leaks through.
    #[test]
    fn an_invalid_type_id_is_reported_like_an_unknown_name() {
        let never: &dyn Fn(&str) -> Option<EntityTypeId> = &|_| Some(INVALID_ENTITY_TYPE_ID);
        let mut response = PyBillingResponse::new(never);
        assert_eq!(
            response.load_entity("Avatar", 7, "", ""),
            Err("Invalid entity type 'Avatar'\n".to_owned())
        );
    }
}
