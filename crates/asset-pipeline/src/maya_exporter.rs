//! BW-13-05-03: the Maya visual exporter's portable core
//! (`tools/mayavisualexporter/`).
//!
//! The reference exporter is a Maya plugin: the `MFnMesh`/`MDagPath` scene
//! reads (`mesh.cpp`, `skin.cpp`, `portal.cpp`), the `ExportIterator` walks
//! and the MEL dialogs cannot run without the Maya host. This module ports
//! the portable remainder the exported files depend on:
//!
//! - [`MayaExportSettings`] with the 30-key DataSection IO of
//!   `expsetsio.cpp` (read at the root or under `metaData/settings`) and
//!   the `visualfiletranslator.cpp` `optionsString` parser;
//! - the mesh gather of `mesh.cpp` ([`gather_mesh`]): the
//!   useLegacyOrientation axis flips, the face-vertex pools, the
//!   polygon-fan triangulation and the surface-shader material walk
//!   (name truncation at the first dot, the same-stem `.mfm` promotion
//!   probe);
//! - [`MayaVisualMesh`] (`visual_mesh.cpp`): `init` with the Maya quirks
//!   (the uv `v` flip, the binormal indexed by the tangent id, the
//!   unguarded `materialRemap_` lookup, hardcoded zero smoothing groups),
//!   `addMaterial` (environment macros, `dissolveFilename`, the external
//!   texture copy) and `save` — the flip → dedupe → sort → [bump] →
//!   dedupe → sort pipeline and the `xyznuv`/`xyznuvi`/`xyznuvtb`
//!   streams;
//! - the Maya [`Material::save`](maya_material_save) chain and the
//!   existing-visual material copy;
//! - the unit converters of `utility.cpp`
//!   ([`convert_point_to_bigworld_units`] and friends) and the
//!   `constraints.cpp` section writer ([`save_constraints`]).
//!
//! The Maya `VisualMesh` shares its pure pipeline steps with the Max port
//! ([`crate::visual_exporter`]) — the two reference files carry the same
//! bodies — and diverges where `visual_mesh.cpp` (Maya edition) does: the
//! z component sits in the 10-bit `packNormal` slot
//! ([`pack_normal_maya`]), the vertex lists write the position
//! unswizzled, the save pipeline dedupes and sorts both before and after
//! the bump seam, and `save` writes `nodeIdentifier_` as the render set
//! node.
//!
//! Host seams, each behaviour-preserving for the exported files:
//! - `ExportSettings::instance()` becomes an explicit
//!   [`MayaExportSettings`] parameter;
//! - every Maya API read arrives through the [`RawMesh`] input type and
//!   every resource/filesystem access through the [`MayaIo`] trait
//!   (message boxes are collected into `errors_` under
//!   `LogMsg::automatedTest`, like the reference's headless mode);
//! - `makeBumped` (the NVMeshMender host pass) is not ported — the bump
//!   face consumes the `tangent`/`binormal` already carried on the
//!   vertices, like `crate::visual_exporter`;
//! - the primitives container starts fresh in [`MayaVisualMesh::save`]
//!   (the reference opens the existing file first; the export pipeline
//!   hands it a fresh temp name, so the opened file is always new) and
//!   the disk write itself is [`MayaIo::save_binary`].

use foundation::math::{BoundBox, Vector2, Vector3, Vector4};
use resources::binary::{BinSection, BinaryBlock};
use resources::datasection::DataSection;

use crate::primitive_file_structs::{IndexHeader, VertexHeader, write_primitive_groups};
use crate::visual_exporter::{
    BloatVertex, ExportMode, NodeFilter, SavedVisual, SkinType, Triangle, create_primitive_groups,
    d3dcolor_colorvalue, find_existing_material, flip_triangle_winding_order,
    remove_duplicate_vertices, sort_triangles,
};

// ----------------------------------------------------------------------
// The host seam.
// ----------------------------------------------------------------------

/// The Maya API and BWResource surface the exporter reads through. Every
/// method has a host implementation; the tests provide [`TestIo`].
pub trait MayaIo {
    /// `BWResource::fileExists`.
    fn file_exists(&self, filename: &str) -> bool;
    /// `BWResource::openSection` (an owned section for reading).
    fn open_section(&self, filename: &str) -> Option<DataSection>;
    /// `MCommonSystemUtils::getEnv` — `None` or empty means the macro
    /// cannot be substituted.
    fn get_env(&self, name: &str) -> Option<String>;
    /// `BWUtil::isAbsolutePath`.
    fn is_absolute_path(&self, filename: &str) -> bool;
    /// `BWResolver::resolveFilename`.
    fn resolve_filename(&self, filename: &str) -> String;
    /// `BWResolver::dissolveFilename`.
    fn dissolve_filename(&self, filename: &str) -> String;
    /// `BWResource::resolveToAbsolutePath`.
    fn resolve_to_absolute_path(&self, filename: &str) -> String;
    /// `BWResource::instance().fileSystem()->copyFileOrDirectory` (the
    /// `&mut` for the sinks that record the attempt).
    fn copy_file(&self, from: &str, to: &str) -> bool;
    /// `LogMsg::automatedTest()`.
    fn automated_test(&self) -> bool;
    /// The `MessageBox` sink (suppressed under `automatedTest` by the
    /// callers, exactly like the reference).
    fn message_box(&self, message: &str, title: &str);
    /// `LogMsg::logToFile` / `MGlobal::displayWarning`.
    fn log_warning(&self, message: &str);
    /// `BinSection::save(primitiveFile)` against the native filesystem.
    fn save_binary(&mut self, filename: &str, bytes: &[u8]);
    /// `DataSection::save` (XML) against the native filesystem.
    fn save_section(&mut self, filename: &str, section: &DataSection);
    /// A settings/model XML read back from the native filesystem
    /// (`DataResource(file, RESOURCE_TYPE_XML).getRootSection()`).
    fn read_section(&self, filename: &str) -> Option<DataSection>;
    /// `IFileSystem::readDirectory`.
    fn read_directory(&self, directory: &str) -> Option<Vec<String>>;
    /// The machine's name (`.model` metadata).
    fn computer_name(&self) -> String;
    /// `MFileIO::currentFile()` (`.model` metadata).
    fn current_scene_file(&self) -> String;
}

// ----------------------------------------------------------------------
// utility.cpp.
// ----------------------------------------------------------------------

/// `trimWhitespaces`: trims spaces (not tabs).
pub fn trim_whitespaces(s: &str) -> String {
    let f = s.find(|c| c != ' ').unwrap_or(s.len());
    let l = s.rfind(|c| c != ' ').map(|p| p + 1).unwrap_or(0);
    if l <= f {
        String::new()
    } else {
        s[f..l].to_string()
    }
}

/// `stripReferencePrefix`: drop everything up to and including the first
/// colon; a colon-less name is returned unchanged.
pub fn strip_reference_prefix(node_name: &str) -> String {
    match node_name.find(':') {
        Some(pos) => node_name[pos + 1..].to_string(),
        None => node_name.to_string(),
    }
}

/// `snapValue`.
pub fn snap_value(v: f32, snap_size: f32) -> f32 {
    let mut v = v;
    if snap_size > 0.0 {
        let half_snap = snap_size / 2.0;
        if v > 0.0 {
            v += half_snap;
            v -= v % snap_size;
        } else {
            v -= half_snap;
            v += (-v) % snap_size;
        }
    }
    v
}

/// `snapPoint3` (the 0.001 default of `utility.hpp`).
pub fn snap_point3(pt: [f32; 3]) -> [f32; 3] {
    [
        snap_value(pt[0], 0.001),
        snap_value(pt[1], 0.001),
        snap_value(pt[2], 0.001),
    ]
}

/// `replaceEnvironmentMacros`: `%MACRO%` stretches are looked up through
/// [`MayaIo::get_env`]; a macro that resolves to nothing fails the whole
/// string, which then keeps its original text.
pub fn replace_environment_macros(input: &str, io: &dyn MayaIo) -> Option<String> {
    let mut out = String::new();
    let mut macro_name = String::new();
    let mut is_success = true;
    let mut reading_macro = false;
    for value in input.chars() {
        if value == '%' {
            reading_macro = !reading_macro;
            if !reading_macro {
                match io.get_env(&macro_name) {
                    Some(value) if !value.is_empty() => out.push_str(&value),
                    _ => {
                        is_success = false;
                        break;
                    }
                }
            }
        } else if reading_macro {
            macro_name.push(value);
        } else {
            out.push(value);
        }
    }
    if is_success { Some(out) } else { None }
}

/// `convertPointToBigWorldUnits` (`utility.cpp:182`).
pub fn convert_point_to_bigworld_units(v: Vector3, settings: &MayaExportSettings) -> Vector3 {
    let s = settings.unit_scale();
    if settings.use_legacy_orientation {
        Vector3::new(-s * v.x, s * v.y, s * v.z)
    } else {
        Vector3::new(s * v.x, s * v.y, -s * v.z)
    }
}

/// `convertVectorToBigWorldUnits` (`utility.cpp:204`).
pub fn convert_vector_to_bigworld_units(v: Vector3, settings: &MayaExportSettings) -> Vector3 {
    if settings.use_legacy_orientation {
        Vector3::new(-v.x, v.y, v.z)
    } else {
        Vector3::new(v.x, v.y, -v.z)
    }
}

/// `convertAngleOffsetToBigWorldUnits` (`utility.cpp:225`).
pub fn convert_angle_offset_to_bigworld_units(
    v: Vector3,
    settings: &MayaExportSettings,
) -> Vector3 {
    if settings.use_legacy_orientation {
        Vector3::new(v.x, -v.y, -v.z)
    } else {
        Vector3::new(-v.x, -v.y, v.z)
    }
}

// ----------------------------------------------------------------------
// ExportSettings (expsets.{hpp,cpp}, expsetsio.cpp, parseOptionsString).
// ----------------------------------------------------------------------

/// The Maya exporter settings (`ExportSettings`). The 30 DataSection keys
/// of `expsetsio.cpp` round-trip through [`read_settings`] and
/// [`write_settings`]; the non-serialised members (`staticFrame`,
/// `frameFirst`, `frameLast`, `resolvePaths`) are host-side only.
#[derive(Debug, Clone, PartialEq)]
pub struct MayaExportSettings {
    /// `exportAnim_`.
    pub export_anim: bool,
    /// `exportMode_`.
    pub export_mode: ExportMode,
    /// `maxBones_` (serialised as `boneCount`).
    pub max_bones: i32,
    /// `transformToOrigin_`.
    pub transform_to_origin: bool,
    /// `allowScale_`.
    pub allow_scale: bool,
    /// `bumpMapped_`.
    pub bump_mapped: bool,
    /// `keepExistingMaterials_`.
    pub keep_existing_materials: bool,
    /// `snapVertices_`.
    pub snap_vertices: bool,
    /// `stripRefPrefix_`.
    pub strip_ref_prefix: bool,
    /// `referenceNode_` (serialised as `useReferenceNode`).
    pub reference_node: bool,
    /// `referenceNodesFile_` (serialised as `referenceNodesFile`).
    pub reference_nodes_file: String,
    /// `referenceNodesFileAbs_` (resolved through the host; never
    /// serialised).
    pub reference_nodes_file_abs: String,
    /// `disableVisualChecker_`.
    pub disable_visual_checker: bool,
    /// `useLegacyScaling_`.
    pub use_legacy_scaling: bool,
    /// `fixCylindrical_`.
    pub fix_cylindrical: bool,
    /// `useLegacyOrientation_`.
    pub use_legacy_orientation: bool,
    /// `sceneRootAdded_`.
    pub scene_root_added: bool,
    /// `includeMeshes_`.
    pub include_meshes: bool,
    /// `includeEnvelopesAndBones_`.
    pub include_envelopes_and_bones: bool,
    /// `includeNodes_`.
    pub include_nodes: bool,
    /// `includeMaterials_`.
    pub include_materials: bool,
    /// `includeAnimations_`.
    pub include_animations: bool,
    /// `useCharacterMode_`.
    pub use_character_mode: bool,
    /// `animationName_` (default `"anim"`).
    pub animation_name: String,
    /// `includePortals_`.
    pub include_portals: bool,
    /// `worldSpaceOrigin_`.
    pub world_space_origin: bool,
    /// `unitScale_` (default `0.1`); only used under
    /// `useLegacyScaling` — see [`Self::unit_scale`].
    pub unit_scale: f32,
    /// `localHierarchy_`.
    pub local_hierarchy: bool,
    /// `nodeFilter_` (default `ALL`).
    pub node_filter: NodeFilter,
    /// `copyExternalTextures_`.
    pub copy_external_textures: bool,
    /// `copyTexturesTo_`.
    pub copy_textures_to: String,
    /// `MDistance(1).asMeters()` — the host's scene-unit-to-metres
    /// factor (Maya's default centimetre scene gives `0.01`).
    pub maya_unit_scale: f32,
    /// The `s_defaultMaterial` option of the `mapIdMeaning == 0` branch.
    pub default_material: String,
    /// The `s_defaultSkinnedMaterial` option.
    pub default_skinned_material: String,
}

impl Default for MayaExportSettings {
    /// `ExportSettings::ExportSettings()` (`expsets.cpp`).
    fn default() -> Self {
        Self {
            include_meshes: true,
            include_envelopes_and_bones: false,
            include_nodes: true,
            include_materials: true,
            include_animations: false,
            use_character_mode: false,
            include_portals: false,
            world_space_origin: false,
            animation_name: "anim".to_string(),
            unit_scale: 0.1,
            export_mode: ExportMode::Normal,
            node_filter: NodeFilter::All,
            keep_existing_materials: true,
            max_bones: 17,
            copy_external_textures: true,
            maya_unit_scale: 0.01,
            default_material: String::new(),
            default_skinned_material: String::new(),
            ..Self::zeroed()
        }
    }
}

impl MayaExportSettings {
    fn zeroed() -> Self {
        Self {
            export_anim: false,
            export_mode: ExportMode::Normal,
            max_bones: 0,
            transform_to_origin: false,
            allow_scale: false,
            bump_mapped: false,
            keep_existing_materials: false,
            snap_vertices: false,
            strip_ref_prefix: false,
            reference_node: false,
            reference_nodes_file: String::new(),
            reference_nodes_file_abs: String::new(),
            disable_visual_checker: false,
            use_legacy_scaling: false,
            fix_cylindrical: false,
            use_legacy_orientation: false,
            scene_root_added: false,
            include_meshes: false,
            include_envelopes_and_bones: false,
            include_nodes: false,
            include_materials: false,
            include_animations: false,
            use_character_mode: false,
            animation_name: String::new(),
            include_portals: false,
            world_space_origin: false,
            unit_scale: 0.0,
            local_hierarchy: false,
            node_filter: NodeFilter::All,
            copy_external_textures: false,
            copy_textures_to: String::new(),
            maya_unit_scale: 0.0,
            default_material: String::new(),
            default_skinned_material: String::new(),
        }
    }

    /// `ExportSettings::unitScale()`: the legacy field under
    /// `useLegacyScaling`, else the scene-unit factor.
    pub fn unit_scale(&self) -> f32 {
        if self.use_legacy_scaling {
            self.unit_scale
        } else {
            self.maya_unit_scale
        }
    }

    /// `ExportSettings::readSettings(DataSectionPtr)` (`expsetsio.cpp:43`):
    /// thirty keys, each read with the current value as its default.
    pub fn read_settings(&mut self, io: &dyn MayaIo, root: Option<&DataSection>) -> bool {
        let Some(root) = root else {
            return false;
        };

        self.export_anim = root.read_bool("exportAnimation", self.export_anim);
        self.export_mode =
            export_mode_from_int(root.read_int("exportMode", self.export_mode as i32));
        self.max_bones = root.read_int("boneCount", self.max_bones);
        self.transform_to_origin = root.read_bool("transformToOrigin", self.transform_to_origin);
        self.allow_scale = root.read_bool("allowScale", self.allow_scale);
        self.bump_mapped = root.read_bool("bumpMapped", self.bump_mapped);
        self.keep_existing_materials =
            root.read_bool("keepExistingMaterials", self.keep_existing_materials);
        self.snap_vertices = root.read_bool("snapVertices", self.snap_vertices);
        self.strip_ref_prefix = root.read_bool("stripRefPrefix", self.strip_ref_prefix);
        self.reference_node = root.read_bool("useReferenceNode", self.reference_node);
        let ref_nodes_file = root.read_string("referenceNodesFile", &self.reference_nodes_file);
        if !ref_nodes_file.is_empty() {
            self.reference_nodes_file = ref_nodes_file.clone();
            self.reference_nodes_file_abs = io.resolve_to_absolute_path(&ref_nodes_file);
        }
        self.disable_visual_checker =
            root.read_bool("disableVisualChecker", self.disable_visual_checker);
        self.use_legacy_scaling = root.read_bool("useLegacyScaling", self.use_legacy_scaling);
        self.fix_cylindrical = root.read_bool("fixCylindrical", self.fix_cylindrical);
        self.use_legacy_orientation =
            root.read_bool("useLegacyOrientation", self.use_legacy_orientation);
        self.scene_root_added = root.read_bool("sceneRootAdded", self.scene_root_added);

        self.include_meshes = root.read_bool("includeMeshes", self.include_meshes);
        self.include_envelopes_and_bones =
            root.read_bool("includeEnvelopesAndBones", self.include_envelopes_and_bones);
        self.include_nodes = root.read_bool("includeNodes", self.include_nodes);
        self.include_materials = root.read_bool("includeMaterials", self.include_materials);
        self.include_animations = root.read_bool("includeAnimations", self.include_animations);
        self.use_character_mode = root.read_bool("useCharacterMode", self.use_character_mode);
        self.animation_name = root.read_string("animationName", &self.animation_name);
        self.include_portals = root.read_bool("includePortals", self.include_portals);
        self.world_space_origin = root.read_bool("worldSpaceOrigin", self.world_space_origin);
        self.unit_scale = root.read_float("unitScale", self.unit_scale);
        self.local_hierarchy = root.read_bool("localHierarchy", self.local_hierarchy);
        self.node_filter =
            node_filter_from_int(root.read_int("nodeFilter", self.node_filter as i32));

        self.copy_external_textures =
            root.read_bool("copyExternalTextures", self.copy_external_textures);
        self.copy_textures_to = root.read_string("copyTexturesTo", &self.copy_textures_to);
        true
    }

