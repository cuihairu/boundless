//! BW-10-03-01 the Terrain2 height map (`lib/terrain/terrain2/terrain_height_map2.*`).
//!
//! Reader face of `TerrainHeightMap2`: the 32-byte `HeightMapHeader`
//! (`lib/terrain/terrain_data.hpp:22-42`), the three load branches of
//! `loadHeightMap` (`terrain_height_map2.cpp:377-532`), and the query face —
//! grid `heightAt(int,int)` plus the parity-diagonal interpolated
//! `heightAt(float,float)` (`terrain_height_map2.cpp:688-763`) that fits the
//! lod0 mesh.
//!
//! Reference quirks are ported on purpose and cited at each site: the
//! relative-uint16 branch reinterprets the header's stored float min/max as
//! raw `int32` bit patterns for the offset arithmetic, and the quantised
//! branch asserts the decoded image size equals the header size.

use crate::png;

/// `HeightMapHeader::MAGIC` — "hmp\0" (`terrain_data.hpp:40-41`).
pub const HEIGHT_MAP_MAGIC: u32 = 0x00706d68;
/// 32 bytes: magic/width/height/compression/version/minHeight/maxHeight/pad.
pub const HEIGHT_MAP_HEADER_SIZE: usize = 32;

pub const HEIGHT_MAP_VERSION_ABS_FLOAT: u32 = 1;
pub const HEIGHT_MAP_VERSION_REL_UINT16: u32 = 2;
pub const HEIGHT_MAP_VERSION_REL_UINT16_PNG: u32 = 3;
pub const HEIGHT_MAP_VERSION_ABS_QFLOAT: u32 = 4;

/// `HeightMapCompression::COMPRESS_RAW` (`terrain_data.hpp:10-14`).
pub const HEIGHT_MAP_COMPRESS_RAW: u32 = 0;

/// `Terrain::DEFAULT_UNIT_SCALE` (`terrain_height_map.hpp:16`): relative
/// heights are centimetres.
pub const DEFAULT_UNIT_SCALE: f32 = 0.01;

/// `TerrainHeightMap2::DEFAULT_VISIBLE_OFFSET` (`terrain_height_map2.hpp:48`):
/// the non-visible border around each side of the height map.
pub const DEFAULT_VISIBLE_OFFSET: u32 = 2;

/// `QUANTISATION_LEVEL` (`height_map_compress.cpp:24`): heights quantise to
/// the millimetre grid.
pub const QUANTISATION_LEVEL: f32 = 0.001;

/// `height_map_compress.cpp:16` — 'qpng'.
const QUANTISED_PNG_VERSION: u32 = 0x71706e67;

/// `HeightMapHeader` (`terrain_data.hpp:22-42`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeightMapHeader {
    pub magic: u32,
    pub width: u32,
    pub height: u32,
    pub compression: u32,
    pub version: u32,
    pub min_height: f32,
    pub max_height: f32,
    pub pad: u32,
}

impl HeightMapHeader {
    /// Parse the fixed 32-byte header and return it with the payload slice.
    pub fn parse(data: &[u8]) -> Result<(Self, &[u8]), LoadError> {
        if data.len() < HEIGHT_MAP_HEADER_SIZE {
            return Err(LoadError::Truncated);
        }
        let u32_at = |off: usize| {
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };
        let header = Self {
            magic: u32_at(0),
            width: u32_at(4),
            height: u32_at(8),
            compression: u32_at(12),
            version: u32_at(16),
            min_height: f32::from_le_bytes([data[20], data[21], data[22], data[23]]),
            max_height: f32::from_le_bytes([data[24], data[25], data[26], data[27]]),
            pad: u32_at(28),
        };
        Ok((header, &data[HEIGHT_MAP_HEADER_SIZE..]))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    /// `header->magic_ != HeightMapHeader::MAGIC` — the reference message is
    /// "dataSection is not for a TerrainHeightMap2".
    NotHeightMap,
    /// An unknown `version_`: "Invalid TerrainHeightMap2 format %u.
    /// You must re-export from World Editor."
    InvalidFormat(u32),
    /// "The height map data could not be decompressed."
    DecompressFailed,
    /// The blob ends before a fixed-size read completes. The reference would
    /// read out of bounds here; the port reports it instead (registered
    /// divergence, as with the compiled-space section reader).
    Truncated,
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoadError::NotHeightMap => write!(f, "dataSection is not for a TerrainHeightMap2"),
            LoadError::InvalidFormat(v) => write!(
                f,
                "Invalid TerrainHeightMap2 format {v}.You must re-export from World Editor."
            ),
            LoadError::DecompressFailed => {
                write!(f, "The height map data could not be decompressed.")
            }
            LoadError::Truncated => write!(f, "height map blob is truncated"),
        }
    }
}

