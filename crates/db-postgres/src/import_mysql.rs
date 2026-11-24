//! Importing an existing MySQL dataset into the PostgreSQL backend
//! (BW-09-07-05, 验收 ID、关系和二进制字段对照).
//!
//! The source is the MySQL table layout the reference writes
//! (`entity_type_mapping.cpp` `createTable`): one row per entity in
//! `tbl_<TypeName>` keyed by the explicit `id` column with one `sm_*`
//! column per persistent property, plus the account mapping
//! (`bigworldLogOnMapping` keyed by the unique log-on name into the
//! binary password columns). The rows arrive decoded — the reference
//! reads them into property values through the entity defs
//! (`MySqlDatabase::getEntity`) — and the import lands them with the
//! explicit-id write (`insertExplicit`, put_entity_task.cpp:89-97) so
//! every source DBID survives verbatim, and the identity sequence is
//! re-seated past each imported id (BW-09-07-06's guarantee).
//!
//! The import is re-runnable (BW-09-07-07): a row whose slot already
//! holds equal content is skipped and reported, a conflicting one fails
//! with both shapes named — a rerun never duplicates, and every rerun
//! re-compares the ids, the account relations and the binary fields.

use db::{DbError, EntityKey, EntityStorage};
use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId};

use crate::accounts::AccountRow;
use crate::schema::EntitySchema;
use crate::store::PostgresStorage;

/// One entity row of the MySQL source (`tbl_<TypeName>`), decoded: the
/// explicit DBID and the `sm_*` columns in the schema's visit order.
/// `PartialEq` only — a row carries floats, so it is not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub struct MySqlEntityRow {
    /// The source row's `id` (`BIGINT UNSIGNED` in the reference).
    pub db_id: DatabaseId,
    /// The `sm_*` columns, in the schema's visit order.
    pub values: Vec<PropertyValue>,
}

/// What a migration pass landed and skipped: `written` rows were
/// imported, `skipped` rows already stood with equal content and were
/// left alone (the rerun's no-duplicate guarantee).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub written: usize,
    pub skipped: usize,
}

/// Imports the entity rows of one type. An absent DBID is created with
/// the explicit-id write (a duplicate id there would fail and leave the
/// original row — the reference's `insertExplicit`), an id already
/// holding equal values is skipped, an id holding different values fails
/// with the conflicting shapes named.
pub fn import_entities(
    storage: &mut PostgresStorage,
    type_id: EntityTypeId,
    schema: &EntitySchema,
    rows: &[MySqlEntityRow],
) -> Result<ImportReport, DbError> {
    let mut report = ImportReport::default();
    for row in rows {
        let key = EntityKey {
            type_id,
            db_id: row.db_id,
        };
        match crate::store::PostgresStorage::get_entity(storage, schema, &key)? {
            None => {
                EntityStorage::put_entity(storage, &key, &row.values, true)?;
                report.written += 1;
            }
            Some(found) => {
                if found.values == row.values {
                    report.skipped += 1;
                } else {
                    return Err(DbError::Storage(format!(
                        "entity {} id {} conflicts: imported {:?}, stored {:?}",
                        schema.type_name, row.db_id, row.values, found.values
                    )));
                }
            }
        }
    }
    Ok(report)
}

/// Imports the account mapping rows. A fresh log-on name is written, a
/// name already mapped identically is skipped, a name mapped elsewhere
/// fails with the contract's duplicate error — the mapping's unique
/// log-on key never moves silently.
pub fn import_accounts(
    storage: &mut PostgresStorage,
    rows: &[AccountRow],
) -> Result<ImportReport, DbError> {
    let mut report = ImportReport::default();
    for row in rows {
        match storage.get_account(&row.log_on_name)? {
            None => {
                storage.put_account(row)?;
                report.written += 1;
            }
            Some(found) => {
                if &found == row {
                    report.skipped += 1;
                } else {
                    return Err(DbError::DuplicateAccount);
                }
            }
        }
    }
    Ok(report)
}