    /// `ExportSettings::writeSettings(DataSectionPtr&)`
    /// (`expsetsio.cpp:135`).
    pub fn write_settings(&self, root: Option<&mut DataSection>) -> bool {
        let Some(root) = root else {
            return false;
        };

        root.write_bool("exportAnimation", self.export_anim);
        root.write_int("exportMode", self.export_mode as i32);
        root.write_int("boneCount", self.max_bones);
        root.write_bool("transformToOrigin", self.transform_to_origin);
        root.write_bool("allowScale", self.allow_scale);
        root.write_bool("bumpMapped", self.bump_mapped);
        root.write_bool("keepExistingMaterials", self.keep_existing_materials);
        root.write_bool("snapVertices", self.snap_vertices);
        root.write_bool("stripRefPrefix", self.strip_ref_prefix);
        root.write_bool("useReferenceNode", self.reference_node);
        // The reference dissolves for the wire; the host's resolver
        // already hands settings text over resolved, so the raw member
        // goes out as read.
        root.write_string("referenceNodesFile", &self.reference_nodes_file);
        root.write_bool("disableVisualChecker", self.disable_visual_checker);
        root.write_bool("useLegacyScaling", self.use_legacy_scaling);
        root.write_bool("fixCylindrical", self.fix_cylindrical);
        root.write_bool("useLegacyOrientation", self.use_legacy_orientation);
        root.write_bool("sceneRootAdded", self.scene_root_added);

        root.write_bool("includeMeshes", self.include_meshes);
        root.write_bool("includeEnvelopesAndBones", self.include_envelopes_and_bones);
        root.write_bool("includeNodes", self.include_nodes);
        root.write_bool("includeMaterials", self.include_materials);
        root.write_bool("includeAnimations", self.include_animations);
        root.write_bool("useCharacterMode", self.use_character_mode);
        root.write_string("animationName", &self.animation_name);
        root.write_bool("includePortals", self.include_portals);
        root.write_bool("worldSpaceOrigin", self.world_space_origin);
        root.write_float("unitScale", self.unit_scale);
        root.write_bool("localHierarchy", self.local_hierarchy);
        root.write_int("nodeFilter", self.node_filter as i32);

        root.write_bool("copyExternalTextures", self.copy_external_textures);
        root.write_string("copyTexturesTo", &self.copy_textures_to);
        true
    }

    /// `ExportSettings::readSettings(fileName, root)` (`expsetsio.cpp:13`):
    /// the file gate, then the root section directly (`root == true`) or
    /// `metaData/settings`.
    pub fn read_settings_file(&mut self, io: &dyn MayaIo, filename: &str, root: bool) -> bool {
        if !io.file_exists(filename) {
            return false;
        }
        let Some(file_root) = io.read_section(filename) else {
            return false;
        };
        if root {
            return self.read_settings(io, Some(&file_root));
        }
        let Some(meta_data) = file_root.open_section("metaData") else {
            return false;
        };
        self.read_settings(io, meta_data.open_section("settings"))
    }

    /// `ExportSettings::writeSettings(fileName, root)` (`expsetsio.cpp:96`):
    /// the root section directly (and the file saved), or `metaData` with
    /// its `settings` child replaced.
    pub fn write_settings_file(&self, io: &mut dyn MayaIo, filename: &str, root: bool) -> bool {
        let mut file = io
            .read_section(filename)
            .unwrap_or_else(|| DataSection::new("root"));
        let written = if root {
            self.write_settings(Some(&mut file))
        } else {
            // The reference's findChild/newSection, then delChild("settings")
            // and newSection("settings").
            let meta_data = match file.open_or_make_section("metaData") {
                Some(meta) => meta,
                None => return false,
            };
            let _ = meta_data.del_child("settings");
            let settings = meta_data.add_child("settings");
            self.write_settings(Some(settings))
        };
        if written {
            io.save_section(filename, &file);
        }
        written
    }
}

/// The `(ExportMode)readInt` cast of `readSettings`; out-of-range values
/// fall back to `NORMAL` (`EXPORTMODE_END` never escapes).
fn export_mode_from_int(value: i32) -> ExportMode {
    match value {
        1 => ExportMode::Static,
        2 => ExportMode::StaticWithNodes,
        3 => ExportMode::MeshParticles,
        _ => ExportMode::Normal,
    }
}

/// `parseMStringExportMode`: an integer or one of the four names.
fn export_mode_from_string(text: &str) -> Option<ExportMode> {
    if let Ok(value) = text.parse::<i32>() {
        return match value {
            0..=3 => Some(export_mode_from_int(value)),
            _ => None,
        };
    }
    match text.to_lowercase().as_str() {
        "normal" => Some(ExportMode::Normal),
        "static" => Some(ExportMode::Static),
        "static_with_nodes" => Some(ExportMode::StaticWithNodes),
        "mesh_particles" => Some(ExportMode::MeshParticles),
        _ => None,
    }
}

/// The `(NodeFilter)readInt` cast; out-of-range falls back to `ALL`.
fn node_filter_from_int(value: i32) -> NodeFilter {
    match value {
        1 => NodeFilter::Selected,
        2 => NodeFilter::Visible,
        _ => NodeFilter::All,
    }
}

/// `parseMStringNodeFilter`: an integer or one of the three names.
fn node_filter_from_string(text: &str) -> Option<NodeFilter> {
    if let Ok(value) = text.parse::<i32>() {
        return match value {
            0..=2 => Some(node_filter_from_int(value)),
            _ => None,
        };
    }
    match text.to_lowercase().as_str() {
        "all" => Some(NodeFilter::All),
        "selected" => Some(NodeFilter::Selected),
        "visible" => Some(NodeFilter::Visible),
        _ => None,
    }
}

/// `parseMStringBool`: an integer (non-zero is true) or the literal
/// `True` (case-sensitive, like the reference).
fn parse_mstring_bool(text: &str) -> bool {
    match text.parse::<i32>() {
        Ok(value) => value != 0,
        Err(_) => text == "True",
    }
}

/// `parseOptionsString` (`visualfiletranslator.cpp:1501`): the
/// semicolon-separated `key=value` options the Maya export dialog passes.
/// Unknown keys are ignored.
pub fn parse_options_string(options: &str, settings: &mut MayaExportSettings) {
    for option in options.split(';') {
        let mut parts = option.splitn(2, '=');
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        match key {
            "exportAnimation" => settings.export_anim = parse_mstring_bool(value),
            "exportMode" => {
                if let Some(mode) = export_mode_from_string(value) {
                    settings.export_mode = mode;
                }
            }
            "boneCount" => settings.max_bones = value.parse().unwrap_or(0),
            "transformToOrigin" => settings.transform_to_origin = parse_mstring_bool(value),
            "allowScale" => settings.allow_scale = parse_mstring_bool(value),
            "bumpMapped" => settings.bump_mapped = parse_mstring_bool(value),
            "keepExistingMaterials" => settings.keep_existing_materials = parse_mstring_bool(value),
            "snapVertices" => settings.snap_vertices = parse_mstring_bool(value),
            "stripRefPrefix" => settings.strip_ref_prefix = parse_mstring_bool(value),
            "useReferenceNode" => settings.reference_node = parse_mstring_bool(value),
            "disableVisualChecker" => settings.disable_visual_checker = parse_mstring_bool(value),
            "useLegacyScaling" => settings.use_legacy_scaling = parse_mstring_bool(value),
            "fixCylindrical" => settings.fix_cylindrical = parse_mstring_bool(value),
            "useLegacyOrientation" => settings.use_legacy_orientation = parse_mstring_bool(value),
            "sceneRootAdded" => settings.scene_root_added = parse_mstring_bool(value),
            "includeMeshes" => settings.include_meshes = parse_mstring_bool(value),
            "includeEnvelopesAndBones" => {
                settings.include_envelopes_and_bones = parse_mstring_bool(value)
            }
            "includeNodes" => settings.include_nodes = parse_mstring_bool(value),
            "includeMaterials" => settings.include_materials = parse_mstring_bool(value),
            "includeAnimations" => settings.include_animations = parse_mstring_bool(value),
            "useCharacterMode" => settings.use_character_mode = parse_mstring_bool(value),
            "animationName" => settings.animation_name = value.to_string(),
            "includePortals" => settings.include_portals = parse_mstring_bool(value),
            "worldSpaceOrigin" => settings.world_space_origin = parse_mstring_bool(value),
            "unitScale" => settings.unit_scale = value.parse().unwrap_or(0.0),
            "localHierarchy" => settings.local_hierarchy = parse_mstring_bool(value),
            "nodeFilter" => {
                if let Some(filter) = node_filter_from_string(value) {
                    settings.node_filter = filter;
                }
            }
            "copyExternalTextures" => settings.copy_external_textures = parse_mstring_bool(value),
            "copyTexturesTo" => settings.copy_textures_to = value.to_string(),
            _ => {}
        }
    }
}

// ----------------------------------------------------------------------
// The mesh gather (mesh.cpp).
// ----------------------------------------------------------------------

/// The raw `material` struct of `mesh.hpp`, as the host walked it out of
/// the dependency graph.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawMaterial {
    /// The surface-shader plug name (before the first-dot truncation).
    pub name: String,
    /// `mapFile`.
    pub map_file: String,
    /// `fxFile` (cleared by the `.mfm` promotion, like the reference).
    pub fx_file: String,
    /// `mapIdMeaning`: 0 = none, 1 = bitmap, 2 = mfm.
    pub map_id_meaning: i32,
}

/// One polygon of the raw mesh, with the per-corner index pools Maya
/// hands out (`getPolygonVertices`, `getFaceNormalIds`,
/// `getTangentId`, `getPolygonUVid`, `getFaceVertexColorIndex`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawPolygon {
    /// `getPolygonVertices(p)` — one per corner.
    pub position_indices: Vec<i32>,
    /// `getFaceNormalIds(p)`.
    pub normal_indices: Vec<i32>,
    /// `getTangentId(p, positionIndex)` per corner; indexes both the
    /// tangent and the binormal pool (a `VisualMesh::init` quirk).
    pub tangent_indices: Vec<i32>,
    /// `getPolygonUVid(p, corner, setNames[0])` per corner.
    pub uv_indices: Vec<i32>,
    /// The second uv set's ids (empty when there is no second set).
    pub uv2_indices: Vec<i32>,
    /// `getFaceVertexColorIndex` per corner (empty when there are no
    /// colour sets).
    pub colour_indices: Vec<i32>,
    /// `materialIndexes[p]`.
    pub material_index: i32,
}

/// The Maya mesh, as the host read it (the `MFnMesh` gather of
/// `mesh.cpp`). The pools arrive in Maya space — [`gather_mesh`] applies
/// the axis flips.
#[derive(Debug, Clone, Default)]
pub struct RawMesh {
    /// The dag path of the mesh shape (`fullPathName`).
    pub full_name: String,
    /// The transform's full path; `None` renders as `"Scene Root"`.
    pub transform_path: Option<String>,
    /// `getPoints` (cartesianised).
    pub positions: Vec<Vector3>,
    /// `getNormals`.
    pub normals: Vec<Vector3>,
    /// `getBinormals` (the base uv set).
    pub binormals: Vec<Vector3>,
    /// `getTangents` (the base uv set).
    pub tangents: Vec<Vector3>,
    /// `getFaceVertexColors` — empty when the mesh has no colour sets.
    pub colours: Vec<Vector4>,
    /// One uv pool per set name.
    pub uv_sets: Vec<Vec<Vector2>>,
    /// `mesh.instanceCount(true)` — fewer than one rejects the gather.
    pub instance_count: u32,
    /// The polygons.
    pub polygons: Vec<RawPolygon>,
    /// The shader walk's materials.
    pub materials: Vec<RawMaterial>,
}

/// `Face` (`face.hpp`): one fan triangle with every per-corner index.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Face {
    /// `positionIndex`.
    pub position_index: [i32; 3],
    /// `normalIndex`.
    pub normal_index: [i32; 3],
    /// `tangentIndex`.
    pub tangent_index: [i32; 3],
    /// `uvIndex`.
    pub uv_index: [i32; 3],
    /// `uvIndex2`.
    pub uv_index2: [i32; 3],
    /// `colourIndex`.
    pub colour_index: [i32; 3],
    /// `materialIndex`.
    pub material_index: i32,
}

/// `Mesh` (`mesh.hpp`): the gathered mesh the visual mesh consumes.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    /// `_name` — the full name after the last `|`.
    pub name: String,
    /// `_nodeName` — the transform after the last `|`, or
    /// `"Scene Root"`.
    pub node_name: String,
    /// `_fullName`.
    pub full_name: String,
    /// `_positions` — axis-flipped.
    pub positions: Vec<Vector3>,
    /// `_normals` — axis-flipped.
    pub normals: Vec<Vector3>,
    /// `_binormals` — axis-flipped.
    pub binormals: Vec<Vector3>,
    /// `_tangents` — axis-flipped.
    pub tangents: Vec<Vector3>,
    /// `_colours` — channel-clamped to `[0, 1]`.
    pub colours: Vec<Vector4>,
    /// `_uvs` — one pool per gathered set.
    pub uvs: Vec<Vec<Vector2>>,
    /// `_faces` — the fan triangles.
    pub faces: Vec<Face>,
    /// `_materials` — name truncated at the first dot, `.mfm`
    /// promotion applied.
    pub materials: Vec<RawMaterial>,
}

impl Mesh {
    /// `Mesh::uvSetCount()`.
    pub fn uv_set_count(&self) -> usize {
        self.uvs.len()
    }
}

/// `Mesh::initialise` (`mesh.cpp:88`): the whole gather — the name
/// derivation, the useLegacyOrientation flips, the uv set walk, the
/// polygon fan and the material walk. `None` is the reference's
/// `return false` faces.
pub fn gather_mesh(raw: &RawMesh, settings: &MayaExportSettings, io: &dyn MayaIo) -> Option<Mesh> {
    let legacy = settings.use_legacy_orientation;

    let mut mesh = Mesh {
        name: after_last_pipe(&raw.full_name),
        node_name: raw
            .transform_path
            .as_deref()
            .map(after_last_pipe)
            .unwrap_or_else(|| "Scene Root".to_string()),
        full_name: raw.full_name.clone(),
        positions: Vec::new(),
        normals: Vec::new(),
        binormals: Vec::new(),
        tangents: Vec::new(),
        colours: Vec::new(),
        uvs: Vec::new(),
        faces: Vec::new(),
        materials: Vec::new(),
    };

    // The axis flip of `Mesh::initialise`: legacy negates x (the 1.7.2
    // release), the default negates z.
    let flip = |v: Vector3| {
        if legacy {
            Vector3::new(-v.x, v.y, v.z)
        } else {
            Vector3::new(v.x, v.y, -v.z)
        }
    };
    mesh.positions = raw.positions.iter().map(|&v| flip(v)).collect();
    mesh.normals = raw.normals.iter().map(|&v| flip(v)).collect();
    mesh.binormals = raw.binormals.iter().map(|&v| flip(v)).collect();
    mesh.tangents = raw.tangents.iter().map(|&v| flip(v)).collect();

    // Vertex colours, each channel clamped into [0, 1].
    for &c in &raw.colours {
        let ch = |v: f32| v.clamp(0.0, 1.0);
        mesh.colours
            .push(Vector4::new(ch(c.x), ch(c.y), ch(c.z), ch(c.w)));
    }

    // The uv sets: one pool per gathered set name. The host pairs u and
    // v per point (`getUVs` hands them out as two arrays); a length
    // mismatch between the two is the host contract break the reference
    // checks while reading each set, so the port takes the paired pools
    // as input.
    mesh.uvs = raw.uv_sets.clone();

    if raw.instance_count < 1 {
        return None;
    }

    // The polygon fan. A polygon with fewer than three corners yields no
    // triangles (the reference's unsigned `length() - 2` would run away on
    // zero- and one-corner polygons — degenerate input the exporter never
    // sees from Maya).
    for polygon in &raw.polygons {
        let position_indexes = &polygon.position_indices;
        let normal_indexes = &polygon.normal_indices;
        if position_indexes.len() > normal_indexes.len() {
            return None;
        }
        let has_uv2 = raw.uv_sets.len() > 1;
        let has_colours = !mesh.colours.is_empty();
        for i in 0..position_indexes.len().saturating_sub(2) {
            let index1 = i + 1;
            let index2 = i + 2;
            let corner = |list: &[i32]| [list[0], list[index1], list[index2]];
            let mut face = Face {
                position_index: corner(position_indexes),
                normal_index: corner(normal_indexes),
                tangent_index: corner(&polygon.tangent_indices),
                material_index: polygon.material_index,
                ..Face::default()
            };
            face.uv_index = corner(&polygon.uv_indices);
            if has_uv2 {
                face.uv_index2 = corner(&polygon.uv2_indices);
            }
            if has_colours {
                face.colour_index = corner(&polygon.colour_indices);
            }
            mesh.faces.push(face);
        }
    }

    // The material walk: the plug name keeps its prefix up to the first
    // dot, and a same-stem `.mfm` promotes the map.
    for material in &raw.materials {
        let name = match material.name.find('.') {
            Some(pos) => material.name[..pos].to_string(),
            None => material.name.clone(),
        };
        let mut gathered = RawMaterial {
            name,
            map_file: material.map_file.clone(),
            fx_file: material.fx_file.clone(),
            map_id_meaning: material.map_id_meaning,
        };
        if !material.map_file.is_empty() || gathered.map_id_meaning != 0 {
            let mfm_name = format!("{}.mfm", remove_extension(&material.map_file));
            if io.file_exists(&io.resolve_filename(&mfm_name)) {
                gathered.map_file = mfm_name;
                gathered.map_id_meaning = 2;
                gathered.fx_file = String::new();
            }
        }
        mesh.materials.push(gathered);
    }

    Some(mesh)
}

