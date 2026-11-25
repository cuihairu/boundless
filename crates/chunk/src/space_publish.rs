//! BW-13-03-06: resource browsing and space publishing.
//!
//! The browse face is the `IChunkWalker` family
//! (`tools/worldeditor/project/chunk_walker.cpp` + `space_helpers.hpp`):
//! the four ways WorldEditor enumerates a space's chunks — linear raster,
//! a bounded rect, a cached coordinate set and a thumbnail file scan.
//! The publish face is `SpaceRecreator` (`tools/worldeditor/terrain/
//! space_recreator.cpp`) plus `SpaceSettings`
//! (`tools/worldeditor/import/space_settings.cpp`) and the
//! `Terrain::TerrainSettings` save face: write the space directory, the
//! `space.settings` (bounds + chunk size + `<terrain>`), `locations.xml`
//! and one virgin terrain chunk per grid cell. The recreator's
//! source-rescale bulk (overlapper/ambient/VLO/station import and the
//! `fixChunks` pass) belongs to faces this tree does not carry — the port
//! publishes the virgin face, where every created chunk is outside the
//! source space bounds and takes the blank-texture under layer.

use crate::identifier::{grid_from_chunk_name, outside_chunk_identifier};
use crate::mapping::{DEFAULT_GRID_SIZE, GeometryMapping};
use resources::datasection::DataSection;
use resources::file_system::FileSystem;
use resources::zip::FileType;
use resources::zip::ZipFileSystem;
use std::collections::HashSet;
use std::rc::Rc;
use terrain::{EditorTerrainBlock2, TerrainSettings};

/// `SPACE_SETTING_FILE_NAME` (`worldeditor/config.hpp`).
pub const SPACE_SETTING_FILE_NAME: &str = "space.settings";

/// `s_spaceRecreatorBlankTexture` (`space_recreator.cpp:50`): the
/// `system/emptyTerrainTextureBmp` default.
pub const SPACE_RECREATOR_BLANK_TEXTURE: &str = "helpers/maps/aid_builder.tga";

/// `DEFAULT_GRID_RESOLUTION` (`chunk_grid_size.hpp:15`) — the same 100.0
/// the chunk crate's [`DEFAULT_GRID_SIZE`] carries.
pub const DEFAULT_GRID_RESOLUTION: f32 = DEFAULT_GRID_SIZE;

/// `SpaceInformation` (`project/space_information.hpp`): the space name,
/// the biased-to-world grid offset and the grid extent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpaceInformation {
    pub space_name: String,
    /// `localToWorld_`: the biased (0..width) grid origin in world grid
    /// coordinates.
    pub local_to_world: (i16, i16),
    pub grid_width: u16,
    pub grid_height: u16,
}

/// `offsetGrid` (`space_helpers.hpp:24-31`): a biased coordinate (0..width)
/// becomes a chunk-style grid coordinate.
#[must_use]
pub fn offset_grid(local_to_world: (i16, i16), x: u16, z: u16) -> (i16, i16) {
    (x as i16 + local_to_world.0, z as i16 + local_to_world.1)
}

/// `biasGrid` (`space_helpers.hpp:38-48`): the inverse of
/// [`offset_grid`]. The reference asserts the biased values are positive;
/// the port wraps like the `uint16` arithmetic would.
#[must_use]
pub fn bias_grid(local_to_world: (i16, i16), x: i16, z: i16) -> (u16, u16) {
    (
        x.wrapping_sub(local_to_world.0) as u16,
        z.wrapping_sub(local_to_world.1) as u16,
    )
}

/// `chunkID` (`space_helpers.hpp:52-67`): the chunk identifier for a grid
/// coordinate — the centre-of-cell position fed through
/// `GeometryMapping::outsideChunkIdentifier`, empty when `check_bounds`
/// and the cell is outside the local bounds.
#[must_use]
pub fn chunk_id(mapping: &GeometryMapping, grid_x: i16, grid_z: i16, check_bounds: bool) -> String {
    mapping.outside_chunk_identifier(i32::from(grid_x), i32::from(grid_z), check_bounds)
}

/// `gridFromThumbnailFileName` (`space_helpers.hpp:52-79`): a `.cdata`
/// entry name is a chunk thumbnail when it is at least
/// `"xxxxxxxo.cdata"` long and carries the outside `o` seven bytes from
/// the end; a stray `space.thumbnail.dds` fails the check. The chunk name
/// keeps its `o`, which [`grid_from_chunk_name`] consumes.
#[must_use]
pub fn grid_from_thumbnail_file_name(file_name: &str) -> Option<(i16, i16)> {
    let bytes = file_name.as_bytes();
    if bytes.len() >= 15 && bytes[bytes.len() - 7] == b'o' {
        let chunk_name = &file_name[..file_name.len() - 6];
        return grid_from_chunk_name(chunk_name);
    }
    None
}

/// `IChunkWalker` (`chunk_walker.hpp:16-22`): chunk names and grid
/// positions in any order, one tile per [`ChunkWalker::next_tile`]. The
/// reference reads the geometry mapping through the WorldEditor
/// singleton inside `chunkID`; the port hands it in with the space
/// information.
pub trait ChunkWalker {
    /// `reset()`.
    fn reset(&mut self);
    /// `spaceInformation(info)`: most walkers restart from the new space.
    fn space_information(&mut self, info: SpaceInformation, mapping: &GeometryMapping);
    /// `nextTile()` — the chunk identifier plus its world grid coordinate,
    /// `None` when the walk is done.
    fn next_tile(&mut self) -> Option<(String, i16, i16)>;
}

/// `LinearChunkWalker` (`chunk_walker.cpp:13-92`): every tile of the
/// space in raster order, x fastest. `load`/`save` park the cursor in a
/// settings section as `nextGridX`/`nextGridZ`.
#[derive(Debug, Clone, Default)]
pub struct LinearChunkWalker {
    info: SpaceInformation,
    mapping: Option<GeometryMapping>,
    x: u16,
    z: u16,
}

impl LinearChunkWalker {
    /// The zero constructor (`chunk_walker.cpp:13-18`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `load` (`chunk_walker.cpp:56-68`): resume from a settings section,
    /// or reset when there is none.
    pub fn load(&mut self, section: Option<&DataSection>) {
        match section {
            Some(section) => {
                self.x = read_int_at(section, "nextGridX") as u16;
                self.z = read_int_at(section, "nextGridZ") as u16;
            }
            None => self.reset(),
        }
    }

    /// `save` (`chunk_walker.cpp:71-81`).
    pub fn save(&self, section: &mut DataSection) {
        section.write_int("nextGridX", i32::from(self.x));
        section.write_int("nextGridZ", i32::from(self.z));
    }
}

impl ChunkWalker for LinearChunkWalker {
    fn reset(&mut self) {
        self.x = 0;
        self.z = 0;
    }

    fn space_information(&mut self, info: SpaceInformation, mapping: &GeometryMapping) {
        self.info = info;
        self.mapping = Some(mapping.clone());
        self.reset();
    }

    fn next_tile(&mut self) -> Option<(String, i16, i16)> {
        if self.z == self.info.grid_height {
            return None;
        }
        let (grid_x, grid_z) = offset_grid(self.info.local_to_world, self.x, self.z);
        let name = self
            .mapping
            .as_ref()
            .map_or(String::new(), |m| chunk_id(m, grid_x, grid_z, true));
        self.x += 1;
        if self.x == self.info.grid_width {
            self.x = 0;
            self.z += 1;
        }
        Some((name, grid_x, grid_z))
    }
}

/// `BoundedChunkWalker` (`chunk_walker.cpp:94-169`): the tiles in
/// `[left, bottom] -> (right, top)` (right/top exclusive), clamped into
/// the space; a rect that misses the space entirely stops the walk.
#[derive(Debug, Clone)]
pub struct BoundedChunkWalker {
    info: SpaceInformation,
    mapping: Option<GeometryMapping>,
    x: i16,
    z: i16,
    left: i16,
    bottom: i16,
    right: i16,
    top: i16,
    is_set: bool,
}

impl BoundedChunkWalker {
    /// The constructor (`chunk_walker.cpp:94-105`).
    #[must_use]
    pub fn new(left: i16, bottom: i16, right: i16, top: i16) -> Self {
        debug_assert!(right >= left);
        debug_assert!(top >= bottom);
        Self {
            info: SpaceInformation::default(),
            mapping: None,
            x: left,
            z: bottom,
            left,
            bottom,
            right,
            top,
            is_set: false,
        }
    }

    /// `stop` (`chunk_walker.cpp:143-146`): park the cursor on the top
    /// row so [`ChunkWalker::next_tile`] answers done.
    pub fn stop(&mut self) {
        self.z = self.top;
    }
}

impl ChunkWalker for BoundedChunkWalker {
    fn reset(&mut self) {
        // A walk range that misses the space bound stops the walk
        // (`chunk_walker.cpp:120-130`).
        if self.left >= i16::try_from(self.info.grid_width).unwrap_or(i16::MAX)
            || self.right <= 0
            || self.bottom >= i16::try_from(self.info.grid_height).unwrap_or(i16::MAX)
            || self.top <= 0
        {
            self.stop();
            return;
        }
        self.left = self.left.max(0);
        self.bottom = self.bottom.max(0);
        self.right = self
            .right
            .min(i16::try_from(self.info.grid_width).unwrap_or(i16::MAX));
        self.top = self
            .top
            .min(i16::try_from(self.info.grid_height).unwrap_or(i16::MAX));
        self.x = self.left;
        self.z = self.bottom;
    }

    fn space_information(&mut self, info: SpaceInformation, mapping: &GeometryMapping) {
        self.info = info;
        self.mapping = Some(mapping.clone());
        self.reset();
        self.is_set = true;
    }

    fn next_tile(&mut self) -> Option<(String, i16, i16)> {
        debug_assert!(self.is_set);
        if self.z == self.top {
            return None;
        }
        let (grid_x, grid_z) = offset_grid(self.info.local_to_world, self.x as u16, self.z as u16);
        let name = self
            .mapping
            .as_ref()
            .map_or(String::new(), |m| chunk_id(m, grid_x, grid_z, true));
        self.x += 1;
        if self.x == self.right {
            self.x = self.left;
            self.z += 1;
        }
        Some((name, grid_x, grid_z))
    }
}

/// `CacheChunkWalker` (`chunk_walker.cpp:171-430`): an explicit coordinate
/// set, walked last-pushed-first, with RLE tile-range save/load so the
/// chunk-processing progress survives a restart.
#[derive(Debug, Clone, Default)]
pub struct CacheChunkWalker {
    info: SpaceInformation,
    mapping: Option<GeometryMapping>,
    xs: Vec<i16>,
    zs: Vec<i16>,
}

impl CacheChunkWalker {
    /// The constructor (`chunk_walker.cpp:171-173`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `add(gridX, gridZ)` (`chunk_walker.cpp:223-230`).
    pub fn add(&mut self, grid_x: i16, grid_z: i16) {
        self.xs.push(grid_x);
        self.zs.push(grid_z);
    }

