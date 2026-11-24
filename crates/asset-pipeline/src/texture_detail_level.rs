//! Texture detail levels (`lib/moo/texture_detail_level*.cpp|hpp`,
//! `format_name_map.cpp`): the per-texture conversion settings the texture
//! converter and the runtime loader share.
//!
//! - [`format_string`]/[`format_type`]: the `FormatNameMap` two-way table of
//!   D3DFMT names (`format_name_map.cpp` — one entry per D3D9 format, see
//!   d3d9types.h). The port codes each entry by its table index; the
//!   pipeline never serialises the codes numerically (texformat XML goes
//!   through the names), only their equality with `D3DFMT_UNKNOWN`.
//! - [`TextureDetailLevel`]: the settings of one texture, read from a
//!   `.texformat` file or the global texture_detail_levels file.
//! - [`TextureDetailLevelManager`]: per-texture lookup — the texture's own
//!   `.texformat` section wins over the global rule set, results are cached
//!   per original name.

use std::collections::HashMap;

use resources::datasection::DataSection;

use crate::hierarchical_config::HierarchicalConfig;

/// `D3DFORMAT`: a code from the [`FORMAT_NAMES`] table (`d3d9types.h`).
pub type Format = u32;

/// `D3DFMT_UNKNOWN` — the table's first entry.
pub const D3DFMT_UNKNOWN: Format = 0;

/// The `FormatNameMap` table, in `format_name_map.cpp` order. Codes are the
/// table indices (see the module docs); names are exact.
pub const FORMAT_NAMES: [&str; 42] = [
    "UNKNOWN",
    "R8G8B8",
    "A8R8G8B8",
    "X8R8G8B8",
    "R5G6B5",
    "X1R5G5B5",
    "A1R5G5B5",
    "A4R4G4B4",
    "R3G3B2",
    "A8",
    "A8R3G3B2",
    "X4R4G4B4",
    "A2B10G10R10",
    "G16R16",
    "A8P8",
    "P8",
    "L8",
    "A8L8",
    "A4L4",
    "V8U8",
    "L6V5U5",
    "X8L8V8U8",
    "Q8W8V8U8",
    "V16U16",
    "A2W10V10U10",
    "UYVY",
    "YUY2",
    "DXT1",
    "DXT2",
    "DXT3",
    "DXT4",
    "DXT5",
    "D16_LOCKABLE",
    "D32",
    "D15S1",
    "D24S8",
    "D16",
    "D24X8",
    "D24X4S4",
    "VERTEXDATA",
    "INDEX16",
    "INDEX32",
];

/// `FormatNameMap::formatString`: the table name, or `D3DFMT_UNKNOWN`.
pub fn format_string(fmt: Format) -> String {
    FORMAT_NAMES
        .get(fmt as usize)
        .map(|name| name.to_string())
        .unwrap_or_else(|| "D3DFMT_UNKNOWN".to_string())
}

/// `FormatNameMap::formatType`: the table code, or [`D3DFMT_UNKNOWN`].
pub fn format_type(name: &str) -> Format {
    FORMAT_NAMES
        .iter()
        .position(|entry| *entry == name)
        .unwrap_or(0) as Format
}

/// `TEXTURE_DETAIL_LEVEL_VERSION` (`texture_detail_level.hpp`).
pub const TEXTURE_DETAIL_LEVEL_VERSION: i32 = 1;

/// The texture extension priority list (`TextureHelpers::textureExts` —
/// `dds` first; the veto order of the texture converter's priority check).
pub const TEXTURE_EXTS: [&str; 9] = [
    "dds", "bmp", "tga", "jpg", "png", "hdr", "pfm", "dib", "ppm",
];

/// `Moo::removeTextureExtension` (`base_texture.cpp`): strip one extension,
/// then a trailing `.c` off the compressed pair (`bark.c.dds` → `bark`).
pub fn remove_texture_extension(filename: &str) -> String {
    let mut base = resources::bwresource::remove_extension(filename).to_string();
    if resources::bwresource::get_extension(&base) == "c" {
        base = resources::bwresource::remove_extension(&base).to_string();
    }
    base
}

/// `TextureDetailLevel::SourceMipType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceMipType {
    #[default]
    NoMipmaps,
    HorizontalMipmaps,
    VerticalMipmaps,
}