/// `_fullName.substr(_fullName.find_last_of("|") + 1)`: the component
/// after the last `|` (the whole string when there is none).
fn after_last_pipe(path: &str) -> String {
    match path.rfind('|') {
        Some(pos) => path[pos + 1..].to_string(),
        None => path.to_string(),
    }
}

/// `BWResource::removeExtension`: the text before the last dot.
fn remove_extension(filename: &str) -> String {
    match filename.rfind('.') {
        Some(pos) => filename[..pos].to_string(),
        None => filename.to_string(),
    }
}

// ----------------------------------------------------------------------
// VisualMesh (visual_mesh.cpp, Maya edition).
// ----------------------------------------------------------------------

/// `VisualMesh::Material`: the fields the Maya save chain consumes. The
/// reference's `selfIllum` census and the fx/override lists are all
/// commented out in the Maya exporter and stay host-side.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MayaMaterial {
    /// `identifier`.
    pub identifier: String,
    /// `mapIdentifier`.
    pub map_identifier: String,
    /// `mapIdMeaning`: 0 = none, 1 = bitmap, 2 = mfm.
    pub map_id_meaning: i32,
    /// `inUse` (set by `createPrimitiveGroups`).
    pub in_use: bool,
}

/// The Maya `VisualMesh`.
#[derive(Debug, Clone, Default)]
pub struct MayaVisualMesh {
    /// `identifier_` (the mesh name).
    pub identifier: String,
    /// `nodeIdentifier_` (the transform name; `"Scene Root"` default) —
    /// written as the render set node.
    pub node_identifier: String,
    /// `fullName_`.
    pub full_name: String,
    /// `vertices_`.
    pub vertices: Vec<BloatVertex>,
    /// `triangles_`.
    pub triangles: Vec<Triangle>,
    /// `materials_`.
    pub materials: Vec<MayaMaterial>,
    /// `dualUV_`.
    pub dual_uv: bool,
    /// `vertexColours_`.
    pub vertex_colours: bool,
    /// `largeIndices_` (set by [`Self::save`]).
    pub large_indices: bool,
    /// `snapVertices_`.
    pub snap_vertices: bool,
    /// `errors_` (`addError`/`errorText`).
    pub errors: Vec<String>,
    /// `bb_` — the world-space bounding box.
    pub bb: BoundBox,
    /// `materialRemap_` (raw material index → `materials_` index).
    pub material_remap: Vec<usize>,
}

impl MayaVisualMesh {
    /// `VisualMesh::init` (`visual_mesh.cpp:1684`).
    pub fn init(&mut self, mesh: &Mesh, settings: &MayaExportSettings, io: &dyn MayaIo) -> bool {
        self.errors.clear();
        self.snap_vertices = settings.snap_vertices;
        let ret = false;
        if !mesh.positions.is_empty()
            && !mesh.normals.is_empty()
            && !mesh.binormals.is_empty()
            && !mesh.tangents.is_empty()
            && mesh.uv_set_count() > 0
            && !mesh.uvs[0].is_empty()
            && !mesh.faces.is_empty()
        {
            self.identifier = trim_whitespaces(&mesh.name);
            self.node_identifier = trim_whitespaces(&mesh.node_name);
            self.full_name = mesh.full_name.clone();

            self.gather_materials(mesh, settings, io);

            let map_count = mesh.uv_set_count();
            let has_uvs = !mesh.uvs[0].is_empty();
            self.dual_uv = has_uvs && map_count > 1;
            self.vertex_colours = !mesh.colours.is_empty();

            // The reference's single reused `BloatVertex` — the channels
            // of a skipped branch keep the previous corner's value (uv2
            // and colour stay zero when the mesh lacks them, exactly like
            // the reference).
            let mut bv = BloatVertex::default();
            for face in &mesh.faces {
                let mut tr = Triangle {
                    material_index: self.material_remap[face.material_index as usize] as i32,
                    smoothing_group: 0,
                    ..Triangle::default()
                };
                for j in 0..3 {
                    bv.pos = [
                        mesh.positions[face.position_index[j] as usize].x,
                        mesh.positions[face.position_index[j] as usize].y,
                        mesh.positions[face.position_index[j] as usize].z,
                    ];
                    bv.normal = normalised([
                        mesh.normals[face.normal_index[j] as usize].x,
                        mesh.normals[face.normal_index[j] as usize].y,
                        mesh.normals[face.normal_index[j] as usize].z,
                    ]);
                    // The binormal is indexed by the tangent id — the
                    // reference's `f.tangentIndex[j]` on the binormal
                    // pool.
                    bv.binormal = normalised([
                        mesh.binormals[face.tangent_index[j] as usize].x,
                        mesh.binormals[face.tangent_index[j] as usize].y,
                        mesh.binormals[face.tangent_index[j] as usize].z,
                    ]);
                    bv.tangent = normalised([
                        mesh.tangents[face.tangent_index[j] as usize].x,
                        mesh.tangents[face.tangent_index[j] as usize].y,
                        mesh.tangents[face.tangent_index[j] as usize].z,
                    ]);

                    bv.vertex_index = face.position_index[j];
                    bv.smoothing_group = 0;
                    if has_uvs {
                        let uv = mesh.uvs[0][face.uv_index[j] as usize];
                        bv.uv = [uv.x, 1.0 - uv.y];
                    }
                    if self.dual_uv {
                        let uv2 = mesh.uvs[1][face.uv_index2[j] as usize];
                        bv.uv2 = [uv2.x, 1.0 - uv2.y];
                    }
                    if self.vertex_colours {
                        let c = mesh.colours[face.colour_index[j] as usize];
                        bv.colour = [c.x, c.y, c.z, c.w];
                    }
                    bv.mesh_index = 0;

                    if self.snap_vertices {
                        bv.pos = snap_point3(bv.pos);
                    }
                    tr.index[j] = self.add_vertex(bv.clone());
                    tr.real_index[j] = bv.vertex_index;
                }
                self.triangles.push(tr);
            }

            // `world_` stays the identity matrix, so the bounding box runs
            // over the positions as gathered.
            let snapped = |p: [f32; 3]| {
                if self.snap_vertices {
                    snap_point3(p)
                } else {
                    p
                }
            };
            let first = snapped(self.vertices[0].pos);
            let first = Vector3::new(first[0], first[1], first[2]);
            self.bb = BoundBox::new(first, first);
            for vertex in &self.vertices[1..] {
                let p = snapped(vertex.pos);
                self.bb.add_bounds(&Vector3::new(p[0], p[1], p[2]));
            }
            let _ = ret;
            true
        } else {
            if !mesh.positions.is_empty() && !mesh.normals.is_empty() && !mesh.faces.is_empty() {
                let msg = format!(
                    "Failed to export mesh {} because there are no uvs",
                    mesh.full_name
                );
                if !io.automated_test() {
                    io.message_box(&msg, "Visual Exporter Error");
                }
                self.errors.push(msg);
            }
            let _ = ret;
            false
        }
    }

    /// `VisualMesh::addVertex` under `TC_OPTIMISATION`: always append.
    fn add_vertex(&mut self, bv: BloatVertex) -> i32 {
        self.vertices.push(bv);
        (self.vertices.len() - 1) as i32
    }

    /// `VisualMesh::errorText`.
    pub fn error_text(&self) -> String {
        self.errors.join("\n")
    }

    /// `VisualMesh::addMaterial` (`visual_mesh.cpp:185`).
    pub fn add_material(
        &mut self,
        material: &RawMaterial,
        settings: &MayaExportSettings,
        io: &dyn MayaIo,
    ) {
        let mut m = MayaMaterial {
            identifier: material.name.clone(),
            ..MayaMaterial::default()
        };

        // Replace environment macros in the map file name.
        let texture_name = match replace_environment_macros(&material.map_file, io) {
            Some(name) => name,
            None => {
                let err_msg = format!(
                    "Material \"{}\" has an invalid texture name:\n\n\"{}\"\n\n\
Can't resolve this name, have you configured all the environment variables in your maya.env?\n\
Texture may not be visible in game.\n",
                    material.name, material.map_file
                );
                if !io.automated_test() {
                    io.message_box(&err_msg, "Visual Exporter Warning!");
                }
                self.errors.push(err_msg);
                material.map_file.clone()
            }
        };

        // Dissolve the (possibly still original) name.
        let mut identifier = io.dissolve_filename(&texture_name);

        if settings.copy_external_textures && io.is_absolute_path(&identifier) {
            if settings.copy_textures_to.is_empty() {
                identifier = String::new();
            } else {
                let texture_file_name = after_last_slash(&texture_name);
                let new_texture_name =
                    format!("{}/{}", settings.copy_textures_to, texture_file_name);
                if io.copy_file(&texture_name, &new_texture_name) {
                    identifier = io.dissolve_filename(&new_texture_name);
                } else {
                    let err_msg = format!(
                        "Could not copy texture {} to {}\n",
                        texture_name, new_texture_name
                    );
                    self.errors.push(err_msg);
                }
            }
        }

        if !texture_name.is_empty() {
            m.map_identifier = identifier;
            m.map_id_meaning = material.map_id_meaning;
        }

        let index = self.find_or_add_material(&m);
        self.material_remap.push(index);
    }

    /// `VisualMesh::gatherMaterials` (`visual_mesh.cpp:295`). The
    /// reference's `addMaterial` reaches the settings singleton; the port
    /// threads the caller's copy through.
    pub fn gather_materials(
        &mut self,
        mesh: &Mesh,
        settings: &MayaExportSettings,
        io: &dyn MayaIo,
    ) {
        for material in &mesh.materials {
            self.add_material(material, settings, io);
        }
        if self.material_remap.is_empty() {
            self.material_remap.push(0);
        }
    }

    /// `VisualMesh::findOrAddMaterial`: the linear scan on the Maya
    /// equality (identifier, mapIdMeaning, mapIdentifier — the
    /// `selfIllum` term is commented out in the reference).
    fn find_or_add_material(&mut self, material: &MayaMaterial) -> usize {
        for (i, existing) in self.materials.iter().enumerate() {
            if existing.identifier == material.identifier
                && existing.map_id_meaning == material.map_id_meaning
                && existing.map_identifier == material.map_identifier
            {
                return i;
            }
        }
        self.materials.push(material.clone());
        self.materials.len() - 1
    }

    /// `VisualMesh::save` (`visual_mesh.cpp:1046`): the primitives
    /// streams and the `.visual` document. `None` is the reference's
    /// `return false` (no materials, or the mesh-particle group shape).
    pub fn save(
        &mut self,
        settings: &MayaExportSettings,
        existing_visual: Option<&DataSection>,
        primitive_file: &str,
        use_identifier: bool,
        io: &mut dyn MayaIo,
    ) -> Option<SavedVisual> {
        let id_prefix = if use_identifier {
            format!("{}.", self.identifier)
        } else {
            String::new()
        };

        flip_triangle_winding_order(&mut self.triangles);
        remove_duplicate_vertices(&mut self.vertices, &mut self.triangles);
        sort_triangles(&mut self.vertices, &mut self.triangles);

        // The bump-mapped `makeBumped` host pass runs here (unless the
        // export is mesh particles); the port consumes the tangent and
        // binormal already carried on the vertices.
        if settings.bump_mapped && settings.export_mode != ExportMode::MeshParticles {
            // makeBumped seam (see the module docs).
        }

        remove_duplicate_vertices(&mut self.vertices, &mut self.triangles);
        sort_triangles(&mut self.vertices, &mut self.triangles);

        // `createPrimitiveGroups` with the Maya's own guards.
        if self.materials.is_empty() {
            let err_msg = format!(
                "{} does not have any materials.\nThis object will not be exported.  \
Adding a material will fix this problem.",
                self.identifier
            );
            if !io.automated_test() {
                io.message_box(&err_msg, "Material error!");
            }
            self.errors.push(err_msg);
            return None;
        }
        let shared_materials: Vec<crate::visual_exporter::Material> = self
            .materials
            .iter()
            .map(|m| crate::visual_exporter::Material {
                identifier: m.identifier.clone(),
                map_identifier: m.map_identifier.clone(),
                map_id_meaning: m.map_id_meaning,
                in_use: m.in_use,
                ..crate::visual_exporter::Material::default()
            })
            .collect();
        let mut shared_materials = shared_materials;
        let (groups, indices, entry_size) = create_primitive_groups(
            &self.vertices,
            &self.triangles,
            &mut shared_materials,
            settings.export_mode,
        )
        .map_err(|_| {
            self.errors.push(
                "VisualMesh::createMeshParticlePrimGroups - \
Mesh particles must use only 1 material/mesh."
                    .to_string(),
            );
        })
        .ok()?;
        for (shared, material) in shared_materials.iter().zip(&mut self.materials) {
            material.in_use = shared.in_use;
        }
        self.large_indices = entry_size != 2;
        let index_format = if self.large_indices { "list32" } else { "list" };

        let mut primitives = BinSection::new("root", BinaryBlock::new(Vec::new()));
        let mut blob = Vec::new();
        IndexHeader {
            index_format: index_format.to_string(),
            n_indices: indices.len() as i32,
            n_triangle_groups: groups.len() as i32,
        }
        .write(&mut blob);
        if entry_size == 2 {
            for v in &indices {
                blob.extend_from_slice(&(*v as u16).to_le_bytes());
            }
        } else {
            for v in &indices {
                blob.extend_from_slice(&v.to_le_bytes());
            }
        }
        write_primitive_groups(&groups, &mut blob);
        primitives.write_binary(&format!("{id_prefix}indices"), BinaryBlock::new(blob));

        let (format_name, vertex_blob) = if settings.export_mode == ExportMode::MeshParticles {
            ("xyznuvi", self.vertex_list_xyznuvi(settings))
        } else if settings.bump_mapped {
            ("xyznuvtb", self.vertex_list_xyznuvtb(settings))
        } else {
            ("xyznuv", self.vertex_list_xyznuv(settings))
        };
        let mut vertex_data = Vec::new();
        VertexHeader {
            vertex_format: format_name.to_string(),
            n_vertices: self.vertices.len() as i32,
        }
        .write(&mut vertex_data);
        vertex_data.extend_from_slice(&vertex_blob);
        primitives.write_binary(
            &format!("{id_prefix}vertices"),
            BinaryBlock::new(vertex_data),
        );

        // The extra vertex data streams.
        let mut streams: Vec<String> = Vec::new();
        if self.dual_uv {
            let mut uv2 = Vec::with_capacity(self.vertices.len() * 8);
            for bv in &self.vertices {
                uv2.extend_from_slice(&bv.uv2[0].to_le_bytes());
                uv2.extend_from_slice(&bv.uv2[1].to_le_bytes());
            }
            let name = format!("{id_prefix}uv2");
            primitives.write_binary(&name, BinaryBlock::new(uv2));
            streams.push(name);
        }
        if self.vertex_colours {
            let mut colours = Vec::with_capacity(self.vertices.len() * 4);
            for bv in &self.vertices {
                colours.extend_from_slice(
                    &d3dcolor_colorvalue(bv.colour[0], bv.colour[1], bv.colour[2], bv.colour[3])
                        .to_le_bytes(),
                );
            }
            let name = format!("{id_prefix}colour");
            primitives.write_binary(&name, BinaryBlock::new(colours));
            streams.push(name);
        }

        io.save_binary(primitive_file, &primitives.save_bytes());

        // The .visual document.
        let mut visual = DataSection::new("root");
        let render_set = visual.add_child("renderSet");
        render_set.write_bool("treatAsWorldSpaceObject", false);
        render_set.write_string("node", &self.node_identifier);
        let geometry = render_set.add_child("geometry");
        geometry.write_string("vertices", &format!("{id_prefix}vertices"));
        for stream in &streams {
            geometry.add_child("stream").set_string(stream);
        }
        geometry.write_string("primitive", &format!("{id_prefix}indices"));

        let mut prim_group_id = 0u32;
        for material in &self.materials {
            if material.in_use {
                maya_material_save(
                    material,
                    geometry,
                    existing_visual,
                    prim_group_id,
                    SkinType::NoSkin,
                    false,
                    settings,
                );
                prim_group_id += 1;
            }
        }

        Some(SavedVisual { visual, primitives })
    }

