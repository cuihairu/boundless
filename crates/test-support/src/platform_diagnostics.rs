//! BW-27-03-03: the tool's path and version diagnostics.
//!
//! The tool runs on three platforms and talks to a server of its own version,
//! so two families of problem arrive as errors. A path written for Windows
//! (`C:\project\x.rs`, `\\share\src`) is not a path POSIX can open, and a tool
//! one minor behind the contract its server speaks cannot be trusted to read
//! its answers. Each error carries a kind — the stable code a client
//! distinguishes by — and a message that names the platform and the versions
//! involved, so a person reads why it failed, not just that it did.

/// The platform a diagnostic came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Windows: drive letters, backslashes, UNC shares.
    Windows,
    /// macOS: POSIX paths, case-sensitive by default.
    Macos,
    /// Linux: POSIX paths.
    Linux,
}

/// The stable kinds a tool error comes in, so a client distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// A path mixes backslash and slash separators.
    MixedSeparators,
    /// A Windows drive letter on a platform that has none.
    WindowsDriveOnPosix,
    /// A backslash-separated path on a POSIX platform.
    BackslashesOnPosix,
    /// A `\\server\share` path on a POSIX platform.
    UncPathOnPosix,
    /// A POSIX absolute path on Windows.
    PosixPathOnWindows,
    /// The tool speaks a newer contract than the server.
    ContractMismatch,
    /// The tool and the server disagree on their major version.
    ServerVersionMismatch,
}

/// One tool error: its kind, the platform it came from, and its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    /// The error's kind: the stable code a client distinguishes by.
    pub kind: ErrorKind,
    /// The platform the error came from.
    pub platform: Platform,
    /// The message a person reads: what failed, and on what input.
    pub message: String,
}

impl ToolError {
    /// The error's kind as a stable string, for a client to switch on.
    pub fn code(&self) -> &'static str {
        match self.kind {
            ErrorKind::MixedSeparators => "mixed-separators",
            ErrorKind::WindowsDriveOnPosix => "windows-drive-on-posix",
            ErrorKind::BackslashesOnPosix => "backslashes-on-posix",
            ErrorKind::UncPathOnPosix => "unc-path-on-posix",
            ErrorKind::PosixPathOnWindows => "posix-path-on-windows",
            ErrorKind::ContractMismatch => "contract-mismatch",
            ErrorKind::ServerVersionMismatch => "server-version-mismatch",
        }
    }
}

/// A platform's name, for the messages.
fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "Windows",
        Platform::Macos => "macOS",
        Platform::Linux => "Linux",
    }
}

/// Whether `raw` starts with a Windows drive letter (`C:`, `d:`).
fn drive_letter(raw: &str) -> bool {
    let mut bytes = raw.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphabetic()) && bytes.next() == Some(b':')
}

/// Diagnose a path the tool was asked to open, on the platform it runs on.
///
/// Returns `None` when the path has nothing wrong with it for that platform.
pub fn path_issue(platform: Platform, raw: &str) -> Option<ToolError> {
    let name = platform_name(platform);
    let windows = platform == Platform::Windows;
    let has_slash = raw.contains('/');
    let has_backslash = raw.contains('\\');

    if has_slash && has_backslash {
        return Some(ToolError {
            kind: ErrorKind::MixedSeparators,
            platform,
            message: format!(
                "{name}: path mixes / and \\ separators: {raw}; \
                 use one separator"
            ),
        });
    }
    if has_backslash {
        // Backslashes are normal on Windows — a relative path or a UNC share —
        // and an import from one on POSIX.
        if windows {
            return None;
        }
        let unc = raw.starts_with("\\\\");
        let (kind, message) = if unc {
            (
                ErrorKind::UncPathOnPosix,
                format!(
                    "{name}: path is a \\\\server\\share share, which {name} \
                     cannot open: {raw}"
                ),
            )
        } else {
            (
                ErrorKind::BackslashesOnPosix,
                format!(
                    "{name}: path uses backslash separators: {raw}; \
                     this platform wants /"
                ),
            )
        };
        return Some(ToolError {
            kind,
            platform,
            message,
        });
    }
    if !windows && drive_letter(raw) {
        return Some(ToolError {
            kind: ErrorKind::WindowsDriveOnPosix,
            platform,
            message: format!(
                "{name}: path carries a Windows drive letter {raw}, which \
                 this platform cannot open"
            ),
        });
    }
    if windows && raw.starts_with('/') && !raw.starts_with("//") {
        return Some(ToolError {
            kind: ErrorKind::PosixPathOnWindows,
            platform,
            message: format!(
                "{name}: path is a POSIX absolute path: {raw}; this \
                 platform wants a drive letter or a UNC share"
            ),
        });
    }
    None
}

