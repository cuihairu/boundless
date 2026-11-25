//! BW-12-02 MessageLogger: the log ingestion protocol and component
//! registration (`lib/network/logger_message_forwarder` +
//! `server/tools/message_logger`).
//!
//! The wire shape this crate ports:
//!
//! * every extension message travels as `[i32 messageID][payload]`; the
//!   `(int)MESSAGE_LOGGER_MSG` the forwarder writes in `parseAndSend` is the
//!   transport's message field (`watcher_nub.cpp:430-440` demuxes it and
//!   hands the tool the payload that follows).
//! * `MESSAGE_LOGGER_REGISTER` carries a `LoggerComponentMessage`.
//! * `MESSAGE_LOGGER_MSG` carries a `LoggerMessageHeader`, the format
//!   string, and the format arguments encoded by `ForwardingStringHandler`.
//!
//! The component-facing pieces (`ComponentMessage`, `ForwardingStringHandler`,
//! `build_record`) assemble exactly the bytes the reference emits; the
//! tool-facing pieces (`Registry`, `LogEvent`) accept them with the
//! reference's filters and decode back.

pub mod blob_args;
pub mod bsd_format;
pub mod component_message;
pub mod entry;
pub mod file_logger;
pub mod format_data;
pub mod format_string;
pub mod forwarding_handler;
pub mod interpolator;
pub mod message_header;
pub mod metadata;
pub mod query;
pub mod registry;
pub mod stream;
pub mod user_components;

/// The conditional MongoDB backend (`HAS_MONGODB_SUPPORT`): compiled only
/// with `--features mongodb`, like the reference builds it only after the
/// CMake probe finds the legacy driver.
#[cfg(feature = "mongodb")]
pub mod mongodb;

/// The protocol version this port speaks
/// (`MESSAGE_LOGGER_VERSION`, `logger_message_forwarder.hpp:63`).
pub const MESSAGE_LOGGER_VERSION: u8 = 10;

/// Versions before this carry no metadata (`logger_message_forwarder.hpp:57`).
pub const MESSAGE_LOGGER_VERSION_WITHOUT_METADATA: u8 = 8;

/// Version 9 carries metadata as a raw string (`logger_message_forwarder.hpp:60`).
pub const MESSAGE_LOGGER_VERSION_STRING_METADATA: u8 = 9;

/// Versions before 3 open the extra UDP socket
/// (`MESSAGE_LOGGER_VERSION_WITHOUT_EXTRA_UDP`, `logger_message_forwarder.hpp:57`).
pub const MESSAGE_LOGGER_VERSION_WITHOUT_EXTRA_UDP: u8 = 3;

/// Versions before 7 encode the logger ID as a `uint8`
/// (`MESSAGE_LOGGER_VERSION_STRING_ID`, `logger_message_forwarder.cpp:83`).
const MESSAGE_LOGGER_VERSION_STRING_ID: u8 = 7;

/// The oldest component version the tool accepts
/// (`logger.cpp:142`).
pub const OLDEST_SUPPORTED_MESSAGE_LOGGER_VERSION: u8 = 7;

/// Components at or below this version send the two-element message header
/// without the source enum (`log_storage.cpp:14`).
pub const OLDEST_VERSION_WITHOUT_SOURCE_ENUM: u8 = 7;

/// The extension message IDs (`logger_message_forwarder.hpp:94-99`,
/// `watcher_nub.hpp:74`).
pub mod message_id {
    /// Log records.
    pub const MESSAGE_LOGGER_MSG: i32 = 107;
    /// Component registration.
    pub const MESSAGE_LOGGER_REGISTER: i32 = 108;
    /// A process appeared.
    pub const MESSAGE_LOGGER_PROCESS_BIRTH: i32 = 109;
    /// A process died.
    pub const MESSAGE_LOGGER_PROCESS_DEATH: i32 = 110;
    /// The app instance ID announcement.
    pub const MESSAGE_LOGGER_APP_ID: i32 = 111;
}

/// `DebugMessageSource` (`debug_message_source.hpp:8-12`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MessageSource {
    /// A C++ call site.
    #[default]
    Cpp,
    /// A script call site.
    Script,
}

impl MessageSource {
    /// The wire byte.
    pub fn to_byte(self) -> u8 {
        match self {
            MessageSource::Cpp => 0,
            MessageSource::Script => 1,
        }
    }

    /// From the wire byte; unknown values keep reading as C++ the way the
    /// enum's implicit conversion does.
    pub fn from_byte(value: u8) -> Self {
        if value == 1 {
            MessageSource::Script
        } else {
            MessageSource::Cpp
        }
    }
}

