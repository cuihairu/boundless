//! Exporting a full backup of the database and restoring it into an
//! empty target (BW-09-07-04, 验收 空目标库恢复相同记录).
//!
//! The reference has no in-tree backup tool — its MySQL server was
//! dumped externally (`mysqldump`). What that dump guarantees is what
//! the acceptance checks against a restored copy (docs/operations/
//! database.md): the same set of values BW-09-07 checks after an import
//! — entity ids, account links, binary fields, defaults, **and the next
//! allocatable DBID**. The last one is the crux: if the next DBID does
//! not line up after a restore, the allocation sequence (BW-09-06-04)
//! was not part of the backup — so each type's identity seat rides with
//! the backup, not with the largest restored id.
//!
//! The export reads the database's own truth: the `bigworldEntityTypes`
//! mapping rows (each type's collapsed id and its name — the table name
//! stem), every row of every mapped type (decoded, through the same
//! registered surface `all_entities` reads), every `bigworldLogOnMapping`
//! row, and each identity's seat — what the next allocation hands out.
//!
//! The restore lands the data into an **empty** target the caller has
//! already brought to the empty-schema state (the entity tables created
//! and the schemas registered, BW-09-07-01's job): mapping rows first
//! (addType's upsert legs), then rows and accounts through the import's
//! three-leg driver (absent → written with the id verbatim, equal →
//! skipped, conflicting → named failure — BW-09-07-07's rerun contract
//! rides through, so re-running a restore over a restored target lands
//! nothing twice), and the identity seats LAST, after every row: the
//! explicit writes re-seat each identity to the largest restored id
//! (`bump_identity`), and the backup's own seat then overrides it —
//! down as well as up, which is exactly the "the sequence was part of
//! the backup" guarantee.

use std::collections::BTreeMap;

use db::{DbError, EntityStorage};
use foundation::ids::{DatabaseId, EntityTypeId};

use crate::accounts::AccountRow;
use crate::import_mysql::{ImportReport, MySqlEntityRow, import_accounts, import_entities};
use crate::schema::{entity_table_name, quote_ident};
use crate::store::{PostgresStorage, map_db_error};

/// One entity type's slice of the backup: its mapping row (the collapsed
/// type id and the name the table stems from), every row of its table,
/// and what its identity hands out on the next allocation.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityTypeBackup {
    /// The type's collapsed id (`typeID`/`bigworldID` in the mapping).
    pub type_id: EntityTypeId,
    /// The type's name: the table is `tbl_<name>`.
    pub name: String,
    /// Every row of the type's table, in id order.
    pub rows: Vec<MySqlEntityRow>,
    /// What the type's identity hands out on the next allocation —
    /// the seat itself, so a restore never re-issues an id the source
    /// had already allocated to a since-deleted row.
    pub next_dbid: DatabaseId,
}

/// A full backup of the database: every entity type's mapping, rows and
/// identity seat, plus every account mapping row.
#[derive(Debug, Clone, PartialEq)]
pub struct DbBackup {
    /// The mapped entity types, in `typeID` order.
    pub entity_types: Vec<EntityTypeBackup>,
    /// Every `bigworldLogOnMapping` row, in log-on-name order.
    pub accounts: Vec<AccountRow>,
}

/// The mapping rows the export carries: `(typeID, name)`, in `typeID`
/// order — the reference's `entityTypeMappings_` surface.
fn list_entity_type_mappings(
    storage: &mut PostgresStorage,
) -> Result<Vec<(EntityTypeId, String)>, DbError> {
    let rows = storage
        .client_mut()
        .query(
            "SELECT \"typeID\", \"name\" FROM \"bigworldEntityTypes\" ORDER BY \"typeID\"",
            &[],
        )
        .map_err(map_db_error)?;
    Ok(rows
        .iter()
        .map(|row| (row.get::<_, i64>(0) as EntityTypeId, row.get(1)))
        .collect())
}

