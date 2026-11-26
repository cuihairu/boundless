//! `offline_processor` port (`tools/offline_processor/`): the command
//! surface (`command_manager.cpp`, `command.cpp`), the cluster-shard hash
//! (`offline_chunk_processor_manager.cpp`), the chunk gathering
//! (`lib/chunk/geometry_mapping.cpp`, `lib/chunk/chunk_format.hpp`), the
//! `.cdata` invalidation writer (`lib/chunk/chunk_processor_manager.cpp`)
//! and the two commands: `process-chunks` and `upgrade-bsp`.
//!
//! Device-free faces only: the Win32 window/console plumbing, the Moo
//! device bring-up, `ChunkManager`'s chunk load/save cycle and the
//! space-browse dialog are the seams — [`ChunkSaveHook`] stands in for the
//! runtime save pass, and `invalidate_all_chunks` writes the `.cdata`
//! flags through the binary-section face so the original tools consume
//! them (`navmeshDirty` + the `"dirtyFlags"` block).

use std::collections::BTreeSet;

use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::{BwResource, get_extension};
use resources::file_system::FileType;

// ---------------------------------------------------------------------------
// CommandLine (`cstdmf/command_line.cpp`)
// ---------------------------------------------------------------------------

/// `CommandLine`: whitespace-split tokens with `"..."` quoting.
#[derive(Clone)]
pub struct CommandLine {
    params: Vec<String>,
}

impl CommandLine {
    pub fn new(command_line: &str) -> Self {
        let bytes = command_line.as_bytes();
        let mut offset = 0usize;
        let mut params = Vec::new();

        while offset < bytes.len() {
            while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
                offset += 1;
            }

            let mut param = Vec::new();
            while offset < bytes.len() {
                if bytes[offset] == b'"' {
                    offset += 1;
                    while offset < bytes.len() && bytes[offset] != b'"' {
                        param.push(bytes[offset]);
                        offset += 1;
                    }
                    if offset < bytes.len() {
                        offset += 1;
                    }
                } else if !bytes[offset].is_ascii_whitespace() {
                    param.push(bytes[offset]);
                    offset += 1;
                } else {
                    break;
                }
            }

            // A trailing separator still yields an empty parameter (the
            // reference pushes unconditionally per outer iteration).
            params.push(String::from_utf8_lossy(&param).into_owned());
        }

        Self { params }
    }

    fn from_params(params: &[&str]) -> Self {
        Self {
            params: params.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// `hasParam`: first char in `/-` and the rest equal to `param`.
    pub fn has_param(&self, param: &str) -> bool {
        self.params
            .iter()
            .any(|entry| is_switch_entry(entry, param))
    }

    /// `getParam`: the token after the switch, `""` when absent or last.
    pub fn get_param(&self, param: &str) -> &str {
        for (index, entry) in self.params.iter().enumerate() {
            if is_switch_entry(entry, param) {
                return self.params.get(index + 1).map(String::as_str).unwrap_or("");
            }
        }
        ""
    }

    /// `getFullParam`: with `param_index > 0` — the token after
    /// `params[param_index]` when that token equals `param_to_find` (the
    /// command-name slot); otherwise the token after the first exact
    /// match. `None` is the reference's NULL (no match), `Some("")` the
    /// match-at-tail case.
    pub fn get_full_param(&self, param_to_find: &str, param_index: i32) -> Option<&str> {
        if param_index > 0 {
            let index = param_index as usize;
            if self.params.len() > index && self.params[index] == param_to_find {
                return Some(self.params.get(index + 1).map(String::as_str).unwrap_or(""));
            }
            None
        } else {
            for (index, entry) in self.params.iter().enumerate() {
                if entry == param_to_find {
                    return Some(self.params.get(index + 1).map(String::as_str).unwrap_or(""));
                }
            }
            None
        }
    }

    /// `hasFullParam`.
    pub fn has_full_param(&self, param_to_find: &str, param_index: i32) -> bool {
        self.get_full_param(param_to_find, param_index).is_some()
    }

    /// `getParamByIndex`: `None` for index 0 (the program-name slot) or
    /// out of range.
    pub fn get_param_by_index(&self, param_index: i32) -> Option<&str> {
        if param_index > 0 && self.params.len() > param_index as usize {
            return Some(self.params[param_index as usize].as_str());
        }
        None
    }

    /// `hasSwitch`: a switch needs at least two symbols and a leading
    /// `/-` char.
    pub fn has_switch(param: &str) -> bool {
        param.len() >= 2 && (param.starts_with('/') || param.starts_with('-'))
    }

    pub fn size(&self) -> usize {
        self.params.len()
    }

    /// `param(index)`: panics out of range (`vector::at`).
    pub fn param(&self, index: usize) -> &str {
        &self.params[index]
    }
}

fn is_switch_entry(entry: &str, param: &str) -> bool {
    let bytes = entry.as_bytes();
    !bytes.is_empty() && (bytes[0] == b'/' || bytes[0] == b'-') && &entry[1..] == param
}

// ---------------------------------------------------------------------------
// Cluster sharding (`offline_chunk_processor_manager.cpp`)
// ---------------------------------------------------------------------------

/// The anonymous-namespace `bw_hash`: a 256-byte table shuffled by four
/// swap passes seeded at `k = 7`, then a byte walk. Returns 0..255.
pub fn bw_hash(s: &str) -> u32 {
    let mut hash = [0u8; 256];
    for (i, slot) in hash.iter_mut().enumerate() {
        *slot = i as u8;
    }
    let mut k: usize = 7;
    for _ in 0..4 {
        for i in 0..256 {
            let s = hash[i];
            k = (k + s as usize) % 256;
            hash[i] = hash[k];
            hash[k] = s;
        }
    }

    let bytes = s.as_bytes();
    let mut result: u32 = ((123 + bytes.len()) % 256) as u32;
    for &b in bytes {
        result = (result + b as u32) % 256;
        result = hash[result as usize] as u32;
    }
    result
}

/// `OfflineChunkProcessorManager::isChunkEditable`: the cluster shard a
/// chunk identifier belongs to.
pub fn is_chunk_editable(chunk: &str, cluster_size: u32, cluster_index: u32) -> bool {
    bw_hash(chunk) % cluster_size == cluster_index
}

/// `atoi` into the reference's unsigned locals: leading sign and digits,
/// non-numeric input answers 0, negatives wrap through `unsigned int`.
fn atoi(s: &str) -> u32 {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut negative = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        negative = bytes[i] == b'-';
        i += 1;
    }
    let mut digits = String::new();
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        digits.push(bytes[i] as char);
        i += 1;
    }
    let value: i32 = digits.parse().unwrap_or(0);
    if negative {
        (-value) as u32
    } else {
        value as u32
    }
}

// ---------------------------------------------------------------------------
// Chunk identifiers (`lib/chunk/chunk_format.hpp`,
// `lib/chunk/geometry_mapping.cpp`)
// ---------------------------------------------------------------------------