/// `DebugMessagePriority` (`debug_message_priority.hpp:23-35`): the higher
/// the value the higher the priority. The wire carries the raw byte.
pub mod message_priority {
    /// `MESSAGE_PRIORITY_TRACE`.
    pub const TRACE: u8 = 0;
    /// `MESSAGE_PRIORITY_DEBUG`.
    pub const DEBUG: u8 = 1;
    /// `MESSAGE_PRIORITY_INFO`.
    pub const INFO: u8 = 2;
    /// `MESSAGE_PRIORITY_NOTICE`.
    pub const NOTICE: u8 = 3;
    /// `MESSAGE_PRIORITY_WARNING`.
    pub const WARNING: u8 = 4;
    /// `MESSAGE_PRIORITY_ERROR`.
    pub const ERROR: u8 = 5;
    /// `MESSAGE_PRIORITY_CRITICAL`.
    pub const CRITICAL: u8 = 6;
    /// `MESSAGE_PRIORITY_HACK`.
    pub const HACK: u8 = 7;
    /// `DEPRECATED_PRIORITY_SCRIPT` — a script priority the current
    /// protocol no longer emits.
    pub const DEPRECATED_SCRIPT: u8 = 8;
    /// `MESSAGE_PRIORITY_ASSET`.
    pub const ASSET: u8 = 9;
}

/// Whether loggers of this version need the extra UDP socket
/// (`versionShouldOpenUDP`, `logger_message_forwarder.cpp:57-60`).
pub fn version_should_open_udp(major: u16, minor: u16) -> bool {
    major < 2 || (major == 2 && minor < MESSAGE_LOGGER_VERSION_WITHOUT_EXTRA_UDP as u16)
}

/// Whether loggers of this version understand metadata
/// (`versionSupportsMetaData`, `logger_message_forwarder.cpp:74-77`).
pub fn version_supports_meta_data(major: u16, minor: u16) -> bool {
    major > 2 || (major == 2 && minor >= MESSAGE_LOGGER_VERSION_STRING_METADATA as u16)
}

/// Whether loggers of this version understand metadata streamed as
/// arguments (`versionSupportsMetaDataArgs`, `logger_message_forwarder.cpp:90-93`).
pub fn version_supports_meta_data_args(major: u16, minor: u16) -> bool {
    major > 2 || (major == 2 && minor > MESSAGE_LOGGER_VERSION_STRING_METADATA as u16)
}

/// The registration version a component announces to a logger of the given
/// version (`LoggerMessageForwarder::addLogger`,
/// `logger_message_forwarder.cpp:542-552`).
pub fn negotiated_component_version(major: u16, minor: u16) -> u8 {
    if version_supports_meta_data_args(major, minor) {
        MESSAGE_LOGGER_VERSION
    } else if version_supports_meta_data(major, minor) {
        MESSAGE_LOGGER_VERSION_STRING_METADATA
    } else {
        MESSAGE_LOGGER_VERSION_WITHOUT_METADATA
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_capabilities_match_the_reference_cutoffs() {
        // Only < BW2.3 opens the extra UDP socket.
        assert!(version_should_open_udp(1, 9));
        assert!(version_should_open_udp(2, 2));
        assert!(!version_should_open_udp(2, 3));
        assert!(!version_should_open_udp(2, 10));
        assert!(!version_should_open_udp(3, 0));

        // Metadata from BW2.9, streamed-args metadata from BW14.4 (2.10).
        assert!(!version_supports_meta_data(2, 8));
        assert!(version_supports_meta_data(2, 9));
        assert!(!version_supports_meta_data_args(2, 9));
        assert!(version_supports_meta_data_args(2, 10));
        assert!(version_supports_meta_data_args(3, 0));

        // And the negotiation picks the component version to announce.
        assert_eq!(negotiated_component_version(2, 10), 10);
        assert_eq!(negotiated_component_version(2, 9), 9);
        assert_eq!(negotiated_component_version(2, 3), 8);
        assert_eq!(negotiated_component_version(2, 2), 8);
    }

    #[test]
    fn message_sources_round_trip_their_wire_byte() {
        assert_eq!(MessageSource::Cpp.to_byte(), 0);
        assert_eq!(MessageSource::Script.to_byte(), 1);
        assert_eq!(MessageSource::from_byte(0), MessageSource::Cpp);
        assert_eq!(MessageSource::from_byte(1), MessageSource::Script);
        // Out-of-range bytes read back as the default C++ source.
        assert_eq!(MessageSource::from_byte(200), MessageSource::Cpp);
    }

    #[test]
    fn message_ids_start_at_the_watcher_extension() {
        assert_eq!(message_id::MESSAGE_LOGGER_MSG, 107);
        assert_eq!(message_id::MESSAGE_LOGGER_REGISTER, 108);
        assert_eq!(message_id::MESSAGE_LOGGER_PROCESS_BIRTH, 109);
        assert_eq!(message_id::MESSAGE_LOGGER_PROCESS_DEATH, 110);
        assert_eq!(message_id::MESSAGE_LOGGER_APP_ID, 111);
    }
}
