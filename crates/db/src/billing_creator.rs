//! The billing system registry
//! (`lib/db_storage/billing_system_creator.{hpp,cpp}`).
//!
//! The DBApp builds its billing face from one bw.xml option,
//! `billingSystem/type` (default `"standard"`): a static set of creators
//! registers itself by type name, `createFromConfig` picks the match,
//! builds it, and applies the `isOkay` gate — a misconfigured system is
//! destroyed again with an ERROR line, an unknown type fails silently.
//!
//! The reference's registered names are `standard` (dbapp),
//! `custom` (dbapp) and `bwauth` (dbapp; an external auth-server client,
//! not yet ported). `standard` in the reference first asks the
//! Personality script's `connectToBillingSystem` and falls back to the
//! database's own face — the script branch produces the BW-09-04-02
//! [`PyBillingSystem`] and lives in the host layer, so the registry here
//! carries the fallback face, which for this crate's XML database is
//! [`XmlBillingSystem`].
//!
//! Layering: domain library (layer 2).

use entitydef::entity_map::EntityDescriptionMap;
use mercury::endpoint::Address;
use resources::datasection::DataSection;

use crate::billing::{
    BillingConfig, BillingSystem, EntityKeyOutcome, SetEntityKeyOutcome, XmlBillingSystem,
};
use crate::custom_billing::CustomBillingSystem;
use crate::storage::EntityKey;
use crate::xml_database::XmlDatabase;

/// The billing systems the registry can build
/// (`BillingSystem *` as created by the registered creators).
#[allow(clippy::large_enum_variant)] // both variants carry a full system; boxing would add indirection to every dispatch
pub enum RegisteredBillingSystem {
    /// The database's own billing face — the
    /// `getIDatabase().createBillingSystem()` fallback of the standard
    /// creator; for this crate's XML database that is the
    /// logOnMapping-driven system.
    Standard(XmlBillingSystem),
    /// The stub custom system (`CustomBillingSystemCreator`).
    Custom(CustomBillingSystem),
}

impl BillingSystem for RegisteredBillingSystem {
    fn get_entity_key_for_account(
        &mut self,
        logon_name: &str,
        password: &str,
        client_addr: Address,
    ) -> EntityKeyOutcome {
        match self {
            Self::Standard(system) => {
                system.get_entity_key_for_account(logon_name, password, client_addr)
            }
            Self::Custom(system) => {
                system.get_entity_key_for_account(logon_name, password, client_addr)
            }
        }
    }

    fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome {
        match self {
            Self::Standard(system) => system.set_entity_key_for_account(username, password, key),
            Self::Custom(system) => system.set_entity_key_for_account(username, password, key),
        }
    }

    fn is_okay(&self) -> bool {
        match self {
            Self::Standard(system) => system.is_okay(),
            Self::Custom(system) => system.is_okay(),
        }
    }
}

/// The outcome of `BillingSystemCreator::createFromConfig`.
#[allow(clippy::large_enum_variant)] // Ready carries the built system by value, as the reference returns the live pointer
pub enum BillingCreation {
    /// The config's type matched a registered creator and the built
    /// system passed `isOkay`.
    Ready {
        type_name: String,
        system: RegisteredBillingSystem,
    },
    /// The type matched but the built system failed `isOkay` — the
    /// reference deletes it again with an ERROR line and returns NULL.
    Misconfigured { type_name: String },
    /// No registered creator names the config's type — the reference
    /// returns NULL without a word.
    Unknown { type_name: String },
}

impl BillingCreation {
    /// The created system with its type name, unless the creation failed.
    pub fn into_system(self) -> Option<(String, RegisteredBillingSystem)> {
        match self {
            Self::Ready { type_name, system } => Some((type_name, system)),
            Self::Misconfigured { .. } | Self::Unknown { .. } => None,
        }
    }

    /// The one debug line `createFromConfig` prints for this outcome;
    /// `None` is the silent-NULL case.
    pub fn log_line(&self) -> Option<String> {
        match self {
            Self::Ready { type_name, .. } => Some(format!("Using billing system: {type_name}\n")),
            Self::Misconfigured { .. } => Some(
                "BillingSystemCreator::createFromConfig: Billing system is not configured \
                 correctly\n"
                    .to_string(),
            ),
            Self::Unknown { .. } => None,
        }
    }
}

/// `billingSystem/type`, with the reference's default
/// (`BWConfig::get( "billingSystem/type", "standard" )`).
pub fn configured_type(config_section: &DataSection) -> String {
    config_section.read_string("type", "standard")
}

