//! BW-13-05-01: the Max visual exporter's portable core
//! (`tools/visualexporter/`).
//!
//! The reference exporter is a 3ds Max plugin: the `max2009`..`max2013`
//! directories only select which SDK to compile against
//! (`BW_VISUAL_EXPORTER_PROJECT` CMake shims), and the plugin surface —
//! `mfxexp`/`mfxnode` walking the Max scene through the `max.h` SDK,
//! `ExportSettings::displayDialog`, and the `makeBumped` tangent pass
//! backed by `third_party/nvmeshmender` — cannot run without the host.
//! This module ports the portable remainder the exported files depend on:
//!
//! - [`ExportSettings`] and its `readSettings`/`writeSettings`
//!   (`expsets.cpp` constructor defaults, `expsetsio.cpp` nine-field
//!   DataSection IO);
//! - the [`BloatVertex`]/[`Triangle`]/[`Material`] model and the save
//!   pipeline of `visual_mesh.cpp`: `flipTriangleWindingOrder`,
//!   `sortTriangles`, `removeDuplicateVertices` (the compiled
//!   `TC_OPTIMISATION` path), `createPrimitiveGroups` (+ the 16-group
//!   mesh-particle form), the three `createVertexList` formats
//!   (`xyznuv`, `xyznuvi`, `xyznuvtb`), and [`VisualMesh::save`] — the
//!   `<idPrefix>indices` / `<idPrefix>vertices` / `uv2` / `colour`
//!   streams plus the `.visual` XML (`renderSet`/
//!   `treatAsWorldSpaceObject`/`node`/`geometry` and the per-material
//!   `primitiveGroup`/`material` sections);
//! - the `Material::save` faces (`checkFilename`, the override
//!   properties, the `mapIdMeaning` branches and the existing-visual
//!   material copy).
//!
//! Host seams and port deviations, each behaviour-preserving for the
//! files themselves:
//! - `ExportSettings::instance()` (the Max plugin's singleton) becomes an
//!   explicit [`ExportSettings`] parameter — the reference reads it
//!   inside the vertex loops (`unitScale`, `useLegacyOrientation`);
//! - `unitScale` is `GetMasterScale(UNITS_CENTIMETERS)/100`; with the
//!   host's default system unit of centimetres the factor is 1, so the
//!   field defaults to `0.01` metres;
//! - `makeBumped` is not ported: the bump-mapped save face consumes the
//!   `tangent`/`binormal` already carried on [`BloatVertex`] — the same
//!   fields `makeBumped` fills on the host;
//! - the primitives container is passed into [`VisualMesh::save`] instead
//!   of opened through `BWResource` (the reference merges `writeBinary`
//!   children into the existing file — [`BinSection::write_binary`]
//!   keeps that replace-one-name semantics), and the disk write itself
//!   is the caller's;
//! - `BWResolver::dissolveFilename` is the identity (paths arrive
//!   resolved) and the two host lookups are the [`VisualExportIo`]
//!   closures;
//! - `sortTriangles` compaction keeps `realIndex` unmapped, exactly like
//!   the reference (only `index` is renumbered per material run);
//! - the reference's `std::sort` tie order is unspecified — the port
//!   uses a stable sort on the same comparator, which is deterministic
//!   and still groups the runs the pipeline requires.

use std::collections::HashMap;

use foundation::math::Vector4;
use resources::binary::{BinSection, BinaryBlock};
use resources::datasection::DataSection;

use crate::primitive_file_structs::{
    IndexHeader, PrimitiveGroup, VertexHeader, write_primitive_groups,
};

// ----------------------------------------------------------------------
// ExportSettings (`expsets.hpp`, `expsets.cpp`, `expsetsio.cpp`).
// ----------------------------------------------------------------------

/// `ExportSettings::ExportMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportMode {
    /// `NORMAL`.
    #[default]
    Normal,
    /// `STATIC`.
    Static,
    /// `STATIC_WITH_NODES`.
    StaticWithNodes,
    /// `MESH_PARTICLES`.
    MeshParticles,
}

impl ExportMode {
    /// The reference's raw `(ExportMode)readInt` cast; out-of-range values
    /// fall back to `NORMAL` (the dialog would never produce them).
    fn from_int(value: i32) -> Self {
        match value {
            1 => Self::Static,
            2 => Self::StaticWithNodes,
            3 => Self::MeshParticles,
            _ => Self::Normal,
        }
    }
}

/// `ExportSettings::NodeFilter` (host-side node culling; never serialised).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodeFilter {
    /// `ALL`.
    All,
    /// `SELECTED`.
    #[default]
    Selected,
    /// `VISIBLE`.
    Visible,
}

/// The exporter settings singleton, made explicit.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSettings {
    /// `boneCount_` (default 17).
    pub bone_count: i32,
    /// `allowScale_`.
    pub allow_scale: bool,
    /// `bumpMapped_`.
    pub bump_mapped: bool,
    /// `fixCylindrical_` (NVMeshMender's cylindrical seam fix).
    pub fix_cylindrical: bool,
    /// `useLegacyOrientation_`.
    pub use_legacy_orientation: bool,
    /// `keepExistingMaterials_` (read by the host when it decides whether
    /// to hand the existing visual to [`VisualMesh::save`]).
    pub keep_existing_materials: bool,
    /// `snapVertices_`.
    pub snap_vertices: bool,
    /// `visualCheckerEnable_`.
    pub visual_checker_enable: bool,
    /// `exportMode_`.
    pub export_mode: ExportMode,
    /// `nodeFilter_` (never serialised).
    pub node_filter: NodeFilter,
    /// `staticFrame_` (never serialised).
    pub static_frame: u32,
    /// `visualTypeIdentifier_` (never serialised).
    pub visual_type_identifier: String,
    /// `unitScale()`: `GetMasterScale(UNITS_CENTIMETERS)/100`; the host's
    /// default system unit of centimetres gives `0.01` metres per unit.
    pub unit_scale: f32,
    /// The `s_defaultMaterial` option behind the `mapIdMeaning == 0` branch.
    pub default_material: String,
    /// The `s_defaultSkinnedMaterial` option (skinned variant of the above).
    pub default_skinned_material: String,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            bone_count: 17,
            allow_scale: false,
            bump_mapped: false,
            fix_cylindrical: false,
            use_legacy_orientation: false,
            keep_existing_materials: true,
            snap_vertices: false,
            visual_checker_enable: true,
            export_mode: ExportMode::Normal,
            node_filter: NodeFilter::Selected,
            static_frame: 0,
            visual_type_identifier: String::new(),
            unit_scale: 0.01,
            default_material: String::new(),
            default_skinned_material: String::new(),
        }
    }
}

impl ExportSettings {
    /// `ExportSettings::ExportSettings()` — the constructor defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// `ExportSettings::readSettings(DataSectionPtr)`: nine fields, each
    /// read with the current value as its default.
    pub fn read_settings(&mut self, section: Option<&DataSection>) -> bool {
        let Some(section) = section else {
            return false;
        };
        self.export_mode =
            ExportMode::from_int(section.read_int("exportMode", self.export_mode as i32));
        self.allow_scale = section.read_bool("allowScale", self.allow_scale);
        self.bump_mapped = section.read_bool("bumpMapped", self.bump_mapped);
        self.keep_existing_materials =
            section.read_bool("keepExistingMaterials", self.keep_existing_materials);
        self.snap_vertices = section.read_bool("snapVertices", self.snap_vertices);
        self.bone_count = section.read_int("boneCount", self.bone_count);
        self.visual_checker_enable =
            section.read_bool("visualCheckerEnable", self.visual_checker_enable);
        self.fix_cylindrical = section.read_bool("fixCylindrical", self.fix_cylindrical);
        self.use_legacy_orientation =
            section.read_bool("useLegacyOrientation", self.use_legacy_orientation);
        true
    }

    /// `ExportSettings::writeSettings(DataSectionPtr)`.
    pub fn write_settings(&self, section: Option<&mut DataSection>) -> bool {
        let Some(section) = section else {
            return false;
        };
        section.write_int("exportMode", self.export_mode as i32);
        section.write_bool("allowScale", self.allow_scale);
        section.write_bool("bumpMapped", self.bump_mapped);
        section.write_bool("keepExistingMaterials", self.keep_existing_materials);
        section.write_bool("snapVertices", self.snap_vertices);
        section.write_int("boneCount", self.bone_count);
        section.write_bool("visualCheckerEnable", self.visual_checker_enable);
        section.write_bool("fixCylindrical", self.fix_cylindrical);
        section.write_bool("useLegacyOrientation", self.use_legacy_orientation);
        true
    }
}

// ----------------------------------------------------------------------
// BloatVertex / Triangle / Material (`visual_mesh.hpp`).
// ----------------------------------------------------------------------

/// `VisualMesh::BloatVertex`: one vertex carrying every channel the
/// exporter may emit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BloatVertex {
    /// `pos` (Max units; scaled by `unitScale` on the way out).
    pub pos: [f32; 3],
    /// `normal`.
    pub normal: [f32; 3],
    /// `colour` (RGBA floats).
    pub colour: [f32; 4],
    /// `uv`.
    pub uv: [f32; 2],
    /// `uv2` (the dual-UV stream).
    pub uv2: [f32; 2],
    /// `vertexIndex`.
    pub vertex_index: i32,
    /// `smoothingGroup`.
    pub smoothing_group: i32,
    /// `binormal` (filled by `makeBumped` on the host).
    pub binormal: [f32; 3],
    /// `tangent` (filled by `makeBumped` on the host).
    pub tangent: [f32; 3],
    /// `meshIndex` (the mesh-particle sub-mesh id, 1..=15).
    pub mesh_index: i32,
}

/// `closeEnough(float)`: `fabs(a - b) < 0.0001f`.
fn close_enough_f32(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.0001
}

/// `pointCloseEnough(Point4)`.
fn point4_close_enough(a: &[f32; 4], b: &[f32; 4]) -> bool {
    close_enough_f32(a[0], b[0])
        && close_enough_f32(a[1], b[1])
        && close_enough_f32(a[2], b[2])
        && close_enough_f32(a[3], b[3])
}

/// `pointCloseEnough(Point3)`.
fn point3_close_enough(a: &[f32; 3], b: &[f32; 3]) -> bool {
    close_enough_f32(a[0], b[0]) && close_enough_f32(a[1], b[1]) && close_enough_f32(a[2], b[2])
}

/// `pointCloseEnough(Point2)`.
fn point2_close_enough(a: &[f32; 2], b: &[f32; 2]) -> bool {
    close_enough_f32(a[0], b[0]) && close_enough_f32(a[1], b[1])
}

