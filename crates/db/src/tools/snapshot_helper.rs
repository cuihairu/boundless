//! The BW-09-05-03 snapshot helper (`server/tools/snapshot_helper/`).
//!
//! The reference binary is a setuid helper the DBApp host runs to take an
//! LVM snapshot of the MySQL data directory: lock the tables, `lvcreate`,
//! unlock, mount, relax permissions, and print the snapshot files path for
//! `transfer_db` to read. The privileged faces (setuid, MySQL wire, execl)
//! stay behind the [`SnapshotSystem`] seam and the bigworld.conf face behind
//! [`ConfigReaderFace`]; the statement/command log a test double records is
//! the reviewable artifact.
//!
//! The reader contract (BW-09-05-04's transfer_db side): the snapshot files
//! path is what appears on stdout between the last two newlines, which is
//! exactly the final `printf("%s\n", snapshotFiles)` of `acquire_snapshot`.
//!
//! Honest modelling note: the reference's `execl` *replaces the process* on
//! success, so nothing after a successful exec runs there. This port models
//! exec success as "command dispatched, flow continues", because the reader
//! contract depends on the final path print being reachable; the recorded
//! exec vectors pin the reference's `argv[0]` quirk (the first *option* is
//! stuffed into the `argv[0]` slot).

use std::collections::BTreeMap;

/// `snapshot_constants.hpp`: values transfer_db must know.
pub const SUCCESS: i32 = 0; // EXIT_SUCCESS
pub const FAILURE: i32 = 1; // EXIT_FAILURE

/// The `bigworld.conf` section and the five values `validate_config` demands.
pub const CONFIG_SECTION: &str = "snapshot";
pub const CONFIG_VALUES: [&str; 5] = ["datadir", "lvgroup", "lvorigin", "lvsnapshot", "lvsizegb"];

/// `LVM_BIN_DIR "lvcreate"` / `"lvremove"`.
pub const LVCREATE: &str = "/usr/sbin/lvcreate";
pub const LVREMOVE: &str = "/usr/sbin/lvremove";

/// The config face: `ConfigReader( "/etc/bigworld.conf" )`.
pub trait ConfigReaderFace {
    /// `ConfigReader::read`.
    fn read(&mut self) -> bool;

    /// `ConfigReader::getValue(section, key)`; `None` when undefined.
    fn get_value(&self, section: &str, key: &str) -> Option<String>;
}

/// The privileged faces `snapshot_helper.cpp` reaches: setuid, the MySQL wire
/// and `execl`. `Err` text is what the reference prints (`strerror(errno)`,
/// `mysql_error()`) or the exec failure marker.
pub trait SnapshotSystem {
    /// `setuid( 0 )`; `Err` is the `strerror(errno)` text.
    fn become_root(&mut self) -> Result<(), String>;

    /// `mysql_init`; `Err` when it returns NULL.
    fn mysql_init(&mut self) -> Result<(), String>;

    /// `mysql_real_connect( ..., "localhost", user, pass, NULL, 0, NULL, 0 )`;
    /// `Err` is the `mysql_error()` text.
    fn mysql_connect(&mut self, user: &str, pass: &str) -> Result<(), String>;

    /// `mysql_real_query`; `Err` is the `mysql_error()` text.
    fn mysql_query(&mut self, statement: &str) -> Result<(), String>;

    /// `mysql_close`.
    fn mysql_close(&mut self);

    /// `execl( path, arg0, ..., NULL )`; `Ok` = dispatched (the port's
    /// flow-continues model), `Err` = execl returned.
    fn exec(&mut self, path: &str, argv: &[String]) -> Result<(), String>;
}

/// `becomeRootUser` (`snapshot_helper.cpp:297-306`).
pub fn become_root_user(system: &mut dyn SnapshotSystem, log: &mut Vec<String>) -> bool {
    if let Err(errno_text) = system.become_root() {
        log.push(format!("setuid root failed: {errno_text}\n"));
        return false;
    }
    true
}

