//! The texture face: `TextureManager::get` and the decode that feeds it
//! (BW-11-04-02).
//!
//! Reference sources: the manager cache and lookup chain are
//! `TextureManager::get` (`lib/moo/texture_manager.cpp:389-477`) with
//! `getUnique` (:589-666); the name normalisation is
//! `TextureHelpers::copyNormalisedTextureName`
//! (`lib/moo/texture_helpers.cpp:16-40`) and `removeTextureExtension` /
//! `canonicalTextureName` (`lib/moo/base_texture.cpp:241-263`); the failure
//! policy is `BaseTexture::processLoaderOutput`
//! (`lib/moo/base_texture.cpp:140-188`) with the lazy
//! `ManagedTexture::load` (`lib/moo/managed_texture.cpp:414-447`).
//!
//! The reference hands file bytes to D3DX (`D3DXCreateTextureFromFile*`,
//! wrapped at `render_context.cpp:2558`). This port models the decode for
//! the two lossless formats the reference's own test assets use —
//! uncompressed BMP (`BI_RGB`, 24/32bpp) and uncompressed true-colour TGA —
//! and routes every other format through the reference's not-found path,
//! which is exactly what an undecodable file takes there. Level-0 pixels
//! are kept as `0xAABBGGRR` in visual top-row-first order: D3DX flips
//! bottom-up sources during load, so texture coordinate (0, 0) addresses
//! the visual top-left. Mip-chain generation and the streaming/LOD stack
//! are D3D-specific faces outside this item's binding scope.

use std::collections::BTreeMap;
use std::rc::Rc;

/// The texture file extensions the manager recognises, in the reference's
/// order (`texture_helpers.cpp:9-13`) — "dds needs to be the first".
pub const TEXTURE_EXTS: [&str; 9] = [
    "dds", "bmp", "tga", "jpg", "png", "hdr", "pfm", "dib", "ppm",
];

/// The D3D format of a loaded texture's level 0. Only the lossless formats
/// this port decodes are named; everything else stays `Unknown`, which is
/// what a failed load leaves (`ManagedTexture::processLoaderOutput`,
/// managed_texture.cpp:188-238).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    /// `D3DFMT_UNKNOWN`.
    Unknown,
    /// `D3DFMT_R8G8B8`, the 24bpp BMP layout.
    R8g8b8,
    /// `D3DFMT_A8R8G8B8`, the 32bpp BMP/TGA layout.
    A8r8g8b8,
}

/// `BaseTexture::Status` (`base_texture.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureStatus {
    NotReady,
    Loading,
    Ready,
    FailedToLoad,
}

/// A loaded texture: the ported `BaseTexture`/`ManagedTexture` level-0 face.
#[derive(Debug, Clone, PartialEq)]
pub struct Texture {
    /// The canonical resource id: `canonicalTextureName` strips a `.c`
    /// between the base and the real extension (base_texture.cpp:252-263).
    pub resource_id: String,
    /// Level-0 width in pixels.
    pub width: u32,
    /// Level-0 height in pixels.
    pub height: u32,
    /// Level-0 format.
    pub format: TextureFormat,
    /// Level-0 pixels as `0xAABBGGRR`, visual top row first.
    pub pixels: Vec<u32>,
    /// Load status.
    pub status: TextureStatus,
    /// Whether this is the manager's `AnimatingTexture` placeholder: the
    /// reference creates one when a `.texanim` file exists and defers frame
    /// loading to its feeder (`texture_manager.cpp:601-616`), so the record
    /// surfaces before any frame is decoded.
    pub animating: bool,
}

impl Texture {
    fn not_ready(resource_id: String, animating: bool) -> Self {
        Self {
            resource_id,
            width: 0,
            height: 0,
            format: TextureFormat::Unknown,
            pixels: Vec::new(),
            status: TextureStatus::NotReady,
            animating,
        }
    }
}

