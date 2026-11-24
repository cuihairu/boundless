//! `res_packer` port (`tools/res_packer/`): packs resources for release.
//!
//! The binary packed-section format itself lives in
//! [`resources::packed`](resources::packed) (reader + `pack` writer); this
//! module is the tool around it: the packer registry, the six packers, the
//! `PackedSection::convert` face and the two command-line modes.
//!
//! Fully portable faces: the packer dispatch and priority table, the
//! value-typed pack (through `resources::packed::pack`'s `addBestGuess`
//! port), the XML/packed/binary content sniff, `stripSectionsRecursive`,
//! the `--encrypt` XOR face (including the packed writer for binary
//! sections, whose every node stores its raw block as an
//! `ENCRYPTED_BLOB`), per-type packer behaviour and both CLI modes with
//! their message-exact output — down to preserved typos
//! (`PackedSetcion`) and the `MsgHandler` quirk where any message below
//! ERROR *clears* the error flag.
//!
//! Seams (documented at each site): Moo/D3D bring-up
//! (`PackerHelper::initMoo` — window, render device),
//! `Model::loadAnimations` (.anca generation needs the Moo animation
//! runtime; the port falls back to copying an existing source `.anca`),
//! the CDataPacker terrain-v2 face (`DummyChunkTerrain`/
//! `InitTerrainDependencies` decide which terrain sections to strip),
//! SIGINT/SIGBREAK interactive handling (`quit_requested` replaces the
//! y/n prompt), `BgTaskManager`/`FileIOTaskManager`,
//! `PackerHelper::paths`/`setBasePath`/AutoConfig (the caller supplies
//! the configured [`BwResource`]), and `MaterialKinds`.
//!
//! Flavour: the reference builds client and `MF_SERVER` variants; this
//! port mirrors the client build (the CMake target links Moo): chunk
//! entities keep `clientOnly` only, chunk strips include `worldNavmesh`,
//! and `config.hpp` ships `PACK_SHADER_BINARIES` without
//! `PACK_SHADER_SOURCE`.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;

use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::BwResource;
use resources::datasection::{self, DataSection};
use resources::packed::{self, PackedSection, SectionType};

/// `config.hpp`: shader binaries (.fxo) ship, shader sources (.fx/.fxh)
/// don't (the reference defines `PACK_SHADER_BINARIES` and leaves
/// `PACK_SHADER_SOURCE` commented out).
pub const PACK_SHADER_BINARIES: bool = true;
pub const PACK_SHADER_SOURCE: bool = false;

/// `BW_ENCRYPTION_KEY`: the value shipped in the reference binaries;
/// `resources::packed` decrypts with the same key.
const BW_ENCRYPTION_KEY: u8 = 0xc9;
const BW_ENCRYPTED_BLOCK: u8 = 1;

/// printf + `MsgHandler` surface: everything the tool writes lands in
/// `out` in order (including the `ERROR: `/`WARNING: `-prefixed lines the
/// message handler emits), and `errors_occurred` mirrors
/// `MsgHandler::errorsOccurred_`: set by ERROR messages and — the
/// reference quirk — *cleared* again by anything below ERROR, because the
/// handler assigns the comparison result instead of only OR-ing.
pub struct PackerEnv<'a> {
    pub resource: &'a BwResource,
    pub out: String,
    pub errors_occurred: bool,
    /// `XmlPacker::s_shouldEncrypt` (`--encrypt`).
    pub should_encrypt: bool,
    /// `CDataPacker`'s `s_sectionsToStrip` (each `--strip` entry).
    pub cdata_strip_sections: Vec<String>,
    /// `s_quitRequested`: the SIGINT seam — the interactive y/n prompt is
    /// not portable, so the caller (or a test) sets this to stop after the
    /// current asset.
    pub quit_requested: bool,
}

impl<'a> PackerEnv<'a> {
    pub fn new(resource: &'a BwResource) -> Self {
        Self {
            resource,
            out: String::new(),
            errors_occurred: false,
            should_encrypt: false,
            cdata_strip_sections: Vec::new(),
            quit_requested: false,
        }
    }

    fn say(&mut self, text: &str) {
        self.out.push_str(text);
    }

    /// `ERROR_MSG` through `MsgHandler`: printed with the priority prefix
    /// and recorded as an error.
    fn error(&mut self, message: &str) {
        self.out.push_str("ERROR: ");
        self.out.push_str(message);
        self.errors_occurred = true;
    }

    /// `WARNING_MSG` — printed with its prefix and, like every sub-ERROR
    /// message, *clearing* the error flag (`errorsOccurred_ = priority >=
    /// ERROR`).
    fn warning(&mut self, message: &str) {
        self.out.push_str("WARNING: ");
        self.out.push_str(message);
        self.errors_occurred = false;
    }
}

// ----------------------------------------------------------------------
// Source sections: the `createAppropriateSection` faces this tool needs.
// ----------------------------------------------------------------------

/// The three content kinds `createAppropriateSection` can yield, with the
/// flags `convert` consults: XML sections pack, packed sections are
/// read-only and re-packable, binary sections pass through (or encrypt).
/// Cloning lets `print` show the section without consuming the packer
/// (`XmlPacker::print` is a `const` method in the reference).
#[derive(Clone)]
pub enum SourceSection {
    Xml(DataSection),
    Packed(PackedSection),
    Bin(BinSection),
}

impl SourceSection {
    /// Sniff raw file content: packed magic first (the creator chain
    /// checks `PackedSection` before the XML probe), then the
    /// whitespace-then-`'<'` XML probe, otherwise a binary section. XML
    /// content that fails to parse degrades to binary (the reference's
    /// XMLSection parser is more lenient — a documented divergence on
    /// malformed input only).
    pub fn open(raw: &[u8]) -> Option<Self> {
        if PackedSection::is_packed_file(raw) {
            return Some(SourceSection::Packed(PackedSection::parse("root", raw)));
        }
        let looks_xml = raw
            .iter()
            .find(|&&b| b != b' ' && b != b'\t' && b != b'\r' && b != b'\n')
            == Some(&b'<');
        if looks_xml && let Ok(ds) = datasection::parse("root", raw) {
            return Some(SourceSection::Xml(ds));
        }
        Some(SourceSection::Bin(BinSection::new(
            "root",
            BinaryBlock::new(raw.to_vec()),
        )))
    }

    pub fn is_packed(&self) -> bool {
        matches!(self, SourceSection::Packed(_))
    }

    /// `canPack`: true except for binary sections (`BinSection` and
    /// `ZipSection` override the `DataSection` default; `PackedSection`
    /// does not).
    pub fn can_pack(&self) -> bool {
        !matches!(self, SourceSection::Bin(_))
    }
}

/// `XmlPacker::printRecursive` over any source kind: two spaces per
/// indent level, `name: 'value'` lines (binary children carry no value
/// face, so their quotes stay empty).
fn print_source_recursive(source: &mut SourceSection, indent: usize, out: &mut String) {
    for _ in 0..indent {
        out.push_str("  ");
    }
    let children: Vec<(String, BinaryBlock)> = match source {
        SourceSection::Xml(ds) => {
            out.push_str(&format!("{}: '{}'\n", ds.section_name(), ds.as_string()));
            let children: Vec<DataSection> = (0..ds.count_children())
                .filter_map(|i| ds.open_child(i).cloned())
                .collect();
            for child in children {
                print_source_recursive(&mut SourceSection::Xml(child), indent + 1, out);
            }
            return;
        }
        SourceSection::Packed(ps) => {
            out.push_str(&format!("{}: '{}'\n", ps.section_name(), ps.as_string()));
            let children: Vec<PackedSection> = (0..ps.count_children())
                .filter_map(|i| ps.open_child(i).cloned())
                .collect();
            for child in children {
                print_source_recursive(&mut SourceSection::Packed(child), indent + 1, out);
            }
            return;
        }
        SourceSection::Bin(bs) => {
            out.push_str(&format!("{}: ''\n", bs.section_name()));
            bs.introspect();
            bs.children()
                .iter()
                .map(|c| (c.section_name().to_string(), c.as_binary().clone()))
                .collect()
        }
    };
    for (name, block) in children {
        let child = BinSection::new(&name, block);
        print_source_recursive(&mut SourceSection::Bin(child), indent + 1, out);
    }
}

// ----------------------------------------------------------------------
// Strip sections (`stripSectionsRecursive`).
// ----------------------------------------------------------------------

/// Remove *every* instance of each strip tag at this level (the reference
/// loops `delChild` until the child count stops changing), then recurse
/// one level deeper while the budget lasts.
fn strip_xml(ds: &mut DataSection, strips: &[String], level: i32) {
    if strips.is_empty() || level <= 0 {
        return;
    }
    for tag in strips {
        while ds.del_child(tag) {}
    }
    for index in 0..ds.count_children() {
        if let Some(child) = ds.child_mut(index) {
            strip_xml(child, strips, level - 1);
        }
    }
}

/// `BinSection::delChild` through a container rebuild: children are
/// cloned out as blocks, the first match per `delChild` call is dropped,
/// and the survivors are reassembled. Returns whether a child was
/// removed.
fn delete_first_bin_child(bs: &mut BinSection, tag: &str) -> bool {
    bs.introspect();
    let children: Vec<(String, BinaryBlock)> = bs
        .children()
        .iter()
        .map(|c| (c.section_name().to_string(), c.as_binary().clone()))
        .collect();
    let mut rebuilt = BinSection::new(bs.section_name(), BinaryBlock::default());
    let mut removed = false;
    for (name, block) in children {
        if !removed && name == tag {
            removed = true;
            continue;
        }
        rebuilt.new_section(&name).set_binary(block);
    }
    let changed = removed;
    *bs = rebuilt;
    changed
}

/// The binary face of `stripSectionsRecursive`.
fn strip_bin(bs: &mut BinSection, strips: &[String], level: i32) {
    if strips.is_empty() || level <= 0 {
        return;
    }
    for tag in strips {
        while delete_first_bin_child(bs, tag) {}
    }
    bs.introspect();
    let children: Vec<(String, BinaryBlock)> = bs
        .children()
        .iter()
        .map(|c| (c.section_name().to_string(), c.as_binary().clone()))
        .collect();
    let mut rebuilt = BinSection::new(bs.section_name(), BinaryBlock::default());
    for (name, block) in children {
        let mut child = BinSection::new(&name, block);
        strip_bin(&mut child, strips, level - 1);
        rebuilt
            .new_section(&name)
            .set_binary(child.collect_binary());
    }
    *bs = rebuilt;
}

