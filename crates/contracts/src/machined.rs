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

/// Map a value onto the 0-255 byte a MachineGuard message carries, using the
/// reference's `value * 0xff / max` scaling.
pub fn scale_percentage(value: u64, max: u64) -> u8 {
    if max == 0 {
        return 0;
    }
    (value.saturating_mul(0xff) / max) as u8
}

/// CPU load from the change in busy jiffies over the change in total jiffies.
///
/// The reference clamps the divisor to at least one (`std::max( delta, 1 )`)
/// so an idle period between samples does not divide by zero.
pub fn cpu_load(busy_delta: u64, total_delta: u64) -> u8 {
    scale_percentage(busy_delta, total_delta.max(1))
}

/// Memory used as a fraction of total, `used * 0xff / total`.
pub fn memory_usage(used: u64, total: u64) -> u8 {
    scale_percentage(used, total)
}

/// IO wait as a fraction of total jiffies, clamped divisor like cpu load.
pub fn io_wait(io_delta: u64, total_delta: u64) -> u8 {
    scale_percentage(io_delta, total_delta.max(1))
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
    /// Write the fixed fields: header through the interface name, with no
    /// extra block. Subclasses (e.g. `ProcessStatsMessage`) add their own
    /// fields between the body and the extra block.
    pub fn write_body(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_u8(self.param);
        stream.write_u8(self.category);
        stream.write_u16(self.uid);
        stream.write_u16(self.pid);
        stream.write_u16(self.port);
        stream.write_u16(self.id);
        stream.write_string(&self.name)?;
        Ok(())
    }

    /// Write the versioned extra block that trails a process body.
    pub fn write_extra(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        // The declared tail size covers everything after the size field, so
        // unknown future fields can be skipped on read.
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

    /// Write the message body plus the versioned extra block.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.write_body(stream)?;
        self.write_extra(stream)
    }

    /// Read the fixed fields with an empty extra block.
    pub fn read_body(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let header = Header::read(stream)?;
        let param = stream.read_u8()?;
        let category = stream.read_u8()?;
        let uid = stream.read_u16()?;
        let pid = stream.read_u16()?;
        let port = stream.read_u16()?;
        let id = stream.read_u16()?;
        let name = stream.read_string()?;
        Ok(Self {
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
        })
    }

    /// Read the versioned extra block if one is present.
    pub fn read_extra(stream: &mut BinaryStream, message: &mut Self) -> Result<(), StreamError> {
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
        Ok(())
    }

    /// Read a message, tolerating senders without the extra block.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let mut message = Self::read_body(stream)?;
        Self::read_extra(stream, &mut message)?;
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// The build configurations the reference accepts for a process start,
/// `BW_CONFIG_HYBRID` and `BW_CONFIG_DEBUG` from `build/make/Makefile`.
pub mod config {
    pub const HYBRID: &str = "hybrid";
    pub const DEBUG: &str = "debug";
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

// ---------------------------------------------------------------------------
// Machine statistics (BW-05-01-01: MachineGuard query messages).
// ---------------------------------------------------------------------------

/// `LowPrecisionInterfaceStats`: one interface's counters for a
/// `WholeMachineMessage`. The bits/packs fields are whole percent of the
/// link, carried in a byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowPrecisionInterfaceStats {
    pub name: String,
    pub bits_in: u8,
    pub bits_out: u8,
    pub pack_in: u8,
    pub pack_out: u8,
}

/// `HighPrecisionInterfaceStats`: the wider counters a
/// `HighPrecisionMachineMessage` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighPrecisionInterfaceStats {
    pub name: String,
    pub bits_in: u32,
    pub bits_out: u32,
    pub pack_in: u32,
    pub pack_out: u32,
}

/// `WholeMachineMessage`: low precision whole-machine statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WholeMachineMessage {
    pub header: Header,
    pub hostname: String,
    /// CPU speed in MHz.
    pub cpu_speed: u16,
    pub n_cpus: u8,
    pub cpu_loads: Vec<u8>,
    /// Memory usage as a percent.
    pub mem: u8,
    pub n_interfaces: u8,
    pub if_stats: Vec<LowPrecisionInterfaceStats>,
    pub in_discards: u8,
    pub out_discards: u8,
    /// The machined version.
    pub version: u8,
}

impl Default for WholeMachineMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::WHOLE_MACHINE,
                flags: 0,
                seq: 0,
            },
            hostname: String::new(),
            cpu_speed: 0,
            n_cpus: 0,
            cpu_loads: Vec::new(),
            mem: 0,
            n_interfaces: 0,
            if_stats: Vec::new(),
            in_discards: 0,
            out_discards: 0,
            version: 0,
        }
    }
}

impl WholeMachineMessage {
    /// Write the message body in the reference field order.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_string(&self.hostname)?;
        stream.write_u16(self.cpu_speed);
        stream.write_u8(self.n_cpus);
        stream.write_u8(self.n_interfaces);
        stream.write_u8(self.mem);
        stream.write_u8(self.version);
        stream.write_u8(self.in_discards);
        stream.write_u8(self.out_discards);
        for load in &self.cpu_loads {
            stream.write_u8(*load);
        }
        for stats in &self.if_stats {
            stream.write_string(&stats.name)?;
            stream.write_u8(stats.bits_in);
            stream.write_u8(stats.bits_out);
            stream.write_u8(stats.pack_in);
            stream.write_u8(stats.pack_out);
        }
        Ok(())
    }

    /// Read a whole-machine message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let header = Header::read(stream)?;
        let hostname = stream.read_string()?;
        let cpu_speed = stream.read_u16()?;
        let n_cpus = stream.read_u8()?;
        let n_interfaces = stream.read_u8()?;
        let mem = stream.read_u8()?;
        let version = stream.read_u8()?;
        let in_discards = stream.read_u8()?;
        let out_discards = stream.read_u8()?;
        let mut cpu_loads = Vec::with_capacity(n_cpus as usize);
        for _ in 0..n_cpus {
            cpu_loads.push(stream.read_u8()?);
        }
        let mut if_stats = Vec::with_capacity(n_interfaces as usize);
        for _ in 0..n_interfaces {
            if_stats.push(LowPrecisionInterfaceStats {
                name: stream.read_string()?,
                bits_in: stream.read_u8()?,
                bits_out: stream.read_u8()?,
                pack_in: stream.read_u8()?,
                pack_out: stream.read_u8()?,
            });
        }
        Ok(Self {
            header,
            hostname,
            cpu_speed,
            n_cpus,
            cpu_loads,
            mem,
            n_interfaces,
            if_stats,
            in_discards,
            out_discards,
            version,
        })
    }
}

/// `HighPrecisionMachineMessage`: detailed whole-machine statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighPrecisionMachineMessage {
    pub header: Header,
    pub hostname: String,
    pub cpu_speed: u16,
    pub n_cpus: u8,
    pub cpu_loads: Vec<u8>,
    /// Percent of time waiting for IO.
    pub io_wait_time: u8,
    pub mem: u8,
    pub n_interfaces: u8,
    pub if_stats: Vec<HighPrecisionInterfaceStats>,
    pub in_discards: u8,
    pub out_discards: u8,
    pub version: u8,
}

impl Default for HighPrecisionMachineMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::HIGH_PRECISION_MACHINE,
                flags: 0,
                seq: 0,
            },
            hostname: String::new(),
            cpu_speed: 0,
            n_cpus: 0,
            cpu_loads: Vec::new(),
            io_wait_time: 0,
            mem: 0,
            n_interfaces: 0,
            if_stats: Vec::new(),
            in_discards: 0,
            out_discards: 0,
            version: 0,
        }
    }
}

impl HighPrecisionMachineMessage {
    /// Write the message body in the reference field order.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_string(&self.hostname)?;
        stream.write_u16(self.cpu_speed);
        stream.write_u8(self.n_cpus);
        stream.write_u8(self.n_interfaces);
        stream.write_u8(self.mem);
        stream.write_u8(self.version);
        stream.write_u8(self.in_discards);
        stream.write_u8(self.out_discards);
        for load in &self.cpu_loads {
            stream.write_u8(*load);
        }
        // This is where the message diverges from `WholeMachineMessage`.
        stream.write_u8(self.io_wait_time);
        for stats in &self.if_stats {
            stream.write_string(&stats.name)?;
            stream.write_u32(stats.bits_in);
            stream.write_u32(stats.bits_out);
            stream.write_u32(stats.pack_in);
            stream.write_u32(stats.pack_out);
        }
        Ok(())
    }

    /// Read a high precision machine message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let header = Header::read(stream)?;
        let hostname = stream.read_string()?;
        let cpu_speed = stream.read_u16()?;
        let n_cpus = stream.read_u8()?;
        let n_interfaces = stream.read_u8()?;
        let mem = stream.read_u8()?;
        let version = stream.read_u8()?;
        let in_discards = stream.read_u8()?;
        let out_discards = stream.read_u8()?;
        let mut cpu_loads = Vec::with_capacity(n_cpus as usize);
        for _ in 0..n_cpus {
            cpu_loads.push(stream.read_u8()?);
        }
        let io_wait_time = stream.read_u8()?;
        let mut if_stats = Vec::with_capacity(n_interfaces as usize);
        for _ in 0..n_interfaces {
            if_stats.push(HighPrecisionInterfaceStats {
                name: stream.read_string()?,
                bits_in: stream.read_u32()?,
                bits_out: stream.read_u32()?,
                pack_in: stream.read_u32()?,
                pack_out: stream.read_u32()?,
            });
        }
        Ok(Self {
            header,
            hostname,
            cpu_speed,
            n_cpus,
            cpu_loads,
            io_wait_time,
            mem,
            n_interfaces,
            if_stats,
            in_discards,
            out_discards,
            version,
        })
    }
}

/// `ProcessStatsMessage`: a process body plus cpu and memory usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessStatsMessage {
    pub process: ProcessMessage,
    /// Process cpu as a proportion of max.
    pub cpu: u8,
    /// Memory usage as a proportion of max.
    pub mem: u8,
}

impl Default for ProcessStatsMessage {
    fn default() -> Self {
        Self {
            process: ProcessMessage {
                header: Header {
                    message: message_id::PROCESS_STATS,
                    flags: 0,
                    seq: 0,
                },
                ..ProcessMessage::default()
            },
            cpu: 0,
            mem: 0,
        }
    }
}

impl ProcessStatsMessage {
    /// Write the process body, then the cpu/mem pair, then the extra block.
    /// The message id is forced to `PROCESS_STATS`, exactly as the reference
    /// constructor sets it, regardless of the wrapped process header.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        let mut process = self.process.clone();
        process.header.message = message_id::PROCESS_STATS;
        process.write_body(stream)?;
        stream.write_u8(self.cpu);
        stream.write_u8(self.mem);
        process.write_extra(stream)
    }

    /// Read a process stats message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let mut process = ProcessMessage::read_body(stream)?;
        let cpu = stream.read_u8()?;
        let mem = stream.read_u8()?;
        process.header.message = message_id::PROCESS_STATS;
        ProcessMessage::read_extra(stream, &mut process)?;
        Ok(Self { process, cpu, mem })
    }
}

