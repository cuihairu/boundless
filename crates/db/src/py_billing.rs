//! The Python-script-driven billing system
//! (`server/dbapp/py_billing_system.{hpp,cpp}`).
//!
//! The standard creator hands the DBApp a Python object with two members,
//! `getEntityKeyForAccount` and `setEntityKeyForAccount`. For a logon the
//! driver calls the first with `(username, password, clientAddr, response)`
//! where `response` is a [`PyBillingResponse`]: the script authenticates by
//! its own means and reports through exactly one of the response's seven
//! methods. A response that never reports — the script raised, or returned
//! without calling anything — reports the general failure in its place,
//! which is the reference's destructor contract.
//!
//! The port keeps the reference's error contract verbatim: every refused
//! method returns the `ValueError` text the reference sets (the port has
//! no live interpreter to raise on, so the message is the observable),
//! and "exactly one response" is enforced by the same "Response already
//! sent" guard the C++ enforces by nulling the handler pointer.
//!
//! Layering: domain library (layer 2). The script members are injected
//! closures — the CPython bridge lives in python-host, and the contract
//! under test here is the one the script sees.

use entitydef::entity_map::EntityDescriptionMap;
use foundation::ids::{DatabaseId, EntityTypeId, INVALID_ENTITY_TYPE_ID};
use mercury::endpoint::Address;

use crate::billing::{BillingSystem, EntityKeyOutcome, SetEntityKeyOutcome};
use crate::log_on_status::{LogOnStatus, codes};
use crate::storage::EntityKey;

/// The script-facing response object
/// (`PyBillingResponse`, `py_billing_system.cpp:31-218`).
///
/// One instance per logon attempt. Every method maps to the reference's
/// `PY_AUTO_METHOD` of the same name, with the same defaults and the same
/// refusal text.
pub struct PyBillingResponse<'a> {
    defs: &'a EntityDescriptionMap,
    /// The one outcome the handler receives; `None` until a method sends.
    outcome: Option<EntityKeyOutcome>,
}

impl<'a> PyBillingResponse<'a> {
    /// Constructor: nothing has been sent yet.
    pub fn new(defs: &'a EntityDescriptionMap) -> Self {
        Self {
            defs,
            outcome: None,
        }
    }

    /// True while no method has reported yet — the reference's non-null
    /// `pHandler_`.
    pub fn has_response(&self) -> bool {
        self.outcome.is_some()
    }

    /// `loadEntity`: a successful authentication naming one entity by
    /// database id.
    pub fn load_entity(
        &mut self,
        entity_type: &str,
        db_id: DatabaseId,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        // The id check runs before the sent guard, exactly as the reference
        // orders it (`py_billing_system.cpp:219-226`).
        if db_id == 0 {
            return Err("0 is an invalid database id".to_string());
        }
        let type_id = self.type_id_for(entity_type)?;
        self.outcome = Some(EntityKeyOutcome::Success {
            key: EntityKey { type_id, db_id },
            data_for_client: data_for_client.to_string(),
            data_for_base_entity: data_for_base_entity.to_string(),
        });
        Ok(())
    }

    /// `loadEntityByName`: request an entity by its Identifier property;
    /// the form used when a base entity does the authentication.
    pub fn load_entity_by_name(
        &mut self,
        entity_type: &str,
        entity_name: &str,
        should_create_unknown: bool,
        should_remember_unknown: bool,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        let type_id = self.type_id_for(entity_type)?;
        self.outcome = Some(EntityKeyOutcome::LoadFromUsername {
            entity_type: type_id,
            username: entity_name.to_string(),
            should_create_new_on_load_failure: should_create_unknown,
            should_remember_new_on_load_failure: should_remember_unknown,
            data_for_client: data_for_client.to_string(),
            data_for_base_entity: data_for_base_entity.to_string(),
        });
        Ok(())
    }

