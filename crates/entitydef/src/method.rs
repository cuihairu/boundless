//! `method_description.hpp/.cpp`, `method_args.cpp`,
//! `entity_method_descriptions.cpp` and `network/exposed_message_range.cpp`:
//! entity methods, their argument lists, the per-entity method table and the
//! exposed-call message ranges.
//!
//! The digest contribution of a method is its member name, the raw `flags_`
//! byte, the argument digests and the *legacy* exposed index — the declaration
//! order count of exposed methods. Return values are deliberately not
//! digested.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use resources::datasection::DataSection;

use crate::data_type::{DataType, TypeContext, build_type, children};
use crate::diagnostics::Diagnostics;
use crate::digest::Md5;
use crate::flags::{CLIENT_SAFE, CLIENT_UNSAFE, CLIENT_UNUSABLE};
use crate::member::{DEFAULT_VARIABLE_LENGTH_HEADER_SIZE, MemberDescription};

/// `MethodDescription::Component` — also the low two bits of `flags_`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Component {
    #[default]
    Client = 0,
    Cell = 1,
    Base = 2,
}

/// `IS_EXPOSED_TO_ALL_CLIENTS`.
pub const IS_EXPOSED_TO_ALL_CLIENTS: u8 = 0x4;
/// `IS_EXPOSED_TO_OWN_CLIENT`.
pub const IS_EXPOSED_TO_OWN_CLIENT: u8 = 0x8;

/// `MethodDescription::ReplayExposureLevel`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReplayExposureLevel {
    None,
    #[default]
    OtherClients,
    AllClients,
}

/// `MethodDescription::priority_` before any `DetailDistance` tag.
const DEFAULT_DETAIL_DISTANCE: f32 = f32::MAX;

// ---------------------------------------------------------------------------
// MethodArgs
// ---------------------------------------------------------------------------

/// `MethodArgs`: an ordered list of (name, type) pairs plus the summed
/// stream size.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MethodArgs {
    args: Vec<(String, Arc<DataType>)>,
    /// -1 once any argument is variable-sized.
    stream_size: i32,
}

impl MethodArgs {
    /// `MethodArgs::parse`.
    ///
    /// Old style takes only `<Arg>` children of the method section; new style
    /// treats every child of `<Args>` (or `<ReturnValues>`) as one argument,
    /// named by its tag.
    pub fn parse(
        &mut self,
        section: &DataSection,
        is_old_style: bool,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        self.args.clear();
        self.stream_size = 0;

        let mut arg_names = BTreeSet::new();

        for child in children(section) {
            if is_old_style && child.section_name() != "Arg" {
                continue;
            }

            let Some(data_type) = build_type(ctx, Some(child), diag) else {
                diag.error(format!(
                    "MethodArgs::parse: Return type #{} is invalid\n",
                    self.args.len()
                ));
                self.args.clear();
                return false;
            };

            let mut arg_name = String::new();
            if !is_old_style {
                arg_name = child.section_name().to_string();

                if arg_names.contains(&arg_name) {
                    diag.error(format!(
                        "MethodArgs::parse: Duplicate argument name '{arg_name}'\n"
                    ));
                    // "Arg" collisions are tolerated (TODO in the reference).
                    if arg_name != "Arg" {
                        return false;
                    }
                }
                arg_names.insert(arg_name.clone());
            }

            if self.stream_size != -1 {
                let data_type_stream_size = data_type.stream_size();
                if data_type_stream_size == -1 {
                    self.stream_size = -1;
                } else {
                    self.stream_size += data_type_stream_size;
                }
            }

            self.args.push((arg_name, Arc::new(data_type)));
        }

        true
    }

    pub fn len(&self) -> usize {
        self.args.len()
    }

    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
    }

    pub fn stream_size(&self) -> i32 {
        self.stream_size
    }

    pub fn args(&self) -> &[(String, Arc<DataType>)] {
        &self.args
    }

    /// `MethodArgs::clientSafety`: OR of every argument type.
    pub fn client_safety(&self) -> u32 {
        let mut safety = CLIENT_SAFE;
        for (_, data_type) in &self.args {
            safety |= data_type.client_safety();
        }
        safety
    }

    /// `MethodArgs::addToMD5`: each argument's type digest in order.
    pub fn add_to_md5(&self, md5: &mut Md5) {
        for (_, data_type) in &self.args {
            data_type.add_to_md5(md5);
        }
    }
}

// ---------------------------------------------------------------------------
// Exposed method message ranges
// ---------------------------------------------------------------------------

/// `ExposedMessageRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExposedMessageRange {
    pub first_msg_id: i32,
    pub last_msg_id: i32,
}

impl ExposedMessageRange {
    pub fn new(first_msg_id: i32, last_msg_id: i32) -> Self {
        Self {
            first_msg_id,
            last_msg_id,
        }
    }

    /// `contains`.
    pub fn contains(&self, msg_id: i32) -> bool {
        self.first_msg_id <= msg_id && msg_id <= self.last_msg_id
    }

    /// `numSlots`.
    pub fn num_slots(&self) -> i32 {
        self.last_msg_id - self.first_msg_id + 1
    }
}

/// `ExposedMethodMessageRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExposedMethodMessageRange {
    pub range: ExposedMessageRange,
}

impl ExposedMethodMessageRange {
    pub fn new(first_msg_id: i32, last_msg_id: i32) -> Self {
        Self {
            range: ExposedMessageRange::new(first_msg_id, last_msg_id),
        }
    }

    /// The wrapped range's slot count.
    pub fn num_slots(&self) -> i32 {
        self.range.num_slots()
    }

    /// `ExposedMethodMessageRange::msgIDFromExposedID`.
    pub fn msg_id_from_exposed_id(&self, exposed_id: i32, num_exposed: i32) -> (i16, i16) {
        let num_remaining_slots = self.num_remaining_slots(num_exposed);

        if exposed_id < num_remaining_slots {
            ((self.range.first_msg_id + exposed_id) as i16, -1)
        } else {
            let overflow = exposed_id - num_remaining_slots;
            let first_overflow_slot = self.range.first_msg_id + num_remaining_slots;
            (
                (first_overflow_slot + overflow / 256) as i16,
                (overflow % 256) as i16,
            )
        }
    }

