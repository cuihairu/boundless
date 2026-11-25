//! BW-09-05-03 the snapshot helper.
//!
//! `server/tools/snapshot_helper/snapshot_helper.cpp` takes a consistent copy
//! of the database directory: connect to MySQL and hold
//! `FLUSH TABLES WITH READ LOCK`, cut an LVM snapshot with `lvcreate -s`,
//! unlock, mount the snapshot under `/mnt/<snapshot>/`, and `chmod -R 755`
//! the data directory so the transfer can take it
//! (`acquire_snapshot`, `snapshot_helper.cpp:180-272`). `release_snapshot`
//! (`:277-310`) unmounts and removes the volume — and keeps going after a
//! failed unmount so the volume is not left behind.
//!
//! The configuration lives in the `[snapshot]` section of
//! `/etc/bigworld.conf` (`CONFIG_SECTION`, `validate_config`
//! `:117-150`); the five values it needs are `datadir`, `lvgroup`,
//! `lvorigin`, `lvsnapshot`, `lvsizegb`.
//!
//! The port drives the same sequence through a [`CommandRunner`] and records
//! every step, so the plan is reviewable without a setuid binary, an LVM
//! volume group, or a MySQL server.

use std::collections::BTreeMap;

/// `SnapshotHelper::SUCCESS` (`snapshot_constants.hpp:11-14`).
pub const SUCCESS: i32 = 0;

/// `SnapshotHelper::FAILURE` (`snapshot_constants.hpp:12-13`).
pub const FAILURE: i32 = 1;

/// `CONFIG_SECTION` (`snapshot_helper.cpp:38`).
pub const CONFIG_SECTION: &str = "snapshot";

/// The values `validate_config` requires, in its order
/// (`snapshot_helper.cpp:122-124`).
pub const CONFIG_KEYS: [&str; 5] = ["datadir", "lvgroup", "lvorigin", "lvsnapshot", "lvsizegb"];

/// `LVCREATE`/`LVREMOVE` (`snapshot_helper.cpp:35,41`).
pub const LVCREATE: &str = "/usr/sbin/lvcreate";
pub const LVREMOVE: &str = "/usr/sbin/lvremove";

/// The read lock the snapshot is cut under
/// (`snapshot_helper.cpp:187-189`).
pub const READ_LOCK_SQL: &str = "FLUSH TABLES WITH READ LOCK";

/// The unlock once the volume exists (`snapshot_helper.cpp:214-216`).
pub const UNLOCK_SQL: &str = "UNLOCK TABLES";

/// The `[snapshot]` section values (`ConfigReader`, `snapshot_helper.cpp:66`).
#[derive(Debug, Default, Clone)]
pub struct SnapshotConfig {
    values: BTreeMap<String, String>,
}

