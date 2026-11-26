//! The default entity a login creates when the account has no entity yet.
//!
//! Reference faces: `createNewEntity` (`server/dbapp/custom.cpp:18-40`) and
//! `DBApp::defaultEntityToStrm` (`server/dbapp/dbapp.cpp:2219-2243`), reached
//! from `LoginHandler::createNewEntity`
//! (`server/dbapp/login_handler.cpp`) when the billing system answers
//! `onGetEntityKeyForAccountCreateNew` or a by-username load fails and
//! `shouldCreateNewOnLoadFailure` is set.
//!
//! The default entity is a section named for the type with one column
//! written: the identifier property, set to the requested name (as blob
//! bytes when the identifier's type is `BLOB`, a string otherwise — the
//! `getPropertyType( typeID, name ) == "BLOB"` test). Every other persistent
//! property is left absent, so it materialises from its type default when
//! the row is written.
//!
//! `defaultEntityToStrm` then streams the section as
//! `BASE_DATA | CELL_DATA | ONLY_PERSISTENT_DATA` and, for a type that can
//! be on a cell, appends an origin pose — `position`, `direction` and
//! `spaceID` all zero (`dbapp.cpp:2228-2235`). The pose is not persistent
//! property data: it stays with the stream face, the way the storage
//! contract already keeps it out of [`crate::record::EntityRecord`].

use entitydef::data_description::DataDescription;
use entitydef::data_type::Kind;
use entitydef::entity_description::EntityDescription;
use entitydef::value::PropertyValue;

/// `createNewEntity`: the default section for `desc`, naming it `name` in
/// the identifier column when the type has an identifier.
///
/// The reference writes into whatever `getIdentifierProperty` returns and
/// leaves the section empty when there is none — a type with no identifier
/// simply creates with all-default values, which is what the FantasyDemo
/// `Account` does.
pub fn create_new_entity(desc: &EntityDescription, name: &str) -> Vec<(String, PropertyValue)> {
    let Some(identifier) = desc.identifier() else {
        return Vec::new();
    };

    let value = match identifier.data_type.kind() {
        Kind::Blob => PropertyValue::Blob(name.as_bytes().to_vec()),
        _ => PropertyValue::String(name.to_string()),
    };
    vec![(identifier.member.name.clone(), value)]
}

/// The values `DBApp::defaultEntityToStrm` hands to the create-entity
/// bundle: the default section streamed as
/// `BASE_DATA | CELL_DATA | ONLY_PERSISTENT_DATA`.
pub struct DefaultEntity {
    /// The persistent values, in visit order: the identifier column where
    /// the type has one, every other column its type default.
    pub values: Vec<PropertyValue>,
    /// `canBeOnCell()`: whether the origin pose follows the values on the
    /// stream (not modelled here — see the module doc).
    pub can_be_on_cell: bool,
}

