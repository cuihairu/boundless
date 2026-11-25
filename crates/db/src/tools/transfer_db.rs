//! The BW-09-05-04 transfer_db tool (`server/tools/transfer_db/`).
//!
//! The reference binary is the snapshot/consolidate driver the DBApp hosts
//! run: `consolidate` hands a secondary sqlite file to the consolidate
//! machinery (BW-09-05-05's face), `snapshotprimary` drives `snapshot_helper`
//! plus rsync to ship a primary LVM snapshot back to the requesting host, and
//! `snapshotsecondary` safely backs up a secondary sqlite database and rsyncs
//! it back. The privileged faces stay behind seams: process startup/monitor
//! ([`LoggerInfra`]), child processes ([`ChildRunner`]), filesystem and
//! sqlite ([`SnapshotFiles`]), the MySQL config ([`DbConfigFace`]) and the
//! consolidate delegate ([`ConsolidateFace`]); the recorded calls and message
//! log are the reviewable artifact ("target data and exit codes match").
//!
//! Honest modelling notes:
//! - the reference's `execl`/`ChildProcess` machinery forks; the hooks that
//!   run in the forked child (`onChildAboutToExec` closing the parent's fds)
//!   run inline before each dispatch here, where a double records them;
//! - `Endpoint::convertAddress` first tries `inet_aton`, then `gethostbyname`
//!   (DNS). The port keeps the numbers-and-dots leg; hostname lookup stays
//!   unported (no resolver in tests);
//! - `Snapshot::init` returns true unconditionally on POSIX (its failure
//!   branch only exists in the Windows stub), so `snapshotPrimary`'s
//!   "Failed to initialise the snapshotter" path is dead reference shape,
//!   kept verbatim for fidelity.

use crate::tools::snapshot_helper::SUCCESS as SNAPSHOT_HELPER_SUCCESS;

/// `EXIT_SUCCESS` (`main.cpp` returns it at the end of the arg loop).
pub const EXIT_SUCCESS: i32 = 0;
/// `EXIT_FAILURE` for init/arity/command failures.
pub const EXIT_FAILURE: i32 = 1;

/// `#define APP_NAME "TransferDB"`.
pub const APP_NAME: &str = "TransferDB";
/// `SNAPSHOT_HELPER "snapshot_helper"`, run under the `_helpers/` prefix.
pub const SNAPSHOT_HELPER: &str = "snapshot_helper";
/// `RSYNC "/usr/bin/rsync"`.
pub const RSYNC: &str = "/usr/bin/rsync";
/// The `mkdtemp` template (`transferSecondary`); its trailing six X's are the
/// wildcard the call replaces.
pub const TEMP_DIR_TEMPLATE: &str = "/tmp/transferdb-XXXXXX";

/// `Mercury::Address`: `ip` in network byte order, `port` as `ntohs(atoi())`
/// stored it (byte-swapped on little-endian — Mercury keeps the port in
/// network order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    /// `addr.ip` — `Endpoint::convertAddress` fills it.
    pub ip: u32,
    /// `addr.port` — `ntohs( atoi( portStr ) )`.
    pub port: u16,
}

/// The process/logging infrastructure `TransferDB::initLogger` wires up.
pub trait LoggerInfra {
    /// `BWConfig::get( "loggerID", "" )`.
    fn logger_id(&mut self) -> String;

    /// `Mercury::MachineDaemon::queryForInternalInterface`; `Ok` carries the
    /// `inet_ntoa`'d address text, `Err` when the query fails.
    fn query_internal_interface(&mut self) -> Result<String, String>;

    /// `WatcherNub::init( ip, 0 )`.
    fn watcher_init(&mut self, ip: &str, port: u16) -> bool;

    /// `LoggerMessageForwarder( appName, socket, dispatcher, loggerID,
    /// enabled=true, spamFilterThreshold=0 )`.
    fn forwarder_start(&mut self, app_name: &str, logger_id: &str);

    /// `DebugFilter::shouldWriteToConsole`.
    fn set_write_to_console(&mut self, enabled: bool);

    /// `DebugFilter::instance().filterThreshold( MESSAGE_PRIORITY_INFO )`.
    fn set_filter_threshold_info(&mut self);
}