/// `validate_config` (`snapshot_helper.cpp:115-138`): all five
/// [`CONFIG_VALUES`] must be defined under [`CONFIG_SECTION`]; every missing
/// value prints its own line.
pub fn validate_config(config: &dyn ConfigReaderFace, log: &mut Vec<String>) -> bool {
    let mut status = true;
    for value in CONFIG_VALUES {
        if config.get_value(CONFIG_SECTION, value).is_none() {
            log.push(format!("Unable to read config value for '{value}'\n"));
            status = false;
        }
    }
    status
}

fn config_value(config: &dyn ConfigReaderFace, key: &str) -> String {
    config.get_value(CONFIG_SECTION, key).unwrap_or_default()
}

/// `acquire_snapshot` (`snapshot_helper.cpp:142-247`). The `TODO: sanitise`
/// comment is the reference's own: the credentials come from the command
/// line.
pub fn acquire_snapshot(
    config: &dyn ConfigReaderFace,
    system: &mut dyn SnapshotSystem,
    db_user: &str,
    db_pass: &str,
    log: &mut Vec<String>,
) -> bool {
    // Elevate privileges for actual operations
    if !become_root_user(system, log) {
        return false;
    }

    let data_dir = config_value(config, "datadir");
    let lv_group = config_value(config, "lvgroup");
    let lv_origin = config_value(config, "lvorigin");
    let lv_snapshot = config_value(config, "lvsnapshot");
    let lv_size_gb = config_value(config, "lvsizegb");

    // Connect to the database and apply a read lock.
    if system.mysql_init().is_err() {
        log.push("Unable to initialise MySQL\n".to_string());
        return false;
    }
    if let Err(error) = system.mysql_connect(db_user, db_pass) {
        log.push(format!("Failed to connect to MySQL: {error}\n"));
        return false;
    }
    if let Err(error) = system.mysql_query("FLUSH TABLES WITH READ LOCK") {
        log.push(format!("Failed to apply read lock MySQL: {error}\n"));
        return false;
    }

    // Now create an LVM snapshot. `execl`'s first variadic argument lands in
    // the argv[0] slot, so the first option goes there (reference quirk).
    let lvcreate_argv = vec![
        format!("-L{lv_size_gb}G"),
        "-s".to_string(),
        "-n".to_string(),
        lv_snapshot.clone(),
        format!("/dev/{lv_group}/{lv_origin}"),
    ];
    if system.exec(LVCREATE, &lvcreate_argv).is_err() {
        log.push("Failed to 'lvcreate' LVM snapshot.\n".to_string());
        system.mysql_close();
        return false;
    }

    if let Err(error) = system.mysql_query("UNLOCK TABLES") {
        log.push(format!("Failed to unlock MySQL: {error}\n"));
        system.mysql_close();
        return false;
    }

    // The MySQL connection is no longer needed
    system.mysql_close();

    if system
        .exec(
            "mount",
            &[
                format!("/dev/{lv_group}/{lv_snapshot}"),
                format!("/mnt/{lv_snapshot}/"),
            ],
        )
        .is_err()
    {
        log.push("Failed to mount newly created snapshot.\n".to_string());
        return false;
    }

    let snapshot_files = format!("/mnt/{lv_snapshot}/{data_dir}");

    // Relax permissions so we can take ownership of the backup files, this
    // makes sending and consolidating easier on the snapshot machine. The
    // failure message keeps the reference's "file.s" typo verbatim.
    if system
        .exec(
            "chmod",
            &["-R".to_string(), "755".to_string(), snapshot_files.clone()],
        )
        .is_err()
    {
        log.push("Failed to chmod the snapshot file.s\n".to_string());
        return false;
    }

    // The reader contract: transfer_db takes the path between the last two
    // newlines of stdout.
    log.push(format!("{snapshot_files}\n"));

    true
}