impl DefaultEntity {
    /// `defaultEntityToStrm`: build the stream-ready default for `desc`.
    ///
    /// A property with no value in the default section takes its type
    /// default (`DataType::initialValue`); a property with neither makes the
    /// creation fail, which the reference reports as `defaultEntityToStrm`
    /// returning `false` — the login is then rejected with
    /// `LOGIN_CUSTOM_DEFINED_ERROR`, "Failed to create default entity".
    pub fn for_type(desc: &EntityDescription, name: &str) -> Option<Self> {
        let named = create_new_entity(desc, name);
        let mut values = Vec::new();
        let mut failed = false;

        if !desc.visit(
            crate::xml_database::DATABASE_DATA_DOMAINS,
            &mut |property: &DataDescription| {
                let supplied = named
                    .iter()
                    .find(|(column, _)| *column == property.member.name)
                    .map(|(_, value)| value.clone());
                let value = supplied.or_else(|| property.data_type.initial_value().ok());
                match value {
                    Some(value) => {
                        values.push(value);
                        true
                    }
                    None => {
                        failed = true;
                        false
                    }
                }
            },
        ) {
            failed = true;
        }

        if failed {
            return None;
        }

        Some(Self {
            values,
            can_be_on_cell: desc.can_be_on_cell(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use entitydef::data_type::TypeContext;
    use entitydef::diagnostics::Diagnostics;
    use entitydef::entity_description::DefResolver;
    use entitydef::entity_map::{EntityDescriptionMap, MessageRanges};
    use resources::datasection::{self, DataSection};
    use std::collections::HashMap;

    const RES: &str = "scripts/entities.xml";
    const DEFS: &str = "scripts/entity_defs";
    /// A persistent, base-data property block.
    const FLAGS: &str = " <Flags> BASE </Flags> <Persistent> true </Persistent> ";

    /// A resolver serving an in-memory res tree.
    struct MemoryResolver {
        files: HashMap<String, Vec<u8>>,
    }

    impl MemoryResolver {
        fn new(files: Vec<(String, Vec<u8>)>) -> Self {
            MemoryResolver {
                files: files.into_iter().collect(),
            }
        }
    }

    impl DefResolver for MemoryResolver {
        fn open_section(&self, path: &str) -> Option<DataSection> {
            datasection::parse("", self.files.get(path)?).ok()
        }

        fn python_script_exists(&self, path: &str) -> bool {
            self.files.contains_key(path) || self.files.contains_key(&format!("{path}/__init__.py"))
        }
    }

    /// One `Hero` type with the given property block, base-only unless the
    /// cell script is registered too.
    fn defs_with(properties: &str, on_cell: bool) -> EntityDescriptionMap {
        let mut files = vec![
            (
                RES.to_string(),
                b"<root> <ClientServerEntities> <Hero/> </ClientServerEntities> \
                  <ServerOnlyEntities/> </root>"
                    .to_vec(),
            ),
            (
                format!("{DEFS}/Hero.def"),
                format!("<root> <Properties> {properties} </Properties> </root>").into_bytes(),
            ),
            ("scripts/base/Hero".to_string(), b"pass".to_vec()),
        ];
        if on_cell {
            files.push(("scripts/cell/Hero".to_string(), b"pass".to_vec()));
        }
        let resolver = MemoryResolver::new(files);

        let entities = resolver.open_section(RES).expect("entities.xml");
        let mut map = EntityDescriptionMap::default();
        let mut diagnostics = Diagnostics::new();
        assert!(
            map.parse(
                Some(&entities),
                &resolver,
                &TypeContext::default(),
                &mut diagnostics,
                &MessageRanges::default(),
            ),
            "defs parse failed: {:?}",
            diagnostics.messages()
        );
        map
    }

    /// `createNewEntity` + `defaultEntityToStrm`: the identifier column
    /// carries the requested name, the other persistent columns keep their
    /// type defaults.
    #[test]
    fn the_identifier_column_carries_the_logon_name() {
        let map = defs_with(
            &format!(
                "<name> <Type> STRING </Type>{FLAGS} <Identifier> true </Identifier> </name> \
                 <hp> <Type> INT32 </Type>{FLAGS} </hp>"
            ),
            false,
        );
        let desc = map.entity_description(0).expect("the one type");

        let default = DefaultEntity::for_type(desc, "hero-7").expect("default entity");
        assert_eq!(
            default.values,
            vec![
                PropertyValue::String("hero-7".to_string()),
                PropertyValue::Int(0),
            ]
        );
        assert!(!default.can_be_on_cell, "a base-only type has no pose tail");
    }

    /// A `BLOB` identifier takes the name's bytes, not the string value.
    #[test]
    fn a_blob_identifier_carries_the_name_bytes() {
        let map = defs_with(
            &format!("<name> <Type> BLOB </Type>{FLAGS} <Identifier> true </Identifier> </name>"),
            false,
        );
        let desc = map.entity_description(0).expect("the one type");

        assert_eq!(
            create_new_entity(desc, "byte-name"),
            vec![(
                "name".to_string(),
                PropertyValue::Blob(b"byte-name".to_vec()),
            )]
        );
        let default = DefaultEntity::for_type(desc, "byte-name").expect("default entity");
        assert_eq!(
            default.values,
            vec![PropertyValue::Blob(b"byte-name".to_vec())]
        );
    }

    /// A type with no identifier creates with every column at its type
    /// default — the FantasyDemo `Account` case.
    #[test]
    fn a_type_without_an_identifier_defaults_every_column() {
        let map = defs_with(
            &format!(
                "<hp> <Type> INT32 </Type>{FLAGS} </hp> \
                 <title> <Type> STRING </Type>{FLAGS} </title>"
            ),
            false,
        );
        let desc = map.entity_description(0).expect("the one type");

        assert!(create_new_entity(desc, "whoever").is_empty());
        let default = DefaultEntity::for_type(desc, "whoever").expect("default entity");
        assert_eq!(
            default.values,
            vec![PropertyValue::Int(0), PropertyValue::String(String::new())]
        );
    }

    /// A type with a cell part reports the pose tail
    /// (`dbapp.cpp:2228-2235`).
    #[test]
    fn a_cell_type_reports_the_pose_tail() {
        let map = defs_with(&format!("<hp> <Type> INT32 </Type>{FLAGS} </hp>"), true);
        let desc = map.entity_description(0).expect("the one type");

        let default = DefaultEntity::for_type(desc, "guard").expect("default entity");
        assert!(default.can_be_on_cell);
        assert_eq!(default.values, vec![PropertyValue::Int(0)]);
    }
}
