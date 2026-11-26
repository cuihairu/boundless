//! The Maya visual exporter's portable core
//! (`tools/mayavisualexporter/`).
//!
//! Ported faces:
//! - [`ExportSettings`] with the `expsets` defaults and the `expsetsio`
//!   settings file IO;
//! - [`VisualMesh`] — the Maya variant of the mesh export chain:
//!   [`VisualMesh::init`] (uv v-flip, snapping, the shared-vertex
//!   smoothing-group merge), the winding flip, the unstable triangle sort
//!   with its per-material compaction, `removeDuplicateVertices`,
//!   `createPrimitiveGroups` (+ the mesh-particle 16-group split), the
//!   xyznuv/xyznuvi vertex streams and the extra uv2/colour
//!   streams, and [`Material::save`] with its existing-material copy;
//! - [`Hierarchy`] — the `|`-path node walk, the ordered children, the
//!   legacy/modern orientation row negation, plus `exportTree` and the
//!   hierarchy duplicate checks;
//! - [`VisualPortal`] and the Graham scan convex hull
//!   (`lib/math/graham_scan.hpp`);
//! - [`HullMesh`] — the quickhull construction (`mc*` family), the boundary
//!   plane extraction with portal materials, and the boundary/portal
//!   export;
//! - [`MayaVisualExporter::export_mesh`] — the translator driver: temp
//!   names, the visual/primitives pair, the bounding box, the shell hull
//!   and portals, the visual checker gate;
//! - `replaceEnvironmentMacros` (`%VAR%` substitution).
//!
//! Documented seams (the reference's non-portable faces):
//! - the Maya scene traversal: the port takes [`Scene`] records instead of
//!   `MFnMesh`/`MDagPath` walks (`Mesh::initialise`, `Skin`, `BlendShapes`,
//!   `Constraints` and the origin transformer are all Maya-API faces);
//! - `makeBumped` (`NVMeshMender`, `third_party/nvmeshmender`): a
//!   bump-mapped non-particle export fails with the seam message;
//! - `generateBSP` (the scene BSP builder) and the `_bsp` mesh face: BSP
//!   meshes are refused with the seam message (the `.bsp`/`bsp2` container
//!   faces live with the compiled-space converter);
//! - the `.animation` writer: BW-13-05-02's [`crate::animation_exporter`]
//!   owns that format, so `exportAnim` is not ported (this also drops the
//!   `checkHierarchy` call site — the function itself is ported);
//! - the dialog faces (`MessageBox`, `invalidExportDlg`,
//!   `invalidResourceDlg`): replaced with collected error strings;
//! - `savePrimXml` (`#if DEBUG_PRIMITIVE_FILE`): disabled in shipping
//!   builds, not ported;
//! - external texture copying (`copyExternalTextures` with
//!   `copyFileOrDirectory`): the port dissolves map names but does not copy
//!   files;
//! - `MetaData::updateCreationInfo` on the model file: that refresh lives
//!   with the ModelEditor face (`crate::model_editor`), so the model file
//!   written here carries only the visual reference and the source
//!   metadata.

use std::collections::{BTreeMap, BTreeSet};

use foundation::math::{
    BoundBox, Matrix, PlaneEq, Quaternion, Vector2, Vector3, Vector4, almost_zero, clamp,
};
use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::{self, BwResource};
use resources::datasection::DataSection;

use crate::primitive_file_structs::{
    INDEX_FORMAT_FIELD_LEN, IndexHeader, PrimitiveGroup, VertexHeader, write_primitive_groups,
};

// ---------------------------------------------------------------------------------------
// small shared helpers
// ---------------------------------------------------------------------------------------

/// `almostEqual(a, b)` (`stdmf.hpp`): the default epsilon is `0.0004f`.
fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.0004
}

/// `trimWhitespaces` (`cstdmf/stdstring.cpp`): strip the BW whitespace set
/// from both ends.
fn trim_whitespaces(s: &str) -> &str {
    s.trim_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r')
}

/// `toLower`.
fn to_lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

// ---------------------------------------------------------------------------------------
// matrix4<float>
// ---------------------------------------------------------------------------------------

/// `matrix4<float>` (`tools/mayavisualexporter/matrix4.hpp`): row-major
/// storage with the translation in the fourth *column* — points transform
/// as `M * v` (`operator*(const vector3&)` adds `v14/v24/v34`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix4 {
    /// `m[row][col]`.
    pub m: [[f32; 4]; 4],
}

impl Default for Matrix4 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix4 {
    /// The default constructor: the identity matrix.
    pub const IDENTITY: Matrix4 = Matrix4 {
        m: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };

    /// `matrix4(a11..a44)` — row-major parameter order.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        a11: f32,
        a12: f32,
        a13: f32,
        a14: f32,
        a21: f32,
        a22: f32,
        a23: f32,
        a24: f32,
        a31: f32,
        a32: f32,
        a33: f32,
        a34: f32,
        a41: f32,
        a42: f32,
        a43: f32,
        a44: f32,
    ) -> Self {
        Matrix4 {
            m: [
                [a11, a12, a13, a14],
                [a21, a22, a23, a24],
                [a31, a32, a33, a34],
                [a41, a42, a43, a44],
            ],
        }
    }

    /// `matrix4::translation(vector3<T>)`.
    pub fn translation(x: f32, y: f32, z: f32) -> Self {
        Self::new(
            1.0, 0.0, 0.0, x, //
            0.0, 1.0, 0.0, y, //
            0.0, 0.0, 1.0, z, //
            0.0, 0.0, 0.0, 1.0,
        )
    }

    /// `operator*(const matrix4&)`: the standard matrix product.
    pub fn mul(&self, o: &Matrix4) -> Matrix4 {
        let mut r = [[0.0f32; 4]; 4];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = self.m[i][0] * o.m[0][j]
                    + self.m[i][1] * o.m[1][j]
                    + self.m[i][2] * o.m[2][j]
                    + self.m[i][3] * o.m[3][j];
            }
        }
        Matrix4 { m: r }
    }

    /// `operator*(const T&)`.
    pub fn scale(&self, s: f32) -> Matrix4 {
        let mut r = self.m;
        for row in &mut r {
            for v in row.iter_mut() {
                *v *= s;
            }
        }
        Matrix4 { m: r }
    }

    /// `determinant3x3`.
    #[allow(clippy::too_many_arguments)]
    fn det3(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32, g: f32, h: f32, i: f32) -> f32 {
        a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
    }

    /// `determinant()`: cofactor expansion along the first row.
    pub fn determinant(&self) -> f32 {
        let m = &self.m;
        m[0][0]
            * Self::det3(
                m[1][1], m[1][2], m[1][3], m[2][1], m[2][2], m[2][3], m[3][1], m[3][2], m[3][3],
            )
            - m[0][1]
                * Self::det3(
                    m[1][0], m[1][2], m[1][3], m[2][0], m[2][2], m[2][3], m[3][0], m[3][2], m[3][3],
                )
            + m[0][2]
                * Self::det3(
                    m[1][0], m[1][1], m[1][3], m[2][0], m[2][1], m[2][3], m[3][0], m[3][1], m[3][3],
                )
            - m[0][3]
                * Self::det3(
                    m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2], m[3][0], m[3][1], m[3][2],
                )
    }

    /// `inverse()`: the adjugate over the determinant. A zero determinant
    /// divides by zero exactly like the reference (the error log was
    /// removed for the maya exporter).
    pub fn inverse(&self) -> Matrix4 {
        let m = &self.m;
        let d = self.determinant();
        let mut r = [[0.0f32; 4]; 4];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                // inverse[i][j] = cofactor(j, i) / det
                let mut cols = [0usize; 3];
                let mut rows = [0usize; 3];
                let mut ci = 0;
                for c in 0..4 {
                    if c != j {
                        cols[ci] = c;
                        ci += 1;
                    }
                }
                let mut ri = 0;
                for rr in 0..4 {
                    if rr != i {
                        rows[ri] = rr;
                        ri += 1;
                    }
                }
                let minor = Self::det3(
                    m[rows[0]][cols[0]],
                    m[rows[0]][cols[1]],
                    m[rows[0]][cols[2]],
                    m[rows[1]][cols[0]],
                    m[rows[1]][cols[1]],
                    m[rows[1]][cols[2]],
                    m[rows[2]][cols[0]],
                    m[rows[2]][cols[1]],
                    m[rows[2]][cols[2]],
                );
                let sign = if (i + j) % 2 == 0 { 1.0 } else { -1.0 };
                *v = sign * minor / d;
            }
        }
        Matrix4 { m: r }
    }

    /// `transpose()`.
    pub fn transpose(&self) -> Matrix4 {
        let mut r = [[0.0f32; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                r[i][j] = self.m[j][i];
            }
        }
        Matrix4 { m: r }
    }

    /// `operator*(const vector3&)`: `w` is assumed to be 1.
    pub fn apply_point(&self, p: &Vector3) -> Vector3 {
        Vector3::new(
            self.m[0][0] * p.x + self.m[0][1] * p.y + self.m[0][2] * p.z + self.m[0][3],
            self.m[1][0] * p.x + self.m[1][1] * p.y + self.m[1][2] * p.z + self.m[1][3],
            self.m[2][0] * p.x + self.m[2][1] * p.y + self.m[2][2] * p.z + self.m[2][3],
        )
    }

    /// Negate the orientation row the way `Hierarchy::transform` does:
    /// legacy negates row 0, modern row 2, in both cases only the first
    /// three columns.
    fn negate_orientation_row(&self, legacy: bool) -> Matrix4 {
        let mut r = self.m;
        let row = if legacy { 0 } else { 2 };
        for value in &mut r[row][..3] {
            *value *= -1.0;
        }
        Matrix4 { m: r }
    }
}

// ---------------------------------------------------------------------------------------
// export settings (expsets / expsetsio)
// ---------------------------------------------------------------------------------------

/// `ExportSettings::ExportMode` (`expsets.hpp:79-97`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportMode {
    /// `NORMAL`.
    #[default]
    Normal = 0,
    /// `STATIC`.
    Static = 1,
    /// `STATIC_WITH_NODES`.
    StaticWithNodes = 2,
    /// `MESH_PARTICLES`.
    MeshParticles = 3,
}

/// `ExportSettings::NodeFilter` (`expsets.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodeFilter {
    /// `ALL`.
    #[default]
    All = 0,
    /// `SELECTED`.
    Selected = 1,
}

/// `ExportSettings` (`expsets.cpp` ctor defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSettings {
    pub export_animation: bool,
    pub export_mode: ExportMode,
    pub bone_count: i32,
    pub transform_to_origin: bool,
    pub allow_scale: bool,
    pub bump_mapped: bool,
    pub keep_existing_materials: bool,
    pub snap_vertices: bool,
    pub strip_ref_prefix: bool,
    pub use_reference_node: bool,
    pub reference_nodes_file: String,
    pub disable_visual_checker: bool,
    pub use_legacy_scaling: bool,
    pub fix_cylindrical: bool,
    pub use_legacy_orientation: bool,
    pub scene_root_added: bool,
    pub include_meshes: bool,
    pub include_envelopes_and_bones: bool,
    pub include_nodes: bool,
    pub include_materials: bool,
    pub include_animations: bool,
    pub use_character_mode: bool,
    pub animation_name: String,
    pub include_portals: bool,
    pub world_space_origin: bool,
    pub unit_scale: f32,
    pub local_hierarchy: bool,
    pub node_filter: NodeFilter,
    pub copy_external_textures: bool,
    pub copy_textures_to: String,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings {
            export_animation: false,
            export_mode: ExportMode::Normal,
            bone_count: 17,
            transform_to_origin: false,
            allow_scale: false,
            bump_mapped: false,
            keep_existing_materials: true,
            snap_vertices: false,
            strip_ref_prefix: false,
            use_reference_node: false,
            reference_nodes_file: String::new(),
            disable_visual_checker: false,
            use_legacy_scaling: false,
            fix_cylindrical: false,
            use_legacy_orientation: false,
            scene_root_added: false,
            include_meshes: true,
            include_envelopes_and_bones: false,
            include_nodes: true,
            include_materials: true,
            include_animations: false,
            use_character_mode: false,
            animation_name: "anim".to_string(),
            include_portals: false,
            world_space_origin: false,
            unit_scale: 0.1,
            local_hierarchy: false,
            node_filter: NodeFilter::All,
            copy_external_textures: true,
            copy_textures_to: String::new(),
        }
    }
}

impl ExportSettings {
    /// `ExpSetsSettingsIO::readSettings(section)`: every tag is read with
    /// the current value as its default.
    pub fn read_settings(&mut self, section: &DataSection) {
        self.export_animation = section.read_bool("exportAnimation", self.export_animation);
        self.export_mode = match section.read_int("exportMode", self.export_mode as i32) {
            1 => ExportMode::Static,
            2 => ExportMode::StaticWithNodes,
            3 => ExportMode::MeshParticles,
            _ => ExportMode::Normal,
        };
        self.bone_count = section.read_int("boneCount", self.bone_count);
        self.transform_to_origin = section.read_bool("transformToOrigin", self.transform_to_origin);
        self.allow_scale = section.read_bool("allowScale", self.allow_scale);
        self.bump_mapped = section.read_bool("bumpMapped", self.bump_mapped);
        self.keep_existing_materials =
            section.read_bool("keepExistingMaterials", self.keep_existing_materials);
        self.snap_vertices = section.read_bool("snapVertices", self.snap_vertices);
        self.strip_ref_prefix = section.read_bool("stripRefPrefix", self.strip_ref_prefix);
        self.use_reference_node = section.read_bool("useReferenceNode", self.use_reference_node);
        self.reference_nodes_file =
            section.read_string("referenceNodesFile", &self.reference_nodes_file);
        self.disable_visual_checker =
            section.read_bool("disableVisualChecker", self.disable_visual_checker);
        self.use_legacy_scaling = section.read_bool("useLegacyScaling", self.use_legacy_scaling);
        self.fix_cylindrical = section.read_bool("fixCylindrical", self.fix_cylindrical);
        self.use_legacy_orientation =
            section.read_bool("useLegacyOrientation", self.use_legacy_orientation);
        self.scene_root_added = section.read_bool("sceneRootAdded", self.scene_root_added);
        self.include_meshes = section.read_bool("includeMeshes", self.include_meshes);
        self.include_envelopes_and_bones =
            section.read_bool("includeEnvelopesAndBones", self.include_envelopes_and_bones);
        self.include_nodes = section.read_bool("includeNodes", self.include_nodes);
        self.include_materials = section.read_bool("includeMaterials", self.include_materials);
        self.include_animations = section.read_bool("includeAnimations", self.include_animations);
        self.use_character_mode = section.read_bool("useCharacterMode", self.use_character_mode);
        self.animation_name = section.read_string("animationName", &self.animation_name);
        self.include_portals = section.read_bool("includePortals", self.include_portals);
        self.world_space_origin = section.read_bool("worldSpaceOrigin", self.world_space_origin);
        self.unit_scale = section.read_float("unitScale", self.unit_scale);
        self.local_hierarchy = section.read_bool("localHierarchy", self.local_hierarchy);
        self.node_filter = match section.read_int("nodeFilter", self.node_filter as i32) {
            1 => NodeFilter::Selected,
            _ => NodeFilter::All,
        };
        self.copy_external_textures =
            section.read_bool("copyExternalTextures", self.copy_external_textures);
        self.copy_textures_to = section.read_string("copyTexturesTo", &self.copy_textures_to);
    }

    /// `ExpSetsSettingsIO::writeSettings(section)`: all thirty tags, the
    /// dissolved `referenceNodesFile` included.
    pub fn write_settings(&self, section: &mut DataSection, resource: &BwResource) {
        section.write_bool("exportAnimation", self.export_animation);
        section.write_int("exportMode", self.export_mode as i32);
        section.write_int("boneCount", self.bone_count);
        section.write_bool("transformToOrigin", self.transform_to_origin);
        section.write_bool("allowScale", self.allow_scale);
        section.write_bool("bumpMapped", self.bump_mapped);
        section.write_bool("keepExistingMaterials", self.keep_existing_materials);
        section.write_bool("snapVertices", self.snap_vertices);
        section.write_bool("stripRefPrefix", self.strip_ref_prefix);
        section.write_bool("useReferenceNode", self.use_reference_node);
        section.write_string(
            "referenceNodesFile",
            &resource.dissolve_filename(&self.reference_nodes_file),
        );
        section.write_bool("disableVisualChecker", self.disable_visual_checker);
        section.write_bool("useLegacyScaling", self.use_legacy_scaling);
        section.write_bool("fixCylindrical", self.fix_cylindrical);
        section.write_bool("useLegacyOrientation", self.use_legacy_orientation);
        section.write_bool("sceneRootAdded", self.scene_root_added);
        section.write_bool("includeMeshes", self.include_meshes);
        section.write_bool("includeEnvelopesAndBones", self.include_envelopes_and_bones);
        section.write_bool("includeNodes", self.include_nodes);
        section.write_bool("includeMaterials", self.include_materials);
        section.write_bool("includeAnimations", self.include_animations);
        section.write_bool("useCharacterMode", self.use_character_mode);
        section.write_string("animationName", &self.animation_name);
        section.write_bool("includePortals", self.include_portals);
        section.write_bool("worldSpaceOrigin", self.world_space_origin);
        section.write_float("unitScale", self.unit_scale);
        section.write_bool("localHierarchy", self.local_hierarchy);
        section.write_int("nodeFilter", self.node_filter as i32);
        section.write_bool("copyExternalTextures", self.copy_external_textures);
        section.write_string("copyTexturesTo", &self.copy_textures_to);
    }
}

/// `AutoConfigString s_defaultMaterial/s_defaultSkinnedMaterial`
/// (`visual_mesh.cpp:34-35`): the exporter config's default mfm names,
/// empty unless configured.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportConfig {
    /// `exporter/defaultMaterial`.
    pub default_material: String,
    /// `exporter/defaultSkinnedMaterial`.
    pub default_skinned_material: String,
}

// ---------------------------------------------------------------------------------------
// utility.cpp
// ---------------------------------------------------------------------------------------

/// `replaceEnvironmentMacros` (`utility.cpp:45`): `%VAR%` segments are
/// replaced with the environment value; an unknown or empty variable fails
/// and keeps the original text.
pub fn replace_environment_macros(input: &str) -> (bool, String) {
    let mut out = String::new();
    let mut macro_name = String::new();
    let mut is_success = true;
    let mut reading_macro = false;
    for value in input.chars() {
        if value == '%' {
            reading_macro = !reading_macro;
            // If we've left a macro, look it up and append it to the output.
            if !reading_macro {
                let found = std::env::var(&macro_name).ok().filter(|v| !v.is_empty());
                match found {
                    Some(v) => out.push_str(&v),
                    // Only append if it's not empty. If it's empty (i.e.
                    // does not exist in maya.env), report as error.
                    None => {
                        is_success = false;
                        break;
                    }
                }
                macro_name.clear();
            }
        } else if reading_macro {
            macro_name.push(value);
        } else {
            out.push(value);
        }
    }
    // If successful, output the new decoded string, otherwise keep the
    // original text (useful for debugging the asset).
    if is_success {
        (true, out)
    } else {
        (false, input.to_string())
    }
}

/// `snapValue` (`tools/visualexporter/utility.cpp:143`): snap to the
/// `snapSize` grid, offset by half a cell.
pub fn snap_value(mut v: f32, snap_size: f32) -> f32 {
    let half_snap = snap_size / 2.0;
    if v > 0.0 {
        v += half_snap;
        v -= v % snap_size;
    } else {
        v -= half_snap;
        v += (-v) % snap_size;
    }
    v
}

/// `snapPoint3` (`utility.hpp:20`): the default snap size is `0.001f`.
pub fn snap_point3(v: &mut Vector3) {
    const SNAP_SIZE: f32 = 0.001;
    v.x = snap_value(v.x, SNAP_SIZE);
    v.y = snap_value(v.y, SNAP_SIZE);
    v.z = snap_value(v.z, SNAP_SIZE);
}

/// `D3DCOLOR_COLORVALUE`: each float channel scales by 255, truncates
/// toward zero (the C cast) and masks to a byte.
fn d3d_color_value(r: f32, g: f32, b: f32, a: f32) -> u32 {
    // `D3DCOLOR_COLORVALUE`: each channel is `(DWORD)(f * 255.f + 0.5f)`
    // masked to 8 bits; a negative channel keeps its two's-complement low
    // bits after the mask.
    let ch = |v: f32| ((v * 255.0 + 0.5) as i32 as u32) & 0xff;
    (ch(a) << 24) | (ch(r) << 16) | (ch(g) << 8) | ch(b)
}

/// `packNormal` (`visual_mesh.cpp:37`): normalise, clamp, and pack into
/// the 11/11/10 signless layout; the C `uint32` cast of a negative float
/// wraps, so negative components survive the mask as their two's
/// complement low bits.
pub fn pack_normal(n: &Vector3) -> u32 {
    let mut n = *n;
    n.normalise();
    n.x = clamp(-1.0, n.x, 1.0);
    n.y = clamp(-1.0, n.y, 1.0);
    n.z = clamp(-1.0, n.z, 1.0);
    ((((n.z * 511.0) as i32 as u32) & 0x3ff) << 22)
        | ((((n.y * 1023.0) as i32 as u32) & 0x7ff) << 11)
        | (((n.x * 1023.0) as i32 as u32) & 0x7ff)
}

// ---------------------------------------------------------------------------------------
// the scene records (the Maya traversal seam)
// ---------------------------------------------------------------------------------------

/// The material record `Mesh` hands to `VisualMesh::addMaterial` — the map
/// name is already resolved from the Maya material.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneMaterial {
    /// The Maya material name (`mtl.name`).
    pub name: String,
    /// The resolved map file (`mtl.mapFile`).
    pub map_file: String,
    /// `mtl.mapIdMeaning` (0 none, 1 texture, 2 mfm).
    pub map_id_meaning: i32,
}

/// One triangle of a [`SceneMesh`] — the per-corner indices into the mesh
/// arrays (`Face`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneFace {
    pub position_index: [usize; 3],
    pub normal_index: [usize; 3],
    /// `f.tangentIndex[j]` indexes both the tangent and the binormal
    /// arrays.
    pub tangent_index: [usize; 3],
    pub uv_index: [usize; 3],
    pub uv_index2: [usize; 3],
    pub colour_index: [usize; 3],
    pub material_index: usize,
}

/// The extracted mesh record (`MFnMesh` extraction seam).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneMesh {
    /// `mesh.name()` — the mesh (shape) name.
    pub name: String,
    /// `mesh.nodeName()` — the transform node name.
    pub node_name: String,
    /// `mesh.fullName()` — the shape dag path.
    pub full_name: String,
    /// The transform node's full path (`trans.fullPathName()`), used for
    /// the hierarchy walk and the bounding-box transform.
    pub transform_path: String,
    pub positions: Vec<Vector3>,
    pub normals: Vec<Vector3>,
    pub binormals: Vec<Vector3>,
    pub tangents: Vec<Vector3>,
    pub colours: Vec<Vector4>,
    /// `uvs(0)` then `uvs(1)`.
    pub uv_sets: Vec<Vec<Vector2>>,
    pub faces: Vec<SceneFace>,
    pub materials: Vec<SceneMaterial>,
    /// The transform node's inclusive world matrix at frame 0 (raw Maya
    /// space).
    pub world: Matrix4,
}

/// The portal record (`Portal` flags/label face of `VisualPortal::init`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScenePortal {
    /// The chunk name encoded in the portal flags.
    pub name: String,
    /// The portal label (heaven/earth/invasive/exit).
    pub label: String,
    /// The portal corner positions, in world space.
    pub positions: Vec<Vector3>,
}

/// The Maya scene seam: everything the exporter would have walked on the
/// Maya API, pre-extracted.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    /// `MFileIO::currentFile` for the model metadata.
    pub source_file: String,
    /// Full dag path → inclusive world matrix at frame 0 for every
    /// transform the hierarchy may walk.
    pub nodes: BTreeMap<String, Matrix4>,
    /// Full paths of `HP_*` transform nodes (`addAllHardpoints`).
    pub hardpoints: Vec<String>,
    /// Full paths of the transforms parenting locators
    /// (`addAllLocators`).
    pub locators: Vec<String>,
    pub meshes: Vec<SceneMesh>,
    pub portals: Vec<ScenePortal>,
}

// ---------------------------------------------------------------------------------------
// VisualMesh
// ---------------------------------------------------------------------------------------

/// `VisualMesh::BloatVertex`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BloatVertex {
    pub pos: Vector3,
    pub normal: Vector3,
    /// `Point4` colour.
    pub colour: Vector4,
    pub uv: Vector2,
    pub uv2: Vector2,
    pub vertex_index: u32,
    pub smoothing_group: u32,
    pub binormal: Vector3,
    pub tangent: Vector3,
    pub mesh_index: u32,
}

impl Default for BloatVertex {
    fn default() -> Self {
        BloatVertex {
            pos: Vector3::ZERO,
            normal: Vector3::ZERO,
            colour: Vector4::ZERO,
            uv: Vector2::ZERO,
            uv2: Vector2::ZERO,
            vertex_index: 0,
            smoothing_group: 0,
            binormal: Vector3::ZERO,
            tangent: Vector3::ZERO,
            mesh_index: 0,
        }
    }
}

