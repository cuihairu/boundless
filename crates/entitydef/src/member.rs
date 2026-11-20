//! `member_description.hpp`: the tags shared by properties and methods.
//!
//! A member is anything with a name inside an interface definition: a
//! property, a method, a component field. The reference gives them all a
//! `MemberDescription` base that parses `SendLatestOnly`, `IsReliable` and
//! `VariableLengthHeaderSize`, and contributes the member name (plus a
//! non-default variable-length header) to the definition digest.

use resources::datasection::DataSection;

use crate::diagnostics::Diagnostics;
use crate::digest::Md5;

/// `Mercury::DEFAULT_VARIABLE_LENGTH_HEADER_SIZE`
/// (`network/interface_element.hpp`).
pub const DEFAULT_VARIABLE_LENGTH_HEADER_SIZE: u32 = 1;

/// `MemberDescription::OversizeWarnLevel`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OversizeWarnLevel {
    /// `OVERSIZE_NO_WARNING`.
    NoWarning,
    /// `OVERSIZE_SHOULD_LOG`.
    #[default]
    ShouldLog,
    /// `OVERSIZE_SHOULD_PRINT_CALLSTACK`.
    ShouldPrintCallstack,
    /// `OVERSIZE_SHOULD_RAISE_EXCEPTION`.
    ShouldRaiseException,
}

impl OversizeWarnLevel {
    /// `oversizeWarnLevelFromString`: `none | log | callstack | exception`.
    fn from_wire_level(level: &str) -> Option<Self> {
        match level {
            "none" => Some(Self::NoWarning),
            "log" => Some(Self::ShouldLog),
            "callstack" => Some(Self::ShouldPrintCallstack),
            "exception" => Some(Self::ShouldRaiseException),
            _ => None,
        }
    }
}

/// The shared part of a property or method (`MemberDescription`).
///
/// The reference leaves `isForClient_` uninitialised until `parse()` runs;
/// here it defaults to false, which is the safe reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDescription {
    /// The entity or interface this member belongs to.
    pub interface_name: String,
    pub name: String,
    /// `latestEventIndex_`: -1 when `SendLatestOnly` is not set.
    pub latest_event_index: i32,
    pub is_reliable: bool,
    pub is_for_client: bool,
    pub var_len_header_size: u32,
    pub oversize_warn_level: OversizeWarnLevel,
}

impl Default for MemberDescription {
    fn default() -> Self {
        Self {
            interface_name: String::new(),
            name: String::new(),
            latest_event_index: -1,
            is_reliable: true,
            is_for_client: false,
            var_len_header_size: DEFAULT_VARIABLE_LENGTH_HEADER_SIZE,
            oversize_warn_level: OversizeWarnLevel::default(),
        }
    }
}

impl MemberDescription {
    /// `shouldSendLatestOnly()`.
    pub fn should_send_latest_only(&self) -> bool {
        self.latest_event_index != -1
    }

    /// `MemberDescription::parse`.
    ///
    /// `num_latest_event_members` is the caller's counter; a property only
    /// gets one when it is other-client data, a method only when it is a
    /// client method.
    pub fn parse(
        &mut self,
        section: &DataSection,
        interface_name: &str,
        is_for_client: bool,
        num_latest_event_members: Option<&mut u32>,
        diag: &mut Diagnostics,
    ) -> bool {
        self.interface_name = interface_name.to_string();
        self.is_for_client = is_for_client;

        if let Some(counter) = num_latest_event_members {
            if crate::data_type::read_bool(section, "SendLatestOnly", false) {
                self.latest_event_index = *counter as i32;
                *counter += 1;
            }
        }

        // The default is the current value, so an absent tag stays reliable.
        self.is_reliable = crate::data_type::read_bool(section, "IsReliable", self.is_reliable);

        if let Some(var_len_section) = section.find_child("VariableLengthHeaderSize") {
            if !is_for_client {
                diag.error(format!(
                    "MemberDescription::parse: {}: VariableLengthHeaderSize is valid only on client methods and properties\n",
                    self.to_property_string()
                ));
                return false;
            }

            let header_size = section
                .open_section("VariableLengthHeaderSize")
                .map_or(DEFAULT_VARIABLE_LENGTH_HEADER_SIZE, |s| {
                    s.as_uint(DEFAULT_VARIABLE_LENGTH_HEADER_SIZE)
                });
            if header_size == 0 || header_size > 4 {
                diag.error(format!(
                    "MemberDescription::parse: Invalid VariableLengthHeaderSize for {}\n",
                    self.to_property_string()
                ));
                return false;
            }
            self.var_len_header_size = header_size;

            let warn_level = var_len_section.read_string("WarnLevel", "log");
            match OversizeWarnLevel::from_wire_level(&warn_level) {
                Some(level) => self.oversize_warn_level = level,
                None => {
                    diag.error(format!(
                        "MemberDescription::parse: Invalid VariableLengthHeaderSize/WarnLevel for {}\n",
                        self.to_property_string()
                    ));
                    return false;
                }
            }
        }

        true
    }

