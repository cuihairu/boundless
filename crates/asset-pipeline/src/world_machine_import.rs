//! BW-13-05-04 the World Machine height importer
//! (`tools/bw_world_machine_import/`).
//!
//! The reference is a World Machine plugin DLL in two halves:
//!
//! - [`SpaceHelper`] (`space_helper.cpp`) reads a space's `space.settings`
//!   and hands out the chunk `.cdata` archives;
//! - the `BWHeightImport` generator (`bw_height_import.cpp`) resamples the
//!   space's terrain height maps into a World Machine height field, plus
//!   the project state (`Load`/`Save`), the two parameters, the space
//!   picker dialog and the plugin enumeration (`wm_plugin_shell.cpp`).
//!
//! The World Machine framework itself — the `Generator`/`Device` base
//! classes, `HField`, the `Parameter` set, `SizeData`/`BuildContext`,
//! `WMGlobal` and the Win32 `GetOpenFileName` dialog — is not in this tree.
//! Those points become the [`WmHost`] seam, cited at each site. The
//! plugin's `GetHeaderVersion` export only returns the SDK header version
//! macro, which is likewise framework-internal and stays behind the seam.
//!
//! The reference carries no material face — the plugin imports heights
//! only — so the "materials match" acceptance reduces to heights.
//!
//! Reference quirks are ported on purpose and cited at each site.

use resources::bwresource::{format_path, get_file_path};
use resources::datasection;
use resources::file_system::{FileSystem, NativeFileSystem};
use resources::zip::{FileType, ZipFileSystem};
use terrain::editor_block::{HEIGHTS_SECTION_NAME, TERRAIN2_SECTION_NAME};
use terrain::height_map::{
    HEIGHT_MAP_MAGIC, HEIGHT_MAP_VERSION_ABS_QFLOAT, HeightMapHeader, QUANTISED_PNG_VERSION,
    unquantise,
};
use terrain::png::decompress_png;

use crate::offline_processor::outside_chunk_identifier;

/// `lifeptrs.nametag` — the project-state tag (`bw_height_import.cpp:48`).
pub const NAMETAG: &str = "BWHI";

/// `HEIGH_SCALE_PROPERTY` (`bw_height_import.cpp:313`).
pub const HEIGHT_SCALE_PROPERTY: &str = "Height Scale";

/// `CHUNK_SCALE_PROPERTY` (`bw_height_import.cpp:314`).
pub const CHUNK_SCALE_PROPERTY: &str = "Chunk Scale";

/// `GetTypeName` (`bw_height_import.hpp:27`).
pub const TYPE_NAME: &str = "BigWorld Height Importer";

/// `GetDescription` (`bw_height_import.hpp:26`).
pub const DESCRIPTION: &str = "Import height map from BigWorld space";

/// The two `AddParam` defaults (`bw_height_import.cpp:53-54`).
pub const HEIGHT_SCALE_DEFAULT: f32 = 1.0;
pub const CHUNK_SCALE_DEFAULT: f32 = 100.0;

/// The `Parameter` ranges both properties are registered with.
pub const PARAM_MIN: f32 = 0.0;
pub const PARAM_MAX: f32 = 10000.0;

/// The four `setHelpString` texts, in registration order
/// (`bw_height_import.cpp:59-62`).
pub const HELP_STRINGS: [&str; 4] = [
    "Height scale of the terrain",
    "Chunk scale for the terrain",
    "Reset the Chunk scale of the terrain",
    "Select a space",
];

/// `DEFAULT_GRID_RESOLUTION` (`chunk/chunk_grid_size.hpp:21`).
pub const DEFAULT_GRID_RESOLUTION: f32 = 100.0;

/// The `GetOpenFileName` flag set the reference ors together
/// (`bw_height_import.cpp:299-300`).
pub const OFN_FILEMUSTEXIST: u32 = 0x1000;
pub const OFN_PATHMUSTEXIST: u32 = 0x0800;
pub const OFN_NOCHANGEDIR: u32 = 0x0008;
pub const OFN_READONLY: u32 = 0x0001;
pub const OFN_HIDEREADONLY: u32 = 0x0004;
pub const SPACE_DIALOG_FLAGS: u32 =
    OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_READONLY | OFN_HIDEREADONLY;

/// The `OPENFILENAME` fields the reference fills (`bw_height_import.cpp:286-301`).
pub struct SpaceDialog {
    /// `"Space Settings\0space.settings\0\0"`.
    pub filter: &'static str,
    /// `"Select space"`.
    pub title: &'static str,
    /// `"settings"` (`ofn.lpstrDefExt`).
    pub default_extension: &'static str,
    /// The `OFN_*` mask above.
    pub flags: u32,
}

impl Default for SpaceDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl SpaceDialog {
    /// The dialog description the reference builds.
    pub fn new() -> Self {
        Self {
            filter: "Space Settings\0space.settings\0\0",
            title: "Select space",
            default_extension: "settings",
            flags: SPACE_DIALOG_FLAGS,
        }
    }
}

/// The `DevPluginStruc` fields `GetDevPlugin` fills
/// (`wm_plugin_shell.cpp:28-49`).
pub struct DevicePlugin {
    /// `dat.name` = `GetTypeName()`.
    pub name: &'static str,
    /// `dat.type` = `TYPE_GENERATOR`.
    pub device_type: &'static str,
    /// `GetDescription()`.
    pub description: &'static str,
    /// `lifeptrs.nametag`.
    pub nametag: &'static str,
}

/// `GetDevPlugin` (`wm_plugin_shell.cpp:28-49`): device 0 is the height
/// importer, any other index enumerates nothing.
pub fn get_dev_plugin(index: usize) -> Option<DevicePlugin> {
    match index {
        0 => Some(DevicePlugin {
            name: TYPE_NAME,
            device_type: "Generator",
            description: DESCRIPTION,
            nametag: NAMETAG,
        }),
        _ => None,
    }
}

/// `Moo::Image<float>` for the height images: width, height and the row
/// major floats, with the boundary-clamped `get` (`image.ipp:347-354`).
#[derive(Debug, Clone, PartialEq)]
pub struct FloatImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<f32>,
}