    /// `ExposedMethodMessageRange::exposedIDFromMsgID`.
    ///
    /// `sub_index` is the byte that follows the message id on the wire; the
    /// reference only reads it when the offset falls into the overflow slots.
    pub fn exposed_id_from_msg_id(&self, msg_id: i32, sub_index: u8, num_exposed: i32) -> i32 {
        let num_remaining_slots = self.num_remaining_slots(num_exposed);
        let offset = msg_id - self.range.first_msg_id;

        if offset < num_remaining_slots {
            offset
        } else {
            num_remaining_slots + 256 * (offset - num_remaining_slots) + i32::from(sub_index)
        }
    }

    /// `needsSubID`.
    pub fn needs_sub_id(&self, exposed_id: i32, num_exposed: i32) -> bool {
        exposed_id >= self.num_remaining_slots(num_exposed)
    }

    /// `numRemainingSlots`.
    fn num_remaining_slots(&self, num_exposed: i32) -> i32 {
        self.range.num_slots() - self.num_sub_slots(num_exposed)
    }

    /// `numSubSlots`.
    fn num_sub_slots(&self, num_exposed: i32) -> i32 {
        let num_excess = num_exposed - self.range.num_slots();
        if num_excess > 0 {
            num_excess / 255 + 1
        } else {
            0
        }
    }
}

// ---------------------------------------------------------------------------
// MethodDescription
// ---------------------------------------------------------------------------

/// One method of one component of an entity or interface (`MethodDescription`).
#[derive(Debug, Clone, PartialEq)]
pub struct MethodDescription {
    /// The `MemberDescription` base.
    pub member: MemberDescription,
    /// `flags_`: component in the low two bits, exposure bits above.
    flags: u8,
    /// `args_`.
    args: MethodArgs,
    /// `returnValues_`.
    return_values: MethodArgs,
    /// `hasReturnValues_`.
    has_return_values: bool,
    /// `internalIndex_`.
    internal_index: i32,
    /// `exposedIndex_`.
    exposed_index: i32,
    /// `exposedMsgID_`.
    exposed_msg_id: i16,
    /// `exposedSubMsgID_`.
    exposed_sub_msg_id: i16,
    /// `priority_`: squared `DetailDistance`.
    priority: f32,
    /// `replayExposureLevel_`.
    replay_exposure_level: ReplayExposureLevel,
    /// `components_`: implementing component names ("" is the entity scope).
    components: BTreeSet<String>,
    /// `isComponentised_`.
    is_componentised: bool,
}

impl Default for MethodDescription {
    fn default() -> Self {
        Self {
            member: MemberDescription::default(),
            flags: 0,
            args: MethodArgs::default(),
            return_values: MethodArgs::default(),
            has_return_values: false,
            internal_index: -1,
            exposed_index: -1,
            exposed_msg_id: -1,
            exposed_sub_msg_id: -1,
            priority: DEFAULT_DETAIL_DISTANCE,
            replay_exposure_level: ReplayExposureLevel::OtherClients,
            components: BTreeSet::new(),
            is_componentised: false,
        }
    }
}

impl MethodDescription {
    pub fn name(&self) -> &str {
        &self.member.name
    }

    /// `flags_`, for signature comparison and the digest.
    pub fn flags(&self) -> u8 {
        self.flags
    }

    pub fn args(&self) -> &MethodArgs {
        &self.args
    }

    pub fn return_values(&self) -> &MethodArgs {
        &self.return_values
    }

    /// `hasReturnValues()`.
    pub fn has_return_values(&self) -> bool {
        self.has_return_values
    }

    pub fn internal_index(&self) -> i32 {
        self.internal_index
    }

    pub fn exposed_index(&self) -> i32 {
        self.exposed_index
    }

    pub fn exposed_msg_id(&self) -> i16 {
        self.exposed_msg_id
    }

    /// `hasSubMsgID()`.
    pub fn has_sub_msg_id(&self) -> bool {
        self.exposed_sub_msg_id != -1
    }

    /// `priority()`: the squared detail distance.
    pub fn priority(&self) -> f32 {
        self.priority
    }

    pub fn replay_exposure_level(&self) -> ReplayExposureLevel {
        self.replay_exposure_level
    }

    /// `component()`: the low two bits.
    pub fn component(&self) -> Component {
        match self.flags & 0x3 {
            1 => Component::Cell,
            2 => Component::Base,
            _ => Component::Client,
        }
    }

    fn set_component(&mut self, component: Component) {
        self.flags |= component as u8;
    }

    /// `isExposed()`.
    pub fn is_exposed(&self) -> bool {
        self.flags & (IS_EXPOSED_TO_ALL_CLIENTS | IS_EXPOSED_TO_OWN_CLIENT) != 0
    }

    /// `isExposedToOwnClientOnly()`.
    pub fn is_exposed_to_own_client_only(&self) -> bool {
        self.flags & IS_EXPOSED_TO_OWN_CLIENT != 0 && self.flags & IS_EXPOSED_TO_ALL_CLIENTS == 0
    }

    /// `isExposedToAllClientsOnly()`.
    pub fn is_exposed_to_all_clients_only(&self) -> bool {
        self.flags & IS_EXPOSED_TO_ALL_CLIENTS != 0 && self.flags & IS_EXPOSED_TO_OWN_CLIENT == 0
    }

    /// `isExposedToDefault()`: both bits, the `<Exposed>`-with-no-value shape.
    pub fn is_exposed_to_default(&self) -> bool {
        self.flags & IS_EXPOSED_TO_OWN_CLIENT != 0 && self.flags & IS_EXPOSED_TO_ALL_CLIENTS != 0
    }

    fn set_exposed_to_all_clients(&mut self) {
        self.flags |= IS_EXPOSED_TO_ALL_CLIENTS;
    }

    fn set_exposed_to_own_client(&mut self) {
        self.flags |= IS_EXPOSED_TO_OWN_CLIENT;
    }

    fn set_exposed_to_default(&mut self) {
        self.flags |= IS_EXPOSED_TO_OWN_CLIENT | IS_EXPOSED_TO_ALL_CLIENTS;
    }

    /// `clientSafety()`, forwarded to the argument list.
    pub fn client_safety(&self) -> u32 {
        self.args.client_safety()
    }