/// `ChunkFormat::outsideChunkIdentifier`: `%04x%04xo`, prefixed by one
/// `…/sep/` level per axis whose u16 coordinate trips that level's wrap
/// window (unless the space is `single_dir`).
pub fn outside_chunk_identifier(grid_x: i32, grid_z: i32, single_dir: bool) -> String {
    let gridxs = grid_x as u16;
    let gridzs = grid_z as u16;

    let mut identifier = String::new();
    if !single_dir {
        // The reference formats are "%01xxxx%01xxxx/sep/",
        // "%02xxx%02xxx/sep/" and "%03xx%03xx/sep/" — 3, 2 and 1 literal
        // x per side after the `%01x`/`%02x`/`%03x` fields.
        if gridxs.wrapping_add(4096) >= 8192 || gridzs.wrapping_add(4096) >= 8192 {
            identifier += &format!("{:01x}xxx{:01x}xxx/sep/", gridxs >> 12, gridzs >> 12);
        }
        if gridxs.wrapping_add(256) >= 512 || gridzs.wrapping_add(256) >= 512 {
            identifier += &format!("{:02x}xx{:02x}xx/sep/", gridxs >> 8, gridzs >> 8);
        }
        if gridxs.wrapping_add(16) >= 32 || gridzs.wrapping_add(16) >= 32 {
            identifier += &format!("{:03x}x{:03x}x/sep/", gridxs >> 4, gridzs >> 4);
        }
    }
    identifier += &format!("{:04x}{:04x}o", gridxs, gridzs);
    identifier
}

/// `GeometryMapping::gridFromChunkName` and its `HexLookup`: the last
/// nine characters (`xxxxxxxxo`) parsed as two four-digit hex words, or
/// `None` when the name does not end in `o` with eight hex digits.
pub fn grid_from_chunk_name(chunk_name: &str) -> Option<(i16, i16)> {
    fn hex_lookup(c: u8) -> u16 {
        match c {
            b'0'..=b'9' => (c - b'0') as u16,
            b'a'..=b'f' => (c - b'a' + 10) as u16,
            b'A'..=b'F' => (c - b'A' + 10) as u16,
            _ => 0,
        }
    }
    fn hex_factor(c: u8) -> u8 {
        u8::from(c.is_ascii_hexdigit())
    }
    fn from_hex(f: &[u8]) -> Option<i16> {
        let value = hex_lookup(f[3])
            + (hex_lookup(f[2]) << 4)
            + (hex_lookup(f[1]) << 8)
            + (hex_lookup(f[0]) << 12);
        let factor = hex_factor(f[0]) & hex_factor(f[1]) & hex_factor(f[2]) & hex_factor(f[3]);
        if factor != 0 {
            Some(value as i16)
        } else {
            None
        }
    }

    let bytes = chunk_name.as_bytes();
    if bytes.last() == Some(&b'o') && bytes.len() >= 9 {
        let tail = &bytes[bytes.len() - 9..];
        let x = from_hex(&tail[..4])?;
        let z = from_hex(&tail[4..8])?;
        return Some((x, z));
    }
    None
}

// ---------------------------------------------------------------------------
// GeometryMapping (identity-mapper face)
// ---------------------------------------------------------------------------

/// `GeometryMapping::getGridSize` default (`chunk_grid_size.hpp`).
pub const DEFAULT_GRID_RESOLUTION: f32 = 100.0;

/// `GeometryMapping` with the identity mapper: the space settings, the
/// grid bounds and the chunk enumeration. The matrix/world-bounds faces
/// wait on the full `ChunkSpace` runtime.
pub struct GeometryMapping {
    path: String,
    min_lgrid_x: i32,
    min_lgrid_y: i32,
    max_lgrid_x: i32,
    max_lgrid_y: i32,
    chunk_size: f32,
    single_dir: bool,
    navmesh_generator: String,
}

impl GeometryMapping {
    /// The constructor's settings read (`openSettings` + the
    /// `bounds/minX`-style children). `None` when `space.settings` is
    /// missing — the reference keeps a NULL settings section and skips
    /// the grid read.
    pub fn open(resource: &BwResource, path: &str) -> Option<Self> {
        let settings = resource.open_section(&format!("{path}/space.settings"), false)?;
        let read_int = |name: &str| settings.read_int(name, 0);
        Some(Self {
            path: format!("{path}/"),
            min_lgrid_x: read_int("bounds/minX"),
            min_lgrid_y: read_int("bounds/minY"),
            max_lgrid_x: read_int("bounds/maxX"),
            max_lgrid_y: read_int("bounds/maxY"),
            chunk_size: settings.read_float("chunkSize", DEFAULT_GRID_RESOLUTION),
            single_dir: settings.read_bool("singleDir", false),
            navmesh_generator: settings.read_string("navmeshGenerator", ""),
        })
    }

    /// `path()`: the space directory with the trailing slash.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// `getGridSize`.
    pub fn chunk_size(&self) -> f32 {
        self.chunk_size
    }

    /// `readString( "navmeshGenerator" )`.
    pub fn navmesh_generator(&self) -> &str {
        &self.navmesh_generator
    }

    /// `singleDir`.
    pub fn single_dir(&self) -> bool {
        self.single_dir
    }

    pub fn min_grid_x(&self) -> i32 {
        self.min_lgrid_x
    }
    pub fn min_grid_y(&self) -> i32 {
        self.min_lgrid_y
    }
    pub fn max_grid_x(&self) -> i32 {
        self.max_lgrid_x
    }
    pub fn max_grid_y(&self) -> i32 {
        self.max_lgrid_y
    }

    /// `inLocalBounds`: the local-grid window (the identity mapper makes
    /// the world grid the same window).
    pub fn in_local_bounds(&self, grid_x: i32, grid_z: i32) -> bool {
        grid_x >= self.min_lgrid_x
            && grid_x <= self.max_lgrid_x
            && grid_z >= self.min_lgrid_y
            && grid_z <= self.max_lgrid_y
    }

    /// `outsideChunkIdentifier(x, z)`: bounds-checked, `""` outside.
    pub fn outside_chunk_identifier(&self, x: i32, z: i32) -> String {
        if !self.in_local_bounds(x, z) {
            return String::new();
        }
        outside_chunk_identifier(x, z, self.single_dir)
    }

    /// `chunkFileExists`.
    pub fn chunk_file_exists(&self, resource: &BwResource, chunk_id: &str) -> bool {
        resource.file_exists(&format!("{}{}.chunk", self.path, chunk_id))
    }

    /// `Util::gatherInternalChunks`: the recursive `xxxxxxxxi.chunk`
    /// scan (names of length 15 whose tail from index 8 is `i.chunk`
    /// contribute their first nine characters).
    pub fn gather_internal_chunks(&self, resource: &BwResource) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        self.gather_internal_chunks_walk(resource, "", &mut result);
        result
    }

    fn gather_internal_chunks_walk(
        &self,
        resource: &BwResource,
        sub_folder: &str,
        result: &mut BTreeSet<String>,
    ) {
        let folder = format!("{}{}", self.path, sub_folder);
        let Some(dir) = resource.file_system().read_directory(&folder) else {
            return;
        };
        for entry in dir {
            if entry.len() == 15 && &entry[8..] == "i.chunk" {
                result.insert(format!("{}{}", sub_folder, &entry[..9]));
            } else if resource.get_file_type(&format!("{folder}{entry}"), None)
                == FileType::Directory
            {
                self.gather_internal_chunks_walk(
                    resource,
                    &format!("{sub_folder}{entry}/"),
                    result,
                );
            }
        }
    }

    /// `gatherChunks`: the internal chunks plus every outside identifier
    /// in the grid window (note the reference iterates the grid **Y**
    /// bounds as the z axis).
    pub fn gather_chunks(&self, resource: &BwResource) -> BTreeSet<String> {
        let mut chunks = self.gather_internal_chunks(resource);
        for i in self.min_lgrid_x..=self.max_lgrid_x {
            for j in self.min_lgrid_y..=self.max_lgrid_y {
                chunks.insert(outside_chunk_identifier(i, j, self.single_dir));
            }
        }
        chunks
    }
}