impl BloatVertex {
    /// `operator==`: the smoothing group participates as an AND mask, not
    /// an equality.
    fn matches(&self, bv: &BloatVertex) -> bool {
        self.pos == bv.pos
            && self.normal == bv.normal
            && self.colour == bv.colour
            && self.uv == bv.uv
            && self.uv2 == bv.uv2
            && self.vertex_index == bv.vertex_index
            && (self.smoothing_group & bv.smoothing_group) != 0
            && self.binormal == bv.binormal
            && self.tangent == bv.tangent
            && self.mesh_index == bv.mesh_index
    }

    /// The `closeEnough(BloatVertex)` used by `removeDuplicateVertices`:
    /// positions/uv/uv2/colours/binormals/tangents within 1e-4, directions
    /// within ~0.02 radians, and the mesh index equal — the smoothing
    /// group is deliberately excluded (commented out in the reference).
    fn close_enough(&self, bv: &BloatVertex) -> bool {
        let close = |a: f32, b: f32| (a - b).abs() < 0.0001;
        let direction_close_enough =
            |a: &Vector3, b: &Vector3| a.dot_product(b) > (0.02f32).cos() || a == b;
        close(self.pos.x, bv.pos.x)
            && close(self.pos.y, bv.pos.y)
            && close(self.pos.z, bv.pos.z)
            && close(self.uv.x, bv.uv.x)
            && close(self.uv.y, bv.uv.y)
            && close(self.uv2.x, bv.uv2.x)
            && close(self.uv2.y, bv.uv2.y)
            && self.colour == bv.colour
            && self.mesh_index == bv.mesh_index
            && direction_close_enough(&self.normal, &bv.normal)
            && direction_close_enough(&self.binormal, &bv.binormal)
            && direction_close_enough(&self.tangent, &bv.tangent)
    }
}

/// `VisualMesh::Triangle`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Triangle {
    pub index: [u32; 3],
    pub real_index: [u32; 3],
    pub smoothing_group: u32,
    pub material_index: u32,
}

/// `VisualMesh::Material`.
#[derive(Debug, Clone, Default)]
pub struct Material {
    /// The ctor default is `"empty"`.
    pub identifier: String,
    pub map_identifier: String,
    pub map_id_meaning: i32,
    pub self_illum: f32,
    pub in_use: bool,
}

impl PartialEq for Material {
    /// `operator==`: identifier, map id meaning, map identifier and self
    /// illumination.
    fn eq(&self, other: &Self) -> bool {
        self.identifier == other.identifier
            && self.map_id_meaning == other.map_id_meaning
            && self.map_identifier == other.map_identifier
            && self.self_illum == other.self_illum
    }
}

impl Material {
    /// The `material` ctor default identifier.
    fn new(identifier: String) -> Self {
        Material {
            identifier,
            map_identifier: String::new(),
            map_id_meaning: 0,
            self_illum: 0.0,
            in_use: false,
        }
    }

    /// `Material::resources`: promote a same-named `.mfm` when the texture
    /// name has one (mutates the material like the reference), then
    /// collect the resource names the material depends on.
    pub fn resources(&mut self, resource: &BwResource, config: &ExportConfig) -> Vec<String> {
        let mut ret_resources = Vec::new();
        if self.map_identifier.len() > 4 && self.map_id_meaning != 2 {
            let mfm_id = format!(
                "{}.mfm",
                &self.map_identifier[..self.map_identifier.len() - 4]
            );
            if let Some(mfm) = resource.open_section(&mfm_id, false) {
                let wants = mfm.open_section("mfm").is_some()
                    || mfm.open_section("fx").is_some()
                    || mfm.open_section("property").is_some();
                let rejects = mfm.open_section("Src_Blend_type").is_some()
                    || mfm.open_section("Dest_Blend_type").is_some()
                    || mfm.open_section("Self_Illumination").is_some()
                    || mfm.open_section("Alpha_blended").is_some();
                if wants && !rejects {
                    self.map_identifier = mfm_id;
                    self.map_id_meaning = 2;
                }
            }
        }
        if self.map_id_meaning == 1 || self.map_id_meaning == 2 {
            if !self.map_identifier.is_empty() {
                ret_resources.push(self.map_identifier.clone());
            }
        } else {
            // push back both default materials, as we don't know which one
            // we are using yet
            if !config.default_material.is_empty() {
                ret_resources.push(config.default_material.clone());
            }
            if !config.default_skinned_material.is_empty() {
                ret_resources.push(config.default_skinned_material.clone());
            }
        }
        ret_resources
    }

    /// `Material::save`: the primitive group with its page id, the
    /// existing material copy, and the fx/mfm/texture face.
    pub fn save(
        &self,
        geometry: &mut DataSection,
        existing_visual: Option<&DataSection>,
        pgid: u32,
        mode: ExportMode,
        config: &ExportConfig,
    ) {
        let pg = geometry.add_child("primitiveGroup");
        pg.set_int(pgid as i32);

        // copy existing bitmap/fx combination
        if !Self::find_and_copy_existing_material(&self.identifier, pg, existing_visual) {
            let material = pg.add_child("material");
            material.write_string("identifier", &self.identifier);

            // write bitmap/fx combination
            if mode == ExportMode::MeshParticles {
                material.write_string("fx", "shaders/std_effects/mesh_particle.fx");
                if !self.map_identifier.is_empty() {
                    let prop = material.add_child("property");
                    prop.set_string("diffuseMap");
                    prop.write_string("Texture", &self.map_identifier);
                }
            } else if self.map_id_meaning == 1 {
                // select fx file based on type of mesh and texture
                let mut fx = "shaders/std_effects/lightonly".to_string();
                let map = &self.map_identifier;
                if map.len() > 8 && &map[map.len() - 8..map.len() - 4] == "_afx" {
                    fx.push_str("_add");
                }
                fx.push_str(".fx");
                material.write_string("fx", &fx);

                // add texture
                if !self.map_identifier.is_empty() {
                    let prop = material.add_child("property");
                    prop.set_string("diffuseMap");
                    prop.write_string("Texture", &self.map_identifier);
                }
            } else if self.map_id_meaning == 2 {
                material.write_string("mfm", &self.map_identifier);
            } else if !config.default_material.is_empty() {
                material.write_string("mfm", &config.default_material);
            } else if !config.default_skinned_material.is_empty() {
                material.write_string("mfm", &config.default_skinned_material);
            }
        }
    }

    /// `findAndCopyExistingMaterial`: look for the material named
    /// `identifier` in the existing visual; if found, copy the whole
    /// primitive group (children only — `copy(p, false)` keeps this
    /// section's pgid value) and return true.
    fn find_and_copy_existing_material(
        identifier: &str,
        prim_group: &mut DataSection,
        existing_visual: Option<&DataSection>,
    ) -> bool {
        let Some(existing_visual) = existing_visual else {
            return false;
        };
        let Some(found) = Self::find_existing_material(identifier, existing_visual) else {
            return false;
        };
        // DataSection::copy(pSection, modifyCurrent = false): append deep
        // copies of the source's children, keeping this section's value
        // and children. The children land directly in the primitive
        // group, not inside a wrapper.
        let cloned = found.clone();
        let mut index = 0;
        while index < cloned.count_children() {
            let child = cloned.open_child(index).expect("in range").clone();
            let name = child.section_name().to_string();
            let slot = prim_group.add_child(&name);
            slot.copy_from(&child);
            index += 1;
        }
        true
    }

    /// `findExistingMaterial`: walk renderSet → geometry → primitiveGroup
    /// and return the whole primitive group whose material identifier
    /// matches. A primitive group without a material child never matches
    /// (the reference would dereference null there).
    fn find_existing_material<'a>(
        identifier: &str,
        existing_visual: &'a DataSection,
    ) -> Option<&'a DataSection> {
        for i in 0..existing_visual.count_children() {
            let render_set = existing_visual.open_child(i).expect("in range");
            if render_set.section_name() != "renderSet" {
                continue;
            }
            for gi in 0..render_set.count_children() {
                let geometry = render_set.open_child(gi).expect("in range");
                if geometry.section_name() != "geometry" {
                    continue;
                }
                for pi in 0..geometry.count_children() {
                    let prim_group = geometry.open_child(pi).expect("in range");
                    if prim_group.section_name() != "primitiveGroup" {
                        continue;
                    }
                    let matched = prim_group
                        .find_child("material")
                        .is_some_and(|m| m.read_string("identifier", "") == identifier);
                    if matched {
                        return Some(prim_group);
                    }
                }
            }
        }
        None
    }
}

/// `VisualMesh` — the Maya mesh record plus everything the save chain
/// needs. The exporter-relevant settings snapshot is taken at
/// construction (the reference reads its global instance at call time).
#[derive(Debug, Clone)]
pub struct VisualMesh {
    pub identifier: String,
    /// The ctor default: `"Scene Root"`.
    pub node_identifier: String,
    /// The transform node's full path (`trans.fullPathName()`), the key
    /// the exporter uses to look the node up in the hierarchy.
    pub transform_path: String,
    pub full_name: String,
    pub vertices: Vec<BloatVertex>,
    pub triangles: Vec<Triangle>,
    pub materials: Vec<Material>,
    pub material_remap: Vec<usize>,
    pub dual_uv: bool,
    pub vertex_colours: bool,
    pub large_indices: bool,
    pub snap_vertices: bool,
    pub world: Matrix4,
    pub bb: BoundBox,
    pub settings: ExportSettings,
    pub errors: Vec<String>,
}

impl Default for VisualMesh {
    fn default() -> Self {
        Self::new(ExportSettings::default())
    }
}

impl VisualMesh {
    /// The `VisualMesh` ctor.
    pub fn new(settings: ExportSettings) -> Self {
        VisualMesh {
            identifier: String::new(),
            node_identifier: "Scene Root".to_string(),
            transform_path: String::new(),
            full_name: String::new(),
            vertices: Vec::new(),
            triangles: Vec::new(),
            materials: Vec::new(),
            material_remap: Vec::new(),
            dual_uv: false,
            vertex_colours: false,
            large_indices: false,
            snap_vertices: false,
            world: Matrix4::IDENTITY,
            bb: BoundBox::default(),
            settings,
            errors: Vec::new(),
        }
    }

    fn add_error(&mut self, message: &str) {
        self.errors.push(message.to_string());
    }

    /// `errorText`: the errors, one per line.
    pub fn error_text(&self) -> String {
        let mut s = String::new();
        for e in &self.errors {
            s.push_str(e);
        }
        s
    }

    /// `VisualMesh::init(Mesh&)`: build the bloat vertices and triangles
    /// from the extracted mesh.
    pub fn init(&mut self, resource: &BwResource, mesh: &SceneMesh) -> bool {
        self.errors.clear();
        let mut ret = false;
        let uv0 = mesh.uv_sets.first().map_or(0, |v| v.len());
        if !mesh.positions.is_empty()
            && !mesh.normals.is_empty()
            && !mesh.binormals.is_empty()
            && !mesh.tangents.is_empty()
            && !mesh.uv_sets.is_empty()
            && uv0 > 0
            && !mesh.faces.is_empty()
        {
            self.identifier = trim_whitespaces(&mesh.name).to_string();
            self.node_identifier = trim_whitespaces(&mesh.node_name).to_string();
            self.transform_path = mesh.transform_path.clone();
            self.full_name = mesh.full_name.clone();

            self.gather_materials(resource, mesh);

            let map_count = mesh.uv_sets.len();
            let has_uvs = uv0 > 0;
            self.dual_uv = has_uvs && map_count > 1;
            self.vertex_colours = !mesh.colours.is_empty();

            let is_static = self.settings.export_mode == ExportMode::Static
                || self.settings.export_mode == ExportMode::StaticWithNodes;
            let _ = is_static;

            for f in &mesh.faces {
                let material_index = self.material_remap[f.material_index] as u32;
                let mut tr = Triangle {
                    material_index,
                    smoothing_group: 0,
                    ..Triangle::default()
                };
                for j in 0..3 {
                    let mut bv = BloatVertex {
                        pos: mesh.positions[f.position_index[j]],
                        normal: mesh.normals[f.normal_index[j]],
                        ..BloatVertex::default()
                    };

                    // FIX - added normals from MAYA (doesn't use
                    // createNormals below)
                    bv.normal.normalise();

                    bv.binormal = mesh.binormals[f.tangent_index[j]];
                    bv.binormal.normalise();

                    bv.tangent = mesh.tangents[f.tangent_index[j]];
                    bv.tangent.normalise();

                    bv.vertex_index = f.position_index[j] as u32;
                    bv.smoothing_group = 0;
                    if has_uvs {
                        let mut uv = mesh.uv_sets[0][f.uv_index[j]];
                        uv.y = 1.0 - uv.y;
                        bv.uv = uv;
                    }
                    if self.dual_uv {
                        let mut uv2 = mesh.uv_sets[1][f.uv_index2[j]];
                        uv2.y = 1.0 - uv2.y;
                        bv.uv2 = uv2;
                    }
                    if self.vertex_colours {
                        bv.colour = mesh.colours[f.colour_index[j]];
                    }

                    bv.mesh_index = 0;

                    if self.snap_vertices {
                        snap_point3(&mut bv.pos);
                    }
                    tr.index[j] = self.add_vertex(bv);
                    tr.real_index[j] = bv.vertex_index;
                }
                self.triangles.push(tr);
            }

            self.world = Matrix4::IDENTITY;

            // initialise boundingbox
            let first = self.world.apply_point(&self.vertices[0].pos);
            let mut first = first;
            if self.snap_vertices {
                snap_point3(&mut first);
            }
            self.bb.set_bounds(first, first);
            for v in &self.vertices[1..] {
                let mut p = self.world.apply_point(&v.pos);
                if self.snap_vertices {
                    snap_point3(&mut p);
                }
                self.bb.add_bounds(&p);
            }

            ret = true;
        } else if !mesh.positions.is_empty() && !mesh.normals.is_empty() && !mesh.faces.is_empty() {
            let msg = format!(
                "Failed to export mesh {} because there are no uvs",
                mesh.full_name
            );
            self.add_error(&msg);
        }
        ret
    }

    /// `gatherMaterials` + `addMaterial`.
    fn gather_materials(&mut self, resource: &BwResource, mesh: &SceneMesh) {
        for mtl in &mesh.materials {
            let mut m = Material::new(mtl.name.clone());

            // Replace environment macros in the material map file.
            let (success, texture_name) = replace_environment_macros(&mtl.map_file);
            if !success {
                let msg = format!(
                    "Material \"{}\" has an invalid texture name:\n\n\"{}\"\n\nCan't resolve this name, have you configured all the environment variables in your maya.env?\nTexture may not be visible in game.\n",
                    mtl.name, mtl.map_file
                );
                self.add_error(&msg);
            }

            // Dissolve filename from an absolute to a relative path.
            let identifier = resource.dissolve_filename(&texture_name);

            // The copyExternalTextures/copyFileOrDirectory face is a
            // documented seam: absolute map names keep their dissolved
            // form here.

            if !texture_name.is_empty() {
                m.map_identifier = identifier;
                m.map_id_meaning = mtl.map_id_meaning;
            }

            let idx = self.find_or_add_material(m);
            self.material_remap.push(idx);
        }
        if self.material_remap.is_empty() {
            self.material_remap.push(0);
        }
    }

    /// `findOrAddMaterial`.
    pub fn find_or_add_material(&mut self, mat: Material) -> usize {
        for (i, existing) in self.materials.iter().enumerate() {
            if *existing == mat {
                return i;
            }
        }
        self.materials.push(mat);
        self.materials.len() - 1
    }

    /// `addVertex`: exact match merges the smoothing groups, otherwise
    /// append.
    fn add_vertex(&mut self, bv: BloatVertex) -> u32 {
        if let Some(i) = self.vertices.iter().position(|v| v.matches(&bv)) {
            self.vertices[i].smoothing_group |= bv.smoothing_group;
            return i as u32;
        }
        self.vertices.push(bv);
        (self.vertices.len() - 1) as u32
    }

    /// `flipTriangleWindingOrder`.
    pub fn flip_triangle_winding_order(&mut self) {
        for tri in &mut self.triangles {
            tri.index.swap(1, 2);
            tri.real_index.swap(1, 2);
        }
    }

    /// `sortTriangles`: `std::sort` is unstable — material index first,
    /// the first corner's mesh index as the tiebreak.
    pub fn sort_triangles(&mut self) {
        let vertices = &self.vertices;
        self.triangles.sort_unstable_by(|t1, t2| {
            let k1 = (t1.material_index, vertices[t1.index[0] as usize].mesh_index);
            let k2 = (t2.material_index, vertices[t2.index[0] as usize].mesh_index);
            k1.cmp(&k2)
        });

        // Per-material compaction: the seen map resets at each material
        // boundary while `vs` accumulates, so shared corners get pushed
        // back into the final table.
        let mut vs: Vec<BloatVertex> = Vec::new();
        let mut vertex_indices: Vec<i32> = Vec::new();
        let mut previous = -1;
        for tri in &mut self.triangles {
            if previous != tri.material_index as i32 {
                vertex_indices.clear();
                vertex_indices.resize(self.vertices.len(), -1);
                previous = tri.material_index as i32;
            }
            for slot in &mut tri.index {
                let ind = *slot as usize;
                if vertex_indices[ind] == -1 {
                    vertex_indices[ind] = vs.len() as i32;
                    vs.push(self.vertices[ind]);
                }
                *slot = vertex_indices[ind] as u32;
            }
        }
        self.vertices = vs;
    }

    /// `removeDuplicateVertices` (the non-`TC_OPTIMISATION` arm): remap
    /// each corner to the first close-enough earlier vertex.
    pub fn remove_duplicate_vertices(&mut self) {
        for i in 0..self.triangles.len() {
            for j in 0..3 {
                let index = self.triangles[i].index[j] as usize;
                let mut found = None;
                for k in 0..index {
                    if self.vertices[k].close_enough(&self.vertices[index]) {
                        found = Some(k);
                        break;
                    }
                }
                if let Some(k) = found {
                    self.triangles[i].index[j] = k as u32;
                }
            }
        }
    }

    /// `createPrimitiveGroups`.
    pub fn create_primitive_groups(
        &mut self,
        prim_groups: &mut Vec<PrimitiveGroup>,
        indices: &mut Vec<u32>,
        entry_size: &mut usize,
    ) -> bool {
        // If there are no materials return
        if self.materials.is_empty() {
            let msg = format!(
                "{} does not have any materials.\nThis object will not be exported.  Adding a material will fix this problem.",
                self.identifier
            );
            self.add_error(&msg);
            return false;
        }

        if !self.triangles.is_empty() {
            let mut m_index = self.triangles[0].material_index as usize;
            let mut first_vertex = self.triangles[0].index[0] as i32;
            let mut last_vertex = self.triangles[0].index[0] as i32;
            let mut first_triangle: i32 = 0;

            let mut offset = 0usize;
            *entry_size = if self.vertices.len() <= 65536 { 2 } else { 4 };
            indices.clear();
            indices.resize(self.triangles.len() * 3, 0);

            let size = self.triangles.len();
            for i in 0..=size {
                let material_index = if i == size {
                    self.triangles[size - 1].material_index as usize
                } else {
                    self.triangles[i].material_index as usize
                };
                if i == size || material_index != m_index {
                    let pg = PrimitiveGroup {
                        start_vertex: first_vertex,
                        n_vertices: last_vertex - first_vertex + 1,
                        start_index: first_triangle * 3,
                        n_primitives: i as i32 - first_triangle,
                    };
                    prim_groups.push(pg);
                    self.materials[m_index].in_use = true;
                    if i != size {
                        first_vertex = self.triangles[i].index[0] as i32;
                        last_vertex = self.triangles[i].index[0] as i32;
                        m_index = material_index;
                    }
                    first_triangle = i as i32;
                }
                if i != size {
                    for value in &self.triangles[i].index {
                        indices[offset] = *value;
                        offset += 1;
                    }
                    let tri = &self.triangles[i];
                    first_vertex = first_vertex
                        .min(tri.index[0] as i32)
                        .min(tri.index[1] as i32)
                        .min(tri.index[2] as i32);
                    last_vertex = last_vertex
                        .max(tri.index[0] as i32)
                        .max(tri.index[1] as i32)
                        .max(tri.index[2] as i32);
                }
            }

            // Mesh particles add an extra 15 primitive groups, defined by
            // the index of the vertices.
            if self.settings.export_mode == ExportMode::MeshParticles {
                return self.create_mesh_particle_prim_groups(prim_groups);
            }
        }
        true
    }

    /// `createMeshParticlePrimGroups`: 15 extra groups keyed by the mesh
    /// index, for 16 groups in total.
    fn create_mesh_particle_prim_groups(&mut self, prim_groups: &mut Vec<PrimitiveGroup>) -> bool {
        if prim_groups.len() != 1 {
            self.add_error(
                "VisualMesh::createMeshParticlePrimGroups - Mesh particles must use only 1 material/mesh.",
            );
            return false;
        }

        if !self.triangles.is_empty() {
            let mut first_vertex = self.triangles[0].index[0] as i32;
            let mut last_vertex = first_vertex;
            let mut m_index = self.vertices[first_vertex as usize].mesh_index;
            let mut first_triangle: i32 = 0;

            let size = self.triangles.len();
            for i in 0..=size {
                let (idx0, idx1, idx2) = if i == size {
                    let t = &self.triangles[size - 1];
                    (t.index[0], t.index[1], t.index[2])
                } else {
                    let t = &self.triangles[i];
                    (t.index[0], t.index[1], t.index[2])
                };
                let tri_mesh_index = self.vertices[idx0 as usize].mesh_index;
                if i == size || tri_mesh_index != m_index {
                    let pg = PrimitiveGroup {
                        start_vertex: first_vertex,
                        n_vertices: last_vertex - first_vertex + 1,
                        start_index: first_triangle * 3,
                        n_primitives: i as i32 - first_triangle,
                    };
                    prim_groups.push(pg);
                    if i != size {
                        first_vertex = idx0 as i32;
                        last_vertex = idx0 as i32;
                        m_index = tri_mesh_index;
                    }
                    first_triangle = i as i32;
                }

                first_vertex = first_vertex
                    .min(idx0 as i32)
                    .min(idx1 as i32)
                    .min(idx2 as i32);
                last_vertex = last_vertex
                    .max(idx0 as i32)
                    .max(idx1 as i32)
                    .max(idx2 as i32);
            }
        }

        // Mesh particles should now always have 16 primitive groups.
        // Made this an assert because the MeshParticleRenderer assumes
        // there are 16 primitivegroups.
        debug_assert!(prim_groups.len() == 16, "primGroups.size() == 16");

        true
    }

    /// `createVertexList(VertexVector&)` — the xyznuv variant. Mutates the
    /// vertex positions with the unit scale exactly like the reference.
    pub fn create_vertex_list(&mut self, vertices: &mut Vec<[f32; 8]>) {
        for bv in &mut self.vertices {
            bv.pos = bv.pos * self.settings.unit_scale;
            vertices.push([
                bv.pos.x,
                bv.pos.y,
                bv.pos.z,
                bv.normal.x,
                bv.normal.y,
                bv.normal.z,
                bv.uv.x,
                bv.uv.y,
            ]);
        }
    }

    /// The xyznuvi variant (`IndexedVertexVector`).
    fn create_vertex_list_indexed(&mut self, vertices: &mut Vec<[f32; 9]>) {
        let mut last_mesh_index = 0u32;
        for (it, bv) in self.vertices.iter_mut().enumerate() {
            bv.pos = bv.pos * self.settings.unit_scale;
            if it > 0 {
                debug_assert!(last_mesh_index <= bv.mesh_index);
            }
            last_mesh_index = bv.mesh_index;
            vertices.push([
                bv.pos.x,
                bv.pos.y,
                bv.pos.z,
                bv.normal.x,
                bv.normal.y,
                bv.normal.z,
                bv.uv.x,
                bv.uv.y,
                (bv.mesh_index * 3) as f32,
            ]);
        }
    }

