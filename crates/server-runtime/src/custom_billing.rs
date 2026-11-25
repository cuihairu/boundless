//! BW-09-04-03 the compiled-in billing extensions and the creator registry
//! (`server/dbapp/custom_billing_system.cpp`,
//! `lib/db_storage/billing_system_creator.cpp`).
//!
//! Every billing backend registers a `BillingSystemCreator` at link time
//! (the reference's static initialisers): the registry is the set of
//! extensions the build compiled in, and `createFromConfig` picks one by
//! the `billingSystem/type` option (default `"standard"`). A created
//! system must pass `isOkay()`, or the reference deletes it again and the
//! DBApp starts without a billing system.
//!
//! The custom extension is the compiled-in stub integration: it never
//! authenticates the password — the credentials are handed to the base
//! entity (the `authenticateViaBaseEntity` behaviour) — and it always
//! offers to create the default entity type without remembering it
//! (`setEntityKeyForAccount` is not implemented). Its one dependency is
//! the `Account` entity type in the definitions: with it the extension is
//! usable, without it `isOkay()` is false and the creator rejects the
//! system ("Billing system is not configured correctly").
//!
//! The reference's `standard` creator (the Python hook of BW-09-04-02) and
//! `bwauth` creator are separate back ends; the registry here takes the
//! compiled-in list, so the acceptance runs per target.

use crate::billing::{EntityKey, EntityTypeID, INVALID_ENTITY_TYPE_ID};

/// `bwConfig: billingSystem/type`'s default (`billing_system_creator.cpp`).
pub const DEFAULT_BILLING_TYPE: &str = "standard";

/// The entity type the custom extension offers (`"Account"`).
pub const CUSTOM_ENTITY_TYPE: &str = "Account";

/// The one callback `getEntityKeyForAccount` makes. The registry's back
/// ends share the Python response's outcome vocabulary: the reference's
/// handler callbacks are exactly the ones `PyBillingResponse` forwards.
pub use crate::py_billing::PyEntityKeyOutcome as BackendOutcome;

/// The one callback `setEntityKeyForAccount` makes
/// (`ISetEntityKeyForAccountHandler`).
pub use crate::py_billing::PySetEntityKeyOutcome as SetBackendOutcome;

/// A billing back end (`BillingSystem`,
/// `db_storage/billing_system.hpp:87-125`).
pub trait BillingSystem {
    /// Turn login credentials into the entity key associated with them.
    fn get_entity_key_for_account(&mut self, username: &str, password: &str) -> BackendOutcome;

    /// Set the mapping between credentials and an entity.
    fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> SetBackendOutcome;

    /// `BillingSystem::isOkay`: configured well enough to run.
    fn is_okay(&self) -> bool;
}

/// A registered creator (`BillingSystemCreator`,
/// `billing_system_creator.hpp`). One per compiled-in extension.
pub trait BillingSystemCreator {
    /// `typeName`: the `billingSystem/type` value that selects this
    /// extension.
    fn type_name(&self) -> &'static str;

    /// `create`: build the back end from the entity definitions. The
    /// reference returns a (possibly broken) system; `isOkay()` is checked
    /// by the registry, not the creator.
    fn create(&self, defs: &crate::py_billing::EntityDefs) -> Option<Box<dyn BillingSystem>>;
}

/// The custom extension (`CustomBillingSystem`,
/// `custom_billing_system.cpp:70-115`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomBillingSystem {
    /// `entityTypeID_`: the `Account` type, resolved at construction.
    entity_type_id: EntityTypeID,
}

impl CustomBillingSystem {
    /// The constructor resolves the `Account` type once; an unknown type
    /// leaves `INVALID_ENTITY_TYPE_ID` and `isOkay()` false.
    pub fn new(defs: &crate::py_billing::EntityDefs) -> Self {
        Self {
            entity_type_id: defs
                .resolve(CUSTOM_ENTITY_TYPE)
                .unwrap_or(INVALID_ENTITY_TYPE_ID),
        }
    }
}

