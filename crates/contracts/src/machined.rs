//! Machined process-discovery contracts (BW-16-01-02 / BW-05).
//!
//! The reference implementation is `lib/network/machine_guard.hpp`: every
//! message shares the three-field header and is carried between roles and
//! `bwmachined`, so the structures live here where any caller can reach them
//! without depending on a peer service crate.
//!
//! Wire layout (little endian, strings are packed-int length plus bytes):
//!
//! ```text
//! header    u8 message, u8 flags, u16 seq
//! process   u8 param, u8 category, u16 uid, u16 pid, u16 port, u16 id,
//!           string name
//! extra     i32 declared tail size, u16 major, u16 minor, u16 patch,
//!           u16 interface, string username, string def digest, tail bytes
//! listener  process, string pre addr, string post addr
//! signal    process, u8 signal
//! ```

use foundation::stream::{BinaryStream, StreamError};

/// Version of the machined Mercury interface, mirrored from the reference.
pub const MERCURY_INTERFACE_VERSION: u16 = 1;

/// `MachineGuardMessage::Message` ids, tool/server to machined.
pub mod message_id {
    pub const WHOLE_MACHINE: u8 = 1;
    pub const PROCESS: u8 = 2;
    pub const PROCESS_STATS: u8 = 3;
    pub const LISTENER: u8 = 4;
    pub const CREATE: u8 = 5;
    pub const SIGNAL: u8 = 6;
    pub const TAGS: u8 = 7;
    pub const USER: u8 = 8;
    pub const PID: u8 = 9;
    pub const RESET: u8 = 10;
    pub const ERROR: u8 = 11;
    pub const QUERY_INTERFACE: u8 = 12;
    pub const CREATE_WITH_ARGS: u8 = 13;
    pub const HIGH_PRECISION_MACHINE: u8 = 14;
    pub const MACHINE_PLATFORM: u8 = 15;

    /// Machined to machined announcements.
    pub const MACHINED_ANNOUNCE: u8 = 64;
}

/// `MachineGuardMessage::Flags`.
pub mod flags {
    pub const MESSAGE_DIRECTION_OUTGOING: u8 = 0x1;
    pub const MESSAGE_NOT_UNDERSTOOD: u8 = 0x2;
}

/// `ProcessMessage::Type`.
pub mod process_type {
    pub const FIND: u8 = 0;
    pub const REGISTER: u8 = 1;
    pub const DEREGISTER: u8 = 2;
    pub const NOTIFY_BIRTH: u8 = 3;
    pub const NOTIFY_DEATH: u8 = 4;
}

/// `ProcessMessage::Param` bitmap.
pub mod process_param {
    pub const USE_CATEGORY: u8 = 0x1;
    pub const USE_UID: u8 = 0x2;
    pub const USE_PID: u8 = 0x4;
    pub const USE_PORT: u8 = 0x8;
    pub const USE_ID: u8 = 0x10;
    pub const USE_NAME: u8 = 0x20;
    pub const IS_MSGTYPE: u8 = 0x80;
}

/// `ProcessMessage::Category`.
pub mod process_category {
    pub const SERVER_COMPONENT: u8 = 0;
    pub const WATCHER_NUB: u8 = 1;
}

/// Reference name of a category, `"UNKNOWN"` outside the enum.
pub fn category_name(category: u8) -> &'static str {
    match category {
        process_category::SERVER_COMPONENT => "SERVER_COMPONENT",
        process_category::WATCHER_NUB => "WATCHER_NUB",
        _ => "UNKNOWN",
    }
}

/// The shared message header: message id, flags and sequence number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub message: u8,
    pub flags: u8,
    pub seq: u16,
}

impl Header {
    fn write(&self, stream: &mut BinaryStream) {
        stream.write_u8(self.message);
        stream.write_u8(self.flags);
        stream.write_u16(self.seq);
    }

    fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        Ok(Self {
            message: stream.read_u8()?,
            flags: stream.read_u8()?,
            seq: stream.read_u16()?,
        })
    }
}

