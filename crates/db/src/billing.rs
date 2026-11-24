//! Billing systems — the account authentication front end.
//!
//! The DBApp turns user/password credentials into an entity key through a
//! pluggable `BillingSystem` (`lib/db_storage/billing_system.{hpp,cpp}`).
//! The standard XML backend keeps a logOnName → (EntityKey, password) map
//! in the database's `LogOnMapping` section
//! (`lib/db_storage_xml/xml_billing_system.{hpp,cpp}`) — the XML twin of
//! the MySQL `bigworldLogOnMapping` table.
//!
//! Layering: domain library (layer 2). The handler callbacks of the
//! reference are modelled as the returned outcome enums — the reference
//! calls exactly one handler method per request.

use std::collections::BTreeMap;

use entitydef::entity_map::EntityDescriptionMap;
use foundation::ids::{EntityTypeId, INVALID_ENTITY_TYPE_ID};
use resources::datasection::DataSection;

use crate::account::AccountLogOn;
use crate::storage::EntityKey;

/// The `billingSystem/` block of bw.xml, read with the reference's
/// defaults (`BillingSystem::BillingSystem`,
/// `db_storage/billing_system.cpp:21-53`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BillingConfig {
    /// `billingSystem/shouldAcceptUnknownUsers`.
    pub should_accept_unknown_users: bool,
    /// `billingSystem/shouldRememberUnknownUsers`.
    pub should_remember_unknown_users: bool,
    /// `billingSystem/authenticateViaBaseEntity`.
    pub authenticate_via_base_entity: bool,
    /// `billingSystem/entityTypeForUnknownUsers`.
    pub entity_type_for_unknown_users: String,
}

impl BillingConfig {
    /// Read the block from a section; missing children keep the defaults
    /// (`BWConfig::get( path, default )`).
    pub fn from_section(section: &DataSection) -> Self {
        Self {
            should_accept_unknown_users: section
                .find_child("shouldAcceptUnknownUsers")
                .map(|s| s.as_bool(false))
                .unwrap_or(false),
            should_remember_unknown_users: section
                .find_child("shouldRememberUnknownUsers")
                .map(|s| s.as_bool(false))
                .unwrap_or(false),
            authenticate_via_base_entity: section
                .find_child("authenticateViaBaseEntity")
                .map(|s| s.as_bool(false))
                .unwrap_or(false),
            entity_type_for_unknown_users: section.read_string("entityTypeForUnknownUsers", ""),
        }
    }
}

/// The one callback `getEntityKeyForAccount` makes
/// (`IGetEntityKeyForAccountHandler`,
/// `db_storage/billing_system.hpp:17-69`).
#[derive(Debug, Clone, PartialEq)]
pub enum EntityKeyOutcome {
    /// The credentials name a known entity
    /// (`onGetEntityKeyForAccountSuccess`).
    Success {
        key: EntityKey,
        data_for_client: String,
        data_for_base_entity: String,
    },
    /// Load the entity with the given username
    /// (`onGetEntityKeyForAccountLoadFromUsername`).
    LoadFromUsername {
        entity_type: EntityTypeId,
        username: String,
        should_create_new_on_load_failure: bool,
        should_remember_new_on_load_failure: bool,
        data_for_client: String,
        data_for_base_entity: String,
    },
    /// Create a new entity for the account
    /// (`onGetEntityKeyForAccountCreateNew`).
    CreateNew {
        entity_type: EntityTypeId,
        should_remember: bool,
        data_for_client: String,
        data_for_base_entity: String,
    },
    /// The account is rejected (`onGetEntityKeyForAccountFailure`).
    Failure {
        status: AccountLogOn,
        error_msg: String,
    },
}

/// The one callback `setEntityKeyForAccount` makes
/// (`ISetEntityKeyForAccountHandler`,
/// `db_storage/billing_system.hpp:71-85`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetEntityKeyOutcome {
    /// `onSetEntityKeyForAccountSuccess`.
    Success(EntityKey),
    /// `onSetEntityKeyForAccountFailure`.
    Failure(EntityKey),
}

