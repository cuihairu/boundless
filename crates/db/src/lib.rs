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

pub mod account;
pub mod buffered_entity_tasks;
pub mod connection;
pub mod error;
pub mod lock_recovery;
pub mod log_on;
pub mod record;
pub mod retry;
pub mod schema;
pub mod storage;
pub mod transaction;
pub mod xml_database;

pub use account::{AccountLogOnStatus, AccountRow, AccountStore, hash_password};
pub use buffered_entity_tasks::{
    BufferedEntityTasks, EntityTask, EntityTaskPtr, TaskQueue, TaskSink,
};
pub use connection::StorageConnection;
pub use error::DbError;
pub use lock_recovery::{
    LockClient, LockedConnection, NamedLock, generate_lock_name, is_lost_connection,
    should_reconnect_and_retry, should_retry, transaction_should_retry,
};
pub use log_on::LogOnRecord;
pub use record::EntityRecord;
pub use retry::{Completion, run_with_retry};
pub use schema::{
    BoundAttribute, EntitySchema, IndexType, MySqlType, Namer, TableDescription, UserTypeBindings,
};
pub use storage::{EntityKey, EntityStorage, MemoryStorage, load_entity, load_entity_by_name};
pub use transaction::{StorageTransaction, run_transactional};
pub use xml_database::{
    AutoLoadUpdate, CellPose, DATABASE_AUTO_LOAD_ENTITIES_SECTION,
    DATABASE_AUTO_LOAD_SPACE_DATA_SECTION, DATABASE_DATA_DOMAINS, DATABASE_INFO_SECTION,
    DATABASE_LOGONMAPPING_SECTION, GetOutcome, XmlDatabase,
};