/// `QueryInterfaceMessage`: asks machined for the address of an interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryInterfaceMessage {
    pub header: Header,
    /// The address of the requesting interface.
    pub address: u32,
}

impl Default for QueryInterfaceMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::QUERY_INTERFACE,
                flags: 0,
                seq: 0,
            },
            address: 0,
        }
    }
}

impl QueryInterfaceMessage {
    /// Write the message body.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_u32(self.address);
        Ok(())
    }

    /// Read a query-interface message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        Ok(Self {
            header: Header::read(stream)?,
            address: stream.read_u32()?,
        })
    }
}

/// `MachinePlatformMessage`: the OS distribution and release string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachinePlatformMessage {
    pub header: Header,
    pub platform_info: String,
}

impl Default for MachinePlatformMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::MACHINE_PLATFORM,
                flags: 0,
                seq: 0,
            },
            platform_info: String::new(),
        }
    }
}

impl MachinePlatformMessage {
    /// Write the message body.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_string(&self.platform_info)?;
        Ok(())
    }

    /// Read a machine platform message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        Ok(Self {
            header: Header::read(stream)?,
            platform_info: stream.read_string()?,
        })
    }
}

/// `TagsMessage`: the tags defined for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagsMessage {
    pub header: Header,
    pub tags: Vec<String>,
    /// Flag indicating if a category exists at all.
    pub exists: u8,
}

impl Default for TagsMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::TAGS,
                flags: 0,
                seq: 0,
            },
            tags: Vec::new(),
            exists: 0,
        }
    }
}

impl TagsMessage {
    /// Write the message body.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_u8(self.tags.len() as u8);
        stream.write_u8(self.exists);
        for tag in &self.tags {
            stream.write_string(tag)?;
        }
        Ok(())
    }

    /// Read a tags message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let header = Header::read(stream)?;
        let tag_count = stream.read_u8()?;
        let exists = stream.read_u8()?;
        let mut tags = Vec::with_capacity(tag_count as usize);
        for _ in 0..tag_count {
            tags.push(stream.read_string()?);
        }
        Ok(Self {
            header,
            tags,
            exists,
        })
    }
}

/// `UserMessage::Param` bits.
pub mod user_param {
    pub const USE_UID: u8 = 0x01;
    pub const USE_NAME: u8 = 0x02;
    pub const CHECK_COREDUMPS: u8 = 0x04;
    pub const REFRESH_ENV: u8 = 0x08;
    pub const GET_VERSION: u8 = 0x10;
}

/// `UserMessage::CoreDump`: one reported coredump file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreDump {
    pub filename: String,
    pub assert: String,
    pub time: u32,
}

/// `UserMessage`: information about one user on the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessage {
    pub header: Header,
    pub param: u8,
    pub uid: u16,
    pub username: String,
    pub fullname: String,
    pub home: String,
    pub mfroot: String,
    pub bwrespath: String,
    pub coredumps: Vec<CoreDump>,
    pub version_string: String,
}

impl Default for UserMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::USER,
                flags: 0,
                seq: 0,
            },
            param: 0,
            uid: 0,
            username: String::new(),
            fullname: String::new(),
            home: String::new(),
            mfroot: String::new(),
            bwrespath: String::new(),
            coredumps: Vec::new(),
            version_string: String::new(),
        }
    }
}

impl UserMessage {
    /// Unsuccessful queries carry this in the uid field.
    pub const UID_NOT_FOUND: u16 = 0xffff;

    /// Write the message body. Coredumps are only reported when requested,
    /// and the version string only when it was asked for, matching the
    /// reference's conditional writes.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_u8(self.param);
        stream.write_u16(self.uid);
        stream.write_string(&self.username)?;
        stream.write_string(&self.fullname)?;
        stream.write_string(&self.home)?;
        stream.write_string(&self.mfroot)?;
        stream.write_string(&self.bwrespath)?;

        if self.param & user_param::CHECK_COREDUMPS != 0 {
            stream.write_u32(self.coredumps.len() as u32);
            for dump in &self.coredumps {
                stream.write_string(&dump.filename)?;
                stream.write_string(&dump.assert)?;
                stream.write_u32(dump.time);
            }
        } else {
            stream.write_u32(0);
        }

        if self.param & user_param::GET_VERSION != 0 {
            stream.write_string(&self.version_string)?;
        }
        Ok(())
    }

    /// Read a user message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let header = Header::read(stream)?;
        let param = stream.read_u8()?;
        let uid = stream.read_u16()?;
        let username = stream.read_string()?;
        let fullname = stream.read_string()?;
        let home = stream.read_string()?;
        let mfroot = stream.read_string()?;
        let bwrespath = stream.read_string()?;
        let count = stream.read_u32()?;
        let mut coredumps = Vec::with_capacity(count as usize);
        for _ in 0..count {
            coredumps.push(CoreDump {
                filename: stream.read_string()?,
                assert: stream.read_string()?,
                time: stream.read_u32()?,
            });
        }

        let mut version_string = String::new();
        if param & user_param::GET_VERSION != 0 && !stream.at_end() {
            version_string = stream.read_string()?;
        }

        Ok(Self {
            header,
            param,
            uid,
            username,
            fullname,
            home,
            mfroot,
            bwrespath,
            coredumps,
            version_string,
        })
    }
}

/// `PidMessage`: asks whether a pid exists on this system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidMessage {
    pub header: Header,
    pub pid: u16,
    pub running: u8,
}

impl Default for PidMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::PID,
                flags: 0,
                seq: 0,
            },
            pid: 0,
            running: 0,
        }
    }
}

impl PidMessage {
    /// Write the message body.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_u16(self.pid);
        stream.write_u8(self.running);
        Ok(())
    }

    /// Read a pid message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        Ok(Self {
            header: Header::read(stream)?,
            pid: stream.read_u16()?,
            running: stream.read_u8()?,
        })
    }
}

/// `ResetMessage`: flush tags and user mappings. Header only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetMessage {
    pub header: Header,
}

impl Default for ResetMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::RESET,
                flags: 0,
                seq: 0,
            },
        }
    }
}

impl ResetMessage {
    /// Write the message body (the header alone).
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        Ok(())
    }

    /// Read a reset message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        Ok(Self {
            header: Header::read(stream)?,
        })
    }
}

/// `ErrorMessage`: machined's way of reporting errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorMessage {
    pub header: Header,
    pub severity: u8,
    pub msg: String,
}

impl Default for ErrorMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::ERROR,
                flags: 0,
                seq: 0,
            },
            severity: 0,
            msg: String::new(),
        }
    }
}

impl ErrorMessage {
    /// Write the message body.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_u8(self.severity);
        stream.write_string(&self.msg)?;
        Ok(())
    }

    /// Read an error message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        Ok(Self {
            header: Header::read(stream)?,
            severity: stream.read_u8()?,
            msg: stream.read_string()?,
        })
    }
}

/// `CreateMessage`: commands machined to spawn a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateMessage {
    pub header: Header,
    /// The name of the executable to start.
    pub name: String,
    /// Hybrid, Debug etc.
    pub config: String,
    pub uid: u16,
    /// Start with `-recover`.
    pub recover: u8,
    /// IP to forward output to.
    pub fwd_ip: u32,
    /// Port to forward output to.
    pub fwd_port: u16,
}

impl Default for CreateMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::CREATE,
                flags: 0,
                seq: 0,
            },
            name: String::new(),
            config: String::new(),
            uid: 0,
            recover: 0,
            fwd_ip: 0,
            fwd_port: 0,
        }
    }
}

impl CreateMessage {
    /// Write the message body.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_string(&self.name)?;
        stream.write_string(&self.config)?;
        stream.write_u16(self.uid);
        stream.write_u8(self.recover);
        stream.write_u32(self.fwd_ip);
        stream.write_u16(self.fwd_port);
        Ok(())
    }

    /// Read a create message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        Ok(Self {
            header: Header::read(stream)?,
            name: stream.read_string()?,
            config: stream.read_string()?,
            uid: stream.read_u16()?,
            recover: stream.read_u8()?,
            fwd_ip: stream.read_u32()?,
            fwd_port: stream.read_u16()?,
        })
    }
}

/// Why a process start request is refused, mirroring the reference's exit
/// states in `handleCreateMessage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateRejectReason {
    /// The config is neither `hybrid` nor `debug`.
    InvalidConfig,
    /// The name or config contains a `..` path escape.
    IllegalPath,
    /// The name runs a `commands/_helpers` setuid helper.
    HelperProcess,
}

/// Validate a process start request the way the reference
/// `handleCreateMessage` does, returning the first reason it would refuse.
///
/// The order matches the reference: config first, then the `..` check, then
/// the helper directory. The platform-dependent checks (the uid exists, its
/// environment, and the binary location) are left to the caller.
pub fn validate_create(cm: &CreateMessage) -> Option<CreateRejectReason> {
    // `bw_stricmp`: a case-insensitive comparison against the two configs.
    let config = cm.config.to_ascii_lowercase();
    if config != config::HYBRID && config != config::DEBUG {
        return Some(CreateRejectReason::InvalidConfig);
    }
    if cm.name.contains("..") || cm.config.contains("..") {
        return Some(CreateRejectReason::IllegalPath);
    }
    if cm.name.contains("commands/_helpers") {
        return Some(CreateRejectReason::HelperProcess);
    }
    None
}

/// `CreateWithArgsMessage`: a create with explicit command line arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWithArgsMessage {
    pub create: CreateMessage,
    pub args: Vec<String>,
}

impl Default for CreateWithArgsMessage {
    fn default() -> Self {
        Self {
            create: CreateMessage {
                header: Header {
                    message: message_id::CREATE_WITH_ARGS,
                    flags: 0,
                    seq: 0,
                },
                ..CreateMessage::default()
            },
            args: Vec::new(),
        }
    }
}

impl CreateWithArgsMessage {
    /// Write the message body.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.create.write(stream)?;
        stream.write_u32(self.args.len() as u32);
        for arg in &self.args {
            stream.write_string(arg)?;
        }
        Ok(())
    }

    /// Read a create-with-args message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let create = CreateMessage::read(stream)?;
        let num_args = stream.read_u32()?;
        let mut args = Vec::with_capacity(num_args as usize);
        for _ in 0..num_args {
            args.push(stream.read_string()?);
        }
        Ok(Self { create, args })
    }
}

/// `MachinedAnnounceMessage::Type`.
pub mod machined_announce_type {
    pub const BIRTH: u8 = 0;
    pub const DEATH: u8 = 1;
    pub const EXISTS: u8 = 2;
}

/// `MachinedAnnounceMessage`: machined-to-machined ring announcements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachinedAnnounceMessage {
    pub header: Header,
    pub announce_type: u8,
    /// Birth replies use this to tell the size of the network; death/exists
    /// use it as the machine address.
    pub count: u32,
}

