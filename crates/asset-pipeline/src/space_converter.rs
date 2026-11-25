//! `SpaceConverter` port (`tools/asset_pipeline/converters/space_converter/`)
//! plus the compiled-space writer faces it drives
//! (`lib/compiled_space/binary_writers/`).
//!
//! The port keeps the reference's on-disk layout:
//! - [`BinaryFormatWriter`] — the `BWTB` container (24-byte header, 24-byte
//!   section headers, concatenated payloads; `fileSize_` is written zero just
//!   like the reference's memset header);
//! - [`StringTableWriter`] — the `BWST` section: an external array of
//!   `{offset, length}` entries, the string data length, then the
//!   NUL-terminated strings (`string_table_writer.cpp`);
//! - [`AssetListWriter`] — the `BWAL` section: an external array of 8-byte
//!   `{AssetType, pad, stringTableIndex}` entries, deduplicated by string
//!   table index (`asset_list_writer.cpp`);
//! - [`SettingsWriter`] — the `BWCS` section: a typed 24-byte AABB header
//!   whose zero bounds cover a space with no generators (`settings_writer.cpp`).
//!
//! Scope of this port: the chunk item dispatch keeps the reference's ignore
//! list plus the `model`/`shell` resource collection
//! (`StaticSceneModelWriter::convertModel/convertShell` →
//! `addFromChunkModel`); the writer types behind the other item handlers
//! (terrain, entities, particles, lights...) arrive with their own tasks, and
//! an unknown item type stays the reference's warning-and-skip.
//!
//! Deviation: the reference walks the space's chunk grid
//! (`boundsMinX_..boundsMaxX_` × `boundsMinZ_..boundsMaxZ_` +
//! `ChunkFormat::outsideChunkIdentifier`); the port enumerates the space
//! directory for `*.chunk`/`*.cdata` pairs instead, which needs neither the
//! grid settings nor the identifier mapping. A chunk whose `.cdata` twin is
//! missing is skipped exactly like the reference's ERROR_MSG branch.

use std::rc::Rc;

use foundation::math::Vector3;
use resources::bwresource::BwResource;
use resources::datasection::DataSection;

use crate::compiler::Compiler;
use crate::converter::Converter;
use crate::dependency_list::DependencyList;

/// `BinaryFormatTypes::FORMAT_MAGIC` (`binary_format_types.hpp`).
const FORMAT_MAGIC: [u8; 4] = *b"BWTB";
/// `BinaryFormatTypes::FORMAT_VERSION`.
const FORMAT_VERSION: u32 = 1;
/// `sizeof(Header)` in the reference's `#pragma pack(1)` header.
const HEADER_SIZE: usize = 24;
/// `sizeof(SectionHeader)`.
const SECTION_HEADER_SIZE: usize = 24;
/// `BinaryFormatTypes::MAX_SECTIONS`.
const MAX_SECTIONS: usize = 32;

/// `StringTableTypes::FORMAT_MAGIC`.
const STRING_TABLE_MAGIC: [u8; 4] = *b"BWST";
/// `StringTableTypes::FORMAT_VERSION`.
const STRING_TABLE_VERSION: u32 = 1;
/// `sizeof(StringTableTypes::Entry)` = `{uint32 offset_, uint32 length_}`.
const STRING_TABLE_ENTRY_SIZE: usize = 8;

/// `AssetListTypes::FORMAT_MAGIC`.
const ASSET_LIST_MAGIC: [u8; 4] = *b"BWAL";
/// `AssetListTypes::FORMAT_VERSION`.
const ASSET_LIST_VERSION: u32 = 1;
/// `sizeof(AssetListTypes::Entry)` = `{AssetType type_, pad, uint32 index_}`.
const ASSET_LIST_ENTRY_SIZE: usize = 8;
/// `AssetListTypes::ASSET_TYPE_MODEL`.
pub const ASSET_TYPE_MODEL: u8 = 6;

/// `CompiledSpaceSettingsTypes::FORMAT_MAGIC`.
const SETTINGS_MAGIC: [u8; 4] = *b"BWCS";
/// `CompiledSpaceSettingsTypes::FORMAT_VERSION`.
const SETTINGS_VERSION: u32 = 1;
/// `sizeof(CompiledSpaceSettingsTypes::Header)` = the AABB alone.
const SETTINGS_HEADER_SIZE: usize = 24;

