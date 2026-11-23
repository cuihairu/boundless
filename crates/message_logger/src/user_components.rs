//! `LoggingComponent` + `UserComponents` (`logging_component.cpp`,
//! `user_components.cpp`, `log_entry_address.cpp`): the per-user components
//! file. One record per registered component that has logged, appended when
//! the component's first entry lands (`UserSegmentWriter::addEntry`).
//!
//! Record layout (`LoggingComponent::write`): the address (`ip`/`port`/
//! `salt`), the on-disk `userComponentID`, the `appInstanceID`, the
//! `appTypeID`, then `LoggerComponentMessage::write` and the
//! `LogEntryAddress` of the component's first entry.

use std::collections::BTreeMap;

use crate::component_message::ComponentMessage;
use crate::registry::Address;
use crate::stream::{MemoryIStream, MemoryOStream};

/// `LogEntryAddress` (`log_entry_address.hpp`): the on-disk location of a
/// log entry — a segment suffix plus the entry index within it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LogEntryAddress {
    /// The segment suffix, e.g. `2026-11-23-21:11:58`.
    pub suffix: String,
    /// The entry index within the segment.
    pub index: i32,
}

impl LogEntryAddress {
    /// `LogEntryAddress::write`: the packed suffix then the index.
    pub fn write_to(&self, os: &mut MemoryOStream) {
        os.write_string(&self.suffix);
        os.write_u32(self.index as u32);
    }

    /// `LogEntryAddress::read`.
    pub fn read_from(is: &mut MemoryIStream) -> Option<Self> {
        let suffix = is.read_string();
        let index = is.read_u32()? as i32;
        Some(Self { suffix, index })
    }
}

/// One components-file record (`LoggingComponent`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LoggingComponent {
    /// The source address of the component.
    pub address: Address,
    /// The on-disk unique ID assigned at registration.
    pub user_component_id: u32,
    /// The server-wide instance ID (e.g. cellapp01), 0 until announced.
    pub app_instance_id: i32,
    /// The process-type ID — the line index in the root `component_names`.
    pub app_type_id: u32,
    /// The registration message fields (version, uid, pid, name…).
    pub message: ComponentMessage,
    /// Where the component's first entry landed.
    pub first_entry: LogEntryAddress,
}

impl LoggingComponent {
    /// The constructor the writer path uses
    /// (`LoggingComponent( UserComponents *, addr, msg, appTypeID )`).
    pub fn new(
        address: Address,
        message: ComponentMessage,
        app_type_id: u32,
        user_component_id: u32,
    ) -> Self {
        Self {
            address,
            user_component_id,
            app_instance_id: 0,
            app_type_id,
            message,
            first_entry: LogEntryAddress::default(),
        }
    }

    /// `LoggingComponent::write` — one record's bytes.
    ///
    /// `Address::operator<<` uses `insertRaw` on `ip`/`port`
    /// (`basictypes.cpp:199`), so those two land in the address's stored
    /// network byte order — the display-order `Address` fields are byte
    /// swapped here. The salt goes through the ordinary streaming operator
    /// and is little-endian.
    pub fn to_disk_bytes(&self) -> Vec<u8> {
        let mut os = MemoryOStream::new();
        os.add_blob(&self.address.ip.to_be_bytes());
        os.add_blob(&self.address.port.to_be_bytes());
        os.write_u16(self.address.salt);
        os.write_u32(self.user_component_id);
        os.write_u32(self.app_instance_id as u32);
        os.write_u32(self.app_type_id);
        self.message.write(&mut os);
        self.first_entry.write_to(&mut os);
        os.data().to_vec()
    }

    /// `LoggingComponent::read` — decode one record from the front of `is`.
    ///
    /// The inverse of `to_disk_bytes`: `extractRaw` leaves the network-order
    /// `ip`/`port` in the address, so they are swapped back to the
    /// display-order form the registry carries.
    pub fn read_from(is: &mut MemoryIStream) -> Option<Self> {
        let ip = is.read_u32()?.swap_bytes();
        let port = is.read_u16()?.swap_bytes();
        let salt = is.read_u16()?;
        let user_component_id = is.read_u32()?;
        let app_instance_id = is.read_u32()? as i32;
        let app_type_id = is.read_u32()?;
        let message = ComponentMessage::read(is)?;
        let first_entry = LogEntryAddress::read_from(is)?;
        if is.error() {
            return None;
        }
        Some(Self {
            address: Address { ip, port, salt },
            user_component_id,
            app_instance_id,
            app_type_id,
            message,
            first_entry,
        })
    }

