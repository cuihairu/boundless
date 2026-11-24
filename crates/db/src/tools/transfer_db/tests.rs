//! Target-data pins for the transfer_db port: the recorded child vectors,
//! filesystem calls and message log are the acceptance artifact ("目标数据和
//! 退出码吻合"), alongside the exit codes of every main-loop branch.

use super::*;
use std::collections::VecDeque;

/// The process/logging infrastructure double.
struct InfraDouble {
    logger_id: String,
    interface: Result<String, String>,
    watcher_ok: bool,
    forwarders: Vec<(String, String)>,
    console: Vec<bool>,
    threshold_set: bool,
}

impl Default for InfraDouble {
    fn default() -> Self {
        InfraDouble {
            logger_id: String::new(),
            interface: Ok("10.0.0.9".to_string()),
            watcher_ok: true,
            forwarders: Vec::new(),
            console: Vec::new(),
            threshold_set: false,
        }
    }
}

impl LoggerInfra for InfraDouble {
    fn logger_id(&mut self) -> String {
        self.logger_id.clone()
    }

    fn query_internal_interface(&mut self) -> Result<String, String> {
        self.interface.clone()
    }

    fn watcher_init(&mut self, _ip: &str, _port: u16) -> bool {
        self.watcher_ok
    }

    fn forwarder_start(&mut self, app_name: &str, logger_id: &str) {
        self.forwarders
            .push((app_name.to_string(), logger_id.to_string()));
    }

    fn set_write_to_console(&mut self, enabled: bool) {
        self.console.push(enabled);
    }

    fn set_filter_threshold_info(&mut self) {
        self.threshold_set = true;
    }
}

/// The child-process double: records the dispatch vectors, hands out canned
/// runs, and falls back to a bare exit-0 run when the queue runs dry.
struct RunnerDouble {
    runs: Vec<(String, Vec<String>, bool, bool)>,
    hooks: usize,
    outcomes: VecDeque<ChildRun>,
}

impl Default for RunnerDouble {
    fn default() -> Self {
        RunnerDouble {
            runs: Vec::new(),
            hooks: 0,
            outcomes: VecDeque::new(),
        }
    }
}

impl ChildRunner for RunnerDouble {
    fn about_to_exec(&mut self) {
        self.hooks += 1;
    }

    fn start_with_pipe(
        &mut self,
        path: &str,
        argv: &[String],
        pipe_stdout: bool,
        pipe_stderr: bool,
    ) -> ChildRun {
        self.runs
            .push((path.to_string(), argv.to_vec(), pipe_stdout, pipe_stderr));
        self.outcomes
            .pop_front()
            .unwrap_or_else(|| ChildRun::exited_with(0))
    }
}

/// The helper's happy stdout: banner line, then the snapshot path the reader
/// contract takes (the double-slash is the reference's own concatenation).
fn helper_stdout() -> String {
    "SnapshotHelper started\n/mnt/snap0//var/lib/mysql\n".to_string()
}

fn ok_run() -> ChildRun {
    ChildRun {
        exited: true,
        exit_code: 0,
        stdout: helper_stdout(),
        stderr: String::new(),
    }
}

fn fail_run() -> ChildRun {
    ChildRun {
        exited: true,
        exit_code: 1,
        stdout: "SnapshotHelper started\n".to_string(),
        stderr: "boom\n".to_string(),
    }
}

/// The filesystem/sqlite double.
struct FilesDouble {
    created: Vec<String>,
    create_err_for: Vec<String>,
    unlinks: Vec<String>,
    unlink_err_for: Vec<String>,
    rmdirs: Vec<String>,
    rmdir_err_for: Vec<String>,
    templates: Vec<String>,
    temp_dir: String,
    mkdtemp_err: Option<String>,
    backups: Vec<(String, String)>,
    backup_result: Result<(), SqliteBackupError>,
}

impl Default for FilesDouble {
    fn default() -> Self {
        FilesDouble {
            created: Vec::new(),
            create_err_for: Vec::new(),
            unlinks: Vec::new(),
            unlink_err_for: Vec::new(),
            rmdirs: Vec::new(),
            rmdir_err_for: Vec::new(),
            templates: Vec::new(),
            temp_dir: String::new(),
            mkdtemp_err: None,
            backups: Vec::new(),
            backup_result: Ok(()),
        }
    }
}

