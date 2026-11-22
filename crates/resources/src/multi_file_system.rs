//! `MultiFileSystem` port (`multi_file_system.cpp`): searches the base file
//! systems in the order they were added.
//!
//! Reference quirks preserved:
//! - every lookup stops at the first base that reports a hit, which is exactly
//!   the same-name resource priority of the res path (earlier path wins);
//! - `addBaseFileSystem` appends when the index is negative or out of range
//!   (the C++ `uint(index) >= size()` cast), otherwise inserts mid-list;
//! - `readDirectory` is a *union* across all bases (the C++ `res |=`), so a
//!   name present under two res paths is listed twice, and the union is cached
//!   keyed on the path minus one trailing `/`;
//! - a successful write/erase/move records the parent directory as modified;
//!   `flushModificationMonitor` then drops those entries from the directory
//!   cache, which is what makes fresh listings visible;
//! - `getAbsolutePath` consults only the first base;
//! - `resolveToAbsolutePath` skips archives (they report no absolute path).

use std::cell::RefCell;
use std::collections::HashMap;

use crate::file_system::{FileInfo, FileSystem, FileType};

/// `MultiFileSystem`: priority search over the res path roots.
pub struct MultiFileSystem {
    base_file_systems: Vec<Box<dyn FileSystem>>,
    /// `dirCache_`, keyed on the path with one trailing `/` removed. The
    /// reference mutates it from `const` lookups, so it sits behind a cell.
    dir_cache: RefCell<HashMap<String, Vec<String>>>,
    /// `modifiedDirectories_`, refilled by successful writes until flushed.
    modified_directories: RefCell<Vec<String>>,
}

impl Default for MultiFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiFileSystem {
    /// `MultiFileSystem::MultiFileSystem`.
    pub fn new() -> Self {
        Self {
            base_file_systems: Vec::new(),
            dir_cache: RefCell::new(HashMap::new()),
            modified_directories: RefCell::new(Vec::new()),
        }
    }

    /// `addBaseFileSystem`: inserts at `index`, appending when the index is
    /// negative or out of range (the C++ `uint(index)` cast quirk).
    pub fn add_base_file_system(&mut self, file_system: Box<dyn FileSystem>, index: isize) {
        if index < 0 || index as usize >= self.base_file_systems.len() {
            self.base_file_systems.push(file_system);
        } else {
            self.base_file_systems.insert(index as usize, file_system);
        }
    }

    /// `delBaseFileSystem`: out-of-range indexes are ignored.
    pub fn del_base_file_system(&mut self, index: usize) {
        if index >= self.base_file_systems.len() {
            return;
        }
        self.base_file_systems.remove(index);
    }

    /// `getFileType`: the first base reporting a hit wins.
    pub fn get_file_type(&self, path: &str, mut info: Option<&mut FileInfo>) -> FileType {
        for base in &self.base_file_systems {
            let ft = base.get_file_type(path, info.as_deref_mut());
            if ft != FileType::NotFound {
                return ft;
            }
        }
        FileType::NotFound
    }

    /// `getFileTypeEx`: first hit wins, archive sniffing included.
    pub fn get_file_type_ex(&self, path: &str, mut info: Option<&mut FileInfo>) -> FileType {
        for base in &self.base_file_systems {
            let ft = base.get_file_type_ex(path, info.as_deref_mut());
            if ft != FileType::NotFound {
                return ft;
            }
        }
        FileType::NotFound
    }

    /// `readDirectory`: union across all bases, served from `dirCache_` on
    /// repeat visits. `None` only when no base could read the directory.
    pub fn read_directory(&self, path: &str) -> Option<Vec<String>> {
        let cache_key = cache_key(path);
        if let Some(cached) = self.dir_cache.borrow().get(cache_key.as_str()) {
            return Some(cached.clone());
        }

        let mut dir: Vec<String> = Vec::new();
        let mut succeeded = false;
        for base in &self.base_file_systems {
            if let Some(entries) = base.read_directory(path) {
                succeeded = true;
                dir.extend(entries);
            }
        }
        if !succeeded {
            return None;
        }

        self.dir_cache.borrow_mut().insert(cache_key, dir.clone());
        Some(dir)
    }