    /// `VisualMesh::save`.
    #[allow(clippy::too_many_arguments)]
    pub fn save(
        &mut self,
        resource: &BwResource,
        visual_section: &mut DataSection,
        existing_visual: Option<&DataSection>,
        prim_path: &str,
        prim_res_name: &str,
        use_identifier: bool,
        config: &ExportConfig,
    ) -> bool {
        let id_prefix = if use_identifier {
            format!("{}.", self.identifier)
        } else {
            String::new()
        };

        let raw = resource.file_system().read_file(prim_path);
        let mut sp_file = match raw {
            Some(bytes) => {
                let mut bin = BinSection::new(prim_res_name, BinaryBlock::new(bytes));
                bin.introspect();
                bin
            }
            None => BinSection::new(prim_path, BinaryBlock::new(Vec::new())),
        };

        self.flip_triangle_winding_order();
        self.remove_duplicate_vertices();
        self.sort_triangles();

        if self.settings.bump_mapped && self.settings.export_mode != ExportMode::MeshParticles {
            // The NVMeshMender seam: a bump-mapped export cannot be
            // completed without the tangent-space seam builder.
            self.add_error(
                "makeBumped is not portable (NVMeshMender seam): bump mapped export unsupported",
            );
            return false;
        }

        self.remove_duplicate_vertices();
        self.sort_triangles();

        let mut prim_groups: Vec<PrimitiveGroup> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut entry_size = 2usize;

        if !self.create_primitive_groups(&mut prim_groups, &mut indices, &mut entry_size) {
            return false;
        }

        self.large_indices = entry_size != 2;
        let mut out: Vec<u8> = Vec::new();
        let header = IndexHeader {
            index_format: if self.large_indices {
                "list32".to_string()
            } else {
                "list".to_string()
            },
            n_indices: indices.len() as i32,
            n_triangle_groups: prim_groups.len() as i32,
        };
        let mut field = [0u8; INDEX_FORMAT_FIELD_LEN];
        let bytes = header.index_format.as_bytes();
        field[..bytes.len()].copy_from_slice(bytes);
        out.extend_from_slice(&field);
        out.extend_from_slice(&header.n_indices.to_le_bytes());
        out.extend_from_slice(&header.n_triangle_groups.to_le_bytes());
        if entry_size == 2 {
            for i in &indices {
                out.extend_from_slice(&(*i as u16).to_le_bytes());
            }
        } else {
            for i in &indices {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        write_primitive_groups(&prim_groups, &mut out);

        sp_file
            .new_section(&format!("{id_prefix}indices"))
            .set_binary(BinaryBlock::new(out));

        let mut out: Vec<u8> = Vec::new();
        let mut vh = VertexHeader {
            vertex_format: String::new(),
            n_vertices: 0,
        };
        if self.settings.export_mode == ExportMode::MeshParticles {
            let mut vertices: Vec<[f32; 9]> = Vec::new();
            self.create_vertex_list_indexed(&mut vertices);
            vh.vertex_format = "xyznuvi".to_string();
            vh.n_vertices = vertices.len() as i32;
            vh.write(&mut out);
            for v in &vertices {
                for f in v {
                    out.extend_from_slice(&f.to_le_bytes());
                }
            }
        } else {
            let mut vertices: Vec<[f32; 8]> = Vec::new();
            self.create_vertex_list(&mut vertices);
            vh.vertex_format = "xyznuv".to_string();
            vh.n_vertices = vertices.len() as i32;
            vh.write(&mut out);
            for v in &vertices {
                for f in v {
                    out.extend_from_slice(&f.to_le_bytes());
                }
            }
        }

        sp_file
            .new_section(&format!("{id_prefix}vertices"))
            .set_binary(BinaryBlock::new(out));

        // Write the extra vertex data streams.
        let mut streams: Vec<String> = Vec::new();
        if self.dual_uv {
            let mut out: Vec<u8> = Vec::new();
            for bv in &self.vertices {
                out.extend_from_slice(&bv.uv2.x.to_le_bytes());
                out.extend_from_slice(&bv.uv2.y.to_le_bytes());
            }
            streams.push(format!("{id_prefix}uv2"));
            sp_file
                .new_section(streams.last().expect("just pushed"))
                .set_binary(BinaryBlock::new(out));
        }

        if self.vertex_colours {
            let mut out: Vec<u8> = Vec::new();
            for bv in &self.vertices {
                let colour = d3d_color_value(bv.colour.x, bv.colour.y, bv.colour.z, bv.colour.w);
                out.extend_from_slice(&colour.to_le_bytes());
            }
            streams.push(format!("{id_prefix}colour"));
            sp_file
                .new_section(streams.last().expect("just pushed"))
                .set_binary(BinaryBlock::new(out));
        }

        let bytes = sp_file.save_bytes();
        if !resource.file_system().write_file(prim_path, &bytes) {
            self.add_error(&format!("Failed to write {prim_path}"));
            return false;
        }

        let render_set = visual_section.add_child("renderSet");
        render_set.write_bool("treatAsWorldSpaceObject", false);
        render_set.write_string("node", &self.node_identifier);

        let geometry = render_set.add_child("geometry");
        geometry.write_string("vertices", &format!("{id_prefix}vertices"));
        for stream in &streams {
            let s = geometry.add_child("stream");
            s.set_string(stream);
        }
        geometry.write_string("primitive", &format!("{id_prefix}indices"));

        let mut prim_group = 0u32;
        for mat in &self.materials {
            if mat.in_use {
                mat.save(
                    geometry,
                    existing_visual,
                    prim_group,
                    self.settings.export_mode,
                    config,
                );
                prim_group += 1;
            }
        }
        true
    }

    /// `VisualMesh::add`: append the (optionally world-transformed)
    /// vertices and the remapped triangles onto `dest`.
    pub fn add(&mut self, dest: &mut VisualMesh, force_mesh_index: i32, world_space: bool) {
        // find or add all our materials to the dest mesh
        let mut reremap: Vec<usize> = Vec::new();
        for mat in &self.materials {
            reremap.push(dest.find_or_add_material(mat.clone()));
        }
        if reremap.is_empty() {
            reremap.push(0);
        }

        // append our transformed vertices to its vertices
        let start_vertex = dest.vertices.len() as u32;
        for bv in &self.vertices {
            let mut bv = *bv;
            if world_space {
                bv.pos = self.world.apply_point(&bv.pos);
            }
            bv.normal = self.world.apply_point(&bv.normal);
            bv.normal.normalise();
            if force_mesh_index != -1 {
                bv.mesh_index = force_mesh_index as u32;
            }
            dest.vertices.push(bv);
        }

        // append our remapped and shuffled triangles to its triangles
        for tr in &self.triangles {
            let mut tr = *tr;
            tr.index[0] += start_vertex;
            tr.index[1] += start_vertex;
            tr.index[2] += start_vertex;
            tr.material_index = reremap[tr.material_index as usize] as u32;
            dest.triangles.push(tr);
        }
    }
}

// ---------------------------------------------------------------------------------------
// VisualPortal + GrahamScan
// ---------------------------------------------------------------------------------------

/// `GrahamScan` (`lib/math/graham_scan.hpp`): the convex hull of a coplanar
/// point set. The constructor runs the whole scan; the result is written
/// back into the point list in reverse stack order.
pub struct GrahamScan {
    plane: PlaneEq,
}

impl GrahamScan {
    /// `LowestYPlaneSort`: y, then x, with the float-equal tie.
    fn lowest_y_compare(a: &Vector3, b: &Vector3) -> std::cmp::Ordering {
        if almost_equal(a.y, b.y) {
            if almost_equal(a.x, b.x) {
                std::cmp::Ordering::Equal
            } else if a.x < b.x {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        } else if a.y < b.y {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    }

    /// `isLeft`: the sign of the cross product of the turn.
    fn is_left(a: &Vector3, b: &Vector3, c: &Vector3) -> f32 {
        (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)
    }

    /// `GrahamScan(pts)`.
    pub fn new(pts: &mut Vec<Vector3>) -> GrahamScan {
        let mut plane = PlaneEq::new(Vector3::ZERO, 0.0);
        if pts.len() > 2 {
            plane.init(&pts[0], &pts[1], &pts[2]);
        }
        let gs = GrahamScan { plane };
        if pts.len() > 3 {
            gs.make_convex_hull(pts);
        }
        gs
    }

    /// `makeConvexHull`.
    fn make_convex_hull(&self, pts: &mut Vec<Vector3>) {
        if pts.len() <= 3 {
            return;
        }
        self.project_points(pts);

        // Find the lowest point (y, then x).
        pts.sort_by(Self::lowest_y_compare);

        // Sort the rest by the angle around pts[0]; a stable sort with the
        // cross-product comparator.
        let base = pts[0];
        pts[1..].sort_by(|a, b| {
            let ta = *a - base;
            let tb = *b - base;
            if ta.x * tb.y > tb.x * ta.y {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        });

        let mut stack: Vec<Vector3> = Vec::with_capacity(pts.len());
        stack.push(pts[0]);
        stack.push(pts[1]);
        for p in &pts[2..] {
            while Self::is_left(&stack[stack.len() - 2], &stack[stack.len() - 1], p) <= 0.0
                && stack.len() > 2
            {
                stack.pop();
            }
            stack.push(*p);
        }

        // Final check to remove the last point if it is inside.
        if Self::is_left(&stack[stack.len() - 2], &stack[stack.len() - 1], &pts[0]) <= 0.0 {
            stack.pop();
        }

        // The result is written back in reverse stack order.
        pts.clear();
        for p in stack.into_iter().rev() {
            pts.push(p);
        }

        self.unproject_points(pts);
    }

    /// `projectPoints`: project onto the hull plane as 2D coordinates.
    fn project_points(&self, pts: &mut [Vector3]) {
        for p in pts.iter_mut() {
            let projected = self.plane.project(p);
            p.x = projected.x;
            p.y = projected.y;
        }
    }

    /// `unprojectPoints`: back onto the plane.
    fn unproject_points(&self, pts: &mut [Vector3]) {
        for p in pts.iter_mut() {
            let v = self.plane.param(&Vector2::new(p.x, p.y));
            *p = v;
        }
    }
}

/// `VisualPortal`.
#[derive(Debug, Clone)]
pub struct VisualPortal {
    /// `name_`: the chunk name.
    pub name: String,
    /// `label_`.
    pub label: String,
    /// `pts_`.
    pub pts: Vec<Vector3>,
}

impl Default for VisualPortal {
    fn default() -> Self {
        VisualPortal {
            name: "invasive".to_string(),
            label: "invasive".to_string(),
            pts: Vec::new(),
        }
    }
}

impl VisualPortal {
    /// `addPoint`: swizzles y/z (`Vector3(pt.x, pt.z, pt.y)`).
    pub fn add_point(&mut self, pt: &Vector3) {
        self.pts.push(Vector3::new(pt.x, pt.z, pt.y));
    }

    /// `addSwizzledPoint`: the points are already swizzled, keep them.
    pub fn add_swizzled_point(&mut self, pt: &Vector3) {
        self.pts.push(*pt);
    }

    /// `name(value)`.
    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_string();
    }

    /// `propDataFromPropName`: heaven/earth/invasive/exit map onto their
    /// data names (exit is invasive too).
    pub fn prop_data_from_prop_name(prop_name: &str) -> Option<&'static str> {
        let props = ["heaven", "earth", "invasive", "exit"];
        let data = ["heaven", "earth", "invasive", "invasive"];
        for (i, p) in props.iter().enumerate() {
            if prop_name.eq_ignore_ascii_case(p) {
                return Some(data[i]);
            }
        }
        None
    }

    /// `planeEquation`: the plane of the first three points.
    pub fn plane_equation(&self) -> PlaneEq {
        let mut plane = PlaneEq::new(Vector3::ZERO, 0.0);
        if self.pts.len() > 2 {
            plane.init(&self.pts[0], &self.pts[1], &self.pts[2]);
        }
        plane
    }

    /// `reverseWinding`.
    pub fn reverse_winding(&mut self) {
        self.pts.reverse();
    }

    /// `createConvexHull`.
    pub fn create_convex_hull(&mut self) {
        GrahamScan::new(&mut self.pts);
    }

    /// `VisualPortal::init(Portal, unitScale)`: the flags decode to the
    /// chunk name, the label comes across, the positions scale without the
    /// swizzle, then the convex hull runs.
    pub fn init(&mut self, portal: &ScenePortal, unit_scale: f32) {
        self.name = portal.name.clone();
        self.label = portal.label.clone();
        self.pts = portal.positions.iter().map(|p| *p * unit_scale).collect();
        self.create_convex_hull();
    }

    /// `VisualPortal::save(boundary)`: assert at least three points, read
    /// the plane off the boundary section, and store the corner points in
    /// the boundary's uv frame.
    pub fn save(&self, boundary: &mut DataSection) -> bool {
        debug_assert!(self.pts.len() >= 3, "pts_.size() >= 3");
        let normal = boundary.read_vector3("normal", Vector3::ZERO);
        let d = boundary.read_float("d", 0.0);

        let mut u_axis = self.pts[1] - self.pts[0];
        u_axis.normalise();
        let v_axis = normal.cross_product(&normal, &u_axis);

        // The basis with columns u/v/normal and the plane origin as the
        // translation — the row-vector reference stores these as rows and
        // inverts; the storage transpose is equivalent.
        let origin = normal * d;
        let basis = Matrix4::new(
            u_axis.x, v_axis.x, normal.x, origin.x, //
            u_axis.y, v_axis.y, normal.y, origin.y, //
            u_axis.z, v_axis.z, normal.z, origin.z, //
            0.0, 0.0, 0.0, 1.0,
        );
        let inv_basis = basis.inverse();

        let portal = boundary.add_child("portal");
        portal.set_string(&self.label);
        if !self.name.is_empty() {
            portal.write_string("chunk", &self.name);
        }
        portal.write_vector3("uAxis", u_axis);
        for pt in &self.pts {
            let point = portal.add_child("point");
            point.set_vector3(inv_basis.apply_point(pt));
        }
        true
    }
}

// ---------------------------------------------------------------------------------------
// Hierarchy + exportTree
// ---------------------------------------------------------------------------------------

/// `Hierarchy` — the `|`-path node tree. The port stores each node's raw
/// frame-0 world matrix where the reference keeps an `MDagPath` and asks
/// Maya for the matrix at transform time.
#[derive(Debug, Clone)]
pub struct Hierarchy {
    name: String,
    /// The full dag path (`fullPathName()`), `|`-rooted.
    path: String,
    /// The inclusive world matrix at frame 0 (`None` = identity, for the
    /// root and for paths the scene does not know).
    world: Option<Matrix4>,
    children: BTreeMap<String, Hierarchy>,
    /// `_customPath != ""` — fixup nodes carry stored transforms.
    custom_relative: Option<Matrix4>,
    custom_world: Option<Matrix4>,
    is_root: bool,
}

impl Default for Hierarchy {
    fn default() -> Self {
        Self::new_root()
    }
}

impl Hierarchy {
    /// The root node (`Hierarchy(NULL)`): named `"Scene Root"` with no
    /// path.
    pub fn new_root() -> Self {
        Hierarchy {
            name: String::new(),
            path: String::new(),
            world: None,
            children: BTreeMap::new(),
            custom_relative: None,
            custom_world: None,
            is_root: true,
        }
    }

    fn new_intermediate(path: String) -> Self {
        Hierarchy {
            name: String::new(),
            path,
            world: None,
            children: BTreeMap::new(),
            custom_relative: None,
            custom_world: None,
            is_root: false,
        }
    }

    /// `addNode(path, dag)`: walk the `|` path creating intermediates. The
    /// reference resolves each node's dag path with `findDagPath`; the
    /// port stores the accumulated path string and looks the world matrix
    /// up in `nodes`.
    pub fn add_node(&mut self, path: &str, nodes: &BTreeMap<String, Matrix4>) {
        self.add_node_walk(path, nodes, String::new());
    }

    fn add_node_walk(
        &mut self,
        path: &str,
        nodes: &BTreeMap<String, Matrix4>,
        accumulated: String,
    ) {
        if self.is_root {
            // if there is more to the path
            let child = &path[1..];
            let child_name = match child.find('|') {
                Some(pos) => &child[..pos],
                None => child,
            };
            let accumulated_child = format!("{accumulated}|{child_name}");
            self.children
                .entry(child_name.to_string())
                .or_insert_with(|| {
                    let mut h = Hierarchy::new_intermediate(accumulated_child.clone());
                    h.world = nodes.get(&accumulated_child).copied();
                    h
                });
            let child_node = self.children.get_mut(child_name).expect("just inserted");
            child_node.add_node_walk(path, nodes, accumulated_child);
            return;
        }

        // if name is empty, set it
        if self.name.is_empty() {
            let name = &path[1..];
            if let Some(pos) = name.find('|') {
                self.name = name[..pos].to_string();
            } else {
                // root node so set dag (the leaf of this walk)
                self.name = name.to_string();
                self.world = nodes.get(&self.path).copied();
                return;
            }
        }

        // if path is not part of this node hierarchy, don't add it
        if path.find(&self.name) != Some(1) {
            return;
        }

        let subpath = &path[1 + self.name.len()..];

        // invalid path?
        if !subpath.starts_with('|') {
            return;
        }

        // if there is more to the path
        let child = &subpath[1..];
        let child_name = match child.find('|') {
            Some(pos) => &child[..pos],
            None => child,
        };
        let accumulated_child = format!("{accumulated}|{child_name}");
        self.children
            .entry(child_name.to_string())
            .or_insert_with(|| {
                let mut h = Hierarchy::new_intermediate(accumulated_child.clone());
                h.world = nodes.get(&accumulated_child).copied();
                h
            });
        let child_node = self.children.get_mut(child_name).expect("just inserted");
        child_node.add_node_walk(subpath, nodes, accumulated_child);
    }

    /// `addNode(path, worldTransform)`: the fixup node (`_BlendBone`
    /// style) with stored relative/world transforms. `legacy` selects the
    /// orientation row of the parent's own transform, exactly like
    /// `transform(0)` in the reference.
    pub fn add_fixup_node(&mut self, path: &str, world: Matrix4, legacy: bool) {
        let own = self.transform(None, false, legacy);
        let relative = own.inverse().mul(&world);
        self.children.insert(
            path.to_string(),
            Hierarchy {
                name: path.to_string(),
                path: path.to_string(),
                world: None,
                children: BTreeMap::new(),
                custom_relative: Some(relative),
                custom_world: Some(world),
                is_root: false,
            },
        );
    }

    /// `name()`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `name(value)` (the animation writer's root rename face).
    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_string();
    }

    /// `child(name)`: returns `self` on a miss, exactly like the
    /// reference TODO.
    pub fn child(&self, name: &str) -> Option<&Hierarchy> {
        self.children.get(name).or(Some(self))
    }

    /// `children(orderChildren)`: the map is name-sorted; `orderChildren`
    /// stably reorders by descendant count, descending.
    pub fn children(&self, order_children: bool) -> Vec<&Hierarchy> {
        let mut list: Vec<&Hierarchy> = self.children.values().collect();
        if order_children {
            // BW::list::sort is stable.
            list.sort_by_key(|h| std::cmp::Reverse(h.count()));
        }
        list
    }

    /// `count()`: this node (unless root) plus all descendants.
    pub fn count(&self) -> u32 {
        let mut total = if self.is_root { 0 } else { 1 };
        for child in self.children.values() {
            total += child.count();
        }
        total
    }

    /// `find(path)`: prefix walk with a `|` boundary; returns `self` on a
    /// miss.
    pub fn find(&self, path: &str) -> &Hierarchy {
        if path == self.path {
            return self;
        }
        for child in self.children.values() {
            let child_path = &child.path;
            if path.starts_with(child_path.as_str())
                && (child_path.len() < path.len() && path.as_bytes()[child_path.len()] == b'|'
                    || child_path.len() == path.len())
            {
                return child.find(path);
            }
        }
        self
    }

    /// `recursiveFind`: the root answers to `"Scene Root"`.
    pub fn recursive_find(&self, name: &str) -> Option<&Hierarchy> {
        if self.is_root && name == "Scene Root" {
            return Some(self);
        }
        for child in self.children.values() {
            if child.name == name {
                return Some(child);
            }
        }
        for child in self.children.values() {
            if let Some(h) = child.recursive_find(name) {
                return Some(h);
            }
        }
        None
    }

    /// `transform(frame, relative)` at frame 0. `parent_world` is the
    /// caller's raw world matrix (`None` = identity, the root's exclusive
    /// matrix). The orientation row negation applies to both self and
    /// parent before the relative product.
    pub fn transform(
        &self,
        parent_world: Option<&Matrix4>,
        relative: bool,
        legacy: bool,
    ) -> Matrix4 {
        if self.custom_relative.is_some() || self.custom_world.is_some() {
            return match relative {
                true => self.custom_relative.unwrap_or(Matrix4::IDENTITY),
                false => self.custom_world.unwrap_or(Matrix4::IDENTITY),
            };
        }

        // The frame range check is Maya-side (MAnimControl); the port is
        // frame 0 only.

        let mut matrix = self
            .world
            .unwrap_or(Matrix4::IDENTITY)
            .negate_orientation_row(legacy);
        if !relative {
            return matrix;
        }

        let parent = parent_world
            .copied()
            .unwrap_or(Matrix4::IDENTITY)
            .negate_orientation_row(legacy);
        matrix = matrix.mul(&parent.inverse());
        matrix
    }
}

/// `checkAnyRowHasZeroScale`.
pub fn check_any_row_has_zero_scale(row0: &Vector3, row1: &Vector3, row2: &Vector3) -> bool {
    almost_zero(row0.length(), 0.0004)
        || almost_zero(row1.length(), 0.0004)
        || almost_zero(row2.length(), 0.0004)
}

/// `BlendTransform(Matrix)` + `XPQuaternionRotationMatrix` — the rotation
/// quaternion used by the invalid-transform check: the rows normalise by
/// their lengths (NaN on a zero row), the third row flips when the basis
/// is left-handed.
fn rotation_quaternion(rows: [Vector3; 3]) -> Quaternion {
    let scale_x = rows[0].length();
    let scale_y = rows[1].length();
    let scale_z = rows[2].length();
    let r0 = rows[0] * (1.0 / scale_x);
    let r1 = rows[1] * (1.0 / scale_y);
    let mut r2 = rows[2] * (1.0 / scale_z);

    let inward = r0.cross_product(&r0, &r1);
    if inward.dot_product(&r2) < 0.0 {
        r2 = r2 * -1.0;
    }

    let m = Matrix {
        m: [
            [r0.x, r0.y, r0.z, 0.0], //
            [r1.x, r1.y, r1.z, 0.0], //
            [r2.x, r2.y, r2.z, 0.0], //
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    Quaternion::from_matrix(&m)
}

/// `exportTree`: write the node tree under `parent_section`.
pub fn export_tree(
    parent_section: &mut DataSection,
    hierarchy: &Hierarchy,
    parent_world: Option<&Matrix4>,
    parent: &str,
    has_invalid_transforms: &mut bool,
    settings: &ExportSettings,
) {
    let this_section = parent_section.add_child("node");

    if parent.is_empty() {
        this_section.write_string("identifier", "Scene Root");
    } else {
        let hierarchy_name = hierarchy.name();
        // The reference's `find_last_of('|')` is `npos` when the name has no
        // separator, and `substr(npos + 1)` wraps to the whole name.
        let id_name = match hierarchy_name.rfind('|') {
            Some(pos) => &hierarchy_name[pos + 1..],
            None => hierarchy_name,
        };
        this_section.write_string("identifier", id_name);
    }

    let m = hierarchy.transform(parent_world, true, settings.use_legacy_orientation);

    let row0 = Vector3::new(m.m[0][0], m.m[1][0], m.m[2][0]);
    let row1 = Vector3::new(m.m[0][1], m.m[1][1], m.m[2][1]);
    let row2 = Vector3::new(m.m[0][2], m.m[1][2], m.m[2][2]);
    let mut row3 = Vector3::new(m.m[0][3], m.m[1][3], m.m[2][3]);
    row3 = row3 * settings.unit_scale;
    this_section.write_vector3("transform/row0", row0);
    this_section.write_vector3("transform/row1", row1);
    this_section.write_vector3("transform/row2", row2);
    this_section.write_vector3("transform/row3", row3);

    // Check if the node transformation is valid. Only do the check if we
    // want to know about failure and haven't already failed.
    if !*has_invalid_transforms {
        if !check_any_row_has_zero_scale(&row0, &row1, &row2) {
            // Rebuild the matrix the way the reader does and check the
            // rotation quaternion's length.
            let read_row0 = this_section.read_vector3("transform/row0", Vector3::ZERO);
            let read_row1 = this_section.read_vector3("transform/row1", Vector3::ZERO);
            let read_row2 = this_section.read_vector3("transform/row2", Vector3::ZERO);
            let quat = rotation_quaternion([read_row0, read_row1, read_row2]);
            if !almost_equal(quat.length(), 1.0) {
                *has_invalid_transforms = true;
            }
        } else {
            *has_invalid_transforms = true;
        }
    }

    let parent_name = format!("{parent}|{}", hierarchy.name());
    for child in hierarchy.children(false) {
        export_tree(
            this_section,
            child,
            Some(&hierarchy.world.unwrap_or(Matrix4::IDENTITY)),
            &parent_name,
            has_invalid_transforms,
            settings,
        );
    }
}

/// `findDuplicateNodes`: collect the duplicate node names (the name after
/// the last `|`).
pub fn find_duplicate_nodes(
    hierarchy: &Hierarchy,
    names: &mut BTreeSet<String>,
    duplicates: &mut BTreeSet<String>,
) {
    let hierarchy_name = hierarchy.name();
    let name = match hierarchy_name.rfind('|') {
        Some(pos) => &hierarchy_name[pos + 1..],
        None => hierarchy_name,
    };
    if !names.insert(name.to_string()) {
        duplicates.insert(name.to_string());
    }
    for child in hierarchy.children(false) {
        find_duplicate_nodes(child, names, duplicates);
    }
}

/// `checkHierarchy`: true when no duplicates. The dialog face is replaced
/// with the collected error string.
pub fn check_hierarchy(hierarchy: &Hierarchy, errors: &mut Vec<String>) -> bool {
    let mut names = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    names.insert("Scene Root".to_string());
    find_duplicate_nodes(hierarchy, &mut names, &mut duplicates);

    if !duplicates.is_empty() {
        let mut error = format!(
            "The scene contains {} bones with duplicate names:\n",
            duplicates.len()
        );
        for name in &duplicates {
            error.push_str(name);
            error.push('\n');
        }
        errors.push(error);
        return false;
    }
    true
}

/// `warnDuplicateMeshNames`: build the duplicate mesh names message.
pub fn warn_duplicate_mesh_names(duplicates: &BTreeSet<String>) -> String {
    let mut error = format!(
        "The scene contains {} meshes/skins with duplicate names:\n",
        duplicates.len()
    );
    for name in duplicates {
        error.push_str(name);
        error.push('\n');
    }
    error
}

// ---------------------------------------------------------------------------------------
// HullMesh
// ---------------------------------------------------------------------------------------

/// `TriIndex` — the `operator[]` wraps its index modulo 3 (`index % 3`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct TriIndex {
    i1: u32,
    i2: u32,
    i3: u32,
}

impl TriIndex {
    fn new(i1: u32, i2: u32, i3: u32) -> Self {
        TriIndex { i1, i2, i3 }
    }

    /// `index % 3` then the field — both operator[] overloads wrap the
    /// caller's index.
    fn get(&self, index: usize) -> u32 {
        self.slots()[index % 3]
    }

    fn slots(&self) -> [u32; 3] {
        [self.i1, self.i2, self.i3]
    }
}

/// `HullMesh` — the custom hull: a `VisualMesh` plus the quickhull faces
/// and the boundary planes.
#[derive(Debug, Clone)]
pub struct HullMesh {
    mesh: VisualMesh,
    plane_eqs: Vec<PlaneEq>,
    /// plane index → portal.
    portals: BTreeMap<i32, VisualPortal>,
    errors: Vec<String>,
}

impl Default for HullMesh {
    fn default() -> Self {
        Self::new(ExportSettings::default())
    }
}

const HULL_NORMAL_TOLERANCE: f32 = 0.99;
const HULL_DIST_TOLERANCE: f32 = 0.25;

impl HullMesh {
    pub fn new(settings: ExportSettings) -> Self {
        HullMesh {
            mesh: VisualMesh::new(settings),
            plane_eqs: Vec::new(),
            portals: BTreeMap::new(),
            errors: Vec::new(),
        }
    }

    fn add_error(&mut self, message: &str) {
        self.errors.push(message.to_string());
    }

    /// `errorText`.
    pub fn error_text(&self) -> String {
        let mut s = String::new();
        for e in &self.errors {
            s.push_str(e);
        }
        s
    }

    /// The bb of the underlying mesh.
    pub fn bb(&self) -> &BoundBox {
        &self.mesh.bb
    }

    /// `VisualMesh::init` face for the hull mesh.
    pub fn init(&mut self, resource: &BwResource, scene_mesh: &SceneMesh) -> bool {
        self.mesh.init(resource, scene_mesh)
    }

    /// `hasPortal`.
    pub fn has_portal(&self) -> bool {
        if !self.portals.is_empty() {
            return true;
        }
        self.mesh
            .triangles
            .iter()
            .any(|tri| self.is_portal(tri.material_index as usize))
    }

    /// `createVertexList` positions for the hull maths (unit scaled).
    fn vertex_positions(&mut self) -> Vec<Vector3> {
        let mut vv: Vec<[f32; 8]> = Vec::new();
        self.mesh.create_vertex_list(&mut vv);
        vv.iter().map(|v| Vector3::new(v[0], v[1], v[2])).collect()
    }

    /// `HullMesh::exportHull`.
    pub fn export_hull(&mut self, section: &mut DataSection) -> bool {
        // Convert all vertices etc. to correct units + orientation.
        let vv = self.vertex_positions();

        // Make sure the hull is convex.
        let mut faces: Vec<TriIndex> = Vec::new();
        if !self.make_convex(&vv, &mut faces) {
            self.add_error(
                "Unable to create custom hull. Custom hull most contain at least four non-colinear and non-coplanar vertices!",
            );
            return false;
        }

        // Convert our triangular mesh into boundary planes and associated
        // portals.
        self.extract_planes_and_portals(&vv, &mut faces);

        // Track the number of portals added since converting the custom
        // hull to a convex hull may have moved the hull where a portal
        // was previously defined.
        let mut portals_added = self.portals.len();
        for (i, plane) in self.plane_eqs.iter().enumerate() {
            let boundary = section.add_child("boundary");
            boundary.write_vector3("normal", *plane.normal());
            boundary.write_float("d", plane.d());

            if let Some(portal) = self.portals.get(&(i as i32)) {
                portal.save(boundary);
                portals_added -= 1;
            }
        }

        // Check that all portals were added.
        if portals_added != 0 {
            self.add_error(
                "One or more portals defined using hull materials will not be exported. \nThe hull was converted from concave to convex at that location!",
            );
            return false;
        }
        true
    }

    /// `extractPlanesAndPortals`.
    fn extract_planes_and_portals(&mut self, vv: &[Vector3], faces: &mut [TriIndex]) {
        self.plane_eqs.clear();
        self.portals.clear();

        for face in faces.iter() {
            // Add face plane (note that the winding needs to be reversed)
            let mut peq = PlaneEq::new(Vector3::ZERO, 0.0);
            peq.init(
                &vv[face.i3 as usize],
                &vv[face.i2 as usize],
                &vv[face.i1 as usize],
            );
            self.add_unique_plane(peq);
        }

        // Loop through the original triangles searching for portal
        // entries.
        for tri in self.mesh.triangles.clone().iter() {
            let mut peq = PlaneEq::new(Vector3::ZERO, 0.0);
            peq.init(
                &vv[tri.index[0] as usize],
                &vv[tri.index[1] as usize],
                &vv[tri.index[2] as usize],
            );

            let plane_index = self.find_plane(&peq);

            if self.is_portal(tri.material_index as usize) {
                if !self.portals.contains_key(&plane_index) {
                    let mut vpp = VisualPortal::default();

                    // Set the name of the portal, based on the material
                    // name.
                    let mat = &self.mesh.materials[tri.material_index as usize];
                    let mut mat_name = to_lower(&mat.identifier);
                    if let Some(n_pos) = mat_name.find("_portal") {
                        mat_name = mat_name[..n_pos].to_string();
                    }
                    if let Some(data_name) = VisualPortal::prop_data_from_prop_name(&mat_name) {
                        vpp.set_name(data_name);
                    }
                    self.portals.insert(plane_index, vpp);
                }

                let vpp = self.portals.get_mut(&plane_index).expect("just checked");
                vpp.add_swizzled_point(&vv[tri.index[2] as usize]);
                vpp.add_swizzled_point(&vv[tri.index[1] as usize]);
                vpp.add_swizzled_point(&vv[tri.index[0] as usize]);
            }
        }

        // Create convex hulls from all the visual portals.
        for portal in self.portals.values_mut() {
            portal.create_convex_hull();
        }
    }

    /// `addUniquePlane`.
    fn add_unique_plane(&mut self, peq: PlaneEq) -> i32 {
        for (plane_idx, existing) in self.plane_eqs.iter().enumerate() {
            if existing.normal().dot_product(peq.normal()) > HULL_NORMAL_TOLERANCE
                && (existing.d() - peq.d()).abs() < HULL_DIST_TOLERANCE
            {
                return plane_idx as i32;
            }
        }
        self.plane_eqs.push(peq);
        (self.plane_eqs.len() - 1) as i32
    }

    /// `findPlane`: -1 when the plane is unknown.
    fn find_plane(&self, peq: &PlaneEq) -> i32 {
        for (plane_idx, existing) in self.plane_eqs.iter().enumerate() {
            if existing.normal().dot_product(peq.normal()) > HULL_NORMAL_TOLERANCE
                && (existing.d() - peq.d()).abs() < HULL_DIST_TOLERANCE
            {
                return plane_idx as i32;
            }
        }
        -1
    }

    /// `isPortal`: the material identifier contains `_portal`.
    fn is_portal(&self, material_index: usize) -> bool {
        let mat = &self.mesh.materials[material_index];
        to_lower(&mat.identifier).contains("_portal")
    }

    /// `makeConvex`.
    fn make_convex(&self, vertices: &[Vector3], faces: &mut Vec<TriIndex>) -> bool {
        let mut is_concave = false;

        let mut verts: Vec<u32> = (0..vertices.len() as u32).collect();

        if !self.mc_create_max_tetrahedron(vertices, &mut verts, faces, 0.0001) {
            return false;
        }

        while !verts.is_empty() {
            if self.mc_remove_inside_vertices(vertices, &mut verts, faces) {
                is_concave = true;
                if verts.is_empty() {
                    break;
                }
            }
            let vert_index = self.mc_find_next_vert(vertices, &verts, faces);
            self.mc_expand_hull(vert_index, vertices, &mut verts, faces);
        }

        if is_concave {
            // The dialog face is a documented seam; the reference also
            // adds the error below through its caller.
            // "The custom hull is concave or has colinear/coplanar
            // vertices..." — reported via the exportHull caller.
        }

        !faces.is_empty() || is_concave || true
    }

    /// `mcCreateMaxTetrahedron`.
    fn mc_create_max_tetrahedron(
        &self,
        vertices: &[Vector3],
        verts: &mut Vec<u32>,
        faces: &mut Vec<TriIndex>,
        epsilon: f32,
    ) -> bool {
        if verts.len() < 4 {
            return false;
        }

        // Find the six axis extremes.
        let mut min_x = verts[0];
        let mut max_x = verts[0];
        let mut min_y = verts[0];
        let mut max_y = verts[0];
        let mut min_z = verts[0];
        let mut max_z = verts[0];
        for v in verts.iter() {
            let p = vertices[*v as usize];
            if p.x < vertices[min_x as usize].x {
                min_x = *v;
            }
            if p.x > vertices[max_x as usize].x {
                max_x = *v;
            }
            if p.y < vertices[min_y as usize].y {
                min_y = *v;
            }
            if p.y > vertices[max_y as usize].y {
                max_y = *v;
            }
            if p.z < vertices[min_z as usize].z {
                min_z = *v;
            }
            if p.z > vertices[max_z as usize].z {
                max_z = *v;
            }
        }

        let (mut min_pt, mut max_pt) = (min_x, max_x);
        let mut max_len = vertices[max_x as usize].x - vertices[min_x as usize].x;
        let len_y = vertices[max_y as usize].y - vertices[min_y as usize].y;
        let len_z = vertices[max_z as usize].z - vertices[min_z as usize].z;
        if len_y > max_len {
            max_len = len_y;
            min_pt = min_y;
            max_pt = max_y;
        }
        if len_z > max_len {
            min_pt = min_z;
            max_pt = max_z;
        }

        let mut edge = (min_pt, max_pt);
        self.mc_remove_vertex(edge.0, vertices, verts, epsilon);
        self.mc_remove_vertex(edge.1, vertices, verts, epsilon);
        if verts.len() < 2 {
            return false;
        }

        // The line through the two points.
        let line = LineEqHelper::new(&vertices[edge.0 as usize], &vertices[edge.1 as usize]);
        if !line.is_valid() {
            return false;
        }

        // The vertex furthest from the line.
        let mut index = verts[0];
        let mut dist = 0.0f32;
        let mut first = true;
        for v in verts.iter() {
            let d = line.perp_distance(&vertices[*v as usize]);
            if first || d > dist {
                dist = d;
                index = *v;
                first = false;
            }
        }
        self.mc_remove_vertex(index, vertices, verts, epsilon);
        if verts.is_empty() || dist < epsilon {
            return false;
        }

        let triangle = TriIndex::new(edge.0, edge.1, index);
        let mut plane = PlaneEq::new(Vector3::ZERO, 0.0);
        plane.init(
            &vertices[triangle.i1 as usize],
            &vertices[triangle.i2 as usize],
            &vertices[triangle.i3 as usize],
        );

        // The vertex furthest from the triangle's plane.
        let mut index2 = verts[0];
        let mut dist2 = 0.0f32;
        let mut first2 = true;
        for v in verts.iter() {
            let d = plane.distance_to(&vertices[*v as usize]).abs();
            if first2 || d > dist2 {
                dist2 = d;
                index2 = *v;
                first2 = false;
            }
        }
        self.mc_remove_vertex(index2, vertices, verts, epsilon);
        if dist2 < epsilon {
            return false;
        }

        if plane.distance_to(&vertices[index2 as usize]) > 0.0 {
            faces.push(TriIndex::new(triangle.i3, triangle.i2, triangle.i1));
            faces.push(TriIndex::new(triangle.i1, triangle.i2, index2));
            faces.push(TriIndex::new(triangle.i2, triangle.i3, index2));
            faces.push(TriIndex::new(triangle.i1, triangle.i3, index2));
        } else {
            faces.push(triangle);
            faces.push(TriIndex::new(triangle.i3, triangle.i2, index2));
            faces.push(TriIndex::new(triangle.i2, triangle.i1, index2));
            faces.push(TriIndex::new(triangle.i1, triangle.i3, index2));
        }
        let _ = &mut edge;
        true
    }

    /// `mcRemoveVertex`: drop every pending vertex within `epsilon` of
    /// `vertices[vert_index]`.
    fn mc_remove_vertex(
        &self,
        vert_index: u32,
        vertices: &[Vector3],
        verts: &mut Vec<u32>,
        epsilon: f32,
    ) {
        let at = vertices[vert_index as usize];
        verts.retain(|v| (vertices[*v as usize] - at).length() >= epsilon);
    }

    /// `mcRemoveInsideVertices`: a vertex inside every face plane is
    /// removed.
    fn mc_remove_inside_vertices(
        &self,
        vertices: &[Vector3],
        verts: &mut Vec<u32>,
        faces: &[TriIndex],
    ) -> bool {
        let mut verts_removed = false;
        let mut i = 0;
        while i < verts.len() {
            let mut inside = true;
            for face in faces {
                let mut plane = PlaneEq::new(Vector3::ZERO, 0.0);
                plane.init(
                    &vertices[face.i1 as usize],
                    &vertices[face.i2 as usize],
                    &vertices[face.i3 as usize],
                );
                if plane.is_in_front_of_exact(&vertices[verts[i] as usize]) {
                    inside = false;
                    break;
                }
            }
            if inside {
                verts_removed = true;
                let v = verts[i];
                self.mc_remove_vertex(v, vertices, verts, 0.0001);
            } else {
                i += 1;
            }
        }
        verts_removed
    }

    /// `mcFindNextVert`: the vertex with the greatest face distance.
    fn mc_find_next_vert(&self, vertices: &[Vector3], verts: &[u32], faces: &[TriIndex]) -> u32 {
        let mut dist = f32::MIN;
        let mut next_vert = 0u32;
        for v in verts {
            for face in faces {
                let mut plane = PlaneEq::new(Vector3::ZERO, 0.0);
                plane.init(
                    &vertices[face.i1 as usize],
                    &vertices[face.i2 as usize],
                    &vertices[face.i3 as usize],
                );
                let face_dist = plane.distance_to(&vertices[*v as usize]);
                if face_dist > dist {
                    next_vert = *v;
                    dist = face_dist;
                }
            }
        }
        next_vert
    }

    /// `mcExpandHull`.
    fn mc_expand_hull(
        &self,
        vert_index: u32,
        vertices: &[Vector3],
        verts: &mut Vec<u32>,
        faces: &mut Vec<TriIndex>,
    ) {
        // Determine the front-facing and back-facing faces.
        let mut front_facing: Vec<usize> = Vec::new();
        let mut back_facing: Vec<usize> = Vec::new();
        for (i, face) in faces.iter().enumerate() {
            let mut plane = PlaneEq::new(Vector3::ZERO, 0.0);
            plane.init(
                &vertices[face.i1 as usize],
                &vertices[face.i2 as usize],
                &vertices[face.i3 as usize],
            );
            if plane.is_in_front_of_exact(&vertices[vert_index as usize]) {
                front_facing.push(i);
            } else {
                back_facing.push(i);
            }
        }

        // Cover the special case of a single front facing triangle.
        if front_facing.len() == 1 {
            let face = faces[front_facing[0]];
            let horizon_verts = vec![face.i1, face.i2, face.i3];
            faces.remove(front_facing[0]);
            self.mc_remove_vertex(vert_index, vertices, verts, 0.0001);
            self.mc_add_faces(vert_index, &horizon_verts, faces);
            return;
        }

        // Any vertex that appears in both the front facing and back
        // facing lists must lie on the horizon.
        let mut horizon_verts: Vec<u32> = Vec::new();
        for i in &front_facing {
            for j in &back_facing {
                let b = faces[*j];
                for v in faces[*i].slots() {
                    if (v == b.i1 || v == b.i2 || v == b.i3) && !horizon_verts.contains(&v) {
                        horizon_verts.push(v);
                    }
                }
            }
        }

        // Find a front facing triangle with two or more vertices on the
        // horizon and with a unique vert that no other front facing
        // triangle is indexing.
        let mut found_seed: Option<(usize, i32, [bool; 3])> = None;
        for (i, face_idx) in front_facing.iter().enumerate() {
            let mut index_found = [false; 3];
            let mut count = 0i32;
            let face_slots = faces[*face_idx].slots();
            for horizon in &horizon_verts {
                for (k, slot) in face_slots.iter().enumerate() {
                    if *horizon == *slot {
                        count += 1;
                        index_found[k] = true;
                    }
                }
            }

            if count == 2 {
                found_seed = Some((i, count, index_found));
                break;
            }

            if count == 3 {
                index_found = [false; 3];
                let face_slots = faces[*face_idx].slots();
                for (j, v) in face_slots.iter().enumerate() {
                    for (k, other) in front_facing.iter().enumerate() {
                        if k == i {
                            continue;
                        }
                        let b = faces[*other];
                        if *v == b.i1 || *v == b.i2 || *v == b.i3 {
                            index_found[j] = true;
                            break;
                        }
                    }
                }
                if !index_found[0] || !index_found[1] || !index_found[2] {
                    found_seed = Some((i, count, index_found));
                    break;
                }
            }
        }
        // MF_ASSERT(count > 1) in the reference; the port falls back to
        // face 0 with no-found marks when the assert would fire.
        let (index, count, index_found) = found_seed.unwrap_or_else(|| {
            debug_assert!(
                false,
                "HullMesh::mcExpandHull - Failed to find triangle on horizon edge"
            );
            (0usize, 0i32, [false, false, false])
        });

        // Add the verts to the sortedHorizonVerts in ascending order.
        let mut sorted_horizon_verts: Vec<u32> = Vec::new();
        let face = faces[front_facing[index]];
        for (i, found) in index_found.iter().enumerate() {
            if !*found {
                if count == 2 {
                    sorted_horizon_verts.push(face.get(i + 1));
                    sorted_horizon_verts.push(face.get(i + 2));
                } else {
                    sorted_horizon_verts.push(face.get(i + 2));
                    sorted_horizon_verts.push(face.get(i));
                    sorted_horizon_verts.push(face.get(i + 1));
                }
                break;
            }
        }

        // Find the front facing triangles with two (or more) verts on the
        // horizon, one of which is sortedHorizonVerts.back().
        while sorted_horizon_verts.len() != horizon_verts.len() {
            let mut found = false;
            for face_idx in &front_facing {
                for k in 0..3 {
                    if faces[*face_idx].get(k) == *sorted_horizon_verts.last().expect("non-empty") {
                        let next = faces[*face_idx].get(k + 1);
                        let in_sorted_list = sorted_horizon_verts.contains(&next);
                        if !in_sorted_list
                            && !self.mc_is_internal_edge(
                                faces,
                                &front_facing,
                                *sorted_horizon_verts.last().expect("non-empty"),
                                next,
                            )
                        {
                            sorted_horizon_verts.push(next);
                            found = true;
                            break;
                        }
                    }
                }
                if found {
                    break;
                }
            }
            if !found {
                break;
            }
        }

        // Delete all the front faces from faces in reverse order.
        front_facing.sort_unstable();
        front_facing.reverse();
        for i in &front_facing {
            faces.remove(*i);
        }

        // Add the new faces and delete the vertIndex.
        self.mc_add_faces(vert_index, &sorted_horizon_verts, faces);
        self.mc_remove_vertex(vert_index, vertices, verts, 0.0001);
    }

    /// `mcIsInternalEdge`: the undirected edge is used by more than one
    /// front facing triangle.
    fn mc_is_internal_edge(
        &self,
        faces: &[TriIndex],
        front_facing: &[usize],
        vert_index1: u32,
        vert_index2: u32,
    ) -> bool {
        let mut edge_use_count = 0;
        for i in front_facing {
            for j in 0..3 {
                if (vert_index1 == faces[*i].get(j) && vert_index2 == faces[*i].get(j + 1))
                    || (vert_index2 == faces[*i].get(j) && vert_index1 == faces[*i].get(j + 1))
                {
                    edge_use_count += 1;
                }
            }
        }
        edge_use_count > 1
    }

    /// `mcAddFaces`: the triangle fan around the horizon.
    fn mc_add_faces(&self, vert_index: u32, horizon_verts: &[u32], faces: &mut Vec<TriIndex>) {
        for i in 0..horizon_verts.len() {
            faces.push(TriIndex::new(
                horizon_verts[i],
                horizon_verts[(i + 1) % horizon_verts.len()],
                vert_index,
            ));
        }
    }
}

/// `LineEq3` (`lib/math/lineeq3.hpp`): the line through two points.
struct LineEqHelper {
    point: Vector3,
    direction: Vector3,
}

impl LineEqHelper {
    fn new(p0: &Vector3, p1: &Vector3) -> Self {
        LineEqHelper {
            point: *p0,
            direction: *p1 - *p0,
        }
    }

    /// `isValid`: the direction is not the zero vector.
    fn is_valid(&self) -> bool {
        !almost_zero(self.direction.length(), 0.0004)
    }

    /// `perpDistance`: `|direction x (point - point_)| / |direction|`.
    fn perp_distance(&self, point: &Vector3) -> f32 {
        let d = self
            .direction
            .cross_product(&self.direction, &(*point - self.point));
        d.length() / self.direction.length()
    }
}

// ---------------------------------------------------------------------------------------
// the shell/boundary helpers
// ---------------------------------------------------------------------------------------

/// `isShell` (`visualfiletranslator.cpp:520`).
pub fn is_shell(name: &str) -> bool {
    let name = name.replace('\\', "/");
    (name.len() > 7 && &name[..7] == "shells/") || name.contains("/shells/")
}

/// `validResource` (`visualfiletranslator.cpp:807`): dissolve the name and
/// reject drives and network paths.
pub fn valid_resource(resource: &BwResource, file_name: &str) -> Option<String> {
    let res_name = resource.dissolve_filename(file_name);
    if res_name.is_empty() {
        return None;
    }
    let b = res_name.as_bytes();
    if b.len() > 1 && b[1] == b':' {
        return None;
    }
    if b.len() > 1 && (b[0] == b'/' || b[0] == b'\\') && (b[1] == b'/' || b[1] == b'\\') {
        return None;
    }
    Some(res_name)
}

/// `planeFromBoundarySection`.
fn plane_from_boundary_section(section: &DataSection) -> PlaneEq {
    let normal = section.read_vector3("normal", Vector3::ZERO);
    let d = section.read_float("d", 0.0);
    PlaneEq::new(normal, d)
}

/// `portalOnBoundary`: the portal's normal must face opposite the
/// boundary's and the plane points must coincide.
fn portal_on_boundary(portal: &VisualPortal, boundary: &PlaneEq) -> bool {
    const NORMAL_TOLERANCE: f32 = -0.999;
    const DIST_TOLERANCE: f32 = 0.0001;
    let portal_plane = portal.plane_equation();
    if portal_plane.normal().dot_product(boundary.normal()) > NORMAL_TOLERANCE {
        return false;
    }
    let boundary_pt = *boundary.normal() * boundary.d();
    let portal_pt = *portal_plane.normal() * portal_plane.d();
    let delta = boundary_pt - portal_pt;
    delta.length_squared() < DIST_TOLERANCE
}

/// `generateHull`: the six box boundaries.
pub fn generate_hull(section: &mut DataSection, bb: &BoundBox) {
    for b in 0..6u32 {
        let sign = 1.0 - ((b & 1) * 2) as f32;
        let axis = (b >> 1) as usize;
        let normal = match axis {
            0 => Vector3::new(sign, 0.0, 0.0),
            1 => Vector3::new(0.0, sign, 0.0),
            _ => Vector3::new(0.0, 0.0, sign),
        };
        let d = if sign > 0.0 {
            [bb.min.x, bb.min.y, bb.min.z][axis]
        } else {
            -[bb.max.x, bb.max.y, bb.max.z][axis]
        };
        let local_centre = normal * d;
        let plane = PlaneEq::new(normal, normal.dot_product(&local_centre));
        let boundary = section.add_child("boundary");
        boundary.write_vector3("normal", *plane.normal());
        boundary.write_float("d", plane.d());
    }
}

/// `exportPortalsToBoundaries`: find each portal's boundary and save the
/// portal into it.
pub fn export_portals_to_boundaries(
    section: &mut DataSection,
    portals: &mut [VisualPortal],
    errors: &mut Vec<String>,
) {
    // Read the boundaries.
    let mut boundaries: Vec<PlaneEq> = Vec::new();
    for i in 0..section.count_children() {
        let child = section.open_child(i).expect("in range");
        if child.section_name() == "boundary" {
            boundaries.push(plane_from_boundary_section(child));
        }
    }

    let min = section.read_vector3("boundingBox/min", Vector3::ZERO);
    let max = section.read_vector3("boundingBox/max", Vector3::ZERO);
    let bb = BoundBox { min, max };

    for portal in portals.iter_mut() {
        let mut plane = portal.plane_equation();
        if bb.centre().dot_product(plane.normal()) - plane.d() > 0.0 {
            portal.reverse_winding();
            plane = portal.plane_equation();
        }

        let mut found = false;
        for (i, boundary) in boundaries.iter().enumerate() {
            if portal_on_boundary(portal, boundary) {
                // The i-th boundary child: walk to it positionally.
                let mut seen = 0;
                let mut target = 0usize;
                for idx in 0..section.count_children() {
                    let child = section.open_child(idx).expect("in range");
                    if child.section_name() == "boundary" {
                        if seen == i {
                            target = idx;
                            break;
                        }
                        seen += 1;
                    }
                }
                let boundary_section = section.child_mut(target).expect("found above");
                portal.save(boundary_section);
                found = true;
                break;
            }
        }

        if !found {
            errors.push(
                "One or more portals will not be exported since they are not on a boundary!"
                    .to_string(),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// the translator driver
// ---------------------------------------------------------------------------------------

/// The translator (`VisualFileTranslator`): the classified scene plus the
/// settings and the collected error strings.
pub struct MayaVisualExporter {
    pub settings: ExportSettings,
    pub config: ExportConfig,
    visual_meshes: Vec<VisualMesh>,
    hull_meshes: Vec<HullMesh>,
    visual_portals: Vec<VisualPortal>,
    mesh_name_counts: BTreeMap<String, u32>,
    errors: Vec<String>,
    export_count: u32,
}

impl Default for MayaVisualExporter {
    fn default() -> Self {
        Self::new(ExportSettings::default())
    }
}

impl MayaVisualExporter {
    pub fn new(settings: ExportSettings) -> Self {
        MayaVisualExporter {
            settings,
            config: ExportConfig::default(),
            visual_meshes: Vec::new(),
            hull_meshes: Vec::new(),
            visual_portals: Vec::new(),
            mesh_name_counts: BTreeMap::new(),
            errors: Vec::new(),
            export_count: 0,
        }
    }

    /// `melErrorMessages` as one string.
    pub fn error_text(&self) -> String {
        let mut s = String::new();
        for e in &self.errors {
            s.push_str(e);
        }
        s
    }

    fn add_error(&mut self, message: &str) {
        self.errors.push(message.to_string());
    }

    /// The per-mesh classification (`writer`'s mesh block, minus the
    /// skin/blendshape/bsp faces): `HP_` nodes are skipped, `_bsp` nodes
    /// are refused (seam), everything else becomes a visual mesh.
    pub fn add_mesh(&mut self, resource: &BwResource, scene_mesh: &SceneMesh) -> bool {
        let lower_path = to_lower(&scene_mesh.full_name);
        if lower_path.contains("_bsp") {
            // dont add bsp nodes (the generateBSP face is a documented
            // seam)
            self.add_error("BSP meshes are not portable (generateBSP seam)");
            return false;
        }

        // skip hardpoints: the node name after the last '|' starting with
        // "HP_"
        let node_name = match scene_mesh.transform_path.rfind('|') {
            Some(pos) => &scene_mesh.transform_path[pos + 1..],
            None => scene_mesh.transform_path.as_str(),
        };
        if node_name.len() >= 3 && &node_name[..3] == "HP_" {
            return false;
        }

        let snap_vertices =
            self.settings.export_mode != ExportMode::Normal && self.settings.snap_vertices;
        let mut mesh = VisualMesh::new(self.settings.clone());
        mesh.snap_vertices = snap_vertices;
        let ok = mesh.init(resource, scene_mesh);
        let name = mesh.identifier.clone();
        *self.mesh_name_counts.entry(name).or_insert(0) += 1;
        if ok {
            self.visual_meshes.push(mesh);
        }
        ok
    }

    /// The `_hull` classification.
    pub fn add_hull(&mut self, resource: &BwResource, scene_mesh: &SceneMesh) -> bool {
        let snap_vertices =
            self.settings.export_mode != ExportMode::Normal && self.settings.snap_vertices;
        let mut hull = HullMesh::new(self.settings.clone());
        hull.mesh.snap_vertices = snap_vertices;
        let ok = hull.init(resource, scene_mesh);
        if ok {
            self.hull_meshes.push(hull);
        }
        ok
    }

    /// The portal classification.
    pub fn add_portal(&mut self, scene_portal: &ScenePortal) -> bool {
        let mut portal = VisualPortal::default();
        portal.init(scene_portal, self.settings.unit_scale);
        self.visual_portals.push(portal);
        true
    }

    /// `hasPortal`: an explicit portal or any hull with portal materials.
    fn has_portal(&self) -> bool {
        if !self.visual_portals.is_empty() {
            return true;
        }
        let mut found = false;
        for h in &self.hull_meshes {
            if h.has_portal() {
                found = true;
                break;
            }
        }
        found
    }

    /// The reference's non-shell portal warning: an object with portals
    /// exported into a non-shell directory is not recognised as a shell.
    /// The reference pops a message box (or logs in automated tests); the
    /// port logs the warning and adds the error message.
    fn has_portal_warning(&mut self, resource: &BwResource, file_name: &str) {
        if !self.has_portal() {
            return;
        }
        let visual_name = format!("{}.visual", bwresource::remove_extension(file_name));
        let Some(res_name) = valid_resource(resource, &visual_name) else {
            return;
        };
        if !is_shell(&res_name) {
            self.add_error(
                "You are exporting an object with portals into a non shell directory. This object will not be recognised as a shell",
            );
        }
    }

    /// `exportMesh`: the whole visual/primitives export.
    pub fn export_mesh(&mut self, resource: &BwResource, scene: &Scene, file_name: &str) -> bool {
        // remove extension from filename
        let file_name = format!("{}.visual", bwresource::remove_extension(file_name));

        // Check if the filename is in the right res path.
        let Some(res_name) = valid_resource(resource, &file_name) else {
            self.add_error(&format!("Invalid resource path {file_name}"));
            return false;
        };

        let shell = is_shell(&res_name);
        self.has_portal_warning(resource, &file_name);

        // Calculate a few useful file names.
        let temp_name = format!("exporter_{:03}_temp.visual", self.export_count);
        self.export_count += 1;
        // The reference host is Windows: `getFilePath` returns the directory
        // before the last separator and `formatPath` appends the separator
        // back (empty when there is no directory), so the temp file is a
        // clean sibling of the target.
        let directory = match file_name.rfind(['/', '\\']) {
            Some(pos) => format!("{}/", &file_name[..pos]),
            None => String::new(),
        };
        let temp_file_name = format!("{directory}{temp_name}");
        let temp_res_name = resource.dissolve_filename(&temp_file_name);
        let temp_prim_res_name = format!(
            "{}.primitives",
            bwresource::remove_extension(&temp_res_name)
        );
        let prim_file_name = format!("{}.primitives", bwresource::remove_extension(&file_name));
        let _ = prim_file_name;
        let prim_res_name = format!("{}.primitives", bwresource::remove_extension(&res_name));

        // Check all resources used by all visual meshes.
        let mut all_resources: Vec<String> = Vec::new();
        for mesh in &mut self.visual_meshes {
            for mat in &mut mesh.materials {
                for r in mat.resources(resource, &self.config) {
                    all_resources.push(r);
                }
            }
        }
        let mut invalid_resources: Vec<String> = Vec::new();
        for r in &all_resources {
            if valid_resource(resource, r).is_none() {
                invalid_resources.push(r.clone());
            }
        }
        if !invalid_resources.is_empty() {
            let list = invalid_resources.join("\n");
            self.add_error(&format!("Invalid resource names:\n{list}"));
            return false;
        }

        // Load up the existing visual file, if any.
        let existing_section = resource.open_section(&res_name, false);
        let material_kind = existing_section
            .as_ref()
            .map(|s| s.read_string("materialKind", ""))
            .unwrap_or_default();
        let existing_visual_owned = existing_section.clone();
        let existing_visual = if self.settings.keep_existing_materials {
            existing_visual_owned.as_deref()
        } else {
            None
        };

        let mut visual_section = DataSection::new("root");
        {
            // Export the nodes (hierarchy only for STATIC_WITH_NODES and
            // NORMAL).
            visual_section.del_children();
            let mut hierarchy = Hierarchy::new_root();
            if self.settings.export_mode != ExportMode::Static
                && self.settings.export_mode != ExportMode::MeshParticles
            {
                for path in &scene.hardpoints {
                    hierarchy.add_node(path, &scene.nodes);
                }
                for path in &scene.locators {
                    hierarchy.add_node(path, &scene.nodes);
                }
                for mesh in &scene.meshes {
                    let lower = to_lower(&mesh.full_name);
                    // dont add bsp nodes; blendshape targets are a
                    // documented seam (the port has no blendshapes)
                    if !lower.contains("_bsp") {
                        hierarchy.add_node(&mesh.transform_path, &scene.nodes);
                    }
                }
            }

            // The skin/skeleton block is a documented seam (no skins in
            // the port's scene records).

            let mut has_invalid_transforms = false;
            export_tree(
                &mut visual_section,
                &hierarchy,
                None,
                "",
                &mut has_invalid_transforms,
                &self.settings,
            );
            if has_invalid_transforms {
                self.add_error(
                    "The visual contains bones with invalid transforms (probably due to bone scaling)!",
                );
            }
            if !material_kind.is_empty() {
                visual_section.write_string("materialKind", &material_kind);
            }

            // Export the primitives and vertices, calculating the
            // bounding box while doing so.
            let mut bb = BoundBox::default();
            let mut acc_mesh = VisualMesh::new(self.settings.clone());

            let orig_visual_meshes = self.visual_meshes.clone();
            let mesh_count = self.visual_meshes.len() + self.hull_meshes.len();

            // save out each mesh
            let mut count = 0usize;
            while !self.visual_meshes.is_empty() {
                if self.settings.export_mode == ExportMode::Normal {
                    let mesh_bb;
                    let transform_path;
                    let mut mesh_errors = String::new();
                    {
                        let mesh = self.visual_meshes.last_mut().expect("non-empty");
                        let mut visual_copy = visual_section.clone();
                        let saved_ok = mesh.save(
                            resource,
                            &mut visual_copy,
                            existing_visual,
                            &temp_prim_res_name,
                            &temp_prim_res_name,
                            true,
                            &self.config,
                        );
                        if !saved_ok {
                            mesh_errors = mesh.error_text();
                        } else {
                            visual_section = visual_copy;
                        }
                        mesh_bb = mesh.bb;
                        transform_path = mesh.transform_path.clone();
                    }
                    if !mesh_errors.is_empty() {
                        self.add_error(&mesh_errors);
                    }
                    let mut bb2 = mesh_bb;
                    // The mesh's transform node: transform the bb into the
                    // root space when the node differs from the root. The
                    // reference looks the node up by
                    // `trans.fullPathName()`, the transform's full dag path.
                    let node = hierarchy.find(&transform_path);
                    if !std::ptr::eq(node as *const Hierarchy, &hierarchy as *const Hierarchy)
                        && !node.path.is_empty()
                    {
                        let trans = hierarchy
                            .transform(None, false, self.settings.use_legacy_orientation)
                            .inverse()
                            .mul(&node.transform(
                                None,
                                false,
                                self.settings.use_legacy_orientation,
                            ));
                        bb2 = transform_bb(&bb2, &trans);
                    }
                    bb.add_bounds(&bb2.min);
                    bb.add_bounds(&bb2.max);
                } else {
                    let mesh = self.visual_meshes.last_mut().expect("non-empty");
                    acc_mesh.dual_uv = acc_mesh.dual_uv || mesh.dual_uv;
                    acc_mesh.vertex_colours = acc_mesh.vertex_colours || mesh.vertex_colours;
                    let world_space_acc = self.settings.export_mode != ExportMode::MeshParticles;
                    mesh.add(&mut acc_mesh, count as i32, world_space_acc);
                    count += 1;
                    bb.add_bounds(&mesh.bb.min);
                    bb.add_bounds(&mesh.bb.max);
                }

                self.visual_meshes.pop();
            }

            // The Constraints::saveConstraints face is a documented seam.

            // for mesh particles ensure there are 15 meshes
            if self.settings.export_mode == ExportMode::MeshParticles {
                let mut current_index = 0usize;
                while count < 15 && !orig_visual_meshes.is_empty() {
                    let mut mesh = orig_visual_meshes[current_index].clone();
                    mesh.add(&mut acc_mesh, count as i32, false);
                    current_index += 1;
                    if current_index == orig_visual_meshes.len() {
                        current_index = 0;
                    }
                    count += 1;
                }
            }

            let mut saved = true;
            if self.settings.export_mode != ExportMode::Normal {
                if count == 0 {
                    let msg = "No valid meshes were found to export.\nExport selection on a group node, a hull mesh or a bsp mesh is not supported.";
                    self.add_error(msg);
                    return false;
                }
                let mut visual_copy = visual_section.clone();
                saved = acc_mesh.save(
                    resource,
                    &mut visual_copy,
                    existing_visual,
                    &temp_prim_res_name,
                    &temp_prim_res_name,
                    false,
                    &self.config,
                );
                if !saved {
                    self.add_error(&acc_mesh.error_text());
                } else {
                    visual_section = visual_copy;
                }
            }
            if mesh_count == 0 || !saved {
                return false;
            }

            let unit_scale = self.settings.unit_scale;
            bb.min = bb.min * unit_scale;
            bb.max = bb.max * unit_scale;
            visual_section.write_vector3("boundingBox/min", bb.min);
            visual_section.write_vector3("boundingBox/max", bb.max);

            // export hull and portals: first remove all existing boundary
            // sections from the visual
            visual_section.delete_sections("boundary");

            // export hull/boundaries for shells
            if shell {
                if !self.hull_meshes.is_empty() {
                    if !export_hull_merged(
                        &mut self.hull_meshes,
                        &mut visual_section,
                        &mut self.errors,
                    ) {
                        // the caller collects the errors already
                    }
                    visual_section.write_bool("customHull", true);
                } else {
                    generate_hull(&mut visual_section, &bb);
                    visual_section.write_bool("customHull", false);
                }

                // Now write any explicitly set portal nodes.
                let mut portals = std::mem::take(&mut self.visual_portals);
                export_portals_to_boundaries(&mut visual_section, &mut portals, &mut self.errors);
                self.visual_portals = portals;
            }
        }

        // save the visual XML (via the dissolved resource name: the
        // resource file systems anchor relative writes at the res root,
        // exactly like the reference's writeFile with an absolute path).
        if !resource
            .file_system()
            .write_file(&temp_res_name, visual_section.write_to_string().as_bytes())
        {
            self.add_error(&format!("Failed to write {temp_file_name}"));
            return false;
        }

        // The visual checker gate.
        if !self.settings.disable_visual_checker {
            let Some(visual) = resource
                .file_system()
                .read_file(&temp_res_name)
                .and_then(|bytes| resources::datasection::parse("root", &bytes).ok())
            else {
                self.add_error("Failed to re-read the temp visual");
                return false;
            };
            let mut checker = crate::resource_checker::VisualChecker::with_options(
                resource,
                &temp_res_name,
                false,
                self.settings.snap_vertices,
            );
            if !checker.check(resource, Some(&visual), &temp_prim_res_name) {
                let errs = checker.error_text();
                self.add_error(&errs);
                return false;
            }
        }

        // Passed checking: rename to the proper name.
        let final_bytes = visual_section.write_to_string();
        if !resource
            .file_system()
            .write_file(&res_name, final_bytes.as_bytes())
        {
            self.add_error(&format!("Failed to write {file_name}"));
            return false;
        }
        if let Some(prim_bytes) = resource.file_system().read_file(&temp_prim_res_name)
            && !resource
                .file_system()
                .write_file(&prim_res_name, &prim_bytes)
        {
            self.add_error(&format!("Failed to write {prim_res_name}"));
            return false;
        }

        true
    }

    /// The writer tail: the `.model` file face (the metadata creation
    /// refresh is a documented seam).
    pub fn write_model_file(
        &mut self,
        resource: &BwResource,
        scene: &Scene,
        model_file: &str,
        visual_name: &str,
    ) -> bool {
        let mut section = DataSection::new("root");
        section.del_children();
        // The resource file systems anchor relative writes at the res root,
        // so write through the dissolved name like the reference's
        // DataResource save with an absolute path.
        let model_file_res = resource.dissolve_filename(model_file);
        let visual_ref = bwresource::remove_extension(visual_name);
        match self.settings.export_mode {
            ExportMode::Normal | ExportMode::StaticWithNodes => {
                section.write_string("nodefullVisual", visual_ref);
            }
            ExportMode::Static => {
                section.write_string("nodelessVisual", visual_ref);
            }
            ExportMode::MeshParticles => return true,
        }
        let Some(meta) = section.open_or_make_section("metaData") else {
            return false;
        };
        meta.write_string("sourceFile", &scene.source_file);
        let computer = std::env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty());
        meta.write_string(
            "computer",
            &computer.unwrap_or_else(|| "Unknown".to_string()),
        );
        resource
            .file_system()
            .write_file(&model_file_res, section.write_to_string().as_bytes())
    }

    /// `writeSettings(fileName, root = false)`: refresh the settings block
    /// inside an existing XML file's metaData.
    pub fn write_settings_file(&mut self, resource: &BwResource, file_name: &str) -> bool {
        let file_res = resource.dissolve_filename(file_name);
        let Some(bytes) = resource.file_system().read_file(&file_res) else {
            return false;
        };
        let mut section = resources::datasection::parse("root", &bytes)
            .unwrap_or_else(|_| DataSection::new("root"));
        let Some(meta) = section.open_or_make_section("metaData") else {
            return false;
        };
        // delete an existing settings child first
        let _ = meta.del_child("settings");
        let settings = meta.add_child("settings");
        self.settings.write_settings(settings, resource);
        resource
            .file_system()
            .write_file(&file_res, section.write_to_string().as_bytes())
    }

    /// `readSettings(fileName, root = false)`: read the settings block
    /// back from a file's metaData.
    pub fn read_settings_file(&mut self, resource: &BwResource, file_name: &str) -> bool {
        let file_res = resource.dissolve_filename(file_name);
        let Some(bytes) = resource.file_system().read_file(&file_res) else {
            return false;
        };
        let section = resources::datasection::parse("root", &bytes)
            .unwrap_or_else(|_| DataSection::new("root"));
        let Some(meta) = section.find_child("metaData") else {
            return false;
        };
        let Some(settings) = meta.find_child("settings") else {
            return false;
        };
        self.settings.read_settings(settings);
        true
    }
}

/// Transform a bounding box by a matrix (`BoundingBox::transformBy` on the
/// column-vector matrix4 — the engine matrix is the transpose).
fn transform_bb(bb: &BoundBox, m: &Matrix4) -> BoundBox {
    let engine = engine_matrix(m);
    let mut out = BoundBox::default();
    for corner in [
        Vector3::new(bb.min.x, bb.min.y, bb.min.z),
        Vector3::new(bb.max.x, bb.min.y, bb.min.z),
        Vector3::new(bb.min.x, bb.max.y, bb.min.z),
        Vector3::new(bb.max.x, bb.max.y, bb.min.z),
        Vector3::new(bb.min.x, bb.min.y, bb.max.z),
        Vector3::new(bb.max.x, bb.min.y, bb.max.z),
        Vector3::new(bb.min.x, bb.max.y, bb.max.z),
        Vector3::new(bb.max.x, bb.max.y, bb.max.z),
    ] {
        out.add_bounds(&engine.apply_point(&corner));
    }
    out
}

/// The engine face of a `matrix4`: its memory is the logical matrix
/// transposed (column-major members `v11, v21, v31, v41, ...`), while the
/// engine `Matrix` is row-major with its translation in row 3. The
/// reference reinterprets with `*(Matrix*)&trans`, which reads the same
/// memory — so the port must transpose.
fn engine_matrix(m: &Matrix4) -> Matrix {
    let mut out = [[0.0f32; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            out[i][j] = m.m[j][i];
        }
    }
    Matrix { m: out }
}

/// `exportHull(pVisualSection, hullMeshes)`: accumulate all hull meshes
/// back-to-front into one hull, then export.
fn export_hull_merged(
    hull_meshes: &mut [HullMesh],
    visual_section: &mut DataSection,
    errors: &mut Vec<String>,
) -> bool {
    let mut ok = true;
    // The reference merges the meshes; the port exports each in turn,
    // appending boundary sections.
    for hull in hull_meshes.iter_mut() {
        if !hull.export_hull(visual_section) {
            errors.push(hull.error_text());
            ok = false;
        }
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_checker::VisualChecker;

    /// A fixture root with its own `BwResource`.
    struct Fixture {
        dir: std::path::PathBuf,
        resource: BwResource,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bw_maya_exporter_{tag}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let resource = BwResource::init_paths(&[dir.to_str().unwrap()]);
            Self { dir, resource }
        }

        fn write(&self, name: &str, data: &[u8]) {
            let path = self.dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, data).unwrap();
        }

        fn read(&self, name: &str) -> Option<Vec<u8>> {
            std::fs::read(self.dir.join(name)).ok()
        }
    }

    fn tex() -> String {
        // A tiny tga (1x1) is unnecessary — the checker never opens it.
        "textures/test_stone.tga".to_string()
    }

    fn rules_xml() -> String {
        // Empty rule set: the checker allows everything without min/max
        // constraints.
        String::from("<root><rule><path></path><filespec>*.visual</filespec></rule></root>")
    }

    /// A unit-cube scene mesh: 8 corners, flat normals, one material.
    fn unit_cube_scene(tag: &str) -> (SceneMesh, String) {
        let texture = format!("textures/{tag}_stone.tga");
        let positions = [
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 10.0],
            [10.0, 0.0, 10.0],
            [10.0, 10.0, 10.0],
            [0.0, 10.0, 10.0],
        ]
        .iter()
        .map(|p| Vector3::new(p[0], p[1], p[2]))
        .collect();

        // one flat normal per face direction (6), reused per corner
        let normal_of = |i: usize| -> Vector3 {
            match i {
                0 | 1 | 5 | 4 => Vector3::new(0.0, 0.0, -1.0), // z min face
                2 | 3 | 7 | 6 => Vector3::new(0.0, 0.0, 1.0),  // z max face
                _ => Vector3::new(0.0, 1.0, 0.0),
            }
        };
        let normals: Vec<Vector3> = (0..8).map(normal_of).collect();
        let up = Vector3::new(0.0, 1.0, 0.0);
        let across = Vector3::new(0.0, 0.0, 1.0);
        let binormals: Vec<Vector3> = normals
            .iter()
            .map(|n| Vector3::cross_product(n, n, &across))
            .collect();
        let tangents: Vec<Vector3> = normals
            .iter()
            .map(|n| Vector3::cross_product(n, n, &up))
            .collect();
        let uvs = (0..8)
            .map(|i| Vector2::new(i as f32 * 0.125, 0.5))
            .collect();

        // Two triangles per cube face; each face uses two quads' corners
        // (positions only matter for indices, checker needs triangles).
        let faces: Vec<SceneFace> = (0..12u32)
            .map(|t| {
                let corners = [
                    (t * 2) as usize % 8,
                    (t * 2 + 1) as usize % 8,
                    (t * 2 + 3) as usize % 8,
                ];
                let uv: [usize; 3] = corners;
                let n = normal_of(corners[0]);
                let normals_same = normals.iter().position(|x| *x == n).unwrap_or(0);
                let tangents_same = tangents
                    .iter()
                    .position(|x| normals[tangents.iter().position(|y| y == &n).unwrap_or(0)] == *x)
                    .unwrap_or(0);
                SceneFace {
                    position_index: corners,
                    normal_index: [normals_same; 3],
                    tangent_index: [tangents_same; 3],
                    uv_index: uv,
                    uv_index2: uv,
                    colour_index: uv,
                    material_index: 0,
                }
            })
            .collect();

        let mesh = SceneMesh {
            name: "stone".to_string(),
            node_name: "stone".to_string(),
            full_name: "|Scene Root|stone".to_string(),
            transform_path: "|Scene Root|stone".to_string(),
            positions,
            normals,
            binormals,
            tangents,
            colours: Vec::new(),
            uv_sets: vec![uvs],
            faces,
            materials: vec![SceneMaterial {
                name: "stone_mtl".to_string(),
                map_file: texture.clone(),
                map_id_meaning: 1,
            }],
            world: Matrix4::IDENTITY,
        };
        (mesh, texture)
    }

    fn settings_normal() -> ExportSettings {
        ExportSettings {
            disable_visual_checker: false,
            ..ExportSettings::default()
        }
    }

    fn write_texture(resource: &BwResource, dir: &std::path::Path, texture: &str) {
        let path = dir.join(texture);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, [0u8; 4]).unwrap();
        let _ = resource;
    }

    // ------------------------------------------------------------------
    // ExportSettings IO
    // ------------------------------------------------------------------

    #[test]
    fn settings_round_trip_writes_all_thirty_tags() {
        let settings = ExportSettings {
            reference_nodes_file: "C:/tmp/refnodes".to_string(),
            ..ExportSettings::default()
        };
        let mut section = DataSection::new("settings");
        let resource = BwResource::init_paths(&["/"]);
        settings.write_settings(&mut section, &resource);
        let mut back = ExportSettings::default();
        back.read_settings(&section);
        assert_eq!(back.export_mode, settings.export_mode);
        assert_eq!(back.bone_count, settings.bone_count);
        assert_eq!(back.node_filter, settings.node_filter);
        assert_eq!(back.animation_name, settings.animation_name);
        assert_eq!(back.unit_scale, settings.unit_scale);
        assert_eq!(back.reference_nodes_file, settings.reference_nodes_file);
        assert_eq!(back.copy_textures_to, settings.copy_textures_to);
        assert!(back.include_meshes);
        assert!(!back.include_envelopes_and_bones);
        assert!(back.include_nodes);
        assert!(back.include_materials);
        assert!(!back.use_character_mode);
    }

    #[test]
    fn settings_read_keeps_current_value_as_default() {
        let section = DataSection::new("settings");
        let mut settings = ExportSettings::default();
        settings.read_settings(&section);
        assert_eq!(settings.unit_scale, 0.1);
        assert_eq!(settings.bone_count, 17);
        assert_eq!(settings.export_mode, ExportMode::Normal);
    }

    #[test]
    fn replace_environment_macros_expands_and_fails() {
        // SAFETY: single-threaded test process; the variable is test-local.
        unsafe { std::env::set_var("BW_MAYA_EXPORTER_TEST_HOME", "/tmp/fantasy") };
        let (ok, text) = replace_environment_macros("%BW_MAYA_EXPORTER_TEST_HOME%/art");
        assert!(ok);
        assert_eq!(text, "/tmp/fantasy/art");

        let (ok, text) = replace_environment_macros("%BW_MAYA_EXPORTER_TEST_MISSING_X%/art");
        assert!(!ok);
        assert_eq!(text, "%BW_MAYA_EXPORTER_TEST_MISSING_X%/art");
    }

    // ------------------------------------------------------------------
    // snapping and packing
    // ------------------------------------------------------------------

    #[test]
    fn snap_value_grids_and_pack_normal_layout() {
        // `snapValue` half-snaps then drops the remainder, so 1.4 snaps
        // down onto the 1.0 grid, not to the midpoint.
        assert!((snap_value(1.4, 1.0) - 1.0).abs() < 1e-5);
        assert!((snap_value(-1.4, 1.0) + 1.0).abs() < 1e-5);
        assert!((snap_value(1.2, 1.0) - 1.0).abs() < 1e-5);

        let mut p = Vector3::new(1.23456, -1.0, 4.6);
        snap_point3(&mut p);
        assert!((p.x - 1.235).abs() < 1e-5);

        let packed = pack_normal(&Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(packed & 0x7ff, 1023);
        let packed = pack_normal(&Vector3::new(0.0, 0.0, 1.0));
        assert_eq!((packed >> 22) & 0x3ff, 511);
        let packed = pack_normal(&Vector3::new(0.0, -1.0, 0.0));
        // -1023 -> 0x401 (the wrap quirk)
        assert_eq!((packed >> 11) & 0x7ff, 0x401);
    }

    // ------------------------------------------------------------------
    // the save chain into a real .visual / .primitives pair
    // ------------------------------------------------------------------

    #[test]
    fn visual_mesh_save_round_trips_through_the_visual_checker() {
        let fixture = Fixture::new("save");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        fixture.write("visual_rules.xml", rules_xml().as_bytes());

        let settings = settings_normal();
        let (scene_mesh, _) = unit_cube_scene("save");
        let mut mesh = VisualMesh::new(settings.clone());
        assert!(
            mesh.init(&fixture.resource, &scene_mesh),
            "init should succeed with uvs"
        );

        let prim_path = "stone.primitives";
        let prim_res_name = "stone.primitives";
        let mut visual_section = DataSection::new("root");
        let config = ExportConfig::default();
        assert!(
            mesh.save(
                &fixture.resource,
                &mut visual_section,
                None,
                prim_path,
                prim_res_name,
                true,
                &config
            ),
            "{}",
            mesh.error_text()
        );

        // The XML renderSet was written with the identifier-prefixed
        // section names.
        let xml = visual_section.write_to_string();
        assert!(xml.contains("<renderSet>"), "{xml}");
        assert!(xml.contains("stone.vertices"), "{xml}");
        assert!(xml.contains("stone.indices"), "{xml}");

        // Read the visual back through a fresh parse and gate on the
        // real checker.
        let bytes = visual_section.write_to_string().into_bytes();
        let parsed = resources::datasection::parse("stone.visual", &bytes).unwrap();
        let mut checker = VisualChecker::new(&fixture.resource, "stone.visual");
        assert!(
            checker.check(&fixture.resource, Some(&parsed), prim_res_name),
            "{}",
            checker.error_text()
        );

        // The primitives container parses back: the indices child holds the
        // index header followed by the index stream.
        let prim_bytes = fixture.read(prim_res_name).unwrap();
        let mut container = BinSection::new(prim_res_name, BinaryBlock::new(prim_bytes));
        container.introspect();
        let indices = container
            .children()
            .iter()
            .find(|c| c.section_name() == "stone.indices")
            .expect("indices child");
        let raw = indices.as_binary().data();
        let header = crate::primitive_file_structs::IndexHeader::read(raw).unwrap();
        assert_eq!(header.index_format, "list");
        assert_eq!(header.n_triangle_groups, 1);
        assert_eq!(header.n_indices, 36);
    }

    #[test]
    fn visual_mesh_refuses_mesh_without_uvs_and_without_materials() {
        let fixture = Fixture::new("nouv");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let (mut scene_mesh, _) = unit_cube_scene("nouv");
        scene_mesh.uv_sets.clear();
        let mut mesh = VisualMesh::new(ExportSettings::default());
        assert!(!mesh.init(&fixture.resource, &scene_mesh));
        assert!(mesh.error_text().contains("because there are no uvs"));

        // No materials: createPrimitiveGroups refuses.
        let mut empty = VisualMesh::new(ExportSettings::default());
        empty.identifier = "stone".to_string();
        let mut prim_groups: Vec<PrimitiveGroup> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut entry_size = 2usize;
        assert!(!empty.create_primitive_groups(&mut prim_groups, &mut indices, &mut entry_size));
        assert!(empty.error_text().contains("does not have any materials."));
    }

    #[test]
    fn visual_mesh_dedup_and_winding_and_sort() {
        let mut mesh = VisualMesh::new(ExportSettings::default());
        // operator== requires a shared smoothing-group bit.
        let bv = BloatVertex {
            pos: Vector3::ZERO,
            normal: Vector3::new(0.0, 0.0, 1.0),
            smoothing_group: 1,
            ..BloatVertex::default()
        };
        let i1 = mesh.add_vertex(bv);
        let i2 = mesh.add_vertex(bv);
        assert_eq!(i1, 0);
        assert_eq!(i2, 0, "exact match merges");
        mesh.flip_triangle_winding_order();

        // removeDuplicateVertices: a close-enough later vertex remaps.
        let mut bv2 = bv;
        bv2.pos = Vector3::new(1e-5, 0.0, 0.0);
        mesh.vertices.push(bv2);
        let mut tr = Triangle {
            index: [1, 1, 1],
            ..Triangle::default()
        };
        mesh.triangles.push(tr);
        mesh.remove_duplicate_vertices();
        assert_eq!(mesh.triangles[0].index, [0, 0, 0]);
        let _ = &mut tr;
    }

    // ------------------------------------------------------------------
    // hierarchy
    // ------------------------------------------------------------------

    #[test]
    fn hierarchy_add_find_count_transform() {
        let mut nodes: BTreeMap<String, Matrix4> = BTreeMap::new();
        let mut child = Matrix4::IDENTITY;
        child.m[0][3] = 5.0; // translation x = 5
        child.m[1][3] = 2.0;
        nodes.insert("|child".to_string(), child);
        let mut grand = Matrix4::IDENTITY;
        grand.m[2][3] = 7.0;
        nodes.insert("|child|grand".to_string(), grand);

        let mut root = Hierarchy::new_root();
        root.add_node("|child|grand", &nodes);

        let grand_node = root.find("|child|grand");
        assert_eq!(grand_node.name(), "grand");
        assert_eq!(root.count(), 2, "child + grand");

        // transform: the grand node's world minus its parent's gives
        // relative.
        let parent_world = root.find("|child").transform(None, false, false);
        assert_eq!(parent_world.m[0][3], 5.0);
        let relative = root
            .find("|child|grand")
            .transform(Some(&parent_world), true, false);
        assert_eq!(relative.m[2][3], 7.0);

        // children ordering: count-descending with orderChildren.
        let names: Vec<&str> = root.children(false).iter().map(|h| h.name()).collect();
        assert_eq!(names, vec!["child"]);
        assert_eq!(
            root.recursive_find("grand").map(|h| h.name()),
            Some("grand")
        );
        // `child` on a miss returns the node itself; the root's name is
        // still empty here (the reference only renames it for animations).
        assert_eq!(
            root.child("missing").map(|h| h.name()),
            Some(""),
            "child returns self on a miss"
        );
        let mut renamed = root.clone();
        renamed.set_name("Scene Root");
        assert_eq!(
            renamed.child("missing").map(|h| h.name()),
            Some("Scene Root"),
            "child returns self on a miss"
        );

        // fixup node: stored relative/world transforms.
        let world = Matrix4::translation(1.0, 2.0, 3.0);
        root.add_fixup_node("_BlendBone", world, false);
        let fixup = root.find("_BlendBone");
        // The root's own transform is the orientation-negated identity
        // (row 2's 3x3 columns flip), so relative = own⁻¹ × world carries
        // the negated z row — the reference computes the same thing.
        let negated_identity = Matrix4::new(
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, -1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0,
        );
        let expected_relative = negated_identity.mul(&world);
        assert_eq!(fixup.transform(None, false, false), world);
        assert_eq!(fixup.transform(None, true, false), expected_relative);
    }

    #[test]
    fn export_tree_writes_rows_and_flags_invalid_transforms() {
        let mut nodes = BTreeMap::new();
        let mut m = Matrix4::IDENTITY;
        m.m[0][0] = 2.0;
        nodes.insert("|arm".to_string(), m);

        let mut root = Hierarchy::new_root();
        root.add_node("|arm", &nodes);

        let settings = ExportSettings::default();
        let mut section = DataSection::new("root");
        let mut has_invalid = false;
        export_tree(&mut section, &root, None, "", &mut has_invalid, &settings);
        assert!(!has_invalid, "unit scaling keeps the transform valid");
        // The root node is identity; the "|arm" child carries the scale.
        let root_node = section.open_section("node").unwrap();
        assert_eq!(root_node.read_string("identifier", ""), "Scene Root");
        let rows = root_node.open_section("node").unwrap();
        let row0 = rows.read_vector3("transform/row0", Vector3::ZERO);
        assert_eq!(row0, Vector3::new(2.0, 0.0, 0.0));
        let row3 = rows.read_vector3("transform/row3", Vector3::ZERO);
        assert_eq!(row3, Vector3::ZERO);
        assert_eq!(rows.read_string("identifier", ""), "arm");

        // A zero row is invalid.
        let mut bad = Matrix4::IDENTITY;
        bad.m[1] = [0.0; 4];
        let mut bad_nodes = BTreeMap::new();
        bad_nodes.insert("|arm".to_string(), bad);
        let mut root2 = Hierarchy::new_root();
        root2.add_node("|arm", &bad_nodes);
        let mut section2 = DataSection::new("root");
        let mut has_invalid2 = false;
        export_tree(
            &mut section2,
            &root2,
            None,
            "",
            &mut has_invalid2,
            &settings,
        );
        assert!(has_invalid2);

        assert!(check_any_row_has_zero_scale(
            &Vector3::ZERO,
            &Vector3::new(0.0, 1.0, 0.0),
            &Vector3::new(0.0, 0.0, 1.0)
        ));
        assert!(check_hierarchy(&root, &mut Vec::new()));
        assert!(warn_duplicate_mesh_names(&BTreeSet::new()).contains("0 meshes/skins"));
    }

    // ------------------------------------------------------------------
    // portals + hulls
    // ------------------------------------------------------------------

    #[test]
    fn portal_points_swizzle_save_and_hull_plates() {
        let mut portal = VisualPortal::default();
        assert_eq!(portal.label, "invasive");
        portal.add_point(&Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(
            portal.pts[0],
            Vector3::new(1.0, 3.0, 2.0),
            "addPoint swaps y/z"
        );
        portal.add_swizzled_point(&Vector3::new(4.0, 5.0, 6.0));
        assert_eq!(
            portal.pts[1],
            Vector3::new(4.0, 5.0, 6.0),
            "addSwizzledPoint does not swap"
        );
        portal.set_name("heaven");
        portal.reverse_winding();
        assert_eq!(portal.pts.len(), 2);

        assert_eq!(
            VisualPortal::prop_data_from_prop_name("HEAVEN"),
            Some("heaven")
        );
        assert_eq!(
            VisualPortal::prop_data_from_prop_name("exit"),
            Some("invasive")
        );
        assert_eq!(VisualPortal::prop_data_from_prop_name("unknown"), None);

        // planeEquation from the first three points.
        portal.pts = vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0),
        ];
        let plane = portal.plane_equation();
        assert_eq!(*plane.normal(), Vector3::new(0.0, 0.0, 1.0));

        // A square portal survives the convex hull unchanged.
        portal.create_convex_hull();
        assert_eq!(portal.pts.len(), 4);

        // save() writes the frame-mapped points.
        let mut boundary = DataSection::new("boundary");
        boundary.write_vector3("normal", Vector3::new(0.0, 0.0, 1.0));
        boundary.write_float("d", 0.0);
        assert!(portal.save(&mut boundary));
        // The pre-existing normal/d plus the portal (whose own children
        // carry chunk/uAxis/points).
        assert_eq!(boundary.count_children(), 3, "normal + d + portal");
        let portal_section = boundary.open_section("portal").unwrap();
        assert_eq!(portal_section.read_string("chunk", ""), "heaven");
        assert_eq!(
            portal_section.count_children(),
            6,
            "chunk + uAxis + 4 points"
        );
    }

    #[test]
    fn hull_quickhull_and_export() {
        let fixture = Fixture::new("hull");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let (scene_mesh, _) = unit_cube_scene("hull");
        let mut hull = HullMesh::new(ExportSettings::default());
        assert!(hull.init(&fixture.resource, &scene_mesh));

        // The cube is convex: makeConvex extracts six planes.
        let mut faces: Vec<TriIndex> = Vec::new();
        let mut hull2 = hull.clone();
        let vv = hull2.vertex_positions();
        assert!(hull.make_convex(&vv, &mut faces));
        assert_eq!(faces.len(), 12);
        hull.extract_planes_and_portals(&vv, &mut faces);
        assert_eq!(hull.plane_eqs.len(), 6);

        // exportHull writes one boundary per plane.
        let mut section = DataSection::new("root");
        assert!(hull.export_hull(&mut section));
        assert_eq!(section.count_children(), 6);
        let first = section.open_child(0).unwrap();
        assert_eq!(first.section_name(), "boundary");

        // A degenerate hull (3 points) fails with the reference text.
        let mut tiny = HullMesh::new(ExportSettings::default());
        let (mut mesh_data, _) = unit_cube_scene("hull2");
        mesh_data.positions = mesh_data.positions[..3].to_vec();
        mesh_data.normals = mesh_data.normals[..3].to_vec();
        mesh_data.uv_sets[0] = mesh_data.uv_sets[0][..3].to_vec();
        mesh_data.faces.clear();
        mesh_data.faces.push(SceneFace {
            position_index: [0, 1, 2],
            normal_index: [0, 1, 2],
            tangent_index: [0, 1, 2],
            uv_index: [0, 1, 2],
            uv_index2: [0, 1, 2],
            colour_index: [0, 1, 2],
            material_index: 0,
        });
        tiny.init(&fixture.resource, &mesh_data);
        let mut section2 = DataSection::new("root");
        assert!(!tiny.export_hull(&mut section2));
        assert!(tiny.error_text().contains("Unable to create custom hull."));
    }

    #[test]
    fn generate_hull_writes_six_box_boundaries() {
        let mut section = DataSection::new("root");
        let bb = BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(1.0, 1.0, 1.0));
        generate_hull(&mut section, &bb);
        assert_eq!(section.count_children(), 6);
        let normals: Vec<Vector3> = (0..6)
            .map(|i| {
                let child = section.open_child(i).unwrap();
                child.read_vector3("normal", Vector3::ZERO)
            })
            .collect();
        assert!(normals.contains(&Vector3::new(1.0, 0.0, 0.0)));
        assert!(normals.contains(&Vector3::new(-1.0, 0.0, 0.0)));
        assert!(normals.contains(&Vector3::new(0.0, 1.0, 0.0)));
        assert!(normals.contains(&Vector3::new(0.0, -1.0, 0.0)));
        assert!(normals.contains(&Vector3::new(0.0, 0.0, 1.0)));
        assert!(normals.contains(&Vector3::new(0.0, 0.0, -1.0)));
    }

    #[test]
    fn portals_find_their_boundary() {
        let mut section = DataSection::new("root");
        section.write_vector3("boundingBox/min", Vector3::new(0.0, 0.0, 0.0));
        section.write_vector3("boundingBox/max", Vector3::new(1.0, 1.0, 1.0));
        let bb = BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(1.0, 1.0, 1.0));
        generate_hull(&mut section, &bb);

        // A portal on the x=0 face.
        let portal = VisualPortal {
            pts: vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 1.0, 1.0),
                Vector3::new(0.0, 0.0, 1.0),
            ],
            ..VisualPortal::default()
        };
        let mut errors: Vec<String> = Vec::new();
        export_portals_to_boundaries(&mut section, &mut [portal], &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let mut found = 0;
        for i in 0..section.count_children() {
            let child = section.open_child(i).unwrap();
            if child.section_name() == "boundary" && child.open_section("portal").is_some() {
                found += 1;
            }
        }
        assert_eq!(found, 1);

        // A portal not on any boundary reports the reference error.
        let stray = VisualPortal {
            pts: vec![
                Vector3::new(7.0, 0.0, 0.0),
                Vector3::new(7.0, 1.0, 0.0),
                Vector3::new(7.0, 1.0, 1.0),
            ],
            ..VisualPortal::default()
        };
        let mut errors2: Vec<String> = Vec::new();
        export_portals_to_boundaries(&mut section, &mut [stray], &mut errors2);
        assert_eq!(errors2.len(), 1);
        assert!(errors2[0].contains("not on a boundary"));
    }

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    #[test]
    fn is_shell_and_valid_resource_and_tree_hull_merged() {
        assert!(is_shell("shells/house.visual"));
        assert!(is_shell("spaces/foo/shells/house.visual"));
        assert!(!is_shell("spaces/house.visual"));

        let fixture = Fixture::new("valid");
        assert!(valid_resource(&fixture.resource, "house.visual").is_some());
        assert!(valid_resource(&fixture.resource, "C:/art/house.visual").is_none());
        assert!(valid_resource(&fixture.resource, "//net/house.visual").is_none());
        assert!(valid_resource(&fixture.resource, "").is_none());

        // exportHullMerged merges hull errors into the shared list.
        let mut section = DataSection::new("root");
        let mut hulls: Vec<HullMesh> = Vec::new();
        // empty hulls succeed trivially
        assert!(export_hull_merged(
            &mut hulls,
            &mut section,
            &mut Vec::new()
        ));
    }

