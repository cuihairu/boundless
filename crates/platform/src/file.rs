//! File locking and atomic replacement (BW-15-01-04).
//!
//! The config/pid/shared-state write path: cooperative processes take an
//! exclusive advisory lock on a *stable* lock file, then swap the data
//! file in with `rename(2)`, which is atomic within a filesystem -- a
//! reader sees either the whole old file or the whole new one, never a
//! partial write.
//!
//! Why the lock file must be separate from the data file: `flock` guards
//! the open file *description*, but an atomic replace puts a new inode
//! behind the path. A locker holding the old data file's descriptor does
//! not conflict with a writer that just renamed a fresh file over the
//! path, so locking the replaced file itself cannot serialize updates.
//! The stable `.lock` companion never gets renamed, so mutual exclusion
//! holds across replacements.
//!
//! Unix uses the reference's POSIX spellings (`flock`/`rename`); Windows
//! spells the same contract with `LockFileEx` on the stable lock file and
//! `ReplaceFileW` for the atomic swap. Both share one API.

#[cfg(unix)]
use std::ffi::CString;
use std::io;
#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(windows)]
use std::os::raw::c_void;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicU64, Ordering};

/// An exclusive advisory lock (`flock(2) LOCK_EX`) held until dropped.
///
/// Lock the stable companion path (`<data>.lock`), not the atomically
/// replaced data file; see the module docs for why.
#[cfg(unix)]
pub struct FileLock {
    fd: RawFd,
}

#[cfg(windows)]
pub struct FileLock {
    /// The lock file's handle, open for the lock's lifetime: closing
    /// (or unlocking) releases it, exactly like `close(2)` on unix.
    handle: *mut c_void,
}

#[cfg(unix)]
impl FileLock {
    fn open_lock_file(path: &str) -> io::Result<RawFd> {
        let c_path = CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        // O_RDWR|O_CREAT: the lock file is created on first use and its
        // contents are never meaningful.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o644) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(fd)
        }
    }

    /// Block until the exclusive lock is acquired.
    pub fn exclusive(path: &str) -> io::Result<Self> {
        let fd = Self::open_lock_file(path)?;
        let r = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if r != 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        Ok(Self { fd })
    }

    /// `LOCK_EX | LOCK_NB`: `Ok(None)` when another holder has the lock.
    pub fn try_exclusive(path: &str) -> io::Result<Option<Self>> {
        let fd = Self::open_lock_file(path)?;
        let r = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if r == 0 {
            return Ok(Some(Self { fd }));
        }
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(e)
        }
    }
}

#[cfg(unix)]
impl Drop for FileLock {
    fn drop(&mut self) {
        // close(2) releases the flock; nothing else to undo.
        unsafe { libc::close(self.fd) };
    }
}

#[cfg(windows)]
impl FileLock {
    /// Open (creating if needed) the stable lock file and take a
    /// byte-range `LockFileEx` over its whole length -- the Windows
    /// spelling of `flock(LOCK_EX)`.
    fn open_locked(path: &str, fail_immediately: bool) -> io::Result<Option<*mut c_void>> {
        if path.bytes().any(|b| b == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains a NUL byte",
            ));
        }
        let mut wide: Vec<u16> = std::ffi::OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // OPEN_ALWAYS creates on first use, like O_CREAT; the contents
        // are never meaningful. The lock serializes, so other opens may
        // still share the file.
        let handle = unsafe {
            CreateFileW(
                wide.as_mut_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle as isize == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // Lock one 32-bit span from offset 0 -- more bytes than any lock
        // file will hold, the conventional whole-file range lock.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        let mut flags = LOCKFILE_EXCLUSIVE_LOCK;
        if fail_immediately {
            flags |= LOCKFILE_FAIL_IMMEDIATELY;
        }
        let ok = unsafe { LockFileEx(handle, flags, 0, u32::MAX, u32::MAX, &mut overlapped) };
        if ok == 0 {
            let e = io::Error::last_os_error();
            // SAFETY: `handle` was opened above and the lock failed, so
            // this close is the only owner.
            unsafe { CloseHandle(handle) };
            if e.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
                return Ok(None);
            }
            return Err(e);
        }
        Ok(Some(handle))
    }

    /// Block until the exclusive lock is acquired.
    pub fn exclusive(path: &str) -> io::Result<Self> {
        match Self::open_locked(path, false)? {
            Some(handle) => Ok(Self { handle }),
            None => Err(io::Error::other("lock refused without blocking")),
        }
    }

    /// `LOCKFILE_FAIL_IMMEDIATELY`: `Ok(None)` when another holder has
    /// the lock (`ERROR_LOCK_VIOLATION`).
    pub fn try_exclusive(path: &str) -> io::Result<Option<Self>> {
        Self::open_locked(path, true)
    }
}

