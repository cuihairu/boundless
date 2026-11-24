//! The PostgreSQL storage backend: entity rows in `tbl_<TypeName>`
//! (`lib/db_storage_mysql`, ported to PostgreSQL).
//!
//! The reference writes one row per entity: the DBID in the id column,
//! one column per persistent property, and a `REPLACE INTO` for the write
//! (create and update share the upsert — the row is keyed by the DBID).
//! The PostgreSQL port keeps the same shape: `INSERT ... ON CONFLICT
//! ("id") DO UPDATE` replaces the row, and `get_entity` reads the columns
//! back in the schema's visit order, one [`PropertyValue`] per column.

use db::{DbError, EntityKey, EntityRecord};
use entitydef::value::PropertyValue;
use postgres::Client;
use postgres::error::SqlState;
use postgres::types::ToSql;

use crate::json;
use crate::schema::{ColumnKind, EntitySchema, ID_COLUMN, column_name, quote_ident};

/// Maps a backend error to the storage contract. A unique-key violation is
/// the contract's duplicate outcome (BW-09-06-03: the accounts table's
/// unique log-on name); everything else is a storage fault.
/// The full message: the top-level `Display` renders just "db error"
/// and the PostgreSQL text lives in the source chain, so the two are
/// joined with `: `.
fn error_message(error: &postgres::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(err) = source {
        message.push_str(&format!(": {err}"));
        source = err.source();
    }
    message
}

pub fn map_db_error(error: postgres::Error) -> DbError {
    let is_duplicate = error
        .code()
        .is_some_and(|code| *code == SqlState::UNIQUE_VIOLATION);
    if is_duplicate {
        DbError::DuplicateAccount
    } else {
        DbError::Storage(error_message(&error))
    }
}

/// A `Uint` must fit `BIGINT` — PostgreSQL has no unsigned integer.
fn uint_to_bigint(value: u64) -> Result<i64, DbError> {
    i64::try_from(value)
        .map_err(|_| DbError::Storage(format!("uint64 value {value} exceeds BIGINT")))
}

/// Validates a record against its schema before any SQL runs: the values
/// must be the schema's columns in visit order, the DBID must be assigned,
/// and every `Uint` must fit `BIGINT`.
fn validate_put(schema: &EntitySchema, record: &EntityRecord) -> Result<(), DbError> {
    if record.values.len() != schema.columns.len() {
        return Err(DbError::Storage(format!(
            "record for {} carries {} values but the schema lists {} columns",
            schema.type_name,
            record.values.len(),
            schema.columns.len()
        )));
    }
    if record.db_id < 0 {
        return Err(DbError::Storage(format!(
            "record for {} still has a pending database id",
            schema.type_name
        )));
    }
    for value in &record.values {
        if let PropertyValue::Uint(n) = value {
            uint_to_bigint(*n)?;
        }
    }
    Ok(())
}

