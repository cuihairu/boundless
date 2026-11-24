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
//!   produce (`png`).
//!
//! Reference quirks are ported on purpose and cited at each site.

pub mod dominant_texture_map;
pub mod height_map;
pub mod hole_map;
pub mod png;

pub use dominant_texture_map::{
    DOMINANT_HEADER_SIZE, DOMINANT_MAGIC, DOMINANT_VERSION_ZIP, DominantLoadError,
    DominantTextureMap2, DominantTextureMapHeader, U8Image, ZIP_HEADER_SIZE, ZIP_MAGIC1,
    ZIP_MAGIC2, is_zip_block, zip_decompress,
};
pub use height_map::{
    DEFAULT_UNIT_SCALE, DEFAULT_VISIBLE_OFFSET, HEIGHT_MAP_COMPRESS_RAW, HEIGHT_MAP_HEADER_SIZE,
    HEIGHT_MAP_MAGIC, HEIGHT_MAP_VERSION_ABS_FLOAT, HEIGHT_MAP_VERSION_ABS_QFLOAT,
    HEIGHT_MAP_VERSION_REL_UINT16, HEIGHT_MAP_VERSION_REL_UINT16_PNG, HeightMapHeader, LoadError,
    QUANTISATION_LEVEL, TerrainHeightMap2, quantise, unquantise,
};
pub use hole_map::{
    BoolImage, HOLES_HEADER_SIZE, HOLES_MAGIC, HoleLoadError, HolesHeader, TerrainHoleMap2,
};
pub use png::{PngError, PngImage, decompress_png};
