//! BW-13-03-03 the terrain editor faces (`EDITOR_ENABLED` slices of
//! `lib/terrain/terrain2/` and the chunk-side terrain item save).
//!
//! - [`TextureLayer2`]: the blend layer save/load pair
//!   (`terrain_texture_layer2.cpp`, the `BlendHeader` container),
//! - [`EditorTerrainBlock2`]: the block that composes the maps this crate
//!   already reads — heights, holes, texture layers, dominant textures —
//!   and saves them into the chunk `.cdata` zip
//!   (`editor_terrain_block2.cpp`),
//! - [`UnsavedTerrainBlocks`]: the save-time bookkeeping
//!   (`lib/chunk/unsaved_terrain_blocks.cpp`),
//! - [`write_terrain_section`]: the chunk item face that points the chunk
//!   at its block (`editor_chunk_terrain.cpp:388-395`).
//!
//! The renderer-owned sub-blocks the reference also saves (normal map, AO
//! map, horizon shadows, lod texture, vertex lods beyond the height
//! pyramid) belong to renderer faces this tree does not carry; each site
//! is marked where the reference saves them.

use crate::dominant_texture_map::{DominantSaveError, DominantTextureMap2, U8Image};
use crate::height_map::{HeightCreateError, TerrainHeightMap2};
use crate::hole_map::TerrainHoleMap2;
use crate::png;
use resources::zip::ZipFileSystem;
use std::collections::BTreeMap;

/// `CommonTerrainBlock2::dataSectionName` (`common_terrain_block2.hpp:81`).
pub const TERRAIN2_SECTION_NAME: &str = "terrain2";

/// `EditorTerrainBlock2::saveHeightmap` section (`:790`).
pub const HEIGHTS_SECTION_NAME: &str = "heights";
/// The hole section (`editor_terrain_block2.cpp:263`).
pub const HOLES_SECTION_NAME: &str = "holes";
/// `TerrainBlock2::LAYER_SECTION_NAME` (`terrain_block2.cpp:231`).
pub const LAYER_SECTION_NAME: &str = "layer";
/// The dominant map section (`editor_terrain_block2.cpp:276`).
pub const DOMINANT_TEXTURES_SECTION_NAME: &str = "dominantTextures";
/// `TerrainBlock2::TERRAIN_BLOCK_META_DATA_SECTION_NAME`
/// (`terrain_block2.cpp:228`).
pub const TERRAIN_BLOCK_META_DATA_SECTION_NAME: &str = "TerrainBlockMetaData";
/// `TerrainBlock2::FORCED_LOD_ATTRIBUTE_NAME` (`terrain_block2.cpp:229`).
pub const FORCED_LOD_ATTRIBUTE_NAME: &str = "ForcedLod";

/// `BlendHeader::MAGIC` — "bld\0" (`terrain_data.hpp:66-67`).
pub const BLEND_MAGIC: u32 = 0x0064_6c62;
/// `BlendHeader::VERSION_RAW_BLENDS` (`terrain_data.hpp:59`).
pub const BLEND_VERSION_RAW: u32 = 1;
/// `BlendHeader::VERSION_PNG_BLENDS` (`terrain_data.hpp:60`).
pub const BLEND_VERSION_PNG: u32 = 2;
/// `sizeof(BlendHeader)`: eight u32 words plus the two `Vector4`
/// projections (`terrain_data.hpp:54-68`).
pub const BLEND_HEADER_SIZE: usize = 64;

/// `s_terrainTextureSpacing` (`terrain_texture_layer.cpp:21`): the default
/// metres-per-texture-tile the default UV projections divide by.
pub const DEFAULT_TEXTURE_SPACING: f32 = 10.0;

/// `TerrainTextureLayer::defaultUVProjections`
/// (`terrain_texture_layer.cpp:403-407`): the u axis runs along +x, the v
/// axis along +z, each scaled to one tile per `textureSpacing` metres.
pub fn default_uv_projections() -> ([f32; 4], [f32; 4]) {
    let inv = 1.0 / DEFAULT_TEXTURE_SPACING;
    ([inv, 0.0, 0.0, 0.0], [0.0, 0.0, inv, 0.0])
}

/// `TerrainTextureLayer2::load` failures. The reference reports most
/// problems by returning false; where it also formats a message the port
/// carries it.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerLoadError {
    /// `asBinary()` is null (`terrain_texture_layer2.cpp:483`).
    NotBinary,
    /// Wrong `BlendHeader::MAGIC` (`:487`) — the reference just fails.
    WrongMagic,
    /// "Blend data is incorrect version.  Wanted 1 or 2, got %u"
    /// (`:634-640`, two spaces preserved from the reference format).
    WrongVersion(u32),
    /// "TerrainTextureLayer2::load: PNG blend data is invalid" (`:617-624`).
    InvalidBlendPng,
    /// The name/blend reads run off the end of the block. The reference
    /// reads out of bounds; the port reports truncation (registered
    /// divergence, as with the map readers).
    Truncated,
}

impl core::fmt::Display for LayerLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LayerLoadError::NotBinary => f.write_str("terrain texture layer is not binary"),
            LayerLoadError::WrongMagic => f.write_str("wrong blend header magic"),
            LayerLoadError::WrongVersion(v) => f.write_str(&format!(
                "Blend data is incorrect version.  Wanted 1 or 2, got {v}\n"
            )),
            LayerLoadError::InvalidBlendPng => {
                f.write_str("TerrainTextureLayer2::load: PNG blend data is invalid\n")
            }
            LayerLoadError::Truncated => f.write_str("terrain texture layer is truncated"),
        }
    }
}

impl std::error::Error for LayerLoadError {}

/// One texture layer (`TerrainTextureLayer2`): the texture (and optional
/// bump) names, the world-space UV projections and the blend weights.
#[derive(Debug, Clone)]
pub struct TextureLayer2 {
    texture_name: String,
    bump_texture_name: String,
    u_projection: [f32; 4],
    v_projection: [f32; 4],
    blends: U8Image,
}

impl TextureLayer2 {
    /// The constructor face the editor uses
    /// (`terrain_texture_layer2.cpp:36-66`): zero size means "the
    /// settings' blend map size", and a new layer starts with zeroed
    /// blends and the default UV projections once the texture is set.
    pub fn new(blend_width: u32, blend_height: u32) -> Self {
        Self {
            texture_name: String::new(),
            bump_texture_name: String::new(),
            u_projection: [0.0; 4],
            v_projection: [0.0; 4],
            blends: U8Image::new(blend_width.max(1), blend_height.max(1)),
        }
    }

    /// `textureName()` (`terrain_texture_layer2.cpp:82-91`).
    pub fn texture_name(&self) -> &str {
        &self.texture_name
    }

    /// `textureName(filename)` — the `MF_SERVER` branch the editor build
    /// also takes when the texture manager is absent (`:104-135`): plain
    /// assign, unconditionally true.
    pub fn set_texture_name(&mut self, filename: &str) -> bool {
        self.texture_name = filename.to_string();
        true
    }

    /// `bumpTextureName()` (`terrain_texture_layer2.cpp:92-103`).
    pub fn bump_texture_name(&self) -> &str {
        &self.bump_texture_name
    }

    /// `bumpTextureName(filename)` (`:139-165`): empty means "no bump".
    pub fn set_bump_texture_name(&mut self, filename: &str) -> bool {
        self.bump_texture_name = filename.to_string();
        true
    }

    /// The saved `uProjection_` (`terrain_texture_layer2.hpp:90`).
    pub fn u_projection(&self) -> [f32; 4] {
        self.u_projection
    }

    /// The saved `vProjection_` (`terrain_texture_layer2.hpp:91`).
    pub fn v_projection(&self) -> [f32; 4] {
        self.v_projection
    }

    /// `setProjection` pair (`terrain_texture_layer2.cpp:249-286`).
    pub fn set_projections(&mut self, u: [f32; 4], v: [f32; 4]) {
        self.u_projection = u;
        self.v_projection = v;
    }

    /// The blend weights (`image()`), row-major over
    /// `width() * height()`.
    pub fn blends(&self) -> &U8Image {
        &self.blends
    }

    /// The funnel write (`terrain_texture_functor.cpp` paints through
    /// `image().set` between a lock pair; the lock bookkeeping is the
    /// GUI's own state).
    pub fn set_blend(&mut self, x: i32, y: i32, value: u8) {
        self.blends.set(x, y, value);
    }

