//! Tests for the dispatch flow (`wWinMain`, `processProtocolCommand`,
//! `registerGame`, `checkInstallation`).

use super::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

/// A scripted host: files on a fake disk, registries as maps, every
/// side effect recorded.
struct FlowHost {
    files: RefCell<BTreeMap<String, Vec<u8>>>,
    directories: RefCell<BTreeSet<String>>,
    /// Registry per hive (`HKEY_CURRENT_USER` / `HKEY_LOCAL_MACHINE`).
    registry_user: RefCell<BTreeMap<(String, String), String>>,
    registry_machine: RefCell<BTreeMap<(String, String), String>>,
    messages: RefCell<Vec<(String, i32)>>,
    urls: RefCell<Vec<String>>,
    runs: RefCell<Vec<(String, String)>>,
    launched: RefCell<Vec<(String, String, String)>>,
    restarts: RefCell<Vec<String>>,
    installed: RefCell<Vec<bool>>,
    /// The per-user / all-users app-data roots (`CSIDL_LOCAL_APPDATA` /
    /// `CSIDL_COMMON_APPDATA`).
    app_data_user: String,
    app_data_machine: String,
    /// Answer for the "install?" question.
    yesno_answer: i32,
}

impl FlowHost {
    fn new() -> Self {
        FlowHost {
            files: RefCell::new(BTreeMap::new()),
            directories: RefCell::new(BTreeSet::new()),
            registry_user: RefCell::new(BTreeMap::new()),
            registry_machine: RefCell::new(BTreeMap::new()),
            messages: RefCell::new(Vec::new()),
            urls: RefCell::new(Vec::new()),
            runs: RefCell::new(Vec::new()),
            launched: RefCell::new(Vec::new()),
            restarts: RefCell::new(Vec::new()),
            installed: RefCell::new(Vec::new()),
            app_data_user: "c:\\users\\me\\appdata".to_string(),
            app_data_machine: "c:\\programdata".to_string(),
            yesno_answer: mb::IDYES,
        }
    }

    fn put(&self, path: &str, body: &str) {
        self.files
            .borrow_mut()
            .insert(path.to_string(), body.as_bytes().to_vec());
    }

    fn app_data(&self, all_users: bool) -> &str {
        if all_users {
            &self.app_data_machine
        } else {
            &self.app_data_user
        }
    }

    fn registry(&self, base: RegBase) -> &RefCell<BTreeMap<(String, String), String>> {
        match base {
            RegBase::User => &self.registry_user,
            RegBase::Machine => &self.registry_machine,
        }
    }

    /// Install the launcher "properly" into the fake hive + disk.
    fn install(&self, all_users: bool) {
        self.installed.borrow_mut().push(all_users);
        let exe = format!(
            "{}\\BigWorld\\{}",
            self.app_data(all_users),
            config::LAUNCHER_EXE_NAME
        );
        self.put(&exe, "binary");
        let cmd = format!("\"{}\" \"%1\"", exe);
        let mut reg = self
            .registry(if all_users {
                RegBase::Machine
            } else {
                RegBase::User
            })
            .borrow_mut();
        reg.insert((REG_PROTOCOL_KEY.to_string(), String::new()), String::new());
        reg.insert(
            (REG_PROTOCOL_KEY.to_string(), "Launcher Version".to_string()),
            config::LAUNCHER_VERSION.to_string(),
        );
        reg.insert(
            (REG_PROTOCOL_KEY.to_string(), "URL Protocol".to_string()),
            String::new(),
        );
        reg.insert((REG_COMMAND_KEY.to_string(), String::new()), cmd);
    }
}