impl SnapshotFiles for FilesDouble {
    fn create_file(&mut self, path: &str) -> Result<(), String> {
        if self.create_err_for.iter().any(|p| p == path) {
            return Err("Permission denied".to_string());
        }
        self.created.push(path.to_string());
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> Result<(), String> {
        if self.unlink_err_for.iter().any(|p| p == path) {
            return Err("No such file or directory".to_string());
        }
        self.unlinks.push(path.to_string());
        Ok(())
    }

    fn rmdir(&mut self, path: &str) -> Result<(), String> {
        if self.rmdir_err_for.iter().any(|p| p == path) {
            return Err("Directory not empty".to_string());
        }
        self.rmdirs.push(path.to_string());
        Ok(())
    }

    fn make_temp_dir(&mut self, template: &str) -> Result<String, String> {
        self.templates.push(template.to_string());
        match self.mkdtemp_err.clone() {
            Some(err) => Err(err),
            None => Ok(self.temp_dir.clone()),
        }
    }

    fn backup_sqlite(&mut self, src: &str, dest: &str) -> Result<(), SqliteBackupError> {
        self.backups.push((src.to_string(), dest.to_string()));
        self.backup_result
    }
}

/// The `DBConfig::get().mysql` double.
struct ConfigDouble {
    user: String,
    pass: String,
}

impl Default for ConfigDouble {
    fn default() -> Self {
        ConfigDouble {
            user: "bw".to_string(),
            pass: "secret".to_string(),
        }
    }
}

impl DbConfigFace for ConfigDouble {
    fn mysql_username(&self) -> String {
        self.user.clone()
    }

    fn mysql_password(&self) -> String {
        self.pass.clone()
    }
}

/// The consolidate delegate double (the real face lands with BW-09-05-05).
struct ConsolidatorDouble {
    calls: Vec<(String, Address)>,
    ok: bool,
}

impl Default for ConsolidatorDouble {
    fn default() -> Self {
        ConsolidatorDouble {
            calls: Vec::new(),
            ok: true,
        }
    }
}

impl ConsolidatorDouble {
    fn failing() -> Self {
        ConsolidatorDouble {
            calls: Vec::new(),
            ok: false,
        }
    }
}

impl ConsolidateFace for ConsolidatorDouble {
    fn transfer_to(&mut self, secondary_db: &str, address: Address) -> bool {
        self.calls.push((secondary_db.to_string(), address));
        self.ok
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Drive `main_face` over the five doubles.
#[allow(clippy::too_many_arguments)]
fn run_main(
    list: &[&str],
    infra: &mut InfraDouble,
    runner: &mut RunnerDouble,
    files: &mut FilesDouble,
    config: &mut ConfigDouble,
    consolidator: &mut ConsolidatorDouble,
    log: &mut Vec<String>,
) -> i32 {
    let mut faces = Faces {
        logger: infra,
        children: runner,
        files,
        db_config: config,
        consolidator,
    };
    main_face(&args(list), &mut faces, log)
}

// -----------------------------------------------------------------------------
// init and the main loop
// -----------------------------------------------------------------------------

#[test]
fn an_interface_query_failure_fails_init() {
    let mut infra = InfraDouble::default();
    infra.interface = Err("no machined".to_string());
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db"],
        &mut infra,
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(
        log.iter()
            .any(|l| l == "TransferDB::initLogger: failed to query for internal interface\n")
    );
    assert!(log.iter().any(|l| l == "Initialisation failed\n"));
}

#[test]
fn a_watcher_init_failure_is_silent_but_fatal() {
    let mut infra = InfraDouble::default();
    infra.watcher_ok = false;
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db"],
        &mut infra,
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    // initLogger itself stays silent on this branch; init reports the failure.
    assert_eq!(
        log,
        [
            "TransferDB::init: Failed to initialise logger\n",
            "Initialisation failed\n"
        ]
    );
}

#[test]
fn no_arguments_exits_success() {
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db"],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
}

#[test]
fn unknown_arguments_are_skipped_silently() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut runner = RunnerDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "--what", "x"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert!(consolidator.calls.is_empty());
    assert!(runner.runs.is_empty());
}

#[test]
fn the_res_flag_skips_its_following_argument() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "--res",
            "bigworld/res",
            "consolidate",
            "sec.db",
            "1.2.3.4:300",
        ],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(consolidator.calls.len(), 1);
    assert_eq!(consolidator.calls[0].0, "sec.db");
}

