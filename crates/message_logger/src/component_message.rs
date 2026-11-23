//! `LoggerComponentMessage` (`logger_message_forwarder.hpp:124-131`): the
//! registration a component announces itself with. The serialised shape must
//! match `LoggerComponentMessage` in the reference's
//! `tools/server/pycommon/messages.py` as well.

use crate::stream::{MemoryIStream, MemoryOStream};
use crate::{MESSAGE_LOGGER_VERSION, MESSAGE_LOGGER_VERSION_STRING_ID};

/// `LoggerID`: the string filter ID a component registers under. Versions
/// before 7 carried a `uint8` instead.
pub type LoggerId = String;

/// The registration message.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ComponentMessage {
    /// `version_`: the protocol version the component speaks.
    pub version: u8,
    /// `loggerID_`: the filter ID (an empty string matches any logger).
    pub logger_id: LoggerId,
    /// `uid_`: the owning user ID.
    pub uid: u16,
    /// `pid_`: the process ID.
    pub pid: u32,
    /// `componentName_`: e.g. "cellapp", "baseappmgr".
    pub component_name: String,
}

impl ComponentMessage {
    /// `LoggerComponentMessage::write`
    /// (`logger_message_forwarder.cpp:119-122`).
    pub fn write(&self, stream: &mut MemoryOStream) {
        stream.write_u8(self.version);
        stream.write_string(&self.logger_id);
        stream.write_u16(self.uid);
        stream.write_u32(self.pid);
        stream.write_string(&self.component_name);
    }

    /// The envelope a component sends: the message ID then the payload
    /// (`logger_message_forwarder.cpp:540-553`).
    pub fn to_register_envelope(&self) -> Vec<u8> {
        let mut stream = MemoryOStream::new();
        stream.write_u32(crate::message_id::MESSAGE_LOGGER_REGISTER as u32);
        self.write(&mut stream);
        stream.data().to_vec()
    }

    /// `LoggerComponentMessage::read`
    /// (`logger_message_forwarder.cpp:124-146`): versions before 7 carried
    /// the logger ID as a `uint8` rendered as decimal, and the version field
    /// is promoted to the current one on read.
    pub fn read(stream: &mut MemoryIStream) -> Option<Self> {
        let version = stream.read_u8()?;

        let logger_id;
        let version = if version < MESSAGE_LOGGER_VERSION_STRING_ID {
            let logger_id_num = stream.read_u8()?;
            logger_id = format!("{}", logger_id_num);
            MESSAGE_LOGGER_VERSION
        } else {
            logger_id = stream.read_string();
            version
        };

        let uid = stream.read_u16()?;
        let pid = stream.read_u32()?;
        let component_name = stream.read_string();

        if stream.error() {
            return None;
        }

        Some(Self {
            version,
            logger_id,
            uid,
            pid,
            component_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ComponentMessage {
        ComponentMessage {
            version: 10,
            logger_id: "localhost".to_string(),
            uid: 1000,
            pid: 4242,
            component_name: "cellapp".to_string(),
        }
    }

    #[test]
    fn a_registration_round_trips_field_for_field() {
        let message = sample();
        let mut stream = MemoryOStream::new();
        message.write(&mut stream);

        let mut input = MemoryIStream::new(stream.data());
        let back = ComponentMessage::read(&mut input).expect("parses");
        assert_eq!(back, message);
        assert_eq!(input.remaining(), 0);
        assert!(!input.error());
    }

    #[test]
    fn the_wire_lays_out_version_id_uid_pid_name() {
        let mut stream = MemoryOStream::new();
        sample().write(&mut stream);
        // version u8 | packed len "localhost" | payload | uid u16 | pid u32 |
        // packed len "cellapp" | payload.
        assert_eq!(
            stream.data(),
            &[
                10, 9, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't', 0xe8, 0x03, 0x92,
                0x10, 0x00, 0x00, 7, b'c', b'e', b'l', b'l', b'a', b'p', b'p'
            ]
        );
    }

    #[test]
    fn legacy_registration_promotes_the_numeric_logger_id() {
        // A pre-7 component: version 6, then the uint8 logger ID.
        let bytes = [6u8, 3, 0x10, 0x01, 0x39, 0x05, 0x00, 0x00, 2, b'c', b'p'];
        let mut input = MemoryIStream::new(&bytes);
        let message = ComponentMessage::read(&mut input).expect("parses");
        // The ID renders as decimal and the version reads back as current.
        assert_eq!(message.logger_id, "3");
        assert_eq!(message.version, MESSAGE_LOGGER_VERSION);
        assert_eq!(message.uid, 0x0110);
        assert_eq!(message.pid, 0x539);
        assert_eq!(message.component_name, "cp");
    }

    #[test]
    fn the_register_envelope_starts_with_the_message_id() {
        let envelope = sample().to_register_envelope();
        let mut input = MemoryIStream::new(&envelope);
        assert_eq!(
            input.read_u32(),
            Some(crate::message_id::MESSAGE_LOGGER_REGISTER as u32)
        );
        let message = ComponentMessage::read(&mut input).expect("parses");
        assert_eq!(message, sample());
        assert_eq!(input.remaining(), 0);
    }

    #[test]
    fn truncated_registrations_fail_cleanly() {
        let mut stream = MemoryOStream::new();
        sample().write(&mut stream);
        for cut in 0..stream.size() {
            let mut input = MemoryIStream::new(&stream.data()[..cut]);
            assert!(ComponentMessage::read(&mut input).is_none());
        }
    }
}