impl FloatImage {
    /// `Image(w, h)` — cleared to zero, like the default-filled `Moo::Image`.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0.0; width as usize * height as usize],
        }
    }

    /// `Image::get` (`image.ipp:347-354`): boundary clamping.
    fn get(&self, x: i32, y: i32) -> f32 {
        let x = x.clamp(0, self.width as i32 - 1);
        let y = y.clamp(0, self.height as i32 - 1);
        self.pixels[(y as u32 * self.width + x as u32) as usize]
    }

    /// `Image::getBilinear` (`image.ipp:386-406`).
    pub fn get_bilinear(&self, x: f32, y: f32) -> f32 {
        let ix = x.floor() as i32;
        let iy = y.floor() as i32;
        let fx = x - ix as f32;
        let fy = y - iy as f32;

        let p00 = self.get(ix, iy);
        let p10 = self.get(ix + 1, iy);
        let p01 = self.get(ix, iy + 1);
        let p11 = self.get(ix + 1, iy + 1);

        let i1 = p00 + fx * (p10 - p00);
        let i2 = p01 + fx * (p11 - p01);

        i1 + fy * (i2 - i1)
    }
}

/// The `SizeData` fields the port consumes: the height-field pixel size
/// (`GetNewHF` builds the `HField` from them) plus `vert_scale` and
/// `scalesize`. The framework's remaining fields stay behind the seam.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizeData {
    pub width: u32,
    pub height: u32,
    pub vert_scale: f32,
    pub scalesize: (f32, f32),
}

/// `CoordF` — the framework's float pair.
pub type CoordF = (f32, f32);

/// The World Machine framework seam:
///
/// - `WMGlobal->km_wm_relation` — the kilometre-to-World-Machine scale;
/// - `GetNewHF(context.GetWorldSize())` + `map->Clear()` — the destination
///   height field;
/// - `context.ReportProgress(this, step, total)`;
/// - `GetOpenFileName` — the space picker; `None` is the cancelled dialog.
pub trait WmHost {
    fn km_wm_relation(&self) -> f32;

    /// `GetNewHF` plus `map->Clear()`: a cleared height field of the
    /// requested size.
    fn new_height_field(&mut self, size: &SizeData) -> FloatImage;

    /// `BuildContext::ReportProgress` — one call per resampled row.
    fn report_progress(&mut self, step: i32, total: i32);

    /// `GetOpenFileName(&ofn)`: `Some(path)` for an accepted dialog, `None`
    /// when cancelled. The path feeds [`SpaceHelper::init`] verbatim.
    fn choose_space(&mut self, dialog: &SpaceDialog) -> Option<String>;
}

/// `SpaceHelper` (`space_helper.hpp:13-44`): the space-side reader.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceHelper {
    x_min: i32,
    x_max: i32,
    z_min: i32,
    z_max: i32,
    chunk_size: f32,
    single_dir: bool,
    space_root: String,
    file_system: Option<NativeFileSystem>,
}

impl Default for SpaceHelper {
    fn default() -> Self {
        Self::new()
    }
}

impl SpaceHelper {
    /// The constructor defaults (`space_helper.cpp:19-27`): zero bounds,
    /// `DEFAULT_GRID_RESOLUTION`, no `singleDir`.
    pub fn new() -> Self {
        Self {
            x_min: 0,
            x_max: 0,
            z_min: 0,
            z_max: 0,
            chunk_size: DEFAULT_GRID_RESOLUTION,
            single_dir: false,
            space_root: String::new(),
            file_system: None,
        }
    }

    pub fn x_min(&self) -> i32 {
        self.x_min
    }

    pub fn x_max(&self) -> i32 {
        self.x_max
    }

    pub fn z_min(&self) -> i32 {
        self.z_min
    }

    pub fn z_max(&self) -> i32 {
        self.z_max
    }

    pub fn chunk_size(&self) -> f32 {
        self.chunk_size
    }

    pub fn single_dir(&self) -> bool {
        self.single_dir
    }

    pub fn space_root(&self) -> &str {
        &self.space_root
    }

    /// `valid()` (`space_helper.hpp:31`).
    pub fn valid(&self) -> bool {
        !self.space_root.is_empty()
    }

    /// `SpaceHelper::init` (`space_helper.cpp:35-60`): read
    /// `space.settings` through the native file system
    /// (`XMLSection::createFromFile`), then the bounds, `chunkSize` and
    /// `singleDir` records. A failed load only clears the root and the
    /// file system — the bounds and chunk size keep their previous values,
    /// exactly like the reference.
    pub fn init(&mut self, space_settings_path: &str) -> bool {
        // `BWResource::nativeFileSystem()->readFile` + parse; a missing or
        // unparseable file is the NULL section.
        let section = NativeFileSystem::new("")
            .read_file(space_settings_path)
            .and_then(|raw| datasection::parse("root", &raw).ok());

        self.space_root.clear();
        self.file_system = None;

        let Some(section) = section else {
            return false;
        };

        self.x_min = section.read_int("bounds/minX", 0);
        self.x_max = section.read_int("bounds/maxX", 0);
        self.z_min = section.read_int("bounds/minY", 0);
        self.z_max = section.read_int("bounds/maxY", 0);
        self.chunk_size = section.read_float("chunkSize", DEFAULT_GRID_RESOLUTION);
        self.single_dir = section.read_bool("singleDir", false);

        // `BWResource::getFilePath` as the plugin sees it — the Windows
        // `BWUtil::getFilePath` returns `formatPath(directory)`, i.e. the
        // directory with the separator ensured. The separator is what the
        // `spaceRoot + "space.settings"` concat in `Load` relies on.
        self.space_root = format_path(&get_file_path(space_settings_path));
        self.file_system = Some(NativeFileSystem::new(&self.space_root));

        // `return pSpaceSettings.exists()`.
        true
    }

    /// `getCDataForChunk` (`space_helper.cpp:69-90`): the chunk's `.cdata`
    /// archive bytes, `None` outside the bounds, without a file system or
    /// when the file is not an archive. The reference hands a `ZipSection`
    /// over the same archive; the readBinary descent it enables lives in
    /// [`load_height_map`].
    pub fn get_cdata_for_chunk(&self, x: i32, z: i32) -> Option<Vec<u8>> {
        let mut cdata_name = String::new();
        if x >= self.x_min && x <= self.x_max && z >= self.z_min && z <= self.z_max {
            cdata_name = format!("{}.cdata", outside_chunk_identifier(x, z, self.single_dir));
        }

        if !cdata_name.is_empty()
            && let Some(file_system) = &self.file_system
            && file_system.get_file_type_ex(&cdata_name, None) == FileType::Archive
        {
            return file_system.read_file(&cdata_name);
        }

        None
    }
}