/// `directionCloseEnough(Point3)`: the dot above `cos(0.02)` (roughly two
/// degrees), with an exact-equality escape for zero-length vectors.
fn direction_close_enough(a: &[f32; 3], b: &[f32; 3]) -> bool {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    (dot as f64) > 0.02f64.cos() || a == b
}

impl BloatVertex {
    /// `closeEnough(BloatVertex)`: the merge predicate of
    /// `removeDuplicateVertices`. Note the reference has the
    /// smoothing-group term commented out; `meshIndex` still participates.
    pub fn close_enough(&self, other: &BloatVertex) -> bool {
        point3_close_enough(&self.pos, &other.pos)
            && direction_close_enough(&self.normal, &other.normal)
            && point2_close_enough(&self.uv, &other.uv)
            && point2_close_enough(&self.uv2, &other.uv2)
            && point4_close_enough(&self.colour, &other.colour)
            && direction_close_enough(&self.binormal, &other.binormal)
            && direction_close_enough(&self.tangent, &other.tangent)
            && self.mesh_index == other.mesh_index
    }

    /// `BloatVertex::operator==`: exact channel equality, with the
    /// reference quirk that the smoothing groups pass on a non-zero
    /// overlap (`this->smoothingGroup & bv.smoothingGroup`) rather than
    /// equality. Used by the host-side merge paths, not the save pipeline.
    pub fn reference_eq(&self, other: &BloatVertex) -> bool {
        self.pos == other.pos
            && self.normal == other.normal
            && self.colour == other.colour
            && self.uv == other.uv
            && self.uv2 == other.uv2
            && self.vertex_index == other.vertex_index
            && (self.smoothing_group & other.smoothing_group) != 0
            && self.binormal == other.binormal
            && self.tangent == other.tangent
            && self.mesh_index == other.mesh_index
    }
}

/// `VisualMesh::Triangle`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Triangle {
    /// `index` (into the vertex list).
    pub index: [i32; 3],
    /// `realIndex` (the Max mesh vertex; never re-indexed by the port —
    /// the reference only renumbers `index`).
    pub real_index: [i32; 3],
    /// `smoothingGroup`.
    pub smoothing_group: i32,
    /// `materialIndex`.
    pub material_index: i32,
}

/// `VisualMesh::Material::SkinType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinType {
    /// `NO_SKIN`.
    NoSkin,
    /// `SOFT_SKIN`.
    SoftSkin,
}

/// `VisualMesh::Material`: the fields `Material::save` consumes. The
/// census fields the host fills (`ambient`/`diffuse`/`specular`/
/// `selfIllum`) never reach `Material::save` and stay on the host side.
#[derive(Debug, Clone, Default)]
pub struct Material {
    /// `identifier` (default `"empty"`).
    pub identifier: String,
    /// `mapIdentifier`.
    pub map_identifier: String,
    /// `mapIdMeaning`: 0 = none, 1 = bitmap, 2 = mfm.
    pub map_id_meaning: i32,
    /// `inUse` (set by [`VisualMesh::create_primitive_groups`]).
    pub in_use: bool,
    /// `textureOverrides_`.
    pub texture_overrides: Vec<(String, String)>,
    /// `vectorOverrides_`.
    pub vector_overrides: Vec<(String, Vector4)>,
    /// `boolOverrides_`.
    pub bool_overrides: Vec<(String, bool)>,
    /// `floatOverrides_`.
    pub float_overrides: Vec<(String, f32)>,
    /// `intOverrides_`.
    pub int_overrides: Vec<(String, i32)>,
    /// `fxFile_`.
    pub fx_file: String,
}

impl Material {
    /// The constructor default: `identifier("empty")`, meaning 0, unused.
    pub fn new() -> Self {
        Self {
            identifier: "empty".to_string(),
            ..Self::default()
        }
    }
}

// ----------------------------------------------------------------------
// The host lookup seams.
// ----------------------------------------------------------------------

/// The `.mfm` promotion probe signature (`BWResource::openSection`).
pub type OpenSectionProbe = Box<dyn Fn(&str) -> Option<DataSection>>;

/// The two host lookups `Material::save` performs through `BWResource`.
pub struct VisualExportIo {
    /// `BWResource::fileExists` — [`Material::check_filename`].
    pub file_exists: Box<dyn Fn(&str) -> bool>,
    /// `BWResource::openSection` — the `.mfm` promotion probe.
    pub open_section: OpenSectionProbe,
}

impl VisualExportIo {
    /// No host: nothing exists, no section opens.
    pub fn none() -> Self {
        Self {
            file_exists: Box::new(|_| false),
            open_section: Box::new(|_| None),
        }
    }
}

// ----------------------------------------------------------------------
// Small helpers shared with the reference's utility.cpp / visual_mesh.cpp.
// ----------------------------------------------------------------------

/// `unifySlashes` (`utility.cpp`).
pub fn unify_slashes(s: &str) -> String {
    s.replace('\\', "/")
}

/// `hashKey(Point3)` (`TC_OPTIMISATION`): the bucket key folding the
/// truncated coordinates into a 64-bit value.
fn hash_key(pos: &[f32; 3]) -> u64 {
    let x = (pos[0] as i64 as u64) << 20;
    let y = (pos[1] as i64 as u64) << 10;
    let z = pos[2] as i64 as u64;
    x.wrapping_add(y).wrapping_add(z)
}

/// `packNormal(Point3)` (`visual_mesh.cpp`): normalise, clamp, and fold
/// the components into 11/10-bit fields. The C float→`uint32` casts are
/// truncations whose low bits wrap (a `-x` contribution keeps its two's
/// complement low bits under the mask).
pub fn pack_normal(n: [f32; 3], settings: &ExportSettings) -> u32 {
    let mut n = n;
    let mag = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if mag > 0.0 {
        n = [n[0] / mag, n[1] / mag, n[2] / mag];
    }
    // The reference clamps with two comparisons, which (like `clamp`)
    // lets NaN through.
    let clamp = |v: f32| v.clamp(-1.0, 1.0);
    let x = clamp(n[0]);
    let y = clamp(n[1]);
    let z = clamp(n[2]);
    // float→uint32 in C: truncate to the integer bit pattern.
    let trunc = |v: f32| (v as i64) as u32;
    if settings.use_legacy_orientation {
        (((trunc(-y * 511.0)) & 0x3ff) << 22)
            | (((trunc(z * 1023.0)) & 0x7ff) << 11)
            | ((trunc(-x * 1023.0)) & 0x7ff)
    } else {
        (((trunc(y * 511.0)) & 0x3ff) << 22)
            | (((trunc(z * 1023.0)) & 0x7ff) << 11)
            | ((trunc(x * 1023.0)) & 0x7ff)
    }
}

/// `D3DCOLOR_COLORVALUE`: each component truncates (`(DWORD)(c*255.f)`)
/// and masks to a byte.
pub fn d3dcolor_colorvalue(r: f32, g: f32, b: f32, a: f32) -> u32 {
    let byte = |v: f32| ((v * 255.0) as i64 as u32) & 0xff;
    (byte(a) << 24) | (byte(r) << 16) | (byte(g) << 8) | byte(b)
}

/// `IndexReference::bestFormat` (`lib/moo/index_buffer.hpp`): 65536
/// vertices still select 16-bit indices (`<=`).
pub fn best_entry_size(vertex_num: u32) -> usize {
    if vertex_num <= 65536 { 2 } else { 4 }
}

// ----------------------------------------------------------------------
// VisualMesh.
// ----------------------------------------------------------------------

/// The save failure faces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// `createMeshParticlePrimGroups`: mesh particles must use exactly one
    /// material/mesh (the reference message-boxes and returns false).
    MeshParticleGroups,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MeshParticleGroups => write!(
                f,
                "VisualMesh::createMeshParticlePrimGroups - Mesh particles \
                 must use only 1 material/mesh."
            ),
        }
    }
}

impl std::error::Error for SaveError {}

/// What [`VisualMesh::save`] produced: the `.visual` document root and
/// the primitives container with the stream children merged in.
pub struct SavedVisual {
    /// The `.visual` root (serialise with `write_to_string`).
    pub visual: DataSection,
    /// The `.primitives` container (serialise with `save_bytes`).
    pub primitives: BinSection,
}

/// The portable `VisualMesh`.
#[derive(Debug, Clone, Default)]
pub struct VisualMesh {
    /// `identifier_` (the render-set node name and the id prefix).
    pub identifier: String,
    /// `vertices_`.
    pub vertices: Vec<BloatVertex>,
    /// `triangles_`.
    pub triangles: Vec<Triangle>,
    /// `materials_`.
    pub materials: Vec<Material>,
    /// `dualUV_` (emit the `uv2` stream).
    pub dual_uv: bool,
    /// `vertexColours_` (emit the `colour` stream).
    pub vertex_colours: bool,
    /// `largeIndices_` (set by [`VisualMesh::save`]).
    pub large_indices: bool,
}

impl VisualMesh {
    /// `flipTriangleWindingOrder`.
    pub fn flip_triangle_winding_order(&mut self) {
        for tri in &mut self.triangles {
            tri.index.swap(1, 2);
            tri.real_index.swap(1, 2);
        }
    }

    /// `triangleSorter`: material first, then the mesh index of the first
    /// vertex.
    fn triangle_sort_key(&self, tri: &Triangle) -> (i32, i32) {
        (
            tri.material_index,
            self.vertices[tri.index[0] as usize].mesh_index,
        )
    }

    /// `sortTriangles`: sort by the sorter, then compact the vertex list
    /// per material run (each run renumbers from zero; a run that starts a
    /// new material forgets the previous runs' assignments). Only `index`
    /// is renumbered — `realIndex` keeps pointing at the Max mesh, and the
    /// unreferenced vertices drop out.
    pub fn sort_triangles(&mut self) {
        let mut keys: Vec<(i32, i32)> = self
            .triangles
            .iter()
            .map(|tri| self.triangle_sort_key(tri))
            .collect();
        let mut order: Vec<usize> = (0..self.triangles.len()).collect();
        order.sort_by_key(|&i| keys[i]);
        let sorted: Vec<Triangle> = order.into_iter().map(|i| self.triangles[i]).collect();
        keys.clear();
        self.triangles = sorted;

        if self.triangles.is_empty() {
            return;
        }

        let mut vertex_indices: Vec<i32> = vec![-1; self.vertices.len()];
        let mut vs: Vec<BloatVertex> = Vec::new();
        let mut m_index = self.triangles[0].material_index;
        for tri in &mut self.triangles {
            if tri.material_index != m_index {
                m_index = tri.material_index;
                vertex_indices.iter_mut().for_each(|v| *v = -1);
            }
            for slot in &mut tri.index {
                let ind = *slot as usize;
                if vertex_indices[ind] == -1 {
                    vertex_indices[ind] = vs.len() as i32;
                    vs.push(self.vertices[ind].clone());
                }
                *slot = vertex_indices[ind];
            }
        }
        self.vertices = vs;
    }

