//! BW-10-02-01 the `BWTB` container header, section table and stream reads.
//!
//! A port of `lib/compiled_space/binary_format_types.hpp` (`Header`,
//! `SectionHeader`, `MAX_SECTIONS`) and `lib/compiled_space/binary_format.cpp`
//! (`BinaryFormat`, `BinaryFormat::Stream`).
//!
//! Byte layout: `#pragma pack(push, 1)` little-endian. The container header is
//! 24 bytes and the section table follows at byte 24 — the reader computes it
//! as `(SectionHeader*)(pHeader_ + 1)` and never consults `Header::headerSize_`,
//! so a header/table disagreement goes unnoticed. `Header::fileSize_` is also
//! never validated: the original writer memsets the header and never stores
//! the file size, so every genuine `space.bin` carries a zero there.

/// A FourCC section/容器 magic: four raw bytes, in file order.
pub type Magic = [u8; 4];

/// `BinaryFormatTypes::FORMAT_MAGIC` — the container magic "BWTB".
pub const FORMAT_MAGIC: Magic = *b"BWTB";

/// `BinaryFormatTypes::FORMAT_VERSION`.
pub const FORMAT_VERSION: u32 = 1;

/// `BinaryFormatTypes::MAX_SECTIONS` — the writer's `appendSection` limit
/// (and the reader's debug-only assert). A genuine file never exceeds it.
pub const MAX_SECTIONS: usize = 32;

/// `sizeof(BinaryFormatTypes::Header)` (`binary_format.cpp:202-215`): the
/// section table starts immediately after the 24-byte header.
pub const HEADER_SIZE: usize = 24;

/// `sizeof(BinaryFormatTypes::SectionHeader)`.
pub const SECTION_HEADER_SIZE: usize = 24;

/// `BinaryFormat::NOT_FOUND` is `size_t(-1)`; the port returns `None` instead.
pub const NOT_FOUND: usize = usize::MAX;

/// `BinaryFormatTypes::Header`: the 24-byte packed container header.
///
/// `header_size` and `file_size` are parsed and exposed but never validated by
/// the reference reader; `file_size` is zero in every genuine file because the
/// original writer never sets it (`binary_format_writer.cpp` memsets the
/// header it writes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub magic: Magic,
    pub version: u32,
    pub header_size: u32,
    pub file_size: u64,
    pub num_sections: u32,
}

impl Header {
    /// Parse a 24-byte little-endian container header.
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_SIZE {
            return None;
        }
        Some(Self {
            magic: [bytes[0], bytes[1], bytes[2], bytes[3]],
            version: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            header_size: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            file_size: u64::from_le_bytes([
                bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18],
                bytes[19],
            ]),
            num_sections: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        })
    }
}

/// `BinaryFormatTypes::SectionHeader`: a 24-byte packed section descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionHeader {
    pub magic: Magic,
    pub version: u32,
    /// Absolute offset from the start of the file (the first payload starts
    /// at `sizeof(Header) + numSections * sizeof(SectionHeader)`).
    pub offset: u64,
    pub length: u64,
}

impl SectionHeader {
    /// Parse a 24-byte little-endian section header.
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < SECTION_HEADER_SIZE {
            return None;
        }
        Some(Self {
            magic: [bytes[0], bytes[1], bytes[2], bytes[3]],
            version: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            offset: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            length: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
        })
    }
}

/// Why `BinaryFormat::open` refused the bytes. The reference returns bare
/// `false` (`binary_format.cpp:158-221`); the port keeps the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// Magic or container version mismatch (`binary_format.cpp:193-200`).
    BadMagicOrVersion,
    /// Fewer than 24 bytes: the first `memoryMap(sizeof(Header))` would fail.
    HeaderTruncated,
    /// The section table `sizeof(Header) + numSections * sizeof(SectionHeader)`
    /// does not fit; the reference's second, larger `memoryMap` would fail
    /// (`binary_format.cpp:202-213`).
    SectionTableTruncated,
}

/// `BinaryFormat`: one parsed `space.bin` holding the whole file.
///
/// The reference memory-maps sections out of an open `IFileStream*`; the port
/// owns the bytes and lends `Stream` borrows into them, which is the same
/// lifetime shape (nothing is copied) without the mmap machinery.
#[derive(Debug, Clone)]
pub struct BinaryFormat {
    resource_id: String,
    data: Vec<u8>,
    header: Header,
    sections: Vec<SectionHeader>,
}

