//! `entity_description.hpp/.cpp` plus the `BaseUserDataObjectDescription`
//! parse path (`base_user_data_object_description.cpp`): one entity type.
//!
//! Parsing is layered: `<Parent>` recursion parses into the *same* object,
//! `<Implements>` interfaces add their members on top, and property overrides
//! reuse the original slot so indices stay stable. The file access goes
//! through [`DefResolver`] so a res tree can live anywhere (tests use a temp
//! directory).

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use resources::datasection::DataSection;

use crate::constants;
use crate::data_description::DataDescription;
use crate::data_type::{children, TypeContext};
use crate::diagnostics::Diagnostics;
use crate::digest::Md5;
use crate::flags::{EXACT_MATCH, ONLY_OTHER_CLIENT_DATA, ONLY_PERSISTENT_DATA};
use crate::lod::{DataLoDLevels, MAX_DATA_LOD_LEVELS, NO_LEVEL, OUTER_LEVEL};
use crate::method::{Component, EntityMethodDescriptions, MethodDescription};
use crate::volatile::VolatileInfo;

/// `EntityTypeID` (`network/basictypes.hpp`).
pub type EntityTypeID = u16;

/// `INVALID_ENTITY_TYPE_ID`.
pub const INVALID_ENTITY_TYPE_ID: EntityTypeID = EntityTypeID::MAX;

/// Access to the res tree the definitions live in (`BWResource`).
pub trait DefResolver {
    /// `BWResource::openSection( path )`.
    fn open_section(&self, path: &str) -> Option<DataSection>;

    /// `pythonScriptExists( path )`: `<path>.py` or `<path>/__init__.py`.
    fn python_script_exists(&self, path: &str) -> bool;
}

/// A resolver rooted at a real directory on disk.
pub struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn full_path(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }
}

impl DefResolver for FsResolver {
    fn open_section(&self, path: &str) -> Option<DataSection> {
        let raw = std::fs::read(self.full_path(path)).ok()?;
        resources::datasection::parse("", &raw).ok()
    }

    fn python_script_exists(&self, path: &str) -> bool {
        self.full_path(&format!("{path}.py")).exists()
            || self.full_path(path).join("__init__.py").exists()
    }
}

/// Resolvers are threaded as `&dyn DefResolver`; let plain references act as
/// resolvers too so trait objects compose.
impl<T: DefResolver + ?Sized> DefResolver for &T {
    fn open_section(&self, path: &str) -> Option<DataSection> {
        (**self).open_section(path)
    }

    fn python_script_exists(&self, path: &str) -> bool {
        (**self).python_script_exists(path)
    }
}

/// The `<Distribution>` tags (`EntityDistribution`): `None` ("Unspecified")
/// only when the whole `<Distribution>` section is missing.
#[derive(Debug, Clone, Copy, Default)]
pub struct EntityDistribution {
    pub client_tag: Option<bool>,
    pub base_tag: Option<bool>,
    pub cell_tag: Option<bool>,
}

/// `readBool( name )`: the reference treats a present-but-empty tag as false.
fn read_tag(section: &DataSection, name: &str) -> bool {
    section
        .find_child(name)
        .is_some_and(|child| child.as_bool(false))
}

impl EntityDistribution {
    fn parse(section: Option<&DataSection>) -> Self {
        // `readBool( "Cell" )` and friends: a `<Distribution>` section decides
        // all three roles, so a missing tag means `false` rather than
        // "unspecified".
        match section {
            Some(section) => Self {
                client_tag: Some(read_tag(section, "Client")),
                base_tag: Some(read_tag(section, "Base")),
                cell_tag: Some(read_tag(section, "Cell")),
            },
            None => Self::default(),
        }
    }
}

/// `IEntityDistributionDecider`: decides which machines an entity can live on.
pub trait DistributionDecider {
    fn can_be_on_cell(&self, name: &str, distr: &EntityDistribution) -> bool;
    fn can_be_on_base(&self, name: &str, distr: &EntityDistribution) -> bool;
    fn can_be_on_client(&self, name: &str, distr: &EntityDistribution) -> bool;
}

/// `HasScriptOrTagDistributionDecider`: script-file existence, with an
/// optional hard override of the client answer (the entities.xml split).
pub struct HasScriptDecider<R> {
    resolver: R,
    /// `EntityDescriptionHasClientScript` override; `None` checks the file.
    has_client_script: Option<bool>,
}

impl<R: DefResolver> HasScriptDecider<R> {
    pub fn new(resolver: R) -> Self {
        Self {
            resolver,
            has_client_script: None,
        }
    }

    /// The `EntityDescriptionHasClientScript( bool )` variant.
    pub fn with_client_script(resolver: R, has_client_script: bool) -> Self {
        Self {
            resolver,
            has_client_script: Some(has_client_script),
        }
    }

    fn file_exists(&self, path: &str, name: &str) -> bool {
        self.resolver
            .python_script_exists(&format!("{path}/{name}"))
    }
}

impl<R: DefResolver> DistributionDecider for HasScriptDecider<R> {
    fn can_be_on_cell(&self, name: &str, distr: &EntityDistribution) -> bool {
        if let Some(tag) = distr.cell_tag {
            return tag;
        }
        self.file_exists(constants::ENTITIES_CELL_PATH, name)
    }

    fn can_be_on_base(&self, name: &str, distr: &EntityDistribution) -> bool {
        if let Some(tag) = distr.base_tag {
            return tag;
        }
        self.file_exists(constants::ENTITIES_BASE_PATH, name)
    }

    fn can_be_on_client(&self, name: &str, distr: &EntityDistribution) -> bool {
        if let Some(tag) = distr.client_tag {
            return tag;
        }
        match self.has_client_script {
            Some(script) => script,
            None => self.file_exists(constants::ENTITIES_CLIENT_PATH, name),
        }
    }
}

/// One entity type (`EntityDescription`).
#[derive(Debug, Clone)]
pub struct EntityDescription {
    /// `name_`.
    pub name: String,
    /// `clientName_`.
    client_name: String,
    /// `index_`.
    index: EntityTypeID,
    /// `clientIndex_`.
    client_index: EntityTypeID,
    /// `canBeOnCell_` etc.
    can_be_on_cell: bool,
    can_be_on_base: bool,
    can_be_on_client: bool,
    /// `hasComponents_`.
    has_components: bool,
    /// `isService_`.
    is_service: bool,
    /// `isPersistent_`.
    is_persistent: bool,
    /// `forceExplicitDBID_`.
    force_explicit_dbid: bool,
    /// `volatileInfo_`.
    pub volatile_info: VolatileInfo,
    /// `properties_`, in declaration order with overrides in place.
    properties: Vec<DataDescription>,
    /// `propertyMap_`: component -> property name -> index.
    property_map: HashMap<String, HashMap<String, usize>>,
    /// `componentNames_`.
    component_names: BTreeSet<String>,
    /// `clientServerProperties_`: property indices, reordered to match the
    /// client-server full indexes.
    client_server_properties: Vec<usize>,
    /// `base_` / `cell_` / `client_`.
    base: EntityMethodDescriptions,
    cell: EntityMethodDescriptions,
    client: EntityMethodDescriptions,
    /// `tempProperties_`.
    temp_properties: BTreeSet<String>,
    /// `numEventStampedProperties_`.
    num_event_stamped_properties: u32,
    /// `numLatestChangeOnlyMembers_`.
    num_latest_change_only_members: u32,
    /// `appealRadius_` (squared on read? No: stored raw, compared squared).
    appeal_radius: f32,
    /// `shouldSendDetailedPosition_`.
    should_send_detailed_position: bool,
    /// `isManualAoI_`.
    is_manual_aoi: bool,
    /// `activeOnServerModes_` (services only).
    active_on_server_modes: String,
    /// `lodLevels_`.
    lod_levels: DataLoDLevels,
}

impl Default for EntityDescription {
    fn default() -> Self {
        Self {
            name: String::new(),
            client_name: String::new(),
            index: INVALID_ENTITY_TYPE_ID,
            client_index: INVALID_ENTITY_TYPE_ID,
            can_be_on_cell: true,
            can_be_on_base: true,
            can_be_on_client: true,
            has_components: false,
            is_service: false,
            is_persistent: true,
            force_explicit_dbid: false,
            volatile_info: VolatileInfo::default(),
            properties: Vec::new(),
            property_map: HashMap::new(),
            component_names: BTreeSet::new(),
            client_server_properties: Vec::new(),
            base: EntityMethodDescriptions::new(),
            cell: EntityMethodDescriptions::new(),
            client: EntityMethodDescriptions::new(),
            temp_properties: BTreeSet::new(),
            num_event_stamped_properties: 0,
            num_latest_change_only_members: 0,
            appeal_radius: 0.0,
            should_send_detailed_position: false,
            is_manual_aoi: false,
            active_on_server_modes: String::new(),
            lod_levels: DataLoDLevels::new(),
        }
    }
}