    /// The layer a new-space template gets (`world_manager.cpp:8000-8012`):
    /// assign the texture, then fill the blends full white under a lock
    /// pair, then the default UV projections are already in place from the
    /// `set_texture` path.
    pub fn fill_white(&mut self) {
        let width = self.blends.width();
        let height = self.blends.height();
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                self.blends.set(x, y, u8::MAX);
            }
        }
    }

    /// `TerrainTextureLayer2::save` (`terrain_texture_layer2.cpp:378-448`):
    /// the `BlendHeader` (always `bpp = 8`, always
    /// `VERSION_PNG_BLENDS`), the texture name as a u32 length plus that
    /// many bytes (`strncpy` without a terminator), the same again for
    /// the bump name when present, then the PNG-compressed blends
    /// (`canZip(false)` on the wrapper).
    pub fn save(&self) -> Vec<u8> {
        let blend_png = png::compress_png(
            self.blends.width(),
            self.blends.height(),
            8,
            self.blends.image_data(),
        );

        let mut out = Vec::with_capacity(
            BLEND_HEADER_SIZE
                + 4
                + self.texture_name.len()
                + if self.bump_texture_name.is_empty() {
                    0
                } else {
                    4 + self.bump_texture_name.len()
                }
                + blend_png.len(),
        );
        out.extend_from_slice(&BLEND_MAGIC.to_le_bytes());
        out.extend_from_slice(&self.blends.width().to_le_bytes());
        out.extend_from_slice(&self.blends.height().to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes());
        for word in self.u_projection {
            out.extend_from_slice(&word.to_le_bytes());
        }
        for word in self.v_projection {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&BLEND_VERSION_PNG.to_le_bytes());
        out.extend_from_slice(&u32::from(!self.bump_texture_name.is_empty()).to_le_bytes());
        out.extend_from_slice(&[0u8; 8]);

        out.extend_from_slice(&(self.texture_name.len() as u32).to_le_bytes());
        out.extend_from_slice(self.texture_name.as_bytes());
        if !self.bump_texture_name.is_empty() {
            out.extend_from_slice(&(self.bump_texture_name.len() as u32).to_le_bytes());
            out.extend_from_slice(self.bump_texture_name.as_bytes());
        }

        out.extend_from_slice(&blend_png);
        out
    }

    /// `TerrainTextureLayer2::load` (`terrain_texture_layer2.cpp:470-648`).
    pub fn load(data: &[u8]) -> Result<Self, LayerLoadError> {
        if data.len() < BLEND_HEADER_SIZE {
            return Err(LayerLoadError::Truncated);
        }
        let word = |off: usize| {
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };
        let floats = |off: usize| {
            [
                f32::from_le_bytes(data[off..off + 4].try_into().unwrap()),
                f32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap()),
                f32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap()),
                f32::from_le_bytes(data[off + 12..off + 16].try_into().unwrap()),
            ]
        };
        if word(0) != BLEND_MAGIC {
            return Err(LayerLoadError::WrongMagic);
        }
        let width = word(4);
        let height = word(8);
        // `bpp_` is "reserved for future use" and never read back.
        let u_projection = floats(16);
        let v_projection = floats(32);
        let version = word(48);
        let has_bump_map = word(52) != 0;
        if version != BLEND_VERSION_RAW && version != BLEND_VERSION_PNG {
            return Err(LayerLoadError::WrongVersion(version));
        }

        // u32 length + bytes; the reference re-trims the assignment at the
        // first NUL (`:520-526`, the `c_str()` round trip).
        let mut cursor = BLEND_HEADER_SIZE;
        let read_name = |cursor: &mut usize| -> Result<String, LayerLoadError> {
            if *cursor + 4 > data.len() {
                return Err(LayerLoadError::Truncated);
            }
            let len = word(*cursor) as usize;
            *cursor += 4;
            let end = cursor.checked_add(len).ok_or(LayerLoadError::Truncated)?;
            if end > data.len() {
                return Err(LayerLoadError::Truncated);
            }
            let window = &data[*cursor..end];
            let stop = window.iter().position(|&b| b == 0).unwrap_or(window.len());
            *cursor = end;
            Ok(String::from_utf8_lossy(&window[..stop]).into_owned())
        };

        let texture_name = read_name(&mut cursor)?;
        let bump_texture_name = if has_bump_map {
            read_name(&mut cursor)?
        } else {
            String::new()
        };

        // `sizeData` is whatever remains; raw copies exactly `width *
        // height` bytes, PNG consumes the rest.
        let blend_data = &data[cursor..];

        let mut blends = U8Image::new(width, height);
        match version {
            BLEND_VERSION_RAW => {
                let need = width as usize * height as usize;
                if blend_data.len() < need {
                    return Err(LayerLoadError::Truncated);
                }
                blends.copy_from(blend_data);
            }
            _ => {
                let image =
                    png::decompress_png(blend_data).map_err(|_| LayerLoadError::InvalidBlendPng)?;
                // `MF_ASSERT(blends_.width() == pHeader->width_)` etc.
                // (`:625-626`).
                assert!(
                    image.width == width && image.height == height,
                    "reference asserts the decoded blend size"
                );
                blends.copy_from(&image.data[..(width as usize * height as usize)]);
            }
        }

        Ok(Self {
            texture_name,
            bump_texture_name,
            u_projection,
            v_projection,
            blends,
        })
    }
}

/// `EditorTerrainBlock2::load` validation failures
/// (`editor_terrain_block2.cpp:198-234`). Each variant is one `ERROR_MSG`
/// the reference prints before failing the load; `filename` is the block
/// resource the message interpolates.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorLoadError {
    /// Height/hole/layer reader failure surfaces verbatim.
    Map(String),
    /// "No texture layers for terrain block %s" (`:211-217`).
    NoTextureLayers(String),
    /// "Invalid texture layers for terrain block %s" (`:218-227`).
    InvalidTextureLayers(String),
    /// "Invalid hole map for terrain block %s" (`:228-234`).
    InvalidHoleMap(String),
}

impl core::fmt::Display for EditorLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EditorLoadError::Map(what) => f.write_str(what),
            EditorLoadError::NoTextureLayers(name) => {
                f.write_str(&format!("No texture layers for terrain block {name}\n"))
            }
            EditorLoadError::InvalidTextureLayers(name) => f.write_str(&format!(
                "Invalid texture layers for terrain block {name}\n"
            )),
            EditorLoadError::InvalidHoleMap(name) => {
                f.write_str(&format!("Invalid hole map for terrain block {name}\n"))
            }
        }
    }
}

impl std::error::Error for EditorLoadError {}

/// `EditorTerrainBlock2` minus the renderer: the height map, the hole
/// map, the texture layers and the dominant map, plus the settings the
/// save/load faces consult. The reference reads those settings from the
/// space's `<terrain>` section with `system/terrain2` defaults; the port
/// takes them explicitly.
pub struct EditorTerrainBlock2 {
    block_size: f32,
    height_map_editor_size: u32,
    hole_map_size: u32,
    blend_map_size: u32,
    num_vertex_lods: u32,
    height_map: TerrainHeightMap2,
    hole_map: TerrainHoleMap2,
    layers: Vec<TextureLayer2>,
    dominant: Option<DominantTextureMap2>,
    /// `forcedLod_` (`terrain_block2.cpp:246`): starts at -1.
    forced_lod: i32,
}

impl EditorTerrainBlock2 {
    /// `EditorTerrainBlock2::create` (`editor_terrain_block2.cpp:69-101`):
    /// a fresh height map at the editor size, a hole-less hole map, and —
    /// unlike the reference — no renderer maps, so nothing to rebuild.
    pub fn create(
        block_size: f32,
        height_map_editor_size: u32,
        hole_map_size: u32,
        blend_map_size: u32,
        num_vertex_lods: u32,
    ) -> Result<Self, HeightCreateError> {
        let mut height_map = TerrainHeightMap2::new(block_size, 0);
        height_map.create(height_map_editor_size)?;
        let mut hole_map = TerrainHoleMap2::new(block_size);
        hole_map.create(hole_map_size);
        Ok(Self {
            block_size,
            height_map_editor_size,
            hole_map_size,
            blend_map_size,
            num_vertex_lods,
            height_map,
            hole_map,
            layers: Vec::new(),
            dominant: None,
            forced_lod: -1,
        })
    }

