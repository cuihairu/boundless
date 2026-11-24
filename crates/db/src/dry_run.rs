//! BW-09-05-02 the sync_db dry-run connection.
//!
//! `server/tools/sync_db/mysql_dry_run_wrapper.{hpp,cpp}` sits between the
//! tool and the server: every statement that is not a read-only query is
//! printed as `DRYRUN:\t<statement>;` and dropped, and only `SHOW` and
//! `SELECT` queries reach the database — so a run reports the plan and leaves
//! the database untouched, which is the acceptance the tool is asked for.
//! `MySqlDryRunConnection` swaps the wrapper in
//! (`mysql_dry_run_connection.cpp:17-19`) and
//! `MySqlSynchronise::createMysqlConnection` picks it when the dry-run flag is
//! set (`mysql_synchronise.cpp:70-82`).
//!
//! The dry-run flag also changes two reads of the walk
//! (`mysql_synchronise.cpp:155-163,209-213`): the database version is only
//! queried when it would have been created anyway, and the synchronisation
//! stops after the special tables when the entity-type table is still missing.

/// `isReadOnlyStatement`
/// (`mysql_dry_run_wrapper.cpp:15-50`): trim leading blanks, require a `SHOW`
/// or `SELECT` head, and refuse the locking select forms.
///
/// An all-blank statement is not read only — the reference logs an error for
/// it (`mysql_dry_run_wrapper.cpp:22-30`).
pub fn is_read_only_statement(statement: &str) -> bool {
    let pos = statement.find(|c: char| !matches!(c, ' ' | '\t'));

    let Some(pos) = pos else {
        return false;
    };

    let trimmed = &statement[pos..];
    let upper: String = trimmed.chars().map(|c| c.to_ascii_uppercase()).collect();

    if upper.starts_with("SHOW") {
        // `SHOW` never takes a locking clause.
        return true;
    }

    if upper.starts_with("SELECT") {
        // MySql supports "SELECT ... FOR UPDATE | LOCK IN SHARE MODE", and
        // those are not read only
        // (`mysql_dry_run_wrapper.cpp:39-45`).
        return !upper.contains("FOR UPDATE") && !upper.contains("LOCK IN SHARE MODE");
    }

    false
}

/// The statements the dry-run run would have run, in order
/// (`DRYRUN:\t%s;`, `mysql_dry_run_wrapper.cpp:57-73`).
#[derive(Debug, Default)]
pub struct Plan(Vec<String>);

impl Plan {
    /// The reported statements, each terminated with `;` like the report.
    pub fn statements(&self) -> &[String] {
        &self.0
    }

    /// The report lines as printed: `DRYRUN:\t<statement>;`.
    pub fn lines(&self) -> Vec<String> {
        self.0.iter().map(|s| format!("DRYRUN:\t{s};")).collect()
    }
}

/// A dry-run database image: everything that reaches the server is recorded,
/// so a before/after comparison can show the database is untouched.
#[derive(Debug, Default)]
pub struct RecordingBackend {
    pub applied: Vec<String>,
}

impl RecordingBackend {
    fn execute(&mut self, statement: &str) {
        self.applied.push(statement.to_string());
    }
}

/// `MySqlDryRunWrapper` (`mysql_dry_run_wrapper.cpp`): writes are reported
/// and dropped, read-only queries go through to the server.
#[derive(Default)]
pub struct DryRunConnection {
    plan: Vec<String>,
    backend: RecordingBackend,
}

impl DryRunConnection {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `execute` (`mysql_dry_run_wrapper.cpp:57-73`): report and drop a
    /// non-read-only statement, otherwise run it.
    pub fn execute(&mut self, statement: &str) {
        if !is_read_only_statement(statement) {
            self.plan.push(statement.to_string());
            return;
        }
        self.backend.execute(statement);
    }

    /// `query`, the non-throwing form
    /// (`mysql_dry_run_wrapper.cpp:129-138`): the same split, reporting 0 for
    /// the statements it never runs.
    pub fn query(&mut self, statement: &str) -> u32 {
        if !is_read_only_statement(statement) {
            self.plan.push(statement.to_string());
            return 0;
        }
        self.backend.execute(statement);
        1
    }

    /// The statements this run would have executed.
    pub fn plan(&self) -> Plan {
        Plan(self.plan.clone())
    }

    /// What actually reached the database image.
    pub fn applied(&self) -> &[String] {
        &self.backend.applied
    }
}

/// The two places the dry-run flag changes the walk
/// (`mysql_synchronise.cpp:155-163,209-213`).
#[derive(Clone, Copy, Debug)]
pub struct DryRunPolicy {
    is_dry_run: bool,
}

impl DryRunPolicy {
    /// Constructor.
    pub fn new(is_dry_run: bool) -> Self {
        Self { is_dry_run }
    }

    /// Whether the database version and password state are read at all: in a
    /// dry run the info table does not exist yet, so the read is skipped
    /// (`mysql_synchronise.cpp:155-163`).
    pub fn should_read_version(&self, info_table_exists: bool) -> bool {
        !self.is_dry_run || info_table_exists
    }

