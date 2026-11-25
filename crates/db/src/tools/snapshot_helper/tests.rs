//! Command-log pins for the snapshot helper port. The doubles record the
//! privileged calls; the reviewable artifact is that log plus stdout.

use super::*;
use std::collections::BTreeMap;

/// The `bigworld.conf` double.
#[derive(Default)]
struct ConfigDouble {
    values: BTreeMap<String, String>,
    read_ok: bool,
}

impl ConfigReaderFace for ConfigDouble {
    fn read(&mut self) -> bool {
        self.read_ok
    }

    fn get_value(&self, section: &str, key: &str) -> Option<String> {
        if section == CONFIG_SECTION {
            self.values.get(key).cloned()
        } else {
            None
        }
    }
}

/// The setuid/MySQL/execl double: canned failures, recorded calls.
#[derive(Default)]
struct SystemDouble {
    root_ok: bool,
    errno_text: String,
    init_ok: bool,
    connect_err: Option<String>,
    query_err_for: Vec<String>,
    exec_err_for: Vec<String>,
    execs: Vec<(String, Vec<String>)>,
    queries: Vec<String>,
    closed: usize,
}

impl SnapshotSystem for SystemDouble {
    fn become_root(&mut self) -> Result<(), String> {
        if self.root_ok {
            Ok(())
        } else {
            Err(self.errno_text.clone())
        }
    }

    fn mysql_init(&mut self) -> Result<(), String> {
        if self.init_ok {
            Ok(())
        } else {
            Err(String::new())
        }
    }

