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
    /// A version the index does not carry.
    Version(String),
}

impl fmt::Display for DocsError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocsError::Git { context, detail } => write!(out, "git {context}: {detail}"),
            DocsError::Io(err) => write!(out, "io: {err}"),
            DocsError::Json(err) => write!(out, "index json: {err}"),
            DocsError::Version(version) => write!(out, "docs version: {version}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn every_failure_names_what_it_is() {
        assert_eq!(
            DocsError::Git {
                context: "rev-parse",
                detail: "reference v9: exit code 128".to_string(),
            }
            .to_string(),
            "git rev-parse: reference v9: exit code 128"
        );
        assert_eq!(
            DocsError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "no index"))
                .to_string(),
            "io: no index"
        );
        assert!(
            DocsError::Json(serde_json::from_str::<serde_json::Value>("{").unwrap_err())
                .to_string()
                .starts_with("index json: ")
        );
        assert_eq!(
            DocsError::Version("v9".into()).to_string(),
            "docs version: v9"
        );
    }

    #[test]
    fn the_platform_failures_become_this_error() {
        let io: DocsError = std::io::Error::other("gone").into();
        assert!(io.to_string().starts_with("io:"));
        let json: DocsError = serde_json::from_str::<serde_json::Value>("nope").unwrap_err().into();
        assert!(json.to_string().starts_with("index json:"));
    }

    #[test]
    fn a_git_failure_without_a_message_still_names_the_exit_code() {
        let status = std::process::ExitStatus::from_raw(128 << 8);
        let err = git_exit("show", status, b"");
        assert_eq!(err.to_string(), "git show: exit code 128");
    }

    #[test]
    fn a_git_failure_with_a_message_carries_it() {
        let status = std::process::ExitStatus::from_raw(1 << 8);
        let err = git_exit("ls-tree", status, b" fatal: bad thing\n");
        assert_eq!(err.to_string(), "git ls-tree: exit code 1: fatal: bad thing");
    }
}