/// `Stream::write( const uint32& )`: the raw four-byte overload (no size
/// prefix — the specialised overload, not the typed template).
fn write_u32(stream: &mut Vec<u8>, value: u32) {
    stream.extend_from_slice(&value.to_le_bytes());
}

/// `Stream::write( const T& )`: the typed template — the size prefix
/// followed by the raw bytes.
fn write_typed(stream: &mut Vec<u8>, value: &[u8]) {
    write_u32(stream, value.len() as u32);
    stream.extend_from_slice(value);
}

/// `Stream::write( const BW::vector<E>& )`: the external-array form —
/// element size, count, then the packed entries.
fn write_vector(stream: &mut Vec<u8>, elem_size: usize, data: &[u8]) {
    write_u32(stream, elem_size as u32);
    write_u32(stream, (data.len() / elem_size) as u32);
    stream.extend_from_slice(data);
}

/// Three little-endian f32s (`Vector3::operator<<` stream order).
fn write_vector3(bytes: &mut Vec<u8>, v: &Vector3) {
    bytes.extend_from_slice(&v.x.to_le_bytes());
    bytes.extend_from_slice(&v.y.to_le_bytes());
    bytes.extend_from_slice(&v.z.to_le_bytes());
}

/// `BinaryFormatWriter` (`binary_format_writer.cpp`): named payload
/// accumulators plus the container serialisation.
#[derive(Debug, Default)]
pub struct BinaryFormatWriter {
    sections: Vec<Section>,
}

#[derive(Debug)]
struct Section {
    magic: [u8; 4],
    version: u32,
    stream: Vec<u8>,
}

impl BinaryFormatWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// `BinaryFormatWriter::numSections`.
    pub fn num_sections(&self) -> usize {
        self.sections.len()
    }

    /// `BinaryFormatWriter::appendSection`: a fresh payload stream per magic.
    /// A duplicate magic is a CRITICAL_MSG in the reference; the port reports
    /// it (and a full section table) as `None`.
    pub fn append_section(&mut self, magic: [u8; 4], version: u32) -> Option<&mut Vec<u8>> {
        if self.sections.len() >= MAX_SECTIONS || self.sections.iter().any(|s| s.magic == magic) {
            return None;
        }
        self.sections.push(Section {
            magic,
            version,
            stream: Vec::new(),
        });
        Some(&mut self.sections.last_mut().unwrap().stream)
    }

    /// `BinaryFormatWriter::write`: the container bytes — the header (with a
    /// zero `fileSize_`, never set by the reference), the section headers
    /// (payload offsets from the start of the file) and the payloads.
    pub fn build(&self) -> Vec<u8> {
        let header_size = (HEADER_SIZE + SECTION_HEADER_SIZE * self.sections.len()) as u32;

        let mut out = Vec::new();
        out.extend_from_slice(&FORMAT_MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&header_size.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(self.sections.len() as u32).to_le_bytes());

        let mut offset = header_size as u64;
        for section in &self.sections {
            out.extend_from_slice(&section.magic);
            out.extend_from_slice(&section.version.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(section.stream.len() as u64).to_le_bytes());
            offset += section.stream.len() as u64;
        }
        for section in &self.sections {
            out.extend_from_slice(&section.stream);
        }
        out
    }
}

/// `StringTableWriter` (`string_table_writer.cpp`): the deduplicating string
/// list behind the `BWST` section.
#[derive(Debug, Default)]
pub struct StringTableWriter {
    strings: Vec<String>,
}

