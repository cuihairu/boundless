//! `IFileSystem`/`UnixFileSystem` port: the per-root file system abstraction
//! (`file_system.hpp`/`unix_file_system.cpp`).
//!
//! Reference quirks preserved:
//! - the base path gets a `'/'` appended unless empty (`UnixFileSystem` ctor);
//! - `getFileTypeEx` promotes `FT_FILE` to `FT_ARCHIVE` when the first two
//!   bytes read little-endian as `ZIP_MAGIC` (0x4b50, i.e. "PK");
//! - `readDirectory` strips `"."`/`".."` but keeps no particular order;
//! - `readFile(dirPath, index)` joins with `'/'` and is bounds-failing silent;
//! - `makeDirectory` creates a single level only and `writeFile` fails when
//!   the parent directory does not exist (`fopen("w")` semantics);
//! - `eraseFileOrDirectory` removes directories recursively;
//! - zip archives expose no absolute path (`ZipFileSystem::getAbsolutePath`
//!   returns an empty string), which callers detect as "cannot resolve".

use std::cell::RefCell;
use std::fs;
use std::io::Read;
use std::path::Path;

pub use crate::zip::{FileInfo, FileType};

/// The little-endian `ZIP_MAGIC` sniffed by `getFileTypeEx`.
pub const ZIP_MAGIC: u16 = 0x4b50;

/// `IFileSystem`: one searchable root. Implementations answer queries about
/// paths that are relative to their base.
pub trait FileSystem {
    /// `getFileType`: type of `path`, filling `info` when present.
    fn get_file_type(&self, path: &str, info: Option<&mut FileInfo>) -> FileType;

    /// `getFileTypeEx`: like `get_file_type` but files starting with the zip
    /// magic are reported as [`FileType::Archive`].
    fn get_file_type_ex(&self, path: &str, info: Option<&mut FileInfo>) -> FileType;

    /// `readDirectory`: entry names of `path` (no `"."`/`".."`), `None` when
    /// the directory cannot be read.
    fn read_directory(&self, path: &str) -> Option<Vec<String>>;

    /// `readFile`: contents of `path`, `None` when unreadable.
    fn read_file(&self, path: &str) -> Option<Vec<u8>>;

    /// `readFile(dirPath, index)`: contents of the index'th directory entry.
    fn read_file_at(&self, dir_path: &str, index: usize) -> Option<Vec<u8>>;

    /// `getAbsolutePath`: base-joined absolute path, or `None` when the
    /// implementation cannot produce one (zip archives).
    fn get_absolute_path(&self, path: &str) -> Option<String>;

    /// `makeDirectory`: creates a single directory level.
    fn make_directory(&self, path: &str) -> bool;

    /// `writeFile`: creates or replaces `path`; fails when the parent
    /// directory is missing.
    fn write_file(&self, path: &str, data: &[u8]) -> bool;

    /// `eraseFileOrDirectory`: removes files, or directories recursively.
    fn erase_file_or_directory(&self, path: &str) -> bool;

    /// `moveFileOrDirectory`: renames `old_path` to `new_path`.
    fn move_file_or_directory(&self, old_path: &str, new_path: &str) -> bool;
}

/// `UnixFileSystem`: a native directory root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFileSystem {
    base_path: String,
}

impl NativeFileSystem {
    /// `UnixFileSystem::UnixFileSystem`: appends `'/'` to a non-empty base
    /// that does not already end with one.
    pub fn new(base_path: &str) -> Self {
        let mut base_path = base_path.to_string();
        if !base_path.is_empty() && !base_path.ends_with('/') && !base_path.ends_with('\\') {
            base_path.push('/');
        }
        Self { base_path }
    }

    /// `getAbsoluteFileType`: stats an absolute path directly.
    pub fn get_absolute_file_type(path: &str, info: Option<&mut FileInfo>) -> FileType {
        stat_file_type(Path::new(path), info)
    }

    fn resolve(&self, path: &str) -> String {
        format!("{}{}", self.base_path, path)
    }
}

fn stat_file_type(path: &Path, mut info: Option<&mut FileInfo>) -> FileType {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return FileType::NotFound;
    };
    // `S_ISREG`/`S_ISDIR` via the platform metadata; symlinks follow
    // `stat` in the reference, so resolve them here.
    let meta = if meta.file_type().is_symlink() {
        match fs::metadata(path) {
            Ok(meta) => meta,
            Err(_) => return FileType::NotFound,
        }
    } else {
        meta
    };
    let mut ft = FileType::NotFound;
    if meta.is_file() {
        ft = FileType::File;
    }
    if meta.is_dir() {
        ft = FileType::Directory;
    }
    if ft != FileType::NotFound
        && let Some(info) = &mut info
        && let Ok(modified) = meta.modified()
        && let Ok(seconds) = modified.duration_since(std::time::UNIX_EPOCH)
    {
        info.size = meta.len() as u32;
        info.modified = seconds.as_secs() as u32;
        info.created = info.modified;
        info.accessed = info.modified;
    }
    ft
}

