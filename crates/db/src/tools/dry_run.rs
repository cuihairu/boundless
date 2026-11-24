//! The BW-09-05-02 dry-run connection.
//!
//! The reference `sync_db --dry-run` swaps the MySQL wrapper for
//! `MySqlDryRunWrapper` (`mysql_dry_run_wrapper.cpp`), which lets read-only
//! statements through and prints every other statement instead of running it.
//! `MySqlDryRunConnection` (`mysql_dry_run_connection.cpp`) is the
//! connection-factory face: its `createMysqlWrapper` returns a
//! `MySqlDryRunWrapper`, which is exactly [`DryRunConnection::new`] wrapping
//! the connection the dry-run tool would otherwise get.
//!
//! `MySqlDryRunWrapper` derives from `MySql` and overrides the four
//! `execute`/`query` overloads only, inheriting everything else; this port
//! does the same against the [`SyncDatabase`] seam, so a
//! [`DryRunConnection`] can be handed to [`super::sync_db::Synchronise`] and
//! the statement log the inner connection records is the proof the acceptance
//! clauses ask for ("the plan is reported and the database is unchanged").
//!
//! Honest seam note: `table_names` and `describe_table` are queries the seam
//! models as trait methods rather than statements, so they bypass the filter.
//! In the reference they run through the wrapper's `execute`/`query` and a
//! real dry-run withholds them too. The filter face ported here is the
//! statement face — the four overloads the reference overrides.

use super::sync_db::{DescribedTable, EngineSupportState, SyncDatabase};

/// The characters `isReadOnlyStatement` skips when locating the first token.
pub const LEADING_WHITESPACE: &str = " \t";

/// `DRYRUN:\t%s;\n` (the INFO_MSG the wrapper prints for every withheld
/// statement).
pub fn dry_run_line(statement: &str) -> String {
    format!("DRYRUN:\t{statement};\n")
}

/// `MySqlDryRunWrapper::isReadOnlyStatement`
/// (`mysql_dry_run_wrapper.cpp:10-42`).
///
/// The statement is upper-cased from its first non-whitespace character, so
/// the checks are case-insensitive; a whitespace-only statement is an error
/// and is not read-only. The error text is appended to `log` verbatim, as the
/// reference logs it. Returns whether the statement only reads.
pub fn is_read_only_statement(statement: &str, log: &mut Vec<String>) -> bool {
    let pos = statement
        .find(|c: char| !LEADING_WHITESPACE.contains(c))
        .unwrap_or(usize::MAX);
    if pos == usize::MAX {
        log.push(format!(
            "MySqlDryRunWrapper::isReadOnlyStatement: Invalid SQL statement: \
             '{statement}'\n"
        ));
        return false;
    }
    let upper: String = statement[pos..]
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .collect();
    // `str.find("SHOW") == 0` in the reference: the token must start the
    // trimmed statement.
    if upper.starts_with("SHOW") {
        return true;
    }
    if upper.starts_with("SELECT") {
        // "MySql supports 'SELECT ... FOR UPDATE | LOCK IN SHARE MODE', this
        // kind of statements should not be considered as read only."
        return !upper.contains("FOR UPDATE") && !upper.contains("LOCK IN SHARE MODE");
    }
    false
}

/// The dry-run connection: the statement face of the reference's
/// `MySqlDryRunWrapper` + `MySqlDryRunConnection` pair. Every statement that
/// is not a read-only `SHOW`/`SELECT` is withheld into `report` instead of
/// reaching `inner`; read-only statements and every non-statement operation
/// pass straight through.
pub struct DryRunConnection<W: SyncDatabase> {
    inner: W,
    /// The withheld statements, in statement order.
    pub report: Vec<String>,
}

impl<W: SyncDatabase> SyncDatabase for DryRunConnection<W> {
    fn execute(&mut self, statement: &str) -> Result<(), String> {
        let is_read_only = is_read_only_statement(statement, &mut self.report);
        if !is_read_only {
            self.report.push(dry_run_line(statement));
            return Ok(());
        }
        self.inner.execute(statement)
    }

    fn commit(&mut self) {
        self.inner.commit()
    }

    fn engine_support_state(&mut self) -> EngineSupportState {
        self.inner.engine_support_state()
    }

    fn check_table_engines(&mut self) -> bool {
        self.inner.check_table_engines()
    }

    fn table_names(&mut self, pattern: &str) -> Vec<String> {
        self.inner.table_names(pattern)
    }

    fn describe_table(&mut self, table: &str) -> DescribedTable {
        self.inner.describe_table(table)
    }

    fn query_version(&mut self) -> Option<u32> {
        self.inner.query_version()
    }

    fn query_is_password_hashed(&mut self) -> Option<bool> {
        self.inner.query_is_password_hashed()
    }

    fn count_secondary_databases(&mut self) -> u32 {
        self.inner.count_secondary_databases()
    }

    fn upgrade_database(&mut self, from_version: u32, log: &mut Vec<String>) -> Result<(), String> {
        self.inner.upgrade_database(from_version, log)
    }
}

impl<W: SyncDatabase> DryRunConnection<W> {
    /// `MySqlDryRunConnection::createMysqlWrapper`: the dry-run connection is
    /// just a real connection behind the statement filter.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            report: Vec::new(),
        }
    }

    /// The connection the wrapper wraps, back out.
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// The statement `execute` face, shared by the reference's three `execute`
    /// overloads: withhold a non-read-only statement or hand it over.
    pub fn execute_statement(&mut self, statement: &str) -> Result<(), String> {
        self.execute(statement)
    }

    /// `MySqlDryRunWrapper::query` (`mysql_dry_run_wrapper.cpp:111-120`), the
    /// non-exception variant of `execute`: a withheld statement reports and
    /// returns the reference's `0`, a read-only one is handed over.
    pub fn query_statement(&mut self, statement: &str) -> Result<i32, String> {
        let is_read_only = is_read_only_statement(statement, &mut self.report);
        if !is_read_only {
            self.report.push(dry_run_line(statement));
            return Ok(0);
        }
        self.execute(statement)?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests;
