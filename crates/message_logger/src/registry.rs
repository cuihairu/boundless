//! The tool side: component registration and record acceptance, mirroring
//! `Logger::handleRegisterRequest` / `shouldConnect` / `handleLogMessage`
//! (`server/tools/message_logger/logger.cpp:1520-1613`) and
//! `LogStorage::addLogMessage`'s header handling
//! (`log_storage.cpp:53-99`). The transport is abstract: callers hand in the
//! payload bytes that follow the message ID and the source address.

use std::collections::BTreeMap;

use crate::component_message::ComponentMessage;
use crate::forwarding_handler::{ArgValue, ForwardingStringHandler};
use crate::message_header::MessageHeader;
use crate::stream::{MemoryIStream, MemoryOStream};
use crate::{
    MESSAGE_LOGGER_VERSION, OLDEST_SUPPORTED_MESSAGE_LOGGER_VERSION,
    OLDEST_VERSION_WITHOUT_SOURCE_ENUM,
};

/// A Mercury-style source address, the registration key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Address {
    /// The IP as host-order bytes.
    pub ip: u32,
    /// The port.
    pub port: u16,
}

impl Address {
    /// Build from parts.
    pub fn new(ip: u32, port: u16) -> Self {
        Self { ip, port }
    }

    /// The reference renders addresses as `ip:port`, most significant
    /// octet first (`Mercury::Address::operator<<`).
    pub fn as_string(&self) -> String {
        format!(
            "{}.{}.{}.{}:{}",
            (self.ip >> 24) & 0xff,
            (self.ip >> 16) & 0xff,
            (self.ip >> 8) & 0xff,
            self.ip & 0xff,
            self.port
        )
    }
}

/// The filters a MessageLogger applies to a registration
/// (`Logger::shouldConnect`, `logger.cpp:1532-1608`).
#[derive(Clone, Default, Debug)]
pub struct RegistryConfig {
    /// `logAllUsers_`: accept any uid; otherwise `log_uid` must match.
    pub log_all_users: bool,
    /// `logUser_`: the single uid accepted when `log_all_users` is false.
    pub log_uid: u16,
    /// `loggerID_`: when non-empty, the component's logger ID must match.
    pub logger_id: String,
    /// `logNames_`: when non-empty, an allow-list of component names.
    pub log_names: Vec<String>,
    /// `doNotLogNames_`: component names never accepted.
    pub do_not_log_names: Vec<String>,
}

/// A registered logging component.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Component {
    /// The registration message the component announced itself with.
    pub message: ComponentMessage,
    /// The source address.
    pub address: Address,
}

/// Why a registration was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RegistrationRejection {
    /// The payload did not parse, or did not fill the stream exactly
    /// (`handleRegisterRequest`, `logger.cpp:1543-1549`).
    Malformed,
    /// `component.version_ < OLDEST_SUPPORTED_MESSAGE_LOGGER_VERSION`.
    VersionTooOld(u8),
    /// `component.version_ > MESSAGE_LOGGER_VERSION`.
    VersionTooNew(u8),
    /// The uid filter refused it.
    UidMismatch,
    /// The logger ID filter refused it.
    LoggerIdMismatch,
    /// The allow-list refused it.
    NameNotAllowed,
    /// The deny-list refused it.
    NameDenied,
}

/// A decoded log record.
#[derive(Clone, PartialEq, Debug)]
pub struct LogEvent {
    /// The record header (legacy components get the script→info swap).
    pub header: MessageHeader,
    /// The format string.
    pub format: String,
    /// The decoded format arguments.
    pub args: Vec<ArgValue>,
    /// The number of trailing bytes the decoder did not consume — metadata
    /// rides there (`log_storage` streams unparsed trailing bytes as
    /// metadata).
    pub trailing: Vec<u8>,
}

/// What `handle_log_message` made of a record.
#[derive(Clone, PartialEq, Debug)]
pub enum RecordOutcome {
    /// The record decoded into an event.
    Accepted(LogEvent),
    /// The source never registered — the reference sends a del response
    /// instead (`handleLogMessage`, `logger.cpp:1492-1504`).
    NotRegistered,
    /// The record failed to decode.
    Malformed,
}

/// The component table keyed by source address.
#[derive(Default, Debug)]
pub struct Registry {
    config: RegistryConfig,
    components: BTreeMap<Address, Component>,
}

impl Registry {
    /// A registry with the given filters.
    pub fn new(config: RegistryConfig) -> Self {
        Self {
            config,
            components: BTreeMap::new(),
        }
    }