#[test]
fn the_short_r_flag_behaves_the_same() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "-r",
            "bigworld/res",
            "consolidate",
            "sec.db",
            "1.2.3.4:300",
        ],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(consolidator.calls.len(), 1);
}

#[test]
fn the_trailing_res_flag_is_not_a_command() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut log = Vec::new();
    // i == argc-1: the skip rule does not fire and no command matches.
    let code = run_main(
        &["transfer_db", "--res"],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert!(consolidator.calls.is_empty());
}

#[test]
fn arity_complaints_are_verbatim_per_command() {
    for (list, message) in [
        (
            &["transfer_db", "consolidate", "x"][..],
            "Incorrect number of arguments provided for 'consolidate' command.\n",
        ),
        (
            &["transfer_db", "snapshotprimary", "a", "b"][..],
            "Incorrect number of arguments provided for 'snapshotprimary' command.\n",
        ),
        (
            &["transfer_db", "snapshotsecondary", "a", "b", "c"][..],
            "Incorrect number of arguments provided for 'snapshotsecondary' command.\n",
        ),
    ] {
        let mut log = Vec::new();
        let code = run_main(
            list,
            &mut InfraDouble::default(),
            &mut RunnerDouble::default(),
            &mut FilesDouble::default(),
            &mut ConfigDouble::default(),
            &mut ConsolidatorDouble::default(),
            &mut log,
        );
        assert_eq!(code, EXIT_FAILURE);
        assert!(
            log.iter().any(|l| l == message),
            "missing {message} in {log:?}"
        );
    }
}

#[test]
fn the_first_command_wins_and_breaks_the_loop() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut runner = RunnerDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "consolidate",
            "s.db",
            "1.2.3.4:1",
            "snapshotprimary",
            "2.3.4.5",
            "/p",
            "64",
        ],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(consolidator.calls.len(), 1);
    // The snapshot command after the consolidate never ran.
    assert!(runner.runs.is_empty());
}

#[test]
fn a_failed_consolidate_is_exit_failure() {
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "consolidate", "s.db", "1.2.3.4:1"],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::failing(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
}

// -----------------------------------------------------------------------------
// consolidate address parsing
// -----------------------------------------------------------------------------

#[test]
fn the_consolidate_address_splits_and_converts() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "consolidate", "s.db", "1.2.3.4:300"],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        consolidator.calls[0].1,
        Address {
            ip: 0x0403_0201,
            // ntohs(atoi(...)): Mercury keeps the port byte-swapped.
            port: 300u16.swap_bytes(),
        }
    );
}

#[test]
fn an_address_without_a_colon_is_rejected() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "consolidate", "s.db", "nocolon"],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(log.iter().any(|l| l == "TransferDB::consolidate: Address provided appears invalid 'nocolon'. Expecting <ip:port>\n"));
    assert!(consolidator.calls.is_empty());
}

#[test]
fn an_unconvertible_ip_is_rejected() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "consolidate", "s.db", "nohost:300"],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(
        log.iter()
            .any(|l| l == "TransferDB::consolidate: Unable to convert string address 'nohost'.\n")
    );
    assert!(consolidator.calls.is_empty());
}

