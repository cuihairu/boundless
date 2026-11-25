//! The log-on record: the checked-out base location of an online entity.
//!
//! The reference stores one row per checked-out entity in
//! `tbl_LogOnRecords` (`EntityTypeMapping::addLogOnRecord`,
//! `entity_type_mapping.cpp:773-783`): the entity's database and type ids
//! plus the base's mailbox — entity id, ip, port, salt. `PutEntityTask`
//! adds or updates the record when a put carries a base mailbox
//! (`put_entity_task.cpp:120-131`, guarded by
//! `definitelyExists || checkExists` so no record ever points at a missing
//! row), `DelEntityTask` removes it (`del_entity_task.cpp:66`), and a
//! relogon overwrites it — always exactly one record per entity row.

use foundation::ids::EntityId;

/// The checked-out base location of an online entity
/// (`tbl_LogOnRecords`, `entity_type_mapping.cpp:773-783`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogOnRecord {
    /// The base entity's id (`mailbox.id`).
    pub entity_id: EntityId,
    /// The base app's address (`mailbox.addr.ip`).
    pub ip: u32,
    /// The base app's port (`mailbox.addr.port`).
    pub port: u16,
    /// The address salt (`mailbox.addr.salt`).
    pub salt: u32,
}