/// `release_snapshot` (`snapshot_helper.cpp:254-290`): unmount, then remove
/// the snapshot volume. Both steps run even when the first fails; the return
/// is false if either did. (The 3.13 migration note in the reference: the
/// lvremove call used to drop the "/dev/" + lvGroup concatenation on the
/// floor through a comma expression — the fixed form passes the full path.)
pub fn release_snapshot(
    config: &dyn ConfigReaderFace,
    system: &mut dyn SnapshotSystem,
    log: &mut Vec<String>,
) -> bool {
    // Elevate privileges for actual operations
    if !become_root_user(system, log) {
        return false;
    }

    let lv_group = config_value(config, "lvgroup");
    let lv_snapshot = config_value(config, "lvsnapshot");

    let mut is_ok = true;

    if system
        .exec("umount", &[format!("/mnt/{lv_snapshot}/")])
        .is_err()
    {
        log.push("Unable to unmount snapshot.\n".to_string());
        is_ok = false;
    }

    if system
        .exec(
            LVREMOVE,
            &["-f".to_string(), format!("/dev/{lv_group}/{lv_snapshot}")],
        )
        .is_err()
    {
        log.push("Unable to 'lvremove' snapshot.\n".to_string());
        is_ok = false;
    }

    is_ok
}

/// `main` (`snapshot_helper.cpp:51-108`), argument list and exit codes.
pub fn main_face(
    args: &[String],
    config: &mut dyn ConfigReaderFace,
    system: &mut dyn SnapshotSystem,
    log: &mut Vec<String>,
) -> i32 {
    log.push("SnapshotHelper started\n".to_string());

    // If no arguments are provided, test if setuid attribute is set
    if args.len() <= 1 {
        return if become_root_user(system, log) {
            SUCCESS
        } else {
            FAILURE
        };
    }

    // Read the configuration file and confirm all appropriate values exist
    if !config.read() {
        log.push("Unable to read config file.\n".to_string());
        return FAILURE;
    }

    if !validate_config(&*config, log) {
        return FAILURE;
    }

    // Execute commands
    if args[1] == "acquire-snapshot" {
        // The reference prints the arity complaint but does *not* return:
        // acquire_snapshot still runs (only-print, no-return quirk).
        if args.len() != 4 {
            log.push("Invalid argument list for 'acquire-snapshot'\n".to_string());
        }
        let db_user = args.get(2).map(String::as_str).unwrap_or_default();
        let db_pass = args.get(3).map(String::as_str).unwrap_or_default();
        if !acquire_snapshot(&*config, system, db_user, db_pass, log) {
            return FAILURE;
        }
    } else if args[1] == "release-snapshot" {
        if args.len() != 2 {
            log.push("Invalid argument list for 'release-snapshot'\n".to_string());
        }
        if !release_snapshot(&*config, system, log) {
            return FAILURE;
        }
    } else {
        log.push("Unknown arguments provided.\n".to_string());
        return FAILURE;
    }

    SUCCESS
}

/// The stdout/stderr face the reader consumes: take the text between the last
/// two newlines (how transfer_db extracts the snapshot files path).
pub fn path_between_last_two_newlines(stdout: &str) -> &str {
    stdout
        .strip_suffix('\n')
        .unwrap_or(stdout)
        .rsplit('\n')
        .next()
        .unwrap_or_default()
}

/// The five `snapshot` values as a ready-made map, for doubles and callers.
pub fn snapshot_config(
    data_dir: &str,
    lv_group: &str,
    lv_origin: &str,
    lv_snapshot: &str,
    lv_size_gb: &str,
) -> BTreeMap<String, String> {
    [
        ("datadir", data_dir),
        ("lvgroup", lv_group),
        ("lvorigin", lv_origin),
        ("lvsnapshot", lv_snapshot),
        ("lvsizegb", lv_size_gb),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[cfg(test)]
mod tests;
