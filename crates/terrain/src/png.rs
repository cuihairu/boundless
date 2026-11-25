//! BW-10-03-01 minimal PNG reader for terrain payloads.
//!
//! The reference decodes terrain images through `Moo::decompressPNG`
//! (`lib/moo/png.cpp:324-395`): plain libpng row reads with no
//! transformations, rows in PNG order, `bpp_ = bit_depth * channels`. The
//! terrain faces only ever produce two shapes of image
//! (`lib/moo/png.cpp:261-267`, `lib/terrain/height_map_compress.cpp`):
//!
//! - 8-bit RGBA (`PNG_COLOR_TYPE_RGBA`, used by the quantised height map,
//!   `bpp_ == 32`),
//! - 16-bit grayscale (`PNG_COLOR_TYPE_GRAY`, used by the relative uint16
//!   height map, `bpp_ == 16`).
//!
//! This decoder supports exactly those two shapes, non-interlaced, with the
//! full filter set libpng's adaptive encoder may pick. Ancillary chunks
//! (`pHYs` and friends) are skipped like libpng skips them.

use std::io::{Read, Write as _};

/// Decoded image: rows in PNG order, `data.len() == width * height * bpp / 8`.
#[derive(Debug, Clone, PartialEq)]
pub struct PngImage {
    pub width: u32,
    pub height: u32,
    /// Bits per pixel, mirroring `PNGImageData::bpp_`.
    pub bpp: u32,
    pub data: Vec<u8>,
}

impl PngImage {
    /// Read a row of pixels as native-endian 32-bit words.
    ///
    /// The quantised height map stores one `int32` per RGBA pixel: the four
    /// channels wrap the little-endian bytes of the word
    /// (`height_map_compress.cpp:99-105` wraps the decoded bytes in
    /// `Moo::Image<int32>` directly).
    pub fn word(&self, x: u32, y: u32) -> i32 {
        debug_assert_eq!(self.bpp, 32);
        let off = (y * self.width + x) as usize * 4;
        i32::from_le_bytes([
            self.data[off],
            self.data[off + 1],
            self.data[off + 2],
            self.data[off + 3],
        ])
    }

    /// Read a row of pixels as 16-bit samples.
    ///
    /// Quirk: PNG keeps 16-bit samples big-endian on the wire, but the
    /// reference never calls `png_set_swap`, so on the little-endian host
    /// `Moo::Image<uint16>` reads the delivered bytes as native u16 —
    /// byte-swapped against the PNG spec. Both sides of the reference skip
    /// the swap, so the port composes little-endian pairs to stay
    /// bit-faithful to what the reference image actually holds.
    pub fn sample16(&self, x: u32, y: u32) -> u16 {
        debug_assert_eq!(self.bpp, 16);
        let off = (y * self.width + x) as usize * 2;
        u16::from_le_bytes([self.data[off], self.data[off + 1]])
    }