    /// `add(Chunk*)` (`chunk_walker.cpp:213-221`): the identifier's grid
    /// coordinate. An unparseable name adds nothing.
    pub fn add_chunk_name(&mut self, chunk_name: &str) {
        if let Some((grid_x, grid_z)) = grid_from_chunk_name(chunk_name) {
            self.add(grid_x, grid_z);
        }
    }

    /// `added(gridX, gridZ)` (`chunk_walker.cpp:242-253`).
    #[must_use]
    pub fn added(&self, grid_x: i16, grid_z: i16) -> bool {
        self.xs
            .iter()
            .zip(self.zs.iter())
            .any(|(&x, &z)| x == grid_x && z == grid_z)
    }

    /// `added(Chunk*)` (`chunk_walker.cpp:232-240`).
    #[must_use]
    pub fn chunk_name_added(&self, chunk_name: &str) -> bool {
        match grid_from_chunk_name(chunk_name) {
            Some((grid_x, grid_z)) => self.added(grid_x, grid_z),
            None => false,
        }
    }

    /// `erase(gridX, gridZ)` (`chunk_walker.cpp:265-280`): remove the
    /// first matching pair.
    pub fn erase(&mut self, grid_x: i16, grid_z: i16) -> bool {
        for i in 0..self.xs.len() {
            if self.xs[i] == grid_x && self.zs[i] == grid_z {
                self.xs.remove(i);
                self.zs.remove(i);
                return true;
            }
        }
        false
    }

    /// `erase(Chunk*)` (`chunk_walker.cpp:255-263`).
    pub fn erase_chunk_name(&mut self, chunk_name: &str) -> bool {
        match grid_from_chunk_name(chunk_name) {
            Some((grid_x, grid_z)) => self.erase(grid_x, grid_z),
            None => false,
        }
    }

    /// `save` (`chunk_walker.cpp:314-334`): every cached coordinate as an
    /// RLE tile range, plus the space geometry the loader needs. Nothing
    /// is written while the cache is empty.
    pub fn save(&self, section: &mut DataSection) {
        if self.xs.is_empty() {
            return;
        }
        let tiles: Vec<u16> = self
            .xs
            .iter()
            .zip(self.zs.iter())
            .map(|(&x, &z)| {
                let (biased_x, biased_z) = bias_grid(self.info.local_to_world, x, z);
                biased_x.wrapping_add(biased_z.wrapping_mul(self.info.grid_width))
            })
            .collect();
        self.save_tiles(section, &tiles);
    }

    /// `save(pSection, modifiedTileNumbers)`
    /// (`chunk_walker.cpp:336-379`): sort the tiles, then concatenate
    /// adjacent ones into `tile` sections carrying `min`/`max`.
    pub fn save_tiles(&self, section: &mut DataSection, modified_tiles: &[u16]) {
        let mut tiles: Vec<u16> = modified_tiles.to_vec();
        tiles.sort_unstable();

        section.write_int("minX", i32::from(self.info.local_to_world.0));
        section.write_int("minZ", i32::from(self.info.local_to_world.1));
        section.write_int("width", i32::from(self.info.grid_width));
        section.write_int("height", i32::from(self.info.grid_height));

        let mut min_range: u16 = 0xffff;
        let mut max_range: u16 = 0;

        for &tile in &tiles {
            // A tile may join the current range when it is inside it or
            // adjacent to it (`inRange` with the neighbour slack).
            let adj_min = min_range.saturating_sub(1);
            if in_range(tile, adj_min, max_range.saturating_add(1)) {
                min_range = min_range.min(tile);
                max_range = max_range.max(tile);
                continue;
            }
            write_range(section, min_range, max_range);
            min_range = tile;
            max_range = tile;
        }
        write_range(section, min_range, max_range);
    }

    /// `load` (`chunk_walker.cpp:381-417`): restore the space geometry
    /// and expand every tile range back into coordinates.
    pub fn load(&mut self, section: &DataSection) {
        self.info.local_to_world.0 = read_int_at(section, "minX") as i16;
        self.info.local_to_world.1 = read_int_at(section, "minZ") as i16;
        self.info.grid_width = read_int_at(section, "width") as u16;
        self.info.grid_height = read_int_at(section, "height") as u16;

        for i in 0..section.count_children() {
            // The reference guards a null child here
            // (`chunk_walker.cpp:389-392`); the port's `open_child`
            // answers `Some` for every in-range index.
            if let Some(child) = section.open_child(i)
                && child.open_section("min").is_some()
            {
                let min_range = read_int_at(child, "min") as u16;
                let max_range = match child.open_section("max") {
                    Some(leaf) => leaf.as_int(i32::from(min_range)) as u16,
                    None => min_range,
                };
                let mut tile = min_range;
                while tile <= max_range {
                    let x = tile % self.info.grid_width;
                    let z = tile / self.info.grid_width;
                    let (offset_x, offset_z) = offset_grid(self.info.local_to_world, x, z);
                    self.xs.push(offset_x);
                    self.zs.push(offset_z);
                    tile += 1;
                }
            }
        }
    }

    /// `size` (`chunk_walker.cpp:419-422`).
    #[must_use]
    pub fn size(&self) -> usize {
        self.xs.len()
    }
}

impl ChunkWalker for CacheChunkWalker {
    fn reset(&mut self) {
        self.xs.clear();
        self.zs.clear();
    }

    fn space_information(&mut self, info: SpaceInformation, mapping: &GeometryMapping) {
        self.info = info;
        self.mapping = Some(mapping.clone());
    }

    fn next_tile(&mut self) -> Option<(String, i16, i16)> {
        let grid_x = self.xs.pop()?;
        let grid_z = self.zs.pop()?;
        let name = self
            .mapping
            .as_ref()
            .map_or(String::new(), |m| chunk_id(m, grid_x, grid_z, true));
        // The reference asserts a non-empty name here — "cached chunk
        // coordinates which don't exist in the current geometry mapping"
        // — and still answers false on the empty name in release builds
        // (`chunk_walker.cpp:196-201`); the port keeps that release face.
        if name.is_empty() {
            return None;
        }
        Some((name, grid_x, grid_z))
    }
}

/// `inRange` (`chunk_walker.cpp:288-294`): the tile survives the
/// min/max pinch unchanged.
fn in_range(tile: u16, min_range: u16, max_range: u16) -> bool {
    tile.max(min_range).min(max_range) == tile
}

/// `writeRange` (`chunk_walker.cpp:298-312`): one `tile` section with
/// `min`, plus `max` when the range spans more than one tile.
fn write_range(section: &mut DataSection, min_range: u16, max_range: u16) {
    if min_range > max_range {
        return;
    }
    let tile = section.add_child("tile");
    tile.write_int("min", i32::from(min_range));
    if min_range != max_range {
        tile.write_int("max", i32::from(max_range));
    }
}

/// The emulated find handle: the current listing and the next index.
type FindListing = (Vec<String>, usize);

/// `FileChunkWalker` (`chunk_walker.cpp:434-730`): scan the space
/// directory tree for `.cdata` thumbnails. The Win32 `FindFirstFile`
/// quantum becomes a per-directory listing walked index-by-index with the
/// same two-phase shape (all `.cdata` files of a folder, then its
/// subfolders), keeping `numIrrelevantPerFind` as the per-call step
/// budget. The relevant-tile face is `ModifiedFileChunkWalker::isRelevant`
/// without the SpaceMap timestamp cache: a tile counts when its thumbnail
/// name parses and it sits inside the walk's space bounds.
pub struct FileChunkWalker {
    fs: Rc<dyn FileSystem>,
    info: SpaceInformation,
    mapping: Option<GeometryMapping>,
    /// `dirMap->path()`.
    space: String,
    folder: String,
    folders: Vec<String>,
    xs: Vec<i16>,
    zs: Vec<i16>,
    num_irrelevant_per_find: u16,
    searching_files: bool,
    find: Option<FindListing>,
    reentry_folder: String,
}

impl FileChunkWalker {
    /// The constructor (`chunk_walker.cpp:434-442`).
    #[must_use]
    pub fn new(fs: Rc<dyn FileSystem>, num_irrelevant_per_find: u16) -> Self {
        Self {
            fs,
            info: SpaceInformation::default(),
            mapping: None,
            space: String::new(),
            folder: String::new(),
            folders: Vec::new(),
            xs: Vec::new(),
            zs: Vec::new(),
            num_irrelevant_per_find,
            searching_files: false,
            find: None,
            reentry_folder: String::new(),
        }
    }

    /// `load` (`chunk_walker.cpp:705-716`): the interrupted search's
    /// reentry folder.
    pub fn load(&mut self, section: Option<&DataSection>) {
        self.reentry_folder = match section {
            Some(section) => section
                .open_section("reentryFolder")
                .map_or(String::new(), |leaf| leaf.as_string().to_string()),
            None => String::new(),
        };
    }

    /// `save` (`chunk_walker.cpp:718-727`).
    pub fn save(&self, section: &mut DataSection) {
        section.write_string("reentryFolder", &self.folder);
    }

    /// `isValidFolder` (`chunk_walker.cpp:668-692`): names shorter than
    /// three characters are dropped, three-character names must start
    /// with `s` (the `"sep"` levels) so `CVS` and its kin are skipped,
    /// longer names pass.
    fn is_valid_folder(&self, name: &str) -> bool {
        match name.as_bytes() {
            [_, _] | [_] | [] => false,
            [first, _, _] => *first == b's',
            _ => true,
        }
    }

    /// `isRelevant` — the `ModifiedFileChunkWalker` geometry face
    /// (`chunk_walker.cpp:782-789` bounds test).
    fn is_relevant(&self, grid_x: i16, grid_z: i16) -> bool {
        let (world_x, world_z) = (
            i32::from(self.info.local_to_world.0),
            i32::from(self.info.local_to_world.1),
        );
        let width = i32::from(self.info.grid_width);
        let height = i32::from(self.info.grid_height);
        i32::from(grid_x) >= world_x
            && i32::from(grid_z) >= world_z
            && i32::from(grid_x) < world_x + width
            && i32::from(grid_z) < world_z + height
    }

    /// `findFolders` (`chunk_walker.cpp:697-718`): push every valid
    /// subfolder of `folder` as `folder + name + "/"`.
    fn find_folders(&mut self, folder: &str) {
        let Some(entries) = self.fs.read_directory(&format!("{}{}", self.space, folder)) else {
            return;
        };
        for name in entries {
            let path = format!("{}{}{}", self.space, folder, name);
            if self.fs.get_file_type(&path, None) == FileType::Directory
                && self.is_valid_folder(&name)
            {
                self.folders.push(format!("{folder}{name}/"));
            }
        }
    }

    /// `findFolder` (`chunk_walker.cpp:630-655`): walk the folder stack
    /// until `target` surfaces, descending into partial prefix matches.
    fn find_folder(&mut self, target: &str) -> bool {
        self.find_folders("");
        while let Some(folder_name) = self.folders.pop() {
            if folder_name == target {
                self.folders.push(folder_name);
                return true;
            }
            if target.starts_with(&folder_name) {
                self.find_folders(&folder_name);
            }
        }
        false
    }