impl EntityDescription {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn client_name(&self) -> &str {
        &self.client_name
    }

    pub fn index(&self) -> EntityTypeID {
        self.index
    }

    pub fn set_index(&mut self, index: EntityTypeID) {
        self.index = index;
    }

    pub fn client_index(&self) -> EntityTypeID {
        self.client_index
    }

    pub fn set_client_index(&mut self, client_index: EntityTypeID) {
        self.client_index = client_index;
    }

    pub fn can_be_on_cell(&self) -> bool {
        self.can_be_on_cell
    }

    pub fn can_be_on_base(&self) -> bool {
        self.can_be_on_base
    }

    pub fn can_be_on_client(&self) -> bool {
        self.can_be_on_client
    }

    pub fn is_service(&self) -> bool {
        self.is_service
    }

    pub fn is_persistent(&self) -> bool {
        self.is_persistent
    }

    /// `activeOnServerModes()` / `setActiveOnServerModes()` (services).
    pub fn active_on_server_modes(&self) -> &str {
        &self.active_on_server_modes
    }

    pub fn set_active_on_server_modes(&mut self, modes: &str) {
        self.active_on_server_modes = modes.to_string();
    }

    /// `isClientType()`: the client sees this type under its own name.
    pub fn is_client_type(&self) -> bool {
        self.name == self.client_name
    }

    /// `isClientOnlyType()`: no cell and no base presence.
    pub fn is_client_only_type(&self) -> bool {
        !self.can_be_on_cell && !self.can_be_on_base
    }

    pub fn property_count(&self) -> usize {
        self.properties.len()
    }

    pub fn property(&self, n: usize) -> Option<&DataDescription> {
        self.properties.get(n)
    }

    pub fn properties(&self) -> &[DataDescription] {
        &self.properties
    }

    pub fn client_server_property_count(&self) -> usize {
        self.client_server_properties.len()
    }

    pub fn client_method_count(&self) -> usize {
        self.client.size()
    }

    /// `exposedBaseMethodCount()` / `exposedCellMethodCount()`.
    pub fn exposed_base_method_count(&self) -> usize {
        self.base.num_exposed()
    }

    pub fn exposed_cell_method_count(&self) -> usize {
        self.cell.num_exposed()
    }

    pub fn base(&self) -> &EntityMethodDescriptions {
        &self.base
    }

    pub fn cell(&self) -> &EntityMethodDescriptions {
        &self.cell
    }

    pub fn client(&self) -> &EntityMethodDescriptions {
        &self.client
    }

    /// `parseService`: the service flavour of `parse`.
    pub fn parse_service(
        &mut self,
        name: &str,
        component_name: &str,
        decider: Option<&dyn DistributionDecider>,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        self.is_service = true;
        self.parse(name, component_name, decider, resolver, ctx, diag)
    }

    /// `EntityDescription::parse`.
    pub fn parse(
        &mut self,
        name: &str,
        component_name: &str,
        decider: Option<&dyn DistributionDecider>,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let filename = format!("{}/{}.def", constants::defs_dir(self.is_service), name);
        let Some(section) = resolver.open_section(&filename) else {
            diag.error(format!(
                "EntityDescription::parse: Could not open {filename}\n"
            ));
            return false;
        };

        let parent_name = section.read_string("Parent", "");

        if !parent_name.is_empty() {
            if !self.parse(
                parent_name.as_str(),
                component_name,
                None,
                resolver,
                ctx,
                diag,
            ) {
                diag.error(format!(
                    "EntityDescription::parse: Could not parse {parent_name}, parent of {name}\n"
                ));
                return false;
            }
        }

        self.name = name.to_string();

        if !self.is_service {
            // A different client-side class name; sticky when absent.
            self.client_name = section.read_string("ClientName", &self.client_name);
        }

        if let Some(decider) = decider {
            let distr = EntityDistribution::parse(section.open_section("Distribution"));

            self.can_be_on_cell = decider.can_be_on_cell(name, &distr);
            self.can_be_on_base = decider.can_be_on_base(name, &distr);
            self.can_be_on_client = decider.can_be_on_client(name, &distr);

            if !self.can_be_on_client {
                self.client_name = String::new();
            } else if self.client_name.is_empty() {
                self.client_name = name.to_string();
            }
        }

        let mut result = self.parse_interface(&section, name, component_name, resolver, ctx, diag);

        if decider.is_some() && !self.is_service {
            // LoDs can be defined in any order and derived interfaces can add
            // more, so the recorded insertion index is now mapped through the
            // sorted levels.
            let mut levels = [0i32; MAX_DATA_LOD_LEVELS + 1];
            for (i, level) in levels.iter_mut().enumerate() {
                *level = self.lod_levels.get_level(i as i32).map_or(0, |l| l.index());
            }

            for property in &mut self.properties {
                match property.detail_level {
                    NO_LEVEL => {}
                    OUTER_LEVEL => {
                        property.detail_level = self.lod_levels.size() - 1;
                    }
                    recorded => {
                        property.detail_level = levels[recorded as usize];
                    }
                }
            }
        }

        // Entities without cell scripts must not carry cell data (checked
        // with the client-only escape hatch).
        if !self.can_be_on_cell && !self.is_client_only_type() {
            for property in &self.properties {
                if property.data_flags.is_cell_data() {
                    diag.warning(format!(
                        "Entity '{}' does not have a cell script but has cell property '{}'.\n",
                        self.name,
                        property.name()
                    ));
                    break;
                }
            }
        }

        if result && !self.can_be_on_cell && self.cell.size() > 0 {
            diag.warning(format!(
                "Entitiy '{}' does not have a cell script but has cell method '{}'\n",
                self.name,
                self.cell
                    .internal_method(0)
                    .map_or("?", MethodDescription::name)
            ));
        }

        if result && !self.can_be_on_base {
            if self.is_service {
                diag.error(format!(
                    "Service '{}' does not have a service script.\n",
                    self.name
                ));
                result = false;
            } else if self.base.size() > 0 {
                diag.warning(format!(
                    "Entity '{}' does not have a base script but has base method '{}'\n",
                    self.name,
                    self.base
                        .internal_method(0)
                        .map_or("?", MethodDescription::name)
                ));
            }
        }

        if result && !self.can_be_on_client && self.client.size() > 0 {
            diag.error(format!(
                "Entity '{}' does not have a client script but has client methods\n",
                self.name
            ));
            result = false;
        }

        if self.is_service || self.is_client_only_type() {
            self.is_persistent = false;
        }

        result
    }

    /// `EntityDescription::parseInterface`, including the
    /// `BaseUserDataObjectDescription` property/interface steps.
    fn parse_interface(
        &mut self,
        section: &DataSection,
        interface_name: &str,
        component_name: &str,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        self.force_explicit_dbid =
            crate::data_type::read_bool(section, "ExplicitDatabaseID", self.force_explicit_dbid);

        let mut result = true;

        if !self.is_service {
            self.is_persistent =
                crate::data_type::read_bool(section, "Persistent", self.is_persistent);

            result &= self
                .lod_levels
                .add_levels(section.open_section("LoDLevels"), diag);

            // BaseUserDataObjectDescription::parseInterface uses &&, so a
            // failed <Implements> skips <Properties>.
            result &= self.parse_implements(
                section.open_section("Implements"),
                component_name,
                resolver,
                ctx,
                diag,
            ) && self.parse_properties(
                section.open_section("Properties"),
                component_name,
                ctx,
                diag,
            );

            result &= self
                .volatile_info
                .parse(section.open_section("Volatile"), diag);

            // `readFloat( "AppealRadius", appealRadius_ )`.
            self.appeal_radius = section
                .open_section("AppealRadius")
                .map_or(self.appeal_radius, |s| s.as_float(self.appeal_radius));

            self.should_send_detailed_position = crate::data_type::read_bool(
                section,
                "ShouldSendDetailedVolatilePosition",
                self.should_send_detailed_position,
            );

            if let Some(manual_aoi) = section.find_child("IsManualAOI") {
                diag.warning(format!(
                    "EntityDescription::parse: For entity type {}, IsManualAOI is deprecated, use IsManualAoI instead\n",
                    self.name
                ));
                self.is_manual_aoi = manual_aoi.as_bool(false);
            }

            // Overrides any deprecated-tag setting from above.
            self.is_manual_aoi =
                crate::data_type::read_bool(section, "IsManualAoI", self.is_manual_aoi);
        } else {
            result &= self.parse_implements(
                section.open_section("Implements"),
                component_name,
                resolver,
                ctx,
                diag,
            );
        }

        result &= self.parse_methods(section, interface_name, component_name, ctx, diag);

        result &= self.parse_temp_properties(section.open_section("TempProperties"));

        result &= self.parse_components(
            section.open_section("Components"),
            interface_name,
            resolver,
            ctx,
            diag,
        );

        result
    }