/// The billing system interface
/// (`BillingSystem`, `db_storage/billing_system.hpp:87-125`).
pub trait BillingSystem {
    /// Turn login credentials into the entity key associated with them.
    fn get_entity_key_for_account(&mut self, logon_name: &str, password: &str) -> EntityKeyOutcome;

    /// Set the mapping between credentials and an entity.
    fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome;

    /// `BillingSystem::isOkay`: configured well enough to run
    /// (`db_storage/billing_system.hpp:116-121`).
    fn is_okay(&self) -> bool;
}

/// The configuration-driven base shared by every backend
/// (`BillingSystem`'s protected members).
#[derive(Debug)]
struct BillingSystemBase {
    config: BillingConfig,
    entity_type_id_for_unknown_users: EntityTypeId,
}

impl BillingSystemBase {
    fn new(config: BillingConfig, defs: &EntityDescriptionMap) -> Self {
        let entity_type_id_for_unknown_users = defs
            .name_to_index(&config.entity_type_for_unknown_users)
            .unwrap_or(INVALID_ENTITY_TYPE_ID);
        Self {
            config,
            entity_type_id_for_unknown_users,
        }
    }
}

impl BillingSystemBase {
    fn is_okay(&self) -> bool {
        (self.entity_type_id_for_unknown_users != INVALID_ENTITY_TYPE_ID)
            || (!self.config.should_accept_unknown_users
                && !self.config.authenticate_via_base_entity)
    }
}

/// One logOnName mapping row (`LogOnMapping`,
/// `db_storage_xml/xml_billing_system.hpp:44-58`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogOnMapping {
    /// The entity the account logs into.
    pub entity_key: EntityKey,
    /// `matchesPassword`: plain equality.
    pub password: String,
}

/// The standard XML billing system
/// (`XMLBillingSystem`,
/// `db_storage_xml/xml_billing_system.{hpp,cpp}`).
pub struct XmlBillingSystem {
    base: BillingSystemBase,
    /// The key is the "logOnName" column.
    logon_map: BTreeMap<String, LogOnMapping>,
    /// The `LogOnMapping` section the mappings round-trip through.
    logon_section: DataSection,
    defs: EntityDescriptionMap,
}

impl XmlBillingSystem {
    /// Build from the LogOnMapping section
    /// (`XMLBillingSystem::XMLBillingSystem` + `initLogOnMapping`,
    /// `xml_billing_system.cpp:25-70`).
    pub fn new(
        logon_section: DataSection,
        config: BillingConfig,
        defs: EntityDescriptionMap,
        database: &crate::xml_database::XmlDatabase,
    ) -> Self {
        let mut system = Self {
            base: BillingSystemBase::new(config, &defs),
            logon_map: BTreeMap::new(),
            logon_section,
            defs,
        };
        system.init_log_on_mapping(database);
        system
    }

    /// Read the section's items into the map. An item whose type is not a
    /// valid entity type is ignored with a warning
    /// (`xml_billing_system.cpp:33-70`).
    fn init_log_on_mapping(&mut self, database: &crate::xml_database::XmlDatabase) {
        for index in 0..self.logon_section.count_children() {
            let item = self
                .logon_section
                .open_child(index)
                .expect("index below count_children");

            let type_name = item.read_string("type", "");
            let type_id = self.defs.name_to_index(&type_name);

            let Some(type_id) = type_id else {
                eprintln!(
                    "XMLDatabase::initLogOnMapping: Logon mapping ignored because \
                     '{type_name}' is not a valid entity type"
                );
                continue;
            };

            let mut entity_id = item
                .find_child("entityID")
                .map(|s| s.as_int64(0))
                .unwrap_or(0);
            if entity_id == 0 {
                entity_id =
                    database.find_entity_by_name(type_id, &item.read_string("entityName", ""));
            }

            let log_on_name = item.read_string("logOnName", "");
            self.logon_map.insert(
                log_on_name,
                LogOnMapping {
                    entity_key: EntityKey {
                        type_id,
                        db_id: entity_id,
                    },
                    password: item.read_string("password", ""),
                },
            );
        }
    }

    /// The resolved map, for inspection and tests.
    pub fn logon_map(&self) -> &BTreeMap<String, LogOnMapping> {
        &self.logon_map
    }
}