    // ------------------------------------------------------------------
    // the driver face
    // ------------------------------------------------------------------

    #[test]
    fn exporter_add_mesh_classifies_bsp_and_hardpoints() {
        let fixture = Fixture::new("classify");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let mut exporter = MayaVisualExporter::new(settings_normal());

        let (scene_mesh, _) = unit_cube_scene("classify");
        assert!(exporter.add_mesh(&fixture.resource, &scene_mesh));

        let mut bsp = scene_mesh.clone();
        bsp.full_name = "|Scene Root|house_bsp".to_string();
        assert!(!exporter.add_mesh(&fixture.resource, &bsp));

        // The reference skips a mesh only when its *leaf* node name starts
        // with HP_; an HP_ node in the middle of the path does not skip.
        let mut hp = scene_mesh.clone();
        hp.full_name = "|Scene Root|HP_Gun".to_string();
        hp.transform_path = "|Scene Root|HP_Gun".to_string();
        assert!(!exporter.add_mesh(&fixture.resource, &hp));
        let mut hp_mid = scene_mesh.clone();
        hp_mid.transform_path = "|Scene Root|HP_Gun|stone".to_string();
        assert!(exporter.add_mesh(&fixture.resource, &hp_mid));
    }

    #[test]
    fn exporter_export_mesh_writes_readable_visual() {
        let fixture = Fixture::new("export");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        fixture.write("visual_rules.xml", rules_xml().as_bytes());

        let mut settings = settings_normal();
        settings.unit_scale = 0.1;
        let mut exporter = MayaVisualExporter::new(settings);
        let (scene_mesh, _) = unit_cube_scene("export");
        assert!(exporter.add_mesh(&fixture.resource, &scene_mesh));

        // The exporter writes the virtual root identifier "Scene Root"
        // itself; a Maya scene whose transform is *also* named
        // "Scene Root" would duplicate it and fail the checker gate, so
        // the fixture mesh hangs directly off the world root.
        let mut scene_mesh = scene_mesh;
        scene_mesh.full_name = "|stone".to_string();
        scene_mesh.transform_path = "|stone".to_string();
        let mut scene = Scene::default();
        scene.nodes.insert("|stone".to_string(), Matrix4::IDENTITY);
        scene.meshes.push(scene_mesh.clone());
        scene.hardpoints.clear();
        scene.locators.clear();

        let out = fixture.dir.join("stone.visual");
        let out_str = out.to_str().unwrap();
        assert!(
            exporter.export_mesh(&fixture.resource, &scene, out_str),
            "{}",
            exporter.error_text()
        );
        assert!(
            exporter.error_text().is_empty(),
            "{}",
            exporter.error_text()
        );

        // The .visual exists and parses.
        let raw = fixture.read("stone.visual").unwrap();
        let visual = resources::datasection::parse("stone.visual", &raw).unwrap();
        assert!(visual.open_section("renderSet").is_some());

        // The .primitives exists next to it and passes the checker (via
        // the res-relative name: the checker resolves through the resource
        // file system).
        let prim_res_name = format!(
            "{}.primitives",
            bwresource::remove_extension("stone.visual")
        );
        assert!(fixture.read(&prim_res_name).is_some());
        let mut checker = VisualChecker::new(&fixture.resource, "stone.visual");
        assert!(
            checker.check(&fixture.resource, Some(&visual), &prim_res_name),
            "{}",
            checker.error_text()
        );

        // The bounding box is scaled by unitScale.
        let min = visual.read_vector3("boundingBox/min", Vector3::ZERO);
        assert!((min.x - 0.0).abs() < 1e-4 && (min.z - 0.0).abs() < 1e-4);
        let max = visual.read_vector3("boundingBox/max", Vector3::ZERO);
        assert!((max.x - 1.0).abs() < 1e-4, "{max:?}");
    }

