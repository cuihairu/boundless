//! Zip archives, an exact port of `resmgr/zip_file_system.cpp` — the read
//! side (`openZip`, `readFile`, `getFileType`, directory listings) plus the
//! in-memory structure writes (`updateFile`, `writeToStructure`, `createZip`).
//!
//! The reference reads the archive through a `FILE*`; this port holds the
//! whole archive in memory and slices it, which keeps every offset and
//! validation order identical while dropping the file-handle plumbing.
//! Nested zip nodes (`ZipFileSystem::childNode` forwarding) arrive with the
//! ZipSection/DataSection layer and are absent here.

use std::collections::HashMap;

pub const DIR_FOOTER_SIGNATURE: u32 = 0x0605_4b50;
pub const DIR_ENTRY_SIGNATURE: u32 = 0x0201_4b50;
pub const LOCAL_HEADER_SIGNATURE: u32 = 0x0403_4b50;

/// Field-length guard applied to every directory entry component.
pub const MAX_FIELD_LENGTH: usize = 1024;

pub const METHOD_STORE: u16 = 0;
pub const METHOD_DEFLATE: u16 = 8;

/// `ZIP_HEADER_MAGIC`: the local-header signature doubles as the "is this a
/// zip" test (`ZipFileSystem::zipTest`).
pub const ZIP_HEADER_MAGIC: u32 = LOCAL_HEADER_SIGNATURE;

/// `IFileSystem::FileType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    NotFound,
    File,
    Directory,
    Archive,
}

/// Optional per-file metadata filled by `get_file_type`/`get_file_type_ex`
/// (`IFileSystem::FileInfo` with the non-Windows DOS packed time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileInfo {
    pub size: u32,
    pub created: u32,
    pub modified: u32,
    pub accessed: u32,
}

/// `LocalHeader`: the record preceding every file's data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalHeader {
    pub signature: u32,
    pub extractor_version: u16,
    pub mask: u16,
    pub compression_method: u16,
    pub modified_time: u16,
    pub modified_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub filename_length: u16,
    pub extra_field_length: u16,
}

pub const LOCAL_HEADER_SIZE: usize = 30;

impl LocalHeader {
    fn parse(bytes: &[u8]) -> LocalHeader {
        LocalHeader {
            signature: u32_le(bytes, 0),
            extractor_version: u16_le(bytes, 4),
            mask: u16_le(bytes, 6),
            compression_method: u16_le(bytes, 8),
            modified_time: u16_le(bytes, 10),
            modified_date: u16_le(bytes, 12),
            crc32: u32_le(bytes, 14),
            compressed_size: u32_le(bytes, 18),
            uncompressed_size: u32_le(bytes, 22),
            filename_length: u16_le(bytes, 26),
            extra_field_length: u16_le(bytes, 28),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.signature.to_le_bytes());
        out.extend_from_slice(&self.extractor_version.to_le_bytes());
        out.extend_from_slice(&self.mask.to_le_bytes());
        out.extend_from_slice(&self.compression_method.to_le_bytes());
        out.extend_from_slice(&self.modified_time.to_le_bytes());
        out.extend_from_slice(&self.modified_date.to_le_bytes());
        out.extend_from_slice(&self.crc32.to_le_bytes());
        out.extend_from_slice(&self.compressed_size.to_le_bytes());
        out.extend_from_slice(&self.uncompressed_size.to_le_bytes());
        out.extend_from_slice(&self.filename_length.to_le_bytes());
        out.extend_from_slice(&self.extra_field_length.to_le_bytes());
    }
}

/// `DirEntry`: the central-directory record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirEntry {
    pub signature: u32,
    pub creator_version: u16,
    pub extractor_version: u16,
    pub mask: u16,
    pub compression_method: u16,
    pub modified_time: u16,
    pub modified_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub filename_length: u16,
    pub extra_field_length: u16,
    pub file_comment_length: u16,
    pub disk_number_start: u16,
    pub internal_file_attr: u16,
    pub external_file_attr: u32,
    /// Signed in the reference; `-1` marks structure-only placeholder rows.
    pub local_header_offset: i32,
}

pub const DIR_ENTRY_SIZE: usize = 46;

impl DirEntry {
    fn parse(bytes: &[u8]) -> DirEntry {
        DirEntry {
            signature: u32_le(bytes, 0),
            creator_version: u16_le(bytes, 4),
            extractor_version: u16_le(bytes, 6),
            mask: u16_le(bytes, 8),
            compression_method: u16_le(bytes, 10),
            modified_time: u16_le(bytes, 12),
            modified_date: u16_le(bytes, 14),
            crc32: u32_le(bytes, 16),
            compressed_size: u32_le(bytes, 20),
            uncompressed_size: u32_le(bytes, 24),
            filename_length: u16_le(bytes, 28),
            extra_field_length: u16_le(bytes, 30),
            file_comment_length: u16_le(bytes, 32),
            disk_number_start: u16_le(bytes, 34),
            internal_file_attr: u16_le(bytes, 36),
            external_file_attr: u32_le(bytes, 38),
            local_header_offset: i32_le(bytes, 42),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.signature.to_le_bytes());
        out.extend_from_slice(&self.creator_version.to_le_bytes());
        out.extend_from_slice(&self.extractor_version.to_le_bytes());
        out.extend_from_slice(&self.mask.to_le_bytes());
        out.extend_from_slice(&self.compression_method.to_le_bytes());
        out.extend_from_slice(&self.modified_time.to_le_bytes());
        out.extend_from_slice(&self.modified_date.to_le_bytes());
        out.extend_from_slice(&self.crc32.to_le_bytes());
        out.extend_from_slice(&self.compressed_size.to_le_bytes());
        out.extend_from_slice(&self.uncompressed_size.to_le_bytes());
        out.extend_from_slice(&self.filename_length.to_le_bytes());
        out.extend_from_slice(&self.extra_field_length.to_le_bytes());
        out.extend_from_slice(&self.file_comment_length.to_le_bytes());
        out.extend_from_slice(&self.disk_number_start.to_le_bytes());
        out.extend_from_slice(&self.internal_file_attr.to_le_bytes());
        out.extend_from_slice(&self.external_file_attr.to_le_bytes());
        out.extend_from_slice(&self.local_header_offset.to_le_bytes());
    }
}

/// `DirFooter`: the end-of-central-directory record (assumed to be the last
/// thing in the file — the reference makes the same assumption).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirFooter {
    pub signature: u32,
    pub disk_number: u16,
    pub dir_start_disk: u16,
    pub dir_entries: u16,
    pub total_dir_entries: u16,
    pub dir_size: u32,
    pub dir_offset: u32,
    pub comment_length: u16,
}

pub const DIR_FOOTER_SIZE: usize = 22;

impl DirFooter {
    fn parse(bytes: &[u8]) -> DirFooter {
        DirFooter {
            signature: u32_le(bytes, 0),
            disk_number: u16_le(bytes, 4),
            dir_start_disk: u16_le(bytes, 6),
            dir_entries: u16_le(bytes, 8),
            total_dir_entries: u16_le(bytes, 10),
            dir_size: u32_le(bytes, 12),
            dir_offset: u32_le(bytes, 16),
            comment_length: u16_le(bytes, 20),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.signature.to_le_bytes());
        out.extend_from_slice(&self.disk_number.to_le_bytes());
        out.extend_from_slice(&self.dir_start_disk.to_le_bytes());
        out.extend_from_slice(&self.dir_entries.to_le_bytes());
        out.extend_from_slice(&self.total_dir_entries.to_le_bytes());
        out.extend_from_slice(&self.dir_size.to_le_bytes());
        out.extend_from_slice(&self.dir_offset.to_le_bytes());
        out.extend_from_slice(&self.comment_length.to_le_bytes());
    }
}

/// One archive row (`ZipFileSystem::LocalFile`).
///
/// `data` mirrors the reference's `pData_`: it holds the bytes exactly as
/// they sit on disk after `update_header` compresses them, and repeated
/// reads bypass it (`readFileInternal`'s early return) — a narrow window
/// the reference exposes too.
#[derive(Debug, Clone, Default)]
pub struct LocalFile {
    pub filename: String,
    pub data: Option<Vec<u8>>,
    pub header: LocalHeader,
    pub entry: DirEntry,
    local_offset: u32,
    is_folder: bool,
    b_compressed: bool,
}

impl LocalFile {
    /// `LocalFile(filename, data)`: compresses immediately and stamps the
    /// modified time.
    pub fn new(filename: &str, data: Vec<u8>) -> LocalFile {
        let mut file = LocalFile {
            filename: filename.to_string(),
            ..Default::default()
        };
        file.entry.local_header_offset = -1;
        let (date, time) = dos_timestamp(now_unix());
        file.update_header_with_time(data, date, time);
        file
    }

    /// Test/fixture variant of [`LocalFile::new`] with a fixed DOS stamp.
    pub fn with_stamp(filename: &str, data: Vec<u8>, date: u16, time: u16) -> LocalFile {
        let mut file = LocalFile {
            filename: filename.to_string(),
            ..Default::default()
        };
        file.entry.local_header_offset = -1;
        file.update_header_with_time(data, date, time);
        file
    }

    /// `LocalFile::update(filename, data)`: new content, header rebuilt.
    pub fn update(&mut self, filename: &str, data: Vec<u8>) {
        self.filename = filename.to_string();
        self.b_compressed = false;
        let (date, time) = dos_timestamp(now_unix());
        self.update_header_with_time(data, date, time);
    }

    /// `LocalFile::update(filename)`: rename only, header refreshed.
    pub fn rename(&mut self, filename: &str) {
        self.filename = filename.to_string();
        self.update_header();
    }

    /// `LocalFile::isValid`: only structure rows have no data.
    pub fn is_valid(&self) -> bool {
        self.data.is_some()
    }

    pub fn compressed_size(&self) -> u32 {
        self.entry.compressed_size
    }

