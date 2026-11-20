//! Telling whether an index still matches the repository it was read from.
//!
//! An index is built once and served many times, so a workspace can move on
//! without anyone rebuilding it. `freshness` compares a stored index against
//! the ref it was built at and names every way the two have diverged, which is
//! the gate CI runs instead of trusting that a build happened.

use crate::index::DocIndex;
use crate::source::GitSource;
use crate::DocsError;

/// Whether one version of an index still describes the repository.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FreshnessReport {
    /// Every way the index and the repository disagree.
    pub reasons: Vec<String>,
}

impl FreshnessReport {
    pub fn stale(&self) -> bool {
        !self.reasons.is_empty()
    }

    /// One line per reason, for a log or a CI failure message.
    pub fn describe(&self) -> Vec<String> {
        self.reasons.clone()
    }
}

/// Compares `reference` of the index with the repository at that same ref.
pub fn freshness(
    index: &DocIndex,
    source: &GitSource,
    reference: &str,
    prefix: &str,
) -> Result<FreshnessReport, DocsError> {
    let current = source.rev(reference)?;
    let mut report = FreshnessReport::default();
    let version = match index.version(reference) {
        None => {
            report
                .reasons
                .push(format!("missing version {reference}"));
            return Ok(report);
        }
        Some(version) => version,
    };
    if version.commit != current {
        report
            .reasons
            .push(format!("moved: {} -> {current}", version.commit));
    }
    let entries = source.entries(reference, prefix)?;
    let on_disk: std::collections::HashMap<&str, &str> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.blob.as_str()))
        .collect();
    let in_index: std::collections::HashMap<&str, &str> = version
        .docs
        .iter()
        .map(|doc| (doc.path.as_str(), doc.blob.as_str()))
        .collect();

    for (path, blob) in &in_index {
        match on_disk.get(path) {
            None => report.reasons.push(format!("removed: {path}")),
            Some(other) if *other != *blob => report.reasons.push(format!("changed: {path}")),
            _ => {}
        }
    }
    for path in on_disk.keys() {
        if !in_index.contains_key(path) {
            report.reasons.push(format!("added: {path}"));
        }
    }
    report.reasons.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A throw-away repository with one page, good enough to move and edit.
    struct Repo {
        dir: tempfile::TempDir,
        source: GitSource,
    }

    impl Repo {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let source = GitSource::new(dir.path());
            let repo = Self { dir, source };
            git(repo.dir.path(), &["init", "-q"]);
            repo.commit("docs/guide/page.md", "# Page\n\n## One\n\nBody.\n", "one");
            repo
        }

        fn commit(&self, path: &str, text: &str, message: &str) {
            let file = self.dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).expect("mkdir");
            std::fs::write(&file, text).expect("write");
            git(self.dir.path(), &["add", "-A"]);
            git(self.dir.path(), &["commit", "-q", "-m", message]);
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn a_rebuilt_index_is_fresh() {
        let repo = Repo::new();
        let mut index = DocIndex::default();
        index
            .add_version(&repo.source, "HEAD", "docs", &["zh"])
            .expect("index");
        let report = freshness(&index, &repo.source, "HEAD", "docs").expect("report");
        assert!(!report.stale());
    }

    #[test]
    fn an_unrebuilt_edit_is_stale() {
        let repo = Repo::new();
        let mut index = DocIndex::default();
        index
            .add_version(&repo.source, "HEAD", "docs", &["zh"])
            .expect("index");
        repo.commit("docs/guide/page.md", "# Page\n\n## Two\n\nNew body.\n", "two");
        let report = freshness(&index, &repo.source, "HEAD", "docs").expect("report");
        assert!(report.stale());
        // The ref moved as well as the page changed; both are reported.
        assert_eq!(report.reasons.len(), 2);
        assert!(report
            .reasons
            .contains(&"changed: guide/page.md".to_string()));
        assert!(report.reasons.iter().any(|reason| reason.starts_with("moved: ")));
    }

    #[test]
    fn an_unknown_version_is_stale_too() {
        let repo = Repo::new();
        let report = freshness(&DocIndex::default(), &repo.source, "HEAD", "docs").expect("report");
        assert_eq!(report.reasons, ["missing version HEAD"]);
    }
}