/// `quantise` (`height_map_compress.cpp:32-35`): millimetre grid, rounding
/// away from zero on the halves (the C cast truncates toward zero).
// C's (int32) cast truncates toward zero, which `as` mirrors; the reference
// cast is UB for out-of-range values and `as` saturates instead (registered
// divergence on an unreachable-in-practice branch).
#[allow(clippy::cast_possible_truncation)]
pub fn quantise(h: f32) -> i32 {
    let scaled = h / QUANTISATION_LEVEL + if h > 0.0 { 0.5 } else { -0.5 };
    scaled as i32
}

/// `unquantise` (`height_map_compress.cpp:44-47`).
pub fn unquantise(q: i32) -> f32 {
    q as f32 * QUANTISATION_LEVEL
}

/// Reader face of `TerrainHeightMap2`.
#[derive(Debug, Clone)]
pub struct TerrainHeightMap2 {
    block_size: f32,
    lod_level: u32,
    visible_offset: u32,
    heights: Vec<f32>,
    poles_width: u32,
    poles_height: u32,
    min_height: f32,
    max_height: f32,
    internal_blocks_width: u32,
    internal_blocks_height: u32,
    spacing_x: f32,
    spacing_z: f32,
}

impl TerrainHeightMap2 {
    /// `TerrainHeightMap2(blockSize, size=0, lodLevel=0, ...)`
    /// (`terrain_height_map2.cpp:63-92`): the visible offset collapses to
    /// zero for every lod above the base level.
    pub fn new(block_size: f32, lod_level: u32) -> Self {
        // `TerrainMap` ctor asserts `!almostZero(blockSize_)`
        // (`terrain_map.hpp:30-33`, epsilon 0.0004 in `stdmf.hpp:616`).
        assert!(
            block_size.abs() >= 0.0004,
            "reference asserts blockSize is not almost zero"
        );
        Self {
            block_size,
            lod_level,
            visible_offset: if lod_level > 0 {
                0
            } else {
                DEFAULT_VISIBLE_OFFSET
            },
            heights: Vec::new(),
            poles_width: 0,
            poles_height: 0,
            min_height: 0.0,
            max_height: 0.0,
            internal_blocks_width: 0,
            internal_blocks_height: 0,
            spacing_x: 0.0,
            spacing_z: 0.0,
        }
    }

    pub fn lod_level(&self) -> u32 {
        self.lod_level
    }

    pub fn block_size(&self) -> f32 {
        self.block_size
    }

    /// `width()` returns the full image width including the invisible border
    /// (`terrain_height_map2.cpp:163-166`) — same as `poles_width`.
    pub fn width(&self) -> u32 {
        self.poles_width
    }

    pub fn height(&self) -> u32 {
        self.poles_height
    }

    pub fn poles_width(&self) -> u32 {
        self.poles_width
    }

    pub fn poles_height(&self) -> u32 {
        self.poles_height
    }

    pub fn vertices_width(&self) -> u32 {
        self.poles_width - self.internal_visible_offset() * 2
    }

    pub fn vertices_height(&self) -> u32 {
        self.poles_height - self.internal_visible_offset() * 2
    }