    /// `BaseUserDataObjectDescription::parseImplements`.
    fn parse_implements(
        &mut self,
        interfaces: Option<&DataSection>,
        component_name: &str,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let Some(interfaces) = interfaces else {
            return true;
        };

        let mut result = true;

        for child in children(interfaces) {
            let interface_name = child.as_string();

            let filename = format!(
                "{}/interfaces/{interface_name}.def",
                constants::defs_dir(self.is_service)
            );
            let interface = resolver.open_section(&filename);

            if !self.parse_interface_for_implements(
                interface.as_ref(),
                &interface_name,
                component_name,
                resolver,
                ctx,
                diag,
            ) {
                diag.error(format!(
                    "BaseUserDataObjectDescription::parseImplements: Failed parsing interface {interface_name}\n"
                ));
                result = false;
            }
        }

        result
    }

    /// The interface body: the same step order as an entity, entered through
    /// `parseInterface` with the interface's own name.
    fn parse_interface_for_implements(
        &mut self,
        section: Option<&DataSection>,
        interface_name: &str,
        component_name: &str,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let Some(section) = section else {
            return false;
        };
        self.parse_interface(section, interface_name, component_name, resolver, ctx, diag)
    }

    /// `EntityDescription::parseComponents`.
    fn parse_components(
        &mut self,
        components: Option<&DataSection>,
        interface_name: &str,
        resolver: &dyn DefResolver,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let Some(components) = components else {
            return true;
        };

        for component in children(components) {
            let component_type = component.section_name().to_string();

            let filename = constants::component_def_file(&component_type);
            let Some(component_file) = resolver.open_section(&filename) else {
                diag.error(format!(
                    "EntityDescription::parseComponents: Failed to open component def file '{filename}'.\n"
                ));
                return false;
            };

            let component_name = component.read_string("name", &component_type);

            if !self.component_names.insert(component_name.clone()) {
                diag.error(format!(
                    "EntityDescription::parseComponents: Component with name '{component_name}' is duplicated.\n"
                ));
                return false;
            }

            if !self.parse_component(&component_file, interface_name, &component_name, ctx, diag) {
                diag.error(format!(
                    "EntityDescription::parseComponents: Failed to parse component '{component_name}' for entity '{interface_name}'.\n"
                ));
                return false;
            }
        }

        true
    }

    /// `EntityDescription::parseComponent`.
    fn parse_component(
        &mut self,
        section: &DataSection,
        interface_name: &str,
        component_name: &str,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        diag.info(format!(
            "EntityDescription::parseComponent: adding component '{}' to definition for entity '{}'.\n",
            component_name, interface_name
        ));

        self.has_components = true;

        let mut result = self.parse_properties(
            section.open_section("Properties"),
            component_name,
            ctx,
            diag,
        );

        result &= self.parse_temp_properties(section.open_section("TempProperties"));

        result &= self.parse_methods(section, interface_name, component_name, ctx, diag);

        result
    }

    // (resolver threading ends at parse_components; components carry no
    // further file lookups of their own)

    /// `EntityDescription::parseMethods` (base/cell/client dispatch).
    fn parse_methods(
        &mut self,
        section: &DataSection,
        interface_name: &str,
        component_name: &str,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let base_tag = if self.is_service {
            "Methods"
        } else {
            "BaseMethods"
        };
        let mut result = self.init_methods(
            MethodTarget::Base,
            section.open_section(base_tag),
            interface_name,
            component_name,
            ctx,
            diag,
        );

        if !self.is_service {
            result &= self.init_methods(
                MethodTarget::Cell,
                section.open_section("CellMethods"),
                interface_name,
                component_name,
                ctx,
                diag,
            );

            result &= self.init_methods(
                MethodTarget::Client,
                section.open_section("ClientMethods"),
                interface_name,
                component_name,
                ctx,
                diag,
            );
        }

        result
    }

    fn init_methods(
        &mut self,
        target: MethodTarget,
        methods: Option<&DataSection>,
        interface_name: &str,
        component_name: &str,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        if methods.is_none() {
            return true;
        }

        let mut counter = self.num_latest_change_only_members;
        let outcome = match target {
            MethodTarget::Base => self.base.init(
                methods,
                Component::Base,
                interface_name,
                component_name,
                None,
                ctx,
                diag,
            ),
            MethodTarget::Cell => self.cell.init(
                methods,
                Component::Cell,
                interface_name,
                component_name,
                None,
                ctx,
                diag,
            ),
            // Only client methods consume latest-event slots.
            MethodTarget::Client => self.client.init(
                methods,
                Component::Client,
                interface_name,
                component_name,
                Some(&mut counter),
                ctx,
                diag,
            ),
        };
        self.num_latest_change_only_members = counter;
        outcome
    }

    /// `EntityDescription::parseProperties`.
    fn parse_properties(
        &mut self,
        properties: Option<&DataSection>,
        component_name: &str,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let Some(properties) = properties else {
            return true;
        };

        if self.is_service {
            diag.warning(format!(
                "EntityDescription::parseProperties: {}: Services do not support properties.\n",
                self.name
            ));
            return true;
        }

        let mut properties_ok = true;

        for section in children(properties) {
            let mut data = DataDescription::default();
            if !data.parse(
                section,
                &self.name,
                component_name,
                Some(&mut self.num_latest_change_only_members),
                ctx,
                diag,
            ) {
                diag.warning(format!("Error parsing properties for {}\n", self.name));
                return false;
            }

            if data.data_flags.is_editor_only() {
                continue;
            }

            let mut index = self.properties.len() as i32;
            let mut client_server_index = -1i32;

            let property_map = self
                .property_map
                .entry(component_name.to_string())
                .or_default();

            if let Some(&existing) = property_map.get(data.name()) {
                let component_suffix = if component_name.is_empty() {
                    String::new()
                } else {
                    format!(".{component_name}")
                };
                diag.info(format!(
                    "EntityDescription::parseProperties: property {}{}.{} is being overridden.\n",
                    self.name,
                    component_suffix,
                    data.name()
                ));
                index = existing as i32;
                if data.data_flags.is_client_server_data() {
                    client_server_index = self.properties[index as usize].client_server_full_index;
                }
            }

            data.index = index;
            property_map.insert(data.name().to_string(), index as usize);

            if data.data_flags.is_client_server_data() {
                if client_server_index != -1 {
                    data.client_server_full_index = client_server_index;
                    self.client_server_properties[client_server_index as usize] = index as usize;
                } else {
                    data.client_server_full_index = self.client_server_properties.len() as i32;
                    self.client_server_properties.push(index as usize);
                }

                let client_safety = data.client_safety();

                if client_safety & crate::flags::CLIENT_UNSAFE != 0 {
                    diag.warning(format!(
                        "{}.{} is a client-server property that has a PYTHON member (potential security hole)\n",
                        self.name,
                        data.name()
                    ));
                }

                if client_safety & crate::flags::CLIENT_UNUSABLE != 0 {
                    diag.error(format!(
                        "{}.{} is a client-server property that has a MAILBOX member (type is not able to be sent to or from client)\n",
                        self.name,
                        data.name()
                    ));
                    properties_ok = false;
                }
            }

            if data.data_flags.is_other_client_data() {
                let mut detail_level = 0i32;
                if self.lod_levels.find_level(
                    section.open_section("DetailLevel"),
                    &mut detail_level,
                    diag,
                ) {
                    data.detail_level = detail_level;
                } else {
                    diag.error(format!(
                        "EntityDescription::parseProperties: Invalid detail level for {}.\n",
                        data.name()
                    ));
                    return false;
                }

                data.event_stamp_index = self.num_event_stamped_properties as i32;
                self.num_event_stamped_properties += 1;
            }

            if index as usize == self.properties.len() {
                self.properties.push(data);
            } else {
                self.properties[index as usize] = data;
            }
        }

        if !properties_ok {
            return false;
        }

        self.allocate_client_server_full_indexes();

        true
    }

