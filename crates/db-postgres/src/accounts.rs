//! The accounts table: `bigworldLogOnMapping` and its unique log-on name
//! (`mysql_synchronise.cpp` fixed tables; `tasks/set_entity_key_for_account_task.cpp`
//! for the semantics).
//!
//! The reference maps one account to one entity: the log-on name (and
//! the email) carry a unique key, so a second account with the same
//! name is a contract error, not a storage fault — the DBApp reports
//! it as the "account already exists" outcome. The insert is a plain
//! `INSERT`: unlike the entity tables it never upserts, because a
//! duplicate name must surface as [`DbError::DuplicateAccount`] rather
//! than silently move the name to another entity.

use db::DbError;
use foundation::ids::{DatabaseId, EntityTypeId};
use postgres::types::ToSql;

use crate::schema::quote_ident;
use crate::store::{PostgresStorage, map_db_error};

/// The reference fixed-table name for the log-on mapping.
pub const ACCOUNT_TABLE: &str = "bigworldLogOnMapping";

/// One `bigworldLogOnMapping` row: the account's log-on name, the
/// password the account authenticates with, the entity type it maps
/// to and the entity's database id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    /// The log-on name (`VARBINARY` in the reference: raw bytes).
    pub log_on_name: Vec<u8>,
    /// The stored password (`VARBINARY`).
    pub password: Vec<u8>,
    /// The entity type the account's entity is created as.
    pub entity_type: EntityTypeId,
    /// The entity's database id (the log-on mapping's target).
    pub entity_id: DatabaseId,
}

/// The `CREATE TABLE` for the log-on mapping: the log-on name is the
/// primary key — the unique constraint the duplicate contract comes
/// from. `VARBINARY` columns are `BYTEA` in PostgreSQL.
pub fn account_table_ddl() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {} ({} BYTEA NOT NULL PRIMARY KEY, \
         {} BYTEA NOT NULL, {} BIGINT NOT NULL, {} BIGINT NOT NULL)",
        quote_ident(ACCOUNT_TABLE),
        quote_ident("logOnName"),
        quote_ident("password"),
        quote_ident("entityType"),
        quote_ident("entityID")
    )
}

impl PostgresStorage {
    /// Creates the account table if it is missing.
    pub fn create_account_table(&mut self) -> Result<(), DbError> {
        self.client_mut()
            .execute(account_table_ddl().as_str(), &[])
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Inserts an account row. A log-on name that already belongs to
    /// another account row is the contract's duplicate outcome
    /// ([`DbError::DuplicateAccount`]) — the reference's "account
    /// already exists" contract error — not a storage fault.
    pub fn put_account(&mut self, row: &AccountRow) -> Result<(), DbError> {
        let sql = format!(
            "INSERT INTO {} ({}, {}, {}, {}) VALUES ($1, $2, $3, $4)",
            quote_ident(ACCOUNT_TABLE),
            quote_ident("logOnName"),
            quote_ident("password"),
            quote_ident("entityType"),
            quote_ident("entityID")
        );
        let params: [&(dyn ToSql + Sync); 4] = [
            &row.log_on_name,
            &row.password,
            &(row.entity_type as i64),
            &row.entity_id,
        ];
        self.client_mut()
            .execute(sql.as_str(), &params)
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Reads an account row by its log-on name: `Ok(None)` is the
    /// reference's "no such user" outcome, not a failure.
    pub fn get_account(&mut self, log_on_name: &[u8]) -> Result<Option<AccountRow>, DbError> {
        let sql = format!(
            "SELECT {}, {}, {}, {} FROM {} WHERE {} = $1",
            quote_ident("logOnName"),
            quote_ident("password"),
            quote_ident("entityType"),
            quote_ident("entityID"),
            quote_ident(ACCOUNT_TABLE),
            quote_ident("logOnName")
        );
        let row = self
            .client_mut()
            .query_opt(sql.as_str(), &[&log_on_name])
            .map_err(map_db_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(AccountRow {
            log_on_name: row.get(0),
            password: row.get(1),
            entity_type: row.get::<_, i64>(2) as EntityTypeId,
            entity_id: row.get::<_, i64>(3),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_on_name_is_the_primary_key() {
        // The unique constraint the duplicate contract comes from: the
        // table's own DDL, not a later index.
        let ddl = account_table_ddl();
        assert_eq!(
            ddl,
            "CREATE TABLE IF NOT EXISTS \"bigworldLogOnMapping\" \
             (\"logOnName\" BYTEA NOT NULL PRIMARY KEY, \
             \"password\" BYTEA NOT NULL, \
             \"entityType\" BIGINT NOT NULL, \
             \"entityID\" BIGINT NOT NULL)"
        );
    }

    #[test]
    fn an_account_row_carries_the_mapping() {
        let row = AccountRow {
            log_on_name: b"alice".to_vec(),
            password: b"secret".to_vec(),
            entity_type: 42,
            entity_id: 99,
        };
        assert_eq!(row.entity_type, 42);
        assert_eq!(row.entity_id, 99);
        assert_eq!(row.log_on_name, b"alice");
    }
}
