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

use std::cell::RefCell;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use crate::data_section_cache::{DataSectionCache, DEFAULT_CACHE_SIZE};
use crate::datasection::{self, DataSection};
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
    /// `DataSectionCache::instance()`: the LRU behind `openSection`,
    /// keyed by full resource path.
    section_cache: RefCell<DataSectionCache<DataSection>>,
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
            section_cache: RefCell::new(DataSectionCache::new()),
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

    /// `BWResource::init(argc, argv)` for unix servers: scans the command
    /// line, falls back through the res-path sources, and returns whether
    /// any path survived plus the surviving arguments (the reference
    /// compacts `argv` in place via `BWUtil::compressArgs`; the port hands
    /// the compressed tail back). The reference logs a "No res path set"
    /// error when nothing resolved.
    ///
    /// Source order is `loadPathsXML`'s: `--res`/`-r` arguments first, then
    /// the (deprecated) `BW_RES_PATH` environment variable, then the first
    /// usable line of `~/.bwmachined.conf` for `getUserId()`'s user.
    pub fn init_from_args(&mut self, args: &[String]) -> (bool, Vec<String>) {
        let exe_dir = executable_directory();
        let uid = current_uid();
        let env_res_path = std::env::var("BW_RES_PATH").ok();
        self.init_from_sources(args, &exe_dir, uid, &passwd_home, env_res_path.as_deref())
    }

    /// The injectable core of [`BwResource::init_from_args`]; `bw_res_path`
    /// and `home_of_uid` stand in for the environment lookups so callers
    /// (and tests) can pin them. `home_of_uid` is consulted only when the
    /// machined-conf branch runs, and it sees the uid *after* any `-uid`
    /// argument rewrote it -- the port of `setenv("UID")` feeding
    /// `getpwuid(getUserId())`.
    pub fn init_from_sources(
        &mut self,
        args: &[String],
        executable_dir: &str,
        uid: u32,
        home_of_uid: &dyn Fn(u32) -> Option<String>,
        bw_res_path: Option<&str>,
    ) -> (bool, Vec<String>) {
        let mut uid = uid;
        // The argv loop runs to `argc - 1`: a flag in the final slot never
        // matches and survives, like the reference.
        let mut remaining: Vec<String> = Vec::new();
        let mut skip_next = false;
        for (i, arg) in args.iter().enumerate() {
            if skip_next {
                skip_next = false;
                continue;
            }
            if let Some(value) = args.get(i + 1) {
                if arg == "--res" || arg == "-r" {
                    self.add_paths_with_source(value, "from command line", executable_dir);
                    skip_next = true;
                    continue;
                }
                if arg == "-UID" || arg == "-uid" {
                    // `setenv("UID", ...)` in the reference; the parsed uid
                    // feeds the machined-conf lookup directly here.
                    uid = atoi_or_zero(value) as u32;
                    skip_next = true;
                    continue;
                }
            }
            remaining.push(arg.clone());
        }

        if self.paths.is_empty() {
            if let Some(res_path) = bw_res_path {
                self.add_paths_with_source(
                    res_path,
                    "from BW_RES_PATH env variable",
                    executable_dir,
                );
            }
        }
        if self.paths.is_empty() {
            if let Some(home) = home_of_uid(uid) {
                self.add_paths_from_machined_conf(home.as_str(), executable_dir);
            }
        }

        self.refresh_paths();
        (!self.paths.is_empty(), remaining)
    }

    /// The `~/.bwmachined.conf` branch of `loadPathsXML`: comment and blank
    /// lines are skipped, each usable line contributes the token after its
    /// first `;`, and reading stops once a line has added a path (the
    /// reference's `while (paths_.empty() && fgets)` gate).
    fn add_paths_from_machined_conf(&mut self, home_dir: &str, executable_dir: &str) {
        let Ok(content) =
            std::fs::read_to_string(format!("{}/.bwmachined.conf", home_dir))
        else {
            return;
        };
        for line in content.lines() {
            if !self.paths.is_empty() {
                break;
            }
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some(res_path) = machined_conf_res_path(trimmed) {
                self.add_paths_with_source(&res_path, "from ~/.bwmachined.conf", executable_dir);
            }
        }
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

    /// `addPaths(path, desc)` used by `init`: like [`BwResource::add_paths`],
    /// plus the `MF_SERVER` branch — a relative part is anchored at the
    /// executable directory and passed through `canonicalize_file_name`
    /// (`fs::canonicalize`); when canonicalisation fails the relative form
    /// survives, and either way a part that does not exist is reported and
    /// dropped instead of pushed.
    pub fn add_paths_with_source(
        &mut self,
        path: &str,
        _source: &str,
        executable_dir: &str,
    ) {
        for part in path.split(BW_RES_PATH_SEPARATOR) {
            let mut formatted = format_search_path(part);
            // Relative res paths resolve against the executable directory.
            if !formatted.is_empty()
                && !formatted.starts_with('/')
                && !formatted.starts_with('\\')
            {
                let anchored = format!("{}/{}", executable_dir, formatted);
                if let Ok(abs) = std::fs::canonicalize(&anchored) {
                    formatted = abs.to_string_lossy().into_owned();
                }
            }
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
        {
            // postAddPaths: BW_CACHE_SIZE overrides the default, then the
            // cache is emptied before the root section is rebuilt.
            let mut cache = self.section_cache.borrow_mut();
            cache.set_size(cache_size_from(
                std::env::var("BW_CACHE_SIZE").ok().as_deref(),
            ));
            cache.clear();
        }
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

    /// `BWResource::openSection`: returns the section for `resource_id`,
    /// serving cache hits straight from the LRU. `DataSectionCensus` —
    /// which the reference consults before the cache — is a later task.
    ///
    /// Loading follows `DirSection::findChild` +
    /// `createAppropriateSection` at the level implemented so far: file
    /// content that starts (after whitespace) with `'<'` parses as an XML
    /// section rooted under the leaf file name. Binary (`BinSection`),
    /// archive (`ZipSection`) and directory (`DirSection`) branches are
    /// later tasks, as is `makeNewSection` for undotted (directory) tags.
    pub fn open_section(
        &self,
        resource_id: &str,
        make_new_section: bool,
    ) -> Option<Arc<DataSection>> {
        // If it's cached, return the cached copy.
        if let Some(section) = self.section_cache.borrow_mut().find(resource_id) {
            return Some(section);
        }

        let (section, bytes) = match self.file_system.get_file_type_ex(resource_id, None)
        {
            FileType::File => {
                let raw = self.file_system.read_file(resource_id)?;
                // createAppropriateSection ignores the extension and sniffs
                // the content: whitespace then '<' means XML. Invalid XML
                // yields no section and stays uncached (the
                // `allowModifyData` branch keeps the NULL); other content
                // would become a BinSection -- a later task.
                let is_xml = raw
                    .iter()
                    .find(|&&b| b != b' ' && b != b'\t' && b != b'\r' && b != b'\n')
                    == Some(&b'<');
                if !is_xml {
                    return None;
                }
                let mut parsed =
                    datasection::parse(get_filename(resource_id), &raw).ok()?;
                // Sized like the reference's `dataSection->bytes()`: the
                // serialised form, not the raw file length.
                let bytes = parsed.write_to_string().len();
                (Arc::new(parsed), bytes)
            }
            FileType::NotFound
                // DirSection::newSection: a dotted tag creates a fresh XML
                // section, cached under the full path with its serialised
                // size; undotted tags create directories instead.
                if make_new_section && get_filename(resource_id).contains('.') =>
            {
                let mut section = DataSection::new(get_filename(resource_id));
                let bytes = section.write_to_string().len();
                (Arc::new(section), bytes)
            }
            _ => return None,
        };

        // A freshly loaded section joins the cache under its full path.
        self.section_cache
            .borrow_mut()
            .add(resource_id, bytes, Arc::clone(&section));
        Some(section)
    }

    /// `BWResource::purge`: drops `path` from the section cache. With
    /// `recursive`, directory children are purged first, innermost paths
    /// before their parents. The census half of the reference (`del` from
    /// `DataSectionCensus`) is a later task.
    pub fn purge(&self, path: &str, recursive: bool) {
        if recursive && self.get_file_type(path, None) == FileType::Directory {
            if let Some(entries) = self.file_system.read_directory(path) {
                for entry in entries {
                    let child = if path.is_empty() {
                        entry
                    } else {
                        format!("{}/{}", path, entry)
                    };
                    self.purge(&child, true);
                }
            }
        }
        self.section_cache.borrow_mut().remove(path);
    }

    /// `BWResource::purgeAll`: empties the cache (and the census, once it
    /// exists).
    pub fn purge_all(&self) {
        self.section_cache.borrow_mut().clear();
    }

    /// The `DirSection::saveChild`/`XMLSection::save` flow for a flat
    /// resource path: serialises the section (byte-exact with
    /// `XMLSection::asBinary`) and writes it through the multiplexer,
    /// whose first root that can write wins and whose parent directory is
    /// marked modified. On success the freshly saved tree joins the cache
    /// under `resource_id` -- the reference gets this for free from the
    /// census holding the same object the cache points at; `save as`
    /// (splitSaveAsFileName) and census bookkeeping arrive with the
    /// dir-section/census tasks. The `isBinary` flag of the reference is a
    /// Windows text-mode concern and has no unix equivalent.
    ///
    /// A failed write leaves the cache untouched.
    pub fn save_section(&self, resource_id: &str, section: &mut DataSection) -> bool {
        let bytes = section.write_to_string();
        if !self.file_system.write_file(resource_id, bytes.as_bytes()) {
            return false;
        }
        self.section_cache.borrow_mut().add(
            resource_id,
            bytes.len(),
            Arc::new(section.clone()),
        );
        true
    }

    /// `XMLSection::saveIfExists`: like [`BwResource::save_section`], but
    /// the write lands in the first root that already holds the path, and
    /// a path no root holds fails without touching the cache.
    pub fn save_section_if_exists(
        &self,
        resource_id: &str,
        section: &mut DataSection,
    ) -> bool {
        let bytes = section.write_to_string();
        if !self
            .file_system
            .write_file_if_exists(resource_id, bytes.as_bytes())
        {
            return false;
        }
        self.section_cache.borrow_mut().add(
            resource_id,
            bytes.len(),
            Arc::new(section.clone()),
        );
        true
    }

    /// Reads the whole cache (hits/misses/order) for diagnostics and tests,
    /// the way the reference watcher exposes `cache/*`.
    pub fn section_cache_stats(&self) -> (usize, usize, usize) {
        let cache = self.section_cache.borrow();
        (cache.hits(), cache.misses(), cache.current_bytes())
    }
}

/// `::getenv("BW_CACHE_SIZE")` + `atoi` from postAddPaths: a missing
/// variable keeps `DEFAULT_CACHE_SIZE`; a present one parses as C `atoi`
/// (see `atoi_or_zero`).
fn cache_size_from(raw: Option<&str>) -> usize {
    match raw {
        Some(raw) => atoi_or_zero(raw),
        None => DEFAULT_CACHE_SIZE,
    }
}

/// C `atoi` on decimal input.
fn atoi_or_zero(raw: &str) -> usize {
    let trimmed = raw.trim_start();
    let (negative, digits_start) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, 1),
        Some(b'+') => (false, 1),
        _ => (false, 0),
    };
    let rest = &trimmed[digits_start..];
    let digits = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("");
    let value: usize = digits.parse().unwrap_or(0);
    if negative {
        0
    } else {
        value
    }
}

