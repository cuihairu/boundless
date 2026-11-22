//! Path casing, encoding and system directories (BW-15-01-06).
//!
//! The reference resolves a path the same way on both ends: the Unix
//! side concatenates its base with the relative path
//! (`unix_file_system.cpp:466`), the Windows side does that and,
//! because the volume is case-insensitive, additionally
//! [`correct_case_of_path`](WinFileSystem::correctCaseOfPath,
//! `win_file_system.cpp:782`). The environment directories come from
//! the same env lookups the reference makes when it builds its
//! resource path (`BW_RES_PATH`, `bwresource.cpp:991`).
//!
//! POSIX spellings only: Windows `%USERPROFILE%`/`%TEMP%`/`\\` and
//! `GetFullPathNameW` are BW-15-03.

use std::borrow::Cow;
use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The path separator, `'/'`.
pub const PATH_SEP: u8 = b'/';

/// `WinFileSystem::correctCaseOfPath` (`win_file_system.cpp:782`): the
/// path with the casing the OS actually has on disk.
///
/// POSIX `realpath` resolves the on-disk casing on a case-insensitive
/// volume (macOS's default APFS) and returns the path untouched on a
/// case-sensitive one (Linux), which is where the reference does no
/// case checking at all. A path that does not exist comes back
/// unchanged, exactly as the reference documents.
pub fn correct_case_of_path(path: &Path) -> PathBuf {
    real_path(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `realpath(3)`: the canonical form of `path` -- symlinks resolved,
/// so the casing and the spelling are what the kernel has on disk.
///
/// The error is the kernel's (`ENOENT`, `ELOOP` for a symlink cycle,
/// ...) with its OS code preserved.
pub fn real_path(path: &Path) -> io::Result<PathBuf> {
    // A POSIX path can never hold a NUL, so the embedded-NUL error is
    // unreachable but kept for the signature's honesty.
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;

    // SAFETY: `c_path` is a valid NUL-terminated C string; a null
    // result buffer makes `realpath` allocate, which we free below.
    let resolved = unsafe { libc::realpath(c_path.as_ptr(), std::ptr::null_mut()) };
    if resolved.is_null() {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `realpath` hands back a `strlen`-able NUL-terminated
    // allocation owned by the caller.
    let len = unsafe { libc::strlen(resolved) };
    // Copy the bytes into a path owned by us *before* releasing the
    // allocation; a `PathBuf` carries its own copy.
    let path = PathBuf::from(OsStr::from_bytes(unsafe {
        std::slice::from_raw_parts(resolved as *const u8, len)
    }));
    unsafe { libc::free(resolved as *mut _) };
    Ok(path)
}

/// `getAbsolutePath` (`unix_file_system.cpp:466`): the base with the
/// rest appended, adding `'/'` when the base does not already end with
/// one (`UnixFileSystem`'s constructor guarantees it).
pub fn join(base: &Path, rest: &Path) -> PathBuf {
    let mut out: Vec<u8> = base.as_os_str().as_bytes().to_vec();
    let rest = rest.as_os_str().as_bytes();
    let last = out.last().copied();
    if !out.is_empty() && last != Some(PATH_SEP) && rest.first() != Some(&PATH_SEP) {
        out.push(PATH_SEP);
    }
    out.extend_from_slice(rest);
    PathBuf::from(OsStr::from_bytes(&out))
}

/// A POSIX absolute path starts with `'/'`.
pub fn is_absolute(path: &Path) -> bool {
    path.as_os_str().as_bytes().first() == Some(&PATH_SEP)
}

/// The home directory, `$HOME`. The reference's user-data directory
/// comes from the same environment (`USERPROFILE` on Windows,
/// BW-15-03). `None` when it is not set.
pub fn home_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home))
}

/// The scratch directory, `$TMPDIR` first as POSIX asks, then `$TMP`/
/// `$TEMP`, then `/tmp`.
pub fn temp_dir() -> PathBuf {
    for var in ["TMPDIR", "TMP", "TEMP"] {
        if let Some(dir) = std::env::var_os(var)
            && !dir.is_empty()
        {
            return PathBuf::from(dir);
        }
    }
    PathBuf::from("/tmp")
}

/// The path's bytes, the encoding-neutral form: a POSIX path is a
/// byte string and does not have to be valid UTF-8.
pub fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