impl StringTableWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// `StringTableWriter::size`.
    pub fn size(&self) -> usize {
        self.strings.len()
    }

    /// `StringTableWriter::addString`: a linear dedupe; the entry's index.
    pub fn add_string(&mut self, string: &str) -> u32 {
        if let Some(index) = self.strings.iter().position(|s| s == string) {
            return index as u32;
        }
        self.strings.push(string.to_string());
        (self.strings.len() - 1) as u32
    }

    /// `StringTableWriter::writeToStream`: the `{offset, length}` entry
    /// array (offsets count the NUL too), the string data length, then the
    /// NUL-terminated strings.
    pub fn write(&self, writer: &mut BinaryFormatWriter) -> bool {
        let Some(stream) = writer.append_section(STRING_TABLE_MAGIC, STRING_TABLE_VERSION) else {
            return false;
        };

        let mut offset: u32 = 0;
        let mut entries: Vec<u8> = Vec::new();
        for string in &self.strings {
            write_u32(&mut entries, offset);
            write_u32(&mut entries, string.len() as u32);
            offset += string.len() as u32 + 1;
        }
        write_vector(stream, STRING_TABLE_ENTRY_SIZE, &entries);

        // "Write out the amount of data we're going to be outputting" — the
        // raw uint32 overload, no size prefix.
        write_u32(stream, offset);

        for string in &self.strings {
            // Making sure we include the NULL terminator.
            stream.extend_from_slice(string.as_bytes());
            stream.push(0);
        }
        true
    }
}

/// `AssetListWriter` (`asset_list_writer.cpp`): the `{type, stringTableIndex}`
/// entries of the `BWAL` section.
#[derive(Debug, Default)]
pub struct AssetListWriter {
    assets: Vec<(u8, u32)>,
}

impl AssetListWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// `AssetListWriter::size`.
    pub fn size(&self) -> usize {
        self.assets.len()
    }

    /// `AssetListWriter::addAsset`: the resource joins the string table
    /// first; an entry already pointing at that index adds nothing.
    pub fn add_asset(
        &mut self,
        asset_type: u8,
        resource_id: &str,
        strings: &mut StringTableWriter,
    ) -> u32 {
        let index = strings.add_string(resource_id);
        if !self.assets.iter().any(|&(_, i)| i == index) {
            self.assets.push((asset_type, index));
        }
        index
    }

    /// `AssetListWriter::write`: the entry external array; the reference's
    /// `#pragma pack(1)` Entry puts three padding bytes after the type.
    pub fn write(&self, writer: &mut BinaryFormatWriter) -> bool {
        let Some(stream) = writer.append_section(ASSET_LIST_MAGIC, ASSET_LIST_VERSION) else {
            return false;
        };

        let mut entries: Vec<u8> = Vec::new();
        for &(asset_type, index) in &self.assets {
            entries.push(asset_type);
            entries.extend_from_slice(&[0, 0, 0]);
            entries.extend_from_slice(&index.to_le_bytes());
        }
        write_vector(stream, ASSET_LIST_ENTRY_SIZE, &entries);
        true
    }
}

/// `SettingsWriter` (`settings_writer.cpp`): the union of the bound
/// generators, folded into the typed AABB header of the `BWCS` section.
/// [`None`] is the reference's inside-out box; a space whose writers
/// generated no bounds writes the zero point.
#[derive(Debug, Default)]
pub struct SettingsWriter {
    bounds: Option<(Vector3, Vector3)>,
}

impl SettingsWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// `AABB::insideOut`: no bounds were generated (or every one of them was
    /// inside-out itself).
    pub fn is_inside_out(&self) -> bool {
        self.bounds.is_none()
    }

    /// The unioned bounds, when any generator produced a usable box.
    pub fn bounds(&self) -> Option<(Vector3, Vector3)> {
        self.bounds
    }

    /// `SettingsWriter::addBounds` + `AABB::addBounds`: inside-out boxes are
    /// dropped; otherwise the running box is expanded to cover the new one.
    pub fn add_bounds(&mut self, min: Vector3, max: Vector3) {
        if min.x > max.x || min.y > max.y || min.z > max.z {
            return;
        }
        self.bounds = Some(match self.bounds {
            None => (min, max),
            Some((mn, mx)) => (
                Vector3::new(mn.x.min(min.x), mn.y.min(min.y), mn.z.min(min.z)),
                Vector3::new(mx.x.max(max.x), mx.y.max(max.y), mx.z.max(max.z)),
            ),
        });
    }

    /// `SettingsWriter::write`: an inside-out box first gains the zero point
    /// (the reference's `addBounds(Vector3::ZERO)`), then the header goes out
    /// through the typed write — `sizeof(Header)` = 24 prefix, then the AABB.
    pub fn write(&mut self, writer: &mut BinaryFormatWriter) -> bool {
        if self.is_inside_out() {
            let zero = Vector3::ZERO;
            self.add_bounds(zero, zero);
        }
        let Some(stream) = writer.append_section(SETTINGS_MAGIC, SETTINGS_VERSION) else {
            return false;
        };

        let (min, max) = self.bounds.unwrap_or_default();
        let mut header = Vec::with_capacity(SETTINGS_HEADER_SIZE);
        write_vector3(&mut header, &min);
        write_vector3(&mut header, &max);
        write_typed(stream, &header);
        true
    }
}