    /// `MemberDescription::addToMD5`.
    ///
    /// The variable-length header contributes its four bytes only for client
    /// members that are variable-sized and use a non-default header; the
    /// name always contributes as a bare byte run.
    pub fn add_to_md5(&self, md5: &mut Md5, server_to_client_stream_size: i32) {
        if self.is_for_client
            && server_to_client_stream_size < 0
            && self.var_len_header_size != DEFAULT_VARIABLE_LENGTH_HEADER_SIZE
        {
            md5.append_i32(self.var_len_header_size as i32);
        }
        md5.append_text(&self.name);
    }
}

/// The `MemberDescription::toString()` shapes used in messages.
///
/// `DataDescription::toString()` is `"Property <iface>.<name>"`;
/// `MethodDescription::toString()` is `"Method <iface>.<name>()"`.
pub enum MemberKind {
    Property,
    Method,
}

impl MemberDescription {
    /// Format like the reference `toString()` for the member kind given.
    pub fn to_string_for(&self, kind: MemberKind) -> String {
        match kind {
            MemberKind::Property => format!("Property {}.{}", self.interface_name, self.name),
            MemberKind::Method => format!("Method {}.{}()", self.interface_name, self.name),
        }
    }

    fn to_property_string(&self) -> String {
        format!("Property {}.{}", self.interface_name, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_xml(xml: &str) -> DataSection {
        resources::datasection::parse("", xml.as_bytes()).expect("valid fixture xml")
    }

    fn try_parse_member(
        xml: &str,
        interface: &str,
        is_for_client: bool,
        counter: Option<&mut u32>,
    ) -> (MemberDescription, Diagnostics) {
        let mut member = MemberDescription::default();
        let mut diag = Diagnostics::new();
        let section = parse_xml(xml);
        let _ = member.parse(&section, interface, is_for_client, counter, &mut diag);
        (member, diag)
    }

    fn parse_member(
        xml: &str,
        interface: &str,
        is_for_client: bool,
        counter: Option<&mut u32>,
    ) -> (MemberDescription, Diagnostics) {
        let (member, diag) = try_parse_member(xml, interface, is_for_client, counter);
        assert!(!diag.has_errors(), "member parse must succeed for {xml}");
        (member, diag)
    }

    #[test]
    fn defaults_match_the_reference_constructor() {
        let member = MemberDescription::default();
        assert_eq!(member.latest_event_index, -1);
        assert!(member.is_reliable);
        assert!(!member.is_for_client);
        assert_eq!(member.var_len_header_size, 1);
        assert_eq!(member.oversize_warn_level, OversizeWarnLevel::ShouldLog);
        assert!(!member.should_send_latest_only());
    }

    #[test]
    fn send_latest_only_consumes_the_callers_counter() {
        let mut counter = 7u32;
        let (member, _) = parse_member("<hp> INT32 </hp>", "Avatar", true, Some(&mut counter));
        assert!(!member.should_send_latest_only());
        assert_eq!(counter, 7, "no index consumed without the tag");

        let (member, _) = parse_member(
            "<pos> INT32 <SendLatestOnly> true </SendLatestOnly> </pos>",
            "Avatar",
            true,
            Some(&mut counter),
        );
        assert!(member.should_send_latest_only());
        assert_eq!(member.latest_event_index, 7);
        assert_eq!(counter, 8);

        // Without a counter the tag is ignored entirely.
        let (member, _) = parse_member(
            "<hp> INT32 <SendLatestOnly> true </SendLatestOnly> </hp>",
            "Avatar",
            true,
            None,
        );
        assert!(!member.should_send_latest_only());
    }

    #[test]
    fn is_reliable_defaults_to_the_current_value() {
        let (member, _) = parse_member("<m> INT32 </m>", "A", false, None);
        assert!(member.is_reliable);

        let (member, _) = parse_member(
            "<m> INT32 <IsReliable> false </IsReliable> </m>",
            "A",
            false,
            None,
        );
        assert!(!member.is_reliable);
    }

    #[test]
    fn variable_length_header_is_client_only_and_bounded() {
        let (_, diag) = try_parse_member(
            "<m> INT32 <VariableLengthHeaderSize> 2 </VariableLengthHeaderSize> </m>",
            "A",
            false,
            None,
        );
        assert!(diag.has_errors());
        assert!(
            diag.errors()[0].contains(
                "VariableLengthHeaderSize is valid only on client methods and properties"
            )
        );

        let (member, _) = parse_member(
            "<m> INT32 <VariableLengthHeaderSize> 3 </VariableLengthHeaderSize> </m>",
            "A",
            true,
            None,
        );
        assert_eq!(member.var_len_header_size, 3);

        for bad in ["0", "5"] {
            let (_, diag) = try_parse_member(
                &format!(
                    "<m> INT32 <VariableLengthHeaderSize> {bad} </VariableLengthHeaderSize> </m>"
                ),
                "A",
                true,
                None,
            );
            assert!(diag.has_errors(), "header size {bad} must be rejected");
        }
    }

    #[test]
    fn warn_level_accepts_only_the_four_wire_names() {
        for (level, expected) in [
            ("none", OversizeWarnLevel::NoWarning),
            ("log", OversizeWarnLevel::ShouldLog),
            ("callstack", OversizeWarnLevel::ShouldPrintCallstack),
            ("exception", OversizeWarnLevel::ShouldRaiseException),
        ] {
            let (member, diag) = parse_member(
                &format!(
                    "<m> INT32 \
                     <VariableLengthHeaderSize> 2 \
                     <WarnLevel> {level} </WarnLevel> \
                     </VariableLengthHeaderSize> </m>"
                ),
                "A",
                true,
                None,
            );
            assert_eq!(member.oversize_warn_level, expected);
            assert!(!diag.has_errors());
        }

        let (_, diag) = try_parse_member(
            "<m> INT32 <VariableLengthHeaderSize> 2 <WarnLevel> loud </WarnLevel> </VariableLengthHeaderSize> </m>",
            "A",
            true,
            None,
        );
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Invalid VariableLengthHeaderSize/WarnLevel"))
        );
    }

    #[test]
    fn md5_contributes_the_header_only_for_variable_client_members() {
        let mut member = MemberDescription {
            interface_name: "A".into(),
            name: "prop".into(),
            ..MemberDescription::default()
        };

        // Fixed-size client member: name only.
        let mut md5 = Md5::new();
        member.add_to_md5(&mut md5, 4);
        let mut expected = Md5::new();
        expected.append_text("prop");
        assert_eq!(md5.quote(), expected.quote());

        // Variable-size with the default header: still name only.
        member.is_for_client = true;
        let mut md5 = Md5::new();
        member.add_to_md5(&mut md5, -1);
        let mut expected = Md5::new();
        expected.append_text("prop");
        assert_eq!(md5.quote(), expected.quote());

        // Variable-size with a custom header: four header bytes, then name.
        member.var_len_header_size = 3;
        let mut md5 = Md5::new();
        member.add_to_md5(&mut md5, -3);
        let mut expected = Md5::new();
        expected.append_i32(3);
        expected.append_text("prop");
        assert_eq!(md5.quote(), expected.quote());

        // Variable-size server member: never the header.
        member.is_for_client = false;
        let mut md5 = Md5::new();
        member.add_to_md5(&mut md5, -1);
        let mut expected = Md5::new();
        expected.append_text("prop");
        assert_eq!(md5.quote(), expected.quote());
    }

    #[test]
    fn to_string_matches_the_reference_shapes() {
        let member = MemberDescription {
            interface_name: "Avatar".into(),
            name: "hp".into(),
            ..MemberDescription::default()
        };
        assert_eq!(
            member.to_string_for(MemberKind::Property),
            "Property Avatar.hp"
        );
        assert_eq!(
            member.to_string_for(MemberKind::Method),
            "Method Avatar.hp()"
        );
    }
}
