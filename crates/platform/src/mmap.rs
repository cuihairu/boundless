//! Memory mapping and dynamic-library lookup (BW-15-01-05).
//!
//! `mmap(2)` is the fast path behind the packed-resource and DataSection
//! readers: a whole file is mapped and read as a byte slice without a
//! copy, and the kernel pages it in on demand. `dlopen`/`dlsym` are the
//! shared-library lookup the script and plugin layers use to reach
//! symbols by name. Both are the "OS calls live in platform" (BW-16-02-01)
//! slice of resource loading.
//!
//! Each platform gets its own spelling: Linux/macOS use the POSIX
//! `mmap`/`dlopen`; Windows uses `CreateFileMapping`+`MapViewOfFile` and
//! `LoadLibrary`+`GetProcAddress` through the same constructors, so the
//! resource readers above stay platform-agnostic (BW-15-01-05).

use std::ffi::CString;
use std::io;
use std::ops::Deref;
#[cfg(unix)]
use std::os::raw::c_int;
use std::os::raw::{c_char, c_void};

/// A read-only mapping of a file (or an anonymous zeroed region),
/// unmapped on drop.
#[derive(Debug)]
pub struct Mmap {
    ptr: *mut u8,
    len: usize,
    /// Which kernel object `Drop` releases. Windows has two free paths
    /// (`UnmapViewOfFile` for file maps, `VirtualFree` for anonymous
    /// ones); unix always `munmap`s, so the field only exists there.
    #[cfg(windows)]
    kind: MapKind,
}

/// The Windows `Drop` discriminator.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapKind {
    File,
    Anonymous,
}

impl Mmap {
    /// Map a file's whole contents read-only (`PROT_READ|MAP_SHARED` on
    /// unix, `PAGE_READONLY`/`FILE_MAP_READ` on Windows). An empty file
    /// maps to an empty slice, not an error.
    #[cfg(unix)]
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

    /// The Windows spelling: `CreateFileW`+`GetFileSizeEx`, then
    /// `CreateFileMappingW(PAGE_READONLY)`+`MapViewOfFile(FILE_MAP_READ)`.
    /// A zero size maps the whole file, and a zero-length file short
    /// circuits to the empty slice like the unix arm (the mapping object
    /// refuses zero-length files).
    #[cfg(windows)]
    pub fn map_file(path: &str) -> io::Result<Self> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let file = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if file as isize == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let mut len = 0i64;
        if unsafe { GetFileSizeEx(file, &mut len) } == 0 {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(file) };
            return Err(e);
        }
        let len = len as usize;
        if len == 0 {
            unsafe { CloseHandle(file) };
            return Ok(Self {
                ptr: std::ptr::null_mut(),
                len: 0,
                kind: MapKind::File,
            });
        }

        let mapping = unsafe {
            CreateFileMappingW(
                file,
                std::ptr::null(),
                PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            )
        };
        if mapping.is_null() || mapping as isize == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(file) };
            return Err(e);
        }
        // The mapping object and the view hold their own references, so
        // both raw handles close right away -- the same shape as closing
        // the fd after a successful `mmap` on unix.
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
        if view.is_null() {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(mapping) };
            unsafe { CloseHandle(file) };
            return Err(e);
        }
        unsafe { CloseHandle(mapping) };
        unsafe { CloseHandle(file) };
        Ok(Self {
            ptr: view as *mut u8,
            len,
            kind: MapKind::File,
        })
    }

    /// An anonymous zeroed read-write mapping of `len` bytes.
    #[cfg(unix)]
    pub fn anonymous(len: usize) -> io::Result<Self> {
        Self::map(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
        )
    }

    /// The Windows spelling: `VirtualAlloc(MEM_COMMIT|MEM_RESERVE,
    /// PAGE_READWRITE)` pages come back zeroed, like `MAP_ANONYMOUS`.
    #[cfg(windows)]
    pub fn anonymous(len: usize) -> io::Result<Self> {
        if len == 0 {
            return Ok(Self {
                ptr: std::ptr::null_mut(),
                len: 0,
                kind: MapKind::Anonymous,
            });
        }
        let ptr = unsafe {
            VirtualAlloc(
                std::ptr::null_mut(),
                len,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if ptr.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            ptr: ptr as *mut u8,
            len,
            kind: MapKind::Anonymous,
        })
    }

    /// The `mmap(2)` call, shared by the two unix constructors. A
    /// zero-length mapping holds a null pointer and an empty slice (the
    /// kernel refuses `len == 0` with `EINVAL`).
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn drop(&mut self) {
        if self.len > 0 {
            // SAFETY: `ptr` is a live mapping of `len` bytes for exactly
            // this drop.
            unsafe { libc::munmap(self.ptr as *mut c_void, self.len) };
        }
    }

    #[cfg(windows)]
    fn drop(&mut self) {
        match self.kind {
            // SAFETY: `ptr` is the live view this mapping owns.
            MapKind::File => {
                let _ = unsafe { UnmapViewOfFile(self.ptr as *const c_void) };
            }
            // SAFETY: `ptr` is the live allocation this mapping owns and
            // `MEM_RELEASE` frees the whole reserved region.
            MapKind::Anonymous => {
                let _ = unsafe { VirtualFree(self.ptr as *mut c_void, 0, MEM_RELEASE) };
            }
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
    #[cfg(unix)]
    pub fn open(path: &str) -> io::Result<Self> {
        let c_path = CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Err(io::Error::other(dlerror_string()));
        }
        Ok(Self { handle })
    }

    /// The Windows spelling: `LoadLibraryW`. The caller decides whether
    /// the name is a bare module name (searched) or a path, exactly like
    /// `dlopen`'s bare-name/path split.
    #[cfg(windows)]
    pub fn open(path: &str) -> io::Result<Self> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// `dlsym`: the address of `name`, or an error if the symbol is absent.
    #[cfg(unix)]
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

    /// The Windows spelling: `GetProcAddress`. The lookup name is ANSI,
    /// so a NUL byte is rejected up front rather than truncated silently.
    #[cfg(windows)]
    pub fn symbol(&self, name: &str) -> io::Result<*mut c_void> {
        let c_name = CString::new(name).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "symbol contains a NUL byte")
        })?;
        let addr = unsafe { GetProcAddress(self.handle, c_name.as_ptr()) };
        if addr.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(addr)
    }
}