/// One finished child process, as `ChildProcess` reports it.
pub struct ChildRun {
    /// `WIFEXITED( status )`.
    pub exited: bool,
    /// `WEXITSTATUS( status )`.
    pub exit_code: i32,
    /// `child.stdOut()` (piped only when the caller asked for it).
    pub stdout: String,
    /// `child.stdErr()`.
    pub stderr: String,
}

impl ChildRun {
    /// A run that exited with `code` and empty pipes.
    pub fn exited_with(code: i32) -> Self {
        ChildRun {
            exited: true,
            exit_code: code,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// `WIFEXITED( status ) && (WEXITSTATUS( status ) == expected)`.
    pub fn was_successful(&self, expected: i32) -> bool {
        self.exited && self.exit_code == expected
    }
}

/// The fork/exec/pump face the snapshot machinery drives. The hooks that the
/// reference runs in the forked child ([`ChildRunner::about_to_exec`]) fire
/// before each dispatch here.
pub trait ChildRunner {
    /// `IChildProcess::onChildAboutToExec` — closes the fds the parent opened,
    /// in the child.
    fn about_to_exec(&mut self);

    /// `ChildProcess::startProcessWithPipe` plus the dispatcher pump until
    /// `isFinished`. `pipe_stdout`/`pipe_stderr` are the two literal flags.
    fn start_with_pipe(
        &mut self,
        path: &str,
        argv: &[String],
        pipe_stdout: bool,
        pipe_stderr: bool,
    ) -> ChildRun;
}

/// The legs of `Snapshot::backupSqliteDatabase`, in reference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteBackupError {
    /// `pSrcDB->open` failed.
    SourceInaccessible,
    /// `pDestDB->open` failed.
    DestinationInaccessible,
    /// `pDestDB->backup( pSrcDB, "main" )` returned false.
    BackupFailed,
}

/// The filesystem/sqlite face of the snapshot machinery.
pub trait SnapshotFiles {
    /// `open( O_WRONLY | O_CREAT, 0644 )` + `close`; `Err` is
    /// `strerror(errno)` when open returned -1.
    fn create_file(&mut self, path: &str) -> Result<(), String>;

    /// `unlink`; `Err` is `strerror(errno)`.
    fn unlink(&mut self, path: &str) -> Result<(), String>;

    /// `rmdir`; `Err` is `strerror(errno)`.
    fn rmdir(&mut self, path: &str) -> Result<(), String>;

    /// `mkdtemp( template )` — the trailing six X's are replaced; `Err` is
    /// `strerror(errno)` when it returns NULL.
    fn make_temp_dir(&mut self, template: &str) -> Result<String, String>;

    /// `Snapshot::backupSqliteDatabase`: open source, open destination, then
    /// the sqlite backup into `"main"`.
    fn backup_sqlite(&mut self, src: &str, dest: &str) -> Result<(), SqliteBackupError>;
}

/// The `DBConfig::get().mysql` credentials `takePrimarySnapshot` reads.
pub trait DbConfigFace {
    /// `config.mysql.username()`.
    fn mysql_username(&self) -> String;

    /// `config.mysql.password()`.
    fn mysql_password(&self) -> String;
}

/// The `Consolidate::transferTo` delegate — the real machinery lands with
/// BW-09-05-05.
pub trait ConsolidateFace {
    /// `Consolidate( secondaryDB )` + `transferTo( addr )`. The address
    /// carries the Mercury quirk: `port` is what `ntohs(atoi())` stored.
    fn transfer_to(&mut self, secondary_db: &str, address: Address) -> bool;
}

/// The seams `main_face` and the commands drive.
pub struct Faces<'a> {
    /// Process/logging infrastructure.
    pub logger: &'a mut dyn LoggerInfra,
    /// Child processes.
    pub children: &'a mut dyn ChildRunner,
    /// Filesystem and sqlite.
    pub files: &'a mut dyn SnapshotFiles,
    /// MySQL credentials.
    pub db_config: &'a mut dyn DbConfigFace,
    /// The consolidate delegate.
    pub consolidator: &'a mut dyn ConsolidateFace,
}