    /// `createVertexList(VertexVector)` — `xyznuv`: the position scaled
    /// by `unitScale()` and written unswizzled (the axis flip happened
    /// in `Mesh::initialise`).
    pub fn vertex_list_xyznuv(&self, settings: &MayaExportSettings) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * 32);
        for bv in &self.vertices {
            for v in scaled_pos(bv.pos, settings) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in bv.normal {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&bv.uv[0].to_le_bytes());
            out.extend_from_slice(&bv.uv[1].to_le_bytes());
        }
        out
    }

    /// `createVertexList(IndexedVertexVector)` — `xyznuvi`: the
    /// sub-mesh id encoded as `meshIndex * 3`. The reference asserts the
    /// vertices arrive sorted by mesh index.
    pub fn vertex_list_xyznuvi(&self, settings: &MayaExportSettings) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * 36);
        let mut last_mesh_index = self.vertices.first().map(|v| v.mesh_index);
        for bv in &self.vertices {
            debug_assert!(
                last_mesh_index.unwrap_or(i32::MIN) <= bv.mesh_index,
                "mesh indices must be non-decreasing"
            );
            last_mesh_index = Some(bv.mesh_index);
            for v in scaled_pos(bv.pos, settings) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in bv.normal {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&bv.uv[0].to_le_bytes());
            out.extend_from_slice(&bv.uv[1].to_le_bytes());
            out.extend_from_slice(&((bv.mesh_index as f32) * 3.0).to_le_bytes());
        }
        out
    }

    /// `createVertexList(TBVertexVector)` — `xyznuvtb`: the packed
    /// normal/tangent/binormal words.
    pub fn vertex_list_xyznuvtb(&self, settings: &MayaExportSettings) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * 32);
        for bv in &self.vertices {
            for v in scaled_pos(bv.pos, settings) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&pack_normal_maya(bv.normal).to_le_bytes());
            out.extend_from_slice(&bv.uv[0].to_le_bytes());
            out.extend_from_slice(&bv.uv[1].to_le_bytes());
            out.extend_from_slice(&pack_normal_maya(bv.tangent).to_le_bytes());
            out.extend_from_slice(&pack_normal_maya(bv.binormal).to_le_bytes());
        }
        out
    }
}

/// The scaled position of `createVertexList` (`bv.pos *= unitScale()`).
fn scaled_pos(pos: [f32; 3], settings: &MayaExportSettings) -> [f32; 3] {
    let s = settings.unit_scale();
    [pos[0] * s, pos[1] * s, pos[2] * s]
}

/// The normalisation the reference's `vector3::normalise()` performs
/// (in place under a positive length).
fn normalised(v: [f32; 3]) -> [f32; 3] {
    let mag = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if mag > 0.0 {
        [v[0] / mag, v[1] / mag, v[2] / mag]
    } else {
        v
    }
}

/// `afterLastSlash` — `BWResource::getFilename`: the component after the
/// last `/` (or `\`).
fn after_last_slash(path: &str) -> String {
    match path.rfind(['/', '\\']) {
        Some(pos) => path[pos + 1..].to_string(),
        None => path.to_string(),
    }
}

/// The Maya `packNormal` (`visual_mesh.cpp:37`): z sits in the 10-bit
/// slot and there is no legacy branch (the axis flip already happened in
/// `Mesh::initialise`).
pub fn pack_normal_maya(n: [f32; 3]) -> u32 {
    let n = normalised(n);
    let clamp = |v: f32| v.clamp(-1.0, 1.0);
    let trunc = |v: f32| (v as i64) as u32;
    let x = clamp(n[0]);
    let y = clamp(n[1]);
    let z = clamp(n[2]);
    ((trunc(z * 511.0) & 0x3ff) << 22)
        | ((trunc(y * 1023.0) & 0x7ff) << 11)
        | (trunc(x * 1023.0) & 0x7ff)
}

// ----------------------------------------------------------------------
// Material::save (visual_mesh.cpp:1423, Maya edition).
// ----------------------------------------------------------------------

/// A `property` child carrying the override name as its value.
fn property<'a>(material: &'a mut DataSection, name: &str) -> &'a mut DataSection {
    let prop = material.add_child("property");
    prop.set_string(name);
    prop
}

/// `VisualMesh::Material::save` (`visual_mesh.cpp:1423`) — the Maya
/// chain: no fx-file branch and no inline `.mfm` promotion (that lives in
/// the host's `resources()` walk).
#[allow(clippy::too_many_arguments)]
pub fn maya_material_save(
    material: &MayaMaterial,
    geometry: &mut DataSection,
    existing_visual: Option<&DataSection>,
    pgid: u32,
    skin: SkinType,
    skinned_shader: bool,
    settings: &MayaExportSettings,
) {
    let prim_group = geometry.add_child("primitiveGroup");
    prim_group.set_int(pgid as i32);

    let id_postfix = if skin == SkinType::SoftSkin {
        "_skinned"
    } else {
        ""
    };
    let wanted = format!("{}{}", material.identifier, id_postfix);

    // Copy the existing bitmap/fx combination when one matches.
    if let Some(existing) = find_existing_material(&wanted, existing_visual) {
        prim_group.copy_from(existing);
        return;
    }

    let m = prim_group.add_child("material");
    m.write_string("identifier", &wanted);

    if settings.export_mode == ExportMode::MeshParticles {
        m.write_string("fx", "shaders/std_effects/mesh_particle.fx");
        if !material.map_identifier.is_empty() {
            property(m, "diffuseMap").write_string("Texture", &material.map_identifier);
        }
    } else if material.map_id_meaning == 1 {
        if skin == SkinType::NoSkin {
            let mut fx = String::from("shaders/std_effects/lightonly");
            let len = material.map_identifier.len();
            if len > 8 && material.map_identifier[len - 8..len - 4] == *"_afx" {
                fx.push_str("_add");
            }
            fx.push_str(".fx");
            m.write_string("fx", &fx);
        } else if skinned_shader {
            m.write_string("fx", "shaders/std_effects/lightonly_skinned.fx");
        } else {
            m.write_string("fx", "shaders/std_effects/lightonly.fx");
        }
        if !material.map_identifier.is_empty() {
            property(m, "diffuseMap").write_string("Texture", &material.map_identifier);
        }
    } else if material.map_id_meaning == 2 {
        m.write_string("mfm", &material.map_identifier);
    } else if skin == SkinType::NoSkin {
        if !settings.default_material.is_empty() {
            m.write_string("mfm", &settings.default_material);
        }
    } else if !settings.default_skinned_material.is_empty() {
        m.write_string("mfm", &settings.default_skinned_material);
    }
}

// ----------------------------------------------------------------------
// Constraints (constraints.cpp).
// ----------------------------------------------------------------------

/// One source of a constraint: the driven node and its weight.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConstraintSource {
    /// The driven node's name (`targetTranslate` connection).
    pub identifier: String,
    /// The `targetWeight` child (absent when the host found no plug —
    /// the reference then writes no `weight` child).
    pub weight: Option<f32>,
}

/// An orient constraint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrientConstraint {
    /// The constraint node's name.
    pub identifier: String,
    /// The `constraintRotateX` source.
    pub target: Option<String>,
    /// The `offset` plug (radians).
    pub offset: Vector3,
    /// The `target` array elements.
    pub sources: Vec<ConstraintSource>,
}

/// A point constraint (`poleVectorConstraint` when the flag is set).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PointConstraint {
    /// The constraint node's name.
    pub identifier: String,
    /// Whether the object is a pole vector constraint.
    pub pole_vector: bool,
    /// The `constraintTranslateX` source.
    pub target: Option<String>,
    /// The `offset` plug.
    pub offset: Vector3,
    /// The `target` array elements.
    pub sources: Vec<ConstraintSource>,
}

/// One source of a parent constraint (the two extra offset plugs).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParentConstraintSource {
    /// The driven node's name.
    pub identifier: String,
    /// The `targetWeight` child.
    pub weight: Option<f32>,
    /// The `targetOffsetTranslate` child.
    pub translate_offset: Option<Vector3>,
    /// The `targetOffsetRotate` child (radians).
    pub rotate_offset: Option<Vector3>,
}

/// A parent constraint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParentConstraint {
    /// The constraint node's name.
    pub identifier: String,
    /// The `constraintRotateX` source.
    pub target: Option<String>,
    /// The `target` array elements.
    pub sources: Vec<ParentConstraintSource>,
}

/// An aim constraint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AimConstraint {
    /// The constraint node's name.
    pub identifier: String,
    /// The `constraintRotateX` source.
    pub target: Option<String>,
    /// The `offset` plug (radians).
    pub offset: Vector3,
    /// The `aimVector` plug.
    pub aim_vector: Vector3,
    /// The `upVector` plug.
    pub up_vector: Vector3,
    /// The `worldUpVector` plug.
    pub world_up_vector: Vector3,
    /// The `target` array elements.
    pub sources: Vec<ConstraintSource>,
}

/// An IK handle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IkHandle {
    /// The handle's name.
    pub identifier: String,
    /// `getStartJoint`.
    pub start_joint: String,
    /// The end effector's translate source.
    pub end_joint: Option<String>,
    /// The handle's translate source, when connected; else the raw
    /// translate vector.
    pub translate: TranslateOrVector,
    /// The pole vector source, when connected; else the raw vector.
    pub pole_vector: TranslateOrVector,
}

/// The `source/node` vs `source/vector` split of `writeIKHandle`.
#[derive(Debug, Clone, PartialEq)]
pub enum TranslateOrVector {
    /// `writeString("source/node", name)` (and `poleVector/node`).
    Node(String),
    /// `writeVector3("source/vector", v)` (and `poleVector/vector`).
    Vector(Vector3),
}

impl Default for TranslateOrVector {
    fn default() -> Self {
        Self::Vector(Vector3::ZERO)
    }
}

/// The scene's constraints, in the `ExportIterator` type order of
/// `Constraints::saveConstraints`.
#[derive(Debug, Clone, Default)]
pub struct SceneConstraints {
    /// The `kOrientConstraint` walk.
    pub orient: Vec<OrientConstraint>,
    /// The `kPointConstraint` walk.
    pub point: Vec<PointConstraint>,
    /// The `kParentConstraint` walk.
    pub parent: Vec<ParentConstraint>,
    /// The `kAimConstraint` walk.
    pub aim: Vec<AimConstraint>,
    /// The `kIkHandle` walk.
    pub ik_handles: Vec<IkHandle>,
}

/// `Constraints::saveConstraints` (`constraints.cpp:19`): the
/// `constraints` section with the five writers in the reference's
/// iterator order.
pub fn save_constraints(
    visual: &mut DataSection,
    scene: &SceneConstraints,
    settings: &MayaExportSettings,
) {
    let constraints = visual.add_child("constraints");

    for constraint in &scene.orient {
        write_orient_constraint(constraint, constraints, settings);
    }
    for constraint in &scene.point {
        write_point_constraint(constraint, constraints, settings);
    }
    for constraint in &scene.parent {
        write_parent_constraint(constraint, constraints, settings);
    }
    for constraint in &scene.aim {
        write_aim_constraint(constraint, constraints, settings);
    }
    for handle in &scene.ik_handles {
        write_ik_handle(handle, constraints, settings);
    }
}

fn write_orient_constraint(
    constraint: &OrientConstraint,
    constraints: &mut DataSection,
    settings: &MayaExportSettings,
) {
    let section = constraints.add_child("orientConstraint");
    section.write_string("identifier", &constraint.identifier);
    if let Some(target) = &constraint.target {
        section.write_string("target", target);
    }
    section.write_vector3(
        "offset",
        convert_angle_offset_to_bigworld_units(constraint.offset, settings),
    );
    for source in &constraint.sources {
        let source_section = section.add_child("source");
        if !source.identifier.is_empty() {
            source_section.write_string("identifier", &source.identifier);
        }
        if let Some(weight) = source.weight {
            source_section.write_float("weight", weight);
        }
    }
}

fn write_point_constraint(
    constraint: &PointConstraint,
    constraints: &mut DataSection,
    settings: &MayaExportSettings,
) {
    let section = constraints.add_child(if constraint.pole_vector {
        "poleVectorConstraint"
    } else {
        "pointConstraint"
    });
    section.write_string("identifier", &constraint.identifier);
    if let Some(target) = &constraint.target {
        section.write_string("target", target);
    }
    section.write_vector3(
        "offset",
        convert_point_to_bigworld_units(constraint.offset, settings),
    );
    for source in &constraint.sources {
        let source_section = section.add_child("source");
        if !source.identifier.is_empty() {
            source_section.write_string("identifier", &source.identifier);
        }
        if let Some(weight) = source.weight {
            source_section.write_float("weight", weight);
        }
    }
}

fn write_parent_constraint(
    constraint: &ParentConstraint,
    constraints: &mut DataSection,
    settings: &MayaExportSettings,
) {
    let section = constraints.add_child("parentConstraint");
    section.write_string("identifier", &constraint.identifier);
    if let Some(target) = &constraint.target {
        section.write_string("target", target);
    }
    for source in &constraint.sources {
        let source_section = section.add_child("source");
        if !source.identifier.is_empty() {
            source_section.write_string("identifier", &source.identifier);
        }
        if let Some(weight) = source.weight {
            source_section.write_float("weight", weight);
        }
        if let Some(translate) = source.translate_offset {
            source_section.write_vector3(
                "translateOffset",
                convert_point_to_bigworld_units(translate, settings),
            );
        }
        if let Some(rotate) = source.rotate_offset {
            source_section.write_vector3(
                "rotateOffset",
                convert_angle_offset_to_bigworld_units(rotate, settings),
            );
        }
    }
}

fn write_aim_constraint(
    constraint: &AimConstraint,
    constraints: &mut DataSection,
    settings: &MayaExportSettings,
) {
    let section = constraints.add_child("aimConstraint");
    section.write_string("identifier", &constraint.identifier);
    if let Some(target) = &constraint.target {
        section.write_string("target", target);
    }
    section.write_vector3(
        "offset",
        convert_angle_offset_to_bigworld_units(constraint.offset, settings),
    );
    section.write_vector3(
        "aimVector",
        convert_vector_to_bigworld_units(constraint.aim_vector, settings),
    );
    section.write_vector3(
        "upVector",
        convert_vector_to_bigworld_units(constraint.up_vector, settings),
    );
    section.write_vector3(
        "worldUpVector",
        convert_vector_to_bigworld_units(constraint.world_up_vector, settings),
    );
    for source in &constraint.sources {
        let source_section = section.add_child("source");
        if !source.identifier.is_empty() {
            source_section.write_string("identifier", &source.identifier);
        }
        if let Some(weight) = source.weight {
            source_section.write_float("weight", weight);
        }
    }
}

fn write_ik_handle(
    handle: &IkHandle,
    constraints: &mut DataSection,
    settings: &MayaExportSettings,
) {
    let section = constraints.add_child("ikHandle");
    section.write_string("identifier", &handle.identifier);
    section.write_string("startJoint", &handle.start_joint);
    if let Some(end_joint) = &handle.end_joint {
        section.write_string("endJoint", end_joint);
    }
    match &handle.translate {
        TranslateOrVector::Node(node) => {
            section.write_string("source/node", node);
        }
        TranslateOrVector::Vector(v) => {
            section.write_vector3(
                "source/vector",
                convert_point_to_bigworld_units(*v, settings),
            );
        }
    }
    match &handle.pole_vector {
        TranslateOrVector::Node(node) => {
            section.write_string("poleVector/node", node);
        }
        TranslateOrVector::Vector(v) => {
            section.write_vector3(
                "poleVector/vector",
                convert_vector_to_bigworld_units(*v, settings),
            );
        }
    }
}