impl BinaryFormat {
    /// `BinaryFormat::open` (`binary_format.cpp:158-221`) over an in-memory
    /// file. `resource_id` is the display name the error messages quote.
    pub fn open(resource_id: &str, data: Vec<u8>) -> Result<Self, OpenError> {
        let header = Header::from_le_bytes(&data).ok_or(OpenError::HeaderTruncated)?;

        // `binary_format.cpp:193-200`: mismatched magic or version fails the
        // open. Nothing else in the header is validated.
        if header.magic != FORMAT_MAGIC || header.version != FORMAT_VERSION {
            return Err(OpenError::BadMagicOrVersion);
        }

        // `binary_format.cpp:202-213`: the reader remaps the header plus the
        // section table sized from numSections_ itself (headerSize_ is never
        // consulted), so a table that runs past the file fails here.
        let table_end = HEADER_SIZE + header.num_sections as usize * SECTION_HEADER_SIZE;
        if table_end > data.len() {
            return Err(OpenError::SectionTableTruncated);
        }

        let mut sections = Vec::with_capacity(header.num_sections as usize);
        for i in 0..header.num_sections as usize {
            let at = HEADER_SIZE + i * SECTION_HEADER_SIZE;
            let header = SectionHeader::from_le_bytes(&data[at..at + SECTION_HEADER_SIZE])
                .expect("the section table was bounds-checked above");
            sections.push(header);
        }

        Ok(Self {
            resource_id: resource_id.to_string(),
            data,
            header,
            sections,
        })
    }

    /// `BinaryFormat::resourceID` (`binary_format.cpp:245-248`).
    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }

    /// The parsed container header (`pHeader_`).
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// `BinaryFormat::numSections` (`binary_format.cpp:259-263`).
    pub fn num_sections(&self) -> usize {
        self.sections.len()
    }

    /// `BinaryFormat::sectionMagic` (`binary_format.cpp:267-272`). The
    /// reference asserts the index and then reads out of bounds in release;
    /// the port returns `None`.
    pub fn section_magic(&self, section_idx: usize) -> Option<Magic> {
        self.sections.get(section_idx).map(|s| s.magic)
    }

    /// `BinaryFormat::sectionVersion` (`binary_format.cpp:276-281`).
    pub fn section_version(&self, section_idx: usize) -> Option<u32> {
        self.sections.get(section_idx).map(|s| s.version)
    }

    /// `BinaryFormat::sectionLength` (`binary_format.cpp:285-290`).
    pub fn section_length(&self, section_idx: usize) -> Option<u64> {
        self.sections.get(section_idx).map(|s| s.length)
    }

    /// `BinaryFormat::findSection` (`binary_format.cpp:294-305`): a linear
    /// scan returning the first match; `NOT_FOUND` becomes `None`.
    pub fn find_section(&self, magic: Magic) -> Option<usize> {
        self.sections.iter().position(|s| s.magic == magic)
    }

    /// `BinaryFormat::openSection` (`binary_format.cpp:309-327`): a stream
    /// over the section payload.
    ///
    /// Divergence registered: the reference `memoryMap`s the declared range
    /// and hands back a stream even when the map failed (a NULL buffer, then
    /// UB on first read); the port returns `None`, which is what lets
    /// [`Self::find_and_open_section`] report the "failed to map" failure its
    /// reference branch was written for.
    pub fn open_section(&self, section_idx: usize) -> Option<Stream<'_>> {
        let section = self.sections.get(section_idx)?;
        let start = usize::try_from(section.offset).ok()?;
        let end = start.checked_add(usize::try_from(section.length).ok()?)?;
        let data = self.data.get(start..end)?;
        Some(Stream::new(data))
    }

    /// `BinaryFormat::findAndOpenSection` (`binary_format.cpp:331-360`) with
    /// the reference's message strings carried in the error.
    ///
    /// Quirk kept: the wrong-version message prints the *found* version as
    /// "expected" and the *required* one as "got", because the reference
    /// passes `foundVersion, requiredVersion` in that order
    /// (`binary_format.cpp:343-349`).
    pub fn find_and_open_section(
        &self,
        magic: Magic,
        required_version: u32,
        name_for_errors: &str,
    ) -> Result<Stream<'_>, String> {
        let section_idx = self.find_section(magic).ok_or_else(|| {
            format!(
                "'{}' does not contain {}.\n",
                self.resource_id, name_for_errors
            )
        })?;

        let found_version = self.section_version(section_idx).expect("find_section hit");
        if found_version != required_version {
            return Err(format!(
                "'{}' has the wrong version of {} (expected {}, got {}).\n",
                self.resource_id, name_for_errors, found_version, required_version
            ));
        }

        self.open_section(section_idx).ok_or_else(|| {
            format!(
                "'{}' failed to map {} into memory.\n",
                self.resource_id, name_for_errors
            )
        })
    }
}

