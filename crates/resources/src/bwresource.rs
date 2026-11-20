//! `BWResource` port (`bwresource.cpp`): the res-path list, the per-path
//! root search, and POSIX path helpers.
//!
//! Reference quirks preserved:
//! - `BW_RES_PATH_SEPARATOR` is `':'` on unix and `addPaths` tokenises on it;
//! - a search path that does not exist is reported and *not* pushed, so the
//!   path list only ever holds live roots (`postAddPaths` maps each to a
//!   zip or native file system, `.zip` roots and `res.zip` packages both
//!   become archives that shadow plain directories);
//! - `pathIsRelative("")` is false — an empty path is treated as absolute;
//! - `getFilePath` is POSIX `dirname()` ("a/b.txt" -> "a", "b.txt" -> "."),
//!   while `getFilename` splits on both separators;
//! - extension helpers ignore dots inside directory components;
//! - `formatSearchPath` replaces backslashes and strips one trailing slash;
//! - `resolveToAbsolutePath` appends `'/'` to resolved directories.

use crate::file_system::{NativeFileSystem, ZipArchiveFs};
use crate::file_system::FileSystem;
use crate::multi_file_system::MultiFileSystem;
use crate::zip::{FileInfo, FileType};

/// `BW_RES_PATH_SEPARATOR` for unix builds.
pub const BW_RES_PATH_SEPARATOR: char = ':';

/// `BWResource::getFilename`: text after the last separator, or the whole
/// string when there is none.
pub fn get_filename(file: &str) -> &str {
    match file.rfind(['/', '\\']) {
        Some(pos) => &file[pos + 1..],
        None => file,
    }
}

/// `BWResource::getFilePath` as built on linux: POSIX `dirname()`.
pub fn get_file_path(file: &str) -> String {
    let bytes = file.as_bytes();
    if bytes.is_empty() {
        return ".".to_string();
    }
    // Find the last run of slashes; glibc trims everything from it.
    let mut last_slash = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            last_slash = Some(i);
            while i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                i += 1;
            }
        }
        i += 1;
    }
    match last_slash {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(start) => file[..start].trim_end_matches('/').to_string(),
    }
}

/// Extension position per `BWResource::findExtensionPos`: the last `'.'`
/// that sits inside the filename (after any separator).
fn find_extension_pos(file: &str) -> Option<usize> {
    let pos = file.rfind('.')?;
    // std::min(find_last_of('/'), find_last_of('\\')): the last separator
    // of either kind, if any.
    let name_pos = file.rfind(['/', '\\']);
    if name_pos.is_none_or(|name_pos| pos > name_pos) {
        Some(pos)
    } else {
        None
    }
}

/// `BWResource::getExtension`: text after the last filename dot, `""` when
/// the name has none.
pub fn get_extension(file: &str) -> &str {
    match find_extension_pos(file) {
        Some(pos) => &file[pos + 1..],
        None => "",
    }
}

/// `BWResource::removeExtension`: the name with its extension stripped.
pub fn remove_extension(file: &str) -> &str {
    match find_extension_pos(file) {
        Some(pos) => &file[..pos],
        None => file,
    }
}

/// `BWResource::changeExtension`: `removeExtension` plus `new_extension`
/// (including its leading dot).
pub fn change_extension(file: &str, new_extension: &str) -> String {
    format!("{}{}", remove_extension(file), new_extension)
}

/// `BWUtil::formatPath`: empty stays empty, otherwise a trailing `/` is
/// ensured (path sanitising is a no-op on linux).
pub fn format_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let mut path = path.to_string();
    if !path.ends_with('/') {
        path.push('/');
    }
    path
}

/// `BWResourceImpl::formatSearchPath` (unix branch): backslashes become
/// slashes and one trailing slash is stripped.
pub fn format_search_path(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    if let Some(stripped) = path.strip_suffix('/') {
        path = stripped.to_string();
    }
    path
}

/// `BWResourceImpl::hasDriveInPath`: a `':'` anywhere or a leading root.
fn has_drive_in_path(path: &str) -> bool {
    !path.is_empty() && (path.contains(':') || path.starts_with(['\\', '/']))
}