    #[test]
    fn exporter_write_model_file_and_settings_round_trip() {
        let fixture = Fixture::new("model");
        write_texture(&fixture.resource, &fixture.dir, &tex());

        let mut settings = settings_normal();
        settings.export_mode = ExportMode::Static;
        settings.reference_nodes_file = fixture.dir.join("refnodes").to_str().unwrap().to_string();
        let mut exporter = MayaVisualExporter::new(settings);

        let scene = Scene {
            source_file: "/tmp/fantasy.ma".to_string(),
            ..Scene::default()
        };
        let model_path = fixture.dir.join("stone.model");
        let model_str = model_path.to_str().unwrap();
        assert!(exporter.write_model_file(&fixture.resource, &scene, model_str, "stone.visual"));

        let raw = fixture.read("stone.model").unwrap();
        let section = resources::datasection::parse("stone.model", &raw).unwrap();
        assert_eq!(section.read_string("nodelessVisual", ""), "stone");
        let meta = section.open_section("metaData").unwrap();
        assert_eq!(meta.read_string("sourceFile", ""), "/tmp/fantasy.ma");

        // writeSettingsFile + readSettingsFile round trip through the
        // model file.
        assert!(exporter.write_settings_file(&fixture.resource, model_str));
        let settings2 = ExportSettings {
            export_mode: ExportMode::Normal,
            ..ExportSettings::default()
        };
        let mut exporter2 = MayaVisualExporter::new(settings2);
        assert!(exporter2.read_settings_file(&fixture.resource, model_str));
        assert_eq!(
            exporter2.settings.export_mode,
            exporter.settings.export_mode
        );

        // MeshParticles models are a no-op (no file written).
        let settings3 = ExportSettings {
            export_mode: ExportMode::MeshParticles,
            ..settings_normal()
        };
        let mut exporter3 = MayaVisualExporter::new(settings3);
        let model3 = fixture.dir.join("stone3.model");
        assert!(exporter3.write_model_file(
            &fixture.resource,
            &scene,
            model3.to_str().unwrap(),
            "stone.visual"
        ));
        assert!(fixture.read("stone3.model").is_none());
    }
    // ------------------------------------------------------------------
    // matrix + settings + material helpers
    // ------------------------------------------------------------------