/// `loadHeightMap` (`bw_height_import.cpp:125-156`): the `terrain2/heights`
/// block of a chunk `.cdata`, decoded as an absolute quantised float map.
/// `None` mirrors every reference failure (no section, no block, wrong
/// magic or version, broken payload).
pub fn load_height_map(cdata: Option<&[u8]>) -> Option<FloatImage> {
    // `if (!pCDataSection) return false;`
    let cdata = cdata?;

    // `pCDataSection->readBinary("terrain2/heights")`: the ZipSection
    // descent through the two zips. A non-zip archive fails the open,
    // which is the reference's failed `ZipSection` load.
    let outer = ZipFileSystem::open("", cdata.to_vec())?;
    let terrain2 = outer.read_file(TERRAIN2_SECTION_NAME)?;
    let inner = ZipFileSystem::open(TERRAIN2_SECTION_NAME, terrain2)?;
    let heights = inner.read_file(HEIGHTS_SECTION_NAME)?;

    // The reference casts the block straight to `HeightMapHeader*` and
    // reads the magic/version; a short block would read out of bounds, so
    // the port reports it through the header parse.
    let (header, payload) = HeightMapHeader::parse(&heights).ok()?;
    if header.magic != HEIGHT_MAP_MAGIC || header.version != HEIGHT_MAP_VERSION_ABS_QFLOAT {
        return None;
    }

    // `decompressHeightMap` (`height_map_compress.cpp:96-141`): 'qpng'
    // magic, then a 32bpp PNG of int32 millimetre heights.
    if payload.len() <= 4
        || u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
            != QUANTISED_PNG_VERSION
    {
        return None;
    }
    let image = decompress_png(&payload[4..]).ok()?;

    // `quantisedPNGDecompress` sizes the float image from the decoded PNG.
    let count = image.width as usize * image.height as usize;
    let mut pixels = Vec::with_capacity(count);
    for index in 0..count {
        let x = (index as u32) % image.width;
        let y = (index as u32) / image.width;
        pixels.push(unquantise(image.word(x, y)));
    }
    Some(FloatImage {
        width: image.width,
        height: image.height,
        pixels,
    })
}

/// The two `AddParam` values (`bw_height_import.cpp:53-54`) the generator
/// reads with `ParmFRef`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeightImportParams {
    pub height_scale: f32,
    pub chunk_scale: f32,
}

impl Default for HeightImportParams {
    fn default() -> Self {
        Self {
            height_scale: HEIGHT_SCALE_DEFAULT,
            chunk_scale: CHUNK_SCALE_DEFAULT,
        }
    }
}

/// `BWHeightImport::resetChunkScale` (`bw_height_import.cpp:271-278`): the
/// chunk-scale parameter takes the space's own chunk size.
pub fn reset_chunk_scale(params: &mut HeightImportParams, helper: &SpaceHelper) {
    params.chunk_scale = helper.chunk_size();
}

/// `BWHeightImport::selectSpace` (`bw_height_import.cpp:284-310`): show the
/// space picker; a non-empty accepted path re-inits the helper.
pub fn select_space(host: &mut dyn WmHost, helper: &mut SpaceHelper) {
    let dialog = SpaceDialog::new();
    if let Some(filename) = host.choose_space(&dialog)
        && !filename.is_empty()
    {
        helper.init(&filename);
    }
}

/// `BWHeightImport::Save` (`bw_height_import.cpp:102-113`): the nametag,
/// a version byte and the space root in a fixed 256-byte field. The
/// `bw_snprintf` writes at most 255 characters plus the terminator; the
/// rest of the field is zeroed (the reference leaves uninitialised stack
/// bytes there). The `Generator::Save` face — the framework's parameter
/// serialisation — is a documented seam the port leaves out.
pub fn save_state(helper: &SpaceHelper) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(NAMETAG.as_bytes());
    out.push(1);
    let mut field = [0u8; 256];
    let root = helper.space_root().as_bytes();
    let len = root.len().min(255);
    field[..len].copy_from_slice(&root[..len]);
    out.extend_from_slice(&field);
    out
}

/// `BWHeightImport::Load` (`bw_height_import.cpp:77-95`): check the
/// nametag, skip the version byte and re-init the helper from the stored
/// root plus `space.settings`. The reference reads a fixed 256-byte field
/// out of the stream; a truncated state zero-fills the tail and terminates
/// at the first NUL, which is where the C string would end (the reference
/// reads into uninitialised stack bytes). A wrong or missing tag returns
/// false before anything else, like the reference. The `Generator::Load`
/// face (the parameter bytes) is a documented seam: the port's base-class
/// state is empty and always succeeds.
pub fn load_state(data: &[u8], helper: &mut SpaceHelper) -> bool {
    if data.len() < 4 || &data[..4] != NAMETAG.as_bytes() {
        return false;
    }

    // `in.read((char*)&ver, 1)` — the version byte is not used.
    let field = &data[5..data.len().min(5 + 256)];
    let mut space_root = [0u8; 256];
    space_root[..field.len()].copy_from_slice(field);
    let end = space_root
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(space_root.len());
    let space_root = String::from_utf8_lossy(&space_root[..end]).into_owned();

    helper.init(&format!("{space_root}space.settings"))
}