/// Every account mapping row, in log-on-name order — the export's
/// account surface; restore lands them through the import's three legs.
fn list_all_accounts(storage: &mut PostgresStorage) -> Result<Vec<AccountRow>, DbError> {
    let rows = storage
        .client_mut()
        .query(
            "SELECT \"logOnName\", \"password\", \"entityType\", \"entityID\" FROM \
             \"bigworldLogOnMapping\" ORDER BY \"logOnName\"",
            &[],
        )
        .map_err(map_db_error)?;
    Ok(rows
        .iter()
        .map(|row| AccountRow {
            log_on_name: row.get(0),
            password: row.get(1),
            entity_type: row.get::<_, i64>(2) as EntityTypeId,
            entity_id: row.get::<_, i64>(3),
        })
        .collect())
}

/// Reads what the table's identity hands out on the next allocation:
/// `last_value + 1` once the sequence has handed anything out,
/// `start_value` while it never has (`pg_sequences` reports a NULL
/// `last_value` for a virgin sequence). The table name travels the way
/// the bump-identity argument does — `pg_get_serial_sequence` parses it
/// as an identifier, and its fully-qualified result names the row of
/// `pg_sequences` exactly.
/// The seat read. The sequence is found by its **raw catalog name**:
/// the create path (`"id" BIGSERIAL`) always names a table's identity
/// `<table>_id_seq`, and `pg_sequences` stores names unquoted — so a
/// plain equality on `sequencename` sidesteps the quoting mismatch a
/// regclass-text comparison hits (`pg_get_serial_sequence` prints
/// `public."tbl_Hero_id_seq"`, the catalog composes
/// `public.tbl_Hero_id_seq`). A missing table simply matches nothing,
/// which also keeps `pg_get_serial_sequence` out of the read entirely —
/// on a nonexistent relation it raises instead of returning NULL, and
/// the planner hoists an uncorrelated scalar subquery out of a CASE
/// into an eagerly evaluated init-plan, so a guard cannot protect it.
const NEXT_IDENTITY_SQL: &str = "SELECT CASE WHEN p.last_value IS NULL THEN p.start_value \
     ELSE p.last_value + 1 END \
     FROM pg_sequences p \
     WHERE p.sequencename = $1";

/// The raw sequence name the create path produced for `table` — the
/// `BIGSERIAL` convention (`<table>_id_seq`), with the quoted form the
/// callers pass stripped back to the bare name the catalog stores.
fn identity_sequence_name(table: &str) -> String {
    format!("{}_id_seq", table.trim_matches('"'))
}

/// The seat write: `is_called` stays true, so re-exporting a restored
/// database computes the same next value back.
const SET_IDENTITY_SEAT_SQL: &str =
    "SELECT setval(pg_get_serial_sequence($1, 'id')::regclass, $2, true)";

/// Reads what the table's identity hands out on the next allocation:
/// `last_value + 1` once the sequence has handed anything out,
/// `start_value` while it never has (`pg_sequences` reports a NULL
/// `last_value` for a virgin sequence). A missing table — or a table
/// without an identity — matches no catalog row and comes back as the
/// named error.
fn next_identity(storage: &mut PostgresStorage, table: &str) -> Result<DatabaseId, DbError> {
    let row = storage
        .client_mut()
        .query_opt(NEXT_IDENTITY_SQL, &[&identity_sequence_name(table)])
        .map_err(map_db_error)?;
    row.map(|row| row.get::<_, i64>(0))
        .ok_or_else(|| DbError::Storage(format!("table {table} has no identity sequence")))
}

/// Re-seats the table's identity so the next allocation hands out
/// `seat + 1` — `is_called` stays true, so re-exporting a restored
/// database computes the same next value back (an export/restore
/// round-trip leaves no trace in the seats). A seat of 0 (a virgin
/// table's next value is 1) needs no call: a freshly created table is
/// already there.
fn set_identity_seat(
    storage: &mut PostgresStorage,
    table: &str,
    seat: DatabaseId,
) -> Result<(), DbError> {
    storage
        .client_mut()
        .execute(SET_IDENTITY_SEAT_SQL, &[&table, &seat])
        .map_err(map_db_error)?;
    Ok(())
}