/// `TextureDetailLevel`: one texture's conversion settings, defaulting
/// exactly like the reference constructor.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureDetailLevel {
    pub prefixes: Vec<String>,
    pub postfixes: Vec<String>,
    pub contains: Vec<String>,
    pub lod_mode: i32,
    pub mip_count: i32,
    pub mip_size: i32,
    pub streaming_priority: i32,
    pub max_extent: i32,
    pub source_mip_type: SourceMipType,
    pub no_resize: bool,
    pub no_filter: bool,
    pub normal_map: bool,
    pub streamable: bool,
    pub format: Format,
    pub format_compressed: Format,
}

impl Default for TextureDetailLevel {
    /// The reference constructor's defaults (`texture_detail_level.cpp`).
    fn default() -> Self {
        Self {
            prefixes: Vec::new(),
            postfixes: Vec::new(),
            contains: Vec::new(),
            lod_mode: 0,
            mip_count: 0,
            mip_size: 0,
            streaming_priority: 0,
            max_extent: 0,
            source_mip_type: SourceMipType::NoMipmaps,
            no_resize: false,
            no_filter: false,
            normal_map: false,
            // streamable_ defaults true.
            streamable: true,
            format: format_type("A8R8G8B8"),
            format_compressed: D3DFMT_UNKNOWN,
        }
    }
}

impl TextureDetailLevel {
    /// `TextureDetailLevel::init` (`texture_detail_level.cpp`): overlay the
    /// section's fields onto the defaults. The `format` name keeps its old
    /// value when unknown; `formatCompressed` keeps unknown.
    pub fn init(&mut self, section: &DataSection) {
        self.prefixes = read_string_list(section, "prefix");
        self.postfixes = read_string_list(section, "postfix");
        self.contains = read_string_list(section, "contains");
        self.lod_mode = section.read_int("lodMode", self.lod_mode);
        self.mip_count = section.read_int("mipCount", self.mip_count);
        if section.read_bool("horizontalMips", false) {
            self.source_mip_type = SourceMipType::HorizontalMipmaps;
        }
        if section.read_bool("verticalMips", false) {
            self.source_mip_type = SourceMipType::VerticalMipmaps;
        }
        self.no_resize = section.read_bool("noResize", self.no_resize);
        self.no_filter = section.read_bool("noFilter", self.no_filter);
        self.normal_map = section.read_bool("normalMap", self.normal_map);
        self.streamable = section.read_bool("streamable", self.streamable);
        self.streaming_priority = section.read_int("streamingPriority", self.streaming_priority);
        self.mip_size = section.read_int("mipSize", self.mip_size);
        self.max_extent = section.read_int("maxExtent", self.max_extent);

        // The format name read-in: unknown names leave the old format.
        let format_name = if format_string(self.format) == "D3DFMT_UNKNOWN" {
            "A8R8G8B8".to_string()
        } else {
            format_string(self.format)
        };
        let format_name = section.read_string("format", &format_name);
        let new_format = format_type(&format_name);
        if new_format != D3DFMT_UNKNOWN {
            self.format = new_format;
        }

        // formatCompressed: missing stays UNKNOWN.
        let name = section.read_string("formatCompressed", "UNKNOWN");
        let new_format = format_type(&name);
        if new_format != D3DFMT_UNKNOWN {
            self.format_compressed = new_format;
        }
    }

    /// `TextureDetailLevel::write` (`texture_detail_level.cpp`): blow the
    /// section away and write every property back (the version child as an
    /// attribute, exactly in field order).
    pub fn write(&self, section: &mut DataSection) {
        section.del_children();

        let version = section.add_child("version");
        version.set_attribute(true);
        version.set_int(TEXTURE_DETAIL_LEVEL_VERSION);

        for name in &self.prefixes {
            section.write_string("prefix", name);
        }
        for name in &self.postfixes {
            section.write_string("postfix", name);
        }
        for name in &self.contains {
            section.write_string("contains", name);
        }
        section.write_int("lodMode", self.lod_mode);
        section.write_int("mipCount", self.mip_count);
        section.write_bool(
            "horizontalMips",
            self.source_mip_type == SourceMipType::HorizontalMipmaps,
        );
        section.write_bool(
            "verticalMips",
            self.source_mip_type == SourceMipType::VerticalMipmaps,
        );
        section.write_bool("noResize", self.no_resize);
        section.write_bool("noFilter", self.no_filter);
        section.write_bool("normalMap", self.normal_map);
        section.write_bool("streamable", self.streamable);
        section.write_int("streamingPriority", self.streaming_priority);
        section.write_int("mipSize", self.mip_size);
        section.write_int("maxExtent", self.max_extent);
        section.write_string("format", &format_string(self.format));
        section.write_string("formatCompressed", &format_string(self.format_compressed));
    }