    /// `removeDuplicateVertices` — the compiled `TC_OPTIMISATION` path:
    /// bucket by `hashKey(pos)`, `closeEnough` within a bucket, remap the
    /// triangles. (The non-optimised O(n²) scan is the reference's own
    /// dead code under the same guard and is not ported.)
    pub fn remove_duplicate_vertices(&mut self) {
        let mut map: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut remap: Vec<usize> = vec![0; self.vertices.len()];
        for j1 in 0..self.vertices.len() {
            let key = hash_key(&self.vertices[j1].pos);
            let mut found = None;
            if let Some(bucket) = map.get(&key) {
                for &hashed in bucket {
                    if self.vertices[j1].close_enough(&self.vertices[hashed]) {
                        found = Some(hashed);
                        break;
                    }
                }
            }
            match found {
                None => {
                    map.entry(key).or_default().push(j1);
                    remap[j1] = j1;
                }
                Some(index) => remap[j1] = index,
            }
        }
        for tri in &mut self.triangles {
            for j in 0..3 {
                tri.index[j] = remap[tri.index[j] as usize] as i32;
            }
        }
        // (sortTriangles removes the unreferenced vertices)
    }

    /// `createPrimitiveGroups`: one `PrimitiveGroup` per material run over
    /// the sorted triangles, the index buffer in triangle order, and the
    /// 16/32-bit selection by vertex count. Marks each emitted run's
    /// material `inUse`. In mesh-particle mode the single material group
    /// hands over to [`VisualMesh::create_mesh_particle_prim_groups`].
    pub fn create_primitive_groups(
        &mut self,
        settings: &ExportSettings,
    ) -> Result<(Vec<PrimitiveGroup>, Vec<u32>, usize), SaveError> {
        let mut groups = Vec::new();
        let mut indices = Vec::new();
        let entry_size = best_entry_size(self.vertices.len() as u32);
        if self.triangles.is_empty() {
            return Ok((groups, indices, entry_size));
        }

        let mut m_index = self.triangles[0].material_index;
        let mut first_vertex = self.triangles[0].index[0];
        let mut last_vertex = self.triangles[0].index[0];
        let mut first_triangle = 0usize;
        let count = self.triangles.len();
        for i in 0..=count {
            let tri = if i == count {
                &self.triangles[count - 1]
            } else {
                &self.triangles[i]
            };
            if i == count || tri.material_index != m_index {
                groups.push(PrimitiveGroup {
                    start_index: (first_triangle * 3) as i32,
                    n_primitives: (i - first_triangle) as i32,
                    start_vertex: first_vertex,
                    n_vertices: last_vertex - first_vertex + 1,
                });
                first_vertex = tri.index[0];
                last_vertex = tri.index[0];
                first_triangle = i;
                self.materials[m_index as usize].in_use = true;
                m_index = tri.material_index;
            }
            if i != count {
                indices.extend_from_slice(&[
                    tri.index[0] as u32,
                    tri.index[1] as u32,
                    tri.index[2] as u32,
                ]);
                first_vertex = first_vertex
                    .min(tri.index[0])
                    .min(tri.index[1])
                    .min(tri.index[2]);
                last_vertex = last_vertex
                    .max(tri.index[0])
                    .max(tri.index[1])
                    .max(tri.index[2]);
            }
        }

        if settings.export_mode == ExportMode::MeshParticles {
            self.create_mesh_particle_prim_groups(&mut groups)?;
        }
        Ok((groups, indices, entry_size))
    }

    /// `createMeshParticlePrimGroups`: 15 more groups keyed on
    /// `meshIndex` (group 0 stays the whole mesh; 1..=15 draw the
    /// sub-meshes one by one). The final loop iteration also shifts the
    /// window — a dead update the reference performs unconditionally.
    fn create_mesh_particle_prim_groups(
        &self,
        groups: &mut Vec<PrimitiveGroup>,
    ) -> Result<(), SaveError> {
        if groups.len() != 1 {
            return Err(SaveError::MeshParticleGroups);
        }

        if !self.triangles.is_empty() {
            let mut first_vertex = self.triangles[0].index[0];
            let mut last_vertex = first_vertex;
            let mut m_index = self.vertices[first_vertex as usize].mesh_index;
            let mut first_triangle = 0usize;
            let count = self.triangles.len();
            for i in 0..=count {
                let tri = if i == count {
                    &self.triangles[count - 1]
                } else {
                    &self.triangles[i]
                };
                if i == count || self.vertices[tri.index[0] as usize].mesh_index != m_index {
                    groups.push(PrimitiveGroup {
                        start_index: (first_triangle * 3) as i32,
                        n_primitives: (i - first_triangle) as i32,
                        start_vertex: first_vertex,
                        n_vertices: last_vertex - first_vertex + 1,
                    });
                    first_vertex = tri.index[0];
                    last_vertex = tri.index[0];
                    m_index = self.vertices[first_vertex as usize].mesh_index;
                    first_triangle = i;
                }

                first_vertex = first_vertex
                    .min(tri.index[0])
                    .min(tri.index[1])
                    .min(tri.index[2]);
                last_vertex = last_vertex
                    .max(tri.index[0])
                    .max(tri.index[1])
                    .max(tri.index[2]);
            }
        }

        // The MeshParticleRenderer assumes 16 groups.
        debug_assert!(
            groups.len() == 16,
            "mesh particles must produce 16 primitive groups"
        );
        Ok(())
    }

    /// The position/normal swizzle of `createVertexList`: (x, z, y), with
    /// the legacy orientation negating x and y.
    fn swizzle(v: [f32; 3], legacy: bool) -> [f32; 3] {
        if legacy {
            [-v[0], v[2], -v[1]]
        } else {
            [v[0], v[2], v[1]]
        }
    }

    /// The `createVertexList` scale-then-swizzle position of one vertex
    /// (the reference scales `bv.pos` in place; the port applies it
    /// inline — nothing reads the scaled positions afterwards).
    fn vertex_pos(bv: &BloatVertex, settings: &ExportSettings) -> [f32; 3] {
        let scaled = [
            bv.pos[0] * settings.unit_scale,
            bv.pos[1] * settings.unit_scale,
            bv.pos[2] * settings.unit_scale,
        ];
        Self::swizzle(scaled, settings.use_legacy_orientation)
    }

    fn vertex_normal(bv: &BloatVertex, settings: &ExportSettings) -> [f32; 3] {
        Self::swizzle(bv.normal, settings.use_legacy_orientation)
    }

    /// `createVertexList(VertexVector)` — `xyznuv`: 8 floats per vertex.
    pub fn vertex_list_xyznuv(&self, settings: &ExportSettings) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * 32);
        for bv in &self.vertices {
            for v in Self::vertex_pos(bv, settings) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in Self::vertex_normal(bv, settings) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&bv.uv[0].to_le_bytes());
            out.extend_from_slice(&bv.uv[1].to_le_bytes());
        }
        out
    }

    /// `createVertexList(IndexedVertexVector)` — `xyznuvi`: the mesh
    /// particle format, the sub-mesh id encoded as `index_ = meshIndex*3`.
    /// The reference asserts the vertices arrive sorted by mesh index.
    pub fn vertex_list_xyznuvi(&self, settings: &ExportSettings) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * 36);
        let mut last_mesh_index = self.vertices.first().map(|v| v.mesh_index);
        for bv in &self.vertices {
            debug_assert!(
                last_mesh_index.unwrap_or(i32::MIN) <= bv.mesh_index,
                "mesh indices must be non-decreasing"
            );
            last_mesh_index = Some(bv.mesh_index);
            for v in Self::vertex_pos(bv, settings) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in Self::vertex_normal(bv, settings) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&bv.uv[0].to_le_bytes());
            out.extend_from_slice(&bv.uv[1].to_le_bytes());
            out.extend_from_slice(&((bv.mesh_index as f32) * 3.0).to_le_bytes());
        }
        out
    }

    /// `createVertexList(TBVertexVector)` — `xyznuvtb`: packed
    /// normal/tangent/binormal words.
    pub fn vertex_list_xyznuvtb(&self, settings: &ExportSettings) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * 32);
        for bv in &self.vertices {
            for v in Self::vertex_pos(bv, settings) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&pack_normal(bv.normal, settings).to_le_bytes());
            out.extend_from_slice(&bv.uv[0].to_le_bytes());
            out.extend_from_slice(&bv.uv[1].to_le_bytes());
            out.extend_from_slice(&pack_normal(bv.tangent, settings).to_le_bytes());
            out.extend_from_slice(&pack_normal(bv.binormal, settings).to_le_bytes());
        }
        out
    }

    /// `VisualMesh::save`: the primitives container streams and the
    /// `.visual` document. `primitives` is the opened-or-new container
    /// (the reference opens it through `BWResource`; same-name children
    /// are replaced, unrelated ones survive).
    pub fn save(
        &mut self,
        settings: &ExportSettings,
        existing_visual: Option<&DataSection>,
        mut primitives: BinSection,
        use_identifier: bool,
        io: &VisualExportIo,
    ) -> Result<SavedVisual, SaveError> {
        let id_prefix = if use_identifier {
            format!("{}.", self.identifier)
        } else {
            String::new()
        };

        self.flip_triangle_winding_order();
        self.sort_triangles();
        // The bump-mapped path would run `makeBumped` here (the NVMeshMender
        // host pass that fills tangent/binormal); the port consumes the
        // vertex fields as given.
        self.remove_duplicate_vertices();
        self.sort_triangles();

        let (groups, indices, entry_size) = self.create_primitive_groups(settings)?;
        self.large_indices = entry_size != 2;
        let index_format = if self.large_indices { "list32" } else { "list" };

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

        // The .visual document.
        let mut visual = DataSection::new("root");
        let render_set = visual.add_child("renderSet");
        render_set.write_bool("treatAsWorldSpaceObject", false);
        render_set.write_string("node", &self.identifier);
        let geometry = render_set.add_child("geometry");
        geometry.write_string("vertices", &format!("{id_prefix}vertices"));
        for stream in &streams {
            geometry.add_child("stream").set_string(stream);
        }
        geometry.write_string("primitive", &format!("{id_prefix}indices"));

        let mut prim_group_id = 0u32;
        for material in &mut self.materials {
            if material.in_use {
                material.save(
                    geometry,
                    existing_visual,
                    prim_group_id,
                    SkinType::NoSkin,
                    false,
                    settings,
                    io,
                );
                prim_group_id += 1;
            }
        }

        Ok(SavedVisual { visual, primitives })
    }
}

// ----------------------------------------------------------------------
// Material::save and friends (`visual_mesh.cpp`).
// ----------------------------------------------------------------------