impl LauncherHost for FlowHost {
    fn app_full_path(&self) -> String {
        "c:\\launch\\bwlauncher.exe".to_string()
    }
    fn windows_path(&self, all_users: bool) -> String {
        self.app_data(all_users).to_string()
    }
    fn file_exists(&self, file_name: &str) -> bool {
        self.files.borrow().contains_key(file_name) || self.directories.borrow().contains(file_name)
    }
    fn verify_registry(&self, base: RegBase, key: &str, name: &str, expected: &str) -> bool {
        match self
            .registry(base)
            .borrow()
            .get(&(key.to_string(), name.to_string()))
        {
            Some(v) => v == expected,
            None => false,
        }
    }
    fn write_registry(
        &mut self,
        base: RegBase,
        key: &str,
        name: &str,
        value: &str,
    ) -> Result<bool, Unprivileged> {
        self.registry(base)
            .borrow_mut()
            .insert((key.to_string(), name.to_string()), value.to_string());
        Ok(true)
    }
    fn display_message(&mut self, msg: &str, flags: i32) -> i32 {
        self.messages.borrow_mut().push((msg.to_string(), flags));
        self.yesno_answer
    }
    fn launch_url(&mut self, url: &str) {
        self.urls.borrow_mut().push(url.to_string());
    }
    fn launch_game(&mut self, exe_path: &str, username: &str, password: &str) -> bool {
        self.launched.borrow_mut().push((
            exe_path.to_string(),
            username.to_string(),
            password.to_string(),
        ));
        true
    }
    fn restart_with_privileges(&mut self, switch: &str) {
        self.restarts
            .borrow_mut()
            .push(format!("{} /restartedWithPrivileges", switch));
    }
    fn run_program(&mut self, cmd: &str) {
        self.runs
            .borrow_mut()
            .push((cmd.to_string(), String::new()));
    }
    fn list_db_files(&self, path: &str) -> Vec<String> {
        self.files
            .borrow()
            .keys()
            .filter(|k| k.starts_with(path) && k.ends_with(".db"))
            .map(|k| k[path.len()..].to_string())
            .collect()
    }
    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        self.files.borrow().get(path).cloned()
    }
    fn file_attributes_are_file(&self, path: &str) -> bool {
        self.files.borrow().contains_key(path) && !self.directories.borrow().contains(path)
    }
    fn write_db_file(&mut self, path: &str, body: &str) -> Result<bool, Unprivileged> {
        self.files
            .borrow_mut()
            .insert(path.to_string(), body.as_bytes().to_vec());
        Ok(true)
    }
    fn install_self(&mut self, all_users: bool) -> Result<(), Unprivileged> {
        self.install(all_users);
        Ok(())
    }
}

fn args_of(line: &str) -> ArgVector {
    let mut args = ArgVector::new();
    split_command_args_default(line, &mut args);
    args
}

// ---------------------------------------------------------------- wWinMain

#[test]
fn uninstalled_flow_offers_the_install_question() {
    let mut host = FlowHost::new();
    host.yesno_answer = mb::IDYES;

    w_win_main(&mut host, &args_of("bwlauncher.exe"));

    assert!(host.installed.borrow().is_empty());
    assert_eq!(
        host.restarts.borrow().as_slice(),
        ["/installAll /runAfterInstall /restartedWithPrivileges"]
    );
    assert_eq!(
        host.messages.borrow()[0].0,
        "Do you want to install the BigWorld launcher?"
    );
}

#[test]
fn declining_the_install_question_stops_the_flow() {
    let mut host = FlowHost::new();
    host.yesno_answer = 7; // IDNO

    w_win_main(&mut host, &args_of("bwlauncher.exe"));

    assert!(host.restarts.borrow().is_empty());
    assert!(host.urls.borrow().is_empty());
}

#[test]
fn installed_flow_dispatches_the_protocol_url() {
    let mut host = FlowHost::new();
    host.install(false);
    host.put(
        "c:\\launch\\crab.db",
        "<root><exePath>c:\\games\\crab.exe</exePath></root>",
    );
    host.put("c:\\games\\crab.exe", "");

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe bigworld://launch/crab/user/pass"),
    );

    assert_eq!(
        host.launched.borrow().as_slice(),
        [(
            "c:\\games\\crab.exe".to_string(),
            "user".to_string(),
            "pass".to_string()
        )]
    );
}

#[test]
fn installed_flow_launches_the_library_on_browse() {
    let mut host = FlowHost::new();
    host.install(false);

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe bigworld://browse/crab/"),
    );

    assert_eq!(
        host.urls.borrow().as_slice(),
        ["http://games.bigworldtech.com:8080/library/crab/"]
    );
    assert!(host.launched.borrow().is_empty());
}

#[test]
fn no_url_argument_opens_the_games_page() {
    let mut host = FlowHost::new();
    host.install(false);

    w_win_main(&mut host, &args_of("bwlauncher.exe"));

    assert_eq!(host.urls.borrow().as_slice(), [config::BW_GAMES_URL]);
}

// ------------------------------------------------- processProtocolCommand

#[test]
fn launch_with_too_few_parts_reports_the_error_and_no_launch() {
    let mut host = FlowHost::new();
    host.install(false);
    host.put(
        "c:\\launch\\crab.db",
        "<root><exePath>c:\\games\\crab.exe</exePath></root>",
    );

    let args = args_of("bwlauncher.exe bigworld://launch/crab");
    process_protocol_command(&mut host, &args);

    assert_eq!(
        host.messages.borrow().as_slice(),
        [(
            "Invalid arguments provided to launch.".to_string(),
            MB_ICONERROR
        )]
    );
    // Reference quirk: the guard does not return and the following
    // `urlParts[1]`/`[2]` reads are out of bounds (undefined behaviour);
    // the port stops after the message.
    assert!(host.launched.borrow().is_empty());
    assert!(host.urls.borrow().is_empty());
}