/// `BillingSystemCreator::createFromConfig`: build the system the config
/// names and apply the `isOkay` gate. The reference's "no billing systems
/// registered" WARNING arm cannot occur here — the registry is the fixed
/// match below, never empty.
pub fn create_from_config(
    config_section: &DataSection,
    logon_section: DataSection,
    defs: EntityDescriptionMap,
    database: &XmlDatabase,
) -> BillingCreation {
    let type_name = configured_type(config_section);
    match type_name.as_str() {
        "standard" => {
            let config = BillingConfig::from_section(config_section);
            let system = RegisteredBillingSystem::Standard(XmlBillingSystem::new(
                logon_section,
                config,
                defs,
                database,
            ));
            if BillingSystem::is_okay(&system) {
                BillingCreation::Ready { type_name, system }
            } else {
                BillingCreation::Misconfigured { type_name }
            }
        }
        "custom" => {
            let system = RegisteredBillingSystem::Custom(CustomBillingSystem::new(&defs));
            if system.is_okay() {
                BillingCreation::Ready { type_name, system }
            } else {
                BillingCreation::Misconfigured { type_name }
            }
        }
        _ => BillingCreation::Unknown { type_name },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_billing::tests::account_defs;
    use crate::xml_database::tests::defs;
    use resources::datasection::parse;

    /// An empty XML database: no logon mappings, no entities.
    fn empty_db() -> XmlDatabase {
        XmlDatabase::new(defs())
    }

    /// The `billingSystem/` block for a config string.
    fn config_section(xml: &str) -> DataSection {
        parse("root", xml.as_bytes()).expect("parse billing config")
    }

    /// The default type is `standard`, and a standard creation passes
    /// the logon through the XML system's reject path.
    #[test]
    fn the_default_type_builds_the_standard_system() {
        let creation = create_from_config(
            &config_section("<root/>"),
            parse("root", b"<root/>").expect("parse logon"),
            defs(),
            &empty_db(),
        );

        assert_eq!(
            creation.log_line().as_deref(),
            Some("Using billing system: standard\n")
        );
        let (type_name, mut system) = creation.into_system().expect("ready");
        assert_eq!(type_name, "standard");
        assert!(system.is_okay());
        assert_eq!(
            system.get_entity_key_for_account("stranger", "pw", Address::new(0, 0)),
            EntityKeyOutcome::Failure {
                status: crate::log_on_status::LogOnStatus::new(
                    crate::log_on_status::codes::LOGIN_REJECTED_NO_SUCH_USER
                ),
                error_msg: String::new(),
            }
        );

        // The setter rides through the same dispatch.
        let avatar = defs().name_to_index("Avatar").unwrap();
        let key = EntityKey {
            type_id: avatar,
            db_id: 2,
        };
        assert_eq!(
            system.set_entity_key_for_account("newuser", "pw", key),
            SetEntityKeyOutcome::Success(key)
        );
    }

    /// The custom type builds the custom system when the defs carry an
    /// `Account` type.
    #[test]
    fn the_custom_type_builds_the_custom_system() {
        let account_map = account_defs();
        let account = account_map.name_to_index("Account").unwrap();

        let creation = create_from_config(
            &config_section("<root> <type> custom </type> </root>"),
            parse("root", b"<root/>").expect("parse logon"),
            account_map,
            &empty_db(),
        );

        assert_eq!(
            creation.log_line().as_deref(),
            Some("Using billing system: custom\n")
        );
        let (_, mut system) = creation.into_system().expect("ready");
        assert!(system.is_okay());
        assert_eq!(
            system.get_entity_key_for_account("anyone", "pw", Address::new(0, 0)),
            EntityKeyOutcome::CreateNew {
                entity_type: account,
                should_remember: false,
                data_for_client: String::new(),
                data_for_base_entity: "pw".to_string(),
            }
        );

        // The setter always reports success, through the same dispatch.
        let key = EntityKey {
            type_id: account,
            db_id: 3,
        };
        assert_eq!(
            system.set_entity_key_for_account("anyone", "pw", key),
            SetEntityKeyOutcome::Success(key)
        );
    }

    /// A custom system whose defs carry no `Account` type fails the
    /// `isOkay` gate with the reference's ERROR line and NULL.
    #[test]
    fn a_misconfigured_custom_system_is_refused_with_the_error_line() {
        let creation = create_from_config(
            &config_section("<root> <type> custom </type> </root>"),
            parse("root", b"<root/>").expect("parse logon"),
            defs(),
            &empty_db(),
        );

        assert_eq!(
            creation.log_line().as_deref(),
            Some(
                "BillingSystemCreator::createFromConfig: Billing system is not configured \
                 correctly\n"
            )
        );
        assert!(creation.into_system().is_none());
    }

    /// A standard system accepting strangers without a valid unknown-user
    /// type fails the gate the same way.
    #[test]
    fn a_misconfigured_standard_system_is_refused() {
        let creation = create_from_config(
            &config_section(
                "<root> <type> standard </type> \
                 <shouldAcceptUnknownUsers> true </shouldAcceptUnknownUsers> \
                 <entityTypeForUnknownUsers> Nobody </entityTypeForUnknownUsers> \
                 </root>",
            ),
            parse("root", b"<root/>").expect("parse logon"),
            defs(),
            &empty_db(),
        );

        assert_eq!(
            creation.log_line().as_deref(),
            Some(
                "BillingSystemCreator::createFromConfig: Billing system is not configured \
                 correctly\n"
            )
        );
        assert!(creation.into_system().is_none());
    }

    /// An unregistered type returns NULL without a word.
    #[test]
    fn an_unknown_type_is_refused_silently() {
        let creation = create_from_config(
            &config_section("<root> <type> nonsense </type> </root>"),
            parse("root", b"<root/>").expect("parse logon"),
            defs(),
            &empty_db(),
        );

        assert_eq!(creation.log_line(), None);
        assert!(creation.into_system().is_none());
    }
}