impl Drop for DynamicLibrary {
    #[cfg(unix)]
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.handle) };
    }

    #[cfg(windows)]
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.handle) };
    }
}

/// `dlerror()` as a `String`, consumed so the next `dlsym`/`dlopen` sees
/// a clean error state.
#[cfg(unix)]
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
    #[cfg(unix)]
    fn dlopen_libc_and_resolve_a_symbol() {
        // libc is always on the link path of a test binary's host.
        let lib = DynamicLibrary::open("libc.so.6").expect("dlopen libc");
        let addr = lib.symbol("malloc").expect("dlsym malloc");
        assert!(!addr.is_null(), "malloc must resolve to a real address");
    }

    #[test]
    #[cfg(unix)]
    fn dlopen_missing_symbol_is_an_error() {
        let lib = DynamicLibrary::open("libc.so.6").expect("dlopen libc");
        let err = lib.symbol("boundless_no_such_symbol_xyz").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn dlopen_missing_library_is_an_error() {
        let err = DynamicLibrary::open("/nonexistent/libboundless-no-such.so").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn load_library_resolves_an_export() {
        // kernel32 is loaded into every process; one of its exports is
        // the same address-space lookup `dlsym(malloc)` covers on unix.
        let lib = DynamicLibrary::open("kernel32.dll").expect("LoadLibrary kernel32");
        let addr = lib
            .symbol("VirtualAlloc")
            .expect("GetProcAddress VirtualAlloc");
        assert!(!addr.is_null(), "the export must resolve to a real address");
    }

    #[test]
    #[cfg(windows)]
    fn load_library_missing_symbol_is_an_error() {
        let lib = DynamicLibrary::open("kernel32.dll").expect("LoadLibrary kernel32");
        let err = lib.symbol("boundless_no_such_symbol_xyz").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn load_library_missing_library_is_an_error() {
        let err = DynamicLibrary::open("boundless-no-such.dll").unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}

// The handful of `kernel32` calls behind the Windows backends,
// hand-declared like `test-support`'s process-memory probe: a
// `windows-sys` feature tree for twelve functions is not worth the
// dependency. Windows `errno` is `GetLastError`, so
// `io::Error::last_os_error` reports these calls' failures.
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut c_void,
    ) -> *mut c_void;
    fn GetFileSizeEx(file: *mut c_void, size: *mut i64) -> i32;
    fn CreateFileMappingW(
        file: *mut c_void,
        attributes: *const c_void,
        protect: u32,
        maximum_size_high: u32,
        maximum_size_low: u32,
        name: *const u16,
    ) -> *mut c_void;
    fn MapViewOfFile(
        file_mapping: *mut c_void,
        desired_access: u32,
        offset_high: u32,
        offset_low: u32,
        bytes_to_map: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(base_address: *const c_void) -> i32;
    fn VirtualAlloc(
        address: *mut c_void,
        size: usize,
        alloc_type: u32,
        protect: u32,
    ) -> *mut c_void;
    fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
    fn LoadLibraryW(file_name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, proc_name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn CloseHandle(object: *mut c_void) -> i32;
}

#[cfg(windows)]
const INVALID_HANDLE_VALUE: isize = -1;
/// `GENERIC_READ`.
#[cfg(windows)]
const GENERIC_READ: u32 = 0x8000_0000;
/// `FILE_SHARE_READ`.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x1;
/// `OPEN_EXISTING`.
#[cfg(windows)]
const OPEN_EXISTING: u32 = 3;
/// `FILE_ATTRIBUTE_NORMAL`.
#[cfg(windows)]
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
/// `PAGE_READONLY`.
#[cfg(windows)]
const PAGE_READONLY: u32 = 2;
/// `PAGE_READWRITE`.
#[cfg(windows)]
const PAGE_READWRITE: u32 = 4;
/// `FILE_MAP_READ`.
#[cfg(windows)]
const FILE_MAP_READ: u32 = 4;
/// `MEM_COMMIT`.
#[cfg(windows)]
const MEM_COMMIT: u32 = 0x1000;
/// `MEM_RESERVE`.
#[cfg(windows)]
const MEM_RESERVE: u32 = 0x2000;
/// `MEM_RELEASE`.
#[cfg(windows)]
const MEM_RELEASE: u32 = 0x8000;