    /// `TextureDetailLevel::isCompressed`.
    pub fn is_compressed(&self) -> bool {
        self.format_compressed != D3DFMT_UNKNOWN
    }
}

/// `readStrings` for repeat children under a tag.
fn read_string_list(section: &DataSection, tag: &str) -> Vec<String> {
    (0..section.count_children())
        .filter_map(|i| section.open_child(i))
        .filter(|child| child.section_name() == tag)
        .map(|child| child.as_string().to_string())
        .collect()
}

/// `s_textureDetailLevels` — the `AutoConfigString` default the manager
/// reports before `init` overrides it.
pub const AUTO_TEXTURE_DETAIL_LEVELS_NAME: &str = "system/textureDetailLevels";

/// `TextureDetailLevelManager` (`texture_detail_level_manager.cpp`): the
/// per-texture lookup over the global rule set.
pub struct TextureDetailLevelManager {
    texture_detail_levels_name: String,
    texture_details: Option<HierarchicalConfig>,
    cache: HashMap<String, TextureDetailLevel>,
}

impl Default for TextureDetailLevelManager {
    fn default() -> Self {
        Self {
            texture_detail_levels_name: AUTO_TEXTURE_DETAIL_LEVELS_NAME.to_string(),
            texture_details: None,
            cache: HashMap::new(),
        }
    }
}

impl TextureDetailLevelManager {
    /// `getTextureDetailLevelsName` — the auto-config string
    /// `system/textureDetailLevels`; the port carries it as a field.
    pub fn get_texture_detail_levels_name(&self) -> &str {
        &self.texture_detail_levels_name
    }

    /// `init()` / `init( textureDetailsName )`: load the global rule set.
    pub fn init(
        &mut self,
        texture_details_name: &str,
        resource: &resources::bwresource::BwResource,
    ) {
        self.texture_detail_levels_name = texture_details_name.to_string();
        let mut details = HierarchicalConfig::new();
        if let Some(section) = resource.open_section(texture_details_name, false) {
            details.load_section(&section);
        }
        self.texture_details = Some(details);
        self.cache.clear();
    }

    /// `fini`.
    pub fn fini(&mut self) {
        self.texture_details = None;
    }