    /// `MethodDescription::parse`.
    pub fn parse(
        &mut self,
        section: &DataSection,
        interface_name: &str,
        component: Component,
        num_latest_event_members: Option<&mut u32>,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let result = self.member.parse(
            section,
            interface_name,
            component == Component::Client,
            num_latest_event_members,
            diag,
        );

        self.member.name = section.section_name().to_string();

        if self.member.name.is_empty() {
            diag.warning("MethodDescription::parse: Missing <Name> tag\n");
            return false;
        }

        if let Some(exposed) = section.find_child("Exposed") {
            if component == Component::Client {
                diag.error(format!(
                    "MethodDescription::parse: Unable to use <Exposed> tag in client method {}.\n",
                    self.member.name
                ));
                return false;
            }

            let exposed_flag = exposed.as_string();
            if component == Component::Cell && exposed_flag == "ALL_CLIENTS" {
                self.set_exposed_to_all_clients();
            } else if exposed_flag == "OWN_CLIENT" {
                self.set_exposed_to_own_client();
            } else if exposed_flag.is_empty() {
                self.set_exposed_to_default();
            } else {
                let all_clients_hint = if component == Component::Cell {
                    " ALL_CLIENTS |"
                } else {
                    ""
                };
                diag.error(format!(
                    "MethodDescription::parse: Could not recognise {exposed_flag} flag in <Exposed> tag in {}. Use <Exposed> [{all_clients_hint} OWN_CLIENT ] </Exposed>\n",
                    self.member.name
                ));
                return false;
            }
        }

        if let Some(replay_exposure) = section.find_child("ReplayExposureLevel") {
            let level = replay_exposure.as_string();
            self.replay_exposure_level = match level {
                "NONE" => ReplayExposureLevel::None,
                "OTHER_CLIENTS" => ReplayExposureLevel::OtherClients,
                "ALL_CLIENTS" => ReplayExposureLevel::AllClients,
                _ => {
                    diag.error(format!(
                        "MethodDescription::parse: Could not recognise {level} flag in <ReplayExposureLevel> tag in {}. Use <ReplayExposureLevel> NONE | OTHER_CLIENTS | ALL_CLIENTS </ReplayExposureLevel>\n",
                        self.member.name
                    ));
                    return false;
                }
            };
        }

        self.set_component(component);

        match section.find_child("Args") {
            Some(args_section) => {
                if !self.args.parse(args_section, false, ctx, diag) {
                    diag.error(format!(
                        "MethodDescription::parse: Failed to parse arguments of {}\n",
                        self.member.name
                    ));
                    return false;
                }
            }
            None => {
                // Old style: <Arg> children directly under the method.
                if !self.args.parse(section, true, ctx, diag) {
                    diag.error(format!(
                        "MethodDescription::parse: Failed to parse arguments of {}\n",
                        self.member.name
                    ));
                    return false;
                }
            }
        }

        if let Some(return_values) = section.find_child("ReturnValues") {
            self.has_return_values = true;

            if !self.return_values.parse(return_values, false, ctx, diag) {
                diag.error(format!(
                    "MethodDescription::parse: Failed to parse return values of {}\n",
                    self.member.name
                ));
                return false;
            }
        }

        self.priority = section
            .open_section("DetailDistance")
            .map_or(DEFAULT_DETAIL_DISTANCE, |s| {
                s.as_float(DEFAULT_DETAIL_DISTANCE)
            });
        if self.priority != DEFAULT_DETAIL_DISTANCE {
            self.priority *= self.priority;
        }

        result
    }

    /// `MethodDescription::getVarLenHeaderSize`: sub-indexed methods must use
    /// the default header so the receiver can size the message first.
    pub fn get_var_len_header_size(&self) -> u32 {
        if self.is_exposed() && self.has_sub_msg_id() {
            DEFAULT_VARIABLE_LENGTH_HEADER_SIZE
        } else {
            self.member.var_len_header_size
        }
    }

    /// `MethodDescription::streamSize( isFromServer )`.
    pub fn stream_size(&self, is_from_server: bool) -> i32 {
        let mut result = self.args.stream_size();

        if result < 0 || (self.is_exposed() && self.has_sub_msg_id()) {
            return -(self.get_var_len_header_size() as i32);
        }

        // The object id rides along when it is not the client talking to the
        // cell.
        if self.is_exposed() && is_from_server && self.component() == Component::Cell {
            result += 4; // sizeof( EntityID )
        }

        result
    }

    /// `isSignatureEqual`: flags byte, arguments and return values.
    pub fn is_signature_equal(&self, other: &MethodDescription) -> bool {
        self.flags == other.flags
            && self.args == other.args
            && self.return_values == other.return_values
    }

    /// `addImplementingComponent`.
    pub fn add_implementing_component(
        &mut self,
        component_name: &str,
        diag: &mut Diagnostics,
    ) -> bool {
        if !component_name.is_empty() {
            if self.components.contains("") {
                diag.error(format!(
                    "MethodDescription::addImplementingComponent: Method '{}' of component '{}' already defined in entity scope",
                    self.member.name, component_name
                ));
                return false;
            }
            self.is_componentised = true;
        } else if self.is_componentised {
            diag.error(format!(
                "MethodDescription::addImplementingComponent: Entity method '{}' already defined by one or more components",
                self.member.name
            ));
            return false;
        }

        if !self.components.insert(component_name.to_string()) {
            let defined_by = if component_name.is_empty() {
                "entity".to_string()
            } else {
                format!("'{component_name}' component")
            };
            diag.error(format!(
                "MethodDescription::addImplementingComponent: Method '{}' already defined by {}",
                self.member.name, defined_by
            ));
            return false;
        }

        true
    }

    /// `isImplementedBy`.
    pub fn is_implemented_by(&self, component_name: &str) -> bool {
        self.components.contains(component_name)
    }

    /// `isComponentised`.
    pub fn is_componentised(&self) -> bool {
        self.is_componentised
    }

    /// `MethodDescription::addToMD5( md5, legacyExposedIndex )`.
    pub fn add_to_md5(&self, md5: &mut Md5, legacy_exposed_index: i32) {
        self.member.add_to_md5(md5, self.stream_size(true));
        md5.append_u8(self.flags);
        self.args.add_to_md5(md5);
        md5.append_i32(legacy_exposed_index);
    }

    /// The `toString()` shape used in messages: `Method <iface>.<name>()`.
    pub fn to_method_string(&self) -> String {
        format!(
            "Method {}.{}()",
            self.member.interface_name, self.member.name
        )
    }