    /// `createNewEntity`: authentication succeeded and a brand new entity
    /// should be created.
    pub fn create_new_entity(
        &mut self,
        entity_type: &str,
        should_remember: bool,
        data_for_client: &str,
        data_for_base_entity: &str,
    ) -> Result<(), String> {
        let type_id = self.type_id_for(entity_type)?;
        self.outcome = Some(EntityKeyOutcome::CreateNew {
            entity_type: type_id,
            should_remember,
            data_for_client: data_for_client.to_string(),
            data_for_base_entity: data_for_base_entity.to_string(),
        });
        Ok(())
    }

    /// `failureInvalidPassword`.
    pub fn failure_invalid_password(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(
            LogOnStatus::new(codes::LOGIN_REJECTED_INVALID_PASSWORD),
            error_msg,
        )
    }

    /// `failureNoSuchUser`.
    pub fn failure_no_such_user(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(
            LogOnStatus::new(codes::LOGIN_REJECTED_NO_SUCH_USER),
            error_msg,
        )
    }

    /// `failureDBError`.
    pub fn failure_db_error(&mut self, error_msg: &str) -> Result<(), String> {
        self.send_failure(
            LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
            error_msg,
        )
    }

    /// `failure`: an arbitrary error code. `okay()` codes are refused —
    /// the reference's `status.okay() || statusAsInt != status.value()`
    /// check, where the round-trip half is dead because the enum cast is
    /// the identity.
    pub fn failure(&mut self, status_as_int: i32, error_msg: &str) -> Result<(), String> {
        let status = LogOnStatus::new(status_as_int);
        if status.okay() || status_as_int != status.value() {
            return Err(format!("Invalid failure status {}", status_as_int));
        }
        self.send_failure(status, error_msg)
    }

    /// `getEntityTypeID`: the shared sent-guard and the name lookup
    /// (`py_billing_system.cpp:284-301`).
    fn type_id_for(&self, entity_type: &str) -> Result<EntityTypeId, String> {
        if self.outcome.is_some() {
            return Err("Response already sent".to_string());
        }
        self.defs
            .name_to_index(entity_type)
            .filter(|id| *id != INVALID_ENTITY_TYPE_ID)
            .ok_or_else(|| format!("Invalid entity type '{}'\n", entity_type))
    }

    /// `sendFailure`: the one failure path, guarded like the reference.
    fn send_failure(&mut self, status: LogOnStatus, error_msg: &str) -> Result<(), String> {
        if self.outcome.is_some() {
            return Err("Response already sent".to_string());
        }
        self.outcome = Some(EntityKeyOutcome::Failure {
            status,
            error_msg: error_msg.to_string(),
        });
        Ok(())
    }

    /// The destructor (`~PyBillingResponse`, `py_billing_system.cpp:40-61`):
    /// a response that never sent anything reports the general failure with
    /// "No response from script", so every logon attempt ends at the handler
    /// exactly once.
    pub fn into_outcome(mut self) -> EntityKeyOutcome {
        self.outcome.take().unwrap_or(EntityKeyOutcome::Failure {
            status: LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
            error_msg: "No response from script".to_string(),
        })
    }
}

/// The script member `getEntityKeyForAccount`: the credentials, the
/// client address and the response object to report through. An `Err`
/// is the script raising — the driver logs it and the response's own
/// contract decides the outcome.
pub type GetEntityKeyFn<'a> =
    Box<dyn FnMut(&str, &str, Address, &mut PyBillingResponse) -> Result<(), String> + 'a>;

/// The script member `setEntityKeyForAccount`: `(username, password,
/// entityType, dbID)`. An `Err` is the script raising
/// (`PyBillingSystem::setEntityKeyForAccount`, `py_billing_system.cpp:418-457`).
pub type SetEntityKeyFn<'a> =
    Box<dyn FnMut(&str, &str, &str, DatabaseId) -> Result<(), String> + 'a>;

/// The members `getMember` reads off the script object
/// (`py_billing_system.cpp:341-357`). A `None` member is the attribute
/// the script object does not carry: `getMember` logs and clears, so
/// construction continues and the failure surfaces at call time.
pub struct PyBillingScript<'a> {
    /// The `getEntityKeyForAccount` attribute.
    pub get_entity_key_for_account: Option<GetEntityKeyFn<'a>>,
    /// The `setEntityKeyForAccount` attribute.
    pub set_entity_key_for_account: Option<SetEntityKeyFn<'a>>,
}

