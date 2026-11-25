//! BW-13-05-05: the BigWorld launcher, a port of the reference
//! `tools/bwlauncher/` (`bwlauncher.cpp`, `launch_db.*`, `utils.*`,
//! `bwlauncher_config.*`).
//!
//! The launcher is the `bigworld://` protocol handler: it registers a game
//! executable under a game ID (`.db` files dropped beside the launcher),
//! then on a `bigworld://launch/<gameID>/<user>/<pass>` URL launches that
//! executable with the credentials on its command line.
//!
//! Everything raw-Win32 (registry, `ShellExecute`, `MessageBox`, CSIDL
//! paths, self-delete batch) is behind the [`LauncherHost`] seam; the
//! portable core is the command-line tokenizer ([`split_command_args`]),
//! the URL grammar ([`split_url`], [`split_string`],
//! [`replace_sub_strings`]), the [`LaunchDB`] reader and the dispatch /
//! install-decision flow ([`process_protocol_command`], [`w_win_main`]).
//!
//! Reference files: `tools/bwlauncher/{bwlauncher,launch_db,utils}.cpp`.

pub mod launch_db;
pub mod utils;

pub use launch_db::LaunchDB;
pub use utils::{
    find_arg, get_base_path, get_filename, got_arg, replace_sub_strings, split_command_args,
    split_command_args_default, split_string, split_url,
};

/// Launcher-wide constants (`bwlauncher_config.hpp|.cpp`).
pub mod config {
    /// `BWLAUNCHER_VERSION`.
    pub const LAUNCHER_VERSION: &str = "100";
    /// `BWLAUNCHER_EXE_NAME`.
    pub const LAUNCHER_EXE_NAME: &str = "bwlauncher.exe";
    /// `BWLAUNCHER_UNINST_BAT`.
    pub const LAUNCHER_UNINST_BAT: &str = "bwlauncher_uninst.bat";
    /// `BW_GAMES_URL`.
    pub const BW_GAMES_URL: &str = "http://games.bigworldtech.com:8080/";
    /// `BW_GAMES_LIBRARY_URL` (the `%(GameID)` tag is substituted).
    pub const BW_GAMES_LIBRARY_URL: &str = "http://games.bigworldtech.com:8080/library/%(GameID)/";
    /// The protocol scheme installed into the registry.
    pub const PROTOCOL_SCHEME: &str = "bigworld";
}

use utils::ArgVector;

/// The registry hive (`HKEY_LOCAL_MACHINE` vs `HKEY_CURRENT_USER`); the
/// all-users install writes the machine hive, the per-user install the
/// user hive (`bwlauncher.cpp:208`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegBase {
    /// `HKEY_LOCAL_MACHINE`.
    Machine,
    /// `HKEY_CURRENT_USER`.
    User,
}

