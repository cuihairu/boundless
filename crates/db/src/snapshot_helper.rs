//! The `snapshot_helper` tool (`server/tools/snapshot_helper/snapshot_helper.cpp`).
//!
//! The helper is a setuid-root utility the `transfer_db` tool runs on the
//! database host: it takes an LVM snapshot of the MySQL data directory while
//! the tables carry a read lock, mounts it, and prints the snapshot path on
//! stdout. The original reader of that stdout is
//! `Snapshot::takePrimarySnapshot` (`transfer_db/snapshot.cpp:282-307`), which
//! finds the path between the last two newlines of the stream — the contract
//! [`parse_snapshot_path`] encodes, so an original-style reader reads the
//! ported helper's result.
//!
//! The tool shells out to LVM and mount through `execl`, so the port hands
//! those invocations to the caller's [`SnapshotOps`] implementation — the
//! same seam the dry-run wrapper uses for its reads. Two reference quirks
//! are kept verbatim: the invalid-arity messages fall through instead of
//! returning (`snapshot_helper.cpp:80-83,92-95`), and `execl`'s `argv[0]`
//! slot carries the first real option (lvcreate is invoked as
//! `lvcreate -L2G -s -n <snap> /dev/<group>/<origin>` with no program
//! name). Each printed line lands in the output sink without its newline.

use std::collections::BTreeMap;

/// `SnapshotHelper::SUCCESS` (`snapshot_constants.hpp`: `EXIT_SUCCESS`).
pub const SUCCESS: i32 = 0;
/// `SnapshotHelper::FAILURE` (`EXIT_FAILURE`).
pub const FAILURE: i32 = 1;

/// `CONFIG_SECTION` (`snapshot_helper.cpp:37`).
pub const CONFIG_SECTION: &str = "snapshot";
/// `LVCREATE` (`snapshot_helper.cpp:40`).
pub const LVCREATE: &str = "/usr/sbin/lvcreate";
/// `LVREMOVE` (`snapshot_helper.cpp:42`).
pub const LVREMOVE: &str = "/usr/sbin/lvremove";

/// The five `[snapshot]` values `validate_config` demands
/// (`snapshot_helper.cpp:125-128`).
const CONFIG_VALUES: [&str; 5] = ["datadir", "lvgroup", "lvorigin", "lvsnapshot", "lvsizegb"];

/// The `ConfigReader` face the helper sees: the config file either loads or
/// it does not, and a section value either exists or it does not.
pub trait SnapshotConfigReader {
    /// `ConfigReader::read` — the whole `/etc/bigworld.conf` load.
    fn read(&self) -> bool;
    /// `ConfigReader::getValue(section, key, out)` — `None` when unset.
    fn value(&self, section: &str, key: &str) -> Option<String>;
}

/// The validated `[snapshot]` section, read back once per operation the way
/// `acquire_snapshot`/`release_snapshot` re-read their values.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SnapshotConfig {
    /// Where the MySQL data directory lives inside the snapshot mount.
    pub datadir: String,
    /// The LVM volume group.
    pub lvgroup: String,
    /// The origin logical volume.
    pub lvorigin: String,
    /// The snapshot logical volume's name.
    pub lvsnapshot: String,
    /// The snapshot size, in GB (feeds lvcreate's `-L<size>G`).
    pub lvsizegb: String,
}

impl SnapshotConfig {
    /// Collect the five validated values into the struct
    /// (`acquire_snapshot`/`release_snapshot` re-read them per operation in
    /// the reference; the ported config carries them).
    fn from_values(values: &BTreeMap<String, String>) -> Self {
        let get = |key: &str| values.get(key).cloned().unwrap_or_default();
        Self {
            datadir: get("datadir"),
            lvgroup: get("lvgroup"),
            lvorigin: get("lvorigin"),
            lvsnapshot: get("lvsnapshot"),
            lvsizegb: get("lvsizegb"),
        }
    }

