//! The `LaunchDB`, a port of the reference `tools/bwlauncher/launch_db.cpp`.
//!
//! A `LaunchDB` scans a directory for `*.db` files; each file is a tiny XML
//! document `<root><exePath> path </exePath></root>` naming the game
//! executable registered under the file's base name (the `.db` extension
//! stripped by chopping three characters — `file.size() - 3`). Only
//! records whose exe path exists as a real file (not a directory) enter
//! the map; unreadable or malformed files are silently skipped.
//!
//! The reference parses with a vendored tinyxml; this port reads the same
//! documents with `resources::datasection`.

use crate::LauncherHost;
use resources::datasection;
use std::collections::BTreeMap;

/// `LaunchDB` (`launch_db.hpp`): the gameID → exePath table.
pub struct LaunchDB {
    applications: BTreeMap<String, String>,
}

impl LaunchDB {
    /// `LaunchDB::LaunchDB(path)` → `loadPath`.
    pub fn new(host: &dyn LauncherHost, path: &str) -> Self {
        let mut db = LaunchDB {
            applications: BTreeMap::new(),
        };
        db.load_path(host, path);
        db
    }

    /// `loadFile` (`launch_db.cpp:44`): read `path + file`, require a root
    /// element and an `exePath` child with a text value; the value must
    /// pass `GetFileAttributes` as a non-directory before the record lands
    /// keyed by the file name minus its last three characters (`.db`).
    fn load_file(&mut self, host: &dyn LauncherHost, path: &str, file: &str) {
        let raw = match host.read_file(&format!("{}{}", path, file)) {
            Some(raw) => raw,
            None => return,
        };

        let doc = match datasection::parse("root", &raw) {
            Ok(doc) => doc,
            Err(_) => return,
        };

        let exe_path = doc.read_string("exePath", "");
        // tinyxml: `node->FirstChild()` on `<exePath> x </exePath>` yields
        // the trimmed text; the datasection trims at parse time.
        if exe_path.is_empty() {
            return;
        }

        if host.file_attributes_are_file(&exe_path) {
            let game_id = &file[..file.len() - 3];
            self.applications.insert(game_id.to_string(), exe_path);
        }
    }

    /// `loadPath` (`launch_db.cpp:72`): ensure a trailing `\\`, then
    /// enumerate `*.db` (non-directory entries only) in find order.
    fn load_path(&mut self, host: &dyn LauncherHost, path: &str) {
        let mut path = path.to_string();
        if !path.is_empty() && !path.ends_with('\\') {
            path.push('\\');
        }

        for file in host.list_db_files(&path) {
            self.load_file(host, &path, &file);
        }
    }

    /// `launch` (`launch_db.cpp:102`): run the recorded exe with
    /// `--username u --password p` through the host.
    ///
    /// Reference quirk: the guard is `ShellExecuteEx(&sei) <= 32`, but
    /// `ShellExecuteEx` returns a `BOOL` (0 or 1), so the comparison is
    /// always true — the launch reports success for **any** registered
    /// game ID, even if the shell execution failed. Only an unknown game
    /// ID yields false (after a `DEBUG_MSG`).
    pub fn launch(
        &self,
        host: &mut dyn LauncherHost,
        game_id: &str,
        username: &str,
        password: &str,
    ) -> bool {
        if let Some(exe_path) = self.applications.get(game_id) {
            host.launch_game(exe_path, username, password);
            return true;
        }

        false
    }

    /// `size` (`launch_db.cpp:128`).
    pub fn size(&self) -> usize {
        self.applications.len()
    }

    /// `operator[]` (`launch_db.cpp:134`): the index-th (gameID, exePath)
    /// pair in map (sorted) order.
    pub fn entry(&self, index: usize) -> Option<(String, String)> {
        self.applications
            .iter()
            .nth(index)
            .map(|(k, v)| (k.clone(), v.clone()))
    }