#[test]
fn a_non_numeric_port_acts_as_zero() {
    let mut consolidator = ConsolidatorDouble::default();
    let mut log = Vec::new();
    // atoi("notaport") == 0 — nothing validates the port before ntohs.
    let code = run_main(
        &["transfer_db", "consolidate", "s.db", "1.2.3.4:notaport"],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut consolidator,
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(consolidator.calls[0].1.port, 0);
}

// -----------------------------------------------------------------------------
// the primary snapshot path
// -----------------------------------------------------------------------------

#[test]
fn the_happy_primary_path_runs_the_five_children() {
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([ok_run(), ok_run()]);
    let mut files = FilesDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        runner.runs,
        [
            (
                "_helpers/snapshot_helper".to_string(),
                Vec::<String>::new(),
                true,
                true,
            ),
            (
                "_helpers/snapshot_helper".to_string(),
                vec![
                    "acquire-snapshot".to_string(),
                    "bw".to_string(),
                    "secret".to_string(),
                ],
                true,
                true,
            ),
            (
                RSYNC.to_string(),
                vec![
                    "--archive".to_string(),
                    "--no-specials".to_string(),
                    "--no-devices".to_string(),
                    "/mnt/snap0//var/lib/mysql".to_string(),
                    "10.0.0.2:/dest".to_string(),
                ],
                false,
                true,
            ),
            (
                RSYNC.to_string(),
                vec![
                    "/tmp/mysql.complete".to_string(),
                    "10.0.0.2:/dest".to_string()
                ],
                false,
                true,
            ),
            (
                "_helpers/snapshot_helper".to_string(),
                vec!["release-snapshot".to_string()],
                true,
                true,
            ),
        ]
    );
    // Every child dispatch ran the child-side fd-closing hooks.
    assert_eq!(runner.hooks, 5);
    assert_eq!(files.created, ["/tmp/mysql.complete"]);
    assert_eq!(files.unlinks, ["/tmp/mysql.complete"]);
    for line in [
        "Snapshot::transferPrimary: Starting primary DB snapshot and transfer.\n",
        "Snapshot::transferPrimary: Testing snapshot_helper is installed with setuid root privileges.\n",
        "Snapshot::transferPrimary: Attempting to run snapshot_helper to create image of primary DB.\n",
        "Snapshot::transferPrimary: Attempting to send new snapshot back to 10.0.0.2 using rsync.\n",
        "Snapshot::transferPrimary: Attempting to remove successfully transferred snapshot.\n",
        "TransferDB::snapshotPrimary: Completed.\n",
    ] {
        assert!(log.iter().any(|l| l == line), "missing {line} in {log:?}");
    }
}

#[test]
fn the_bwlimit_argument_only_appears_for_nonzero_kbps() {
    for (kbps, bwlimit) in [("4096", Some("--bwlimit=4096")), ("abc", None), ("0", None)] {
        let mut runner = RunnerDouble::default();
        runner.outcomes = VecDeque::from([ok_run(), ok_run()]);
        let mut log = Vec::new();
        let code = run_main(
            &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", kbps],
            &mut InfraDouble::default(),
            &mut runner,
            &mut FilesDouble::default(),
            &mut ConfigDouble::default(),
            &mut ConsolidatorDouble::default(),
            &mut log,
        );
        assert_eq!(code, EXIT_SUCCESS);
        let rsync_argv = &runner.runs[2].1;
        assert_eq!(
            rsync_argv.iter().any(|a| a.starts_with("--bwlimit")),
            bwlimit.is_some()
        );
        if let Some(limit) = bwlimit {
            assert_eq!(rsync_argv.last().map(String::as_str), Some(limit));
        }
    }
}

#[test]
fn a_failing_setuid_probe_aborts_the_transfer() {
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([fail_run()]);
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(
        log.iter()
            .any(|l| l == "Snapshot::isSnapshotHelperSetUidRoot: snapshot_helper failed to run.\n")
    );
    assert!(
        log.iter()
            .any(|l| l == "snapshot_helper stdout:\nSnapshotHelper started\n\n")
    );
    assert!(log.iter().any(|l| l == "snapshot_helper stderr:\nboom\n\n"));
    assert!(log.iter().any(|l| l == "Snapshot::transferPrimary: snapshot_helper binary appears to be incorrectly installed.\n"));
    assert_eq!(runner.runs.len(), 1);
}

#[test]
fn empty_db_credentials_stop_the_snapshot() {
    let mut config = ConfigDouble::default();
    config.user = String::new();
    let mut runner = RunnerDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut config,
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    // The credential check precedes the acquire dispatch.
    assert_eq!(runner.runs.len(), 1);
    assert!(
        log.iter()
            .any(|l| l == "Snapshot::takePrimarySnapshot: dbapp username / password empty.\n")
    );
    assert!(
        log.iter()
            .any(|l| l == "Snapshot::transferPrimary: Failed to take primary snapshot.\n")
    );
}