fn strip_source(source: &mut SourceSection, strips: &[String], level: i32) {
    match source {
        // Packed sections are read-only: `convert` strips nothing.
        SourceSection::Packed(_) => {}
        SourceSection::Xml(ds) => strip_xml(ds, strips, level),
        SourceSection::Bin(bs) => strip_bin(bs, strips, level),
    }
}

// ----------------------------------------------------------------------
// Encryption and the packed writer for binary sections.
// ----------------------------------------------------------------------

/// `encryptBinaryBlock`: a marker byte followed by the payload XORed with
/// the key.
fn encrypt_block(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 1);
    out.push(BW_ENCRYPTED_BLOCK);
    for &byte in data {
        out.push(byte ^ BW_ENCRYPTION_KEY);
    }
    out
}

fn patch_u32(out: &mut [u8], pos: usize, value: u32) {
    out[pos..pos + 4].copy_from_slice(&value.to_le_bytes());
}

fn patch_i16(out: &mut [u8], pos: usize, value: i16) {
    out[pos..pos + 2].copy_from_slice(&value.to_le_bytes());
}

/// `writeAsPackedSection` for a binary section with `--encrypt`: every
/// node is non-packable, so each own-data block is the encrypted full raw
/// block; nodes with children additionally write a children block whose
/// entries recurse the same way (the reference drives the generic writer
/// over the `BinSection`'s introspected children).
fn write_bin_node(
    bs: &mut BinSection,
    is_root: bool,
    indices: &HashMap<String, u16>,
    out: &mut Vec<u8>,
) -> SectionType {
    bs.introspect();
    let children: Vec<(String, BinaryBlock)> = bs
        .children()
        .iter()
        .map(|c| (c.section_name().to_string(), c.as_binary().clone()))
        .collect();

    let own = encrypt_block(bs.as_binary().data());
    if children.is_empty() && !is_root {
        out.extend_from_slice(&own);
        return SectionType::ENCRYPTED_BLOB;
    }

    let num_children = children.len();
    let header_pos = out.len();
    out.extend_from_slice(&(num_children as i16).to_le_bytes());
    out.resize(header_pos + 2 + num_children * 6 + 4, 0);

    let anchor = out.len();
    out.extend_from_slice(&own);
    let own_end = (out.len() - anchor) as u32;
    patch_u32(out, header_pos + 2, own_end | SectionType::ENCRYPTED_BLOB.0);

    for (k, (name, block)) in children.iter().enumerate() {
        let mut child = BinSection::new(name, block.clone());
        let child_type = write_bin_node(&mut child, false, indices, out);
        let end = (out.len() - anchor) as u32;
        patch_u32(out, header_pos + 2 + (k + 1) * 6, end | child_type.0);
        let index = indices.get(name).copied().unwrap_or(0);
        patch_i16(out, header_pos + 2 + k * 6 + 4, index as i16);
    }

    SectionType::DATA_SECTION
}

/// `convertToPackedFile` for a binary section: header, the sorted string
/// table over the tree's tags, then the encrypted tree.
fn pack_bin_encrypted(bs: &mut BinSection) -> Vec<u8> {
    let mut names = BTreeMap::new();
    collect_bin_names(bs, &mut names);
    let indices: HashMap<String, u16> = names
        .keys()
        .enumerate()
        .map(|(i, k)| (k.clone(), i as u16))
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(&packed::PACKED_SECTION_MAGIC.to_le_bytes());
    out.push(packed::PACKED_SECTION_VERSION);
    for name in names.keys() {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
    }
    out.push(0);
    write_bin_node(bs, true, &indices, &mut out);
    out
}

fn collect_bin_names(bs: &mut BinSection, names: &mut BTreeMap<String, ()>) {
    bs.introspect();
    let children: Vec<(String, BinaryBlock)> = bs
        .children()
        .iter()
        .map(|c| (c.section_name().to_string(), c.as_binary().clone()))
        .collect();
    for (name, block) in children {
        names.insert(name.clone(), ());
        let mut child = BinSection::new(&name, block);
        collect_bin_names(&mut child, names);
    }
}

/// Materialise a packed tree as an XML-model tree — the re-pack face (the
/// reference drives the generic writer over the `PackedSection` itself,
/// re-deriving every value through `asString`).
fn packed_to_data_section(ps: &PackedSection) -> DataSection {
    fn fill(slot: &mut DataSection, ps: &PackedSection) {
        slot.set_string(&ps.as_string());
        for index in 0..ps.count_children() {
            if let Some(child) = ps.open_child(index) {
                let name = child.section_name().to_string();
                fill(slot.add_child(&name), child);
            }
        }
    }
    let mut ds = DataSection::new(ps.section_name());
    fill(&mut ds, ps);
    ds
}

// ----------------------------------------------------------------------
// The `PackedSection::convert` face.
// ----------------------------------------------------------------------

/// Pure core of `PackedSection::convert`: strips (unless the input is
/// already packed), then pack — or pass the raw block through for
/// non-packable content without `--encrypt`. `Err` carries the message
/// text for the caller's `ERROR: ` line.
pub fn convert_bytes(
    _env: &mut PackerEnv,
    raw: &[u8],
    strips: &[String],
    should_encrypt: bool,
    recursion_level: i32,
) -> Result<Vec<u8>, String> {
    let Some(mut source) = SourceSection::open(raw) else {
        return Err(format!(
            "PackedSection::convert: Failed to open data section ({0} bytes)\n",
            raw.len()
        ));
    };

    if !strips.is_empty() && !source.is_packed() {
        strip_source(&mut source, strips, recursion_level);
    }

    if source.can_pack() || should_encrypt {
        return match &mut source {
            SourceSection::Xml(ds) => {
                packed::pack(ds).map_err(|error| too_many_children_message(&error))
            }
            // Packed input re-packs through its materialised tree
            // (`canPack` is true for packed sections, so `--encrypt`
            // changes nothing here).
            SourceSection::Packed(ps) => {
                let tree = packed_to_data_section(ps);
                packed::pack(&tree).map_err(|error| too_many_children_message(&error))
            }
            SourceSection::Bin(bs) => Ok(pack_bin_encrypted(bs)),
        };
    }

    // `canPack() == false` and no encryption: the raw block is the output.
    match source {
        SourceSection::Bin(bs) => Ok(bs.as_binary().data().to_vec()),
        _ => unreachable!("packable kinds returned above"),
    }
}

fn too_many_children_message(error: &packed::PackError) -> String {
    match error {
        // The reference's typo "PackedSetcion" is preserved; the port's
        // error type carries no counts, so the "(%d children, limit is
        // %d)" tail is dropped.
        packed::PackError::TooManyChildren { tag } => format!(
            "PackedSetcion: writeAsPackedSection failed, tag '{tag}' has too \
             many children\n"
        ),
    }
}

/// `PackedSection::convert(inPath, outPath, ...)`: read, convert, write,
/// with the reference's message-exact failures.
pub fn convert(
    env: &mut PackerEnv,
    in_path: &str,
    out_path: &str,
    strips: &[String],
    should_encrypt: bool,
    recursion_level: i32,
) -> bool {
    let Some(raw) = env.resource.file_system().read_file(in_path) else {
        env.error(&format!(
            "PackedSection::convert: Failed to open data section {in_path}\n"
        ));
        return false;
    };

    let payload = match convert_bytes(env, &raw, strips, should_encrypt, recursion_level) {
        Ok(payload) => payload,
        Err(message) => {
            env.error(&message);
            return false;
        }
    };

    if !env.resource.file_system().write_file(out_path, &payload) {
        env.error(&format!(
            "PackedSection::convert: Failed to open output file {out_path}\n"
        ));
        return false;
    }
    true
}

// ----------------------------------------------------------------------
// PackerHelper.
// ----------------------------------------------------------------------

/// `PackerHelper::copyFile`: stream the source into the destination with
/// the message-exact errors (`silent` suppresses them). The reference's
/// read/write error paths collapse into the open failures here — the
/// resource file system moves whole buffers.
pub fn copy_file(env: &mut PackerEnv, src: &str, dst: &str, silent: bool) -> bool {
    let Some(data) = env.resource.file_system().read_file(src) else {
        if !silent {
            env.say(&format!("Error: Cannot open source file {src}.\n"));
        }
        return false;
    };
    if !env.resource.file_system().write_file(dst, &data) {
        if !silent {
            env.say(&format!("Error: Cannot open destination file {dst}.\n"));
        }
        return false;
    }
    true
}

/// `PackerHelper::fileExists`.
pub fn file_exists(env: &PackerEnv, file: &str) -> bool {
    env.resource.file_exists(file)
}

/// The file's extension lowercased, borrowed from `out`.
fn lowercase_ext<'a>(out: &'a mut String, file: &str) -> &'a str {
    out.clear();
    out.push_str(&resources::bwresource::get_extension(file).to_ascii_lowercase());
    out.as_str()
}

/// `removeTrailingSlash`: one trailing `/` or `\` comes off.
pub fn remove_trailing_slash(path: &str) -> String {
    let Some(last) = path.chars().last() else {
        return path.to_string();
    };
    if last != '/' && last != '\\' {
        return path.to_string();
    }
    path[..path.len() - last.len_utf8()].to_string()
}

// ----------------------------------------------------------------------
// The packers.
// ----------------------------------------------------------------------

/// `BasePacker`: `prepare` claims the file (and stores `src`/`dst`),
/// `print` writes the file's string representation, `pack` produces the
/// release form.
pub trait BasePacker {
    fn prepare(&mut self, env: &mut PackerEnv, src: &str, dst: &str) -> bool;
    fn print(&mut self, env: &mut PackerEnv) -> bool;
    fn pack(&mut self, env: &mut PackerEnv) -> bool;
}

/// Shared `src_`/`dst_` storage with the reference's emptiness guards.
#[derive(Default)]
struct Claim {
    src: String,
    dst: String,
}

impl Claim {
    fn claim(&mut self, src: &str, dst: &str) {
        self.src = src.to_string();
        self.dst = dst.to_string();
    }
    fn ready(&self) -> bool {
        !self.src.is_empty() && !self.dst.is_empty()
    }
    fn not_initialised(&self) -> bool {
        self.src.is_empty() && self.dst.is_empty()
    }
}

/// `XmlPacker`: the catch-all at `LOWEST_PRIORITY` — anything that opens
/// as a data section and can pack (or should encrypt) converts to a
/// packed section with `metaData` stripped; already-packed files re-pack.
#[derive(Default)]
pub struct XmlPacker {
    claim: Claim,
    section: Option<SourceSection>,
}