/// C `atoi`: optional whitespace and sign, then digits; 0 on no digits. The
/// reference reads the port and the kbps limit through it (with its own
/// `TODO: strtoul()` on the port leg).
pub fn atoi_c(text: &str) -> i32 {
    let text = text.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut value: i64 = 0;
    for c in digits.chars().take_while(char::is_ascii_digit) {
        value = (value * 10 + (c as u8 - b'0') as i64).min(i32::MAX as i64);
    }
    if negative {
        -value as i32
    } else {
        value as i32
    }
}

/// The numbers-and-dots leg of `Endpoint::convertAddress`: four decimal
/// octets packed in network byte order, like `inet_addr("1.2.3.4")` =
/// 0x04030201. (The reference also accepts hex/octal forms and hostnames via
/// `gethostbyname`; both stay unported — see the module notes.)
pub fn convert_address(text: &str) -> Option<u32> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (slot, part) in octets.iter_mut().zip(parts) {
        if part.is_empty() || !part.chars().all(|c: char| c.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse::<u8>().ok()?;
    }
    Some(
        u32::from(octets[3]) << 24
            | u32::from(octets[2]) << 16
            | u32::from(octets[1]) << 8
            | u32::from(octets[0]),
    )
}

/// libgen `basename`: trailing slashes stripped, then the component after the
/// last `/`. The root itself stays `"/"`.
pub fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && path.starts_with('/') {
        return "/";
    }
    match trimmed.rfind('/') {
        Some(slash) => &trimmed[slash + 1..],
        None => trimmed,
    }
}

/// `TransferDB::initLogger` (`transfer_db.cpp:91-131`).
pub fn init_logger(
    infra: &mut dyn LoggerInfra,
    app_name: &str,
    is_verbose: bool,
    log: &mut Vec<String>,
) -> bool {
    let logger_id = infra.logger_id();

    // Get the internal IP
    let internal_ip = match infra.query_internal_interface() {
        Ok(ip) => ip,
        Err(_) => {
            log.push(
                "TransferDB::initLogger: failed to query for internal interface\n".to_string(),
            );
            return false;
        }
    };

    // Set up the watcher nub and message forwarder manually
    if !infra.watcher_init(&internal_ip, 0) {
        // The reference resets the nub and returns without a message.
        return false;
    }

    infra.forwarder_start(app_name, &logger_id);

    infra.set_write_to_console(true);
    if !is_verbose {
        infra.set_filter_threshold_info();
    } else {
        infra.set_write_to_console(true);
    }

    true
}

/// `TransferDB` (`transfer_db.hpp`): the singleton whose init wires the
/// logger and whose three command methods main dispatches to. The seams come
/// in per call — the reference reaches them through its singleton world.
pub struct TransferDB<'a> {
    /// The message log (the port's ERROR_MSG/NOTICE_MSG/INFO_MSG sink).
    pub log: &'a mut Vec<String>,
}

impl<'a> TransferDB<'a> {
    /// `TransferDB::init` (`transfer_db.cpp:62-72`).
    pub fn init(&mut self, faces: &mut Faces, is_verbose: bool) -> bool {
        if !init_logger(faces.logger, APP_NAME, is_verbose, self.log) {
            self.log
                .push("TransferDB::init: Failed to initialise logger\n".to_string());
            return false;
        }
        true
    }

    /// `TransferDB::consolidate` (`transfer_db.cpp:138-171`): split
    /// `<ip:port>`, convert, hand to [`ConsolidateFace`].
    pub fn consolidate(
        &mut self,
        faces: &mut Faces,
        secondary_db: &str,
        send_to_addr: &str,
    ) -> bool {
        let token_pos = match send_to_addr.rfind(':') {
            Some(pos) => pos,
            None => {
                self.log.push(format!(
                    "TransferDB::consolidate: Address provided appears invalid '{send_to_addr}'. Expecting <ip:port>\n"
                ));
                return false;
            }
        };

        let ip_addr_str = &send_to_addr[..token_pos];
        let port_str = &send_to_addr[token_pos + 1..];

        let ip = match convert_address(ip_addr_str) {
            Some(ip) => ip,
            None => {
                self.log.push(format!(
                    "TransferDB::consolidate: Unable to convert string address '{ip_addr_str}'.\n"
                ));
                return false;
            }
        };

        // TODO: strtoul() — the reference's own TODO; the port keeps atoi and
        // the network-order port.
        let address = Address {
            ip,
            port: (atoi_c(port_str) as u16).swap_bytes(),
        };

        faces.consolidator.transfer_to(secondary_db, address)
    }