#[test]
fn unknown_game_id_falls_back_to_the_library() {
    let mut host = FlowHost::new();
    host.install(false);

    let args = args_of("bwlauncher.exe bigworld://launch/ghost/user/pass");
    process_protocol_command(&mut host, &args);

    assert!(host.launched.borrow().is_empty());
    assert_eq!(
        host.urls.borrow().as_slice(),
        ["http://games.bigworldtech.com:8080/library/ghost/"]
    );
}

#[test]
fn browse_without_a_game_id_is_rejected() {
    let mut host = FlowHost::new();
    host.install(false);

    let args = args_of("bwlauncher.exe bigworld://browse/");
    process_protocol_command(&mut host, &args);

    assert_eq!(
        host.messages.borrow().as_slice(),
        [("No game ID provided.".to_string(), MB_ICONERROR)]
    );
}

#[test]
fn an_unknown_command_is_reported_verbatim() {
    let mut host = FlowHost::new();
    host.install(false);

    let args = args_of("bwlauncher.exe bigworld://frobnicate/crab");
    process_protocol_command(&mut host, &args);

    assert_eq!(
        host.messages.borrow().as_slice(),
        [("Unknown command 'frobnicate'".to_string(), MB_ICONERROR)]
    );
}

#[test]
fn trailing_slash_is_stripped_before_parsing() {
    let mut host = FlowHost::new();
    host.install(false);
    host.put(
        "c:\\launch\\crab.db",
        "<root><exePath>c:\\games\\crab.exe</exePath></root>",
    );
    host.put("c:\\games\\crab.exe", "");

    let args = args_of("bwlauncher.exe bigworld://launch/crab/user/pass/");
    process_protocol_command(&mut host, &args);

    assert_eq!(
        host.launched.borrow().as_slice(),
        [(
            "c:\\games\\crab.exe".to_string(),
            "user".to_string(),
            "pass".to_string()
        )]
    );
}

// --------------------------------------------------------- /registerGame

#[test]
fn register_game_writes_the_db_file() {
    let mut host = FlowHost::new();
    host.install(false);

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe /registerGame crab c:\\games\\crab.exe"),
    );

    // registerGame writes beside the found install (current user here).
    let db_path = format!("{}\\BigWorld\\crab.db", host.app_data_user);
    let body = String::from_utf8(host.files.borrow()[&db_path].clone()).unwrap();
    assert_eq!(
        body,
        "<root>\n\t<exePath> c:\\games\\crab.exe </exePath>\n</root>\n"
    );
}

#[test]
fn register_game_without_an_install_is_rejected() {
    let mut host = FlowHost::new();

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe /registerGame crab c:\\games\\crab.exe"),
    );

    assert_eq!(
        host.messages.borrow()[0].0,
        "Cannot register a game because the BigWorld launcher is not properly installed."
    );
}

#[test]
fn register_game_with_missing_args_shows_usage() {
    let mut host = FlowHost::new();
    host.install(false);

    w_win_main(&mut host, &args_of("bwlauncher.exe /registerGame crab"));

    assert_eq!(
        host.messages.borrow()[0].0,
        "Usage: /registerGame gameID executablePath."
    );
    assert_eq!(host.messages.borrow()[0].1, MB_ICONEXCLAMATION);
}

#[test]
fn register_game_denied_write_restarts_with_privileges() {
    let mut host = DenyHost {
        inner: FlowHost::new(),
        deny: DenyWhat::DbWrite,
    };
    host.inner.install(false);

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe /registerGame crab c:\\games\\crab.exe"),
    );

    assert_eq!(
        host.inner.restarts.borrow().as_slice(),
        ["/registerGame \"crab\" \"c:\\games\\crab.exe\" /restartedWithPrivileges"]
    );

    // Already-restarted: surfaces the error box instead.
    let mut host = DenyHost {
        inner: FlowHost::new(),
        deny: DenyWhat::DbWrite,
    };
    host.inner.install(false);

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe /registerGame crab c:\\games\\crab.exe /restartedWithPrivileges"),
    );

    assert!(host.inner.restarts.borrow().is_empty());
    assert_eq!(
        host.inner.messages.borrow()[0].0,
        "Error installing BigWorld Launcher, could not gain required privileges."
    );
}