    pub fn uncompressed_size(&self) -> u32 {
        self.entry.uncompressed_size
    }

    /// `LocalFile::clear`.
    pub fn clear(&mut self) {
        self.header = LocalHeader::default();
        self.data = None;
        self.filename.clear();
    }

    /// The on-disk name: folders carry a trailing slash
    /// (`LocalFile::diskName`).
    pub fn disk_name(&self) -> String {
        if self.is_folder {
            format!("{}/", self.filename)
        } else {
            self.filename.clone()
        }
    }

    pub fn is_folder(&self) -> bool {
        self.is_folder
    }

    pub fn set_folder(&mut self, is_folder: bool) {
        self.is_folder = is_folder;
    }

    /// Bytes this row occupies in a freshly built archive
    /// (`LocalFile::sizeOnDisk`): the name appears in both the local header
    /// and the central directory.
    pub fn size_on_disk(&self) -> u32 {
        LOCAL_HEADER_SIZE as u32
            + self.disk_name().len() as u32 * 2
            + self.data.as_ref().map_or(0, |d| d.len() as u32)
            + DIR_ENTRY_SIZE as u32
    }

    /// `LocalFile::updateHeader(clear=true, ...)`: reset the header,
    /// install fresh bytes, then rebuild — with a stamped time.
    ///
    /// `can_zip` is unconditionally true in this port: the reference's
    /// `BinaryBlock::canZip_` only goes false for blocks produced by its
    /// own compress()/decompress() pair (BW-01-02-04).
    fn update_header_with_time(&mut self, data: Vec<u8>, date: u16, time: u16) {
        self.update_header_keep_data();
        if !self.b_compressed {
            self.data = Some(data);
        }
        self.update_header_common(date, time);
    }

    /// Rename/serialise path (`updateHeader(false, false)`): re-derive
    /// header fields without touching `data` or the stored time.
    fn update_header(&mut self) {
        let date = self.header.modified_date;
        let time = self.header.modified_time;
        self.update_header_common(date, time);
    }

    /// The `clear` half of `updateHeader`: reset, preserving the stored
    /// timestamp (the reference's `updateModifiedTime=false` branch).
    fn update_header_keep_data(&mut self) {
        let date = self.header.modified_date;
        let time = self.header.modified_time;
        self.header = LocalHeader::default();
        self.header.modified_date = date;
        self.header.modified_time = time;
    }

    /// The common tail of `updateHeader`: signature, lengths, and the
    /// compress-or-store decision.
    fn update_header_common(&mut self, date: u16, time: u16) {
        self.header.signature = LOCAL_HEADER_SIGNATURE;
        self.header.filename_length = self.disk_name().len() as u16;
        self.header.extra_field_length = 0;
        self.header.extractor_version = 10;

        if !self.b_compressed && self.data.is_some() {
            // The reference latches bCompressed_ before deciding, so a
            // later rename's updateHeader never re-walks this block.
            self.b_compressed = true;
            let data = self.data.take().unwrap();
            self.header.crc32 = crc32fast::hash(&data);
            self.header.uncompressed_size = data.len() as u32;

            let compressed = if !zip_test(&data) {
                compress_data(&data)
            } else {
                None
            };
            match compressed {
                Some(c) => {
                    self.header.compression_method = METHOD_DEFLATE;
                    self.data = Some(c);
                }
                None => {
                    self.header.compression_method = METHOD_STORE;
                    self.data = Some(data);
                }
            }
            self.header.compressed_size = self.data.as_ref().unwrap().len() as u32;
        }

        self.header.modified_date = date;
        self.header.modified_time = time;
    }

    /// `LocalFile::writeFile(BinaryPtr&, uint32&)`.
    fn write_file(&mut self, dst: &mut Vec<u8>, offset: &mut u32) {
        // In case the filename and header length drifted apart.
        self.update_header();
        self.local_offset = *offset;

        self.header.write(dst);
        *offset += LOCAL_HEADER_SIZE as u32;

        let name = self.disk_name();
        dst.extend_from_slice(name.as_bytes());
        *offset += name.len() as u32;

        let data = self.data.clone().unwrap_or_default();
        dst.extend_from_slice(&data);
        *offset += data.len() as u32;
    }

    /// `LocalFile::writeDirEntry`.
    fn write_dir_entry(&mut self, dst: &mut Vec<u8>, offset: &mut u32) {
        let name = self.disk_name();
        self.entry = DirEntry {
            signature: DIR_ENTRY_SIGNATURE,
            creator_version: self.header.extractor_version,
            extractor_version: self.header.extractor_version,
            filename_length: name.len() as u16,
            local_header_offset: self.local_offset as i32,
            modified_date: self.header.modified_date,
            modified_time: self.header.modified_time,
            uncompressed_size: self.header.uncompressed_size,
            compressed_size: self.header.compressed_size,
            compression_method: self.header.compression_method,
            crc32: self.header.crc32,
            ..Default::default()
        };

        self.entry.write(dst);
        *offset += DIR_ENTRY_SIZE as u32;
        dst.extend_from_slice(name.as_bytes());
        *offset += self.entry.filename_length as u32;
    }
}

/// `DirPair`: `<internal name, external>` listings.
#[derive(Debug, Clone, Default)]
struct DirPair {
    first: Vec<String>,
    second: Vec<String>,
}

/// The in-memory zip filesystem (`ZipFileSystem`, root node only).
pub struct ZipFileSystem {
    /// The whole archive (stands in for the reference's `FILE*`).
    data: Vec<u8>,
    /// `fileMap_`: encoded name → `<central dir index, directory index>`.
    file_map: HashMap<String, (u32, u32)>,
    /// `duplicates_`: decoded name → occurrence count.
    duplicates: HashMap<String, u32>,
    /// `dirMap_`: directory path → its listings.
    dir_map: HashMap<String, DirPair>,
    /// `centralDirectory_`.
    central_directory: Vec<LocalFile>,
    /// `path_`: the archive's own resource path.
    path: String,
    /// `tag_`: always empty on the root node.
    tag: String,
    /// `size_`: total archive length.
    size: usize,
}

impl ZipFileSystem {
    /// `openZip`: parse the central directory. `None` on any of the
    /// reference's fatal validation failures.
    pub fn open(path: &str, data: Vec<u8>) -> Option<ZipFileSystem> {
        let mut fs = ZipFileSystem {
            data,
            file_map: HashMap::new(),
            duplicates: HashMap::new(),
            dir_map: HashMap::new(),
            central_directory: Vec::new(),
            path: path.to_string(),
            tag: String::new(),
            size: 0,
        };
        fs.open_zip().then_some(fs)
    }

    fn open_zip(&mut self) -> bool {
        // The root directory node.
        self.dir_map.insert(String::new(), DirPair::default());
        self.size = self.data.len();

        if self.data.len() < DIR_FOOTER_SIZE {
            return false;
        }
        let footer = DirFooter::parse(&self.data[self.size - DIR_FOOTER_SIZE..]);
        if footer.signature != DIR_FOOTER_SIGNATURE {
            return false;
        }

        let dir_offset = footer.dir_offset as usize;
        let dir_size = footer.dir_size as usize;
        if dir_offset + dir_size > self.data.len() {
            return false;
        }
        if dir_size == 0 {
            return true; // just an empty zip
        }
        // Copied out so directory rows can materialise parents mid-loop.
        let dir_block = self.data[dir_offset..dir_offset + dir_size].to_vec();

        self.central_directory
            .resize_with(footer.total_dir_entries as usize, LocalFile::default);
        let mut dir_offset_in_block = 0usize;

        for i in 0..footer.total_dir_entries as usize {
            if dir_offset_in_block + DIR_ENTRY_SIZE > dir_block.len() {
                return false;
            }
            let local_file = &mut self.central_directory[i];
            local_file.entry = DirEntry::parse(&dir_block[dir_offset_in_block..]);
            dir_offset_in_block += DIR_ENTRY_SIZE;
            let entry = local_file.entry;

            if entry.signature != DIR_ENTRY_SIGNATURE
                || entry.filename_length as usize > MAX_FIELD_LENGTH
                || entry.extra_field_length as usize > MAX_FIELD_LENGTH
                || entry.file_comment_length as usize > MAX_FIELD_LENGTH
            {
                return false;
            }

            let name_start = dir_offset_in_block;
            if name_start + entry.filename_length as usize > dir_block.len() {
                return false;
            }
            local_file.filename =
                String::from_utf8_lossy(&dir_block[name_start..name_start + entry.filename_length as usize])
                    .into_owned();
            clean_slashes(&mut local_file.filename);
            dir_offset_in_block += entry.filename_length as usize;
            dir_offset_in_block += entry.extra_field_length as usize;
            dir_offset_in_block += entry.file_comment_length as usize;

            // Folder entries would be flagged by the trailing slash here,
            // but clean_slashes already stripped it: the reference's own
            // check is unreachable and folders materialise below through
            // the directory-component mechanism instead.

            let filename = local_file.filename.clone();

            // Encoded duplicates register under their decoded name.
            if check_duplicate(&filename) {
                let decoded = decode_duplicate(&filename);
                *self.duplicates.entry(decoded.to_owned()).or_insert(0) += 1;
            }

            // The filename -> directory index mapping.
            self.file_map.insert(filename.clone(), (i as u32, 0));

            let (dir_component, file_component) = split_dir(&filename);
            match self.dir_map.get_mut(&dir_component) {
                None => {
                    self.dir_map.insert(dir_component.clone(), DirPair::default());
                    let pair = self.dir_map.get_mut(&dir_component).unwrap();
                    pair.first.push(file_component.clone());
                    pair.second.push(decoded_component(&file_component));
                    self.file_map.get_mut(&filename).unwrap().1 = 0;

                    // Materialise the directory row itself.
                    if !self.file_map.contains_key(&dir_component) {
                        self.update_file(&dir_component, None);
                    }
                }
                Some(pair) => {
                    let index = pair.first.len() as u32;
                    self.file_map.get_mut(&filename).unwrap().1 = index;
                    pair.first.push(file_component.clone());
                    pair.second.push(decoded_component(&file_component));
                }
            }
        }

        true
    }