impl Default for MachinedAnnounceMessage {
    fn default() -> Self {
        Self {
            header: Header {
                message: message_id::MACHINED_ANNOUNCE,
                flags: 0,
                seq: 0,
            },
            announce_type: machined_announce_type::BIRTH,
            count: 0,
        }
    }
}

impl MachinedAnnounceMessage {
    /// Write the message body.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_u8(self.announce_type);
        stream.write_u32(self.count);
        Ok(())
    }

    /// Read an announce message.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        Ok(Self {
            header: Header::read(stream)?,
            announce_type: stream.read_u8()?,
            count: stream.read_u32()?,
        })
    }
}

/// `UnknownMessage`: an unrecognised message echoed back with the
/// `MESSAGE_NOT_UNDERSTOOD` flag. The raw bytes are kept as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMessage {
    pub header: Header,
    /// Bytes after the header, carried through untouched.
    pub data: Vec<u8>,
}

impl UnknownMessage {
    /// Write the header plus the raw tail.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        self.header.write(stream);
        stream.write_bytes(&self.data);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MGMPacket carrier and message dispatch (BW-05-01-01).
// ---------------------------------------------------------------------------

/// `MGMPacket::Flags`.
pub mod mgm_packet_flags {
    /// Replies may be staggered.
    pub const PACKET_STAGGER_REPLIES: u8 = 0x1;
}

/// `MGMPacket`: a UDP datagram carrying one or more MachineGuardMessages.
///
/// Wire layout: flags u8, buddy u32, then each message as `u16 length`
/// (network order) plus the message bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MgmPacket {
    pub flags: u8,
    pub buddy: u32,
    pub messages: Vec<MachineGuardMessage>,
}

impl MgmPacket {
    /// The reference datagram size ceiling.
    pub const MAX_SIZE: usize = 32768;

    /// Build a packet carrying the given messages.
    pub fn new(messages: Vec<MachineGuardMessage>) -> Self {
        Self {
            flags: 0,
            buddy: 0,
            messages,
        }
    }

    /// Should replies to this packet be staggered?
    pub fn should_stagger_reply(&self) -> bool {
        self.flags & mgm_packet_flags::PACKET_STAGGER_REPLIES != 0
    }

    /// Serialize the packet. The buddy field of the packet is substituted
    /// for every message's own buddy when set by machined; the wire carries
    /// the packet buddy once.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        stream.write_u8(self.flags);
        stream.write_u32(self.buddy);
        for message in &self.messages {
            let mut sub = BinaryStream::new();
            message.write(&mut sub)?;
            let bytes = sub.as_bytes();
            stream.write_u16(bytes.len() as u16);
            stream.write_bytes(bytes);
        }
        Ok(())
    }

    /// Parse a packet, giving each enclosed message its own bounded stream
    /// exactly like the reference's per-message `MemoryIStream`.
    pub fn read(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let flags = stream.read_u8()?;
        let buddy = stream.read_u32()?;
        let mut messages = Vec::new();
        while !stream.at_end() {
            let msglen = stream.read_u16()? as usize;
            let bytes = stream.read_blob_raw(msglen)?;
            let mut sub = BinaryStream::from_slice(&bytes);
            messages.push(MachineGuardMessage::create(&mut sub)?);
        }
        Ok(Self {
            flags,
            buddy,
            messages,
        })
    }

    /// Serialize a packet to a byte vector, refusing a datagram the
    /// reference would report as oversized.
    pub fn to_bytes(&self) -> Result<Vec<u8>, StreamError> {
        let mut stream = BinaryStream::new();
        self.write(&mut stream)?;
        Ok(stream.as_bytes().to_vec())
    }

    /// Parse a packet from raw bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StreamError> {
        let mut stream = BinaryStream::from_slice(bytes);
        let packet = Self::read(&mut stream)?;
        Ok(packet)
    }
}

/// A dispatched MachineGuard message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineGuardMessage {
    WholeMachine(WholeMachineMessage),
    Process(ProcessMessage),
    ProcessStats(ProcessStatsMessage),
    Listener(ListenerMessage),
    Create(CreateMessage),
    CreateWithArgs(CreateWithArgsMessage),
    Signal(SignalMessage),
    Tags(TagsMessage),
    User(UserMessage),
    Pid(PidMessage),
    Reset(ResetMessage),
    Error(ErrorMessage),
    QueryInterface(QueryInterfaceMessage),
    HighPrecision(HighPrecisionMachineMessage),
    MachinePlatform(MachinePlatformMessage),
    MachinedAnnounce(MachinedAnnounceMessage),
    Unknown(UnknownMessage),
}

impl MachineGuardMessage {
    /// Peek at the message id without consuming it.
    pub fn message_id(stream: &BinaryStream) -> Option<u8> {
        stream.peek()
    }

    /// Construct the right variant for the message at the cursor. Unknown
    /// ids, and any message whose typed read fails, fall back to
    /// `UnknownMessage`, matching the reference's defensive path.
    pub fn create(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let id = stream.peek().ok_or(StreamError::Truncated)?;
        let parsed = match id {
            message_id::WHOLE_MACHINE => WholeMachineMessage::read(stream).map(Self::WholeMachine),
            message_id::HIGH_PRECISION_MACHINE => {
                HighPrecisionMachineMessage::read(stream).map(Self::HighPrecision)
            }
            message_id::PROCESS => ProcessMessage::read(stream).map(Self::Process),
            message_id::PROCESS_STATS => ProcessStatsMessage::read(stream).map(Self::ProcessStats),
            message_id::LISTENER => ListenerMessage::read(stream).map(Self::Listener),
            message_id::CREATE => CreateMessage::read(stream).map(Self::Create),
            message_id::CREATE_WITH_ARGS => {
                CreateWithArgsMessage::read(stream).map(Self::CreateWithArgs)
            }
            message_id::SIGNAL => SignalMessage::read(stream).map(Self::Signal),
            message_id::TAGS => TagsMessage::read(stream).map(Self::Tags),
            message_id::USER => UserMessage::read(stream).map(Self::User),
            message_id::PID => PidMessage::read(stream).map(Self::Pid),
            message_id::RESET => ResetMessage::read(stream).map(Self::Reset),
            message_id::ERROR => ErrorMessage::read(stream).map(Self::Error),
            message_id::QUERY_INTERFACE => {
                QueryInterfaceMessage::read(stream).map(Self::QueryInterface)
            }
            message_id::MACHINE_PLATFORM => {
                MachinePlatformMessage::read(stream).map(Self::MachinePlatform)
            }
            message_id::MACHINED_ANNOUNCE => {
                MachinedAnnounceMessage::read(stream).map(Self::MachinedAnnounce)
            }
            _ => {
                // Echo the message back with the not-understood flag set,
                // which is what the reference does for unknown ids.
                stream.rewind();
                let mut header = Self::read_header(stream)?;
                header.flags |= flags::MESSAGE_NOT_UNDERSTOOD;
                let data = stream.read_blob_raw(stream.remaining())?;
                return Ok(Self::Unknown(UnknownMessage { header, data }));
            }
        };

        match parsed {
            Ok(message) => Ok(message),
            Err(_) => {
                // A message that failed its typed read falls back to the raw
                // echo the reference produces.
                stream.rewind();
                let mut header = Self::read_header(stream)?;
                header.flags |= flags::MESSAGE_NOT_UNDERSTOOD;
                let data = stream.read_blob_raw(stream.remaining())?;
                Ok(Self::Unknown(UnknownMessage { header, data }))
            }
        }
    }

    fn read_header(stream: &mut BinaryStream) -> Result<Header, StreamError> {
        Header::read(stream)
    }

    /// Serialize the message to the stream.
    pub fn write(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        match self {
            Self::WholeMachine(m) => m.write(stream),
            Self::Process(m) => m.write(stream),
            Self::ProcessStats(m) => m.write(stream),
            Self::Listener(m) => m.write(stream),
            Self::Create(m) => m.write(stream),
            Self::CreateWithArgs(m) => m.write(stream),
            Self::Signal(m) => m.write(stream),
            Self::Tags(m) => m.write(stream),
            Self::User(m) => m.write(stream),
            Self::Pid(m) => m.write(stream),
            Self::Reset(m) => m.write(stream),
            Self::Error(m) => m.write(stream),
            Self::QueryInterface(m) => m.write(stream),
            Self::HighPrecision(m) => m.write(stream),
            Self::MachinePlatform(m) => m.write(stream),
            Self::MachinedAnnounce(m) => m.write(stream),
            Self::Unknown(m) => m.write(stream),
        }
    }

    /// The message header.
    pub fn header(&self) -> &Header {
        match self {
            Self::WholeMachine(m) => &m.header,
            Self::Process(m) => &m.header,
            Self::ProcessStats(m) => &m.process.header,
            Self::Listener(m) => &m.process.header,
            Self::Create(m) => &m.header,
            Self::CreateWithArgs(m) => &m.create.header,
            Self::Signal(m) => &m.process.header,
            Self::Tags(m) => &m.header,
            Self::User(m) => &m.header,
            Self::Pid(m) => &m.header,
            Self::Reset(m) => &m.header,
            Self::Error(m) => &m.header,
            Self::QueryInterface(m) => &m.header,
            Self::HighPrecision(m) => &m.header,
            Self::MachinePlatform(m) => &m.header,
            Self::MachinedAnnounce(m) => &m.header,
            Self::Unknown(m) => &m.header,
        }
    }

    /// Set the sequence number, as the reference's `copySeq` does when
    /// tagging a pre-generated reply.
    pub fn set_seq(&mut self, seq: u16) {
        let header = self.header_mut();
        header.seq = seq;
    }

    fn header_mut(&mut self) -> &mut Header {
        match self {
            Self::WholeMachine(m) => &mut m.header,
            Self::Process(m) => &mut m.header,
            Self::ProcessStats(m) => &mut m.process.header,
            Self::Listener(m) => &mut m.process.header,
            Self::Create(m) => &mut m.header,
            Self::CreateWithArgs(m) => &mut m.create.header,
            Self::Signal(m) => &mut m.process.header,
            Self::Tags(m) => &mut m.header,
            Self::User(m) => &mut m.header,
            Self::Pid(m) => &mut m.header,
            Self::Reset(m) => &mut m.header,
            Self::Error(m) => &mut m.header,
            Self::QueryInterface(m) => &mut m.header,
            Self::HighPrecision(m) => &mut m.header,
            Self::MachinePlatform(m) => &mut m.header,
            Self::MachinedAnnounce(m) => &mut m.header,
            Self::Unknown(m) => &mut m.header,
        }
    }

    /// Serialize to a byte vector.
    pub fn to_bytes(&self) -> Result<Vec<u8>, StreamError> {
        let mut stream = BinaryStream::new();
        self.write(&mut stream)?;
        Ok(stream.as_bytes().to_vec())
    }

    /// Parse one message from raw bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StreamError> {
        let mut stream = BinaryStream::from_slice(bytes);
        Self::create(&mut stream)
    }
}