    /// `TransferDB::snapshotPrimary` (`transfer_db.cpp:177-197`).
    pub fn snapshot_primary(
        &mut self,
        faces: &mut Faces,
        destination_ip: &str,
        destination_path: &str,
        limit_kbps: &str,
    ) -> bool {
        let mut snapshot = Snapshot::default();

        if !snapshot.init(destination_ip, destination_path, limit_kbps) {
            self.log.push(
                "TransferDB::snapshotPrimary: Failed to initialise the snapshotter. Terminating.\n"
                    .to_string(),
            );
            return false;
        }

        let was_snapshot_successful = snapshot.transfer_primary(faces, self.log);
        if was_snapshot_successful {
            self.log
                .push("TransferDB::snapshotPrimary: Completed.\n".to_string());
        }

        was_snapshot_successful
    }

    /// `TransferDB::snapshotSecondary` (`transfer_db.cpp:203-223`). The init
    /// failure returns silently — asymmetric with `snapshotPrimary`.
    pub fn snapshot_secondary(
        &mut self,
        faces: &mut Faces,
        secondary_db: &str,
        destination_ip: &str,
        destination_path: &str,
        limit_kbps: &str,
    ) -> bool {
        let mut snapshot = Snapshot::default();

        if !snapshot.init(destination_ip, destination_path, limit_kbps) {
            return false;
        }

        let was_snapshot_successful = snapshot.transfer_secondary(faces, self.log, secondary_db);
        if was_snapshot_successful {
            self.log.push(format!(
                "TransferDB::snapshotSecondary: Completed transfer of '{secondary_db}'\n"
            ));
        }

        was_snapshot_successful
    }
}

/// `Snapshot` (`snapshot.hpp`): the snapshot/transfer machinery both primary
/// and secondary commands drive.
#[derive(Default)]
pub struct Snapshot {
    destination_ip: String,
    destination_path: String,
    limit_kbps: String,
    /// `pSignalProcessor_` — created in `init`, destroyed in the child before
    /// each exec (the port models both ends as this flag).
    signal_processor_armed: bool,
}

impl Snapshot {
    /// `Snapshot::init` (`snapshot.cpp:160-177`): store the destination and
    /// arm the signal processor. Always true on POSIX.
    pub fn init(&mut self, destination_ip: &str, destination_path: &str, limit_kbps: &str) -> bool {
        self.signal_processor_armed = true;
        self.destination_ip = destination_ip.to_string();
        self.destination_path = destination_path.to_string();
        self.limit_kbps = limit_kbps.to_string();
        true
    }

    /// `Snapshot::onChildAboutToExec` (`snapshot.cpp:223-227`).
    pub fn on_child_about_to_exec(&mut self) {
        self.signal_processor_armed = false;
    }

    /// Whether the signal processor is still armed (false once a child has
    /// been dispatched).
    pub fn is_signal_processor_armed(&self) -> bool {
        self.signal_processor_armed
    }

    /// The merged `ip:path` form rsync copies to (`snapshot.cpp:357-362`).
    pub fn destination(&self) -> String {
        format!("{}:{}", self.destination_ip, self.destination_path)
    }

    /// One child dispatch: the child-side hooks fire, then the process runs
    /// to completion under the dispatcher pump.
    fn run_child(
        &mut self,
        children: &mut dyn ChildRunner,
        path: &str,
        argv: &[String],
        pipe_stdout: bool,
        pipe_stderr: bool,
    ) -> ChildRun {
        self.on_child_about_to_exec();
        children.about_to_exec();
        children.start_with_pipe(path, argv, pipe_stdout, pipe_stderr)
    }