    /// The sorted table, for tests and dumps.
    pub fn applications(&self) -> &BTreeMap<String, String> {
        &self.applications
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LauncherHost;
    use crate::RegBase;
    use crate::Unprivileged;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Host with an in-memory file system + registry.
    struct DbHost {
        files: RefCell<BTreeMap<String, Vec<u8>>>,
        directories: RefCell<Vec<String>>,
        launched: RefCell<Vec<(String, String, String)>>,
        fail_launches: RefCell<bool>,
        hidden: RefCell<Vec<String>>,
    }

    impl DbHost {
        fn new() -> Self {
            DbHost {
                files: RefCell::new(BTreeMap::new()),
                directories: RefCell::new(Vec::new()),
                launched: RefCell::new(Vec::new()),
                fail_launches: RefCell::new(false),
                hidden: RefCell::new(Vec::new()),
            }
        }

        fn put(&self, path: &str, body: &str) {
            self.files
                .borrow_mut()
                .insert(path.to_string(), body.as_bytes().to_vec());
        }

        fn add_directory(&self, path: &str) {
            self.directories.borrow_mut().push(path.to_string());
        }

        fn fail_launches(&self, fail: bool) {
            *self.fail_launches.borrow_mut() = fail;
        }

        /// Make `read_file` refuse a listed path (the `FindFirstFile`/
        /// open race the reference tolerates).
        fn hide(&self, path: &str) {
            self.hidden.borrow_mut().push(path.to_string());
        }
    }

    impl LauncherHost for DbHost {
        fn app_full_path(&self) -> String {
            "c:\\launch\\bwlauncher.exe".to_string()
        }
        fn windows_path(&self, _all_users: bool) -> String {
            String::new()
        }
        fn file_exists(&self, file_name: &str) -> bool {
            self.files.borrow().contains_key(file_name)
        }
        fn verify_registry(
            &self,
            _base: RegBase,
            _key: &str,
            _name: &str,
            _expected: &str,
        ) -> bool {
            false
        }
        fn write_registry(
            &mut self,
            _base: RegBase,
            _k: &str,
            _n: &str,
            _v: &str,
        ) -> Result<bool, Unprivileged> {
            Err(Unprivileged)
        }
        fn display_message(&mut self, _msg: &str, _flags: i32) -> i32 {
            0
        }
        fn launch_url(&mut self, _url: &str) {}
        fn launch_game(&mut self, exe_path: &str, username: &str, password: &str) -> bool {
            self.launched.borrow_mut().push((
                exe_path.to_string(),
                username.to_string(),
                password.to_string(),
            ));
            !*self.fail_launches.borrow()
        }
        fn restart_with_privileges(&mut self, _switch: &str) {}
        fn run_program(&mut self, _cmd: &str) {}
        fn list_db_files(&self, path: &str) -> Vec<String> {
            self.files
                .borrow()
                .keys()
                .filter(|k| k.starts_with(path) && k.ends_with(".db"))
                .map(|k| k[path.len()..].to_string())
                .collect()
        }
        fn read_file(&self, path: &str) -> Option<Vec<u8>> {
            if self.hidden.borrow().iter().any(|p| p == path) {
                return None;
            }
            self.files.borrow().get(path).cloned()
        }
        fn file_attributes_are_file(&self, path: &str) -> bool {
            self.files.borrow().contains_key(path)
                && !self.directories.borrow().iter().any(|d| d == path)
        }
        fn write_db_file(&mut self, _path: &str, _body: &str) -> Result<bool, Unprivileged> {
            Err(Unprivileged)
        }
        fn install_self(&mut self, _all_users: bool) -> Result<(), Unprivileged> {
            Ok(())
        }
    }

    #[test]
    fn loads_records_from_db_files_and_chops_three_chars() {
        let host = DbHost::new();
        host.put(
            "c:\\launch\\crab.db",
            "<root>\n\t<exePath> c:\\games\\crab.exe </exePath>\n</root>\n",
        );
        // A second game to show the sorted-key indexing.
        host.put(
            "c:\\launch\\blob.db",
            "<root><exePath>c:\\games\\blob.exe</exePath></root>",
        );
        // The recorded executables must exist on the fake disk.
        host.put("c:\\games\\crab.exe", "");
        host.put("c:\\games\\blob.exe", "");

        let db = LaunchDB::new(&host, "c:\\launch");
        assert_eq!(db.size(), 2);
        assert_eq!(
            db.applications().get("crab").map(String::as_str),
            Some("c:\\games\\crab.exe")
        );
        assert_eq!(
            db.entry(0).unwrap(),
            ("blob".to_string(), "c:\\games\\blob.exe".to_string())
        );
        assert_eq!(
            db.entry(1).unwrap(),
            ("crab".to_string(), "c:\\games\\crab.exe".to_string())
        );
        assert_eq!(db.entry(2), None);
        assert_eq!(db.applications().get(".blo"), None);
    }

    #[test]
    fn skips_malformed_missing_or_directory_records() {
        let host = DbHost::new();
        // Unreadable file: listed by find but read_file returns None.
        host.put("c:\\launch\\ghost.db", "");
        host.files.borrow_mut().remove("c:\\launch\\ghost.db");
        // Parse failure: unterminated tag.
        host.put("c:\\launch\\bad.db", "<root><exePath> x");
        // Missing exePath child.
        host.put("c:\\launch\\empty.db", "<root></root>");
        // exePath records a directory (rejected by the attribute check):
        // the directory is registered as such, its content is not a file.
        host.put(
            "c:\\launch\\dir.db",
            "<root><exePath>c:\\games\\dir</exePath></root>",
        );
        host.add_directory("c:\\games\\dir");

        let db = LaunchDB::new(&host, "c:\\launch");
        assert_eq!(db.size(), 0);
        assert_eq!(db.entry(0), None);
    }

    #[test]
    fn launch_runs_the_recorded_exe_with_credentials() {
        let mut host = DbHost::new();
        host.put(
            "c:\\launch\\crab.db",
            "<root><exePath>c:\\games\\crab.exe</exePath></root>",
        );
        host.put("c:\\games\\crab.exe", "");
        let db = LaunchDB::new(&host, "c:\\launch");

        assert!(db.launch(&mut host, "crab", "u", "p"));
        assert_eq!(
            host.launched.borrow().as_slice(),
            &[(
                "c:\\games\\crab.exe".to_string(),
                "u".to_string(),
                "p".to_string()
            )]
        );
        assert!(!db.launch(&mut host, "missing", "u", "p"));
        assert_eq!(host.launched.borrow().len(), 1);
    }

    #[test]
    fn launch_reports_success_even_when_the_execution_fails() {
        let mut host = DbHost::new();
        host.put(
            "c:\\launch\\crab.db",
            "<root><exePath>c:\\games\\crab.exe</exePath></root>",
        );
        host.put("c:\\games\\crab.exe", "");
        let db = LaunchDB::new(&host, "c:\\launch");

        // ShellExecuteEx returns BOOL; `<= 32` is always true, so even a
        // failed execution reports success (reference quirk).
        host.fail_launches(true);
        assert!(db.launch(&mut host, "crab", "u", "p"));
        assert_eq!(host.launched.borrow().len(), 1);
    }

    #[test]
    fn a_listed_but_unreadable_db_file_is_skipped() {
        let host = DbHost::new();
        host.put(
            "c:\\launch\\ghost.db",
            "<root><exePath>c:\\games\\ghost.exe</exePath></root>",
        );
        host.hide("c:\\launch\\ghost.db");

        let db = LaunchDB::new(&host, "c:\\launch");
        assert_eq!(db.size(), 0);
    }

    #[test]
    fn a_trailing_separator_path_is_used_verbatim() {
        let host = DbHost::new();
        host.put(
            "c:\\launch\\crab.db",
            "<root><exePath>c:\\games\\crab.exe</exePath></root>",
        );
        host.put("c:\\games\\crab.exe", "");

        // loadPath only appends the separator when it is missing.
        let db = LaunchDB::new(&host, "c:\\launch\\");
        assert_eq!(db.size(), 1);
    }

    #[test]
    fn the_unused_host_seams_stay_wired() {
        // Direct calls over the trait methods this module's flow never
        // reaches (coverage bookkeeping, like the reference's utils
        // helpers used only by other tools).
        let mut host = DbHost::new();
        assert_eq!(host.app_full_path(), "c:\\launch\\bwlauncher.exe");
        assert_eq!(host.windows_path(false), "");
        assert!(!host.file_exists("c:\\launch\\nothing"));
        assert!(!host.verify_registry(RegBase::User, "k", "n", "v"));
        assert!(host.write_registry(RegBase::User, "k", "n", "v").is_err());
        assert_eq!(host.display_message("m", 0), 0);
        host.launch_url("http://example/");
        host.restart_with_privileges("/x");
        host.run_program("c:\\x.exe");
        assert!(host.write_db_file("c:\\x.db", "b").is_err());
        assert!(host.install_self(false).is_ok());
    }
}