/// The path as text, lossy: invalid UTF-8 sequences become the
/// replacement character instead of an error.
pub fn path_to_string_lossy(path: &Path) -> Cow<'_, str> {
    path.to_string_lossy()
}

/// Rebuild a path from arbitrary bytes, the inverse of [`path_bytes`].
pub fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

/// Rebuild a path from an OS-native string.
pub fn path_from_os_string(text: OsString) -> PathBuf {
    PathBuf::from(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn fixture(name: &str) -> PathBuf {
        // A fixture in the system scratch directory: the
        // representative case "the fixture is addressable".
        join(&temp_dir(), Path::new(name))
    }

    #[test]
    fn temp_dir_holds_an_addressable_fixture() {
        let path = fixture("boundless_path_fixture.txt");
        fs::write(&path, b"addressable").expect("write the fixture");
        assert_eq!(fs::read(&path).expect("read the fixture"), b"addressable");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn home_dir_is_absolute_when_set() {
        let Some(home) = home_dir() else {
            return; // the runner gave no HOME
        };
        assert!(is_absolute(&home));
        // Resolving the real spelling of a path under it must succeed,
        // which is what "the fixture is addressable" means here.
        assert!(real_path(&home).is_ok());
    }

    #[test]
    fn join_inserts_the_separator_only_when_missing() {
        assert_eq!(
            join(Path::new("/base"), Path::new("data")),
            Path::new("/base/data")
        );
        assert_eq!(
            join(Path::new("/base/"), Path::new("data")),
            Path::new("/base/data"),
            "no doubled separator"
        );
        assert_eq!(join(Path::new(""), Path::new("data")), Path::new("data"));
        assert_eq!(
            join(Path::new("relative"), Path::new("/absolute")),
            Path::new("relative/absolute"),
        );
    }

    #[test]
    fn absolute_paths_start_with_a_slash() {
        assert!(is_absolute(Path::new("/tmp/x")));
        assert!(!is_absolute(Path::new("tmp/x")));
        assert!(!is_absolute(Path::new("")));
        assert_eq!(PATH_SEP, b'/');
    }

    #[test]
    fn real_path_resolves_the_spelling_on_disk() {
        let path = fixture("boundless_realpath_fixture");
        fs::create_dir_all(&path).expect("create the fixture");

        let canonical = real_path(&path).expect("the fixture exists");
        assert_eq!(
            real_path(&canonical).expect("the canonical spelling is real"),
            canonical,
            "a resolved path is a fixed point"
        );

        // A symlink is resolved to what it names.
        let link = fixture("boundless_realpath_link");
        fs::remove_file(&link).ok();
        symlink(&path, &link).expect("symlink to the fixture");
        assert_eq!(real_path(&link).expect("the link"), canonical);

        // A missing path is a real error, not a silent guess.
        let missing = real_path(&fixture("boundless_missing_fixture"));
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);

        let _ = fs::remove_file(&link);
        let _ = fs::remove_dir(&path);
    }

    #[test]
    fn correct_case_of_path_leaves_a_missing_path_alone() {
        // `win_file_system.cpp:782`: "If the path does not exist then
        // it is returned."
        let missing = Path::new("/nope/does/not/exist");
        assert_eq!(
            correct_case_of_path(missing),
            Path::new("/nope/does/not/exist")
        );

        let present = fixture("boundless_case_fixture");
        fs::write(&present, b"x").expect("write");
        assert_eq!(
            correct_case_of_path(&present),
            real_path(&present).expect("exists")
        );
        let _ = fs::remove_file(&present);
    }

    #[test]
    fn path_bytes_round_trip_without_needing_utf8() {
        // A POSIX path is bytes: a 0xFF byte is a legal filename
        // character even though it is not valid UTF-8.
        let bytes: &[u8] = b"/tmp/\xff\xff-name";
        let path = path_from_bytes(bytes);
        assert_eq!(path_bytes(&path), bytes);

        let lossy = path_to_string_lossy(&path);
        assert_eq!(lossy.chars().filter(|&c| c == '\u{FFFD}').count(), 2);

        let round = path_from_os_string(OsString::from("/tmp/x"));
        assert_eq!(path_bytes(&round), b"/tmp/x");
    }
}
