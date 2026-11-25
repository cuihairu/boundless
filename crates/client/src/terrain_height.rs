//! BW-11-04-04 terrain height data: a port of the height map model
//! (`lib/terrain/terrain_height_map.{hpp,cpp}` base class plus the
//! `terrain2/terrain_height_map2.{hpp,cpp}` implementation), the half of the
//! terrain-and-environment item that the fixed-camera acceptance exercises
//! through pole data, sample spacing and the interpolated `heightAt` /
//! `normalAt` queries.
//!
//! Model notes:
//! - The pole image is a flat row-major `Vec<f32>` with `Moo::Image::get`'s
//!   boundary clamping (`image.ipp:349-354`): every lookup saturates to the
//!   nearest border pole, which is exactly how the `DEFAULT_VISIBLE_OFFSET`
//!   margin keeps edge queries on visible data. `internalHeightAt` adds the
//!   visible offset and relies on that clamp; it does no clamping itself.
//! - `refreshInternalDimensions` (`terrain_height_map2.hpp:268-281`) derives
//!   the sample spacing from `blockSize / (poles - visibleOffset*2 - 1)` —
//!   the `-1` converts a pole count to a block count, so lod 0 with the
//!   default offset yields `blockSize / (poles - 5)`, not `poles - 2`. A
//!   degenerate 0-pole header wraps the block count the way the reference's
//!   unsigned arithmetic does (`wrapping_sub`), giving a near-zero spacing
//!   instead of a panic.
//! - `normalAt(int,int)` keeps the reference's fake normal-y formula
//!   (`ret.y = spaceX*2 + spaceZ*2`, the live `#else` branch in
//!   `terrain_height_map2.cpp:784-792`): it exists to add WorldMachine-like
//!   contrast, not to be geometrically meaningful. The cached
//!   `diagonalDistanceX4` is written by `refreshInternalDimensions` but its
//!   only reader sits in an `#if 0` block, so it is write-only in the
//!   shipped reference.
//! - The loader ports the `VERSION_ABS_FLOAT` arm of `loadHeightMap`
//!   (`terrain_height_map2.cpp:421-436`) byte for byte, including the swap
//!   of an inverted `min/max` pair. The PNG and quantised-float versions
//!   need the reference decompressors, so they report the same error string
//!   the reference produces when `decompressImage`/`decompressHeightMap`
//!   fail. Truncated buffers and headers shorter than 32 bytes are reference
//!   crashes (the C++ reads past the blob); the port reports an error
//!   instead.

use foundation::math::Vector3;

/// `HeightMapHeader::MAGIC` (`terrain_data.hpp:40`): `'hmp\0'` as a
/// little-endian u32.
pub const MAGIC: u32 = 0x00706d68;

/// `HeightMapHeader::VERSION_ABS_FLOAT` (`terrain_data.hpp:26`).
pub const VERSION_ABS_FLOAT: u32 = 1;
/// `HeightMapHeader::VERSION_REL_UINT16` (`terrain_data.hpp:27`).
pub const VERSION_REL_UINT16: u32 = 2;
/// `HeightMapHeader::VERSION_REL_UINT16_PNG` (`terrain_data.hpp:28`).
pub const VERSION_REL_UINT16_PNG: u32 = 3;
/// `HeightMapHeader::VERSION_ABS_QFLOAT` (`terrain_data.hpp:29`).
pub const VERSION_ABS_QFLOAT: u32 = 4;

/// `COMPRESS_RAW` (`terrain_data.hpp:11`).
pub const COMPRESS_RAW: u32 = 0;

/// `MINIMUM_TERRAIN_HEIGHT_MAP` (`terrain_data.hpp:8`).
pub const MINIMUM_TERRAIN_HEIGHT_MAP: u32 = 32;

/// `TerrainHeightMap::DEFAULT_UNIT_SCALE` (`terrain_height_map.hpp:16`).
pub const DEFAULT_UNIT_SCALE: f32 = 0.01;

/// `TerrainHeightMap2::DEFAULT_VISIBLE_OFFSET` (`terrain_height_map2.hpp:49`):
/// the non-visible border of poles around each side of the block.
pub const DEFAULT_VISIBLE_OFFSET: u32 = 2;

/// Size of the on-disk `HeightMapHeader` in bytes: five u32s, two f32s and a
/// pad word.
pub const HEADER_SIZE: usize = 32;

/// The error strings the reference writes through its `error` out-parameter,
/// plus the truncation message the reference has no branch for.
const ERR_NOT_A_HEIGHT_MAP: &str = "dataSection is not for a TerrainHeightMap2";
const ERR_DECOMPRESS: &str = "The height map data could not be decompressed.";
const ERR_TRUNCATED: &str = "The height map data is truncated.";

