//! Secondary (SQLite) database reader.
//!
//! The BaseApp spills pending entity writes into a rolling SQLite file
//! (`server/baseapp/sqlite_database.cpp`): two tables, `tbl_flip` and
//! `tbl_flop`, flip-flopped on every commit, plus a `tbl_checksum` table
//! holding the entity-defs digest. The consolidate tool then reads those
//! files back into the primary database
//! (`server/tools/consolidate_dbs/secondary_database.cpp` and
//! `secondary_database_table.cpp`).
//!
//! This module is the read half: open a secondary database file, discover
//! its data tables (oldest first, `tableAgeComparator`), report the
//! checksum digest and entity count, and stream the rows out losslessly
//! (`SELECT sm_dbID, sm_typeID, sm_time, sm_blob FROM <table>`,
//! `secondary_database_table.cpp:52-57`).
//!
//! Layering: this is a domain library (layer 2) next to
//! [`crate::xml_database`], which it can import into.

use std::path::Path;

use rusqlite::Connection;

use foundation::ids::{DatabaseId, EntityTypeId};
use foundation::stream::BinaryStream;

use crate::error::DbError;
use crate::xml_database::{AutoLoadUpdate, XmlDatabase};

/// The checksum table (`secondary_database.cpp:15`).
pub const CHECKSUM_TABLE_NAME: &str = "tbl_checksum";
/// The checksum column (`secondary_database.cpp:16`).
pub const CHECKSUM_COLUMN_NAME: &str = "sm_checksum";
/// The first rolling data table (`secondary_database.cpp:100`).
pub const FLIP_TABLE: &str = "tbl_flip";
/// The second rolling data table (`secondary_database.cpp:100`).
pub const FLOP_TABLE: &str = "tbl_flop";

/// The row layout is fixed by the BaseApp's create statement
/// (`sqlite_database.cpp:610-617`).
const DATA_QUERY: &str = "SELECT sm_dbID, sm_typeID, sm_time, sm_blob FROM ";

/// One pending entity write as stored in a secondary database row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryRow {
    /// The entity's database id (`sm_dbID`, bound as int64,
    /// `sqlite_database.cpp:69`).
    pub db_id: DatabaseId,
    /// The entity type id (`sm_typeID`, bound as int,
    /// `sqlite_database.cpp:76`).
    pub type_id: EntityTypeId,
    /// The game time of the write (`sm_time`, bound as int,
    /// `sqlite_database.cpp:83`).
    pub time: u32,
    /// The serialised entity properties (`sm_blob`).
    pub blob: Vec<u8>,
}

/// A data table discovered in a secondary database file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryTable {
    /// `tbl_flip` or `tbl_flop`.
    pub name: String,
    /// `SELECT COUNT(*)` (`secondary_database_table.cpp:74-79`).
    pub num_rows: i64,
    /// The game time of the first row, 0 when the table is empty
    /// (`SecondaryDatabaseTable::firstGameTime`,
    /// `secondary_database_table.cpp:87-99`).
    pub first_game_time: u32,
}

/// A reader over a BaseApp secondary database file
/// (`server/tools/consolidate_dbs/secondary_database.hpp`).
#[derive(Debug)]
pub struct SecondaryDatabase {
    conn: Connection,
    path: String,
    /// Oldest table first (`sortTablesByAge`,
    /// `secondary_database.cpp:136-141`).
    tables: Vec<SecondaryTable>,
    num_entities: i64,
}

impl SecondaryDatabase {
    /// Open the file and read its tables (`SecondaryDatabase::init`,
    /// `secondary_database.cpp:40-64`).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .map_err(|e| DbError::Storage(format!("could not open '{}': {e}", path.display())))?;