    /// `LoggingComponent::updateFirstEntry`.
    pub fn update_first_entry(&mut self, suffix: &str, num_entries: i32) {
        self.first_entry = LogEntryAddress {
            suffix: suffix.to_string(),
            index: num_entries,
        };
    }

    /// `LoggingComponent::isSameComponentAs`: same announcing process.
    pub fn is_same_component_as(&self, other: &ComponentMessage) -> bool {
        self.message.version == other.version
            && self.message.uid == other.uid
            && self.message.pid == other.pid
            && self.message.component_name == other.component_name
    }

    /// `LoggingComponent::getString` — the human-readable form.
    pub fn string(&self) -> String {
        format!(
            "{}{:02} (id:{}) {}",
            self.message.component_name,
            self.app_instance_id,
            self.user_component_id,
            self.address.as_string()
        )
    }
}

/// The component table (`UserComponents`): the runtime mappings plus the
/// record order the file carries.
#[derive(Clone, Debug, Default)]
pub struct UserComponents {
    /// Records in file order — appended at each component's first entry.
    records: Vec<LoggingComponent>,
    /// The address map (`addrMap_`).
    by_addr: BTreeMap<Address, usize>,
    /// The ID map (`idMap_`).
    by_id: BTreeMap<u32, usize>,
    /// `idTicker_`: the next `userComponentID` to hand out.
    id_ticker: u32,
}

impl UserComponents {
    /// `UserComponents::read`: walk records until the file ends. `false`
    /// when a record is truncated.
    pub fn read(&mut self, data: &[u8]) -> bool {
        let mut is = MemoryIStream::new(data);
        while is.remaining() > 0 {
            let Some(component) = LoggingComponent::read_from(&mut is) else {
                return false;
            };
            let index = self.records.len();
            if component.user_component_id >= self.id_ticker {
                self.id_ticker = component.user_component_id + 1;
            }
            self.by_addr.insert(component.address, index);
            self.by_id.insert(component.user_component_id, index);
            self.records.push(component);
        }
        !is.error()
    }

    /// `UserComponents::getComponentFromMessage`: the component for a
    /// registration, replacing a stale record for the same address from a
    /// different process.
    pub fn get_or_insert_from_message(
        &mut self,
        message: &ComponentMessage,
        address: Address,
        app_type_id: u32,
    ) -> &mut LoggingComponent {
        if let Some(&index) = self.by_addr.get(&address) {
            if self.records[index].is_same_component_as(message) {
                return &mut self.records[index];
            }
            self.erase(address);
        }
        let index = self.records.len();
        let component =
            LoggingComponent::new(address, message.clone(), app_type_id, self.id_ticker);
        self.id_ticker += 1;
        self.by_addr.insert(address, index);
        self.by_id.insert(component.user_component_id, index);
        self.records.push(component);
        &mut self.records[index]
    }

    /// `UserComponents::getComponentByAddr`.
    pub fn get_by_addr(&self, address: &Address) -> Option<&LoggingComponent> {
        self.by_addr.get(address).map(|&i| &self.records[i])
    }

    /// `UserComponents::getComponentByID`.
    pub fn get_by_id(&self, component_id: u32) -> Option<&LoggingComponent> {
        self.by_id.get(&component_id).map(|&i| &self.records[i])
    }

    /// `UserComponents::erase`: drop the component for an address.
    pub fn erase(&mut self, address: Address) -> bool {
        let Some(&index) = self.by_addr.get(&address) else {
            return false;
        };
        self.by_addr.remove(&address);
        self.by_id.remove(&self.records[index].user_component_id);
        self.records.remove(index);
        // The following records slide down one slot.
        for i in index..self.records.len() {
            self.by_addr.insert(self.records[i].address, i);
            self.by_id.insert(self.records[i].user_component_id, i);
        }
        true
    }

    /// `UserComponents::write`: the bytes the record appends to the file.
    pub fn write(&mut self, address: Address) -> Option<Vec<u8>> {
        let index = *self.by_addr.get(&address)?;
        Some(self.records[index].to_disk_bytes())
    }

    /// All records, in file order.
    pub fn records(&self) -> &[LoggingComponent] {
        &self.records
    }