    /// `detailLevel( originalName )`: overrides first (none in this port),
    /// then the cache, then the texture's `.texformat` section, then the
    /// global rule set.
    pub fn detail_level(
        &mut self,
        original_name: &str,
        resource: &resources::bwresource::BwResource,
    ) -> TextureDetailLevel {
        if let Some(cached) = self.cache.get(original_name) {
            return cached.clone();
        }

        let mut level = TextureDetailLevel::default();
        let texformat = format!("{}.texformat", remove_texture_extension(original_name));
        let section = resource
            .open_section(&texformat, false)
            .map(|section| (*section).clone())
            .or_else(|| {
                self.texture_details
                    .as_ref()
                    .map(|details| details.get(original_name))
            });
        if let Some(section) = section {
            level.init(&section);
        }

        self.cache.insert(original_name.to_string(), level.clone());
        level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resources::bwresource::BwResource;
    use resources::datasection;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("texlod-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &std::path::Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// The format table round-trips names both ways and reports unknowns.
    /// `UNKNOWN` is a table entry — `D3DFMT_UNKNOWN` is only the fallback
    /// for codes outside the table.
    #[test]
    fn format_table_maps_both_ways() {
        assert_eq!(format_string(D3DFMT_UNKNOWN), "UNKNOWN");
        assert_eq!(format_string(99), "D3DFMT_UNKNOWN");
        assert_eq!(format_type("A8R8G8B8"), 2);
        assert_eq!(format_string(2), "A8R8G8B8");
        assert_eq!(format_type("DXT5"), 31);
        assert_eq!(format_string(31), "DXT5");
        assert_eq!(format_type("NOT_A_FORMAT"), D3DFMT_UNKNOWN);
    }

    /// `removeTextureExtension` strips one extension, plus a trailing `.c`
    /// off the compressed pair.
    #[test]
    fn texture_extension_stripping_walks_the_priority_list() {
        assert_eq!(
            remove_texture_extension("textures/wall.tga"),
            "textures/wall"
        );
        assert_eq!(remove_texture_extension("bark.c.dds"), "bark");
        // Only the last extension goes, then a `.c` — never a second walk.
        assert_eq!(remove_texture_extension("a.tga.dds"), "a.tga");
        assert_eq!(remove_texture_extension("a.texformat"), "a");
        assert_eq!(remove_texture_extension("noext"), "noext");
    }

    /// `init` overlays the section onto the defaults; `write` serialises
    /// the same fields back.
    #[test]
    fn detail_level_init_overlays_and_write_round_trips() {
        let raw = "<texformat>\n\t<format>DXT5</format>\n\t<formatCompressed>DXT1</formatCompressed>\n\t<maxExtent>128</maxExtent>\n\t<normalMap>true</normalMap>\n\t<prefix>env</prefix>\n\t<contains>stone</contains>\n</texformat>\n";
        let section = datasection::parse("texformat", raw.as_bytes()).unwrap();
        let mut level = TextureDetailLevel::default();
        level.init(&section);

        assert!(level.is_compressed());
        assert_eq!(format_string(level.format), "DXT5");
        assert_eq!(format_string(level.format_compressed), "DXT1");
        assert_eq!(level.max_extent, 128);
        assert!(level.normal_map);
        assert_eq!(level.prefixes, vec!["env".to_string()]);
        assert_eq!(level.contains, vec!["stone".to_string()]);
        // Untouched fields keep their defaults.
        assert_eq!(level.lod_mode, 0);
        assert!(level.streamable);

        // write serialises the same values; re-init reads them back.
        let mut out = DataSection::new("texformat");
        level.write(&mut out);
        let xml = out.write_to_string();
        assert!(xml.contains("version=\"1\""));
        let mut reloaded = TextureDetailLevel::default();
        reloaded.init(&datasection::parse("texformat", xml.as_bytes()).unwrap());
        assert_eq!(reloaded, level);

        // An unknown format name keeps the current value; formatCompressed
        // unknown leaves UNKNOWN.
        let dir = temp_root("bad");
        write_file(
            &dir.join("keep.texformat"),
            "<texformat><format>NOPE</format><formatCompressed>NOPE</formatCompressed></texformat>",
        );
        let mut resource = BwResource::init_paths(&[dir.to_str().unwrap()]);
        resource.enable_editor_default_root();
        let fmt = resource.open_section("keep.texformat", false).unwrap();
        let mut keep = TextureDetailLevel::default();
        keep.init(&fmt);
        assert_eq!(format_string(keep.format), "A8R8G8B8");
        assert!(!keep.is_compressed());
    }

    /// The manager: the texture's `.texformat` wins over the global rule
    /// set, and lookups are cached.
    #[test]
    fn manager_prefers_texformat_and_caches_lookups() {
        let dir = temp_root("manager");
        write_file(
            &dir.join("textures/wall.texformat"),
            "<texformat><format>A8R8G8B8</format></texformat>",
        );
        write_file(
            &dir.join("system/texture_detail_levels.xml"),
            "<root>\n\t<rule>\n\t\t<pattern>.*\\.tga</pattern>\n\t\t<format>DXT5</format>\n\t\t<formatCompressed>DXT1</formatCompressed>\n\t</rule>\n</root>\n",
        );
        let mut resource = BwResource::init_paths(&[dir.to_str().unwrap()]);
        resource.enable_editor_default_root();

        let mut manager = TextureDetailLevelManager::default();
        manager.init("system/texture_detail_levels.xml", &resource);
        assert_eq!(
            manager.get_texture_detail_levels_name(),
            "system/texture_detail_levels.xml"
        );

        // wall.tga carries its own texformat: uncompressed, no compressed
        // pass.
        let level = manager.detail_level("textures/wall.tga", &resource);
        assert_eq!(format_string(level.format), "A8R8G8B8");
        assert!(!level.is_compressed());

        // ground.tga has none: the global rule set gives the compressed
        // pair.
        let level = manager.detail_level("textures/ground.tga", &resource);
        assert_eq!(format_string(level.format), "DXT5");
        assert!(level.is_compressed());

        // sky.png matches no rule: the defaults.
        let level = manager.detail_level("textures/sky.png", &resource);
        assert_eq!(level, TextureDetailLevel::default());
    }
}
