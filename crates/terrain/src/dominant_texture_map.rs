//! BW-10-03-03: the Terrain2 dominant texture map reader.
//!
//! `DominantTextureMap2::load` (`terrain2/dominant_texture_map2.cpp:108-197`)
//! reads a `DominantTextureMapHeader` (`terrain_data.hpp:126-137`), a table
//! of fixed-width NUL-padded texture names, and an image of per-cell bytes
//! whose low nibble is the dominant material index and whose high nibble is
//! the ground (collision) type. The image sub-block is a `BinaryBlock`
//! compression stream (`resmgr/binary_block.cpp:256-343, 407-419`).
//!
//! The point queries (`dominant_texture_map.cpp:152-175, 208-217`) map a
//! block-relative coordinate with the `+0.5f` rounding quirk and clamp.

use std::fmt;

use flate2::read::ZlibDecoder;
use std::io::Read as _;

/// `DominantTextureMapHeader::MAGIC` — "mat\0" (`terrain_data.hpp:135`).
pub const DOMINANT_MAGIC: u32 = 0x0074_616d;
/// `DominantTextureMapHeader::VERSION_ZIP` (`terrain_data.hpp:128`).
pub const DOMINANT_VERSION_ZIP: u32 = 1;
/// `sizeof(DominantTextureMapHeader)` — 8 fields, 32 bytes.
pub const DOMINANT_HEADER_SIZE: usize = 32;

/// `COMPRESSED_MAGIC1` — "zip\0" (`binary_block.cpp:23`).
pub const ZIP_MAGIC1: u32 = 0x7a69_7000;
/// `COMPRESSED_MAGIC2` — a random number (`binary_block.cpp:24`).
pub const ZIP_MAGIC2: u32 = 0x42af_9021;
/// `COMPRESSED_HEADER_SZ * sizeof(uint32)` (`binary_block.cpp:25`).
pub const ZIP_HEADER_SIZE: usize = 12;

/// The load errors, each carrying the reference message where one exists
/// (`dominant_texture_map2.cpp:130-186`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DominantLoadError {
    /// `asBinary() == NULL` (`:132`).
    NotBinary,
    /// Wrong magic or version (`:144`).
    WrongMapType,
    /// Zero width/height (`:152`).
    ZeroDims,
    /// Image smaller than width*height (`:185`).
    Overflow,
    /// Registered divergence: the reference reads out of bounds where the
    /// port reports truncation (header, name table, image sub-block).
    Truncated,
}

impl fmt::Display for DominantLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DominantLoadError::NotBinary => {
                "Couldn't load dominant texture map: dataSection not binary."
            }
            DominantLoadError::WrongMapType => "Wrong map type when loading dominant texture map.",
            DominantLoadError::ZeroDims => {
                "Failed to load dominant texture map: image has zero width/height."
            }
            DominantLoadError::Overflow => {
                "Image data size overflow when loading dominant texture map."
            }
            DominantLoadError::Truncated => {
                "Registered divergence: the reference reads out of bounds here."
            }
        };
        f.write_str(s)
    }
}

impl std::error::Error for DominantLoadError {}

/// The 32-byte on-disk header (`terrain_data.hpp:126-137`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DominantTextureMapHeader {
    pub magic: u32,
    pub version: u32,
    pub num_textures: u32,
    pub texture_name_size: u32,
    pub width: u32,
    pub height: u32,
}

impl DominantTextureMapHeader {
    pub fn parse(data: &[u8]) -> Result<(Self, &[u8]), DominantLoadError> {
        if data.len() < DOMINANT_HEADER_SIZE {
            return Err(DominantLoadError::Truncated);
        }
        let (head, rest) = data.split_at(DOMINANT_HEADER_SIZE);
        let word = |i: usize| u32::from_le_bytes(head[i * 4..i * 4 + 4].try_into().unwrap());
        Ok((
            Self {
                magic: word(0),
                version: word(1),
                num_textures: word(2),
                texture_name_size: word(3),
                width: word(4),
                height: word(5),
                // pad_[2] never read.
            },
            rest,
        ))
    }
}