    /// `findReentryFolder` (`chunk_walker.cpp:604-616`).
    fn find_reentry_folder(&mut self) {
        let reentry = std::mem::take(&mut self.reentry_folder);
        if reentry.is_empty() {
            return;
        }
        self.find_folder(&reentry);
    }

    /// One `FindFirstFile` listing: the current folder's entries that
    /// pass `filter`, in directory order.
    fn listing(&self, filter: impl Fn(&str) -> bool) -> Vec<String> {
        self.fs
            .read_directory(&format!("{}{}", self.space, self.folder))
            .unwrap_or_default()
            .into_iter()
            .filter(|name| filter(name.as_str()))
            .collect()
    }

    /// The folder-search phase lists `*.` like the reference: every
    /// entry, filtered to directories (`FILE_ATTRIBUTE_DIRECTORY`) whose
    /// name [`FileChunkWalker::is_valid_folder`] accepts.
    fn folder_listing(&self) -> Vec<String> {
        self.listing(|name| {
            let path = format!("{}{}{}", self.space, self.folder, name);
            self.fs.get_file_type(&path, None) == FileType::Directory && self.is_valid_folder(name)
        })
    }

    /// `advanceFileHandle` (`chunk_walker.cpp:659-692` + the
    /// `FindFirstFile`/`FindNextFile` calls it wraps): produce the next
    /// entry of the current phase, opening the next folder when the
    /// handle is idle, and flip from files to folders when the file
    /// search runs out.
    fn advance_file_handle(&mut self) -> Option<String> {
        if self.find.is_none() {
            // A new search: advance the handle to the next folder.
            self.folder = self.folders.pop().unwrap_or_default();
            self.searching_files = true;
            let entries = self.listing(|name| name.ends_with(".cdata"));
            self.find = Some((entries, 0));
        } else {
            let has_next = self
                .find
                .as_ref()
                .is_some_and(|(entries, index)| *index < entries.len());
            if has_next {
                let (entries, index) = self.find.as_mut().expect("checked above");
                let name = entries[*index].clone();
                *index += 1;
                return Some(name);
            }
            // No more found in the current phase.
            if self.searching_files {
                // If we were doing a file search, begin a folder search.
                self.searching_files = false;
                let entries = self.folder_listing();
                self.find = Some((entries, 0));
            } else {
                self.find = None;
                return None;
            }
        }
        // Surface the first entry of a freshly opened listing.
        if let Some((entries, index)) = self.find.as_mut()
            && *index < entries.len()
        {
            let name = entries[*index].clone();
            *index += 1;
            return Some(name);
        }
        None
    }

    /// `findNextFile` (`chunk_walker.cpp:488-536`): burn up to
    /// `numIrrelevantPerFind` find steps; a step that surfaces an entry
    /// (a file or a folder) consumes the call — only failed handles keep
    /// looping.
    fn find_next_file(&mut self) {
        for _ in 0..self.num_irrelevant_per_find {
            let Some(name) = self.advance_file_handle() else {
                continue;
            };
            if self.searching_files {
                // One files-phase entry per call, relevant or not; the
                // Win32 `*.cdata` pattern already filtered the names.
                if let Some((grid_x, grid_z)) = grid_from_thumbnail_file_name(&name)
                    && self.is_relevant(grid_x, grid_z)
                {
                    self.xs.push(grid_x);
                    self.zs.push(grid_z);
                }
            } else {
                // The folder listing already filtered to directories
                // whose names `isValidFolder` accepts.
                self.folders.push(format!("{}{}/sep/", self.folder, name));
            }
            return;
        }
    }
}

impl ChunkWalker for FileChunkWalker {
    fn reset(&mut self) {
        self.find = None;
        self.folders.clear();
        self.folder = String::new();
        self.xs.clear();
        self.zs.clear();
    }

    fn space_information(&mut self, info: SpaceInformation, mapping: &GeometryMapping) {
        self.space = mapping.path.clone();
        self.info = info;
        self.mapping = Some(mapping.clone());
        self.reset();
    }

    fn next_tile(&mut self) -> Option<(String, i16, i16)> {
        if !self.reentry_folder.is_empty() {
            self.find_reentry_folder();
        }
        if self.xs.is_empty() {
            self.find_next_file();
        }
        let grid_x = self.xs.pop()?;
        let grid_z = self.zs.pop()?;
        let name = self
            .mapping
            .as_ref()
            .map_or(String::new(), |m| chunk_id(m, grid_x, grid_z, true));
        Some((name, grid_x, grid_z))
    }
}

fn read_int_at(section: &DataSection, path: &str) -> i32 {
    section.open_section(path).map_or(0, |leaf| leaf.as_int(0))
}

/// `TerrainUtils::terrainSize` face: the source space's grid size and its
/// chunk-index bounds (`maxX`/`maxZ` inclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceSpace {
    pub grid_size: f32,
    pub min_x: i32,
    pub min_z: i32,
    pub max_x: i32,
    pub max_z: i32,
}

/// The source space bounding box the under-layer fill consults
/// (`srcSpaceBB_`, `space_recreator.cpp:142-145`): the chunk-height Y
/// range always brackets the sample plane, so the port keeps the X/Z
/// ranges.
#[derive(Debug, Clone, Copy)]
pub struct SourceSpaceBounds {
    min_x: f32,
    max_x: f32,
    min_z: f32,
    max_z: f32,
}

impl SourceSpaceBounds {
    #[must_use]
    pub fn new(src: &SourceSpace) -> Self {
        Self {
            min_x: src.min_x as f32 * src.grid_size,
            max_x: (src.max_x + 1) as f32 * src.grid_size,
            min_z: src.min_z as f32 * src.grid_size,
            max_z: (src.max_z + 1) as f32 * src.grid_size,
        }
    }

    /// `BoundingBox::intersects(Vector3)` on the sample plane, inclusive
    /// on every face.
    #[must_use]
    pub fn contains(&self, x: f32, z: f32) -> bool {
        x >= self.min_x && x <= self.max_x && z >= self.min_z && z <= self.max_z
    }
}

/// `SpaceRecreator::TerrainInfo` (`space_recreator.hpp:31-40`): the
/// terrain block geometry the new space builds with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrainInfo {
    pub version: u32,
    pub height_map_editor_size: u32,
    pub height_map_size: u32,
    pub normal_map_size: u32,
    pub hole_map_size: u32,
    pub shadow_map_size: u32,
    pub blend_map_size: u32,
}

/// `SpaceRecreator` (`tools/worldeditor/terrain/space_recreator.cpp`):
/// publish a space — directory, `space.settings`, `locations.xml` and one
/// virgin terrain chunk per grid cell.
pub struct SpaceRecreator {
    src_space_name: String,
    space_name: String,
    chunk_size: f32,
    terrain_info: TerrainInfo,
    src_bounds: SourceSpaceBounds,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
    x_offset: i32,
    z_offset: i32,
    sub_dirs: HashSet<String>,
    error_message: String,
    terrain_settings: TerrainSettings,
}

impl SpaceRecreator {
    /// The constructor (`space_recreator.cpp:120-148`): derive the bounds
    /// from the requested chunk grid and offset the source space onto it.
    #[must_use]
    pub fn new(
        src_space_name: &str,
        space_name: &str,
        chunk_size: f32,
        width: i32,
        height: i32,
        terrain_info: TerrainInfo,
        src: SourceSpace,
    ) -> Self {
        let (min_x, max_x, min_z, max_z) = Self::calc_bounds(width, height);
        let x_offset = ((src.min_x as f32 * src.grid_size) - (min_x as f32 * chunk_size)) as i32;
        let z_offset = ((src.min_z as f32 * src.grid_size) - (min_z as f32 * chunk_size)) as i32;
        Self {
            src_space_name: src_space_name.to_string(),
            space_name: space_name.to_string(),
            chunk_size,
            terrain_info,
            src_bounds: SourceSpaceBounds::new(&src),
            min_x,
            max_x,
            min_z,
            max_z,
            x_offset,
            z_offset,
            sub_dirs: HashSet::new(),
            error_message: String::new(),
            terrain_settings: TerrainSettings::new(),
        }
    }

    /// `changingChunkSize_`.
    #[must_use]
    pub fn changing_chunk_size(&self, src: &SourceSpace) -> bool {
        src.grid_size != self.chunk_size
    }

    /// `calcBounds` (`space_recreator.cpp:238-254`): centre the space on
    /// 0,0 — even widths get one more negative row/column.
    #[must_use]
    pub fn calc_bounds(width: i32, height: i32) -> (i32, i32, i32, i32) {
        let width = width - 1;
        let max_x = width / 2;
        let min_x = max_x - width;
        let height = height - 1;
        let max_z = height / 2;
        let min_z = max_z - height;
        (min_x, max_x, min_z, max_z)
    }

    /// `chunkName` (`space_recreator.cpp:334-339`): the standard prefixed
    /// outside identifier.
    #[must_use]
    pub fn chunk_name(&self, x: i32, z: i32) -> String {
        outside_chunk_identifier(x, z, false)
    }

    /// `errorMessage`.
    #[must_use]
    pub fn error_message(&self) -> &str {
        &self.error_message
    }

    /// The `<terrain>` settings the recreate produced
    /// (`pTerrainSettings_`).
    #[must_use]
    pub fn terrain_settings(&self) -> &TerrainSettings {
        &self.terrain_settings
    }

    /// `create` → `createInternal` (`space_recreator.cpp:169-222`) with
    /// the source-import stages (`copyAssets`/`fixChunks`) left to the
    /// faces this tree does not carry: directory, settings, chunks.
    pub fn create(&mut self, file_system: &dyn FileSystem) -> bool {
        self.error_message = self.create_space_directory(file_system);
        if self.should_abort() {
            return false;
        }
        self.error_message = self.create_space_settings(file_system);
        if self.should_abort() {
            return false;
        }
        self.error_message = self.create_chunks(file_system);
        !self.should_abort()
    }

    /// `shouldAbort` (`space_recreator.cpp:226-239`) without the progress
    /// bar cancel: an error message stops the run.
    fn should_abort(&self) -> bool {
        !self.error_message.is_empty()
    }

    /// `createSpaceDirectory` (`space_recreator.cpp:256-264`): ensure the
    /// directory exists, then answer the error when it still cannot be
    /// opened.
    fn create_space_directory(&mut self, file_system: &dyn FileSystem) -> String {
        ensure_path_exists(file_system, &format!("{}/", self.space_name));
        if file_system.get_file_type(&self.space_name, None) == FileType::Directory {
            String::new()
        } else {
            "Unable to create new space directory\n".to_string()
        }
    }