/// The Win32 faces of the launcher (`ShellExecute`, registry, message box,
/// app paths), abstracted so the dispatch flow stays testable.
///
/// Mirrors the free functions in the reference `utils.cpp`; the registry
/// tree is modelled as an owned map (`Software\Classes\bigworld\...`), the
/// `runas`/`open` verbs collapse to [`LauncherHost::run_escaped`] +
/// [`LauncherHost::run_program`].
pub trait LauncherHost {
    /// The full path of the running executable including the file name
    /// (`getAppFullPath`, lower-cased as `_tcslwr_s` does).
    fn app_full_path(&self) -> String;
    /// `getWindowsPath`: the resolved per/all-user app-data root, `""` when
    /// the folder cannot be resolved.
    fn windows_path(&self, all_users: bool) -> String;
    /// `fileExists` (`GetFileAttributes != INVALID_FILE_ATTRIBUTES`): any
    /// file-system entry counts, directories included.
    fn file_exists(&self, file_name: &str) -> bool;
    /// `verifyRegistry`: on the given hive, the key exists, `name` is a
    /// `REG_SZ` value on it and (when `expected` is given) matches it
    /// exactly.
    fn verify_registry(&self, base: RegBase, key: &str, name: &str, expected: &str) -> bool;
    /// `writeToRegistry` on the given hive; `Err(Unprivileged)` is the
    /// reference's `UnprivilegedException` (access denied), `Ok(false)`
    /// any other failure.
    fn write_registry(
        &mut self,
        base: RegBase,
        key: &str,
        name: &str,
        value: &str,
    ) -> Result<bool, Unprivileged>;
    /// `displayMessage`; returns the message-box result (`IDYES` = 6 etc.).
    fn display_message(&mut self, msg: &str, mb_flags: i32) -> i32;
    /// `launchURL` / `launchGameLibrary` after substitution.
    fn launch_url(&mut self, url: &str);
    /// `LaunchDB::launch`'s `ShellExecuteEx`: run `exe_path` with
    /// `--username u --password p`; `true` = executed.
    fn launch_game(&mut self, exe_path: &str, username: &str, password: &str) -> bool;
    /// `restartWithPrivileges` (`runas` verb, appends
    /// ` /restartedWithPrivileges`); `Err(())` = `UnprivilegedException`
    /// surfaced by a denied write.
    fn restart_with_privileges(&mut self, switch: &str);
    /// `runProgram` (`open` verb, hidden).
    fn run_program(&mut self, cmd: &str);
    /// Reading a directory for [`LaunchDB`] (`FindFirstFile` over `*.db`).
    /// Returns the file names (not paths) in the reference's find order.
    fn list_db_files(&self, path: &str) -> Vec<String>;
    /// Reading a `.db` file's bytes for [`LaunchDB`] (`TiXmlDocument`'s
    /// load); `None` = the file could not be read.
    fn read_file(&self, path: &str) -> Option<Vec<u8>>;
    /// `GetFileAttributes` on the exe path recorded in a `.db`: `false`
    /// means the recorded path is a directory or missing.
    fn file_attributes_are_file(&self, path: &str) -> bool;
    /// Writing the `<gameID>.db` body in [`register_game`]
    /// (`_wfopen_s`+`fprintf`); `Err(Unprivileged)` is the reference's
    /// `UnprivilegedException`.
    fn write_db_file(&mut self, path: &str, body: &str) -> Result<bool, Unprivileged>;
    /// `installSelf` (`bwlauncher.cpp:176`): the copy-self + registry
    /// install, kept behind the seam (needs real file copies);
    /// `Err(Unprivileged)` is the reference's `UnprivilegedException`
    /// (a denied `CopyFile`/registry write).
    fn install_self(&mut self, all_users: bool) -> Result<(), Unprivileged>;
}

/// Message-box flag bits used by the flow (`winuser.h` values).
pub mod mb {
    /// `MB_ICONERROR`
    pub const MB_ICONERROR: i32 = 0x10;
    /// `MB_ICONQUESTION`
    pub const MB_ICONQUESTION: i32 = 0x20;
    /// `MB_ICONEXCLAMATION`
    pub const MB_ICONEXCLAMATION: i32 = 0x30;
    /// `MB_YESNO`
    pub const MB_YESNO: i32 = 0x04;
    /// `IDYES`
    pub const IDYES: i32 = 6;
}

use mb::*;

/// `UnprivilegedException` (`utils.hpp`): a permissions problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unprivileged;

/// `getLauncherInstalledPath` (`utils.cpp:294`): app-data root + `\BigWorld`.
pub fn get_launcher_installed_path(host: &dyn LauncherHost, all_users: bool) -> String {
    let mut target_dir = host.windows_path(all_users);
    target_dir.push_str("\\BigWorld");
    target_dir
}

/// `getLauncherInstalledExePath` (`utils.cpp:305`): install dir +
/// `\bwlauncher.exe`.
pub fn get_launcher_installed_exe_path(host: &dyn LauncherHost, all_users: bool) -> String {
    let mut full_path = get_launcher_installed_path(host, all_users);
    full_path.push('\\');
    full_path.push_str(config::LAUNCHER_EXE_NAME);
    full_path
}

/// `getProtocolShellCmd` (`utils.cpp:317`): `"exe" "%1"`.
pub fn get_protocol_shell_cmd(host: &dyn LauncherHost, all_users: bool) -> String {
    format!(
        "\"{}\" \"%1\"",
        get_launcher_installed_exe_path(host, all_users)
    )
}

/// Registry paths under `Software\Classes\bigworld` (`bwlauncher.cpp:211`).
const REG_PROTOCOL_KEY: &str = "Software\\Classes\\bigworld";
const REG_COMMAND_KEY: &str = "Software\\Classes\\bigworld\\Shell\\Open\\Command";