    /// `setExposedMsgID`.
    pub fn set_exposed_msg_id(
        &mut self,
        exposed_id: i32,
        num_exposed: i32,
        range: Option<&ExposedMethodMessageRange>,
        diag: &mut Diagnostics,
    ) {
        self.exposed_index = exposed_id;

        if let Some(range) = range {
            let (msg_id, sub_msg_id) = range.msg_id_from_exposed_id(exposed_id, num_exposed);
            self.exposed_msg_id = msg_id;
            self.exposed_sub_msg_id = sub_msg_id;

            let var_hdr_size = self.member.var_len_header_size;
            if self.exposed_sub_msg_id != -1 && var_hdr_size != DEFAULT_VARIABLE_LENGTH_HEADER_SIZE
            {
                diag.warning(format!(
                    "MethodDescription::setExposedMsgID: VariableLengthHeaderSize value ({}) will be ignored for {} because a sub-index is required for this method, the default {}-byte(s) header will be used instead.\n",
                    var_hdr_size,
                    self.to_method_string(),
                    DEFAULT_VARIABLE_LENGTH_HEADER_SIZE
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// EntityMethodDescriptions
// ---------------------------------------------------------------------------

/// The per-entity-type method table (`EntityMethodDescriptions`).
#[derive(Debug, Clone, Default)]
pub struct EntityMethodDescriptions {
    internal_methods: Vec<MethodDescription>,
    /// name -> index into `internal_methods`.
    map: HashMap<String, usize>,
    /// Indices of exposed methods, in declaration order until
    /// `set_exposed_msg_ids` sorts them.
    exposed_methods: Vec<usize>,
    max_exposed_method_count: i32,
}

impl EntityMethodDescriptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// `EntityMethodDescriptions::init`: parse every child as a method of the
    /// given component. Callable repeatedly for `<Implements>` interfaces.
    ///
    /// Kept flat to stay a literal transcription of the reference's
    /// parameter list.
    #[allow(clippy::too_many_arguments)]
    pub fn init(
        &mut self,
        methods: Option<&DataSection>,
        component: Component,
        interface_name: &str,
        component_name: &str,
        mut num_latest_event_members: Option<&mut u32>,
        ctx: &TypeContext,
        diag: &mut Diagnostics,
    ) -> bool {
        let Some(methods) = methods else {
            diag.warning("EntityMethodDescriptions::init: pMethods is NULL\n");
            return false;
        };

        for section in children(methods) {
            let mut method = MethodDescription::default();

            let latest = num_latest_event_members.as_deref_mut();
            if !method.parse(section, interface_name, component, latest, ctx, diag) {
                diag.warning(format!("Error parsing method {}\n", method.name()));
                return false;
            }

            if component == Component::Client {
                // All client methods are exposed.
                method.set_exposed_to_all_clients_public();
            }

            method.internal_index = self.internal_methods.len() as i32;
            if !method.add_implementing_component(component_name, diag) {
                return false;
            }

            match self.map.get(method.name()) {
                Some(&existing_index) => {
                    let existing = &mut self.internal_methods[existing_index];
                    if method.is_signature_equal(existing) {
                        // Same method via another component.
                        let name = method.name().to_string();
                        if !existing.add_implementing_component(component_name, diag) {
                            return false;
                        }
                        let _ = name;
                    } else {
                        diag.error(format!(
                            "EntityMethodDescriptions::init: Method '{}' already defined with a different signature \n",
                            method.name()
                        ));
                        return false;
                    }
                }
                None => {
                    let is_exposed = method.is_exposed();
                    let internal_index = method.internal_index as usize;
                    self.map.insert(method.name().to_string(), internal_index);
                    self.internal_methods.push(method);

                    if is_exposed {
                        self.exposed_methods.push(internal_index);
                    }
                }
            }
        }

        self.check_exposed_for_client_safety(interface_name, diag)
    }

    /// `checkExposedForClientSafety`: warn on PYTHON args, error on MAILBOX.
    pub fn check_exposed_for_client_safety(
        &self,
        interface_name: &str,
        diag: &mut Diagnostics,
    ) -> bool {
        let mut methods_ok = true;

        for &iindex in &self.exposed_methods {
            let method = &self.internal_methods[iindex];

            let client_safety = method.client_safety();
            let method_type = match method.component() {
                Component::Client => "a client",
                _ => "an Exposed",
            };

            if client_safety & CLIENT_UNSAFE != 0 {
                diag.warning(format!(
                    "{interface_name}.{} is {method_type} method that takes a PYTHON arg (potential security hole)\n",
                    method.name()
                ));
            }

            if client_safety & CLIENT_UNUSABLE != 0 {
                diag.error(format!(
                    "{interface_name}.{} is {method_type} method that takes a MAILBOX arg (type is not able to be sent to or from client)\n",
                    method.name()
                ));
                methods_ok = false;
            }
        }

        methods_ok
    }

    /// `setExposedMsgIDs`: sort exposed ids so fixed-size messages come
    /// first, then hand out ids (and message ids, when a range is given).
    pub fn set_exposed_msg_ids(
        &mut self,
        max_exposed_method_count: i32,
        range: Option<&ExposedMethodMessageRange>,
        diag: &mut Diagnostics,
    ) {
        self.max_exposed_method_count = max_exposed_method_count;

        // ExposedMethodsSortHelper on int16-truncated stream sizes.
        self.exposed_methods.sort_by(|&a, &b| {
            let size1 = self.internal_methods[a].stream_size(true) as i16;
            let size2 = self.internal_methods[b].stream_size(true) as i16;
            // Both variable size: smaller absolute header first.
            // Both fixed: ascending. Mixed: fixed is always lesser.
            match (size1 < 0, size2 < 0) {
                (true, true) => (-size1).cmp(&(-size2)),
                (false, false) => size1.cmp(&size2),
                // "return size1 > size2": fixed (positive) sorts first.
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
            }
        });

        let num_exposed = self.exposed_methods.len() as i32;
        let exposed_methods = self.exposed_methods.clone();
        for (exposed_id, &iindex) in exposed_methods.iter().enumerate() {
            self.internal_methods[iindex].set_exposed_msg_id(
                exposed_id as i32,
                num_exposed,
                range,
                diag,
            );
        }
    }

    /// `size()`.
    pub fn size(&self) -> usize {
        self.internal_methods.len()
    }

    /// `internalMethod( index )`.
    pub fn internal_method(&self, index: usize) -> Option<&MethodDescription> {
        self.internal_methods.get(index)
    }

    /// The internal method list in declaration order, for digest and dump
    /// walks (`EntityMethod` iteration).
    pub fn internal_methods(&self) -> &[MethodDescription] {
        &self.internal_methods
    }

    /// `exposedMethod( index )`.
    pub fn exposed_method(&self, index: usize) -> Option<&MethodDescription> {
        self.exposed_methods
            .get(index)
            .map(|&iindex| &self.internal_methods[iindex])
    }

    /// The number of exposed methods.
    pub fn num_exposed(&self) -> usize {
        self.exposed_methods.len()
    }

    /// `find( name )`.
    pub fn find(&self, name: &str) -> Option<&MethodDescription> {
        self.map
            .get(name)
            .and_then(|&i| self.internal_methods.get(i))
    }
}

/// Public shim: `init` needs to flip exposure on client methods from outside
/// the module-private setters, matching `setExposedToAllClients()` being
/// public in the reference.
impl MethodDescription {
    pub fn set_exposed_to_all_clients_public(&mut self) {
        self.set_exposed_to_all_clients();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_xml(xml: &str) -> DataSection {
        resources::datasection::parse("", xml.as_bytes()).expect("valid fixture xml")
    }

    fn ctx() -> TypeContext {
        TypeContext::default()
    }

    fn parse_method(xml: &str, component: Component) -> (MethodDescription, Diagnostics) {
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml(xml);
        let ok = method.parse(&section, "Avatar", component, None, &ctx(), &mut diag);
        assert!(ok, "parse must succeed for {xml}: {:?}", diag.messages());
        (method, diag)
    }

    #[test]
    fn defaults_match_the_reference_constructor() {
        let method = MethodDescription::default();
        assert_eq!(method.flags(), 0);
        assert_eq!(method.internal_index(), -1);
        assert_eq!(method.exposed_index(), -1);
        assert_eq!(method.exposed_msg_id(), -1);
        assert!(!method.has_sub_msg_id());
        assert_eq!(method.priority(), f32::MAX);
        assert_eq!(
            method.replay_exposure_level(),
            ReplayExposureLevel::OtherClients
        );
        assert!(!method.has_return_values());
        assert!(!method.is_exposed());
        assert!(!method.is_componentised());
    }

    #[test]
    fn old_style_args_read_only_arg_children() {
        let (method, diag) = parse_method(
            "<jump> \
                 <Arg> INT32 </Arg> \
                 <Arg> FLOAT32 </Arg> \
                 <Exposed> </Exposed> \
             </jump>",
            Component::Cell,
        );
        assert_eq!(method.args().len(), 2);
        assert_eq!(method.args().stream_size(), 8);
        assert!(method.is_exposed_to_default());
        assert!(method.component() == Component::Cell);
        assert!(!diag.has_errors());
    }

    #[test]
    fn new_style_args_carry_names_and_detect_duplicates() {
        let (method, _) = parse_method(
            "<jump> \
                 <Args> \
                     <amount> INT32 </amount> \
                     <target> UINT32 </target> \
                 </Args> \
                 <Exposed> OWN_CLIENT </Exposed> \
             </jump>",
            Component::Base,
        );
        let args = method.args().args();
        assert_eq!(args[0].0, "amount");
        assert_eq!(args[1].0, "target");
        assert!(method.is_exposed_to_own_client_only());
        assert!(method.component() == Component::Base);

        // Duplicate names fail.
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<jump> <Args> <a> INT32 </a> <a> INT32 </a> </Args> </jump>");
        assert!(!method.parse(&section, "Avatar", Component::Base, None, &ctx(), &mut diag));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Duplicate argument name 'a'"))
        );
    }

    #[test]
    fn exposed_tag_rules_per_component() {
        // ALL_CLIENTS on a cell method.
        let (method, _) = parse_method(
            "<shout> <Exposed> ALL_CLIENTS </Exposed> </shout>",
            Component::Cell,
        );
        assert!(method.is_exposed_to_all_clients_only());

        // ALL_CLIENTS on a base method is not recognised.
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<shout> <Exposed> ALL_CLIENTS </Exposed> </shout>");
        assert!(!method.parse(&section, "Avatar", Component::Base, None, &ctx(), &mut diag));
        assert_eq!(
            diag.errors(),
            vec![
                "MethodDescription::parse: Could not recognise ALL_CLIENTS flag in <Exposed> tag in shout. Use <Exposed> [ OWN_CLIENT ] </Exposed>\n"
            ]
        );

        // On a cell method the hint mentions ALL_CLIENTS.
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<shout> <Exposed> LOUD </Exposed> </shout>");
        assert!(!method.parse(&section, "Avatar", Component::Cell, None, &ctx(), &mut diag));
        assert!(diag.errors()[0].contains("[ ALL_CLIENTS | OWN_CLIENT ]"));

        // Exposed on a client method is refused outright.
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<hush> <Exposed> </Exposed> </hush>");
        assert!(!method.parse(
            &section,
            "Avatar",
            Component::Client,
            None,
            &ctx(),
            &mut diag
        ));
        assert_eq!(
            diag.errors(),
            vec!["MethodDescription::parse: Unable to use <Exposed> tag in client method hush.\n"]
        );
    }

    #[test]
    fn client_methods_are_auto_exposed_and_component_bits_land() {
        let (method, _) = parse_method("<wave> </wave>", Component::Client);
        assert_eq!(method.component(), Component::Client);
        // Auto-exposure happens in EntityMethodDescriptions::init, not parse.
        assert!(!method.is_exposed());
    }

    #[test]
    fn replay_exposure_levels_parse() {
        for (tag, expected) in [
            ("NONE", ReplayExposureLevel::None),
            ("OTHER_CLIENTS", ReplayExposureLevel::OtherClients),
            ("ALL_CLIENTS", ReplayExposureLevel::AllClients),
        ] {
            let (method, _) = parse_method(
                &format!("<m> <ReplayExposureLevel> {tag} </ReplayExposureLevel> </m>"),
                Component::Cell,
            );
            assert_eq!(method.replay_exposure_level(), expected);
        }

        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<m> <ReplayExposureLevel> SOMETIMES </ReplayExposureLevel> </m>");
        assert!(!method.parse(&section, "Avatar", Component::Cell, None, &ctx(), &mut diag));
        assert!(diag.errors()[0].contains(
            "Use <ReplayExposureLevel> NONE | OTHER_CLIENTS | ALL_CLIENTS </ReplayExposureLevel>"
        ));
    }

    #[test]
    fn return_values_are_kept_but_not_part_of_args() {
        let (method, _) = parse_method(
            "<query> <ReturnValues> <answer> INT32 </answer> </ReturnValues> </query>",
            Component::Base,
        );
        assert!(method.has_return_values());
        assert_eq!(method.return_values().len(), 1);
        assert_eq!(method.return_values().stream_size(), 4);

        // Missing ReturnValues tag means no two-way call.
        let (method, _) = parse_method("<query> </query>", Component::Base);
        assert!(!method.has_return_values());
    }

    #[test]
    fn detail_distance_is_squared() {
        let (method, _) = parse_method(
            "<m> <DetailDistance> 100 </DetailDistance> </m>",
            Component::Cell,
        );
        assert_eq!(method.priority(), 10000.0);

        let (method, _) = parse_method("<m> </m>", Component::Cell);
        assert_eq!(method.priority(), f32::MAX);
    }

    #[test]
    fn stream_size_follows_the_reference_branches() {
        // Fixed args, not exposed: plain sum.
        let (method, _) = parse_method(
            "<m> <Arg> INT32 </Arg> <Arg> INT32 </Arg> </m>",
            Component::Cell,
        );
        assert_eq!(method.stream_size(false), 8);
        assert_eq!(method.stream_size(true), 8);

        // Exposed cell method from the server: EntityID rides along.
        let (method, _) = parse_method(
            "<m> <Arg> INT32 </Arg> <Exposed> ALL_CLIENTS </Exposed> </m>",
            Component::Cell,
        );
        assert_eq!(method.stream_size(true), 8);
        assert_eq!(method.stream_size(false), 4);

        // Variable args: negated header width.
        let (method, _) = parse_method("<m> <Arg> STRING </Arg> </m>", Component::Cell);
        assert_eq!(method.stream_size(true), -1);

        // Client component with custom header keeps the member header.
        let (method, _) = parse_method(
            "<m> <Arg> STRING </Arg> <VariableLengthHeaderSize> 2 </VariableLengthHeaderSize> </m>",
            Component::Client,
        );
        assert_eq!(method.stream_size(true), -2);
    }

    #[test]
    fn digest_is_member_flags_args_legacy_index() {
        let (method, _) = parse_method(
            "<hit> <Arg> INT32 </Arg> <Exposed> ALL_CLIENTS </Exposed> </hit>",
            Component::Cell,
        );
        let mut method = method;
        method.internal_index = 3;

        let mut md5 = Md5::new();
        method.add_to_md5(&mut md5, 7);

        let mut expected = Md5::new();
        expected.append_text("hit"); // fixed size: no header bytes
        expected.append_u8(0x4 | 0x1); // exposed to all + CELL component
        expected.append_cstr("Int");
        expected.append_i32(4);
        expected.append_i32(7);
        assert_eq!(md5.quote(), expected.quote());
    }

    #[test]
    fn implementing_component_rules() {
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();

        assert!(method.add_implementing_component("", &mut diag));
        assert!(!method.is_componentised());
        // Entity scope twice is an error.
        assert!(!method.add_implementing_component("", &mut diag));
        assert!(diag.errors()[0].contains("already defined by entity"));

        // Component after entity scope is fine; entity after component is not.
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        assert!(method.add_implementing_component("movement", &mut diag));
        assert!(method.is_componentised());
        assert!(method.is_implemented_by("movement"));
        assert!(!method.add_implementing_component("", &mut diag));
        assert!(diag.errors()[0].contains("already defined by one or more components"));
    }

    #[test]
    fn init_merges_interfaces_and_rejects_signature_conflicts() {
        let ctx = ctx();

        // Two components implementing the same cell method with identical
        // specs merge into one entry (flags carry the component type only,
        // so the merge is per component — the reference compares raw
        // flags_ via isSignatureEqual).
        let first = parse_xml("<root> <jump> <Arg> INT32 </Arg> </jump> </root>");
        let shared = parse_xml("<root> <jump> <Arg> INT32 </Arg> </jump> <order> </order> </root>");
        let conflicting = parse_xml("<root> <jump> <Arg> STRING </Arg> </jump> </root>");

        let mut methods = EntityMethodDescriptions::new();
        let mut diag = Diagnostics::new();
        assert!(methods.init(
            Some(&first),
            Component::Cell,
            "Avatar",
            "movement",
            None,
            &ctx,
            &mut diag
        ));
        assert!(methods.init(
            Some(&shared),
            Component::Cell,
            "Avatar",
            "animation",
            None,
            &ctx,
            &mut diag
        ));
        assert_eq!(methods.size(), 2, "jump is shared, not duplicated");

        // jump exists once, found by name.
        let jump = methods.find("jump").expect("jump");
        assert_eq!(jump.component(), Component::Cell, "first parse wins");

        // Re-declaring the same signature at entity scope fails on the
        // component bookkeeping: the reference's components_ set rejects
        // the duplicate "" insert.
        let mut diag = Diagnostics::new();
        assert!(!methods.init(
            Some(&first),
            Component::Cell,
            "Avatar",
            "",
            None,
            &ctx,
            &mut diag
        ));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("already defined by one or more components"))
        );

        let mut diag = Diagnostics::new();
        assert!(!methods.init(
            Some(&conflicting),
            Component::Cell,
            "Avatar",
            "",
            None,
            &ctx,
            &mut diag
        ));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("already defined with a different signature"))
        );
    }

    #[test]
    fn client_methods_are_auto_exposed_in_init() {
        let ctx = ctx();
        let client_methods = parse_xml("<root> <wave> </wave> </root>");

        let mut methods = EntityMethodDescriptions::new();
        let mut diag = Diagnostics::new();
        assert!(methods.init(
            Some(&client_methods),
            Component::Client,
            "Avatar",
            "",
            None,
            &ctx,
            &mut diag
        ));
        assert_eq!(methods.num_exposed(), 1);
        let wave = methods.find("wave").expect("wave");
        assert!(wave.is_exposed_to_all_clients_only());
    }

    #[test]
    fn exposed_ids_sort_fixed_before_variable_and_assign_indices() {
        let ctx = ctx();
        let methods_xml = parse_xml(
            "<root> \
                 <var> <Arg> STRING </Arg> <Exposed> </Exposed> </var> \
                 <fixed_two> <Arg> INT32 </Arg> <Arg> INT32 </Arg> <Exposed> </Exposed> </fixed_two> \
                 <fixed_one> <Arg> INT32 </Arg> <Exposed> </Exposed> </fixed_one> \
             </root>",
        );

        let mut methods = EntityMethodDescriptions::new();
        let mut diag = Diagnostics::new();
        assert!(methods.init(
            Some(&methods_xml),
            Component::Cell,
            "Avatar",
            "",
            None,
            &ctx,
            &mut diag
        ));

        let mut diag = Diagnostics::new();
        methods.set_exposed_msg_ids(3, None, &mut diag);

        // Fixed sizes first (ascending), then variable.
        assert_eq!(methods.exposed_method(0).expect("id 0").name(), "fixed_one");
        assert_eq!(methods.exposed_method(1).expect("id 1").name(), "fixed_two");
        assert_eq!(methods.exposed_method(2).expect("id 2").name(), "var");

        assert_eq!(methods.exposed_method(0).expect("id 0").exposed_index(), 0);
    }

    #[test]
    fn exposed_message_ranges_map_ids_and_sub_ids() {
        let range = ExposedMethodMessageRange::new(100, 130);

        // Within the range: plain ids, no sub id.
        assert_eq!(range.msg_id_from_exposed_id(0, 10), (100, -1));
        assert_eq!(range.msg_id_from_exposed_id(9, 10), (109, -1));
        assert!(!range.needs_sub_id(9, 10));

        // Overflow once the number of exposed methods exceeds the slots.
        // 31 slots, 50 exposed: one sub slot, 30 plain slots remain.
        assert_eq!(range.msg_id_from_exposed_id(30, 50), (130, 0));
        assert_eq!(range.msg_id_from_exposed_id(31, 50), (130, 1));
        assert_eq!(range.msg_id_from_exposed_id(286, 50), (131, 0));
        assert!(range.needs_sub_id(30, 50));

        // Inverse mapping, including the sub index byte.
        assert_eq!(range.exposed_id_from_msg_id(105, 0, 10), 5);
        assert_eq!(range.exposed_id_from_msg_id(129, 7, 50), 29);
        assert_eq!(range.exposed_id_from_msg_id(130, 3, 50), 30 + 3);
        assert_eq!(range.exposed_id_from_msg_id(131, 0, 50), 30 + 256);
    }

    #[test]
    fn client_safety_checks_exposed_methods() {
        let ctx = ctx();
        // A MAILBOX argument on an exposed method is a hard error.
        let methods_xml =
            parse_xml("<root> <m> <Arg> MAILBOX </Arg> <Exposed> </Exposed> </m> </root>");

        let mut methods = EntityMethodDescriptions::new();
        let mut diag = Diagnostics::new();
        assert!(!methods.init(
            Some(&methods_xml),
            Component::Cell,
            "Avatar",
            "",
            None,
            &ctx,
            &mut diag
        ));
        assert!(diag.errors().iter().any(|m| m.contains(
            "Avatar.m is an Exposed method that takes a MAILBOX arg (type is not able to be sent to or from client)"
        )));
    }

    #[test]
    fn invalid_new_style_arg_fails_with_the_return_type_message() {
        // New-style <Args> sections take every child as an argument; a bad
        // type reports through MethodArgs::parse (which says "Return type"
        // even for arguments, matching the reference).
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<m> <Args> <bad> NOT_A_TYPE </bad> </Args> </m>");
        assert!(!method.parse(&section, "Avatar", Component::Cell, None, &ctx(), &mut diag));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Return type #0 is invalid"))
        );
    }