    /// `createSpaceSettings` (`space_recreator.cpp:271-330`): copy the
    /// source settings, override chunk size and bounds, rebuild the
    /// `<terrain>` section from the source settings plus the terrain
    /// info, then write `locations.xml` (empty when the source has none).
    fn create_space_settings(&mut self, file_system: &dyn FileSystem) -> String {
        let mut result = String::new();

        let src_settings = file_system
            .read_file(&format!(
                "{}/{}",
                self.src_space_name, SPACE_SETTING_FILE_NAME
            ))
            .and_then(|raw| resources::datasection::parse(SPACE_SETTING_FILE_NAME, &raw).ok());

        if let Some(src) = src_settings.as_ref() {
            let mut space_settings = src.clone();
            let src_chunk_size = src
                .open_section("chunkSize")
                .map_or(terrain::DEFAULT_BLOCK_SIZE, |leaf| {
                    leaf.as_float(terrain::DEFAULT_BLOCK_SIZE)
                });
            space_settings.write_float("chunkSize", self.chunk_size);
            space_settings.write_int("bounds/minX", self.min_x);
            space_settings.write_int("bounds/maxX", self.max_x);
            space_settings.write_int("bounds/minY", self.min_z);
            space_settings.write_int("bounds/maxY", self.max_z);

            // write terrain settings: drop the source section, rebuild
            // from it with the terrain info overrides.
            let src_terrain = src.open_section("terrain");
            space_settings.delete_sections("terrain");
            {
                let mut settings = TerrainSettings::new();
                settings.init(self.chunk_size, src_terrain, src_chunk_size);
                settings.set_version(self.terrain_info.version);
                settings.set_height_map_editor_size(self.terrain_info.height_map_editor_size);
                settings.set_height_map_size(self.terrain_info.height_map_size);
                settings.set_normal_map_size(self.terrain_info.normal_map_size);
                settings.set_hole_map_size(self.terrain_info.hole_map_size);
                settings.set_shadow_map_size(self.terrain_info.shadow_map_size);
                settings.set_blend_map_size(self.terrain_info.blend_map_size);
                let terrain_section = space_settings.add_child("terrain");
                settings.save(terrain_section);
                self.terrain_settings = settings;
            }

            let xml = space_settings.write_to_string();
            let settings_path = format!("{}/{}", self.space_name, SPACE_SETTING_FILE_NAME);
            if !file_system.write_file(&settings_path, xml.as_bytes()) {
                result += &format!("Unable to save {SPACE_SETTING_FILE_NAME}\n");
            }
        } else {
            // The recreator publishes the space the editor has open — a
            // missing source settings section fails the copy, exactly as
            // in the reference (`space_recreator.cpp:323-326`).
            result += &format!("Unable to copy {SPACE_SETTING_FILE_NAME}\n");
        }

        // srcLocations might not exist, that's fine.
        let src_locations = file_system
            .read_file(&format!("{}/{}", self.src_space_name, "locations.xml"))
            .and_then(|raw| resources::datasection::parse("locations.xml", &raw).ok());
        let mut locations = src_locations.unwrap_or_else(|| DataSection::new("locations.xml"));
        let xml = locations.write_to_string();
        let locations_path = format!("{}/{}", self.space_name, "locations.xml");
        if !file_system.write_file(&locations_path, xml.as_bytes()) {
            result += "Unable to save locations.xml\n";
        }

        result
    }

    /// `createChunks` (`space_recreator.cpp:1083-1118`): walk the grid
    /// column-major, accumulate one error per failed chunk, and stop at
    /// the first failure. The per-column `BWResource::purgeAll` is a
    /// section-cache face the port's direct file writes do not need.
    fn create_chunks(&mut self, file_system: &dyn FileSystem) -> String {
        let mut result = String::new();
        self.sub_dirs.clear();
        let mut x = self.min_x;
        while x <= self.max_x && result.is_empty() {
            let mut z = self.min_z;
            while z <= self.max_z && result.is_empty() {
                if !self.create_chunk(file_system, x, z) {
                    result += &format!("Unable to create chunk {}\n", self.chunk_name(x, z));
                }
                z += 1;
            }
            x += 1;
        }
        result
    }

    /// `createChunk` (`space_recreator.cpp:464-1079`), virgin face: the
    /// source space contributes no overlapping chunks here, so every
    /// chunk is virgin — one terrain block with the blank-texture under
    /// layer filled wherever the sample plane sits outside the source
    /// bounds, saved to the chunk's `.cdata`, and referenced from a
    /// `<terrain>` section of the chunk XML. The port's
    /// `insertTextureLayer` is infallible, so the `-1` guard of the
    /// reference has no branch here.
    fn create_chunk(&mut self, file_system: &dyn FileSystem, x: i32, z: i32) -> bool {
        let name = self.chunk_name(x, z);

        if let Some(pos) = name.rfind('/') {
            let path = &name[..pos + 1];
            if self.sub_dirs.insert(path.to_string()) {
                ensure_path_exists(file_system, &format!("{}/{}", self.space_name, path));
            }
        }

        let chunk_min_x = x as f32 * self.chunk_size + self.x_offset as f32;
        let chunk_min_z = z as f32 * self.chunk_size + self.z_offset as f32;

        let mut chunk_ds = DataSection::new("root");
        let mut cdata = ZipFileSystem::open("", ZipFileSystem::empty_archive())
            .expect("an empty archive always parses");

        // Create a terrain block for this chunk; a virgin chunk keeps the
        // under layer, so `wroteToUnderLayer` is true whenever any sample
        // lands outside the source bounds.
        let blend_size = self.terrain_settings.blend_map_size();
        let mut block = match EditorTerrainBlock2::create(
            self.chunk_size,
            self.terrain_info.height_map_editor_size,
            self.terrain_info.hole_map_size,
            blend_size,
            self.terrain_settings.num_vertex_lods(),
        ) {
            Ok(block) => block,
            Err(_) => return false,
        };
        let under_layer_index = block.insert_texture_layer();
        let mut wrote_to_under_layer = false;
        {
            let layer = block.texture_layer_mut(under_layer_index);
            layer.set_texture_name(SPACE_RECREATOR_BLANK_TEXTURE);
            let inverse = 1.0 / self.chunk_size;
            layer.set_projections([inverse, 0.0, 0.0, 0.0], [0.0, 0.0, -inverse, 0.0]);
            // Fill in any terrain that's outside our srcSpaceBB: sample
            // the layer pixel centres against the source bounds
            // (`space_recreator.cpp:551-572`).
            let width = i32::try_from(layer.blends().width()).unwrap_or(0);
            let height = i32::try_from(layer.blends().height()).unwrap_or(0);
            let spacing_x = self.chunk_size / width as f32;
            let half_spacing_x = spacing_x / 2.0;
            let spacing_z = self.chunk_size / height as f32;
            let half_spacing_z = spacing_z / 2.0;
            for px in 0..width {
                let world_x = chunk_min_x + px as f32 * spacing_x + half_spacing_x;
                for pz in 0..height {
                    let world_z = chunk_min_z + pz as f32 * spacing_z + half_spacing_z;
                    if !self.src_bounds.contains(world_x, world_z) {
                        layer.set_blend(px, pz, u8::MAX);
                        wrote_to_under_layer = true;
                    }
                }
            }
        }

        // A virgin chunk has no source items to import, so the walker
        // loop over overlapping source chunks finds nothing here.

        let mut good = true;

        if !wrote_to_under_layer {
            block.remove_texture_layer(under_layer_index);
        }

        if block.number_texture_layers() > 0 {
            // Finish terrain texturing and write it to .cdata. The
            // renderer tails (deleteEmptyLayers/optimiseLayers/
            // rebuildNormalMap/rebuildLodTexture) belong to faces this
            // tree does not carry.
            normalise_terrain_block_blends(&mut block);
            good &= block.save_to_cdata(&mut cdata);

            // `hasTerrain || virginChunk`: a virgin chunk always writes
            // the terrain reference.
            let terrain_section = chunk_ds.add_child("terrain");
            good &= terrain::write_terrain_section(terrain_section, &name);
        }

        // Write out the chunk files.
        let chunk_path = format!("{}/{}.chunk", self.space_name, name);
        good &= file_system.write_file(&chunk_path, chunk_ds.write_to_string().as_bytes());
        let cdata_path = format!("{}/{}.cdata", self.space_name, name);
        good &= file_system.write_file(&cdata_path, &cdata.create_zip());

        good
    }
}

/// `BWResource::ensurePathExists`: create every missing component of the
/// path; failures are the caller's problem, as in the reference.
fn ensure_path_exists(file_system: &dyn FileSystem, path: &str) {
    let mut walked = String::new();
    for component in path.split('/') {
        if component.is_empty() {
            continue;
        }
        walked.push_str(component);
        walked.push('/');
        if file_system.get_file_type(&walked, None) != FileType::Directory {
            file_system.make_directory(&walked);
        }
    }
}

/// `SpaceRecreator::normaliseTerrainBlockBlends`
/// (`space_recreator.cpp:340-462`): fixed-point repaint of the layer
/// blends so every pixel sums to full strength. A zero pixel cuts a hole
/// and full-blends the lowest layer; a full pixel is left alone; anything
/// else is rescaled across the non-zero layers with the rounding leftover
/// given to the strongest layer.
pub fn normalise_terrain_block_blends(block: &mut EditorTerrainBlock2) -> bool {
    let mut modified = false;

    let num_layers = block.number_texture_layers();
    if num_layers == 0 {
        // MF_ASSERT in the reference; the port answers unmodified.
        return false;
    }

    // Fixed-point paint constants (`space_recreator.cpp:349-352`).
    const STRENGTH_EXP: u32 = 20;
    const HALF_STRENGTH: i64 = 1 << (STRENGTH_EXP - 1);

    // The reference wraps the hole map in a read-write holder for the
    // pass (`space_recreator.cpp:367-369`).
    block.hole_map_mut().lock(false);

    let height = i32::try_from(block.texture_layer(0).blends().height()).unwrap_or(0);
    let width = i32::try_from(block.texture_layer(0).blends().width()).unwrap_or(0);
    let hole_width = block.hole_map().image().width();
    let hole_height = block.hole_map().image().height();
    let hole_factor_x = hole_width as f32 / width as f32;
    let hole_factor_y = hole_height as f32 / height as f32;

    for y in 0..height {
        for x in 0..width {
            let mut sum: i32 = 0;
            for layer in 0..num_layers {
                sum += i32::from(block.texture_layer(layer).blends().get(x, y));
            }

            if sum == 0 {
                // There's no blends at this point, so cut a hole instead.
                let hole_x_start = (hole_factor_x * x as f32).floor() as i32;
                let mut hole_x_end = (hole_factor_x * (x + 1) as f32).floor() as i32;
                let hole_y_start = (hole_factor_y * y as f32).floor() as i32;
                let mut hole_y_end = (hole_factor_y * (y + 1) as f32).floor() as i32;
                if hole_x_end == hole_x_start {
                    hole_x_end += 1;
                }
                if hole_y_end == hole_y_start {
                    hole_y_end += 1;
                }
                for hole_x in hole_x_start..hole_x_end {
                    for hole_y in hole_y_start..hole_y_end {
                        block.hole_map_mut().image_mut().set(hole_x, hole_y, true);
                    }
                }
                // Full-blend the lowest layer.
                block.texture_layer_mut(0).set_blend(x, y, u8::MAX);
                continue;
            }

            if sum == 255 {
                continue;
            }

            modified = true;

            // Spread the difference across the non-zero layers, leftover
            // to the strongest one (`space_recreator.cpp:407-447`).
            let mut max_layer = usize::MAX;
            let mut max_value: i32 = 0;
            let ratio = i64::from((255u32 << STRENGTH_EXP) / sum as u32);
            for layer in 0..num_layers {
                let value = i32::from(block.texture_layer(layer).blends().get(x, y));
                if value == 0 {
                    continue;
                }
                if value > max_value {
                    max_value = value;
                    max_layer = layer;
                }
                sum -= value;
                let scaled = ((i64::from(value) * ratio + HALF_STRENGTH) >> STRENGTH_EXP) as u8;
                sum += i32::from(scaled);
                block.texture_layer_mut(layer).set_blend(x, y, scaled);
            }
            debug_assert!(max_layer != usize::MAX);
            // Remember, this is uint8 math — the correction truncates.
            let current = block.texture_layer(max_layer).blends().get(x, y);
            if sum > 255 {
                block.texture_layer_mut(max_layer).set_blend(
                    x,
                    y,
                    (i32::from(current) - (sum - 255)) as u8,
                );
            } else if sum < 255 {
                block.texture_layer_mut(max_layer).set_blend(
                    x,
                    y,
                    (i32::from(current) + (255 - sum)) as u8,
                );
            }
        }
    }

    block.hole_map_mut().unlock();

    modified
}