// ---------------------------------------------------------------------------
// ChunkCleanFlags (`lib/chunk/chunk_clean_flags.cpp`)
// ---------------------------------------------------------------------------

/// The odd-but-compatibility name of the clean-flags child.
pub const CLEAN_FLAGS_NAME: &str = "dirtyFlags";

/// `ChunkCleanFlags`: the three `uint32` update stamps kept as the 12
/// bytes of a `dirtyFlags` binary child (struct memcpy → little-endian).
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChunkCleanFlags {
    pub thumbnail: u32,
    pub shadow: u32,
    pub terrain_lod: u32,
}

impl ChunkCleanFlags {
    pub const SERIALIZED_SIZE: usize = 12;

    pub fn pack(&self) -> [u8; Self::SERIALIZED_SIZE] {
        let mut out = [0u8; Self::SERIALIZED_SIZE];
        out[0..4].copy_from_slice(&self.thumbnail.to_le_bytes());
        out[4..8].copy_from_slice(&self.shadow.to_le_bytes());
        out[8..12].copy_from_slice(&self.terrain_lod.to_le_bytes());
        out
    }

    /// Only a 12-byte block decodes (the reference's `len()` check).
    pub fn unpack(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::SERIALIZED_SIZE {
            return None;
        }
        Some(Self {
            thumbnail: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            shadow: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            terrain_lod: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        })
    }

    /// The constructor's read: zeros when the child is missing or the
    /// wrong size.
    pub fn from_cdata(cdata: &mut BinSection) -> Self {
        cdata
            .read_binary(CLEAN_FLAGS_NAME)
            .and_then(|block| Self::unpack(block.data()))
            .unwrap_or_default()
    }

    /// `save`: create the child and swap the block in.
    pub fn save_to(&self, cdata: &mut BinSection) {
        cdata
            .new_section(CLEAN_FLAGS_NAME)
            .set_binary(BinaryBlock::new(self.pack().to_vec()));
    }
}

// ---------------------------------------------------------------------------
// Invalidation (`ChunkProcessorManager::invalidateAllChunks`)
// ---------------------------------------------------------------------------

