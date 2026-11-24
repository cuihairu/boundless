//! The PostgreSQL storage backend: entity rows in `tbl_<TypeName>`
//! (`lib/db_storage_mysql`, ported to PostgreSQL).
//!
//! The reference writes one row per entity: the DBID in the id column,
//! one column per persistent property, and a `REPLACE INTO` for the write
//! (create and update share the upsert — the row is keyed by the DBID).
//! The PostgreSQL port keeps the same shape: `INSERT ... ON CONFLICT
//! ("id") DO UPDATE` replaces the row, and `get_entity` reads the columns
//! back in the schema's visit order, one [`PropertyValue`] per column.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;

use db::{BaseMailbox, CheckOut, DbError, EntityKey, EntityRecord, EntityStorage};
use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId, PENDING_DATABASE_ID};
use mercury::endpoint::Address;
use postgres::error::SqlState;
use postgres::types::ToSql;
use postgres::Client;

use crate::json;
use crate::schema::{
    column_name, entity_table_name, quote_ident, ColumnKind, EntitySchema, ID_COLUMN,
};

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
    // The lock conditions are the retryable ones
    // (`database_exception.cpp:35-40`: `ER_LOCK_DEADLOCK` and
    // `ER_LOCK_WAIT_TIMEOUT`; retry.rs:96-98). PostgreSQL spells them
    // 40P01 `deadlock_detected` and 55P03 `lock_not_available`.
    let is_retryable_lock = error.code().is_some_and(|code| {
        *code == SqlState::T_R_DEADLOCK_DETECTED || *code == SqlState::LOCK_NOT_AVAILABLE
    });
    let is_duplicate = error
        .code()
        .is_some_and(|code| *code == SqlState::UNIQUE_VIOLATION);
    if is_retryable_lock {
        return DbError::Retry;
    }
    if is_duplicate {
        return DbError::DuplicateAccount;
    }
    DbError::Storage(error_message(&error))
}

/// Maps a backend error on an *entity* table. Entity tables carry no other
/// unique key than the primary id, so a unique violation there is the
/// duplicate-id outcome of `insertExplicit`
/// (`put_entity_task.cpp:93-97`) — [`DbError::AlreadyExists`]. The
/// account tables keep [`DbError::DuplicateAccount`].
fn map_entity_db_error(error: postgres::Error) -> DbError {
    if error
        .code()
        .is_some_and(|code| *code == SqlState::UNIQUE_VIOLATION)
    {
        DbError::AlreadyExists
    } else {
        map_db_error(error)
    }
}

/// A `Uint` must fit `BIGINT` — PostgreSQL has no unsigned integer.
fn uint_to_bigint(value: u64) -> Result<i64, DbError> {
    i64::try_from(value)
        .map_err(|_| DbError::Storage(format!("uint64 value {value} exceeds BIGINT")))
}

/// Validates a record's values against its schema before any SQL runs:
/// the values must be the schema's columns in visit order and every `Uint`
/// must fit `BIGINT`.
fn validate_put_values(schema: &EntitySchema, record: &EntityRecord) -> Result<(), DbError> {
    if record.values.len() != schema.columns.len() {
        return Err(DbError::Storage(format!(
            "record for {} carries {} values but the schema lists {} columns",
            schema.type_name,
            record.values.len(),
            schema.columns.len()
        )));
    }
    for value in &record.values {
        if let PropertyValue::Uint(n) = value {
            uint_to_bigint(*n)?;
        }
    }
    Ok(())
}

/// `validate_put_values` plus the rule for a row that already has a
/// DatabaseID: a pending one (the reference's `PENDING_DATABASE_ID`,
/// `-1`) means the table has not handed the entity a DBID out yet.
fn validate_put(schema: &EntitySchema, record: &EntityRecord) -> Result<(), DbError> {
    validate_put_values(schema, record)?;
    if record.db_id < 0 {
        return Err(DbError::Storage(format!(
            "record for {} still has a pending database id",
            schema.type_name
        )));
    }
    Ok(())
}