/// A `property` child carrying the override name as its value.
fn property<'a>(material: &'a mut DataSection, name: &str) -> &'a mut DataSection {
    let prop = material.add_child("property");
    prop.set_string(name);
    prop
}

impl Material {
    /// `VisualMesh::Material::save`.
    #[allow(clippy::too_many_arguments)]
    pub fn save(
        &mut self,
        geometry: &mut DataSection,
        existing_visual: Option<&DataSection>,
        pgid: u32,
        skin: SkinType,
        skinned_shader: bool,
        settings: &ExportSettings,
        io: &VisualExportIo,
    ) {
        let prim_group = geometry.add_child("primitiveGroup");
        prim_group.set_int(pgid as i32);

        let id_postfix = if skin == SkinType::SoftSkin {
            "_skinned"
        } else {
            ""
        };
        let wanted = format!("{}{}", self.identifier, id_postfix);

        // Copy the existing bitmap/fx combination when one matches.
        if self.find_and_copy_existing_material(&wanted, prim_group, existing_visual) {
            return;
        }

        let material = prim_group.add_child("material");
        material.write_string("identifier", &wanted);

        if settings.export_mode == ExportMode::MeshParticles {
            material.write_string("fx", "shaders/std_effects/mesh_particle.fx");
            if !self.map_identifier.is_empty() {
                property(material, "diffuseMap").write_string("Texture", &self.map_identifier);
            }
        } else if !self.fx_file.is_empty() {
            let mut fx_name = String::new();
            let fx_postfix = if skinned_shader {
                id_postfix.to_string()
            } else {
                String::new()
            };
            // The return value is ignored on this path — the unresolved
            // name still gets written (only the host's resources() walk
            // consumes the bool).
            let _ = self.check_filename(&mut fx_name, &fx_postfix, io);
            // `BWResolver::dissolveFilename` is the identity here.
            material.write_string("fx", &fx_name);
            for (name, texture) in &self.texture_overrides {
                property(material, name).write_string("Texture", &unify_slashes(texture));
            }
            for (name, vector) in &self.vector_overrides {
                property(material, name).write_vector4("Vector4", *vector);
            }
            for (name, value) in &self.bool_overrides {
                property(material, name).write_bool("Bool", *value);
            }
            for (name, value) in &self.float_overrides {
                property(material, name).write_float("Float", *value);
            }
            for (name, value) in &self.int_overrides {
                property(material, name).write_int("Int", *value);
            }
        } else {
            // A same-stem `.mfm` next to the bitmap promotes the map when
            // it looks like a material (and not a legacy blend dump).
            if self.map_identifier.len() > 4 && self.map_id_meaning != 2 {
                let mfm_id = format!(
                    "{}.mfm",
                    &self.map_identifier[..self.map_identifier.len() - 4]
                );
                if let Some(mfm) = (io.open_section)(&mfm_id) {
                    let looks_material = mfm.open_section("mfm").is_some()
                        || mfm.open_section("fx").is_some()
                        || mfm.open_section("property").is_some();
                    let looks_legacy = mfm.open_section("Src_Blend_type").is_some()
                        || mfm.open_section("Dest_Blend_type").is_some()
                        || mfm.open_section("Self_Illumination").is_some()
                        || mfm.open_section("Alpha_blended").is_some();
                    if looks_material && !looks_legacy {
                        self.map_identifier = mfm_id;
                        self.map_id_meaning = 2;
                    }
                }
            }

            if self.map_id_meaning == 1 {
                if skin == SkinType::NoSkin {
                    let mut fx = String::from("shaders/std_effects/lightonly");
                    let len = self.map_identifier.len();
                    if len > 8 && self.map_identifier[len - 8..len - 4] == *"_afx" {
                        fx.push_str("_add");
                    }
                    fx.push_str(".fx");
                    material.write_string("fx", &fx);
                } else if skinned_shader {
                    material.write_string("fx", "shaders/std_effects/lightonly_skinned.fx");
                } else {
                    material.write_string("fx", "shaders/std_effects/lightonly.fx");
                }
                if !self.map_identifier.is_empty() {
                    property(material, "diffuseMap").write_string("Texture", &self.map_identifier);
                }
            } else if self.map_id_meaning == 2 {
                material.write_string("mfm", &self.map_identifier);
            } else if skin == SkinType::NoSkin {
                if !settings.default_material.is_empty() {
                    material.write_string("mfm", &settings.default_material);
                }
            } else if !settings.default_skinned_material.is_empty() {
                material.write_string("mfm", &settings.default_skinned_material);
            }
        }
    }

    /// `checkFilename`: resolve the `_skinned` variant against the host.
    /// `fx_name` always leaves with the (possibly unresolved) name.
    pub fn check_filename(
        &self,
        fx_name: &mut String,
        fx_postfix: &str,
        io: &VisualExportIo,
    ) -> bool {
        *fx_name = unify_slashes(&self.fx_file);
        if fx_name.ends_with(".fx") {
            let stem = &fx_name[..fx_name.len() - 3];
            let mut skinned_fx = false;
            let (pre, post) = match stem.find("_skinned") {
                Some(pos) => {
                    skinned_fx = true;
                    let post = if pos + 8 < stem.len() {
                        &stem[pos + 8..]
                    } else {
                        ""
                    };
                    (&stem[..pos], post)
                }
                None => (stem, ""),
            };
            let candidate = format!("{pre}{fx_postfix}{post}.fx");
            if (io.file_exists)(&candidate) {
                *fx_name = candidate;
            } else if skinned_fx {
                return false;
            }
        }
        true
    }

    /// `findAndCopyExistingMaterial`.
    fn find_and_copy_existing_material(
        &self,
        identifier: &str,
        prim_group: &mut DataSection,
        existing_visual: Option<&DataSection>,
    ) -> bool {
        if let Some(material) = self.find_existing_material(identifier, existing_visual) {
            prim_group.copy_from(material);
            true
        } else {
            false
        }
    }

    /// `findExistingMaterial`: the first `primitiveGroup` whose
    /// `material` carries `identifier`.
    fn find_existing_material<'a>(
        &self,
        identifier: &str,
        existing_visual: Option<&'a DataSection>,
    ) -> Option<&'a DataSection> {
        let existing = existing_visual?;
        for r in 0..existing.count_children() {
            let render_set = existing.open_child(r).unwrap();
            if render_set.section_name() != "renderSet" {
                continue;
            }
            for g in 0..render_set.count_children() {
                let geometry = render_set.open_child(g).unwrap();
                if geometry.section_name() != "geometry" {
                    continue;
                }
                for p in 0..geometry.count_children() {
                    let prim_group = geometry.open_child(p).unwrap();
                    if prim_group.section_name() != "primitiveGroup" {
                        continue;
                    }
                    if let Some(material) = prim_group.open_section("material")
                        && material.read_string("identifier", "") == identifier
                    {
                        return Some(prim_group);
                    }
                }
            }
        }
        None
    }
}