#[test]
fn a_failing_acquire_aborts_before_any_rsync() {
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([ok_run(), fail_run()]);
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(
        log.iter().any(|l| l
            == "Snapshot::takePrimarySnapshot: snapshot_helper failed to acquire a snapshot.\n")
    );
    assert!(
        log.iter()
            .any(|l| l == "Snapshot::transferPrimary: Failed to take primary snapshot.\n")
    );
    // No rsync, and no release either — the transfer bails outright.
    assert_eq!(runner.runs.len(), 2);
}

#[test]
fn stdout_without_a_newline_is_an_error() {
    let mut runner = RunnerDouble::default();
    let mut banner = ok_run();
    banner.stdout = "no newline here".to_string();
    runner.outcomes = VecDeque::from([ok_run(), banner]);
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(log.iter().any(|l| l == "Snapshot::takePrimarySnapshot: Unable to find snapshot path from snapshot_helper output.\n"));
    assert!(log.iter().any(|l| l == "no newline here\n"));
}

#[test]
fn a_single_trailing_newline_is_an_error_too() {
    let mut runner = RunnerDouble::default();
    let mut banner = ok_run();
    banner.stdout = "abc\n".to_string();
    runner.outcomes = VecDeque::from([ok_run(), banner]);
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(log.iter().any(|l| l == "Snapshot::takePrimarySnapshot: Unable to find start of snapshot path from snapshot_helper output.\n"));
}

#[test]
fn a_leading_newline_yields_the_wrapped_empty_path() {
    // eolPos == 0 underflows the rfind position and the substr count: the
    // reference takes the empty path and carries on.
    let mut runner = RunnerDouble::default();
    let mut bare = ok_run();
    bare.stdout = "\n".to_string();
    runner.outcomes = VecDeque::from([ok_run(), bare]);
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(runner.runs[2].1[3], "");
    assert_eq!(runner.runs[3].1[0], "/tmp/.complete");
    assert!(
        log.iter()
            .any(|l| l == "TransferDB::snapshotPrimary: Completed.\n")
    );
}

#[test]
fn a_failing_rsync_send_skips_the_completion_file() {
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([ok_run(), ok_run(), fail_run()]);
    let mut files = FilesDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(
        log.iter()
            .any(|l| l == "Snapshot::sendSnapshot: rsync failed to transfer snapshot.\n")
    );
    // The canned stderr already ends in a newline; %s\n adds another.
    assert!(log.iter().any(|l| l == "rsync stderr:\nboom\n\n"));
    assert!(log.iter().any(|l| l == "Snapshot::transferPrimary: Failed to transfer successful snapshot back to the requesting host.\n"));
    // No completion attempt, but the release still runs.
    assert!(files.created.is_empty());
    assert_eq!(runner.runs.len(), 4);
    assert_eq!(runner.runs[3].1, vec!["release-snapshot".to_string()]);
}

#[test]
fn completion_create_failure_is_reported() {
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([ok_run(), ok_run()]);
    let mut files = FilesDouble::default();
    files.create_err_for = vec!["/tmp/mysql.complete".to_string()];
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(log.iter().any(|l| l == "Snapshot::sendCompletionFile: Unable to create completion file '/tmp/mysql.complete': Permission denied\n"));
    // The release still runs after the failed completion.
    assert_eq!(runner.runs.len(), 4);
}

#[test]
fn a_failed_completion_rsync_still_unlinks() {
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([ok_run(), ok_run(), ChildRun::exited_with(0), fail_run()]);
    let mut files = FilesDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(log.iter().any(|l| l
        == "Snapshot::sendCompletionFile: rsync failed to transfer snapshot. stderr follows.\n"));
    assert!(log.iter().any(|l| l == "boom\n\n"));
    // Purposefully not returning false here to allow unlinking — the
    // reference's own comment; the unlink happened regardless.
    assert_eq!(files.unlinks, ["/tmp/mysql.complete"]);
    assert_eq!(runner.runs.len(), 5);
}

