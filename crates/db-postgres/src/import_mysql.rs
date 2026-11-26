//! Moving an existing MySQL dataset into the PostgreSQL backend
//! (BW-09-07-05).
//!
//! The source is the layout the reference writes (`entity_type_mapping.cpp`
//! `createTable`): one row per entity in `tbl_<TypeName>`, keyed by the
//! explicit `id` column with one `sm_<propName>` column per persistent
//! property, plus the account mapping `bigworldLogOnMapping`, keyed by
//! the unique log-on name into the binary password columns. The rows
//! arrive decoded — in the reference they are read into property values
//! through the entity defs (`MySqlDatabase::getEntity`), so the driver
//! owns the decoding and this module owns the landing.
//!
//! Every source DBID survives verbatim: the landing is the explicit-id
//! write (`insertExplicit`, put_entity_task.cpp:89-97), and the account
//! relation resolves to the same entity through the same mapping row.
//! The binary fields (`BLOB`/`VARBINARY` columns) land and read back
//! byte-for-byte.
//!
//! A pass is re-runnable: a slot that already holds equal content is
//! skipped and reported, a conflicting one fails with both shapes named —
//! so a rerun never duplicates.

use db::{DbError, EntityKey, EntityStorage};
use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId};

use crate::accounts::AccountRow;
use crate::schema::EntitySchema;
use crate::store::PostgresStorage;

/// One entity row of the source dataset, decoded: the row's explicit DBID
/// and its `sm_*` columns in the schema's visit order. `PartialEq` only —
/// a property value may be a float, so the row is not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub struct MySqlEntityRow {
    /// The source row's `id` column (`BIGINT UNSIGNED` in the reference).
    pub db_id: DatabaseId,
    /// The decoded properties, in the schema's visit order.
    pub values: Vec<PropertyValue>,
}

/// What a migration pass landed and skipped: `written` rows were imported,
/// `skipped` rows already stood with equal content and were left alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Rows the pass wrote.
    pub written: usize,
    /// Rows that already stood with equal content.
    pub skipped: usize,
}

/// Imports the entity rows of one type. An absent slot takes the
/// explicit-id write (a duplicate id there would fail and leave the
/// original row — the reference's `insertExplicit`), a slot already
/// holding equal values is skipped, and a slot holding different values
/// fails with the conflicting shapes named — a rerun never duplicates,
/// and it never silently overwrites the stored row.
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

/// Imports the account mapping rows: the log-on name to the password
/// bytes and the entity the account owns. A fresh name is written, a name
/// already mapped identically is skipped, and a name mapped elsewhere
/// fails — the mapping's unique log-on key never moves silently.
pub fn import_accounts(
    storage: &mut PostgresStorage,
    rows: &[AccountRow],
) -> Result<ImportReport, DbError> {
    let mut report = ImportReport::default();
    for row in rows {
        let found = storage.get_account(&row.log_on_name)?;
        match found {
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