/// `BinaryBlock::isCompressed` (`binary_block.cpp:407-419`): longer than
/// the 12-byte header and both magic words match.
pub fn is_zip_block(data: &[u8]) -> bool {
    data.len() > ZIP_HEADER_SIZE
        && u32::from_le_bytes(data[0..4].try_into().unwrap()) == ZIP_MAGIC1
        && u32::from_le_bytes(data[4..8].try_into().unwrap()) == ZIP_MAGIC2
}

/// `BinaryBlock::decompress` (`binary_block.cpp:350-393`): the third header
/// word is the uncompressed size, the rest is a zlib stream. The reference
/// `MF_ASSERT`s `uncompress == Z_OK` — a corrupt or overlong stream panics
/// here the same way. A stream that decompresses shorter than the declared
/// size keeps the declared length (the reference tail is uninitialised
/// memory; the port zero-fills — registered).
pub fn zip_decompress(data: &[u8]) -> Vec<u8> {
    let expected = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    ZlibDecoder::new(&data[ZIP_HEADER_SIZE..])
        .read_to_end(&mut out)
        .expect("reference MF_ASSERTs uncompress == Z_OK");
    assert!(
        out.len() <= expected,
        "reference MF_ASSERTs uncompress == Z_OK"
    );
    out.resize(expected, 0);
    out
}

