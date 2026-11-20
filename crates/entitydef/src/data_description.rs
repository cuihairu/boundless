//! `data_description.hpp` / `data_description.cpp`: one replicated property.
//!
//! `DataDescription::parse` runs the property's `<Type>` through the meta-type
//! registry, resolves its `<Flags>` in the distribution table, then layers the
//! persistence / identifier / index / replay bits on top. The digest order is
//! member name, distribution flags, then the type's own digest.

use std::sync::Arc;

use resources::datasection::DataSection;

use crate::data_type::{TypeContext, build_type, read_bool, read_int};
use crate::diagnostics::Diagnostics;
use crate::digest::Md5;
use crate::flags::{
    DEFAULT_DATABASE_LENGTH, DatabaseIndexing, EntityDataFlags, ID, PERSISTENT, REPLAY,
    set_entity_data_flags,
};
use crate::lod::NO_LEVEL;
use crate::member::MemberDescription;

/// One property description (`DataDescription`).
#[derive(Debug, Clone)]
pub struct DataDescription {
    /// The `MemberDescription` base: name, reliability, header sizing.
    pub member: MemberDescription,
    /// `pDataType_`.
    pub data_type: Arc<crate::data_type::DataType>,
    /// `dataFlags_`.
    pub data_flags: EntityDataFlags,
    /// `index_`: the other-client property ordering, set during merge.
    pub index: i32,
    /// `localIndex_`.
    pub local_index: i32,
    /// `eventStampIndex_`: -1 until the property is given a detail slot.
    pub event_stamp_index: i32,
    /// `clientServerFullIndex_`.
    pub client_server_full_index: i32,
    /// `detailLevel_`.
    pub detail_level: i32,
    /// `databaseLength_`.
    pub database_length: i32,
    /// `databaseIndexingType_`.
    pub database_indexing: DatabaseIndexing,
    /// `componentName_`: set when the property came from a component.
    pub component_name: String,
    /// `pInitialValue_` source: the `<Default>` child of a const type.
    pub initial_value_section: Option<DataSection>,
    /// `pDefaultSection_`: the `<Default>` child of a non-const type.
    pub default_section: Option<DataSection>,
}

impl Default for DataDescription {
    fn default() -> Self {
        Self {
            member: MemberDescription::default(),
            data_type: Arc::new(crate::data_type::DataType::default()),
            data_flags: EntityDataFlags::default(),
            index: -1,
            local_index: -1,
            event_stamp_index: -1,
            client_server_full_index: -1,
            detail_level: NO_LEVEL,
            database_length: DEFAULT_DATABASE_LENGTH,
            database_indexing: DatabaseIndexing::None,
            component_name: String::new(),
            initial_value_section: None,
            default_section: None,
        }
    }
}

impl DataDescription {
    /// The property name (`name_`, lives on the base class).
    pub fn name(&self) -> &str {
        &self.member.name
    }

    /// `getFileName()`-style full name: `<interface>.<name>` plus the
    /// component when present (`fullName()`).
    pub fn full_name(&self) -> String {
        match self.component_name.is_empty() {
            true => format!("{}.{}", self.member.interface_name, self.member.name),
            false => format!(
                "{}.{}.{}",
                self.member.interface_name, self.component_name, self.member.name
            ),
        }
    }

