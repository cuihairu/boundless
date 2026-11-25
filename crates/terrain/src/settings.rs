//! `Terrain::TerrainSettings` (`lib/terrain/terrain_settings.cpp`): the
//! `<terrain>` section of a space's `space.settings` — the version, the
//! renderer map sizes and the vertex-lod distances a terrain block builds
//! against, plus the sound occlusion floats.
//!
//! The renderer hooks (`TerrainGraphicsOptions`, the lod/bump modifiers,
//! `TerrainRenderer2::init`) belong to faces this tree does not carry;
//! [`TerrainSettings::init`] keeps the data-section reads and the
//! editor-side `serverHeightMapLod` derivation. The `system/terrain2`
//! defaults resource is not in the source drop, so the "defaults" fallback
//! of `readIntSetting`/`readFloatSetting` is the bare read (zero when the
//! key is absent) — the same answer the reference gives with a NULL
//! defaults section.

use resources::datasection::DataSection;

/// `TerrainSettings::TERRAIN1_VERSION` (`terrain_settings.cpp:110`).
pub const TERRAIN1_VERSION: u32 = 100;
/// `TerrainSettings::TERRAIN2_VERSION` (`terrain_settings.cpp:111`).
pub const TERRAIN2_VERSION: u32 = 200;
/// `DEFAULT_BLOCK_SIZE` (`terrain_settings.hpp:24`).
pub const DEFAULT_BLOCK_SIZE: f32 = 100.0;

/// One space's terrain settings.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainSettings {
    block_size: f32,
    version: u32,
    lod_map_size: u32,
    ao_map_size: u32,
    height_map_editor_size: u32,
    height_map_size: u32,
    normal_map_size: u32,
    hole_map_size: u32,
    shadow_map_size: u32,
    blend_map_size: u32,
    num_vertex_lods: u32,
    vertex_lod_distances: Vec<f32>,
    vertex_lod_start_bias: f32,
    vertex_lod_end_bias: f32,
    lod_texture_start: f32,
    lod_texture_distance: f32,
    blend_preload_distance: f32,
    bump_fading_start: f32,
    bump_fading_distance: f32,
    lod_normal_start: f32,
    lod_normal_distance: f32,
    normal_preload_distance: f32,
    default_height_map_lod: u32,
    detail_height_map_distance: f32,
    server_height_map_lod: u32,
    direct_sound_occlusion: f32,
    reverb_sound_occlusion: f32,
}

impl Default for TerrainSettings {
    /// The raw constructor (`terrain_settings.cpp:125-130`): only
    /// `blockSize_` is initialised (0) — and the editor/server builds zero
    /// `serverHeightMapLod_`; every other member starts at zero bytes, the
    /// same face the port's zeroed fields give.
    fn default() -> Self {
        Self {
            block_size: 0.0,
            version: 0,
            lod_map_size: 0,
            ao_map_size: 0,
            height_map_editor_size: 0,
            height_map_size: 0,
            normal_map_size: 0,
            hole_map_size: 0,
            shadow_map_size: 0,
            blend_map_size: 0,
            num_vertex_lods: 0,
            vertex_lod_distances: Vec::new(),
            vertex_lod_start_bias: 0.0,
            vertex_lod_end_bias: 0.0,
            lod_texture_start: 0.0,
            lod_texture_distance: 0.0,
            blend_preload_distance: 0.0,
            bump_fading_start: 0.0,
            bump_fading_distance: 0.0,
            lod_normal_start: 0.0,
            lod_normal_distance: 0.0,
            normal_preload_distance: 0.0,
            default_height_map_lod: 0,
            detail_height_map_distance: 0.0,
            server_height_map_lod: 0,
            direct_sound_occlusion: 0.0,
            reverb_sound_occlusion: 0.0,
        }
    }
}

impl TerrainSettings {
    pub fn new() -> Self {
        Self::default()
    }