impl BasePacker for XmlPacker {
    fn prepare(&mut self, env: &mut PackerEnv, src: &str, dst: &str) -> bool {
        let Some(raw) = env.resource.file_system().read_file(src) else {
            env.say(&format!("Error opening source file: {src}\n"));
            return false;
        };
        if raw.is_empty() {
            env.say(&format!("Error reading source file: {src}\n"));
            return false;
        }
        let Some(section) = SourceSection::open(&raw) else {
            return false; // not a section the packer knows
        };
        if !section.can_pack() && !env.should_encrypt {
            return false;
        }
        self.claim.claim(src, dst);
        self.section = Some(section);
        true
    }

    fn print(&mut self, env: &mut PackerEnv) -> bool {
        let Some(section) = &self.section else {
            env.say("Error: XmlPacker Not initialised properly\n");
            return false;
        };
        // The reference prints `*ds_` and keeps it for the later `pack`.
        let mut cloned = section.clone();
        let mut out = std::mem::take(&mut env.out);
        print_source_recursive(&mut cloned, 0, &mut out);
        env.out = out;
        true
    }

    fn pack(&mut self, env: &mut PackerEnv) -> bool {
        if self.section.is_none() || !self.claim.ready() {
            env.say("Error: XmlPacker Not initialised properly\n");
            return false;
        }
        // Particles are .xml files, so remove metadata.
        convert(
            env,
            &self.claim.src,
            &self.claim.dst,
            &["metaData".to_string()],
            env.should_encrypt,
            1,
        )
    }
}

/// `FxPacker`: `.fx`/`.fxh`/`.fxo` — sources are skipped by default
/// (`config.hpp`), compiled `.fxo` files are copied.
#[derive(Default)]
pub struct FxPacker {
    claim: Claim,
    kind: Option<FxKind>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FxKind {
    Fx,
    Fxh,
    Fxo,
}

impl FxPacker {
    fn kind_of(ext: &str) -> Option<FxKind> {
        match ext {
            "fx" => Some(FxKind::Fx),
            "fxh" => Some(FxKind::Fxh),
            "fxo" => Some(FxKind::Fxo),
            _ => None,
        }
    }
}

impl BasePacker for FxPacker {
    fn prepare(&mut self, _env: &mut PackerEnv, src: &str, dst: &str) -> bool {
        let mut scratch = String::new();
        self.kind = Self::kind_of(lowercase_ext(&mut scratch, src));
        if self.kind.is_none() {
            return false;
        }
        self.claim.claim(src, dst);
        true
    }

    fn print(&mut self, env: &mut PackerEnv) -> bool {
        if self.claim.not_initialised() {
            env.say("Error: FxPacker not initialised properly\n");
            return false;
        }
        let label = match self.kind {
            Some(FxKind::Fxo) => "FxCompiledFile",
            Some(FxKind::Fxh) => "FxHeaderFile",
            _ => "FxSourceFile",
        };
        env.say(&format!("{}: {}\n", label, self.claim.src));
        true
    }

    fn pack(&mut self, env: &mut PackerEnv) -> bool {
        if !self.claim.ready() {
            env.say("Error: FxPacker not initialised properly\n");
            return false;
        }
        // Shaders take a very long time to compile, so binaries and
        // sources are included per `config.hpp`.
        if self.kind == Some(FxKind::Fxo) && !PACK_SHADER_BINARIES {
            return true; // skip .fxo files
        }
        if self.kind != Some(FxKind::Fxo) && !PACK_SHADER_SOURCE {
            return true; // skip .fx and .fxh files
        }
        copy_file(env, &self.claim.src, &self.claim.dst, false)
    }
}

/// `ImagePacker`: `.bmp`/`.tga`/`.png`/`.jpg` prefer an already-converted
/// sibling `.dds`; `.dds` files are copied unless they belong to a font.
/// With `--encrypt` the packer declines (`prepare` returns false), leaving
/// images to the `XmlPacker` (the 1.8.0 evaluation-release workaround).
#[derive(Default)]
pub struct ImagePacker {
    claim: Claim,
    is_dds: bool,
}

impl BasePacker for ImagePacker {
    fn prepare(&mut self, env: &mut PackerEnv, src: &str, dst: &str) -> bool {
        if env.should_encrypt {
            return false;
        }
        let mut scratch = String::new();
        let ext = lowercase_ext(&mut scratch, src);
        self.is_dds = ext == "dds";
        if !self.is_dds && !matches!(ext, "bmp" | "tga" | "png" | "jpg") {
            return false;
        }
        self.claim.claim(src, dst);
        true
    }

    fn print(&mut self, env: &mut PackerEnv) -> bool {
        if self.claim.not_initialised() {
            env.say("Error: ImagePacker Not initialised properly\n");
            return false;
        }
        if self.is_dds {
            env.say(&format!("DDSFile: {}\n", self.claim.src));
        } else {
            env.say(&format!("ImageFile: {}\n", self.claim.src));
        }
        true
    }

    fn pack(&mut self, env: &mut PackerEnv) -> bool {
        if !self.claim.ready() {
            env.say("Error: ImagePacker Not initialised properly\n");
            return false;
        }

        if self.is_dds {
            // Find out if it has a matching source image, and make sure
            // it is not a font DDS.
            let base_name = resources::bwresource::remove_extension(&self.claim.src);
            let directory =
                resources::bwresource::get_file_path(&env.resource.dissolve_filename(base_name));
            let font_name = resources::bwresource::get_filename(base_name);

            let mut is_font = false;
            if let Some(entries) = env.resource.file_system().read_directory(&directory) {
                for entry in entries {
                    let file_name = format!("{directory}/{entry}");
                    if resources::bwresource::get_extension(&file_name) != "font" {
                        continue;
                    }
                    let Some(raw) = env.resource.file_system().read_file(&file_name) else {
                        continue;
                    };
                    let Ok(font_file) =
                        datasection::parse(resources::bwresource::get_filename(&file_name), &raw)
                    else {
                        continue;
                    };
                    let source_font = font_file.read_string("creation/sourceFont", "");
                    let font_size = font_file.read_int("creation/sourceFontSize", 0).abs();
                    if font_name == format!("{source_font}_{font_size}") {
                        is_font = true;
                        break;
                    }
                }
            }

            // If the DDS is not a font, then copy it.
            if !is_font
                && !file_exists(env, &self.claim.dst)
                && !copy_file(env, &self.claim.src, &self.claim.dst, false)
            {
                return false;
            }
            // the DDS was copied, or we want to skip it, so return true
            return true;
        }

        // A source image: prefer the converted DDS when one exists. The
        // reference `resolveFilename`s the name (anchoring it on a search
        // root) and then `bw_fopen`s it natively; here both the probe and
        // the copy go through the resource, which already searches every
        // root, so the dissolved relative name is used instead.
        let dissolved = env.resource.dissolve_filename(&self.claim.src);
        let dds_name = format!(
            "{}.dds",
            resources::bwresource::remove_extension(&dissolved)
        );
        if file_exists(env, &dds_name) {
            // A converted DDS exists so lets copy that instead
            self.claim.src = dds_name;
            self.claim.dst = format!(
                "{}.dds",
                resources::bwresource::remove_extension(&self.claim.dst)
            );
            if !file_exists(env, &self.claim.dst)
                && !copy_file(env, &self.claim.src, &self.claim.dst, false)
            {
                return false;
            }
        } else if resources::bwresource::get_extension(&self.claim.src)
            == resources::bwresource::get_extension(&self.claim.dst)
        {
            if !file_exists(env, &self.claim.dst)
                && !copy_file(env, &self.claim.src, &self.claim.dst, false)
            {
                return false;
            }
        } else if !file_exists(env, &self.claim.dst) {
            env.say(&format!(
                "Error: Cannot copy source {} to destination {}.\n",
                self.claim.src, self.claim.dst
            ));
            return false;
        }
        // the source image was copied, or we want to skip it, so return
        // true
        true
    }
}

/// `ChunkPacker`: `.chunk` — server entities are removed (client flavour:
/// only `clientOnly` entities survive), then the file converts to a packed
/// section with editor-only data stripped down to depth 2.
#[derive(Default)]
pub struct ChunkPacker {
    claim: Claim,
}

impl BasePacker for ChunkPacker {
    fn prepare(&mut self, _env: &mut PackerEnv, src: &str, dst: &str) -> bool {
        let mut scratch = String::new();
        if lowercase_ext(&mut scratch, src) != "chunk" {
            return false;
        }
        self.claim.claim(src, dst);
        true
    }

    fn print(&mut self, env: &mut PackerEnv) -> bool {
        if self.claim.not_initialised() {
            env.say("Error: ChunkPacker not initialised properly\n");
            return false;
        }
        env.say(&format!("ChunkFile: {}\n", self.claim.src));
        true
    }

