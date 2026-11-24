//! Backup export and restore for the PostgreSQL storage database
//! (BW-09-07-04: an empty target database restores identical records).
//!
//! The reference ships no backup tool of its own: the primary MySQL
//! database is backed up and restored at the operator level, and what a
//! backup must carry is the database's whole state — the fixed
//! `bigworld*` tables (the schema version, the entity-type registry, the
//! log-on and checkout records, the DBID books, the space data, the
//! secondary-database registry and the entity-defs checksum) plus every
//! registered entity table (`mysql_synchronise.cpp`
//! `createSpecialBigWorldTables`, `entity_type_mapping.cpp`). This module
//! carries that state in a portable dump: each table's rows read back as
//! typed cells, and a restore replays them into a database initialised
//! by [`crate::schema_init`] so every record reads back the same — and
//! the restored registry's serial keeps allocating past the restored
//! rows.
//!
//! The restore clears each target table before replaying its rows, all
//! inside one transaction, so a failed restore leaves the target
//! untouched.

use std::time::SystemTime;

use db::DbError;
use postgres::Client;
use postgres::types::ToSql;

use crate::schema::{entity_table_name, quote_ident};
use crate::store::map_db_error;

/// One portable cell value: the value domains the fixed and entity
/// tables actually use (integers, booleans, text, bytes, doubles, float
/// vectors, JSONB text and timestamps), each nullable.
#[derive(Debug, Clone, PartialEq)]
pub enum BackupCell {
    /// SQL `NULL`.
    Null,
    /// `INTEGER`/`BIGINT` (and `BIGSERIAL` values).
    Int(i64),
    /// `BOOLEAN`.
    Bool(bool),
    /// `VARCHAR`/`TEXT`.
    Text(String),
    /// `BYTEA`.
    Bytes(Vec<u8>),
    /// `DOUBLE PRECISION`.
    Float(f64),
    /// `REAL[]`.
    RealVector(Vec<f32>),
    /// `JSONB`, as its canonical text.
    Json(String),
    /// `TIMESTAMPTZ`, at the server's microsecond precision.
    Timestamp(SystemTime),
}

/// One column's storage kind: how its cells are read and bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupColumnKind {
    /// `INTEGER` (`int4`): the reference's `INT`, and the widened
    /// `SMALLINT UNSIGNED`.
    Int32,
    /// `BIGINT` (`int8`): the reference's `INT UNSIGNED`/`INT64`.
    Int64,
    /// `BIGSERIAL`: read and bound as an `int8`, and after a restore
    /// the column's sequence is advanced past the restored rows.
    Serial,
    /// `BOOLEAN`.
    Bool,
    /// `VARCHAR`/`TEXT`.
    Text,
    /// `BYTEA`.
    Bytes,
    /// `DOUBLE PRECISION`.
    Float,
    /// `REAL[]`.
    RealVector,
    /// `JSONB`.
    Json,
    /// `TIMESTAMPTZ`.
    Timestamp,
}

/// One backed-up column: its name and storage kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupColumn {
    /// The column's name.
    pub name: String,
    /// The column's storage kind.
    pub kind: BackupColumnKind,
}

/// One table's shape: its name, its columns and the columns its dump
/// rows are ordered by (so two exports of the same records compare
/// equal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupTable {
    /// The table's name.
    pub name: String,
    /// The table's columns, in dump order.
    pub columns: Vec<BackupColumn>,
    /// The columns the dump rows are ordered by.
    pub order_by: Vec<String>,
}

/// One table's rows: the table's shape plus one cell row per record.
#[derive(Debug, Clone, PartialEq)]
pub struct BackupTableData {
    /// The table's shape.
    pub table: BackupTable,
    /// The rows, one cell per column, in the dump's order.
    pub rows: Vec<Vec<BackupCell>>,
}

/// The whole dump: the fixed tables (in the reference's creation order)
/// followed by the registered entity tables (in type id order).
#[derive(Debug, Clone, PartialEq)]
pub struct BackupDump {
    /// The dumped tables.
    pub tables: Vec<BackupTableData>,
}

