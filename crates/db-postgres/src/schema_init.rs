//! Database initialisation: the fixed `bigworld*` tables and the schema
//! version (`server/tools/sync_db/mysql_synchronise.cpp`
//! `createSpecialBigWorldTables`, `lib/db_storage_mysql/versions.hpp`).
//!
//! The reference creates its fixed tables with `CREATE TABLE IF NOT
//! EXISTS` on every sync, then seeds `bigworldInfo.version` when the
//! table has no row (`getBigWorldDBVersion`). A freshly created database
//! is therefore "current version, empty" and the DBApp can start against
//! it. This module carries the same statements for PostgreSQL: the same
//! table and column names, the reference's `INT` types widened where
//! PostgreSQL has no unsigned form, `BLOB`/`VARBINARY` as `BYTEA`, and
//! `AUTO_INCREMENT` as the identity column.
//!
//! The initialisation is planned first (`plan`), so the dry-run item
//! (BW-09-07-02) can print the statements without executing them and the
//! apply path (this item) executes exactly the plan.

use db::DbError;
use postgres::Client;

use crate::accounts::account_table_ddl;
use crate::schema::quote_ident;

/// `DBAPP_CURRENT_VERSION` (`versions.hpp`): the schema this build
/// writes and supports.
pub const CURRENT_VERSION: u32 = 9;

/// `DBAPP_OLDEST_SUPPORTED_VERSION` (`versions.hpp`).
pub const OLDEST_SUPPORTED_VERSION: u32 = 1;

/// The fixed tables the reference creates, in its own order. Each
/// statement is `CREATE TABLE IF NOT EXISTS`, so re-running the
/// initialisation on a live database changes nothing.
pub fn fixed_table_ddl() -> Vec<String> {
    let mut statements = Vec::new();

    // Version table.
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} BIGINT NOT NULL, {} TIMESTAMPTZ NULL, \
         {} BOOLEAN NOT NULL DEFAULT FALSE)",
        quote_ident("bigworldInfo"),
        quote_ident("version"),
        quote_ident("snapshotTime"),
        quote_ident("isPasswordHashed")
    ));

    // Metadata tables.
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} BIGSERIAL NOT NULL PRIMARY KEY, \
         {} BIGINT, {} VARCHAR(255) NOT NULL UNIQUE)",
        quote_ident("bigworldEntityTypes"),
        quote_ident("typeID"),
        quote_ident("bigworldID"),
        quote_ident("name")
    ));

    // Logon/checkout tables.
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} BIGINT NOT NULL, {} INTEGER NOT NULL, \
         {} INTEGER, {} BIGINT, {} INTEGER, {} INTEGER, \
         {} BOOLEAN NOT NULL DEFAULT FALSE, PRIMARY KEY ({}, {}))",
        quote_ident("bigworldLogOns"),
        quote_ident("databaseID"),
        quote_ident("typeID"),
        quote_ident("objectID"),
        quote_ident("ip"),
        quote_ident("port"),
        quote_ident("salt"),
        quote_ident("shouldAutoLoad"),
        quote_ident("typeID"),
        quote_ident("databaseID")
    ));
    statements.push(account_table_ddl());

    // Entity ID tables.
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} INTEGER NOT NULL)",
        quote_ident("bigworldNewID"),
        quote_ident("id")
    ));
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} INTEGER NOT NULL)",
        quote_ident("bigworldUsedIDs"),
        quote_ident("id")
    ));

    // Game time.
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} INTEGER NOT NULL)",
        quote_ident("bigworldGameTime"),
        quote_ident("time")
    ));

    // Space data tables.
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} INTEGER NOT NULL UNIQUE)",
        quote_ident("bigworldSpaces"),
        quote_ident("id")
    ));
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} INTEGER NOT NULL, \
         {} BIGINT NOT NULL, {} INTEGER NOT NULL, {} BYTEA NOT NULL)",
        quote_ident("bigworldSpaceData"),
        quote_ident("id"),
        quote_ident("spaceEntryID"),
        quote_ident("entryKey"),
        quote_ident("data")
    ));
    statements.push(format!(
        "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
        quote_ident("bigworldSpaceData_id_idx"),
        quote_ident("bigworldSpaceData"),
        quote_ident("id")
    ));

    // Secondary databases.
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} BIGINT NOT NULL, {} INTEGER NOT NULL, \
         {} BYTEA NOT NULL)",
        quote_ident("bigworldSecondaryDatabases"),
        quote_ident("ip"),
        quote_ident("port"),
        quote_ident("location")
    ));
    statements.push(format!(
        "CREATE INDEX IF NOT EXISTS {} ON {} ({}, {})",
        quote_ident("bigworldSecondaryDatabases_addr_idx"),
        quote_ident("bigworldSecondaryDatabases"),
        quote_ident("ip"),
        quote_ident("port")
    ));

    // SQLite checksum table.
    statements.push(format!(
        "CREATE TABLE IF NOT EXISTS {} ({} VARCHAR(255))",
        quote_ident("bigworldEntityDefsChecksum"),
        quote_ident("checksum")
    ));

    statements
}