    pub fn blocks_width(&self) -> u32 {
        self.internal_blocks_width
    }

    pub fn blocks_height(&self) -> u32 {
        self.internal_blocks_height
    }

    pub fn internal_visible_offset(&self) -> u32 {
        self.visible_offset
    }

    pub fn x_visible_offset(&self) -> u32 {
        self.visible_offset
    }

    pub fn z_visible_offset(&self) -> u32 {
        self.visible_offset
    }

    pub fn spacing_x(&self) -> f32 {
        self.spacing_x
    }

    pub fn spacing_z(&self) -> f32 {
        self.spacing_z
    }

    pub fn min_height(&self) -> f32 {
        self.min_height
    }

    pub fn max_height(&self) -> f32 {
        self.max_height
    }

    /// `TerrainHeightMap2::load` (`terrain_height_map2.cpp:356-375`).
    ///
    /// Boundary: the reference first asks the section for binary data and
    /// un-zips `BinaryBlock` compression if present (`pBin->isCompressed()`).
    /// The port takes the plain bytes — section lookup and the zip face
    /// belong to the resource layer, which hands over decompressed blocks.
    pub fn load(&mut self, data: &[u8]) -> Result<(), LoadError> {
        self.load_height_map(data)
    }

    /// `TerrainHeightMap2::loadHeightMap` (`terrain_height_map2.cpp:377-532`).
    fn load_height_map(&mut self, data: &[u8]) -> Result<(), LoadError> {
        let (header, payload) = HeightMapHeader::parse(data)?;
        if header.magic != HEIGHT_MAP_MAGIC {
            return Err(LoadError::NotHeightMap);
        }

        match header.version {
            HEIGHT_MAP_VERSION_REL_UINT16_PNG => {
                // Quirk: the header stores min/max as floats, but this branch
                // reinterprets the raw bits as int32 for the offset
                // arithmetic (`*((int32 *)&header->maxHeight_)`,
                // terrain_height_map2.cpp:395-396, 424).
                let min_bits = header.min_height.to_bits() as i32;
                let max_bits = header.max_height.to_bits() as i32;
                self.max_height = convert_height_2_float(max_bits);
                self.min_height = convert_height_2_float(min_bits);
                if self.max_height < self.min_height {
                    core::mem::swap(&mut self.min_height, &mut self.max_height);
                }

                let image =
                    png::decompress_png(payload).map_err(|_| LoadError::DecompressFailed)?;
                // The reference iterates header dims, not the image's.
                let count = header.width as usize * header.height as usize;
                if image.width != header.width
                    || image.height != header.height
                    || image.data.len() / 2 < count
                {
                    return Err(LoadError::DecompressFailed);
                }
                self.poles_width = header.width;
                self.poles_height = header.height;
                self.heights = (0..count)
                    .map(|i| {
                        let rel =
                            image.sample16((i as u32) % header.width, (i as u32) / header.width);
                        // `*pRelHeights++ + *((int32 *)&header->minHeight_)`:
                        // the offset adds in int32 space, then converts.
                        convert_height_2_float(rel as i32 + min_bits)
                    })
                    .collect();
            }
            HEIGHT_MAP_VERSION_ABS_FLOAT => {
                self.max_height = header.max_height;
                self.min_height = header.min_height;
                if self.max_height < self.min_height {
                    core::mem::swap(&mut self.min_height, &mut self.max_height);
                }

                let count = header.width as usize * header.height as usize;
                if payload.len() < count * 4 {
                    return Err(LoadError::Truncated);
                }
                self.poles_width = header.width;
                self.poles_height = header.height;
                self.heights = (0..count)
                    .map(|i| {
                        let off = i * 4;
                        f32::from_le_bytes([
                            payload[off],
                            payload[off + 1],
                            payload[off + 2],
                            payload[off + 3],
                        ])
                    })
                    .collect();
            }
            HEIGHT_MAP_VERSION_ABS_QFLOAT => {
                self.max_height = header.max_height;
                self.min_height = header.min_height;
                if self.max_height < self.min_height {
                    core::mem::swap(&mut self.min_height, &mut self.max_height);
                }

                // `decompressHeightMap` = quantised PNG (`height_map_compress.cpp:192-198`):
                // 'qpng' magic, then a 32bpp PNG of int32 millimetre heights.
                if payload.len() <= 4
                    || u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
                        != QUANTISED_PNG_VERSION
                {
                    return Err(LoadError::DecompressFailed);
                }
                let image =
                    png::decompress_png(&payload[4..]).map_err(|_| LoadError::DecompressFailed)?;
                // `MF_ASSERT(heights_.width() == header->width_)` etc.
                // (terrain_height_map2.cpp:454-455).
                assert!(
                    image.width == header.width && image.height == header.height,
                    "reference asserts the decoded height map size"
                );
                self.poles_width = header.width;
                self.poles_height = header.height;
                self.heights = (0..image.width as usize * image.height as usize)
                    .map(|i| {
                        let x = (i as u32) % image.width;
                        let y = (i as u32) / image.width;
                        unquantise(image.word(x, y))
                    })
                    .collect();
            }
            other => return Err(LoadError::InvalidFormat(other)),
        }

        self.refresh_internal_dimensions();
        Ok(())
    }