/// Maps an `information_schema` column type (its `data_type` and
/// `udt_name`) to a backup column kind. Every type the schema items
/// create is covered; anything else is a fault, because silently
/// text-encoding an unknown type would lose the record.
fn column_kind_for(data_type: &str, udt_name: &str) -> Result<BackupColumnKind, DbError> {
    match data_type {
        "integer" => Ok(BackupColumnKind::Int32),
        "bigint" => Ok(BackupColumnKind::Int64),
        "boolean" => Ok(BackupColumnKind::Bool),
        "text" | "character varying" => Ok(BackupColumnKind::Text),
        "bytea" => Ok(BackupColumnKind::Bytes),
        "double precision" => Ok(BackupColumnKind::Float),
        "jsonb" => Ok(BackupColumnKind::Json),
        "timestamp with time zone" => Ok(BackupColumnKind::Timestamp),
        "ARRAY" if udt_name == "_float4" => Ok(BackupColumnKind::RealVector),
        other => Err(DbError::Storage(format!(
            "column type {other} is not covered by the backup dump"
        ))),
    }
}

/// Builds a table shape: the names and kinds zipped, then the ordering
/// columns.
fn table(name: &str, columns: &[(&str, BackupColumnKind)], order_by: &[&str]) -> BackupTable {
    BackupTable {
        name: name.to_string(),
        columns: columns
            .iter()
            .map(|(name, kind)| BackupColumn {
                name: (*name).to_string(),
                kind: *kind,
            })
            .collect(),
        order_by: order_by.iter().map(|name| (*name).to_string()).collect(),
    }
}

/// The fixed tables the reference creates, in its own order, with the
/// columns the initialisation item gives them (`schema_init.rs`). The
/// entity-type registry's `typeID` is the one serial column the state
/// carries.
pub fn fixed_table_specs() -> Vec<BackupTable> {
    vec![
        table(
            "bigworldInfo",
            &[
                ("version", BackupColumnKind::Int64),
                ("snapshotTime", BackupColumnKind::Timestamp),
                ("isPasswordHashed", BackupColumnKind::Bool),
            ],
            &["version"],
        ),
        table(
            "bigworldEntityTypes",
            &[
                ("typeID", BackupColumnKind::Serial),
                ("bigworldID", BackupColumnKind::Int64),
                ("name", BackupColumnKind::Text),
            ],
            &["typeID"],
        ),
        table(
            "bigworldLogOns",
            &[
                ("databaseID", BackupColumnKind::Int64),
                ("typeID", BackupColumnKind::Int32),
                ("objectID", BackupColumnKind::Int32),
                ("ip", BackupColumnKind::Int64),
                ("port", BackupColumnKind::Int32),
                ("salt", BackupColumnKind::Int32),
                ("shouldAutoLoad", BackupColumnKind::Bool),
            ],
            &["databaseID", "typeID"],
        ),
        table(
            "bigworldLogOnMapping",
            &[
                ("logOnName", BackupColumnKind::Bytes),
                ("password", BackupColumnKind::Bytes),
                ("entityType", BackupColumnKind::Int64),
                ("entityID", BackupColumnKind::Int64),
            ],
            &["logOnName"],
        ),
        table("bigworldNewID", &[("id", BackupColumnKind::Int32)], &["id"]),
        table(
            "bigworldUsedIDs",
            &[("id", BackupColumnKind::Int32)],
            &["id"],
        ),
        table(
            "bigworldGameTime",
            &[("time", BackupColumnKind::Int32)],
            &["time"],
        ),
        table(
            "bigworldSpaces",
            &[("id", BackupColumnKind::Int32)],
            &["id"],
        ),
        table(
            "bigworldSpaceData",
            &[
                ("id", BackupColumnKind::Int32),
                ("spaceEntryID", BackupColumnKind::Int64),
                ("entryKey", BackupColumnKind::Int32),
                ("data", BackupColumnKind::Bytes),
            ],
            &["id", "spaceEntryID", "entryKey"],
        ),
        table(
            "bigworldSecondaryDatabases",
            &[
                ("ip", BackupColumnKind::Int64),
                ("port", BackupColumnKind::Int32),
                ("location", BackupColumnKind::Bytes),
            ],
            &["ip", "port"],
        ),
        table(
            "bigworldEntityDefsChecksum",
            &[("checksum", BackupColumnKind::Text)],
            &["checksum"],
        ),
    ]
}