/// `HeightMapHeader` (`terrain_data.hpp:22-42`): the fixed-layout block at
/// the front of every saved height map.
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
    /// Read the header off the front of a blob. The reference casts the data
    /// pointer to the struct; the port requires the full 32 bytes and reads
    /// them little-endian (the reference platform's layout).
    pub fn from_bytes(data: &[u8]) -> Option<HeightMapHeader> {
        if data.len() < HEADER_SIZE {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        let f32_at = |i: usize| f32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        Some(HeightMapHeader {
            magic: u32_at(0),
            width: u32_at(4),
            height: u32_at(8),
            compression: u32_at(12),
            version: u32_at(16),
            min_height: f32_at(20),
            max_height: f32_at(24),
            pad: u32_at(28),
        })
    }
}

/// A `TerrainHeightMap2` block: the pole grid with its spacing and the
/// cached values `refreshInternalDimensions` maintains.
#[derive(Debug)]
pub struct TerrainHeightMap {
    /// `heights_`: row-major, index `z * width + x`.
    heights: Vec<f32>,
    width: u32,
    height: u32,
    block_size: f32,
    unit_scale: f32,
    min_height: f32,
    max_height: f32,
    visible_offset: u32,
    lod_level: u32,
    internal_blocks_width: u32,
    internal_blocks_height: u32,
    internal_spacing_x: f32,
    internal_spacing_z: f32,
    diagonal_distance_x4: f32,
}

/// `int32(floorf(v))`: Rust's `as` casts truncate toward zero exactly like
/// the C cast and saturate instead of wrapping where the C is undefined.
fn int32_floor(v: f32) -> i32 {
    v.floor() as i32
}

impl TerrainHeightMap {
    /// The member initialisers of the constructor
    /// (`terrain_height_map2.cpp:62-79`) without the resize/refresh: a
    /// zero-pole map whose dimensions `load` fills in.
    fn empty(block_size: f32, lod_level: u32, unit_scale: f32) -> TerrainHeightMap {
        TerrainHeightMap {
            heights: Vec::new(),
            width: 0,
            height: 0,
            block_size,
            unit_scale,
            min_height: 0.0,
            max_height: 0.0,
            visible_offset: if lod_level > 0 {
                0
            } else {
                DEFAULT_VISIBLE_OFFSET
            },
            lod_level,
            internal_blocks_width: 0,
            internal_blocks_height: 0,
            internal_spacing_x: 0.0,
            internal_spacing_z: 0.0,
            diagonal_distance_x4: 0.0,
        }
    }

    /// The constructor (`terrain_height_map2.cpp:80-95`): a `size`-pole block
    /// plus the visible-offset margin, dimensions refreshed immediately.
    pub fn new(block_size: f32, size: u32, lod_level: u32, unit_scale: f32) -> TerrainHeightMap {
        let mut map = TerrainHeightMap::empty(block_size, lod_level, unit_scale);
        let dim = size + map.visible_offset * 2;
        map.width = dim;
        map.height = dim;
        map.heights = vec![0.0; (dim * dim) as usize];
        map.refresh_internal_dimensions();
        map
    }

    /// `TerrainHeightMap2::load` + `loadHeightMap`
    /// (`terrain_height_map2.cpp:355-530`): parse the header, then the
    /// `VERSION_ABS_FLOAT` payload. `visibleOffset_` comes from the lod level
    /// exactly as in the constructor.
    pub fn load(
        block_size: f32,
        lod_level: u32,
        unit_scale: f32,
        data: &[u8],
    ) -> Result<TerrainHeightMap, String> {
        let header = match HeightMapHeader::from_bytes(data) {
            Some(header) => header,
            // The reference casts and reads regardless; a blob that cannot
            // hold a header cannot be one.
            None => return Err(ERR_NOT_A_HEIGHT_MAP.to_string()),
        };
        if header.magic != MAGIC {
            return Err(ERR_NOT_A_HEIGHT_MAP.to_string());
        }

        let mut map = TerrainHeightMap::empty(block_size, lod_level, unit_scale);

        if header.version == VERSION_ABS_FLOAT {
            map.min_height = header.min_height;
            map.max_height = header.max_height;
            if map.max_height < map.min_height {
                std::mem::swap(&mut map.min_height, &mut map.max_height);
            }

            let poles = header.width as usize * header.height as usize;
            let payload = &data[HEADER_SIZE..];
            if payload.len() < poles * 4 {
                return Err(ERR_TRUNCATED.to_string());
            }
            map.width = header.width;
            map.height = header.height;
            map.heights = payload[..poles * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|chunk| f32::from_le_bytes(*chunk))
                .collect();
        } else if header.version == VERSION_REL_UINT16_PNG || header.version == VERSION_ABS_QFLOAT {
            // The PNG and quantised-float arms call decompressImage /
            // decompressHeightMap; those decompressors are not ported, so the
            // load fails the way the reference reports a failed
            // decompression.
            return Err(ERR_DECOMPRESS.to_string());
        } else {
            // `terrain_height_map2.cpp:511-521`: the message is the
            // concatenation of two string literals, so there is no space
            // before "You".
            return Err(format!(
                "Invalid TerrainHeightMap2 format {}.You must re-export from World Editor.",
                header.version
            ));
        }

        map.refresh_internal_dimensions();
        Ok(map)
    }

