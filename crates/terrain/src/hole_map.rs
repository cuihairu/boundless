//! BW-10-03-02 the Terrain2 hole map
//! (`lib/terrain/terrain2/terrain_hole_map2.*`).
//!
//! Reader face of `TerrainHoleMap2`: the 16-byte `HolesHeader`
//! (`terrain_data.hpp:78-86`), the load path through the file-local
//! `HoleHelper` (`terrain_hole_map2.cpp:26-129`, `load` 345-411), the
//! lock/image/unlock state machine, `recalcHoles` (556-591) and the two
//! `holeAt` faces (599-638).
//!
//! Reference quirks are ported on purpose and cited at each site: the
//! load-side row stride `(width/8) + ((width & 3) ? 1 : 0)` disagrees with
//! the writer's `ceil(width/8)` for every `width % 8 == 4`, and the
//! degenerate (all/none) maps drop their image but keep the dimensions so
//! the next lock refills it.

/// `HolesHeader::MAGIC` — "hol\0" (`terrain_data.hpp:84-85`).
pub const HOLES_MAGIC: u32 = 0x006c6f68;
/// 16 bytes: magic/width/height/version.
pub const HOLES_HEADER_SIZE: usize = 16;

/// `HolesHeader` (`terrain_data.hpp:78-86`). `version_` is written as 1 by
/// the reference save but never read back (`load` ignores it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HolesHeader {
    pub magic: u32,
    pub width: u32,
    pub height: u32,
    pub version: u32,
}

impl HolesHeader {
    /// Parse the fixed 16-byte header and return it with the payload slice.
    pub fn parse(data: &[u8]) -> Result<(Self, &[u8]), HoleLoadError> {
        if data.len() < HOLES_HEADER_SIZE {
            return Err(HoleLoadError::Truncated);
        }
        let u32_at = |off: usize| {
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };
        let header = Self {
            magic: u32_at(0),
            width: u32_at(4),
            height: u32_at(8),
            version: u32_at(12),
        };
        Ok((header, &data[HOLES_HEADER_SIZE..]))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HoleLoadError {
    /// `dataSection->asBinary()` is null — the reference message is
    /// "NULL DataSection" (`terrain_hole_map2.cpp:407-408`).
    NullDataSection,
    /// The blob ends before the row reads complete. The reference would
    /// read out of bounds here; the port reports it instead (registered
    /// divergence, as with the height map).
    Truncated,
}

impl core::fmt::Display for HoleLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HoleLoadError::NullDataSection => write!(f, "NULL DataSection"),
            HoleLoadError::Truncated => write!(f, "hole map blob is truncated"),
        }
    }
}

/// `BW::largerPow2` (`math/math_extra.cpp:49-73`): the next power of two at
/// or above `number`, with 0 -> 1 and a `MAX_POW_2` (2^31) cap.
fn larger_pow2(number: u32) -> u32 {
    const MAX_POW_2: u32 = 0x8000_0000;
    if number == 0 {
        return 1;
    }
    if number > MAX_POW_2 {
        // WARNING_MSG "returning a smaller power of 2" in the reference.
        return MAX_POW_2;
    }
    let mut n = number - 1;
    n |= n >> 1;
    n |= n >> 2;
    n |= n >> 4;
    n |= n >> 8;
    n |= n >> 16;
    n + 1
}

/// The packed `Moo::Image<bool>` face the hole map uses
/// (`image.ipp:1350-2010` bool specialization): rows packed to bytes,
/// `stride = ceil(width/8)`, bit `x & 7` of byte `x >> 3`.
///
/// `None` data mirrors `data_ == NULL`: `isEmpty()` is allocation state,
/// not zero dimensions, and `clear()` frees the pixels while keeping
/// width/height (the reference `destroy()` does exactly that).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoolImage {
    width: u32,
    height: u32,
    stride: u32,
    data: Option<Vec<u8>>,
}

impl BoolImage {
    /// `Image<bool>::stride(w)` (`image.ipp:1972-1975`).
    fn stride_for(w: u32) -> u32 {
        (w >> 3) + u32::from(!w.is_multiple_of(8))
    }

