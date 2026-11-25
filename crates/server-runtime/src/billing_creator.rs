//! BW-09-04-03 the billing system registry and the compiled-in custom
//! extension.
//!
//! The DBApp picks its billing system from `bw.xml`'s `billingSystem/type`
//! through a registry of creators (`lib/db_storage/billing_system_creator.
//! {hpp,cpp}`): every compiled extension contributes one creator by
//! constructing a global `BillingSystemCreator` subclass — the
//! `IntrusiveObject` base splices it into a link-time collection
//! (`billing_system_creator.cpp:31-33`) — and `createFromConfig` walks the
//! collection for the configured type name (`billing_system_creator.cpp:
//! 39-79`). The shipped build always links the standard creator
//! (`server/dbapp/standard_billing_system.cpp:43`) and optionally the custom
//! one, gated by `useCustomBillingSystem` (default `0`,
//! `server/dbapp/Makefile.rules:9,17-19`).
//!
//! The custom extension itself (`server/dbapp/custom_billing_system.
//! {hpp,cpp}`) resolves the `Account` entity type at construction
//! (`custom_billing_system.cpp:51-55`) and delegates authentication to the
//! base entity: `getEntityKeyForAccount` answers `CreateNew` with
//! `shouldRemember=false` and the password riding as `dataForBaseEntity`
//! (`custom_billing_system.cpp:70-87` — the TODO comment defers the real
//! lookup), `setEntityKeyForAccount` is a stub that echoes the key back as
//! success (`custom_billing_system.cpp:94-102`), and `isOkay` fails when the
//! defs have no `Account` type (`custom_billing_system.cpp:108-111`).
//!
//! The standard creator (`standard_billing_system.cpp:47-82`) first asks the
//! personality module for `connectToBillingSystem`: a script error fails the
//! creation ("Failed. See script errors"), a `None` answer falls through, and
//! a system object is wrapped as the script billing bridge
//! (`PyBillingSystem`, ported separately). Without one — or when the
//! personality module is absent — the storage backend's own billing system is
//! used (`getIDatabase().createBillingSystem()`, line 78); this port's
//! storage stand-in is the in-memory logon map (`crate::billing`).
//!
//! Rust has no link-time static initialisation, so the creators register
//! explicitly: `register_standard_creator` / `register_custom_creator` play
//! the two staticInitialisers, and `register_creator` is the extension point
//! a compiled-in extension would drive from its own initialisation.

use std::sync::Mutex;

use crate::billing::{
    AccountResolution, BillingConfig, EntityKey, EntityTypeID, INVALID_ENTITY_TYPE_ID,
    LogonMapBillingSystem,
};

/// The billing system interface
/// (`db_storage/billing_system.hpp:87-125`): the two account operations the
/// DBApp drives and the configuration check `createFromConfig` enforces.
pub trait BillingSystem {
    /// `getEntityKeyForAccount` — the outcome is exactly one handler callback
    /// (`AccountResolution`).
    fn get_entity_key_for_account(
        &mut self,
        log_on_name: &str,
        password: &str,
    ) -> AccountResolution;

    /// `setEntityKeyForAccount` — the outcome is exactly one handler callback
    /// (`ISetEntityKeyForAccountHandler`, `billing_system.hpp:71-85`).
    fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome;

    /// `BillingSystem::isOkay` (`billing_system.hpp:116-121`).
    fn is_okay(&self) -> bool;
}

/// The one callback `setEntityKeyForAccount` makes
/// (`ISetEntityKeyForAccountHandler`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetEntityKeyOutcome {
    /// `onSetEntityKeyForAccountSuccess`.
    Success(EntityKey),
    /// `onSetEntityKeyForAccountFailure`.
    Failure(EntityKey),
}