impl FileSystem for NativeFileSystem {
    fn get_file_type(&self, path: &str, info: Option<&mut FileInfo>) -> FileType {
        Self::get_absolute_file_type(&self.resolve(path), info)
    }

    fn get_file_type_ex(&self, path: &str, info: Option<&mut FileInfo>) -> FileType {
        let ft = self.get_file_type(path, info);
        if ft == FileType::File
            && let Ok(mut file) = fs::File::open(self.resolve(path))
        {
            let mut magic = [0u8; 2];
            if file.read_exact(&mut magic).is_ok() && u16::from_le_bytes(magic) == ZIP_MAGIC {
                return FileType::Archive;
            }
        }
        ft
    }

    fn read_directory(&self, path: &str) -> Option<Vec<String>> {
        let entries = fs::read_dir(self.resolve(path)).ok()?;
        let mut dir = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name != "." && name != ".." {
                dir.push(name.into_owned());
            }
        }
        Some(dir)
    }

    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        fs::read(self.resolve(path)).ok()
    }

    fn read_file_at(&self, dir_path: &str, index: usize) -> Option<Vec<u8>> {
        let dir = self.read_directory(dir_path)?;
        if dir.is_empty() || index >= dir.len() {
            return None;
        }
        self.read_file(&format!("{}/{}", dir_path, dir[index]))
    }

    fn get_absolute_path(&self, path: &str) -> Option<String> {
        Some(self.resolve(path))
    }

    fn make_directory(&self, path: &str) -> bool {
        fs::create_dir(self.resolve(path)).is_ok()
    }

    fn write_file(&self, path: &str, data: &[u8]) -> bool {
        fs::write(self.resolve(path), data).is_ok()
    }

    fn erase_file_or_directory(&self, path: &str) -> bool {
        match self.get_file_type(path, None) {
            FileType::File => fs::remove_file(self.resolve(path)).is_ok(),
            FileType::Directory => fs::remove_dir_all(self.resolve(path)).is_ok(),
            _ => false,
        }
    }

    fn move_file_or_directory(&self, old_path: &str, new_path: &str) -> bool {
        fs::rename(self.resolve(old_path), self.resolve(new_path)).is_ok()
    }
}

/// Adapter presenting [`crate::zip::ZipFileSystem`] as a [`FileSystem`].
///
/// Reads delegate to the in-memory archive; writes land in the archive image
/// (a `createZip` flush to disk is a later integration). `get_absolute_path`
/// is `None`, matching the empty string the reference zip returns so
/// `resolveToAbsolutePath` skips archives.
pub struct ZipArchiveFs {
    inner: RefCell<crate::zip::ZipFileSystem>,
}

impl ZipArchiveFs {
    /// Opens `data` as a searchable archive root.
    pub fn open(data: Vec<u8>) -> Option<Self> {
        crate::zip::ZipFileSystem::open("", data).map(|inner| Self {
            inner: RefCell::new(inner),
        })
    }
}

impl ZipArchiveFs {
    /// `createZip`: serialises the archive image. Rows written in-session
    /// stay unreadable (`local_header_offset < 0`) until this runs and the
    /// image is reopened -- the reference gates reads the same way.
    pub fn flush(&self) -> Option<Vec<u8>> {
        Some(self.inner.borrow_mut().create_zip())
    }
}

impl FileSystem for ZipArchiveFs {
    fn get_file_type(&self, path: &str, info: Option<&mut FileInfo>) -> FileType {
        self.inner.borrow().get_file_type(path, info)
    }

    fn get_file_type_ex(&self, path: &str, info: Option<&mut FileInfo>) -> FileType {
        self.inner.borrow().get_file_type_ex(path, info)
    }