    #[test]
    fn old_style_ignores_non_arg_children() {
        // Old-style <Arg> parsing only consumes children named <Arg>, so an
        // unrelated child section is neither an argument nor an error.
        let (method, diag) = parse_method(
            "<m> <Arg> INT32 </Arg> <dontcare> 42 </dontcare> </m>",
            Component::Cell,
        );
        assert!(!diag.has_errors());
        assert_eq!(method.args().len(), 1);
    }

    #[test]
    fn method_args_report_length_and_stream_size() {
        let empty = MethodArgs::default();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let (method, _) = parse_method(
            "<jump> <Arg> INT32 </Arg> <Arg> STRING </Arg> </jump>",
            Component::Cell,
        );
        let args = method.args();
        assert!(!args.is_empty());
        assert_eq!(args.len(), 2);
        // INT32 (4) + STRING (-1) makes the whole argument list variable.
        assert_eq!(args.stream_size(), -1);
    }

    /// Duplicate names are only tolerated for the implicit old-style "Arg".
    #[test]
    fn new_style_arg_collisions_are_only_tolerated_for_arg() {
        let (method, _) = parse_method(
            "<jump> <Args> <Arg> INT32 </Arg> <Arg> STRING </Arg> </Args> </jump>",
            Component::Cell,
        );
        assert_eq!(method.args().len(), 2);
        assert_eq!(method.args().args()[0].0, "Arg");
        assert_eq!(method.args().args()[1].0, "Arg");
    }