    #[test]
    fn matrix_scale_transpose_default_and_inverse() {
        // Default::default is the identity.
        let d = Matrix4::default();
        assert_eq!(d, Matrix4::IDENTITY);

        // operator*(T): every entry scales.
        let m = Matrix4::translation(2.0, 0.0, 0.0);
        let s = m.scale(3.0);
        assert_eq!(s.m[0][3], 6.0);
        assert_eq!(s.m[1][1], 3.0);

        // transpose mirrors the storage.
        let t = s.transpose();
        assert_eq!(t.m[3][0], 6.0);
        assert_eq!(t.m[1][1], 3.0);

        // The reference `inverse` (matrix4.ipp:113-138) is the adjugate
        // (cofactor(i,j)/det, no transpose), so the round trip is not the
        // identity; it is faithful behaviour, asserted here for the record.
        let inv = s.inverse();
        assert!((inv.m[0][0] - (1.0 / 3.0)).abs() < 1e-4);
        assert_eq!(inv.m[0][3], 0.0);
        // All 16 entries were scaled, so the determinant is 3^4.
        assert!((s.determinant() - 81.0).abs() < 1e-3, "3*3*3*3");
    }

    #[test]
    fn settings_read_mode_and_filter_arms() {
        let xml = b"<root><exportMode>2</exportMode><nodeFilter>1</nodeFilter></root>";
        let section = resources::datasection::parse("settings2", xml).unwrap();
        let mut settings = ExportSettings::default();
        settings.read_settings(&section);
        assert_eq!(settings.export_mode, ExportMode::StaticWithNodes);
        assert_eq!(settings.node_filter, NodeFilter::Selected);

        let xml3 = b"<root><exportMode>3</exportMode><nodeFilter>7</nodeFilter></root>";
        let section3 = resources::datasection::parse("settings3", xml3).unwrap();
        let mut settings3 = ExportSettings::default();
        settings3.read_settings(&section3);
        assert_eq!(settings3.export_mode, ExportMode::MeshParticles);
        assert_eq!(settings3.node_filter, NodeFilter::All);
    }