    /// `EntityDescription::parseTempProperties`: names only, no descriptions.
    fn parse_temp_properties(&mut self, temp_props: Option<&DataSection>) -> bool {
        let Some(temp_props) = temp_props else {
            return true;
        };

        for child in children(temp_props) {
            self.temp_properties
                .insert(child.section_name().to_string());
        }

        true
    }

    /// `EntityDescription::allocateClientServerFullIndexes`: stable sort by
    /// bandwidth (fixed ascending, then variable by header width), then the
    /// full indexes follow the new order.
    pub fn allocate_client_server_full_indexes(&mut self) {
        self.client_server_properties.sort_by(|&a, &b| {
            let size1 = self.properties[a].stream_size() as i16;
            let size2 = self.properties[b].stream_size() as i16;
            match (size1 < 0, size2 < 0) {
                (true, true) => (-size1).cmp(&(-size2)),
                (false, false) => size1.cmp(&size2),
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
            }
        });

        for (i, &property_index) in self.client_server_properties.iter().enumerate() {
            self.properties[property_index].client_server_full_index = i as i32;
        }
    }

    /// `EntityDescription::checkExposedMethods`.
    pub fn check_exposed_methods(&self, is_proxy: bool, diag: &mut Diagnostics) -> bool {
        let mut is_okay = true;

        for i in 0..self.base.num_exposed() {
            let method = self.base.exposed_method(i).expect("exposed base method");
            if !is_proxy {
                diag.warning(format!(
                    "EntityDescription::checkExposedMethods: Base method {} in non-proxy entity {} cannot be exposed\n",
                    method.name(),
                    self.name
                ));
                is_okay = false;
            } else if method.is_exposed_to_all_clients_only() {
                diag.warning(format!(
                    "EntityDescription::checkExposedMethods: Base method {} in entity {} cannot be exposed with ALL_CLIENTS flag\n",
                    method.name(),
                    self.name
                ));
                is_okay = false;
            }
        }

        for i in 0..self.cell.num_exposed() {
            let method = self.cell.exposed_method(i).expect("exposed cell method");
            if !is_proxy {
                if method.is_exposed_to_own_client_only() {
                    diag.warning(format!(
                        "EntityDescription::checkExposedMethods: Cell method {} in non-proxy entity {} cannot be exposed with OWN_CLIENT flag\n",
                        method.name(),
                        self.name
                    ));
                    is_okay = false;
                }
            } else if method.is_exposed_to_default() {
                diag.warning(format!(
                    "EntityDescription::checkExposedMethods: Cell method {} in proxy entity {} must be exposed explicitly with ALL_CLIENTS or OWN_CLIENT flag\n",
                    method.name(),
                    self.name
                ));
                is_okay = false;
            }
        }

        is_okay
    }

    /// `EntityDescription::shouldConsiderData( pass, pDD, dataDomains )`.
    pub fn should_consider_data(
        pass: usize,
        property: &DataDescription,
        data_domains: u32,
    ) -> bool {
        crate::flags::PASS_FILTER[pass][0] == property.data_flags.is_base_data()
            && crate::flags::PASS_FILTER[pass][1] == property.data_flags.is_client_server_data()
            && (property.data_flags.is_other_client_data()
                || data_domains & ONLY_OTHER_CLIENT_DATA == 0)
            && (property.data_flags.is_persistent() || data_domains & ONLY_PERSISTENT_DATA == 0)
    }

    /// `EntityDescription::shouldSkipPass( pass, dataDomains )`.
    pub fn should_skip_pass(pass: usize, data_domains: u32) -> bool {
        let pass_jumper = crate::flags::PASS_JUMPER[pass];

        if data_domains & EXACT_MATCH != 0 {
            data_domains != pass_jumper
        } else {
            data_domains & pass_jumper == 0
        }
    }

    /// `EntityDescription::visit`: each property matching `data_domains`, in
    /// pass order then declaration order.
    pub fn visit(
        &self,
        data_domains: u32,
        visitor: &mut dyn FnMut(&DataDescription) -> bool,
    ) -> bool {
        for pass in 0..crate::flags::NUM_PASSES {
            if Self::should_skip_pass(pass, data_domains) {
                continue;
            }
            for property in &self.properties {
                if Self::should_consider_data(pass, property, data_domains) && !visitor(property) {
                    return false;
                }
            }
        }
        true
    }

    /// `EntityDescription::clientServerProperty( n )`.
    pub fn client_server_property(&self, n: usize) -> Option<&DataDescription> {
        self.properties.get(*self.client_server_properties.get(n)?)
    }

    /// `EntityDescription::addToMD5`: name, then client-server properties
    /// (declaration order, each preceded by its full index), then client
    /// methods (all), base and cell methods (exposed only), each with its
    /// legacy exposed index.
    pub fn add_to_md5(&self, md5: &mut Md5) {
        md5.append_text(&self.name);

        for property in &self.properties {
            // Ignore the server-side-only ones.
            if property.data_flags.is_client_server_data() {
                md5.append_i32(property.client_server_full_index);
                property.add_to_md5(md5);
            }
        }

        for (count, method) in self.client.internal_methods().iter().enumerate() {
            method.add_to_md5(md5, count as i32);
        }

        let mut count = 0;
        for method in self.base.internal_methods() {
            if method.is_exposed() {
                method.add_to_md5(md5, count);
                count += 1;
            }
        }

        let mut count = 0;
        for method in self.cell.internal_methods() {
            if method.is_exposed() {
                method.add_to_md5(md5, count);
                count += 1;
            }
        }
    }

    /// `EntityDescription::addPersistentPropertiesToMD5`.
    pub fn add_persistent_properties_to_md5(&self, md5: &mut Md5) {
        md5.append_text(&self.name);

        for property in &self.properties {
            if property.data_flags.is_persistent() {
                property.add_to_md5(md5);
            }
        }
    }

    /// `EntityDescription::setExposedMsgIDs`.
    ///
    /// Kept flat to stay a literal transcription of the reference's parameter
    /// list (six ranges plus the diagnostics sink).
    #[allow(clippy::too_many_arguments)]
    pub fn set_exposed_msg_ids(
        &mut self,
        max_client: i32,
        client_range: Option<&crate::method::ExposedMethodMessageRange>,
        max_base: i32,
        base_range: Option<&crate::method::ExposedMethodMessageRange>,
        max_cell: i32,
        cell_range: Option<&crate::method::ExposedMethodMessageRange>,
        diag: &mut Diagnostics,
    ) {
        self.client
            .set_exposed_msg_ids(max_client, client_range, diag);
        self.base.set_exposed_msg_ids(max_base, base_range, diag);
        self.cell.set_exposed_msg_ids(max_cell, cell_range, diag);
    }

    /// `isTempProperty`.
    pub fn is_temp_property(&self, name: &str) -> bool {
        self.temp_properties.contains(name)
    }

    /// `pIdentifier`: the first identifier property, if any.
    pub fn identifier(&self) -> Option<&DataDescription> {
        self.properties
            .iter()
            .find(|p| p.data_flags.is_identifier())
    }

    /// Test/inspection helper: the sorted property-index list.
    pub fn client_server_property_indexes(&self) -> &[usize] {
        &self.client_server_properties
    }
}

enum MethodTarget {
    Base,
    Cell,
    Client,
}