/// `SpaceConverter` (`space_converter.cpp`): the `.settings` → `space.bin`
/// converter of a compiled space.
pub struct SpaceConverter {
    resource: Rc<BwResource>,
}

/// The item types the reference routes to an ignore handler
/// (`SpaceConverter::initializeWriters`).
const IGNORE_ITEMS: [&str; 6] = [
    "transform",
    "boundingBox",
    "worldNavmesh",
    "waypointGenerationTime",
    "ambientLight",
    "directionLight",
];

/// `spaceDirFromSettings`: the settings path minus its `/space.settings`
/// tail.
fn space_dir_from_settings(source_file: &str) -> String {
    source_file
        .strip_suffix("/space.settings")
        .unwrap_or_default()
        .to_string()
}

impl SpaceConverter {
    pub fn new(resource: Rc<BwResource>) -> Self {
        Self { resource }
    }

    /// The port's chunk walk: every `*.chunk` entry of the space directory
    /// whose `.cdata` twin also loads. Both sections must open, exactly like
    /// the reference's `ctx.pChunkDS && ctx.pCDataDS` guard.
    fn process_chunks(
        &self,
        space_dir: &str,
        strings: &mut StringTableWriter,
        assets: &mut AssetListWriter,
    ) {
        let Some(entries) = self.resource.file_system().read_directory(space_dir) else {
            return;
        };
        for entry in entries {
            let Some(chunk_id) = entry.strip_suffix(".chunk") else {
                continue;
            };
            let chunk_file = format!("{space_dir}/{chunk_id}.chunk");
            let cdata_file = format!("{space_dir}/{chunk_id}.cdata");
            let Some(chunk) = self.resource.open_section(&chunk_file, false) else {
                continue;
            };
            if self.resource.open_section(&cdata_file, false).is_none() {
                continue;
            }
            Self::convert_chunk(&chunk, strings, assets);
        }
    }

    /// `ChunkConverter::convertChunk`: every child is offered to the item
    /// dispatch by its tag.
    fn convert_chunk(
        chunk: &DataSection,
        strings: &mut StringTableWriter,
        assets: &mut AssetListWriter,
    ) {
        for index in 0..chunk.count_children() {
            let Some(item) = chunk.open_child(index) else {
                continue;
            };
            let item_type = item.section_name();
            if IGNORE_ITEMS.contains(&item_type) {
                continue;
            }
            if item_type == "model" || item_type == "shell" {
                Self::convert_model(item, strings, assets);
            }
            // Everything else is the reference's "Unsupported chunk item
            // type" warning: skipped, not a failure.
        }
    }

    /// `StaticSceneModelWriter::addFromChunkModel`: the chunk model's
    /// `resource` strings become MODEL assets. A model with no resources is
    /// the reference's ERROR_MSG; the port skips the item.
    fn convert_model(
        item: &DataSection,
        strings: &mut StringTableWriter,
        assets: &mut AssetListWriter,
    ) {
        let resources: Vec<String> = (0..item.count_children())
            .filter_map(|index| item.open_child(index))
            .filter(|child| child.section_name() == "resource")
            .map(|child| child.as_string().to_string())
            .collect();
        for resource in resources {
            assets.add_asset(ASSET_TYPE_MODEL, &resource, strings);
        }
    }