/// Builds the upsert statement for a schema: the id column first, then one
/// placeholder per property column, nested columns cast to JSONB. The
/// `ON CONFLICT ("id") DO UPDATE` clause is the reference `REPLACE INTO`.
fn put_entity_sql(schema: &EntitySchema) -> String {
    let mut placeholders = vec!["$1".to_string()];
    for (index, (_, kind)) in schema.columns.iter().enumerate() {
        let placeholder = format!("${}", index + 2);
        placeholders.push(match kind {
            // The param binds as text and casts server-side: an un-annotated
            // CAST would type-infer the parameter itself as jsonb, which the
            // text binding then cannot serialize.
            ColumnKind::Nested => format!("CAST({placeholder}::text AS JSONB)"),
            _ => placeholder,
        });
    }
    let assignments = schema
        .columns
        .iter()
        .map(|(prop, _)| {
            format!(
                "{} = EXCLUDED.{}",
                quote_ident(&column_name(prop)),
                quote_ident(&column_name(prop))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) DO UPDATE SET {}",
        schema.quoted_table(),
        schema.quoted_columns().join(", "),
        placeholders.join(", "),
        quote_ident(ID_COLUMN),
        assignments
    )
}

/// A bound parameter (the id first, then one per column).
type PutParam = Box<dyn ToSql + Sync>;

/// Builds the parameter list for a record, in the statement's placeholder
/// order (the id first). Nested values bind as their JSON text; the caller
/// keeps the text strings alive for the borrow.
fn put_entity_params(
    schema: &EntitySchema,
    record: &EntityRecord,
) -> Result<(Vec<PutParam>, Vec<String>), DbError> {
    validate_put(schema, record)?;
    let mut params: Vec<PutParam> = vec![Box::new(record.db_id)];
    let mut nested_texts: Vec<String> = Vec::new();
    for value in &record.values {
        match value {
            PropertyValue::Int(n) => params.push(Box::new(*n)),
            PropertyValue::Uint(n) => params.push(Box::new(uint_to_bigint(*n)?)),
            PropertyValue::Float(x) => params.push(Box::new(*x)),
            PropertyValue::String(s) => params.push(Box::new(s.clone())),
            PropertyValue::Blob(bytes) => params.push(Box::new(bytes.clone())),
            PropertyValue::Vector(components) => params.push(Box::new(components.clone())),
            nested => {
                let text = json::encode_to_string(nested);
                nested_texts.push(text.clone());
                params.push(Box::new(text));
            }
        }
    }
    Ok((params, nested_texts))
}

/// Builds the select statement for a schema: the id column first, then one
/// quoted column per property in visit order, JSONB columns cast to their
/// canonical JSON text.
fn get_entity_sql(schema: &EntitySchema) -> String {
    let mut select_columns = vec![quote_ident(ID_COLUMN)];
    select_columns.extend(schema.columns.iter().map(|(prop, kind)| {
        let name = quote_ident(&column_name(prop));
        match kind {
            ColumnKind::Nested => format!("({name})::text"),
            _ => name,
        }
    }));
    format!(
        "SELECT {} FROM {} WHERE {} = $1",
        select_columns.join(", "),
        schema.quoted_table(),
        quote_ident(ID_COLUMN)
    )
}

/// A row must carry exactly the id column plus one column per property:
/// the driver returns the columns the select listed, so a mismatch means
/// the schema and the statement drifted apart.
fn check_row_width(
    type_name: &str,
    schema_columns: usize,
    row_columns: usize,
) -> Result<(), DbError> {
    if row_columns != schema_columns + 1 {
        return Err(DbError::Storage(format!(
            "row for {type_name} carries {row_columns} columns but the schema lists {schema_columns}"
        )));
    }
    Ok(())
}

/// Decodes a row's columns (skipping the id column) into the record's
/// values, in the schema's visit order.
fn decode_row(
    schema: &EntitySchema,
    key: &EntityKey,
    row: &postgres::Row,
) -> Result<EntityRecord, DbError> {
    check_row_width(&schema.type_name, schema.columns.len(), row.len())?;
    let mut values = Vec::with_capacity(schema.columns.len());
    for (index, (_, kind)) in schema.columns.iter().enumerate() {
        let column = index + 1;
        let value = match kind {
            ColumnKind::Int => PropertyValue::Int(row.get::<_, i64>(column)),
            ColumnKind::Uint => PropertyValue::Uint(row.get::<_, i64>(column) as u64),
            ColumnKind::Float => PropertyValue::Float(row.get::<_, f64>(column)),
            ColumnKind::String => PropertyValue::String(row.get::<_, String>(column)),
            ColumnKind::Blob => PropertyValue::Blob(row.get::<_, Vec<u8>>(column)),
            ColumnKind::Vector => PropertyValue::Vector(row.get::<_, Vec<f32>>(column)),
            ColumnKind::Nested => {
                let text = row.get::<_, String>(column);
                json::decode_from_str(&text)?
            }
        };
        values.push(value);
    }
    Ok(EntityRecord {
        type_id: key.type_id,
        db_id: key.db_id,
        values,
    })
}

/// The PostgreSQL backend: one client connection to the storage database.
pub struct PostgresStorage {
    client: Client,
}

impl std::fmt::Debug for PostgresStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresStorage").finish_non_exhaustive()
    }
}

impl PostgresStorage {
    /// Connects to the storage database (`libpq` connection string).
    pub fn connect(conninfo: &str) -> Result<Self, DbError> {
        let client = Client::connect(conninfo, postgres::NoTls).map_err(map_db_error)?;
        Ok(Self { client })
    }

    /// The underlying client, for the schema tools (BW-09-07).
    pub fn client_mut(&mut self) -> &mut Client {
        &mut self.client
    }