/// The entity-type-name resolver standing in for
/// `EntityDefs::getEntityType` (`db_entitydefs.hpp`): the unknown name
/// reports [`INVALID_ENTITY_TYPE_ID`].
pub type EntityTypeResolver<'a> = &'a (dyn Fn(&str) -> EntityTypeID + 'a);

/// The creator interface every billing system contributes
/// (`db_storage/billing_system_creator.hpp:21-41`).
pub trait BillingSystemCreator: Send {
    /// `typeName()` — the `billingSystem/type` string this creator answers
    /// to (`billing_system_creator.cpp:54`).
    fn type_name(&self) -> &'static str;

    /// `create(entityDefs, app)` — the port passes the `bw.xml`
    /// `billingSystem/` block and the entity-type resolver the reference
    /// creators read from `EntityDefs` themselves.
    fn create(
        &self,
        config: &BillingConfig,
        entity_type_of: EntityTypeResolver<'_>,
    ) -> Option<Box<dyn BillingSystem>>;
}

/// `g_pBillingSystemCollection` (`billing_system_creator.cpp:19`): the
/// creators a compiled extension spliced in at link time. The reference
/// starts the pointer `NULL` and allocates it on the first registration;
/// an empty vector here is that same "nothing linked in" state.
static CREATORS: Mutex<Vec<Box<dyn BillingSystemCreator + Send>>> = Mutex::new(Vec::new());

/// The personality module's `connectToBillingSystem` hook
/// (`standard_billing_system.cpp:54-58`): the compiled script extension the
/// standard creator consults first.
pub trait PersonalityHook: Send {
    /// Resolve the attribute and call it (`getAttribute` + `callFunction`,
    /// `standard_billing_system.cpp:56-62`). `Err` is the script-error path
    /// (`ScriptErrorPrint`); `Ok(None)` covers both the absent attribute
    /// (`ScriptObject()` default) and the attribute answering `None`
    /// (`billingSystem.isNone()`, line 70).
    fn connect_to_billing_system(&mut self) -> Result<Option<Box<dyn BillingSystem>>, String>;
}

/// The personality module (`Personality::instance()`,
/// `lib/pyscript/personality.cpp`), set by the script host at startup.
static PERSONALITY: Mutex<Option<Box<dyn PersonalityHook + Send>>> = Mutex::new(None);

/// Link a billing system extension into the registry — the port of the
/// `IntrusiveObject` static-initialiser splice
/// (`billing_system_creator.cpp:31-33`).
pub fn register_creator(creator: Box<dyn BillingSystemCreator + Send>) {
    CREATORS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(creator);
}

/// Register the standard creator — `StandardBillingSystemCreator
/// staticInitialiser` (`standard_billing_system.cpp:43`), linked into every
/// dbapp build. Idempotent.
pub fn register_standard_creator() {
    register_once(StandardBillingSystemCreator);
}

/// Register the custom creator — `CustomBillingSystemCreator
/// staticInitialiser` (`custom_billing_system.cpp:39`), linked only when
/// `useCustomBillingSystem = 1` (`Makefile.rules:17-19`). A build that
/// leaves the gate off has no `custom` creator, which is the "dependency
/// missing" shape of the acceptance. Idempotent.
pub fn register_custom_creator() {
    register_once(CustomBillingSystemCreator);
}

/// Push a creator unless its type name is already registered (the
/// staticInitialisers run once per link; the explicit ports are called at
/// startup and stay idempotent).
fn register_once<C: BillingSystemCreator + Send + 'static>(creator: C) {
    let mut creators = CREATORS.lock().unwrap_or_else(|p| p.into_inner());
    if creators
        .iter()
        .any(|c| c.type_name() == creator.type_name())
    {
        return;
    }
    creators.push(Box::new(creator));
}

/// Install (or clear) the personality hook the standard creator consults.
pub fn set_personality_hook(hook: Option<Box<dyn PersonalityHook + Send>>) {
    *PERSONALITY.lock().unwrap_or_else(|p| p.into_inner()) = hook;
}