impl BillingSystem for CustomBillingSystem {
    /// `getEntityKeyForAccount`: no lookup, no password check — the base
    /// entity authenticates, and a fresh entity of the default type is
    /// offered, never remembered
    /// (`custom_billing_system.cpp:80-93`).
    fn get_entity_key_for_account(&mut self, _username: &str, password: &str) -> BackendOutcome {
        BackendOutcome::CreateNew {
            entity_type: self.entity_type_id,
            should_remember: false,
            data_for_client: String::new(),
            data_for_base_entity: password.to_string(),
        }
    }

    /// `setEntityKeyForAccount`: unimplemented in the reference — the write
    /// reports success and the caller keeps the entity
    /// (`custom_billing_system.cpp:95-106`).
    fn set_entity_key_for_account(
        &mut self,
        _username: &str,
        _password: &str,
        key: EntityKey,
    ) -> SetBackendOutcome {
        SetBackendOutcome::Success(key)
    }

    /// `isOkay()`: the `Account` dependency is present
    /// (`custom_billing_system.cpp:108-114`).
    fn is_okay(&self) -> bool {
        self.entity_type_id != INVALID_ENTITY_TYPE_ID
    }
}

/// The custom extension's creator (`CustomBillingSystemCreator`,
/// `custom_billing_system.cpp:21-39`).
#[derive(Debug, Clone, Copy, Default)]
pub struct CustomBillingSystemCreator;

impl BillingSystemCreator for CustomBillingSystemCreator {
    fn type_name(&self) -> &'static str {
        "custom"
    }

    fn create(&self, defs: &crate::py_billing::EntityDefs) -> Option<Box<dyn BillingSystem>> {
        Some(Box::new(CustomBillingSystem::new(defs)))
    }
}

/// The extensions this build compiled in (the reference's static
/// initialiser collection). The standard Python hook and the `bwauth`
/// back end are separate items; this build ships the custom extension.
pub fn builtin_billing_system_creators() -> Vec<Box<dyn BillingSystemCreator>> {
    vec![Box::new(CustomBillingSystemCreator)]
}

/// `BillingSystemCreator::createFromConfig`'s outcomes
/// (`billing_system_creator.cpp:38-79`).
pub enum CreatedBillingSystem {
    /// The named extension was created and `isOkay()` — the reference logs
    /// `"Using billing system: %s"`.
    Created(Box<dyn BillingSystem>),
    /// The created system failed `isOkay()` — the reference deletes it and
    /// logs `"Billing system is not configured correctly"`.
    NotConfigured,
    /// The creator produced no system (the reference returns null).
    CreatorFailed,
    /// No compiled-in extension matches `billingSystem/type`.
    NoSuchType,
    /// No extension is compiled in (`g_pBillingSystemCollection == NULL`).
    NoCreatorsRegistered,
}