// ---------------------------------------------------------------------------
// Query answering (BW-05-01-01).
// ---------------------------------------------------------------------------

/// Outcome of a `register` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// A new process entry was appended.
    Registered,
    /// A process with the same pid, category and name already existed; its
    /// fields were replaced in place. The reference logs this as a bad
    /// re-registration but keeps the existing slot.
    ReRegistered,
}

/// Outcome of a `deregister` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeregisterOutcome {
    /// The process was found and removed.
    Removed,
    /// No process with that pid existed.
    NotFound,
}

/// A registered process and its most recently collected stats.
///
/// Mirrors the reference `ProcessInfo`: the process message plus the cpu and
/// memory bytes the statistics query carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredProcess {
    pub process: ProcessMessage,
    /// CPU load as a 0-255 byte, `(busy_jiffies_delta * 0xff / cpu_delta)`.
    pub cpu: u8,
    /// Memory as a 0-255 byte, `(vsize * 0xff / total_mem)`.
    pub mem: u8,
}

impl RegisteredProcess {
    fn new(process: ProcessMessage) -> Self {
        Self {
            process,
            cpu: 0,
            mem: 0,
        }
    }

    /// Whether this process matches a discovery query.
    pub fn matches(&self, query: &ProcessMessage) -> bool {
        self.process.matches(query)
    }
}

/// The bwmachined process registry.
///
/// Mirrors the reference `BWMachined::procs_` list. Registering appends a
/// process (or replaces one that already claims the same pid/category/name or
/// the same port), deregistering removes by pid, and discovery queries read
/// from the same list, so a query issued immediately after a change reflects
/// it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MachinedRegistry {
    processes: Vec<RegisteredProcess>,
}

impl MachinedRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry pre-seeded with processes.
    pub fn from_processes(processes: Vec<ProcessMessage>) -> Self {
        Self {
            processes: processes.into_iter().map(RegisteredProcess::new).collect(),
        }
    }

    /// The registered processes, in registration order.
    pub fn processes(&self) -> &[RegisteredProcess] {
        &self.processes
    }

    pub fn len(&self) -> usize {
        self.processes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    /// Register a process.
    ///
    /// Matches the reference clash loop: a process with the same pid,
    /// category and name re-uses its existing slot (re-registration), and any
    /// process already on the same port is removed first (the reference's
    /// "can't have two things on one port" rule, applied to port 0 as well).
    /// Re-registration resets the collected stats.
    pub fn register(&mut self, pm: &ProcessMessage) -> RegisterOutcome {
        let mut i = 0;
        while i < self.processes.len() {
            let existing = &self.processes[i].process;
            if pm.pid == existing.pid
                && pm.category == existing.category
                && pm.name == existing.name
            {
                break;
            }
            if pm.port == existing.port {
                // The new process claims this port; drop the old owner and
                // re-examine the slot its removal left behind.
                self.processes.remove(i);
            } else {
                i += 1;
            }
        }

        if i < self.processes.len() {
            self.processes[i] = RegisteredProcess::new(pm.clone());
            RegisterOutcome::ReRegistered
        } else {
            self.processes.push(RegisteredProcess::new(pm.clone()));
            RegisterOutcome::Registered
        }
    }

    /// Deregister the first process with the given pid.
    pub fn deregister(&mut self, pid: u16) -> DeregisterOutcome {
        match self.processes.iter().position(|p| p.process.pid == pid) {
            Some(index) => {
                self.processes.remove(index);
                DeregisterOutcome::Removed
            }
            None => DeregisterOutcome::NotFound,
        }
    }

    /// The first process matching a discovery query's requested fields.
    pub fn find(&self, query: &ProcessMessage) -> Option<&RegisteredProcess> {
        self.processes.iter().find(|p| p.matches(query))
    }

    /// Whether any registered process has the given pid.
    pub fn is_running(&self, pid: u16) -> bool {
        self.processes.iter().any(|p| p.process.pid == pid)
    }

    /// Record freshly collected cpu and mem stats for a process.
    ///
    /// The values are the 0-255 bytes computed from the raw counters. Returns
    /// false if the process is not registered.
    pub fn set_process_stats(&mut self, pid: u16, cpu: u8, mem: u8) -> bool {
        match self.processes.iter_mut().find(|p| p.process.pid == pid) {
            Some(entry) => {
                entry.cpu = cpu;
                entry.mem = mem;
                true
            }
            None => false,
        }
    }

    /// The most recently collected stats for a process, if registered.
    pub fn process_stats(&self, pid: u16) -> Option<(u8, u8)> {
        self.processes
            .iter()
            .find(|p| p.process.pid == pid)
            .map(|p| (p.cpu, p.mem))
    }

    /// The pids a signal command targets.
    ///
    /// Matches the reference `sendSignal`, which signals every registered
    /// process whose message matches the command's filter bits. The actual
    /// `kill` syscall is left to the caller.
    pub fn signal_targets(&self, sm: &SignalMessage) -> Vec<u16> {
        self.processes
            .iter()
            .filter(|entry| entry.process.matches(&sm.process))
            .map(|entry| entry.process.pid)
            .collect()
    }

    /// Apply a register or deregister command.
    ///
    /// Returns the confirmation process message the sender expects (the
    /// request itself, as the reference appends the same `pm` to the reply),
    /// or `None` for message types that are not registry commands — birth and
    /// death notifications are listener traffic (BW-05-01-03), and a message
    /// without `IS_MSGTYPE` is a plain discovery query.
    pub fn handle_command(&mut self, pm: &ProcessMessage) -> Option<ProcessMessage> {
        if pm.param & process_param::IS_MSGTYPE == 0 {
            return None;
        }
        match pm.param & !process_param::IS_MSGTYPE {
            process_type::REGISTER => {
                self.register(pm);
                Some(pm.clone())
            }
            process_type::DEREGISTER => {
                self.deregister(pm.pid);
                Some(pm.clone())
            }
            _ => None,
        }
    }
}

/// A birth or death notification addressed to one listener.
///
/// The reference builds this in `Listeners::handleNotify`: the listener's
/// `pre_addr`, the notifying process's `addr + port`, then the listener's
/// `post_addr`. The listener embeds its own port in the registration so the
/// payload slot is filled with the process that actually came up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// The listener's port the payload is sent to.
    pub port: u16,
    /// The listener's address the payload is sent to.
    pub addr: [u8; 4],
    /// `pre_addr` + notifying `addr` + notifying `port` + `post_addr`.
    pub data: Vec<u8>,
}

/// One registered birth or death listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    /// The registration message, whose process fields are the filters.
    pub lm: ListenerMessage,
    /// The address the listener registered from.
    pub addr: [u8; 4],
}

impl Listener {
    /// Whether this listener wants to hear about the given process.
    ///
    /// The reference matches on category, an optional uid (`ANY_UID` hears
    /// every user) and an optional name (an empty name hears every name).
    pub fn matches(&self, pm: &ProcessMessage) -> bool {
        let filter = &self.lm.process;
        filter.category == pm.category
            && (filter.uid == listener_type::ANY_UID || filter.uid == pm.uid)
            && (filter.name == pm.name || filter.name.is_empty())
    }
}

/// Registered birth and death listeners.
///
/// Mirrors the reference `Listeners`: `add` appends a member, and
/// `notify` walks the members in registration order, so every event reaches
/// subscribers in the order they subscribed and events are delivered in the
/// order machined observed them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Listeners {
    members: Vec<Listener>,
}

impl Listeners {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a listener. Order is preserved.
    pub fn add(&mut self, lm: &ListenerMessage, addr: [u8; 4]) {
        self.members.push(Listener {
            lm: lm.clone(),
            addr,
        });
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn members(&self) -> &[Listener] {
        &self.members
    }

    /// Deliver a birth or death notification to every matching listener, in
    /// registration order.
    pub fn notify(&self, pm: &ProcessMessage, addr: [u8; 4]) -> Vec<Notification> {
        self.members
            .iter()
            .filter(|listener| listener.matches(pm))
            .map(|listener| {
                let mut data = Vec::with_capacity(
                    listener.lm.pre_addr.len() + 6 + listener.lm.post_addr.len(),
                );
                data.extend_from_slice(listener.lm.pre_addr.as_bytes());
                data.extend_from_slice(&addr);
                data.extend_from_slice(&pm.port.to_le_bytes());
                data.extend_from_slice(listener.lm.post_addr.as_bytes());
                Notification {
                    port: listener.lm.process.port,
                    addr: listener.addr,
                    data,
                }
            })
            .collect()
    }

    /// Drop listeners whose process is no longer running, matching the
    /// reference `checkListeners`. Returns how many were dropped.
    pub fn drop_dead<F>(&mut self, mut is_running: F) -> usize
    where
        F: FnMut(u16) -> bool,
    {
        let before = self.members.len();
        self.members
            .retain(|listener| is_running(listener.lm.process.pid));
        before - self.members.len()
    }
}

/// The machined's two listener lists, one for births and one for deaths,
/// mirroring `birthListeners_` and `deathListeners_`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListenerRegistry {
    pub birth: Listeners,
    pub death: Listeners,
}

impl ListenerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.birth.len() + self.death.len()
    }

    pub fn is_empty(&self) -> bool {
        self.birth.is_empty() && self.death.is_empty()
    }

    /// Register a listener from a `ListenerMessage`.
    ///
    /// Returns whether the message type was recognised. The reference only
    /// accepts `ADD_BIRTH_LISTENER` or `ADD_DEATH_LISTENER` or'ed with
    /// `PARAM_IS_MSGTYPE`; anything else is logged and refused.
    pub fn add(&mut self, lm: &ListenerMessage, addr: [u8; 4]) -> bool {
        let param = lm.process.param;
        if param & process_param::IS_MSGTYPE == 0 {
            return false;
        }
        match param & !process_param::IS_MSGTYPE {
            listener_type::ADD_BIRTH_LISTENER => {
                self.birth.add(lm, addr);
                true
            }
            listener_type::ADD_DEATH_LISTENER => {
                self.death.add(lm, addr);
                true
            }
            _ => false,
        }
    }

    /// Deliver a birth notification to the birth listeners.
    pub fn notify_birth(&self, pm: &ProcessMessage, addr: [u8; 4]) -> Vec<Notification> {
        self.birth.notify(pm, addr)
    }

    /// Deliver a death notification to the death listeners.
    pub fn notify_death(&self, pm: &ProcessMessage, addr: [u8; 4]) -> Vec<Notification> {
        self.death.notify(pm, addr)
    }

    /// Drop listeners whose process died, in both lists.
    pub fn drop_dead<F>(&mut self, mut is_running: F) -> usize
    where
        F: FnMut(u16) -> bool,
    {
        self.birth.drop_dead(&mut is_running) + self.death.drop_dead(is_running)
    }
}