    /// `resize(w, h, value)` (`image.ipp`): memset 0xff/0x00 — the padding
    /// bits of the last byte are set along with the real ones.
    pub fn resize(&mut self, w: u32, h: u32, value: bool) {
        self.width = w;
        self.height = h;
        self.stride = Self::stride_for(w);
        let fill = if value { 0xff } else { 0x00 };
        self.data = Some(vec![fill; self.stride as usize * h as usize]);
    }

    /// `Image(w, h)` ctor: zero-filled allocation.
    pub fn new(w: u32, h: u32) -> Self {
        let mut image = Self::default();
        image.resize(w, h, false);
        image
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_none()
    }

    /// `clear()` = `destroy()`: frees the pixels, keeps the dimensions.
    pub fn clear(&mut self) {
        self.data = None;
    }

    /// `get(x, y)`: both coordinates clamp to `[0, dim-1]`
    /// (`image.ipp:1666-1674`).
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    pub fn get(&self, x: i32, y: i32) -> bool {
        let data = self.data.as_ref().expect("image is allocated");
        let x = x.clamp(0, self.width as i32 - 1);
        let y = y.clamp(0, self.height as i32 - 1);
        let (x, y) = (x as u32, y as u32);
        data[(y * self.stride + (x >> 3)) as usize] & (1 << (x & 0x7)) != 0
    }

    /// `set(x, y, value)` with the same clamping (`image.ipp:1676-1689`).
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    pub fn set(&mut self, x: i32, y: i32, value: bool) {
        let data = self.data.as_mut().expect("image is allocated");
        let x = x.clamp(0, self.width as i32 - 1);
        let y = y.clamp(0, self.height as i32 - 1);
        let (x, y) = (x as u32, y as u32);
        let byte = &mut data[(y * self.stride + (x >> 3)) as usize];
        if value {
            *byte |= 1 << (x & 0x7);
        } else {
            *byte &= !(1 << (x & 0x7));
        }
    }

    /// `rawRowSize()` = the packed stride.
    pub fn raw_row_size(&self) -> u32 {
        self.stride
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// `getRawRow(y)` (`image.ipp:1658-1663`): the top is the only clamp —
    /// `y >= height - 1` snaps to the last row.
    fn get_raw_row_mut(&mut self, y: u32) -> &mut [u8] {
        let data = self.data.as_mut().expect("image is allocated");
        let y = if y >= self.height - 1 {
            self.height - 1
        } else {
            y
        };
        let start = (y * self.stride) as usize;
        &mut data[start..start + self.stride as usize]
    }
}

/// Reader face of `TerrainHoleMap2` (the server build; the client-only
/// DXT1 texture face and `save` are not ported — registered boundary).
#[derive(Debug, Clone)]
pub struct TerrainHoleMap2 {
    block_size: f32,
    image: BoolImage,
    width: u32,
    height: u32,
    lock_count: usize,
    read_only: bool,
    all_holes: bool,
    no_holes: bool,
    holes_size: u32,
    holes_map_size: u32,
}

impl TerrainHoleMap2 {
    /// `TerrainHoleMap2(block)` — the block only contributes its block size
    /// (`terrain_hole_map2.cpp:138-153`); `TerrainMap` asserts
    /// `!almostZero(blockSize)` (`terrain_map.hpp:30-33`, epsilon 0.0004).
    pub fn new(block_size: f32) -> Self {
        assert!(
            block_size.abs() >= 0.0004,
            "reference asserts blockSize is not almost zero"
        );
        Self {
            block_size,
            image: BoolImage::default(),
            width: 0,
            height: 0,
            lock_count: 0,
            read_only: true,
            all_holes: false,
            no_holes: true,
            holes_size: 0,
            holes_map_size: 0,
        }
    }

    pub fn no_holes(&self) -> bool {
        self.no_holes
    }