    /// `TerrainSettings::init` (`terrain_settings.cpp:168-310`), editor
    /// face: read the sizes, derive the vertex-lod count, read (and
    /// rescale when the block size changed) the lod distances and the
    /// lodInfo floats. A section-less init assumes terrain 1 and an
    /// unsupported version reports failure exactly once, after the sound
    /// occlusions are read (`:294-297`).
    pub fn init(
        &mut self,
        block_size: f32,
        settings: Option<&DataSection>,
        old_block_size: f32,
    ) -> bool {
        self.block_size = block_size;
        self.version = match settings {
            // "no settings, so assume terrain 1" (`:181-185`).
            None => TERRAIN1_VERSION,
            Some(section) => read_int(Some(section), "version") as u32,
        };

        if self.version != TERRAIN2_VERSION {
            self.read_sound_occlusions(settings);
            return false;
        }

        let read_size = |key: &str| read_int(settings, key) as u32;
        self.lod_map_size = read_size("lodMapSize");
        self.ao_map_size = read_size("aoMapSize");
        self.height_map_size = read_size("heightMapSize");
        self.height_map_editor_size = match settings {
            // `settings->readInt("heightMapEditorSize", heightMapSize_)`.
            Some(section) => section
                .open_section("heightMapEditorSize")
                .map_or(self.height_map_size, |s| s.as_int(0) as u32),
            None => self.height_map_size,
        };
        self.normal_map_size = read_size("normalMapSize");
        self.hole_map_size = read_size("holeMapSize");
        self.shadow_map_size = read_size("shadowMapSize");
        self.blend_map_size = read_size("blendMapSize");

        // `numVertexLods_ = log2RoundUp(max(heightMapSize_, heightMapEditorSize_))`
        // with the MF_ASSERT power-of-two check.
        let largest = self.height_map_size.max(self.height_map_editor_size);
        self.num_vertex_lods = log2_round_up(largest);
        debug_assert_eq!(1u32 << self.num_vertex_lods, largest);

        // `readLevelDistances` (`:67-100`): one float per vertex lod.
        self.vertex_lod_distances = (0..self.num_vertex_lods)
            .map(|level| read_float(settings, &format!("lodDistances/distance{level}")))
            .collect();

        // `EDITOR_ENABLED`: rescale when the caller changed the block size.
        if old_block_size != 0.0 {
            self.update_lod_distances(old_block_size);
        }

        self.vertex_lod_start_bias = read_float(settings, "lodInfo/startBias");
        self.vertex_lod_end_bias = read_float(settings, "lodInfo/endBias");

        self.lod_texture_start = read_float(settings, "lodInfo/lodTextureStart");
        self.lod_texture_distance = read_float(settings, "lodInfo/lodTextureDistance");
        self.blend_preload_distance = read_float(settings, "lodInfo/blendPreloadDistance");

        // Normal map lod, with the start clamp
        // (`:247-248`): normalStart >= textureStart + textureDistance.
        self.lod_normal_start = read_float(settings, "lodInfo/lodNormalStart");
        self.lod_normal_start = self
            .lod_normal_start
            .max(self.lod_texture_start + self.lod_texture_distance);
        self.lod_normal_distance = read_float(settings, "lodInfo/lodNormalDistance");
        self.normal_preload_distance = read_float(settings, "lodInfo/normalPreloadDistance");

        self.bump_fading_start = read_float(settings, "lodInfo/bumpFadingStart");
        self.bump_fading_distance = read_float(settings, "lodInfo/bumpFadingDistance");

        self.default_height_map_lod = self.num_vertex_lods
            - log2_round_up(self.height_map_size.min(self.height_map_editor_size));
        self.detail_height_map_distance = read_float(settings, "lodInfo/detailHeightMapDistance");

        // `EDITOR_ENABLED` (`:303-309`): the base height map lod setting.
        self.server_height_map_lod = self.default_height_map_lod;

        self.read_sound_occlusions(settings);
        true
    }

    /// The two `soundOcclusion` floats every init path reads
    /// (`terrain_settings.cpp:294-297`).
    fn read_sound_occlusions(&mut self, settings: Option<&DataSection>) {
        self.direct_sound_occlusion = read_float(settings, "soundOcclusion/directOcclusion");
        self.reverb_sound_occlusion = read_float(settings, "soundOcclusion/reverbOcclusion");
    }

