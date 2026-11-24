//! Exporting and restoring a whole-database backup (BW-09-07-04).
//!
//! The reference's backup is not a dump at all: `snapshot_helper` takes
//! an LVM snapshot of the MySQL datadir and ships it with rsync. It
//! drives four external programs under `execl` (lvcreate, mount, chmod,
//! umount, lvremove — `snapshot_helper.cpp:195-283`) and brackets the
//! copy with `FLUSH TABLES WITH READ LOCK` / `UNLOCK TABLES`
//! (`snapshot_helper.cpp:187-218`); the transfer leg is
//! `rsync --archive --no-specials --no-devices`
//! (`transfer_db/snapshot.cpp:376-391`) with a `.complete` sentinel
//! (`snapshot.cpp:414-431`). The MySQL server is touched only through
//! the client library, never a dump tool. There is no restore program
//! either — putting the datadir files back is an operator act outside
//! the code — and no post-restore verifier: the `AUTO_INCREMENT` state
//! simply travels inside the copied files.
//!
//! The PostgreSQL port keeps the shape and swaps the mechanics. The
//! consistent image of the database is a `pg_dump` SQL dump, whose own
//! consistent-snapshot transaction stands in for the read lock; the
//! dump file is the `.complete`-style artifact. The restore, which the
//! reference leaves to the operator, is defined here: `psql` replays
//! the dump into an empty target database, stopping at the first
//! failing statement — a plain dump carries no `DROP`/`IF NOT EXISTS`,
//! so a target that is not empty fails loudly instead of
//! half-restoring. Verifying the records afterwards is likewise this
//! port's own contract: the reference's closest analogue is the
//! schema-only `bigworldEntityDefsChecksum` digest
//! (`mysql_database.cpp:962-970`, `consolidate_dbs_app.cpp:377-388`),
//! which never compares rows. The dump carries the allocation state:
//! every identity column's `setval` travels with it, so the next
//! allocatable DBID of the restored table is the source's
//! ([`crate::store::PostgresStorage::next_allocatable_id`]).
//!
//! Both legs run the server's own command-line tools the way the
//! reference drives its programs: arguments, an observed exit status,
//! and no shell.

use std::process::Command;

use db::DbError;

/// Runs a tool command and maps every failure into the storage contract:
/// a tool that is missing ("would not run") or one that exits nonzero
/// ("failed with", stderr included) is a storage fault. The child's
/// standard input is closed (`Command::output`), so a tool that waits on
/// a prompt fails instead of hanging.
fn run_tool(mut command: Command, what: &str) -> Result<(), DbError> {
    let output = command
        .output()
        .map_err(|err| DbError::Storage(format!("{what} would not run: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(DbError::Storage(format!(
        "{what} failed with {}: {}",
        output.status,
        stderr.trim()
    )))
}

/// The `pg_dump` invocation for an export: the connection string on the
/// command line (the reference passes its database credentials to the
/// helper the same way, `snapshot.cpp:252-261`), the dump written to
/// `path`. `--no-owner --no-privileges` keeps the artifact restorable by
/// any role: the records are the contract, not the grant set.
fn export_command(conninfo: &str, path: &str) -> Command {
    let mut command = Command::new("pg_dump");
    command.arg(format!("--dbname={conninfo}"));
    command.arg(format!("--file={path}"));
    command.arg("--no-owner");
    command.arg("--no-privileges");
    command
}

/// The `psql` invocation for a restore: replay the dump file into the
/// target database, aborting the replay on the first failing statement
/// (`ON_ERROR_STOP` gives psql a nonzero exit, which [`run_tool`]
/// reports).
fn restore_command(conninfo: &str, path: &str) -> Command {
    let mut command = Command::new("psql");
    command.arg(format!("--dbname={conninfo}"));
    command.arg(format!("--file={path}"));
    command.arg("--set=ON_ERROR_STOP=1");
    command
}

/// Writes a consistent SQL dump of the database at `conninfo` to `path`
/// with `pg_dump`: this backend's export leg. The dump is a plain SQL
/// file — table definitions, every row as `COPY` data, and one `setval`
/// per identity column — and it restores into an *empty* database.
pub fn export_database(conninfo: &str, path: &str) -> Result<(), DbError> {
    run_tool(
        export_command(conninfo, path),
        "pg_dump (the database export)",
    )
}

/// Restores a dump written by [`export_database`] into the empty
/// database at `conninfo` with `psql`. The target must be empty: the
/// dump's `CREATE TABLE` statements collide with any existing table and
/// the replay aborts there, leaving the target as it was.
pub fn restore_database(conninfo: &str, path: &str) -> Result<(), DbError> {
    run_tool(
        restore_command(conninfo, path),
        "psql (the database restore)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_export_runs_pg_dump_on_the_conninfo_and_the_file() {
        let command = export_command("host=/var/run/postgresql dbname=src", "/tmp/dump.sql");
        assert_eq!(command.get_program().to_string_lossy(), "pg_dump");
        assert_eq!(
            arguments(&command),
            vec![
                "--dbname=host=/var/run/postgresql dbname=src",
                "--file=/tmp/dump.sql",
                "--no-owner",
                "--no-privileges",
            ]
        );
    }

    #[test]
    fn the_restore_runs_psql_and_stops_on_the_first_error() {
        let command = restore_command("dbname=dst", "/tmp/dump.sql");
        assert_eq!(command.get_program().to_string_lossy(), "psql");
        assert_eq!(
            arguments(&command),
            vec![
                "--dbname=dbname=dst",
                "--file=/tmp/dump.sql",
                "--set=ON_ERROR_STOP=1",
            ]
        );
    }

    #[test]
    fn a_missing_tool_is_a_storage_fault() {
        let err = run_tool(Command::new("boundless-missing-tool-xyz"), "the probe").unwrap_err();
        assert!(err.to_string().contains("the probe would not run"));
    }

    #[test]
    fn a_failing_tool_reports_its_status_and_stderr() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo boom >&2; exit 3"]);
        let err = run_tool(command, "the probe").unwrap_err();
        assert!(err.to_string().contains("the probe failed with"));
        assert!(err.to_string().contains("boom"));
    }
}