/// `BWUtil::executableDirectory`: the directory of the running image,
/// empty when it cannot be determined.
fn executable_directory() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|dir| dir.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// `getUserId()`: the `UID` environment variable parsed with atoi when
/// present (even when it does not name a number -- atoi yields 0), else
/// the process's real uid, read from the `/proc/self` owner.
fn current_uid() -> u32 {
    current_uid_from(std::env::var("UID").ok().as_deref())
}

/// The injectable core of [`current_uid`].
fn current_uid_from(env_uid: Option<&str>) -> u32 {
    match env_uid {
        Some(raw) => atoi_or_zero(raw) as u32,
        None => std::fs::metadata("/proc/self")
            .map(|meta| meta.uid())
            .unwrap_or(0),
    }
}

/// The home directory `/etc/passwd` lists for `uid` (the reference asks
/// `getpwuid`); `None` when no entry matches.
fn passwd_home(uid: u32) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 6
            && fields[2].parse::<u32>() == Ok(uid)
        {
            return Some(fields[5].to_string());
        }
    }
    None
}

/// `sscanf(line, "%*[^;];%s")` on a `~/.bwmachined.conf` line: the value
/// is the first whitespace-delimited token after the first `;`. A line
/// with an empty head before the `;` or no token after it fails the
/// reference's `== 1` check and yields `None`.
fn machined_conf_res_path(line: &str) -> Option<String> {
    let (head, tail) = line.split_once(';')?;
    if head.is_empty() {
        return None;
    }
    let tail = tail.trim_start();
    let end = tail
        .find(|c: char| c.is_whitespace())
        .unwrap_or(tail.len());
    if end == 0 {
        return None;
    }
    Some(tail[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A `home_of_uid` lookup that never finds a home.
    fn no_home(_: u32) -> Option<String> {
        None
    }

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

    #[test]
    fn open_section_serves_cache_hits_and_counts_them() {
        let a = temp_root("cache_hits");
        write(&a, "/etc/chars/server.xml", b"<server><value>old</value></server>");

        let resource = BwResource::init_paths(&[&a]);
        // First open: a miss that loads from disk and caches the section.
        let first = resource.open_section("etc/chars/server.xml", false).unwrap();
        assert_eq!(first.section_name(), "server.xml");
        assert_eq!(first.find_child("value").unwrap().as_string(), "old");
        assert_eq!(resource.section_cache_stats(), (0, 1, first_size()));

        // Second open: the same Arc comes straight out of the LRU.
        let second = resource.open_section("etc/chars/server.xml", false).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(resource.section_cache_stats(), (1, 1, first_size()));

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn overwrite_only_becomes_visible_after_purge() {
        let a = temp_root("cache_overwrite");
        write(&a, "/game.xml", b"<game><speed>1</speed></game>");

        let resource = BwResource::init_paths(&[&a]);
        let stale = resource.open_section("game.xml", false).unwrap();
        assert_eq!(stale.find_child("speed").unwrap().as_string(), "1");

        // Overwrite behind the cache's back: opens keep serving the stale
        // section -- this is the acceptance proof that the cache exists.
        write(&a, "/game.xml", b"<game><speed>2</speed></game>");
        let still_stale = resource.open_section("game.xml", false).unwrap();
        assert!(Arc::ptr_eq(&stale, &still_stale));
        assert_eq!(
            still_stale.find_child("speed").unwrap().as_string(),
            "1"
        );

        // purge drops exactly this path; the next open re-reads the file.
        resource.purge("game.xml", false);
        let fresh = resource.open_section("game.xml", false).unwrap();
        assert!(!Arc::ptr_eq(&stale, &fresh));
        assert_eq!(fresh.find_child("speed").unwrap().as_string(), "2");

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn recursive_purge_clears_directory_subtrees() {
        let a = temp_root("cache_recursive");
        write(&a, "/root.xml", b"<r><a>1</a></r>");
        write(&a, "/scripts/inner.xml", b"<s><b>1</b></s>");
        write(&a, "/scripts/deep/leaf.xml", b"<l><c>1</c></l>");

        let resource = BwResource::init_paths(&[&a]);
        for path in ["root.xml", "scripts/inner.xml", "scripts/deep/leaf.xml"] {
            resource.open_section(path, false).unwrap();
        }
        let (_, _, bytes_before) = resource.section_cache_stats();
        assert!(bytes_before > 0);

        // Non-recursive purge of the directory touches nothing inside.
        resource.purge("scripts", false);
        let cached = resource.open_section("scripts/inner.xml", false).unwrap();
        assert_eq!(cached.find_child("b").unwrap().as_string(), "1");

        // Recursive purge walks the whole subtree, then the directory.
        write(&a, "/scripts/inner.xml", b"<s><b>2</b></s>");
        write(&a, "/scripts/deep/leaf.xml", b"<l><c>2</c></l>");
        resource.purge("scripts", true);
        let inner = resource.open_section("scripts/inner.xml", false).unwrap();
        let leaf = resource.open_section("scripts/deep/leaf.xml", false).unwrap();
        assert_eq!(inner.find_child("b").unwrap().as_string(), "2");
        assert_eq!(leaf.find_child("c").unwrap().as_string(), "2");
        // root.xml was never purged: it still serves the cached copy.
        let root = resource.open_section("root.xml", false).unwrap();
        assert_eq!(root.find_child("a").unwrap().as_string(), "1");

        // Purging the empty root path recurses over every top entry.
        write(&a, "/scripts/inner.xml", b"<s><b>3</b></s>");
        resource.purge("", true);
        let inner = resource.open_section("scripts/inner.xml", false).unwrap();
        assert_eq!(inner.find_child("b").unwrap().as_string(), "3");
        let root = resource.open_section("root.xml", false).unwrap();
        assert_eq!(root.find_child("a").unwrap().as_string(), "1");

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn purge_all_and_cache_size_rebuild() {
        let a = temp_root("cache_purge_all");
        write(&a, "/one.xml", b"<one/>");
        write(&a, "/two.xml", b"<two/>");

        let resource = BwResource::init_paths(&[&a]);
        resource.open_section("one.xml", false).unwrap();
        resource.open_section("two.xml", false).unwrap();
        assert!(resource.section_cache_stats().2 > 0);

        // purgeAll empties the cache *and* the statistics.
        resource.purge_all();
        assert_eq!(resource.section_cache_stats(), (0, 0, 0));
        resource.open_section("one.xml", false).unwrap();
        assert_eq!(resource.section_cache_stats(), (0, 1, one_size()));

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn content_sniffing_and_uncached_sections_match_the_reference() {
        let a = temp_root("cache_sniff");
        write(&a, "/looks.xml", b"definitely not xml");
        // Leading whitespace before '<' still parses as XML.
        write(&a, "/padded.xml", b"  \r\n\t<value>pad</value>");
        // A parse failure leaves nothing in the cache.
        write(&a, "/broken.xml", b"<unclosed>oops");

        let resource = BwResource::init_paths(&[&a]);
        // BinSection content has no section at this level and never caches.
        assert_eq!(resource.open_section("looks.xml", false), None);
        assert_eq!(resource.section_cache_stats(), (0, 1, 0));
        // The padded file parses; its root keeps the leaf name.
        let padded = resource.open_section("padded.xml", false).unwrap();
        assert_eq!(padded.section_name(), "padded.xml");
        assert_eq!(padded.as_string(), "pad");
        // Invalid XML: no section, no cache entry.
        assert_eq!(resource.open_section("broken.xml", false), None);
        assert_eq!(resource.section_cache_stats(), (0, 3, padded_size()));

        // Missing paths return nothing unless makeNewSection asks for an
        // empty dotted section, which then lives in the cache.
        assert_eq!(resource.open_section("settings/missing.xml", false), None);
        let created =
            resource.open_section("settings/missing.xml", true).unwrap();
        assert_eq!(created.section_name(), "missing.xml");
        assert_eq!(created.count_children(), 0);
        let again =
            resource.open_section("settings/missing.xml", false).unwrap();
        assert!(Arc::ptr_eq(&created, &again));
        // Undotted tags would create directories (DirSection, later task).
        assert_eq!(resource.open_section("settings/plaindir", true), None);

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn atoi_reads_the_bw_cache_size_like_c() {
        assert_eq!(atoi_or_zero("1024"), 1024);
        assert_eq!(atoi_or_zero(" 96"), 96);
        assert_eq!(atoi_or_zero("+7"), 7);
        assert_eq!(atoi_or_zero("12trailing"), 12);
        // Negative budgets cache nothing (nothing fits), like the int math.
        assert_eq!(atoi_or_zero("-5"), 0);
        assert_eq!(atoi_or_zero(""), 0);
        assert_eq!(atoi_or_zero("junk"), 0);
        // A missing variable keeps the default; a present one goes through
        // the atoi path.
        assert_eq!(cache_size_from(None), DEFAULT_CACHE_SIZE);
        assert_eq!(cache_size_from(Some("64")), 64);
        assert_eq!(DEFAULT_CACHE_SIZE, 100 * 1024);
    }

    /// Serialised size of `<server><value>old</value></server>`.
    fn first_size() -> usize {
        let mut section =
            DataSection::new("server.xml");
        let value = section.add_child("value");
        value.set_string("old");
        section.write_to_string().len()
    }

    /// Serialised size of `<one/>` written back out.
    fn one_size() -> usize {
        let mut section = DataSection::new("one.xml");
        section.write_to_string().len()
    }

    /// Serialised size of `  \r\n\t<value>pad</value>`.
    fn padded_size() -> usize {
        let mut section = DataSection::new("padded.xml");
        section.set_string("pad");
        section.write_to_string().len()
    }

    #[test]
    fn saved_xml_is_byte_exact_with_the_reference_layout() {
        let a = temp_root("save_exact");
        let resource = BwResource::init_paths(&[&a]);

        let source = "<npc name=\"Bob\">\n\t<position>12.5 3 -7</position>\
\n\t<inventory>\n\t\t<item />\n\t\t<item>sword</item>\n\t</inventory>\
\n\t<flags/>\n</npc>";
        let mut tree = datasection::parse("", source.as_bytes()).unwrap();
        assert!(resource.save_section("npc.xml", &mut tree));

        // The file on disk is byte-exact with XMLSection::writeToStream:
        // attribute lines carry level+2 tabs, leaf values sit inline after
        // one tab and keep the closing tab, empty nodes collapse to
        // `<tag />`, and every element ends with a newline.
        let raw = fs::read(format!("{}/npc.xml", a)).unwrap();
        let expected = "<npc\n\t\tname=\"Bob\">\n\
\t<position>\t12.5 3 -7\t</position>\n\
\t<inventory>\n\
\t\t<item />\n\
\t\t<item>\tsword\t</item>\n\
\t</inventory>\n\
\t<flags />\n\
</npc>\n";
        assert_eq!(raw, expected.as_bytes());

        // The original engine reads what we wrote: re-parsing the saved
        // bytes (the pinned createFromBinary semantics) rebuilds the same
        // tree, attribute and repeated children included.
        let reread = datasection::parse("", &raw).unwrap();
        let original = datasection::parse("", source.as_bytes()).unwrap();
        assert_eq!(reread, original);

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn save_then_update_flows_through_the_cache() {
        let a = temp_root("save_update");
        write(&a, "/cfg/old.xml", b"<cfg><speed>1</speed></cfg>");
        let resource = BwResource::init_paths(&[&a]);

        // Saving a v2 tree over an existing file: the cache immediately
        // serves the saved tree, no purge needed.
        let mut tree = datasection::parse("", b"<cfg><speed>2</speed></cfg>").unwrap();
        assert!(resource.save_section("cfg/old.xml", &mut tree));
        let opened = resource.open_section("cfg/old.xml", false).unwrap();
        assert_eq!(opened.find_child("speed").unwrap().as_string(), "2");
        // The disk copy carries the serialised form.
        assert_eq!(
            fs::read(format!("{}/cfg/old.xml", a)).unwrap(),
            b"<cfg>\n\t<speed>\t2\t</speed>\n</cfg>\n"
        );

        // Update: mutate and save again; opens serve the new tree.
        tree.child_mut(0).unwrap().set_string("3");
        assert!(resource.save_section("cfg/old.xml", &mut tree));
        let opened = resource.open_section("cfg/old.xml", false).unwrap();
        assert_eq!(opened.find_child("speed").unwrap().as_string(), "3");

        // After a purge the disk copy confirms the same value: write and
        // cache agree.
        resource.purge("cfg/old.xml", false);
        let reopened = resource.open_section("cfg/old.xml", false).unwrap();
        assert_eq!(reopened.find_child("speed").unwrap().as_string(), "3");

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn save_if_exists_lands_in_the_root_that_holds_the_file() {
        let first = temp_root("save_if_first");
        let second = temp_root("save_if_second");
        write(&second, "/keep/old.xml", b"<cfg><v>1</v></cfg>");

        let resource = BwResource::init_paths(&[&first, &second]);
        let mut tree = datasection::parse("", b"<cfg><v>2</v></cfg>").unwrap();

        // Plain save writes through the first base that accepts: the first
        // root has no keep/ directory (fopen("w") fails), so the fall-through
        // overwrites the file in the second root.
        assert!(resource.save_section("keep/old.xml", &mut tree));
        assert_eq!(
            fs::read(format!("{}/keep/old.xml", second)).unwrap(),
            b"<cfg>\n\t<v>\t2\t</v>\n</cfg>\n"
        );
        assert!(!std::path::Path::new(&format!("{}/keep/old.xml", first)).exists());

        // When both roots hold the file, the Exists variant writes the
        // first root and leaves the second untouched.
        write(&first, "/keep/shared.xml", b"<cfg><v>1</v></cfg>");
        assert!(resource.save_section_if_exists("keep/shared.xml", &mut tree));
        assert_eq!(
            fs::read(format!("{}/keep/shared.xml", first)).unwrap(),
            b"<cfg>\n\t<v>\t2\t</v>\n</cfg>\n"
        );
        assert_eq!(
            fs::read(format!("{}/keep/shared.xml", second))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );

        // A path no root holds fails and stays out of the cache; plain
        // save fails the same way when no root can create the parents.
        let (hits, misses, bytes) = resource.section_cache_stats();
        assert!(bytes > 0);
        assert!(!resource.save_section_if_exists("absent.xml", &mut tree));
        assert!(!resource.save_section("no/such/dir.xml", &mut tree));
        assert_eq!(resource.section_cache_stats(), (hits, misses, bytes));
        assert_eq!(resource.open_section("absent.xml", false), None);

        fs::remove_dir_all(&first).unwrap();
        fs::remove_dir_all(&second).unwrap();
    }

    #[test]
    fn the_same_command_resolves_identical_paths() {
        let a = temp_root("init_det");
        fs::create_dir(format!("{}/res1", a)).unwrap();
        fs::create_dir(format!("{}/res2", a)).unwrap();
        let canonical = |name: &str| {
            fs::canonicalize(format!("{}/{}", a, name))
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };

        let args = vec![
            "cellapp".to_string(),
            "--res".to_string(),
            "res1:res2".to_string(),
        ];
        // Same command twice: the same configuration paths, in the same
        // order -- the acceptance determinism.
        let mut one = BwResource::new();
        let (ok_one, rem_one) =
            one.init_from_sources(&args, &a, 1000, &no_home, None);
        let mut two = BwResource::new();
        let (ok_two, rem_two) =
            two.init_from_sources(&args, &a, 1000, &no_home, None);

        assert!(ok_one && ok_two);
        assert_eq!(one.get_path_num(), 2);
        assert_eq!(one.get_path(0), canonical("res1"));
        assert_eq!(one.get_path(1), canonical("res2"));
        assert_eq!(one.get_path(0), two.get_path(0));
        assert_eq!(one.get_path(1), two.get_path(1));
        // compressArgs dropped the consumed pairs and kept the rest.
        assert_eq!(rem_one, vec!["cellapp".to_string()]);
        assert_eq!(rem_two, rem_one);

        // Absolute parts skip the executable anchoring and pass through
        // verbatim (canonicalisation is a relative-branch concern).
        let abs = format!("{}/res1", a);
        let mut three = BwResource::new();
        three.init_from_sources(
            &["cellapp".to_string(), "-r".to_string(), abs.clone()],
            &a,
            1000,
            &no_home,
            None,
        );
        assert_eq!(three.get_path(0), abs);

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn argv_scan_matches_the_reference_bounds() {
        let a = temp_root("init_argv");
        fs::create_dir(format!("{}/res1", a)).unwrap();
        let canonical = fs::canonicalize(format!("{}/res1", a))
            .unwrap()
            .to_string_lossy()
            .into_owned();

        // A flag in the final slot never matches and survives.
        let mut resource = BwResource::new();
        let (ok, remaining) = resource.init_from_sources(
            &["prog".to_string(), "--res".to_string()],
            &a,
            1000,
            &no_home,
            None,
        );
        assert!(!ok);
        assert_eq!(
            remaining,
            vec!["prog".to_string(), "--res".to_string()]
        );
        assert_eq!(resource.get_path_num(), 0);

        // -uid consumes its value and rewires the conf lookup: only uid
        // 1001 finds the fixture home, so the surviving res path must have
        // come through that branch.
        fs::create_dir(format!("{}/uid1001", a)).unwrap();
        fs::write(
            format!("{}/uid1001/.bwmachined.conf", a),
            "machined;res1\n",
        )
        .unwrap();
        let mut resource = BwResource::new();
        let home_of_uid = |uid: u32| {
            if uid == 1001 {
                Some(format!("{}/uid1001", a))
            } else {
                None
            }
        };
        let (ok, remaining) = resource.init_from_sources(
            &["prog".to_string(), "-uid".to_string(), "1001".to_string()],
            &a,
            1000,
            &home_of_uid,
            None,
        );
        assert!(ok);
        assert_eq!(remaining, vec!["prog".to_string()]);
        assert_eq!(resource.get_path(0), canonical);

        // A different -uid misses the same lookup and resolves nothing:
        // the flag value drives the branch.
        let mut resource = BwResource::new();
        let (ok, _) = resource.init_from_sources(
            &["prog".to_string(), "-uid".to_string(), "2000".to_string()],
            &a,
            1000,
            &home_of_uid,
            None,
        );
        assert!(!ok);

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn fallback_chain_runs_env_then_machined_conf() {
        let a = temp_root("init_fallback");
        fs::create_dir(format!("{}/res1", a)).unwrap();
        fs::create_dir(format!("{}/res2", a)).unwrap();
        let canonical = |name: &str| {
            fs::canonicalize(format!("{}/{}", a, name))
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };

        // Command-line paths silence the env variable entirely.
        let mut resource = BwResource::new();
        let (ok, _) = resource.init_from_sources(
            &[],
            &a,
            1000,
            &no_home,
            Some(&format!("{}/res1", a)),
        );
        assert!(ok);
        assert_eq!(resource.get_path(0), format!("{}/res1", a));

        // A -r that resolves nothing leaves the chain open: the env
        // variable answers next.
        let mut resource = BwResource::new();
        let (ok, _) = resource.init_from_sources(
            &["prog".to_string(), "-r".to_string(), "missing".to_string()],
            &a,
            1000,
            &no_home,
            Some("res1:res2"),
        );
        assert!(ok);
        assert_eq!(resource.get_path(0), canonical("res1"));
        assert_eq!(resource.get_path(1), canonical("res2"));

        // Without env, the conf file answers: the first usable line wins
        // even when later lines name live paths, comments and invalid
        // lines never register.
        let home = temp_root("init_home");
        fs::write(
            format!("{}/.bwmachined.conf", home),
            "# comment\n   \n;<nocolon>\nbroken;\nmissing\nmachined;res2:res1\nmachined;res1\n",
        )
        .unwrap();
        let mut resource = BwResource::new();
        let (ok, remaining) =
            resource.init_from_sources(&[], &a, 1000, &|_| Some(home.clone()), None);
        assert!(ok);
        assert_eq!(resource.get_path_num(), 2);
        assert_eq!(resource.get_path(0), canonical("res2"));
        assert_eq!(resource.get_path(1), canonical("res1"));
        assert_eq!(remaining, Vec::<String>::new());

        // Nothing resolvable: init fails but reports the arguments whole.
        let empty_home = temp_root("init_empty");
        let mut resource = BwResource::new();
        let (ok, remaining) = resource.init_from_sources(
            &["prog".to_string(), "x".to_string()],
            &a,
            1000,
            &|_| Some(empty_home.clone()),
            None,
        );
        assert!(!ok);
        assert_eq!(resource.get_path_num(), 0);
        assert_eq!(
            remaining,
            vec!["prog".to_string(), "x".to_string()]
        );

        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&home).unwrap();
        fs::remove_dir_all(&empty_home).unwrap();
    }

    #[test]
    fn machined_conf_lines_parse_like_sscanf() {
        assert_eq!(
            machined_conf_res_path("machined;<res>/path</res>"),
            Some("<res>/path</res>".to_string())
        );
        // %s stops at whitespace.
        assert_eq!(
            machined_conf_res_path("head; first rest"),
            Some("first".to_string())
        );
        // An empty head before ';' fails the scanf match, as does a
        // missing token after it.
        assert_eq!(machined_conf_res_path(";value"), None);
        assert_eq!(machined_conf_res_path("head;   "), None);
        assert_eq!(machined_conf_res_path("no semicolon"), None);
    }

    #[test]
    fn init_from_args_wraps_the_real_environment() {
        // The real-environment wrapper: with no flags to consume, the
        // arguments survive whole. Whether a res path resolves depends on
        // the host (env variable, machined conf), so only the mechanics
        // are pinned.
        let args = vec!["cellapp".to_string(), "extra".to_string()];
        let mut resource = BwResource::new();
        let (ok, remaining) = resource.init_from_args(&args);
        let _ = ok;
        assert_eq!(remaining, args);
    }

    #[test]
    fn init_drops_unreadable_archives_and_rebuilds_late_adds() {
        let a = temp_root("init_badzip");
        // A *.zip path that is not an archive is dropped from the paths
        // (the reference's "not a valid zip file" branch): anchored under
        // plain/ it neither canonicalises nor exists from the working
        // directory.
        fs::write(format!("{}/bad.zip", a), b"not a zip at all").unwrap();
        fs::create_dir(format!("{}/plain", a)).unwrap();
        let plain = format!("{}/plain", a);
        let mut resource = BwResource::new();
        let (ok, _) = resource.init_from_sources(
            &[
                "prog".to_string(),
                "--res".to_string(),
                format!("{}:{}", a, "bad.zip"),
            ],
            &plain,
            1000,
            &no_home,
            None,
        );
        assert!(ok);
        assert_eq!(resource.get_path_num(), 1);
        // The bad zip contributed no root: the directory is the only base.
        assert_eq!(resource.file_system().base_file_system_count(), 1);

        // A directory whose res.zip package is corrupt yields no root at
        // all: the package consumes the path and the raw directory branch
        // is skipped.
        fs::create_dir(format!("{}/withpack", a)).unwrap();
        fs::write(format!("{}/withpack/res.zip", a), b"garbage").unwrap();
        let mut resource = BwResource::new();
        let (ok, _) = resource.init_from_sources(
            &[
                "prog".to_string(),
                "--res".to_string(),
                format!("{}/withpack", a),
            ],
            &plain,
            1000,
            &no_home,
            None,
        );
        assert!(ok);
        assert_eq!(resource.get_path_num(), 1);
        assert_eq!(resource.file_system().base_file_system_count(), 0);

        // add_paths_with_source on an already-built instance triggers the
        // same rebuild the late addPath does.
        fs::create_dir(format!("{}/late", a)).unwrap();
        let mut built = BwResource::init_paths(&[&plain]);
        assert_eq!(built.file_system().base_file_system_count(), 1);
        built.add_paths_with_source(&format!("{}/late", a), "later", "");
        assert_eq!(built.get_path_num(), 2);
        assert_eq!(built.file_system().base_file_system_count(), 2);

        fs::remove_dir_all(&a).unwrap();
    }

    #[test]
    fn uid_helpers_read_the_running_account() {
        // A present-but-junk UID lands on 0 through atoi, like the
        // reference getenv branch.
        assert_eq!(current_uid_from(Some("not-a-number")), 0);
        assert_eq!(current_uid_from(Some("1001")), 1001);
        // The env-free path falls back to the process uid, whose passwd
        // entry exists.
        let uid = current_uid_from(None);
        assert!(passwd_home(uid).is_some());
        // Root is always in /etc/passwd; an absurd uid is not.
        assert!(passwd_home(0).is_some());
        assert_eq!(passwd_home(u32::MAX - 1), None);
    }
}