    /// Whether the walk stops after the special tables: a dry run against a
    /// database without the entity-type table has nothing to synchronise yet
    /// (`mysql_synchronise.cpp:209-213`).
    pub fn should_stop_early(&self, entity_types_table_exists: bool) -> bool {
        self.is_dry_run && !entity_types_table_exists
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only `SHOW` and plain `SELECT` are read only
    /// (`mysql_dry_run_wrapper.cpp:15-50`).
    #[test]
    fn only_show_and_plain_select_are_read_only() {
        assert!(is_read_only_statement("SHOW TABLES LIKE 'tbl_%'"));
        assert!(is_read_only_statement(
            "SELECT sm_dbID, sm_typeID FROM tbl_Account WHERE id = 42"
        ));
        assert!(is_read_only_statement("   \t SELECT 1"));

        // A locking select is not read only.
        assert!(!is_read_only_statement(
            "SELECT id FROM tbl_Account FOR UPDATE"
        ));
        assert!(!is_read_only_statement(
            "SELECT id FROM tbl_Account LOCK IN SHARE MODE"
        ));
        assert!(!is_read_only_statement(
            "ALTER TABLE tbl_Account DROP COLUMN gone"
        ));
        assert!(!is_read_only_statement("DROP TABLE tbl_Obsolete"));

        // Blanks alone are not a statement at all.
        assert!(!is_read_only_statement("  \t "));
        assert!(!is_read_only_statement(""));
    }

    /// The reported statements carry the report prefix and a terminator
    /// (`mysql_dry_run_wrapper.cpp:66-68`).
    #[test]
    fn writes_are_reported_and_dropped() {
        let mut connection = DryRunConnection::new();

        connection.execute(
            "CREATE TABLE IF NOT EXISTS tbl_Account (id BIGINT AUTO_INCREMENT, \
             PRIMARY KEY idKey (id)) ENGINE=InnoDB",
        );
        connection.execute("ALTER TABLE tbl_Account ADD COLUMN hp INT");
        connection.execute("UPDATE tbl_Account SET data='0'");

        assert_eq!(
            connection.plan().lines(),
            vec![
                "DRYRUN:\tCREATE TABLE IF NOT EXISTS tbl_Account (id BIGINT \
                 AUTO_INCREMENT, PRIMARY KEY idKey (id)) ENGINE=InnoDB;"
                    .to_string(),
                "DRYRUN:\tALTER TABLE tbl_Account ADD COLUMN hp INT;".to_string(),
                "DRYRUN:\tUPDATE tbl_Account SET data='0';".to_string(),
            ]
        );
        // Nothing of it reached the database.
        assert!(connection.applied().is_empty());
    }

    /// The acceptance: a dry run reports the plan and the database image is
    /// unchanged before and after.
    #[test]
    fn a_dry_run_reports_the_plan_without_touching_the_database() {
        let mut connection = DryRunConnection::new();

        // The walk's read probes pass through: they are read only.
        let select = "SELECT COUNT(*) FROM bigworldInfo";
        let show = "SHOW TABLES LIKE 'bigworld%'";
        connection.execute(select);
        connection.execute(show);
        let before: Vec<String> = connection.applied().to_vec();

        // The walk's writes are all reported, none applied.
        connection.execute("DROP TABLE tbl_Obsolete");
        connection.query("CREATE INDEX hpIndex ON tbl_Account (hp)");

        let after: Vec<String> = connection.applied().to_vec();

        // The database is the same before and after the run: only the read
        // probes reached it.
        assert_eq!(before, after);
        assert_eq!(after, vec![select.to_string(), show.to_string()]);
        // The plan reports every statement it did not run.
        assert_eq!(
            connection.plan().statements(),
            &[
                "DROP TABLE tbl_Obsolete".to_string(),
                "CREATE INDEX hpIndex ON tbl_Account (hp)".to_string()
            ]
        );
    }

    /// `query` reports 0 for the statements it never runs
    /// (`mysql_dry_run_wrapper.cpp:129-138`).
    #[test]
    fn query_reports_zero_for_a_write() {
        let mut connection = DryRunConnection::new();
        assert_eq!(connection.query("DROP TABLE tbl_Obsolete"), 0);
        assert_eq!(connection.query("SELECT 1"), 1);
        assert_eq!(connection.plan().statements().len(), 1);
    }

    /// The flag's two effects on the walk
    /// (`mysql_synchronise.cpp:155-163,209-213`).
    #[test]
    fn the_flag_guards_the_reads() {
        let dry = DryRunPolicy::new(true);
        let live = DryRunPolicy::new(false);

        // A dry run against an untouched database skips the version read.
        assert!(!dry.should_read_version(false));
        assert!(dry.should_read_version(true));
        // A live run always reads it.
        assert!(live.should_read_version(false));

        // A dry run stops early while the entity-type table is missing, and
        // goes on once it exists.
        assert!(dry.should_stop_early(false));
        assert!(!dry.should_stop_early(true));
        assert!(!live.should_stop_early(false));
    }
}