    /// Creates this schema's table if it is missing (the entity-mapping
    /// part of a `sync_db` run; the fixed tables and the dry-run report
    /// join with the BW-09-07 items).
    pub fn create_entity_table(&mut self, schema: &EntitySchema) -> Result<(), DbError> {
        self.client
            .execute(schema.create_table_ddl().as_str(), &[])
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Writes an entity row: the `REPLACE INTO` semantics of the
    /// reference's entity write. The record's values must be the schema's
    /// columns in visit order.
    pub fn put_entity(
        &mut self,
        schema: &EntitySchema,
        record: &EntityRecord,
    ) -> Result<(), DbError> {
        let sql = put_entity_sql(schema);
        let (params, nested_texts) = put_entity_params(schema, record)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
        self.client
            .execute(sql.as_str(), &refs)
            .map_err(map_db_error)?;
        drop(nested_texts);
        Ok(())
    }

    /// Reads an entity row back: `Ok(None)` is the reference's "row
    /// missing" outcome, not a failure.
    pub fn get_entity(
        &mut self,
        schema: &EntitySchema,
        key: &EntityKey,
    ) -> Result<Option<EntityRecord>, DbError> {
        if key.db_id < 0 {
            return Ok(None);
        }
        let row = self
            .client
            .query_opt(get_entity_sql(schema).as_str(), &[&key.db_id])
            .map_err(map_db_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        decode_row(schema, key, &row).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> EntitySchema {
        let values = vec![
            PropertyValue::String("cui".into()),
            PropertyValue::Int(-7),
            PropertyValue::Blob(vec![0]),
        ];
        EntitySchema::for_record("Account", &["name", "level", "secret"], &values).unwrap()
    }

    fn record(db_id: i64) -> EntityRecord {
        let mut record = EntityRecord::new(0);
        record.db_id = db_id;
        record.values = vec![
            PropertyValue::String("cui".into()),
            PropertyValue::Int(-7),
            PropertyValue::Blob(vec![0]),
        ];
        record
    }

    #[test]
    fn uint_values_must_fit_bigint() {
        assert_eq!(uint_to_bigint(0).unwrap(), 0);
        assert_eq!(uint_to_bigint(i64::MAX as u64).unwrap(), i64::MAX);
        let err = uint_to_bigint(u64::MAX).unwrap_err();
        assert!(err.to_string().contains("exceeds BIGINT"));
    }

    #[test]
    fn put_rejects_records_the_schema_cannot_carry() {
        // Arity mismatch.
        let mut short = record(1);
        short.values.truncate(1);
        let err = validate_put(&schema(), &short).unwrap_err();
        assert!(err.to_string().contains("carries 1 values"));

        // A pending database id is not a row key.
        let err = validate_put(&schema(), &record(-1)).unwrap_err();
        assert!(err.to_string().contains("pending database id"));

        // A uint64 over BIGINT cannot bind.
        let mut huge = record(1);
        huge.values[1] = PropertyValue::Uint(u64::MAX);
        let err = validate_put(&schema(), &huge).unwrap_err();
        assert!(err.to_string().contains("exceeds BIGINT"));
    }

    #[test]
    fn put_sql_upserts_on_the_id_key() {
        let sql = put_entity_sql(&schema());
        assert!(sql.starts_with(
            "INSERT INTO \"tbl_Account\" (\"id\", \"sm_name\", \"sm_level\", \"sm_secret\")"
        ));
        assert!(sql.contains("VALUES ($1, $2, $3, $4)"));
        assert!(sql.contains("ON CONFLICT (\"id\") DO UPDATE SET"));
        assert!(sql.contains("\"sm_name\" = EXCLUDED.\"sm_name\""));
    }

    #[test]
    fn put_sql_casts_nested_placeholders_to_jsonb() {
        let values = vec![
            PropertyValue::String("cui".into()),
            PropertyValue::Sequence(vec![PropertyValue::Int(2)]),
        ];
        let schema = EntitySchema::for_record("Monster", &["name", "loot"], &values).unwrap();
        let sql = put_entity_sql(&schema);
        assert!(sql.contains("VALUES ($1, $2, CAST($3::text AS JSONB))"));
    }

    #[test]
    fn put_params_bind_the_id_then_each_value() {
        let (params, nested) = put_entity_params(&schema(), &record(41)).unwrap();
        assert_eq!(params.len(), 4);
        assert!(nested.is_empty());
        // A schema with a nested column proves the nested-text leg.
        let values = vec![PropertyValue::FixedDict(None)];
        let schema = EntitySchema::for_record("Monster", &["extra"], &values).unwrap();
        let mut nested_record = EntityRecord::new(0);
        nested_record.db_id = 2;
        nested_record.values = values.clone();
        let (params, nested) = put_entity_params(&schema, &nested_record).unwrap();
        assert_eq!(params.len(), 2);
        assert_eq!(nested, vec!["{\"d\":null}".to_string()]);
    }

    #[test]
    fn the_row_width_check_catches_drift() {
        assert_eq!(check_row_width("Account", 3, 4).unwrap(), ());
        let err = check_row_width("Account", 3, 5).unwrap_err();
        assert!(err.to_string().contains("carries 5 columns"));
    }

    #[test]
    fn get_sql_selects_the_columns_in_visit_order() {
        let sql = get_entity_sql(&schema());
        assert_eq!(
            sql,
            concat!(
                "SELECT \"id\", \"sm_name\", \"sm_level\", \"sm_secret\" ",
                "FROM \"tbl_Account\" WHERE \"id\" = $1"
            )
        );

        let values = vec![PropertyValue::FixedDict(None)];
        let schema = EntitySchema::for_record("Monster", &["extra"], &values).unwrap();
        let sql = get_entity_sql(&schema);
        assert!(sql.contains("(\"sm_extra\")::text"));
    }
}