// The name-sorted iteration and map-level parse live in `entity_map`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flags::{
        BASE, BASE_DATA, CLIENT_DATA, FROM_BASE_TO_CLIENT_DATA, FROM_CELL_TO_CLIENT_DATA,
    };

    fn ctx() -> TypeContext {
        TypeContext::default()
    }

    /// A resolver serving an in-memory tree, built from (path, xml) pairs.
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

    const RES: &str = "scripts/entities.xml";
    const DEFS: &str = "scripts/entity_defs";

    fn basic_fixture() -> MemoryResolver {
        MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Account/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Account.def"),
                "<root> \
                     <Properties> \
                         <playerName> \
                             <Type> STRING </Type> \
                             <Flags> BASE_AND_CLIENT </Flags> \
                             <Persistent> true </Persistent> \
                         </playerName> \
                     </Properties> \
                 </root>",
            ),
        ])
    }

    #[test]
    fn a_minimal_entity_parses_with_script_decider() {
        let resolver = basic_fixture();
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();

        let decider = HasScriptDecider::with_client_script(&resolver, true);
        assert!(desc.parse("Account", "", Some(&decider), &resolver, &ctx(), &mut diag));

        assert_eq!(desc.name, "Account");
        assert!(desc.can_be_on_client());
        assert!(!desc.can_be_on_cell(), "no scripts/cell/Account.py");
        assert!(!desc.can_be_on_base(), "no scripts/base/Account.py");
        assert!(desc.is_client_type());
        assert_eq!(desc.property_count(), 1);

        let prop = desc.property(0).expect("playerName");
        assert_eq!(prop.name(), "playerName");
        assert_eq!(prop.index, 0);
        assert_eq!(prop.client_server_full_index, 0);
        assert!(prop.data_flags.is_base_data());
        assert!(prop.data_flags.is_own_client_data());
    }

    #[test]
    fn missing_def_file_is_an_error() {
        let resolver = MemoryResolver::new(&[(RES, "<root/>")]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        let decider = HasScriptDecider::new(&resolver);
        assert!(!desc.parse("Ghost", "", Some(&decider), &resolver, &ctx(), &mut diag));
        assert_eq!(
            diag.errors(),
            vec!["EntityDescription::parse: Could not open scripts/entity_defs/Ghost.def\n"]
        );
    }

    #[test]
    fn parent_parse_layers_into_the_same_object_and_overrides_reuse_slots() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Child/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Parent.def"),
                "<root> \
                     <Properties> \
                         <hp> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </hp> \
                         <mp> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </mp> \
                     </Properties> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Child.def"),
                "<root> \
                     <Parent> Parent </Parent> \
                     <Properties> \
                         <hp> <Type> INT64 </Type> <Flags> ALL_CLIENTS </Flags> </hp> \
                     </Properties> \
                 </root>",
            ),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        let decider = HasScriptDecider::with_client_script(&resolver, true);
        assert!(desc.parse("Child", "", Some(&decider), &resolver, &ctx(), &mut diag));

        assert_eq!(desc.property_count(), 2, "hp overridden in place");
        let hp = desc.property(0).expect("hp");
        assert_eq!(hp.data_type.meta_name(), "INT64", "child wins");
        assert_eq!(hp.index, 0);
        assert!(diag
            .messages()
            .iter()
            .any(|m| m.contains("property Child.hp is being overridden.")));
    }

    #[test]
    fn implements_interfaces_add_members_with_their_own_sections() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Knight/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/interfaces/Combat.def"),
                "<root> \
                     <Properties> \
                         <attack> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </attack> \
                     </Properties> \
                     <CellMethods> \
                         <strike> <Arg> INT32 </Arg> <Exposed> ALL_CLIENTS </Exposed> </strike> \
                     </CellMethods> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Knight.def"),
                "<root> \
                     <Implements> <Interface> Combat </Interface> </Implements> \
                     <Properties> \
                         <armour> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </armour> \
                     </Properties> \
                 </root>",
            ),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        let decider = HasScriptDecider::with_client_script(&resolver, true);
        assert!(desc.parse("Knight", "", Some(&decider), &resolver, &ctx(), &mut diag));

        // Interface properties come first (parsed before the entity's own).
        let names: Vec<&str> = desc
            .properties()
            .iter()
            .map(DataDescription::name)
            .collect();
        assert_eq!(names, vec!["attack", "armour"]);
        assert_eq!(desc.cell.size(), 1, "interface cell methods merge in");
    }

    /// BW-04-01-02: a parent method redeclared with another signature is a
    /// conflict, not a second method.
    #[test]
    fn a_conflicting_parent_method_fails_the_child() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Child/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Parent.def"),
                "<root> <CellMethods> \
                         <jump> <Arg> INT32 </Arg> <Exposed> </Exposed> </jump> \
                     </CellMethods> </root>",
            ),
            (
                &format!("{DEFS}/Child.def"),
                "<root> <Parent> Parent </Parent> <CellMethods> \
                         <jump> <Arg> STRING </Arg> <Exposed> </Exposed> </jump> \
                     </CellMethods> </root>",
            ),
            ("scripts/base/Child.py", "# base script"),
            ("scripts/base/Parent.py", "# base script"),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Child",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag
            .errors()
            .iter()
            .any(|m| m.contains("Method 'jump' already defined with a different signature")));
    }

    /// The same method through an `<Implements>` interface conflicts the same
    /// way.
    #[test]
    fn an_interface_method_conflicts_with_the_entity_method() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Knight/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/interfaces/Combat.def"),
                "<root> <BaseMethods> <shout> <Arg> INT32 </Arg> <Exposed> </Exposed> </shout> \
                     </BaseMethods> </root>",
            ),
            (
                &format!("{DEFS}/Knight.def"),
                "<root> \
                     <Implements> <Interface> Combat </Interface> </Implements> \
                     <BaseMethods> <shout> <Exposed> </Exposed> </shout> </BaseMethods> \
                 </root>",
            ),
            ("scripts/base/Knight.py", "# base script"),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Knight",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag
            .errors()
            .iter()
            .any(|m| m.contains("Method 'shout' already defined with a different signature")));
    }

    /// Volatile priorities are sticky: a child keeps its parent's values
    /// unless it declares its own.
    #[test]
    fn volatile_priorities_inherit_from_the_parent() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Child/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Parent.def"),
                "<root> <Volatile> <position> 10 </position> </Volatile> </root>",
            ),
            (
                &format!("{DEFS}/Child.def"),
                "<root> <Parent> Parent </Parent> </root>",
            ),
            ("scripts/base/Child.py", "# base script"),
            ("scripts/base/Parent.py", "# base script"),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            desc.parse(
                "Child",
                "",
                Some(&HasScriptDecider::with_client_script(&resolver, true)),
                &resolver,
                &ctx(),
                &mut diag
            ),
            "{:?}",
            diag.messages()
        );
        // The parent's 10 is squared by the priority rule and inherited.
        assert_eq!(desc.volatile_info.position_priority(), 100.0);

        // An explicit child section replaces the inherited values.
        let mut resolver = resolver;
        resolver.files.insert(
            format!("{DEFS}/Child.def"),
            "<root> <Parent> Parent </Parent> <Volatile> <position> 3 </position> \
             </Volatile> </root>"
                .as_bytes()
                .to_vec(),
        );
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            desc.parse(
                "Child",
                "",
                Some(&HasScriptDecider::with_client_script(&resolver, true)),
                &resolver,
                &ctx(),
                &mut diag
            ),
            "{:?}",
            diag.messages()
        );
        assert_eq!(desc.volatile_info.position_priority(), 9.0);
    }

    /// Interfaces can themselves implement interfaces: members arrive in
    /// declaration order, deepest first.
    #[test]
    fn interface_chains_merge_deep_first() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Knight/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/interfaces/Base.def"),
                "<root> <Properties> \
                         <stance> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </stance> \
                     </Properties> </root>",
            ),
            (
                &format!("{DEFS}/interfaces/Combat.def"),
                "<root> <Implements> <Interface> Base </Interface> </Implements> \
                     <Properties> \
                         <attack> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </attack> \
                     </Properties> </root>",
            ),
            (
                &format!("{DEFS}/Knight.def"),
                "<root> <Implements> <Interface> Combat </Interface> </Implements> \
                     <Properties> \
                         <armour> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </armour> \
                     </Properties> </root>",
            ),
            ("scripts/base/Knight.py", "# base script"),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            desc.parse(
                "Knight",
                "",
                Some(&HasScriptDecider::with_client_script(&resolver, true)),
                &resolver,
                &ctx(),
                &mut diag
            ),
            "{:?}",
            diag.messages()
        );

        let names: Vec<&str> = desc
            .properties()
            .iter()
            .map(DataDescription::name)
            .collect();
        assert_eq!(names, vec!["stance", "attack", "armour"]);
        assert_eq!(desc.properties()[0].index, 0);
        assert_eq!(desc.properties()[1].index, 1);
        assert_eq!(desc.properties()[2].index, 2);
    }

    #[test]
    fn visit_orders_by_pass_then_declaration() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <A/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/A.def"),
                "<root> \
                     <Properties> \
                         <baseClient> <Type> INT32 </Type> <Flags> BASE_AND_CLIENT </Flags> <Persistent> true </Persistent> </baseClient> \
                         <cellClient> <Type> INT32 </Type> <Flags> ALL_CLIENTS </Flags> </cellClient> \
                         <baseOnly> <Type> INT32 </Type> <Flags> BASE </Flags> <Persistent> true </Persistent> </baseOnly> \
                         <cellOnly> <Type> INT32 </Type> <Flags> CELL_PUBLIC </Flags> </cellOnly> \
                     </Properties> \
                 </root>",
            ),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        let decider = HasScriptDecider::with_client_script(&resolver, true);
        assert!(desc.parse("A", "", Some(&decider), &resolver, &ctx(), &mut diag));

        // CLIENT_DATA visits the base+client pass first, then cell+client.
        let mut seen = Vec::new();
        desc.visit(CLIENT_DATA, &mut |p| {
            seen.push(p.name().to_string());
            true
        });
        assert_eq!(seen, vec!["baseClient", "cellClient"]);

        // Exact-match base-to-client.
        let mut seen = Vec::new();
        desc.visit(FROM_BASE_TO_CLIENT_DATA, &mut |p| {
            seen.push(p.name().to_string());
            true
        });
        assert_eq!(seen, vec!["baseClient"]);

        let mut seen = Vec::new();
        desc.visit(FROM_CELL_TO_CLIENT_DATA, &mut |p| {
            seen.push(p.name().to_string());
            true
        });
        assert_eq!(seen, vec!["cellClient"]);
    }

    #[test]
    fn should_skip_pass_and_consider_data_follow_the_tables() {
        // EXACT_MATCH: only the pass whose jumper equals the domain runs.
        assert!(!EntityDescription::should_skip_pass(
            1,
            FROM_BASE_TO_CLIENT_DATA
        ));
        assert!(EntityDescription::should_skip_pass(
            0,
            FROM_BASE_TO_CLIENT_DATA
        ));
        assert!(EntityDescription::should_skip_pass(
            2,
            FROM_BASE_TO_CLIENT_DATA
        ));

        // Non-exact: any overlap keeps the pass.
        assert!(!EntityDescription::should_skip_pass(0, BASE_DATA));
        assert!(EntityDescription::should_skip_pass(2, BASE_DATA));

        // The property classes land in their one matching pass.
        let base_cs = crate::flags::EntityDataFlags::new(BASE | crate::flags::OWN_CLIENT);
        assert!(EntityDescription::should_consider_data(
            1,
            &prop_with(base_cs),
            CLIENT_DATA
        ));
        assert!(!EntityDescription::should_consider_data(
            0,
            &prop_with(base_cs),
            CLIENT_DATA
        ));
        assert!(!EntityDescription::should_consider_data(
            2,
            &prop_with(base_cs),
            CLIENT_DATA
        ));
    }

    fn prop_with(flags: crate::flags::EntityDataFlags) -> DataDescription {
        DataDescription {
            data_flags: flags,
            ..DataDescription::default()
        }
    }

    #[test]
    fn digest_covers_name_cs_properties_and_exposed_methods() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <M/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/M.def"),
                "<root> \
                     <Properties> \
                         <hp> <Type> INT32 </Type> <Flags> BASE_AND_CLIENT </Flags> <Persistent> true </Persistent> </hp> \
                         <secret> <Type> INT32 </Type> <Flags> BASE </Flags> <Persistent> true </Persistent> </secret> \
                     </Properties> \
                     <ClientMethods> \
                         <ping> <Arg> INT32 </Arg> </ping> \
                     </ClientMethods> \
                     <BaseMethods> \
                         <quit> <Exposed> OWN_CLIENT </Exposed> </quit> \
                         <quiet> </quiet> \
                     </BaseMethods> \
                 </root>",
            ),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        let decider = HasScriptDecider::with_client_script(&resolver, true);
        assert!(desc.parse("M", "", Some(&decider), &resolver, &ctx(), &mut diag));

        let mut md5 = Md5::new();
        desc.add_to_md5(&mut md5);

        let mut expected = Md5::new();
        expected.append_text("M");
        // hp: full index then property digest.
        expected.append_i32(0);
        expected.append_text("hp");
        expected.append_i32((crate::flags::OWN_CLIENT | crate::flags::BASE) as i32);
        expected.append_cstr("Int");
        expected.append_i32(4);
        // client method: everything counts, in declaration order. Init has
        // already auto-exposed it to all clients.
        expected.append_text("ping");
        expected.append_u8(0x4); // CLIENT component + IS_EXPOSED_TO_ALL_CLIENTS
        expected.append_cstr("Int");
        expected.append_i32(4);
        expected.append_i32(0);
        // base methods: only exposed ones count.
        expected.append_text("quit");
        expected.append_u8(0x2 | 0x8); // BASE + exposed to own client
        expected.append_i32(0);

        assert_eq!(
            md5.quote(),
            expected.quote(),
            "digest covers name, cs properties, then per-component methods"
        );
    }

    #[test]
    fn temp_properties_are_names_only() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <T/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/T.def"),
                "<root> \
                     <TempProperties> <scratch/> <notes/> </TempProperties> \
                 </root>",
            ),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        let decider = HasScriptDecider::with_client_script(&resolver, true);
        assert!(desc.parse("T", "", Some(&decider), &resolver, &ctx(), &mut diag));

        assert!(desc.is_temp_property("scratch"));
        assert!(desc.is_temp_property("notes"));
        assert!(!desc.is_temp_property("other"));
    }

    #[test]
    fn check_exposed_methods_flags_wrong_exposures() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Npc/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Npc.def"),
                "<root> \
                     <BaseMethods> \
                         <boom> <Exposed> </Exposed> </boom> \
                     </BaseMethods> \
                 </root>",
            ),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        let decider = HasScriptDecider::with_client_script(&resolver, true);
        assert!(desc.parse("Npc", "", Some(&decider), &resolver, &ctx(), &mut diag));

        // Not a proxy (no scripts/base/Npc.py): exposed base method warns.
        assert!(!desc.check_exposed_methods(false, &mut diag));
        assert!(diag
            .warnings()
            .iter()
            .any(|m| m.contains("Base method boom in non-proxy entity Npc cannot be exposed")));
    }

    #[test]
    fn distribution_tags_override_script_detection() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Account/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Account.def"),
                "<root> \
                     <Distribution> \
                         <Cell> true </Cell> \
                         <Base> false </Base> \
                     </Distribution> \
                 </root>",
            ),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        // The blanket `&T` impl lets plain references compose as resolvers.
        let boxed: &MemoryResolver = &resolver;
        assert!(desc.parse(
            "Account",
            "",
            Some(&HasScriptDecider::new(boxed)),
            boxed,
            &ctx(),
            &mut diag
        ));

        assert!(
            desc.can_be_on_cell(),
            "the Cell tag wins over the missing script"
        );
        assert!(!desc.can_be_on_base(), "the Base tag wins over any script");
        assert!(
            !desc.can_be_on_client(),
            "an absent Client tag means false: a <Distribution> decides"
        );
        assert_eq!(desc.client_name(), "", "no client, no client name");
    }

    #[test]
    fn unparseable_parent_is_an_error() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Avatar/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Avatar.def"),
                "<root> <Parent> definitely-not-a-def </Parent> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse("Avatar", "", None, &resolver, &ctx(), &mut diag));
        assert!(diag
            .errors()
            .iter()
            .any(|m| m.contains("Could not parse definitely-not-a-def, parent of Avatar")));
    }

    #[test]
    fn lod_levels_remap_detail_levels_after_parse() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Guard/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Guard.def"),
                "<root> \
                     <LoDLevels> \
                         <l1> 100 <label> near </label> </l1> \
                         <l2> 200 <label> mid </label> </l2> \
                     </LoDLevels> \
                     <Properties> \
                         <hp> \
                             <Type> INT32 </Type> \
                             <Flags> OTHER_CLIENTS </Flags> \
                             <DetailLevel> mid </DetailLevel> \
                         </hp> \
                         <mood> \
                             <Type> INT32 </Type> \
                             <Flags> OTHER_CLIENTS </Flags> \
                         </mood> \
                     </Properties> \
                 </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Guard",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));

        // After the remap, recorded indexes point at the sorted level table.
        let hp = desc.property(0).expect("hp");
        assert_eq!(hp.name(), "hp");
        let mood = desc.property(1).expect("mood");
        assert!(
            mood.detail_level != crate::lod::NO_LEVEL && mood.detail_level >= 0,
            "no DetailLevel records the outer level"
        );
    }

    #[test]
    fn invalid_detail_level_fails_the_property() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Guard/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Guard.def"),
                "<root> <Properties> \
                         <hp> \
                             <Type> INT32 </Type> \
                             <Flags> OTHER_CLIENTS </Flags> \
                             <DetailLevel> nowhere </DetailLevel> \
                         </hp> \
                     </Properties> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Guard",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag.errors().iter().any(|m| {
            m.contains("EntityDescription::parseProperties: Invalid detail level for hp.")
        }));
    }

    #[test]
    fn client_methods_without_a_client_script_fail_the_parse() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Account/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Account.def"),
                "<root> <ClientMethods> <ping> </ping> </ClientMethods> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Account",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, false)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag.errors().iter().any(|m| {
            m.contains("Entity 'Account' does not have a client script but has client methods")
        }));
    }

    #[test]
    fn client_only_entities_are_never_persistent() {
        // Client script present, no base/cell scripts: client-only.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <ClientOnly/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/ClientOnly.def"),
                "<root> <Persistent> true </Persistent> </root>",
            ),
            ("scripts/client/ClientOnly.py", "# client script"),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "ClientOnly",
            "",
            Some(&HasScriptDecider::new(&resolver)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(desc.is_client_only_type());
        assert!(!desc.is_persistent(), "client-only types drop persistence");
    }

    #[test]
    fn deprecated_is_manual_aoi_warns_and_is_manual_aoi_overrides() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Guard/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Guard.def"),
                "<root> <IsManualAOI> true </IsManualAOI> \
                     <IsManualAoI> false </IsManualAoI> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Guard",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag
            .warnings()
            .iter()
            .any(|m| m.contains("IsManualAOI is deprecated, use IsManualAoI instead")));
        assert!(
            !desc.appeal_radius.is_nan(),
            "parse kept going after the deprecated tag"
        );
    }

    #[test]
    fn failed_interface_reported_per_interface() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Avatar/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Avatar.def"),
                "<root> <Implements> <i> Male </i> </Implements> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            !desc.parse("Avatar", "", None, &resolver, &ctx(), &mut diag),
            "{:?}",
            diag.messages()
        );
        assert!(diag.errors().iter().any(|m| m.contains(
            "BaseUserDataObjectDescription::parseImplements: Failed parsing interface Male"
        )));
    }

    #[test]
    fn components_load_from_their_own_def_files() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Robot/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Robot.def"),
                "<root> <Components> <movement> <name> walk </name> </movement> </Components> </root>",
            ),
            (
                "scripts/component_defs/movement.def",
                "<root> <Properties> \
                         <speed> <Type> INT32 </Type> <Flags> BASE </Flags> </speed> \
                     </Properties> \
                     <TempProperties> <target/> </TempProperties> \
                 </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Robot",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert_eq!(desc.property_count(), 1);
        assert_eq!(desc.property(0).expect("speed").component_name, "walk");
        assert!(desc.is_temp_property("target"));
        assert!(diag.messages().iter().any(|m| m.contains(
            "EntityDescription::parseComponent: adding component 'walk' to definition for entity 'Robot'."
        )));

        // A duplicated component name errors out.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Robot/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Robot.def"),
                "<root> <Components> <movement/> <movement/> </Components> </root>",
            ),
            ("scripts/component_defs/movement.def", "<root/>"),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Robot",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag
            .errors()
            .iter()
            .any(|m| m.contains("Component with name 'movement' is duplicated.")));

        // A missing component def file errors out.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Robot/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Robot.def"),
                "<root> <Components> <legs/> </Components> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Robot",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag.errors().iter().any(|m| m.contains(
            "EntityDescription::parseComponents: Failed to open component def file 'scripts/component_defs/legs.def'."
        )));
    }

    #[test]
    fn services_reject_properties_with_a_warning() {
        // The service flavour skips its own <Properties>; only the
        // component pass reaches the property parser.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Cleaner/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                // Service definitions live in their own directory, not with
                // the entity defs.
                "scripts/service_defs/Cleaner.def",
                "<root> <Components> <mop/> </Components> </root>",
            ),
            (
                "scripts/component_defs/mop.def",
                "<root> <Properties> \
                         <handle> <Type> INT32 </Type> <Flags> BASE </Flags> </handle> \
                     </Properties> </root>",
            ),
            ("scripts/service/Cleaner.py", "# service script"),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        let decider = crate::entity_map::ServiceScriptDecider::new(&resolver);
        assert!(
            desc.parse_service("Cleaner", "", Some(&decider), &resolver, &ctx(), &mut diag),
            "{:?}",
            diag.messages()
        );
        assert!(diag.warnings().iter().any(|m| m.contains(
            "EntityDescription::parseProperties: Cleaner: Services do not support properties."
        )));
        assert_eq!(desc.property_count(), 0);
    }

    #[test]
    fn broken_property_fails_the_entity_parse() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Guard/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Guard.def"),
                "<root> <Properties> \
                         <hp> <Type> INT32 </Type> <Flags> NOT_A_FLAGS_NAME </Flags> </hp> \
                     </Properties> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Guard",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag
            .warnings()
            .iter()
            .any(|m| m.contains("Error parsing properties for Guard")));
    }

    #[test]
    fn editor_only_properties_are_skipped() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Guard/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Guard.def"),
                "<root> <Properties> \
                         <spawn> <Type> INT32 </Type> <Flags> EDITOR_ONLY </Flags> </spawn> \
                         <hp> <Type> INT32 </Type> <Flags> BASE </Flags> </hp> \
                     </Properties> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Guard",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert_eq!(desc.property_count(), 1, "spawn is editor-only");
        assert_eq!(desc.property(0).expect("hp").name(), "hp");
    }

    #[test]
    fn overridden_client_server_property_reuses_the_full_index() {
        // Two passes at entity scope: the interface property first, then the
        // entity overrides it. The client-server full index is inherited.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Avatar/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Avatar.def"),
                "<root> \
                     <Implements> <i> shared </i> </Implements> \
                     <Properties> \
                         <hp> <Type> INT32 </Type> <Flags> BASE_AND_CLIENT </Flags> </hp> \
                     </Properties> \
                 </root>",
            ),
            (
                &format!("{DEFS}/interfaces/shared.def"),
                "<root> <Properties> \
                         <hp> <Type> INT32 </Type> <Flags> BASE_AND_CLIENT </Flags> </hp> \
                         <gold> <Type> INT32 </Type> <Flags> BASE_AND_CLIENT </Flags> </gold> \
                     </Properties> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            desc.parse("Avatar", "", None, &resolver, &ctx(), &mut diag),
            "{:?}",
            diag.messages()
        );
        assert!(diag
            .messages()
            .iter()
            .any(|m| m.contains("property Avatar.hp is being overridden.")));
        assert_eq!(desc.property_count(), 2, "hp replaces its interface slot");
        assert_eq!(
            desc.client_server_property_count(),
            2,
            "hp and gold share the two client-server slots"
        );
        assert_eq!(
            desc.property(0).expect("hp").client_server_full_index,
            0,
            "the override keeps the interface's full index"
        );
    }

    #[test]
    fn python_and_mailbox_members_flag_client_safety() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Guard/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Guard.def"),
                "<root> <Properties> \
                         <risky> \
                             <Type> FIXED_DICT <Properties> \
                                 <fn> <Type> PYTHON </Type> </fn> \
                             </Properties> </Type> \
                             <Flags> BASE_AND_CLIENT </Flags> \
                         </risky> \
                     </Properties> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Guard",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag.warnings().iter().any(|m| m.contains(
            "Guard.risky is a client-server property that has a PYTHON member (potential security hole)"
        )));

        // MAILBOX members make the parse fail outright.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Guard/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Guard.def"),
                "<root> <Properties> \
                         <risky> \
                             <Type> FIXED_DICT <Properties> \
                                 <mb> <Type> MAILBOX </Type> </mb> \
                             </Properties> </Type> \
                             <Flags> BASE_AND_CLIENT </Flags> \
                         </risky> \
                     </Properties> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Guard",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag.errors().iter().any(|m| m.contains(
            "Guard.risky is a client-server property that has a MAILBOX member (type is not able to be sent to or from client)"
        )));
    }

    #[test]
    fn exposed_method_rules_follow_the_proxy_role() {
        // Proxy entity: a bare base exposure is fine, but a default cell
        // exposure must be flagged explicitly.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Proxy/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Proxy.def"),
                "<root> \
                     <BaseMethods> \
                         <createCell> <Exposed/> </createCell> \
                     </BaseMethods> \
                     <CellMethods> \
                         <tick> <Exposed/> </tick> \
                         <growl> <Exposed> ALL_CLIENTS </Exposed> </growl> \
                     </CellMethods> \
                 </root>",
            ),
            ("scripts/base/Proxy.py", "# base script"),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            desc.parse(
                "Proxy",
                "",
                Some(&HasScriptDecider::new(&resolver)),
                &resolver,
                &ctx(),
                &mut diag
            ),
            "{:?}",
            diag.messages()
        );
        // A default (no-flag) cell exposure on a proxy must be exposed
        // explicitly; the ALL_CLIENTS cell exposure passes the role check.
        assert!(!desc.check_exposed_methods(true, &mut diag));
        assert!(diag.warnings().iter().any(|m| m.contains(
            "Cell method tick in proxy entity Proxy must be exposed explicitly with ALL_CLIENTS or OWN_CLIENT flag"
        )));
        assert!(!diag.warnings().iter().any(|m| m.contains("growl")));

        // Non-proxy: OWN_CLIENT on a cell method is rejected.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Npc/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Npc.def"),
                "<root> <CellMethods> \
                         <growl> <Exposed> OWN_CLIENT </Exposed> </growl> \
                     </CellMethods> </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Npc",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(!desc.check_exposed_methods(false, &mut diag));
        assert!(diag.warnings().iter().any(|m| m.contains(
            "Cell method growl in non-proxy entity Npc cannot be exposed with OWN_CLIENT flag"
        )));
    }

    #[test]
    fn visit_stops_when_the_visitor_says_stop() {
        let resolver = basic_fixture();
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Account",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));

        let mut seen = 0;
        assert!(!desc.visit(FROM_BASE_TO_CLIENT_DATA, &mut |_| {
            seen += 1;
            false
        }));
        assert_eq!(seen, 1, "the visitor vetoed after the first property");
    }

    #[test]
    fn client_server_property_accessor_and_persistent_digest() {
        let resolver = basic_fixture();
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Account",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));

        let cs = desc.client_server_property(0).expect("first cs property");
        assert_eq!(cs.name(), "playerName");
        assert!(desc.client_server_property(9).is_none());

        // The persistent digest carries the entity name then each persistent
        // property digest in declaration order.
        let mut md5 = crate::digest::Md5::new();
        desc.add_persistent_properties_to_md5(&mut md5);
        let mut expected = crate::digest::Md5::new();
        expected.append_text("Account");
        let player_name = desc.property(0).expect("playerName");
        player_name.add_to_md5(&mut expected);
        assert_eq!(md5.quote(), expected.quote());
    }

    #[test]
    fn set_exposed_msg_ids_lands_on_each_component_table() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Avatar/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Avatar.def"),
                "<root> \
                     <ClientMethods> <ping> <Arg> INT32 </Arg> </ping> </ClientMethods> \
                     <BaseMethods> <logOff> <Exposed/> </logOff> </BaseMethods> \
                 </root>",
            ),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(desc.parse(
            "Avatar",
            "",
            Some(&HasScriptDecider::with_client_script(&resolver, true)),
            &resolver,
            &ctx(),
            &mut diag
        ));

        let client_range = crate::method::ExposedMethodMessageRange::new(100, 130);
        let base_range = crate::method::ExposedMethodMessageRange::new(200, 230);
        let cell_range = crate::method::ExposedMethodMessageRange::new(300, 330);
        desc.set_exposed_msg_ids(
            1,
            Some(&client_range),
            1,
            Some(&base_range),
            0,
            Some(&cell_range),
            &mut diag,
        );

        let ping = desc.client().find("ping").expect("ping");
        assert_eq!(ping.exposed_index(), 0);
        assert_eq!(ping.exposed_msg_id(), 100, "first slot maps to range start");
        assert!(!ping.has_sub_msg_id());

        let log_off = desc.base().find("logOff").expect("logOff");
        assert_eq!(log_off.exposed_index(), 0);
        assert_eq!(log_off.exposed_msg_id(), 200);
        assert_eq!(desc.exposed_cell_method_count(), 0);
    }

    /// A `&Resolver` acts as a resolver itself, and the `Client` tag override
    /// beats the script-file lookup.
    #[test]
    fn resolver_references_forward_and_client_tags_win() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Ghost/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Ghost.def"),
                "<root> \
                     <Distribution> <Client> true </Client> </Distribution> \
                     <BaseMethods> <logOff> <Exposed/> </logOff> </BaseMethods> \
                 </root>",
            ),
            ("scripts/base/Ghost.py", "# base script"),
        ]);

        // A reference to a resolver resolves through the blanket impl.
        let forwarded: &dyn DefResolver = &&resolver;
        assert!(forwarded.python_script_exists("scripts/base/Ghost"));
        assert!(forwarded.open_section(RES).is_some());

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            desc.parse(
                "Ghost",
                "",
                Some(&HasScriptDecider::new(&resolver)),
                &resolver,
                &ctx(),
                &mut diag
            ),
            "{:?}",
            diag.messages()
        );
        // No scripts/client/Ghost.py, but the tag says yes.
        assert!(desc.can_be_on_client());
        assert_eq!(desc.exposed_base_method_count(), 1);
        assert_eq!(desc.exposed_cell_method_count(), 0);
    }

    #[test]
    fn cell_data_without_a_cell_script_warns() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Drone/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Drone.def"),
                "<root> <Properties> \
                     <coord> <Type> VECTOR3 </Type> <Flags> CELL_PRIVATE </Flags> </coord> \
                 </Properties> </root>",
            ),
            ("scripts/base/Drone.py", "# base script"),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            desc.parse(
                "Drone",
                "",
                Some(&HasScriptDecider::new(&resolver)),
                &resolver,
                &ctx(),
                &mut diag
            ),
            "{:?}",
            diag.messages()
        );
        assert!(diag.warnings().iter().any(|m| m.contains(
            "Entity 'Drone' does not have a cell script but has cell property 'coord'."
        )));
    }

    #[test]
    fn a_broken_component_fails_the_entity_parse() {
        // Craft references the mover component, whose def has a property
        // with no <Type>: parsing it fails the whole entity.
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Craft/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Craft.def"),
                "<root> <Components> <mover/> </Components> </root>",
            ),
            (
                "scripts/component_defs/mover.def",
                "<root> <Properties> \
                     <speed> <Type> NOT_A_TYPE </Type> <Flags> BASE </Flags> </speed> \
                 </Properties> </root>",
            ),
            ("scripts/base/Craft.py", "# base script"),
        ]);

        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(!desc.parse(
            "Craft",
            "",
            Some(&HasScriptDecider::new(&resolver)),
            &resolver,
            &ctx(),
            &mut diag
        ));
        assert!(diag.errors().iter().any(|m| m
            .contains("EntityDescription::parseComponents: Failed to parse component 'mover' for entity 'Craft'.")));
    }

    /// An override inside a component keeps the parent's index and reports
    /// the component in the message.
    #[test]
    fn component_overrides_reuse_the_full_index() {
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> <ClientServerEntities> <Hero/> </ClientServerEntities> \
                     <ServerOnlyEntities/> </root>",
            ),
            (
                &format!("{DEFS}/Hero.def"),
                "<root> <Properties> \
                     <note> <Type> STRING </Type> <Flags> CELL_PUBLIC_AND_OWN </Flags> </note> \
                     <pid> <Type> INT64 </Type> <Flags> BASE </Flags> \
                          <Identifier> true </Identifier> <Persistent> true </Persistent> \
                     </pid> \
                 </Properties> \
                 <Components> <mover/> </Components> </root>",
            ),
            (
                "scripts/component_defs/mover.def",
                "<root> <Properties> \
                     <hp> <Type> INT32 </Type> <Flags> CELL_PUBLIC_AND_OWN </Flags> </hp> \
                     <hp> <Type> UINT32 </Type> <Flags> CELL_PUBLIC_AND_OWN </Flags> </hp> \
                 </Properties> </root>",
            ),
            ("scripts/base/Hero.py", "# base script"),
        ]);
        let mut desc = EntityDescription::new();
        let mut diag = Diagnostics::new();
        assert!(
            desc.parse(
                "Hero",
                "",
                Some(&HasScriptDecider::new(&resolver)),
                &resolver,
                &ctx(),
                &mut diag
            ),
            "{:?}",
            diag.messages()
        );
        assert!(diag.messages().iter().any(|m| m.contains(
            "EntityDescription::parseProperties: property Hero.mover.hp is being overridden."
        )));

        // The override reuses the first declaration's index.
        assert_eq!(desc.property_count(), 3);
        assert_eq!(desc.client_server_property_count(), 2);
        // Fixed sizes sort ahead of variable ones.
        assert_eq!(desc.client_server_property_indexes(), &[2, 0]);

        let identifier = desc.identifier().expect("identifier property");
        assert_eq!(identifier.name(), "pid");

        let mut md5 = Md5::new();
        desc.add_to_md5(&mut md5);
        assert_eq!(md5.quote().len(), 32);

        let mut md5 = Md5::new();
        desc.add_persistent_properties_to_md5(&mut md5);
        assert_eq!(md5.quote().len(), 32);
    }
}
