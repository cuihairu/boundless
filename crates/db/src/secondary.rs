//! The secondary database reader — `server/baseapp/sqlite_database.cpp`
//! writes it, `server/tools/consolidate_dbs/secondary_database.cpp` reads
//! it, and BW-09-03-02 ports the reading half.
//!
//! A secondary database is the baseapp's local SQLite file holding entity
//! snapshots while the primary (MySQL/XML) database is unreachable. Its
//! schema is the writer's own (`sqlite_database.cpp:572-617`):
//!
//! * `tbl_checksum` with the single TEXT column `sm_checksum`, holding the
//!   entity-def digest the file was written against;
//! * `tbl_flip` and `tbl_flop`, each `sm_dbID INTEGER, sm_typeID INTEGER,
//!   sm_time INTEGER, sm_blob BLOB`. The writer flip-flops between the two
//!   tables so a crash mid-flip keeps the previous generation readable.
//!
//! Reading follows `SecondaryDatabase::init`/`readTables`
//! (`secondary_database.cpp:50-148`): both tables are probed for
//! existence, counted, and then sorted so the table with the *older* first
//! `sm_time` row consolidates first — the consolidator applies rows in
//! order into the primary database, so the older generation lands first
//! and the newer one wins.
//!
//! The `sm_blob` of each row is the entity property stream: the
//! BASE|CELL|ONLY_PERSISTENT properties in definition order followed by
//! the fixed cell properties (`position`, `direction`, `spaceID`) for a
//! cell-capable entity (`entity_type_mapping.cpp:646-656`). The meta
//! properties (`gameTime`, `timestamp`) are *not* in the stream here —
//! `ConsolidateEntityTask` hands the primary database the row's `sm_time`
//! instead (`consolidate_entity_task.cpp:18-25`). The per-property
//! encodings are the reference's stream operators: fixed-width little
//! endian integers and floats, packed-int-prefixed strings and blobs
//! (`binary_stream.hpp:254-296,309-317`), a `u32` count before sequence
//! elements, fixed dicts field by field, vectors as `n` float32
//! components, and a mailbox as `id, ip, port, salt`
//! (`basictypes.hpp:322-334`, 12 bytes total).
//!
//! Consolidation itself — the same-entity version arbitration and the
//! auto-load handling — is BW-09-03-03 and BW-09-03-04.

use std::path::Path;

use foundation::ids::{DatabaseId, EntityTypeId, GameTime};

use crate::error::DbError;
use crate::record::EntityRecord;
use entitydef::value::PropertyValue;

/// `flipTable_` (`sqlite_database.cpp:327`).
pub const FLIP_TABLE: &str = "tbl_flip";
/// `flopTable_` (`sqlite_database.cpp:328`).
pub const FLOP_TABLE: &str = "tbl_flop";
/// `checksumTable_` (`sqlite_database.cpp:333`).
pub const CHECKSUM_TABLE: &str = "tbl_checksum";
/// `dbIDColumn_` (`sqlite_database.cpp:329`).
pub const DB_ID_COLUMN: &str = "sm_dbID";
/// `typeIDColumn_` (`sqlite_database.cpp:330`).
pub const TYPE_ID_COLUMN: &str = "sm_typeID";
/// `timeColumn_` (`sqlite_database.cpp:331`).
pub const TIME_COLUMN: &str = "sm_time";
/// `blobColumn_` (`sqlite_database.cpp:332`).
pub const BLOB_COLUMN: &str = "sm_blob";
/// `checksumColumn_` (`sqlite_database.cpp:334`).
pub const CHECKSUM_COLUMN: &str = "sm_checksum";

/// The tables the reader probes, in the reference's probe order
/// (`secondary_database.cpp:109`).
pub const TABLE_NAMES: [&str; 2] = [FLIP_TABLE, FLOP_TABLE];

// -----------------------------------------------------------------------------
// Section: the stream face of the entity properties
// -----------------------------------------------------------------------------

/// The value shape one property takes in the property stream, at the
/// width the stream carries (`SimplePropertyMapping`'s `stream >> value`).
/// This is the stream face of [`crate::xml::XmlKind`]; the def loader
/// adapts its types onto this.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamKind {
    /// The signed integer family, by stream width.
    Int8,
    Int16,
    Int32,
    Int64,
    /// The unsigned integer family, by stream width.
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    /// `FLOAT32`.
    Float32,
    /// `FLOAT64`.
    Float64,
    /// `StringDataType` — packed length + bytes.
    String,
    /// `UnicodeStringDataType` — the same stream shape as [`Self::String`]
    /// (`unicode_string_mapping.cpp` reads it as a string).
    UnicodeString,
    /// `BlobDataType` — packed length + bytes.
    Blob,
    /// `Vector2DataType`.
    Vector2,
    /// `Vector3DataType`.
    Vector3,
    /// `Vector4DataType`.
    Vector4,
    /// A sequence: `u32` count, then that many elements.
    Sequence(Box<StreamKind>),
    /// A fixed dict: every field in declaration order, no count.
    FixedDict(Vec<(String, StreamKind)>),
    /// `MailBoxDataType`: `id` u32, `ip` u32, `port` u16, `salt` u16.
    MailBox,
}

/// One property of a [`StreamTypeDef`].
#[derive(Clone, Debug)]
pub struct StreamPropDef {
    /// The property's name (diagnostics only on the stream face — the
    /// stream itself carries no names).
    pub name: String,
    /// The value shape.
    pub kind: StreamKind,
}

/// One entity type's stream face: the persistent properties in stream
/// order — BASE|CELL|ONLY_PERSISTENT in definition order, then
/// `position`/`direction`/`spaceID` for a cell-capable entity.
#[derive(Clone, Debug, Default)]
pub struct StreamTypeDef {
    /// The entity type name.
    pub name: String,
    /// The type's id.
    pub type_id: EntityTypeId,
    /// The properties in stream order.
    pub properties: Vec<StreamPropDef>,
}