/// A running process registered with machined: name, pid, port and version
/// info, used by register, birth/death notification and queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMessage {
    pub header: Header,
    /// Bitmap for filtering, `process_param` bits.
    pub param: u8,
    pub category: u8,
    pub uid: u16,
    pub pid: u16,
    pub port: u16,
    /// BigWorld app id, as in `cellapp01`.
    pub id: u16,
    /// The nub's interface, e.g. `CellAppInterface`.
    pub name: String,
    pub major_version: u16,
    pub minor_version: u16,
    pub patch_version: u16,
    pub interface_version: u16,
    pub username: String,
    pub def_digest: String,
}

impl Default for ProcessMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::PROCESS,
                flags: 0,
                seq: 0,
            },
            param: 0,
            category: process_category::SERVER_COMPONENT,
            uid: 0,
            pid: 0,
            port: 0,
            id: 0,
            name: String::new(),
            major_version: 0,
            minor_version: 0,
            patch_version: 0,
            interface_version: MERCURY_INTERFACE_VERSION,
            username: String::new(),
            def_digest: String::new(),
        }
    }
}

impl ProcessMessage {
    /// Write the message body plus the versioned extra block.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_u8(self.param);
        stream.write_u8(self.category);
        stream.write_u16(self.uid);
        stream.write_u16(self.pid);
        stream.write_u16(self.port);
        stream.write_u16(self.id);
        stream.write_string(&self.name)?;

        // Extra block: the declared tail size covers everything after the
        // size field, so unknown future fields can be skipped on read.
        let tail = 2 * 4 + packed_len(&self.username) + packed_len(&self.def_digest);
        stream.write_i32(tail);
        stream.write_u16(self.major_version);
        stream.write_u16(self.minor_version);
        stream.write_u16(self.patch_version);
        stream.write_u16(self.interface_version);
        stream.write_string(&self.username)?;
        stream.write_string(&self.def_digest)?;
        Ok(())
    }

    /// Read a message, tolerating senders without the extra block.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let header = Header::read(stream)?;
        let param = stream.read_u8()?;
        let category = stream.read_u8()?;
        let uid = stream.read_u16()?;
        let pid = stream.read_u16()?;
        let port = stream.read_u16()?;
        let id = stream.read_u16()?;
        let name = stream.read_string()?;

        let mut message = Self {
            header,
            param,
            category,
            uid,
            pid,
            port,
            id,
            name,
            major_version: 0,
            minor_version: 0,
            patch_version: 0,
            interface_version: 0,
            username: String::new(),
            def_digest: String::new(),
        };

        if !stream.at_end() {
            let declared_tail = stream.read_i32()?;
            let before = stream.remaining();
            message.major_version = stream.read_u16()?;
            message.minor_version = stream.read_u16()?;
            message.patch_version = stream.read_u16()?;
            message.interface_version = stream.read_u16()?;
            message.username = stream.read_string()?;
            message.def_digest = stream.read_string()?;
            let consumed = before - stream.remaining();
            let unread = declared_tail - consumed as i32;
            if unread < 0 {
                return Err(StreamError::Truncated);
            }
            // Future fields: skip what this version does not know.
            stream.read_blob_raw(unread as usize)?;
        }
        Ok(message)
    }

    /// The reference `matches`: every field requested in `query.param` must
    /// be equal; unrequested fields are ignored.
    pub fn matches(&self, query: &Self) -> bool {
        if query.param & process_param::USE_UID != 0 && query.uid != self.uid {
            return false;
        }
        if query.param & process_param::USE_PID != 0 && query.pid != self.pid {
            return false;
        }
        if query.param & process_param::USE_ID != 0 && query.id != self.id {
            return false;
        }
        if query.param & process_param::USE_NAME != 0 && query.name != self.name {
            return false;
        }
        if query.param & process_param::USE_CATEGORY != 0 && query.category != self.category {
            return false;
        }
        if query.param & process_param::USE_PORT != 0 && query.port != self.port {
            return false;
        }
        true
    }

    /// The reference `c_str`: `"<name> (<category>) (pid:<pid>)"`.
    pub fn describe(&self) -> String {
        format!(
            "{} ({}) (pid:{})",
            self.name,
            category_name(self.category),
            self.pid
        )
    }
}

/// Bytes a string occupies on the wire: packed length prefix plus payload.
fn packed_len(text: &str) -> i32 {
    (foundation::stream::packed_int_size(text.len() as u32) + text.len()) as i32
}