impl BillingSystem for XmlBillingSystem {
    /// `XMLBillingSystem::getEntityKeyForAccount`,
    /// `xml_billing_system.cpp:79-116`.
    fn get_entity_key_for_account(&mut self, logon_name: &str, password: &str) -> EntityKeyOutcome {
        if self.base.config.authenticate_via_base_entity {
            return EntityKeyOutcome::LoadFromUsername {
                entity_type: self.base.entity_type_id_for_unknown_users,
                username: logon_name.to_string(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
                data_for_client: String::new(),
                // The password rides to the base entity as its data.
                data_for_base_entity: password.to_string(),
            };
        }

        if let Some(mapping) = self.logon_map.get(logon_name) {
            if mapping.password == password {
                EntityKeyOutcome::Success {
                    key: mapping.entity_key,
                    data_for_client: String::new(),
                    data_for_base_entity: String::new(),
                }
            } else {
                EntityKeyOutcome::Failure {
                    status: AccountLogOn::RejectedInvalidPassword,
                    error_msg: String::new(),
                }
            }
        } else if self.base.config.should_accept_unknown_users {
            EntityKeyOutcome::CreateNew {
                entity_type: self.base.entity_type_id_for_unknown_users,
                should_remember: self.base.config.should_remember_unknown_users,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        } else {
            EntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedNoSuchUser,
                error_msg: String::new(),
            }
        }
    }

    /// `XMLBillingSystem::setEntityKeyForAccount`,
    /// `xml_billing_system.cpp:119-159`. An existing mapping updates its
    /// item in place; a new one appends an `<item>`.
    fn set_entity_key_for_account(
        &mut self,
        username: &str,
        password: &str,
        key: EntityKey,
    ) -> SetEntityKeyOutcome {
        // Try to find existing section (`xml_billing_system.cpp:123-136`).
        let mut item_index = None;
        if self.logon_map.contains_key(username) {
            for index in 0..self.logon_section.count_children() {
                let item = self
                    .logon_section
                    .open_child(index)
                    .expect("index below count_children");
                if item.read_string("logOnName", "") == username {
                    item_index = Some(index);
                    break;
                }
            }
        }

        self.logon_map.insert(
            username.to_string(),
            LogOnMapping {
                entity_key: key,
                password: password.to_string(),
            },
        );

        let type_name = self
            .defs
            .entity_description(key.type_id)
            .map(|desc| desc.name.clone())
            .unwrap_or_default();

        let item_index = match item_index {
            Some(index) => index,
            None => {
                self.logon_section.add_child("item");
                self.logon_section.count_children() - 1
            }
        };

        let item = self
            .logon_section
            .child_mut(item_index)
            .expect("index just chosen");
        item.write_string("logOnName", username);
        item.write_string("password", password);
        item.write_string("type", &type_name);
        // `pSection->write( "entityID", ekey.dbID )` — find-or-create the
        // child, then set its value.
        if item.find_child("entityID").is_none() {
            item.add_child("entityID");
        }
        for index in 0..item.count_children() {
            if let Some(child) = item.child_mut(index)
                && child.section_name() == "entityID"
            {
                child.set_int64(key.db_id);
                break;
            }
        }

        SetEntityKeyOutcome::Success(key)
    }

    fn is_okay(&self) -> bool {
        self.base.is_okay()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_database::tests::defs;
    use crate::xml_database::{AutoLoadUpdate, XmlDatabase};
    use foundation::stream::BinaryStream;
    use resources::datasection::parse;

    /// A database holding one named Avatar (id 1) and a LogOnMapping
    /// section with a by-id row, a by-name row and an invalid-type row.
    fn fixture(logon_xml: &str) -> (XmlDatabase, XmlBillingSystem, u16) {
        let mut database = XmlDatabase::new(defs());
        let avatar = defs().name_to_index("Avatar").unwrap();
        let mut stream = BinaryStream::new();
        let _ = stream.write_string("moe");
        stream.write_i32(10);
        stream.write_i64(0);
        database
            .put_entity_stream(avatar, 1, &mut stream, AutoLoadUpdate::Retain, true)
            .expect("put");

        let section = parse("root", logon_xml.as_bytes()).expect("parse logon map");
        let config = BillingConfig {
            should_accept_unknown_users: false,
            should_remember_unknown_users: false,
            authenticate_via_base_entity: false,
            entity_type_for_unknown_users: "Avatar".to_string(),
        };
        let system = XmlBillingSystem::new(section, config, defs(), &database);
        (database, system, avatar)
    }

    const LOGON_MAP: &str = "<root> \
         <item> <logOnName> moe </logOnName> <password> secret </password> \
             <type> Avatar </type> <entityID> 1 </entityID> </item> \
         <item> <logOnName> byname </logOnName> <password> pw2 </password> \
             <type> Avatar </type> <entityName> moe </entityName> </item> \
         <item> <logOnName> badtype </logOnName> <password> pw3 </password> \
             <type> Nobody </type> </item> \
     </root>";

    /// The `billingSystem/` block keeps the defaults for missing children
    /// (`BWConfig::get( path, default )`).
    #[test]
    fn the_billing_config_block_reads_with_defaults() {
        let empty = parse("root", b"<root/>").expect("parse");
        let config = BillingConfig::from_section(&empty);
        assert!(!config.should_accept_unknown_users);
        assert!(!config.should_remember_unknown_users);
        assert!(!config.authenticate_via_base_entity);
        assert_eq!(config.entity_type_for_unknown_users, "");

        let full = parse(
            "root",
            b"<root> \
               <shouldAcceptUnknownUsers> true </shouldAcceptUnknownUsers> \
               <shouldRememberUnknownUsers> TRUE </shouldRememberUnknownUsers> \
               <authenticateViaBaseEntity> false </authenticateViaBaseEntity> \
               <entityTypeForUnknownUsers> Nobody </entityTypeForUnknownUsers> \
             </root>",
        )
        .expect("parse");
        let config = BillingConfig::from_section(&full);
        assert!(config.should_accept_unknown_users);
        assert!(config.should_remember_unknown_users);
        assert!(!config.authenticate_via_base_entity);
        assert_eq!(config.entity_type_for_unknown_users, "Nobody");
    }

    #[test]
    fn a_legal_account_matches_to_its_mapped_key() {
        let (_database, mut system, avatar) = fixture(LOGON_MAP);

        assert_eq!(
            system.get_entity_key_for_account("moe", "secret"),
            EntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: avatar,
                    db_id: 1
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    #[test]
    fn a_zero_entity_id_resolves_the_entity_by_name() {
        let (database, mut system, avatar) = fixture(LOGON_MAP);
        let _ = database;

        assert_eq!(
            system.get_entity_key_for_account("byname", "pw2"),
            EntityKeyOutcome::Success {
                key: EntityKey {
                    type_id: avatar,
                    db_id: 1
                },
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    #[test]
    fn an_invalid_password_and_an_unknown_user_are_rejected() {
        let (_database, mut system, _avatar) = fixture(LOGON_MAP);

        assert_eq!(
            system.get_entity_key_for_account("moe", "wrong"),
            EntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedInvalidPassword,
                error_msg: String::new(),
            }
        );
        // The bad-type row was ignored at init, so it is an unknown user.
        assert_eq!(
            system.get_entity_key_for_account("badtype", "pw3"),
            EntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedNoSuchUser,
                error_msg: String::new(),
            }
        );
    }

    #[test]
    fn unknown_users_are_accepted_only_when_configured() {
        let (_database, _system, avatar) = fixture(LOGON_MAP);

        // shouldAcceptUnknownUsers on: a stranger gets a create-new with
        // the configured unknown-user type and the remember flag.
        let section = parse("root", b"<root/>").expect("parse");
        let config = BillingConfig {
            should_accept_unknown_users: true,
            should_remember_unknown_users: true,
            authenticate_via_base_entity: false,
            entity_type_for_unknown_users: "Avatar".to_string(),
        };
        let mut accepting = XmlBillingSystem::new(section, config, defs(), &empty_db());
        assert_eq!(
            accepting.get_entity_key_for_account("stranger", "pw"),
            EntityKeyOutcome::CreateNew {
                entity_type: avatar,
                should_remember: true,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
    }

    #[test]
    fn base_entity_authentication_defers_to_the_username_load() {
        let section = parse("root", b"<root/>").expect("parse");
        let config = BillingConfig {
            should_accept_unknown_users: false,
            should_remember_unknown_users: false,
            authenticate_via_base_entity: true,
            entity_type_for_unknown_users: "Avatar".to_string(),
        };
        let mut system = XmlBillingSystem::new(section, config, defs(), &empty_db());

        // The password rides as the base entity's data
        // (`xml_billing_system.cpp:83-91`).
        assert_eq!(
            system.get_entity_key_for_account("whoever", "their-password"),
            EntityKeyOutcome::LoadFromUsername {
                entity_type: 0,
                username: "whoever".to_string(),
                should_create_new_on_load_failure: true,
                should_remember_new_on_load_failure: false,
                data_for_client: String::new(),
                data_for_base_entity: "their-password".to_string(),
            }
        );
    }

    #[test]
    fn set_updates_the_existing_item_and_appends_new_ones() {
        let (mut database, mut system, avatar) = fixture(LOGON_MAP);
        let children_before = system.logon_section_child_count();

        // Password rotation on "moe": one item, changed password.
        let key = EntityKey {
            type_id: avatar,
            db_id: 1,
        };
        assert_eq!(
            system.set_entity_key_for_account("moe", "rotated", key),
            SetEntityKeyOutcome::Success(key)
        );
        assert_eq!(system.logon_section_child_count(), children_before);
        assert_eq!(
            system.get_entity_key_for_account("moe", "rotated"),
            EntityKeyOutcome::Success {
                key,
                data_for_client: String::new(),
                data_for_base_entity: String::new(),
            }
        );
        assert_eq!(
            system.get_entity_key_for_account("moe", "secret"),
            EntityKeyOutcome::Failure {
                status: AccountLogOn::RejectedInvalidPassword,
                error_msg: String::new(),
            }
        );

        // A brand-new account appends exactly one item, written in the
        // reference's column order.
        let key2 = EntityKey {
            type_id: avatar,
            db_id: 2,
        };
        assert_eq!(
            system.set_entity_key_for_account("curly", "pw", key2),
            SetEntityKeyOutcome::Success(key2)
        );
        assert_eq!(system.logon_section_child_count(), children_before + 1);
        let item = system
            .logon_section_ref()
            .open_child(system.logon_section_ref().count_children() - 1)
            .expect("appended item");
        assert_eq!(item.read_string("logOnName", ""), "curly");
        assert_eq!(item.read_string("password", ""), "pw");
        assert_eq!(item.read_string("type", ""), "Avatar");
        assert_eq!(
            item.find_child("entityID")
                .map(|c| c.as_int64(0))
                .unwrap_or(-1),
            2
        );

        // The map agrees with the section.
        assert_eq!(system.logon_map().len(), 3);
        assert_eq!(system.logon_map()["curly"].password, "pw");

        // And the whole thing still round-trips through the database's
        // info section on save.
        database.set_file_path("unused");
        let _ = database.to_xml();
    }

    #[test]
    fn is_okay_follows_the_configuration_matrix() {
        let section = parse("root", b"<root/>").expect("parse");

        // No unknown-user type, no accepting flags: the default setup is
        // fine (it just rejects strangers).
        let plain = XmlBillingSystem::new(
            parse("root", b"<root/>").expect("parse"),
            BillingConfig::default(),
            defs(),
            &empty_db(),
        );
        assert!(BillingSystem::is_okay(&plain));

        // Accepting strangers without a valid unknown-user type is not.
        let broken = XmlBillingSystem::new(
            section,
            BillingConfig {
                should_accept_unknown_users: true,
                should_remember_unknown_users: false,
                authenticate_via_base_entity: false,
                entity_type_for_unknown_users: "Nobody".to_string(),
            },
            defs(),
            &empty_db(),
        );
        assert!(!BillingSystem::is_okay(&broken));
    }

    // -- helpers ---------------------------------------------------------

    fn empty_db() -> XmlDatabase {
        XmlDatabase::new(defs())
    }

    impl XmlBillingSystem {
        fn logon_section_child_count(&self) -> usize {
            self.logon_section_ref().count_children()
        }

        fn logon_section_ref(&self) -> &DataSection {
            &self.logon_section
        }
    }
}