    /// `Snapshot::isSnapshotHelperSetUidRoot` (`snapshot.cpp:184-216`): run
    /// the helper without arguments to test it is setuid-root capable.
    fn is_snapshot_helper_set_uid_root(
        &mut self,
        children: &mut dyn ChildRunner,
        log: &mut Vec<String>,
    ) -> bool {
        let path_to_snapshot_helper = format!("_helpers/{SNAPSHOT_HELPER}");

        let child = self.run_child(children, &path_to_snapshot_helper, &[], true, true);

        if !child.was_successful(SNAPSHOT_HELPER_SUCCESS) {
            log.push(
                "Snapshot::isSnapshotHelperSetUidRoot: snapshot_helper failed to run.\n"
                    .to_string(),
            );
            log.push(format!("snapshot_helper stdout:\n{}\n", child.stdout));
            log.push(format!("snapshot_helper stderr:\n{}\n", child.stderr));
            return false;
        }

        true
    }

    /// `Snapshot::takePrimarySnapshot` (`snapshot.cpp:234-310`): run the
    /// helper to acquire the snapshot and pull the snapshot files path out of
    /// its stdout tail.
    fn take_primary_snapshot(
        &mut self,
        children: &mut dyn ChildRunner,
        db_config: &mut dyn DbConfigFace,
        log: &mut Vec<String>,
    ) -> Option<String> {
        let db_username = db_config.mysql_username();
        let db_password = db_config.mysql_password();

        if db_username.is_empty() || db_password.is_empty() {
            log.push(
                "Snapshot::takePrimarySnapshot: dbapp username / password empty.\n".to_string(),
            );
            return None;
        }

        // Snapshot helper command:
        // snapshot_helper  acquire-snapshot  <db username>  <db password>
        let path_to_snapshot_helper = format!("_helpers/{SNAPSHOT_HELPER}");
        let argv = vec!["acquire-snapshot".to_string(), db_username, db_password];

        let child = self.run_child(children, &path_to_snapshot_helper, &argv, true, true);

        if !child.was_successful(SNAPSHOT_HELPER_SUCCESS) {
            log.push(
                "Snapshot::takePrimarySnapshot: snapshot_helper failed to acquire a snapshot.\n"
                    .to_string(),
            );
            log.push(format!("snapshot_helper stdout:\n{}\n", child.stdout));
            log.push(format!("snapshot_helper stderr:\n{}\n", child.stderr));
            return None;
        }

        snapshot_path_from_stdout(&child.stdout, log)
    }

    /// `Snapshot::removePrimarySnapshot` (`snapshot.cpp:317-350`).
    fn remove_primary_snapshot(
        &mut self,
        children: &mut dyn ChildRunner,
        log: &mut Vec<String>,
    ) -> bool {
        // Snapshot helper command:
        // snapshot_helper  release-snapshot
        let path_to_snapshot_helper = format!("_helpers/{SNAPSHOT_HELPER}");

        let child = self.run_child(
            children,
            &path_to_snapshot_helper,
            &["release-snapshot".to_string()],
            true,
            true,
        );

        if !child.was_successful(SNAPSHOT_HELPER_SUCCESS) {
            log.push("Snapshot::removePrimarySnapshot: Failed to release snapshot\n".to_string());
            log.push(format!("snapshot_helper stdout:\n{}\n", child.stdout));
            log.push(format!("snapshot_helper stderr:\n{}\n", child.stderr));
            return false;
        }

        true
    }

    /// `Snapshot::sendSnapshot` (`snapshot.cpp:368-408`): rsync the snapshot
    /// to the destination, honouring a nonzero kbps limit.
    fn send_snapshot(
        &mut self,
        children: &mut dyn ChildRunner,
        log: &mut Vec<String>,
        src_path: &str,
    ) -> bool {
        let mut argv = vec![
            "--archive".to_string(),
            "--no-specials".to_string(),
            "--no-devices".to_string(),
            src_path.to_string(),
            self.destination(),
        ];

        // TODO: change atoi to strtoul — the reference's own TODO.
        if atoi_c(&self.limit_kbps) != 0 {
            argv.push(format!("--bwlimit={}", self.limit_kbps));
        }

        let child = self.run_child(children, RSYNC, &argv, false, true);

        if !child.was_successful(EXIT_SUCCESS) {
            log.push("Snapshot::sendSnapshot: rsync failed to transfer snapshot.\n".to_string());
            log.push(format!("rsync stderr:\n{}\n", child.stderr));
            return false;
        }

        true
    }