/// The Python billing system
/// (`PyBillingSystem`, `py_billing_system.cpp:344-457`).
pub struct PyBillingSystem<'a> {
    defs: &'a EntityDescriptionMap,
    get_func: Option<GetEntityKeyFn<'a>>,
    set_func: Option<SetEntityKeyFn<'a>>,
    /// The `ERROR_MSG` lines the reference would have printed, in order.
    pub errors: Vec<String>,
}

impl<'a> PyBillingSystem<'a> {
    /// Constructor: read the two members, logging the missing ones the way
    /// `getMember` does (`No such method %s`).
    pub fn new(script: PyBillingScript<'a>, defs: &'a EntityDescriptionMap) -> Self {
        let mut errors = Vec::new();
        if script.get_entity_key_for_account.is_none() {
            errors.push(
                "PyBillingSystem::getMember: No such method getEntityKeyForAccount\n".to_string(),
            );
        }
        if script.set_entity_key_for_account.is_none() {
            errors.push(
                "PyBillingSystem::getMember: No such method setEntityKeyForAccount\n".to_string(),
            );
        }
        Self {
            defs,
            get_func: script.get_entity_key_for_account,
            set_func: script.set_entity_key_for_account,
            errors,
        }
    }

    /// `getEntityKeyForAccount`: run the script member against a fresh
    /// response, then deliver whatever the response ended up carrying —
    /// including the destructor's general failure when the script raised
    /// or returned without reporting.
    pub fn get_entity_key_for_account_with_client_addr(
        &mut self,
        username: &str,
        password: &str,
        client_addr: Address,
    ) -> EntityKeyOutcome {
        let Some(func) = self.get_func.as_mut() else {
            self.errors.push(
                "PyBillingSystem::getEntityKeyForAccount: No getEntityKeyForAccount method\n"
                    .to_string(),
            );
            return EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
                error_msg: String::new(),
            };
        };

        let mut response = PyBillingResponse::new(self.defs);
        if func(username, password, client_addr, &mut response).is_err() {
            // `ERROR_MSG ... Failed. username = %s` then `PyErr_PrintEx` —
            // the failure is logged, and the response's destructor sends
            // the outcome.
            self.errors.push(format!(
                "PyBillingSystem::getEntityKeyForAccount: Failed. username = {}\n",
                username
            ));
        }
        response.into_outcome()
    }

    /// `setEntityKeyForAccount`: call the script member with the entity's
    /// type name and database id; a script failure fails the mapping.
    pub fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome {
        let Some(func) = self.set_func.as_mut() else {
            self.errors.push(
                "PyBillingSystem::setEntityKeyForAccount: No setEntityKeyForAccount function \
                 exists. sendCreateNew should not be called with shouldRemember as True\n"
                    .to_string(),
            );
            return SetEntityKeyOutcome::Failure(key);
        };

        // `entityDefs_.getEntityDescription( ekey.typeID ).name()`; an id
        // outside the set has no description to name, and the port passes
        // the empty string instead of reading one out of bounds.
        let type_name = self.defs.index_to_name(key.type_id).unwrap_or("");
        match func(username, password, type_name, key.db_id) {
            Ok(()) => SetEntityKeyOutcome::Success(key),
            Err(_) => {
                self.errors.push(format!(
                    "PyBillingSystem::setLoginMapping: Failed. username = {}\n",
                    username
                ));
                SetEntityKeyOutcome::Failure(key)
            }
        }
    }
}