    /// `"/dev/<lvgroup>/<lvorigin>"` — the lvcreate origin path.
    pub fn origin_path(&self) -> String {
        format!("/dev/{}/{}", self.lvgroup, self.lvorigin)
    }

    /// `"/dev/<lvgroup>/<lvsnapshot>"` — the snapshot device.
    pub fn snapshot_device(&self) -> String {
        format!("/dev/{}/{}", self.lvgroup, self.lvsnapshot)
    }

    /// `"/mnt/<lvsnapshot>/"` — the mount point.
    pub fn mount_point(&self) -> String {
        format!("/mnt/{}/", self.lvsnapshot)
    }

    /// `"/mnt/<lvsnapshot>/<datadir>"` — the path printed on success, which
    /// the reader hands to rsync.
    pub fn snapshot_files_path(&self) -> String {
        format!("/mnt/{}/{}", self.lvsnapshot, self.datadir)
    }
}

/// `validate_config` (`snapshot_helper.cpp:115-139`): every one of the five
/// values must be present; each missing one prints its own complaint. The
/// validated config comes back only when all five are there.
pub fn validate_config(
    reader: &dyn SnapshotConfigReader,
    out: &mut Vec<String>,
) -> Option<SnapshotConfig> {
    let mut values = BTreeMap::new();
    let mut status = true;
    for key in CONFIG_VALUES {
        match reader.value(CONFIG_SECTION, key) {
            Some(value) => {
                values.insert(key.to_owned(), value);
            }
            None => {
                out.push(format!("Unable to read config value for '{key}'"));
                status = false;
            }
        }
    }
    status.then(|| SnapshotConfig::from_values(&values))
}

/// `becomeRootUser` (`snapshot_helper.cpp:297-305`): the reference prints the
/// complaint itself before returning false, so every call site shares it.
fn become_root(ops: &mut dyn SnapshotOps, out: &mut Vec<String>) -> bool {
    match ops.become_root() {
        Ok(()) => true,
        Err(errno) => {
            out.push(format!("setuid root failed: {errno}"));
            false
        }
    }
}

/// Why the MySQL face of [`SnapshotOps`] refused. `mysql_init` failing and
/// `mysql_real_connect` failing print different lines
/// (`snapshot_helper.cpp:171-184`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectFailure {
    /// `mysql_init` returned NULL — no MySQL handle exists to name an error.
    Init,
    /// `mysql_real_connect` failed; the string is `mysql_error`.
    Connect(String),
}

/// The privileged operations the helper shells out to. Every `exec`-shaped
/// method mirrors an `execl`: it returns whether the exec *failed* (a
/// successful exec never returns, so `!= 0` is the only outcome the caller
/// can observe) and receives the child's full argv including the `argv[0]`
/// slot the reference fills with the first option.
pub trait SnapshotOps {
    /// `setuid(0)` (`snapshot_helper.cpp:297-305`); `Err` is the
    /// `strerror(errno)` text.
    fn become_root(&mut self) -> Result<(), String>;
    /// `mysql_init` + `mysql_real_connect("localhost", user, pass, ...)`
    /// (`snapshot_helper.cpp:163-184`).
    fn connect(&mut self, user: &str, pass: &str) -> Result<(), ConnectFailure>;
    /// `FLUSH TABLES WITH READ LOCK`; `Err` is the `mysql_error` text.
    fn lock_tables(&mut self) -> Result<(), String>;
    /// `UNLOCK TABLES`; `Err` is the `mysql_error` text.
    fn unlock_tables(&mut self) -> Result<(), String>;
    /// One `execl`: `command` plus the child argv (including the `argv[0]`
    /// slot). True when the exec failed.
    fn exec_failed(&mut self, command: &str, args: &[String]) -> bool;
}