    /// `SpaceConverter::outputWriterData`: the string table and asset list
    /// first, then every space writer in list order.
    fn output_writer_data(
        strings: &StringTableWriter,
        assets: &AssetListWriter,
        settings: &mut SettingsWriter,
        writer: &mut BinaryFormatWriter,
    ) -> bool {
        strings.write(writer) && assets.write(writer) && settings.write(writer)
    }
}

impl Converter for SpaceConverter {
    /// `SpaceConverter::createDependencies`: the space directory's
    /// `*.chunk`/`*.cdata` files as one critical secondary directory
    /// dependency.
    fn create_dependencies(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        let mut space_dir = space_dir_from_settings(source_file);
        compiler.resolve_relative_path(&mut space_dir);
        dependencies.add_secondary_directory_dependency(
            space_dir,
            r".*\.(chunk|cdata)$",
            true,
            true,
            true,
        );
        true
    }

    /// `SpaceConverter::convert`: open the settings, walk the chunks, write
    /// `space.bin` into the space directory and report it as the output.
    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        let mut space_dir = space_dir_from_settings(source_file);
        compiler.resolve_relative_path(&mut space_dir);

        if self.resource.open_section(source_file, false).is_none() {
            compiler.set_error();
            return false;
        }

        let mut strings = StringTableWriter::new();
        let mut assets = AssetListWriter::new();
        let mut settings = SettingsWriter::new();

        self.process_chunks(&space_dir, &mut strings, &mut assets);

        let mut writer = BinaryFormatWriter::new();
        if !Self::output_writer_data(&strings, &assets, &mut settings, &mut writer) {
            compiler.set_error();
            return false;
        }
        let bytes = writer.build();

        // The reference writes the raw space path, then resolves the same
        // string for its report (`compiler.resolveOutputPath` mutates it).
        let mut binary_filename = format!("{space_dir}/space.bin");
        if !self
            .resource
            .file_system()
            .write_file(&binary_filename, &bytes)
        {
            compiler.set_error();
            return false;
        }
        compiler.resolve_output_path(&mut binary_filename);
        output_files.push(binary_filename);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use crate::dependency::Dependency;
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A stub compiler recording the flags raised, with the resolving
    /// methods kept as no-ops (the paths already resolve).
    struct StubCompiler {
        errored: RefCell<bool>,
    }

    impl StubCompiler {
        fn new() -> Self {
            Self {
                errored: RefCell::new(false),
            }
        }
    }

