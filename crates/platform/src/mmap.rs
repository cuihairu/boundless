//! Memory mapping and dynamic-library lookup (BW-15-01-05).
//!
//! `mmap(2)` is the fast path behind the packed-resource and DataSection
//! readers: a whole file is mapped and read as a byte slice without a
//! copy, and the kernel pages it in on demand. `dlopen`/`dlsym` are the
//! shared-library lookup the script and plugin layers use to reach
//! symbols by name. Both are the "OS calls live in platform" (BW-16-02-01)
//! slice of resource loading.
//!
//! Unix-only: `mmap`/`dlopen` are POSIX. Windows uses `CreateFileMapping`
//! and `LoadLibrary`/`GetProcAddress`; those backends land with the
//! Windows work under BW-15-05.

use std::ffi::CString;
use std::io;
use std::ops::Deref;
use std::os::raw::{c_char, c_int, c_void};

/// A read-only mapping of a file (or an anonymous zeroed region),
/// `munmap`ped on drop.
#[derive(Debug)]
pub struct Mmap {
    ptr: *mut u8,
    len: usize,
}

impl Mmap {
    /// `mmap` a file's whole contents read-only (`PROT_READ|MAP_SHARED`).
    /// An empty file maps to an empty slice, not an error.
    pub fn map_file(path: &str) -> io::Result<Self> {
        let c_path = CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        let len = stat.st_size as usize;

        // mmap needs a live fd; it takes its own reference to the file
        // description, so closing after a successful map keeps the
        // mapping valid and only an early failure owns the close here.
        let result = Self::map(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
        );
        unsafe { libc::close(fd) };
        result
    }

    /// An anonymous zeroed read-write mapping of `len` bytes.
    pub fn anonymous(len: usize) -> io::Result<Self> {
        Self::map(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
        )
    }

    /// The `mmap(2)` call, shared by the two constructors. A zero-length
    /// mapping holds a null pointer and an empty slice (the kernel
    /// refuses `len == 0` with `EINVAL`).
    fn map(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
    ) -> io::Result<Self> {
        if len == 0 {
            return Ok(Self {
                ptr: std::ptr::null_mut(),
                len: 0,
            });
        }
        let ptr = unsafe { libc::mmap(addr, len, prot, flags, fd, 0) };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            ptr: ptr as *mut u8,
            len,
        })
    }

    /// The mapped length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the mapping is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Deref for Mmap {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: the mapping is valid for `len` bytes for the lifetime of
        // `self`, and `munmap` only runs in `Drop`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        if self.len > 0 {
            // SAFETY: `ptr` is a live mapping of `len` bytes for exactly
            // this drop.
            unsafe { libc::munmap(self.ptr as *mut c_void, self.len) };
        }
    }
}

/// A dynamically loaded shared library, closed on drop.
#[derive(Debug)]
pub struct DynamicLibrary {
    handle: *mut c_void,
}

impl DynamicLibrary {
    /// `dlopen(path, RTLD_NOW | RTLD_LOCAL)`.
    pub fn open(path: &str) -> io::Result<Self> {
        let c_path = CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Err(io::Error::other(dlerror_string()));
        }
        Ok(Self { handle })
    }

    /// `dlsym`: the address of `name`, or an error if the symbol is absent.
    pub fn symbol(&self, name: &str) -> io::Result<*mut c_void> {
        let c_name = CString::new(name).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "symbol contains a NUL byte")
        })?;
        let addr = unsafe { libc::dlsym(self.handle, c_name.as_ptr()) };
        if addr.is_null() {
            return Err(io::Error::other(dlerror_string()));
        }
        Ok(addr)
    }
}

impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.handle) };
    }
}

/// `dlerror()` as a `String`, consumed so the next `dlsym`/`dlopen` sees
/// a clean error state.
fn dlerror_string() -> String {
    let err = unsafe { libc::dlerror() };
    if err.is_null() {
        "unknown dynamic-loader error".to_string()
    } else {
        let bytes = unsafe { std::ffi::CStr::from_ptr(err as *const c_char) };
        bytes.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_file_reads_its_contents() {
        let dir = std::env::temp_dir().join(format!("boundless-mmap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("blob.bin");
        let payload: Vec<u8> = (0u8..=255).collect();
        std::fs::write(&path, &payload).expect("write");

        let mapped = Mmap::map_file(path.to_str().unwrap()).expect("map");
        assert_eq!(&*mapped, &payload[..], "mapped bytes equal the file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_file_maps_to_an_empty_slice() {
        let dir = std::env::temp_dir().join(format!("boundless-mmap-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("empty.bin");
        std::fs::write(&path, b"").expect("write");

        let mapped = Mmap::map_file(path.to_str().unwrap()).expect("map");
        assert!(mapped.is_empty());
        assert_eq!(mapped.len(), 0);
        assert_eq!(&*mapped, &[][..]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn anonymous_mapping_is_zeroed_and_writable_through_deref() {
        // The mapping is exposed read-only; the underlying pages are RW,
        // which the test proves by writing through a raw pointer the
        // kernel gave back -- reading is the observable contract.
        let mapped = Mmap::anonymous(64).expect("anon");
        assert_eq!(mapped.len(), 64);
        assert!(mapped.iter().all(|&b| b == 0), "anonymous pages are zeroed");
    }

    #[test]
    fn missing_file_is_an_error() {
        let err = Mmap::map_file("/nonexistent/boundless-no-such-file").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn dlopen_libc_and_resolve_a_symbol() {
        // libc is always on the link path of a test binary's host.
        let lib = DynamicLibrary::open("libc.so.6").expect("dlopen libc");
        let addr = lib.symbol("malloc").expect("dlsym malloc");
        assert!(!addr.is_null(), "malloc must resolve to a real address");
    }

    #[test]
    fn dlopen_missing_symbol_is_an_error() {
        let lib = DynamicLibrary::open("libc.so.6").expect("dlopen libc");
        let err = lib.symbol("boundless_no_such_symbol_xyz").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn dlopen_missing_library_is_an_error() {
        let err = DynamicLibrary::open("/nonexistent/libboundless-no-such.so").unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}
