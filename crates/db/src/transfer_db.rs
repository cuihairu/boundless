//! BW-09-05-04 the transfer_db tool.
//!
//! `server/tools/transfer_db` moves database data between hosts: it
//! snapshots the primary MySQL volume through `snapshot_helper` and sends it
//! with `rsync` (`Snapshot::transferPrimary`,
//! `snapshot.cpp:469-520`), backs up a secondary SQLite database with the
//! sqlite backup API and sends the copy the same way
//! (`Snapshot::transferSecondary`, `:614-662`), and hands `consolidate` to
//! the consolidator (`TransferDB::consolidate`,
//! `transfer_db.cpp:138-176`).
//!
//! The child processes (`snapshot_helper`, `rsync`) are driven through a
//! [`ChildRunner`], so the exact argument lists and the exit codes are
//! reviewable and testable without the binaries — the acceptance the tool is
//! asked for. The completion marker file and the temporary directory follow
//! the reference's `/tmp` convention (`sendCompletionFile`
//! `:437-489`, `transferSecondary` `:614-662`); the port roots them at
//! [`Snapshot::file_root`] so tests do not touch the real `/tmp`.

use std::path::PathBuf;

/// `EXIT_SUCCESS` (`main.cpp:117`).
pub const EXIT_SUCCESS: i32 = 0;

/// `EXIT_FAILURE` (`main.cpp:31,117`).
pub const EXIT_FAILURE: i32 = 1;

/// `SNAPSHOT_HELPER` (`snapshot.cpp:26-27`): the helper under the tools'
/// `_helpers` directory.
pub const SNAPSHOT_HELPER_PATH: &str = "_helpers/snapshot_helper";

/// `RSYNC` (`snapshot.cpp:28-29`).
pub const RSYNC: &str = "/usr/bin/rsync";

/// The completion-file suffix (`sendCompletionFile`,
/// `snapshot.cpp:442-444`): `/tmp/<basename>.complete`.
pub const COMPLETION_SUFFIX: &str = ".complete";

/// The temporary-directory template (`transferSecondary`,
/// `snapshot.cpp:625-632`): `/tmp/transferdb-XXXXXX`, made unique here with
/// the process id instead of `mkdtemp`'s random characters.
pub const TEMP_DIR_PREFIX: &str = "transferdb-";

/// The outcome of one child process: the exit code and both pipes
/// (`ChildProcess::stdOut/stdErr` + `PrimarySnapshotHelper::onChildComplete`
/// `snapshot.cpp:57-66`).
#[derive(Clone, Debug)]
pub struct ChildResult {
    /// The exit status when the process exited normally.
    pub exit_code: i32,
    /// The captured stdout pipe.
    pub stdout: String,
    /// The captured stderr pipe.
    pub stderr: String,
}