/// Decode an uncompressed BMP (`BI_RGB`, 24 or 32bpp). Rows are flipped to
/// visual top-first, matching the D3DX orientation handling. Anything else
/// — compressed formats, palettes, exotic bpp — fails, and the load takes
/// the same not-found path a corrupt file takes in the reference.
pub fn decode_bmp(raw: &[u8]) -> Option<(u32, u32, TextureFormat, Vec<u32>)> {
    if raw.len() < 34 || &raw[0..2] != b"BM" {
        return None;
    }
    let offset = u32::from_le_bytes(raw[10..14].try_into().ok()?) as usize;
    let header = u32::from_le_bytes(raw[14..18].try_into().ok()?);
    if header < 40 {
        return None;
    }
    let width = i32::from_le_bytes(raw[18..22].try_into().ok()?);
    let height = i32::from_le_bytes(raw[22..26].try_into().ok()?);
    let bpp = u16::from_le_bytes(raw[28..30].try_into().ok()?);
    let compression = u32::from_le_bytes(raw[30..34].try_into().ok()?);
    if compression != 0 || width <= 0 || !(bpp == 24 || bpp == 32) {
        return None;
    }
    let bytes_per_pixel = (bpp / 8) as usize;
    let stride = (width as usize * bytes_per_pixel).div_ceil(4) * 4;
    let height = height as usize;
    if raw.len() < offset + stride * height {
        return None;
    }
    let mut rows = Vec::with_capacity(height);
    for y in 0..height {
        // Bottom-up storage: file row 0 is the visual bottom scanline.
        let base = offset + y * stride;
        let mut row = Vec::with_capacity(width as usize);
        for x in 0..width as usize {
            let px = base + x * bytes_per_pixel;
            let (b, g, r) = (raw[px], raw[px + 1], raw[px + 2]);
            let a = if bytes_per_pixel == 4 {
                raw[px + 3]
            } else {
                0xFF
            };
            row.push(
                (u32::from(a) << 24) | (u32::from(b) << 16) | (u32::from(g) << 8) | u32::from(r),
            );
        }
        rows.push(row);
    }
    rows.reverse();
    let format = if bpp == 24 {
        TextureFormat::R8g8b8
    } else {
        TextureFormat::A8r8g8b8
    };
    Some((
        width as u32,
        height as u32,
        format,
        rows.into_iter().flatten().collect(),
    ))
}

/// Decode an uncompressed true-colour TGA (image type 2, no colour map,
/// 24 or 32bpp). Rows are flipped to visual top-first unless the descriptor
/// bit 5 marks the file top-down. RLE and other types fail into the
/// not-found path.
pub fn decode_tga(raw: &[u8]) -> Option<(u32, u32, TextureFormat, Vec<u32>)> {
    if raw.len() < 18 {
        return None;
    }
    let id_len = raw[0] as usize;
    if raw[1] != 0 || raw[2] != 2 {
        return None;
    }
    let width = u16::from_le_bytes(raw[12..14].try_into().ok()?) as usize;
    let height = u16::from_le_bytes(raw[14..16].try_into().ok()?) as usize;
    let bpp = raw[16] as usize;
    let descriptor = raw[17];
    if !(bpp == 24 || bpp == 32) || width == 0 || height == 0 {
        return None;
    }
    let bytes_per_pixel = bpp / 8;
    let offset = 18 + id_len;
    let stride = width * bytes_per_pixel;
    if raw.len() < offset + stride * height {
        return None;
    }
    let mut rows = Vec::with_capacity(height);
    for y in 0..height {
        let base = offset + y * stride;
        let mut row = Vec::with_capacity(width);
        for x in 0..width {
            let px = base + x * bytes_per_pixel;
            let (b, g, r) = (raw[px], raw[px + 1], raw[px + 2]);
            let a = if bytes_per_pixel == 4 {
                raw[px + 3]
            } else {
                0xFF
            };
            row.push(
                (u32::from(a) << 24) | (u32::from(b) << 16) | (u32::from(g) << 8) | u32::from(r),
            );
        }
        rows.push(row);
    }
    // Descriptor bit 5 (0x20) marks the origin at the top-left; without it
    // the file is bottom-up and the rows flip.
    if descriptor & 0x20 == 0 {
        rows.reverse();
    }
    let format = if bpp == 24 {
        TextureFormat::R8g8b8
    } else {
        TextureFormat::A8r8g8b8
    };
    Some((
        width as u32,
        height as u32,
        format,
        rows.into_iter().flatten().collect(),
    ))
}