// ----------------------------------------------------------------------
// Tests.
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use resources::bwresource::BwResource;

    /// A vertex with everything zeroed except the given channels.
    fn bv(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> BloatVertex {
        BloatVertex {
            pos,
            normal,
            uv,
            ..BloatVertex::default()
        }
    }

    /// A triangle with `index` set and `realIndex` mirroring it.
    fn tri(index: [i32; 3], material_index: i32) -> Triangle {
        Triangle {
            index,
            real_index: [10, 11, 12],
            smoothing_group: 1,
            material_index,
        }
    }

    fn settings() -> ExportSettings {
        ExportSettings::new()
    }

    #[test]
    fn settings_round_trip_and_defaults() {
        let settings = settings();
        assert!(settings.bone_count == 17);
        assert!(settings.keep_existing_materials);
        assert!(settings.visual_checker_enable);
        assert_eq!(settings.export_mode, ExportMode::Normal);
        assert_eq!(settings.node_filter, NodeFilter::Selected);
        assert_eq!(settings.static_frame, 0);
        assert_eq!(settings.unit_scale, 0.01);
        assert!(!settings.allow_scale && !settings.bump_mapped);
        assert!(!settings.fix_cylindrical && !settings.use_legacy_orientation);
        assert!(!settings.snap_vertices);

        let mut written = DataSection::new("settings");
        assert!(settings.write_settings(Some(&mut written)));
        let mut read_back = ExportSettings {
            bone_count: 1,
            allow_scale: true,
            bump_mapped: true,
            keep_existing_materials: false,
            snap_vertices: true,
            visual_checker_enable: false,
            fix_cylindrical: true,
            use_legacy_orientation: true,
            export_mode: ExportMode::StaticWithNodes,
            ..ExportSettings::new()
        };
        assert!(read_back.read_settings(Some(&written)));
        assert_eq!(read_back, settings);
        assert_eq!(
            written.write_to_string(),
            "<settings>\n\t<exportMode>\t0\t</exportMode>\n\t\
             <allowScale>\tfalse\t</allowScale>\n\t<bumpMapped>\tfalse\t</bumpMapped>\n\t\
             <keepExistingMaterials>\ttrue\t</keepExistingMaterials>\n\t\
             <snapVertices>\tfalse\t</snapVertices>\n\t<boneCount>\t17\t</boneCount>\n\t\
             <visualCheckerEnable>\ttrue\t</visualCheckerEnable>\n\t\
             <fixCylindrical>\tfalse\t</fixCylindrical>\n\t\
             <useLegacyOrientation>\tfalse\t</useLegacyOrientation>\n</settings>\n"
        );
    }

    #[test]
    fn settings_without_a_section_are_not_read_or_written() {
        let mut settings = settings();
        assert!(!settings.read_settings(None));
        assert!(!settings.write_settings(None));
    }

    #[test]
    fn read_settings_takes_every_field_and_casts_the_mode() {
        let mut section = DataSection::new("settings");
        section.write_int("exportMode", 3);
        section.write_bool("allowScale", true);
        section.write_bool("bumpMapped", true);
        section.write_bool("keepExistingMaterials", true);
        section.write_bool("snapVertices", true);
        section.write_int("boneCount", 42);
        section.write_bool("visualCheckerEnable", true);
        section.write_bool("fixCylindrical", true);
        section.write_bool("useLegacyOrientation", true);
        let mut settings = ExportSettings {
            export_mode: ExportMode::MeshParticles,
            ..ExportSettings::new()
        };
        assert!(settings.read_settings(Some(&section)));
        assert_eq!(settings.export_mode, ExportMode::MeshParticles);
        assert_eq!(settings.bone_count, 42);
        assert!(
            settings.allow_scale
                && settings.bump_mapped
                && settings.keep_existing_materials
                && settings.snap_vertices
                && settings.visual_checker_enable
                && settings.fix_cylindrical
                && settings.use_legacy_orientation
        );

        // The raw cast: every dialog value, and the out-of-range fallback.
        assert_eq!(ExportMode::from_int(1), ExportMode::Static);
        assert_eq!(ExportMode::from_int(2), ExportMode::StaticWithNodes);
        assert_eq!(ExportMode::from_int(3), ExportMode::MeshParticles);
        assert_eq!(ExportMode::from_int(0), ExportMode::Normal);
        assert_eq!(ExportMode::from_int(9), ExportMode::Normal);
    }

    #[test]
    fn sort_triangles_on_an_empty_mesh_returns_untouched() {
        let mut mesh = VisualMesh::default();
        mesh.sort_triangles();
        assert!(mesh.triangles.is_empty());
        assert!(mesh.vertices.is_empty());
    }

    #[test]
    fn close_enough_matches_the_reference_tolerances() {
        let a = bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        // Within the 1e-4 tolerance on every channel.
        let mut near = a.clone();
        near.pos = [0.00005, 0.0, 0.0];
        near.normal = [0.0, 0.01, 0.9999];
        near.uv = [0.00005, 0.0];
        assert!(a.close_enough(&near));
        // Past the position tolerance.
        let mut far = a.clone();
        far.pos = [0.0002, 0.0, 0.0];
        assert!(!a.close_enough(&far));
        // A unit direction five degrees off fails the dot test.
        let mut turned = a.clone();
        turned.normal = [0.0872, 0.0, 0.9962];
        assert!(!a.close_enough(&turned));
        // A short vector on both sides has a dot under the threshold but
        // passes through the exact-equality escape.
        let mut flat = a.clone();
        flat.normal = [0.0, 0.0, 0.0];
        let mut flat_too = flat.clone();
        flat_too.pos = [0.00005, 0.0, 0.0];
        assert!(flat.close_enough(&flat_too));
        // A short vector against a unit one is neither close in direction
        // nor exactly equal.
        let mut same = a.clone();
        same.normal = [0.0, 0.0, 0.5];
        assert!(!a.close_enough(&same));
        // meshIndex participates.
        let mut other_mesh = a.clone();
        other_mesh.mesh_index = 3;
        assert!(!a.close_enough(&other_mesh));
        // smoothingGroup does not (the reference's commented-out term).
        let mut other_group = a.clone();
        other_group.smoothing_group = 0b100;
        assert!(a.close_enough(&other_group));
    }

    #[test]
    fn reference_equality_requires_a_smoothing_group_subset() {
        let mut a = bv([1.0, 2.0, 3.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        a.vertex_index = 4;
        a.smoothing_group = 0b11;
        let mut b = a.clone();
        b.smoothing_group = 0b01;
        // 0b01 & 0b11 is non-zero: equal under operator==.
        assert!(a.reference_eq(&b));
        assert!(b.reference_eq(&a));
        b.smoothing_group = 0b100;
        assert!(!a.reference_eq(&b));
        // vertex_index participates in == but not in closeEnough.
        let mut c = bv([1.0, 2.0, 3.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        c.smoothing_group = 0b11;
        assert!(a.close_enough(&c));
        assert!(!a.reference_eq(&c));
    }

    #[test]
    fn flip_triangle_winding_order_swaps_both_index_sets() {
        let mut mesh = VisualMesh {
            triangles: vec![Triangle {
                index: [0, 1, 2],
                real_index: [3, 4, 5],
                smoothing_group: 1,
                material_index: 0,
            }],
            ..VisualMesh::default()
        };
        mesh.flip_triangle_winding_order();
        assert_eq!(mesh.triangles[0].index, [0, 2, 1]);
        assert_eq!(mesh.triangles[0].real_index, [3, 5, 4]);
    }

    #[test]
    fn sort_triangles_groups_by_material_then_mesh_index() {
        let mut mesh = VisualMesh::default();
        // Four distinct vertices, mesh indices 10..13.
        for (i, mesh_index) in [10, 11, 12, 13].into_iter().enumerate() {
            let mut v = bv([i as f32, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
            v.mesh_index = mesh_index;
            mesh.vertices.push(v);
        }
        mesh.triangles = vec![
            tri([2, 1, 0], 5),
            tri([0, 1, 2], 1),
            tri([3, 3, 3], 5),
            tri([1, 2, 3], 1),
        ];
        mesh.sort_triangles();
        // Material 1 first (keys (1,10) then (1,11)), then material 5
        // ((5,12) then (5,13)).
        assert_eq!(
            mesh.triangles,
            vec![
                tri([0, 1, 2], 1),
                tri([1, 2, 3], 1),
                tri([4, 5, 6], 5),
                tri([7, 7, 7], 5),
            ]
        );
        // Each material run restarts the *seen map* but the vertex list
        // keeps accumulating — the second run's vertices land after the
        // first run's (the save pipeline's dedup collapses the run-local
        // duplicates).
        assert_eq!(
            mesh.vertices.iter().map(|v| v.pos[0]).collect::<Vec<_>>(),
            vec![0.0, 1.0, 2.0, 3.0, 2.0, 1.0, 0.0, 3.0]
        );
        assert_eq!(mesh.triangles[0].real_index, [10, 11, 12]);
    }

    #[test]
    fn remove_duplicate_vertices_merges_within_tolerance_and_remaps() {
        let mut mesh = VisualMesh::default();
        let mut near = bv([0.00005, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        near.mesh_index = 0;
        let mut far = bv([0.001, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        far.mesh_index = 0;
        let mut other_mesh = bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        other_mesh.mesh_index = 1;
        mesh.vertices = vec![
            bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            near,
            far,
            other_mesh,
        ];
        mesh.triangles = vec![tri([0, 1, 2], 0), tri([3, 2, 0], 0)];
        mesh.remove_duplicate_vertices();
        // 1 merges into 0 (position within 1e-4, same mesh index); 2 and 3
        // stay (3 differs from 0 only by meshIndex).
        assert_eq!(mesh.triangles[0].index, [0, 0, 2]);
        assert_eq!(mesh.triangles[1].index, [3, 2, 0]);
    }

    #[test]
    fn primitive_groups_cover_each_material_run() {
        let mut mesh = VisualMesh::default();
        for i in 0..6 {
            let mut v = bv([i as f32, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
            v.mesh_index = i;
            mesh.vertices.push(v);
        }
        mesh.materials = vec![Material::new(), Material::new()];
        mesh.triangles = vec![tri([0, 1, 2], 0), tri([1, 2, 3], 0), tri([3, 4, 5], 1)];
        let (groups, indices, entry_size) = mesh.create_primitive_groups(&settings()).unwrap();
        assert_eq!(entry_size, 2);
        assert_eq!(indices, vec![0, 1, 2, 1, 2, 3, 3, 4, 5]);
        assert_eq!(
            groups,
            vec![
                PrimitiveGroup {
                    start_index: 0,
                    n_primitives: 2,
                    start_vertex: 0,
                    n_vertices: 4
                },
                PrimitiveGroup {
                    start_index: 6,
                    n_primitives: 1,
                    start_vertex: 3,
                    n_vertices: 3
                },
            ]
        );
        assert!(mesh.materials[0].in_use && mesh.materials[1].in_use);
        // An unused material stays unused.
        let mut solo = mesh.clone();
        solo.materials.push(Material::new());
        solo.triangles = vec![tri([0, 1, 2], 0)];
        solo.materials.iter_mut().for_each(|m| m.in_use = false);
        let _ = solo.create_primitive_groups(&settings()).unwrap();
        assert!(solo.materials[0].in_use && !solo.materials[1].in_use);
    }

    #[test]
    fn best_entry_size_keeps_the_reference_boundary() {
        assert_eq!(best_entry_size(0), 2);
        assert_eq!(best_entry_size(65536), 2);
        assert_eq!(best_entry_size(65537), 4);
    }

    #[test]
    fn mesh_particles_get_sixteen_groups_by_mesh_index() {
        // 15 sub-meshes (mesh indices 1..=15) over one material.
        let mut mesh = VisualMesh {
            materials: vec![Material::new()],
            ..VisualMesh::default()
        };
        for j in 0..15 {
            let mut v = bv([j as f32, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
            v.mesh_index = j + 1;
            mesh.vertices.push(v);
            mesh.triangles.push(tri([j, j, j], 0));
        }
        let mut settings = settings();
        settings.export_mode = ExportMode::MeshParticles;
        let (groups, indices, _) = mesh.create_primitive_groups(&settings).unwrap();
        assert_eq!(groups.len(), 16);
        // Group 0: the whole mesh.
        assert_eq!(
            groups[0],
            PrimitiveGroup {
                start_index: 0,
                n_primitives: 15,
                start_vertex: 0,
                n_vertices: 15
            }
        );
        // Group k: the single triangle (and vertex) of mesh index k+1.
        for (k, group) in groups[1..].iter().enumerate() {
            assert_eq!(
                group,
                &PrimitiveGroup {
                    start_index: (k * 3) as i32,
                    n_primitives: 1,
                    start_vertex: k as i32,
                    n_vertices: 1
                }
            );
        }
        assert_eq!(indices.len(), 45);
    }

    #[test]
    fn mesh_particle_export_refuses_multiple_materials() {
        let mut mesh = VisualMesh {
            materials: vec![Material::new(), Material::new()],
            ..VisualMesh::default()
        };
        mesh.vertices = vec![
            bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            bv([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            bv([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
        ];
        mesh.triangles = vec![tri([0, 1, 2], 0), tri([2, 1, 0], 1)];
        let mut settings = settings();
        settings.export_mode = ExportMode::MeshParticles;
        assert_eq!(
            mesh.create_primitive_groups(&settings),
            Err(SaveError::MeshParticleGroups)
        );
        assert_eq!(
            SaveError::MeshParticleGroups.to_string(),
            "VisualMesh::createMeshParticlePrimGroups - Mesh particles must \
             use only 1 material/mesh."
        );
    }

    #[test]
    fn empty_mesh_produces_empty_streams() {
        let mut mesh = VisualMesh::default();
        let (groups, indices, entry_size) = mesh.create_primitive_groups(&settings()).unwrap();
        assert!(groups.is_empty() && indices.is_empty() && entry_size == 2);
    }

    #[test]
    fn vertex_list_swizzles_scales_and_supports_legacy_orientation() {
        let mesh = VisualMesh {
            vertices: vec![bv([1.0, 2.0, 3.0], [0.3, 0.4, 0.5], [0.25, 0.75])],
            ..VisualMesh::default()
        };

        let bytes = mesh.vertex_list_xyznuv(&settings());
        let floats: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        assert_eq!(floats.len(), 8);
        // Unit scale 0.01, then the (x, z, y) swizzle.
        assert_eq!(floats[..3], [0.01, 0.03, 0.02]);
        assert_eq!(floats[3..6], [0.3, 0.5, 0.4]);
        assert_eq!(floats[6..], [0.25, 0.75]);

        let mut legacy = settings();
        legacy.use_legacy_orientation = true;
        let bytes = mesh.vertex_list_xyznuv(&legacy);
        let floats: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        assert_eq!(floats[..3], [-0.01, 0.03, -0.02]);
        assert_eq!(floats[3..6], [-0.3, 0.5, -0.4]);
    }

    #[test]
    fn pack_normal_matches_the_reference_field_folds() {
        let settings = settings();
        // +x: 1023 in the low field.
        assert_eq!(pack_normal([2.0, 0.0, 0.0], &settings), 1023);
        // +z: 1023 shifted 11.
        assert_eq!(pack_normal([0.0, 0.0, 5.0], &settings), 1023 << 11);
        // +y: 511 shifted 22.
        assert_eq!(pack_normal([0.0, 1.0, 0.0], &settings), 511 << 22);
        // Legacy negations wrap through the masks: -1023 keeps the low
        // bits 0x401 of the two's complement.
        let mut legacy = ExportSettings::new();
        legacy.use_legacy_orientation = true;
        assert_eq!(pack_normal([1.0, 0.0, 0.0], &legacy), 0x401);
        assert_eq!(pack_normal([0.0, 1.0, 0.0], &legacy), (1024 - 511) << 22);
        assert_eq!(pack_normal([0.0, 0.0, 1.0], &legacy), 1023 << 11);
    }

    #[test]
    fn tb_vertex_list_packs_words_and_keeps_uv() {
        let mut mesh = VisualMesh::default();
        let mut v = bv([1.0, 2.0, 3.0], [1.0, 0.0, 0.0], [0.25, 0.75]);
        v.tangent = [0.0, 1.0, 0.0];
        v.binormal = [0.0, 0.0, 1.0];
        mesh.vertices = vec![v];
        let bytes = mesh.vertex_list_xyznuvtb(&settings());
        assert_eq!(bytes.len(), 32);
        let words: Vec<u32> = bytes[12..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        assert_eq!(words[0], 1023); // normal +x
        let uv: [f32; 2] = [
            f32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            f32::from_le_bytes(bytes[20..24].try_into().unwrap()),
        ];
        assert_eq!(uv, [0.25, 0.75]);
        assert_eq!(words[3], 511 << 22); // tangent +y
        assert_eq!(words[4], 1023 << 11); // binormal +z
    }

    #[test]
    fn vertex_list_index_channel_encodes_mesh_particle_ids() {
        let mut mesh = VisualMesh::default();
        let mut a = bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        a.mesh_index = 2;
        let mut b = bv([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        b.mesh_index = 5;
        mesh.vertices = vec![a, b];
        let bytes = mesh.vertex_list_xyznuvi(&settings());
        assert_eq!(bytes.len(), 72);
        let index = f32::from_le_bytes(bytes[32..36].try_into().unwrap());
        assert_eq!(index, 6.0); // 2 * 3
        let index = f32::from_le_bytes(bytes[68..72].try_into().unwrap());
        assert_eq!(index, 15.0); // 5 * 3
    }

    #[test]
    fn d3dcolor_truncates_each_component() {
        // 0.5 * 255 = 127.5 truncates to 127 (no round-up).
        assert_eq!(d3dcolor_colorvalue(0.5, 0.5, 0.5, 1.0), 0xFF7F7F7F);
        assert_eq!(d3dcolor_colorvalue(1.0, 0.0, 0.25, 0.0), 0x00FF003F);
    }

    #[test]
    fn save_writes_readable_primitives_and_visual() {
        let mut mesh = VisualMesh {
            identifier: "cube".to_string(),
            ..VisualMesh::default()
        };
        mesh.dual_uv = true;
        mesh.vertex_colours = true;
        // Two triangles sharing a duplicate vertex position (merge via the
        // dedup pass), two materials.
        let mut colour = BloatVertex {
            pos: [0.0, 0.0, 0.0],
            ..BloatVertex::default()
        };
        colour.normal = [0.0, 0.0, 1.0];
        colour.uv = [0.0, 0.0];
        colour.uv2 = [0.1, 0.2];
        colour.colour = [0.5, 0.5, 0.5, 1.0];
        let mut twin = colour.clone();
        twin.pos = [0.00005, 0.0, 0.0];
        let mut tip = colour.clone();
        tip.pos = [1.0, 0.0, 0.0];
        let mut last = colour.clone();
        last.pos = [0.0, 1.0, 0.0];
        mesh.vertices = vec![colour, twin, tip, last];
        mesh.triangles = vec![tri([0, 2, 1], 0), tri([0, 3, 2], 1)];
        mesh.materials = vec![
            Material {
                identifier: "brick".to_string(),
                map_identifier: "grid.tga".to_string(),
                map_id_meaning: 1,
                ..Material::new()
            },
            Material {
                identifier: "metal".to_string(),
                map_identifier: "steel.mfm".to_string(),
                map_id_meaning: 2,
                ..Material::new()
            },
        ];

        let saved = mesh
            .save(
                &settings(),
                None,
                BinSection::new("cube.primitives", BinaryBlock::default()),
                true,
                &VisualExportIo::none(),
            )
            .unwrap();

        // -- the primitives container --
        let mut prims = saved.primitives;
        assert!(prims.read_binary("cube.indices").is_some());
        assert!(prims.read_binary("cube.vertices").is_some());
        assert!(prims.read_binary("cube.uv2").is_some());
        assert!(prims.read_binary("cube.colour").is_some());

        let indices_blob = prims.read_binary("cube.indices").unwrap();
        let header = IndexHeader::read(indices_blob.data()).expect("index header");
        assert_eq!(header.index_format, "list");
        assert_eq!(header.n_indices, 6);
        assert_eq!(header.n_triangle_groups, 2);
        let groups = write_and_read_groups(indices_blob.data(), &header);
        assert_eq!(groups.len(), 2);
        // Reference-faithful artifact: dedup merges the near-duplicate
        // corners, and then the *second* sort re-compacts per material run —
        // which pushes the shared corners back into the vertex list, so the
        // second group's ranges start after the first run's entries.
        assert_eq!(
            groups[0],
            PrimitiveGroup {
                start_index: 0,
                n_primitives: 1,
                start_vertex: 0,
                n_vertices: 2
            }
        );
        assert_eq!(
            groups[1],
            PrimitiveGroup {
                start_index: 3,
                n_primitives: 1,
                start_vertex: 2,
                n_vertices: 3
            }
        );
        let base = IndexHeader::SIZE;
        let idx: Vec<u16> = indices_blob.data()[base..base + 12]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        assert_eq!(idx, vec![0, 0, 1, 2, 3, 4]);
        for group in &groups {
            let start = group.start_index as usize * 2 + base;
            let span = group.n_primitives as usize * 6;
            for chunk in indices_blob.data()[start..start + span].chunks(2) {
                let v = u16::from_le_bytes(chunk.try_into().unwrap()) as i32;
                assert!(v >= group.start_vertex);
                assert!(v < group.start_vertex + group.n_vertices);
            }
        }

        let vertex_blob = prims.read_binary("cube.vertices").unwrap();
        let vheader = VertexHeader::read(vertex_blob.data()).expect("vertex header");
        assert_eq!(vheader.vertex_format, "xyznuv");
        assert_eq!(vheader.n_vertices, 5);
        let floats: Vec<f32> = vertex_blob.data()[VertexHeader::SIZE..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        // [origin, tip, origin, tip, last corner]: the second sort's
        // per-material-run compaction pushes the shared corners back in.
        // The (x,z,y) swizzle and the 0.01 unit scale apply; all normals
        // point up-z in the source, i.e. up-y here.
        assert_eq!(floats.len(), 40);
        assert_eq!(
            floats,
            vec![
                0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, //
                0.01, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, //
                0.01, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 0.01, 0.0, 1.0, 0.0, 0.0, 0.0, //
            ]
        );

        let uv2 = prims.read_binary("cube.uv2").unwrap();
        assert_eq!(uv2.data().len(), 40);
        let stream_uv: Vec<f32> = uv2
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        assert_eq!(&stream_uv[..2], &[0.1, 0.2]);
        assert_eq!(&stream_uv[8..10], &[0.1, 0.2]);

        let colours = prims.read_binary("cube.colour").unwrap();
        assert_eq!(colours.data().len(), 20);
        assert_eq!(
            u32::from_le_bytes(colours.data()[..4].try_into().unwrap()),
            0xFF7F7F7F
        );

        // -- the visual document --
        let mut visual = saved.visual;
        let xml = visual.write_to_string();
        let parsed = resources::datasection::parse("cube.visual", xml.as_bytes()).unwrap();
        let render_set = parsed.open_section("renderSet").unwrap();
        assert!(!render_set.read_bool("treatAsWorldSpaceObject", true));
        assert_eq!(render_set.read_string("node", ""), "cube");
        let geometry = render_set.open_section("geometry").unwrap();
        assert_eq!(geometry.read_string("vertices", ""), "cube.vertices");
        assert_eq!(geometry.read_string("primitive", ""), "cube.indices");
        let streams: Vec<String> = (0..geometry.count_children())
            .filter_map(|i| {
                let child = geometry.open_child(i).unwrap();
                (child.section_name() == "stream").then(|| child.as_string().to_string())
            })
            .collect();
        assert_eq!(streams, vec!["cube.uv2", "cube.colour"]);
        let groups: Vec<&DataSection> = (0..geometry.count_children())
            .filter_map(|i| {
                let child = geometry.open_child(i).unwrap();
                (child.section_name() == "primitiveGroup").then_some(child)
            })
            .collect();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].as_int(255), 0);
        assert_eq!(groups[0].read_string("material/identifier", ""), "brick");
        assert_eq!(
            groups[0].read_string("material/fx", ""),
            "shaders/std_effects/lightonly.fx"
        );
        assert_eq!(
            groups[0].read_string("material/property/Texture", ""),
            "grid.tga"
        );
        assert_eq!(groups[1].as_int(255), 1);
        assert_eq!(groups[1].read_string("material/mfm", ""), "steel.mfm");
    }

    /// Serialise the groups back out of a saved index blob the way a
    /// reader walks the container: header, then indices, then groups.
    fn write_and_read_groups(data: &[u8], header: &IndexHeader) -> Vec<PrimitiveGroup> {
        let base = IndexHeader::SIZE + header.n_indices as usize * 2;
        crate::primitive_file_structs::read_primitive_groups(
            &data[base..],
            header.n_triangle_groups as usize,
        )
    }

    #[test]
    fn save_round_trips_through_the_visual_checker() {
        // The engine-side readability face: the checker opens the
        // primitives container, parses the index headers and counts the
        // triangles — the same files this exporter wrote.
        let dir = std::env::temp_dir().join(format!("bw_visual_exporter_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let resource = BwResource::init_paths(&[dir.to_str().unwrap()]);

        std::fs::write(
            dir.join("visual_rules.xml"),
            "<root>\n<rule><path>structures/</path><filespec>*.visual</filespec>\n\
             <identifier>normal</identifier><exportAs>normal</exportAs>\n\
             </rule>\n</root>",
        )
        .unwrap();

        let mut mesh = VisualMesh {
            identifier: "house".to_string(),
            ..VisualMesh::default()
        };
        mesh.vertices = vec![
            bv([0.0, 0.0, 100.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            bv([100.0, 0.0, 100.0], [0.0, 0.0, 1.0], [1.0, 0.0]),
            bv([0.0, 100.0, 100.0], [0.0, 0.0, 1.0], [0.0, 1.0]),
            bv([100.0, 100.0, 100.0], [0.0, 0.0, 1.0], [1.0, 1.0]),
        ];
        mesh.materials = vec![Material {
            identifier: "wall".to_string(),
            map_identifier: "brick.tga".to_string(),
            map_id_meaning: 1,
            ..Material::new()
        }];
        mesh.triangles = vec![tri([0, 2, 1], 0), tri([1, 2, 3], 0)];

        let mut saved = mesh
            .save(
                &settings(),
                None,
                BinSection::new("house.primitives", BinaryBlock::default()),
                false,
                &VisualExportIo::none(),
            )
            .unwrap();
        let primitives_bytes = saved.primitives.save_bytes();
        std::fs::create_dir_all(dir.join("structures")).unwrap();
        std::fs::write(dir.join("structures/house.primitives"), primitives_bytes).unwrap();
        let mut visual = saved.visual;
        let xml = visual.write_to_string();
        std::fs::write(dir.join("structures/house.visual"), &xml).unwrap();

        let parsed = resources::datasection::parse("house.visual", xml.as_bytes()).unwrap();
        let mut checker = crate::resource_checker::VisualChecker::with_options(
            &resource,
            "structures/house.visual",
            true,
            true,
        );
        let ok = checker.check(&resource, Some(&parsed), "structures/house.primitives");
        assert!(ok, "checker errors: {}", checker.error_text());
    }

    #[test]
    fn save_skips_materials_no_triangle_uses() {
        let mut mesh = VisualMesh {
            identifier: "cube".to_string(),
            ..VisualMesh::default()
        };
        mesh.vertices = vec![bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0])];
        mesh.materials = vec![
            Material {
                identifier: "used".to_string(),
                map_identifier: "grid.tga".to_string(),
                map_id_meaning: 1,
                ..Material::new()
            },
            Material {
                identifier: "spare".to_string(),
                map_identifier: "grid.tga".to_string(),
                map_id_meaning: 1,
                ..Material::new()
            },
        ];
        mesh.triangles = vec![tri([0, 0, 0], 0)];

        let saved = mesh
            .save(
                &settings(),
                None,
                BinSection::new("cube.primitives", BinaryBlock::default()),
                false,
                &VisualExportIo::none(),
            )
            .unwrap();
        let render_set = saved.visual.open_section("renderSet").unwrap();
        let geometry = render_set.open_section("geometry").unwrap();
        let group_count = (0..geometry.count_children())
            .filter(|&i| geometry.open_child(i).unwrap().section_name() == "primitiveGroup")
            .count();
        assert_eq!(group_count, 1);
        assert_eq!(
            geometry.read_string("primitiveGroup/material/identifier", ""),
            "used"
        );
    }

    #[test]
    fn save_switches_to_32_bit_indices_past_65536_vertices() {
        // bestFormat keeps 16 bits at exactly 65536; one more vertex
        // moves the whole index stream to "list32".
        let mut mesh = VisualMesh {
            identifier: "big".to_string(),
            ..VisualMesh::default()
        };
        mesh.vertices = (0..65537)
            .map(|i| bv([i as f32, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]))
            .collect();
        mesh.materials = vec![Material::new()];
        mesh.triangles = (0..65537).map(|i| tri([i, i, i], 0)).collect();

        let saved = mesh
            .save(
                &settings(),
                None,
                BinSection::new("big.primitives", BinaryBlock::default()),
                true,
                &VisualExportIo::none(),
            )
            .unwrap();
        let mut prims = saved.primitives;
        let blob = prims.read_binary("big.indices").unwrap();
        let header = IndexHeader::read(blob.data()).unwrap();
        assert_eq!(header.index_format, "list32");
        assert_eq!(header.n_indices, 65537 * 3);
        assert_eq!(header.n_triangle_groups, 1);
        // The last written index is the final vertex, as a u32.
        let last = blob.data()
            [IndexHeader::SIZE + 4 * (65537 * 3 - 1)..IndexHeader::SIZE + 4 * (65537 * 3)]
            .try_into()
            .unwrap();
        assert_eq!(u32::from_le_bytes(last), 65536);
        let vertex_blob = prims.read_binary("big.vertices").unwrap();
        assert_eq!(
            VertexHeader::read(vertex_blob.data()).unwrap().n_vertices,
            65537
        );
    }

    #[test]
    fn saving_into_an_existing_container_keeps_unrelated_children() {
        let mut mesh = VisualMesh {
            identifier: "part".to_string(),
            ..VisualMesh::default()
        };
        mesh.vertices = vec![bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0])];
        mesh.materials = vec![Material::new()];
        mesh.triangles = vec![tri([0, 0, 0], 0)];

        let mut container = BinSection::new("shared.primitives", BinaryBlock::default());
        container
            .new_section("other.vertices")
            .set_binary(BinaryBlock::new(vec![1, 2, 3, 4]));

        let saved = mesh
            .save(&settings(), None, container, false, &VisualExportIo::none())
            .unwrap();
        let mut prims = saved.primitives;
        assert!(prims.read_binary("other.vertices").is_some());
        assert!(prims.read_binary("vertices").is_some());
        // Re-saving replaces the same-name child instead of appending.
        let saved = mesh
            .save(&settings(), None, prims, false, &VisualExportIo::none())
            .unwrap();
        let mut prims = saved.primitives;
        assert!(prims.read_binary("other.vertices").is_some());
        let names: Vec<String> = prims
            .children()
            .iter()
            .map(|c| c.section_name().to_string())
            .collect();
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn material_save_covers_every_map_meaning() {
        let mut settings = settings();
        settings.default_material = "system/default.mfm".to_string();
        settings.default_skinned_material = "system/skinned.mfm".to_string();

        // meaning 1, plain bitmap.
        let mut material = Material {
            identifier: "brick".to_string(),
            map_identifier: "grid.tga".to_string(),
            map_id_meaning: 1,
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
            &VisualExportIo::none(),
        );
        let group = geometry.open_section("primitiveGroup").unwrap();
        assert_eq!(group.as_int(255), 0);
        assert_eq!(
            group.read_string("material/fx", ""),
            "shaders/std_effects/lightonly.fx"
        );
        // The property tag carries the override name as its value, with
        // the texture as a child tag.
        assert_eq!(group.read_string("material/property", ""), "diffuseMap");
        assert_eq!(
            group.read_string("material/property/Texture", ""),
            "grid.tga"
        );

        // meaning 1 under a soft skin: the host picks between the skinned
        // and plain lightonly shaders.
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::SoftSkin,
            true,
            &settings,
            &VisualExportIo::none(),
        );
        assert_eq!(
            geometry.read_string("primitiveGroup/material/fx", ""),
            "shaders/std_effects/lightonly_skinned.fx"
        );
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::SoftSkin,
            false,
            &settings,
            &VisualExportIo::none(),
        );
        assert_eq!(
            geometry.read_string("primitiveGroup/material/fx", ""),
            "shaders/std_effects/lightonly.fx"
        );

        // meaning 1 with the _afx suffix selects the additive shader.
        let mut glow = Material {
            identifier: "glow".to_string(),
            map_identifier: "grid_afx.tga".to_string(),
            map_id_meaning: 1,
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        glow.save(
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
            &VisualExportIo::none(),
        );
        assert_eq!(
            geometry.read_string("primitiveGroup/material/fx", ""),
            "shaders/std_effects/lightonly_add.fx"
        );

        // meaning 2 writes the mfm.
        let mut mfm = Material {
            identifier: "mfm".to_string(),
            map_identifier: "steel.mfm".to_string(),
            map_id_meaning: 2,
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        mfm.save(
            &mut geometry,
            None,
            1,
            SkinType::NoSkin,
            false,
            &settings,
            &VisualExportIo::none(),
        );
        let group = geometry.open_section("primitiveGroup").unwrap();
        assert_eq!(group.as_int(255), 1);
        assert_eq!(group.read_string("material/mfm", ""), "steel.mfm");

        // meaning 0 falls back to the default material option.
        let mut plain = Material {
            identifier: "plain".to_string(),
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        plain.save(
            &mut geometry,
            None,
            2,
            SkinType::NoSkin,
            false,
            &settings,
            &VisualExportIo::none(),
        );
        assert_eq!(
            geometry.read_string("primitiveGroup/material/mfm", ""),
            "system/default.mfm"
        );

        // The skinned default for a soft-skinned material.
        let mut geometry = DataSection::new("geometry");
        plain.save(
            &mut geometry,
            None,
            3,
            SkinType::SoftSkin,
            true,
            &settings,
            &VisualExportIo::none(),
        );
        let group = geometry.open_section("primitiveGroup").unwrap();
        assert_eq!(
            group.read_string("material/identifier", ""),
            "plain_skinned"
        );
        assert_eq!(group.read_string("material/mfm", ""), "system/skinned.mfm");
    }

    #[test]
    fn material_save_promotes_a_same_stem_mfm() {
        let mut material = Material {
            identifier: "paint".to_string(),
            map_identifier: "paint.tga".to_string(),
            map_id_meaning: 0,
            ..Material::new()
        };
        let io = VisualExportIo {
            file_exists: Box::new(|_| false),
            open_section: Box::new(|name| {
                if name == "paint.mfm" {
                    let mut section = DataSection::new("paint.mfm");
                    section.add_child("fx");
                    Some(section)
                } else {
                    None
                }
            }),
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &ExportSettings::new(),
            &io,
        );
        // Promoted: the map became the mfm and meaning flipped to 2.
        assert_eq!(material.map_identifier, "paint.mfm");
        assert_eq!(material.map_id_meaning, 2);
        assert_eq!(
            geometry.read_string("primitiveGroup/material/mfm", ""),
            "paint.mfm"
        );

        // A probe that finds no section leaves the bitmap alone.
        let mut material = Material {
            identifier: "wall".to_string(),
            map_identifier: "wall.tga".to_string(),
            map_id_meaning: 0,
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &ExportSettings::new(),
            &io,
        );
        assert_eq!(material.map_identifier, "wall.tga");
        assert_eq!(material.map_id_meaning, 0);

        // A legacy blend dump is not a material — no promotion.
        let mut material = Material {
            identifier: "paint".to_string(),
            map_identifier: "paint.tga".to_string(),
            map_id_meaning: 0,
            ..Material::new()
        };
        let io = VisualExportIo {
            file_exists: Box::new(|_| false),
            open_section: Box::new(|_| {
                let mut section = DataSection::new("paint.mfm");
                section.add_child("property");
                section.add_child("Alpha_blended");
                Some(section)
            }),
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &ExportSettings::new(),
            &io,
        );
        assert_eq!(material.map_id_meaning, 0);
        assert!(
            geometry
                .open_section("primitiveGroup/material")
                .unwrap()
                .open_section("mfm")
                .is_none()
        );
    }

    #[test]
    fn material_save_writes_the_fx_file_and_its_overrides() {
        let mut material = Material {
            identifier: "custom".to_string(),
            fx_file: "effects\\custom.fx".to_string(),
            texture_overrides: vec![("diffuseMap".to_string(), "textures\\brick.tga".to_string())],
            vector_overrides: vec![("tint".to_string(), Vector4::new(1.0, 0.5, 0.25, 1.0))],
            bool_overrides: vec![("glow".to_string(), true)],
            float_overrides: vec![("roughness".to_string(), 0.75)],
            int_overrides: vec![("passes".to_string(), 2)],
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &ExportSettings::new(),
            &VisualExportIo::none(),
        );
        let group = geometry.open_section("primitiveGroup").unwrap();
        assert_eq!(group.read_string("material/fx", ""), "effects/custom.fx");
        let material = group.open_section("material").unwrap();
        let props: Vec<&DataSection> = (0..material.count_children())
            .filter_map(|i| {
                let child = material.open_child(i).unwrap();
                (child.section_name() == "property").then_some(child)
            })
            .collect();
        assert_eq!(props.len(), 5);
        assert_eq!(props[0].as_string(), "diffuseMap");
        assert_eq!(props[0].read_string("Texture", ""), "textures/brick.tga");
        assert_eq!(
            props[1]
                .open_section("Vector4")
                .unwrap()
                .as_vector4(Vector4::ZERO),
            Vector4::new(1.0, 0.5, 0.25, 1.0)
        );
        assert_eq!(props[2].as_string(), "glow");
        assert!(props[2].read_bool("Bool", false));
        assert_eq!(props[3].as_string(), "roughness");
        assert_eq!(props[3].read_float("Float", 0.0), 0.75);
        assert_eq!(props[4].read_int("Int", 0), 2);

        // A skinned host passes the identifier postfix through to the fx
        // resolution: "custom.fx" resolves to "custom_skinned.fx" when the
        // host has that file.
        let files = ["custom_skinned.fx"];
        let io = VisualExportIo {
            file_exists: Box::new(move |name| files.contains(&name)),
            open_section: Box::new(|_| None),
        };
        let mut material = Material {
            identifier: "custom".to_string(),
            fx_file: "custom.fx".to_string(),
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::SoftSkin,
            true,
            &ExportSettings::new(),
            &io,
        );
        assert_eq!(
            geometry.read_string("primitiveGroup/material/fx", ""),
            "custom_skinned.fx"
        );
    }

    #[test]
    fn mesh_particle_material_uses_the_particle_shader() {
        let mut settings = settings();
        settings.export_mode = ExportMode::MeshParticles;
        let mut material = Material {
            identifier: "spark".to_string(),
            map_identifier: "spark.tga".to_string(),
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
            &VisualExportIo::none(),
        );
        let group = geometry.open_section("primitiveGroup").unwrap();
        assert_eq!(
            group.read_string("material/fx", ""),
            "shaders/std_effects/mesh_particle.fx"
        );
        assert_eq!(
            group.read_string("material/property/Texture", ""),
            "spark.tga"
        );

        // No map: the property is skipped entirely.
        let mut bare = Material {
            identifier: "bare".to_string(),
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        bare.save(
            &mut geometry,
            None,
            0,
            SkinType::NoSkin,
            false,
            &settings,
            &VisualExportIo::none(),
        );
        let group = geometry.open_section("primitiveGroup").unwrap();
        assert!(group.open_section("material/property").is_none());
    }

    #[test]
    fn material_save_copies_the_existing_visual_material() {
        let mut existing = DataSection::new("root");
        // Junk around the real entry: the walk skips non-renderSet tops,
        // non-geometry renderSet children, non-primitiveGroup geometry
        // children and primitiveGroups without a material.
        existing.add_child("boundingBox");
        let stray_set = existing.add_child("renderSet");
        stray_set.add_child("extents");
        let render_set = existing.add_child("renderSet");
        render_set.add_child("worldTransform");
        let geometry = render_set.add_child("geometry");
        geometry.add_child("vertices");
        geometry.add_child("primitiveGroup");
        let group = geometry.add_child("primitiveGroup");
        let material = group.add_child("material");
        material.write_string("identifier", "brick");
        material.write_string("fx", "shaders/old.fx");

        let mut material = Material {
            identifier: "brick".to_string(),
            map_identifier: "grid.tga".to_string(),
            map_id_meaning: 1,
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            Some(&existing),
            0,
            SkinType::NoSkin,
            false,
            &ExportSettings::new(),
            &VisualExportIo::none(),
        );
        // The copied material replaces the generated one wholesale.
        let group = geometry.open_section("primitiveGroup").unwrap();
        assert_eq!(group.read_string("material/identifier", ""), "brick");
        assert_eq!(group.read_string("material/fx", ""), "shaders/old.fx");
        assert!(
            group
                .open_section("material")
                .unwrap()
                .open_section("property")
                .is_none()
        );

        // A different identifier does not match; the generated material
        // carries the lightonly fx.
        let mut material = Material {
            identifier: "other".to_string(),
            map_identifier: "grid.tga".to_string(),
            map_id_meaning: 1,
            ..Material::new()
        };
        let mut geometry = DataSection::new("geometry");
        material.save(
            &mut geometry,
            Some(&existing),
            0,
            SkinType::NoSkin,
            false,
            &ExportSettings::new(),
            &VisualExportIo::none(),
        );
        assert_eq!(
            geometry.read_string("primitiveGroup/material/fx", ""),
            "shaders/std_effects/lightonly.fx"
        );
    }

    #[test]
    fn check_filename_resolves_the_skinned_variant() {
        let files: Vec<String> = vec![
            "armor.fx".to_string(),
            "armor_skinned.fx".to_string(),
            "armor_fire.fx".to_string(),
            "plain.fx".to_string(),
        ];
        let io = VisualExportIo {
            file_exists: Box::new(move |name| files.contains(&name.to_string())),
            open_section: Box::new(|_| None),
        };

        // Already-skinned name, skinned postfix: the marker is stripped
        // and re-inserted, so the candidate is the name itself.
        let material = Material {
            fx_file: "armor_skinned.fx".to_string(),
            ..Material::new()
        };
        let mut fx_name = String::new();
        assert!(material.check_filename(&mut fx_name, "_skinned", &io));
        assert_eq!(fx_name, "armor_skinned.fx");

        // Same name, plain postfix: the original stem without the marker
        // is restored.
        let mut fx_name = String::new();
        assert!(material.check_filename(&mut fx_name, "", &io));
        assert_eq!(fx_name, "armor.fx");

        // A marker followed by more stem keeps the tail.
        let material = Material {
            fx_file: "armor_skinned_fire.fx".to_string(),
            ..Material::new()
        };
        let mut fx_name = String::new();
        assert!(material.check_filename(&mut fx_name, "", &io));
        assert_eq!(fx_name, "armor_fire.fx");

        // Plain name with a skinned postfix that does not exist: the name
        // keeps the original (only an existing candidate rewrites it).
        let material = Material {
            fx_file: "plain.fx".to_string(),
            ..Material::new()
        };
        let mut fx_name = String::new();
        assert!(material.check_filename(&mut fx_name, "_skinned", &io));
        assert_eq!(fx_name, "plain.fx");

        // A skinned request that does not exist fails — the name keeps
        // the original value.
        let material = Material {
            fx_file: "missing_skinned.fx".to_string(),
            ..Material::new()
        };
        let mut fx_name = String::new();
        assert!(!material.check_filename(&mut fx_name, "_skinned", &io));
        assert_eq!(fx_name, "missing_skinned.fx");

        // Non-.fx names pass through untouched.
        let material = Material {
            fx_file: "effect.fx.bak".to_string(),
            ..Material::new()
        };
        let mut fx_name = String::new();
        assert!(material.check_filename(&mut fx_name, "_skinned", &io));
        assert_eq!(fx_name, "effect.fx.bak");
    }

    #[test]
    fn tb_vertex_list_and_streams_round_trip_through_headers() {
        // The bump-mapped face: format name xyznuvtb, and the vertex
        // header reads back the pre-dedup count is impossible to assert
        // here — instead pin the format selection switch.
        let mut mesh = VisualMesh {
            vertices: vec![bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0])],
            ..VisualMesh::default()
        };
        mesh.materials = vec![Material::new()];
        mesh.triangles = vec![tri([0, 0, 0], 0)];
        let mut settings = settings();
        settings.bump_mapped = true;
        let saved = mesh
            .save(
                &settings,
                None,
                BinSection::new("b.primitives", BinaryBlock::default()),
                false,
                &VisualExportIo::none(),
            )
            .unwrap();
        let mut prims = saved.primitives;
        let blob = prims.read_binary("vertices").unwrap();
        assert_eq!(
            VertexHeader::read(blob.data()).unwrap().vertex_format,
            "xyznuvtb"
        );

        // Mesh-particle mode picks xyznuvi. The mesh-particle group pass
        // demands a whole-mesh group plus one per sub-mesh 1..=15, so the
        // fixture needs all fifteen mesh indices represented.
        let mut mesh = VisualMesh {
            vertices: (0..15)
                .map(|j| {
                    let mut v = bv([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
                    v.mesh_index = j + 1;
                    v
                })
                .collect(),
            ..VisualMesh::default()
        };
        mesh.materials = vec![Material::new()];
        mesh.triangles = (0..15).map(|j| tri([j, j, j], 0)).collect();
        let mut settings = ExportSettings::new();
        settings.bump_mapped = false;
        settings.export_mode = ExportMode::MeshParticles;
        let saved = mesh
            .save(
                &settings,
                None,
                BinSection::new("mp.primitives", BinaryBlock::default()),
                false,
                &VisualExportIo::none(),
            )
            .unwrap();
        let mut prims = saved.primitives;
        let blob = prims.read_binary("vertices").unwrap();
        assert_eq!(
            VertexHeader::read(blob.data()).unwrap().vertex_format,
            "xyznuvi"
        );
    }
}
