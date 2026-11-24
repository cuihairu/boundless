//! BW-09-05-06 acceptance: isolated samples (real temp files through
//! `StdFileSystem`) verify each operation and exit code of the Python
//! script port; recording doubles stand in for sqlite3 and the os module.

use super::*;
use std::cell::RefCell;

struct SqliteDouble {
    queries: Vec<String>,
    result: Result<(), String>,
}

impl Default for SqliteDouble {
    fn default() -> Self {
        Self {
            queries: Vec::new(),
            result: Ok(()),
        }
    }
}

impl RemoveDbSqlite for SqliteDouble {
    fn clear_journal_files(&mut self, path: &str) -> Result<(), String> {
        self.queries.push(path.to_string());
        self.result.clone()
    }
}

/// `os.stat`/`os.remove` recorder with per-operation overrides, so call
/// order and failures are observable without touching the disk.
struct FsDouble {
    stat_ok: bool,
    remove_ok: bool,
    calls: RefCell<Vec<String>>,
}

impl Default for FsDouble {
    fn default() -> Self {
        Self {
            stat_ok: true,
            remove_ok: true,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl RemoveDbFileSystem for FsDouble {
    fn stat(&self, path: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("stat:{}", path));
        if self.stat_ok {
            Ok(())
        } else {
            Err("missing".to_string())
        }
    }

    fn remove(&self, path: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("remove:{}", path));
        if self.remove_ok {
            Ok(())
        } else {
            Err("busy".to_string())
        }
    }
}

fn args(items: &[&str]) -> Vec<String> {
    std::iter::once("remove_db".to_string())
        .chain(items.iter().map(|item| item.to_string()))
        .collect()
}

/// Unique temp sample path (tests may run multi-threaded).
fn sample_path(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "db_remove_db_{}_{}_{}_{}",
        tag,
        std::process::id(),
        nanos,
        unique
    ))
}

fn remove_db(items: &[String], sqlite: &mut SqliteDouble) -> i32 {
    main_face(items, &StdFileSystem, sqlite)
}

#[test]
fn happy_path_removes_real_sample() {
    let path = sample_path("happy");
    std::fs::write(&path, b"stub").unwrap();
    let items = args(&[path.to_str().unwrap()]);

    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);

    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(sqlite.queries, vec![path.to_str().unwrap().to_string()]);
    assert!(!path.exists(), "sample file should be removed");
}

#[test]
fn missing_file_exits_failure_before_sqlite() {
    let path = sample_path("missing");
    let items = args(&[path.to_str().unwrap()]);

    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);

    assert_eq!(code, EXIT_FAILURE);
    assert!(sqlite.queries.is_empty());
}

#[test]
fn res_prefix_strips_both_entries() {
    let db_path = sample_path("res_db");
    std::fs::write(&db_path, b"stub").unwrap();
    let res_path = sample_path("res_dir");
    let items = args(&[
        "--res",
        res_path.to_str().unwrap(),
        db_path.to_str().unwrap(),
    ]);

    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);

    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(sqlite.queries, vec![db_path.to_str().unwrap().to_string()]);
    assert!(!db_path.exists());
}

#[test]
fn trailing_res_clamps_to_single_deletion() {
    // del args[resIdx:resIdx+2] clamps at the end, so a trailing --res
    // deletes only itself and args[1] stays the db path.
    let db_path = sample_path("trailing");
    std::fs::write(&db_path, b"stub").unwrap();
    let items = args(&[db_path.to_str().unwrap(), "--res"]);

    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);

    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(sqlite.queries, vec![db_path.to_str().unwrap().to_string()]);
    assert!(!db_path.exists());
}

#[test]
fn lone_res_leaves_index_error_exit() {
    let items = args(&["--res"]);
    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);
    assert_eq!(code, EXIT_FAILURE);
    assert!(sqlite.queries.is_empty());
}

#[test]
fn res_pair_only_leaves_index_error_exit() {
    // --res plus its value drain to nothing past argv[0].
    let items = args(&["--res", "/game/res"]);
    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);
    assert_eq!(code, EXIT_FAILURE);
    assert!(sqlite.queries.is_empty());
}

#[test]
fn no_arguments_exits_failure() {
    let items = args(&[]);
    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);
    assert_eq!(code, EXIT_FAILURE);
    assert!(sqlite.queries.is_empty());
}

#[test]
fn only_first_res_occurrence_is_drained() {
    // args.index('--res') finds the first one; the second pair survives, so
    // args[1] is the literal "--res" and stat fails.
    let items = args(&["--res", "a", "--res", "b", "/tmp/never.db"]);
    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);
    assert_eq!(code, EXIT_FAILURE);
    assert!(sqlite.queries.is_empty());
}

#[test]
fn res_match_is_exact() {
    // Python compares exact strings: "--RES" is not "--res" and lands on
    // args[1], where stat fails.
    let items = args(&["--RES", "/tmp/never.db"]);
    let mut sqlite = SqliteDouble::default();
    let code = remove_db(&items, &mut sqlite);
    assert_eq!(code, EXIT_FAILURE);
    assert!(sqlite.queries.is_empty());
}

#[test]
fn sqlite_failure_exits_failure_and_keeps_file() {
    let path = sample_path("sqlite_fail");
    std::fs::write(&path, b"stub").unwrap();
    let items = args(&[path.to_str().unwrap()]);

    let mut sqlite = SqliteDouble::default();
    sqlite.result = Err("no such table: tbl_checksum".to_string());
    let code = remove_db(&items, &mut sqlite);

    assert_eq!(code, EXIT_FAILURE);
    assert!(path.exists(), "file must survive a sqlite failure");
}

#[test]
fn operation_order_is_stat_then_sqlite_then_remove() {
    let items = args(&["/some/db"]);
    let fs = FsDouble::default();
    let mut sqlite = SqliteDouble::default();
    let code = main_face(&items, &fs, &mut sqlite);

    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        fs.calls.borrow().clone(),
        vec!["stat:/some/db".to_string(), "remove:/some/db".to_string()]
    );
    assert_eq!(sqlite.queries, vec!["/some/db".to_string()]);
}

#[test]
fn remove_failure_exits_failure() {
    let items = args(&["/some/db"]);
    let fs = FsDouble {
        remove_ok: false,
        ..FsDouble::default()
    };
    let mut sqlite = SqliteDouble::default();
    let code = main_face(&items, &fs, &mut sqlite);

    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(fs.calls.borrow().len(), 2);
}