    /// `blockSize()`.
    pub fn block_size(&self) -> f32 {
        self.block_size
    }

    /// The settings the reference reads off `settings_`
    /// (`terrain_settings.cpp`): the editor height-map size, the hole-map
    /// size, the blend-map size and the vertex-lod count.
    pub fn height_map_editor_size(&self) -> u32 {
        self.height_map_editor_size
    }

    /// `settings_->holeMapSize()`.
    pub fn hole_map_size(&self) -> u32 {
        self.hole_map_size
    }

    /// `settings_->blendMapSize()`.
    pub fn blend_map_size(&self) -> u32 {
        self.blend_map_size
    }

    /// `settings_->numVertexLods()`.
    pub fn num_vertex_lods(&self) -> u32 {
        self.num_vertex_lods
    }

    /// `heightMap2()`.
    pub fn height_map(&self) -> &TerrainHeightMap2 {
        &self.height_map
    }

    /// Mutable `heightMap2()` for the edit funnels.
    pub fn height_map_mut(&mut self) -> &mut TerrainHeightMap2 {
        &mut self.height_map
    }

    /// `holeMap2()`.
    pub fn hole_map(&self) -> &TerrainHoleMap2 {
        &self.hole_map
    }

    /// Mutable `holeMap2()` for the edit funnels.
    pub fn hole_map_mut(&mut self) -> &mut TerrainHoleMap2 {
        &mut self.hole_map
    }

    /// `numberTextureLayers()` (`editor_terrain_block2.cpp:324-336`).
    pub fn number_texture_layers(&self) -> usize {
        self.layers.len()
    }

    /// `maxNumberTextureLayers()` (`:338-342`): the reference answers
    /// `size_t`'s maximum.
    pub fn max_number_texture_layers(&self) -> usize {
        usize::MAX
    }

    /// `textureLayer(idx)`.
    ///
    /// # Panics
    /// Out-of-range indices panic, like the reference's unchecked
    /// `textureLayers_[idx]`.
    pub fn texture_layer(&self, idx: usize) -> &TextureLayer2 {
        &self.layers[idx]
    }

    /// Mutable `textureLayer(idx)` for the paint funnels.
    ///
    /// # Panics
    /// Out-of-range indices panic, like the reference.
    pub fn texture_layer_mut(&mut self, idx: usize) -> &mut TextureLayer2 {
        &mut self.layers[idx]
    }

    /// `EditorTerrainBlock2::insertTextureLayer`
    /// (`editor_terrain_block2.cpp:347-362`): push a default-sized layer,
    /// answer its index.
    pub fn insert_texture_layer(&mut self) -> usize {
        self.layers
            .push(TextureLayer2::new(self.blend_map_size, self.blend_map_size));
        self.layers.len() - 1
    }

    /// `EditorTerrainBlock2::removeTextureLayer` (`:364-374`): the erase
    /// happens but the function returns false — quirk preserved.
    pub fn remove_texture_layer(&mut self, idx: usize) -> bool {
        if idx < self.layers.len() {
            self.layers.remove(idx);
        }
        false
    }

    /// `dominantTextureMap()` — present only when a block carried one.
    pub fn dominant_texture_map(&self) -> Option<&DominantTextureMap2> {
        self.dominant.as_ref()
    }

    /// Mutable `dominantTextureMap()`.
    pub fn dominant_texture_map_mut(&mut self) -> Option<&mut DominantTextureMap2> {
        self.dominant.as_mut()
    }

    /// `getForcedLod()`.
    pub fn forced_lod(&self) -> i32 {
        self.forced_lod
    }

    /// `forceLodFeedback`/`setForcedLod`
    /// (`terrain_block2.cpp:1180-1190` clamps into the lod range; the
    /// clamp belongs to the lod manager face, so the setter stores raw).
    pub fn set_forced_lod(&mut self, forced_lod: i32) {
        self.forced_lod = forced_lod;
    }

    /// `EditorTerrainBlock2::save(DataSectionPtr)`
    /// (`editor_terrain_block2.cpp:234-292`) as the ordered
    /// section-name/payload list a zip face writes: heights (plus one
    /// down-sampled section per vertex lod above the base), holes when
    /// there are any, one `layer` section per texture layer, the dominant
    /// map when present, and the metadata XML. The renderer-owned
    /// sub-blocks (normal/AO/horizon shadows, lod texture, the legacy
    /// 1.9 vertex lods) have no face in this tree and are not emitted;
    /// each has its own `deleteSections`/`save` pair in the reference.
    pub fn save(&self) -> Result<Vec<(String, Vec<u8>)>, DominantSaveError> {
        let mut sections: Vec<(String, Vec<u8>)> = Vec::new();

        // `saveHeightmap` (`:785-848`): the base map under "heights", then
        // `AliasedHeightMap` down-samples under "heights%d".
        sections.push((HEIGHTS_SECTION_NAME.into(), self.height_map.save()));
        let src_vis = self.height_map.vertices_width();
        for lod in 1..self.num_vertex_lods {
            let lod_size = (src_vis >> lod) + 1;
            let mut down = TerrainHeightMap2::with_size(self.block_size, lod_size, lod);
            for z in 0..lod_size {
                for x in 0..lod_size {
                    let h = self
                        .height_map
                        .height_at((x << lod) as i32, (z << lod) as i32);
                    down.set_pole(x as i32, z as i32, h);
                }
            }
            sections.push((format!("{}{lod}", HEIGHTS_SECTION_NAME), down.save()));
        }

        // `holes` only when there is something to save (`:263-268`).
        if !self.hole_map.no_holes() {
            sections.push((HOLES_SECTION_NAME.into(), self.hole_map.save()));
        }

        for layer in &self.layers {
            sections.push((LAYER_SECTION_NAME.into(), layer.save()));
        }

        if let Some(dominant) = &self.dominant {
            sections.push((DOMINANT_TEXTURES_SECTION_NAME.into(), dominant.save()?));
        }

        // `TerrainBlockMetaData` is always created (`openSection(...,
        // true, XMLSection::creator())`, `:286-292`) and always carries
        // `ForcedLod`, even at its -1 default.
        let mut meta =
            resources::datasection::DataSection::new(TERRAIN_BLOCK_META_DATA_SECTION_NAME);
        meta.write_int(FORCED_LOD_ATTRIBUTE_NAME, self.forced_lod);
        sections.push((
            TERRAIN_BLOCK_META_DATA_SECTION_NAME.into(),
            meta.write_to_string().into_bytes(),
        ));

        Ok(sections)
    }

    /// `EditorTerrainBlock2::saveToCData` (`editor_terrain_block2.cpp:293-314`):
    /// the section list becomes a nested zip under `terrain2` inside the
    /// chunk `.cdata`, which is then re-serialised. An existing `terrain2`
    /// entry is replaced, exactly like `deleteSections` before `newSection`.
    pub fn save_to_cdata(&self, cdata: &mut ZipFileSystem) -> bool {
        let Ok(sections) = self.save() else {
            return false;
        };
        let mut inner = ZipFileSystem::open("", ZipFileSystem::empty_archive())
            .expect("an empty archive always parses");
        for (name, bytes) in sections {
            inner.write_to_structure(&name, bytes);
        }
        cdata.write_to_structure(TERRAIN2_SECTION_NAME, inner.create_zip())
    }

    /// `EditorTerrainBlock2::save(filename)` (`:316-322`): open (or
    /// create) the `.cdata` zip and run [`Self::save_to_cdata`] on it.
    ///
    /// The reference reaches the zip through `BWResource::openSection`, so
    /// the `ZipSection` layer holds the children in authority: before
    /// `createZip` it recollects every readable child as a fresh row
    /// (`ZipSection::asBinary`, `zip_section.cpp:529-546`). Rows parsed
    /// straight out of an archive carry no bytes and `createZip` drops
    /// them, so the port re-materialises the existing entries the same
    /// way before mutating.
    pub fn save_file(
        &self,
        file_system: &resources::multi_file_system::MultiFileSystem,
        filename: &str,
    ) -> bool {
        let raw = file_system.read_file(filename).unwrap_or_default();
        let mut cdata = ZipFileSystem::open("", ZipFileSystem::empty_archive())
            .expect("an empty archive always parses");
        if let Some(existing) = ZipFileSystem::open(filename, raw) {
            for index in 0..existing.file_count("") {
                let Some(name) = existing.file_name("", index) else {
                    continue;
                };
                if let Some(bytes) = existing.read_file_at("", index) {
                    cdata.write_to_structure(&name, bytes);
                }
            }
        }
        if !self.save_to_cdata(&mut cdata) {
            return false;
        }
        file_system.write_file(filename, &cdata.create_zip())
    }