    /// The active filters.
    pub fn config(&self) -> &RegistryConfig {
        &self.config
    }

    /// The registered components, keyed by address.
    pub fn components(&self) -> &BTreeMap<Address, Component> {
        &self.components
    }

    /// `Logger::shouldConnect` (`logger.cpp:1532-1608`): the version bounds,
    /// the uid, logger ID and name filters, in the reference's order.
    pub fn should_connect(&self, component: &ComponentMessage) -> Option<RegistrationRejection> {
        if component.version < OLDEST_SUPPORTED_MESSAGE_LOGGER_VERSION {
            return Some(RegistrationRejection::VersionTooOld(component.version));
        }

        if component.version > MESSAGE_LOGGER_VERSION {
            return Some(RegistrationRejection::VersionTooNew(component.version));
        }

        if !self.config.log_all_users && self.config.log_uid != component.uid {
            return Some(RegistrationRejection::UidMismatch);
        }

        if component.component_name != "Tools"
            && !self.config.logger_id.is_empty()
            && self.config.logger_id != component.logger_id
        {
            return Some(RegistrationRejection::LoggerIdMismatch);
        }

        if !self.config.log_names.is_empty()
            && !self.config.log_names.contains(&component.component_name)
        {
            return Some(RegistrationRejection::NameNotAllowed);
        }

        if self
            .config
            .do_not_log_names
            .contains(&component.component_name)
        {
            return Some(RegistrationRejection::NameDenied);
        }

        None
    }

    /// `Logger::handleRegisterRequest` (`logger.cpp:1520-1530`): parse the
    /// registration payload, require it to consume the stream exactly, and
    /// register on the address when the filters accept it.
    pub fn handle_register_request(
        &mut self,
        address: Address,
        payload: &[u8],
    ) -> Result<Component, RegistrationRejection> {
        let mut stream = MemoryIStream::new(payload);
        let Some(message) = ComponentMessage::read(&mut stream) else {
            return Err(RegistrationRejection::Malformed);
        };

        if stream.error() || stream.remaining() > 0 {
            return Err(RegistrationRejection::Malformed);
        }

        if let Some(rejection) = self.should_connect(&message) {
            return Err(rejection);
        }

        let component = Component { message, address };
        self.components.insert(address, component.clone());
        Ok(component)
    }

    /// `Logger::handleLogMessage` + `LogStorage::addLogMessage`
    /// (`logger.cpp:1476-1504`, `log_storage.cpp:53-99`): the source must be
    /// registered; the header reads in the component's own shape (the
    /// legacy two-element header at or before version 7); then the format
    /// string and its arguments decode, with any trailing bytes kept for
    /// the metadata stage.
    pub fn handle_log_message(&self, address: Address, payload: &[u8]) -> RecordOutcome {
        let Some(component) = self.components.get(&address) else {
            return RecordOutcome::NotRegistered;
        };

        let mut stream = MemoryIStream::new(payload);

        let header = if component.message.version <= OLDEST_VERSION_WITHOUT_SOURCE_ENUM {
            MessageHeader::read_without_source_enum(&mut stream)
        } else {
            MessageHeader::read(&mut stream)
        };
        let Some(header) = header else {
            return RecordOutcome::Malformed;
        };
        // The reference drops records below the tool's filter threshold
        // here (`shouldLogPriority`); the port accepts them all, as a
        // threshold-free configuration does.

        let format = stream.read_string();
        if stream.error() {
            return RecordOutcome::Malformed;
        }

        let handler = ForwardingStringHandler::new(&format, false);
        if !handler.is_ok() {
            return RecordOutcome::Malformed;
        }

        let Some(args) = handler.decode_args(&mut stream) else {
            return RecordOutcome::Malformed;
        };

        let trailing = {
            let mut tail = MemoryOStream::new();
            tail.add_blob(&payload[payload.len() - stream.remaining()..]);
            tail.data().to_vec()
        };

        RecordOutcome::Accepted(LogEvent {
            header,
            format,
            args,
            trailing,
        })
    }

