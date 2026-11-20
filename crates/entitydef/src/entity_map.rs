//! `entity_description_map.hpp` / `entity_description_map.cpp`: the whole
//! definition set, loaded from `scripts/entities.xml`.
//!
//! The load order matters for the type ids: client-server entities first,
//! then server-only ones, then services — the index loop afterwards stamps
//! `index`/`clientIndex` in vector order. The definition digest covers only
//! the client-visible types, in that same vector order.

use std::collections::BTreeMap;

use resources::datasection::DataSection;

use crate::constants;
use crate::data_type::{children, TypeContext};
use crate::diagnostics::Diagnostics;
use crate::digest::Md5;
use crate::entity_description::{
    DefResolver, DistributionDecider, EntityDescription, EntityTypeID, HasScriptDecider,
    INVALID_ENTITY_TYPE_ID,
};
use crate::method::ExposedMethodMessageRange;

/// One parsed definition set (`EntityDescriptionMap`).
#[derive(Debug, Default)]
pub struct EntityDescriptionMap {
    /// `vector_`: every entity and service, in load order.
    entities: Vec<EntityDescription>,
    /// `map_`: type name -> vector index. The reference's `std::map` is
    /// name-sorted; `BTreeMap` reproduces that iteration order.
    map: BTreeMap<String, EntityTypeID>,
    /// `digest_`.
    digest: String,
    /// `maxClientServerPropertyCount_` and friends.
    max_client_server_property_count: usize,
    max_exposed_client_method_count: usize,
    max_exposed_base_method_count: usize,
    max_exposed_cell_method_count: usize,
}

/// The message ranges a server build would pass in. `None` everywhere is the
/// dump-equivalent configuration (no message ids are assigned).
#[derive(Debug, Default, Clone, Copy)]
pub struct MessageRanges<'a> {
    /// Slot count of the client property range, when known.
    pub client_property_slots: Option<i32>,
    pub client: Option<&'a ExposedMethodMessageRange>,
    pub base: Option<&'a ExposedMethodMessageRange>,
    pub cell: Option<&'a ExposedMethodMessageRange>,
}

impl MessageRanges<'_> {
    fn any(&self) -> bool {
        self.client_property_slots.is_some()
            || self.client.is_some()
            || self.base.is_some()
            || self.cell.is_some()
    }
}

/// `ServiceDescriptionHasPythonScript`: services never face the client or a
/// cell; their base half lives under `scripts/service/`.
pub struct ServiceScriptDecider<'a> {
    resolver: &'a dyn DefResolver,
}

impl<'a> ServiceScriptDecider<'a> {
    /// Serve a parse from `resolver`'s tree.
    pub fn new(resolver: &'a dyn DefResolver) -> Self {
        Self { resolver }
    }
}

use crate::entity_description::EntityDistribution;

impl DistributionDecider for ServiceScriptDecider<'_> {
    fn can_be_on_cell(&self, _name: &str, _distr: &EntityDistribution) -> bool {
        false
    }

    fn can_be_on_base(&self, name: &str, distr: &EntityDistribution) -> bool {
        if let Some(tag) = distr.base_tag {
            return tag;
        }
        self.resolver
            .python_script_exists(&format!("{}/{name}", constants::ENTITIES_SERVICE_PATH))
    }

    fn can_be_on_client(&self, _name: &str, _distr: &EntityDistribution) -> bool {
        false
    }
}