#[test]
fn a_failed_completion_unlink_reports_and_fails() {
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([ok_run(), ok_run()]);
    let mut files = FilesDouble::default();
    files.unlink_err_for = vec!["/tmp/mysql.complete".to_string()];
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(log.iter().any(|l| l == "Snapshot::sendCompletionFile: Unable to unlink completion file '/tmp/mysql.complete': No such file or directory\n"));
    assert_eq!(runner.runs.len(), 5);
}

#[test]
fn a_failing_release_fails_the_whole_transfer() {
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([
        ok_run(),
        ok_run(),
        ChildRun::exited_with(0),
        ChildRun::exited_with(0),
        fail_run(),
    ]);
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db", "snapshotprimary", "10.0.0.2", "/dest", "0"],
        &mut InfraDouble::default(),
        &mut runner,
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(
        log.iter()
            .any(|l| l == "Snapshot::removePrimarySnapshot: Failed to release snapshot\n")
    );
    assert!(log.iter().any(|l| l == "snapshot_helper stderr:\nboom\n\n"));
    assert!(log.iter().any(
        |l| l == "Snapshot::transferPrimary: Failed to release snapshot of primary database.\n"
    ));
    // The transfer itself succeeded, but both legs must hold.
    assert!(
        !log.iter()
            .any(|l| l == "TransferDB::snapshotPrimary: Completed.\n")
    );
}

// -----------------------------------------------------------------------------
// the secondary snapshot path
// -----------------------------------------------------------------------------

#[test]
fn the_happy_secondary_path_backs_up_transfers_and_cleans_up() {
    let mut files = FilesDouble::default();
    files.temp_dir = "/tmp/transferdb-abc123".to_string();
    let mut runner = RunnerDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "snapshotsecondary",
            "/var/db/sec.db",
            "10.0.0.2",
            "/dest",
            "4096",
        ],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(files.templates, [TEMP_DIR_TEMPLATE]);
    assert_eq!(
        files.backups,
        [(
            "/var/db/sec.db".to_string(),
            "/tmp/transferdb-abc123/sec.db".to_string(),
        )]
    );
    assert_eq!(
        runner.runs,
        [
            (
                RSYNC.to_string(),
                vec![
                    "--archive".to_string(),
                    "--no-specials".to_string(),
                    "--no-devices".to_string(),
                    "/tmp/transferdb-abc123/sec.db".to_string(),
                    "10.0.0.2:/dest".to_string(),
                    "--bwlimit=4096".to_string(),
                ],
                false,
                true,
            ),
            (
                RSYNC.to_string(),
                vec![
                    "/tmp/sec.db.complete".to_string(),
                    "10.0.0.2:/dest".to_string()
                ],
                false,
                true,
            ),
        ]
    );
    // The completion file unlinks first, then the snapshot copy.
    assert_eq!(
        files.unlinks,
        [
            "/tmp/sec.db.complete".to_string(),
            "/tmp/transferdb-abc123/sec.db".to_string(),
        ]
    );
    assert_eq!(files.rmdirs, ["/tmp/transferdb-abc123"]);
    assert!(log
        .iter()
        .any(|l| l == "TransferDB::snapshotSecondary: Completed transfer of '/var/db/sec.db'\n"));
}

#[test]
fn a_temp_dir_failure_stops_the_secondary_transfer() {
    let mut files = FilesDouble::default();
    files.mkdtemp_err = Some("No space left on device".to_string());
    let mut runner = RunnerDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "snapshotsecondary",
            "/var/db/sec.db",
            "10.0.0.2",
            "/dest",
            "0",
        ],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    // The reference message carries no trailing newline on this branch.
    assert!(log.iter().any(|l| l == "Snapshot::transferSecondary: Failed to create temporary directory: No space left on device"));
    assert!(runner.runs.is_empty());
    assert!(files.backups.is_empty());
}