/// Builds the upsert statement for a schema: the id column first, then one
/// placeholder per property column, nested columns cast to JSONB. The
/// `ON CONFLICT ("id") DO UPDATE` clause is the reference `REPLACE INTO`.
fn put_entity_sql(schema: &EntitySchema) -> String {
    let mut placeholders = vec!["$1".to_string()];
    placeholders.extend(placeholders_from(schema, 2));
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
        "INSERT INTO {} ({}) OVERRIDING SYSTEM VALUE VALUES ({}) ON CONFLICT ({}) DO UPDATE SET {}",
        schema.quoted_table(),
        schema.quoted_columns().join(", "),
        placeholders.join(", "),
        quote_ident(ID_COLUMN),
        assignments
    )
}

/// One placeholder per property column, numbered from `first`.
fn placeholders_from(schema: &EntitySchema, first: usize) -> Vec<String> {
    schema
        .columns
        .iter()
        .enumerate()
        .map(|(index, (_, kind))| nested_placeholder(first + index, kind))
        .collect()
}

/// The explicit-id insert: the reference's `insertExplicit`
/// (`put_entity_task.cpp:89-97`) is a plain INSERT, so a duplicate primary
/// key fails the statement ([`DbError::AlreadyExists`]) and the original
/// row survives — the upsert ([`put_entity_sql`]) would overwrite instead.
fn insert_explicit_entity_sql(schema: &EntitySchema) -> String {
    let mut placeholders = vec!["$1".to_string()];
    placeholders.extend(placeholders_from(schema, 2));
    format!(
        "INSERT INTO {} ({}) OVERRIDING SYSTEM VALUE VALUES ({})",
        schema.quoted_table(),
        schema.quoted_columns().join(", "),
        placeholders.join(", ")
    )
}