    /// `readFile`: contents from the first base that has the file.
    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        for base in &self.base_file_systems {
            if let Some(data) = base.read_file(path) {
                return Some(data);
            }
        }
        None
    }

    /// `readFile(dirPath, index)`: by-index read on the first base that
    /// resolves it.
    pub fn read_file_at(&self, dir_path: &str, index: usize) -> Option<Vec<u8>> {
        for base in &self.base_file_systems {
            if let Some(data) = base.read_file_at(dir_path, index) {
                return Some(data);
            }
        }
        None
    }

    /// `collateFiles`: the same path as seen by *every* base that has it,
    /// in priority order.
    pub fn collate_files(&self, path: &str) -> Vec<Vec<u8>> {
        let mut ret = Vec::new();
        for base in &self.base_file_systems {
            if let Some(data) = base.read_file(path) {
                ret.push(data);
            }
        }
        ret
    }

    /// `makeDirectory`: first base that succeeds.
    pub fn make_directory(&self, path: &str) -> bool {
        for base in &self.base_file_systems {
            if base.make_directory(path) {
                return true;
            }
        }
        false
    }

    /// `writeFile`: writes through the first base that accepts the write and
    /// marks the parent directory modified.
    pub fn write_file(&self, path: &str, data: &[u8]) -> bool {
        for base in &self.base_file_systems {
            if base.write_file(path, data) {
                self.add_modified_directory(&crate::bwresource::get_file_path(path));
                return true;
            }
        }
        false
    }

    /// `writeFileIfExists`: writes into the first base that already holds
    /// the path -- not necessarily the first base overall, unlike
    /// `write_file`. Marks the parent directory modified.
    pub fn write_file_if_exists(&self, path: &str, data: &[u8]) -> bool {
        for base in &self.base_file_systems {
            if base.get_file_type(path, None) == FileType::File && base.write_file(path, data) {
                self.add_modified_directory(&crate::bwresource::get_file_path(path));
                return true;
            }
        }
        false
    }

    /// `eraseFileOrDirectory`: erases from the first base that has it and
    /// marks the parent directory modified.
    pub fn erase_file_or_directory(&self, path: &str) -> bool {
        for base in &self.base_file_systems {
            if base.erase_file_or_directory(path) {
                self.add_modified_directory(&crate::bwresource::get_file_path(path));
                return true;
            }
        }
        false
    }

    /// `moveFileOrDirectory`: first base that succeeds; both parents are
    /// marked modified.
    pub fn move_file_or_directory(&self, old_path: &str, new_path: &str) -> bool {
        for base in &self.base_file_systems {
            if base.move_file_or_directory(old_path, new_path) {
                self.add_modified_directory(&crate::bwresource::get_file_path(old_path));
                self.add_modified_directory(&crate::bwresource::get_file_path(new_path));
                return true;
            }
        }
        false
    }

    /// `resolveToAbsolutePath`: the first base containing `path` replaces it
    /// with its absolute form. Archives (no absolute path) are skipped.
    pub fn resolve_to_absolute_path(&self, path: &mut String) -> FileType {
        for base in &self.base_file_systems {
            let ft = base.get_file_type(path, None);
            if ft != FileType::NotFound
                && let Some(full_path) = base.get_absolute_path(path)
            {
                *path = full_path;
                return ft;
            }
        }
        FileType::NotFound
    }

    /// `getAbsolutePath`: only the first base is consulted. An archive first
    /// base yields the empty string (its join is empty), and with no base at
    /// all the input echoes back.
    pub fn get_absolute_path(&self, path: &str) -> String {
        match self.base_file_systems.first() {
            Some(base) => base.get_absolute_path(path).unwrap_or_default(),
            None => path.to_string(),
        }
    }

    /// `addModifiedDirectory`: records the parent directory, stripping one
    /// trailing `/` like the reference cache keys.
    fn add_modified_directory(&self, dir: &str) {
        let dir = dir.strip_suffix('/').unwrap_or(dir);
        self.modified_directories.borrow_mut().push(dir.to_string());
    }

    /// `flushModificationMonitor`: drops every modified directory from the
    /// cache so subsequent listings re-read the bases.
    pub fn flush_modification_monitor(&mut self) {
        let modified = std::mem::take(&mut *self.modified_directories.borrow_mut());
        let mut cache = self.dir_cache.borrow_mut();
        for dir in modified {
            cache.remove(dir.as_str());
        }
    }

    /// Number of registered bases (`baseFileSystems_` size).
    pub fn base_file_system_count(&self) -> usize {
        self.base_file_systems.len()
    }
}