    /// `refreshInternalDimensions` (`terrain_height_map2.hpp:268-281`). The
    /// subtraction wraps on a degenerate 0-pole grid the way the reference's
    /// unsigned arithmetic does.
    fn refresh_internal_dimensions(&mut self) {
        self.internal_blocks_width = self
            .width
            .wrapping_sub(self.internal_visible_offset() * 2)
            .wrapping_sub(1);
        self.internal_blocks_height = self
            .height
            .wrapping_sub(self.internal_visible_offset() * 2)
            .wrapping_sub(1);
        self.internal_spacing_x = self.block_size / self.internal_blocks_width as f32;
        self.internal_spacing_z = self.block_size / self.internal_blocks_height as f32;
        self.diagonal_distance_x4 =
            (self.internal_spacing_x * self.internal_spacing_z * 2.0).sqrt() * 4.0;
    }

    /// `Moo::Image::get` (`image.ipp:349-354`): boundary-clamped lookup,
    /// `lines_[y][x]` — x is the in-row index, z the row.
    fn pole(&self, x: i32, z: i32) -> f32 {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let z = z.clamp(0, self.height as i32 - 1) as usize;
        self.heights[z * self.width as usize + x]
    }

    /// `internalHeightAt` (`terrain_height_map2.hpp:300-304`): the visible
    /// offset shift, with the clamp supplied by the image `get`.
    pub fn internal_height_at(&self, x: i32, z: i32) -> f32 {
        let offset = self.visible_offset as i32;
        self.pole(x + offset, z + offset)
    }

    /// `heightAt(int,int)` (`terrain_height_map2.cpp:688-692`).
    pub fn height_at_int(&self, x: i32, z: i32) -> f32 {
        self.internal_height_at(x, z)
    }

    /// `heightAt(float,float)` (`terrain_height_map2.cpp:702-771`): the
    /// diagonal-triangle interpolation whose cell picks its diagonal from
    /// the parity of the cell coordinates.
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        let xs = x / self.internal_spacing_x + self.visible_offset as f32;
        let zs = z / self.internal_spacing_z + self.visible_offset as f32;

        let xf = xs - xs.floor();
        let zf = zs - zs.floor();

        let x_off = int32_floor(xs);
        let z_off = int32_floor(zs);