/// The same write for an entity the table has not given a DBID to: the
/// id column is left out so the identity column allocates one, and the
/// statement reads it back. This is the reference's shape — the buffered
/// task path turns a `PENDING_DATABASE_ID` into `0` before the insert, so
/// the table's `AUTO_INCREMENT` key column is what supplies the value.
fn put_new_entity_sql(schema: &EntitySchema) -> String {
    format!(
        "INSERT INTO {} ({}) VALUES ({}) RETURNING {}",
        schema.quoted_table(),
        schema.quoted_property_columns().join(", "),
        placeholders_from(schema, 1).join(", "),
        quote_ident(ID_COLUMN)
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

fn read_ip(raw: i64) -> u32 {
    (raw as u32).swap_bytes()
}

fn read_port(raw: i32) -> u16 {
    (raw as u16).swap_bytes()
}

/// The reference's `salt` column is `SMALLINT UNSIGNED`, and the ported
/// address type carries a `u16` salt — the INTEGER column holds it whole.
fn stored_salt(salt: u16) -> i32 {
    i32::from(salt)
}

fn read_salt(raw: i32) -> u16 {
    raw as u16
}

/// MySQL's `AUTO_INCREMENT` counter climbs past explicit ids — after an
/// `insertExplicit` of 5 the next created entity gets 6 — while a
/// PostgreSQL identity sequence does not move for `OVERRIDING SYSTEM
/// VALUE` inserts and would later collide with the explicit row. The port
/// re-seats the sequence past the explicit id before the insert.
/// `nextval` first means the call never lowers the sequence, and the
/// reference's counter moves on refused inserts too, so the bump runs
/// before the insert's outcome is known.
fn bump_identity_sql() -> String {
    "SELECT setval(pg_get_serial_sequence($1, 'id')::regclass, \
     GREATEST($2, nextval(pg_get_serial_sequence($1, 'id')::regclass)), true)"
        .to_string()
}

/// The row-existence check the update leg and the checkout run first: the
/// upsert would create a missing row instead of failing it, and no
/// log-on record ever points at a missing entity
/// (`put_entity_task.cpp:103-108` checks existence first).
fn row_exists_sql(schema: &EntitySchema) -> String {
    format!(
        "SELECT 1 FROM {} WHERE {} = $1",
        schema.quoted_table(),
        quote_ident(ID_COLUMN)
    )
}

/// One placeholder for a property column. Nested values bind as text and
/// cast server-side: an un-annotated `CAST` would type-infer the parameter
/// itself as `jsonb`, which the text binding then cannot serialize.
fn nested_placeholder(number: usize, kind: &ColumnKind) -> String {
    let placeholder = format!("${number}");
    match kind {
        ColumnKind::Nested => format!("CAST({placeholder}::text AS JSONB)"),
        _ => placeholder,
    }
}

/// A bound parameter (the id first, then one per column).
type PutParam = Box<dyn ToSql + Sync>;

/// Builds the parameter list for a record, in the statement's placeholder
/// order (the id first). Nested values bind as their JSON text; the caller
/// keeps the text strings alive for the borrow.
fn put_entity_params(
    schema: &EntitySchema,
    record: &EntityRecord,
    include_db_id: bool,
) -> Result<(Vec<PutParam>, Vec<String>), DbError> {
    if include_db_id {
        validate_put(schema, record)?;
    } else {
        validate_put_values(schema, record)?;
    }
    let mut params: Vec<PutParam> = if include_db_id {
        vec![Box::new(record.db_id)]
    } else {
        Vec::new()
    };
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

/// The select column list for a schema: the id column first, then one
/// quoted column per property in visit order, JSONB columns cast to their
/// canonical JSON text.
fn select_columns(schema: &EntitySchema) -> Vec<String> {
    let mut columns = vec![quote_ident(ID_COLUMN)];
    columns.extend(schema.columns.iter().map(|(prop, kind)| {
        let name = quote_ident(&column_name(prop));
        match kind {
            ColumnKind::Nested => format!("({name})::text"),
            _ => name,
        }
    }));
    columns
}

/// Builds the select statement for a schema: the id column first, then one
/// quoted column per property in visit order, JSONB columns cast to their
/// canonical JSON text.
fn get_entity_sql(schema: &EntitySchema) -> String {
    format!(
        "SELECT {} FROM {} WHERE {} = $1",
        select_columns(schema).join(", "),
        schema.quoted_table(),
        quote_ident(ID_COLUMN)
    )
}

/// The whole-table select `all_entities` runs per registered type.
fn select_all_entity_sql(schema: &EntitySchema) -> String {
    format!(
        "SELECT {} FROM {}",
        select_columns(schema).join(", "),
        schema.quoted_table()
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
///
/// The client sits in a `RefCell` because the storage contract
/// (`EntityStorage`) reads through `&self` — the shared shape of the
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
    /// Whether this connection was flagged as lost, the reference's
    /// `sql_.hasLostConnection( true )` — set when rolling back finds the
    /// connection already gone (transaction.cpp:41-47).
    connection_lost: bool,
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
            connection_lost: false,
        })
    }

    /// Opens a server-side transaction on this connection: the reference's
    /// `MySqlTransaction` constructor issues `START TRANSACTION` and only
    /// then marks the connection as being inside one
    /// (`transaction.cpp:15-23` — the flag is last because the execute can
    /// fail). Every statement this connection runs afterwards belongs to
    /// the transaction, so the contract operations send nothing else.
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
            .execute("START TRANSACTION", &[])
            .map_err(map_db_error)?;
        // `inTransaction(true)` is set last in the reference: a failing
        // execute leaves the connection outside any transaction
        // (transaction.cpp:19-22).
        self.in_transaction = true;
        Ok(())
    }

    /// Commits the open transaction (`transaction.cpp:66-71`): the writes
    /// this connection has made are visible from the other connections.
    pub fn commit_transaction(&mut self) -> Result<(), DbError> {
        if !self.in_transaction {
            return Err(DbError::Storage("there is no transaction to commit".into()));
        }
        self.client
            .borrow_mut()
            .execute("COMMIT", &[])
            .map_err(map_db_error)?;
        self.in_transaction = false;
        Ok(())
    }

    /// Whether a transaction is open on this connection.
    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    /// Rolls the open transaction back (`transaction.cpp:29-51`), the
    /// reference's destructor doing the work: nothing it made is visible to
    /// the other connections, which keep reading the values that stood
    /// there. The connection stops being inside a transaction whatever
    /// happens (`inTransaction( false )` runs after the rollback, so it
    /// clears even when the rollback itself failed).
    ///
    /// When the rollback finds the connection already gone the failure is
    /// swallowed and the connection is flagged instead
    /// (`transaction.cpp:41-47`).
    pub fn rollback_transaction(&mut self) -> Result<(), DbError> {
        if !self.in_transaction {
            // The reference's destructor rolls back only a transaction it
            // is inside (`if (!committed_ …)`), so there is nothing to
            // roll back.
            return Err(DbError::Storage(
                "there is no transaction to roll back".into(),
            ));
        }
        match self.client.borrow_mut().execute("ROLLBACK", &[]) {
            Ok(_) => {}
            // `catch (DatabaseException & e) { if (e.isLostConnection())
            // sql_.hasLostConnection( true ); }` (transaction.cpp:41-47).
            // A closed client and an I/O failure are both connection-level
            // (the reference's `CR_*` codes); a `ROLLBACK` carries no
            // parameters, so any server-reported error is a real SQL
            // failure and the other arm reports it.
            Err(error) if error.is_closed() || error.as_db_error().is_none() => {
                self.connection_lost = true;
            }
            Err(error) => {
                // Every other rollback failure escapes the reference's
                // destructor as a DatabaseException, which terminate()
                // would turn into an abort — so the port reports it.
                self.in_transaction = false;
                return Err(map_db_error(error));
            }
        }
        self.in_transaction = false;
        Ok(())
    }

    /// Whether this connection was flagged as lost, the reference's
    /// `sql_.hasLostConnection( true )`.
    pub fn connection_lost(&self) -> bool {
        self.connection_lost
    }

    /// The underlying client, for the schema tools (BW-09-07).
    pub fn client_mut(&mut self) -> RefMut<'_, Client> {
        self.client.borrow_mut()
    }

    /// Registers an entity type with this connection: the contract
    /// operations (`EntityStorage`) resolve the table shape by
    /// `EntityTypeId`, the way the reference's `entityTypeMappings_`
    /// vector indexes each type's mapping. Registration is in-memory —
    /// the table itself is [`PostgresStorage::create_entity_table`]'s face.
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

    /// Re-seats the identity sequence past `id` — see
    /// [`bump_identity_sql`]. The table name travels quoted:
    /// `pg_get_serial_sequence` parses its argument as an identifier, so
    /// the mixed-case reference names would fold without the quotes.
    fn bump_identity(&mut self, schema: &EntitySchema, id: DatabaseId) -> Result<(), DbError> {
        let table = quote_ident(&entity_table_name(&schema.type_name));
        self.client
            .borrow_mut()
            .execute(bump_identity_sql().as_str(), &[&table, &id])
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Writes the log-on record for `key` (`addLogOnRecord`,
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
                    &(key.type_id as i32),
                    &mailbox.entity_id,
                    &stored_ip(mailbox.addr.ip),
                    &stored_port(mailbox.addr.port),
                    &stored_salt(mailbox.addr.salt),
                ],
            )
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Creates this schema's table if it is missing (the entity-mapping
    /// part of a `sync_db` run; the fixed tables and the dry-run report
    /// join with the BW-09-07 items).
    pub fn create_entity_table(&mut self, schema: &EntitySchema) -> Result<(), DbError> {
        self.client
            .borrow_mut()
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
        let (params, nested_texts) = put_entity_params(schema, record, true)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
        self.client
            .borrow_mut()
            .execute(sql.as_str(), &refs)
            .map_err(map_db_error)?;
        drop(nested_texts);
        Ok(())
    }

    /// Writes an entity the table has not given a DatabaseID to and
    /// returns the DatabaseID the table allocated. This is the
    /// reference's buffered-task path: a `PENDING_DATABASE_ID` becomes
    /// `0` before the insert, so the key column's `AUTO_INCREMENT`
    /// supplies the value — in PostgreSQL, the identity column.
    pub fn put_new_entity(
        &mut self,
        schema: &EntitySchema,
        record: &EntityRecord,
    ) -> Result<i64, DbError> {
        let sql = put_new_entity_sql(schema);
        let (params, nested_texts) = put_entity_params(schema, record, false)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
        let row = self
            .client
            .borrow_mut()
            .query_one(sql.as_str(), &refs)
            .map_err(map_db_error)?;
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

/// The shared storage contract (`IDatabase`) on PostgreSQL. The schema-
/// shaped inherent methods keep their BW-09-06-02/03 names, so the
/// contract methods are reached through the trait — callers write
/// `EntityStorage::put_entity(&mut storage, …)`.
impl EntityStorage for PostgresStorage {
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
        // An unknown type has no table: the "no row" outcome. The shape is
        // cloned out of the registry so the operation runs without the
        // borrow.
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
                "entity type {} is invalid",
                key.type_id
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
            return PostgresStorage::put_new_entity(self, &schema, &record);
        }

        if put_explicit_id {
            // insertExplicit: a plain INSERT, so a duplicate id fails and
            // the original row survives (put_entity_task.cpp:89-97).
            let sql = insert_explicit_entity_sql(&schema);
            let (params, nested_texts) = put_entity_params(&schema, &record, true)?;
            self.bump_identity(&schema, key.db_id)?;
            let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref()).collect();
            self.client
                .borrow_mut()
                .execute(sql.as_str(), &refs)
                .map_err(map_entity_db_error)?;
            drop(nested_texts);
            return Ok(key.db_id);
        }

        // The update leg: a missing row is the XML backend's `isOK=false`
        // (xml_database.cpp:617-620) — the upsert would create the row
        // instead, so the existence check runs first.
        let exists_sql = row_exists_sql(&schema);
        let found = self
            .client
            .borrow_mut()
            .query_opt(exists_sql.as_str(), &[&key.db_id])
            .map_err(map_db_error)?;
        if found.is_none() {
            return Err(DbError::NotFound);
        }
        PostgresStorage::put_entity(self, &schema, &record)?;
        Ok(key.db_id)
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        let Some(schema) = self.schema_of(key.type_id).cloned() else {
            return Err(DbError::NotFound);
        };
        let sql = format!(
            "DELETE FROM {} WHERE {} = $1",
            schema.quoted_table(),
            quote_ident(ID_COLUMN)
        );
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
                &[&key.db_id, &(key.type_id as i32)],
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
                &[&key.db_id, &(key.type_id as i32)],
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
                &[&key.db_id, &(key.type_id as i32)],
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
        let (params, nested) = put_entity_params(&schema(), &record(41), true).unwrap();
        assert_eq!(params.len(), 4);
        assert!(nested.is_empty());
        // A schema with a nested column proves the nested-text leg.
        let values = vec![PropertyValue::FixedDict(None)];
        let schema = EntitySchema::for_record("Monster", &["extra"], &values).unwrap();
        let mut nested_record = EntityRecord::new(0);
        nested_record.db_id = 2;
        nested_record.values = values.clone();
        let (params, nested) = put_entity_params(&schema, &nested_record, true).unwrap();
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
    fn an_explicit_dbid_overrides_the_identity_column() {
        let sql = put_entity_sql(&schema());
        assert!(sql.starts_with(concat!(
            "INSERT INTO \"tbl_Account\" ",
            "(\"id\", \"sm_name\", \"sm_level\", \"sm_secret\") OVERRIDING SYSTEM VALUE"
        )));
        assert!(sql.contains("ON CONFLICT (\"id\") DO UPDATE SET"));
    }

    #[test]
    fn a_new_entity_leaves_the_id_to_the_table_and_reads_it_back() {
        assert_eq!(
            put_new_entity_sql(&schema()),
            concat!(
                "INSERT INTO \"tbl_Account\" ",
                "(\"sm_name\", \"sm_level\", \"sm_secret\") VALUES ($1, $2, $3) ",
                "RETURNING \"id\""
            )
        );
    }

    #[test]
    fn new_entity_params_skip_the_dbid_but_keep_the_values() {
        // The pending dbid is exactly what makes the row a new one, so it
        // must not be rejected on this path.
        let (params, nested) = put_entity_params(&schema(), &record(-1), false).unwrap();
        assert_eq!(params.len(), 3);
        assert!(nested.is_empty());
    }

    #[test]
    fn a_pending_dbid_is_rejected_for_an_explicit_write() {
        let err = put_entity_params(&schema(), &record(-1), true).unwrap_err();
        assert!(err.to_string().contains("pending database id"));
    }

    #[test]
    fn the_explicit_insert_is_a_plain_insert_that_keeps_the_original_row() {
        assert_eq!(
            insert_explicit_entity_sql(&schema()),
            concat!(
                "INSERT INTO \"tbl_Account\" ",
                "(\"id\", \"sm_name\", \"sm_level\", \"sm_secret\") ",
                "OVERRIDING SYSTEM VALUE VALUES ($1, $2, $3, $4)"
            )
        );
    }

    #[test]
    fn the_logon_statements_match_the_reference_queries() {
        // addLogOnQuery is INSERT ... ON DUPLICATE KEY UPDATE
        // (entity_type_mapping.cpp:749-758).
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
        // getLogOnQuery (entity_type_mapping.cpp:764-766).
        assert_eq!(
            get_logon_sql(),
            concat!(
                "SELECT \"objectID\", \"ip\", \"port\", \"salt\" ",
                "FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1 AND \"typeID\" = $2"
            )
        );
        // removeLogOnQuery (entity_type_mapping.cpp:527-529).
        assert_eq!(
            remove_logon_sql(),
            "DELETE FROM \"bigworldLogOns\" WHERE \"databaseID\" = $1 AND \"typeID\" = $2"
        );
    }

    #[test]
    fn the_address_travels_in_network_byte_order() {
        // htonl/htons on the way in (entity_type_mapping.cpp:779-781),
        // ntohl/ntohs on the way out (:815-816).
        assert_eq!(stored_ip(0x0A00_0001), 0x0100_000A);
        assert_eq!(read_ip(stored_ip(0x0A00_0001)), 0x0A00_0001);
        assert_eq!(stored_port(30000), 0x3075);
        assert_eq!(read_port(stored_port(30000)), 30000);
        // The salt keeps its whole width: the ported address carries a
        // u16 salt and the INTEGER column holds it whole.
        assert_eq!(stored_salt(u16::MAX), 65535);
        assert_eq!(read_salt(stored_salt(u16::MAX)), u16::MAX);
        assert_eq!(read_salt(stored_salt(7)), 7);
    }

    #[test]
    fn the_identity_bump_reseats_the_sequence_past_explicit_ids() {
        assert_eq!(
            bump_identity_sql(),
            concat!(
                "SELECT setval(pg_get_serial_sequence($1, 'id')::regclass, ",
                "GREATEST($2, nextval(pg_get_serial_sequence($1, 'id')::regclass)), true)"
            )
        );
    }

    #[test]
    fn the_existence_check_and_the_full_scan_select_the_stored_columns() {
        assert_eq!(
            row_exists_sql(&schema()),
            "SELECT 1 FROM \"tbl_Account\" WHERE \"id\" = $1"
        );
        assert_eq!(
            select_all_entity_sql(&schema()),
            concat!(
                "SELECT \"id\", \"sm_name\", \"sm_level\", \"sm_secret\" ",
                "FROM \"tbl_Account\""
            )
        );
    }
}