    pub fn all_holes(&self) -> bool {
        self.all_holes
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// `holesSize()` — the header width (`terrain_hole_map2.cpp:371`).
    pub fn holes_size(&self) -> u32 {
        self.holes_size
    }

    /// `holesMapSize()` — `largerPow2(holesSize)` (`cpp:372`).
    pub fn holes_map_size(&self) -> u32 {
        self.holes_map_size
    }

    pub fn block_size(&self) -> f32 {
        self.block_size
    }

    /// `lock(readOnly)` (`terrain_hole_map2.cpp:212-233`): the nesting
    /// MF_ASSERT, the degenerate-refill resize, and the first-lock-wins
    /// read-only flag.
    pub fn lock(&mut self, read_only: bool) -> bool {
        assert!(
            self.lock_count == 0
                || (self.lock_count != 0 && (self.read_only == read_only || !self.read_only)),
            "reference MF_ASSERTs the lock nesting rule"
        );
        // If the image was empty because there are no holes or everything
        // was a hole then create the image.
        if self.lock_count == 0 && self.image.is_empty() && self.width != 0 && self.height != 0 {
            self.image.resize(self.width, self.height, self.all_holes);
        }
        if self.lock_count == 0 {
            self.read_only = read_only;
        }
        self.lock_count += 1;
        true
    }

    /// `image()` — `MF_ASSERT(lockCount_ != 0)` before access
    /// (`terrain_hole_map2.cpp:241-259`).
    pub fn image(&self) -> &BoolImage {
        assert!(self.lock_count != 0, "reference MF_ASSERTs lockCount");
        &self.image
    }

    /// Mutable `image()` for edits between lock and unlock.
    pub fn image_mut(&mut self) -> &mut BoolImage {
        assert!(self.lock_count != 0, "reference MF_ASSERTs lockCount");
        &mut self.image
    }

    /// `unlock()` (`terrain_hole_map2.cpp:265-293`). A redundant unlock
    /// wraps the counter like the reference's unsigned decrement (the
    /// zero check then simply does not fire). The client texture update
    /// is not ported (server face).
    pub fn unlock(&mut self) -> bool {
        let ret = true;
        self.lock_count = self.lock_count.wrapping_sub(1);
        if self.lock_count == 0 {
            if !self.read_only {
                // also clears image if all holes or no holes.
                self.recalc_holes();
            }
            self.read_only = true;
        }
        ret
    }

    /// `create(size)` (`terrain_hole_map2.cpp:422-443`): an empty (no
    /// holes) map of `size` x `size`.
    pub fn create(&mut self, size: u32) {
        self.holes_size = size;
        self.holes_map_size = larger_pow2(size);
        self.width = size;
        self.height = size;
        self.all_holes = false;
        self.no_holes = true;
        self.image.clear();
    }

    /// `load(dataSection)` (`terrain_hole_map2.cpp:345-411`), server face.
    ///
    /// Boundary: the reference first asks the section for binary data and
    /// un-zips `BinaryBlock` compression if present (`cpp:366-367`). The
    /// port takes the plain bytes — `None` is the null-section path, and
    /// the zip face belongs to the resource layer, which hands over
    /// decompressed blocks.
    pub fn load(&mut self, data: Option<&[u8]>) -> Result<(), HoleLoadError> {
        self.width = 0;
        self.height = 0;
        self.all_holes = false;
        self.no_holes = true;
        self.holes_size = 0;
        self.holes_map_size = 0;
        self.image.clear();

        let bytes = match data {
            None => return Err(HoleLoadError::NullDataSection),
            Some(bytes) => bytes,
        };
        let (header, payload) = HolesHeader::parse(bytes)?;
        // HoleHelper ctor: `MF_ASSERT(pHeader_->magic_ == HolesHeader::MAGIC)`
        // (cpp:38) — an assert, not an error branch.
        assert!(
            header.magic == HOLES_MAGIC,
            "reference MF_ASSERTs the holes magic"
        );

        self.holes_size = header.width;
        self.holes_map_size = larger_pow2(header.width);

        // `image_ = Moo::Image<bool>(width_, height_)` (cpp:380) — fresh
        // zero-filled allocation.
        self.image = BoolImage::new(header.width, header.height);

        // HoleHelper ctor stride (`cpp:36-37`): `(width/8) + ((width & 3) ?
        // 1 : 0)` — a quarter of the widths (%8 == 4) get a stride one
        // byte short of the writer's ceil(width/8), resampling the rows.
        // Ported as written.
        let src_stride = (header.width / 8) + u32::from(!header.width.is_multiple_of(4));
        let sz = src_stride.min(self.image.raw_row_size());
        if header.height > 0 && header.width > 0 {
            let needed = (header.height as usize - 1) * src_stride as usize + sz as usize;
            if payload.len() < needed {
                return Err(HoleLoadError::Truncated);
            }
            for z in 0..header.height {
                let src = &payload[z as usize * src_stride as usize..][..sz as usize];
                let dst = self.image.get_raw_row_mut(z);
                dst[..sz as usize].copy_from_slice(src);
            }
        }
        self.width = header.width;
        self.height = header.height;
        self.recalc_holes();
        Ok(())
    }

    /// `recalcHoles` (`terrain_hole_map2.cpp:556-591`): scan the whole
    /// image for the two flags, then drop a degenerate image (keeping the
    /// dimensions, like the reference `clear()`).
    fn recalc_holes(&mut self) {
        if self.image.is_empty() {
            self.all_holes = false;
            self.no_holes = true;
        } else {
            let (w, h) = (self.image.width(), self.image.height());
            let mut all_holes = true;
            let mut holes = false;
            for y in 0..h {
                for x in 0..w {
                    let v = self.image.get(x as i32, y as i32);
                    all_holes &= v;
                    holes |= v;
                }
            }
            self.no_holes = !holes;
            self.all_holes = all_holes;
            if self.no_holes || self.all_holes {
                self.image.clear();
            }
        }
    }

    /// `holeAt(x, z)` (`terrain_hole_map2.cpp:599-606`): block-local metres
    /// truncated (C cast) to a cell.
    pub fn hole_at(&self, x: f32, z: f32) -> bool {
        if self.all_holes {
            return true;
        }
        if self.no_holes {
            return false;
        }
        #[allow(clippy::cast_possible_truncation)]
        let cx = ((x / self.block_size()) * self.width() as f32) as i32;
        #[allow(clippy::cast_possible_truncation)]
        let cz = ((z / self.block_size()) * self.height() as f32) as i32;
        self.image.get(cx, cz)
    }

    /// `holeAt(xs, zs, xe, ze)` (`terrain_hole_map2.cpp:615-638`): true if
    /// the closed rect touches a hole, with floorf edges.
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    pub fn hole_at_rect(&self, xs: f32, zs: f32, xe: f32, ze: f32) -> bool {
        if self.all_holes {
            return true;
        }
        if self.no_holes {
            return false;
        }
        let x_start = ((xs / self.block_size()) * self.width() as f32).floor() as i32;
        let x_end = ((xe / self.block_size()) * self.width() as f32).floor() as i32;
        let z_start = ((zs / self.block_size()) * self.height() as f32).floor() as i32;
        let z_end = ((ze / self.block_size()) * self.height() as f32).floor() as i32;

        let mut z = z_start;
        while z <= z_end {
            let mut x = x_start;
            while x <= x_end {
                if self.image.get(x, z) {
                    return true;
                }
                x += 1;
            }
            z += 1;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn larger_pow2_matches_the_reference_bit_smear() {
        assert_eq!(larger_pow2(0), 1);
        assert_eq!(larger_pow2(1), 1);
        assert_eq!(larger_pow2(2), 2);
        assert_eq!(larger_pow2(3), 4);
        assert_eq!(larger_pow2(5), 8);
        assert_eq!(larger_pow2(10), 16);
        assert_eq!(larger_pow2(16), 16);
        assert_eq!(larger_pow2(17), 32);
        assert_eq!(larger_pow2(0x7fff_ffff), 0x8000_0000);
        assert_eq!(larger_pow2(0x8000_0000), 0x8000_0000);
        // Above MAX_POW_2 the reference returns the cap with a warning.
        assert_eq!(larger_pow2(0x8000_0001), 0x8000_0000);
        assert_eq!(larger_pow2(u32::MAX), 0x8000_0000);
    }

    #[test]
    fn create_reports_no_holes_with_pow2_texture_dims() {
        let mut map = TerrainHoleMap2::new(100.0);
        map.create(10);
        assert_eq!(map.width(), 10);
        assert_eq!(map.height(), 10);
        assert_eq!(map.holes_size(), 10);
        assert_eq!(map.holes_map_size(), 16);
        assert!(map.no_holes());
        assert!(!map.all_holes());
        // Every point answers no hole through the early-return arm.
        assert!(!map.hole_at(5.0, 5.0));
        assert!(!map.hole_at(-5.0, -5.0));
        assert!(!map.hole_at_rect(0.0, 0.0, 100.0, 100.0));
    }

    #[test]
    fn null_section_reports_the_reference_message() {
        let mut map = TerrainHoleMap2::new(100.0);
        assert_eq!(map.load(None), Err(HoleLoadError::NullDataSection));
        assert_eq!(
            HoleLoadError::NullDataSection.to_string(),
            "NULL DataSection"
        );
        assert_eq!(
            HoleLoadError::Truncated.to_string(),
            "hole map blob is truncated"
        );
        // The reset ran before the error: a previously loaded map is gone.
        map.create(10);
        let _ = map.load(None);
        assert_eq!(map.width(), 0);
        assert!(map.no_holes());
    }

    #[test]
    fn short_blobs_are_truncated() {
        let mut map = TerrainHoleMap2::new(100.0);
        assert_eq!(map.load(Some(&[0u8; 8])), Err(HoleLoadError::Truncated));
        // A full header plus too few rows: width 10 needs ceil(10/8)... the
        // reader stride is width/8 + (width&3 ? 1 : 0) = 2, two rows need
        // (2-1)*2 + 2 = 4 payload bytes; three do not.
        let header = HolesHeader {
            magic: HOLES_MAGIC,
            width: 10,
            height: 2,
            version: 1,
        };
        let mut blob = Vec::new();
        blob.extend_from_slice(&header.magic.to_le_bytes());
        blob.extend_from_slice(&header.width.to_le_bytes());
        blob.extend_from_slice(&header.height.to_le_bytes());
        blob.extend_from_slice(&header.version.to_le_bytes());
        blob.extend_from_slice(&[0u8; 3]);
        assert_eq!(map.load(Some(&blob)), Err(HoleLoadError::Truncated));
    }

    #[test]
    fn foreign_magic_panics_like_the_reference_assert() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&0x006c_6f69u32.to_le_bytes()); // "iol\0"
        blob.extend_from_slice(&10u32.to_le_bytes());
        blob.extend_from_slice(&10u32.to_le_bytes());
        blob.extend_from_slice(&1u32.to_le_bytes());
        blob.extend_from_slice(&[0u8; 4]);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut map = TerrainHoleMap2::new(100.0);
            let _ = map.load(Some(&blob));
        }));
        assert!(result.is_err(), "reference MF_ASSERTs the holes magic");
    }

    /// A packed 10x10 image with the fixture pattern as raw bytes:
    /// (0,0), (3,2), (4,2), (3,3), (7,5), (5,7), (9,9).
    fn mixed_blob() -> Vec<u8> {
        let mut image = BoolImage::new(10, 10);
        for (x, y, v) in [
            (0i32, 0i32, true),
            (3, 2, true),
            (4, 2, true),
            (3, 3, true),
            (7, 5, true),
            (5, 7, true),
            (9, 9, true),
        ] {
            image.set(x, y, v);
        }
        let mut blob = Vec::new();
        blob.extend_from_slice(&HOLES_MAGIC.to_le_bytes());
        blob.extend_from_slice(&10u32.to_le_bytes());
        blob.extend_from_slice(&10u32.to_le_bytes());
        blob.extend_from_slice(&1u32.to_le_bytes());
        blob.extend_from_slice(image.data.as_deref().expect("allocated"));
        blob
    }

    #[test]
    fn mixed_pattern_answers_point_and_rect_probes() {
        let mut map = TerrainHoleMap2::new(100.0);
        map.load(Some(&mixed_blob())).expect("load");
        assert_eq!(map.width(), 10);
        assert_eq!(map.height(), 10);
        assert_eq!(map.holes_size(), 10);
        assert_eq!(map.holes_map_size(), 16);
        assert!(!map.no_holes());
        assert!(!map.all_holes());

        // Cell centres: x = (cell + 0.5) m per cell, C truncation.
        assert!(map.hole_at(5.0, 5.0)); // (0,0)
        assert!(map.hole_at(35.0, 25.0)); // (3,2)
        assert!(map.hole_at(45.0, 25.0)); // (4,2)
        assert!(map.hole_at(35.0, 35.0)); // (3,3)
        assert!(map.hole_at(75.0, 55.0)); // (7,5)
        assert!(map.hole_at(55.0, 75.0)); // (5,7)
        assert!(map.hole_at(95.0, 95.0)); // (9,9)
        assert!(!map.hole_at(15.0, 5.0)); // (1,0)
        assert!(!map.hole_at(75.0, 25.0)); // (7,2)

        // Truncation toward zero on negatives: -0.5 truncates to cell 0.
        assert!(map.hole_at(-5.0, -5.0)); // clamps into the (0,0) hole
        assert!(!map.hole_at(50.0, -1.0)); // (5,0), not a hole
        // Out-of-range clamps: (105, 50) reads (9,5) — no hole there.
        assert!(!map.hole_at(105.0, 50.0));
        assert!(map.hole_at(101.0, 101.0)); // clamps into (9,9)
        assert!(map.hole_at(99.99, 99.99)); // 9.999 -> cell 9

        // Rect probes.
        assert!(map.hole_at_rect(30.0, 20.0, 49.0, 39.0));
        assert!(!map.hole_at_rect(50.0, 50.0, 59.0, 59.0));
        assert!(map.hole_at_rect(30.0, 20.0, 30.0, 20.0));
        assert!(!map.hole_at_rect(29.99, 20.0, 29.99, 39.0));
        assert!(map.hole_at_rect(70.0, 50.0, 79.0, 59.0));
        assert!(map.hole_at_rect(-50.0, -50.0, 200.0, 200.0));
    }

    #[test]
    fn degenerate_maps_round_trip_through_the_flags() {
        let mut map = TerrainHoleMap2::new(100.0);

        // No holes: after recalc the image is dropped but the flags answer.
        let mut none_blob = mixed_blob();
        for byte in &mut none_blob[16..] {
            *byte = 0;
        }
        map.load(Some(&none_blob)).expect("load none");
        assert!(map.no_holes());
        assert!(!map.all_holes());
        assert!(!map.hole_at(5.0, 5.0));
        assert!(!map.hole_at_rect(0.0, 0.0, 100.0, 100.0));
        // The lock face refills the degenerate image on demand.
        map.lock(true);
        assert!(!map.image().is_empty());
        assert!(!map.image().get(3, 2));
        map.unlock();

        // All holes: every real bit set, padding bits included.
        let mut all_blob = mixed_blob();
        for byte in &mut all_blob[16..] {
            *byte = 0xff;
        }
        map.load(Some(&all_blob)).expect("load all");
        assert!(!map.no_holes());
        assert!(map.all_holes());
        assert!(map.hole_at(15.0, 5.0));
        assert!(map.hole_at(-5.0, -5.0));
        assert!(map.hole_at_rect(29.99, 20.0, 29.99, 39.0));
    }

    #[test]
    fn the_load_stride_quirk_resamples_quarter_widths() {
        // width % 8 == 4: the reader stride (12/8 + (12&3 ? 1 : 0) = 1) is
        // one byte short of the writer's ceil(12/8) = 2, so the rows read
        // back resampled from the byte stream. Ported as written.
        let mut blob = Vec::new();
        blob.extend_from_slice(&HOLES_MAGIC.to_le_bytes());
        blob.extend_from_slice(&12u32.to_le_bytes());
        blob.extend_from_slice(&2u32.to_le_bytes());
        blob.extend_from_slice(&1u32.to_le_bytes());
        // A writer-stride layout: row 0 = [0b00000001, 0b00001111],
        // row 1 = [0b00000000, 0b00001110]. The reader picks bytes 0..1.
        blob.extend_from_slice(&[0b0000_0001, 0b0000_1111, 0b0000_0000, 0b0000_1110]);
        let mut map = TerrainHoleMap2::new(100.0);
        map.load(Some(&blob)).expect("load");
        // Image row 0 got byte 0 (bit 0 set), image row 1 got byte 1
        // (0x0f: bits 0-3 set). Cells: x scales by width 12, z by height 2.
        assert!(map.hole_at(4.0, 12.5)); // (0,0): bit 0 of 0x01
        assert!(!map.hole_at(12.0, 4.0)); // (1,0): bit 1 of 0x01 clear
        assert!(!map.hole_at(20.8, 12.5)); // (2,0): bit 2 of 0x01 clear
        assert!(map.hole_at(4.0, 75.0)); // (0,1): bit 0 of 0x0f
        assert!(map.hole_at(20.8, 75.0)); // (2,1): bit 2 of 0x0f
        assert!(!map.hole_at(45.0, 75.0)); // (5,1): bit 5 of 0x0f clear
    }

    #[test]
    fn edits_through_the_lock_face_recalculate_on_unlock() {
        let mut map = TerrainHoleMap2::new(100.0);
        map.create(10);
        map.lock(false);
        map.image_mut().set(3, 2, true);
        // Nested read-only locks are allowed on top of a write lock.
        assert!(map.lock(true));
        map.unlock();
        assert_eq!(map.lock_count, 1);
        map.unlock();
        // Mixed now: recalc kept the image and flipped the flags.
        assert!(!map.no_holes());
        assert!(!map.all_holes());
        assert!(map.hole_at(35.0, 25.0));

        // Undo it: back to the no-holes degenerate state.
        map.lock(false);
        map.image_mut().set(3, 2, false);
        map.unlock();
        assert!(map.no_holes());
        assert!(!map.hole_at(35.0, 25.0));

        // A read-only lock never recalculates on unlock.
        map.lock(true);
        map.unlock();
        assert!(map.no_holes());
    }

    #[test]
    fn nested_write_locks_panic_like_the_reference_assert() {
        let mut map = TerrainHoleMap2::new(100.0);
        map.create(10);
        map.lock(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            map.lock(false);
        }));
        assert!(result.is_err(), "reference MF_ASSERTs the nesting rule");
        // Stacking read-only locks is fine; the clean unlocks land back at
        // zero with the read-only flag restored.
        assert!(map.lock(true));
        assert!(map.unlock());
        assert!(map.unlock());
        assert_eq!(map.lock_count, 0);
        assert!(map.read_only);
    }

    #[test]
    fn zero_dim_headers_and_fresh_maps_reach_the_defensive_arms() {
        // A fresh map that was never created or loaded has no dimensions:
        // a write lock cannot refill the image, so unlock's recalc takes
        // the empty-image arm (all = false, none = true).
        let mut map = TerrainHoleMap2::new(100.0);
        assert!(map.lock(false));
        assert!(map.image().is_empty());
        assert!(map.unlock());
        assert!(map.no_holes());
        assert!(!map.all_holes());

        // A 0x0 header skips the row copy entirely; the reference `new
        // uint8[0]` still allocates, so the scan runs over nothing and
        // both flags end up true before the degenerate clear drops the
        // image.
        let mut blob = Vec::new();
        blob.extend_from_slice(&HOLES_MAGIC.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&1u32.to_le_bytes());
        map.load(Some(&blob)).expect("zero dims load");
        assert_eq!(map.width(), 0);
        assert_eq!(map.height(), 0);
        assert_eq!(map.holes_size(), 0);
        // largerPow2(0) == 1.
        assert_eq!(map.holes_map_size(), 1);
        assert!(map.no_holes());
        assert!(map.all_holes());
    }
}