    /// `Snapshot::sendCompletionFile` (`snapshot.cpp:414-466`): a single
    /// empty file tells the receiving end the transfer is done. A failed
    /// rsync still unlinks (the reference's own note), but reports false.
    fn send_completion_file(
        &mut self,
        children: &mut dyn ChildRunner,
        files: &mut dyn SnapshotFiles,
        log: &mut Vec<String>,
        src_path: &str,
    ) -> bool {
        let src_base_path = format!("/tmp/{}.complete", basename(src_path));

        if let Err(errno_text) = files.create_file(&src_base_path) {
            log.push(format!(
                "Snapshot::sendCompletionFile: Unable to create completion file '{src_base_path}': {errno_text}\n"
            ));
            return false;
        }

        let child = self.run_child(
            children,
            RSYNC,
            &[src_base_path.clone(), self.destination()],
            false,
            true,
        );

        if !child.was_successful(EXIT_SUCCESS) {
            log.push("Snapshot::sendCompletionFile: rsync failed to transfer snapshot. stderr follows.\n".to_string());
            log.push(format!("{}\n", child.stderr));
            // Purposefully not returning false here to allow unlinking
        }

        if let Err(errno_text) = files.unlink(&src_base_path) {
            log.push(format!(
                "Snapshot::sendCompletionFile: Unable to unlink completion file '{src_base_path}': {errno_text}\n"
            ));
            return false;
        }

        child.was_successful(EXIT_SUCCESS)
    }

    /// `Snapshot::transferSnapshotViaRsync` (`snapshot.cpp:473-477`).
    fn transfer_snapshot_via_rsync(
        &mut self,
        children: &mut dyn ChildRunner,
        files: &mut dyn SnapshotFiles,
        log: &mut Vec<String>,
        src_path: &str,
    ) -> bool {
        self.send_snapshot(children, log, src_path)
            && self.send_completion_file(children, files, log, src_path)
    }

    /// `Snapshot::transferPrimary` (`snapshot.cpp:483-538`): probe the
    /// helper, take the snapshot, rsync it back, then release it — the
    /// release runs even when the transfer failed, and the return needs both.
    pub fn transfer_primary(&mut self, faces: &mut Faces, log: &mut Vec<String>) -> bool {
        log.push(
            "Snapshot::transferPrimary: Starting primary DB snapshot and transfer.\n".to_string(),
        );

        // Test that the snapshot_helper process can be run before using it.
        log.push("Snapshot::transferPrimary: Testing snapshot_helper is installed with setuid root privileges.\n".to_string());

        if !self.is_snapshot_helper_set_uid_root(faces.children, log) {
            log.push("Snapshot::transferPrimary: snapshot_helper binary appears to be incorrectly installed.\n".to_string());
            return false;
        }

        log.push("Snapshot::transferPrimary: Attempting to run snapshot_helper to create image of primary DB.\n".to_string());

        // Take the snapshot using snapshot_helper
        let path_to_snapshot =
            match self.take_primary_snapshot(faces.children, faces.db_config, log) {
                Some(path) => path,
                None => {
                    log.push(
                        "Snapshot::transferPrimary: Failed to take primary snapshot.\n".to_string(),
                    );
                    return false;
                }
            };

        // Once the snapshot has been successfully taken it must be sent back to
        // the machine that initiated the snapshotting procedure.
        log.push(format!(
            "Snapshot::transferPrimary: Attempting to send new snapshot back to {} using rsync.\n",
            self.destination_ip
        ));

        let was_transferred =
            self.transfer_snapshot_via_rsync(faces.children, faces.files, log, &path_to_snapshot);
        if !was_transferred {
            log.push("Snapshot::transferPrimary: Failed to transfer successful snapshot back to the requesting host.\n".to_string());
        }

        // Now that the snapshot has been transferred back to the requesting machine
        // we remove the snapshot directory using snapshot_helper.
        log.push(
            "Snapshot::transferPrimary: Attempting to remove successfully transferred snapshot.\n"
                .to_string(),
        );
        let was_snapshot_removed = self.remove_primary_snapshot(faces.children, log);
        if !was_snapshot_removed {
            log.push(
                "Snapshot::transferPrimary: Failed to release snapshot of primary database.\n"
                    .to_string(),
            );
        }

        was_transferred && was_snapshot_removed
    }