    /// `DataDescription::parse`, with the meta-type registry threaded
    /// explicitly (`ctx` stands in for the reference's global alias table).
    pub fn parse(
        &mut self,
        section: &DataSection,
        parent_name: &str,
        component_name: &str,
        num_latest_event_members: Option<&mut u32>,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        self.member.name = section.section_name().to_string();
        self.component_name = component_name.to_string();

        let type_section = section.open_section("Type");
        let Some(data_type) = build_type(ctx, type_section, diag) else {
            diag.error(format!(
                "DataDescription::parse: Unable to find data type '{}' for {}.{}\n",
                section.read_string("Type", ""),
                parent_name,
                self.member.name
            ));
            return false;
        };
        self.data_type = Arc::new(data_type);

        // setEntityDataFlags: an unknown or missing <Flags> fails the parse.
        match set_entity_data_flags(&section.read_string("Flags", "")) {
            Some(flags) => self.data_flags = flags,
            None => {
                diag.error(format!(
                    "DataDescription::parse: Invalid Flags section '{}' for {}\n",
                    section.read_string("Flags", ""),
                    self.member.name
                ));
                return false;
            }
        }

        if read_bool(section, "Persistent", false) {
            self.data_flags = self.data_flags | EntityDataFlags::new(PERSISTENT);
        }

        let mut is_indexed = false;
        let mut is_unique = false;

        if read_bool(section, "Identifier", false) {
            self.data_flags = self.data_flags | EntityDataFlags::new(ID);
            is_indexed = true;
            is_unique = true;
        }

        // Identifier properties are expected to be indexed, so they default
        // to indexed.
        if read_bool(section, "Indexed", is_indexed) {
            if !self.data_flags.is_persistent() {
                diag.error(format!(
                    "DataDescription::parse: {} must be persistent to be indexed\n",
                    self.member.name
                ));
                return false;
            }

            self.database_indexing = if read_bool(section, "Indexed/Unique", is_unique) {
                DatabaseIndexing::Unique
            } else {
                DatabaseIndexing::NonUnique
            };
        }

        let default_sub_section = section.find_child("Default");

        // Other-client data is exposed for replay by default.
        if self.data_flags.is_client_server_data() {
            let is_exposed_for_replay = read_bool(
                section,
                "ExposedForReplay",
                self.data_flags.is_other_client_data(),
            );
            self.data_flags = EntityDataFlags::new(if is_exposed_for_replay {
                self.data_flags.bits() | REPLAY
            } else {
                self.data_flags.bits() & !REPLAY
            });
        }

        // A <Default> on a const type becomes the initial value; on a
        // non-const type it is kept as a section for later construction.
        if let Some(default_sub_section) = default_sub_section {
            if self.data_type.is_const() {
                self.initial_value_section = Some(default_sub_section.clone());
            } else {
                self.default_section = Some(default_sub_section.clone());
            }
        }

        self.database_length = read_int(section, "DatabaseLength", self.database_length);

        let is_okay = self.member.parse(
            section,
            parent_name,
            self.data_flags.is_client_server_data(),
            if self.data_flags.is_other_client_data() {
                num_latest_event_members
            } else {
                None
            },
            diag,
        );

        if !self.member.is_reliable && !self.member.should_send_latest_only() {
            diag.warning(format!(
                "DataDescription::parse: {}.{} is unreliable but does not have SendLatestOnly set.\n",
                parent_name, self.member.name
            ));
        }

        is_okay
    }

    /// `DataDescription::streamSize`: fixed types report their size, variable
    /// types report the negated header width.
    pub fn stream_size(&self) -> i32 {
        let data_type_stream_size = self.data_type.stream_size();
        if data_type_stream_size < 0 {
            return -(self.member.var_len_header_size as i32);
        }
        data_type_stream_size
    }

    /// `DataDescription::addToMD5`: member, distribution flags, type digest.
    pub fn add_to_md5(&self, md5: &mut Md5) {
        self.member.add_to_md5(md5, self.data_type.stream_size());

        let md5_data_flags = self.data_flags.distribution_bits();
        md5.append_i32(md5_data_flags as i32);

        self.data_type.add_to_md5(md5);
    }

    /// `clientSafety()`, forwarded to the type.
    pub fn client_safety(&self) -> u32 {
        self.data_type.client_safety()
    }

