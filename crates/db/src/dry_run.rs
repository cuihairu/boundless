//! The `sync_db --dry-run` face — `MySqlDryRunWrapper`
//! (`server/tools/sync_db/mysql_dry_run_wrapper.{hpp,cpp}`) and
//! `MySqlDryRunConnection` (`mysql_dry_run_connection.cpp`).
//!
//! A dry run swaps the MySQL connection for a wrapper that only lets
//! read-only statements through: `SHOW` and plain `SELECT` still run
//! against the real database (so the schema inspection reads the truth),
//! everything else — the whole DDL plan [`crate::sync_db`] computes — is
//! printed as `DRYRUN:\t<statement>;` and never executed. That is the
//! acceptance in both halves: the plan is reported, and the database is
//! byte-for-byte the same on the way out.
//!
//! The wrapper does not throw: the non-exception `query` face returns 0
//! for withheld statements, and the exception `execute` face simply
//! returns. Only the leading `SPACE` and `TAB` count as whitespace —
//! a statement that starts with a newline is not read-only, exactly as
//! `find_first_not_of( " \t" )` decides.

use crate::sync_db::SyncStep;

/// The line prefix the reference logs a withheld statement with
/// (`mysql_dry_run_wrapper.cpp:44` — `INFO_MSG( "DRYRUN:\t%s;\n", ... )`).
pub const DRY_RUN_PREFIX: &str = "DRYRUN:";

/// The empty-statement error text (`mysql_dry_run_wrapper.cpp:28-29`).
pub const INVALID_STATEMENT_MSG: &str =
    "MySqlDryRunWrapper::isReadOnlyStatement: Invalid SQL statement";

/// `MySqlDryRunWrapper::isReadOnlyStatement`
/// (`mysql_dry_run_wrapper.cpp:21-51`): true only for `SHOW` and for a
/// `SELECT` without `FOR UPDATE` or `LOCK IN SHARE MODE` — MySQL's
/// locking reads are writers in disguise. The prefix test runs on the
/// uppercased statement after `SPACE`/`TAB` leading whitespace.
pub fn is_read_only_statement(statement: &str) -> bool {
    let pos = match statement.find(|c| c != ' ' && c != '\t') {
        Some(pos) => pos,
        None => return false,
    };
    let upper = statement[pos..].to_ascii_uppercase();
    if upper.starts_with("SHOW") {
        return true;
    }
    if upper.starts_with("SELECT") {
        return !upper.contains("FOR UPDATE") && !upper.contains("LOCK IN SHARE MODE");
    }
    false
}

/// The dry-run wrapper: the state a dry run leaves behind — the
/// statements it withheld (the report) and nothing else. The real
/// database connection is the caller's; reads are handed to it through
/// the `run_read_only` closure and writes never reach it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DryRunWrapper {
    withheld: Vec<String>,
}

impl DryRunWrapper {
    /// A fresh wrapper.
    pub fn new() -> Self {
        Self::default()
    }

    /// The withheld statements, in arrival order — the plan the run
    /// would have executed.
    pub fn withheld(&self) -> &[String] {
        &self.withheld
    }

    /// The report lines, formatted exactly as the reference logs them.
    pub fn report_lines(&self) -> Vec<String> {
        self.withheld
            .iter()
            .map(|statement| format!("{DRY_RUN_PREFIX}\t{statement};"))
            .collect()
    }

    /// `execute` (`mysql_dry_run_wrapper.cpp:53-110`): read-only
    /// statements run through `run_read_only`; anything else is recorded
    /// and never reaches the writer. True when the statement ran.
    pub fn execute(&mut self, statement: &str, run_read_only: impl FnOnce(&str)) -> bool {
        if !is_read_only_statement(statement) {
            self.withheld.push(statement.to_owned());
            return false;
        }
        run_read_only(statement);
        true
    }

    /// `query` (`mysql_dry_run_wrapper.cpp:112-126`): the non-exception
    /// face — a withheld statement reports success (`0`) without running.
    pub fn query(&mut self, statement: &str, run_read_only: impl FnOnce(&str)) -> i32 {
        if self.execute(statement, run_read_only) {
            1
        } else {
            0
        }
    }
}