    /// `TerrainSettings::save` (`terrain_settings.cpp:431-483`): drop
    /// every child, write the version, and for terrain 2 the sizes and
    /// the `lodInfo` section — `heightMapEditorSize` only when it differs
    /// from `heightMapSize` — then the two sound occlusion floats.
    pub fn save(&self, section: &mut DataSection) {
        while section.del_child_at(0) {}

        section.write_int("version", self.version as i32);

        if self.version == TERRAIN2_VERSION {
            section.write_int("lodMapSize", self.lod_map_size as i32);
            section.write_int("aoMapSize", self.ao_map_size as i32);

            if self.height_map_editor_size != self.height_map_size {
                section.write_int("heightMapEditorSize", self.height_map_editor_size as i32);
            }

            section.write_int("heightMapSize", self.height_map_size as i32);
            section.write_int("normalMapSize", self.normal_map_size as i32);
            section.write_int("holeMapSize", self.hole_map_size as i32);
            section.write_int("shadowMapSize", self.shadow_map_size as i32);
            section.write_int("blendMapSize", self.blend_map_size as i32);

            let lod = section.add_child("lodInfo");
            lod.write_float("startBias", self.vertex_lod_start_bias);
            lod.write_float("endBias", self.vertex_lod_end_bias);
            lod.write_float("lodTextureStart", self.lod_texture_start);
            lod.write_float("lodTextureDistance", self.lod_texture_distance);
            lod.write_float("blendPreloadDistance", self.blend_preload_distance);
            lod.write_float("bumpFadingStart", self.bump_fading_start);
            lod.write_float("bumpFadingDistance", self.bump_fading_distance);
            lod.write_float("lodNormalStart", self.lod_normal_start);
            lod.write_float("lodNormalDistance", self.lod_normal_distance);
            lod.write_float("normalPreloadDistance", self.normal_preload_distance);
            lod.write_float("detailHeightMapDistance", self.detail_height_map_distance);

            // `saveLevelDistances` (`:89-100`).
            for (level, distance) in self.vertex_lod_distances.iter().enumerate() {
                lod.write_float(&format!("lodDistances/distance{level}"), *distance);
            }

            lod.write_int("server/heightMapLod", self.server_height_map_lod as i32);
        }

        section.write_float(
            "soundOcclusion/directOcclusion",
            self.direct_sound_occlusion,
        );
        section.write_float(
            "soundOcclusion/reverbOcclusion",
            self.reverb_sound_occlusion,
        );
    }

    /// `TerrainSettings::heightMapSize(value)` (`:501-510`): store, then
    /// recalculate the vertex-lod count and rescale the distances.
    pub fn set_height_map_size(&mut self, value: u32) {
        self.height_map_size = value;
        self.recalculate_vertex_lods();
    }

    /// `TerrainSettings::heightMapEditorSize(value)` (`:511-520`).
    pub fn set_height_map_editor_size(&mut self, value: u32) {
        self.height_map_editor_size = value;
        self.recalculate_vertex_lods();
    }

    /// The shared tail of the two height-map size setters.
    fn recalculate_vertex_lods(&mut self) {
        self.num_vertex_lods = log2_round_up(self.height_map_size.max(self.height_map_editor_size));
        let block_size = self.block_size;
        self.update_lod_distances(block_size);
    }

    /// `TerrainSettings::version(value)` (`:488-491`).
    pub fn set_version(&mut self, value: u32) {
        self.version = value;
    }

    /// `TerrainSettings::lodMapSize(value)` (`:493-496`).
    pub fn set_lod_map_size(&mut self, value: u32) {
        self.lod_map_size = value;
    }

    /// `TerrainSettings::aoMapSize(value)` (`:498-501`).
    pub fn set_ao_map_size(&mut self, value: u32) {
        self.ao_map_size = value;
    }

    /// `TerrainSettings::normalMapSize(value)` (`:521-524`).
    pub fn set_normal_map_size(&mut self, value: u32) {
        self.normal_map_size = value;
    }

    /// `TerrainSettings::holeMapSize(value)` (`:525-528`).
    pub fn set_hole_map_size(&mut self, value: u32) {
        self.hole_map_size = value;
    }

    /// `TerrainSettings::shadowMapSize(value)` (`:529-532`).
    pub fn set_shadow_map_size(&mut self, value: u32) {
        self.shadow_map_size = value;
    }

    /// `TerrainSettings::blendMapSize(value)` (`:533-536`).
    pub fn set_blend_map_size(&mut self, value: u32) {
        self.blend_map_size = value;
    }

