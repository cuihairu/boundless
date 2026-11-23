//! `LoggerMessageHeader` (`logger_message_forwarder.hpp:110-117`): the
//! per-record preamble — component priority, message priority, source and
//! category — plus the legacy two-element variant the tool reads from
//! components at or before version 7 (`log_storage.cpp:83-99`).

use crate::stream::{MemoryIStream, MemoryOStream};
use crate::{MessageSource, message_priority};

/// The record preamble.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MessageHeader {
    /// `componentPriority_`: never assigned by `parseAndSend` — the C++
    /// leaves this indeterminate, the port pins it to zero so records stay
    /// reproducible.
    pub component_priority: u8,
    /// `messagePriority_`: the `DebugMessagePriority` byte.
    pub message_priority: u8,
    /// `messageSource_`: the `DebugMessageSource` byte.
    pub message_source: MessageSource,
    /// `category_`: the message category, empty when unset.
    pub category: String,
}

impl MessageHeader {
    /// `LoggerMessageHeader::write`
    /// (`logger_message_forwarder.cpp:94-99`).
    pub fn write(&self, stream: &mut MemoryOStream) {
        stream.write_u8(self.component_priority);
        stream.write_u8(self.message_priority);
        stream.write_u8(self.message_source.to_byte());
        stream.write_string(&self.category);
    }

    /// `LoggerMessageHeader::read`
    /// (`logger_message_forwarder.cpp:101-106`).
    pub fn read(stream: &mut MemoryIStream) -> Option<Self> {
        let component_priority = stream.read_u8()?;
        let message_priority = stream.read_u8()?;
        let message_source = MessageSource::from_byte(stream.read_u8()?);
        let category = stream.read_string();
        if stream.error() {
            return None;
        }

        Some(Self {
            component_priority,
            message_priority,
            message_source,
            category,
        })
    }

    /// The legacy header a component at or before version 7 sends: only the
    /// component and message priorities travel. The deprecated script
    /// priority re-marks the record as script-sourced info
    /// (`log_storage.cpp:86-95`).
    pub fn read_without_source_enum(stream: &mut MemoryIStream) -> Option<Self> {
        let component_priority = stream.read_u8()?;
        let mut message_priority = stream.read_u8()?;
        let mut message_source = MessageSource::Cpp;
        if message_priority == message_priority::DEPRECATED_SCRIPT {
            message_priority = message_priority::INFO;
            message_source = MessageSource::Script;
        }
        Some(Self {
            component_priority,
            message_priority,
            message_source,
            category: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MessageHeader {
        MessageHeader {
            component_priority: 0,
            message_priority: message_priority::WARNING,
            message_source: MessageSource::Cpp,
            category: "Network".to_string(),
        }
    }

    #[test]
    fn a_header_round_trips_field_for_field() {
        let mut stream = MemoryOStream::new();
        sample().write(&mut stream);
        let mut input = MemoryIStream::new(stream.data());
        assert_eq!(MessageHeader::read(&mut input), Some(sample()));
        assert_eq!(input.remaining(), 0);
    }

    #[test]
    fn the_wire_lays_out_two_bytes_source_and_category() {
        let mut stream = MemoryOStream::new();
        sample().write(&mut stream);
        assert_eq!(
            stream.data(),
            &[0, 4, 0, 7, b'N', b'e', b't', b'w', b'o', b'r', b'k']
        );
    }

    #[test]
    fn truncated_headers_fail_cleanly() {
        let mut stream = MemoryOStream::new();
        sample().write(&mut stream);
        for cut in 0..stream.size() {
            let mut input = MemoryIStream::new(&stream.data()[..cut]);
            assert_eq!(MessageHeader::read(&mut input), None);
        }
    }

    #[test]
    fn the_legacy_header_swaps_the_script_priority() {
        let mut stream = MemoryOStream::new();
        stream.write_u8(0); // component priority
        stream.write_u8(message_priority::DEPRECATED_SCRIPT);
        let mut input = MemoryIStream::new(stream.data());
        let header = MessageHeader::read_without_source_enum(&mut input).expect("parses");
        assert_eq!(header.message_priority, message_priority::INFO);
        assert_eq!(header.message_source, MessageSource::Script);
        assert_eq!(header.category, "");
        assert_eq!(input.remaining(), 0);

        // Any other priority keeps its value and the C++ source.
        let mut stream = MemoryOStream::new();
        stream.write_u8(0);
        stream.write_u8(message_priority::ERROR);
        let mut input = MemoryIStream::new(stream.data());
        let header = MessageHeader::read_without_source_enum(&mut input).expect("parses");
        assert_eq!(header.message_priority, message_priority::ERROR);
        assert_eq!(header.message_source, MessageSource::Cpp);
    }

    #[test]
    fn the_component_priority_reads_back_unchanged() {
        // Even though the sender never assigns it, the field travels and
        // reads back as-is.
        let mut stream = MemoryOStream::new();
        let header = MessageHeader {
            component_priority: 42,
            ..sample()
        };
        header.write(&mut stream);
        let mut input = MemoryIStream::new(stream.data());
        assert_eq!(
            MessageHeader::read(&mut input).unwrap().component_priority,
            42
        );
    }
}