/// The version seed: `getBigWorldDBVersion` inserts the current version
/// when the table has no row, so a freshly created database is "current
/// version, empty". Guarded, because the seed must not add a second row
/// when one is already there.
pub fn version_seed_ddl() -> String {
    format!(
        "INSERT INTO {} ({}) SELECT {} WHERE NOT EXISTS (SELECT 1 FROM {})",
        quote_ident("bigworldInfo"),
        quote_ident("version"),
        CURRENT_VERSION,
        quote_ident("bigworldInfo")
    )
}

/// The whole initialisation plan, in execution order: every fixed table,
/// then the version seed.
pub fn plan() -> Vec<String> {
    let mut statements = fixed_table_ddl();
    statements.push(version_seed_ddl());
    statements
}

/// Reads the schema version: the seeded row's version, or `None` when
/// the database has never been initialised.
pub fn read_version(client: &mut Client) -> Result<Option<u32>, DbError> {
    let sql = format!(
        "SELECT {} FROM {}",
        quote_ident("version"),
        quote_ident("bigworldInfo")
    );
    let row = client
        .query_opt(sql.as_str(), &[])
        .map_err(crate::store::map_db_error)?;
    Ok(row.map(|row| row.get::<_, i64>(0) as u32))
}

/// Runs a plan's statements in order, on one connection.
pub fn apply(client: &mut Client, statements: &[String]) -> Result<(), DbError> {
    for statement in statements {
        client
            .execute(statement.as_str(), &[])
            .map_err(crate::store::map_db_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_creates_every_reference_table() {
        let plan = plan();
        // The reference creates eleven fixed tables (bigworldInfo,
        // bigworldEntityTypes, bigworldLogOns, bigworldLogOnMapping,
        // bigworldNewID, bigworldUsedIDs, bigworldGameTime, bigworldSpaces,
        // bigworldSpaceData, bigworldSecondaryDatabases,
        // bigworldEntityDefsChecksum) and two indexes, then seeds the
        // version.
        let created: Vec<String> = plan
            .iter()
            .filter_map(|statement| {
                statement
                    .strip_prefix("CREATE TABLE IF NOT EXISTS \"")
                    .and_then(|rest| rest.split('"').next())
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            created,
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
        assert_eq!(plan.len(), 11 + 2 + 1);
        assert!(
            plan.last()
                .unwrap()
                .starts_with("INSERT INTO \"bigworldInfo\"")
        );
    }

    #[test]
    fn every_statement_is_idempotent() {
        // Re-running the initialisation on a live database changes
        // nothing: every statement is guarded.
        for statement in plan() {
            assert!(
                statement.contains("IF NOT EXISTS") || statement.starts_with("INSERT INTO"),
                "unguarded statement: {statement}"
            );
        }
    }

    #[test]
    fn types_follow_the_reference_widened_for_postgres() {
        let plan = plan().join("\n");
        // INT UNSIGNED -> BIGINT (no unsigned form).
        assert!(plan.contains("\"version\" BIGINT NOT NULL"));
        // INT -> INTEGER, BIGINT -> BIGINT.
        assert!(plan.contains("\"databaseID\" BIGINT NOT NULL"));
        assert!(plan.contains("\"typeID\" INTEGER NOT NULL"));
        // SMALLINT UNSIGNED -> INTEGER.
        assert!(plan.contains("\"port\" INTEGER"));
        // BOOL -> BOOLEAN with the reference default.
        assert!(plan.contains("\"shouldAutoLoad\" BOOLEAN NOT NULL DEFAULT FALSE"));
        // CHAR/VARCHAR -> VARCHAR; VARBINARY/BLOB -> BYTEA.
        assert!(plan.contains("\"name\" VARCHAR(255) NOT NULL UNIQUE"));
        assert!(plan.contains("\"logOnName\" BYTEA NOT NULL PRIMARY KEY"));
        assert!(plan.contains("\"location\" BYTEA NOT NULL"));
        // AUTO_INCREMENT -> identity.
        assert!(plan.contains("\"typeID\" BIGSERIAL NOT NULL PRIMARY KEY"));
    }

    #[test]
    fn the_primary_keys_match_the_reference() {
        let plan = plan().join("\n");
        assert!(plan.contains("PRIMARY KEY (\"typeID\", \"databaseID\")"));
        assert!(plan.contains("\"logOnName\" BYTEA NOT NULL PRIMARY KEY"));
        assert!(plan.contains("\"id\" INTEGER NOT NULL UNIQUE"));
    }

    #[test]
    fn the_version_seed_is_guarded() {
        let seed = version_seed_ddl();
        assert_eq!(
            seed,
            "INSERT INTO \"bigworldInfo\" (\"version\") SELECT 9 \
             WHERE NOT EXISTS (SELECT 1 FROM \"bigworldInfo\")"
        );
        assert_eq!(CURRENT_VERSION, 9);
        assert_eq!(OLDEST_SUPPORTED_VERSION, 1);
    }
}
