//! BW-09 DBApp persistence.
//!
//! The DBApp in the reference (`server/dbapp/` + `lib/db*`) reads and writes
//! entity property data through a storage interface with one backend per
//! format (`db_storage_mysql`, `db_storage_xml`, and the SQLite secondary
//! store). This crate carries the storage contract and the storage-agnostic
//! entity operations, and holds the in-memory backend that the entity
//! operation acceptance cases run against. Real backends join as their own
//! `todo.md` items land (BW-09-02 MySQL, BW-09-03 XML/SQLite, BW-09-06
//! PostgreSQL).
//!
//! Layering: this is a domain library (layer 2). It composes with
//! [`entitydef`] for the persistent-property model and [`foundation`] for
//! the id vocabulary, and is consumed by the `dbapp` role runtime and the
//! BW-09-05 operational tools.

pub mod error;
pub mod lookup;
pub mod record;
pub mod storage;

pub use error::DbError;
pub use lookup::{
    EntityDefIndex, LookUpEntitiesCriteria, LookUpEntitiesFilter, LookupDefs,
    get_database_id_from_name, lookup_entities, lookup_entity_by_name,
};
pub use record::EntityRecord;
pub use storage::{EntityKey, EntityStorage, MemoryConnection, MemoryStorage, load_entity};
