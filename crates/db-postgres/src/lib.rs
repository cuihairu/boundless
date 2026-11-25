//! BW-09-06 the PostgreSQL storage backend for the DBApp.
//!
//! The reference's production backend is MySQL (`lib/db_storage_mysql`);
//! Boundless runs PostgreSQL, so this crate is that backend's port: the
//! same storage contract ([`db`]), the same table and column naming
//! (`tbl_<TypeName>` / `sm_<propName>`, `entity_type_mapping.cpp`), one
//! PostgreSQL column type per property kind, and the same write semantics
//! (`REPLACE INTO` becomes `INSERT ... ON CONFLICT DO UPDATE`). Nested
//! property values (`ARRAY`, `TUPLE`, `FIXED_DICT`, `MAILBOX`) travel as
//! tagged JSONB (`schema`, `json`).
//!
//! The PostgreSQL cases run against a real server: they read their
//! connection string from `BOUNDLESS_PG_URL` (or the CI storage service's
//! `CI_STORE_PG_*` settings), and skip with a notice when no server is
//! reachable — the same gate the differential cases use. The connection
//! string is a libpq `key=value` string, and the cases create and drop
//! their own private databases, so a run never leaves residue behind.
//!
//! Layering: this is a domain library (layer 2). It composes with [`db`]
//! for the storage contract, [`entitydef`] for the property-value model
//! and [`foundation`] for the id vocabulary.

pub mod accounts;
pub mod backup;
pub mod dry_run;
pub mod import_mysql;
pub mod json;
pub mod schema;
pub mod schema_init;
pub mod store;
pub mod upgrade;

pub use accounts::{AccountRow, account_table_ddl};
pub use backup::{
    BackupCell, BackupColumn, BackupColumnKind, BackupDump, BackupTable, BackupTableData,
    export_backup, fixed_table_specs, restore_backup,
};
pub use import_mysql::{ImportReport, MySqlEntityRow, import_accounts, import_entities};
pub use schema::{ColumnKind, EntitySchema, column_name, entity_table_name, quote_ident};
pub use schema_init::{CURRENT_VERSION, OLDEST_SUPPORTED_VERSION};
pub use store::PostgresStorage;
