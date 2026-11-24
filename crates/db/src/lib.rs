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
pub mod buffered;
pub mod charset;
pub mod config;
pub mod dry_run;
pub mod error;
pub mod lookup;
pub mod namer;
pub mod record;
pub mod recovery;
pub mod retry;
pub mod storage;
pub mod sync_db;
pub mod transaction;
pub mod xml;

pub use config::{DatabaseConfig, PostgresConnection, StorageBackend};
pub use account::{
    AccountLogOn, AccountRow, BIGWORLD_ID_COLUMN, ENTITY_ID_COLUMN, ENTITY_TYPE_COLUMN,
    ENTITY_TYPES_TABLE, INFO_TABLE, IS_PASSWORD_HASHED_COLUMN, LOG_ON_MAPPING_TABLE,
    LOG_ON_NAME_COLUMN, PASSWORD_COLUMN, UserTypeBinding, bind_user_type, check_password,
};
pub use buffered::{Admission, BufferedEntityTasks, EntityTask};
pub use charset::{
    CharsetWidths, utf8_byte_length_for, utf8_char_length, utf8_char_offsets,
    utf8_truncate_to_chars,
};
pub use error::DbError;
pub use lookup::{
    EntityDefIndex, LookUpEntitiesCriteria, LookUpEntitiesFilter, LookupDefs,
    get_database_id_from_name, lookup_entities, lookup_entity_by_name,
};
pub use namer::{
    COMPONENT_NAME_SEPARATOR, ColumnIndexType, DEFAULT_SEQUENCE_COLUMN_NAME,
    DEFAULT_SEQUENCE_TABLE_NAME, GAME_TIME_COLUMN_NAME, ID_COLUMN_NAME, Indexing, Namer,
    PARENTID_COLUMN_NAME, TABLE_NAME_PREFIX, TIMESTAMP_COLUMN_NAME, column_index_type,
};
pub use record::EntityRecord;
pub use recovery::{TRANSACTION_ATTEMPTS, query_with_reconnect, retry_query, retry_transaction};
pub use retry::{
    CR_CONN_HOST_ERROR, CR_CONNECTION_ERROR, CR_SERVER_GONE_ERROR, CR_SERVER_LOST, DatabaseFailure,
    ER_LOCK_DEADLOCK, ER_LOCK_WAIT_TIMEOUT, RetryTask, RunReport, failure_from_db_error,
    run_with_retry,
};
pub use storage::{
    BaseMailbox, CheckOut, EntityKey, EntityStorage, EntityStorageImage, MemoryConnection,
    MemoryStorage, StorageImage, load_entity,
};
pub use transaction::{EntityTransaction, StagedWrite};
pub use xml::{XmlDatabase, XmlKind, XmlPropDef, XmlSection, XmlTypeDef};
