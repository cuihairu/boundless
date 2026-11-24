//! The billing system creator registry and the compiled-in custom
//! extension (`db_storage/billing_system_creator.{hpp,cpp}` +
//! `dbapp/custom_billing_system.{hpp,cpp}`).
//!
//! The reference picks its billing system from `bw.xml`:
//! `BWConfig::get( "billingSystem/type", "standard" )` scans the creators
//! that statically registered with `BillingSystemCreator` — the built-in
//! set is `standard`, `custom`, and `bwauth`. This module ports the
//! registry contract and the `custom` creator; `standard` lives in
//! [`crate::billing`] and its script wrapper in
//! [`crate::py_billing`], and `bwauth` (an HTTP auth-service client with
//! its own task machinery) is not in this lane's scope.
//!
//! The gate the registry applies is the acceptance: a creator that
//! answers to the configured target is run, and the system it builds is
//! kept only if `isOkay()` holds — the reference deletes a failed system
//! with "Billing system is not configured correctly" and starts without
//! one. The custom system's dependency is the `Account` entity type, so
//! a definition set without it is exactly the missing-dependency case.

use foundation::ids::{EntityTypeId, INVALID_ENTITY_TYPE_ID};

use crate::billing::{BillingConfig, GetEntityKeyOutcome, SetEntityKeyOutcome};
use crate::py_billing::PyBillingSystem;
use crate::storage::EntityKey;

/// `CustomBillingSystem`'s hardcoded type name
/// (`custom_billing_system.cpp:53`).
pub const ACCOUNT_TYPE_NAME: &str = "Account";

/// The `billingSystem/type` default (`billing_system_creator.cpp:52`).
pub const DEFAULT_BILLING_TYPE: &str = "standard";

/// `CustomBillingSystem` (`custom_billing_system.cpp:51-111`): the
/// compiled-in extension — every login creates an `Account` entity and
/// the password is left to the entity to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomBillingSystem {
    /// `entityTypeID_`: the resolved `Account` type.
    entity_type_id: EntityTypeId,
}

impl CustomBillingSystem {
    /// The constructor: `Account` resolved through the definitions; an
    /// unresolvable name leaves the invalid type id.
    pub fn new(resolve: impl FnOnce(&str) -> Option<EntityTypeId>) -> Self {
        Self {
            entity_type_id: resolve(ACCOUNT_TYPE_NAME).unwrap_or(INVALID_ENTITY_TYPE_ID),
        }
    }

    /// The resolved `Account` type id.
    pub fn entity_type_id(&self) -> EntityTypeId {
        self.entity_type_id
    }

    /// `isOkay` (`custom_billing_system.cpp:108-111`): the `Account` type
    /// must exist.
    pub fn is_okay(&self) -> bool {
        self.entity_type_id != INVALID_ENTITY_TYPE_ID
    }

    /// `getEntityKeyForAccount` (`custom_billing_system.cpp:70-87`): the
    /// reference's fallback stub — create the default type, do not
    /// remember it (`setEntityKeyForAccount` is not implemented by this
    /// system), and since the password was never checked, hand it to the
    /// base entity. That is the `authenticateViaBaseEntity` shape of the
    /// standard system.
    pub fn get_entity_key_for_account(&self, password: &str) -> GetEntityKeyOutcome {
        GetEntityKeyOutcome::CreateNew {
            type_id: self.entity_type_id,
            should_remember: false,
            data_for_client: String::new(),
            data_for_base_entity: password.to_owned(),
        }
    }

    /// `setEntityKeyForAccount` (`custom_billing_system.cpp:94-102`):
    /// always succeeds — the stub has nothing to store.
    pub fn set_entity_key_for_account(&self, key: EntityKey) -> SetEntityKeyOutcome {
        SetEntityKeyOutcome::Success(key)
    }
}