/// Create the billing system named by `billingSystem/type`
/// (`BillingSystemCreator::createFromConfig`,
/// `billing_system_creator.cpp:39-79`).
///
/// An unknown type name — the shipped default build asked for `custom` with
/// the gate off, say — returns `None` with no message: the reference walk
/// simply falls off the end of the collection (line 78).
pub fn create_from_config(
    type_name: &str,
    config: &BillingConfig,
    entity_type_of: EntityTypeResolver<'_>,
) -> Option<Box<dyn BillingSystem>> {
    let creators = CREATORS.lock().unwrap_or_else(|p| p.into_inner());
    create_from_creators(type_name, config, entity_type_of, &creators)
}

/// The `createFromConfig` walk over an explicit creator list (the static
/// collection, taken by reference so every branch stays testable).
fn create_from_creators(
    type_name: &str,
    config: &BillingConfig,
    entity_type_of: EntityTypeResolver<'_>,
    creators: &[Box<dyn BillingSystemCreator + Send>],
) -> Option<Box<dyn BillingSystem>> {
    if creators.is_empty() {
        eprintln!("BillingSystemCreator::createFromConfig: No billing systems registered.");
        return None;
    }

    for creator in creators {
        if creator.type_name() != type_name {
            continue;
        }

        let billing_system = creator.create(config, entity_type_of)?;
        if !billing_system.is_okay() {
            eprintln!(
                "BillingSystemCreator::createFromConfig: Billing system is not configured correctly"
            );
            return None;
        }
        eprintln!("Using billing system: {type_name}");
        return Some(billing_system);
    }

    None
}

/// The standard creator (`server/dbapp/standard_billing_system.cpp`): the
/// personality extension when one answers, else the storage backend's own
/// billing system.
struct StandardBillingSystemCreator;

impl BillingSystemCreator for StandardBillingSystemCreator {
    fn type_name(&self) -> &'static str {
        "standard"
    }

    fn create(
        &self,
        config: &BillingConfig,
        entity_type_of: EntityTypeResolver<'_>,
    ) -> Option<Box<dyn BillingSystem>> {
        let mut billing_system: Option<Box<dyn BillingSystem>> = None;

        // `Personality::instance().getAttribute( "connectToBillingSystem",
        // ScriptErrorClear() )` then `func.callFunction( ScriptErrorPrint() )`
        // (standard_billing_system.cpp:52-68).
        let mut personality = PERSONALITY.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(hook) = personality.as_mut() {
            match hook.connect_to_billing_system() {
                Ok(Some(system)) => billing_system = Some(system),
                Ok(None) => {}
                Err(_) => {
                    eprintln!("StandardBillingSystemCreator::create: Failed. See script errors");
                    return None;
                }
            }
        }
        drop(personality);

        if billing_system.is_none() {
            // `DBApp::instance().getIDatabase().createBillingSystem()`
            // (line 78): the storage backend's own billing system. This
            // port's stand-in is the in-memory logon map.
            billing_system = Some(Box::new(LogonMapBillingSystem::new(config.clone())));
        }

        let _ = entity_type_of;
        billing_system
    }
}

/// The compiled-in custom extension (`server/dbapp/custom_billing_system.
/// {hpp,cpp}`): a stub that accepts every login through the base entity.
pub struct CustomBillingSystem {
    /// `entityTypeID_` — the `Account` entity type resolved at construction
    /// (`custom_billing_system.cpp:53`).
    entity_type_id: EntityTypeID,
}

impl CustomBillingSystem {
    /// `CustomBillingSystem( entityDefs )` (`custom_billing_system.cpp:51`).
    pub fn new(entity_type_of: impl Fn(&str) -> EntityTypeID) -> Self {
        Self {
            entity_type_id: entity_type_of("Account"),
        }
    }
}