    /// `resolvePath`: normalise and (when `extra_checks`) strip the
    /// archive's own path prefix.
    fn resolve_path(&self, path: &mut String, extra_checks: bool) {
        clean_slashes(path);
        if self.path.is_empty() {
            return;
        }
        if *path == self.tag {
            path.clear();
            return;
        }
        if extra_checks {
            let dir_path = clean_slashed(&self.path);
            if let Some(off) = path.find(&dir_path) {
                let cut = (off + dir_path.len() + 1).min(path.len());
                *path = path[cut..].to_string();
            }
            if path.len() > 1 && path.ends_with('.') {
                path.truncate(path.len() - 2);
            }
        }
    }

    /// `resolveDuplicate`: map a decoded name onto its first encoded row.
    fn resolve_duplicate(&self, path: &mut String) {
        if self.duplicates.contains_key(path.as_str()) {
            *path = encode_duplicate(path, self.duplicates[path] as i32);
        }
    }

    /// `getFileTypes`: file or not-found, plus DOS-stamped info.
    pub fn get_file_type(&self, path: &str, info: Option<&mut FileInfo>) -> FileType {
        let mut path2 = path.to_string();
        self.resolve_path(&mut path2, true);
        let Some(&(central, _)) = self.file_map.get(&path2) else {
            return FileType::NotFound;
        };
        let file = &self.central_directory[central as usize];
        let ft = if self.dir_map.contains_key(&path2) {
            FileType::Directory
        } else {
            FileType::File
        };
        if let Some(info) = info {
            let alltime = (file.entry.modified_date as u32) << 16 | file.entry.modified_time as u32;
            info.size = file.entry.uncompressed_size;
            info.created = alltime;
            info.modified = alltime;
            info.accessed = alltime;
        }
        ft
    }

    /// `getFileTypeEx`: additionally sniffs nested zip archives, reading
    /// through the compression when needed.
    pub fn get_file_type_ex(&self, path: &str, info: Option<&mut FileInfo>) -> FileType {
        let mut path2 = path.to_string();
        self.resolve_path(&mut path2, true);

        self.resolve_duplicate(&mut path2);
        let Some(&(central, _)) = self.file_map.get(&path2) else {
            return FileType::NotFound;
        };

        let file = &self.central_directory[central as usize];
        // Directories short-circuit before the info is touched.
        if self.dir_map.contains_key(&path2) {
            return FileType::Directory;
        }
        let mut ft = FileType::File;
        if let Some(info) = info {
            let alltime = (file.entry.modified_date as u32) << 16 | file.entry.modified_time as u32;
            info.size = file.entry.uncompressed_size;
            info.created = alltime;
            info.modified = alltime;
            info.accessed = alltime;
        }

        // Already loaded rows sniff their in-memory (compressed) bytes.
        if let Some(data) = &file.data {
            if file.entry.compression_method == METHOD_DEFLATE {
                if let Some(magic) = inflate_prefix(data, 4) {
                    if magic == ZIP_HEADER_MAGIC {
                        ft = FileType::Archive;
                    }
                }
            } else if zip_test(data) {
                ft = FileType::Archive;
            }
        } else {
            // Not loaded: read the head straight out of the archive.
            let header = self.begin_read_local_header(file);
            if file.entry.uncompressed_size == 0 {
                return FileType::File;
            }
            let start = file.entry.local_header_offset as usize
                + LOCAL_HEADER_SIZE
                + header.filename_length as usize
                + header.extra_field_length as usize;
            let end = start + file.entry.compressed_size as usize;
            if end > self.data.len() {
                return FileType::File;
            }
            let stored = &self.data[start..end];
            let magic = if file.entry.compression_method == METHOD_DEFLATE {
                inflate_prefix(stored, 4)
            } else if stored.len() >= 4 {
                Some(u32_le(stored, 0))
            } else {
                None
            };
            if magic == Some(ZIP_HEADER_MAGIC) {
                ft = FileType::Archive;
            }
        }
        ft
    }