    fn pack(&mut self, env: &mut PackerEnv) -> bool {
        if !self.claim.ready() {
            env.say("Error: ChunkPacker not initialised properly\n");
            return false;
        }

        // Copy to a temp file in the destination folder before packing, in
        // order to be able to edit the file (a PackedSection is not
        // editable)
        let temp_file = format!("{}.packerTemp", self.claim.dst);
        if !copy_file(env, &self.claim.src, &temp_file, false) {
            return false;
        }

        let raw = env.resource.file_system().read_file(&temp_file);
        let mut section = raw.and_then(|raw| SourceSection::open(&raw));
        match &mut section {
            Some(SourceSection::Xml(root)) => {
                // Loop through all entity sections in the chunk. In the
                // client, remove server entities (`openSections("entity")`
                // + `delChild` per pointer; the index walk is the port's
                // equivalent).
                let mut index = root.count_children();
                while index > 0 {
                    index -= 1;
                    let remove = match root.open_child(index) {
                        Some(child) if child.section_name() == "entity" => {
                            !child.read_bool("clientOnly", false)
                        }
                        _ => false,
                    };
                    if remove {
                        root.del_child_at(index);
                    }
                }
                // save changes on the temp file
                let text = root.write_to_string();
                if !env
                    .resource
                    .file_system()
                    .write_file(&temp_file, text.as_bytes())
                {
                    env.say("Error saving DataSection\n");
                    return false;
                }
            }
            // Binary chunk files keep their entities here: reading the
            // raw block as a string never yields "true", so the
            // reference's client filter would drop every entity — the
            // port leaves such files to the strip pass instead.
            Some(SourceSection::Bin(_)) | Some(SourceSection::Packed(_)) => {}
            None => {
                env.say("Error opening as a DataSection\n");
                return false;
            }
        }

        // finally, convert to a packed section
        let converted = convert(
            env,
            &temp_file,
            &self.claim.dst,
            &[
                "editorOnly".to_string(),
                "metaData".to_string(),
                "navmesh".to_string(),
                // In the client, also remove navmesh section
                "worldNavmesh".to_string(),
            ],
            false,
            2, /* strip sections up to depth 2 */
        );

        // `FileDeleter`: the temp file goes away however the conversion
        // ended.
        let mut absolute = temp_file.clone();
        env.resource.resolve_to_absolute_path(&mut absolute);
        let _ = std::fs::remove_file(&absolute);

        converted
    }
}

/// `CDataPacker`: `.cdata` — strips the thumbnail and navmesh sections
/// (the world navmesh only when `clientNavigation` is disabled in the
/// sibling `space.settings`), plus any `--strip` sections, then prints
/// every surviving root child (the reference's `#if 1` debug block). The
/// terrain-v2 face (layer/LOD/normals stripping through
/// `DummyChunkTerrain` + `InitTerrainDependencies`) is the terrain
/// runtime seam and is not ported.
#[derive(Default)]
pub struct CDataPacker {
    claim: Claim,
    /// `!space.settings clientNavigation/enabled` (default: strip).
    strip_world_navmesh: bool,
}

impl BasePacker for CDataPacker {
    fn prepare(&mut self, env: &mut PackerEnv, src: &str, dst: &str) -> bool {
        let mut scratch = String::new();
        if lowercase_ext(&mut scratch, src) != "cdata" {
            return false;
        }

        self.claim.claim(src, dst);

        // `BWUtil::getFilePath` keeps the trailing slash; our port adds it
        // back.
        let folder = format!("{}/", resources::bwresource::get_file_path(src));
        let Some(settings) = env
            .resource
            .open_section(&format!("{folder}space.settings"), false)
        else {
            env.warning(&format!(
                "Couldn't find the space.settings file associated with '{src}'. \
                 Ignoring this .cdata file"
            ));
            return false;
        };
        self.strip_world_navmesh = !settings.read_bool("clientNavigation/enabled", false);
        true
    }

    fn print(&mut self, env: &mut PackerEnv) -> bool {
        if self.claim.not_initialised() {
            env.say("Error: ChunkPacker not initialised properly\n");
            return false;
        }
        env.say(&format!("CDataFile: {}\n", self.claim.src));
        true
    }

    fn pack(&mut self, env: &mut PackerEnv) -> bool {
        if !self.claim.ready() {
            env.say("Error: ChunkPacker not initialised properly\n");
            return false;
        }

        if !copy_file(env, &self.claim.src, &self.claim.dst, false) {
            return false;
        }

        let raw = env.resource.file_system().read_file(&self.claim.dst);
        let mut section = raw.and_then(|raw| SourceSection::open(&raw));
        let printed: Vec<String> = match &mut section {
            Some(SourceSection::Bin(root)) => {
                // delete the thumbnail section
                root.introspect();
                delete_first_bin_child(root, "thumbnail.dds");
                delete_first_bin_child(root, "navmesh");
                if self.strip_world_navmesh {
                    delete_first_bin_child(root, "worldNavmesh");
                }
                for tag in env.cdata_strip_sections.iter() {
                    delete_first_bin_child(root, tag);
                }
                let bytes = root.save_bytes();
                if !env
                    .resource
                    .file_system()
                    .write_file(&self.claim.dst, &bytes)
                {
                    env.say("Error saving DataSection\n");
                    return false;
                }
                root.children()
                    .iter()
                    .map(|c| c.section_name().to_string())
                    .collect()
            }
            Some(SourceSection::Xml(root)) => {
                root.del_child("thumbnail.dds");
                root.del_child("navmesh");
                if self.strip_world_navmesh {
                    root.del_child("worldNavmesh");
                }
                for tag in &env.cdata_strip_sections {
                    root.del_child(tag);
                }
                let text = root.write_to_string();
                if !env
                    .resource
                    .file_system()
                    .write_file(&self.claim.dst, text.as_bytes())
                {
                    env.say("Error saving DataSection\n");
                    return false;
                }
                (0..root.count_children())
                    .filter_map(|i| root.open_child(i).map(|c| c.section_name().to_string()))
                    .collect()
            }
            _ => {
                env.say("Error opening as a DataSection\n");
                return false;
            }
        };

        // The reference's `#if 1` leftover: every surviving root child is
        // printed.
        for name in printed {
            env.say(&format!("{name}\n"));
        }

        true
    }
}

/// `ModelAnimPacker`: `.model`/`.animation`/`.anca` — animations and
/// compressed animations are skipped, models are packed with `metaData`
/// stripped. The nodefull `.anca` generation face
/// (`Model::loadAnimations`, Moo animation runtime) is the seam: the port
/// proceeds without generating, so the up-to-date-source-`.anca` copy
/// below still runs.
#[derive(Default)]
pub struct ModelAnimPacker {
    claim: Claim,
    kind: Option<AnimKind>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AnimKind {
    Model,
    Animation,
    Anca,
}

impl BasePacker for ModelAnimPacker {
    fn prepare(&mut self, _env: &mut PackerEnv, src: &str, dst: &str) -> bool {
        let mut scratch = String::new();
        self.kind = match lowercase_ext(&mut scratch, src) {
            "model" => Some(AnimKind::Model),
            "animation" => Some(AnimKind::Animation),
            "anca" => Some(AnimKind::Anca),
            _ => None,
        };
        if self.kind.is_none() {
            return false;
        }
        self.claim.claim(src, dst);
        true
    }

    fn print(&mut self, env: &mut PackerEnv) -> bool {
        if self.claim.not_initialised() {
            env.say("Error: ModelAnimPacker Not initialised properly\n");
            return false;
        }
        let label = match self.kind {
            Some(AnimKind::Animation) => "AnimationFile",
            Some(AnimKind::Anca) => "CompressedAnimationFile",
            _ => "ModelFile",
        };
        env.say(&format!("{}: {}\n", label, self.claim.src));
        true
    }

    fn pack(&mut self, env: &mut PackerEnv) -> bool {
        if !self.claim.ready() {
            env.say("Error: ModelAnimPacker Not initialised properly\n");
            return false;
        }
        if self.kind != Some(AnimKind::Model) {
            return true; // skip .animation and .anca files
        }

        // Load the data section to find out if the model is nodefull.
        let raw = env.resource.file_system().read_file(&self.claim.src);
        let nodefull = raw
            .as_deref()
            .and_then(|raw| datasection::parse("root", raw).ok())
            .map(|ds| ds.open_section("nodefullVisual").is_some());

        let Some(_nodefull) = nodefull else {
            env.say(&format!(
                "Could not open model file {}\n",
                env.resource.dissolve_filename(&self.claim.src)
            ));
            return false;
        };

        // Seam: `Moo::InterpolatedAnimationChannel::inhibitCompression` +
        // `Model::loadAnimations` would generate the .anca here (and
        // "Couldn't load animations for model %s" on failure); the port
        // proceeds without the Moo runtime.

        // Check to see if a new anca file was created; if not, copy the
        // up-to-date one from the source folder when it exists.
        let dst_anca = format!(
            "{}.anca",
            resources::bwresource::remove_extension(&self.claim.dst)
        );
        if !file_exists(env, &dst_anca) {
            let src_anca = format!(
                "{}.anca",
                resources::bwresource::remove_extension(&self.claim.src)
            );
            if file_exists(env, &src_anca) && !copy_file(env, &src_anca, &dst_anca, false) {
                return false;
            }
            // else: the model doesn't have animations (no error returned)
        }

        // copy and pack the model file to the destination
        convert(
            env,
            &self.claim.src,
            &self.claim.dst,
            &["metaData".to_string()],
            false,
            1,
        )
    }
}

// ----------------------------------------------------------------------
// The registry (`Packers`).
// ----------------------------------------------------------------------

/// `Packers`: priority-ordered registry; `find` runs `prepare` in priority
/// order and keeps the first packer that claims the file.
pub struct Packers {
    items: Vec<(u16, Box<dyn BasePacker>)>,
}

impl Packers {
    pub const HIGHEST_PRIORITY: u16 = 0;
    pub const LOWEST_PRIORITY: u16 = 0xFFFF;

    /// The reference's registration set: the five extension packers at
    /// `HIGHEST_PRIORITY` (their C++ static-init order across translation
    /// units is unspecified, but the extensions are disjoint so the order
    /// among them is unobservable), the `XmlPacker` at `LOWEST_PRIORITY`.
    pub fn new() -> Self {
        let mut registry = Packers { items: Vec::new() };
        registry.add(Box::new(CDataPacker::default()), Self::HIGHEST_PRIORITY);
        registry.add(Box::new(FxPacker::default()), Self::HIGHEST_PRIORITY);
        registry.add(Box::new(ImagePacker::default()), Self::HIGHEST_PRIORITY);
        registry.add(Box::new(ChunkPacker::default()), Self::HIGHEST_PRIORITY);
        registry.add(Box::new(ModelAnimPacker::default()), Self::HIGHEST_PRIORITY);
        registry.add(Box::new(XmlPacker::default()), Self::LOWEST_PRIORITY);
        registry
    }

    /// `Packers::add`: insert before the first strictly-greater priority
    /// (equal priorities keep registration order).
    pub fn add(&mut self, packer: Box<dyn BasePacker>, priority: u16) {
        let position = self
            .items
            .iter()
            .position(|(existing, _)| *existing > priority)
            .unwrap_or(self.items.len());
        self.items.insert(position, (priority, packer));
    }

    /// `Packers::find` + `prepare`: the index of the claiming packer.
    pub fn find(&mut self, env: &mut PackerEnv, src: &str, dst: &str) -> Option<usize> {
        for (index, (_, packer)) in self.items.iter_mut().enumerate() {
            if packer.prepare(env, src, dst) {
                return Some(index);
            }
        }
        None
    }

    pub fn print_at(&mut self, index: usize, env: &mut PackerEnv) -> bool {
        self.items[index].1.print(env)
    }