/// A version, major and minor.
pub type Version = (u32, u32);

/// Diagnose a tool/server version pair. `None` when they agree enough to talk:
/// the same major, and the tool not older than the contract the server speaks.
pub fn version_issue(tool: Version, server: Version) -> Option<ToolError> {
    let text = format!("{}.{}", tool.0, tool.1);
    let server_text = format!("{}.{}", server.0, server.1);
    if tool.0 != server.0 {
        return Some(ToolError {
            kind: ErrorKind::ServerVersionMismatch,
            platform: Platform::Linux,
            message: format!(
                "the tool is {text} but the server is {server_text}; \
                 major versions must agree"
            ),
        });
    }
    if tool.1 < server.1 {
        return Some(ToolError {
            kind: ErrorKind::ContractMismatch,
            platform: Platform::Linux,
            message: format!(
                "the tool is {text} but the server speaks {server_text}; \
                 the tool is behind its contract"
            ),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_drive_path_is_unusable_on_posix() {
        let issue = path_issue(Platform::Linux, "C:\\project\\x.rs").unwrap();

        // The diagnostic names the platform and the offending path.
        assert_eq!(issue.kind, ErrorKind::BackslashesOnPosix);
        assert_eq!(issue.platform, Platform::Linux);
        assert_eq!(issue.code(), "backslashes-on-posix");
        assert!(issue.message.contains("C:\\project\\x.rs"));
        assert!(issue.message.contains("Linux"));
    }

    #[test]
    fn a_unc_share_is_unusable_on_posix() {
        let issue = path_issue(Platform::Macos, "\\\\share\\src\\main.py").unwrap();
        assert_eq!(issue.kind, ErrorKind::UncPathOnPosix);
        assert_eq!(issue.code(), "unc-path-on-posix");
        assert!(issue.message.contains("macOS"));
    }

    #[test]
    fn a_mixed_separator_path_is_reported_on_every_platform() {
        for platform in [Platform::Windows, Platform::Macos, Platform::Linux] {
            let issue = path_issue(platform, "c:/project/crates\\x.rs").unwrap();
            assert_eq!(issue.kind, ErrorKind::MixedSeparators);
            assert_eq!(issue.platform, platform);
            assert_eq!(issue.code(), "mixed-separators");
        }
    }

    #[test]
    fn a_posix_absolute_path_is_reported_on_windows() {
        let issue = path_issue(Platform::Windows, "/home/cui/app/x.rs").unwrap();
        assert_eq!(issue.kind, ErrorKind::PosixPathOnWindows);
        assert!(issue.message.contains("Windows"));
    }

    #[test]
    fn a_path_in_the_platforms_own_shape_is_fine() {
        // Nothing wrong with a path written for the platform that runs it.
        assert_eq!(path_issue(Platform::Linux, "/home/cui/work/app/x.rs"), None);
        assert_eq!(path_issue(Platform::Macos, "/Users/cui/app/x.rs"), None);
        assert_eq!(path_issue(Platform::Windows, "C:/project/x.rs"), None);
        // A UNC share and a relative backslash path are both Windows.
        assert_eq!(path_issue(Platform::Windows, "\\\\share\\x.rs"), None);
        assert_eq!(path_issue(Platform::Windows, "crates\\x.rs"), None);
    }

    #[test]
    fn a_bare_drive_letter_on_posix_is_flagged() {
        let issue = path_issue(Platform::Linux, "D:/build/app/x.rs").unwrap();
        assert_eq!(issue.kind, ErrorKind::WindowsDriveOnPosix);
        assert_eq!(issue.code(), "windows-drive-on-posix");
    }

    #[test]
    fn a_major_version_gap_is_distinguishable_from_a_contract_gap() {
        let major = version_issue((1, 0), (2, 0)).unwrap();
        assert_eq!(major.kind, ErrorKind::ServerVersionMismatch);
        assert_eq!(major.code(), "server-version-mismatch");
        // Each message names both versions, so a reader sees the gap.
        assert!(major.message.contains("1.0") && major.message.contains("2.0"));

        let contract = version_issue((1, 0), (1, 3)).unwrap();
        assert_eq!(contract.kind, ErrorKind::ContractMismatch);
        assert_eq!(contract.code(), "contract-mismatch");
        assert!(contract.message.contains("1.0") && contract.message.contains("1.3"));
    }

    #[test]
    fn an_agreeing_pair_is_fine() {
        // Same major and the tool not behind: the pair can talk.
        assert_eq!(version_issue((1, 2), (1, 2)), None);
        assert_eq!(version_issue((1, 3), (1, 2)), None);
    }
}