    /// `readFile(path)`: fully read and (for deflate rows) decompress.
    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let mut path2 = path.to_string();
        self.resolve_path(&mut path2, false);
        self.read_file_internal(&path2, true)
    }

    /// `readFile(dirPath, index)`.
    pub fn read_file_at(&self, dir_path: &str, index: usize) -> Option<Vec<u8>> {
        let mut path2 = dir_path.to_string();
        self.resolve_path(&mut path2, false);
        let pair = self.dir_map.get(&path2)?;
        if index >= pair.first.len() {
            return None;
        }
        if path2.is_empty() {
            self.read_file_internal(&pair.first[index], true)
        } else {
            self.read_file_internal(&format!("{}/{}", path2, pair.first[index]), true)
        }
    }

    /// `beginReadLocalFile`: load the row's 30-byte local header. A bad
    /// signature yields a zeroed header — the reference ignores the failure
    /// and its `dataOffset()` then reads from the shifted position.
    fn begin_read_local_header(&self, file: &LocalFile) -> LocalHeader {
        let start = file.entry.local_header_offset as usize;
        if start + LOCAL_HEADER_SIZE > self.data.len() {
            return LocalHeader::default();
        }
        let header = LocalHeader::parse(&self.data[start..]);
        if header.signature != LOCAL_HEADER_SIGNATURE {
            return LocalHeader::default();
        }
        header
    }

    /// `readFileInternal`.
    fn read_file_internal(&self, path: &str, uncompress_data: bool) -> Option<Vec<u8>> {
        let mut path = path.to_string();
        self.resolve_duplicate(&mut path);
        let &(central, _) = self.file_map.get(&path)?;
        let file = &self.central_directory[central as usize];

        // Structure-only row (a directory placeholder).
        if file.entry.local_header_offset < 0 {
            return None;
        }

        // Rows created through updateFile hold their (compressed) bytes.
        if let Some(data) = &file.data {
            return Some(data.clone());
        }

        let header = self.begin_read_local_header(file);
        let start = file.entry.local_header_offset as usize
            + LOCAL_HEADER_SIZE
            + header.filename_length as usize
            + header.extra_field_length as usize;

        if file.entry.compression_method != METHOD_STORE
            && file.entry.compression_method != METHOD_DEFLATE
        {
            return None;
        }

        let end = start + file.entry.compressed_size as usize;
        if end > self.data.len() {
            return None;
        }
        let stored = &self.data[start..end];

        if uncompress_data
            && file.entry.compressed_size > 0
            && file.entry.compression_method == METHOD_DEFLATE
        {
            inflate_exact(stored, file.entry.uncompressed_size as usize)
        } else {
            Some(stored.to_vec())
        }
    }

    /// `readDirectory`: the decoded names of a directory's rows.
    pub fn read_directory(&self, path: &str) -> Option<Vec<String>> {
        let mut path2 = path.to_string();
        self.resolve_path(&mut path2, false);
        self.dir_map.get(&path2).map(|pair| pair.second.clone())
    }

    /// `fileCount`.
    pub fn file_count(&self, dir_path: &str) -> usize {
        let mut path2 = dir_path.to_string();
        self.resolve_path(&mut path2, false);
        self.dir_map
            .get(&path2)
            .map_or(0, |pair| pair.first.len())
    }

    /// `fileName`: the decoded name at `index` within `dir_path`.
    pub fn file_name(&self, dir_path: &str, index: usize) -> Option<String> {
        let mut path2 = dir_path.to_string();
        self.resolve_path(&mut path2, false);
        self.dir_map
            .get(&path2)
            .and_then(|pair| pair.second.get(index))
            .cloned()
    }

    /// `fileIndex`: the directory index of a file, if present.
    pub fn file_index(&self, name: &str) -> Option<usize> {
        let mut path = name.to_string();
        self.resolve_duplicate(&mut path);
        self.file_map.get(&path).map(|&(_, dir)| dir as usize)
    }

    /// `dirTest`: is the name a known directory?
    pub fn dir_test(&self, name: &str) -> bool {
        self.dir_map.contains_key(name)
    }

    /// `fileSize`: loaded bytes win over the recorded size.
    pub fn file_size(&self, dir_path: &str, index: usize) -> u32 {
        let mut path2 = dir_path.to_string();
        self.resolve_path(&mut path2, false);
        let Some(pair) = self.dir_map.get(&path2) else {
            return 0;
        };
        if index >= pair.first.len() {
            return 0;
        }
        let full_name = if path2.is_empty() {
            pair.first[index].clone()
        } else {
            format!("{}/{}", path2, pair.first[index])
        };
        let Some(&(central, _)) = self.file_map.get(&full_name) else {
            // Defensive only: erase/clear keep listings and the file map in
            // step, so no public flow reaches this — same as the reference.
            return 0;
        };
        let file = &self.central_directory[central as usize];
        match &file.data {
            Some(data) => data.len() as u32,
            None => file.entry.uncompressed_size,
        }
    }

    /// `writeToStructure`: register bytes without persisting.
    pub fn write_to_structure(&mut self, path: &str, data: Vec<u8>) -> bool {
        let mut filename = path.to_string();
        self.resolve_path(&mut filename, false);
        self.update_file(&filename, Some(data));
        true
    }

    /// `updateFile`: insert or duplicate-encode a row, refreshing the
    /// directory listings (and materialising parent directories).
    pub fn update_file(&mut self, name: &str, data: Option<Vec<u8>>) {
        let mut real_name = name.to_string();
        clean_slashes(&mut real_name);

        match self.file_map.get(&name.to_string()).copied() {
            None => {
                // A decoded duplicate gets an encoded row name.
                if self.duplicates.contains_key(name) {
                    let count = self.duplicates.entry(name.to_string()).or_insert(0);
                    *count += 1;
                    real_name = encode_duplicate(&real_name, *count as i32);
                }
                let file = match data {
                    Some(bytes) => LocalFile::new(&real_name, bytes),
                    None => {
                        let mut f = LocalFile {
                            filename: real_name.clone(),
                            ..Default::default()
                        };
                        f.entry.local_header_offset = -1;
                        f
                    }
                };
                self.central_directory.push(file);
                let central = (self.central_directory.len() - 1) as u32;
                self.file_map.insert(real_name.clone(), (central, 0));
            }
            Some((central_index, dir_index)) => {
                // Register as a duplicate: rename the old row, add a new
                // one holding the fresh bytes.
                real_name = encode_duplicate(name, 1);
                self.duplicates.insert(name.to_string(), 1);
                self.central_directory[central_index as usize].rename(&real_name);

                let (dir, file_name) = split_dir(&real_name);
                let pair = self.dir_map.get_mut(&dir).expect("existing row has a dir");
                pair.first[dir_index as usize] = file_name;

                self.file_map.remove(name);
                self.file_map.insert(real_name.clone(), (central_index, 0));

                real_name = encode_duplicate(name, 2);
                let file = match data {
                    Some(bytes) => LocalFile::new(&real_name, bytes),
                    None => {
                        let mut f = LocalFile {
                            filename: real_name.clone(),
                            ..Default::default()
                        };
                        f.entry.local_header_offset = -1;
                        f
                    }
                };
                self.central_directory.push(file);
                let central = (self.central_directory.len() - 1) as u32;
                self.file_map.insert(real_name.clone(), (central, 0));
                *self.duplicates.entry(name.to_string()).or_insert(0) += 1;
            }
        }

        // Refresh the directory listings.
        let (dir_component, file_component) = split_dir(&real_name);
        match self.dir_map.get_mut(&dir_component) {
            None => {
                self.dir_map.insert(dir_component.clone(), DirPair::default());
                let pair = self.dir_map.get_mut(&dir_component).unwrap();
                pair.first.push(file_component.clone());
                pair.second.push(decoded_component(&file_component));
                self.file_map.get_mut(&real_name).unwrap().1 = 0;

                if !self.file_map.contains_key(&dir_component) && !dir_component.is_empty() {
                    self.update_file(&dir_component, None);
                }
            }
            Some(pair) => {
                let index = pair.first.len() as u32;
                self.file_map.get_mut(&real_name).unwrap().1 = index;
                pair.first.push(file_component.clone());
                pair.second.push(decoded_component(&file_component));
            }
        }
    }

    /// `eraseFileOrDirectory`.
    pub fn erase_file_or_directory(&mut self, file_path: &str) -> bool {
        let mut filename = file_path.to_string();
        self.resolve_path(&mut filename, false);

        if let Some(count) = self.duplicates.get(&filename).copied() {
            // The decoded name stays registered with a reduced count.
            if count > 1 {
                self.duplicates.insert(filename.clone(), count - 1);
            } else {
                self.duplicates.remove(&filename);
            }
            filename = encode_duplicate(&filename, count as i32);
        }

        let (dir_component, _file_component) = split_dir(&filename);
        let Some((file_index, dir_index)) = self.file_map.get(&filename).copied() else {
            return false;
        };
        {
            let pair = self
                .dir_map
                .get_mut(&dir_component)
                .expect("file row implies its directory");
            pair.first.remove(dir_index as usize);
            pair.second.remove(dir_index as usize);
        }

        self.file_map.remove(&filename);
        self.central_directory[file_index as usize].clear();

        // Re-index the rows that shifted within this directory.
        let pair = self.dir_map.get(&dir_component).unwrap().clone();
        for (pos, name) in pair.first.iter().enumerate() {
            let full_name = if dir_component.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", dir_component, name)
            };
            if let Some(entry) = self.file_map.get_mut(&full_name) {
                entry.1 = pos as u32;
            }
        }
        true
    }

    /// `clear(dirPath)`: drop every row under a directory.
    pub fn clear_dir(&mut self, dir_path: &str) {
        let mut path2 = dir_path.to_string();
        self.resolve_path(&mut path2, false);
        let Some(pair) = self.dir_map.get(&path2).cloned() else {
            return;
        };
        for name in &pair.first {
            let filename = if path2.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", path2, name)
            };
            if let Some(&(file_index, _)) = self.file_map.get(&filename) {
                self.file_map.remove(&filename);
                self.central_directory[file_index as usize].clear();
            }
            // Encoded rows retire their decoded entry too.
            if check_duplicate(&filename) {
                let decoded = decode_duplicate(&filename);
                self.duplicates.remove(decoded);
            }
        }
        if let Some(pair) = self.dir_map.get_mut(&path2) {
            pair.first.clear();
            pair.second.clear();
        }
    }

    /// `createZip`: serialise every valid row into a fresh archive.
    pub fn create_zip(&mut self) -> Vec<u8> {
        // Drop invalid/uninitialised rows.
        self.central_directory.retain(|file| file.is_valid());

        let total: u32 = self
            .central_directory
            .iter_mut()
            .map(|file| file.size_on_disk())
            .sum::<u32>()
            + DIR_FOOTER_SIZE as u32;

        let mut out = Vec::with_capacity(total as usize);
        let mut offset = 0u32;
        for file in &mut self.central_directory {
            file.write_file(&mut out, &mut offset);
        }

        // Central directory, then the footer.
        let dir_offset = offset;
        for file in &mut self.central_directory {
            file.write_dir_entry(&mut out, &mut offset);
        }
        let count = self.central_directory.len() as u16;
        let footer = DirFooter {
            signature: DIR_FOOTER_SIGNATURE,
            dir_offset,
            dir_entries: count,
            total_dir_entries: count,
            dir_size: offset - dir_offset,
            comment_length: 0,
            ..Default::default()
        };
        footer.write(&mut out);
        debug_assert_eq!(out.len(), total as usize);
        out
    }
}

/// `ZipFileSystem::zipTest`: more than four bytes starting with the local
/// header signature.
pub fn zip_test(data: &[u8]) -> bool {
    data.len() > 4 && u32_le(data, 0) == ZIP_HEADER_MAGIC
}

/// `cleanSlashes`: forward slashes only, no duplicates, no edge slashes.
pub fn clean_slashes(filename: &mut String) {
    let mut result = std::mem::take(filename);
    while let Some(pos) = result.find('\\') {
        result.replace_range(pos..pos + 1, "/");
    }
    while let Some(pos) = result.find("//") {
        result.replace_range(pos..pos + 2, "/");
    }
    if result.starts_with('/') {
        result.remove(0);
    }
    if result.ends_with('/') {
        result.pop();
    }
    *filename = result;
}

fn clean_slashed(path: &str) -> String {
    let mut s = path.to_string();
    clean_slashes(&mut s);
    s
}

/// `checkDuplicate`: the space is the escape marker.
fn check_duplicate(filename: &str) -> bool {
    filename.contains(' ')
}

/// `decodeDuplicate`: everything before the first space.
fn decode_duplicate(filename: &str) -> &str {
    match filename.find(' ') {
        Some(pos) => &filename[..pos],
        None => filename,
    }
}

/// Directory listings show the decoded form of encoded components.
fn decoded_component(component: &str) -> String {
    if check_duplicate(component) {
        decode_duplicate(component).to_string()
    } else {
        component.to_string()
    }
}

/// `encodeDuplicate`: append the occurrence count.
fn encode_duplicate(filename: &str, count: i32) -> String {
    format!("{} {}", filename, count)
}

/// Split at the last slash: `(dir component, file component)`.
fn split_dir(path: &str) -> (String, String) {
    match path.rfind('/') {
        None => (String::new(), path.to_string()),
        Some(pos) => (path[..pos].to_string(), path[pos + 1..].to_string()),
    }
}

/// `LocalFile::compressData`: raw deflate (no zlib header); the reference
/// gives up — and stores instead — when the output does not fit in
/// `len + 1`.
fn compress_data(data: &[u8]) -> Option<Vec<u8>> {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, data).ok()?;
    encoder.finish().ok().and_then(|out| {
        if out.len() <= data.len() + 1 {
            Some(out)
        } else {
            None
        }
    })
}

/// Raw inflate of exactly `expected` bytes, requiring the stream to end
/// there (the reference's `inflate(Z_FINISH) == Z_STREAM_END`).
fn inflate_exact(data: &[u8], expected: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut decoder = flate2::read::DeflateDecoder::new(data);
    let mut out = vec![0u8; expected];
    decoder.read_exact(&mut out).ok()?;
    // The stream must terminate exactly at the expected length.
    decoder.read(&mut [0u8; 1]).ok()?;
    Some(out)
}

/// Decompress just the first `n` bytes (`getFileTypeEx`'s streaming sniff).
fn inflate_prefix(data: &[u8], n: usize) -> Option<u32> {
    use std::io::Read;
    let mut decoder = flate2::read::DeflateDecoder::new(data);
    let mut out = [0u8; 4];
    let read = decoder.read(&mut out[..n.min(4)]).ok()?;
    if read < n.min(4) {
        return None;
    }
    Some(u32_le(&out, 0))
}

