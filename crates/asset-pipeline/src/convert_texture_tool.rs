//! `Moo::ConvertTextureTool` port (`lib/moo/convert_texture_tool.*`): the
//! pixel-work seam behind the texture converter.
//!
//! The reference `convert` hands the source to `TextureCompressor`, which
//! needs an initialised D3D device (`initMoo` registers a window class,
//! creates a `Renderer` and creates the device) — not portable. The port
//! keeps the tool as a [`TextureTool`] face and ships a device-free face,
//! [`TgaDdsTool`]: it decodes an uncompressed truecolor TGA (24/32 bpp) and
//! writes an uncompressed A8R8G8B8 DDS. Block compression (DXT) and the
//! mipmap/rescale work driven by the detail level (`maxExtent`, `mipCount`)
//! stay behind the seam; the face writes the same uncompressed DDS for the
//! compressed pass, so the converter shell's `.c.dds` plumbing is exercised
//! end to end.

use std::rc::Rc;

use resources::bwresource::BwResource;

use crate::texture_detail_level::TextureDetailLevel;

/// The tool seam (`ConvertTextureTool::convert`): read the source texture,
/// write the converted DDS.
pub trait TextureTool {
    fn convert(
        &self,
        src_name: &str,
        dest_name: &str,
        lod: &TextureDetailLevel,
        compression: bool,
    ) -> bool;
}

/// `DDS_HEADER` constants for an uncompressed A8R8G8B8 surface.
const DDS_MAGIC: u32 = 0x20534444; // "DDS "
const DDSD_CAPS: u32 = 0x1;
const DDSD_HEIGHT: u32 = 0x2;
const DDSD_WIDTH: u32 = 0x4;
const DDSD_PITCH: u32 = 0x8;
const DDSD_PIXELFORMAT: u32 = 0x1000;
const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_RGB: u32 = 0x40;
const DDSCAPS_TEXTURE: u32 = 0x1000;

/// A decoded TGA image: RGBA bytes in the file's bottom-up row order
/// (top-first sources are flipped at decode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgaImage {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

/// `decode_tga`: the uncompressed truecolor face of the TGA format
/// (image type 2, 24/32 bpp, no colour map). Everything else — RLE,
/// greyscale, colour-mapped, 16 bpp — returns `None`.
pub fn decode_tga(raw: &[u8]) -> Option<TgaImage> {
    if raw.len() < 18 {
        return None;
    }
    let id_length = raw[0] as usize;
    let color_map_type = raw[1];
    let image_type = raw[2];
    let width = u16::from_le_bytes([raw[12], raw[13]]);
    let height = u16::from_le_bytes([raw[14], raw[15]]);
    let depth = raw[16];
    let descriptor = raw[17];

    if color_map_type != 0 || image_type != 2 || (depth != 24 && depth != 32) {
        return None;
    }

    let bytes_pp = depth as usize / 8;
    let start = 18 + id_length;
    let pixels = width as usize * height as usize * bytes_pp;
    if raw.len() < start + pixels {
        return None;
    }
    let data = &raw[start..start + pixels];

    // BGR(A) byte order → RGBA.
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for px in data.chunks_exact(bytes_pp) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], px.get(3).copied().unwrap_or(0xFF)]);
    }

    let mut image = TgaImage {
        width,
        height,
        rgba,
    };
    // Descriptor bit 5: origin top-left (rows stored top first). The DDS
    // face keeps bottom-up order, so flip them now.
    if descriptor & 0x20 != 0 {
        flip_rows(&mut image.rgba, width as usize);
    }
    Some(image)
}

/// Reverse the row order of an RGBA buffer in place.
fn flip_rows(rgba: &mut [u8], width: usize) {
    let stride = width * 4;
    let rows = rgba.len() / stride;
    for row in 0..rows / 2 {
        let top = row * stride;
        let bottom = (rows - 1 - row) * stride;
        for i in 0..stride {
            rgba.swap(top + i, bottom + i);
        }
    }
}