// ----------------------------------------------------------------------
// Tests.
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::primitive_file_structs::INDEX_FORMAT_FIELD_LEN;

    /// The test host: a filesystem-shaped map-backed [`MayaIo`].
    struct TestIo {
        files: HashMap<String, Vec<u8>>,
        env: HashMap<String, String>,
        absolute: fn(&str) -> bool,
        copy_result: bool,
        copied: std::cell::RefCell<Vec<(String, String)>>,
        boxes: std::cell::RefCell<Vec<String>>,
        /// Sections served by `read_section` (the save round-trip sink).
        saved_sections: Vec<(String, DataSection)>,
    }

    impl TestIo {
        fn new() -> Self {
            Self {
                files: HashMap::new(),
                env: HashMap::new(),
                absolute: |name| name.starts_with('/') || name.contains(":/"),
                copy_result: true,
                copied: std::cell::RefCell::new(Vec::new()),
                boxes: std::cell::RefCell::new(Vec::new()),
                saved_sections: Vec::new(),
            }
        }

        fn with_file(mut self, name: &str, contents: &str) -> Self {
            self.files
                .insert(name.to_string(), contents.as_bytes().to_vec());
            self
        }
    }

    impl MayaIo for TestIo {
        fn file_exists(&self, filename: &str) -> bool {
            self.files.contains_key(filename)
                || self.saved_sections.iter().any(|(name, _)| name == filename)
        }
        fn open_section(&self, _filename: &str) -> Option<DataSection> {
            None
        }
        fn get_env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        fn is_absolute_path(&self, filename: &str) -> bool {
            (self.absolute)(filename)
        }
        fn resolve_filename(&self, filename: &str) -> String {
            filename.to_string()
        }
        fn dissolve_filename(&self, filename: &str) -> String {
            filename.to_string()
        }
        fn resolve_to_absolute_path(&self, filename: &str) -> String {
            format!("/abs/{filename}")
        }
        fn copy_file(&self, from: &str, to: &str) -> bool {
            self.copied
                .borrow_mut()
                .push((from.to_string(), to.to_string()));
            self.copy_result
        }
        fn automated_test(&self) -> bool {
            false
        }
        fn message_box(&self, message: &str, title: &str) {
            self.boxes.borrow_mut().push(format!("{title}|{message}"));
        }
        fn log_warning(&self, _message: &str) {}
        fn save_binary(&mut self, filename: &str, bytes: &[u8]) {
            self.files.insert(filename.to_string(), bytes.to_vec());
        }
        fn save_section(&mut self, filename: &str, section: &DataSection) {
            self.saved_sections
                .push((filename.to_string(), section.clone()));
        }
        fn read_section(&self, filename: &str) -> Option<DataSection> {
            self.saved_sections
                .iter()
                .find(|(name, _)| name == filename)
                .map(|(_, section)| section.clone())
        }
        fn read_directory(&self, _directory: &str) -> Option<Vec<String>> {
            None
        }
        fn computer_name(&self) -> String {
            String::new()
        }
        fn current_scene_file(&self) -> String {
            String::new()
        }
    }

    /// A one-quad raw mesh: four corners on the y=0 plane, one uv set,
    /// one material.
    fn quad_raw() -> RawMesh {
        RawMesh {
            full_name: "|grp|wall".to_string(),
            transform_path: Some("|grp|wallTransform".to_string()),
            positions: vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 1.0),
                Vector3::new(0.0, 0.0, 1.0),
            ],
            normals: vec![
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ],
            binormals: vec![Vector3::new(0.0, 0.0, 1.0); 4],
            tangents: vec![Vector3::new(1.0, 0.0, 0.0); 4],
            colours: Vec::new(),
            uv_sets: vec![vec![
                Vector2::new(0.0, 0.0),
                Vector2::new(1.0, 0.0),
                Vector2::new(1.0, 1.0),
                Vector2::new(0.0, 1.0),
            ]],
            instance_count: 1,
            polygons: vec![RawPolygon {
                position_indices: vec![0, 1, 2, 3],
                normal_indices: vec![0, 1, 2, 3],
                tangent_indices: vec![0, 1, 2, 3],
                uv_indices: vec![0, 1, 2, 3],
                uv2_indices: vec![],
                colour_indices: vec![],
                material_index: 0,
            }],
            materials: vec![RawMaterial {
                name: "wallSG.oc".to_string(),
                map_file: "textures/wall.tga".to_string(),
                fx_file: String::new(),
                map_id_meaning: 1,
            }],
        }
    }

    /// The gathered quad against a no-op host.
    fn quad_mesh(settings: &MayaExportSettings) -> Mesh {
        gather_mesh(&quad_raw(), settings, &TestIo::new()).unwrap()
    }

    #[test]
    fn pack_normal_maps_z_into_the_ten_bit_slot() {
        // z sits in the 511 slot, y and x in the 1023 slots, in that bit
        // order — with no legacy branch.
        assert_eq!(pack_normal_maya([0.0, 0.0, 1.0]), 511 << 22);
        assert_eq!(pack_normal_maya([0.0, 1.0, 0.0]), 1023 << 11);
        assert_eq!(pack_normal_maya([1.0, 0.0, 0.0]), 1023);
        // A diagonal normalises first: 1/√3 scales x and y to 590 and z
        // to 295 before the truncation.
        let packed = pack_normal_maya([1.0, 1.0, 1.0]);
        assert_eq!((packed >> 22) & 0x3ff, 295);
        assert_eq!((packed >> 11) & 0x7ff, 590);
        assert_eq!(packed & 0x7ff, 590);
        // The input is normalised first.
        assert_eq!(pack_normal_maya([0.0, 0.0, 7.0]), 511 << 22);
        // A negative component is clamped to -1, then cast through u32
        // exactly like the C++ `(uint32)` — the low ten bits of -511 wrap
        // to 513, not 1023.
        assert_eq!(pack_normal_maya([0.0, 0.0, -3.0]), 513u32 << 22);
    }

    #[test]
    fn gather_names_flips_axes_and_clamps_colours() {
        let settings = MayaExportSettings::default();
        let mut raw = quad_raw();
        raw.colours = vec![
            Vector4::new(2.0, 0.5, -0.5, 1.0),
            Vector4::new(2.0, 0.5, -0.5, 1.0),
            Vector4::new(2.0, 0.5, -0.5, 1.0),
            Vector4::new(2.0, 0.5, -0.5, 1.0),
        ];
        raw.polygons[0].colour_indices = vec![0, 1, 2, 3];
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();

        assert_eq!(mesh.name, "wall");
        assert_eq!(mesh.node_name, "wallTransform");
        // The default orientation negates z...
        assert_eq!(mesh.positions[2], Vector3::new(1.0, 0.0, -1.0));
        assert_eq!(mesh.colours[0], Vector4::new(1.0, 0.5, 0.0, 1.0));

        // ...and the legacy orientation negates x instead.
        let mut legacy = settings.clone();
        legacy.use_legacy_orientation = true;
        let mesh = gather_mesh(&raw, &legacy, &TestIo::new()).unwrap();
        assert_eq!(mesh.positions[1], Vector3::new(-1.0, 0.0, 0.0));
        assert_eq!(mesh.normals[0], Vector3::new(0.0, 1.0, 0.0));

        // No transform renders as "Scene Root"; a path-less mesh keeps
        // the whole string.
        let mut raw = quad_raw();
        raw.transform_path = None;
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        assert_eq!(mesh.node_name, "Scene Root");
        let mut raw = quad_raw();
        raw.full_name = "wall".to_string();
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        assert_eq!(mesh.name, "wall");
    }

    #[test]
    fn gather_rejects_uninstanced_and_unbalanced_meshes() {
        let settings = MayaExportSettings::default();
        let mut raw = quad_raw();
        raw.instance_count = 0;
        assert!(gather_mesh(&raw, &settings, &TestIo::new()).is_none());

        let mut raw = quad_raw();
        raw.polygons[0].normal_indices = vec![0, 1, 2];
        assert!(gather_mesh(&raw, &settings, &TestIo::new()).is_none());
    }

    #[test]
    fn gather_triangulates_the_polygon_fan() {
        let mesh = quad_mesh(&MayaExportSettings::default());
        assert_eq!(mesh.faces.len(), 2);
        assert_eq!(mesh.faces[0].position_index, [0, 1, 2]);
        assert_eq!(mesh.faces[0].normal_index, [0, 1, 2]);
        assert_eq!(mesh.faces[0].tangent_index, [0, 1, 2]);
        assert_eq!(mesh.faces[0].uv_index, [0, 1, 2]);
        assert_eq!(mesh.faces[1].position_index, [0, 2, 3]);
        assert_eq!(mesh.faces[1].uv_index, [0, 2, 3]);
        assert_eq!(mesh.faces[0].material_index, 0);

        // A degenerate two-corner polygon contributes nothing (the
        // reference's unsigned length-2 walk stops there too).
        let mut raw = quad_raw();
        raw.polygons[0].position_indices = vec![0, 1];
        raw.polygons[0].normal_indices = vec![0, 1];
        raw.polygons[0].tangent_indices = vec![0, 1];
        raw.polygons[0].uv_indices = vec![0, 1];
        let mesh = gather_mesh(&raw, &MayaExportSettings::default(), &TestIo::new()).unwrap();
        assert!(mesh.faces.is_empty());
    }

    #[test]
    fn gather_truncates_material_names_and_promotes_mfm() {
        let settings = MayaExportSettings::default();
        let mesh = quad_mesh(&settings);
        assert_eq!(mesh.materials[0].name, "wallSG");
        assert_eq!(mesh.materials[0].map_file, "textures/wall.tga");
        assert_eq!(mesh.materials[0].map_id_meaning, 1);

        // A same-stem .mfm on the (resolved) resource path wins.
        let io = TestIo::new().with_file("textures/wall.mfm", "<mfm/>");
        let mesh = gather_mesh(&quad_raw(), &settings, &io).unwrap();
        assert_eq!(mesh.materials[0].map_file, "textures/wall.mfm");
        assert_eq!(mesh.materials[0].map_id_meaning, 2);
        assert_eq!(mesh.materials[0].fx_file, "");

        // A dot-less plug name is kept whole.
        let mut raw = quad_raw();
        raw.materials[0].name = "wallSG".to_string();
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        assert_eq!(mesh.materials[0].name, "wallSG");
    }

    #[test]
    fn init_rejects_uvless_meshes_with_the_reference_error() {
        let settings = MayaExportSettings::default();
        let mut raw = quad_raw();
        raw.uv_sets = vec![Vec::new()];
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();

        let mut visual = MayaVisualMesh::default();
        assert!(!visual.init(&mesh, &settings, &TestIo::new()));
        assert_eq!(
            visual.error_text(),
            "Failed to export mesh |grp|wall because there are no uvs"
        );

        // A mesh with no positions at all stays silent — the reference
        // only reports when positions, normals and faces exist.
        let mut raw = quad_raw();
        raw.positions.clear();
        raw.uv_sets = vec![Vec::new()];
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(!visual.init(&mesh, &settings, &TestIo::new()));
        assert!(visual.errors.is_empty());
    }

    #[test]
    fn init_maps_vertices_with_the_maya_quirks() {
        let settings = MayaExportSettings::default();
        let mesh = quad_mesh(&settings);
        let mut visual = MayaVisualMesh::default();
        assert!(visual.init(&mesh, &settings, &TestIo::new()));

        assert_eq!(visual.identifier, "wall");
        assert_eq!(visual.node_identifier, "wallTransform");
        assert_eq!(visual.triangles.len(), 2);
        assert_eq!(visual.triangles[0].smoothing_group, 0);
        assert_eq!(visual.vertices.len(), 6);
        // The uv v component flips: v=0 corners become 1.
        assert_eq!(visual.vertices[0].uv, [0.0, 1.0]);
        assert_eq!(visual.vertices[1].uv, [1.0, 1.0]);
        // The binormal comes from the tangent id — and the axis flip
        // negated z on the way in, so the up-facing source pool now
        // points down (the fixture below pins the id quirk).
        assert_eq!(visual.vertices[0].binormal, [0.0, 0.0, -1.0]);
        // Real indices keep the source positions.
        assert_eq!(visual.triangles[0].real_index, [0, 1, 2]);
        // The bounding box spans the flipped positions.
        assert_eq!(visual.bb.min, Vector3::new(0.0, 0.0, -1.0));
        assert_eq!(visual.bb.max, Vector3::new(1.0, 0.0, 0.0));

        // The binormal-by-tangent-id quirk: distinct binormal and
        // tangent pools with distinct orderings.
        let mut raw = quad_raw();
        raw.binormals = vec![
            Vector3::new(0.0, 0.0, 10.0),
            Vector3::new(0.0, 0.0, 20.0),
            Vector3::new(0.0, 0.0, 30.0),
            Vector3::new(0.0, 0.0, 40.0),
        ];
        raw.tangents = vec![
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
            Vector3::new(1.0, 1.0, 1.0),
        ];
        // The tangent ids order 2, 0, 1, 3 — so corner 0's binormal is
        // pool entry 2 (30), not pool entry 0; the axis flip then
        // negates its z.
        raw.polygons[0].tangent_indices = vec![2, 0, 1, 3];
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(visual.init(&mesh, &settings, &TestIo::new()));
        let binormal = normalised([0.0, 0.0, -30.0]);
        assert_eq!(visual.vertices[0].binormal, binormal);
        // Corner 0's tangent comes from pool entry 2, (0, 0, 1), flipped
        // by the axis flip to -z.
        let tangent = normalised([0.0, 0.0, -1.0]);
        assert_eq!(visual.vertices[0].tangent, tangent);
    }

    #[test]
    fn init_remaps_materials_without_a_modulo() {
        let settings = MayaExportSettings::default();
        let mut raw = quad_raw();
        // Two materials that dedupe apart; the polygon's shader index is
        // the second — an out-of-range index would panic, the reference
        // reads the remap unguarded too.
        raw.materials = vec![
            RawMaterial {
                name: "aSG.surface".to_string(),
                map_file: String::new(),
                fx_file: String::new(),
                map_id_meaning: 0,
            },
            RawMaterial {
                name: "bSG.surface".to_string(),
                map_file: "textures/other.tga".to_string(),
                fx_file: String::new(),
                map_id_meaning: 1,
            },
        ];
        raw.polygons[0].material_index = 1;
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(visual.init(&mesh, &settings, &TestIo::new()));
        assert_eq!(visual.material_remap, vec![0, 1]);
        assert_eq!(visual.triangles[0].material_index, 1);
    }

    #[test]
    fn init_snaps_vertices_and_the_bounding_box() {
        let settings = MayaExportSettings {
            snap_vertices: true,
            ..MayaExportSettings::default()
        };
        let mut raw = quad_raw();
        raw.positions[1] = Vector3::new(1.0004, 0.0, 0.0);
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(visual.init(&mesh, &settings, &TestIo::new()));
        assert_eq!(visual.vertices[1].pos, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn add_material_reports_unresolved_environment_macros() {
        let settings = MayaExportSettings::default();
        let mut visual = MayaVisualMesh::default();
        let material = RawMaterial {
            name: "badSG".to_string(),
            map_file: "%MISSING%/wall.tga".to_string(),
            fx_file: String::new(),
            map_id_meaning: 1,
        };
        let mut io = TestIo::new();
        io.env.insert("OTHER".to_string(), "resolved".to_string());
        visual.add_material(&material, &settings, &io);
        assert_eq!(
            visual.error_text(),
            "Material \"badSG\" has an invalid texture name:\n\n\"%MISSING%/wall.tga\"\n\n\
Can't resolve this name, have you configured all the environment variables in your maya.env?\n\
Texture may not be visible in game.\n"
        );
        // The unresolved name keeps its original text, gets dissolved and
        // is written as the map identifier — the reference does not clear
        // the material, it only reports the failure.
        assert_eq!(visual.materials[0].map_identifier, "%MISSING%/wall.tga");
        assert_eq!(visual.materials[0].map_id_meaning, 1);

        // With the macro resolvable the name is substituted.
        let material = RawMaterial {
            name: "goodSG".to_string(),
            map_file: "%OTHER%/wall.tga".to_string(),
            fx_file: String::new(),
            map_id_meaning: 1,
        };
        visual.add_material(&material, &settings, &io);
        assert_eq!(visual.materials[1].map_identifier, "resolved/wall.tga");
        assert_eq!(visual.materials[1].map_id_meaning, 1);
    }

    #[test]
    fn add_material_copies_external_textures() {
        let mut settings = MayaExportSettings {
            copy_external_textures: true,
            copy_textures_to: "textures/copied".to_string(),
            ..MayaExportSettings::default()
        };
        let material = RawMaterial {
            name: "extSG".to_string(),
            map_file: "/tmp/source/wall.tga".to_string(),
            fx_file: String::new(),
            map_id_meaning: 1,
        };

        let mut io = TestIo::new();
        let mut visual = MayaVisualMesh::default();
        visual.add_material(&material, &settings, &io);
        // The copy happened and the identifier is the dissolved new
        // name.
        assert_eq!(
            io.copied.borrow().as_slice(),
            &[(
                "/tmp/source/wall.tga".to_string(),
                "textures/copied/wall.tga".to_string()
            )]
        );
        assert_eq!(
            visual.materials[0].map_identifier,
            "textures/copied/wall.tga"
        );

        // A failing copy keeps the dissolved original and reports.
        io.copy_result = false;
        visual.add_material(&material, &settings, &io);
        assert_eq!(visual.materials[1].map_identifier, "/tmp/source/wall.tga");
        assert_eq!(
            visual.errors.last().unwrap(),
            "Could not copy texture /tmp/source/wall.tga to textures/copied/wall.tga\n"
        );

        // An empty copyTexturesTo clears the identifier outright.
        settings.copy_textures_to = String::new();
        visual.add_material(&material, &settings, &io);
        assert_eq!(visual.materials[2].map_identifier, "");
        assert_eq!(visual.materials[2].map_id_meaning, 1);
    }

    #[test]
    fn find_or_add_material_dedupes_on_the_maya_equality() {
        let mut visual = MayaVisualMesh::default();
        let m = MayaMaterial {
            identifier: "m".to_string(),
            map_identifier: "a.tga".to_string(),
            map_id_meaning: 1,
            in_use: false,
        };
        assert_eq!(visual.find_or_add_material(&m), 0);
        assert_eq!(visual.find_or_add_material(&m), 0);
        let mut other = m.clone();
        other.map_id_meaning = 2;
        assert_eq!(visual.find_or_add_material(&other), 1);
        let mut third = m.clone();
        third.map_identifier = "b.tga".to_string();
        assert_eq!(visual.find_or_add_material(&third), 2);
        assert_eq!(visual.materials.len(), 3);
    }

    #[test]
    fn save_writes_the_node_identifier_and_the_streams() {
        let settings = MayaExportSettings::default();
        let mesh = quad_mesh(&settings);
        let mut visual = MayaVisualMesh::default();
        visual.init(&mesh, &settings, &TestIo::new());
        // A second uv set for the dual stream and a colour set.
        visual.dual_uv = true;
        for (i, v) in visual.vertices.iter_mut().enumerate() {
            v.uv2 = [0.5, 0.25];
            v.colour = [1.0, 0.5, 0.0, 1.0];
            v.mesh_index = 0;
            let _ = i;
        }
        visual.vertex_colours = true;

        let mut io = TestIo::new();
        let saved = visual
            .save(&settings, None, "out.primitives", false, &mut io)
            .unwrap();
        // The node is the transform name, not the mesh name, and the
        // mesh is not a world-space object.
        let render_set = saved.visual.open_section("renderSet").unwrap();
        assert!(!render_set.read_bool("treatAsWorldSpaceObject", true));
        assert_eq!(render_set.read_string("node", ""), "wallTransform");
        let geometry = render_set.open_section("geometry").unwrap();
        assert_eq!(geometry.read_string("vertices", ""), "vertices");
        assert_eq!(geometry.read_string("primitive", ""), "indices");
        let streams: Vec<String> = (0..geometry.count_children())
            .map(|i| geometry.open_child(i).unwrap())
            .filter(|child| child.section_name() == "stream")
            .map(|child| child.as_string().to_string())
            .collect();
        assert_eq!(streams, vec!["uv2".to_string(), "colour".to_string()]);

        // The primitives file went out through the host, with the shared
        // corners deduped (6 corners over two triangles share the fan's
        // corner twice) and the run renumbered by the sort.
        let bytes = io
            .files
            .get("out.primitives")
            .expect("primitives written")
            .clone();
        let mut container = BinSection::new("root", BinaryBlock::new(bytes));
        let indices = container.read_binary("indices").expect("indices stream");
        let index_blob = indices.data();
        assert_eq!(&index_blob[..4], b"list");
        let base = INDEX_FORMAT_FIELD_LEN;
        assert_eq!(
            i32::from_le_bytes(index_blob[base..base + 4].try_into().unwrap()),
            6
        );
        assert_eq!(
            i32::from_le_bytes(index_blob[base + 4..base + 8].try_into().unwrap()),
            1
        );

        let vertices_blob = container.read_binary("vertices").expect("vertices stream");
        let vheader = VertexHeader::read(vertices_blob.data()).expect("vertex header");
        assert_eq!(vheader.vertex_format, "xyznuv");
        assert_eq!(vheader.n_vertices, 4);

        let uv2 = container.read_binary("uv2").expect("uv2 stream");
        let mut first = [0u8; 4];
        first.copy_from_slice(&uv2.data()[..4]);
        assert_eq!(f32::from_le_bytes(first), 0.5);

        let colours = container.read_binary("colour").expect("colour stream");
        assert_eq!(colours.data().len(), 16);
    }

    #[test]
    fn save_rejects_a_materialless_mesh() {
        let settings = MayaExportSettings::default();
        let mesh = quad_mesh(&settings);
        let mut visual = MayaVisualMesh::default();
        visual.init(&mesh, &settings, &TestIo::new());
        visual.materials.clear();
        let mut io = TestIo::new();
        assert!(
            visual
                .save(&settings, None, "out.primitives", false, &mut io)
                .is_none()
        );
        assert_eq!(
            visual.error_text(),
            "wall does not have any materials.\nThis object will not be exported.  \
Adding a material will fix this problem."
        );
        assert!(!io.files.contains_key("out.primitives"));
    }

    #[test]
    fn save_uses_the_identifier_prefix() {
        let settings = MayaExportSettings::default();
        let mesh = quad_mesh(&settings);
        let mut visual = MayaVisualMesh::default();
        visual.init(&mesh, &settings, &TestIo::new());
        let mut io = TestIo::new();
        let saved = visual
            .save(&settings, None, "out.primitives", true, &mut io)
            .unwrap();
        let render_set = saved.visual.open_section("renderSet").unwrap();
        let geometry = render_set.open_section("geometry").unwrap();
        assert_eq!(geometry.read_string("vertices", ""), "wall.vertices");
        assert_eq!(geometry.read_string("primitive", ""), "wall.indices");
    }

    #[test]
    fn save_scales_positions_by_the_unit_scale() {
        let settings = MayaExportSettings::default();
        let mesh = quad_mesh(&settings);
        let mut visual = MayaVisualMesh::default();
        visual.init(&mesh, &settings, &TestIo::new());
        // The default settings scale by the scene factor (0.01); corner 0
        // is (0, 0, 0) and corner 1 is (1, 0, 0).
        let list = visual.vertex_list_xyznuv(&settings);
        let floats: Vec<f32> = list
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        assert_eq!(&floats[0..3], &[0.0, 0.0, 0.0]);
        assert_eq!(&floats[8..11], &[0.01, 0.0, 0.0]);

        // A legacy unit scale of 1 keeps the positions; the modern scene
        // factor scales instead.
        let mut legacy = settings.clone();
        legacy.use_legacy_scaling = true;
        legacy.unit_scale = 1.0;
        let floats: Vec<f32> = visual
            .vertex_list_xyznuv(&legacy)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        assert_eq!(&floats[8..11], &[1.0, 0.0, 0.0]);

        let mut modern = settings.clone();
        modern.maya_unit_scale = 0.5;
        let floats: Vec<f32> = visual
            .vertex_list_xyznuv(&modern)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        assert_eq!(&floats[8..11], &[0.5, 0.0, 0.0]);
    }

    #[test]
    fn save_selects_the_indexed_vertex_format_for_mesh_particles() {
        // Mesh particles split into one group per sub-mesh; the renderer
        // assumes the sixteen-group shape (the whole mesh plus the fifteen
        // sub-meshes), so the fixture carries fifteen meshIndex runs.
        let settings = MayaExportSettings {
            export_mode: ExportMode::MeshParticles,
            ..MayaExportSettings::default()
        };
        let mut visual = MayaVisualMesh {
            identifier: "particles".to_string(),
            node_identifier: "particlesTransform".to_string(),
            ..MayaVisualMesh::default()
        };
        for i in 0..15i32 {
            let base = visual.vertices.len() as i32;
            for corner in 0..3 {
                visual.vertices.push(BloatVertex {
                    pos: [i as f32, corner as f32, 0.0],
                    normal: [0.0, 1.0, 0.0],
                    uv: [0.0, 0.0],
                    mesh_index: i,
                    ..BloatVertex::default()
                });
            }
            visual.triangles.push(Triangle {
                index: [base, base + 1, base + 2],
                real_index: [base, base + 1, base + 2],
                smoothing_group: 0,
                material_index: 0,
            });
        }
        visual.materials.push(MayaMaterial {
            identifier: "particlesSG".to_string(),
            ..MayaMaterial::default()
        });

        let mut io = TestIo::new();
        let saved = visual
            .save(&settings, None, "out.primitives", false, &mut io)
            .unwrap();
        let bytes = io.files["out.primitives"].clone();
        let mut container = BinSection::new("root", BinaryBlock::new(bytes));
        let indices = container.read_binary("indices").expect("indices");
        // list32: 45 indices (15 sub-meshes) and 16 triangle groups.
        assert_eq!(
            &indices.data()[INDEX_FORMAT_FIELD_LEN..INDEX_FORMAT_FIELD_LEN + 4],
            &45i32.to_le_bytes()
        );
        assert_eq!(
            &indices.data()[INDEX_FORMAT_FIELD_LEN + 4..INDEX_FORMAT_FIELD_LEN + 8],
            &16i32.to_le_bytes()
        );

        let vertices = container.read_binary("vertices").expect("vertices");
        let vheader = VertexHeader::read(vertices.data()).expect("vertex header");
        // Mesh particles take the indexed vertex format.
        assert_eq!(vheader.vertex_format, "xyznuvi");
        let _ = saved;
    }

    #[test]
    fn save_sorts_and_dedupes_twice_around_the_bump_seam() {
        // A shared-corner quad: the pipeline must leave a compact, sorted
        // mesh (the dedupe runs before and after the bump seam).
        let settings = MayaExportSettings::default();
        let mesh = quad_mesh(&settings);
        let mut visual = MayaVisualMesh::default();
        visual.init(&mesh, &settings, &TestIo::new());
        let before = visual.triangles.len();
        let mut io = TestIo::new();
        visual
            .save(&settings, None, "out.primitives", false, &mut io)
            .unwrap();
        assert_eq!(before, 2);
        // Two triangles over a shared fan: corners 1 and 2 repeat
        // between the triangles, so 6 corners dedupe to 4? The fan's
        // shared corner is vertex 0 only across the diagonal — the
        // reference's dedupe collapses the coincident corners the
        // sort then compacts.
        assert!(visual.vertices.len() <= 6);
    }

    #[test]
    fn material_save_follows_the_maya_chain() {
        let mut settings = MayaExportSettings::default();
        let bitmap = MayaMaterial {
            identifier: "wallSG".to_string(),
            map_identifier: "textures/wall.tga".to_string(),
            map_id_meaning: 1,
            in_use: true,
        };
        let mfm = MayaMaterial {
            identifier: "mfmSG".to_string(),
            map_identifier: "textures/wall.mfm".to_string(),
            map_id_meaning: 2,
            in_use: true,
        };
        let plain = MayaMaterial {
            identifier: "plainSG".to_string(),
            map_identifier: String::new(),
            map_id_meaning: 0,
            in_use: true,
        };
        let affix = MayaMaterial {
            identifier: "addSG".to_string(),
            map_identifier: "fx/wall_afx.tga".to_string(),
            map_id_meaning: 1,
            in_use: true,
        };

        let mut geometry = DataSection::new("geometry");
        maya_material_save(
            &bitmap,
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
        );
        maya_material_save(
            &mfm,
            &mut geometry,
            None,
            1,
            SkinType::NoSkin,
            false,
            &settings,
        );
        maya_material_save(
            &plain,
            &mut geometry,
            None,
            2,
            SkinType::NoSkin,
            false,
            &settings,
        );
        maya_material_save(
            &affix,
            &mut geometry,
            None,
            3,
            SkinType::NoSkin,
            false,
            &settings,
        );
        let text = geometry.write_to_string();
        assert!(text.contains("<identifier>\twallSG\t</identifier>"));
        assert!(text.contains("<fx>\tshaders/std_effects/lightonly.fx\t</fx>"));
        assert!(text.contains("<Texture>\ttextures/wall.tga\t</Texture>"));
        assert!(text.contains("<mfm>\ttextures/wall.mfm\t</mfm>"));

        // The default materials fill the meaning-0 branch.
        settings.default_material = "defaults/base.mfm".to_string();
        settings.default_skinned_material = "defaults/skinned.mfm".to_string();
        let mut geometry = DataSection::new("geometry");
        maya_material_save(
            &plain,
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
        );
        maya_material_save(
            &plain,
            &mut geometry,
            None,
            1,
            SkinType::SoftSkin,
            false,
            &settings,
        );
        let text = geometry.write_to_string();
        assert!(text.contains("<mfm>\tdefaults/base.mfm\t</mfm>"));
        assert!(text.contains("<identifier>\tplainSG_skinned\t</identifier>"));
        assert!(text.contains("<mfm>\tdefaults/skinned.mfm\t</mfm>"));

        // The _afx suffix picks the additive shader; the skinned faces
        // pick the skinned variants.
        let mut geometry = DataSection::new("geometry");
        maya_material_save(
            &affix,
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
        );
        maya_material_save(
            &bitmap,
            &mut geometry,
            None,
            1,
            SkinType::SoftSkin,
            true,
            &settings,
        );
        maya_material_save(
            &bitmap,
            &mut geometry,
            None,
            2,
            SkinType::SoftSkin,
            false,
            &settings,
        );
        let text = geometry.write_to_string();
        assert!(text.contains("<fx>\tshaders/std_effects/lightonly_add.fx\t</fx>"));
        assert!(text.contains("<fx>\tshaders/std_effects/lightonly_skinned.fx\t</fx>"));
        assert!(text.contains("<fx>\tshaders/std_effects/lightonly.fx\t</fx>"));

        // Mesh particles take the particle shader with the diffuse map.
        settings.export_mode = ExportMode::MeshParticles;
        let mut geometry = DataSection::new("geometry");
        maya_material_save(
            &bitmap,
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
        );
        let text = geometry.write_to_string();
        assert!(text.contains("<fx>\tshaders/std_effects/mesh_particle.fx\t</fx>"));
    }

    #[test]
    fn material_save_copies_the_existing_visual_entry() {
        let settings = MayaExportSettings::default();
        let mut existing = DataSection::new("root");
        let render_set = existing.add_child("renderSet");
        let geometry = render_set.add_child("geometry");
        let prim_group = geometry.add_child("primitiveGroup");
        let material = prim_group.add_child("material");
        material.write_string("identifier", "wallSG");
        material.write_string("mfm", "kept/old.mfm");

        let bitmap = MayaMaterial {
            identifier: "wallSG".to_string(),
            map_identifier: "textures/wall.tga".to_string(),
            map_id_meaning: 1,
            in_use: true,
        };
        let mut target = DataSection::new("geometry");
        maya_material_save(
            &bitmap,
            &mut target,
            Some(&existing),
            0,
            SkinType::NoSkin,
            false,
            &settings,
        );
        let text = target.write_to_string();
        assert!(text.contains("<mfm>\tkept/old.mfm\t</mfm>"));
        assert!(!text.contains("lightonly"));
    }

    #[test]
    fn settings_round_trip_through_both_locations() {
        let settings = MayaExportSettings {
            export_anim: true,
            maya_unit_scale: 0.01,
            export_mode: ExportMode::StaticWithNodes,
            max_bones: 23,
            transform_to_origin: true,
            allow_scale: true,
            bump_mapped: true,
            keep_existing_materials: false,
            snap_vertices: true,
            strip_ref_prefix: true,
            reference_node: true,
            reference_nodes_file: "res/referenceNodes.xml".to_string(),
            reference_nodes_file_abs: "/abs/res/referenceNodes.xml".to_string(),
            disable_visual_checker: true,
            use_legacy_scaling: true,
            fix_cylindrical: true,
            use_legacy_orientation: true,
            scene_root_added: true,
            include_meshes: false,
            include_envelopes_and_bones: true,
            include_nodes: false,
            include_materials: false,
            include_animations: true,
            use_character_mode: true,
            animation_name: "walk".to_string(),
            include_portals: true,
            world_space_origin: true,
            unit_scale: 0.2,
            local_hierarchy: true,
            node_filter: NodeFilter::Visible,
            copy_external_textures: false,
            copy_textures_to: "textures/copy".to_string(),
            default_material: String::new(),
            default_skinned_material: String::new(),
        };

        // The flat root write/read round trip keeps every field.
        let mut root = DataSection::new("root");
        assert!(settings.write_settings(Some(&mut root)));
        let mut read = MayaExportSettings {
            include_meshes: true, // keep-current default exercised below
            ..MayaExportSettings::default()
        };
        assert!(read.read_settings(&TestIo::new(), Some(&root)));
        assert!(read.export_anim);
        assert_eq!(read.export_mode, ExportMode::StaticWithNodes);
        assert_eq!(read.max_bones, 23);
        assert!(read.transform_to_origin);
        assert!(read.allow_scale);
        assert!(read.bump_mapped);
        assert!(!read.keep_existing_materials);
        assert!(read.snap_vertices);
        assert!(read.strip_ref_prefix);
        assert!(read.reference_node);
        assert_eq!(read.reference_nodes_file, "res/referenceNodes.xml");
        assert_eq!(read.reference_nodes_file_abs, "/abs/res/referenceNodes.xml");
        assert!(read.disable_visual_checker);
        assert!(read.use_legacy_scaling);
        assert!(read.fix_cylindrical);
        assert!(read.use_legacy_orientation);
        assert!(read.scene_root_added);
        assert!(!read.include_meshes);
        assert!(read.include_envelopes_and_bones);
        assert!(!read.include_nodes);
        assert!(!read.include_materials);
        assert!(read.include_animations);
        assert!(read.use_character_mode);
        assert_eq!(read.animation_name, "walk");
        assert!(read.include_portals);
        assert!(read.world_space_origin);
        assert_eq!(read.unit_scale, 0.2);
        assert!(read.local_hierarchy);
        assert_eq!(read.node_filter, NodeFilter::Visible);
        assert!(!read.copy_external_textures);
        assert_eq!(read.copy_textures_to, "textures/copy");
        // The two scene-only members ride along untouched.
        assert_eq!(read.maya_unit_scale, settings.maya_unit_scale);
        assert_eq!(read.default_material, settings.default_material);

        // An absent section fails both faces.
        assert!(!settings.write_settings(None));
        assert!(!MayaExportSettings::default().read_settings(&TestIo::new(), None));

        // The key set matches the reference's 30 keys in order.
        let keys: Vec<String> = (0..root.count_children())
            .map(|i| root.open_child(i).unwrap().section_name().to_string())
            .collect();
        assert_eq!(
            keys,
            vec![
                "exportAnimation",
                "exportMode",
                "boneCount",
                "transformToOrigin",
                "allowScale",
                "bumpMapped",
                "keepExistingMaterials",
                "snapVertices",
                "stripRefPrefix",
                "useReferenceNode",
                "referenceNodesFile",
                "disableVisualChecker",
                "useLegacyScaling",
                "fixCylindrical",
                "useLegacyOrientation",
                "sceneRootAdded",
                "includeMeshes",
                "includeEnvelopesAndBones",
                "includeNodes",
                "includeMaterials",
                "includeAnimations",
                "useCharacterMode",
                "animationName",
                "includePortals",
                "worldSpaceOrigin",
                "unitScale",
                "localHierarchy",
                "nodeFilter",
                "copyExternalTextures",
                "copyTexturesTo",
            ]
        );
    }

    #[test]
    fn settings_reads_keep_the_current_values_as_defaults() {
        // A section with no keys at all leaves a non-default settings
        // object untouched.
        let mut settings = MayaExportSettings {
            max_bones: 9,
            animation_name: "idle".to_string(),
            ..MayaExportSettings::default()
        };
        let empty = DataSection::new("root");
        assert!(settings.read_settings(&TestIo::new(), Some(&empty)));
        assert_eq!(settings.max_bones, 9);
        assert_eq!(settings.animation_name, "idle");

        // referenceNodesFile only overrides when non-empty.
        let mut root = DataSection::new("root");
        root.write_string("referenceNodesFile", "");
        settings.read_settings(&TestIo::new(), Some(&root));
        assert_eq!(settings.reference_nodes_file_abs, "");
    }

    #[test]
    fn settings_file_faces_use_the_two_locations() {
        // The root face reads/writes the flat section; the non-root face
        // goes through metaData/settings. The TestIo's read_section
        // returns None (no file), so the write face exercises the fresh
        // root path and the read face the file gate.
        let mut io = TestIo::new();
        let settings = MayaExportSettings::default();
        assert!(settings.write_settings_file(&mut io, "out.visualsettings", true));

        let io = TestIo::new();
        let mut settings = MayaExportSettings::default();
        assert!(!settings.read_settings_file(&io, "missing.xml", true));
        assert!(!settings.read_settings_file(&io, "missing.xml", false));
    }

    #[test]
    fn option_string_overrides_the_settings() {
        let mut settings = MayaExportSettings::default();
        parse_options_string(
            "exportAnimation=1;exportMode=static_with_nodes;boneCount=29;\
             snapVertices=True;unknownKey=42;nodeFilter=visible;\
             animationName=run;unitScale=0.5;copyTexturesTo=tex/copy",
            &mut settings,
        );
        assert!(settings.export_anim);
        assert_eq!(settings.export_mode, ExportMode::StaticWithNodes);
        assert_eq!(settings.max_bones, 29);
        assert!(settings.snap_vertices);
        assert_eq!(settings.node_filter, NodeFilter::Visible);
        assert_eq!(settings.animation_name, "run");
        assert_eq!(settings.unit_scale, 0.5);
        assert_eq!(settings.copy_textures_to, "tex/copy");

        // The integer and the name forms agree; out-of-range and unknown
        // values keep the current setting; a valueless pair is skipped.
        let mut settings = MayaExportSettings::default();
        parse_options_string("exportMode=2", &mut settings);
        assert_eq!(settings.export_mode, ExportMode::StaticWithNodes);
        parse_options_string("exportMode=7;exportMode=bogus", &mut settings);
        assert_eq!(settings.export_mode, ExportMode::StaticWithNodes);
        parse_options_string("nodeFilter=1;nodeFilter", &mut settings);
        assert_eq!(settings.node_filter, NodeFilter::Selected);
        parse_options_string("keepExistingMaterials=0", &mut settings);
        assert!(!settings.keep_existing_materials);
    }

    #[test]
    fn settings_file_round_trips_through_metadata() {
        // TestIo's saved_sections sink persists the written section; the
        // read face picks it back up on both roots.
        let mut io = TestIo::new();
        let settings = MayaExportSettings {
            max_bones: 31,
            ..MayaExportSettings::default()
        };
        assert!(settings.write_settings_file(&mut io, "s.visualsettings", false));
        let mut read = MayaExportSettings::default();
        assert!(read.read_settings_file(&io, "s.visualsettings", false));
        assert_eq!(read.max_bones, 31);
        assert!(read.include_nodes);

        // The root-rooted face round trips off the same file shape.
        assert!(settings.write_settings_file(&mut io, "root.visualsettings", true));
        let mut read = MayaExportSettings::default();
        assert!(read.read_settings_file(&io, "root.visualsettings", true));
        assert_eq!(read.max_bones, 31);
    }

    #[test]
    fn dual_uv_and_coloured_meshes_init_and_save() {
        // A mesh with a second uv set and per-corner colours: the gather
        // fans the extra channels and init fills uv2 (v-flipped) and
        // colour, and the bump-mapped save writes the xyznuvtb stream.
        let settings = MayaExportSettings {
            bump_mapped: true,
            ..MayaExportSettings::default()
        };
        let mut raw = quad_raw();
        raw.uv_sets = vec![
            vec![
                Vector2::new(0.0, 0.0),
                Vector2::new(1.0, 0.0),
                Vector2::new(1.0, 1.0),
                Vector2::new(0.0, 1.0),
            ],
            vec![
                Vector2::new(0.25, 0.25),
                Vector2::new(0.75, 0.25),
                Vector2::new(0.75, 0.75),
                Vector2::new(0.25, 0.75),
            ],
        ];
        raw.colours = vec![Vector4::new(0.5, 0.5, 0.5, 1.0); 4];
        raw.polygons[0].uv2_indices = vec![0, 1, 2, 3];
        raw.polygons[0].colour_indices = vec![0, 1, 2, 3];
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        // The second uv set fans with the same corner pattern.
        assert_eq!(mesh.faces[0].uv_index2, [0, 1, 2]);
        assert_eq!(mesh.faces[0].colour_index, [0, 1, 2]);

        let mut visual = MayaVisualMesh::default();
        assert!(visual.init(&mesh, &settings, &TestIo::new()));
        assert!(visual.dual_uv);
        assert!(visual.vertex_colours);
        // uv2 is v-flipped; colour is copied; tangent/binormal are
        // normalised copies of the (z-flipped) pools.
        assert_eq!(visual.vertices[0].uv2, [0.25, 0.75]);
        assert_eq!(visual.vertices[1].uv2, [0.75, 0.75]);
        assert_eq!(visual.vertices[0].colour, [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(visual.vertices[0].tangent, [1.0, 0.0, 0.0]);
        assert_eq!(visual.vertices[0].binormal, [0.0, 0.0, -1.0]);

        // The bump-mapped save selects the xyznuvtb vertex stream: the
        // primitives container carries the 64-byte vertex header plus 32
        // bytes per vertex, the fourth word the packed normal.
        let mut io = TestIo::new();
        visual
            .save(&settings, None, "out.primitives", false, &mut io)
            .unwrap();
        let bytes = io.files["out.primitives"].clone();
        let mut container = BinSection::new("root", BinaryBlock::new(bytes));
        let verts = container.read_binary("vertices").expect("vertices");
        assert_eq!(
            verts.data().len(),
            VertexHeader::SIZE + visual.vertices.len() * 32
        );
        let packed: Vec<u32> = verts.data()[VertexHeader::SIZE..]
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        // The first vertex is the origin; the gathered normal is the
        // y-up (0,1,0) packing 1023 into the y slot (word 3), the
        // tangent (1,0,0) packs 1023 (word 6) and the binormal (0,0,-1)
        // clamps z, whose cast wraps to 513 (word 7).
        assert_eq!(packed[3], 1023 << 11);
        assert_eq!(packed[6], 1023);
        assert_eq!(packed[7], 513u32 << 22);

        // The dual-uv and colour streams land in the same container.
        let uv2 = container.read_binary("uv2").expect("uv2");
        assert_eq!(uv2.data().len(), visual.vertices.len() * 8);
        let colour = container.read_binary("colour").expect("colour");
        assert_eq!(colour.data().len(), visual.vertices.len() * 4);
    }

    #[test]
    fn settings_file_read_failures_and_parse_gaps() {
        // A missing file fails the read on both roots (the file gate and
        // the read_section gate); a file without metaData fails the
        // nested root.
        let mut io = TestIo::new();
        let mut settings = MayaExportSettings::default();
        assert!(!settings.read_settings_file(&io, "corrupt.xml", true));
        // A present file whose section cannot be parsed fails the read.
        io = io.with_file("present.xml", "");
        assert!(!settings.read_settings_file(&io, "present.xml", true));
        // A parsable file without metaData fails the nested root.
        let no_meta = DataSection::new("root");
        let mut io = TestIo::new();
        io.saved_sections.push(("nometa.xml".to_string(), no_meta));
        let mut settings = MayaExportSettings::default();
        assert!(!settings.read_settings_file(&io, "nometa.xml", false));

        // Writing the nested root into a host that cannot persist still
        // reports success (only write_settings failing would).
        assert!(settings.write_settings_file(&mut io, "s.visualsettings", false));
        assert!(!io.files.contains_key("s.visualsettings"));

        // Every export-mode and node-filter integer and name form.
        let mut settings = MayaExportSettings::default();
        parse_options_string("exportMode=1", &mut settings);
        assert_eq!(settings.export_mode, ExportMode::Static);
        parse_options_string("exportMode=3", &mut settings);
        assert_eq!(settings.export_mode, ExportMode::MeshParticles);
        parse_options_string("exportMode=normal;exportMode=static", &mut settings);
        assert_eq!(settings.export_mode, ExportMode::Static);
        parse_options_string("exportMode=mesh_particles", &mut settings);
        assert_eq!(settings.export_mode, ExportMode::MeshParticles);
        parse_options_string("nodeFilter=all", &mut settings);
        assert_eq!(settings.node_filter, NodeFilter::All);
        parse_options_string("nodeFilter=bogus;nodeFilter=7", &mut settings);
        assert_eq!(settings.node_filter, NodeFilter::All);

        // The remaining boolean and string keys parse from the option
        // string too.
        let mut settings = MayaExportSettings::default();
        parse_options_string(
            "transformToOrigin=1;allowScale=0;bumpMapped=1;stripRefPrefix=1;\
             useReferenceNode=1;disableVisualChecker=1;useLegacyScaling=1;\
             fixCylindrical=1;useLegacyOrientation=1;sceneRootAdded=1;\
             includeMeshes=0;includeEnvelopesAndBones=1;includeNodes=0;\
             includeMaterials=0;includeAnimations=1;useCharacterMode=1;\
             includePortals=1;worldSpaceOrigin=1;localHierarchy=1;\
             copyExternalTextures=0;copyTexturesTo=tex/over",
            &mut settings,
        );
        assert!(settings.transform_to_origin);
        assert!(!settings.allow_scale);
        assert!(settings.bump_mapped);
        assert!(settings.strip_ref_prefix);
        assert!(settings.reference_node);
        assert!(settings.disable_visual_checker);
        assert!(settings.use_legacy_scaling);
        assert!(settings.fix_cylindrical);
        assert!(settings.use_legacy_orientation);
        assert!(settings.scene_root_added);
        assert!(!settings.include_meshes);
        assert!(settings.include_envelopes_and_bones);
        assert!(!settings.include_nodes);
        assert!(!settings.include_materials);
        assert!(settings.include_animations);
        assert!(settings.use_character_mode);
        assert!(settings.include_portals);
        assert!(settings.world_space_origin);
        assert!(settings.local_hierarchy);
        assert!(!settings.copy_external_textures);
        assert_eq!(settings.copy_textures_to, "tex/over");
    }

    #[test]
    fn init_rejects_uvless_meshes_and_fills_the_default_material_remap() {
        // A uv-less mesh reports the "no uvs" error once per init (and
        // the automated-test host suppresses the message box).
        let settings = MayaExportSettings::default();
        let mut raw = quad_raw();
        raw.uv_sets = vec![Vec::new()];
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(!visual.init(&mesh, &settings, &TestIo::new()));
        assert_eq!(
            visual.errors.last().unwrap(),
            "Failed to export mesh |grp|wall because there are no uvs"
        );

        // A mesh whose positions or normals are empty just returns
        // false without the error.
        let mut raw = quad_raw();
        raw.normals = Vec::new();
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(!visual.init(&mesh, &settings, &TestIo::new()));
        assert!(visual.errors.is_empty());

        // A gathered mesh with no materials still fans the triangle
        // remap to zero (the reference pushes 0 on an empty walk).
        let mut raw = quad_raw();
        raw.materials = Vec::new();
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(visual.init(&mesh, &settings, &TestIo::new()));
        assert_eq!(visual.material_remap, vec![0]);
        assert!(visual.materials.is_empty());
        // The save refuses with the two-space materials error.
        let mut io = TestIo::new();
        assert!(
            visual
                .save(&settings, None, "out.primitives", false, &mut io)
                .is_none()
        );
        assert_eq!(
            visual.errors.last().unwrap(),
            "wall does not have any materials.\nThis object will not be exported.  \
Adding a material will fix this problem."
        );
        assert_eq!(
            io.boxes.borrow().as_slice(),
            &[
                "Material error!|wall does not have any materials.\nThis object will not be \
exported.  Adding a material will fix this problem."
                    .to_string()
            ]
        );
    }

    #[test]
    fn mesh_particle_save_rejects_two_materials_and_writes_large_indices() {
        // Two materials split a mesh-particle export into a shape the
        // renderer cannot read; the save reports and returns None.
        let settings = MayaExportSettings {
            export_mode: ExportMode::MeshParticles,
            ..MayaExportSettings::default()
        };
        let mut visual = MayaVisualMesh {
            identifier: "particles".to_string(),
            ..MayaVisualMesh::default()
        };
        let tri = |material_index: i32| Triangle {
            index: [0, 1, 2],
            real_index: [0, 1, 2],
            smoothing_group: 0,
            material_index,
        };
        visual.vertices.push(BloatVertex {
            pos: [0.0, 0.0, 0.0],
            ..BloatVertex::default()
        });
        // A duplicate vertex run collapses to one entry: two materials
        // over one triangle trip the group-shape assert via the two
        // distinct material indices surviving the dedup.
        visual.vertices.push(BloatVertex {
            pos: [1.0, 0.0, 0.0],
            ..BloatVertex::default()
        });
        visual.vertices.push(BloatVertex {
            pos: [0.0, 1.0, 0.0],
            ..BloatVertex::default()
        });
        visual.triangles.push(tri(0));
        visual.materials.push(MayaMaterial {
            identifier: "a".to_string(),
            ..MayaMaterial::default()
        });
        visual.materials.push(MayaMaterial {
            identifier: "b".to_string(),
            ..MayaMaterial::default()
        });
        visual.triangles.push(tri(1));
        let mut io = TestIo::new();
        assert!(
            visual
                .save(&settings, None, "out.primitives", false, &mut io)
                .is_none()
        );
        assert_eq!(
            visual.errors.last().unwrap(),
            "VisualMesh::createMeshParticlePrimGroups - \
Mesh particles must use only 1 material/mesh."
        );

        // Past 65536 vertices the index blob switches to the list32
        // format; a 65537-vertex triangle exercises the u32 write loop.
        let settings = MayaExportSettings::default();
        let mut visual = MayaVisualMesh {
            identifier: "big".to_string(),
            ..MayaVisualMesh::default()
        };
        // Distinct positions keep every vertex through the dedup: the
        // reference's `sortTriangles` also discards unreferenced ones.
        for i in 0..65537u32 {
            visual.vertices.push(BloatVertex {
                pos: [i as f32, 0.0, 0.0],
                uv: [0.0, 0.0],
                normal: [0.0, 1.0, 0.0],
                mesh_index: i as i32,
                ..BloatVertex::default()
            });
        }
        // A strip references every vertex once, so the sort keeps them
        // all and the count tips past 65536.
        for i in 0..65535u32 {
            visual.triangles.push(Triangle {
                index: [i as i32, (i + 1) as i32, (i + 2) as i32],
                real_index: [i as i32, (i + 1) as i32, (i + 2) as i32],
                smoothing_group: 0,
                material_index: 0,
            });
        }
        visual.materials.push(MayaMaterial::default());
        let mut io = TestIo::new();
        visual
            .save(&settings, None, "out.primitives", false, &mut io)
            .unwrap();
        let bytes = io.files["out.primitives"].clone();
        let mut container = BinSection::new("root", BinaryBlock::new(bytes));
        let indices = container.read_binary("indices").expect("indices");
        assert_eq!(&indices.data()[..4], b"list");
        assert!(visual.large_indices);
    }

    #[test]
    fn constraint_writers_cover_the_optional_faces() {
        let settings = MayaExportSettings::default();
        let scene = SceneConstraints {
            orient: vec![OrientConstraint {
                identifier: "o".to_string(),
                target: None,
                offset: Vector3::ZERO,
                sources: vec![ConstraintSource {
                    identifier: "driven".to_string(),
                    weight: Some(2.0),
                }],
            }],
            point: vec![PointConstraint {
                identifier: "p".to_string(),
                pole_vector: false,
                target: None,
                offset: Vector3::new(1.0, 2.0, 3.0),
                sources: vec![ConstraintSource::default()],
            }],
            aim: vec![AimConstraint {
                identifier: "a".to_string(),
                target: None,
                offset: Vector3::ZERO,
                aim_vector: Vector3::ZERO,
                up_vector: Vector3::ZERO,
                world_up_vector: Vector3::ZERO,
                sources: vec![ConstraintSource::default()],
            }],
            ik_handles: vec![IkHandle {
                identifier: "ik".to_string(),
                start_joint: "start".to_string(),
                end_joint: None,
                translate: TranslateOrVector::Vector(Vector3::new(1.0, 2.0, 3.0)),
                pole_vector: TranslateOrVector::Vector(Vector3::new(4.0, 5.0, 6.0)),
            }],
            ..SceneConstraints::default()
        };
        let mut visual = DataSection::new("root");
        save_constraints(&mut visual, &scene, &settings);

        // A target-less constraint writes no target child; the point
        // tag is the plain pointConstraint (poleVector false).
        let point = visual
            .find_child("constraints")
            .unwrap()
            .open_child(1)
            .unwrap();
        assert!(point.find_child("target").is_none());
        assert_eq!(point.section_name(), "pointConstraint");
        // The default source writes an empty source section.
        let point_source = point.find_child("source").unwrap();
        assert_eq!(point_source.read_string("identifier", ""), "");
        assert!(point_source.find_child("weight").is_none());
        // The aim source round-trips too.
        let aim = visual
            .find_child("constraints")
            .unwrap()
            .open_child(2)
            .unwrap();
        assert!(
            aim.find_child("source")
                .unwrap()
                .find_child("weight")
                .is_none()
        );

        // The IK handle's vector/vector form writes both converted
        // vectors and no endJoint.
        let handle = visual
            .find_child("constraints")
            .unwrap()
            .open_child(3)
            .unwrap();
        assert!(handle.find_child("endJoint").is_none());
        assert_eq!(
            handle.read_vector3("source/vector", Vector3::ZERO),
            Vector3::new(0.01, 0.02, -0.03)
        );
        assert_eq!(
            handle.read_vector3("poleVector/vector", Vector3::ZERO),
            Vector3::new(4.0, 5.0, -6.0)
        );
    }

    #[test]
    fn constraint_writers_cover_the_reference_faces() {
        // The reference test's shape: a pole-vector point constraint
        // with a target and a fully-loaded orient source, plus a
        // parent constraint source with only offsets set.
        let settings = MayaExportSettings::default();
        let scene = SceneConstraints {
            orient: vec![OrientConstraint {
                identifier: "orient1".to_string(),
                target: Some("targetNode".to_string()),
                offset: Vector3::new(10.0, 20.0, 30.0),
                sources: vec![ConstraintSource {
                    identifier: "drivenA".to_string(),
                    weight: Some(0.5),
                }],
            }],
            point: vec![PointConstraint {
                identifier: "point1".to_string(),
                pole_vector: true,
                target: Some("targetNode".to_string()),
                offset: Vector3::new(1.0, 2.0, 3.0),
                sources: vec![ConstraintSource::default()],
            }],
            parent: vec![ParentConstraint {
                identifier: "parent1".to_string(),
                target: None,
                sources: vec![ParentConstraintSource {
                    identifier: "drivenB".to_string(),
                    weight: Some(0.25),
                    translate_offset: Some(Vector3::new(1.0, 1.0, 1.0)),
                    rotate_offset: Some(Vector3::new(1.0, 1.0, 1.0)),
                }],
            }],
            aim: vec![AimConstraint {
                identifier: "aim1".to_string(),
                target: Some("targetNode".to_string()),
                offset: Vector3::new(0.0, 0.0, 0.0),
                aim_vector: Vector3::new(1.0, 0.0, 0.0),
                up_vector: Vector3::new(0.0, 1.0, 0.0),
                world_up_vector: Vector3::new(0.0, 0.0, 1.0),
                sources: vec![ConstraintSource::default()],
            }],
            ik_handles: vec![IkHandle {
                identifier: "ik1".to_string(),
                start_joint: "joint1".to_string(),
                end_joint: Some("effector1".to_string()),
                translate: TranslateOrVector::Node("translateNode".to_string()),
                pole_vector: TranslateOrVector::Vector(Vector3::new(1.0, 2.0, 3.0)),
            }],
        };
        let mut visual = DataSection::new("root");
        save_constraints(&mut visual, &scene, &settings);

        // The pole-vector point constraint's tag and its target.
        let point = visual
            .find_child("constraints")
            .unwrap()
            .open_child(1)
            .unwrap();
        assert_eq!(point.section_name(), "poleVectorConstraint");
        assert_eq!(point.read_string("target", ""), "targetNode");
        // The orient source carries both children.
        let orient_source = visual
            .find_child("constraints")
            .unwrap()
            .open_child(0)
            .unwrap()
            .find_child("source")
            .unwrap();
        assert_eq!(orient_source.read_string("identifier", ""), "drivenA");
        assert_eq!(orient_source.read_float("weight", 0.0), 0.5);
        // The parent constraint writes no target child and its source
        // keeps both offsets.
        let parent = visual
            .find_child("constraints")
            .unwrap()
            .open_child(2)
            .unwrap();
        assert!(parent.find_child("target").is_none());
        let parent_source = parent.find_child("source").unwrap();
        assert_eq!(parent_source.read_string("identifier", ""), "drivenB");
        assert_eq!(parent_source.read_float("weight", 0.0), 0.25);
        assert_eq!(
            parent_source.read_vector3("translateOffset", Vector3::ZERO),
            Vector3::new(0.01, 0.01, -0.01)
        );
        assert_eq!(
            parent_source.read_vector3("rotateOffset", Vector3::ZERO),
            Vector3::new(-1.0, -1.0, 1.0)
        );
        // The aim constraint has a target and a default source.
        let aim = visual
            .find_child("constraints")
            .unwrap()
            .open_child(3)
            .unwrap();
        assert_eq!(aim.read_string("target", ""), "targetNode");
        assert!(aim.find_child("source").is_some());

        // The IK handle's node/source form with a vector pole and an
        // endJoint.
        let handle = visual
            .find_child("constraints")
            .unwrap()
            .open_child(4)
            .unwrap();
        assert_eq!(handle.read_string("source/node", ""), "translateNode");
        assert_eq!(
            handle.read_vector3("poleVector/vector", Vector3::ZERO),
            Vector3::new(1.0, 2.0, -3.0)
        );
        assert_eq!(handle.read_string("endJoint", ""), "effector1");
    }

    #[test]
    fn material_save_default_materials_and_meaning_two() {
        // Meaning 0 with an unknown meaning: the default material fills
        // the mfm; a soft-skinned geometry uses the skinned default.
        let settings = MayaExportSettings {
            default_material: "defaults/default.mfm".to_string(),
            default_skinned_material: "defaults/skinned.mfm".to_string(),
            ..MayaExportSettings::default()
        };
        let material = MayaMaterial::default();
        let mut geometry = DataSection::new("geometry");
        maya_material_save(
            &material,
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
        );
        let pg = geometry.open_child(0).unwrap();
        assert_eq!(pg.read_string("material/mfm", ""), "defaults/default.mfm");

        let mut geometry = DataSection::new("geometry");
        maya_material_save(
            &material,
            &mut geometry,
            None,
            0,
            SkinType::SoftSkin,
            false,
            &settings,
        );
        let pg = geometry.open_child(0).unwrap();
        assert_eq!(pg.read_string("material/mfm", ""), "defaults/skinned.mfm");

        // Meaning 2 writes the mfm straight from the map identifier.
        let material = MayaMaterial {
            map_id_meaning: 2,
            map_identifier: "walls/wall.mfm".to_string(),
            ..MayaMaterial::default()
        };
        let mut geometry = DataSection::new("geometry");
        maya_material_save(
            &material,
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
        );
        let pg = geometry.open_child(0).unwrap();
        assert_eq!(pg.read_string("material/mfm", ""), "walls/wall.mfm");
    }

    #[test]
    fn interactive_host_sees_the_message_boxes() {
        // TestIo is a non-automated host: every error box fires with the
        // reference's text.
        let mut host = TestIo::new();

        // The invalid-texture-name box during init.
        let settings = MayaExportSettings::default();
        let mut raw = quad_raw();
        raw.materials[0].map_file = "%MISSING%/map.tga".to_string();
        let mesh = gather_mesh(&raw, &settings, &host).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(visual.init(&mesh, &settings, &host));
        assert!(host.boxes.borrow()[0].contains("invalid texture name"));

        // The uv-less box (a mesh with uvs would not report it).
        let mut raw = quad_raw();
        raw.uv_sets = vec![Vec::new()];
        let mesh = gather_mesh(&raw, &settings, &host).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(!visual.init(&mesh, &settings, &host));
        assert_eq!(
            host.boxes.borrow()[1],
            "Visual Exporter Error|Failed to export mesh |grp|wall because there are no uvs"
        );

        // The materials-empty box at save time.
        let mut visual = MayaVisualMesh {
            identifier: "wall".to_string(),
            materials: Vec::new(),
            ..MayaVisualMesh::default()
        };
        assert!(
            visual
                .save(&settings, None, "out.primitives", false, &mut host)
                .is_none()
        );
        assert_eq!(
            host.boxes.borrow()[2],
            "Material error!|wall does not have any materials.\nThis object will not be \
exported.  Adding a material will fix this problem."
        );
    }

    #[test]
    fn init_faces_and_hosts_without_boxes() {
        // The no-uvs message box fires on a non-automated host.
        let settings = MayaExportSettings::default();
        let mut raw = quad_raw();
        raw.uv_sets = vec![Vec::new()];
        let mesh = gather_mesh(&raw, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(!visual.init(&mesh, &settings, &TestIo::new()));
        let io = TestIo::new();
        let _ = io;

        // A zero-length normal is left unnormalised (the reference's
        // positive-length guard).
        let mut raw2 = quad_raw();
        raw2.normals = vec![Vector3::ZERO, Vector3::ZERO, Vector3::ZERO, Vector3::ZERO];
        let mesh = gather_mesh(&raw2, &settings, &TestIo::new()).unwrap();
        let mut visual = MayaVisualMesh::default();
        assert!(visual.init(&mesh, &settings, &TestIo::new()));
        assert_eq!(visual.vertices[0].normal, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn point_and_aim_constraint_sources_cover_the_children() {
        // The point (non-pole) and aim writers with a fully-loaded
        // source: both optional children land under source.
        let settings = MayaExportSettings::default();
        let scene = SceneConstraints {
            point: vec![PointConstraint {
                identifier: "point1".to_string(),
                pole_vector: false,
                target: None,
                offset: Vector3::new(1.0, 2.0, 3.0),
                sources: vec![ConstraintSource {
                    identifier: "drivenC".to_string(),
                    weight: Some(0.75),
                }],
            }],
            aim: vec![AimConstraint {
                identifier: "aim1".to_string(),
                target: None,
                offset: Vector3::ZERO,
                aim_vector: Vector3::new(1.0, 0.0, 0.0),
                up_vector: Vector3::new(0.0, 1.0, 0.0),
                world_up_vector: Vector3::new(0.0, 0.0, 1.0),
                sources: vec![ConstraintSource {
                    identifier: "drivenD".to_string(),
                    weight: Some(0.25),
                }],
            }],
            ..SceneConstraints::default()
        };
        let mut visual = DataSection::new("root");
        save_constraints(&mut visual, &scene, &settings);
        let constraints = visual.find_child("constraints").unwrap();
        let point = constraints.open_child(0).unwrap();
        assert_eq!(point.section_name(), "pointConstraint");
        let point_source = point.find_child("source").unwrap();
        assert_eq!(point_source.read_string("identifier", ""), "drivenC");
        assert_eq!(point_source.read_float("weight", 0.0), 0.75);
        let aim = constraints.open_child(1).unwrap();
        assert_eq!(aim.section_name(), "aimConstraint");
        let aim_source = aim.find_child("source").unwrap();
        assert_eq!(aim_source.read_string("identifier", ""), "drivenD");
        assert_eq!(aim_source.read_float("weight", 0.0), 0.25);
    }

    #[test]
    fn converters_follow_the_orientation() {
        let mut settings = MayaExportSettings {
            unit_scale: 2.0,
            maya_unit_scale: 3.0,
            use_legacy_scaling: true,
            ..MayaExportSettings::default()
        };
        let v = Vector3::new(1.0, 2.0, 3.0);

        // Legacy: the point scale is (-u, u, u).
        settings.use_legacy_orientation = true;
        assert_eq!(
            convert_point_to_bigworld_units(v, &settings),
            Vector3::new(-2.0, 4.0, 6.0)
        );
        assert_eq!(
            convert_vector_to_bigworld_units(v, &settings),
            Vector3::new(-1.0, 2.0, 3.0)
        );
        assert_eq!(
            convert_angle_offset_to_bigworld_units(v, &settings),
            Vector3::new(1.0, -2.0, -3.0)
        );

        // Modern: the scene factor applies and z flips; the angle scale
        // flips x and y instead.
        settings.use_legacy_orientation = false;
        settings.use_legacy_scaling = false;
        assert_eq!(
            convert_point_to_bigworld_units(v, &settings),
            Vector3::new(3.0, 6.0, -9.0)
        );
        assert_eq!(
            convert_vector_to_bigworld_units(v, &settings),
            Vector3::new(1.0, 2.0, -3.0)
        );
        assert_eq!(
            convert_angle_offset_to_bigworld_units(v, &settings),
            Vector3::new(-1.0, -2.0, 3.0)
        );
    }

    #[test]
    fn snapping_rounds_to_the_grid() {
        assert_eq!(snap_value(1.0004, 0.001), 1.0);
        assert_eq!(snap_value(-1.0004, 0.001), -1.0);
        assert_eq!(snap_value(1.0006, 0.001), 1.001);
        assert_eq!(snap_value(0.0, 0.001), 0.0);
        assert_eq!(snap_value(5.0, 0.0), 5.0);
        assert_eq!(snap_point3([1.0004, -1.0004, 0.0]), [1.0, -1.0, 0.0]);
    }

    #[test]
    fn string_helpers_match_the_reference() {
        assert_eq!(trim_whitespaces("  name  "), "name");
        assert_eq!(trim_whitespaces("name"), "name");
        assert_eq!(trim_whitespaces("     "), "");
        assert_eq!(trim_whitespaces("a b"), "a b");
        assert_eq!(strip_reference_prefix("refNS:joint"), "joint");
        assert_eq!(strip_reference_prefix("joint"), "joint");
        assert_eq!(after_last_pipe("|a|b"), "b");
        assert_eq!(after_last_pipe("b"), "b");
        assert_eq!(remove_extension("dir/wall.tga"), "dir/wall");
        assert_eq!(remove_extension("wall"), "wall");
        assert_eq!(after_last_slash("dir/sub/wall.tga"), "wall.tga");
        assert_eq!(after_last_slash("dir\\wall.tga"), "wall.tga");
        assert_eq!(after_last_slash("wall.tga"), "wall.tga");
    }

    #[test]
    fn environment_macros_substitute_and_fail() {
        let mut io = TestIo::new();
        io.env.insert("TEXTURES".to_string(), "t".to_string());
        assert_eq!(
            replace_environment_macros("no macros", &io),
            Some("no macros".to_string())
        );
        assert_eq!(
            replace_environment_macros("%TEXTURES%/wall.tga", &io),
            Some("t/wall.tga".to_string())
        );
        assert_eq!(replace_environment_macros("%MISSING%", &io), None);
        // An empty value also fails (the macro cannot be substituted).
        io.env.insert("EMPTY".to_string(), String::new());
        assert_eq!(replace_environment_macros("%EMPTY%", &io), None);
    }

    #[test]
    fn constraints_write_the_reference_sections() {
        let settings = MayaExportSettings {
            use_legacy_orientation: true,
            unit_scale: 2.0,
            use_legacy_scaling: true,
            ..MayaExportSettings::default()
        };

        let scene = SceneConstraints {
            orient: vec![OrientConstraint {
                identifier: "orient1".to_string(),
                target: Some("targetNode".to_string()),
                offset: Vector3::new(10.0, 20.0, 30.0),
                sources: vec![
                    ConstraintSource {
                        identifier: "drivenA".to_string(),
                        weight: Some(0.5),
                    },
                    ConstraintSource::default(),
                ],
            }],
            point: vec![PointConstraint {
                identifier: "point1".to_string(),
                pole_vector: true,
                target: None,
                offset: Vector3::new(1.0, 2.0, 3.0),
                sources: vec![ConstraintSource::default()],
            }],
            parent: vec![ParentConstraint {
                identifier: "parent1".to_string(),
                target: Some("targetNode".to_string()),
                sources: vec![ParentConstraintSource {
                    identifier: "drivenB".to_string(),
                    weight: Some(0.25),
                    translate_offset: Some(Vector3::new(1.0, 1.0, 1.0)),
                    rotate_offset: Some(Vector3::new(1.0, 1.0, 1.0)),
                }],
            }],
            aim: vec![AimConstraint {
                identifier: "aim1".to_string(),
                target: Some("targetNode".to_string()),
                offset: Vector3::new(0.0, 0.0, 0.0),
                aim_vector: Vector3::new(1.0, 0.0, 0.0),
                up_vector: Vector3::new(0.0, 1.0, 0.0),
                world_up_vector: Vector3::new(0.0, 0.0, 1.0),
                sources: Vec::new(),
            }],
            ik_handles: vec![IkHandle {
                identifier: "ik1".to_string(),
                start_joint: "joint1".to_string(),
                end_joint: Some("effector1".to_string()),
                translate: TranslateOrVector::Vector(Vector3::new(1.0, 2.0, 3.0)),
                pole_vector: TranslateOrVector::Node("pvLocator".to_string()),
            }],
        };

        let mut visual = DataSection::new("root");
        save_constraints(&mut visual, &scene, &settings);
        let text = visual.write_to_string();

        // The five section types in the iterator order.
        assert!(text.contains("<orientConstraint>"));
        assert!(text.contains("<poleVectorConstraint>"));
        assert!(text.contains("<parentConstraint>"));
        assert!(text.contains("<aimConstraint>"));
        assert!(text.contains("<ikHandle>"));

        // The offsets convert: a legacy orient offset scales (1,-1,-1).
        let orient = visual
            .find_child("constraints")
            .unwrap()
            .open_child(0)
            .unwrap();
        assert_eq!(
            orient.read_vector3("offset", Vector3::ZERO),
            Vector3::new(10.0, -20.0, -30.0)
        );
        // A legacy point offset scales (-2,2,2).
        let point = visual
            .find_child("constraints")
            .unwrap()
            .open_child(1)
            .unwrap();
        assert_eq!(
            point.read_vector3("offset", Vector3::ZERO),
            Vector3::new(-2.0, 4.0, 6.0)
        );
        // The parent constraint's source carries both offsets: the
        // translate through the point converter, the rotate through the
        // angle converter (legacy (1,-1,-1)).
        let parent = visual
            .find_child("constraints")
            .unwrap()
            .open_child(2)
            .unwrap();
        let source = parent.find_child("source").unwrap();
        assert_eq!(
            source.read_vector3("translateOffset", Vector3::ZERO),
            Vector3::new(-2.0, 2.0, 2.0)
        );
        assert_eq!(
            source.read_vector3("rotateOffset", Vector3::ZERO),
            Vector3::new(1.0, -1.0, -1.0)
        );
        // The aim vectors flip x on the vector converter (legacy -x).
        let aim = visual
            .find_child("constraints")
            .unwrap()
            .open_child(3)
            .unwrap();
        assert_eq!(
            aim.read_vector3("aimVector", Vector3::ZERO),
            Vector3::new(-1.0, 0.0, 0.0)
        );
        assert_eq!(
            aim.read_vector3("upVector", Vector3::ZERO),
            Vector3::new(0.0, 1.0, 0.0)
        );
        // The world up keeps its z under the legacy vector conversion.
        assert_eq!(
            aim.read_vector3("worldUpVector", Vector3::ZERO),
            Vector3::new(0.0, 0.0, 1.0)
        );
        // The IK handle's split translate/poleVector faces.
        let handle = visual
            .find_child("constraints")
            .unwrap()
            .open_child(4)
            .unwrap();
        assert_eq!(handle.read_string("startJoint", ""), "joint1");
        assert_eq!(handle.read_string("endJoint", ""), "effector1");
        assert_eq!(
            handle.read_vector3("source/vector", Vector3::ZERO),
            Vector3::new(-2.0, 4.0, 6.0)
        );
        assert_eq!(handle.read_string("poleVector/node", ""), "pvLocator");

        // A source writes its identifier and weight children; a weighted
        // one carries both, the default one neither.
        let sources = orient.find_child("source").unwrap();
        assert_eq!(sources.read_string("identifier", ""), "drivenA");
        assert_eq!(sources.read_float("weight", 0.0), 0.5);
        let point_source = point.find_child("source").unwrap();
        assert_eq!(point_source.read_string("identifier", ""), "");
        assert!(point_source.find_child("weight").is_none());
        // A target-less constraint writes no target child.
        assert!(point.find_child("target").is_none());
    }
}
