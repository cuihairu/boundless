//! A real git repository standing in for a workspace, so tests exercise the
//! same Git plumbing the server uses.

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