    fn mysql_connect(&mut self, _user: &str, _pass: &str) -> Result<(), String> {
        match self.connect_err.clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn mysql_query(&mut self, statement: &str) -> Result<(), String> {
        if self.query_err_for.iter().any(|s| s == statement) {
            Err("query error text".to_string())
        } else {
            self.queries.push(statement.to_string());
            Ok(())
        }
    }

    fn mysql_close(&mut self) {
        self.closed += 1
    }

    fn exec(&mut self, path: &str, argv: &[String]) -> Result<(), String> {
        if self.exec_err_for.iter().any(|s| s == path) {
            Err("exec failed".to_string())
        } else {
            self.execs.push((path.to_string(), argv.to_vec()));
            Ok(())
        }
    }
}

/// A fully working environment: config complete, setuid effective, MySQL up.
fn working() -> (ConfigDouble, SystemDouble) {
    (
        ConfigDouble {
            values: snapshot_config("/var/lib/mysql", "vg0", "origin0", "snap0", "5"),
            read_ok: true,
        },
        SystemDouble {
            root_ok: true,
            errno_text: "Operation not permitted".to_string(),
            init_ok: true,
            ..Default::default()
        },
    )
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// -----------------------------------------------------------------------------
// main face
// -----------------------------------------------------------------------------

#[test]
fn the_banner_is_always_printed_first() {
    let (mut config, mut system) = working();
    let mut log = Vec::new();
    let _ = main_face(&args(&[]), &mut config, &mut system, &mut log);
    assert_eq!(log[0], "SnapshotHelper started\n");
}

#[test]
fn no_arguments_tests_the_setuid_bit() {
    let (mut config, mut system) = working();
    let mut log = Vec::new();
    assert_eq!(
        main_face(&args(&[]), &mut config, &mut system, &mut log),
        SUCCESS
    );

    let (mut config, mut system) = working();
    system.root_ok = false;
    let mut log = Vec::new();
    assert_eq!(
        main_face(&args(&[]), &mut config, &mut system, &mut log),
        FAILURE
    );
    assert!(
        log.iter()
            .any(|l| l == "setuid root failed: Operation not permitted\n")
    );
}

#[test]
fn an_unreadable_config_file_is_a_failure() {
    let (mut config, mut system) = working();
    config.read_ok = false;
    let mut log = Vec::new();
    assert_eq!(
        main_face(
            &args(&["snapshot_helper", "release-snapshot"]),
            &mut config,
            &mut system,
            &mut log
        ),
        FAILURE
    );
    assert!(log.iter().any(|l| l == "Unable to read config file.\n"));
}

#[test]
fn every_missing_config_value_prints_its_own_line() {
    let (mut config, mut system) = working();
    config.values.remove("lvsizegb");
    let mut log = Vec::new();
    assert_eq!(
        main_face(
            &args(&["snapshot_helper", "release-snapshot"]),
            &mut config,
            &mut system,
            &mut log
        ),
        FAILURE
    );
    assert!(
        log.iter()
            .any(|l| l == "Unable to read config value for 'lvsizegb'\n")
    );
}

#[test]
fn an_unknown_command_is_a_failure() {
    let (mut config, mut system) = working();
    let mut log = Vec::new();
    assert_eq!(
        main_face(
            &args(&["snapshot_helper", "frobnicate"]),
            &mut config,
            &mut system,
            &mut log
        ),
        FAILURE
    );
    assert!(log.iter().any(|l| l == "Unknown arguments provided.\n"));
}

#[test]
fn the_acquire_arity_complaint_prints_but_does_not_stop() {
    // The reference prints the complaint and then keeps going into
    // acquire_snapshot regardless.
    let (mut config, mut system) = working();
    let mut log = Vec::new();
    assert_eq!(
        main_face(
            &args(&["snapshot_helper", "acquire-snapshot"]),
            &mut config,
            &mut system,
            &mut log
        ),
        SUCCESS
    );
    assert!(
        log.iter()
            .any(|l| l == "Invalid argument list for 'acquire-snapshot'\n")
    );
    assert_eq!(system.execs.len(), 3);

    // The well-formed call prints no complaint.
    let (mut config, mut system) = working();
    let mut log = Vec::new();
    assert_eq!(
        main_face(
            &args(&["snapshot_helper", "acquire-snapshot", "bw", "secret"]),
            &mut config,
            &mut system,
            &mut log
        ),
        SUCCESS
    );
    assert!(!log.iter().any(|l| l.starts_with("Invalid argument list")));
}

#[test]
fn the_release_arity_complaint_prints_but_does_not_stop() {
    let (mut config, mut system) = working();
    let mut log = Vec::new();
    assert_eq!(
        main_face(
            &args(&["snapshot_helper", "release-snapshot", "extra"]),
            &mut config,
            &mut system,
            &mut log
        ),
        SUCCESS
    );
    assert!(
        log.iter()
            .any(|l| l == "Invalid argument list for 'release-snapshot'\n")
    );
    assert_eq!(system.execs.len(), 2);
}

// -----------------------------------------------------------------------------
// acquire_snapshot
// -----------------------------------------------------------------------------

#[test]
fn the_acquire_happy_path_records_the_commands() {
    let (config, mut system) = working();
    let mut log = Vec::new();
    assert!(acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    // The chmod target keeps the reference's double slash: "/mnt/" + snapshot
    // + "/" + a leading-slash datadir.
    assert_eq!(
        system.execs,
        [
            (
                LVCREATE.to_string(),
                vec![
                    "-L5G".to_string(),
                    "-s".to_string(),
                    "-n".to_string(),
                    "snap0".to_string(),
                    "/dev/vg0/origin0".to_string(),
                ],
            ),
            (
                "mount".to_string(),
                vec!["/dev/vg0/snap0".to_string(), "/mnt/snap0/".to_string()],
            ),
            (
                "chmod".to_string(),
                vec![
                    "-R".to_string(),
                    "755".to_string(),
                    "/mnt/snap0//var/lib/mysql".to_string(),
                ],
            ),
        ]
    );
    assert_eq!(
        system.queries,
        ["FLUSH TABLES WITH READ LOCK", "UNLOCK TABLES"]
    );
    assert_eq!(system.closed, 1);
    assert_eq!(
        log.last().map(String::as_str),
        Some("/mnt/snap0//var/lib/mysql\n")
    );
}

#[test]
fn the_execl_argv0_slot_holds_the_first_option() {
    // execl's first variadic argument lands in argv[0]: lvcreate sees
    // argv[0] == "-L5G", not a program name.
    let (config, mut system) = working();
    let mut log = Vec::new();
    let _ = acquire_snapshot(&config, &mut system, "bw", "secret", &mut log);
    assert_eq!(system.execs[0].1[0], "-L5G");
}

#[test]
fn acquire_stops_when_setuid_fails() {
    let (config, mut system) = working();
    system.root_ok = false;
    let mut log = Vec::new();
    assert!(!acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    assert!(
        log.iter()
            .any(|l| l == "setuid root failed: Operation not permitted\n")
    );
    assert!(system.queries.is_empty());
    assert!(system.execs.is_empty());
}

#[test]
fn release_stops_when_setuid_fails() {
    let (config, mut system) = working();
    system.root_ok = false;
    let mut log = Vec::new();
    assert!(!release_snapshot(&config, &mut system, &mut log));
    assert!(
        log.iter()
            .any(|l| l == "setuid root failed: Operation not permitted\n")
    );
    assert!(system.execs.is_empty());
}

#[test]
fn a_failed_acquire_is_a_main_failure() {
    let (mut config, mut system) = working();
    system.connect_err = Some("Access denied".to_string());
    let mut log = Vec::new();
    assert_eq!(
        main_face(
            &args(&["snapshot_helper", "acquire-snapshot", "bw", "secret"]),
            &mut config,
            &mut system,
            &mut log
        ),
        FAILURE
    );
    assert!(
        log.iter()
            .any(|l| l == "Failed to connect to MySQL: Access denied\n")
    );
}

#[test]
fn a_failed_release_is_a_main_failure() {
    let (mut config, mut system) = working();
    system.exec_err_for = vec![LVREMOVE.to_string()];
    let mut log = Vec::new();
    assert_eq!(
        main_face(
            &args(&["snapshot_helper", "release-snapshot"]),
            &mut config,
            &mut system,
            &mut log
        ),
        FAILURE
    );
    assert!(log.iter().any(|l| l == "Unable to 'lvremove' snapshot.\n"));
}

#[test]
fn the_original_reader_takes_the_path_between_the_last_two_newlines() {
    // transfer_db reads the helper's stdout and takes the path from between
    // the last two newlines.
    let (config, mut system) = working();
    let mut log = Vec::new();
    let _ = acquire_snapshot(&config, &mut system, "bw", "secret", &mut log);
    let stdout = log.join("");
    assert_eq!(
        path_between_last_two_newlines(&stdout),
        "/mnt/snap0//var/lib/mysql"
    );
}

#[test]
fn the_reader_falls_back_without_a_trailing_newline() {
    assert_eq!(path_between_last_two_newlines("path only"), "path only");
    assert_eq!(path_between_last_two_newlines(""), "");
}

#[test]
fn a_mysql_init_failure_is_reported_and_stops() {
    let (config, mut system) = working();
    system.init_ok = false;
    let mut log = Vec::new();
    assert!(!acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    assert!(log.iter().any(|l| l == "Unable to initialise MySQL\n"));
    assert!(system.queries.is_empty());
    assert_eq!(system.execs.len(), 0);
}

#[test]
fn a_connect_failure_carries_the_mysql_error() {
    let (config, mut system) = working();
    system.connect_err = Some("Access denied for user 'bw'@'localhost'".to_string());
    let mut log = Vec::new();
    assert!(!acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    assert!(
        log.iter()
            .any(|l| l == "Failed to connect to MySQL: Access denied for user 'bw'@'localhost'\n")
    );
}

#[test]
fn a_read_lock_failure_is_reported_and_stops() {
    let (config, mut system) = working();
    system.query_err_for = vec!["FLUSH TABLES WITH READ LOCK".to_string()];
    let mut log = Vec::new();
    assert!(!acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    assert!(
        log.iter()
            .any(|l| l == "Failed to apply read lock MySQL: query error text\n")
    );
    assert!(system.execs.is_empty());
}

#[test]
fn an_lvcreate_failure_closes_the_connection_and_stops() {
    let (config, mut system) = working();
    system.exec_err_for = vec![LVCREATE.to_string()];
    let mut log = Vec::new();
    assert!(!acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    assert!(
        log.iter()
            .any(|l| l == "Failed to 'lvcreate' LVM snapshot.\n")
    );
    // The reference closes the MySQL connection only on this path and the
    // unlock path.
    assert_eq!(system.closed, 1);
    assert!(!system.queries.iter().any(|s| s == "UNLOCK TABLES"));
}

#[test]
fn an_unlock_failure_closes_the_connection_and_stops() {
    let (config, mut system) = working();
    system.query_err_for = vec!["UNLOCK TABLES".to_string()];
    let mut log = Vec::new();
    assert!(!acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    assert!(
        log.iter()
            .any(|l| l == "Failed to unlock MySQL: query error text\n")
    );
    assert_eq!(system.closed, 1);
    assert_eq!(system.execs.len(), 1); // lvcreate succeeded, mount never ran
}

#[test]
fn a_mount_failure_stops_before_chmod() {
    let (config, mut system) = working();
    system.exec_err_for = vec!["mount".to_string()];
    let mut log = Vec::new();
    assert!(!acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    assert!(
        log.iter()
            .any(|l| l == "Failed to mount newly created snapshot.\n")
    );
    assert_eq!(system.execs.len(), 1);
    assert!(!log.iter().any(|l| l.ends_with("/var/lib/mysql\n")));
}

#[test]
fn a_chmod_failure_keeps_the_file_dot_s_typo() {
    let (config, mut system) = working();
    system.exec_err_for = vec!["chmod".to_string()];
    let mut log = Vec::new();
    assert!(!acquire_snapshot(
        &config,
        &mut system,
        "bw",
        "secret",
        &mut log
    ));
    // "file.s" — the reference's typo, kept verbatim.
    assert!(
        log.iter()
            .any(|l| l == "Failed to chmod the snapshot file.s\n")
    );
    assert!(!log.iter().any(|l| l.ends_with("/var/lib/mysql\n")));
}

// -----------------------------------------------------------------------------
// release_snapshot
// -----------------------------------------------------------------------------

#[test]
fn the_release_happy_path_unmounts_then_removes_the_volume() {
    let (config, mut system) = working();
    let mut log = Vec::new();
    assert!(release_snapshot(&config, &mut system, &mut log));
    assert_eq!(
        system.execs,
        [
            ("umount".to_string(), vec!["/mnt/snap0/".to_string()]),
            (
                LVREMOVE.to_string(),
                vec!["-f".to_string(), "/dev/vg0/snap0".to_string()],
            ),
        ]
    );
    assert!(log.is_empty());
}

#[test]
fn a_failed_umount_does_not_stop_the_lvremove() {
    let (config, mut system) = working();
    system.exec_err_for = vec!["umount".to_string()];
    let mut log = Vec::new();
    assert!(!release_snapshot(&config, &mut system, &mut log));
    assert!(log.iter().any(|l| l == "Unable to unmount snapshot.\n"));
    assert_eq!(system.execs.len(), 1); // only the lvremove call was recorded
    assert!(!log.iter().any(|l| l.contains("lvremove")));
}

#[test]
fn a_failed_lvremove_is_reported() {
    let (config, mut system) = working();
    system.exec_err_for = vec![LVREMOVE.to_string()];
    let mut log = Vec::new();
    assert!(!release_snapshot(&config, &mut system, &mut log));
    assert!(log.iter().any(|l| l == "Unable to 'lvremove' snapshot.\n"));
}

// -----------------------------------------------------------------------------
// constants and helpers
// -----------------------------------------------------------------------------

#[test]
fn the_constants_match_the_reference() {
    assert_eq!(CONFIG_SECTION, "snapshot");
    assert_eq!(
        CONFIG_VALUES,
        ["datadir", "lvgroup", "lvorigin", "lvsnapshot", "lvsizegb"]
    );
    assert_eq!(LVCREATE, "/usr/sbin/lvcreate");
    assert_eq!(LVREMOVE, "/usr/sbin/lvremove");
    assert_eq!(SUCCESS, 0);
    assert_eq!(FAILURE, 1);
}

#[test]
fn the_config_helper_builds_all_five_values() {
    let config = snapshot_config("/var/lib/mysql", "vg0", "origin0", "snap0", "5");
    for key in CONFIG_VALUES {
        assert_eq!(config.get(key).map(String::as_str), Some(key_value(key)));
    }
}

fn key_value(key: &str) -> &'static str {
    match key {
        "datadir" => "/var/lib/mysql",
        "lvgroup" => "vg0",
        "lvorigin" => "origin0",
        "lvsnapshot" => "snap0",
        "lvsizegb" => "5",
        _ => unreachable!(),
    }
}