/// A birth or death listener registration, `ListenerMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerMessage {
    pub process: ProcessMessage,
    pub pre_addr: String,
    pub post_addr: String,
}

/// `ListenerMessage::Type`.
pub mod listener_type {
    pub const ADD_BIRTH_LISTENER: u8 = 0;
    pub const ADD_DEATH_LISTENER: u8 = 1;

    /// Register for births or deaths of any user.
    pub const ANY_UID: u16 = 0xffff;
}

impl ListenerMessage {
    /// Write the process body followed by both addresses.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.process.write(stream)?;
        stream.write_string(&self.pre_addr)?;
        stream.write_string(&self.post_addr)?;
        Ok(())
    }

    /// Read a listener message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let process = ProcessMessage::read(stream)?;
        Ok(Self {
            process,
            pre_addr: stream.read_string()?,
            post_addr: stream.read_string()?,
        })
    }
}

/// A signal command for one process, `SignalMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalMessage {
    pub process: ProcessMessage,
    pub signal: u8,
}

/// Signal numbers the reference builds in via `setControlledShutdown` and
/// friends, mirroring the `signal.h` values the reference falls back to.
pub mod signal {
    pub const SIGINT: u8 = 2;
    pub const SIGQUIT: u8 = 3;
    pub const SIGUSR1: u8 = 10;
}

