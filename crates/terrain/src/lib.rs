//! BW-10-03 the Terrain2 readers (`lib/terrain/terrain2/`).
//!
//! - the height map: `HeightMapHeader` container, the three load branches of
//!   `TerrainHeightMap2::loadHeightMap`, and the grid/interpolated height
//!   queries (`height_map`),
//! - the hole map: `HolesHeader`, the packed `Moo::Image<bool>` face, and
//!   the `holeAt` queries (`hole_map`),
//! - the dominant texture map: `DominantTextureMapHeader`, the `BinaryBlock`
//!   zip sub-block, and the material/ground-type point queries
//!   (`dominant_texture_map`),
//! - the PNG payload decoder for the two image shapes the terrain faces
//!   produce (`png`),
//! - the editor faces: texture-layer blend save/load, the block that
//!   composes the maps and saves them into the chunk `.cdata`, the
//!   unsaved-block bookkeeping and the chunk item resource reference
//!   (`editor_block`).
//!
//! Reference quirks are ported on purpose and cited at each site.

pub mod dominant_texture_map;
pub mod editor_block;
pub mod height_map;
pub mod hole_map;
pub mod png;

pub use dominant_texture_map::{
    DOMINANT_HEADER_SIZE, DOMINANT_MAGIC, DOMINANT_VERSION_ZIP, DominantLoadError,
    DominantSaveError, DominantTextureMap2, DominantTextureMapHeader, U8Image, ZIP_HEADER_SIZE,
    ZIP_MAGIC1, ZIP_MAGIC2, is_zip_block, zip_compress, zip_decompress,
};
pub use editor_block::{
    BLEND_HEADER_SIZE, BLEND_MAGIC, BLEND_VERSION_PNG, BLEND_VERSION_RAW,
    DOMINANT_TEXTURES_SECTION_NAME, EditorLoadError, EditorTerrainBlock2,
    FORCED_LOD_ATTRIBUTE_NAME, HEIGHTS_SECTION_NAME, HOLES_SECTION_NAME, LAYER_SECTION_NAME,
    LayerLoadError, TERRAIN_BLOCK_META_DATA_SECTION_NAME, TERRAIN2_SECTION_NAME, TerrainBlockFile,
    TextureLayer2, UnsavedTerrainBlocks, default_uv_projections, terrain_resource_id,
    unsaved_block_save_name, write_terrain_section,
};
pub use height_map::{
    DEFAULT_UNIT_SCALE, DEFAULT_VISIBLE_OFFSET, HEIGHT_MAP_COMPRESS_RAW, HEIGHT_MAP_HEADER_SIZE,
    HEIGHT_MAP_MAGIC, HEIGHT_MAP_VERSION_ABS_FLOAT, HEIGHT_MAP_VERSION_ABS_QFLOAT,
    HEIGHT_MAP_VERSION_REL_UINT16, HEIGHT_MAP_VERSION_REL_UINT16_PNG, HeightCreateError,
    HeightMapHeader, LoadError, QUANTISATION_LEVEL, TerrainHeightMap2, quantise, unquantise,
};
pub use hole_map::{
    BoolImage, HOLES_HEADER_SIZE, HOLES_MAGIC, HoleLoadError, HolesHeader, TerrainHoleMap2,
};
pub use png::{PngError, PngImage, compress_png, decompress_png};