/// `BillingSystemCreator::createFromConfig`
/// (`billing_system_creator.cpp:38-79`): pick the configured extension
/// from the compiled-in set and require `isOkay()`.
pub fn create_from_config(
    type_name: &str,
    defs: &crate::py_billing::EntityDefs,
    creators: &[Box<dyn BillingSystemCreator>],
) -> CreatedBillingSystem {
    if creators.is_empty() {
        // "No billing systems registered."
        return CreatedBillingSystem::NoCreatorsRegistered;
    }
    for creator in creators {
        if creator.type_name() != type_name {
            continue;
        }
        let Some(system) = creator.create(defs) else {
            return CreatedBillingSystem::CreatorFailed;
        };
        if !system.is_okay() {
            // "Billing system is not configured correctly".
            return CreatedBillingSystem::NotConfigured;
        }
        return CreatedBillingSystem::Created(system);
    }
    CreatedBillingSystem::NoSuchType
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::DatabaseID;
    use crate::loginapp::LogOnStatus;
    use crate::py_billing::{EntityDefs, PyEntityKeyOutcome};

    const ACCOUNT_TYPE: EntityTypeID = 3;
    const ACCOUNT_DBID: DatabaseID = 42;

    fn defs() -> EntityDefs {
        let mut defs = EntityDefs::new();
        defs.add_entity("Account", ACCOUNT_TYPE);
        defs
    }

    #[test]
    fn the_custom_extension_authenticates_via_the_base_entity() {
        let mut system = CustomBillingSystem::new(&defs());
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret"),
            BackendOutcome::CreateNew {
                entity_type: ACCOUNT_TYPE,
                should_remember: false,
                data_for_client: String::new(),
                // The password goes to the base entity unchecked.
                data_for_base_entity: "s3cret".to_string(),
            }
        );
    }

    #[test]
    fn the_custom_extension_set_reports_success_without_storing() {
        let mut system = CustomBillingSystem::new(&defs());
        let key = EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID);
        assert_eq!(
            system.set_entity_key_for_account("alice", "s3cret", key),
            SetBackendOutcome::Success(key)
        );
    }

    #[test]
    fn a_custom_extension_with_the_account_dependency_is_usable() {
        let creators = builtin_billing_system_creators();
        assert_eq!(creators[0].type_name(), "custom");
        let created = create_from_config("custom", &defs(), &creators);
        assert!(matches!(created, CreatedBillingSystem::Created(_)));
    }

    #[test]
    fn a_missing_account_dependency_rejects_the_extension() {
        // A build whose definitions carry no `Account` entity type.
        let mut defs = EntityDefs::new();
        defs.add_entity("Avatar", 7);
        let creators = builtin_billing_system_creators();
        assert!(matches!(
            create_from_config("custom", &defs, &creators),
            CreatedBillingSystem::NotConfigured
        ));
        // The system the creator built still answers with the invalid type
        // id; the registry is what keeps it out of the DBApp.
        let mut system = CustomBillingSystem::new(&defs);
        assert!(!system.is_okay());
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret"),
            BackendOutcome::CreateNew {
                entity_type: INVALID_ENTITY_TYPE_ID,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: "s3cret".to_string(),
            }
        );
    }

    #[test]
    fn the_config_default_selects_standard_and_misses_without_it() {
        let creators = builtin_billing_system_creators();
        // `bw.xml` without a billingSystem/type block asks for "standard".
        assert_eq!(DEFAULT_BILLING_TYPE, "standard");
        assert!(matches!(
            create_from_config(DEFAULT_BILLING_TYPE, &defs(), &creators),
            CreatedBillingSystem::NoSuchType
        ));
    }

    #[test]
    fn an_empty_registry_reports_no_creators() {
        assert!(matches!(
            create_from_config("custom", &defs(), &[]),
            CreatedBillingSystem::NoCreatorsRegistered
        ));
    }

    #[test]
    fn a_creator_that_builds_nothing_reports_the_failure() {
        struct BrokenCreator;
        impl BillingSystemCreator for BrokenCreator {
            fn type_name(&self) -> &'static str {
                "custom"
            }
            fn create(&self, _defs: &EntityDefs) -> Option<Box<dyn BillingSystem>> {
                None
            }
        }
        let creators: Vec<Box<dyn BillingSystemCreator>> = vec![Box::new(BrokenCreator)];
        assert!(matches!(
            create_from_config("custom", &defs(), &creators),
            CreatedBillingSystem::CreatorFailed
        ));
    }

    #[test]
    fn the_python_hook_wires_into_the_registry_outcome_vocabulary() {
        // The reference's `standard` creator wraps the script object in
        // `PyBillingSystem`; its outcomes share the handler callbacks the
        // custom extension reports.
        use crate::py_billing::{BillingScript, PyBillingSystem, PyGetEntityKeyFn};
        let script = BillingScript {
            get_entity_key_for_account: Some(PyGetEntityKeyFn::new(Box::new(
                |_u, _p, _a, response| response.load_entity("Account", ACCOUNT_DBID, "", ""),
            ))),
            ..Default::default()
        };
        let mut system = PyBillingSystem::new(script, defs());
        assert_eq!(
            system.get_entity_key_for_account("alice", "s3cret", (0x7f00_0001, 20000)),
            PyEntityKeyOutcome::Success {
                key: EntityKey::new(ACCOUNT_TYPE, ACCOUNT_DBID),
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
        let _ = LogOnStatus::LoginRejectedDbGeneralFailure;
    }
}