#[test]
fn a_source_sqlite_failure_stops_before_rsync() {
    let mut files = FilesDouble::default();
    files.temp_dir = "/tmp/transferdb-abc123".to_string();
    files.backup_result = Err(SqliteBackupError::SourceInaccessible);
    let mut runner = RunnerDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "snapshotsecondary",
            "/var/db/sec.db",
            "10.0.0.2",
            "/dest",
            "0",
        ],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(
        log.iter()
            .any(|l| l == "Snapshot::backupSqliteDatabase: Source database inaccessible\n")
    );
    assert!(
        log.iter().any(|l| l
            == "Snapshot::transferSecondary: Failed to backup the sqlite db '/var/db/sec.db'.\n")
    );
    assert!(runner.runs.is_empty());
    // Cleanup still happens — and its failures would not matter anyway.
    assert_eq!(files.unlinks, ["/tmp/transferdb-abc123/sec.db"]);
    assert_eq!(files.rmdirs, ["/tmp/transferdb-abc123"]);
}

#[test]
fn a_destination_sqlite_failure_is_verbatim() {
    let mut files = FilesDouble::default();
    files.temp_dir = "/tmp/transferdb-abc123".to_string();
    files.backup_result = Err(SqliteBackupError::DestinationInaccessible);
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "snapshotsecondary",
            "/var/db/sec.db",
            "10.0.0.2",
            "/dest",
            "0",
        ],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(
        log.iter()
            .any(|l| l == "Snapshot::backupSqliteDatabase: Destination database inaccessible\n")
    );
}

#[test]
fn a_backup_failure_has_no_source_line() {
    let mut files = FilesDouble::default();
    files.temp_dir = "/tmp/transferdb-abc123".to_string();
    files.backup_result = Err(SqliteBackupError::BackupFailed);
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "snapshotsecondary",
            "/var/db/sec.db",
            "10.0.0.2",
            "/dest",
            "0",
        ],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    // pDestDB->backup's own false returns silently; the caller still reports.
    assert!(!log.iter().any(|l| l.contains("backupSqliteDatabase")));
    assert!(
        log.iter().any(|l| l
            == "Snapshot::transferSecondary: Failed to backup the sqlite db '/var/db/sec.db'.\n")
    );
}

#[test]
fn a_failing_secondary_transfer_still_cleans_up() {
    let mut files = FilesDouble::default();
    files.temp_dir = "/tmp/transferdb-abc123".to_string();
    let mut runner = RunnerDouble::default();
    runner.outcomes = VecDeque::from([fail_run()]);
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "snapshotsecondary",
            "/var/db/sec.db",
            "10.0.0.2",
            "/dest",
            "0",
        ],
        &mut InfraDouble::default(),
        &mut runner,
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(log.iter().any(|l| l == "Snapshot::transferSecondary: Failed to transfer successful secondary snapshot back to the requesting host.\n"));
    assert_eq!(files.unlinks, ["/tmp/transferdb-abc123/sec.db"]);
    assert_eq!(files.rmdirs, ["/tmp/transferdb-abc123"]);
}