/// `dirCache_`/`addModifiedDirectory` key: one trailing `/` removed.
fn cache_key(path: &str) -> String {
    path.strip_suffix('/').unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_system::NativeFileSystem;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "boundless_mfs_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn write(root: &str, rel: &str, data: &[u8]) {
        let full = format!("{}{}", root.trim_end_matches('/'), rel);
        if let Some(parent) = std::path::Path::new(&full).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, data).unwrap();
    }

    fn native(root: &str) -> Box<dyn FileSystem> {
        Box::new(NativeFileSystem::new(root))
    }

    #[test]
    fn first_base_wins_for_same_named_resources() {
        let a = temp_root("prio_a");
        let b = temp_root("prio_b");
        write(&a, "/config.xml", b"<first/>");
        write(&b, "/config.xml", b"<second/>");
        write(&b, "/only_b.txt", b"b");

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(native(&a), -1);
        mfs.add_base_file_system(native(&b), -1);

        // Same name in both: the earlier res path serves the read.
        assert_eq!(mfs.get_file_type("config.xml", None), FileType::File);
        assert_eq!(mfs.read_file("config.xml"), Some(b"<first/>".to_vec()));
        // Missing in the first falls through to the second.
        assert_eq!(mfs.read_file("only_b.txt"), Some(b"b".to_vec()));
        assert_eq!(mfs.get_file_type("neither", None), FileType::NotFound);
        assert_eq!(mfs.read_file("neither"), None);

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn insertion_index_shuffles_priority_like_add_base_file_system() {
        let a = temp_root("idx_a");
        let b = temp_root("idx_b");
        write(&a, "/x.txt", b"a");
        write(&b, "/x.txt", b"b");

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(native(&a), -1);
        mfs.add_base_file_system(native(&b), -1);
        assert_eq!(mfs.read_file("x.txt"), Some(b"a".to_vec()));

        // Move b to the front: priority flips.
        mfs.add_base_file_system(native(&b), 0);
        assert_eq!(mfs.base_file_system_count(), 3);
        assert_eq!(mfs.read_file("x.txt"), Some(b"b".to_vec()));
        // Removing the front copy of b restores a's priority.
        mfs.del_base_file_system(0);
        assert_eq!(mfs.base_file_system_count(), 2);
        assert_eq!(mfs.read_file("x.txt"), Some(b"a".to_vec()));
        // Out-of-range deletes are no-ops.
        mfs.del_base_file_system(9);
        assert_eq!(mfs.base_file_system_count(), 2);

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn file_type_ex_takes_the_first_hit_including_archives() {
        let a = temp_root("ex_a");
        let b = temp_root("ex_b");
        write(&a, "/pack.bin", b"PK\x03\x04");
        write(&b, "/pack.bin", b"not a zip");

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(native(&a), -1);
        mfs.add_base_file_system(native(&b), -1);

        let mut info = FileInfo::default();
        assert_eq!(
            mfs.get_file_type_ex("pack.bin", Some(&mut info)),
            FileType::Archive
        );
        assert_eq!(info.size, 4);

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn directory_listings_are_a_union_and_are_cached() {
        let a = temp_root("dir_a");
        let b = temp_root("dir_b");
        write(&a, "/shared/common.txt", b"a");
        write(&a, "/shared/only_a.txt", b"a");
        write(&b, "/shared/common.txt", b"b");
        write(&b, "/shared/only_b.txt", b"b");

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(native(&a), -1);
        mfs.add_base_file_system(native(&b), -1);

        // Duplicates survive: a name in two roots is listed twice.
        let listing = mfs.read_directory("shared").unwrap();
        let mut names = listing.clone();
        names.sort();
        assert_eq!(
            names,
            vec!["common.txt", "common.txt", "only_a.txt", "only_b.txt"]
        );

        // The cache key ignores one trailing '/', and serves the same union.
        assert_eq!(mfs.read_directory("shared/"), Some(listing));

        // A write through the multiplexer does NOT appear until the monitor
        // flush invalidates the cached directory.
        assert!(mfs.write_file("shared/new.txt", b"new"));
        let stale = mfs.read_directory("shared").unwrap();
        assert!(!stale.contains(&"new.txt".to_string()));

        mfs.flush_modification_monitor();
        let fresh = mfs.read_directory("shared").unwrap();
        assert!(fresh.contains(&"new.txt".to_string()));

        // No base can read the directory at all -> None.
        assert_eq!(mfs.read_directory("missing"), None);

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn root_writes_mark_the_dot_dir_and_leave_the_root_cache_stale() {
        let a = temp_root("dot_a");
        write(&a, "/top.txt", b"a");
        write(&a, "/sub/inner.txt", b"i");

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(native(&a), -1);

        // Prime the caches.
        let before = mfs.read_directory("").unwrap();
        assert!(before.contains(&"top.txt".to_string()));
        assert!(
            mfs.read_directory("sub")
                .unwrap()
                .contains(&"inner.txt".to_string())
        );

        // dirname("top.txt") is "." on POSIX, so the *dot* slot is marked and
        // the root listing cache ("") is not invalidated by the flush.
        assert!(mfs.write_file("top.txt", b"b"));
        mfs.flush_modification_monitor();
        let stale = mfs.read_directory("").unwrap();
        assert!(!stale.contains(&"inner.txt".to_string()));
        assert!(stale.contains(&"top.txt".to_string()));
        assert_eq!(mfs.read_file("top.txt"), Some(b"b".to_vec()));

        // A sub-directory write marks "sub", which the flush does invalidate.
        assert!(mfs.write_file("sub/new.txt", b"n"));
        assert!(
            !mfs.read_directory("sub")
                .unwrap()
                .contains(&"new.txt".to_string())
        );
        mfs.flush_modification_monitor();
        assert!(
            mfs.read_directory("sub")
                .unwrap()
                .contains(&"new.txt".to_string())
        );

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn collate_files_collects_every_base_in_order() {
        let a = temp_root("col_a");
        let b = temp_root("col_b");
        write(&a, "/config.xml", b"<first/>");
        write(&b, "/config.xml", b"<second/>");

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(native(&a), -1);
        mfs.add_base_file_system(native(&b), -1);

        assert_eq!(
            mfs.collate_files("config.xml"),
            vec![b"<first/>".to_vec(), b"<second/>".to_vec()]
        );
        assert!(mfs.collate_files("missing").is_empty());

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn resolve_to_absolute_path_skips_archives() {
        let a = temp_root("res_a");
        write(&a, "/dir/leaf.txt", b"leaf");

        let mut mfs = MultiFileSystem::new();
        // An archive first: it contains nothing and resolves no absolute path.
        let mut builder = crate::zip::ZipFileSystem::open("", crate::zip::empty_archive()).unwrap();
        builder.update_file("elsewhere.txt", Some(b"x".to_vec()));
        let bytes = builder.create_zip();
        mfs.add_base_file_system(
            Box::new(crate::file_system::ZipArchiveFs::open(bytes).unwrap()),
            -1,
        );
        mfs.add_base_file_system(native(&a), -1);

        let mut path = "dir/leaf.txt".to_string();
        assert_eq!(mfs.resolve_to_absolute_path(&mut path), FileType::File);
        assert_eq!(path, format!("{}/dir/leaf.txt", a));

        // Unresolvable paths come back untouched.
        let mut missing = "nope.txt".to_string();
        assert_eq!(
            mfs.resolve_to_absolute_path(&mut missing),
            FileType::NotFound
        );
        assert_eq!(missing, "nope.txt");

        // getAbsolutePath consults only the first base (the zip here), whose
        // join is the empty string, so the result is empty.
        assert_eq!(mfs.get_absolute_path("dir/leaf.txt"), "");

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn make_erase_and_move_take_the_first_base_that_succeeds() {
        let a = temp_root("ops_a");
        let b = temp_root("ops_b");

        let mut mfs = MultiFileSystem::new();
        // A zip base accepts writes in memory, so it shadows b for mutations.
        let mut builder = crate::zip::ZipFileSystem::open("", crate::zip::empty_archive()).unwrap();
        builder.update_file("live.txt", Some(b"zip".to_vec()));
        let bytes = builder.create_zip();
        mfs.add_base_file_system(
            Box::new(crate::file_system::ZipArchiveFs::open(bytes).unwrap()),
            -1,
        );
        mfs.add_base_file_system(native(&b), -1);

        // The zip refuses mkdirs, so the request falls to the native base.
        assert!(mfs.make_directory("fresh"));
        assert_eq!(mfs.get_file_type("fresh", None), FileType::Directory);
        // Writes land in the zip image, not on disk b -- and stay gated
        // until a createZip flush, so reads fall through to nothing here.
        assert!(mfs.write_file("live.txt", b"zip2"));
        assert_eq!(mfs.read_file("live.txt"), None);
        assert_eq!(
            fs::read(format!("{}/live.txt", b)).map_err(|e| e.kind()),
            Err(std::io::ErrorKind::NotFound)
        );

        // Erase and move hit the first base that has the entry.
        assert!(mfs.erase_file_or_directory("live.txt"));
        assert_eq!(mfs.get_file_type("live.txt", None), FileType::NotFound);

        write(&b, "/disk.txt", b"disk");
        assert!(mfs.move_file_or_directory("disk.txt", "disk2.txt"));
        assert_eq!(mfs.get_file_type("disk2.txt", None), FileType::File);

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn miss_paths_exhaust_every_base() {
        let a = temp_root("miss_a");
        let b = temp_root("miss_b");
        write(&a, "/keep.txt", b"k");

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(native(&a), -1);
        mfs.add_base_file_system(native(&b), -1);

        assert_eq!(mfs.get_file_type_ex("ghost.txt", None), FileType::NotFound);
        assert!(!mfs.write_file("no/dir/ghost.txt", b"g"));
        assert!(!mfs.erase_file_or_directory("ghost.txt"));
        assert!(!mfs.move_file_or_directory("ghost.txt", "moved.txt"));

        // An exhausted Default multiplexer answers nothing and echoes
        // absolute joins back.
        let empty = MultiFileSystem::default();
        assert_eq!(empty.get_file_type("x", None), FileType::NotFound);
        assert_eq!(empty.get_absolute_path("x"), "x");

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn resolve_skips_a_zip_hit_without_an_absolute_path() {
        let a = temp_root("zipskip_a");
        write(&a, "/leaf.txt", b"leaf");

        // The zip holds the file, but archives resolve no absolute path, so
        // resolution falls through to the native base behind it.
        let mut builder = crate::zip::ZipFileSystem::open("", crate::zip::empty_archive()).unwrap();
        builder.update_file("leaf.txt", Some(b"zipped".to_vec()));
        let bytes = builder.create_zip();

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(
            Box::new(crate::file_system::ZipArchiveFs::open(bytes).unwrap()),
            -1,
        );
        mfs.add_base_file_system(native(&a), -1);

        // The read still comes from the zip (first hit).
        assert_eq!(mfs.read_file("leaf.txt"), Some(b"zipped".to_vec()));

        let mut path = "leaf.txt".to_string();
        assert_eq!(mfs.resolve_to_absolute_path(&mut path), FileType::File);
        assert_eq!(path, format!("{}/leaf.txt", a));

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn read_file_at_delegates_in_priority_order() {
        let a = temp_root("at_a");
        let b = temp_root("at_b");
        write(&a, "/only/one.txt", b"a0");
        write(&b, "/only/two.txt", b"b1");

        let mut mfs = MultiFileSystem::new();
        mfs.add_base_file_system(native(&a), -1);
        mfs.add_base_file_system(native(&b), -1);

        // The reference asks each base separately with the same index: base a
        // resolves index 0 (one.txt), but index 1 is out of range for a's
        // single-entry listing and b's listing only holds two.txt at index 0,
        // so index 1 resolves nowhere.
        assert_eq!(mfs.read_file_at("only", 0), Some(b"a0".to_vec()));
        assert_eq!(mfs.read_file_at("only", 1), None);
        assert_eq!(mfs.read_file_at("missing", 0), None);

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }
}