/// `BinaryIStream::readPackedInt` (`binary_stream.hpp:287-296`): one byte,
/// or `0xFF` followed by `BW_UNPACK3`'s three little-endian bytes
/// (`binary_stream.ipp:166-171`).
fn read_packed_int(stream: &mut Reader<'_>) -> Result<u32, DbError> {
    let first = stream.byte()?;
    if first == 0xFF {
        Ok(u32::from_le_bytes([
            stream.byte()?,
            stream.byte()?,
            stream.byte()?,
            0,
        ]))
    } else {
        Ok(first as u32)
    }
}

/// `BinaryOStream::writePackedInt` (`binary_stream.hpp:254-269`).
fn write_packed_int(out: &mut Vec<u8>, value: u32) {
    if value >= 0xFF {
        out.push(0xFF);
        out.extend_from_slice(&value.to_le_bytes()[..3]);
    } else {
        out.push(value as u8);
    }
}

/// A cursor over the property stream, little-endian like the reference's
/// native `retrieve` (`binary_stream.hpp`, `ASSERT(LITTLE_ENDIAN)`).
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, DbError> {
        let byte = *self
            .bytes
            .get(self.pos)
            .ok_or_else(|| DbError::Storage("secondary db: stream ended early".to_owned()))?;
        self.pos += 1;
        Ok(byte)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], DbError> {
        if self.pos + len > self.bytes.len() {
            return Err(DbError::Storage(format!(
                "secondary db: stream ended early at byte {} of {}",
                self.pos,
                self.bytes.len()
            )));
        }
        let slice = &self.bytes[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn uint(&mut self, width: usize) -> Result<u64, DbError> {
        let bytes = self.take(width)?;
        let mut raw = [0u8; 8];
        raw[..width].copy_from_slice(bytes);
        Ok(u64::from_le_bytes(raw))
    }

    fn int(&mut self, width: usize) -> Result<i64, DbError> {
        let bytes = self.take(width)?;
        let mut raw = [0u8; 8];
        raw[..width].copy_from_slice(bytes);
        // The sign lives in the declared width's top bit, not the u64's.
        let shift = 64 - width * 8;
        Ok((i64::from_le_bytes(raw) << shift) >> shift)
    }

    fn float32(&mut self) -> Result<f32, DbError> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn float64(&mut self) -> Result<f64, DbError> {
        Ok(f64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    /// `operator>>(BinaryIStream&, BW::string&)`
    /// (`binary_stream.hpp:309-317`): a packed length, then that many raw
    /// bytes.
    fn bytes_prefixed(&mut self) -> Result<Vec<u8>, DbError> {
        let len = read_packed_int(self)? as usize;
        Ok(self.take(len)?.to_vec())
    }
}

/// Read one value of `kind` off the stream.
fn read_value(stream: &mut Reader<'_>, kind: &StreamKind) -> Result<PropertyValue, DbError> {
    Ok(match kind {
        StreamKind::Int8 => PropertyValue::Int(stream.int(1)?),
        StreamKind::Int16 => PropertyValue::Int(stream.int(2)?),
        StreamKind::Int32 => PropertyValue::Int(stream.int(4)?),
        StreamKind::Int64 => PropertyValue::Int(stream.int(8)?),
        StreamKind::Uint8 => PropertyValue::Uint(stream.uint(1)?),
        StreamKind::Uint16 => PropertyValue::Uint(stream.uint(2)?),
        StreamKind::Uint32 => PropertyValue::Uint(stream.uint(4)?),
        StreamKind::Uint64 => PropertyValue::Uint(stream.uint(8)?),
        StreamKind::Float32 => PropertyValue::Float(stream.float32()? as f64),
        StreamKind::Float64 => PropertyValue::Float(stream.float64()?),
        StreamKind::String | StreamKind::UnicodeString => {
            PropertyValue::String(String::from_utf8_lossy(&stream.bytes_prefixed()?).into_owned())
        }
        StreamKind::Blob => PropertyValue::Blob(stream.bytes_prefixed()?),
        StreamKind::Vector2 => PropertyValue::Vector(vec![stream.float32()?, stream.float32()?]),
        StreamKind::Vector3 => PropertyValue::Vector(vec![
            stream.float32()?,
            stream.float32()?,
            stream.float32()?,
        ]),
        StreamKind::Vector4 => PropertyValue::Vector(vec![
            stream.float32()?,
            stream.float32()?,
            stream.float32()?,
            stream.float32()?,
        ]),
        StreamKind::Sequence(element) => {
            let count = stream.uint(4)?;
            let mut items = Vec::with_capacity(count.min(1024) as usize);
            for _ in 0..count {
                items.push(read_value(stream, element)?);
            }
            PropertyValue::Sequence(items)
        }
        StreamKind::FixedDict(fields) => PropertyValue::FixedDict(Some(
            fields
                .iter()
                .map(|(name, kind)| Ok((name.clone(), read_value(stream, kind)?)))
                .collect::<Result<Vec<_>, DbError>>()?,
        )),
        StreamKind::MailBox => PropertyValue::MailBox {
            id: stream.uint(4)? as i32,
            ip: stream.uint(4)? as u32,
            port: stream.uint(2)? as u16,
            salt: stream.uint(2)? as u16,
        },
    })
}

/// Write one value of `kind` onto the stream. A value whose shape does not
/// fit the kind writes nothing (a caller contract breach, the same way a
/// failed `addToStream` leaves the stream short).
fn write_value(out: &mut Vec<u8>, value: &PropertyValue, kind: &StreamKind) {
    let push_int = |out: &mut Vec<u8>, v: i64, width: usize| {
        out.extend_from_slice(&v.to_le_bytes()[..width]);
    };
    match (value, kind) {
        (PropertyValue::Int(v), StreamKind::Int8) => push_int(out, *v, 1),
        (PropertyValue::Int(v), StreamKind::Int16) => push_int(out, *v, 2),
        (PropertyValue::Int(v), StreamKind::Int32) => push_int(out, *v, 4),
        (PropertyValue::Int(v), StreamKind::Int64) => out.extend_from_slice(&v.to_le_bytes()),
        (PropertyValue::Uint(v), StreamKind::Uint8) => push_int(out, *v as i64, 1),
        (PropertyValue::Uint(v), StreamKind::Uint16) => push_int(out, *v as i64, 2),
        (PropertyValue::Uint(v), StreamKind::Uint32) => push_int(out, *v as i64, 4),
        (PropertyValue::Uint(v), StreamKind::Uint64) => out.extend_from_slice(&v.to_le_bytes()),
        (PropertyValue::Float(v), StreamKind::Float32) => {
            out.extend_from_slice(&(*v as f32).to_le_bytes())
        }
        (PropertyValue::Float(v), StreamKind::Float64) => out.extend_from_slice(&v.to_le_bytes()),
        (PropertyValue::String(v), StreamKind::String | StreamKind::UnicodeString) => {
            write_packed_int(out, v.len() as u32);
            out.extend_from_slice(v.as_bytes());
        }
        (PropertyValue::Blob(v), StreamKind::Blob) => {
            write_packed_int(out, v.len() as u32);
            out.extend_from_slice(v);
        }
        (
            PropertyValue::Vector(v),
            kind @ (StreamKind::Vector2 | StreamKind::Vector3 | StreamKind::Vector4),
        ) => {
            let n = match kind {
                StreamKind::Vector2 => 2,
                StreamKind::Vector3 => 3,
                _ => 4,
            };
            for component in v.iter().take(n) {
                out.extend_from_slice(&component.to_le_bytes());
            }
        }
        (PropertyValue::Sequence(items), StreamKind::Sequence(element)) => {
            out.extend_from_slice(&(items.len() as u32).to_le_bytes());
            for item in items {
                write_value(out, item, element);
            }
        }
        (PropertyValue::FixedDict(Some(fields)), StreamKind::FixedDict(defs)) => {
            for ((_, value), (_, kind)) in fields.iter().zip(defs) {
                write_value(out, value, kind);
            }
        }
        (PropertyValue::MailBox { id, ip, port, salt }, StreamKind::MailBox) => {
            out.extend_from_slice(&(*id as u32).to_le_bytes());
            out.extend_from_slice(&ip.to_le_bytes());
            out.extend_from_slice(&port.to_le_bytes());
            out.extend_from_slice(&salt.to_le_bytes());
        }
        _ => {}
    }
}

/// Decode one row's `sm_blob` into the entity's values in the def's stream
/// order. The stream must be exactly consumed — a trailing byte is a
/// corrupt payload, since the writer writes every property of the def
/// (`entity_type_mapping.cpp:646-656`).
pub fn blob_to_values(blob: &[u8], def: &StreamTypeDef) -> Result<Vec<PropertyValue>, DbError> {
    let mut stream = Reader {
        bytes: blob,
        pos: 0,
    };
    let mut values = Vec::with_capacity(def.properties.len());
    for prop in &def.properties {
        values.push(read_value(&mut stream, &prop.kind).map_err(|error| {
            DbError::Storage(format!("secondary db: '{}': {error}", prop.name))
        })?);
    }
    if stream.pos != blob.len() {
        return Err(DbError::Storage(format!(
            "secondary db: '{}' row has {} trailing bytes",
            def.name,
            blob.len() - stream.pos
        )));
    }
    Ok(values)
}

/// Encode values into a `sm_blob` in the def's stream order.
pub fn values_to_blob(values: &[PropertyValue], def: &StreamTypeDef) -> Vec<u8> {
    let mut blob = Vec::new();
    for (prop, value) in def.properties.iter().zip(values) {
        write_value(&mut blob, value, &prop.kind);
    }
    blob
}

// -----------------------------------------------------------------------------
// Section: the database reader
// -----------------------------------------------------------------------------

/// One entity row of a secondary database table.
#[derive(Clone, Debug, PartialEq)]
pub struct SecondaryRow {
    /// `sm_dbID`.
    pub db_id: DatabaseId,
    /// `sm_typeID`.
    pub type_id: EntityTypeId,
    /// `sm_time` — the game time the row was written at; the primary
    /// database's merge uses it to pick the newer version (BW-09-03-03).
    pub time: GameTime,
    /// `sm_blob` — the property stream.
    pub blob: Vec<u8>,
}

impl SecondaryRow {
    /// Decode the row's blob into values (`readTables`' consumers decode
    /// with the same def the writer streamed from).
    pub fn values(&self, def: &StreamTypeDef) -> Result<Vec<PropertyValue>, DbError> {
        blob_to_values(&self.blob, def)
    }

    /// The row as an [`EntityRecord`].
    pub fn record(&self, def: &StreamTypeDef) -> Result<EntityRecord, DbError> {
        Ok(EntityRecord {
            type_id: self.type_id,
            db_id: self.db_id,
            values: self.values(def)?,
        })
    }
}

/// One `tbl_flip`/`tbl_flop` generation of the file.
#[derive(Clone, Debug)]
pub struct SecondaryTable {
    /// The table name.
    pub name: String,
    /// `firstGameTime` (`secondary_database_table.cpp:119-134`): the first
    /// row's `sm_time`, or 0 for an empty table.
    pub first_game_time: GameTime,
    /// `numRows` (`secondary_database_table.cpp:106-112`).
    pub num_rows: usize,
}

/// A secondary database file: the parsed tables and their counts.
///
/// The tables are kept in consolidation order — older `firstGameTime`
/// first, exactly `sortTablesByAge`'s ascending order
/// (`secondary_database.cpp:170-175`).
#[derive(Debug)]
pub struct SecondaryDatabase {
    connection: rusqlite::Connection,
    tables: Vec<SecondaryTable>,
    num_entities: usize,
}

impl SecondaryDatabase {
    /// `SecondaryDatabase::init` (`secondary_database.cpp:50-72`): open
    /// the file and read the flip/flop tables. Both tables may be absent
    /// (a freshly created file writes the checksum table first); a table
    /// that exists must be readable.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let connection = rusqlite::Connection::open(path).map_err(|error| {
            DbError::Storage(format!("secondary db: could not open the file: {error}"))
        })?;
        let mut database = Self {
            connection,
            tables: Vec::new(),
            num_entities: 0,
        };
        database.read_tables()?;
        Ok(database)
    }

    /// `readTables` (`secondary_database.cpp:107-148`) plus
    /// `sortTablesByAge`.
    fn read_tables(&mut self) -> Result<(), DbError> {
        self.tables.clear();
        self.num_entities = 0;
        for name in TABLE_NAMES {
            if !self.table_exists(name) {
                continue;
            }
            let num_rows = self.count_rows(name)?;
            let first_game_time = self.first_game_time(name)?;
            self.tables.push(SecondaryTable {
                name: name.to_owned(),
                first_game_time,
                num_rows,
            });
            self.num_entities += num_rows;
        }
        // The older table consolidates first, so the newer generation's
        // rows overwrite it in the primary database.
        self.tables.sort_by_key(|table| table.first_game_time);
        Ok(())
    }

    /// `tableExists` (`secondary_database.cpp:154-164`): a table the file
    /// does not have fails the probe statement.
    fn table_exists(&self, name: &str) -> bool {
        let sql = format!("SELECT COUNT(*) FROM {name} WHERE 0");
        self.connection
            .prepare(&sql)
            .map(|mut statement| statement.exists([]).unwrap_or(false))
            .unwrap_or(false)
    }

    fn count_rows(&self, name: &str) -> Result<usize, DbError> {
        let sql = format!("SELECT COUNT(*) FROM {name}");
        self.connection
            .query_row(&sql, [], |row| row.get::<_, i64>(0))
            .map(|count| count.max(0) as usize)
            .map_err(|error| DbError::Storage(format!("secondary db: count failed: {error}")))
    }

    /// `SecondaryDatabaseTable::firstGameTime`
    /// (`secondary_database_table.cpp:119-134`): the first row's
    /// `sm_time`, or 0 when the table is empty.
    fn first_game_time(&self, name: &str) -> Result<GameTime, DbError> {
        let sql = format!("SELECT {TIME_COLUMN} FROM {name} ORDER BY rowid LIMIT 1");
        match self
            .connection
            .query_row(&sql, [], |row| row.get::<_, i64>(0))
        {
            Ok(time) => Ok(time.max(0) as GameTime),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(error) => Err(DbError::Storage(format!(
                "secondary db: time read failed: {error}"
            ))),
        }
    }

    /// The tables in consolidation order (older generation first).
    pub fn tables(&self) -> &[SecondaryTable] {
        &self.tables
    }

    /// `numEntities` (`secondary_database.hpp:32-33`).
    pub fn num_entities(&self) -> usize {
        self.num_entities
    }

    /// `getChecksumDigest` (`secondary_database.cpp:78-101`): the first
    /// row of `tbl_checksum`, or an error when the table is absent or
    /// empty.
    pub fn checksum_digest(&self) -> Result<String, DbError> {
        let sql = format!("SELECT {CHECKSUM_COLUMN} FROM {CHECKSUM_TABLE}");
        self.connection
            .query_row(&sql, [], |row| row.get::<_, String>(0))
            .map_err(|error| {
                DbError::Storage(format!(
                    "secondary db: failed to read the checksum table: {error}"
                ))
            })
    }

    /// Every row of one table, in insertion (rowid) order — the order
    /// `SecondaryDatabaseTable::consolidate` steps them in
    /// (`secondary_database_table.cpp:140-188`).
    pub fn rows(&self, table: &SecondaryTable) -> Result<Vec<SecondaryRow>, DbError> {
        let sql = format!(
            "SELECT {DB_ID_COLUMN}, {TYPE_ID_COLUMN}, {TIME_COLUMN}, {BLOB_COLUMN} FROM {}",
            table.name
        );
        let mut statement = self
            .connection
            .prepare(&sql)
            .map_err(|error| DbError::Storage(format!("secondary db: select failed: {error}")))?;
        let mut rows = statement
            .query([])
            .map_err(|error| DbError::Storage(format!("secondary db: select failed: {error}")))?;
        let mut out = Vec::with_capacity(table.num_rows);
        while let Some(row) = rows
            .next()
            .map_err(|error| DbError::Storage(format!("secondary db: step failed: {error}")))?
        {
            out.push(SecondaryRow {
                db_id: row.get(0).map_err(|error| {
                    DbError::Storage(format!("secondary db: dbID column: {error}"))
                })?,
                type_id: row
                    .get::<_, i64>(1)
                    .map_err(|error| {
                        DbError::Storage(format!("secondary db: typeID column: {error}"))
                    })?
                    .clamp(0, EntityTypeId::MAX as i64) as EntityTypeId,
                time: row
                    .get::<_, i64>(2)
                    .map_err(|error| {
                        DbError::Storage(format!("secondary db: time column: {error}"))
                    })?
                    .clamp(0, GameTime::MAX as i64) as GameTime,
                blob: row.get(3).map_err(|error| {
                    DbError::Storage(format!("secondary db: blob column: {error}"))
                })?,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's schema statements, verbatim
    /// (`sqlite_database.cpp:572-617`): the checksum table and the flip /
    /// flop table.
    fn create_tables(connection: &rusqlite::Connection, table: &str) {
        connection
            .execute(
                &format!(
                    "CREATE TABLE {table} ({DB_ID_COLUMN} INTEGER, \
                     {TYPE_ID_COLUMN} INTEGER, {TIME_COLUMN} INTEGER, \
                     {BLOB_COLUMN} BLOB)"
                ),
                [],
            )
            .expect("the entity table creates");
    }

    /// One temporary file per call: the tests run in parallel threads and
    /// share the process, so a shared path would be a shared database.
    static TEMP_FILE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn unique_path() -> std::path::PathBuf {
        let index = TEMP_FILE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("db-secondary-{}-{}", std::process::id(), index))
    }

    /// The reference's row insert (`Transaction::commit`,
    /// `sqlite_database.cpp:133-137`).
    fn insert_row(
        connection: &rusqlite::Connection,
        table: &str,
        db_id: i64,
        type_id: i64,
        time: i64,
        blob: &[u8],
    ) {
        connection
            .execute(
                &format!("INSERT INTO {table} VALUES (?, ?, ?, ?)"),
                rusqlite::params![db_id, type_id, time, blob],
            )
            .expect("the row inserts");
    }

    fn prop(name: &str, kind: StreamKind) -> StreamPropDef {
        StreamPropDef {
            name: name.to_owned(),
            kind,
        }
    }

    /// An Avatar-shaped stream def: two persistent properties plus the
    /// fixed cell trio.
    fn avatar_def() -> StreamTypeDef {
        StreamTypeDef {
            name: "Avatar".to_owned(),
            type_id: 1,
            properties: vec![
                prop("playerName", StreamKind::String),
                prop("level", StreamKind::Int32),
                prop("hp", StreamKind::Float32),
                prop("items", StreamKind::Sequence(Box::new(StreamKind::Int64))),
                prop(
                    "stats",
                    StreamKind::FixedDict(vec![
                        ("mana".to_owned(), StreamKind::Uint16),
                        ("stamina".to_owned(), StreamKind::Int16),
                    ]),
                ),
                prop("bind", StreamKind::MailBox),
                prop("position", StreamKind::Vector3),
                prop("direction", StreamKind::Vector3),
                prop("spaceID", StreamKind::Uint32),
            ],
        }
    }

    fn avatar_values(name: &str) -> Vec<PropertyValue> {
        vec![
            PropertyValue::String(name.to_owned()),
            PropertyValue::Int(7),
            PropertyValue::Float(2.5),
            PropertyValue::Sequence(vec![PropertyValue::Int(11), PropertyValue::Int(22)]),
            PropertyValue::FixedDict(Some(vec![
                ("mana".to_owned(), PropertyValue::Uint(900)),
                ("stamina".to_owned(), PropertyValue::Int(-4)),
            ])),
            PropertyValue::MailBox {
                id: 42,
                ip: 0x0102_0304,
                port: 20933,
                salt: 0x0103,
            },
            PropertyValue::Vector(vec![1.5, -2.25, 3.0]),
            PropertyValue::Vector(vec![0.0, 1.0, 0.0]),
            PropertyValue::Uint(5),
        ]
    }

    /// A fixture file with one known Avatar row in `tbl_flip` and nothing
    /// else.
    fn single_row_fixture() -> SecondaryDatabase {
        fixture_file(
            &[(FLIP_TABLE, &[(7, 1, 900, avatar_values("FantasyHero"))])],
            Some("digest-1"),
        )
    }

    /// One fixture generation: a table name and its
    /// (dbID, typeID, time, values) rows.
    type FixtureGeneration<'a> = (&'a str, &'a [(i64, i64, i64, Vec<PropertyValue>)]);

    /// Build a real temporary secondary database file from (table, rows)
    /// generations.
    fn fixture_file(
        generations: &[FixtureGeneration<'_>],
        checksum: Option<&str>,
    ) -> SecondaryDatabase {
        let path = unique_path();

        let connection = rusqlite::Connection::open(&path).expect("the fixture file opens");
        if let Some(checksum) = checksum {
            connection
                .execute(
                    &format!("CREATE TABLE {CHECKSUM_TABLE} ({CHECKSUM_COLUMN} TEXT)"),
                    [],
                )
                .expect("the checksum table creates");
            connection
                .execute(
                    &format!("INSERT INTO {CHECKSUM_TABLE} VALUES (\"{checksum}\")"),
                    [],
                )
                .expect("the checksum inserts");
        }
        for (table, rows) in generations {
            create_tables(&connection, table);
            for (db_id, type_id, time, values) in *rows {
                let blob = values_to_blob(values, &avatar_def());
                insert_row(&connection, table, *db_id, *type_id, *time, &blob);
            }
        }
        drop(connection);

        SecondaryDatabase::open(&path).expect("the fixture reads")
    }

    #[test]
    fn a_known_record_imports_losslessly() {
        let database = single_row_fixture();
        assert_eq!(database.num_entities(), 1);
        assert_eq!(
            database.checksum_digest().expect("the digest reads"),
            "digest-1"
        );

        let table = &database.tables()[0];
        assert_eq!(table.name, FLIP_TABLE);
        assert_eq!(table.num_rows, 1);
        assert_eq!(table.first_game_time, 900);

        let rows = database.rows(table).expect("the rows read");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].db_id, 7);
        assert_eq!(rows[0].type_id, 1);
        assert_eq!(rows[0].time, 900);
        assert_eq!(
            rows[0].values(&avatar_def()).expect("the blob decodes"),
            avatar_values("FantasyHero"),
            "every property survives the file"
        );
    }

    #[test]
    fn every_stream_kind_round_trips_through_a_file() {
        let values = vec![
            PropertyValue::Int(-128),
            PropertyValue::Int(-32768),
            PropertyValue::Int(-2_147_483_648),
            PropertyValue::Int(-9_223_372_036_854_775_808),
            PropertyValue::Uint(255),
            PropertyValue::Uint(65_535),
            PropertyValue::Uint(4_000_000_000),
            PropertyValue::Uint(u64::MAX),
            PropertyValue::Float(1.25),
            PropertyValue::Float(-0.5),
            PropertyValue::String("héllo".to_owned()),
            PropertyValue::Blob(vec![0u8, 255, 7]),
            PropertyValue::Vector(vec![1.0, 2.0]),
            PropertyValue::Vector(vec![1.0, 2.0, 3.0]),
            PropertyValue::Vector(vec![1.0, 2.0, 3.0, 4.0]),
            PropertyValue::Sequence(vec![]),
            PropertyValue::FixedDict(Some(vec![("mana".to_owned(), PropertyValue::Uint(65535))])),
        ];
        let def = StreamTypeDef {
            name: "Everything".to_owned(),
            type_id: 9,
            properties: vec![
                prop("a", StreamKind::Int8),
                prop("b", StreamKind::Int16),
                prop("c", StreamKind::Int32),
                prop("d", StreamKind::Int64),
                prop("e", StreamKind::Uint8),
                prop("f", StreamKind::Uint16),
                prop("g", StreamKind::Uint32),
                prop("h", StreamKind::Uint64),
                prop("i", StreamKind::Float32),
                prop("j", StreamKind::Float64),
                prop("k", StreamKind::String),
                prop("l", StreamKind::Blob),
                prop("m", StreamKind::Vector2),
                prop("n", StreamKind::Vector3),
                prop("o", StreamKind::Vector4),
                prop("p", StreamKind::Sequence(Box::new(StreamKind::Int8))),
                prop(
                    "q",
                    StreamKind::FixedDict(vec![("mana".to_owned(), StreamKind::Uint16)]),
                ),
            ],
        };

        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).expect("the file opens");
        create_tables(&connection, FLOP_TABLE);
        let blob = values_to_blob(&values, &def);
        insert_row(&connection, FLOP_TABLE, 3, 9, 5, &blob);
        drop(connection);

        let database = SecondaryDatabase::open(&path).expect("the file reads");
        let rows = database.rows(&database.tables()[0]).expect("the rows read");
        assert_eq!(rows[0].values(&def).expect("the blob decodes"), values);
    }

    #[test]
    fn the_older_table_consolidates_first() {
        // flip's first row is older than flop's, so flip sorts first even
        // though flop was probed second.
        let database = fixture_file(
            &[
                (FLIP_TABLE, &[(1, 1, 100, avatar_values("older"))]),
                (FLOP_TABLE, &[(1, 1, 200, avatar_values("newer"))]),
            ],
            None,
        );
        let names: Vec<&str> = database.tables().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec![FLIP_TABLE, FLOP_TABLE]);
        assert_eq!(database.tables()[0].first_game_time, 100);
        assert_eq!(database.tables()[1].first_game_time, 200);
        assert_eq!(database.num_entities(), 2);

        // The reverse ages flip the order too.
        let database = fixture_file(
            &[
                (FLIP_TABLE, &[(1, 1, 300, avatar_values("newer"))]),
                (FLOP_TABLE, &[(1, 1, 50, avatar_values("older"))]),
            ],
            None,
        );
        let names: Vec<&str> = database.tables().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec![FLOP_TABLE, FLIP_TABLE]);
    }

    #[test]
    fn an_empty_table_has_time_zero_and_counts_zero() {
        let database = fixture_file(
            &[
                (FLIP_TABLE, &[]),
                (FLOP_TABLE, &[(4, 1, 10, avatar_values("x"))]),
            ],
            None,
        );
        assert_eq!(database.tables()[0].name, FLIP_TABLE);
        assert_eq!(database.tables()[0].first_game_time, 0);
        assert_eq!(database.tables()[0].num_rows, 0);
        assert_eq!(database.tables()[1].first_game_time, 10);
        assert_eq!(database.num_entities(), 1);
        assert!(
            database
                .rows(&database.tables()[0])
                .expect("the rows read")
                .is_empty()
        );
    }

    #[test]
    fn a_file_with_only_flip_or_only_flop_reads_what_it_has() {
        let database = fixture_file(&[(FLOP_TABLE, &[(6, 1, 1, avatar_values("x"))])], None);
        assert_eq!(database.tables().len(), 1);
        assert_eq!(database.tables()[0].name, FLOP_TABLE);
        assert_eq!(database.num_entities(), 1);

        let database = fixture_file(&[], None);
        assert_eq!(database.tables().len(), 0);
        assert_eq!(database.num_entities(), 0);
    }

    #[test]
    fn a_missing_or_empty_checksum_table_is_an_error() {
        let database = fixture_file(&[(FLIP_TABLE, &[])], None);
        assert!(
            database.checksum_digest().is_err(),
            "no checksum table at all"
        );

        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).expect("the file opens");
        create_tables(&connection, FLIP_TABLE);
        drop(connection);
        let database = SecondaryDatabase::open(&path).expect("the file reads");
        assert!(
            database.checksum_digest().is_err(),
            "an empty checksum table has no row"
        );
    }

    #[test]
    fn a_missing_file_fails_the_open() {
        assert!(SecondaryDatabase::open("/nonexistent/secondary.db").is_err());
    }

    #[test]
    fn a_truncated_or_trailing_blob_fails_the_decode() {
        let def = avatar_def();
        let full = values_to_blob(&avatar_values("x"), &def);
        assert!(
            blob_to_values(&full[..full.len() - 1], &def).is_err(),
            "a short stream cannot decode"
        );
        let mut trailing = full.clone();
        trailing.push(0);
        assert!(
            blob_to_values(&trailing, &def).is_err(),
            "a trailing byte is a corrupt payload"
        );
        let error = blob_to_values(&[], &def).expect_err("the empty blob fails");
        assert!(
            error.to_string().contains("secondary db: 'playerName'"),
            "the error names the property: {error}"
        );
    }

    #[test]
    fn packed_string_lengths_round_trip_across_the_255_boundary() {
        for len in [0usize, 1, 254, 255, 256, 1000] {
            let text = "x".repeat(len);
            let def = StreamTypeDef {
                name: "S".to_owned(),
                type_id: 1,
                properties: vec![prop("name", StreamKind::String)],
            };
            let blob = values_to_blob(&[PropertyValue::String(text.clone())], &def);
            assert_eq!(
                blob_to_values(&blob, &def).expect("the blob decodes"),
                vec![PropertyValue::String(text)],
                "length {len} round-trips"
            );
            // The packed length itself: 1 byte below 0xFF, 4 bytes from it.
            let expected_len_len = if len >= 0xFF { 4 } else { 1 };
            assert_eq!(blob.len(), len + expected_len_len);
        }
    }

    #[test]
    fn a_row_decodes_into_a_record() {
        let database = fixture_file(
            &[(FLIP_TABLE, &[(11, 1, 60, avatar_values("Recorded"))])],
            None,
        );
        let rows = database.rows(&database.tables()[0]).expect("the rows read");
        let record = rows[0].record(&avatar_def()).expect("the record decodes");
        assert_eq!(record.type_id, 1);
        assert_eq!(record.db_id, 11);
        assert_eq!(record.values, avatar_values("Recorded"));
    }

    #[test]
    fn the_mailbox_streams_as_twelve_bytes() {
        let def = StreamTypeDef {
            name: "M".to_owned(),
            type_id: 1,
            properties: vec![prop("bind", StreamKind::MailBox)],
        };
        let value = PropertyValue::MailBox {
            id: -1,
            ip: 0xFFFF_FFFF,
            port: 0xFFFF,
            salt: 0xFFFF,
        };
        let blob = values_to_blob(std::slice::from_ref(&value), &def);
        assert_eq!(blob.len(), 12, "id u32 + ip u32 + port u16 + salt u16");
        assert_eq!(
            blob_to_values(&blob, &def).expect("the blob decodes"),
            vec![value]
        );
    }

    #[test]
    fn a_mailbox_value_decodes_like_the_reference_reader() {
        // `EntityMailBoxRef` streams `id, ip, port, salt`
        // (`basictypes.hpp:322-334`); decode those bytes by hand.
        let def = StreamTypeDef {
            name: "M".to_owned(),
            type_id: 1,
            properties: vec![prop("bind", StreamKind::MailBox)],
        };
        let mut blob = Vec::new();
        blob.extend_from_slice(&42u32.to_le_bytes());
        blob.extend_from_slice(&0x0102_0304u32.to_le_bytes());
        blob.extend_from_slice(&20933u16.to_le_bytes());
        blob.extend_from_slice(&0x0103u16.to_le_bytes());
        assert_eq!(
            blob_to_values(&blob, &def).expect("the blob decodes"),
            vec![PropertyValue::MailBox {
                id: 42,
                ip: 0x0102_0304,
                port: 20933,
                salt: 0x0103,
            }]
        );
    }

    #[test]
    fn rows_come_out_in_insertion_order() {
        let database = fixture_file(
            &[
                (
                    FLIP_TABLE,
                    &[
                        (3, 1, 10, avatar_values("first")),
                        (1, 1, 20, avatar_values("second")),
                        (2, 1, 30, avatar_values("third")),
                    ],
                ),
                (FLOP_TABLE, &[]),
            ],
            None,
        );
        // The empty flop sorts first (time 0), so address flip by name.
        let flip = database
            .tables()
            .iter()
            .find(|table| table.name == FLIP_TABLE)
            .expect("the flip table is listed");
        let rows = database.rows(flip).expect("the rows read");
        let db_ids: Vec<i64> = rows.iter().map(|row| row.db_id).collect();
        assert_eq!(db_ids, vec![3, 1, 2], "rowid order, not id order");
    }

    #[test]
    fn unicode_strings_stream_like_strings() {
        let def = StreamTypeDef {
            name: "U".to_owned(),
            type_id: 1,
            properties: vec![prop("title", StreamKind::UnicodeString)],
        };
        let values = vec![PropertyValue::String("英雄".to_owned())];
        let blob = values_to_blob(&values, &def);
        assert_eq!(
            blob_to_values(&blob, &def).expect("the blob decodes"),
            values
        );
    }

    #[test]
    fn negative_and_wide_integers_keep_their_widths() {
        let def = StreamTypeDef {
            name: "I".to_owned(),
            type_id: 1,
            properties: vec![
                prop("a", StreamKind::Int8),
                prop("b", StreamKind::Int16),
                prop("c", StreamKind::Int32),
                prop("d", StreamKind::Int64),
            ],
        };
        let values = vec![
            PropertyValue::Int(-1),
            PropertyValue::Int(-2),
            PropertyValue::Int(-3),
            PropertyValue::Int(-4),
        ];
        let blob = values_to_blob(&values, &def);
        assert_eq!(blob.len(), 1 + 2 + 4 + 8, "each kind writes its width");
        // -1 in one byte is 0xFF, the packed-int escape — but integers are
        // raw, so nothing reads it as a length.
        assert_eq!(blob[0], 0xFF);
        assert_eq!(
            blob_to_values(&blob, &def).expect("the blob decodes"),
            values
        );
    }

    #[test]
    fn the_row_face_reads_raw_columns_with_extreme_values() {
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).expect("the file opens");
        create_tables(&connection, FLIP_TABLE);
        insert_row(&connection, FLIP_TABLE, i64::MAX, 65_535, i64::MAX, b"\x00");
        insert_row(&connection, FLIP_TABLE, -5, -1, -1, b"");
        drop(connection);

        let database = SecondaryDatabase::open(&path).expect("the file reads");
        let rows = database.rows(&database.tables()[0]).expect("the rows read");
        assert_eq!(rows[0].db_id, i64::MAX);
        assert_eq!(rows[0].type_id, EntityTypeId::MAX);
        assert_eq!(rows[0].time, GameTime::MAX);
        assert_eq!(rows[0].blob, b"\x00".to_vec());
        assert_eq!(rows[1].db_id, -5);
        assert_eq!(rows[1].type_id, 0, "a negative typeID clamps to zero");
        assert_eq!(rows[1].time, 0, "a negative time clamps to zero");
        assert!(rows[1].blob.is_empty());
    }

    /// A file whose flip table holds one good Avatar row followed by one
    /// row with the given raw columns (SQLite is dynamically typed, so a
    /// NULL or TEXT rides in an INTEGER column just fine).
    fn good_row_then_raw_columns(second: &[&dyn rusqlite::ToSql]) -> SecondaryDatabase {
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).expect("the fixture file opens");
        create_tables(&connection, FLIP_TABLE);
        let blob = values_to_blob(&avatar_values("good"), &avatar_def());
        insert_row(&connection, FLIP_TABLE, 1, 1, 100, &blob);
        connection
            .execute(
                &format!("INSERT INTO {FLIP_TABLE} VALUES (?, ?, ?, ?)"),
                second,
            )
            .expect("the raw row inserts");
        drop(connection);
        SecondaryDatabase::open(&path).expect("the file reads")
    }

    #[test]
    fn a_null_dbid_fails_the_row_read() {
        let database =
            good_row_then_raw_columns(&[&rusqlite::types::Null, &1i64, &100i64, &vec![1u8, 2, 3]]);
        let error = database
            .rows(&database.tables()[0])
            .expect_err("a NULL sm_dbID cannot decode");
        assert!(error.to_string().contains("dbID column"), "{error}");
    }

    #[test]
    fn a_text_typeid_fails_the_row_read() {
        let database =
            good_row_then_raw_columns(&[&2i64, &"avatar".to_owned(), &100i64, &vec![1u8, 2, 3]]);
        let error = database
            .rows(&database.tables()[0])
            .expect_err("a TEXT sm_typeID cannot decode");
        assert!(error.to_string().contains("typeID column"), "{error}");
    }

    #[test]
    fn a_null_time_fails_the_open() {
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).expect("the fixture file opens");
        create_tables(&connection, FLIP_TABLE);
        connection
            .execute(
                &format!("INSERT INTO {FLIP_TABLE} VALUES (?, ?, ?, ?)"),
                rusqlite::params![1i64, 1, rusqlite::types::Null, vec![1u8]],
            )
            .expect("the raw row inserts");
        drop(connection);

        let error = SecondaryDatabase::open(&path)
            .expect_err("a NULL sm_time cannot read as the first game time");
        assert!(error.to_string().contains("time read failed"), "{error}");
    }

    #[test]
    fn a_null_blob_fails_the_row_read() {
        let database = good_row_then_raw_columns(&[&2i64, &1i64, &100i64, &rusqlite::types::Null]);
        let error = database
            .rows(&database.tables()[0])
            .expect_err("a NULL sm_blob cannot decode");
        assert!(error.to_string().contains("blob column"), "{error}");
    }

    #[test]
    fn a_null_time_on_a_later_row_fails_the_row_read() {
        // The first row's time feeds firstGameTime; a NULL time on a later
        // row only trips the row read itself.
        let database =
            good_row_then_raw_columns(&[&2i64, &1i64, &rusqlite::types::Null, &vec![1u8, 2, 3]]);
        let error = database
            .rows(&database.tables()[0])
            .expect_err("a NULL sm_time cannot decode");
        assert!(error.to_string().contains("time column"), "{error}");
    }

    #[test]
    fn a_table_dropped_after_the_open_fails_the_row_read() {
        let path = unique_path();
        {
            let connection = rusqlite::Connection::open(&path).expect("the fixture file opens");
            create_tables(&connection, FLIP_TABLE);
            insert_row(&connection, FLIP_TABLE, 1, 1, 100, b"\x01");
        }
        let database = SecondaryDatabase::open(&path).expect("the file reads");

        // A second writer drops the table out from under the open reader.
        let connection = rusqlite::Connection::open(&path).expect("the writer opens");
        connection
            .execute(&format!("DROP TABLE {FLIP_TABLE}"), [])
            .expect("the drop commits");
        drop(connection);

        let error = database
            .rows(&database.tables()[0])
            .expect_err("the table is gone");
        assert!(error.to_string().contains("step failed"), "{error}");
    }

    #[test]
    fn a_failure_inside_a_fixed_dict_reports_through_the_property() {
        let def = StreamTypeDef {
            name: "D".to_owned(),
            type_id: 1,
            properties: vec![prop(
                "stats",
                StreamKind::FixedDict(vec![("label".to_owned(), StreamKind::String)]),
            )],
        };
        let error = blob_to_values(&[], &def).expect_err("the empty blob fails");
        assert!(error.to_string().contains("'stats'"), "{error}");
    }

    #[test]
    fn a_value_that_does_not_match_its_kind_writes_nothing() {
        let def = StreamTypeDef {
            name: "M".to_owned(),
            type_id: 1,
            properties: vec![prop("name", StreamKind::String)],
        };
        let blob = values_to_blob(&[PropertyValue::Int(5)], &def);
        assert!(blob.is_empty(), "the mismatch writes nothing");
        assert!(blob_to_values(&blob, &def).is_err());
    }
}