/// `checkInstallation` (`bwlauncher.cpp:227`): every protocol key matches
/// exactly (the empty default value, the launcher version, the `URL
/// Protocol` presence) **and** the installed exe exists on disk.
///
/// Note the reference checks the current-user and all-users installs
/// independently; a per-user check does not imply the all-users one.
pub fn check_installation(host: &dyn LauncherHost, all_users: bool) -> bool {
    let protocol_value = get_protocol_shell_cmd(host, all_users);
    let base = if all_users {
        RegBase::Machine
    } else {
        RegBase::User
    };

    host.verify_registry(base, REG_PROTOCOL_KEY, "", "")
        && host.verify_registry(
            base,
            REG_PROTOCOL_KEY,
            "Launcher Version",
            config::LAUNCHER_VERSION,
        )
        && host.verify_registry(base, REG_PROTOCOL_KEY, "URL Protocol", "")
        && host.verify_registry(base, REG_COMMAND_KEY, "", &protocol_value)
        && host.file_exists(&get_launcher_installed_exe_path(host, all_users))
}

/// `findInstalledLocation` (`bwlauncher.cpp:248`): current user first,
/// then all users, else `""`.
pub fn find_installed_location(host: &dyn LauncherHost) -> String {
    if check_installation(host, false) {
        get_launcher_installed_path(host, false)
    } else if check_installation(host, true) {
        get_launcher_installed_path(host, true)
    } else {
        String::new()
    }
}

/// `registerGame` (`bwlauncher.cpp:312`): write
/// `<installedLoc>\<gameID>.db` containing `<root><exePath> path </exePath></root>`.
///
/// `write_db_file` returning `Err(())` is the reference's
/// `_wfopen_s` failure → `UnprivilegedException`.
pub fn register_game(
    host: &mut dyn LauncherHost,
    game_id: &str,
    exe_path: &str,
) -> Result<bool, Unprivileged> {
    let installed_loc = find_installed_location(host);
    if installed_loc.is_empty() {
        return Ok(false);
    }

    let db_path = format!("{}\\{}.db", installed_loc, game_id);
    let body = format!("<root>\n\t<exePath> {} </exePath>\n</root>\n", exe_path);

    // fprintf writes; _wfopen_s failure raises UnprivilegedException.
    host.write_db_file(&db_path, &body)
}

/// The self-delete batch script (`SELF_DELETE_BAT`, `bwlauncher.cpp:13`).
pub fn self_delete_bat() -> String {
    format!(
        "@echo off\n:Repeat\n@del \"{}\" > NUL\n@if exist \"{}\" goto Repeat\n@del \"{}\" > NUL\n",
        config::LAUNCHER_EXE_NAME,
        config::LAUNCHER_EXE_NAME,
        config::LAUNCHER_UNINST_BAT
    )
}

/// `processProtocolCommand` (`bwlauncher.cpp:344`).
///
/// Parses `args[1]` as a `bigworld://` URL, strips a trailing `/` and
/// dispatches `launch` / `browse`; with no URL argument it just opens
/// [`config::BW_GAMES_URL`].
pub fn process_protocol_command(host: &mut dyn LauncherHost, args: &ArgVector) -> bool {
    let app_path = {
        let full = host.app_full_path();
        get_base_path(&full)
    };
    let db = LaunchDB::new(host, &app_path);

    if args.len() > 1 {
        // make sure the URL does not end in a / to make parsing easier.
        let mut url = args[1].clone();
        if url.ends_with('/') {
            url.truncate(url.len() - 1);
        }

        let (cmd, remainder) = split_url(&url);
        let url_parts = split_string(&remainder, "/");

        if cmd == "launch" {
            if url_parts.len() < 3 {
                host.display_message("Invalid arguments provided to launch.", MB_ICONERROR);
            }

            // Reference indexing: the guard above does not return, and the
            // `urlParts[0..3]` reads that follow are out of bounds in C++
            // when the URL is short — undefined behaviour no port can
            // reproduce. The port stops after the message.
            if url_parts.len() >= 3 && !db.launch(host, &url_parts[0], &url_parts[1], &url_parts[2])
            {
                launch_game_library(host, &url_parts[0]);
            }
        } else if cmd == "browse" {
            if url_parts.first().is_none_or(|s| s.is_empty()) {
                host.display_message("No game ID provided.", MB_ICONERROR);
            } else {
                launch_game_library(host, &url_parts[0]);
            }
        } else {
            host.display_message(&format!("Unknown command '{}'", cmd), MB_ICONERROR);
        }
    } else {
        // Just show the web page if nothing was provided.
        let url = config::BW_GAMES_URL.to_string();
        host.launch_url(&url);
    }

    false
}