/// A dry run over a whole sync plan: the ordered steps
/// ([`SyncStep::CreateSpecialTables`] and friends) and the statements
/// each step would have issued, split into what the wrapper withheld
/// (the report) and what still ran against the database (the reads).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DryRunSyncReport {
    /// The withheld statements, in plan order.
    pub planned_statements: Vec<String>,
    /// The read-only statements the run executed.
    pub read_statements: Vec<String>,
}

/// Run `statements` through a dry-run wrapper
/// (`MySqlSynchronise::run` with `isDryRun_`): every DDL statement lands
/// in [`DryRunSyncReport::planned_statements`], every `SHOW`/`SELECT`
/// rides through to the caller's real reader.
pub fn dry_run_statements(statements: &[String]) -> DryRunSyncReport {
    let mut wrapper = DryRunWrapper::new();
    let mut read_statements = Vec::new();
    for statement in statements {
        let sink = &mut read_statements;
        wrapper.execute(statement, |read_only| sink.push(read_only.to_owned()));
    }
    DryRunSyncReport {
        planned_statements: wrapper.withheld().to_vec(),
        read_statements,
    }
}

/// What a dry run of the whole tool does with a plan
/// (`main.cpp`'s `isDryRun` wiring): the steps are computed as usual by
/// [`crate::sync_db::plan_sync`], but every statement behind them —
/// the special-table catalogue for [`SyncStep::CreateSpecialTables`],
/// [`SyncStep::AlterDatabaseCharset`]'s `ALTER`, the definitions plan —
/// is reported here, not executed.
pub fn dry_run_steps(steps: &[SyncStep], max_space_data_size: u32) -> DryRunSyncReport {
    let mut planned = Vec::new();
    let mut read = Vec::new();
    for step in steps {
        let statements: Vec<String> = match step {
            SyncStep::CreateSpecialTables => {
                crate::sync_db::special_table_statements(max_space_data_size)
            }
            SyncStep::AlterDatabaseCharset { statement } => vec![statement.clone()],
            // The hashing upgrade's two UPDATEs and the definitions leg
            // ride their own statements; the steps here name the action,
            // the caller's plan carries the SQL.
            SyncStep::UpgradeDatabase { .. }
            | SyncStep::EnablePasswordHashing
            | SyncStep::SyncEntityDefinitions => continue,
        };
        let mut split = dry_run_statements(&statements);
        planned.append(&mut split.planned_statements);
        read.append(&mut split.read_statements);
    }
    DryRunSyncReport {
        planned_statements: planned,
        read_statements: read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namer::ColumnIndexType;
    use crate::sync_db::{
        ColumnInfo, ColumnType, SyncPlanRequest, SyncStep, TableAction, plan_statements,
        special_table_statements,
    };
    use std::collections::BTreeMap;

    fn plain(field_type: crate::sync_db::FieldType) -> ColumnInfo {
        ColumnInfo::new(ColumnType::simple(field_type), ColumnIndexType::None)
    }

    /// The panic that proves a writer was never reached.
    fn never_write(statement: &str) {
        panic!("the dry run executed a write: {statement}");
    }

    #[test]
    fn the_read_only_predicate_is_show_and_plain_select() {
        assert!(is_read_only_statement("SHOW ENGINES"));
        assert!(is_read_only_statement("show tables like 'bigworld%'"));
        assert!(is_read_only_statement("SELECT version FROM bigworldInfo"));
        assert!(is_read_only_statement("select * from tbl_avatar"));
        assert!(is_read_only_statement(" \t SELECT 1"));
    }

    #[test]
    fn locking_selects_are_writers_in_disguise() {
        assert!(!is_read_only_statement(
            "SELECT * FROM tbl_avatar FOR UPDATE"
        ));
        assert!(!is_read_only_statement(
            "select sm_name from tbl_avatar lock in share mode"
        ));
        assert!(!is_read_only_statement(
            "SELECT * FROM t WHERE x FOR UPDATE LOCK IN SHARE MODE"
        ));
    }

    #[test]
    fn every_write_shape_is_withheld() {
        assert!(!is_read_only_statement(
            "CREATE TABLE IF NOT EXISTS bigworldInfo (version INT) ENGINE=InnoDB"
        ));
        assert!(!is_read_only_statement(
            "ALTER TABLE tbl_avatar, ADD COLUMN hp INT"
        ));
        assert!(!is_read_only_statement("DROP TABLE tbl_ghost"));
        assert!(!is_read_only_statement("UPDATE tbl_avatar SET sm_hp='10'"));
        assert!(!is_read_only_statement(
            "INSERT INTO bigworldNewID (id) VALUES (3)"
        ));
        assert!(!is_read_only_statement(
            "DELETE FROM bigworldUsedIDs WHERE 1=1"
        ));
    }

    #[test]
    fn only_space_and_tab_count_as_leading_whitespace() {
        assert!(is_read_only_statement("\t \tSHOW ENGINES"));
        assert!(
            !is_read_only_statement("\nSELECT 1"),
            "a newline-led statement is not read-only, per find_first_not_of"
        );
    }

    #[test]
    fn an_empty_statement_is_never_read_only() {
        assert!(!is_read_only_statement(""));
        assert!(!is_read_only_statement("   \t\t  "));
        assert!(INVALID_STATEMENT_MSG.contains("Invalid SQL statement"));
    }

    #[test]
    fn the_wrapper_runs_reads_and_withholds_writes() {
        let mut wrapper = DryRunWrapper::new();
        let mut reads = Vec::new();

        assert!(wrapper.execute("SHOW ENGINES", |statement| reads.push(statement.to_owned())));
        assert!(
            wrapper.execute("SELECT version FROM bigworldInfo", |statement| reads
                .push(statement.to_owned()))
        );
        assert!(!wrapper.execute(
            "CREATE TABLE IF NOT EXISTS bigworldInfo (version INT) ENGINE=InnoDB",
            never_write
        ));

        assert_eq!(
            reads,
            vec![
                "SHOW ENGINES".to_owned(),
                "SELECT version FROM bigworldInfo".to_owned()
            ]
        );
        assert_eq!(
            wrapper.withheld(),
            vec!["CREATE TABLE IF NOT EXISTS bigworldInfo (version INT) ENGINE=InnoDB"]
        );
    }

    #[test]
    fn the_non_exception_query_reports_success_without_running() {
        let mut wrapper = DryRunWrapper::new();
        let mut ran = Vec::new();
        assert_eq!(
            wrapper.query("SHOW TABLES LIKE 'bigworld%'", |s| ran.push(s.to_owned())),
            1,
            "a read query ran and reports success"
        );
        assert_eq!(ran, vec!["SHOW TABLES LIKE 'bigworld%'".to_owned()]);
        assert_eq!(
            wrapper.query("DROP TABLE tbl_ghost", never_write),
            0,
            "withheld queries return 0, the reference's silent success"
        );
        assert_eq!(wrapper.withheld().len(), 1);
    }

    #[test]
    fn the_report_lines_match_the_reference_log_format() {
        let mut wrapper = DryRunWrapper::new();
        wrapper.execute("CREATE INDEX smIndex ON tbl_avatar (sm_hp)", never_write);
        assert_eq!(
            wrapper.report_lines(),
            vec!["DRYRUN:\tCREATE INDEX smIndex ON tbl_avatar (sm_hp);".to_owned()]
        );
        assert_eq!(DRY_RUN_PREFIX, "DRYRUN:");
    }

    #[test]
    fn the_whole_special_table_plan_is_withheld() {
        let report = dry_run_statements(&special_table_statements(2048));
        assert_eq!(
            report.planned_statements,
            special_table_statements(2048),
            "no bookkeeping CREATE is read-only"
        );
        assert!(report.read_statements.is_empty());
    }

    #[test]
    fn the_whole_definitions_plan_is_withheld() {
        let mut columns = BTreeMap::new();
        columns.insert("sm_id".to_owned(), plain(crate::sync_db::FieldType::Long));
        let mut observed = BTreeMap::new();
        observed.insert("tbl_ghost".to_owned(), BTreeMap::new());
        let required = vec![("tbl_new".to_owned(), columns)];
        let inspection = crate::sync_db::inspect(&required, &observed).expect("no duplicates");
        let statements = plan_statements(&inspection, true);
        assert_eq!(statements.len(), 3, "CREATE, ALTER, DROP TABLE");

        let report = dry_run_statements(&statements);
        assert_eq!(report.planned_statements, statements);
        assert!(report.read_statements.is_empty());
    }

    #[test]
    fn a_dry_run_steps_through_the_special_tables_without_writes() {
        let request = SyncPlanRequest {
            engine_support: crate::sync_db::EngineSupport::Default,
            dry_run: true,
            info_table_exists: true,
            entity_types_table_exists: true,
            ..SyncPlanRequest::default()
        };
        let steps = crate::sync_db::plan_sync(&request).expect("gates pass");
        assert!(matches!(
            steps.last(),
            Some(SyncStep::SyncEntityDefinitions)
        ));

        let report = dry_run_steps(&steps, 2048);
        assert_eq!(report.planned_statements.len(), 12);
        assert!(
            report
                .planned_statements
                .iter()
                .all(|statement| !is_read_only_statement(statement))
        );
        assert!(report.read_statements.is_empty());
    }

    #[test]
    fn a_dry_run_leaves_the_database_byte_for_byte_the_same() {
        // The writer is a ledger of what actually reached the database.
        let mut written: Vec<String> = Vec::new();
        let mut wrapper = DryRunWrapper::new();
        let statements = vec![
            "SHOW ENGINES".to_owned(),
            "CREATE TABLE IF NOT EXISTS bigworldInfo (version INT) ENGINE=InnoDB".to_owned(),
            "SELECT version FROM bigworldInfo".to_owned(),
            "ALTER TABLE tbl_avatar, ADD COLUMN sm_hp INT NOT NULL".to_owned(),
        ];
        for statement in &statements {
            let ledger = &mut written;
            wrapper.execute(statement, |read_only| {
                ledger.push(format!("READ:{read_only}"));
            });
        }
        assert_eq!(
            written,
            vec![
                "READ:SHOW ENGINES".to_owned(),
                "READ:SELECT version FROM bigworldInfo".to_owned(),
            ],
            "only the reads landed"
        );
        assert_eq!(wrapper.withheld().len(), 2);
    }

    #[test]
    fn an_unchanged_table_plan_reports_nothing() {
        let mut columns = BTreeMap::new();
        columns.insert("sm_id".to_owned(), plain(crate::sync_db::FieldType::Long));
        let mut observed = BTreeMap::new();
        observed.insert("tbl_fine".to_owned(), columns.clone());
        let required = vec![("tbl_fine".to_owned(), columns)];
        let inspection = crate::sync_db::inspect(&required, &observed).expect("no duplicates");
        assert_eq!(
            inspection
                .tables
                .iter()
                .map(|plan| plan.action.clone())
                .collect::<Vec<_>>(),
            vec![TableAction::Unchanged]
        );
        let report = dry_run_statements(&plan_statements(&inspection, true));
        assert!(report.planned_statements.is_empty());
        assert!(report.read_statements.is_empty());
    }

    #[test]
    fn the_charset_alter_is_reported_and_the_upgrade_step_carries_no_sql() {
        let request = crate::sync_db::SyncPlanRequest {
            engine_support: crate::sync_db::EngineSupport::Default,
            character_set: "utf8".to_owned(),
            collation: "utf8_bin".to_owned(),
            db_version: Some(5),
            was_password_hashed: Some(true),
            should_hash_passwords: true,
            ..SyncPlanRequest::default()
        };
        let steps = crate::sync_db::plan_sync(&request).expect("gates pass");
        let report = dry_run_steps(&steps, 2048);
        assert_eq!(
            report.planned_statements.last().map(String::as_str),
            Some("ALTER DATABASE CHARACTER SET utf8 COLLATE utf8_bin"),
            "the special tables plus the charset alter, all withheld"
        );
        assert_eq!(report.planned_statements.len(), 13);
        assert!(report.read_statements.is_empty());
    }
}
