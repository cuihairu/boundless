//! The stub custom billing system
//! (`server/dbapp/custom_billing_system.{hpp,cpp}`).
//!
//! The reference's custom system is deliberately minimal: it never
//! authenticates anything, always hands back a create-new outcome for the
//! `Account` entity type with the password passed through as the base
//! entity's data (the standard system's `authenticateViaBaseEntity`
//! behaviour), and remembers nothing because its `setEntityKeyForAccount`
//! is a no-op success. `isOkay` is the one real check: the entity
//! definitions must carry an `Account` type.
//!
//! Layering: domain library (layer 2).

use entitydef::entity_map::EntityDescriptionMap;
use foundation::ids::{EntityTypeId, INVALID_ENTITY_TYPE_ID};
use mercury::endpoint::Address;

use crate::billing::{BillingSystem, EntityKeyOutcome, SetEntityKeyOutcome};
use crate::storage::EntityKey;

/// The custom billing system (`CustomBillingSystem`).
pub struct CustomBillingSystem {
    /// `entityTypeID_`: the `Account` type resolved at construction,
    /// `INVALID_ENTITY_TYPE_ID` when the defs carry no such type.
    entity_type_id: EntityTypeId,
}

impl CustomBillingSystem {
    /// Constructor: resolve `Account` once
    /// (`CustomBillingSystem::CustomBillingSystem`).
    pub fn new(defs: &EntityDescriptionMap) -> Self {
        Self {
            entity_type_id: defs
                .name_to_index("Account")
                .unwrap_or(INVALID_ENTITY_TYPE_ID),
        }
    }

    /// The resolved `Account` type id.
    pub fn entity_type_id(&self) -> EntityTypeId {
        self.entity_type_id
    }
}

impl BillingSystem for CustomBillingSystem {
    /// `getEntityKeyForAccount`: no authentication — a create-new for the
    /// `Account` type, not remembered (the setter stores nothing), with
    /// the password handed to the base entity to deal with.
    fn get_entity_key_for_account(
        &mut self,
        _username: &str,
        password: &str,
        _client_addr: Address,
    ) -> EntityKeyOutcome {
        EntityKeyOutcome::CreateNew {
            entity_type: self.entity_type_id,
            should_remember: false,
            data_for_client: String::new(),
            data_for_base_entity: password.to_string(),
        }
    }

    /// `setEntityKeyForAccount`: nothing to store — the mapping is
    /// reported successful either way.
    fn set_entity_key_for_account(
        &mut self,
        _username: &str,
        _password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome {
        SetEntityKeyOutcome::Success(key)
    }

    /// `isOkay`: the defs must carry an `Account` type.
    fn is_okay(&self) -> bool {
        self.entity_type_id != INVALID_ENTITY_TYPE_ID
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::xml_database::tests::defs;
    use entitydef::data_type::TypeContext;
    use entitydef::diagnostics::Diagnostics;
    use entitydef::entity_description::DefResolver;
    use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};
    use resources::datasection::{DataSection, parse};
    use std::collections::HashMap;

    const RES: &str = "scripts/entities.xml";
    const DEFS: &str = "scripts/entity_defs";

    /// A resolver serving an in-memory res tree.
    struct MemoryResolver {
        files: HashMap<String, Vec<u8>>,
    }

    impl MemoryResolver {
        fn new(files: &[(&str, &str)]) -> Self {
            MemoryResolver {
                files: files
                    .iter()
                    .map(|(path, xml)| (path.to_string(), xml.as_bytes().to_vec()))
                    .collect(),
            }
        }
    }

    impl DefResolver for MemoryResolver {
        fn open_section(&self, path: &str) -> Option<DataSection> {
            parse("", self.files.get(path)?).ok()
        }

        fn python_script_exists(&self, path: &str) -> bool {
            self.files.contains_key(path) || self.files.contains_key(&format!("{path}/__init__.py"))
        }
    }

    /// A defs set carrying an `Account` type — the one type the custom
    /// system resolves at construction.
    pub(crate) fn account_defs() -> EntityDescriptionMap {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Account/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (&format!("{DEFS}/Account.def"), "<root/>"),
        ]);

        let section = resolver.open_section(RES).expect("entities.xml");
        let mut map = EntityDescriptionMap::default();
        assert!(
            map.parse(
                Some(&section),
                &resolver,
                &TypeContext::default(),
                &mut Diagnostics::new(),
                &MessageRanges::default(),
            ),
            "defs parse failed"
        );
        map
    }

    /// With an `Account` type present: every logon becomes a create-new
    /// for that type, not remembered, with the password as the base
    /// entity's data and nothing for the client.
    #[test]
    fn a_logon_creates_a_new_account_entity() {
        let defs_map = account_defs();
        let account = defs_map.name_to_index("Account").unwrap();
        let mut system = CustomBillingSystem::new(&defs_map);
        assert_eq!(system.entity_type_id(), account);
        assert!(system.is_okay());

        assert_eq!(
            system.get_entity_key_for_account("anyone", "their-password", Address::new(0, 0)),
            EntityKeyOutcome::CreateNew {
                entity_type: account,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: "their-password".to_string(),
            }
        );
    }

    /// The setter stores nothing and reports success for any key — even
    /// one naming a type the defs do not carry.
    #[test]
    fn set_entity_key_always_succeeds() {
        let defs_map = account_defs();
        let mut system = CustomBillingSystem::new(&defs_map);

        let key = EntityKey {
            type_id: 7,
            db_id: 12,
        };
        assert_eq!(
            system.set_entity_key_for_account("anyone", "pw", key),
            SetEntityKeyOutcome::Success(key)
        );
    }

    /// Without an `Account` type the system is misconfigured — but the
    /// reference's getter carries no gate, so logons still come back as
    /// create-new with the invalid type id.
    #[test]
    fn without_an_account_type_the_system_is_misconfigured() {
        let defs_map = defs();
        let mut system = CustomBillingSystem::new(&defs_map);

        assert_eq!(system.entity_type_id(), INVALID_ENTITY_TYPE_ID);
        assert!(!system.is_okay());

        assert_eq!(
            system.get_entity_key_for_account("anyone", "pw", Address::new(0, 0)),
            EntityKeyOutcome::CreateNew {
                entity_type: INVALID_ENTITY_TYPE_ID,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_string(),
            }
        );
    }
}