    impl Compiler for StubCompiler {
        fn get_resource_paths(&self) -> &[String] {
            &[]
        }
        fn get_hash(&self, _dependency: &Dependency) -> u64 {
            0
        }
        fn get_file_hash(&self, _file_name: &str, _force_update: bool) -> u64 {
            0
        }
        fn get_directory_hash(
            &self,
            _directory: &str,
            _pattern: &str,
            _regex: bool,
            _recursive: bool,
        ) -> u64 {
            0
        }
        fn check_file_hash_up_to_date(&self, _file_name: &str) -> bool {
            true
        }
        fn ensure_up_to_date(
            &self,
            _dependency: &Dependency,
        ) -> (bool, Option<crate::conversion_task::TaskHandle>) {
            (true, None)
        }
        fn get_source_file(&self, _file: &str) -> Option<String> {
            None
        }
        fn queue_task(&self, _task: crate::conversion_task::TaskHandle) {}
        fn get_next_task(&self) -> Option<crate::conversion_task::TaskHandle> {
            None
        }
        fn has_tasks(&self) -> bool {
            false
        }
        fn set_error(&self) {
            *self.errored.borrow_mut() = true;
        }
        fn set_warning(&self) {}
        fn reset_error_flags(&self) {
            *self.errored.borrow_mut() = false;
        }
        fn has_error(&self) -> bool {
            *self.errored.borrow()
        }
        fn has_warning(&self) -> bool {
            false
        }
        fn resolve_relative_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_source_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_intermediate_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_output_path(&self, _path: &mut String) -> bool {
            true
        }
        fn cache_read(&self, _file_name: &str, _hash: u64) -> bool {
            false
        }
        fn cache_write(&self, _file_name: &str, _hash: u64) -> bool {
            false
        }
        fn should_iterate_file(&self, _file: &str) -> bool {
            true
        }
        fn should_iterate_directory(&self, _directory: &str) -> bool {
            true
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spaceconv-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A small space: two chunk pairs (the second repeating one model, which
    /// must dedupe), an ignore-list item and an unsupported one per chunk.
    fn make_space(tag: &str) -> (PathBuf, Rc<BwResource>) {
        let root = temp_root(tag);

        write_file(
            &root.join("maps/cavern/space.settings"),
            "<space>\n\t<chunkSize>200</chunkSize>\n</space>\n",
        );
        write_file(
            &root.join("maps/cavern/o0010c0002.chunk"),
            "<chunk>\n\t<transform>\n\t\t<translation>200 0 400</translation>\n\t</transform>\n\t<boundingBox>\n\t\t<min>-200 0 -400</min>\n\t\t<max>200 100 -200</max>\n\t</boundingBox>\n\t<model>\n\t\t<resource>models/box.model</resource>\n\t\t<resource>models/spikes.model</resource>\n\t</model>\n\t<shell>\n\t\t<resource>models/shell.model</resource>\n\t</shell>\n\t<entity>\n\t\t<name>unsupported</name>\n\t</entity>\n</chunk>\n",
        );
        write_file(&root.join("maps/cavern/o0010c0002.cdata"), "<cdata/>\n");
        write_file(
            &root.join("maps/cavern/o0011c0002.chunk"),
            "<chunk>\n\t<model>\n\t\t<resource>models/box.model</resource>\n\t</model>\n</chunk>\n",
        );
        write_file(&root.join("maps/cavern/o0011c0002.cdata"), "<cdata/>\n");
        // A chunk without its .cdata twin: skipped like the reference's
        // failed chunk load.
        write_file(
            &root.join("maps/cavern/o0012c0002.chunk"),
            "<chunk>\n\t<model>\n\t\t<resource>models/orphan.model</resource>\n\t</model>\n</chunk>\n",
        );

        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        (root, Rc::new(resource))
    }

    /// The round trip: the space directory's models land in `space.bin` as
    /// string table entries and MODEL assets, and the container reads back
    /// through the compiled-space readers.
    #[test]
    fn space_convert_round_trips_through_compiled_space_readers() {
        let (_root, resource) = make_space("roundtrip");
        let converter = SpaceConverter::new(Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut intermediates = Vec::new();
        let mut outputs = Vec::new();
        assert!(converter.convert(
            "maps/cavern/space.settings",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
        assert!(!compiler.has_error());
        assert_eq!(outputs, vec!["maps/cavern/space.bin".to_string()]);

        let bytes = resource
            .file_system()
            .read_file("maps/cavern/space.bin")
            .expect("space.bin written");
        let format =
            compiled_space::binary_format::BinaryFormat::open("maps/cavern/space.bin", bytes)
                .expect("BWTB container parses");

        // BWST, BWAL and BWCS in writer order.
        assert_eq!(format.num_sections(), 3);

        let strings =
            compiled_space::string_table::StringTable::read(&format).expect("string table reads");
        assert_eq!(strings.size(), 3);
        assert_eq!(strings.entry_str(0), Some("models/box.model"));
        assert_eq!(strings.entry_str(1), Some("models/spikes.model"));
        assert_eq!(strings.entry_str(2), Some("models/shell.model"));

        let assets =
            compiled_space::asset_list::AssetList::read(&format).expect("asset list reads");
        assert_eq!(assets.size(), 3);
        for idx in 0..3 {
            assert_eq!(
                assets.asset_type(idx),
                Some(compiled_space::asset_list::ASSET_TYPE_MODEL)
            );
            assert_eq!(assets.string_table_index(idx), Some(idx as u32));
        }

        let settings =
            compiled_space::settings::CompiledSpaceSettings::read(&format).expect("settings read");
        let bounds = settings.bounding_box();
        assert_eq!(bounds.min, Vector3::ZERO);
        assert_eq!(bounds.max, Vector3::ZERO);
    }

    /// `createDependencies` registers the space directory as one critical
    /// secondary directory dependency with the chunk/cdata pattern.
    #[test]
    fn create_dependencies_registers_the_space_directory() {
        let (_root, resource) = make_space("deps");
        let converter = SpaceConverter::new(resource);
        let compiler = StubCompiler::new();
        let compiler_dyn: &dyn Compiler = &compiler;
        let mut dependencies: DependencyList<&dyn Compiler> = DependencyList::new(compiler_dyn);
        assert!(converter.create_dependencies(
            "maps/cavern/space.settings",
            &compiler,
            &mut dependencies
        ));

        let inputs = dependencies.secondary_inputs();
        assert_eq!(inputs.len(), 1);
        let (dependency, hash) = &inputs[0];
        assert_eq!(*hash, 0);
        assert!(dependency.is_critical());
        let Dependency::Directory(dir) = dependency else {
            panic!("expected a directory dependency");
        };
        assert_eq!(dir.directory(), "maps/cavern");
        assert_eq!(dir.pattern(), r".*\.(chunk|cdata)$");
        assert!(dir.is_regex());
        assert!(dir.is_recursive());
    }

    /// A source that is not a settings file, or one that fails to open,
    /// errors the compiler and converts nothing.
    #[test]
    fn missing_settings_fails_the_convert() {
        let (_root, resource) = make_space("missing");
        let converter = SpaceConverter::new(resource);
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        assert!(!converter.convert(
            "maps/cavern/nope.settings",
            &compiler,
            &mut Vec::new(),
            &mut outputs
        ));
        assert!(compiler.has_error());
        assert!(outputs.is_empty());
    }

    /// The writer faces byte checks: a duplicate section magic is refused, a
    /// full table too, and the settings writer unions generated bounds.
    #[test]
    fn writer_faces_refuse_duplicates_and_union_bounds() {
        let mut writer = BinaryFormatWriter::new();
        assert!(writer.append_section(*b"BWST", 1).is_some());
        assert!(writer.append_section(*b"BWST", 1).is_none());
        for n in writer.num_sections()..MAX_SECTIONS {
            let magic = [b'X', b'W', b'0', b'a' + n as u8];
            assert!(writer.append_section(magic, 1).is_some());
        }
        assert!(writer.append_section(*b"BWZL", 1).is_none());

        let mut settings = SettingsWriter::new();
        assert!(settings.is_inside_out());
        settings.add_bounds(Vector3::new(-1.0, -2.0, -3.0), Vector3::new(1.0, 2.0, 3.0));
        // An inside-out box is dropped, not folded in.
        settings.add_bounds(Vector3::new(5.0, 5.0, 5.0), Vector3::new(-5.0, -5.0, -5.0));
        settings.add_bounds(Vector3::new(-10.0, 0.0, 0.0), Vector3::new(0.0, 10.0, 10.0));
        assert!(!settings.is_inside_out());
        let (min, max) = settings.bounds().expect("bounds unioned");
        assert_eq!(min, Vector3::new(-10.0, -2.0, -3.0));
        assert_eq!(max, Vector3::new(1.0, 10.0, 10.0));
    }

    /// The container layout the readers demand: header, section headers and
    /// payloads concatenated.
    #[test]
    fn binary_format_writer_builds_the_reference_layout() {
        let mut writer = BinaryFormatWriter::new();
        let stream = writer.append_section(*b"BWXX", 7).unwrap();
        stream.extend_from_slice(&[1, 2, 3, 4, 5]);
        let bytes = writer.build();

        assert_eq!(bytes.len(), HEADER_SIZE + SECTION_HEADER_SIZE + 5);
        assert_eq!(&bytes[0..4], b"BWTB");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            FORMAT_VERSION
        );
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            (HEADER_SIZE + SECTION_HEADER_SIZE) as u32
        );
        // fileSize_ is never set.
        assert_eq!(u64::from_le_bytes(bytes[12..20].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 1);

        // The section header sits at 24 and its payload at 48.
        assert_eq!(&bytes[24..28], b"BWXX");
        assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), 7);
        assert_eq!(
            u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            (HEADER_SIZE + SECTION_HEADER_SIZE) as u64
        );
        assert_eq!(u64::from_le_bytes(bytes[40..48].try_into().unwrap()), 5);
        assert_eq!(&bytes[48..53], &[1, 2, 3, 4, 5]);
    }
}