/// Exports a full backup of the database. The type surface is the
/// mapping table's: every mapped type carries its mapping row, its
/// identity seat, and — when the type is registered on this connection —
/// its rows decoded in the schema's visit order (a mapped type that is
/// not registered exports with no rows; the port's lifecycle maps every
/// registered type, the way the reference's `entityTypeMappings_` is
/// the one list the startup walks).
pub fn export_backup(storage: &mut PostgresStorage) -> Result<DbBackup, DbError> {
    // The rows first, decoded and grouped per type — the same registered
    // surface `all_entities` reads, already in (type, id) order.
    let mut rows: BTreeMap<EntityTypeId, Vec<MySqlEntityRow>> = BTreeMap::new();
    for record in EntityStorage::all_entities(storage)? {
        rows.entry(record.type_id)
            .or_default()
            .push(MySqlEntityRow {
                db_id: record.db_id,
                values: record.values,
            });
    }
    let mut entity_types = Vec::new();
    for (type_id, name) in list_entity_type_mappings(storage)? {
        let table = quote_ident(&entity_table_name(&name));
        entity_types.push(EntityTypeBackup {
            type_id,
            name: name.clone(),
            rows: rows.remove(&type_id).unwrap_or_default(),
            next_dbid: next_identity(storage, &table)?,
        });
    }
    let accounts = list_all_accounts(storage)?;
    Ok(DbBackup {
        entity_types,
        accounts,
    })
}

/// Restores a backup into an **empty** target — the caller has already
/// created the entity tables and registered the schemas (BW-09-07-01's
/// empty-schema state); this lands the data: mapping rows, entity rows
/// with their ids verbatim, account mappings, and the identity seats.
/// The report counts entity rows and account rows together: a restore
/// into an empty target writes everything once, a re-run over a
/// restored target skips everything (`BW-09-07-07`'s rerun contract —
/// nothing lands twice).
pub fn restore_backup(
    storage: &mut PostgresStorage,
    backup: &DbBackup,
) -> Result<ImportReport, DbError> {
    let mut report = ImportReport::default();
    for entry in &backup.entity_types {
        // The mapping row first (addType's upsert legs), the way the
        // reference's startup registers its types before any row moves.
        storage.add_entity_type(entry.type_id, &entry.name)?;
        let schema = storage.schema_of(entry.type_id).cloned().ok_or_else(|| {
            DbError::Storage(format!(
                "entity type {} is not registered on the target",
                entry.type_id
            ))
        })?;
        if !entry.rows.is_empty() {
            let landed = import_entities(storage, entry.type_id, &schema, &entry.rows)?;
            report.written += landed.written;
            report.skipped += landed.skipped;
        }
    }
    let landed = import_accounts(storage, &backup.accounts)?;
    report.written += landed.written;
    report.skipped += landed.skipped;
    // The seats land last, after every row: the explicit writes have
    // re-seated each identity to the largest restored id, and the
    // backup's own seat now overrides it — down as well as up.
    for entry in &backup.entity_types {
        if entry.next_dbid > 1 {
            let table = quote_ident(&entity_table_name(&entry.name));
            set_identity_seat(storage, &table, entry.next_dbid - 1)?;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_read_keys_the_raw_sequence_name() {
        // The seat read finds the sequence by its raw catalog name (the
        // BIGSERIAL convention), with the virgin NULL `last_value`
        // falling back to the start value — no relation resolution that
        // would raise on a missing table.
        assert!(NEXT_IDENTITY_SQL.contains("WHERE p.sequencename = $1"));
        assert!(NEXT_IDENTITY_SQL.contains("WHEN p.last_value IS NULL THEN p.start_value"));
        assert_eq!(identity_sequence_name("\"tbl_Hero\""), "tbl_Hero_id_seq");
    }

    #[test]
    fn the_seat_write_sets_is_called_true() {
        // The write keeps is_called true so a re-export computes the
        // same next value back — an export/restore round-trip leaves no
        // trace in the seats.
        assert!(SET_IDENTITY_SEAT_SQL.ends_with(", true)"));
    }
}