    /// `isTypeDeprecated()`-adjacent helper used by the merge: does the type
    /// refuse to travel to the client at all?
    pub fn is_client_unusable(&self) -> bool {
        self.client_safety() & crate::flags::CLIENT_UNUSABLE != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_xml(xml: &str) -> DataSection {
        resources::datasection::parse("", xml.as_bytes()).expect("valid fixture xml")
    }

    fn ctx_with_alias() -> TypeContext {
        let mut ctx = TypeContext::default();
        let mut diag = Diagnostics::new();
        ctx.init_aliases(
            || {
                Some(parse_xml(
                    "<root> <HP> INT32 <Default> 100 </Default> </HP> </root>",
                ))
            },
            &mut diag,
        );
        ctx
    }

    fn parse_prop(xml: &str, ctx: &TypeContext) -> (DataDescription, Diagnostics) {
        let mut prop = DataDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml(xml);
        let ok = prop.parse(&section, "Avatar", "", None, ctx, &mut diag);
        assert!(ok, "parse must succeed for {xml}: {:?}", diag.messages());
        (prop, diag)
    }

    #[test]
    fn defaults_match_the_reference_constructor() {
        let prop = DataDescription::default();
        assert_eq!(prop.index, -1);
        assert_eq!(prop.local_index, -1);
        assert_eq!(prop.event_stamp_index, -1);
        assert_eq!(prop.client_server_full_index, -1);
        assert_eq!(prop.detail_level, NO_LEVEL);
        assert_eq!(prop.database_length, DEFAULT_DATABASE_LENGTH);
        assert_eq!(prop.database_indexing, DatabaseIndexing::None);
        assert_eq!(prop.data_flags, EntityDataFlags::default());
    }

    #[test]
    fn a_simple_property_resolves_flags_and_type() {
        let ctx = ctx_with_alias();
        let (prop, diag) = parse_prop(
            "<hp> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> <Default> 5 </Default> </hp>",
            &ctx,
        );
        assert_eq!(prop.name(), "hp");
        assert_eq!(
            prop.data_flags,
            // ALL_CLIENTS plus the replay bit that other-client data gains.
            EntityDataFlags::new(
                crate::flags::GHOSTED
                    | crate::flags::OTHER_CLIENT
                    | crate::flags::OWN_CLIENT
                    | crate::flags::REPLAY
            )
        );
        assert_eq!(prop.data_type.meta_name(), "INT32");
        assert_eq!(prop.stream_size(), 4);
        // INT32 is const, so the <Default> becomes the initial value.
        assert!(prop.initial_value_section.is_some());
        assert!(prop.default_section.is_none());
        assert!(!diag.has_errors());
    }

    #[test]
    fn missing_flags_is_a_parse_failure() {
        let ctx = ctx_with_alias();
        let mut prop = DataDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<hp> <Type> INT32 </Type> </hp>");
        assert!(!prop.parse(&section, "Avatar", "", None, &ctx, &mut diag));
        assert_eq!(
            diag.errors(),
            vec!["DataDescription::parse: Invalid Flags section '' for hp\n"]
        );
    }

    #[test]
    fn unknown_type_is_reported_twice_like_the_reference() {
        let ctx = TypeContext::default();
        let mut prop = DataDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<hp> <Type> NOT_A_TYPE </Type> <Flags> BASE </Flags> </hp>");
        assert!(!prop.parse(&section, "Avatar", "", None, &ctx, &mut diag));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Could not find MetaDataType 'NOT_A_TYPE'"))
        );
        assert!(diag.errors().iter().any(|m| m
            == "DataDescription::parse: Unable to find data type 'NOT_A_TYPE' for Avatar.hp\n"));
    }

    #[test]
    fn identifier_implies_indexed_and_needs_persistence() {
        let ctx = ctx_with_alias();

        let (prop, _) = parse_prop(
            "<id> <Type> INT32 </Type> <Flags> BASE </Flags> <Identifier> true </Identifier> <Persistent> true </Persistent> </id>",
            &ctx,
        );
        assert!(prop.data_flags.is_identifier());
        assert!(prop.data_flags.is_persistent());
        assert_eq!(prop.database_indexing, DatabaseIndexing::Unique);
        assert!(prop.database_indexing.is_indexed());

        // Indexed without persistent fails.
        let mut prop = DataDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml(
            "<hp> <Type> INT32 </Type> <Flags> BASE </Flags> <Indexed> true </Indexed> </hp>",
        );
        assert!(!prop.parse(&section, "Avatar", "", None, &ctx, &mut diag));
        assert_eq!(
            diag.errors(),
            vec!["DataDescription::parse: hp must be persistent to be indexed\n"]
        );

        // Non-unique index: Indexed without Identifier defaults Unique off.
        let (prop, _) = parse_prop(
            "<name> <Type> UNICODE_STRING </Type> <Flags> BASE </Flags> <Indexed> true </Indexed> <Persistent> true </Persistent> </name>",
            &ctx,
        );
        assert_eq!(prop.database_indexing, DatabaseIndexing::NonUnique);
    }

    #[test]
    fn replay_defaults_on_for_other_client_data_only() {
        let ctx = ctx_with_alias();

        let (prop, _) = parse_prop(
            "<hp> <Type> INT32 </Type> <Flags> OTHER_CLIENTS </Flags> </hp>",
            &ctx,
        );
        assert!(prop.data_flags.is_exposed_for_replay());

        let (prop, _) = parse_prop(
            "<hp> <Type> INT32 </Type> <Flags> BASE </Flags> </hp>",
            &ctx,
        );
        assert!(!prop.data_flags.is_exposed_for_replay());

        // And the tag can opt other-client data out of replay.
        let (prop, _) = parse_prop(
            "<hp> <Type> INT32 </Type> <Flags> OTHER_CLIENTS </Flags> <ExposedForReplay> false </ExposedForReplay> </hp>",
            &ctx,
        );
        assert!(
            !prop.data_flags.is_exposed_for_replay(),
            "the tag overrides the other-client default"
        );
    }

    #[test]
    fn non_const_defaults_go_to_the_default_section() {
        let ctx = TypeContext::default();
        let (prop, _) = parse_prop(
            "<bag> <Type> ARRAY <size> 3 </size> <of> INT8 </of> </Type> <Flags> CELL_PUBLIC </Flags> <Default> 1 </Default> </bag>",
            &ctx,
        );
        assert!(prop.initial_value_section.is_none());
        assert!(prop.default_section.is_some());
    }