/// `BinaryFormat::Stream` (`binary_format.cpp:15-137`): a cursor over one
/// mapped section payload.
///
/// Read protocol (`binary_format.hpp:107-182`):
///
/// - `read(uint32&)` is a raw little-endian u32 with no prefix;
/// - `read(T&)` / `read(T*&)` is a u32 typeSize prefix (which must equal
///   `sizeof(T)`) followed by the payload;
/// - `read(ExternalArray<T>&)` is the same prefix plus a u32 count followed
///   by `count * sizeof(T)` raw bytes.
#[derive(Debug, Clone, Copy)]
pub struct Stream<'a> {
    data: &'a [u8],
    cursor: usize,
}

impl<'a> Stream<'a> {
    /// `BinaryFormat::Stream::Stream(void*, size_t)` — over a whole payload.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, cursor: 0 }
    }

    /// `BinaryFormat::Stream::isValid` (`binary_format.cpp:44-47`): a buffer
    /// is valid only when it is non-NULL *and* has length.
    pub fn is_valid(&self) -> bool {
        !self.data.is_empty()
    }

    /// `BinaryFormat::Stream::length` / `size` (`binary_format.cpp:51-68`).
    pub fn length(&self) -> usize {
        self.data.len()
    }

    /// `BinaryFormat::Stream::eof` (`binary_format.cpp:58-61`).
    pub fn eof(&self) -> bool {
        self.cursor >= self.data.len()
    }

    /// `BinaryFormat::Stream::tell` (`binary_format.cpp:80-83`).
    pub fn tell(&self) -> usize {
        self.cursor
    }

    /// `BinaryFormat::Stream::seek` (`binary_format.cpp:87-90`): the offset
    /// clamps to the length rather than failing.
    pub fn seek(&mut self, offset: usize) {
        self.cursor = offset.min(self.data.len());
    }

    /// `BinaryFormat::Stream::readRaw` (`binary_format.cpp:94-104`): hands
    /// back `len` bytes and advances, or fails at the end of the section.
    /// A zero-length read at EOF succeeds, exactly like the reference.
    pub fn read_raw(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.cursor.checked_add(len)?;
        if end > self.data.len() {
            return None;
        }
        let out = &self.data[self.cursor..end];
        self.cursor = end;
        Some(out)
    }

    /// `read(uint32&)` (`binary_format.cpp:108-118`): four raw little-endian
    /// bytes, no typeSize prefix.
    pub fn read_u32(&mut self) -> Option<u32> {
        let bytes = self.read_raw(4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// `read(T&)` / `read(T*&)` (`binary_format.hpp:108-148`): a u32 typeSize
    /// prefix that must equal `size`, then `size` raw bytes.
    pub fn read_typed(&mut self, size: usize) -> Option<&'a [u8]> {
        let type_size = self.read_u32()?;
        if type_size as usize != size {
            return None;
        }
        self.read_raw(size)
    }

    /// `read(ExternalArray<T>&)` (`binary_format.hpp:150-182`): a u32
    /// typeSize prefix that must equal `elem_size`, a u32 count, then
    /// `count * elem_size` raw bytes.
    pub fn read_external_array(&mut self, elem_size: usize) -> Option<&'a [u8]> {
        let type_size = self.read_u32()?;
        if type_size as usize != elem_size {
            return None;
        }
        let count = self.read_u32()? as usize;
        self.read_raw(elem_size.checked_mul(count)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_bytes(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn header_bytes(magic: Magic, version: u32, num_sections: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&magic);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&96u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&num_sections.to_le_bytes());
        out
    }

    fn section_bytes(magic: Magic, version: u32, offset: u64, length: u64) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&magic);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
        out
    }

    /// The minimal container shape the round-trip tests share: one section
    /// whose payload starts right after the table.
    fn one_section_container(payload: &[u8]) -> Vec<u8> {
        let mut out = header_bytes(FORMAT_MAGIC, FORMAT_VERSION, 1);
        out.extend_from_slice(&section_bytes(*b"TEST", 7, 48, payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn header_parses_the_24_byte_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BWTB");
        bytes.extend_from_slice(&u32_bytes(1));
        bytes.extend_from_slice(&u32_bytes(96));
        bytes.extend_from_slice(&0x1122334455667788u64.to_le_bytes());
        bytes.extend_from_slice(&u32_bytes(3));
        assert_eq!(bytes.len(), 24);

        let header = Header::from_le_bytes(&bytes).unwrap();
        assert_eq!(header.magic, *b"BWTB");
        assert_eq!(header.version, 1);
        assert_eq!(header.header_size, 96);
        assert_eq!(header.file_size, 0x1122334455667788);
        assert_eq!(header.num_sections, 3);
    }

    #[test]
    fn header_needs_24_bytes() {
        assert!(Header::from_le_bytes(&[0; 23]).is_none());
        assert!(Header::from_le_bytes(&[]).is_none());
        assert!(Header::from_le_bytes(&[0; 24]).is_some());
    }

    #[test]
    fn section_header_parses_the_24_byte_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BWST");
        bytes.extend_from_slice(&u32_bytes(1));
        bytes.extend_from_slice(&0x0102030405060708u64.to_le_bytes());
        bytes.extend_from_slice(&0xb8u64.to_le_bytes());
        assert_eq!(bytes.len(), 24);

        let section = SectionHeader::from_le_bytes(&bytes).unwrap();
        assert_eq!(section.magic, *b"BWST");
        assert_eq!(section.version, 1);
        assert_eq!(section.offset, 0x0102030405060708);
        assert_eq!(section.length, 0xb8);
    }

    #[test]
    fn section_header_needs_24_bytes() {
        assert!(SectionHeader::from_le_bytes(&[0; 23]).is_none());
    }

    #[test]
    fn open_rejects_a_truncated_header() {
        assert_eq!(
            BinaryFormat::open("short.bin", vec![0; 23]).unwrap_err(),
            OpenError::HeaderTruncated
        );
    }

    #[test]
    fn open_rejects_the_wrong_magic_or_version() {
        let bad_magic = header_bytes(*b"XXXX", FORMAT_VERSION, 0);
        assert_eq!(
            BinaryFormat::open("bad.bin", bad_magic).unwrap_err(),
            OpenError::BadMagicOrVersion
        );

        let bad_version = header_bytes(FORMAT_MAGIC, FORMAT_VERSION + 1, 0);
        assert_eq!(
            BinaryFormat::open("bad.bin", bad_version).unwrap_err(),
            OpenError::BadMagicOrVersion
        );
    }

    #[test]
    fn open_rejects_a_truncated_section_table() {
        // The header claims two sections but only one table entry is present.
        let mut bytes = header_bytes(FORMAT_MAGIC, FORMAT_VERSION, 2);
        bytes.extend_from_slice(&section_bytes(*b"TEST", 1, 48, 0));
        assert_eq!(
            BinaryFormat::open("cut.bin", bytes).unwrap_err(),
            OpenError::SectionTableTruncated
        );
    }

    #[test]
    fn open_accepts_a_header_only_container() {
        // The empty container of the original unit test: zero sections.
        let bytes = header_bytes(FORMAT_MAGIC, FORMAT_VERSION, 0);
        let format = BinaryFormat::open("empty.bin", bytes).unwrap();
        assert_eq!(format.num_sections(), 0);
        assert_eq!(format.resource_id(), "empty.bin");
        // fileSize is never validated: zero stays zero.
        assert_eq!(format.header().file_size, 0);
    }

    #[test]
    fn section_accessors_read_the_table() {
        let bytes = one_section_container(b"payload");
        let format = BinaryFormat::open("one.bin", bytes).unwrap();
        assert_eq!(format.num_sections(), 1);
        assert_eq!(format.section_magic(0), Some(*b"TEST"));
        assert_eq!(format.section_version(0), Some(7));
        assert_eq!(format.section_length(0), Some(7));
        assert_eq!(format.section_magic(1), None);
        assert_eq!(format.section_version(1), None);
        assert_eq!(format.section_length(1), None);
    }

    #[test]
    fn find_section_scans_linearly() {
        let mut bytes = header_bytes(FORMAT_MAGIC, FORMAT_VERSION, 2);
        bytes.extend_from_slice(&section_bytes(*b"AAAA", 1, 72, 0));
        bytes.extend_from_slice(&section_bytes(*b"BBBB", 1, 72, 0));
        let format = BinaryFormat::open("two.bin", bytes).unwrap();

        assert_eq!(format.find_section(*b"BBBB"), Some(1));
        assert_eq!(format.find_section(*b"AAAA"), Some(0));
        assert_eq!(format.find_section(*b"ZZZZ"), None);
    }

    #[test]
    fn find_and_open_section_reports_missing_sections() {
        let format = BinaryFormat::open("one.bin", one_section_container(b"payload")).unwrap();
        let err = format
            .find_and_open_section(*b"MISS", 1, "StringTable")
            .unwrap_err();
        assert_eq!(err, "'one.bin' does not contain StringTable.\n");
    }

    #[test]
    fn find_and_open_section_reports_wrong_versions_with_swapped_arguments() {
        // Reference quirk: the found version prints as "expected" and the
        // required one as "got" (binary_format.cpp:343-349).
        let format = BinaryFormat::open("one.bin", one_section_container(b"payload")).unwrap();
        let err = format
            .find_and_open_section(*b"TEST", 9, "StringTable")
            .unwrap_err();
        assert_eq!(
            err,
            "'one.bin' has the wrong version of StringTable (expected 7, got 9).\n"
        );
    }

    #[test]
    fn find_and_open_section_reports_unmappable_sections() {
        // The section header declares more payload than the file holds.
        let mut bytes = header_bytes(FORMAT_MAGIC, FORMAT_VERSION, 1);
        bytes.extend_from_slice(&section_bytes(*b"TEST", 7, 24, 100));
        let format = BinaryFormat::open("map.bin", bytes).unwrap();
        let err = format
            .find_and_open_section(*b"TEST", 7, "StringTable")
            .unwrap_err();
        assert_eq!(err, "'map.bin' failed to map StringTable into memory.\n");
    }

    #[test]
    fn find_and_open_section_hands_back_the_payload_stream() {
        let format = BinaryFormat::open("one.bin", one_section_container(b"payload")).unwrap();
        let mut stream = format
            .find_and_open_section(*b"TEST", 7, "StringTable")
            .unwrap();
        assert_eq!(stream.read_raw(7).unwrap(), b"payload");
    }

    #[test]
    fn stream_reads_raw_bytes_and_reports_eof() {
        let mut stream = Stream::new(b"abcdef");
        assert!(!stream.eof());
        assert_eq!(stream.length(), 6);
        assert_eq!(stream.tell(), 0);
        assert_eq!(stream.read_raw(3).unwrap(), b"abc");
        assert_eq!(stream.tell(), 3);
        assert_eq!(stream.read_raw(4), None);
        // A zero-length read at the end still succeeds (readRaw(0)).
        assert_eq!(stream.read_raw(3).unwrap(), b"def");
        assert!(stream.eof());
        assert_eq!(stream.read_raw(0).unwrap(), b"");
    }

    #[test]
    fn stream_seek_clamps_to_the_length() {
        let mut stream = Stream::new(b"abcdef");
        stream.seek(2);
        assert_eq!(stream.tell(), 2);
        stream.seek(99);
        assert_eq!(stream.tell(), 6);
        assert!(stream.eof());
    }

    #[test]
    fn stream_validity_requires_a_non_empty_buffer() {
        assert!(Stream::new(b"x").is_valid());
        assert!(!Stream::new(b"").is_valid());
    }

    #[test]
    fn stream_reads_raw_little_endian_u32s() {
        let mut stream = Stream::new(&[0x78, 0x56, 0x34, 0x12, 0xff]);
        assert_eq!(stream.read_u32(), Some(0x12345678));
        assert_eq!(stream.read_u32(), None);
    }

    #[test]
    fn read_typed_enforces_the_type_size_prefix() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32_bytes(24));
        bytes.extend_from_slice(&[0u8; 24]);
        let mut stream = Stream::new(&bytes);
        assert_eq!(stream.read_typed(24).unwrap(), &[0u8; 24]);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32_bytes(23));
        bytes.extend_from_slice(&[0u8; 23]);
        let mut stream = Stream::new(&bytes);
        assert_eq!(stream.read_typed(24), None);
    }

    #[test]
    fn read_external_array_enforces_prefix_count_and_extent() {
        // Prefix + count + two 8-byte elements.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32_bytes(8));
        bytes.extend_from_slice(&u32_bytes(2));
        bytes.extend_from_slice(&[0u8; 16]);
        let mut stream = Stream::new(&bytes);
        assert_eq!(stream.read_external_array(8).unwrap().len(), 16);

        // Wrong type size fails immediately.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32_bytes(5));
        let mut stream = Stream::new(&bytes);
        assert_eq!(stream.read_external_array(8), None);

        // Count beyond the section fails the extent read.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32_bytes(8));
        bytes.extend_from_slice(&u32_bytes(4));
        bytes.extend_from_slice(&[0u8; 16]);
        let mut stream = Stream::new(&bytes);
        assert_eq!(stream.read_external_array(8), None);

        // A zero-length array is valid.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32_bytes(8));
        bytes.extend_from_slice(&u32_bytes(0));
        let mut stream = Stream::new(&bytes);
        assert_eq!(stream.read_external_array(8).unwrap(), b"");
    }
}
