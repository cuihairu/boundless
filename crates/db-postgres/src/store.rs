//! The PostgreSQL storage backend: entity rows in `tbl_<TypeName>`
//! (`lib/db_storage_mysql`, ported to PostgreSQL).
//!
//! The reference writes one row per entity: the DBID in the id column,
//! one column per persistent property, and a `REPLACE INTO` for the write
//! (create and update share the upsert — the row is keyed by the DBID).
//! The PostgreSQL port keeps the same shape: `INSERT ... ON CONFLICT
//! ("id") DO UPDATE` replaces the row, and `get_entity` reads the columns
//! back in the schema's visit order, one [`PropertyValue`] per column.
//!
//! The DBID allocation follows the reference's `putEntity` create leg:
//! `PutEntityTask` takes `insertNew` when the key carries no id
//! (`tasks/put_entity_task.cpp:60-98`), the generated statement lists
//! the property columns only (`utils.cpp:48-76`), and the caller reads
//! the DB's own choice back with `LAST_INSERT_ID` (`wrapper.hpp:75`).
//! The id column is the table's `AUTO_INCREMENT` (`column_type.hpp:144-146`,
//! `mysql_table_initialiser.cpp:366-369`), which is this backend's
//! identity column: the reference has no DBID watermark table of its own,
//! so the guarantee is the DBMS sequence's.

use std::cell::RefCell;
use std::collections::HashMap;

use db::{BaseMailbox, CheckOut, DbError, EntityKey, EntityRecord, EntityStorage};
use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId, PENDING_DATABASE_ID};
use mercury::endpoint::Address;
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
    map_unique_violation(error, DbError::DuplicateAccount)
}

/// Maps the error of an entity write: an entity table's only unique
/// constraint is its id, so a unique violation there is a duplicate
/// DBID — the contract's `AlreadyExists`, not the accounts table's
/// duplicate-name outcome.
pub fn map_write_error(error: postgres::Error) -> DbError {
    map_unique_violation(error, DbError::AlreadyExists)
}