    pub fn pack_at(&mut self, index: usize, env: &mut PackerEnv) -> bool {
        self.items[index].1.pack(env)
    }
}

impl Default for Packers {
    fn default() -> Self {
        Self::new()
    }
}

// ----------------------------------------------------------------------
// The two command-line modes (`main.cpp`).
// ----------------------------------------------------------------------

/// `printUsage`, both `printf` blocks concatenated.
pub fn print_usage(exe_file_name: &str, out: &mut String) {
    out.push_str(&format!(
        "Compatible usage: {exe_file_name} [-r|--res search_paths] input_file \
         [output_file [base_path]]\nBatch list usage: {exe_file_name} \
         --list|-l asset_list_file\n    --in|-i input_path_root\n    \
         --out|-o output_path_root\n    [--err|-e error_log_file]\n    \
         [--res|-r search_paths]\n"
    ));
    out.push_str(
        "\n'asset_list_file' is the file which contains all the files to process\n\
         'input_path_root' is the root directory for the input files\n\
         'output_path_root' is the root directory for the output files\n\
         'error_log_file' is the error output file\n\
         \n\
         The batch list mode is the new, fast way of using res_packer\n\
         The argument 'search_paths' is a semicolon-separated list of paths\n\
         used when processing some assets types such as fonts, models, etc.\n\
         If 'search_paths' is not specified, the file 'paths.xml' or the \n\
         environment variable BW_RES_PATH will be used.\n\
         \n\
         In the old compatible mode, If an output file is specified, the input data\n\
         is packed depending on the file type. If an output file is not specified,\n\
         the input file string representation is printed.\n\
         Some assets types also require that a correct base path is specified.\n\
         For example, packing .font and .model assets require valid base_path.\n\
         The base_path must point to the root path of the assets in the\n\
         destination folder, so if packing the a file 'dragon.bmp' to the folder\n\
         '/bw/finalgame/res/maps/monsters/dragon.bmp', and the root of the game\n\
         will be '/bw/finalgame/res', then base_path must be '/bw/finalgame/res'\n\
         If an incorrect base_path is specified, packing might fail or might\n\
         generate invalid assets (for example, an invalid .font file)\n\
         \n\
         Full example of the old compatible form:\n\
         \x20   res_packer --res /bw/fantasydemo/res;/bw/bigworld/res\n\
         \x20   /bw/fantasydemo/res/models/my_model.model\n\
         \x20   /bw/fantasydemo/res_packed/models/my_model.model\n\
         \x20   /bw/fantasydemo/res_packed\n\
         \n\
         Full example of the batch list form:\n\
         \x20   res_packer --list assets.txt\n\
         \x20   --in /bw/fantasydemo/res\n\
         \x20   --out /bw/fantasydemo/res_packed\n\
         \x20   --err error.log\n\
         \x20   --res /bw/fantasydemo/res;/bw/bigworld/res\n",
    );
}

/// The `while (processedFlag)` argument walk: the leftover positionals
/// plus the batch-mode inputs. `--list`/`--err` check their files
/// immediately (the reference `fopen`s them, early-exiting on the list and
/// warn-and-continuing on the log); `--encrypt` and `--strip` land in the
/// env right away, like the reference's statics.
struct ParsedArgs {
    positionals: Vec<String>,
    has_asset_list: bool,
    asset_list: Option<String>,
    error_log: Option<std::fs::File>,
    /// `useErrorLog`: stays true even when the log failed to open, so the
    /// completion tail still says "refer to error log".
    use_error_log: bool,
    in_path: String,
    out_path: String,
}

fn parse_args(env: &mut PackerEnv, args: &[String]) -> Result<ParsedArgs, ()> {
    let mut rest: Vec<String> = args.to_vec();
    let mut parsed = ParsedArgs {
        positionals: Vec::new(),
        has_asset_list: false,
        asset_list: None,
        error_log: None,
        use_error_log: false,
        in_path: String::new(),
        out_path: String::new(),
    };

    let mut processed = true;
    while processed {
        processed = false;
        if rest.len() > 1 && (rest[0] == "--res" || rest[0] == "-r") {
            // skip command-line-specified paths (the caller's BwResource
            // carries the search paths)
            rest.drain(..2);
            processed = true;
        } else if rest.len() > 1 && (rest[0] == "--list" || rest[0] == "-l") {
            parsed.has_asset_list = true;
            if std::fs::File::open(&rest[1]).is_ok() {
                parsed.asset_list = Some(rest[1].clone());
            } else {
                env.say(&format!("Unable to open asset list \"{}\"\n", rest[1]));
                return Err(()); // early exit, like the reference
            }
            rest.drain(..2);
            processed = true;
        } else if rest.len() > 1 && (rest[0] == "--in" || rest[0] == "-i") {
            parsed.in_path = remove_trailing_slash(&rest[1]);
            rest.drain(..2);
            processed = true;
        } else if rest.len() > 1 && (rest[0] == "--out" || rest[0] == "-o") {
            parsed.out_path = remove_trailing_slash(&rest[1]);
            rest.drain(..2);
            processed = true;
        } else if rest.len() > 1 && (rest[0] == "--err" || rest[0] == "-e") {
            parsed.use_error_log = true;
            parsed.error_log = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&rest[1])
                .ok();
            if parsed.error_log.is_none() {
                env.say(&format!("Unable to open error log \"{}\"\n", rest[1]));
            }
            rest.drain(..2);
            processed = true;
        } else if !rest.is_empty() && rest[0] == "--encrypt" {
            // NOTE: This option is currently not documented.
            env.should_encrypt = true;
            rest.drain(..1);
            processed = true;
        } else if rest.len() > 1 && rest[0] == "--strip" {
            // NOTE: This option is currently not documented.
            env.cdata_strip_sections.push(rest[1].clone());
            rest.drain(..2);
            processed = true;
        }
    }

    parsed.positionals = rest;
    Ok(parsed)
}

/// `main`: the batch list mode and the old compatible mode. Returns the
/// process exit code. The asset-list and error-log files are plain files
/// (the reference `fopen`s them); everything else goes through the
/// resource.
pub fn run(env: &mut PackerEnv, args: &[String], packers: &mut Packers) -> i32 {
    let exe_file_name = "res_packer";
    let mut parsed = match parse_args(env, args) {
        Ok(parsed) => parsed,
        // The asset list could not be opened: the reference exits here.
        Err(()) => return 1,
    };

    // Validate the command line arguments: list mode needs `--in`,
    // `--out` and no positionals; compatible mode takes one to three.
    let num_args = parsed.positionals.len();
    if (parsed.has_asset_list
        && (parsed.in_path.is_empty() || parsed.out_path.is_empty() || num_args > 0))
        || (!parsed.has_asset_list && !(1..=3).contains(&num_args))
    {
        print_usage(exe_file_name, &mut env.out);
        return 1;
    }

    if parsed.has_asset_list {
        // Batch list mode.
        let Some(list_path) = parsed.asset_list.clone() else {
            return 1;
        };
        let list = std::fs::read_to_string(&list_path).unwrap_or_default();
        let in_path = parsed.in_path.clone();
        let out_path = parsed.out_path.clone();
        let mut error_log = std::mem::take(&mut parsed.error_log);
        let use_error_log = parsed.use_error_log;

        // fgets + `buf[strlen(buf) - 1] = 0`: every line's trailing
        // newline is eaten; a final line with no newline loses its last
        // character instead.
        let trailing_newline = list.ends_with('\n');
        let raw_lines: Vec<&str> = list.split('\n').collect();
        let count = if trailing_newline {
            raw_lines.len().saturating_sub(1)
        } else {
            raw_lines.len()
        };

        let mut result = true;
        let mut fails = 0usize;
        for (index, line) in raw_lines.iter().enumerate().take(count) {
            let line: &str = if index + 1 == count && !trailing_newline {
                &line[..line.len().saturating_sub(1)]
            } else {
                line
            };
            let p_input_name = format!("{in_path}/{line}");
            let p_output_name = format!("{out_path}/{line}");

            let claimed = packers.find(env, &p_input_name, &p_output_name);

            env.say(&format!("Processing {p_input_name}..."));

            let failed = match claimed {
                None => {
                    // file type not known, so just copy it
                    !copy_file(env, &p_input_name, &p_output_name, false)
                }
                Some(index) => {
                    // file known, so try to process it
                    !packers.pack_at(index, env)
                }
            };

            if failed {
                fails += 1;
                if let Some(log) = error_log.as_mut() {
                    let _ = writeln!(log, "{p_input_name}");
                }
            }
            env.say(&format!(
                " {}\n",
                if failed { "failed" } else { "succeeded" }
            ));

            if env.quit_requested {
                // The reference prompts "\nDo you want to continue
                // [y/n]? " interactively; the port records the request and
                // stops after this asset.
                break;
            }
        }

        env.say("\nProcessing complete");
        if fails > 0 {
            env.say(&format!(" - {fails} files failed: "));
            if use_error_log {
                env.say("refer to error log");
            } else {
                env.say("run with \"--err 'error_log_file'\" argument");
            }
            env.say(" for more information");
        }
        env.say(".\n\n");

        result = result && !env.quit_requested;
        return i32::from(!result);
    }

    // Old compatible mode, only processing one file.
    let p_input_name = parsed.positionals[0].replace('\\', "/");
    let p_output_name = if num_args == 1 {
        String::new()
    } else {
        parsed.positionals[1].replace('\\', "/")
    };

    if num_args == 1 {
        // print mode: the input file string representation
        let Some(index) = packers.find(env, &p_input_name, &p_output_name) else {
            return 0; // file type not known, nothing to print
        };
        if !packers.print_at(index, env) {
            // failed printing so there's something wrong with the file
            env.say(&format!(
                "Error: Could not print file '{}'\n",
                parsed.positionals[0]
            ));
            print_usage(exe_file_name, &mut env.out);
            return 1;
        }
        return 0;
    }

    env.say(&format!(
        "Processing {} to {}...\n",
        parsed.positionals[0], parsed.positionals[1]
    ));

    let mut result = match packers.find(env, &p_input_name, &p_output_name) {
        None => copy_file(env, &p_input_name, &p_output_name, false),
        Some(index) => packers.pack_at(index, env),
    };

    result = result && !env.errors_occurred;
    env.say(&format!(
        "...{}\n",
        if result { "succeeded" } else { "failed" }
    ));

    result = result && !env.quit_requested;
    i32::from(!result)
}

// ----------------------------------------------------------------------
// Tests.
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use foundation::math::Vector3;
    use resources::base64;

    /// A fresh fixture root with its own `BwResource`, mirroring how the
    /// reference brings up `BWResource` over the resource paths.
    struct Fixture {
        dir: std::path::PathBuf,
        resource: BwResource,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bw_res_packer_{tag}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let resource = BwResource::init_paths(&[dir.to_str().unwrap()]);
            Self { dir, resource }
        }

