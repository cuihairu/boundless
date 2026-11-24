//! Port of `server/tools/remove_db/remove_db` — the 42-line Python script
//! that DBApp asks bwmachined to run when retiring obsolete secondary
//! database files left behind by crashed BaseApps.
//!
//! Verbatim semantics kept:
//! - The FIRST `--res` entry and the one after it are deleted
//!   (`del args[resIdx:resIdx+2]` — the slice clamps at the end, so a
//!   trailing `--res` deletes only itself).
//! - `filePath = args[1]` — no second entry → IndexError → exit 1.
//! - `os.stat(filePath)` — the existence check; failure → exit 1.
//! - sqlite3 (pysqlite2 fallback): connect + `SELECT * FROM tbl_checksum`
//!   executed but never fetched + close — the open/close cycle is what
//!   clears leftover journal files; failure → exit 1.
//! - `os.remove(filePath)` last; success → exit 0.
//! - The script has no error handling anywhere: any exception → traceback
//!   → exit 1.

pub const EXIT_SUCCESS: i32 = 0;
/// A Python script dying on an uncaught exception terminates with 1.
pub const EXIT_FAILURE: i32 = 1;

/// `os.stat` / `os.remove`.
pub trait RemoveDbFileSystem {
    fn stat(&self, path: &str) -> Result<(), String>;
    fn remove(&self, path: &str) -> Result<(), String>;
}

/// sqlite3.connect + execute("SELECT * FROM tbl_checksum") (never fetched)
/// + close, folded into one seam — the fetch-less execute is the point.
pub trait RemoveDbSqlite {
    fn clear_journal_files(&mut self, path: &str) -> Result<(), String>;
}

/// std::fs-backed face for real runs (the isolated-sample acceptance).
pub struct StdFileSystem;

impl RemoveDbFileSystem for StdFileSystem {
    fn stat(&self, path: &str) -> Result<(), String> {
        std::fs::metadata(path)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn remove(&self, path: &str) -> Result<(), String> {
        std::fs::remove_file(path)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// The script's `main`. `args` is `sys.argv` — entry 0 is the script path.
pub fn main_face(
    args: &[String],
    fs: &dyn RemoveDbFileSystem,
    sqlite: &mut dyn RemoveDbSqlite,
) -> i32 {
    let mut args = args.to_vec();

    // if ('--res' in args): resIdx = args.index('--res'); del args[resIdx:resIdx+2]
    if let Some(res_idx) = args.iter().position(|arg| arg == "--res") {
        let end = (res_idx + 2).min(args.len());
        args.drain(res_idx..end);
    }

    // filePath = args[1] — IndexError when absent.
    let Some(file_path) = args.get(1) else {
        return EXIT_FAILURE;
    };
    let file_path = file_path.as_str();

    // stat = os.stat(filePath)
    if fs.stat(file_path).is_err() {
        return EXIT_FAILURE;
    }

    // sqlite3 connect + SELECT * FROM tbl_checksum + close.
    if sqlite.clear_journal_files(file_path).is_err() {
        return EXIT_FAILURE;
    }

    // os.remove(filePath)
    match fs.remove(file_path) {
        Ok(()) => EXIT_SUCCESS,
        Err(_) => EXIT_FAILURE,
    }
}

#[cfg(test)]
mod tests;