/// `SpaceSettings` (`tools/worldeditor/import/space_settings.cpp`): one
/// space's bounds, grid size and terrain settings, read from its
/// `space.settings`.
pub struct SpaceSettings {
    space: String,
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
    grid_size: f32,
    terrain_settings: Option<TerrainSettings>,
}

impl SpaceSettings {
    /// The constructor (`space_settings.cpp:19-52`): bounds default to
    /// the empty rect `1/1/-1/-1`, the grid size to
    /// [`DEFAULT_GRID_RESOLUTION`], and the terrain settings only load
    /// when the space is not the currently open one.
    #[must_use]
    pub fn new(file_system: &dyn FileSystem, space: &str, current_space: &str) -> Self {
        let mut out = Self {
            space: space.to_string(),
            min_x: 1,
            min_y: 1,
            max_x: -1,
            max_y: -1,
            grid_size: DEFAULT_GRID_RESOLUTION,
            terrain_settings: None,
        };
        let doc = file_system
            .read_file(&format!("{space}/{SPACE_SETTING_FILE_NAME}"))
            .and_then(|raw| resources::datasection::parse(SPACE_SETTING_FILE_NAME, &raw).ok());
        let Some(doc) = doc else {
            return out;
        };
        out.min_x = doc
            .open_section("bounds/minX")
            .map_or(1, |leaf| leaf.as_int(1));
        out.min_y = doc
            .open_section("bounds/minY")
            .map_or(1, |leaf| leaf.as_int(1));
        out.max_x = doc
            .open_section("bounds/maxX")
            .map_or(-1, |leaf| leaf.as_int(-1));
        out.max_y = doc
            .open_section("bounds/maxY")
            .map_or(-1, |leaf| leaf.as_int(-1));
        out.grid_size = doc
            .open_section("chunkSize")
            .map_or(DEFAULT_GRID_RESOLUTION, |leaf| {
                leaf.as_float(DEFAULT_GRID_RESOLUTION)
            });

        if space != current_space {
            // `ChunkSpace::getTerrainSettingsDataSection` is
            // `openSection("terrain")` (`chunk_space.cpp:1409-1413`).
            if let Some(terrain) = doc.open_section("terrain") {
                let mut settings = TerrainSettings::new();
                if settings.init(out.grid_size, Some(terrain), 0.0) {
                    out.terrain_settings = Some(settings);
                }
            }
        }
        out
    }

    /// `SpaceSettings::save` (`space_settings.cpp:55-64`): refresh the
    /// file's `<terrain>` section from the stored settings and save.
    pub fn save(&self, file_system: &dyn FileSystem) -> bool {
        let raw = file_system.read_file(&format!("{}/{}", self.space, SPACE_SETTING_FILE_NAME));
        let mut doc = raw
            .and_then(|raw| resources::datasection::parse(SPACE_SETTING_FILE_NAME, &raw).ok())
            .unwrap_or_else(|| DataSection::new(SPACE_SETTING_FILE_NAME));
        if let Some(settings) = &self.terrain_settings {
            doc.delete_sections("terrain");
            if let Some(terrain) = doc.open_or_make_section("terrain") {
                settings.save(terrain);
            }
        }
        let xml = doc.write_to_string();
        file_system.write_file(
            &format!("{}/{}", self.space, SPACE_SETTING_FILE_NAME),
            xml.as_bytes(),
        )
    }

    #[must_use]
    pub fn space(&self) -> &str {
        &self.space
    }

    #[must_use]
    pub fn min_x(&self) -> i32 {
        self.min_x
    }

    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    #[must_use]
    pub fn max_x(&self) -> i32 {
        self.max_x
    }

    #[must_use]
    pub fn max_y(&self) -> i32 {
        self.max_y
    }

    #[must_use]
    pub fn grid_size(&self) -> f32 {
        self.grid_size
    }

    #[must_use]
    pub fn terrain_settings(&self) -> Option<&TerrainSettings> {
        self.terrain_settings.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkSpace, GridBounds};
    use resources::datasection;
    use resources::file_system::NativeFileSystem;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("spacepub-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture(tag: &str) -> (std::path::PathBuf, NativeFileSystem) {
        let root = temp_root(tag);
        let fs = NativeFileSystem::new(root.to_str().expect("utf8 root"));
        (root, fs)
    }

    /// A 2x2 identity mapping at the given origin.
    fn mapping(path: &str, min: i32, max: i32) -> GeometryMapping {
        let bounds = GridBounds {
            min_l_grid_x: min,
            min_l_grid_y: min,
            max_l_grid_x: max,
            max_l_grid_y: max,
            min_grid_x: min,
            min_grid_y: min,
            max_grid_x: max,
            max_grid_y: max,
        };
        GeometryMapping::new(path, DEFAULT_GRID_RESOLUTION, bounds, false)
    }

    fn space_info(width: u16, height: u16) -> SpaceInformation {
        SpaceInformation {
            space_name: "space".to_string(),
            local_to_world: (0, 0),
            grid_width: width,
            grid_height: height,
        }
    }

    /// The version-200 terrain section shape a real `space.settings` saves.
    fn source_settings_xml() -> String {
        [
            "<space.settings>",
            "  <chunkSize>100.0</chunkSize>",
            "  <bounds>",
            "    <minX>2</minX><minY>2</minY><maxX>3</maxX><maxY>3</maxY>",
            "  </bounds>",
            "  <terrain>",
            "    <version>200</version>",
            "    <heightMapEditorSize>64</heightMapEditorSize>",
            "    <heightMapSize>128</heightMapSize>",
            "    <normalMapSize>128</normalMapSize>",
            "    <holeMapSize>32</holeMapSize>",
            "    <shadowMapSize>32</shadowMapSize>",
            "    <blendMapSize>32</blendMapSize>",
            "    <detailHeightMapDistance>300</detailHeightMapDistance>",
            "    <blendPreloadDistance>50</blendPreloadDistance>",
            "    <bumpFadingStart>350</bumpFadingStart>",
            "    <bumpFadingDistance>1000</bumpFadingDistance>",
            "    <lodTextureStart>30</lodTextureStart>",
            "    <lodTextureDistance>100</lodTextureDistance>",
            "    <lodNormalStart>130</lodNormalStart>",
            "    <lodNormalDistance>100</lodNormalDistance>",
            "    <normalPreloadDistance>200</normalPreloadDistance>",
            "    <lodInfo><lodDistances><distance0>300</distance0></lodDistances></lodInfo>",
            "  </terrain>",
            "</space.settings>",
        ]
        .concat()
    }

    /// Seed a source space with settings (+ optionally locations) and
    /// answer the recreator inputs for a 4x4 target.
    fn source_space(root: &std::path::Path, with_settings: bool) -> SourceSpace {
        std::fs::create_dir_all(root.join("space_src")).unwrap();
        if with_settings {
            std::fs::write(root.join("space_src/space.settings"), source_settings_xml()).unwrap();
        }
        std::fs::write(
            root.join("space_src/locations.xml"),
            "<locations.xml></locations.xml>",
        )
        .unwrap();
        SourceSpace {
            grid_size: 100.0,
            min_x: 2,
            min_z: 2,
            max_x: 3,
            max_z: 3,
        }
    }

    fn recreator() -> SpaceRecreator {
        SpaceRecreator::new(
            "space_src",
            "space_new",
            100.0,
            4,
            4,
            TerrainInfo {
                version: 200,
                height_map_editor_size: 64,
                height_map_size: 128,
                normal_map_size: 128,
                hole_map_size: 32,
                shadow_map_size: 32,
                blend_map_size: 32,
            },
            SourceSpace {
                grid_size: 100.0,
                min_x: 2,
                min_z: 2,
                max_x: -1,
                max_z: -1,
            },
        )
    }

    // ------------------------------------------------------------------
    // space helpers
    // ------------------------------------------------------------------

    #[test]
    fn offset_and_bias_grid_round_trip() {
        let ltw = (10, 20);
        assert_eq!(offset_grid(ltw, 1, 2), (11, 22));
        assert_eq!(bias_grid(ltw, 11, 22), (1, 2));
        // The reference wraps like uint16 arithmetic would.
        assert_eq!(bias_grid(ltw, -1, -1), (65525, 65515));
    }

    #[test]
    fn chunk_id_checks_bounds_and_prefixes() {
        let m = mapping("space/", 0, 0);
        assert_eq!(chunk_id(&m, 0, 0, true), "00000000o");
        // Outside the local bounds with the check on: empty.
        assert_eq!(chunk_id(&m, 1, 0, true), "");
        // ... and the real identifier with the check off.
        assert_eq!(chunk_id(&m, 1, 0, false), "00010000o");
    }

    #[test]
    fn thumbnail_names_parse_like_the_reference() {
        assert_eq!(
            grid_from_thumbnail_file_name("00010000o.cdata"),
            Some((1, 0))
        );
        // Seven bytes from the end must be the outside `o`.
        assert_eq!(grid_from_thumbnail_file_name("space.thumbnail.dds"), None);
        assert_eq!(grid_from_thumbnail_file_name("short.cdata"), None);
    }

    // ------------------------------------------------------------------
    // walkers
    // ------------------------------------------------------------------