/// `acquire_snapshot` (`snapshot_helper.cpp:142-249`): lock the tables,
/// lvcreate the snapshot, unlock, mount, relax the permissions, print the
/// path. Once lvcreate has succeeded the snapshot exists — the reference's
/// TODO about lvremove-on-failure is left unimplemented, so a later failure
/// leaves the snapshot behind rather than rolling back.
pub fn acquire_snapshot(
    config: &SnapshotConfig,
    db_user: &str,
    db_pass: &str,
    ops: &mut dyn SnapshotOps,
    out: &mut Vec<String>,
) -> bool {
    if !become_root(ops, out) {
        return false;
    }

    if let Err(failure) = ops.connect(db_user, db_pass) {
        match failure {
            ConnectFailure::Init => out.push("Unable to initialise MySQL".to_owned()),
            ConnectFailure::Connect(error) => {
                out.push(format!("Failed to connect to MySQL: {error}"))
            }
        }
        return false;
    }

    if let Err(error) = ops.lock_tables() {
        out.push(format!("Failed to apply read lock MySQL: {error}"));
        return false;
    }

    let lvcreate_args = vec![
        format!("-L{}G", config.lvsizegb),
        "-s".to_owned(),
        "-n".to_owned(),
        config.lvsnapshot.clone(),
        config.origin_path(),
    ];
    if ops.exec_failed(LVCREATE, &lvcreate_args) {
        out.push("Failed to 'lvcreate' LVM snapshot.".to_owned());
        return false;
    }

    if let Err(error) = ops.unlock_tables() {
        out.push(format!("Failed to unlock MySQL: {error}"));
        return false;
    }

    let mount_args = vec![config.snapshot_device(), config.mount_point()];
    if ops.exec_failed("mount", &mount_args) {
        out.push("Failed to mount newly created snapshot.".to_owned());
        return false;
    }

    let snapshot_files = config.snapshot_files_path();
    let chmod_args = vec!["-R".to_owned(), "755".to_owned(), snapshot_files.clone()];
    if ops.exec_failed("chmod", &chmod_args) {
        // The reference's message carries the stray '.' — kept verbatim.
        out.push("Failed to chmod the snapshot file.s".to_owned());
        return false;
    }

    // The reader (Snapshot::takePrimarySnapshot) takes the path from
    // between the last two newlines of stdout.
    out.push(snapshot_files);
    true
}

/// `release_snapshot` (`snapshot_helper.cpp:254-290`): unmount the snapshot
/// and lvremove it, reporting — and continuing past — either failure.
pub fn release_snapshot(
    config: &SnapshotConfig,
    ops: &mut dyn SnapshotOps,
    out: &mut Vec<String>,
) -> bool {
    if !become_root(ops, out) {
        return false;
    }

    let mut is_ok = true;
    let umount_args = vec![config.mount_point()];
    if ops.exec_failed("umount", &umount_args) {
        out.push("Unable to unmount snapshot.".to_owned());
        is_ok = false;
    }
    let lvremove_args = vec!["-f".to_owned(), config.snapshot_device()];
    if ops.exec_failed(LVREMOVE, &lvremove_args) {
        out.push("Unable to 'lvremove' snapshot.".to_owned());
        is_ok = false;
    }
    is_ok
}