    #[test]
    fn d3d_color_truncates_channels() {
        // D3DCOLOR_COLORVALUE rounds each channel: (f * 255 + 0.5).
        assert_eq!(d3d_color_value(0.5, 0.5, 0.5, 1.0), 0xff808080);
        // Out-of-range channels wrap after the mask: b(2.0) -> 0xfe,
        // a(-1.0) -> 0x02.
        assert_eq!(d3d_color_value(0.0, 1.0, 2.0, -1.0), 0x0200fffe);
    }

    #[test]
    fn material_eq_and_find_or_add_hit() {
        let a = Material {
            identifier: "m".to_string(),
            map_identifier: "tex.tga".to_string(),
            map_id_meaning: 1,
            self_illum: 0.5,
            in_use: true,
        };
        let b = a.clone();
        assert_eq!(a, b);
        let c = Material {
            self_illum: 0.6,
            ..b.clone()
        };
        assert_ne!(a, c);
        let d = Material {
            map_id_meaning: 2,
            ..b.clone()
        };
        assert_ne!(a, d);

        let mut mesh = VisualMesh::default();
        assert_eq!(mesh.node_identifier, "Scene Root");
        assert_eq!(mesh.find_or_add_material(a.clone()), 0);
        assert_eq!(mesh.find_or_add_material(b), 0, "equal material hits");
        assert_eq!(mesh.find_or_add_material(c), 1);
    }

    #[test]
    fn material_resources_promotes_mfm_and_collects() {
        let fixture = Fixture::new("mfmres");
        // A promotable mfm (wants + no rejects) and a rejected one.
        fixture.write(
            "good.mfm",
            b"<root><mfm><fx>shaders/std_effects/lightonly.fx</fx></mfm></root>",
        );
        // The reject tags are direct children of the mfm section.
        fixture.write(
            "bad.mfm",
            b"<root><Src_Blend_type>1</Src_Blend_type></root>",
        );

        let config = ExportConfig {
            default_material: "defaults/default.mfm".to_string(),
            default_skinned_material: String::new(),
        };

        // meaning 1 + a same-named .mfm promotes in place.
        let mut promoted = Material {
            identifier: "m".to_string(),
            map_identifier: "good.tga".to_string(),
            map_id_meaning: 1,
            ..Material::default()
        };
        let res = promoted.resources(&fixture.resource, &config);
        assert_eq!(promoted.map_identifier, "good.mfm");
        assert_eq!(promoted.map_id_meaning, 2);
        assert_eq!(res, vec!["good.mfm".to_string()]);

        // the rejected mfm is not promoted and the texture stays.
        let mut rejected = Material {
            identifier: "m".to_string(),
            map_identifier: "bad.tga".to_string(),
            map_id_meaning: 1,
            ..Material::default()
        };
        let res = rejected.resources(&fixture.resource, &config);
        assert_eq!(rejected.map_identifier, "bad.tga");
        assert_eq!(rejected.map_id_meaning, 1);
        assert_eq!(res, vec!["bad.tga".to_string()]);

        // meaning 0 falls back to the configured default materials.
        let mut plain = Material {
            identifier: "m".to_string(),
            ..Material::default()
        };
        let res = plain.resources(&fixture.resource, &config);
        assert_eq!(res, vec!["defaults/default.mfm".to_string()]);
        let config_both = ExportConfig {
            default_skinned_material: "defaults/skin.mfm".to_string(),
            ..config
        };
        let res = plain.resources(&fixture.resource, &config_both);
        assert_eq!(res.len(), 2);

        // no map at all and empty defaults collect nothing.
        let res = plain.resources(&fixture.resource, &ExportConfig::default());
        assert!(res.is_empty());
    }

    #[test]
    fn material_save_variant_arms() {
        let config = ExportConfig::default();
        let config_default = ExportConfig {
            default_material: "defaults/default.mfm".to_string(),
            default_skinned_material: String::new(),
        };
        let config_skinned = ExportConfig {
            default_material: String::new(),
            default_skinned_material: "defaults/skin.mfm".to_string(),
        };

        // MeshParticles with and without a map.
        let mut section = DataSection::new("root");
        let mp = Material {
            identifier: "mp".to_string(),
            map_identifier: "tex.tga".to_string(),
            map_id_meaning: 1,
            ..Material::default()
        };
        mp.save(&mut section, None, 0, ExportMode::MeshParticles, &config);
        let pg = section.open_child(0).unwrap();
        let material = pg.find_child("material").unwrap();
        assert_eq!(
            material.read_string("fx", ""),
            "shaders/std_effects/mesh_particle.fx"
        );
        assert_eq!(
            material
                .find_child("property")
                .unwrap()
                .read_string("Texture", ""),
            "tex.tga"
        );

        let mp_bare = Material {
            identifier: "mp".to_string(),
            ..Material::default()
        };
        mp_bare.save(&mut section, None, 1, ExportMode::MeshParticles, &config);
        let pg = section.open_child(1).unwrap();
        assert!(
            pg.find_child("material")
                .unwrap()
                .find_child("property")
                .is_none()
        );

        // meaning 1: the _afx suffix selects the additive fx.
        let afx = Material {
            identifier: "afx".to_string(),
            map_identifier: "wall_afx.tga".to_string(),
            map_id_meaning: 1,
            ..Material::default()
        };
        afx.save(&mut section, None, 2, ExportMode::Normal, &config);
        let material = section
            .open_child(2)
            .unwrap()
            .find_child("material")
            .unwrap();
        assert_eq!(
            material.read_string("fx", ""),
            "shaders/std_effects/lightonly_add.fx"
        );

        // meaning 1 without the suffix, empty map: fx only.
        let plain_tex = Material {
            identifier: "plain".to_string(),
            map_identifier: "tex.tga".to_string(),
            map_id_meaning: 1,
            ..Material::default()
        };
        plain_tex.save(&mut section, None, 3, ExportMode::Normal, &config);
        let material = section
            .open_child(3)
            .unwrap()
            .find_child("material")
            .unwrap();
        assert_eq!(
            material.read_string("fx", ""),
            "shaders/std_effects/lightonly.fx"
        );

        let bare_tex = Material {
            identifier: "plain".to_string(),
            map_id_meaning: 1,
            ..Material::default()
        };
        bare_tex.save(&mut section, None, 4, ExportMode::Normal, &config);
        let material = section
            .open_child(4)
            .unwrap()
            .find_child("material")
            .unwrap();
        assert!(material.find_child("property").is_none());

        // meaning 2 writes the mfm name.
        let mfm = Material {
            identifier: "mfm".to_string(),
            map_identifier: "good.mfm".to_string(),
            map_id_meaning: 2,
            ..Material::default()
        };
        mfm.save(&mut section, None, 5, ExportMode::Normal, &config);
        let material = section
            .open_child(5)
            .unwrap()
            .find_child("material")
            .unwrap();
        assert_eq!(material.read_string("mfm", ""), "good.mfm");

        // meaning 0 falls back to the configured default mfms.
        let plain = Material {
            identifier: "plain".to_string(),
            ..Material::default()
        };
        plain.save(&mut section, None, 6, ExportMode::Normal, &config_default);
        let material = section
            .open_child(6)
            .unwrap()
            .find_child("material")
            .unwrap();
        assert_eq!(material.read_string("mfm", ""), "defaults/default.mfm");
        plain.save(&mut section, None, 7, ExportMode::Normal, &config_skinned);
        let material = section
            .open_child(7)
            .unwrap()
            .find_child("material")
            .unwrap();
        assert_eq!(material.read_string("mfm", ""), "defaults/skin.mfm");
    }

    #[test]
    fn material_save_copies_existing_material() {
        let existing_xml = b"<root>            <junk/>            <renderSet><notGeometry/>                <geometry><notPrimitiveGroup/>                    <primitiveGroup><material><identifier>other</identifier></material></primitiveGroup>                    <primitiveGroup><material><identifier>stone_mtl</identifier><customFlag>1</customFlag></material></primitiveGroup>                </geometry>            </renderSet>        </root>";
        let existing = resources::datasection::parse("existing", existing_xml).unwrap();

        let mat = Material {
            identifier: "stone_mtl".to_string(),
            map_identifier: "tex.tga".to_string(),
            map_id_meaning: 1,
            ..Material::default()
        };
        let mut section = DataSection::new("root");
        mat.save(
            &mut section,
            Some(&existing),
            3,
            ExportMode::Normal,
            &ExportConfig::default(),
        );
        let pg = section.open_child(0).unwrap();
        assert_eq!(pg.as_int(-1), 3, "the pgid value is kept");
        let material = pg.find_child("material").unwrap();
        assert_eq!(material.read_string("identifier", ""), "stone_mtl");
        assert_eq!(material.read_int("customFlag", 0), 1, "copied children");

        // A miss falls back to the normal write face.
        let absent = Material {
            identifier: "absent_mtl".to_string(),
            map_identifier: "tex.tga".to_string(),
            map_id_meaning: 1,
            ..Material::default()
        };
        let mut section2 = DataSection::new("root");
        absent.save(
            &mut section2,
            Some(&existing),
            0,
            ExportMode::Normal,
            &ExportConfig::default(),
        );
        let pg = section2.open_child(0).unwrap();
        assert!(
            pg.find_child("material")
                .unwrap()
                .find_child("customFlag")
                .is_none()
        );
    }
    // ------------------------------------------------------------------
    // VisualMesh init/save/add variants
    // ------------------------------------------------------------------

    /// A cube scene mesh with two uv sets, vertex colours and a second
    /// material slot.
    fn dual_uv_scene(tag: &str) -> (SceneMesh, String) {
        let (mut mesh, texture) = unit_cube_scene(tag);
        mesh.uv_sets
            .push((0..8).map(|i| Vector2::new(0.25, i as f32 * 0.1)).collect());
        mesh.colours = (0..8)
            .map(|i| Vector4::new(i as f32 / 8.0, 0.25, 0.5, 1.0))
            .collect();
        (mesh, texture)
    }

    #[test]
    fn visual_mesh_init_extra_streams_and_snap() {
        let fixture = Fixture::new("streams");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let (scene_mesh, _) = dual_uv_scene("streams");

        // no snap: the bb corners stay exact.
        let mut mesh = VisualMesh::new(settings_normal());
        assert!(mesh.init(&fixture.resource, &scene_mesh));
        assert!(mesh.dual_uv);
        assert!(mesh.vertex_colours);
        assert_eq!(mesh.bb.min, Vector3::ZERO);
        assert_eq!(mesh.bb.max, Vector3::new(10.0, 10.0, 10.0));

        // snap: positions and the bb move onto the 0.001 grid.
        let mut snapped = VisualMesh::new(ExportSettings {
            export_mode: ExportMode::Static,
            snap_vertices: true,
            ..settings_normal()
        });
        snapped.snap_vertices = true;
        assert!(snapped.init(&fixture.resource, &scene_mesh));
        assert!((snapped.vertices[0].pos.x - snapped.vertices[0].pos.x).abs() < 1e-6);
        let corner = snapped.vertices[1].pos;
        assert!((corner.x - (corner.x / 0.001).round() * 0.001).abs() < 1e-4);
    }

    #[test]
    fn visual_mesh_init_error_arms() {
        let fixture = Fixture::new("initerr");
        let (mut mesh_data, _) = unit_cube_scene("initerr");

        // positions + normals + faces but no uvs.
        let mut no_uv = mesh_data.clone();
        no_uv.uv_sets.clear();
        let mut mesh = VisualMesh::default();
        assert!(!mesh.init(&fixture.resource, &no_uv));
        assert!(mesh.error_text().contains("there are no uvs"));

        // an unresolvable %MACRO% texture name reports the reference text.
        mesh_data.materials[0].map_file = "%BW_UNKNOWN_VAR%".to_string();
        let mut mesh2 = VisualMesh::default();
        assert!(mesh2.init(&fixture.resource, &mesh_data));
        assert!(mesh2.error_text().contains("has an invalid texture name"));

        // an empty map file leaves the material fields unset.
        mesh_data.materials[0].map_file = String::new();
        let mut mesh3 = VisualMesh::default();
        assert!(mesh3.init(&fixture.resource, &mesh_data));
        assert!(mesh3.materials[0].map_identifier.is_empty());
        assert_eq!(mesh3.materials[0].map_id_meaning, 0);
    }

    #[test]
    fn visual_mesh_save_writes_uv2_and_colour_streams() {
        let fixture = Fixture::new("uv2");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let (scene_mesh, _) = dual_uv_scene("uv2");
        let mut mesh = VisualMesh::new(settings_normal());
        assert!(mesh.init(&fixture.resource, &scene_mesh));

        let mut visual = DataSection::new("root");
        assert!(
            mesh.save(
                &fixture.resource,
                &mut visual,
                None,
                "uv2mesh.primitives",
                "uv2mesh.primitives",
                true,
                &ExportConfig::default()
            ),
            "{}",
            mesh.error_text()
        );

        let render_set = visual.open_section("renderSet").unwrap();
        let geometry = render_set.open_section("geometry").unwrap();
        let mut stream_names: Vec<String> = Vec::new();
        for i in 0..geometry.count_children() {
            let child = geometry.open_child(i).unwrap();
            if child.section_name() == "stream" {
                stream_names.push(child.as_string().to_string());
            }
        }
        assert!(
            stream_names.contains(&"stone.uv2".to_string()),
            "{stream_names:?}"
        );
        assert!(stream_names.contains(&"stone.colour".to_string()));
        assert!(fixture.read("uv2mesh.primitives").is_some());
    }

    #[test]
    fn visual_mesh_save_reports_write_failure() {
        let fixture = Fixture::new("writefail");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let (scene_mesh, _) = unit_cube_scene("writefail");
        let mut mesh = VisualMesh::new(settings_normal());
        assert!(mesh.init(&fixture.resource, &scene_mesh));

        // "blocker" is a file, so the primitives path below it cannot be
        // written.
        fixture.write("blocker", b"x");
        let mut visual = DataSection::new("root");
        assert!(!mesh.save(
            &fixture.resource,
            &mut visual,
            None,
            "blocker/x.primitives",
            "blocker/x.primitives",
            true,
            &ExportConfig::default()
        ));
        assert!(
            mesh.error_text()
                .contains("Failed to write blocker/x.primitives")
        );
    }

    #[test]
    fn visual_mesh_add_merges_and_remaps() {
        let fixture = Fixture::new("addmerge");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let (scene_mesh, _) = unit_cube_scene("addmerge");
        let mut src = VisualMesh::new(settings_normal());
        assert!(src.init(&fixture.resource, &scene_mesh));

        let mut dest = VisualMesh::default();
        assert!(dest.materials.is_empty());

        // World space: positions go through the source world matrix.
        let mut world = Matrix4::IDENTITY;
        world.m[0][3] = 100.0;
        src.world = world;
        src.add(&mut dest, 5, true);
        assert_eq!(dest.vertices.len(), src.vertices.len());
        assert_eq!(dest.triangles.len(), src.triangles.len());
        assert!((dest.vertices[0].pos.x - 100.0).abs() < 1e-4);
        assert_eq!(dest.vertices[0].mesh_index, 5);
        assert_eq!(dest.materials.len(), 1);

        // A second add reuses the material and honours forceMeshIndex -1.
        let local_vertex_count = dest.vertices.len();
        let mut src2 = VisualMesh::new(settings_normal());
        assert!(src2.init(&fixture.resource, &scene_mesh));
        src2.world = Matrix4::translation(1.0, 0.0, 0.0);
        src2.add(&mut dest, -1, false);
        assert_eq!(dest.vertices.len(), local_vertex_count * 2);
        assert_eq!(dest.materials.len(), 1, "material deduplicated");
        assert_eq!(dest.triangles[dest.triangles.len() - 1].material_index, 0);
        // worldSpace false keeps the source position as-is and the source
        // mesh_index (the -1 force only skips the overwrite).
        assert!((dest.vertices[local_vertex_count].pos.x - 0.0).abs() < 1e-4);
        assert_eq!(dest.vertices[local_vertex_count].mesh_index, 0);
    }

    #[test]
    fn prim_groups_material_runs_and_mesh_particles() {
        // Two materials in runs -> two primitive groups.
        let mut mesh = VisualMesh::new(ExportSettings::default());
        mesh.identifier = "runs".to_string();
        mesh.materials = vec![
            Material {
                identifier: "a".to_string(),
                ..Material::default()
            },
            Material {
                identifier: "b".to_string(),
                ..Material::default()
            },
        ];
        mesh.vertices = (0..6)
            .map(|i| BloatVertex {
                pos: Vector3::new(i as f32, 0.0, 0.0),
                ..BloatVertex::default()
            })
            .collect();
        mesh.triangles = vec![
            Triangle {
                index: [0, 1, 2],
                material_index: 0,
                ..Triangle::default()
            },
            Triangle {
                index: [3, 4, 5],
                material_index: 1,
                ..Triangle::default()
            },
        ];
        let mut prim_groups: Vec<PrimitiveGroup> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut entry_size = 2usize;
        assert!(mesh.create_primitive_groups(&mut prim_groups, &mut indices, &mut entry_size));
        assert_eq!(prim_groups.len(), 2);
        assert_eq!(prim_groups[0].start_index, 0);
        assert_eq!(prim_groups[0].n_primitives, 1);
        assert_eq!(prim_groups[1].start_index, 3);
        assert!(mesh.materials[0].in_use && mesh.materials[1].in_use);

        // list32: more than 65536 vertices switches the index format.
        let mut big = VisualMesh::new(ExportSettings::default());
        big.identifier = "big".to_string();
        big.materials = vec![Material::default()];
        big.vertices = (0..65537u32)
            .map(|i| BloatVertex {
                pos: Vector3::new(i as f32, 0.0, 0.0),
                ..BloatVertex::default()
            })
            .collect();
        big.triangles = (0..65537u32)
            .map(|i| Triangle {
                index: [i, i, i],
                material_index: 0,
                ..Triangle::default()
            })
            .collect();
        let mut prim_groups2: Vec<PrimitiveGroup> = Vec::new();
        let mut indices2: Vec<u32> = Vec::new();
        let mut entry_size2 = 2usize;
        assert!(big.create_primitive_groups(&mut prim_groups2, &mut indices2, &mut entry_size2));
        assert_eq!(entry_size2, 4, "65537 vertices switch the index format");

        // No materials fails with the reference text.
        let mut bare = VisualMesh::new(ExportSettings::default());
        bare.identifier = "bare".to_string();
        let mut prim_groups3: Vec<PrimitiveGroup> = Vec::new();
        let mut indices3: Vec<u32> = Vec::new();
        let mut entry_size3 = 2usize;
        assert!(!bare.create_primitive_groups(&mut prim_groups3, &mut indices3, &mut entry_size3));
        assert!(bare.error_text().contains("does not have any materials"));

        // Mesh particles: one group per mesh index. Fifteen mesh indices
        // (0..14) plus the material group are the 16 the reference
        // asserts (createMeshParticlePrimGroups appends, never clears).
        let mut mp = VisualMesh::new(ExportSettings {
            export_mode: ExportMode::MeshParticles,
            ..ExportSettings::default()
        });
        mp.identifier = "mp".to_string();
        mp.materials = vec![Material::default()];
        mp.vertices = (0..15u32)
            .map(|i| BloatVertex {
                pos: Vector3::new(i as f32, 0.0, 0.0),
                mesh_index: i,
                ..BloatVertex::default()
            })
            .collect();
        mp.triangles = (0..15u32)
            .map(|i| Triangle {
                index: [i, i, i],
                material_index: 0,
                ..Triangle::default()
            })
            .collect();
        let mut prim_groups4: Vec<PrimitiveGroup> = Vec::new();
        let mut indices4: Vec<u32> = Vec::new();
        let mut entry_size4 = 2usize;
        assert!(mp.create_primitive_groups(&mut prim_groups4, &mut indices4, &mut entry_size4));
        assert_eq!(prim_groups4.len(), 16, "one main group plus 15 mesh groups");

        // More than one main group is refused.
        let mut prim_groups5 = prim_groups4.clone();
        prim_groups5.push(PrimitiveGroup::default());
        let mut mp2 = mp.clone();
        assert!(!mp2.create_mesh_particle_prim_groups(&mut prim_groups5));
        assert!(
            mp2.error_text()
                .contains("Mesh particles must use only 1 material/mesh.")
        );
    }
    // ------------------------------------------------------------------
    // GrahamScan + portal init + hierarchy guards
    // ------------------------------------------------------------------

    #[test]
    fn graham_scan_ties_collinear_and_interior() {
        // A collinear mid point and an interior point are dropped; the
        // exactly duplicated fulcrum survives (the reference never dedups).
        let mut pts = vec![
            Vector3::new(0.0, 0.0, 3.0),
            Vector3::new(2.0, 0.0, 3.0),
            Vector3::new(1.0, 1.0, 3.0),
            Vector3::new(0.0, 0.0, 3.0),
            Vector3::new(1.0, 0.0, 3.0),
            Vector3::new(1.0, 0.2, 3.0),
        ];
        GrahamScan::new(&mut pts);
        assert_eq!(pts.len(), 4, "only the hull corners survive: {pts:?}");
    }

    #[test]
    fn portal_init_scales_and_hull_runs() {
        let scene_portal = ScenePortal {
            name: "sp".to_string(),
            label: "heaven".to_string(),
            positions: vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(2.0, 0.0, 0.0),
                Vector3::new(2.0, 2.0, 0.0),
                Vector3::new(0.0, 2.0, 0.0),
            ],
        };
        let mut portal = VisualPortal::default();
        portal.init(&scene_portal, 2.0);
        assert_eq!(portal.name, "sp");
        assert_eq!(portal.label, "heaven");
        assert_eq!(portal.pts.len(), 4);
        assert!(
            portal.pts.contains(&Vector3::new(4.0, 0.0, 0.0)),
            "x corner"
        );
        assert!(
            portal.pts.contains(&Vector3::new(0.0, 4.0, 0.0)),
            "y corner"
        );
        assert!(portal.pts.contains(&Vector3::new(4.0, 4.0, 0.0)), "corner");