    fn read_directory(&self, path: &str) -> Option<Vec<String>> {
        self.inner.borrow().read_directory(path)
    }

    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        self.inner.borrow().read_file(path)
    }

    fn read_file_at(&self, dir_path: &str, index: usize) -> Option<Vec<u8>> {
        self.inner.borrow().read_file_at(dir_path, index)
    }

    fn get_absolute_path(&self, _path: &str) -> Option<String> {
        // `ZipFileSystem::getAbsolutePath` returns "" (path_ is empty), which
        // `MultiFileSystem::resolveToAbsolutePath` treats as unresolvable.
        None
    }

    fn make_directory(&self, _path: &str) -> bool {
        // `ZipFileSystem::makeDirectory` hard-codes false.
        false
    }

    fn write_file(&self, path: &str, data: &[u8]) -> bool {
        self.inner
            .borrow_mut()
            .write_to_structure(path, data.to_vec())
    }

    fn erase_file_or_directory(&self, path: &str) -> bool {
        self.inner.borrow_mut().erase_file_or_directory(path)
    }

    fn move_file_or_directory(&self, _old_path: &str, _new_path: &str) -> bool {
        // The reference rename path goes through DataSection updateFile;
        // surfaced by a later integration task.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "boundless_fs_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn write(root: &str, rel: &str, data: &[u8]) {
        let full = format!("{}{}", root.trim_end_matches('/'), rel);
        if let Some(parent) = Path::new(&full).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, data).unwrap();
    }

    #[test]
    fn base_path_gains_a_trailing_slash() {
        assert_eq!(NativeFileSystem::new("res/base").base_path, "res/base/");
        assert_eq!(NativeFileSystem::new("res/base/").base_path, "res/base/");
        assert_eq!(NativeFileSystem::new("").base_path, "");
        // Windows separators also satisfy the constructor check.
        assert_eq!(NativeFileSystem::new("res\\base").base_path, "res\\base/");
    }

    #[test]
    fn file_types_cover_all_states_and_fill_info() {
        let root = temp_root("types");
        write(&root, "/file.txt", b"hello");
        fs::create_dir(format!("{}/sub", root)).unwrap();

        let fs = NativeFileSystem::new(&root);
        assert_eq!(fs.get_file_type("file.txt", None), FileType::File);
        assert_eq!(fs.get_file_type("sub", None), FileType::Directory);
        assert_eq!(fs.get_file_type("nope", None), FileType::NotFound);

        let mut info = FileInfo::default();
        assert_eq!(
            fs.get_file_type("file.txt", Some(&mut info)),
            FileType::File
        );
        assert_eq!(info.size, 5);
        assert!(info.modified > 0);

        // Absolute probing goes through the same stat path.
        assert_eq!(
            NativeFileSystem::get_absolute_file_type(&format!("{}/file.txt", root), None),
            FileType::File
        );
        assert_eq!(
            NativeFileSystem::get_absolute_file_type("", None),
            FileType::NotFound
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn file_type_ex_sniffs_the_zip_magic() {
        let root = temp_root("magic");
        write(&root, "/pack.zip", b"PK\x03\x04rest");
        write(&root, "/plain.txt", b"plain");
        fs::create_dir(format!("{}/dir", root)).unwrap();

        let fs = NativeFileSystem::new(&root);
        assert_eq!(fs.get_file_type("pack.zip", None), FileType::File);
        assert_eq!(fs.get_file_type_ex("pack.zip", None), FileType::Archive);
        assert_eq!(fs.get_file_type_ex("plain.txt", None), FileType::File);
        // Directories are never sniffed.
        assert_eq!(fs.get_file_type_ex("dir", None), FileType::Directory);
        // A "PK" prefix shorter than the magic does not promote.
        write(&root, "/short", b"P");
        assert_eq!(fs.get_file_type_ex("short", None), FileType::File);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn directory_reading_strips_dot_entries() {
        let root = temp_root("listing");
        write(&root, "/dir/a.txt", b"a");
        write(&root, "/dir/b.txt", b"b");

        let fs = NativeFileSystem::new(&root);
        let mut names = fs.read_directory("dir").unwrap();
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
        assert_eq!(fs.read_directory("missing"), None);

        // By-index reads join with '/' and stay silent out of bounds.
        // Directory order is the OS's, so only membership is asserted.
        let at_0 = fs.read_file_at("dir", 0).unwrap();
        let at_1 = fs.read_file_at("dir", 1).unwrap();
        assert!(at_0 == b"a".to_vec() || at_0 == b"b".to_vec());
        assert!(at_1 == b"a".to_vec() || at_1 == b"b".to_vec());
        assert_ne!(at_0, at_1);
        assert_eq!(fs.read_file_at("dir", 2), None);
        assert_eq!(fs.read_file_at("missing", 0), None);
    }

    #[test]
    #[cfg(unix)]
    fn symlinks_follow_stat_semantics() {
        let root = temp_root("link");
        write(&root, "/real.txt", b"real");
        let link = format!("{}/alias.txt", root);
        std::os::unix::fs::symlink(format!("{}/real.txt", root), &link).unwrap();
        let dead = format!("{}/dead.txt", root);
        std::os::unix::fs::symlink(format!("{}/gone.txt", root), &dead).unwrap();

        let fs = NativeFileSystem::new(&root);
        assert_eq!(fs.get_file_type("alias.txt", None), FileType::File);
        assert_eq!(fs.read_file("alias.txt"), Some(b"real".to_vec()));
        // A dangling link fails the follow, like stat().
        assert_eq!(fs.get_file_type("dead.txt", None), FileType::NotFound);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn erasing_a_single_file_succeeds() {
        let root = temp_root("rm");
        write(&root, "/victim.txt", b"x");
        let fs = NativeFileSystem::new(&root);
        assert!(fs.erase_file_or_directory("victim.txt"));
        assert_eq!(fs.get_file_type("victim.txt", None), FileType::NotFound);
        assert!(!fs.erase_file_or_directory("victim.txt"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn absolute_paths_round_trip_through_the_base() {
        let root = temp_root("abs");
        let fs = NativeFileSystem::new(&root);
        assert_eq!(
            fs.get_absolute_path("dir/file.txt").unwrap(),
            format!("{}/dir/file.txt", root)
        );
    }

    #[test]
    fn writes_directories_and_erasure_follow_the_reference() {
        let root = temp_root("mutate");
        let fs = NativeFileSystem::new(&root);

        // writeFile with a missing parent fails like fopen("w").
        assert!(!fs.write_file("no/such/dir.txt", b"x"));
        assert!(fs.write_file("made.txt", b"data"));
        assert_eq!(fs.read_file("made.txt"), Some(b"data".to_vec()));
        // Overwrite succeeds; an empty payload still creates the file.
        assert!(fs.write_file("made.txt", b""));
        assert_eq!(fs.read_file("made.txt"), Some(Vec::new()));

        assert!(fs.make_directory("one"));
        assert!(!fs.make_directory("one/two/three"));
        assert_eq!(fs.get_file_type("one", None), FileType::Directory);

        write(&root, "/tree/leaf.bin", b"leaf");
        assert!(fs.erase_file_or_directory("tree"));
        assert_eq!(fs.get_file_type("tree", None), FileType::NotFound);
        assert!(!fs.erase_file_or_directory("tree"));

        assert!(fs.write_file("from.txt", b"1"));
        assert!(fs.move_file_or_directory("from.txt", "to.txt"));
        assert_eq!(fs.get_file_type("from.txt", None), FileType::NotFound);
        assert_eq!(fs.read_file("to.txt"), Some(b"1".to_vec()));
        assert!(!fs.move_file_or_directory("from.txt", "elsewhere.txt"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn zip_roots_answer_reads_and_refuse_absolute_paths() {
        // Build the archive bytes through the same machinery as BW-01-03-03.
        let mut builder = crate::zip::ZipFileSystem::open("", crate::zip::empty_archive()).unwrap();
        builder.update_file("res/config.xml", Some(b"<root/>".to_vec()));
        let bytes = builder.create_zip();

        let fs = ZipArchiveFs::open(bytes).unwrap();
        assert_eq!(fs.get_file_type("res/config.xml", None), FileType::File);
        // The archive-side ex probe sniffs nested zips the same way.
        assert_eq!(fs.get_file_type_ex("res/config.xml", None), FileType::File);
        assert_eq!(fs.read_file("res/config.xml"), Some(b"<root/>".to_vec()));
        let mut names = fs.read_directory("res").unwrap();
        names.sort();
        assert_eq!(names, vec!["config.xml"]);
        assert_eq!(fs.read_file_at("res", 0), Some(b"<root/>".to_vec()));
        // Archives resolve no absolute path and grow no directories.
        assert_eq!(fs.get_absolute_path("res/config.xml"), None);
        assert!(!fs.make_directory("res/extra"));
        // In-session writes take, but the row stays gated (offset < 0)
        // until the archive is flushed and reopened -- the reference's
        // updateFile/createZip dance.
        assert!(fs.write_file("res/config.xml", b"<new/>"));
        assert_eq!(fs.read_file("res/config.xml"), None);
        let bytes = fs.flush().unwrap();
        let fs = ZipArchiveFs::open(bytes).unwrap();
        // The flushed image keeps only rows carrying data (isValid):
        // the renamed-but-unloaded original is dropped, leaving the raw
        // duplicate-encoded row as the sole survivor.
        assert_eq!(fs.read_file("res/config.xml"), None);
        assert_eq!(fs.read_file("res/config.xml 2"), Some(b"<new/>".to_vec()));

        // Erasures apply immediately on the in-memory image; the survivor
        // is addressed by its encoded duplicate name.
        assert!(fs.erase_file_or_directory("res/config.xml 2"));
        assert_eq!(
            fs.get_file_type("res/config.xml 2", None),
            FileType::NotFound
        );
    }
}