impl BillingSystem for PyBillingSystem<'_> {
    /// The DBAppInterface::authenticateAccount path always passes
    /// `Mercury::Address( 0, 0 )` as the client address
    /// (`authenticate_account_handler.cpp:47`).
    fn get_entity_key_for_account(
        &mut self,
        logon_name: &str,
        password: &str,
        client_addr: Address,
    ) -> EntityKeyOutcome {
        self.get_entity_key_for_account_with_client_addr(logon_name, password, client_addr)
    }

    fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome {
        PyBillingSystem::set_entity_key_for_account(self, username, password, key)
    }

    fn is_okay(&self) -> bool {
        // The Python system inherits the base predicate
        // (`billing_system.hpp:116-121`); `PyBillingSystem` adds no
        // configuration of its own.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_database::tests::defs;

    fn addr() -> Address {
        Address::new(0, 0)
    }

    /// `loadEntity` with a real type and a non-zero id reports the key
    /// with the optional data strings.
    #[test]
    fn load_entity_sends_the_key_and_the_data() {
        let avatar = defs().name_to_index("Avatar").unwrap();
        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);

        response
            .load_entity("Avatar", 7, "for the client", "for the entity")
            .expect("loadEntity");

        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: avatar,
                    db_id: 7
                },
                data_for_client: "for the client".to_string(),
                data_for_base_entity: "for the entity".to_string(),
            }
        );
    }

    /// The defaults: omitted data strings are empty, and the type id comes
    /// from the definition set.
    #[test]
    fn load_entity_keeps_the_default_data_strings() {
        let avatar = defs().name_to_index("Avatar").unwrap();
        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);

        response
            .load_entity("Avatar", 3, "", "")
            .expect("loadEntity");

        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: avatar,
                    db_id: 3
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    /// The reference's two refusals, in its own order: the zero id first,
    /// the unknown type second — each with its verbatim `ValueError` text.
    #[test]
    fn load_entity_refuses_the_zero_id_and_the_unknown_type() {
        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);

        assert_eq!(
            response.load_entity("Avatar", 0, "", "").unwrap_err(),
            "0 is an invalid database id"
        );
        assert!(!response.has_response(), "nothing was sent");
        assert_eq!(
            response.load_entity("Nobody", 4, "", "").unwrap_err(),
            "Invalid entity type 'Nobody'\n"
        );
        assert!(!response.has_response());
    }

    #[test]
    fn load_entity_by_name_carries_every_argument() {
        let avatar = defs().name_to_index("Avatar").unwrap();
        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);

        response
            .load_entity_by_name("Avatar", "moe", true, false, "c", "b")
            .expect("loadEntityByName");

        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::LoadFromUsername {
                entity_type: avatar,
                username: "moe".to_string(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
                data_for_client: "c".to_string(),
                data_for_base_entity: "b".to_string(),
            }
        );
    }

    #[test]
    fn create_new_entity_carries_the_remember_flag() {
        let avatar = defs().name_to_index("Avatar").unwrap();
        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);

        response
            .create_new_entity("Avatar", true, "", "")
            .expect("createNewEntity");

        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::CreateNew {
                entity_type: avatar,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    /// The three named failures map to their `LogOnStatus` codes.
    #[test]
    fn the_named_failures_carry_their_status_codes() {
        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);
        response.failure_invalid_password("bad password").unwrap();
        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_INVALID_PASSWORD),
                error_msg: "bad password".to_string(),
            }
        );

        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);
        response.failure_no_such_user("").unwrap();
        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_NO_SUCH_USER),
                error_msg: String::new(),
            }
        );

        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);
        response.failure_db_error("db broke").unwrap();
        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
                error_msg: "db broke".to_string(),
            }
        );
    }

    /// `failure` passes any non-`okay` code through and refuses the `okay`
    /// ones with the reference's text; an unknown type name is refused
    /// before anything else for the load forms.
    #[test]
    fn failure_rejects_only_the_okay_codes() {
        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);

        assert_eq!(
            response.failure(codes::NOT_SET, "").unwrap_err(),
            "Invalid failure status 0"
        );
        assert_eq!(
            response.failure(codes::LOGGED_ON, "").unwrap_err(),
            "Invalid failure status 1"
        );
        assert!(!response.has_response());

        response
            .failure(codes::LOGIN_REJECTED_BAN, "banned")
            .unwrap();
        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_BAN),
                error_msg: "banned".to_string(),
            }
        );
    }

    /// Exactly one response: the second reporting method hits the
    /// "Response already sent" guard.
    #[test]
    fn a_second_report_is_refused_with_the_reference_text() {
        let defs_map = defs();
        let mut response = PyBillingResponse::new(&defs_map);
        response.failure_invalid_password("first").unwrap();

        assert_eq!(
            response.failure_no_such_user("second").unwrap_err(),
            "Response already sent"
        );
        assert_eq!(
            response.load_entity("Avatar", 1, "", "").unwrap_err(),
            "Response already sent"
        );
        assert_eq!(
            response.failure_db_error("").unwrap_err(),
            "Response already sent"
        );

        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_INVALID_PASSWORD),
                error_msg: "first".to_string(),
            }
        );
    }

    /// The destructor contract: a response that never reported sends the
    /// general failure with the reference's message.
    #[test]
    fn an_unanswered_response_sends_the_general_failure() {
        let defs_map = defs();
        let response = PyBillingResponse::new(&defs_map);
        assert!(!response.has_response());

        assert_eq!(
            response.into_outcome(),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
                error_msg: "No response from script".to_string(),
            }
        );
    }

    /// A script that answers normally drives the outcome through; the
    /// driver adds no error lines.
    #[test]
    fn a_reporting_script_passes_its_outcome_through() {
        let defs_map = defs();
        let mut system = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: Some(Box::new(|_, _, _, response| {
                    response.load_entity("Avatar", 5, "", "")
                })),
                set_entity_key_for_account: Some(Box::new(|_, _, _, _| Ok(()))),
            },
            &defs_map,
        );
        assert!(system.errors.is_empty());

        let avatar = defs().name_to_index("Avatar").unwrap();
        assert_eq!(
            system.get_entity_key_for_account_with_client_addr("moe", "pw", addr()),
            EntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: avatar,
                    db_id: 5
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
        assert!(system.errors.is_empty());
    }

    /// A script that returns without reporting ends at the destructor's
    /// general failure — the reference's `~PyBillingResponse` path.
    #[test]
    fn a_silent_script_ends_at_the_general_failure() {
        let defs_map = defs();
        let mut system = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: Some(Box::new(|_, _, _, _| Ok(()))),
                set_entity_key_for_account: Some(Box::new(|_, _, _, _| Ok(()))),
            },
            &defs_map,
        );

        assert_eq!(
            system.get_entity_key_for_account_with_client_addr("moe", "pw", addr()),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
                error_msg: "No response from script".to_string(),
            }
        );
        assert!(system.errors.is_empty());
    }

    /// A script that raises is logged with the reference's message and
    /// ends the same way. A script that reports *and then* raises keeps
    /// its report — the handler was already nulled.
    #[test]
    fn a_raising_script_is_logged_and_defaults() {
        let defs_map = defs();
        let mut system = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: Some(Box::new(|_, _, _, _| Err("boom".to_string()))),
                set_entity_key_for_account: Some(Box::new(|_, _, _, _| Ok(()))),
            },
            &defs_map,
        );

        assert_eq!(
            system.get_entity_key_for_account_with_client_addr("curly", "pw", addr()),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
                error_msg: "No response from script".to_string(),
            }
        );
        assert_eq!(
            system.errors,
            vec!["PyBillingSystem::getEntityKeyForAccount: Failed. username = curly\n".to_string()]
        );
    }

    /// A missing `getEntityKeyForAccount` member is logged at construction
    /// (`getMember`) and fails the logon immediately at call time.
    #[test]
    fn a_missing_get_member_fails_the_logon_at_call_time() {
        let defs_map = defs();
        let mut system = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: None,
                set_entity_key_for_account: None,
            },
            &defs_map,
        );
        assert_eq!(
            system.errors,
            vec![
                "PyBillingSystem::getMember: No such method getEntityKeyForAccount\n".to_string(),
                "PyBillingSystem::getMember: No such method setEntityKeyForAccount\n".to_string(),
            ]
        );

        let before = system.errors.len();
        assert_eq!(
            system.get_entity_key_for_account_with_client_addr("moe", "pw", addr()),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_DB_GENERAL_FAILURE),
                error_msg: String::new(),
            }
        );
        assert_eq!(
            system.errors[before],
            "PyBillingSystem::getEntityKeyForAccount: No getEntityKeyForAccount method\n"
                .to_string()
        );
    }

    /// The setter: success passes the type name and the database id;
    /// a missing member or a script failure fails the mapping, with the
    /// reference's log lines.
    #[test]
    fn set_entity_key_dispatches_to_the_script_member() {
        let avatar = defs().name_to_index("Avatar").unwrap();
        let key = EntityKey {
            type_id: avatar,
            db_id: 9,
        };

        let mut seen = None;
        {
            let defs_map = defs();
            let mut system = PyBillingSystem::new(
                PyBillingScript {
                    get_entity_key_for_account: None,
                    set_entity_key_for_account: Some(Box::new(|username, password, ty, db_id| {
                        seen = Some((
                            username.to_string(),
                            password.to_string(),
                            ty.to_string(),
                            db_id,
                        ));
                        Ok(())
                    })),
                },
                &defs_map,
            );
            assert_eq!(
                system.set_entity_key_for_account("moe", "pw", key),
                SetEntityKeyOutcome::Success(key)
            );
        }
        assert_eq!(
            seen,
            Some(("moe".to_string(), "pw".to_string(), "Avatar".to_string(), 9))
        );

        let defs_map = defs();
        let mut failing = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: None,
                set_entity_key_for_account: Some(Box::new(|_, _, _, _| Err("raise".to_string()))),
            },
            &defs_map,
        );
        assert_eq!(
            failing.set_entity_key_for_account("larry", "pw", key),
            SetEntityKeyOutcome::Failure(key)
        );
        assert_eq!(
            failing.errors,
            vec![
                "PyBillingSystem::getMember: No such method getEntityKeyForAccount\n".to_string(),
                "PyBillingSystem::setLoginMapping: Failed. username = larry\n".to_string(),
            ]
        );

        let defs_map = defs();
        let mut missing = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: None,
                set_entity_key_for_account: None,
            },
            &defs_map,
        );
        assert_eq!(
            missing.set_entity_key_for_account("moe", "pw", key),
            SetEntityKeyOutcome::Failure(key)
        );
        assert_eq!(
            missing.errors[2],
            "PyBillingSystem::setEntityKeyForAccount: No setEntityKeyForAccount function exists. \
             sendCreateNew should not be called with shouldRemember as True\n"
                .to_string()
        );
    }

    /// Through the shared `BillingSystem` trait the Python system behaves
    /// like any other billing face, with the handler's zero client
    /// address.
    #[test]
    fn the_trait_face_dispatches_with_the_zero_address() {
        let defs_map = defs();
        let mut system = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: Some(Box::new(|_, _, client_addr, response| {
                    assert_eq!(client_addr, Address::new(0, 0));
                    response.failure_no_such_user("who?")
                })),
                set_entity_key_for_account: None,
            },
            &defs_map,
        );

        assert_eq!(
            BillingSystem::get_entity_key_for_account(&mut system, "moe", "pw", addr()),
            EntityKeyOutcome::Failure {
                status: LogOnStatus::new(codes::LOGIN_REJECTED_NO_SUCH_USER),
                error_msg: "who?".to_string(),
            }
        );

        // The trait face delegates the setter to the script member and
        // inherits the base `isOkay` predicate.
        let avatar = defs().name_to_index("Avatar").unwrap();
        let key = EntityKey {
            type_id: avatar,
            db_id: 4,
        };
        let mut system = PyBillingSystem::new(
            PyBillingScript {
                get_entity_key_for_account: None,
                set_entity_key_for_account: Some(Box::new(|_, _, ty, db_id| {
                    assert_eq!(ty, "Avatar");
                    assert_eq!(db_id, 4);
                    Ok(())
                })),
            },
            &defs_map,
        );
        assert_eq!(
            BillingSystem::set_entity_key_for_account(&mut system, "moe", "pw", key),
            SetEntityKeyOutcome::Success(key)
        );
        assert!(BillingSystem::is_okay(&system));
    }
}