    /// `Snapshot::backupSqliteDatabase` (`snapshot.cpp:550-573`).
    fn backup_sqlite_database(
        &mut self,
        files: &mut dyn SnapshotFiles,
        log: &mut Vec<String>,
        src_path: &str,
        dest_path: &str,
    ) -> bool {
        match files.backup_sqlite(src_path, dest_path) {
            Ok(()) => true,
            Err(SqliteBackupError::SourceInaccessible) => {
                log.push(
                    "Snapshot::backupSqliteDatabase: Source database inaccessible\n".to_string(),
                );
                false
            }
            Err(SqliteBackupError::DestinationInaccessible) => {
                log.push(
                    "Snapshot::backupSqliteDatabase: Destination database inaccessible\n"
                        .to_string(),
                );
                false
            }
            // The reference returns the backup call's own result; no message.
            Err(SqliteBackupError::BackupFailed) => false,
        }
    }

    /// `Snapshot::transferSecondary` (`snapshot.cpp:584-650`): safe sqlite
    /// backup into a fresh temp dir, rsync it back, clean up — cleanup
    /// failures are logged but never fail the transfer.
    pub fn transfer_secondary(
        &mut self,
        faces: &mut Faces,
        log: &mut Vec<String>,
        sqlite_path: &str,
    ) -> bool {
        log.push(
            "Snapshot::transferSecondary: Starting secondary DB snapshot and transfer.\n"
                .to_string(),
        );

        let sqlite_filename = basename(sqlite_path);

        let temp_dirname = match faces.files.make_temp_dir(TEMP_DIR_TEMPLATE) {
            Ok(dir) => dir,
            Err(errno_text) => {
                // The reference message carries no trailing newline here.
                log.push(format!("Snapshot::transferSecondary: Failed to create temporary directory: {errno_text}"));
                return false;
            }
        };

        let snapshot_path = format!("{temp_dirname}/{sqlite_filename}");

        // For details on the sqlite backup functionality see
        //  http://www.sqlite.org/backup.html
        let was_copied = self.backup_sqlite_database(faces.files, log, sqlite_path, &snapshot_path);
        if !was_copied {
            log.push(format!(
                "Snapshot::transferSecondary: Failed to backup the sqlite db '{sqlite_path}'.\n"
            ));
        }

        let mut was_transferred = false;
        if was_copied {
            // Perform the rsync of the backup now
            was_transferred =
                self.transfer_snapshot_via_rsync(faces.children, faces.files, log, &snapshot_path);
            if !was_transferred {
                log.push("Snapshot::transferSecondary: Failed to transfer successful secondary snapshot back to the requesting host.\n".to_string());
            }
        }

        if let Err(errno_text) = faces.files.unlink(&snapshot_path) {
            log.push(format!(
                "Snapshot::transferSecondary: Unable to remove secondary db snapshot '{snapshot_path}': {errno_text}\n"
            ));
            // This shouldn't be considered a failure case as the temporary
            // directory won't hinder any operational behaviour.
        }

        if let Err(errno_text) = faces.files.rmdir(&temp_dirname) {
            log.push(format!(
                "Snapshot::transferSecondary: Unable to remove temporary directory '{temp_dirname}': {errno_text}\n"
            ));
            // This shouldn't be considered a failure case as the temporary
            // directory won't hinder any operational behaviour.
        }

        was_copied && was_transferred
    }
}

