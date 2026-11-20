//! A real git repository standing in for a workspace, so tests exercise the
//! same Git plumbing the server uses. Not every test file uses every helper.
#![allow(dead_code)]

use std::process::Command;

use boundless_docs_mcp::source::GitSource;

/// One temp repository, already initialised, with the documents under `docs/`.
pub struct Repo {
    _dir: tempfile::TempDir,
    pub source: GitSource,
}

impl Repo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = GitSource::new(dir.path());
        git(&dir, &["init", "-q"]);
        let repo = Self { _dir: dir, source };
        repo.commit_doc("guide/empty.md", "# Empty\n", "seed");
        repo
    }

    /// Writes or rewrites `docs/<rel>` and commits it.
    pub fn commit_doc(&self, rel: &str, text: &str, message: &str) {
        let file = self._dir.path().join("docs").join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).expect("mkdir");
        std::fs::write(&file, text).expect("write");
        git(&self._dir, &["add", "-A"]);
        git(&self._dir, &["commit", "-q", "-m", message]);
    }

    /// Tags the current commit.
    pub fn tag(&self, label: &str) {
        git(&self._dir, &["tag", label]);
    }

    /// Removes `docs/<rel>` and commits it.
    pub fn delete(&self, rel: &str, message: &str) {
        let file = self._dir.path().join("docs").join(rel);
        std::fs::remove_file(&file).expect("remove");
        git(&self._dir, &["add", "-A"]);
        git(&self._dir, &["commit", "-q", "-m", message]);
    }

    /// The repository root, for `--root`.
    pub fn path(&self) -> std::path::PathBuf {
        self._dir.path().to_path_buf()
    }
}

fn git(dir: &tempfile::TempDir, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .arg("-C")
        .arg(dir.path())
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