/// DOS packed timestamp from a unix time (the reference uses `localtime`;
/// UTC keeps this deterministic).
fn dos_timestamp(unix: i64) -> (u16, u16) {
    let days = unix.div_euclid(86400);
    let secs = unix.rem_euclid(86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    let year = if y <= 1980 { 0 } else { y - 1980 };
    let date = (d as u16) + ((m as u16) << 5) + ((year as u16) << 9);
    let (hour, min, sec) = (
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
    );
    let time = ((sec >> 1) as u16) + ((min as u16) << 5) + ((hour as u16) << 11);
    (date, time)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn u16_le(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_le(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn i32_le(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    impl ZipFileSystem {
        /// `ZipFileSystem::updateFile` with a fixed DOS stamp, for
        /// byte-exact expectations.
        fn update_file_with_stamp(&mut self, name: &str, data: &[u8], date: u16, time: u16) {
            let mut file = LocalFile {
                filename: name.to_string(),
                ..Default::default()
            };
            file.entry.local_header_offset = -1;
            file.update_header_with_time(data.to_vec(), date, time);
            self.central_directory.push(file);
            let central = (self.central_directory.len() - 1) as u32;
            self.file_map.insert(name.to_string(), (central, 0));

            // Directory listings, mirroring updateFile's tail.
            let (dir_component, file_component) = split_dir(name);
            match self.dir_map.get_mut(&dir_component) {
                None => {
                    self.dir_map
                        .insert(dir_component.clone(), DirPair::default());
                    let pair = self.dir_map.get_mut(&dir_component).unwrap();
                    pair.first.push(file_component.clone());
                    pair.second.push(decoded_component(&file_component));
                    self.file_map.get_mut(name).unwrap().1 = 0;
                    if !self.file_map.contains_key(&dir_component) && !dir_component.is_empty() {
                        self.update_file(&dir_component, None);
                    }
                }
                Some(pair) => {
                    let index = pair.first.len() as u32;
                    self.file_map.get_mut(name).unwrap().1 = index;
                    pair.first.push(file_component.clone());
                    pair.second.push(decoded_component(&file_component));
                }
            }
        }
    }

    /// A fixture row for `build_raw_zip`, assembled by hand so the parser is
    /// validated against bytes that never went through the writer.
    struct RawEntry<'a> {
        name: &'a str,
        data: &'a [u8],
        /// Raw bytes to store for this row: set this to a deflate stream to
        /// model a compressed row; sizes stay `data`-driven like a real
        /// writer's.
        stored: Option<Vec<u8>>,
        method: u16,
        date: u16,
        time: u16,
    }

    impl<'a> RawEntry<'a> {
        fn stored(name: &'a str, data: &'a [u8]) -> RawEntry<'a> {
            RawEntry {
                name,
                data,
                stored: None,
                method: METHOD_STORE,
                date: 0,
                time: 0,
            }
        }

        fn deflated(name: &'a str, data: &'a [u8]) -> RawEntry<'a> {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(data).unwrap();
            RawEntry {
                name,
                data,
                stored: Some(encoder.finish().unwrap()),
                method: METHOD_DEFLATE,
                date: 0,
                time: 0,
            }
        }

        fn at(&mut self, date: u16, time: u16) -> &mut RawEntry<'a> {
            self.date = date;
            self.time = time;
            self
        }
    }

    /// Hand-assemble a zip archive (local headers + data, central directory,
    /// footer) without touching the writer under test.
    fn build_raw_zip(entries: &[RawEntry<'_>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut offsets = Vec::new();
        for e in entries {
            offsets.push(out.len() as u32);
            let payload = e.stored.clone().unwrap_or_else(|| e.data.to_vec());
            out.extend_from_slice(&LOCAL_HEADER_SIGNATURE.to_le_bytes());
            out.extend_from_slice(&10u16.to_le_bytes()); // extractor version
            out.extend_from_slice(&0u16.to_le_bytes()); // mask
            out.extend_from_slice(&e.method.to_le_bytes());
            out.extend_from_slice(&e.time.to_le_bytes());
            out.extend_from_slice(&e.date.to_le_bytes());
            out.extend_from_slice(&crc32fast::hash(e.data).to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra field
            out.extend_from_slice(e.name.as_bytes());
            out.extend_from_slice(&payload);
        }

        let dir_offset = out.len() as u32;
        let mut dir_size = 0usize;
        for (e, off) in entries.iter().zip(&offsets) {
            let payload = e.stored.clone().unwrap_or_else(|| e.data.to_vec());
            out.extend_from_slice(&DIR_ENTRY_SIGNATURE.to_le_bytes());
            out.extend_from_slice(&10u16.to_le_bytes()); // creator version
            out.extend_from_slice(&10u16.to_le_bytes()); // extractor version
            out.extend_from_slice(&0u16.to_le_bytes()); // mask
            out.extend_from_slice(&e.method.to_le_bytes());
            out.extend_from_slice(&e.time.to_le_bytes());
            out.extend_from_slice(&e.date.to_le_bytes());
            out.extend_from_slice(&crc32fast::hash(e.data).to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra field
            out.extend_from_slice(&0u16.to_le_bytes()); // comment length
            out.extend_from_slice(&0u16.to_le_bytes()); // disk number
            out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            out.extend_from_slice(&off.to_le_bytes());
            out.extend_from_slice(e.name.as_bytes());
            dir_size += DIR_ENTRY_SIZE + e.name.len();
        }

        out.extend_from_slice(&DIR_FOOTER_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number
        out.extend_from_slice(&0u16.to_le_bytes()); // dir start disk
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir_size as u32).to_le_bytes());
        out.extend_from_slice(&dir_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        out
    }

    /// An empty archive: just the footer.
    fn empty_zip() -> Vec<u8> {
        build_raw_zip(&[])
    }

    /// xorshift PRNG: bytes deflate cannot shrink, forcing METHOD_STORE.
    fn noisy(len: usize) -> Vec<u8> {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    fn open_fixture(entries: &[RawEntry<'_>]) -> ZipFileSystem {
        ZipFileSystem::open("", build_raw_zip(entries)).unwrap()
    }

    #[test]
    fn dos_timestamp_packs_calendared_fields() {
        // 2000-03-01 00:00:00 UTC is unix day 11017.
        let (date, time) = dos_timestamp(11_017 * 86_400);
        assert_eq!(date, 1 + (3 << 5) + (20 << 9), "day 1, month 3, year 20");
        assert_eq!(time, 0);

        // 12:34:56 packs as sec>>1 | min<<5 | hour<<11.
        let (_, time) = dos_timestamp(11_017 * 86_400 + 12 * 3600 + 34 * 60 + 56);
        assert_eq!(time, (56 >> 1) + (34 << 5) + (12 << 11));

        // Before 1980 only the year field clamps to zero; day/month stay.
        assert_eq!(dos_timestamp(0), (1 + (1 << 5), 0));
        // 1980-01-01 is unix day 3652.
        assert_eq!(dos_timestamp(3_652 * 86_400).0 & 0x1f, 1);
        assert_eq!((dos_timestamp(3_652 * 86_400).0 >> 9) & 0x7f, 0);
    }

    #[test]
    fn zip_test_requires_signature_beyond_four_bytes() {
        assert!(!zip_test(&[]));
        assert!(!zip_test(&[0x50, 0x4b, 0x03, 0x04]), "exactly four bytes");
        assert!(zip_test(&[0x50, 0x4b, 0x03, 0x04, 0x00]));
        assert!(!zip_test(&noisy(8)));
    }

    #[test]
    fn clean_slashes_normalises_separators() {
        let cases = [
            ("\\dir\\file.txt", "dir/file.txt"),
            ("a//b///c", "a/b/c"),
            ("/leading/", "leading"),
            ("/", ""),
            ("plain.txt", "plain.txt"),
            ("", ""),
        ];
        for (raw, want) in cases {
            let mut s = raw.to_string();
            clean_slashes(&mut s);
            assert_eq!(s, want, "input {raw:?}");
        }
    }

    #[test]
    fn resolve_path_strips_the_archive_prefix() {
        // An archive mounted under a path strips that prefix from queries.
        let fs = ZipFileSystem::open(
            "packs/base.zip",
            build_raw_zip(&[
                RawEntry::stored("dir/a.txt", b"1"),
                RawEntry::stored("top.txt", b"2"),
            ]),
        )
        .unwrap();

        // Type queries strip the prefix; the bare name works too.
        assert_eq!(
            fs.get_file_type("packs/base.zip/dir/a.txt", None),
            FileType::File
        );
        assert_eq!(fs.get_file_type("dir/a.txt", None), FileType::File);
        // The tag itself ("") clears to the archive root.
        assert_eq!(fs.get_file_type("", None), FileType::NotFound);
        assert_eq!(fs.file_count("dir"), 1);
        // readFile skips the extra checks: the prefixed name misses.
        assert_eq!(fs.read_file("packs/base.zip/dir/a.txt"), None);
        assert_eq!(fs.read_file("dir/a.txt").unwrap(), b"1");
        // The trailing-dot quirk erases the dot AND the preceding char
        // (erase(size-2, size)), so the query misses entirely.
        assert_eq!(
            fs.get_file_type("packs/base.zip/dir/a.txt.", None),
            FileType::NotFound,
            "'dir/a.txt.' is cut to 'dir/a.tx'"
        );
    }

    #[test]
    fn duplicate_name_helpers_are_total() {
        assert_eq!(decode_duplicate("no-space"), "no-space");
        assert_eq!(decode_duplicate("cfg.txt 12"), "cfg.txt");
        assert!(check_duplicate("cfg.txt 12"));
        assert!(!check_duplicate("cfg.txt"));
        assert_eq!(encode_duplicate("cfg.txt", 3), "cfg.txt 3");
    }

    #[test]
    fn hand_built_store_archive_parses_and_reads() {
        let payload = noisy(64);
        let fs = open_fixture(&[
            RawEntry::stored("root.txt", b"root data"),
            {
                let mut e = RawEntry::stored("dir/nested.bin", &payload);
                e.at(0x5821, 0x6c38); // DOS stamp to read back
                e
            },
            RawEntry::stored("dir/sub/deep.txt", b"deep"),
        ]);

        // Store rows hand their bytes back verbatim, slashes cleaned.
        assert_eq!(fs.read_file("root.txt").unwrap(), b"root data");
        assert_eq!(fs.read_file("dir/nested.bin").unwrap(), payload);
        assert_eq!(fs.read_file("/dir//sub/deep.txt/").unwrap(), b"deep");
        assert_eq!(fs.read_file_at("", 0).unwrap(), b"root data");
        assert_eq!(fs.read_file_at("dir", 0).unwrap(), payload);

        // Unknown rows and structure-only directories read as nothing.
        assert!(fs.read_file("nope.txt").is_none());
        assert!(fs.read_file("dir").is_none(), "placeholder row");
        assert!(fs.read_file_at("dir", 99).is_none());

        // Timestamps surface through get_file_type.
        let mut info = FileInfo::default();
        assert_eq!(fs.get_file_type("dir/nested.bin", Some(&mut info)), FileType::File);
        assert_eq!(info.size, 64);
        let stamp = (0x5821u32) << 16 | 0x6c38;
        assert_eq!(info.created, stamp);
        assert_eq!(info.modified, stamp);
        assert_eq!(info.accessed, stamp);
    }

    #[test]
    fn deflate_rows_inflate_back() {
        let payload = b"compressible text repeated repeated repeated".to_vec();
        let fs = open_fixture(&[RawEntry::deflated("log.txt", &payload)]);
        assert_eq!(fs.read_file("log.txt").unwrap(), payload);
        assert_eq!(fs.get_file_type("log.txt", None), FileType::File);
    }

    #[test]
    fn empty_archive_opens_with_a_root_directory() {
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        assert_eq!(fs.file_count(""), 0);
        // The root itself has no file row, so it types as not-found.
        assert_eq!(fs.get_file_type("", None), FileType::NotFound);
        assert_eq!(fs.get_file_type("x", None), FileType::NotFound);
        assert_eq!(fs.create_zip(), empty_zip());
    }

    #[test]
    fn open_rejects_broken_archives() {
        // No footer at all.
        assert!(ZipFileSystem::open("", noisy(10)).is_none());
        // Footer signature wrong.
        let mut bad = empty_zip();
        bad[0] = 0x00;
        assert!(ZipFileSystem::open("", bad).is_none());
        // Directory block runs past the file.
        let mut bad = empty_zip();
        bad[16..20].copy_from_slice(&0xffff_ffffu32.to_le_bytes()); // dirOffset
        assert!(ZipFileSystem::open("", bad).is_none());
        // Directory entry signature wrong.
        let mut bad = build_raw_zip(&[RawEntry::stored("a.txt", b"x")]);
        let dir_offset = u32_le(&bad, bad.len() - DIR_FOOTER_SIZE + 16) as usize;
        bad[dir_offset] = 0x00;
        assert!(ZipFileSystem::open("", bad).is_none());
        // Footer claims more entries than the block holds.
        let mut bad = build_raw_zip(&[RawEntry::stored("a.txt", b"x")]);
        let tail = bad.len() - DIR_FOOTER_SIZE;
        bad[tail + 10..tail + 12].copy_from_slice(&3u16.to_le_bytes());
        assert!(ZipFileSystem::open("", bad).is_none());
        // Footer truncates the directory block mid-name.
        let mut bad = build_raw_zip(&[RawEntry::stored("a.txt", b"x")]);
        let tail = bad.len() - DIR_FOOTER_SIZE;
        bad[tail + 12..tail + 16].copy_from_slice(&(DIR_ENTRY_SIZE as u32 + 2).to_le_bytes());
        assert!(ZipFileSystem::open("", bad).is_none());
    }

    #[test]
    fn unsupported_compression_methods_fail_reads() {
        let mut e = RawEntry::stored("mystery.bin", b"payload");
        e.method = 99;
        let fs = open_fixture(&[e]);
        assert!(fs.read_file("mystery.bin").is_none());
        assert_eq!(fs.get_file_type_ex("mystery.bin", None), FileType::File);
    }

    #[test]
    fn corrupt_local_headers_degrade_like_the_reference() {
        // Stomped local-header signature: beginReadLocalFile leaves a zeroed
        // header, so the read starts at offset+30 — straight into the name
        // field — and hands back those bytes.
        let mut bad = build_raw_zip(&[RawEntry::stored("a.txt", b"xyz")]);
        bad[0] = 0x00;
        let fs = ZipFileSystem::open("", bad).unwrap();
        assert_eq!(fs.read_file("a.txt").unwrap(), b"a.t");

        // compressed_size overrunning the archive fails the read.
        let mut bad = build_raw_zip(&[RawEntry::stored("a.txt", b"xyz")]);
        let dir_offset = u32_le(&bad, bad.len() - DIR_FOOTER_SIZE + 16) as usize;
        bad[dir_offset + 18..dir_offset + 22]
            .copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        let fs = ZipFileSystem::open("", bad).unwrap();
        assert!(fs.read_file("a.txt").is_none());
    }

    #[test]
    fn file_types_cover_all_four_states() {
        let inner = build_raw_zip(&[RawEntry::stored("inner.txt", b"i")]);
        let fs = open_fixture(&[
            RawEntry::stored("dir/child.txt", b"c"),
            RawEntry::stored("plain.txt", b"p"),
            RawEntry::stored("bundle.zip", &inner),
            RawEntry::deflated("packed.zip", &inner),
        ]);

        assert_eq!(fs.get_file_type("plain.txt", None), FileType::File);
        assert_eq!(fs.get_file_type("dir", None), FileType::Directory);
        // The root directory has a listing but no file row: type queries on
        // "" report not-found, exactly like the reference.
        assert_eq!(fs.get_file_type("", None), FileType::NotFound);
        assert_eq!(fs.get_file_type("missing", None), FileType::NotFound);
        assert!(fs.dir_test("dir"));
        assert!(!fs.dir_test("plain.txt"));
        assert_eq!(fs.file_index("dir/child.txt"), Some(0));
        assert_eq!(fs.file_index("missing"), None);

        // get_file_type never sniffs archives; get_file_type_ex does, for
        // both stored and deflated rows (all still unloaded here).
        assert_eq!(fs.get_file_type("bundle.zip", None), FileType::File);
        assert_eq!(fs.get_file_type_ex("bundle.zip", None), FileType::Archive);
        assert_eq!(fs.get_file_type_ex("packed.zip", None), FileType::Archive);
        assert_eq!(fs.get_file_type_ex("plain.txt", None), FileType::File);
        assert_eq!(fs.get_file_type_ex("missing", None), FileType::NotFound);
    }

    #[test]
    fn loaded_rows_are_sniffed_from_their_in_memory_bytes() {
        let inner = build_raw_zip(&[RawEntry::stored("inner.txt", b"i")]);
        let mut fs = open_fixture(&[RawEntry::stored("keeper.txt", b"k")]);

        // Written rows hold their bytes in memory.
        fs.write_to_structure("bundle.zip", inner.clone());
        assert_eq!(fs.get_file_type_ex("bundle.zip", None), FileType::Archive);
        fs.write_to_structure("packed.zip", noisy(32)); // not a zip
        assert_eq!(fs.get_file_type_ex("packed.zip", None), FileType::File);
        assert_eq!(fs.get_file_type_ex("keeper.txt", None), FileType::File);

        // A loaded deflated row is sniffed through the inflate: white-box
        // fixture for a row the public API cannot produce (updateFile stores
        // anything zip-magic-headed verbatim).
        let ghost_data = {
            let mut encoder = flate2::write::DeflateEncoder::new(
                Vec::new(),
                flate2::Compression::default(),
            );
            encoder.write_all(&inner).unwrap();
            encoder.finish().unwrap()
        };
        fs.central_directory.push(LocalFile {
            filename: "ghost.zip".to_string(),
            data: Some(ghost_data),
            entry: DirEntry {
                signature: DIR_ENTRY_SIGNATURE,
                compression_method: METHOD_DEFLATE,
                ..Default::default()
            },
            ..Default::default()
        });
        let ghost_index = fs.central_directory.len() - 1;
        fs.file_map
            .insert("ghost.zip".to_string(), (ghost_index as u32, 0));
        assert_eq!(fs.get_file_type_ex("ghost.zip", None), FileType::Archive);
    }

    #[test]
    fn file_type_ex_edge_paths_match_the_reference() {
        let fs = open_fixture(&[
            RawEntry::stored("dir/child.txt", b"c"),
            RawEntry::stored("empty.bin", b""),
            RawEntry::stored("plain.txt", b"p"),
        ]);

        // Directories short-circuit before any sniffing or info fill.
        assert_eq!(fs.get_file_type_ex("dir", None), FileType::Directory);
        let mut info = FileInfo::default();
        assert_eq!(fs.get_file_type_ex("plain.txt", Some(&mut info)), FileType::File);
        assert_eq!(info.size, 1);
        let stamp = 0u32;
        assert_eq!(info.modified, stamp);

        // Zero-size and overrunning rows report as plain files.
        assert_eq!(fs.get_file_type_ex("empty.bin", None), FileType::File);
        let mut bad = build_raw_zip(&[RawEntry::stored("a.txt", b"xyz")]);
        let dir_offset = u32_le(&bad, bad.len() - DIR_FOOTER_SIZE + 16) as usize;
        bad[dir_offset + 20..dir_offset + 24]
            .copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        let fs = ZipFileSystem::open("", bad).unwrap();
        assert_eq!(fs.get_file_type_ex("a.txt", None), FileType::File);

        // A corrupted local-header offset degrades reads: the header load
        // falls back to zeros and the read then overruns.
        let mut bad = build_raw_zip(&[RawEntry::stored("a.txt", b"xyz")]);
        let dir_offset = u32_le(&bad, bad.len() - DIR_FOOTER_SIZE + 16) as usize;
        bad[dir_offset + 42..dir_offset + 46]
            .copy_from_slice(&0x7fff_fff0i32.to_le_bytes());
        let fs = ZipFileSystem::open("", bad).unwrap();
        assert!(fs.read_file("a.txt").is_none());
    }

    #[test]
    fn loaded_deflate_sniff_tolerates_short_streams() {
        // A loaded deflated row whose stream yields fewer than four bytes
        // sniffs as a plain file.
        let mut fs = open_fixture(&[RawEntry::stored("keeper.txt", b"k")]);
        let tiny = {
            let mut encoder = flate2::write::DeflateEncoder::new(
                Vec::new(),
                flate2::Compression::default(),
            );
            encoder.write_all(b"a").unwrap();
            encoder.finish().unwrap()
        };
        fs.central_directory.push(LocalFile {
            filename: "tiny.bin".to_string(),
            data: Some(tiny),
            entry: DirEntry {
                signature: DIR_ENTRY_SIGNATURE,
                compression_method: METHOD_DEFLATE,
                ..Default::default()
            },
            ..Default::default()
        });
        let index = fs.central_directory.len() - 1;
        fs.file_map.insert("tiny.bin".to_string(), (index as u32, 0));
        assert_eq!(fs.get_file_type_ex("tiny.bin", None), FileType::File);
    }

    #[test]
    fn local_file_update_replaces_content() {
        // LocalFile::update(filename, data) rebuilds the header around the
        // new bytes, re-latching the compression state.
        let mut file = LocalFile::with_stamp("old.txt", b"first".to_vec(), 0x5821, 0x6c38);
        let old_crc = file.header.crc32;
        file.update("new.txt", b"second!".to_vec());
        assert_eq!(file.filename, "new.txt");
        assert_eq!(file.header.uncompressed_size, 7);
        assert_eq!(file.header.crc32, crc32fast::hash(b"second!"));
        assert_ne!(file.header.crc32, old_crc);
        // The rebuilt row serialises and reads back under the new name.
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        fs.central_directory.push(file);
        fs.file_map.insert("new.txt".to_string(), (0, 0));
        fs.dir_map
            .get_mut(&String::new())
            .unwrap()
            .first
            .push("new.txt".to_string());
        let fs = ZipFileSystem::open("", fs.create_zip()).unwrap();
        assert_eq!(fs.read_file("new.txt").unwrap(), b"second!");
    }

    #[test]
    fn written_rows_read_directly_after_create_zip() {
        // createZip fills the entry offsets, and reads then hand back the
        // in-memory bytes without touching the archive buffer.
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        fs.update_file("kept.txt", Some(b"data".to_vec()));
        assert_eq!(fs.read_file("kept.txt"), None, "no offsets yet");
        fs.create_zip();
        assert_eq!(fs.read_file("kept.txt").unwrap(), b"data");
        // Loaded bytes win over the recorded entry size.
        assert_eq!(fs.file_size("", 0), 4);
        let (central, _) = fs.file_map["kept.txt"];
        assert_eq!(fs.central_directory[central as usize].uncompressed_size(), 4);
        assert_eq!(
            fs.central_directory[central as usize].compressed_size(),
            4,
            "tiny text deflates no smaller than len+1, so it stores"
        );
    }

    #[test]
    fn rewriting_with_no_data_leaves_a_placeholder() {
        // updateFile(name, NULL) on an existing row renames the old row and
        // registers a structure-only successor.
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        fs.update_file("a.txt", Some(b"1".to_vec()));
        fs.update_file("a.txt", None);
        assert_eq!(fs.file_count(""), 2);
        let bytes = fs.create_zip();
        let fs = ZipFileSystem::open("", bytes).unwrap();
        assert_eq!(fs.read_file("a.txt 1").unwrap(), b"1");
        assert_eq!(fs.get_file_type("a.txt", None), FileType::NotFound);
        assert_eq!(fs.file_count(""), 1);
    }

    #[test]
    fn file_size_prefers_loaded_bytes_and_tolerates_gaps() {
        let mut fs = open_fixture(&[
            RawEntry::stored("dir/a.txt", b"12345"),
            RawEntry::stored("top.txt", b"1"),
        ]);
        assert_eq!(fs.file_size("nothere", 0), 0);
        assert_eq!(fs.file_size("", 1), 1);
        assert_eq!(fs.file_size("dir", 0), 5, "recorded size when unloaded");

        // Removing the directory placeholder drops it from the parent
        // listing, so the surviving row shifts to index 0.
        assert!(fs.erase_file_or_directory("dir/a.txt"));
        assert!(fs.erase_file_or_directory("dir"));
        assert_eq!(fs.file_count(""), 1);
        assert_eq!(fs.file_size("", 0), 1, "top.txt shifted forward");
    }

    #[test]
    fn directory_listings_decode_duplicate_names() {
        let fs = open_fixture(&[
            RawEntry::stored("dir/a.txt", b"1"),
            RawEntry::stored("dir/b.txt", b"2"),
            RawEntry::stored("top.txt", b"3"),
        ]);
        assert_eq!(fs.file_count("dir"), 2);
        // The root lists "dir" (materialised) plus "top.txt".
        assert_eq!(fs.file_count(""), 2);
        assert_eq!(fs.file_count("nothere"), 0);
        assert_eq!(fs.file_name("dir", 0).unwrap(), "a.txt");
        assert_eq!(fs.file_name("dir", 1).unwrap(), "b.txt");
        assert_eq!(fs.file_name("dir", 2), None);
        assert_eq!(fs.read_directory("dir").unwrap(), vec!["a.txt", "b.txt"]);
        assert_eq!(fs.read_directory("").unwrap(), vec!["dir", "top.txt"]);
        assert_eq!(fs.read_directory("nothere"), None);
        assert_eq!(fs.file_size("dir", 0), 1);
        assert_eq!(fs.file_size("dir", 5), 0);
    }

    #[test]
    fn written_files_roundtrip_through_create_zip() {
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        assert!(fs.write_to_structure("hello.txt", b"hello".to_vec()));
        fs.update_file("dir/data.bin", Some(noisy(64)));
        fs.update_file("dir/sub/deep.txt", Some(b"deep".to_vec()));

        let bytes = fs.create_zip();
        let fs = ZipFileSystem::open("", bytes).unwrap();
        assert_eq!(fs.read_file("hello.txt").unwrap(), b"hello");
        assert_eq!(fs.read_file("dir/data.bin").unwrap(), noisy(64));
        assert_eq!(fs.read_file("dir/sub/deep.txt").unwrap(), b"deep");
        assert_eq!(fs.get_file_type("dir", None), FileType::Directory);
        assert_eq!(fs.get_file_type("dir/sub", None), FileType::Directory);
        // Root lists "hello.txt" plus the materialised "dir".
        assert_eq!(fs.file_count(""), 2);
        assert_eq!(fs.file_count("dir"), 2);
    }

    #[test]
    fn create_zip_keeps_only_rows_carrying_data() {
        // isValid() is pData_ != NULL in the reference: rows freshly parsed
        // from an archive hold no bytes, so createZip drops them alongside
        // the structure-only directory placeholders.
        let fs = open_fixture(&[
            RawEntry::stored("dir/child.txt", b"c"),
            RawEntry::stored("top.txt", b"t"),
        ]);
        let mut fs = fs;
        assert!(fs.erase_file_or_directory("dir/child.txt"));
        let bytes = fs.create_zip();
        assert_eq!(bytes, empty_zip(), "nothing was loaded, nothing survives");
        let fs = ZipFileSystem::open("", bytes).unwrap();
        assert_eq!(fs.get_file_type("top.txt", None), FileType::NotFound);
        assert_eq!(fs.file_count(""), 0);

        // Rows written in-session carry their bytes and survive.
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        fs.update_file("kept.txt", Some(b"data".to_vec()));
        let bytes = fs.create_zip();
        let fs = ZipFileSystem::open("", bytes).unwrap();
        assert_eq!(fs.read_file("kept.txt").unwrap(), b"data");
        assert_eq!(fs.file_count(""), 1);
    }

    #[test]
    fn rewriting_a_name_encodes_both_rows() {
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        fs.update_file("cfg.txt", Some(b"one".to_vec()));
        fs.update_file("cfg.txt", Some(b"two".to_vec()));

        // In-session rows have no local-header offset yet: reads stay empty
        // until createZip serialises them (reference readFileInternal order).
        assert_eq!(fs.read_file("cfg.txt"), None);
        assert_eq!(fs.file_count(""), 2);
        // Listings show the decoded form twice.
        assert_eq!(
            fs.read_directory("").unwrap(),
            vec!["cfg.txt".to_string(), "cfg.txt".to_string()]
        );

        // A third write encodes as "cfg.txt 3" (all rows in-session, so one
        // createZip serialises all of them).
        fs.update_file("cfg.txt", Some(b"three".to_vec()));
        assert_eq!(fs.file_count(""), 3);
        let bytes = fs.create_zip();
        let fs = ZipFileSystem::open("", bytes).unwrap();
        // The decoded name resolves to the newest row.
        assert_eq!(fs.read_file("cfg.txt").unwrap(), b"three");
        assert_eq!(fs.read_file("cfg.txt 2").unwrap(), b"two");
        // The first occurrence survives under its encoded name.
        assert_eq!(fs.read_file("cfg.txt 1").unwrap(), b"one");
    }

    #[test]
    fn erasing_reindexes_the_directory_and_duplicates() {
        let mut fs = open_fixture(&[
            RawEntry::stored("dir/a.txt", b"1"),
            RawEntry::stored("dir/b.txt", b"2"),
            RawEntry::stored("dir/c.txt", b"3"),
        ]);
        assert!(fs.erase_file_or_directory("dir/b.txt"));
        assert_eq!(fs.file_count("dir"), 2);
        assert_eq!(fs.file_name("dir", 0).unwrap(), "a.txt");
        assert_eq!(fs.file_name("dir", 1).unwrap(), "c.txt");
        assert_eq!(fs.read_file("dir/c.txt").unwrap(), b"3");
        assert_eq!(fs.file_index("dir/c.txt"), Some(1));
        assert!(fs.read_file("dir/b.txt").is_none());
        assert!(!fs.erase_file_or_directory("dir/b.txt"), "already gone");

        // Duplicate rows erase newest-first through the decoded name.
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        fs.update_file("cfg.txt", Some(b"one".to_vec()));
        fs.update_file("cfg.txt", Some(b"two".to_vec()));
        let bytes = fs.create_zip();
        let mut fs = ZipFileSystem::open("", bytes).unwrap();
        assert!(fs.erase_file_or_directory("cfg.txt"));
        assert_eq!(fs.read_file("cfg.txt").unwrap(), b"one", "count fell to 1");
        assert_eq!(fs.file_count(""), 1);
        assert!(fs.erase_file_or_directory("cfg.txt"));
        assert!(fs.read_file("cfg.txt").is_none());
        assert_eq!(fs.file_count(""), 0);
    }

    #[test]
    fn clear_dir_empties_one_directory() {
        let mut fs = open_fixture(&[
            RawEntry::stored("dir/a.txt", b"1"),
            RawEntry::stored("dir/b.txt", b"2"),
            RawEntry::stored("top.txt", b"3"),
        ]);
        fs.clear_dir("dir");
        assert_eq!(fs.file_count("dir"), 0);
        assert!(fs.read_file("dir/a.txt").is_none());
        assert_eq!(fs.read_file("top.txt").unwrap(), b"3");
        fs.clear_dir("");
        assert_eq!(fs.file_count(""), 0);
        assert!(fs.read_file("top.txt").is_none());

        // Encoded rows retire their decoded duplicates entry on clear.
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        fs.update_file("cfg.txt", Some(b"one".to_vec()));
        fs.update_file("cfg.txt", Some(b"two".to_vec()));
        fs.clear_dir("");
        assert_eq!(fs.file_count(""), 0);
        assert!(fs.duplicates.is_empty());

        // Clearing an unknown directory is a no-op.
        fs.clear_dir("nothere");
        assert_eq!(fs.file_count(""), 0);
    }

    #[test]
    fn create_zip_matches_hand_computed_bytes() {
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        let payload = noisy(64);
        fs.update_file_with_stamp("a.bin", &payload, 0x5821, 0x6c38);
        fs.update_file_with_stamp("d/b.txt", b"hey", 0x4d21, 0x0000);

        let out = fs.create_zip();
        let mut want = Vec::new();

        // Local row 1: "a.bin", stored, 64 bytes.
        want.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        want.extend_from_slice(&10u16.to_le_bytes());
        want.extend_from_slice(&0u16.to_le_bytes());
        want.extend_from_slice(&METHOD_STORE.to_le_bytes());
        want.extend_from_slice(&0x6c38u16.to_le_bytes());
        want.extend_from_slice(&0x5821u16.to_le_bytes());
        want.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        want.extend_from_slice(&64u32.to_le_bytes());
        want.extend_from_slice(&64u32.to_le_bytes());
        want.extend_from_slice(&5u16.to_le_bytes());
        want.extend_from_slice(&0u16.to_le_bytes());
        want.extend_from_slice(b"a.bin");
        want.extend_from_slice(&payload);

        // Local row 2: "d/b.txt".
        want.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        want.extend_from_slice(&10u16.to_le_bytes());
        want.extend_from_slice(&0u16.to_le_bytes());
        want.extend_from_slice(&METHOD_STORE.to_le_bytes());
        want.extend_from_slice(&0x0000u16.to_le_bytes());
        want.extend_from_slice(&0x4d21u16.to_le_bytes());
        want.extend_from_slice(&crc32fast::hash(b"hey").to_le_bytes());
        want.extend_from_slice(&3u32.to_le_bytes());
        want.extend_from_slice(&3u32.to_le_bytes());
        want.extend_from_slice(&7u16.to_le_bytes());
        want.extend_from_slice(&0u16.to_le_bytes());
        want.extend_from_slice(b"d/b.txt");
        want.extend_from_slice(b"hey");

        let dir_offset = want.len() as u32;
        for (name, data, off, date, time, size) in [
            ("a.bin", payload.as_slice(), 0u32, 0x5821u16, 0x6c38u16, 64u32),
            ("d/b.txt", b"hey".as_slice(), 30 + 5 + 64, 0x4d21, 0x0000, 3),
        ] {
            want.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            want.extend_from_slice(&10u16.to_le_bytes());
            want.extend_from_slice(&10u16.to_le_bytes());
            want.extend_from_slice(&0u16.to_le_bytes());
            want.extend_from_slice(&METHOD_STORE.to_le_bytes());
            want.extend_from_slice(&time.to_le_bytes());
            want.extend_from_slice(&date.to_le_bytes());
            want.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
            want.extend_from_slice(&size.to_le_bytes());
            want.extend_from_slice(&size.to_le_bytes());
            want.extend_from_slice(&(name.len() as u16).to_le_bytes());
            want.extend_from_slice(&0u16.to_le_bytes());
            want.extend_from_slice(&0u16.to_le_bytes());
            want.extend_from_slice(&0u16.to_le_bytes());
            want.extend_from_slice(&0u16.to_le_bytes());
            want.extend_from_slice(&0u32.to_le_bytes());
            want.extend_from_slice(&off.to_le_bytes());
            want.extend_from_slice(name.as_bytes());
        }
        let dir_size = want.len() as u32 - dir_offset;

        want.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        want.extend_from_slice(&0u16.to_le_bytes());
        want.extend_from_slice(&0u16.to_le_bytes());
        want.extend_from_slice(&2u16.to_le_bytes());
        want.extend_from_slice(&2u16.to_le_bytes());
        want.extend_from_slice(&dir_size.to_le_bytes());
        want.extend_from_slice(&dir_offset.to_le_bytes());
        want.extend_from_slice(&0u16.to_le_bytes());

        assert_eq!(out, want);
    }

    #[test]
    fn rename_keeps_the_original_compression_intact() {
        // Regression for the bCompressed_ latch: renaming must not re-walk
        // the compress block or the data would deflate twice.
        let mut fs = ZipFileSystem::open("", empty_zip()).unwrap();
        fs.update_file("old.txt", Some(b"payload payload payload".to_vec()));
        let (central, _) = fs.file_map["old.txt"];
        let method = fs.central_directory[central as usize].header.compression_method;
        fs.update_file("new.txt", Some(b"other".to_vec()));
        fs.update_file("old.txt", Some(b"replacement".to_vec())); // renames old to "old.txt 1"

        let renamed = &fs.central_directory[central as usize];
        assert_eq!(renamed.filename, "old.txt 1");
        assert_eq!(renamed.header.compression_method, method);
        // The local header keeps the original stamp: updateHeader never
        // re-walked the compress block.
        assert_eq!(renamed.header.uncompressed_size, 23);
        assert_eq!(renamed.uncompressed_size(), 0, "entry fills at writeDirEntry");
        let bytes = fs.create_zip();
        let fs = ZipFileSystem::open("", bytes).unwrap();
        assert_eq!(
            fs.read_file("old.txt 1").unwrap(),
            b"payload payload payload"
        );
    }

    #[test]
    fn local_header_records_roundtrip_byte_for_byte() {
        let header = LocalHeader {
            signature: LOCAL_HEADER_SIGNATURE,
            extractor_version: 10,
            mask: 0xabcd,
            compression_method: METHOD_DEFLATE,
            modified_time: 0x1234,
            modified_date: 0x5678,
            crc32: 0xdead_beef,
            compressed_size: 111,
            uncompressed_size: 222,
            filename_length: 3,
            extra_field_length: 4,
        };
        let mut bytes = Vec::new();
        header.write(&mut bytes);
        assert_eq!(bytes.len(), LOCAL_HEADER_SIZE);
        assert_eq!(LocalHeader::parse(&bytes), header);

        let entry = DirEntry {
            signature: DIR_ENTRY_SIGNATURE,
            creator_version: 10,
            extractor_version: 10,
            mask: 7,
            compression_method: METHOD_STORE,
            modified_time: 0x1111,
            modified_date: 0x2222,
            crc32: 0x3333_4444,
            compressed_size: 1,
            uncompressed_size: 2,
            filename_length: 5,
            extra_field_length: 6,
            file_comment_length: 7,
            disk_number_start: 8,
            internal_file_attr: 9,
            external_file_attr: 10,
            local_header_offset: -1,
        };
        let mut bytes = Vec::new();
        entry.write(&mut bytes);
        assert_eq!(bytes.len(), DIR_ENTRY_SIZE);
        assert_eq!(DirEntry::parse(&bytes), entry);

        let footer = DirFooter {
            signature: DIR_FOOTER_SIGNATURE,
            disk_number: 1,
            dir_start_disk: 2,
            dir_entries: 3,
            total_dir_entries: 4,
            dir_size: 5,
            dir_offset: 6,
            comment_length: 7,
        };
        let mut bytes = Vec::new();
        footer.write(&mut bytes);
        assert_eq!(bytes.len(), DIR_FOOTER_SIZE);
        assert_eq!(DirFooter::parse(&bytes), footer);
    }

    #[test]
    fn disk_names_and_size_on_disk_cover_folder_rows() {
        let file = LocalFile::with_stamp("plain.txt", b"12345".to_vec(), 0, 0);
        assert_eq!(file.disk_name(), "plain.txt");
        // 30 local header + name twice (9*2) + data + 46 directory entry.
        assert_eq!(file.size_on_disk(), 30 + 18 + 5 + 46);
        assert!(file.is_valid());

        // A folder row: trailing slash on disk, no payload.
        let mut folder = LocalFile {
            filename: "dir".to_string(),
            ..Default::default()
        };
        folder.set_folder(true);
        folder.entry.local_header_offset = -1;
        assert!(folder.is_folder());
        assert_eq!(folder.disk_name(), "dir/");
        assert_eq!(folder.size_on_disk(), 30 + 8 + 0 + 46);
        assert!(!folder.is_valid());
        folder.clear();
        assert!(folder.filename.is_empty());
        assert!(!folder.is_valid());
    }
}