    /// The full ingestion of one UDP datagram: the message ID, then the
    /// payload. Register (108) and log record (107) are the messages this
    /// crate consumes; birth (109), death (110) and app ID (111) travel
    /// through untouched.
    pub fn handle_datagram(&mut self, address: Address, datagram: &[u8]) -> Option<RecordOutcome> {
        let mut stream = MemoryIStream::new(datagram);
        let message_id = stream.read_u32()? as i32;
        let payload = &datagram[datagram.len() - stream.remaining()..];

        match message_id {
            crate::message_id::MESSAGE_LOGGER_REGISTER => {
                // A refused registration just leaves the table untouched;
                // the reference sends a del response on the wire.
                let _ = self.handle_register_request(address, payload);
                None
            }
            crate::message_id::MESSAGE_LOGGER_MSG => {
                Some(self.handle_log_message(address, payload))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarding_handler::build_record;
    use crate::message_header::MessageHeader;
    use crate::{MessageSource, message_id, message_priority, negotiated_component_version};

    fn v10_component(uid: u16) -> ComponentMessage {
        ComponentMessage {
            version: 10,
            logger_id: "localhost".to_string(),
            uid,
            pid: 4242,
            component_name: "cellapp".to_string(),
        }
    }

    fn info_header() -> MessageHeader {
        MessageHeader {
            message_priority: message_priority::INFO,
            message_source: MessageSource::Cpp,
            ..Default::default()
        }
    }

    fn addr(port: u16) -> Address {
        Address::new(0x7f00_0001, port) // 127.0.0.1
    }

    /// Destructure an accepted outcome, panicking with the actual value
    /// otherwise.
    fn expect_accepted(outcome: RecordOutcome) -> LogEvent {
        if let RecordOutcome::Accepted(event) = outcome {
            event
        } else {
            panic!("expected acceptance, got {outcome:?}")
        }
    }

    fn open_registry() -> Registry {
        Registry::new(RegistryConfig {
            log_all_users: true,
            ..Default::default()
        })
    }

    fn register_cellapp(registry: &mut Registry, port: u16) -> Component {
        registry
            .handle_register_request(addr(port), &v10_component(1000).to_register_envelope()[4..])
            .expect("registers")
    }

    fn submit(
        registry: &mut Registry,
        port: u16,
        header: &MessageHeader,
        format: &str,
        args: &[ArgValue],
    ) -> RecordOutcome {
        let handler = ForwardingStringHandler::new(format, false);
        let record = build_record(header, &handler, args);
        registry.handle_log_message(addr(port), &record[4..])
    }

    #[test]
    fn addresses_render_like_the_reference() {
        assert_eq!(addr(32000).as_string(), "127.0.0.1:32000");
    }

    #[test]
    fn the_registry_reports_its_filters() {
        let registry = open_registry();
        assert!(registry.config().log_all_users);
        assert_eq!(registry.config().log_uid, 0);
        assert!(registry.config().logger_id.is_empty());
    }

    #[test]
    fn an_original_registration_fills_the_table() {
        let mut registry = open_registry();
        let component = register_cellapp(&mut registry, 31001);
        assert_eq!(component.message.component_name, "cellapp");
        assert_eq!(component.message.uid, 1000);
        assert_eq!(component.address, addr(31001));
        assert_eq!(registry.components().len(), 1);
    }

    #[test]
    fn a_fixed_event_submitted_by_a_registered_component_decodes() {
        let mut registry = open_registry();
        register_cellapp(&mut registry, 31002);

        let outcome = submit(
            &mut registry,
            31002,
            &info_header(),
            "addBot: id=%d",
            &[ArgValue::Int(11)],
        );
        let LogEvent {
            header,
            format,
            args,
            ..
        } = match outcome {
            RecordOutcome::Accepted(event) => event,
            other => panic!("expected acceptance, got {other:?}"),
        };
        assert_eq!(header, info_header());
        assert_eq!(format, "addBot: id=%d");
        assert_eq!(args, vec![ArgValue::Int(11)]);
    }

    #[test]
    fn the_full_datagram_path_routes_register_and_records() {
        let mut registry = open_registry();
        let address = addr(31003);

        // The register datagram, envelope and all.
        assert_eq!(
            registry.handle_datagram(address, &v10_component(1000).to_register_envelope()),
            None
        );
        assert_eq!(registry.components().len(), 1);

        // Then a fixed event datagram.
        let handler = ForwardingStringHandler::new("appID %d", false);
        let record = build_record(&info_header(), &handler, &[ArgValue::Int(7)]);
        let event = expect_accepted(
            registry
                .handle_datagram(address, &record)
                .expect("accepted"),
        );
        assert_eq!(event.format, "appID %d");
        assert_eq!(event.args, vec![ArgValue::Int(7)]);

        // Unrelated message IDs pass through untouched.
        let mut datagram = Vec::new();
        datagram.extend_from_slice(&(message_id::MESSAGE_LOGGER_APP_ID as u32).to_le_bytes());
        datagram.extend_from_slice(&5i32.to_le_bytes());
        assert_eq!(registry.handle_datagram(address, &datagram), None);
    }

    #[test]
    fn an_unregistered_source_gets_no_entry_and_no_event() {
        let registry = open_registry();
        let handler = ForwardingStringHandler::new("%d", false);
        let record = build_record(&info_header(), &handler, &[ArgValue::Int(1)]);
        assert_eq!(
            registry.handle_log_message(addr(31999), &record[4..]),
            RecordOutcome::NotRegistered
        );
    }

    #[test]
    fn malformed_registrations_are_refused() {
        let mut registry = open_registry();
        let full = v10_component(1000).to_register_envelope()[4..].to_vec();

        // A cut payload fails to fill the stream.
        for cut in 0..full.len() {
            assert_eq!(
                registry.handle_register_request(addr(1), &full[..cut]),
                Err(RegistrationRejection::Malformed)
            );
        }

        // Trailing bytes beyond the message are refused too
        // (`remainingLength() > 0`).
        let mut padded = full.clone();
        padded.push(0);
        assert_eq!(
            registry.handle_register_request(addr(1), &padded),
            Err(RegistrationRejection::Malformed)
        );
    }

    #[test]
    fn version_bounds_reject_out_of_range_components() {
        let mut registry = open_registry();

        // A pre-7 component does not hit the too-old arm on the wire: the
        // read promotes its numeric logger ID and its version to the
        // current protocol, so the legacy shape registers cleanly.
        let legacy = [
            6u8, 5, // version, uint8 logger ID
            0xe8, 0x03, // uid 1000
            0x92, 0x10, 0x00, 0x00, // pid 4242
            7, b'c', b'e', b'l', b'l', b'a', b'p', b'p',
        ];
        let component = registry
            .handle_register_request(addr(1), &legacy)
            .expect("registers");
        assert_eq!(component.message.version, MESSAGE_LOGGER_VERSION);
        assert_eq!(component.message.logger_id, "5");

        // The too-old and too-new arms apply to the string-protocol
        // versions 7..10, checked against the filter directly.
        let mut oldest = v10_component(1000);
        oldest.version = OLDEST_SUPPORTED_MESSAGE_LOGGER_VERSION - 1;
        assert_eq!(
            registry.should_connect(&oldest),
            Some(RegistrationRejection::VersionTooOld(6))
        );

        let mut future = v10_component(1000);
        future.version = 11;
        assert_eq!(
            registry.should_connect(&future),
            Some(RegistrationRejection::VersionTooNew(11))
        );

        // Exactly the bounds pass.
        let mut oldest = v10_component(1000);
        oldest.version = OLDEST_SUPPORTED_MESSAGE_LOGGER_VERSION;
        assert_eq!(registry.should_connect(&oldest), None);
        let mut newest = v10_component(1000);
        newest.version = MESSAGE_LOGGER_VERSION;
        assert_eq!(registry.should_connect(&newest), None);
        assert!(
            registry
                .handle_register_request(addr(1), &newest.to_register_envelope()[4..])
                .is_ok()
        );
    }

    #[test]
    fn the_filters_refuse_mismatched_uids_ids_and_names() {
        // A uid-scoped logger.
        let mut registry = Registry::new(RegistryConfig {
            log_all_users: false,
            log_uid: 2000,
            ..Default::default()
        });
        assert_eq!(
            registry
                .handle_register_request(addr(1), &v10_component(1000).to_register_envelope()[4..]),
            Err(RegistrationRejection::UidMismatch)
        );
        assert!(
            registry
                .handle_register_request(addr(1), &v10_component(2000).to_register_envelope()[4..])
                .is_ok()
        );

        // A logger-ID-scoped logger.
        let mut registry = Registry::new(RegistryConfig {
            log_all_users: true,
            logger_id: "cluster-a".to_string(),
            ..Default::default()
        });
        assert_eq!(
            registry
                .handle_register_request(addr(1), &v10_component(1).to_register_envelope()[4..]),
            Err(RegistrationRejection::LoggerIdMismatch)
        );
        let mut scoped = v10_component(1);
        scoped.logger_id = "cluster-a".to_string();
        assert!(
            registry
                .handle_register_request(addr(1), &scoped.to_register_envelope()[4..])
                .is_ok()
        );

        // Name allow/deny lists: the allow-list check runs first, so a
        // name outside it is refused for that reason even when denied too.
        let mut registry = Registry::new(RegistryConfig {
            log_all_users: true,
            log_names: vec!["baseapp".to_string()],
            do_not_log_names: vec!["client".to_string()],
            ..Default::default()
        });
        assert_eq!(
            registry
                .handle_register_request(addr(1), &v10_component(1).to_register_envelope()[4..]),
            Err(RegistrationRejection::NameNotAllowed)
        );
        // Inside the allow-list but also denied: the deny-list fires.
        let mut registry = Registry::new(RegistryConfig {
            log_all_users: true,
            log_names: vec!["client".to_string()],
            do_not_log_names: vec!["client".to_string()],
            ..Default::default()
        });
        let mut client = v10_component(1);
        client.component_name = "client".to_string();
        assert_eq!(
            registry.handle_register_request(addr(1), &client.to_register_envelope()[4..]),
            Err(RegistrationRejection::NameDenied)
        );
    }

    #[test]
    fn a_v7_component_takes_the_legacy_header_path() {
        let mut registry = open_registry();
        let mut v7 = v10_component(1000);
        v7.version = 7;
        registry
            .handle_register_request(addr(4), &v7.to_register_envelope()[4..])
            .expect("registers");

        // The record carries only the two priority bytes; the deprecated
        // script priority swaps to a script-sourced info record.
        let mut payload = Vec::new();
        payload.push(0); // component priority
        payload.push(message_priority::DEPRECATED_SCRIPT);
        payload.push(2);
        payload.extend_from_slice(b"%d");
        payload.extend_from_slice(&9i32.to_le_bytes());
        let outcome = registry.handle_log_message(addr(4), &payload);
        let event = expect_accepted(outcome);
        assert_eq!(event.header.message_priority, message_priority::INFO);
        assert_eq!(event.header.message_source, MessageSource::Script);
        assert_eq!(event.format, "%d");
        assert_eq!(event.args, vec![ArgValue::Int(9)]);
    }

    #[test]
    fn trailing_bytes_are_kept_for_the_metadata_stage() {
        let mut registry = open_registry();
        register_cellapp(&mut registry, 31005);

        // A v14.4 logger receives metadata as unparsed trailing bytes.
        let handler = ForwardingStringHandler::new("%d", false);
        let mut record = build_record(&info_header(), &handler, &[ArgValue::Int(3)]);
        let mut metadata = Vec::new();
        metadata.push(1); // MLVT_LONG_LONG_INT key type
        metadata.push(4);
        metadata.extend_from_slice(b"cid");
        metadata.push(1);
        metadata.extend_from_slice(&77i64.to_le_bytes());
        record.extend_from_slice(&metadata);

        let event = expect_accepted(registry.handle_log_message(addr(31005), &record[4..]));
        assert_eq!(event.args, vec![ArgValue::Int(3)]);
        assert_eq!(event.trailing, metadata);
    }

    #[test]
    fn malformed_records_are_refused() {
        let mut registry = open_registry();
        register_cellapp(&mut registry, 31006);

        // Too short to hold a header.
        assert_eq!(
            registry.handle_log_message(addr(31006), &[1]),
            RecordOutcome::Malformed
        );

        // A header but no format string.
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, message_priority::INFO, 0, 0]);
        assert_eq!(
            registry.handle_log_message(addr(31006), &payload),
            RecordOutcome::Malformed
        );

        // An illegal conversion in the format string.
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, message_priority::INFO, 0, 0]);
        payload.push(2);
        payload.extend_from_slice(b"%y");
        assert_eq!(
            registry.handle_log_message(addr(31006), &payload),
            RecordOutcome::Malformed
        );