fn map_unique_violation(error: postgres::Error, duplicate: DbError) -> DbError {
    let is_duplicate = error
        .code()
        .is_some_and(|code| *code == SqlState::UNIQUE_VIOLATION);
    if is_duplicate {
        duplicate
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
/// must be the schema's columns in visit order, and the DBID must be
/// assigned — a pending id is not a row key, the create leg takes one.
fn validate_put(schema: &EntitySchema, record: &EntityRecord) -> Result<(), DbError> {
    validate_values(schema, &record.values)?;
    if record.db_id < 0 {
        return Err(DbError::Storage(format!(
            "record for {} still has a pending database id",
            schema.type_name
        )));
    }
    Ok(())
}

/// Validates a value run against its schema's arity: one value per column,
/// in visit order.
fn validate_values(schema: &EntitySchema, values: &[PropertyValue]) -> Result<(), DbError> {
    if values.len() != schema.columns.len() {
        return Err(DbError::Storage(format!(
            "record for {} carries {} values but the schema lists {} columns",
            schema.type_name,
            values.len(),
            schema.columns.len()
        )));
    }
    Ok(())
}

/// Builds the upsert statement for a schema: the id column first, then one
/// placeholder per property column, nested columns cast to JSONB. The
/// `ON CONFLICT ("id") DO UPDATE` clause is the reference `REPLACE INTO`.
fn put_entity_sql(schema: &EntitySchema) -> String {
    let placeholders = put_entity_sql_placeholders(schema);
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
        placeholders,
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
    let (values, nested_texts) = value_params(&record.values)?;
    let mut params: Vec<PutParam> = Vec::with_capacity(values.len() + 1);
    params.push(Box::new(record.db_id));
    params.extend(values);
    Ok((params, nested_texts))
}

/// Builds the parameter list for a create: one placeholder per property,
/// in the schema's visit order, and no id parameter — the statement leaves
/// the column out for the identity to fill in.
fn create_entity_params(
    schema: &EntitySchema,
    values: &[PropertyValue],
) -> Result<(Vec<PutParam>, Vec<String>), DbError> {
    validate_values(schema, values)?;
    value_params(values)
}

/// The bound parameters for one run of property values. A `Uint` must fit
/// `BIGINT`, and nested values bind as their JSON text — the returned texts
/// keep that text alive for the borrow.
fn value_params(values: &[PropertyValue]) -> Result<(Vec<PutParam>, Vec<String>), DbError> {
    let mut params: Vec<PutParam> = Vec::with_capacity(values.len());
    let mut nested_texts: Vec<String> = Vec::new();
    for value in values {
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

/// The create statement (`insertNew`, `utils.cpp:48-76`): the property
/// columns only, the id column left out of the column list, and the
/// server's allocation read back. A table with no properties inserts
/// nothing but still asks for its id.
fn create_entity_sql(schema: &EntitySchema) -> String {
    let returning = format!("RETURNING {}", quote_ident(ID_COLUMN));
    if schema.columns.is_empty() {
        return format!(
            "INSERT INTO {} DEFAULT VALUES {}",
            schema.quoted_table(),
            returning
        );
    }
    let mut columns = Vec::with_capacity(schema.columns.len());
    let mut placeholders = Vec::with_capacity(schema.columns.len());
    for (index, (prop, kind)) in schema.columns.iter().enumerate() {
        columns.push(quote_ident(&column_name(prop)));
        let placeholder = format!("${}", index + 1);
        placeholders.push(match kind {
            // The param binds as text and casts server-side: an un-annotated
            // CAST would type-infer the parameter itself as jsonb, which the
            // text binding then cannot serialize.
            ColumnKind::Nested => format!("CAST({placeholder}::text AS JSONB)"),
            _ => placeholder,
        });
    }
    format!(
        "INSERT INTO {} ({}) VALUES ({}) {}",
        schema.quoted_table(),
        columns.join(", "),
        placeholders.join(", "),
        returning
    )
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

/// The whole-table select `all_entities` runs per registered type: the
/// same columns as [`get_entity_sql`], without the id filter.
fn select_all_entity_sql(schema: &EntitySchema) -> String {
    let mut select_columns = vec![quote_ident(ID_COLUMN)];
    select_columns.extend(schema.columns.iter().map(|(prop, kind)| {
        let name = quote_ident(&column_name(prop));
        match kind {
            ColumnKind::Nested => format!("({name})::text"),
            _ => name,
        }
    }));
    format!(
        "SELECT {} FROM {}",
        select_columns.join(", "),
        schema.quoted_table()
    )
}

/// The explicit-id insert (`put_entity_task.cpp:89-97` `insertExplicit`):
/// a plain INSERT, so a duplicate primary key fails the statement
/// ([`DbError::AlreadyExists`] via [`map_write_error`]) and the original
/// row survives — the upsert ([`put_entity_sql`]) would overwrite
/// instead. The id column is `GENERATED BY DEFAULT AS IDENTITY`, so an
/// explicit value needs no override clause.
fn insert_explicit_entity_sql(schema: &EntitySchema) -> String {
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        schema.quoted_table(),
        schema.quoted_columns().join(", "),
        put_entity_sql_placeholders(schema)
    )
}

/// The placeholder run for a write that carries the id: `$1` for the id,
/// then one per property column, nested columns cast to JSONB.
fn put_entity_sql_placeholders(schema: &EntitySchema) -> String {
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
    placeholders.join(", ")
}

/// The parameters for an update: the values first, the row key last.
fn update_entity_params(
    schema: &EntitySchema,
    record: &EntityRecord,
) -> Result<(Vec<PutParam>, Vec<String>), DbError> {
    validate_put(schema, record)?;
    let (values, nested_texts) = value_params(&record.values)?;
    let mut params = values;
    params.push(Box::new(record.db_id));
    Ok((params, nested_texts))
}

/// The update leg of the contract's put: an existing row is rewritten in
/// place, a missing one fails the statement — the XML backend's
/// `isOK=false` (`xml_database.cpp:617-620`). The upsert
/// ([`put_entity_sql`]) would create the row instead, so the contract's
/// update goes through this statement and reads the affected count.
fn update_entity_sql(schema: &EntitySchema) -> String {
    let assignments = schema
        .columns
        .iter()
        .enumerate()
        .map(|(index, (prop, kind))| {
            let placeholder = format!("${}", index + 1);
            let value = match kind {
                ColumnKind::Nested => format!("CAST({placeholder}::text AS JSONB)"),
                _ => placeholder,
            };
            format!("{} = {value}", quote_ident(&column_name(prop)))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "UPDATE {} SET {} WHERE {} = ${}",
        schema.quoted_table(),
        assignments,
        quote_ident(ID_COLUMN),
        schema.columns.len() + 1
    )
}

/// The contract's delete: the row by its key.
fn delete_entity_sql(schema: &EntitySchema) -> String {
    format!(
        "DELETE FROM {} WHERE {} = $1",
        schema.quoted_table(),
        quote_ident(ID_COLUMN)
    )
}

/// The row-existence probe the checkout runs first: no log-on record ever
/// points at a missing entity (`put_entity_task.cpp:103-108` checks
/// existence before adding an invalid log-on record).
fn row_exists_sql(schema: &EntitySchema) -> String {
    format!(
        "SELECT 1 FROM {} WHERE {} = $1",
        schema.quoted_table(),
        quote_ident(ID_COLUMN)
    )
}

/// The calibration statement (see [`PostgresStorage::calibrate_dbid_allocation`]):
/// moves the table's sequence to `max(id) + 1` and leaves it un-consumed,
/// so the next allocation is exactly that value. The table name goes in as
/// a parameter — `pg_get_serial_sequence` parses its arguments like
/// identifiers, so the name arrives already quoted.
fn calibrate_dbid_sql(schema: &EntitySchema) -> String {
    format!(
        "SELECT setval(pg_get_serial_sequence($1, 'id'), \
         (SELECT COALESCE(MAX({}), 0) + 1 FROM {}), false)",
        quote_ident(ID_COLUMN),
        schema.quoted_table()
    )
}

/// The logon-record statements. `bigworldLogOns` is a fixed table — the
/// database initialisation creates it — and `entity_type_mapping.cpp`
/// carries the reference's queries. A relogon overwrites: `addLogOnQuery`
/// is `INSERT ... ON DUPLICATE KEY UPDATE`
/// (entity_type_mapping.cpp:749-758), so the port is an `ON CONFLICT`
/// upsert on the same two-column key.
fn add_logon_sql() -> String {
    "INSERT INTO \"bigworldLogOns\" (\"databaseID\", \"typeID\", \"objectID\", \"ip\", \"port\", \"salt\") \
     VALUES ($1, $2, $3, $4, $5, $6) \
     ON CONFLICT (\"typeID\", \"databaseID\") DO UPDATE SET \
     \"objectID\" = EXCLUDED.\"objectID\", \"ip\" = EXCLUDED.\"ip\", \
     \"port\" = EXCLUDED.\"port\", \"salt\" = EXCLUDED.\"salt\""
        .to_string()
}

/// `getLogOnQuery` (entity_type_mapping.cpp:764-766): the holder of an
/// entity, if any.
fn get_logon_sql() -> String {
    "SELECT \"objectID\", \"ip\", \"port\", \"salt\" \
     FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1 AND \"typeID\" = $2"
        .to_string()
}

/// `removeLogOnQuery` (entity_type_mapping.cpp:527-529): the reference
/// deletes unconditionally, so releasing nothing held is fine.
fn remove_logon_sql() -> String {
    "DELETE FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1 AND \"typeID\" = $2".to_string()
}

/// `updateLogOnAutoLoadQuery` (entity_type_mapping.cpp:761,
/// `:793-797`): the auto-load status of an entity's logon record. An
/// entity without a record matches no rows and the update stays silent
/// — the reference's MySQL UPDATE semantics.
fn update_auto_load_sql() -> String {
    "UPDATE \"bigworldLogOns\" SET \"shouldAutoLoad\" = $1 \
     WHERE \"databaseID\" = $2 AND \"typeID\" = $3"
        .to_string()
}

/// `autoLoadEntities`' cleanup leg (`mysql_database.cpp:674-675`): the
/// sessions of the entities that must not auto-load are stale at a
/// restart and go first.
fn clear_non_autoload_sql() -> String {
    "DELETE FROM \"bigworldLogOns\" WHERE NOT \"shouldAutoLoad\"".to_string()
}

/// `autoLoadEntities`' listing leg (`mysql_database.cpp:677-683`): the
/// flagged entities joined with the meta table for the running-server
/// type id — `typeID` is the id used internally by the database,
/// `bigworldID` the one used by a running server.
fn auto_load_list_sql() -> String {
    "SELECT logOn.\"databaseID\", entityType.\"bigworldID\" \
     FROM \"bigworldLogOns\" logOn, \"bigworldEntityTypes\" entityType \
     WHERE logOn.\"typeID\" = entityType.\"typeID\" AND logOn.\"shouldAutoLoad\""
        .to_string()
}

/// `autoLoadEntities`' holder leg (`mysql_database.cpp:702`): the
/// restart hands every loaded entity to a fresh base, so the stored
/// addresses are stale — they zero, while `objectID` and `salt` stay.
fn clear_holders_sql() -> String {
    "UPDATE \"bigworldLogOns\" SET \"ip\" = 0, \"port\" = 0".to_string()
}

/// `getEntityTypeIDQuery` (`bw_meta_data.cpp:13`): the running-server
/// id of a type, by name.
fn get_entity_type_id_sql() -> &'static str {
    "SELECT \"bigworldID\" FROM \"bigworldEntityTypes\" WHERE \"name\" = $1"
}

/// `addEntityTypeQuery` (`bw_meta_data.cpp:19-21`): the meta row the
/// auto-load join resolves through. The reference's two type-id spaces
/// — the database-internal `typeID` and the `entities.xml` `bigworldID`
/// — collapse into the port's one [`EntityTypeId`] space, so both
/// columns carry the caller's value.
fn add_entity_type_sql() -> String {
    "INSERT INTO \"bigworldEntityTypes\" (\"typeID\", \"bigworldID\", \"name\") \
     VALUES ($1, $1, $2)"
        .to_string()
}

/// `setEntityTypeIDQuery` (`bw_meta_data.cpp:16`): the reference updates
/// `bigworldID` alone — its internal `typeID` is stable, so old log-on
/// records keep joining after a re-point. The port's one type-id space
/// carries the running-server id in both columns, so both move
/// together.
fn set_entity_type_id_sql() -> String {
    "UPDATE \"bigworldEntityTypes\" SET \"typeID\" = $1, \"bigworldID\" = $1 \
     WHERE \"name\" = $2"
        .to_string()
}

/// The reference stores the base address in network byte order:
/// `htonl(mailbox.addr.ip)` and `htons(mailbox.addr.port)` go in
/// (`addLogOnRecord`, entity_type_mapping.cpp:779-781) and `ntohl`/`ntohs`
/// come back (`getLogOnRecord`, :815-816) — a byte swap on both legs,
/// which the port keeps.
fn stored_ip(ip: u32) -> i64 {
    i64::from(ip.swap_bytes())
}

fn stored_port(port: u16) -> i32 {
    i32::from(port.swap_bytes())
}

/// The reference's `salt` column is `SMALLINT UNSIGNED`, and the ported
/// address type carries a `u16` salt — the INTEGER column holds it whole.
fn stored_salt(salt: u16) -> i32 {
    i32::from(salt)
}

/// `objectID` is the INTEGER entity id (`INT` in the reference) and `ip`
/// the BIGINT the swapped `u32` needs — see the `bigworldLogOns` DDL in
/// `schema_init`.
fn read_ip(raw: i64) -> u32 {
    (raw as u32).swap_bytes()
}

fn read_port(raw: i32) -> u16 {
    (raw as u16).swap_bytes()
}

fn read_salt(raw: i32) -> u16 {
    raw as u16
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
///
/// The client sits in a `RefCell` because the storage contract
/// ([`EntityStorage`]) reads through `&self` — the shared shape of the
/// in-memory and XML backends — while `postgres::Client` runs its
/// statements through `&mut self`.
pub struct PostgresStorage {
    client: RefCell<Client>,
    /// The entity types this connection knows: `EntityTypeId -> table
    /// shape`. The reference keeps one `EntityTypeMapping` per type and
    /// indexes them by id (`entityTypeMappings_`); the contract
    /// operations resolve the table shape through the same map.
    schemas: HashMap<EntityTypeId, EntitySchema>,
    /// Whether this connection is inside a transaction the reference
    /// would be holding as an open `MySqlTransaction`
    /// (`transaction.cpp:15-23` keeps `inTransaction` on the connection).
    in_transaction: bool,
    /// The stored connection string, `connectInfo_` (`wrapper.hpp:111`) —
    /// [`PostgresStorage::reconnect`] re-runs it.
    conninfo: String,
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
        Ok(Self {
            client: RefCell::new(client),
            schemas: HashMap::new(),
            in_transaction: false,
            conninfo: conninfo.to_string(),
        })
    }

    /// Replaces this connection with a fresh one to the same database:
    /// the reference's `MySql` keeps `connectInfo_` for exactly this
    /// (`wrapper.hpp:111`), so a connection the server dropped comes
    /// back without the caller knowing the string. The registered
    /// entity types survive — they are the connection's own knowledge
    /// of the table shapes, not server state — and a transaction that
    /// was open on the dead connection is gone with it, the way the
    /// reference's `hasLostConnection` flag abandons one.
    pub fn reconnect(&mut self) -> Result<(), DbError> {
        let client =
            Client::connect(self.conninfo.as_str(), postgres::NoTls).map_err(map_db_error)?;
        self.client = RefCell::new(client);
        self.in_transaction = false;
        Ok(())
    }

    /// The underlying client, for the schema tools (BW-09-07).
    pub fn client_mut(&mut self) -> &mut Client {
        self.client.get_mut()
    }

    /// Registers an entity type with this connection: the contract
    /// operations ([`EntityStorage`]) resolve the table shape by
    /// `EntityTypeId`, the way the reference's `entityTypeMappings_`
    /// vector indexes each type's mapping. Registration is in-memory —
    /// the table itself is [`PostgresStorage::create_entity_table`]'s
    /// face.
    pub fn register_entity_type(&mut self, type_id: EntityTypeId, schema: &EntitySchema) {
        self.schemas.insert(type_id, schema.clone());
    }

    /// The registered shape of `type_id`, if this connection knows it.
    /// The reference indexes `entityTypeMappings_` unguarded; the port
    /// checks and lets each operation pick its own outcome for the
    /// unknown type (a get answers "no row", a put fails, a delete and a
    /// checkout report "not found").
    pub fn schema_of(&self, type_id: EntityTypeId) -> Option<&EntitySchema> {
        self.schemas.get(&type_id)
    }

    /// Opens a server-side transaction on this connection: the reference's
    /// `MySqlTransaction` constructor issues `START TRANSACTION` and only
    /// then marks the connection as being inside one
    /// (`transaction.cpp:15-23` — the flag is last because the execute can
    /// fail). Every statement this connection runs afterwards belongs to
    /// the transaction: another connection sees none of it until
    /// [`PostgresStorage::commit`].
    pub fn begin_transaction(&mut self) -> Result<(), DbError> {
        // The reference only ever has one transaction per connection
        // (the constructor sets `inTransaction`), so a second begin is
        // refused rather than left to PostgreSQL's warning-and-no-op.
        if self.in_transaction {
            return Err(DbError::Storage(
                "the connection already has a transaction open".into(),
            ));
        }
        self.client
            .borrow_mut()
            .batch_execute("BEGIN")
            .map_err(map_db_error)?;
        self.in_transaction = true;
        Ok(())
    }

    /// Commits the open transaction: every write this connection made
    /// since [`PostgresStorage::begin_transaction`] becomes visible to
    /// the other connections at once.
    pub fn commit(&mut self) -> Result<(), DbError> {
        if !self.in_transaction {
            return Err(DbError::Storage(
                "the connection has no transaction open".into(),
            ));
        }
        self.client
            .borrow_mut()
            .batch_execute("COMMIT")
            .map_err(map_db_error)?;
        self.in_transaction = false;
        Ok(())
    }

    /// Rolls the open transaction back: the entity rows and the
    /// associated records written since
    /// [`PostgresStorage::begin_transaction`] never happened, and the
    /// other connections keep reading the old values.
    ///
    /// On a lost connection the reference skips the `ROLLBACK` — there is
    /// no connection left to send it to — and just drops the flag
    /// (`transaction.cpp:52-59`); the port treats a failed `ROLLBACK` the
    /// same way.
    pub fn rollback(&mut self) -> Result<(), DbError> {
        if !self.in_transaction {
            return Err(DbError::Storage(
                "the connection has no transaction open".into(),
            ));
        }
        self.in_transaction = false;
        self.client
            .borrow_mut()
            .batch_execute("ROLLBACK")
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Writes the logon record for `key` (`addLogOnRecord`,
    /// entity_type_mapping.cpp:773-783): the reference's statement is
    /// `INSERT ... ON DUPLICATE KEY UPDATE`, so a relogon overwrites the
    /// record instead of failing on the duplicate key. This is the raw
    /// face — the contract's [`EntityStorage::check_out`] wraps it with
    /// the occupancy grant.
    pub fn put_logon_record(
        &mut self,
        key: &EntityKey,
        mailbox: &BaseMailbox,
    ) -> Result<(), DbError> {
        self.client
            .borrow_mut()
            .execute(
                add_logon_sql().as_str(),
                &[
                    &key.db_id,
                    &i32::from(key.type_id),
                    &mailbox.entity_id,
                    &stored_ip(mailbox.addr.ip),
                    &stored_port(mailbox.addr.port),
                    &stored_salt(mailbox.addr.salt),
                ],
            )
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Writes the `bigworldEntityTypes` row the auto-load join resolves
    /// through — the sync pass's `TypesCollector::addType`
    /// (table_inspector.cpp:250-272) driving `BigWorldMetaData`
    /// (bw_meta_data.cpp:42-70): absent by name the row is added, a
    /// different id re-points it, the same id is a no-op.
    pub fn add_entity_type(&mut self, type_id: EntityTypeId, name: &str) -> Result<(), DbError> {
        let current = self
            .client
            .borrow_mut()
            .query_opt(get_entity_type_id_sql(), &[&name])
            .map_err(map_db_error)?;
        let id = i64::from(type_id);
        match current.map(|row| row.get::<_, i64>(0)) {
            None => {
                self.client
                    .borrow_mut()
                    .execute(add_entity_type_sql().as_str(), &[&id, &name])
                    .map_err(map_db_error)?;
            }
            Some(stored) if stored != id => {
                self.client
                    .borrow_mut()
                    .execute(set_entity_type_id_sql().as_str(), &[&id, &name])
                    .map_err(map_db_error)?;
            }
            Some(_) => {}
        }
        Ok(())
    }

    /// Recalibrates the table's identity sequence after explicit ids were
    /// written (the import and the restore): an identity column does not
    /// advance for a value given outright, so the first create after the
    /// pass would hand back an id an imported row already holds — the
    /// duplicate-primary-key outcome of `insertExplicit`
    /// (put_entity_task.cpp:93-97). The reference never needs the step:
    /// its `AUTO_INCREMENT` state lives in the table itself and travels
    /// with the copied data, and its only row-merge path replays rows
    /// into a live primary (consolidate_entity_task.cpp:27-34), so this
    /// is the port's own seam. `setval` runs with `is_called` false, so
    /// the next `nextval` returns exactly `max(id) + 1` rather than
    /// skipping a value; an empty table recalibrates to its start value.
    /// Returns the next allocatable DBID.
    pub fn calibrate_dbid_allocation(
        &mut self,
        schema: &EntitySchema,
    ) -> Result<DatabaseId, DbError> {
        let row = self
            .client
            .borrow_mut()
            .query_one(
                calibrate_dbid_sql(schema).as_str(),
                &[&schema.quoted_table()],
            )
            .map_err(map_db_error)?;
        Ok(row.get::<_, i64>(0))
    }

    /// Sets the auto-load status of an entity's logon record
    /// (`EntityTypeMapping::updateAutoLoad`, entity_type_mapping.cpp:792-800).
    /// An entity without a record matches no rows and the update stays
    /// silent — the task that carries the flag writes it right after the
    /// record itself (put_entity_task.cpp:117-121).
    pub fn set_auto_load(
        &mut self,
        key: &EntityKey,
        should_auto_load: bool,
    ) -> Result<(), DbError> {
        self.client
            .borrow_mut()
            .execute(
                update_auto_load_sql().as_str(),
                &[&should_auto_load, &key.db_id, &i32::from(key.type_id)],
            )
            .map_err(map_db_error)?;
        Ok(())
    }

    /// The restart face (`MySqlDatabase::autoLoadEntities`,
    /// mysql_database.cpp:667-713): the sessions of the entities that
    /// must not auto-load are stale and go first, the flagged entities
    /// are listed for the auto-loader — running-server type id first,
    /// then `databaseID`, the `autoLoader.addEntity` order — and when
    /// the list is non-empty the stored holders zero (`ip = 0,
    /// port = 0`). The caller feeds the list to the loader's
    /// `reserve`/`addEntity` and drives `start` — the auto-load recovery
    /// seam.
    pub fn auto_load_entities(&mut self) -> Result<Vec<(EntityTypeId, DatabaseId)>, DbError> {
        self.client
            .borrow_mut()
            .execute(clear_non_autoload_sql().as_str(), &[])
            .map_err(map_db_error)?;
        let rows = self
            .client
            .borrow_mut()
            .query(auto_load_list_sql().as_str(), &[])
            .map_err(map_db_error)?;
        let mut list = Vec::with_capacity(rows.len());
        for row in &rows {
            list.push((row.get::<_, i64>(1) as EntityTypeId, row.get::<_, i64>(0)));
        }
        if !list.is_empty() {
            self.client
                .borrow_mut()
                .execute(clear_holders_sql().as_str(), &[])
                .map_err(map_db_error)?;
        }
        Ok(list)
    }

    /// Creates this schema's table if it is missing (the entity-mapping
    /// part of a `sync_db` run; the fixed tables and the dry-run report
    /// join with the BW-09-07 items).
    pub fn create_entity_table(&self, schema: &EntitySchema) -> Result<(), DbError> {
        self.client
            .borrow_mut()
            .execute(schema.create_table_ddl().as_str(), &[])
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Writes an entity row: the `REPLACE INTO` semantics of the
    /// reference's entity write. The record's values must be the schema's
    /// columns in visit order.
    pub fn put_entity(&self, schema: &EntitySchema, record: &EntityRecord) -> Result<(), DbError> {
        let sql = put_entity_sql(schema);
        let (params, nested_texts) = put_entity_params(schema, record)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
        self.client
            .borrow_mut()
            .execute(sql.as_str(), &refs)
            .map_err(map_db_error)?;
        drop(nested_texts);
        Ok(())
    }

    /// Writes a new entity row and allocates its DBID: the reference's
    /// `putEntity` create leg, where the key carries no id yet
    /// (`put_entity_task.cpp:60-98` → `insertNew`). The statement lists
    /// the property columns only, so the table's own sequence assigns the
    /// id; `RETURNING "id"` is this port's `LAST_INSERT_ID` read-back
    /// (`wrapper.hpp:75`).
    ///
    /// The allocation is safe under concurrency and across a restart
    /// because the sequence is the table's own catalog state: `nextval`
    /// advances it atomically, and it is not in-process state, so a
    /// reopened connection continues where the table left off. An id
    /// inserted explicitly does not advance it (the reference is the same:
    /// MySQL does not move `AUTO_INCREMENT` for an explicit value), so
    /// importing rows must recalibrate it afterwards.
    pub fn create_entity(
        &self,
        schema: &EntitySchema,
        values: &[PropertyValue],
    ) -> Result<DatabaseId, DbError> {
        let sql = create_entity_sql(schema);
        let (params, nested_texts) = create_entity_params(schema, values)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
        let row = self
            .client
            .borrow_mut()
            .query_one(sql.as_str(), &refs)
            .map_err(map_write_error)?;
        drop(nested_texts);
        Ok(row.get::<_, i64>(0))
    }

    /// Reads an entity row back: `Ok(None)` is the reference's "row
    /// missing" outcome, not a failure.
    pub fn get_entity(
        &self,
        schema: &EntitySchema,
        key: &EntityKey,
    ) -> Result<Option<EntityRecord>, DbError> {
        if key.db_id < 0 {
            return Ok(None);
        }
        let row = self
            .client
            .borrow_mut()
            .query_opt(get_entity_sql(schema).as_str(), &[&key.db_id])
            .map_err(map_db_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        decode_row(schema, key, &row).map(Some)
    }
}

/// The storage contract over PostgreSQL (BW-09-06-05): the shared shape
/// the in-memory and XML backends answer, so the DBApp's entity
/// operations run unchanged against this backend. Every operation
/// resolves the row's table through the registered schema
/// ([`PostgresStorage::register_entity_type`]); an unregistered type has
/// no table, which each operation folds into its own outcome.
impl EntityStorage for PostgresStorage {
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
        // An unknown type has no table: the "no row" outcome. The shape
        // is cloned out of the registry so the operation runs without
        // the borrow.
        let Some(schema) = self.schema_of(key.type_id).cloned() else {
            return Ok(None);
        };
        PostgresStorage::get_entity(self, &schema, key)
    }

    fn put_entity(
        &mut self,
        key: &EntityKey,
        values: &[PropertyValue],
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        let Some(schema) = self.schema_of(key.type_id).cloned() else {
            // The XML backend's unknown-type rejection.
            return Err(DbError::Storage(format!(
                "unknown entity type {type_id}",
                type_id = key.type_id
            )));
        };
        let record = EntityRecord {
            type_id: key.type_id,
            db_id: key.db_id,
            values: values.to_vec(),
        };

        if key.db_id == PENDING_DATABASE_ID {
            // The create leg: the reference turns the pending id into 0
            // and the table's key column hands the id out
            // (buffered_entity_tasks.cpp:316-320, put_entity_task.cpp:85-88).
            return self.create_entity(&schema, values);
        }

        if put_explicit_id {
            // `insertExplicit`: a plain INSERT, so a duplicate id fails
            // and the original row survives (put_entity_task.cpp:89-97).
            let sql = insert_explicit_entity_sql(&schema);
            let (params, nested_texts) = put_entity_params(&schema, &record)?;
            let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
            self.client
                .borrow_mut()
                .execute(sql.as_str(), &refs)
                .map_err(map_write_error)?;
            drop(nested_texts);
            return Ok(key.db_id);
        }

        // The update leg: a missing row is the XML backend's
        // `isOK=false` (xml_database.cpp:617-620) — the affected count
        // is the existence check.
        let sql = update_entity_sql(&schema);
        let (params, nested_texts) = update_entity_params(&schema, &record)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
        let updated = self
            .client
            .borrow_mut()
            .execute(sql.as_str(), &refs)
            .map_err(map_db_error)?;
        drop(nested_texts);
        if updated == 0 {
            return Err(DbError::NotFound);
        }
        Ok(key.db_id)
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        let Some(schema) = self.schema_of(key.type_id).cloned() else {
            return Err(DbError::NotFound);
        };
        let sql = delete_entity_sql(&schema);
        let deleted = self
            .client
            .borrow_mut()
            .execute(sql.as_str(), &[&key.db_id])
            .map_err(map_db_error)?;
        if deleted == 0 {
            // `deleteWithID` fails without touching the log-on record
            // (del_entity_task.cpp:69-74).
            return Err(DbError::NotFound);
        }
        // On success the log-on record goes with the row
        // (del_entity_task.cpp:76).
        self.client
            .borrow_mut()
            .execute(
                remove_logon_sql().as_str(),
                &[&key.db_id, &i32::from(key.type_id)],
            )
            .map_err(map_db_error)?;
        Ok(())
    }

    fn logon_of(&self, key: &EntityKey) -> Result<Option<BaseMailbox>, DbError> {
        let row = self
            .client
            .borrow_mut()
            .query_opt(
                get_logon_sql().as_str(),
                &[&key.db_id, &i32::from(key.type_id)],
            )
            .map_err(map_db_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        // `getResult(...)` then the ntohl/ntohs back-conversion
        // (entity_type_mapping.cpp:811-816).
        Ok(Some(BaseMailbox {
            addr: Address {
                ip: read_ip(row.get::<_, i64>(1)),
                port: read_port(row.get::<_, i32>(2)),
                salt: read_salt(row.get::<_, i32>(3)),
            },
            entity_id: row.get::<_, i32>(0),
        }))
    }

    fn check_out(&mut self, key: &EntityKey, mailbox: BaseMailbox) -> Result<CheckOut, DbError> {
        // No log-on record ever points at a missing row
        // (put_entity_task.cpp:103-108 checks existence first).
        let Some(schema) = self.schema_of(key.type_id).cloned() else {
            return Err(DbError::NotFound);
        };
        let exists_sql = row_exists_sql(&schema);
        let found = self
            .client
            .borrow_mut()
            .query_opt(exists_sql.as_str(), &[&key.db_id])
            .map_err(map_db_error)?;
        if found.is_none() {
            return Err(DbError::NotFound);
        }
        if let Some(holder) = self.logon_of(key)? {
            return Ok(CheckOut::Elsewhere(holder));
        }
        // The grant records the holder with the reference's own upsert.
        self.put_logon_record(key, &mailbox)?;
        Ok(CheckOut::Granted)
    }

    fn check_in(&mut self, key: &EntityKey) -> Result<(), DbError> {
        // removeLogOnRecord deletes unconditionally
        // (entity_type_mapping.cpp:533-535).
        self.client
            .borrow_mut()
            .execute(
                remove_logon_sql().as_str(),
                &[&key.db_id, &i32::from(key.type_id)],
            )
            .map_err(map_db_error)?;
        Ok(())
    }

    fn all_entities(&self) -> Result<Vec<EntityRecord>, DbError> {
        // The reference queries per type (each `EntityTypeMapping` builds
        // its own SELECT); the registry is the type list.
        let mut entries: Vec<(EntityTypeId, EntitySchema)> = self
            .schemas
            .iter()
            .map(|(type_id, schema)| (*type_id, schema.clone()))
            .collect();
        entries.sort_by_key(|(type_id, _)| *type_id);
        let mut records = Vec::new();
        for (type_id, schema) in &entries {
            let sql = select_all_entity_sql(schema);
            let rows = self
                .client
                .borrow_mut()
                .query(sql.as_str(), &[])
                .map_err(map_db_error)?;
            for row in &rows {
                let key = EntityKey {
                    type_id: *type_id,
                    db_id: row.get::<_, i64>(0),
                };
                records.push(decode_row(schema, &key, row)?);
            }
        }
        records.sort_by_key(|record| (record.type_id, record.db_id));
        Ok(records)
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

        // A uint64 over BIGINT cannot bind; that check lives where the
        // parameters are built, and the create leg runs it the same way.
        let mut huge = record(1);
        huge.values[1] = PropertyValue::Uint(u64::MAX);
        let err = put_entity_params(&schema(), &huge).unwrap_err();
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
    fn create_sql_leaves_the_id_column_out() {
        // `insertNew`: the property columns only, and the id read back.
        let sql = create_entity_sql(&schema());
        assert_eq!(
            sql,
            concat!(
                "INSERT INTO \"tbl_Account\" ",
                "(\"sm_name\", \"sm_level\", \"sm_secret\") ",
                "VALUES ($1, $2, $3) RETURNING \"id\""
            )
        );
    }

    #[test]
    fn create_sql_casts_nested_placeholders_to_jsonb() {
        let values = vec![
            PropertyValue::String("cui".into()),
            PropertyValue::Sequence(vec![PropertyValue::Int(2)]),
        ];
        let schema = EntitySchema::for_record("Monster", &["name", "loot"], &values).unwrap();
        assert_eq!(
            create_entity_sql(&schema),
            "INSERT INTO \"tbl_Monster\" (\"sm_name\", \"sm_loot\") \
             VALUES ($1, CAST($2::text AS JSONB)) RETURNING \"id\""
        );
    }

    #[test]
    fn a_propertyless_table_inserts_a_default_row() {
        let schema = EntitySchema::for_record("Empty", &[], &[]).unwrap();
        assert_eq!(
            create_entity_sql(&schema),
            "INSERT INTO \"tbl_Empty\" DEFAULT VALUES RETURNING \"id\""
        );
    }

    #[test]
    fn create_params_carry_the_values_without_an_id() {
        // One parameter per column: the statement has no id placeholder, so
        // the numbering starts where the put statement's properties begin.
        let (params, nested) = create_entity_params(&schema(), &record(0).values).unwrap();
        assert_eq!(params.len(), 3);
        assert!(nested.is_empty());

        let mut short = record(0);
        short.values.truncate(1);
        let err = create_entity_params(&schema(), &short.values).unwrap_err();
        assert!(err.to_string().contains("carries 1 values"));
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

    #[test]
    fn update_sql_rewrites_the_columns_and_reports_the_key() {
        assert_eq!(
            update_entity_sql(&schema()),
            concat!(
                "UPDATE \"tbl_Account\" SET \"sm_name\" = $1, \"sm_level\" = $2, ",
                "\"sm_secret\" = $3 WHERE \"id\" = $4"
            )
        );

        // A nested column casts its text placeholder server-side, like the
        // insert statements do.
        let values = vec![
            PropertyValue::String("cui".into()),
            PropertyValue::Sequence(vec![PropertyValue::Int(2)]),
        ];
        let schema = EntitySchema::for_record("Monster", &["name", "loot"], &values).unwrap();
        assert_eq!(
            update_entity_sql(&schema),
            concat!(
                "UPDATE \"tbl_Monster\" SET \"sm_name\" = $1, ",
                "\"sm_loot\" = CAST($2::text AS JSONB) WHERE \"id\" = $3"
            )
        );
    }

    #[test]
    fn update_params_carry_the_values_then_the_row_key() {
        let (params, nested) = update_entity_params(&schema(), &record(41)).unwrap();
        assert_eq!(params.len(), 4);
        assert!(nested.is_empty());

        let mut short = record(41);
        short.values.truncate(1);
        let err = update_entity_params(&schema(), &short).unwrap_err();
        assert!(err.to_string().contains("carries 1 values"));

        let err = update_entity_params(&schema(), &record(-1)).unwrap_err();
        assert!(err.to_string().contains("pending database id"));
    }

    #[test]
    fn explicit_insert_sql_is_a_plain_insert() {
        assert_eq!(
            insert_explicit_entity_sql(&schema()),
            concat!(
                "INSERT INTO \"tbl_Account\" (\"id\", \"sm_name\", \"sm_level\", \"sm_secret\") ",
                "VALUES ($1, $2, $3, $4)"
            )
        );
    }

    #[test]
    fn select_all_sql_lists_the_columns_without_a_filter() {
        assert_eq!(
            select_all_entity_sql(&schema()),
            "SELECT \"id\", \"sm_name\", \"sm_level\", \"sm_secret\" FROM \"tbl_Account\""
        );
    }

    #[test]
    fn delete_and_row_exists_sql_target_the_id_key() {
        assert_eq!(
            delete_entity_sql(&schema()),
            "DELETE FROM \"tbl_Account\" WHERE \"id\" = $1"
        );
        assert_eq!(
            row_exists_sql(&schema()),
            "SELECT 1 FROM \"tbl_Account\" WHERE \"id\" = $1"
        );
    }

    #[test]
    fn the_calibration_moves_the_sequence_past_the_last_explicit_id() {
        assert_eq!(
            calibrate_dbid_sql(&schema()),
            concat!(
                "SELECT setval(pg_get_serial_sequence($1, 'id'), ",
                "(SELECT COALESCE(MAX(\"id\"), 0) + 1 FROM \"tbl_Account\"), false)"
            )
        );
    }

    #[test]
    fn logon_statements_match_the_reference_queries() {
        assert_eq!(
            add_logon_sql(),
            concat!(
                "INSERT INTO \"bigworldLogOns\" (\"databaseID\", \"typeID\", \"objectID\", ",
                "\"ip\", \"port\", \"salt\") VALUES ($1, $2, $3, $4, $5, $6) ",
                "ON CONFLICT (\"typeID\", \"databaseID\") DO UPDATE SET ",
                "\"objectID\" = EXCLUDED.\"objectID\", \"ip\" = EXCLUDED.\"ip\", ",
                "\"port\" = EXCLUDED.\"port\", \"salt\" = EXCLUDED.\"salt\""
            )
        );
        assert_eq!(
            get_logon_sql(),
            concat!(
                "SELECT \"objectID\", \"ip\", \"port\", \"salt\" ",
                "FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1 AND \"typeID\" = $2"
            )
        );
        assert_eq!(
            remove_logon_sql(),
            "DELETE FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1 AND \"typeID\" = $2"
        );
    }

    #[test]
    fn auto_load_statements_match_the_reference_queries() {
        assert_eq!(
            update_auto_load_sql(),
            concat!(
                "UPDATE \"bigworldLogOns\" SET \"shouldAutoLoad\" = $1 ",
                "WHERE \"databaseID\" = $2 AND \"typeID\" = $3"
            )
        );
        assert_eq!(
            clear_non_autoload_sql(),
            "DELETE FROM \"bigworldLogOns\" WHERE NOT \"shouldAutoLoad\""
        );
        assert_eq!(
            auto_load_list_sql(),
            concat!(
                "SELECT logOn.\"databaseID\", entityType.\"bigworldID\" ",
                "FROM \"bigworldLogOns\" logOn, \"bigworldEntityTypes\" entityType ",
                "WHERE logOn.\"typeID\" = entityType.\"typeID\" AND logOn.\"shouldAutoLoad\""
            )
        );
        assert_eq!(
            clear_holders_sql(),
            "UPDATE \"bigworldLogOns\" SET \"ip\" = 0, \"port\" = 0"
        );
        assert_eq!(
            get_entity_type_id_sql(),
            "SELECT \"bigworldID\" FROM \"bigworldEntityTypes\" WHERE \"name\" = $1"
        );
        assert_eq!(
            add_entity_type_sql(),
            concat!(
                "INSERT INTO \"bigworldEntityTypes\" (\"typeID\", \"bigworldID\", \"name\") ",
                "VALUES ($1, $1, $2)"
            )
        );
        assert_eq!(
            set_entity_type_id_sql(),
            concat!(
                "UPDATE \"bigworldEntityTypes\" SET \"typeID\" = $1, \"bigworldID\" = $1 ",
                "WHERE \"name\" = $2"
            )
        );
    }

    #[test]
    fn the_address_swaps_bytes_on_both_legs() {
        // htonl/htons in, ntohl/ntohs back: high bits move to the other
        // end and survive the round trip.
        for ip in [0x0102_0304, 0xff00_00aa] {
            assert_eq!(stored_ip(ip) as u32, ip.swap_bytes());
            assert_eq!(read_ip(stored_ip(ip)), ip);
        }
        assert_eq!(stored_ip(0x0102_0304), 0x0403_0201u64 as i64);

        for port in [0x1234, 0xabcd] {
            assert_eq!(stored_port(port) as u16, port.swap_bytes());
            assert_eq!(read_port(stored_port(port)), port);
        }
        // The salt is stored whole — no swap on either leg.
        assert_eq!(stored_salt(0xbeef), 0xbeef);
        assert_eq!(read_salt(stored_salt(0xbeef)), 0xbeef);
    }

    #[test]
    fn the_logon_record_params_follow_the_column_order() {
        // The parameter run is (databaseID, typeID, objectID, ip, port,
        // salt); the shapes must be the columns'. This exercises the
        // statement's bind site without a server.
        let key = EntityKey {
            type_id: 7,
            db_id: 41,
        };
        let mailbox = BaseMailbox {
            addr: Address {
                ip: 0x0102_0304,
                port: 0x1234,
                salt: 9,
            },
            entity_id: -3,
        };
        // The conversions the parameters run through.
        let params = (
            key.db_id,
            i32::from(key.type_id),
            mailbox.entity_id,
            stored_ip(mailbox.addr.ip),
            stored_port(mailbox.addr.port),
            stored_salt(mailbox.addr.salt),
        );
        assert_eq!(params.0, 41);
        assert_eq!(params.1, 7);
        assert_eq!(params.2, -3);
        assert_eq!(params.3, 0x0403_0201u64 as i64);
        assert_eq!(params.4 as u16, 0x3412);
        assert_eq!(params.5, 9);
    }
}