/// The machine and process state a Machined answers queries from. Built by
/// the discovery and statistics steps (BW-05-01-02/04); the query path only
/// needs to serialize it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachinedSnapshot {
    pub hostname: String,
    pub cpu_speed: u16,
    pub cpu_loads: Vec<u8>,
    pub io_wait_time: u8,
    pub mem: u8,
    pub interfaces: Vec<HighPrecisionInterfaceStats>,
    pub in_discards: u8,
    pub out_discards: u8,
    /// The machined version byte.
    pub version: u8,
    pub platform_info: String,
    pub tags: Vec<String>,
    /// The process registry; discovery queries read from it.
    pub registry: MachinedRegistry,
    pub user: UserMessage,
}

/// Answer a MachineGuard query with a compatible reply.
///
/// The reply keeps the request's sequence number, as the reference does
/// with `copySeq`, so the tool can correlate response to request.
pub fn answer_query(
    snapshot: &MachinedSnapshot,
    request: &MachineGuardMessage,
) -> MachineGuardMessage {
    let seq = request.header().seq;
    let mut reply = match request {
        MachineGuardMessage::WholeMachine(_) => {
            let n_interfaces = snapshot.interfaces.len().min(u8::MAX as usize) as u8;
            let n_cpus = snapshot.cpu_loads.len().min(u8::MAX as usize) as u8;
            MachineGuardMessage::WholeMachine(WholeMachineMessage {
                header: Header {
                    message: message_id::WHOLE_MACHINE,
                    flags: 0,
                    seq,
                },
                hostname: snapshot.hostname.clone(),
                cpu_speed: snapshot.cpu_speed,
                n_cpus,
                cpu_loads: snapshot.cpu_loads.clone(),
                mem: snapshot.mem,
                n_interfaces,
                if_stats: snapshot
                    .interfaces
                    .iter()
                    .map(|i| LowPrecisionInterfaceStats {
                        name: i.name.clone(),
                        bits_in: i.bits_in.min(u8::MAX as u32) as u8,
                        bits_out: i.bits_out.min(u8::MAX as u32) as u8,
                        pack_in: i.pack_in.min(u8::MAX as u32) as u8,
                        pack_out: i.pack_out.min(u8::MAX as u32) as u8,
                    })
                    .collect(),
                in_discards: snapshot.in_discards,
                out_discards: snapshot.out_discards,
                version: snapshot.version,
            })
        }
        MachineGuardMessage::HighPrecision(_) => {
            let n_interfaces = snapshot.interfaces.len().min(u8::MAX as usize) as u8;
            let n_cpus = snapshot.cpu_loads.len().min(u8::MAX as usize) as u8;
            MachineGuardMessage::HighPrecision(HighPrecisionMachineMessage {
                header: Header {
                    message: message_id::HIGH_PRECISION_MACHINE,
                    flags: 0,
                    seq,
                },
                hostname: snapshot.hostname.clone(),
                cpu_speed: snapshot.cpu_speed,
                n_cpus,
                cpu_loads: snapshot.cpu_loads.clone(),
                io_wait_time: snapshot.io_wait_time,
                mem: snapshot.mem,
                n_interfaces,
                if_stats: snapshot.interfaces.clone(),
                in_discards: snapshot.in_discards,
                out_discards: snapshot.out_discards,
                version: snapshot.version,
            })
        }
        MachineGuardMessage::QueryInterface(q) => {
            MachineGuardMessage::QueryInterface(QueryInterfaceMessage {
                header: Header {
                    message: message_id::QUERY_INTERFACE,
                    flags: 0,
                    seq,
                },
                address: q.address,
            })
        }
        MachineGuardMessage::MachinePlatform(_) => {
            MachineGuardMessage::MachinePlatform(MachinePlatformMessage {
                header: Header {
                    message: message_id::MACHINE_PLATFORM,
                    flags: 0,
                    seq,
                },
                platform_info: snapshot.platform_info.clone(),
            })
        }
        MachineGuardMessage::Tags(_) => MachineGuardMessage::Tags(TagsMessage {
            header: Header {
                message: message_id::TAGS,
                flags: 0,
                seq,
            },
            tags: snapshot.tags.clone(),
            exists: u8::from(!snapshot.tags.is_empty()),
        }),
        MachineGuardMessage::User(_) => {
            let mut user = snapshot.user.clone();
            user.header = Header {
                message: message_id::USER,
                flags: 0,
                seq,
            };
            MachineGuardMessage::User(user)
        }
        MachineGuardMessage::Pid(p) => {
            let running = snapshot.registry.is_running(p.pid) as u8;
            MachineGuardMessage::Pid(PidMessage {
                header: Header {
                    message: message_id::PID,
                    flags: 0,
                    seq,
                },
                pid: p.pid,
                running,
            })
        }
        MachineGuardMessage::Process(query) => {
            // A register/deregister command is not a discovery query; the
            // registry command path handles it. The reference's Process
            // handler switches on the msgtype and only the query case falls
            // through to the match-and-answer behaviour.
            if query.param & process_param::IS_MSGTYPE != 0 {
                return MachineGuardMessage::Unknown(UnknownMessage {
                    header: Header {
                        message: message_id::PROCESS,
                        flags: flags::MESSAGE_NOT_UNDERSTOOD,
                        seq,
                    },
                    data: Vec::new(),
                });
            }
            // Return the first process matching the query's requested fields.
            // The reference answers one process per reply for a targeted
            // query.
            let process = snapshot
                .registry
                .find(query)
                .map(|entry| entry.process.clone())
                .unwrap_or_else(ProcessMessage::default);
            let mut reply = process;
            reply.header = Header {
                message: message_id::PROCESS,
                flags: 0,
                seq,
            };
            MachineGuardMessage::Process(reply)
        }
        MachineGuardMessage::ProcessStats(query) => {
            let entry = snapshot.registry.find(&query.process);
            let mut process = match entry {
                Some(entry) => {
                    // The reference answers each matching process with the
                    // cpu and mem it last collected for it.
                    let mut process = entry.process.clone();
                    process.header.message = message_id::PROCESS_STATS;
                    process.header.seq = seq;
                    process
                }
                None => {
                    // The reference zeroes the pid so the client's recv loop
                    // can terminate on the miss.
                    let mut process = query.process.clone();
                    process.pid = 0;
                    process.header.message = message_id::PROCESS_STATS;
                    process.header.seq = seq;
                    process
                }
            };
            let (cpu, mem) = match entry {
                Some(entry) => (entry.cpu, entry.mem),
                None => (0, 0),
            };
            MachineGuardMessage::ProcessStats(ProcessStatsMessage { process, cpu, mem })
        }
        // Commands and notifications are not answered by this path.
        other => {
            return MachineGuardMessage::Unknown(UnknownMessage {
                header: Header {
                    message: other.header().message,
                    flags: flags::MESSAGE_NOT_UNDERSTOOD,
                    seq,
                },
                data: Vec::new(),
            });
        }
    };
    reply.set_seq(seq);
    reply
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

    /// A whole-machine reply, hand-encoded field by field to pin the wire
    /// layout to the reference `writeImpl` order.
    #[test]
    fn whole_machine_message_matches_reference_bytes() {
        let message = WholeMachineMessage {
            hostname: "test".to_string(),
            cpu_speed: 2000,
            n_cpus: 2,
            cpu_loads: vec![10, 20],
            mem: 50,
            n_interfaces: 1,
            if_stats: vec![LowPrecisionInterfaceStats {
                name: "eth0".to_string(),
                bits_in: 5,
                bits_out: 6,
                pack_in: 7,
                pack_out: 8,
            }],
            in_discards: 0,
            out_discards: 1,
            version: 1,
            ..WholeMachineMessage::default()
        };

        let mut stream = BinaryStream::new();
        message.write(&mut stream).expect("write");

        let expected: Vec<u8> = [
            // header: message, flags, seq
            message_id::WHOLE_MACHINE,
            0,
            0,
            0,
            // hostname
            4,
            b't',
            b'e',
            b's',
            b't',
            // cpu speed
            208,
            7,
            // n cpus, n interfaces, mem, version, discards
            2,
            1,
            50,
            1,
            0,
            1,
            // cpu loads
            10,
            20,
            // eth0 stats: name, bits in/out, packs in/out
            4,
            b'e',
            b't',
            b'h',
            b'0',
            5,
            6,
            7,
            8,
        ]
        .to_vec();
        assert_eq!(stream.as_bytes(), expected);

        stream.rewind();
        assert_eq!(
            WholeMachineMessage::read(&mut stream).expect("read"),
            message
        );
    }

    /// A high precision reply diverges from the low precision one only after
    /// the cpu loads: an io wait byte, then wide interface counters.
    #[test]
    fn high_precision_message_matches_reference_bytes() {
        let message = HighPrecisionMachineMessage {
            hostname: "test".to_string(),
            cpu_speed: 2000,
            n_cpus: 1,
            cpu_loads: vec![30],
            io_wait_time: 4,
            mem: 50,
            n_interfaces: 1,
            if_stats: vec![HighPrecisionInterfaceStats {
                name: "eth0".to_string(),
                bits_in: 0x0100,
                bits_out: 0x0200,
                pack_in: 3,
                pack_out: 4,
            }],
            in_discards: 0,
            out_discards: 0,
            version: 1,
            ..HighPrecisionMachineMessage::default()
        };

        let mut stream = BinaryStream::new();
        message.write(&mut stream).expect("write");

        let expected: Vec<u8> = [
            message_id::HIGH_PRECISION_MACHINE,
            0,
            0,
            0,
            4,
            b't',
            b'e',
            b's',
            b't',
            208,
            7,
            1,
            1,
            50,
            1,
            0,
            0,
            // cpu loads
            30,
            // io wait comes after the loads
            4,
            // eth0: name, then four u32 counters
            4,
            b'e',
            b't',
            b'h',
            b'0',
            0,
            1,
            0,
            0,
            0,
            2,
            0,
            0,
            3,
            0,
            0,
            0,
            4,
            0,
            0,
            0,
        ]
        .to_vec();
        assert_eq!(stream.as_bytes(), expected);

        stream.rewind();
        assert_eq!(
            HighPrecisionMachineMessage::read(&mut stream).expect("read"),
            message
        );
    }

    /// Process stats is the process body, then cpu/mem, then the extra block.
    #[test]
    fn process_stats_message_matches_reference_bytes() {
        let message = ProcessStatsMessage {
            process: ProcessMessage {
                header: Header {
                    message: message_id::PROCESS_STATS,
                    flags: 0,
                    seq: 0,
                },
                pid: 11,
                name: "baseapp01".to_string(),
                ..ProcessMessage::default()
            },
            cpu: 12,
            mem: 13,
        };

        let mut stream = BinaryStream::new();
        message.write(&mut stream).expect("write");

        let expected: Vec<u8> = [
            message_id::PROCESS_STATS,
            0,
            0,
            0,
            // process body: param, category, uid, pid, port, id
            0,
            0,
            0,
            0,
            11,
            0,
            0,
            0,
            0,
            0,
            // name
            9,
            b'b',
            b'a',
            b's',
            b'e',
            b'a',
            b'p',
            b'p',
            b'0',
            b'1',
            // cpu, mem come before the extra block
            12,
            13,
            // extra: declared tail 10 (i32), four u16 versions, two empty
            // strings
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

        stream.rewind();
        assert_eq!(
            ProcessStatsMessage::read(&mut stream).expect("read"),
            message
        );
    }

    /// The remaining simple query bodies in one pass.
    #[test]
    fn simple_query_messages_match_reference_bytes() {
        // QueryInterface: header + address.
        let query = QueryInterfaceMessage {
            address: 0x7f000001,
            ..QueryInterfaceMessage::default()
        };
        let mut stream = BinaryStream::new();
        query.write(&mut stream).expect("write");
        assert_eq!(
            stream.as_bytes(),
            [message_id::QUERY_INTERFACE, 0, 0, 0, 1, 0, 0, 127].to_vec()
        );
        stream.rewind();
        assert_eq!(
            QueryInterfaceMessage::read(&mut stream).expect("read"),
            query
        );

        // MachinePlatform: header + platform string.
        let platform = MachinePlatformMessage {
            platform_info: "Linux el7".to_string(),
            ..MachinePlatformMessage::default()
        };
        let mut stream = BinaryStream::new();
        platform.write(&mut stream).expect("write");
        let mut expected = vec![message_id::MACHINE_PLATFORM, 0, 0, 0, 9];
        expected.extend_from_slice(b"Linux el7");
        assert_eq!(stream.as_bytes(), expected);
        stream.rewind();
        assert_eq!(
            MachinePlatformMessage::read(&mut stream).expect("read"),
            platform
        );

        // Pid: header + pid + running.
        let pid = PidMessage {
            pid: 4242,
            running: 1,
            ..PidMessage::default()
        };
        let mut stream = BinaryStream::new();
        pid.write(&mut stream).expect("write");
        assert_eq!(
            stream.as_bytes(),
            [message_id::PID, 0, 0, 0, 0x92, 0x10, 1].to_vec()
        );
        stream.rewind();
        assert_eq!(PidMessage::read(&mut stream).expect("read"), pid);

        // Tags: header + count + exists + strings.
        let tags = TagsMessage {
            tags: vec!["dev".to_string()],
            exists: 1,
            ..TagsMessage::default()
        };
        let mut stream = BinaryStream::new();
        tags.write(&mut stream).expect("write");
        assert_eq!(
            stream.as_bytes(),
            [message_id::TAGS, 0, 0, 0, 1, 1, 3, b'd', b'e', b'v'].to_vec()
        );
        stream.rewind();
        assert_eq!(TagsMessage::read(&mut stream).expect("read"), tags);
    }

    /// User writes coredumps only when asked, and the version string only
    /// when requested, exactly like the reference's conditional writes.
    #[test]
    fn user_message_conditional_fields_round_trip() {
        let base = UserMessage {
            param: 0,
            uid: 1000,
            username: "cui".to_string(),
            fullname: "Cui Hairu".to_string(),
            home: "/home/cui".to_string(),
            mfroot: "/opt/bw".to_string(),
            bwrespath: "/opt/bw/res".to_string(),
            coredumps: vec![CoreDump {
                filename: "core.1".to_string(),
                assert: "assert.txt".to_string(),
                time: 1234,
            }],
            version_string: "1.2.3".to_string(),
            ..UserMessage::default()
        };

        // Without the check bits neither tail field goes on the wire.
        let mut stream = BinaryStream::new();
        base.write(&mut stream).expect("write");
        let read = UserMessage::read(&mut stream).expect("read");
        assert!(read.coredumps.is_empty());
        assert_eq!(read.version_string, "");

        // With both bits everything round trips.
        let with_all = UserMessage {
            param: user_param::CHECK_COREDUMPS | user_param::GET_VERSION,
            ..base.clone()
        };
        let mut stream = BinaryStream::new();
        with_all.write(&mut stream).expect("write");
        assert_eq!(UserMessage::read(&mut stream).expect("read"), with_all);

        // A coredump entry is two strings plus a u32 time.
        let with_dumps = UserMessage {
            param: user_param::CHECK_COREDUMPS,
            ..base.clone()
        };
        let mut stream = BinaryStream::new();
        with_dumps.write(&mut stream).expect("write");
        let bytes = stream.as_bytes();
        // 6 strings + count u32 = 4 + 2*2 + 10 + 10 + 10 + 9 + 12 + 4 = 53
        // header 4, param 1, uid 2 = 7 before the six strings and the vector.
        // header 4 + param 1 + uid 2 + five strings + count u32 + one
        // coredump (two strings + u32).
        let expected_len = 4 + 1 + 2 + 4 + 10 + 10 + 8 + 12 + 4 + 7 + 11 + 4;
        assert_eq!(bytes.len(), expected_len);
    }

    /// Create and announce bodies round trip.
    #[test]
    fn create_and_announce_round_trip() {
        let create = CreateMessage {
            name: "cellapp".to_string(),
            config: "Hybrid".to_string(),
            uid: 1000,
            recover: 1,
            fwd_ip: 0x7f000001,
            fwd_port: 30000,
            ..CreateMessage::default()
        };
        let mut stream = BinaryStream::new();
        create.write(&mut stream).expect("write");
        assert_eq!(CreateMessage::read(&mut stream).expect("read"), create);

        let with_args = CreateWithArgsMessage {
            create: CreateMessage {
                name: "baseapp".to_string(),
                ..CreateMessage::default()
            },
            args: vec!["--recover".to_string(), "1".to_string()],
        };
        let mut stream = BinaryStream::new();
        with_args.write(&mut stream).expect("write");
        assert_eq!(
            CreateWithArgsMessage::read(&mut stream).expect("read"),
            with_args
        );

        let announce = MachinedAnnounceMessage {
            announce_type: machined_announce_type::EXISTS,
            count: 0x7f000001,
            ..MachinedAnnounceMessage::default()
        };
        let mut stream = BinaryStream::new();
        announce.write(&mut stream).expect("write");
        assert_eq!(
            MachinedAnnounceMessage::read(&mut stream).expect("read"),
            announce
        );

        // Reset and error bodies.
        let reset = ResetMessage::default();
        let mut stream = BinaryStream::new();
        reset.write(&mut stream).expect("write");
        assert_eq!(stream.as_bytes(), [message_id::RESET, 0, 0, 0].to_vec());
        stream.rewind();
        assert_eq!(ResetMessage::read(&mut stream).expect("read"), reset);

        let error = ErrorMessage {
            severity: 3,
            msg: "boom".to_string(),
            ..ErrorMessage::default()
        };
        let mut stream = BinaryStream::new();
        error.write(&mut stream).expect("write");
        let mut expected = vec![message_id::ERROR, 0, 0, 0, 3, 4];
        expected.extend_from_slice(b"boom");
        assert_eq!(stream.as_bytes(), expected);
        stream.rewind();
        assert_eq!(ErrorMessage::read(&mut stream).expect("read"), error);
    }

    /// A packet carrying one whole-machine query round trips through the
    /// length-prefixed framing and dispatches back to the same variant.
    #[test]
    fn mgm_packet_round_trips_and_dispatches() {
        let query = MachineGuardMessage::WholeMachine(WholeMachineMessage {
            hostname: "h".to_string(),
            ..WholeMachineMessage::default()
        });
        let packet = MgmPacket::new(vec![query.clone()]);

        let bytes = packet.to_bytes().expect("write");
        let read = MgmPacket::from_bytes(&bytes).expect("read");
        assert_eq!(read, packet);
        assert_eq!(read.messages, vec![query]);
        assert!(!read.should_stagger_reply());

        // Two messages dispatch independently.
        let packet = MgmPacket::new(vec![
            MachineGuardMessage::Reset(ResetMessage::default()),
            MachineGuardMessage::Pid(PidMessage {
                pid: 1,
                ..PidMessage::default()
            }),
        ]);
        let bytes = packet.to_bytes().expect("write");
        assert_eq!(MgmPacket::from_bytes(&bytes).expect("read"), packet);

        // The stagger flag survives the round trip.
        let mut packet = MgmPacket::new(vec![MachineGuardMessage::Reset(ResetMessage::default())]);
        packet.flags = mgm_packet_flags::PACKET_STAGGER_REPLIES;
        let bytes = packet.to_bytes().expect("write");
        assert!(
            MgmPacket::from_bytes(&bytes)
                .expect("read")
                .should_stagger_reply()
        );
    }

    /// An unrecognised message id echoes back with the not-understood flag.
    #[test]
    fn dispatch_unknown_message_sets_flag() {
        // A body with a message id nothing handles, followed by junk.
        let mut stream = BinaryStream::new();
        stream.write_u8(0x77);
        stream.write_u8(0);
        stream.write_u16(9);
        stream.write_bytes(&[1, 2, 3]);

        let message = MachineGuardMessage::create(&mut stream).expect("create");
        match message {
            MachineGuardMessage::Unknown(unknown) => {
                assert_eq!(unknown.header.message, 0x77);
                assert_eq!(unknown.header.seq, 9);
                assert_ne!(unknown.header.flags & flags::MESSAGE_NOT_UNDERSTOOD, 0);
                assert_eq!(unknown.data, vec![1, 2, 3]);
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    /// A malformed typed message also falls back to the raw echo.
    #[test]
    fn dispatch_malformed_message_falls_back_to_unknown() {
        // A tags message declaring two tags but carrying none.
        let mut stream = BinaryStream::new();
        stream.write_u8(message_id::TAGS);
        stream.write_u8(0);
        stream.write_u16(0);
        stream.write_u8(2);
        stream.write_u8(1);

        let message = MachineGuardMessage::create(&mut stream).expect("create");
        match message {
            MachineGuardMessage::Unknown(unknown) => {
                assert_eq!(unknown.header.message, message_id::TAGS);
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    /// The query path: a whole-machine request over a fixed snapshot
    /// produces the reference-compatible reply bytes, and the same request
    /// always produces the same payload.
    #[test]
    fn answer_query_returns_compatible_payload() {
        let snapshot = MachinedSnapshot {
            hostname: "test".to_string(),
            cpu_speed: 2000,
            cpu_loads: vec![10, 20],
            io_wait_time: 4,
            mem: 50,
            interfaces: vec![HighPrecisionInterfaceStats {
                name: "eth0".to_string(),
                bits_in: 5,
                bits_out: 6,
                pack_in: 7,
                pack_out: 8,
            }],
            in_discards: 0,
            out_discards: 1,
            version: 1,
            platform_info: "Linux el7".to_string(),
            tags: vec!["dev".to_string()],
            registry: MachinedRegistry::from_processes(vec![ProcessMessage {
                pid: 11,
                name: "baseapp01".to_string(),
                ..ProcessMessage::default()
            }]),
            user: UserMessage::default(),
        };

        let request = MachineGuardMessage::WholeMachine(WholeMachineMessage {
            header: Header {
                message: message_id::WHOLE_MACHINE,
                flags: 0,
                seq: 77,
            },
            ..WholeMachineMessage::default()
        });

        let reply = answer_query(&snapshot, &request);
        let bytes = reply.to_bytes().expect("write");
        assert_eq!(
            bytes,
            [
                message_id::WHOLE_MACHINE,
                0,
                77,
                0,
                4,
                b't',
                b'e',
                b's',
                b't',
                208,
                7,
                2,
                1,
                50,
                1,
                0,
                1,
                10,
                20,
                4,
                b'e',
                b't',
                b'h',
                b'0',
                5,
                6,
                7,
                8,
            ]
            .to_vec()
        );

        // The same request gives the same payload.
        let again = answer_query(&snapshot, &request).to_bytes().expect("write");
        assert_eq!(bytes, again);

        // Pid answers from the process registry.
        let hit = answer_query(
            &snapshot,
            &MachineGuardMessage::Pid(PidMessage {
                pid: 11,
                ..PidMessage::default()
            }),
        )
        .to_bytes()
        .expect("write");
        assert_eq!(hit, [message_id::PID, 0, 0, 0, 11, 0, 1].to_vec());

        let miss = answer_query(
            &snapshot,
            &MachineGuardMessage::Pid(PidMessage {
                pid: 12,
                ..PidMessage::default()
            }),
        )
        .to_bytes()
        .expect("write");
        assert_eq!(miss, [message_id::PID, 0, 0, 0, 12, 0, 0].to_vec());

        // Commands are not answered by the query path.
        let command = answer_query(
            &snapshot,
            &MachineGuardMessage::Reset(ResetMessage::default()),
        );
        match command {
            MachineGuardMessage::Unknown(unknown) => {
                assert_ne!(unknown.header.flags & flags::MESSAGE_NOT_UNDERSTOOD, 0);
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    fn base_snapshot() -> MachinedSnapshot {
        MachinedSnapshot {
            hostname: "test".to_string(),
            cpu_speed: 2000,
            cpu_loads: vec![10],
            io_wait_time: 0,
            mem: 50,
            interfaces: Vec::new(),
            in_discards: 0,
            out_discards: 0,
            version: 1,
            platform_info: "Linux".to_string(),
            tags: Vec::new(),
            registry: MachinedRegistry::new(),
            user: UserMessage::default(),
        }
    }

    /// A register command for a `cellapp01` process on port 50001.
    fn register_command(pid: u16, port: u16, name: &str) -> ProcessMessage {
        ProcessMessage {
            pid,
            port,
            id: 1,
            name: name.to_string(),
            param: process_param::IS_MSGTYPE | process_type::REGISTER,
            ..ProcessMessage::default()
        }
    }

    /// A discovery query for a process by pid.
    fn pid_query(pid: u16) -> MachineGuardMessage {
        MachineGuardMessage::Pid(PidMessage {
            pid,
            ..PidMessage::default()
        })
    }

    /// A discovery query for a process by name.
    fn name_query(name: &str) -> MachineGuardMessage {
        MachineGuardMessage::Process(ProcessMessage {
            param: process_param::USE_NAME,
            name: name.to_string(),
            ..ProcessMessage::default()
        })
    }

    /// Registering a process makes an immediately-following discovery query
    /// report it as running; deregistering makes it disappear again.
    #[test]
    fn register_and_deregister_reflect_immediately_in_discovery() {
        let mut snapshot = base_snapshot();
        let cmd = register_command(101, 50001, "cellapp01");

        let confirmation = snapshot.registry.handle_command(&cmd).expect("confirm");
        assert_eq!(confirmation.pid, 101);
        assert_eq!(confirmation.param, cmd.param);

        let running = answer_query(&snapshot, &pid_query(101));
        let MachineGuardMessage::Pid(pid) = running else {
            panic!("expected pid reply");
        };
        assert_eq!(pid.running, 1);

        let found = answer_query(&snapshot, &name_query("cellapp01"));
        let MachineGuardMessage::Process(found) = found else {
            panic!("expected process reply");
        };
        assert_eq!(found.pid, 101);

        // The same query before registration would have missed; the registry
        // also reports the process count.
        assert_eq!(snapshot.registry.len(), 1);

        let dereg = ProcessMessage {
            pid: 101,
            param: process_param::IS_MSGTYPE | process_type::DEREGISTER,
            ..ProcessMessage::default()
        };
        snapshot.registry.handle_command(&dereg).expect("confirm");

        let gone = answer_query(&snapshot, &pid_query(101));
        let MachineGuardMessage::Pid(pid) = gone else {
            panic!("expected pid reply");
        };
        assert_eq!(pid.running, 0);
        assert!(snapshot.registry.is_empty());
    }

    /// A discovery query for a never-registered pid reports not-running.
    #[test]
    fn discovery_misses_absent_processes() {
        let snapshot = base_snapshot();
        let running = answer_query(&snapshot, &pid_query(999));
        let MachineGuardMessage::Pid(pid) = running else {
            panic!("expected pid reply");
        };
        assert_eq!(pid.running, 0);
    }

    /// Re-registering the same pid/category/name reuses the slot instead of
    /// appending a duplicate.
    #[test]
    fn register_reuses_slot_for_same_pid_category_name() {
        let mut registry = MachinedRegistry::new();
        let first = register_command(7, 100, "baseapp01");
        assert_eq!(registry.register(&first), RegisterOutcome::Registered);

        let mut second = first.clone();
        second.port = 200;
        assert_eq!(registry.register(&second), RegisterOutcome::ReRegistered);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.processes()[0].process.port, 200);
    }

    /// A new process claiming an already-used port displaces the old owner.
    #[test]
    fn register_displaces_port_clash() {
        let mut registry = MachinedRegistry::new();
        let old = register_command(1, 6000, "baseapp01");
        registry.register(&old);
        let new = register_command(2, 6000, "cellapp01");
        registry.register(&new);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.processes()[0].process.pid, 2);
        assert!(!registry.is_running(1));
    }

    /// Deregistering an unknown pid is reported as not found and leaves the
    /// registry unchanged.
    #[test]
    fn deregister_unknown_pid_reports_not_found() {
        let mut registry = MachinedRegistry::new();
        registry.register(&register_command(1, 6000, "baseapp01"));
        assert_eq!(registry.deregister(999), DeregisterOutcome::NotFound);
        assert_eq!(registry.len(), 1);
    }

    /// `handle_command` confirms register and deregister with the same
    /// message, and returns nothing for listener notifications.
    #[test]
    fn handle_command_confirms_commands_only() {
        let mut registry = MachinedRegistry::new();
        let register = register_command(3, 7000, "dbapp01");
        let confirmed = registry.handle_command(&register).expect("confirm");
        let confirmed_bytes = MachineGuardMessage::Process(confirmed)
            .to_bytes()
            .expect("write");
        let register_bytes = MachineGuardMessage::Process(register)
            .to_bytes()
            .expect("write");
        assert_eq!(confirmed_bytes, register_bytes);

        let birth = ProcessMessage {
            param: process_param::IS_MSGTYPE | process_type::NOTIFY_BIRTH,
            ..ProcessMessage::default()
        };
        assert!(registry.handle_command(&birth).is_none());

        let plain_query = ProcessMessage {
            param: process_param::USE_PID,
            ..ProcessMessage::default()
        };
        assert!(registry.handle_command(&plain_query).is_none());
    }

    /// A register/deregister command arriving at the query path is not
    /// treated as a discovery query; it is rejected as not understood.
    #[test]
    fn query_path_rejects_register_command() {
        let snapshot = base_snapshot();
        let cmd = register_command(4, 8000, "cellapp01");
        let reply = answer_query(&snapshot, &MachineGuardMessage::Process(cmd));
        let MachineGuardMessage::Unknown(unknown) = reply else {
            panic!("expected unknown reply");
        };
        assert_ne!(unknown.header.flags & flags::MESSAGE_NOT_UNDERSTOOD, 0);
    }

    /// A birth listener registration for the given filter and endpoint.
    fn birth_listener(
        port: u16,
        addr: [u8; 4],
        pre: &str,
        post: &str,
        uid: u16,
        name: &str,
    ) -> (ListenerMessage, [u8; 4]) {
        let mut process = ProcessMessage::default();
        process.header.message = message_id::LISTENER;
        process.param = process_param::IS_MSGTYPE | listener_type::ADD_BIRTH_LISTENER;
        process.port = port;
        process.uid = uid;
        process.name = name.to_string();
        (
            ListenerMessage {
                process,
                pre_addr: pre.to_string(),
                post_addr: post.to_string(),
            },
            addr,
        )
    }

    /// A death listener registration for the given filter.
    fn death_listener(port: u16, name: &str) -> ListenerMessage {
        let mut process = ProcessMessage::default();
        process.header.message = message_id::LISTENER;
        process.param = process_param::IS_MSGTYPE | listener_type::ADD_DEATH_LISTENER;
        process.port = port;
        process.name = name.to_string();
        ListenerMessage {
            process,
            pre_addr: String::new(),
            post_addr: String::new(),
        }
    }

    /// A process event as `Listeners::notify` sees it, with a sender address.
    fn event(pid: u16, port: u16, name: &str, addr: [u8; 4]) -> (ProcessMessage, [u8; 4]) {
        let mut pm = ProcessMessage {
            pid,
            port,
            name: name.to_string(),
            ..ProcessMessage::default()
        };
        pm.header.message = message_id::PROCESS;
        (pm, addr)
    }

    /// A single event reaches every matching subscriber in the order they
    /// subscribed.
    #[test]
    fn notification_reaches_subscribers_in_registration_order() {
        let mut registry = ListenerRegistry::new();
        let (a, a_addr) = birth_listener(3001, [10, 0, 0, 1], "preA", "postA", 500, "");
        let (b, b_addr) = birth_listener(3002, [10, 0, 0, 2], "preB", "postB", 500, "");
        assert!(registry.add(&a, a_addr));
        assert!(registry.add(&b, b_addr));

        // The event belongs to the same uid the listeners filter on.
        let (mut pm, pm_addr) = event(1, 4001, "cellapp01", [10, 0, 0, 9]);
        pm.uid = 500;
        let notifications = registry.notify_birth(&pm, pm_addr);

        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0].port, 3001);
        assert_eq!(notifications[0].addr, a_addr);
        assert_eq!(notifications[1].port, 3002);
        assert_eq!(notifications[1].addr, b_addr);

        // Payload is pre + notifying addr + notifying port + post.
        let mut expected = Vec::new();
        expected.extend_from_slice(b"preA");
        expected.extend_from_slice(&pm_addr);
        expected.extend_from_slice(&pm.port.to_le_bytes());
        expected.extend_from_slice(b"postA");
        assert_eq!(notifications[0].data, expected);
    }

    /// Two events are delivered in the order they occurred.
    #[test]
    fn notification_keeps_event_order() {
        let mut registry = ListenerRegistry::new();
        let (lm, addr) = birth_listener(3001, [10, 0, 0, 1], "", "", 0, "");
        registry.add(&lm, addr);

        let (first, first_addr) = event(1, 4001, "cellapp01", [10, 0, 0, 9]);
        let (second, second_addr) = event(2, 4002, "cellapp02", [10, 0, 0, 9]);
        let all = registry.notify_birth(&first, first_addr);
        let mut all = all;
        all.extend(registry.notify_birth(&second, second_addr));

        assert_eq!(all.len(), 2);
        assert!(all[0].data.ends_with(&first.port.to_le_bytes()));
        assert!(all[1].data.ends_with(&second.port.to_le_bytes()));
    }

    /// Birth and death notifications reach their own listener lists.
    #[test]
    fn birth_and_death_listeners_are_separate() {
        let mut registry = ListenerRegistry::new();
        let (birth, birth_addr) = birth_listener(3001, [10, 0, 0, 1], "", "", 0, "");
        let death = death_listener(3002, "");
        assert!(registry.add(&birth, birth_addr));
        assert!(registry.add(&death, birth_addr));

        let (pm, pm_addr) = event(1, 4001, "cellapp01", [10, 0, 0, 9]);
        assert_eq!(registry.notify_birth(&pm, pm_addr).len(), 1);
        assert_eq!(registry.notify_death(&pm, pm_addr).len(), 1);
        assert_eq!(registry.birth.len(), 1);
        assert_eq!(registry.death.len(), 1);
    }

    /// A listener with a name or uid filter only hears matching processes.
    #[test]
    fn listener_filters_narrow_notifications() {
        let mut registry = ListenerRegistry::new();
        let (cell, cell_addr) = birth_listener(3001, [10, 0, 0, 1], "", "", 0, "cellapp01");
        let (uid7, uid7_addr) = birth_listener(3002, [10, 0, 0, 2], "", "", 7, "");
        registry.add(&cell, cell_addr);
        registry.add(&uid7, uid7_addr);

        // A baseapp01 for uid 7: the cell listener filters it out, the uid
        // listener hears it.
        let (mut pm, pm_addr) = event(1, 4001, "baseapp01", [10, 0, 0, 9]);
        pm.uid = 7;
        let hits = registry.notify_birth(&pm, pm_addr);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].port, 3002);
    }

    /// `ANY_UID` hears every user.
    #[test]
    fn any_uid_listener_hears_every_user() {
        let mut registry = ListenerRegistry::new();
        let (any, any_addr) =
            birth_listener(3001, [10, 0, 0, 1], "", "", listener_type::ANY_UID, "");
        registry.add(&any, any_addr);

        let (mut pm, pm_addr) = event(1, 4001, "cellapp01", [10, 0, 0, 9]);
        pm.uid = 42;
        assert_eq!(registry.notify_birth(&pm, pm_addr).len(), 1);
    }

    /// An unrecognised listener param is refused.
    #[test]
    fn unrecognised_listener_param_is_refused() {
        let mut registry = ListenerRegistry::new();
        let (lm, addr) = birth_listener(3001, [10, 0, 0, 1], "", "", 0, "");
        let mut bad = lm.clone();
        bad.process.param = process_param::IS_MSGTYPE | 7; // not a listener type
        assert!(!registry.add(&bad, addr));
        assert!(registry.is_empty());
    }

    /// A dead listener is dropped on the next check.
    #[test]
    fn drop_dead_removes_stale_listeners() {
        let mut registry = ListenerRegistry::new();
        let (a, a_addr) = birth_listener(3001, [10, 0, 0, 1], "", "", 0, "");
        let (b, b_addr) = birth_listener(3002, [10, 0, 0, 2], "", "", 0, "");
        let mut a = a;
        let mut b = b;
        a.process.pid = 10;
        b.process.pid = 20;
        registry.add(&a, a_addr);
        registry.add(&b, b_addr);

        let dropped = registry.drop_dead(|pid| pid == 10);
        assert_eq!(dropped, 1);
        assert_eq!(registry.birth.len(), 1);
        assert_eq!(registry.birth.members()[0].lm.process.pid, 10);
    }

    /// The metric bytes are the reference's `value * 0xff / max` scaling.
    #[test]
    fn stat_formulas_match_reference_scaling() {
        // 25 of 100 busy jiffies is 25% of the 0-255 scale.
        assert_eq!(cpu_load(25, 100), 63);
        // A quarter of memory used.
        assert_eq!(memory_usage(1, 4), 63);
        // Half of total jiffies in IO wait.
        assert_eq!(io_wait(50, 100), 127);
        // A quarter of cpu used.
        assert_eq!(cpu_load(1, 4), 63);

        // The reference clamps the divisor to at least one, so an idle
        // sample still divides rather than trapping.
        assert_eq!(cpu_load(1, 0), 255);
        assert_eq!(io_wait(1, 0), 255);
        // Zero memory total has no scale to report.
        assert_eq!(memory_usage(100, 0), 0);
    }

    /// A process stats query carries the cpu and mem collected for the
    /// matching process.
    #[test]
    fn process_stats_query_carries_collected_cpu_and_mem() {
        let mut snapshot = base_snapshot();
        let cmd = register_command(21, 50001, "cellapp01");
        snapshot.registry.handle_command(&cmd);

        assert!(snapshot.registry.set_process_stats(21, 90, 40));
        assert_eq!(snapshot.registry.process_stats(21), Some((90, 40)));

        let query = MachineGuardMessage::ProcessStats(ProcessStatsMessage {
            process: ProcessMessage {
                param: process_param::USE_PID,
                pid: 21,
                ..ProcessMessage::default()
            },
            ..ProcessStatsMessage::default()
        });
        let reply = answer_query(&snapshot, &query);
        let MachineGuardMessage::ProcessStats(reply) = reply else {
            panic!("expected process stats reply");
        };
        assert_eq!(reply.cpu, 90);
        assert_eq!(reply.mem, 40);
        assert_eq!(reply.process.pid, 21);
    }

    /// A missing process stats query reports the reference's pid 0 sentinel
    /// with no load.
    #[test]
    fn missing_process_stats_reports_pid_zero() {
        let snapshot = base_snapshot();
        let query = MachineGuardMessage::ProcessStats(ProcessStatsMessage {
            process: ProcessMessage {
                param: process_param::USE_PID,
                pid: 77,
                ..ProcessMessage::default()
            },
            ..ProcessStatsMessage::default()
        });
        let reply = answer_query(&snapshot, &query);
        let MachineGuardMessage::ProcessStats(reply) = reply else {
            panic!("expected process stats reply");
        };
        assert_eq!(reply.process.pid, 0);
        assert_eq!(reply.cpu, 0);
        assert_eq!(reply.mem, 0);
    }

    /// Stats for an unregistered pid are refused, and collecting for a new
    /// pid resets the previous sample.
    #[test]
    fn process_stats_track_only_registered_pids() {
        let mut registry = MachinedRegistry::new();
        assert!(!registry.set_process_stats(9, 10, 20));
        assert_eq!(registry.process_stats(9), None);

        registry.register(&register_command(9, 5000, "baseapp01"));
        assert!(registry.set_process_stats(9, 10, 20));
        assert_eq!(registry.process_stats(9), Some((10, 20)));

        let mut re = register_command(9, 5000, "baseapp01");
        re.pid = 12;
        registry.deregister(9);
        registry.register(&re);
        assert_eq!(registry.process_stats(12), Some((0, 0)));
    }

    /// A create command for a valid executable, the two legal configs, and
    /// the three illegal shapes.
    fn create_message(name: &str, config: &str) -> CreateMessage {
        CreateMessage {
            name: name.to_string(),
            config: config.to_string(),
            ..CreateMessage::default()
        }
    }

    /// Create validation refuses the illegal shapes the reference refuses,
    /// in the reference's order.
    #[test]
    fn create_validation_matches_reference_exit_states() {
        // The two legal configs, case-insensitively as `bw_stricmp` compares.
        assert_eq!(validate_create(&create_message("cellapp", "hybrid")), None);
        assert_eq!(validate_create(&create_message("cellapp", "debug")), None);
        assert_eq!(validate_create(&create_message("cellapp", "Hybrid")), None);

        // An unknown config is the first refusal.
        assert_eq!(
            validate_create(&create_message("cellapp", "release")),
            Some(CreateRejectReason::InvalidConfig)
        );
        // Config is checked before the path escape.
        assert_eq!(
            validate_create(&create_message("../escape", "release")),
            Some(CreateRejectReason::InvalidConfig)
        );
        // A `..` in either field refuses.
        assert_eq!(
            validate_create(&create_message("../escape", "hybrid")),
            Some(CreateRejectReason::IllegalPath)
        );
        // The reference checks the config before the path, and a legal config
        // can never contain `..`, so a `..`-bearing config is refused as an
        // invalid config. The path branch is defensive there.
        assert_eq!(
            validate_create(&create_message("cellapp", "hybrid/../x")),
            Some(CreateRejectReason::InvalidConfig)
        );
        // The setuid helper directory is denied.
        assert_eq!(
            validate_create(&create_message("commands/_helpers/snapshot", "hybrid")),
            Some(CreateRejectReason::HelperProcess)
        );
    }

    /// A signal command targets exactly the registered processes its filter
    /// selects; an unregistered pid targets nothing.
    #[test]
    fn signal_targets_registered_processes() {
        let mut registry = MachinedRegistry::new();
        registry.register(&register_command(31, 5100, "baseapp01"));
        registry.register(&register_command(32, 5200, "cellapp01"));

        let mut by_pid = SignalMessage {
            ..SignalMessage::default()
        };
        by_pid.process.param = process_param::USE_PID;
        by_pid.process.pid = 31;
        assert_eq!(registry.signal_targets(&by_pid), vec![31]);

        // No filter bits matches every registered process.
        let broadcast = SignalMessage::default();
        assert_eq!(registry.signal_targets(&broadcast), vec![31, 32]);

        // A pid nothing registered under targets nothing.
        let mut unknown = SignalMessage::default();
        unknown.process.param = process_param::USE_PID;
        unknown.process.pid = 99;
        assert!(registry.signal_targets(&unknown).is_empty());
    }

    /// The lifecycle helpers carry the reference's signal numbers.
    #[test]
    fn lifecycle_signals_carry_reference_numbers() {
        let mut shutdown = SignalMessage::default();
        shutdown.set_controlled_shutdown();
        assert_eq!(shutdown.signal, signal::SIGUSR1);

        let mut kill = SignalMessage::default();
        kill.set_kill();
        assert_eq!(kill.signal, signal::SIGINT);

        let mut hard_kill = SignalMessage::default();
        hard_kill.set_hard_kill();
        assert_eq!(hard_kill.signal, signal::SIGQUIT);

        // A controlled shutdown of one registered process resolves to that
        // pid and the SIGUSR1 number.
        let mut registry = MachinedRegistry::new();
        registry.register(&register_command(41, 5300, "baseapp01"));
        let mut command = SignalMessage::default();
        command.set_controlled_shutdown();
        command.process.param = process_param::USE_PID;
        command.process.pid = 41;
        assert_eq!(registry.signal_targets(&command), vec![41]);
        assert_eq!(command.signal, signal::SIGUSR1);
    }
}