/// Whether a table exists in the connected database (the export reads
/// run on a plain connection).
fn table_exists(client: &mut Client, name: &str) -> Result<bool, DbError> {
    let row = client
        .query_opt("SELECT to_regclass($1) IS NOT NULL", &[&quote_ident(name)])
        .map_err(map_db_error)?;
    Ok(row.is_some_and(|row| row.get::<_, bool>(0)))
}

/// Whether the table exists inside the restore transaction.
fn table_exists_in(client: &mut postgres::Transaction<'_>, name: &str) -> Result<bool, DbError> {
    let row = client
        .query_opt("SELECT to_regclass($1) IS NOT NULL", &[&quote_ident(name)])
        .map_err(map_db_error)?;
    Ok(row.is_some_and(|row| row.get::<_, bool>(0)))
}

/// The entity tables registered in `bigworldEntityTypes`, in type id
/// order. An uninitialised database registers nothing.
fn registered_entity_tables(client: &mut Client) -> Result<Vec<String>, DbError> {
    if !table_exists(client, "bigworldEntityTypes")? {
        return Ok(Vec::new());
    }
    let rows = client
        .query(
            format!(
                "SELECT {} FROM {} ORDER BY {}",
                quote_ident("name"),
                quote_ident("bigworldEntityTypes"),
                quote_ident("typeID")
            )
            .as_str(),
            &[],
        )
        .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect())
}

/// Discovers a registered entity type's table shape from the catalogue:
/// the table's columns in ordinal order, one kind per catalogue type.
/// `Ok(None)` is a registered name whose table does not exist — the dump
/// simply skips it, the way an absent fixed table is skipped.
fn entity_table_spec(client: &mut Client, type_name: &str) -> Result<Option<BackupTable>, DbError> {
    let table_name = entity_table_name(type_name);
    if !table_exists(client, &table_name)? {
        return Ok(None);
    }
    let rows = client
        .query(
            "SELECT column_name, data_type, udt_name \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = $1 \
             ORDER BY ordinal_position",
            &[&table_name],
        )
        .map_err(map_db_error)?;
    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row.get(0);
        let data_type: String = row.get(1);
        let udt_name: String = row.get(2);
        let kind = column_kind_for(&data_type, &udt_name)?;
        columns.push(BackupColumn { name, kind });
    }
    Ok(Some(BackupTable {
        name: table_name,
        columns,
        // The id column is the table's primary key.
        order_by: vec!["id".to_string()],
    }))
}