// ------------------------------------------------------ checkInstallation

#[test]
fn check_installation_requires_every_registry_value_and_the_exe() {
    let host = FlowHost::new();

    // Nothing installed at all.
    assert!(!check_installation(&host, false));
    assert!(!check_installation(&host, true));

    // Full install: registry + exe.
    host.install(false);
    assert!(check_installation(&host, false));
    host.install(true);
    assert!(check_installation(&host, true));

    // Corrupt the launcher version → invalid.
    host.registry(RegBase::User).borrow_mut().insert(
        (REG_PROTOCOL_KEY.to_string(), "Launcher Version".to_string()),
        "0".to_string(),
    );
    assert!(!check_installation(&host, false));

    // Remove the exe from disk → invalid even with good registry values.
    host.install(false);
    let exe = format!(
        "{}\\BigWorld\\{}",
        host.app_data_user,
        config::LAUNCHER_EXE_NAME
    );
    host.files.borrow_mut().remove(&exe);
    assert!(!check_installation(&host, false));
}

#[test]
fn find_installed_location_prefers_current_user() {
    let host = FlowHost::new();
    assert_eq!(find_installed_location(&host), "");

    host.install(true);
    host.install(false);
    assert_eq!(
        find_installed_location(&host),
        format!("{}\\BigWorld", host.app_data_user)
    );
}

#[test]
fn find_installed_location_falls_back_to_all_users() {
    let host = FlowHost::new();
    host.install(true);

    // The hives are distinct: a machine-only install does not satisfy the
    // current-user check ("currentUser doesn't imply allUsers").
    assert!(!check_installation(&host, false));
    assert!(check_installation(&host, true));
    assert_eq!(
        find_installed_location(&host),
        format!("{}\\BigWorld", host.app_data_machine)
    );
}

#[test]
fn register_game_without_an_install_reports_no_location() {
    let mut host = FlowHost::new();

    // Direct face: the reference returns false after a DEBUG_MSG (no
    // message box on this path).
    assert_eq!(
        register_game(&mut host, "crab", "c:\\games\\crab.exe"),
        Ok(false)
    );
}

// ---------------------------------------------------------------- paths

#[test]
fn installed_paths_follow_the_windows_layout() {
    let host = FlowHost::new();
    assert_eq!(
        get_launcher_installed_path(&host, false),
        "c:\\users\\me\\appdata\\BigWorld"
    );
    assert_eq!(
        get_launcher_installed_exe_path(&host, false),
        "c:\\users\\me\\appdata\\BigWorld\\bwlauncher.exe"
    );
    assert_eq!(
        get_protocol_shell_cmd(&host, false),
        "\"c:\\users\\me\\appdata\\BigWorld\\bwlauncher.exe\" \"%1\""
    );
    assert_eq!(
        get_launcher_installed_path(&host, true),
        "c:\\programdata\\BigWorld"
    );
    assert_eq!(
        get_launcher_installed_exe_path(&host, true),
        "c:\\programdata\\BigWorld\\bwlauncher.exe"
    );
}

#[test]
fn the_self_delete_batch_loops_over_the_exe_then_the_bat() {
    assert_eq!(
        self_delete_bat(),
        "@echo off\n:Repeat\n@del \"bwlauncher.exe\" > NUL\n@if exist \"bwlauncher.exe\" goto Repeat\n@del \"bwlauncher_uninst.bat\" > NUL\n"
    );
}

// -------------------------------------------------------- /install family

#[test]
fn install_current_runs_the_user_path_after_install() {
    let mut host = FlowHost::new();

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe /installCurrent /runAfterInstall"),
    );

    assert_eq!(host.installed.borrow().as_slice(), [false]);
    assert_eq!(
        host.runs.borrow().as_slice(),
        [(
            "c:\\users\\me\\appdata\\BigWorld\\bwlauncher.exe".to_string(),
            String::new()
        )]
    );
}

#[test]
fn install_all_runs_the_all_users_path_after_install() {
    let mut host = FlowHost::new();

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe /installAll /runAfterInstall"),
    );

    assert_eq!(host.installed.borrow().as_slice(), [true]);
    assert_eq!(
        host.runs.borrow().as_slice(),
        [(
            "c:\\programdata\\BigWorld\\bwlauncher.exe".to_string(),
            String::new()
        )]
    );
}