    /// Read one raw byte of the image (`data_[index]`), the generic face the
    /// terrain faces use when walking a row themselves (the texture blends
    /// walk `Image<uint8>` rows byte by byte through `getRow`).
    pub fn byte_at(&self, index: usize) -> u8 {
        self.data[index]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PngError {
    /// Bad signature, chunk walk or CRC.
    Corrupt(&'static str),
    /// Interlaced or otherwise unsupported image shape (the terrain faces
    /// never produce one; libpng would decode it but Moo never feeds it).
    Unsupported(&'static str),
    /// zlib wrapper or DEFLATE stream failure.
    Decompress,
}

impl core::fmt::Display for PngError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PngError::Corrupt(what) => write!(f, "corrupt png: {what}"),
            PngError::Unsupported(what) => write!(f, "unsupported png: {what}"),
            PngError::Decompress => write!(f, "png deflate stream failed"),
        }
    }
}

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// `PNG_COLOR_TYPE_GRAY` on the wire.
const COLOR_TYPE_GRAY: u8 = 0;
/// `PNG_COLOR_TYPE_RGBA` on the wire.
const COLOR_TYPE_RGBA: u8 = 6;

fn crc32(chunk_type: &[u8], data: &[u8]) -> u32 {
    let mut table = crc32fast::Hasher::new();
    table.update(chunk_type);
    table.update(data);
    table.finalize()
}

/// `Moo::decompressPNG` for the two terrain image shapes.
pub fn decompress_png(bytes: &[u8]) -> Result<PngImage, PngError> {
    if bytes.len() < 8 || bytes[..8] != SIGNATURE {
        return Err(PngError::Corrupt("signature"));
    }

    let mut width = 0u32;
    let mut height = 0u32;
    let mut bit_depth = 0u8;
    let mut color_type = 0u8;
    let mut ihdr_seen = false;
    let mut idat = Vec::new();
    let mut iend_seen = false;

    let mut pos = 8usize;
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let typ = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = match data_start.checked_add(len) {
            Some(end) if end + 4 <= bytes.len() => end,
            _ => return Err(PngError::Corrupt("chunk bounds")),
        };
        let data = &bytes[data_start..data_end];
        let crc = u32::from_be_bytes([
            bytes[data_end],
            bytes[data_end + 1],
            bytes[data_end + 2],
            bytes[data_end + 3],
        ]);
        if crc != crc32(typ, data) {
            return Err(PngError::Corrupt("chunk crc"));
        }

        match typ {
            b"IHDR" => {
                if len != 13 {
                    return Err(PngError::Corrupt("ihdr length"));
                }
                width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                bit_depth = data[8];
                color_type = data[9];
                if data[10] != 0 || data[11] != 0 {
                    return Err(PngError::Unsupported("compression or filter method"));
                }
                if data[12] != 0 {
                    return Err(PngError::Unsupported("interlace"));
                }
                ihdr_seen = true;
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => {
                iend_seen = true;
                break;
            }
            // Ancillary and unknown chunks are skipped, like libpng.
            _ => {}
        }
        pos = data_end + 4;
    }

    if !ihdr_seen || !iend_seen {
        return Err(PngError::Corrupt("missing IHDR or IEND"));
    }
    if width == 0 || height == 0 {
        return Err(PngError::Corrupt("empty image"));
    }

    // The shapes the terrain faces produce (`png.cpp:261-267` keeps
    // `PNG_COLOR_TYPE_GRAY` for anything that is not 32bpp; `compressImage`
    // stores 16-bit heights as gray16 and 8-bit texture blends as gray8).
    let channels: u8 = match (color_type, bit_depth) {
        (6, 8) => 4,  // RGBA8: quantised height map
        (0, 8) => 1,  // gray8: texture blends (`compressImage(Image<uint8>)`)
        (0, 16) => 1, // gray16: relative uint16 height map
        _ => return Err(PngError::Unsupported("colour type or bit depth")),
    };
    let bpp = bit_depth as u32 * channels as u32;
    let bytes_pp = bpp as usize / 8;
    let stride = width as usize * bytes_pp;
    let raw_len = stride
        .checked_mul(height as usize)
        .ok_or(PngError::Corrupt("image size overflow"))?;

    // Unfilter with the previous row kept adjacent, like png_read_row does.
    let filtered = inflate_zlib(&idat)?;
    if filtered.len() != raw_len + height as usize {
        return Err(PngError::Corrupt("decompressed size"));
    }
    let mut out = vec![0u8; raw_len];
    let mut prev: Vec<u8> = Vec::new();
    for y in 0..height as usize {
        let filter = filtered[y * (stride + 1)];
        let src = &filtered[y * (stride + 1) + 1..y * (stride + 1) + 1 + stride];
        let dst = &mut out[y * stride..(y + 1) * stride];
        unfilter_row(filter, src, &prev, dst, bytes_pp)?;
        prev.clear();
        prev.extend_from_slice(dst);
    }

    Ok(PngImage {
        width,
        height,
        bpp,
        data: out,
    })
}

/// `Moo::compressPNG` (`lib/moo/png.cpp:226-320`) for the shapes the
/// terrain editor saves: `bpp == 32` writes `PNG_COLOR_TYPE_RGBA` with
/// 8 bits per channel, everything else stays `PNG_COLOR_TYPE_GRAY` with
/// `bpp` bits per sample (`png.cpp:258-267`).
///
/// Rows are written with filter type 0 (`png_write_image` through libpng's
/// default adaptive selection picks per-row filters; any filter set is a
/// valid PNG and the readers accept all of them — registered divergence in
/// the compressed bytes, never in the decoded image). The compression level
/// mirrors `png_set_compression_level(pngPtr, 3)` (`png.cpp:289`). The
/// `pHYs` chunk mirrors the reference's `png_set_pHYs` with the same
/// unknown-resolution semantics.
pub fn compress_png(width: u32, height: u32, bpp: u32, data: &[u8]) -> Vec<u8> {
    assert!(bpp.is_power_of_two() && bpp <= 32, "reference asserts bpp");
    assert!(width > 0 && height > 0, "reference returns NULL on empty");
    let (bit_depth, color_type) = if bpp == 32 {
        (8u8, COLOR_TYPE_RGBA)
    } else {
        (bpp as u8, COLOR_TYPE_GRAY)
    };
    let bytes_pp = (bpp / 8) as usize;
    let stride = width as usize * bytes_pp;
    assert_eq!(data.len(), stride * height as usize, "image size mismatch");

    let mut out = Vec::new();
    out.extend_from_slice(&SIGNATURE);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);