/// The table's read statement: its columns in dump order — JSONB
/// columns cast to their canonical text (the storage read's own device:
/// the driver returns no `String` for a raw JSONB column) — ordered by
/// the shape's ordering columns.
fn select_sql(spec: &BackupTable) -> String {
    let columns = spec
        .columns
        .iter()
        .map(|column| match column.kind {
            BackupColumnKind::Json => format!("({})::text", quote_ident(&column.name)),
            _ => quote_ident(&column.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!("SELECT {columns} FROM {}", quote_ident(&spec.name));
    if !spec.order_by.is_empty() {
        let order = spec
            .order_by
            .iter()
            .map(|name| quote_ident(name))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" ORDER BY {order}"));
    }
    sql
}

/// The width contract: a row carries exactly one column per dump column.
fn check_row_width(spec: &BackupTable, row_columns: usize) -> Result<(), DbError> {
    if row_columns != spec.columns.len() {
        return Err(DbError::Storage(format!(
            "row from {} carries {} columns but the dump lists {}",
            spec.name,
            row_columns,
            spec.columns.len()
        )));
    }
    Ok(())
}

/// Reads one row into cells, one per column, in the dump order.
fn decode_row(spec: &BackupTable, row: &postgres::Row) -> Result<Vec<BackupCell>, DbError> {
    check_row_width(spec, row.len())?;
    let mut cells = Vec::with_capacity(row.len());
    for (index, column) in spec.columns.iter().enumerate() {
        let cell = match column.kind {
            BackupColumnKind::Int64 | BackupColumnKind::Serial => {
                row.get::<_, Option<i64>>(index).map(BackupCell::Int)
            }
            BackupColumnKind::Int32 => row
                .get::<_, Option<i32>>(index)
                .map(|value| BackupCell::Int(i64::from(value))),
            BackupColumnKind::Bool => row.get::<_, Option<bool>>(index).map(BackupCell::Bool),
            BackupColumnKind::Text => row.get::<_, Option<String>>(index).map(BackupCell::Text),
            BackupColumnKind::Bytes => row.get::<_, Option<Vec<u8>>>(index).map(BackupCell::Bytes),
            BackupColumnKind::Float => row.get::<_, Option<f64>>(index).map(BackupCell::Float),
            BackupColumnKind::RealVector => row
                .get::<_, Option<Vec<f32>>>(index)
                .map(BackupCell::RealVector),
            BackupColumnKind::Json => row.get::<_, Option<String>>(index).map(BackupCell::Json),
            BackupColumnKind::Timestamp => row
                .get::<_, Option<SystemTime>>(index)
                .map(BackupCell::Timestamp),
        };
        cells.push(cell.unwrap_or(BackupCell::Null));
    }
    Ok(cells)
}

/// Exports one table's rows.
fn export_table(client: &mut Client, spec: BackupTable) -> Result<BackupTableData, DbError> {
    let rows = client
        .query(select_sql(&spec).as_str(), &[])
        .map_err(map_db_error)?;
    let rows = rows
        .iter()
        .map(|row| decode_row(&spec, row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BackupTableData { table: spec, rows })
}

/// Exports the database's whole state: the fixed tables that exist (in
/// the reference's creation order) followed by the registered entity
/// tables (in type id order).
pub fn export_backup(client: &mut Client) -> Result<BackupDump, DbError> {
    let mut tables = Vec::new();
    for spec in fixed_table_specs() {
        if table_exists(client, &spec.name)? {
            tables.push(export_table(client, spec)?);
        }
    }
    for type_name in registered_entity_tables(client)? {
        if let Some(spec) = entity_table_spec(client, &type_name)? {
            tables.push(export_table(client, spec)?);
        }
    }
    Ok(BackupDump { tables })
}

/// The table-clearing statement a restore starts with.
fn delete_sql(name: &str) -> String {
    format!("DELETE FROM {}", quote_ident(name))
}

/// The table's insert statement: one placeholder per column, JSONB
/// columns bound as text and cast server-side (the storage write's own
/// device — an un-annotated parameter would type-infer as jsonb, which
/// the text binding then cannot serialize).
fn insert_sql(spec: &BackupTable) -> String {
    let columns = spec
        .columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = spec
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let placeholder = format!("${}", index + 1);
            match column.kind {
                BackupColumnKind::Json => format!("CAST({placeholder}::text AS JSONB)"),
                _ => placeholder,
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_ident(&spec.name),
        columns,
        placeholders
    )
}

/// Binds one row's cells in the statement's placeholder order. A null
/// cell binds as the column type's `None`, so the driver still declares
/// the parameter's type.
fn bind_row(spec: &BackupTable, row: &[BackupCell]) -> Result<Vec<Box<dyn ToSql + Sync>>, DbError> {
    if row.len() != spec.columns.len() {
        return Err(DbError::Storage(format!(
            "row for {} carries {} cells but the dump lists {} columns",
            spec.name,
            row.len(),
            spec.columns.len()
        )));
    }
    let mut params: Vec<Box<dyn ToSql + Sync>> = Vec::with_capacity(row.len());
    for (cell, column) in row.iter().zip(&spec.columns) {
        params.push(match (cell, column.kind) {
            (BackupCell::Null, BackupColumnKind::Timestamp) => Box::new(Option::<SystemTime>::None),
            (BackupCell::Null, BackupColumnKind::Int32) => Box::new(Option::<i32>::None),
            (BackupCell::Null, _) => Box::new(Option::<i64>::None),
            // An int32 column binds as its own width: the driver sends
            // the binary format the column expects.
            (BackupCell::Int(value), BackupColumnKind::Int32) => {
                Box::new(i32::try_from(*value).map_err(|_| {
                    DbError::Storage(format!(
                        "value {value} does not fit the int32 column {}",
                        column.name
                    ))
                })?)
            }
            (BackupCell::Int(value), _) => Box::new(*value),
            (BackupCell::Bool(value), _) => Box::new(*value),
            (BackupCell::Text(value), _) => Box::new(value.clone()),
            (BackupCell::Bytes(value), _) => Box::new(value.clone()),
            (BackupCell::Float(value), _) => Box::new(*value),
            (BackupCell::RealVector(value), _) => Box::new(value.clone()),
            (BackupCell::Json(value), _) => Box::new(value.clone()),
            (BackupCell::Timestamp(value), _) => Box::new(*value),
        });
    }
    Ok(params)
}

/// Advances a serial column's sequence past the restored rows, so the
/// next server-generated value continues after them. The relation
/// argument is the quoted identifier, because `pg_get_serial_sequence`
/// resolves an unquoted text against the lower-cased fold.
fn sequence_sql(name: &str, column: &str) -> String {
    let literal = format!("'{}'", quote_ident(name).replace('\'', "''"));
    let column_literal = format!("'{}'", column.replace('\'', "''"));
    format!(
        "SELECT setval(pg_get_serial_sequence({literal}, {column_literal}), \
         GREATEST((SELECT COALESCE(MAX({}), 0) FROM {}), 1))",
        quote_ident(column),
        quote_ident(name)
    )
}

/// Restores one table's rows inside the restore transaction: the target
/// table must exist, its existing rows are cleared, the dump's rows are
/// replayed, and a serial column's sequence follows its restored rows.
fn restore_table(
    client: &mut postgres::Transaction<'_>,
    data: &BackupTableData,
) -> Result<(), DbError> {
    let spec = &data.table;
    if !table_exists_in(client, &spec.name)? {
        return Err(DbError::Storage(format!(
            "backup table {} is missing in the target database",
            spec.name
        )));
    }
    let sql = delete_sql(&spec.name);
    client.execute(&sql, &[]).map_err(map_db_error)?;
    if data.rows.is_empty() {
        return Ok(());
    }
    let sql = insert_sql(spec);
    for row in &data.rows {
        let params = bind_row(spec, row)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
        client.execute(&sql, &refs).map_err(map_db_error)?;
    }
    for column in &spec.columns {
        if column.kind == BackupColumnKind::Serial {
            let sql = sequence_sql(&spec.name, &column.name);
            client.execute(&sql, &[]).map_err(map_db_error)?;
        }
    }
    Ok(())
}

/// Restores a dump into the connected database: every dumped table's
/// rows replace the target's, inside one transaction, so a failure
/// anywhere leaves the target as it was.
pub fn restore_backup(client: &mut Client, dump: &BackupDump) -> Result<(), DbError> {
    let mut transaction = client.transaction().map_err(map_db_error)?;
    for data in &dump.tables {
        restore_table(&mut transaction, data)?;
    }
    transaction.commit().map_err(map_db_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_fixed_specs_match_the_initialisation_order() {
        // The reference creates eleven fixed tables; the dump carries
        // them in the same order with the same names.
        let names: Vec<String> = fixed_table_specs()
            .iter()
            .map(|spec| spec.name.clone())
            .collect();
        assert_eq!(
            names,
            vec![
                "bigworldInfo",
                "bigworldEntityTypes",
                "bigworldLogOns",
                "bigworldLogOnMapping",
                "bigworldNewID",
                "bigworldUsedIDs",
                "bigworldGameTime",
                "bigworldSpaces",
                "bigworldSpaceData",
                "bigworldSecondaryDatabases",
                "bigworldEntityDefsChecksum",
            ]
        );

        // The registry's typeID is the one serial column: after a
        // restore it must keep allocating past the restored rows.
        let registry = &fixed_table_specs()[1];
        assert_eq!(registry.columns[0].kind, BackupColumnKind::Serial);
        assert_eq!(registry.columns[0].name, "typeID");
        assert_eq!(registry.columns[1].name, "bigworldID");
        assert_eq!(registry.order_by, vec!["typeID".to_string()]);

        // The log-on mapping carries the account's bytes and its target.
        let accounts = &fixed_table_specs()[3];
        assert_eq!(
            accounts
                .columns
                .iter()
                .map(|column| (column.name.as_str(), column.kind))
                .collect::<Vec<_>>(),
            vec![
                ("logOnName", BackupColumnKind::Bytes),
                ("password", BackupColumnKind::Bytes),
                ("entityType", BackupColumnKind::Int64),
                ("entityID", BackupColumnKind::Int64),
            ]
        );
    }

    #[test]
    fn the_catalogue_types_map_one_kind_per_domain() {
        assert_eq!(
            column_kind_for("integer", "").unwrap(),
            BackupColumnKind::Int32
        );
        assert_eq!(
            column_kind_for("bigint", "").unwrap(),
            BackupColumnKind::Int64
        );
        assert_eq!(
            column_kind_for("boolean", "").unwrap(),
            BackupColumnKind::Bool
        );
        assert_eq!(column_kind_for("text", "").unwrap(), BackupColumnKind::Text);
        assert_eq!(
            column_kind_for("character varying", "").unwrap(),
            BackupColumnKind::Text
        );
        assert_eq!(
            column_kind_for("bytea", "").unwrap(),
            BackupColumnKind::Bytes
        );
        assert_eq!(
            column_kind_for("double precision", "").unwrap(),
            BackupColumnKind::Float
        );
        assert_eq!(
            column_kind_for("jsonb", "").unwrap(),
            BackupColumnKind::Json
        );
        assert_eq!(
            column_kind_for("timestamp with time zone", "").unwrap(),
            BackupColumnKind::Timestamp
        );
        assert_eq!(
            column_kind_for("ARRAY", "_float4").unwrap(),
            BackupColumnKind::RealVector
        );
        // Anything the schema items do not create is refused, not
        // silently re-encoded.
        let err = column_kind_for("xmltype", "").unwrap_err();
        assert!(err.to_string().contains("not covered by the backup dump"));
        // A non-float array is likewise refused.
        let err = column_kind_for("ARRAY", "_int4").unwrap_err();
        assert!(err.to_string().contains("not covered by the backup dump"));
    }

    #[test]
    fn the_select_lists_the_columns_and_the_order() {
        let spec = table(
            "bigworldLogOns",
            &[
                ("databaseID", BackupColumnKind::Int64),
                ("typeID", BackupColumnKind::Int32),
                ("shouldAutoLoad", BackupColumnKind::Bool),
            ],
            &["databaseID", "typeID"],
        );
        assert_eq!(
            select_sql(&spec),
            "SELECT \"databaseID\", \"typeID\", \"shouldAutoLoad\" \
             FROM \"bigworldLogOns\" ORDER BY \"databaseID\", \"typeID\""
        );

        // A JSONB column reads as its canonical text, the way the
        // storage read casts it.
        let spec = table(
            "tbl_Monster",
            &[
                ("id", BackupColumnKind::Int64),
                ("sm_loot", BackupColumnKind::Json),
            ],
            &["id"],
        );
        assert_eq!(
            select_sql(&spec),
            "SELECT \"id\", (\"sm_loot\")::text \
             FROM \"tbl_Monster\" ORDER BY \"id\""
        );
    }

    #[test]
    fn the_insert_casts_jsonb_placeholders_from_text() {
        let spec = table(
            "tbl_Monster",
            &[
                ("id", BackupColumnKind::Int64),
                ("sm_loot", BackupColumnKind::Json),
                ("sm_name", BackupColumnKind::Text),
            ],
            &["id"],
        );
        assert_eq!(
            insert_sql(&spec),
            "INSERT INTO \"tbl_Monster\" (\"id\", \"sm_loot\", \"sm_name\") \
             VALUES ($1, CAST($2::text AS JSONB), $3)"
        );
    }

    #[test]
    fn bind_row_rejects_a_row_that_misses_columns() {
        let spec = table("bigworldNewID", &[("id", BackupColumnKind::Int32)], &["id"]);
        let err = bind_row(&spec, &[]).unwrap_err();
        assert!(err.to_string().contains("carries 0 cells"));
    }

    #[test]
    fn bind_row_binds_nulls_as_the_column_type() {
        // A null in every kind binds: each parameter still declares its
        // column's type so the driver can serialize it.
        let spec = table(
            "mixed",
            &[
                ("a", BackupColumnKind::Int64),
                ("b", BackupColumnKind::Bool),
                ("c", BackupColumnKind::Text),
                ("d", BackupColumnKind::Bytes),
                ("e", BackupColumnKind::Float),
                ("f", BackupColumnKind::RealVector),
                ("g", BackupColumnKind::Json),
                ("h", BackupColumnKind::Timestamp),
            ],
            &[],
        );
        let row = vec![BackupCell::Null; 8];
        let params = bind_row(&spec, &row).unwrap();
        assert_eq!(params.len(), 8);
    }

    #[test]
    fn bind_row_binds_each_cell_domain() {
        let spec = table(
            "mixed",
            &[
                ("a", BackupColumnKind::Int64),
                ("b", BackupColumnKind::Bool),
                ("c", BackupColumnKind::Text),
                ("d", BackupColumnKind::Bytes),
                ("e", BackupColumnKind::Float),
                ("f", BackupColumnKind::RealVector),
                ("g", BackupColumnKind::Json),
                ("h", BackupColumnKind::Timestamp),
            ],
            &[],
        );
        let row = vec![
            BackupCell::Int(-7),
            BackupCell::Bool(true),
            BackupCell::Text("崔".into()),
            BackupCell::Bytes(vec![0, 255]),
            BackupCell::Float(1.5),
            BackupCell::RealVector(vec![1.0, 2.0]),
            BackupCell::Json("{\"d\":null}".into()),
            BackupCell::Timestamp(
                SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123),
            ),
        ];
        let params = bind_row(&spec, &row).unwrap();
        assert_eq!(params.len(), 8);
    }

    #[test]
    fn an_int32_column_refuses_a_value_that_does_not_fit() {
        let spec = table("bigworldNewID", &[("id", BackupColumnKind::Int32)], &["id"]);
        let row = vec![BackupCell::Int(i32::MAX as i64 + 1)];
        let err = bind_row(&spec, &row).unwrap_err();
        assert!(err.to_string().contains("does not fit the int32 column"));
    }

    #[test]
    fn the_sequence_statement_targets_the_serial_column() {
        let sql = sequence_sql("bigworldEntityTypes", "typeID");
        assert_eq!(
            sql,
            "SELECT setval(pg_get_serial_sequence('\"bigworldEntityTypes\"', 'typeID'), \
             GREATEST((SELECT COALESCE(MAX(\"typeID\"), 0) FROM \"bigworldEntityTypes\"), 1))"
        );
    }

    #[test]
    fn the_width_check_rejects_a_row_that_misses_columns() {
        let spec = table("bigworldNewID", &[("id", BackupColumnKind::Int32)], &["id"]);
        assert_eq!(check_row_width(&spec, 1).unwrap(), ());
        let err = check_row_width(&spec, 0).unwrap_err();
        assert!(err.to_string().contains("carries 0 columns"));
    }
}