    /// The load half of the editor chain: read `terrain2` back out of a
    /// chunk `.cdata`, hand the sub-sections to the map readers, then run
    /// the `EditorTerrainBlock2::load` validations
    /// (`editor_terrain_block2.cpp:173-234`). The normal-map and horizon
    /// checks belong to renderer faces this tree does not carry.
    ///
    /// `filename` only feeds the error messages, exactly as in the
    /// reference.
    pub fn load(
        block_size: f32,
        height_map_editor_size: u32,
        hole_map_size: u32,
        blend_map_size: u32,
        num_vertex_lods: u32,
        cdata: &ZipFileSystem,
        filename: &str,
    ) -> Result<Self, EditorLoadError> {
        let mut block = Self::create(
            block_size,
            height_map_editor_size,
            hole_map_size,
            blend_map_size,
            num_vertex_lods,
        )
        .map_err(|e| EditorLoadError::Map(e.to_string()))?;

        let terrain2 = cdata
            .read_file(TERRAIN2_SECTION_NAME)
            .ok_or_else(|| EditorLoadError::Map("no terrain2 entry".into()))?;
        let inner = ZipFileSystem::open(TERRAIN2_SECTION_NAME, terrain2)
            .ok_or_else(|| EditorLoadError::Map("terrain2 entry is not a zip".into()))?;

        // Heights: the base map is mandatory, the lod sections are
        // renderer-streaming data the editor load re-derives.
        let heights = inner
            .read_file(HEIGHTS_SECTION_NAME)
            .ok_or_else(|| EditorLoadError::Map("no heights section".into()))?;
        block
            .height_map
            .load(&heights)
            .map_err(|e| EditorLoadError::Map(e.to_string()))?;

        if let Some(holes) = inner.read_file(HOLES_SECTION_NAME) {
            // The resource layer hands over decompressed blocks; here the
            // section still carries the `BinaryBlock::compress` wrapper.
            let holes = crate::dominant_texture_map::zip_decompress(&holes);
            block
                .hole_map
                .load(Some(&holes))
                .map_err(|e| EditorLoadError::Map(e.to_string()))?;
        }

        // One `layer` row per texture layer, in zip order — repeated
        // section tags are packed as duplicate-encoded zip rows and the
        // dir listing answers their decoded names, which is what the
        // reference chain reads through `openSections("layer")`
        // (`terrain_blends.cpp:75`).
        let mut layers = Vec::new();
        for index in 0..inner.file_count("") {
            let Some(name) = inner.file_name("", index) else {
                continue;
            };
            if name != LAYER_SECTION_NAME {
                continue;
            }
            let bytes = inner
                .read_file_at("", index)
                .ok_or_else(|| EditorLoadError::Map("layer row is unreadable".into()))?;
            layers.push(
                TextureLayer2::load(&bytes).map_err(|e| EditorLoadError::Map(e.to_string()))?,
            );
        }
        block.layers = layers;

        if let Some(dominant) = inner.read_file(DOMINANT_TEXTURES_SECTION_NAME) {
            let mut map = DominantTextureMap2::new(block_size);
            map.load(Some(&dominant))
                .map_err(|e| EditorLoadError::Map(e.to_string()))?;
            block.dominant = Some(map);
        }

        if let Some(meta) = inner.read_file(TERRAIN_BLOCK_META_DATA_SECTION_NAME)
            && let Ok(section) =
                resources::datasection::parse(TERRAIN_BLOCK_META_DATA_SECTION_NAME, &meta)
        {
            block.forced_lod = section.read_int(FORCED_LOD_ATTRIBUTE_NAME, -1);
        }

        // The validation run (`:198-234`), in reference order minus the
        // two renderer checks.
        if block.number_texture_layers() == 0 {
            return Err(EditorLoadError::NoTextureLayers(filename.into()));
        }
        if block.texture_layer(0).blends().width() != blend_map_size {
            return Err(EditorLoadError::InvalidTextureLayers(filename.into()));
        }
        if block.hole_map.holes_size() != hole_map_size {
            return Err(EditorLoadError::InvalidHoleMap(filename.into()));
        }

        Ok(block)
    }
}

/// The chunk item save face (`EditorChunkTerrain::edSave`,
/// `editor_chunk_terrain.cpp:388-395`): write the `resource` reference the
/// chunk uses to find its block — `identifier + ".cdata/" + terrain2`.
/// The reference also runs `edCommonSave` first; that hook writes no
/// terrain fields, so the port has nothing to call.
pub fn write_terrain_section(
    section: &mut resources::datasection::DataSection,
    chunk_identifier: &str,
) -> bool {
    let resource = format!("{chunk_identifier}.cdata/{TERRAIN2_SECTION_NAME}");
    section.write_string("resource", &resource)
}

/// `EditorChunkTerrain::toss` resource id (`editor_chunk_terrain.cpp:603-607`):
/// the block's full resource name carries the mapping path.
pub fn terrain_resource_id(mapping_path: &str, chunk_identifier: &str) -> String {
    format!("{mapping_path}{chunk_identifier}.cdata/{TERRAIN2_SECTION_NAME}")
}

/// What a terrain block needs to be bookkept and written back
/// (`EditorBaseTerrainBlock`): its full resource name and the
/// `save(filename)` that re-serialises its `.cdata`.
pub trait TerrainBlockFile {
    /// `resourceName()`.
    fn resource_name(&self) -> &str;
    /// `save(filename)`.
    fn save(&self, filename: &str) -> bool;
}

/// `UnsavedTerrainBlocks::save` filename derivation
/// (`unsaved_terrain_blocks.cpp:41-49`): keep the six bytes from the last
/// dot — exactly the `.cdata` suffix — which strips the `/terrain2` leaf
/// and answers the `.cdata` file path the block save expects.
pub fn unsaved_block_save_name(resource_name: &str) -> String {
    match resource_name.rfind('.') {
        Some(pos) => {
            let end = (pos + 6).min(resource_name.len());
            resource_name[..end].to_string()
        }
        // The reference indexes past `npos` here; with no dot there is no
        // `.cdata` tail to trim, so the port keeps the name (registered
        // divergence on an unreachable-in-practice path).
        None => resource_name.to_string(),
    }
}

/// `UnsavedTerrainBlocks` (`lib/chunk/unsaved_terrain_blocks.cpp`): the
/// blocks changed since the last save, keyed by resource name.
#[derive(Default)]
pub struct UnsavedTerrainBlocks {
    blocks: BTreeMap<String, Box<dyn TerrainBlockFile>>,
    saved_blocks: BTreeMap<String, ()>,
}

impl UnsavedTerrainBlocks {
    /// `add` (`unsaved_terrain_blocks.cpp:15-21`).
    pub fn add(&mut self, block: Box<dyn TerrainBlockFile>) {
        let name = block.resource_name().to_string();
        self.blocks.insert(name, block);
    }