/// `BWResource::getExtension`: the text after the last dot. Callers compare
/// case-insensitively in the reference (`equals_lower`).
fn get_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) => &name[dot + 1..],
        None => "",
    }
}

/// `BWResource::removeExtension`: the name without its last dot suffix.
fn remove_extension(name: &str) -> &str {
    match name.rfind('.') {
        // A leading dot is the name's first character: nothing before it to
        // keep, matching the reference's `rfind` on a non-empty prefix.
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// `removeTextureExtension` (base_texture.cpp:241-249): strip the extension,
/// then strip a `.c` — the compiled-copy suffix — if that is what is left.
fn remove_texture_extension(name: &str) -> String {
    let mut base = remove_extension(name);
    if get_extension(base).eq_ignore_ascii_case("c") {
        base = remove_extension(base);
    }
    base.to_owned()
}

/// `canonicalTextureName` (base_texture.cpp:252-263): base plus the
/// extension as given, with a `.c` stripped in between.
fn canonical_texture_name(name: &str) -> String {
    if name.is_empty() {
        return name.to_owned();
    }
    let extension = get_extension(name);
    format!("{}.{}", remove_texture_extension(name), extension)
}

/// `TextureHelpers::copyNormalisedTextureName` (texture_helpers.cpp:16-40):
/// strip a leading slash, convert backslashes to forward slashes. No
/// lower-casing.
fn normalise_texture_name(name: &str) -> String {
    let stripped = name.strip_prefix('/').unwrap_or(name);
    stripped.replace('\\', "/")
}

/// The ported `TextureManager` (`texture_manager.hpp`): the cache, the
/// not-found policy knobs and the res tree the loads read from.
///
/// The manager's file access goes through `BWResource::openSection(...) ->
/// asBinary()` in the reference; the ported `add_file` plays that role for
/// vendored assets, so `get` exercises the same cache and failure chain.
pub struct TextureManager {
    /// The res tree: resource path -> file bytes (`BWResource` face).
    files: BTreeMap<String, Vec<u8>>,
    /// The cache, keyed exactly as the reference caches: `BW::map` over the
    /// lookup names, so a `BTreeMap` reproduces the iteration order.
    textures: BTreeMap<String, Rc<Texture>>,
    /// `useDummyTexture_`: when set, every request is redirected to the
    /// not-found bmp by name (texture_manager.cpp:398).
    use_dummy_texture: bool,
    /// `TextureManager::notFoundBmp()`: the `AutoConfigString` value
    /// ("system/notFoundBmp"); without the config data the reference still
    /// names the key, so the port carries the key as the value.
    not_found_bmp: String,
    /// The load-time messages, mirrored from the reference's ASSET_MSG /
    /// WARNING_MSG texts.
    pub log: Vec<String>,
}

impl Default for TextureManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TextureManager {
    /// A manager with an empty res tree and cache.
    pub fn new() -> Self {
        Self {
            files: BTreeMap::new(),
            textures: BTreeMap::new(),
            use_dummy_texture: false,
            not_found_bmp: "system/notFoundBmp".to_owned(),
            log: Vec::new(),
        }
    }

    /// Add a file to the res tree the manager reads through. The bytes are
    /// decoded through the same load path a `get` uses.
    pub fn add_file(&mut self, name: &str, raw: &[u8]) {
        self.files.insert(name.to_owned(), raw.to_vec());
    }

    /// Redirect every request to the not-found bmp
    /// (`TextureManager::useDummyTexture`, texture_manager.hpp).
    pub fn use_dummy_texture(&mut self, on: bool) {
        self.use_dummy_texture = on;
    }

    /// `TextureManager::get` with its default arguments
    /// (texture_manager.hpp:79-81: `allowAnimation = true, mustExist = true,
    /// loadIfMissing = true`), the form the effect property path uses.
    pub fn get(&mut self, resource_id: &str) -> Option<Rc<Texture>> {
        self.get_explicit(resource_id, true, true, true)
    }

    /// `TextureManager::get(resourceID, allowAnimation, mustExist,
    /// loadIfMissing)` (texture_manager.cpp:389-477).
    pub fn get_explicit(
        &mut self,
        resource_id: &str,
        allow_animation: bool,
        must_exist: bool,
        load_if_missing: bool,
    ) -> Option<Rc<Texture>> {
        // `useDummyTexture_ ? s_notFoundBmp.value() : resourceID`, then the
        // normalisation copy (:396-400).
        let name = if self.use_dummy_texture {
            normalise_texture_name(&self.not_found_bmp.clone())
        } else {
            normalise_texture_name(resource_id)
        };
        let base_name = remove_texture_extension(&name);

        // Empty path: ASSET_MSG and NULL (non-editor branch, :404-411).
        if name.is_empty() {
            self.log
                .push("Moo::TextureManager::get: empty texture path was requested!".to_owned());
            return None;
        }

        // The texanim cache probe only fires when animation is allowed and
        // the name is long enough to bother (:425-437).
        if name.len() > 3 && allow_animation {
            let anim_name = format!("{}.texanim", base_name);
            if let Some(found) = self.find(&anim_name) {
                return Some(found);
            }
        }

        // The direct cache probe (:441-444)...
        if let Some(found) = self.find(&name) {
            return Some(found);
        }

        // ...then the same texture loaded under its dds-swapped name
        // (:446-456): "try it as an already loaded dds then".
        let dds_name = format!("{}.dds", base_name);
        if let Some(found) = self.find(&dds_name) {
            return Some(found);
        }

        if !load_if_missing {
            return None;
        }

        self.load_unique(&name, &base_name, allow_animation, must_exist)
    }

    /// `TextureManager::find`: an exact-key cache probe (:984-996).
    fn find(&self, name: &str) -> Option<Rc<Texture>> {
        self.textures.get(name).cloned()
    }

    /// `TextureManager::getUnique` (texture_manager.cpp:589-666) reduced to
    /// the file-load face: the LOD detail levels and the streaming manager
    /// are D3D-specific intermediaries, and the reference's fallback for
    /// their absence is exactly this `ManagedTexture` file load.
    fn load_unique(
        &mut self,
        name: &str,
        base_name: &str,
        allow_animation: bool,
        must_exist: bool,
    ) -> Option<Rc<Texture>> {
        // The animating texture: when a `.texanim` file exists the manager
        // creates the record, caches it under its own name and returns it
        // before any file decode (:601-616). Frame loading is the feeder
        // system's face and stays outside the binding scope.
        if allow_animation {
            let anim_name = format!("{}.texanim", remove_texture_extension(name));
            if self.files.contains_key(&anim_name) {
                let texture = Rc::new(Texture::not_ready(anim_name.clone(), true));
                self.textures.insert(anim_name.clone(), texture.clone());
                return Some(texture);
            }
        }

        // The internal cache id for a newly loaded file is the resource id
        // with its extension swapped to `.dds` (:632-640) — so every later
        // probe, including the `.dds` one above, finds it.
        let internal_id = format!("{}.dds", base_name);

        match self.load_file(name) {
            Some((width, height, format, pixels)) => {
                let texture = Rc::new(Texture {
                    resource_id: canonical_texture_name(name),
                    width,
                    height,
                    format,
                    pixels,
                    status: TextureStatus::Ready,
                    animating: false,
                });
                self.textures.insert(internal_id, texture.clone());
                Some(texture)
            }
            None => {
                if must_exist {
                    // `FAIL_ATTEMPT_LOAD_PLACEHOLDER`: report the missing
                    // file, rebind to the not-found bmp and try that
                    // (`BaseTexture::processLoaderOutput`,
                    // base_texture.cpp:146-166).
                    self.log.push(format!(
                        "Moo::ManagedTexture::load: Can't read texture file '{}'.",
                        name
                    ));
                    match self.load_file(&self.not_found_bmp.clone()) {
                        Some((width, height, format, pixels)) => {
                            let texture = Rc::new(Texture {
                                resource_id: canonical_texture_name(&self.not_found_bmp.clone()),
                                width,
                                height,
                                format,
                                pixels,
                                status: TextureStatus::Ready,
                                animating: false,
                            });
                            self.textures.insert(internal_id, texture.clone());
                            Some(texture)
                        }
                        None => {
                            // The fallback itself is missing: report it too
                            // and hand back nothing (:167-172).
                            self.log.push(format!(
                                "Moo::BaseTexture::reportMissingTexture: Can't read texture file '{}'.",
                                self.not_found_bmp
                            ));
                            None
                        }
                    }
                } else {
                    // `FAIL_RETURN_NULL`: the load fails silently into a
                    // `FAILED_TO_LOAD` texture that `getUnique` discards
                    // (:652-660).
                    None
                }
            }
        }
    }

    /// The file half of `TextureLoader::loadTextureFromFile`: read the bytes
    /// from the res tree and decode. The reference hands file bytes to D3DX,
    /// which sniffs the content; the port dispatches on the extension and
    /// falls back to the same content sniff (the `BM` magic, then the TGA
    /// header checks) when the extension names no ported decoder — that is
    /// how a no-extension config name like `system/notFoundBmp` still loads.
    /// Formats without a ported decoder fail, taking the reference's
    /// undecodable-file path.
    fn load_file(&self, name: &str) -> Option<(u32, u32, TextureFormat, Vec<u32>)> {
        let raw = self.files.get(name)?;
        match get_extension(name).to_ascii_lowercase().as_str() {
            "bmp" => decode_bmp(raw),
            "tga" => decode_tga(raw),
            _ => {
                if raw.len() >= 2 && &raw[0..2] == b"BM" {
                    decode_bmp(raw)
                } else {
                    decode_tga(raw)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material_oracle_data::{AXE_DIFFUSE_FIRST_PIXELS, AXE_SPEC_FIRST_PIXELS};

    const AXE_TGA: &[u8] = include_bytes!("../res/axe.tga");
    const AXE_SPEC_BMP: &[u8] = include_bytes!("../res/axe_spec.bmp");

    /// The manager carrying the vendored axe fixtures at their res paths.
    fn axe_manager() -> TextureManager {
        let mut manager = TextureManager::new();
        manager.add_file("sets/town/props/axe.tga", AXE_TGA);
        manager.add_file("sets/town/props/axe_spec.bmp", AXE_SPEC_BMP);
        manager
    }

    #[test]
    fn axe_tga_decodes_to_the_oracle_pixels() {
        let raw = decode_tga(AXE_TGA).expect("axe.tga decodes");
        let (width, height, format, pixels) = raw;
        assert_eq!((width, height), (256, 256));
        assert_eq!(format, TextureFormat::A8r8g8b8);
        assert_eq!(&pixels[..8], &AXE_DIFFUSE_FIRST_PIXELS, "visual top row");
        assert_eq!(pixels.len(), 256 * 256);
    }

    #[test]
    fn axe_spec_bmp_decodes_to_the_oracle_pixels() {
        let raw = decode_bmp(AXE_SPEC_BMP).expect("axe_spec.bmp decodes");
        let (width, height, format, pixels) = raw;
        assert_eq!((width, height), (128, 128));
        assert_eq!(format, TextureFormat::R8g8b8);
        assert_eq!(
            &pixels[..8],
            &AXE_SPEC_FIRST_PIXELS,
            "flipped visual top row"
        );
        assert_eq!(pixels.len(), 128 * 128);
    }

    #[test]
    fn a_bmp_with_an_exotic_bpp_fails() {
        // BITMAPINFOHEADER claiming 8bpp: outside the lossless decode face.
        let mut raw = AXE_SPEC_BMP.to_vec();
        raw[28..30].copy_from_slice(&8u16.to_le_bytes());
        assert!(decode_bmp(&raw).is_none());
    }

    #[test]
    fn a_rle_tga_fails_into_the_not_found_path() {
        // Image type 10 (RLE true-colour): not decoded, so the load reports
        // the file and the not-found bmp, then hands back nothing.
        let mut manager = axe_manager();
        let mut rle = AXE_TGA.to_vec();
        rle[2] = 10;
        manager.add_file("broken.tga", &rle);
        assert!(manager.get("broken.tga").is_none());
        assert_eq!(
            manager.log[0],
            "Moo::ManagedTexture::load: Can't read texture file 'broken.tga'."
        );
        assert!(manager.log[1].contains("system/notFoundBmp"));
    }

    #[test]
    fn get_loads_and_caches_under_the_dds_swapped_name() {
        let mut manager = axe_manager();
        let texture = manager.get("sets/town/props/axe.tga").expect("loads");
        assert_eq!(texture.status, TextureStatus::Ready);
        assert_eq!(texture.width, 256);
        assert_eq!(texture.resource_id, "sets/town/props/axe.tga");
        // The internal id carries the `.dds` extension swap.
        assert!(manager.textures.contains_key("sets/town/props/axe.dds"));
        assert!(!manager.textures.contains_key("sets/town/props/axe.tga"));
    }

    #[test]
    fn a_second_get_and_the_dds_spelling_hit_the_same_texture() {
        let mut manager = axe_manager();
        let first = manager.get("sets/town/props/axe.tga").expect("loads");
        let again = manager.get("sets/town/props/axe.tga").expect("cached");
        let as_dds = manager.get("sets/town/props/axe.dds").expect("cached");
        assert!(Rc::ptr_eq(&first, &again));
        assert!(Rc::ptr_eq(&first, &as_dds));
    }

    #[test]
    fn a_missing_texture_reports_then_falls_back_to_null() {
        let mut manager = axe_manager();
        assert!(manager.get("system/maps/default_norms.bmp").is_none());
        assert_eq!(
            manager.log[0],
            "Moo::ManagedTexture::load: Can't read texture file 'system/maps/default_norms.bmp'."
        );
        assert_eq!(
            manager.log[1],
            "Moo::BaseTexture::reportMissingTexture: Can't read texture file 'system/notFoundBmp'."
        );
        // Nothing was cached: the next get repeats the whole chain.
        assert!(manager.textures.is_empty());
    }

    #[test]
    fn load_if_missing_false_returns_null_without_a_word() {
        let mut manager = axe_manager();
        assert!(
            manager
                .get_explicit("system/maps/default_norms.bmp", true, true, false)
                .is_none()
        );
        assert!(manager.log.is_empty());
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let mut manager = axe_manager();
        assert!(manager.get("").is_none());
        assert_eq!(
            manager.log[0],
            "Moo::TextureManager::get: empty texture path was requested!"
        );
    }

    #[test]
    fn dummy_texture_mode_redirects_every_request() {
        let mut manager = axe_manager();
        manager.add_file("system/notFoundBmp", AXE_SPEC_BMP);
        manager.use_dummy_texture(true);
        let texture = manager.get("sets/town/props/axe.tga").expect("dummy");
        // `canonicalTextureName` appends a dot plus the extension, and an
        // extension-less name still gets the dot (base_texture.cpp:252-263).
        assert_eq!(texture.resource_id, "system/notFoundBmp.");
        assert_eq!(texture.width, 128);
        // Cached under the not-found name's dds swap, per the internal id.
        assert!(manager.textures.contains_key("system/notFoundBmp.dds"));
    }

    #[test]
    fn a_texanim_file_becomes_an_animating_texture_record() {
        let mut manager = axe_manager();
        manager.add_file("sets/town/props/axe.texanim", b"<texanim/>");
        let texture = manager.get("sets/town/props/axe.tga").expect("animating");
        assert!(texture.animating);
        assert_eq!(texture.resource_id, "sets/town/props/axe.texanim");
        assert_eq!(texture.status, TextureStatus::NotReady);
        // The next request resolves through the texanim cache probe.
        let again = manager.get("sets/town/props/axe.tga").expect("cached");
        assert!(Rc::ptr_eq(&texture, &again));
    }

    #[test]
    fn names_are_normalised_before_the_cache_chain() {
        let mut manager = axe_manager();
        let plain = manager.get("sets/town/props/axe.tga").expect("loads");
        // Same path modulo the leading slash and backslash conversion (the
        // reference never case-folds), so the dds-swapped cache probe hits.
        let normal = manager.get("/sets\\town\\props\\axe.tga").expect("cached");
        assert!(Rc::ptr_eq(&plain, &normal));
    }

    #[test]
    fn the_c_copy_suffix_is_stripped_for_the_cache_base() {
        let mut manager = axe_manager();
        manager.add_file("art/tex.c.tga", AXE_TGA);
        let texture = manager.get("art/tex.c.tga").expect("loads");
        assert_eq!(texture.resource_id, "art/tex.tga");
        // The internal id dropped the `.c`: "art/tex.dds".
        assert!(manager.textures.contains_key("art/tex.dds"));
        let as_dds = manager.get("art/tex.dds").expect("cached");
        assert!(Rc::ptr_eq(&texture, &as_dds));
    }

    #[test]
    fn unsupported_extensions_take_the_placeholder_path() {
        let mut manager = axe_manager();
        manager.add_file("art/cube.dds", b"not a real dds");
        assert!(manager.get("art/cube.dds").is_none());
        assert_eq!(
            manager.log[0],
            "Moo::ManagedTexture::load: Can't read texture file 'art/cube.dds'."
        );
    }

    #[test]
    fn a_missing_file_with_must_exist_false_fails_quietly() {
        let mut manager = axe_manager();
        assert!(
            manager
                .get_explicit("system/maps/default_norms.bmp", true, false, true)
                .is_none()
        );
        assert!(manager.log.is_empty());
    }

    #[test]
    fn short_names_skip_the_cache_probe_but_not_animating_creation() {
        // The length gate (`name.length() > 3`) sits on the texanim *cache
        // probe* only (texture_manager.cpp:425); `getUnique`'s animating
        // creation (:601) has no gate. So a short name never finds a cached
        // animating texture, but still creates one from a `.texanim` file.
        let mut manager = axe_manager();
        manager.add_file("a", AXE_TGA);
        manager.add_file("a.texanim", b"<texanim/>");
        let texture = manager.get("a").expect("animating");
        assert!(texture.animating);
        assert_eq!(texture.resource_id, "a.texanim");
    }

    /// An uncompressed BMP: `BITMAPINFOHEADER` (40 bytes) or, for a core
    /// header, a truncated data slice — all rejected into the not-found
    /// path (base_texture.cpp:140-188).
    fn bmp(width: i32, height: i32, bpp: u16, header_size: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(14 + 40 + pixels.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(14u32 + 40).to_le_bytes());
        out.extend_from_slice(&header_size.to_le_bytes());
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&bpp.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(pixels);
        out
    }

    #[test]
    fn a_corrupt_bmp_fails_into_the_not_found_path() {
        assert!(decode_bmp(b"junk").is_none());
        // A core header (`BITMAPCOREHEADER`, 12 bytes) is too small.
        let core = bmp(1, 1, 24, 12, &[1, 2, 3]);
        assert!(decode_bmp(&core).is_none());
        // The pixel data is truncated.
        let mut truncated = bmp(2, 2, 24, 40, &[1, 2, 3]);
        truncated.truncate(14 + 40 + 3);
        assert!(decode_bmp(&truncated).is_none());
    }

    #[test]
    fn a_32bpp_bmp_decodes_with_alpha() {
        // One 4-byte pixel: BGR + alpha, stored as the only bottom-up row.
        let raw = bmp(1, 1, 32, 40, &[0x82, 0x39, 0x45, 0x7F, 1, 2, 3, 4]);
        let (width, height, format, pixels) = decode_bmp(&raw).expect("decodes");
        assert_eq!((width, height), (1, 1));
        assert_eq!(format, TextureFormat::A8r8g8b8);
        assert_eq!(pixels, vec![0x7F823945]);
    }

    /// An uncompressed true-colour TGA: id length 0, colour map off, image
    /// type 2, uncompressed, the given bpp and descriptor.
    fn tga(width: u16, height: u16, bpp: u8, descriptor: u8, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0); // id length
        out.push(0); // colour map type
        out.push(2); // image type: uncompressed true-colour
        // Colour map spec: five bytes (first-entry index, length, size).
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.push(0);
        // X and Y origin.
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.push(bpp);
        out.push(descriptor);
        out.extend_from_slice(pixels);
        out
    }

    #[test]
    fn a_24bpp_top_down_tga_decodes_without_a_flip() {
        // Descriptor bit 5 set: the rows arrive in visual order.
        let raw = tga(1, 2, 24, 0x20, &[10, 20, 30, 40, 50, 60]);
        let (width, height, format, pixels) = decode_tga(&raw).expect("decodes");
        assert_eq!((width, height), (1, 2));
        assert_eq!(format, TextureFormat::R8g8b8);
        // Visual top row first, no flip; 24bpp pixels read alpha 0xFF.
        assert_eq!(pixels, vec![0xFF0A141E, 0xFF28323C]);
    }

    #[test]
    fn a_bottom_up_24bpp_tga_decodes_with_a_flip() {
        let raw = tga(1, 2, 24, 0x00, &[10, 20, 30, 40, 50, 60]);
        let (_width, _height, _format, pixels) = decode_tga(&raw).expect("decodes");
        assert_eq!(pixels, vec![0xFF28323C, 0xFF0A141E]);
    }

    #[test]
    fn a_corrupt_tga_fails_into_the_not_found_path() {
        assert!(decode_tga(&tga(1, 1, 16, 0, &[1, 2, 3])).is_none());
        assert!(decode_tga(&tga(0, 1, 24, 0, &[])).is_none());
        // Truncated pixel data.
        let mut truncated = tga(2, 2, 24, 0, &[1, 2, 3]);
        truncated.truncate(18 + 3);
        assert!(decode_tga(&truncated).is_none());
    }

    #[test]
    fn canonical_texture_name_of_an_empty_name_is_empty() {
        // The extension-less-name quirk carries the trailing dot; an empty
        // name has neither base nor extension.
        assert_eq!(canonical_texture_name(""), "");
    }

    #[test]
    fn default_manager_matches_new() {
        let manager = TextureManager::default();
        assert_eq!(manager.not_found_bmp, "system/notFoundBmp");
        assert!(!manager.use_dummy_texture);
        assert!(manager.textures.is_empty());
    }

    #[test]
    fn a_missing_texture_rebinds_to_a_registered_not_found_bmp() {
        // The `FAIL_ATTEMPT_LOAD_PLACEHOLDER` path (base_texture.cpp:146-166):
        // the missing file is reported, the not-found bmp loads in its place
        // and the cache keeps the requested name's dds spelling.
        let mut manager = axe_manager();
        manager.add_file("system/notFoundBmp", AXE_SPEC_BMP);
        let texture = manager
            .get("sets/town/props/missing.bmp")
            .expect("placeholder");
        assert_eq!(texture.resource_id, "system/notFoundBmp.");
        assert_eq!(texture.width, 128);
        assert_eq!(
            manager
                .textures
                .get("sets/town/props/missing.dds")
                .unwrap()
                .width,
            128
        );
        assert_eq!(
            manager.log[0],
            "Moo::ManagedTexture::load: Can't read texture file 'sets/town/props/missing.bmp'."
        );
    }
}