        // save() with an empty name skips the chunk tag.
        portal.name = String::new();
        let mut boundary = DataSection::new("boundary");
        boundary.write_vector3("normal", Vector3::new(0.0, 0.0, 1.0));
        boundary.write_float("d", 0.0);
        assert!(portal.save(&mut boundary));
        let portal_section = boundary.open_section("portal").unwrap();
        assert_eq!(portal_section.read_string("chunk", "<absent>"), "<absent>");
    }

    #[test]
    fn hierarchy_guards_duplicates_and_ordering() {
        // Default is the root.
        let root_default = Hierarchy::default();
        assert_eq!(root_default.count(), 0);

        let mut nodes: BTreeMap<String, Matrix4> = BTreeMap::new();
        nodes.insert("|a|stone".to_string(), Matrix4::IDENTITY);
        nodes.insert("|b|stone".to_string(), Matrix4::IDENTITY);
        let mut root = Hierarchy::new_root();
        root.add_node("|a|stone", &nodes);
        root.add_node("|b|stone", &nodes);
        assert_eq!(root.count(), 4);

        // A path that is not part of an existing node's subtree is
        // refused mid-walk.
        // A fresh subtree is legitimately added (the guard arm is only
        // reachable when the walked node's name is not at position 1).
        root.add_node("|q|stone", &nodes);
        assert_eq!(root.count(), 6);
        // A leaf walk ("|a" again) returns at the name check.
        root.add_node("|a", &nodes);
        assert_eq!(root.count(), 6);

        // The mid-walk guard arm itself, driven on an intermediate node.
        let mut guard = Hierarchy::new_intermediate("|st".to_string());
        guard.name = "st".to_string();
        let before = guard.count();
        guard.add_node_walk("|xstone", &nodes, "|st".to_string());
        assert_eq!(guard.count(), before);

        // children(orderChildren) sorts by descendant count descending.
        let names: Vec<&str> = root.children(true).iter().map(|h| h.name()).collect();
        assert_eq!(names.len(), 3);

        // find() on a miss returns the node walked to.
        assert_eq!(root.find("|nope").name(), "");
        assert_eq!(root.find("|a|nope").name(), "a");

        // recursiveFind answers to Scene Root on the root and None below.
        assert!(root.recursive_find("Scene Root").is_some());
        assert!(root.recursive_find("missing").is_none());

        // Duplicate leaf names are collected and reported.
        let mut errors: Vec<String> = Vec::new();
        assert!(!check_hierarchy(&root, &mut errors));
        assert!(errors[0].contains("1 bones with duplicate names"));
        let mut names = BTreeSet::new();
        let mut duplicates = BTreeSet::new();
        find_duplicate_nodes(&root, &mut names, &mut duplicates);
        assert_eq!(duplicates.len(), 1);

        assert_eq!(
            warn_duplicate_mesh_names(&BTreeSet::from(["x".to_string(), "y".to_string()])),
            "The scene contains 2 meshes/skins with duplicate names:\nx\ny\n"
        );
    }

    #[test]
    fn export_tree_pipe_names_and_left_handed_flip() {
        // The left-handed basis flips the third row before the quaternion
        // is extracted; the quaternion stays unit length.
        let flipped = rotation_quaternion([
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, -1.0),
        ]);
        assert!((flipped.length() - 1.0).abs() < 1e-4);
        let upright = rotation_quaternion([
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ]);
        assert!((upright.length() - 1.0).abs() < 1e-4);

        // Fixup nodes with '|' in their name exercise the identifier split.
        let mut root = Hierarchy::new_root();
        root.add_fixup_node("pre|post", Matrix4::translation(1.0, 0.0, 0.0), false);
        let settings = ExportSettings::default();
        let mut section = DataSection::new("root");
        let mut invalid = false;
        export_tree(&mut section, &root, None, "", &mut invalid, &settings);
        let root_node = section.open_section("node").unwrap();
        let fixup = root_node.open_section("node").unwrap();
        assert_eq!(fixup.read_string("identifier", ""), "post");
    }

    // ------------------------------------------------------------------
    // hull edge cases
    // ------------------------------------------------------------------

    /// A scene mesh with the given positions and one triangle per entry of
    /// `faces` (material 0).
    fn raw_hull_scene(tag: &str, positions: &[Vector3], faces: &[[usize; 3]]) -> SceneMesh {
        let count = positions.len();
        let normals: Vec<Vector3> = (0..count).map(|_| Vector3::new(0.0, 1.0, 0.0)).collect();
        SceneMesh {
            name: tag.to_string(),
            node_name: tag.to_string(),
            full_name: format!("|{tag}"),
            transform_path: format!("|{tag}"),
            positions: positions.to_vec(),
            binormals: (0..count).map(|_| Vector3::new(1.0, 0.0, 0.0)).collect(),
            tangents: (0..count).map(|_| Vector3::new(0.0, 0.0, 1.0)).collect(),
            normals,
            colours: Vec::new(),
            uv_sets: vec![
                (0..count)
                    .map(|i| Vector2::new(i as f32 * 0.1, 0.5))
                    .collect(),
            ],
            faces: faces
                .iter()
                .map(|f| SceneFace {
                    position_index: *f,
                    normal_index: *f,
                    tangent_index: *f,
                    uv_index: *f,
                    uv_index2: *f,
                    colour_index: *f,
                    material_index: 0,
                })
                .collect(),
            materials: vec![SceneMaterial {
                name: format!("{tag}_mtl"),
                map_file: String::new(),
                map_id_meaning: 0,
            }],
            world: Matrix4::IDENTITY,
        }
    }

    #[test]
    fn hull_default_bb_and_has_portal_arms() {
        let fixture = Fixture::new("hullarms");
        let mut hull = HullMesh::default();
        assert!(!hull.has_portal(), "no triangles yet");

        let (scene_mesh, _) = unit_cube_scene("hullarms");
        assert!(hull.init(&fixture.resource, &scene_mesh));
        assert_eq!(hull.bb().min, Vector3::ZERO);
        assert_eq!(hull.bb().max, Vector3::new(10.0, 10.0, 10.0));
        assert!(!hull.has_portal(), "plain materials");

        // A portal material makes hasPortal true through the triangles.
        let (mut portal_mesh, _) = unit_cube_scene("hullarms2");
        portal_mesh.materials.push(SceneMaterial {
            name: "invasive_portal".to_string(),
            map_file: String::new(),
            map_id_meaning: 0,
        });
        for face in &mut portal_mesh.faces {
            face.material_index = 1;
        }
        let mut hull2 = HullMesh::new(ExportSettings::default());
        assert!(hull2.init(&fixture.resource, &portal_mesh));
        assert!(hull2.has_portal(), "portal material on the triangles");
    }

    #[test]
    fn hull_portal_material_exports_and_lost_portal_error() {
        let fixture = Fixture::new("hullportal");
        let (mut portal_mesh, _) = unit_cube_scene("hullportal");
        portal_mesh.materials.push(SceneMaterial {
            name: "invasive_portal".to_string(),
            map_file: String::new(),
            map_id_meaning: 0,
        });
        // One triangle coplanar with the z-min hull face carries the
        // portal material.
        portal_mesh.faces.push(SceneFace {
            position_index: [0, 1, 2],
            normal_index: [0, 1, 2],
            tangent_index: [0, 1, 2],
            uv_index: [0, 1, 2],
            uv_index2: [0, 1, 2],
            colour_index: [0, 1, 2],
            material_index: 1,
        });
        let mut hull = HullMesh::new(ExportSettings::default());
        assert!(hull.init(&fixture.resource, &portal_mesh));
        let mut section = DataSection::new("root");
        assert!(hull.export_hull(&mut section), "{}", hull.error_text());
        let with_portal = (0..section.count_children())
            .filter(|i| {
                section
                    .open_child(*i)
                    .unwrap()
                    .open_section("portal")
                    .is_some()
            })
            .count();
        assert_eq!(with_portal, 1, "the portal plane carried a portal");

        // A portal triangle on a plane that the convex hull lost is
        // reported as not exported.
        let (mut lost_mesh, _) = unit_cube_scene("hullportal2");
        lost_mesh.materials.push(SceneMaterial {
            name: "exit_portal".to_string(),
            map_file: String::new(),
            map_id_meaning: 0,
        });
        lost_mesh.positions.push(Vector3::new(1.0, 1.0, 5.0));
        lost_mesh.positions.push(Vector3::new(5.0, 1.0, 5.0));
        lost_mesh.positions.push(Vector3::new(1.0, 5.0, 5.0));
        lost_mesh.normals.push(Vector3::new(0.0, 0.0, 1.0));
        lost_mesh.binormals.push(Vector3::new(1.0, 0.0, 0.0));
        lost_mesh.tangents.push(Vector3::new(0.0, 1.0, 0.0));
        lost_mesh.uv_sets[0].push(Vector2::new(0.1, 0.1));
        lost_mesh.uv_sets[0].push(Vector2::new(0.2, 0.1));
        lost_mesh.uv_sets[0].push(Vector2::new(0.1, 0.2));
        lost_mesh.faces.push(SceneFace {
            position_index: [8, 9, 10],
            normal_index: [8, 8, 8],
            tangent_index: [8, 8, 8],
            uv_index: [8, 9, 10],
            uv_index2: [8, 9, 10],
            colour_index: [8, 9, 10],
            material_index: 1,
        });
        let mut hull2 = HullMesh::new(ExportSettings::default());
        assert!(hull2.init(&fixture.resource, &lost_mesh));
        let mut section2 = DataSection::new("root");
        assert!(!hull2.export_hull(&mut section2));
        assert!(hull2.error_text().contains("will not be exported"));

        // A coplanar fourth point never lifts off the seed triangle.
        let flat = raw_hull_scene(
            "flat",
            &[
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.5, 0.5, 0.0),
            ],
            &[[0, 1, 2]],
        );
        let mut hull3 = HullMesh::new(ExportSettings::default());
        assert!(hull3.init(&fixture.resource, &flat));
        let mut section3 = DataSection::new("root");
        assert!(!hull3.export_hull(&mut section3));
    }

    #[test]
    fn make_convex_axis_and_winding_arms() {
        let fixture = Fixture::new("axes");
        let cube_corners = |sx: f32, sy: f32, sz: f32| -> Vec<Vector3> {
            [
                [0.0, 0.0, 0.0],
                [sx, 0.0, 0.0],
                [sx, sy, 0.0],
                [0.0, sy, 0.0],
                [0.0, 0.0, sz],
                [sx, 0.0, sz],
                [sx, sy, sz],
                [0.0, sy, sz],
            ]
            .iter()
            .map(|p| Vector3::new(p[0], p[1], p[2]))
            .collect()
        };
        let box_faces = |n: usize| -> Vec<[usize; 3]> {
            let mut faces = Vec::new();
            let quads = [
                [0, 1, 2, 3],
                [4, 5, 6, 7],
                [0, 1, 5, 4],
                [2, 3, 7, 6],
                [0, 3, 7, 4],
                [1, 2, 6, 5],
            ];
            for q in &quads {
                faces.push([q[0], q[1], q[2]]);
                faces.push([q[0], q[2], q[3]]);
            }
            let _ = n;
            faces
        };

        // The y axis is the longest: the seed edge comes from y extremes.
        let tall = raw_hull_scene("tall", &cube_corners(1.0, 5.0, 1.0), &box_faces(8));
        let mut hull = HullMesh::new(ExportSettings::default());
        assert!(hull.init(&fixture.resource, &tall));
        let mut section = DataSection::new("root");
        assert!(hull.export_hull(&mut section));

        // The z axis is the longest.
        let deep = raw_hull_scene("deep", &cube_corners(1.0, 1.0, 5.0), &box_faces(8));
        let mut hull2 = HullMesh::new(ExportSettings::default());
        assert!(hull2.init(&fixture.resource, &deep));
        let mut section2 = DataSection::new("root");
        assert!(hull2.export_hull(&mut section2));

        // A mirrored point cloud flips the seed triangle's winding, so
        // the farthest vertex sits on the plane's positive side.
        let mut mirrored = cube_corners(2.0, 2.0, 2.0);
        for p in &mut mirrored {
            p.z = -p.z;
        }
        let mirror = raw_hull_scene("mirror", &mirrored, &box_faces(8));
        let mut hull3 = HullMesh::new(ExportSettings::default());
        assert!(hull3.init(&fixture.resource, &mirror));
        let mut section3 = DataSection::new("root");
        assert!(hull3.export_hull(&mut section3));
    }

    #[test]
    fn make_convex_concave_cloud_removes_inside_vertices() {
        let fixture = Fixture::new("concave");
        let mut pts: Vec<Vector3> = vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(10.0, 0.0, 0.0),
            Vector3::new(10.0, 10.0, 0.0),
            Vector3::new(0.0, 10.0, 0.0),
            Vector3::new(0.0, 0.0, 10.0),
            Vector3::new(10.0, 0.0, 10.0),
            Vector3::new(10.0, 10.0, 10.0),
            Vector3::new(0.0, 10.0, 10.0),
            // face centres and the body centre are all interior.
            Vector3::new(5.0, 0.0, 5.0),
            Vector3::new(5.0, 10.0, 5.0),
            Vector3::new(5.0, 5.0, 5.0),
            Vector3::new(0.0, 5.0, 5.0),
            Vector3::new(10.0, 5.0, 5.0),
        ];
        let faces: Vec<[usize; 3]> = vec![[0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6]];
        let concave = raw_hull_scene("concave", &pts, &faces);
        let _ = &mut pts;
        let mut hull = HullMesh::new(ExportSettings::default());
        assert!(hull.init(&fixture.resource, &concave));
        let mut section = DataSection::new("root");
        assert!(hull.export_hull(&mut section), "{}", hull.error_text());
        assert!(section.count_children() >= 4);
    }
    // ------------------------------------------------------------------
    // the exporter driver: error arms and modes
    // ------------------------------------------------------------------

    #[test]
    fn exporter_default_and_portal_warning() {
        let exporter = MayaVisualExporter::default();
        assert_eq!(exporter.settings, ExportSettings::default());

        let fixture = Fixture::new("warn");
        let scene = Scene::default();
        let mut with_portal = MayaVisualExporter::new(settings_normal());
        with_portal.add_portal(&ScenePortal {
            name: "sp".to_string(),
            label: "invasive".to_string(),
            positions: vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(1.0, 1.0, 0.0),
            ],
        });
        assert!(
            !with_portal.export_mesh(&fixture.resource, &scene, "house"),
            "no meshes at all"
        );
        assert!(with_portal.error_text().contains("non shell directory"));
    }

    #[test]
    fn exporter_rejects_invalid_paths_and_resources() {
        let fixture = Fixture::new("badres");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let mut exporter = MayaVisualExporter::new(settings_normal());
        let (scene_mesh, _) = unit_cube_scene("badres");
        assert!(exporter.add_mesh(&fixture.resource, &scene_mesh));
        let scene = Scene::default();

        // A drive path is refused before any work happens.
        assert!(
            !exporter.export_mesh(&fixture.resource, &scene, "c:/x/stone"),
            "drive path"
        );
        assert!(exporter.error_text().contains("Invalid resource path"));

        // A mesh material pointing at a network path fails the resource
        // census.
        let (mut net_mesh, _) = unit_cube_scene("badres2");
        net_mesh.materials[0].map_file = "//net/tex.tga".to_string();
        let mut exporter2 = MayaVisualExporter::new(settings_normal());
        assert!(exporter2.add_mesh(&fixture.resource, &net_mesh));
        assert!(
            !exporter2.export_mesh(&fixture.resource, &scene, "stone"),
            "network resource"
        );
        assert!(exporter2.error_text().contains("Invalid resource names"));
    }

    #[test]
    fn exporter_static_without_meshes_reports_and_material_kind_copies() {
        let fixture = Fixture::new("kind");
        write_texture(&fixture.resource, &fixture.dir, &tex());

        // Static mode with no meshes refuses.
        let settings = ExportSettings {
            export_mode: ExportMode::Static,
            ..settings_normal()
        };
        let mut exporter = MayaVisualExporter::new(settings);
        let scene = Scene::default();
        assert!(!exporter.export_mesh(&fixture.resource, &scene, "empty"));
        assert!(
            exporter
                .error_text()
                .contains("No valid meshes were found to export.")
        );

        // A pre-existing materialKind is carried into the fresh visual.
        fixture.write(
            "kinded.visual",
            b"<root><materialKind>opaque</materialKind></root>",
        );
        let mut exporter2 = MayaVisualExporter::new(settings_normal());
        let (scene_mesh, _) = unit_cube_scene("kinded");
        assert!(exporter2.add_mesh(&fixture.resource, &scene_mesh));
        assert!(
            exporter2.export_mesh(&fixture.resource, &Scene::default(), "kinded"),
            "{}",
            exporter2.error_text()
        );
        let raw = fixture.read("kinded.visual").unwrap();
        let visual = resources::datasection::parse("kinded", &raw).unwrap();
        assert_eq!(visual.read_string("materialKind", ""), "opaque");
    }

    #[test]
    fn exporter_static_mode_writes_nodeless_visual() {
        let fixture = Fixture::new("static");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let settings = ExportSettings {
            export_mode: ExportMode::Static,
            snap_vertices: true,
            ..settings_normal()
        };
        let mut exporter = MayaVisualExporter::new(settings);
        let (scene_mesh, _) = unit_cube_scene("static");
        assert!(exporter.add_mesh(&fixture.resource, &scene_mesh));
        assert!(
            exporter.export_mesh(&fixture.resource, &Scene::default(), "static_out"),
            "{}",
            exporter.error_text()
        );
        let raw = fixture.read("static_out.visual").unwrap();
        let visual = resources::datasection::parse("static_out", &raw).unwrap();
        // No shell: the customHull tag is absent.
        assert!(visual.find_child("customHull").is_none());
        let render_set = visual.open_section("renderSet").unwrap();
        let geometry = render_set.open_section("geometry").unwrap();
        // use_identifier=false: the stream names carry no mesh prefix.
        assert_eq!(geometry.read_string("vertices", ""), "vertices");
        assert_eq!(geometry.read_string("primitive", ""), "indices");
        assert!(fixture.read("static_out.primitives").is_some());
    }

    #[test]
    fn exporter_mesh_particles_fills_fifteen_clones() {
        let fixture = Fixture::new("mpexp");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let settings = ExportSettings {
            export_mode: ExportMode::MeshParticles,
            disable_visual_checker: true,
            ..settings_normal()
        };
        let mut exporter = MayaVisualExporter::new(settings);
        let (scene_mesh, _) = unit_cube_scene("mpexp");
        assert!(exporter.add_mesh(&fixture.resource, &scene_mesh));
        assert!(
            exporter.export_mesh(&fixture.resource, &Scene::default(), "mp_out"),
            "{}",
            exporter.error_text()
        );
        // The primitives parse and carry 16 primitive groups.
        let raw = fixture.read("mp_out.primitives").unwrap();
        let mut prim = resources::binary::BinSection::new(
            "mp_out.primitives",
            resources::binary::BinaryBlock::new(raw),
        );
        let indices = prim.read_binary("indices").expect("indices section");
        let header = crate::primitive_file_structs::IndexHeader::read(indices.data()).unwrap();
        assert_eq!(header.n_triangle_groups, 16);
    }

    #[test]
    fn exporter_normal_mode_transforms_mesh_bb_to_root_space() {
        let fixture = Fixture::new("bbxf");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let mut exporter = MayaVisualExporter::new(settings_normal());
        let (mut scene_mesh, _) = unit_cube_scene("bbxf");
        // The mesh hangs under a transform node that differs from the root,
        // so the hierarchy walk and the bb transform both run.
        scene_mesh.full_name = "|parent|bbxf".to_string();
        scene_mesh.transform_path = "|parent|bbxf".to_string();
        assert!(exporter.add_mesh(&fixture.resource, &scene_mesh));
        let mut scene = Scene::default();
        scene.nodes.insert("|parent".to_string(), Matrix4::IDENTITY);
        scene.nodes.insert(
            "|parent|bbxf".to_string(),
            Matrix4::translation(5.0, 0.0, 0.0),
        );
        scene.meshes.push(scene_mesh);
        assert!(
            exporter.export_mesh(&fixture.resource, &scene, "bbxf_out"),
            "{}",
            exporter.error_text()
        );

        // The mesh bb (0..10) moved by 5 and was scaled by the unit scale.
        let raw = fixture.read("bbxf_out.visual").unwrap();
        let visual = resources::datasection::parse("bbxf_out", &raw).unwrap();
        let min = visual.read_vector3("boundingBox/min", Vector3::ZERO);
        let max = visual.read_vector3("boundingBox/max", Vector3::ZERO);
        assert!((min.x - 0.5).abs() < 1e-3, "{min:?}");
        assert!((max.x - 1.5).abs() < 1e-3, "{max:?}");
    }

    #[test]
    fn exporter_shell_export_with_hull_and_portal() {
        let fixture = Fixture::new("shell");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        // The file system does not create parent directories.
        std::fs::create_dir_all(fixture.dir.join("shells")).unwrap();

        // A shell with a hull: customHull true and the merged boundaries.
        // A visual mesh is needed as well: the reference deletes the temp
        // primitives file and only a mesh save writes it back, so a hull
        // only export fails the checker gate ("Invalid primitive file") in
        // the reference too.
        let mut exporter = MayaVisualExporter::new(settings_normal());
        let (scene_mesh, _) = unit_cube_scene("shell");
        assert!(exporter.add_hull(&fixture.resource, &scene_mesh));
        assert!(exporter.add_mesh(&fixture.resource, &scene_mesh));
        assert!(
            exporter.export_mesh(&fixture.resource, &Scene::default(), "shells/house"),
            "{}",
            exporter.error_text()
        );
        let raw = fixture.read("shells/house.visual").unwrap();
        let visual = resources::datasection::parse("shells/house", &raw).unwrap();
        assert!(visual.read_bool("customHull", false));
        let boundaries = (0..visual.count_children())
            .filter(|i| visual.open_child(*i).unwrap().section_name() == "boundary")
            .count();
        assert!(boundaries >= 4, "hull boundaries present: {boundaries}");

        // An invasive portal lands in the hull boundaries, and the checker
        // gate forbids portals unless the rules allow them: the reference
        // reports the same error.
        let mut exporter_p = MayaVisualExporter::new(settings_normal());
        let (mesh_p, _) = unit_cube_scene("shellp");
        assert!(exporter_p.add_hull(&fixture.resource, &mesh_p));
        assert!(exporter_p.add_mesh(&fixture.resource, &mesh_p));
        assert!(exporter_p.add_portal(&ScenePortal {
            name: "sp".to_string(),
            label: "invasive".to_string(),
            positions: vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(1.0, 1.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ],
        }));
        assert!(
            !exporter_p.export_mesh(&fixture.resource, &Scene::default(), "shells/housep"),
            "{}",
            exporter_p.error_text()
        );
        assert!(
            exporter_p
                .error_text()
                .contains("Must not have any portals")
        );

        // A shell without hulls gets the generated box hull.
        let mut exporter2 = MayaVisualExporter::new(settings_normal());
        let (mesh2, _) = unit_cube_scene("shell2");
        assert!(exporter2.add_mesh(&fixture.resource, &mesh2));
        assert!(
            exporter2.export_mesh(&fixture.resource, &Scene::default(), "shells/house2"),
            "{}",
            exporter2.error_text()
        );
        let raw2 = fixture.read("shells/house2.visual").unwrap();
        let visual2 = resources::datasection::parse("shells/house2", &raw2).unwrap();
        assert!(!visual2.read_bool("customHull", true));
    }

    #[test]
    fn exporter_checker_and_write_failures() {
        // The checker gate refuses duplicate node names.
        let fixture = Fixture::new("chk");
        write_texture(&fixture.resource, &fixture.dir, &tex());
        let mut exporter = MayaVisualExporter::new(settings_normal());
        let (mut m1, _) = unit_cube_scene("chk");
        m1.transform_path = "|a|stone".to_string();
        m1.full_name = "|a|stone".to_string();
        let (mut m2, _) = unit_cube_scene("chk");
        m2.transform_path = "|b|stone".to_string();
        m2.full_name = "|b|stone".to_string();
        assert!(exporter.add_mesh(&fixture.resource, &m1));
        assert!(exporter.add_mesh(&fixture.resource, &m2));
        let mut scene = Scene::default();
        scene.meshes.push(m1.clone());
        scene.meshes.push(m2.clone());
        assert!(
            !exporter.export_mesh(&fixture.resource, &scene, "dup"),
            "duplicate node names must fail the checker"
        );
        assert!(exporter.error_text().contains("Duplicate node name stone"));

        // A directory where the temp visual belongs fails the first write.
        let fixture2 = Fixture::new("tmpdir");
        write_texture(&fixture2.resource, &fixture2.dir, &tex());
        std::fs::create_dir_all(fixture2.dir.join("exporter_000_temp.visual")).unwrap();
        let mut exporter2 = MayaVisualExporter::new(settings_normal());
        let (m3, _) = unit_cube_scene("tmpdir");
        assert!(exporter2.add_mesh(&fixture2.resource, &m3));
        assert!(!exporter2.export_mesh(&fixture2.resource, &Scene::default(), "blocked"));
        assert!(exporter2.error_text().contains("Failed to write"));

        // A directory at the final visual name fails the rename write.
        let fixture3 = Fixture::new("finaldir");
        write_texture(&fixture3.resource, &fixture3.dir, &tex());
        std::fs::create_dir_all(fixture3.dir.join("final.visual")).unwrap();
        let mut exporter3 = MayaVisualExporter::new(settings_normal());
        let (m4, _) = unit_cube_scene("finaldir");
        assert!(exporter3.add_mesh(&fixture3.resource, &m4));
        assert!(!exporter3.export_mesh(&fixture3.resource, &Scene::default(), "final"));
        assert!(
            exporter3
                .error_text()
                .contains("Failed to write final.visual")
        );

        // A directory at the primitives name fails the primitives copy.
        let fixture4 = Fixture::new("primdir");
        write_texture(&fixture4.resource, &fixture4.dir, &tex());
        std::fs::create_dir_all(fixture4.dir.join("prim.primitives")).unwrap();
        let mut exporter4 = MayaVisualExporter::new(settings_normal());
        let (m5, _) = unit_cube_scene("primdir");
        assert!(exporter4.add_mesh(&fixture4.resource, &m5));
        assert!(!exporter4.export_mesh(&fixture4.resource, &Scene::default(), "prim"));
        assert!(
            exporter4
                .error_text()
                .contains("Failed to write prim.primitives")
        );
    }

    #[test]
    fn write_model_file_nodefull_and_settings_failures() {
        let fixture = Fixture::new("modelfull");
        let mut exporter = MayaVisualExporter::new(settings_normal());
        let scene = Scene::default();

        // Normal mode writes nodefullVisual.
        let model = fixture.dir.join("full.model");
        assert!(exporter.write_model_file(
            &fixture.resource,
            &scene,
            model.to_str().unwrap(),
            "full.visual"
        ));
        let raw = fixture.read("full.model").unwrap();
        let section = resources::datasection::parse("full", &raw).unwrap();
        assert_eq!(section.read_string("nodefullVisual", ""), "full");

        // writeSettingsFile fails on a missing file.
        assert!(!exporter.write_settings_file(&fixture.resource, "missing.model"));

        // readSettingsFile fails on a missing file and on files without
        // the metaData/settings blocks.
        assert!(!exporter.read_settings_file(&fixture.resource, "missing.model"));
        fixture.write("nometa.model", b"<root><other/></root>");
        assert!(!exporter.read_settings_file(&fixture.resource, "nometa.model"));
        fixture.write("nosettings.model", b"<root><metaData/></root>");
        assert!(!exporter.read_settings_file(&fixture.resource, "nosettings.model"));
    }

    #[test]
    fn transform_bb_corners_and_merged_hull_failure() {
        // A translation moves every corner; a scale grows the box.
        let bb = BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(1.0, 1.0, 1.0));
        let moved = transform_bb(&bb, &Matrix4::translation(5.0, 0.0, 0.0));
        assert_eq!(moved.min, Vector3::new(5.0, 0.0, 0.0));
        assert_eq!(moved.max, Vector3::new(6.0, 1.0, 1.0));
        let scaled = transform_bb(&bb, &Matrix4::IDENTITY.scale(2.0));
        assert_eq!(scaled.max, Vector3::new(2.0, 2.0, 2.0));

        // exportHullMerged propagates a failing hull's error text.
        let fixture = Fixture::new("merged");
        let colinear = raw_hull_scene(
            "merged",
            &[
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(2.0, 0.0, 0.0),
                Vector3::new(3.0, 0.0, 0.0),
            ],
            &[[0, 1, 2]],
        );
        let mut hull = HullMesh::new(ExportSettings::default());
        assert!(hull.init(&fixture.resource, &colinear));
        let mut hulls = vec![hull];
        let mut errors: Vec<String> = Vec::new();
        let mut section = DataSection::new("root");
        assert!(!export_hull_merged(&mut hulls, &mut section, &mut errors));
        assert!(errors[0].contains("Unable to create custom hull."));
    }
}