/// `launchGameLibrary` (`utils.cpp:49`): substitute `%(GameID)` into
/// [`config::BW_GAMES_LIBRARY_URL`] and open it.
pub fn launch_game_library(host: &mut dyn LauncherHost, game_id: &str) {
    let mut tags = std::collections::BTreeMap::new();
    tags.insert("%(GameID)".to_string(), game_id.to_string());
    let browse_url = replace_sub_strings(config::BW_GAMES_LIBRARY_URL, &tags);
    host.launch_url(&browse_url);
}

/// The reference `wWinMain` decision flow (`bwlauncher.cpp:37`).
///
/// `/installCurrent` has **no** exception guard in the reference: an
/// `UnprivilegedException` from `installSelf(false)` escapes `wWinMain`
/// and terminates the process — ported as a panic. The `/install`/
/// `/installAll` leg catches it and either re-launches itself elevated
/// (without `/restartedWithPrivileges`) or shows the error box.
pub fn w_win_main(host: &mut dyn LauncherHost, args: &ArgVector) {
    if got_arg("/installCurrent", args) {
        // installSelf(false) + optional /runAfterInstall. The reference
        // does not catch here: the exception terminates the process.
        if host.install_self(false).is_err() {
            panic!("uncaught UnprivilegedException");
        }
        if got_arg("/runAfterInstall", args) {
            let path = get_launcher_installed_exe_path(host, false);
            host.run_program(&path);
        }
    } else if got_arg("/install", args) || got_arg("/installAll", args) {
        match host.install_self(true) {
            Ok(_) => {
                if got_arg("/runAfterInstall", args) {
                    let path = get_launcher_installed_exe_path(host, true);
                    host.run_program(&path);
                }
            }
            Err(Unprivileged) => {
                if !got_arg("/restartedWithPrivileges", args) {
                    host.restart_with_privileges("/installAll");
                } else {
                    host.display_message(
                        "Error installing BigWorld Launcher, could not gain required privileges.",
                        MB_ICONERROR,
                    );
                }
            }
        }
    } else if got_arg("/registerGame", args) {
        if !check_installation(host, true) && !check_installation(host, false) {
            host.display_message(
                "Cannot register a game because the BigWorld launcher is not properly installed.",
                MB_ICONERROR,
            );
        } else {
            // Get the next two args, which should be the game ID and the
            // full path to the executable.
            let idx = find_arg("/registerGame", args);
            let idx = idx.expect("got_arg checked above");

            // `idx+2 >= args.size()` (not just the count after idx) rejects.
            if idx + 2 >= args.len() {
                host.display_message(
                    "Usage: /registerGame gameID executablePath.",
                    MB_ICONEXCLAMATION,
                );
            } else {
                let game_id = args[idx + 1].clone();
                let exe_path = args[idx + 2].clone();

                match register_game(host, &game_id, &exe_path) {
                    Ok(true) => {}
                    Ok(false) => {
                        // findInstalledLocation failed: reference DEBUG_MSGs.
                    }
                    Err(Unprivileged) => {
                        // UnprivilegedException from _wfopen_s.
                        if !got_arg("/restartedWithPrivileges", args) {
                            host.restart_with_privileges(&format!(
                                "/registerGame \"{}\" \"{}\"",
                                game_id, exe_path
                            ));
                        } else {
                            host.display_message(
                                "Error installing BigWorld Launcher, could not gain required privileges.",
                                MB_ICONERROR,
                            );
                        }
                    }
                }
            }
        }
    } else if !check_installation(host, true) && !check_installation(host, false) {
        let ret = host.display_message(
            "Do you want to install the BigWorld launcher?",
            MB_ICONQUESTION | MB_YESNO,
        );
        if ret == IDYES {
            host.restart_with_privileges("/installAll /runAfterInstall");
        }
    } else {
        process_protocol_command(host, args);
    }
}

#[cfg(test)]
mod tests;