impl BillingSystem for CustomBillingSystem {
    fn get_entity_key_for_account(
        &mut self,
        _log_on_name: &str,
        password: &str,
    ) -> AccountResolution {
        // The stub accepts every login and creates the default entity type,
        // not remembered (setEntityKeyForAccount is not implemented), with
        // the unauthenticated password handed to the base entity — the
        // authenticateViaBaseEntity equivalence
        // (custom_billing_system.cpp:75-86).
        AccountResolution::CreateNew {
            entity_type: self.entity_type_id,
            should_remember: false,
            data_for_client: String::new(),
            data_for_base_entity: password.to_string(),
        }
    }

    fn set_entity_key_for_account(
        &mut self,
        _username: &str,
        _password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome {
        // TODO in the reference too: nothing is stored; the key echoes back
        // as success (custom_billing_system.cpp:94-102).
        SetEntityKeyOutcome::Success(key)
    }

    fn is_okay(&self) -> bool {
        // The extension needs the Account entity type in the defs
        // (custom_billing_system.cpp:108-111).
        self.entity_type_id != INVALID_ENTITY_TYPE_ID
    }
}

/// `CustomBillingSystemCreator` (`custom_billing_system.cpp:21-34`): type
/// name `custom`, constructs the extension.
pub struct CustomBillingSystemCreator;

impl BillingSystemCreator for CustomBillingSystemCreator {
    fn type_name(&self) -> &'static str {
        "custom"
    }

    fn create(
        &self,
        _config: &BillingConfig,
        entity_type_of: EntityTypeResolver<'_>,
    ) -> Option<Box<dyn BillingSystem>> {
        Some(Box::new(CustomBillingSystem::new(entity_type_of)))
    }
}

/// The storage-backed billing system (`crate::billing`'s logon map) as a
/// registry member: the `getIDatabase().createBillingSystem()` fallback of
/// the standard creator.
impl BillingSystem for LogonMapBillingSystem {
    fn get_entity_key_for_account(
        &mut self,
        log_on_name: &str,
        password: &str,
    ) -> AccountResolution {
        LogonMapBillingSystem::get_entity_key_for_account(self, log_on_name, password)
    }

    fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome {
        // The XML billing system always reports success
        // (xml_billing_system.cpp setEntityKeyForAccount).
        self.add_mapping(username, key, password);
        SetEntityKeyOutcome::Success(key)
    }