impl SignalMessage {
    /// Write the process body followed by the signal number.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.process.write(stream)?;
        stream.write_u8(self.signal);
        Ok(())
    }

    /// Read a signal message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let process = ProcessMessage::read(stream)?;
        Ok(Self {
            process,
            signal: stream.read_u8()?,
        })
    }

    /// A controlled shutdown, `SIGUSR1` in the reference.
    pub fn set_controlled_shutdown(&mut self) {
        self.signal = signal::SIGUSR1;
    }

    /// A kill, `SIGINT` in the reference.
    pub fn set_kill(&mut self) {
        self.signal = signal::SIGINT;
    }

    /// A hard kill, `SIGQUIT` in the reference.
    pub fn set_hard_kill(&mut self) {
        self.signal = signal::SIGQUIT;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `cellapp01` process message, hand-encoded field by field to pin the
    /// wire layout to the reference implementation.
    #[test]
    fn process_message_matches_reference_bytes() {
        let mut message = ProcessMessage {
            pid: 0x1092,
            id: 1,
            name: "cellapp01".to_string(),
            ..ProcessMessage::default()
        };
        message.header.seq = 7;

        let mut stream = BinaryStream::new();
        message.write(&mut stream).expect("write");

        let expected: Vec<u8> = [
            // header: message, flags, seq
            message_id::PROCESS,
            0,
            7,
            0,
            // process: param, category, uid, pid, port, id
            0,
            0,
            0,
            0,
            0x92,
            0x10,
            0,
            0,
            1,
            0,
            // name: packed length 9 plus payload
            9,
            b'c',
            b'e',
            b'l',
            b'l',
            b'a',
            b'p',
            b'p',
            b'0',
            b'1',
            // extra: declared tail 10, four u16 versions, two empty strings
            10,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
            0,
            0,
            0,
        ]
        .to_vec();
        assert_eq!(stream.as_bytes(), expected);
    }

    #[test]
    fn process_message_round_trips() {
        let message = ProcessMessage {
            uid: 1000,
            pid: 4242,
            port: 3456,
            id: 2,
            name: "baseapp01".to_string(),
            major_version: 1,
            minor_version: 2,
            patch_version: 3,
            interface_version: MERCURY_INTERFACE_VERSION,
            username: "cui".to_string(),
            def_digest: "d41d8cd9".to_string(),
            ..ProcessMessage::default()
        };

        let mut stream = BinaryStream::new();
        message.write(&mut stream).expect("write");
        let read = ProcessMessage::read(&mut stream).expect("read");
        assert_eq!(read, message);
        assert!(stream.at_end());
    }

    #[test]
    fn process_message_without_extra_block_is_accepted() {
        // Old senders stop after the name field.
        let mut stream = BinaryStream::new();
        stream.write_u8(message_id::PROCESS);
        stream.write_u8(0);
        stream.write_u16(0);
        stream.write_u8(process_param::USE_NAME);
        stream.write_u8(process_category::SERVER_COMPONENT);
        stream.write_u16(0);
        stream.write_u16(11);
        stream.write_u16(0);
        stream.write_u16(0);
        stream.write_string("cellapp01").expect("write");

        let read = ProcessMessage::read(&mut stream).expect("read");
        assert_eq!(read.pid, 11);
        assert_eq!(read.major_version, 0);
        assert_eq!(read.interface_version, 0);
        assert_eq!(read.username, "");
    }

    #[test]
    fn future_extra_fields_are_skipped() {
        // A sender from a newer version appends an unknown u16 to the extra
        // block and declares it in the tail size; the reader must skip it.
        let mut stream = BinaryStream::new();
        stream.write_u8(message_id::PROCESS);
        stream.write_u8(0);
        stream.write_u16(0);
        stream.write_u8(0);
        stream.write_u8(process_category::SERVER_COMPONENT);
        stream.write_u16(0);
        stream.write_u16(4242);
        stream.write_u16(0);
        stream.write_u16(0);
        stream.write_string("cellapp01").expect("write");
        // Tail covers versions, empty username, empty digest and 2 future bytes.
        stream.write_i32(12);
        stream.write_u16(0);
        stream.write_u16(0);
        stream.write_u16(0);
        stream.write_u16(MERCURY_INTERFACE_VERSION);
        stream.write_string("").expect("write");
        stream.write_string("").expect("write");
        stream.write_u16(0xBEEF);

        let read = ProcessMessage::read(&mut stream).expect("read");
        assert_eq!(read.pid, 4242);
        assert_eq!(read.name, "cellapp01");
        assert_eq!(read.interface_version, MERCURY_INTERFACE_VERSION);
        assert!(stream.at_end());
    }

    #[test]
    fn matches_honors_only_requested_fields() {
        let process = ProcessMessage {
            uid: 1000,
            pid: 4242,
            name: "cellapp01".to_string(),
            ..ProcessMessage::default()
        };

        let mut query = ProcessMessage {
            param: process_param::USE_PID,
            pid: 4242,
            ..ProcessMessage::default()
        };
        assert!(process.matches(&query));

        query.pid = 1;
        assert!(!process.matches(&query));

        // Without the bit set the difference is ignored.
        query.param = 0;
        query.pid = 1;
        assert!(process.matches(&query));

        // The type bit itself only rides along in real traffic.
        query.param = process_param::USE_NAME | process_param::IS_MSGTYPE;
        query.name = "baseapp01".to_string();
        assert!(!process.matches(&query));
    }

    #[test]
    fn describe_matches_reference_format() {
        let message = ProcessMessage {
            pid: 55,
            name: "cellapp01".to_string(),
            ..ProcessMessage::default()
        };
        assert_eq!(message.describe(), "cellapp01 (SERVER_COMPONENT) (pid:55)");
    }

    #[test]
    fn listener_message_round_trips() {
        let mut listener = ListenerMessage {
            process: ProcessMessage {
                name: "CellAppInterface".to_string(),
                ..ProcessMessage::default()
            },
            pre_addr: "127.0.0.1:7000".to_string(),
            post_addr: "127.0.0.1:7001".to_string(),
        };
        listener.process.param = process_param::IS_MSGTYPE | listener_type::ADD_BIRTH_LISTENER;

        let mut stream = BinaryStream::new();
        listener.write(&mut stream).expect("write");
        assert_eq!(ListenerMessage::read(&mut stream).expect("read"), listener);
    }

    #[test]
    fn signal_message_round_trips_and_shutdown_levels() {
        let mut command = SignalMessage {
            process: ProcessMessage {
                name: "cellapp01".to_string(),
                ..ProcessMessage::default()
            },
            signal: 0,
        };

        let mut stream = BinaryStream::new();
        command.write(&mut stream).expect("write");
        assert_eq!(SignalMessage::read(&mut stream).expect("read"), command);

        command.set_controlled_shutdown();
        assert_eq!(command.signal, signal::SIGUSR1);
        command.set_kill();
        assert_eq!(command.signal, signal::SIGINT);
        command.set_hard_kill();
        assert_eq!(command.signal, signal::SIGQUIT);
    }
}