impl EntityDescriptionMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// `size()`.
    pub fn size(&self) -> usize {
        self.entities.len()
    }

    /// `entityDescription( index )`.
    pub fn entity_description(&self, index: EntityTypeID) -> Option<&EntityDescription> {
        self.entities.get(index as usize)
    }

    pub fn entity_description_mut(
        &mut self,
        index: EntityTypeID,
    ) -> Option<&mut EntityDescription> {
        self.entities.get_mut(index as usize)
    }

    /// The load-order vector.
    pub fn entities(&self) -> &[EntityDescription] {
        &self.entities
    }

    /// `nameToIndex`.
    pub fn name_to_index(&self, name: &str) -> Option<EntityTypeID> {
        self.map.get(name).copied()
    }

    /// `digest()`: uppercase hex, as `MD5::Digest::quote()` renders it.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn max_client_server_property_count(&self) -> usize {
        self.max_client_server_property_count
    }

    pub fn max_exposed_client_method_count(&self) -> usize {
        self.max_exposed_client_method_count
    }

    pub fn max_exposed_base_method_count(&self) -> usize {
        self.max_exposed_base_method_count
    }

    pub fn max_exposed_cell_method_count(&self) -> usize {
        self.max_exposed_cell_method_count
    }

    /// `EntityDescriptionMap::parse`.
    #[allow(clippy::too_many_arguments)]
    pub fn parse(
        &mut self,
        section: Option<&DataSection>,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
        ranges: &MessageRanges,
    ) -> bool {
        let Some(section) = section else {
            diag.error("EntityDescriptionMap::parse: pSection is NULL\n".to_string());
            return false;
        };

        let mut is_okay = true;

        let services_section = resolver.open_section("scripts/services.xml");

        let client_server_entities = section.find_child("ClientServerEntities");
        let server_only_entities = section.find_child("ServerOnlyEntities");

        if let Some(client_server_entities) = client_server_entities {
            if let Some(server_only_entities) = server_only_entities {
                is_okay &= self.parse_internal(
                    client_server_entities,
                    &HasScriptDecider::with_client_script(resolver, true),
                    None,
                    resolver,
                    ctx,
                    diag,
                );

                is_okay &= self.parse_internal(
                    server_only_entities,
                    &HasScriptDecider::with_client_script(resolver, false),
                    None,
                    resolver,
                    ctx,
                    diag,
                );
            } else {
                diag.warning(
                    "EntityDescriptionMap::parse: scripts/entities.xml does not have a <ServerOnlyEntities> section.\n"
                        .to_string(),
                );

                is_okay &= self.parse_internal(
                    client_server_entities,
                    &HasScriptDecider::with_client_script(resolver, true),
                    None,
                    resolver,
                    ctx,
                    diag,
                );
            }
        } else {
            diag.notice(
                "EntityDescriptionMap::parse: scripts/entities.xml does not have a <ClientServerEntities> section. Reverting to old-style parsing where server-only entities types were detected by non-existence of client Python script file.\n"
                    .to_string(),
            );

            let mut server_only_descriptions = Vec::new();
            is_okay &= self.parse_internal(
                section,
                &HasScriptDecider::new(resolver),
                Some(&mut server_only_descriptions),
                resolver,
                ctx,
                diag,
            );

            self.entities.extend(server_only_descriptions);
        }

        for (i, desc) in self.entities.iter_mut().enumerate() {
            desc.set_index(i as EntityTypeID);
            self.map.insert(desc.name.clone(), desc.index());

            if desc.is_client_type() {
                desc.set_client_index(i as EntityTypeID);
            }
        }

        if is_okay {
            if let Some(services_section) = services_section {
                is_okay &= self.parse_services(&services_section, resolver, ctx, diag);
            }
        }

        // The ClientName feature is deprecated but still honoured.
        is_okay &= self.adjust_for_client_name(diag);

        self.set_exposed_message_ids(ranges, diag);

        if is_okay && ranges.any() {
            diag.info("Highest exposed counts:\n".to_string());

            if let Some(slots) = ranges.client_property_slots {
                is_okay &= self.check_count(
                    "client top-level property",
                    &EntityDescription::client_server_property_count,
                    slots,
                    256,
                    diag,
                );
            }

            if let Some(range) = ranges.client {
                let num_slots = range.num_slots();
                is_okay &= self.check_count(
                    "client method",
                    &EntityDescription::client_method_count,
                    num_slots,
                    256 * num_slots,
                    diag,
                );
            }

            if let Some(range) = ranges.base {
                let num_slots = range.num_slots();
                is_okay &= self.check_count(
                    "base method",
                    &EntityDescription::exposed_base_method_count,
                    num_slots,
                    256 * num_slots,
                    diag,
                );
            }

            if let Some(range) = ranges.cell {
                let num_slots = range.num_slots();
                is_okay &= self.check_count(
                    "cell method",
                    &EntityDescription::exposed_cell_method_count,
                    num_slots,
                    256 * num_slots,
                    diag,
                );
            }

            diag.info("\n".to_string());
        }

        let mut md5 = Md5::new();
        self.add_to_md5(&mut md5);
        self.digest = md5.quote();

        is_okay
    }

    /// `EntityDescriptionMap::parseInternal`.
    fn parse_internal(
        &mut self,
        section: &DataSection,
        decider: &dyn DistributionDecider,
        mut server_only: Option<&mut Vec<EntityDescription>>,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let mut is_okay = true;

        for sub_section in children(section) {
            let type_name = sub_section.section_name().to_string();
            let mut desc = EntityDescription::new();

            if desc.parse(&type_name, "", Some(decider), resolver, ctx, diag) {
                if !desc.is_client_type() {
                    match server_only.as_deref_mut() {
                        Some(server_only) => server_only.push(desc),
                        // The new-style split has no server-only bucket:
                        // entities.xml already made the decision.
                        None => self.entities.push(desc),
                    }
                } else {
                    self.entities.push(desc);
                }
            } else {
                diag.error(format!(
                    "EntityDescriptionMap: Failed to load or parse def for entity type {type_name}\n"
                ));
                is_okay = false;
            }
        }

        is_okay
    }

    /// `EntityDescriptionMap::parseServices`.
    fn parse_services(
        &mut self,
        section: &DataSection,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let decider = ServiceScriptDecider { resolver };

        let initial_type_id = self.entities.len();
        let size = section.count_children();
        self.entities
            .resize(initial_type_id + size, EntityDescription::new());

        for (i, sub_section) in children(section).enumerate() {
            let type_id = (initial_type_id + i) as EntityTypeID;
            let service_name = sub_section.section_name().to_string();
            let desc = &mut self.entities[type_id as usize];

            if !desc.parse_service(&service_name, "", Some(&decider), resolver, ctx, diag) {
                diag.error(format!(
                    "EntityDescriptionMap::parseServices: Failed to load or parse def for service {service_name}\n"
                ));
                return false;
            }

            let active_on_server_modes = sub_section.read_string("activeOnServerModes", "any");
            if active_on_server_modes != "any" {
                desc.set_active_on_server_modes(&active_on_server_modes);
            }

            desc.set_index(type_id);
            self.map.insert(desc.name.clone(), desc.index());
        }

        true
    }

    /// `EntityDescriptionMap::adjustForClientName`.
    fn adjust_for_client_name(&mut self, diag: &mut Diagnostics) -> bool {
        let mut is_okay = true;

        for i in 0..self.entities.len() {
            let (can_be_on_client, client_name, name, is_client_type) = {
                let desc = &self.entities[i];
                (
                    desc.can_be_on_client(),
                    desc.client_name().to_string(),
                    desc.name.clone(),
                    desc.is_client_type(),
                )
            };

            if can_be_on_client && !is_client_type {
                match self.name_to_index(&client_name) {
                    Some(id) => {
                        let alias = &self.entities[id as usize];

                        if alias.client_index() != INVALID_ENTITY_TYPE_ID {
                            if alias.client_server_property_count()
                                == self.entities[i].client_server_property_count()
                                && alias.client_method_count()
                                    == self.entities[i].client_method_count()
                            {
                                let alias_client_index = alias.client_index();
                                let alias_name = alias.name.clone();
                                self.entities[i].set_client_index(alias_client_index);
                                diag.info(format!("{name} is aliased as {alias_name}\n"));
                            } else {
                                diag.error(format!(
                                    "EntityDescriptionMap::parse: {name} has mismatched ClientName {client_name}\n"
                                ));
                                diag.error(format!(
                                    "EntityDescriptionMap::parse: There are {} methods and {} props instead of {} and {}\n",
                                    alias.client_method_count(),
                                    alias.client_server_property_count(),
                                    self.entities[i].client_method_count(),
                                    self.entities[i].client_server_property_count(),
                                ));
                                is_okay = false;
                            }
                        } else {
                            diag.error(format!(
                                "EntityDescriptionMap::parse: {name} has server-only ClientName {client_name}\n"
                            ));
                            is_okay = false;
                        }
                    }
                    None => {
                        diag.error(format!(
                            "EntityDescriptionMap::parse: {name} has invalid ClientName {client_name}\n"
                        ));
                        is_okay = false;
                    }
                }
            }
        }

        is_okay
    }

    /// `EntityDescriptionMap::setExposedMessageIDs`: the maxima first, then
    /// every entity gets its exposed ids assigned against them.
    fn set_exposed_message_ids(&mut self, ranges: &MessageRanges, diag: &mut Diagnostics) {
        for desc in &self.entities {
            self.max_client_server_property_count = self
                .max_client_server_property_count
                .max(desc.client_server_property_count());
            self.max_exposed_client_method_count = self
                .max_exposed_client_method_count
                .max(desc.client().num_exposed());
            self.max_exposed_base_method_count = self
                .max_exposed_base_method_count
                .max(desc.base().num_exposed());
            self.max_exposed_cell_method_count = self
                .max_exposed_cell_method_count
                .max(desc.cell().num_exposed());
        }

        let max_client = self.max_exposed_client_method_count as i32;
        let max_base = self.max_exposed_base_method_count as i32;
        let max_cell = self.max_exposed_cell_method_count as i32;

        for desc in &mut self.entities {
            desc.set_exposed_msg_ids(
                max_client,
                ranges.client,
                max_base,
                ranges.base,
                max_cell,
                ranges.cell,
                diag,
            );
        }
    }

    /// `EntityDescriptionMap::checkCount`.
    fn check_count(
        &self,
        description: &str,
        counter: &dyn Fn(&EntityDescription) -> usize,
        max_efficient: i32,
        max_allowed: i32,
        diag: &mut Diagnostics,
    ) -> bool {
        if self.entities.is_empty() {
            return true;
        }

        let mut best: &EntityDescription = &self.entities[0];
        let mut max_count = 0usize;

        for desc in &self.entities {
            let count = counter(desc);
            if count > max_count {
                best = desc;
                max_count = count;
            }
        }

        if max_count <= max_allowed as usize {
            diag.info(format!(
                "\t{}: {description} count = {max_count}. Efficient to {max_efficient} (limit is {max_allowed})\n",
                best.name
            ));
            true
        } else {
            diag.error(format!(
                "EntityDescriptionMap::checkCount: {} exposed {description} count of {max_count} is more than allowed ({max_allowed})\n",
                best.name
            ));
            false
        }
    }

    /// `EntityDescriptionMap::addToMD5`: client-visible types only, in
    /// vector order.
    pub fn add_to_md5(&self, md5: &mut Md5) {
        for desc in &self.entities {
            // Ignore the server side only ones.
            if desc.is_client_type() {
                desc.add_to_md5(md5);
            }
        }
    }

    /// Test/inspection helper: name -> index iteration in name order (the
    /// reference's `std::map` order).
    pub fn name_to_index_entries(&self) -> Vec<(&str, EntityTypeID)> {
        self.map
            .iter()
            .map(|(name, &index)| (name.as_str(), index))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::entity_description::FsResolver;

    const RES: &str = "scripts/entities.xml";
    const DEFS: &str = "scripts/entity_defs";

    /// In-memory resolver: files are (path, xml) pairs; python script
    /// existence is derived from `scripts/.../<Name>.py` entries.
    struct MemoryResolver {
        files: HashMap<String, Vec<u8>>,
    }

    impl MemoryResolver {
        fn new(files: &[(&str, &str)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(p, xml)| (p.to_string(), xml.as_bytes().to_vec()))
                    .collect(),
            }
        }
    }

    impl DefResolver for MemoryResolver {
        fn open_section(&self, path: &str) -> Option<DataSection> {
            let raw = self.files.get(path)?;
            resources::datasection::parse("", raw).ok()
        }

        fn python_script_exists(&self, path: &str) -> bool {
            self.files.contains_key(&format!("{path}.py"))
                || self.files.contains_key(&format!("{path}/__init__.py"))
        }
    }

    fn parse_map(
        resolver: &MemoryResolver,
        diag: &mut Diagnostics,
    ) -> (EntityDescriptionMap, bool) {
        let section = resolver.open_section(RES).expect("entities.xml");
        let mut map = EntityDescriptionMap::new();
        let ok = map.parse(
            Some(&section),
            resolver,
            &crate::data_type::TypeContext::default(),
            diag,
            &MessageRanges::default(),
        );
        (map, ok)
    }

    #[test]
    fn new_style_split_assigns_indexes_and_client_indexes() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Account/> <Avatar/> </ClientServerEntities> \
                     <ServerOnlyEntities> <Guard/> </ServerOnlyEntities> \
                 </root>",
            ),
            (&format!("{DEFS}/Account.def"), "<root/>"),
            (&format!("{DEFS}/Avatar.def"), "<root/>"),
            (&format!("{DEFS}/Guard.def"), "<root/>"),
        ]);

        let mut diag = Diagnostics::new();
        let (map, ok) = parse_map(&resolver, &mut diag);
        assert!(ok, "{:?}", diag.messages());

        assert_eq!(map.size(), 3);
        // Vector order: client-server entities then server-only.
        assert_eq!(map.entity_description(0).unwrap().name, "Account");
        assert_eq!(map.entity_description(1).unwrap().name, "Avatar");
        assert_eq!(map.entity_description(2).unwrap().name, "Guard");

        assert_eq!(map.entity_description(0).unwrap().index(), 0);
        assert_eq!(map.entity_description(1).unwrap().index(), 1);
        assert_eq!(map.entity_description(2).unwrap().index(), 2);

        // Client types carry clientIndex; the server-only one does not.
        assert_eq!(map.entity_description(0).unwrap().client_index(), 0);
        assert_eq!(map.entity_description(1).unwrap().client_index(), 1);
        assert_eq!(
            map.entity_description(2).unwrap().client_index(),
            INVALID_ENTITY_TYPE_ID
        );

        // Name lookups point at the vector slots.
        assert_eq!(map.name_to_index("Avatar"), Some(1));
        assert_eq!(map.name_to_index("Guard"), Some(2));
        assert_eq!(map.name_to_index("Nope"), None);

        // The map iterates in name order (std::map semantics).
        let names: Vec<&str> = map
            .name_to_index_entries()
            .iter()
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(names, vec!["Account", "Avatar", "Guard"]);
    }

    #[test]
    fn missing_server_only_section_warns_but_parses() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Account/> </ClientServerEntities> </root>",
            ),
            (&format!("{DEFS}/Account.def"), "<root/>"),
        ]);

        let mut diag = Diagnostics::new();
        let (map, ok) = parse_map(&resolver, &mut diag);
        assert!(ok);
        assert_eq!(map.size(), 1);
        assert!(diag.warnings().iter().any(|m| {
            m.contains("scripts/entities.xml does not have a <ServerOnlyEntities> section.")
        }));
    }

    #[test]
    fn old_style_falls_back_to_script_detection() {
        // No ClientServerEntities: every child is an entity, client presence
        // detected via scripts/client/<Name>.py.
        let resolver = MemoryResolver::new(&[
            (RES, "<root> <Account/> <Guard/> </root>"),
            (&format!("{DEFS}/Account.def"), "<root/>"),
            (&format!("{DEFS}/Guard.def"), "<root/>"),
            ("scripts/client/Account.py", "# client script"),
        ]);

        let mut diag = Diagnostics::new();
        let (map, ok) = parse_map(&resolver, &mut diag);
        assert!(ok, "{:?}", diag.messages());
        assert!(diag.messages().iter().any(|m| m
            .contains("Reverting to old-style parsing where server-only entities types were detected by non-existence of client Python script file.")));

        // Guard has no client script: it lands after Account and is not a
        // client type.
        assert_eq!(map.entity_description(0).unwrap().name, "Account");
        assert!(map.entity_description(0).unwrap().is_client_type());
        assert_eq!(map.entity_description(1).unwrap().name, "Guard");
        assert!(!map.entity_description(1).unwrap().is_client_type());
        assert_eq!(
            map.entity_description(1).unwrap().client_index(),
            INVALID_ENTITY_TYPE_ID
        );
    }

    #[test]
    fn digest_covers_client_types_in_vector_order_only() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <A/> </ClientServerEntities> \
                     <ServerOnlyEntities> <B/> </ServerOnlyEntities> \
                 </root>",
            ),
            (&format!("{DEFS}/A.def"), "<root/>"),
            (&format!("{DEFS}/B.def"), "<root/>"),
        ]);

        let mut diag = Diagnostics::new();
        let (map, ok) = parse_map(&resolver, &mut diag);
        assert!(ok);

        let mut expected = Md5::new();
        map.entity_description(0).unwrap().add_to_md5(&mut expected);

        assert_eq!(map.digest(), expected.quote());
    }

    #[test]
    fn client_name_aliasing_copies_the_client_index() {
        // Grunt and Guard have identical client surfaces; Grunt declares
        // ClientName Guard.
        let prop = "<hp> <Type> INT32 </Type> <Flags> BASE_AND_CLIENT </Flags> </hp>";
        let method = "<ping> <Arg> INT32 </Arg> </ping>";
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Guard/> <Grunt/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Guard.def"),
                format!(
                    "<root> <Properties> {prop} </Properties> <ClientMethods> {method} </ClientMethods> </root>"
                )
                .as_str(),
            ),
            (
                &format!("{DEFS}/Grunt.def"),
                format!(
                    "<root> <ClientName> Guard </ClientName> <Properties> {prop} </Properties> <ClientMethods> {method} </ClientMethods> </root>"
                )
                .as_str(),
            ),
        ]);

        let mut diag = Diagnostics::new();
        let (map, ok) = parse_map(&resolver, &mut diag);
        assert!(ok, "{:?}", diag.messages());
        assert!(diag
            .messages()
            .iter()
            .any(|m| m.contains("Grunt is aliased as Guard\n")));
        // Grunt adopts Guard's clientIndex (0) while keeping its own index.
        assert_eq!(map.entity_description(1).unwrap().client_index(), 0);
    }

    #[test]
    fn mismatched_client_name_errors() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Guard/> <Grunt/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (&format!("{DEFS}/Guard.def"), "<root/>"),
            (
                &format!("{DEFS}/Grunt.def"),
                "<root> \
                     <ClientName> Guard </ClientName> \
                     <Properties> \
                         <hp> <Type> INT32 </Type> <Flags> BASE_AND_CLIENT </Flags> </hp> \
                     </Properties> \
                 </root>",
            ),
        ]);

        let mut diag = Diagnostics::new();
        let (map, ok) = parse_map(&resolver, &mut diag);
        assert!(!ok);
        assert!(diag.errors().iter().any(|m| {
            m.contains("EntityDescriptionMap::parse: Grunt has mismatched ClientName Guard\n")
        }));
        assert!(diag
            .errors()
            .iter()
            .any(|m| m.contains("There are 0 methods and 0 props instead of 0 and 1\n")));
        assert_eq!(
            map.entity_description(1).unwrap().client_index(),
            INVALID_ENTITY_TYPE_ID
        );
    }

    #[test]
    fn server_only_and_invalid_client_names_error() {
        // Grunt aliases Boss, but Boss is server-only: clientIndex invalid.
        // Ghost aliases Nothing, which does not exist.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Grunt/> <Ghost/> </ClientServerEntities> \
                     <ServerOnlyEntities> <Boss/> </ServerOnlyEntities> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Grunt.def"),
                "<root> <ClientName> Boss </ClientName> </root>",
            ),
            (
                &format!("{DEFS}/Ghost.def"),
                "<root> <ClientName> Nothing </ClientName> </root>",
            ),
            (&format!("{DEFS}/Boss.def"), "<root/>"),
        ]);

        let mut diag = Diagnostics::new();
        let (_, ok) = parse_map(&resolver, &mut diag);
        assert!(!ok);
        assert!(diag.errors().iter().any(|m| {
            m.contains("EntityDescriptionMap::parse: Grunt has server-only ClientName Boss\n")
        }));
        assert!(diag.errors().iter().any(|m| {
            m.contains("EntityDescriptionMap::parse: Ghost has invalid ClientName Nothing\n")
        }));
    }

    #[test]
    fn null_section_is_an_error() {
        let mut map = EntityDescriptionMap::new();
        let mut diag = Diagnostics::new();
        let resolver = MemoryResolver::new(&[]);
        assert!(!map.parse(
            None,
            &resolver,
            &crate::data_type::TypeContext::default(),
            &mut diag,
            &MessageRanges::default(),
        ));
        assert_eq!(
            diag.errors(),
            vec!["EntityDescriptionMap::parse: pSection is NULL\n"]
        );
    }

    #[test]
    fn failed_def_parse_reports_the_type() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Broken/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            // No .def file at all -> parse failure.
        ]);

        let mut diag = Diagnostics::new();
        let (_, ok) = parse_map(&resolver, &mut diag);
        assert!(!ok);
        assert!(diag.errors().iter().any(|m| m.contains(
            "EntityDescriptionMap: Failed to load or parse def for entity type Broken\n"
        )));
    }

    #[test]
    fn services_parse_after_entities_and_register_indexes() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Account/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (&format!("{DEFS}/Account.def"), "<root/>"),
            ("scripts/services.xml", "<root> <Debugger/> </root>"),
            (
                "scripts/service_defs/Debugger.def",
                "<root> <Methods> <attach> </attach> </Methods> </root>",
            ),
            ("scripts/service/Debugger.py", "# service script"),
        ]);

        let mut diag = Diagnostics::new();
        let section = resolver.open_section(RES).unwrap();
        let mut map = EntityDescriptionMap::new();
        let ok = map.parse(
            Some(&section),
            &resolver,
            &crate::data_type::TypeContext::default(),
            &mut diag,
            &MessageRanges::default(),
        );
        assert!(ok, "{:?}", diag.messages());

        // The service lands after the entities, never a client type.
        let service = map.entity_description(1).unwrap();
        assert_eq!(service.name, "Debugger");
        assert!(service.is_service());
        assert!(!service.is_client_type());
        assert!(!service.is_persistent(), "services are not persistent");
        assert_eq!(service.index(), 1);
        assert_eq!(map.name_to_index("Debugger"), Some(1));
        assert_eq!(service.base().size(), 1);
    }

    #[test]
    fn services_respect_active_on_server_modes() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities/> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                "scripts/services.xml",
                "<root> \
                     <Cleaner/> \
                     <Watcher> <activeOnServerModes> baseapp </activeOnServerModes> </Watcher> \
                 </root>",
            ),
            ("scripts/service_defs/Cleaner.def", "<root/>"),
            ("scripts/service/Cleaner.py", "# service script"),
            ("scripts/service_defs/Watcher.def", "<root/>"),
            ("scripts/service/Watcher.py", "# service script"),
        ]);

        let mut diag = Diagnostics::new();
        let section = resolver.open_section(RES).unwrap();
        let mut map = EntityDescriptionMap::new();
        assert!(map.parse(
            Some(&section),
            &resolver,
            &crate::data_type::TypeContext::default(),
            &mut diag,
            &MessageRanges::default(),
        ));

        assert_eq!(
            map.entity_description(0).unwrap().active_on_server_modes(),
            ""
        );
        assert_eq!(
            map.entity_description(1).unwrap().active_on_server_modes(),
            "baseapp"
        );
    }

    /// The committed service fixture
    /// (`docs/review/fixtures/service_def`): a service's public method list is
    /// its `<Methods>` section plus the methods of its `<Implements>` chain, in
    /// that order, and services never gain cell or client methods.
    #[test]
    fn a_service_lists_its_public_methods_from_the_methods_section() {
        let root = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/review/fixtures/service_def/res"
        ));
        let resolver = crate::entity_description::FsResolver::new(root);
        let mut ctx = crate::data_type::TypeContext::default();
        let mut diag = Diagnostics::new();
        ctx.init_aliases(
            || resolver.open_section(crate::constants::ALIASES_FILE),
            &mut diag,
        );

        let entities = resolver
            .open_section(crate::constants::ENTITIES_FILE)
            .expect("the fixture has scripts/entities.xml");
        let mut map = EntityDescriptionMap::new();
        assert!(
            map.parse(
                Some(&entities),
                &resolver,
                &ctx,
                &mut diag,
                &MessageRanges::default(),
            ),
            "{:?}",
            diag.messages()
        );

        // Services load after every entity, so their ids start at 2 here.
        assert_eq!(map.name_to_index("Cleaner"), Some(2));
        let cleaner = map.entity_description(2).expect("the service exists");
        assert!(cleaner.is_service());
        assert!(!cleaner.can_be_on_cell());
        assert!(!cleaner.can_be_on_client());
        assert!(cleaner.can_be_on_base());

        let methods = cleaner.base().internal_methods();
        assert_eq!(
            methods.iter().map(|m| m.name()).collect::<Vec<_>>(),
            // The interface method precedes the service's own.
            ["vacuum", "sweep", "tally"]
        );
        assert_eq!(
            methods.iter().map(|m| m.is_exposed()).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(cleaner.cell().size(), 0, "services have no cell half");
        assert_eq!(cleaner.client().size(), 0, "services have no client half");
        assert_eq!(
            cleaner.property_count(),
            0,
            "service properties are dropped"
        );
    }

    #[test]
    fn service_without_script_fails() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities/> <ServerOnlyEntities/> </root>",
            ),
            ("scripts/services.xml", "<root> <Lost/> </root>"),
            ("scripts/service_defs/Lost.def", "<root/>"),
            // No scripts/service/Lost.py.
        ]);

        let mut diag = Diagnostics::new();
        let section = resolver.open_section(RES).unwrap();
        let mut map = EntityDescriptionMap::new();
        assert!(!map.parse(
            Some(&section),
            &resolver,
            &crate::data_type::TypeContext::default(),
            &mut diag,
            &MessageRanges::default(),
        ));
        assert!(diag
            .errors()
            .iter()
            .any(|m| m.contains("Service 'Lost' does not have a service script.\n")));
    }

    #[test]
    fn fs_resolver_serves_files_from_a_root() {
        let dir = std::env::temp_dir().join(format!("entitydef_map_test_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/entities.xml"), b"<root/>").unwrap();

        let resolver = FsResolver::new(&dir);
        assert!(resolver.open_section("scripts/entities.xml").is_some());
        assert!(resolver.open_section("scripts/missing.xml").is_none());
        assert!(!resolver.python_script_exists("scripts/base/Account"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A server-build parse with real message ranges reports the highest
    /// exposed counts per channel.
    #[test]
    fn known_ranges_report_the_highest_exposed_counts() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Account/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Account.def"),
                "<root> \
                     <Properties> \
                         <gold> <Type> INT32 </Type> <Flags> CELL_PUBLIC_AND_OWN </Flags> </gold> \
                     </Properties> \
                     <ClientMethods> <ping> <Arg> INT32 </Arg> </ping> </ClientMethods> \
                     <BaseMethods> <createCell> <Exposed/> </createCell> </BaseMethods> \
                     <CellMethods> <tick> <Exposed/> </tick> </CellMethods> \
                 </root>",
            ),
            ("scripts/base/Account.py", "# base script"),
        ]);

        let client_range = ExposedMethodMessageRange::new(100, 130);
        let base_range = ExposedMethodMessageRange::new(200, 230);
        let cell_range = ExposedMethodMessageRange::new(300, 330);
        let ranges = MessageRanges {
            client_property_slots: Some(31),
            client: Some(&client_range),
            base: Some(&base_range),
            cell: Some(&cell_range),
        };

        let mut diag = Diagnostics::new();
        let section = resolver.open_section(RES).unwrap();
        let mut map = EntityDescriptionMap::new();
        assert!(
            map.parse(
                Some(&section),
                &resolver,
                &crate::data_type::TypeContext::default(),
                &mut diag,
                &ranges,
            ),
            "{:?}",
            diag.messages()
        );

        let stored = diag.messages();
        let messages = stored.iter().map(String::as_str).collect::<Vec<_>>();
        assert!(messages.contains(&"Highest exposed counts:\n"));
        assert!(messages.iter().any(|m| m.contains(
            "\tAccount: client top-level property count = 1. Efficient to 31 (limit is 256)\n"
        )));
        assert!(messages.iter().any(|m| {
            m.contains("\tAccount: client method count = 1. Efficient to 31 (limit is 7936)\n")
        }));
        assert!(messages.iter().any(|m| {
            m.contains("\tAccount: base method count = 1. Efficient to 31 (limit is 7936)\n")
        }));
        assert!(messages.iter().any(|m| {
            m.contains("\tAccount: cell method count = 1. Efficient to 31 (limit is 7936)\n")
        }));
        assert!(messages.last().copied() == Some("\n"));

        // The maxima ride along with the range pass.
        assert_eq!(map.max_client_server_property_count(), 1);
        assert_eq!(map.max_exposed_client_method_count(), 1);
        assert_eq!(map.max_exposed_base_method_count(), 1);
        assert_eq!(map.max_exposed_cell_method_count(), 1);

        // The load-order view and the mutable accessor stay in step.
        assert_eq!(map.entities().len(), 1);
        assert_eq!(map.entities()[0].name, "Account");
        assert_eq!(
            map.entity_description_mut(0).map(|d| d.name.as_str()),
            Some("Account")
        );
        assert!(map.entity_description_mut(9).is_none());
    }

    /// A client-server property count over 256 fails the whole parse.
    #[test]
    fn too_many_client_server_properties_fail_the_map() {
        let mut properties = String::from("<root> <Properties>");
        for i in 0..257 {
            properties.push_str(&format!(
                "<p{i}> <Type> INT32 </Type> <Flags> CELL_PUBLIC_AND_OWN </Flags> </p{i}>"
            ));
        }
        properties.push_str("</Properties> </root>");

        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Avatar/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (&format!("{DEFS}/Avatar.def"), properties.as_str()),
        ]);

        let ranges = MessageRanges {
            client_property_slots: Some(31),
            ..MessageRanges::default()
        };

        let mut diag = Diagnostics::new();
        let section = resolver.open_section(RES).unwrap();
        let mut map = EntityDescriptionMap::new();
        assert!(!map.parse(
            Some(&section),
            &resolver,
            &crate::data_type::TypeContext::default(),
            &mut diag,
            &ranges,
        ));
        assert!(diag.errors().iter().any(|m| m.contains(
            "EntityDescriptionMap::checkCount: Avatar exposed client top-level property count of 257 is more than allowed (256)\n"
        )));
    }
}