impl ChildResult {
    /// A successful run with the given output.
    pub fn success(stdout: &str) -> Self {
        Self {
            exit_code: EXIT_SUCCESS,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    /// A failed run with the given output.
    pub fn failure(stderr: &str) -> Self {
        Self {
            exit_code: EXIT_FAILURE,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }

    /// `PrimarySnapshotHelper::onChildComplete`
    /// (`snapshot.cpp:57-66`): the child exited with
    /// `SnapshotHelper::SUCCESS`.
    fn succeeded(&self) -> bool {
        self.exit_code == crate::snapshot::SUCCESS
    }
}

/// The fork/exec stand-in (`ChildProcess`,
/// `server/child_process.{hpp,cpp}`).
pub trait ChildRunner {
    /// Run the program with the arguments and wait for it.
    fn run(&mut self, program: &str, args: &[String]) -> ChildResult;
}

/// The messages the tool prints beside failures (`ERROR_MSG`).
#[derive(Debug, Default)]
pub struct Snapshot {
    destination_ip: String,
    destination_path: String,
    limit_kbps: String,
    /// Where the completion file and the temporary directory live
    /// (`/tmp` in the reference).
    pub file_root: PathBuf,
    errors: Vec<String>,
    /// The basename counter keeping the temporary directories unique
    /// (`mkdtemp` stand-in).
    temp_counter: u32,
}

impl Snapshot {
    /// A snapshot aimed at the given destination, rooted at the reference's
    /// `/tmp`.
    pub fn new(destination_ip: &str, destination_path: &str, limit_kbps: &str) -> Self {
        let mut this = Self {
            destination_ip: String::new(),
            destination_path: String::new(),
            limit_kbps: String::new(),
            file_root: PathBuf::from("/tmp"),
            errors: Vec::new(),
            temp_counter: 0,
        };
        this.init(destination_ip, destination_path, limit_kbps);
        this
    }

    /// `Snapshot::init` (`snapshot.cpp:95-120`): remember the destination
    /// and the bandwidth limit.
    pub fn init(&mut self, destination_ip: &str, destination_path: &str, limit_kbps: &str) {
        self.destination_ip = destination_ip.to_string();
        self.destination_path = destination_path.to_string();
        self.limit_kbps = limit_kbps.to_string();
    }

    /// The recorded failure messages.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The `ip:path` form rsync copies to (`Snapshot::destination`,
    /// `snapshot.cpp:353-361`).
    pub fn destination(&self) -> String {
        format!("{}:{}", self.destination_ip, self.destination_path)
    }

    /// `basename` as the reference's `basename(3)` call resolves it: the
    /// text after the last `/`.
    fn basename(path: &str) -> &str {
        match path.rfind('/') {
            Some(pos) => &path[pos + 1..],
            None => path,
        }
    }

    /// C `atoi`: the leading integer, or zero.
    fn atoi(text: &str) -> i64 {
        let trimmed = text.trim_start();
        let digits: &str = &trimmed[..trimmed.chars().take_while(char::is_ascii_digit).count()];
        digits.parse().unwrap_or(0)
    }

    /// `Snapshot::isSnapshotHelperSetUidRoot` (`snapshot.cpp:126-160`): run
    /// the helper without arguments to test the setuid attribute.
    pub fn is_snapshot_helper_set_uid_root(&mut self, runner: &mut dyn ChildRunner) -> bool {
        let result = runner.run(SNAPSHOT_HELPER_PATH, &[]);
        if !result.succeeded() {
            self.errors.push(
                "Snapshot::isSnapshotHelperSetUidRoot: snapshot_helper failed to run.".to_string(),
            );
            self.errors
                .push(format!("snapshot_helper stdout:\n{}", result.stdout));
            self.errors
                .push(format!("snapshot_helper stderr:\n{}", result.stderr));
            return false;
        }
        true
    }

    /// `Snapshot::takePrimarySnapshot` (`snapshot.cpp:166-257`): the helper
    /// acquires the snapshot and its last output line carries the path.
    pub fn take_primary_snapshot(
        &mut self,
        runner: &mut dyn ChildRunner,
        db_username: &str,
        db_password: &str,
    ) -> Option<String> {
        if db_username.is_empty() || db_password.is_empty() {
            self.errors.push(
                "Snapshot::takePrimarySnapshot: dbapp username / password empty.".to_string(),
            );
            return None;
        }

        // snapshot_helper acquire-snapshot <db username> <db password>
        let result = runner.run(
            SNAPSHOT_HELPER_PATH,
            &[
                "acquire-snapshot".to_string(),
                db_username.to_string(),
                db_password.to_string(),
            ],
        );

        if !result.succeeded() {
            self.errors.push(
                "Snapshot::takePrimarySnapshot: snapshot_helper failed to acquire a snapshot."
                    .to_string(),
            );
            self.errors
                .push(format!("snapshot_helper stdout:\n{}", result.stdout));
            self.errors
                .push(format!("snapshot_helper stderr:\n{}", result.stderr));
            return None;
        }

        // The last line of the stdout stream carries the path; it starts
        // after the newline before it (`snapshot.cpp:226-253`).
        let output = &result.stdout;
        let Some(eol) = output.rfind('\n') else {
            self.errors.push(
                "Snapshot::takePrimarySnapshot: Unable to find snapshot path from snapshot_helper output."
                    .to_string(),
            );
            self.errors.push(output.clone());
            return None;
        };
        let Some(start) = output[..eol].rfind('\n') else {
            self.errors.push(
                "Snapshot::takePrimarySnapshot: Unable to find start of snapshot path from snapshot_helper output."
                    .to_string(),
            );
            return None;
        };

        Some(output[start + 1..eol].to_string())
    }

    /// `Snapshot::removePrimarySnapshot` (`snapshot.cpp:263-310`): the
    /// helper releases the snapshot.
    pub fn remove_primary_snapshot(&mut self, runner: &mut dyn ChildRunner) -> bool {
        // snapshot_helper release-snapshot
        let result = runner.run(SNAPSHOT_HELPER_PATH, &["release-snapshot".to_string()]);
        if !result.succeeded() {
            self.errors
                .push("Snapshot::removePrimarySnapshot: Failed to release snapshot".to_string());
            self.errors
                .push(format!("snapshot_helper stdout:\n{}", result.stdout));
            self.errors
                .push(format!("snapshot_helper stderr:\n{}", result.stderr));
            return false;
        }
        true
    }

    /// `Snapshot::sendSnapshot` (`snapshot.cpp:364-412`): the archive copy
    /// without specials or devices, bandwidth-limited when asked.
    pub fn send_snapshot(&mut self, runner: &mut dyn ChildRunner, src_path: &str) -> bool {
        let mut args = vec![
            "--archive".to_string(),
            "--no-specials".to_string(),
            "--no-devices".to_string(),
            src_path.to_string(),
            self.destination(),
        ];

        if Self::atoi(&self.limit_kbps) != 0 {
            args.push(format!("--bwlimit={}", self.limit_kbps));
        }

        let result = runner.run(RSYNC, &args);
        if !result.succeeded() {
            self.errors
                .push("Snapshot::sendSnapshot: rsync failed to transfer snapshot.".to_string());
            self.errors
                .push(format!("rsync stderr:\n{}", result.stderr));
            return false;
        }
        true
    }

    /// `Snapshot::sendCompletionFile` (`snapshot.cpp:418-489`): one empty
    /// marker file tells the receiving end the transfer is done. A failed
    /// rsync is reported but the marker is still removed.
    pub fn send_completion_file(&mut self, runner: &mut dyn ChildRunner, src_path: &str) -> bool {
        let complete_path = self
            .file_root
            .join(format!("{}{COMPLETION_SUFFIX}", Self::basename(src_path)));

        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode_if_unix(0o644)
            .open(&complete_path);
        if let Err(e) = created {
            self.errors.push(format!(
                "Snapshot::sendCompletionFile: Unable to create completion file '{}': {}",
                complete_path.display(),
                e
            ));
            return false;
        }

        // The bare copy: no archive flags, just the file (`:477-481`).
        let result = runner.run(
            RSYNC,
            &[complete_path.display().to_string(), self.destination()],
        );
        let rsync_ok = result.succeeded();
        if !rsync_ok {
            self.errors.push(
                "Snapshot::sendCompletionFile: rsync failed to transfer snapshot. stderr follows."
                    .to_string(),
            );
            self.errors.push(result.stderr.clone());
            // Purposefully not returning false here to allow unlinking.
        }

        if std::fs::remove_file(&complete_path).is_err() {
            self.errors.push(format!(
                "Snapshot::sendCompletionFile: Unable to unlink completion file '{}'",
                complete_path.display()
            ));
            return false;
        }

        rsync_ok
    }

    /// `Snapshot::transferSnapshotViaRsync` (`:494-501`): the data, then the
    /// completion marker.
    pub fn transfer_snapshot_via_rsync(&mut self, runner: &mut dyn ChildRunner, src: &str) -> bool {
        self.send_snapshot(runner, src) && self.send_completion_file(runner, src)
    }

    /// `Snapshot::transferPrimary` (`snapshot.cpp:507-520`): probe the
    /// helper, take the snapshot, send it, and release it — the release
    /// happens even after a failed transfer, and both results count.
    pub fn transfer_primary(
        &mut self,
        runner: &mut dyn ChildRunner,
        db_username: &str,
        db_password: &str,
    ) -> bool {
        // Test that the snapshot_helper process can be run before using it.
        if !self.is_snapshot_helper_set_uid_root(runner) {
            self.errors.push(
                "Snapshot::transferPrimary: snapshot_helper binary appears to be incorrectly installed."
                    .to_string(),
            );
            return false;
        }

        // Take the snapshot using snapshot_helper.
        let path_to_snapshot = match self.take_primary_snapshot(runner, db_username, db_password) {
            Some(path) => path,
            None => {
                self.errors.push(
                    "Snapshot::transferPrimary: Failed to take primary snapshot.".to_string(),
                );
                return false;
            }
        };

        // Once the snapshot has been successfully taken it must be sent back
        // to the machine that initiated the snapshotting procedure.
        let was_transferred = self.transfer_snapshot_via_rsync(runner, &path_to_snapshot);
        if !was_transferred {
            self.errors.push(
                "Snapshot::transferPrimary: Failed to transfer successful snapshot back to the requesting host."
                    .to_string(),
            );
        }

        // Now that the snapshot has been transferred back we remove the
        // snapshot directory using snapshot_helper.
        let was_snapshot_removed = self.remove_primary_snapshot(runner);
        if !was_snapshot_removed {
            self.errors.push(
                "Snapshot::transferPrimary: Failed to release snapshot of primary database."
                    .to_string(),
            );
        }

        was_transferred && was_snapshot_removed
    }

    /// `Snapshot::backupSqliteDatabase` (`snapshot.cpp:530-570`): both ends
    /// open, then the sqlite backup API copies the `main` database across.
    pub fn backup_sqlite_database(&mut self, src_path: &str, dest_path: &str) -> bool {
        let Ok(src) = rusqlite::Connection::open(src_path) else {
            self.errors
                .push("Snapshot::backupSqliteDatabase: Source database inaccessible".to_string());
            return false;
        };
        let Ok(mut dest) = rusqlite::Connection::open(dest_path) else {
            self.errors.push(
                "Snapshot::backupSqliteDatabase: Destination database inaccessible".to_string(),
            );
            return false;
        };

        // Both ends copy their `main` databases
        // (`sqliteSecondaryDatabaseName`, `snapshot.cpp:565-566`).
        match rusqlite::backup::Backup::new(&src, &mut dest) {
            Ok(backup) => backup
                .run_to_completion(5, std::time::Duration::from_millis(25), None)
                .is_ok(),
            Err(_) => false,
        }
    }

    /// A unique directory under [`Self::file_root`] standing in for the
    /// `mkdtemp("/tmp/transferdb-XXXXXX")` call (`snapshot.cpp:625-636`).
    fn make_temp_dir(&mut self) -> Option<PathBuf> {
        let pid = std::process::id();
        loop {
            let dir = self
                .file_root
                .join(format!("{}{}-{}", TEMP_DIR_PREFIX, pid, self.temp_counter));
            self.temp_counter += 1;
            match std::fs::create_dir(&dir) {
                Ok(()) => return Some(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
    }

    /// `Snapshot::transferSecondary` (`snapshot.cpp:614-662`): a safe backup
    /// of the sqlite database, sent with rsync, the temporary copy removed
    /// either way. Cleanup failures are reported, not fatal.
    pub fn transfer_secondary(&mut self, runner: &mut dyn ChildRunner, sqlite_path: &str) -> bool {
        let sqlite_filename = Self::basename(sqlite_path);

        let Some(temp_dir) = self.make_temp_dir() else {
            self.errors.push(
                "Snapshot::transferSecondary: Failed to create temporary directory".to_string(),
            );
            return false;
        };
        let snapshot_path = temp_dir.join(sqlite_filename);
        let snapshot_path_str = snapshot_path.display().to_string();

        // The sqlite backup of the live database
        // (http://www.sqlite.org/backup.html).
        let was_copied = self.backup_sqlite_database(sqlite_path, &snapshot_path_str);
        if !was_copied {
            self.errors.push(format!(
                "Snapshot::transferSecondary: Failed to backup the sqlite db '{sqlite_path}'."
            ));
        }

        let mut was_transferred = false;
        if was_copied {
            was_transferred = self.transfer_snapshot_via_rsync(runner, &snapshot_path_str);
            if !was_transferred {
                self.errors.push(
                    "Snapshot::transferSecondary: Failed to transfer successful secondary snapshot back to the requesting host."
                        .to_string(),
                );
            }
        }

        // Neither cleanup failure counts as a tool failure
        // (`snapshot.cpp:647-659`).
        if std::fs::remove_file(&snapshot_path).is_err() {
            self.errors.push(format!(
                "Snapshot::transferSecondary: Unable to remove secondary db snapshot '{}'",
                snapshot_path_str
            ));
        }
        if std::fs::remove_dir(&temp_dir).is_err() {
            self.errors.push(format!(
                "Snapshot::transferSecondary: Unable to remove temporary directory '{}'",
                temp_dir.display()
            ));
        }

        was_copied && was_transferred
    }
}

/// `OpenOptions::mode`, where the platform has it (the completion file is
/// created 0644, `snapshot.cpp:445-447`).
trait ModeIfUnix {
    fn mode_if_unix(&mut self, mode: u32) -> &mut Self;
}

#[cfg(unix)]
impl ModeIfUnix for std::fs::OpenOptions {
    fn mode_if_unix(&mut self, mode: u32) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(mode)
    }
}

#[cfg(not(unix))]
impl ModeIfUnix for std::fs::OpenOptions {
    fn mode_if_unix(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

/// The `TransferDB` app: the command dispatch (`main.cpp:26-118`).
pub struct TransferDb {
    /// `TransferDB::init` (`transfer_db.cpp:62-89`): the logger and watcher
    /// setup the port models as a flag.
    pub init_ok: bool,
}

impl TransferDb {
    /// Constructor.
    pub fn new(init_ok: bool) -> Self {
        Self { init_ok }
    }

    /// The `BIGWORLD_MAIN` loop (`main.cpp:26-118`). The database
    /// credentials come from the config (`TransferDB::snapshotPrimary`
    /// reads `dbAppConfig_.getMySQLUsername()/getMySQLPassword()`,
    /// `transfer_db.cpp:210-216`). `consolidate` is handed to its callback —
    /// the consolidator itself is BW-09-05-05.
    pub fn run(
        &mut self,
        args: &[&str],
        snapshot: &mut Snapshot,
        runner: &mut dyn ChildRunner,
        db_username: &str,
        db_password: &str,
        consolidate: &mut dyn FnMut(&str, &str) -> bool,
    ) -> i32 {
        if !self.init_ok {
            // ERROR_MSG( "Initialisation failed\n" )
            return EXIT_FAILURE;
        }

        let mut i = 0;
        while i < args.len() {
            if (args[i] == "--res" || args[i] == "-r") && i < args.len() - 1 {
                // The resource path argument and its value are skipped.
                i += 2;
                continue;
            }

            match args[i] {
                "consolidate" => {
                    // We expect 2 more args for consolidate db.
                    if args.len() - (i + 1) < 2 {
                        return EXIT_FAILURE;
                    }
                    let secondary_db = args[i + 1];
                    let send_to_addr = args[i + 2];
                    if !consolidate(secondary_db, send_to_addr) {
                        return EXIT_FAILURE;
                    }
                    break;
                }
                "snapshotprimary" => {
                    // We expect 3 more args for primary DB snapshotting.
                    if args.len() - (i + 1) < 3 {
                        return EXIT_FAILURE;
                    }
                    let dest_ip = args[i + 1];
                    let dest_path = args[i + 2];
                    let limit_kbps = args[i + 3];
                    snapshot.init(dest_ip, dest_path, limit_kbps);
                    if !snapshot.transfer_primary(runner, db_username, db_password) {
                        return EXIT_FAILURE;
                    }
                    break;
                }
                "snapshotsecondary" => {
                    // We expect 4 more args for secondary DB snapshotting.
                    if args.len() - (i + 1) < 4 {
                        return EXIT_FAILURE;
                    }
                    let secondary_db = args[i + 1];
                    let dest_ip = args[i + 2];
                    let dest_path = args[i + 3];
                    let limit_kbps = args[i + 4];
                    snapshot.init(dest_ip, dest_path, limit_kbps);
                    if !snapshot.transfer_secondary(runner, secondary_db) {
                        return EXIT_FAILURE;
                    }
                    break;
                }
                _ => {}
            }
            i += 1;
        }

        EXIT_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner scripted with the answers, in call order, recording every
    /// invocation.
    struct Scripted {
        answers: std::collections::VecDeque<ChildResult>,
        calls: Vec<(String, Vec<String>)>,
        default: ChildResult,
    }

    impl Scripted {
        fn new(answers: Vec<ChildResult>) -> Self {
            Self {
                answers: answers.into(),
                calls: Vec::new(),
                default: ChildResult::success("SnapshotHelper started\n"),
            }
        }
    }

    impl ChildRunner for Scripted {
        fn run(&mut self, program: &str, args: &[String]) -> ChildResult {
            self.calls.push((program.to_string(), args.to_vec()));
            self.answers
                .pop_front()
                .unwrap_or_else(|| self.default.clone())
        }
    }

    /// The snapshot config the tests share.
    fn aimed() -> Snapshot {
        Snapshot::new("10.0.0.9", "/incoming", "512")
    }

    /// A scratch directory for the completion files, unique per test.
    fn unique_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "transferdb-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        root
    }

    /// The dispatch: the three commands carry their argument lists, the
    /// count refusals name the command, and `--res` skips its value.
    #[test]
    fn the_dispatch_routes_the_three_commands() {
        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![]);
        let mut consolidated = Vec::new();

        let mut db = TransferDb::new(true);
        assert_eq!(
            db.run(
                &["consolidate", "/data/secondary.db", "10.0.0.9:5000"],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |db, addr| {
                    consolidated.push((db.to_string(), addr.to_string()));
                    true
                }
            ),
            EXIT_SUCCESS
        );
        assert_eq!(
            consolidated,
            vec![(
                "/data/secondary.db".to_string(),
                "10.0.0.9:5000".to_string()
            )]
        );

        // The snapshot commands break out of the loop on completion.
        let mut snapshot = aimed();
        snapshot.file_root = unique_root("dispatch-primary");
        let mut runner = Scripted::new(vec![
            // the setuid probe
            ChildResult::success("SnapshotHelper started\n"),
            // the acquire-snapshot run
            ChildResult::success("SnapshotHelper started\n/mnt/snap/var/lib/mysql\n"),
            // the rsync of the snapshot
            ChildResult::success(""),
            // the rsync of the completion file
            ChildResult::success(""),
            // the release-snapshot run
            ChildResult::success("SnapshotHelper started\n"),
        ]);
        let mut db = TransferDb::new(true);
        assert_eq!(
            db.run(
                &["snapshotprimary", "10.0.0.9", "/incoming", "512"],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| true
            ),
            EXIT_SUCCESS
        );

        // A failing command fails the run.
        let mut snapshot = Snapshot::new("10.0.0.9", "/incoming", "0");
        let mut runner = Scripted::new(vec![
            // the setuid probe fails: the run stops
            ChildResult::failure("setuid root failed"),
        ]);
        let mut db = TransferDb::new(true);
        assert_eq!(
            db.run(
                &["snapshotprimary", "10.0.0.9", "/incoming", "0"],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| true
            ),
            EXIT_FAILURE
        );

        // A refused consolidate fails the run.
        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![]);
        let mut db = TransferDb::new(true);
        assert_eq!(
            db.run(
                &["consolidate", "/data/secondary.db", "10.0.0.9:5000"],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| false
            ),
            EXIT_FAILURE
        );

        // Unknown words are ignored; the run still succeeds.
        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![]);
        let mut db = TransferDb::new(true);
        assert_eq!(
            db.run(
                &["--res", "/game/res", "wibble"],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| true
            ),
            EXIT_SUCCESS
        );

        // A resource flag at the very end has no value to skip; it is
        // ignored like any unknown word.
        assert_eq!(
            db.run(
                &["-r"],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| true
            ),
            EXIT_SUCCESS
        );

        // A failed init fails the run before any command.
        let mut db = TransferDb::new(false);
        assert_eq!(
            db.run(
                &[],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| true
            ),
            EXIT_FAILURE
        );
    }

    /// The argument-count refusals (`main.cpp:49-56,70-77,87-94`).
    #[test]
    fn short_argument_lists_are_refused() {
        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![]);
        let mut db = TransferDb::new(true);

        assert_eq!(
            db.run(
                &["consolidate", "/data/secondary.db"],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| true
            ),
            EXIT_FAILURE
        );
        assert_eq!(
            db.run(
                &["snapshotprimary", "10.0.0.9", "/incoming"],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| true
            ),
            EXIT_FAILURE
        );
        assert_eq!(
            db.run(
                &[
                    "snapshotsecondary",
                    "/data/secondary.db",
                    "10.0.0.9",
                    "/incoming"
                ],
                &mut snapshot,
                &mut runner,
                "bwadmin",
                "secret",
                &mut |_, _| true
            ),
            EXIT_FAILURE
        );
    }

    /// The setuid probe (`snapshot.cpp:126-160`).
    #[test]
    fn the_setuid_probe_carries_the_child_output_on_failure() {
        let mut snapshot = aimed();

        let mut runner = Scripted::new(vec![ChildResult::success("ok\n")]);
        assert!(snapshot.is_snapshot_helper_set_uid_root(&mut runner));
        assert_eq!(
            runner.calls,
            vec![(SNAPSHOT_HELPER_PATH.to_string(), Vec::<String>::new())]
        );

        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![ChildResult::failure(
            "setuid root failed: Operation not permitted\n",
        )]);
        assert!(!snapshot.is_snapshot_helper_set_uid_root(&mut runner));
        assert_eq!(
            snapshot.errors(),
            &[
                "Snapshot::isSnapshotHelperSetUidRoot: snapshot_helper failed to run.".to_string(),
                "snapshot_helper stdout:\n".to_string(),
                "snapshot_helper stderr:\nsetuid root failed: Operation not permitted\n"
                    .to_string(),
            ]
        );
    }

    /// `takePrimarySnapshot` (`snapshot.cpp:166-257`): the path is the last
    /// line of the helper's stdout, behind the credentials check.
    #[test]
    fn the_primary_snapshot_path_is_the_last_stdout_line() {
        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![ChildResult::success(
            "SnapshotHelper started\n/mnt/mysql-snap/var/lib/mysql\n",
        )]);

        let path = snapshot
            .take_primary_snapshot(&mut runner, "bwadmin", "secret")
            .expect("acquired");
        assert_eq!(path, "/mnt/mysql-snap/var/lib/mysql");
        assert_eq!(
            runner.calls[0].1,
            vec![
                "acquire-snapshot".to_string(),
                "bwadmin".to_string(),
                "secret".to_string(),
            ]
        );

        // Empty credentials are refused before any child runs.
        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![]);
        assert!(snapshot
            .take_primary_snapshot(&mut runner, "", "secret")
            .is_none());
        assert_eq!(
            snapshot.errors(),
            &["Snapshot::takePrimarySnapshot: dbapp username / password empty.".to_string()]
        );
        assert!(runner.calls.is_empty());

        // A failed acquire carries both pipes.
        let mut snapshot = aimed();
        let mut failed = ChildResult::failure("Failed to 'lvcreate' LVM snapshot.\n");
        failed.stdout = "SnapshotHelper started\n".to_string();
        let mut runner = Scripted::new(vec![failed]);
        assert!(snapshot
            .take_primary_snapshot(&mut runner, "bwadmin", "secret")
            .is_none());
        assert_eq!(
            snapshot.errors()[0],
            "Snapshot::takePrimarySnapshot: snapshot_helper failed to acquire a snapshot."
        );

        // Output without two newlines has no path to extract.
        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![ChildResult::success("no path here")]);
        assert!(snapshot
            .take_primary_snapshot(&mut runner, "bwadmin", "secret")
            .is_none());
        assert_eq!(
            snapshot.errors(),
            &[
                "Snapshot::takePrimarySnapshot: Unable to find snapshot path from snapshot_helper output.".to_string(),
                "no path here".to_string(),
            ]
        );
    }

    /// `sendSnapshot` (`snapshot.cpp:364-412`): the archive flag set, and
    /// the bandwidth limit only when it parses non-zero.
    #[test]
    fn the_rsync_copy_carries_the_archive_flags_and_bwlimit() {
        let mut snapshot = Snapshot::new("10.0.0.9", "/incoming", "512");
        let mut runner = Scripted::new(vec![ChildResult::success("")]);
        assert!(snapshot.send_snapshot(&mut runner, "/mnt/snap/mysql"));
        assert_eq!(
            runner.calls[0],
            (
                RSYNC.to_string(),
                vec![
                    "--archive".to_string(),
                    "--no-specials".to_string(),
                    "--no-devices".to_string(),
                    "/mnt/snap/mysql".to_string(),
                    "10.0.0.9:/incoming".to_string(),
                    "--bwlimit=512".to_string(),
                ]
            )
        );

        // A zero or unparsable limit adds nothing.
        for kbps in ["0", "", "fast"] {
            let mut snapshot = Snapshot::new("10.0.0.9", "/incoming", kbps);
            let mut runner = Scripted::new(vec![ChildResult::success("")]);
            assert!(snapshot.send_snapshot(&mut runner, "/mnt/snap/mysql"));
            assert_eq!(runner.calls[0].1.len(), 5, "no bwlimit for {kbps:?}");
        }

        // A failing rsync reports the stderr pipe.
        let mut snapshot = Snapshot::new("10.0.0.9", "/incoming", "512");
        let mut runner = Scripted::new(vec![ChildResult::failure("rsync: connection refused")]);
        assert!(!snapshot.send_snapshot(&mut runner, "/mnt/snap/mysql"));
        assert_eq!(
            snapshot.errors(),
            &[
                "Snapshot::sendSnapshot: rsync failed to transfer snapshot.".to_string(),
                "rsync stderr:\nrsync: connection refused".to_string(),
            ]
        );
    }

    /// `sendCompletionFile` (`snapshot.cpp:418-489`): the bare copy of the
    /// marker, removed either way, and a failed rsync does not stop the
    /// removal.
    #[test]
    fn the_completion_file_is_sent_and_unlinked() {
        let root = std::env::temp_dir().join(format!("transferdb-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");

        let mut snapshot = Snapshot::new("10.0.0.9", "/incoming", "512");
        snapshot.file_root = root.clone();
        let mut runner = Scripted::new(vec![ChildResult::success("")]);

        assert!(snapshot.send_completion_file(&mut runner, "/mnt/snap/var/lib/mysql"));
        assert_eq!(
            runner.calls[0],
            (
                RSYNC.to_string(),
                vec![
                    root.join("mysql.complete").display().to_string(),
                    "10.0.0.9:/incoming".to_string(),
                ]
            )
        );
        // The marker is gone again.
        assert!(!root.join("mysql.complete").exists());
        std::fs::remove_dir_all(&root).ok();

        // A failed rsync is reported but the marker is still unlinked, and
        // the call reports the rsync failure.
        let root = std::env::temp_dir().join(format!("transferdb-test2-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let mut snapshot = Snapshot::new("10.0.0.9", "/incoming", "512");
        snapshot.file_root = root.clone();
        let mut runner = Scripted::new(vec![ChildResult::failure("rsync: broke")]);
        assert!(!snapshot.send_completion_file(&mut runner, "/mnt/snap/mysql"));
        assert!(!root.join("mysql.complete").exists());
        assert_eq!(
            snapshot.errors()[0],
            "Snapshot::sendCompletionFile: rsync failed to transfer snapshot. stderr follows."
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// `transferPrimary` (`snapshot.cpp:507-520`): the four children run in
    /// order — probe, acquire, rsync, completion, release — and the release
    /// happens even after a failed transfer.
    #[test]
    fn the_primary_transfer_runs_all_children_in_order() {
        let mut snapshot = aimed();
        snapshot.file_root = unique_root("primary");
        let mut runner = Scripted::new(vec![
            ChildResult::success("ok\n"),
            ChildResult::success("SnapshotHelper started\n/mnt/snap/mysql\n"),
            ChildResult::success(""), // rsync of the snapshot
            ChildResult::success(""), // rsync of the completion file
            ChildResult::success(""), // release
        ]);

        assert!(snapshot.transfer_primary(&mut runner, "bwadmin", "secret"));
        let programs: Vec<&str> = runner.calls.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            programs,
            vec![
                SNAPSHOT_HELPER_PATH,
                SNAPSHOT_HELPER_PATH,
                RSYNC,
                RSYNC,
                SNAPSHOT_HELPER_PATH,
            ]
        );

        // The rsync of the snapshot and the completion file go to the
        // snapshot path from the helper output.
        assert_eq!(runner.calls[2].1[3], "/mnt/snap/mysql");
        assert!(runner.calls[3].1[0].ends_with("mysql.complete"));

        // A failed transfer still releases the snapshot, and both results
        // count.
        let mut snapshot = aimed();
        let mut runner = Scripted::new(vec![
            ChildResult::success("ok\n"),
            ChildResult::success("SnapshotHelper started\n/mnt/snap/mysql\n"),
            ChildResult::failure("rsync: broke"), // the snapshot copy fails
        ]);
        assert!(!snapshot.transfer_primary(&mut runner, "bwadmin", "secret"));
        assert_eq!(runner.calls.len(), 4, "the release still ran");
        assert_eq!(runner.calls[3].0, SNAPSHOT_HELPER_PATH);
        assert_eq!(
            snapshot.errors(),
            &[
                "Snapshot::sendSnapshot: rsync failed to transfer snapshot.".to_string(),
                "rsync stderr:\nrsync: broke".to_string(),
                "Snapshot::transferPrimary: Failed to transfer successful snapshot back to the requesting host.".to_string(),
            ]
        );
    }

    /// `backupSqliteDatabase` (`snapshot.cpp:530-570`): the backup carries
    /// the source tables across.
    #[test]
    fn the_sqlite_backup_copies_the_data() {
        let root = std::env::temp_dir().join(format!("transferdb-bak-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let src = root.join("secondary.db");
        let dest = root.join("snapshot.db");

        {
            let conn = rusqlite::Connection::open(&src).expect("src");
            conn.execute_batch(
                "CREATE TABLE tbl_Account (id INTEGER PRIMARY KEY, name TEXT);
                 INSERT INTO tbl_Account VALUES (1, 'alice'), (2, 'bob');",
            )
            .expect("seed");
        }

        let mut snapshot = aimed();
        assert!(snapshot.backup_sqlite_database(
            src.display().to_string().as_str(),
            dest.display().to_string().as_str()
        ));

        // The destination carries the source rows: the target data matches.
        let conn = rusqlite::Connection::open(&dest).expect("dest");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tbl_Account", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 2);
        let name: String = conn
            .query_row("SELECT name FROM tbl_Account WHERE id = 2", [], |r| {
                r.get(0)
            })
            .expect("row");
        assert_eq!(name, "bob");

        std::fs::remove_dir_all(&root).ok();
    }

    /// `transferSecondary` (`snapshot.cpp:614-662`): the backup lands in a
    /// temporary directory, is sent with the completion marker, and the
    /// temporary directory does not survive the call.
    #[test]
    fn the_secondary_transfer_backs_up_sends_and_cleans_up() {
        let root = std::env::temp_dir().join(format!("transferdb-sec-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let src = root.join("secondary.db");
        {
            let conn = rusqlite::Connection::open(&src).expect("src");
            conn.execute_batch("CREATE TABLE tbl_Avatar (id INTEGER PRIMARY KEY); INSERT INTO tbl_Avatar VALUES (7);")
                .expect("seed");
        }

        let mut snapshot = Snapshot::new("10.0.0.9", "/incoming", "512");
        snapshot.file_root = root.clone();
        let mut runner = Scripted::new(vec![ChildResult::success(""), ChildResult::success("")]);

        assert!(snapshot.transfer_secondary(&mut runner, src.display().to_string().as_str()));

        // The rsync sent the backed-up file and the completion marker.
        assert_eq!(runner.calls[0].0, RSYNC);
        assert!(runner.calls[0].1[3].ends_with("secondary.db"));
        assert!(runner.calls[1].1[0].ends_with("secondary.db.complete"));

        // No temporary directory is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .expect("root")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_DIR_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // The source database is untouched by the call.
        let conn = rusqlite::Connection::open(&src).expect("src");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tbl_Avatar", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1);

        std::fs::remove_dir_all(&root).ok();
    }
}