    /// The whole file's bytes, every record in order.
    pub fn to_file_bytes(&self) -> Vec<u8> {
        self.records
            .iter()
            .flat_map(|record| record.to_disk_bytes())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference-written components file (`/tmp/bwlog_sample/.../components`,
    /// 63 bytes): one cellappmgr component with uid 1000, pid 4242, whose
    /// first entry is entry 0 of the 21:11:58 segment.
    const SAMPLE_COMPONENTS: [u8; 63] = [
        0xc0, 0xa8, 0x05, 0xbc, 0xc5, 0x8a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x00, 0xe8, 0x03, 0x92, 0x10, 0x00, 0x00, 0x0a, 0x63,
        0x65, 0x6c, 0x6c, 0x61, 0x70, 0x70, 0x6d, 0x67, 0x72, 0x13, 0x32, 0x30, 0x32, 0x36, 0x2d,
        0x31, 0x31, 0x2d, 0x32, 0x33, 0x2d, 0x32, 0x31, 0x3a, 0x31, 0x31, 0x3a, 0x35, 0x38, 0x00,
        0x00, 0x00, 0x00,
    ];

    fn sample_message() -> ComponentMessage {
        ComponentMessage {
            version: 10,
            logger_id: String::new(),
            uid: 1000,
            pid: 4242,
            component_name: "cellappmgr".to_string(),
        }
    }

    fn sample_address() -> Address {
        Address {
            ip: 0xc0a8_05bc, // 192.168.5.188
            port: 50570,     // 0xc58a, as the reference's writeToString prints it
            salt: 0,
        }
    }

    #[test]
    fn writes_the_reference_components_file_byte_for_byte() {
        let mut components = UserComponents::default();
        let component = components.get_or_insert_from_message(
            &sample_message(),
            sample_address(),
            0, // "cellappmgr" is line 0 of component_names
        );
        component.update_first_entry("2026-11-23-21:11:58", 0);
        assert_eq!(components.to_file_bytes(), SAMPLE_COMPONENTS);
    }

    #[test]
    fn reads_the_reference_components_file_back() {
        let mut components = UserComponents::default();
        assert!(components.read(&SAMPLE_COMPONENTS));
        assert_eq!(components.records().len(), 1);
        let component = components.get_by_addr(&sample_address()).unwrap();
        assert_eq!(component.address, sample_address());
        assert_eq!(component.user_component_id, 0);
        assert_eq!(component.app_instance_id, 0);
        assert_eq!(component.app_type_id, 0);
        assert_eq!(component.message, sample_message());
        assert_eq!(component.first_entry.suffix, "2026-11-23-21:11:58");
        assert_eq!(component.first_entry.index, 0);

        // The ID the file already hands out stays taken.
        let next = components.get_or_insert_from_message(
            &ComponentMessage {
                pid: 1,
                ..sample_message()
            },
            Address::new(0x7f00_0001, 40000),
            1,
        );
        assert_eq!(next.user_component_id, 1);
    }

    #[test]
    fn a_reregistration_of_the_same_process_is_reused() {
        let mut components = UserComponents::default();
        let first = components.get_or_insert_from_message(&sample_message(), sample_address(), 0);
        first.update_first_entry("2026-11-23-21:11:58", 0);
        let second = components.get_or_insert_from_message(&sample_message(), sample_address(), 0);
        assert_eq!(second.user_component_id, 0);
        assert_eq!(components.records().len(), 1);
    }

    #[test]
    fn a_registration_from_a_new_process_replaces_the_old_record() {
        let mut components = UserComponents::default();
        components.get_or_insert_from_message(&sample_message(), sample_address(), 0);
        // Same address, different pid: a fresh process replaced it.
        let replacement = components.get_or_insert_from_message(
            &ComponentMessage {
                pid: 9999,
                ..sample_message()
            },
            sample_address(),
            0,
        );
        assert_eq!(replacement.user_component_id, 1);
        assert_eq!(components.records().len(), 1);
        assert_eq!(components.get_by_id(0), None);
    }

    #[test]
    fn erasing_removes_the_component_from_both_maps() {
        let mut components = UserComponents::default();
        components.get_or_insert_from_message(&sample_message(), sample_address(), 0);
        assert!(components.erase(sample_address()));
        assert_eq!(components.get_by_addr(&sample_address()), None);
        assert_eq!(components.get_by_id(0), None);
        assert!(!components.erase(sample_address()));
    }

    #[test]
    fn the_human_readable_form_matches_the_reference_format() {
        let mut components = UserComponents::default();
        let component =
            components.get_or_insert_from_message(&sample_message(), sample_address(), 0);
        component.app_instance_id = 1;
        assert_eq!(
            component.string(),
            "cellappmgr01 (id:0) 192.168.5.188:50570"
        );
    }

    #[test]
    fn a_truncated_record_fails_the_read() {
        let mut components = UserComponents::default();
        assert!(!components.read(&SAMPLE_COMPONENTS[..40]));
    }
}
