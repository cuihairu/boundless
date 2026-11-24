//! Statement-log pins for the dry-run port. The double records what actually
//! reaches the connection; the reviewable artifact is that nothing does.

use super::*;
use crate::tools::sync_db::{
    special_bigworld_table_statements, ColumnIndexType, ColumnInfo, ColumnType, DescribedTable,
    DesiredTable, EngineSupportState, Field, FieldType, SyncConfig, Synchronise,
    AUTO_INCREMENT_FLAG, PRI_KEY_FLAG,
};

/// The connection a dry-run would otherwise get: it records what reaches it.
#[derive(Default)]
struct Recording {
    statements: Vec<String>,
    commits: usize,
    bigworld_tables: Vec<String>,
    entity_tables: Vec<String>,
    fail_execute_on: Vec<String>,
}

impl SyncDatabase for Recording {
    fn execute(&mut self, statement: &str) -> Result<(), String> {
        for needle in &self.fail_execute_on {
            if statement.contains(needle) {
                return Err(format!("execute failed: {statement}"));
            }
        }
        self.statements.push(statement.to_string());
        Ok(())
    }

    fn commit(&mut self) {
        self.commits += 1
    }

    fn engine_support_state(&mut self) -> EngineSupportState {
        EngineSupportState::Supported
    }

    fn check_table_engines(&mut self) -> bool {
        true
    }

    fn table_names(&mut self, pattern: &str) -> Vec<String> {
        if pattern == "bigworld%" {
            self.bigworld_tables.clone()
        } else {
            self.entity_tables.clone()
        }
    }

    fn describe_table(&mut self, _table: &str) -> DescribedTable {
        DescribedTable::default()
    }

    fn query_version(&mut self) -> Option<u32> {
        Some(9)
    }

    fn query_is_password_hashed(&mut self) -> Option<bool> {
        Some(true)
    }

    fn count_secondary_databases(&mut self) -> u32 {
        0
    }
}