impl SnapshotConfig {
    /// A config from key/value pairs.
    pub fn from_pairs(pairs: [(&str, &str); 5]) -> Self {
        Self {
            values: pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// `ConfigReader::getValue` (`snapshot_helper.cpp:129-130`).
    pub fn get_value(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

/// One recorded step of the plan: a MySQL session event, an external
/// program, or the final report line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// The MySQL session opened for the read lock (the password is part of
    /// the session, never of the log).
    MySqlConnect(String),
    /// A statement sent to the MySQL session.
    MySqlQuery(String),
    /// An external program with its argument list.
    Exec(String, Vec<String>),
    /// The snapshot file path the tool prints on success
    /// (`snapshot_helper.cpp:267-269`).
    Report(String),
}

/// The external commands (`execl`, `snapshot_helper.cpp:202-262`).
pub trait CommandRunner {
    /// Run the program with the arguments; true when it succeeded.
    fn exec(&mut self, program: &str, args: &[String]) -> bool;
}

/// A runner that succeeds for every command, for the happy-path drives.
#[derive(Default)]
pub struct SucceedsAll;

impl CommandRunner for SucceedsAll {
    fn exec(&mut self, _program: &str, _args: &[String]) -> bool {
        true
    }
}

/// The acquire/release driver (`snapshot_helper.cpp`).
#[derive(Default)]
pub struct SnapshotHelper {
    log: Vec<Step>,
    errors: Vec<String>,
    /// The answers the MySQL session gives to its statements, in order; an
    /// exhausted queue means success.
    sql_answers: std::collections::VecDeque<(bool, String)>,
}

impl SnapshotHelper {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// The answers the MySQL session gives, statement by statement
    /// (`snapshot_helper.cpp:187-192,214-220`), for the failure paths.
    pub fn expect_sql(&mut self, answers: &[(bool, &str)]) {
        self.sql_answers = answers
            .iter()
            .map(|(ok, error)| (*ok, error.to_string()))
            .collect();
    }

    /// The recorded plan.
    pub fn log(&self) -> &[Step] {
        &self.log
    }

    /// The messages printed beside failures (`printf`).
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// `becomeRootUser` (`snapshot_helper.cpp:316-326`): `setuid(0)` or the
    /// `setuid root failed: %s` report.
    fn become_root(&mut self, root_ok: bool) -> bool {
        if !root_ok {
            self.errors
                .push("setuid root failed: Operation not permitted".to_string());
            return false;
        }
        true
    }

    /// `validate_config` (`snapshot_helper.cpp:117-150`): every required
    /// value must be present; each missing one is reported.
    pub fn validate_config(&mut self, config: &SnapshotConfig) -> bool {
        let mut status = true;
        for key in CONFIG_KEYS {
            if config.get_value(key).is_none() {
                self.errors
                    .push(format!("Unable to read config value for '{key}'"));
                status = false;
            }
        }
        status
    }

    /// `acquire_snapshot` (`snapshot_helper.cpp:152-272`): lock, cut, unlock,
    /// mount, open the permissions, and report the snapshot data directory.
    ///
    /// The password reaches the MySQL session like the reference's
    /// `mysql_real_connect(..., dbPass, ...)`
    /// (`snapshot_helper.cpp:180-185`) and never the recorded plan.
    pub fn acquire(
        &mut self,
        runner: &mut dyn CommandRunner,
        config: &SnapshotConfig,
        db_user: &str,
        _db_password: &str,
        root_ok: bool,
    ) -> bool {
        // Elevate privileges for actual operations.
        if !self.become_root(root_ok) {
            return false;
        }

        let data_dir = config.get_value("datadir").unwrap_or_default();
        let lv_group = config.get_value("lvgroup").unwrap_or_default();
        let lv_origin = config.get_value("lvorigin").unwrap_or_default();
        let lv_snapshot = config.get_value("lvsnapshot").unwrap_or_default();
        let lv_size_gb = config.get_value("lvsizegb").unwrap_or_default();

        // Connect to the database and apply a read lock.
        self.log.push(Step::MySqlConnect(db_user.to_string()));
        if !self.apply_sql(READ_LOCK_SQL, "Failed to apply read lock MySQL") {
            return false;
        }

        // Now create an LVM snapshot. Anything past this point must remove
        // the volume if it fails (`snapshot_helper.cpp:198-201`).
        let volume = format!("/dev/{lv_group}/{lv_origin}");
        let lvcreate_args = vec![
            format!("-L{lv_size_gb}G"),
            "-s".to_string(),
            "-n".to_string(),
            lv_snapshot.to_string(),
            volume,
        ];
        self.log
            .push(Step::Exec(LVCREATE.to_string(), lvcreate_args.clone()));
        if !runner.exec(LVCREATE, &lvcreate_args) {
            self.errors
                .push("Failed to 'lvcreate' LVM snapshot.".to_string());
            // The MySQL connection is closed before leaving.
            return false;
        }

        if !self.apply_sql(UNLOCK_SQL, "Failed to unlock MySQL") {
            return false;
        }

        let mounted = format!("/dev/{lv_group}/{lv_snapshot}");
        let mount_args = vec![mounted, format!("/mnt/{lv_snapshot}/")];
        self.log
            .push(Step::Exec("mount".to_string(), mount_args.clone()));
        if !runner.exec("mount", &mount_args) {
            self.errors
                .push("Failed to mount newly created snapshot.".to_string());
            return false;
        }

        let snapshot_files = format!("/mnt/{lv_snapshot}/{data_dir}");
        // Relax permissions so we can take ownership of the backup files
        // (`snapshot_helper.cpp:252-256`).
        let chmod_args = vec!["-R".to_string(), "755".to_string(), snapshot_files.clone()];
        self.log
            .push(Step::Exec("chmod".to_string(), chmod_args.clone()));
        if !runner.exec("chmod", &chmod_args) {
            self.errors
                .push("Failed to chmod the snapshot file.s".to_string());
            return false;
        }

        self.log.push(Step::Report(snapshot_files.clone()));
        true
    }

    /// `release_snapshot` (`snapshot_helper.cpp:277-310`): unmount and remove
    /// the volume; a failed unmount does not stop the removal.
    pub fn release(
        &mut self,
        runner: &mut dyn CommandRunner,
        config: &SnapshotConfig,
        root_ok: bool,
    ) -> bool {
        if !self.become_root(root_ok) {
            return false;
        }

        let lv_group = config.get_value("lvgroup").unwrap_or_default();
        let lv_snapshot = config.get_value("lvsnapshot").unwrap_or_default();

        let mut is_ok = true;

        let umount_args = vec![format!("/mnt/{lv_snapshot}/")];
        self.log
            .push(Step::Exec("umount".to_string(), umount_args.clone()));
        if !runner.exec("umount", &umount_args) {
            self.errors.push("Unable to unmount snapshot.".to_string());
            is_ok = false;
        }

        let lvremove_args = vec!["-f".to_string(), format!("/dev/{lv_group}/{lv_snapshot}")];
        self.log
            .push(Step::Exec(LVREMOVE.to_string(), lvremove_args.clone()));
        if !runner.exec(LVREMOVE, &lvremove_args) {
            self.errors
                .push("Unable to 'lvremove' snapshot.".to_string());
            is_ok = false;
        }

        is_ok
    }

    /// The `main` dispatch (`snapshot_helper.cpp:55-115`): the argument-less
    /// run only tests the setuid attribute; the two commands carry their
    /// argument lists; anything else is refused.
    pub fn run(
        &mut self,
        args: &[&str],
        config: Option<&SnapshotConfig>,
        runner: &mut dyn CommandRunner,
        root_ok: bool,
    ) -> i32 {
        // If no arguments are provided, test if setuid attribute is set.
        if args.is_empty() {
            return if self.become_root(root_ok) {
                SUCCESS
            } else {
                FAILURE
            };
        }

        let Some(config) = config else {
            self.errors.push("Unable to read config file.".to_string());
            return FAILURE;
        };

        if !self.validate_config(config) {
            return FAILURE;
        }

        match args[0] {
            "acquire-snapshot" => {
                // The command carries the database user and password.
                if args.len() != 3 {
                    self.errors
                        .push("Invalid argument list for 'acquire-snapshot'".to_string());
                    if args.len() < 3 {
                        // Without them there is nothing to connect with.
                        return FAILURE;
                    }
                }
                if !self.acquire(runner, config, args[1], args[2], root_ok) {
                    return FAILURE;
                }
            }
            "release-snapshot" => {
                if args.len() != 1 {
                    self.errors
                        .push("Invalid argument list for 'release-snapshot'".to_string());
                }
                if !self.release(runner, config, root_ok) {
                    return FAILURE;
                }
            }
            _ => {
                self.errors.push("Unknown arguments provided.".to_string());
                return FAILURE;
            }
        }

        SUCCESS
    }

    /// One statement through the MySQL session; a failure is reported the
    /// way `mysql_real_query`'s caller reports it
    /// (`snapshot_helper.cpp:187-192,214-220`).
    fn apply_sql(&mut self, statement: &str, failure: &str) -> bool {
        self.log.push(Step::MySqlQuery(statement.to_string()));
        match self.sql_answers.pop_front() {
            Some((true, _)) | None => true,
            Some((false, error)) => {
                self.errors.push(format!("{failure}: {error}"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config the tool requires.
    fn config() -> SnapshotConfig {
        SnapshotConfig::from_pairs([
            ("datadir", "var/lib/mysql"),
            ("lvgroup", "vg0"),
            ("lvorigin", "mysql"),
            ("lvsnapshot", "mysql-snap"),
            ("lvsizegb", "10"),
        ])
    }

    /// A runner that fails the named programs.
    struct FailsAt(Vec<&'static str>);

    impl CommandRunner for FailsAt {
        fn exec(&mut self, program: &str, _args: &[String]) -> bool {
            !self.0.contains(&program)
        }
    }

    /// `validate_config` (`snapshot_helper.cpp:117-150`): one report per
    /// missing value.
    #[test]
    fn a_missing_config_value_is_reported() {
        let mut helper = SnapshotHelper::new();
        let sparse = SnapshotConfig::from_pairs([
            ("datadir", "var/lib/mysql"),
            ("lvgroup", "vg0"),
            ("lvorigin", "mysql"),
            ("lvsnapshot", "mysql-snap"),
            ("lvsizegb", ""),
        ]);

        // An empty value still reads; only absent keys fail.
        assert!(helper.validate_config(&sparse));
        assert!(helper.errors().is_empty());

        let mut values = config();
        values.values.remove("lvsnapshot");
        values.values.remove("lvsizegb");
        assert!(!helper.validate_config(&values));
        assert_eq!(
            helper.errors(),
            &[
                "Unable to read config value for 'lvsnapshot'".to_string(),
                "Unable to read config value for 'lvsizegb'".to_string(),
            ]
        );
    }

    /// `acquire_snapshot` (`snapshot_helper.cpp:152-272`): the whole
    /// sequence, ending with the snapshot data directory it reports.
    #[test]
    fn acquire_locks_cuts_mounts_and_reports() {
        let mut helper = SnapshotHelper::new();
        let mut runner = SucceedsAll;

        assert!(helper.acquire(&mut runner, &config(), "bwadmin", "pw", true));

        assert_eq!(
            helper.log(),
            &[
                Step::MySqlConnect("bwadmin".to_string()),
                Step::MySqlQuery(READ_LOCK_SQL.to_string()),
                Step::Exec(
                    LVCREATE.to_string(),
                    vec![
                        "-L10G".to_string(),
                        "-s".to_string(),
                        "-n".to_string(),
                        "mysql-snap".to_string(),
                        "/dev/vg0/mysql".to_string(),
                    ]
                ),
                Step::MySqlQuery(UNLOCK_SQL.to_string()),
                Step::Exec(
                    "mount".to_string(),
                    vec![
                        "/dev/vg0/mysql-snap".to_string(),
                        "/mnt/mysql-snap/".to_string()
                    ]
                ),
                Step::Exec(
                    "chmod".to_string(),
                    vec![
                        "-R".to_string(),
                        "755".to_string(),
                        "/mnt/mysql-snap/var/lib/mysql".to_string()
                    ]
                ),
                // The reported path the transfer takes over.
                Step::Report("/mnt/mysql-snap/var/lib/mysql".to_string()),
            ]
        );
        assert!(helper.errors().is_empty());
    }

    /// A failing `lvcreate` stops the run before the unlock — the tables
    /// stay locked until the session closes
    /// (`snapshot_helper.cpp:202-212`).
    #[test]
    fn a_failed_lvcreate_stops_before_the_unlock() {
        let mut helper = SnapshotHelper::new();
        let mut runner = FailsAt(vec![LVCREATE]);

        assert!(!helper.acquire(&mut runner, &config(), "bwadmin", "pw", true));
        assert_eq!(
            helper.errors(),
            &["Failed to 'lvcreate' LVM snapshot.".to_string()]
        );
        assert!(!helper
            .log()
            .iter()
            .any(|s| matches!(s, Step::MySqlQuery(sql) if sql == UNLOCK_SQL)));
        assert!(!helper
            .log()
            .iter()
            .any(|s| matches!(s, Step::Exec(p, _) if p == "mount")));
    }

    /// A failed `lvcreate` is reported without an error trail from the later
    /// steps; a failed chmod still happened after the mount
    /// (`snapshot_helper.cpp:258-266`).
    #[test]
    fn a_failed_chmod_reports_without_the_path() {
        let mut helper = SnapshotHelper::new();
        let mut runner = FailsAt(vec!["chmod"]);

        assert!(!helper.acquire(&mut runner, &config(), "bwadmin", "pw", true));
        assert_eq!(
            helper.errors(),
            &["Failed to chmod the snapshot file.s".to_string()]
        );
        // The volume was cut and mounted, but nothing is reported.
        assert!(helper
            .log()
            .iter()
            .any(|s| matches!(s, Step::Exec(p, _) if p == "mount")));
        assert!(!helper.log().iter().any(|s| matches!(s, Step::Report(_))));
    }

    /// A failed mount stops the run after the unlock: the volume exists but
    /// is neither opened nor reported
    /// (`snapshot_helper.cpp:243-249`).
    #[test]
    fn a_failed_mount_stops_after_the_unlock() {
        let mut helper = SnapshotHelper::new();
        let mut runner = FailsAt(vec!["mount"]);

        assert!(!helper.acquire(&mut runner, &config(), "bwadmin", "pw", true));
        assert_eq!(
            helper.errors(),
            &["Failed to mount newly created snapshot.".to_string()]
        );
        // The snapshot was cut and unlocked, so the caller must remove the
        // volume; the tool itself only reports the failure.
        assert!(helper
            .log()
            .iter()
            .any(|s| matches!(s, Step::MySqlQuery(sql) if sql == UNLOCK_SQL)));
        assert!(!helper
            .log()
            .iter()
            .any(|s| matches!(s, Step::Exec(p, _) if p == "chmod")));
        assert!(!helper.log().iter().any(|s| matches!(s, Step::Report(_))));
    }

    /// `release_snapshot` (`snapshot_helper.cpp:277-310`): a failed unmount
    /// does not stop the volume removal.
    #[test]
    fn release_removes_the_volume_even_after_a_failed_unmount() {
        let mut helper = SnapshotHelper::new();
        let mut runner = FailsAt(vec!["umount", LVREMOVE]);

        assert!(!helper.release(&mut runner, &config(), true));
        assert_eq!(
            helper.errors(),
            &[
                "Unable to unmount snapshot.".to_string(),
                "Unable to 'lvremove' snapshot.".to_string(),
            ]
        );

        // Both commands ran despite the first failing.
        assert_eq!(
            helper.log(),
            &[
                Step::Exec("umount".to_string(), vec!["/mnt/mysql-snap/".to_string()]),
                Step::Exec(
                    LVREMOVE.to_string(),
                    vec!["-f".to_string(), "/dev/vg0/mysql-snap".to_string()]
                ),
            ]
        );

        // The happy release is quiet and successful.
        let mut helper = SnapshotHelper::new();
        let mut runner = SucceedsAll;
        assert!(helper.release(&mut runner, &config(), true));
        assert!(helper.errors().is_empty());
    }

    /// The `main` dispatch (`snapshot_helper.cpp:55-115`).
    #[test]
    fn the_entrypoint_dispatches_and_reports_exit_codes() {
        let mut runner = SucceedsAll;

        // No arguments: only the setuid attribute is tested.
        let mut helper = SnapshotHelper::new();
        assert_eq!(helper.run(&[], Some(&config()), &mut runner, true), SUCCESS);
        let mut helper = SnapshotHelper::new();
        assert_eq!(
            helper.run(&[], Some(&config()), &mut runner, false),
            FAILURE
        );
        assert_eq!(
            helper.errors(),
            &["setuid root failed: Operation not permitted".to_string()]
        );

        // No readable config: refused before the commands.
        let mut helper = SnapshotHelper::new();
        assert_eq!(
            helper.run(&["release-snapshot"], None, &mut runner, true),
            FAILURE
        );
        assert_eq!(
            helper.errors(),
            &["Unable to read config file.".to_string()]
        );

        // The two commands.
        let mut helper = SnapshotHelper::new();
        assert_eq!(
            helper.run(
                &["acquire-snapshot", "bwadmin", "secret"],
                Some(&config()),
                &mut runner,
                true
            ),
            SUCCESS
        );
        // The password stays out of the recorded plan.
        assert!(!format!("{:?}", helper.log()).contains("secret"));
        assert!(helper
            .log()
            .contains(&Step::MySqlConnect("bwadmin".to_string())));

        let mut helper = SnapshotHelper::new();
        assert_eq!(
            helper.run(&["release-snapshot"], Some(&config()), &mut runner, true),
            SUCCESS
        );

        // Unknown arguments are refused.
        let mut helper = SnapshotHelper::new();
        assert_eq!(
            helper.run(&["snapshot-now"], Some(&config()), &mut runner, true),
            FAILURE
        );
        assert_eq!(
            helper.errors(),
            &["Unknown arguments provided.".to_string()]
        );

        // A short argument list is reported and cannot connect at all.
        let mut helper = SnapshotHelper::new();
        assert_eq!(
            helper.run(&["acquire-snapshot"], Some(&config()), &mut runner, true),
            FAILURE
        );
        assert_eq!(
            helper.errors(),
            &["Invalid argument list for 'acquire-snapshot'".to_string()]
        );

        // A refused acquire fails the run.
        let mut helper = SnapshotHelper::new();
        let mut failing = FailsAt(vec![LVCREATE]);
        assert_eq!(
            helper.run(
                &["acquire-snapshot", "bwadmin", "secret"],
                Some(&config()),
                &mut failing,
                true
            ),
            FAILURE
        );
    }

    /// A failed read lock stops the run before the volume is cut, and a
    /// failed unlock stops it after
    /// (`snapshot_helper.cpp:187-192,214-220`).
    #[test]
    fn a_failed_mysql_statement_stops_the_run() {
        let mut helper = SnapshotHelper::new();
        let mut runner = SucceedsAll;
        helper.expect_sql(&[(false, "Access denied for user 'bwadmin'")]);

        assert!(!helper.acquire(&mut runner, &config(), "bwadmin", "pw", true));
        assert_eq!(
            helper.errors(),
            &["Failed to apply read lock MySQL: Access denied for user 'bwadmin'".to_string()]
        );
        // Nothing ran: no volume, no unlock.
        assert!(!helper
            .log()
            .iter()
            .any(|s| matches!(s, Step::Exec(p, _) if p == LVCREATE)));

        // A read lock that holds but an unlock that fails: the volume was
        // already cut, so the caller must remove it.
        let mut helper = SnapshotHelper::new();
        let mut runner = SucceedsAll;
        helper.expect_sql(&[(true, ""), (false, "Server has gone away")]);
        let cut = helper.acquire(&mut runner, &config(), "bwadmin", "pw", true);
        assert!(!cut);
        assert_eq!(
            helper.errors(),
            &["Failed to unlock MySQL: Server has gone away".to_string()]
        );
        assert!(helper
            .log()
            .iter()
            .any(|s| matches!(s, Step::Exec(p, _) if p == LVCREATE)));
        assert!(!helper.log().iter().any(|s| matches!(s, Step::Report(_))));

        // The happy session answers every statement.
        let mut helper = SnapshotHelper::new();
        let mut runner = SucceedsAll;
        assert!(helper.acquire(&mut runner, &config(), "bwadmin", "pw", true));
    }

    /// A release with a stray argument is reported but still runs, and a
    /// failing release fails the run
    /// (`snapshot_helper.cpp:105-112`).
    #[test]
    fn the_release_run_reports_and_fails_like_the_tool() {
        // The stray argument is reported; the release goes ahead.
        let mut helper = SnapshotHelper::new();
        let mut runner = SucceedsAll;
        assert_eq!(
            helper.run(
                &["release-snapshot", "extra"],
                Some(&config()),
                &mut runner,
                true
            ),
            SUCCESS
        );
        assert_eq!(
            helper.errors(),
            &["Invalid argument list for 'release-snapshot'".to_string()]
        );

        // A failing release fails the whole run.
        let mut helper = SnapshotHelper::new();
        let mut failing = FailsAt(vec!["umount"]);
        assert_eq!(
            helper.run(&["release-snapshot"], Some(&config()), &mut failing, true),
            FAILURE
        );
        assert_eq!(
            helper.errors(),
            &["Unable to unmount snapshot.".to_string()]
        );
    }

    /// Without the root attribute the privileged operations never happen
    /// (`snapshot_helper.cpp:316-326`).
    #[test]
    fn without_root_nothing_is_run() {
        let mut helper = SnapshotHelper::new();
        let mut runner = SucceedsAll;

        assert!(!helper.acquire(&mut runner, &config(), "bwadmin", "pw", false));
        assert!(helper.log().is_empty());
        assert!(!helper.release(&mut runner, &config(), false));

        let mut helper = SnapshotHelper::new();
        let mut failing = FailsAt(vec!["umount", LVREMOVE]);
        // The release reports both the unmount and the removal it still made.
        assert!(!helper.release(&mut failing, &config(), true));
        assert_eq!(
            helper.errors(),
            &[
                "Unable to unmount snapshot.".to_string(),
                "Unable to 'lvremove' snapshot.".to_string(),
            ]
        );
    }

    /// An extra argument is reported but the command still runs
    /// (`snapshot_helper.cpp:84-88`).
    #[test]
    fn an_extra_argument_is_reported_but_runs() {
        let mut helper = SnapshotHelper::new();
        let mut runner = SucceedsAll;

        assert_eq!(
            helper.run(
                &["acquire-snapshot", "bwadmin", "secret", "extra"],
                Some(&config()),
                &mut runner,
                true
            ),
            SUCCESS
        );
        assert_eq!(
            helper.errors(),
            &["Invalid argument list for 'acquire-snapshot'".to_string()]
        );
        // The acquisition went ahead and reported its snapshot path.
        assert!(helper
            .log()
            .iter()
            .any(|s| matches!(s, Step::Report(p) if p == "/mnt/mysql-snap/var/lib/mysql")));
    }

    /// A config the tool cannot work with stops it before any command
    /// (`snapshot_helper.cpp:76-82,117-150`).
    #[test]
    fn an_incomplete_config_stops_the_run() {
        let mut helper = SnapshotHelper::new();
        let mut incomplete = config();
        incomplete.values.remove("lvgroup");

        let mut runner = SucceedsAll;
        assert_eq!(
            helper.run(&["release-snapshot"], Some(&incomplete), &mut runner, true),
            FAILURE
        );
        assert!(helper.log().is_empty());
        assert_eq!(
            helper.errors(),
            &["Unable to read config value for 'lvgroup'".to_string()]
        );
    }
}