/// `main` (`snapshot_helper.cpp:51-109`): the argument dispatch and the
/// exit codes `transfer_db` watches. Returns the exit code and everything
/// the tool printed, one entry per `printf` line.
pub fn run(
    args: &[String],
    reader: &dyn SnapshotConfigReader,
    ops: &mut dyn SnapshotOps,
) -> (i32, Vec<String>) {
    let mut out = Vec::new();
    out.push("SnapshotHelper started".to_owned());

    // No arguments: the probe transfer_db runs to test the setuid bit.
    if args.is_empty() {
        return (finish(become_root(ops, &mut out)), out);
    }

    if !reader.read() {
        out.push("Unable to read config file.".to_owned());
        return (FAILURE, out);
    }
    let Some(config) = validate_config(reader, &mut out) else {
        return (FAILURE, out);
    };

    match args[0].as_str() {
        "acquire-snapshot" => {
            if args.len() != 3 {
                // The reference prints this and falls through anyway; with
                // fewer arguments the credentials read as empty strings,
                // which fails the connection exactly as NULLs do there.
                out.push("Invalid argument list for 'acquire-snapshot'".to_owned());
            }
            let user = args.get(1).map(String::as_str).unwrap_or("");
            let pass = args.get(2).map(String::as_str).unwrap_or("");
            (
                finish(acquire_snapshot(&config, user, pass, ops, &mut out)),
                out,
            )
        }
        "release-snapshot" => {
            if args.len() != 1 {
                out.push("Invalid argument list for 'release-snapshot'".to_owned());
            }
            (finish(release_snapshot(&config, ops, &mut out)), out)
        }
        _ => {
            out.push("Unknown arguments provided.".to_owned());
            (FAILURE, out)
        }
    }
}

fn finish(success: bool) -> i32 {
    if success { SUCCESS } else { FAILURE }
}