    #[test]
    fn linear_walk_is_raster_x_fastest_then_stops() {
        let m = mapping("space/", 0, 4);
        let mut walker = LinearChunkWalker::new();
        walker.space_information(space_info(2, 3), &m);
        let mut seen = Vec::new();
        while let Some((name, x, z)) = walker.next_tile() {
            seen.push((name, x, z));
        }
        assert_eq!(
            seen,
            vec![
                ("00000000o".to_string(), 0, 0),
                ("00010000o".to_string(), 1, 0),
                ("00000001o".to_string(), 0, 1),
                ("00010001o".to_string(), 1, 1),
                ("00000002o".to_string(), 0, 2),
                ("00010002o".to_string(), 1, 2),
            ]
        );
        // Done stays done.
        assert!(walker.next_tile().is_none());
    }

    #[test]
    fn linear_walk_resumes_from_a_saved_cursor() {
        let m = mapping("space/", 0, 4);
        let mut walker = LinearChunkWalker::new();
        walker.space_information(space_info(2, 2), &m);
        assert!(walker.next_tile().is_some());
        assert!(walker.next_tile().is_some());
        let mut section = DataSection::new("progress");
        walker.save(&mut section);

        let mut resumed = LinearChunkWalker::new();
        resumed.space_information(space_info(2, 2), &m);
        resumed.load(Some(&section));
        let mut seen = Vec::new();
        while let Some((_, x, z)) = resumed.next_tile() {
            seen.push((x, z));
        }
        assert_eq!(seen, vec![(0, 1), (1, 1)]);

        // No section: restart from the origin.
        resumed.load(None);
        let (name, x, z) = resumed.next_tile().expect("two tiles left");
        assert_eq!((name.as_str(), x, z), ("00000000o", 0, 0));
    }

    #[test]
    fn bounded_walk_clamps_and_skips_empty_rects() {
        let m = mapping("space/", 0, 2);
        let mut walker = BoundedChunkWalker::new(-5, -5, 99, 99);
        walker.space_information(space_info(3, 3), &m);
        let mut count = 0;
        while walker.next_tile().is_some() {
            count += 1;
        }
        assert_eq!(count, 9);

        // A rect that misses the space entirely stops the walk.
        let mut walker = BoundedChunkWalker::new(10, 10, 20, 20);
        walker.space_information(space_info(3, 3), &m);
        assert!(walker.next_tile().is_none());

        // An exact sub-rect: right/top exclusive.
        let mut walker = BoundedChunkWalker::new(1, 1, 3, 2);
        walker.space_information(space_info(3, 3), &m);
        let mut seen = Vec::new();
        while let Some((_, x, z)) = walker.next_tile() {
            seen.push((x, z));
        }
        assert_eq!(seen, vec![(1, 1), (2, 1)]);
    }

    #[test]
    fn cache_walk_is_lifo_with_add_erase_and_size() {
        let m = mapping("space/", 0, 4);
        let mut walker = CacheChunkWalker::new();
        walker.space_information(space_info(2, 2), &m);
        assert_eq!(walker.size(), 0);

        walker.add(0, 0);
        walker.add(1, 0);
        walker.add_chunk_name("00010001o"); // grid (1, 1)
        walker.add_chunk_name("garbage");
        assert_eq!(walker.size(), 3);
        assert!(walker.added(1, 0));
        assert!(walker.chunk_name_added("00010001o"));
        assert!(!walker.chunk_name_added("garbage"));

        // Last pushed first.
        assert_eq!(
            walker.next_tile().expect("lifo"),
            ("00010001o".to_string(), 1, 1)
        );

        assert!(walker.erase(1, 0));
        assert!(!walker.erase(1, 0));
        assert!(walker.erase_chunk_name("00000000o"));
        assert!(!walker.erase_chunk_name("garbage"));
        assert_eq!(walker.size(), 0);
        assert!(walker.next_tile().is_none());

        // reset clears the cache.
        walker.add(0, 0);
        walker.reset();
        assert_eq!(walker.size(), 0);

        // Cached coordinates outside the geometry mapping answer None in
        // release builds (the reference asserts in debug).
        walker.add(9, 9);
        assert!(walker.next_tile().is_none());
    }

    #[test]
    fn cache_save_writes_rle_tile_ranges_and_load_restores() {
        let m = mapping("space/", 0, 4);
        let mut walker = CacheChunkWalker::new();
        walker.space_information(space_info(4, 4), &m);
        for (x, z) in [(0, 0), (1, 0), (2, 0), (0, 1)] {
            walker.add(x, z);
        }
        let mut section = DataSection::new("cache");
        walker.save(&mut section);
        // minX/minZ/width/height + a 0..2 run + a lone tile.
        assert_eq!(section.count_children(), 6);
        assert_eq!(section.open_section("minX").unwrap().as_int(0), 0);
        assert_eq!(section.open_section("width").unwrap().as_int(0), 4);

        let mut restored = CacheChunkWalker::new();
        restored.space_information(space_info(4, 4), &m);
        restored.load(&section);
        assert_eq!(restored.size(), 4);
        let mut seen = Vec::new();
        while let Some((_, x, z)) = restored.next_tile() {
            seen.push((x, z));
        }
        seen.sort();
        assert_eq!(seen, vec![(0, 0), (0, 1), (1, 0), (2, 0)]);

        // An empty cache writes nothing at all.
        let mut empty = CacheChunkWalker::new();
        empty.space_information(space_info(4, 4), &m);
        let mut section = DataSection::new("cache");
        empty.save(&mut section);
        assert_eq!(section.count_children(), 0);
    }

    /// The directory tree the file walker scans: a root-level thumbnail,
    /// an out-of-bounds one, a stray non-thumbnail file, a prefixed chunk
    /// under `fxxxfxxx/sep/` (the walker descends every folder into its
    /// `sep/` child) and a rejected `CVS` folder carrying a
    /// relevant-looking name.
    fn file_walker_tree(root: &std::path::Path) {
        for dir in ["space", "space/fxxxfxxx/sep", "space/fxxxfxxx/sep/CVS"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "space/00000000o.cdata",
            "space/00020002o.cdata", // out of bounds
            "space/notes.txt",
            "space/fxxxfxxx/sep/00010000o.cdata",
            "space/fxxxfxxx/sep/CVS/00000001o.cdata", // rejected folder
        ] {
            std::fs::write(root.join(file), b"stub").unwrap();
        }
    }

    #[test]
    fn file_walk_scans_the_space_tree_for_relevant_thumbnails() {
        let (root, fs) = fixture("filewalk");
        file_walker_tree(&root);
        let m = mapping("space/", 0, 1);
        let mut walker = FileChunkWalker::new(Rc::new(fs), 2);
        walker.space_information(space_info(2, 2), &m);
        // Once the folder tree is exhausted the walker restarts from the
        // root (the reference's idle handle falls back to `folder_ = ""`),
        // so the collection is capped and deduped — the face under test
        // is which tiles surface at all.
        let mut seen = Vec::new();
        for _ in 0..40 {
            let Some((name, x, z)) = walker.next_tile() else {
                continue;
            };
            let tile = (name, x, z);
            if !seen.contains(&tile) {
                seen.push(tile);
            }
        }
        seen.sort();
        // The out-of-bounds thumbnail, `notes.txt` and the CVS folder's
        // name-alike all stay out.
        assert_eq!(
            seen,
            vec![
                ("00000000o".to_string(), 0, 0),
                ("00010000o".to_string(), 1, 0),
            ]
        );
    }

    #[test]
    fn file_walk_resumes_from_a_reentry_folder() {
        let (root, _fs) = fixture("filewalk2");
        file_walker_tree(&root);
        let m = mapping("space/", 0, 1);

        // A first pass parks its folder in the settings section.
        let fs_for_first = NativeFileSystem::new(root.to_str().expect("utf8 root"));
        let mut walker = FileChunkWalker::new(Rc::new(fs_for_first), 2);
        walker.space_information(space_info(2, 2), &m);
        // The walk answers done at its first tile-less quantum.
        while walker.next_tile().is_some() {}
        let mut section = DataSection::new("progress");
        walker.save(&mut section);

        // A fresh walker resumes from the saved folder: the prefixed
        // chunk surfaces from the reentry descent.
        let fs_for_resume = NativeFileSystem::new(root.to_str().expect("utf8 root"));
        let mut resumed = FileChunkWalker::new(Rc::new(fs_for_resume), 2);
        resumed.space_information(space_info(2, 2), &m);
        let mut saved = DataSection::new("progress");
        resumed.save(&mut saved);
        resumed.load(Some(&saved));
        resumed.reentry_folder = "fxxxfxxx/sep/".to_string();
        let (name, x, z) = resumed.next_tile().expect("resumed tile");
        assert_eq!((name.as_str(), x, z), ("00010000o", 1, 0));

        // A pending tile is answered without a new find, even while the
        // reentry folder resolves.
        let fs_for_pending = NativeFileSystem::new(root.to_str().expect("utf8 root"));
        let mut pending = FileChunkWalker::new(Rc::new(fs_for_pending), 2);
        pending.space_information(space_info(2, 2), &m);
        pending.xs.push(0);
        pending.zs.push(0);
        pending.reentry_folder = "fxxxfxxx/sep/".to_string();
        assert_eq!(
            pending.next_tile().expect("pending tile"),
            ("00000000o".to_string(), 0, 0)
        );

        // A reentry folder that matches nothing descends nowhere.
        let fs_for_missing = NativeFileSystem::new(root.to_str().expect("utf8 root"));
        let mut missing = FileChunkWalker::new(Rc::new(fs_for_missing), 2);
        missing.space_information(space_info(2, 2), &m);
        missing.reentry_folder = "zzz/".to_string();
        assert!(missing.folders.is_empty());

        // No section: nothing saved to resume.
        let fs_for_fresh = NativeFileSystem::new(root.to_str().expect("utf8 root"));
        let mut fresh = FileChunkWalker::new(Rc::new(fs_for_fresh), 2);
        fresh.load(None);
        assert!(fresh.reentry_folder.is_empty());
    }

    #[test]
    fn file_walk_on_an_empty_space_answers_none() {
        let (_root, fs) = fixture("filewalk3");
        let m = mapping("space/", 0, 1);
        let mut walker = FileChunkWalker::new(Rc::new(fs), 4);
        walker.space_information(space_info(2, 2), &m);
        assert!(walker.next_tile().is_none());
    }

    // ------------------------------------------------------------------
    // recreator
    // ------------------------------------------------------------------

    #[test]
    fn calc_bounds_centres_on_zero() {
        // Even side: one more negative row.
        assert_eq!(SpaceRecreator::calc_bounds(4, 4), (-2, 1, -2, 1));
        // Odd side: symmetric.
        assert_eq!(SpaceRecreator::calc_bounds(3, 5), (-1, 1, -2, 2));
    }

    #[test]
    fn changing_chunk_size_follows_the_source_grid() {
        let src = SourceSpace {
            grid_size: 100.0,
            min_x: 2,
            min_z: 2,
            max_x: -1,
            max_z: -1,
        };
        assert!(!recreator().changing_chunk_size(&src));
        let rescaled = SourceSpace {
            grid_size: 200.0,
            ..src
        };
        assert!(recreator().changing_chunk_size(&rescaled));
    }