        // Arguments shorter than the format demands.
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, message_priority::INFO, 0, 0]);
        payload.push(2);
        payload.extend_from_slice(b"%d");
        payload.push(1);
        assert_eq!(
            registry.handle_log_message(addr(31006), &payload),
            RecordOutcome::Malformed
        );
    }

    #[test]
    fn the_negotiated_version_matches_the_reference_matrix() {
        // The registered component announces the version the negotiation
        // picks for a v14.4 logger.
        let negotiated = negotiated_component_version(2, 10);
        let mut registry = open_registry();
        let mut component = v10_component(1000);
        component.version = negotiated;
        assert!(
            registry
                .handle_register_request(addr(1), &component.to_register_envelope()[4..])
                .is_ok()
        );
    }

    #[test]
    fn re_registration_replaces_the_component_at_the_address() {
        let mut registry = open_registry();
        register_cellapp(&mut registry, 31007);

        let mut renamed = v10_component(1000);
        renamed.component_name = "baseapp".to_string();
        registry
            .handle_register_request(addr(31007), &renamed.to_register_envelope()[4..])
            .expect("replaces");
        assert_eq!(registry.components().len(), 1);
        assert_eq!(
            registry.components()[&addr(31007)].message.component_name,
            "baseapp"
        );
    }
}