/// `invalidateAllChunks`: for every editable chunk of the mapping, write
/// `navmeshDirty = true` into its `.cdata` (and zero the clean flags
/// unless `invalidate_only_navmesh`). Returns `false` when cancelled.
/// The reference's 0.1-second progress throttle is console seam; the
/// cancel check runs every editable iteration here.
pub fn invalidate_all_chunks(
    resource: &BwResource,
    mapping: &GeometryMapping,
    cluster_size: u32,
    cluster_index: u32,
    invalidate_only_navmesh: bool,
    is_cancelled: &dyn Fn() -> bool,
) -> bool {
    let space_path = mapping.path();
    let chunks = mapping.gather_chunks(resource);

    for chunk in &chunks {
        if is_chunk_editable(chunk, cluster_size, cluster_index) {
            if is_cancelled() {
                return false;
            }

            let cdata_path = format!("{space_path}{chunk}.cdata");
            let Some(raw) = resource.file_system().read_file(&cdata_path) else {
                continue;
            };
            let mut cdata = BinSection::new(chunk, BinaryBlock::new(raw));

            // navmeshDirty: a one-byte bool set to true.
            let mut updated = BinSection::new(cdata.section_name(), BinaryBlock::default());
            for child in cdata.children() {
                updated
                    .new_section(child.section_name())
                    .set_binary(child.as_binary().clone());
            }
            updated
                .new_section("navmeshDirty")
                .set_binary(BinaryBlock::new(vec![1]));

            if !invalidate_only_navmesh {
                ChunkCleanFlags::default().save_to(&mut updated);
            }

            resource
                .file_system()
                .write_file(&cdata_path, &updated.save_bytes());
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Commands (`command.hpp`, `command_process_chunks.cpp`,
// `command_upgrade_bsp.cpp`, `command_manager.cpp`)
// ---------------------------------------------------------------------------

/// The runtime save pass behind `saveChunks` — `ChunkManager`'s
/// load/process/save cycle is the seam; an embedder receives the gathered
/// chunk set and answers whether the save succeeded.
pub trait ChunkSaveHook {
    fn save_chunks(&mut self, resource: &BwResource, chunks: &BTreeSet<String>) -> bool;
}

/// Always-succeeding stand-in.
pub struct NoopChunkSaver;

impl ChunkSaveHook for NoopChunkSaver {
    fn save_chunks(&mut self, _resource: &BwResource, _chunks: &BTreeSet<String>) -> bool {
        true
    }
}

/// The help and status lines a command produces; printing them is the
/// console seam so tests can compare them message-exact.
pub trait Command {
    fn str_command(&self) -> &'static str;
    fn show_detailed_help(&self) -> String;
    fn show_short_help(&self) -> String;
    fn process(
        &mut self,
        resource: &mut BwResource,
        saver: &mut dyn ChunkSaveHook,
        out: &mut Vec<String>,
    ) -> bool;
}

/// `CommandProcessChunks::process`: the workflow skeleton — cluster
/// params, space settings validation, the overwrite invalidation and the
/// save pass (via the hook).
pub struct ProcessChunksCommand {
    command_line: CommandLine,
}

impl ProcessChunksCommand {
    pub fn new(command_line: CommandLine) -> Self {
        Self { command_line }
    }
}

impl Command for ProcessChunksCommand {
    fn str_command(&self) -> &'static str {
        "process-chunks"
    }

    fn show_detailed_help(&self) -> String {
        [
            "Usage: offline_processor process-chunks [options]\n",
            "Runs offline chunk processing generating navigation data:\n",
            "Options:\n",
            "\t-space <space path>\t\tProcess the given space\n",
            "\t-cluster-index <cluster index>\tSet the computer index in cluster\n",
            "\t-cluster-size <cluster size>\tSet the number of computers in cluster\n",
            "\t-overwrite\t\t\tOverwrite all existing generated data\n",
            "\t-nogui\t\t\t\tDisplay no GUI\n",
            "\t-unattended\t\t\tRun offline_processor in autotest mode\n",
        ]
        .concat()
    }

    fn show_short_help(&self) -> String {
        "process-chunks\t\tRun offline chunk processing and generate navigation data.".to_string()
    }

    fn process(
        &mut self,
        resource: &mut BwResource,
        saver: &mut dyn ChunkSaveHook,
        out: &mut Vec<String>,
    ) -> bool {
        let mut exit_code = 1;
        let mut cluster_size = 1u32;
        let mut cluster_index = 0u32;

        if self.command_line.has_param("cluster-index")
            && self.command_line.has_param("cluster-size")
        {
            // The reference reads the params swapped (clusterSize from
            // "cluster-index", clusterIndex from "cluster-size");
            // preserved.
            cluster_size = atoi(self.command_line.get_param("cluster-index")).max(1);
            cluster_index = atoi(self.command_line.get_param("cluster-size")).min(cluster_size - 1);
        }

        // The space-browse dialog runs only when -space is missing and
        // the GUI is up; headless ports fall straight to the error.
        let space = self.command_line.get_param("space").to_string();
        let nogui = self.command_line.has_param("nogui");

        let space_settings = if space.is_empty() {
            None
        } else {
            resource.open_section(&format!("{space}/space.settings"), false)
        };

        if let Some(space_settings) = space_settings {
            let mapping =
                GeometryMapping::open(resource, &space).expect("space.settings just opened");

            let navmesh_generator = space_settings.read_string("navmeshGenerator", "");
            if navmesh_generator != "recast" {
                if nogui {
                    out.push(
                        "The navmeshGenerator specified in space.settings is not supported.\
                         offline_processor supports \"recast\" only."
                            .to_string(),
                    );
                }
                // The GUI builds show a MessageBox instead. Either way the
                // reference returns `1` from the bool process() here, so
                // the tool exits successfully.
                return true;
            }

            let mut quitting = false;
            if self.command_line.has_param("overwrite") {
                // Only navigation data is invalidated for now (the
                // reference's hard-coded true).
                quitting = !invalidate_all_chunks(
                    resource,
                    &mapping,
                    cluster_size,
                    cluster_index,
                    true,
                    &|| false,
                );
            }

            if !quitting {
                let chunks = mapping.gather_chunks(resource);
                exit_code = i32::from(!saver.save_chunks(resource, &chunks));
            }
        } else if space.is_empty() {
            out.push("You have to specify a space using /space".to_string());
        } else {
            out.push(format!("Cannot open space {space}"));
        }

        exit_code == 0
    }
}

/// `BatchCommand::FileProcessResult`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileProcessResult {
    Success,
    Failure,
    UnsupportedFile,
}

/// `BatchCommand::ResourceSystemInitResult`; the paths.xml/`addPath`
/// bring-up is the embedder's seam, so only the file-type classification
/// is ported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ResourceSystemInitResult {
    SuccessFile,
    SuccessDirectory,
    Failed,
}

/// `CommandUpgradeBSP`: the batch BSP upgrade.
pub struct UpgradeBspCommand {
    command_line: CommandLine,
    allow_missing_files: bool,
    num_errors: i32,
}

impl UpgradeBspCommand {
    pub fn new(command_line: CommandLine) -> Self {
        Self {
            command_line,
            allow_missing_files: false,
            num_errors: 0,
        }
    }

    /// `initResourceSystem`'s classification. The current-directory retry
    /// and the path-bookkeeping (`addPath`) ride on the embedder's
    /// resource setup.
    fn init_resource_system(
        &self,
        resource: &BwResource,
        file_name: &str,
        out: &mut Vec<String>,
    ) -> ResourceSystemInitResult {
        match resource.get_file_type(file_name, None) {
            FileType::File => ResourceSystemInitResult::SuccessFile,
            FileType::Directory => ResourceSystemInitResult::SuccessDirectory,
            FileType::NotFound if self.allow_missing_files => ResourceSystemInitResult::SuccessFile,
            _ => {
                out.push(format!(
                    " trying to initialise resource system, but cannot find file {file_name}"
                ));
                ResourceSystemInitResult::Failed
            }
        }
    }

    /// `processFiles`: the single file, or the (possibly recursive)
    /// directory walk with the `./` prefix strip.
    fn process_files(
        &mut self,
        resource: &BwResource,
        path: &str,
        process_recursively: bool,
        out: &mut Vec<String>,
    ) -> i32 {
        let mut num_files_processed = 0;
        let ft = resource.get_file_type(path, None);
        if ft == FileType::File || (ft == FileType::NotFound && self.allow_missing_files) {
            return match self.process_file(resource, path, out) {
                FileProcessResult::Success => 1,
                FileProcessResult::Failure => {
                    self.num_errors += 1;
                    0
                }
                FileProcessResult::UnsupportedFile => 0,
            };
        }

        if let Some(dir_list) = resource.file_system().read_directory(path) {
            for entry in dir_list {
                let mut sub_name = format!("{}{}", resources::bwresource::format_path(path), entry);
                if let Some(stripped) = sub_name.strip_prefix("./") {
                    sub_name = stripped.to_string();
                }

                let is_directory = resource.get_file_type(&sub_name, None) == FileType::Directory;
                if process_recursively && is_directory {
                    num_files_processed +=
                        self.process_files(resource, &sub_name, process_recursively, out);
                } else if !is_directory {
                    match self.process_file(resource, &sub_name, out) {
                        FileProcessResult::Success => num_files_processed += 1,
                        FileProcessResult::Failure => self.num_errors += 1,
                        FileProcessResult::UnsupportedFile => {}
                    }
                }
            }
        }
        num_files_processed
    }

    /// `CommandUpgradeBSP::processFile`.
    fn process_file(
        &mut self,
        resource: &BwResource,
        file_name: &str,
        out: &mut Vec<String>,
    ) -> FileProcessResult {
        let dis_file_name = resource.dissolve_filename(file_name);
        let ext = get_extension(&dis_file_name);
        if ext == "primitives" {
            if self.do_format_upgrade_bsp_in_primitives(resource, &dis_file_name, out) {
                FileProcessResult::Success
            } else {
                FileProcessResult::Failure
            }
        } else if ext == "bsp2" {
            if self.do_format_upgrade_standalone_bsp_file(resource, &dis_file_name, out) {
                FileProcessResult::Success
            } else {
                FileProcessResult::Failure
            }
        } else {
            FileProcessResult::UnsupportedFile
        }
    }

    /// `doFormatUpgradeBSPInPrimitives`.
    fn do_format_upgrade_bsp_in_primitives(
        &self,
        resource: &BwResource,
        prim_name: &str,
        out: &mut Vec<String>,
    ) -> bool {
        use physics::bsp::BspTree;

        let Some(raw) = resource.file_system().read_file(prim_name) else {
            out.push(format!("ERROR: Unable to open primitives {prim_name}"));
            return false;
        };
        let mut container = BinSection::new("primitives", BinaryBlock::new(raw));
        let Some(bsp) = container.read_binary("bsp2") else {
            // A primitive without a bsp section just means there is no
            // bsp in there — not an error.
            return true;
        };
        let Ok(tree) = BspTree::load(bsp.data()) else {
            out.push(format!("ERROR: Can't load bsp for  {prim_name}"));
            return false;
        };
        let saved = BinaryBlock::new(tree.save());

        // `writeBinary( "bsp2", bp )`: the child is replaced in place,
        // the siblings survive.
        let mut rebuilt = BinSection::new(container.section_name(), BinaryBlock::default());
        for child in container.children() {
            if child.section_name() == "bsp2" {
                rebuilt.new_section("bsp2").set_binary(saved.clone());
            } else {
                rebuilt
                    .new_section(child.section_name())
                    .set_binary(child.as_binary().clone());
            }
        }

        if !resource
            .file_system()
            .write_file(prim_name, &rebuilt.save_bytes())
        {
            out.push(format!(
                "ERROR: Failed ot write regenerated bsp for {prim_name}"
            ));
            return false;
        }
        out.push(format!("regenerating bsp in {prim_name}"));
        true
    }

    /// `doFormatUpgradeStandaloneBSPFile`.
    fn do_format_upgrade_standalone_bsp_file(
        &self,
        resource: &BwResource,
        bsp_name: &str,
        out: &mut Vec<String>,
    ) -> bool {
        use physics::bsp::BspTree;

        let Some(raw) = resource.file_system().read_file(bsp_name) else {
            out.push(format!("ERROR: Could not load BSP section from {bsp_name}"));
            return false;
        };
        let Ok(tree) = BspTree::load(&raw) else {
            out.push(format!(
                "ERROR: Could not load BSP data from file {bsp_name}"
            ));
            return false;
        };
        // `BSPTreeTool::saveBSPInFile`: straight back out in the current
        // format.
        if !resource.file_system().write_file(bsp_name, &tree.save()) {
            return false;
        }
        out.push(format!("regenerating bsp in {bsp_name}"));
        true
    }
}

impl Command for UpgradeBspCommand {
    fn str_command(&self) -> &'static str {
        "upgrade-bsp"
    }

    fn show_detailed_help(&self) -> String {
        [
            "Usage: offline_processor upgrade-bsp [file] [options]\n",
            "Loads and saves bsp data upgrading it to the latest format:\n",
            "[file] name can be a folder name\n",
            "Options:\n",
            "\t-recursive\t\tProcess all files recursively starting from the given folder.\n",
        ]
        .concat()
    }

    fn show_short_help(&self) -> String {
        "upgrade-bsp\t\tUpgrade bsp in .primitives and .bsp2 to the latest format.".to_string()
    }

    /// `BatchCommand::process`.
    fn process(
        &mut self,
        resource: &mut BwResource,
        _saver: &mut dyn ChunkSaveHook,
        out: &mut Vec<String>,
    ) -> bool {
        let mut path = self
            .command_line
            .get_full_param(self.str_command(), 1)
            .unwrap_or("")
            .to_string();

        let mut command_param_valid = !path.is_empty() && !CommandLine::has_switch(&path);
        let process_recursively = self.command_line.has_param("recursive");

        if !command_param_valid && process_recursively {
            path = ".".to_string();
            command_param_valid = true;
        }

        if !command_param_valid {
            out.push(self.show_detailed_help());
            return false;
        }

        let res_init = self.init_resource_system(resource, &path, out);
        let mut num_files_processed = 0;

        if res_init != ResourceSystemInitResult::Failed {
            if res_init == ResourceSystemInitResult::SuccessFile
                || res_init == ResourceSystemInitResult::SuccessDirectory
            {
                path = resource.dissolve_filename(&path);
            } else {
                path = String::new();
            }
            num_files_processed = self.process_files(resource, &path, process_recursively, out);
        } else {
            out.push(format!("[OfflineProcessor] ERROR: Can' find path {path}."));
            self.num_errors = 1;
        }

        out.push(format!("{} files processed.", num_files_processed));
        out.push(format!("{} error (s).", self.num_errors));
        if res_init == ResourceSystemInitResult::SuccessDirectory
            && num_files_processed == 0
            && !process_recursively
        {
            out.push("Did you forget to add -recursive option ?".to_string());
        }

        self.num_errors == 0
    }
}