    #[test]
    fn published_space_boots_back_as_a_chunk_space() {
        let (root, fs) = fixture("publish");
        let src = source_space(&root, true);
        let mut rec = recreator();
        assert!(!rec.changing_chunk_size(&src));
        assert!(rec.create(&fs), "create failed: {}", rec.error_message());
        let _ = src;

        // The directory and the two top-level files exist.
        assert_eq!(
            fs.get_file_type("space_new/", None),
            FileType::Directory,
            "space directory missing"
        );

        // space.settings: the copied settings with overridden bounds and
        // the rebuilt terrain section.
        let raw = fs.read_file("space_new/space.settings").expect("settings");
        let doc = datasection::parse("space.settings", &raw).expect("settings parse");
        assert_eq!(doc.open_section("chunkSize").unwrap().as_float(0.0), 100.0);
        for (path, want) in [
            ("bounds/minX", -2),
            ("bounds/maxX", 1),
            ("bounds/minY", -2),
            ("bounds/maxY", 1),
        ] {
            assert_eq!(doc.open_section(path).unwrap().as_int(99), want, "{path}");
        }
        let terrain = doc.open_section("terrain").expect("terrain section");
        assert_eq!(terrain.open_section("version").unwrap().as_int(0), 200);
        assert_eq!(
            terrain
                .open_section("heightMapEditorSize")
                .unwrap()
                .as_int(0),
            64
        );
        assert_eq!(
            terrain.open_section("heightMapSize").unwrap().as_int(0),
            128
        );
        // The recreator's own settings match the section.
        assert_eq!(rec.terrain_settings().version(), 200);
        assert_eq!(rec.terrain_settings().height_map_editor_size(), 64);

        // locations.xml exists even when the source had none.
        let locations = fs.read_file("space_new/locations.xml").expect("locations");
        assert!(datasection::parse("locations.xml", &locations).is_ok());

        // One chunk file pair per grid cell. The coords -2..1 sit in the
        // uint16 wrap neighbourhood, where the reference's
        // `uint16(gridxs + 4096) >= 8192` tier tests all fail — every
        // identifier is flat, no prefix directories.
        let count_chunks = |fs: &NativeFileSystem, suffix: &str| -> usize {
            fs.read_directory("space_new")
                .expect("space dir")
                .into_iter()
                .filter(|name| name.ends_with(suffix))
                .count()
        };
        assert_eq!(count_chunks(&fs, ".chunk"), 16, "chunk xml count");
        assert_eq!(count_chunks(&fs, ".cdata"), 16, "chunk cdata count");

        // The most negative chunk carries the terrain reference; a virgin
        // chunk references its `.cdata/terrain2`.
        let prefixed = "fffefffeo";
        let chunk_raw = fs
            .read_file(&format!("space_new/{prefixed}.chunk"))
            .expect("negative chunk");
        let chunk_doc = datasection::parse("root", &chunk_raw).expect("chunk parse");
        assert_eq!(
            chunk_doc
                .open_section("terrain/resource")
                .unwrap()
                .as_string(),
            format!("{prefixed}.cdata/terrain2")
        );
        let plain_raw = fs
            .read_file("space_new/00000000o.chunk")
            .expect("plain chunk");
        let plain_doc = datasection::parse("root", &plain_raw).expect("chunk parse");
        assert_eq!(
            plain_doc
                .open_section("terrain/resource")
                .unwrap()
                .as_string(),
            "00000000o.cdata/terrain2"
        );

        // The published terrain block loads back with its blank under
        // layer fully blended and holeless.
        let cdata_raw = fs
            .read_file(&format!("space_new/{prefixed}.cdata"))
            .expect("cdata");
        let cdata =
            ZipFileSystem::open(&format!("{prefixed}.cdata"), cdata_raw).expect("cdata zip");
        let mut block = EditorTerrainBlock2::load(100.0, 64, 32, 32, 7, &cdata, "terrain2")
            .expect("terrain block loads");
        assert_eq!(block.number_texture_layers(), 1);
        assert_eq!(
            block.texture_layer(0).texture_name(),
            SPACE_RECREATOR_BLANK_TEXTURE
        );
        let blends = block.texture_layer(0).blends();
        for x in 0..blends.width() {
            for y in 0..blends.height() {
                assert_eq!(blends.get(x as i32, y as i32), u8::MAX, "blend ({x},{y})");
            }
        }
        let hole_map = block.hole_map_mut();
        assert!(hole_map.lock(true));
        let holes = hole_map.image();
        let holeless =
            (0..holes.width()).all(|x| (0..holes.height()).all(|y| !holes.get(x as i32, y as i32)));
        assert!(holeless, "published blocks must be holeless");
        hole_map.unlock();

        // The whole space boots as a chunk space: every chunk loads and
        // binds through its mapping.
        let mapping = GeometryMapping::from_settings("space_new/", &doc);
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        let resolver = |file: &str| -> Option<DataSection> {
            fs.read_file(&format!("{}{}", mapping.path, file))
                .and_then(|raw| datasection::parse("root", &raw).ok())
        };
        for x in -2..=1 {
            for z in -2..=1 {
                let identifier = mapping.outside_chunk_identifier(x, z, false);
                assert!(
                    space.load_chunk(&identifier, &resolver),
                    "load {identifier}"
                );
                assert!(space.bind(&identifier), "bind {identifier}");
            }
        }
    }

    #[test]
    fn missing_source_settings_fails_the_copy_but_still_writes_locations() {
        let (root, fs) = fixture("nosettings");
        source_space(&root, false);
        let mut rec = recreator();
        assert!(!rec.create(&fs));
        assert_eq!(rec.error_message(), "Unable to copy space.settings\n");
        // locations.xml is still written.
        assert!(fs.read_file("space_new/locations.xml").is_some());
        assert!(fs.read_file("space_new/space.settings").is_none());
    }

    #[test]
    fn a_non_power_of_two_editor_map_fails_every_chunk() {
        let (root, fs) = fixture("badmap");
        source_space(&root, true);
        let mut rec = SpaceRecreator::new(
            "space_src",
            "space_new",
            100.0,
            2,
            2,
            TerrainInfo {
                version: 200,
                height_map_editor_size: 63,
                height_map_size: 128,
                normal_map_size: 128,
                hole_map_size: 32,
                shadow_map_size: 32,
                blend_map_size: 32,
            },
            SourceSpace {
                grid_size: 100.0,
                min_x: 2,
                min_z: 2,
                max_x: -1,
                max_z: -1,
            },
        );
        assert!(!rec.create(&fs));
        let identifier = rec.chunk_name(-1, -1);
        assert_eq!(
            rec.error_message(),
            format!("Unable to create chunk {identifier}\n")
        );
    }

    /// A `NativeFileSystem` wrapper that refuses `write_file` for paths
    /// matching any needle.
    struct FlakyFs {
        inner: NativeFileSystem,
        needles: Vec<String>,
        fail_make: bool,
    }

    impl FileSystem for FlakyFs {
        fn get_file_type(
            &self,
            path: &str,
            info: Option<&mut resources::zip::FileInfo>,
        ) -> FileType {
            self.inner.get_file_type(path, info)
        }
        fn get_file_type_ex(
            &self,
            path: &str,
            info: Option<&mut resources::zip::FileInfo>,
        ) -> FileType {
            self.inner.get_file_type_ex(path, info)
        }
        fn read_directory(&self, path: &str) -> Option<Vec<String>> {
            self.inner.read_directory(path)
        }
        fn read_file(&self, path: &str) -> Option<Vec<u8>> {
            self.inner.read_file(path)
        }
        fn read_file_at(&self, dir_path: &str, index: usize) -> Option<Vec<u8>> {
            self.inner.read_file_at(dir_path, index)
        }
        fn get_absolute_path(&self, path: &str) -> Option<String> {
            self.inner.get_absolute_path(path)
        }
        fn make_directory(&self, path: &str) -> bool {
            if self.fail_make {
                return false;
            }
            self.inner.make_directory(path)
        }
        fn write_file(&self, path: &str, data: &[u8]) -> bool {
            if self.needles.iter().any(|needle| path.contains(needle)) {
                return false;
            }
            self.inner.write_file(path, data)
        }
        fn erase_file_or_directory(&self, path: &str) -> bool {
            self.inner.erase_file_or_directory(path)
        }
        fn move_file_or_directory(&self, old_path: &str, new_path: &str) -> bool {
            self.inner.move_file_or_directory(old_path, new_path)
        }
    }

    #[test]
    fn write_failures_surface_the_reference_errors() {
        let (root, fs) = fixture("flaky");
        source_space(&root, true);

        // space.settings write fails.
        let flaky = FlakyFs {
            inner: NativeFileSystem::new(root.to_str().expect("utf8 root")),
            fail_make: false,
            needles: vec!["space_new/space.settings".to_string()],
        };
        let mut rec = recreator();
        assert!(!rec.create(&flaky));
        assert_eq!(rec.error_message(), "Unable to save space.settings\n");

        // locations.xml write fails.
        let flaky = FlakyFs {
            inner: NativeFileSystem::new(root.to_str().expect("utf8 root")),
            fail_make: false,
            needles: vec!["space_new/locations.xml".to_string()],
        };
        let mut rec = recreator();
        assert!(!rec.create(&flaky));
        assert_eq!(rec.error_message(), "Unable to save locations.xml\n");
        assert!(fs.read_file("space_new/space.settings").is_some());

        // The first chunk's .chunk write fails: the run stops there.
        let flaky = FlakyFs {
            inner: NativeFileSystem::new(root.to_str().expect("utf8 root")),
            fail_make: false,
            needles: vec!["fffefffeo.chunk".to_string()],
        };
        let mut rec = recreator();
        assert!(!rec.create(&flaky));
        let identifier = rec.chunk_name(-2, -2);
        assert_eq!(
            rec.error_message(),
            format!("Unable to create chunk {identifier}\n")
        );
    }

    #[test]
    fn ensure_path_exists_creates_every_level() {
        let (_root, fs) = fixture("ensure");
        ensure_path_exists(&fs, "a/b/c/");
        assert_eq!(fs.get_file_type("a/b/c/", None), FileType::Directory);
        // Existing directories are left alone.
        ensure_path_exists(&fs, "a/b/c/");
        assert_eq!(fs.get_file_type("a/b/c/", None), FileType::Directory);
    }

    // ------------------------------------------------------------------
    // blend normalisation
    // ------------------------------------------------------------------

    fn fresh_block() -> EditorTerrainBlock2 {
        EditorTerrainBlock2::create(100.0, 64, 32, 32, 7).expect("block creates")
    }

