//! The schema dry run (`server/tools/sync_db/mysql_dry_run_wrapper.cpp`).
//!
//! The reference's `--dry-run` runs the schema tools against the live
//! database with one rule: read-only statements (`SELECT` without `FOR
//! UPDATE` / `LOCK IN SHARE MODE`, and `SHOW`) execute normally — the
//! tools must inspect the catalog to know what is missing — and every
//! other statement is printed as `DRYRUN:\t%s;` instead of executing.
//! The database comes out unchanged and the report is the list of
//! statements that would have run.
//!
//! This module carries that rule for PostgreSQL plus the schema diff
//! that produces the plan: the fixed tables, their indexes and the
//! version seed that a not-yet-initialised database is missing.

use db::DbError;
use postgres::Client;

use crate::schema::quote_ident;
use crate::schema_init;

/// `MySqlDryRunWrapper::isReadOnlyStatement`: a statement the dry run
/// may execute, because it only reads. Case-insensitive on the leading
/// keyword; `SELECT` is rejected when it locks (`FOR UPDATE`,
/// `LOCK IN SHARE MODE`).
pub fn is_read_only_statement(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head = trimmed
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match head.as_str() {
        "select" => {
            let upper = trimmed.to_ascii_lowercase();
            !upper.contains("for update") && !upper.contains("lock in share mode")
        }
        "show" | "explain" => true,
        _ => false,
    }
}

/// Formats the report lines the way the reference prints them:
/// `DRYRUN:\t<statement>;` per statement.
pub fn dry_run_lines(statements: &[String]) -> Vec<String> {
    statements
        .iter()
        .map(|statement| format!("DRYRUN:\t{statement};"))
        .collect()
}

/// The statements a database is missing to reach the initialisation
/// target: fixed tables that do not exist yet, their indexes, and the
/// version seed when the version table has no row. Statements for
/// objects that already exist are left out, so the plan of an
/// up-to-date database is empty.
pub fn plan_missing_schema(client: &mut Client) -> Result<Vec<String>, DbError> {
    let mut missing = Vec::new();
    let table_present = |client: &mut Client, table: &str| -> Result<bool, DbError> {
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM pg_tables WHERE schemaname = 'public' \
             AND tablename = $1)"
        );
        // pg_tables carries the unquoted name.
        let name = table.trim_matches('"');
        let row = client
            .query_one(sql.as_str(), &[&name])
            .map_err(crate::store::map_db_error)?;
        Ok(row.get(0))
    };

    for statement in schema_init::fixed_table_ddl()
        .into_iter()
        .filter(|statement| statement.starts_with("CREATE TABLE IF NOT EXISTS"))
    {
        let table = statement
            .strip_prefix("CREATE TABLE IF NOT EXISTS \"")
            .and_then(|rest| rest.split('"').next())
            .map(str::to_string)
            .expect("fixed table DDL is well formed");
        if !table_present(client, &table)? {
            missing.push(statement);
        }
    }

    let index_present = |client: &mut Client, index: &str| -> Result<bool, DbError> {
        let sql = "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'public' \
                   AND indexname = $1)";
        let row = client
            .query_one(sql, &[&index])
            .map_err(crate::store::map_db_error)?;
        Ok(row.get(0))
    };
    for statement in fixed_index_ddl() {
        let index = statement
            .strip_prefix("CREATE INDEX IF NOT EXISTS \"")
            .and_then(|rest| rest.split('"').next())
            .map(str::to_string)
            .expect("index DDL is well formed");
        if !index_present(client, &index)? {
            missing.push(statement);
        }
    }

    // The version seed: when the version table is missing (it will
    // exist once the plan applies) or carries no version row yet.
    let info_present = table_present(client, "bigworldInfo")?;
    if !info_present || schema_init::read_version(client)?.is_none() {
        missing.push(schema_init::version_seed_ddl());
    }

    Ok(missing)
}

/// The fixed indexes, split out of [`schema_init::fixed_table_ddl`] so
/// the diff can check them against `pg_indexes`.
fn fixed_index_ddl() -> Vec<String> {
    schema_init::fixed_table_ddl()
        .into_iter()
        .filter(|statement| statement.starts_with("CREATE INDEX IF NOT EXISTS"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_execute_and_writes_do_not() {
        // The reference rule: SELECT and SHOW run, everything else is
        // reported.
        assert!(is_read_only_statement("SELECT version FROM bigworldInfo"));
        assert!(is_read_only_statement("  select 1"));
        assert!(is_read_only_statement("SHOW TABLES"));
        assert!(is_read_only_statement("EXPLAIN SELECT 1"));
        assert!(!is_read_only_statement(
            "CREATE TABLE IF NOT EXISTS bigworldInfo (version INT)"
        ));
        assert!(!is_read_only_statement(
            "INSERT INTO bigworldInfo (version) VALUES (9)"
        ));
        assert!(!is_read_only_statement(
            "UPDATE bigworldInfo SET version = 9"
        ));
        assert!(!is_read_only_statement("DELETE FROM bigworldInfo"));
        assert!(!is_read_only_statement("DROP TABLE bigworldInfo"));
        assert!(!is_read_only_statement(
            "ALTER TABLE bigworldInfo ADD COLUMN x INT"
        ));
    }

    #[test]
    fn locking_selects_are_not_read_only() {
        assert!(!is_read_only_statement(
            "SELECT version FROM bigworldInfo FOR UPDATE"
        ));
        assert!(!is_read_only_statement(
            "SELECT version FROM bigworldInfo LOCK IN SHARE MODE"
        ));
    }

    #[test]
    fn the_report_carries_the_reference_prefix() {
        let lines = dry_run_lines(&["CREATE TABLE x (id INT)".to_string()]);
        assert_eq!(lines, vec!["DRYRUN:\tCREATE TABLE x (id INT);"]);
    }
}