    #[test]
    fn message_ranges_report_containment_and_slots() {
        let range = ExposedMessageRange::new(100, 130);
        assert!(range.contains(100));
        assert!(range.contains(130));
        assert!(range.contains(115));
        assert!(!range.contains(99));
        assert!(!range.contains(131));
        assert_eq!(range.num_slots(), 31);

        let exposed_range = ExposedMethodMessageRange::new(200, 230);
        assert_eq!(exposed_range.num_slots(), 31);
    }

    #[test]
    fn arg_and_return_value_failures_are_named() {
        // Old-style argument with an unknown type.
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<m> <Arg> NOT_A_TYPE </Arg> </m>");
        assert!(!method.parse(&section, "Avatar", Component::Cell, None, &ctx(), &mut diag));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Failed to parse arguments of m"))
        );

        // Return values with an unknown type.
        let mut method = MethodDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml("<m> <ReturnValues> <Arg> NOT_A_TYPE </Arg> </ReturnValues> </m>");
        assert!(!method.parse(&section, "Avatar", Component::Cell, None, &ctx(), &mut diag));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Failed to parse return values of m"))
        );
    }

    #[test]
    fn add_implementing_component_rejects_scope_conflicts() {
        let mut diag = Diagnostics::new();

        // Entity scope first, then a component: the entity definition wins.
        let (mut method, _) = parse_method("<jump> </jump>", Component::Cell);
        assert!(method.add_implementing_component("", &mut diag));
        assert!(!method.add_implementing_component("movement", &mut diag));
        assert!(diag.errors().iter().any(|m| {
            m.contains("Method 'jump' of component 'movement' already defined in entity scope")
        }));

        // The same component claim twice names the component.
        let mut diag = Diagnostics::new();
        let (mut method, _) = parse_method("<jump> </jump>", Component::Cell);
        assert!(method.add_implementing_component("movement", &mut diag));
        assert!(!method.add_implementing_component("movement", &mut diag));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Method 'jump' already defined by 'movement' component"))
        );

        // The same entity-scope claim twice.
        let mut diag = Diagnostics::new();
        let (mut method, _) = parse_method("<jump> </jump>", Component::Cell);
        assert!(method.add_implementing_component("", &mut diag));
        assert!(!method.add_implementing_component("", &mut diag));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Method 'jump' already defined by entity"))
        );
    }

    #[test]
    fn full_ranges_force_sub_indexed_message_ids() {
        let (mut method, _) = parse_method(
            "<ping> <VariableLengthHeaderSize> 2 </VariableLengthHeaderSize> </ping>",
            Component::Client,
        );
        assert_eq!(method.to_method_string(), "Method Avatar.ping()");

        // A saturated range pushes every exposed id into the overflow
        // arithmetic, so the custom header size must be ignored.
        let range = ExposedMethodMessageRange::new(100, 100);
        let mut diag = Diagnostics::new();
        method.set_exposed_msg_id(0, 2, Some(&range), &mut diag);
        assert_eq!(method.exposed_msg_id(), 100);
        assert!(method.has_sub_msg_id());
        assert!(diag.warnings().iter().any(|m| m.contains(
            "VariableLengthHeaderSize value (2) will be ignored for Method Avatar.ping()"
        )));

        // A sub-indexed exposed method must advertise the default header, so
        // the receiver can size the message before reading the sub-index.
        method.set_exposed_to_all_clients_public();
        assert_eq!(
            method.get_var_len_header_size(),
            DEFAULT_VARIABLE_LENGTH_HEADER_SIZE
        );

        // A wide range keeps direct slots (and the custom header).
        let mut diag = Diagnostics::new();
        let wide = ExposedMethodMessageRange::new(100, 130);
        method.set_exposed_msg_id(0, 1, Some(&wide), &mut diag);
        assert_eq!(method.exposed_msg_id(), 100);
        assert!(!method.has_sub_msg_id());
        assert!(diag.warnings().is_empty());
    }

    /// Two variable-size exposed methods exercise the both-variable sort
    /// arm; the fixed one still sorts ahead of both.
    #[test]
    fn variable_size_methods_sort_by_header_then_name() {
        let ctx = ctx();
        let methods_xml = parse_xml(
            "<root> \
                 <blob_a> <Arg> BLOB </Arg> <Exposed> </Exposed> </blob_a> \
                 <blob_b> <Arg> BLOB </Arg> <Exposed> </Exposed> </blob_b> \
                 <tick> <Arg> INT32 </Arg> <Exposed> </Exposed> </tick> \
             </root>",
        );

        let mut methods = EntityMethodDescriptions::new();
        let mut diag = Diagnostics::new();
        assert!(methods.init(
            Some(&methods_xml),
            Component::Cell,
            "Avatar",
            "",
            None,
            &ctx,
            &mut diag
        ));
        methods.set_exposed_msg_ids(3, None, &mut diag);

        let mut names = Vec::new();
        for i in 0..methods.num_exposed() {
            names.push(
                methods
                    .exposed_method(i)
                    .expect("exposed method")
                    .name()
                    .to_string(),
            );
        }
        assert_eq!(names, ["tick", "blob_a", "blob_b"]);
    }

    #[test]
    fn fixed_header_sizes_survive_when_no_sub_index_is_needed() {
        let (mut method, _) = parse_method(
            "<ping> <VariableLengthHeaderSize> 2 </VariableLengthHeaderSize> </ping>",
            Component::Client,
        );
        method.set_exposed_to_all_clients_public();
        assert_eq!(method.get_var_len_header_size(), 2);

        let wide = ExposedMethodMessageRange::new(100, 130);
        let mut diag = Diagnostics::new();
        method.set_exposed_msg_id(0, 1, Some(&wide), &mut diag);
        assert!(!method.has_sub_msg_id());
        assert_eq!(method.get_var_len_header_size(), 2);
        assert!(diag.warnings().is_empty());
    }

    #[test]
    fn init_reports_a_missing_section_and_an_unparseable_method() {
        let ctx = ctx();

        let mut methods = EntityMethodDescriptions::new();
        let mut diag = Diagnostics::new();
        assert!(!methods.init(None, Component::Cell, "Avatar", "", None, &ctx, &mut diag));
        assert_eq!(
            diag.warnings(),
            vec!["EntityMethodDescriptions::init: pMethods is NULL\n"]
        );

        let broken = parse_xml("<root> <shout> <Exposed> LOUD </Exposed> </shout> </root>");
        let mut methods = EntityMethodDescriptions::new();
        let mut diag = Diagnostics::new();
        assert!(!methods.init(
            Some(&broken),
            Component::Cell,
            "Avatar",
            "",
            None,
            &ctx,
            &mut diag
        ));
        assert!(
            diag.warnings()
                .iter()
                .any(|m| m.contains("Error parsing method shout\n"))
        );
        assert_eq!(methods.size(), 0);
    }

    #[test]
    fn exposed_python_args_warn_for_client_safety() {
        let ctx = ctx();
        let methods_xml =
            parse_xml("<root> <sniff> <Arg> PYTHON </Arg> <Exposed> </Exposed> </sniff> </root>");

        let mut methods = EntityMethodDescriptions::new();
        let mut diag = Diagnostics::new();
        assert!(methods.init(
            Some(&methods_xml),
            Component::Cell,
            "Npc",
            "",
            None,
            &ctx,
            &mut diag
        ));

        // PYTHON only warns; the method stays usable.
        assert!(methods.check_exposed_for_client_safety("Npc", &mut diag));
        assert!(diag.warnings().iter().any(|m| m.contains(
            "Npc.sniff is an Exposed method that takes a PYTHON arg (potential security hole)"
        )));
    }
}