/// A decoded DDS image: RGBA bytes in bottom-up row order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdsImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// `encode_dds`: wrap RGBA pixels (bottom-up) in an uncompressed A8R8G8B8
/// DDS — header then raw rows, no mips.
pub fn encode_dds(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 + rgba.len());
    out.extend_from_slice(&DDS_MAGIC.to_le_bytes());
    out.extend_from_slice(&124u32.to_le_bytes());
    let flags = DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PITCH | DDSD_PIXELFORMAT;
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&(width * 4).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // depth
    out.extend_from_slice(&0u32.to_le_bytes()); // mipMapCount
    out.extend_from_slice(&[0u8; 44]); // 11 reserved words
    out.extend_from_slice(&32u32.to_le_bytes()); // pixel format size
    out.extend_from_slice(&(DDPF_ALPHAPIXELS | DDPF_RGB).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // fourCC
    out.extend_from_slice(&32u32.to_le_bytes()); // RGBBitCount
    out.extend_from_slice(&0x00FF0000u32.to_le_bytes()); // R mask
    out.extend_from_slice(&0x0000FF00u32.to_le_bytes()); // G mask
    out.extend_from_slice(&0x000000FFu32.to_le_bytes()); // B mask
    out.extend_from_slice(&0xFF000000u32.to_le_bytes()); // A mask
    out.extend_from_slice(&DDSCAPS_TEXTURE.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // caps2..caps4 + reserved2
    out.extend_from_slice(rgba);
    out
}

/// `decode_dds`: read back what [`encode_dds`] wrote (uncompressed 32 bpp).
pub fn decode_dds(raw: &[u8]) -> Option<DdsImage> {
    let u32_at = |at: usize| -> Option<u32> {
        raw.get(at..at + 4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
    };
    if raw.len() < 128 || u32_at(0)? != DDS_MAGIC {
        return None;
    }
    let pixel_flags = u32_at(80)?;
    let bit_count = u32_at(88)?;
    if pixel_flags & DDPF_RGB == 0 || bit_count != 32 {
        return None;
    }
    let height = u32_at(12)?;
    let width = u32_at(16)?;
    let stride = width as usize * 4;
    let pixels = height as usize * stride;
    if raw.len() < 128 + pixels {
        return None;
    }
    Some(DdsImage {
        width,
        height,
        rgba: raw[128..128 + pixels].to_vec(),
    })
}

/// The device-free [`TextureTool`] face: TGA source in, uncompressed DDS
/// out.
pub struct TgaDdsTool {
    resource: Rc<BwResource>,
}

impl TgaDdsTool {
    pub fn new(resource: Rc<BwResource>) -> Self {
        Self { resource }
    }
}

impl TextureTool for TgaDdsTool {
    /// The reference flow — `TextureCompressor(srcName).compressToMemory()`
    /// then `writeFile` — with the compressor behind the seam.
    fn convert(
        &self,
        src_name: &str,
        dest_name: &str,
        _lod: &TextureDetailLevel,
        _compression: bool,
    ) -> bool {
        let Some(raw) = self.resource.file_system().read_file(src_name) else {
            eprintln!("Could not convert texture {src_name}: could not read source.");
            return false;
        };
        let Some(image) = decode_tga(&raw) else {
            eprintln!("Could not convert texture {src_name}: unsupported TGA variant.");
            return false;
        };
        let dds = encode_dds(image.width as u32, image.height as u32, &image.rgba);
        if !self.resource.file_system().write_file(dest_name, &dds) {
            eprintln!("Failed to convert {src_name} to {dest_name}.");
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("textool-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &std::path::Path, contents: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A 2x2 uncompressed truecolor TGA. `top_first` controls descriptor
    /// bit 5; `bytes_pp` picks 24 or 32 bpp.
    fn make_tga(width: u16, height: u16, bytes_pp: u8, top_first: bool, id: &str) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.push(id.len() as u8);
        raw.push(0); // colour map type
        raw.push(2); // uncompressed truecolor
        raw.extend_from_slice(&[0; 5]); // colour map spec
        raw.extend_from_slice(&0u16.to_le_bytes()); // x origin
        raw.extend_from_slice(&0u16.to_le_bytes()); // y origin
        raw.extend_from_slice(&width.to_le_bytes());
        raw.extend_from_slice(&height.to_le_bytes());
        raw.push(bytes_pp * 8); // pixel depth is bits
        raw.push(if top_first { 0x20 } else { 0x00 });
        raw.extend_from_slice(id.as_bytes());
        // Row 0 is the bottom row for bottom-first sources.
        for row in 0..height {
            for _col in 0..width {
                let shade = (row * 16 + 8) as u8;
                raw.push(shade); // B
                raw.push(shade + 1); // G
                raw.push(shade + 2); // R
                if bytes_pp == 4 {
                    raw.push(0x80); // A
                }
            }
        }
        raw
    }

    /// TGA decode: BGR(A) → RGBA, 24 bpp gains opaque alpha, top-first
    /// sources come out flipped to bottom-up.
    #[test]
    fn tga_decode_reads_truecolor_pixels() {
        // 32 bpp, bottom-up: row 0 in the file is the bottom row.
        let image = decode_tga(&make_tga(2, 2, 4, false, "id")).unwrap();
        assert_eq!(image.width, 2);
        assert_eq!(image.height, 2);
        assert_eq!(image.rgba.len(), 16);
        let px = |row: usize, col: usize| &image.rgba[(row * 2 + col) * 4..(row * 2 + col) * 4 + 4];
        assert_eq!(px(0, 0), &[10, 9, 8, 0x80]);
        assert_eq!(px(1, 1), &[26, 25, 24, 0x80]);

        // 24 bpp: alpha is forced opaque.
        let image = decode_tga(&make_tga(1, 1, 3, false, "")).unwrap();
        assert_eq!(&image.rgba[..], &[10, 9, 8, 0xFF]);

        // Top-first: rows arrive flipped, so bottom-up row 0 is the file's
        // last row.
        let image = decode_tga(&make_tga(1, 2, 4, true, "")).unwrap();
        assert_eq!(&image.rgba[..4], &[26, 25, 24, 0x80]);
        assert_eq!(&image.rgba[4..], &[10, 9, 8, 0x80]);
    }

    /// Colour-mapped, RLE, greyscale, 16 bpp and truncated sources are
    /// refused.
    #[test]
    fn tga_decode_rejects_the_unportable_variants() {
        let mut raw = make_tga(1, 1, 4, false, "");
        raw[1] = 1; // colour map type
        assert!(decode_tga(&raw).is_none());

        let mut raw = make_tga(1, 1, 4, false, "");
        raw[2] = 10; // RLE truecolor
        assert!(decode_tga(&raw).is_none());

        let mut raw = make_tga(1, 1, 4, false, "");
        raw[2] = 3; // uncompressed greyscale
        assert!(decode_tga(&raw).is_none());

        let mut raw = make_tga(1, 1, 4, false, "");
        raw[16] = 16; // 16 bpp
        assert!(decode_tga(&raw).is_none());

        let mut raw = make_tga(2, 2, 4, false, "");
        raw.truncate(raw.len() - 1); // one byte short of the pixels
        assert!(decode_tga(&raw).is_none());

        assert!(decode_tga(&[0u8; 4]).is_none());
    }

    /// The DDS layout: exact header fields, bottom-up rows, and the reader
    /// round-trips it.
    #[test]
    fn dds_round_trips_through_the_reader() {
        let rgba: Vec<u8> = (0..4 * 2 * 4).map(|i| (i % 251) as u8).collect();
        let dds = encode_dds(4, 2, &rgba);

        let u32_at = |at: usize| u32::from_le_bytes(dds[at..at + 4].try_into().unwrap());
        assert_eq!(u32_at(0), 0x20534444);
        assert_eq!(u32_at(4), 124);
        assert_eq!(
            u32_at(8),
            DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PITCH | DDSD_PIXELFORMAT
        );
        assert_eq!(u32_at(12), 2); // height
        assert_eq!(u32_at(16), 4); // width
        assert_eq!(u32_at(20), 16); // pitch
        assert_eq!(u32_at(80), DDPF_ALPHAPIXELS | DDPF_RGB);
        assert_eq!(u32_at(84), 0); // fourCC
        assert_eq!(u32_at(88), 32); // bit count
        assert_eq!(u32_at(92), 0x00FF0000); // R mask
        assert_eq!(u32_at(108), DDSCAPS_TEXTURE);
        assert_eq!(dds.len(), 128 + rgba.len());
        // Rows pass through in the bottom-up order they were given.
        assert_eq!(&dds[128..128 + 16], &rgba[..16]);

        let image = decode_dds(&dds).unwrap();
        assert_eq!(image.width, 4);
        assert_eq!(image.height, 2);
        assert_eq!(image.rgba, rgba);

        // A colour-mapped or non-32bpp header is refused.
        let mut raw = dds.clone();
        raw[88..92].copy_from_slice(&8u32.to_le_bytes());
        assert!(decode_dds(&raw).is_none());
        let mut raw = dds;
        raw[80..84].copy_from_slice(&DDPF_ALPHAPIXELS.to_le_bytes());
        assert!(decode_dds(&raw).is_none());
    }

    /// The tool face: TGA in the resource tree, DDS out, pixels unchanged.
    #[test]
    fn tool_convert_round_trips_tga_to_dds() {
        let dir = temp_root("convert");
        let source = make_tga(3, 2, 4, false, "");
        write_file(&dir.join("wall.tga"), &source);

        let mut resource = resources::bwresource::BwResource::init_paths(&[dir.to_str().unwrap()]);
        resource.enable_editor_default_root();
        let resource = Rc::new(resource);

        let tool = TgaDdsTool::new(Rc::clone(&resource));
        assert!(tool.convert(
            "wall.tga",
            "wall.dds",
            &TextureDetailLevel::default(),
            false
        ));

        let dds = resource.file_system().read_file("wall.dds").unwrap();
        let image = decode_dds(&dds).unwrap();
        let source = decode_tga(&source).unwrap();
        assert_eq!(image.width, 3);
        assert_eq!(image.height, 2);
        assert_eq!(image.rgba, source.rgba);

        // A missing source fails cleanly.
        assert!(!tool.convert(
            "gone.tga",
            "gone.dds",
            &TextureDetailLevel::default(),
            false
        ));
    }
}