#[test]
fn secondary_cleanup_failures_are_not_failures() {
    let mut files = FilesDouble::default();
    files.temp_dir = "/tmp/transferdb-abc123".to_string();
    files.unlink_err_for = vec!["/tmp/transferdb-abc123/sec.db".to_string()];
    files.rmdir_err_for = vec!["/tmp/transferdb-abc123".to_string()];
    let mut log = Vec::new();
    let code = run_main(
        &[
            "transfer_db",
            "snapshotsecondary",
            "/var/db/sec.db",
            "10.0.0.2",
            "/dest",
            "0",
        ],
        &mut InfraDouble::default(),
        &mut RunnerDouble::default(),
        &mut files,
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    assert!(log.iter().any(|l| l == "Snapshot::transferSecondary: Unable to remove secondary db snapshot '/tmp/transferdb-abc123/sec.db': No such file or directory\n"));
    assert!(log.iter().any(|l| l == "Snapshot::transferSecondary: Unable to remove temporary directory '/tmp/transferdb-abc123': Directory not empty\n"));
    assert!(log
        .iter()
        .any(|l| l == "TransferDB::snapshotSecondary: Completed transfer of '/var/db/sec.db'\n"));
}

// -----------------------------------------------------------------------------
// helpers, seams and constants
// -----------------------------------------------------------------------------

#[test]
fn the_snapshot_path_extraction_reads_the_stdout_tail() {
    let mut log = Vec::new();
    assert_eq!(
        snapshot_path_from_stdout("a\nb\n/mnt/snap\n", &mut log),
        Some("/mnt/snap".to_string())
    );
    assert!(log.is_empty());
}

#[test]
fn basename_matches_libgen() {
    assert_eq!(basename("/a/b"), "b");
    assert_eq!(basename("/a/b/"), "b");
    assert_eq!(basename("/"), "/");
    assert_eq!(basename("x"), "x");
    assert_eq!(basename(""), "");
}

#[test]
fn the_signal_processor_disarms_when_a_child_dispatches() {
    let mut files = FilesDouble::default();
    files.temp_dir = "/tmp/transferdb-abc123".to_string();
    let mut runner = RunnerDouble::default();
    let mut config = ConfigDouble::default();
    let mut consolidator = ConsolidatorDouble::default();
    let mut log = Vec::new();
    let mut snapshot = Snapshot::default();
    snapshot.init("10.0.0.2", "/dest", "0");
    assert!(snapshot.is_signal_processor_armed());
    assert_eq!(snapshot.destination(), "10.0.0.2:/dest");
    let mut infra = InfraDouble::default();
    let mut faces = Faces {
        logger: &mut infra,
        children: &mut runner,
        files: &mut files,
        db_config: &mut config,
        consolidator: &mut consolidator,
    };
    assert!(snapshot.transfer_secondary(&mut faces, &mut log, "/var/db/sec.db"));
    // The child-side hook dropped the parent's signal processor.
    assert!(!snapshot.is_signal_processor_armed());
}

#[test]
fn a_quiet_init_lowercases_the_console_threshold() {
    let mut infra = InfraDouble::default();
    let mut log = Vec::new();
    assert!(init_logger(&mut infra, APP_NAME, false, &mut log));
    assert!(infra.threshold_set);
    assert_eq!(infra.forwarders, [(APP_NAME.to_string(), String::new())]);
    assert!(log.is_empty());
}

#[test]
fn the_console_flag_is_set_before_init_and_reaffirmed() {
    let mut infra = InfraDouble::default();
    let mut log = Vec::new();
    let code = run_main(
        &["transfer_db"],
        &mut infra,
        &mut RunnerDouble::default(),
        &mut FilesDouble::default(),
        &mut ConfigDouble::default(),
        &mut ConsolidatorDouble::default(),
        &mut log,
    );
    assert_eq!(code, EXIT_SUCCESS);
    // main's own shouldWriteToConsole, then initLogger's unconditional one,
    // then the verbose branch's repeat.
    assert_eq!(infra.console, [true, true, true]);
}

#[test]
fn atoi_c_matches_the_c_semantics() {
    assert_eq!(atoi_c(""), 0);
    assert_eq!(atoi_c("abc"), 0);
    assert_eq!(atoi_c("300"), 300);
    assert_eq!(atoi_c("-5"), -5);
    assert_eq!(atoi_c("+7"), 7);
    assert_eq!(atoi_c(" 42junk"), 42);
    assert_eq!(atoi_c("999999999999"), i32::MAX);
}

#[test]
fn convert_address_packs_the_dotted_quad_in_network_order() {
    assert_eq!(convert_address("1.2.3.4"), Some(0x0403_0201));
    assert_eq!(convert_address("0.0.0.0"), Some(0));
    assert_eq!(convert_address("nohost"), None);
    assert_eq!(convert_address("1.2.3"), None);
    assert_eq!(convert_address("1.2.3.4.5"), None);
    assert_eq!(convert_address("256.1.1.1"), None);
    assert_eq!(convert_address("1.2.3."), None);
}

#[test]
fn the_constants_match_the_reference() {
    assert_eq!(EXIT_SUCCESS, 0);
    assert_eq!(EXIT_FAILURE, 1);
    assert_eq!(APP_NAME, "TransferDB");
    assert_eq!(SNAPSHOT_HELPER, "snapshot_helper");
    assert_eq!(RSYNC, "/usr/bin/rsync");
    assert_eq!(TEMP_DIR_TEMPLATE, "/tmp/transferdb-XXXXXX");
    // Both success markers are the same zero, by two names.
    assert_eq!(SNAPSHOT_HELPER_SUCCESS, EXIT_SUCCESS);
}
