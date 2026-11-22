//! Where documents come from: a Git ref of a repository, or the working tree.
//!
//! The server takes the workspace's version as a ref (`HEAD`, a tag, a
//! commit), so every indexed page is pinned to exactly the content that Git
//! served at that ref.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::errors::{git_exit, DocsError};

/// One markdown file in a Git tree, with the blob id of its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub blob: String,
}

/// A checkout whose documents are read through `git show`/`git ls-tree` at a
/// ref, so nothing on disk can change what a version means.
pub struct GitSource {
    repo: PathBuf,
    git: PathBuf,
}

impl GitSource {
    pub fn new(repo: impl Into<PathBuf>) -> Self {
        Self {
            repo: repo.into(),
            git: PathBuf::from("git"),
        }
    }

    /// Runs a git command inside the repository and returns stdout.
    fn git(&self, context: &'static str, args: &[&str]) -> Result<String, DocsError> {
        let out = Command::new(&self.git)
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .stderr(Stdio::piped())
            .output()
            .map_err(DocsError::Io)?;
        if !out.status.success() {
            return Err(git_exit(context, out.status, &out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The short commit hash a ref resolves to, its index key. Git's own
    /// message for a bad ref does not repeat the ref, so we name it ourselves.
    pub fn rev(&self, reference: &str) -> Result<String, DocsError> {
        match self
            .git("rev-parse", &["rev-parse", "--short", "--verify", reference])
        {
            Ok(text) => Ok(text.trim().to_string()),
            Err(err) => Err(DocsError::Git {
                context: "rev-parse",
                detail: format!("reference {reference}: {err}"),
            }),
        }
    }

    /// Markdown files under `prefix` at the ref, paths relative to the prefix.
    pub fn list_markdown(
        &self,
        reference: &str,
        prefix: &str,
    ) -> Result<Vec<String>, DocsError> {
        Ok(self.entries(reference, prefix)?.into_iter().map(|e| e.path).collect())
    }

    /// Every entry under `prefix` at the ref with its blob id, which is the
    /// exact fingerprint of the content Git serves.
    pub fn entries(
        &self,
        reference: &str,
        prefix: &str,
    ) -> Result<Vec<TreeEntry>, DocsError> {
        let spec = format!("{reference}:{prefix}");
        let listed = self.git("ls-tree", &["ls-tree", "-r", &spec])?;
        let mut out = Vec::new();
        for line in listed.lines() {
            let Some((meta, path)) = line.split_once('\t') else {
                continue;
            };
            let Some(blob) = meta.rsplit_once(' ').map(|(_, id)| id) else {
                continue;
            };
            let rel = path.strip_prefix(prefix).unwrap_or(path);
            let rel = rel.trim_start_matches('/');
            if rel.ends_with(".md") {
                out.push(TreeEntry {
                    path: rel.to_string(),
                    blob: blob.to_string(),
                });
            }
        }
        Ok(out)
    }

    /// The file content at the ref.
    pub fn read(&self, reference: &str, path: &str) -> Result<String, DocsError> {
        let spec = format!("{reference}:{path}");
        self.git("show", &["show", &spec])
    }
}