fn special_names() -> Vec<String> {
    [
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
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn account_table() -> Vec<DesiredTable> {
    vec![DesiredTable::new("tbl_Account").with_column(
        "id",
        ColumnInfo {
            column_type: ColumnType::from_field(&Field {
                name: "id".to_string(),
                field_type: Some(FieldType::LongLong),
                flags: PRI_KEY_FLAG | AUTO_INCREMENT_FLAG,
                length: 8,
                charsetnr: 63,
                charset_width: 1,
                def: None,
            }),
            index_type: ColumnIndexType::Primary,
        },
    )]
}

// -----------------------------------------------------------------------------
// isReadOnlyStatement
// -----------------------------------------------------------------------------

#[test]
fn an_empty_statement_is_a_reported_error_and_not_read_only() {
    let mut log = Vec::new();
    assert!(!is_read_only_statement("", &mut log));
    assert_eq!(
        log,
        vec!["MySqlDryRunWrapper::isReadOnlyStatement: Invalid SQL statement: ''\n"]
    );
}

#[test]
fn a_whitespace_only_statement_is_the_same_reported_error() {
    let mut log = Vec::new();
    assert!(!is_read_only_statement(" \t ", &mut log));
    assert_eq!(
        log,
        vec!["MySqlDryRunWrapper::isReadOnlyStatement: Invalid SQL statement: ' \t '\n"]
    );
}

#[test]
fn only_space_and_tab_are_skipped_when_locating_the_first_token() {
    // A newline is not skipped, so it becomes the first token: not read-only.
    let mut log = Vec::new();
    assert!(!is_read_only_statement("\nSELECT 1", &mut log));
    assert!(log.is_empty());

    let mut log = Vec::new();
    assert!(is_read_only_statement("   \tselect 1", &mut log));
    assert!(log.is_empty());
}

#[test]
fn show_is_read_only_regardless_of_case() {
    let mut log = Vec::new();
    assert!(is_read_only_statement(
        "show columns from tbl_Account",
        &mut log
    ));
    assert!(is_read_only_statement(
        "SHOW INDEX FROM tbl_Account",
        &mut log
    ));
    assert!(log.is_empty());
}

#[test]
fn a_write_statement_is_not_read_only() {
    let mut log = Vec::new();
    for statement in [
        "insert into bigworldInfo (version) values (9)",
        "UPDATE bigworldLogOns SET ip = 1",
        "ALTER TABLE bigworldSpaceData MODIFY data BLOB",
        "CREATE TABLE tbl_Account (id BIGINT)",
        "DROP TABLE tbl_Old",
        "set autocommit = 1",
    ] {
        assert!(!is_read_only_statement(statement, &mut log), "{statement}");
    }
    assert!(log.is_empty());
}

#[test]
fn the_token_must_start_the_trimmed_statement() {
    // `str.find("SHOW") == 0`: a later occurrence does not count, and a column
    // merely named SHOW does not make a write statement read-only.
    let mut log = Vec::new();
    assert!(is_read_only_statement("SELECT SHOW FROM t", &mut log));
    assert!(!is_read_only_statement("DROP SHOW", &mut log));
    assert!(!is_read_only_statement("SHOPS", &mut log));
    assert!(log.is_empty());
}

#[test]
fn a_select_for_update_is_not_read_only() {
    let mut log = Vec::new();
    assert!(!is_read_only_statement(
        "SELECT * FROM bigworldUsedIDs FOR UPDATE",
        &mut log
    ));
    assert!(log.is_empty());
}

#[test]
fn a_select_lock_in_share_mode_is_not_read_only() {
    let mut log = Vec::new();
    assert!(!is_read_only_statement(
        "select a from t LOCK IN SHARE MODE",
        &mut log
    ));
    assert!(log.is_empty());
}

#[test]
fn the_lock_check_is_case_insensitive_and_anywhere_in_the_statement() {
    let mut log = Vec::new();
    assert!(!is_read_only_statement(
        "selECt * from t lock in sHARE mode",
        &mut log
    ));
    // The reference searches the whole trimmed statement, so even a literal
    // containing the phrase disqualifies the statement.
    assert!(!is_read_only_statement(
        "SELECT * FROM t WHERE note = 'FOR UPDATE'",
        &mut log
    ));
    assert!(log.is_empty());
}

#[test]
fn the_lock_check_is_an_exact_substring_search() {
    // `find("FOR UPDATE")` is not word-aware: an underscored column name does
    // not match, so such a statement stays read-only.
    let mut log = Vec::new();
    assert!(is_read_only_statement("select for_update from t", &mut log));
    assert!(log.is_empty());
}

#[test]
fn the_dry_run_line_is_rendered_verbatim() {
    assert_eq!(
        dry_run_line("DROP TABLE tbl_Old"),
        "DRYRUN:\tDROP TABLE tbl_Old;\n"
    );
}

// -----------------------------------------------------------------------------
// DryRunConnection
// -----------------------------------------------------------------------------

#[test]
fn a_non_read_only_execute_is_withheld_and_reported() {
    let mut conn = DryRunConnection::new(Recording::default());
    assert!(conn
        .execute_statement("CREATE TABLE tbl_Account (id BIGINT) ENGINE=InnoDB")
        .is_ok());
    assert_eq!(
        conn.report,
        ["DRYRUN:\tCREATE TABLE tbl_Account (id BIGINT) ENGINE=InnoDB;\n"]
    );
    assert!(conn.into_inner().statements.is_empty());
}

#[test]
fn a_read_only_execute_reaches_the_inner_connection() {
    let mut conn = DryRunConnection::new(Recording::default());
    assert!(conn
        .execute_statement("SHOW COLUMNS FROM tbl_Account")
        .is_ok());
    assert!(conn.report.is_empty());
    assert_eq!(
        conn.into_inner().statements,
        vec!["SHOW COLUMNS FROM tbl_Account"]
    );
}

#[test]
fn the_query_face_reports_and_returns_zero_for_a_withheld_statement() {
    let mut conn = DryRunConnection::new(Recording::default());
    assert_eq!(conn.query_statement("DROP TABLE tbl_Old"), Ok(0));
    assert_eq!(conn.report, ["DRYRUN:\tDROP TABLE tbl_Old;\n"]);
}

#[test]
fn the_query_face_runs_read_only_statements_and_propagates_failures() {
    let inner = Recording {
        fail_execute_on: vec!["SHOW".to_string()],
        ..Default::default()
    };
    let mut conn = DryRunConnection::new(inner);
    assert_eq!(conn.query_statement("SELECT 1"), Ok(0));
    assert!(matches!(
        conn.query_statement("SHOW INDEX FROM t"),
        Err(e) if e == "execute failed: SHOW INDEX FROM t"
    ));
    assert!(conn.report.is_empty());
}

#[test]
fn the_statement_filter_is_not_the_only_thing_the_wrapper_is() {
    let inner = Recording {
        bigworld_tables: special_names(),
        entity_tables: vec!["tbl_Account".to_string()],
        ..Default::default()
    };
    let mut conn = DryRunConnection::new(inner);
    assert_eq!(conn.engine_support_state(), EngineSupportState::Supported);
    assert!(conn.check_table_engines());
    assert_eq!(conn.table_names("tbl_%"), vec!["tbl_Account".to_string()]);
    assert_eq!(conn.table_names("bigworld%").len(), 11);
    assert_eq!(conn.query_version(), Some(9));
    assert_eq!(conn.query_is_password_hashed(), Some(true));
    assert_eq!(conn.count_secondary_databases(), 0);
    conn.commit();
    assert!(conn.upgrade_database(1, &mut Vec::new()).is_ok());
    assert_eq!(conn.into_inner().commits, 1);
}

#[test]
fn the_dry_run_reports_the_plan_and_leaves_the_database_alone() {
    // The acceptance clause: the plan is reported and nothing reaches the
    // connection.
    let inner = Recording {
        bigworld_tables: special_names(),
        ..Default::default()
    };
    let mut conn = DryRunConnection::new(inner);
    let mut log = Vec::new();
    let sync = Synchronise::new(SyncConfig::default(), true);
    assert!(sync.run(&mut conn, &account_table(), &mut log));
    assert!(conn
        .report
        .iter()
        .any(|line| line.starts_with("DRYRUN:\tCREATE TABLE IF NOT EXISTS bigworldInfo")));
    assert!(conn
        .report
        .iter()
        .any(|line| line.starts_with("DRYRUN:\tALTER DATABASE CHARACTER SET ? COLLATE ?")));
    assert!(conn
        .report
        .iter()
        .any(|line| line.starts_with("DRYRUN:\tCREATE TABLE IF NOT EXISTS tbl_Account")));
    // 12 special statements, the charset statement, and the one entity table.
    assert_eq!(
        conn.report.len(),
        special_bigworld_table_statements(2048).len() + 2
    );
    // The notices are the plan's prose half: the engine support notice for a
    // supported-but-not-default InnoDB, then the table creation.
    assert_eq!(
        log,
        [
            "MySqlSynchronise::doSynchronise: InnoDB is not the default MySQL storage engine. Add 'default-storage-engine=InnoDB' to /etc/my.cnf and restart the MySQL server\n",
            "\tCreating table tbl_Account\n",
        ]
    );
    // Nothing reached the database.
    assert!(conn.into_inner().statements.is_empty());
}