// ---------------------------------------------------------------------------
// CommandManager (`command_manager.cpp`)
// ---------------------------------------------------------------------------

/// `CommandManager`: the command table in registration order
/// (`process-chunks` first, then `upgrade-bsp`), command selection, the
/// help surfaces and `run`.
pub struct CommandManager {
    application_command_line: CommandLine,
    verbose: bool,
}

impl CommandManager {
    /// The test constructor (the raw command line flavour).
    pub fn new(application_command_line: &str) -> Self {
        Self {
            application_command_line: CommandLine::new(application_command_line),
            verbose: false,
        }
    }

    fn command_names(&self) -> [&'static str; 2] {
        ["process-chunks", "upgrade-bsp"]
    }

    /// `selectCommand`: the first command named at the parameter-1 slot.
    pub fn select_command(&self) -> Option<&'static str> {
        self.command_names()
            .into_iter()
            .find(|name| self.application_command_line.has_full_param(name, 1))
    }

    /// `showAllSupportedCommands`.
    pub fn show_all_supported_commands(&self) -> String {
        let process_chunks = ProcessChunksCommand::new(CommandLine::from_params(&["app"]));
        let upgrade_bsp = UpgradeBspCommand::new(CommandLine::from_params(&["app"]));
        [
            "BigWorld offline processor\n\n",
            "Usage: offline_processor [command] [options]\n\n",
            "Commands:\n\n",
            &process_chunks.show_short_help(),
            "\n",
            &upgrade_bsp.show_short_help(),
            "\n",
            "help\t\t\tShow this help\n\n",
            "Use offline_processor help [command] for detailed help",
        ]
        .concat()
    }

    /// `displayHelp`: `None` when the command is unknown (the caller
    /// falls back to the command list).
    pub fn display_help(&self, help_cmd: &str) -> Option<String> {
        let process_chunks = ProcessChunksCommand::new(CommandLine::from_params(&["app"]));
        if process_chunks.str_command() == help_cmd {
            return Some(process_chunks.show_detailed_help());
        }
        let upgrade_bsp = UpgradeBspCommand::new(CommandLine::from_params(&["app"]));
        if upgrade_bsp.str_command() == help_cmd {
            return Some(upgrade_bsp.show_detailed_help());
        }
        None
    }

    /// `run`: dispatch on the command slot, or the help surfaces. The
    /// `-unattended` autotest check (`CStdMf::checkUnattended`) is the
    /// harness seam.
    pub fn run(
        &mut self,
        resource: &mut BwResource,
        saver: &mut dyn ChunkSaveHook,
        out: &mut Vec<String>,
    ) -> bool {
        if let Some(help_cmd) = self.application_command_line.get_full_param("help", -1) {
            match self.display_help(help_cmd) {
                Some(text) => out.push(text),
                None => out.push(self.show_all_supported_commands()),
            }
            return true;
        }

        self.verbose = self.application_command_line.has_param("verbose");

        match self.select_command() {
            Some("process-chunks") => {
                let mut command = ProcessChunksCommand::new(self.application_command_line.clone());
                command.process(resource, saver, out)
            }
            Some("upgrade-bsp") => {
                let mut command = UpgradeBspCommand::new(self.application_command_line.clone());
                command.process(resource, saver, out)
            }
            _ => {
                if let Some(cmd) = self
                    .application_command_line
                    .get_param_by_index(1)
                    .filter(|cmd| !cmd.is_empty())
                {
                    out.push(format!(
                        "[CommandManager::run()] ERROR: Cannot recognise command {cmd}"
                    ));
                }
                out.push(self.show_all_supported_commands());
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use physics::bsp::{BSP_FILE_CUR_VERSION, BSP_FILE_LEGACY_TOKEN, BspTree, bsp_version};
    use resources::bwresource::BwResource;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("offproc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &std::path::Path, contents: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn space_settings(min_x: i32, min_y: i32, max_x: i32, max_y: i32) -> String {
        format!(
            "<root>\n <chunkSize>100</chunkSize>\n <navmeshGenerator>recast</navmeshGenerator>\n\
             \x20<bounds>\n\x20\x20<minX>{min_x}</minX>\n\x20\x20<minY>{min_y}</minY>\n\
             \x20\x20<maxX>{max_x}</maxX>\n\x20\x20<maxY>{max_y}</maxY>\n\x20</bounds>\n</root>\n"
        )
    }

    /// A one-node legacy BSP over two triangles (the same 1.x layout the
    /// bsp converter fixture uses), which the upgrade command converts to
    /// the current format.
    fn legacy_bsp() -> Vec<u8> {
        use foundation::math::Vector3;
        use physics::world_triangle::WorldTriangle;

        let triangles = [
            WorldTriangle::new(
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                0,
            ),
            WorldTriangle::new(
                Vector3::new(2.0, 0.0, 0.0),
                Vector3::new(3.0, 0.0, 0.0),
                Vector3::new(2.0, 1.0, 0.0),
                0,
            ),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(&BSP_FILE_LEGACY_TOKEN.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes()); // maxTriangles, skipped
        for tri in &triangles {
            out.extend_from_slice(&tri.to_row());
        }
        out.push(0); // flags
        out.extend_from_slice(&[0u8; 16]); // planeEq
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out
    }

    fn make_fixture(tag: &str) -> (PathBuf, BwResource) {
        let root = temp_root(tag);
        let space = root.join("spaces/lot");
        write_file(
            &space.join("space.settings"),
            space_settings(0, 0, 1, 0).as_bytes(),
        );
        write_file(
            &space.join("00000000o.chunk"),
            b"<root>\n <transformedChunk/>\n</root>\n",
        );
        write_file(&space.join("00000000o.cdata"), &[]);
        write_file(&space.join("00010000o.chunk"), b"<root>\n</root>\n");
        // An internal chunk one directory down.
        write_file(&space.join("room/00000000i.chunk"), b"<root>\n</root>\n");
        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        (root, resource)
    }

    fn line_from(args: &[&str]) -> CommandLine {
        CommandLine::from_params(args)
    }

    #[derive(Default)]
    struct CountingSaver {
        calls: usize,
        fail: bool,
        last_chunks: BTreeSet<String>,
    }

    impl ChunkSaveHook for CountingSaver {
        fn save_chunks(&mut self, _resource: &BwResource, chunks: &BTreeSet<String>) -> bool {
            self.calls += 1;
            self.last_chunks = chunks.clone();
            !self.fail
        }
    }

    /// CommandLine: quoting, switches, full-param positions, out-of-range
    /// NULLs and the trailing-separator empty parameter.
    #[test]
    fn command_line_splits_and_resolves() {
        let line = CommandLine::new(
            "offline_processor process-chunks -space \"spaces/my space\" -overwrite",
        );
        assert_eq!(line.size(), 5);
        assert_eq!(line.param(0), "offline_processor");
        assert_eq!(line.get_param_by_index(1), Some("process-chunks"));
        assert_eq!(line.get_param_by_index(0), None);
        assert_eq!(line.get_param_by_index(9), None);
        assert!(line.has_param("space"));
        // The param never carries the switch char itself.
        assert!(!line.has_param("/space"));
        assert!(!line.has_param("-space"));
        assert!(line.has_param("overwrite"));
        assert_eq!(line.get_param("space"), "spaces/my space");
        // A trailing switch resolves to "" (the reference's "", not NULL).
        assert_eq!(line.get_param("overwrite"), "");

        // hasFullParam(name, 1) selects the command slot.
        assert!(line.has_full_param("process-chunks", 1));
        assert!(!line.has_full_param("upgrade-bsp", 1));
        assert_eq!(line.get_full_param("process-chunks", 1), Some("-space"));
        assert_eq!(line.get_full_param("nosuch", 1), None);

        // The search form walks any position.
        let help = CommandLine::new("offline_processor help process-chunks");
        assert_eq!(help.get_full_param("help", -1), Some("process-chunks"));
        let tail = CommandLine::new("app -space");
        // getFullParam matches whole tokens; the switch form is not a hit.
        assert_eq!(tail.get_full_param("space", -1), None);
        assert_eq!(tail.get_param("space"), "");
        // A trailing separator yields an empty parameter, like the
        // reference's unconditional push.
        assert_eq!(CommandLine::new("a ").size(), 2);
        assert_eq!(CommandLine::new("a ").param(1), "");

        assert!(CommandLine::has_switch("-recursive"));
        assert!(CommandLine::has_switch("/nogui"));
        assert!(!CommandLine::has_switch("spaces/lot"));
        assert!(!CommandLine::has_switch("-"));
    }

    /// bw_hash: deterministic, byte-order sensitive, inside one byte;
    /// the empty string lands on the initial result.
    #[test]
    fn bw_hash_shards_stably() {
        let a = bw_hash("00000000o");
        assert!(a <= 255);
        assert_eq!(a, bw_hash("00000000o"));
        assert_ne!(bw_hash("00000001o"), bw_hash("00010000o"));
        // result starts at (123 + 0) % 256 = 123 and the loop never runs.
        assert_eq!(bw_hash(""), 123);
        // Shard membership: every chunk lands in exactly one index.
        for size in [1u32, 3, 7] {
            for index in 0..size {
                assert_eq!(
                    is_chunk_editable("00000000o", size, index),
                    bw_hash("00000000o") % size == index
                );
            }
        }
    }

    /// The ChunkFormat identifier flavour and the reverse parse.
    #[test]
    fn chunk_identifiers_round_trip() {
        assert_eq!(outside_chunk_identifier(0, 0, false), "00000000o");
        assert_eq!(outside_chunk_identifier(2, 2, false), "00020002o");
        // Level three fires at grid 16 ("%03xx%03xx/sep/").
        assert_eq!(
            outside_chunk_identifier(16, 0, false),
            "001x000x/sep/00100000o"
        );
        // All three levels at grid x 4096 (z stays in the plain window).
        assert_eq!(
            outside_chunk_identifier(4096, 0, false),
            "1xxx0xxx/sep/10xx00xx/sep/100x000x/sep/10000000o"
        );
        // A z grid of 20 trips only the third level.
        assert_eq!(
            outside_chunk_identifier(4096, 20, false),
            "1xxx0xxx/sep/10xx00xx/sep/100x001x/sep/10000014o"
        );
        // Negatives truncate into the u16 top.
        assert_eq!(outside_chunk_identifier(-1, -1, false), "ffffffffo");
        // singleDir skips every sep level.
        assert_eq!(outside_chunk_identifier(4096, 20, true), "10000014o");

        // The reverse parse (and its rejection paths).
        assert_eq!(
            grid_from_chunk_name("001x000x/sep/00100000o"),
            Some((16, 0))
        );
        assert_eq!(grid_from_chunk_name("ffffffffo"), Some((-1, -1)));
        assert_eq!(grid_from_chunk_name("0010000o"), None);
        assert_eq!(grid_from_chunk_name("no-chunk-here"), None);
        assert_eq!(grid_from_chunk_name("zzzzzzzzo"), None);
    }

    /// The mapping: settings read, bounds-checked identifiers and the
    /// internal/outside chunk gathering.
    #[test]
    fn geometry_mapping_gathers_chunks() {
        let (_root, resource) = make_fixture("mapping");
        let mapping = GeometryMapping::open(&resource, "spaces/lot").unwrap();

        assert_eq!(mapping.path(), "spaces/lot/");
        assert_eq!(mapping.chunk_size(), 100.0);
        assert_eq!(mapping.navmesh_generator(), "recast");
        assert!(!mapping.single_dir());
        assert!(mapping.in_local_bounds(1, 0));
        assert!(!mapping.in_local_bounds(2, 0));
        assert_eq!(mapping.outside_chunk_identifier(2, 0), "");
        assert_eq!(mapping.outside_chunk_identifier(0, 0), "00000000o");
        assert!(mapping.chunk_file_exists(&resource, "00000000o"));
        assert!(!mapping.chunk_file_exists(&resource, "00020000o"));

        let chunks = mapping.gather_chunks(&resource);
        assert!(chunks.contains("00000000o"));
        assert!(chunks.contains("00010000o"));
        assert!(chunks.contains("room/00000000i"));
        // The grid window is 0..1 × 0..0 — exactly two outside chunks.
        assert_eq!(chunks.iter().filter(|c| c.ends_with('o')).count(), 2);

        // A missing space.settings opens nothing.
        assert!(GeometryMapping::open(&resource, "spaces/nowhere").is_none());
    }

    /// The clean-flags block: pack/unpack and the cdata child read/write.
    #[test]
    fn clean_flags_keep_the_dirtyflags_block() {
        let flags = ChunkCleanFlags {
            thumbnail: 7,
            shadow: 0x1020_3040,
            terrain_lod: 1,
        };
        assert_eq!(flags.pack().len(), 12);
        assert_eq!(ChunkCleanFlags::unpack(&flags.pack()), Some(flags));
        assert_eq!(ChunkCleanFlags::unpack(&[0u8; 11]), None);

        let mut cdata = BinSection::new("00000000o", BinaryBlock::default());
        assert_eq!(
            ChunkCleanFlags::from_cdata(&mut cdata),
            ChunkCleanFlags::default()
        );
        flags.save_to(&mut cdata);
        assert_eq!(ChunkCleanFlags::from_cdata(&mut cdata), flags);
    }

    /// The invalidation: navmesh-only keeps the flags absent, full
    /// invalidation zeroes them, a missing cdata is skipped and a cancel
    /// between chunks aborts with false.
    #[test]
    fn invalidate_writes_consumable_cdata() {
        let (root, resource) = make_fixture("invalidate");
        let mapping = GeometryMapping::open(&resource, "spaces/lot").unwrap();

        assert!(invalidate_all_chunks(
            &resource,
            &mapping,
            1,
            0,
            true,
            &|| false
        ));

        let raw = fs::read(root.join("spaces/lot/00000000o.cdata")).unwrap();
        let mut cdata = BinSection::new("00000000o", BinaryBlock::new(raw));
        assert_eq!(cdata.read_binary("navmeshDirty").unwrap().data(), &[1]);
        // navmesh-only: the flags child stays absent.
        assert!(cdata.read_binary(CLEAN_FLAGS_NAME).is_none());

        // Full invalidation zeroes the flags block.
        assert!(invalidate_all_chunks(
            &resource,
            &mapping,
            1,
            0,
            false,
            &|| false
        ));
        let raw = fs::read(root.join("spaces/lot/00000000o.cdata")).unwrap();
        let mut cdata = BinSection::new("00000000o", BinaryBlock::new(raw));
        assert_eq!(
            ChunkCleanFlags::unpack(cdata.read_binary(CLEAN_FLAGS_NAME).unwrap().data()),
            Some(ChunkCleanFlags::default())
        );

        // The sharding skips chunks outside the cluster's residue: with
        // size 2 and index 1, only the residue-1 half is rewritten.
        let touched = fs::metadata(root.join("spaces/lot/00000000o.cdata"))
            .unwrap()
            .modified()
            .unwrap();
        let shard = bw_hash("00000000o") % 2;
        let _ = invalidate_all_chunks(&resource, &mapping, 2, 1 - shard, true, &|| false);
        assert_eq!(
            fs::metadata(root.join("spaces/lot/00000000o.cdata"))
                .unwrap()
                .modified()
                .unwrap(),
            touched
        );

        // A cancel on the first editable chunk aborts with false.
        assert!(!invalidate_all_chunks(
            &resource,
            &mapping,
            1,
            0,
            true,
            &|| true
        ));
    }

    /// The chunk save hook receives the gathered set and gates the exit
    /// code; a missing generator refuses but still "succeeds" (the
    /// reference's `return 1`), and a missing space fails.
    #[test]
    fn process_chunks_command_runs_the_workflow() {
        let (_root, mut resource) = make_fixture("workflow");
        let mut saver = CountingSaver::default();

        let mut command = ProcessChunksCommand::new(line_from(&[
            "offline_processor",
            "process-chunks",
            "-space",
            "spaces/lot",
            "-nogui",
        ]));

        let mut out = Vec::new();
        assert!(command.process(&mut resource, &mut saver, &mut out));
        assert!(out.is_empty());
        assert_eq!(saver.calls, 1);
        assert_eq!(saver.last_chunks.len(), 3);

        // A failed save answers false.
        saver.fail = true;
        let mut out = Vec::new();
        assert!(!command.process(&mut resource, &mut saver, &mut out));

        // No -space: the error line and a failure.
        let mut command = ProcessChunksCommand::new(line_from(&[
            "offline_processor",
            "process-chunks",
            "-nogui",
        ]));
        let mut out = Vec::new();
        assert!(!command.process(&mut resource, &mut saver, &mut out));
        assert_eq!(
            out.last().unwrap(),
            "You have to specify a space using /space"
        );

        // Unknown space directory.
        let mut command = ProcessChunksCommand::new(line_from(&[
            "offline_processor",
            "process-chunks",
            "-space",
            "spaces/gone",
            "-nogui",
        ]));
        let mut out = Vec::new();
        assert!(!command.process(&mut resource, &mut saver, &mut out));
        assert_eq!(out.last().unwrap(), "Cannot open space spaces/gone");

        // A non-recast generator: message-exact refusal that reports
        // success.
        let (root, mut resource) = make_fixture("badgen");
        write_file(
            &root.join("spaces/lot/space.settings"),
            b"<root>\n <navmeshGenerator>legacy</navmeshGenerator>\n</root>\n",
        );
        resource.purge("spaces/lot/space.settings", true);
        let mut command = ProcessChunksCommand::new(line_from(&[
            "offline_processor",
            "process-chunks",
            "-space",
            "spaces/lot",
            "-nogui",
        ]));
        let mut out = Vec::new();
        assert!(command.process(&mut resource, &mut saver, &mut out));
        assert_eq!(
            out.last().unwrap(),
            "The navmeshGenerator specified in space.settings is not supported.\
             offline_processor supports \"recast\" only."
        );
        assert_eq!(saver.calls, 2);

        // -overwrite runs the invalidation before the save.
        let (_root, mut resource) = make_fixture("overwrite");
        saver.fail = false;
        let mut command = ProcessChunksCommand::new(line_from(&[
            "offline_processor",
            "process-chunks",
            "-space",
            "spaces/lot",
            "-nogui",
            "-overwrite",
        ]));
        let mut out = Vec::new();
        assert!(command.process(&mut resource, &mut saver, &mut out));
        assert_eq!(saver.calls, 3);

        // The cluster params quirk: values swap (clusterSize from
        // "cluster-index", clusterIndex clamped from "cluster-size").
        let mut command = ProcessChunksCommand::new(line_from(&[
            "offline_processor",
            "process-chunks",
            "-cluster-index",
            "4",
            "-cluster-size",
            "2",
            "-space",
            "spaces/lot",
            "-nogui",
        ]));
        let mut out = Vec::new();
        assert!(command.process(&mut resource, &mut saver, &mut out));
    }

    /// The BSP upgrade command: legacy files upgrade to the current
    /// format, the primitives container keeps its siblings, unsupported
    /// extensions count neither processed files nor errors, and the
    /// counters and error lines come out message-exact.
    #[test]
    fn upgrade_bsp_command_upgrades_and_walks() {
        let (root, mut resource) = make_fixture("bsp");
        let space = root.join("spaces/lot");

        // A legacy standalone .bsp2 at the space root.
        write_file(&space.join("00000000o.bsp2"), &legacy_bsp());

        let mut command = UpgradeBspCommand::new(line_from(&[
            "offline_processor",
            "upgrade-bsp",
            "spaces/lot/00000000o.bsp2",
        ]));
        let mut saver = NoopChunkSaver;
        let mut out = Vec::new();
        assert!(command.process(&mut resource, &mut saver, &mut out));
        assert!(out.contains(&"regenerating bsp in spaces/lot/00000000o.bsp2".to_string()));
        assert!(out.contains(&"1 files processed.".to_string()));
        assert!(out.contains(&"0 error (s).".to_string()));
        let raw = fs::read(space.join("00000000o.bsp2")).unwrap();
        assert_eq!(bsp_version(&raw), BSP_FILE_CUR_VERSION);
        assert!(BspTree::load(&raw).is_ok());

        // Unsupported extensions report zero processed files and no
        // errors.
        write_file(&space.join("readme.txt"), b"hi");
        let mut command = UpgradeBspCommand::new(line_from(&[
            "offline_processor",
            "upgrade-bsp",
            "spaces/lot/readme.txt",
        ]));
        let mut out = Vec::new();
        assert!(command.process(&mut resource, &mut saver, &mut out));
        assert!(out.contains(&"0 files processed.".to_string()));
        assert!(out.contains(&"0 error (s).".to_string()));

        // A missing path fails with the exact error line.
        let mut command = UpgradeBspCommand::new(line_from(&[
            "offline_processor",
            "upgrade-bsp",
            "spaces/nowhere",
        ]));
        let mut out = Vec::new();
        assert!(!command.process(&mut resource, &mut saver, &mut out));
        assert!(
            out.iter()
                .any(|line| line == "[OfflineProcessor] ERROR: Can' find path spaces/nowhere.")
        );
        assert!(
            out.iter()
                .any(|line| line.contains("cannot find file spaces/nowhere"))
        );

        // No path at all: the detailed help and a failure.
        let mut command = UpgradeBspCommand::new(line_from(&["offline_processor", "upgrade-bsp"]));
        let mut out = Vec::new();
        assert!(!command.process(&mut resource, &mut saver, &mut out));
        assert_eq!(out.last().unwrap(), &command.show_detailed_help());
    }

    /// The batch walk: a directory without -recursive only counts the
    /// top-level files (and hints at -recursive), with -recursive it
    /// descends and upgrades the .bsp2 it finds.
    #[test]
    fn upgrade_bsp_batch_walk() {
        let (root, mut resource) = make_fixture("walk");
        let space = root.join("spaces/lot");
        write_file(&space.join("wall.bsp2"), &legacy_bsp());

        // Recursive walk over the space directory.
        let mut command = UpgradeBspCommand::new(line_from(&[
            "offline_processor",
            "upgrade-bsp",
            "spaces/lot",
            "-recursive",
        ]));
        let mut saver = NoopChunkSaver;
        let mut out = Vec::new();
        assert!(command.process(&mut resource, &mut saver, &mut out));
        assert!(out.contains(&"regenerating bsp in spaces/lot/wall.bsp2".to_string()));
        let raw = fs::read(space.join("wall.bsp2")).unwrap();
        assert_eq!(bsp_version(&raw), BSP_FILE_CUR_VERSION);

        // Without -recursive the directory lists but no .bsp2 sits at the
        // top level (only .chunk/.cdata/.settings/.txt/.bsp2 from before
        // — wall.bsp2 IS top-level, so it is upgraded again).
        write_file(&space.join("wall.bsp2"), &legacy_bsp());
        let mut command = UpgradeBspCommand::new(line_from(&[
            "offline_processor",
            "upgrade-bsp",
            "spaces/lot",
        ]));
        let mut out = Vec::new();
        assert!(command.process(&mut resource, &mut saver, &mut out));
        assert!(out.contains(&"regenerating bsp in spaces/lot/wall.bsp2".to_string()));
    }

    /// The manager: command selection, help surfaces, unknown-command
    /// error and dispatch.
    #[test]
    fn command_manager_selects_and_reports() {
        let (root, mut resource) = make_fixture("manager");
        write_file(&root.join("spaces/lot/00000000o.bsp2"), &legacy_bsp());

        assert_eq!(
            CommandManager::new("offline_processor process-chunks -nogui").select_command(),
            Some("process-chunks")
        );
        assert_eq!(
            CommandManager::new("offline_processor upgrade-bsp x").select_command(),
            Some("upgrade-bsp")
        );
        assert_eq!(
            CommandManager::new("offline_processor").select_command(),
            None
        );

        // Help surfaces, message-exact.
        let mut manager = CommandManager::new("offline_processor help upgrade-bsp");
        let mut out = Vec::new();
        assert!(manager.run(&mut resource, &mut NoopChunkSaver, &mut out));
        assert_eq!(
            out.last().unwrap(),
            "Usage: offline_processor upgrade-bsp [file] [options]\n\
             Loads and saves bsp data upgrading it to the latest format:\n\
             [file] name can be a folder name\n\
             Options:\n\
             \t-recursive\t\tProcess all files recursively starting from the given folder.\n"
        );

        // Unknown help topic falls back to the command list.
        let mut manager = CommandManager::new("offline_processor help wat");
        let mut out = Vec::new();
        assert!(manager.run(&mut resource, &mut NoopChunkSaver, &mut out));
        assert_eq!(out.last().unwrap(), &manager.show_all_supported_commands());
        assert!(
            manager
                .show_all_supported_commands()
                .starts_with("BigWorld offline processor\n\n")
        );

        // An unrecognised command.
        let mut manager = CommandManager::new("offline_processor transmogrify");
        let mut out = Vec::new();
        assert!(!manager.run(&mut resource, &mut NoopChunkSaver, &mut out));
        assert!(
            out.iter().any(|line| line
                == "[CommandManager::run()] ERROR: Cannot recognise command transmogrify")
        );

        // Dispatch runs the command end to end.
        let mut manager =
            CommandManager::new("offline_processor upgrade-bsp spaces/lot/00000000o.bsp2");
        let mut out = Vec::new();
        assert!(manager.run(&mut resource, &mut NoopChunkSaver, &mut out));
        assert!(out.contains(&"regenerating bsp in spaces/lot/00000000o.bsp2".to_string()));
    }
}
