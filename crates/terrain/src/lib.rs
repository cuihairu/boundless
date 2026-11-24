//! BW-10-03 the Terrain2 readers (`lib/terrain/terrain2/`).
//!
//! - the height map: `HeightMapHeader` container, the three load branches of
//!   `TerrainHeightMap2::loadHeightMap`, and the grid/interpolated height
//!   queries (`height_map`),
//! - the PNG payload decoder for the two image shapes the terrain faces
//!   produce (`png`).
//!
//! Reference quirks are ported on purpose and cited at each site.

pub mod height_map;
pub mod png;

pub use height_map::{
    DEFAULT_UNIT_SCALE, DEFAULT_VISIBLE_OFFSET, HEIGHT_MAP_COMPRESS_RAW, HEIGHT_MAP_HEADER_SIZE,
    HEIGHT_MAP_MAGIC, HEIGHT_MAP_VERSION_ABS_FLOAT, HEIGHT_MAP_VERSION_ABS_QFLOAT,
    HEIGHT_MAP_VERSION_REL_UINT16, HEIGHT_MAP_VERSION_REL_UINT16_PNG, HeightMapHeader, LoadError,
    QUANTISATION_LEVEL, TerrainHeightMap2, quantise, unquantise,
};
pub use png::{PngError, PngImage, decompress_png};