    /// `TerrainSettings::updateLodDistances` (`terrain_settings.cpp:576-618`):
    /// scale every saved distance by new/old, resize to `numVertexLods - 1`
    /// rows, and fill brand-new rows from the defaults resource scaled by
    /// new/`DEFAULT_BLOCK_SIZE`. The defaults resource (`system/terrain2`)
    /// is not in the source drop, so new rows read zero — the same answer
    /// a NULL defaults section gives.
    pub fn update_lod_distances(&mut self, old_block_size: f32) {
        let old_block_size = if old_block_size == 0.0 {
            DEFAULT_BLOCK_SIZE
        } else {
            old_block_size
        };
        let new_block_size = if self.block_size > 0.0 {
            self.block_size
        } else {
            DEFAULT_BLOCK_SIZE
        };
        let ratio = new_block_size / old_block_size;
        let old_saved_lods = self.vertex_lod_distances.len() as u32;
        let scaled_lods = self.num_vertex_lods.saturating_sub(1).min(old_saved_lods);

        for distance in self.vertex_lod_distances[..scaled_lods as usize].iter_mut() {
            *distance *= ratio;
        }

        let saved_lods = self.num_vertex_lods.saturating_sub(1);
        self.vertex_lod_distances.resize(saved_lods as usize, 0.0);

        if old_saved_lods < saved_lods {
            let ratio = new_block_size / DEFAULT_BLOCK_SIZE;
            for row in
                self.vertex_lod_distances[old_saved_lods as usize..saved_lods as usize].iter_mut()
            {
                *row = 0.0 * ratio;
            }
        }
    }