/// The systems the registered creators hand back — the polymorphic
/// `BillingSystem *` as the port sees it. The get/set faces stay on the
/// concrete kinds (their inputs differ: the standard system wants the
/// account lookup, the script system wants the script call), so the
/// registry only answers `isOkay` across kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreatedBillingSystem {
    /// The per-database standard system: its configuration face
    /// ([`crate::billing`]).
    Standard(BillingConfig),
    /// A script object wrapped as a billing system
    /// ([`crate::py_billing`]).
    Py(PyBillingSystem),
    /// The compiled-in custom extension.
    Custom(CustomBillingSystem),
}

impl CreatedBillingSystem {
    /// `BillingSystem::isOkay` per kind (`billing_system.hpp:110-114` and
    /// each subclass's override).
    pub fn is_okay(&self) -> bool {
        match self {
            Self::Standard(config) => config.is_okay(),
            Self::Py(system) => system.is_okay(),
            Self::Custom(system) => system.is_okay(),
        }
    }
}

/// Why [`create_from_config`] refused. The reference tells these apart
/// in its log stream and dies on all of them; the port surfaces them as
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreationFailure {
    /// No creator answers to the configured type (`billing_system_creator.cpp:78-82`
    /// — falling off the scan returns NULL).
    NoTypeRegistered,
    /// The creator's own construction failed (`:62-68` — the standard
    /// creator's script leg returning NULL).
    ConstructionFailed,
    /// The system built but `isOkay()` refused; the reference deletes it
    /// with "Billing system is not configured correctly" (`:69-76`).
    NotConfiguredCorrectly,
}

/// One registered creator (`billing_system_creator.hpp:28-41`): the type
/// name it answers to and its constructor, which closes over whatever the
/// reference's static initialisers close over — the personality script,
/// the config flags, the definitions.
pub type RegisteredCreator<'a> = (&'a str, &'a dyn Fn() -> Option<CreatedBillingSystem>);