/// `BWResourceImpl::pathIsRelative`: non-empty, no drive, no leading root.
/// The empty path reports *not relative*.
pub fn path_is_relative(path: &str) -> bool {
    !path.is_empty() && !has_drive_in_path(path) && !path.starts_with(['\\', '/'])
}

/// `BWResource`: the res path list plus its multiplexed file system.
pub struct BwResource {
    paths: Vec<String>,
    file_system: MultiFileSystem,
    native_file_system: NativeFileSystem,
    /// `fileSystem_` is only built by `postAddPaths`; later path edits
    /// trigger a refresh exactly when it already exists.
    file_system_built: bool,
    /// The `#ifdef EDITOR_ENABLED` branch of `postAddPaths`: an empty-base
    /// native root is appended so absolute paths resolve in the tools.
    editor_default_root: bool,
}

impl Default for BwResource {
    fn default() -> Self {
        Self::new()
    }
}

impl BwResource {
    /// A fresh, empty instance (`BWResource` before `init`).
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            file_system: MultiFileSystem::new(),
            native_file_system: NativeFileSystem::new(""),
            file_system_built: false,
            editor_default_root: false,
        }
    }

    /// `init(vector<BW::string>)`: adds every path then builds the file
    /// systems (`postAddPaths`).
    pub fn init_paths(paths: &[&str]) -> Self {
        let mut resource = Self::new();
        for path in paths {
            resource.add_path(path, -1);
        }
        resource.refresh_paths();
        resource
    }

    /// `#ifdef EDITOR_ENABLED` in `postAddPaths`: appends an empty-base
    /// native root, letting absolute paths (and the search helpers, which
    /// build absolute candidates) resolve through the multiplexer.
    pub fn enable_editor_default_root(&mut self) {
        self.editor_default_root = true;
        if self.file_system_built {
            self.refresh_paths();
        }
    }

    /// `addPath`: formats the path and inserts it at `index`, appending when
    /// the index is negative or out of range. Rebuilds the file systems when
    /// they already exist.
    pub fn add_path(&mut self, path: &str, index: isize) {
        let fpath = format_search_path(path);
        let index = if index < 0 || index as usize >= self.paths.len() {
            self.paths.len()
        } else {
            index as usize
        };
        self.paths.insert(index, fpath);
        if self.file_system_built {
            self.refresh_paths();
        }
    }

    /// `addPaths`: tokenises on [`BW_RES_PATH_SEPARATOR`] and keeps only the
    /// paths that exist on disk (the reference reports the others).
    pub fn add_paths(&mut self, path: &str) {
        for part in path.split(BW_RES_PATH_SEPARATOR) {
            let formatted = format_search_path(part);
            if self.native_file_system.get_file_type(&formatted, None)
                == FileType::NotFound
            {
                continue;
            }
            self.paths.push(formatted);
        }
        if self.file_system_built {
            self.refresh_paths();
        }
    }

    /// `delPath`: out-of-range indexes are ignored.
    pub fn del_path(&mut self, index: usize) {
        if index < self.paths.len() {
            self.paths.remove(index);
        }
    }

    /// `getPath`: `""` for out-of-range indexes (the loop in
    /// `postAddPaths` relies on this terminator).
    pub fn get_path(&self, index: usize) -> &str {
        self.paths.get(index).map(String::as_str).unwrap_or("")
    }

    /// `getPathNum`.
    pub fn get_path_num(&self) -> usize {
        self.paths.len()
    }

    /// `isValid`: at least one res path is registered.
    pub fn is_valid(&self) -> bool {
        !self.paths.is_empty()
    }

    /// `postAddPaths`/`refreshPaths`: rebuilds the multiplexer from the path
    /// list. Every path becomes exactly one root:
    /// - `*.zip` file or `<path>/res.zip` package -> a zip archive;
    /// - a directory -> a native root;
    /// - anything else is skipped (the reference aborts on it).
    pub fn refresh_paths(&mut self) {
        self.file_system = MultiFileSystem::new();
        let paths = self.paths.clone();
        for path in &paths {
            let base = self.native_file_system.get_file_type(path, None);
            let pak_path = format!("{}/res.zip", path);
            if get_extension(path) == "zip"
                && (base == FileType::Archive || base == FileType::File)
            {
                if let Some(data) = self.native_file_system.read_file(path) {
                    if let Some(zip) = ZipArchiveFs::open(data) {
                        self.file_system
                            .add_base_file_system(Box::new(zip), -1);
                        continue;
                    }
                }
                // Invalid archives are dropped like the reference's
                // "not a valid zip file" critical.
                continue;
            }
            if self.native_file_system.get_file_type(&pak_path, None)
                == FileType::File
            {
                if let Some(data) = self.native_file_system.read_file(&pak_path) {
                    if let Some(zip) = ZipArchiveFs::open(data) {
                        self.file_system
                            .add_base_file_system(Box::new(zip), -1);
                        continue;
                    }
                }
                continue;
            }
            if base == FileType::Directory {
                let root = format!("{}/", path);
                self.file_system
                    .add_base_file_system(Box::new(NativeFileSystem::new(&root)), -1);
            }
        }
        if self.editor_default_root {
            self.file_system
                .add_base_file_system(Box::new(NativeFileSystem::new("")), -1);
        }
        self.file_system_built = true;
    }

    /// `fileSystem()`: the multiplexer over all registered roots.
    pub fn file_system(&self) -> &MultiFileSystem {
        &self.file_system
    }

    /// `nativeFileSystem()`: the drive-level root used for absolute paths.
    pub fn native_file_system(&self) -> &NativeFileSystem {
        &self.native_file_system
    }

    /// `BWResource::getFileType`: first root reporting a hit.
    pub fn get_file_type(&self, path: &str, info: Option<&mut FileInfo>) -> FileType {
        self.file_system.get_file_type(path, info)
    }

    /// `isDir`.
    pub fn is_dir(&self, path: &str) -> bool {
        self.file_system.get_file_type(path, None) == FileType::Directory
    }

    /// `isFile`.
    pub fn is_file(&self, path: &str) -> bool {
        self.file_system.get_file_type(path, None) == FileType::File
    }

    /// `isDirOrBasePath`: a directory, or literally one of the res paths.
    pub fn is_dir_or_base_path(&self, path: &str) -> bool {
        if !self.is_dir(path) {
            for index in 0..self.get_path_num() {
                let curr_path = self.get_path(index).to_string();
                let curr_path_with_slash = format_path(&curr_path);
                if path == curr_path || path == curr_path_with_slash {
                    return true;
                }
            }
        }
        false
    }

    /// `fileExists`: any file type, archive or directory.
    pub fn file_exists(&self, file: &str) -> bool {
        self.file_system.get_file_type(file, None) != FileType::NotFound
    }

    /// `fileAbsolutelyExists`: probes the native file system directly.
    pub fn file_absolutely_exists(&self, file: &str) -> bool {
        self.native_file_system.get_file_type(file, None) != FileType::NotFound
    }

    /// `resolveToAbsolutePath`: relative (or empty) paths resolve against the
    /// multiplexer; absolute paths are probed natively. Directories gain a
    /// trailing `/`.
    pub fn resolve_to_absolute_path(&self, path: &mut String) -> FileType {
        let ft = if path.is_empty() || !path_is_relative(path) {
            NativeFileSystem::get_absolute_file_type(path, None)
        } else {
            self.file_system.resolve_to_absolute_path(path)
        };

        if ft == FileType::Directory && !path.is_empty() && !path.ends_with('/') {
            path.push('/');
        }
        ft
    }

    /// `searchPathsForFile`: finds `file` by name under any res path,
    /// recursing into sub-directories, and rewrites it to the full res path.
    pub fn search_paths_for_file(&self, file: &mut String) -> bool {
        let mut filename = get_filename(file).to_string();
        for index in 0..self.get_path_num() {
            if self.search_for_file(self.get_path(index), &mut filename) {
                *file = filename;
                return true;
            }
        }
        false
    }

    /// `searchForFile`: depth-first search under `directory`; on success
    /// `file` is rewritten to the full path where the file was found.
    pub fn search_for_file(&self, directory: &str, file: &mut String) -> bool {
        let formatted = format_path(directory);
        let mut search_for = format!("{}{}", formatted, file);
        if self.file_system.get_file_type(&search_for, None) == FileType::File {
            *file = search_for;
            return true;
        }

        let Some(dir_list) = self.file_system.read_directory(&formatted) else {
            return false;
        };
        for entry in dir_list {
            search_for = format!("{}{}", formatted, entry);
            if self.file_system.get_file_type(&search_for, None) == FileType::Directory
                && self.search_for_file(&search_for, file)
            {
                return true;
            }
        }
        false
    }

    /// `ensurePathExists`: creates each component of the res-relative path
    /// through the multiplexer. A file-style path loses its filename first
    /// (`getFilePath`). Always reports success (like the reference).
    pub fn ensure_path_exists(&self, path: &str) -> bool {
        if path.is_empty() {
            return false;
        }
        let mut n_path = path.to_string();
        if !n_path.ends_with('/') {
            n_path = get_file_path(&n_path);
        }

        // Walk each separator like the reference's find_first_of loop,
        // skipping the first character so leading roots are not remade.
        let mut offset = 0;
        while let Some(pos) = n_path[offset + 1..].find('/').map(|p| p + offset + 1) {
            eprintln!("ensure mkdir {:?} pos={} offset={} -> {}", &n_path[..pos], pos, offset, self.file_system.make_directory(&n_path[..pos]));
            offset = pos;
        }
        true
    }

    /// `modifiedTime`: the mtime of the res-relative file, 0 when missing or
    /// not a plain file.
    pub fn modified_time(&self, file_or_resource: &str) -> u64 {
        let mut info = FileInfo::default();
        if self.file_system.get_file_type(file_or_resource, Some(&mut info))
            == FileType::File
        {
            return info.modified as u64;
        }
        0
    }

    /// `isFileOlder`: true when `old_file` predates `new_file` by at least
    /// `must_be_older_than_secs`; false when either side is missing.
    pub fn is_file_older(
        &self,
        old_file: &str,
        new_file: &str,
        must_be_older_than_secs: u64,
    ) -> bool {
        let old_changed = self.modified_time(old_file);
        let new_changed = self.modified_time(new_file);
        if old_changed != 0 && new_changed != 0 {
            old_changed + must_be_older_than_secs * 10_000_000 < new_changed
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "boundless_bwr_{}_{}_{}",
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

    #[test]
    fn filename_and_path_split_like_posix_tools() {
        assert_eq!(get_filename("dir/sub/file.txt"), "file.txt");
        assert_eq!(get_filename("dir\\sub\\file.txt"), "file.txt");
        assert_eq!(get_filename("plain.txt"), "plain.txt");
        assert_eq!(get_filename("dir/"), "");

        assert_eq!(get_file_path("dir/sub/file.txt"), "dir/sub");
        assert_eq!(get_file_path("plain.txt"), ".");
        assert_eq!(get_file_path(""), ".");
        assert_eq!(get_file_path("/root"), "/");
        assert_eq!(get_file_path("/root/"), "/root");
        assert_eq!(get_file_path("dir//leaf"), "dir");
        assert_eq!(get_file_path("trailing/"), "trailing");
        assert_eq!(get_file_path("/"), "/");
    }

    #[test]
    fn extensions_ignore_directory_dots() {
        assert_eq!(get_extension("dir.name/file"), "");
        assert_eq!(get_extension("dir.name/file.txt"), "txt");
        assert_eq!(get_extension("archive.zip"), "zip");
        assert_eq!(get_extension("noext"), "");
        // A leading dot belongs to the extension (pos > npos holds).
        assert_eq!(get_extension(".hidden"), "hidden");
        assert_eq!(remove_extension(".hidden"), "");
        assert_eq!(remove_extension("a.b/c"), "a.b/c");
        assert_eq!(remove_extension("model.model"), "model");
        assert_eq!(change_extension("model.model", ".primitives"), "model.primitives");
        assert_eq!(change_extension("plain", ".txt"), "plain.txt");
    }

    #[test]
    fn formatting_and_relative_rules_match_the_reference() {
        assert_eq!(format_path(""), "");
        assert_eq!(format_path("res"), "res/");
        assert_eq!(format_path("res/"), "res/");

        assert_eq!(format_search_path("res\\win\\path"), "res/win/path");
        assert_eq!(format_search_path("res/"), "res");
        assert_eq!(format_search_path(""), "");

        // Empty paths are "absolute" in the reference.
        assert!(!path_is_relative(""));
        assert!(path_is_relative("res/path"));
        assert!(!path_is_relative("/res/path"));
        assert!(!path_is_relative("\\res\\path"));
        assert!(!path_is_relative("c:/res/path"));
    }

    #[test]
    fn same_name_resources_resolve_by_path_priority() {
        let a = temp_root("acc_a");
        let b = temp_root("acc_b");
        write(&a, "/config.xml", b"<from_a/>");
        write(&b, "/config.xml", b"<from_b/>");

        let resource = BwResource::init_paths(&[&a, &b]);
        assert!(resource.is_valid());
        assert_eq!(resource.get_path_num(), 2);
        assert_eq!(resource.get_path(0), a);
        // Out-of-range paths read as "".
        assert_eq!(resource.get_path(2), "");

        // The first res path wins for the same name.
        assert_eq!(
            resource.file_system().read_file("config.xml"),
            Some(b"<from_a/>".to_vec())
        );
        assert!(resource.is_file("config.xml"));
        assert!(resource.file_exists("config.xml"));

        // Drop the first path and refresh: priority falls to b.
        let mut resource = resource;
        resource.del_path(0);
        resource.refresh_paths();
        assert_eq!(
            resource.file_system().read_file("config.xml"),
            Some(b"<from_b/>".to_vec())
        );

        // Re-add a at the front: it wins again.
        resource.add_path(&a, 0);
        assert_eq!(
            resource.file_system().read_file("config.xml"),
            Some(b"<from_a/>".to_vec())
        );
        // Appending a missing path is recorded but serves nothing; appending
        // an existing one extends the search tail.
        resource.add_path(&format!("{}/missing", a), -1);
        resource.refresh_paths();
        assert_eq!(resource.get_path_num(), 3);

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn add_paths_splits_on_the_colon_and_skips_dead_paths() {
        let a = temp_root("colon_a");
        let b = temp_root("colon_b");
        write(&a, "/f.txt", b"a");
        write(&b, "/f.txt", b"b");

        let mut resource = BwResource::new();
        resource.add_paths(&format!("{}:{}:{}:", a, b, format!("{}/gone", a)));
        assert_eq!(resource.get_path_num(), 2);
        // Like the reference, the multiplexer is only (re)built once a
        // refresh runs -- init calls postAddPaths at the very end.
        assert_eq!(resource.file_system().base_file_system_count(), 0);
        resource.refresh_paths();
        assert_eq!(resource.get_path(0), a);
        assert_eq!(resource.get_path(1), b);
        assert_eq!(
            resource.file_system().read_file("f.txt"),
            Some(b"a".to_vec())
        );

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn zip_roots_and_res_zip_packages_shadow_directories() {
        let dir = temp_root("ziproot");
        write(&dir, "/plain.txt", b"plain");

        // A res.zip package: the directory root is NOT added, only the zip.
        let mut builder =
            crate::zip::ZipFileSystem::open("", crate::zip::empty_archive()).unwrap();
        builder.update_file("packed.txt", Some(b"packed".to_vec()));
        let bytes = builder.create_zip();
        fs::write(format!("{}/res.zip", dir), &bytes).unwrap();

        let resource = BwResource::init_paths(&[&dir]);
        assert_eq!(
            resource.file_system().read_file("packed.txt"),
            Some(b"packed".to_vec())
        );
        // The plain directory file is invisible: only the package was added.
        assert_eq!(resource.file_system().read_file("plain.txt"), None);

        // A bare .zip res path is also an archive root.
        let zip_path = format!("{}/solo.zip", dir);
        fs::write(&zip_path, &bytes).unwrap();
        let resource = BwResource::init_paths(&[&zip_path]);
        assert_eq!(
            resource.file_system().read_file("packed.txt"),
            Some(b"packed".to_vec())
        );

        // A corrupt archive is dropped rather than served.
        let broken = format!("{}/broken.zip", dir);
        fs::write(&broken, b"PK\x03\x04truncated").unwrap();
        let resource = BwResource::init_paths(&[&broken]);
        assert_eq!(resource.file_system().base_file_system_count(), 0);
        assert_eq!(resource.get_path_num(), 1);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refresh_edges_cover_corrupt_packages_and_live_rebuilds() {
        let dir = temp_root("corrupt");
        // A res.zip that cannot be opened is dropped like the reference's
        // "not a valid zip file" critical; the directory behind it is not
        // added either.
        fs::write(format!("{}/res.zip", dir), b"PK\x03\x04junk").unwrap();

        let mut resource = BwResource::new();
        resource.add_path(&dir, -1);
        resource.refresh_paths();
        assert_eq!(resource.file_system().base_file_system_count(), 0);

        // Path edits after the multiplexer exists rebuild it in place.
        let live = temp_root("live");
        write(&live, "/alive.txt", b"yes");
        resource.add_paths(&live);
        assert_eq!(
            resource.file_system().read_file("alive.txt"),
            Some(b"yes".to_vec())
        );
        // The editor root can also be flipped on after the fact.
        resource.enable_editor_default_root();
        assert_eq!(
            resource.get_file_type(&format!("{}/alive.txt", live), None),
            FileType::File
        );

        // Out-of-range deletes stay no-ops; the Default constructor is the
        // pre-init instance.
        resource.del_path(9);
        assert!(resource.get_path_num() >= 1);
        assert!(!BwResource::default().is_valid());
        assert_eq!(
            BwResource::default().native_file_system().get_file_type("", None),
            FileType::NotFound
        );

        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&live).unwrap();
    }

    #[test]
    fn search_in_an_unreadable_directory_fails_fast() {
        let a = temp_root("unread");
        let mut resource = BwResource::new();
        resource.add_path(&a, -1);
        resource.enable_editor_default_root();
        resource.refresh_paths();

        // The editor root cannot list a missing directory, so the search
        // bails before recursing.
        let mut file = "x.txt".to_string();
        assert!(!resource.search_for_file("/no/such/dir", &mut file));
        assert_eq!(file, "x.txt");

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn resolve_to_absolute_path_maps_relative_paths_onto_roots() {
        let a = temp_root("res_a");
        write(&a, "/dir/leaf.txt", b"leaf");
        fs::create_dir(format!("{}/folder", a)).unwrap();

        let resource = BwResource::init_paths(&[&a]);

        // A file resolves to the owning root, unchanged tail.
        let mut path = "dir/leaf.txt".to_string();
        assert_eq!(resource.resolve_to_absolute_path(&mut path), FileType::File);
        assert_eq!(path, format!("{}/dir/leaf.txt", a));

        // A directory gains the trailing slash.
        let mut dir_path = "folder".to_string();
        assert_eq!(
            resource.resolve_to_absolute_path(&mut dir_path),
            FileType::Directory
        );
        assert_eq!(dir_path, format!("{}/folder/", a));

        // Already-absolute paths are probed natively.
        let mut absolute = format!("{}/dir/leaf.txt", a);
        assert_eq!(
            resource.resolve_to_absolute_path(&mut absolute),
            FileType::File
        );
        assert_eq!(absolute, format!("{}/dir/leaf.txt", a));

        // Empty paths are "absolute", so they resolve nowhere.
        let mut empty = String::new();
        assert_eq!(
            resource.resolve_to_absolute_path(&mut empty),
            FileType::NotFound
        );
        assert_eq!(empty, "");

        // Missing relative paths stay untouched.
        let mut missing = "ghost.txt".to_string();
        assert_eq!(
            resource.resolve_to_absolute_path(&mut missing),
            FileType::NotFound
        );
        assert_eq!(missing, "ghost.txt");

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn dir_and_base_path_checks_cover_the_res_list() {
        let a = temp_root("base_a");
        write(&a, "/thing.txt", b"t");
        fs::create_dir(format!("{}/sub", a)).unwrap();

        let resource = BwResource::init_paths(&[&a]);
        assert!(resource.is_dir("sub"));
        assert!(!resource.is_dir("thing.txt"));
        // The res path itself is not visible through the roots (the root is
        // the path's contents), but isDirOrBasePath accepts it verbatim.
        assert!(!resource.is_dir(&a));
        assert!(resource.is_dir_or_base_path(&a));
        assert!(resource.is_dir_or_base_path(&format!("{}/", a)));
        assert!(!resource.is_dir_or_base_path("thing.txt"));
        assert!(resource.is_file("thing.txt"));
        assert!(!resource.file_absolutely_exists("thing.txt"));
        assert!(resource.file_absolutely_exists(&format!("{}/thing.txt", a)));

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn search_paths_finds_files_recurring_into_subdirectories() {
        let a = temp_root("srch_a");
        let b = temp_root("srch_b");
        write(&a, "/deep/nest/target.xml", b"<found/>");

        // The search helpers build absolute candidates, so they need the
        // editor-style empty-base root the reference appends under
        // EDITOR_ENABLED.
        let mut resource = BwResource::new();
        resource.add_path(&a, -1);
        resource.add_path(&b, -1);
        resource.enable_editor_default_root();
        resource.refresh_paths();

        let mut file = "target.xml".to_string();
        assert!(resource.search_paths_for_file(&mut file));
        assert_eq!(file, format!("{}deep/nest/target.xml", format_path(&a)));

        // Unfound searches leave the input alone.
        let mut missing = "nothere.xml".to_string();
        assert!(!resource.search_paths_for_file(&mut missing));
        assert_eq!(missing, "nothere.xml");

        // Direct search recurses the same way and rewrites to the hit path.
        let mut direct = "target.xml".to_string();
        assert!(resource.search_for_file(&a, &mut direct));
        assert_eq!(direct, format!("{}deep/nest/target.xml", format_path(&a)));
        let mut not_there = "nothere.xml".to_string();
        assert!(!resource.search_for_file(&a, &mut not_there));
        assert_eq!(not_there, "nothere.xml");

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn ensure_path_creates_missing_directories() {
        let a = temp_root("ens_a");
        let resource = BwResource::init_paths(&[&a]);

        assert!(resource.ensure_path_exists("level1/level2/level3/"));
        assert_eq!(
            resource.file_system().get_file_type("level1/level2/level3", None),
            FileType::Directory
        );
        // A file-style path loses its tail before creating; and because the
        // find-first-separator loop never yields the tail component, the
        // last directory of a slash-less path is skipped (reference quirk).
        assert!(resource.ensure_path_exists("level1/level2/deeper/file.bin"));
        assert_eq!(
            resource.file_system().get_file_type("level1/level2/deeper", None),
            FileType::NotFound
        );
        assert!(!resource.ensure_path_exists(""));

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn modification_times_gate_the_older_check() {
        let a = temp_root("mtime_a");
        write(&a, "/old.bin", b"o");
        write(&a, "/new.bin", b"n");

        let resource = BwResource::init_paths(&[&a]);
        assert!(resource.modified_time("old.bin") > 0);
        assert_eq!(resource.modified_time("missing.bin"), 0);
        // Directories report no time (only files do).
        fs::create_dir(format!("{}/adir", a)).unwrap();
        assert_eq!(resource.modified_time("adir"), 0);

        // Same-second mtimes need the slack of a large threshold to flip.
        assert!(!resource.is_file_older("old.bin", "new.bin", 60));
        assert!(!resource.is_file_older("old.bin", "missing.bin", 0));
        assert!(!resource.is_file_older("missing.bin", "new.bin", 0));

        fs::remove_dir_all(&a).unwrap();
    }
}