    pub fn block_size(&self) -> f32 {
        self.block_size
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn lod_map_size(&self) -> u32 {
        self.lod_map_size
    }

    pub fn ao_map_size(&self) -> u32 {
        self.ao_map_size
    }

    pub fn height_map_editor_size(&self) -> u32 {
        self.height_map_editor_size
    }

    pub fn height_map_size(&self) -> u32 {
        self.height_map_size
    }

    pub fn normal_map_size(&self) -> u32 {
        self.normal_map_size
    }

    pub fn hole_map_size(&self) -> u32 {
        self.hole_map_size
    }

    pub fn shadow_map_size(&self) -> u32 {
        self.shadow_map_size
    }

    pub fn blend_map_size(&self) -> u32 {
        self.blend_map_size
    }

    pub fn num_vertex_lods(&self) -> u32 {
        self.num_vertex_lods
    }

    pub fn vertex_lod_distances(&self) -> &[f32] {
        &self.vertex_lod_distances
    }

    pub fn vertex_lod_start_bias(&self) -> f32 {
        self.vertex_lod_start_bias
    }

    pub fn vertex_lod_end_bias(&self) -> f32 {
        self.vertex_lod_end_bias
    }

    pub fn lod_texture_start(&self) -> f32 {
        self.lod_texture_start
    }

    pub fn lod_texture_distance(&self) -> f32 {
        self.lod_texture_distance
    }

    pub fn blend_preload_distance(&self) -> f32 {
        self.blend_preload_distance
    }

    pub fn bump_fading_start(&self) -> f32 {
        self.bump_fading_start
    }

    pub fn bump_fading_distance(&self) -> f32 {
        self.bump_fading_distance
    }

    pub fn lod_normal_start(&self) -> f32 {
        self.lod_normal_start
    }

    pub fn lod_normal_distance(&self) -> f32 {
        self.lod_normal_distance
    }

    pub fn normal_preload_distance(&self) -> f32 {
        self.normal_preload_distance
    }

    pub fn default_height_map_lod(&self) -> u32 {
        self.default_height_map_lod
    }

    pub fn detail_height_map_distance(&self) -> f32 {
        self.detail_height_map_distance
    }

    pub fn server_height_map_lod(&self) -> u32 {
        self.server_height_map_lod
    }

    pub fn direct_sound_occlusion(&self) -> f32 {
        self.direct_sound_occlusion
    }

    pub fn reverb_sound_occlusion(&self) -> f32 {
        self.reverb_sound_occlusion
    }
}

/// `log2RoundUp` (`terrain_settings.cpp:22-29`): the smallest `n` with
/// `2^n >= x`.
fn log2_round_up(x: u32) -> u32 {
    let mut result = 0;
    while (x >> result) > 1 {
        result += 1;
    }
    result
}

/// `readIntSetting` with a NULL defaults section
/// (`terrain_settings.cpp:33-42`): the bare read, zero when absent.
fn read_int(settings: Option<&DataSection>, key: &str) -> i32 {
    settings
        .and_then(|section| section.open_section(key))
        .map_or(0, |leaf| leaf.as_int(0))
}

/// `readFloatSetting` with a NULL defaults section (`:45-54`).
fn read_float(settings: Option<&DataSection>, key: &str) -> f32 {
    settings
        .and_then(|section| section.open_section(key))
        .map_or(0.0, |leaf| leaf.as_float(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(xml: &str) -> DataSection {
        resources::datasection::parse("root", xml.as_bytes()).expect("fixture parses")
    }

    #[test]
    fn init_without_a_section_assumes_terrain1_and_fails() {
        let mut settings = TerrainSettings::new();
        assert!(!settings.init(100.0, None, 0.0));
        assert_eq!(settings.version(), TERRAIN1_VERSION);
        assert_eq!(settings.block_size(), 100.0);
    }

    #[test]
    fn init_reads_sizes_and_derives_vertex_lods() {
        let source = doc("<root>\
                <version>200</version>\
                <lodMapSize>256</lodMapSize>\
                <aoMapSize>128</aoMapSize>\
                <heightMapSize>128</heightMapSize>\
                <heightMapEditorSize>64</heightMapEditorSize>\
                <normalMapSize>256</normalMapSize>\
                <holeMapSize>64</holeMapSize>\
                <shadowMapSize>32</shadowMapSize>\
                <blendMapSize>32</blendMapSize>\
                <lodDistances>\
                    <distance0>300.0</distance0>\
                    <distance1>600.0</distance1>\
                </lodDistances>\
                <lodInfo>\
                    <startBias>1.0</startBias>\
                    <endBias>1.0</endBias>\
                    <lodTextureStart>30.0</lodTextureStart>\
                    <lodTextureDistance>100.0</lodTextureDistance>\
                    <blendPreloadDistance>150.0</blendPreloadDistance>\
                    <bumpFadingStart>20.0</bumpFadingStart>\
                    <bumpFadingDistance>50.0</bumpFadingDistance>\
                    <lodNormalStart>120.0</lodNormalStart>\
                    <lodNormalDistance>200.0</lodNormalDistance>\
                    <normalPreloadDistance>180.0</normalPreloadDistance>\
                    <detailHeightMapDistance>250.0</detailHeightMapDistance>\
                </lodInfo>\
                <soundOcclusion>\
                    <directOcclusion>0.25</directOcclusion>\
                    <reverbOcclusion>0.5</reverbOcclusion>\
                </soundOcclusion>\
            </root>");
        let mut settings = TerrainSettings::new();
        assert!(settings.init(100.0, Some(&source), 0.0));
        assert_eq!(settings.version(), TERRAIN2_VERSION);
        assert_eq!(settings.lod_map_size(), 256);
        assert_eq!(settings.ao_map_size(), 128);
        assert_eq!(settings.height_map_size(), 128);
        assert_eq!(settings.height_map_editor_size(), 64);
        assert_eq!(settings.normal_map_size(), 256);
        assert_eq!(settings.hole_map_size(), 64);
        assert_eq!(settings.shadow_map_size(), 32);
        assert_eq!(settings.blend_map_size(), 32);
        // log2RoundUp(max(128, 64)) == 7, and the editor face derives
        // serverHeightMapLod from it.
        assert_eq!(settings.num_vertex_lods(), 7);
        assert_eq!(settings.default_height_map_lod(), 1);
        assert_eq!(settings.server_height_map_lod(), 1);
        // One distance per vertex lod; the missing rows read zero.
        assert_eq!(
            settings.vertex_lod_distances(),
            &[300.0, 600.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(settings.vertex_lod_start_bias(), 1.0);
        assert_eq!(settings.vertex_lod_end_bias(), 1.0);
        assert_eq!(settings.lod_texture_start(), 30.0);
        assert_eq!(settings.lod_texture_distance(), 100.0);
        assert_eq!(settings.blend_preload_distance(), 150.0);
        assert_eq!(settings.bump_fading_start(), 20.0);
        assert_eq!(settings.bump_fading_distance(), 50.0);
        // The normal start clamp: 120 < 30 + 100, so it lifts to 130.
        assert_eq!(settings.lod_normal_start(), 130.0);
        assert_eq!(settings.lod_normal_distance(), 200.0);
        assert_eq!(settings.normal_preload_distance(), 180.0);
        assert_eq!(settings.detail_height_map_distance(), 250.0);
        assert_eq!(settings.direct_sound_occlusion(), 0.25);
        assert_eq!(settings.reverb_sound_occlusion(), 0.5);
    }

    #[test]
    fn an_unsupported_version_still_reads_sound_occlusions() {
        let source = doc("<root>\
                <version>100</version>\
                <soundOcclusion><directOcclusion>0.5</directOcclusion></soundOcclusion>\
            </root>");
        let mut settings = TerrainSettings::new();
        assert!(!settings.init(100.0, Some(&source), 0.0));
        assert_eq!(settings.direct_sound_occlusion(), 0.5);
        assert_eq!(settings.reverb_sound_occlusion(), 0.0);
    }

    #[test]
    fn init_clamps_the_normal_start_to_the_texture_reach() {
        let source = doc("<root>\
                <version>200</version>\
                <heightMapSize>128</heightMapSize>\
                <lodInfo>\
                    <lodTextureStart>40.0</lodTextureStart>\
                    <lodTextureDistance>100.0</lodTextureDistance>\
                    <lodNormalStart>50.0</lodNormalStart>\
                </lodInfo>\
            </root>");
        let mut settings = TerrainSettings::new();
        assert!(settings.init(100.0, Some(&source), 0.0));
        assert_eq!(settings.lod_normal_start(), 140.0);
    }

    #[test]
    fn save_round_trips_through_init_except_the_distance_location() {
        let mut settings = TerrainSettings::new();
        let source = doc("<root>\
                <version>200</version>\
                <heightMapSize>128</heightMapSize>\
                <lodDistances>\
                    <distance0>300.0</distance0>\
                    <distance1>600.0</distance1>\
                </lodDistances>\
                <lodInfo><lodTextureStart>30.0</lodTextureStart></lodInfo>\
                <soundOcclusion><reverbOcclusion>0.75</reverbOcclusion></soundOcclusion>\
            </root>");
        assert!(settings.init(100.0, Some(&source), 0.0));

        let mut saved = DataSection::new("terrain");
        settings.save(&mut saved);
        let mut reloaded = TerrainSettings::new();
        assert!(reloaded.init(100.0, Some(&saved), 0.0));

        // The reference asymmetry, ported verbatim: `init` reads the
        // lod distances from the terrain section's own
        // `lodDistances/distance*` children (`terrain_settings.cpp:97`),
        // while `save` writes them under `lodInfo/lodDistances/distance*`
        // (`:79-84`), so a save/init round trip resets the distances.
        assert_eq!(
            reloaded.vertex_lod_distances(),
            vec![0.0; settings.vertex_lod_distances().len()]
        );

        // Every other field survives.
        assert_eq!(reloaded.block_size(), 100.0);
        assert_eq!(reloaded.version(), TERRAIN2_VERSION);
        assert_eq!(reloaded.height_map_size(), 128);
        assert_eq!(reloaded.height_map_editor_size(), 128);
        assert_eq!(reloaded.lod_texture_start(), 30.0);
        assert_eq!(reloaded.reverb_sound_occlusion(), 0.75);
    }

    #[test]
    fn save_omits_editor_size_when_it_matches_height_map_size() {
        let mut settings = TerrainSettings::new();
        settings.set_version(TERRAIN2_VERSION);
        settings.set_height_map_size(128);
        // `heightMapEditorSize` still 0 — the save writes it since 0 != 128.
        let mut saved = DataSection::new("terrain");
        settings.save(&mut saved);
        assert!(saved.open_section("heightMapEditorSize").is_some());

        // Matching sizes drop the editor size key.
        settings.set_height_map_editor_size(128);
        let mut saved = DataSection::new("terrain");
        settings.save(&mut saved);
        assert!(saved.open_section("heightMapEditorSize").is_none());
        assert!(saved.open_section("heightMapSize").is_some());
    }

    #[test]
    fn save_of_terrain1_writes_version_and_occlusions_only() {
        let mut settings = TerrainSettings::new();
        settings.set_version(TERRAIN1_VERSION);
        settings.set_height_map_size(128);
        let mut saved = DataSection::new("terrain");
        settings.save(&mut saved);
        // version + the one soundOcclusion section carrying both floats.
        assert_eq!(saved.count_children(), 2);
        assert!(saved.open_section("heightMapSize").is_none());
        assert!(saved.open_section("lodInfo").is_none());
    }

    #[test]
    fn set_height_map_size_rescales_saved_distances() {
        let mut settings = TerrainSettings::new();
        settings.set_version(TERRAIN2_VERSION);
        // init via a saved section so distances exist.
        let source = doc("<root>\
                <version>200</version>\
                <heightMapSize>64</heightMapSize>\
                <heightMapEditorSize>64</heightMapEditorSize>\
                <lodDistances>\
                    <distance0>100.0</distance0>\
                    <distance1>200.0</distance1>\
                </lodDistances>\
            </root>");
        assert!(settings.init(200.0, Some(&source), 0.0));
        // One row per vertex lod (log2RoundUp(64) == 6), zeros beyond
        // the two written rows.
        assert_eq!(
            settings.vertex_lod_distances(),
            &[100.0, 200.0, 0.0, 0.0, 0.0, 0.0]
        );
        // The setter path scales by new/old == blockSize/blockSize == 1,
        // so the rows survive; only the lod count grows.
        settings.set_height_map_size(128);
        assert_eq!(settings.num_vertex_lods(), 7);
        assert_eq!(
            settings.vertex_lod_distances(),
            &[100.0, 200.0, 0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn init_rescales_saved_distances_when_the_block_size_changed() {
        // old_block_size != 0 makes init run updateLodDistances.
        let source = doc("<root>\
                <version>200</version>\
                <heightMapSize>64</heightMapSize>\
                <lodDistances>\
                    <distance0>100.0</distance0>\
                    <distance1>200.0</distance1>\
                </lodDistances>\
            </root>");
        let mut settings = TerrainSettings::new();
        assert!(settings.init(400.0, Some(&source), 200.0));
        // ratio = 400/200 = 2 on the saved rows, then resize to 5 rows.
        assert_eq!(
            settings.vertex_lod_distances(),
            &[200.0, 400.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn set_height_map_editor_size_participates_in_the_lod_derivation() {
        let mut settings = TerrainSettings::new();
        settings.set_version(TERRAIN2_VERSION);
        settings.set_height_map_size(64);
        assert_eq!(settings.num_vertex_lods(), 6);
        settings.set_height_map_editor_size(128);
        assert_eq!(settings.num_vertex_lods(), 7);
    }

    #[test]
    fn update_lod_distances_falls_back_to_the_default_block_size() {
        // No block size: the distance scale falls back to DEFAULT_BLOCK_SIZE.
        let mut settings = TerrainSettings::new();
        settings.block_size = 0.0;
        settings.num_vertex_lods = 3;
        settings.vertex_lod_distances = vec![10.0, 20.0];
        settings.update_lod_distances(0.0);
        // new/old == DEFAULT/DEFAULT == 1, resize to numVertexLods-1 = 2.
        assert_eq!(settings.vertex_lod_distances(), &[10.0, 20.0]);
    }

    #[test]
    fn update_lod_distances_shrinks_and_grows() {
        let mut settings = TerrainSettings::new();
        settings.block_size = 100.0;
        settings.num_vertex_lods = 3;
        settings.vertex_lod_distances = vec![100.0, 200.0, 300.0];
        // Shrink to two rows at half the block size.
        settings.update_lod_distances(200.0);
        assert_eq!(settings.vertex_lod_distances(), &[50.0, 100.0]);
        // Grow back at twice the block size: the third row comes from the
        // absent defaults (zero).
        settings.num_vertex_lods = 4;
        settings.update_lod_distances(50.0);
        assert_eq!(settings.vertex_lod_distances(), &[100.0, 200.0, 0.0]);
    }

    #[test]
    fn plain_setters_store_their_values() {
        let mut settings = TerrainSettings::new();
        settings.set_version(TERRAIN2_VERSION);
        settings.set_lod_map_size(64);
        settings.set_ao_map_size(32);
        settings.set_normal_map_size(128);
        settings.set_hole_map_size(16);
        settings.set_shadow_map_size(8);
        settings.set_blend_map_size(4);
        assert_eq!(settings.version(), TERRAIN2_VERSION);
        assert_eq!(settings.lod_map_size(), 64);
        assert_eq!(settings.ao_map_size(), 32);
        assert_eq!(settings.normal_map_size(), 128);
        assert_eq!(settings.hole_map_size(), 16);
        assert_eq!(settings.shadow_map_size(), 8);
        assert_eq!(settings.blend_map_size(), 4);
    }

    #[test]
    fn log2_round_up_matches_the_reference_loop() {
        assert_eq!(log2_round_up(1), 0);
        assert_eq!(log2_round_up(2), 1);
        assert_eq!(log2_round_up(3), 1);
        assert_eq!(log2_round_up(4), 2);
        assert_eq!(log2_round_up(64), 6);
        assert_eq!(log2_round_up(128), 7);
    }
}