    // `png_set_pHYs(pngPtr, pngInfo, width, height, PNG_RESOLUTION_UNKNOWN)`.
    let mut phys = Vec::with_capacity(9);
    phys.extend_from_slice(&width.to_be_bytes());
    phys.extend_from_slice(&height.to_be_bytes());
    phys.push(0); // unit: unknown
    write_chunk(&mut out, b"pHYs", &phys);

    // zlib level 3, filter type 0 rows, exactly like the decoder reads.
    let mut idat = Vec::new();
    let mut z = flate2::write::ZlibEncoder::new(&mut idat, flate2::Compression::new(3));
    for y in 0..height as usize {
        let _ = z.write_all(&[0u8]); // filter type 0
        let row = &data[y * stride..(y + 1) * stride];
        let _ = z.write_all(row);
    }
    drop(z);
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);

    out
}

fn write_chunk(out: &mut Vec<u8>, typ: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(typ, data).to_be_bytes());
}

fn inflate_zlib(idat: &[u8]) -> Result<Vec<u8>, PngError> {
    let mut out = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(idat);
    decoder
        .read_to_end(&mut out)
        .map_err(|_| PngError::Decompress)?;
    Ok(out)
}

fn unfilter_row(
    filter: u8,
    src: &[u8],
    prev: &[u8],
    dst: &mut [u8],
    bytes_pp: usize,
) -> Result<(), PngError> {
    match filter {
        0 => dst.copy_from_slice(src),
        1 => {
            for i in 0..dst.len() {
                let left = if i >= bytes_pp { dst[i - bytes_pp] } else { 0 };
                dst[i] = src[i].wrapping_add(left);
            }
        }
        2 => {
            for i in 0..dst.len() {
                dst[i] = src[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..dst.len() {
                let left = if i >= bytes_pp {
                    dst[i - bytes_pp] as u16
                } else {
                    0
                };
                let up = prev[i] as u16;
                dst[i] = src[i].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            for i in 0..dst.len() {
                let a = if i >= bytes_pp {
                    dst[i - bytes_pp] as i16
                } else {
                    0
                };
                let b = prev[i] as i16;
                let c = if i >= bytes_pp {
                    prev[i - bytes_pp] as i16
                } else {
                    0
                };
                let p = a + b - c;
                let pa = (p - a).abs();
                let pb = (p - b).abs();
                let pc = (p - c).abs();
                let pred = if pa <= pb && pa <= pc {
                    a
                } else if pb <= pc {
                    b
                } else {
                    c
                };
                dst[i] = src[i].wrapping_add(pred as u8);
            }
        }
        _ => return Err(PngError::Corrupt("row filter")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a valid single-IDAT PNG with libpng's writer configuration
    /// (RGBA8 or gray16, adaptive filter types per row) so the decoder is
    /// exercised on containers exactly like the reference produces.
    fn build_png(width: u32, height: u32, rgba8: bool, rows: &[Vec<u8>]) -> Vec<u8> {
        fn chunk(typ: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut c = Vec::new();
            c.extend_from_slice(&(data.len() as u32).to_be_bytes());
            c.extend_from_slice(typ);
            c.extend_from_slice(data);
            c.extend_from_slice(&crc32(typ, data).to_be_bytes());
            c
        }

        let mut png = Vec::new();
        png.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(if rgba8 { 8 } else { 16 });
        ihdr.push(if rgba8 { 6 } else { 0 });
        ihdr.push(0); // compression
        ihdr.push(0); // filter
        ihdr.push(0); // interlace
        png.extend_from_slice(&chunk(b"IHDR", &ihdr));

        // Encode each raw row with a different real filter method (0..=4
        // wrapping) so the decoder must reconstruct all of them. Row y uses
        // method y%5 against the previously encoded *decoded* row.
        let stride = rows[0].len();
        let bytes_pp = if rgba8 { 4 } else { 2 };
        let mut filtered = Vec::new();
        let mut prev: Vec<u8> = vec![0; stride];
        for (y, raw) in rows.iter().enumerate() {
            let f = (y % 5) as u8;
            filtered.push(f);
            for i in 0..stride {
                let x = raw[i] as i16;
                let a = if i >= bytes_pp {
                    raw[i - bytes_pp] as i16
                } else {
                    0
                };
                let b = prev[i] as i16;
                let c = if i >= bytes_pp {
                    prev[i - bytes_pp] as i16
                } else {
                    0
                };
                let p = a + b - c;
                let pa = (p - a).abs();
                let pb = (p - b).abs();
                let pc = (p - c).abs();
                let pred = match f {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => (a + b) / 2,
                    4 => {
                        if pa <= pb && pa <= pc {
                            a
                        } else if pb <= pc {
                            b
                        } else {
                            c
                        }
                    }
                    _ => unreachable!(),
                };
                filtered.push((x - pred) as u8);
            }
            // prev must track the decoded output, which equals the raw row.
            prev.copy_from_slice(raw);
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&filtered).unwrap();
        png.extend_from_slice(&chunk(b"IDAT", &z.finish().unwrap()));
        png.extend_from_slice(&chunk(b"IEND", &[]));

        // The filtered buffer must be exactly one filter byte per row.
        assert_eq!(filtered.len(), height as usize * (stride + 1));
        png
    }

    #[test]
    fn decodes_rgba8_through_all_filters() {
        // Deterministic pixel data: one int32 word per pixel.
        let (w, h) = (7u32, 5u32);
        let mut rows = vec![vec![0u8; w as usize * 4]; h as usize];
        let mut words = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let q = x as i32 * 1000 - y as i32 * 500;
                words.push(q);
                let b = q.to_le_bytes();
                rows[y as usize][x as usize * 4..x as usize * 4 + 4].copy_from_slice(&b);
            }
        }
        let png = build_png(w, h, true, &rows);
        let img = decompress_png(&png).expect("decode");
        assert_eq!(img.width, w);
        assert_eq!(img.height, h);
        assert_eq!(img.bpp, 32);
        for (i, q) in words.iter().enumerate() {
            let x = i as u32 % w;
            let y = i as u32 / w;
            assert_eq!(img.word(x, y), *q, "pixel ({x},{y})");
        }
    }

    #[test]
    fn decodes_gray16_with_the_little_endian_quirk() {
        // Wire bytes are big-endian per PNG; the reference image holds the
        // little-endian composition, so 0x1234 on the wire reads as 0x3412.
        let (w, h) = (3u32, 2u32);
        let mut rows = vec![vec![0u8; w as usize * 2]; h as usize];
        for y in 0..h {
            for x in 0..w {
                let v = 0x1234u16.wrapping_add(0x111 * x as u16);
                rows[y as usize][x as usize * 2] = (v >> 8) as u8;
                rows[y as usize][x as usize * 2 + 1] = (v & 0xff) as u8;
            }
        }
        let png = build_png(w, h, false, &rows);
        let img = decompress_png(&png).expect("decode");
        assert_eq!(img.bpp, 16);
        for y in 0..h {
            for x in 0..w {
                let wire = u16::from_be_bytes([
                    rows[y as usize][x as usize * 2],
                    rows[y as usize][x as usize * 2 + 1],
                ]);
                // The reference reads wire bytes as native LE.
                assert_eq!(img.sample16(x, y), wire.swap_bytes());
            }
        }
    }

    #[test]
    fn rejects_corrupt_containers() {
        assert!(matches!(
            decompress_png(b"not a png"),
            Err(PngError::Corrupt("signature"))
        ));

        let (w, h) = (2u32, 2u32);
        let rows = vec![vec![0u8; w as usize * 4]; h as usize];
        let mut png = build_png(w, h, true, &rows);
        // Flip a bit in the IHDR CRC (byte 29..33 after the signature and
        // length+type).
        let last = png.len() - 1;
        png[last] ^= 0xff;
        assert!(matches!(decompress_png(&png), Err(PngError::Corrupt(_))));
    }

    /// One wire-format chunk: length, type, payload, CRC.
    fn chunk(typ: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(&(data.len() as u32).to_be_bytes());
        c.extend_from_slice(typ);
        c.extend_from_slice(data);
        c.extend_from_slice(&crc32(typ, data).to_be_bytes());
        c
    }

    /// Hand-write a one-chunk PNG whose IHDR carries `fields` after the
    /// dimensions, with an optional trailing chunk.
    fn raw_png(ihdr_rest: [u8; 5], tail: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
        let mut png = Vec::new();
        png.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&ihdr_rest);
        png.extend_from_slice(&chunk(b"IHDR", &ihdr));
        if let Some((typ, data)) = tail {
            png.extend_from_slice(&chunk(typ, data));
        }
        png
    }

    /// zlib-compress a filtered row stream and frame it as the only IDAT of
    /// a complete 2x2 RGBA container (IEND included).
    fn png_with_filtered_rows(filtered: &[u8]) -> Vec<u8> {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        z.write_all(filtered).unwrap();
        let mut png = raw_png([8, 6, 0, 0, 0], Some((b"IDAT", &z.finish().unwrap())));
        png.extend_from_slice(&chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn displays_errors_like_the_variants_name_them() {
        assert_eq!(
            PngError::Corrupt("signature").to_string(),
            "corrupt png: signature"
        );
        assert_eq!(
            PngError::Unsupported("interlace").to_string(),
            "unsupported png: interlace"
        );
        assert_eq!(
            PngError::Decompress.to_string(),
            "png deflate stream failed"
        );
    }

    #[test]
    fn the_chunk_walk_rejects_every_defect_class() {
        // A chunk whose declared length walks past the buffer.
        let mut png = raw_png([8, 6, 0, 0, 0], None);
        png[8..12].copy_from_slice(&u32::MAX.to_be_bytes()); // IHDR len
        png[8 + 4..8 + 4 + 4].copy_from_slice(b"IHDR");
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Corrupt("chunk bounds"))
        ));

        // IHDR payload not 13 bytes: a valid-CRC chunk typed IHDR whose
        // payload is only 5 bytes (the CRC is checked first, so the patched
        // length alone would trip "chunk crc" instead).
        let png = {
            let mut p = Vec::new();
            p.extend_from_slice(&SIGNATURE);
            p.extend_from_slice(&chunk(b"IHDR", &[8, 6, 0, 0, 0]));
            p
        };
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Corrupt("ihdr length"))
        ));

        // Non-zero compression method (IHDR byte 10).
        let png = raw_png([8, 6, 1, 0, 0], None);
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Unsupported("compression or filter method"))
        ));

        // Interlaced (IHDR byte 12).
        let png = raw_png([8, 6, 0, 0, 1], None);
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Unsupported("interlace"))
        ));

        // No IEND after the IHDR.
        let png = raw_png([8, 6, 0, 0, 0], None);
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Corrupt("missing IHDR or IEND"))
        ));

        // Zero dimensions (IHDR height = 0), with an IEND present.
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&0u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut ihdr_chunk = Vec::new();
        ihdr_chunk.extend_from_slice(&(ihdr.len() as u32).to_be_bytes());
        ihdr_chunk.extend_from_slice(b"IHDR");
        ihdr_chunk.extend_from_slice(&ihdr);
        ihdr_chunk.extend_from_slice(&crc32(b"IHDR", &ihdr).to_be_bytes());
        let mut iend = Vec::new();
        iend.extend_from_slice(&0u32.to_be_bytes());
        iend.extend_from_slice(b"IEND");
        iend.extend_from_slice(&crc32(b"IEND", &[]).to_be_bytes());
        let mut png = Vec::new();
        png.extend_from_slice(&SIGNATURE);
        png.extend_from_slice(&ihdr_chunk);
        png.extend_from_slice(&iend);
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Corrupt("empty image"))
        ));

        // An unsupported colour type: truecolour 8-bit, in a complete
        // container (the missing-IEND check runs before the shape check).
        let png = raw_png([8, 2, 0, 0, 0], Some((b"IEND", b"")));
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Unsupported("colour type or bit depth"))
        ));

        // A complete container whose IDAT is not a zlib stream.
        let mut png = raw_png([8, 6, 0, 0, 0], Some((b"IDAT", b"not deflate")));
        png.extend_from_slice(&chunk(b"IEND", &[]));
        assert!(matches!(decompress_png(&png), Err(PngError::Decompress)));
    }

    #[test]
    fn the_size_check_and_bad_row_filter_are_reported() {
        // Valid IHDR but a filtered stream sized for fewer rows: patch a
        // real container's height so the zlib payload no longer matches.
        let (w, h) = (2u32, 2u32);
        let rows = vec![vec![0u8; w as usize * 4]; h as usize];
        let mut png = build_png(w, h, true, &rows);
        // Double the declared height (rebuild IHDR chunk in place: same
        // length, recompute CRC over the patched payload).
        let payload = &mut png[16..16 + 13];
        payload[4..8].copy_from_slice(&4u32.to_be_bytes());
        let crc = crc32(b"IHDR", payload).to_be_bytes();
        png[16 + 13..16 + 17].copy_from_slice(&crc);
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Corrupt("decompressed size"))
        ));

        // A filter byte nobody defines (5) in row 0: the filter lives inside
        // the compressed stream, so build the filtered rows by hand.
        let stride = w as usize * 4;
        let mut filtered = Vec::new();
        filtered.push(5u8); // row 0: invalid filter
        filtered.resize(filtered.len() + stride, 0);
        filtered.push(0); // row 1: none
        filtered.resize(filtered.len() + stride, 0);
        let png = png_with_filtered_rows(&filtered);
        assert!(matches!(
            decompress_png(&png),
            Err(PngError::Corrupt("row filter"))
        ));
    }

    #[test]
    fn paeth_predictor_picks_each_neighbour() {
        // Row 0 = [0,0,0,0, 151,0,0,0]. Row 1 byte 4: a = 150 (decoded
        // left), b = 151 (up), c = 0 (upper-left) -> p = 301 and pb = 150
        // is strictly the smallest distance, so the b predictor wins.
        let stream = [
            0, 0, 0, 0, 0, 151, 0, 0, 0, // row 0: none
            4, 150, 0, 0, 0, 105, 0, 0, 0, // row 1: paeth
        ];
        let img = decompress_png(&png_with_filtered_rows(&stream)).expect("decode");
        assert_eq!(
            img.data,
            [[0, 0, 0, 0, 151, 0, 0, 0], [150, 0, 0, 0, 0, 0, 0, 0]].concat()
        );

        // Row 0 = [150,0,0,0, 200,0,0,0]. Row 1 byte 4: a = 100, b = 200,
        // c = 150 -> p = 150 and pc = 0 is strictly the smallest distance,
        // so the c predictor wins.
        let stream = [
            0, 150, 0, 0, 0, 200, 0, 0, 0, // row 0: none
            4, 206, 0, 0, 0, 106, 0, 0, 0, // row 1: paeth
        ];
        let img = decompress_png(&png_with_filtered_rows(&stream)).expect("decode");
        assert_eq!(
            img.data,
            [[150, 0, 0, 0, 200, 0, 0, 0], [100, 0, 0, 0, 0, 0, 0, 0]].concat()
        );
    }
}