    fn is_okay(&self) -> bool {
        LogonMapBillingSystem::is_okay(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Account` entity type the custom extension resolves, as the defs
    /// would report it; any other name is unknown.
    fn account_defs(name: &str) -> EntityTypeID {
        if name == "Account" {
            7
        } else {
            INVALID_ENTITY_TYPE_ID
        }
    }

    /// The defs without an `Account` type — the custom extension's
    /// `isOkay` failure.
    fn defs_without_account(_name: &str) -> EntityTypeID {
        INVALID_ENTITY_TYPE_ID
    }

    /// Clear the link-time collection and the personality module, so each
    /// test starts from the build shape it installs itself. The returned
    /// guard serialises the tests against each other: the reference
    /// process holds exactly one collection, so the port's global registry
    /// needs the same exclusion.
    fn reset() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        CREATORS.lock().unwrap_or_else(|p| p.into_inner()).clear();
        *PERSONALITY.lock().unwrap_or_else(|p| p.into_inner()) = None;
        guard
    }

    /// The exclusion the registry-sharing tests hold for their whole body.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// A creator whose `create` always returns a not-okay system — the
    /// "Billing system is not configured correctly" shape.
    struct BrokenCreator;

    impl BillingSystemCreator for BrokenCreator {
        fn type_name(&self) -> &'static str {
            "broken"
        }

        fn create(
            &self,
            _config: &BillingConfig,
            _entity_type_of: EntityTypeResolver<'_>,
        ) -> Option<Box<dyn BillingSystem>> {
            // The configuration matrix of `isOkay`: unknown users accepted
            // through the base entity with no entity type to create.
            let mut config = BillingConfig::new();
            config.should_accept_unknown_users = true;
            config.authenticate_via_base_entity = true;
            Some(Box::new(LogonMapBillingSystem::new(config)))
        }
    }

    /// A personality extension: its system answers `CreateNew` with the
    /// scripted entity type.
    struct ScriptedHook(EntityTypeID);

    impl PersonalityHook for ScriptedHook {
        fn connect_to_billing_system(&mut self) -> Result<Option<Box<dyn BillingSystem>>, String> {
            Ok(Some(Box::new(CustomBillingSystem::new(|_| self.0))))
        }
    }

    /// A personality extension whose `connectToBillingSystem` raises.
    struct RaisingHook;

    impl PersonalityHook for RaisingHook {
        fn connect_to_billing_system(&mut self) -> Result<Option<Box<dyn BillingSystem>>, String> {
            Err("boom".to_string())
        }
    }

    /// A personality extension whose attribute answers `None`.
    struct NoneHook;

    impl PersonalityHook for NoneHook {
        fn connect_to_billing_system(&mut self) -> Result<Option<Box<dyn BillingSystem>>, String> {
            Ok(None)
        }
    }

    #[test]
    fn the_custom_extension_answers_when_compiled_in() {
        let _lock = reset();
        register_custom_creator();

        let mut system = create_from_config("custom", &BillingConfig::new(), &account_defs)
            .expect("the compiled-in custom creator answers");

        assert!(system.is_okay());

        // The defs resolver reports any other name as unknown, which is the
        // isOkay failure of the extension.
        assert_eq!(account_defs("Avatar"), INVALID_ENTITY_TYPE_ID);

        // getEntityKeyForAccount: create the Account type, nothing
        // remembered, password handed to the base entity
        // (custom_billing_system.cpp:84-86).
        assert_eq!(
            system.get_entity_key_for_account("bob", "pw"),
            AccountResolution::CreateNew {
                entity_type: 7,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_string(),
            }
        );

        // setEntityKeyForAccount echoes the key back as success
        // (custom_billing_system.cpp:101).
        let key = EntityKey::new(7, 42);
        assert_eq!(
            system.set_entity_key_for_account("bob", "pw", key),
            SetEntityKeyOutcome::Success(key)
        );
    }

    #[test]
    fn the_custom_extension_is_missing_when_not_compiled_in() {
        let _lock = reset();
        // The shipped build shape: `useCustomBillingSystem = 0` links only
        // the standard creator (Makefile.rules:9,17-19).
        register_standard_creator();

        assert!(
            create_from_config("custom", &BillingConfig::new(), &account_defs).is_none(),
            "no compiled-in creator answers to 'custom'"
        );
    }

    #[test]
    fn a_missing_account_entity_type_refuses_the_custom_extension() {
        let _lock = reset();
        register_custom_creator();

        // The defs have no Account type: isOkay fails and createFromConfig
        // deletes the system (billing_system_creator.cpp:59-65).
        assert!(
            create_from_config("custom", &BillingConfig::new(), &defs_without_account).is_none()
        );
    }

    #[test]
    fn the_standard_extension_falls_back_to_the_storage_map() {
        let _lock = reset();
        register_standard_creator();

        let mut system = create_from_config("standard", &BillingConfig::new(), &account_defs)
            .expect("the linked-in standard creator answers");

        // The storage fallback answers through the logon map.
        let key = EntityKey::new(3, 9);
        assert_eq!(
            system.set_entity_key_for_account("alice", "secret", key),
            SetEntityKeyOutcome::Success(key)
        );
        assert_eq!(
            system.get_entity_key_for_account("alice", "secret"),
            AccountResolution::Success {
                entity_key: key,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
        assert_eq!(
            system.get_entity_key_for_account("alice", "wrong"),
            AccountResolution::Failure {
                status: crate::loginapp::LogOnStatus::LoginRejectedInvalidPassword,
                error_msg: String::new(),
            }
        );
    }

    #[test]
    fn a_personality_extension_replaces_the_storage_fallback() {
        let _lock = reset();
        register_standard_creator();
        set_personality_hook(Some(Box::new(ScriptedHook(5))));

        let mut system = create_from_config("standard", &BillingConfig::new(), &account_defs)
            .expect("the scripted extension answers");

        // The scripted system, not the logon map: every login creates the
        // scripted type with the password handed to the base entity.
        assert_eq!(
            system.get_entity_key_for_account("bob", "pw"),
            AccountResolution::CreateNew {
                entity_type: 5,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_string(),
            }
        );
    }

    #[test]
    fn a_raising_personality_extension_fails_the_standard_creator() {
        let _lock = reset();
        register_standard_creator();
        set_personality_hook(Some(Box::new(RaisingHook)));

        assert!(
            create_from_config("standard", &BillingConfig::new(), &account_defs).is_none(),
            "the script error fails the creation (standard_billing_system.cpp:63-68)"
        );
    }

    #[test]
    fn a_none_personality_extension_falls_through_to_storage() {
        let _lock = reset();
        register_standard_creator();
        set_personality_hook(Some(Box::new(NoneHook)));

        // `billingSystem.isNone()` skips the wrap and the storage map answers
        // (standard_billing_system.cpp:70-79).
        assert!(create_from_config("standard", &BillingConfig::new(), &account_defs).is_some());
    }

    #[test]
    fn an_empty_registry_warns_and_refuses() {
        // The reference's NULL collection: nothing linked in at all
        // (billing_system_creator.cpp:44-49).
        let empty: Vec<Box<dyn BillingSystemCreator + Send>> = Vec::new();
        assert!(
            create_from_creators("standard", &BillingConfig::new(), &account_defs, &empty)
                .is_none()
        );
    }

    #[test]
    fn an_unknown_type_in_a_linked_registry_refuses_silently() {
        // Two compiled-in creators, none answering to the configured name:
        // the walk skips both and falls off the end of the collection
        // (billing_system_creator.cpp:78) without a message.
        let creators: Vec<Box<dyn BillingSystemCreator + Send>> = vec![
            Box::new(CustomBillingSystemCreator),
            Box::new(StandardBillingSystemCreator),
        ];
        assert!(
            create_from_creators("absent", &BillingConfig::new(), &account_defs, &creators)
                .is_none()
        );
    }

    #[test]
    fn an_extension_links_its_own_creator() {
        let _lock = reset();
        // `register_creator` is the port of the `IntrusiveObject` splice an
        // extension drives from its own initialisation
        // (billing_system_creator.cpp:31-33).
        register_creator(Box::new(BrokenCreator));
        assert_eq!(registered_type_names(), ["broken"]);
        assert!(
            create_from_config("broken", &BillingConfig::new(), &account_defs).is_none(),
            "the extension's not-okay system is refused by the walk"
        );
    }

    #[test]
    fn a_not_okay_system_is_deleted_by_the_walk() {
        let creators: Vec<Box<dyn BillingSystemCreator + Send>> = vec![Box::new(BrokenCreator)];
        assert!(
            create_from_creators("broken", &BillingConfig::new(), &account_defs, &creators)
                .is_none()
        );
    }

    #[test]
    fn the_standard_creator_registers_once() {
        let _lock = reset();
        register_standard_creator();
        register_standard_creator();
        assert_eq!(registered_type_names(), ["standard"]);
    }

    #[test]
    fn the_custom_creator_registers_once() {
        let _lock = reset();
        register_custom_creator();
        register_custom_creator();
        assert_eq!(registered_type_names(), ["custom"]);
    }

    /// The link-time collection, by type name, for the registration tests.
    fn registered_type_names() -> Vec<String> {
        CREATORS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|c| c.type_name().to_string())
            .collect()
    }
}