/// `BillingSystemCreator::createFromConfig`
/// (`billing_system_creator.cpp:49-88`): scan for the configured type,
/// construct, and keep only systems that pass `isOkay`.
pub fn create_from_config(
    creators: &[RegisteredCreator],
    config_type: &str,
) -> Result<CreatedBillingSystem, CreationFailure> {
    let Some((_name, create)) = creators.iter().find(|(name, _)| *name == config_type) else {
        return Err(CreationFailure::NoTypeRegistered);
    };
    let Some(system) = create() else {
        return Err(CreationFailure::ConstructionFailed);
    };
    if !system.is_okay() {
        return Err(CreationFailure::NotConfiguredCorrectly);
    }
    Ok(system)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The definitions face with `Account` present or not.
    fn defs_with_account(name: &str) -> Option<EntityTypeId> {
        (name == ACCOUNT_TYPE_NAME).then_some(2)
    }

    /// The definitions face without `Account` — the missing dependency.
    fn defs_without_account(name: &str) -> Option<EntityTypeId> {
        (name == "Avatar").then_some(1)
    }

    #[test]
    fn the_default_target_is_standard() {
        assert_eq!(DEFAULT_BILLING_TYPE, "standard");
        assert_eq!(ACCOUNT_TYPE_NAME, "Account");
    }

    /// The custom target, Account present: the registry hands the
    /// system back and its faces work — create-not-remember with the
    /// password riding to the base entity.
    #[test]
    fn the_custom_extension_is_usable_when_account_exists() {
        let create_custom = || -> Option<CreatedBillingSystem> {
            Some(CreatedBillingSystem::Custom(CustomBillingSystem::new(
                defs_with_account,
            )))
        };
        let creators: Vec<RegisteredCreator> = vec![("custom", &create_custom)];
        let system = create_from_config(&creators, "custom").expect("Account exists");
        let CreatedBillingSystem::Custom(custom) = system else {
            panic!("the custom creator built the custom system");
        };
        assert!(custom.is_okay());
        assert_eq!(custom.entity_type_id(), 2);
        assert_eq!(
            custom.get_entity_key_for_account("pw"),
            GetEntityKeyOutcome::CreateNew {
                type_id: 2,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_owned(),
            }
        );
        assert_eq!(
            custom.set_entity_key_for_account(EntityKey {
                type_id: 2,
                db_id: 7
            }),
            SetEntityKeyOutcome::Success(EntityKey {
                type_id: 2,
                db_id: 7
            })
        );
    }

    /// The missing-dependency case: no `Account` type in the
    /// definitions — the system builds, the isOkay gate deletes it.
    #[test]
    fn the_custom_extension_without_account_is_refused() {
        let create_custom = || -> Option<CreatedBillingSystem> {
            Some(CreatedBillingSystem::Custom(CustomBillingSystem::new(
                defs_without_account,
            )))
        };
        let creators: Vec<RegisteredCreator> = vec![("custom", &create_custom)];
        assert_eq!(
            create_from_config(&creators, "custom"),
            Err(CreationFailure::NotConfiguredCorrectly)
        );
    }

    /// A configured type no creator answers to.
    #[test]
    fn an_unregistered_target_is_refused() {
        let creators: Vec<RegisteredCreator> = vec![];
        assert_eq!(
            create_from_config(&creators, "custom"),
            Err(CreationFailure::NoTypeRegistered)
        );
    }

    /// A creator whose own construction fails — the standard creator's
    /// script leg.
    #[test]
    fn a_failing_construction_is_refused() {
        let create_standard = || -> Option<CreatedBillingSystem> { None };
        let creators: Vec<RegisteredCreator> = vec![("standard", &create_standard)];
        assert_eq!(
            create_from_config(&creators, "standard"),
            Err(CreationFailure::ConstructionFailed)
        );
    }

    /// The isOkay gate holds across kinds: a script system without the
    /// get face is refused, one with it passes; a standard config that
    /// needs an unknown-user type it lacks is refused too.
    #[test]
    fn the_gate_applies_to_every_kind() {
        let faceless = PyBillingSystem::new(false, true);
        let working = PyBillingSystem::new(true, true);
        let blind_config = BillingConfig::from_name(true, true, false, "Nope", |_| None);
        let create_py_bad =
            || -> Option<CreatedBillingSystem> { Some(CreatedBillingSystem::Py(faceless.clone())) };
        let create_py_good =
            || -> Option<CreatedBillingSystem> { Some(CreatedBillingSystem::Py(working.clone())) };
        let create_standard_bad = || -> Option<CreatedBillingSystem> {
            Some(CreatedBillingSystem::Standard(blind_config.clone()))
        };
        let creators: Vec<RegisteredCreator> = vec![
            ("py-bad", &create_py_bad),
            ("py-good", &create_py_good),
            ("standard-bad", &create_standard_bad),
        ];
        assert_eq!(
            create_from_config(&creators, "py-bad"),
            Err(CreationFailure::NotConfiguredCorrectly)
        );
        assert!(create_from_config(&creators, "py-good").is_ok());
        assert_eq!(
            create_from_config(&creators, "standard-bad"),
            Err(CreationFailure::NotConfiguredCorrectly)
        );
    }

    /// The custom face without Account still hands out its stub — it
    /// does not gate itself; `is_okay` plus the registry gate are what
    /// stop the invalid type from reaching the DBApp.
    #[test]
    fn the_custom_face_does_not_gate_itself() {
        let blind = CustomBillingSystem::new(|_| None);
        assert!(!blind.is_okay());
        assert_eq!(blind.entity_type_id(), INVALID_ENTITY_TYPE_ID);
        assert_eq!(
            blind.get_entity_key_for_account("pw"),
            GetEntityKeyOutcome::CreateNew {
                type_id: INVALID_ENTITY_TYPE_ID,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_owned(),
            }
        );
    }
}