    #[test]
    fn normalise_zero_pixels_cut_holes_and_full_blend_layer0() {
        let mut block = fresh_block();
        block.insert_texture_layer();
        // A fresh layer is all zeroes: every pixel takes the hole path,
        // which the reference does not count as a modification.
        assert!(!normalise_terrain_block_blends(&mut block));
        let blends = block.texture_layer(0).blends();
        for x in 0..blends.width() {
            for y in 0..blends.height() {
                assert_eq!(blends.get(x as i32, y as i32), u8::MAX);
            }
        }
        let hole_map = block.hole_map_mut();
        assert!(hole_map.lock(true));
        let holes = hole_map.image();
        let hole_count = (0..holes.width())
            .flat_map(|x| (0..holes.height()).map(move |y| (x, y)))
            .filter(|&(x, y)| holes.get(x as i32, y as i32))
            .count();
        let hole_total = (holes.width() * holes.height()) as usize;
        hole_map.unlock();
        assert_eq!(hole_count, hole_total);
    }

    #[test]
    fn normalise_leaves_full_pixels_alone() {
        let mut block = fresh_block();
        let index = block.insert_texture_layer();
        let layer = block.texture_layer_mut(index);
        for x in 0..32 {
            for y in 0..32 {
                layer.set_blend(x, y, u8::MAX);
            }
        }
        assert!(!normalise_terrain_block_blends(&mut block));
    }

    #[test]
    fn normalise_rescales_partial_pixels_with_leftover_to_the_max_layer() {
        let mut block = fresh_block();
        let a = block.insert_texture_layer();
        let b = block.insert_texture_layer();
        let c = block.insert_texture_layer();
        // One rescaled pixel and one full pixel.
        block.texture_layer_mut(a).set_blend(0, 0, 2);
        block.texture_layer_mut(b).set_blend(0, 0, 2);
        block.texture_layer_mut(c).set_blend(0, 0, 3);
        block.texture_layer_mut(a).set_blend(1, 0, 255);

        assert!(normalise_terrain_block_blends(&mut block));

        // 2+2+3=7 scales to 73+73+113=259; the four-unit surplus lands on
        // the strongest layer (113 -> 109).
        assert_eq!(block.texture_layer(a).blends().get(0, 0), 73);
        assert_eq!(block.texture_layer(b).blends().get(0, 0), 73);
        assert_eq!(block.texture_layer(c).blends().get(0, 0), 109);
        // The full pixel stays.
        assert_eq!(block.texture_layer(a).blends().get(1, 0), 255);
        assert_eq!(block.texture_layer(b).blends().get(1, 0), 0);
    }

    #[test]
    fn normalise_without_layers_answers_unmodified() {
        let mut block = fresh_block();
        assert!(!normalise_terrain_block_blends(&mut block));
    }

    // ------------------------------------------------------------------
    // space settings
    // ------------------------------------------------------------------

    #[test]
    fn space_settings_defaults_and_reads() {
        let (_root, fs) = fixture("ssdefault");
        let settings = SpaceSettings::new(&fs, "space_none", "space_open");
        assert_eq!(settings.min_x(), 1);
        assert_eq!(settings.min_y(), 1);
        assert_eq!(settings.max_x(), -1);
        assert_eq!(settings.max_y(), -1);
        assert_eq!(settings.grid_size(), DEFAULT_GRID_RESOLUTION);
        assert!(settings.terrain_settings().is_none());

        let (root, fs) = fixture("ssread");
        std::fs::create_dir_all(root.join("space_src")).unwrap();
        std::fs::write(root.join("space_src/space.settings"), source_settings_xml()).unwrap();

        // Not the current space: the terrain settings load.
        let settings = SpaceSettings::new(&fs, "space_src", "space_open");
        assert_eq!(settings.space(), "space_src");
        assert_eq!(settings.min_x(), 2);
        assert_eq!(settings.min_y(), 2);
        assert_eq!(settings.max_x(), 3);
        assert_eq!(settings.max_y(), 3);
        assert_eq!(settings.grid_size(), 100.0);
        let terrain = settings.terrain_settings().expect("terrain settings");
        assert_eq!(terrain.version(), 200);
        assert_eq!(terrain.height_map_size(), 128);
        assert_eq!(terrain.num_vertex_lods(), 7);

        // The current space: no terrain settings are built.
        let settings = SpaceSettings::new(&fs, "space_src", "space_src");
        assert!(settings.terrain_settings().is_none());

        // A settings file without a terrain section builds none either.
        std::fs::create_dir_all(root.join("space_noterrain")).unwrap();
        std::fs::write(
            root.join("space_noterrain/space.settings"),
            "<space.settings><chunkSize>50.0</chunkSize></space.settings>",
        )
        .unwrap();
        let settings = SpaceSettings::new(&fs, "space_noterrain", "space_open");
        assert_eq!(settings.grid_size(), 50.0);
        assert!(settings.terrain_settings().is_none());
    }

    #[test]
    fn space_settings_without_a_file_saves_an_empty_one() {
        let (_root, fs) = fixture("sssaveempty");
        ensure_path_exists(&fs, "space_new/");
        let settings = SpaceSettings::new(&fs, "space_new", "space_open");
        assert!(settings.terrain_settings().is_none());
        assert!(settings.save(&fs));
        assert!(fs.read_file("space_new/space.settings").is_some());
    }

    #[test]
    fn space_settings_save_refreshes_the_terrain_section() {
        let (root, fs) = fixture("sssave");
        std::fs::create_dir_all(root.join("space_src")).unwrap();
        std::fs::write(root.join("space_src/space.settings"), source_settings_xml()).unwrap();

        let settings = SpaceSettings::new(&fs, "space_src", "space_open");
        assert!(settings.save(&fs));

        let raw = fs
            .read_file("space_src/space.settings")
            .expect("settings still there");
        let doc = datasection::parse("space.settings", &raw).expect("parse");
        let terrain = doc.open_section("terrain").expect("terrain refreshed");
        assert_eq!(terrain.open_section("version").unwrap().as_int(0), 200);
        assert_eq!(
            terrain
                .open_section("lodInfo/lodDistances/distance0")
                .map(|leaf| leaf.as_int(0)),
            Some(0),
            "the reference save writes the distances under lodInfo"
        );
    }

    #[test]
    fn folder_names_filter_like_the_reference() {
        let (_root, _fs) = fixture("foldernames");
        let mut walker = FileChunkWalker::new(
            Rc::new(NativeFileSystem::new(_root.to_str().expect("utf8 root"))),
            1,
        );
        assert!(!walker.is_valid_folder(""));
        assert!(!walker.is_valid_folder("a"));
        assert!(!walker.is_valid_folder("ab"));
        assert!(walker.is_valid_folder("sep"));
        assert!(!walker.is_valid_folder("CVS"));
        assert!(walker.is_valid_folder("subfolder"));

        // A space directory that cannot be read contributes no folders,
        // and an empty reentry folder descends nowhere.
        walker.find_folders("missing/");
        assert!(walker.folders.is_empty());
        walker.find_reentry_folder();
        assert!(walker.folders.is_empty());
    }

    #[test]
    fn prefixed_chunks_create_their_sub_directory_once() {
        let (root, fs) = fixture("prefixed");
        source_space(&root, true);
        let mut rec = recreator();
        assert!(rec.create(&fs), "{}", rec.error_message());

        // Grid (16,0) trips the finest tier on the x side alone.
        assert_eq!(rec.chunk_name(16, 0), "001x000x/sep/00100000o");
        assert!(rec.create_chunk(&fs, 16, 0));
        assert!(
            fs.read_file("space_new/001x000x/sep/00100000o.chunk")
                .is_some()
        );
        assert!(
            fs.read_file("space_new/001x000x/sep/00100000o.cdata")
                .is_some()
        );
        // A second chunk under the same prefix skips the ensure.
        assert!(rec.create_chunk(&fs, 16, 1));
    }

    #[test]
    fn a_chunk_fully_inside_the_source_keeps_no_terrain() {
        let (root, fs) = fixture("covered");
        source_space(&root, true);
        // The source spans the whole target: every under-layer sample
        // lands inside the source bounds, so the layer is dropped.
        let mut rec = SpaceRecreator::new(
            "space_src",
            "space_new",
            100.0,
            4,
            4,
            TerrainInfo {
                version: 200,
                height_map_editor_size: 64,
                height_map_size: 128,
                normal_map_size: 128,
                hole_map_size: 32,
                shadow_map_size: 32,
                blend_map_size: 32,
            },
            SourceSpace {
                grid_size: 100.0,
                min_x: -2,
                min_z: -2,
                max_x: 1,
                max_z: 1,
            },
        );
        assert!(rec.create(&fs), "{}", rec.error_message());
        let raw = fs.read_file("space_new/00000000o.chunk").expect("chunk");
        let doc = datasection::parse("root", &raw).expect("chunk parse");
        assert!(
            doc.open_section("terrain").is_none(),
            "a covered chunk carries no terrain reference"
        );
        assert!(fs.read_file("space_new/00000000o.cdata").is_some());
    }

    #[test]
    fn normalise_hole_pixels_cover_the_smaller_hole_grid() {
        // Blend grid twice the hole grid: consecutive blend pixels share
        // one hole pixel, and the floor-equal ends widen the range.
        let mut block = EditorTerrainBlock2::create(100.0, 32, 32, 64, 5).expect("block");
        block.insert_texture_layer();
        assert!(!normalise_terrain_block_blends(&mut block));
        let hole_map = block.hole_map_mut();
        assert!(hole_map.lock(true));
        let holes = hole_map.image();
        let hole_count = (0..holes.width())
            .flat_map(|x| (0..holes.height()).map(move |y| (x, y)))
            .filter(|&(x, y)| holes.get(x as i32, y as i32))
            .count();
        hole_map.unlock();
        assert_eq!(hole_count, 32 * 32);
    }

    #[test]
    fn an_unwritable_space_directory_fails_the_recreate() {
        let (root, _fs) = fixture("nodir");
        source_space(&root, true);
        let flaky = FlakyFs {
            inner: NativeFileSystem::new(root.to_str().expect("utf8 root")),
            needles: Vec::new(),
            fail_make: true,
        };
        let mut rec = recreator();
        assert!(!rec.create(&flaky));
        assert_eq!(
            rec.error_message(),
            "Unable to create new space directory\n"
        );
    }

    #[test]
    fn flaky_fs_delegates_the_rest() {
        let (root, _fs) = fixture("flakydeleg");
        source_space(&root, true);
        let flaky = FlakyFs {
            inner: NativeFileSystem::new(root.to_str().expect("utf8 root")),
            needles: Vec::new(),
            fail_make: false,
        };
        assert_eq!(
            flaky.get_file_type_ex("space_src", None),
            FileType::Directory
        );
        assert_eq!(flaky.read_directory("space_src").map(|e| e.len()), Some(2));
        assert!(flaky.read_file_at("space_src", 0).is_some());
        assert!(flaky.get_absolute_path("space_src").is_some());
        assert!(!flaky.erase_file_or_directory("space_src/nope"));
        assert!(!flaky.move_file_or_directory("space_src/nope", "nope2"));
    }
}