        let path = path.display().to_string();
        let mut database = Self {
            conn,
            path,
            tables: Vec::new(),
            num_entities: 0,
        };
        database.read_tables()?;
        Ok(database)
    }

    /// The file this database was opened from.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The discovered data tables, oldest first.
    pub fn tables(&self) -> &[SecondaryTable] {
        &self.tables
    }

    /// Total rows across the data tables (`numEntities`,
    /// `secondary_database.cpp:117-124`).
    pub fn num_entities(&self) -> i64 {
        self.num_entities
    }

    /// The entity-defs checksum stored in the file
    /// (`getChecksumDigest`, `secondary_database.cpp:67-90`). `None` when
    /// the table is missing or empty.
    pub fn checksum_digest(&self) -> Option<String> {
        let sql = format!("SELECT {CHECKSUM_COLUMN_NAME} FROM {CHECKSUM_TABLE_NAME}");
        let mut stmt = self.conn.prepare(&sql).ok()?;
        let mut rows = stmt.query([]).ok()?;
        match rows.next() {
            Ok(Some(row)) => row.get::<_, String>(0).ok(),
            // A missing or empty checksum table yields no digest.
            _ => None,
        }
    }

    /// Read the rows of one data table in stored order
    /// (`SecondaryDatabaseTable::consolidate`'s select,
    /// `secondary_database_table.cpp:52-57` and `:117-155`).
    pub fn read_table(&self, table: &str) -> Result<Vec<SecondaryRow>, DbError> {
        let sql = format!("{DATA_QUERY}{table}");
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| DbError::Storage(format!("could not read '{table}': {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SecondaryRow {
                    db_id: row.get(0)?,
                    type_id: row.get::<_, i64>(1)? as EntityTypeId,
                    time: row.get::<_, i64>(2)? as u32,
                    blob: row.get(3)?,
                })
            })
            .map_err(|e| DbError::Storage(format!("could not read '{table}': {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| DbError::Storage(format!("bad row: {e}")))?);
        }
        Ok(out)
    }

    /// Read every data table, oldest first
    /// (`SecondaryDatabase::consolidate`'s table walk,
    /// `secondary_database.cpp:180-207`).
    pub fn read_all(&self) -> Result<Vec<SecondaryRow>, DbError> {
        let mut out = Vec::new();
        for table in &self.tables {
            out.extend(self.read_table(&table.name)?);
        }
        Ok(out)
    }

    /// Import every row into an [`XmlDatabase`] in table age order, the
    /// consolidate step of the reference tool. Each row's blob is the
    /// entity's stream exactly as written, so the import is lossless.
    /// Returns the database ids that were written.
    pub fn import_into(&self, database: &mut XmlDatabase) -> Result<Vec<DatabaseId>, DbError> {
        let mut written = Vec::new();
        for row in self.read_all()? {
            let is_new = !database
                .get_entity_stream(row.type_id, row.db_id, None, &mut BinaryStream::new())
                .found;
            let mut stream = BinaryStream::from_slice(&row.blob);
            written.push(database.put_entity_stream(
                row.type_id,
                row.db_id,
                &mut stream,
                AutoLoadUpdate::Retain,
                is_new,
            )?);
        }
        Ok(written)
    }

    /// Discover the data tables and total the rows
    /// (`readTables`, `secondary_database.cpp:92-134`).
    fn read_tables(&mut self) -> Result<(), DbError> {
        self.num_entities = 0;
        self.tables.clear();

        for name in [FLIP_TABLE, FLOP_TABLE] {
            if !self.table_exists(name) {
                continue;
            }
            let num_rows = self.count_rows(name)?;
            let first_game_time = self.first_game_time(name)?;
            self.tables.push(SecondaryTable {
                name: name.to_string(),
                num_rows,
                first_game_time,
            });
            self.num_entities += num_rows;
        }

        // Have a good guess about which table is older and do the younger
        // one first (`secondary_database.cpp:136-141`).
        self.tables.sort_by_key(|table| table.first_game_time);
        Ok(())
    }

    /// `SELECT COUNT(*) FROM <table> WHERE 0` compiles only when the table
    /// exists (`tableExists`, `secondary_database.cpp:144-159`).
    fn table_exists(&self, table: &str) -> bool {
        self.conn
            .prepare(&format!("SELECT COUNT(*) FROM {table} WHERE 0"))
            .is_ok()
    }

    fn count_rows(&self, table: &str) -> Result<i64, DbError> {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        self.conn
            .query_row(&sql, [], |row| row.get(0))
            .map_err(|e| DbError::Storage(format!("could not count '{table}': {e}")))
            .map(|n: i64| n as i64)
    }

    /// The game time of the first row, or 0 for an empty table
    /// (`SecondaryDatabaseTable::firstGameTime`,
    /// `secondary_database_table.cpp:87-99`).
    fn first_game_time(&self, table: &str) -> Result<u32, DbError> {
        let sql = format!("{DATA_QUERY}{table}");
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| DbError::Storage(format!("could not read '{table}': {e}")))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| DbError::Storage(format!("could not step '{table}': {e}")))?;
        let first = match rows.next() {
            Ok(Some(row)) => Some(row),
            Ok(None) => None,
            Err(e) => return Err(DbError::Storage(format!("could not step '{table}': {e}"))),
        };
        match first {
            Some(row) => Ok(row
                .get::<_, i64>(2)
                .map_err(|e| DbError::Storage(e.to_string()))? as u32),
            // No rows, return 0 (`secondary_database_table.cpp:93-96`).
            None => Ok(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DbError;
    use rusqlite::params;
    use std::path::PathBuf;

    /// The BaseApp's own DDL (`writeChecksumTable` and `flipTable`,
    /// `sqlite_database.cpp:579-617`), used here to build fixtures that
    /// match what the reference writes.
    fn create_checksum_table(conn: &Connection, checksum: &str) {
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS {CHECKSUM_TABLE_NAME};
             CREATE TABLE {CHECKSUM_TABLE_NAME} ({CHECKSUM_COLUMN_NAME} TEXT);
             INSERT INTO {CHECKSUM_TABLE_NAME} VALUES (\"{checksum}\");"
        ))
        .expect("checksum table");
    }

    fn create_data_table(conn: &Connection, table: &str) {
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS {table};
             CREATE TABLE {table} (sm_dbID INTEGER, sm_typeID INTEGER, \
                 sm_time INTEGER, sm_blob BLOB);"
        ))
        .expect("data table");
    }

    fn insert_row(
        conn: &Connection,
        table: &str,
        db_id: i64,
        type_id: i64,
        time: i64,
        blob: &[u8],
    ) {
        conn.execute(
            &format!("INSERT INTO {table} VALUES (?, ?, ?, ?)"),
            params![db_id, type_id, time, blob],
        )
        .expect("insert");
    }

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "boundless-secondarydb-{}-{name}.sqlite",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            Self { path }
        }

        fn connect(&self) -> Connection {
            Connection::open(&self.path).expect("open fixture")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn reads_checksum_tables_and_rows() {
        let fixture = Fixture::new("basic");
        {
            let conn = fixture.connect();
            create_checksum_table(&conn, "abc123");
            create_data_table(&conn, FLIP_TABLE);
            insert_row(&conn, FLIP_TABLE, 7, 1, 100, b"moe-stream");
            insert_row(&conn, FLIP_TABLE, 8, 2, 200, &[0u8, 255, 1, 254]);
            create_data_table(&conn, FLOP_TABLE);
            insert_row(&conn, FLOP_TABLE, 9, 1, 50, b"older");
        }

        let database = SecondaryDatabase::open(&fixture.path).expect("open");
        assert_eq!(database.path(), fixture.path.display().to_string());
        assert_eq!(database.checksum_digest().as_deref(), Some("abc123"));
        assert_eq!(database.num_entities(), 3);

        // Oldest table first (`secondary_database.cpp:136-141`).
        let names: Vec<&str> = database.tables().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec![FLOP_TABLE, FLIP_TABLE]);
        assert_eq!(database.tables()[0].num_rows, 1);
        assert_eq!(database.tables()[0].first_game_time, 50);
        assert_eq!(database.tables()[1].num_rows, 2);
        assert_eq!(database.tables()[1].first_game_time, 100);

        let all = database.read_all().expect("read_all");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].db_id, 9);
        assert_eq!(all[0].type_id, 1);
        assert_eq!(all[0].time, 50);
        assert_eq!(all[0].blob, b"older");
        assert_eq!(all[1].blob, b"moe-stream");
        assert_eq!(all[2].blob, vec![0u8, 255, 1, 254]);
    }

    #[test]
    fn a_missing_file_opens_empty_and_an_unopenable_path_errors() {
        // `sqlite3_open` creates the file when it is missing
        // (`sqlite_connection.cpp:44-55`), so a fresh path yields an empty
        // secondary database rather than an error.
        let fixture = Fixture::new("created");
        let database = SecondaryDatabase::open(&fixture.path).expect("open");
        assert!(database.tables().is_empty());
        assert_eq!(database.num_entities(), 0);
        assert!(database.checksum_digest().is_none());

        // Only an unopenable path errors.
        let dir = std::env::temp_dir();
        let err = SecondaryDatabase::open(dir.join(".")).unwrap_err();
        assert!(
            matches!(err, DbError::Storage(ref m) if m.contains("could not open")),
            "{err:?}"
        );
    }

    #[test]
    fn a_database_without_data_tables_reads_empty() {
        let fixture = Fixture::new("no-tables");
        {
            let conn = fixture.connect();
            create_checksum_table(&conn, "deadbeef");
        }

        let database = SecondaryDatabase::open(&fixture.path).expect("open");
        assert!(database.tables().is_empty());
        assert_eq!(database.num_entities(), 0);
        assert_eq!(database.checksum_digest().as_deref(), Some("deadbeef"));
        assert!(database.read_all().expect("read_all").is_empty());
    }

    #[test]
    fn an_empty_table_reports_zero_age() {
        let fixture = Fixture::new("empty-table");
        {
            let conn = fixture.connect();
            create_data_table(&conn, FLIP_TABLE);
            create_data_table(&conn, FLOP_TABLE);
        }

        let database = SecondaryDatabase::open(&fixture.path).expect("open");
        // Both tables exist with no rows: the age comparator sees 0 for
        // both, so the reference's stable sort keeps the declared
        // flip-then-flop order (`secondary_database.cpp:136-141`).
        assert_eq!(database.num_entities(), 0);
        let names: Vec<&str> = database.tables().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec![FLIP_TABLE, FLOP_TABLE]);
        assert!(database.tables()[0].first_game_time == 0);
        assert!(database.checksum_digest().is_none());
    }

    #[test]
    fn a_missing_table_cannot_be_read() {
        let fixture = Fixture::new("no-such-table");
        {
            let conn = fixture.connect();
            create_data_table(&conn, FLIP_TABLE);
        }

        let database = SecondaryDatabase::open(&fixture.path).expect("open");
        let err = database.read_table("tbl_absent").unwrap_err();
        assert!(
            matches!(err, DbError::Storage(ref m) if m.contains("could not read")),
            "{err:?}"
        );
    }

    #[test]
    fn rows_import_losslessly_into_the_xml_database() {
        let defs = crate::xml_database::tests::defs();
        let avatar = defs.name_to_index("Avatar").unwrap();

        let fixture = Fixture::new("import");
        {
            let conn = fixture.connect();
            create_data_table(&conn, FLIP_TABLE);
            create_data_table(&conn, FLOP_TABLE);
            // Flip holds the older write, flop the newer one: the reader
            // must replay them in age order and the newer write wins.
            let older = crate::xml_database::tests::avatar_stream(42, 0);
            insert_row(&conn, FLIP_TABLE, 1, avatar as i64, 10, older.as_bytes());
            let newer = crate::xml_database::tests::avatar_stream(99, 0);
            insert_row(&conn, FLOP_TABLE, 1, avatar as i64, 20, newer.as_bytes());
        }

        let secondary = SecondaryDatabase::open(&fixture.path).expect("open");
        let mut xml = XmlDatabase::new(defs);
        let written = secondary.import_into(&mut xml).expect("import");

        // Both rows name database id 1: the create then the update.
        assert_eq!(written, vec![1, 1]);
        let outcome = xml.get_entity_stream(avatar, 1, None, &mut BinaryStream::new());
        assert!(outcome.found);
        assert_eq!(outcome.name.as_deref(), Some("avatar-99"));
    }
}