#[test]
fn install_without_run_after_installs_only() {
    let mut host = FlowHost::new();

    w_win_main(&mut host, &args_of("bwlauncher.exe /install"));

    assert_eq!(host.installed.borrow().as_slice(), [true]);
    assert!(host.runs.borrow().is_empty());
}

#[test]
fn install_current_without_run_after_installs_only() {
    let mut host = FlowHost::new();

    w_win_main(&mut host, &args_of("bwlauncher.exe /installCurrent"));

    assert_eq!(host.installed.borrow().as_slice(), [false]);
    assert!(host.runs.borrow().is_empty());
}

/// Which seam a [`DenyHost`] refuses (the `UnprivilegedException` paths).
#[derive(Clone, Copy, PartialEq, Eq)]
enum DenyWhat {
    /// `_wfopen_s` fails in `registerGame`.
    DbWrite,
    /// `CopyFile`/registry write fails in `installSelf`.
    Install,
}

/// A delegating host that denies one seam with `UnprivilegedException`.
struct DenyHost {
    inner: FlowHost,
    deny: DenyWhat,
}

impl LauncherHost for DenyHost {
    fn app_full_path(&self) -> String {
        self.inner.app_full_path()
    }
    fn windows_path(&self, all_users: bool) -> String {
        self.inner.windows_path(all_users)
    }
    fn file_exists(&self, f: &str) -> bool {
        self.inner.file_exists(f)
    }
    fn verify_registry(&self, base: RegBase, k: &str, n: &str, e: &str) -> bool {
        self.inner.verify_registry(base, k, n, e)
    }
    fn write_registry(
        &mut self,
        base: RegBase,
        k: &str,
        n: &str,
        v: &str,
    ) -> Result<bool, Unprivileged> {
        self.inner.write_registry(base, k, n, v)
    }
    fn display_message(&mut self, m: &str, f: i32) -> i32 {
        self.inner.display_message(m, f)
    }
    fn launch_url(&mut self, u: &str) {
        self.inner.launch_url(u)
    }
    fn launch_game(&mut self, e: &str, u: &str, p: &str) -> bool {
        self.inner.launch_game(e, u, p)
    }
    fn restart_with_privileges(&mut self, s: &str) {
        self.inner.restart_with_privileges(s)
    }
    fn run_program(&mut self, c: &str) {
        self.inner.run_program(c)
    }
    fn list_db_files(&self, p: &str) -> Vec<String> {
        self.inner.list_db_files(p)
    }
    fn read_file(&self, p: &str) -> Option<Vec<u8>> {
        self.inner.read_file(p)
    }
    fn file_attributes_are_file(&self, p: &str) -> bool {
        self.inner.file_attributes_are_file(p)
    }
    fn write_db_file(&mut self, p: &str, b: &str) -> Result<bool, Unprivileged> {
        match self.deny {
            DenyWhat::DbWrite => Err(Unprivileged),
            _ => self.inner.write_db_file(p, b),
        }
    }
    fn install_self(&mut self, all_users: bool) -> Result<(), Unprivileged> {
        match self.deny {
            DenyWhat::Install => Err(Unprivileged),
            _ => self.inner.install_self(all_users),
        }
    }
}

#[test]
fn a_denied_all_users_install_restarts_elevated_once() {
    let mut host = DenyHost {
        inner: FlowHost::new(),
        deny: DenyWhat::Install,
    };

    w_win_main(&mut host, &args_of("bwlauncher.exe /installAll"));

    assert_eq!(
        host.inner.restarts.borrow().as_slice(),
        ["/installAll /restartedWithPrivileges"]
    );
    assert!(host.inner.messages.borrow().is_empty());
}

#[test]
fn a_denied_restarted_install_shows_the_privilege_error() {
    let mut host = DenyHost {
        inner: FlowHost::new(),
        deny: DenyWhat::Install,
    };

    w_win_main(
        &mut host,
        &args_of("bwlauncher.exe /installAll /restartedWithPrivileges"),
    );

    assert!(host.inner.restarts.borrow().is_empty());
    assert_eq!(
        host.inner.messages.borrow().as_slice(),
        [(
            "Error installing BigWorld Launcher, could not gain required privileges.".to_string(),
            MB_ICONERROR
        )]
    );
}

#[test]
#[should_panic(expected = "uncaught UnprivilegedException")]
fn a_denied_current_user_install_terminates_the_flow() {
    // The reference /installCurrent leg has no catch: the exception kills
    // the process.
    let mut host = DenyHost {
        inner: FlowHost::new(),
        deny: DenyWhat::Install,
    };

    w_win_main(&mut host, &args_of("bwlauncher.exe /installCurrent"));
}