/// The minimal `Moo::Image<uint8>` face the loader needs: `get` clamps
/// both coordinates (`moo/image.ipp:347-354`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct U8Image {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl U8Image {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; width as usize * height as usize],
        }
    }

    /// Clamping `get` like the reference image.
    pub fn get(&self, x: i32, y: i32) -> u8 {
        let x = x.clamp(0, self.width as i32 - 1) as u32;
        let y = y.clamp(0, self.height as i32 - 1) as u32;
        self.data[(y * self.width + x) as usize]
    }

    /// Copy `width*height` bytes in like `copyFrom` (row-wise memcpy,
    /// `moo/image.ipp:861-871`; the caller has already bounds-checked).
    pub fn copy_from(&mut self, bytes: &[u8]) {
        let n = self.data.len();
        self.data.copy_from_slice(&bytes[..n]);
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

/// The dominant texture map: material indices in the low nibble, ground
/// (collision) types in the high nibble, one texture name per index.
#[derive(Debug, Clone, Default)]
pub struct DominantTextureMap2 {
    block_size: f32,
    image: U8Image,
    texture_names: Vec<String>,
}

impl DominantTextureMap2 {
    /// `DominantTextureMap2(blockSize)` for the load interface
    /// (`dominant_texture_map2.cpp:22-25`); `TerrainMap` asserts
    /// `!almostZero(blockSize)` (`terrain_map.hpp:30-33`).
    pub fn new(block_size: f32) -> Self {
        assert!(
            block_size.abs() >= 0.0004,
            "reference asserts blockSize is not almost zero"
        );
        Self {
            block_size,
            image: U8Image::default(),
            texture_names: Vec::new(),
        }
    }

    /// `DominantTextureMap2::load` (`dominant_texture_map2.cpp:108-197`).
    /// The port takes the section's binary payload; `None` is the
    /// reference's non-binary section.
    pub fn load(&mut self, data: Option<&[u8]>) -> Result<(), DominantLoadError> {
        let data = match data {
            Some(d) => d,
            None => return Err(DominantLoadError::NotBinary),
        };
        let (header, payload) = DominantTextureMapHeader::parse(data)?;

        if header.magic != DOMINANT_MAGIC || header.version != DOMINANT_VERSION_ZIP {
            return Err(DominantLoadError::WrongMapType);
        }
        if header.width == 0 || header.height == 0 {
            return Err(DominantLoadError::ZeroDims);
        }

        // Fixed-width NUL-padded name table. Each entry is the window
        // truncated at its first NUL; a window with no NUL keeps all
        // `textureNameSize` bytes (`dominant_texture_map2.cpp:160-170`,
        // the `strncpy` + `min(strlen, size)` pair).
        let texture_data_size =
            (header.texture_name_size.wrapping_mul(header.num_textures)) as usize;
        let names_end = DOMINANT_HEADER_SIZE
            .checked_add(texture_data_size)
            .ok_or(DominantLoadError::Truncated)?;
        if names_end > data.len() {
            return Err(DominantLoadError::Truncated);
        }
        let mut names = Vec::with_capacity(header.num_textures as usize);
        for i in 0..header.num_textures as usize {
            let start = DOMINANT_HEADER_SIZE + i * header.texture_name_size as usize;
            let window = &data[start..start + header.texture_name_size as usize];
            let end = window.iter().position(|&b| b == 0).unwrap_or(window.len());
            names.push(String::from_utf8_lossy(&window[..end]).into_owned());
        }
        self.texture_names = names;

        // Image sub-block: compressed or raw (`:172-180`). The reference
        // builds its length by unsigned subtraction — an underflow reads
        // out of bounds, the port reports truncation (registered).
        let image_bytes = &payload[texture_data_size..];
        let image_bytes = if is_zip_block(image_bytes) {
            zip_decompress(image_bytes)
        } else {
            image_bytes.to_vec()
        };

        let mut image = U8Image::new(header.width, header.height);
        if (header.width as usize * header.height as usize) > image_bytes.len() {
            return Err(DominantLoadError::Overflow);
        }
        image.copy_from(&image_bytes);
        self.image = image;
        Ok(())
    }

    /// `materialIndex` (`dominant_texture_map.cpp:152-162`): the
    /// `+0.5f` before the `int()` truncation rounds half up, the
    /// coordinate scales by `width - 1`, and the low nibble is the index.
    pub fn material_index(&self, x: f32, z: f32) -> u8 {
        let x = ((x / self.block_size) * (self.image.width - 1) as f32 + 0.5) as i32;
        let z = ((z / self.block_size) * (self.image.height - 1) as f32 + 0.5) as i32;
        self.image.get(x, z) & 0x0f
    }

    /// `groundType` (`dominant_texture_map.cpp:165-173`): the same
    /// rounded coordinate, high nibble.
    pub fn ground_type(&self, x: f32, z: f32) -> u8 {
        let x = ((x / self.block_size) * (self.image.width - 1) as f32 + 0.5) as i32;
        let z = ((z / self.block_size) * (self.image.height - 1) as f32 + 0.5) as i32;
        self.image.get(x, z) >> 4
    }

    /// `texture` (`dominant_texture_map.cpp:208-217`): the name for the
    /// dominant index; the reference `MF_ASSERT`s the index is in range.
    pub fn texture(&self, x: f32, z: f32) -> &str {
        let id = self.material_index(x, z) as usize;
        assert!(
            id < self.texture_names.len(),
            "reference MF_ASSERTs id < textureNames.size()"
        );
        &self.texture_names[id]
    }

    pub fn width(&self) -> u32 {
        self.image.width
    }

    pub fn height(&self) -> u32 {
        self.image.height
    }

    pub fn texture_names(&self) -> &[String] {
        &self.texture_names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: f32 = 100.0;

    /// A `BinaryBlock::compress` stream: magic pair, uncompressed size,
    /// zlib payload.
    fn zip_block(raw: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(raw).unwrap();
        let mut b = Vec::new();
        b.extend_from_slice(&ZIP_MAGIC1.to_le_bytes());
        b.extend_from_slice(&ZIP_MAGIC2.to_le_bytes());
        b.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        b.extend_from_slice(&z.finish().unwrap());
        b
    }

    /// A section payload in the save layout (`dominant_texture_map2.cpp:
    /// 47-104`): 32-byte header, NUL-padded name windows, image sub-block.
    fn fixture_blob(
        version: u32,
        w: u32,
        h: u32,
        names: &[&str],
        tns: u32,
        image: &[u8],
        zip: bool,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&DOMINANT_MAGIC.to_le_bytes());
        b.extend_from_slice(&version.to_le_bytes());
        b.extend_from_slice(&(names.len() as u32).to_le_bytes());
        b.extend_from_slice(&tns.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&[0u8; 8]); // pad_[2]
        for n in names {
            let mut win = vec![0u8; tns as usize];
            let n = &n.as_bytes()[..n.len().min(tns as usize)];
            win[..n.len()].copy_from_slice(n);
            b.extend_from_slice(&win);
        }
        if zip {
            b.extend_from_slice(&zip_block(image));
        } else {
            b.extend_from_slice(image);
        }
        b
    }

    #[test]
    fn null_section_reports_the_reference_message() {
        let mut map = DominantTextureMap2::new(BLOCK);
        assert_eq!(map.load(None), Err(DominantLoadError::NotBinary));
        assert_eq!(
            map.load(None).unwrap_err().to_string(),
            "Couldn't load dominant texture map: dataSection not binary."
        );
    }

    #[test]
    fn wrong_magic_or_version_reports_the_reference_message() {
        let img = [0u8; 16];
        for blob in [fixture_blob(2, 4, 4, &["a"], 2, &img, true), {
            let mut b = fixture_blob(1, 4, 4, &["a"], 2, &img, true);
            b[0] ^= 0xff; // break the magic
            b
        }] {
            let mut map = DominantTextureMap2::new(BLOCK);
            let err = map.load(Some(&blob)).unwrap_err();
            assert_eq!(err, DominantLoadError::WrongMapType);
            assert_eq!(
                err.to_string(),
                "Wrong map type when loading dominant texture map."
            );
        }
    }

    #[test]
    fn zero_dims_report_the_reference_message() {
        let img = [0u8; 1];
        for (w, h) in [(0u32, 4u32), (4, 0), (0, 0)] {
            let blob = fixture_blob(1, w, h, &["a"], 2, &img, true);
            let mut map = DominantTextureMap2::new(BLOCK);
            let err = map.load(Some(&blob)).unwrap_err();
            assert_eq!(err, DominantLoadError::ZeroDims, "{w}x{h}");
        }
        let mut map = DominantTextureMap2::new(BLOCK);
        assert_eq!(
            map.load(Some(&fixture_blob(1, 0, 0, &["a"], 2, &img, true)))
                .unwrap_err()
                .to_string(),
            "Failed to load dominant texture map: image has zero width/height."
        );
    }

    #[test]
    fn short_blobs_are_truncated() {
        // A header cut short, and a name table that runs past the blob.
        let mut map = DominantTextureMap2::new(BLOCK);
        assert_eq!(
            map.load(Some(&[0u8; 31])),
            Err(DominantLoadError::Truncated)
        );
        let img = [0u8; 16];
        let mut blob = fixture_blob(1, 4, 4, &["a"], 2, &img, true);
        blob.truncate(DOMINANT_HEADER_SIZE + 1); // one byte of one window
        let err = map.load(Some(&blob)).unwrap_err();
        assert_eq!(err, DominantLoadError::Truncated);
        assert_eq!(
            err.to_string(),
            "Registered divergence: the reference reads out of bounds here."
        );
    }

    #[test]
    fn the_u8_image_face_clamps_like_the_reference() {
        let img = U8Image::new(2, 3);
        assert_eq!((img.width(), img.height()), (2, 3));
        assert_eq!(img.get(0, 0), 0, "fresh cells are zero");
        assert_eq!(img.get(-5, 9), 0, "clamped to the last cell");
    }

    #[test]
    fn image_size_overflow_reports_the_reference_message() {
        let blob = fixture_blob(1, 4, 4, &["a"], 2, &[0u8; 15], true);
        let mut map = DominantTextureMap2::new(BLOCK);
        let err = map.load(Some(&blob)).unwrap_err();
        assert_eq!(err, DominantLoadError::Overflow);
        assert_eq!(
            err.to_string(),
            "Image data size overflow when loading dominant texture map."
        );
    }

    #[test]
    fn the_zip_sub_block_round_trips() {
        // 4x4 cells, three names; every cell a distinct material index
        // with a varying ground type.
        let image: Vec<u8> = (0..16u8).map(|i| ((i % 5) << 4) | i).collect();
        let blob = fixture_blob(1, 4, 4, &["grass", "rock", "sand"], 6, &image, true);
        let mut map = DominantTextureMap2::new(BLOCK);
        map.load(Some(&blob)).expect("load");
        assert_eq!(map.width(), 4);
        assert_eq!(map.height(), 4);
        assert_eq!(map.texture_names(), ["grass", "rock", "sand"]);
        for y in 0..4i32 {
            for x in 0..4i32 {
                let cell = (y * 4 + x) as u8;
                let probe = (x as f32 * 25.0 + 12.5, y as f32 * 25.0 + 12.5);
                assert_eq!(map.material_index(probe.0, probe.1), cell & 0x0f);
                assert_eq!(map.ground_type(probe.0, probe.1), (cell % 5));
            }
        }
    }

    #[test]
    fn the_uncompressed_image_branch_loads_verbatim() {
        // Save always compresses, but load only decompresses when the
        // magic pair is there — a raw sub-block is accepted as-is.
        let image: Vec<u8> = (0..16u8).collect();
        let blob = fixture_blob(1, 4, 4, &["a"], 2, &image, false);
        let mut map = DominantTextureMap2::new(BLOCK);
        map.load(Some(&blob)).expect("load");
        assert_eq!(map.material_index(12.5, 87.5), 3 * 4);
    }

    #[test]
    fn name_windows_truncate_at_the_first_nul() {
        // "abcd" fills its window with no NUL and keeps every byte;
        // "e\0" truncates to "e"; tns smaller than a name truncates it.
        let blob = fixture_blob(1, 1, 1, &["abcd", "e", "longer-than-window"], 4, &[0], true);
        let mut map = DominantTextureMap2::new(BLOCK);
        map.load(Some(&blob)).expect("load");
        assert_eq!(map.texture_names(), ["abcd", "e", "long"]);
    }

    #[test]
    fn queries_round_half_up_then_truncate() {
        // 4x4 grid, cell value = x + 4*y so the answer names the cell.
        let image: Vec<u8> = (0..16u8).collect();
        let blob = fixture_blob(1, 4, 4, &["a"], 2, &image, true);
        let mut map = DominantTextureMap2::new(BLOCK);
        map.load(Some(&blob)).expect("load");

        // (x/100)*3 + 0.5 crosses 2 exactly at x = 50 (round half up) and
        // crosses 1 between 16.66 and 16.67 (50/3) — a hair of input
        // flips the cell.
        assert_eq!(map.material_index(16.66, 12.5), 0);
        assert_eq!(map.material_index(16.67, 12.5), 1);
        assert_eq!(map.material_index(50.0, 12.5), 2);
        assert_eq!(map.material_index(49.99, 12.5), 1);
        // z scales by height - 1 = 3 the same way.
        assert_eq!(map.material_index(12.5, 16.66), 0);
        assert_eq!(map.material_index(12.5, 16.67), 4);
        assert_eq!(map.material_index(12.5, 50.0), 8);
        // Far coordinates clamp into the last cell; negatives clamp to 0.
        assert_eq!(map.material_index(200.0, 500.0), 15);
        assert_eq!(map.material_index(-50.0, -50.0), 0);
        assert_eq!(map.material_index(-0.01, -0.01), 0);
    }

    #[test]
    fn corrupt_zlib_panics_like_the_reference_assert() {
        let mut blob = fixture_blob(1, 4, 4, &["a"], 2, &[0u8; 16], true);
        // Keep the 12-byte zip header, corrupt the stream after it.
        for b in &mut blob[DOMINANT_HEADER_SIZE + 2 + ZIP_HEADER_SIZE..] {
            *b ^= 0xff;
        }
        let mut map = DominantTextureMap2::new(BLOCK);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| map.load(Some(&blob))));
        assert!(r.is_err(), "reference MF_ASSERTs uncompress == Z_OK");
    }

    #[test]
    fn texture_asserts_like_the_reference_when_the_index_is_out_of_range() {
        // Low nibble 7 with only two names: material_index is fine, the
        // name lookup MF_ASSERTs.
        let blob = fixture_blob(1, 1, 1, &["a", "b"], 2, &[7], true);
        let mut map = DominantTextureMap2::new(BLOCK);
        map.load(Some(&blob)).expect("load");
        assert_eq!(map.material_index(50.0, 50.0), 7);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| map.texture(50.0, 50.0)));
        assert!(r.is_err(), "reference MF_ASSERTs id < textureNames.size()");
    }

    #[test]
    fn constructor_asserts_an_almost_zero_block_size() {
        let r = std::panic::catch_unwind(|| DominantTextureMap2::new(0.0001));
        assert!(r.is_err());
        assert!(DominantTextureMap2::new(100.0).width() == 0);
    }
}