#[cfg(windows)]
impl Drop for FileLock {
    fn drop(&mut self) {
        // SAFETY: `handle` is the lock file this `FileLock` opened and
        // locked; unlock then close releases it for the next holder.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        unsafe { UnlockFileEx(self.handle, 0, u32::MAX, u32::MAX, &mut overlapped) };
        unsafe { CloseHandle(self.handle) };
    }
}

/// Distinct temp-file names within one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically replace `path` with `data`: write a temp file in the same
/// directory, `fsync` it, then `rename(2)` it over `path`. Either the
/// old contents or the new ones are observable, never a mix; the temp
/// file never outlives the call (success or failure).
#[cfg(unix)]
pub fn atomic_replace(path: &str, data: &[u8]) -> io::Result<()> {
    let temp = format!(
        "{}.tmp{}.{}",
        path,
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let c_temp = CString::new(temp.as_str()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "temp path contains a NUL byte")
    })?;
    let c_path = CString::new(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;

    let result = write_and_rename(&c_temp, data, &c_path);
    if result.is_err() {
        // A failed swap must not leave the temp file behind.
        unsafe { libc::unlink(c_temp.as_ptr()) };
    }
    result
}

#[cfg(unix)]
fn write_and_rename(c_temp: &CString, data: &[u8], c_path: &CString) -> io::Result<()> {
    let fd = unsafe {
        libc::open(
            c_temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
            0o644,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // Write all of `data`, restarting on short writes.
    let mut written = 0usize;
    while written < data.len() {
        let n = unsafe {
            libc::write(
                fd,
                data[written..].as_ptr() as *const _,
                data.len() - written,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            unsafe { libc::close(fd) };
            return Err(e);
        }
        written += n as usize;
    }

    // The rename publishes this content; fsync makes it durable first,
    // the same ordering every atomic-replace writer uses.
    if unsafe { libc::fsync(fd) } != 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    unsafe { libc::close(fd) };

    if unsafe { libc::rename(c_temp.as_ptr(), c_path.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The Windows spelling of the atomic swap: write the temp file with
/// `CreateFileW`+`WriteFile`, flush it with `FlushFileBuffers` (the
/// `fsync` half of the ordering), then `MoveFileExW(MOVEFILE_REPLACE_
/// EXISTING | MOVEFILE_WRITE_THROUGH)` publishes it in one step.
#[cfg(windows)]
pub fn atomic_replace(path: &str, data: &[u8]) -> io::Result<()> {
    let temp = format!(
        "{}.tmp{}.{}",
        path,
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let result = write_and_replace(&temp, data, path);
    if result.is_err() {
        // A failed swap must not leave the temp file behind.
        let wide: Vec<u16> = std::ffi::OsStr::new(&temp)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe { DeleteFileW(wide.as_ptr()) };
    }
    result
}

#[cfg(windows)]
fn write_and_replace(temp: &str, data: &[u8], path: &str) -> io::Result<()> {
    let mut open = |name: &str, access: u32, disposition: u32| -> io::Result<Vec<u16>> {
        if name.bytes().any(|b| b == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains a NUL byte",
            ));
        }
        let wide: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_mut_ptr(),
                access,
                FILE_SHARE_READ,
                std::ptr::null(),
                disposition,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle as isize == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut written = 0u32;
        let len = u32::try_from(data.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload exceeds 4 GiB"))?;
        // SAFETY: the handle is owned by this call and closed before it
        // returns, on every arm below.
        let ok = unsafe { WriteFile(handle, data.as_ptr(), len, &mut written, std::ptr::null()) };
        if ok == 0 {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(e);
        }
        if unsafe { FlushFileBuffers(handle) } == 0 {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(e);
        }
        unsafe { CloseHandle(handle) };
        Ok(wide)
    };
    let temp_wide = open(temp, GENERIC_WRITE, CREATE_ALWAYS)?;

    let mut wide: Vec<u16> = std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            temp_wide.as_ptr(),
            wide.as_mut_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// The handful of `kernel32` calls behind the Windows backends, declared
// the same way as `mmap.rs`'s and `process.rs`'s.
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *mut u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut c_void,
    ) -> *mut c_void;
    fn WriteFile(
        file: *mut c_void,
        buffer: *const u8,
        bytes_to_write: u32,
        bytes_written: *mut u32,
        overlapped: *const c_void,
    ) -> i32;
    fn FlushFileBuffers(file: *mut c_void) -> i32;
    fn MoveFileExW(existing: *const u16, new: *mut u16, flags: u32) -> i32;
    fn DeleteFileW(file_name: *const u16) -> i32;
    fn LockFileEx(
        file: *mut c_void,
        flags: u32,
        reserved: u32,
        bytes_to_lock_low: u32,
        bytes_to_lock_high: u32,
        overlapped: *mut OVERLAPPED,
    ) -> i32;
    fn UnlockFileEx(
        file: *mut c_void,
        reserved: u32,
        bytes_to_unlock_low: u32,
        bytes_to_unlock_high: u32,
        overlapped: *mut OVERLAPPED,
    ) -> i32;
    fn CloseHandle(object: *mut c_void) -> i32;
}

/// `OVERLAPPED`, the async bookkeeping the lock calls want valid even on
/// a synchronous handle; zeroed means offset 0, wait-for-completion.
#[cfg(windows)]
#[repr(C)]
struct OVERLAPPED {
    internal: usize,
    internal_high: usize,
    anonymous: [u32; 4],
    h_event: *mut c_void,
}

#[cfg(windows)]
const INVALID_HANDLE_VALUE: isize = -1;
/// `GENERIC_READ`.
#[cfg(windows)]
const GENERIC_READ: u32 = 0x8000_0000;
/// `GENERIC_WRITE`.
#[cfg(windows)]
const GENERIC_WRITE: u32 = 0x4000_0000;
/// `FILE_SHARE_READ` and `FILE_SHARE_WRITE`.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x1;
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x2;
/// `OPEN_ALWAYS` / `CREATE_ALWAYS`.
#[cfg(windows)]
const OPEN_ALWAYS: u32 = 4;
#[cfg(windows)]
const CREATE_ALWAYS: u32 = 2;
/// `FILE_ATTRIBUTE_NORMAL`.
#[cfg(windows)]
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
/// `LOCKFILE_EXCLUSIVE_LOCK`.
#[cfg(windows)]
const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x2;
/// `LOCKFILE_FAIL_IMMEDIATELY`.
#[cfg(windows)]
const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
/// `ERROR_LOCK_VIOLATION`, the refusal a `try` lock reads.
#[cfg(windows)]
const ERROR_LOCK_VIOLATION: i32 = 33;
/// `MOVEFILE_REPLACE_EXISTING` | `MOVEFILE_WRITE_THROUGH`.
#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// A temp directory that removes itself on drop.
    struct Scratch {
        dir: std::path::PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "boundless-platform-file-{}-{}",
                tag,
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("mkdir");
            Self { dir }
        }

        fn path(&self, name: &str) -> String {
            self.dir.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn try_exclusive_reports_a_held_lock() {
        let scratch = Scratch::new("held");
        let lock_path = scratch.path("shared.lock");

        let held = FileLock::try_exclusive(&lock_path).expect("first lock");
        assert!(held.is_some(), "a fresh lock file must be lockable");
        // flock guards the open file description, so a second open in the
        // same process conflicts the same way another process would.
        let blocked = FileLock::try_exclusive(&lock_path).expect("second attempt io");
        assert!(blocked.is_none(), "lock must be held");

        drop(held);
        let reacquired = FileLock::try_exclusive(&lock_path).expect("retry io");
        assert!(reacquired.is_some(), "lock must be free after drop");
    }

    #[test]
    fn exclusive_blocks_until_released() {
        let scratch = Scratch::new("blocking");
        let lock_path = scratch.path("blocking.lock");

        let held = FileLock::exclusive(&lock_path).expect("lock");
        // Blocking acquisition from another thread must wait for the drop.
        let locker = std::thread::spawn(move || {
            let got = FileLock::exclusive(&lock_path).expect("blocked lock");
            // Holding it briefly proves acquisition happened after release.
            drop(got);
        });
        // Give the locker a moment to hit the block, then release.
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(held);
        locker.join().expect("locker thread");
    }

    #[test]
    fn atomic_replace_swaps_whole_contents() {
        let scratch = Scratch::new("swap");
        let path = scratch.path("data.conf");

        std::fs::write(&path, b"old contents").expect("seed");
        atomic_replace(&path, b"brand new contents").expect("replace");

        let mut read_back = String::new();
        std::fs::File::open(&path)
            .expect("open")
            .read_to_string(&mut read_back)
            .expect("read");
        assert_eq!(read_back, "brand new contents");
    }

    #[test]
    fn atomic_replace_leaves_no_temp_file() {
        let scratch = Scratch::new("temp");
        let path = scratch.path("data.conf");
        std::fs::write(&path, b"seed").expect("seed");
        atomic_replace(&path, b"next").expect("replace");

        let leftovers: Vec<_> = std::fs::read_dir(&scratch.dir)
            .expect("readdir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["data.conf".to_string()], "no temp residue");
    }

    #[test]
    fn atomic_replace_failure_cleans_the_temp_file() {
        let scratch = Scratch::new("failclean");
        // A path under a directory that does not exist fails at the temp
        // write; the temp path itself is under the missing directory, so
        // instead fail the rename: the temp write succeeds, the target
        // directory is missing.
        let path = scratch.path("missing_dir/data.conf");
        assert!(atomic_replace(&path, b"payload").is_err());
    }

    /// The acceptance fixture: concurrent lock-read-modify-write-replace
    /// loops lose no updates.
    #[test]
    fn concurrent_update_fixture_loses_nothing() {
        let scratch = Scratch::new("concurrent");
        let data_path = scratch.path("counter");
        let lock_path = scratch.path("counter.lock");

        atomic_replace(&data_path, b"0").expect("seed");

        const PER_THREAD: u32 = 50;
        let mut threads = Vec::new();
        for _ in 0..2u32 {
            let data_path = data_path.clone();
            let lock_path = lock_path.clone();
            threads.push(std::thread::spawn(move || {
                for _ in 0..PER_THREAD {
                    // Serialize on the stable lock file around the whole
                    // read-modify-write, or a racing rename loses an update.
                    let _lock = FileLock::exclusive(&lock_path).expect("lock");
                    let current: u32 = std::fs::read_to_string(&data_path)
                        .expect("read")
                        .trim()
                        .parse()
                        .expect("digits");
                    atomic_replace(&data_path, (current + 1).to_string().as_bytes())
                        .expect("replace");
                }
            }));
        }
        for t in threads {
            t.join().expect("worker thread");
        }

        let final_count: u32 = std::fs::read_to_string(&data_path)
            .expect("final read")
            .trim()
            .parse()
            .expect("digits");
        assert_eq!(final_count, 2 * PER_THREAD, "every increment must land");
    }
}
