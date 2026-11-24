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
pub mod auto_load;
pub mod billing;
pub mod billing_creator;
pub mod buffered;
pub mod charset;
pub mod clear_auto_load;
pub mod consolidate;
pub mod consolidate_dbs;
pub mod dry_run;
pub mod error;
pub mod log_on_records;
pub mod lookup;
pub mod namer;
pub mod py_billing;
pub mod record;
pub mod recovery;
pub mod remove_db;
pub mod retry;
pub mod secondary;
pub mod snapshot_helper;
pub mod storage;
pub mod sync_db;
pub mod transaction;
pub mod transfer_db;
pub mod xml;

pub use account::{
    AccountLogOn, AccountRow, BIGWORLD_ID_COLUMN, ENTITY_ID_COLUMN, ENTITY_TYPE_COLUMN,
    ENTITY_TYPES_TABLE, INFO_TABLE, IS_PASSWORD_HASHED_COLUMN, LOG_ON_MAPPING_TABLE,
    LOG_ON_NAME_COLUMN, PASSWORD_COLUMN, UserTypeBinding, bind_user_type, check_password,
};
pub use auto_load::{
    DEFAULT_MAX_OUTSTANDING, EntityAutoLoader, PacedEntityAutoLoader, UpdateAutoLoad,
};
pub use billing::{
    AccountLookup, BillingConfig, GetEntityKeyOutcome, SetEntityKeyOutcome, XmlBillingSystem,
    XmlLogOnEntry, XmlLogOnEntryWrite, XmlLogOnMapping,
};
pub use billing_creator::{
    ACCOUNT_TYPE_NAME, CreatedBillingSystem, CreationFailure, CustomBillingSystem,
    DEFAULT_BILLING_TYPE, RegisteredCreator,
};
pub use buffered::{Admission, BufferedEntityTasks, EntityTask};
pub use charset::{
    CharsetWidths, utf8_byte_length_for, utf8_char_length, utf8_char_offsets,
    utf8_truncate_to_chars,
};
pub use clear_auto_load::{
    ClearAutoLoadDatabase, ParseOutcome, check_tables_exist, delete_auto_load_entities, parse_args,
};
pub use consolidate::{
    ConsolidationOutcome, ConsolidationQueue, ConsolidationReport, consolidate_database,
};
pub use consolidate_dbs::{
    CommandLineOptions, ConnectionInfo, ConsolidationSession, DbConsolidatorErrors,
    PrimaryDatabase, SecondaryDbInfo, StorageConsolidator, USAGE_MESSAGE, defs_digest_matches,
    parse_command_line,
};
pub use dry_run::{
    DRY_RUN_PREFIX, DryRunSyncReport, DryRunWrapper, INVALID_STATEMENT_MSG, dry_run_statements,
    dry_run_steps, is_read_only_statement,
};
pub use error::DbError;
pub use log_on_records::{
    LOGIN_IN_PROGRESS_MSG, LogOnRecordsCache, RELOGIN_IN_PROGRESS_MSG, RelogonVerdict,
};
pub use lookup::{
    EntityDefIndex, LookUpEntitiesCriteria, LookUpEntitiesFilter, LookupDefs,
    get_database_id_from_name, lookup_entities, lookup_entity_by_name,
};
pub use namer::{
    COMPONENT_NAME_SEPARATOR, ColumnIndexType, DEFAULT_SEQUENCE_COLUMN_NAME,
    DEFAULT_SEQUENCE_TABLE_NAME, GAME_TIME_COLUMN_NAME, ID_COLUMN_NAME, Indexing, Namer,
    PARENTID_COLUMN_NAME, PARENTID_INDEX_NAME, TABLE_NAME_PREFIX, TIMESTAMP_COLUMN_NAME,
    column_index_type,
};
pub use py_billing::{PyBillingResponse, PyBillingSystem};
pub use record::EntityRecord;
pub use recovery::{TRANSACTION_ATTEMPTS, query_with_reconnect, retry_query, retry_transaction};
pub use remove_db::remove_database;
pub use retry::{
    CR_CONN_HOST_ERROR, CR_CONNECTION_ERROR, CR_SERVER_GONE_ERROR, CR_SERVER_LOST, DatabaseFailure,
    ER_LOCK_DEADLOCK, ER_LOCK_WAIT_TIMEOUT, RetryTask, RunReport, failure_from_db_error,
    run_with_retry,
};
pub use secondary::{
    SecondaryDatabase, SecondaryRow, SecondaryTable, StreamKind, StreamPropDef, StreamTypeDef,
    blob_to_values, values_to_blob,
};
pub use snapshot_helper::{
    CONFIG_SECTION, ConnectFailure, FAILURE, LVCREATE, LVREMOVE, SUCCESS, SnapshotConfig,
    SnapshotConfigReader, SnapshotOps, acquire_snapshot, parse_snapshot_path, release_snapshot,
    run, validate_config,
};
pub use storage::{
    BaseMailbox, CheckOut, EntityKey, EntityStorage, EntityStorageImage, MemoryConnection,
    MemoryStorage, StorageImage, load_entity,
};
pub use sync_db::{
    AlterTableBuilder, ColumnDiff, ColumnInfo, ColumnType, DBAPP_CURRENT_VERSION, EngineSupport,
    FieldType, InspectionReport, MAX_TEXT_BLOB_INDEX_LENGTH, MYSQL_ENGINE_TYPE, SyncError,
    SyncPlanRequest, SyncRefusal, SyncStep, TableAction, TablePlan, convert_charset_statement,
    create_entity_table_statement, create_index_for, create_index_statement, generate_index_name,
    inspect, plan_statements, plan_sync, plan_table_statements, remove_index_statement,
    special_table_statements, string_blob_type_name,
};
pub use transaction::{EntityTransaction, StagedWrite};
pub use transfer_db::{
    APP_NAME, ChildRuns, ConsolidateLink, RSYNC, SendToAddressError, SnapshotPlan, TcpLink,
    TransferCommands, atoi_prefix, consolidate, convert_address, parse_send_to_address,
    transfer_primary, transfer_secondary, transfer_to,
};
pub use xml::{XmlDatabase, XmlKind, XmlPropDef, XmlSection, XmlTypeDef};