    /// `clear` (`:23-28`).
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.saved_blocks.clear();
    }

    /// `empty` (`:60-63`).
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// `save` (`:30-58`): every pending block is written (through its
    /// derived `.cdata` name) and recorded as saved, failures included —
    /// the reference never inspects `save`'s result.
    pub fn save(&mut self) {
        let names: Vec<String> = self.blocks.keys().cloned().collect();
        for name in names {
            let block = &self.blocks[&name];
            let filename = unsaved_block_save_name(&name);
            let _ = block.save(&filename);
            self.saved_blocks.insert(name, ());
        }
    }

    /// `filter` (`:65-82`): drop everything the other set already saved.
    pub fn filter(&mut self, utb: &UnsavedTerrainBlocks) {
        self.blocks
            .retain(|name, _| !utb.saved_blocks.contains_key(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{zip_compress, zip_decompress};
    use resources::file_system::NativeFileSystem;
    use resources::multi_file_system::MultiFileSystem;

    const BLOCK: f32 = 100.0;

    /// The quantised save carries millimetre-grid heights; the f32 image of
    /// e.g. 12.5 is `unquantise(quantise(12.5))`, not 12.5 itself. Every
    /// pole comparison goes through this.
    fn within_mm(got: f32, want: f32) -> bool {
        (got - want).abs() <= crate::height_map::QUANTISATION_LEVEL
    }

    /// A blend-sized layer carrying just its texture name.
    fn named_layer(name: &str) -> Vec<u8> {
        let mut layer = TextureLayer2::new(32, 32);
        layer.set_texture_name(name);
        layer.save()
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("terredit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture(tag: &str) -> (std::path::PathBuf, MultiFileSystem) {
        let root = temp_root(tag);
        std::fs::create_dir_all(root.join("space")).unwrap();
        let mut file_system = MultiFileSystem::new();
        file_system.add_base_file_system(
            Box::new(NativeFileSystem::new(root.to_str().expect("utf8 root"))),
            -1,
        );
        (root, file_system)
    }

    /// A dominant map section payload in the save layout — the editor test
    /// builds real ones through [`DominantTextureMap2::save`], this is the
    /// raw-bytes loader fixture.
    fn dominant_blob(names: &[&str], w: u32, h: u32, image: &[u8]) -> Vec<u8> {
        let tns = names.iter().map(|n| n.len()).max().unwrap() as u32;
        let mut b = Vec::new();
        b.extend_from_slice(&crate::dominant_texture_map::DOMINANT_MAGIC.to_le_bytes());
        b.extend_from_slice(&crate::dominant_texture_map::DOMINANT_VERSION_ZIP.to_le_bytes());
        b.extend_from_slice(&(names.len() as u32).to_le_bytes());
        b.extend_from_slice(&tns.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&[0u8; 8]);
        for n in names {
            let mut win = vec![0u8; tns as usize];
            win[..n.len()].copy_from_slice(n.as_bytes());
            b.extend_from_slice(&win);
        }
        b.extend_from_slice(&zip_compress(image));
        b
    }

    /// `block_size` 100, editor heights 64, holes 32, blends 32, 3 vertex
    /// lods — one block, edited and saved, per the acceptance run.
    fn edited_block() -> EditorTerrainBlock2 {
        let mut block = EditorTerrainBlock2::create(BLOCK, 64, 32, 32, 3).unwrap();
        // Height edits: absolute and relative funnel writes, on the visible
        // grid; every value lands on the millimetre grid so the quantised
        // round trip is exact.
        block.height_map_mut().set_pole(34, 34, 12.5);
        block.height_map_mut().set_pole(35, 34, -3.25);
        block.height_map_mut().add_pole(36, 36, 7.0);

        // Hole edits under the funnel pattern: lock, paint, unlock.
        let holes = block.hole_map_mut();
        holes.lock(false);
        holes.image_mut().set(5, 5, true);
        holes.image_mut().set(6, 5, true);
        holes.unlock();

        // The material face: one layer painted like the new-space template
        // plus one edited layer with a bump map and moved projections.
        let idx = block.insert_texture_layer();
        {
            let layer = block.texture_layer_mut(idx);
            layer.set_texture_name("environments/default/grass.texture");
            let (u, v) = default_uv_projections();
            layer.set_projections(u, v);
            layer.fill_white();
        }
        let idx = block.insert_texture_layer();
        {
            let layer = block.texture_layer_mut(idx);
            layer.set_texture_name("environments/default/rock.texture");
            layer.set_bump_texture_name("environments/default/rock_bm.texture");
            layer.set_projections([0.2, 0.0, 0.0, 0.0], [0.0, 0.0, 0.2, 0.0]);
            layer.set_blend(1, 2, 200);
            layer.set_blend(31, 31, 55);
        }

        // A dominant map rides along, as the reference block carries one.
        let mut dominant = DominantTextureMap2::new(BLOCK);
        dominant
            .load(Some(&dominant_blob(
                &[
                    "environments/default/grass.texture",
                    "environments/default/rock.texture",
                ],
                16,
                16,
                &[0x12u8; 256],
            )))
            .unwrap();
        block.dominant = Some(dominant);

        block.set_forced_lod(2);
        block
    }

    #[test]
    fn png_round_trips_both_terrain_shapes() {
        // gray8: the blends shape.
        let gray: Vec<u8> = (0..25u8).collect();
        let encoded = png::compress_png(5, 5, 8, &gray);
        let decoded = png::decompress_png(&encoded).unwrap();
        assert_eq!(decoded.bpp, 8);
        assert_eq!(decoded.data, gray);

        // rgba8: the quantised height shape — the words must survive.
        let words: Vec<i32> = [
            0,
            1,
            -1,
            12500,
            -12500,
            i32::MAX,
            i32::MIN,
            7,
            8,
            9,
            10,
            11,
            12,
            13,
            14,
            15,
        ]
        .to_vec();
        let mut bytes = Vec::new();
        for w in &words {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let encoded = png::compress_png(4, 4, 32, &bytes);
        let decoded = png::decompress_png(&encoded).unwrap();
        assert_eq!(decoded.bpp, 32);
        let back: Vec<i32> = (0..16).map(|i| decoded.word(i % 4, i / 4)).collect();
        assert_eq!(back, words);
    }

    #[test]
    fn height_create_rejects_non_power_of_two_with_the_reference_message() {
        let mut map = TerrainHeightMap2::new(BLOCK, 0);
        assert_eq!(map.create(63), Err(HeightCreateError));
        assert_eq!(
            HeightCreateError.to_string(),
            "Height map size is not a power of two."
        );
    }

    #[test]
    fn height_save_and_load_agree_after_edits() {
        let mut map = TerrainHeightMap2::new(BLOCK, 0);
        map.create(64).unwrap();
        // Funnel writes land on full-image pole coordinates.
        map.set_pole(34, 34, 12.5);
        map.set_pole(5, 6, -3.25);
        map.add_pole(36, 36, 7.0);

        let saved = map.save();
        // Header: COMPRESS_RAW + VERSION_ABS_QFLOAT, real min/max up front.
        let (header, payload) = crate::height_map::HeightMapHeader::parse(&saved).unwrap();
        assert_eq!(header.magic, crate::height_map::HEIGHT_MAP_MAGIC);
        assert_eq!(
            header.compression,
            crate::height_map::HEIGHT_MAP_COMPRESS_RAW
        );
        assert_eq!(
            header.version,
            crate::height_map::HEIGHT_MAP_VERSION_ABS_QFLOAT
        );
        assert_eq!(header.width, 69);
        assert_eq!(header.height, 69);
        assert_eq!(header.min_height, -3.25);
        assert_eq!(header.max_height, 12.5);
        // The payload starts with the 'qpng' version word, stored
        // little-endian like every header u32.
        assert_eq!(
            &payload[..4],
            &crate::height_map::QUANTISED_PNG_VERSION.to_le_bytes()
        );

        let mut reloaded = TerrainHeightMap2::new(BLOCK, 0);
        reloaded.load(&saved).unwrap();
        assert_eq!(reloaded.min_height(), -3.25);
        assert_eq!(reloaded.max_height(), 12.5);
        // heightAt answers the visible pole face — image minus the lod-0
        // visible offset of 2.
        assert!(within_mm(reloaded.height_at(32, 32), 12.5));
        assert!(within_mm(reloaded.height_at(3, 4), -3.25));
        // The relative funnel write landed.
        assert!(within_mm(reloaded.height_at(34, 34), 7.0));
    }

    #[test]
    fn hole_save_and_load_agree_and_skip_when_there_are_none() {
        let mut holes = TerrainHoleMap2::new(BLOCK);
        holes.create(32);
        // A hole-less map saves nothing — the block save omits the section.
        let empty = holes.save();
        let inflated = zip_decompress(&empty);
        let (header, bytes) = crate::hole_map::HolesHeader::parse(&inflated).unwrap();
        assert_eq!(header.magic, crate::hole_map::HOLES_MAGIC);
        assert_eq!(header.version, 1);
        assert!(bytes.iter().all(|&b| b == 0));

        holes.lock(false);
        holes.image_mut().set(5, 5, true);
        holes.image_mut().set(31, 31, true);
        holes.unlock();
        assert!(!holes.no_holes());

        let mut reloaded = TerrainHoleMap2::new(BLOCK);
        let saved = holes.save();
        let inflated = zip_decompress(&saved);
        reloaded.load(Some(&inflated)).unwrap();
        // image() MF_ASSERTs a lock; the editor locks before reading.
        reloaded.lock(false);
        assert!(reloaded.image().get(5, 5));
        assert!(reloaded.image().get(31, 31));
        assert!(!reloaded.image().get(6, 6));
        assert_eq!(reloaded.holes_size(), 32);
        reloaded.unlock();
    }

    #[test]
    fn layer_save_and_load_agree() {
        let mut layer = TextureLayer2::new(32, 32);
        layer.set_texture_name("environments/default/rock.texture");
        layer.set_bump_texture_name("environments/default/rock_bm.texture");
        layer.set_projections([0.2, 0.0, 0.0, 0.0], [0.0, 0.0, 0.2, 0.0]);
        layer.set_blend(1, 2, 200);
        layer.set_blend(31, 31, 55);

        let saved = layer.save();
        // BlendHeader fields on the wire: magic, size, bpp 8, PNG version,
        // hasBumpMap, then the names and the blend PNG.
        assert_eq!(
            u32::from_le_bytes(saved[0..4].try_into().unwrap()),
            BLEND_MAGIC
        );
        assert_eq!(u32::from_le_bytes(saved[4..8].try_into().unwrap()), 32);
        assert_eq!(u32::from_le_bytes(saved[12..16].try_into().unwrap()), 8);
        assert_eq!(
            u32::from_le_bytes(saved[48..52].try_into().unwrap()),
            BLEND_VERSION_PNG
        );
        assert_eq!(u32::from_le_bytes(saved[52..56].try_into().unwrap()), 1);
        let name_len = u32::from_le_bytes(
            saved[BLEND_HEADER_SIZE..BLEND_HEADER_SIZE + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        assert_eq!(
            &saved[BLEND_HEADER_SIZE + 4..BLEND_HEADER_SIZE + 4 + name_len],
            b"environments/default/rock.texture"
        );

        let reloaded = TextureLayer2::load(&saved).unwrap();
        assert_eq!(reloaded.texture_name(), "environments/default/rock.texture");
        assert_eq!(
            reloaded.bump_texture_name(),
            "environments/default/rock_bm.texture"
        );
        assert_eq!(reloaded.u_projection(), [0.2, 0.0, 0.0, 0.0]);
        assert_eq!(reloaded.v_projection(), [0.0, 0.0, 0.2, 0.0]);
        assert_eq!(reloaded.blends().get(1, 2), 200);
        assert_eq!(reloaded.blends().get(31, 31), 55);
        assert_eq!(reloaded.blends().get(0, 0), 0);
    }

    #[test]
    fn layer_load_rejects_bad_versions_with_the_reference_message() {
        let mut layer = TextureLayer2::new(4, 4);
        layer.set_texture_name("a.texture");
        let mut saved = layer.save();
        saved[48] = 9; // blend version word
        let err = TextureLayer2::load(&saved).expect_err("the load fails");
        assert_eq!(err, LayerLoadError::WrongVersion(9));
        // Two spaces after the period, straight from the format string.
        assert_eq!(
            err.to_string(),
            "Blend data is incorrect version.  Wanted 1 or 2, got 9\n"
        );

        saved[0] ^= 0xff;
        assert!(matches!(
            TextureLayer2::load(&saved),
            Err(LayerLoadError::WrongMagic)
        ));
    }

    #[test]
    fn layer_load_reads_the_raw_blend_version() {
        // VERSION_RAW_BLENDS carries the weights unpacked.
        let mut raw = Vec::new();
        raw.extend_from_slice(&BLEND_MAGIC.to_le_bytes());
        raw.extend_from_slice(&4u32.to_le_bytes());
        raw.extend_from_slice(&4u32.to_le_bytes());
        raw.extend_from_slice(&8u32.to_le_bytes());
        // uProjection then vProjection, eight words over the 64-byte header.
        raw.extend_from_slice(&0.5f32.to_le_bytes());
        for _ in 0..7 {
            raw.extend_from_slice(&0.0f32.to_le_bytes());
        }
        raw.extend_from_slice(&BLEND_VERSION_RAW.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&[0u8; 8]);
        raw.extend_from_slice(&(9u32.to_le_bytes()));
        raw.extend_from_slice(b"a.texture");
        raw.extend_from_slice(&[
            10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160,
        ]);

        let layer = TextureLayer2::load(&raw).unwrap();
        assert_eq!(layer.texture_name(), "a.texture");
        assert_eq!(layer.blends().get(0, 0), 10);
        assert_eq!(layer.blends().get(3, 3), 160);
    }

    #[test]
    fn dominant_save_refuses_empty_and_round_trips() {
        let empty = DominantTextureMap2::new(BLOCK);
        assert_eq!(empty.save(), Err(DominantSaveError));
        assert_eq!(
            DominantSaveError.to_string(),
            "Failed to save dominant texture map: empty texture names and/or image\n"
        );

        let mut map = DominantTextureMap2::new(BLOCK);
        map.load(Some(&dominant_blob(
            &[
                "environments/default/grass.texture",
                "environments/default/rock.texture",
            ],
            16,
            16,
            &[0x12u8; 256],
        )))
        .unwrap();

        let saved = map.save().unwrap();
        let mut reloaded = DominantTextureMap2::new(BLOCK);
        reloaded.load(Some(&saved)).unwrap();
        assert_eq!(reloaded.texture_names(), map.texture_names());
        for y in 0..16i32 {
            for x in 0..16i32 {
                assert_eq!(reloaded.ground_type(x as f32, y as f32), 1);
                assert_eq!(reloaded.material_index(x as f32, y as f32), 2);
            }
        }
    }

    #[test]
    fn default_uv_projections_match_the_texture_spacing() {
        let (u, v) = default_uv_projections();
        assert_eq!(u, [0.1, 0.0, 0.0, 0.0]);
        assert_eq!(v, [0.0, 0.0, 0.1, 0.0]);
    }

    #[test]
    fn block_layer_faces_match_the_reference_semantics() {
        let mut block = EditorTerrainBlock2::create(BLOCK, 64, 32, 32, 1).unwrap();
        assert_eq!(block.number_texture_layers(), 0);
        assert_eq!(block.max_number_texture_layers(), usize::MAX);
        // insert answers the new index; remove erases but answers false.
        assert_eq!(block.insert_texture_layer(), 0);
        assert_eq!(block.insert_texture_layer(), 1);
        assert_eq!(block.number_texture_layers(), 2);
        assert!(!block.remove_texture_layer(0));
        assert_eq!(block.number_texture_layers(), 1);
        assert_eq!(block.texture_layer(0).blends().width(), 32);
        assert_eq!(block.forced_lod(), -1);
    }

    #[test]
    fn saved_block_reloads_with_matching_height_holes_and_materials() {
        let block = edited_block();

        let mut cdata = ZipFileSystem::open("", ZipFileSystem::empty_archive())
            .expect("an empty archive always parses");
        assert!(block.save_to_cdata(&mut cdata));
        let cdata_bytes = cdata.create_zip();

        let stored = ZipFileSystem::open("001x000x.cdata", cdata_bytes).unwrap();
        let mut reloaded =
            EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 3, &stored, "001x000x.cdata/terrain2")
                .expect("the edited block reloads");

        // Heights: every edited pole survives the quantised PNG round trip.
        // Funnel writes are image coords, heightAt the visible face.
        assert!(within_mm(reloaded.height_map().height_at(32, 32), 12.5));
        assert!(within_mm(reloaded.height_map().height_at(33, 32), -3.25));
        assert!(within_mm(reloaded.height_map().height_at(34, 34), 7.0));
        assert_eq!(reloaded.height_map().min_height(), -3.25);
        assert_eq!(reloaded.height_map().max_height(), 12.5);

        // Holes: both punched cells, and only those — image() behind a
        // lock, like the reference MF_ASSERT demands.
        assert!(reloaded.hole_map_mut().lock(false));
        {
            let holes = reloaded.hole_map();
            assert!(holes.image().get(5, 5));
            assert!(holes.image().get(6, 5));
            assert!(!holes.image().get(7, 5));
            assert_eq!(holes.holes_size(), 32);
        }
        reloaded.hole_map_mut().unlock();

        // Materials: layer order, names, bump, projections and weights.
        assert_eq!(reloaded.number_texture_layers(), 2);
        assert_eq!(
            reloaded.texture_layer(0).texture_name(),
            "environments/default/grass.texture"
        );
        assert_eq!(
            reloaded.texture_layer(1).texture_name(),
            "environments/default/rock.texture"
        );
        assert_eq!(
            reloaded.texture_layer(1).bump_texture_name(),
            "environments/default/rock_bm.texture"
        );
        assert_eq!(
            reloaded.texture_layer(1).u_projection(),
            [0.2, 0.0, 0.0, 0.0]
        );
        assert_eq!(reloaded.texture_layer(0).blends().get(3, 3), 255);
        assert_eq!(reloaded.texture_layer(1).blends().get(1, 2), 200);
        assert_eq!(reloaded.texture_layer(1).blends().get(31, 31), 55);

        // Dominant materials and the metadata.
        let dominant = reloaded.dominant_texture_map().expect("dominant map saved");
        assert_eq!(dominant.texture_names().len(), 2);
        assert_eq!(dominant.ground_type(8.0, 8.0), 1);
        assert_eq!(dominant.material_index(8.0, 8.0), 2);
        assert_eq!(reloaded.forced_lod(), 2);
    }

    #[test]
    fn vertex_lod_height_sections_downsample_the_base_map() {
        let block = edited_block();
        let sections = block.save().unwrap();
        let names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
        // heights, heights1, heights2 for numVertexLods 3.
        assert!(names.contains(&"heights"));
        assert!(names.contains(&"heights1"));
        assert!(names.contains(&"heights2"));
        assert_eq!(
            names.iter().filter(|n| **n == "layer").count(),
            2,
            "one section per texture layer"
        );

        // The lod sections sample the parent map: lod 1 pole (x, z)
        // answers base heightAt(x<<1, z<<1) (AliasedHeightMap), so lod 1
        // pole (17, 17) is the base pole edited at image (36, 36).
        let lod1 = sections
            .iter()
            .find(|(n, _)| n == "heights1")
            .map(|(_, b)| b)
            .unwrap();
        let mut map = TerrainHeightMap2::new(BLOCK, 1);
        map.load(lod1).unwrap();
        assert!(within_mm(map.height_at(17, 17), 7.0));
        // And lod 1 pole (16, 16) carries the image (34, 34) edit.
        assert!(within_mm(map.height_at(16, 16), 12.5));
    }

    #[test]
    fn hole_less_blocks_write_no_holes_section() {
        let mut block = EditorTerrainBlock2::create(BLOCK, 64, 32, 32, 1).unwrap();
        let idx = block.insert_texture_layer();
        block.texture_layer_mut(idx).set_texture_name("a.texture");
        let sections = block.save().unwrap();
        let names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
        assert!(!names.contains(&"holes"));
    }

    #[test]
    fn save_file_round_trips_through_the_multi_file_system() {
        let (_root, file_system) = fixture("savefile");
        let path = "space/001x000x.cdata";
        let block = edited_block();
        assert!(block.save_file(&file_system, path));

        // Re-save with a different forced lod: the terrain2 entry is
        // replaced, not duplicated.
        let raw = file_system.read_file(path).unwrap();
        let stored = ZipFileSystem::open(path, raw).unwrap();
        let mut second = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 3, &stored, path).unwrap();
        second.set_forced_lod(5);
        assert!(second.save_file(&file_system, path));

        let raw = file_system.read_file(path).unwrap();
        let stored = ZipFileSystem::open(path, raw).unwrap();
        let reloaded = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 3, &stored, path).unwrap();
        assert_eq!(reloaded.forced_lod(), 5);
        assert!(within_mm(reloaded.height_map().height_at(32, 32), 12.5));
        assert_eq!(
            reloaded.texture_layer(0).texture_name(),
            "environments/default/grass.texture"
        );
    }

    #[test]
    fn load_validations_report_the_reference_messages() {
        // A block with a layer whose blend size disagrees with the settings.
        let mut cdata = ZipFileSystem::open("", ZipFileSystem::empty_archive()).unwrap();
        let mut inner = ZipFileSystem::open("", ZipFileSystem::empty_archive()).unwrap();
        let mut heights = TerrainHeightMap2::new(BLOCK, 0);
        heights.create(64).unwrap();
        inner.write_to_structure("heights", heights.save());
        let mut layer = TextureLayer2::new(16, 16); // settings say 32
        layer.set_texture_name("a.texture");
        inner.write_to_structure("layer", layer.save());
        cdata.write_to_structure("terrain2", inner.create_zip());

        let err = EditorTerrainBlock2::load(
            BLOCK,
            64,
            32,
            32,
            1,
            &ZipFileSystem::open("", cdata.create_zip()).unwrap(),
            "001x000x.cdata/terrain2",
        )
        .err()
        .expect("the mismatched blend size fails the load");
        assert_eq!(
            err,
            EditorLoadError::InvalidTextureLayers("001x000x.cdata/terrain2".into())
        );
        assert_eq!(
            err.to_string(),
            "Invalid texture layers for terrain block 001x000x.cdata/terrain2\n"
        );

        // No layers at all trips the earlier check.
        let mut cdata2 = ZipFileSystem::open("", ZipFileSystem::empty_archive()).unwrap();
        let mut inner2 = ZipFileSystem::open("", ZipFileSystem::empty_archive()).unwrap();
        inner2.write_to_structure("heights", heights.save());
        cdata2.write_to_structure("terrain2", inner2.create_zip());
        let err = EditorTerrainBlock2::load(
            BLOCK,
            64,
            32,
            32,
            1,
            &ZipFileSystem::open("", cdata2.create_zip()).unwrap(),
            "001x000x.cdata/terrain2",
        )
        .err()
        .expect("the layer-less block fails the load");
        assert_eq!(
            err.to_string(),
            "No texture layers for terrain block 001x000x.cdata/terrain2\n"
        );
    }

    #[test]
    fn chunk_item_save_writes_the_cdata_reference() {
        let mut section = resources::datasection::DataSection::new("001x000x.chunk");
        assert!(write_terrain_section(&mut section, "001x000x"));
        assert_eq!(
            section.read_string("resource", ""),
            "001x000x.cdata/terrain2"
        );
        // The toss face carries the mapping path.
        assert_eq!(
            terrain_resource_id("spaces/main/", "001x000x"),
            "spaces/main/001x000x.cdata/terrain2"
        );
    }

    /// A [`TerrainBlockFile`] that records the filenames its save is
    /// called with, standing in for a real block.
    struct RecordingBlock {
        name: String,
        log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    }

    impl TerrainBlockFile for RecordingBlock {
        fn resource_name(&self) -> &str {
            &self.name
        }

        fn save(&self, filename: &str) -> bool {
            self.log.borrow_mut().push(filename.to_string());
            true
        }
    }

    #[test]
    fn unsaved_blocks_save_under_the_derived_cdata_names() {
        let log_a = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log_b = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

        let mut unsaved = UnsavedTerrainBlocks::default();
        assert!(unsaved.is_empty());
        unsaved.add(Box::new(RecordingBlock {
            name: "spaces/main/001x000x.cdata/terrain2".into(),
            log: log_a.clone(),
        }));
        unsaved.add(Box::new(RecordingBlock {
            name: "spaces/main/001x001x.cdata/terrain2".into(),
            log: log_b.clone(),
        }));
        assert!(!unsaved.is_empty());

        // save strips the terrain2 leaf: everything up to and including
        // the `.cdata/` tail.
        unsaved.save();
        assert_eq!(log_a.borrow().as_slice(), ["spaces/main/001x000x.cdata"]);
        assert_eq!(log_b.borrow().as_slice(), ["spaces/main/001x001x.cdata"]);

        // filter drops exactly the ones the other set saved; clear empties.
        let mut other = UnsavedTerrainBlocks::default();
        other
            .saved_blocks
            .insert("spaces/main/001x000x.cdata/terrain2".into(), ());
        unsaved.filter(&other);
        assert_eq!(unsaved.blocks.len(), 1);
        assert_eq!(unsaved.saved_blocks.len(), 2);
        unsaved.clear();
        assert!(unsaved.is_empty());
    }

    #[test]
    fn layer_error_arms_carry_the_reference_wording() {
        assert_eq!(
            LayerLoadError::NotBinary.to_string(),
            "terrain texture layer is not binary"
        );
        assert_eq!(
            LayerLoadError::WrongMagic.to_string(),
            "wrong blend header magic"
        );
        assert_eq!(
            LayerLoadError::InvalidBlendPng.to_string(),
            "TerrainTextureLayer2::load: PNG blend data is invalid\n"
        );
        assert_eq!(
            LayerLoadError::Truncated.to_string(),
            "terrain texture layer is truncated"
        );
        // A blob shorter than the 64-byte header reports truncated.
        assert!(matches!(
            TextureLayer2::load(&[0u8; 10]),
            Err(LayerLoadError::Truncated)
        ));
        // A PNG-version layer whose payload is not a PNG reports the
        // invalid-blend-PNG message.
        let mut layer = TextureLayer2::new(4, 4);
        layer.set_texture_name("a.texture");
        let mut saved = layer.save();
        let payload_at = BLEND_HEADER_SIZE + 4 + 9;
        saved[payload_at] ^= 0xff; // corrupt the blend PNG
        assert!(matches!(
            TextureLayer2::load(&saved),
            Err(LayerLoadError::InvalidBlendPng)
        ));
        // The RAW branch needs width*height blend bytes.
        let mut raw = Vec::new();
        raw.extend_from_slice(&BLEND_MAGIC.to_le_bytes());
        raw.extend_from_slice(&4u32.to_le_bytes());
        raw.extend_from_slice(&4u32.to_le_bytes());
        raw.extend_from_slice(&8u32.to_le_bytes());
        for _ in 0..8 {
            raw.extend_from_slice(&0.0f32.to_le_bytes());
        }
        raw.extend_from_slice(&BLEND_VERSION_RAW.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&[0u8; 8]);
        raw.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            TextureLayer2::load(&raw),
            Err(LayerLoadError::Truncated)
        ));
    }

    #[test]
    fn block_settings_and_layer_removal_answer_the_reference_values() {
        let mut block = EditorTerrainBlock2::create(BLOCK, 64, 32, 32, 3).unwrap();
        assert_eq!(block.block_size(), BLOCK);
        assert_eq!(block.height_map_editor_size(), 64);
        assert_eq!(block.hole_map_size(), 32);
        assert_eq!(block.blend_map_size(), 32);
        assert_eq!(block.num_vertex_lods(), 3);
        // Out-of-range removal is a no-op that still answers false.
        assert!(!block.remove_texture_layer(7));
        // The mutable dominant face exposes the carried map.
        assert!(block.dominant_texture_map().is_none());
        let mut dominant = DominantTextureMap2::new(BLOCK);
        dominant
            .load(Some(&dominant_blob(
                &["a.texture"],
                4,
                4,
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            )))
            .unwrap();
        block.dominant = Some(dominant);
        let names = block
            .dominant_texture_map_mut()
            .map(|map| map.texture_names().to_vec())
            .unwrap_or_default();
        assert_eq!(names, ["a.texture"]);
    }

    #[test]
    fn save_faces_refuse_a_block_whose_sections_fail() {
        let (_root, file_system) = fixture("refused");
        let path = "space/001x000x.cdata";
        let mut block = EditorTerrainBlock2::create(BLOCK, 64, 32, 32, 1).unwrap();
        let idx = block.insert_texture_layer();
        block.texture_layer_mut(idx).set_texture_name("a.texture");
        // An empty dominant map cannot serialise, and the failure climbs
        // save -> saveToCData -> save.
        block.dominant = Some(DominantTextureMap2::new(BLOCK));
        assert!(
            !block.save_to_cdata(
                &mut ZipFileSystem::open("", ZipFileSystem::empty_archive()).unwrap()
            )
        );
        assert!(!block.save_file(&file_system, path));
    }

    #[test]
    fn load_surfaces_reader_failures_verbatim() {
        let bare = || ZipFileSystem::open("", ZipFileSystem::empty_archive()).unwrap();
        let wrap = |mut inner: ZipFileSystem| {
            let mut outer = bare();
            outer.write_to_structure(TERRAIN2_SECTION_NAME, inner.create_zip());
            ZipFileSystem::open("", outer.create_zip()).unwrap()
        };

        // No terrain2 entry at all.
        let err = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 1, &bare(), "x")
            .err()
            .expect("no terrain2 entry fails");
        assert_eq!(err, EditorLoadError::Map("no terrain2 entry".into()));

        // A terrain2 entry that is not a zip.
        let mut outer = bare();
        outer.write_to_structure(TERRAIN2_SECTION_NAME, vec![1, 2, 3]);
        let outer = ZipFileSystem::open("", outer.create_zip()).unwrap();
        let err = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 1, &outer, "x")
            .err()
            .expect("a non-zip terrain2 fails");
        assert_eq!(
            err,
            EditorLoadError::Map("terrain2 entry is not a zip".into())
        );

        // Heights missing, then heights that do not parse.
        let mut heights = TerrainHeightMap2::new(BLOCK, 0);
        heights.create(64).unwrap();
        let good_heights = heights.save();
        let mut inner = bare();
        inner.write_to_structure("layer0", named_layer("a.texture"));
        let err = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 1, &wrap(inner), "x")
            .err()
            .expect("the load fails");
        assert_eq!(err, EditorLoadError::Map("no heights section".into()));

        let mut inner = bare();
        inner.write_to_structure(HEIGHTS_SECTION_NAME, vec![7u8; 40]);
        inner.write_to_structure("layer0", named_layer("a.texture"));
        let err = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 1, &wrap(inner), "x")
            .err()
            .expect("the load fails");
        // The garbage words fail the header magic first.
        assert_eq!(
            err,
            EditorLoadError::Map("dataSection is not for a TerrainHeightMap2".into())
        );

        // A holes section that parses as a zip block but truncates mid-map.
        let mut inner = bare();
        inner.write_to_structure(HEIGHTS_SECTION_NAME, good_heights.clone());
        inner.write_to_structure(HOLES_SECTION_NAME, zip_compress(b"xx"));
        inner.write_to_structure("layer0", named_layer("a.texture"));
        let err = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 1, &wrap(inner), "x")
            .err()
            .expect("the load fails");
        assert!(matches!(err, EditorLoadError::Map(_)));

        // A layer blob that is not a blend, a dominant blob that is not a
        // dominant map, and a metadata section that does not parse — the
        // first fails the load, the other two degrade exactly like the
        // reference.
        let mut inner = bare();
        inner.write_to_structure(HEIGHTS_SECTION_NAME, good_heights);
        inner.write_to_structure(LAYER_SECTION_NAME, vec![9u8; 12]);
        let err = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 1, &wrap(inner), "x")
            .err()
            .expect("the load fails");
        assert!(matches!(err, EditorLoadError::Map(_)));

        let mut inner = bare();
        inner.write_to_structure(HEIGHTS_SECTION_NAME, heights.save());
        inner.write_to_structure(LAYER_SECTION_NAME, named_layer("a.texture"));
        inner.write_to_structure(DOMINANT_TEXTURES_SECTION_NAME, vec![3u8; 20]);
        let err = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 1, &wrap(inner), "x")
            .err()
            .expect("the load fails");
        assert!(matches!(err, EditorLoadError::Map(_)));

        let mut inner = bare();
        inner.write_to_structure(HEIGHTS_SECTION_NAME, heights.save());
        inner.write_to_structure(LAYER_SECTION_NAME, named_layer("a.texture"));
        inner.write_to_structure(TERRAIN_BLOCK_META_DATA_SECTION_NAME, vec![5u8; 30]);
        let block = EditorTerrainBlock2::load(BLOCK, 64, 32, 32, 1, &wrap(inner), "x")
            .expect("unparsable metadata degrades to the default");
        assert_eq!(block.forced_lod(), -1);
    }

    #[test]
    fn unsaved_save_name_keeps_the_six_bytes_after_the_last_dot() {
        assert_eq!(
            unsaved_block_save_name("spaces/main/001x000x.cdata/terrain2"),
            "spaces/main/001x000x.cdata"
        );
        assert_eq!(unsaved_block_save_name("a.cdata/terrain2"), "a.cdata");
        // No dot: the reference indexes past npos; the port keeps the name.
        assert_eq!(unsaved_block_save_name("plain"), "plain");
    }
}