/// What `Snapshot::takePrimarySnapshot` reads back from the helper's stdout
/// (`transfer_db/snapshot.cpp:282-306`): the path sits between the last two
/// newlines. Needs two of them — a stream without a terminator, or with a
/// single line, is reported as "unable to find snapshot path".
pub fn parse_snapshot_path(helper_stdout: &str) -> Option<String> {
    let eol = helper_stdout.rfind('\n')?;
    let start = helper_stdout[..eol].rfind('\n')? + 1;
    Some(helper_stdout[start..eol].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `/etc/bigworld.conf` `[snapshot]` section.
    struct Conf {
        read: bool,
        values: BTreeMap<String, String>,
    }

    impl Conf {
        fn full() -> Self {
            let values = [
                ("datadir", "mysql-data"),
                ("lvgroup", "vg0"),
                ("lvorigin", "mysql"),
                ("lvsnapshot", "snap0"),
                ("lvsizegb", "2"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
            Self { read: true, values }
        }

        fn without(mut self, key: &str) -> Self {
            self.values.remove(key);
            self
        }
    }

    impl SnapshotConfigReader for Conf {
        fn read(&self) -> bool {
            self.read
        }
        fn value(&self, _section: &str, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }
    }

    /// The ledger an operation run leaves behind.
    #[derive(Default)]
    struct Ledger {
        ops: Vec<String>,
        root_ok: bool,
        connect: Option<Result<(), ConnectFailure>>,
        locked: Option<Result<(), String>>,
        unlocked: Option<Result<(), String>>,
        failed_execs: Vec<String>,
    }

    impl SnapshotOps for Ledger {
        fn become_root(&mut self) -> Result<(), String> {
            if self.root_ok {
                Ok(())
            } else {
                Err("Operation not permitted".to_owned())
            }
        }
        fn connect(&mut self, user: &str, pass: &str) -> Result<(), ConnectFailure> {
            self.ops.push(format!("connect:{user}:{pass}"));
            self.connect.clone().unwrap_or(Ok(()))
        }
        fn lock_tables(&mut self) -> Result<(), String> {
            self.ops.push("lock".to_owned());
            self.locked.clone().unwrap_or(Ok(()))
        }
        fn unlock_tables(&mut self) -> Result<(), String> {
            self.ops.push("unlock".to_owned());
            self.unlocked.clone().unwrap_or(Ok(()))
        }
        fn exec_failed(&mut self, command: &str, args: &[String]) -> bool {
            self.ops.push(format!("exec:{command} {args:?}"));
            self.failed_execs.iter().any(|failed| failed == command)
        }
    }

    fn full_config() -> SnapshotConfig {
        SnapshotConfig {
            datadir: "mysql-data".to_owned(),
            lvgroup: "vg0".to_owned(),
            lvorigin: "mysql".to_owned(),
            lvsnapshot: "snap0".to_owned(),
            lvsizegb: "2".to_owned(),
        }
    }

    #[test]
    fn a_bare_invocation_probes_the_setuid_bit() {
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let (code, out) = run(&[], &Conf::full(), &mut ledger);
        assert_eq!(code, SUCCESS);
        assert_eq!(out, vec!["SnapshotHelper started"]);
        assert!(ledger.ops.is_empty(), "the probe runs no operations");
    }

    #[test]
    fn a_bare_invocation_fails_without_root() {
        let mut ledger = Ledger::default();
        let (code, out) = run(&[], &Conf::full(), &mut ledger);
        assert_eq!(code, FAILURE);
        let expected = vec![
            "SnapshotHelper started",
            "setuid root failed: Operation not permitted",
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn acquire_fails_before_touching_mysql_without_root() {
        let config = full_config();
        let mut ledger = Ledger::default();
        let mut out = Vec::new();
        assert!(!acquire_snapshot(&config, "u", "p", &mut ledger, &mut out));
        assert_eq!(out, vec!["setuid root failed: Operation not permitted"]);
        assert!(ledger.ops.is_empty(), "no connection is attempted");
    }

    #[test]
    fn release_fails_without_ever_unmounting() {
        let config = full_config();
        let mut ledger = Ledger::default();
        let mut out = Vec::new();
        assert!(!release_snapshot(&config, &mut ledger, &mut out));
        assert_eq!(out, vec!["setuid root failed: Operation not permitted"]);
        assert!(ledger.ops.is_empty(), "the snapshot is left mounted");
    }

    #[test]
    fn an_unreadable_config_fails_the_tool() {
        let conf = Conf {
            read: false,
            ..Conf::full()
        };
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let (code, out) = run(&["release-snapshot".to_owned()], &conf, &mut ledger);
        assert_eq!(code, FAILURE);
        assert!(out.contains(&"Unable to read config file.".to_owned()));
        assert!(ledger.ops.is_empty(), "no operations without a config");
    }

    #[test]
    fn a_config_missing_values_is_rejected_with_one_line_each() {
        let conf = Conf::full()
            .without("lvgroup")
            .without("lvorigin")
            .without("lvsizegb");
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let (code, out) = run(&["release-snapshot".to_owned()], &conf, &mut ledger);
        assert_eq!(code, FAILURE);
        for key in ["lvgroup", "lvorigin", "lvsizegb"] {
            assert!(out.contains(&format!("Unable to read config value for '{key}'")));
        }
        assert!(ledger.ops.is_empty());
    }

    #[test]
    fn acquire_takes_the_lock_snapshots_and_prints_the_path() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(acquire_snapshot(
            &config,
            "bwuser",
            "bwpass",
            &mut ledger,
            &mut out
        ));

        assert_eq!(ledger.ops[0], "connect:bwuser:bwpass");
        assert_eq!(ledger.ops[1], "lock");
        assert_eq!(
            ledger.ops[2],
            format!(
                "exec:{} [\"-L2G\", \"-s\", \"-n\", \"snap0\", \"/dev/vg0/mysql\"]",
                LVCREATE
            ),
            "the child argv carries the size as its argv[0] slot, per execl"
        );
        assert_eq!(ledger.ops[3], "unlock");
        assert_eq!(
            ledger.ops[4],
            "exec:mount [\"/dev/vg0/snap0\", \"/mnt/snap0/\"]"
        );
        assert_eq!(
            ledger.ops[5],
            "exec:chmod [\"-R\", \"755\", \"/mnt/snap0/mysql-data\"]"
        );
        assert_eq!(out, vec!["/mnt/snap0/mysql-data"]);
    }

    #[test]
    fn the_original_reader_reads_the_printed_path() {
        // The acceptance face: Snapshot::takePrimarySnapshot parses the
        // helper stdout exactly like parse_snapshot_path does.
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(acquire_snapshot(
            &config,
            "bwuser",
            "bwpass",
            &mut ledger,
            &mut out
        ));

        let stdout = format!("SnapshotHelper started\n{}\n", out.join("\n"));
        assert_eq!(
            parse_snapshot_path(&stdout),
            Some("/mnt/snap0/mysql-data".to_owned())
        );
    }

    #[test]
    fn acquire_fails_when_mysql_cannot_be_initialised() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            connect: Some(Err(ConnectFailure::Init)),
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!acquire_snapshot(&config, "u", "p", &mut ledger, &mut out));
        assert_eq!(out, vec!["Unable to initialise MySQL"]);
        assert_eq!(ledger.ops, vec!["connect:u:p"], "no lock, no exec");
    }

    #[test]
    fn acquire_fails_when_the_connection_is_refused() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            connect: Some(Err(ConnectFailure::Connect("Access denied".to_owned()))),
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!acquire_snapshot(&config, "u", "p", &mut ledger, &mut out));
        assert_eq!(out, vec!["Failed to connect to MySQL: Access denied"]);
        assert!(ledger.ops.len() == 1);
    }

    #[test]
    fn acquire_fails_when_the_read_lock_is_refused() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            locked: Some(Err("Table is read only".to_owned())),
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!acquire_snapshot(&config, "u", "p", &mut ledger, &mut out));
        assert_eq!(
            out,
            vec!["Failed to apply read lock MySQL: Table is read only"]
        );
        assert!(
            ledger.ops.len() == 2,
            "no snapshot is attempted under a failed lock"
        );
    }

    #[test]
    fn acquire_fails_when_lvcreate_fails_and_leaves_tables_locked() {
        // The reference returns without unlocking — the lock dies with the
        // connection, and its lvremove-on-failure TODO is unimplemented.
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            failed_execs: vec![LVCREATE.to_owned()],
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!acquire_snapshot(&config, "u", "p", &mut ledger, &mut out));
        assert_eq!(out, vec!["Failed to 'lvcreate' LVM snapshot."]);
        assert_eq!(
            ledger
                .ops
                .iter()
                .filter(|op| op == &&"unlock".to_owned())
                .count(),
            0
        );
    }

    #[test]
    fn acquire_fails_when_the_unlock_fails_after_the_snapshot_exists() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            unlocked: Some(Err("Server shutdown in progress".to_owned())),
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!acquire_snapshot(&config, "u", "p", &mut ledger, &mut out));
        assert_eq!(
            out,
            vec!["Failed to unlock MySQL: Server shutdown in progress"]
        );
        assert!(
            ledger.ops.iter().all(|op| !op.starts_with("exec:mount")),
            "the snapshot exists but is never mounted"
        );
    }

    #[test]
    fn acquire_fails_when_the_mount_fails() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            failed_execs: vec!["mount".to_owned()],
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!acquire_snapshot(&config, "u", "p", &mut ledger, &mut out));
        assert_eq!(out, vec!["Failed to mount newly created snapshot."]);
        assert!(ledger.ops.iter().all(|op| !op.starts_with("exec:chmod")));
    }

    #[test]
    fn acquire_fails_when_chmod_fails() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            failed_execs: vec!["chmod".to_owned()],
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!acquire_snapshot(&config, "u", "p", &mut ledger, &mut out));
        // The stray '.' before 's' is the reference's own typo.
        assert_eq!(out, vec!["Failed to chmod the snapshot file.s"]);
    }

    #[test]
    fn release_unmounts_and_removes_the_volume() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(release_snapshot(&config, &mut ledger, &mut out));
        assert_eq!(out, Vec::<String>::new());
        assert_eq!(
            ledger.ops,
            vec![
                "exec:umount [\"/mnt/snap0/\"]".to_owned(),
                format!("exec:{} [\"-f\", \"/dev/vg0/snap0\"]", LVREMOVE),
            ]
        );
    }

    #[test]
    fn release_reports_both_failures_and_still_removes() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            failed_execs: vec!["umount".to_owned(), LVREMOVE.to_owned()],
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!release_snapshot(&config, &mut ledger, &mut out));
        assert_eq!(
            out,
            vec![
                "Unable to unmount snapshot.",
                "Unable to 'lvremove' snapshot."
            ]
        );
        assert_eq!(
            ledger.ops.len(),
            2,
            "a failed umount does not stop lvremove"
        );
    }

    #[test]
    fn release_survives_a_single_umount_failure() {
        let config = full_config();
        let mut ledger = Ledger {
            root_ok: true,
            failed_execs: vec!["umount".to_owned()],
            ..Ledger::default()
        };
        let mut out = Vec::new();
        assert!(!release_snapshot(&config, &mut ledger, &mut out));
        assert_eq!(out, vec!["Unable to unmount snapshot."]);
    }

    #[test]
    fn the_release_entry_point_reports_the_exit_code() {
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let (code, out) = run(&["release-snapshot".to_owned()], &Conf::full(), &mut ledger);
        assert_eq!(code, SUCCESS);
        assert_eq!(
            out.first().map(String::as_str),
            Some("SnapshotHelper started")
        );
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn an_unknown_command_is_refused() {
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let (code, out) = run(&["frobnicate".to_owned()], &Conf::full(), &mut ledger);
        assert_eq!(code, FAILURE);
        assert!(out.contains(&"Unknown arguments provided.".to_owned()));
    }

    #[test]
    fn an_acquire_with_the_wrong_arity_is_still_attempted() {
        let mut ledger = Ledger {
            root_ok: true,
            // Empty credentials, as the reference's NULLs would carry them.
            connect: Some(Err(ConnectFailure::Connect("Access denied".to_owned()))),
            ..Ledger::default()
        };
        let (code, out) = run(
            &["acquire-snapshot".to_owned(), "loneuser".to_owned()],
            &Conf::full(),
            &mut ledger,
        );
        assert_eq!(code, FAILURE);
        assert!(
            out.contains(&"Invalid argument list for 'acquire-snapshot'".to_owned()),
            "the complaint prints and the run falls through, per the reference"
        );
        assert_eq!(
            ledger.ops[0], "connect:loneuser:",
            "the missing password reads as empty, as NULL creds fail there"
        );
    }

    #[test]
    fn a_release_with_extra_arguments_is_still_attempted() {
        let mut ledger = Ledger {
            root_ok: true,
            ..Ledger::default()
        };
        let (code, out) = run(
            &["release-snapshot".to_owned(), "unexpected".to_owned()],
            &Conf::full(),
            &mut ledger,
        );
        assert_eq!(code, SUCCESS);
        assert!(
            out.contains(&"Invalid argument list for 'release-snapshot'".to_owned()),
            "the complaint prints and the release proceeds, per the reference"
        );
    }

    #[test]
    fn the_reader_takes_the_path_between_the_last_two_newlines() {
        assert_eq!(
            parse_snapshot_path("SnapshotHelper started\n/mnt/snap0/mysql-data\n"),
            Some("/mnt/snap0/mysql-data".to_owned())
        );
        assert_eq!(parse_snapshot_path("/mnt/snap0/mysql-data\n"), None);
        assert_eq!(parse_snapshot_path("no newline at all"), None);
        assert_eq!(parse_snapshot_path(""), None);
    }

    #[test]
    fn the_constants_match_the_reference() {
        assert_eq!(SUCCESS, 0);
        assert_eq!(FAILURE, 1);
        assert_eq!(CONFIG_SECTION, "snapshot");
        assert_eq!(LVCREATE, "/usr/sbin/lvcreate");
        assert_eq!(LVREMOVE, "/usr/sbin/lvremove");
    }
}