    #[test]
    fn send_latest_only_only_counts_for_other_client_data() {
        let ctx = ctx_with_alias();
        let mut counter = 0u32;

        // ALL_CLIENTS includes other-client: the counter is consumed.
        let (prop, _) = {
            let mut prop = DataDescription::default();
            let mut diag = Diagnostics::new();
            let section = parse_xml(
                "<hp> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> <SendLatestOnly> true </SendLatestOnly> </hp>",
            );
            assert!(prop.parse(&section, "Avatar", "", Some(&mut counter), &ctx, &mut diag));
            (prop, diag)
        };
        assert!(prop.member.should_send_latest_only());
        assert_eq!(prop.member.latest_event_index, 0);
        assert_eq!(counter, 1);

        // BASE data: no counter, so SendLatestOnly is ignored.
        let (prop, _) = {
            let mut prop = DataDescription::default();
            let mut diag = Diagnostics::new();
            let section = parse_xml(
                "<gold> <Type> INT32 </Type> <Flags> BASE </Flags> <SendLatestOnly> true </SendLatestOnly> </gold>",
            );
            assert!(prop.parse(&section, "Avatar", "", Some(&mut counter), &ctx, &mut diag));
            (prop, diag)
        };
        assert!(!prop.member.should_send_latest_only());
        assert_eq!(counter, 1, "no counter consumed for base data");
    }

    #[test]
    fn unreliable_without_send_latest_only_warns() {
        let ctx = ctx_with_alias();
        let mut prop = DataDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml(
            "<hp> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> <IsReliable> false </IsReliable> </hp>",
        );
        assert!(prop.parse(&section, "Avatar", "", None, &ctx, &mut diag));
        assert_eq!(
            diag.warnings(),
            vec![
                "DataDescription::parse: Avatar.hp is unreliable but does not have SendLatestOnly set.\n"
            ]
        );
    }

    #[test]
    fn variable_types_report_the_negated_header_size() {
        let ctx = TypeContext::default();
        let (prop, _) = parse_prop(
            "<name> <Type> UNICODE_STRING </Type> <Flags> ALL_CLIENTS </Flags> </name>",
            &ctx,
        );
        assert_eq!(prop.data_type.stream_size(), -1);
        assert_eq!(prop.stream_size(), -1);

        // A custom header width is picked up by the member parse.
        let (prop, _) = parse_prop(
            "<name> <Type> UNICODE_STRING </Type> <Flags> ALL_CLIENTS </Flags> <VariableLengthHeaderSize> 2 </VariableLengthHeaderSize> </name>",
            &ctx,
        );
        assert_eq!(prop.stream_size(), -2);
    }

    #[test]
    fn digest_is_member_then_flags_then_type() {
        let ctx = TypeContext::default();
        let (prop, _) = parse_prop(
            "<hp> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </hp>",
            &ctx,
        );

        let mut md5 = Md5::new();
        prop.add_to_md5(&mut md5);

        let mut expected = Md5::new();
        expected.append_text("hp");
        expected.append_i32(
            (crate::flags::GHOSTED | crate::flags::OTHER_CLIENT | crate::flags::OWN_CLIENT) as i32,
        );
        // The INT32 type digest: "Int" + NUL, then the 4-byte size.
        expected.append_cstr("Int");
        expected.append_i32(4);
        assert_eq!(
            md5.quote(),
            expected.quote(),
            "member name, distribution flags, then the type digest"
        );
    }

    #[test]
    fn mailbox_types_refuse_client_travel() {
        let ctx = TypeContext::default();
        let (prop, _) = parse_prop(
            "<link> <Type> MAILBOX </Type> <Flags> BASE </Flags> </link>",
            &ctx,
        );
        assert!(prop.is_client_unusable());
        assert_eq!(prop.client_safety(), crate::flags::CLIENT_UNUSABLE);

        let (prop, _) = parse_prop(
            "<hp> <Type> INT32 </Type> <Flags> BASE </Flags> </hp>",
            &ctx,
        );
        assert!(!prop.is_client_unusable());
        assert_eq!(prop.client_safety(), crate::flags::CLIENT_SAFE);
    }

    #[test]
    fn full_name_includes_the_component() {
        let ctx = TypeContext::default();
        let (prop, _) = parse_prop(
            "<hp> <Type> INT32 </Type> <Flags> BASE </Flags> </hp>",
            &ctx,
        );
        assert_eq!(prop.full_name(), "Avatar.hp");

        let mut component_prop = prop.clone();
        component_prop.component_name = "movement".into();
        assert_eq!(component_prop.full_name(), "Avatar.movement.hp");
    }
}
