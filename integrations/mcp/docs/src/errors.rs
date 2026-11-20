//! The error type shared by every layer of the index and server.

use std::fmt;
use std::process::ExitStatus;

/// Anything that can go wrong while building or querying the index.
#[derive(Debug)]
pub enum DocsError {
    /// A git invocation failed (bad ref, missing repo, ...).
    Git {
        context: &'static str,
        detail: String,
    },
    /// A filesystem operation failed.
    Io(std::io::Error),
    /// The index file could not be parsed.
    Json(serde_json::Error),
}

impl fmt::Display for DocsError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocsError::Git { context, detail } => write!(out, "git {context}: {detail}"),
            DocsError::Io(err) => write!(out, "io: {err}"),
            DocsError::Json(err) => write!(out, "index json: {err}"),
        }
    }
}

impl std::error::Error for DocsError {}

impl From<std::io::Error> for DocsError {
    fn from(err: std::io::Error) -> Self {
        DocsError::Io(err)
    }
}

impl From<serde_json::Error> for DocsError {
    fn from(err: serde_json::Error) -> Self {
        DocsError::Json(err)
    }
}

/// A non-zero exit of the git binary.
pub fn git_exit(context: &'static str, status: ExitStatus, stderr: &[u8]) -> DocsError {
    let code = status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let detail = if stderr.is_empty() {
        format!("exit code {code}")
    } else {
        format!("exit code {code}: {stderr}")
    };
    DocsError::Git { context, detail }
}