    /// `refreshInternalDimensions` (`terrain_height_map2.hpp:267-277`).
    fn refresh_internal_dimensions(&mut self) {
        let offset = self.internal_visible_offset();
        self.internal_blocks_width = self.poles_width - (offset * 2) - 1;
        self.internal_blocks_height = self.poles_height - (offset * 2) - 1;
        self.spacing_x = self.block_size / self.internal_blocks_width as f32;
        self.spacing_z = self.block_size / self.internal_blocks_height as f32;
    }

    /// `Moo::Image::get` (`image.ipp:347-354`): boundary clamping.
    fn get(&self, x: i32, y: i32) -> f32 {
        let x = x.clamp(0, self.poles_width as i32 - 1);
        let y = y.clamp(0, self.poles_height as i32 - 1);
        self.heights[(y as u32 * self.poles_width + x as u32) as usize]
    }

    /// `internalHeightAt` (`terrain_height_map2.hpp:299-302`): grid position
    /// plus the visible offset, through the clamped get.
    pub fn height_at(&self, x: i32, z: i32) -> f32 {
        let offset = self.internal_visible_offset() as i32;
        self.get(x + offset, z + offset)
    }

    /// `TerrainHeightMap2::heightAt(float, float)`
    /// (`terrain_height_map2.cpp:702-763`): block-local metres to grid
    /// space, then triangle interpolation over the cell diagonal that
    /// alternates with the parity of the cell — matching the lod0 mesh.
    pub fn height_at_interpolated(&self, x: f32, z: f32) -> f32 {
        let offset = self.internal_visible_offset() as f32;
        let xs = x / self.spacing_x + offset;
        let zs = z / self.spacing_z + offset;

        let xf = xs - xs.floor();
        let zf = zs - zs.floor();

        // `int32(floorf(xs))` — C truncation, mirrored exactly.
        #[allow(clippy::cast_possible_truncation)]
        let x_off = xs.floor() as i32;
        #[allow(clippy::cast_possible_truncation)]
        let z_off = zs.floor() as i32;

        // Neighbouring cells have opposing diagonals: `(xOff ^ zOff) & 1`
        // on the signed ints, exactly like the reference.
        if (x_off ^ z_off) & 1 != 0 {
            // The cell diagonal runs bottom-left to top-right.
            let h01 = self.get(x_off, z_off + 1);
            let h10 = self.get(x_off + 1, z_off);
            if (1.0 - xf) > zf {
                let h00 = self.get(x_off, z_off);
                h00 + (h10 - h00) * xf + (h01 - h00) * zf
            } else {
                let h11 = self.get(x_off + 1, z_off + 1);
                h11 + (h01 - h11) * (1.0 - xf) + (h10 - h11) * (1.0 - zf)
            }
        } else {
            let h00 = self.get(x_off, z_off);
            let h11 = self.get(x_off + 1, z_off + 1);
            if xf > zf {
                let h10 = self.get(x_off + 1, z_off);
                h10 + (h00 - h10) * (1.0 - xf) + (h11 - h10) * zf
            } else {
                let h01 = self.get(x_off, z_off + 1);
                h01 + (h11 - h01) * xf + (h00 - h01) * (1.0 - zf)
            }
        }
    }
}