/// `BWHeightImport::Activate` (`bw_height_import.cpp:165-266`): resample
/// the space's chunk height maps into a fresh World Machine height field
/// and hand it back (`StoreData`). Runs with an invalid helper too — the
/// map then stays cleared, and the function still returns it (the
/// reference always stores and returns true).
pub fn activate(
    params: &HeightImportParams,
    helper: &SpaceHelper,
    host: &mut dyn WmHost,
    size: &SizeData,
    pan_loc: CoordF,
) -> FloatImage {
    // Create a new heightfield using current specified world size.
    let mut map = host.new_height_field(size);

    if helper.valid() {
        // Calculate scale from BigWorld height units to World Machine.
        let mut height_scale = params.height_scale;
        height_scale *= size.vert_scale / 1000.0;

        let km = host.km_wm_relation();

        // Get the offset of the lower corner of the WM machine height map
        // in metres.
        let dest_offset = (pan_loc.0 * km * 1000.0, pan_loc.1 * km * 1000.0);

        // The size of the world machine height map in metres.
        let dest_size = (
            size.scalesize.0 * km * 1000.0,
            size.scalesize.1 * km * 1000.0,
        );

        // Get the user defined chunk scale.
        let chunk_scale = params.chunk_scale;

        // Calculate the overlap between the World Machine height map and
        // the BigWorld space (`int(floorf(..))`).
        let chunk_x_min = helper
            .x_min()
            .max((dest_offset.0 / chunk_scale).floor() as i32);
        let chunk_x_max = helper
            .x_max()
            .min(((dest_offset.0 + dest_size.0) / chunk_scale).floor() as i32);
        let chunk_z_min = helper
            .z_min()
            .max((dest_offset.1 / chunk_scale).floor() as i32);
        let chunk_z_max = helper
            .z_max()
            .min(((dest_offset.1 + dest_size.1) / chunk_scale).floor() as i32);

        // Calculate the number of steps to perform while building the
        // height map.
        let num_build_steps = (chunk_x_max - chunk_x_min + 1) * map.height as i32;
        let mut build_progress: i32 = 0;

        // Iterate over the chunks to load up their height maps.
        for chunk_z in chunk_z_min..=chunk_z_max {
            for chunk_x in chunk_x_min..=chunk_x_max {
                let Some(heights) =
                    load_height_map(helper.get_cdata_for_chunk(chunk_x, chunk_z).as_deref())
                else {
                    continue;
                };

                // The offset of the bottom left corner of the chunk.
                let chunk_corner = (
                    chunk_x as f32 * chunk_scale - dest_offset.0,
                    chunk_z as f32 * chunk_scale - dest_offset.1,
                );

                // The start of the chunk in the destination map.
                let x_chunk_start = chunk_corner.0 / dest_size.0 * map.width as f32;
                let y_chunk_start = chunk_corner.1 / dest_size.1 * map.height as f32;

                // The pixel indexes this chunk covers in the destination
                // height map.
                let x_start = x_chunk_start.ceil() as i32;
                let y_start = y_chunk_start.ceil() as i32;
                let x_end =
                    ((chunk_corner.0 + chunk_scale) / dest_size.0 * map.width as f32).ceil() as i32;
                let y_end = ((chunk_corner.1 + chunk_scale) / dest_size.1 * map.height as f32)
                    .ceil() as i32;

                let x_start = x_start.max(0);
                let y_start = y_start.max(0);
                let x_end = x_end.min(map.width as i32);
                let y_end = y_end.min(map.height as i32);

                // Calculate the mapping from one pixel in the destination
                // height map to the source height map. `width - 5` runs in
                // unsigned arithmetic before the float cast, exactly like
                // the reference.
                let gradient = (
                    (heights.width.wrapping_sub(5)) as f32
                        / chunk_scale
                        / (map.width as f32 / dest_size.0),
                    (heights.height.wrapping_sub(5)) as f32
                        / chunk_scale
                        / (map.height as f32 / dest_size.1),
                );

                // Calculate the offset into the source height map that
                // maps to 0,0 in the destination height map.
                let corner_offset = (
                    -x_chunk_start * gradient.0 + 2.0,
                    -y_chunk_start * gradient.1 + 2.0,
                );

                for y in y_start..y_end {
                    for x in x_start..x_end {
                        let coord = (y as u32 * map.width + x as u32) as usize;
                        map.pixels[coord] = height_scale
                            * heights.get_bilinear(
                                corner_offset.0 + x as f32 * gradient.0,
                                corner_offset.1 + y as f32 * gradient.1,
                            );
                    }

                    host.report_progress(build_progress + 1, num_build_steps);
                    build_progress += 1;
                }
            }
        }
    }

    // pass the heightfield to the output stage.
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use terrain::height_map::{TerrainHeightMap2, quantise};

    /// A space directory with `space.settings` and one `.cdata` per grid
    /// cell, each carrying a pole-height ramp `f(x, z)`.
    fn make_space(
        dir: &PathBuf,
        min_x: i32,
        max_x: i32,
        min_z: i32,
        max_z: i32,
        heights: impl Fn(f32, f32) -> f32,
    ) -> SpaceHelper {
        fs::create_dir_all(dir).unwrap();
        let settings = format!(
            "<space.settings><bounds><minX>{min_x}</minX><maxX>{max_x}</maxX>\
             <minY>{min_z}</minY><maxY>{max_z}</maxY></bounds>\
             <chunkSize>100</chunkSize><singleDir>false</singleDir></space.settings>"
        );
        fs::write(dir.join("space.settings"), settings).unwrap();

        // `with_size(100, 32, 0)` gives a 36x36 pole image (32 + the
        // visible offsets).
        for chunk_z in min_z..=max_z {
            for chunk_x in min_x..=max_x {
                let mut map = TerrainHeightMap2::with_size(100.0, 32, 0);
                let poles = map.image_data().len();
                let width = map.poles_width();
                for index in 0..poles {
                    let x = (index as u32 % width) as f32;
                    let z = (index as u32 / width) as f32;
                    map.set_pole(x as i32, z as i32, heights(x, z));
                }
                let mut inner = ZipFileSystem::open("", ZipFileSystem::empty_archive())
                    .expect("an empty archive always parses");
                assert!(inner.write_to_structure(HEIGHTS_SECTION_NAME, map.save()));
                let mut cdata = ZipFileSystem::open("", ZipFileSystem::empty_archive())
                    .expect("an empty archive always parses");
                assert!(cdata.write_to_structure(TERRAIN2_SECTION_NAME, inner.create_zip()));
                let name = format!(
                    "{}.cdata",
                    outside_chunk_identifier(chunk_x, chunk_z, false)
                );
                fs::write(dir.join(name), cdata.create_zip()).unwrap();
            }
        }

        let mut helper = SpaceHelper::new();
        let settings_path = dir.join("space.settings");
        assert!(helper.init(settings_path.to_str().unwrap()));
        helper
    }

    /// A fresh temp directory per test.
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bw_wm_import_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// The reference bilinear, re-derived in the test from `image.ipp`.
    fn oracle_bilinear(image: &FloatImage, x: f32, y: f32) -> f32 {
        let ix = x.floor() as i32;
        let iy = y.floor() as i32;
        let fx = x - ix as f32;
        let fy = y - iy as f32;
        let p00 = image.get(ix, iy);
        let p10 = image.get(ix + 1, iy);
        let p01 = image.get(ix, iy + 1);
        let p11 = image.get(ix + 1, iy + 1);
        let i1 = p00 + fx * (p10 - p00);
        let i2 = p01 + fx * (p11 - p01);
        i1 + fy * (i2 - i1)
    }

    const SIZE: SizeData = SizeData {
        width: 128,
        height: 64,
        vert_scale: 1000.0,
        scalesize: (1.0, 1.0),
    };

    /// The host: records progress and dialog calls, hands out cleared
    /// height fields of the requested size.
    struct TestHost {
        km_wm_relation: f32,
        progress: RefCell<Vec<(i32, i32)>>,
    }

    impl TestHost {
        fn new() -> Self {
            Self {
                km_wm_relation: 1.0,
                progress: RefCell::new(Vec::new()),
            }
        }
    }

    impl WmHost for TestHost {
        fn km_wm_relation(&self) -> f32 {
            self.km_wm_relation
        }

        fn new_height_field(&mut self, size: &SizeData) -> FloatImage {
            FloatImage::new(size.width, size.height)
        }

        fn report_progress(&mut self, step: i32, total: i32) {
            self.progress.borrow_mut().push((step, total));
        }

        fn choose_space(&mut self, _dialog: &SpaceDialog) -> Option<String> {
            None
        }
    }

    /// The settings reader: bounds, chunk size, singleDir and the derived
    /// space root.
    #[test]
    fn init_reads_the_bounds_chunk_size_and_single_dir() {
        let dir = temp_root("init");
        let helper = make_space(&dir, -1, 1, -2, 2, |x, z| 10.0 * x + 0.5 * z);
        assert!(helper.valid());
        assert_eq!(helper.x_min(), -1);
        assert_eq!(helper.x_max(), 1);
        assert_eq!(helper.z_min(), -2);
        assert_eq!(helper.z_max(), 2);
        assert_eq!(helper.chunk_size(), 100.0);
        assert!(!helper.single_dir());
        assert_eq!(helper.space_root(), format!("{}/", dir.to_str().unwrap()));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A missing file or a broken XML fails the init and leaves the root
    /// empty — but the numeric records keep their previous values, like
    /// the reference.
    #[test]
    fn init_failures_clear_the_root_and_keep_the_old_records() {
        let dir = temp_root("initfail");
        fs::create_dir_all(&dir).unwrap();
        let mut helper = make_space(&dir, 2, 5, 3, 6, |x, _z| x);

        // A missing file.
        let missing = dir.join("missing.settings");
        assert!(!helper.init(missing.to_str().unwrap()));
        assert!(!helper.valid());
        assert_eq!((helper.x_min(), helper.x_max()), (2, 5));
        assert_eq!((helper.z_min(), helper.z_max()), (3, 6));
        assert_eq!(helper.chunk_size(), 100.0);

        // A broken XML document.
        let broken = dir.join("broken.settings");
        fs::write(&broken, "<space.settings><bounds><minX>9").unwrap();
        assert!(!helper.init(broken.to_str().unwrap()));
        assert!(!helper.valid());
        assert_eq!(helper.x_min(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Defaults before any init.
    #[test]
    fn a_fresh_helper_uses_the_constructor_defaults() {
        let helper = SpaceHelper::new();
        assert_eq!(helper, SpaceHelper::default());
        assert!(!helper.valid());
        assert_eq!(helper.chunk_size(), DEFAULT_GRID_RESOLUTION);
        assert!(!helper.single_dir());
        assert_eq!(
            (
                helper.x_min(),
                helper.x_max(),
                helper.z_min(),
                helper.z_max()
            ),
            (0, 0, 0, 0)
        );
    }

    /// In-bounds chunks read their archive; out-of-bounds grid cells and
    /// missing files answer `None`.
    #[test]
    fn cdata_reads_follow_the_bounds() {
        let dir = temp_root("cdata");
        let helper = make_space(&dir, 0, 1, 0, 0, |x, z| 10.0 * x + 0.5 * z);

        assert!(helper.get_cdata_for_chunk(0, 0).is_some());
        assert!(helper.get_cdata_for_chunk(1, 0).is_some());

        // Outside the bounds — the name is never built.
        assert!(helper.get_cdata_for_chunk(2, 0).is_none());
        assert!(helper.get_cdata_for_chunk(-1, 0).is_none());

        // In bounds but no such file.
        fs::remove_file(dir.join("00010000o.cdata")).unwrap();
        assert!(helper.get_cdata_for_chunk(1, 0).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A chunk file that exists but is not a zip is not an archive and
    /// answers `None`.
    #[test]
    fn a_plain_file_is_not_a_cdata_archive() {
        let dir = temp_root("plain");
        let helper = make_space(&dir, 0, 0, 0, 0, |x, _z| x);
        fs::write(dir.join("00000000o.cdata"), b"not a zip").unwrap();
        assert!(helper.get_cdata_for_chunk(0, 0).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// The height map decoder round-trips the quantised PNG the terrain
    /// editor writes, millimetre grid included.
    #[test]
    fn load_height_map_decodes_the_quantised_png() {
        let dir = temp_root("decode");
        let helper = make_space(&dir, 0, 0, 0, 0, |x, z| 10.0 * x + 0.5 * z);
        let cdata = helper.get_cdata_for_chunk(0, 0).unwrap();
        let image = load_height_map(Some(&cdata)).unwrap();
        assert_eq!((image.width, image.height), (36, 36));
        // Pole (3, 5) sits at 10*3 + 0.5*5 = 32.5 on the millimetre grid.
        let index = (5 * image.width + 3) as usize;
        assert_eq!(image.pixels[index], unquantise(quantise(32.5)));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Every decoder failure answers `None`.
    #[test]
    fn load_height_map_rejects_the_broken_shapes() {
        // No section at all.
        assert!(load_height_map(None).is_none());

        // Not a zip.
        assert!(load_height_map(Some(b"junk")).is_none());

        // An archive without a terrain2 entry.
        let mut bare = ZipFileSystem::open("", ZipFileSystem::empty_archive())
            .expect("an empty archive always parses");
        assert!(bare.write_to_structure("other", b"n".to_vec()));
        let bytes = bare.create_zip();
        assert!(load_height_map(Some(&bytes)).is_none());

        // A terrain2 entry without heights.
        let mut inner = ZipFileSystem::open("", ZipFileSystem::empty_archive())
            .expect("an empty archive always parses");
        assert!(inner.write_to_structure("holes", b"n".to_vec()));
        let mut noheights = ZipFileSystem::open("", ZipFileSystem::empty_archive())
            .expect("an empty archive always parses");
        assert!(noheights.write_to_structure(TERRAIN2_SECTION_NAME, inner.create_zip()));
        let bytes = noheights.create_zip();
        assert!(load_height_map(Some(&bytes)).is_none());

        // Heights with a truncated header.
        let build = |heights: Vec<u8>| {
            let mut inner = ZipFileSystem::open("", ZipFileSystem::empty_archive())
                .expect("an empty archive always parses");
            assert!(inner.write_to_structure(HEIGHTS_SECTION_NAME, heights));
            let mut cdata = ZipFileSystem::open("", ZipFileSystem::empty_archive())
                .expect("an empty archive always parses");
            assert!(cdata.write_to_structure(TERRAIN2_SECTION_NAME, inner.create_zip()));
            cdata.create_zip()
        };
        assert!(load_height_map(Some(&build(vec![0u8; 4]))).is_none());

        // Heights with the wrong magic and the wrong version — full
        // 32-byte headers so the parse passes and the magic/version
        // check itself rejects.
        let header = |magic: u32, version: u32| {
            let mut blob = Vec::new();
            blob.extend_from_slice(&magic.to_le_bytes());
            blob.extend_from_slice(&36u32.to_le_bytes());
            blob.extend_from_slice(&36u32.to_le_bytes());
            blob.extend_from_slice(&0u32.to_le_bytes());
            blob.extend_from_slice(&version.to_le_bytes());
            blob.extend_from_slice(&0.0f32.to_le_bytes());
            blob.extend_from_slice(&0.0f32.to_le_bytes());
            blob.extend_from_slice(&0u32.to_le_bytes());
            blob
        };
        let wrong_magic = header(0x12345678, HEIGHT_MAP_VERSION_ABS_QFLOAT);
        let wrong_version = header(HEIGHT_MAP_MAGIC, 3);
        assert!(load_height_map(Some(&build(wrong_magic))).is_none());
        assert!(load_height_map(Some(&build(wrong_version))).is_none());

        // A payload without the qpng magic.
        let mut blob = header(HEIGHT_MAP_MAGIC, HEIGHT_MAP_VERSION_ABS_QFLOAT);
        blob.extend_from_slice(&0x12345678u32.to_le_bytes());
        assert!(load_height_map(Some(&build(blob))).is_none());

        // A qpng payload whose PNG body is broken.
        let mut blob = header(HEIGHT_MAP_MAGIC, HEIGHT_MAP_VERSION_ABS_QFLOAT);
        blob.extend_from_slice(&QUANTISED_PNG_VERSION.to_le_bytes());
        blob.extend_from_slice(b"broken png");
        assert!(load_height_map(Some(&build(blob))).is_none());

        // And a good one, as a sanity anchor for the arms above.
        let dir = temp_root("anchor");
        let good = make_space(&dir, 0, 0, 0, 0, |x, _z| x);
        let cdata = good.get_cdata_for_chunk(0, 0).unwrap();
        assert!(load_height_map(Some(&cdata)).is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    /// `getBilinear` matches the reference formula, including the
    /// boundary-clamped `get`.
    #[test]
    fn bilinear_matches_the_reference_and_clamps_the_edges() {
        let mut image = FloatImage::new(4, 3);
        for y in 0..3 {
            for x in 0..4 {
                image.pixels[(y * 4 + x) as usize] = (x * 10 + y) as f32;
            }
        }
        // Interior: (2.5, 1.25) between p00=(2,1)=21, p10=(3,1)=31,
        // p01=(2,2)=22, p11=(3,2)=32.
        let got = image.get_bilinear(2.5, 1.25);
        assert_eq!(got, oracle_bilinear(&image, 2.5, 1.25));
        assert_eq!(got, 21.0 + 0.5 * 10.0 + 0.25 * 1.0);

        // A negative coordinate clamps into the image on both axes.
        let got = image.get_bilinear(-1.0, -2.0);
        assert_eq!(got, oracle_bilinear(&image, -1.0, -2.0));
        assert_eq!(got, 0.0);

        // Beyond the far edges everything clamps to the last row/column.
        let got = image.get_bilinear(100.0, 100.0);
        assert_eq!(got, oracle_bilinear(&image, 100.0, 100.0));
        assert_eq!(got, 32.0);
    }

    /// The acceptance face: terrain size, sampling and the height-scale
    /// factor. A one-chunk space with a linear ramp; the resampled map is
    /// compared value for value against the reference arithmetic applied
    /// to the decoded source image, and the map keeps the requested
    /// height-field size. The plugin carries no material face, so nothing
    /// else moves.
    #[test]
    fn activate_resamples_the_space_onto_the_requested_size() {
        let dir = temp_root("activate");
        let helper = make_space(&dir, 0, 0, 0, 0, |x, z| 10.0 * x + 0.5 * z);
        let params = HeightImportParams::default();
        let mut host = TestHost::new();

        let map = activate(&params, &helper, &mut host, &SIZE, (0.0, 0.0));

        // The destination keeps the requested height-field size.
        assert_eq!((map.width, map.height), (SIZE.width, SIZE.height));

        // Reference arithmetic, re-derived: the chunk covers the whole
        // destination, so every pixel samples the decoded source image.
        let heights = load_height_map(helper.get_cdata_for_chunk(0, 0).as_deref()).unwrap();
        let dest_size = (SIZE.scalesize.0 * 1000.0, SIZE.scalesize.1 * 1000.0);
        let x_chunk_start = 0.0f32 / dest_size.0 * SIZE.width as f32;
        let y_chunk_start = 0.0f32 / dest_size.1 * SIZE.height as f32;
        let x_start = x_chunk_start.ceil() as i32;
        let y_start = y_chunk_start.ceil() as i32;
        let x_end = ((100.0 / dest_size.0) * SIZE.width as f32).ceil() as i32;
        let y_end = ((100.0 / dest_size.1) * SIZE.height as f32).ceil() as i32;
        let x_start = x_start.max(0);
        let y_start = y_start.max(0);
        let x_end = x_end.min(SIZE.width as i32);
        let y_end = y_end.min(SIZE.height as i32);
        let gradient = (
            (heights.width.wrapping_sub(5)) as f32 / 100.0 / (SIZE.width as f32 / dest_size.0),
            (heights.height.wrapping_sub(5)) as f32 / 100.0 / (SIZE.height as f32 / dest_size.1),
        );
        let corner_offset = (
            -x_chunk_start * gradient.0 + 2.0,
            -y_chunk_start * gradient.1 + 2.0,
        );

        // One progress call per written row, total = chunk span × height.
        let progress = host.progress.borrow();
        assert_eq!(progress.len(), (y_end - y_start) as usize);
        assert!(
            progress
                .iter()
                .all(|&(_, total)| total == SIZE.height as i32)
        );
        for (index, &(step, _)) in progress.iter().enumerate() {
            assert_eq!(step, (index + 1) as i32);
        }

        // Every written pixel, value for value; the untouched fringe stays
        // zero.
        for y in 0..SIZE.height as i32 {
            for x in 0..SIZE.width as i32 {
                let coord = (y as u32 * map.width + x as u32) as usize;
                if (x_start..x_end).contains(&x) && (y_start..y_end).contains(&y) {
                    let want = (1.0f32 * (1000.0f32 / 1000.0f32))
                        * oracle_bilinear(
                            &heights,
                            corner_offset.0 + x as f32 * gradient.0,
                            corner_offset.1 + y as f32 * gradient.1,
                        );
                    assert_eq!(map.pixels[coord], want, "pixel ({x}, {y})");
                } else {
                    assert_eq!(map.pixels[coord], 0.0, "fringe ({x}, {y})");
                }
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// The scale chain: `heightScale *= vert_scale / 1000`, the pan
    /// offset and the world size all go through `km_wm_relation * 1000`.
    #[test]
    fn activate_applies_the_scale_chain_and_the_pan_offset() {
        let dir = temp_root("scale");
        let helper = make_space(&dir, 0, 1, 0, 0, |x, z| 10.0 * x + 0.5 * z);
        let params = HeightImportParams {
            height_scale: 2.0,
            chunk_scale: 100.0,
        };
        let mut host = TestHost::new();
        host.km_wm_relation = 0.5;

        let size = SizeData {
            vert_scale: 500.0,
            ..SIZE
        };
        let map = activate(&params, &helper, &mut host, &size, (0.1, 0.0));

        // Reference arithmetic, re-derived: height_scale = 2 * 500/1000;
        // dest_size = (1,1) * 0.5 * 1000 = 500 metres; dest_offset =
        // (0.1, 0) * 0.5 * 1000 = (50, 0) metres. The second chunk's
        // window therefore shifts 50 m left in destination space.
        let heights = [
            load_height_map(helper.get_cdata_for_chunk(0, 0).as_deref()).unwrap(),
            load_height_map(helper.get_cdata_for_chunk(1, 0).as_deref()).unwrap(),
        ];
        let dest_size = (
            size.scalesize.0 * 0.5 * 1000.0,
            size.scalesize.1 * 0.5 * 1000.0,
        );
        let dest_offset = (0.1 * 0.5 * 1000.0, 0.0f32);
        let mut want = FloatImage::new(size.width, size.height);
        for chunk_x in 0..=1 {
            let heights = &heights[chunk_x as usize];
            let chunk_corner = (
                chunk_x as f32 * 100.0 - dest_offset.0,
                0.0f32 * 100.0 - dest_offset.1,
            );
            let x_chunk_start = chunk_corner.0 / dest_size.0 * size.width as f32;
            let y_chunk_start = chunk_corner.1 / dest_size.1 * size.height as f32;
            let x_start = x_chunk_start.ceil() as i32;
            let y_start = y_chunk_start.ceil() as i32;
            let x_end = ((chunk_corner.0 + 100.0) / dest_size.0 * size.width as f32).ceil() as i32;
            let y_end = ((chunk_corner.1 + 100.0) / dest_size.1 * size.height as f32).ceil() as i32;
            let x_start = x_start.max(0);
            let y_start = y_start.max(0);
            let x_end = x_end.min(size.width as i32);
            let y_end = y_end.min(size.height as i32);
            let gradient = (
                (heights.width.wrapping_sub(5)) as f32 / 100.0 / (size.width as f32 / dest_size.0),
                (heights.height.wrapping_sub(5)) as f32
                    / 100.0
                    / (size.height as f32 / dest_size.1),
            );
            let corner_offset = (
                -x_chunk_start * gradient.0 + 2.0,
                -y_chunk_start * gradient.1 + 2.0,
            );
            for y in y_start..y_end {
                for x in x_start..x_end {
                    let coord = (y as u32 * want.width + x as u32) as usize;
                    want.pixels[coord] = (2.0f32 * (500.0f32 / 1000.0f32))
                        * oracle_bilinear(
                            heights,
                            corner_offset.0 + x as f32 * gradient.0,
                            corner_offset.1 + y as f32 * gradient.1,
                        );
                }
            }
        }
        assert_eq!(map, want);
        let _ = fs::remove_dir_all(&dir);
    }

    /// An invalid helper skips the resample but the cleared map is still
    /// stored and no progress is reported.
    #[test]
    fn activate_with_an_invalid_helper_stores_the_cleared_map() {
        let helper = SpaceHelper::new();
        let params = HeightImportParams::default();
        let mut host = TestHost::new();

        let map = activate(&params, &helper, &mut host, &SIZE, (0.0, 0.0));
        assert_eq!((map.width, map.height), (SIZE.width, SIZE.height));
        assert!(map.pixels.iter().all(|&h| h == 0.0));
        assert!(host.progress.borrow().is_empty());
    }

    /// A chunk inside the destination window without a readable height
    /// map contributes nothing, and its rows are not reported.
    #[test]
    fn activate_skips_unreadable_chunks() {
        let dir = temp_root("gap");
        let helper = make_space(&dir, 0, 1, 0, 0, |x, _z| x);
        fs::remove_file(dir.join("00010000o.cdata")).unwrap();
        let params = HeightImportParams::default();
        let mut host = TestHost::new();

        let map = activate(&params, &helper, &mut host, &SIZE, (0.0, 0.0));
        // The first chunk still wrote rows.
        let first_row = (0..SIZE.width)
            .map(|x| map.pixels[x as usize])
            .collect::<Vec<_>>();
        assert!(first_row.iter().any(|&h| h != 0.0));
        let _ = fs::remove_dir_all(&dir);
    }

    /// `resetChunkScale` takes the space's chunk size.
    #[test]
    fn reset_chunk_scale_takes_the_space_value() {
        let dir = temp_root("reset");
        let helper = make_space(&dir, 0, 0, 0, 0, |x, _z| x);
        let mut params = HeightImportParams {
            chunk_scale: 55.0,
            ..HeightImportParams::default()
        };
        reset_chunk_scale(&mut params, &helper);
        assert_eq!(params.chunk_scale, 100.0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The space picker: a chosen path re-inits, an empty path and a
    /// cancelled dialog leave the helper alone. The dialog description is
    /// the reference's `OPENFILENAME` face.
    #[test]
    fn select_space_follows_the_dialog_results() {
        let dir = temp_root("select");
        let settings = dir.join("space.settings");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &settings,
            "<root><bounds><minX>3</minX><maxX>4</maxX><minY>5</minY><maxY>6</maxY>\
             </bounds></root>",
        )
        .unwrap();

        struct DialogHost {
            answers: Vec<Option<String>>,
            seen_dialogs: usize,
        }
        impl WmHost for DialogHost {
            fn km_wm_relation(&self) -> f32 {
                1.0
            }
            fn new_height_field(&mut self, size: &SizeData) -> FloatImage {
                FloatImage::new(size.width, size.height)
            }
            fn report_progress(&mut self, _step: i32, _total: i32) {}
            fn choose_space(&mut self, dialog: &SpaceDialog) -> Option<String> {
                self.seen_dialogs += 1;
                assert_eq!(dialog.title, "Select space");
                assert_eq!(dialog.filter, "Space Settings\0space.settings\0\0");
                assert_eq!(dialog.default_extension, "settings");
                assert_eq!(
                    dialog.flags,
                    OFN_FILEMUSTEXIST
                        | OFN_PATHMUSTEXIST
                        | OFN_NOCHANGEDIR
                        | OFN_READONLY
                        | OFN_HIDEREADONLY
                );
                self.answers.pop().flatten()
            }
        }

        let mut helper = SpaceHelper::new();

        // Cancelled dialog.
        let mut host = DialogHost {
            answers: vec![None],
            seen_dialogs: 0,
        };
        select_space(&mut host, &mut helper);
        assert_eq!(host.seen_dialogs, 1);
        assert!(!helper.valid());

        // Accepted but empty path.
        let mut host = DialogHost {
            answers: vec![Some(String::new())],
            seen_dialogs: 0,
        };
        select_space(&mut host, &mut helper);
        assert_eq!(host.seen_dialogs, 1);
        assert!(!helper.valid());

        // A real selection.
        let mut host = DialogHost {
            answers: vec![Some(settings.to_str().unwrap().to_string())],
            seen_dialogs: 0,
        };
        select_space(&mut host, &mut helper);
        assert!(helper.valid());
        assert_eq!((helper.x_min(), helper.x_max()), (3, 4));
        assert_eq!((helper.z_min(), helper.z_max()), (5, 6));

        // The remaining framework faces the host carries.
        assert_eq!(host.km_wm_relation(), 1.0);
        let field = host.new_height_field(&SIZE);
        assert_eq!((field.width, field.height), (SIZE.width, SIZE.height));
        host.report_progress(3, 7);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The project state round-trips through the nametag, version byte
    /// and 256-byte root field.
    #[test]
    fn state_round_trips_and_rejects_a_wrong_tag() {
        let dir = temp_root("state");
        let helper = make_space(&dir, 1, 2, 3, 4, |x, _z| x);

        let state = save_state(&helper);
        assert_eq!(&state[..4], NAMETAG.as_bytes());
        assert_eq!(state[4], 1);
        assert_eq!(state.len(), 4 + 1 + 256);
        // The root text is NUL-terminated inside the field.
        let root = &state[5..5 + helper.space_root().len()];
        assert_eq!(root, helper.space_root().as_bytes());
        assert_eq!(state[5 + helper.space_root().len()], 0);

        let mut loaded = SpaceHelper::new();
        assert!(load_state(&state, &mut loaded));
        assert_eq!(loaded, helper);

        // A wrong tag fails before anything else.
        let mut wrong = state.clone();
        wrong[0] = b'X';
        let mut untouched = SpaceHelper::new();
        assert!(!load_state(&wrong, &mut untouched));
        assert!(!untouched.valid());

        // A truncated state still reads the short root, which then points
        // at a directory that does not hold space.settings.
        let mut short = state.clone();
        short.truncate(20);
        let mut truncated = SpaceHelper::new();
        assert!(!load_state(&short, &mut truncated));
        let _ = fs::remove_dir_all(&dir);
    }

    /// `GetDevPlugin` enumerates exactly one device with the reference
    /// strings; the constants carry the property names, help texts,
    /// defaults and ranges.
    #[test]
    fn dev_plugin_enumerates_the_height_importer() {
        let device = get_dev_plugin(0).unwrap();
        assert_eq!(device.name, TYPE_NAME);
        assert_eq!(device.device_type, "Generator");
        assert_eq!(device.description, DESCRIPTION);
        assert_eq!(device.nametag, "BWHI");
        assert!(get_dev_plugin(1).is_none());

        assert_eq!(HEIGHT_SCALE_PROPERTY, "Height Scale");
        assert_eq!(CHUNK_SCALE_PROPERTY, "Chunk Scale");
        assert_eq!(
            HELP_STRINGS,
            [
                "Height scale of the terrain",
                "Chunk scale for the terrain",
                "Reset the Chunk scale of the terrain",
                "Select a space",
            ]
        );
        assert_eq!((HEIGHT_SCALE_DEFAULT, CHUNK_SCALE_DEFAULT), (1.0, 100.0));
        assert_eq!((PARAM_MIN, PARAM_MAX), (0.0, 10000.0));
        assert_eq!(
            HeightImportParams::default(),
            HeightImportParams {
                height_scale: 1.0,
                chunk_scale: 100.0,
            }
        );

        // The dialog description's Default is the reference face.
        let dialog = SpaceDialog::default();
        assert_eq!(
            (
                dialog.filter,
                dialog.title,
                dialog.default_extension,
                dialog.flags
            ),
            (
                "Space Settings\0space.settings\0\0",
                "Select space",
                "settings",
                SPACE_DIALOG_FLAGS
            )
        );
    }

    /// A cancelled dialog through the plain test host leaves the helper
    /// alone, like the reference's cancelled `GetOpenFileName`.
    #[test]
    fn select_space_with_a_cancelled_dialog_keeps_the_helper() {
        let mut helper = SpaceHelper::new();
        let mut host = TestHost::new();
        select_space(&mut host, &mut helper);
        assert!(!helper.valid());
    }

    /// The settings defaults apply when the records are missing
    /// (`readInt` default 0, `chunkSize` default
    /// `DEFAULT_GRID_RESOLUTION`, `singleDir` default false).
    #[test]
    fn settings_defaults_apply_when_records_are_missing() {
        let dir = temp_root("defaults");
        fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("space.settings");
        fs::write(&settings, "<root></root>").unwrap();

        let mut helper = SpaceHelper::new();
        assert!(helper.init(settings.to_str().unwrap()));
        assert_eq!(
            (
                helper.x_min(),
                helper.x_max(),
                helper.z_min(),
                helper.z_max()
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(helper.chunk_size(), DEFAULT_GRID_RESOLUTION);
        assert!(!helper.single_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A `DataSection` walk guard for the settings document: the bounds
    /// live under the `bounds` child, as the reference paths expect.
    #[test]
    fn settings_bounds_live_under_the_bounds_section() {
        let raw = b"<root><bounds><minX>1</minX><maxX>2</maxX><minY>3</minY>\
                    <maxY>4</maxY></bounds><chunkSize>200</chunkSize>\
                    <singleDir>true</singleDir></root>";
        let section = datasection::parse("root", raw).unwrap();
        assert_eq!(section.read_int("bounds/minX", 0), 1);
        assert_eq!(section.read_int("bounds/maxX", 0), 2);
        assert_eq!(section.read_int("bounds/minY", 0), 3);
        assert_eq!(section.read_int("bounds/maxY", 0), 4);
        assert_eq!(section.read_float("chunkSize", 100.0), 200.0);
        assert!(section.read_bool("singleDir", false));
    }
}