/// `takePrimarySnapshot`'s stdout extraction (`snapshot.cpp:281-305`): the
/// path sits between the last two newlines. A missing newline and a missing
/// second newline are distinct error branches; an index-0 newline makes the
/// C position arithmetic underflow and yields the empty path (kept verbatim).
fn snapshot_path_from_stdout(stdout: &str, log: &mut Vec<String>) -> Option<String> {
    let eol_pos = match stdout.rfind('\n') {
        Some(pos) => pos,
        None => {
            log.push("Snapshot::takePrimarySnapshot: Unable to find snapshot path from snapshot_helper output.\n".to_string());
            log.push(format!("{stdout}\n"));
            return None;
        }
    };

    // C++ rfind('\n', eolPos-1): when eolPos is 0 the position underflows to
    // npos and the search covers the whole string, finding the index-0 '\n'.
    let start_path_pos = if eol_pos == 0 {
        0
    } else {
        match stdout[..eol_pos].rfind('\n') {
            Some(pos) => pos,
            None => {
                log.push("Snapshot::takePrimarySnapshot: Unable to find start of snapshot path from snapshot_helper output.\n".to_string());
                return None;
            }
        }
    };

    // Move the start position right so we don't include the \n
    let start_path_pos = start_path_pos + 1;
    Some(if start_path_pos > eol_pos {
        // The wrapped substr count takes everything after the '\n'.
        stdout[start_path_pos..].to_string()
    } else {
        stdout[start_path_pos..eol_pos].to_string()
    })
}

/// `BIGWORLD_MAIN` (`main.cpp:26-126`): init, then walk the argument list.
/// `--res`/`-r` skip their following argument (only when one exists), unknown
/// tokens are skipped silently, the first command runs and breaks the loop,
/// and any command failure is an immediate `EXIT_FAILURE`.
pub fn main_face(args: &[String], faces: &mut Faces, log: &mut Vec<String>) -> i32 {
    let is_verbose = true;

    // DebugFilter::shouldWriteToConsole( isVerbose )
    faces.logger.set_write_to_console(is_verbose);

    let mut transfer_db = TransferDB { log: &mut *log };

    if !transfer_db.init(faces, is_verbose) {
        transfer_db.log.push("Initialisation failed\n".to_string());
        return EXIT_FAILURE;
    }

    // START_MSG( "TransferDB" ) — a logger-side banner, not part of the
    // reviewed data flow.

    let mut i = 1usize;
    while i < args.len() {
        let arg = args[i].clone();

        if (arg == "--res" || arg == "-r") && i < args.len() - 1 {
            i += 1;
        } else if arg == "consolidate" {
            // We expect 2 more args for consolidate db
            if args.len() - (i + 1) < 2 {
                transfer_db.log.push(
                    "Incorrect number of arguments provided for 'consolidate' command.\n"
                        .to_string(),
                );
                return EXIT_FAILURE;
            }

            let secondary_db = args[i + 1].clone();
            let send_to_addr = args[i + 2].clone();

            if !transfer_db.consolidate(faces, &secondary_db, &send_to_addr) {
                return EXIT_FAILURE;
            }

            break;
        } else if arg == "snapshotprimary" {
            // We expect 3 more args for primary DB snapshotting
            if args.len() - (i + 1) < 3 {
                transfer_db.log.push(
                    "Incorrect number of arguments provided for 'snapshotprimary' command.\n"
                        .to_string(),
                );
                return EXIT_FAILURE;
            }

            let dest_ip = args[i + 1].clone();
            let dest_path = args[i + 2].clone();
            let limit_kbps = args[i + 3].clone();

            if !transfer_db.snapshot_primary(faces, &dest_ip, &dest_path, &limit_kbps) {
                return EXIT_FAILURE;
            }
            break;
        } else if arg == "snapshotsecondary" {
            // We expect 4 more args for secondary DB snapshotting
            if args.len() - (i + 1) < 4 {
                transfer_db.log.push(
                    "Incorrect number of arguments provided for 'snapshotsecondary' command.\n"
                        .to_string(),
                );
                return EXIT_FAILURE;
            }

            let secondary_db = args[i + 1].clone();
            let dest_ip = args[i + 2].clone();
            let dest_path = args[i + 3].clone();
            let limit_kbps = args[i + 4].clone();

            if !transfer_db.snapshot_secondary(
                faces,
                &secondary_db,
                &dest_ip,
                &dest_path,
                &limit_kbps,
            ) {
                return EXIT_FAILURE;
            }
            break;
        }

        i += 1;
    }

    EXIT_SUCCESS
}

#[cfg(test)]
mod tests;