/// `TerrainHeightMap::convertHeight2Float` (`terrain_height_map.hpp:215-218`):
/// relative centimetres to metres.
fn convert_height_2_float(height: i32) -> f32 {
    height as f32 * DEFAULT_UNIT_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(version: u32) -> HeightMapHeader {
        HeightMapHeader {
            magic: HEIGHT_MAP_MAGIC,
            width: 3,
            height: 2,
            compression: HEIGHT_MAP_COMPRESS_RAW,
            version,
            min_height: 0.0,
            max_height: 0.0,
            pad: 0,
        }
    }

    fn assemble(header: &HeightMapHeader, payload: &[u8]) -> Vec<u8> {
        let mut blob = Vec::new();
        blob.extend_from_slice(&header.magic.to_le_bytes());
        blob.extend_from_slice(&header.width.to_le_bytes());
        blob.extend_from_slice(&header.height.to_le_bytes());
        blob.extend_from_slice(&header.compression.to_le_bytes());
        blob.extend_from_slice(&header.version.to_le_bytes());
        blob.extend_from_slice(&header.min_height.to_le_bytes());
        blob.extend_from_slice(&header.max_height.to_le_bytes());
        blob.extend_from_slice(&header.pad.to_le_bytes());
        blob.extend_from_slice(payload);
        blob
    }

    #[test]
    fn rejects_foreign_magic() {
        let mut h = header(HEIGHT_MAP_VERSION_ABS_FLOAT);
        h.magic = 0x00706d69; // "imp\0"
        let mut blob = assemble(&h, &[0u8; 24]);
        blob.extend_from_slice(&[0u8; 24]); // 3x2 floats
        let mut map = TerrainHeightMap2::new(100.0, 0);
        assert_eq!(map.load(&blob), Err(LoadError::NotHeightMap));
    }

    #[test]
    fn rejects_unknown_versions_with_the_reference_message() {
        let h = header(99);
        let blob = assemble(&h, &[]);
        let mut map = TerrainHeightMap2::new(100.0, 0);
        assert_eq!(map.load(&blob), Err(LoadError::InvalidFormat(99)));
        assert_eq!(
            map.load(&blob).unwrap_err().to_string(),
            "Invalid TerrainHeightMap2 format 99.You must re-export from World Editor."
        );
    }

    #[test]
    fn absolute_float_loads_verbatim_and_swaps_reversed_min_max() {
        let mut h = header(HEIGHT_MAP_VERSION_ABS_FLOAT);
        h.min_height = 12.0;
        h.max_height = 3.0;
        // Row-major 3x2 heights. Built at lod 1 so the collapsed visible
        // offset keeps the tiny 3x2 grid valid (the border needs
        // poles >= 2*offset + 1 vertices, see refresh_internal_dimensions).
        let heights = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut payload = Vec::new();
        for v in heights {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        let blob = assemble(&h, &payload);
        let mut map = TerrainHeightMap2::new(100.0, 1);
        map.load(&blob).expect("load");
        // Reversed header bounds are swapped (cpp:432-433).
        assert_eq!(map.min_height(), 3.0);
        assert_eq!(map.max_height(), 12.0);
        assert_eq!(map.poles_width(), 3);
        assert_eq!(map.internal_visible_offset(), 0);
        assert_eq!(map.blocks_width(), 2);
        assert_eq!(map.height_at(0, 0), 1.0);
        assert_eq!(map.height_at(2, 1), 6.0);
        // Grid reads clamp like Moo::Image::get.
        assert_eq!(map.height_at(-5, 0), 1.0);
        assert_eq!(map.height_at(9, 9), 6.0);
        // The remaining accessors and error displays.
        assert_eq!(map.lod_level(), 1);
        assert_eq!(map.block_size(), 100.0);
        assert_eq!(map.width(), 3);
        assert_eq!(map.height(), 2);
        assert_eq!(map.poles_height(), 2);
        assert_eq!(map.blocks_height(), 1);
        assert_eq!(
            LoadError::NotHeightMap.to_string(),
            "dataSection is not for a TerrainHeightMap2"
        );
        assert_eq!(
            LoadError::DecompressFailed.to_string(),
            "The height map data could not be decompressed."
        );
        assert_eq!(
            LoadError::Truncated.to_string(),
            "height map blob is truncated"
        );
    }

    #[test]
    fn absolute_float_truncated_payload_reports_truncation() {
        let h = header(HEIGHT_MAP_VERSION_ABS_FLOAT);
        // Three of six floats.
        let mut payload = Vec::new();
        for v in [1.0f32, 2.0, 3.0] {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        let blob = assemble(&h, &payload);
        let mut map = TerrainHeightMap2::new(100.0, 1);
        assert_eq!(map.load(&blob), Err(LoadError::Truncated));
    }

    #[test]
    fn quantised_fixture_with_broken_payload_fails_to_decompress() {
        let h = header(HEIGHT_MAP_VERSION_ABS_QFLOAT);
        // Wrong payload magic: not 'qpng'.
        let blob = assemble(&h, &[1, 2, 3, 4, 5]);
        let mut map = TerrainHeightMap2::new(100.0, 1);
        assert_eq!(map.load(&blob), Err(LoadError::DecompressFailed));
    }

    #[test]
    fn relative_uint16_with_a_corrupt_png_fails_to_decompress() {
        let mut h = header(HEIGHT_MAP_VERSION_REL_UINT16_PNG);
        h.min_height = 12.0;
        h.max_height = 13.0;
        // A payload that is not a PNG at all.
        let blob = assemble(&h, b"plainly not png");
        let mut map = TerrainHeightMap2::new(100.0, 1);
        assert_eq!(map.load(&blob), Err(LoadError::DecompressFailed));
    }

    #[test]
    fn interpolated_height_walks_all_four_triangle_arms() {
        // 3x3 grid at lod 1 (no border): spacing = 100 / 2 = 50.
        let mut h = header(HEIGHT_MAP_VERSION_ABS_FLOAT);
        h.width = 3;
        h.height = 3;
        let heights = [
            0.0f32, 10.0, 20.0, // z = 0
            100.0, 110.0, 120.0, // z = 1
            200.0, 210.0, 220.0, // z = 2
        ];
        let mut payload = Vec::new();
        for v in heights {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        let blob = assemble(&h, &payload);
        let mut map = TerrainHeightMap2::new(100.0, 1);
        map.load(&blob).expect("load");

        // Even-parity cells ((xOff ^ zOff) & 1 == 0): diagonal
        // (0,0)-(1,1).
        // xf == zf picks the h01 arm (res = h01 + (h11-h01)*xf +
        // (h00-h01)*(1-zf)).
        assert!((map.height_at_interpolated(25.0, 25.0) - 55.0).abs() < 1e-4);
        // xf > zf picks the h10 arm.
        assert!((map.height_at_interpolated(30.0, 25.0) - 56.0).abs() < 1e-4);

        // Odd-parity cells: diagonal (0,1)-(1,0).
        // (1.0 - xf) > zf picks the h00 arm.
        assert!((map.height_at_interpolated(75.0, 24.0) - 63.0).abs() < 1e-4);
        // The equality tie picks the h11 arm.
        assert!((map.height_at_interpolated(75.0, 25.0) - 65.0).abs() < 1e-4);

        // Negative metres floor into negative cells, clamping into the
        // corner: every sample around (-0.5,-0.5) cells reads the (0,0)
        // height.
        assert!(map.height_at_interpolated(-25.0, -25.0).abs() < 1e-5);
    }

    #[test]
    fn visible_offset_collapses_for_lods_above_the_base() {
        let map = TerrainHeightMap2::new(100.0, 3);
        assert_eq!(map.internal_visible_offset(), 0);
        assert_eq!(map.x_visible_offset(), 0);
        let base = TerrainHeightMap2::new(100.0, 0);
        assert_eq!(base.internal_visible_offset(), DEFAULT_VISIBLE_OFFSET);
    }

    #[test]
    fn constructor_asserts_an_almost_zero_block_size() {
        let result = std::panic::catch_unwind(|| TerrainHeightMap2::new(0.0001, 0));
        assert!(result.is_err(), "reference MF_ASSERTs !almostZero");
    }

    #[test]
    fn quantise_rounds_away_from_zero_on_the_millimetre_grid() {
        assert_eq!(quantise(1.5), 1500);
        assert_eq!(quantise(0.0015), 2); // 1.5mm + 0.5 -> 2
        assert_eq!(quantise(-0.0015), -2); // -1.5mm - 0.5 -> -2
        assert_eq!(quantise(0.001), 1); // 1.0mm + 0.5 truncates to 1
        assert_eq!(quantise(-1.5), -1500);
        // f32 rounding: 1500 * 0.001f lands one ulp above 1.5, exactly like
        // the reference float math.
        assert!((unquantise(1500) - 1.5).abs() < 1e-5);
        assert_eq!(unquantise(-1), -0.001);
    }

    #[test]
    fn relative_uint16_adds_the_offset_in_int32_bit_space() {
        // Header min/max are floats, but the branch reads their raw bits as
        // the int32 offset (cpp:395-396, 424). 12.0f has bits 0x41400000 =
        // 1095761920, so relative heights 200 and 300 convert as
        // (rel + 1095761920) * 0.01.
        let mut h = header(HEIGHT_MAP_VERSION_REL_UINT16_PNG);
        h.min_height = 12.0;
        h.max_height = 13.0;
        let min_bits = 12.0f32.to_bits() as i32;

        // A 3x2 gray16 PNG (filter-0 rows, wire big-endian) — the payload
        // the v3 branch decodes. 'qpng' header is not part of the v3
        // branch. Built at lod 1: the tiny 3x2 grid cannot carry the lod-0
        // invisible border.
        let blob = assemble(&h, &gray16_png(3, 2, &[200, 300, 400, 500, 600, 700]));
        let mut map = TerrainHeightMap2::new(100.0, 1);
        map.load(&blob).expect("load");
        let expect = |rel: u16| (rel as i32 + min_bits) as f32 * DEFAULT_UNIT_SCALE;
        assert_eq!(map.height_at(0, 0), expect(200));
        assert_eq!(map.height_at(1, 0), expect(300));
        assert_eq!(map.height_at(2, 1), expect(700));
        // min/max came back through convertHeight2Float on the bits.
        assert_eq!(map.min_height(), min_bits as f32 * DEFAULT_UNIT_SCALE);
    }

    #[test]
    fn relative_uint16_swaps_reversed_bounds_and_rejects_mismatched_dims() {
        // Reversed header bounds swap after the bit-reinterpret conversion,
        // like the other branches (cpp:409-411).
        let mut h = header(HEIGHT_MAP_VERSION_REL_UINT16_PNG);
        h.min_height = 13.0;
        h.max_height = 12.0;
        let blob = assemble(&h, &gray16_png(3, 2, &[200, 300, 400, 500, 600, 700]));
        let mut map = TerrainHeightMap2::new(100.0, 1);
        map.load(&blob).expect("load");
        assert_eq!(
            map.min_height(),
            12.0f32.to_bits() as i32 as f32 * DEFAULT_UNIT_SCALE
        );
        assert_eq!(
            map.max_height(),
            13.0f32.to_bits() as i32 as f32 * DEFAULT_UNIT_SCALE
        );
        assert!(map.min_height() < map.max_height());

        // A decoded image whose dims disagree with the header: the load
        // fails before any height is read (cpp:414-419).
        let mut h2 = header(HEIGHT_MAP_VERSION_REL_UINT16_PNG);
        h2.width = 4; // the PNG is only 3 wide
        let blob2 = assemble(&h2, &gray16_png(3, 2, &[200, 300, 400, 500, 600, 700]));
        let mut map2 = TerrainHeightMap2::new(100.0, 1);
        assert_eq!(map2.load(&blob2), Err(LoadError::DecompressFailed));
    }

    #[test]
    fn quantised_fixture_swaps_reversed_header_bounds() {
        // v4 with reversed bounds: a tiny 'qpng' payload of int32
        // millimetre words under a flipped min/max header.
        let mut h = header(HEIGHT_MAP_VERSION_ABS_QFLOAT);
        h.width = 2;
        h.height = 2;
        h.min_height = 5.0;
        h.max_height = 1.0;
        let mut payload = Vec::new();
        // The tag is a uint32 written little-endian by the reference, so
        // the file bytes read "gnpq" (the fixture shows exactly that).
        payload.extend_from_slice(&QUANTISED_PNG_VERSION.to_le_bytes());
        payload.extend_from_slice(&rgba_png(2, 2, &[1000, 2000, 3000, 4000]));
        let blob = assemble(&h, &payload);
        let mut map = TerrainHeightMap2::new(100.0, 1);
        map.load(&blob).expect("load");
        assert_eq!(map.min_height(), 1.0);
        assert_eq!(map.max_height(), 5.0);
        assert_eq!(map.height_at(0, 0), unquantise(1000));
        assert_eq!(map.height_at(1, 1), unquantise(4000));
    }

    /// One wire-format PNG chunk (length, type, payload, CRC).
    fn png_chunk(typ: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut h = crc32fast::Hasher::new();
        h.update(typ);
        h.update(data);
        let mut c = Vec::new();
        c.extend_from_slice(&(data.len() as u32).to_be_bytes());
        c.extend_from_slice(typ);
        c.extend_from_slice(data);
        c.extend_from_slice(&h.finalize().to_be_bytes());
        c
    }

    /// Wrap a filtered raw stream in a complete PNG: IHDR(depth, colour) +
    /// IDAT + IEND, like `Moo::compressPNG` frames its rows.
    fn ihdr_png(w: u32, h: u32, depth: u8, colour: u8, raw: &[u8]) -> Vec<u8> {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        z.write_all(raw).unwrap();
        let zd = z.finish().unwrap();
        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[depth, colour, 0, 0, 0]);
        png.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        png.extend_from_slice(&png_chunk(b"IDAT", &zd));
        png.extend_from_slice(&png_chunk(b"IEND", &[]));
        png
    }

    /// A gray16 PNG (filter-0 rows, wire big-endian) — the relative uint16
    /// height map payload shape.
    fn gray16_png(w: u32, h: u32, rel: &[u16]) -> Vec<u8> {
        let mut raw = Vec::new();
        for y in 0..h as usize {
            raw.push(0u8); // filter none
            for x in 0..w as usize {
                // The wire is big-endian; the reference image reads LE.
                let v = rel[y * w as usize + x].swap_bytes();
                raw.extend_from_slice(&v.to_be_bytes());
            }
        }
        ihdr_png(w, h, 16, 0, &raw)
    }

    /// A 32bpp RGBA PNG carrying one little-endian int32 per pixel — the
    /// quantised height map payload shape.
    fn rgba_png(w: u32, h: u32, words: &[i32]) -> Vec<u8> {
        let mut raw = Vec::new();
        for y in 0..h as usize {
            raw.push(0u8);
            for x in 0..w as usize {
                raw.extend_from_slice(&words[y * w as usize + x].to_le_bytes());
            }
        }
        ihdr_png(w, h, 8, 6, &raw)
    }
}