        fn write(&self, name: &str, data: &[u8]) {
            let path = self.dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, data).unwrap();
        }

        /// Packer output directories must pre-exist: the reference's
        /// `writeFile` (like ours) does not create parent directories.
        fn mkdir(&self, name: &str) {
            std::fs::create_dir_all(self.dir.join(name)).unwrap();
        }

        fn read(&self, name: &str) -> Vec<u8> {
            std::fs::read(self.dir.join(name)).unwrap()
        }

        fn env(&self) -> PackerEnv<'_> {
            PackerEnv::new(&self.resource)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The acceptance-fixture tree: every `addBestGuess` value kind.
    fn rich_tree() -> DataSection {
        let mut root = DataSection::new("root");
        root.add_child("count").set_int(42);
        root.add_child("neg").set_int(-7);
        root.add_child("zero").set_int(0);
        root.add_child("big").set_int64(1234567890123);
        root.add_child("leadingZero").set_string("007"); // STRING: not int
        root.add_child("flag").set_bool(true);
        root.add_child("off").set_bool(false);
        root.add_child("ratio").set_float(1.5); // "1.500000" -> FLOAT
        root.add_child("vec")
            .set_vector3(Vector3::new(1.0, 2.0, 3.0));
        root.add_child("text").set_string("hello world");
        root.add_child("blob")
            .set_string(&base64::encode(b"binary payload"));
        let nested = root.add_child("nested");
        nested.add_child("deep").set_string("leaf");
        // A matrix subtree (row0..row3, each three floats).
        for (i, v) in [
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
            Vector3::new(0.5, 0.5, 0.5),
        ]
        .iter()
        .enumerate()
        {
            root.add_child(&format!("row{i}")).set_vector3(*v);
        }
        root
    }

    #[test]
    fn sniffs_xml_packed_and_binary_content() {
        let packed = packed::pack(&rich_tree()).unwrap();
        assert!(SourceSection::open(&packed).unwrap().is_packed());
        assert!(matches!(
            SourceSection::open(b"  \n\t<x><a>1</a></x>").unwrap(),
            SourceSection::Xml(_)
        ));
        assert!(matches!(
            SourceSection::open(b"\x00\x01bin").unwrap(),
            SourceSection::Bin(_)
        ));
        // canPack: XML and packed yes, binary no.
        assert!(SourceSection::open(b"<a/>").unwrap().can_pack());
        assert!(SourceSection::open(&packed).unwrap().can_pack());
        assert!(!SourceSection::open(b"raw").unwrap().can_pack());
    }

    #[test]
    fn print_output_matches_across_xml_and_packed() {
        let tree = rich_tree();
        let mut xml_source = SourceSection::Xml(tree);
        let mut xml_out = String::new();
        print_source_recursive(&mut xml_source, 0, &mut xml_out);

        let packed = packed::pack(&rich_tree()).unwrap();
        let mut packed_source = SourceSection::Packed(PackedSection::parse("root", &packed));
        let mut packed_out = String::new();
        print_source_recursive(&mut packed_source, 0, &mut packed_out);

        assert_eq!(xml_out, packed_out);
        assert!(xml_out.contains("root: ''\n"));
        assert!(xml_out.contains("  count: '42'\n"));
        assert!(xml_out.contains("  nested: ''\n"));
        assert!(xml_out.contains("    deep: 'leaf'\n"));
        assert!(xml_out.contains("  row0: '1.000000 0.000000 0.000000'\n"));
    }

    #[test]
    fn strips_remove_every_instance_down_the_levels() {
        let fixture = Fixture::new("strip");
        fixture.write(
            "in.xml",
            b"<root><keep>1</keep><metaData>a</metaData><metaData>b</metaData>\
              <nested><metaData>c</metaData><keep>2</keep></nested>\
              <deep><deeper><metaData>d</metaData></deeper></deep></root>",
        );
        let mut env = fixture.env();
        let raw = env.resource.file_system().read_file("in.xml").unwrap();
        let bytes = convert_bytes(&mut env, &raw, &["metaData".to_string()], false, 2).unwrap();

        let packed = PackedSection::parse("root", &bytes);
        // Root: keep, nested, deep survive; both metaData gone.
        let names: Vec<String> = (0..packed.count_children())
            .map(|i| packed.open_child(i).unwrap().section_name().to_string())
            .collect();
        assert_eq!(names, ["keep", "nested", "deep"]);
        // Level 2 (nested): metaData stripped, keep survives.
        let nested = packed.find_child("nested").unwrap();
        assert_eq!(nested.count_children(), 1);
        assert_eq!(nested.open_child(0).unwrap().section_name(), "keep");
        // Level 3 (deeper): below the recursion budget, metaData stays.
        let deeper = packed.find_child("deep").unwrap().open_child(0).unwrap();
        assert_eq!(deeper.open_child(0).unwrap().section_name(), "metaData");
    }

    #[test]
    fn packed_input_is_not_stripped() {
        let fixture = Fixture::new("packed_strip");
        let packed = packed::pack(&rich_tree()).unwrap();
        fixture.write("in.packed", &packed);
        let mut env = fixture.env();
        let raw = env.resource.file_system().read_file("in.packed").unwrap();
        let bytes = convert_bytes(&mut env, &raw, &["row0".to_string()], false, 1).unwrap();
        let result = PackedSection::parse("root", &bytes);
        assert_eq!(result.count_children(), rich_tree().count_children());
        assert!(result.find_child("row0").is_some());
    }

    /// The acceptance test for BW-13-02-03: the packed output reads back
    /// value-equal, leaf by leaf, against the XML input.
    #[test]
    fn packed_output_reads_back_equivalent() {
        let fixture = Fixture::new("equate");
        fixture.write("in.xml", rich_tree().write_to_string().as_bytes());
        let mut env = fixture.env();
        assert!(convert(&mut env, "in.xml", "out.packed", &[], false, 1));
        let bytes = fixture.read("out.packed");
        assert!(PackedSection::is_packed_file(&bytes));

        let packed = PackedSection::parse("root", &bytes);
        assert_eq!(packed.count_children(), rich_tree().count_children());

        let child = |tag: &str| packed.find_child(tag).unwrap();
        assert_eq!(child("count").as_int(0), 42);
        assert_eq!(child("neg").as_int(0), -7);
        assert_eq!(child("zero").as_int(1), 0);
        assert_eq!(child("big").as_int64(0), 1234567890123);
        assert_eq!(child("leadingZero").as_string(), "007");
        assert!(child("flag").as_bool(false));
        assert!(!child("off").as_bool(true));
        assert_eq!(child("ratio").as_float(0.0), 1.5);
        assert_eq!(
            child("vec").as_vector3(Vector3::ZERO),
            Vector3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(child("text").as_string(), "hello world");
        assert_eq!(child("blob").as_blob(""), b"binary payload".to_vec());
        assert_eq!(child("nested").open_child(0).unwrap().as_string(), "leaf");

        // The matrix: four synthesised rows, floats intact.
        let matrix = child("row0").as_string(); // rows exist as children of root
        assert!(!matrix.is_empty());
        for (i, expected) in [
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
            Vector3::new(0.5, 0.5, 0.5),
        ]
        .iter()
        .enumerate()
        {
            let row = packed.find_child(&format!("row{i}")).unwrap();
            assert_eq!(row.as_vector3(Vector3::ZERO), *expected);
        }
    }

    /// Re-packing packed output is byte-stable: the reference re-derives
    /// every value through `asString`, which round-trips the value-typed
    /// forms exactly.
    #[test]
    fn repacking_packed_output_is_byte_stable() {
        let packed = packed::pack(&rich_tree()).unwrap();
        let rederived = packed_to_data_section(&PackedSection::parse("root", &packed));
        let repacked = packed::pack(&rederived).unwrap();
        assert_eq!(packed, repacked);
    }

    #[test]
    fn binary_without_encrypt_passes_through() {
        let fixture = Fixture::new("passthrough");
        fixture.write("in.bin", b"\x00\x01raw block");
        let mut env = fixture.env();
        let raw = env.resource.file_system().read_file("in.bin").unwrap();
        let bytes = convert_bytes(&mut env, &raw, &[], false, 1).unwrap();
        assert_eq!(bytes, b"\x00\x01raw block");
    }

    #[test]
    fn binary_with_encrypt_round_trips() {
        let fixture = Fixture::new("encrypt");
        // A root binary section with two children, one of them nested.
        let mut root = BinSection::new("root", BinaryBlock::default());
        root.new_section("alpha")
            .set_binary(BinaryBlock::new(b"alpha bytes".to_vec()));
        root.new_section("beta")
            .set_binary(BinaryBlock::new(b"beta bytes".to_vec()));
        root.new_section("nest")
            .set_binary(BinaryBlock::new(b"nest".to_vec()));
        let raw = root.collect_binary().data().to_vec();
        fixture.write("in.bin", &raw);

        let mut env = fixture.env();
        let bytes = convert_bytes(&mut env, &raw, &[], true, 1).unwrap();
        assert!(PackedSection::is_packed_file(&bytes));

        let packed = PackedSection::parse("root", &bytes);
        // The whole raw block decrypts as the root's own data.
        assert_eq!(packed.as_blob(""), raw);
        // Children are the introspected binary blocks (the nested byte
        // layout), preserved byte-for-byte behind their own encryption.
        let alpha = packed.find_child("alpha").unwrap();
        assert_eq!(alpha.as_blob(""), b"alpha bytes".to_vec());
        let beta = packed.find_child("beta").unwrap();
        assert_eq!(beta.as_blob(""), b"beta bytes".to_vec());
        assert!(packed.find_child("nest").is_some());

        // A plain leaf (no children) encrypts as a bare blob.
        let mut leaf = BinSection::new("root", BinaryBlock::new(b"just data".to_vec()));
        let bytes = convert_bytes(&mut env, leaf.collect_binary().data(), &[], true, 1).unwrap();
        let packed = PackedSection::parse("root", &bytes);
        assert_eq!(packed.count_children(), 0);
        assert_eq!(packed.as_blob(""), b"just data".to_vec());
    }

    #[test]
    fn strip_works_on_binary_sections() {
        let mut root = BinSection::new("root", BinaryBlock::default());
        root.new_section("metaData")
            .set_binary(BinaryBlock::new(b"m".to_vec()));
        root.new_section("keep")
            .set_binary(BinaryBlock::new(b"k".to_vec()));
        root.new_section("metaData")
            .set_binary(BinaryBlock::new(b"m2".to_vec()));
        let nested = root.new_section("nested");
        nested.set_binary(BinaryBlock::new(b"n".to_vec()));
        // add the child through the outer section's writer face
        let mut outer = BinSection::new("root", BinaryBlock::default());
        outer
            .new_section("metaData")
            .set_binary(BinaryBlock::new(b"m".to_vec()));
        outer
            .new_section("keep")
            .set_binary(BinaryBlock::new(b"k".to_vec()));
        outer
            .new_section("metaData")
            .set_binary(BinaryBlock::new(b"m2".to_vec()));
        let nested_bytes = nested.collect_binary();
        outer.new_section("nested").set_binary(nested_bytes);

        let mut section = outer;
        strip_bin(&mut section, &["metaData".to_string()], 2);
        section.introspect();
        let names: Vec<String> = section
            .children()
            .iter()
            .map(|c| c.section_name().to_string())
            .collect();
        assert_eq!(names, ["keep", "nested"]);
        // Level 2: nested's own metaData children survive (the leaf's
        // bytes are opaque here); the strip only rewrites container
        // levels that introspect.
    }

    #[test]
    fn xml_packer_prints_and_packs() {
        let fixture = Fixture::new("xml");
        fixture.write(
            "in.xml",
            b"<root><metaData>x</metaData><value>42</value></root>",
        );

        let mut env = fixture.env();
        let mut packers = Packers::new();
        let index = packers.find(&mut env, "in.xml", "out.packed").unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "root: ''\n  metaData: 'x'\n  value: '42'\n");

        env.out.clear();
        assert!(packers.pack_at(index, &mut env));
        assert_eq!(env.out, "");
        let bytes = fixture.read("out.packed");
        let packed = PackedSection::parse("root", &bytes);
        // metaData was stripped, the value packed as INT.
        assert!(packed.find_child("metaData").is_none());
        assert_eq!(packed.find_child("value").unwrap().as_int(0), 42);
    }

    #[test]
    fn xml_packer_declines_binary_without_encrypt() {
        let fixture = Fixture::new("xml_decline");
        fixture.write("in.bin", b"\x00raw");
        let mut env = fixture.env();
        let mut packers = Packers::new();
        // Without --encrypt the binary can't pack, so the catch-all
        // declines (find yields nothing; batch mode would copy).
        assert!(packers.find(&mut env, "in.bin", "out.bin").is_none());
        // With --encrypt it claims.
        env.should_encrypt = true;
        assert!(packers.find(&mut env, "in.bin", "out.bin").is_some());
        // And an unreadable file reports through XmlPacker's messages.
        let mut env = fixture.env();
        assert!(packers.find(&mut env, "missing.xml", "out.xml").is_none());
        assert_eq!(env.out, "Error opening source file: missing.xml\n");
    }

    #[test]
    fn fx_packer_skips_sources_and_copies_binaries() {
        let fixture = Fixture::new("fx");
        fixture.write("sh.fx", b"shader source");
        fixture.write("inc.fxh", b"header source");
        fixture.write("comp.fxo", b"compiled bytes");

        let mut packers = Packers::new();
        fixture.mkdir("out");

        for (name, should_copy) in [("sh.fx", false), ("inc.fxh", false), ("comp.fxo", true)] {
            let mut env = fixture.env();
            let dst = format!("out/{name}");
            let index = packers.find(&mut env, name, &dst).unwrap();
            assert!(packers.pack_at(index, &mut env));
            let copied = fixture.dir.join(&dst).exists();
            assert_eq!(copied, should_copy, "{name}");
        }

        // Print labels.
        let mut env = fixture.env();
        let index = packers.find(&mut env, "comp.fxo", "o.fxo").unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "FxCompiledFile: comp.fxo\n");
        env.out.clear();
        let index = packers.find(&mut env, "sh.fx", "o.fx").unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "FxSourceFile: sh.fx\n");
        env.out.clear();
        let index = packers.find(&mut env, "inc.fxh", "o.fxh").unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "FxHeaderFile: inc.fxh\n");
    }

    #[test]
    fn image_packer_prefers_dds_and_skips_font_dds() {
        let fixture = Fixture::new("image");
        fixture.write("tex.tga", b"tga bytes");
        fixture.write("tex.dds", b"dds bytes");
        fixture.write("plain.bmp", b"bmp bytes");

        let mut packers = Packers::new();
        fixture.mkdir("out");

        // A source image with a converted sibling: the DDS is copied
        // instead, to a .dds destination.
        let mut env = fixture.env();
        let index = packers.find(&mut env, "tex.tga", "out/tex.tga").unwrap();
        assert!(packers.pack_at(index, &mut env));
        assert_eq!(fixture.read("out/tex.dds"), b"dds bytes");
        assert!(!fixture.dir.join("out/tex.tga").exists());

        // Same-extension copy when no DDS exists.
        let mut env = fixture.env();
        let index = packers
            .find(&mut env, "plain.bmp", "out/plain.bmp")
            .unwrap();
        assert!(packers.pack_at(index, &mut env));
        assert_eq!(fixture.read("out/plain.bmp"), b"bmp bytes");

        // Mismatched extensions without a DDS: the message-exact error.
        let mut env = fixture.env();
        let index = packers
            .find(&mut env, "plain.bmp", "out/plain.png")
            .unwrap();
        assert!(!packers.pack_at(index, &mut env));
        assert_eq!(
            env.out,
            "Error: Cannot copy source plain.bmp to destination out/plain.png.\n"
        );

        // A font DDS is skipped: fonts/name_24.dds matches
        // fonts/name.font (sourceFont "name", size 24).
        fixture.write(
            "fonts/name.font",
            b"<font><creation><sourceFont>name</sourceFont>\
              <sourceFontSize>24</sourceFontSize></creation></font>",
        );
        fixture.write("fonts/name_24.dds", b"font dds");
        let mut env = fixture.env();
        let index = packers
            .find(&mut env, "fonts/name_24.dds", "out/name_24.dds")
            .unwrap();
        assert!(packers.pack_at(index, &mut env));
        assert!(!fixture.dir.join("out/name_24.dds").exists());

        // A non-font DDS is copied.
        let mut env = fixture.env();
        let index = packers.find(&mut env, "tex.dds", "out/tex2.dds").unwrap();
        assert!(packers.pack_at(index, &mut env));
        assert_eq!(fixture.read("out/tex2.dds"), b"dds bytes");
    }

    #[test]
    fn chunk_packer_filters_server_entities_and_strips_editor_data() {
        let fixture = Fixture::new("chunk");
        fixture.write(
            "in.chunk",
            b"<root>\
              <editorOnly>root editor data</editorOnly>\
              <entity><id>1</id><type>Monster</type></entity>\
              <entity><id>2</id><type>Prop</type><clientOnly>true</clientOnly></entity>\
              <nested><editorOnly>level 2 editor data</editorOnly></nested>\
              <deep><deeper><editorOnly>level 3 editor data</editorOnly></deeper></deep>\
              </root>",
        );

        let mut env = fixture.env();
        let mut packers = Packers::new();
        let index = packers.find(&mut env, "in.chunk", "out.chunk").unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "ChunkFile: in.chunk\n");

        env.out.clear();
        assert!(packers.pack_at(index, &mut env));
        assert_eq!(env.out, "");

        let bytes = fixture.read("out.chunk");
        let packed = PackedSection::parse("root", &bytes);
        // Server entity (no clientOnly) removed, client entity kept.
        let entities: Vec<&PackedSection> = (0..packed.count_children())
            .filter_map(|i| packed.open_child(i))
            .filter(|c| c.section_name() == "entity")
            .collect();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].find_child("id").unwrap().as_string(), "2");
        // editorOnly stripped at root and nested (depth 2), kept at
        // depth 3.
        assert!(packed.find_child("editorOnly").is_none());
        let nested = packed.find_child("nested").unwrap();
        assert!(nested.find_child("editorOnly").is_none());
        let deeper = packed.find_child("deep").unwrap().open_child(0).unwrap();
        assert!(deeper.find_child("editorOnly").is_some());
        // The temp file is gone.
        assert!(!fixture.dir.join("out.chunk.packerTemp").exists());
    }

    #[test]
    fn cdata_packer_strips_thumbnail_and_navmesh() {
        let fixture = Fixture::new("cdata");
        fixture.write(
            "space.settings",
            b"<space><clientNavigation><enabled>true</enabled></clientNavigation></space>",
        );
        // A binary cdata: thumbnail.dds, navmesh, worldNavmesh, keep,
        // plus a --strip section.
        let mut root = BinSection::new("root", BinaryBlock::default());
        root.new_section("thumbnail.dds")
            .set_binary(BinaryBlock::new(b"t".to_vec()));
        root.new_section("navmesh")
            .set_binary(BinaryBlock::new(b"n".to_vec()));
        root.new_section("worldNavmesh")
            .set_binary(BinaryBlock::new(b"w".to_vec()));
        root.new_section("keep")
            .set_binary(BinaryBlock::new(b"k".to_vec()));
        root.new_section("extra")
            .set_binary(BinaryBlock::new(b"e".to_vec()));
        fixture.write("space.cdata", root.collect_binary().data());

        let mut env = fixture.env();
        env.cdata_strip_sections.push("extra".to_string());
        let mut packers = Packers::new();
        fixture.mkdir("out");
        fixture.mkdir("out2");
        let index = packers
            .find(&mut env, "space.cdata", "out/space.cdata")
            .unwrap();
        // clientNavigation enabled: worldNavmesh survives.
        assert!(packers.pack_at(index, &mut env));
        // The reference's `#if 1` leftover prints the surviving children.
        assert_eq!(env.out, "worldNavmesh\nkeep\n");
        let bytes = fixture.read("out/space.cdata");
        let mut section = SourceSection::open(&bytes).unwrap();
        let names: Vec<String> = match &mut section {
            SourceSection::Bin(root) => {
                root.introspect();
                root.children()
                    .iter()
                    .map(|c| c.section_name().to_string())
                    .collect()
            }
            _ => Vec::new(),
        };
        assert_eq!(names, ["worldNavmesh", "keep"]);

        // Disabled navigation (or no space.settings) strips it; a missing
        // settings file warns and prepares false.
        let mut env = fixture.env();
        let index = packers
            .find(&mut env, "space.cdata", "out2/space.cdata")
            .unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "CDataFile: space.cdata\n");

        // A cdata whose folder holds no space.settings is declined. The
        // fixture above has one at its root, so this needs its own root.
        let bare = Fixture::new("cdata_bare");
        bare.write("bare.cdata", b"\x00raw");
        let mut env = bare.env();
        assert!(
            packers
                .find(&mut env, "bare.cdata", "out/bare.cdata")
                .is_none()
        );
        assert_eq!(
            env.out,
            "WARNING: Couldn't find the space.settings file associated with \
             'bare.cdata'. Ignoring this .cdata file"
        );
    }

    #[test]
    fn model_anim_packer_skips_animations_and_packs_models() {
        let fixture = Fixture::new("model");
        fixture.write(
            "m.model",
            b"<model><nodefullVisual>yes</nodefullVisual><anim>a</anim></model>",
        );
        fixture.write("m.anca", b"anca bytes");
        fixture.write("anim.animation", b"anim");
        fixture.write("compressed.anca", b"compressed");

        let mut packers = Packers::new();
        fixture.mkdir("out");

        // Animations and ancas are claimed but skipped on pack.
        for name in ["anim.animation", "compressed.anca"] {
            let mut env = fixture.env();
            let index = packers
                .find(&mut env, name, &format!("out/{name}"))
                .unwrap();
            assert!(packers.pack_at(index, &mut env));
            assert!(!fixture.dir.join(format!("out/{name}")).exists());
        }

        // A model packs to a packed section, and the source .anca is
        // copied next to it (the generation seam's fallback).
        let mut env = fixture.env();
        let index = packers.find(&mut env, "m.model", "out/m.model").unwrap();
        assert!(packers.pack_at(index, &mut env));
        let bytes = fixture.read("out/m.model");
        let packed = PackedSection::parse("root", &bytes);
        assert!(packed.find_child("nodefullVisual").is_some());
        assert!(packed.find_child("anim").is_some());
        assert_eq!(fixture.read("out/m.anca"), b"anca bytes");

        // Print labels.
        let mut env = fixture.env();
        let index = packers.find(&mut env, "m.model", "o.model").unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "ModelFile: m.model\n");
        env.out.clear();
        let index = packers
            .find(&mut env, "anim.animation", "o.animation")
            .unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "AnimationFile: anim.animation\n");
        env.out.clear();
        let index = packers.find(&mut env, "compressed.anca", "o.anca").unwrap();
        assert!(packers.print_at(index, &mut env));
        assert_eq!(env.out, "CompressedAnimationFile: compressed.anca\n");

        // An unreadable model reports through the message-exact line.
        let mut env = fixture.env();
        let index = packers
            .find(&mut env, "missing.model", "out/x.model")
            .unwrap();
        assert!(!packers.pack_at(index, &mut env));
        assert_eq!(env.out, "Could not open model file missing.model\n");
    }

    #[test]
    fn registry_dispatches_by_extension_and_priority() {
        let fixture = Fixture::new("registry");
        fixture.write(
            "a.cdata", b"<root/>", // prepare will warn on space.settings but claims ext
        );
        // .cdata declines (no space.settings) so it stays unclaimed
        // unless settings exist — claimed index check needs the file to
        // succeed, so give it a settings file.
        fixture.write("space.settings", b"<space/>");
        fixture.write("s.chunk", b"<root/>");
        fixture.write("f.fxo", b"x");
        fixture.write("i.tga", b"x");
        fixture.write("m.model", b"<model/>");
        fixture.write("x.xml", b"<root/>");

        let mut env = fixture.env();
        let mut packers = Packers::new();
        let index = packers.find(&mut env, "a.cdata", "o").unwrap();
        assert!(packers.print_at(index, &mut env));
        assert!(env.out.starts_with("CDataFile: a.cdata\n"));
        let kinds = [
            ("s.chunk", "ChunkFile: s.chunk\n"),
            ("f.fxo", "FxCompiledFile: f.fxo\n"),
            ("i.tga", "ImageFile: i.tga\n"),
            ("m.model", "ModelFile: m.model\n"),
            ("x.xml", "root: ''\n"),
        ];
        for (name, expected) in kinds {
            let mut env = fixture.env();
            let index = packers.find(&mut env, name, "out/x").unwrap();
            assert!(packers.print_at(index, &mut env), "{name}");
            assert_eq!(env.out, expected, "{name}");
        }
    }

    #[test]
    fn batch_mode_processes_the_list() {
        let fixture = Fixture::new("batch");
        fixture.write("res/in.xml", b"<root><a>42</a></root>");
        fixture.write("res/raw.dat", b"\x00\x01raw");
        fixture.write("res/never.txt", b"should not appear");

        // The final line has no trailing newline, so its last character is
        // eaten: "never.tx" doesn't exist and fails to copy.
        let list_path = fixture.dir.join("list.txt");
        std::fs::write(&list_path, "in.xml\nraw.dat\nnever.txt").unwrap();
        fixture.mkdir("packed");
        fixture.mkdir("packed2");

        let mut env = fixture.env();
        let mut packers = Packers::new();
        let args = vec![
            "--list".to_string(),
            list_path.to_str().unwrap().to_string(),
            "--in".to_string(),
            "res".to_string(),
            "--out".to_string(),
            "packed".to_string(),
        ];
        let code = run(&mut env, &args, &mut packers);
        assert_eq!(code, 0);
        // XmlPacker's own prepare message comes out during `find`, before
        // the "Processing" line; the copy error lands between it and the
        // result suffix.
        assert_eq!(
            env.out,
            "Processing res/in.xml... succeeded\n\
             Processing res/raw.dat... succeeded\n\
             Error opening source file: res/never.tx\n\
             Processing res/never.tx...Error: Cannot open source file \
             res/never.tx.\n failed\n\
             \nProcessing complete - 1 files failed: run with \
             \"--err 'error_log_file'\" argument for more information.\n\n"
        );
        assert!(PackedSection::is_packed_file(
            &fixture.read("packed/in.xml")
        ));
        assert_eq!(fixture.read("packed/raw.dat"), b"\x00\x01raw");

        // With --err the failure lands in the log and the tail points at
        // it.
        let err_path = fixture.dir.join("error.log");
        let args: Vec<String> = [
            "--list",
            list_path.to_str().unwrap(),
            "--in",
            "res",
            "--out",
            "packed2",
            "--err",
            err_path.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut env = fixture.env();
        let code = run(&mut env, &args, &mut packers);
        assert_eq!(code, 0);
        assert!(
            env.out
                .ends_with(" - 1 files failed: refer to error log for more information.\n\n")
        );
        assert_eq!(
            std::fs::read_to_string(&err_path).unwrap(),
            "res/never.tx\n"
        );
    }

    #[test]
    fn batch_mode_validates_flags_and_opens_files_eagerly() {
        let fixture = Fixture::new("validate");
        fixture.write("list.txt", b"in.xml\n");

        // List mode needs --in and --out and no positionals.
        let mut env = fixture.env();
        let mut packers = Packers::new();
        let list_path = fixture.dir.join("list.txt");
        let args = vec![
            "--list".to_string(),
            list_path.to_str().unwrap().to_string(),
        ];
        assert_eq!(run(&mut env, &args, &mut packers), 1);
        assert!(env.out.starts_with("Compatible usage: res_packer"));

        // A missing list file fails during the flag walk, before
        // validation.
        let mut env = fixture.env();
        let args = vec![
            "--list".to_string(),
            "missing.txt".to_string(),
            "--in".to_string(),
            "res".to_string(),
            "--out".to_string(),
            "out".to_string(),
        ];
        assert_eq!(run(&mut env, &args, &mut packers), 1);
        assert!(
            env.out
                .contains("Unable to open asset list \"missing.txt\"\n")
        );
        assert!(!env.out.contains("Compatible usage"));

        // Compatible mode takes one to three positionals.
        let mut env = fixture.env();
        let args: Vec<String> = Vec::new();
        assert_eq!(run(&mut env, &args, &mut packers), 1);
        assert!(env.out.starts_with("Compatible usage: res_packer"));
    }

    #[test]
    fn compatible_mode_packs_prints_and_fails() {
        let fixture = Fixture::new("compatible");
        fixture.write("in.xml", b"<root><a>7</a></root>");
        fixture.write("plain.bmp", b"bmp bytes");

        // Print mode: one positional prints the string representation.
        let mut env = fixture.env();
        let mut packers = Packers::new();
        let args = ["in.xml".to_string()];
        assert_eq!(run(&mut env, &args, &mut packers), 0);
        assert_eq!(env.out, "root: ''\n  a: '7'\n");

        // An unknown file: `find` probes it and the reference's
        // XmlPacker::prepare reports the open failure, then print mode
        // returns success with nothing printed.
        let mut env = fixture.env();
        let args = ["unknown.abc".to_string()];
        assert_eq!(run(&mut env, &args, &mut packers), 0);
        assert_eq!(env.out, "Error opening source file: unknown.abc\n");

        // Pack mode: "Processing %s to %s..." + result. `main` ends with
        // `return result ? EXIT_SUCCESS : EXIT_FAILURE`.
        let mut env = fixture.env();
        let args = ["in.xml".to_string(), "out.packed".to_string()];
        assert_eq!(run(&mut env, &args, &mut packers), 0);
        assert_eq!(
            env.out,
            "Processing in.xml to out.packed...\n...succeeded\n"
        );
        assert!(PackedSection::is_packed_file(&fixture.read("out.packed")));

        // A failing pack reports failed and exits non-zero (bmp to png,
        // no DDS).
        let mut env = fixture.env();
        let args = ["plain.bmp".to_string(), "out.png".to_string()];
        assert_eq!(run(&mut env, &args, &mut packers), 1);
        assert!(env.out.contains("Processing plain.bmp to out.png...\n"));
        assert!(env.out.ends_with("...failed\n"));
        assert!(
            env.out
                .contains("Error: Cannot copy source plain.bmp to destination out.png.\n")
        );

        // Errors flag the result even when the pack returns true: the
        // image was already produced but the message handler saw an
        // ERROR. Covered via the error prefix path above.
    }

    #[test]
    fn remove_trailing_slash_removes_one_separator() {
        assert_eq!(remove_trailing_slash("res/"), "res");
        assert_eq!(remove_trailing_slash("res\\"), "res");
        assert_eq!(remove_trailing_slash("res//"), "res/");
        assert_eq!(remove_trailing_slash("res"), "res");
    }

    #[test]
    fn too_many_children_reports_the_typo_message() {
        let mut root = DataSection::new("root");
        for _ in 0..(i16::MAX as usize + 1) {
            root.add_child("kid");
        }
        let fixture = Fixture::new("toomany");
        let mut env = fixture.env();
        let result = convert_bytes(&mut env, b"<root/>", &[], false, 1);
        assert!(result.is_ok()); // the tiny input converts fine
        let packed = packed::pack(&root).unwrap_err();
        let message = too_many_children_message(&packed);
        assert_eq!(
            message,
            "PackedSetcion: writeAsPackedSection failed, tag 'root' has too many \
             children\n"
        );
    }
}
