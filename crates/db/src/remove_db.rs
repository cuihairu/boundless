//! The `remove_db` tool face — `server/tools/remove_db/remove_db`
//! (BW-09-05-06), a Python script in the reference.
//!
//! DBApp dispatches it through bwmachined (`sendRemoveDBCmd`,
//! dbapp.cpp:1650-1660) to delete obsolete secondary databases left
//! behind by crashed BaseApps. The script's whole contract: strip the
//! first `--res <value>` pair, take the one positional path, and
//! — after a throwaway `SELECT * FROM tbl_checksum` that clears any
//! stale journal files — remove the file. Every failure is a Python
//! traceback and exit status 1, so this port speaks purely in exit
//! codes and never touches the file on failure.

/// The script's exit codes: `main()` returning `None` is 0, a traceback
/// is 1.
pub const SUCCESS: i32 = 0;
/// The `EXIT_FAILURE` half of the pair.
pub const FAILURE: i32 = 1;

/// The script body between the argument strip and `os.remove`
/// (`remove_db:24-33`): the file must exist and read as a secondary
/// database — a valid SQLite file with a `tbl_checksum` table — before
/// it may be removed.
pub fn remove_database(path: &str) -> bool {
    // `stat = os.stat(filePath)` — a missing file dies here, leaving
    // nothing to clean up.
    if std::fs::metadata(path).is_err() {
        return false;
    }
    // Open and close the database to get rid of any journal files. The
    // script only executes the SELECT — it never fetches — so an empty
    // table is fine; only an execution failure dies.
    let touched = (|| -> Result<(), rusqlite::Error> {
        let connection = rusqlite::Connection::open(path)?;
        let mut statement = connection.prepare("SELECT * FROM tbl_checksum")?;
        let mut rows = statement.query([])?;
        rows.next().map(|_| ())
    })();
    if touched.is_err() {
        return false;
    }
    // Remove the file.
    std::fs::remove_file(path).is_ok()
}

/// The script's `main` (`remove_db:14-38`). `args` excludes `argv[0]`.
/// The `--res` strip deletes the pair with Python's slice `del` — the
/// element after `--res` goes too, even when it is the path, and a
/// trailing lone `--res` takes only itself. Anything left over beyond
/// the path is ignored, as the script ignores it.
pub fn run(args: &[String]) -> i32 {
    let mut args = args.to_vec();
    if let Some(index) = args.iter().position(|arg| arg == "--res") {
        let end = (index + 2).min(args.len());
        args.drain(index..end);
    }
    match args.first() {
        Some(path) if remove_database(path) => SUCCESS,
        _ => FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One temporary file per call: the tests run in parallel threads.
    static TEMP_FILE: AtomicUsize = AtomicUsize::new(0);

    fn unique_path() -> String {
        let index = TEMP_FILE.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir()
            .join(format!("db-remove-db-{}-{}", std::process::id(), index))
            .to_str()
            .unwrap()
            .to_owned()
    }

    /// A real secondary database file, schema as the baseapp writes it.
    fn secondary_file() -> String {
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute("CREATE TABLE tbl_checksum (sm_checksum TEXT)", [])
            .unwrap();
        drop(connection);
        path
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_real_secondary_database_is_removed() {
        let path = secondary_file();
        assert_eq!(run(&args(&[&path])), SUCCESS);
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn a_missing_file_fails_without_a_trace() {
        let path = unique_path();
        assert_eq!(run(&args(&[&path])), FAILURE);
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn a_file_that_is_not_a_database_survives() {
        // `sqlite3.connect` opens garbage happily; the SELECT is what
        // dies — and `os.remove` is never reached.
        let path = unique_path();
        std::fs::write(&path, b"this is not a database at all").unwrap();
        assert_eq!(run(&args(&[&path])), FAILURE);
        assert!(std::path::Path::new(&path).exists());
    }

    #[test]
    fn a_database_without_a_checksum_table_survives() {
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute("CREATE TABLE tbl_flip (sm_dbID INTEGER)", [])
            .unwrap();
        drop(connection);
        assert_eq!(run(&args(&[&path])), FAILURE);
        assert!(std::path::Path::new(&path).exists());
    }

    #[test]
    fn an_empty_file_survives() {
        // A zero-byte file reads as an empty database: no checksum table.
        let path = unique_path();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(run(&args(&[&path])), FAILURE);
        assert!(std::path::Path::new(&path).exists());
    }

    #[test]
    fn the_res_pair_is_stripped_before_the_path_is_taken() {
        let path = secondary_file();
        assert_eq!(run(&args(&["--res", "/game/res", &path])), SUCCESS);
        assert!(!std::path::Path::new(&path).exists());

        // The strip happens first, wherever `--res` sits.
        let path = secondary_file();
        assert_eq!(run(&args(&[&path, "--res", "/game/res"])), SUCCESS);
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn a_res_pair_can_swallow_the_path_itself() {
        // Python's slice `del` takes the element after `--res` without
        // asking what it is: the path never survives, and the script
        // fails with nothing removed.
        let path = secondary_file();
        assert_eq!(run(&args(&["--res", &path])), FAILURE);
        assert!(std::path::Path::new(&path).exists());

        // A trailing lone `--res` takes only itself.
        let path = secondary_file();
        assert_eq!(run(&args(&[&path, "--res"])), SUCCESS);
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn no_path_at_all_fails() {
        assert_eq!(run(&args(&[])), FAILURE);
    }
}