        if (x_off ^ z_off) & 1 != 0 {
            let h01 = self.pole(x_off, z_off + 1);
            let h10 = self.pole(x_off + 1, z_off);
            if (1.0 - xf) > zf {
                let h00 = self.pole(x_off, z_off);
                h00 + (h10 - h00) * xf + (h01 - h00) * zf
            } else {
                let h11 = self.pole(x_off + 1, z_off + 1);
                h11 + (h01 - h11) * (1.0 - xf) + (h10 - h11) * (1.0 - zf)
            }
        } else {
            let h00 = self.pole(x_off, z_off);
            let h11 = self.pole(x_off + 1, z_off + 1);
            if xf > zf {
                let h10 = self.pole(x_off + 1, z_off);
                h10 + (h00 - h10) * (1.0 - xf) + (h11 - h10) * zf
            } else {
                let h01 = self.pole(x_off, z_off + 1);
                h01 + (h11 - h01) * xf + (h00 - h01) * (1.0 - zf)
            }
        }
    }

    /// `normalAt(int,int)` (`terrain_height_map2.cpp:773-845`): central
    /// differences plus the two diagonals, with the fake contrast y. The
    /// border slow path clamps through the same image `get`, so on a flat
    /// grid both paths agree.
    pub fn normal_at_int(&self, x: i32, z: i32) -> Vector3 {
        let space_x = self.internal_spacing_x;
        let space_z = self.internal_spacing_z;

        // The reference writes the literal `0.7071067811f` (sqrt(0.5)
        // truncated); keep the same bits rather than the library constant.
        #[allow(clippy::approx_constant, clippy::excessive_precision)]
        const DIAGONAL_MULTIPLIER: f32 = 0.7071067811;

        let mut ret = Vector3::ZERO;
        ret.y = space_x * 2.0 + space_z * 2.0;

        // The reference guard reads `x - 1 < 0 || z - 1 < 0 || x + 1 >=
        // width || z + 1 >= height`; keep its shape.
        let out_of_bounds =
            x - 1 < 0 || z - 1 < 0 || x + 1 >= self.width as i32 || z + 1 >= self.height as i32;

        let mut val1;
        let mut val2;
        if out_of_bounds {
            // Slower path, need to clamp in one or more axes.
            ret.x = self.pole(x - 1, z) - self.pole(x + 1, z);
            ret.z = self.pole(x, z - 1) - self.pole(x, z + 1);

            val1 = self.pole(x - 1, z - 1) - self.pole(x + 1, z + 1);
            val2 = self.pole(x - 1, z + 1) - self.pole(x + 1, z - 1);
        } else {
            // Fast path, everything in range, no need to clamp.
            ret.x = self.pole(x - 1, z) - self.pole(x + 1, z);
            ret.z = self.pole(x, z - 1) - self.pole(x, z + 1);

            val1 = self.pole(x - 1, z - 1) - self.pole(x + 1, z + 1);
            val2 = self.pole(x - 1, z + 1) - self.pole(x + 1, z - 1);
        }

        val1 *= DIAGONAL_MULTIPLIER;
        val2 *= DIAGONAL_MULTIPLIER;

        ret.x += val1;
        ret.z += val1;

        ret.x += val2;
        ret.z -= val2;
        ret.normalise();

        ret
    }

    /// `normalAt(float,float)` (`terrain_height_map2.cpp:847-916`): bilinear
    /// over the four corner normals, short-cutting whole axes.
    pub fn normal_at(&self, x: f32, z: f32) -> Vector3 {
        let xf = x / self.internal_spacing_x + self.visible_offset as f32;
        let zf = z / self.internal_spacing_z + self.visible_offset as f32;

        let x_frac = xf - xf.floor();
        let z_frac = zf - zf.floor();

        let xf = xf - x_frac;
        let zf = zf - z_frac;

        let xfi = xf as i32;
        let zfi = zf as i32;

        const FRAC_EPSILON: f32 = 1e-6;
        if x_frac.abs() < FRAC_EPSILON {
            if z_frac.abs() < FRAC_EPSILON {
                return self.normal_at_int(xfi, zfi);
            }
            let mut ret = self.normal_at_int(xfi, zfi) * (1.0 - z_frac);
            ret = ret + self.normal_at_int(xfi, zfi + 1) * z_frac;
            ret.normalise();
            return ret;
        } else if z_frac.abs() < FRAC_EPSILON {
            let mut ret = self.normal_at_int(xfi, zfi) * (1.0 - x_frac);
            ret = ret + self.normal_at_int(xfi + 1, zfi) * x_frac;
            ret.normalise();
            return ret;
        }

        let n1 = self.normal_at_int(xfi, zfi) * (1.0 - x_frac) * (1.0 - z_frac);
        let n2 = self.normal_at_int(xfi + 1, zfi) * x_frac * (1.0 - z_frac);
        let n3 = self.normal_at_int(xfi, zfi + 1) * (1.0 - x_frac) * z_frac;
        let n4 = self.normal_at_int(xfi + 1, zfi + 1) * x_frac * z_frac;

        let mut ret = n1;
        ret = ret + n2;
        ret = ret + n3;
        ret = ret + n4;
        ret.normalise();

        ret
    }

    /// `TerrainHeightMap::slopeAt` (`terrain_height_map.cpp:32-44`): the
    /// slope in degrees from a normal's clamped y.
    pub fn slope_at(&self, normal: Vector3) -> f32 {
        const RAD_TO_DEG: f32 = 180.0 / std::f32::consts::PI;
        RAD_TO_DEG * normal.y.clamp(-1.0, 1.0).acos()
    }

    /// `TerrainHeightMap::convertHeight2Float`
    /// (`terrain_height_map.hpp:215-218`).
    pub fn convert_height2_float(&self, height: i32) -> f32 {
        height as f32 * self.unit_scale
    }

    /// `TerrainHeightMap::convertFloat2Height`
    /// (`terrain_height_map.hpp:220-224`).
    pub fn convert_float2_height(&self, height: f32) -> i32 {
        ((height + 0.5 * self.unit_scale) / self.unit_scale).floor() as i32
    }

    /// `spacingX` (`terrain_height_map2.cpp:554-561`).
    pub fn spacing_x(&self) -> f32 {
        self.internal_spacing_x
    }

    /// `spacingZ` (`terrain_height_map2.cpp:569-576`).
    pub fn spacing_z(&self) -> f32 {
        self.internal_spacing_z
    }

    /// `blocksWidth` (`terrain_height_map2.cpp:590-597`).
    pub fn blocks_width(&self) -> u32 {
        self.internal_blocks_width
    }

    /// `blocksHeight` (`terrain_height_map2.cpp:606-613`).
    pub fn blocks_height(&self) -> u32 {
        self.internal_blocks_height
    }

    /// `verticesWidth` (`terrain_height_map2.cpp:628-635`).
    pub fn vertices_width(&self) -> u32 {
        self.width - self.visible_offset * 2
    }

    /// `verticesHeight` (`terrain_height_map2.cpp:643-650`).
    pub fn vertices_height(&self) -> u32 {
        self.height - self.visible_offset * 2
    }

    /// `polesWidth` (`terrain_height_map2.cpp:668-675`).
    pub fn poles_width(&self) -> u32 {
        self.width
    }

    /// `polesHeight` (`terrain_height_map2.cpp:682-689`).
    pub fn poles_height(&self) -> u32 {
        self.height
    }

    /// `internalVisibleOffset` (`terrain_height_map2.hpp:263-266`).
    pub fn internal_visible_offset(&self) -> u32 {
        self.visible_offset
    }

    /// `lodLevel` (`terrain_height_map2.hpp:306-309`).
    pub fn lod_level(&self) -> u32 {
        self.lod_level
    }

    /// `blockSize` (`terrain_map.hpp:45`).
    pub fn block_size(&self) -> f32 {
        self.block_size
    }

    /// `unitScale_` (`terrain_height_map.hpp:202`).
    pub fn unit_scale(&self) -> f32 {
        self.unit_scale
    }

    /// `minHeight` (`terrain_height_map.hpp`).
    pub fn min_height(&self) -> f32 {
        self.min_height
    }

    /// `maxHeight` (`terrain_height_map.hpp`).
    pub fn max_height(&self) -> f32 {
        self.max_height
    }

    /// `quadtreeThreshold` (`terrain_height_map2.hpp:314-319`).
    pub fn quadtree_threshold(&self) -> f32 {
        (self.internal_spacing_x * 4.0).max(4.0)
    }

    /// Test probe for the cached diagonal: `diagonalDistanceX4_` is
    /// write-only in the shipped reference (its only reader sits in an
    /// `#if 0` block), so there is no public accessor to port.
    #[cfg(test)]
    fn diagonal_cache_probe(&self) -> f32 {
        self.diagonal_distance_x4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 0.01;

    fn almost(value: f32, expected: f32) {
        assert!((value - expected).abs() < EPSILON, "{value} !~ {expected}");
    }

    /// A raw-float blob with every pole at `value` and header min/max set
    /// around it.
    fn flat_grid(width: u32, height: u32, value: f32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&MAGIC.to_le_bytes());
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&COMPRESS_RAW.to_le_bytes());
        data.extend_from_slice(&VERSION_ABS_FLOAT.to_le_bytes());
        data.extend_from_slice(&(value - 1.0).to_le_bytes());
        data.extend_from_slice(&(value + 1.0).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..width * height {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data
    }

    /// A raw-float blob whose pole (x,z) height is `f(x, z)` over the *raw*
    /// pole coordinates (the visible margin included).
    fn grid_with(width: u32, height: u32, f: impl Fn(u32, u32) -> f32) -> Vec<u8> {
        let mut data = flat_grid(width, height, 0.0);
        for z in 0..height {
            for x in 0..width {
                let i = HEADER_SIZE + ((z * width + x) * 4) as usize;
                data[i..i + 4].copy_from_slice(&f(x, z).to_le_bytes());
            }
        }
        data
    }

    #[test]
    fn header_layout_is_little_endian_and_fixed() {
        let data = flat_grid(5, 5, 3.0);
        let header = HeightMapHeader::from_bytes(&data).unwrap();
        assert_eq!(header.magic, MAGIC);
        assert_eq!(header.magic, 0x00706d68);
        assert_eq!(header.width, 5);
        assert_eq!(header.height, 5);
        assert_eq!(header.compression, COMPRESS_RAW);
        assert_eq!(header.version, VERSION_ABS_FLOAT);
        almost(header.min_height, 2.0);
        almost(header.max_height, 4.0);
        assert_eq!(header.pad, 0);
        assert_eq!(data.len(), HEADER_SIZE + 25 * 4);

        assert!(HeightMapHeader::from_bytes(&data[..31]).is_none());
    }

    #[test]
    fn constants_match_the_reference_headers() {
        assert_eq!(VERSION_ABS_FLOAT, 1);
        assert_eq!(VERSION_REL_UINT16, 2);
        assert_eq!(VERSION_REL_UINT16_PNG, 3);
        assert_eq!(VERSION_ABS_QFLOAT, 4);
        assert_eq!(COMPRESS_RAW, 0);
        assert_eq!(MINIMUM_TERRAIN_HEIGHT_MAP, 32);
        assert_eq!(DEFAULT_UNIT_SCALE, 0.01);
        assert_eq!(DEFAULT_VISIBLE_OFFSET, 2);
    }

    #[test]
    fn load_rejects_blobs_that_are_not_height_maps() {
        // No header at all.
        assert_eq!(
            TerrainHeightMap::load(100.0, 0, DEFAULT_UNIT_SCALE, &[]).unwrap_err(),
            ERR_NOT_A_HEIGHT_MAP
        );

        // Wrong magic.
        let mut data = flat_grid(5, 5, 1.0);
        data[0..4].copy_from_slice(&0x00706d69u32.to_le_bytes());
        assert_eq!(
            TerrainHeightMap::load(100.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap_err(),
            ERR_NOT_A_HEIGHT_MAP
        );
    }

    #[test]
    fn load_reports_unknown_versions_with_the_exact_message() {
        let mut data = flat_grid(5, 5, 1.0);
        data[16..20].copy_from_slice(&9u32.to_le_bytes());
        assert_eq!(
            TerrainHeightMap::load(100.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap_err(),
            "Invalid TerrainHeightMap2 format 9.You must re-export from World Editor."
        );
    }

    #[test]
    fn compressed_versions_fail_like_a_failed_reference_decompression() {
        for version in [VERSION_REL_UINT16_PNG, VERSION_ABS_QFLOAT] {
            let mut data = flat_grid(5, 5, 1.0);
            data[16..20].copy_from_slice(&version.to_le_bytes());
            assert_eq!(
                TerrainHeightMap::load(100.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap_err(),
                ERR_DECOMPRESS
            );
        }
    }

    #[test]
    fn truncated_float_payload_is_rejected() {
        let data = flat_grid(5, 5, 1.0);
        assert_eq!(
            TerrainHeightMap::load(100.0, 0, DEFAULT_UNIT_SCALE, &data[..data.len() - 4])
                .unwrap_err(),
            ERR_TRUNCATED
        );
    }

    #[test]
    fn lod_zero_keeps_the_two_pole_margin_and_higher_lods_drop_it() {
        // 12 visible poles + the 4-pole margin = 16 poles, 11 blocks of 10.
        let lod0 = TerrainHeightMap::new(110.0, 12, 0, DEFAULT_UNIT_SCALE);
        assert_eq!(lod0.internal_visible_offset(), DEFAULT_VISIBLE_OFFSET);
        assert_eq!(lod0.poles_width(), 16);
        assert_eq!(lod0.poles_height(), 16);
        assert_eq!(lod0.blocks_width(), 11);
        assert_eq!(lod0.blocks_height(), 11);
        assert_eq!(lod0.vertices_width(), 12);
        assert_eq!(lod0.vertices_height(), 12);
        assert_eq!(lod0.lod_level(), 0);

        let lod1 = TerrainHeightMap::new(110.0, 12, 1, DEFAULT_UNIT_SCALE);
        assert_eq!(lod1.internal_visible_offset(), 0);
        assert_eq!(lod1.poles_width(), 12);
        assert_eq!(lod1.blocks_width(), 11);
        assert_eq!(lod1.lod_level(), 1);

        // spacing = blockSize / blocks on both axes; the cached diagonal is
        // sqrt(sx*sz*2)*4.
        almost(lod0.spacing_x(), 10.0);
        almost(lod0.spacing_z(), 10.0);
        almost(
            lod0.diagonal_cache_probe(),
            (10.0f32 * 10.0 * 2.0).sqrt() * 4.0,
        );
        // The quadtree threshold floors at 4 cm.
        almost(lod0.quadtree_threshold(), 40.0);
        almost(lod1.quadtree_threshold(), 40.0);
    }

    #[test]
    fn pole_lookups_clamp_to_the_border_like_the_reference_image_get() {
        // 9x9 poles, lod 0 (offset 2), 4 blocks of 10.
        let data = flat_grid(9, 9, 7.0);
        let map = TerrainHeightMap::load(40.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap();
        assert_eq!(map.blocks_width(), 4);
        assert_eq!(map.blocks_height(), 4);

        // Internal coordinates -2..=6 all land on a real pole.
        for x in -2..7 {
            for z in -2..7 {
                almost(map.internal_height_at(x, z), 7.0);
            }
        }

        // Out-of-range pole queries saturate at the border, not wrap.
        almost(map.internal_height_at(-100, 0), 7.0);
        almost(map.internal_height_at(0, 100), 7.0);
    }

    #[test]
    fn height_at_reads_pole_values_and_interpolates_the_diagonal_mesh() {
        // 9x9 poles, lod 0 (offset 2), spacing 10. Raw pole heights are
        // h(x,z) = x + 10z, so the visible poles at internal (0,0)..(4,4)
        // read 22, 23, ... 55 diagonally by +1 in x and +10 in z.
        let data = grid_with(9, 9, |x, z| x as f32 + 10.0 * z as f32);
        let map = TerrainHeightMap::load(40.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap();
        assert_eq!(map.blocks_width(), 4);
        almost(map.spacing_x(), 10.0);

        // Pole queries land exactly.
        almost(map.height_at_int(0, 0), 22.0);
        almost(map.height_at_int(3, 3), 55.0);

        // Whole and half cells of the even-parity block whose origin pole is
        // (2,2) = 22.
        almost(map.height_at(0.0, 0.0), 22.0);
        almost(map.height_at(10.0, 0.0), 23.0);
        almost(map.height_at(0.0, 10.0), 32.0);
        almost(map.height_at(10.0, 10.0), 33.0);
        // xf == zf == 0.5 takes the h01 triangle: 32 + (33-32)*0.5 +
        // (22-32)*0.5.
        almost(map.height_at(5.0, 5.0), 27.5);
        // xf=0.25 > zf=0 takes the h10 triangle: 23 + (22-23)*0.75.
        almost(map.height_at(2.5, 0.0), 22.25);
        // xf=0.7, zf=0.3, same even cell, h10 triangle: 23 - 0.3 + 3.
        almost(map.height_at(7.0, 3.0), 25.7);

        // An odd-parity cell (xOff=3, zOff=4): xf=0.2, zf=0.3 and
        // (1-xf)=0.8 > zf takes the h00 triangle: h00=pole(3,4)=43 plus
        // (44-43)*0.2 plus (53-43)*0.3.
        almost(map.height_at(12.0, 23.0), 46.2);
        // The same parity cell with xf=0.7, zf=0.5, so (1-xf)=0.3 <= zf
        // takes the h11 triangle: h11=pole(4,5)=54 plus (53-54)*0.3 plus
        // (44-54)*0.5.
        almost(map.height_at(17.0, 25.0), 48.7);
    }

    #[test]
    fn normal_at_int_uses_central_differences_and_the_fake_contrast_y() {
        // A rise of one pole per pole index in x, flat in z: 9x9 poles, lod
        // 0, spacing 10.
        let data = grid_with(9, 9, |x, _| x as f32);
        let map = TerrainHeightMap::load(40.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap();

        // Unnormalised: y = 10*2 + 10*2 = 40; x = h(1)-h(3) = -2; z = 0; the
        // diagonals both read -2 and rotate into x/z cancelling z. After
        // normalising: (40, -4.828427, 0) / 40.29052.
        let n = map.normal_at_int(2, 2);
        almost(n.y, 0.99279);
        almost(n.x, -0.11984);
        almost(n.z, 0.0);
        almost((n.x * n.x + n.y * n.y + n.z * n.z).sqrt(), 1.0);

        // A rise in z only: h(x,z) = z gives (40, 0, -4.828427) normalised.
        let data = grid_with(9, 9, |_, z| z as f32);
        let map = TerrainHeightMap::load(40.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap();
        let n = map.normal_at_int(2, 2);
        almost(n.z, -0.11984);
        almost(n.x, 0.0);
        almost(n.y, 0.99279);
    }

    #[test]
    fn normal_at_int_border_path_matches_the_clamped_slow_path() {
        // Flat grid: the border slow path must also produce the up normal.
        let data = flat_grid(9, 9, 5.0);
        let map = TerrainHeightMap::load(40.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap();
        // (0,0) and (3,3) take different code paths; both are flat.
        for (x, z) in [(0, 0), (0, 3), (3, 0), (3, 3)] {
            let n = map.normal_at_int(x, z);
            almost(n.y, 1.0);
            almost(n.x, 0.0);
            almost(n.z, 0.0);
        }
    }

    #[test]
    fn normal_at_float_shortcuts_whole_axes_and_bilinears_the_rest() {
        // A ridge along z: pole height = |x - 4|.
        let data = grid_with(9, 9, |x, _| (x as i32 - 4).abs() as f32);
        let map = TerrainHeightMap::load(40.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap();

        // Whole axes: world (30,20) sits on pole (5,4) exactly.
        let n = map.normal_at(30.0, 20.0);
        let pole = map.normal_at_int(5, 4);
        assert_eq!(n.x, pole.x);
        assert_eq!(n.y, pole.y);
        assert_eq!(n.z, pole.z);

        // Fractional x only (z whole): world (15,0) is between poles (3,2)
        // and (4,2), blended 50/50 then renormalised.
        let mut expected = map.normal_at_int(3, 2) * 0.5 + map.normal_at_int(4, 2) * 0.5;
        expected.normalise();
        let n = map.normal_at(15.0, 0.0);
        assert!((n.x - expected.x).abs() < 1e-5);
        assert!((n.y - expected.y).abs() < 1e-5);
        assert!((n.z - expected.z).abs() < 1e-5);

        // Fractional z only (x whole): world (10,3) is between poles (3,2)
        // and (3,3), blended 70/30 along z then renormalised.
        let mut expected = map.normal_at_int(3, 2) * 0.7 + map.normal_at_int(3, 3) * 0.3;
        expected.normalise();
        let n = map.normal_at(10.0, 3.0);
        assert!((n.x - expected.x).abs() < 1e-5);
        assert!((n.y - expected.y).abs() < 1e-5);
        assert!((n.z - expected.z).abs() < 1e-5);

        // Both fractional: world (17,6) sits at xFrac=0.7, zFrac=0.6 inside
        // the cell whose corners are poles (3,2)..(4,3); the bilinear blends
        // the four corner normals with the reference weight layout.
        let n1 = map.normal_at_int(3, 2);
        let n2 = map.normal_at_int(4, 2);
        let n3 = map.normal_at_int(3, 3);
        let n4 = map.normal_at_int(4, 3);
        let mut expected =
            n1 * (0.3 * 0.4) + n2 * (0.7 * 0.4) + n3 * (0.3 * 0.6) + n4 * (0.7 * 0.6);
        expected.normalise();
        let n = map.normal_at(17.0, 6.0);
        assert!((n.x - expected.x).abs() < 1e-5);
        assert!((n.y - expected.y).abs() < 1e-5);
        assert!((n.z - expected.z).abs() < 1e-5);
    }

    #[test]
    fn slope_converts_the_normal_y_to_degrees() {
        // Flat grid -> up normal -> slope 0.
        let data = flat_grid(9, 9, 2.0);
        let map = TerrainHeightMap::load(40.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap();
        let n = map.normal_at_int(2, 2);
        almost(map.slope_at(n), 0.0);

        // Unit-y 0.5 -> 60 degrees.
        almost(map.slope_at(Vector3::new(0.0, 0.5, 0.0)), 60.0);
        // Clamped outside [-1,1].
        almost(map.slope_at(Vector3::new(0.0, 5.0, 0.0)), 0.0);
        almost(map.slope_at(Vector3::new(0.0, -5.0, 0.0)), 180.0);
    }

    #[test]
    fn unit_scale_converters_round_trip_through_the_quantised_ints() {
        let map = TerrainHeightMap::new(100.0, 8, 0, DEFAULT_UNIT_SCALE);
        almost(map.convert_height2_float(1234), 12.34);
        assert_eq!(map.convert_float2_height(12.34), 1234);
        assert_eq!(map.convert_float2_height(12.344), 1234);
        assert_eq!(map.convert_float2_height(12.35), 1235);
        assert_eq!(map.unit_scale(), DEFAULT_UNIT_SCALE);
        assert_eq!(map.block_size(), 100.0);
    }

    #[test]
    fn load_swaps_inverted_min_max_like_the_reference() {
        let mut data = flat_grid(5, 5, 3.0);
        // Header carries max < min.
        data[20..24].copy_from_slice(&4.0f32.to_le_bytes());
        data[24..28].copy_from_slice(&2.0f32.to_le_bytes());
        let map = TerrainHeightMap::load(100.0, 0, DEFAULT_UNIT_SCALE, &data).unwrap();
        almost(map.min_height(), 2.0);
        almost(map.max_height(), 4.0);
    }

    #[test]
    fn new_blocks_have_zero_min_max_and_a_zero_grid() {
        let map = TerrainHeightMap::new(100.0, 8, 0, DEFAULT_UNIT_SCALE);
        assert_eq!(map.min_height(), 0.0);
        assert_eq!(map.max_height(), 0.0);
        almost(map.height_at_int(0, 0), 0.0);
        almost(map.height_at(0.0, 0.0), 0.0);
    }
}
